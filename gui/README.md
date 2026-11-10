# GoodbyeDPI Control

Ứng dụng Windows viết bằng Rust/egui, đóng gói giao diện và engine gốc vào một file `.exe`.

## Build

Trên Windows, cài Rust stable, mở PowerShell, vào thư mục `gui` rồi chạy:

```powershell
.\build.ps1
```

Script tải GoodbyeDPI bản `0.2.3rc3-2`, đối chiếu SHA256, chỉ lấy runtime `x86_64`, chạy unit test và tạo `target/release/goodbyedpi-control.exe`.

## Tính năng

- Hồ sơ Steam có sẵn bảy domain, có thể sửa; thêm `www.example.com` sẽ thêm cả domain gốc `example.com`.
- Tự dò tám cấu hình, thử kết nối HTTPS tới Steam Store và Steam Community.
- Kiểm tra Steam, một số website phổ biến và khả năng phân giải DNS.
- Chạy cùng Windows bằng Task Scheduler mức quyền cao; tự bật engine khi mở app.
- Khay hệ thống: click trái bật/tắt engine, menu chuột phải để mở, bật/tắt hoặc thoát app.
- Ghi log stdout/stderr, xem log bằng Notepad; Job Object dọn engine khi app đóng.
- DNS Cloudflare là tuỳ chọn, mặc định tắt. `-q` cũng tắt mặc định vì chặn QUIC/HTTP3 trên toàn máy.

## Lưu ý

GoodbyeDPI cần quyền Administrator để nạp WinDivert. Blacklist lọc theo domain cho mọi ứng dụng Windows, không giới hạn riêng tiến trình Steam. `src/` ở thư mục gốc là mã C upstream; build giao diện chỉ dùng Rust và runtime phát hành đã ghim. Giấy phép Apache-2.0 của repo được giữ nguyên.
