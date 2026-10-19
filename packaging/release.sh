#!/bin/sh
# Builds the release packages (.deb and AppImage) in an Ubuntu 24.04 container:
#   packaging/release.sh            -> dist/
set -eu

cd "$(dirname "$0")/.."
image=linuxviewer-build
docker build -q -t "$image" packaging >/dev/null
# Cargo caches persist in a named volume; the host's target/ is left alone.
docker run --rm -v linuxviewer-build-cache:/cache "$image" chown "$(id -u):$(id -g)" /cache
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp -e CARGO_TARGET_DIR=/cache/target -e CARGO_HOME=/cache/cargo \
    -v linuxviewer-build-cache:/cache \
    -v "$PWD:/src" -w /src \
    "$image" sh -c 'packaging/build-deb.sh && packaging/build-appimage.sh'
(cd dist && sha256sum -- *.deb *.AppImage > SHA256SUMS)
ls -lh dist
