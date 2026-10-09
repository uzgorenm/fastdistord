# fastdistord

Discord voice and text in a small native Rust app. Starts muted.

[Mac Apple Silicon 0.02](https://github.com/uzgorenm/fastdistord/releases/download/v0.02/Fastdistord-0.02-macos-arm64.dmg) · [Windows x64 0.02](https://github.com/uzgorenm/fastdistord/releases/download/v0.02/Fastdistord-0.02-windows-x64-setup.exe) · [Release notes and checksums](https://github.com/uzgorenm/fastdistord/releases/tag/v0.02)

Mac requires macOS 13 or later; ad-hoc signed, not notarized. The Windows installer is unsigned and cross-compiled; Windows runtime testing is pending. Linux downloads are not included.

Current source includes improvements beyond the published 0.02 installers. See [verification and live-test limits](docs/VERIFICATION.md).

## Use

1. On Mac, open the DMG, drag Fastdistord into Applications, then open it. On Windows, run the installer and open Fastdistord from Start.
2. Choose **Connect with QR code**, scan with Discord on your phone, and approve only a login you started. Optional **Remember me** saves access in macOS Keychain.
3. Choose a friend to chat or **Call**. For servers, select a server, then **Chat** or **Voice → Join · channel**. Allow microphone access, wait for encrypted voice readiness, then unmute.

Settings has devices, a local microphone test, optional noise suppression and gain control, participant volume, shortcuts and appearance. **Leave** ends voice, **Quit** exits, and **Log out** removes remembered access. [Controls and troubleshooting](docs/USING.md).

## Limits

Experimental, unofficial personal-account access may break or lead to Discord account restrictions. Fastdistord is not affiliated with Discord.

Two-way audio and hardware recovery still need live testing. Use headphones: echo cancellation is unavailable. Optional RNNoise suppression and automatic gain are off by default. Video and screen sharing are unfinished. [Verification](docs/VERIFICATION.md) · [Media status](docs/MEDIA.md).

No saved audio recordings or telemetry. The explicit microphone test keeps at most five seconds in memory for local playback. [Build from source](docs/GETTING_STARTED.md#build) · [Contribute](CONTRIBUTING.md) · [Packaging](PACKAGING.md).

[MIT](LICENSE). Built with [fastframe](https://github.com/crmne/fastframe). Vendored Songbird retains its [ISC license](vendor/songbird/LICENSE.md) and [patch attribution](vendor/songbird/FASTDISTORD_PATCH.md).
