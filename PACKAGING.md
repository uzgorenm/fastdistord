# Packaging

Current version: 0.1.0 experimental. There are no published releases, package-manager recipes, notarized builds or automated release uploads. Successful private CI runs retain macOS arm64 ZIP and Linux x86_64 tarball build artifacts for14days. Upload steps must finish before a run has downloads.

## Reproducible source build

Commit `Cargo.lock` and the pinned `rust-toolchain.toml`; build with `cargo build --locked --release`. Fastframe, egui and winit use exact Git revisions. Songbird source and its license are vendored with a documented safety patch. Native compilation also requires CMake and platform libraries; a lockfile does not promise bit-for-bit output across different operating systems or compilers.

## Apple Silicon `.app`

On macOS with Xcode command-line tools and CMake:

```sh
rustup target add aarch64-apple-darwin
./scripts/bundle-macos.sh
```

Output: `dist/Fastdistord.app`. The script includes and validates `NSMicrophoneUsageDescription`, copies the arm64 release binary and applies/verifies a local ad-hoc signature. This is not Developer ID signing or notarization. No release is uploaded. Review the bundle on an actual Mac before use or sharing; Linux smoke tests cannot establish native permissions/audio behavior.

## Validation before any future distribution

- Run contributor checks against the exact source commit.
- Build and open the native bundle; verify microphone consent and Keychain behavior.
- Complete explicitly authorized two-way voice, sustained DAVE membership, hidden-call, PTT, recovery and leave/quit tests.
- Record target-machine release memory/CPU/startup/underruns; compare against the stated goals rather than asserting them.
- Review licenses, write version-specific release notes and produce checksums.
- Obtain explicit approval before publishing, changing repository visibility, signing with persistent credentials or uploading a release.

CI checks Linux and macOS source builds and macOS bundle structure. It retains private Actions build artifacts; it does not publish a GitHub Release or claim a live Discord call. There is no updater or download button pointing to nonexistent binaries.

The Mac bundle includes camera and microphone usage descriptions. Local camera preview requires explicit access and Start actions. The native window picker is available on macOS 14 or later. Packaging checks do not verify physical capture or live Discord media. See [media status](docs/MEDIA.md).
