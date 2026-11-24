use std::io::{BufRead, Write};

use crate::{
    RemoteDesktopEndpoint, RemoteDesktopErrorCategory, RemoteDesktopFrame,
    RemoteDesktopFrameFormat, RemoteDesktopHelperEvent, RemoteDesktopHelperRequest,
    RemoteDesktopJsonLineError, RemoteDesktopProtocol, RemoteDesktopSessionStatus,
    RemoteDesktopSize, read_request_line, write_event_line,
};

#[derive(Clone, Debug)]
pub struct RemoteDesktopFakeBackend {
    protocol: RemoteDesktopProtocol,
    endpoint: Option<RemoteDesktopEndpoint>,
    size: RemoteDesktopSize,
    status: RemoteDesktopSessionStatus,
    read_only: bool,
}

impl Default for RemoteDesktopFakeBackend {
    fn default() -> Self {
        Self::new(RemoteDesktopProtocol::Rdp)
    }
}

impl RemoteDesktopFakeBackend {
    pub fn new(protocol: RemoteDesktopProtocol) -> Self {
        Self {
            protocol,
            endpoint: None,
            size: RemoteDesktopSize::clamped(1024, 768),
            status: RemoteDesktopSessionStatus::Idle,
            read_only: false,
        }
    }

    pub fn status(&self) -> RemoteDesktopSessionStatus {
        self.status
    }

    pub fn handle_request(
        &mut self,
        request: RemoteDesktopHelperRequest,
    ) -> Vec<RemoteDesktopHelperEvent> {
        match request {
            RemoteDesktopHelperRequest::StartConnect {
                protocol,
                endpoint,
                transport_endpoint: _,
                socks_proxy: _,
                size,
                scale_factor: _,
                read_only,
                session_options: _,
                monitor_layout: _,
                password_available: _,
                username_available: _,
            } => self.connect(protocol, endpoint, size, read_only),
            RemoteDesktopHelperRequest::Connect {
                protocol,
                endpoint,
                username: _username,
                password: _password,
                domain: _domain,
                size,
                scale_factor: _scale_factor,
                read_only,
            } => self.connect(protocol, endpoint, size, read_only),
            RemoteDesktopHelperRequest::Resize { size, .. } => {
                self.size = RemoteDesktopSize::clamped(size.width, size.height);
                vec![
                    RemoteDesktopHelperEvent::Connected { size: self.size },
                    RemoteDesktopHelperEvent::Frame {
                        frame: self.synthetic_frame(),
                    },
                ]
            }
            RemoteDesktopHelperRequest::Reconnect => {
                if self.endpoint.is_none() {
                    self.status = RemoteDesktopSessionStatus::Failed;
                    return vec![RemoteDesktopHelperEvent::ConnectionFailure {
                        message: "No previous fake remote desktop endpoint exists.".to_string(),
                        category: Some(RemoteDesktopErrorCategory::Configuration),
                    }];
                }

                self.status = RemoteDesktopSessionStatus::Connected;
                vec![
                    RemoteDesktopHelperEvent::Status {
                        status: RemoteDesktopSessionStatus::Reconnecting,
                        message: Some("Fake remote desktop helper is reconnecting.".to_string()),
                    },
                    RemoteDesktopHelperEvent::Connected { size: self.size },
                    RemoteDesktopHelperEvent::Frame {
                        frame: self.synthetic_frame(),
                    },
                ]
            }
            RemoteDesktopHelperRequest::Close => {
                self.status = RemoteDesktopSessionStatus::Disconnected;
                self.endpoint = None;
                vec![RemoteDesktopHelperEvent::Disconnected {
                    reason: Some("Fake remote desktop helper closed.".to_string()),
                }]
            }
            RemoteDesktopHelperRequest::RequestFrame => {
                if self.status == RemoteDesktopSessionStatus::Connected {
                    vec![RemoteDesktopHelperEvent::Frame {
                        frame: self.synthetic_frame(),
                    }]
                } else {
                    Vec::new()
                }
            }
            RemoteDesktopHelperRequest::MouseMove { .. }
            | RemoteDesktopHelperRequest::MouseButton { .. }
            | RemoteDesktopHelperRequest::Wheel { .. }
            | RemoteDesktopHelperRequest::Key { .. }
            | RemoteDesktopHelperRequest::Text { .. }
            | RemoteDesktopHelperRequest::ClipboardText { .. }
            | RemoteDesktopHelperRequest::PasteText { .. }
            | RemoteDesktopHelperRequest::ClipboardData { .. }
            | RemoteDesktopHelperRequest::ClipboardFiles { .. }
            | RemoteDesktopHelperRequest::VncListRemoteFiles { .. }
            | RemoteDesktopHelperRequest::VncDownloadRemoteFiles { .. }
            | RemoteDesktopHelperRequest::CancelVncFileTransfer { .. }
            | RemoteDesktopHelperRequest::CancelClipboardTransfer { .. }
            | RemoteDesktopHelperRequest::UpdateDisplayLayout { .. }
            | RemoteDesktopHelperRequest::Authenticate { .. }
            | RemoteDesktopHelperRequest::SynchronizeLockKeys { .. }
            | RemoteDesktopHelperRequest::ReleaseAllInputs => Vec::new(),
        }
    }

    fn connect(
        &mut self,
        protocol: RemoteDesktopProtocol,
        endpoint: RemoteDesktopEndpoint,
        size: RemoteDesktopSize,
        read_only: bool,
    ) -> Vec<RemoteDesktopHelperEvent> {
        self.protocol = protocol;
        self.endpoint = Some(endpoint);
        self.size = RemoteDesktopSize::clamped(size.width, size.height);
        self.read_only = read_only;
        self.status = RemoteDesktopSessionStatus::Connected;
        vec![
            RemoteDesktopHelperEvent::Status {
                status: RemoteDesktopSessionStatus::Connecting,
                message: Some("Fake remote desktop helper is opening.".to_string()),
            },
            RemoteDesktopHelperEvent::Connected { size: self.size },
            RemoteDesktopHelperEvent::Frame {
                frame: self.synthetic_frame(),
            },
        ]
    }

    fn synthetic_frame(&self) -> RemoteDesktopFrame {
        let expected = RemoteDesktopFrame::expected_len(self.size).unwrap_or_default();
        let mut bytes = Vec::with_capacity(expected);
        let protocol_bias = match self.protocol {
            RemoteDesktopProtocol::Rdp => 0x30,
            RemoteDesktopProtocol::Vnc => 0x80,
        };

        // The fake frame is deterministic so tests can validate lifecycle code
        // without needing a real RDP/VNC server or image decoder.
        for index in 0..(expected / 4) {
            let stripe = ((index as u32 / self.size.width.max(1)) % 255) as u8;
            bytes.extend_from_slice(&[protocol_bias, stripe, 255_u8.saturating_sub(stripe), 255]);
        }

        RemoteDesktopFrame::new(self.size, RemoteDesktopFrameFormat::Rgba8, bytes)
    }
}

pub fn run_fake_backend_stdio(
    backend: &mut RemoteDesktopFakeBackend,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<usize, RemoteDesktopJsonLineError> {
    let mut handled = 0;
    while let Some(request) = read_request_line(reader)? {
        handled += 1;
        let should_close = matches!(request, RemoteDesktopHelperRequest::Close);
        for event in backend.handle_request(request) {
            write_event_line(writer, &event)?;
        }
        if should_close {
            break;
        }
    }

    Ok(handled)
}
