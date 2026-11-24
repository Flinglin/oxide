

use std::time::Instant as StdInstant;

use oxide_x11_forwarding::{
    X11AuthMaterial, X11PreparedForwarding, X11RuntimeError, connect_local_x11_endpoint,
    prepare_x11_forwarding,
};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const X11_CHANNEL_READ_BUFFER_BYTES: usize = 32 * 1024;
const X11_CHANNEL_LIMIT: usize = 32;
const X11_SETUP_TIMEOUT: Duration = Duration::from_secs(10);


/// Contains categories only; display paths and authorization cookies stay transport-owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X11ForwardingFailure {
    LocalDisplayUnavailable,
    LocalAuthorizationUnavailable,
    ServerRejected,
    RequestTimedOut,
    ChannelUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X11ForwardingStatus {
    Accepted,
    Unavailable(X11ForwardingFailure),
}

#[derive(Clone)]
struct X11ForwardDispatcher {
    state: Arc<Mutex<X11ForwardDispatcherState>>,
}

struct X11ForwardDispatcherState {
    auth_registry: X11AuthSpoofRegistry<String>,
    routes: HashMap<String, X11ForwardRoute>,
}

#[derive(Clone)]
struct X11ForwardRoute {
    endpoint: X11LocalEndpoint,
    expires_at: Option<StdInstant>,
    connection_owner: Option<X11ConnectionOwner>,
}

#[derive(Clone)]
enum X11ConnectionOwner {
    Registry {
        registry: SshConnectionRegistry,
        connection_id: String,
    },
    Standalone(std::sync::Weak<dyn Send + Sync>),
}

struct X11ForwardRouteGuard {
    dispatcher: X11ForwardDispatcher,
    route_id: String,
}

impl std::fmt::Debug for X11ForwardRouteGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("X11ForwardRouteGuard(<redacted route>)")
    }
}

impl X11ForwardDispatcher {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(X11ForwardDispatcherState {
                auth_registry: X11AuthSpoofRegistry::new(),
                routes: HashMap::new(),
            })),
        }
    }

    fn register(
        &self,
        route_id: String,
        endpoint: X11LocalEndpoint,
        auth: X11AuthMaterial,
        single_connection: bool,
        acceptance_timeout: Option<Duration>,
        connection_owner: Option<X11ConnectionOwner>,
    ) -> X11ForwardRouteGuard {
        let expires_at = acceptance_timeout.and_then(|duration| StdInstant::now().checked_add(duration));
        let mut state = self.state.lock();
        prune_expired_x11_routes(&mut state);
        state.auth_registry.insert(X11SpoofedAuth {
            channel_id: route_id.clone(),
            auth,
            single_connection,
        });
        state.routes.insert(
            route_id.clone(),
            X11ForwardRoute {
                endpoint,
                expires_at,
                connection_owner,
            },
        );
        X11ForwardRouteGuard {
            dispatcher: self.clone(),
            route_id,
        }
    }

    fn has_active_routes(&self) -> bool {
        let mut state = self.state.lock();
        prune_expired_x11_routes(&mut state);
        !state.routes.is_empty()
    }

    async fn bridge(
        &self,
        mut stream: BoxedSshForwardStream,
    ) -> Result<(), X11RuntimeError> {
        let mut setup = X11SetupBuffer::new();
        let mut read_buffer = vec![0u8; X11_CHANNEL_READ_BUFFER_BYTES];
        let setup_deadline = tokio::time::Instant::now() + X11_SETUP_TIMEOUT;
        loop {
            let read = tokio::time::timeout_at(setup_deadline, stream.read(&mut read_buffer))
                .await
                .map_err(|_| {
                    X11RuntimeError::Io("X11 setup packet timed out".to_string())
                })??;
            if read == 0 {
                return Err(X11RuntimeError::Io(
                    "X11 channel closed before setup packet completed".to_string(),
                ));
            }
            let decision = {
                let mut state = self.state.lock();
                prune_expired_x11_routes(&mut state);
                setup.push_registry_decision(&read_buffer[..read], &mut state.auth_registry)?
            };
            let Some(decision) = decision else {
                continue;
            };
            match decision {
                X11RegisteredSetupDecision::Forward(forward) => {
                    let route = {
                        let mut state = self.state.lock();
                        prune_expired_x11_routes(&mut state);
                        state.routes.get(&forward.channel_id).cloned()
                    };
                    let Some(route) = route else {
                        return Err(X11RuntimeError::Io(
                            "X11 forwarding route expired before channel setup".to_string(),
                        ));
                    };
                    let _connection_lease = X11BridgeConnectionLease::acquire(
                        route.connection_owner,
                        &forward.channel_id,
                    )?;
                    let mut local = connect_local_x11_endpoint(&route.endpoint).await?;
                    local.write_all(&forward.rewrite.rewritten_setup).await?;
                    if !forward.rewrite.trailing_data.is_empty() {
                        local.write_all(&forward.rewrite.trailing_data).await?;
                    }
                    tokio::io::copy_bidirectional(&mut stream, &mut local).await?;
                    return Ok(());
                }
                X11RegisteredSetupDecision::Reject(reject) => {
                    stream.write_all(&reject.failure_response).await?;
                    let _ = stream.shutdown().await;
                    return Ok(());
                }
            }
        }
    }

    fn remove(&self, route_id: &str) {
        let mut state = self.state.lock();
        state.routes.remove(route_id);
        state.auth_registry.remove_channel(&route_id.to_string());
    }
}

struct X11BridgeConnectionLease {
    registry_release: Option<(SshConnectionRegistry, String, ConnectionConsumer)>,
    // Standalone shells have no registry entry, so an established bridge keeps
    // the physical transport alive directly until bidirectional copy finishes.
    _standalone_owner: Option<Arc<dyn Send + Sync>>,
}

impl X11BridgeConnectionLease {
    fn acquire(
        owner: Option<X11ConnectionOwner>,
        route_id: &str,
    ) -> Result<Self, X11RuntimeError> {
        let Some(owner) = owner else {
            return Ok(Self {
                registry_release: None,
                _standalone_owner: None,
            });
        };
        match owner {
            X11ConnectionOwner::Registry {
                registry,
                connection_id,
            } => {
                let consumer = ConnectionConsumer::X11Forward(format!(
                    "{route_id}:{}",
                    uuid::Uuid::new_v4()
                ));
                if registry
                    .acquire_consumer_for_connection(&connection_id, consumer.clone())
                    .is_none()
                {
                    return Err(X11RuntimeError::Io(
                        "SSH connection owner disappeared before X11 channel setup".to_string(),
                    ));
                }
                Ok(Self {
                    registry_release: Some((registry, connection_id, consumer)),
                    _standalone_owner: None,
                })
            }
            X11ConnectionOwner::Standalone(owner) => {
                let owner = owner.upgrade().ok_or_else(|| {
                    X11RuntimeError::Io(
                        "standalone SSH connection disappeared before X11 channel setup"
                            .to_string(),
                    )
                })?;
                Ok(Self {
                    registry_release: None,
                    _standalone_owner: Some(owner),
                })
            }
        }
    }
}

impl Drop for X11BridgeConnectionLease {
    fn drop(&mut self) {
        if let Some((registry, connection_id, consumer)) = self.registry_release.take() {
            registry.release(&connection_id, &consumer);
        }
    }
}

impl Drop for X11ForwardRouteGuard {
    fn drop(&mut self) {
        self.dispatcher.remove(&self.route_id);
    }
}

fn prune_expired_x11_routes(state: &mut X11ForwardDispatcherState) {
    let now = StdInstant::now();
    let expired = state
        .routes
        .iter()
        .filter_map(|(route_id, route)| {
            route
                .expires_at
                .is_some_and(|expires_at| expires_at <= now)
                .then(|| route_id.clone())
        })
        .collect::<Vec<_>>();
    for route_id in expired {
        state.routes.remove(&route_id);
        state.auth_registry.remove_channel(&route_id);
    }
}

async fn prepare_x11_material(
    policy: X11ForwardPolicy,
) -> Result<X11PreparedForwarding, X11ForwardingFailure> {
    prepare_x11_forwarding(policy)
        .await
        .map_err(|error| match error {
            oxide_x11_forwarding::X11ForwardingError::EmptyDisplay
            | oxide_x11_forwarding::X11ForwardingError::MissingDisplay
            | oxide_x11_forwarding::X11ForwardingError::InvalidDisplay(_)
            | oxide_x11_forwarding::X11ForwardingError::DisplayPortOutOfRange(_) => {
                X11ForwardingFailure::LocalDisplayUnavailable
            }
            _ => X11ForwardingFailure::LocalAuthorizationUnavailable,
        })
}

fn register_x11_route(
    dispatcher: &X11ForwardDispatcher,
    route_id: String,
    prepared: X11PreparedForwarding,
    connection_owner: Option<X11ConnectionOwner>,
) -> (X11SshRequest, X11ForwardRouteGuard) {
    let X11PreparedForwarding {
        endpoint,
        auth,
        request,
        acceptance_timeout,
    } = prepared;
    let guard = dispatcher.register(
        route_id,
        endpoint,
        auth,
        request.single_connection,
        acceptance_timeout,
        connection_owner,
    );
    (request, guard)
}
