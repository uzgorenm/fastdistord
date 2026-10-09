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

Application ownership must account for this upstream `Driver` behavior:
dropping any clone sends `CoreMessage::Poison` and shuts down its workers.
Fastdistord transfers the original driver out of its pending setup guard on
success; it must not clone and drop the original at that boundary. Offline
transport regressions verify that the registered event handler survives the
handoff and that aborted setup releases its workers. The vendor's clone/drop
semantics are unchanged.

The standalone driver accepts a private call with `guild_id: None`. Voice
Identify and Resume then use the real DM channel ID as `server_id`; guild calls
continue using their guild ID. Connect/disconnect event metadata keeps the
optional guild rather than fabricating one. The bot gateway frontend still
creates guild calls only. This adds private-call routing, not bot access to DMs.
Private-channel `server_id` routing is described by the reverse-engineered
reference https://docs.discord.food/topics/voice-connections, rather than
Discord's official bot API documentation; live user-account compatibility is
still unverified.

The application accepts strictly validated Discord voice authorities with an
optional canonical decimal port in 1..65535. Songbird keeps explicit ports in
its WSS URL; the authenticated Gateway selects that signaling authority. The
UDP media address/port arrives separately in Voice Ready. An earlier fixed
443/80/2048 allowlist rejected the user's live endpoint and was removed; no port
is guessed or silently stripped. WSS, certificate verification and the Discord
hostname suffix restriction remain mandatory. Other schemes/hosts, userinfo,
paths, IP literals, malformed DNS labels and malformed/out-of-range ports fail
without echoing the supplied address. See the official endpoint example and
separate UDP Ready payload in
https://docs.discord.com/developers/topics/voice-connections.

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

## Pending sole-member handshake

`DaveHandshake` records bounded structural observations and an opt-in memory-only
64-event trace. The application can wait without opening initial audio devices
when its complete account Gateway voice roster confirms only itself is present.
Unknown/incomplete rosters, multiple participants and MLS failures retain a
bounded media handshake deadline. A locally prepared MLS group remains distinct
from an executed transition; neither a timer nor peer absence makes it ready.

A bounded public external-sender package is retained if it arrives before a
version-zero session upgrades through PrepareEpoch. Readiness invalidation also
closes the application's capture gate synchronously, revoking queued PCM and
requiring a fresh PTT press. Existing packet-generation and media-lock guards
continue to reject stale frames across prepare/execute transitions.

Protocol: https://github.com/discord/dave-protocol/blob/main/protocol.md#sole-member-reset
Offline crypto fixtures verify pending groups cannot encrypt Opus and that the
creator's own accepted commit can establish a group without receiving Welcome.
These checks do not establish live Discord interoperability.
