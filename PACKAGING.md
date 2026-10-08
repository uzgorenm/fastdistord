# Packaging Fastdistord 0.01

The public-facing version is **0.01**, tag **v0.01**; Cargo and native package metadata use SemVer **0.0.1**. Releases stay in the private repository. Publishing does not merge PR #1 or change repository visibility.

## Packages

- Mac Apple Silicon: `scripts/bundle-macos.sh`, then `scripts/package-macos.sh`. The DMG includes Fastdistord.app, an Applications link and the MIT license. The app is ad-hoc signed, not notarized or Developer ID signed.
- Windows x64: NSIS 3.11 compiles `packaging/windows/installer.nsi`. It installs for the current user, creates Start menu shortcuts and includes an uninstaller. No administrator access or automatic app launch. Not Authenticode signed.
- Linux amd64: `scripts/package-linux.sh` creates a `.deb` with desktop entry and license. Runtime dependencies are derived from the actual executable, with required dynamically loaded GUI libraries included. Built on Ubuntu 24.04; compatible Debian/Ubuntu distributions only.

Build with Rust 1.99.0, CMake and the prerequisites in [Getting started](docs/GETTING_STARTED.md). Installers include upstream dependency/source notices, font licensing and Songbird patch attribution. All dependencies are locked. Native tools/compiler versions can still affect output; this is not a bit-for-bit reproducibility claim.

## Release verification

The Check workflow runs format, strict Clippy, tests, Songbird policy checks and release builds on all three platforms. It verifies the mounted DMG, payload hashes, version startup, Linux/Windows installation and uninstall. These are package smoke checks, not GUI or live-call tests.

Only a push of the exact `v0.01` tag starts the release job after all three checks pass. The job confirms the repository is private and the tag matches its checkout. It uploads the actual three installers, `SOURCE_COMMIT.txt` and `SHA256SUMS` to a draft release, downloads every asset, compares bytes and checksums, then publishes. Failures leave the release unpublished. No persistent signing or account credentials are required.

Before tagging, verify the exact branch commit and its checks. Never replace a published tag or silently broaden the release to newer branch changes. [Release notes](packaging/RELEASE_NOTES.md) keep unfinished video/screen sharing and unverified live behavior explicit.
