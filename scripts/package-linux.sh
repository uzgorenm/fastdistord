#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Linux ]
BINARY=target/release/fastdistord
"$BINARY" --version | grep -Fx 'fastdistord 0.01'
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM
install -Dm755 "$BINARY" "$STAGE/usr/bin/fastdistord"
install -Dm644 packaging/linux/fastdistord.desktop "$STAGE/usr/share/applications/fastdistord.desktop"
install -Dm644 LICENSE "$STAGE/usr/share/doc/fastdistord/copyright"
mkdir -p "$STAGE/DEBIAN" dist
[ -f dist/third-party/NOTICE_COLLECTION_COMPLETE ] || python3 scripts/collect-notices.py
cp -R dist/third-party "$STAGE/usr/share/doc/fastdistord/third-party"
# Derive runtime dependencies from the actual linked release executable.
SOURCE_DIR=$(pwd)
mkdir -p "$STAGE/debian"
cat > "$STAGE/debian/control" <<'CONTROL'
Source: fastdistord
Section: net
Priority: optional
Maintainer: Mehmet Uzgoren <89664350+uzgorenm@users.noreply.github.com>

Package: fastdistord
Architecture: amd64
Description: Native Discord voice and text client
CONTROL
DEPS=$(cd "$STAGE"; dpkg-shlibdeps -O -e"$SOURCE_DIR/$BINARY" | sed 's/^shlibs:Depends=//')
rm -rf "$STAGE/debian"
[ -n "$DEPS" ]
cat > "$STAGE/DEBIAN/control" <<EOF
Package: fastdistord
Version: 0.0.1
Architecture: amd64
Maintainer: Mehmet Uzgoren <89664350+uzgorenm@users.noreply.github.com>
Depends: $DEPS, libx11-6, libxkbcommon0, libwayland-client0, libgl1
Section: net
Priority: optional
Description: Native Discord voice and text client
 Fastdistord 0.01. Unofficial personal-account access; live calls unverified.
EOF
(cd "$STAGE"; find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums)
dpkg-deb --root-owner-group --build "$STAGE" dist/fastdistord-0.01-linux-amd64.deb
dpkg-deb --info dist/fastdistord-0.01-linux-amd64.deb
dpkg-deb --contents dist/fastdistord-0.01-linux-amd64.deb
sha256sum dist/fastdistord-0.01-linux-amd64.deb
