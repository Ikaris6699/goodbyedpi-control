# GoodbyeDPI Control

Ứng dụng Windows viết bằng Rust, cung cấp giao diện trực quan cho GoodbyeDPI:

- Chỉ phân mảnh các domain/URL người dùng thêm vào danh sách.
- Ba mức độ `Nhẹ`, `Cân bằng`, `Mạnh` và chế độ `Tuỳ chỉnh`.
- Khởi động/dừng engine trực tiếp từ giao diện.
- Khởi động cùng Windows và chạy nền trong system tray.
- Nhập danh sách domain từ file `.txt`.
- Đóng gói engine, WinDivert DLL và driver vào một file `.exe`.

## Build bản một file

Yêu cầu Windows, Rust stable và PowerShell:

```powershell
cd gui
.\build.ps1
```

Script lấy asset `.zip` mới nhất từ release chính thức của GoodbyeDPI, đặt ba file runtime vào `assets/runtime/`, sau đó nhúng chúng vào `goodbyedpi-control.exe`. Khi chạy, ứng dụng giải nén runtime vào thư mục tạm riêng rồi khởi động engine với `--blacklist`.

Nếu đã có sẵn ba file runtime, dùng:

```powershell
.\build.ps1 -SkipRuntimeDownload
```

Đầu ra: `gui/target/release/goodbyedpi-control.exe`.

## Lưu ý

Ứng dụng yêu cầu quyền quản trị viên vì engine gốc sử dụng WinDivert. Chế độ `Mạnh` có thể kém tương thích với một số mạng hoặc thiết bị; nên bắt đầu bằng `Cân bằng`.

