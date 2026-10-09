Fastdistord 0.02 includes the shorter Settings labels, About credit ("built with vibes by Mehmet Serhat Uzgoren") and concise documentation, plus all voice and chat fixes from 0.01.

Downloads:
- macOS Apple Silicon: open the DMG and drag Fastdistord into Applications. Requires macOS 13 or later. Ad-hoc signed; not Developer ID signed or notarized.
- Windows x64: unsigned per-user EXE installer with Start menu shortcuts and an uninstaller. Cross-compiled on Mac; Windows runtime testing is pending.

Linux packages are not included. Both installers must identify the same release source commit and version 0.02 (package version 0.0.2).

The release retains required voice encryption, membership checks and mute, deafen and push-to-talk gates. The user confirmed outgoing audibility on an earlier build only. Live incoming audio, microphone quality, two-way calls and hardware recovery still need testing. Use headphones: echo cancellation, noise suppression and automatic gain control are absent. Video and screen sharing remain unfinished.

Personal-account access is unofficial and may lead to account restrictions. The app starts muted and does not record audio or collect telemetry. Enter credentials only locally; never send them in chat.

`SOURCE_COMMIT.txt` identifies the release source. Separate Mac and Windows checksum and verification reports describe checks actually performed and remaining limits. Upload to a draft release, then download and compare the assets before publication. Keep the repository private and preserve v0.01.
