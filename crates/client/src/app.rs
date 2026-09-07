//! The window: a list of PCs with a Connect button each, settings, and the
//! stream screen once connected.

use crate::config::{ClientConfig, Codec, Resolution};
use crate::session::{self, Connect, Discovery, Live, Pc, Progress, Step, Target};
use crate::stream::{self, Action};
use crate::update;
use brolink_core::api::PowerAction;
use brolink_core::tailscale;
use brolink_stream::Event;
use brolink_ui::{self as ui, Tone, PALETTE as P};
use eframe::egui;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

const COLUMN_WIDTH: f32 = 560.0;

/// A one-line result, filled in by a worker thread.
type Notice = Arc<Mutex<Option<(Tone, String)>>>;

pub struct ClientApp {
    cfg: ClientConfig,
    dirty: bool,
    discovery: Arc<Mutex<Discovery>>,
    progress: Arc<Mutex<Progress>>,
    live: Arc<Mutex<Option<Live>>>,
    view: stream::View,
    tailscale_ok: bool,
    tailscale_checked: Instant,
    brand: ui::Brand,
    settings_open: bool,
    fullscreen: bool,
    /// A restart or shutdown waits for a second click.
    confirm: Option<(String, PowerAction)>,
    notice: Option<(Tone, String, Instant)>,
    /// After a session ends, offer to sleep this PC.
    offer_sleep: Option<Pc>,
    ended_seen: bool,
    pending_notice: Option<Notice>,
    updates: Arc<Mutex<update::State>>,
}

impl ClientApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let discovery = Arc::new(Mutex::new(Discovery::default()));
        session::spawn_discovery(discovery.clone(), cc.egui_ctx.clone());
        let app = Self::with_shared(cc, discovery.clone(), Arc::default(), true);
        update::spawn(
            app.updates.clone(),
            discovery,
            app.live.clone(),
            cc.egui_ctx.clone(),
        );
        app
    }

    fn with_shared(
        cc: &eframe::CreationContext<'_>,
        discovery: Arc<Mutex<Discovery>>,
        progress: Arc<Mutex<Progress>>,
        probe_tools: bool,
    ) -> Self {
        ui::apply(&cc.egui_ctx);
        if let Some(rs) = &cc.wgpu_render_state {
            crate::video::install(rs);
        }
        Self {
            cfg: ClientConfig::load(),
            dirty: false,
            discovery,
            progress,
            live: Arc::default(),
            view: stream::View::default(),
            tailscale_ok: !probe_tools || tailscale::cli().is_some(),
            tailscale_checked: Instant::now(),
            brand: ui::Brand::new(&cc.egui_ctx),
            settings_open: false,
            fullscreen: false,
            confirm: None,
            notice: None,
            offer_sleep: None,
            ended_seen: true,
            pending_notice: None,
            updates: Arc::default(),
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
        session::connect(Connect {
            target,
            settings: self.cfg.stream.clone(),
            native: Self::native_pixels(ctx),
            progress: self.progress.clone(),
            live: self.live.clone(),
            ctx: ctx.clone(),
        });
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen != on {
            self.fullscreen = on;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
    }

    /// Run `job` off the UI thread and show what it returns as the notice.
    fn notify_later(&mut self, job: impl FnOnce() -> (Tone, String) + Send + 'static) {
        let notice: Notice = Arc::default();
        let out = notice.clone();
        std::thread::spawn(move || *out.lock() = Some(job()));
        self.pending_notice = Some(notice);
    }

    fn power(&mut self, ip: std::net::Ipv4Addr, name: &str, action: PowerAction) {
        let name = name.to_string();
        // End the stream first so Sunshine sees a clean disconnect.
        self.disconnect();
        self.notify_later(move || {
            std::thread::sleep(Duration::from_millis(600));
            match session::power(ip, action) {
                Ok(()) => (
                    Tone::Success,
                    format!("{name}: {} requested.", action.label().to_lowercase()),
                ),
                Err(e) => (Tone::Danger, format!("{name}: {e}")),
            }
        });
    }

    fn test_wake(&mut self, pc: &Pc) {
        let pc = pc.clone();
        self.notify_later(move || match session::wake_test(&pc) {
            Ok(true) => (
                Tone::Success,
                format!("{} received the wake packet. Waking it from here will work.", pc.name),
            ),
            Ok(false) => (
                Tone::Danger,
                format!(
                    "The wake packet did not reach {} from this network. Its router would have to forward UDP 9 to it.",
                    pc.name
                ),
            ),
            Err(e) => (Tone::Danger, format!("{}: {e}", pc.name)),
        });
    }

    fn disconnect(&mut self) {
        let mut p = self.progress.lock();
        if p.active() {
            p.cancel = true;
        }
        drop(p);
        if let Some(l) = self.live.lock().as_ref() {
            l.session.stop();
        }
    }

    /// Move the stream's events into the progress state; drop the stream once
    /// it has fully stopped.
    fn poll_live(&mut self, ctx: &egui::Context) {
        let mut fullscreen = None;
        {
            let mut guard = self.live.lock();
            let Some(live) = guard.as_ref() else { return };
            let mut prog = self.progress.lock();
            while let Ok(ev) = live.events.try_recv() {
                match ev {
                    Event::Stage(s) => {
                        if prog.step == Step::Connecting {
                            prog.detail = s;
                        }
                    }
                    Event::Connected => {
                        prog.step = Step::Streaming;
                        prog.since = Instant::now();
                        if self.cfg.stream.fullscreen {
                            fullscreen = Some(true);
                        }
                    }
                    Event::Failed { stage, code } => {
                        prog.step = Step::Ended {
                            error: Some(format!("Connecting failed at {stage} (code {code}).")),
                        };
                    }
                    Event::Terminated { code, message } => {
                        let cancelled = prog.cancel;
                        prog.step = Step::Ended {
                            error: (code != 0 && !cancelled).then_some(message),
                        };
                    }
                    Event::Poor(p) => self.view.set_poor(p),
                }
            }
            // A stop we asked for ends without a Terminated event.
            let ended = matches!(prog.step, Step::Ended { .. });
            if live.session.finished() {
                if !ended {
                    prog.step = Step::Ended { error: None };
                }
                self.view.reset(ctx, Some(live));
                *guard = None;
                fullscreen = Some(false);
            } else if ended {
                live.session.stop();
            }
        }
        if let Some(on) = fullscreen {
            self.set_fullscreen(ctx, on);
        }
    }

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
        self.poll_live(ctx);
        self.poll_notice();

        let streaming = self.live.lock().is_some();
        if streaming {
            // The stream repaints itself on every decoded frame.
            ctx.request_repaint_after(Duration::from_millis(250));
            let live = self.live.clone();
            let guard = live.lock();
            if let Some(l) = guard.as_ref() {
                let actions = self.view.show(ctx, l, &self.cfg, self.fullscreen);
                let (ip, name) = (l.ip, l.pc.clone());
                drop(guard);
                for a in actions {
                    match a {
                        Action::Disconnect => self.disconnect(),
                        Action::Power(action) => self.power(ip, &name, action),
                        Action::Fullscreen(on) => {
                            self.set_fullscreen(ctx, on);
                            self.cfg.stream.fullscreen = on;
                            self.dirty = true;
                        }
                        Action::ToggleCmd => {
                            self.cfg.cmd_is_ctrl = !self.cfg.cmd_is_ctrl;
                            self.dirty = true;
                        }
                    }
                }
            }
            self.commit();
            return;
        }

        ctx.request_repaint_after(Duration::from_millis(400));
        if self.tailscale_checked.elapsed() > Duration::from_secs(2) {
            self.tailscale_checked = Instant::now();
            self.tailscale_ok = tailscale::cli().is_some();
        }
        let disc = self.discovery.lock().clone();
        let prog = self.progress.lock().clone();
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
            } else if prog.active() {
                ("Connecting", Tone::Accent)
            } else {
                ("Ready", Tone::Success)
            };
            self.brand.header(ui, "BroLink", |ui| {
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
                    let label = if self.settings_open {
                        "Close settings"
                    } else {
                        "Settings"
                    };
                    if ui::ghost_button(ui, label).clicked() {
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
                        self.tailscale_card(ui, &disc);
                        if let Some((tone, text, _)) = &self.notice {
                            ui::notice(ui, *tone, text);
                        }
                        if let Some((tone, text)) = self.updates.lock().notice.clone() {
                            ui::notice(ui, tone, &text);
                        }
                        if prog.active() {
                            self.session_card(ui, &prog);
                        } else if let Step::Ended { error } = &prog.step {
                            self.ended_card(ui, &prog, error.as_deref());
                        }
                        self.pcs_card(ui, ctx, &disc, &prog);
                        self.key_expiry_notices(ui, &disc);
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
    fn tailscale_card(&mut self, ui: &mut egui::Ui, disc: &Discovery) {
        let problem = if !self.tailscale_ok {
            Some("Tailscale is not installed.".to_string())
        } else {
            disc.error.as_ref().map(|e| format!("Tailscale: {e}."))
        };
        let Some(problem) = problem else { return };
        ui::toned_card(ui, Tone::Danger, |ui| {
            ui::heading(
                ui,
                "Tailscale is needed",
                Some("It connects this Mac to your PC from anywhere and confirms it is yours."),
            );
            ui::setting_row(ui, "Tailscale", Some(&problem), |ui| {
                if ui::primary_button(ui, "Get Tailscale").clicked() {
                    ui.ctx()
                        .open_url(egui::OpenUrl::new_tab("https://tailscale.com/download/mac"));
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
                            && !pc.remembered
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
                            self.notice = Some(match session::wake_only(pc) {
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
                            "{} {name}? Programs are closed without asking; anything unsaved is lost.",
                            action.label()
                        ),
                    );
                    ui.horizontal(|ui| {
                        if ui::toned_button(ui, action.label(), Tone::Danger).clicked() {
                            if let Some(pc) = pcs.iter().find(|p| p.name == name) {
                                if let Some(ip) = pc.ip {
                                    self.power(ip, &pc.name, action);
                                }
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

    /// A Tailscale node key that expires is the one thing that can take a
    /// far-away PC off the tailnet with nobody there to sign it back in.
    fn key_expiry_notices(&self, ui: &mut egui::Ui, disc: &Discovery) {
        for text in key_expiry_warnings(disc) {
            let urgent = text.contains("expired") || text.contains(" days") && days_in(&text) <= 30;
            ui::notice(ui, if urgent { Tone::Danger } else { Tone::Info }, &text);
        }
    }

    fn power_menu(&mut self, ui: &mut egui::Ui, pc: &Pc) {
        ui::menu_button(ui, "PC", |ui| {
            if ui.button("Sleep").clicked() {
                if let Some(ip) = pc.ip {
                    self.power(ip, &pc.name, PowerAction::Sleep);
                }
                ui.close_menu();
            }
            if ui.button("Restart…").clicked() {
                self.confirm = Some((pc.name.clone(), PowerAction::Restart));
                ui.close_menu();
            }
            if pc.can_wake() && ui.button("Test wake").clicked() {
                self.test_wake(pc);
                ui.close_menu();
            }
            if ui.button("Shut down…").clicked() {
                self.confirm = Some((pc.name.clone(), PowerAction::Shutdown));
                ui.close_menu();
            }
        });
    }

    fn session_card(&mut self, ui: &mut egui::Ui, prog: &Progress) {
        ui::toned_card(ui, Tone::Accent, |ui| {
            let title = match &prog.step {
                Step::Waking => "Waking the PC",
                Step::Waiting => "Waiting for the PC",
                Step::Pairing { .. } => "Pairing",
                Step::Launching | Step::Connecting | Step::Streaming => "Connecting",
                _ => "",
            };
            ui::heading(ui, title, None);
            match &prog.step {
                Step::Pairing { pin } => {
                    ui.label(format!(
                        "First time with {}. BroLink Host on the PC enters this PIN in Sunshine for you.",
                        prog.pc
                    ));
                    ui::display_digits(ui, pin);
                    if !prog.detail.is_empty() {
                        ui::notice(ui, Tone::Accent, &prog.detail);
                    }
                }
                _ => ui::empty_state(ui, &prog.detail, true),
            }
            ui.horizontal(|ui| {
                if ui::danger_button(ui, "Cancel").clicked() {
                    self.disconnect();
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
                    ui::heading(ui, &format!("{} disconnected", prog.pc), None);
                    ui.label(e);
                }
                None => ui::heading(
                    ui,
                    "Session ended",
                    Some(&format!("Leave {} on, or put it to sleep?", prog.pc)),
                ),
            }
            ui.horizontal(|ui| {
                if let Some(pc) = self.offer_sleep.clone() {
                    if ui::primary_button(ui, "Sleep the PC").clicked() {
                        if let Some(ip) = pc.ip {
                            self.power(ip, &pc.name, PowerAction::Sleep);
                        }
                        self.offer_sleep = None;
                        self.progress.lock().step = Step::Idle;
                    }
                }
                let label = if error.is_some() {
                    "Dismiss"
                } else {
                    "Leave it on"
                };
                if ui::ghost_button(ui, label).clicked() {
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
                let native = Self::native_pixels(ctx);
                let s = &mut self.cfg.stream;
                let hint = format!(
                "This screen is {}×{}. “This screen” is exact only with a virtual display on the PC; otherwise the PC's monitor is scaled.",
                native.0, native.1
            );
                ui::setting_row(ui, "Resolution", Some(&hint), |ui| {
                    if ui::segmented(
                        ui,
                        &[
                            (Resolution::P1080, "1080p"),
                            (Resolution::P1440, "1440p"),
                            (Resolution::P2160, "4K"),
                            (Resolution::Native, "This screen"),
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
                ui::setting_row(
                    ui,
                    "Codec",
                    Some("Auto uses HEVC when the PC can encode it."),
                    |ui| {
                        if ui::segmented(
                            ui,
                            &[(Codec::Auto, "Auto"), (Codec::H264, "H.264")],
                            &mut s.codec,
                        ) {
                            self.dirty = true;
                        }
                    },
                );
                ui::row_separator(ui);
                if ui::toggle_row(
                    ui,
                    &mut s.fullscreen,
                    "Full screen",
                    Some("Otherwise the stream fills this window."),
                ) {
                    self.dirty = true;
                }
                ui::row_separator(ui);
                ui::setting_row(
                    ui,
                    "App",
                    Some("What Sunshine starts. “Desktop” is the whole PC."),
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
                &mut self.cfg.cmd_is_ctrl,
                "Command key acts as Ctrl",
                Some("So ⌘C, ⌘V and ⌘Z do what you expect on the PC. Off makes it the Windows key."),
            ) {
                self.dirty = true;
            }
                ui::row_separator(ui);
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.sleep_prompt,
                    "Offer to sleep the PC after each session",
                    Some("Asleep, the PC wakes from this Mac in seconds."),
                ) {
                    self.dirty = true;
                }
                ui::row_separator(ui);
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.auto_update,
                    "Keep BroLink and your PCs up to date",
                    Some("Checks GitHub every few hours, installs new versions of this app, and sends BroLink Host updates to your PCs over Tailscale."),
                ) {
                    self.dirty = true;
                }
                let (message, checked) = {
                    let st = self.updates.lock();
                    (st.message.clone(), st.checked)
                };
                let hint = format!(
                    "{} · {}",
                    if message.is_empty() {
                        "Waiting for the first check."
                    } else {
                        &message
                    },
                    update::ago(checked)
                );
                ui::setting_row(ui, "Updates", Some(&hint), |ui| {
                    if ui::ghost_button(ui, "Check now").clicked() {
                        self.updates.lock().check_now = true;
                    }
                });
            },
        );
    }
}

/// One line per machine whose Tailscale key expires, this Mac included.
fn key_expiry_warnings(disc: &Discovery) -> Vec<String> {
    let mut out = Vec::new();
    for pc in &disc.pcs {
        if let Some(d) = pc.key_expiry_days {
            out.push(if d <= 0 {
                format!(
                    "{}'s Tailscale key has expired: it is off the tailnet until someone signs Tailscale in at the PC.",
                    pc.name
                )
            } else {
                format!(
                    "{}'s Tailscale key expires in {d} days. In the Tailscale admin console (login.tailscale.com/admin/machines), open {} and choose Disable key expiry; otherwise it drops off the tailnet and needs a sign-in at the PC.",
                    pc.name, pc.name
                )
            });
        }
    }
    if let Some(d) = disc.self_key_days {
        out.push(if d <= 0 {
            "This Mac's Tailscale key has expired; sign in to Tailscale again.".to_string()
        } else {
            format!(
                "This Mac's Tailscale key expires in {d} days; disable key expiry for it in the admin console as well."
            )
        });
    }
    out
}

/// The day count inside a warning line, for its tone.
fn days_in(text: &str) -> i64 {
    text.split(" in ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(i64::MAX)
}

/// The second line under a PC's name. Kept short: it is cut, not wrapped.
fn describe(pc: &Pc) -> String {
    let ip = pc.ip.map(|ip| ip.to_string()).unwrap_or_default();
    if pc.remembered {
        let seen = pc.known.as_ref().and_then(|k| k.last_seen_unix);
        return match seen {
            Some(t) => format!(
                "Last seen {} · Tailscale is off on this Mac",
                brolink_core::dates::ymd(t)
            ),
            None => "Tailscale is off on this Mac".into(),
        };
    }
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
        (Some(_), false) => format!("Online · {ip} · nothing is streaming from it yet"),
        (None, true) => format!("Online · {ip} · no BroLink Host: no wake or sleep"),
        (None, false) => format!("Online · {ip} · nothing to stream from"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KnownPc;

    #[test]
    fn remembered_pcs_say_when_they_were_seen_and_keys_get_warnings() {
        let pc = Pc {
            name: "Gaming-PC".into(),
            remembered: true,
            known: Some(KnownPc {
                last_seen_unix: Some(1_788_739_200),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            describe(&pc),
            "Last seen 2026-09-07 · Tailscale is off on this Mac"
        );
        let disc = Discovery {
            pcs: vec![
                Pc {
                    name: "Gaming-PC".into(),
                    key_expiry_days: Some(176),
                    ..Default::default()
                },
                Pc {
                    name: "Den".into(),
                    key_expiry_days: None,
                    ..Default::default()
                },
                Pc {
                    name: "Office".into(),
                    key_expiry_days: Some(-2),
                    ..Default::default()
                },
            ],
            self_key_days: Some(12),
            ..Default::default()
        };
        let w = key_expiry_warnings(&disc);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].contains("Gaming-PC") && w[0].contains("176 days"));
        assert_eq!(days_in(&w[0]), 176);
        assert!(w[1].contains("Office") && w[1].contains("expired"));
        assert!(w[2].starts_with("This Mac") && days_in(&w[2]) == 12);
        assert!(key_expiry_warnings(&Discovery::default()).is_empty());
    }

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
        assert!(describe(&pc).contains("no BroLink Host"));
        assert!(describe(&pc).len() < 60, "{}", describe(&pc));
        pc.host = Some(brolink_core::api::Status::default());
        assert_eq!(describe(&pc), "Ready · 203.0.113.10");
    }
}

/// `cargo test -p brolink-client snapshots -- --ignored` writes PNGs of each
/// screen to `target/ui-snapshots/`.
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
                ..Default::default()
            }),
            key_expiry_days: Some(176),
            ..Default::default()
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
            ..Default::default()
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
            .wgpu()
            .with_size(egui::vec2(640.0, 1100.0))
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
        let mut h = build(pcs(), Progress::default(), true);
        save(h.render().unwrap(), "client-settings.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn pairing_and_waking() {
        let mut p = Progress {
            pc: "Gaming-PC".into(),
            step: Step::Pairing { pin: "4821".into() },
            ..Default::default()
        };
        let mut h = build(pcs(), p.clone(), false);
        save(h.render().unwrap(), "client-pairing.png");
        p.step = Step::Waking;
        p.detail = "Waking Gaming-PC… 12s".into();
        let mut h = build(pcs(), p, false);
        save(h.render().unwrap(), "client-waking.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn failed_and_empty() {
        let p = Progress {
            pc: "Office".into(),
            step: Step::Ended {
                error: Some("Office did not wake up. A wake packet only reaches it from its own network, or through a router that forwards UDP 9 to it.".into()),
            },
            ..Default::default()
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

/// Streams from the Sunshine on this machine into the window for a few
/// seconds and saves what the window shows:
/// `BROLINK_DEV_LOCAL=1 cargo test -p brolink-client stream_snapshot -- --ignored --nocapture`
#[cfg(test)]
mod live_snapshot {
    use super::*;
    use crate::config::KnownPc;

    #[test]
    #[ignore = "needs a paired Sunshine on this machine and a GPU"]
    fn stream_snapshot() {
        let disc = Arc::new(Mutex::new(Discovery::default()));
        let prog = Arc::new(Mutex::new(Progress::default()));
        let mut harness = egui_kittest::Harness::builder()
            .wgpu()
            .with_size(egui::vec2(1512.0, 982.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(8)
            .build_eframe({
                let (disc, prog) = (disc.clone(), prog.clone());
                move |cc| {
                    let mut app = ClientApp::with_shared(cc, disc, prog, false);
                    app.cfg.stream.fullscreen = false;
                    app.cfg.stream.resolution = Resolution::P1080;
                    app.cfg.stream.codec = Codec::H264;
                    app
                }
            });
        harness.run_steps(2);
        let pc = Pc {
            node_id: "local".into(),
            name: "GAMING-PC".into(),
            ip: Some("127.0.0.1".parse().unwrap()),
            online: true,
            sunshine: true,
            known: Some(KnownPc {
                server_cert: std::fs::read(
                    std::env::temp_dir().join("brolink-pair-test/server.der"),
                )
                .ok()
                .map(|d| brolink_stream::nvhttp::hex(&d)),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ctx = harness.ctx.clone();
        harness.state_mut().connect(&ctx, &pc);
        let start = Instant::now();
        let mut shot = 0;
        while start.elapsed() < Duration::from_secs(10) {
            harness.run_steps(1);
            let step = prog.lock().step.clone();
            if let Step::Ended { error } = step {
                panic!("ended: {error:?}");
            }
            if step == Step::Streaming && start.elapsed() > Duration::from_secs(3) && shot == 0 {
                let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/ui-snapshots");
                std::fs::create_dir_all(&dir).unwrap();
                harness
                    .render()
                    .unwrap()
                    .save(dir.join("client-stream.png"))
                    .unwrap();
                eprintln!("wrote client-stream.png");
                shot += 1;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        assert_eq!(shot, 1, "never streamed: {:?}", prog.lock().step);
        harness.state_mut().disconnect();
        let t = Instant::now();
        while harness.state().live.lock().is_some() && t.elapsed() < Duration::from_secs(8) {
            harness.run_steps(1);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(harness.state().live.lock().is_none(), "stream did not stop");
    }
}
