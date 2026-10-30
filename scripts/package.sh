#!/bin/sh
# Makes this system's download in dist/:
#   macOS:   wave9000-<v>-macos-universal.zip (Apple Silicon and Intel)
#   Windows: wave9000-<v>-windows-<x64|arm64>.zip
#   Linux:   wave9000-<v>-linux-<x64|arm64>.tar.gz, with install.sh
# Usage: scripts/package.sh
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
mkdir -p dist

# What cargo builds for, not what the shell says: on Arm Windows, Git Bash
# is an x64 program and reports x64.
arch() {
  case "$(rustc -vV | sed -n 's/^host: //p')" in
    aarch64*) echo arm64 ;;
    *) echo x64 ;;
  esac
}

case "$(uname -s)" in
  Linux)
    cargo build --release
    d=wave9000-$VERSION-linux-$(arch)
    rm -rf target/stage && mkdir -p "target/stage/$d"
    cp target/release/wave9000 README.md LICENSE "target/stage/$d/"
    cp packaging/linux/install.sh packaging/linux/wave9000.desktop "target/stage/$d/"
    cp assets/icon-256.png "target/stage/$d/wave9000.png"
    tar -czf "dist/$d.tar.gz" -C target/stage "$d"
    ls -la dist
    exit 0
    ;;
  MINGW*|MSYS*|CYGWIN*)
    cargo build --release
    rm -rf target/stage && mkdir -p target/stage
    cp target/release/wave9000.exe README.md LICENSE target/stage/
    out="dist/wave9000-$VERSION-windows-$(arch).zip"
    rm -f "$out"
    (cd target/stage && python -m zipfile -c "../../$out" wave9000.exe README.md LICENSE)
    ls -la dist
    exit 0
    ;;
esac

# macOS
APP=target/wave9000.app

rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
mkdir -p target/release
lipo -create -output target/release/wave9000-universal \
  target/aarch64-apple-darwin/release/wave9000 target/x86_64-apple-darwin/release/wave9000

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/wave9000-universal "$APP/Contents/MacOS/wave9000"

# The 1024px PNG as an .icns with every size macOS asks for.
ICONSET=target/wave9000.iconset
rm -rf "$ICONSET" && mkdir "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/wave9000.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>wave9000</string>
  <key>CFBundleDisplayName</key><string>wave9000</string>
  <key>CFBundleIdentifier</key><string>app.wave9000.workstation</string>
  <key>CFBundleExecutable</key><string>wave9000</string>
  <key>CFBundleIconFile</key><string>wave9000</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSMicrophoneUsageDescription</key><string>wave9000 records from your microphone or audio interface when you press Record.</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>© 2026 Vishal Mohan · MIT License</string>
  <!-- Its own files open here by double-click; the media only lists it
       under Open With, so no other app's defaults change. -->
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>wave9000 Session</string>
      <key>CFBundleTypeExtensions</key><array><string>wave</string></array>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Owner</string>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>splice9000 Project</string>
      <key>CFBundleTypeExtensions</key><array><string>splice</string></array>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Alternate</string>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>Sound</string>
      <key>CFBundleTypeExtensions</key><array><string>wav</string><string>mp3</string><string>flac</string><string>aiff</string><string>aif</string><string>m4a</string><string>aac</string><string>ogg</string><string>oga</string><string>opus</string><string>wma</string><string>caf</string></array>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Alternate</string>
    </dict>
  </array>
</dict>
</plist>
PLIST


codesign --force --deep --sign - "$APP"
rm -f "dist/wave9000-$VERSION-macos-universal.zip"
# ditto keeps the signature, permissions and icon intact.
ditto -c -k --sequesterRsrc --keepParent "$APP" "dist/wave9000-$VERSION-macos-universal.zip"
ls -la dist
