# Native Mac continuation, 2026-10-08

The Mac checkout began at `bb9536ba5b2356e81ba59bdb29e6184d10a0eb6e`. The interrupted account/messaging edits were preserved and integrated with text-channel selection, manual history refresh and explicit plain-text Send. Pending history requests are canceled on scope changes; stale results cannot replace the current account/channel snapshot. A regression test checks that scope changes erase displayed text and cancel pending reads without altering voice mute state.

Native Apple Silicon Rust 1.99.0 checks passed: format, strict Clippy, 77 deterministic tests and six standalone Songbird DAVE policy tests. Three hardware/socket-dependent tests are ignored by default. The composed headless native test was run separately and passed: synthetic H.264 encoding, real two-participant DAVE encryption, RTP fragmentation, transport AEAD/authentication, reassembly, DAVE decryption, VideoToolbox decoding and checkerboard pixel fidelity. Tests also cover tamper/replay rejection, participant removal and stale fragment authority. Both transport modes passed authentication/replay tests, including nonce exhaustion and permanent revocation. A synthetic DAVE/RTP/AEAD loopback UDP test passed separately. These are local fixtures, not Discord interoperability or live membership verification.

A local release build and `scripts/bundle-macos.sh` succeeded. `plutil` and strict deep ad-hoc signature verification passed for `dist/Fastdistord.app`. The bundle includes microphone and camera usage descriptions; the screen picker requires macOS 14 at runtime. A cached build-script warning reported debug-info stripping unavailable; compilation and signing still succeeded. The bundle is not Developer ID signed or notarized.

The exact computer-use action `cua.getApp('/Users/mehmetuzgoren/Desktop/Side Projects/fastdistord/dist/Fastdistord.app')` returned **aborted by user** after 3121.2 seconds. It was not retried. App startup was not confirmed; this was not a macOS permission-denial result. The bundle remains available for manual opening. No GUI screenshot, packaged microphone/camera permission, physical device preview/round trip, background call, PTT hardware result or Mac resource measurement is claimed.

The app now exposes explicit local camera/window preview. Capture adapters compile, but no camera/screen content was captured or transmitted. The offline RTP/AEAD helpers do not implement Discord negotiation or network video. See [media status](MEDIA.md).

No credential was entered and no account/channel session or outgoing text/audio/video test was performed. Two-way voice, live DAVE membership changes, leave/rejoin and recovery require user credential handoff, target channel, second participant and approved test content. League overlay work and League interactions are stopped. No public release was published.

Focused source review fixed editor undo retention across text scope changes/logout, clearing drafts even while the preview tab is open, stopping preview on minimize/occlusion, and clearing its texture after native capture stops. RustCrypto zeroization features now erase expanded AES/GHASH state on drop. The editor regression test passed; physical window/capture behavior still needs manual verification.

The report below records earlier cloud checks, not native hardware acceptance.

# Verification report — 2026-10-07

## Passed locally in the cloud

Environment: x86_64 Debian 13, Rust 1.99.0, official native development packages extracted to a workspace-only sysroot. No user Mac was used for implementation or tests.

- `cargo fmt --all -- --check`
- `cargo test --locked`: **54 passed**, 0 failed
- `cargo clippy --locked --all-targets -- -D warnings`: passed
- `cargo build --locked --release`: passed; baseline final link/build36.87 seconds with dependencies already compiled; recovery rebuild checked separately
- Six standalone production DAVE/retry policy tests: passed
- Native offline GUI smoke: startup, Settings, normal960×660/minimum740×540 resizing, scrolling to Quit and normal window-close clean exit
- Independent source review rechecked identified lifecycle/privacy problems after fixes

Coverage includes audio mute/PTT/unknown state, queued-sample epochs, stale session revocation, bounded mixing, synthetic duplex resampling, old-device failure/cleanup isolation, credential-safe diagnostics, voice endpoint validation, canceled joins, initial roster updates, permission-event scoping and concurrent PTT publish/focus-clear. DAVE policy tests cover unready/protocol-zero/plaintext refusal and rekey-generation invalidation. They are **not** full handshake, MLS cryptographic or real-network integration tests.

## Measured disconnected baseline release build (26161d9)

Cloud Linux Xvfb with Mesa software rendering, no account connected and no microphone opened:

| Observation | Result |
|---|---:|
| Executable size |28,494,232 bytes (27.17 MiB)|
| Process launch to viewable window |0.6661 seconds|
| Resident memory |117,152 KiB (114.4 MiB)|
| Process CPU ticks over5.0001 seconds |0, at100 ticks/second resolution|
| Threads |35|
| Normal close |Exit code0|

Startup was polled every100ms; disk/cache state was uncontrolled. This is not a cold-start or sustained performance benchmark. Zero measured ticks over this short interval does not establish a universal CPU guarantee. The observed cloud RSS is above the provisional100MB idle goal. Software rendering and Linux runtime costs cannot predict the target Mac result.

No connected-silent, active call, hidden active-call, multiuser or official-client comparison measurements were performed. The <150MB call memory goal and target-Mac idle CPU goal remain unverified.

## Implemented, not live-verified

- Unofficial personal-account identity/guild/channel REST and Gateway signaling
- Existing-user voice join/leave/switch and initial voice-state roster handling
- Songbird/Opus transport plus Davey/OpenMLS DAVE negotiation/membership paths
- Explicit fail-closed DAVE/media-generation and late packet privacy guards
- CPAL device pipeline, microphone permissions, hardware recovery, global shortcuts
- macOS Keychain and ad-hoc-signed `.app` packaging script
- Tray-resident calls with window hidden; server mute/deafen/move/kick behavior

## Blocked acceptance

No Discord account session, real microphone, second participant or live voice channel was used. Supported personal `voice` OAuth access remains restricted to approved partners. The experimental adapter needs explicit informed opt-in, a credential entered locally, a concrete authorized channel/session and a real second participant before live acceptance can be attempted. It may fail or expose the account to policy risk.

This cloud is Linux. The baseline private CI run built and validated a macOS `.app` bundle. It did not retain that bundle as a download. Native Mac microphone permission, Keychain round trip, audio and performance tests were not performed. The source includes reproducible build steps and native CI/bundle validation, not a claim that those hardware tests passed. CI results on the remote commit must be checked separately after push.

## Remaining high-impact limitations

- Undocumented personal-account protocol may break or be rejected; do not promise Discord compatibility.
- No AEC, noise suppression or AGC; use headphones.
- Bluetooth, sleep/wake, packet loss, sustained multiuser DAVE and background PTT need real hardware/live verification.
- Selecting different devices intentionally requires an explicit new Join. Same-device failures and resumable transient signaling losses now have bounded automatic recovery; invalid sessions, ambiguous disconnects, server revocation and exhausted retries require explicit action.
- Native Wayland lacks the selected global shortcut backend; the visible hold control is the fallback.
- No signed/notarized download or release was published.

## Recovery delta

The recovery implementation adds five-attempt1/2/4/8/16-second backoff, a60-second stable-success budget reset, exact session-generation checks and pinned device IDs. Signaling uses Discord Gateway RESUME with the supplied validated Discord resume endpoint; it never automatically falls back to fresh identification. Replayed kick/move/permission events invalidate the voice cache before RESUMED permits restoration. Voice recovery reuses only the same authorized guild/channel/session credentials; it sends no automatic channel-join command. Device recovery reopens only the same logical microphone/speaker IDs, preserving current volume and explicit mute; PTT resets to unknown/released.

New local tests cover backoff exhaustion/stability, canceled retry generations, revoked/session-changed voice authority, missing device pins and resume-endpoint/close-code restrictions. These are offline tests of production decision helpers. No real network interruption, unplug, default-route switch, Bluetooth or sleep/wake recovery was exercised. Automatic recovery is implemented, but hardware/live interoperability remains unverified. OS-managed virtual devices may change their physical backing outside the app.

Protocol reference: [Discord Gateway resume lifecycle](https://docs.discord.com/developers/events/gateway#resuming). Its documented transport rules do not make this personal-account adapter an approved integration.

Baseline CI succeeded on Ubuntu24.04 and macOS14 for remote commit1666044c43a0e20b44fcf2c3f91941200cabc3ea, including tests/release and macOS bundle validation: [verified run](https://github.com/uzgorenm/fastdistord/actions/runs/37679324707). Recovery-delta CI and the newly added private build-artifact upload steps require separate verification after push. No download is claimed ready before its run completes.
