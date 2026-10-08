# fastdistord

**Discord voice and text in a small native app.** Built with Rust and fastframe, with a focused interface for channels, calls and messages.

[Get started](docs/GETTING_STARTED.md) · [Controls and help](docs/USING.md) · [Contribute](CONTRIBUTING.md)

![Native Linux app, shown offline without an account connected](docs/images/offline-startup.png)

## Download and install

Installers for the first release, **0.01**, are being prepared:

- **macOS Apple Silicon:** DMG
- **Windows:** EXE installer
- **Linux:** native package

These installers have not been published yet. Download links will be added here when the private GitHub Release is available. For now, you can [build from source](#build-from-source). Existing Mac app bundles use a local ad-hoc signature and are not notarized.

## Features

- Browse servers and voice channels, with participants and speaking indicators
- Two-way voice with DAVE encryption, mute, deafen and push-to-talk
- Choose microphones and speakers, check input levels and adjust output volume
- Stay in a call from the tray where supported, with bounded device/session recovery
- Read the latest 50 text-channel messages, refresh and send plain-text messages
- Preview a camera or selected window locally on Mac, with explicit Start and Stop controls
- Enter credentials in a local masked field, with optional macOS Keychain storage

The app starts muted and does not record audio or collect telemetry.

## Open and use

1. Launch Fastdistord. You can explore the offline interface without credentials.
2. If you choose to connect, read and accept the unofficial-access disclosure in the app.
3. Enter your credential only in the local masked field. Never send tokens in chat or bug reports, or extract them from another application. Keychain storage is optional.
4. Choose a server and voice channel, then select **Join**. Allow microphone access if prompted, confirm encrypted voice readiness and unmute when ready.
5. For push-to-talk, hold **Ctrl+Shift+Space** or the **Talk** button. Select a text channel to read, refresh or send messages.
6. **Leave** releases audio devices. Closing the window may keep an active call running; **Quit** ends it.

See the [first-call walkthrough](docs/GETTING_STARTED.md#account-and-first-call) and [controls and troubleshooting](docs/USING.md).

## Current limitations

Personal-account access uses an unofficial integration that may break or lead to Discord account restrictions. Fastdistord is independent and is not affiliated with Discord.

Voice and text are implemented but have not been verified in a live Discord session. Mac camera/window preview still needs physical-device testing. Discord video and screen-share signaling is unfinished, so local previews do not send media to a call. Use headphones: echo cancellation, noise suppression and automatic gain control are not implemented.

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
