//! Authenticated RTP datagrams for the existing offline DAVE/RTP pipeline.
//! Wire format follows Discord's documented *_rtpsize modes and the pinned
//! Songbird crypto implementation: clear RTP header as AAD, encrypted payload,
//! 16-byte tag, then a big-endian 4-byte counter padded with zeros for the nonce.
//! https://docs.discord.com/developers/topics/voice-connections#transport-encryption-modes
//!
//! No sockets or Discord session negotiation. Only fixed 12-byte RTP headers
//! are supported; extensions, CSRCs and padding fail closed. One sender MUST
//! exclusively own the nonce space for its transport key across ALL SSRCs and
//! media. Never initialize this alongside Songbird with Songbird's existing key.
//! Resume must retain this object/counter; replacement requires a fresh key.
//! These primitives do not authorize capture or transmission.
use crate::{media::rtp::VideoRtpPacket, video::VideoAuthority};
use aes_gcm::{
    Aes256Gcm,
    aead::{AeadInPlace, KeyInit},
};
use chacha20poly1305::XChaCha20Poly1305;
use zeroize::Zeroizing;

const HEADER: usize = 12;
const OVERHEAD: usize = 20;
const MAX_DATAGRAM: usize = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportMode {
    Aes256Gcm,
    XChaCha20Poly1305,
}
impl TransportMode {
    pub fn from_negotiated(mode: &str) -> Result<Self, DatagramError> {
        match mode {
            "aead_aes256_gcm_rtpsize" => Ok(Self::Aes256Gcm),
            "aead_xchacha20_poly1305_rtpsize" => Ok(Self::XChaCha20Poly1305),
            _ => Err(DatagramError::Unsupported),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatagramError {
    Unsupported,
    InvalidPacket,
    Authentication,
    Replay,
    NotReady,
    Exhausted,
}

// Deliberately no Clone or Debug on cipher/session owners or media buffers.
enum Cipher {
    Aes(Box<Aes256Gcm>),
    ChaCha(Box<XChaCha20Poly1305>),
}
impl Cipher {
    fn new(mode: TransportMode, key: Zeroizing<[u8; 32]>) -> Self {
        match mode {
            TransportMode::Aes256Gcm => Self::Aes(Box::new(Aes256Gcm::new((&*key).into()))),
            TransportMode::XChaCha20Poly1305 => {
                Self::ChaCha(Box::new(XChaCha20Poly1305::new((&*key).into())))
            }
        }
    }
    fn seal(&self, counter: u32, header: &[u8], body: &mut Vec<u8>) -> Result<(), DatagramError> {
        match self {
            Self::Aes(cipher) => {
                let mut nonce = aes_gcm::Nonce::default();
                nonce[..4].copy_from_slice(&counter.to_be_bytes());
                cipher.encrypt_in_place(&nonce, header, body)
            }
            Self::ChaCha(cipher) => {
                let mut nonce = chacha20poly1305::XNonce::default();
                nonce[..4].copy_from_slice(&counter.to_be_bytes());
                cipher.encrypt_in_place(&nonce, header, body)
            }
        }
        .map_err(|_| DatagramError::Authentication)
    }
    fn open(&self, counter: u32, header: &[u8], body: &mut Vec<u8>) -> Result<(), DatagramError> {
        match self {
            Self::Aes(cipher) => {
                let mut nonce = aes_gcm::Nonce::default();
                nonce[..4].copy_from_slice(&counter.to_be_bytes());
                cipher.decrypt_in_place(&nonce, header, body)
            }
            Self::ChaCha(cipher) => {
                let mut nonce = chacha20poly1305::XNonce::default();
                nonce[..4].copy_from_slice(&counter.to_be_bytes());
                cipher.decrypt_in_place(&nonce, header, body)
            }
        }
        .map_err(|_| DatagramError::Authentication)
    }
}
fn valid_header(packet: &[u8]) -> bool {
    packet.len() > HEADER && packet[0] == 0x80
}

/// Exclusive outbound counter owner. No wraparound or automatic reconnect reset.
pub struct RtpSender {
    cipher: Option<Cipher>,
    epoch: u64,
    next: Option<u32>,
}
impl RtpSender {
    /// `initial_counter` comes from the negotiated connection's exclusive nonce
    /// owner. A fresh key may start at zero. Never recreate this for the same key.
    pub fn new(
        mode: TransportMode,
        key: Zeroizing<[u8; 32]>,
        epoch: u64,
        initial_counter: u32,
    ) -> Self {
        Self {
            cipher: Some(Cipher::new(mode, key)),
            epoch,
            next: Some(initial_counter),
        }
    }
    pub fn invalidate(&mut self) {
        self.cipher = None;
        self.next = None;
    }
    pub fn seal_video(
        &mut self,
        packet: &VideoRtpPacket,
        authority: VideoAuthority,
    ) -> Result<Vec<u8>, DatagramError> {
        self.check(authority)?;
        let bytes = packet
            .bytes_for(authority)
            .map_err(|_| DatagramError::NotReady)?;
        self.seal_rtp(bytes, authority)
    }
    fn check(&mut self, authority: VideoAuthority) -> Result<(), DatagramError> {
        if !authority.ready || authority.epoch != self.epoch || self.cipher.is_none() {
            self.invalidate();
            return Err(DatagramError::NotReady);
        }
        Ok(())
    }
    // Only typed DAVE-encrypted video packets reach this in production.
    fn seal_rtp(
        &mut self,
        packet: &[u8],
        authority: VideoAuthority,
    ) -> Result<Vec<u8>, DatagramError> {
        self.check(authority)?;
        if !valid_header(packet) || packet.len() > MAX_DATAGRAM - OVERHEAD {
            return Err(DatagramError::InvalidPacket);
        }
        let counter = self.next.ok_or(DatagramError::Exhausted)?;
        // Burn the nonce even if encryption fails; never reuse it.
        self.next = counter.checked_add(1);
        let mut body = packet[HEADER..].to_vec();
        self.cipher.as_ref().ok_or(DatagramError::NotReady)?.seal(
            counter,
            &packet[..HEADER],
            &mut body,
        )?;
        let mut output = Vec::with_capacity(packet.len() + OVERHEAD);
        output.extend_from_slice(&packet[..HEADER]);
        output.extend(body);
        output.extend(counter.to_be_bytes());
        Ok(output)
    }
}

/// One authenticated SSRC with a bounded 128-counter replay window. Authenticate
/// before committing replay state, so forged counters cannot evict valid packets.
pub struct RtpReceiver {
    cipher: Option<Cipher>,
    epoch: u64,
    ssrc: u32,
    highest: Option<u32>,
    seen: u128,
}
impl RtpReceiver {
    pub fn new(mode: TransportMode, key: Zeroizing<[u8; 32]>, epoch: u64, ssrc: u32) -> Self {
        Self {
            cipher: Some(Cipher::new(mode, key)),
            epoch,
            ssrc,
            highest: None,
            seen: 0,
        }
    }
    pub fn invalidate(&mut self) {
        self.cipher = None;
        self.highest = None;
        self.seen = 0;
    }
    pub fn open(
        &mut self,
        packet: &[u8],
        authority: VideoAuthority,
    ) -> Result<Vec<u8>, DatagramError> {
        if !authority.ready || authority.epoch != self.epoch || self.cipher.is_none() {
            self.invalidate();
            return Err(DatagramError::NotReady);
        }
        if packet.len() <= HEADER + OVERHEAD
            || packet.len() > MAX_DATAGRAM
            || !valid_header(packet)
            || u32::from_be_bytes(packet[8..12].try_into().unwrap()) != self.ssrc
        {
            return Err(DatagramError::InvalidPacket);
        }
        let end = packet.len() - 4;
        let counter = u32::from_be_bytes(packet[end..].try_into().unwrap());
        if let Some(highest) = self.highest
            && counter <= highest
        {
            let distance = highest - counter;
            if distance >= 128 || self.seen & (1_u128 << distance) != 0 {
                return Err(DatagramError::Replay);
            }
        }
        let mut body = packet[HEADER..end].to_vec();
        self.cipher.as_ref().ok_or(DatagramError::NotReady)?.open(
            counter,
            &packet[..HEADER],
            &mut body,
        )?;
        match self.highest {
            Some(highest) if counter > highest => {
                let shift = counter - highest;
                self.seen = if shift >= 128 {
                    1
                } else {
                    (self.seen << shift) | 1
                };
                self.highest = Some(counter);
            }
            Some(highest) => self.seen |= 1_u128 << (highest - counter),
            None => {
                self.highest = Some(counter);
                self.seen = 1;
            }
        }
        let mut output = Vec::with_capacity(HEADER + body.len());
        output.extend_from_slice(&packet[..HEADER]);
        output.extend(body);
        Ok(output)
    }
}

#[cfg(test)]
mod tests;
