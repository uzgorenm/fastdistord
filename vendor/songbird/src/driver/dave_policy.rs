//! fastdistord's fail-closed policy only. Cryptography remains in davey.
//! This module can be tested without a socket, account, or audio device.

pub(crate) fn negotiated_ready(
    active: bool,
    protocol: u16,
    session_ready: bool,
    connected: bool,
) -> bool {
    active && protocol != 0 && session_ready && connected
}

pub(crate) fn allow_transmit(required: bool, negotiated: bool, encrypted: bool) -> bool {
    !required || (negotiated && encrypted)
}

pub(crate) fn allow_receive(
    required: bool,
    negotiated: bool,
    protocol: u16,
    transport_decrypted: bool,
    frame: &[u8],
) -> bool {
    !required
        || (negotiated
            && protocol != 0
            && transport_decrypted
            && frame.len() >= 11
            && frame.ends_with(&[0xfa, 0xfa]))
}

pub(crate) fn allow_send(
    required: bool,
    negotiated: bool,
    packet_generation: u64,
    current_generation: u64,
) -> bool {
    !required || (negotiated && packet_generation == current_generation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_ready_is_insufficient_for_dave_readiness() {
        assert!(!negotiated_ready(false, 1, true, true));
        assert!(!negotiated_ready(true, 1, false, true));
        assert!(!negotiated_ready(true, 0, true, true));
        assert!(!negotiated_ready(true, 1, true, false));
        assert!(negotiated_ready(true, 1, true, true));
    }

    #[test]
    fn required_transmit_has_no_plaintext_or_unnegotiated_fallback() {
        for negotiated in [false, true] {
            for encrypted in [false, true] {
                assert_eq!(
                    allow_transmit(true, negotiated, encrypted),
                    negotiated && encrypted
                );
            }
        }
    }

    #[test]
    fn required_receive_rejects_plaintext_silence_and_downgrades() {
        let mut encrypted_frame = [0u8; 11];
        encrypted_frame[9..].copy_from_slice(&[0xfa, 0xfa]);
        assert!(allow_receive(true, true, 1, true, &encrypted_frame));
        assert!(!allow_receive(true, false, 1, true, &encrypted_frame));
        assert!(!allow_receive(true, true, 0, true, &encrypted_frame));
        assert!(!allow_receive(true, true, 1, false, &encrypted_frame));
        assert!(!allow_receive(true, true, 1, true, &[0xf8, 0xff, 0xfe]));
        assert!(!allow_receive(true, true, 1, true, &[0xfa, 0xfa]));
        assert!(!allow_receive(true, true, 1, true, &[]));
    }

    #[test]
    fn prepare_execute_cycle_drops_old_ciphertext_even_when_ready_again() {
        assert!(allow_send(true, true, 7, 7));
        assert!(!allow_send(true, false, 7, 8));
        assert!(!allow_send(true, true, 7, 8));
        assert!(allow_send(true, true, 8, 8));
    }

    #[test]
    fn non_required_mode_retains_upstream_behavior() {
        assert!(allow_transmit(false, false, false));
        assert!(allow_receive(false, false, 0, false, &[]));
    }
}

/// Strict-mode retry authority belongs to the application coordinator.
pub(crate) fn allow_internal_retry(require_dave: bool) -> bool {
    !require_dave
}
#[cfg(test)]
mod retry_tests {
    #[test]
    fn strict_mode_does_not_hide_internal_retries() {
        assert!(!super::allow_internal_retry(true));
        assert!(super::allow_internal_retry(false));
    }
}
