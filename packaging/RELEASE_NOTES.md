Fastdistord 0.01 provides native Discord voice controls and text channels, with fresh mobile-approved QR login and optional macOS Keychain storage.

Downloads:
- macOS Apple Silicon: open the DMG and drag Fastdistord into Applications. Ad-hoc signed; not Developer ID signed or notarized.
- Windows x64: per-user EXE installer with Start menu shortcuts and an uninstaller. Not Authenticode signed.
- Linux amd64: Debian package built on Ubuntu 24.04. Install with `sudo apt install ./fastdistord-0.01-linux-amd64.deb`; requires a compatible Debian/Ubuntu system and GUI/audio libraries.

Package verification uses local native build hosts; only installers actually built and checked are published. This does not establish native GUI, hardware or live Discord behavior. Voice/text still need live testing. Video/screen-share sending is unfinished; standalone media previews and production diagnostics are not included.

Personal-account access is unofficial and may lead to account restrictions. The app starts muted and does not record audio or collect telemetry. Enter credentials only locally; never send them in chat.

`SOURCE_COMMIT.txt` identifies the exact source commit. `SHA256SUMS` covers the installers and source identity. Assets must be uploaded and downloaded for byte comparison before publication. Repository visibility remains private.
