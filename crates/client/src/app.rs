//! The window: a list of PCs with a Connect button each, settings, and the
//! stream screen once connected.

use crate::config::ClientConfig;
#[cfg(test)]
use crate::config::{Codec, Resolution};
use crate::handover::{self, Handover};
use crate::path;
use crate::session::{
    self, Connect, Discovery, Live, Pc, PeerRelayServers, Progress, Step, Target,
};
use crate::stream::{self, Action, Env};
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

const COLUMN_WIDTH: f32 = 860.0;

const RELAY_NONE: &str = "No relay on this network. Streams use Tailscale's default relay if a direct path is not available. You can run your own relay with the deploy kit.";
const RELAY_READY: &str = "Using your network relay when a direct path is not available.";
const RELAY_OFFLINE: &str = "A relay is on this network but it is offline. Streams use Tailscale's default relay until it comes back.";
const RELAY_CHECKING: &str = "A relay is on this network. BroLink could not tell whether this device may use it — that is not a denial. Peer relay needs Tailscale 1.86 or later on every device.";
const RELAY_UNAVAILABLE: &str = "A relay is on this network but it is not available to this device yet. That is not a denial. Confirm the relay is configured, and paste this grant into the tailnet policy if it is missing.";
#[cfg(test)]
const RELAY_UNGRANTED: &str = "A relay node is online but this device is not granted access.";
const RELAY_GRANT: &str = "{\n  \"src\": [\"autogroup:member\"],\n  \"dst\": [\"tag:relay\"],\n  \"app\": {\n    \"tailscale.com/cap/relay\": []\n  }\n}";
const RELAY_DOCS: &str = "https://tailscale.com/docs/features/peer-relay";

/// A one-line result, filled in by a worker thread.
type Notice = Arc<Mutex<Option<(Tone, String)>>>;

/// Give the window the stream's proportions, keeping its width, so the
/// picture fills it with no bar on any side. Only in a window: full screen
/// is the screen's shape, which Match screen already is.
fn fit_window_to(ctx: &egui::Context, width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let size = ctx.screen_rect().size();
    let want = window_size_for(size, width as f32 / height as f32);
    if (want.y - size.y).abs() >= 1.0 {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
    }
}

/// `current` with its height changed to give `aspect`, never under the
/// window's minimum.
fn window_size_for(current: egui::Vec2, aspect: f32) -> egui::Vec2 {
    let width = current.x.max(640.0);
    egui::Vec2::new(width, (width / aspect).round().max(420.0))
}

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
    notices_open: bool,
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
    display_at_connect: (u32, u32),
    display_change: Option<((u32, u32), Instant)>,
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
            app.progress.clone(),
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
            notices_open: false,
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
            display_at_connect: (0, 0),
            display_change: None,
        }
    }

    fn commit(&mut self) {
        if self.dirty {
            self.dirty = false;
            match self.cfg.save_settings() {
                Ok(merged) => self.cfg = merged,
                Err(e) => tracing::warn!("could not save settings: {e:#}"),
            }
        }
    }

    pub(crate) fn native_pixels(ctx: &egui::Context) -> (u32, u32) {
        let ppp = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(ctx.pixels_per_point());
        let size = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or_else(|| ctx.screen_rect().size());
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
        self.display_at_connect = Self::native_pixels(ctx);
        self.display_change = None;
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
                        } else {
                            fit_window_to(ctx, live.requested.0, live.requested.1);
                        }
                        self.view.stream_started(ctx, live, self.cfg.capture_mouse);
                        if let Some(problem) = host_audio_problem(&self.discovery, &live.node_id) {
                            self.view.toast(
                                Tone::Danger,
                                format!(
                                    "{} has no sound to send: the PC reports “{problem}”. See the host window.",
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
                        Action::MouseCapture(on) => {
                            self.cfg.capture_mouse = on;
                            self.dirty = true;
                        }
                        Action::ApplySettings(settings) => {
                            self.cfg.stream = settings;
                            self.dirty = true;
                            self.restart_capture = true;
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
            let size = Self::native_pixels(ctx);
            if size != self.display_at_connect && self.reconnect.is_none() {
                match self.display_change {
                    Some((candidate, at))
                        if candidate == size && at.elapsed() >= Duration::from_secs(1) =>
                    {
                        self.restart_capture = true;
                        self.reconnect = self.last_pc.clone();
                        self.disconnect();
                    }
                    Some((candidate, _)) if candidate == size => {}
                    _ => self.display_change = Some((size, Instant::now())),
                }
            } else {
                self.display_change = None;
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
                ui::status_pill(ui, label, tone);
            });
        });

        ui::bottom_bar(ctx, "bottom", |ui| {
            // The button first, so a long login is cut rather than pushing
            // it out of the window.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut line = format!("v{}", env!("CARGO_PKG_VERSION"));
                if !disc.login.is_empty() {
                    line.push_str(&format!(" · Tailscale as {}", disc.login));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(line).truncate());
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
                        if self.settings_open {
                            self.settings_card(ui, ctx, &prog);
                            self.relay_card(ui, &disc);
                        } else {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Your workspace, anywhere.").font(ui::theme::semibold(30.0)).color(P.text));
                            ui::caption(ui, "Choose a PC to open its desktop. Your display and quality settings follow you.");
                            ui.add_space(14.0);
                            self.pcs_card(ui, ctx, &disc, &prog);
                            ui::titled_card(ui, "Next session", None, |ui| {
                                let settings = path::effective(&self.cfg.stream, &path::Path::default());
                                let (w, h) = settings.resolution.pixels(Self::native_pixels(ctx));
                                ui.horizontal_wrapped(|ui| {
                                    ui::status_pill(ui, &format!("{w} × {h}"), Tone::Info);
                                    ui::status_pill(ui, &format!("{} fps", settings.fps), Tone::Neutral);
                                    ui::status_pill(ui, &format!("{} Mbps target", settings.bitrate_kbps / 1000), Tone::Neutral);
                                    if ui::ghost_button(ui, "Configure stream").clicked() { self.settings_open = true; }
                                });
                            });
                            if !path_warnings(&disc).is_empty() {
                                egui::CollapsingHeader::new("Connection details").show(ui, |ui| self.path_notices(ui, &disc));
                            }
                            self.key_expiry_notices(ui, &disc);
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
                        "First time with {}. BroLink Host on the PC enters this PIN for you.",
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
                    // The lighter profile is the likeliest to hold if the
                    // network, not the PC, ended the last one.
                    if ui::primary_button(ui, "Try again at Smooth (1080p · 12 Mbps)").clicked() {
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
                if crate::settings::stream_controls(ui, &mut self.cfg.stream, native) {
                    self.dirty = true;
                }
                ui::row_separator(ui);
                let s = &mut self.cfg.stream;
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
                    Some("What the PC starts. “Desktop” is the whole PC."),
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
                ui::row_separator(ui);
                ui::open_source_row(ui, &mut self.notices_open);
            },
        );
    }

    fn relay_card(&mut self, ui: &mut egui::Ui, disc: &Discovery) {
        let state = relay_state(disc);
        ui::titled_card(ui, "Relay", Some(state.sentence()), |ui| match &state {
            RelayState::Ready { name, ip } => {
                let detail = match ip {
                    Some(ip) => format!("{name} · {ip}"),
                    None => name.clone(),
                };
                ui::caption(ui, detail);
            }
            RelayState::Unavailable { name } => {
                ui::caption(
                    ui,
                    format!(
                        "{name} is on the tailnet. This grant is what Tailscale needs if the relay is yours:"
                    ),
                );
                ui::well(ui, |ui| {
                    ui.label(egui::RichText::new(RELAY_GRANT).monospace().color(P.muted));
                });
                ui.horizontal(|ui| {
                    if ui::ghost_button(ui, "Copy grant").clicked() {
                        ui.ctx().copy_text(RELAY_GRANT.to_string());
                        self.notice =
                            Some((Tone::Info, "Copied the grant.".into(), Instant::now()));
                    }
                    if ui::ghost_button(ui, "How it works").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(RELAY_DOCS));
                    }
                });
            }
            RelayState::None | RelayState::Offline { .. } | RelayState::Checking { .. } => {
                ui.horizontal(|ui| {
                    if ui::ghost_button(ui, "How it works").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(RELAY_DOCS));
                    }
                });
            }
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RelayState {
    None,
    Offline {
        name: String,
    },
    Checking {
        name: String,
    },
    Unavailable {
        name: String,
    },
    Ready {
        name: String,
        ip: Option<std::net::Ipv4Addr>,
    },
}

impl RelayState {
    fn sentence(&self) -> &'static str {
        match self {
            Self::None => RELAY_NONE,
            Self::Offline { .. } => RELAY_OFFLINE,
            Self::Checking { .. } => RELAY_CHECKING,
            Self::Unavailable { .. } => RELAY_UNAVAILABLE,
            Self::Ready { .. } => RELAY_READY,
        }
    }
}

fn ip_listed(servers: &[String], ip: Option<std::net::Ipv4Addr>) -> bool {
    ip.is_some_and(|ip| servers.iter().any(|s| s == &ip.to_string()))
}

/// Ready only when an online `tag:relay` node's Tailscale IP is in the debug
/// list. `Known([])` is not ACL denial. A nonempty list that matches no
/// tagged IP is not Ready.
fn relay_state(disc: &Discovery) -> RelayState {
    let Some(first) = disc.relays.first() else {
        return RelayState::None;
    };
    if let PeerRelayServers::Known(servers) = &disc.peer_relay_servers {
        if let Some(r) = disc
            .relays
            .iter()
            .find(|r| r.online && ip_listed(servers, r.ip))
        {
            return RelayState::Ready {
                name: r.name.clone(),
                ip: r.ip,
            };
        }
        if let Some(r) = disc
            .relays
            .iter()
            .find(|r| !r.online && ip_listed(servers, r.ip))
        {
            return RelayState::Offline {
                name: r.name.clone(),
            };
        }
    }
    let relay = disc.relays.iter().find(|r| r.online).unwrap_or(first);
    let name = relay.name.clone();
    if !disc.relays.iter().any(|r| r.online) {
        return RelayState::Offline { name };
    }
    match &disc.peer_relay_servers {
        PeerRelayServers::Unknown => RelayState::Checking { name },
        PeerRelayServers::Known(_) => RelayState::Unavailable { name },
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
                    "{} encodes video in software: the streaming engine found no GPU encoder there, so frames are slow to make whatever the network does. Check the GPU driver on the PC, or keep the stream at 1080p and 30 fps.",
                    pc.name
                ));
            }
            if !h.streamer.audio_problem.is_empty() {
                out.push(format!(
                    "{} has no sound to send: the PC reports “{}”. A PC with no monitor or speakers has no audio device to capture; give it a virtual one (Steam's Streaming Speakers, or VB-CABLE) and pick it as the PC's audio sink.",
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
    use crate::session::Relay;

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
            ..Default::default()
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
                ..Default::default()
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

    fn a_relay(online: bool) -> Relay {
        Relay {
            name: "relay-sj".into(),
            ip: Some("100.64.0.40".parse().unwrap()),
            online,
        }
    }

    fn disc_with(relays: Vec<Relay>, servers: PeerRelayServers) -> Discovery {
        Discovery {
            relays,
            peer_relay_servers: servers,
            ..Default::default()
        }
    }

    #[test]
    fn relay_card_copy_covers_the_three_states_and_does_not_invent_a_denial() {
        assert_eq!(RelayState::None.sentence(), RELAY_NONE);
        assert_eq!(
            RelayState::Ready {
                name: "relay-sj".into(),
                ip: None,
            }
            .sentence(),
            RELAY_READY
        );
        for s in [RELAY_NONE, RELAY_READY, RELAY_UNAVAILABLE] {
            assert!(!s.to_lowercase().contains("shared"));
            assert!(!s.contains("BroLink relay"));
        }

        assert_eq!(
            relay_state(&Discovery::default()),
            RelayState::None,
            "no tagged node is no relay, even before the debug command runs"
        );
        assert_eq!(
            relay_state(&disc_with(
                vec![],
                PeerRelayServers::Known(vec!["100.64.0.40".into()]),
            )),
            RelayState::None,
            "a grant without a tagged node is not a relay on this network"
        );

        let online = vec![a_relay(true)];
        assert_eq!(
            relay_state(&disc_with(online.clone(), PeerRelayServers::Unknown)),
            RelayState::Checking {
                name: "relay-sj".into()
            }
        );
        assert_ne!(
            relay_state(&disc_with(online.clone(), PeerRelayServers::Unknown)).sentence(),
            RELAY_UNGRANTED,
            "Unknown is not a denial"
        );
        assert!(
            relay_state(&disc_with(online.clone(), PeerRelayServers::Unknown))
                .sentence()
                .contains("not a denial")
        );
        assert!(
            relay_state(&disc_with(online.clone(), PeerRelayServers::Unknown))
                .sentence()
                .contains("1.86")
        );

        let empty = relay_state(&disc_with(online.clone(), PeerRelayServers::Known(vec![])));
        assert_eq!(
            empty,
            RelayState::Unavailable {
                name: "relay-sj".into()
            }
        );
        assert_ne!(
            empty.sentence(),
            RELAY_UNGRANTED,
            "Known([]) is not ACL denial"
        );
        assert!(
            empty.sentence().contains("not a denial"),
            "{}",
            empty.sentence()
        );

        assert_eq!(
            relay_state(&disc_with(
                online.clone(),
                PeerRelayServers::Known(vec!["100.64.0.40".into()]),
            )),
            RelayState::Ready {
                name: "relay-sj".into(),
                ip: Some("100.64.0.40".parse().unwrap()),
            }
        );

        let mismatch = relay_state(&disc_with(
            online.clone(),
            PeerRelayServers::Known(vec!["100.64.0.99".into()]),
        ));
        assert_eq!(
            mismatch,
            RelayState::Unavailable {
                name: "relay-sj".into()
            },
            "a nonempty list that matches no tagged IP is not Ready"
        );
        assert_ne!(mismatch.sentence(), RELAY_READY);
        assert_ne!(mismatch.sentence(), RELAY_UNGRANTED);

        assert_eq!(
            relay_state(&disc_with(
                vec![a_relay(false)],
                PeerRelayServers::Known(vec![]),
            )),
            RelayState::Offline {
                name: "relay-sj".into()
            },
            "an offline node is not 'online but ungranted'"
        );
        assert_eq!(
            relay_state(&disc_with(
                vec![a_relay(false)],
                PeerRelayServers::Known(vec!["100.64.0.40".into()]),
            )),
            RelayState::Offline {
                name: "relay-sj".into()
            },
            "permitted server whose tagged node is offline is Offline, not Ready"
        );

        let decoy_online = Relay {
            name: "decoy".into(),
            ip: Some("100.64.0.40".parse().unwrap()),
            online: true,
        };
        let permitted_offline = Relay {
            name: "relay-sj".into(),
            ip: Some("100.64.0.41".parse().unwrap()),
            online: false,
        };
        assert_eq!(
            relay_state(&disc_with(
                vec![decoy_online, permitted_offline],
                PeerRelayServers::Known(vec!["100.64.0.41".into()]),
            )),
            RelayState::Offline {
                name: "relay-sj".into()
            },
            "must not Ready an unmatched online tagged node"
        );

        assert!(RELAY_GRANT.contains("autogroup:member"));
        assert!(RELAY_GRANT.contains("tag:relay"));
        assert!(RELAY_GRANT.contains("tailscale.com/cap/relay"));
        assert!(!RELAY_GRANT.contains("\"*\""));
        assert_eq!(RELAY_DOCS, "https://tailscale.com/docs/features/peer-relay");
    }
}

/// `cargo test -p brolink-client snapshots -- --ignored` writes PNGs of each
/// screen to `target/ui-snapshots/`.
#[cfg(test)]
mod snapshots {
    use super::*;
    use crate::config::KnownPc;
    use crate::session::Relay;
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
                ..Default::default()
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
        build_with(disc, prog, settings, size, ppp, true)
    }

    fn build_with(
        disc: Discovery,
        prog: Progress,
        settings: bool,
        size: egui::Vec2,
        ppp: f32,
        gpu: bool,
    ) -> egui_kittest::Harness<'static, ClientApp> {
        let disc = Arc::new(Mutex::new(disc));
        let prog = Arc::new(Mutex::new(prog));
        let mut builder = egui_kittest::Harness::builder()
            .with_size(size)
            .with_pixels_per_point(ppp)
            .with_max_steps(8);
        if gpu {
            builder = builder.wgpu();
        }
        let mut harness = builder.build_eframe(move |cc| {
            let mut app = ClientApp::with_shared(cc, disc, prog, false);
            app.settings_open = settings;
            app
        });
        harness.run_steps(3);
        harness
    }

    /// Every widget sits inside the window, and no two controls overlap.
    fn assert_fits(h: &egui_kittest::Harness<'_, ClientApp>, width: f32) {
        use egui::accesskit::Role;
        use egui_kittest::kittest::{By, Queryable};
        let mut controls = Vec::new();
        for node in h.query_all(By::new().predicate(|_| true)) {
            let Some(b) = node.raw_bounds() else { continue };
            let text = node.label().or_else(|| node.value()).unwrap_or_default();
            assert!(
                b.x0 >= -0.5 && b.x1 <= f64::from(width) + 0.5,
                "{:?} {text:?} runs past the {width}-wide window: {b:?}",
                node.role()
            );
            if matches!(
                node.role(),
                Role::Button | Role::CheckBox | Role::RadioButton | Role::ComboBox
            ) {
                controls.push((
                    text,
                    egui::Rect::from_min_max(
                        egui::pos2(b.x0 as f32, b.y0 as f32),
                        egui::pos2(b.x1 as f32, b.y1 as f32),
                    ),
                ));
            }
        }
        assert!(!controls.is_empty(), "no controls found");
        for (i, (a_name, a)) in controls.iter().enumerate() {
            for (b_name, b) in &controls[i + 1..] {
                let hit = a.intersect(*b);
                assert!(
                    hit.width() <= 1.0 || hit.height() <= 1.0,
                    "{a_name} {a:?} overlaps {b_name} {b:?}"
                );
            }
        }
    }

    const MIN_WINDOW: egui::Vec2 = egui::vec2(640.0, 420.0);

    #[test]
    fn lobby_fits_the_minimum_window() {
        use egui_kittest::kittest::Queryable;
        let h = build_with(pcs(), Progress::default(), false, MIN_WINDOW, 1.0, false);
        assert_fits(&h, 640.0);
        assert_eq!(h.query_all_by_label("Connect").count(), 3);
        let mut disc = pcs();
        disc.login = "someone.with.a.long.name@example-mail-provider.com".into();
        let h = build_with(disc, Progress::default(), false, MIN_WINDOW, 1.0, false);
        assert_fits(&h, 640.0);
        assert!(h.query_by_label("Settings").is_some());
    }

    #[test]
    fn settings_fit_the_minimum_window() {
        use egui_kittest::kittest::Queryable;
        let h = build_with(pcs(), Progress::default(), true, MIN_WINDOW, 1.0, false);
        assert_fits(&h, 640.0);
        assert!(h.query_by_label("Close settings").is_some());
    }

    #[test]
    fn open_source_row_shows_the_bundled_notices() {
        use egui_kittest::kittest::Queryable;
        let mut h = build_with(pcs(), Progress::default(), true, MIN_WINDOW, 1.0, false);
        assert!(h.query_by_label("Open source").is_some());
        assert!(h.query_by_label_contains("moonlight-common-c").is_none());
        h.get_by_label("Show notices").click();
        h.run_steps(3);
        assert!(h.state().notices_open);
        assert!(h.query_by_label_contains("moonlight-common-c").is_some());
        assert!(h.query_by_label("Hide notices").is_some());
        assert_fits(&h, 640.0);
    }

    #[test]
    fn an_empty_lobby_fits_the_minimum_window() {
        let disc = Discovery {
            login: "user@example.com".into(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let h = build_with(disc, Progress::default(), false, MIN_WINDOW, 1.0, false);
        assert_fits(&h, 640.0);
    }

    #[test]
    fn settings_opens_and_closes_as_a_bool_toggled_view() {
        use egui_kittest::kittest::Queryable;
        let mut h = build_with(pcs(), Progress::default(), false, MIN_WINDOW, 1.0, false);
        assert!(!h.state().settings_open);
        assert!(h.query_by_label("How it works").is_none());
        h.get_by_label("Settings").click();
        h.run_steps(3);
        assert!(h.state().settings_open);
        assert!(h.query_by_label("Close settings").is_some());
        assert!(h.query_by_label("How it works").is_some());
        h.get_by_label("Close settings").click();
        h.run_steps(3);
        assert!(!h.state().settings_open);
        assert!(h.query_by_label("Settings").is_some());
        assert!(h.query_by_label("How it works").is_none());
    }

    const TAGGED_RELAY_STATUS: &str = r#"{"BackendState":"Running","Peer":{
      "nodekey:relay": {"ID":"nRELAY","HostName":"sj-instance","OS":"linux","Online":true,
                        "TailscaleIPs":["100.64.0.40"],"Tags":["tag:relay"]},
      "nodekey:winrelay": {"ID":"nWINREL","HostName":"relay-pc","OS":"windows","Online":true,
                           "TailscaleIPs":["100.64.0.42"],"Tags":["tag:relay"]},
      "nodekey:pc": {"ID":"nPC","HostName":"Gaming-PC","OS":"windows","Online":true,
                     "TailscaleIPs":["100.64.0.41"]},
      "nodekey:other": {"ID":"nOTHER","HostName":"tagged-pc","OS":"windows","Online":true,
                        "TailscaleIPs":["100.64.0.43"],"Tags":["tag:other"]}
    }}"#;

    fn discovery_from_status(json: &str) -> Discovery {
        let st = brolink_core::tailscale::parse_status(json).expect("fixture");
        Discovery {
            login: "user@example.com".into(),
            refreshed: Some(Instant::now()),
            pcs: st
                .windows_peers()
                .into_iter()
                .map(|n| Pc {
                    node_id: n.id.clone(),
                    name: n.host_name.clone(),
                    ip: n.ipv4(),
                    online: n.online,
                    sunshine: true,
                    ..Default::default()
                })
                .collect(),
            relays: st
                .relay_peers()
                .into_iter()
                .map(|n| Relay {
                    name: n.host_name.clone(),
                    ip: n.ipv4(),
                    online: n.online,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn tagged_relay_nodes_stay_out_of_the_pc_list_and_in_discovery() {
        use egui_kittest::kittest::Queryable;
        let disc = discovery_from_status(TAGGED_RELAY_STATUS);
        assert_eq!(
            disc.pcs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Gaming-PC", "tagged-pc"]
        );
        assert_eq!(
            disc.relays
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["relay-pc", "sj-instance"]
        );

        let h = build_with(
            disc.clone(),
            Progress::default(),
            false,
            MIN_WINDOW,
            1.0,
            false,
        );
        assert_fits(&h, 640.0);
        assert!(h.query_by_label("Gaming-PC").is_some());
        assert!(h.query_by_label("tagged-pc").is_some());
        assert!(h.query_by_label("sj-instance").is_none());
        assert!(h.query_by_label("relay-pc").is_none());
        assert_eq!(h.query_all_by_label("Connect").count(), 2);

        let h = build_with(disc, Progress::default(), true, MIN_WINDOW, 1.0, false);
        assert_fits(&h, 640.0);
        assert!(h.query_by_label("How it works").is_some());
        assert!(h.query_by_label("sj-instance").is_none());
        assert!(h.query_by_label("relay-pc").is_none());
    }

    fn relay_disc(online: bool, servers: PeerRelayServers) -> Discovery {
        Discovery {
            login: "user@example.com".into(),
            refreshed: Some(Instant::now()),
            relays: vec![Relay {
                name: "relay-sj".into(),
                ip: Some("100.64.0.40".parse().unwrap()),
                online,
            }],
            peer_relay_servers: servers,
            ..Default::default()
        }
    }

    #[test]
    fn relay_card_fits_the_minimum_window_in_every_state() {
        use egui_kittest::kittest::Queryable;
        let states = [
            Discovery {
                login: "user@example.com".into(),
                refreshed: Some(Instant::now()),
                ..Default::default()
            },
            relay_disc(false, PeerRelayServers::Unknown),
            relay_disc(true, PeerRelayServers::Unknown),
            relay_disc(true, PeerRelayServers::Known(vec![])),
            relay_disc(true, PeerRelayServers::Known(vec!["100.64.0.99".into()])),
            relay_disc(true, PeerRelayServers::Known(vec!["100.64.0.40".into()])),
        ];
        for disc in states {
            let h = build_with(disc, Progress::default(), true, MIN_WINDOW, 1.0, false);
            assert_fits(&h, 640.0);
        }
        let h = build_with(
            relay_disc(true, PeerRelayServers::Known(vec![])),
            Progress::default(),
            true,
            MIN_WINDOW,
            1.0,
            false,
        );
        assert!(h.query_by_label("Copy grant").is_some());
        assert!(h.query_by_label("How it works").is_some());
        let h = build_with(
            Discovery {
                login: "user@example.com".into(),
                refreshed: Some(Instant::now()),
                ..Default::default()
            },
            Progress::default(),
            true,
            MIN_WINDOW,
            1.0,
            false,
        );
        assert!(h.query_by_label("Copy grant").is_none());
        assert!(h.query_by_label("How it works").is_some());
        let h = build_with(
            relay_disc(true, PeerRelayServers::Known(vec!["100.64.0.40".into()])),
            Progress::default(),
            true,
            MIN_WINDOW,
            1.0,
            false,
        );
        assert!(h.query_by_label("Copy grant").is_none());
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
            ..Default::default()
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
        let mut h = build_sized(pcs(), Progress::default(), false, MIN_WINDOW, 2.0);
        save(h.render().unwrap(), "client-lobby-640x420.png");
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

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn open_source_notices() {
        use egui_kittest::kittest::Queryable;
        let mut h = build_sized(
            pcs(),
            Progress::default(),
            true,
            egui::vec2(1024.0, 2400.0),
            1.0,
        );
        h.get_by_label("Show notices").click();
        h.run_steps(3);
        save(h.render().unwrap(), "client-open-source.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn relay_card_states() {
        let none = Discovery {
            login: "user@example.com".into(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let mut h = build(none, Progress::default(), true);
        save(h.render().unwrap(), "client-relay-none.png");
        let mut h = build(
            relay_disc(false, PeerRelayServers::Unknown),
            Progress::default(),
            true,
        );
        save(h.render().unwrap(), "client-relay-offline.png");
        let mut h = build(
            relay_disc(true, PeerRelayServers::Unknown),
            Progress::default(),
            true,
        );
        save(h.render().unwrap(), "client-relay-checking.png");
        let mut h = build(
            relay_disc(true, PeerRelayServers::Known(vec![])),
            Progress::default(),
            true,
        );
        save(h.render().unwrap(), "client-relay-unavailable.png");
        let mut h = build(
            relay_disc(true, PeerRelayServers::Known(vec!["100.64.0.99".into()])),
            Progress::default(),
            true,
        );
        save(h.render().unwrap(), "client-relay-mismatch.png");
        let mut h = build(
            relay_disc(true, PeerRelayServers::Known(vec!["100.64.0.40".into()])),
            Progress::default(),
            true,
        );
        save(h.render().unwrap(), "client-relay-ready.png");
        let mut h = build_sized(
            relay_disc(true, PeerRelayServers::Known(vec![])),
            Progress::default(),
            true,
            MIN_WINDOW,
            2.0,
        );
        save(h.render().unwrap(), "client-relay-unavailable-640x420.png");
    }
}

#[cfg(test)]
mod window_fit {
    use super::window_size_for;

    #[test]
    fn a_window_takes_the_streams_proportions_at_its_own_width() {
        let s = window_size_for(egui::Vec2::new(1100.0, 720.0), 3024.0 / 1964.0);
        assert_eq!(s.x, 1100.0);
        assert!((s.y - 714.0).abs() <= 1.0, "{}", s.y);
        let s = window_size_for(egui::Vec2::new(1100.0, 720.0), 16.0 / 9.0);
        assert!((s.y - 619.0).abs() <= 1.0, "{}", s.y);
        let tiny = window_size_for(egui::Vec2::new(300.0, 100.0), 16.0 / 9.0);
        assert_eq!(
            tiny,
            egui::Vec2::new(640.0, 420.0),
            "never under the minimum"
        );
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
            .with_pixels_per_point(2.0)
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
        let mut sampled = 0;
        while start.elapsed() < Duration::from_secs(20) {
            harness.run_steps(1);
            let step = prog.lock().step.clone();
            if let Step::Ended { error } = step {
                panic!("ended: {error:?}");
            }
            if let Some(live) = harness.state().live.lock().as_ref() {
                decoded = live.frames.seq();
                let second = start.elapsed().as_secs();
                if second > sampled {
                    sampled = second;
                    eprintln!("sample {second}s: {:?}", live.session.stats());
                }
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
