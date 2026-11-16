#!/bin/bash
# Builds a universal (arm64 + x86_64) .app bundle and a .dmg in dist/.
# Needs: rustup targets aarch64-apple-darwin and x86_64-apple-darwin, Xcode command line tools.
# The bundle is ad-hoc signed, not notarized.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Claude Code Session Links"
BUNDLE_ID="io.github.xdflight.claude-code-ln"
BIN=claude-code-ln
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST=dist
APP="$DIST/$APP_NAME.app"
DMG="$DIST/$BIN-$VERSION-macos-universal.dmg"

export MACOSX_DEPLOYMENT_TARGET=11.0
# Keep the builder's home directory (and so their user name) out of the binary.
export RUSTFLAGS="--remap-path-prefix=$HOME=/build ${RUSTFLAGS:-}"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --locked --target "$target"
done

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/$BIN" \
    "target/aarch64-apple-darwin/release/$BIN" "target/x86_64-apple-darwin/release/$BIN"

# icon.png (1024px) -> AppIcon.icns
ICONSET=$(mktemp -d)/AppIcon.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z $size $size packaging/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) packaging/icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key><string>$BIN</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP"

STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -format UDZO -ov "$DMG"
rm -rf "$STAGE"

echo "$DMG"
