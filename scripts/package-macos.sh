#!/usr/bin/env bash
#
# package-macos.sh: build EigenRadio.app, a double-clickable Mac app.
#
# It makes a release build, assembles the .app bundle by hand (cargo does not produce
# one) and signs it ad hoc, which needs no Apple developer account. It needs only Rust
# and the Xcode command line tools.
#
# The app is built for the kind of Mac this runs on (Apple Silicon or Intel), not both.
#
# Usage:  scripts/package-macos.sh
#         open dist/EigenRadio.app
#
# The zip in dist/ is the thing to hand to someone. Because the app is not notarised by
# Apple, they will be told it cannot be verified the first time they open it, and must
# allow it once under System Settings > Privacy & Security > Open Anyway.
#
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="EigenRadio"
BUNDLE_ID="ohffs.EigenRadio"
# Rust's own floor for Apple Silicon. Nothing older has been tried.
MIN_MACOS="11.0"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
APP="dist/${APP_NAME}.app"
ZIP="dist/${APP_NAME}-${VERSION}-macos-$(uname -m).zip"

echo "==> Checking that every dependency is at least a week old"
python3 scripts/lock_ages.py

echo "==> Building ${APP_NAME} ${VERSION}"
cargo build --release --locked

echo "==> Assembling ${APP}"
rm -rf "$APP" "$ZIP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/eigenrust "$APP/Contents/MacOS/$APP_NAME"
cp assets/EigenRadio.icns "$APP/Contents/Resources/$APP_NAME.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIconFile</key>
    <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>${MIN_MACOS}</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.music</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

# Without at least an ad hoc signature, Apple Silicon Macs call a downloaded app "damaged".
echo "==> Signing ad hoc"
codesign --force --deep --sign - "$APP"
codesign --verify --strict "$APP"

echo "==> Zipping"
ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> Done"
echo "    $APP"
echo "    $ZIP"
