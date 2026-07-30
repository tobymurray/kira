#!/usr/bin/env bash
#
# Regenerate the site icons from the source artwork.
#
# Requires ImageMagick 7 (`magick`). Outputs are committed, so this only needs
# running when the artwork changes:
#
#   ./tools/make-icons.sh
#
set -euo pipefail

cd "$(dirname "$0")/.."

SRC=assets/kira-mark.png
OUT=site/img

# The source is a 1408x768 render with the mark centred and a generator
# watermark in the bottom-right corner. This crop is the measured bounding box
# of the mark itself, which excludes that watermark -- do not replace it with
# `-trim`, which would include the watermark and produce an 828x508 box.
EXPECT_W=1408
EXPECT_H=768
CROP=440x440+484+164

# Background sampled from the source. Kept opaque on purpose: the mark is white
# and cyan, so a transparent version would vanish against a light background.
BG='#080808'

[ -f "$SRC" ] || { echo "missing source artwork: $SRC" >&2; exit 1; }

# The trailing newline matters: without it `read` sees EOF, returns non-zero and
# `set -e` aborts the script before anything is generated.
read -r w h < <(magick identify -format '%w %h\n' "$SRC")
if [ "$w" != "$EXPECT_W" ] || [ "$h" != "$EXPECT_H" ]; then
  echo "$SRC is ${w}x${h}, expected ${EXPECT_W}x${EXPECT_H}." >&2
  echo "The crop geometry ($CROP) was measured against the original; re-measure" >&2
  echo "the mark's bounding box before trusting this script with new artwork." >&2
  exit 1
fi

mkdir -p "$OUT"

# Master tile: the mark inset inside a square so it has breathing room at small
# sizes, on its own background so it reads on any browser chrome.
magick "$SRC" -crop "$CROP" +repage \
  -resize 424x424 \
  -background "$BG" -gravity center -extent 512x512 \
  "$OUT/mark-512.png"

for size in 192 180 48 32 16; do
  magick "$OUT/mark-512.png" -resize "${size}x${size}" "$OUT/mark-${size}.png"
done

# Multi-resolution .ico for browsers that still ask for one by convention.
magick "$OUT/mark-512.png" -define icon:auto-resize=48,32,16 site/favicon.ico

# Link-preview card at the 1.91:1 ratio scrapers expect. Composed on a fresh
# canvas rather than cropped from the source, so the watermark cannot sneak in.
magick -size 1200x630 "xc:$BG" \
  \( "$OUT/mark-512.png" -resize 360x360 \) -gravity center -composite \
  "$OUT/og-image.png"

echo "Wrote:"
ls -1 "$OUT" | sed 's/^/  site\/img\//'
echo "  site/favicon.ico"
