// SPDX-License-Identifier: Apache-2.0

//! Authenticated datagram envelopes derived from RFC 7253 and clean-room tests.

use aes::Aes128;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ocb3::Ocb3;
use ocb3::aead::{Aead, KeyInit};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

const SESSION_KEY_BYTES: usize = 16;
const BASE64_PADDING_QUANTUM_BYTES: usize = 4;
const PACKET_PREFIX_BYTES: usize = 8;
const OCB_NONCE_BYTES: usize = 12;
const OCB_TAG_BYTES: usize = 16;
const DIRECTION_BIT: u64 = 1_u64 << 63;
const MAX_COUNTER: u64 = DIRECTION_BIT - 1;
const MINIMUM_PACKET_BYTES: usize = PACKET_PREFIX_BYTES + OCB_TAG_BYTES;

type DatagramCipher = Ocb3<Aes128>;

/// Owns a decoded session key without exposing it through formatting or cloning.
pub struct SessionKey(Zeroizing<[u8; SESSION_KEY_BYTES]>);

impl SessionKey {
    /// Decodes the Base64 key supplied by the authenticated bootstrap.
    ///
    /// # Errors
    ///
    /// Returns an error when the text is not Base64 or does not decode
    /// to the required 16-byte AES key.
    pub fn decode(value: &str) -> Result<Self, KeyError> {
        let mut padded_value = Zeroizing::new(value.to_owned());
        while !padded_value
            .len()
            .is_multiple_of(BASE64_PADDING_QUANTUM_BYTES)
        {
            padded_value.push('=');
        }
        let mut decoded = Zeroizing::new(
            STANDARD
                .decode(padded_value.as_bytes())
                .map_err(|_| KeyError::InvalidBase64)?,
        );
        if decoded.len() != SESSION_KEY_BYTES {
            return Err(KeyError::InvalidLength);
        }

        let mut bytes = Zeroizing::new([0_u8; SESSION_KEY_BYTES]);
        bytes.copy_from_slice(&decoded);
        decoded.zeroize();
        Ok(Self(bytes))
    }

    fn expose(&self) -> &[u8; SESSION_KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionKey([REDACTED])")
    }
}

/// Identifies whether this process behaves as a protocol client or server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerRole {
    Client,
    Server,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketDirection {
    ClientToServer,
    ServerToClient,
}

impl PacketDirection {
    fn for_outbound(role: PeerRole) -> Self {
        match role {
            PeerRole::Client => Self::ClientToServer,
            PeerRole::Server => Self::ServerToClient,
        }
    }

    fn for_inbound(role: PeerRole) -> Self {
        match role {
            PeerRole::Client => Self::ServerToClient,
            PeerRole::Server => Self::ClientToServer,
        }
    }

    fn encode(self, counter: u64) -> u64 {
        match self {
            Self::ClientToServer => counter,
            Self::ServerToClient => counter | DIRECTION_BIT,
        }
    }

    fn decode(value: u64) -> (Self, u64) {
        let direction = if value & DIRECTION_BIT == 0 {
            Self::ClientToServer
        } else {
            Self::ServerToClient
        };
        (direction, value & MAX_COUNTER)
    }
}

/// Couples one outbound nonce sequence with an authenticated inbound opener.
pub struct SecureChannel {
    outbound_cipher: DatagramCipher,
    inbound_cipher: DatagramCipher,
    outbound_direction: PacketDirection,
    inbound_direction: PacketDirection,
    next_counter: Option<u64>,
}

impl SecureChannel {
    /// Consumes the key so only expanded cipher state remains in the channel.
    #[must_use]
    pub fn new(role: PeerRole, key: SessionKey) -> Self {
        let outbound_cipher = DatagramCipher::new(key.expose().into());
        let inbound_cipher = DatagramCipher::new(key.expose().into());
        drop(key);
        Self {
            outbound_cipher,
            inbound_cipher,
            outbound_direction: PacketDirection::for_outbound(role),
            inbound_direction: PacketDirection::for_inbound(role),
            next_counter: Some(0),
        }
    }

    /// Encrypts once with the next counter and advances it after success.
    ///
    /// # Errors
    ///
    /// Returns an error if the 63-bit counter space is exhausted or the
    /// authenticated-encryption backend rejects the operation.
    pub fn seal_next(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
        let counter = self.next_counter.ok_or(SealError::CounterExhausted)?;
        let encoded_number = self.outbound_direction.encode(counter);
        let prefix = encoded_number.to_be_bytes();
        let nonce = make_nonce(prefix);
        let sealed = self
            .outbound_cipher
            .encrypt(&nonce.into(), plaintext)
            .map_err(|_| SealError::EncryptionFailed)?;

        self.next_counter = counter.checked_add(1).filter(|next| *next <= MAX_COUNTER);
        let mut packet = Vec::with_capacity(PACKET_PREFIX_BYTES + sealed.len());
        packet.extend_from_slice(&prefix);
        packet.extend_from_slice(&sealed);
        Ok(packet)
    }

    /// Authenticates a packet before returning any plaintext to the caller.
    ///
    /// # Errors
    ///
    /// Returns an error for undersized packets, failed authentication, or an
    /// authenticated packet sent in the opposite protocol direction.
    pub fn open(&self, packet: &[u8]) -> Result<AuthenticatedPacket, OpenError> {
        if packet.len() < MINIMUM_PACKET_BYTES {
            return Err(OpenError::PacketTooShort);
        }

        let prefix: [u8; PACKET_PREFIX_BYTES] = packet[..PACKET_PREFIX_BYTES]
            .try_into()
            .map_err(|_| OpenError::PacketTooShort)?;
        let encoded_number = u64::from_be_bytes(prefix);
        let (direction, counter) = PacketDirection::decode(encoded_number);
        let nonce = make_nonce(prefix);
        let mut plaintext = self
            .inbound_cipher
            .decrypt(&nonce.into(), &packet[PACKET_PREFIX_BYTES..])
            .map_err(|_| OpenError::AuthenticationFailed)?;

        if direction != self.inbound_direction {
            plaintext.zeroize();
            return Err(OpenError::UnexpectedDirection);
        }

        Ok(AuthenticatedPacket { counter, plaintext })
    }
}

/// Plaintext is constructible only after successful authentication.
#[derive(Eq, PartialEq)]
pub struct AuthenticatedPacket {
    pub counter: u64,
    pub plaintext: Vec<u8>,
}

impl fmt::Debug for AuthenticatedPacket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedPacket")
            .field("counter", &self.counter)
            .field("plaintext_bytes", &self.plaintext.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    InvalidBase64,
    InvalidLength,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SealError {
    CounterExhausted,
    EncryptionFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenError {
    PacketTooShort,
    AuthenticationFailed,
    UnexpectedDirection,
}

fn make_nonce(prefix: [u8; PACKET_PREFIX_BYTES]) -> [u8; OCB_NONCE_BYTES] {
    let mut nonce = [0_u8; OCB_NONCE_BYTES];
    nonce[OCB_NONCE_BYTES - PACKET_PREFIX_BYTES..].copy_from_slice(&prefix);
    nonce
}
