#!/bin/sh
# Installs Mélodie from the folder of its release archive: `melodie` and `melodie-desktop` go into
# ${PREFIX:-$HOME/.local}/bin, and `melodie.desktop` into ${PREFIX:-$HOME/.local}/share/applications
# (the launchers' entry for the window, which MPRIS's DesktopEntry points to). It stops the running player first (`melodie quit`), so that the
# next `melodie play` starts the new one. It never asks for root, and never touches your
# configuration, your local copy of the library or your keyring entry.
#
#   ./install.sh
#   PREFIX=/some/where ./install.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-${HOME:?HOME is not set: say where to install with PREFIX}/.local}
bin=$prefix/bin
applications=$prefix/share/applications

for program in melodie melodie-desktop; do
  [ -x "$here/$program" ] || { echo "install.sh: $here/$program is missing" >&2; exit 1; }
done
[ -f "$here/melodie.desktop" ] || { echo "install.sh: $here/melodie.desktop is missing" >&2; exit 1; }

# The older player, if there is one, is the one that understands the daemon it started. `quit`
# exits 1 when nothing ran; any other failure (an older `melodie` without `quit`, a missing
# library) tells nothing about whether a player runs, so it is not reported as "none".
if [ -x "$bin/melodie" ]; then
  status=0
  "$bin/melodie" quit >/dev/null 2>&1 || status=$?
  case $status in
    0) echo "Stopped the running player." ;;
    1) echo "No player was running." ;;
    *) echo "Could not stop the player: stop it before using the new one." >&2 ;;
  esac
fi

mkdir -p "$bin"
for program in melodie melodie-desktop; do
  # Copied beside, then renamed: a launcher starting the program meanwhile never finds a half
  # written file, and a program still running is not hurt.
  install -m 755 "$here/$program" "$bin/.$program.new"
  mv -f "$bin/.$program.new" "$bin/$program"
  echo "Installed $bin/$program"
done

mkdir -p "$applications"
install -m 644 "$here/melodie.desktop" "$applications/.melodie.desktop.new"
mv -f "$applications/.melodie.desktop.new" "$applications/melodie.desktop"
echo "Installed $applications/melodie.desktop"
# Only a cache for desktops that read it: never required, and its failure does not fail the install.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$applications" >/dev/null 2>&1 || true
fi

case ":${PATH:-}:" in
  *":$bin:"*) ;;
  *) echo "Note: $bin is not in your PATH." ;;
esac
# libmpv is the one thing the system must provide (libmpv2 on Ubuntu, mpv on Arch). Looked for
# before the programs are run: without it, they do not start.
if command -v ldd >/dev/null 2>&1 && ldd "$bin/melodie" 2>/dev/null | grep -q 'not found'; then
  echo "Note: a library is missing for melodie; install libmpv (see the README)." >&2
fi
started=yes
for program in melodie melodie-desktop; do
  "$bin/$program" --version || { echo "Note: $bin/$program does not start." >&2; started=no; }
done
echo "Done. The configuration, the local copy and the keyring were left as they were."
[ "$started" = yes ]
