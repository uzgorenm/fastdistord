# Contributing

Keep this a lightweight native voice and text client. Video and screen sharing are in scope but unfinished; League overlay work is stopped. No browser engine, bot/RPC substitute, saved audio recordings, telemetry, account-protection bypass or unrequested service backend. Prefer small changes with a clear user benefit and tests. Discuss substantial scope changes first.

## Before reporting a problem

Check existing issues. Include the source commit, OS/architecture, exact steps, expected result, actual result and whether it was an offline, hardware or live Discord test. Redact account information from screenshots. Never attach tokens, session IDs, full Gateway payloads or audio recordings.

## Development checks

Install the prerequisites in [Getting started](docs/GETTING_STARTED.md). Run the local baseline checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
rustc --edition 2021 --test vendor/songbird/src/driver/dave_policy.rs -o /tmp/fastdistord-dave-tests
/tmp/fastdistord-dave-tests
```

Launch `cargo run --locked` without credentials for the real offline interface. Do not add fake live-success states. UI changes should include actual before/after captures at matching normal and narrow sizes, plus keyboard and close/reopen checks. Do not claim a screenshot, mock or unit test verifies a Discord call.

Keep networking/device work off the UI thread and allocation/locks/I/O out of hardware callbacks. Preserve bounded buffers, direct privacy gates, session ownership and fail-closed encryption. New dependencies need a concrete maintenance/security justification.

Never enable dependency TRACE logging: the pinned Davey source includes sensitive cryptographic diagnostics at that level. Keep compile-time tracing caps. Cryptography and codecs stay in established libraries.

Update relevant guides with behavior changes. Document platform-specific code and honestly identify targets actually tested. Native Keychain, microphone permission and global shortcut changes require real macOS verification before claiming support is verified. Live tests require explicit account/channel consent and the [acceptance checklist](docs/LIVE_TEST.md).

Preserve upstream licenses and update [Songbird patch notes](vendor/songbird/FASTDISTORD_PATCH.md) for vendor changes. Do not publish tags, releases, packages or signing credentials as part of ordinary checks.
