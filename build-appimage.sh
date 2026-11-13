#!/usr/bin/env bash
# STUB: the next stage finalises this script.
# Builds release binaries and lays out an AppDir. Packaging with linuxdeploy /
# appimagetool is left as TODO.
#
# Known issue to solve in the final stage: pkexec runs the helper as root, and
# root cannot read an AppImage's FUSE mount. The helper must be installed to
# /usr/lib/mirrorscope/ (with the polkit policy) outside the AppImage, or be
# copied there on first run.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --workspace
APPDIR=AppDir
rm -rf "$APPDIR"
install -Dm755 target/release/mirrorscope "$APPDIR/usr/bin/mirrorscope"
install -Dm755 target/release/mirrorscope-helper "$APPDIR/usr/lib/mirrorscope/mirrorscope-helper"
install -Dm644 packaging/mirrorscope.desktop "$APPDIR/usr/share/applications/mirrorscope.desktop"
install -Dm644 packaging/icons/mirrorscope.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/mirrorscope.svg"
install -Dm644 packaging/mirrorscope.metainfo.xml "$APPDIR/usr/share/metainfo/io.github.mirrorscope.Mirrorscope.metainfo.xml"

echo "AppDir prepared in $APPDIR."
echo "TODO: run linuxdeploy --appdir $APPDIR --output appimage (or appimagetool $APPDIR)."
