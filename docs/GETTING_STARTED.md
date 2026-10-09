# Getting started

A native Rust/fastframe Discord voice and text client. macOS Apple Silicon is the primary target. No Electron, webview, bot account, desktop RPC dependency, saved audio recordings, or telemetry.

**Live-call behavior is not yet verified.** Discord's supported personal-user `voice` OAuth scope is restricted to approved partners. No qualifying generally available supported route was found for arbitrary existing server channels. This implementation therefore isolates an **unofficial personal-account adapter**, disabled until explicit risk acceptance in the app. It may violate Discord policy, cause account restrictions, or break without notice. It is not affiliated with Discord. Do not use an important account without understanding that risk.

## What is implemented

- Native resident fastframe shell, tray reopen/quit, account/server/channel selection and live participant events
- Opt-in personal REST/Gateway adapter; bounded signaling queues and heartbeat failure handling
- Songbird Opus/UDP transport plus Davey/OpenMLS DAVE; a small vendored patch refuses unready/non-DAVE audio instead of falling back to transport-only encryption
- CPAL capture/output, established Rubato sample-rate conversion, bounded buffers, multi-speaker receive mixing
- Immediate atomic mute/deafen/PTT gates, epoch invalidation, late pre-send gate, default muted
- Optional configurable local/global shortcuts; visible press-and-hold talk fallback
- Session-only credentials by default; explicitly optional macOS Keychain save, removed on logout
- Audio shutdown on leave/quit/logout, stale join cancellation, safe stop on disconnect/move/permission changes

- Friends and private conversations, individual DM calls, and plain-text guild/DM messaging; bounded history and explicit Send, with mentions and embeds suppressed

Discord video and screen sending are unfinished and are not exposed as working call controls; see [media status](MEDIA.md). League overlay work is stopped.

## Build

Install Rust 1.99.0 and CMake. macOS also needs Xcode command-line tools. All Rust dependencies are locked; fastframe and compatible egui/winit forks are pinned by Git revision.

```sh
cargo build --locked --release
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

Linux development requires ALSA, X11, XKB, Wayland and OpenGL development packages. Example on Debian/Ubuntu:

```sh
sudo apt-get install libasound2-dev libx11-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev cmake pkg-config
```

For an Apple Silicon app bundle, on a Mac:

```sh
rustup target add aarch64-apple-darwin
./scripts/bundle-macos.sh
open dist/latest/Fastdistord.app
```

The script validates the microphone usage description and adds a local ad-hoc signature. It does not notarize, upload, or publish a release. Linux cannot validate Apple hardware, Keychain prompts, microphone permission, or produce a tested Mac app merely by cross-compiling.

## Account and first call

1. Read the unofficial-access risk disclosure. No official desktop/web client needs to be running for this adapter.
2. Read the short account-access disclosure and choose **Connect with QR code**. Scan the code using Discord on your phone and approve the login there. Only approve a code you started yourself. An expired/canceled login needs a fresh Connect.
3. Leave **Remember me** unchecked for session-only access. Optional saving happens after successful account validation. **Connect from Keychain** reads a credential you previously chose to save in this app. Existing local session entry remains available under its disclosure; never extract another app’s credentials or put tokens in chat or commands.
4. Choose **Servers**, select a server, then click **Join · channel**. Or choose a friend and click **Call** in the conversation. **This action authorizes opening your selected audio devices and joining that specific channel.** The OS may request microphone permission.
5. Transmission starts muted. Use headphones; select your intended mode and unmute. Enabling PTT does not itself unmute. Deafen also blocks outgoing audio.
6. Leave releases capture. Closing the window keeps a call running when the tray is available; otherwise it quits. Use the tray to reopen or Quit to exit.

Server mute/deafen/suppression is respected. A server move, channel change or uncertain access stops audio rather than automatically undoing the change. Select/rejoin explicitly after checking access. Device changes apply at the next Join. Transient network/device failures use bounded recovery of the same authorized session and pinned devices; mute is preserved and PTT is reset. Invalid/ambiguous sessions, server revocation and exhausted retries require explicit reconnect/rejoin. See [Recovery boundaries](USING.md#recovery-boundaries).

### Login and account sync

Discord’s standard OAuth scopes can identify an account and list basic guild information, but its `voice` scope requires approved-partner access. That sign-in cannot authorize this adapter. No supported export or synchronization of the installed Discord client’s session credential is provided. Fresh QR login instead asks for mobile approval of broad personal-account access. Remote interoperability remains unverified until the user tests it.

## Limits

Optional RNNoise suppression and conservative automatic gain are off by default. Echo cancellation is unavailable, so speakerphone use may echo. Bluetooth routes and sleep/wake require hardware testing. No claim of production-grade background PTT safety: platform key-release reliability still needs live verification; focus/visibility transitions close the gate. Native Wayland global shortcuts are unavailable with the selected backend and use the visible hold-to-talk fallback.

Initial guild voice snapshots and later updates seed the roster, but this adapter does not yet implement every undocumented personal Gateway payload; missing names may display a user ID. Stage channels are excluded. Large servers are bounded to protect memory. Permission/channel updates conservatively stop a call. Not every browser/client capability, account challenge or protocol variation is supported.

Do not treat local tests, a loopback, UDP readiness or a successfully compiled binary as a successful Discord call. See [verification](VERIFICATION.md) and [manual acceptance](LIVE_TEST.md).

## Architecture and licenses

`ui/` is presentation and platform shortcuts; `account.rs`, `runtime.rs`, `transport.rs` handle authorization/signaling/lifecycle; `audio.rs` handles local audio. Networking, codecs and device work run away from UI rendering. Audio callbacks use bounded preallocated queues and atomic controls, with no file I/O or network calls.

MIT project; vendored Songbird retains its ISC license and modification notes. Fastframe/egui/winit, CPAL, Rubato, Opus, Davey and OpenMLS retain their own upstream licenses. No codec or cryptography is reimplemented.

References: [fastframe](https://github.com/crmne/fastframe), [Discord OAuth scopes](https://docs.discord.com/developers/topics/oauth2), [voice/DAVE protocol](https://docs.discord.com/developers/topics/voice-connections), [Songbird](https://github.com/serenity-rs/songbird), [official libdave](https://github.com/discord/libdave), [Davey](https://github.com/Snazzah/davey).
