# Third-party software

LinuxViewer statically includes compression and decompression portions of
[Zstandard 1.5.7](https://github.com/facebook/zstd/tree/v1.5.7), copyright Meta
Platforms, Inc. and affiliates, under its BSD license. The source's alternative
GPLv2 license is also preserved. Original notices are in
`crates/ultraviewer-protocol/vendor/zstd/LICENSE` and `.../zstd/COPYING`;
`install.sh` copies both into `share/licenses/linuxviewer/Zstandard/`.
Upstream source files are unmodified and `provenance.json` records the exact
release, archive digest and selected files. The build includes compression,
decompression and legacy formats 4–7 and disables architecture-specific assembly.

Computer audio decoding includes the WebRTC G.722 decoder from revision
`4fdd50a57b79826ea87cc5170be78b6444d9cdf9`. Its original public-domain notice
and WebRTC BSD license are preserved in `crates/ultraviewer-protocol/vendor/g722/LICENSE`
and `.../g722/WEBRTC_LICENSE`, and copied into `share/licenses/linuxviewer/G722/`.
`MacViewerG722.c` is the local allocation-checked wrapper from MacViewer.

LinuxViewer links dynamically against system libraries (GTK 4, libadwaita,
OpenSSL, libjpeg-turbo, PulseAudio, PipeWire) and uses Rust crates listed in
`Cargo.lock`, each under its own license.

The application includes the supplied `UltraViewer/8bit.dat` Zstandard decoding
dictionary as data (`crates/ultraviewer-protocol/resources/8bit.dat`). It is
112,640 bytes with SHA-256
`9aa45789d9ff9d329a623a685d57b67e33c777019de9ce1f273f21860e4bfe6b`
and dictionary ID `0x3da0bd10`. This is an UltraViewer-supplied asset, not an
independently authored or open-source dictionary. No redistribution license for
that asset was established; the supplied asset is retained for this local
interoperability build. No Windows executable code is embedded.
