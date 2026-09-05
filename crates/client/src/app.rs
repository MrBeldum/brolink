//! Client lobby + stream view.

use crate::clipboard::ClipboardBridge;
use crate::decode::VideoSink;
use crate::input_map::InputCollector;
use crate::session::{ClientCmd, ClientEvent, ConnectRequest};
use brolink_core::config::{ClientConfig, QualityPreset, StreamQuality};
use brolink_core::discovery::{decode_beacon, join_multicast, prune, upsert, DiscoveredHost};
use brolink_core::identity::Identity;
use brolink_core::net::bind_udp_blocking_reuse;
use brolink_core::proto::{ControlMsg, PowerAction, DEFAULT_PORT};
use brolink_ui::{self as ui, Tone, PALETTE as P};
use eframe::egui;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Widest the lobby's column of cards gets; wider than this and a 1100 px
/// window reads like a settings page rather than a launcher.
const COLUMN_WIDTH: f32 = 720.0;

/// The key that frees the pointer, as the user should press it. F8 is a media
/// key on Mac keyboards unless Fn is held, so say so.
const RELEASE_KEY: &str = if cfg!(target_os = "macos") {
    "fn+F8"
} else {
    "F8"
};

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Lobby,
    Connecting,
    Pin,
    Stream,
}

pub struct ClientApp {
    cfg: ClientConfig,
    identity: Identity,
    cmd: mpsc::UnboundedSender<ClientCmd>,
    ev: mpsc::UnboundedReceiver<ClientEvent>,
    video: Arc<VideoSink>,
    target: String,
    pin: String,
    /// Put the caret in the PIN box the first frame it appears.
    pin_focus: bool,
    mode: Mode,
    log: Vec<String>,
    error: Option<String>,
    hosts: Vec<DiscoveredHost>,
    video_tex: Option<egui::TextureHandle>,
    video_size: [usize; 2],
    captured: bool,
    fullscreen: bool,
    input: InputCollector,
    stats: String,
    encoder: String,
    host_name: String,
    discovered: Arc<parking_lot::Mutex<Vec<DiscoveredHost>>>,
    clipboard: ClipboardBridge,
    last_clip: Instant,
    user_hangup: bool,
    ever_ready: bool,
    reconnect_at: Option<Instant>,
    reconnect_attempt: u32,
    show_hud: bool,
    /// Whether the connected host honours power commands.
    power_control: bool,
    /// A restart or shut down waiting for the user to confirm it.
    pending_power: Option<PowerAction>,
    brand: ui::Brand,
}

impl ClientApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: ClientConfig,
        identity: Identity,
        cmd: mpsc::UnboundedSender<ClientCmd>,
        ev: mpsc::UnboundedReceiver<ClientEvent>,
        video: Arc<VideoSink>,
    ) -> Self {
        ui::apply(&cc.egui_ctx);
        let brand = ui::Brand::new(&cc.egui_ctx);
        let target = cfg.last_ticket.clone();
        let show_hud = cfg.show_hud;
        let discovered = Arc::new(parking_lot::Mutex::new(Vec::new()));
        spawn_discovery(discovered.clone());
        Self {
            cfg,
            identity,
            cmd,
            ev,
            video,
            target,
            pin: String::new(),
            pin_focus: false,
            mode: Mode::Lobby,
            log: vec!["BroLink client ready.".into()],
            error: None,
            hosts: Vec::new(),
            video_tex: None,
            video_size: [1920, 1080],
            captured: false,
            fullscreen: false,
            input: InputCollector::new(),
            stats: String::new(),
            encoder: String::new(),
            host_name: String::new(),
            discovered,
            clipboard: ClipboardBridge::new(),
            last_clip: Instant::now(),
            user_hangup: false,
            ever_ready: false,
            reconnect_at: None,
            reconnect_attempt: 0,
            show_hud,
            power_control: false,
            pending_power: None,
            brand,
        }
    }

    pub fn auto_connect_if_target(&mut self) {
        if !self.target.trim().is_empty() && self.mode == Mode::Lobby {
            self.connect();
        }
    }

    /// A connection the user asked for: wakes the PC if it does not answer.
    fn connect(&mut self) {
        self.connect_with(true);
    }

    fn connect_with(&mut self, wake: bool) {
        self.error = None;
        self.user_hangup = false;
        self.reconnect_at = None;
        self.pending_power = None;
        self.cfg.last_ticket = self.target.clone();
        if let Err(e) = self.cfg.save() {
            tracing::warn!("could not save client config: {e:#}");
        }
        self.mode = Mode::Connecting;
        self.log
            .push(format!("Connecting to {}…", short(&self.target)));
        let _ = self.cmd.send(ClientCmd::Connect(Box::new(ConnectRequest {
            target: self.target.clone(),
            cfg: self.cfg.clone(),
            identity: self.identity.clone(),
            wake_mac: self.cfg.wake_mac_for(&self.target),
            wake,
        })));
    }

    /// Ask the PC to sleep, restart, or shut down. The host ends the session
    /// itself; no reconnect follows, or the Mac would wake it straight back up.
    fn send_power(&mut self, action: PowerAction) {
        self.pending_power = None;
        self.user_hangup = true;
        self.ever_ready = false;
        self.log
            .push(format!("Asked the PC to {}.", action.as_str()));
        let _ = self
            .cmd
            .send(ClientCmd::Control(ControlMsg::Power { action }));
    }

    /// Leave the stream and return to the lobby, releasing the pointer.
    fn leave_stream(&mut self, ctx: &egui::Context) {
        self.set_capture(ctx, false);
        self.mode = Mode::Lobby;
        self.video_tex = None;
        if self.fullscreen {
            self.fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn set_capture(&mut self, ctx: &egui::Context, on: bool) {
        if self.captured == on {
            return;
        }
        self.captured = on;
        // Tell the host, so it can switch between relative and absolute mouse
        // handling instead of guessing from the event stream.
        let _ = self.cmd.send(ClientCmd::Control(ControlMsg::MouseCaptured {
            captured: on,
            relative: on,
        }));
        let grab = if on {
            egui::viewport::CursorGrab::Locked
        } else {
            egui::viewport::CursorGrab::None
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!on));
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.ev.try_recv() {
            match ev {
                ClientEvent::Log(s) => self.log.push(s),
                ClientEvent::NeedPin { host } => {
                    self.host_name = host;
                    self.mode = Mode::Pin;
                    self.pin_focus = true;
                    self.log.push("Host asked for a pairing PIN.".into());
                }
                ClientEvent::Ready {
                    ready: r,
                    host_name,
                } => {
                    self.encoder = r.encoder.clone();
                    self.host_name = host_name;
                    self.power_control = r.power_control;
                    self.video_size = [r.width as usize, r.height as usize];
                    self.mode = Mode::Stream;
                    self.pin.clear();
                    self.ever_ready = true;
                    self.reconnect_attempt = 0;
                    self.cfg
                        .remember_host(&self.host_name, &self.target, r.wake_mac.as_deref());
                    let _ = self.cfg.save();
                    self.log.push(format!(
                        "Streaming {}x{} @ {} fps via {}",
                        r.width, r.height, r.fps, r.encoder
                    ));
                }
                ClientEvent::Stats {
                    rtt_ms,
                    fps,
                    bitrate_kbps,
                    loss,
                    decoder_ms,
                } => {
                    self.stats = format!(
                        "{fps:.0} fps  ·  {:.1} Mbps  ·  {rtt_ms:.0} ms rtt  ·  dec {decoder_ms:.1} ms  ·  loss {loss:.1}%  ·  {}",
                        bitrate_kbps / 1000.0,
                        self.encoder
                    );
                }
                ClientEvent::Error(e) => {
                    self.log.push(format!("error: {e}"));
                    self.error = Some(e);
                    self.schedule_reconnect();
                }
                ClientEvent::Disconnected => {
                    if self.mode != Mode::Lobby {
                        self.log.push("Disconnected.".into());
                    }
                    self.mode = Mode::Lobby;
                    self.captured = false;
                    self.stats.clear();
                    self.schedule_reconnect();
                }
                ClientEvent::Clipboard { text } => self.clipboard.apply_remote(&text),
            }
        }
        if self.log.len() > 200 {
            let extra = self.log.len() - 200;
            self.log.drain(..extra);
        }
    }

    /// Upload the newest decoded frame, handing its buffer back for reuse.
    fn refresh_video(&mut self, ctx: &egui::Context) {
        let Some(pic) = self.video.take() else {
            return;
        };
        self.video_size = [pic.width, pic.height];
        let img = egui::ColorImage::from_rgba_unmultiplied([pic.width, pic.height], &pic.rgba);
        match self.video_tex.as_mut() {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => {
                self.video_tex = Some(ctx.load_texture("remote", img, egui::TextureOptions::LINEAR))
            }
        }
        self.video.recycle(pic.rgba);
    }

    fn schedule_reconnect(&mut self) {
        if self.user_hangup || !self.cfg.auto_reconnect || !self.ever_ready {
            return;
        }
        if self.reconnect_attempt >= 6 {
            self.log
                .push("Gave up reconnecting. Click Connect when the PC is back.".into());
            self.ever_ready = false;
            return;
        }
        let wait = 1u64 << self.reconnect_attempt.min(4);
        self.reconnect_attempt += 1;
        self.reconnect_at = Some(Instant::now() + Duration::from_secs(wait));
        self.log.push(format!(
            "Reconnecting in {wait}s… (attempt {})",
            self.reconnect_attempt
        ));
    }

    fn hangup(&mut self) {
        self.user_hangup = true;
        self.reconnect_at = None;
        self.ever_ready = false;
        let _ = self.cmd.send(ClientCmd::Disconnect);
    }
}

impl eframe::App for ClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint continuously while streaming; the lobby can idle.
        if self.mode == Mode::Stream {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        self.drain_events();
        if let Some(at) = self.reconnect_at {
            if Instant::now() >= at && self.mode == Mode::Lobby {
                // Never wake on an automatic retry: the drop may be the PC
                // going to sleep on purpose.
                self.connect_with(false);
            }
        }

        if self.mode == Mode::Stream {
            self.refresh_video(ctx);
            self.stream_ui(ctx);
        } else {
            self.hosts = self.discovered.lock().clone();
            self.lobby_ui(ctx);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Let the network thread say goodbye so the host stops encoding and
        // releases any keys we left held.
        let _ = self.cmd.send(ClientCmd::Disconnect);
        std::thread::sleep(Duration::from_millis(120));
    }
}

// ---------------------------------------------------------------------------
// Lobby
// ---------------------------------------------------------------------------

impl ClientApp {
    fn lobby_ui(&mut self, ctx: &egui::Context) {
        ui::top_bar(ctx, "top", |ui| {
            let (label, tone) = self.status();
            self.brand
                .header(ui, "BroLink", "Play your Windows PC", |ui| {
                    ui::status_pill(ui, label, tone);
                });
        });
        ui::bottom_bar(ctx, "bottom", |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.label("·");
                ui.label(format!("this Mac is “{}”", self.cfg.name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{RELEASE_KEY} frees the mouse  ·  F11 fullscreen  ·  Ctrl+Shift+Q disconnects"
                    ));
                });
            });
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(P.bg))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(22.0);
                    ui::content_column(ui, COLUMN_WIDTH, |ui| {
                        ui.spacing_mut().item_spacing.y = 14.0;
                        // Lead with whatever starts a session fastest: a PIN the
                        // host is waiting on, a PC we know, one announcing itself
                        // on this network, then a pasted ticket. Everything else
                        // is tucked below.
                        if self.mode == Mode::Pin {
                            self.pin_card(ui);
                        }
                        self.saved_card(ui);
                        self.discovery_card(ui);
                        self.connect_card(ui);
                        ui::collapsible(ui, "settings", "Settings", false, |ui| {
                            self.settings_ui(ui)
                        });
                        ui::collapsible(ui, "log", "Log", false, |ui| {
                            ui::log_view(ui, "log_lines", &self.log, 180.0);
                        });
                        ui.add_space(10.0);
                    });
                });
            });
    }

    /// What the pill in the header says.
    fn status(&self) -> (&'static str, Tone) {
        match self.mode {
            Mode::Connecting => ("CONNECTING", Tone::Accent),
            Mode::Pin => ("PAIRING", Tone::Accent),
            Mode::Stream => ("STREAMING", Tone::Success),
            Mode::Lobby if self.reconnect_at.is_some() => ("RECONNECTING", Tone::Accent),
            Mode::Lobby => ("READY", Tone::Neutral),
        }
    }

    fn connect_card(&mut self, ui: &mut egui::Ui) {
        ui::titled_card(
            ui,
            "Connect with a ticket",
            Some("Paste the ticket from BroLink Host, or type an IP or Tailscale address."),
            |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.target)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("blk1_…  or  192.168.1.20  or  100.x.y.z"),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let ready = !self.target.trim().is_empty() && self.mode != Mode::Connecting;
                    let mut go = false;
                    ui.add_enabled_ui(ready, |ui| {
                        go = ui::primary_button(ui, "Connect").clicked();
                    });
                    if go {
                        self.connect();
                    }
                    if self.mode == Mode::Connecting {
                        ui.add(egui::Spinner::new().size(16.0).color(P.accent));
                        ui::muted(
                            ui,
                            self.log
                                .last()
                                .map(|l| short(l))
                                .unwrap_or_else(|| "Connecting…".into()),
                        );
                        if ui::ghost_button(ui, "Cancel").clicked() {
                            self.hangup();
                            self.mode = Mode::Lobby;
                        }
                    }
                });
                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui::notice(ui, Tone::Danger, err);
                }
            },
        );
    }

    fn pin_card(&mut self, ui: &mut egui::Ui) {
        let mut submit = false;
        ui::toned_card(ui, Tone::Accent, |ui| {
            ui::heading(
                ui,
                "Pairing PIN",
                Some(&format!(
                    "Enter the 6-digit PIN shown on “{}”. You only do this once per PC.",
                    self.host_name
                )),
            );
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.pin)
                        .char_limit(6)
                        .font(egui::FontId::monospace(26.0))
                        .desired_width(190.0)
                        .horizontal_align(egui::Align::Center)
                        .hint_text("000000"),
                );
                if self.pin_focus {
                    resp.request_focus();
                    self.pin_focus = false;
                }
                self.pin.retain(|c| c.is_ascii_digit());
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_enabled_ui(self.pin.len() == 6, |ui| {
                    if ui::primary_button(ui, "Pair").clicked() {
                        submit = true;
                    }
                });
                if ui::ghost_button(ui, "Cancel").clicked() {
                    self.pin.clear();
                    self.hangup();
                    self.mode = Mode::Lobby;
                }
            });
        });
        if submit && self.pin.len() == 6 {
            let _ = self.cmd.send(ClientCmd::Pin(self.pin.clone()));
            self.mode = Mode::Connecting;
        }
    }

    fn saved_card(&mut self, ui: &mut egui::Ui) {
        if self.cfg.saved_hosts.is_empty() && self.reconnect_at.is_none() {
            return;
        }
        let mut connect_to: Option<String> = None;
        let mut forget: Option<String> = None;
        let mut wake: Option<(String, String)> = None;
        let now = unix_now();
        ui::titled_card(
            ui,
            "Your PCs",
            Some(
                "Connect wakes a sleeping PC on its own. Leave the PC asleep rather than shut \
                 down: it comes back in seconds with everything still open.",
            ),
            |ui| {
                if let Some(at) = self.reconnect_at {
                    let left = at.saturating_duration_since(Instant::now()).as_secs();
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(P.accent));
                        ui.label(format!(
                            "Reconnecting to {} in {left}s…",
                            short(&self.target)
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui::ghost_button(ui, "Stop").clicked() {
                                self.hangup();
                            }
                        });
                    });
                    if !self.cfg.saved_hosts.is_empty() {
                        ui::row_separator(ui);
                    }
                }
                let busy = self.mode == Mode::Connecting;
                for (i, h) in self.cfg.saved_hosts.iter().enumerate() {
                    if i > 0 {
                        ui::row_separator(ui);
                    }
                    let detail = format!(
                        "{}  ·  {}",
                        short(&h.ticket),
                        ago(h.last_connected_unix, now)
                    );
                    ui::list_row(ui, &h.name, &detail, |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            if ui::primary_button(ui, "Connect")
                                .on_hover_text("Wakes the PC first if it is asleep")
                                .clicked()
                            {
                                connect_to = Some(h.ticket.clone());
                            }
                        });
                        if let Some(mac) = &h.wake_mac {
                            if ui::ghost_button(ui, "Wake")
                                .on_hover_text(format!(
                                    "Send a wake-up to {mac} without connecting"
                                ))
                                .clicked()
                            {
                                wake = Some((h.ticket.clone(), mac.clone()));
                            }
                        }
                        if ui::danger_button(ui, "Remove").clicked() {
                            forget = Some(h.ticket.clone());
                        }
                    });
                }
            },
        );
        if let Some((ticket, mac)) = wake {
            let _ = self.cmd.send(ClientCmd::Wake { ticket, mac });
        }
        if let Some(t) = forget {
            self.cfg.forget_host(&t);
            let _ = self.cfg.save();
        }
        if let Some(t) = connect_to {
            self.target = t;
            self.connect();
        }
    }

    fn discovery_card(&mut self, ui: &mut egui::Ui) {
        let mut connect_to: Option<String> = None;
        ui::titled_card(ui, "On this network", None, |ui| {
            if self.hosts.is_empty() {
                ui::empty_state(
                    ui,
                    "Looking for PCs running BroLink Host on this network…",
                    true,
                );
            }
            let busy = self.mode == Mode::Connecting;
            for (i, h) in self.hosts.iter().enumerate() {
                if i > 0 {
                    ui::row_separator(ui);
                }
                let detail = format!(
                    "{}  ·  {}  ·  v{}",
                    h.addr, h.beacon.encoder, h.beacon.version
                );
                ui::list_row(ui, &h.beacon.name, &detail, |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui::primary_button(ui, "Connect").clicked() {
                            // Prefer the ticket the beacon carries: it names the
                            // host key, so the client can verify who answered
                            // instead of trusting whatever replies from that IP.
                            connect_to = Some(h.connect_target());
                        }
                    });
                });
            }
        });
        if let Some(t) = connect_to {
            self.target = t;
            self.connect();
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut save = false;
        ui::setting_row(
            ui,
            "Quality",
            Some("Applies on the next connection. Competitive is best for games."),
            |ui| {
                let mut preset = self.cfg.quality.preset;
                let options = [
                    (QualityPreset::Competitive, "Competitive"),
                    (QualityPreset::Balanced, "Balanced"),
                    (QualityPreset::Quality, "Quality"),
                ];
                if ui::segmented(ui, &options, &mut preset) {
                    self.cfg.quality = StreamQuality::from_preset(preset);
                    save = true;
                }
            },
        );
        ui::setting_row(ui, "Volume", None, |ui| {
            let resp = ui.add(
                egui::Slider::new(&mut self.cfg.volume, 0.0..=2.0)
                    .show_value(false)
                    .trailing_fill(true),
            );
            ui::caption(ui, format!("{:.0}%", self.cfg.volume * 100.0));
            if resp.drag_stopped() {
                save = true;
            }
        });
        ui::row_separator(ui);
        if ui::toggle_row(
            ui,
            &mut self.cfg.auto_reconnect,
            "Reconnect if the session drops",
            None,
        ) {
            save = true;
        }
        if ui::toggle_row(
            ui,
            &mut self.cfg.enable_clipboard,
            "Share the clipboard",
            None,
        ) {
            save = true;
        }
        if ui::toggle_row(
            ui,
            &mut self.cfg.show_hud,
            "Show the overlay while streaming",
            Some("F7 toggles it during a session"),
        ) {
            self.show_hud = self.cfg.show_hud;
            save = true;
        }
        if save {
            let _ = self.cfg.save();
        }
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

impl ClientApp {
    fn stream_ui(&mut self, ctx: &egui::Context) {
        let (release, disconnect, toggle_fs, toggle_hud) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F8),
                i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::Q),
                i.key_pressed(egui::Key::F11),
                i.key_pressed(egui::Key::F7),
            )
        });
        if disconnect {
            self.hangup();
            self.leave_stream(ctx);
            return;
        }
        if toggle_hud {
            self.show_hud = !self.show_hud;
        }
        if release {
            let on = !self.captured;
            self.set_capture(ctx, on);
        }
        if toggle_fs {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }

        // Free the pointer whenever the window loses focus. On macOS F8 is a
        // media key the app never receives, so without this a captured cursor is
        // almost impossible to release; switching away (Cmd-Tab, Mission
        // Control) is the one gesture that always works, and it drops focus.
        if self.captured && !ctx.input(|i| i.focused) {
            self.set_capture(ctx, false);
        }

        let mut video_rect = egui::Rect::NOTHING;
        let mut clicked_video = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                let avail = ui.available_size();
                match &self.video_tex {
                    Some(tex) => {
                        let (vw, vh) = (self.video_size[0] as f32, self.video_size[1] as f32);
                        // Letterbox: never stretch, so aiming stays 1:1.
                        let scale = (avail.x / vw).min(avail.y / vh).max(0.01);
                        let size = egui::vec2(vw * scale, vh * scale);
                        ui.centered_and_justified(|ui| {
                            let resp = ui.add(
                                egui::Image::new((tex.id(), size)).sense(egui::Sense::click()),
                            );
                            video_rect = resp.rect;
                            clicked_video = resp.clicked();
                        });
                    }
                    None => {
                        ui.vertical_centered(|ui| {
                            ui.add_space((avail.y / 2.0 - 24.0).max(0.0));
                            ui.add(egui::Spinner::new().size(18.0).color(P.muted));
                            ui::muted(ui, "Waiting for video…");
                        });
                    }
                }
            });
        if clicked_video && !self.captured {
            self.set_capture(ctx, true);
        }

        if self.show_hud {
            self.hud(ctx);
            if self.mode == Mode::Lobby {
                // The HUD's Disconnect fired.
                return;
            }
        }

        if self.cfg.enable_clipboard && self.last_clip.elapsed() >= Duration::from_millis(400) {
            self.last_clip = Instant::now();
            if let Some(msg) = self.clipboard.poll_outgoing() {
                let _ = self.cmd.send(ClientCmd::Control(msg));
            }
        }

        // Collect input every frame — even uncaptured, so the gamepad works
        // while the pointer is free, and so anything still held gets released.
        let captured = self.captured;
        let events = ctx.input(|i| self.input.collect(i, captured, video_rect));
        if !events.is_empty() {
            let _ = self.cmd.send(ClientCmd::Input(events));
        }
    }

    /// The overlay in the top-left corner of the stream.
    fn hud(&mut self, ctx: &egui::Context) {
        let mut power: Option<PowerAction> = None;
        let mut confirm = false;
        let mut leave = false;
        let white = |a: u8| egui::Color32::from_white_alpha(a);
        egui::Area::new(egui::Id::new("hud"))
            .fixed_pos(egui::pos2(14.0, 12.0))
            .show(ctx, |ui| {
                ui::overlay_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.label(egui::RichText::new("●").size(9.0).color(P.success));
                        ui.label(
                            egui::RichText::new(&self.host_name)
                                .font(brolink_ui::theme::medium(13.0))
                                .color(white(235)),
                        );
                        hud_divider(ui);
                        let hint = if self.captured {
                            format!("Mouse captured  ·  {RELEASE_KEY} or switch windows to release")
                        } else {
                            "Click the picture to control the PC".to_string()
                        };
                        ui.label(egui::RichText::new(hint).size(12.5).color(white(200)));
                        if !self.stats.is_empty() {
                            hud_divider(ui);
                            ui.label(
                                egui::RichText::new(&self.stats)
                                    .size(12.0)
                                    .color(white(140)),
                            );
                        }
                        if !self.captured {
                            hud_divider(ui);
                            if self.power_control {
                                ui::menu_button(ui, "PC", |ui| {
                                    if ui.button("Sleep").clicked() {
                                        power = Some(PowerAction::Sleep);
                                        ui.close_menu();
                                    }
                                    if ui.button("Restart…").clicked() {
                                        power = Some(PowerAction::Restart);
                                        ui.close_menu();
                                    }
                                    if ui.button("Shut down…").clicked() {
                                        power = Some(PowerAction::Shutdown);
                                        ui.close_menu();
                                    }
                                });
                            }
                            if ui::ghost_button(ui, "Disconnect").clicked() {
                                leave = true;
                            }
                        }
                    });
                });
                if let Some(p) = self.pending_power {
                    ui.add_space(6.0);
                    ui::overlay_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.label(egui::RichText::new("●").size(9.0).color(P.danger));
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} the PC? Unsaved work on it will be lost.",
                                    capitalize(p.as_str())
                                ))
                                .color(white(235)),
                            );
                            if ui::toned_button(ui, &capitalize(p.as_str()), Tone::Danger).clicked()
                            {
                                confirm = true;
                            }
                            if ui::ghost_button(ui, "Cancel").clicked() {
                                self.pending_power = None;
                            }
                        });
                    });
                }
            });
        match power {
            // Sleep is safe to do at once: everything is still there on wake.
            Some(PowerAction::Sleep) => self.send_power(PowerAction::Sleep),
            Some(p) => self.pending_power = Some(p),
            None => {}
        }
        if confirm {
            if let Some(p) = self.pending_power {
                self.send_power(p);
            }
        }
        if leave {
            self.hangup();
            self.leave_stream(ctx);
        }
    }
}

fn hud_divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        brolink_ui::theme::stroke(1.0, egui::Color32::from_white_alpha(40)),
    );
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Trim a pasted ticket down to something that fits in a log line.
fn short(target: &str) -> String {
    let t = target.trim();
    // Count characters, not bytes: slicing a ticket mid-codepoint panics.
    match t.char_indices().nth(32) {
        Some((cut, _)) => format!("{}…", &t[..cut]),
        None => t.to_string(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "last used 3 h ago", for a saved PC. Coarse on purpose; it is a memory
/// jogger, not a log.
fn ago(then_unix: u64, now_unix: u64) -> String {
    if then_unix == 0 {
        return "not connected yet".into();
    }
    let d = now_unix.saturating_sub(then_unix);
    let s = if d < 60 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{} min ago", d / 60)
    } else if d < 86_400 {
        format!("{} h ago", d / 3600)
    } else {
        format!("{} d ago", d / 86_400)
    };
    format!("last used {s}")
}

/// Listen for host beacons on the LAN.
pub fn spawn_discovery(hosts: Arc<parking_lot::Mutex<Vec<DiscoveredHost>>>) {
    std::thread::Builder::new()
        .name("brolink-discovery".into())
        .spawn(move || {
            // SO_REUSEADDR matters here: the host's own beacon listener may
            // already hold this port on the same machine, and without it
            // discovery silently falls back to a port nobody broadcasts to.
            let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT));
            let sock = match bind_udp_blocking_reuse(addr) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("LAN discovery unavailable: {e:#}");
                    return;
                }
            };
            let _ = sock.set_broadcast(true);
            let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
            join_multicast(&sock);
            let mut buf = [0u8; 2048];
            loop {
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    if let Some(b) = decode_beacon(&buf[..n]) {
                        upsert(&mut hosts.lock(), b, from);
                    }
                }
                prune(&mut hosts.lock(), brolink_core::discovery::HOST_TIMEOUT);
            }
        })
        .expect("spawn discovery thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_tickets_are_shortened_for_the_log() {
        assert_eq!(short("  192.168.1.5  "), "192.168.1.5");
        let long = "blk1_".to_string() + &"a".repeat(200);
        let s = short(&long);
        assert_eq!(s.chars().count(), 33);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn shortening_never_splits_a_character() {
        // 32 bytes lands mid-character for multi-byte input; slicing there
        // would panic, so anything short enough is returned whole.
        let s = short(&"é".repeat(20));
        assert!(!s.is_empty());
    }

    #[test]
    fn last_used_is_coarse_and_never_negative() {
        assert_eq!(ago(0, 1_000), "not connected yet");
        assert_eq!(ago(1_000, 1_030), "last used just now");
        assert_eq!(ago(1_000, 1_000 + 5 * 60), "last used 5 min ago");
        assert_eq!(ago(1_000, 1_000 + 3 * 3600), "last used 3 h ago");
        assert_eq!(ago(1_000, 1_000 + 9 * 86_400), "last used 9 d ago");
        // A clock that went backwards reads as "just now", not a panic.
        assert_eq!(ago(5_000, 1_000), "last used just now");
    }
}

/// Render the screens to PNGs without a host, a display, or a Windows PC.
///
/// ```text
/// cargo test -p brolink-client snapshots -- --ignored --nocapture
/// ```
///
/// Output lands in `target/ui-snapshots/`. Ignored by default: it needs a GPU
/// (wgpu picks Metal, Vulkan or DX12) and is a review aid, not a test.
#[cfg(test)]
mod snapshots {
    use super::*;
    use brolink_core::config::SavedHost;
    use brolink_core::discovery::Beacon;

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

    fn sample_cfg() -> ClientConfig {
        let now = unix_now();
        ClientConfig {
            name: "Example Mac".into(),
            saved_hosts: vec![
                SavedHost {
                    name: "GAMING-PC".into(),
                    ticket: "blk1_eyJ2IjoxLCJuIjoiR0FNSU5HLVBDIiwiayI6IjM0YjA".into(),
                    last_connected_unix: now - 2 * 3600,
                    wake_mac: Some("02:00:00:00:00:02".into()),
                },
                SavedHost {
                    name: "OFFICE".into(),
                    ticket: "blk1_eyJ2IjoxLCJuIjoiT0ZGSUNFIiwiayI6IjA4ZjEwYz".into(),
                    last_connected_unix: now - 6 * 86_400,
                    wake_mac: None,
                },
            ],
            ..ClientConfig::default()
        }
    }

    fn build(
        size: egui::Vec2,
        setup: impl FnOnce(&egui::Context, &mut ClientApp),
    ) -> egui_kittest::Harness<'static, ClientApp> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (_ev_tx, ev_rx) = mpsc::unbounded_channel();
        // Keep the command receiver alive so sends do not fail noisily.
        std::mem::forget(cmd_rx);
        let video = Arc::new(VideoSink::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_size(size)
            .with_pixels_per_point(2.0)
            .with_max_steps(8)
            .build_eframe(|cc| {
                let mut app =
                    ClientApp::new(cc, sample_cfg(), Identity::generate(), cmd_tx, ev_rx, video);
                setup(&cc.egui_ctx, &mut app);
                app
            });
        harness.run_steps(3);
        harness
    }

    fn discovered(name: &str, addr: &str) -> DiscoveredHost {
        DiscoveredHost {
            beacon: Beacon {
                name: name.into(),
                host_id: "3f9a1c".into(),
                port: DEFAULT_PORT,
                version: env!("CARGO_PKG_VERSION").into(),
                encoder: "h264_nvenc".into(),
                ticket: String::new(),
            },
            addr: addr.parse().unwrap(),
            last_seen: Instant::now(),
        }
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn lobby() {
        let mut h = build(egui::vec2(1100.0, 720.0), |_, app| {
            app.discovered
                .lock()
                .push(discovered("GAMING-PC", "192.168.1.20:47850"));
        });
        save(h.render().unwrap(), "client-lobby.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn lobby_pairing_and_error() {
        let mut h = build(egui::vec2(1100.0, 720.0), |_, app| {
            app.mode = Mode::Pin;
            app.host_name = "GAMING-PC".into();
            app.pin = "42".into();
            app.error = Some("Timed out waiting for the host to answer.".into());
        });
        save(h.render().unwrap(), "client-lobby-pin.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn lobby_empty() {
        let mut h = build(egui::vec2(1100.0, 720.0), |_, app| {
            app.cfg.saved_hosts.clear();
        });
        save(h.render().unwrap(), "client-lobby-empty.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn lobby_settings_open() {
        let mut h = build(egui::vec2(1100.0, 1200.0), |ctx, app| {
            app.cfg.saved_hosts.truncate(1);
            brolink_ui::set_collapsible_open(ctx, "settings", true);
        });
        save(h.render().unwrap(), "client-lobby-settings.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn stream() {
        let mut h = build(egui::vec2(1100.0, 720.0), |ctx, app| {
            let (w, hgt) = (1920usize, 1080usize);
            let mut px = vec![0u8; w * hgt * 4];
            for y in 0..hgt {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    px[i] = (x * 255 / w) as u8 / 2 + 20;
                    px[i + 1] = (y * 255 / hgt) as u8 / 2 + 30;
                    px[i + 2] = 70;
                    px[i + 3] = 255;
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([w, hgt], &px);
            app.video_tex = Some(ctx.load_texture("fake", img, egui::TextureOptions::LINEAR));
            app.video_size = [w, hgt];
            app.mode = Mode::Stream;
            app.host_name = "GAMING-PC".into();
            app.encoder = "h264_nvenc".into();
            app.power_control = true;
            app.stats =
                "60 fps  ·  24.8 Mbps  ·  11 ms rtt  ·  dec 1.3 ms  ·  loss 0.0%  ·  h264_nvenc"
                    .into();
        });
        save(h.render().unwrap(), "client-stream.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn stream_confirm_power() {
        let mut h = build(egui::vec2(1100.0, 720.0), |_, app| {
            app.mode = Mode::Stream;
            app.host_name = "GAMING-PC".into();
            app.power_control = true;
            app.pending_power = Some(PowerAction::Restart);
            app.stats = "60 fps  ·  24.8 Mbps  ·  11 ms rtt".into();
        });
        save(h.render().unwrap(), "client-stream-confirm.png");
    }
}
