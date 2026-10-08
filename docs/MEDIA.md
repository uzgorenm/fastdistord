# Local media and Discord video status

The Mac app has a **Local preview** tab for a selected camera or window. It does not send video to Discord. Preview works without signing in. Camera permission is requested only through its explicit button; starting the camera is a separate action. Window selection uses Apple's ScreenCaptureKit picker on macOS 14 or later, with screen audio disabled. Stop, changing tabs, hiding the window, logout and quit release capture and preview frames.

Capture retains one bounded BGRA frame in memory. It does not record media. Physical camera/window preview and permission behavior have not been verified on this Mac because the app-opening computer-use request was canceled. Open the local bundle manually to perform those checks with safe content. League interactions and overlay work are stopped.

## Implemented and verified offline

- AVFoundation camera and ScreenCaptureKit window capture adapters compile on Apple Silicon; actual capture remains untested.
- VideoToolbox H.264 encoding/decoding, bounded AVCC/Annex B conversion and synthetic checkerboard validation passed a headless native test. Sessions currently encode standalone keyframes; sustained real-time performance is not established.
- Codec-aware Davey encryption passed real two-participant MLS tests for H.264 and VP8, in both directions, with tamper/replay rejection and participant removal.
- Bounded RFC 6184 single-NAL/FU-A RTP packetization and ordered reassembly preserve the encrypted frame and reject stale epochs and discontinuities. These are offline helpers with caller-supplied metadata.
- The composed native test passed: generated frame → H.264 → DAVE → RTP → reassembly → DAVE decryption → native decoding → pixel-fidelity check. It captured no camera/screen content and used no Discord connection.

Run deterministic tests with `cargo test --locked`. On an authorized Mac, the optional hardware-dependent synthetic check is `cargo test --locked --test video_dave native_h264_dave_rtp_decode_roundtrip -- --ignored --nocapture`. The packaged executable also accepts `--check-native-video` for a synthetic codec check without opening the GUI or capture devices.

## Remaining Discord pipeline

No video stream/codec/SSRC negotiation, transport AEAD, UDP video sending, live receive/playback or Go Live integration is implemented. RTP helpers have no sockets, extensions, retransmission or jitter buffer. The native preview and offline encrypted pipeline are separate; captured frames are not fed into a sender.

Keep the order **encoded frame → DAVE → RTP → transport encryption**. Songbird's current driver is for audio. Do not invent video endpoints or reuse voice SSRCs. Public voice documentation does not provide a complete personal-account Go Live implementation.

Real two-way voice remains the first live acceptance gate: microphone/playback, DAVE membership changes, leave/rejoin and recovery. Live audio, text, camera or screen tests require local credential handoff, a target channel, second participant and approved content. Persistent credential storage requires separate approval.

## References

- [Discord voice negotiation](https://docs.discord.com/developers/topics/voice-connections), [DAVE protocol](https://daveprotocol.com/) and [official libdave](https://github.com/discord/libdave)
- [Davey 0.1.4](https://docs.rs/davey/0.1.4/davey/) and pinned local source
- [RFC 6184 H.264 RTP](https://www.rfc-editor.org/rfc/rfc6184)
- Apple [AVFoundation](https://developer.apple.com/documentation/avfoundation), [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit) and [VideoToolbox](https://developer.apple.com/documentation/videotoolbox)

Davey, OpenMLS, Songbird and objc2 bindings retain their upstream licenses. This project does not implement cryptography or a video codec.
