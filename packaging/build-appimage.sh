#!/bin/sh
# Builds dist/LinuxViewer-<version>-<arch>.AppImage with linuxdeploy and its
# GTK plugin (see packaging/Dockerfile; run through packaging/release.sh).
set -eu

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(uname -m)
target=${CARGO_TARGET_DIR:-target}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
appdir="$work/AppDir"
id=io.github.linuxviewer

cargo build --release --locked -p linuxviewer --no-default-features
BINARY="$target/release/linuxviewer" PREFIX=/usr DESTDIR="$appdir" ./install.sh >/dev/null

export APPIMAGE_EXTRACT_AND_RUN=1 DEPLOY_GTK_VERSION=4 ARCH="$arch" LINUXDEPLOY_OUTPUT_VERSION="$version"
# PipeWire's client library must match the host daemon and its plugin paths.
linuxdeploy --appdir "$appdir" \
    --executable "$appdir/usr/bin/linuxviewer" \
    --desktop-file "$appdir/usr/share/applications/$id.desktop" \
    --icon-file "$appdir/usr/share/icons/hicolor/scalable/apps/$id.svg" \
    --exclude-library 'libpipewire-0.3.so*' \
    --plugin gtk

# The GTK plugin forces the X11 backend and a GTK theme. GTK 4 works natively
# on Wayland (preferred by the screen-sharing portal and input methods), and
# libadwaita styles itself and follows the desktop's dark preference.
hook="$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
if [ -f "$hook" ]; then
    sed -i -e 's/^\(\s*\)export GDK_BACKEND=x11/\1: # GDK_BACKEND left to GTK/' \
        -e 's/^\(\s*\)export GTK_THEME=/\1: # GTK_THEME left to libadwaita: /' "$hook"
fi

mkdir -p dist
output="$PWD/dist/LinuxViewer-$version-$arch.AppImage"
(cd "$work" && OUTPUT="$output" linuxdeploy --appdir "$appdir" --output appimage >/dev/null)
echo "dist/LinuxViewer-$version-$arch.AppImage"
