# fastdistord

**Discord voice and text in a small native app.** Built with Rust and fastframe, with a focused interface for channels, calls and messages.

[Get started](docs/GETTING_STARTED.md) · [Controls and help](docs/USING.md) · [Contribute](CONTRIBUTING.md)

The sign-in screen offers fresh QR login with mobile approval or explicit macOS Keychain reconnect. Use **Friends | Servers** in one sidebar. Friends open conversations; server channels appear beneath the selected server. Click the selected server to collapse or expand its channels. Friends follow recent DM activity; profile controls stay visible while you browse.

## Download and install

Version **0.01** is being verified and has not been published as a GitHub Release yet. Hosted download links will be added after the actual packages are available. For now, [build from source](#build-from-source).

Packages: Mac Apple Silicon DMG, Windows x64 EXE installer, and a Linux amd64 Debian package for Ubuntu 24.04 or compatible systems. Mac/Windows packages are not notarized/vendor-signed.

## Features

- Browse friends, private conversations and server channels in one sidebar
- Start individual friend calls, with participant and speaking indicators
- Two-way voice with DAVE encryption, mute, deafen and push-to-talk
- Choose microphones and speakers, check input levels and adjust output volume
- Stay in a call from the tray where supported, with bounded device/session recovery
- Read the latest 50 channel or DM messages and send plain text; new messages arrive through the Gateway
- Connect with a fresh QR code and mobile approval; optional macOS Keychain storage

The app starts muted and does not record audio or collect telemetry.

## Open and use

1. Launch Fastdistord. You can explore the offline interface without credentials.
2. Read the account-access sentence, then choose **Connect with QR code**.
3. Scan the fresh code with Discord on your phone and approve there. Only approve a login you started here. Keychain storage is optional; otherwise the session stays in memory.
4. Choose **Servers**, then a server and **Join · channel**. Allow microphone access if prompted, confirm encrypted voice readiness and unmute when ready.
5. Choose a friend to chat; **Call** starts an individual voice call. Enable push-to-talk in Settings, then hold **Ctrl+Shift+Space** or **Hold to talk** while unmuted.
6. **Disconnect** releases audio devices. Closing the window may keep an active call running; **Quit** ends it.

See the [first-call walkthrough](docs/GETTING_STARTED.md#account-and-first-call) and [controls and troubleshooting](docs/USING.md).

## Current limitations

Personal-account access uses an unofficial integration that may break or lead to Discord account restrictions. Fastdistord is independent and is not affiliated with Discord.

Voice, friend calls and messaging still need live testing. Video and screen-share sending are unfinished. Use headphones: echo cancellation, noise suppression and automatic gain control are not implemented.

See [verification status](docs/VERIFICATION.md), [media status](docs/MEDIA.md) and the [live-test checklist](docs/LIVE_TEST.md) for details.

## Build from source

Install Rust 1.99.0, CMake and the [platform prerequisites](docs/GETTING_STARTED.md#build), then:

```sh
cargo build --locked --release
./target/release/fastdistord
```

On Windows, the built executable is `target\release\fastdistord.exe`.

To create and open an Apple Silicon app bundle, run on macOS with Xcode command-line tools:

```sh
rustup target add aarch64-apple-darwin
./scripts/bundle-macos.sh
open dist/Fastdistord.app
```

See [Packaging](PACKAGING.md) for bundle details.

## Help and contributing

Start with [Controls and help](docs/USING.md). When reporting a problem, include your OS, app commit, steps and what happened. Keep credentials and private account details out of reports and screenshots.

[Contributing](CONTRIBUTING.md) covers development checks and useful bug reports.

## Acknowledgements and license

Built on [fastframe](https://github.com/crmne/fastframe), egui/winit, CPAL, Rubato, Songbird/Opus and Davey/OpenMLS.

Licensed under [MIT](LICENSE). The vendored Songbird patch retains its [ISC license](vendor/songbird/LICENSE.md) and [modification notes](vendor/songbird/FASTDISTORD_PATCH.md).
