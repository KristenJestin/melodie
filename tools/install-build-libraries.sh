#!/bin/sh
# The system libraries Mélodie builds against, on an Ubuntu runner. The one list: the release and
# beta workflows call it, and `verify` in ci.yml, so that a new dependency is added here once and
# not forgotten in one of the three builds.
#
#   tools/install-build-libraries.sh
set -eu

# `dbus` provides the `dbus-daemon` the daemon test starts privately; without it the MPRIS checks
# are skipped. The rest is what the window (GPUI) links against: fonts, keyboard maps, Wayland and
# X11, and libmpv for the sound.
sudo apt-get update
sudo apt-get install -y \
  dbus \
  libfontconfig-dev \
  libfreetype-dev \
  libmpv-dev \
  libvulkan-dev \
  libwayland-dev \
  libx11-xcb-dev \
  libxkbcommon-dev \
  libxkbcommon-x11-dev
