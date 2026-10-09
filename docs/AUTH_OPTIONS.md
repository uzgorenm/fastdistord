# Sign-in options

Fastdistord needs functional channel access, not a profile-only login presented as a connected Discord client. Account identity, service capabilities and local device permissions must be tracked separately.

## Browser sign-in research

Discord's [Social SDK account-linking guide](https://docs.discord.com/developers/discord-social-sdk/development-guides/account-linking-with-discord) documents public-client PKCE, loopback callbacks and a browser fallback. This requires a registered application/client ID and an accepted callback configuration. Fastdistord has not registered or tested those prerequisites. This SDK-specific guide is a technical reference, not approval to use the SDK in this client. [Social SDK Terms §2.b(vii)](https://support-dev.discord.com/hc/en-us/articles/30225844245271-Discord-Social-SDK-Terms) restrict use to develop products that could compete with Discord services; the SDK is not an approved replacement-client route.

Ordinary `identify`/`guilds` grants identify an account and list basic servers. They do not authorize this adapter's message history/send or arbitrary guild voice. The [voice OAuth scope](https://docs.discord.com/developers/topics/oauth2#oauth2-scopes) requires approved-partner access. SDK lobby communications are a separate integration, not a replacement for ordinary server calls.

Any future browser identity flow must first verify the application configuration, eligible APIs and applicable terms. Its design would use a system browser, fresh PKCE/state values, a bounded loopback listener and explicit capability checks. No client secret belongs in a distributed binary. Identity-only login must leave unsupported chat/voice disabled and explain the missing authorization. This is a design option, not an implemented or granted login.

## Fresh session with mobile approval

The user-supplied [reverse-engineered desktop remote-auth description](https://docs.discord.food/remote-authentication/desktop) describes an ephemeral RSA-OAEP exchange, fingerprint-bound QR and explicit mobile approval leading to an encrypted personal session credential. It is not official OAuth/device-code authorization. It carries broad account access and the same personal-client policy/maintenance risk as the existing adapter. It does not guarantee a new or narrowly scoped token. Discord’s [official QR login FAQ](https://support.discord.com/hc/en-us/articles/360039213771-QR-Code-Login-FAQ) describes its own client login, not approval for a third-party implementation.

The native implementation sends the Discord web Origin value required by the reverse-engineered protocol. This is unofficial interoperability, not an OAuth grant or supported Discord integration. It does not embed Discord’s web client, change TLS checks, relay credentials or extract another app’s session.

The explicit Connect action starts a fresh ephemeral RSA-2048 OAEP/SHA-256 handshake. The app verifies the returned fingerprint against its public key before rendering a QR. The user scans and approves on their phone; the app exchanges the resulting ticket for its encrypted session. Protocol order, expiry, heartbeat acknowledgments and bounded messages are checked. Cancel, window closure, disconnect and expiry stop the attempt; a fresh Connect is required to retry. MFA/challenges and rate limits stop the flow rather than being bypassed.

Private key components and owned decrypted nonce/token buffers are erased on drop. Transport/serialization libraries may hold temporary copies; memory erasure is not a guarantee of eliminating every allocator copy. The session is sent to the existing account adapter without logs or chat exposure. Optional Keychain saving is separately opt-in and occurs after account validation. Logout erases local access and saved credentials; it does not prove server-side revocation. Use Discord device/session controls or change the Discord password to end all sessions.

Offline crypto, timing and protocol-order fixtures are tested. A network probe using the app’s Rust transport received Discord’s public hello, then closed without sending init or requesting a QR/session. The observed server lifetime was 308377 ms; the app now caps that to its three-minute local limit instead of rejecting the handshake. Error messages expose only locally defined categories and numeric status/close codes, never server payloads. Native GUI, the remaining remote-auth handshake and mobile approval still require user testing.

## Local devices

OS microphone/camera/screen permission only authorizes local capture. It cannot grant Discord voice, video or message access. Version 0.02 includes QR login, optional existing local credential entry and explicit Keychain reconnect. It does not present OAuth as channel access or extract installed-app/browser credentials.
