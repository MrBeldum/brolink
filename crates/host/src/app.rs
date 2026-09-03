//! Host control panel.

use crate::engine::{Engine, HostStatus};
use crate::windows_setup;
use brolink_core::config::{
    HostConfig, QualityPreset, StreamQuality, MAX_BITRATE_KBPS, MAX_FPS, MIN_BITRATE_KBPS, MIN_FPS,
};
use eframe::egui;
use std::sync::Arc;
use std::time::Duration;

pub struct HostApp {
    engine: Arc<Engine>,
    cfg: HostConfig,
    copied_until: Option<std::time::Instant>,
    /// Set when a setting changes; saved once at the end of the frame so a
    /// slider drag does not write the config file on every pixel.
    dirty: bool,
}

impl HostApp {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: Arc<Engine>, cfg: HostConfig) -> Self {
        apply_theme(&cc.egui_ctx);
        Self {
            engine,
            cfg,
            copied_until: None,
            dirty: false,
        }
    }

    fn commit(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Err(e) = self.cfg.save() {
            tracing::warn!("could not save host config: {e:#}");
        }
        self.engine.update_config(self.cfg.clone());
    }
}

impl eframe::App for HostApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(200));
        let status = self.engine.status.lock().clone();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.heading("BroLink Host");
                ui.label(egui::RichText::new("  Your PC, from your Mac").weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    status_pill(ui, &status);
                });
            });
            ui.add_space(8.0);
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak(format!(
                    "v{}  ·  UDP {}  ·  encoder {}",
                    env!("CARGO_PKG_VERSION"),
                    self.cfg.port,
                    status.encoder
                ));
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(8.0);
                if !status.ffmpeg_ok {
                    ui.add_space(8.0);
                    card(ui, "Encoder setup", |ui| {
                        ui.label(
                            "FFmpeg is required to capture this desktop. The host downloads it \
                             automatically the first time; leave this window open until the log \
                             says it is ready. You can also drop ffmpeg.exe next to the app.",
                        );
                    });
                }
                self.ticket_card(ui, &status);
                ui.add_space(12.0);
                self.internet_card(ui, &status);
                if let Some(pin) = status.pending_pin.clone() {
                    ui.add_space(12.0);
                    card(ui, "Pairing PIN", |ui| {
                        ui.label(format!(
                            "Device '{}' wants to connect. Enter this PIN on the client:",
                            status.pending_name.clone().unwrap_or_else(|| "Mac".into())
                        ));
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(&pin).size(36.0).strong().color(GOLD));
                    });
                }
                ui.add_space(12.0);
                self.session_card(ui, &status);
                ui.add_space(12.0);
                self.paired_card(ui, &status);
                ui.add_space(12.0);
                self.quality_card(ui, &status);
                ui.add_space(12.0);
                card(ui, "Log", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &status.log {
                                ui.monospace(line);
                            }
                        });
                });
                ui.add_space(16.0);
            });
        });

        self.commit();
    }
}

impl HostApp {
    fn ticket_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        card(ui, "Connect from your Mac", |ui| {
            ui.label(
                "On your Mac, paste this ticket and click Connect. It carries every \
                 address this PC can be reached on.",
            );
            ui.add_space(8.0);
            let mut ticket = if status.ticket_display.is_empty() {
                "starting…".to_string()
            } else {
                status.ticket_display.clone()
            };
            // Read-only: the field exists so the ticket can be selected and
            // copied, not edited.
            ui.add(
                egui::TextEdit::multiline(&mut ticket)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .interactive(false),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ready = !status.ticket_display.is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new("Copy ticket"))
                    .clicked()
                {
                    ui.ctx().copy_text(status.ticket_display.clone());
                    self.copied_until = Some(std::time::Instant::now() + Duration::from_secs(2));
                }
                if self
                    .copied_until
                    .is_some_and(|t| t > std::time::Instant::now())
                {
                    ui.strong("Copied");
                }
            });
            ui.add_space(8.0);
            match status.lan {
                Some(lan) => kv(ui, "LAN", lan.to_string()),
                None => kv(ui, "LAN", "no local address found".into()),
            }
            match status.wan {
                Some(wan) => kv(ui, "WAN (STUN)", wan.to_string()),
                None => kv(
                    ui,
                    "WAN (STUN)",
                    "not available — use Tailscale, a relay, or port-forward UDP".into(),
                ),
            }
            if let Some(ts) = status.tailscale {
                kv(ui, "Tailscale", ts.to_string());
            }
            if let Some(relay) = status.relay {
                kv(ui, "Relay", relay.to_string());
            }
            if let Some(upnp) = &status.upnp {
                kv(ui, "Port mapping", upnp.clone());
            }
        });
    }

    fn internet_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        card(ui, "Internet access", |ui| {
            ui.label(&status.internet);
            ui.add_space(6.0);
            if ui
                .checkbox(
                    &mut self.cfg.enable_upnp,
                    "Ask my router to open the port (UPnP / NAT-PMP)",
                )
                .changed()
            {
                self.dirty = true;
            }
            ui.weak(
                "If this stays on “local network only”, add a relay below or install Tailscale \
                 on both machines. CGNAT (many mobile ISPs) cannot be mapped.",
            );
        });
    }

    fn paired_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        if status.paired.is_empty() {
            return;
        }
        let mut revoke = None;
        card(ui, "Paired Macs", |ui| {
            for (name, hex) in &status.paired {
                ui.horizontal(|ui| {
                    ui.strong(name);
                    ui.weak(&hex[..hex.len().min(12)]);
                    if ui.small_button("Revoke").clicked() {
                        revoke = Some(hex.clone());
                    }
                });
            }
        });
        if let Some(hex) = revoke {
            self.engine.revoke_client(hex);
        }
    }

    fn session_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        card(ui, "Session", |ui| {
            match &status.client {
                Some(c) => {
                    kv(ui, "Client", c.clone());
                    kv(
                        ui,
                        "Streaming",
                        if status.streaming { "yes" } else { "starting" }.into(),
                    );
                    kv(ui, "Frames sent", status.frames_sent.to_string());
                    kv(ui, "Rate", format!("{:.0} fps", status.fps));
                    kv(
                        ui,
                        "Bitrate",
                        format!("{:.1} Mbps", status.bitrate_kbps / 1000.0),
                    );
                }
                None => {
                    ui.label("Waiting for a Mac… keep this window open (or Start with Windows).");
                }
            }
            if !status.path.is_empty() {
                kv(ui, "Path", status.path.clone());
            }
            if status.client.is_some() && ui.button("Disconnect Mac").clicked() {
                self.engine.kick_client();
            }
            if let Some(err) = &status.last_error {
                ui.colored_label(egui::Color32::from_rgb(248, 81, 73), err);
            }
        });
    }

    fn quality_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        card(ui, "Quality", |ui| {
            ui.horizontal(|ui| {
                for p in QualityPreset::all() {
                    if p == QualityPreset::Custom {
                        continue;
                    }
                    let selected = self.cfg.quality.preset == p;
                    if ui.selectable_label(selected, p.as_str()).clicked() {
                        self.cfg.quality = StreamQuality::from_preset(p);
                        self.dirty = true;
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Bitrate");
                let mut br = self.cfg.quality.bitrate_kbps as f32;
                if ui
                    .add(
                        egui::Slider::new(
                            &mut br,
                            MIN_BITRATE_KBPS as f32..=MAX_BITRATE_KBPS as f32,
                        )
                        .suffix(" kbps"),
                    )
                    .changed()
                {
                    self.cfg.quality.preset = QualityPreset::Custom;
                    self.cfg.quality.bitrate_kbps = br as u32;
                    self.dirty = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("FPS");
                let mut fps = self.cfg.quality.fps as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut fps, MIN_FPS as f32..=MAX_FPS as f32).suffix(" fps"),
                    )
                    .changed()
                {
                    self.cfg.quality.preset = QualityPreset::Custom;
                    self.cfg.quality.fps = fps as u32;
                    self.dirty = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Monitor index");
                if ui
                    .add(egui::DragValue::new(&mut self.cfg.monitor_index).range(0..=7))
                    .changed()
                {
                    self.dirty = true;
                }
            });
            // Every one of these used to change only the in-memory copy, so a
            // restart silently reverted them.
            if ui
                .checkbox(&mut self.cfg.enable_audio, "Capture system audio")
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .checkbox(
                    &mut self.cfg.enable_gamepad,
                    "Virtual Xbox 360 gamepad (needs ViGEmBus)",
                )
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .checkbox(&mut self.cfg.enable_clipboard, "Share the clipboard")
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .checkbox(
                    &mut self.cfg.adaptive_bitrate,
                    "Adapt bitrate when the Mac reports loss",
                )
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .checkbox(
                    &mut self.cfg.start_with_windows,
                    "Start with Windows (this user)",
                )
                .changed()
            {
                self.dirty = true;
                if let Ok(exe) = std::env::current_exe() {
                    if let Err(e) =
                        windows_setup::set_start_with_windows(self.cfg.start_with_windows, &exe)
                    {
                        tracing::warn!("start with Windows: {e:#}");
                    }
                }
            }
            if ui
                .checkbox(
                    &mut self.cfg.auto_trust,
                    "Skip the PIN for new clients (LAN only — anyone who can reach this PC can connect)",
                )
                .changed()
            {
                self.dirty = true;
            }
            ui.horizontal(|ui| {
                ui.label("PC name");
                if ui.text_edit_singleline(&mut self.cfg.name).lost_focus() {
                    self.dirty = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Relay");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.relay)
                        .hint_text("relay.example.com:47851 (optional)"),
                );
                if resp.lost_focus() {
                    self.dirty = true;
                }
            });
            ui.add_space(4.0);
            if status.client.is_some() {
                ui.weak("Quality and relay changes apply the next time a client connects.");
            } else {
                ui.weak("Applied when a client connects.");
            }
        });
    }
}

const GOLD: egui::Color32 = egui::Color32::from_rgb(245, 165, 36);
const BG: egui::Color32 = egui::Color32::from_rgb(14, 17, 22);
const CARD: egui::Color32 = egui::Color32::from_rgb(22, 27, 34);
const TEXT: egui::Color32 = egui::Color32::from_rgb(230, 237, 243);

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.override_text_color = Some(TEXT);
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

fn kv(ui: &mut egui::Ui, k: &str, v: String) {
    ui.horizontal(|ui| {
        ui.weak(k);
        ui.label(v);
    });
}

fn status_pill(ui: &mut egui::Ui, st: &HostStatus) {
    let (label, color) = if st.streaming {
        ("STREAMING", egui::Color32::from_rgb(63, 185, 80))
    } else if !st.ffmpeg_ok {
        ("NEEDS SETUP", egui::Color32::from_rgb(248, 81, 73))
    } else if st.running {
        ("READY", GOLD)
    } else {
        ("STARTING", egui::Color32::GRAY)
    };
    ui.colored_label(color, "●");
    ui.strong(label);
}
