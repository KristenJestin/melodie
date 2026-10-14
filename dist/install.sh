#!/bin/sh
# Installs Mélodie from the folder of its release archive: `melodie` and `melodie-desktop` go into
# ${PREFIX:-$HOME/.local}/bin. It stops the running player first (`melodie quit`), so that the
# next `melodie play` starts the new one. It never asks for root, and never touches your
# configuration, your local copy of the library or your keyring entry.
#
#   ./install.sh
#   PREFIX=/some/where ./install.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-${HOME:?HOME is not set: say where to install with PREFIX}/.local}
bin=$prefix/bin

for program in melodie melodie-desktop; do
  [ -x "$here/$program" ] || { echo "install.sh: $here/$program is missing" >&2; exit 1; }
done

# The older player, if there is one, is the one that understands the daemon it started.
if [ -x "$bin/melodie" ]; then
  if "$bin/melodie" quit >/dev/null 2>&1; then
    echo "Stopped the running player."
  else
    echo "No player was running."
  fi
fi

mkdir -p "$bin"
for program in melodie melodie-desktop; do
  # `install` replaces the file instead of writing into it: a program still running is not hurt.
  install -m 755 "$here/$program" "$bin/$program"
  echo "Installed $bin/$program"
done

"$bin/melodie" --version
"$bin/melodie-desktop" --version

case ":${PATH:-}:" in
  *":$bin:"*) ;;
  *) echo "Note: $bin is not in your PATH." ;;
esac
# libmpv is the one thing the system must provide (libmpv2 on Ubuntu, mpv on Arch).
if command -v ldd >/dev/null 2>&1 && ldd "$bin/melodie" 2>/dev/null | grep -q 'not found'; then
  echo "Note: a library is missing for melodie; install libmpv (see the README)." >&2
fi
echo "Done. The configuration, the local copy and the keyring were left as they were."
