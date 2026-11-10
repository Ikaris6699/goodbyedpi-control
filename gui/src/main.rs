#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    env,
    fs,
    io,
    net::ToSocketAddrs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

use directories::ProjectDirs;
use eframe::egui::{self, Align, Color32, Context, Layout, RichText, ScrollArea, Ui, Vec2};
use eframe::{App as EframeApp, CreationContext, Frame, NativeOptions};
use serde::{Deserialize, Serialize};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

mod embedded_runtime {
    include!(concat!(env!("OUT_DIR"), "/embedded_runtime.rs"));
}

const APP_NAME: &str = "GoodbyeDPI Control";
const CONFIG_FILE: &str = "settings.json";
const BLACKLIST_FILE: &str = "domains.txt";
const STEAM_DOMAINS: &[&str] = &["steampowered.com", "steamcommunity.com", "steamstatic.com", "steamcontent.com", "steamserver.net", "steam-chat.com", "steamusercontent.com"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Language {
    Vietnamese,
    English,
}

impl Default for Language {
    fn default() -> Self { Self::Vietnamese }
}

impl Language {
    fn text(self, vietnamese: &'static str, english: &'static str) -> &'static str {
        match self { Self::Vietnamese => vietnamese, Self::English => english }
    }
}

#[derive(Debug, Clone, Copy)]
enum TrayAction { Show, Start, Stop, Toggle, Quit }

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
    fn title(self, language: Language) -> &'static str {
        match self {
            Self::Light => language.text("Tương thích", "Compatible"),
            Self::Balanced => language.text("Khuyến nghị", "Recommended"),
            Self::Strong => language.text("Mạnh nhất", "Strongest"),
            Self::Custom => language.text("Tuỳ chỉnh", "Custom"),
        }
    }

    fn description(self, language: Language) -> &'static str {
        match self {
            Self::Light => language.text("Chia mảnh cơ bản, ưu tiên tương thích.", "Basic fragmentation, prioritizes compatibility."),
            Self::Balanced => language.text("Cấu hình khuyến nghị cho hầu hết mạng.", "Recommended for most networks."),
            Self::Strong => language.text("Có thể làm một số kết nối lỗi.", "May break some connections."),
            Self::Custom => language.text("Tự chọn từng kỹ thuật xử lý.", "Choose each packet technique yourself."),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    language: Language,
    domains: Vec<String>,
    intensity: Intensity,
    http_fragment: u16,
    https_fragment: u16,
    reverse_fragment: bool,
    wrong_sequence: bool,
    wrong_checksum: bool,
    block_quic: bool,
    auto_ttl: bool,
    use_public_dns: bool,
    start_with_windows: bool,
    minimize_to_tray: bool,
    auto_start_engine: bool,
    steam_profile: bool,
    resume_engine: bool,
}

enum AutoMessage { Progress(String), Success(Child, Settings), Failed(String) }

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::Vietnamese,
            domains: STEAM_DOMAINS.iter().map(|s| (*s).to_string()).collect(),
            intensity: Intensity::Balanced,
            http_fragment: 2,
            https_fragment: 2,
            reverse_fragment: true,
            wrong_sequence: true,
            wrong_checksum: false,
            block_quic: false,
            auto_ttl: false,
            use_public_dns: false,
            start_with_windows: false,
            minimize_to_tray: true,
            auto_start_engine: true,
            steam_profile: true,
            resume_engine: false,
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
                self.block_quic = false;
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
        if self.auto_ttl { args.push("--auto-ttl".into()); }
        if self.use_public_dns { args.extend(["--dns-addr".into(), "1.1.1.1".into(), "--dns-port".into(), "53".into()]); }
        if self.block_quic {
            args.push("-q".into());
        }
        args.push("--blacklist".into());
        args.push(blacklist.to_string_lossy().into_owned());
        args
    }
}

fn preset_candidates(base: &Settings) -> Vec<Settings> {
    let mut candidates = Vec::new();
    for (wrong_sequence, wrong_checksum) in [(false, false), (true, false), (false, true), (true, true)] {
        for auto_ttl in [false, true] {
            let mut preset = base.clone();
            preset.intensity = Intensity::Custom;
            preset.wrong_sequence = wrong_sequence;
            preset.wrong_checksum = wrong_checksum;
            preset.auto_ttl = auto_ttl;
            preset.block_quic = false;
            candidates.push(preset);
        }
    }
    candidates
}

fn select_first_passing(results: &[bool]) -> Option<usize> { results.iter().position(|passed| *passed) }

fn probe(agent: &ureq::Agent, url: &str) -> bool {
    matches!(agent.get(url).call(), Ok(_) | Err(ureq::Error::Status(_, _)))
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
    tray_actions: Receiver<TrayAction>,
    show_window_requested: bool,
    start_minimized: bool,
    settings_path: PathBuf,
    blacklist_path: PathBuf,
    log_path: PathBuf,
    start_check_at: Option<Instant>,
    auto_start_pending: bool,
    log_text: String,
    quit_requested: bool,
    job: JobGuard,
    last_tray_running: Option<bool>,
    connection_rx: Option<Receiver<Vec<(String, bool)>>>,
    connection_results: Option<Vec<(String, bool)>>,
    auto_rx: Option<Receiver<AutoMessage>>,
    auto_progress: Option<String>,
}

#[cfg(windows)]
struct JobGuard(windows_sys::Win32::Foundation::HANDLE);
#[cfg(not(windows))]
struct JobGuard;

impl JobGuard {
    fn new() -> Self {
        #[cfg(windows)] unsafe {
            use windows_sys::Win32::System::JobObjects::{CreateJobObjectW, SetInformationJobObject, JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE};
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if !handle.is_null() {
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(handle, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32) == 0 {
                    let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
                    return Self(std::ptr::null_mut());
                }
            }
            Self(handle)
        }
        #[cfg(not(windows))] { Self }
    }

    fn assign(&self, process_id: u32) -> Result<(), String> {
        #[cfg(windows)] unsafe {
            use windows_sys::Win32::{Foundation::CloseHandle, System::{JobObjects::AssignProcessToJobObject, Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE}}};
            if self.0.is_null() { return Err("Windows không tạo được Job Object".into()); }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, process_id);
            if process.is_null() { return Err(std::io::Error::last_os_error().to_string()); }
            let assigned = AssignProcessToJobObject(self.0, process) != 0;
            let _ = CloseHandle(process);
            if assigned { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) }
        }
        #[cfg(not(windows))] { let _ = process_id; Ok(()) }
    }
}

#[cfg(windows)]
struct SingleInstanceGuard(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) { unsafe { let _ = windows_sys::Win32::Foundation::CloseHandle(self.0); } }
}

#[cfg(windows)]
fn claim_single_instance() -> Option<SingleInstanceGuard> {
    use windows_sys::Win32::{Foundation::{CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS}, System::Threading::CreateMutexW};
    let name: Vec<u16> = "Local\\GoodbyeDPI-Control-SingleInstance\0".encode_utf16().collect();
    unsafe {
        SetLastError(0);
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() { return None; }
        if GetLastError() == ERROR_ALREADY_EXISTS { let _ = CloseHandle(handle); None } else { Some(SingleInstanceGuard(handle)) }
    }
}
#[cfg(not(windows))]
fn claim_single_instance() -> Option<()> { Some(()) }

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe { if !self.0.is_null() { let _ = windows_sys::Win32::Foundation::CloseHandle(self.0); } }
    }
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
        let mut settings = load_settings(&settings_path);
        if settings.domains.is_empty() { settings.domains = Settings::default().domains; }
        let _ = set_startup(settings.start_with_windows, settings.minimize_to_tray);
        let log_path = data_dir.join("engine.log");
        let auto_start_pending = settings.auto_start_engine || settings.resume_engine;
        let job = JobGuard::new();
        let start_minimized = env::args().any(|arg| arg == "--minimized");
        cleanup_orphaned_engines();
        let (tray_sender, tray_actions) = mpsc::channel();
        let tray = create_tray(tray_sender, cc.egui_ctx.clone(), settings.language);
        Self {
            settings,
            page: Page::Overview,
            pending_domain: String::new(),
            error: None,
            notice: None,
            child: None,
            runtime_dir: None,
            tray,
            tray_actions,
            show_window_requested: false,
            start_minimized,
            settings_path,
            blacklist_path,
            log_path,
            start_check_at: None,
            auto_start_pending,
            log_text: String::new(),
            quit_requested: false,
            job,
            last_tray_running: None,
            connection_rx: None,
            connection_results: None,
            auto_rx: None,
            auto_progress: None,
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
        let lang = self.settings.language;
        if self.is_running() {
            return;
        }
        self.error = None;
        if self.settings.domains.is_empty() {
            self.error = Some(lang.text("Hãy thêm ít nhất một website trước khi khởi động.", "Add at least one website before starting.").into());
            self.page = Page::Websites;
            return;
        }
        if let Err(err) = write_blacklist(&self.blacklist_path, &self.settings.domains) {
            self.error = Some(format!("{}: {err}", lang.text("Không tạo được danh sách domain", "Could not create the domain list")));
            return;
        }
        let _ = fs::remove_file(&self.log_path);
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        match command.spawn() {
            Ok(mut child) => {
                if let Err(err) = self.job.assign(child.id()) {
                    let mut child = child;
                    let _ = child.kill();
                    self.error = Some(format!("Không quản lý được tiến trình engine: {err}"));
                    return;
                }
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let log_path = self.log_path.clone();
                if let Some(out) = stdout { pipe_engine_log(out, log_path.clone()); }
                if let Some(err) = stderr { pipe_engine_log(err, log_path); }
                self.child = Some(child);
                self.start_check_at = Some(Instant::now());
                self.log_text.clear();
                self.settings.resume_engine = true;
                self.save();
            }
            Err(err) => self.error = Some(format!("Không thể khởi động engine: {err}")),
        }
    }

    fn check_connections(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.connection_rx = Some(rx);
        self.connection_results = None;
        std::thread::spawn(move || {
            let sites = [
                ("Steam Store", "https://store.steampowered.com/"),
                ("Steam Community", "https://steamcommunity.com/"),
                ("Wikipedia", "https://www.wikipedia.org/"),
                ("Example", "https://example.com/"),
                ("GitHub", "https://github.com/"),
            ];
            let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(3)).timeout_read(Duration::from_secs(3)).timeout_write(Duration::from_secs(3)).build();
            let results = sites.iter().map(|(name, url)| {
                let reachable = match agent.get(url).call() { Ok(_) | Err(ureq::Error::Status(_, _)) => true, Err(_) => false };
                ((*name).to_string(), reachable)
            }).collect();
            let mut results = results;
            for domain in ["store.steampowered.com", "steamcommunity.com"] {
                let resolved = (domain, 443).to_socket_addrs().map(|mut addresses| addresses.next().is_some()).unwrap_or(false);
                results.push((format!("DNS {domain}"), resolved));
            }
            let _ = tx.send(results);
        });
    }

    fn auto_detect(&mut self) {
        if self.auto_rx.is_some() { return; }
        if self.is_running() { self.stop(); }
        let backend = match prepare_runtime(&mut self.runtime_dir) { Ok(path) => path, Err(err) => { self.error = Some(err); return; } };
        if let Err(err) = write_blacklist(&self.blacklist_path, &self.settings.domains) { self.error = Some(err.to_string()); return; }
        let base = self.settings.clone();
        let blacklist = self.blacklist_path.clone();
        let log_path = self.log_path.clone();
        let _ = fs::remove_file(&log_path);
        let (tx, rx) = mpsc::channel();
        self.auto_rx = Some(rx);
        self.auto_progress = Some(self.settings.language.text("Chuẩn bị các cấu hình thử…", "Preparing configuration tests…").into());
        std::thread::spawn(move || {
            let candidates = preset_candidates(&base);
            for (index, candidate) in candidates.into_iter().enumerate() {
                let label = format!("{} {}/8 — {}", base.language.text("Thử cấu hình", "Testing mode"), index + 1, format!("-{} {}", index / 2 + 5, if candidate.auto_ttl { "+ auto TTL" } else { "" }));
                let _ = tx.send(AutoMessage::Progress(label));
                let mut cmd = Command::new(&backend);
                cmd.args(candidate.cli_args(&blacklist)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
                #[cfg(windows)] { use std::os::windows::process::CommandExt; cmd.creation_flags(0x08000000); }
                let mut child = match cmd.spawn() { Ok(c) => c, Err(err) => { let _ = tx.send(AutoMessage::Failed(err.to_string())); return; } };
                if let Some(out) = child.stdout.take() { pipe_engine_log(out, log_path.clone()); }
                if let Some(err) = child.stderr.take() { pipe_engine_log(err, log_path.clone()); }
                std::thread::sleep(Duration::from_millis(1400));
                if child.try_wait().ok().flatten().is_some() { continue; }
                let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout_read(Duration::from_secs(2)).timeout_write(Duration::from_secs(2)).build();
                let store = probe(&agent, "https://store.steampowered.com/");
                let community = probe(&agent, "https://steamcommunity.com/");
                if store && community && child.try_wait().ok().flatten().is_none() {
                    candidate.intensity = Intensity::Custom;
                    let _ = tx.send(AutoMessage::Success(child, candidate));
                    return;
                }
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = tx.send(AutoMessage::Failed("Không tìm thấy cấu hình truy cập được cả Steam Store và Community. Hãy kiểm tra log/quyền quản trị và thử mức Khuyến nghị.".into()));
        });
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            self.notify("Đã dừng GoodbyeDPI.");
        }
        self.settings.resume_engine = false;
        self.save();
        self.start_check_at = None;
    }

    fn add_domain(&mut self) {
        let lang = self.settings.language;
        match normalize_domain_additions(&self.pending_domain) {
            Ok(additions) if additions.iter().any(|item| !self.settings.domains.contains(item)) => {
                for domain in additions { if !self.settings.domains.contains(&domain) { self.settings.domains.push(domain); } }
                self.settings.steam_profile = false;
                self.settings.domains.sort();
                self.pending_domain.clear();
                self.save();
                self.notify(lang.text("Đã thêm website vào danh sách.", "Website added to the list."));
            }
            Ok(_) => {
                self.pending_domain.clear();
                self.error = Some(lang.text("Website này đã có trong danh sách.", "This website is already in the list.").into());
            }
            Err(err) => self.error = Some(err),
        }
    }

    fn hide_window(&mut self, ctx: &Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn render_header(&mut self, ui: &mut Ui) {
        let lang = self.settings.language;
        ui.horizontal(|ui| {
            ui.label(RichText::new("GOODBYEDPI").strong().size(21.0).color(Color32::from_rgb(123, 224, 164)));
            ui.label(RichText::new("CONTROL").strong().size(21.0).color(Color32::from_rgb(226, 232, 240)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(lang.text("Ẩn vào khay", "Hide to tray")).clicked() {
                    self.hide_window(ui.ctx());
                }
                let running = self.is_running();
                let label = if running { lang.text("● Đang chạy", "● Running") } else { lang.text("○ Đang dừng", "○ Stopped") };
                ui.colored_label(if running { Color32::from_rgb(123, 224, 164) } else { Color32::from_rgb(148, 163, 184) }, label);
            });
        });
        ui.add_space(8.0);
        ui.separator();
    }

    fn render_sidebar(&mut self, ui: &mut Ui) {
        let lang = self.settings.language;
        ui.set_width(210.0);
        ui.label(RichText::new(lang.text("BẢNG ĐIỀU KHIỂN", "CONTROL PANEL")).small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(8.0);
        nav_button(ui, &mut self.page, Page::Overview, lang.text("Tổng quan", "Overview"), "⌂");
        nav_button(ui, &mut self.page, Page::Websites, lang.text("Website phân mảnh", "Selected websites"), "◎");
        nav_button(ui, &mut self.page, Page::Settings, lang.text("Cài đặt", "Settings"), "⚙");
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new(lang.text("MỨC ĐỘ HIỆN TẠI", "CURRENT LEVEL")).small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(5.0);
        ui.label(RichText::new(self.settings.intensity.title(lang)).size(17.0).strong().color(Color32::from_rgb(123, 224, 164)));
        ui.label(RichText::new(self.settings.intensity.description(lang)).small().color(Color32::from_rgb(148, 163, 184)));
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new(lang.text("LƯU Ý", "NOTE")).small().color(Color32::from_rgb(148, 163, 184)));
        ui.label(RichText::new(lang.text("Ứng dụng cần quyền quản trị viên để nạp WinDivert.", "Run as administrator to load WinDivert.")).small().color(Color32::from_rgb(203, 213, 225)));
    }

    fn render_overview(&mut self, ui: &mut Ui) {
        let lang = self.settings.language;
        let running = self.child.is_some();
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(if running { lang.text("Steam đang được hỗ trợ", "Steam protection is on") } else { lang.text("Steam chưa được hỗ trợ", "Steam protection is off") }).size(20.0).strong());
            ui.label(format!("{} {}", self.settings.domains.len(), lang.text("website trong hồ sơ Steam", "sites in the Steam profile")));
            ui.add_space(16.0);
            let button = egui::Button::new(RichText::new(if running { lang.text("TẮT", "TURN OFF") } else { lang.text("BẬT STEAM", "TURN STEAM ON") }).size(22.0).strong())
                .fill(if running { Color32::from_rgb(83, 55, 61) } else { Color32::from_rgb(0, 105, 124) });
            if ui.add_sized([ui.available_width(), 82.0], button).clicked() { if running { self.stop(); } else { self.start(); } }
            ui.add_space(16.0);
        });
        egui::ComboBox::from_label(lang.text("Hồ sơ website", "Website profile")).selected_text(if self.settings.steam_profile { "Steam" } else { lang.text("Tuỳ chỉnh", "Custom") }).show_ui(ui, |ui| {
            if ui.selectable_label(self.settings.steam_profile, "Steam").clicked() { self.settings.steam_profile = true; self.settings.domains = Settings::default().domains; self.save(); }
            if ui.selectable_label(!self.settings.steam_profile, lang.text("Tuỳ chỉnh", "Custom")).clicked() { self.settings.steam_profile = false; self.save(); }
        });
        ui.label(lang.text("Mức độ", "Mode"));
        ui.horizontal_wrapped(|ui| {
            for intensity in [Intensity::Light, Intensity::Balanced, Intensity::Strong] {
                if ui.selectable_label(self.settings.intensity == intensity, intensity.title(lang)).clicked() { self.settings.apply_preset(intensity); self.save(); }
            }
        });
        ui.add_space(12.0);
        if ui.button(lang.text("Tự dò cấu hình Steam", "Auto-detect Steam settings")).clicked() { self.auto_detect(); }
        if let Some(progress) = &self.auto_progress { ui.label(progress); }
        if ui.button(lang.text("Kiểm tra kết nối", "Check connection")).clicked() { self.check_connections(); }
        if let Some(results) = &self.connection_results {
            for (site, reachable) in results { ui.colored_label(if *reachable { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }, format!("{}  {}", if *reachable { "✓" } else { "✗" }, site)); }
            if results.iter().any(|(site, ok)| site.starts_with("DNS ") && !ok) { ui.label(lang.text("DNS không phân giải được Steam; mở Nâng cao để thử DNS Cloudflare.", "Steam DNS lookup failed; try Cloudflare DNS in Advanced.")); }
        }
        ui.horizontal(|ui| {
            if ui.button(lang.text("Sửa danh sách Steam", "Edit Steam sites")).clicked() { self.page = Page::Websites; }
            if ui.button(lang.text("Xem log", "View log")).clicked() { open_log(&self.log_path); }
        });
    }

    fn render_websites(&mut self, ui: &mut Ui) {
        let lang = self.settings.language;
        ui.heading(lang.text("Website phân mảnh", "Selected websites"));
        ui.label(RichText::new(lang.text("Thêm domain hoặc URL. Ứng dụng sẽ chuẩn hoá thành hostname và bỏ qua đường dẫn phía sau.", "Add a domain or URL. The app converts it to a hostname and ignores the path.")).color(Color32::from_rgb(148, 163, 184)));
        ui.label(RichText::new(lang.text("Lọc theo domain áp dụng cho mọi ứng dụng. Engine hiện chưa hỗ trợ giới hạn theo tiến trình như Steam.", "Domain filtering applies to all apps. The engine does not currently support restricting traffic to a process such as Steam.")).small().color(Color32::from_rgb(250, 204, 110)));
        ui.add_space(18.0);
        egui::ComboBox::from_label(lang.text("Hồ sơ", "Profile")).selected_text(if self.settings.steam_profile { "Steam" } else { lang.text("Tuỳ chỉnh", "Custom") }).show_ui(ui, |ui| {
            if ui.selectable_label(self.settings.steam_profile, "Steam").clicked() { self.settings.steam_profile = true; self.settings.domains = Settings::default().domains; self.save(); }
            if ui.selectable_label(!self.settings.steam_profile, lang.text("Tuỳ chỉnh", "Custom")).clicked() { self.settings.steam_profile = false; self.save(); }
        });
        ui.horizontal(|ui| {
            let response = ui.add_sized([ui.available_width() - 90.0, 34.0], egui::TextEdit::singleline(&mut self.pending_domain).hint_text(lang.text("ví dụ: example.com", "e.g. example.com")));
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) { self.add_domain(); }
            if ui.button(lang.text("＋ Thêm website", "＋ Add website")).clicked() { self.add_domain(); }
        });
        ui.add_space(16.0);
        section(ui, &format!("{} ({})", lang.text("Danh sách đã chọn", "Selected list"), self.settings.domains.len()), |ui| {
            if self.settings.domains.is_empty() {
                ui.label(RichText::new(lang.text("Chưa có website nào. Hãy thêm domain để bật chế độ phân mảnh có chọn lọc.", "No websites yet. Add a domain to enable selective fragmentation.")).color(Color32::from_rgb(148, 163, 184)));
            } else {
                let mut remove_at = None;
                ScrollArea::vertical().max_height(310.0).show(ui, |ui| {
                    for (index, domain) in self.settings.domains.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(Color32::from_rgb(123, 224, 164)));
                            ui.label(domain);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(lang.text("Xoá", "Remove")).clicked() { remove_at = Some(index); }
                            });
                        });
                        ui.separator();
                    }
                });
                if let Some(index) = remove_at {
                    self.settings.domains.remove(index);
                    self.settings.steam_profile = false;
                    self.save();
                    self.notify(lang.text("Đã xoá website khỏi danh sách.", "Website removed from the list."));
                }
            }
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button(lang.text("Nhập từ file .txt", "Import from .txt")).clicked() {
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
                            self.settings.steam_profile = false;
                            self.save();
                            self.notify(format!("{} {added} {}", lang.text("Đã nhập", "Imported"), lang.text("website.", "websites.")));
                        }
                        Err(err) => self.error = Some(format!("Không đọc được file: {err}")),
                    }
                }
            }
            if ui.button(lang.text("Xoá toàn bộ", "Clear all")).clicked() {
                self.settings.domains.clear();
                self.settings.steam_profile = false;
                self.save();
                self.notify(lang.text("Đã xoá toàn bộ danh sách website.", "All websites removed."));
            }
        });
    }

    fn render_settings(&mut self, ui: &mut Ui) {
        let lang = self.settings.language;
        let language_before = lang;
        ui.heading(lang.text("Cài đặt", "Settings"));
        ui.label(RichText::new(lang.text("Điều chỉnh cách ứng dụng khởi động và mức độ xử lý gói tin.", "Configure startup and packet processing options.")).color(Color32::from_rgb(148, 163, 184)));
        egui::ComboBox::from_label(lang.text("Ngôn ngữ", "Language"))
            .selected_text(match lang { Language::Vietnamese => "Tiếng Việt", Language::English => "English" })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.language, Language::Vietnamese, "Tiếng Việt");
                ui.selectable_value(&mut self.settings.language, Language::English, "English");
            });
        if self.settings.language != language_before { self.save(); }
        ui.add_space(18.0);
        section(ui, lang.text("Mức độ phân mảnh", "Fragmentation level"), |ui| {
            ui.horizontal_wrapped(|ui| {
                for intensity in [Intensity::Light, Intensity::Balanced, Intensity::Strong, Intensity::Custom] {
                    let selected = self.settings.intensity == intensity;
                    if ui.selectable_label(selected, intensity.title(lang)).clicked() {
                        self.settings.apply_preset(intensity);
                        self.save();
                    }
                }
            });
            ui.add_space(8.0);
            ui.label(self.settings.intensity.description(lang));
        });
        ui.add_space(14.0);
        section(ui, lang.text("Kỹ thuật xử lý", "Processing techniques"), |ui| {
            ui.horizontal(|ui| {
                ui.label("HTTP");
                if ui.add(egui::DragValue::new(&mut self.settings.http_fragment).range(1..=32)).changed() { self.settings.intensity = Intensity::Custom; self.save(); }
                ui.label("HTTPS");
                if ui.add(egui::DragValue::new(&mut self.settings.https_fragment).range(1..=64)).changed() { self.settings.intensity = Intensity::Custom; self.save(); }
            });
            ui.add_space(8.0);
            let mut changed = false;
            for (label, value, help) in [
                (lang.text("Đảo thứ tự mảnh", "Reverse fragment order"), &mut self.settings.reverse_fragment, lang.text("Tăng khả năng vượt DPI nhưng một số thiết bị cũ có thể kém tương thích.", "May improve DPI bypass; some older devices may be less compatible.")),
                (lang.text("SEQ giả", "Fake SEQ"), &mut self.settings.wrong_sequence, lang.text("Gửi thêm gói TCP có số thứ tự không hợp lệ.", "Sends an extra TCP packet with an invalid sequence number.")),
                (lang.text("Checksum giả", "Fake checksum"), &mut self.settings.wrong_checksum, lang.text("Gửi thêm gói có TCP checksum không hợp lệ.", "Sends an extra packet with an invalid TCP checksum.")),
                (lang.text("Chặn QUIC/HTTP3", "Block QUIC/HTTP3"), &mut self.settings.block_quic, lang.text("Buộc trình duyệt quay về TCP/HTTP2 trong một số trường hợp.", "May force browsers to fall back to TCP/HTTP2.")),
            ] {
                changed |= ui.checkbox(value, label).on_hover_text(help).changed();
            }
            if changed { self.settings.intensity = Intensity::Custom; self.save(); }
            if ui.checkbox(&mut self.settings.use_public_dns, lang.text("Dùng DNS Cloudflare (1.1.1.1)", "Use Cloudflare DNS (1.1.1.1)")).on_hover_text(lang.text("Mặc định tắt. Đổi DNS có thể giúp khi DNS nhà mạng trả sai; tuỳ chọn này không kiểm tra trực tiếp DNS nhà mạng.", "Off by default. This may help when ISP DNS returns incorrect results; it does not directly test ISP DNS.")).changed() { self.save(); }
            ui.label(RichText::new(lang.text("-q chặn QUIC/HTTP3 trên toàn máy, không chỉ các domain đã chọn.", "-q blocks QUIC/HTTP3 system-wide, not only selected domains.")).small().color(Color32::YELLOW));
        });
        ui.add_space(14.0);
        section(ui, lang.text("Windows & khay hệ thống", "Windows & system tray"), |ui| {
            if ui.checkbox(&mut self.settings.start_with_windows, lang.text("Khởi động cùng Windows", "Start with Windows")).changed() {
                match set_startup(self.settings.start_with_windows, self.settings.minimize_to_tray) {
                    Ok(()) => { self.save(); self.notify(lang.text("Đã cập nhật tuỳ chọn khởi động cùng Windows.", "Windows startup preference updated.")); }
                    Err(err) => self.error = Some(err),
                }
            }
            ui.label(RichText::new(lang.text("Ứng dụng sẽ tự mở cùng Windows; cần bật tuỳ chọn thu nhỏ vào khay nếu muốn chạy nền.", "The app starts with Windows. Enable tray mode to keep it running in the background.")).small().color(Color32::from_rgb(148, 163, 184)));
            if ui.checkbox(&mut self.settings.minimize_to_tray, lang.text("Ưu tiên chạy nền trong khay hệ thống", "Start minimized to the system tray")).changed() {
                if self.settings.start_with_windows {
                    if let Err(err) = set_startup(true, self.settings.minimize_to_tray) { self.error = Some(err); }
                }
                self.save();
            }
            if ui.checkbox(&mut self.settings.auto_start_engine, lang.text("Tự bật engine khi mở app", "Start engine when app opens")).changed() { self.save(); }
        });
    }
}

impl EframeApp for App {
    fn update(&mut self, ctx: &Context, _frame: &mut Frame) {
        if self.auto_start_pending {
            self.auto_start_pending = false;
            self.start();
        }
        let running = self.is_running();
        if self.last_tray_running != Some(running) {
            if let (Some(tray), Some(icon)) = (self.tray.as_mut(), tray_icon_image(running)) { let _ = tray.set_icon(Some(icon)); }
            self.last_tray_running = Some(running);
        }
        if running || self.start_check_at.is_some() || self.auto_rx.is_some() || self.connection_rx.is_some() { ctx.request_repaint_after(Duration::from_millis(450)); }
        if self.start_check_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(1600)) {
            self.start_check_at = None;
            if self.is_running() {
                self.notify(self.settings.language.text("Engine đã chạy. Nếu Steam vẫn lỗi, hãy chọn Tự dò cấu hình.", "Engine is running. If Steam still fails, try Auto-detect."));
            } else {
                let details = fs::read_to_string(&self.log_path).unwrap_or_default();
                let tail = details.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
                self.error = Some(if tail.is_empty() {
                    self.settings.language.text("Engine đã tự thoát. Kiểm tra quyền quản trị, WinDivert và phần mềm diệt virus. Bấm Xem log để biết thêm.", "Engine exited. Check administrator rights, WinDivert, and antivirus. Open the log for details.").to_string()
                } else { format!("{}: {tail}", self.settings.language.text("Engine đã thoát", "Engine exited")) });
            }
        }
        if let Some(rx) = self.connection_rx.take() {
            match rx.try_recv() {
                Ok(results) => self.connection_results = Some(results),
                Err(mpsc::TryRecvError::Empty) => self.connection_rx = Some(rx),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if let Some(rx) = self.auto_rx.take() {
            let mut finished = false;
            while let Ok(message) = rx.try_recv() {
                match message {
                    AutoMessage::Progress(progress) => self.auto_progress = Some(progress),
                    AutoMessage::Failed(error) => { self.auto_progress = None; self.error = Some(error); finished = true; }
                    AutoMessage::Success(mut child, settings) => {
                        if let Err(error) = self.job.assign(child.id()) { let _ = child.kill(); self.error = Some(error); }
                        else {
                            if let Some(out) = child.stdout.take() { pipe_engine_log(out, self.log_path.clone()); }
                            if let Some(err) = child.stderr.take() { pipe_engine_log(err, self.log_path.clone()); }
                            self.settings = settings;
                            self.settings.resume_engine = true;
                            self.child = Some(child);
                            self.auto_progress = None;
                            self.save();
                            self.notify(self.settings.language.text("Đã chọn cấu hình truy cập được Steam Store và Community.", "Selected settings reached Steam Store and Community."));
                        }
                        finished = true;
                    }
                }
            }
            if !finished { self.auto_rx = Some(rx); }
        }

        if self.start_minimized {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.start_minimized = false;
        }
        if self.show_window_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.show_window_requested = false;
        }

        while let Ok(action) = self.tray_actions.try_recv() {
            match action {
                TrayAction::Show => self.show_window_requested = true,
                TrayAction::Start => self.start(),
                TrayAction::Stop => self.stop(),
                TrayAction::Toggle => { if self.is_running() { self.stop(); } else { self.start(); } }
                TrayAction::Quit => {
                    self.stop();
                    self.quit_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        if !self.quit_requested && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::TopBottomPanel::top("header").frame(egui::Frame::none().fill(Color32::from_rgb(15, 23, 42)).inner_margin(egui::Margin::symmetric(14.0, 10.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("GOODBYEDPI CONTROL").strong().color(Color32::from_rgb(123, 224, 164)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_source("lang").selected_text(match self.settings.language { Language::Vietnamese => "VI", Language::English => "EN" }).show_ui(ui, |ui| {
                        if ui.selectable_value(&mut self.settings.language, Language::Vietnamese, "Tiếng Việt").changed() { self.save(); }
                        if ui.selectable_value(&mut self.settings.language, Language::English, "English").changed() { self.save(); }
                    });
                });
            });
        });
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::from_rgb(10, 17, 31)).inner_margin(egui::Margin::symmetric(14.0, 12.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                nav_button(ui, &mut self.page, Page::Overview, self.settings.language.text("Trang chính", "Home"), "⌂");
                nav_button(ui, &mut self.page, Page::Websites, self.settings.language.text("Website", "Sites"), "◎");
                nav_button(ui, &mut self.page, Page::Settings, self.settings.language.text("Nâng cao", "Advanced"), "⚙");
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                Page::Overview => self.render_overview(ui),
                Page::Websites => self.render_websites(ui),
                Page::Settings => { ui.collapsing(self.settings.language.text("Mở cài đặt nâng cao", "Expand advanced settings"), |ui| self.render_settings(ui)); }
            });
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
        if let Some(mut child) = self.child.take() { let _ = child.kill(); let _ = child.wait(); }
    }
}

fn configure_theme(ctx: &Context) {
    // egui's bundled default font does not cover every Vietnamese character.
    // Use the Windows system font first so the interface renders Vietnamese correctly.
    let mut fonts = egui::FontDefinitions::default();
    #[cfg(target_os = "windows")]
    if let Ok(bytes) = fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert(
            "segoe_ui".to_owned(),
            egui::FontData::from_owned(bytes),
        );
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "segoe_ui".to_owned());
        }
    }
    ctx.set_fonts(fonts);

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
    let response = ui.add_sized([100.0, 34.0], egui::Button::new(RichText::new(format!("{glyph} {label}")).size(12.0)).selected(selected));
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

fn pipe_engine_log<R: io::Read + Send + 'static>(reader: R, path: PathBuf) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = io::BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                        use io::Write;
                        let _ = file.write_all(line.as_bytes());
                    }
                }
            }
        }
    });
}

fn open_log(path: &Path) {
    #[cfg(windows)] { let _ = Command::new("notepad.exe").arg(path).spawn(); }
    #[cfg(not(windows))] { let _ = Command::new("xdg-open").arg(path).spawn(); }
}

#[cfg(windows)]
fn cleanup_orphaned_engines() {
    use std::os::windows::process::CommandExt;
    let program_data = env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let legacy_temp = env::temp_dir().join("GoodbyeDPI-Control").to_string_lossy().into_owned();
    let script = "$roots=@($env:ProgramData + '\\GoodbyeDPI-Control', $env:GOODBYEDPI_OLD_TEMP); Get-CimInstance Win32_Process -Filter \"Name='goodbyedpi.exe'\" | Where-Object { $p=$_.ExecutablePath; $p -and ($roots | Where-Object { $p.StartsWith($_, [StringComparison]::OrdinalIgnoreCase) }) } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }";
    let _ = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]).env("ProgramData", program_data).env("GOODBYEDPI_OLD_TEMP", legacy_temp).creation_flags(0x08000000).status();
}
#[cfg(not(windows))]
fn cleanup_orphaned_engines() {}

fn normalize_domain(input: &str) -> Result<String, String> {
    let mut value = input.trim().to_lowercase();
    if value.is_empty() { return Err("Hãy nhập một domain hoặc URL.".into()); }
    if !value.contains("://") { value = format!("https://{value}"); }
    let parsed = url::Url::parse(&value).map_err(|_| "URL không hợp lệ. Ví dụ: example.com".to_string())?;
    let host = parsed.host_str().ok_or_else(|| "Không tìm thấy domain trong URL.".to_string())?;
    if host.parse::<std::net::IpAddr>().is_ok() || host == "localhost" || host.ends_with(".localhost") {
        return Err("Chỉ nhập tên website, không nhập địa chỉ IP hoặc localhost.".into());
    }
    if host.contains('.') && host.split('.').all(|part| !part.is_empty() && part.len() <= 63 && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !part.starts_with('-') && !part.ends_with('-')) {
        Ok(host.trim_end_matches('.').to_string())
    } else {
        Err("Domain cần có dạng example.com hoặc sub.example.com.".into())
    }
}

fn normalize_domain_additions(input: &str) -> Result<Vec<String>, String> {
    let domain = normalize_domain(input)?;
    let mut additions = vec![domain.clone()];
    if let Some(root) = domain.strip_prefix("www.") { additions.push(root.to_string()); }
    Ok(additions)
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
        let dll = embedded_runtime::EMBEDDED_WINDIVERT_DLL
            .ok_or_else(|| "Thiếu WinDivert.dll trong gói ứng dụng.".to_string())?;
        let sys = embedded_runtime::EMBEDDED_WINDIVERT_SYS
            .ok_or_else(|| "Thiếu driver WinDivert64.sys trong gói ứng dụng.".to_string())?;
        // Each app build gets its own stable folder. Windows keeps a loaded
        // WinDivert driver file locked, so never overwrite a runtime another
        // running GoodbyeDPI instance may still be using.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        exe.hash(&mut hasher);
        dll.hash(&mut hasher);
        sys.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let base = env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("GoodbyeDPI-Control")
            .join(format!("{fingerprint:016x}"));
        let runtime_root = base.parent().ok_or_else(|| "Đường dẫn runtime không hợp lệ.".to_string())?;
        fs::create_dir_all(runtime_root).map_err(|err| format!("Không tạo được thư mục runtime: {err}"))?;
        #[cfg(windows)] restrict_runtime_permissions(runtime_root)?;
        fs::create_dir_all(&base).map_err(|err| format!("Không tạo được thư mục runtime: {err}"))?;
        write_runtime_file(&base.join("goodbyedpi.exe"), exe, "engine GoodbyeDPI")?;
        write_runtime_file(&base.join("WinDivert.dll"), dll, "WinDivert.dll")?;
        write_runtime_file(&base.join("WinDivert64.sys"), sys, "driver WinDivert64.sys")?;
        if let Ok(entries) = fs::read_dir(runtime_root) {
            for entry in entries.flatten() {
                if entry.path() != base { let _ = fs::remove_dir_all(entry.path()); }
            }
        }
        *runtime_dir = Some(base.clone());
        return Ok(base.join("goodbyedpi.exe"));
    }
    let adjacent = env::current_exe().ok().and_then(|path| path.parent().map(|dir| dir.join("goodbyedpi.exe")));
    adjacent.filter(|path| path.is_file()).ok_or_else(|| "Chưa có engine GoodbyeDPI. Hãy chạy gui\\build.ps1 để tạo bản đóng gói một file.".to_string())
}

#[cfg(windows)]
fn restrict_runtime_permissions(path: &Path) -> Result<(), String> {
    let identity = Command::new("whoami.exe").output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).ok_or_else(|| "Không xác định được tài khoản Windows hiện tại.".to_string())?;
    let user_acl = format!("{identity}:(OI)(CI)F");
    let output = Command::new("icacls.exe").arg(path).arg("/inheritance:r").arg("/grant:r").arg("*S-1-5-18:(OI)(CI)F").arg("*S-1-5-32-544:(OI)(CI)F").arg(user_acl).output().map_err(|err| format!("Không thể bảo vệ thư mục runtime: {err}"))?;
    if output.status.success() { Ok(()) } else { Err(format!("Không thể giới hạn quyền thư mục runtime: {}", String::from_utf8_lossy(&output.stderr).trim())) }
}

fn write_runtime_file(path: &Path, contents: &[u8], label: &str) -> Result<(), String> {
    if fs::read(path).map(|existing| existing == contents).unwrap_or(false) {
        return Ok(());
    }
    fs::write(path, contents).map_err(|err| {
        format!(
            "Không ghi được {label}: {err}. Hãy thoát các cửa sổ GoodbyeDPI Control/GoodbyeDPI đang chạy rồi thử lại."
        )
    })
}

fn create_tray(sender: Sender<TrayAction>, ctx: Context, lang: Language) -> Option<TrayIcon> {
    let menu = Menu::new();
    let show = MenuItem::with_id("show", lang.text("Mở cửa sổ", "Open window"), true, None);
    let start = MenuItem::with_id("start", lang.text("Khởi động", "Start"), true, None);
    let stop = MenuItem::with_id("stop", lang.text("Dừng", "Stop"), true, None);
    let quit = MenuItem::with_id("quit", lang.text("Thoát", "Quit"), true, None);
    let _ = menu.append(&show);
    let _ = menu.append(&start);
    let _ = menu.append(&stop);
    let _ = menu.append(&quit);
    let menu_sender = sender.clone();
    let menu_ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id.0.as_str() {
            "show" => Some(TrayAction::Show),
            "start" => Some(TrayAction::Start),
            "stop" => Some(TrayAction::Stop),
            "quit" => Some(TrayAction::Quit),
            _ => None,
        };
        if let Some(action) = action { let _ = menu_sender.send(action); menu_ctx.request_repaint(); }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            let _ = sender.send(TrayAction::Toggle);
            ctx.request_repaint();
        }
    }));
    let icon = tray_icon_image(false)?;
    TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(APP_NAME).with_icon(icon).build().ok()
}

fn tray_icon_image(running: bool) -> Option<Icon> {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let dx = x as i32 - 8;
            let dy = y as i32 - 8;
            let inside = dx * dx + dy * dy <= 49;
            let accent = inside && (x == 7 || x == 8 || (y > 7 && x > 5 && x < 11));
            if accent && running { rgba.extend_from_slice(&[68, 220, 130, 255]); }
            else if accent { rgba.extend_from_slice(&[148, 163, 184, 255]); }
            else if inside { rgba.extend_from_slice(&[30, 41, 59, 255]); }
            else { rgba.extend_from_slice(&[0, 0, 0, 0]); }
        }
    }
    Icon::from_rgba(rgba, 16, 16).ok()
}

#[cfg(windows)]
fn set_startup(enabled: bool, start_minimized: bool) -> Result<(), String> {
    const TASK_NAME: &str = "GoodbyeDPI-Control-AtLogon";
    let _ = Command::new("reg.exe").args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", APP_NAME, "/f"]).output();
    if enabled {
        let exe = env::current_exe().map_err(|err| format!("Không lấy được đường dẫn ứng dụng: {err}"))?;
        let minimized_arg = if start_minimized { " --minimized" } else { "" };
        let task_command = format!("\"{}\"{minimized_arg}", exe.display());
        let output = Command::new("schtasks.exe").arg("/Create").arg("/SC").arg("ONLOGON").arg("/TN").arg(TASK_NAME).arg("/TR").arg(task_command).arg("/RL").arg("HIGHEST").arg("/IT").arg("/F").output().map_err(|err| format!("Không mở được Task Scheduler: {err}"))?;
        if !output.status.success() { return Err(format!("Không tạo được tác vụ Windows: {}", String::from_utf8_lossy(&output.stderr).trim())); }
    } else {
        let output = Command::new("schtasks.exe").args(["/Delete", "/TN", TASK_NAME, "/F"]).output().map_err(|err| format!("Không mở được Task Scheduler: {err}"))?;
        let details = format!("{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if !output.status.success() && !details.to_lowercase().contains("cannot find") && !details.to_lowercase().contains("does not exist") { return Err(format!("Không xoá được tác vụ Windows: {}", details.trim())); }
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_startup(_enabled: bool, _start_minimized: bool) -> Result<(), String> {
    Err("Tính năng này chỉ hỗ trợ Windows.".into())
}

fn main() -> eframe::Result {
    let _instance_guard = match claim_single_instance() { Some(guard) => guard, None => return Ok(()) };
    let native_options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size(Vec2::new(380.0, 520.0))
            .with_min_inner_size(Vec2::new(340.0, 420.0)),
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
        assert_eq!(normalize_domain_additions("www.example.com").unwrap(), vec!["www.example.com".to_string(), "example.com".to_string()]);
    }

    #[test]
    fn rejects_invalid_domains() {
        assert!(normalize_domain("").is_err());
        assert!(normalize_domain("not-a-domain").is_err());
        assert!(normalize_domain("127.0.0.1").is_err());
        assert!(normalize_domain("localhost").is_err());
    }

    #[test]
    fn strong_preset_does_not_block_quic_globally() {
        let mut settings = Settings::default();
        settings.apply_preset(Intensity::Strong);
        let args = settings.cli_args(Path::new("domains.txt"));
        assert!(args.contains(&"--wrong-seq".to_string()));
        assert!(args.contains(&"--wrong-chksum".to_string()));
        assert!(!args.contains(&"-q".to_string()));
        assert!(args.contains(&"--blacklist".to_string()));
    }

    #[test]
    fn steam_profile_is_populated_by_default() { assert!(STEAM_DOMAINS.iter().all(|domain| Settings::default().domains.contains(&domain.to_string()))); }

    #[test]
    fn autodetect_tests_eight_preset_combinations() {
        let candidates = preset_candidates(&Settings::default());
        assert_eq!(candidates.len(), 8);
        assert_eq!(select_first_passing(&[false, false, true, true]), Some(2));
        assert_eq!(select_first_passing(&[false, false]), None);
    }

    #[test]
    fn dns_override_is_opt_in() {
        let args = Settings::default().cli_args(Path::new("domains.txt"));
        assert!(!args.contains(&"--dns-addr".to_string()));
    }
}

