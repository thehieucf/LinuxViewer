#!/bin/sh
# Builds and installs LinuxViewer.
#   ./install.sh                     install into ~/.local
#   sudo ./install.sh --system       install into /usr/local (+ uinput udev rule)
#   ./install.sh --uninstall [--system]
# PREFIX and DESTDIR override the destination; BINARY skips the build and
# installs an already built executable (for packaging).
set -eu

cd "$(dirname "$0")"
mode=install
system=0
for arg in "$@"; do
    case "$arg" in
        --system) system=1 ;;
        --uninstall) mode=uninstall ;;
        -h|--help) sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [ "$system" = 1 ]; then
    PREFIX=${PREFIX:-/usr/local}
else
    PREFIX=${PREFIX:-$HOME/.local}
fi
DESTDIR=${DESTDIR:-}
root="$DESTDIR$PREFIX"
id=io.github.linuxviewer
udev_rule="$DESTDIR${UDEV_RULES_DIR:-/etc/udev/rules.d}/60-linuxviewer-uinput.rules"

files="
$root/bin/linuxviewer
$root/share/applications/$id.desktop
$root/share/metainfo/$id.metainfo.xml
$root/share/icons/hicolor/scalable/apps/$id.svg
"

refresh_caches() {
    [ -n "$DESTDIR" ] && return 0
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$PREFIX/share/applications" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true
}

if [ "$mode" = uninstall ]; then
    for file in $files; do rm -f "$file"; done
    rm -rf "$root/share/licenses/linuxviewer" "$root/share/doc/linuxviewer"
    [ "$system" = 1 ] && rm -f "$udev_rule"
    refresh_caches
    echo "LinuxViewer removed from $root"
    exit 0
fi

if [ -z "${BINARY:-}" ]; then
    cargo build --release --locked -p linuxviewer
    BINARY=target/release/linuxviewer
fi

install -Dm755 "$BINARY" "$root/bin/linuxviewer"
install -Dm644 packaging/$id.desktop "$root/share/applications/$id.desktop"
install -Dm644 packaging/$id.metainfo.xml "$root/share/metainfo/$id.metainfo.xml"
install -Dm644 crates/linuxviewer/resources/$id.svg "$root/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 LICENSE "$root/share/licenses/linuxviewer/LICENSE"
install -Dm644 THIRD_PARTY_NOTICES.md "$root/share/licenses/linuxviewer/THIRD_PARTY_NOTICES.md"
vendor=crates/ultraviewer-protocol/vendor
install -Dm644 $vendor/zstd/LICENSE "$root/share/licenses/linuxviewer/Zstandard/LICENSE"
install -Dm644 $vendor/zstd/COPYING "$root/share/licenses/linuxviewer/Zstandard/COPYING"
install -Dm644 $vendor/g722/LICENSE "$root/share/licenses/linuxviewer/G722/LICENSE"
install -Dm644 $vendor/g722/WEBRTC_LICENSE "$root/share/licenses/linuxviewer/G722/WEBRTC_LICENSE"
install -Dm644 README.md "$root/share/doc/linuxviewer/README.md"

if [ "$system" = 1 ]; then
    install -Dm644 packaging/60-linuxviewer-uinput.rules "$udev_rule"
    if [ -z "$DESTDIR" ] && command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload-rules && udevadm trigger --name-match=uinput || true
    fi
fi

refresh_caches
echo "LinuxViewer installed into $root"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Note: $PREFIX/bin is not in PATH" ;;
esac
