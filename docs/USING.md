# Controls and troubleshooting

[Back to README](../README.md) · [Setup](GETTING_STARTED.md) · [Verification](VERIFICATION.md)

## Everyday controls

| Control | Behavior |
|---|---|
| Connect | Explicitly opts into the unofficial adapter and checks the entered account |
| Server/channel | Browses available voice channels; selecting does not join |
| Join | Joins the selected channel and opens audio only after DAVE readiness |
| Mute | Immediately closes outgoing audio; buffered pre-mute samples are invalidated |
| Deafen | Silences receive audio and closes outgoing audio |
| Push to talk | While unmuted, hold Ctrl+Shift+Space or the visible Talk control |
| Settings | Device selection, output volume and account controls |
| Reconnect | Restarts failed signaling with the in-memory credential; explicitly rejoin voice afterward |
| Leave | Ends voice and releases both audio devices |
| Log out | Clears the account and removes a saved macOS Keychain credential |
| Quit | Stops audio and exits, including a hidden active call |

PTT starts disabled. Modifier-only hotkeys are not used. The global shortcut is registered only while PTT is enabled. If the OS backend cannot register it, the app reports that and the visible hold control remains available. Native Wayland has no global shortcut support in this backend. Focus/visibility changes release pending talk state; uncertain input fails muted.

## Text channels

Choose **Text**, select a server and text channel, then read its latest 50 messages. **Refresh** fetches a new snapshot; history does not update automatically. The voice controls remain available during text use.

Type plain text and press **Send**. Enter adds a line rather than sending. Messages are limited to 2000 characters. The app suppresses mentions, text-to-speech and link embeds. Attachments, threads, reactions, edits and direct messages are not supported. Empty-content messages display a placeholder; they may contain unsupported attachments or system content.

Sending disables server/channel changes until the response arrives. A failed send keeps the draft. If its outcome is uncertain, refresh history before retrying to avoid duplicates. Switching text channels or servers clears the unsent draft. Logout cancels pending text work and clears displayed history. Channel changes, successful sends and logout also erase the editor undo history, so Undo cannot restore a draft from an earlier scope.

## Devices and background use

Choose stable device IDs through their friendly names in Settings. Selections apply on your next Join: leave and rejoin to change devices safely. Removed devices are labeled unavailable. Idle/disconnected mode never captures audio. A device failure closes this audio engine immediately. Recovery retries only the same pinned microphone and speaker IDs with bounded backoff. It never substitutes another device; exhausted retries leave an actionable failure. OS-managed virtual devices can change their underlying physical route outside the app, which still needs hardware testing.

Closing a window keeps an active call running only when a usable tray/menu-bar path exists. Reopen from the tray; choose Quit to exit. Without a functioning tray, ordinary window close exits so the process does not become inaccessible. Audio and global PTT are independent of rendered UI frames.

## Common problems

- **Account denied:** the unofficial adapter may not be accepted for your account. No MFA/CAPTCHA/protection bypass is provided. Do not repeatedly retry; use official Discord if this route is unavailable.
- **Rate limited:** stop and wait before retrying. There is no automatic bypass or alternate account attempt.
- **Signaling connected, voice not ready:** this is not an active call. DAVE must complete; an incompatible/failed session times out with the microphone closed.
- **Microphone or speaker unavailable:** recovery retries the same authorized device IDs; it does not fall back to a different microphone or speaker. Check OS permissions, connected devices and selected route. On macOS, launch the `.app` containing its microphone usage description. Leave, select a valid device and rejoin.
- **Silent while PTT is enabled:** both explicit unmute and a held talk control are necessary. Server mute/deafen/suppression remains authoritative.
- **Moved, kicked, or permissions changed:** audio stops. Check access and explicitly choose/join a channel; the app does not undo the server action.
- **Network interrupted:** a resumable session is retried with bounded backoff. Missed events are replayed before voice restoration, and any kick, move or permission revocation cancels restoration. No automatic fresh login or channel-rejoin command is sent. Explicit mute is preserved; PTT returns released/unknown. If the session cannot safely resume or retries are exhausted, choose Reconnect and explicitly Join.
- **Echo or poor speakerphone quality:** use headphones. No AEC/noise suppression/AGC is included.
- **Unknown/missing participant names:** initial voice snapshots and later updates are supported, but undocumented personal Gateway payloads vary. Unknown identities may display a user ID; this needs live interoperability testing.
- **Mac security warning:** this project does not ship a notarized download. Do not bypass browser security warnings or trust unexpected third-party binaries. Build locally from reviewed source; ask the maintainer about signing before distribution.

## Privacy and stored data

No raw audio is written to disk. No telemetry is included. Text history and drafts stay in memory; the app sends text only when you press Send. Credentials are memory-only unless you explicitly select macOS Keychain storage. Linux has no plaintext persistence fallback. The Keychain service is `fastdistord.personal-account`; logout removes it. If deletion fails, the app tells you to remove it in Keychain Access.

The app contacts Discord's HTTPS API, account Gateway, negotiated Discord voice endpoint and UDP voice server. No project-operated relay or fallback service is used. Dependencies' debug/trace logging is compiled out to prevent sensitive cryptographic diagnostic events.

For bug reports, include platform, app commit, expected/actual behavior, steps and a screenshot with personal information removed. Never include credentials, raw Gateway payloads, session IDs, voice tokens or recordings.

## Recovery boundaries

Transient recovery uses five attempts with1/2/4/8/16-second delays. A retry budget resets only after60seconds of stable success, preventing endless rapid failure loops. Each session-resume attempt has a deadline; authentication failures, invalid sessions, rate limits, unknown/ambiguous disconnects and DAVE failures require explicit action. Voice restoration requires the previously authorized same guild/channel/session and the same pinned devices. Fresh server endpoint updates replace cached voice credentials; cancellation clears them.

Leave, Log out, switching accounts/channels, server move/kick, permission changes and a removed voice endpoint invalidate pending retries immediately. A closed media/session gate prevents an older asynchronous result from restoring audio after a newer action. Selecting a new device still requires an explicit next Join; automatic recovery does not treat a new selection as permission to swap microphones mid-call.
