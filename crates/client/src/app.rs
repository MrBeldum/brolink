//! The window: a list of PCs with a Connect button each, settings, and the
//! stream screen once connected.

use crate::config::{ClientConfig, Codec, Quality, Resolution};
use crate::handover::{self, Handover};
use crate::path;
use crate::session::{self, Connect, Discovery, Live, Pc, Progress, Step, Target};
use crate::stream::{self, Action, Env, QualityChoice};
use crate::update;
use brolink_core::api::PowerAction;
use brolink_core::tailscale;
use brolink_stream::Event;
use brolink_ui::{self as ui, Tone, PALETTE as P};
use eframe::egui;
use parking_lot::Mutex;
use semver::Version;
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
    /// The PC last connected to, for a reconnect at another quality.
    last_pc: Option<Pc>,
    /// Connect again as soon as the current stream has stopped.
    reconnect: Option<Pc>,
    restart_capture: bool,
    /// What the PC said about its black picture, and whether a fix is on
    /// its way. Asked for once per stream, by the thread that answers.
    video_help: Arc<Mutex<Option<crate::display::Help>>>,
    asked_about_video: bool,
    /// An install of BroLink Host through the stream, while it runs and a
    /// little after.
    handover: Option<Handover>,
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
            last_pc: None,
            reconnect: None,
            restart_capture: false,
            video_help: Arc::default(),
            asked_about_video: false,
            handover: None,
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
        self.last_pc = Some(pc.clone());
        self.asked_about_video = false;
        *self.video_help.lock() = None;
        session::connect(Connect {
            target,
            settings: self.cfg.stream.clone(),
            restart_capture: std::mem::take(&mut self.restart_capture),
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

    /// The first time a connected stream diagnoses its own picture as
    /// broken, ask the PC what it can see. One question per stream: the
    /// answer does not change while the same capture keeps failing.
    fn ask_why_black(&mut self, ctx: &egui::Context, live: &Live) {
        if self.asked_about_video
            || live.session.stats().video_problem.is_none()
            || !live.session.connected()
        {
            return;
        }
        self.asked_about_video = true;
        let (ip, help, ctx) = (live.ip, self.video_help.clone(), ctx.clone());
        std::thread::spawn(move || {
            let answer = match crate::display::ask(ip) {
                Ok(report) => crate::display::verdict(&report),
                Err(e) => {
                    tracing::warn!("display report: {e:#}");
                    return;
                }
            };
            if !answer.message.is_empty() {
                *help.lock() = Some(answer);
                ctx.request_repaint();
            }
        });
    }

    /// Turn the PC's HDR desktop off, then start a fresh capture: the old
    /// one keeps producing the black frames it was already producing.
    fn turn_off_hdr(&mut self, _ctx: &egui::Context, ip: std::net::Ipv4Addr) {
        let help = self.video_help.clone();
        if help.lock().as_ref().is_some_and(|h| h.busy) {
            return;
        }
        if let Some(h) = help.lock().as_mut() {
            h.busy = true;
        }
        self.notify_later(move || {
            let told = match crate::display::set_hdr(ip, false) {
                Ok(state) => {
                    if crate::display::hdr_is_on(&serde_json::json!({"advanced_color": state})) {
                        (Tone::Danger, "The PC kept its HDR desktop on.".to_string())
                    } else {
                        (
                            Tone::Success,
                            "HDR is off on the PC. Starting a fresh capture…".to_string(),
                        )
                    }
                }
                Err(e) => (Tone::Danger, format!("Could not turn HDR off: {e}")),
            };
            if let Some(h) = help.lock().as_mut() {
                h.busy = false;
                h.hdr_is_on = told.0 != Tone::Success;
            }
            told
        });
        // The picture cannot recover on the capture that is already black.
        self.restart_capture = true;
        self.reconnect = self.last_pc.clone();
        self.disconnect();
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
                        if let Some(problem) = host_audio_problem(&self.discovery, &live.node_id) {
                            self.view.toast(
                                Tone::Danger,
                                format!(
                                    "{} has no sound to send: Sunshine reports “{problem}”. See the host window.",
                                    live.pc
                                ),
                            );
                        }
                    }
                    Event::Failed { stage, code } => {
                        let cancelled = prog.cancel;
                        prog.step = Step::Ended {
                            error: (!cancelled)
                                .then_some(format!("Connecting failed at {stage} (code {code}).")),
                        };
                    }
                    Event::Terminated { code, message } => {
                        let cancelled = prog.cancel;
                        prog.step = Step::Ended {
                            error: (code != 0 && !cancelled).then_some(message),
                        };
                    }
                    Event::Poor(p) => self.view.set_poor(p),
                    Event::NoAudio(e) => self
                        .view
                        .toast(Tone::Danger, format!("No sound on this Mac: {e}.")),
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
                // Text copied on the PC lands in this Mac's clipboard.
                if let Some(text) = l.clipboard.take_incoming() {
                    ctx.copy_text(text);
                }
                let disc = self.discovery.lock().clone();
                let pc_now = disc.pcs.iter().find(|p| p.node_id == l.node_id);
                let latest = self.updates.lock().latest.clone();
                let old_host = pc_now
                    .and_then(|p| p.host.as_ref())
                    .and_then(|h| stream::old_host(&h.version, latest.as_ref()));
                self.tend_handover(pc_now, &l.pc);
                self.ask_why_black(ctx, l);
                let help = self.video_help.lock().clone();
                let env = Env {
                    live: l,
                    cfg: &self.cfg,
                    fullscreen: self.fullscreen,
                    path: pc_now.map(|p| p.path.clone()),
                    old_host,
                    handover: self.handover.as_ref().map(|h| h.status(&l.pc)),
                    video_help: help,
                };
                let actions = self.view.show(ctx, &env);
                let (ip, name, input) = (l.ip, l.pc.clone(), l.input.clone());
                drop(guard);
                for a in actions {
                    match a {
                        Action::Disconnect => self.disconnect(),
                        Action::RestartStream => {
                            self.restart_capture = true;
                            self.reconnect = self.last_pc.clone();
                            self.disconnect();
                        }
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
                        Action::Quality(choice) => {
                            match choice {
                                QualityChoice::Auto => self.cfg.stream.quality = Quality::Auto,
                                QualityChoice::Preset(p) => self.cfg.stream.apply_preset(p),
                            }
                            self.dirty = true;
                            self.reconnect = self.last_pc.clone();
                            self.disconnect();
                        }
                        Action::TurnOffHdr => self.turn_off_hdr(ctx, ip),
                        Action::InstallHost => {
                            self.start_handover(ctx, ip, &name, input.clone(), &disc)
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
        if let Some(pc) = self.reconnect.take() {
            if prog.active() {
                self.reconnect = Some(pc);
            } else {
                // Fresh details if discovery has them; the saved ones otherwise.
                let fresh = disc
                    .pcs
                    .iter()
                    .find(|p| p.node_id == pc.node_id)
                    .cloned()
                    .unwrap_or(pc);
                self.connect(ctx, &fresh);
                self.commit();
                return;
            }
        }
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
                        self.path_notices(ui, &disc);
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

    /// Why a PC is relayed, a PC whose host encodes in software, and a PC
    /// whose host is too old to update itself: each gets one line under
    /// the list, with what to do about it.
    fn path_notices(&self, ui: &mut egui::Ui, disc: &Discovery) {
        for text in path_warnings(disc) {
            ui::notice(ui, Tone::Accent, &text);
        }
    }

    /// Start installing the newest BroLink Host on the PC through the
    /// stream. See `handover.rs`.
    fn start_handover(
        &mut self,
        ctx: &egui::Context,
        ip: std::net::Ipv4Addr,
        name: &str,
        input: brolink_stream::Input,
        disc: &Discovery,
    ) {
        let Some(mac_ip) = disc.self_ip else {
            self.view.toast(
                Tone::Danger,
                "Tailscale on this Mac has no address to serve from.",
            );
            return;
        };
        let running = disc
            .pcs
            .iter()
            .find(|p| p.ip == Some(ip))
            .and_then(|p| p.host.as_ref())
            .and_then(|h| Version::parse(&h.version).ok())
            .unwrap_or_else(|| Version::new(0, 0, 0));
        let release = self.updates.lock().release.clone();
        if let Some(rel) = &release {
            if let Err(e) = handover::usable(rel, &running) {
                self.view.toast(Tone::Danger, format!("{name}: {e}."));
                return;
            }
        }
        let token = brolink_core::update::token(self.cfg.github_token.as_deref());
        self.handover = Some(Handover::start(
            release,
            token,
            mac_ip,
            ip,
            input,
            ctx.clone(),
        ));
        self.view.toast(
            Tone::Info,
            format!("Installing BroLink Host on {name} through the stream…"),
        );
    }

    /// Notice when the install through the stream has landed (the host
    /// reports the new version) or has given up, and let it go.
    fn tend_handover(&mut self, pc_now: Option<&Pc>, name: &str) {
        let Some(h) = &self.handover else { return };
        let p = h.progress();
        let landed = p.version.as_ref().is_some_and(|v| {
            pc_now
                .and_then(|pc| pc.host.as_ref())
                .and_then(|st| Version::parse(&st.version).ok())
                .is_some_and(|running| running >= *v)
        });
        if landed {
            self.view.toast(
                Tone::Success,
                format!(
                    "{name} now runs BroLink Host {}. Updates arrive by themselves from here on.",
                    p.version.map(|v| v.to_string()).unwrap_or_default()
                ),
            );
            self.handover = None;
        } else if !h.active() && p.since.elapsed() > Duration::from_secs(20) {
            self.handover = None;
        }
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
                if let (Some(_), Some(pc)) = (error, self.last_pc.clone()) {
                    // A stream that died is usually one the path could not
                    // carry; the smallest ask is the likeliest to hold.
                    if ui::primary_button(ui, "Try again at Smooth").clicked() {
                        self.cfg.stream.apply_preset(crate::config::Preset::Smooth);
                        self.dirty = true;
                        self.reconnect = Some(pc.clone());
                        self.progress.lock().step = Step::Idle;
                    }
                    if ui::ghost_button(ui, "Try again").clicked() {
                        self.reconnect = Some(pc);
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
                ui::setting_row(
                    ui,
                    "Quality",
                    Some("Auto picks resolution, frame rate and bitrate from the path to the PC each time you connect: less through a relay or across a long round trip, more on a LAN. Custom uses the values below. The toolbar's Quality menu switches while streaming."),
                    |ui| {
                        if ui::segmented(
                            ui,
                            &[(Quality::Auto, "Auto"), (Quality::Custom, "Custom")],
                            &mut s.quality,
                        ) {
                            self.dirty = true;
                        }
                    },
                );
                ui::row_separator(ui);
                let custom = s.quality == Quality::Custom;
                let hint = format!(
                "This screen is {}×{}. “This screen” is exact only with a virtual display on the PC; otherwise the PC's monitor is scaled.{}",
                native.0, native.1,
                if custom { "" } else { " Set by Auto." }
            );
                ui.add_enabled_ui(custom, |ui| {
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
                        if ui::segmented(
                            ui,
                            &[(30u32, "30"), (60, "60"), (90, "90"), (120, "120")],
                            &mut s.fps,
                        ) {
                            self.dirty = true;
                        }
                    });
                    ui::row_separator(ui);
                    ui::setting_row(
                        ui,
                        "Bitrate",
                        Some("Higher is sharper; lower survives a slow uplink. A relayed path carries a few Mbps at best."),
                        |ui| {
                            let mut mbps = s.bitrate_kbps / 1000;
                            if ui
                                .add(egui::Slider::new(&mut mbps, 2..=150).suffix(" Mbps"))
                                .changed()
                            {
                                s.bitrate_kbps = mbps * 1000;
                                self.dirty = true;
                            }
                        },
                    );
                });
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
                    Some("Asleep, Tailscale is off. This Mac can only wake the PC from that PC's own network, not from elsewhere."),
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
    let path = if pc.path.direct.is_some() {
        format!(" · {}", pc.path.label())
    } else {
        String::new()
    };
    match (&pc.host, pc.sunshine) {
        (Some(h), true) if h.setup.is_empty() => format!("Ready · {ip}{path}"),
        (Some(_), true) => format!("Ready · {ip} · the PC still needs setup"),
        (Some(_), false) => format!("Online · {ip} · nothing is streaming from it yet"),
        (None, true) => format!("Online · {ip}{path} · no BroLink Host: no wake or sleep"),
        (None, false) => format!("Online · {ip} · nothing to stream from"),
    }
}

/// Sunshine's audio failure on the PC with `node_id`, as its host last
/// reported it (3.1+ hosts); `None` when sound works or nothing is known.
fn host_audio_problem(discovery: &Mutex<Discovery>, node_id: &str) -> Option<String> {
    discovery
        .lock()
        .pcs
        .iter()
        .find(|p| p.node_id == node_id)
        .and_then(|p| p.host.as_ref())
        .map(|h| h.streamer.audio_problem.clone())
        .filter(|s| !s.is_empty())
}

/// One line per PC that is relayed, encodes in software, has no sound to
/// send, or runs a host too old to update itself.
fn path_warnings(disc: &Discovery) -> Vec<String> {
    let mut out = Vec::new();
    for pc in disc.pcs.iter().filter(|p| !p.remembered && p.online) {
        let nat = pc.host.as_ref().and_then(|h| h.nat.as_ref());
        if let Some(t) = path::explain(&pc.name, &pc.path, nat, disc.self_nat.as_ref()) {
            out.push(t);
        }
        if let Some(h) = &pc.host {
            if h.streamer.encoder == "software" {
                out.push(format!(
                    "{} encodes video in software: Sunshine found no GPU encoder there, so frames are slow to make whatever the network does. Check the GPU driver on the PC, or keep the stream at 1080p and 30 fps.",
                    pc.name
                ));
            }
            if !h.streamer.audio_problem.is_empty() {
                out.push(format!(
                    "{} has no sound to send: Sunshine reports “{}”. A PC with no monitor or speakers has no audio device to capture; give it a virtual one (Steam's Streaming Speakers, or VB-CABLE) and pick it as Sunshine's audio sink.",
                    pc.name, h.streamer.audio_problem
                ));
            }
            if let Ok(v) = Version::parse(&h.version) {
                if !brolink_core::update::host_can_receive_update(&v) {
                    out.push(update::old_host_message(&pc.name, &v));
                }
            }
        }
    }
    out
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
        pc.path = crate::path::Path {
            direct: Some(false),
            relay: "tok".into(),
            rtt_ms: Some(210),
        };
        assert_eq!(
            describe(&pc),
            "Ready · 203.0.113.10 · Relayed via Tokyo · 210 ms"
        );
    }

    #[test]
    fn the_lobby_warns_about_relays_software_encoders_and_old_hosts() {
        let mut gaming_pc = Pc {
            name: "Gaming-PC".into(),
            online: true,
            ip: Some("100.64.0.10".parse().unwrap()),
            path: crate::path::Path {
                direct: Some(false),
                relay: "tok".into(),
                rtt_ms: Some(200),
            },
            host: Some(brolink_core::api::Status {
                version: "3.0.0".into(),
                streamer: brolink_core::api::Streamer {
                    encoder: "software".into(),
                    audio_problem:
                        "Unable to initialize audio capture. The stream will not have audio.".into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let disc = Discovery {
            pcs: vec![gaming_pc.clone()],
            ..Default::default()
        };
        let w = path_warnings(&disc);
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(w[0].contains("Tokyo relay"), "{}", w[0]);
        assert!(w[1].contains("software"), "{}", w[1]);
        assert!(
            w[2].contains("no sound") && w[2].contains("audio capture"),
            "{}",
            w[2]
        );
        assert!(w[3].contains("Update BroLink Host"), "{}", w[3]);
        assert_eq!(
            host_audio_problem(&Mutex::new(disc.clone()), &gaming_pc.node_id).as_deref(),
            Some("Unable to initialize audio capture. The stream will not have audio.")
        );
        // Direct, GPU encoder, sound, current host: nothing to say.
        gaming_pc.path.direct = Some(true);
        let h = gaming_pc.host.as_mut().unwrap();
        h.version = "3.1.0".into();
        h.streamer.encoder = "nvenc".into();
        h.streamer.audio_problem.clear();
        let disc = Discovery {
            pcs: vec![gaming_pc.clone()],
            ..Default::default()
        };
        assert!(path_warnings(&disc).is_empty());
        // Nothing is said about a PC that is off.
        gaming_pc.online = false;
        gaming_pc.path.direct = Some(false);
        let disc = Discovery {
            pcs: vec![gaming_pc],
            ..Default::default()
        };
        assert!(path_warnings(&disc).is_empty());
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
                version: "3.0.0".into(),
                power_allowed: true,
                streamer: brolink_core::api::Streamer {
                    encoder: "software".into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            path: crate::path::Path {
                direct: Some(false),
                relay: "tok".into(),
                rtt_ms: Some(210),
            },
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
            self_nat: Some(brolink_core::api::NatReport {
                udp: true,
                ipv4: true,
                ipv6: false,
                hard: Some(false),
                portmap: false,
                derp: "tok".into(),
            }),
            ..Default::default()
        }
    }

    fn build(
        disc: Discovery,
        prog: Progress,
        settings: bool,
    ) -> egui_kittest::Harness<'static, ClientApp> {
        build_sized(disc, prog, settings, egui::vec2(640.0, 1100.0), 2.0)
    }

    fn build_sized(
        disc: Discovery,
        prog: Progress,
        settings: bool,
        size: egui::Vec2,
        ppp: f32,
    ) -> egui_kittest::Harness<'static, ClientApp> {
        let disc = Arc::new(Mutex::new(disc));
        let prog = Arc::new(Mutex::new(prog));
        let mut harness = egui_kittest::Harness::builder()
            .wgpu()
            .with_size(size)
            .with_pixels_per_point(ppp)
            .with_max_steps(8)
            .build_eframe(move |cc| {
                let mut app = ClientApp::with_shared(cc, disc, prog, false);
                app.settings_open = settings;
                app
            });
        harness.run_steps(3);
        harness
    }

    /// The stream screen with its toolbar, before any picture has arrived:
    /// the session points at an address that never answers.
    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn stream_toolbar() {
        let mut h = build_sized(
            pcs(),
            Progress::default(),
            false,
            egui::vec2(1400.0, 860.0),
            1.0,
        );
        let ctx = h.ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let frames = Arc::new(brolink_stream::FrameSlot::default());
        let path = crate::path::Path {
            direct: Some(false),
            relay: "tok".into(),
            rtt_ms: Some(210),
        };
        let settings = crate::path::effective(&crate::config::StreamSettings::default(), &path);
        let session = brolink_stream::Session::start(
            brolink_stream::session::Server {
                address: "10.255.255.1".into(),
                app_version: "7.1.431.-1".into(),
                gfe_version: "3.23.0.74".into(),
                rtsp_url: "rtsp://10.255.255.1:48010".into(),
                codec_mode_support: 1,
            },
            brolink_stream::Settings {
                width: 1920,
                height: 1080,
                fps: 30,
                bitrate_kbps: 4000,
                hevc: true,
                remote: true,
            },
            [0; 16],
            [0; 16],
            frames.clone(),
            tx,
            || {},
        );
        let ip: std::net::Ipv4Addr = "100.64.0.10".parse().unwrap();
        let input = session.input();
        let live = Live {
            pc: "Gaming-PC".into(),
            node_id: "n1".into(),
            ip,
            session,
            input,
            frames,
            events: rx,
            started: Instant::now(),
            codec: "HEVC",
            requested: (1920, 1080, 30),
            settings,
            path,
            clipboard: crate::clipboard::Sync::spawn(ip, ctx),
        };
        *h.state().live.lock() = Some(live);
        h.state().progress.lock().step = Step::Streaming;
        h.run_steps(3);
        save(h.render().unwrap(), "client-stream-toolbar.png");
        h.state_mut().disconnect();
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
        let _ = tracing_subscriber::fmt().with_target(false).try_init();
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
                    if std::env::var_os("BROLINK_TEST_PC").is_none() {
                        app.cfg.stream.resolution = Resolution::P1080;
                        app.cfg.stream.codec = Codec::H264;
                    }
                    if let Ok(codec) = std::env::var("BROLINK_TEST_CODEC") {
                        app.cfg.stream.codec = match codec.as_str() {
                            "h264" => Codec::H264,
                            "hevc" => Codec::Hevc,
                            _ => panic!("BROLINK_TEST_CODEC must be h264 or hevc"),
                        };
                    }
                    app
                }
            });
        harness.run_steps(2);
        let pc = if let Ok(name) = std::env::var("BROLINK_TEST_PC") {
            let cfg = ClientConfig::load();
            let (id, known) = cfg
                .pcs
                .iter()
                .find(|(id, pc)| **id == name || pc.name == name)
                .expect("BROLINK_TEST_PC must name a saved PC");
            Pc {
                node_id: id.clone(),
                name: known.name.clone(),
                ip: Some(known.tailscale_ip.as_ref().unwrap().parse().unwrap()),
                online: true,
                sunshine: true,
                known: Some(known.clone()),
                ..Default::default()
            }
        } else {
            Pc {
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
            }
        };
        let ctx = harness.ctx.clone();
        harness.state_mut().connect(&ctx, &pc);
        let start = Instant::now();
        let mut shot = 0;
        let mut decoded = 0;
        let mut black = None;
        let mut problem = None;
        while start.elapsed() < Duration::from_secs(20) {
            harness.run_steps(1);
            let step = prog.lock().step.clone();
            if let Step::Ended { error } = step {
                panic!("ended: {error:?}");
            }
            if let Some(live) = harness.state().live.lock().as_ref() {
                decoded = live.frames.seq();
            }
            if step == Step::Streaming
                && decoded > 30
                && start.elapsed() > Duration::from_secs(15)
                && shot == 0
            {
                if let Some(live) = harness.state().live.lock().as_ref() {
                    problem = live.session.stats().video_problem;
                    if let Some(frame) = live.frames.take() {
                        black = Some(frame.is_black());
                        eprintln!("black picture: {black:?}; video problem: {problem:?}");
                        live.frames.publish(frame);
                    }
                }
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
        eprintln!("decoded {decoded} frames");
        let expect_black = std::env::var_os("BROLINK_TEST_EXPECT_BLACK").is_some();
        let mut restarted = !expect_black;
        if expect_black && black == Some(true) {
            use egui_kittest::kittest::Queryable;
            let before = harness.state().live.lock().as_ref().unwrap().started;
            harness.get_by_label("Restart stream").click();
            let restart = Instant::now();
            while restart.elapsed() < Duration::from_secs(25) {
                harness.run_steps(1);
                if let Some(live) = harness.state().live.lock().as_ref() {
                    if live.started > before && live.session.connected() && live.frames.seq() > 30 {
                        restarted = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        harness.state_mut().disconnect();
        let t = Instant::now();
        while harness.state().live.lock().is_some() && t.elapsed() < Duration::from_secs(8) {
            harness.run_steps(1);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(harness.state().live.lock().is_none(), "stream did not stop");
        assert_eq!(shot, 1, "never decoded video: {:?}", prog.lock().step);
        assert!(decoded > 60, "too few decoded frames: {decoded}");
        if expect_black {
            assert_eq!(black, Some(true));
            assert!(problem
                .as_deref()
                .is_some_and(|p| p.contains("black picture")));
            assert!(
                restarted,
                "Restart stream did not establish a new video session"
            );
        } else {
            assert_eq!(
                black,
                Some(false),
                "the host supplied no visible picture: {problem:?}"
            );
        }
    }
}
