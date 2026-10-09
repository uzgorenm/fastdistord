//! Offline MLS delivery fixture: no Discord token, gateway, capture or network.
//! External Add/Remove construction follows OpenMLS's primary implementation:
//! https://github.com/openmls/openmls/blob/openmls-v0.8.2/openmls/tests/book_code.rs
use std::num::NonZeroU16;

use davey::{DaveSession, MediaType, ProposalsOperationType};
use fastdistord::video::{EncodedVideoFrame, VideoAuthority, VideoCodec};
use openmls::prelude::*;
use openmls::treesync::LeafNodeSource;
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
        assert!(
            !alice.is_ready() && !bob.is_ready(),
            "a sole pending group must not expose a media ratchet"
        );
        assert!(alice.encrypt_opus(&[0xf8, 0xff, 0xfe]).is_err());
        let empty = VLBytes::new(vec![]).tls_serialize_detached().unwrap();
        assert!(
            alice
                .process_proposals(ProposalsOperationType::APPEND, &empty, Some(&[101]))
                .unwrap()
                .is_none()
        );
        assert!(!alice.is_ready());
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
        assert!(
            !alice.is_ready() && !bob.is_ready(),
            "preparing a commit is not negotiated media readiness"
        );
        alice.process_commit(&result.commit).unwrap();
        assert!(
            alice.is_ready(),
            "the creator establishes keys from its echoed commit without receiving a Welcome"
        );
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
    use fastdistord::media::datagram::{RtpReceiver, RtpSender, TransportMode};
    use zeroize::Zeroizing;
    let mut transport_tx = RtpSender::new(TransportMode::Aes256Gcm, Zeroizing::new([7; 32]), 9, 0);
    let mut transport_rx =
        RtpReceiver::new(TransportMode::Aes256Gcm, Zeroizing::new([7; 32]), 9, 44);
    let mut receiver = H264Reassembler::new(44, 102, authority).unwrap();
    let mut reconstructed = None;
    for (index, packet) in packets.iter().enumerate() {
        let datagram = transport_tx.seal_video(packet, authority).unwrap();
        let authenticated = transport_rx.open(&datagram, authority).unwrap();
        reconstructed = receiver
            .push(&authenticated, authority)
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

#[test]
fn dave_rtp_transport_aead_roundtrip_in_both_modes() {
    use fastdistord::media::{
        datagram::{RtpReceiver, RtpSender, TransportMode},
        rtp::{H264Reassembler, RtpMetadata, packetize_h264},
    };
    use zeroize::Zeroizing;
    for mode in [TransportMode::Aes256Gcm, TransportMode::XChaCha20Poly1305] {
        let mut group = OfflineGroup::new();
        let authority = VideoAuthority {
            epoch: 7,
            ready: true,
        };
        let plaintext = h264_sample();
        let frame = EncodedVideoFrame::new(VideoCodec::H264, plaintext.clone())
            .unwrap()
            .encrypt(&mut group.alice, authority)
            .unwrap();
        // Synthetic fixture key, never taken from an active audio connection.
        let mut tx = RtpSender::new(mode, Zeroizing::new([7; 32]), 7, 100);
        let mut rx = RtpReceiver::new(mode, Zeroizing::new([7; 32]), 7, 44);
        let mut assembler = H264Reassembler::new(44, 102, authority).unwrap();
        let packets = packetize_h264(
            &frame,
            authority,
            RtpMetadata {
                ssrc: 44,
                payload_type: 102,
                timestamp: 9000,
                first_sequence: 0,
            },
            1180,
        )
        .unwrap();
        let mut output = None;
        for packet in packets {
            let datagram = tx.seal_video(&packet, authority).unwrap();
            assert!(datagram.len() <= 1200);
            let authenticated = rx.open(&datagram, authority).unwrap();
            output = assembler.push(&authenticated, authority).unwrap();
        }
        assert_eq!(
            group
                .bob
                .decrypt(101, MediaType::VIDEO, &output.unwrap())
                .unwrap(),
            plaintext
        );
    }
}

/// Explicit loopback diagnostic: only synthetic ciphertext on 127.0.0.1.
/// No Discord, credentials, capture, account signaling or external destination.
#[test]
#[ignore = "requires permission to bind/send local UDP sockets"]
fn synthetic_encrypted_video_over_loopback_udp() {
    use fastdistord::media::{
        datagram::{RtpReceiver, RtpSender, TransportMode},
        rtp::{H264Reassembler, RtpMetadata, packetize_h264},
    };
    use std::{net::UdpSocket, time::Duration};
    use zeroize::Zeroizing;
    let mut group = OfflineGroup::new();
    let authority = VideoAuthority {
        epoch: 7,
        ready: true,
    };
    let plaintext = h264_sample();
    let frame = EncodedVideoFrame::new(VideoCodec::H264, plaintext.clone())
        .unwrap()
        .encrypt(&mut group.alice, authority)
        .unwrap();
    let socket_tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    let socket_rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket_tx.connect(socket_rx.local_addr().unwrap()).unwrap();
    socket_rx.connect(socket_tx.local_addr().unwrap()).unwrap();
    socket_rx
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut tx = RtpSender::new(TransportMode::Aes256Gcm, Zeroizing::new([7; 32]), 7, 0);
    let mut rx = RtpReceiver::new(TransportMode::Aes256Gcm, Zeroizing::new([7; 32]), 7, 44);
    let mut assembler = H264Reassembler::new(44, 102, authority).unwrap();
    let packets = packetize_h264(
        &frame,
        authority,
        RtpMetadata {
            ssrc: 44,
            payload_type: 102,
            timestamp: 9000,
            first_sequence: 65534,
        },
        1180,
    )
    .unwrap();
    let mut output = None;
    for packet in packets {
        let datagram = tx.seal_video(&packet, authority).unwrap();
        assert_eq!(socket_tx.send(&datagram).unwrap(), datagram.len());
        let mut buffer = [0; 1500];
        let length = socket_rx.recv(&mut buffer).unwrap();
        let authenticated = rx.open(&buffer[..length], authority).unwrap();
        output = assembler.push(&authenticated, authority).unwrap();
    }
    assert_eq!(
        group
            .bob
            .decrypt(101, MediaType::VIDEO, &output.unwrap())
            .unwrap(),
        plaintext
    );
}

#[test]
fn reported_initial_order_matches_bare_package_send_contract_and_preserves_welcome_key() {
    let version = NonZeroU16::new(1).unwrap();
    let trace = songbird::DaveHandshake::default();
    trace.set_enabled(true);
    trace.record(songbird::DaveStage::Connected, 0);
    trace.record(songbird::DaveStage::Identify, 1);
    trace.record(songbird::DaveStage::Protocol, 1);
    let mut pending = DaveSession::new(version, 202, 303, None).unwrap();
    let raw = pending.create_key_package().unwrap();
    // libdave ExternalSender::ProposeAdd consumes a bare KeyPackage, while
    // discord.js prepends only opcode 26 to Davey's unchanged package.
    let frame = songbird::key_package_frame(&raw);
    assert!(frame.len() == raw.len() + 1 && frame[0] == 26 && frame[1..] == raw);
    let package = KeyPackageIn::tls_deserialize_exact_bytes(&frame[1..])
        .unwrap()
        .validate(
            OpenMlsRustCrypto::default().crypto(),
            ProtocolVersion::Mls10,
        )
        .unwrap();
    assert!(package.ciphersuite() == pending.ciphersuite());
    assert!(package.ciphersuite().tls_serialize_detached().unwrap() == [0, 2]);
    assert!(package.leaf_node().credential().credential_type() == CredentialType::Basic);
    assert!(package.leaf_node().credential().serialized_content() == 202u64.to_be_bytes());
    let LeafNodeSource::KeyPackage(lifetime) = package.leaf_node().leaf_node_source() else {
        panic!("Expected key package lifetime");
    };
    assert!(lifetime.not_before() == 0 && lifetime.not_after() == u64::MAX);
    trace.record(songbird::DaveStage::KeyPackage, 26);
    trace.record(songbird::DaveStage::Peers, 2);
    let mut creator = DaveSession::new(version, 101, 303, None).unwrap();
    let sender = SignatureKeyPair::new(creator.ciphersuite().signature_algorithm()).unwrap();
    let external = ExternalSender::new(
        sender.public().into(),
        BasicCredential::new(b"offline-delivery-fixture".to_vec()).into(),
    )
    .tls_serialize_detached()
    .unwrap();
    pending.set_external_sender(&external).unwrap();
    creator.set_external_sender(&external).unwrap();
    trace.record(songbird::DaveStage::ExternalSender, 25);
    assert!(!pending.is_ready());
    assert!(pending.encrypt_opus(&[0xf8, 0xff, 0xfe]).is_err());
    let proposal = ExternalProposal::new_add::<OpenMlsRustCrypto>(
        package,
        creator.group().unwrap().group_id().clone(),
        creator.epoch().unwrap(),
        &sender,
        SenderExtensionIndex::new(0),
    )
    .unwrap()
    .tls_serialize_detached()
    .unwrap();
    let proposals = VLBytes::new(proposal).tls_serialize_detached().unwrap();
    let candidate = creator
        .process_proposals(
            ProposalsOperationType::APPEND,
            &proposals,
            Some(&[101, 202]),
        )
        .unwrap()
        .unwrap();
    creator.process_commit(&candidate.commit).unwrap();
    pending
        .process_welcome(candidate.welcome.as_ref().unwrap())
        .unwrap();
    assert!(pending.is_ready() && creator.is_ready());
    // Synthetic encoded bytes, deliberately different from the special F8FFFE
    // silence packet. This checks DAVE encryption/authentication, not Opus playback.
    let encoded = [0xf8, 0x01, 0x02, 0x03, 0x04];
    let audio = pending.encrypt_opus(&encoded).unwrap();
    assert!(audio.as_ref() != encoded);
    let mut tampered = audio.to_vec();
    tampered[1] ^= 1;
    assert!(creator.decrypt(202, MediaType::AUDIO, &tampered).is_err());
    assert_eq!(
        creator.decrypt(202, MediaType::AUDIO, &audio).unwrap(),
        encoded
    );
    assert!(trace.trace().contains("KeyPackage 26"));
}

#[test]
fn v4_binary_decode_records_safe_headers_and_local_failures() {
    let trace = songbird::DaveHandshake::default();
    trace.set_enabled(true);
    for frame in [
        vec![25, 1, 2],
        vec![27, 0, 0],
        vec![29, 0, 1, 0],
        vec![30, 0, 1, 0],
    ] {
        assert!(songbird::decode_dave_binary(&frame, &trace).is_ok());
    }
    for frame in [
        vec![],
        vec![27],
        vec![27, 9],
        vec![29, 0],
        vec![30, 0],
        vec![0, 1, 27, 0],
    ] {
        assert!(songbird::decode_dave_binary(&frame, &trace).is_err());
    }
    let exported = trace.trace();
    for opcode in [25, 27, 29, 30] {
        assert!(exported.contains(&format!("BinaryDecoded {opcode}")));
    }
    assert!(exported.contains("SequencedOpcode 27"));
    assert!(exported.contains("BinaryDecodeFailed 1"));
    assert!(exported.contains("BinaryDecodeFailed 2"));
    assert!(exported.contains("BinaryDecodeFailed 3"));
    assert!(!exported.contains("payload") && !exported.contains("data="));
}

#[test]
fn reset_invalidates_old_package_and_fresh_package_can_be_welcomed() {
    let version = NonZeroU16::new(1).unwrap();
    let mut pending = DaveSession::new(version, 202, 303, None).unwrap();
    let old = pending.create_key_package().unwrap();
    let sender = SignatureKeyPair::new(pending.ciphersuite().signature_algorithm()).unwrap();
    let external = ExternalSender::new(
        sender.public().into(),
        BasicCredential::new(b"offline-delivery-fixture".to_vec()).into(),
    )
    .tls_serialize_detached()
    .unwrap();
    pending.set_external_sender(&external).unwrap();
    pending.reinit(version, 202, 303, None).unwrap();
    let fresh = pending.create_key_package().unwrap();
    let make_welcome = |package: &[u8]| {
        let mut creator = DaveSession::new(version, 101, 303, None).unwrap();
        creator.set_external_sender(&external).unwrap();
        let package = KeyPackageIn::tls_deserialize_exact_bytes(package)
            .unwrap()
            .validate(
                OpenMlsRustCrypto::default().crypto(),
                ProtocolVersion::Mls10,
            )
            .unwrap();
        let proposal = ExternalProposal::new_add::<OpenMlsRustCrypto>(
            package,
            creator.group().unwrap().group_id().clone(),
            creator.epoch().unwrap(),
            &sender,
            SenderExtensionIndex::new(0),
        )
        .unwrap()
        .tls_serialize_detached()
        .unwrap();
        let candidate = creator
            .process_proposals(
                ProposalsOperationType::APPEND,
                &VLBytes::new(proposal).tls_serialize_detached().unwrap(),
                Some(&[101, 202]),
            )
            .unwrap()
            .unwrap();
        candidate.welcome.unwrap()
    };
    assert!(pending.process_welcome(&make_welcome(&old)).is_err());
    assert!(!pending.is_ready());
    pending.process_welcome(&make_welcome(&fresh)).unwrap();
    assert!(pending.is_ready());
}

#[test]
fn json_receive_trace_retains_only_opcode_length_and_outcome() {
    let trace = songbird::DaveHandshake::default();
    trace.set_enabled(true);
    assert!(
        songbird::decode_dave_json(r#"{"op":11,"d":{"user_ids":["101","202"]}}"#, &trace).is_ok()
    );
    assert!(
        songbird::decode_dave_json(
            r#"{"op":999,"d":{"token":"synthetic-private-data"}}"#,
            &trace
        )
        .is_err()
    );
    assert!(songbird::decode_dave_json(r#"{"op":6,"d":1234567890123}"#, &trace).is_ok());
    trace.record(songbird::DaveStage::VoiceLoopStarted, 0);
    trace.record(songbird::DaveStage::HeartbeatSending, 0);
    trace.record(songbird::DaveStage::HeartbeatSent, 0);
    trace.record(songbird::DaveStage::HeartbeatAck, 1);
    trace.record(songbird::DaveStage::VoiceEventHandled, 6);
    trace.record(songbird::DaveStage::VoiceLoopStopped, 0);
    let exported = trace.trace();
    assert!(!exported.contains("1234567890123"));
    assert!(exported.contains("JsonDecoded 6"));
    assert!(exported.contains("HeartbeatAck 1"));
    assert!(exported.contains("VoiceLoopStopped 0"));
    assert!(exported.contains("JsonDecoded 11"));
    assert!(exported.contains("JsonDecodeFailed 999"));
    assert!(
        !exported.contains("synthetic-private-data")
            && !exported.contains("user_ids")
            && !exported.contains("token")
    );
}

#[test]
fn optional_json_envelope_fields_do_not_drop_membership_or_heartbeat() {
    let trace = songbird::DaveHandshake::default();
    trace.set_enabled(true);
    let clients = r#"{"s":10,"op":11,"d":{"user_ids":["101","202"]},"extra":{"token":"synthetic-private-data"}}"#;
    assert!(serde_json::from_str::<songbird::model::Event>(clients).is_err());
    assert!(matches!(
        songbird::decode_dave_json(clients, &trace).unwrap(),
        songbird::model::Event::ClientsConnect(_)
    ));
    for heartbeat in [r#"{"op":6,"d":1,"s":11}"#, r#"{"d":1,"op":6,"s":11}"#] {
        assert!(matches!(
            songbird::decode_dave_json(heartbeat, &trace).unwrap(),
            songbird::model::Event::HeartbeatAck(_)
        ));
    }
    assert!(!trace.trace().contains("synthetic-private-data"));
}
