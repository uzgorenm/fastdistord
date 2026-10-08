# Sign-in options

Fastdistord needs functional channel access, not a profile-only login presented as a connected Discord client. Account identity, service capabilities and local device permissions must be tracked separately.

## Supported browser sign-in

Discord's [Social SDK account-linking guide](https://docs.discord.com/developers/discord-social-sdk/development-guides/account-linking-with-discord) documents public-client PKCE, loopback callbacks and a browser fallback. This requires a registered application/client ID and an accepted callback configuration. Fastdistord has not registered or tested those prerequisites.

Ordinary `identify`/`guilds` grants identify an account and list basic servers. They do not authorize this adapter's message history/send or arbitrary guild voice. The [voice OAuth scope](https://docs.discord.com/developers/topics/oauth2#oauth2-scopes) requires approved-partner access. SDK lobby communications are a separate integration, not a replacement for ordinary server calls.

A future supported identity flow can use a system browser, fresh PKCE/state values, a bounded loopback listener and explicit capability checks. No client secret belongs in a distributed binary. Identity-only login must leave unsupported chat/voice disabled and explain the missing authorization. This is a design option, not an implemented or granted login.

## Fresh session with mobile approval

The user-supplied [reverse-engineered desktop remote-auth description](https://docs.discord.food/remote-authentication/desktop) describes an ephemeral RSA-OAEP exchange, fingerprint-bound QR and explicit mobile approval leading to an encrypted personal session credential. It is not official OAuth/device-code authorization. It carries broad account access and the same personal-client policy/maintenance risk as the existing adapter.

The documented service requires a Discord web Origin header. A native implementation claiming that origin is not a verified supported route; this project will not bypass origin restrictions or relay credentials. Source-level feasibility does not establish accepted or secure interoperability. No session, QR, application registration or grant was created during investigation.

Any future authorized investigation must keep session establishment behind an explicit start, hand off mobile/account approval to the user, enforce fingerprint/expiry/cancellation, avoid logging tickets or account payloads, erase ephemeral keys, and make Keychain saving separately optional. These are proposed requirements, not a claim that the flow is implemented.

## Local devices

OS microphone/camera/screen permission only authorizes local capture. It cannot grant Discord voice, video or message access. Version 0.01 retains explicit local credential entry and Keychain reconnect; it does not add a placeholder OAuth button or credential extraction from installed apps/browsers.
