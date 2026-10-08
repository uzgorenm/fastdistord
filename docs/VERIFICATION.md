# MLS key-package envelope, QR exchange diagnostics and Discord call messages, 2026-10-08

The user's structural trace stopped after Connected, Identify, Protocol 1, KeyPackage 0, Peers 2 and ExternalSender 25. KeyPackage 0 was a fixed initial-send marker emitted after successful WebSocket send, not a zero-length package or server acknowledgement. Peers includes self; ExternalSender means Davey accepted the package. Source confirmed a concrete defect: Davey 0.1.4 serializes a bare KeyPackage, while [Discord opcode 26](https://github.com/discord/dave-protocol/blob/main/protocol.md#dave_mls_key_package-26) requires an MLSMessage containing it. The old WebSocket serializer omitted that envelope. A central boundary adapter now emits opcode 26 + MLS protocol version 1 + key_package wire format 5 + raw package, for initial and reinit sends. Initial trace now records KeyPackage 26. Gateway version 4 and opcode-first server parsing remain unchanged; no v8 sequence offset, readiness bypass or longer timeout was added.

A real OpenMLS/Davey replay generates the package before external sender, verifies the prior raw payload fails MLSMessage parsing, validates the corrected envelope and compares it exactly with OpenMLS serialization. It applies the external sender afterward, processes an authenticated fixture Add/commit/welcome, then verifies encrypted audio round trip. This proves pending-group creation preserves the already-created key package. It does not prove Discord accepted the new live frame. Bounded opt-in structural receive diagnostics now distinguish binary arrival, capped length, decoded opcode, candidate sequenced-header opcode and local parse error class (1 insufficient data, 2 invalid opcode, 3 invalid proposal operation, 4 parse error), plus JSON decode failure. The candidate marker is diagnostic only. No wire payloads or raw JSON are added to the trace; payload-bearing JSON debug output was removed.

The user's QR screenshot showed HTTP 400 after mobile approval, not 429. Existing endpoint and ticket-only request match the [remote-auth reference](https://docs.discord.food/remote-authentication/desktop); source found no duplicate exchange or evidence that normal post-approval closure caused the HTTP rejection. The earlier build discarded the response, so its specific cause is unknown. Bounded zeroized error parsing retains only numeric Discord code, CAPTCHA-field presence, MFA boolean and numeric retry duration. Server messages, tickets and challenge material are skipped. HTTP 400 no longer suggests a rate-limit/challenge bypass; only detected challenge/MFA flags produce corresponding guidance. HTTP 429 displays the supplied duration and gates the QR button during this launch; an unknown duration is not invented. Requests explicitly disable retry; the state machine allows one exchange, validates ticket bounds and never replays it. No challenge solving, client impersonation headers, authentication request or session approval was performed by the agents.

The user requested Discord's real call message rather than duplicate local text. Message type 3 and supplied call metadata now render directly from history/Gateway messages, with participant counts, end duration and missed-call interpretation only when valid metadata supports them. Partial MESSAGE_UPDATE replaces the existing ID without requiring repeated author/content; missing call fields preserve prior metadata. History and create events deduplicate by message ID. Local call timeline data/entries were removed; the persistent call panel and local tones remain. Known system events no longer get an empty-text placeholder; unsupported types use a generic system label. [Call message fields](https://docs.discord.food/resources/message#message-call-object) are best effort, so missing end timestamps do not assert a live call or fabricate duration.

Format, strict all-target Clippy and 119 offline tests passed; four hardware/socket checks remain ignored. Three standalone structural handshake tests passed. These checks include the real MLS envelope/order regression, reset invalidation and fresh-package recovery, actual v4 binary decoder fixtures for opcodes 25/27/29/30 and malformed frames, duplicate/partial call-message/end updates, missing metadata, redacted 400/CAPTCHA/MFA/429 diagnostics and one-use exchange schema. Live QR completion, Discord acceptance, peer changes and two-way audio remain user tests. Remembered login changes are preserved. Actions remain removed; PR 2 remains draft, no merge/public release.

# Remembered login startup, 2026-10-08

Source confirmed successful Keychain saves were possible, but startup never loaded a saved login; users had to press Connect from Keychain. Remember defaulted false every launch. Quit already preserved storage. Keychain save failure incorrectly aborted an otherwise authenticated connection. No credential values or local Keychain entries were inspected.

Explicit Remember me now saves after authentication and then writes a nonsecret opt-in preference under Application Support. Startup checks that preference and makes one in-app saved-login attempt without joining voice. Session-only login disables startup; Quit/window-close cleanup never removes credentials. Save/preference failure remains visible in Settings while the authenticated session stays usable. HTTP 401 and Gateway 4004 authentication rejection are typed, stop reconnecting and attempt stale remembered credential removal; transient failures and channel permissions do not delete credentials. Explicit Logout disables the preference and attempts Keychain removal independently, clears account state and revokes audio. Service/account and bundle identifier remain stable across versioned app filenames. Ad-hoc signing may still cause macOS Keychain access prompts; no grant, ACL change, security bypass or plaintext credential fallback was added.

Format, strict all-target Clippy and 112 offline tests passed (four hardware/socket tests ignored). New fake-store/policy checks cover persistence across simulated lifecycles, save denial/cancellation represented as storage failure, opt-in failure, deletion denial, logout and fixed authentication diagnostics versus network/channel errors. Native Keychain prompts, QR save and actual process relaunch remain user tests; the agents did not load/save a real credential or connect the account. Live MLS failure remains unresolved. Actions remain removed; PR 2 remains draft, no merge/public release.

# Multi-peer live stall, 2026-10-08

The user reports that other participants cannot hear them. Their screenshot shows protocol 1, two voice peers (including self), account roster 1, and no accepted MLS commit/welcome. This is a live multi-peer handshake failure, not proof of microphone denial. The disagreement cannot authorize indefinite solo waiting. Two-way voice remains unverified.

Structural diagnostics now distinguish no proposals received, proposals received without an accepted commit, and a candidate commit successfully sent but not accepted. Opt-in trace also identifies proposals without a local session and proposals processed without a candidate. CommitSent is recorded after a successful WebSocket send. Epoch resets clear proposal/commit observations. No keys, payloads or account identifiers are retained; readiness and timeout policy are unchanged. Three standalone handshake regressions and strict all-target Clippy passed. This diagnostic change does not establish or repair the live failure's cause. Compare an opt-in trace from a fresh user-authorized join with [Discord's initial group creation sequence](https://github.com/discord/dave-protocol/blob/main/protocol.md#initial-group-creation).

# Call feedback and confirmed mute follow-up, 2026-10-08

The user reported no calling sound/visible call change and that web Discord showed muted while the app showed unmuted. Source confirmed there were no call tones or persistent call-status panel; successful ring REST results were ignored by the UI. More significantly, mute updates required the recovery predicate's pinned devices, so a desired unmute during DAVE setup could be withheld, and the later ready event did not resend it. UI mute represented intent, while incoming confirmed self_mute/self_deaf were not applied to the transmission gate. These findings explain possible stale/misleading state; they do not establish the observed microphone permission result or remote notification delivery.

The coordinator now separates intent, confirmed self flags, server suppression/deafen, DAVE readiness and native permission. OP4 synchronization uses the owned channel/session, independently of recovery device pins. Initial/pending audio sends self_mute=true; encrypted/device readiness resends the current intent. Capture stays closed until the matching own Gateway state confirms unmuted/undeafened. Remote mute/unknown state revokes queued PCM and held PTT. Server restrictions remain authoritative; a different voice owner stops this client rather than being overwritten. UI/tray controls show blocked/pending state and explicit unmute retry.

A persistent call panel shows target, observed participants, distinct request/ringing/pending/active stages, cancel/leave and connected elapsed time. Bounded local call events appear only in their matching conversation, after server call confirmation, and clear on logout/account changes. No chat message is automatically sent. Original synthesized CPAL tones use the selected speaker, optional volume and deafen/sound toggles; no microphone or recording is involved. The worker blocks without audio resources when idle and releases one-shot streams. Ringing follows actual Gateway ringing state, stops on answer/cancel/leave/logout, and cannot restart from a stale UI snapshot. A bounded, non-retried stop-ringing request targets only the recipient(s) of the explicit current call. Its outcome can be uncertain. Reference: [ring/stop-ring and call events](https://docs.discord.food/resources/channel).

A read-only packaged executable invocation with --microphone-status returned “Microphone permission denied” in the tool's CLI launch context. No permission prompt or grant was triggered. This does not establish the previous running GUI build's authorization or prove the sole cause of the reported mute mismatch; confirm the new GUI's Settings status after manual launch.

Native AVFoundation reports permission status without TCC database access. An explicit authorized Join/Call requests access only when DAVE is ready, checks ownership/readiness again after the asynchronous response, and opens CPAL only after authorization. Denied/restricted/unknown/unanswered requests fail without automatic retries or grant/bypass. The bundle's NSMicrophoneUsageDescription remains present. Settings guidance follows [Apple microphone settings](https://support.apple.com/guide/mac-help/control-access-to-the-microphone-on-mac-mchla1b1e1fe/mac) and [AVFoundation authorization](https://developer.apple.com/documentation/avfoundation/requesting-authorization-to-capture-and-save-media).

Format, strict all-target Clippy and 108 offline tests passed, with four hardware/socket checks ignored. Offline regressions cover owned-session mute sync without device pins, confirmed/unknown/admin mute/deafen gates, queued PCM/PTT revocation, pending/denied unmute policy, channel-isolated real call events, answer/ring cancellation, active-only time, bounded original tones and stale-snapshot sound cancellation. These are offline lifecycle checks, not audible playback, native dialog, layout or live Discord interoperability tests. The agents did not open the account, ring users, send messages/media or capture a microphone. Native/live user retesting remains required. League overlay work is canceled. Actions remain removed; PR 2 stays draft, without a merge or public release.

# Pending DAVE and profile navigation, 2026-10-08

The user's next join reached Connecting, then failed with “DAVE end-to-end encryption did not become ready; microphone stayed closed.” The user confirmed they were alone. No successful two-way call has been established.

A transport connection can now remain joined with encryption pending when a complete account Gateway roster confirms only the local user in that channel. Initial voice audio streams stay closed; optional local call tones can briefly open the speaker. A peer join starts the bounded MLS deadline; unknown/incomplete rosters and failed MLS exchanges cannot gain this indefinite waiting state. Audio starts only after an accepted commit/welcome and executed DAVE transition. Capture gating invalidates queued PCM and resets PTT across readiness changes. This does not mark pending groups ready, generate empty commits or bypass encryption. An early external-sender package arriving before a version-zero session upgrades is now retained within a fixed size bound. Explicit individual Calls ring after secure transport connection so a recipient can join the pending group; ringing remains scoped and canceled on leave/logout/switch.

Settings has an opt-in, memory-only 64-event structural handshake trace. It contains local stages, relative timing, versions, counts and transition IDs, without packet payloads, keys, user IDs or credentials. Disable clears retention. Dependency TRACE logging remains compiled out. Protocol reference: [sole-member reset](https://github.com/discord/dave-protocol/blob/main/protocol.md#sole-member-reset).

Selected server clicks collapse or expand its nested channels without sending a selection/leave command. Friends sort by the latest known individual DM message Snowflake, with stable name/ID ties, and update on send/receive without fetching each history. Microphone, deafen and Settings icons sit beside the profile; keyboard button behavior and accessible labels are retained. Public user portraits and guild icons use static PNGs, one background worker, fixed Discord CDN URLs, no redirects/authentication, bounded response/decode/cache sizes and initial fallbacks. Logout cancels fetches and clears textures. Presence uses observed Gateway statuses and an aggregate/single self session; missing/stale/unsupported status stays unknown. The app does not infer presence from connection state or try to detect invisible users. Sources: [Discord CDN image formats](https://docs.discord.com/developers/reference#image-formatting), [presence events](https://docs.discord.food/gateway/gateway-events#presence-update), [session aggregate semantics](https://docs.discord.food/resources/presence#session-object).

Format, strict Clippy, 101 offline tests, two standalone handshake tests and six standalone DAVE policy tests passed; four hardware/socket tests remain ignored. Offline regressions exercise sole/unknown/multiple rosters, the bounded peer deadline, queued audio/PTT invalidation, pending real MLS encryption refusal and the creator's own commit without Welcome, DM recency updates, partial/unknown presence, and restricted CDN keys. Existing encryption/transition checks remain. Native visual layout, the sole-member reset sequence on Discord, two-way hardware audio, peer membership changes and recovery require another user test. The agents did not open an account, send content, ring a recipient or capture live audio/video. Process inspection was denied by the host; no running-build claim is made. PR 2 remains draft, Actions remain removed, and publication remains paused.

# Live-test failure fixes, 2026-10-08

The user retested build 65b40b8 and reported two failures: “Discord returned invalid conversation recipients” in Friends, and “Discord supplied an unsupported voice server port” when joining voice. These were real failures; the earlier offline checks did not establish live compatibility.

Friends loading coupled two independent REST results and aborted on one conversation without usable recipients. Channel recipients are optional in the documented channel schema. List parsing now skips unsupported or malformed entries while preserving valid conversations; explicit open-DM responses remain strict. The friends and conversation requests run independently, so a failed conversation fetch cannot discard accepted friends. No recipient is inferred from a channel ID, recipient_ids, friend ordering or another account. Empty/missing recipient lists and unknown channel types are hidden with a bounded count. Invalid accepted relationships cannot discard other valid friends. Top-level invalid/oversized responses still fail.

The endpoint adapter imposed a fixed signaling-port list that Discord's endpoint contract and Songbird's WSS URL construction do not require. It now preserves any canonical decimal port in 1..65535 on a validated Discord voice hostname. TLS certificate verification and the existing hostname, userinfo/path/query/IP/DNS restrictions remain. The media UDP port is separately negotiated in Voice Ready; it is not used to replace the signaling port. No downgrade, port guessing, connection fallback or automatic live join was added. Sources: [official voice endpoint and Ready payloads](https://docs.discord.com/developers/topics/voice-connections), [channel recipient schema](https://docs.discord.food/resources/channel).

Format, strict Clippy and 94 offline tests passed, with four hardware/socket tests ignored. New regressions cover mixed valid/partial/empty/group/unsupported conversations, malformed relationships, independently failed social fetches, and canonical explicit signaling ports. The six existing DAVE policy checks remain unchanged and previously passed. Rebuilt Mac packaging is verified separately. No credentials, raw payloads, recipient details or voice/session tokens were inspected or logged. The actual rejected conversation entry and actual endpoint port remain unknown; this fixes the confirmed code failure paths, while successful Friends loading and two-way Discord voice require another user test. Native GUI automation is not retried. PR 2 remains draft and release publication stays paused.

# Friends, server navigation and voice endpoint follow-up, 2026-10-08

The connected UI now uses one sidebar with equal-width Friends | Servers tabs, inline server channels, direct conversation selection, and persistent call controls. Settings holds device, push-to-talk and volume controls; advanced explanations are collapsed. Friend and server conversation selections are remembered separately.

The account adapter loads accepted friends and existing private conversations, opens a DM only on friend selection, and sends or calls only on explicit actions. Individual calls use a null guild ID on the account Gateway and the actual private channel ID for voice identification. Ringing names that recipient explicitly and runs once after encrypted voice readiness. Message scope changes cancel stale reads and clear drafts. Gateway message events update only the selected known channel.

The reported join error was “Gateway supplied an invalid Discord voice endpoint.” Source inspection confirmed that the validator rejected port forms appearing in Discord documentation: port 2048 in the official voice example and port 80 in a historical API issue. The validator now accepts those forms while retaining WSS, Discord hostname checks, and rejection of arbitrary authorities, paths, credentials and malformed ports. The user's actual negotiated endpoint was not inspected, so this finding does not prove the precise cause of that live failure. References: [official voice documentation](https://docs.discord.com/developers/topics/voice-connections), [historical port report](https://github.com/discord/discord-api-docs/issues/1694). Private-call routing and ringing use the [reverse-engineered protocol reference](https://docs.discord.food/topics/voice-connections) and [channel reference](https://docs.discord.food/resources/channel); they remain unofficial and need live interoperability testing.

Local format, strict Clippy, 91 deterministic tests, and six standalone Songbird DAVE policy tests passed. Four hardware/socket-dependent tests remain ignored. Focused regressions cover the rejected endpoint forms and DM/private-call scope. The Mac release build, microphone usage description and strict deep ad-hoc signature checks passed. A cached debug-info stripping warning did not prevent compilation or signing. No Actions workflow was added or run.

Native GUI inspection was not retried after cancellation. The screenshot download returned HTTP 403; the error text above came from the parent task's pixel inspection. No credential entry, outgoing message, ring, microphone capture or Discord call was performed by the agents. The user must verify layout, keyboard/narrow-window behavior, friend/DM loading, text send/receive, two-way voice, DAVE membership changes, leave/rejoin, and recovery. PR 2 remains draft; release publication and merging are paused.

# Version 0.01 packaging and production UI, 2026-10-08

Current production UI contains account entry, voice and text only. Standalone media previews, codec diagnostics and the audio diagnostics panel were removed; automated/headless media tests remain. Local format, strict Clippy, all 77 deterministic tests and the SemVer 0.0.1 Mac bundle/signature checks passed. The user-facing version is 0.01. Native GUI inspection was not retried after cancellation. Hardware and live Discord behavior remain unverified; the user will perform live testing.

GitHub Actions workflows were removed at the user’s request. Packaging now uses local scripts on native build hosts. Historical runs below remain historical evidence only; they do not verify newer changes or a published release.

# Account flow and compact UI follow-up, 2026-10-08

The signed-out view now uses a centered account form instead of empty server and call panels. Session entry and explicit Keychain reconnect have separate tabs. Risk acceptance still gates both actions; changing tabs does not load credentials, and password undo history is cleared on either path. Connected accounts retain channel navigation, participant states, text composition and persistent mute/deafen/PTT/leave controls with tighter spacing. Diagnostic details remain in Settings. The obsolete startup screenshot was removed from the README because it no longer represents the current layout.

Local format and strict Clippy checks, all 77 deterministic tests, the release build and Mac bundle/signature validation passed. No new tests were added for this presentation change; existing privacy and authorization regression checks remain intact.

Current Discord OAuth documentation confirms that the `voice` scope requires approved-partner access. Standard identity/guild OAuth cannot authorize this adapter. No supported installed-client credential export or account sync was added. No credential was read or entered, and no outgoing Discord content was sent.

Computer-use inventory reported Fastdistord running. Read-only selection of the existing Fastdistord app via `cua.getApp("me.uzgoren.fastdistord")` was canceled by the user after 5959.5 seconds, before window content was returned. It was not retried. Visual layout, keyboard traversal, resizing and native accessibility remain unverified for this follow-up; the prior Linux GUI check below applies only to its older UI.

# Native Mac continuation, 2026-10-08

The Mac checkout began at `bb9536ba5b2356e81ba59bdb29e6184d10a0eb6e`. The interrupted account/messaging edits were preserved and integrated with text-channel selection, manual history refresh and explicit plain-text Send. Pending history requests are canceled on scope changes; stale results cannot replace the current account/channel snapshot. A regression test checks that scope changes erase displayed text and cancel pending reads without altering voice mute state.

Native Apple Silicon Rust 1.99.0 checks passed: format, strict Clippy, 77 deterministic tests and six standalone Songbird DAVE policy tests. Three hardware/socket-dependent tests are ignored by default. The composed headless native test was run separately and passed: synthetic H.264 encoding, real two-participant DAVE encryption, RTP fragmentation, transport AEAD/authentication, reassembly, DAVE decryption, VideoToolbox decoding and checkerboard pixel fidelity. Tests also cover tamper/replay rejection, participant removal and stale fragment authority. Both transport modes passed authentication/replay tests, including nonce exhaustion and permanent revocation. A synthetic DAVE/RTP/AEAD loopback UDP test passed separately. These are local fixtures, not Discord interoperability or live membership verification.

A local release build and `scripts/bundle-macos.sh` succeeded. `plutil` and strict deep ad-hoc signature verification passed for `dist/Fastdistord.app`. The bundle includes microphone and camera usage descriptions; the screen picker requires macOS 14 at runtime. A cached build-script warning reported debug-info stripping unavailable; compilation and signing still succeeded. The bundle is not Developer ID signed or notarized.

The exact computer-use action `cua.getApp('/Users/mehmetuzgoren/Desktop/Side Projects/fastdistord/dist/Fastdistord.app')` returned **aborted by user** after 3121.2 seconds. It was not retried. App startup was not confirmed; this was not a macOS permission-denial result. The bundle remains available for manual opening. No GUI screenshot, packaged microphone/camera permission, physical device preview/round trip, background call, PTT hardware result or Mac resource measurement is claimed.

The app now exposes explicit local camera/window preview. Capture adapters compile, but no camera/screen content was captured or transmitted. The offline RTP/AEAD helpers do not implement Discord negotiation or network video. See [media status](MEDIA.md).

No credential was entered and no account/channel session or outgoing text/audio/video test was performed. Two-way voice, live DAVE membership changes, leave/rejoin and recovery require user credential handoff, target channel, second participant and approved test content. League overlay work and League interactions are stopped. No public release was published.

Focused source review fixed editor undo retention across text scope changes/logout, clearing drafts even while the preview tab is open, stopping preview on minimize/occlusion, and clearing its texture after native capture stops. RustCrypto zeroization features now erase expanded AES/GHASH state on drop. The editor regression test passed; physical window/capture behavior still needs manual verification.

The report below records earlier cloud checks, not native hardware acceptance.

# Verification report — 2026-10-07

## Passed locally in the cloud

Environment: x86_64 Debian 13, Rust 1.99.0, official native development packages extracted to a workspace-only sysroot. No user Mac was used for implementation or tests.

- `cargo fmt --all -- --check`
- `cargo test --locked`: **54 passed**, 0 failed
- `cargo clippy --locked --all-targets -- -D warnings`: passed
- `cargo build --locked --release`: passed; baseline final link/build36.87 seconds with dependencies already compiled; recovery rebuild checked separately
- Six standalone production DAVE/retry policy tests: passed
- Native offline GUI smoke: startup, Settings, normal960×660/minimum740×540 resizing, scrolling to Quit and normal window-close clean exit
- Independent source review rechecked identified lifecycle/privacy problems after fixes

Coverage includes audio mute/PTT/unknown state, queued-sample epochs, stale session revocation, bounded mixing, synthetic duplex resampling, old-device failure/cleanup isolation, credential-safe diagnostics, voice endpoint validation, canceled joins, initial roster updates, permission-event scoping and concurrent PTT publish/focus-clear. DAVE policy tests cover unready/protocol-zero/plaintext refusal and rekey-generation invalidation. They are **not** full handshake, MLS cryptographic or real-network integration tests.

## Measured disconnected baseline release build (26161d9)

Cloud Linux Xvfb with Mesa software rendering, no account connected and no microphone opened:

| Observation | Result |
|---|---:|
| Executable size |28,494,232 bytes (27.17 MiB)|
| Process launch to viewable window |0.6661 seconds|
| Resident memory |117,152 KiB (114.4 MiB)|
| Process CPU ticks over5.0001 seconds |0, at100 ticks/second resolution|
| Threads |35|
| Normal close |Exit code0|

Startup was polled every100ms; disk/cache state was uncontrolled. This is not a cold-start or sustained performance benchmark. Zero measured ticks over this short interval does not establish a universal CPU guarantee. The observed cloud RSS is above the provisional100MB idle goal. Software rendering and Linux runtime costs cannot predict the target Mac result.

No connected-silent, active call, hidden active-call, multiuser or official-client comparison measurements were performed. The <150MB call memory goal and target-Mac idle CPU goal remain unverified.

## Implemented, not live-verified

- Unofficial personal-account identity/guild/channel REST and Gateway signaling
- Existing-user voice join/leave/switch and initial voice-state roster handling
- Songbird/Opus transport plus Davey/OpenMLS DAVE negotiation/membership paths
- Explicit fail-closed DAVE/media-generation and late packet privacy guards
- CPAL device pipeline, microphone permissions, hardware recovery, global shortcuts
- macOS Keychain and ad-hoc-signed `.app` packaging script
- Tray-resident calls with window hidden; server mute/deafen/move/kick behavior

## Blocked acceptance

No Discord account session, real microphone, second participant or live voice channel was used. Supported personal `voice` OAuth access remains restricted to approved partners. The experimental adapter needs explicit informed opt-in, a credential entered locally, a concrete authorized channel/session and a real second participant before live acceptance can be attempted. It may fail or expose the account to policy risk.

This cloud is Linux. The baseline private CI run built and validated a macOS `.app` bundle. It did not retain that bundle as a download. Native Mac microphone permission, Keychain round trip, audio and performance tests were not performed. The source includes reproducible build steps and native CI/bundle validation, not a claim that those hardware tests passed. CI results on the remote commit must be checked separately after push.

## Remaining high-impact limitations

- Undocumented personal-account protocol may break or be rejected; do not promise Discord compatibility.
- No AEC, noise suppression or AGC; use headphones.
- Bluetooth, sleep/wake, packet loss, sustained multiuser DAVE and background PTT need real hardware/live verification.
- Selecting different devices intentionally requires an explicit new Join. Same-device failures and resumable transient signaling losses now have bounded automatic recovery; invalid sessions, ambiguous disconnects, server revocation and exhausted retries require explicit action.
- Native Wayland lacks the selected global shortcut backend; the visible hold control is the fallback.
- No signed/notarized download or release was published.

## Recovery delta

The recovery implementation adds five-attempt1/2/4/8/16-second backoff, a60-second stable-success budget reset, exact session-generation checks and pinned device IDs. Signaling uses Discord Gateway RESUME with the supplied validated Discord resume endpoint; it never automatically falls back to fresh identification. Replayed kick/move/permission events invalidate the voice cache before RESUMED permits restoration. Voice recovery reuses only the same authorized guild/channel/session credentials; it sends no automatic channel-join command. Device recovery reopens only the same logical microphone/speaker IDs, preserving current volume and explicit mute; PTT resets to unknown/released.

New local tests cover backoff exhaustion/stability, canceled retry generations, revoked/session-changed voice authority, missing device pins and resume-endpoint/close-code restrictions. These are offline tests of production decision helpers. No real network interruption, unplug, default-route switch, Bluetooth or sleep/wake recovery was exercised. Automatic recovery is implemented, but hardware/live interoperability remains unverified. OS-managed virtual devices may change their physical backing outside the app.

Protocol reference: [Discord Gateway resume lifecycle](https://docs.discord.com/developers/events/gateway#resuming). Its documented transport rules do not make this personal-account adapter an approved integration.

Baseline CI succeeded on Ubuntu24.04 and macOS14 for remote commit1666044c43a0e20b44fcf2c3f91941200cabc3ea, including tests/release and macOS bundle validation: [verified run](https://github.com/uzgorenm/fastdistord/actions/runs/37679324707). Recovery-delta CI and the newly added private build-artifact upload steps require separate verification after push. No download is claimed ready before its run completes.
