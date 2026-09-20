//! The stream screen: the PC's picture filling the window, and nothing
//! else until Ctrl+Alt is pressed. That frees the mouse and drops a toolbar
//! over the top of the picture, the way a hypervisor's host key does; a
//! click on the picture (or Ctrl+Alt again) hides it and captures the
//! mouse back. Short notices (clipboard, connection, an install in
//! progress) appear under the toolbar's place and fade.
//!
//! allow: SIZE_OK — one stream view; later UI tasks own any split.

use crate::clipboard;
use crate::config::{ClientConfig, StreamSettings};
use crate::input::{self, press_chord, Held};
use crate::path::Path;
use crate::session::Live;
use crate::video;
use brolink_core::api::PowerAction;
use brolink_ui::{self as ui, Tone, PALETTE as P};
use egui::{
    Align, Color32, CursorIcon, Event, Frame, Id, Layout, Margin, Pos2, Rect, RichText, Vec2,
    ViewportCommand,
};
use semver::Version;
use std::time::{Duration, Instant};

const BAR: f32 = 40.0;
/// Below this width the right-hand controls collapse into a real egui More
/// menu so they cannot overlap Disconnect or the PC name.
const OVERFLOW_BELOW: f32 = 1200.0;
const STATUS: f32 = 24.0;
const TOAST_FOR: Duration = Duration::from_secs(5);
/// Each side of an overlay frame (its inner margin) and the picture it
/// leaves visible beyond that.
const OVERLAY_PAD: f32 = 12.0;

/// Content width of an overlay: `max`, or what the screen leaves once the
/// frame's sides and a margin either side are taken off.
fn overlay_width(max: f32, screen_w: f32) -> f32 {
    max.min(screen_w - 4.0 * OVERLAY_PAD).max(0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Disconnect,
    RestartStream,
    /// Ask the PC to leave HDR, which is what a black capture usually is.
    TurnOffHdr,
    Power(PowerAction),
    Fullscreen(bool),
    ToggleCmd,
    /// Install a new BroLink Host on the PC through the stream.
    InstallHost,
    ApplySettings(StreamSettings),
    /// Whether a click on the picture captures the mouse.
    MouseCapture(bool),
}

/// What the window knows that the stream screen should show.
pub struct Env<'a> {
    pub live: &'a Live,
    pub cfg: &'a ClientConfig,
    pub fullscreen: bool,
    /// The path as discovery sees it now; fresher than `live.path`.
    pub path: Option<Path>,
    /// The PC's host version when it cannot take updates over the network,
    /// and the release that could be installed through the stream.
    pub old_host: Option<(Version, Option<Version>)>,
    /// An install through the stream, its status line.
    pub handover: Option<String>,
    /// What the PC said about a black picture, once it has answered.
    pub video_help: Option<crate::display::Help>,
}

struct Toast {
    tone: Tone,
    text: String,
    at: Instant,
}

pub struct View {
    /// The mouse is captured: hidden, held in place, raw movement sent.
    captured: bool,
    grabbed: bool,
    /// Ctrl+Alt dropped the toolbar over the picture.
    bar_shown: bool,
    stats: bool,
    settings: Option<StreamSettings>,
    held: Held,
    scroll: (f32, f32),
    motion: (f32, f32),
    /// Last measured toolbar height, so the notices sit under it.
    bar_h: f32,
    confirm: Option<PowerAction>,
    confirm_install: bool,
    poor: bool,
    poor_hinted: bool,
    toasts: Vec<Toast>,
    /// Pastes seen through `clipboard.pastes_done()`.
    pastes_seen: u32,
    /// Ctrl+Alt was down when capture last toggled; ignore further edges
    /// until both keys are up. `release_all` clears `Held.modifiers`, so
    /// deriving "was down" from that retriggered the toggle every frame.
    capture_chord_held: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            captured: false,
            grabbed: false,
            bar_shown: false,
            stats: false,
            settings: None,
            held: Held::default(),
            scroll: (0.0, 0.0),
            motion: (0.0, 0.0),
            bar_h: BAR,
            confirm: None,
            confirm_install: false,
            poor: false,
            poor_hinted: false,
            toasts: Vec::new(),
            pastes_seen: 0,
            capture_chord_held: false,
        }
    }
}

impl View {
    pub fn set_poor(&mut self, poor: bool) {
        if poor && !self.poor && !self.poor_hinted {
            self.poor_hinted = true;
            self.toast(
                Tone::Danger,
                "Video is arriving unevenly. Open Stats to check loss and decode time, or lower the bitrate in Stream settings.",
            );
        }
        self.poor = poor;
    }

    /// Show a short line under the toolbar for a few seconds.
    pub fn toast(&mut self, tone: Tone, text: impl Into<String>) {
        let text = text.into();
        if self.toasts.iter().any(|t| t.text == text) {
            return;
        }
        self.toasts.push(Toast {
            tone,
            text,
            at: Instant::now(),
        });
    }

    /// Release everything and let the cursor go; called when the stream
    /// ends or the window loses focus.
    pub fn reset(&mut self, ctx: &egui::Context, live: Option<&Live>) {
        if let Some(l) = live {
            self.held.release_all(&l.input);
        }
        self.set_captured(ctx, false);
        self.bar_shown = false;
        self.confirm = None;
        self.settings = None;
        self.confirm_install = false;
        self.poor = false;
        self.poor_hinted = false;
        self.toasts.clear();
        self.capture_chord_held = false;
    }

    fn set_captured(&mut self, ctx: &egui::Context, on: bool) {
        self.captured = on;
        if self.grabbed != on {
            self.grabbed = on;
            let grab = if on {
                if cfg!(target_os = "macos") {
                    egui::CursorGrab::Locked
                } else {
                    egui::CursorGrab::Confined
                }
            } else {
                egui::CursorGrab::None
            };
            ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab));
            // The cursor is hidden through `set_cursor_icon(None)` alone,
            // every frame the pointer is on the picture. A
            // `CursorVisible(true)` here would show it behind egui's
            // back: egui only re-applies an icon when it changes, so the
            // Mac cursor would stay on top of the PC's until the pointer
            // left the window — two pointers.
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, env: &Env<'_>) -> Vec<Action> {
        let live = env.live;
        let mut actions = Vec::new();
        let screen = ctx.screen_rect();
        let stats = live.session.stats();
        let (vw, vh) = if stats.width > 0 {
            (stats.width as f32, stats.height as f32)
        } else {
            (live.requested.0 as f32, live.requested.1 as f32)
        };

        // The picture fills the window, in its own proportions; the
        // toolbar, when Ctrl+Alt has asked for it, lies over the top of it.
        // A menu or a question keeps it there until it is answered.
        let video = fit(screen, vw / vh);
        let bottom_gap = screen.bottom() - video.bottom();
        let popup = ctx.memory(|m| m.any_popup_open());
        let bar_visible = self.bar_shown || popup || self.confirm.is_some() || self.confirm_install;
        let mut bar_rect = bar_visible
            .then(|| Rect::from_min_size(screen.min, Vec2::new(screen.width(), self.bar_h)));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(Color32::BLACK))
            .show(ctx, |ui| {
                if live.frames.seq() > 0 {
                    ui.painter().add(egui::Shape::Callback(
                        egui_wgpu::Callback::new_paint_callback(
                            video,
                            video::Paint {
                                frames: live.frames.clone(),
                            },
                        ),
                    ));
                } else {
                    ui.painter().rect_filled(video, 0.0, P.bg);
                    let c = video.center();
                    ui.put(
                        Rect::from_center_size(c, Vec2::new(300.0, 60.0)),
                        ui::spinner(22.0, P.accent),
                    );
                    ui.painter().text(
                        c + Vec2::new(0.0, 36.0),
                        egui::Align2::CENTER_CENTER,
                        if live.session.connected() {
                            format!("Waiting for video from {}…", live.pc)
                        } else {
                            format!("Connecting to {}…", live.pc)
                        },
                        egui::FontId::proportional(15.0),
                        P.muted,
                    );
                    ui.painter().text(
                        c + Vec2::new(0.0, 60.0),
                        egui::Align2::CENTER_CENTER,
                        format!("{} · {}", live.path.label(), live.settings.describe()),
                        egui::FontId::proportional(12.5),
                        P.faint,
                    );
                }
                if bottom_gap >= STATUS || self.stats {
                    self.status_line(ui, screen, video, env, &stats, bottom_gap >= STATUS);
                }
            });

        if let Some(rect) = bar_rect {
            let measured = self.toolbar(ctx, rect, env, &mut actions);
            self.bar_h = measured.height().max(BAR);
            bar_rect = Some(Rect::from_min_size(
                screen.min,
                Vec2::new(screen.width(), self.bar_h),
            ));
        }
        self.toasts(
            ctx,
            screen,
            bar_rect,
            env,
            stats.video_problem.as_deref(),
            &mut actions,
        );

        self.settings_panel(ctx, env, &mut actions);
        if self.stats {
            self.diagnostics(ctx, env, &stats);
        }
        self.input(ctx, live, env.cfg, video, bar_rect);
        actions
    }

    fn settings_panel(&mut self, ctx: &egui::Context, env: &Env<'_>, actions: &mut Vec<Action>) {
        let Some(settings) = self.settings.as_mut() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Stream settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .max_width((ctx.screen_rect().width() - 48.0).max(280.0))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 150.0).max(120.0))
                    .show(ui, |ui| {
                        crate::settings::stream_controls(
                            ui,
                            settings,
                            crate::app::ClientApp::native_pixels(ctx),
                        );
                    });
                ui::row_separator(ui);
                ui::caption(ui, "Applying reconnects the stream with these settings.");
                ui.horizontal(|ui| {
                    apply = ui::primary_button(ui, "Apply and reconnect").clicked();
                    cancel = ui::ghost_button(ui, "Cancel").clicked();
                });
            });
        if apply {
            actions.push(Action::ApplySettings(settings.clone()));
        }
        if !open || apply || cancel {
            self.settings = None;
        }
        let _ = env;
    }

    fn diagnostics(&mut self, ctx: &egui::Context, env: &Env<'_>, stats: &brolink_stream::Stats) {
        let live = env.live;
        egui::Window::new("Stream performance").open(&mut self.stats)
            .resizable(false).collapsible(false).default_pos(Pos2::new(16.0, 60.0)).default_width(330.0)
            .show(ctx, |ui| {
                let path = env.path.as_ref().unwrap_or(&live.path);
                ui::status_pill(ui, &path.label(), path.tone());
                ui::caption(ui, live.quality_label());
                ui.add_space(8.0);
                egui::Grid::new("stream-metrics").num_columns(2).spacing([18.0, 10.0]).show(ui, |ui| {
                    for (name, value) in [
                        ("Stream resolution", format!("{} × {}", stats.width, stats.height)),
                        ("Frames decoded", format!("{:.1} / {} fps", stats.fps, live.requested.2)),
                        ("Video received", format!("{:.2} Mbps", stats.mbps)),
                        ("Bitrate target", format!("{} Mbps", live.settings.bitrate_kbps / 1000)),
                        ("Network round trip", format!("{} ± {} ms", stats.rtt_ms, stats.rtt_var_ms)),
                        ("Packet loss", format!("{:.2}%", stats.loss_pct)),
                        ("Host processing", format!("{:.2} ms", stats.host_ms)),
                        ("Frame assembly", format!("{:.2} ms", stats.assembly_ms)),
                        ("Decoder queue", format!("{:.2} ms", stats.queue_ms)),
                        ("Decode time", format!("{:.2} ms", stats.decode_ms)),
                        ("Decoder", stats.decoder.to_string()),
                    ] {
                        ui::caption(ui, name);
                        ui.label(RichText::new(value).color(P.text));
                        ui.end_row();
                    }
                });
                ui.add_space(8.0);
                ui::caption(ui, "The encoder holds the target bitrate (CBR). Compare received against target; large swings mean the path is dropping packets.");
                if !stats.audio.is_empty() { ui::caption(ui, &stats.audio); }
                if ui::ghost_button(ui, "Copy diagnostics").clicked() {
                    ctx.copy_text(format!("BroLink {}\n{}\nRequested: {} × {}, {} fps, {} Mbps\n{stats:#?}",
                        env!("CARGO_PKG_VERSION"), path.label(), live.requested.0, live.requested.1,
                        live.requested.2, live.settings.bitrate_kbps / 1000));
                }
            });
    }

    fn toolbar(
        &mut self,
        ctx: &egui::Context,
        rect: Rect,
        env: &Env<'_>,
        actions: &mut Vec<Action>,
    ) -> Rect {
        let live = env.live;
        let cfg = env.cfg;
        let overflow = rect.width() < OVERFLOW_BELOW;
        let frame = ui::overlay_frame()
            .corner_radius(0)
            .inner_margin(Margin::symmetric(12, 0));
        let path = env.path.clone().unwrap_or_else(|| live.path.clone());
        let bar = egui::Area::new(Id::new("stream-toolbar"))
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_min_width(rect.width());
                ui.set_max_width(rect.width());
                ui.set_min_height(BAR);
                frame.show(ui, |ui| {
                    ui.set_min_height(BAR);
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let s = live.session.stats();
                        let tone = if !live.session.connected() {
                            Tone::Accent
                        } else if self.poor || s.video_problem.is_some() {
                            Tone::Danger
                        } else {
                            Tone::Success
                        };
                        ui.label(RichText::new("●").size(9.0).color(tone.color()));
                        ui.label(
                            RichText::new(&live.pc)
                                .font(brolink_ui::theme::medium(13.5))
                                .color(P.text),
                        );
                        if !overflow {
                            let (w, h) = if s.width > 0 {
                                (s.width, s.height)
                            } else {
                                (live.requested.0, live.requested.1)
                            };
                            ui::caption(
                                ui,
                                format!("{w}×{h} · {} fps · {}", live.requested.2, live.codec),
                            );
                            ui.add_space(2.0);
                            let pill = ui::status_pill(ui, &path.label(), path.tone());
                            if path.relayed() {
                                pill.on_hover_text(
                                    "This is the route currently in use. A relay does not impose a bitrate limit in BroLink.",
                                );
                            } else if path.direct == Some(true) {
                                pill.on_hover_text("Packets go straight to the PC.");
                            }
                            ui::caption(ui, "Ctrl+Alt hides this bar");
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui::danger_button(ui, "Disconnect").clicked() {
                                actions.push(Action::Disconnect);
                            }
                            if ui::ghost_button(ui, "Stream settings").clicked() {
                                self.held.release_all(&live.input);
                                self.set_captured(ctx, false);
                                self.settings = Some(cfg.stream.clone());
                            }
                            if overflow {
                                self.more_menu(ui, ctx, env, actions);
                            } else {
                                self.pc_menu(ui, env, actions);
                                self.fullscreen_button(ui, env, actions);
                                self.stats_button(ui);
                                self.keys_menu(ui, live, cfg, actions);
                                self.mouse_menu(ui, ctx, live, cfg, actions);
                            }
                        });
                    });
                });
            });

        if let Some(action) = self.confirm {
            let w = overlay_width(440.0, rect.width());
            egui::Area::new(Id::new("stream-confirm"))
                .fixed_pos(Pos2::new(
                    rect.center().x - w / 2.0 - OVERLAY_PAD,
                    rect.bottom() + 8.0,
                ))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(w);
                        ui.label(
                            RichText::new(format!(
                            "{} {}? Programs are closed without asking; anything unsaved is lost.",
                            action.label(),
                            live.pc
                        ))
                            .color(P.text),
                        );
                        ui.horizontal(|ui| {
                            if ui::toned_button(ui, action.label(), Tone::Danger).clicked() {
                                actions.push(Action::Power(action));
                                self.confirm = None;
                            }
                            if ui::ghost_button(ui, "Cancel").clicked() {
                                self.confirm = None;
                            }
                        });
                    });
                });
        }
        if self.confirm_install {
            let (running, latest) = env
                .old_host
                .clone()
                .map(|(r, l)| (r.to_string(), l.map(|v| v.to_string())))
                .unwrap_or_default();
            let w = overlay_width(500.0, rect.width());
            egui::Area::new(Id::new("stream-install"))
                .fixed_pos(Pos2::new(
                    rect.center().x - w / 2.0 - OVERLAY_PAD,
                    rect.bottom() + 8.0,
                ))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(w);
                        ui.label(
                            RichText::new(format!("Install BroLink Host {} on {}", latest.as_deref().unwrap_or("(latest)"), live.pc))
                                .font(brolink_ui::theme::medium(14.0))
                                .color(P.text),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} runs {running}, which cannot take updates over the network. This Mac will press Win+R on the PC, type one line that fetches the new version from this Mac over Tailscale, and press Enter. A PowerShell window appears on the PC, swaps the file in and restarts the service; the stream is not interrupted. The PC's desktop must be unlocked and in front.",
                                live.pc
                            ))
                            .color(P.muted),
                        );
                        ui.horizontal(|ui| {
                            if ui::primary_button(ui, "Install through the stream").clicked() {
                                actions.push(Action::InstallHost);
                                self.confirm_install = false;
                            }
                            if ui::ghost_button(ui, "Cancel").clicked() {
                                self.confirm_install = false;
                            }
                        });
                    });
                });
        }
        bar.response.rect
    }

    fn more_menu(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        env: &Env<'_>,
        actions: &mut Vec<Action>,
    ) {
        let live = env.live;
        let cfg = env.cfg;
        let response = ui::ghost_button(ui, "More");
        let popup_id = Id::new("stream-more");
        if response.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &response,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(180.0);
                self.mouse_menu(ui, ctx, live, cfg, actions);
                self.keys_menu(ui, live, cfg, actions);
                if ui
                    .button(if self.stats { "Hide stats" } else { "Stats" })
                    .clicked()
                {
                    self.stats = !self.stats;
                    ui.memory_mut(|m| m.close_popup());
                }
                let fs = if env.fullscreen {
                    "Exit full screen"
                } else {
                    "Full screen"
                };
                if ui.button(fs).clicked() {
                    actions.push(Action::Fullscreen(!env.fullscreen));
                    ui.memory_mut(|m| m.close_popup());
                }
                self.pc_menu(ui, env, actions);
            },
        );
    }

    fn fullscreen_button(&self, ui: &mut egui::Ui, env: &Env<'_>, actions: &mut Vec<Action>) {
        if ui::ghost_button(
            ui,
            if env.fullscreen {
                "Exit full screen"
            } else {
                "Full screen"
            },
        )
        .clicked()
        {
            actions.push(Action::Fullscreen(!env.fullscreen));
        }
    }

    fn stats_button(&mut self, ui: &mut egui::Ui) {
        if ui::ghost_button(ui, if self.stats { "Hide stats" } else { "Stats" })
            .on_hover_text(
                "Show received and target bitrate, frame rate, network delay and decoder performance",
            )
            .clicked()
        {
            self.stats = !self.stats;
        }
    }

    fn pc_menu(&mut self, ui: &mut egui::Ui, env: &Env<'_>, actions: &mut Vec<Action>) {
        ui::menu_button(ui, "PC", |ui| {
            if ui.button("Sleep").clicked() {
                actions.push(Action::Power(PowerAction::Sleep));
                ui.close_menu();
            }
            if ui.button("Restart…").clicked() {
                self.confirm = Some(PowerAction::Restart);
                ui.close_menu();
            }
            if ui.button("Shut down…").clicked() {
                self.confirm = Some(PowerAction::Shutdown);
                ui.close_menu();
            }
            if let Some((running, _)) = &env.old_host {
                ui.separator();
                let label = format!("Update BroLink Host… (runs {running})");
                if ui
                    .add_enabled(env.handover.is_none(), egui::Button::new(label))
                    .on_hover_text("Installs the newest BroLink Host on the PC through this stream, so future updates arrive by themselves.")
                    .clicked()
                {
                    self.confirm_install = true;
                    ui.close_menu();
                }
            }
        });
    }

    fn keys_menu(
        &mut self,
        ui: &mut egui::Ui,
        live: &Live,
        cfg: &ClientConfig,
        actions: &mut Vec<Action>,
    ) {
        ui::menu_button(ui, "Keys", |ui| {
            let input = &live.input;
            for (label, keys) in [
                (
                    "Ctrl+Alt+Del",
                    &[input::VK_CONTROL, input::VK_MENU, input::VK_DELETE][..],
                ),
                ("Windows key", &[input::VK_LWIN][..]),
                ("Alt+Tab", &[input::VK_MENU, input::VK_TAB][..]),
                ("Esc", &[input::VK_ESCAPE][..]),
                ("Print Screen", &[input::VK_SNAPSHOT][..]),
            ] {
                if ui.button(label).clicked() {
                    self.held.chord(input, keys);
                    ui.close_menu();
                }
            }
            ui.separator();
            let cmd = if cfg.cmd_is_ctrl {
                "● ⌘ acts as Ctrl (⌘C, ⌘V, ⌘Z work)"
            } else {
                "○ ⌘ acts as Ctrl (now the Windows key)"
            };
            if ui
                .button(cmd)
                .on_hover_text("Click to switch what the Command key does on the PC")
                .clicked()
            {
                actions.push(Action::ToggleCmd);
                ui.close_menu();
            }
            ui.separator();
            ui::caption(
                ui,
                "Everything else goes to the PC as pressed. Ctrl+Alt toggles mouse capture.",
            );
            if live.clipboard.unsupported() {
                ui::caption(
                    ui,
                    "Clipboard sync needs BroLink Host 3.1 on the PC (PC → Update BroLink Host).",
                );
            } else {
                ui::caption(
                    ui,
                    "⌘C on the PC copies to this Mac; ⌘V pastes this Mac's text.",
                );
            }
        });
    }

    fn mouse_menu(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        live: &Live,
        cfg: &ClientConfig,
        actions: &mut Vec<Action>,
    ) {
        let label = if cfg.capture_mouse {
            "Mouse: captured"
        } else {
            "Mouse: free"
        };
        ui::menu_button(ui, label, |ui| {
            let mark = |on: bool| if on { "● " } else { "   " };
            if ui
                .button(format!(
                    "{}Captured · raw movement, which games read. A click on the picture captures; Ctrl+Alt frees",
                    mark(cfg.capture_mouse)
                ))
                .clicked()
            {
                if !cfg.capture_mouse {
                    actions.push(Action::MouseCapture(true));
                }
                ui.close_menu();
            }
            if ui
                .button(format!(
                    "{}Free · the Mac cursor's position is sent, 1:1 on the PC",
                    mark(!cfg.capture_mouse)
                ))
                .clicked()
            {
                if cfg.capture_mouse {
                    actions.push(Action::MouseCapture(false));
                    self.release(ctx, live);
                }
                ui.close_menu();
            }
        });
    }

    /// Short notices under the toolbar: clipboard, connection, an install.
    fn toasts(
        &mut self,
        ctx: &egui::Context,
        screen: Rect,
        bar: Option<Rect>,
        env: &Env<'_>,
        video_problem: Option<&str>,
        actions: &mut Vec<Action>,
    ) {
        self.toasts.retain(|t| t.at.elapsed() < TOAST_FOR);
        let mut lines: Vec<(Tone, String)> = self
            .toasts
            .iter()
            .map(|t| (t.tone, t.text.clone()))
            .collect();
        if let Some(h) = &env.handover {
            lines.push((Tone::Info, h.clone()));
        }
        if let Some(n) = env.live.clipboard.note() {
            lines.push((Tone::Neutral, n));
        }
        if lines.is_empty() && video_problem.is_none() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        let top = bar.map(|b| b.bottom()).unwrap_or(screen.top()) + 10.0;
        let w = overlay_width(496.0, screen.width());
        egui::Area::new(Id::new("stream-toasts"))
            .fixed_pos(Pos2::new(screen.center().x - w / 2.0 - OVERLAY_PAD, top))
            .order(egui::Order::Foreground)
            .interactable(video_problem.is_some())
            .show(ctx, |ui| {
                ui.set_width(w + 2.0 * OVERLAY_PAD);
                if let Some(problem) = video_problem {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(w);
                        ui.label(RichText::new(problem).color(P.text));
                        // The PC's own answer, once it has given one.
                        if let Some(help) = env.video_help.as_ref() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(&help.message).color(P.muted));
                        }
                        ui.horizontal(|ui| {
                            if ui::ghost_button(ui, "Restart stream").clicked() {
                                actions.push(Action::RestartStream);
                            }
                            let Some(help) = env.video_help.as_ref() else {
                                return;
                            };
                            if !help.hdr_is_on {
                                return;
                            }
                            if help.busy {
                                ui.label(
                                    RichText::new(format!("Turning {} off…", help.mode))
                                        .color(P.muted),
                                );
                            } else if ui::ghost_button(
                                ui,
                                &format!("Turn off {} on the PC", help.mode),
                            )
                            .clicked()
                            {
                                actions.push(Action::TurnOffHdr);
                            }
                        });
                    });
                }
                for (tone, text) in lines {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(w);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("●").size(9.0).color(tone.color()));
                            ui.label(RichText::new(text).color(P.text));
                        });
                    });
                }
            });
    }

    fn status_line(
        &self,
        ui: &mut egui::Ui,
        screen: Rect,
        video: Rect,
        env: &Env<'_>,
        stats: &brolink_stream::Stats,
        in_gap: bool,
    ) {
        let live = env.live;
        let secs = live.started.elapsed().as_secs();
        let mut text = format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60
        );
        text.push_str(&format!(
            "   {:.0} / {} fps   {:.1} / {} Mbps   {} ms RTT",
            stats.fps,
            live.requested.2,
            stats.mbps,
            live.settings.bitrate_kbps / 1000,
            stats.rtt_ms
        ));
        let font = egui::FontId::monospace(12.0);
        if in_gap {
            let y = video.bottom() + (screen.bottom() - video.bottom()) / 2.0;
            ui.painter().text(
                Pos2::new(screen.left() + 14.0, y),
                egui::Align2::LEFT_CENTER,
                text,
                font,
                P.faint,
            );
        } else {
            let galley = ui.painter().layout_no_wrap(text, font, P.text);
            let size = galley.size() + Vec2::new(20.0, 10.0);
            let rect = Rect::from_min_size(
                Pos2::new(screen.left() + 12.0, screen.bottom() - size.y - 12.0),
                size,
            );
            ui.painter()
                .rect_filled(rect, 6.0, Color32::from_black_alpha(190));
            ui.painter()
                .galley(rect.min + Vec2::new(10.0, 5.0), galley, P.text);
        }
    }

    /// Capture the mouse and take the toolbar away: the picture is all.
    fn grab(&mut self, ctx: &egui::Context, live: &Live) {
        self.held.release_all(&live.input);
        self.motion = (0.0, 0.0);
        self.set_captured(ctx, true);
        self.bar_shown = false;
    }

    /// Give the mouse back.
    fn release(&mut self, ctx: &egui::Context, live: &Live) {
        self.held.release_all(&live.input);
        self.set_captured(ctx, false);
    }

    /// Ctrl+Alt, the host key. From the picture it frees the mouse and
    /// drops the toolbar; from the toolbar it hides it and, when capture
    /// is on, takes the mouse back.
    fn host_key(&mut self, ctx: &egui::Context, live: &Live, cfg: &ClientConfig) {
        if self.captured || !self.bar_shown {
            self.release(ctx, live);
            self.bar_shown = true;
        } else {
            self.bar_shown = false;
            if cfg.capture_mouse {
                self.grab(ctx, live);
            }
        }
    }

    /// The stream has just connected: take the mouse if the pointer is
    /// here already, and say once how to get it back.
    pub fn stream_started(&mut self, ctx: &egui::Context, live: &Live, capture: bool) {
        // Capture as soon as the stream connects, whenever the window has
        // focus, without waiting for the pointer to be over the picture. A
        // game that reads raw relative motion (Genshin's camera, say) only
        // gets it while captured; requiring the pointer to already sit on the
        // picture meant a connect with the pointer elsewhere left the game
        // deaf to the mouse until the user knew to click. Ctrl+Alt still frees
        // it. The click-to-capture path stays for re-capturing after a free.
        let focused = ctx.input(|i| i.focused);
        if capture && focused {
            self.grab(ctx, live);
        }
        self.toast(
            Tone::Info,
            if capture {
                "Ctrl+Alt frees the mouse and shows the toolbar; a click on the picture captures it again."
            } else {
                "Ctrl+Alt shows and hides the toolbar."
            },
        );
    }

    /// Forward this frame's keyboard and mouse events to the PC.
    fn input(
        &mut self,
        ctx: &egui::Context,
        live: &Live,
        cfg: &ClientConfig,
        video: Rect,
        bar: Option<Rect>,
    ) {
        let input = &live.input;
        let (events, modifiers, pointer, focused) = ctx.input(|i| {
            (
                i.events.clone(),
                i.modifiers,
                i.pointer.latest_pos(),
                i.focused,
            )
        });
        let popup = ctx.memory(|m| m.any_popup_open());
        let overlay = self.confirm.is_some() || self.confirm_install || self.settings.is_some();
        let over_bar = pointer.is_some_and(|p| bar.is_some_and(|b| b.contains(p)));
        let over_overlay = pointer.is_some_and(|p| {
            ctx.layer_id_at(p)
                .is_some_and(|layer| layer.order > egui::Order::Background)
        });
        let over_video = pointer_on_stream(
            pointer.is_some_and(|p| video.contains(p)),
            over_bar,
            popup,
            overlay,
            over_overlay,
        );
        if self.captured && !popup && !overlay {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        let keys_to_pc =
            focused && !ctx.wants_keyboard_input() && !popup && !overlay && input.connected();
        let ppp = ctx.pixels_per_point();

        if !focused {
            if self.held.any_down() || self.captured {
                self.reset(ctx, Some(live));
            }
            return;
        }
        // The escape chord must remain available even if an overlay took focus.
        let ctrl_alt = modifiers.ctrl && modifiers.alt;
        if !ctrl_alt {
            self.capture_chord_held = false;
        }
        if ctrl_alt && !self.capture_chord_held && input.connected() {
            self.capture_chord_held = true;
            self.host_key(ctx, live, cfg);
            return;
        }
        if keys_to_pc {
            self.held.modifiers(input, modifiers, cfg.cmd_is_ctrl);
            // A paste's chord releases the modifier it pressed once the text
            // has reached the PC. If ⌘ is still held here, press it again
            // on the PC, so ⌘V ⌘V in one hold pastes twice.
            let done = live.clipboard.pastes_done();
            if done != self.pastes_seen {
                self.pastes_seen = done;
                for vk in [input::VK_CONTROL, input::VK_LWIN] {
                    if self.held.is_down(vk) {
                        input.key(vk, true, self.held.mask());
                    }
                }
            }
        } else if self.held.any_down() {
            self.held.release_all(input);
        }

        if (over_video || self.captured) && input.connected() {
            ctx.set_cursor_icon(CursorIcon::None);
        }

        let mut position: Option<Pos2> = None;
        for ev in events {
            match ev {
                Event::Key {
                    physical_key,
                    key,
                    pressed,
                    repeat,
                    ..
                } if keys_to_pc && (pressed || !repeat) => {
                    // OS autorepeat arrives as extra Downs with `repeat`.
                    // Sunshine does not generate them, so holding a key
                    // would otherwise type once. Releases never repeat.
                    if let Some(vk) = physical_key.and_then(input::vk).or_else(|| input::vk(key)) {
                        self.held.key(input, vk, pressed);
                    }
                }
                Event::Copy if keys_to_pc => {
                    self.clipboard_chord(live, input::VK_C, cfg);
                    live.clipboard.poke();
                }
                Event::Cut if keys_to_pc => {
                    self.clipboard_chord(live, input::VK_X, cfg);
                    live.clipboard.poke();
                }
                Event::Paste(text) if keys_to_pc => {
                    // The Mac's text first, then the paste: the PC pastes
                    // what was copied here, not what it had. The chord is
                    // sent whole: by the time the text has crossed the
                    // network a quick tap has let go of ⌘.
                    let keys = vec![self.clipboard_modifier(cfg), input::VK_V];
                    let mask = self.held.mask();
                    let handle = input.clone();
                    let text = if clipboard::worth_sending(&text) {
                        text
                    } else {
                        String::new()
                    };
                    live.clipboard
                        .push(text, move || press_chord(&handle, &keys, mask));
                }
                Event::PointerMoved(p) if !self.captured && over_video => position = Some(p),
                Event::MouseMoved(d) if self.captured => {
                    self.motion.0 += d.x;
                    self.motion.1 += d.y;
                }
                Event::PointerButton {
                    button, pressed, ..
                } if self.captured || over_video || (!pressed && self.held.any_down()) => {
                    // A click on the picture puts the toolbar away and, when
                    // capture is on, takes the mouse. The click itself still
                    // goes to the PC: what was clicked on is what was meant.
                    if pressed && over_video && !self.captured {
                        self.bar_shown = false;
                        if cfg.capture_mouse {
                            self.grab(ctx, live);
                        }
                    }
                    if let Some(b) = input::button(button) {
                        self.held.button(input, b, pressed);
                    }
                }
                Event::MouseWheel { unit, delta, .. } if self.captured || over_video => {
                    let scale = match unit {
                        egui::MouseWheelUnit::Point => 3.0,
                        egui::MouseWheelUnit::Line => 120.0,
                        egui::MouseWheelUnit::Page => 1200.0,
                    };
                    self.scroll.0 += delta.x * scale;
                    self.scroll.1 += delta.y * scale;
                }
                Event::WindowFocused(false) => self.reset(ctx, Some(live)),
                _ => {}
            }
        }
        if let Some(p) = position {
            let x = ((p.x - video.left()) * ppp).round() as i16;
            let y = ((p.y - video.top()) * ppp).round() as i16;
            input.mouse_position(
                x,
                y,
                (video.width() * ppp).round() as i16,
                (video.height() * ppp).round() as i16,
            );
        }
        if self.captured && (self.motion.0.abs() >= 1.0 || self.motion.1.abs() >= 1.0) {
            let dx = self.motion.0.trunc();
            let dy = self.motion.1.trunc();
            self.motion.0 -= dx;
            self.motion.1 -= dy;
            input.mouse_move(dx as i16, dy as i16);
        }
        if self.scroll.0.abs() >= 1.0 || self.scroll.1.abs() >= 1.0 {
            let h = self.scroll.0.trunc();
            let v = self.scroll.1.trunc();
            self.scroll.0 -= h;
            self.scroll.1 -= v;
            input.scroll(
                v.clamp(-32000.0, 32000.0) as i16,
                h.clamp(-32000.0, 32000.0) as i16,
            );
        }
    }

    /// What ⌘ stands for in a clipboard shortcut.
    fn clipboard_modifier(&self, cfg: &ClientConfig) -> i16 {
        if cfg!(target_os = "macos") && !cfg.cmd_is_ctrl {
            input::VK_LWIN
        } else {
            input::VK_CONTROL
        }
    }

    /// egui swallows Cmd/Ctrl+C/X/V into clipboard events; replay them as
    /// the key chord the PC expects.
    fn clipboard_chord(&mut self, live: &Live, vk: i16, cfg: &ClientConfig) {
        let modifier = self.clipboard_modifier(cfg);
        self.held.chord(&live.input, &[modifier, vk]);
    }
}

/// Whether a pointer in the picture should be sent to the PC. A real egui
/// popup (More) sets `popup`; a hand-rolled `Area` at `Order::Background`
/// would not, and clicks would leak through.
fn pointer_on_stream(
    in_video: bool,
    over_bar: bool,
    popup: bool,
    overlay: bool,
    over_overlay: bool,
) -> bool {
    in_video && !over_bar && !popup && !overlay && !over_overlay
}

/// "Auto · Smooth" → "Auto", "Custom · Sharp" → "Sharp", else "Custom".
/// Whether `host_version` is too old for `/v1/update`, with the release
/// that an install through the stream would put on it.
pub fn old_host(
    host_version: &str,
    latest: Option<&Version>,
) -> Option<(Version, Option<Version>)> {
    let v = Version::parse(host_version).ok()?;
    if brolink_core::update::host_can_receive_update(&v) {
        return None;
    }
    Some((v, latest.cloned()))
}

/// The largest rect of the given aspect ratio centred in `area`.
pub fn fit(area: Rect, aspect: f32) -> Rect {
    let (w, h) = if area.width() / area.height() > aspect {
        (area.height() * aspect, area.height())
    } else {
        (area.width(), area.width() / aspect)
    };
    Rect::from_center_size(area.center(), Vec2::new(w.floor(), h.floor()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;
    use crate::session::Live;

    #[test]
    fn fit_letterboxes_a_16_9_stream_on_a_16_10_screen() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1512.0, 982.0));
        let v = fit(screen, 16.0 / 9.0);
        assert_eq!(v.width(), 1512.0);
        assert!((v.height() - 850.0).abs() <= 1.0);
        assert!(
            v.top() > 60.0,
            "a 16:9 picture on a 16:10 screen leaves a bar above: {}",
            v.top()
        );
        let tall = fit(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 900.0)),
            16.0 / 9.0,
        );
        assert_eq!(tall.width(), 800.0);
    }

    #[test]
    fn a_3_0_host_is_offered_an_install_through_the_stream() {
        let latest = Version::new(3, 1, 0);
        let (v, l) = old_host("3.0.0", Some(&latest)).unwrap();
        assert_eq!(v, Version::new(3, 0, 0));
        assert_eq!(l, Some(latest));
        assert!(old_host("3.1.0", None).is_none());
        assert!(old_host("soon", None).is_none());
        assert!(old_host("3.0.1", None).is_some());
    }

    #[test]
    fn overlay_width_clamps_to_the_screen() {
        assert_eq!(overlay_width(440.0, 640.0), 440.0);
        assert_eq!(overlay_width(500.0, 640.0), 500.0);
        assert_eq!(overlay_width(496.0, 640.0), 496.0);
        assert_eq!(overlay_width(440.0, 400.0), 400.0 - 4.0 * OVERLAY_PAD);
        assert!(overlay_width(500.0, 400.0) + 4.0 * OVERLAY_PAD <= 400.0);
    }

    #[test]
    fn a_popup_eats_clicks_that_would_otherwise_hit_the_picture() {
        assert!(pointer_on_stream(true, false, false, false, false));
        assert!(
            !pointer_on_stream(true, false, true, false, false),
            "More open: any_popup_open must block remote mouse"
        );
        assert!(!pointer_on_stream(true, true, false, false, false));
        assert!(!pointer_on_stream(true, false, false, true, false));
        assert!(!pointer_on_stream(true, false, false, false, true));
        assert!(
            pointer_on_stream(true, false, false, false, false),
            "a Background Area would leave popup=false and leak the click"
        );
    }

    struct Fixture {
        view: View,
        live: Option<Live>,
        cfg: ClientConfig,
        fullscreen: bool,
        applied: bool,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(live) = self.live.take() {
                live.session.stop();
            }
        }
    }

    fn dummy_live(ctx: &egui::Context) -> Live {
        let (tx, rx) = std::sync::mpsc::channel();
        let frames = std::sync::Arc::new(brolink_stream::FrameSlot::default());
        let path = crate::path::Path {
            direct: Some(false),
            relay: "tok".into(),
            rtt_ms: Some(210),
            ..Default::default()
        };
        let settings = crate::config::StreamSettings::default();
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
        Live {
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
            clipboard: crate::clipboard::Sync::spawn(ip, ctx.clone()),
        }
    }

    fn paint(ctx: &egui::Context, f: &mut Fixture) {
        if !f.applied {
            brolink_ui::apply(ctx);
            f.applied = true;
            return;
        }
        if f.live.is_none() {
            f.live = Some(dummy_live(ctx));
        }
        let Fixture {
            view,
            live,
            cfg,
            fullscreen,
            ..
        } = f;
        let env = Env {
            live: live.as_ref().unwrap(),
            cfg,
            fullscreen: *fullscreen,
            path: None,
            old_host: None,
            handover: None,
            video_help: None,
        };
        view.show(ctx, &env);
    }

    /// A view with the toolbar dropped, as Ctrl+Alt leaves it.
    fn view_with_bar() -> View {
        View {
            bar_shown: true,
            ..View::default()
        }
    }

    fn harness(size: Vec2) -> egui_kittest::Harness<'static, Fixture> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .with_pixels_per_point(1.0)
            .with_max_steps(2)
            .build_state(
                paint,
                Fixture {
                    view: view_with_bar(),
                    live: None,
                    cfg: ClientConfig::default(),
                    fullscreen: false,
                    applied: false,
                },
            )
    }

    fn bar_control_rects(h: &egui_kittest::Harness<'_, Fixture>) -> Vec<(String, Rect)> {
        use egui_kittest::kittest::Queryable;
        let names = [
            "Disconnect",
            "More",
            "PC",
            "Full screen",
            "Exit full screen",
            "Stats",
            "Hide stats",
            "Keys",
            "Stream settings",
            "Mouse",
        ];
        let mut out = Vec::new();
        for name in names {
            for node in h.query_all_by_label_contains(name) {
                let Some(b) = node.raw_bounds() else { continue };
                if b.y0 > f64::from(BAR + 8.0) {
                    continue;
                }
                out.push((
                    name.to_string(),
                    Rect::from_min_max(
                        Pos2::new(b.x0 as f32, b.y0 as f32),
                        Pos2::new(b.x1 as f32, b.y1 as f32),
                    ),
                ));
            }
        }
        out
    }

    fn assert_no_overlap(rects: &[(String, Rect)]) {
        for (i, (a_name, a)) in rects.iter().enumerate() {
            for (b_name, b) in rects.iter().skip(i + 1) {
                let hit = a.intersect(*b);
                assert!(
                    hit.width() <= 1.0 || hit.height() <= 1.0,
                    "{a_name} {} overlaps {b_name} {}",
                    a,
                    b
                );
            }
        }
    }

    #[test]
    fn toolbar_at_640_does_not_overlap_and_keeps_disconnect() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness(Vec2::new(640.0, 420.0));
        h.run_steps(2);
        assert!(
            h.query_by_label("Disconnect").is_some(),
            "Disconnect must stay reachable"
        );
        assert!(
            h.query_by_label_contains("More").is_some(),
            "narrow width must overflow into More"
        );
        assert!(
            h.query_by_label("Stream settings").is_some(),
            "Stream settings stays on the bar at every width"
        );
        let rects = bar_control_rects(&h);
        assert_no_overlap(&rects);
        assert!(
            h.state().view.bar_h >= BAR,
            "bar_rect is measured, got {}",
            h.state().view.bar_h
        );
    }

    #[test]
    fn more_is_a_real_egui_popup_and_consumes_clicks() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness(Vec2::new(640.0, 420.0));
        h.run_steps(2);
        assert!(
            !h.ctx.memory(|m| m.any_popup_open()),
            "closed More is not a popup"
        );
        h.get_by_label("More").simulate_click();
        h.run_steps(2);
        assert!(
            h.ctx.memory(|m| m.any_popup_open()),
            "More must use egui::popup so any_popup_open is true; a Background Area would fail this"
        );
        assert!(
            h.query_by_label("Stream settings").is_some()
                || h.query_by_label_contains("Keys").is_some()
                || h.query_by_label("Stats").is_some(),
            "More lists the overflowed controls"
        );
        let popup_open = h.ctx.memory(|m| m.any_popup_open());
        assert!(!pointer_on_stream(true, false, popup_open, false, false));
        if let Some(stats) = h.query_by_label("Stats") {
            stats.simulate_click();
            h.run_steps(2);
            assert!(
                h.state().view.stats,
                "click inside More is consumed by the popup, not forwarded"
            );
        }
    }

    #[test]
    fn toolbar_at_normal_width_keeps_controls_on_the_bar() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness(Vec2::new(1400.0, 860.0));
        h.run_steps(2);
        assert!(h.query_by_label("Disconnect").is_some());
        assert!(h.query_by_label_contains("More").is_none());
        assert!(h.query_by_label("Stream settings").is_some());
        assert!(h.query_by_label_contains("Keys").is_some());
        assert_no_overlap(&bar_control_rects(&h));
    }

    fn save_snapshot(img: image::RgbaImage, name: &str) {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
        std::fs::create_dir_all(&dir).expect("create snapshot dir");
        let path = dir.join(name);
        img.save(&path).expect("write png");
        eprintln!("wrote {}", path.display());
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn stream_toolbar_snapshot_min_width() {
        let mut h = egui_kittest::Harness::builder()
            .wgpu()
            .with_size(Vec2::new(640.0, 420.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(2)
            .build_state(
                paint,
                Fixture {
                    view: view_with_bar(),
                    live: None,
                    cfg: ClientConfig::default(),
                    fullscreen: false,
                    applied: false,
                },
            );
        h.run_steps(3);
        save_snapshot(h.render().unwrap(), "client-stream-toolbar-640.png");
    }

    #[test]
    #[ignore = "renders with a GPU; run on demand to review the UI"]
    fn stream_toolbar_snapshot_normal_width() {
        let mut h = egui_kittest::Harness::builder()
            .wgpu()
            .with_size(Vec2::new(1400.0, 860.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(2)
            .build_state(
                paint,
                Fixture {
                    view: view_with_bar(),
                    live: None,
                    cfg: ClientConfig::default(),
                    fullscreen: false,
                    applied: false,
                },
            );
        h.run_steps(3);
        save_snapshot(h.render().unwrap(), "client-stream-toolbar.png");
    }
}
