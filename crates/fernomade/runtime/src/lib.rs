// SPDX-License-Identifier: Apache-2.0

//! Deterministic host-facing session runtime without socket or terminal access.

use fernomade_crypto::SessionKey;
use fernomade_session::{ClientProtocol, ReceivedState, SessionError};
use fernomade_wire::{
    ByteRun, Instruction, InstructionBatch, MessageError, SESSION_CONTROL_CAPABILITY,
    SESSION_CREATE_REQUEST, SESSION_CREATED, SESSION_LIST_REQUEST, SESSION_LIST_RESPONSE,
    SESSION_SWITCH_REQUEST, SESSION_SWITCHED, SessionControl, ViewportSize,
};
use std::collections::VecDeque;
use std::fmt;

const INITIAL_RETRANSMIT_MILLISECONDS: u64 = 1_000;
const MINIMUM_RETRANSMIT_MILLISECONDS: u64 = 250;
const MAXIMUM_RETRANSMIT_MILLISECONDS: u64 = 10_000;
const SERVER_UNRESPONSIVE_MILLISECONDS: u64 = 30_000;
const MAXIMUM_QUEUED_INPUT_BYTES: usize = 1024 * 1024;
const SHUTDOWN_RETRY_LIMIT: u8 = 16;
const SHUTDOWN_TIMEOUT_MILLISECONDS: u64 = 10_000;
const MOSH_GO_NO_ECHO_TIMESTAMP: u16 = 0;
const STANDARD_MOSH_NO_ECHO_TIMESTAMP: u16 = u16::MAX;

/// Version of the host-facing runtime contract documented by this crate.
pub const EMBEDDING_API_VERSION: u16 = 10;

/// A monotonic millisecond value supplied by the embedding host.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicTime(u64);

impl MonotonicTime {
    #[must_use]
    pub const fn from_milliseconds(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn milliseconds(self) -> u64 {
        self.0
    }

    fn elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// An effect the host must apply in order.
#[derive(Eq, PartialEq)]
pub enum SessionAction {
    SendDatagram(Vec<u8>),
    WriteTerminal(Vec<u8>),
    ResizeTerminal {
        columns: u16,
        rows: u16,
    },
    AcknowledgePrediction(u64),
    RemoteStateAdvanced(u64),
    ConnectionStateChanged(ConnectionState),
    RoundTripEstimate(u16),
    CapabilitiesChanged(Vec<u8>),
    RemoteSessionControl {
        kind: u32,
        payload: Vec<u8>,
    },
    SessionLifecycleChanged(SessionLifecycle),
    ShutdownComplete(ShutdownOutcome),
    UdpBindingChanged(u64),
    /// Reports content-free protocol metadata to an embedding host.
    Diagnostic(DiagnosticEvent),
}

impl fmt::Debug for SessionAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SendDatagram(bytes) => formatter
                .debug_struct("SendDatagram")
                .field("bytes", &bytes.len())
                .finish(),
            Self::WriteTerminal(bytes) => formatter
                .debug_struct("WriteTerminal")
                .field("bytes", &bytes.len())
                .finish(),
            Self::ResizeTerminal { columns, rows } => formatter
                .debug_struct("ResizeTerminal")
                .field("columns", columns)
                .field("rows", rows)
                .finish(),
            Self::AcknowledgePrediction(value) => formatter
                .debug_tuple("AcknowledgePrediction")
                .field(value)
                .finish(),
            Self::RemoteStateAdvanced(value) => formatter
                .debug_tuple("RemoteStateAdvanced")
                .field(value)
                .finish(),
            Self::ConnectionStateChanged(value) => formatter
                .debug_tuple("ConnectionStateChanged")
                .field(value)
                .finish(),
            Self::RoundTripEstimate(value) => formatter
                .debug_tuple("RoundTripEstimate")
                .field(value)
                .finish(),
            Self::CapabilitiesChanged(value) => formatter
                .debug_struct("CapabilitiesChanged")
                .field("bytes", &value.len())
                .finish(),
            Self::RemoteSessionControl { kind, payload } => formatter
                .debug_struct("RemoteSessionControl")
                .field("kind", kind)
                .field("payload_bytes", &payload.len())
                .finish(),
            Self::SessionLifecycleChanged(value) => formatter
                .debug_tuple("SessionLifecycleChanged")
                .field(value)
                .finish(),
            Self::ShutdownComplete(value) => formatter
                .debug_tuple("ShutdownComplete")
                .field(value)
                .finish(),
            Self::UdpBindingChanged(value) => formatter
                .debug_tuple("UdpBindingChanged")
                .field(value)
                .finish(),
            Self::Diagnostic(value) => formatter.debug_tuple("Diagnostic").field(value).finish(),
        }
    }
}

/// Content-free protocol metadata suitable for host-controlled diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticEvent {
    ConnectionStateChanged {
        state: ConnectionState,
    },
    FreshUpdatePrepared {
        state_id: u64,
        datagram_count: u64,
        datagram_bytes: u64,
        instruction_count: u64,
        input_bytes: u64,
    },
    RetransmissionPrepared {
        state_id: u64,
        datagram_count: u64,
        datagram_bytes: u64,
        retransmit_delay_milliseconds: u64,
    },
    InboundUpdateAccepted {
        packet_counter: u64,
        base_state: u64,
        target_state: u64,
        acknowledged_state: u64,
        discard_before: u64,
        delta_bytes: u64,
        advances_remote_state: bool,
    },
    RoundTripUpdated {
        milliseconds: u16,
    },
    SessionLifecycleChanged {
        state: SessionLifecycle,
    },
    UdpBindingChanged {
        generation: u64,
    },
    ShutdownStarted,
    ShutdownComplete {
        outcome: ShutdownOutcome,
    },
}

/// Describes why the bounded SSP shutdown handshake ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    Acknowledged,
    PeerRequested,
    TimedOut,
}

/// Describes whether an embedding host is actively driving the session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionLifecycle {
    #[default]
    Running,
    Paused,
    Cancelled,
}

/// A terminal input event supplied by an embedding host without using stdio.
#[derive(Eq, PartialEq)]
pub enum TerminalInputEvent {
    Bytes(Vec<u8>),
    Resize { columns: u16, rows: u16 },
}

impl fmt::Debug for TerminalInputEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => formatter
                .debug_struct("Bytes")
                .field("length", &bytes.len())
                .finish(),
            Self::Resize { columns, rows } => formatter
                .debug_struct("Resize")
                .field("columns", columns)
                .field("rows", rows)
                .finish(),
        }
    }
}

/// Describes whether an authenticated server session has made recent progress.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionState {
    /// No authenticated server state has been received yet.
    #[default]
    Connecting,
    /// Authenticated server traffic is arriving normally.
    Connected,
    /// A previously connected session has stopped responding temporarily.
    Interrupted,
}

/// A read-only snapshot of content-free SSP transport state for an embedding host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportStatus {
    pub acked_by_remote: u64,
    pub sent_num: u64,
    pub last_recv_old_num: u64,
    pub last_recv_new_num: u64,
    pub throwaway_num: u64,
    pub retransmit_timeout_milliseconds: u64,
}

/// A raw SSP update for hosts that own terminal-state reconstruction.
#[derive(Eq, PartialEq)]
pub struct RawRemoteUpdate {
    pub old_num: u64,
    pub new_num: u64,
    pub ack_num: u64,
    pub throwaway_num: u64,
    pub diff: Vec<u8>,
}

impl fmt::Debug for RawRemoteUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawRemoteUpdate")
            .field("old_num", &self.old_num)
            .field("new_num", &self.new_num)
            .field("ack_num", &self.ack_num)
            .field("throwaway_num", &self.throwaway_num)
            .field("diff_bytes", &self.diff.len())
            .finish()
    }
}

/// Owns synchronization, timers, and bounded local input pending transport.
pub struct SessionRuntime {
    protocol: ClientProtocol,
    queued_instructions: VecDeque<Instruction>,
    queued_input_bytes: usize,
    last_send: Option<MonotonicTime>,
    last_receive: MonotonicTime,
    retransmit_milliseconds: u64,
    smoothed_round_trip_milliseconds: Option<u64>,
    round_trip_variation_milliseconds: u64,
    association: AssociationState,
    acknowledgement_due: bool,
    received_server_state: bool,
    round_trip_milliseconds: Option<u16>,
    connection_state: ConnectionState,
    lifecycle: SessionLifecycle,
    udp_binding_generation: u64,
    shutdown: ShutdownState,
    peer_shutdown_acknowledgement_due: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownState {
    Open,
    LocalRequested {
        started_at: MonotonicTime,
        transmissions: u8,
    },
    PeerRequested,
    Complete(ShutdownOutcome),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssociationState {
    Due,
    Sent,
}

struct ProcessedDatagram {
    actions: Vec<SessionAction>,
    raw_update: Option<RawRemoteUpdate>,
}

impl SessionRuntime {
    #[must_use]
    pub fn new(key: SessionKey, now: MonotonicTime) -> Self {
        Self {
            protocol: ClientProtocol::new(key),
            queued_instructions: VecDeque::new(),
            queued_input_bytes: 0,
            last_send: None,
            last_receive: now,
            retransmit_milliseconds: INITIAL_RETRANSMIT_MILLISECONDS,
            smoothed_round_trip_milliseconds: None,
            round_trip_variation_milliseconds: 0,
            association: AssociationState::Due,
            acknowledgement_due: false,
            received_server_state: false,
            round_trip_milliseconds: None,
            connection_state: ConnectionState::Connecting,
            lifecycle: SessionLifecycle::Running,
            udp_binding_generation: 0,
            shutdown: ShutdownState::Open,
            peer_shutdown_acknowledgement_due: false,
        }
    }

    /// Queues a host-provided terminal event without reading process stdio.
    ///
    /// # Errors
    ///
    /// Returns an error if the session is paused, cancelled, shutting down, or
    /// the input queue would exceed its fixed memory bound.
    pub fn queue_terminal_event(&mut self, event: TerminalInputEvent) -> Result<(), RuntimeError> {
        self.ensure_accepting_input()?;
        match event {
            TerminalInputEvent::Bytes(bytes) => self.queue_input(bytes),
            TerminalInputEvent::Resize { columns, rows } => {
                self.queue_resize(columns, rows);
                Ok(())
            }
        }
    }

    /// Queues terminal input under a fixed memory bound.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or if total unsent
    /// input would exceed the bound.
    pub fn queue_input(&mut self, bytes: Vec<u8>) -> Result<(), RuntimeError> {
        self.ensure_accepting_input()?;
        let new_size = self
            .queued_input_bytes
            .checked_add(bytes.len())
            .ok_or(RuntimeError::InputQueueFull)?;
        if new_size > MAXIMUM_QUEUED_INPUT_BYTES {
            return Err(RuntimeError::InputQueueFull);
        }
        self.queued_input_bytes = new_size;
        self.queued_instructions.push_back(Instruction {
            bytes: Some(ByteRun { value: bytes }),
            viewport: None,
            marker: None,
            session_control: None,
        });
        Ok(())
    }

    pub fn queue_resize(&mut self, columns: u16, rows: u16) {
        if self.lifecycle != SessionLifecycle::Running || self.shutdown != ShutdownState::Open {
            return;
        }
        self.protocol.set_initial_remote_viewport(columns, rows);
        let viewport = ViewportSize {
            columns: u64::from(columns),
            rows: u64::from(rows),
        };
        self.queued_instructions.push_back(Instruction {
            bytes: None,
            viewport: Some(viewport),
            marker: None,
            session_control: None,
        });
    }

    /// Sets capability bytes advertised on subsequent protocol updates.
    pub fn set_local_capabilities(&mut self, capabilities: Vec<u8>) {
        self.protocol.set_local_capabilities(capabilities);
    }

    /// Returns the latest non-empty capability advertisement received from the peer.
    #[must_use]
    pub fn remote_capabilities(&self) -> &[u8] {
        self.protocol.remote_capabilities()
    }

    /// Reports whether a capability bit is present in both peers' first capability byte.
    #[must_use]
    pub fn has_negotiated_capability(&self, capability: u8) -> bool {
        self.protocol.has_negotiated_capability(capability)
    }

    /// Queues a mosh-go session-control extension after capability negotiation.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or the peer did not
    /// negotiate the session-control capability.
    pub fn queue_session_control(
        &mut self,
        kind: u32,
        payload: Vec<u8>,
    ) -> Result<(), RuntimeError> {
        self.ensure_accepting_input()?;
        if !self.has_negotiated_capability(SESSION_CONTROL_CAPABILITY) {
            return Err(RuntimeError::CapabilityNotNegotiated);
        }
        let new_size = self
            .queued_input_bytes
            .checked_add(payload.len())
            .ok_or(RuntimeError::InputQueueFull)?;
        if new_size > MAXIMUM_QUEUED_INPUT_BYTES {
            return Err(RuntimeError::InputQueueFull);
        }
        self.queued_input_bytes = new_size;
        self.queued_instructions.push_back(Instruction {
            bytes: None,
            viewport: None,
            marker: None,
            session_control: Some(SessionControl { kind, payload }),
        });
        Ok(())
    }

    /// Queues a negotiated mosh-go session-list request.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or session control
    /// was not negotiated.
    pub fn queue_session_list_request(&mut self) -> Result<(), RuntimeError> {
        self.queue_session_control(SESSION_LIST_REQUEST, Vec::new())
    }

    /// Queues a negotiated mosh-go session-list response with host-defined payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or session control
    /// was not negotiated.
    pub fn queue_session_list_response(&mut self, payload: Vec<u8>) -> Result<(), RuntimeError> {
        self.queue_session_control(SESSION_LIST_RESPONSE, payload)
    }

    /// Queues a negotiated mosh-go session-switch request with host-defined payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or session control
    /// was not negotiated.
    pub fn queue_session_switch_request(&mut self, payload: Vec<u8>) -> Result<(), RuntimeError> {
        self.queue_session_control(SESSION_SWITCH_REQUEST, payload)
    }

    /// Queues a negotiated mosh-go session-switch completion with host-defined payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or session control
    /// was not negotiated.
    pub fn queue_session_switched(&mut self, payload: Vec<u8>) -> Result<(), RuntimeError> {
        self.queue_session_control(SESSION_SWITCHED, payload)
    }

    /// Queues a negotiated mosh-go session-create request with host-defined payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or session control
    /// was not negotiated.
    pub fn queue_session_create_request(&mut self, payload: Vec<u8>) -> Result<(), RuntimeError> {
        self.queue_session_control(SESSION_CREATE_REQUEST, payload)
    }

    /// Queues a negotiated mosh-go session-create completion with host-defined payload bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime cannot accept input or session control
    /// was not negotiated.
    pub fn queue_session_created(&mut self, payload: Vec<u8>) -> Result<(), RuntimeError> {
        self.queue_session_control(SESSION_CREATED, payload)
    }

    /// Pauses timers and rejects new host input until `resume` is called.
    #[must_use]
    pub fn pause(&mut self) -> Vec<SessionAction> {
        if self.lifecycle != SessionLifecycle::Running {
            return Vec::new();
        }
        self.lifecycle = SessionLifecycle::Paused;
        lifecycle_actions(SessionLifecycle::Paused)
    }

    /// Cancels further protocol work and clears queued, unsent host input.
    ///
    /// The embedding host should drop the runtime after applying the returned
    /// event so its in-memory session key is released immediately.
    #[must_use]
    pub fn cancel(&mut self) -> Vec<SessionAction> {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return Vec::new();
        }
        self.lifecycle = SessionLifecycle::Cancelled;
        self.queued_instructions.clear();
        self.queued_input_bytes = 0;
        lifecycle_actions(SessionLifecycle::Cancelled)
    }

    /// Starts the bounded SSP clean-shutdown handshake.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime is paused or cancelled.
    pub fn request_shutdown(
        &mut self,
        now: MonotonicTime,
    ) -> Result<Vec<SessionAction>, RuntimeError> {
        self.ensure_running()?;
        if self.shutdown == ShutdownState::Open {
            self.shutdown = ShutdownState::LocalRequested {
                started_at: now,
                transmissions: 0,
            };
            return Ok(vec![SessionAction::Diagnostic(
                DiagnosticEvent::ShutdownStarted,
            )]);
        }
        Ok(Vec::new())
    }

    /// Reports a host-managed local UDP rebind without changing the trusted peer.
    #[must_use]
    pub fn notify_udp_rebound(&mut self) -> Vec<SessionAction> {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return Vec::new();
        }
        self.udp_binding_generation = self.udp_binding_generation.saturating_add(1);
        self.force_next_send();
        vec![
            SessionAction::UdpBindingChanged(self.udp_binding_generation),
            SessionAction::Diagnostic(DiagnosticEvent::UdpBindingChanged {
                generation: self.udp_binding_generation,
            }),
        ]
    }

    /// Re-arms liveness after the host reports a system resume or another
    /// interval in which network progress could not have been observed.
    pub fn resume(&mut self, now: MonotonicTime) {
        self.resume_state(now);
    }

    /// Resumes a paused session and returns the structured lifecycle event.
    #[must_use]
    pub fn resume_with_actions(&mut self, now: MonotonicTime) -> Vec<SessionAction> {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return Vec::new();
        }
        let was_paused = self.lifecycle == SessionLifecycle::Paused;
        self.resume_state(now);
        if was_paused {
            lifecycle_actions(SessionLifecycle::Running)
        } else {
            Vec::new()
        }
    }

    /// Makes the next `poll` send immediately without changing SSP state.
    ///
    /// This mirrors mosh-go's `ForceNextSend` for embedding transports that
    /// have become writable again and want to re-drive the existing session.
    pub fn force_next_send(&mut self) {
        if self.lifecycle == SessionLifecycle::Running
            && !matches!(self.shutdown, ShutdownState::Complete(_))
        {
            self.last_send = None;
        }
    }

    fn resume_state(&mut self, now: MonotonicTime) {
        if self.lifecycle == SessionLifecycle::Cancelled {
            return;
        }
        self.lifecycle = SessionLifecycle::Running;
        self.last_receive = now;
        // A resumed or rebound transport should probe immediately with its existing SSP state.
        self.force_next_send();
    }

    /// Accepts an authenticated server datagram and returns terminal effects.
    ///
    /// # Errors
    ///
    /// Returns an error for authentication, replay, framing, decompression, or
    /// unsupported message failures.
    pub fn receive_datagram(
        &mut self,
        packet: &[u8],
        now: MonotonicTime,
    ) -> Result<Vec<SessionAction>, RuntimeError> {
        Ok(self.receive_datagram_inner(packet, now)?.actions)
    }

    /// Accepts a datagram and returns its raw SSP diff without terminal actions.
    ///
    /// This mirrors mosh-go's manual raw-receive mode for hosts that reconstruct
    /// terminal state themselves. It returns `None` for heartbeats, incomplete
    /// fragments, duplicates, empty diffs, and updates that do not advance
    /// remote state.
    ///
    /// # Errors
    ///
    /// Returns an error for authentication, replay, framing, decompression, or
    /// unsupported message failures.
    pub fn receive_datagram_raw(
        &mut self,
        packet: &[u8],
        now: MonotonicTime,
    ) -> Result<Option<RawRemoteUpdate>, RuntimeError> {
        Ok(self.receive_datagram_inner(packet, now)?.raw_update)
    }

    /// Processes a raw datagram with mosh-go's best-effort receive semantics.
    ///
    /// Authentication, replay, framing, decompression, and message failures
    /// are treated as a dropped datagram and return `None`.
    #[must_use]
    pub fn receive_datagram_raw_lossy(
        &mut self,
        packet: &[u8],
        now: MonotonicTime,
    ) -> Option<RawRemoteUpdate> {
        self.receive_datagram_raw(packet, now).ok().flatten()
    }

    fn receive_datagram_inner(
        &mut self,
        packet: &[u8],
        now: MonotonicTime,
    ) -> Result<ProcessedDatagram, RuntimeError> {
        self.ensure_running()?;
        let previous_capabilities = self.protocol.remote_capabilities().to_vec();
        let receipt = self
            .protocol
            .ingest_packet(packet)
            .map_err(RuntimeError::Session)?;
        self.last_receive = now;
        let mut actions = Vec::new();
        if self.protocol.remote_capabilities() != previous_capabilities {
            actions.push(SessionAction::CapabilitiesChanged(
                self.protocol.remote_capabilities().to_vec(),
            ));
        }
        if self.connection_state != ConnectionState::Connected {
            self.connection_state = ConnectionState::Connected;
            actions.push(SessionAction::ConnectionStateChanged(
                ConnectionState::Connected,
            ));
            actions.push(SessionAction::Diagnostic(
                DiagnosticEvent::ConnectionStateChanged {
                    state: ConnectionState::Connected,
                },
            ));
        }
        if is_round_trip_sample(receipt.echoed_timestamp) {
            let elapsed = timestamp(now).wrapping_sub(receipt.echoed_timestamp);
            if elapsed <= 30_000 {
                actions.extend(self.record_round_trip_sample(elapsed));
            }
        }
        let raw_update = receipt
            .state
            .map(|state| self.apply_received_state(state, &mut actions))
            .transpose()?;
        Ok(ProcessedDatagram {
            actions,
            raw_update: raw_update.flatten(),
        })
    }

    fn apply_received_state(
        &mut self,
        state: ReceivedState,
        actions: &mut Vec<SessionAction>,
    ) -> Result<Option<RawRemoteUpdate>, RuntimeError> {
        let ReceivedState {
            packet_counter,
            advances_remote_state,
            raw_delta,
            update,
            ..
        } = state;
        self.acknowledgement_due |= advances_remote_state;
        self.received_server_state = true;
        if update.target_state == u64::MAX {
            self.peer_shutdown_acknowledgement_due = true;
            if self.shutdown == ShutdownState::Open {
                self.shutdown = ShutdownState::PeerRequested;
            }
        }
        actions.push(SessionAction::Diagnostic(
            DiagnosticEvent::InboundUpdateAccepted {
                packet_counter,
                base_state: update.base_state,
                target_state: update.target_state,
                acknowledged_state: update.acknowledged_state,
                discard_before: update.discard_before,
                delta_bytes: usize_as_u64(update.delta.len()),
                advances_remote_state,
            },
        ));
        let raw_update = if advances_remote_state && !raw_delta.is_empty() {
            Some(RawRemoteUpdate {
                old_num: update.base_state,
                new_num: update.target_state,
                ack_num: update.acknowledged_state,
                throwaway_num: update.discard_before,
                diff: raw_delta,
            })
        } else {
            None
        };
        if !advances_remote_state {
            return Ok(raw_update);
        }
        actions.push(SessionAction::RemoteStateAdvanced(update.target_state));
        let instructions = update
            .decode_instructions()
            .map_err(RuntimeError::Message)?;
        actions.extend(
            instructions
                .instructions
                .into_iter()
                .flat_map(|instruction| {
                    let mut actions = Vec::with_capacity(2);
                    if let Some(bytes) = instruction.bytes {
                        actions.push(SessionAction::WriteTerminal(bytes.value));
                    }
                    if let Some(viewport) = instruction.viewport {
                        let dimensions = (
                            u16::try_from(viewport.columns),
                            u16::try_from(viewport.rows),
                        );
                        if let (Ok(columns), Ok(rows)) = dimensions {
                            actions.push(SessionAction::ResizeTerminal { columns, rows });
                        }
                    }
                    if let Some(marker) = instruction.marker {
                        actions.push(SessionAction::AcknowledgePrediction(marker.value));
                    }
                    if let Some(control) = instruction.session_control {
                        actions.push(SessionAction::RemoteSessionControl {
                            kind: control.kind,
                            payload: control.payload,
                        });
                    }
                    actions
                }),
        );
        Ok(raw_update)
    }

    /// Processes a datagram with mosh-go's best-effort receive semantics.
    ///
    /// Authentication, replay, framing, decompression, and message failures
    /// are treated as a dropped datagram and produce no host actions.
    #[must_use]
    pub fn receive_datagram_lossy(
        &mut self,
        packet: &[u8],
        now: MonotonicTime,
    ) -> Vec<SessionAction> {
        self.receive_datagram(packet, now).unwrap_or_default()
    }

    /// Advances timers and returns datagrams that should be sent now.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol encoding failure.
    pub fn poll(&mut self, now: MonotonicTime) -> Result<Vec<SessionAction>, RuntimeError> {
        if self.lifecycle != SessionLifecycle::Running {
            return Ok(Vec::new());
        }
        if matches!(self.shutdown, ShutdownState::Complete(_)) {
            return Ok(Vec::new());
        }
        let mut actions = Vec::new();
        if self.association == AssociationState::Due {
            let packets = self
                .protocol
                .build_association(timestamp(now))
                .map_err(RuntimeError::Session)?;
            let diagnostic = fresh_update_diagnostic(0, &packets, 0, 0);
            self.association = AssociationState::Sent;
            self.last_send = Some(now);
            actions.extend(send_actions(packets));
            actions.push(SessionAction::Diagnostic(diagnostic));
        }
        if self.protocol.local_shutdown_acknowledged() && !self.peer_shutdown_acknowledgement_due {
            return Ok(self.complete_shutdown(ShutdownOutcome::Acknowledged));
        }
        if !self.peer_shutdown_acknowledgement_due && self.shutdown_timed_out(now) {
            return Ok(self.complete_shutdown(ShutdownOutcome::TimedOut));
        }
        actions.extend(
            self.connection_state_action(now)
                .into_iter()
                .flat_map(|state| {
                    [
                        SessionAction::ConnectionStateChanged(state),
                        SessionAction::Diagnostic(DiagnosticEvent::ConnectionStateChanged {
                            state,
                        }),
                    ]
                }),
        );
        let has_queued_state = !self.queued_instructions.is_empty();
        let local_shutdown_unsent = matches!(self.shutdown, ShutdownState::LocalRequested { .. })
            && !self.protocol.local_shutdown_sent();
        if (has_queued_state || local_shutdown_unsent) && !self.protocol.has_pending_update() {
            let (packets, diagnostic) = self.build_queued_update(now)?;
            actions.extend(send_actions(packets));
            actions.push(SessionAction::Diagnostic(diagnostic));
            self.record_shutdown_transmission();
            self.complete_peer_shutdown_after_ack(&mut actions);
            return Ok(actions);
        }

        if self.protocol.has_pending_update() {
            if !self.acknowledgement_due {
                if let Some(last_send) = self.last_send {
                    if now.elapsed_since(last_send) < self.retransmit_milliseconds {
                        return Ok(actions);
                    }
                }
            }
            let packets = self
                .protocol
                .retransmit_pending(timestamp(now))
                .map_err(RuntimeError::Session)?;
            let diagnostic = retransmission_diagnostic(
                self.protocol.latest_local_state(),
                &packets,
                self.retransmit_milliseconds,
            );
            self.last_send = Some(now);
            self.acknowledgement_due = false;
            actions.extend(send_actions(packets));
            actions.push(SessionAction::Diagnostic(diagnostic));
            self.record_shutdown_transmission();
            self.complete_peer_shutdown_after_ack(&mut actions);
            return Ok(actions);
        }

        let heartbeat_due = self
            .last_send
            .is_none_or(|last_send| now.elapsed_since(last_send) >= self.retransmit_milliseconds);
        if !self.acknowledgement_due && !heartbeat_due {
            return Ok(actions);
        }
        let packets = self
            .protocol
            .build_acknowledgement(timestamp(now))
            .map_err(RuntimeError::Session)?;
        let diagnostic =
            fresh_update_diagnostic(self.protocol.latest_local_state(), &packets, 0, 0);
        self.last_send = Some(now);
        self.acknowledgement_due = false;
        actions.extend(send_actions(packets));
        actions.push(SessionAction::Diagnostic(diagnostic));
        self.complete_peer_shutdown_after_ack(&mut actions);
        Ok(actions)
    }

    fn build_queued_update(
        &mut self,
        now: MonotonicTime,
    ) -> Result<(Vec<Vec<u8>>, DiagnosticEvent), RuntimeError> {
        let state_id = if (matches!(self.shutdown, ShutdownState::LocalRequested { .. })
            && !self.protocol.local_shutdown_sent())
            || (self.peer_shutdown_acknowledgement_due
                && self.protocol.local_shutdown_acknowledged())
        {
            u64::MAX
        } else {
            self.protocol.next_local_state()
        };
        let input_bytes = self.queued_input_bytes;
        let instructions = InstructionBatch {
            instructions: self.queued_instructions.drain(..).collect(),
        };
        let instruction_count = instructions.instructions.len();
        self.queued_input_bytes = 0;
        self.acknowledgement_due = false;
        let packets = if self.peer_shutdown_acknowledgement_due
            && self.protocol.local_shutdown_acknowledged()
        {
            self.protocol
                .build_shutdown_ack(timestamp(now))
                .map_err(RuntimeError::Session)?
        } else if matches!(self.shutdown, ShutdownState::LocalRequested { .. })
            && !self.protocol.local_shutdown_sent()
        {
            self.protocol
                .build_shutdown(timestamp(now), &instructions)
                .map_err(RuntimeError::Session)?
        } else {
            self.protocol
                .build_update(timestamp(now), &instructions)
                .map_err(RuntimeError::Session)?
        };
        self.last_send = Some(now);
        let diagnostic =
            fresh_update_diagnostic(state_id, &packets, instruction_count, input_bytes);
        Ok((packets, diagnostic))
    }

    #[must_use]
    pub fn milliseconds_until_next_poll(&self, now: MonotonicTime) -> u64 {
        if self.lifecycle != SessionLifecycle::Running
            || matches!(self.shutdown, ShutdownState::Complete(_))
        {
            return u64::MAX;
        }
        let can_send_queued = !self.protocol.has_pending_update();
        let protocol_wait = if self.association == AssociationState::Due
            || self.acknowledgement_due
            || (can_send_queued && !self.queued_instructions.is_empty())
            || (matches!(self.shutdown, ShutdownState::LocalRequested { .. })
                && !self.protocol.local_shutdown_sent()
                && can_send_queued)
        {
            0
        } else if self.protocol.has_pending_update() {
            self.last_send.map_or(0, |last_send| {
                self.retransmit_milliseconds
                    .saturating_sub(now.elapsed_since(last_send))
            })
        } else {
            self.last_send.map_or(0, |last_send| {
                self.retransmit_milliseconds
                    .saturating_sub(now.elapsed_since(last_send))
            })
        };
        match self.shutdown {
            ShutdownState::LocalRequested {
                started_at,
                transmissions,
            } => {
                let timeout_wait = if transmissions >= SHUTDOWN_RETRY_LIMIT {
                    0
                } else {
                    SHUTDOWN_TIMEOUT_MILLISECONDS.saturating_sub(now.elapsed_since(started_at))
                };
                protocol_wait.min(timeout_wait)
            }
            _ => protocol_wait,
        }
    }

    #[must_use]
    pub const fn has_received_server_state(&self) -> bool {
        self.received_server_state
    }

    /// Reports whether the server has responded recently without ending an
    /// otherwise recoverable Mosh session when connectivity is intermittent.
    #[must_use]
    pub fn is_server_responsive(&self, now: MonotonicTime) -> bool {
        now.elapsed_since(self.last_receive) < SERVER_UNRESPONSIVE_MILLISECONDS
    }

    #[must_use]
    pub const fn round_trip_milliseconds(&self) -> Option<u16> {
        self.round_trip_milliseconds
    }

    /// Returns a content-free snapshot equivalent to mosh-go's transport queries.
    #[must_use]
    pub fn transport_status(&self) -> TransportStatus {
        let state_numbers = self.protocol.transport_state_numbers();
        TransportStatus {
            acked_by_remote: state_numbers.acked_by_remote,
            sent_num: state_numbers.sent_num,
            last_recv_old_num: state_numbers.last_recv_old_num,
            last_recv_new_num: state_numbers.last_recv_new_num,
            throwaway_num: state_numbers.throwaway_num,
            retransmit_timeout_milliseconds: self.retransmit_milliseconds,
        }
    }

    /// Returns the SSP frame that will contain input queued before the next poll.
    #[must_use]
    pub fn prediction_frame_id(&self) -> u64 {
        self.protocol.next_local_state()
    }

    #[must_use]
    pub const fn connection_state(&self) -> ConnectionState {
        self.connection_state
    }

    #[must_use]
    pub const fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn shutdown_outcome(&self) -> Option<ShutdownOutcome> {
        match self.shutdown {
            ShutdownState::Complete(outcome) => Some(outcome),
            _ => None,
        }
    }

    #[must_use]
    pub const fn shutdown_in_progress(&self) -> bool {
        !matches!(
            self.shutdown,
            ShutdownState::Open | ShutdownState::Complete(_)
        )
    }

    /// Returns the time since the most recent authenticated server datagram.
    #[must_use]
    pub fn milliseconds_since_server_response(&self, now: MonotonicTime) -> u64 {
        now.elapsed_since(self.last_receive)
    }

    fn connection_state_action(&mut self, now: MonotonicTime) -> Option<ConnectionState> {
        if self.connection_state == ConnectionState::Connected && !self.is_server_responsive(now) {
            self.connection_state = ConnectionState::Interrupted;
            return Some(ConnectionState::Interrupted);
        }
        None
    }

    fn update_retransmit_timeout(&mut self, sample_milliseconds: u64) {
        if let Some(smoothed) = self.smoothed_round_trip_milliseconds {
            let difference = smoothed.abs_diff(sample_milliseconds);
            self.round_trip_variation_milliseconds = self
                .round_trip_variation_milliseconds
                .saturating_mul(3)
                .saturating_add(difference)
                / 4;
            self.smoothed_round_trip_milliseconds = Some(
                smoothed
                    .saturating_mul(7)
                    .saturating_add(sample_milliseconds)
                    / 8,
            );
        } else {
            self.smoothed_round_trip_milliseconds = Some(sample_milliseconds);
            self.round_trip_variation_milliseconds = sample_milliseconds / 2;
        }
        let smoothed = self
            .smoothed_round_trip_milliseconds
            .unwrap_or(sample_milliseconds);
        self.retransmit_milliseconds = smoothed
            .saturating_add(self.round_trip_variation_milliseconds.saturating_mul(4))
            .clamp(
                MINIMUM_RETRANSMIT_MILLISECONDS,
                MAXIMUM_RETRANSMIT_MILLISECONDS,
            );
    }

    fn record_round_trip_sample(&mut self, milliseconds: u16) -> Vec<SessionAction> {
        self.update_retransmit_timeout(u64::from(milliseconds));
        if self.round_trip_milliseconds == Some(milliseconds) {
            return Vec::new();
        }
        self.round_trip_milliseconds = Some(milliseconds);
        vec![
            SessionAction::RoundTripEstimate(milliseconds),
            SessionAction::Diagnostic(DiagnosticEvent::RoundTripUpdated { milliseconds }),
        ]
    }

    fn ensure_running(&self) -> Result<(), RuntimeError> {
        match self.lifecycle {
            SessionLifecycle::Running => Ok(()),
            SessionLifecycle::Paused => Err(RuntimeError::SessionPaused),
            SessionLifecycle::Cancelled => Err(RuntimeError::SessionCancelled),
        }
    }

    fn ensure_accepting_input(&self) -> Result<(), RuntimeError> {
        self.ensure_running()?;
        if self.shutdown != ShutdownState::Open {
            return Err(RuntimeError::ShutdownInProgress);
        }
        Ok(())
    }

    fn shutdown_timed_out(&self, now: MonotonicTime) -> bool {
        let ShutdownState::LocalRequested {
            started_at,
            transmissions,
        } = self.shutdown
        else {
            return false;
        };
        transmissions >= SHUTDOWN_RETRY_LIMIT
            || now.elapsed_since(started_at) >= SHUTDOWN_TIMEOUT_MILLISECONDS
    }

    fn record_shutdown_transmission(&mut self) {
        if let ShutdownState::LocalRequested { transmissions, .. } = &mut self.shutdown {
            *transmissions = transmissions.saturating_add(1);
        }
    }

    fn complete_peer_shutdown_after_ack(&mut self, actions: &mut Vec<SessionAction>) {
        if self.peer_shutdown_acknowledgement_due {
            self.peer_shutdown_acknowledgement_due = false;
            actions.extend(self.complete_shutdown(ShutdownOutcome::PeerRequested));
        }
    }

    fn complete_shutdown(&mut self, outcome: ShutdownOutcome) -> Vec<SessionAction> {
        self.shutdown = ShutdownState::Complete(outcome);
        vec![
            SessionAction::ShutdownComplete(outcome),
            SessionAction::Diagnostic(DiagnosticEvent::ShutdownComplete { outcome }),
        ]
    }
}

fn lifecycle_actions(state: SessionLifecycle) -> Vec<SessionAction> {
    vec![
        SessionAction::SessionLifecycleChanged(state),
        SessionAction::Diagnostic(DiagnosticEvent::SessionLifecycleChanged { state }),
    ]
}

fn fresh_update_diagnostic(
    state_id: u64,
    packets: &[Vec<u8>],
    instruction_count: usize,
    input_bytes: usize,
) -> DiagnosticEvent {
    DiagnosticEvent::FreshUpdatePrepared {
        state_id,
        datagram_count: usize_as_u64(packets.len()),
        datagram_bytes: total_packet_bytes(packets),
        instruction_count: usize_as_u64(instruction_count),
        input_bytes: usize_as_u64(input_bytes),
    }
}

fn retransmission_diagnostic(
    state_id: u64,
    packets: &[Vec<u8>],
    retransmit_delay_milliseconds: u64,
) -> DiagnosticEvent {
    DiagnosticEvent::RetransmissionPrepared {
        state_id,
        datagram_count: usize_as_u64(packets.len()),
        datagram_bytes: total_packet_bytes(packets),
        retransmit_delay_milliseconds,
    }
}

fn total_packet_bytes(packets: &[Vec<u8>]) -> u64 {
    packets.iter().fold(0_u64, |total, packet| {
        total.saturating_add(usize_as_u64(packet.len()))
    })
}

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn timestamp(now: MonotonicTime) -> u16 {
    let low_bits = now.milliseconds() & u64::from(u16::MAX);
    u16::try_from(low_bits).expect("timestamp is masked to 16 bits")
}

fn is_round_trip_sample(echoed_timestamp: u16) -> bool {
    // mosh-go uses zero before it has a peer timestamp, while standard Mosh
    // commonly uses the all-ones sentinel. Neither value is an RTT sample.
    !matches!(
        echoed_timestamp,
        MOSH_GO_NO_ECHO_TIMESTAMP | STANDARD_MOSH_NO_ECHO_TIMESTAMP
    )
}

fn send_actions(packets: Vec<Vec<u8>>) -> Vec<SessionAction> {
    packets
        .into_iter()
        .map(SessionAction::SendDatagram)
        .collect()
}

#[derive(Debug)]
pub enum RuntimeError {
    InputQueueFull,
    CapabilityNotNegotiated,
    SessionPaused,
    SessionCancelled,
    ShutdownInProgress,
    Session(SessionError),
    Message(MessageError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputQueueFull => formatter.write_str("terminal input queue is full"),
            Self::CapabilityNotNegotiated => {
                formatter.write_str("session-control capability was not negotiated")
            }
            Self::SessionPaused => formatter.write_str("session is paused"),
            Self::SessionCancelled => formatter.write_str("session is cancelled"),
            Self::ShutdownInProgress => formatter.write_str("session shutdown is in progress"),
            Self::Session(error) => write!(formatter, "session protocol failed: {error:?}"),
            Self::Message(error) => write!(formatter, "session message failed: {error:?}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
