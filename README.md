# fastdistord

Discord voice and text in a small native Rust app.

Browse friends and servers, read and send messages, and join voice calls with DAVE encryption, mute, deafen and push-to-talk. The app starts muted. Closing the window keeps a call running when the tray is available; Quit ends it.

## Download

[Download 0.01 for Apple Silicon Macs](https://github.com/uzgorenm/fastdistord/releases/download/v0.01/Fastdistord-0.01-macos-arm64.dmg) · [Release notes and checksums](https://github.com/uzgorenm/fastdistord/releases/tag/v0.01)

Requires macOS 13 or later. Open the DMG and drag Fastdistord into Applications. This experimental build is ad-hoc signed and not notarized. Repository access is required while the project is private. Windows and Linux downloads are not available yet.

## Get started

1. Open Fastdistord and choose **Connect with QR code**. Scan with Discord on your phone and approve only a login you started here. **Remember me** saves access in macOS Keychain for future launches; otherwise it stays in memory.
2. Choose a friend to chat, or **Servers → server → Chat** for text channels. **Send** sends plain text; Enter adds a line.
3. For voice, choose **Call** in a friend's conversation or **Voice → Join · channel** in a server. Allow microphone access if prompted, wait for encrypted voice readiness, then unmute.
4. Settings has device selection, volume and push-to-talk. When enabled and unmuted, hold **Ctrl+Shift+Space** or **Hold to talk**. **Leave** releases audio devices; **Log out** also removes remembered access.

See [Controls and troubleshooting](docs/USING.md) for help.

## Limits

Personal-account access is unofficial. It may break or lead to Discord account restrictions. Fastdistord is not affiliated with Discord.

Incoming audio, microphone quality, two-way calls and hardware recovery still need live testing. Use headphones: echo cancellation, noise suppression and automatic gain control are not implemented. Video and screen sharing are unfinished. Text supports the latest 50 messages and plain-text sending; attachments, threads, reactions and edits are not supported.

The app does not record audio or collect telemetry. See [verification](docs/VERIFICATION.md) and [media status](docs/MEDIA.md) for what has been tested.

## Build and contribute

Install Rust 1.99.0, CMake and the [platform prerequisites](docs/GETTING_STARTED.md#build), then:

```sh
cargo build --locked --release
```

Run `target/release/fastdistord` (`target\release\fastdistord.exe` on Windows). For a Mac app bundle, see [Packaging](PACKAGING.md). [Contributing](CONTRIBUTING.md) covers local checks and bug reports. Keep credentials and private account details out of reports.

## License

[MIT](LICENSE). Built with [fastframe](https://github.com/crmne/fastframe), egui/winit, CPAL, Rubato, Songbird/Opus and Davey/OpenMLS. Vendored Songbird retains its [ISC license](vendor/songbird/LICENSE.md) and [patch attribution](vendor/songbird/FASTDISTORD_PATCH.md).
