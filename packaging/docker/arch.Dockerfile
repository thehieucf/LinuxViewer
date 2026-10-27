# Arch Linux package build environment (makepkg runs as the calling user).
FROM archlinux:latest
RUN pacman -Syu --noconfirm --needed base-devel git rust clang pkgconf \
        gtk4 libadwaita webkitgtk-6.0 openssl libjpeg-turbo libpulse libpipewire xdg-desktop-portal \
    && pacman -Scc --noconfirm
