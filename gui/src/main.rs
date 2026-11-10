#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    env,
    fs,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use directories::ProjectDirs;
use eframe::egui::{self, Align, Color32, Context, Layout, RichText, ScrollArea, Ui, Vec2};
use eframe::{App as EframeApp, CreationContext, Frame, NativeOptions};
use serde::{Deserialize, Serialize};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

mod embedded_runtime {
    include!(concat!(env!("OUT_DIR"), "/embedded_runtime.rs"));
}

const APP_NAME: &str = "GoodbyeDPI Control";
const CONFIG_FILE: &str = "settings.json";
const BLACKLIST_FILE: &str = "domains.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Intensity {
    Light,
    Balanced,
    Strong,
    Custom,
}

impl Default for Intensity {
    fn default() -> Self {
        Self::Balanced
    }
}

impl Intensity {
    fn title(self) -> &'static str {
        match self {
            Self::Light => "Nhẹ",
            Self::Balanced => "Cân bằng",
            Self::Strong => "Mạnh",
            Self::Custom => "Tuỳ chỉnh",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Light => "Chia mảnh cơ bản, ưu tiên tương thích.",
            Self::Balanced => "Cấu hình khuyến nghị cho hầu hết mạng.",
            Self::Strong => "Thêm kỹ thuật giả gói, có thể kém tương thích hơn.",
            Self::Custom => "Tự chọn từng kỹ thuật xử lý.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    domains: Vec<String>,
    intensity: Intensity,
    http_fragment: u16,
    https_fragment: u16,
    reverse_fragment: bool,
    wrong_sequence: bool,
    wrong_checksum: bool,
    block_quic: bool,
    start_with_windows: bool,
    minimize_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            domains: Vec::new(),
            intensity: Intensity::Balanced,
            http_fragment: 2,
            https_fragment: 2,
            reverse_fragment: true,
            wrong_sequence: true,
            wrong_checksum: false,
            block_quic: false,
            start_with_windows: false,
            minimize_to_tray: true,
        }
    }
}

impl Settings {
    fn apply_preset(&mut self, intensity: Intensity) {
        self.intensity = intensity;
        match intensity {
            Intensity::Light => {
                self.http_fragment = 2;
                self.https_fragment = 2;
                self.reverse_fragment = true;
                self.wrong_sequence = false;
                self.wrong_checksum = false;
                self.block_quic = false;
            }
            Intensity::Balanced => {
                self.http_fragment = 2;
                self.https_fragment = 2;
                self.reverse_fragment = true;
                self.wrong_sequence = true;
                self.wrong_checksum = false;
                self.block_quic = false;
            }
            Intensity::Strong => {
                self.http_fragment = 2;
                self.https_fragment = 2;
                self.reverse_fragment = true;
                self.wrong_sequence = true;
                self.wrong_checksum = true;
                self.block_quic = true;
            }
            Intensity::Custom => {}
        }
    }

    fn cli_args(&self, blacklist: &Path) -> Vec<String> {
        let mut args = vec![
            "-f".into(),
            self.http_fragment.to_string(),
            "-e".into(),
            self.https_fragment.to_string(),
            "--max-payload".into(),
            "1200".into(),
        ];

        if self.reverse_fragment {
            args.push("--reverse-frag".into());
        }
        if self.wrong_sequence {
            args.push("--wrong-seq".into());
        }
        if self.wrong_checksum {
            args.push("--wrong-chksum".into());
        }
        if self.block_quic {
            args.push("-q".into());
        }
        args.push("--blacklist".into());
        args.push(blacklist.to_string_lossy().into_owned());
        args
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Websites,
    Settings,
}

struct App {
    settings: Settings,
    page: Page,
    pending_domain: String,
    error: Option<String>,
    notice: Option<(String, Instant)>,
    child: Option<Child>,
    runtime_dir: Option<PathBuf>,
    tray: Option<TrayIcon>,
    show_window_requested: bool,
    start_minimized: bool,
    settings_path: PathBuf,
    blacklist_path: PathBuf,
}

impl App {
    fn new(cc: &CreationContext<'_>) -> Self {
        configure_theme(&cc.egui_ctx);
        let data_dir = ProjectDirs::from("com", "GoodbyeDPI", "Control")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let _ = fs::create_dir_all(&data_dir);
        let settings_path = data_dir.join(CONFIG_FILE);
        let blacklist_path = data_dir.join(BLACKLIST_FILE);
        let settings = load_settings(&settings_path);
        let start_minimized = env::args().any(|arg| arg == "--minimized");
        let tray = create_tray();
        Self {
            settings,
            page: Page::Overview,
            pending_domain: String::new(),
            error: None,
            notice: None,
            child: None,
            runtime_dir: None,
            tray,
            show_window_requested: false,
            start_minimized,
            settings_path,
            blacklist_path,
        }
    }

    fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => {
                    self.child = None;
                    false
                }
                Ok(None) => true,
            }
        } else {
            false
        }
    }

    fn save(&mut self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.settings) {
            if let Err(err) = fs::write(&self.settings_path, json) {
                self.error = Some(format!("Không lưu được cấu hình: {err}"));
            }
        }
    }

    fn notify(&mut self, message: impl Into<String>) {
        self.notice = Some((message.into(), Instant::now()));
    }

    fn start(&mut self) {
        if self.is_running() {
            return;
        }
        self.error = None;
        if self.settings.domains.is_empty() {
            self.error = Some("Hãy thêm ít nhất một website trước khi khởi động.".into());
            self.page = Page::Websites;
            return;
        }
        if let Err(err) = write_blacklist(&self.blacklist_path, &self.settings.domains) {
            self.error = Some(format!("Không tạo được danh sách domain: {err}"));
            return;
        }
        self.save();

        let backend = match prepare_runtime(&mut self.runtime_dir) {
            Ok(path) => path,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        let args = self.settings.cli_args(&self.blacklist_path);
        let mut command = Command::new(backend);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        match command.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.notify("Đã khởi động phân mảnh cho các website đã chọn.");
            }
            Err(err) => self.error = Some(format!("Không thể khởi động engine: {err}")),
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            self.notify("Đã dừng GoodbyeDPI.");
        }
    }

    fn add_domain(&mut self) {
        match normalize_domain(&self.pending_domain) {
            Ok(domain) if !self.settings.domains.iter().any(|item| item == &domain) => {
                self.settings.domains.push(domain);
                self.settings.domains.sort();
                self.pending_domain.clear();
                self.save();
                self.notify("Đã thêm website vào danh sách.");
            }
            Ok(_) => {
                self.pending_domain.clear();
                self.error = Some("Website này đã có trong danh sách.".into());
            }
            Err(err) => self.error = Some(err),
        }
    }

    fn hide_window(&mut self, ctx: &Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn render_header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("GOODBYEDPI").strong().size(21.0).color(Color32::from_rgb(123, 224, 164)));
            ui.label(RichText::new("CONTROL").strong().size(21.0).color(Color32::from_rgb(226, 232, 240)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Ẩn vào khay").clicked() {
                    self.hide_window(ui.ctx());
                }
                let running = self.is_running();
                let label = if running { "● Đang chạy" } else { "○ Đang dừng" };
                ui.colored_label(if running { Color32::from_rgb(123, 224, 164) } else { Color32::from_rgb(148, 163, 184) }, label);
            });
        });
        ui.add_space(8.0);
        ui.separator();
    }

    fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.set_width(210.0);
        ui.label(RichText::new("BẢNG ĐIỀU KHIỂN").small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(8.0);
        nav_button(ui, &mut self.page, Page::Overview, "Tổng quan", "⌂");
        nav_button(ui, &mut self.page, Page::Websites, "Website phân mảnh", "◎");
        nav_button(ui, &mut self.page, Page::Settings, "Cài đặt", "⚙");
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new("MỨC ĐỘ HIỆN TẠI").small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(5.0);
        ui.label(RichText::new(self.settings.intensity.title()).size(17.0).strong().color(Color32::from_rgb(123, 224, 164)));
        ui.label(RichText::new(self.settings.intensity.description()).small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new("LƯU Ý").small().color(Color32::from_rgb(148, 163, 184)));
        ui.label(RichText::new("Ứng dụng cần quyền quản trị viên để nạp WinDivert.").small().color(Color32::from_rgb(203, 213, 225)));
    }

    fn render_overview(&mut self, ui: &mut Ui) {
        ui.heading("Tổng quan");
        ui.label(RichText::new("Chỉ phân mảnh những website bạn chọn, với cấu hình rõ ràng và có thể thay đổi bất cứ lúc nào.").color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(22.0);
        ui.horizontal(|ui| {
            stat_card(ui, "WEBSITE", self.settings.domains.len().to_string(), "đã chọn");
            stat_card(ui, "MỨC ĐỘ", self.settings.intensity.title().to_owned(), "phân mảnh");
            stat_card(ui, "TRẠNG THÁI", if self.is_running() { "Đang chạy".into() } else { "Đã dừng".into() }, "engine");
        });
        ui.add_space(24.0);
        section(ui, "Khởi động nhanh", |ui| {
            ui.label("Chọn mức độ phù hợp rồi bấm Khởi động. Danh sách domain sẽ được áp dụng qua --blacklist của GoodbyeDPI.");
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                for intensity in [Intensity::Light, Intensity::Balanced, Intensity::Strong] {
                    let selected = self.settings.intensity == intensity;
                    if ui.selectable_label(selected, intensity.title()).clicked() {
                        self.settings.apply_preset(intensity);
                        self.save();
                    }
                }
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let running = self.is_running();
                let button = if running { ui.button("■  Dừng engine") } else { ui.button("▶  Khởi động") };
                if button.clicked() {
                    if running { self.stop(); } else { self.start(); }
                }
                if ui.button("Mở danh sách website").clicked() {
                    self.page = Page::Websites;
                }
            });
        });
        ui.add_space(16.0);
        section(ui, "Cấu hình đang dùng", |ui| {
            let mut items = vec![
                format!("HTTP: chia mảnh mức {}", self.settings.http_fragment),
                format!("HTTPS: chia mảnh mức {}", self.settings.https_fragment),
                "Chỉ áp dụng cho blacklist".to_string(),
            ];
            if self.settings.reverse_fragment { items.push("Đảo thứ tự mảnh".into()); }
            if self.settings.wrong_sequence { items.push("SEQ giả".into()); }
            if self.settings.wrong_checksum { items.push("Checksum giả".into()); }
            if self.settings.block_quic { items.push("Chặn QUIC/HTTP3".into()); }
            ui.horizontal_wrapped(|ui| {
                for item in items {
                    ui.label(RichText::new(item).background_color(Color32::from_rgb(30, 48, 60)).color(Color32::from_rgb(191, 219, 254)));
                }
            });
        });
    }

    fn render_websites(&mut self, ui: &mut Ui) {
        ui.heading("Website phân mảnh");
        ui.label(RichText::new("Thêm domain hoặc URL. Ứng dụng sẽ chuẩn hoá thành hostname và bỏ qua đường dẫn phía sau.").color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            let response = ui.add_sized([430.0, 34.0], egui::TextEdit::singleline(&mut self.pending_domain).hint_text("ví dụ: https://example.com/path"));
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) { self.add_domain(); }
            if ui.button("＋ Thêm website").clicked() { self.add_domain(); }
        });
        ui.add_space(16.0);
        section(ui, &format!("Danh sách đã chọn ({})", self.settings.domains.len()), |ui| {
            if self.settings.domains.is_empty() {
                ui.label(RichText::new("Chưa có website nào. Hãy thêm domain để bật chế độ phân mảnh có chọn lọc.").color(Color32::from_rgb(148, 163, 184)));
            } else {
                let mut remove_at = None;
                ScrollArea::vertical().max_height(310.0).show(ui, |ui| {
                    for (index, domain) in self.settings.domains.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(Color32::from_rgb(123, 224, 164)));
                            ui.label(domain);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Xoá").clicked() { remove_at = Some(index); }
                            });
                        });
                        ui.separator();
                    }
                });
                if let Some(index) = remove_at {
                    self.settings.domains.remove(index);
                    self.save();
                    self.notify("Đã xoá website khỏi danh sách.");
                }
            }
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Nhập từ file .txt").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).pick_file() {
                    match fs::read_to_string(path) {
                        Ok(content) => {
                            let mut added = 0;
                            for line in content.lines() {
                                if let Ok(domain) = normalize_domain(line) {
                                    if !self.settings.domains.contains(&domain) {
                                        self.settings.domains.push(domain);
                                        added += 1;
                                    }
                                }
                            }
                            self.settings.domains.sort();
                            self.save();
                            self.notify(format!("Đã nhập {added} website."));
                        }
                        Err(err) => self.error = Some(format!("Không đọc được file: {err}")),
                    }
                }
            }
            if ui.button("Xoá toàn bộ").clicked() {
                self.settings.domains.clear();
                self.save();
                self.notify("Đã xoá toàn bộ danh sách website.");
            }
        });
    }

    fn render_settings(&mut self, ui: &mut Ui) {
        ui.heading("Cài đặt");
        ui.label(RichText::new("Điều chỉnh cách ứng dụng khởi động và mức độ xử lý gói tin.").color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(18.0);
        section(ui, "Mức độ phân mảnh", |ui| {
            ui.horizontal_wrapped(|ui| {
                for intensity in [Intensity::Light, Intensity::Balanced, Intensity::Strong, Intensity::Custom] {
                    let selected = self.settings.intensity == intensity;
                    if ui.selectable_label(selected, intensity.title()).clicked() {
                        self.settings.apply_preset(intensity);
                        self.save();
                    }
                }
            });
            ui.add_space(8.0);
            ui.label(self.settings.intensity.description());
        });
        ui.add_space(14.0);
        section(ui, "Kỹ thuật xử lý", |ui| {
            ui.horizontal(|ui| {
                ui.label("HTTP");
                if ui.add(egui::DragValue::new(&mut self.settings.http_fragment).range(1..=32)).changed() { self.settings.intensity = Intensity::Custom; self.save(); }
                ui.label("HTTPS");
                if ui.add(egui::DragValue::new(&mut self.settings.https_fragment).range(1..=64)).changed() { self.settings.intensity = Intensity::Custom; self.save(); }
            });
            ui.add_space(8.0);
            let mut changed = false;
            for (label, value, help) in [
                ("Đảo thứ tự mảnh", &mut self.settings.reverse_fragment, "Tăng khả năng vượt DPI nhưng một số thiết bị cũ có thể kém tương thích."),
                ("SEQ giả", &mut self.settings.wrong_sequence, "Gửi thêm gói TCP có số thứ tự không hợp lệ."),
                ("Checksum giả", &mut self.settings.wrong_checksum, "Gửi thêm gói có TCP checksum không hợp lệ."),
                ("Chặn QUIC/HTTP3", &mut self.settings.block_quic, "Buộc trình duyệt quay về TCP/HTTP2 trong một số trường hợp."),
            ] {
                changed |= ui.checkbox(value, label).on_hover_text(help).changed();
            }
            if changed { self.settings.intensity = Intensity::Custom; self.save(); }
        });
        ui.add_space(14.0);
        section(ui, "Windows & khay hệ thống", |ui| {
            if ui.checkbox(&mut self.settings.start_with_windows, "Khởi động cùng Windows").changed() {
                match set_startup(self.settings.start_with_windows) {
                    Ok(()) => { self.save(); self.notify("Đã cập nhật tuỳ chọn khởi động cùng Windows."); }
                    Err(err) => self.error = Some(err),
                }
            }
            ui.label(RichText::new("Ứng dụng sẽ tự mở cùng Windows; cần bật tuỳ chọn thu nhỏ vào khay nếu muốn chạy nền.").small().color(Color32::from_rgb(148, 163, 184)));
            if ui.checkbox(&mut self.settings.minimize_to_tray, "Ưu tiên chạy nền trong khay hệ thống").changed() { self.save(); }
        });
    }
}

impl EframeApp for App {
    fn update(&mut self, ctx: &Context, _frame: &mut Frame) {
        let _ = self.is_running();
        ctx.request_repaint_after(Duration::from_millis(350));

        if self.start_minimized {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.start_minimized = false;
        }
        if self.show_window_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.show_window_requested = false;
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id.0.as_str() {
                "show" => self.show_window_requested = true,
                "start" => self.start(),
                "stop" => self.stop(),
                "quit" => {
                    self.stop();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                _ => {}
            }
        }

        egui::TopBottomPanel::top("header").frame(egui::Frame::none().fill(Color32::from_rgb(15, 23, 42)).inner_margin(egui::Margin::symmetric(22.0, 18.0))).show(ctx, |ui| self.render_header(ui));
        egui::SidePanel::left("sidebar").resizable(false).frame(egui::Frame::none().fill(Color32::from_rgb(15, 23, 42)).inner_margin(egui::Margin::symmetric(18.0, 20.0))).show(ctx, |ui| self.render_sidebar(ui));
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::from_rgb(10, 17, 31)).inner_margin(egui::Margin::symmetric(26.0, 24.0))).show(ctx, |ui| {
            match self.page {
                Page::Overview => self.render_overview(ui),
                Page::Websites => self.render_websites(ui),
                Page::Settings => self.render_settings(ui),
            }
            if let Some(error) = &self.error {
                ui.add_space(16.0);
                ui.colored_label(Color32::from_rgb(248, 113, 113), format!("⚠ {error}"));
            }
            if let Some((notice, at)) = &self.notice {
                if at.elapsed() < Duration::from_secs(4) {
                    ui.add_space(8.0);
                    ui.colored_label(Color32::from_rgb(123, 224, 164), format!("✓ {notice}"));
                } else {
                    self.notice = None;
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop();
    }
}

fn configure_theme(ctx: &Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgb(15, 23, 42);
    visuals.panel_fill = Color32::from_rgb(10, 17, 31);
    visuals.faint_bg_color = Color32::from_rgb(24, 37, 56);
    visuals.extreme_bg_color = Color32::from_rgb(7, 12, 23);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(30, 41, 59);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(44, 63, 83);
    visuals.widgets.active.bg_fill = Color32::from_rgb(34, 88, 70);
    ctx.set_visuals(visuals);
}

fn nav_button(ui: &mut Ui, page: &mut Page, target: Page, label: &str, glyph: &str) {
    let selected = *page == target;
    let response = ui.add_sized([170.0, 38.0], egui::Button::new(RichText::new(format!("{glyph}   {label}")).size(14.0)).selected(selected));
    if response.clicked() { *page = target; }
}

fn stat_card(ui: &mut Ui, title: &str, value: String, suffix: &str) {
    egui::Frame::group(ui.style()).fill(Color32::from_rgb(20, 33, 49)).inner_margin(egui::Margin::same(16.0)).show(ui, |ui| {
        ui.set_min_width(170.0);
        ui.label(RichText::new(title).small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(3.0);
        ui.label(RichText::new(value).size(22.0).strong().color(Color32::from_rgb(226, 232, 240)));
        ui.label(RichText::new(suffix).small().color(Color32::from_rgb(148, 163, 184)));
    });
}

fn section(ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui)) {
    egui::Frame::group(ui.style()).fill(Color32::from_rgb(16, 28, 44)).inner_margin(egui::Margin::same(16.0)).show(ui, |ui| {
        ui.label(RichText::new(title).strong().color(Color32::from_rgb(226, 232, 240)));
        ui.add_space(9.0);
        contents(ui);
    });
}

fn load_settings(path: &Path) -> Settings {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn write_blacklist(path: &Path, domains: &[String]) -> io::Result<()> {
    let mut contents = domains.join("\n");
    contents.push('\n');
    fs::write(path, contents)
}

fn normalize_domain(input: &str) -> Result<String, String> {
    let mut value = input.trim().to_lowercase();
    if value.is_empty() { return Err("Hãy nhập một domain hoặc URL.".into()); }
    if !value.contains("://") { value = format!("https://{value}"); }
    let parsed = url::Url::parse(&value).map_err(|_| "URL không hợp lệ. Ví dụ: example.com".to_string())?;
    let host = parsed.host_str().ok_or_else(|| "Không tìm thấy domain trong URL.".to_string())?;
    if host == "localhost" || host.parse::<std::net::IpAddr>().is_ok() || host.contains('.') {
        Ok(host.trim_end_matches('.').to_string())
    } else {
        Err("Domain cần có dạng example.com hoặc sub.example.com.".into())
    }
}

fn prepare_runtime(runtime_dir: &mut Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = runtime_dir.as_ref() {
        let executable = path.join("goodbyedpi.exe");
        if executable.is_file() {
            return Ok(executable);
        }
    }
    *runtime_dir = None;
    if let Some(exe) = embedded_runtime::EMBEDDED_GOODBYEDPI {
        let base = env::temp_dir().join("GoodbyeDPI-Control");
        fs::create_dir_all(&base).map_err(|err| format!("Không tạo được thư mục runtime: {err}"))?;
        fs::write(base.join("goodbyedpi.exe"), exe).map_err(|err| format!("Không ghi được engine: {err}"))?;
        fs::write(base.join("WinDivert.dll"), embedded_runtime::EMBEDDED_WINDIVERT_DLL.unwrap()).map_err(|err| format!("Không ghi được WinDivert.dll: {err}"))?;
        fs::write(base.join("WinDivert64.sys"), embedded_runtime::EMBEDDED_WINDIVERT_SYS.unwrap()).map_err(|err| format!("Không ghi được driver WinDivert: {err}"))?;
        *runtime_dir = Some(base.clone());
        return Ok(base.join("goodbyedpi.exe"));
    }
    let adjacent = env::current_exe().ok().and_then(|path| path.parent().map(|dir| dir.join("goodbyedpi.exe")));
    adjacent.filter(|path| path.is_file()).ok_or_else(|| "Chưa có engine GoodbyeDPI. Hãy chạy gui\\build.ps1 để tạo bản đóng gói một file.".to_string())
}

fn create_tray() -> Option<TrayIcon> {
    let menu = Menu::new();
    let show = MenuItem::with_id("show", "Mở cửa sổ", true, None);
    let start = MenuItem::with_id("start", "Khởi động", true, None);
    let stop = MenuItem::with_id("stop", "Dừng", true, None);
    let quit = MenuItem::with_id("quit", "Thoát", true, None);
    let _ = menu.append(&show);
    let _ = menu.append(&start);
    let _ = menu.append(&stop);
    let _ = menu.append(&quit);
    let icon = tray_icon_image()?;
    TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(APP_NAME).with_icon(icon).build().ok()
}

fn tray_icon_image() -> Option<Icon> {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let dx = x as i32 - 8;
            let dy = y as i32 - 8;
            let inside = dx * dx + dy * dy <= 49;
            let accent = inside && (x == 7 || x == 8 || (y > 7 && x > 5 && x < 11));
            if accent { rgba.extend_from_slice(&[123, 224, 164, 255]); }
            else if inside { rgba.extend_from_slice(&[30, 41, 59, 255]); }
            else { rgba.extend_from_slice(&[0, 0, 0, 0]); }
        }
    }
    Icon::from_rgba(rgba, 16, 16).ok()
}

#[cfg(windows)]
fn set_startup(enabled: bool) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run").map_err(|err| format!("Không truy cập được Windows Registry: {err}"))?;
    if enabled {
        let exe = env::current_exe().map_err(|err| format!("Không lấy được đường dẫn ứng dụng: {err}"))?;
        key.set_value(APP_NAME, &format!("\"{}\" --minimized", exe.display())).map_err(|err| format!("Không bật được khởi động cùng Windows: {err}"))?;
    } else {
        let _ = key.delete_value(APP_NAME);
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_startup(_enabled: bool) -> Result<(), String> {
    Err("Tính năng này chỉ hỗ trợ Windows.".into())
}

fn main() -> eframe::Result {
    let native_options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size(Vec2::new(1100.0, 740.0))
            .with_min_inner_size(Vec2::new(860.0, 600.0)),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, native_options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls_to_hostnames() {
        assert_eq!(normalize_domain("https://WWW.Example.com/path").unwrap(), "www.example.com");
        assert_eq!(normalize_domain("sub.example.com:8443").unwrap(), "sub.example.com");
    }

    #[test]
    fn rejects_invalid_domains() {
        assert!(normalize_domain("").is_err());
        assert!(normalize_domain("not-a-domain").is_err());
    }

    #[test]
    fn strong_preset_contains_expected_flags() {
        let mut settings = Settings::default();
        settings.apply_preset(Intensity::Strong);
        let args = settings.cli_args(Path::new("domains.txt"));
        assert!(args.contains(&"--wrong-seq".to_string()));
        assert!(args.contains(&"--wrong-chksum".to_string()));
        assert!(args.contains(&"-q".to_string()));
        assert!(args.contains(&"--blacklist".to_string()));
    }
}

