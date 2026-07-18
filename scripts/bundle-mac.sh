#!/usr/bin/env bash
# Build Vouch.app (and Vouch.zip beside it) for macOS — the minimal
# version of Zed's script/bundle-mac.
#
# Signing: with MACOS_CERTIFICATE (base64 .p12), MACOS_CERTIFICATE_PASSWORD,
# APPLE_NOTARIZATION_APPLE_ID, APPLE_NOTARIZATION_TEAM_ID, and
# APPLE_NOTARIZATION_PASSWORD (an app-specific password) set, the bundle is
# Developer-ID signed and notarized — required for a downloaded app to open
# without Gatekeeper blocking it. Without them it falls back to an ad-hoc
# signature: fine locally, but friends will need right-click → Open.
#
# VOUCH_BUILD_STAMP (YYYYMMDDHHMMSS, UTC) bakes the nightly build identity
# into the binary; the in-app updater compares it against the published
# nightly to know when it's out of date. Unset = dev build, updater off.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

STAMP="${VOUCH_BUILD_STAMP:-}"
echo "==> building release binary (stamp: ${STAMP:-dev})"
VOUCH_BUILD_STAMP="$STAMP" cargo build --release --package vouch

APP_DIR="target/release/Vouch.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp target/release/vouch "$APP_DIR/Contents/MacOS/vouch"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>online.vouch-app.Vouch</string>
    <key>CFBundleName</key><string>Vouch</string>
    <key>CFBundleExecutable</key><string>vouch</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0${STAMP:++nightly.$STAMP}</string>
    <key>CFBundleVersion</key><string>${STAMP:-0}</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [[ -n "${MACOS_CERTIFICATE:-}" ]]; then
  echo "==> signing with Developer ID"
  KEYCHAIN="vouch-build.keychain"
  KEYCHAIN_PASSWORD="$(openssl rand -hex 16)"
  echo "$MACOS_CERTIFICATE" | base64 --decode > /tmp/vouch-cert.p12
  security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
  security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
  security import /tmp/vouch-cert.p12 -k "$KEYCHAIN" \
    -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
  security set-key-partition-list -S apple-tool:,apple: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" > /dev/null
  security list-keychains -d user -s "$KEYCHAIN" login.keychain
  rm /tmp/vouch-cert.p12
  IDENTITY="$(security find-identity -v -p codesigning "$KEYCHAIN" | grep -m1 -o '"[^"]*"' | tr -d '"')"
  codesign --force --deep --options runtime --timestamp \
    --sign "$IDENTITY" --keychain "$KEYCHAIN" "$APP_DIR"

  echo "==> notarizing"
  ditto -c -k --keepParent "$APP_DIR" target/release/Vouch-notarize.zip
  xcrun notarytool submit target/release/Vouch-notarize.zip --wait \
    --apple-id "$APPLE_NOTARIZATION_APPLE_ID" \
    --team-id "$APPLE_NOTARIZATION_TEAM_ID" \
    --password "$APPLE_NOTARIZATION_PASSWORD"
  xcrun stapler staple "$APP_DIR"
  rm -f target/release/Vouch-notarize.zip
  security delete-keychain "$KEYCHAIN"
else
  echo "==> no signing secrets: ad-hoc signature (downloads will hit Gatekeeper)"
  codesign --force --deep --sign - "$APP_DIR"
fi

echo "==> zipping"
rm -f target/release/Vouch.zip
ditto -c -k --keepParent "$APP_DIR" target/release/Vouch.zip
echo "==> done: $APP_DIR and target/release/Vouch.zip"
