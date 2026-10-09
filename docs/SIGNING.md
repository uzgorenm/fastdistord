# Signing and updates

Run `sh scripts/check-signing-readiness.sh` for a read-only check. It reports tool
availability and the count of valid macOS Developer ID Application identities.
It does not print identity names, export keys, inspect credentials, sign files,
or submit anything to Apple.

## macOS distribution

The bundle script currently produces a local ad-hoc signature. Distribution
signing needs an Apple Developer Program team, a valid **Developer ID
Application** certificate and its usable private key in an owner-approved
signing setup. The owner must separately authorize access for notarization.
Finding a certificate does not verify that signing or notarization works.

Before a signed release, configure hardened runtime and the microphone input
entitlement, preserve `me.uzgoren.fastdistord` as the bundle identity, sign nested
code before the app, and include a secure timestamp. Verify the resulting app,
submit the distribution archive with `notarytool`, wait for acceptance, then
staple and validate the ticket. Test installation, microphone authorization and
notification authorization on a separate Mac. Do not replace the current ad-hoc
packaging behavior until this setup is approved and tested.

Apple documents [Developer ID signing and notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## Windows distribution

Authenticode needs a trusted code-signing certificate with access to its private
key through an owner-approved certificate store, hardware token, or signing
service. A cross-compiler and NSIS do not supply that identity. Use a supported
signing tool, SHA-256 and an approved timestamp service; sign the application
before packaging, and sign the generated uninstaller and final installer. Verify
signatures and install/uninstall behavior on Windows. This repository does not
enroll in a service, store signing secrets, or claim SmartScreen reputation.

Microsoft documents [SignTool](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool)
and [Authenticode timestamps](https://learn.microsoft.com/en-us/windows/win32/seccrypto/time-stamping-authenticode-signatures).

## Update and notification behavior

Update checks are manual unless the user enables a startup check. They fetch a
bounded list of published releases, including previews, from the fixed GitHub
repository. They do not download or install software. Anonymous checks cannot
read a private repository's releases; the app offers the
[releases page](https://github.com/uzgorenm/fastdistord/releases) for the user's
signed-in browser. The check never borrows Discord or browser credentials.
[GitHub documents release access](https://docs.github.com/rest/releases/releases).

macOS incoming-call notifications require an explicit permission request and a
separate enabled preference. The packaged app submits a generic alert without
caller names, channel identifiers, sounds or badges. The in-app call prompt owns
answer/decline and ringtone behavior. macOS decides whether to display the alert;
Focus mode or OS settings can suppress it. Other platforms currently use the
in-app prompt. Permission prompts and delivery require manual testing from the
packaged app; offline tests use a fake notification backend.
[Apple documents notification permission](https://developer.apple.com/documentation/usernotifications/asking-permission-to-use-notifications).
