//! The stream screen: the PC's picture, letterboxed, with a toolbar in the
//! bar above it and a status line in the bar below. In full screen on a
//! display whose shape matches the stream, the toolbar slides in from the
//! top edge instead.

use crate::config::ClientConfig;
use crate::input::{self, Held};
use crate::session::Live;
use crate::video;
use brolink_core::api::PowerAction;
use brolink_ui::{self as ui, Tone, PALETTE as P};
use egui::{
    Align, Color32, CursorIcon, Event, Frame, Id, Layout, Margin, Pos2, Rect, RichText, Vec2,
    ViewportCommand,
};
use std::time::{Duration, Instant};

const BAR: f32 = 36.0;
const STATUS: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Disconnect,
    Power(PowerAction),
    Fullscreen(bool),
    ToggleCmd,
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
    poor: bool,
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
            poor: false,
        }
    }
}

impl View {
    pub fn set_poor(&mut self, poor: bool) {
        self.poor = poor;
    }

    /// Release everything and let the cursor go; called when the stream
    /// ends or the window loses focus.
    pub fn reset(&mut self, ctx: &egui::Context, live: Option<&Live>) {
        if let Some(l) = live {
            self.held.release_all(&l.session);
        }
        self.set_captured(ctx, false);
        self.confirm = None;
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

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        live: &Live,
        cfg: &ClientConfig,
        fullscreen: bool,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let screen = ctx.screen_rect();
        let stats = live.session.stats();
        let (vw, vh) = if stats.width > 0 {
            (stats.width as f32, stats.height as f32)
        } else {
            (live.requested.0 as f32, live.requested.1 as f32)
        };

        // Where the picture goes: everything, or everything under the bar.
        let area = if fullscreen {
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
        let bar_rect = if !fullscreen || top_gap >= BAR - 6.0 {
            Some(Rect::from_min_size(
                screen.min,
                Vec2::new(screen.width(), BAR.min(top_gap.max(BAR))),
            ))
        } else {
            let at_edge = pointer.is_some_and(|p| p.y <= 2.0) && !self.captured;
            if at_edge {
                self.bar_until = now + Duration::from_millis(1500);
            }
            let hovering = pointer.is_some_and(|p| p.y <= BAR) && !self.captured;
            (self.bar_until > now || hovering)
                .then(|| Rect::from_min_size(screen.min, Vec2::new(screen.width(), BAR)))
        };
        let floating = fullscreen && top_gap < BAR - 6.0;

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
                        format!("Connecting to {}…", live.pc),
                        egui::FontId::proportional(15.0),
                        P.muted,
                    );
                }
                if bottom_gap >= STATUS || self.stats {
                    self.status_line(ui, screen, video, live, &stats, bottom_gap >= STATUS);
                }
            });

        if let Some(rect) = bar_rect {
            self.toolbar(ctx, rect, floating, live, cfg, fullscreen, &mut actions);
        }

        self.input(ctx, live, cfg, video, bar_rect);
        actions
    }

    #[allow(clippy::too_many_arguments)]
    fn toolbar(
        &mut self,
        ctx: &egui::Context,
        rect: Rect,
        floating: bool,
        live: &Live,
        cfg: &ClientConfig,
        fullscreen: bool,
        actions: &mut Vec<Action>,
    ) {
        let frame = if floating {
            ui::overlay_frame()
                .corner_radius(0)
                .inner_margin(Margin::symmetric(12, 0))
        } else {
            Frame::new()
                .fill(Color32::BLACK)
                .inner_margin(Margin::symmetric(12, 0))
        };
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
                        let tone = if !live.session.connected() {
                            Tone::Accent
                        } else if self.poor {
                            Tone::Danger
                        } else {
                            Tone::Success
                        };
                        ui.label(RichText::new("●").size(9.0).color(tone.color()));
                        ui.label(RichText::new(&live.pc).font(brolink_ui::theme::medium(13.5)).color(P.text));
                        let s = live.session.stats();
                        let (w, h) = if s.width > 0 { (s.width, s.height) } else { (live.requested.0, live.requested.1) };
                        ui::caption(ui, format!("{w}×{h} · {} · {} fps", live.codec, live.requested.2));
                        if self.captured {
                            ui::caption(ui, "Ctrl+Alt frees the mouse");
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui::danger_button(ui, "Disconnect").clicked() {
                                actions.push(Action::Disconnect);
                            }
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
                            });
                            if ui::ghost_button(ui, if fullscreen { "Exit full screen" } else { "Full screen" }).clicked() {
                                actions.push(Action::Fullscreen(!fullscreen));
                            }
                            if ui::ghost_button(ui, if self.stats { "Hide stats" } else { "Stats" }).clicked() {
                                self.stats = !self.stats;
                            }
                            ui::menu_button(ui, "Keys", |ui| {
                                let s = &live.session;
                                if ui.button("Ctrl+Alt+Del").clicked() {
                                    self.held.chord(s, &[input::VK_CONTROL, input::VK_MENU, input::VK_DELETE]);
                                    ui.close_menu();
                                }
                                if ui.button("Windows key").clicked() {
                                    self.held.chord(s, &[input::VK_LWIN]);
                                    ui.close_menu();
                                }
                                if ui.button("Alt+Tab").clicked() {
                                    self.held.chord(s, &[input::VK_MENU, input::VK_TAB]);
                                    ui.close_menu();
                                }
                                if ui.button("Esc").clicked() {
                                    self.held.chord(s, &[input::VK_ESCAPE]);
                                    ui.close_menu();
                                }
                                if ui.button("Print Screen").clicked() {
                                    self.held.chord(s, &[input::VK_SNAPSHOT]);
                                    ui.close_menu();
                                }
                            });
                            let cmd = if cfg.cmd_is_ctrl { "⌘ = Ctrl" } else { "⌘ = Win" };
                            if ui::ghost_button(ui, cmd).on_hover_text("What the Command key does on the PC").clicked() {
                                actions.push(Action::ToggleCmd);
                            }
                            let label = if self.captured { "Mouse: captured" } else { "Mouse: free" };
                            if ui::ghost_button(ui, label)
                                .on_hover_text("Captured hides the cursor and sends raw movement, for games. Ctrl+Alt toggles.")
                                .clicked()
                            {
                                self.toggle_capture(ctx, live);
                            }
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
    }

    fn status_line(
        &self,
        ui: &mut egui::Ui,
        screen: Rect,
        video: Rect,
        live: &Live,
        stats: &brolink_stream::Stats,
        in_gap: bool,
    ) {
        let secs = live.started.elapsed().as_secs();
        let mut text = format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60
        );
        if stats.fps > 0.0 {
            text.push_str(&format!(
                "   {:.0} fps   {:.1} Mbps   {} ms rtt   {:.1} ms decode   {}",
                stats.fps, stats.mbps, stats.rtt_ms, stats.decode_ms, stats.decoder
            ));
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
        self.held.release_all(&live.session);
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
        let session = &live.session;
        let (events, modifiers, pointer, focused) = ctx.input(|i| {
            (
                i.events.clone(),
                i.modifiers,
                i.pointer.latest_pos(),
                i.focused,
            )
        });
        let popup = ctx.memory(|m| m.any_popup_open());
        let over_bar = pointer.is_some_and(|p| bar.is_some_and(|b| b.contains(p)));
        let over_video = pointer.is_some_and(|p| video.contains(p)) && !over_bar && !popup;
        let keys_to_pc = focused && !ctx.wants_keyboard_input() && !popup && session.connected();
        let ppp = ctx.pixels_per_point();

        if !focused {
            if self.held.any_down() || self.captured {
                self.reset(ctx, Some(live));
            }
            return;
        }
        if keys_to_pc {
            let ctrl_alt = modifiers.ctrl && modifiers.alt;
            let was = self.held_ctrl_alt();
            self.held.modifiers(session, modifiers, cfg.cmd_is_ctrl);
            if ctrl_alt && !was {
                self.toggle_capture(ctx, live);
                return;
            }
        } else if self.held.any_down() {
            self.held.release_all(session);
        }

        if (over_video || self.captured) && session.connected() {
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
                } if keys_to_pc && !repeat => {
                    if let Some(vk) = physical_key.and_then(input::vk).or_else(|| input::vk(key)) {
                        self.held.key(session, vk, pressed);
                    }
                }
                Event::Copy if keys_to_pc => self.clipboard_chord(session, input::VK_C, cfg),
                Event::Cut if keys_to_pc => self.clipboard_chord(session, input::VK_X, cfg),
                Event::Paste(_) if keys_to_pc => self.clipboard_chord(session, input::VK_V, cfg),
                Event::PointerMoved(p) if !self.captured && over_video => position = Some(p),
                Event::MouseMoved(d) if self.captured => {
                    self.motion.0 += d.x;
                    self.motion.1 += d.y;
                }
                Event::PointerButton {
                    button, pressed, ..
                } if self.captured || over_video || (!pressed && self.held.any_down()) => {
                    if let Some(b) = input::button(button) {
                        self.held.button(session, b, pressed);
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
            session.mouse_position(
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
            session.mouse_move(dx as i16, dy as i16);
        }
        if self.scroll.0.abs() >= 1.0 || self.scroll.1.abs() >= 1.0 {
            let h = self.scroll.0.trunc();
            let v = self.scroll.1.trunc();
            self.scroll.0 -= h;
            self.scroll.1 -= v;
            session.scroll(
                v.clamp(-32000.0, 32000.0) as i16,
                h.clamp(-32000.0, 32000.0) as i16,
            );
        }
    }

    fn held_ctrl_alt(&self) -> bool {
        self.held.modifiers_now().ctrl && self.held.modifiers_now().alt
    }

    /// egui swallows Cmd/Ctrl+C/X/V into clipboard events; replay them as
    /// the key chord the PC expects.
    fn clipboard_chord(&mut self, session: &brolink_stream::Session, vk: i16, cfg: &ClientConfig) {
        let modifier = if cfg!(target_os = "macos") && !cfg.cmd_is_ctrl {
            input::VK_LWIN
        } else {
            input::VK_CONTROL
        };
        self.held.chord(session, &[modifier, vk]);
    }
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
}
