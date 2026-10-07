#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo 'Run on macOS with Xcode command-line tools.' >&2; exit 1; }
TARGET=${TARGET:-aarch64-apple-darwin}
cargo build --locked --release --target "$TARGET"
APP="dist/Fastdistord.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/fastdistord" "$APP/Contents/MacOS/fastdistord"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Fastdistord</string>
<key>CFBundleDisplayName</key><string>Fastdistord</string>
<key>CFBundleIdentifier</key><string>me.uzgoren.fastdistord</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleExecutable</key><string>fastdistord</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSMicrophoneUsageDescription</key><string>Fastdistord uses your microphone only after you join a voice channel. Leave or quit to release it.</string>
</dict></plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"
# Local ad-hoc signature only. Not notarized or distributed as a signed release.
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
printf 'Built %s (local ad-hoc signature; no notarization).\n' "$APP"
