//! Interface language: English keys with a Vietnamese table. The language is
//! chosen at startup from the settings ("system", "en" or "vi").

use std::collections::HashMap;
use std::sync::OnceLock;

static LANGUAGE: OnceLock<Language> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Vietnamese,
}

pub fn system_prefers_vietnamese() -> bool {
    ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| value.split(':').next().unwrap_or("").starts_with("vi"))
}

/// Call once before building any interface.
pub fn init(setting: &str) {
    let language = match setting {
        "vi" => Language::Vietnamese,
        "en" => Language::English,
        _ if system_prefers_vietnamese() => Language::Vietnamese,
        _ => Language::English,
    };
    let _ = LANGUAGE.set(language);
}

pub fn language() -> Language {
    *LANGUAGE.get().unwrap_or(&Language::English)
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| VIETNAMESE.iter().copied().collect())
}

/// Translates an English interface string.
pub fn tr(key: &str) -> String {
    if language() == Language::Vietnamese {
        if let Some(value) = table().get(key) {
            return (*value).to_string();
        }
        // Parameterized protocol messages that embed a value.
        for (prefix, suffix, format) in [
            ("UltraViewer declined the partner lookup (code ", ").", "UltraViewer declined the partner lookup (code {})."),
            ("The secure UltraViewer connection failed: ", "", "The secure UltraViewer connection failed: {}"),
            ("The network connection failed: ", "", "The network connection failed: {}"),
            ("The remote computer sent an invalid message: ", "", "The remote computer sent an invalid message: {}"),
            ("The incoming connection sent an invalid message: ", "", "The incoming connection sent an invalid message: {}"),
            ("The relay sent an invalid frame: ", "", "The relay sent an invalid frame: {}"),
            ("This computer's client profile is invalid: ", "", "This computer's client profile is invalid: {}"),
            ("This computer's host profile is invalid: ", "", "This computer's host profile is invalid: {}"),
            ("The client profile could not be saved: ", "", "The client profile could not be saved: {}"),
        ] {
            if let Some(value) = key.strip_prefix(prefix).and_then(|rest| rest.strip_suffix(suffix)) {
                if let Some(translated) = table().get(format) {
                    return translated.replacen("{}", value, 1);
                }
            }
        }
    }
    key.to_string()
}

/// Translates and fills `{}` placeholders in order.
pub fn trf(key: &str, values: &[&str]) -> String {
    let mut text = tr(key);
    for value in values {
        text = text.replacen("{}", value, 1);
    }
    text
}

static VIETNAMESE: &[(&str, &str)] = &[
    // Application
    ("LinuxViewer", "LinuxViewer"),
    ("Works with UltraViewer", "Tương thích với UltraViewer"),
    ("UltraViewer compatible", "Tương thích UltraViewer"),
    ("Connect and share", "Kết nối và chia sẻ"),
    ("Control another computer, or share this computer.", "Điều khiển máy tính khác hoặc chia sẻ máy tính này."),
    ("CONNECT", "KẾT NỐI"),
    ("SHARE", "CHIA SẺ"),
    ("Connect to another computer", "Kết nối đến máy khác"),
    ("Share this computer", "Chia sẻ máy tính này"),
    ("Partner ID", "ID đối tác"),
    ("Partner password", "Mật khẩu đối tác"),
    ("Partner's UltraViewer ID", "ID UltraViewer của đối tác"),
    ("Partner's password", "Mật khẩu của đối tác"),
    ("Password cleared", "Đã xóa mật khẩu khỏi ô nhập"),
    ("Remember password on this computer", "Ghi nhớ mật khẩu trên máy này"),
    ("Passwords of successful connections are saved in the system keyring.", "Mật khẩu kết nối thành công được lưu trong keyring của hệ thống."),
    ("Turn on Control in the session toolbar to use the keyboard and mouse.", "Bật Điều khiển trên thanh công cụ để dùng bàn phím và chuột."),
    ("Open screen", "Mở màn hình"),
    ("Disconnect", "Ngắt kết nối"),
    ("Connect", "Kết nối"),
    ("Your ID", "ID của bạn"),
    ("Your password", "Mật khẩu của bạn"),
    ("Start sharing to get this computer's UltraViewer ID.", "Bắt đầu chia sẻ để lấy ID UltraViewer của máy này."),
    ("Getting your ID…", "Đang lấy ID…"),
    ("Copy ID", "Sao chép ID"),
    ("Copy password", "Sao chép mật khẩu"),
    ("New password", "Đổi mật khẩu"),
    ("Show", "Hiện"),
    ("Hide", "Ẩn"),
    ("Start sharing this computer", "Bắt đầu chia sẻ máy này"),
    ("Stop sharing", "Dừng chia sẻ"),
    ("Try sharing again", "Thử chia sẻ lại"),
    ("Enter this ID and password on the other computer. Your screen is shared only after you approve.", "Nhập ID và mật khẩu này trên máy kia. Màn hình chỉ được chia sẻ sau khi bạn chấp thuận."),
    ("Incoming access paused", "Đã tạm dừng truy cập từ xa"),
    ("Incoming access on", "Đã bật truy cập từ xa"),
    ("Screen shared", "Đang chia sẻ màn hình"),
    ("Recent computers", "Máy tính gần đây"),
    ("Choose a computer to reuse its connection details.", "Chọn máy tính để dùng lại thông tin kết nối."),
    ("Clear history", "Xóa lịch sử"),
    ("Remote computer", "Máy tính từ xa"),
    ("Select", "Chọn"),
    ("Remove from history", "Xóa khỏi lịch sử"),
    ("Settings", "Cài đặt"),
    ("About LinuxViewer", "Giới thiệu LinuxViewer"),
    ("Keyboard shortcuts", "Phím tắt"),
    ("Quit", "Thoát"),
    // Sharing permissions (Linux)
    ("Sharing on Linux", "Chia sẻ trên Linux"),
    ("Screen capture", "Chụp màn hình"),
    ("Keyboard and mouse control", "Điều khiển bàn phím và chuột"),
    ("Ready", "Sẵn sàng"),
    ("Asks when sharing starts", "Hỏi khi bắt đầu chia sẻ"),
    ("Asks when sharing starts.", "Hệ thống sẽ hỏi khi bắt đầu chia sẻ."),
    ("Unavailable", "Không khả dụng"),
    ("The desktop asks which screen to share when a connection is approved.", "Hệ thống sẽ hỏi chọn màn hình cần chia sẻ khi bạn chấp thuận kết nối."),
    // Incoming
    ("Incoming connection", "Kết nối đến"),
    ("{} wants to access this computer.", "{} muốn truy cập máy tính này."),
    ("Allow viewing shares your screen. Allow control also gives access to your mouse and keyboard.", "Cho phép xem để chia sẻ màn hình. Cho phép điều khiển sẽ cấp thêm quyền dùng chuột và bàn phím."),
    ("Decline", "Từ chối"),
    ("Allow viewing", "Cho phép xem"),
    ("Allow control", "Cho phép điều khiển"),
    ("Allow control of this computer", "Cho phép điều khiển máy này"),
    ("End incoming session", "Kết thúc phiên truy cập"),
    ("Pause incoming access", "Tạm dừng truy cập từ xa"),
    ("Viewing: {}", "Đang xem: {}"),
    ("Another computer", "Máy khác"),
    ("Waiting for another computer to connect.", "Đang chờ máy khác kết nối."),
    ("Reconnecting to UltraViewer…", "Đang kết nối lại với UltraViewer…"),
    ("Incoming access is paused.", "Đã tạm dừng truy cập từ xa."),
    ("An incoming connection needs your approval.", "Một kết nối đến cần bạn chấp thuận."),
    ("Your computer is being viewed and controlled.", "Máy tính đang được xem và điều khiển."),
    ("Your computer is being viewed.", "Máy tính đang được xem."),
    ("Ending the incoming session…", "Đang kết thúc phiên truy cập đến…"),
    ("This computer is unavailable for incoming connections.", "Máy tính hiện không thể nhận kết nối."),
    ("Remote control paused: {}", "Đã tạm dừng điều khiển: {}"),
    ("Remote control could not be updated: {}", "Không thể cập nhật quyền điều khiển từ xa: {}"),
    ("Switch control sides", "Đổi chiều điều khiển"),
    ("Switch control sides…", "Đổi chiều điều khiển…"),
    ("Switch control sides?", "Đổi chiều điều khiển?"),
    ("Let the other computer connect to this computer", "Cho phép máy kia kết nối đến máy này"),
    ("LinuxViewer will share this computer's connection ID and password with the connected computer. You will approve its incoming request here.", "LinuxViewer sẽ gửi ID kết nối và mật khẩu của máy này cho máy đang kết nối. Bạn sẽ chấp thuận yêu cầu truy cập đến tại đây."),
    ("The other computer wants to switch control sides.", "Máy kia muốn đổi chiều điều khiển."),
    ("End this incoming session and connect to the same computer? Control will stay off until you enable it.", "Kết thúc phiên truy cập đến này và kết nối đến cùng máy đó? Chức năng điều khiển sẽ vẫn tắt cho đến khi bạn bật."),
    ("Connect to it", "Kết nối đến máy đó"),
    ("Could not switch control sides. Ask the other computer to try again.", "Không thể đổi chiều điều khiển. Hãy yêu cầu người dùng máy kia thử lại."),
    // Session toolbar
    ("Home", "Trang chủ"),
    ("Return home without disconnecting", "Về trang chủ và giữ kết nối"),
    ("End the connection to this computer", "Ngắt kết nối với máy tính này"),
    ("Actions", "Thao tác"),
    ("Configuration", "Cấu hình"),
    ("Capture", "Chụp hình"),
    ("Control", "Điều khiển"),
    ("Hide session toolbar", "Ẩn thanh công cụ"),
    ("Show toolbar", "Hiện thanh công cụ"),
    ("Send Ctrl-Alt-Delete", "Gửi Ctrl + Alt + Del"),
    ("Lock remote screen", "Khóa màn hình từ xa"),
    ("Restart remote computer…", "Khởi động lại máy từ xa…"),
    ("Restart remote computer", "Khởi động lại máy từ xa"),
    ("Restart {}?", "Khởi động lại {}?"),
    ("The remote session will disconnect. Unsaved work on that computer may be lost.", "Phiên truy cập sẽ bị ngắt. Công việc chưa lưu trên máy đó có thể bị mất."),
    ("Release pressed keys and buttons", "Nhả các phím và nút chuột đang giữ"),
    ("Refresh remote screen", "Làm mới màn hình từ xa"),
    ("Monitor", "Màn hình"),
    ("All monitors", "Tất cả màn hình"),
    ("Monitor {} · {} × {}", "Màn hình {} · {} × {}"),
    ("Display size", "Kích thước hiển thị"),
    ("Fit to window", "Vừa cửa sổ"),
    ("Actual size (100%)", "Kích thước thực (100%)"),
    ("Smooth scaling", "Làm mượt hình ảnh"),
    ("View only", "Chỉ xem"),
    ("Show remote wallpaper", "Hiện hình nền từ xa"),
    ("Show remote cursor", "Hiện chuột từ xa"),
    ("Computer audio", "Âm thanh máy tính"),
    ("Image quality", "Chất lượng hình ảnh"),
    ("Automatic", "Tự động"),
    ("Prefer speed", "Ưu tiên tốc độ"),
    ("Prefer image quality", "Ưu tiên chất lượng"),
    ("Custom…", "Tùy chỉnh…"),
    ("Lower quality uses less bandwidth. Higher quality keeps more image detail.", "Chất lượng thấp dùng ít băng thông hơn. Chất lượng cao giữ được nhiều chi tiết hơn."),
    ("Quality: {}", "Chất lượng: {}"),
    ("Cancel", "Hủy"),
    ("Apply", "Áp dụng"),
    ("OK", "OK"),
    ("Save screenshot as PNG…", "Lưu ảnh màn hình thành PNG…"),
    ("Capture and annotate…", "Chụp và chú thích…"),
    ("Capture a region…", "Chụp một vùng…"),
    ("Capture a region — {}", "Chụp một vùng — {}"),
    ("Annotate capture — {}", "Chú thích ảnh chụp — {}"),
    ("Select region", "Chọn vùng"),
    ("Draw", "Vẽ"),
    ("Rectangle", "Hình chữ nhật"),
    ("Red", "Đỏ"),
    ("Yellow", "Vàng"),
    ("Blue", "Xanh dương"),
    ("Full image", "Toàn bộ ảnh"),
    ("Undo drawing", "Hoàn tác nét vẽ"),
    ("Drag over the image to select a region.", "Kéo trên ảnh để chọn một vùng."),
    ("Save PNG…", "Lưu PNG…"),
    ("Select a region at least 2 × 2 pixels.", "Hãy chọn vùng có kích thước tối thiểu 2 × 2 điểm ảnh."),
    ("Copy screenshot", "Sao chép ảnh màn hình"),
    ("Open capture folder", "Mở thư mục ảnh chụp"),
    ("Record remote screen", "Quay màn hình từ xa"),
    ("Stop recording ({})", "Dừng quay ({})"),
    ("Screenshot saved to {}", "Đã lưu ảnh màn hình tại {}"),
    ("Screenshot copied", "Đã sao chép ảnh màn hình"),
    ("Recording saved to {}", "Đã lưu video tại {}"),
    ("Recording needs ffmpeg. Install the ffmpeg package and try again.", "Cần ffmpeg để quay video. Hãy cài gói ffmpeg rồi thử lại."),
    ("The recording could not be saved: {}", "Không thể lưu video: {}"),
    ("Capture failed", "Không thể chụp hình"),
    ("Keyboard & mouse enabled", "Đã bật bàn phím và chuột"),
    ("View only · Turn on Control to use the keyboard and mouse", "Chỉ xem · Bật Điều khiển để dùng bàn phím và chuột"),
    ("Remote computer screen", "Màn hình máy tính từ xa"),
    // Chat & clipboard
    ("Chat", "Trò chuyện"),
    ("Show chat", "Hiện trò chuyện"),
    ("Hide chat", "Ẩn trò chuyện"),
    ("Chat with {}", "Trò chuyện với {}"),
    ("Message", "Tin nhắn"),
    ("Send", "Gửi"),
    ("You", "Tôi"),
    ("{} is typing…", "{} đang nhập…"),
    ("No messages yet.", "Chưa có tin nhắn."),
    ("Clipboard", "Bảng nhớ tạm"),
    ("Share clipboard", "Chia sẻ bảng nhớ tạm"),
    ("Newly copied text is shared in both directions.", "Văn bản vừa sao chép được chia sẻ theo cả hai chiều."),
    ("Clipboard sharing is off.", "Đã tắt chia sẻ bảng nhớ tạm."),
    ("Enabling clipboard sharing…", "Đang bật chia sẻ bảng nhớ tạm…"),
    ("Clipboard sharing could not be enabled.", "Không thể bật chia sẻ bảng nhớ tạm."),
    ("Clipboard text exceeds the 1 MB sharing limit.", "Văn bản vượt quá giới hạn chia sẻ 1 MB."),
    ("Clipboard text could not be sent.", "Không thể gửi văn bản trong bảng nhớ tạm."),
    ("The chat message could not be sent.", "Không thể gửi tin nhắn."),
    ("Keep chat messages under 16,384 characters.", "Tin nhắn phải ngắn hơn 16.384 ký tự."),
    ("Sent to the other computer", "Đã gửi sang máy kia"),
    ("Copied from the other computer", "Đã copy từ máy kia"),
    ("Nothing shared yet. Copy text on either computer.", "Chưa có gì. Hãy copy chữ ở một trong hai máy."),
    ("Sent from this computer. Click to copy again.", "Đã gửi từ máy này. Bấm để copy lại."),
    ("Received from the other computer. Click to copy.", "Nhận từ máy kia. Bấm để copy."),
    // Settings
    ("General", "Chung"),
    ("Language", "Ngôn ngữ"),
    ("App language", "Ngôn ngữ ứng dụng"),
    ("System language", "Theo hệ thống"),
    ("English", "English"),
    ("Tiếng Việt", "Tiếng Việt"),
    ("Restart LinuxViewer to apply the new language.", "Khởi động lại LinuxViewer để áp dụng ngôn ngữ mới."),
    ("Restart now", "Khởi động lại ngay"),
    ("History", "Lịch sử"),
    ("Remember successfully connected partner IDs", "Ghi nhớ ID đã kết nối thành công"),
    ("Passwords are kept separately in the system keyring.", "Mật khẩu được lưu riêng trong keyring của hệ thống."),
    ("Clear recent partners", "Xóa danh sách gần đây"),
    ("Forget saved passwords", "Xóa mật khẩu đã lưu"),
    ("Saved partner passwords were removed from the keyring.", "Đã xóa mật khẩu đối tác khỏi keyring."),
    ("Saved passwords could not be removed: {}", "Không thể xóa các mật khẩu đã lưu: {}"),
    ("Saved password unavailable: {} Enter it manually.", "Không thể lấy mật khẩu đã lưu: {} Hãy nhập mật khẩu thủ công."),
    ("Connected, but the password could not be saved: {}", "Đã kết nối nhưng không thể lưu mật khẩu: {}"),
    ("Remote screen", "Màn hình từ xa"),
    ("Smooth image scaling", "Làm mượt khi thu phóng"),
    ("Scroll speed", "Tốc độ cuộn"),
    ("Send system shortcuts to the other computer", "Gửi phím tắt hệ thống sang máy kia"),
    ("While controlling, Super and Alt+Tab go to the remote computer when the desktop allows it.", "Khi đang điều khiển, phím Super và Alt+Tab được gửi sang máy kia nếu môi trường desktop cho phép."),
    ("Share clipboard with the other computer", "Chia sẻ bảng nhớ tạm với máy kia"),
    ("Text copied on either computer can be pasted on the other. Applies to new sessions.", "Chữ copy ở máy này có thể dán ở máy kia và ngược lại. Áp dụng cho các phiên kết nối mới."),
    ("Connection", "Kết nối"),
    ("Direct connection on the local network", "Kết nối trực tiếp trong mạng nội bộ"),
    ("Uses a faster encrypted LAN path when both computers are on the same network, and falls back to the relay.", "Dùng đường truyền LAN mã hóa nhanh hơn khi hai máy cùng mạng, tự quay về máy chủ chuyển tiếp khi cần."),
    ("Capture folder", "Thư mục ảnh chụp"),
    ("Choose…", "Chọn…"),
    // Connection states
    ("Not connected", "Chưa kết nối"),
    ("Connecting", "Đang kết nối"),
    ("Connected", "Đã kết nối"),
    ("Disconnected", "Đã ngắt kết nối"),
    ("Enter the ID and password from the other computer.", "Nhập ID và mật khẩu từ máy kia."),
    ("Connecting to {}…", "Đang kết nối {}…"),
    ("Connected to {}.", "Đã kết nối với {}."),
    ("You disconnected from the remote computer.", "Bạn đã ngắt kết nối với máy tính từ xa."),
    ("The remote session ended.", "Phiên truy cập từ xa đã kết thúc."),
    ("The remote session ended with an error.", "Phiên truy cập đã kết thúc do lỗi."),
    ("The connection could not be established.", "Không thể thiết lập kết nối."),
    ("The connection ended before the other computer authenticated the session.", "Kết nối đã kết thúc trước khi máy kia xác thực phiên."),
    ("Enter the partner ID shown in UltraViewer on the other computer.", "Nhập ID đối tác trong UltraViewer trên máy kia."),
    ("Enter the password shown on the other computer.", "Nhập mật khẩu hiển thị trên máy kia."),
    ("Remote input stopped because its queue filled. Turn on Control again to retry.", "Đã dừng gửi lệnh điều khiển vì hàng đợi đã đầy. Bật lại Điều khiển để thử lại."),
    ("Remote input stopped: {}", "Đã dừng gửi lệnh điều khiển từ xa: {}"),
    ("The remote action could not be completed: {}", "Không thể hoàn tất thao tác từ xa: {}"),
    ("Computer audio stopped: {}", "Âm thanh máy tính đã dừng: {}"),
    ("Audio output is unavailable.", "Không có thiết bị phát âm thanh."),
    // Protocol/session messages (from the engine)
    ("Registering this computer…", "Đang đăng ký máy tính này…"),
    ("Finding the remote computer…", "Đang tìm máy kia…"),
    ("Connecting to the remote computer…", "Đang kết nối đến máy kia…"),
    ("Waiting for the remote computer to accept the session…", "Đang chờ máy kia chấp nhận phiên…"),
    ("Refreshing the remote screen…", "Đang làm mới màn hình từ xa…"),
    ("Remote screen refreshed.", "Đã làm mới màn hình từ xa."),
    ("Connected directly over the local network.", "Đã kết nối trực tiếp qua mạng nội bộ."),
    ("Restoring the relay connection…", "Đang khôi phục kết nối qua máy chủ chuyển tiếp…"),
    ("A remote session is already open.", "Đã có một phiên truy cập đang mở."),
    ("Enter the remote computer's UltraViewer ID and password.", "Nhập ID và mật khẩu UltraViewer của máy kia."),
    ("The remote session is not connected.", "Phiên truy cập từ xa chưa được kết nối."),
    ("The remote connection timed out.", "Kết nối từ xa đã hết thời gian chờ."),
    ("The remote computer disconnected.", "Máy tính từ xa đã ngắt kết nối."),
    ("UltraViewer rejected the partner password.", "UltraViewer từ chối mật khẩu đối tác."),
    ("UltraViewer declined the partner lookup (code {}).", "UltraViewer từ chối tìm đối tác (mã {}). Hãy kiểm tra ID và chắc chắn máy kia đang mở UltraViewer."),
    ("UltraViewer returned an incomplete partner address.", "UltraViewer trả về địa chỉ đối tác không đầy đủ."),
    ("The remote computer blocked this connection.", "Máy kia đã chặn kết nối này."),
    ("This UltraViewer version is not supported yet.", "Phiên bản UltraViewer này chưa được hỗ trợ."),
    ("The remote computer returned incomplete screen information.", "Máy kia trả về thông tin màn hình không đầy đủ."),
    ("The remote session could not deliver its status updates.", "Phiên truy cập không thể chuyển các cập nhật trạng thái."),
    ("The remote computer has enabled view-only access.", "Máy kia đang chỉ cho phép xem."),
    ("The remote computer sent an invalid message: {}", "Máy kia gửi thông điệp không hợp lệ: {}"),
    ("The network connection is already open.", "Kết nối mạng đã được mở."),
    ("The relay returned an invalid network address.", "Máy chủ chuyển tiếp trả về địa chỉ mạng không hợp lệ."),
    ("The UltraViewer relay did not respond. Check the connection and try again.", "Máy chủ chuyển tiếp UltraViewer không phản hồi. Kiểm tra kết nối rồi thử lại."),
    ("The UltraViewer network connection closed. Try connecting again.", "Kết nối mạng UltraViewer đã đóng. Hãy thử kết nối lại."),
    ("The relay connection ended before an update finished. Try connecting again.", "Kết nối chuyển tiếp kết thúc trước khi cập nhật hoàn tất. Hãy thử kết nối lại."),
    ("The remote connection could not process its updates. Try connecting again.", "Kết nối từ xa không thể xử lý các cập nhật. Hãy thử kết nối lại."),
    ("The UltraViewer service address is invalid.", "Địa chỉ dịch vụ UltraViewer không hợp lệ."),
    ("The UltraViewer service address could not be resolved. Check the connection and try again.", "Không phân giải được địa chỉ dịch vụ UltraViewer. Kiểm tra kết nối mạng rồi thử lại."),
    ("The secure UltraViewer connection failed: {}", "Kết nối bảo mật tới UltraViewer thất bại: {}"),
    ("The network connection failed: {}", "Kết nối mạng thất bại: {}"),
    ("The relay sent an oversized HTTP header.", "Máy chủ chuyển tiếp gửi phần đầu HTTP quá lớn."),
    ("Getting this computer's UltraViewer ID…", "Đang lấy ID UltraViewer của máy này…"),
    ("Preparing the incoming connection…", "Đang chuẩn bị kết nối đến…"),
    ("Waiting for the other computer's login…", "Đang chờ máy kia đăng nhập…"),
    ("The remote viewer needs modern 16-bit screen mode.", "Máy xem cần dùng chế độ màn hình 16 bit hiện đại."),
    ("Incoming access is already running.", "Truy cập từ xa đã được bật."),
    ("The incoming connection is no longer available.", "Kết nối đến không còn khả dụng."),
    ("The UltraViewer incoming relay is unavailable. Try again.", "Máy chủ chuyển tiếp UltraViewer cho kết nối đến hiện không khả dụng. Hãy thử lại."),
    ("The incoming UltraViewer connection timed out.", "Kết nối UltraViewer đến đã hết thời gian chờ."),
    ("UltraViewer asks you to solve a captcha before it gives this computer an ID.", "UltraViewer yêu cầu giải captcha trước khi cấp ID cho máy tính này."),
    ("Solve UltraViewer's captcha to get this computer's ID.", "Hãy giải captcha của UltraViewer để nhận ID cho máy tính này."),
    ("Solve UltraViewer's captcha to continue connecting.", "Hãy giải captcha của UltraViewer để tiếp tục kết nối."),
    ("Verify this computer", "Xác minh máy tính này"),
    ("UltraViewer asks for a captcha, which this build cannot show. Install the .deb, .rpm or Arch package to verify this computer once.", "UltraViewer yêu cầu giải captcha nhưng bản build này không hiển thị được. Hãy cài gói .deb, .rpm hoặc gói Arch để xác minh máy tính này một lần."),
    ("UltraViewer needs to verify this computer once before it gives it an ID. Type the characters you see and confirm.", "UltraViewer cần xác minh máy tính này một lần trước khi cấp ID. Hãy nhập các ký tự bạn thấy rồi bấm xác nhận."),
    ("The UltraViewer verification page could not be loaded. Check the connection and try again.", "Không tải được trang xác minh của UltraViewer. Hãy kiểm tra kết nối mạng rồi thử lại."),
    ("The remote viewer requested an unsupported screen color mode. Use 16-bit color.", "Máy xem yêu cầu chế độ màu không được hỗ trợ. Hãy dùng màu 16 bit."),
    ("The incoming connection sent an invalid message: {}", "Kết nối đến gửi thông điệp không hợp lệ: {}"),
    ("The incoming request was declined.", "Đã từ chối yêu cầu kết nối đến."),
    ("You ended the incoming session.", "Bạn đã kết thúc phiên truy cập đến."),
    ("The other computer disconnected.", "Máy kia đã ngắt kết nối."),
    ("The incoming connection timed out. Ask the other computer to connect again.", "Kết nối đến đã hết thời gian chờ. Hãy yêu cầu người dùng máy kia kết nối lại."),
    ("The incoming relay did not provide an address. Ask the other computer to connect again.", "Máy chủ chuyển tiếp chưa cung cấp địa chỉ cho kết nối đến. Hãy yêu cầu người dùng máy kia kết nối lại."),
    ("The incoming relay disconnected.", "Máy chủ chuyển tiếp cho kết nối đến đã ngắt kết nối."),
    ("The screen is no longer available.", "Màn hình không còn khả dụng."),
    // Profile
    ("No active IPv4 network connection is available. Connect this computer to a network and try again.", "Không có kết nối mạng IPv4 nào đang hoạt động. Hãy kết nối mạng rồi thử lại."),
    ("This computer's network interfaces could not be read. Try connecting again.", "Không đọc được thông tin card mạng của máy. Hãy thử lại."),
    ("No hardware address is available for this computer's active network interfaces.", "Không tìm thấy địa chỉ phần cứng (MAC) của card mạng đang hoạt động."),
    ("The private client profile location is unavailable or is not a regular folder/file.", "Thư mục hồ sơ máy khách không khả dụng hoặc không phải thư mục/tệp thông thường."),
    ("The relay sent an invalid frame: {}", "Máy chủ chuyển tiếp gửi khung dữ liệu không hợp lệ: {}"),
    ("This computer's client profile is invalid: {}", "Hồ sơ máy khách của máy này không hợp lệ: {}"),
    ("This computer's host profile is invalid: {}", "Hồ sơ chia sẻ của máy này không hợp lệ: {}"),
    ("The client profile could not be saved: {}", "Không thể lưu hồ sơ máy khách: {}"),
    ("The saved client profile is invalid. Its installation identity has been preserved on disk.", "Hồ sơ máy khách đã lưu không hợp lệ. Định danh cài đặt vẫn được giữ trên đĩa."),
    // Hosting backends
    ("Screen sharing was cancelled.", "Đã hủy chia sẻ màn hình."),
    ("Screen sharing is unavailable: {}", "Không thể chia sẻ màn hình: {}"),
    ("Keyboard and mouse control is unavailable: {}", "Không thể điều khiển bàn phím và chuột: {}"),
    ("Remote control needs write access to /dev/uinput (add your user to the input group) or a desktop with remote-desktop portal support.", "Điều khiển từ xa cần quyền ghi /dev/uinput (thêm người dùng vào nhóm input) hoặc môi trường desktop hỗ trợ portal RemoteDesktop."),
    // About
    ("An independent Linux client for UltraViewer interoperability, ported from MacViewer by thehieucf.", "Ứng dụng Linux độc lập, tương thích với UltraViewer, được thehieucf port từ MacViewer."),
    ("Ported from", "Port từ"),
    ("LinuxViewer is an independent implementation and not an official UltraViewer product. Only connect to computers you are authorized to access.", "LinuxViewer là bản triển khai độc lập, không phải sản phẩm chính thức của UltraViewer. Chỉ kết nối tới những máy tính bạn được phép truy cập."),
];
