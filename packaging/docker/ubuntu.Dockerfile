# .deb and AppImage build environment: Ubuntu 24.04 (glibc 2.39, GTK 4.14, libadwaita 1.5)
# so the .deb and the AppImage run on Ubuntu 24.04+, Debian 13 and newer distros.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates clang curl dpkg-dev file git libclang-dev pkg-config \
        libgtk-4-dev libadwaita-1-dev libssl-dev libturbojpeg0-dev libpulse-dev libpipewire-0.3-dev \
        librsvg2-common gdk-pixbuf2.0-bin desktop-file-utils patchelf squashfs-tools zsync \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && chmod -R a+rwX /opt/rustup /opt/cargo

# AppImage tooling: linuxdeploy (continuous build, includes the AppImage output
# plugin) and its GTK plugin.
ARG LINUXDEPLOY_URL=https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
ARG LINUXDEPLOY_GTK_URL=https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
RUN mkdir -p /opt/linuxdeploy && cd /opt/linuxdeploy \
    && curl -sSfL -o linuxdeploy-x86_64.AppImage "$LINUXDEPLOY_URL" \
    && curl -sSfL -o linuxdeploy-plugin-gtk.sh "$LINUXDEPLOY_GTK_URL" \
    && chmod +x linuxdeploy-x86_64.AppImage linuxdeploy-plugin-gtk.sh \
    && ./linuxdeploy-x86_64.AppImage --appimage-extract >/dev/null \
    && mv squashfs-root linuxdeploy \
    && chmod -R a+rX /opt/linuxdeploy \
    && ln -s /opt/linuxdeploy/linuxdeploy/AppRun /usr/local/bin/linuxdeploy \
    && ln -s /opt/linuxdeploy/linuxdeploy-plugin-gtk.sh /usr/local/bin/linuxdeploy-plugin-gtk.sh
