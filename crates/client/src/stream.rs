//! The stream screen: the PC's picture, letterboxed, with a toolbar in the
//! bar above it and a status line in the bar below. In full screen on a
//! display whose shape matches the stream, the toolbar slides in from the
//! top edge instead. Short notices (clipboard, connection, an install in
//! progress) appear under the toolbar and fade.

use crate::clipboard;
use crate::config::{ClientConfig, Preset, Quality};
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
const STATUS: f32 = 24.0;
const TOAST_FOR: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Disconnect,
    RestartStream,
    Power(PowerAction),
    Fullscreen(bool),
    ToggleCmd,
    /// Reconnect at this quality.
    Quality(QualityChoice),
    /// Install a new BroLink Host on the PC through the stream.
    InstallHost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityChoice {
    Auto,
    Preset(Preset),
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
}

struct Toast {
    tone: Tone,
    text: String,
    at: Instant,
}

pub struct View {
    captured: bool,
    grabbed: bool,
    stats: bool,
    held: Held,
    scroll: (f32, f32),
    motion: (f32, f32),
    last_pointer: Instant,
    bar_until: Instant,
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
            stats: false,
            held: Held::default(),
            scroll: (0.0, 0.0),
            motion: (0.0, 0.0),
            last_pointer: Instant::now(),
            bar_until: Instant::now() + Duration::from_secs(3),
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
                "The connection is struggling. Quality → Smooth asks less of it.",
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
        self.confirm = None;
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
            ctx.send_viewport_cmd(ViewportCommand::CursorVisible(!on));
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

        // Where the picture goes: everything, or everything under the bar.
        let area = if env.fullscreen {
            screen
        } else {
            Rect::from_min_max(Pos2::new(screen.left(), screen.top() + BAR), screen.max)
        };
        let video = fit(area, vw / vh);
        let top_gap = video.top() - screen.top();
        let bottom_gap = screen.bottom() - video.bottom();

        let pointer = ctx.input(|i| i.pointer.latest_pos());
        let now = Instant::now();
        if ctx.input(|i| i.pointer.is_moving()) {
            self.last_pointer = now;
        }
        // In full screen without a spare bar, the toolbar shows when the
        // pointer touches the top edge and stays a moment after it leaves.
        let bar_rect = if !env.fullscreen || top_gap >= BAR - 6.0 {
            Some(Rect::from_min_size(
                screen.min,
                Vec2::new(screen.width(), BAR.min(top_gap.max(BAR))),
            ))
        } else {
            let at_edge = pointer.is_some_and(|p| p.y <= 4.0) && !self.captured;
            if at_edge {
                self.bar_until = now + Duration::from_millis(2500);
            }
            let hovering = pointer.is_some_and(|p| p.y <= BAR + 4.0) && !self.captured;
            let popup = ctx.memory(|m| m.any_popup_open());
            (self.bar_until > now || hovering || popup || self.confirm.is_some())
                .then(|| Rect::from_min_size(screen.min, Vec2::new(screen.width(), BAR)))
        };
        let floating = env.fullscreen && top_gap < BAR - 6.0;

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
                        egui::Spinner::new().size(22.0).color(P.accent),
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
            self.toolbar(ctx, rect, floating, env, &mut actions);
        }
        self.toasts(
            ctx,
            screen,
            bar_rect,
            env,
            stats.video_problem.as_deref(),
            &mut actions,
        );

        self.input(ctx, live, env.cfg, video, bar_rect);
        actions
    }

    fn toolbar(
        &mut self,
        ctx: &egui::Context,
        rect: Rect,
        floating: bool,
        env: &Env<'_>,
        actions: &mut Vec<Action>,
    ) {
        let live = env.live;
        let cfg = env.cfg;
        let frame = if floating {
            ui::overlay_frame()
                .corner_radius(0)
                .inner_margin(Margin::symmetric(12, 0))
        } else {
            Frame::new()
                .fill(Color32::BLACK)
                .inner_margin(Margin::symmetric(12, 0))
        };
        let path = env.path.clone().unwrap_or_else(|| live.path.clone());
        egui::Area::new(Id::new("stream-toolbar"))
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_min_size(rect.size());
                ui.set_max_size(rect.size());
                frame.show(ui, |ui| {
                    ui.set_min_height(rect.height());
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
                                "Packets go through a Tailscale relay, not straight to the PC. The lobby explains why and what would fix it.",
                            );
                        } else if path.direct == Some(true) {
                            pill.on_hover_text("Packets go straight to the PC.");
                        }
                        if self.captured {
                            ui::caption(ui, "Ctrl+Alt frees the mouse");
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui::danger_button(ui, "Disconnect").clicked() {
                                actions.push(Action::Disconnect);
                            }
                            self.pc_menu(ui, env, actions);
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
                            if ui::ghost_button(ui, if self.stats { "Hide stats" } else { "Stats" })
                                .on_hover_text("Frame rate, bitrate, round trip, loss and decode time in the status line")
                                .clicked()
                            {
                                self.stats = !self.stats;
                            }
                            self.keys_menu(ui, live, cfg, actions);
                            self.quality_menu(ui, live, cfg, actions);
                            self.mouse_menu(ui, ctx, live);
                        });
                    });
                });
            });

        if let Some(action) = self.confirm {
            egui::Area::new(Id::new("stream-confirm"))
                .fixed_pos(Pos2::new(rect.center().x - 220.0, rect.bottom() + 8.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(440.0);
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
            egui::Area::new(Id::new("stream-install"))
                .fixed_pos(Pos2::new(rect.center().x - 250.0, rect.bottom() + 8.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(500.0);
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

    fn quality_menu(
        &mut self,
        ui: &mut egui::Ui,
        live: &Live,
        cfg: &ClientConfig,
        actions: &mut Vec<Action>,
    ) {
        let label = format!("Quality: {}", short_quality(live));
        let current = if cfg.stream.quality == Quality::Auto {
            QualityChoice::Auto
        } else {
            cfg.stream
                .preset()
                .map(QualityChoice::Preset)
                .unwrap_or(QualityChoice::Auto)
        };
        let custom = cfg.stream.quality == Quality::Custom && cfg.stream.preset().is_none();
        ui::menu_button(ui, &label, |ui| {
            ui::caption(ui, "Changing this reconnects in a few seconds.");
            let mark = |on: bool| if on { "● " } else { "   " };
            let auto = format!(
                "{}Auto · picks from the path ({})",
                mark(current == QualityChoice::Auto && !custom),
                live.path.label()
            );
            if ui.button(auto).clicked() {
                actions.push(Action::Quality(QualityChoice::Auto));
                ui.close_menu();
            }
            for p in Preset::ALL {
                let on = !custom && current == QualityChoice::Preset(p);
                let text = format!("{}{} · {}", mark(on), p.label(), p.describe());
                if ui.button(text).clicked() {
                    actions.push(Action::Quality(QualityChoice::Preset(p)));
                    ui.close_menu();
                }
            }
            if custom {
                ui::caption(
                    ui,
                    format!("● Custom · {} (from Settings)", cfg.stream.describe()),
                );
            }
        });
    }

    fn mouse_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, live: &Live) {
        let label = if self.captured {
            "Mouse: captured"
        } else {
            "Mouse: free"
        };
        ui::menu_button(ui, label, |ui| {
            let mark = |on: bool| if on { "● " } else { "   " };
            if ui
                .button(format!(
                    "{}Free · the Mac cursor moves 1:1 on the PC",
                    mark(!self.captured)
                ))
                .clicked()
            {
                if self.captured {
                    self.toggle_capture(ctx, live);
                }
                ui.close_menu();
            }
            if ui
                .button(format!(
                    "{}Captured · raw movement, for games (Ctrl+Alt frees it)",
                    mark(self.captured)
                ))
                .clicked()
            {
                if !self.captured {
                    self.toggle_capture(ctx, live);
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
        egui::Area::new(Id::new("stream-toasts"))
            .fixed_pos(Pos2::new(screen.center().x - 260.0, top))
            .order(egui::Order::Foreground)
            .interactable(video_problem.is_some())
            .show(ctx, |ui| {
                ui.set_width(520.0);
                if let Some(problem) = video_problem {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(496.0);
                        ui.label(RichText::new(problem).color(P.text));
                        if ui::ghost_button(ui, "Restart stream").clicked() {
                            actions.push(Action::RestartStream);
                        }
                    });
                }
                for (tone, text) in lines {
                    ui::overlay_frame().show(ui, |ui| {
                        ui.set_width(496.0);
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
        if stats.fps > 0.0 {
            text.push_str(&format!(
                "   {:.0} fps   {:.1} Mbps   {} ms rtt   {:.1}% loss   {:.1} ms decode   {}",
                stats.fps, stats.mbps, stats.rtt_ms, stats.loss_pct, stats.decode_ms, stats.decoder
            ));
        }
        if self.stats {
            let path = env.path.clone().unwrap_or_else(|| live.path.clone());
            text.push_str(&format!("   {}   {}", path.label(), live.quality_label()));
            if !stats.audio.is_empty() {
                text.push_str(&format!("   {}", stats.audio));
            }
        }
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

    fn toggle_capture(&mut self, ctx: &egui::Context, live: &Live) {
        let on = !self.captured;
        self.held.release_all(&live.input);
        self.set_captured(ctx, on);
        if on {
            self.bar_until = Instant::now();
        }
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
        let overlay = self.confirm.is_some() || self.confirm_install;
        let over_bar = pointer.is_some_and(|p| bar.is_some_and(|b| b.contains(p)));
        let over_overlay = pointer.is_some_and(|p| {
            ctx.layer_id_at(p)
                .is_some_and(|layer| layer.order > egui::Order::Background)
        });
        let over_video = pointer.is_some_and(|p| video.contains(p))
            && !over_bar
            && !popup
            && !overlay
            && !over_overlay;
        let keys_to_pc =
            focused && !ctx.wants_keyboard_input() && !popup && !overlay && input.connected();
        let ppp = ctx.pixels_per_point();

        if !focused {
            if self.held.any_down() || self.captured {
                self.reset(ctx, Some(live));
            }
            return;
        }
        if keys_to_pc {
            let ctrl_alt = modifiers.ctrl && modifiers.alt;
            if !ctrl_alt {
                self.capture_chord_held = false;
            }
            self.held.modifiers(input, modifiers, cfg.cmd_is_ctrl);
            if ctrl_alt && !self.capture_chord_held {
                self.capture_chord_held = true;
                self.toggle_capture(ctx, live);
                return;
            }
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

/// "Auto · Smooth" → "Auto", "Custom · Sharp" → "Sharp", else "Custom".
fn short_quality(live: &Live) -> String {
    match (live.settings.quality, live.settings.preset()) {
        (Quality::Auto, _) => "Auto".into(),
        (Quality::Custom, Some(p)) => p.label().into(),
        (Quality::Custom, None) => "Custom".into(),
    }
}

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

    #[test]
    fn fit_letterboxes_a_16_9_stream_on_a_16_10_screen() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1512.0, 982.0));
        let v = fit(screen, 16.0 / 9.0);
        assert_eq!(v.width(), 1512.0);
        assert!((v.height() - 850.0).abs() <= 1.0);
        assert!(
            v.top() > 60.0,
            "there is a bar for the toolbar: {}",
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
}
