#!/usr/bin/env bash
# Render icon.svg into the PNG and .icns files the app uses.
# Needs rsvg-convert (librsvg) and, for the .icns, png2icns (icnsutils) or
# iconutil (macOS).
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=assets/icon
mkdir -p "$OUT"
for s in 16 32 64 128 256 512 1024; do
  rsvg-convert -w "$s" -h "$s" icon.svg -o "$OUT/icon-$s.png"
done
if command -v iconutil >/dev/null; then
  SET="$(mktemp -d)/AppIcon.iconset"
  mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    cp "$OUT/icon-$s.png" "$SET/icon_${s}x${s}.png"
    cp "$OUT/icon-$((s * 2)).png" "$SET/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$SET" -o "$OUT/AppIcon.icns"
else
  png2icns "$OUT/AppIcon.icns" "$OUT"/icon-{16,32,128,256,512,1024}.png >/dev/null
fi
echo "Rendered $OUT"
