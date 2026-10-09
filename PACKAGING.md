# Packaging Fastdistord 0.02

The public-facing version is **0.02**, tag **v0.02**; Cargo and native package metadata use SemVer **0.0.2**. Releases stay in the private repository.

## Packages

- Mac Apple Silicon: `scripts/bundle-macos.sh`, then `scripts/package-macos.sh`. The DMG includes Fastdistord.app, an Applications link and the MIT license. The app is ad-hoc signed, not notarized or Developer ID signed.
- Windows x64: NSIS 3.13 compiles `packaging/windows/installer.nsi`. It installs for the current user, creates Start menu shortcuts and includes an uninstaller. No administrator access or automatic app launch. Not Authenticode signed.
- Linux amd64: `scripts/package-linux.sh` creates a `.deb` with desktop entry and license. Runtime dependencies are derived from the actual executable, with required dynamically loaded GUI libraries included. Requires a native compatible Debian/Ubuntu build host; no Linux artifact is included in v0.02.

Build with Rust 1.99.0, CMake and the prerequisites in [Getting started](docs/GETTING_STARTED.md). Installers include upstream dependency/source notices, font licensing and Songbird patch attribution. All dependencies are locked. Native tools/compiler versions can still affect output; this is not a bit-for-bit reproducibility claim.

## Release verification

Build and test locally on each target platform. Verify mounted DMG contents and signature, installed payload hashes, `--version`, and Windows/Linux installation and uninstall. These package checks do not establish GUI or live-call behavior.

GitHub Actions workflows have been removed. Never use an Actions fallback or a paid build service without approval. This Mac builds the Apple Silicon app and DMG natively and can cross-compile Windows x64 with MinGW-w64/GCC and NSIS. Set the Windows target linker and C/C++/archive tools, bundle Opus for that target, and collect notices for the Windows dependency graph. Archive inspection does not replace Windows installation, startup, audio or uninstall testing. Linux still needs a native build host.

Publish only actual verified installers. Record the exact source commit in `SOURCE_COMMIT.txt` and asset hashes in `SHA256SUMS`. Verify private repository visibility, upload to a draft release, download and compare every asset, then publish. Never replace a published tag. See [Release notes](packaging/RELEASE_NOTES.md).
