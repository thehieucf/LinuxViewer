# Packages a binary built by packaging/build-rpm.sh from this source tree.
%global debug_package %{nil}

Name:           linuxviewer
Version:        %{version}
Release:        1
Summary:        UltraViewer-compatible remote desktop client
License:        MIT
URL:            https://github.com/thehieucf/LinuxViewer
Recommends:     xdg-desktop-portal
Recommends:     pipewire
Suggests:       ffmpeg

%description
Control another computer with its UltraViewer ID and password, or share this
Linux computer: screen, keyboard and mouse, computer audio, chat, clipboard,
screenshots and recording. Not an official UltraViewer product.

%install
cd %{tree}
BINARY=%{binary} PREFIX=/usr DESTDIR=%{buildroot} UDEV_RULES_DIR=/usr/lib/udev/rules.d ./install.sh --system >/dev/null

%post
udevadm control --reload-rules >/dev/null 2>&1 || :
udevadm trigger --name-match=uinput >/dev/null 2>&1 || :

%postun
udevadm control --reload-rules >/dev/null 2>&1 || :

%files
%{_bindir}/linuxviewer
%{_datadir}/applications/io.github.linuxviewer.desktop
%{_datadir}/metainfo/io.github.linuxviewer.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.github.linuxviewer.svg
%{_datadir}/doc/linuxviewer
%{_datadir}/licenses/linuxviewer
/usr/lib/udev/rules.d/60-linuxviewer-uinput.rules
