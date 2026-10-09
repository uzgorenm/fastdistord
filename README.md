# fastdistord

Discord voice and text in a small native Rust app. Starts muted.

[Download 0.01 for Apple Silicon Macs](https://github.com/uzgorenm/fastdistord/releases/download/v0.01/Fastdistord-0.01-macos-arm64.dmg) · [Release notes and checksums](https://github.com/uzgorenm/fastdistord/releases/tag/v0.01)

Requires macOS 13 or later. Ad-hoc signed, not notarized. Private repository access required. Windows and Linux downloads are not available yet.

## Use

1. Open the DMG, drag Fastdistord into Applications, then open it.
2. Choose **Connect with QR code**, scan with Discord on your phone, and approve only a login you started. Optional **Remember me** saves access in macOS Keychain.
3. Choose a friend to chat or **Call**. For servers, select a server, then **Chat** or **Voice → Join · channel**. Allow microphone access, wait for encrypted voice readiness, then unmute.

Settings has devices, volume and push-to-talk. **Leave** ends voice, **Quit** exits, and **Log out** removes remembered access. [Controls and troubleshooting](docs/USING.md).

## Limits

Experimental, unofficial personal-account access may break or lead to Discord account restrictions. Fastdistord is not affiliated with Discord.

Two-way audio and hardware recovery still need live testing. Use headphones: echo cancellation, noise suppression and automatic gain control are absent. Video and screen sharing are unfinished. [Verification](docs/VERIFICATION.md) · [Media status](docs/MEDIA.md).

No audio recording or telemetry. [Build from source](docs/GETTING_STARTED.md#build) · [Contribute](CONTRIBUTING.md) · [Packaging](PACKAGING.md).

[MIT](LICENSE). Built with [fastframe](https://github.com/crmne/fastframe). Vendored Songbird retains its [ISC license](vendor/songbird/LICENSE.md) and [patch attribution](vendor/songbird/FASTDISTORD_PATCH.md).
