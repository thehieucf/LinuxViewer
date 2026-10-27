#!/bin/sh
# Builds dist/linuxviewer_<version>_<arch>.deb. Run on Debian/Ubuntu (see
# packaging/release.sh, which uses the Ubuntu 24.04 image in this directory).
set -eu

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(dpkg --print-architecture)
target=${CARGO_TARGET_DIR:-target}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# GTK 4.14 / libadwaita 1.5 API so the package installs on Ubuntu 24.04.
cargo build --release --locked -p linuxviewer --no-default-features --features captcha
BINARY="$target/release/linuxviewer" PREFIX=/usr DESTDIR="$stage" \
    UDEV_RULES_DIR=/usr/lib/udev/rules.d ./install.sh --system >/dev/null
mkdir -p "$stage/usr/share/doc/linuxviewer"
mv "$stage/usr/share/licenses/linuxviewer"/* "$stage/usr/share/doc/linuxviewer/"
rmdir "$stage/usr/share/licenses/linuxviewer" "$stage/usr/share/licenses"
cp LICENSE "$stage/usr/share/doc/linuxviewer/copyright"

# Shared-library dependencies as dpkg-shlibdeps computes them.
work=$(mktemp -d)
mkdir -p "$work/debian"
printf 'Source: linuxviewer\n\nPackage: linuxviewer\nArchitecture: any\n' > "$work/debian/control"
depends=$(cd "$work" && dpkg-shlibdeps -O "$stage/usr/bin/linuxviewer" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
rm -rf "$work"
# Ubuntu ships libturbojpeg.so.0 as "libturbojpeg", Debian as "libturbojpeg0".
depends=$(printf '%s' "$depends" | sed 's/libturbojpeg (\([^)]*\))/libturbojpeg (\1) | libturbojpeg0/')

mkdir -p "$stage/DEBIAN"
size=$(du -sk --exclude=DEBIAN "$stage" | cut -f1)
cat > "$stage/DEBIAN/control" <<CONTROL
Package: linuxviewer
Version: $version
Architecture: $arch
Maintainer: thehieucf <69306656+thehieucf@users.noreply.github.com>
Installed-Size: $size
Depends: $depends
Recommends: xdg-desktop-portal, pipewire, gnome-keyring | kwalletmanager | keepassxc
Suggests: ffmpeg
Section: net
Priority: optional
Homepage: https://github.com/thehieucf/LinuxViewer
Description: UltraViewer-compatible remote desktop client
 Control another computer with its UltraViewer ID and password, or share this
 Linux computer: screen, keyboard and mouse, computer audio, chat, clipboard,
 screenshots and recording. Not an official UltraViewer product.
CONTROL
cat > "$stage/DEBIAN/postinst" <<'SCRIPT'
#!/bin/sh
set -e
# Lets the logged-in user create the virtual keyboard and pointer (uinput).
if [ "$1" = configure ] && command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --name-match=uinput 2>/dev/null || true
fi
SCRIPT
cat > "$stage/DEBIAN/postrm" <<'SCRIPT'
#!/bin/sh
set -e
if [ "$1" = remove ] && command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
fi
SCRIPT
chmod 755 "$stage/DEBIAN/postinst" "$stage/DEBIAN/postrm"
find "$stage" -type d -exec chmod 755 {} +

mkdir -p dist
output="dist/linuxviewer_${version}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$stage" "$output" >/dev/null
echo "$output"
