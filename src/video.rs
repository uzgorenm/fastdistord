//! Encoded video -> DAVE boundary, shared by camera and screen sharing.
//!
//! This is an offline pipeline component, not a Discord video transport.
//! Capture, encoding, codec/SSRC negotiation, RTP packetization, transport AEAD
//! and receive/decode are not wired into the app. Never send these bytes directly
//! over UDP. A transport must hold its DAVE state lock while checking the current
//! epoch and ready gate and committing a packet, as the voice path does.
use std::{borrow::Cow, fmt};

use davey::{Codec, DaveSession, MediaType};

pub const MAX_ENCODED_FRAME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum VideoCodec {
    /// Annex B access unit, not VideoToolbox's length-prefixed AVCC output.
    H264,
    Vp8,
}

impl VideoCodec {
    fn dave_codec(self) -> Codec {
        match self {
            Self::H264 => Codec::H264,
            Self::Vp8 => Codec::VP8,
        }
    }
}

/// Published by the negotiated session owner; every transition increments epoch.
#[derive(Clone, Copy)]
pub struct VideoAuthority {
    pub epoch: u64,
    pub ready: bool,
}

pub struct EncodedVideoFrame {
    codec: VideoCodec,
    bytes: Vec<u8>,
}

impl EncodedVideoFrame {
    pub fn new(codec: VideoCodec, bytes: Vec<u8>) -> Result<Self, VideoError> {
        if bytes.is_empty() || bytes.len() > MAX_ENCODED_FRAME_BYTES {
            return Err(VideoError::InvalidFrame);
        }
        if matches!(codec, VideoCodec::H264)
            && !(bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]))
        {
            return Err(VideoError::InvalidFrame);
        }
        Ok(Self { codec, bytes })
    }

    /// Codec headers/ranges are handled by Davey. No plaintext fallback is exposed.
    pub fn encrypt(
        self,
        session: &mut DaveSession,
        authority: VideoAuthority,
    ) -> Result<EncryptedVideoFrame, VideoError> {
        if !authority.ready || !session.is_ready() {
            return Err(VideoError::NotReady);
        }
        let encrypted = session
            .encrypt(MediaType::VIDEO, self.codec.dave_codec(), &self.bytes)
            .map_err(|_| VideoError::EncryptionFailed)?;
        let Cow::Owned(bytes) = encrypted else {
            return Err(VideoError::EncryptionFailed);
        };
        // Same structural filter as strict voice receive; Davey performs the
        // cryptography. The marker alone never authenticates a received frame.
        if bytes.len() > MAX_ENCODED_FRAME_BYTES + 4096
            || bytes.len() < 11
            || !bytes.ends_with(&[0xfa, 0xfa])
        {
            return Err(VideoError::EncryptionFailed);
        }
        Ok(EncryptedVideoFrame {
            bytes,
            epoch: authority.epoch,
        })
    }
}

/// Frame-level ciphertext to packetize, never raw codec output.
/// Intentionally no Debug implementation: diagnostics must not contain media.
pub struct EncryptedVideoFrame {
    bytes: Vec<u8>,
    epoch: u64,
}

impl EncryptedVideoFrame {
    /// Recheck during final packet commit under the session owner's lock.
    /// An epoch change invalidates the entire frame, including queued fragments.
    pub fn payload_for(&self, authority: VideoAuthority) -> Result<&[u8], VideoError> {
        if !authority.ready || authority.epoch != self.epoch {
            return Err(VideoError::NotReady);
        }
        Ok(&self.bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoError {
    InvalidFrame,
    NotReady,
    EncryptionFailed,
}
impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidFrame => "Video frame is empty, too large or in an unsupported format.",
            Self::NotReady => "Video encryption authority is not ready or was superseded.",
            Self::EncryptionFailed => "Video encryption failed; the frame was discarded.",
        })
    }
}
impl std::error::Error for VideoError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    // Capture/encoder mistakes must fail before touching session cryptography.
    #[test]
    fn rejects_oversized_frames_and_avcc_input() {
        assert!(EncodedVideoFrame::new(VideoCodec::Vp8, vec![]).is_err());
        assert!(
            EncodedVideoFrame::new(VideoCodec::Vp8, vec![0; MAX_ENCODED_FRAME_BYTES + 1]).is_err()
        );
        assert!(EncodedVideoFrame::new(VideoCodec::H264, vec![0, 0, 0, 5, 0x65]).is_err());
        assert!(EncodedVideoFrame::new(VideoCodec::H264, vec![0, 0, 0, 1, 0x65, 1]).is_ok());
    }

    // Real Davey session, without a handshake: neither caller readiness nor
    // codec support can authorize unnegotiated video transmission.
    #[test]
    fn unnegotiated_dave_session_cannot_encrypt_video() {
        let mut session = DaveSession::new(NonZeroU16::new(1).unwrap(), 1, 2, None).unwrap();
        for ready in [false, true] {
            let frame = EncodedVideoFrame::new(VideoCodec::Vp8, vec![0x10, 1, 2, 3]).unwrap();
            assert!(matches!(
                frame.encrypt(&mut session, VideoAuthority { epoch: 7, ready }),
                Err(VideoError::NotReady)
            ));
        }
    }

    // A complete ready -> negotiating -> ready cycle must revoke old fragments.
    #[test]
    fn prepared_frame_is_revoked_by_transition_or_closed_gate() {
        let frame = EncryptedVideoFrame {
            bytes: vec![1, 2, 3],
            epoch: 7,
        };
        assert!(
            frame
                .payload_for(VideoAuthority {
                    epoch: 7,
                    ready: true
                })
                .is_ok()
        );
        assert_eq!(
            frame
                .payload_for(VideoAuthority {
                    epoch: 7,
                    ready: false
                })
                .unwrap_err(),
            VideoError::NotReady
        );
        assert_eq!(
            frame
                .payload_for(VideoAuthority {
                    epoch: 8,
                    ready: true
                })
                .unwrap_err(),
            VideoError::NotReady
        );
    }
}
