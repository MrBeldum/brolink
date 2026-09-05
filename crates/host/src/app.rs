//! Host control panel.

use crate::engine::{Engine, HostStatus};
use crate::windows_setup;
use brolink_core::config::{
    HostConfig, QualityPreset, StreamQuality, MAX_BITRATE_KBPS, MAX_FPS, MIN_BITRATE_KBPS, MIN_FPS,
};
use brolink_ui::{self as ui, Tone, PALETTE as P};
use eframe::egui;
use std::sync::Arc;
use std::time::Duration;

/// The window is 720 wide by default; leave a gutter either side.
const COLUMN_WIDTH: f32 = 680.0;

pub struct HostApp {
    engine: Arc<Engine>,
    cfg: HostConfig,
    copied_until: Option<std::time::Instant>,
    /// Set when a setting changes; saved once at the end of the frame so a
    /// slider drag does not write the config file on every pixel.
    dirty: bool,
    brand: ui::Brand,
}

impl HostApp {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: Arc<Engine>, cfg: HostConfig) -> Self {
        ui::apply(&cc.egui_ctx);
        Self {
            engine,
            cfg,
            copied_until: None,
            dirty: false,
            brand: ui::Brand::new(&cc.egui_ctx),
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

        ui::top_bar(ctx, "top", |ui| {
            let (label, tone) = status_of(&status);
            self.brand
                .header(ui, "BroLink Host", "Your PC, from your Mac", |ui| {
                    ui::status_pill(ui, label, tone);
                });
        });

        ui::bottom_bar(ctx, "bottom", |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.label("·");
                ui.label(format!("UDP {}", self.cfg.port));
                ui.label("·");
                ui.label(format!("encoder {}", status.encoder));
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(P.bg))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(20.0);
                    ui::content_column(ui, COLUMN_WIDTH, |ui| {
                        ui.spacing_mut().item_spacing.y = 14.0;
                        if !status.ffmpeg_ok {
                            ui::toned_card(ui, Tone::Danger, |ui| {
                                ui::heading(ui, "Encoder setup", None);
                                ui.label(
                                    "FFmpeg is required to capture this desktop. The host downloads \
                                     it automatically the first time; leave this window open until \
                                     the log says it is ready. You can also drop ffmpeg.exe next to \
                                     the app.",
                                );
                            });
                        }
                        if let Some(pin) = status.pending_pin.clone() {
                            self.pin_card(ui, &status, &pin);
                        }
                        self.ticket_card(ui, &status);
                        self.session_card(ui, &status);
                        self.internet_card(ui, &status);
                        self.wake_card(ui, &status);
                        self.paired_card(ui, &status);
                        self.settings_card(ui, &status);
                        ui::titled_card(ui, "Log", None, |ui| {
                            ui::log_view(ui, "host_log", &status.log, 200.0);
                        });
                        ui.add_space(10.0);
                    });
                });
            });

        self.commit();
    }
}

impl HostApp {
    fn pin_card(&mut self, ui: &mut egui::Ui, status: &HostStatus, pin: &str) {
        ui::toned_card(ui, Tone::Accent, |ui| {
            ui::heading(
                ui,
                "Pairing PIN",
                Some(&format!(
                    "“{}” wants to connect. Enter this PIN on the Mac; it is only asked once.",
                    status.pending_name.clone().unwrap_or_else(|| "Mac".into())
                )),
            );
            ui.vertical_centered(|ui| {
                ui::display_digits(ui, pin);
            });
        });
    }

    fn ticket_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        ui::titled_card(
            ui,
            "Connect from your Mac",
            Some(
                "On your Mac, paste this ticket and click Connect. It carries every address \
                 this PC can be reached on.",
            ),
            |ui| {
                let ready = !status.ticket_display.is_empty();
                ui::well(ui, |ui| {
                    if ready {
                        ui.label(
                            egui::RichText::new(&status.ticket_display)
                                .monospace()
                                .color(P.text),
                        );
                    } else {
                        ui::empty_state(ui, "Preparing the ticket…", true);
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(ready, |ui| {
                        if ui::primary_button(ui, "Copy ticket").clicked() {
                            ui.ctx().copy_text(status.ticket_display.clone());
                            self.copied_until =
                                Some(std::time::Instant::now() + Duration::from_secs(2));
                        }
                    });
                    if self
                        .copied_until
                        .is_some_and(|t| t > std::time::Instant::now())
                    {
                        ui::dot_label(ui, Tone::Success, "Copied");
                    }
                });
                ui.add_space(6.0);
                let mut rows: Vec<(&str, String)> = Vec::new();
                rows.push((
                    "LAN",
                    status
                        .lan
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "no local address found".into()),
                ));
                rows.push((
                    "Internet",
                    status.wan.map(|a| a.to_string()).unwrap_or_else(|| {
                        "not available — use a relay, Tailscale, or port-forward UDP".into()
                    }),
                ));
                if let Some(ts) = status.tailscale {
                    rows.push(("Tailscale", ts.to_string()));
                }
                if let Some(relay) = status.relay {
                    rows.push(("Relay", relay.to_string()));
                }
                if let Some(upnp) = &status.upnp {
                    rows.push(("Port mapping", upnp.clone()));
                }
                ui::kv_grid(ui, "addresses", &rows);
            },
        );
    }

    fn session_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        ui::titled_card(ui, "Session", None, |ui| {
            match &status.client {
                Some(c) => {
                    let mut rows: Vec<(&str, String)> = vec![
                        ("Client", c.clone()),
                        (
                            "Streaming",
                            if status.streaming { "yes" } else { "starting" }.into(),
                        ),
                        ("Frames sent", status.frames_sent.to_string()),
                        ("Rate", format!("{:.0} fps", status.fps)),
                        (
                            "Bitrate",
                            format!("{:.1} Mbps", status.bitrate_kbps / 1000.0),
                        ),
                    ];
                    if !status.path.is_empty() {
                        rows.push(("Path", status.path.clone()));
                    }
                    ui::kv_grid(ui, "session", &rows);
                    ui.add_space(6.0);
                    if ui::danger_button(ui, "Disconnect the Mac").clicked() {
                        self.engine.kick_client();
                    }
                }
                None => {
                    ui::empty_state(
                        ui,
                        "Waiting for a Mac to connect. Keep this window open, or turn on \
                         Start with Windows.",
                        status.running,
                    );
                    if !status.path.is_empty() {
                        ui::kv(ui, "Path", status.path.clone());
                    }
                }
            }
            if let Some(err) = &status.last_error {
                ui.add_space(6.0);
                ui::notice(ui, Tone::Danger, err);
            }
        });
    }

    fn internet_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        ui::titled_card(ui, "Internet access", None, |ui| {
            let reachable =
                status.upnp.is_some() || status.tailscale.is_some() || status.relay.is_some();
            let tone = if reachable {
                Tone::Success
            } else {
                Tone::Accent
            };
            ui::dot_label(ui, tone, &status.internet);
            ui.add_space(4.0);
            if ui::toggle_row(
                ui,
                &mut self.cfg.enable_upnp,
                "Ask my router to open the port",
                Some("UPnP / NAT-PMP. Most home routers allow it; CGNAT cannot be mapped."),
            ) {
                self.dirty = true;
            }
            ui::caption(
                ui,
                "If this stays on the local network only, set a relay in Settings below or \
                 install Tailscale on both machines. Neither needs any router changes.",
            );
        });
    }

    fn wake_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        ui::titled_card(ui, "Wake and power from the Mac", None, |ui| {
            match &status.wake {
                None => {
                    ui::empty_state(ui, "Checking whether this PC can be woken remotely…", true);
                }
                Some(w) => {
                    ui::kv_grid(
                        ui,
                        "wake",
                        &[
                            ("Adapter", w.adapter.clone()),
                            ("MAC", w.mac.clone().unwrap_or_else(|| "not found".into())),
                        ],
                    );
                    ui.add_space(4.0);
                    match w.magic_packet {
                        Some(true) => ui::dot_label(
                            ui,
                            Tone::Success,
                            "Wake-on-LAN is on: the Mac can wake this PC from sleep.",
                        ),
                        Some(false) => {
                            ui::dot_label(
                                ui,
                                Tone::Danger,
                                "Wake-on-LAN is off, so a sleeping PC cannot be woken from the Mac.",
                            );
                            if ui::primary_button(ui, "Enable Wake-on-LAN")
                                .on_hover_text("Asks for administrator approval")
                                .clicked()
                            {
                                self.engine.enable_wake();
                            }
                        }
                        None => {
                            ui::caption(ui, "Could not read the adapter's wake settings.");
                        }
                    }
                    if w.fast_startup == Some(true) {
                        ui::caption(
                            ui,
                            "Fast Startup is on, so waking after a full shut down is unreliable. \
                             Use Sleep from the Mac; it wakes in seconds with everything still open.",
                        );
                    }
                }
            }
            ui.add_space(4.0);
            if ui::toggle_row(
                ui,
                &mut self.cfg.allow_power_control,
                "Let a paired Mac sleep, restart, or shut down this PC",
                None,
            ) {
                self.dirty = true;
            }
            ui::caption(
                ui,
                "Turn on Start with Windows in Settings so the host is waiting after a restart. \
                 A sleeping PC keeps the host running and resumes on its own.",
            );
        });
    }

    fn paired_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        if status.paired.is_empty() {
            return;
        }
        let mut revoke = None;
        ui::titled_card(ui, "Paired Macs", None, |ui| {
            for (i, (name, hex)) in status.paired.iter().enumerate() {
                if i > 0 {
                    ui::row_separator(ui);
                }
                ui::list_row(ui, name, &hex[..hex.len().min(12)], |ui| {
                    if ui::danger_button(ui, "Revoke").clicked() {
                        revoke = Some(hex.clone());
                    }
                });
            }
        });
        if let Some(hex) = revoke {
            self.engine.revoke_client(hex);
        }
    }

    fn settings_card(&mut self, ui: &mut egui::Ui, status: &HostStatus) {
        ui::titled_card(
            ui,
            "Settings",
            Some("The Mac picks a preset; the quality here is the most this PC will encode."),
            |ui| {
                ui::setting_row(ui, "Maximum quality", None, |ui| {
                    let mut preset = self.cfg.quality.preset;
                    let options = [
                        (QualityPreset::Competitive, "Competitive"),
                        (QualityPreset::Balanced, "Balanced"),
                        (QualityPreset::Quality, "Quality"),
                    ];
                    if ui::segmented(ui, &options, &mut preset) {
                        self.cfg.quality = StreamQuality::from_preset(preset);
                        self.dirty = true;
                    }
                });
                ui::setting_row(ui, "Bitrate", None, |ui| {
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
                ui::setting_row(ui, "Frame rate", None, |ui| {
                    let mut fps = self.cfg.quality.fps as f32;
                    if ui
                        .add(
                            egui::Slider::new(&mut fps, MIN_FPS as f32..=MAX_FPS as f32)
                                .suffix(" fps"),
                        )
                        .changed()
                    {
                        self.cfg.quality.preset = QualityPreset::Custom;
                        self.cfg.quality.fps = fps as u32;
                        self.dirty = true;
                    }
                });
                ui::setting_row(ui, "Monitor", Some("0 is the primary display"), |ui| {
                    if ui
                        .add(egui::DragValue::new(&mut self.cfg.monitor_index).range(0..=7))
                        .changed()
                    {
                        self.dirty = true;
                    }
                });
                ui::row_separator(ui);
                // Every one of these used to change only the in-memory copy, so
                // a restart silently reverted them; `dirty` saves at frame end.
                if ui::toggle_row(ui, &mut self.cfg.enable_audio, "Capture system audio", None) {
                    self.dirty = true;
                }
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.enable_gamepad,
                    "Virtual Xbox 360 gamepad",
                    Some("Needs the ViGEmBus driver"),
                ) {
                    self.dirty = true;
                }
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.enable_clipboard,
                    "Share the clipboard",
                    None,
                ) {
                    self.dirty = true;
                }
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.adaptive_bitrate,
                    "Adapt bitrate when the Mac reports loss",
                    None,
                ) {
                    self.dirty = true;
                }
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.start_with_windows,
                    "Start with Windows",
                    Some("For this user account"),
                ) {
                    self.dirty = true;
                    if let Ok(exe) = std::env::current_exe() {
                        if let Err(e) =
                            windows_setup::set_start_with_windows(self.cfg.start_with_windows, &exe)
                        {
                            tracing::warn!("start with Windows: {e:#}");
                        }
                    }
                }
                if ui::toggle_row(
                    ui,
                    &mut self.cfg.auto_trust,
                    "Skip the PIN for new clients",
                    Some("Local network only: anyone who can reach this PC can connect"),
                ) {
                    self.dirty = true;
                }
                ui::row_separator(ui);
                ui::setting_row(ui, "PC name", None, |ui| {
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.cfg.name).desired_width(240.0))
                        .lost_focus()
                    {
                        self.dirty = true;
                    }
                });
                ui::setting_row(
                    ui,
                    "Relay",
                    Some("Optional. Lets the Mac connect from anywhere with no port forwarding."),
                    |ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.relay)
                                    .desired_width(240.0)
                                    .hint_text("relay.example.com:47851"),
                            )
                            .lost_focus()
                        {
                            self.dirty = true;
                        }
                    },
                );
                ui.add_space(2.0);
                ui::caption(
                    ui,
                    if status.client.is_some() {
                        "Quality and relay changes apply the next time a Mac connects."
                    } else {
                        "Applied when a Mac connects."
                    },
                );
            },
        );
    }
}

fn status_of(st: &HostStatus) -> (&'static str, Tone) {
    if st.streaming {
        ("STREAMING", Tone::Success)
    } else if !st.ffmpeg_ok {
        ("NEEDS SETUP", Tone::Danger)
    } else if st.running {
        ("READY", Tone::Accent)
    } else {
        ("STARTING", Tone::Neutral)
    }
}

/// Render the control panel to PNGs without a GPU encoder or a Windows PC.
///
/// ```text
/// cargo test -p brolink-host snapshots -- --ignored --nocapture
/// ```
///
/// Output lands in `target/ui-snapshots/`. Ignored by default: it needs a GPU
/// and is a review aid, not a test.
#[cfg(test)]
mod snapshots {
    use super::*;
    use crate::wake::WakeInfo;
    use brolink_core::identity::Identity;

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

    fn sample_status() -> HostStatus {
        HostStatus {
            running: true,
            encoder: "h264_nvenc".into(),
            lan: Some("192.168.1.20:47850".parse().unwrap()),
            wan: Some("203.0.113.9:47850".parse().unwrap()),
            tailscale: Some("100.101.102.103:47850".parse().unwrap()),
            ticket_display: "blk1_eyJ2IjoxLCJuIjoiR0FNSU5HLVBDIiwiayI6IjM0YjA4ZjEwYzJlNDliN2E4NzFlNjEyOWU4ZTNjOWQxYzY4YTAxNmE0ZjMxOGNlN2ZlOTIzMDFiNTk5ZjA0ZDMiLCJhIjpbIjE5Mi4xNjguMS4yMDo0Nzg1MCIsIjIwMy4wLjExMy45OjQ3ODUwIiwiMTAwLjEwMS4xMDIuMTAzOjQ3ODUwIl19".into(),
            ffmpeg_ok: true,
            upnp: Some("UDP 47850 → 47850 on 203.0.113.9 (permanent)".into()),
            internet: "Reachable from the internet: the router opened UDP 47850.".into(),
            paired: vec![
                ("Example Mac".into(), "0000000000000001".into()),
                ("Second Example Mac".into(), "0000000000000002".into()),
            ],
            wake: Some(WakeInfo {
                mac: Some("02:00:00:00:00:02".into()),
                adapter: "Example NIC".into(),
                magic_packet: Some(true),
                fast_startup: Some(true),
            }),
            log: vec![
                "host id 3f9a1c, listening on UDP 47850".into(),
                "encoder probe: h264_nvenc ok".into(),
                "UPnP: mapped UDP 47850 (permanent lease)".into(),
                "STUN: 203.0.113.9:47850".into(),
                "ticket ready".into(),
            ],
            ..HostStatus::default()
        }
    }

    fn build(status: HostStatus) -> egui_kittest::Harness<'static, HostApp> {
        let cfg = HostConfig::default();
        let engine = Arc::new(Engine::new(cfg.clone(), Identity::generate()));
        *engine.status.lock() = status;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(720.0, 2700.0))
            .with_pixels_per_point(2.0)
            .with_max_steps(8)
            .build_eframe(move |cc| HostApp::new(cc, engine, cfg));
        harness.run_steps(3);
        harness
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn ready() {
        let mut h = build(sample_status());
        save(h.render().unwrap(), "host-ready.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn streaming_with_pin() {
        let mut st = sample_status();
        st.pending_pin = Some("482913".into());
        st.pending_name = Some("Example Mac".into());
        st.client = Some("Example Mac (192.168.1.31)".into());
        st.streaming = true;
        st.fps = 60.0;
        st.bitrate_kbps = 24_800.0;
        st.frames_sent = 18_422;
        st.path = "LAN, direct".into();
        let mut h = build(st);
        save(h.render().unwrap(), "host-streaming-pin.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn needs_setup() {
        let st = HostStatus {
            running: true,
            ffmpeg_ok: false,
            internet: "Local network only so far.".into(),
            last_error: Some("ffmpeg.exe not found; downloading…".into()),
            ..HostStatus::default()
        };
        let mut h = build(st);
        save(h.render().unwrap(), "host-setup.png");
    }
}
