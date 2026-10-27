# RPM build environment: Fedora 43 (glibc 2.42). The package requires shared
# libraries by soname, so it installs on Fedora 43+ and openSUSE Tumbleweed.
FROM fedora:43
RUN dnf -y install rust cargo clang clang-devel gcc pkgconf-pkg-config rpm-build \
        gtk4-devel libadwaita-devel webkitgtk6.0-devel openssl-devel turbojpeg-devel pulseaudio-libs-devel pipewire-devel \
    && dnf clean all
