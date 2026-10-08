//! Offline RFC 6184 non-interleaved H264 packetization, single NAL and FU-A only.
//! References: https://www.rfc-editor.org/rfc/rfc6184#section-5.6 and #section-5.8;
//! https://daveprotocol.com/#h264--h265. Original implementation, no copied code.
//!
//! DAVE encrypts the complete encoded access unit before this layer. Its codec
//! transform canonicalizes start codes and avoids start codes in ciphertext.
//! Supplemental authentication/nonce/range bytes stay on the final NAL's tail;
//! they are not a separate NAL and must never be stripped by depacketization.
//! This narrow, ordered offline subset is NOT a negotiated Discord transport:
//! no RTP extensions, retransmission, jitter buffer, STAP-A, transport AEAD,
//! SSRC/PT selection, sockets, or gateway integration. Callers supply metadata.
use crate::video::{EncryptedVideoFrame, MAX_ENCODED_FRAME_BYTES, VideoAuthority, VideoCodec};

const HEADER_BYTES: usize = 12;
const MAX_FRAME: usize = MAX_ENCODED_FRAME_BYTES + 4096;
const MAX_PACKETS: usize = 4096;
const START: [u8; 4] = [0, 0, 0, 1];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtpError {
    NotReady,
    Unsupported,
    InvalidPacket,
    InvalidFrame,
    Limit,
    Discontinuity,
}

#[derive(Clone, Copy)]
pub struct RtpMetadata {
    pub ssrc: u32,
    pub payload_type: u8,
    /// H264 sampling clock, 90 kHz; same value for every packet in an access unit.
    pub timestamp: u32,
    pub first_sequence: u16,
}

/// Contains ciphertext only. No Debug implementation to prevent media logging.
pub struct VideoRtpPacket {
    bytes: Vec<u8>,
    epoch: u64,
}
impl VideoRtpPacket {
    /// Recheck this immediately before transport encryption/commit while holding
    /// the negotiated session owner's lock. Every fragment carries its epoch.
    pub fn bytes_for(&self, authority: VideoAuthority) -> Result<&[u8], RtpError> {
        if !authority.ready || authority.epoch != self.epoch {
            return Err(RtpError::NotReady);
        }
        Ok(&self.bytes)
    }
}

pub fn packetize_h264(
    frame: &EncryptedVideoFrame,
    authority: VideoAuthority,
    metadata: RtpMetadata,
    max_packet_bytes: usize,
) -> Result<Vec<VideoRtpPacket>, RtpError> {
    if frame.codec() != VideoCodec::H264 {
        return Err(RtpError::Unsupported);
    }
    if metadata.payload_type > 127 || !(64..=1500).contains(&max_packet_bytes) {
        return Err(RtpError::InvalidPacket);
    }
    let frame = frame
        .payload_for(authority)
        .map_err(|_| RtpError::NotReady)?;
    let nals = split_nals(frame)?;
    let capacity = max_packet_bytes - HEADER_BYTES;
    let count: usize = nals
        .iter()
        .map(|nal| {
            if nal.len() <= capacity {
                1
            } else {
                (nal.len() - 1).div_ceil(capacity - 2)
            }
        })
        .sum();
    if count > MAX_PACKETS {
        return Err(RtpError::Limit);
    }
    let mut packets = Vec::with_capacity(count);
    for nal in nals {
        if nal.len() <= capacity {
            packets.push(make_packet(nal, metadata, packets.len(), authority.epoch));
        } else {
            let fragments = (nal.len() - 1).div_ceil(capacity - 2);
            for (index, chunk) in nal[1..].chunks(capacity - 2).enumerate() {
                let mut payload = Vec::with_capacity(chunk.len() + 2);
                payload.push((nal[0] & 0xe0) | 28); // FU-A indicator: F/NRI + type28.
                payload.push(
                    (nal[0] & 0x1f)
                        | if index == 0 { 0x80 } else { 0 }
                        | if index + 1 == fragments { 0x40 } else { 0 },
                );
                payload.extend_from_slice(chunk);
                packets.push(make_packet(
                    &payload,
                    metadata,
                    packets.len(),
                    authority.epoch,
                ));
            }
        }
    }
    packets.last_mut().ok_or(RtpError::InvalidFrame)?.bytes[1] |= 0x80;
    Ok(packets)
}

fn make_packet(payload: &[u8], metadata: RtpMetadata, index: usize, epoch: u64) -> VideoRtpPacket {
    let mut bytes = Vec::with_capacity(HEADER_BYTES + payload.len());
    bytes.extend_from_slice(&[0x80, metadata.payload_type]);
    bytes.extend_from_slice(
        &metadata
            .first_sequence
            .wrapping_add(index as u16)
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&metadata.timestamp.to_be_bytes());
    bytes.extend_from_slice(&metadata.ssrc.to_be_bytes());
    bytes.extend_from_slice(payload);
    VideoRtpPacket { bytes, epoch }
}

fn split_nals(frame: &[u8]) -> Result<Vec<&[u8]>, RtpError> {
    if frame.len() > MAX_FRAME || !frame.starts_with(&START) {
        return Err(RtpError::InvalidFrame);
    }
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset + 3 <= frame.len() {
        if frame[offset..].starts_with(&START) {
            if starts.len() >= MAX_PACKETS {
                return Err(RtpError::Limit);
            }
            starts.push(offset);
            offset += 4;
        } else if frame[offset..].starts_with(&[0, 0, 1]) {
            // Encryption must already have normalized authenticated start codes.
            return Err(RtpError::InvalidFrame);
        } else {
            offset += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(frame.len());
        let nal = &frame[start + 4..end];
        if nal.is_empty() || nal[0] & 0x80 != 0 || !(1..=23).contains(&(nal[0] & 0x1f)) {
            return Err(RtpError::InvalidFrame);
        }
        nals.push(nal);
    }
    Ok(nals)
}

/// Ordered bounded receive fixture. Input must have had transport AEAD verified.
/// The epoch is local authority, never trusted metadata read from RTP. Dropped,
/// reordered, duplicated or interleaved packets discard the whole access unit.
/// A new stream must start with a new receiver supplied by the session owner.
pub struct H264Reassembler {
    ssrc: u32,
    payload_type: u8,
    epoch: u64,
    ready: bool,
    timestamp: Option<u32>,
    next_sequence: Option<u16>,
    fragment_header: Option<u8>,
    bytes: Vec<u8>,
    packet_count: usize,
    last_timestamp: Option<u32>,
}
impl H264Reassembler {
    pub fn new(ssrc: u32, payload_type: u8, authority: VideoAuthority) -> Result<Self, RtpError> {
        if payload_type > 127 || !authority.ready {
            return Err(RtpError::NotReady);
        }
        Ok(Self {
            ssrc,
            payload_type,
            epoch: authority.epoch,
            ready: true,
            timestamp: None,
            next_sequence: None,
            fragment_header: None,
            bytes: Vec::new(),
            packet_count: 0,
            last_timestamp: None,
        })
    }
    fn discard(&mut self) {
        self.timestamp = None;
        self.fragment_header = None;
        self.bytes.clear();
        self.packet_count = 0;
    }
    pub fn push(
        &mut self,
        packet: &[u8],
        authority: VideoAuthority,
    ) -> Result<Option<Vec<u8>>, RtpError> {
        if !self.ready || !authority.ready || authority.epoch != self.epoch {
            self.ready = false;
            self.discard();
            return Err(RtpError::NotReady);
        }
        let result = self.push_checked(packet);
        if result.is_err() {
            self.discard();
        }
        result
    }
    fn push_checked(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>, RtpError> {
        if packet.len() <= HEADER_BYTES
            || packet.len() > 1500
            || packet[0] != 0x80
            || packet[1] & 0x7f != self.payload_type
            || u32::from_be_bytes(packet[8..12].try_into().unwrap()) != self.ssrc
        {
            return Err(RtpError::InvalidPacket);
        }
        let sequence = u16::from_be_bytes(packet[2..4].try_into().unwrap());
        let timestamp = u32::from_be_bytes(packet[4..8].try_into().unwrap());
        if self.next_sequence.is_some_and(|next| next != sequence) {
            // Move past the discarded packet; missing FU starts still fail below.
            self.next_sequence = Some(sequence.wrapping_add(1));
            return Err(RtpError::Discontinuity);
        }
        self.next_sequence = Some(sequence.wrapping_add(1));
        if let Some(current) = self.timestamp {
            if timestamp != current {
                return Err(RtpError::Discontinuity);
            }
        } else {
            if self.last_timestamp.is_some_and(|last| {
                let delta = timestamp.wrapping_sub(last);
                delta == 0 || delta >= 1 << 31
            }) {
                return Err(RtpError::Discontinuity);
            }
            self.timestamp = Some(timestamp);
        }
        self.packet_count += 1;
        if self.packet_count > MAX_PACKETS {
            return Err(RtpError::Limit);
        }
        let payload = &packet[HEADER_BYTES..];
        if payload[0] & 0x80 != 0 {
            return Err(RtpError::InvalidPacket);
        }
        match payload[0] & 0x1f {
            1..=23 => {
                if self.fragment_header.is_some() {
                    return Err(RtpError::Discontinuity);
                }
                self.append(&START)?;
                self.append(payload)?;
            }
            28 => {
                if payload.len() < 3 || payload[1] & 0x20 != 0 {
                    return Err(RtpError::InvalidPacket);
                }
                let start = payload[1] & 0x80 != 0;
                let end = payload[1] & 0x40 != 0;
                let header = (payload[0] & 0xe0) | (payload[1] & 0x1f);
                if start && end || !(1..=23).contains(&(header & 0x1f)) {
                    return Err(RtpError::InvalidPacket);
                }
                if start {
                    if self.fragment_header.is_some() {
                        return Err(RtpError::Discontinuity);
                    }
                    self.append(&START)?;
                    self.append(&[header])?;
                    self.fragment_header = Some(header);
                } else if self.fragment_header != Some(header) {
                    return Err(RtpError::Discontinuity);
                }
                self.append(&payload[2..])?;
                if end {
                    self.fragment_header = None;
                }
            }
            _ => return Err(RtpError::Unsupported),
        }
        if packet[1] & 0x80 != 0 {
            if self.fragment_header.is_some() {
                return Err(RtpError::InvalidPacket);
            }
            self.last_timestamp = self.timestamp;
            let frame = std::mem::take(&mut self.bytes);
            self.discard();
            // Structural filter only. Davey must authenticate before decoding.
            if frame.len() < 11 || !frame.ends_with(&[0xfa, 0xfa]) {
                return Err(RtpError::InvalidFrame);
            }
            return Ok(Some(frame));
        }
        Ok(None)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), RtpError> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_FRAME {
            return Err(RtpError::Limit);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> VideoAuthority {
        VideoAuthority {
            epoch: 1,
            ready: true,
        }
    }
    fn packet(payload: &[u8], sequence: u16, marker: bool) -> Vec<u8> {
        let mut packet = make_packet(
            payload,
            RtpMetadata {
                ssrc: 4,
                payload_type: 102,
                timestamp: 90,
                first_sequence: sequence,
            },
            0,
            1,
        )
        .bytes;
        if marker {
            packet[1] |= 0x80;
        }
        packet
    }

    #[test]
    fn malformed_packet_and_fu_headers_fail_closed() {
        for length in 0..13 {
            let mut receive = H264Reassembler::new(4, 102, authority()).unwrap();
            assert!(receive.push(&vec![0; length], authority()).is_err());
        }
        for payload in [
            vec![28],
            vec![28, 0x85],
            vec![28, 0xc5, 1],
            vec![28, 0xa5, 1],
            vec![28, 0x80, 1],
            vec![24, 1, 2],
        ] {
            let mut receive = H264Reassembler::new(4, 102, authority()).unwrap();
            assert!(
                receive
                    .push(&packet(&payload, 0, false), authority())
                    .is_err()
            );
        }
        // Marker on an unfinished FU cannot finish the access unit.
        let mut receive = H264Reassembler::new(4, 102, authority()).unwrap();
        assert!(
            receive
                .push(&packet(&[28, 0x85, 1], 0, true), authority())
                .is_err()
        );
    }

    #[test]
    fn receive_frame_size_and_packet_count_are_bounded() {
        let mut receive = H264Reassembler::new(4, 102, authority()).unwrap();
        let mut payload = vec![0x42; 1488];
        payload[0] = 0x65;
        let mut hit_limit = false;
        for sequence in 0..MAX_PACKETS as u16 {
            match receive.push(&packet(&payload, sequence, false), authority()) {
                Err(RtpError::Limit) => {
                    hit_limit = true;
                    break;
                }
                Ok(None) => assert!(receive.bytes.len() <= MAX_FRAME),
                _ => panic!("unexpected bounded receive result"),
            }
        }
        assert!(hit_limit);
        assert!(receive.bytes.is_empty());
        let mut receive = H264Reassembler::new(4, 102, authority()).unwrap();
        for sequence in 0..MAX_PACKETS as u16 {
            assert!(
                receive
                    .push(&packet(&[0x65], sequence, false), authority())
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            receive.push(&packet(&[0x65], MAX_PACKETS as u16, false), authority()),
            Err(RtpError::Limit)
        );
    }

    #[test]
    fn canonical_nal_boundary_parser_rejects_malformed_and_excess_units() {
        for bytes in [
            vec![],
            vec![0, 0, 1, 0x65],
            vec![0, 0, 0, 1],
            vec![0, 0, 0, 1, 0xe5],
            vec![0, 0, 0, 1, 0x65, 0, 0, 1, 0x65],
        ] {
            assert!(split_nals(&bytes).is_err());
        }
        let mut excessive = Vec::new();
        for _ in 0..MAX_PACKETS + 1 {
            excessive.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        }
        assert_eq!(split_nals(&excessive), Err(RtpError::Limit));
    }
}
