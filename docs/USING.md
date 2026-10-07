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
| Settings | Device selection, output volume, audio diagnostics and risk information |
| Reconnect | Restarts failed signaling with the in-memory credential; explicitly rejoin voice afterward |
| Leave | Ends voice and releases both audio devices |
| Log out | Clears the account and removes a saved macOS Keychain credential |
| Quit | Stops audio and exits, including a hidden active call |

PTT starts disabled. Modifier-only hotkeys are not used. The global shortcut is registered only while PTT is enabled. If the OS backend cannot register it, the app reports that and the visible hold control remains available. Native Wayland has no global shortcut support in this backend. Focus/visibility changes release pending talk state; uncertain input fails muted.

## Devices and background use

Choose stable device IDs through their friendly names in Settings. Selections apply on your next Join: leave and rejoin to change devices safely. Removed devices are labeled unavailable. Idle/disconnected mode never captures audio. Device failure stops the current call rather than silently selecting another microphone.

Closing a window keeps an active call running only when a usable tray/menu-bar path exists. Reopen from the tray; choose Quit to exit. Without a functioning tray, ordinary window close exits so the process does not become inaccessible. Audio and global PTT are independent of rendered UI frames.

## Common problems

- **Account denied:** the unofficial adapter may not be accepted for your account. No MFA/CAPTCHA/protection bypass is provided. Do not repeatedly retry; use official Discord if this route is unavailable.
- **Rate limited:** stop and wait before retrying. There is no automatic bypass or alternate account attempt.
- **Signaling connected, voice not ready:** this is not an active call. DAVE must complete; an incompatible/failed session times out with the microphone closed.
- **Microphone or speaker unavailable:** check OS permissions, connected devices and selected route. On macOS, launch the `.app` containing its microphone usage description. Leave, select a valid device and rejoin.
- **Silent while PTT is enabled:** both explicit unmute and a held talk control are necessary. Server mute/deafen/suppression remains authoritative.
- **Moved, kicked, or permissions changed:** audio stops. Check access and explicitly choose/join a channel; the app does not undo the server action.
- **Network interrupted:** choose Reconnect, wait for signaling, and Join again. It does not silently resume microphone transmission.
- **Echo or poor speakerphone quality:** use headphones. No AEC/noise suppression/AGC is included.
- **Unknown/missing participant names:** initial voice snapshots and later updates are supported, but undocumented personal Gateway payloads vary. Unknown identities may display a user ID; this needs live interoperability testing.
- **Mac security warning:** this project does not ship a notarized download. Do not bypass browser security warnings or trust unexpected third-party binaries. Build locally from reviewed source; ask the maintainer about signing before distribution.

## Privacy and stored data

No raw audio is written to disk. No telemetry or text-chat feature is included. Credentials are memory-only unless you explicitly select macOS Keychain storage. Linux has no plaintext persistence fallback. The Keychain service is `fastdistord.personal-account`; logout removes it. If deletion fails, the app tells you to remove it in Keychain Access.

The app contacts Discord's HTTPS API, account Gateway, negotiated Discord voice endpoint and UDP voice server. No project-operated relay or fallback service is used. Dependencies' debug/trace logging is compiled out to prevent sensitive cryptographic diagnostic events.

For bug reports, include platform, app commit, expected/actual behavior, steps and a screenshot with personal information removed. Never include credentials, raw Gateway payloads, session IDs, voice tokens or recordings.
