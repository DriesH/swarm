#!/bin/sh
# Builds build/SwarmMenubarSpike.app: GPUI only enables notifications inside an app bundle.
# Usage: ./bundle.sh [extra cargo args, e.g. --features snapshot]
set -eu
cd "$(dirname "$0")"

cargo build --release "$@"

app=build/SwarmMenubarSpike.app
mkdir -p "$app/Contents/MacOS"
cp target/release/swarm-menubar-spike "$app/Contents/MacOS/swarm-menubar-spike"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID:-dev.swarm.spike.menubar}</string>
  <key>CFBundleName</key><string>Swarm Spike</string>
  <key>CFBundleDisplayName</key><string>Swarm Spike</string>
  <key>CFBundleExecutable</key><string>swarm-menubar-spike</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <!-- Default style for new apps: alerts stay on screen and show their buttons. -->
  <key>NSUserNotificationAlertStyle</key><string>alert</string>
</dict>
</plist>
PLIST

# Ad-hoc signature: enough for local runs. Distribution needs Developer ID + notarization.
codesign --force --sign - --timestamp=none "$app"
codesign --verify --verbose=1 "$app"
echo "built $app"
