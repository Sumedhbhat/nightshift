#!/bin/sh
# Install the system libraries GPUI needs to build and run on Debian-based Linux.
set -eu
if [ "$(id -u)" -eq 0 ]; then maysudo=""; else maysudo="sudo"; fi
$maysudo apt-get update
$maysudo apt-get install --yes \
  build-essential \
  pkg-config \
  libfontconfig1-dev \
  libfreetype6-dev \
  libvulkan-dev \
  libwayland-dev \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libx11-xcb-dev \
  libxcb-randr0-dev \
  libxcb-render0-dev \
  libxcb-shape0-dev \
  libxcb-xfixes0-dev \
  libxcb-xkb-dev
