#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
VERSION=0.01
APP=dist/latest/Fastdistord.app
[ -d "$APP" ] || { echo 'Build the Mac bundle first.' >&2; exit 1; }
[ "$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")" = 0.0.1 ]
codesign --verify --deep --strict "$APP"
"$APP/Contents/MacOS/fastdistord" --version | grep -Fx "fastdistord $VERSION"
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/fastdistord-dmg.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM
cp -R "$APP" "$STAGE/Fastdistord.app"
cp LICENSE "$STAGE/LICENSE.txt"
ln -s /Applications "$STAGE/Applications"
hdiutil create -ov -volname "Fastdistord $VERSION" -srcfolder "$STAGE" -format UDZO "dist/Fastdistord-$VERSION-macos-arm64.dmg"
hdiutil verify "dist/Fastdistord-$VERSION-macos-arm64.dmg"
shasum -a 256 "dist/Fastdistord-$VERSION-macos-arm64.dmg"
