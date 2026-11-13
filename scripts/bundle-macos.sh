#!/usr/bin/env bash
# Build Zeble as a portable, universal (Apple Silicon + Intel) macOS app.
#
#   scripts/bundle-macos.sh [output-dir]     # default: ./tmp
#
# Produces <output-dir>/Zeble.app and <output-dir>/Zeble.zip. Run on macOS
# with Rust and the Xcode command line tools installed.
set -euo pipefail

cd "$(dirname "$0")/.."
OUT="${1:-tmp}"
APP="$OUT/Zeble.app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)

if [[ "$(uname)" != "Darwin" ]]; then
  echo "This script must run on macOS." >&2
  exit 1
fi

rustup target add "${TARGETS[@]}"
for t in "${TARGETS[@]}"; do
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release -p zeble-app --target "$t"
done

rm -rf "$APP" "$OUT/Zeble.zip"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/zeble" \
  target/aarch64-apple-darwin/release/zeble target/x86_64-apple-darwin/release/zeble

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Zeble</string>
  <key>CFBundleDisplayName</key><string>Zeble</string>
  <key>CFBundleIdentifier</key><string>dev.zeble.app</string>
  <key>CFBundleExecutable</key><string>zeble</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# Ad-hoc signature: required for Apple Silicon to launch the binary.
codesign --force --deep --sign - "$APP"
ditto -c -k --keepParent "$APP" "$OUT/Zeble.zip"

echo "Built $APP ($(lipo -archs "$APP/Contents/MacOS/zeble"))"
echo "Zipped $OUT/Zeble.zip"
