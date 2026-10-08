# Video and screen sharing

Camera video and screen sharing are unfinished. The desktop app does not expose a Start camera or Share screen action. No camera/screen permission is requested, and no video is transmitted.

`src/video.rs` is the first offline component of a separate media pipeline. It accepts bounded encoded H.264 Annex B or VP8 frames, delegates codec-aware video encryption to pinned Davey 0.1.4, rejects unready sessions and plaintext output, and attaches an encryption epoch to each encrypted frame. A later packet sender must check current authority under the session owner's lock; changing epochs invalidates queued frames and fragments. This module is not connected to Songbird's negotiated session yet.

Tests cover size/format rejection, refusal by a real unnegotiated Davey session and revocation of prepared frames. They do not prove a successful video handshake, encrypted-video round trip, native capture, playback or Discord interoperability.

## Remaining pipeline

| Stage | Status |
|---|---|
| AVFoundation camera capture | Candidate; not implemented or tested |
| ScreenCaptureKit source picker/capture | Candidate; not implemented or tested |
| VideoToolbox hardware encoding and AVCC-to-Annex-B conversion | Candidate; not implemented or tested |
| Encoded frame to codec-aware DAVE | Offline component implemented; successful negotiated encryption untested |
| Codec, stream and SSRC negotiation | Not implemented |
| Encrypted frame to RTP, transport AEAD and UDP | Not implemented; never send raw DAVE bytes directly |
| Receive, authenticate, reassemble, decrypt, decode and display | Not implemented |
| Camera/share stop, session revocation and permission denial | Must close capture and queued-send authority before cleanup |

Keep the order **encoded frame → DAVE → RTP packetization → transport encryption**. The current Songbird driver is an audio implementation, not a video/Go Live transport. Do not invent stream endpoints or reuse voice SSRCs as video SSRCs. A negotiated video session and its membership transitions must be verified before introducing live transmission. The public voice guide describes voice negotiation and DAVE but does not supply a complete personal-account Go Live implementation.

Prove the existing two-way voice call first. Then exercise synthetic native video encoding offline, followed by local preview of a user-selected camera or test window. Live camera/screen tests require a specific channel, participant and approved content. Session-only credential entry stays local; persistent storage requires separate approval. League overlay work is stopped.

## References

- [Discord voice negotiation and DAVE](https://docs.discord.com/developers/topics/voice-connections)
- [DAVE protocol whitepaper](https://daveprotocol.com/)
- [Official libdave implementation](https://github.com/discord/libdave)
- [Davey 0.1.4](https://docs.rs/davey/0.1.4/davey/) and its pinned local source
- Apple API candidates: [AVFoundation](https://developer.apple.com/documentation/avfoundation), [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit), [VideoToolbox](https://developer.apple.com/documentation/videotoolbox)

Davey, OpenMLS and Songbird retain their upstream licenses. This project does not implement cryptography or a video codec.
