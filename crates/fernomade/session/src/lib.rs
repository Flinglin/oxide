// SPDX-License-Identifier: Apache-2.0

//! Client-side protocol state independent of sockets and terminal adapters.

use fernomade_crypto::{OpenError, PeerRole, SealError, SecureChannel, SessionKey};
use fernomade_state::{RemoteTerminalState, StateError};
use fernomade_wire::{
    Fragment, FragmentAccumulator, FragmentError, Instruction, InstructionBatch, MessageError,
    StateUpdate, decode_compressed_update, encode_compressed_update,
};
use std::collections::{BTreeMap, VecDeque};

const NO_ECHO_TIMESTAMP: u16 = u16::MAX;
const MAXIMUM_REASSEMBLED_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_ACTIVE_ASSEMBLIES: usize = 64;
const MAXIMUM_SENT_STATES: usize = 32;
const MAXIMUM_RECEIVED_STATES: usize = 1_024;
const REPLAY_WINDOW_BITS: u64 = 128;
const DEFAULT_REMOTE_COLUMNS: u16 = 80;
const DEFAULT_REMOTE_ROWS: u16 = 24;
const TIMESTAMP_HEADER_BYTES: usize = 4;
const MINIMUM_FRAGMENT_PLAINTEXT_BYTES: usize = 14;

/// Produces authenticated client updates and accepts authenticated server states.
pub struct ClientProtocol {
    secure_channel: SecureChannel,
    next_local_state: u64,
    latest_remote_state: u64,
    last_remote_base_state: u64,
    last_remote_target_state: u64,
    latest_peer_timestamp: Option<u16>,
    assemblies: BTreeMap<u64, FragmentAccumulator>,
    local_instructions: Vec<Instruction>,
    sent_states: VecDeque<SentState>,
    received_states: BTreeMap<u64, RemoteTerminalState>,
    delivered_remote_state: RemoteTerminalState,
    remote_throwaway_floor: u64,
    local_capabilities: Vec<u8>,
    remote_capabilities: Vec<u8>,
    replay_window: ReplayWindow,
}

impl ClientProtocol {
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn new(key: SessionKey) -> Self {
        let initial_remote_state =
            RemoteTerminalState::new(DEFAULT_REMOTE_COLUMNS, DEFAULT_REMOTE_ROWS)
                .expect("the default remote viewport is valid");
        Self {
            secure_channel: SecureChannel::new(PeerRole::Client, key),
            next_local_state: 1,
            latest_remote_state: 0,
            last_remote_base_state: 0,
            last_remote_target_state: 0,
            latest_peer_timestamp: None,
            assemblies: BTreeMap::new(),
            local_instructions: Vec::new(),
            sent_states: VecDeque::from([SentState {
                state_id: 0,
                instruction_end: 0,
            }]),
            received_states: BTreeMap::from([(0, initial_remote_state.clone())]),
            delivered_remote_state: initial_remote_state,
            remote_throwaway_floor: 0,
            local_capabilities: Vec::new(),
            remote_capabilities: Vec::new(),
            replay_window: ReplayWindow::default(),
        }
    }

    /// Builds the initial empty state-zero datagram used to associate a client endpoint.
    ///
    /// This packet does not advance local SSP state or create retransmission history.
    ///
    /// # Errors
    ///
    /// Returns an error if compression, fragmentation, or authenticated encryption fails.
    pub fn build_association(&mut self, sent_timestamp: u16) -> Result<Vec<Vec<u8>>, SessionError> {
        self.build_acknowledgement(sent_timestamp)
    }

    /// Builds an empty acknowledgement or heartbeat without advancing local SSP state.
    ///
    /// # Errors
    ///
    /// Returns an error if state history, compression, fragmentation, or encryption fails.
    pub fn build_acknowledgement(
        &mut self,
        sent_timestamp: u16,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        let local_state = self
            .sent_states
            .back()
            .ok_or(SessionError::StateHistoryUnavailable)?
            .state_id;
        let mut update = StateUpdate::new(local_state, local_state, self.latest_remote_state);
        update.discard_before = self
            .sent_states
            .front()
            .ok_or(SessionError::StateHistoryUnavailable)?
            .state_id;
        update.capabilities.clone_from(&self.local_capabilities);
        let compressed = encode_compressed_update(&update).map_err(SessionError::Message)?;
        seal_fragments(
            &mut self.secure_channel,
            &compressed,
            sent_timestamp,
            self.latest_peer_timestamp,
            local_state,
        )
    }

    /// Sets extension capability bytes advertised on subsequent updates.
    pub fn set_local_capabilities(&mut self, capabilities: Vec<u8>) {
        self.local_capabilities = capabilities;
    }

    /// Returns the latest non-empty capability advertisement from the peer.
    #[must_use]
    pub fn remote_capabilities(&self) -> &[u8] {
        &self.remote_capabilities
    }

    /// Reports whether both peers advertise the requested first-byte capability bit.
    #[must_use]
    pub fn has_negotiated_capability(&self, capability: u8) -> bool {
        self.local_capabilities
            .first()
            .zip(self.remote_capabilities.first())
            .is_some_and(|(local, remote)| local & remote & capability != 0)
    }

    /// Encodes one client state update and returns one or more UDP payloads.
    ///
    /// # Errors
    ///
    /// Returns an error for state-number exhaustion, compression, excessive
    /// fragmentation, or authenticated-encryption failure.
    pub fn build_update(
        &mut self,
        sent_timestamp: u16,
        instructions: &InstructionBatch,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        let target_state = self.next_local_state;
        if target_state == u64::MAX {
            return Err(SessionError::StateExhausted);
        }
        let packets = self.build_state_update(sent_timestamp, instructions, target_state)?;
        self.next_local_state = target_state + 1;
        Ok(packets)
    }

    /// Encodes the reserved SSP shutdown state with any final queued input.
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown was already sent, state history is
    /// unavailable, or encoding and authentication fail.
    pub fn build_shutdown(
        &mut self,
        sent_timestamp: u16,
        instructions: &InstructionBatch,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        if self.local_shutdown_sent() {
            return Err(SessionError::ShutdownAlreadySent);
        }
        self.build_state_update(sent_timestamp, instructions, u64::MAX)
    }

    /// Re-sends the reserved local shutdown state to acknowledge a peer
    /// shutdown request after the peer has already acknowledged local state.
    ///
    /// # Errors
    ///
    /// Returns an error unless the acknowledged local shutdown state is the
    /// only retained state, or if encoding and authentication fail.
    pub fn build_shutdown_ack(
        &mut self,
        sent_timestamp: u16,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        if !self.local_shutdown_acknowledged() {
            return Err(SessionError::ShutdownNotAcknowledged);
        }
        let mut update = StateUpdate::new(u64::MAX, u64::MAX, self.latest_remote_state);
        update.discard_before = u64::MAX;
        update.capabilities.clone_from(&self.local_capabilities);
        update.delta = InstructionBatch {
            instructions: Vec::new(),
        }
        .encode_bytes();
        let compressed = encode_compressed_update(&update).map_err(SessionError::Message)?;
        seal_fragments(
            &mut self.secure_channel,
            &compressed,
            sent_timestamp,
            self.latest_peer_timestamp,
            u64::MAX,
        )
    }

    fn build_state_update(
        &mut self,
        sent_timestamp: u16,
        instructions: &InstructionBatch,
        target_state: u64,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        let base_state = self
            .sent_states
            .back()
            .ok_or(SessionError::StateHistoryUnavailable)?
            .state_id;
        let discard_before = self
            .sent_states
            .front()
            .ok_or(SessionError::StateHistoryUnavailable)?
            .state_id;
        let instruction_end = self
            .local_instructions
            .len()
            .checked_add(instructions.instructions.len())
            .ok_or(SessionError::StateExhausted)?;
        let mut update = StateUpdate::new(base_state, target_state, self.latest_remote_state);
        update.discard_before = discard_before;
        update.delta = instructions.encode_bytes();
        update.capabilities.clone_from(&self.local_capabilities);
        let compressed = encode_compressed_update(&update).map_err(SessionError::Message)?;
        let packets = seal_fragments(
            &mut self.secure_channel,
            &compressed,
            sent_timestamp,
            self.latest_peer_timestamp,
            target_state,
        )?;
        self.local_instructions
            .extend(instructions.instructions.iter().cloned());
        self.sent_states.push_back(SentState {
            state_id: target_state,
            instruction_end,
        });
        self.limit_sent_states();
        Ok(packets)
    }

    /// Rebuilds all unacknowledged input from the confirmed state and seals it
    /// with fresh packet counters after a timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when no state awaits acknowledgement or when fragment
    /// encoding or authenticated encryption fails.
    pub fn retransmit_pending(
        &mut self,
        sent_timestamp: u16,
    ) -> Result<Vec<Vec<u8>>, SessionError> {
        if !self.has_pending_update() {
            return Err(SessionError::NoPendingUpdate);
        }
        let confirmed = self
            .sent_states
            .front()
            .ok_or(SessionError::StateHistoryUnavailable)?;
        let latest = self
            .sent_states
            .back()
            .ok_or(SessionError::StateHistoryUnavailable)?;
        let instructions = InstructionBatch {
            instructions: self.local_instructions
                [confirmed.instruction_end..latest.instruction_end]
                .to_vec(),
        };
        let mut update = StateUpdate::new(
            confirmed.state_id,
            latest.state_id,
            self.latest_remote_state,
        );
        update.discard_before = confirmed.state_id;
        update.delta = instructions.encode_bytes();
        update.capabilities.clone_from(&self.local_capabilities);
        let compressed = encode_compressed_update(&update).map_err(SessionError::Message)?;
        seal_fragments(
            &mut self.secure_channel,
            &compressed,
            sent_timestamp,
            self.latest_peer_timestamp,
            latest.state_id,
        )
    }

    /// Accepts one UDP payload and returns a complete server state when ready.
    ///
    /// # Errors
    ///
    /// Returns an error for failed authentication, invalid fragments, bounded
    /// reassembly failures or invalid messages.
    pub fn ingest(&mut self, packet: &[u8]) -> Result<Option<ReceivedState>, SessionError> {
        self.ingest_packet(packet).map(|receipt| receipt.state)
    }

    /// Accepts one UDP payload and reports every authenticated packet, including
    /// timestamp-only heartbeats and incomplete fragmented messages.
    ///
    /// # Errors
    ///
    /// Returns an error for failed authentication, replay, framing, bounded
    /// reassembly failures or invalid messages.
    pub fn ingest_packet(&mut self, packet: &[u8]) -> Result<PacketReceipt, SessionError> {
        let authenticated = self
            .secure_channel
            .open(packet)
            .map_err(SessionError::Open)?;
        if !self.replay_window.accept(authenticated.counter) {
            return Err(SessionError::ReplayDetected);
        }
        let timestamp_header = authenticated
            .plaintext
            .get(..TIMESTAMP_HEADER_BYTES)
            .ok_or(SessionError::Fragment(FragmentError::HeaderTooShort))?;
        let sent_timestamp = u16::from_be_bytes([timestamp_header[0], timestamp_header[1]]);
        let echoed_timestamp = u16::from_be_bytes([timestamp_header[2], timestamp_header[3]]);
        self.latest_peer_timestamp = Some(sent_timestamp);
        if authenticated.plaintext.len() < MINIMUM_FRAGMENT_PLAINTEXT_BYTES {
            return Ok(PacketReceipt {
                packet_counter: authenticated.counter,
                sent_timestamp,
                echoed_timestamp,
                state: None,
            });
        }
        let fragment = Fragment::parse(&authenticated.plaintext).map_err(SessionError::Fragment)?;
        let header = fragment.header;
        if !self.assemblies.contains_key(&header.state_id)
            && self.assemblies.len() >= MAXIMUM_ACTIVE_ASSEMBLIES
        {
            return Err(SessionError::TooManyActiveAssemblies);
        }
        let assembly = self.assemblies.entry(header.state_id).or_insert_with(|| {
            FragmentAccumulator::new(header.state_id, MAXIMUM_REASSEMBLED_BYTES)
        });
        let Some(compressed) = assembly.push(fragment).map_err(SessionError::Fragment)? else {
            return Ok(PacketReceipt {
                packet_counter: authenticated.counter,
                sent_timestamp,
                echoed_timestamp,
                state: None,
            });
        };
        self.assemblies.remove(&header.state_id);
        let update = decode_compressed_update(&compressed).map_err(SessionError::Message)?;
        let remote_capabilities = update.capabilities.clone();
        self.process_acknowledgement(update.acknowledged_state);
        let (advances_remote_state, delivered_update, raw_delta) =
            self.accept_remote_update(update)?;
        if !remote_capabilities.is_empty() {
            self.remote_capabilities = remote_capabilities;
        }
        let state = ReceivedState {
            packet_counter: authenticated.counter,
            sent_timestamp: header.sent_timestamp,
            echoed_timestamp: header.echoed_timestamp,
            advances_remote_state,
            raw_delta,
            update: delivered_update,
        };
        Ok(PacketReceipt {
            packet_counter: authenticated.counter,
            sent_timestamp,
            echoed_timestamp,
            state: Some(state),
        })
    }

    #[must_use]
    pub fn latest_remote_state(&self) -> u64 {
        self.latest_remote_state
    }

    /// Returns the state number that will contain newly queued input.
    #[must_use]
    pub fn next_local_state(&self) -> u64 {
        self.next_local_state
    }

    /// Returns the newest local state represented by pending transport history.
    #[must_use]
    pub fn latest_local_state(&self) -> u64 {
        self.sent_states.back().map_or(0, |state| state.state_id)
    }

    /// Returns content-free SSP state numbers for host transport diagnostics.
    #[must_use]
    pub fn transport_state_numbers(&self) -> TransportStateNumbers {
        TransportStateNumbers {
            acked_by_remote: self.sent_states.front().map_or(0, |state| state.state_id),
            sent_num: self.latest_local_state(),
            last_recv_old_num: self.last_remote_base_state,
            last_recv_new_num: self.last_remote_target_state,
            throwaway_num: self.remote_throwaway_floor,
        }
    }

    /// Updates the blank remote baseline before the first server state arrives.
    pub fn set_initial_remote_viewport(&mut self, columns: u16, rows: u16) {
        if self.latest_remote_state != 0 || self.received_states.len() != 1 {
            return;
        }
        let Ok(initial_state) = RemoteTerminalState::new(columns.max(1), rows.max(1)) else {
            return;
        };
        self.received_states.insert(0, initial_state.clone());
        self.delivered_remote_state = initial_state;
    }

    #[must_use]
    pub fn has_pending_update(&self) -> bool {
        self.sent_states.len() > 1
    }

    #[must_use]
    pub fn local_shutdown_sent(&self) -> bool {
        self.sent_states
            .back()
            .is_some_and(|state| state.state_id == u64::MAX)
    }

    #[must_use]
    pub fn local_shutdown_acknowledged(&self) -> bool {
        self.sent_states
            .front()
            .is_some_and(|state| state.state_id == u64::MAX)
    }

    #[must_use]
    pub const fn remote_shutdown_received(&self) -> bool {
        self.latest_remote_state == u64::MAX
    }

    fn process_acknowledgement(&mut self, acknowledged_state: u64) {
        let Some(acknowledged_index) = self
            .sent_states
            .iter()
            .position(|state| state.state_id == acknowledged_state)
        else {
            return;
        };
        self.sent_states.drain(..acknowledged_index);

        let confirmed_instruction_end = self
            .sent_states
            .front()
            .expect("an exact acknowledgement leaves its state in history")
            .instruction_end;
        if confirmed_instruction_end == 0 {
            return;
        }
        self.local_instructions.drain(..confirmed_instruction_end);
        for state in &mut self.sent_states {
            state.instruction_end -= confirmed_instruction_end;
        }
    }

    fn limit_sent_states(&mut self) {
        while self.sent_states.len() > MAXIMUM_SENT_STATES {
            // Match Mosh's bounded-history policy by removing a middle state
            // while preserving the confirmed front and the newest states.
            let removal_index = self.sent_states.len() - 16;
            self.sent_states.remove(removal_index);
        }
    }

    fn accept_remote_update(
        &mut self,
        update: StateUpdate,
    ) -> Result<(bool, StateUpdate, Vec<u8>), SessionError> {
        let raw_delta = update.delta.clone();
        if self.received_states.contains_key(&update.target_state) {
            return Ok((false, update, raw_delta));
        }
        if update.discard_before > update.base_state || update.target_state <= update.base_state {
            return Err(SessionError::InvalidStateTransition);
        }
        let Some(mut target_state) = self.received_states.get(&update.base_state).cloned() else {
            return Ok((false, update, raw_delta));
        };
        let instructions = update
            .decode_instructions()
            .map_err(SessionError::Message)?;
        target_state
            .apply(&instructions)
            .map_err(SessionError::State)?;

        let throwaway_floor = self.remote_throwaway_floor.max(update.discard_before);
        self.received_states
            .retain(|state_id, _| *state_id >= throwaway_floor);
        self.remote_throwaway_floor = throwaway_floor;
        if self.received_states.len() >= MAXIMUM_RECEIVED_STATES {
            return Err(SessionError::TooManyRemoteStates);
        }
        self.received_states
            .insert(update.target_state, target_state.clone());
        self.last_remote_base_state = update.base_state;
        self.last_remote_target_state = update.target_state;

        if update.target_state <= self.latest_remote_state {
            return Ok((false, update, raw_delta));
        }
        let Ok(diff) = target_state.diff_from(&self.delivered_remote_state) else {
            return Ok((false, update, raw_delta));
        };
        let mut delivered_update = update;
        delivered_update.delta = diff.into_instructions().encode_bytes();
        self.latest_remote_state = delivered_update.target_state;
        self.delivered_remote_state = target_state;
        Ok((true, delivered_update, raw_delta))
    }
}

struct SentState {
    state_id: u64,
    instruction_end: usize,
}

/// Content-free SSP state numbers observed by the client protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportStateNumbers {
    pub acked_by_remote: u64,
    pub sent_num: u64,
    pub last_recv_old_num: u64,
    pub last_recv_new_num: u64,
    pub throwaway_num: u64,
}

#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    seen: u128,
}

impl ReplayWindow {
    fn accept(&mut self, counter: u64) -> bool {
        let Some(highest) = self.highest else {
            self.highest = Some(counter);
            self.seen = 1;
            return true;
        };
        if counter > highest {
            let advance = counter - highest;
            self.seen = if advance >= REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << advance) | 1
            };
            self.highest = Some(counter);
            return true;
        }

        let age = highest - counter;
        if age >= REPLAY_WINDOW_BITS {
            return false;
        }
        let bit = 1_u128 << age;
        if self.seen & bit != 0 {
            return false;
        }
        self.seen |= bit;
        true
    }
}

fn seal_fragments(
    channel: &mut SecureChannel,
    compressed: &[u8],
    sent_timestamp: u16,
    latest_peer_timestamp: Option<u16>,
    state_id: u64,
) -> Result<Vec<Vec<u8>>, SessionError> {
    let fragments = Fragment::split(
        compressed,
        sent_timestamp,
        latest_peer_timestamp.unwrap_or(NO_ECHO_TIMESTAMP),
        state_id,
    )
    .map_err(SessionError::Fragment)?;
    fragments
        .into_iter()
        .map(|fragment| {
            channel
                .seal_next(&fragment.encode())
                .map_err(SessionError::Seal)
        })
        .collect()
}

/// Metadata observed from every authenticated server packet.
#[derive(PartialEq)]
pub struct PacketReceipt {
    pub packet_counter: u64,
    pub sent_timestamp: u16,
    pub echoed_timestamp: u16,
    pub state: Option<ReceivedState>,
}

impl std::fmt::Debug for PacketReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PacketReceipt")
            .field("packet_counter", &self.packet_counter)
            .field("sent_timestamp", &self.sent_timestamp)
            .field("echoed_timestamp", &self.echoed_timestamp)
            .field("has_state", &self.state.is_some())
            .finish()
    }
}

#[derive(PartialEq)]
pub struct ReceivedState {
    pub packet_counter: u64,
    pub sent_timestamp: u16,
    pub echoed_timestamp: u16,
    pub advances_remote_state: bool,
    /// Original encoded instruction bytes from the accepted peer update.
    pub raw_delta: Vec<u8>,
    pub update: StateUpdate,
}

impl std::fmt::Debug for ReceivedState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReceivedState")
            .field("packet_counter", &self.packet_counter)
            .field("sent_timestamp", &self.sent_timestamp)
            .field("echoed_timestamp", &self.echoed_timestamp)
            .field("advances_remote_state", &self.advances_remote_state)
            .field("base_state", &self.update.base_state)
            .field("target_state", &self.update.target_state)
            .field("acknowledged_state", &self.update.acknowledged_state)
            .field("discard_before", &self.update.discard_before)
            .field("raw_delta_bytes", &self.raw_delta.len())
            .field("delta_bytes", &self.update.delta.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum SessionError {
    StateExhausted,
    ShutdownAlreadySent,
    ShutdownNotAcknowledged,
    StateHistoryUnavailable,
    UpdateAwaitingAcknowledgement,
    NoPendingUpdate,
    ReplayDetected,
    TooManyActiveAssemblies,
    TooManyRemoteStates,
    InvalidStateTransition,
    State(StateError),
    Seal(SealError),
    Open(OpenError),
    Fragment(FragmentError),
    Message(MessageError),
}
