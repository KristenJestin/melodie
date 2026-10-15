#!/bin/sh
# Generates every file of the logo from the SVGs of assets/brand, which are its only source: the
# PNGs of assets/brand/png and the icon of the desktop entry, dist/melodie.svg. The text of the
# social preview is drawn with the fonts of assets/brand/fonts only, through a fontconfig made for
# the run, so that the same PNG comes out on any machine. Needs rsvg-convert (librsvg).
#
#   tools/brand.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
brand="$root/assets/brand"
command -v rsvg-convert > /dev/null || { echo "tools/brand.sh: rsvg-convert (librsvg) is needed" >&2; exit 1; }

fonts=$(mktemp -d)
trap 'rm -rf "$fonts"' EXIT
cat > "$fonts/fonts.conf" <<CONF
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig><dir>$brand/fonts</dir><cachedir>$fonts/cache</cachedir></fontconfig>
CONF
export FONTCONFIG_FILE="$fonts/fonts.conf"

mkdir -p "$brand/png"
for size in 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$brand/icon.svg" -o "$brand/png/icon-$size.png"
done
rsvg-convert "$brand/social-preview.svg" -o "$brand/png/social-preview.png"
cp "$brand/icon.svg" "$root/dist/melodie.svg"
echo "Generated assets/brand/png and dist/melodie.svg"
