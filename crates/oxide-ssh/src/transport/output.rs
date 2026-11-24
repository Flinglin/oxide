struct SshOutputBatcher {
    pending: Vec<u8>,
    flush_deadline: Option<Instant>,
    interactive_until: Option<Instant>,
}

impl SshOutputBatcher {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            flush_deadline: None,
            interactive_until: None,
        }
    }

    fn note_interaction(&mut self) {
        self.interactive_until =
            Some(Instant::now() + Duration::from_millis(SSH_OUTPUT_INTERACTIVE_WINDOW_MS));
        self.refresh_deadline();
    }

    fn push(&mut self, bytes: &[u8]) -> bool {
        // Transport bytes can belong to a binary protocol. Text decoders own
        // incomplete characters only after protocol consumers release display data.
        if self.pending.is_empty() {
            self.pending = bytes.to_vec();
        } else {
            self.pending.extend_from_slice(bytes);
        }
        self.refresh_deadline();
        self.pending.len() >= SSH_OUTPUT_BATCH_MAX_BYTES
    }

    fn flush_due(&self) -> Option<Instant> {
        (!self.pending.is_empty())
            .then_some(self.flush_deadline?)
            .or(None)
    }

    fn take_flush(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            self.flush_deadline = None;
            return None;
        }
        self.flush_deadline = None;
        Some(std::mem::take(&mut self.pending))
    }

    fn refresh_deadline(&mut self) {
        if self.pending.is_empty() {
            self.flush_deadline = None;
            return;
        }

        let now = Instant::now();
        let interactive = self
            .interactive_until
            .is_some_and(|deadline| deadline > now);
        let delay = if interactive {
            SSH_OUTPUT_INTERACTIVE_FLUSH_MS
        } else {
            SSH_OUTPUT_FLUSH_MS
        };
        // More output must not postpone bytes already waiting for the consumer.
        let deadline = now + Duration::from_millis(delay);
        self.flush_deadline = Some(
            self.flush_deadline
                .map_or(deadline, |current| current.min(deadline)),
        );
    }
}

#[derive(Clone)]
struct SshSftpChannelOpener {
    _connection: SshConnectionHandle,
    transport: Arc<PooledSshConnection>,
}

impl SshConnectionHandle {
    pub(crate) async fn create_sftp_session(&self) -> Result<oxide_sftp::SftpSession, SftpError> {
        let transport = self.physical::<PooledSshConnection>().ok_or_else(||
            SftpError::ChannelError("no active SSH connection is available for SFTP".into()))?;
        let opener = SshSftpChannelOpener { _connection: self.clone(), transport };
        oxide_sftp::SftpSession::new(opener, self.connection_id().to_string()).await
    }
}

impl SftpChannelOpener for SshSftpChannelOpener {
    fn audit_context(&self) -> Option<oxide_audit::AuditContext> { self.transport.audit.clone() }

    fn open_sftp_channel(&self) -> impl Future<Output = Result<russh::Channel<client::Msg>, SftpError>> + Send {
        async {
            // Auxiliary channels belong to this SFTP session's transport, never a later registry binding.
            self.transport.target.channel_open_session().await
                      .map_err(|error| match error {
                    russh::Error::ChannelOpenFailure(
                        russh::ChannelOpenFailure::ConnectFailed | russh::ChannelOpenFailure::ResourceShortage,
                    ) => SftpError::ChannelOpenRejected(error.to_string()),
                    _ => SftpError::ChannelError(error.to_string()),
                })
        }
    }
}

impl SftpExecChannelOpener for SshConnectionHandle {
    fn open_exec_channel(
        &self,
    ) -> impl Future<Output = Result<russh::Channel<client::Msg>, SftpError>> + Send {
        async {
            self.open_session_channel()
                .await
                .map_err(|error| SftpError::ChannelError(error.to_string()))
        }
    }
}
