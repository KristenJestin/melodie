#!/bin/sh
# Packs the two built programs with their desktop entry, their icon and the install script, and writes the SHA-256 checksum beside the
# archive. Prints the archive's file name.
#
#   tools/package.sh <version> <binaries-dir> <out-dir>
set -eu

[ "$#" -eq 3 ] || { echo "usage: tools/package.sh <version> <binaries-dir> <out-dir>" >&2; exit 2; }
version=$1 binaries=$2 out=$3
root=$(cd "$(dirname "$0")/.." && pwd)

name="melodie-$version-linux-x86_64"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir "$stage/$name"
install -m 755 "$binaries/melodie" "$stage/$name/melodie"
install -m 755 "$binaries/melodie-desktop" "$stage/$name/melodie-desktop"
install -m 644 "$root/dist/melodie.desktop" "$stage/$name/melodie.desktop"
install -m 644 "$root/dist/melodie.svg" "$stage/$name/melodie.svg"
install -m 755 "$root/dist/install.sh" "$stage/$name/install.sh"

mkdir -p "$out"
tar --owner=0 --group=0 -C "$stage" -czf "$out/$name.tar.gz" "$name"
# Relative name, so that `sha256sum --check` works from the folder the two files are in.
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$name.tar.gz"
