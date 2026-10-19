# LinuxViewer

Ứng dụng Linux tương thích **UltraViewer**, port từ [MacViewer](https://github.com/DeanOpen/MacViewer) (Swift/SwiftUI) sang Rust + GTK4/libadwaita.
Điều khiển máy tính khác, hoặc chia sẻ máy Linux này. Không dùng Wine, không chạy mã Windows.

> LinuxViewer là bản triển khai độc lập, không phải sản phẩm chính thức của UltraViewer.
> Chỉ kết nối tới máy tính bạn được phép truy cập.

## Cấu trúc

```
crates/ultraviewer-protocol/   Engine giao thức (port 1:1 từ UltraViewerProtocol của MacViewer)
  vendor/zstd, vendor/g722     Mã C đi kèm (Zstandard 1.5.7, WebRTC G.722)
  tests/codec.rs               41 test vector byte-level port từ bộ test Swift
  examples/                    loopback, host_demo, viewer_demo, tls_probe (kiểm thử qua relay thật)
crates/linuxviewer/            Ứng dụng GTK4 (giao diện, chụp màn hình, input, âm thanh)
packaging/                     .desktop, metainfo, udev rule cho uinput, PKGBUILD (Arch)
install.sh                     Build + cài đặt / gỡ cài đặt
```

## Yêu cầu

Rust ≥ 1.80, GTK ≥ 4.16, libadwaita ≥ 1.6, OpenSSL, libjpeg-turbo (turbojpeg), libpulse,
PipeWire, xdg-desktop-portal (+ backend của desktop), clang (bindgen cho PipeWire).
Tùy chọn: `ffmpeg` (quay video), một Secret Service như gnome-keyring/KWallet (lưu mật khẩu).

Arch: `sudo pacman -S gtk4 libadwaita openssl libjpeg-turbo libpulse pipewire xdg-desktop-portal clang`
Debian/Ubuntu: `sudo apt install libgtk-4-dev libadwaita-1-dev libssl-dev libturbojpeg0-dev libpulse-dev libpipewire-0.3-dev clang pkg-config`
Fedora: `sudo dnf install gtk4-devel libadwaita-devel openssl-devel turbojpeg-devel pulseaudio-libs-devel pipewire-devel clang`

## Build & chạy

```sh
git clone https://github.com/thehieucf/LinuxViewer.git && cd LinuxViewer
cargo test --workspace            # 52 test
cargo build --release
./target/release/linuxviewer
```

## Cài đặt

```sh
./install.sh                      # vào ~/.local (bin, menu ứng dụng, biểu tượng)
sudo ./install.sh --system        # vào /usr/local + udev rule cho /dev/uinput
./install.sh --uninstall          # gỡ (thêm --system nếu đã cài hệ thống)
cd packaging && makepkg -si       # Arch: tạo gói pacman từ thư mục mã nguồn này
```

udev rule `60-linuxviewer-uinput.rules` cho phép người dùng đang đăng nhập tạo bàn phím/chuột ảo,
cần cho chế độ cho phép điều khiển trên compositor không có portal RemoteDesktop (Hyprland, sway).

## Tính năng

- **Kết nối**: ID + mật khẩu UltraViewer, relay TLS (OpenSSL, tự bổ sung chứng chỉ trung gian qua AIA),
  LAN direct, giải mã màn hình (zstd + từ điển, RGB555, JPEG atlas bằng TurboJPEG, tile cache, scroll),
  bàn phím/chuột (Super→Win), gõ tiếng Việt qua IME (fcitx5/ibus), nhiều màn hình, vừa cửa sổ/100%,
  chất lượng ảnh, hình nền, con trỏ từ xa, âm thanh máy tính (PulseAudio/PipeWire), chụp màn hình,
  trình chú thích ảnh chụp (chọn vùng, bút, khung chữ nhật, 3 màu, hoàn tác, sao chép/lưu PNG),
  quay video H.264 15 fps (cần `ffmpeg`), chat, clipboard hai chiều, lưu mật khẩu trong keyring (Secret Service),
  Ctrl-Alt-Del / khóa / khởi động lại, đổi chiều điều khiển.
- **Chia sẻ**: chụp màn hình qua portal ScreenCast + PipeWire (Wayland & X11), điều khiển qua portal
  RemoteDesktop, **uinput** (Hyprland/sway — cần quyền ghi `/dev/uinput`, ví dụ thuộc nhóm `input`) hoặc XTest (X11);
  chấp thuận từng kết nối, chỉ xem/cho phép điều khiển, chat, clipboard.
- Giao diện Tiếng Việt / English (Cài đặt → Ngôn ngữ; mặc định theo `LC_MESSAGES`).

## Khác biệt so với bản Mac

- Dùng chung một đăng ký UltraViewer cho cả hai chiều (server định tuyến yêu cầu đến tới lần đăng ký mới nhất).
- Gửi keepalive kênh lệnh mỗi 30s kể từ lúc đăng ký (bản Mac có thể bị server ngắt sau ~70s nếu duyệt lâu).
- Viewer chờ tối đa 60s để máy kia chấp thuận (thay vì 20s).
- Atlas JPEG nhiều cột của UltraViewer 6.6 (màn hình lớn, ví dụ 2240×1400) bị Windows làm tròn chiều cao xuống,
  mất 1–2 ô cuối; LinuxViewer bỏ qua các ô đó thay vì từ chối cả khung hình (bản Mac báo "invalid image tile").
- Không có kiểm tra cập nhật tự động (cập nhật qua trình quản lý gói hoặc build lại).

## Gỡ lỗi

Log của lần chạy gần nhất nằm ở `~/.local/state/linuxviewer/linuxviewer.log` (lần trước: `.log.old`).
Chạy `RUST_LOG=debug linuxviewer` để có log chi tiết hơn. Để phân tích lỗi hiển thị, chạy
`env LINUXVIEWER_DUMP_SCREEN=$HOME/linuxviewer-dump linuxviewer` (thoát ứng dụng trước): các atlas JPEG
cùng vị trí ô được lưu vào thư mục đó — chúng chứa nội dung màn hình máy kia.

## Giới hạn đã biết

- Chưa thử với máy Windows chạy UltraViewer thật; mới kiểm thử qua relay thật giữa ứng dụng và
  `host_demo`/`viewer_demo`. Đường kết nối LAN direct chưa được kiểm thử thực tế.
- Khi chia sẻ máy qua **uinput** hoặc **XTest**, ký tự Unicode do máy kia gửi dạng VK_PACKET (ví dụ tiếng Việt
  gõ bằng Unikey) chỉ gõ được nếu là ASCII; gõ đầy đủ Unicode cần portal RemoteDesktop (GNOME, KDE).

## Giấy phép

MIT (`LICENSE`). Thành phần bên thứ ba giữ giấy phép riêng (xem `THIRD_PARTY_NOTICES.md`);
`8bit.dat` là dữ liệu của UltraViewer, không thuộc giấy phép MIT.
