#!/bin/sh
# Builds dist/linuxviewer-<version>-<rel>-<arch>.pkg.tar.zst from PKGBUILD
# (see packaging/release.sh).
set -eu

cd "$(dirname "$0")"
mkdir -p ../dist
BUILDDIR=$(mktemp -d) PKGDEST="$PWD/../dist" makepkg --force --nocheck --noconfirm >/dev/null
ls ../dist/linuxviewer-*.pkg.tar.zst
