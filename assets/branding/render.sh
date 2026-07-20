#!/usr/bin/env bash
# Regenerate every raster asset from the SVG masters (the source of truth).
# Tools: cairosvg (SVG→PNG) + Pillow (PNG→ICO/ICNS). No runtime dependency.
set -eu
cd "$(dirname "$0")"
mkdir -p png previews

# PNG ladder from the icon: small-optimized art for 16/32, full icon for 48+.
for s in 48 64 128 256 512 1024; do
  cairosvg tazamun.svg -o "png/tazamun-$s.png" --output-width "$s" --output-height "$s"
done
for s in 16 32; do
  cairosvg tazamun-icon-16.svg -o "png/tazamun-$s.png" --output-width "$s" --output-height "$s"
done

# Wordmark PNG for docs/README (transparent; reads on light backgrounds).
cairosvg tazamun-wordmark.svg -o png/tazamun-wordmark-1024.png --output-width 1024

# Multi-resolution .ico (Windows) + .icns (macOS) from the PNGs.
python3 - <<'PY'
from PIL import Image
base = Image.open('png/tazamun-256.png').convert('RGBA')
extra = [Image.open(f'png/tazamun-{s}.png').convert('RGBA') for s in (16, 32, 48, 64, 128)]
base.save('tazamun.ico', format='ICO', append_images=extra)
Image.open('png/tazamun-1024.png').convert('RGBA').save('tazamun.icns')
print('wrote tazamun.ico + tazamun.icns')
PY
echo "branding assets regenerated"
