# Controls and troubleshooting

[Back to README](../README.md) · [Setup](GETTING_STARTED.md) · [Verification](VERIFICATION.md)

Use `open dist/latest/Fastdistord.app` after a local bundle build. Settings → About shows the source commit so you can identify the running build.

Settings separates Voice & Audio, Notifications, Shortcuts, Appearance, Account and About into tabs. Each category scrolls independently when needed. Clicking either a friend’s avatar or name selects the same conversation.

Remember me saves to macOS Keychain after a successful login. Quit preserves it; startup restores it without joining voice. Existing saved entries can migrate when no preference exists. An explicit session-only login or Logout prevents startup restore. Storage errors appear in the app; approve its macOS Keychain prompt when needed. The ad-hoc signature can cause another prompt after an update. No password is stored in the preference file.

## Everyday controls

| Control | Behavior |
|---|---|
| Connect | Explicitly opts into the unofficial adapter and checks the entered account |
| Friends / Servers | Switches the sidebar list; servers start collapsed; click a server, then Chat or Voice to see its channels |
| Friend | Opens a private conversation without calling |
| Call | Starts the selected individual DM call; audio waits for DAVE and microphone permission. Ring requests and confirmed ringing are shown separately |
| Join · channel | Joins that voice channel and opens audio only after DAVE readiness |
| Mute | Immediately closes outgoing audio; buffered pre-mute samples are invalidated |
| Deafen | Silences receive audio and closes outgoing audio |
| Push to talk | While unmuted, hold the configured talk shortcut or the visible Talk control |
| Settings | Audio test/processing, participant volume, shortcuts, appearance, updates and account controls |
| Reconnect | Restarts failed signaling with the in-memory credential; explicitly rejoin voice afterward |
| Leave / Cancel | Ends voice and releases both audio devices |
| Log out | Clears the account and removes a saved macOS Keychain credential |
| Quit | Stops audio and exits; preserves a remembered login |

PTT and all shortcut bindings start disabled. Settings can assign mute, deafen, answer, leave and hold-to-talk to this window or an explicitly selected global scope. Local shortcuts are suppressed while typing. Global registration failures appear in Settings; the visible hold control remains available. Enabling PTT alone does not register a global shortcut. Native Wayland has no global shortcut support in this backend. Focus/visibility changes release pending talk state; uncertain input fails muted.

## Text channels

Choose a friend, or choose **Servers**, a server and its text channel. The app loads the latest 50 messages and listens for new messages in the selected conversation. **Load older** fetches another page, up to 200 retained messages. **Refresh** fetches a new history snapshot. Events received during a fetch are reconciled so deleted messages do not return. Call controls remain available while browsing.

Type plain text and press **Send**. Enter adds a line rather than sending. Messages are limited to 2000 characters. The app suppresses mentions, text-to-speech and link embeds. Hover or open a message action menu to reply, react, edit your own supported message, or copy text. The same actions are keyboard accessible. Discord checks effective channel permissions. Attachments and threads are not supported. Empty-content messages display a placeholder; they may contain unsupported attachments or system content.

Sending disables server/channel changes until the response arrives. A failed send keeps the draft. If its outcome is uncertain, refresh history before retrying to avoid duplicates. Selecting a different text channel or server clears the unsent draft. Switching Friends/Servers or Chat/Voice only changes the sidebar and preserves the draft. Logout cancels pending text work and clears displayed history. Channel changes, successful sends and logout also erase the editor undo history, so Undo cannot restore a draft from an earlier scope.

Unread dots and mention counts use account read state and Gateway events. Missing or interrupted state remains unknown. **Mark read** sends an explicit acknowledgement for the selected latest message; badges change when Discord confirms it. Reaction counts that overlap a history fetch remain unknown until a complete update or refresh.

## Call feedback

Actual incoming Gateway rings show a compact Answer/Decline prompt. Answer joins without ringing the other person again; Decline silences locally and asks Discord to stop ringing this account. Remote cancellation can fail independently. Call alerts expire after 45 seconds and clear on answer, decline, cancel, leave, logout or signaling loss. macOS desktop alerts require an explicit permission request and opt-in. Other platforms report desktop notifications unavailable.

The bottom bar keeps your profile and call controls together while you browse. It shows the call target and connection or microphone status. Hover over the status for participant information and elapsed time. Cancel/Leave ends the call.

Conversations render Discord’s actual call system messages once, including supplied participant/end metadata. The current-call panel shows live state separately; the app does not invent call history. Original local tones provide connecting, confirmed-ringing and join/leave feedback. Settings has Call sounds and a separate volume control; deafen silences them. Local sound is not proof of remote notification. Ring cancellation is one scoped best-effort request, without retries or a delivery guarantee.

The microphone icon reflects the actual transmission gate. An unmute request can stay pending during encryption setup or until Discord confirms self-mute is off. Server mute/deafen, another voice owner, released/unknown PTT, missing permission and device failures can keep it closed. A new voice-session owner stops this client; it does not fight the other session. Use Unmute on Discord to explicitly retry a ready call's unmute.

## Audio test and processing

In Settings → Voice & Audio, choose an input and select **Record test**. The app confirms the actual selected device, captures at most five seconds in memory, then waits for **Play test**. Playback consumes the clip. **Stop & erase**, leaving the Voice & Audio tab, closing Settings, hiding the window, changing devices or processing, starting a call, logout and quit dispose of the test. An unused clip expires after one minute. Test samples never enter the call transport or a file.

Noise suppression uses the native Rust [nnnoiseless](https://github.com/jneem/nnnoiseless) RNNoise implementation. Automatic gain is a separate conservative level adjustment. Both are optional and off by default. Echo cancellation is unavailable: the app does not have the synchronized speaker reference and delay handling it requires. Use headphones. Participant sliders attenuate received audio from that stable user ID from zero to normal volume; at most 128 custom values are stored.

## Devices and background use

Choose stable device IDs through their friendly names in Settings. Selections apply on your next Join: leave and rejoin to change devices safely. Removed devices are labeled unavailable. Idle/disconnected mode captures no audio unless you explicitly start the local microphone test in Settings. A device failure closes this audio engine immediately. Recovery retries only the same pinned microphone and speaker IDs with bounded backoff. It never substitutes another device; exhausted retries leave an actionable failure. OS-managed virtual devices can change their underlying physical route outside the app, which still needs hardware testing.

Closing a window keeps an active call running only when a usable tray/menu-bar path exists. Reopen from the tray; choose Quit to exit. Without a functioning tray, ordinary window close exits so the process does not become inaccessible. Audio and global PTT are independent of rendered UI frames.

On macOS, Settings reports the native microphone authorization status. An explicit Join/Call requests access only when encryption is ready to open audio. Denial or an unanswered request stops that attempt; the app cannot grant permission. For denied access, use [System Settings → Privacy & Security → Microphone](https://support.apple.com/guide/mac-help/control-access-to-the-microphone-on-mac-mchla1b1e1fe/mac), then explicitly Join again. A pending sole-member MLS group may not reach the permission request yet.

## Common problems

- **Account denied:** the unofficial adapter may not be accepted for your account. No MFA/CAPTCHA/protection bypass is provided. Do not repeatedly retry; use official Discord if this route is unavailable.
- **Rate limited:** stop and wait before retrying. There is no automatic bypass or alternate account attempt.
- **Joined · You’re alone:** a complete account roster containing only you, with no contradictory voice peers or MLS failure, allows staying joined while waiting for the MLS group exchange. Unknown or contradictory rosters display **Joined · encryption pending**. Voice audio streams remain closed; optional local feedback can use the speaker. When a peer joins, DAVE must finish before audio opens. An unknown roster, peer handshake stall or MLS failure still has a bounded timeout.
- In Settings, enable **Record redacted voice handshake trace** before a test join, then use **Copy handshake trace** to report a failure. This memory-only trace contains typed stages, timings, protocol versions, participant counts, initial roster, JSON/binary decode outcomes, socket close codes, voice-loop lifecycle, heartbeat send/ACK outcomes and transition IDs; no credentials, user IDs, keys or packet payloads. Disabling it clears the retained trace.
- **Microphone or speaker unavailable:** recovery retries the same authorized device IDs; it does not fall back to a different microphone or speaker. Check OS permissions, connected devices and selected route. On macOS, launch the `.app` containing its microphone usage description. Leave, select a valid device and rejoin.
- **Silent while PTT is enabled:** both explicit unmute and a held talk control are necessary. Server mute/deafen/suppression remains authoritative.
- **Moved, kicked, or permissions changed:** audio stops. Check access and explicitly choose/join a channel; the app does not undo the server action.
- **Network interrupted:** a resumable session is retried with bounded backoff. Missed events are replayed before voice restoration, and any kick, move or permission revocation cancels restoration. No automatic fresh login or channel-rejoin command is sent. Explicit mute is preserved; PTT returns released/unknown. If the session cannot safely resume or retries are exhausted, choose Reconnect and explicitly Join.
- **Echo or poor speakerphone quality:** use headphones. Echo cancellation is unavailable; optional suppression and automatic gain cannot remove speaker echo.
- **Unknown/missing participant names:** initial voice snapshots and later updates are supported, but undocumented personal Gateway payloads vary. Unknown identities may display a user ID; this needs live interoperability testing.
- **Mac security warning:** this project does not ship a notarized download. Do not bypass browser security warnings or trust unexpected third-party binaries. Build locally from reviewed source; ask the maintainer about signing before distribution.

## Privacy and stored data

No raw audio is written to disk. No telemetry is included. Text history and drafts stay in memory; the app sends text or message actions only when you explicitly submit them. Credentials are memory-only unless you explicitly select macOS Keychain storage. Linux has no plaintext persistence fallback. The Keychain service is `fastdistord.personal-account`; logout removes it. If deletion fails, the app tells you to remove it in Keychain Access.

The app contacts Discord's HTTPS API, account Gateway, negotiated Discord voice endpoint and UDP voice server. An explicit update check, or an opted-in startup check, also requests bounded anonymous metadata from GitHub. Private releases may require opening GitHub in your browser; the app does not reuse Discord credentials for update checks. No project-operated relay or fallback service is used. Dependencies' debug/trace logging is compiled out to prevent sensitive cryptographic diagnostic events.

For bug reports, include platform, app commit, expected/actual behavior, steps and a screenshot with personal information removed. Never include credentials, raw Gateway payloads, session IDs, voice tokens or recordings.

## Recovery boundaries

Transient recovery uses five attempts with1/2/4/8/16-second delays. A retry budget resets only after60seconds of stable success, preventing endless rapid failure loops. Each session-resume attempt has a deadline; authentication failures, invalid sessions, rate limits, unknown/ambiguous disconnects and DAVE failures require explicit action. Voice restoration requires the previously authorized same guild/channel/session and the same pinned devices. Fresh server endpoint updates replace cached voice credentials; cancellation clears them.

Leave, Log out, switching accounts/channels, server move/kick, permission changes and a removed voice endpoint invalidate pending retries immediately. A closed media/session gate prevents an older asynchronous result from restoring audio after a newer action. Selecting a new device still requires an explicit next Join; automatic recovery does not treat a new selection as permission to swap microphones mid-call.

Choose **Remember me — save in macOS Keychain** before QR login to reconnect on future launches. A successful save enables one startup connection attempt; it does not join voice. Settings shows whether saving succeeded. Denied/canceled Keychain access leaves the current successful login usable and shows a save failure. A network failure preserves the saved credential; a rejected login disables startup and attempts to forget it. Explicit Log out disables startup and attempts Keychain deletion even if either operation fails. Nonsecret appearance, processing, shortcut, notification/update opt-ins and bounded participant volumes are stored separately from credentials in a small preferences JSON file. On macOS this is in Application Support; Windows uses AppData and Linux uses the user configuration directory. The bundle identifier (`me.uzgoren.fastdistord`) and Keychain service/account (`fastdistord.personal-account` / `default`) are stable across versioned app names. These local builds are ad-hoc signed; macOS may ask for Keychain access again after an update. Approve or cancel that prompt yourself; no permission bypass is included.

A QR session-exchange HTTP 400 is not a confirmed rate limit. The app displays only validated numeric Discord codes and detected challenge/MFA flags, never raw authentication responses. Unsupported challenges stop login and require the official client. HTTP 429 shows a supplied retry duration and disables the QR button for that interval within this launch; no automatic authentication retry occurs. An unknown failure has no invented cooldown.

## Appearance and updates

Settings offers compact or comfortable message spacing, text size from 12 to 20, and dark or light colors. Preferences apply locally and survive logout. Images load only when visible and use bounded caches with expiry for failed requests.

Update checking is off by default, never installs code, and does not publish releases. The current public repository supports anonymous release checks. If access changes or the check is unavailable, use the offered releases link. Distribution still requires the signing prerequisites in [Signing](SIGNING.md).
