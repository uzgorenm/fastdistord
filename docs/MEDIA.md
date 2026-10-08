# Local media and Discord video status

The Mac app has a **Local preview** tab for a selected camera or window. It does not send video to Discord. Preview works without signing in. Camera permission is requested only through its explicit button; starting the camera is a separate action. Window selection uses Apple's ScreenCaptureKit picker on macOS 14 or later, with screen audio disabled. Stop, changing tabs, hiding/minimizing/occluding the window, logout and quit release capture and preview frames.

Capture retains one bounded BGRA frame in memory. It does not record media. Physical camera/window preview and permission behavior have not been verified on this Mac because the app-opening computer-use request was canceled. Open the local bundle manually to perform those checks with safe content. League interactions and overlay work are stopped.

## Implemented and verified offline

- AVFoundation camera and ScreenCaptureKit window capture adapters compile on Apple Silicon; actual capture remains untested.
- VideoToolbox H.264 encoding/decoding, bounded AVCC/Annex B conversion and synthetic checkerboard validation passed a headless native test. Sessions currently encode standalone keyframes; sustained real-time performance is not established.
- Codec-aware Davey encryption passed real two-participant MLS tests for H.264 and VP8, in both directions, with tamper/replay rejection and participant removal.
- Bounded RFC 6184 single-NAL/FU-A RTP packetization and ordered reassembly preserve the encrypted frame and reject stale epochs and discontinuities. These are offline helpers with caller-supplied metadata.
- Transport AES256-GCM and XChaCha20-Poly1305 RTP-size modes authenticate headers and payload, burn each nonce once, refuse wraparound and maintain a bounded authenticated replay window. Only typed DAVE-encrypted video packets enter the public sender. Expanded AES/GHASH state uses upstream zeroize-on-drop features. Fixed RTP headers only; unsupported extensions, CSRCs and padding fail closed.
- A synthetic DAVE/RTP/AEAD round trip passed over two loopback UDP sockets. No Discord or external destination was used.
- The composed native test passed: generated frame → H.264 → DAVE → RTP → transport AEAD → authentication → reassembly → DAVE decryption → native decoding → pixel-fidelity check. It captured no camera/screen content and used no Discord connection.

Run deterministic tests with `cargo test --locked`. On an authorized Mac, the optional hardware-dependent synthetic check is `cargo test --locked --test video_dave native_h264_dave_rtp_decode_roundtrip -- --ignored --nocapture`. The packaged executable also accepts `--check-native-video` for a synthetic codec check without opening the GUI or capture devices.

## Remaining Discord pipeline

No Discord video stream/codec/SSRC negotiation, UDP video sending, live receive/playback or Go Live integration is implemented. RTP/AEAD helpers have no sockets, extensions, retransmission or jitter buffer. The native preview and offline encrypted pipeline are separate; captured frames are not fed into a sender.

Keep the order **encoded frame → DAVE → RTP → transport encryption**. Songbird's current driver is for audio. Do not invent video endpoints or reuse voice SSRCs. Public voice documentation does not provide a complete personal-account Go Live implementation.

## Negotiation facts still needed

The current [experimental video client's connection source](https://github.com/Discord-RE/Discord-video-stream/blob/master/src/client/voice/BaseMediaConnection.ts), inspected on 2026-10-08, uses WebRTC/SDP and reads stream SSRCs from Ready. It is an experimental protocol observation, not official Discord documentation or proof of compatibility. Our pinned Songbird models currently retain only the audio Ready SSRC and transport key/mode/DAVE version; they do not retain video codec or stream negotiation.

Before a live sender can be connected, confirm the selected account/session's accepted protocol (UDP video versus WebRTC), video/RTX SSRC assignment, payload type and H.264 profile/packetization mode, required RTP extensions, stream activation and server acknowledgements. A WebRTC path also needs verified SDP/ICE/DTLS handling. Go Live needs its separately supplied stream server/session and separate DAVE group; it cannot reuse the main call's group by assumption.

If sharing the audio UDP connection, there must be one exclusive transport nonce allocator for every audio/video SSRC under its transport key. The new standalone sender must **never** be initialized with Songbird's active key alongside Songbird's own counter. It retains its counter during its lifetime, closes permanently on revocation, and must not be reconstructed with an old key after reconnect or an MLS transition. Connecting it requires a shared connection owner that preserves counters while changing frame authority, or a freshly negotiated independent key. Final UDP commits must recheck authority under that owner's lock. No such live connection owner is claimed implemented.

Real two-way voice remains the first live acceptance gate: microphone/playback, DAVE membership changes, leave/rejoin and recovery. Live audio, text, camera or screen tests require local credential handoff, a target channel, second participant and approved content. Persistent credential storage requires separate approval.

## References

- [Discord voice negotiation](https://docs.discord.com/developers/topics/voice-connections), [DAVE protocol](https://daveprotocol.com/) and [official libdave](https://github.com/discord/libdave)
- [Davey 0.1.4](https://docs.rs/davey/0.1.4/davey/) and pinned local source
- [RFC 6184 H.264 RTP](https://www.rfc-editor.org/rfc/rfc6184)
- Apple [AVFoundation](https://developer.apple.com/documentation/avfoundation), [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit) and [VideoToolbox](https://developer.apple.com/documentation/videotoolbox)

Davey, OpenMLS, Songbird, RustCrypto AEAD crates and objc2 bindings retain their upstream licenses. This project does not implement cryptography or a video codec.
