# Verification report — 2026-10-07

## Passed locally in the cloud

Environment: x86_64 Debian 13, Rust 1.99.0, official native development packages extracted to a workspace-only sysroot. No user Mac was used for implementation or tests.

- `cargo fmt --all -- --check`
- `cargo test --locked`: **38 passed**, 0 failed
- `cargo clippy --locked --all-targets -- -D warnings`: passed
- `cargo build --locked --release`: passed; final link/build36.87 seconds with dependencies already compiled
- Five standalone production DAVE policy tests: passed
- Native offline GUI smoke: startup, Settings, normal960×660/minimum740×540 resizing, scrolling to Quit and normal window-close clean exit
- Independent source review rechecked identified lifecycle/privacy problems after fixes

Coverage includes audio mute/PTT/unknown state, queued-sample epochs, stale session revocation, bounded mixing, synthetic duplex resampling, old-device failure/cleanup isolation, credential-safe diagnostics, voice endpoint validation, canceled joins, initial roster updates, permission-event scoping and concurrent PTT publish/focus-clear. DAVE policy tests cover unready/protocol-zero/plaintext refusal and rekey-generation invalidation. They are **not** full handshake, MLS cryptographic or real-network integration tests.

## Measured disconnected release build

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

This cloud is Linux. No runnable macOS `.app`, native Mac permission validation, Keychain round trip, Mac audio test or Mac performance benchmark was produced. The source includes reproducible build steps and native CI/bundle validation, not a claim that those hardware tests passed. CI results on the remote commit must be checked separately after push.

## Remaining high-impact limitations

- Undocumented personal-account protocol may break or be rejected; do not promise Discord compatibility.
- No AEC, noise suppression or AGC; use headphones.
- Bluetooth, sleep/wake, packet loss, sustained multiuser DAVE and background PTT need real hardware/live verification.
- Device changes intentionally require leave/rejoin. Network reconnect is explicit, with no automatic microphone resumption.
- Native Wayland lacks the selected global shortcut backend; the visible hold control is the fallback.
- No signed/notarized download or release was published.
