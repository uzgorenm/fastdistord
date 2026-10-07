# fastdistord's Songbird 0.6.0 patch

Upstream: https://github.com/serenity-rs/songbird

Source revision: `5d46185511316b8efb3c65a1104e149e4df557cd` (0.6.0).
The original ISC license is retained in `LICENSE.md`.

## Why the patch exists

The upstream mixer transmits transport-encrypted Opus when DAVE is absent or
not ready. Upstream `DriverConnect` reports transport establishment, not completed
MLS negotiation. Neither behavior satisfies fastdistord's mandatory end-to-end
voice requirement. This fork adds policy checks around upstream cryptography;
it does not implement or replace any encryption, MLS, codec, or packet format.

## Changes

- `Config.require_dave` opts into fail-closed media handling. fastdistord always
  sets it to true.
- `Config.dave_ready` is published only when a nonzero DAVE protocol, a successful
  active MLS transition, and davey's ready session all agree. It resets before
  transitions, reinitialization, connection attempts, and disconnection.
- The transmit path drops media rather than falling back to transport-only
  encryption. DAVE encryption failures cannot reach the UDP send path. Borrowed
  davey output is rejected; with pinned davey 0.1.4 this includes standard Opus
  silence frames. No special plaintext-silence exception is enabled.
- Required receive mode drops plaintext/non-DAVE media before Opus decoding.
  The marker check is only a filter; davey still authenticates/decrypts the frame.
- DAVE changes increment a monotonic generation. Preparation captures that
  generation while encrypting. The final send rejects a changed generation,
  including a full ready → negotiating → ready cycle between encryption and send.
  A shared short synchronous mutex orders state invalidation, encryption, and
  the final UDP send. It is never held across an async await. A poisoned lock
  fails closed. Already-sent network packets cannot be recalled.
- `Config.packet_gate` adds a final synchronous privacy check immediately before
  UDP send. The application checks session ownership, device health, mute/deafen,
  PTT state, and the capture epoch. A gate change invalidates buffered frames;
  only a freshly replaced input track can arm the new epoch on the mixer thread.
- Strict mode never enables upstream's transport-only receive passthrough.
- Standalone `receive` now enables its required `dashmap` dependency without
  pulling in the Discord bot gateway.
- Voice session IDs are redacted from `ConnectionInfo` and partial-state Debug.
  Unused bot-gateway internals are conditionally compiled. Bench entries whose
  sources are not vendored were removed.

- Strict fastdistord mode disables upstream internal connection/WebSocket retries. The application coordinator exclusively owns bounded retry authority; unknown closures cannot silently resume underneath it. Explicit WebSocket I/O failures retain their transient classification.

## Verification and limits

The application transport tests cover credential-safe diagnostics, endpoint
validation, channel-ID mapping, invalid IDs, and capture-epoch revocation.
`src/driver/dave_policy.rs` contains dependency-free production policy tests for
protocol zero, unready sessions, plaintext/silence rejection, and the
prepare/execute generation race. Run them without a device or network:

    rustc --edition 2021 --test vendor/songbird/src/driver/dave_policy.rs -o /tmp/fastdistord-dave-policy-tests
    /tmp/fastdistord-dave-policy-tests
    cargo test transport::tests

These are offline safety/structure checks. They do not establish successful
live Discord compatibility, real hardware behavior, audio quality, or a full
cryptographic audit. Live testing requires the user's explicit authorization.
No Discord session was opened or microphone captured during implementation.

Do not enable dependency TRACE logging: davey 0.1.4 includes cryptographic secret
material in TRACE events. The application must compile those levels out and
must not attach a subscriber that restores them in an altered dependency build.
