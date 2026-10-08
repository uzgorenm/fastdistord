#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo 'Run on macOS with Xcode command-line tools.' >&2; exit 1; }
TARGET=${TARGET:-aarch64-apple-darwin}
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
export FASTDISTORD_BUILD_COMMIT=$(git rev-parse HEAD)
if [ "$HOST_TARGET" = "$TARGET" ]; then
    cargo build --locked --release
    BINARY="target/release/fastdistord"
else
    cargo build --locked --release --target "$TARGET"
    BINARY="target/$TARGET/release/fastdistord"
fi
APP="dist/latest/Fastdistord.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/fastdistord"
[ -f dist/third-party/NOTICE_COLLECTION_COMPLETE ] || python3 scripts/collect-notices.py
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
cp -R dist/third-party "$APP/Contents/Resources/third-party"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Fastdistord</string>
<key>CFBundleDisplayName</key><string>Fastdistord</string>
<key>CFBundleIdentifier</key><string>me.uzgoren.fastdistord</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.0.1</string>
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
