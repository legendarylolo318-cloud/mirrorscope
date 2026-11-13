#!/usr/bin/env bash
# Build mirrorscope-<version>-x86_64.AppImage (+ .sha256) for Arch Linux.
#
# Usage: ./build-appimage.sh            # uses $APPIMAGETOOL, PATH, or downloads a pinned copy
#        APPIMAGETOOL=/path/to/appimagetool ./build-appimage.sh
#
# Layout notes:
# - pkexec runs the helper as root, and root cannot read the user's AppImage
#   FUSE mount. The helper and polkit policy are therefore bundled only as a
#   payload for `mirrorscope --install-helper`, which installs them to
#   /usr/lib/mirrorscope/ and /usr/share/polkit-1/actions/ (one polkit prompt).
# - Only glibc and fontconfig are linked; X11/Wayland/xkbcommon are dlopened.
#   All of these are present on any Arch desktop, so no libraries are bundled.
set -euo pipefail
cd "$(dirname "$0")"

ARCH=x86_64
APPIMAGETOOL_VERSION=1.9.1
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "cannot determine version from Cargo.toml" >&2; exit 1; }
OUT="mirrorscope-${VERSION}-${ARCH}.AppImage"
mkdir -p dist

# --- appimagetool -----------------------------------------------------------
find_appimagetool() {
    if [ -n "${APPIMAGETOOL:-}" ]; then echo "$APPIMAGETOOL"; return; fi
    if command -v appimagetool >/dev/null 2>&1; then command -v appimagetool; return; fi
    local cache="${XDG_CACHE_HOME:-$HOME/.cache}/mirrorscope-build"
    local tool="$cache/appimagetool-${APPIMAGETOOL_VERSION}-${ARCH}.AppImage"
    if [ ! -x "$tool" ]; then
        mkdir -p "$cache"
        echo "Downloading appimagetool ${APPIMAGETOOL_VERSION}..." >&2
        curl -fsSL -o "$tool.part" \
            "https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-${ARCH}.AppImage"
        echo "${APPIMAGETOOL_SHA256}  $tool.part" | sha256sum -c --quiet - >&2
        chmod +x "$tool.part"
        mv "$tool.part" "$tool"
    fi
    echo "$tool"
}
TOOL=$(find_appimagetool)

# --- build ------------------------------------------------------------------
cargo build --release --locked --workspace

# --- AppDir -----------------------------------------------------------------
APPDIR=AppDir
rm -rf "$APPDIR"
install -Dm755 target/release/mirrorscope        "$APPDIR/usr/bin/mirrorscope"
install -Dm755 target/release/mirrorscope-helper "$APPDIR/usr/lib/mirrorscope/mirrorscope-helper"
install -Dm644 packaging/polkit/io.github.mirrorscope.policy \
    "$APPDIR/usr/share/polkit-1/actions/io.github.mirrorscope.policy"
install -Dm644 packaging/mirrorscope.desktop     "$APPDIR/usr/share/applications/mirrorscope.desktop"
install -Dm644 packaging/icons/mirrorscope.svg   "$APPDIR/usr/share/icons/hicolor/scalable/apps/mirrorscope.svg"
install -Dm644 packaging/mirrorscope.metainfo.xml \
    "$APPDIR/usr/share/metainfo/io.github.mirrorscope.Mirrorscope.metainfo.xml"

# AppImage root entries
cp packaging/mirrorscope.desktop "$APPDIR/mirrorscope.desktop"
cp packaging/icons/mirrorscope.svg "$APPDIR/mirrorscope.svg"
ln -sf mirrorscope.svg "$APPDIR/.DirIcon"
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
# --cli and --install-helper are handled by the binary itself.
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/usr/bin/mirrorscope" "$@"
EOF
chmod 755 "$APPDIR/AppRun"

# --- package ----------------------------------------------------------------
rm -f "dist/$OUT" "dist/$OUT.sha256"
# --appimage-extract-and-run lets this work in containers without FUSE (CI).
ARCH=$ARCH "$TOOL" --appimage-extract-and-run --no-appstream "$APPDIR" "dist/$OUT"
(cd dist && sha256sum "$OUT" > "$OUT.sha256")

echo
echo "Built dist/$OUT ($(du -h "dist/$OUT" | cut -f1))"
cat "dist/$OUT.sha256"
