#!/bin/sh
# Builds dist/linuxviewer-<version>-1.<arch>.rpm (see packaging/release.sh).
set -eu

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo build --release --locked -p linuxviewer
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2>/dev/null || date +%s) rpmbuild -bb packaging/linuxviewer.spec --quiet \
    --define "_topdir $work" --define "version $version" \
    --define "tree $PWD" --define "binary $target/release/linuxviewer"
mkdir -p dist
cp "$work"/RPMS/*/linuxviewer-"$version"-1.*.rpm dist/
ls dist/linuxviewer-"$version"-1.*.rpm
