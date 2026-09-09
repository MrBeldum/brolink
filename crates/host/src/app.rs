//! The host control panel: a status window with one setup button.

use crate::config::{self, HostConfig};
use crate::setup;
use crate::streamer::Api;
use brolink_core::api::Status;
use brolink_core::http;
use brolink_core::{CONTROL_PORT, SUNSHINE_WEB_PORT};
use brolink_ui::{self as ui, Tone, PALETTE as P};
use eframe::egui;
use parking_lot::Mutex;
use semver::Version;
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const COLUMN_WIDTH: f32 = 680.0;

/// What the background threads know, read by the UI every frame.
#[derive(Default)]
pub struct Shared {
    pub status: Option<Status>,
    pub service_error: Option<String>,
    pub clients: Vec<(String, String)>,
    pub gamepad_driver: Option<bool>,
    pub setup_running: bool,
    pub setup_result: Option<Result<(), String>>,
    pub setup_log: Vec<String>,
}

pub struct HostApp {
    shared: Arc<Mutex<Shared>>,
    cfg: HostConfig,
    autostart: bool,
    dirty: bool,
    brand: ui::Brand,
    confirm_unpair: Option<String>,
    busy_since: Option<Instant>,
    /// The Sunshine installer sits beside the exe, so setup needs no download.
    bundled_sunshine: bool,
}

impl HostApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        spawn_poller(shared.clone(), cc.egui_ctx.clone());
        Self::with_shared(cc, shared)
    }

    fn with_shared(cc: &eframe::CreationContext<'_>, shared: Arc<Mutex<Shared>>) -> Self {
        ui::apply(&cc.egui_ctx);
        Self {
            shared,
            cfg: HostConfig::load(),
            autostart: setup::starts_with_windows(),
            dirty: false,
            brand: ui::Brand::new(&cc.egui_ctx),
            confirm_unpair: None,
            busy_since: None,
            bundled_sunshine: std::env::current_exe().is_ok_and(|e| setup::bundled_sunshine(&e)),
        }
    }

    fn commit(&mut self) {
        if self.dirty {
            self.dirty = false;
            if let Err(e) = self.cfg.save() {
                tracing::warn!("could not save host config: {e:#}");
            }
        }
    }

    fn api(&self) -> Option<Api<'_>> {
        self.cfg.has_creds().then_some(Api {
            user: &self.cfg.sunshine_user,
            pass: &self.cfg.sunshine_pass,
        })
    }

    /// Generate a Sunshine login if there is none, save it, and run the
    /// elevated script on a thread.
    fn start_setup(&mut self, status: &Status) {
        if !self.cfg.has_creds() {
            self.cfg.sunshine_user = "brolink".into();
            self.cfg.sunshine_pass = config::random_password();
            self.dirty = true;
            self.commit();
        }
        let shared = self.shared.clone();
        {
            let mut s = shared.lock();
            s.setup_running = true;
            s.setup_result = None;
            s.setup_log.clear();
        }
        let cfg = self.cfg.clone();
        let install_sunshine = !status.streamer.installed;
        let adapter = status.wake_adapter.clone();
        let desc = status.wake_adapter_description.clone();
        std::thread::spawn(move || {
            let result = std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| {
                    setup::run(&setup::Plan {
                        exe: &exe,
                        install_sunshine,
                        sunshine_user: &cfg.sunshine_user,
                        sunshine_pass: &cfg.sunshine_pass,
                        adapter: &adapter,
                        adapter_description: &desc,
                    })
                    .map_err(|e| e.to_string())?;
                    // The service is what the Mac needs at logon; turn it on
                    // once setup has succeeded rather than asking.
                    let _ = setup::set_start_with_windows(true, &exe);
                    Ok(())
                });
            let mut s = shared.lock();
            s.setup_running = false;
            s.setup_log = read_setup_log();
            s.setup_result = Some(result);
        });
    }
}

impl eframe::App for HostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(500));
        let shared = self.shared.lock();
        let status = shared.status.clone();
        let service_error = shared.service_error.clone();
        let clients = shared.clients.clone();
        let gamepad = shared.gamepad_driver;
        let setup_running = shared.setup_running;
        let setup_result = shared.setup_result.clone();
        let setup_log = shared.setup_log.clone();
        drop(shared);
        if setup_running {
            self.busy_since.get_or_insert_with(Instant::now);
        } else {
            self.busy_since = None;
        }

        ui::top_bar(ctx, "top", |ui| {
            let (label, tone) = pill(status.as_ref(), service_error.as_deref());
            self.brand.header(ui, "BroLink Host", |ui| {
                ui::status_pill(ui, label, tone);
            });
        });

        ui::bottom_bar(ctx, "bottom", |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.label("·");
                ui.label(format!("control port TCP {CONTROL_PORT}"));
                if let Some(s) = &status {
                    if s.version != env!("CARGO_PKG_VERSION") {
                        ui.label("·");
                        ui.colored_label(
                            P.accent,
                            format!("service is v{}, restarting it", s.version),
                        );
                    }
                }
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(P.bg))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(20.0);
                    ui::content_column(ui, COLUMN_WIDTH, |ui| {
                        ui.spacing_mut().item_spacing.y = 14.0;
                        match &status {
                            Some(s) => {
                                self.setup_card(
                                    ui,
                                    s,
                                    setup_running,
                                    setup_result.as_ref(),
                                    &setup_log,
                                );
                                self.pc_card(ui, s, gamepad);
                                self.paired_card(ui, &clients);
                                self.settings_card(ui);
                                ui::titled_card(ui, "Log", None, |ui| {
                                    ui::log_view(ui, "host_log", &s.log, 200.0);
                                });
                            }
                            None => {
                                ui::toned_card(ui, Tone::Neutral, |ui| {
                                    ui::heading(ui, "Starting the background service", None);
                                    ui::empty_state(
                                        ui,
                                        service_error
                                            .as_deref()
                                            .unwrap_or("This takes a second or two."),
                                        true,
                                    );
                                });
                            }
                        }
                        ui.add_space(10.0);
                    });
                });
            });

        self.commit();
    }
}

impl HostApp {
    fn setup_card(
        &mut self,
        ui: &mut egui::Ui,
        s: &Status,
        running: bool,
        result: Option<&Result<(), String>>,
        log: &[String],
    ) {
        // Tailscale is the user's job (sign in), not the script's.
        let admin_items: Vec<&String> = s
            .setup
            .iter()
            .filter(|l| !l.starts_with("Tailscale"))
            .collect();
        let tailscale_item = s.setup.iter().find(|l| l.starts_with("Tailscale"));
        if admin_items.is_empty() && tailscale_item.is_none() && result.is_none() && !running {
            return;
        }
        let tone = if admin_items.is_empty() && tailscale_item.is_none() {
            Tone::Success
        } else {
            Tone::Accent
        };
        ui::toned_card(ui, tone, |ui| {
            ui::heading(
                ui,
                "Set up this PC",
                Some("One administrator prompt does everything below. Nothing else needs configuring."),
            );
            if let Some(t) = tailscale_item {
                ui::notice(ui, Tone::Danger, t);
                ui.horizontal(|ui| {
                    if ui::ghost_button(ui, "Get Tailscale").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                            "https://tailscale.com/download/windows",
                        ));
                    }
                    ui::caption(ui, "Sign in with the same account as your Mac.");
                });
            }
            for item in &admin_items {
                ui::dot_label(ui, Tone::Accent, item);
            }
            if admin_items.is_empty() && tailscale_item.is_none() {
                ui::dot_label(ui, Tone::Success, "Everything is in place.");
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if running {
                    ui::empty_state(
                        ui,
                        &format!(
                            "Waiting for the administrator prompt and the setup steps… {}s",
                            self.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0)
                        ),
                        true,
                    );
                } else {
                    let label = if admin_items.is_empty() {
                        "Run setup again"
                    } else {
                        "Set up this PC (administrator)"
                    };
                    if ui::primary_button(ui, label).clicked() {
                        self.start_setup(s);
                    }
                    if !s.streamer.installed {
                        ui::caption(
                            ui,
                            if self.bundled_sunshine {
                                "Installs the Sunshine that ships with BroLink, silently."
                            } else {
                                "Downloads Sunshine from GitHub and installs it silently."
                            },
                        );
                    }
                }
            });
            match result {
                Some(Ok(())) => {
                    ui::notice(
                        ui,
                        Tone::Success,
                        "Setup finished. The service re-checks everything within a few seconds.",
                    );
                }
                Some(Err(e)) => {
                    ui::notice(ui, Tone::Danger, e);
                }
                None => {}
            }
            if !log.is_empty() {
                ui::collapsible(
                    ui,
                    "setup_log",
                    "Setup log",
                    result.as_ref().is_some_and(|r| r.is_err()),
                    |ui| {
                        ui::log_view(ui, "setup_log_view", log, 160.0);
                    },
                );
            }
        });
    }

    fn pc_card(&mut self, ui: &mut egui::Ui, s: &Status, gamepad: Option<bool>) {
        ui::titled_card(ui, "This PC", None, |ui| {
            let tailscale = match (&s.tailscale_ip, &s.tailscale_login) {
                (Some(ip), Some(login)) => format!("{ip} · signed in as {login}"),
                (Some(ip), None) => ip.clone(),
                _ => "not running".into(),
            };
            let streamer = if !s.streamer.installed {
                "not installed".to_string()
            } else {
                format!(
                    "{} · {}{}{}{}",
                    s.streamer.kind,
                    if s.streamer.running {
                        "running"
                    } else {
                        "not running"
                    },
                    if s.streamer.api_ok {
                        " · BroLink logged in"
                    } else {
                        ""
                    },
                    match s.streamer.encoder.as_str() {
                        "" => String::new(),
                        "software" =>
                            " · software encoder (no GPU encoder worked: streams will be slow)"
                                .into(),
                        e => format!(" · {e} encoder"),
                    },
                    if s.streamer.audio_problem.is_empty() {
                        String::new()
                    } else {
                        format!(" · no sound: {}", s.streamer.audio_problem)
                    }
                )
            };
            let network = match &s.nat {
                Some(n) => crate::service::describe_nat(n)
                    .trim_start_matches("network: ")
                    .to_string(),
                None => "checking…".into(),
            };
            let mut wake = match (&s.mac, s.wake_ready) {
                (Some(mac), Some(true)) => format!("ready · {} · {mac}", s.wake_adapter),
                (Some(mac), Some(false)) => format!("off · {} · {mac}", s.wake_adapter),
                (Some(mac), None) => format!("unknown · {} · {mac}", s.wake_adapter),
                (None, _) => "no wired adapter found".into(),
            };
            if s.fast_startup == Some(true) {
                wake.push_str(" · Fast Startup on");
            }
            if let Some(age) = s.wake_packet_age_secs {
                wake.push_str(&format!(" · packet received {age}s ago"));
            }
            let gamepad_text = match gamepad {
                Some(true) => "virtual controller driver installed",
                Some(false) => "virtual controller driver missing",
                None => "unknown until Sunshine is running",
            };
            ui::kv_grid(
                ui,
                "pc_grid",
                &[
                    ("Name", s.name.clone()),
                    ("Tailscale", tailscale),
                    ("Network", network),
                    ("Streaming", streamer),
                    ("Wake-on-LAN", wake),
                    (
                        "LAN address",
                        s.lan_ip.clone().unwrap_or_else(|| "—".into()),
                    ),
                    ("Gamepads", gamepad_text.into()),
                ],
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if s.streamer.running && ui::ghost_button(ui, "Open Sunshine settings").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                        "https://localhost:{SUNSHINE_WEB_PORT}"
                    )));
                }
                if gamepad == Some(false)
                    && ui::ghost_button(ui, "Install controller driver").clicked()
                {
                    if let Some(api) = self.api() {
                        if let Err(e) = api.install_gamepad_driver() {
                            tracing::warn!("ViGEmBus install: {e:#}");
                        }
                    }
                }
            });
            if s.streamer.api_ok && self.cfg.has_creds() {
                ui::caption(
                    ui,
                    format!(
                        "Sunshine web login for you: {} / {}",
                        self.cfg.sunshine_user, self.cfg.sunshine_pass
                    ),
                );
            }
        });
    }

    fn paired_card(&mut self, ui: &mut egui::Ui, clients: &[(String, String)]) {
        ui::titled_card(
            ui,
            "Paired Macs",
            Some("A Mac pairs itself the first time it connects; nothing to type here."),
            |ui| {
                if clients.is_empty() {
                    ui::empty_state(ui, "No Mac has paired yet.", false);
                }
                for (i, (name, uuid)) in clients.iter().enumerate() {
                    if i > 0 {
                        ui::row_separator(ui);
                    }
                    ui::list_row(ui, name, &uuid[..uuid.len().min(8)], |ui| {
                        if self.confirm_unpair.as_deref() == Some(uuid) {
                            if ui::toned_button(ui, "Unpair", Tone::Danger).clicked() {
                                if let Some(api) = self.api() {
                                    if let Err(e) = api.unpair(uuid) {
                                        tracing::warn!("unpair: {e:#}");
                                    }
                                }
                                self.confirm_unpair = None;
                            }
                            if ui::ghost_button(ui, "Keep").clicked() {
                                self.confirm_unpair = None;
                            }
                        } else if ui::danger_button(ui, "Unpair…").clicked() {
                            self.confirm_unpair = Some(uuid.clone());
                        }
                    });
                }
            },
        );
    }

    fn settings_card(&mut self, ui: &mut egui::Ui) {
        ui::titled_card(ui, "Settings", None, |ui| {
            if ui::toggle_row(
                ui,
                &mut self.cfg.power_allowed,
                "Let a paired Mac sleep, restart, or shut down this PC",
                Some("Only a Mac on your own Tailscale account can ask. Asleep, the PC wakes from the Mac in seconds."),
            ) {
                self.dirty = true;
            }
            ui::row_separator(ui);
            let mut auto = self.autostart;
            if ui::toggle_row(
                ui,
                &mut self.cfg.stay_awake,
                "Keep this PC awake while plugged in",
                Some("Tailscale only works while the PC is on. Asleep, a Mac on another network cannot wake it. Sleep from the Mac or the Start menu still works."),
            ) {
                self.dirty = true;
            }
            ui::row_separator(ui);
            if ui::toggle_row(
                ui,
                &mut auto,
                "Start the background service with Windows",
                Some("On by default, so the Mac can reach this PC after every restart without anyone at the keyboard."),
            ) {
                if let Ok(exe) = std::env::current_exe() {
                    match setup::set_start_with_windows(auto, &exe) {
                        Ok(()) => {
                            self.autostart = auto;
                            self.cfg.start_with_windows = auto;
                            self.dirty = true;
                        }
                        Err(e) => tracing::warn!("autostart: {e:#}"),
                    }
                }
            }
            ui::row_separator(ui);
            ui::setting_row(
                ui,
                "Updates",
                Some("New versions of BroLink Host arrive from your Mac over Tailscale and install by themselves; nothing to do here."),
                |ui| {
                    ui::muted(ui, format!("v{}", env!("CARGO_PKG_VERSION")));
                },
            );
            ui::row_separator(ui);
            ui.horizontal(|ui| {
                if ui::danger_button(ui, "Stop the background service").clicked() {
                    // Off the UI thread: the service may take a moment to
                    // answer, and the window must not freeze meanwhile.
                    std::thread::spawn(|| {
                        let _ = http::request(
                            ("127.0.0.1", CONTROL_PORT),
                            "POST",
                            "/v1/quit",
                            None,
                            Duration::from_secs(2),
                        );
                    });
                    let mut s = self.shared.lock();
                    s.status = None;
                    s.service_error =
                        Some("Stopped. It starts again next time this window opens.".into());
                }
                if ui::ghost_button(ui, "Open log folder").clicked() {
                    if let Ok(dir) = brolink_core::config::data_dir() {
                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    }
                }
            });
        });
    }
}

fn pill(status: Option<&Status>, err: Option<&str>) -> (&'static str, Tone) {
    match status {
        None if err.is_some() => ("Service stopped", Tone::Neutral),
        None => ("Starting", Tone::Neutral),
        Some(s) if s.setup.iter().any(|l| l.starts_with("Tailscale")) => {
            ("Tailscale off", Tone::Danger)
        }
        Some(s) if !s.setup.is_empty() => ("Needs setup", Tone::Accent),
        Some(_) => ("Ready", Tone::Success),
    }
}

/// Last lines of the elevated script's transcript, BOM and blanks dropped.
fn read_setup_log() -> Vec<String> {
    let Some(path) = setup::log_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let text = decode_log(&bytes);
    let lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    lines.into_iter().rev().take(40).rev().collect()
}

/// `Out-File -Encoding utf8` writes a BOM; older PowerShell writes UTF-16.
fn decode_log(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes))
            .into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateAction {
    None,
    /// Service is older than this panel: quit it and start the file on disk.
    RestartService,
    /// Service is newer: this panel is the leftover window, replace it.
    RelaunchPanel,
}

fn update_action(service: &str, panel: &str) -> UpdateAction {
    let (Ok(s), Ok(p)) = (Version::parse(service), Version::parse(panel)) else {
        return UpdateAction::None;
    };
    match s.cmp(&p) {
        Ordering::Less => UpdateAction::RestartService,
        Ordering::Greater => UpdateAction::RelaunchPanel,
        Ordering::Equal => UpdateAction::None,
    }
}

fn relaunch_this_exe() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    std::process::Command::new(exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Poll the service every second; refresh Sunshine's client list now and
/// then; restart the service after an update.
fn spawn_poller(shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut failures = 0u32;
        let mut tick = 0u64;
        let mut restarted_service = false;
        loop {
            let r: Result<Status, _> = http::get_json(
                ("127.0.0.1", CONTROL_PORT),
                "/v1/status",
                Duration::from_millis(800),
            );
            match r {
                Ok(st) => {
                    failures = 0;
                    match update_action(&st.version, env!("CARGO_PKG_VERSION")) {
                        UpdateAction::RestartService if !restarted_service => {
                            // This panel is newer than the service (an
                            // update replaced the exe; current_exe still
                            // names it). Restart once; looping on != used
                            // to kill a *newer* service every second.
                            restarted_service = true;
                            let _ = http::request(
                                ("127.0.0.1", CONTROL_PORT),
                                "POST",
                                "/v1/quit",
                                None,
                                Duration::from_secs(2),
                            );
                            std::thread::sleep(Duration::from_secs(1));
                            crate::ensure_service_running();
                        }
                        UpdateAction::RelaunchPanel if relaunch_this_exe() => {
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                    let cfg = HostConfig::load();
                    if st.streamer.api_ok && cfg.has_creds() && tick.is_multiple_of(10) {
                        let api = Api {
                            user: &cfg.sunshine_user,
                            pass: &cfg.sunshine_pass,
                        };
                        let clients = api.clients();
                        let gamepad = api.gamepad_driver();
                        let mut s = shared.lock();
                        s.clients = clients;
                        s.gamepad_driver = gamepad;
                    }
                    let mut s = shared.lock();
                    s.status = Some(st);
                    if s.service_error
                        .as_deref()
                        .is_some_and(|e| !e.starts_with("Stopped"))
                    {
                        s.service_error = None;
                    }
                }
                Err(e) => {
                    failures += 1;
                    let mut s = shared.lock();
                    let stopped_by_user = s
                        .service_error
                        .as_deref()
                        .is_some_and(|e| e.starts_with("Stopped"));
                    s.status = None;
                    if !stopped_by_user {
                        s.service_error = Some(format!("{e}"));
                    }
                    drop(s);
                    if failures % 4 == 2 && !stopped_by_user {
                        crate::ensure_service_running();
                    }
                }
            }
            ctx.request_repaint();
            tick += 1;
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_log_decodes_utf16_and_utf8_with_boms() {
        let utf8 = [&[0xEF, 0xBB, 0xBF][..], "a\r\n\r\nb\n".as_bytes()].concat();
        assert_eq!(
            decode_log(&utf8)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count(),
            2
        );
        let mut utf16 = vec![0xFF, 0xFE];
        for c in "hi\n".encode_utf16() {
            utf16.extend_from_slice(&c.to_le_bytes());
        }
        assert_eq!(decode_log(&utf16), "hi\n");
    }

    #[test]
    fn a_newer_service_does_not_get_killed_by_the_old_panel() {
        assert_eq!(update_action("3.1.0", "3.1.0"), UpdateAction::None);
        assert_eq!(
            update_action("3.0.1", "3.1.0"),
            UpdateAction::RestartService
        );
        assert_eq!(update_action("3.1.0", "3.0.1"), UpdateAction::RelaunchPanel);
        assert_eq!(update_action("nope", "3.1.0"), UpdateAction::None);
    }

    #[test]
    fn pill_reflects_setup_state() {
        assert_eq!(pill(None, None).0, "Starting");
        let mut s = Status::default();
        assert_eq!(pill(Some(&s), None).0, "Ready");
        s.setup.push("Sunshine is not installed.".into());
        assert_eq!(pill(Some(&s), None).0, "Needs setup");
        s.setup.push("Tailscale: not running".into());
        assert_eq!(pill(Some(&s), None).0, "Tailscale off");
    }
}

/// Render the panel to PNGs for review without a PC:
///
/// ```text
/// cargo test -p brolink-host snapshots -- --ignored
/// ```
#[cfg(test)]
mod snapshots {
    use super::*;
    use brolink_core::api::Streamer;

    fn out_dir() -> std::path::PathBuf {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
        std::fs::create_dir_all(&dir).expect("create snapshot dir");
        dir
    }

    fn save(img: image::RgbaImage, name: &str) {
        let path = out_dir().join(name);
        img.save(&path).expect("write png");
        eprintln!("wrote {}", path.display());
    }

    fn ready_status() -> Status {
        Status {
            app: "brolink".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            name: "GAMING-PC".into(),
            tailscale_ip: Some("100.64.0.10".into()),
            tailscale_login: Some("user@example.com".into()),
            lan_ip: Some("192.168.1.10".into()),
            mac: Some("02:00:00:00:00:01".into()),
            wake_ready: Some(true),
            wake_adapter: "Ethernet".into(),
            wake_adapter_description: "Example NIC".into(),
            wake_packet_age_secs: Some(42),
            fast_startup: Some(false),
            streamer: Streamer {
                kind: "Sunshine".into(),
                installed: true,
                running: true,
                api_ok: true,
                encoder: "nvenc".into(),
                audio_problem: String::new(),
            },
            power_allowed: true,
            nat: Some(brolink_core::api::NatReport {
                udp: true,
                ipv4: true,
                ipv6: false,
                hard: Some(true),
                portmap: false,
                derp: "tok".into(),
            }),
            setup: vec![],
            log: vec![
                "BroLink Host 3.0.1 listening on TCP 47850".into(),
                "listening for wake packets on UDP 9".into(),
                "Tailscale up as user@example.com (100.64.0.10)".into(),
                "Sunshine is running and BroLink is logged in".into(),
                "Wake-on-LAN ready on Ethernet (02:00:00:00:00:01)".into(),
                "example-mac (user@example.com) asked".into(),
                "paired \"Example Mac\"".into(),
            ],
        }
    }

    fn build(shared: Shared) -> egui_kittest::Harness<'static, HostApp> {
        let shared = Arc::new(Mutex::new(shared));
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(720.0, 1500.0))
            .with_pixels_per_point(2.0)
            .with_max_steps(8)
            .build_eframe(move |cc| HostApp::with_shared(cc, shared));
        harness.run_steps(3);
        harness
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn ready() {
        let mut h = build(Shared {
            status: Some(ready_status()),
            clients: vec![
                ("Example Mac".into(), "0000000000000001".into()),
                ("Second Example Mac".into(), "0000000000000002".into()),
            ],
            gamepad_driver: Some(true),
            ..Default::default()
        });
        save(h.render().unwrap(), "host-ready.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn needs_setup() {
        let mut st = ready_status();
        st.streamer = Streamer::default();
        st.wake_ready = Some(false);
        st.fast_startup = Some(true);
        st.setup = vec![
            "Sunshine is not installed.".into(),
            "Wake-on-LAN is off on Ethernet.".into(),
            "Fast Startup is on, so the PC cannot be woken after a shutdown.".into(),
        ];
        let mut h = build(Shared {
            status: Some(st),
            gamepad_driver: None,
            ..Default::default()
        });
        save(h.render().unwrap(), "host-setup.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn setup_failed_no_tailscale() {
        let mut st = ready_status();
        st.tailscale_ip = None;
        st.tailscale_login = None;
        st.setup = vec!["Tailscale: Tailscale is not installed. Install it and sign in.".into()];
        let mut h = build(Shared {
            status: Some(st),
            setup_result: Some(Err("the administrator prompt was declined or setup failed (see C:\\Users\\Example User\\AppData\\Local\\BroLink\\setup.log)".into())),
            setup_log: vec![
                "[14:02:11] BroLink setup started".into(),
                "[14:02:11] Downloading Sunshine".into(),
                "  sunshine: curl: (6) Could not resolve host: api.github.com".into(),
            ],
            ..Default::default()
        });
        save(h.render().unwrap(), "host-setup-failed.png");
    }
}
