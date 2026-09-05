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
use eframe::egui;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const GOLD: egui::Color32 = egui::Color32::from_rgb(245, 165, 36);
const BG: egui::Color32 = egui::Color32::from_rgb(10, 12, 16);
const CARD: egui::Color32 = egui::Color32::from_rgb(22, 27, 34);
const RED: egui::Color32 = egui::Color32::from_rgb(248, 81, 73);

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
        apply_theme(&cc.egui_ctx);
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

impl ClientApp {
    fn lobby_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.heading("BroLink");
                ui.label(egui::RichText::new("  Play your Windows PC").weak());
            });
            ui.add_space(8.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(8.0);
                self.saved_card(ui);
                ui.add_space(12.0);
                self.connect_card(ui);
                if self.mode == Mode::Pin {
                    ui.add_space(12.0);
                    self.pin_card(ui);
                }
                ui.add_space(12.0);
                self.discovery_card(ui);
                ui.add_space(12.0);
                self.quality_card(ui);
                ui.add_space(12.0);
                card(ui, "Log", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .stick_to_bottom(true)
                        .id_salt("log")
                        .show(ui, |ui| {
                            for line in &self.log {
                                ui.monospace(line);
                            }
                        });
                });
                ui.add_space(16.0);
            });
        });
    }

    fn connect_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Have a ticket?", |ui| {
            ui.label("Paste the ticket from BroLink Host, or type an IP / Tailscale address.");
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::multiline(&mut self.target)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3)
                    .hint_text("blk1_… or 192.168.1.20 or 100.x.y.z"),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ready = !self.target.trim().is_empty() && self.mode != Mode::Connecting;
                if ui
                    .add_enabled(ready, egui::Button::new("Connect"))
                    .clicked()
                {
                    self.connect();
                }
                if self.mode == Mode::Connecting {
                    ui.spinner();
                    ui.label(
                        self.log
                            .last()
                            .map(|l| short(l))
                            .unwrap_or_else(|| "Connecting…".into()),
                    );
                    if ui.button("Cancel").clicked() {
                        self.hangup();
                        self.mode = Mode::Lobby;
                    }
                }
            });
            if let Some(err) = &self.error {
                ui.add_space(4.0);
                ui.colored_label(RED, err);
            }
        });
    }

    fn pin_card(&mut self, ui: &mut egui::Ui) {
        let mut submit = false;
        card(ui, "Pairing PIN", |ui| {
            ui.label(format!(
                "Enter the 6-digit PIN shown on '{}':",
                self.host_name
            ));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.pin)
                    .char_limit(6)
                    .hint_text("000000"),
            );
            self.pin.retain(|c| c.is_ascii_digit());
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if ui
                .add_enabled(self.pin.len() == 6, egui::Button::new("Submit PIN"))
                .clicked()
            {
                submit = true;
            }
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
        card(ui, "Your PCs", |ui| {
            if let Some(at) = self.reconnect_at {
                let left = at.saturating_duration_since(Instant::now()).as_secs();
                ui.label(format!(
                    "Reconnecting to {} in {left}s…",
                    short(&self.target)
                ));
                if ui.button("Stop reconnecting").clicked() {
                    self.hangup();
                }
            }
            for h in &self.cfg.saved_hosts {
                ui.horizontal(|ui| {
                    ui.strong(&h.name);
                    ui.weak(short(&h.ticket));
                    if ui
                        .add_enabled(self.mode != Mode::Connecting, egui::Button::new("Connect"))
                        .on_hover_text("Wakes the PC first if it is asleep")
                        .clicked()
                    {
                        connect_to = Some(h.ticket.clone());
                    }
                    if let Some(mac) = &h.wake_mac {
                        if ui
                            .small_button("Wake")
                            .on_hover_text(format!("Send a wake-up to {mac} without connecting"))
                            .clicked()
                        {
                            wake = Some((h.ticket.clone(), mac.clone()));
                        }
                    }
                    if ui.small_button("Remove").clicked() {
                        forget = Some(h.ticket.clone());
                    }
                });
            }
            ui.weak(
                "Connect wakes a sleeping PC on its own. Leave the PC asleep rather than \
                 shut down: it comes back in seconds and the session is still there.",
            );
        });
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
        card(ui, "PCs on this network", |ui| {
            if self.hosts.is_empty() {
                ui.weak(
                    "No BroLink hosts announcing yet. Make sure the Windows host is running \
                     and on the same network.",
                );
            }
            for h in &self.hosts {
                ui.horizontal(|ui| {
                    ui.strong(&h.beacon.name);
                    ui.weak(h.addr.to_string());
                    if ui.button("Connect").clicked() {
                        // Prefer the ticket the beacon carries: it names the
                        // host key, so the client can verify who answered
                        // instead of trusting whatever replies from that IP.
                        connect_to = Some(h.connect_target());
                    }
                });
            }
        });
        if let Some(t) = connect_to {
            self.target = t;
            self.connect();
        }
    }

    fn quality_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Quality request", |ui| {
            ui.horizontal(|ui| {
                for p in QualityPreset::all() {
                    if p == QualityPreset::Custom {
                        continue;
                    }
                    let sel = self.cfg.quality.preset == p;
                    if ui.selectable_label(sel, p.as_str()).clicked() {
                        self.cfg.quality = StreamQuality::from_preset(p);
                        let _ = self.cfg.save();
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Volume");
                if ui
                    .add(egui::Slider::new(&mut self.cfg.volume, 0.0..=2.0))
                    .drag_stopped()
                {
                    let _ = self.cfg.save();
                }
            });
            if ui
                .checkbox(
                    &mut self.cfg.auto_reconnect,
                    "Reconnect if the session drops",
                )
                .changed()
            {
                let _ = self.cfg.save();
            }
            if ui
                .checkbox(&mut self.cfg.enable_clipboard, "Share the clipboard")
                .changed()
            {
                let _ = self.cfg.save();
            }
            if ui
                .checkbox(&mut self.cfg.show_hud, "Show stream HUD")
                .changed()
            {
                self.show_hud = self.cfg.show_hud;
                let _ = self.cfg.save();
            }
            ui.weak("Quality applies on the next connection. Competitive is best for games.");
        });
    }

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
                        ui.centered_and_justified(|ui| {
                            ui.label("Waiting for video…");
                        });
                    }
                }
            });
        if clicked_video && !self.captured {
            self.set_capture(ctx, true);
        }

        if self.show_hud {
            let mut power: Option<PowerAction> = None;
            let mut confirm = false;
            let mut cancel = false;
            egui::Area::new(egui::Id::new("hud"))
                .fixed_pos(egui::pos2(16.0, 12.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let hud = if self.captured {
                            format!(
                                "F8 release  ·  F11 fullscreen  ·  F7 HUD  ·  Ctrl+Shift+Q quit  ·  {}",
                                self.stats
                            )
                        } else {
                            format!(
                                "Click to capture mouse  ·  F8 toggle  ·  F7 HUD  ·  {}",
                                self.stats
                            )
                        };
                        ui.label(
                            egui::RichText::new(hud)
                                .size(13.0)
                                .color(egui::Color32::from_white_alpha(220)),
                        );
                        if self.power_control && !self.captured {
                            ui.menu_button("PC ▾", |ui| {
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
                                if ui.button("Disconnect").clicked() {
                                    power = None;
                                    cancel = true;
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                    if let Some(p) = self.pending_power {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                RED,
                                format!(
                                    "{} the PC? Unsaved work on it will be lost.",
                                    capitalize(p.as_str())
                                ),
                            );
                            if ui.button(capitalize(p.as_str())).clicked() {
                                confirm = true;
                            }
                            if ui.button("Cancel").clicked() {
                                self.pending_power = None;
                            }
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
            if cancel {
                self.hangup();
                self.leave_stream(ctx);
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

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(33, 38, 45);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(48, 54, 61);
    visuals.selection.bg_fill = GOLD;
    visuals.selection.stroke.color = GOLD;
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    ctx.set_style(style);
}

fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).strong().size(16.0).color(GOLD));
            ui.add_space(6.0);
            add(ui);
        });
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
}
