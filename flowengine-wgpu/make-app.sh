#!/usr/bin/env bash
# Build flowengine as a macOS .app bundle so it can obtain Microphone permission.
#
# Why: a bare `cargo run` binary is attributed to the launching Terminal for TCC, and
# without an Info.plist declaring NSMicrophoneUsageDescription macOS silently feeds
# SILENCE (all-zero samples) — the visual runs but never reacts, with no error. Launching
# a signed .app via LaunchServices gives it its own TCC identity, so the "allow the
# microphone" prompt appears; click Allow once and the mic works from then on.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release

APP="dist/flowengine.app"
mkdir -p "$APP/Contents/MacOS"
cp target/release/flowengine "$APP/Contents/MacOS/flowengine"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>flowengine</string>
  <key>CFBundleIdentifier</key><string>com.soulbrews.flowengine</string>
  <key>CFBundleName</key><string>flowengine</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>flowengine turns your voice into a live audio-reactive visual.</string>
</dict>
</plist>
PLIST

# ad-hoc sign so TCC can attribute the grant to a stable bundle identity
codesign --force --deep -s - "$APP"

echo "built $APP"
echo "launching — click Allow on the microphone prompt, then speak"
open "$APP"
