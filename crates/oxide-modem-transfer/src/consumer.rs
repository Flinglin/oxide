use crate::detector::{DetectedModemProtocol, detect_modem_start};
use crate::stream::{ModemTransfer, ModemWakeCallback};
use crate::zmodem::ZFrameType;
use crate::zmodem_transfer::parse_zmodem_header_prefix;
use std::{
    borrow::Cow,
    fmt,
    time::{Duration, Instant},
};

const PLAIN_HISTORY_LIMIT: usize = 512;
const MODEM_PREFIX_IDLE_TIMEOUT: Duration = Duration::from_millis(50);
const XYMODEM_CANCEL_BYTES: &[u8] = &[crate::xymodem::CAN; 8];
const ZMODEM_CANCEL_BYTES: &[u8] = &[
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    crate::zmodem::ZDLE,
    0x08,
    0x08,
    0x08,
    0x08,
    0x08,
    0x08,
    0x08,
    0x08,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModemTransferDirection {
    Upload,
    Download,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModemTransferRequest {
    pub protocol: DetectedModemProtocol,
    pub direction: ModemTransferDirection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModemConsumerEvent {
    WriteTerminal(Vec<u8>),
    SendServer(Vec<u8>),
    TransferStarted(ModemTransferRequest),
    TransferDataQueued,
    TransferCancelRequested,
}

pub struct ModemConsumer {
    plain_tail: Vec<u8>,
    detection_tail: Vec<u8>,
    plain_tail_deadline: Option<Instant>,
    plain_history: Vec<u8>,
    plain_history_starts_mid_line: bool,
    detection_scope: ModemDetectionScope,
    pending: Option<PendingTransfer>,
    transfer: Option<ModemTransfer>,
    transfer_input: Option<ModemTransfer>,
    wake_host: Option<ModemWakeCallback>,
}

impl fmt::Debug for ModemConsumer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModemConsumer")
            .field("plain_tail_len", &self.plain_tail.len())
            .field("detection_tail_len", &self.detection_tail.len())
            .field("plain_history_len", &self.plain_history.len())
            .field("pending", &self.pending)
            .field("has_transfer", &self.transfer.is_some())
            .field("has_transfer_input", &self.transfer_input.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct PendingTransfer {
    protocol: DetectedModemProtocol,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalControlStringKind {
    Osc,
    Dcs,
    Apc,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ModemDetectionScope {
    #[default]
    Ground,
    Escape,
    ControlString {
        kind: TerminalControlStringKind,
        escape_pending: bool,
    },
}

impl ModemDetectionScope {
    fn mask_control_strings<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        // Spaces preserve text-token boundaries without allowing binary image
        // payload bytes to participate in modem marker or command detection.
        const MASKED_BYTE: u8 = b' ';

        if matches!(self, Self::Ground)
            && !bytes
                .iter()
                .any(|byte| matches!(*byte, 0x1b | 0x90 | 0x9d | 0x9f))
        {
            return Cow::Borrowed(bytes);
        }

        Cow::Owned(
            bytes
                .iter()
                .map(|byte| {
                    let byte = *byte;
                    match *self {
                        Self::Ground => match byte {
                            0x1b => {
                                *self = Self::Escape;
                                MASKED_BYTE
                            }
                            0x90 => {
                                *self = Self::control_string(TerminalControlStringKind::Dcs);
                                MASKED_BYTE
                            }
                            0x9d => {
                                *self = Self::control_string(TerminalControlStringKind::Osc);
                                MASKED_BYTE
                            }
                            0x9f => {
                                *self = Self::control_string(TerminalControlStringKind::Apc);
                                MASKED_BYTE
                            }
                            _ => byte,
                        },
                        Self::Escape => match byte {
                            b']' => {
                                *self = Self::control_string(TerminalControlStringKind::Osc);
                                MASKED_BYTE
                            }
                            b'P' => {
                                *self = Self::control_string(TerminalControlStringKind::Dcs);
                                MASKED_BYTE
                            }
                            b'_' => {
                                *self = Self::control_string(TerminalControlStringKind::Apc);
                                MASKED_BYTE
                            }
                            0x1b => MASKED_BYTE,
                            _ => {
                                *self = Self::Ground;
                                byte
                            }
                        },
                        Self::ControlString {
                            kind,
                            escape_pending,
                        } => {
                            let terminated_by_bel =
                                kind == TerminalControlStringKind::Osc && byte == 0x07;
                            if terminated_by_bel
                                || byte == 0x9c
                                || (escape_pending && byte == b'\\')
                            {
                                *self = Self::Ground;
                            } else {
                                *self = Self::ControlString {
                                    kind,
                                    escape_pending: byte == 0x1b,
                                };
                            }
                            MASKED_BYTE
                        }
                    }
                })
                .collect(),
        )
    }

    const fn control_string(kind: TerminalControlStringKind) -> Self {
        Self::ControlString {
            kind,
            escape_pending: false,
        }
    }

    fn reset(&mut self) {
        *self = Self::Ground;
    }
}

impl Default for ModemConsumer {
    fn default() -> Self {
        Self::new()
    }
}

impl ModemConsumer {
    pub fn new() -> Self {
        Self {
            plain_tail: Vec::new(),
            detection_tail: Vec::new(),
            plain_tail_deadline: None,
            plain_history: Vec::new(),
            plain_history_starts_mid_line: false,
            detection_scope: ModemDetectionScope::default(),
            pending: None,
            transfer: None,
            transfer_input: None,
            wake_host: None,
        }
    }

    pub fn with_wake(wake_host: ModemWakeCallback) -> Self {
        Self {
            wake_host: Some(wake_host),
            ..Self::new()
        }
    }

    pub fn active_transfer(&self) -> Option<&ModemTransfer> {
        self.transfer.as_ref()
    }

    pub fn take_active_transfer(&mut self) -> Option<ModemTransfer> {
        self.transfer.take()
    }

    pub fn start_manual_transfer(
        &mut self,
        request: ModemTransferRequest,
    ) -> Option<ModemTransfer> {
        if self.transfer.is_some() {
            return None;
        }
        Some(self.start_transfer(&[], request))
    }

    pub fn finish_transfer(&mut self) -> Vec<u8> {
        // A successful protocol may share its final transport packet with the
        // next shell prompt. Return those unconsumed bytes to the terminal.
        let trailing_output = self
            .transfer_input
            .as_ref()
            .map(ModemTransfer::drain_remote_output)
            .unwrap_or_default();
        self.transfer = None;
        self.transfer_input = None;
        self.pending = None;
        self.plain_tail.clear();
        self.detection_tail.clear();
        self.plain_tail_deadline = None;
        self.plain_history.clear();
        self.plain_history_starts_mid_line = false;
        self.detection_scope.reset();
        trailing_output
    }

    pub fn interrupt_transfer(&mut self) {
        if let Some(transfer) = &self.transfer {
            transfer.stop();
        }
        if let Some(input) = &self.transfer_input {
            input.stop();
        }
        // Keep the stopped transfer attached until the transport drains its
        // protocol cancellation bytes. The completion path calls finish.
    }

    pub fn active_transfer_input(&self) -> Option<ModemTransfer> {
        self.transfer_input.clone()
    }

    pub fn process_server_output(&mut self, bytes: &[u8]) -> Vec<ModemConsumerEvent> {
        self.process_server_output_at(bytes, Instant::now())
    }

    fn process_server_output_at(&mut self, bytes: &[u8], now: Instant) -> Vec<ModemConsumerEvent> {
        let mut events = self.flush_expired_plain_output_at(now);
        events.extend(self.process_server_output_inner(bytes, now));
        events
    }

    fn process_server_output_inner(
        &mut self,
        bytes: &[u8],
        now: Instant,
    ) -> Vec<ModemConsumerEvent> {
        if bytes.is_empty() {
            return Vec::new();
        }

        if let Some(input) = &self.transfer_input {
            input.push_remote_output(bytes);
            return vec![ModemConsumerEvent::TransferDataQueued];
        }

        if self.pending.is_some() {
            return self.process_pending_server_output(bytes);
        }

        let unbuffered = self.plain_tail.is_empty() && self.detection_tail.is_empty();
        // Ordinary output needs one scan for both control strings and modem
        // markers. Buffered prefixes still require the stateful detector.
        let ordinary_output = unbuffered
            && matches!(self.detection_scope, ModemDetectionScope::Ground)
            && !bytes.iter().any(|byte| {
                matches!(
                    *byte,
                    0x1b | 0x90
                        | 0x9d
                        | 0x9f
                        | crate::zmodem::ZPAD
                        | crate::xymodem::WANT_CRC
                        | crate::xymodem::NAK
                        | crate::xymodem::SOH
                        | crate::xymodem::STX
                )
            });
        let masked_bytes = if ordinary_output {
            Cow::Borrowed(bytes)
        } else {
            self.detection_scope.mask_control_strings(bytes)
        };
        if unbuffered && (ordinary_output || !contains_modem_candidate(masked_bytes.as_ref())) {
            self.remember_plain_output(masked_bytes.as_ref());
            let mut events = vec![ModemConsumerEvent::WriteTerminal(bytes.to_vec())];
            if let Some(request) = self.xymodem_download_request() {
                self.start_transfer(&[], request.clone());
                events.push(ModemConsumerEvent::TransferStarted(request));
            }
            return events;
        }

        let scan_bytes = if self.plain_tail.is_empty() {
            Cow::Borrowed(bytes)
        } else {
            let mut combined = Vec::with_capacity(self.plain_tail.len() + bytes.len());
            combined.extend_from_slice(&self.plain_tail);
            combined.extend_from_slice(bytes);
            self.plain_tail.clear();
            self.plain_tail_deadline = None;
            Cow::Owned(combined)
        };
        let detection_bytes = if self.detection_tail.is_empty() {
            Cow::Borrowed(masked_bytes.as_ref())
        } else {
            let mut combined = Vec::with_capacity(self.detection_tail.len() + masked_bytes.len());
            combined.extend_from_slice(&self.detection_tail);
            combined.extend_from_slice(masked_bytes.as_ref());
            self.detection_tail.clear();
            Cow::Owned(combined)
        };

        let Some(start) = detect_modem_start(detection_bytes.as_ref()) else {
            let hold = possible_modem_prefix_suffix_len(detection_bytes.as_ref());
            if hold == scan_bytes.len() {
                self.hold_plain_tail(scan_bytes.as_ref(), detection_bytes.as_ref(), now);
                return Vec::new();
            }
            let split = scan_bytes.len() - hold;
            self.hold_plain_tail(&scan_bytes[split..], &detection_bytes[split..], now);
            self.remember_plain_output(&detection_bytes[..split]);
            let mut events = vec![ModemConsumerEvent::WriteTerminal(
                scan_bytes[..split].to_vec(),
            )];
            if let Some(request) = self.xymodem_download_request() {
                self.start_transfer(&[], request.clone());
                events.push(ModemConsumerEvent::TransferStarted(request));
            }
            return events;
        };

        let mut events = Vec::new();
        if start.offset > 0 {
            self.remember_plain_output(&detection_bytes[..start.offset]);
            events.push(ModemConsumerEvent::WriteTerminal(
                scan_bytes[..start.offset].to_vec(),
            ));
        }

        let protocol_bytes = &scan_bytes[start.offset..];
        if let Some(request) =
            request_for_protocol(start.protocol, protocol_bytes, self.command_history())
        {
            self.start_transfer(protocol_bytes, request.clone());
            events.push(ModemConsumerEvent::TransferStarted(request));
        } else if should_wait_for_protocol_confirmation(start.protocol, protocol_bytes) {
            self.pending = Some(PendingTransfer {
                protocol: start.protocol,
                bytes: protocol_bytes.to_vec(),
            });
        } else if let Some(control_len) = zmodem_control_prefix_len(start.protocol, protocol_bytes)
        {
            // A retried completion/control header belongs to the previous peer
            // state and must not become terminal text or a new file prompt.
            let remaining = &protocol_bytes[control_len..];
            if !remaining.is_empty() {
                self.remember_plain_output(remaining);
                events.push(ModemConsumerEvent::WriteTerminal(remaining.to_vec()));
            }
        } else {
            self.remember_plain_output(protocol_bytes);
            events.push(ModemConsumerEvent::WriteTerminal(protocol_bytes.to_vec()));
        }

        events
    }

    pub fn pending_plain_output_delay(&self) -> Option<Duration> {
        self.plain_tail_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub fn flush_expired_plain_output(&mut self) -> Vec<ModemConsumerEvent> {
        self.flush_expired_plain_output_at(Instant::now())
    }

    pub fn flush_pending_plain_output(&mut self) -> Vec<ModemConsumerEvent> {
        if self.plain_tail.is_empty() {
            self.plain_tail_deadline = None;
            return Vec::new();
        }

        let plain = std::mem::take(&mut self.plain_tail);
        let detection = std::mem::take(&mut self.detection_tail);
        self.plain_tail_deadline = None;
        self.remember_plain_output(&detection);
        vec![ModemConsumerEvent::WriteTerminal(plain)]
    }

    fn flush_expired_plain_output_at(&mut self, now: Instant) -> Vec<ModemConsumerEvent> {
        if self
            .plain_tail_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return Vec::new();
        }
        self.flush_pending_plain_output()
    }

    fn hold_plain_tail(&mut self, plain: &[u8], detection: &[u8], now: Instant) {
        if plain.is_empty() {
            self.plain_tail_deadline = None;
            return;
        }
        self.plain_tail.extend_from_slice(plain);
        self.detection_tail.extend_from_slice(detection);
        // A short idle window joins transport fragments without hiding ordinary
        // text indefinitely when it happens to share a modem header prefix.
        self.plain_tail_deadline = Some(now + MODEM_PREFIX_IDLE_TIMEOUT);
    }

    fn process_pending_server_output(&mut self, bytes: &[u8]) -> Vec<ModemConsumerEvent> {
        let mut pending = self.pending.take().expect("pending transfer");
        pending.bytes.extend_from_slice(bytes);
        if let Some(request) =
            request_for_protocol(pending.protocol, &pending.bytes, self.command_history())
        {
            let initial = pending.bytes;
            self.start_transfer(&initial, request.clone());
            vec![ModemConsumerEvent::TransferStarted(request)]
        } else if should_wait_for_protocol_confirmation(pending.protocol, &pending.bytes) {
            self.pending = Some(pending);
            Vec::new()
        } else if let Some(control_len) =
            zmodem_control_prefix_len(pending.protocol, &pending.bytes)
        {
            let remaining = pending.bytes[control_len..].to_vec();
            self.remember_plain_output(&remaining);
            (!remaining.is_empty())
                .then_some(ModemConsumerEvent::WriteTerminal(remaining))
                .into_iter()
                .collect()
        } else {
            let plain = pending.bytes;
            self.remember_plain_output(&plain);
            vec![ModemConsumerEvent::WriteTerminal(plain)]
        }
    }

    fn start_transfer(
        &mut self,
        initial_bytes: &[u8],
        request: ModemTransferRequest,
    ) -> ModemTransfer {
        let cancellation_bytes = match request.protocol {
            DetectedModemProtocol::Zmodem => ZMODEM_CANCEL_BYTES,
            DetectedModemProtocol::Xmodem
            | DetectedModemProtocol::XymodemNegotiation
            | DetectedModemProtocol::Ymodem => XYMODEM_CANCEL_BYTES,
        };
        let transfer = ModemTransfer::new_with_wake_and_cancel(
            initial_bytes,
            self.wake_host.clone(),
            cancellation_bytes,
        );
        self.transfer_input = Some(transfer.clone());
        self.transfer = Some(transfer.clone());
        self.plain_tail_deadline = None;
        self.plain_history.clear();
        self.plain_history_starts_mid_line = false;
        transfer
    }

    fn remember_plain_output(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= PLAIN_HISTORY_LIMIT {
            let discard = bytes.len() - PLAIN_HISTORY_LIMIT;
            let previous_byte = if discard == 0 {
                self.plain_history.last().copied()
            } else {
                Some(bytes[discard - 1])
            };
            if let Some(previous_byte) = previous_byte {
                self.plain_history_starts_mid_line = !matches!(previous_byte, b'\r' | b'\n');
            }
            self.plain_history.clear();
            self.plain_history
                .extend_from_slice(&bytes[bytes.len() - PLAIN_HISTORY_LIMIT..]);
            return;
        }

        let retained_bytes = PLAIN_HISTORY_LIMIT - bytes.len();
        if self.plain_history.len() > retained_bytes {
            let discard = self.plain_history.len() - retained_bytes;
            self.plain_history_starts_mid_line =
                !matches!(self.plain_history[discard - 1], b'\r' | b'\n');
            self.plain_history.drain(..discard);
        }
        self.plain_history.extend_from_slice(bytes);
    }

    fn xymodem_download_request(&self) -> Option<ModemTransferRequest> {
        // PTYs echo characters while the user is still typing. Wait for the
        // command line terminator so entering "sx" cannot open a prompt early.
        if !self.plain_history.ends_with(b"\r") && !self.plain_history.ends_with(b"\n") {
            return None;
        }
        let text = String::from_utf8_lossy(self.command_history());
        let (protocol, direction) = xymodem_command_hint(text.lines().next_back()?)?;
        (direction == ModemTransferDirection::Download).then_some(ModemTransferRequest {
            protocol,
            direction,
        })
    }

    fn command_history(&self) -> &[u8] {
        // Trimming can leave a word suffix such as "ry" from "history".
        // Only a real line boundary may make the retained suffix a command.
        if self.plain_history_starts_mid_line {
            self.plain_history
                .iter()
                .position(|byte| matches!(*byte, b'\r' | b'\n'))
                .map_or(&[], |end| &self.plain_history[end + 1..])
        } else {
            &self.plain_history
        }
    }
}

fn request_for_protocol(
    protocol: DetectedModemProtocol,
    bytes: &[u8],
    plain_history: &[u8],
) -> Option<ModemTransferRequest> {
    let (protocol, direction) = match protocol {
        DetectedModemProtocol::Zmodem => match parse_zmodem_header_prefix(bytes).ok().flatten()? {
            header if header.frame_type == ZFrameType::ZrInit => {
                (protocol, ModemTransferDirection::Upload)
            }
            header
                if matches!(
                    header.frame_type,
                    ZFrameType::ZrqInit | ZFrameType::ZFile | ZFrameType::ZData
                ) =>
            {
                (protocol, ModemTransferDirection::Download)
            }
            _ => return None,
        },
        DetectedModemProtocol::XymodemNegotiation => {
            let protocol = xymodem_negotiation_protocol_hint(plain_history)?;
            (protocol, ModemTransferDirection::Upload)
        }
        DetectedModemProtocol::Xmodem | DetectedModemProtocol::Ymodem => {
            (protocol, ModemTransferDirection::Download)
        }
    };
    Some(ModemTransferRequest {
        protocol,
        direction,
    })
}

fn should_wait_for_protocol_confirmation(protocol: DetectedModemProtocol, bytes: &[u8]) -> bool {
    // Only framed ZMODEM headers have enough structure to justify holding output
    // while waiting for more bytes; lone X/YMODEM negotiation bytes are common text.
    matches!(protocol, DetectedModemProtocol::Zmodem)
        && matches!(parse_zmodem_header_prefix(bytes), Ok(None))
}

fn zmodem_control_prefix_len(protocol: DetectedModemProtocol, bytes: &[u8]) -> Option<usize> {
    if protocol != DetectedModemProtocol::Zmodem {
        return None;
    }
    let header = parse_zmodem_header_prefix(bytes).ok().flatten()?;
    if matches!(
        header.frame_type,
        ZFrameType::ZrInit | ZFrameType::ZrqInit | ZFrameType::ZFile | ZFrameType::ZData
    ) {
        return None;
    }
    for include_xon in [false, true] {
        let encoded =
            crate::zmodem::encode_hex_header(header.frame_type, header.position, include_xon);
        if bytes.starts_with(&encoded) {
            return Some(encoded.len());
        }
    }
    None
}

fn xymodem_negotiation_protocol_hint(plain_history: &[u8]) -> Option<DetectedModemProtocol> {
    let text = String::from_utf8_lossy(plain_history);
    text.lines()
        .rev()
        .take(4)
        .find_map(|line| match xymodem_command_hint(line) {
            Some((protocol, ModemTransferDirection::Upload)) => Some(protocol),
            _ => None,
        })
}

fn xymodem_command_hint(line: &str) -> Option<(DetectedModemProtocol, ModemTransferDirection)> {
    for token in line.split_whitespace() {
        let command = token
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(token)
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_');
        let request = match command {
            "rx" | "lrx" => Some((
                DetectedModemProtocol::Xmodem,
                ModemTransferDirection::Upload,
            )),
            "rb" | "ry" | "lrb" | "lry" => Some((
                DetectedModemProtocol::Ymodem,
                ModemTransferDirection::Upload,
            )),
            "sx" | "lsx" => Some((
                DetectedModemProtocol::Xmodem,
                ModemTransferDirection::Download,
            )),
            "sb" | "sy" | "lsb" | "lsy" => Some((
                DetectedModemProtocol::Ymodem,
                ModemTransferDirection::Download,
            )),
            _ => None,
        };
        if request.is_some() {
            return request;
        }
        if !is_shell_prompt_token(token) {
            return None;
        }
    }
    None
}

fn is_shell_prompt_token(token: &str) -> bool {
    token == "PS"
        || token
            .chars()
            .last()
            .is_some_and(|last| matches!(last, '$' | '#' | '>' | '%'))
}

fn possible_modem_prefix_suffix_len(bytes: &[u8]) -> usize {
    const PREFIXES: [&[u8]; 6] = [
        &[crate::zmodem::ZPAD],
        &[crate::zmodem::ZPAD, crate::zmodem::ZPAD],
        &[
            crate::zmodem::ZPAD,
            crate::zmodem::ZPAD,
            crate::zmodem::ZDLE,
        ],
        &[crate::zmodem::ZPAD, crate::zmodem::ZDLE],
        &[crate::xymodem::SOH],
        &[crate::xymodem::STX],
    ];
    PREFIXES
        .iter()
        .filter(|prefix| bytes.ends_with(prefix))
        .map(|prefix| prefix.len())
        .max()
        .unwrap_or(0)
}

fn contains_modem_candidate(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| {
        matches!(
            *byte,
            crate::zmodem::ZPAD
                | crate::xymodem::WANT_CRC
                | crate::xymodem::NAK
                | crate::xymodem::SOH
                | crate::xymodem::STX
        )
    })
}
