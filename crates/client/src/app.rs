//! The client window: your PCs, one Connect button each, and the settings
//! Moonlight is started with.

use crate::config::{ClientConfig, Codec, Resolution};
use crate::moonlight;
use crate::session::{self, Discovery, Pc, Progress, Step, Target};
use brolink_core::api::PowerAction;
use brolink_core::tailscale;
use brolink_ui::{self as ui, Tone, PALETTE as P};
use eframe::egui;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

const COLUMN_WIDTH: f32 = 520.0;

/// A one-line result with the tone to show it in.
type Notice = (Tone, String);

/// Install-Moonlight progress, from its thread.
#[derive(Default)]
pub struct InstallState {
    pub running: bool,
    pub note: String,
    pub result: Option<Result<(), String>>,
}

pub struct ClientApp {
    cfg: ClientConfig,
    dirty: bool,
    discovery: Arc<Mutex<Discovery>>,
    progress: Arc<Mutex<Progress>>,
    install: Arc<Mutex<InstallState>>,
    /// Checked once a second; Moonlight may be installed while we run.
    moonlight_ok: bool,
    moonlight_checked: Instant,
    tailscale_ok: bool,
    brand: ui::Brand,
    settings_open: bool,
    /// A restart or shutdown waits for a second click.
    confirm: Option<(String, PowerAction)>,
    /// One-line result of the last power request.
    notice: Option<(Tone, String, Instant)>,
    /// After a session ends, offer to sleep this PC.
    offer_sleep: Option<Pc>,
    ended_seen: bool,
    /// Filled by the power thread, moved into `notice` on the next frame.
    pending_notice: Option<Arc<Mutex<Option<Notice>>>>,
}

impl ClientApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let discovery = Arc::new(Mutex::new(Discovery::default()));
        session::spawn_discovery(discovery.clone(), cc.egui_ctx.clone());
        Self::with_shared(
            cc,
            discovery,
            Arc::new(Mutex::new(Progress::default())),
            true,
        )
    }

    fn with_shared(
        cc: &eframe::CreationContext<'_>,
        discovery: Arc<Mutex<Discovery>>,
        progress: Arc<Mutex<Progress>>,
        probe_tools: bool,
    ) -> Self {
        ui::apply(&cc.egui_ctx);
        Self {
            cfg: ClientConfig::load(),
            dirty: false,
            discovery,
            progress,
            install: Arc::new(Mutex::new(InstallState::default())),
            moonlight_ok: !probe_tools || moonlight::cli().is_some(),
            moonlight_checked: Instant::now(),
            tailscale_ok: !probe_tools || tailscale::cli().is_some(),
            brand: ui::Brand::new(&cc.egui_ctx),
            settings_open: false,
            confirm: None,
            notice: None,
            offer_sleep: None,
            ended_seen: true,
            pending_notice: None,
        }
    }

    fn commit(&mut self) {
        if self.dirty {
            self.dirty = false;
            if let Err(e) = self.cfg.save() {
                tracing::warn!("could not save settings: {e:#}");
            }
        }
    }

    fn native_pixels(ctx: &egui::Context) -> (u32, u32) {
        let ppp = ctx.pixels_per_point();
        let size = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(egui::vec2(2560.0, 1440.0));
        let even = |v: f32| (((v * ppp).round() as u32) / 2) * 2;
        (even(size.x).max(640), even(size.y).max(400))
    }

    fn connect(&mut self, ctx: &egui::Context, pc: &Pc) {
        let Some(target) = Target::from_pc(pc) else {
            return;
        };
        self.ended_seen = false;
        self.offer_sleep = None;
        session::connect(
            target,
            self.cfg.stream.clone(),
            Self::native_pixels(ctx),
            self.progress.clone(),
            ctx.clone(),
        );
    }

    fn power(&mut self, pc: &Pc, action: PowerAction) {
        let Some(ip) = pc.ip else { return };
        let name = pc.name.clone();
        let progress = self.progress.clone();
        let notice = Arc::new(Mutex::new(None));
        // End the stream first so Sunshine sees a clean disconnect.
        if progress.lock().active() {
            progress.lock().cancel = true;
        }
        let out = notice.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            let r = session::power(ip, action);
            *out.lock() = Some(match r {
                Ok(()) => (
                    Tone::Success,
                    format!("{name}: {} requested.", action.label().to_lowercase()),
                ),
                Err(e) => (Tone::Danger, format!("{name}: {e}")),
            });
        });
        self.pending_notice = Some(notice);
    }
}

/// Deferred notice slot, filled by the power thread.
impl ClientApp {
    fn poll_notice(&mut self) {
        let ready = self.pending_notice.as_ref().and_then(|n| n.lock().take());
        if let Some((tone, text)) = ready {
            self.notice = Some((tone, text, Instant::now()));
            self.pending_notice = None;
        }
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(400));
        if self.moonlight_checked.elapsed() > Duration::from_secs(2) {
            self.moonlight_checked = Instant::now();
            self.moonlight_ok = moonlight::cli().is_some();
            self.tailscale_ok = tailscale::cli().is_some();
        }
        self.poll_notice();
        let disc = self.discovery.lock().clone();
        let prog = self.progress.lock().clone();
        let install = {
            let i = self.install.lock();
            (i.running, i.note.clone(), i.result.clone())
        };
        if let Step::Ended { .. } = &prog.step {
            if !self.ended_seen {
                self.ended_seen = true;
                if self.cfg.sleep_prompt {
                    self.offer_sleep = disc
                        .pcs
                        .iter()
                        .find(|p| p.name == prog.pc && p.power_allowed())
                        .cloned();
                }
            }
        }
        if let Some((_, _, at)) = &self.notice {
            if at.elapsed() > Duration::from_secs(12) {
                self.notice = None;
            }
        }

        ui::top_bar(ctx, "top", |ui| {
            let (label, tone) = if !self.tailscale_ok || disc.error.is_some() {
                ("Tailscale off", Tone::Danger)
            } else if !self.moonlight_ok {
                ("Moonlight missing", Tone::Accent)
            } else if prog.active() {
                ("Connected", Tone::Success)
            } else {
                ("Ready", Tone::Success)
            };
            self.brand
                .header(ui, "BroLink", "Your Windows PC, on this Mac", |ui| {
                    ui::status_pill(ui, label, tone);
                });
        });

        ui::bottom_bar(ctx, "bottom", |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                if !disc.login.is_empty() {
                    ui.label("·");
                    ui.label(format!("Tailscale as {}", disc.login));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui::ghost_button(
                        ui,
                        if self.settings_open {
                            "Close settings"
                        } else {
                            "Settings"
                        },
                    )
                    .clicked()
                    {
                        self.settings_open = !self.settings_open;
                    }
                });
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(P.bg))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(20.0);
                    ui::content_column(ui, COLUMN_WIDTH, |ui| {
                        ui.spacing_mut().item_spacing.y = 14.0;
                        self.setup_card(ui, &disc, &install);
                        if let Some((tone, text, _)) = &self.notice {
                            ui::notice(ui, *tone, text);
                        }
                        if prog.active() {
                            self.session_card(ui, &prog, &disc);
                        } else if let Step::Ended { error } = &prog.step {
                            self.ended_card(ui, &prog, error.as_deref());
                        }
                        self.pcs_card(ui, ctx, &disc, &prog);
                        if self.settings_open {
                            self.settings_card(ui, ctx, &prog);
                        }
                        ui.add_space(10.0);
                    });
                });
            });
        self.commit();
    }
}

impl ClientApp {
    fn setup_card(
        &mut self,
        ui: &mut egui::Ui,
        disc: &Discovery,
        install: &(bool, String, Option<Result<(), String>>),
    ) {
        let tailscale_problem = if !self.tailscale_ok {
            Some("Tailscale is not installed.".to_string())
        } else {
            disc.error.as_ref().map(|e| format!("Tailscale: {e}."))
        };
        if tailscale_problem.is_none() && self.moonlight_ok && install.2.is_none() && !install.0 {
            return;
        }
        ui::toned_card(ui, Tone::Accent, |ui| {
            ui::heading(
                ui,
                "Two things this Mac needs",
                Some("Tailscale carries the connection; Moonlight shows the picture."),
            );
            ui::setting_row(
                ui,
                "Tailscale",
                Some(
                    tailscale_problem
                        .as_deref()
                        .unwrap_or("Installed and signed in."),
                ),
                |ui| {
                    if tailscale_problem.is_some()
                        && ui::primary_button(ui, "Get Tailscale").clicked()
                    {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab("https://tailscale.com/download/mac"));
                    }
                    if tailscale_problem.is_none() {
                        ui::status_pill(ui, "Ready", Tone::Success);
                    }
                },
            );
            ui::row_separator(ui);
            let note = if install.0 {
                install.1.clone()
            } else {
                match &install.2 {
                    Some(Ok(())) => "Installed.".into(),
                    Some(Err(e)) => e.clone(),
                    None if self.moonlight_ok => "Installed.".into(),
                    None => "Downloads the latest release into /Applications.".into(),
                }
            };
            ui::setting_row(ui, "Moonlight", Some(&note), |ui| {
                if install.0 {
                    ui.add(egui::Spinner::new().size(16.0).color(P.accent));
                } else if self.moonlight_ok {
                    ui::status_pill(ui, "Ready", Tone::Success);
                } else if ui::primary_button(ui, "Install Moonlight").clicked() {
                    let state = self.install.clone();
                    {
                        let mut s = state.lock();
                        s.running = true;
                        s.result = None;
                        s.note = "Starting…".into();
                    }
                    std::thread::spawn(move || {
                        let note_state = state.clone();
                        let r = moonlight::install(&|m| note_state.lock().note = m.to_string())
                            .map_err(|e| e.to_string());
                        let mut s = state.lock();
                        s.running = false;
                        s.result = Some(r);
                    });
                }
            });
        });
    }

    fn pcs_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        disc: &Discovery,
        prog: &Progress,
    ) {
        ui::titled_card(
            ui,
            "Your PCs",
            Some("Windows machines on your Tailscale account."),
            |ui| {
                if disc.pcs.is_empty() {
                    let text = if disc.error.is_some() || !self.tailscale_ok {
                        "Sign in to Tailscale to see your PCs."
                    } else if disc.refreshed.is_none() {
                        "Looking for PCs…"
                    } else {
                        "No Windows PC on this tailnet yet. Install BroLink Host on the PC and sign it in to the same Tailscale account."
                    };
                    ui::empty_state(ui, text, disc.refreshed.is_none() && disc.error.is_none());
                }
                let pcs = disc.pcs.clone();
                for (i, pc) in pcs.iter().enumerate() {
                    if i > 0 {
                        ui::row_separator(ui);
                    }
                    let detail = describe(pc);
                    ui::list_row(ui, &pc.name, &detail, |ui| {
                        let busy = prog.active();
                        if pc.can_stream()
                            && self.moonlight_ok
                            && !busy
                            && ui::primary_button(ui, "Connect").clicked()
                        {
                            self.connect(ctx, pc);
                        }
                        if !pc.online
                            && pc.can_wake()
                            && !busy
                            && ui::ghost_button(ui, "Wake").clicked()
                        {
                            let r = session::wake_only(pc);
                            self.notice = Some(match r {
                                Ok(n) => (
                                    Tone::Info,
                                    format!("Sent {n} wake packets to {}.", pc.name),
                                    Instant::now(),
                                ),
                                Err(e) => (Tone::Danger, e.to_string(), Instant::now()),
                            });
                        }
                        if pc.power_allowed() {
                            self.power_menu(ui, pc);
                        }
                    });
                }
                if let Some((name, action)) = self.confirm.clone() {
                    ui::notice(
                    ui,
                    Tone::Danger,
                    &format!(
                        "{} {name}? Anything unsaved on it is lost; programs are closed without asking.",
                        action.label()
                    ),
                );
                    ui.horizontal(|ui| {
                        if ui::toned_button(ui, action.label(), Tone::Danger).clicked() {
                            if let Some(pc) = pcs.iter().find(|p| p.name == name).cloned() {
                                self.power(&pc, action);
                            }
                            self.confirm = None;
                        }
                        if ui::ghost_button(ui, "Cancel").clicked() {
                            self.confirm = None;
                        }
                    });
                }
            },
        );
    }

    fn power_menu(&mut self, ui: &mut egui::Ui, pc: &Pc) {
        ui::menu_button(ui, "PC", |ui| {
            if ui.button("Sleep").clicked() {
                self.power(pc, PowerAction::Sleep);
                ui.close_menu();
            }
            if ui.button("Restart…").clicked() {
                self.confirm = Some((pc.name.clone(), PowerAction::Restart));
                ui.close_menu();
            }
            if ui.button("Shut down…").clicked() {
                self.confirm = Some((pc.name.clone(), PowerAction::Shutdown));
                ui.close_menu();
            }
        });
    }

    fn session_card(&mut self, ui: &mut egui::Ui, prog: &Progress, disc: &Discovery) {
        let tone = if prog.step == Step::Streaming {
            Tone::Success
        } else {
            Tone::Accent
        };
        ui::toned_card(ui, tone, |ui| {
            let title = match &prog.step {
                Step::Waking => "Waking the PC",
                Step::Waiting => "Waiting for the PC",
                Step::Pairing { .. } => "Pairing",
                Step::Launching => "Connecting",
                Step::Streaming => "Streaming",
                _ => "",
            };
            ui::heading(ui, title, None);
            match &prog.step {
                Step::Streaming => {
                    ui.label(format!(
                        "Moonlight is showing {} full screen. Press Ctrl+Alt+Shift+Q in Moonlight to end the session, or switch back here.",
                        prog.pc
                    ));
                    ui::caption(
                        ui,
                        format!("{} min so far", prog.since.elapsed().as_secs() / 60),
                    );
                }
                Step::Pairing { pin } => {
                    ui.label(format!(
                        "First time with {}: Moonlight is pairing with PIN {pin}. BroLink Host on the PC enters it for you.",
                        prog.pc
                    ));
                }
                _ => {
                    ui::empty_state(ui, &prog.detail, true);
                }
            }
            ui.horizontal(|ui| {
                let label = if prog.step == Step::Streaming {
                    "Disconnect"
                } else {
                    "Cancel"
                };
                if ui::danger_button(ui, label).clicked() {
                    self.progress.lock().cancel = true;
                }
                if prog.step == Step::Streaming {
                    if let Some(pc) = disc
                        .pcs
                        .iter()
                        .find(|p| p.name == prog.pc && p.power_allowed())
                        .cloned()
                    {
                        if ui::ghost_button(ui, "Disconnect and sleep the PC").clicked() {
                            self.power(&pc, PowerAction::Sleep);
                        }
                    }
                }
            });
        });
    }

    fn ended_card(&mut self, ui: &mut egui::Ui, prog: &Progress, error: Option<&str>) {
        if error.is_none() && self.offer_sleep.is_none() {
            return;
        }
        let tone = if error.is_some() {
            Tone::Danger
        } else {
            Tone::Neutral
        };
        ui::toned_card(ui, tone, |ui| {
            match error {
                Some(e) => {
                    ui::heading(ui, &format!("Could not connect to {}", prog.pc), None);
                    ui.label(e);
                }
                None => {
                    ui::heading(
                        ui,
                        "Session ended",
                        Some(&format!("Leave {} on, or put it to sleep?", prog.pc)),
                    );
                }
            }
            ui.horizontal(|ui| {
                if let Some(pc) = self.offer_sleep.clone() {
                    if ui::primary_button(ui, "Sleep the PC").clicked() {
                        self.power(&pc, PowerAction::Sleep);
                        self.offer_sleep = None;
                        self.progress.lock().step = Step::Idle;
                    }
                }
                if ui::ghost_button(
                    ui,
                    if error.is_some() {
                        "Dismiss"
                    } else {
                        "Leave it on"
                    },
                )
                .clicked()
                {
                    self.offer_sleep = None;
                    self.progress.lock().step = Step::Idle;
                }
            });
        });
    }

    fn settings_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, prog: &Progress) {
        ui::titled_card(
            ui,
            "Settings",
            Some("Applied the next time you connect."),
            |ui| {
                let s = &mut self.cfg.stream;
                let native = Self::native_pixels(ctx);
                ui::setting_row(ui, "Mouse", Some("Desktop maps your cursor 1:1 onto the PC; Game hides it and sends raw movement."), |ui| {
                let mut game = s.game_mode;
                if ui::segmented(ui, &[(false, "Desktop"), (true, "Game")], &mut game) {
                    s.game_mode = game;
                    self.dirty = true;
                }
            });
                ui::row_separator(ui);
                let hint = format!(
                    "This Mac is {}×{}. Exact only with a virtual display on the PC (Apollo); Sunshine scales its monitor otherwise.",
                    native.0, native.1
                );
                ui::setting_row(ui, "Resolution", Some(&hint), |ui| {
                    if ui::segmented(
                        ui,
                        &[
                            (Resolution::P1080, "1080p"),
                            (Resolution::P1440, "1440p"),
                            (Resolution::P2160, "4K"),
                            (Resolution::Native, "This Mac"),
                        ],
                        &mut s.resolution,
                    ) {
                        self.dirty = true;
                    }
                });
                ui::row_separator(ui);
                ui::setting_row(ui, "Frame rate", None, |ui| {
                    if ui::segmented(ui, &[(60u32, "60"), (90, "90"), (120, "120")], &mut s.fps) {
                        self.dirty = true;
                    }
                });
                ui::row_separator(ui);
                ui::setting_row(
                    ui,
                    "Bitrate",
                    Some("Higher is sharper; lower survives a slow uplink."),
                    |ui| {
                        let mut mbps = s.bitrate_kbps / 1000;
                        if ui
                            .add(egui::Slider::new(&mut mbps, 5..=150).suffix(" Mbps"))
                            .changed()
                        {
                            s.bitrate_kbps = mbps * 1000;
                            self.dirty = true;
                        }
                    },
                );
                ui::row_separator(ui);
                ui::setting_row(ui, "Codec", Some("Auto picks the best both sides support: AV1 on an M3 with a recent GPU, otherwise HEVC."), |ui| {
                if ui::segmented(
                    ui,
                    &[(Codec::Auto, "Auto"), (Codec::Hevc, "HEVC"), (Codec::Av1, "AV1"), (Codec::H264, "H.264")],
                    &mut s.codec,
                ) {
                    self.dirty = true;
                }
            });
                ui::row_separator(ui);
                if ui::toggle_row(
                    ui,
                    &mut s.fullscreen,
                    "Full screen",
                    Some("Off opens Moonlight in a window."),
                ) {
                    self.dirty = true;
                }
                ui::row_separator(ui);
                ui::setting_row(
                    ui,
                    "App",
                    Some("What Sunshine launches. “Desktop” is the whole PC."),
                    |ui| {
                        let mut app = s.app.clone();
                        let mut changed = false;
                        if prog.apps.is_empty() {
                            changed = ui
                                .add(egui::TextEdit::singleline(&mut app).desired_width(160.0))
                                .changed();
                        } else {
                            egui::ComboBox::from_id_salt("app_pick")
                                .selected_text(app.clone())
                                .show_ui(ui, |ui| {
                                    for a in &prog.apps {
                                        if ui.selectable_value(&mut app, a.clone(), a).clicked() {
                                            changed = true;
                                        }
                                    }
                                });
                        }
                        if changed && !app.trim().is_empty() {
                            s.app = app;
                            self.dirty = true;
                        }
                    },
                );
                ui::row_separator(ui);
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.sleep_prompt,
                    "Offer to sleep the PC after each session",
                    Some("Asleep, the PC wakes from this Mac in seconds and uses almost no power."),
                ) {
                    self.dirty = true;
                }
            },
        );
    }
}

/// The second line under a PC's name. Kept short: it is cut, not wrapped.
fn describe(pc: &Pc) -> String {
    let ip = pc.ip.map(|ip| ip.to_string()).unwrap_or_default();
    if !pc.online {
        return if pc.can_wake() {
            "Asleep or off · Connect wakes it".into()
        } else {
            "Offline · turn it on once with BroLink Host running".into()
        };
    }
    match (&pc.host, pc.sunshine) {
        (Some(h), true) if h.setup.is_empty() => format!("Ready · {ip}"),
        (Some(_), true) => format!("Ready · {ip} · the PC still needs setup"),
        (Some(_), false) => format!("Online · {ip} · Sunshine is not running"),
        (None, true) => format!("Online · {ip} · Sunshine only, no wake or sleep"),
        (None, false) => format!("Online · {ip} · nothing to stream from"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KnownPc;

    #[test]
    fn descriptions_cover_every_state() {
        let mut pc = Pc {
            name: "Gaming-PC".into(),
            ip: Some("203.0.113.10".parse().unwrap()),
            ..Default::default()
        };
        assert!(describe(&pc).starts_with("Offline"));
        pc.known = Some(KnownPc {
            mac: Some("02:00:00:00:00:01".into()),
            ..Default::default()
        });
        assert!(describe(&pc).starts_with("Asleep"));
        pc.online = true;
        assert!(describe(&pc).contains("nothing to stream"));
        pc.sunshine = true;
        assert!(describe(&pc).contains("Sunshine only"));
        assert!(describe(&pc).len() < 60, "{}", describe(&pc));
        pc.host = Some(brolink_core::api::Status::default());
        assert_eq!(describe(&pc), "Ready · 203.0.113.10");
    }
}

/// Render the window to PNGs for review without a Mac:
///
/// ```text
/// cargo test -p brolink-client snapshots -- --ignored
/// ```
#[cfg(test)]
mod snapshots {
    use super::*;
    use crate::config::KnownPc;
    use brolink_core::api::Status;

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

    fn pcs() -> Discovery {
        let ready = Pc {
            node_id: "n1".into(),
            name: "Gaming-PC".into(),
            ip: Some("100.64.0.10".parse().unwrap()),
            online: true,
            host: Some(Status {
                app: "brolink".into(),
                power_allowed: true,
                ..Default::default()
            }),
            sunshine: true,
            known: Some(KnownPc {
                name: "Gaming-PC".into(),
                mac: Some("02:00:00:00:00:01".into()),
                lan_ip: Some("192.168.1.10".into()),
            }),
        };
        let asleep = Pc {
            node_id: "n2".into(),
            name: "Office".into(),
            ip: Some("100.64.0.30".parse().unwrap()),
            known: Some(KnownPc {
                mac: Some("aa:bb:cc:dd:ee:01".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let bare = Pc {
            node_id: "n3".into(),
            name: "Den".into(),
            ip: Some("100.64.0.31".parse().unwrap()),
            online: true,
            sunshine: true,
            ..Default::default()
        };
        Discovery {
            error: None,
            login: "user@example.com".into(),
            pcs: vec![ready, asleep, bare],
            refreshed: Some(Instant::now()),
        }
    }

    fn build(
        disc: Discovery,
        prog: Progress,
        settings: bool,
    ) -> egui_kittest::Harness<'static, ClientApp> {
        let disc = Arc::new(Mutex::new(disc));
        let prog = Arc::new(Mutex::new(prog));
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(560.0, 1200.0))
            .with_pixels_per_point(2.0)
            .with_max_steps(8)
            .build_eframe(move |cc| {
                let mut app = ClientApp::with_shared(cc, disc, prog, false);
                app.settings_open = settings;
                app
            });
        harness.run_steps(3);
        harness
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn lobby() {
        let mut h = build(pcs(), Progress::default(), false);
        save(h.render().unwrap(), "client-lobby.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn lobby_settings() {
        let mut h = build(pcs(), Progress::default(), true);
        save(h.render().unwrap(), "client-settings.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn waking() {
        let mut p = Progress {
            pc: "Office".into(),
            ..Default::default()
        };
        p.step = Step::Waking;
        p.detail = "Waking Office… 12s".into();
        let mut h = build(pcs(), p, false);
        save(h.render().unwrap(), "client-waking.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn pairing_then_streaming() {
        let mut p = Progress {
            pc: "Gaming-PC".into(),
            ..Default::default()
        };
        p.step = Step::Pairing { pin: "4821".into() };
        let mut h = build(pcs(), p.clone(), false);
        save(h.render().unwrap(), "client-pairing.png");
        p.step = Step::Streaming;
        let mut h = build(pcs(), p, false);
        save(h.render().unwrap(), "client-streaming.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn failed_and_empty() {
        let mut p = Progress {
            pc: "Office".into(),
            ..Default::default()
        };
        p.step = Step::Ended {
            error: Some("Office did not wake up. Wake-on-LAN only reaches it from its own network unless a Tailscale subnet router is on that network.".into()),
        };
        let mut h = build(pcs(), p, false);
        save(h.render().unwrap(), "client-failed.png");
        let mut h = build(
            Discovery {
                error: Some("Tailscale is stopped".into()),
                refreshed: Some(Instant::now()),
                ..Default::default()
            },
            Progress::default(),
            false,
        );
        save(h.render().unwrap(), "client-no-tailscale.png");
    }
}
