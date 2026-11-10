#!/usr/bin/env bash
#
# make-icon.sh: draw the app icon and pack it as assets/EigenRadio.icns.
#
# Only needed when the icon's design (scripts/icon.swift) changes. The result is
# committed, so packaging the app does not need to run this.
#
# Usage: scripts/make-icon.sh
#
set -euo pipefail
cd "$(dirname "$0")/.."

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/EigenRadio.iconset"
mkdir -p "$ICONSET"

echo "==> Drawing"
swift scripts/icon.swift "$WORK/icon-1024.png"

echo "==> Resizing"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$WORK/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$WORK/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done

echo "==> Packing"
iconutil --convert icns "$ICONSET" --output assets/EigenRadio.icns
echo "==> Done: assets/EigenRadio.icns"
