//! Offline MLS delivery fixture: no Discord token, gateway, capture or network.
//! External Add/Remove construction follows OpenMLS's primary implementation:
//! https://github.com/openmls/openmls/blob/openmls-v0.8.2/openmls/tests/book_code.rs
use std::num::NonZeroU16;

use davey::{DaveSession, MediaType, ProposalsOperationType};
use fastdistord::video::{EncodedVideoFrame, VideoAuthority, VideoCodec};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use tls_codec::{DeserializeBytes, Serialize};

struct OfflineGroup {
    alice: DaveSession,
    bob: DaveSession,
    sender: SignatureKeyPair,
}

impl OfflineGroup {
    fn new() -> Self {
        let version = NonZeroU16::new(1).unwrap();
        let mut alice = DaveSession::new(version, 101, 303, None).unwrap();
        let mut bob = DaveSession::new(version, 202, 303, None).unwrap();
        let sender = SignatureKeyPair::new(alice.ciphersuite().signature_algorithm()).unwrap();
        let external = ExternalSender::new(
            sender.public().into(),
            BasicCredential::new(b"offline-delivery-fixture".to_vec()).into(),
        )
        .tls_serialize_detached()
        .unwrap();
        alice.set_external_sender(&external).unwrap();
        bob.set_external_sender(&external).unwrap();
        let package = KeyPackageIn::tls_deserialize_exact_bytes(&bob.create_key_package().unwrap())
            .unwrap()
            .validate(
                OpenMlsRustCrypto::default().crypto(),
                ProtocolVersion::Mls10,
            )
            .unwrap();
        let proposal = ExternalProposal::new_add::<OpenMlsRustCrypto>(
            package,
            alice.group().unwrap().group_id().clone(),
            alice.epoch().unwrap(),
            &sender,
            SenderExtensionIndex::new(0),
        )
        .unwrap()
        .tls_serialize_detached()
        .unwrap();
        let proposals = VLBytes::new(proposal).tls_serialize_detached().unwrap();
        let result = alice
            .process_proposals(
                ProposalsOperationType::APPEND,
                &proposals,
                Some(&[101, 202]),
            )
            .unwrap()
            .unwrap();
        alice.process_commit(&result.commit).unwrap();
        bob.process_welcome(result.welcome.as_ref().unwrap())
            .unwrap();
        assert!(alice.is_ready() && bob.is_ready());
        assert_eq!(
            alice.get_epoch_authenticator().unwrap().as_slice(),
            bob.get_epoch_authenticator().unwrap().as_slice()
        );
        alice.set_passthrough_mode(false, Some(0));
        bob.set_passthrough_mode(false, Some(0));
        Self { alice, bob, sender }
    }
}

fn h264_sample() -> Vec<u8> {
    // Synthetic VCL access unit, not a claim of decoder validity. Exp-Golomb
    // slice metadata remains readable for Davey's codec-aware parser.
    let mut frame = vec![0, 0, 0, 1, 0x65, 0xb8];
    frame.extend(std::iter::repeat_n(0x42, 4096));
    frame
}

#[test]
fn two_participant_video_encryption_roundtrip_and_tamper_rejection() {
    let mut group = OfflineGroup::new();
    for (codec, plaintext) in [
        (VideoCodec::H264, h264_sample()),
        (
            VideoCodec::Vp8,
            vec![1, 9, 8, 7, 6, 5, 4, 3, 2, 1, 11, 12, 13],
        ),
    ] {
        let authority = VideoAuthority {
            epoch: 4,
            ready: true,
        };
        let encrypted = EncodedVideoFrame::new(codec, plaintext.clone())
            .unwrap()
            .encrypt(&mut group.alice, authority)
            .unwrap();
        let ciphertext = encrypted.payload_for(authority).unwrap();
        assert_ne!(ciphertext, plaintext);
        // Corrupt authenticated ciphertext before attempting the original frame.
        let mut tampered = ciphertext.to_vec();
        tampered[11] ^= 0x40;
        assert!(group.bob.decrypt(101, MediaType::VIDEO, &tampered).is_err());
        assert_eq!(
            group
                .bob
                .decrypt(101, MediaType::VIDEO, ciphertext)
                .unwrap(),
            plaintext
        );
        assert!(
            group
                .bob
                .decrypt(101, MediaType::VIDEO, ciphertext)
                .is_err(),
            "replay must fail"
        );
        assert!(
            group
                .bob
                .decrypt(999, MediaType::VIDEO, ciphertext)
                .is_err()
        );
    }
    let plaintext = h264_sample();
    let authority = VideoAuthority {
        epoch: 4,
        ready: true,
    };
    let encrypted = EncodedVideoFrame::new(VideoCodec::H264, plaintext.clone())
        .unwrap()
        .encrypt(&mut group.bob, authority)
        .unwrap();
    assert_eq!(
        group
            .alice
            .decrypt(
                202,
                MediaType::VIDEO,
                encrypted.payload_for(authority).unwrap()
            )
            .unwrap(),
        plaintext
    );
}

#[test]
fn membership_removal_revokes_video_authority_and_recipient() {
    let mut group = OfflineGroup::new();
    let authority = VideoAuthority {
        epoch: 4,
        ready: true,
    };
    let queued = EncodedVideoFrame::new(VideoCodec::H264, h264_sample())
        .unwrap()
        .encrypt(&mut group.alice, authority)
        .unwrap();
    let removed = group.bob.own_leaf_index().unwrap();
    let proposal = ExternalProposal::new_remove::<OpenMlsRustCrypto>(
        removed,
        group.alice.group().unwrap().group_id().clone(),
        group.alice.epoch().unwrap(),
        &group.sender,
        SenderExtensionIndex::new(0),
    )
    .unwrap()
    .tls_serialize_detached()
    .unwrap();
    let proposals = VLBytes::new(proposal).tls_serialize_detached().unwrap();
    let result = group
        .alice
        .process_proposals(ProposalsOperationType::APPEND, &proposals, Some(&[101]))
        .unwrap()
        .unwrap();
    assert!(result.welcome.is_none());
    group.alice.process_commit(&result.commit).unwrap();
    assert!(
        queued
            .payload_for(VideoAuthority {
                epoch: 5,
                ready: true
            })
            .is_err()
    );
    assert!(
        queued
            .payload_for(VideoAuthority {
                epoch: 4,
                ready: false
            })
            .is_err()
    );
    assert_eq!(group.alice.get_user_ids().unwrap(), vec![101]);
    let fresh = EncodedVideoFrame::new(VideoCodec::H264, h264_sample())
        .unwrap()
        .encrypt(
            &mut group.alice,
            VideoAuthority {
                epoch: 5,
                ready: true,
            },
        )
        .unwrap();
    assert!(
        group
            .bob
            .decrypt(
                101,
                MediaType::VIDEO,
                fresh
                    .payload_for(VideoAuthority {
                        epoch: 5,
                        ready: true
                    })
                    .unwrap()
            )
            .is_err()
    );
}

#[test]
fn h264_dave_rtp_fragmentation_reassembly_preserves_authenticated_frame() {
    use fastdistord::media::rtp::{H264Reassembler, RtpMetadata, packetize_h264};
    let mut group = OfflineGroup::new();
    let authority = VideoAuthority {
        epoch: 7,
        ready: true,
    };
    // Two NALs exercise a small single packet before fragmented slice. Davey
    // authenticates SPS bytes and the start codes, and encrypts VCL data.
    let mut plaintext = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xaa];
    plaintext.extend(h264_sample());
    let encrypted = EncodedVideoFrame::new(VideoCodec::H264, plaintext.clone())
        .unwrap()
        .encrypt(&mut group.alice, authority)
        .unwrap();
    let metadata = RtpMetadata {
        ssrc: 44,
        payload_type: 102,
        timestamp: 9000,
        first_sequence: 65534,
    };
    let packets = packetize_h264(&encrypted, authority, metadata, 500).unwrap();
    assert!(packets.len() > 3);
    let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
    let mut output = None;
    for (index, packet) in packets.iter().enumerate() {
        let bytes = packet.bytes_for(authority).unwrap();
        assert!(bytes.len() <= 500);
        assert_eq!(bytes[1] & 0x80 != 0, index + 1 == packets.len());
        assert_eq!(
            u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
            65534_u16.wrapping_add(index as u16)
        );
        output = receiver.push(bytes, authority).unwrap();
        if index + 1 != packets.len() {
            assert!(output.is_none());
        }
    }
    let ciphertext = output.unwrap();
    assert_eq!(ciphertext, encrypted.payload_for(authority).unwrap());
    assert_eq!(
        group
            .bob
            .decrypt(101, MediaType::VIDEO, &ciphertext)
            .unwrap(),
        plaintext
    );
    for packet in &packets {
        assert!(
            packet
                .bytes_for(VideoAuthority {
                    epoch: 8,
                    ready: true
                })
                .is_err()
        );
        assert!(
            packet
                .bytes_for(VideoAuthority {
                    epoch: 7,
                    ready: false
                })
                .is_err()
        );
    }
}

#[test]
fn bounded_rtp_receiver_rejects_loss_spoofing_replay_and_stale_fragments() {
    use fastdistord::media::rtp::{H264Reassembler, RtpMetadata, packetize_h264};
    let mut group = OfflineGroup::new();
    let authority = VideoAuthority {
        epoch: 7,
        ready: true,
    };
    let frame = EncodedVideoFrame::new(VideoCodec::H264, h264_sample())
        .unwrap()
        .encrypt(&mut group.alice, authority)
        .unwrap();
    let packets = packetize_h264(
        &frame,
        authority,
        RtpMetadata {
            ssrc: 44,
            payload_type: 102,
            timestamp: 9000,
            first_sequence: 22,
        },
        500,
    )
    .unwrap();
    let first = packets[0].bytes_for(authority).unwrap();
    let second = packets[1].bytes_for(authority).unwrap();
    let third = packets[2].bytes_for(authority).unwrap();
    let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
    assert!(receiver.push(first, authority).unwrap().is_none());
    assert!(receiver.push(third, authority).is_err()); // Lost second packet.
    assert!(receiver.push(second, authority).is_err()); // Reordering cannot repair.
    for (offset, value) in [(0, 0x90), (1, 101), (8, 9), (4, 5)] {
        let mut altered = second.to_vec();
        altered[offset] = value;
        let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
        receiver.push(first, authority).unwrap();
        assert!(receiver.push(&altered, authority).is_err());
    }
    let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
    receiver.push(first, authority).unwrap();
    assert!(receiver.push(first, authority).is_err()); // Duplicate sequence.
    let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
    receiver.push(first, authority).unwrap();
    assert!(
        receiver
            .push(
                second,
                VideoAuthority {
                    epoch: 8,
                    ready: true
                }
            )
            .is_err()
    );
    // Epoch revocation permanently invalidates this assembler.
    assert!(receiver.push(second, authority).is_err());
    assert!(
        packetize_h264(
            &frame,
            authority,
            RtpMetadata {
                ssrc: 44,
                payload_type: 128,
                timestamp: 0,
                first_sequence: 0
            },
            500
        )
        .is_err()
    );
}

/// Explicit native-hardware diagnostic. Synthetic pixels only: no capture,
/// microphone, credentials, GUI, network or transmission. Ignored because
/// VideoToolbox encoder availability depends on the host execution environment.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an available native VideoToolbox encoder/decoder"]
fn native_h264_dave_rtp_decode_roundtrip() {
    use fastdistord::media::macos_codec::{
        H264Codec, synthetic_encoded_frame, verify_synthetic_frame,
    };
    use fastdistord::media::rtp::{H264Reassembler, RtpMetadata, packetize_h264};
    use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};

    let codec = H264Codec::new(320, 180).expect("native codec dimensions");
    let encoded = synthetic_encoded_frame(320, 180).expect("native synthetic H264 encode");
    let mut group = OfflineGroup::new();
    let authority = VideoAuthority {
        epoch: 9,
        ready: true,
    };
    let encrypted = EncodedVideoFrame::new(VideoCodec::H264, encoded.clone())
        .expect("native Annex B access unit")
        .encrypt(&mut group.alice, authority)
        .expect("real Alice DAVE encryption");
    let packets = packetize_h264(
        &encrypted,
        authority,
        RtpMetadata {
            ssrc: 44,
            payload_type: 102,
            timestamp: 9000,
            first_sequence: 65000,
        },
        1200,
    )
    .expect("offline H264 RTP packetization");
    let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
    let mut reconstructed = None;
    for (index, packet) in packets.iter().enumerate() {
        reconstructed = receiver
            .push(packet.bytes_for(authority).unwrap(), authority)
            .expect("ordered offline reassembly");
        assert_eq!(reconstructed.is_some(), index + 1 == packets.len());
    }
    let ciphertext = reconstructed.expect("complete encrypted access unit");
    assert_eq!(ciphertext, encrypted.payload_for(authority).unwrap());
    let decrypted = group
        .bob
        .decrypt(101, MediaType::VIDEO, &ciphertext)
        .expect("real Bob DAVE authentication/decryption");
    assert_eq!(
        decrypted, encoded,
        "RTP must preserve all authenticated native NAL bytes"
    );
    let decoded = codec
        .decode(&decrypted)
        .expect("native decode after decryption");
    verify_synthetic_frame(&decoded, 320, 180).expect("decoded checkerboard fidelity");
    assert_eq!(
        (
            CVPixelBufferGetWidth(&decoded),
            CVPixelBufferGetHeight(&decoded)
        ),
        (320, 180)
    );
}
