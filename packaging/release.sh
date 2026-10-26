#!/bin/sh
# Builds every release package into dist/, each in its distribution's container:
#   Ubuntu 24.04 -> .deb + AppImage, Arch Linux -> .pkg.tar.zst, Fedora 43 -> .rpm
# Usage: packaging/release.sh [deb] [appimage] [arch] [rpm]   (default: all)
set -eu

cd "$(dirname "$0")/.."
targets=${*:-deb appimage arch rpm}
mkdir -p dist

# run <distro> <command>: Cargo caches persist in a named volume per distro;
# the host's target/ is left alone.
run() {
    image="linuxviewer-build-$1"
    docker build -q -t "$image" -f "packaging/docker/$1.Dockerfile" packaging >/dev/null
    docker run --rm -v "linuxviewer-cache-$1:/cache" "$image" chown "$(id -u):$(id -g)" /cache
    docker run --rm --user "$(id -u):$(id -g)" \
        -e HOME=/tmp -e CARGO_TARGET_DIR=/cache/target -e CARGO_HOME=/cache/cargo \
        -v "linuxviewer-cache-$1:/cache" -v "$PWD:/src" -w /src \
        "$image" sh -c "$2"
}

for target in $targets; do
    case "$target" in
        deb) run ubuntu packaging/build-deb.sh ;;
        appimage) run ubuntu packaging/build-appimage.sh ;;
        arch) run arch packaging/build-arch.sh ;;
        rpm) run fedora packaging/build-rpm.sh ;;
        *) echo "unknown target: $target" >&2; exit 2 ;;
    esac
done
(cd dist && sha256sum -- *.deb *.AppImage *.pkg.tar.zst *.rpm 2>/dev/null > SHA256SUMS || true)
ls -lh dist
