use std::{sync::Arc, time::Duration};

use futures::{AsyncReadExt, future::BoxFuture};
use gpui::http_client::{
    AsyncBody, HttpClient, RedirectPolicy, Request, RequestTimeout, Response, Url,
    http::HeaderValue,
};
use tokio_util::task::AbortOnDropHandle;

/// Bridges GPUI asset requests to the application's existing Tokio runtime.
pub struct AssetHttpClient {
    runtime: Arc<tokio::runtime::Runtime>,
    redirects: reqwest::Client,
    no_redirects: reqwest::Client,
}

impl AssetHttpClient {
    pub fn new(runtime: Arc<tokio::runtime::Runtime>) -> anyhow::Result<Self> {
        let client = |redirect| {
            reqwest::Client::builder()
                .redirect(redirect)
                .timeout(Duration::from_secs(30))
                .build()
        };
        Ok(Self {
            runtime,
            redirects: client(reqwest::redirect::Policy::limited(10))?,
            no_redirects: client(reqwest::redirect::Policy::none())?,
        })
    }
}

impl HttpClient for AssetHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let timeout = request
            .extensions()
            .get::<RequestTimeout>()
            .map_or(Duration::from_secs(30), |timeout| timeout.0);
        let client = match request.extensions().get::<RedirectPolicy>() {
            Some(RedirectPolicy::FollowAll) => Ok(self.redirects.clone()),
            Some(RedirectPolicy::FollowLimit(limit)) => reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(*limit as usize))
                .timeout(Duration::from_secs(30))
                .build(),
            _ => Ok(self.no_redirects.clone()),
        };
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let client = client.map_err(reqwest::Error::without_url)?;
            // GPUI polls assets outside Tokio. Keep the runtime alive for the request
            // and cancel network work when GPUI drops the asset future.
            AbortOnDropHandle::new(runtime.spawn(async move {
                tokio::time::timeout(timeout, async move {
                    let (parts, mut body) = request.into_parts();
                    let has_body = !matches!(&body.0, gpui::http_client::Inner::Empty);
                    let mut bytes = zeroize::Zeroizing::new(Vec::new());
                    body.read_to_end(&mut bytes)
                        .await
                        .map_err(|_| anyhow::anyhow!("Unable to read asset HTTP request body"))?;
                    // Reqwest owns the protocol buffers; the bridge's temporary body is cleared on drop.
                    let mut request = client
                        .request(parts.method, parts.uri.to_string())
                        .headers(parts.headers)
                        .version(parts.version)
                        .timeout(timeout);
                    if has_body {
                        request = request.body(bytes.to_vec());
                    }
                    let request = request.build().map_err(reqwest::Error::without_url)?;
                    drop(bytes);
                    let response = client
                        .execute(request)
                        .await
                        .map_err(reqwest::Error::without_url)?;
                    let status = response.status();
                    let version = response.version();
                    let headers = response.headers().clone();
                    let body = response
                        .bytes()
                        .await
                        .map_err(reqwest::Error::without_url)?
                        .to_vec();
                    let mut response = Response::builder()
                        .status(status)
                        .version(version)
                        .body(AsyncBody::from(body))?;
                    *response.headers_mut() = headers;
                    Ok(response)
                })
                .await
                .map_err(|_| anyhow::anyhow!("Asset HTTP request timed out"))?
            }))
            .await?
        })
    }
}
