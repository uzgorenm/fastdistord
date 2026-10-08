//! Synthetic transport tests: no credentials, capture or Discord connection.
use super::{DatagramError, RtpReceiver, RtpSender, TransportMode};
use crate::video::VideoAuthority;
use zeroize::Zeroizing;

fn packet(sequence: u16) -> Vec<u8> {
    let mut bytes = vec![0x80, 102];
    bytes.extend(sequence.to_be_bytes());
    bytes.extend(9000_u32.to_be_bytes());
    bytes.extend(44_u32.to_be_bytes());
    bytes.extend([0x65, 0xb8, 1, 2, 3]);
    bytes
}
fn authority() -> VideoAuthority {
    VideoAuthority {
        epoch: 7,
        ready: true,
    }
}

#[test]
fn both_modes_authenticate_header_payload_and_counter_without_replay_poisoning() {
    for mode in [TransportMode::Aes256Gcm, TransportMode::XChaCha20Poly1305] {
        let mut sender = RtpSender::new(mode, Zeroizing::new([7; 32]), 7, 0);
        let mut receiver = RtpReceiver::new(mode, Zeroizing::new([7; 32]), 7, 44);
        let bytes = sender.seal_rtp(&packet(1), authority()).unwrap();
        for offset in [0, 1, 2, 4, 8, 12, bytes.len() - 1] {
            let mut corrupt = bytes.clone();
            corrupt[offset] ^= 1;
            assert!(receiver.open(&corrupt, authority()).is_err());
        }
        assert_eq!(receiver.open(&bytes, authority()).unwrap(), packet(1));
        assert_eq!(
            receiver.open(&bytes, authority()),
            Err(DatagramError::Replay)
        );
        let mut wrong = RtpReceiver::new(mode, Zeroizing::new([8; 32]), 7, 44);
        assert!(wrong.open(&bytes, authority()).is_err());
        let mut wrong = RtpReceiver::new(mode, Zeroizing::new([7; 32]), 7, 45);
        assert!(wrong.open(&bytes, authority()).is_err());
    }
}

#[test]
fn reordered_authenticated_packets_have_a_bounded_replay_window() {
    let mut tx = RtpSender::new(TransportMode::Aes256Gcm, Zeroizing::new([9; 32]), 7, 0);
    let mut rx = RtpReceiver::new(TransportMode::Aes256Gcm, Zeroizing::new([9; 32]), 7, 44);
    let packets: Vec<_> = (0..140)
        .map(|s| tx.seal_rtp(&packet(s), authority()).unwrap())
        .collect();
    assert!(rx.open(&packets[139], authority()).is_ok());
    assert!(rx.open(&packets[138], authority()).is_ok());
    assert_eq!(
        rx.open(&packets[138], authority()),
        Err(DatagramError::Replay)
    );
    assert_eq!(
        rx.open(&packets[0], authority()),
        Err(DatagramError::Replay)
    );
}

#[test]
fn exhausted_nonce_and_revoked_sessions_never_reopen() {
    let mut tx = RtpSender::new(
        TransportMode::Aes256Gcm,
        Zeroizing::new([9; 32]),
        7,
        u32::MAX,
    );
    let bytes = tx.seal_rtp(&packet(1), authority()).unwrap();
    assert_eq!(
        tx.seal_rtp(&packet(2), authority()),
        Err(DatagramError::Exhausted)
    );
    let mut rx = RtpReceiver::new(TransportMode::Aes256Gcm, Zeroizing::new([9; 32]), 7, 44);
    let revoked = VideoAuthority {
        epoch: 8,
        ready: true,
    };
    assert_eq!(rx.open(&bytes, revoked), Err(DatagramError::NotReady));
    assert_eq!(rx.open(&bytes, authority()), Err(DatagramError::NotReady));
    let mut tx = RtpSender::new(TransportMode::Aes256Gcm, Zeroizing::new([9; 32]), 7, 0);
    tx.invalidate();
    assert_eq!(
        tx.seal_rtp(&packet(1), authority()),
        Err(DatagramError::NotReady)
    );
}

#[test]
fn malformed_rtp_and_unknown_modes_fail_before_transport_use() {
    assert_eq!(
        TransportMode::from_negotiated("xsalsa20_poly1305"),
        Err(DatagramError::Unsupported)
    );
    assert_eq!(
        TransportMode::from_negotiated("aead_aes256_gcm_rtpsize"),
        Ok(TransportMode::Aes256Gcm)
    );
    let mut tx = RtpSender::new(TransportMode::Aes256Gcm, Zeroizing::new([1; 32]), 7, 0);
    for bytes in [vec![], vec![0; 13], vec![0x80; 1600]] {
        assert!(tx.seal_rtp(&bytes, authority()).is_err());
    }
    for header in [0x90, 0xa0, 0x81] {
        let mut bytes = packet(0);
        bytes[0] = header;
        assert!(tx.seal_rtp(&bytes, authority()).is_err());
    }
}
