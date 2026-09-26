//! Roblox-style transform gizmos drawn on top of the editor preview.

use crate::editor::{EditorMessage, Tool};
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Fill, Point, Rectangle, Renderer, Size, Theme};

/// Which part of the gizmo is under the cursor / being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoHandle {
    MoveFree,
    MoveX,
    MoveY,
    ScaleUniform,
    ScaleX,
    ScaleY,
    Rotate,
}

#[derive(Debug, Clone, Default)]
pub struct GizmoState {
    pub active: Option<GizmoHandle>,
}

/// Drawn overlay: axes + handles + hit testing → editor messages.
#[derive(Debug, Clone)]
pub struct GizmoOverlay {
    pub tool: Tool,
    pub has_selection: bool,
    /// Pivot in **widget** coordinates (Contain-fit space of the preview).
    pub pivot: Point,
    /// Ortho size of the scene (for mapping).
    pub ortho: [f32; 2],
    /// Axis length in screen pixels.
    pub axis_len: f32,
}

impl GizmoOverlay {
    pub fn view(self) -> Element<'static, EditorMessage> {
        Canvas::new(self).width(Fill).height(Fill).into()
    }

    fn handle_hit(&self, cursor: Point) -> Option<GizmoHandle> {
        if !self.has_selection {
            return None;
        }
        let p = self.pivot;
        let len = self.axis_len;
        let hit_r = 12.0_f32;

        match self.tool {
            Tool::Select => None,
            Tool::Move => {
                // Center free-move
                if dist(cursor, p) <= hit_r {
                    return Some(GizmoHandle::MoveFree);
                }
                // X axis tip (right)
                let x_tip = Point::new(p.x + len, p.y);
                if dist(cursor, x_tip) <= hit_r || near_segment(cursor, p, x_tip, 8.0) {
                    return Some(GizmoHandle::MoveX);
                }
                // Positive authored Y points up on the preview.
                let y_tip = Point::new(p.x, p.y - len);
                if dist(cursor, y_tip) <= hit_r || near_segment(cursor, p, y_tip, 8.0) {
                    return Some(GizmoHandle::MoveY);
                }
                None
            }
            Tool::Scale => {
                let x_tip = Point::new(p.x + len, p.y);
                let y_tip = Point::new(p.x, p.y - len);
                let corner = Point::new(p.x + len * 0.75, p.y + len * 0.75);
                if dist(cursor, corner) <= hit_r {
                    return Some(GizmoHandle::ScaleUniform);
                }
                if dist(cursor, x_tip) <= hit_r {
                    return Some(GizmoHandle::ScaleX);
                }
                if dist(cursor, y_tip) <= hit_r {
                    return Some(GizmoHandle::ScaleY);
                }
                if dist(cursor, p) <= hit_r {
                    return Some(GizmoHandle::ScaleUniform);
                }
                None
            }
            Tool::Rotate => {
                let r = len;
                let d = dist(cursor, p);
                // Ring hit band
                if (d - r).abs() <= 14.0 {
                    return Some(GizmoHandle::Rotate);
                }
                // Grab handle at top of ring
                let grab = Point::new(p.x, p.y - r);
                if dist(cursor, grab) <= hit_r {
                    return Some(GizmoHandle::Rotate);
                }
                None
            }
        }
    }
}

impl canvas::Program<EditorMessage> for GizmoOverlay {
    type State = GizmoState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<EditorMessage>> {
        let cursor_pos = cursor.position_in(bounds)?;

        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | iced::Event::Touch(iced::touch::Event::FingerPressed { .. }) => {
                if let Some(h) = self.handle_hit(cursor_pos) {
                    state.active = Some(h);
                    return Some(
                        canvas::Action::publish(EditorMessage::GizmoPress {
                            handle: h,
                            x: cursor_pos.x,
                            y: cursor_pos.y,
                            w: bounds.width,
                            h: bounds.height,
                        })
                        .and_capture(),
                    );
                }
                // Pass through: empty click for select-tool pick on image underneath
                // (canvas is on top — publish pick at this point)
                if self.tool == Tool::Select || !self.has_selection {
                    return Some(
                        canvas::Action::publish(EditorMessage::CanvasPressAt {
                            x: cursor_pos.x,
                            y: cursor_pos.y,
                            w: bounds.width,
                            h: bounds.height,
                        })
                        .and_capture(),
                    );
                }
                None
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { .. })
            | iced::Event::Touch(iced::touch::Event::FingerMoved { .. }) => {
                if state.active.is_some() {
                    return Some(
                        canvas::Action::publish(EditorMessage::GizmoDrag {
                            x: cursor_pos.x,
                            y: cursor_pos.y,
                            w: bounds.width,
                            h: bounds.height,
                        })
                        .and_capture(),
                    );
                }
                // Hover redraw for handle highlight
                Some(canvas::Action::request_redraw())
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | iced::Event::Touch(iced::touch::Event::FingerLifted { .. }) => {
                if state.active.take().is_some() {
                    return Some(
                        canvas::Action::publish(EditorMessage::GizmoRelease).and_capture(),
                    );
                }
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        if !self.has_selection {
            // Dim hint
            return vec![frame.into_geometry()];
        }

        let p = self.pivot;
        let len = self.axis_len;
        let hover = cursor.position_in(bounds).and_then(|c| self.handle_hit(c));
        let active = state.active.or(hover);

        match self.tool {
            Tool::Select => {
                // Selection marker only
                draw_center(&mut frame, p, 6.0, Color::from_rgb(1.0, 1.0, 1.0));
            }
            Tool::Move => {
                // Y axis (green, down)
                let y_tip = Point::new(p.x, p.y - len);
                let y_hot = matches!(
                    active,
                    Some(GizmoHandle::MoveY) | Some(GizmoHandle::MoveFree)
                );
                draw_axis_arrow(
                    &mut frame,
                    p,
                    y_tip,
                    Color::from_rgb(0.25, 0.85, 0.35),
                    y_hot,
                );
                // X axis (red, right)
                let x_tip = Point::new(p.x + len, p.y);
                let x_hot = matches!(
                    active,
                    Some(GizmoHandle::MoveX) | Some(GizmoHandle::MoveFree)
                );
                draw_axis_arrow(
                    &mut frame,
                    p,
                    x_tip,
                    Color::from_rgb(0.95, 0.30, 0.28),
                    x_hot,
                );
                // Center free move
                let free_hot = matches!(active, Some(GizmoHandle::MoveFree));
                draw_center(
                    &mut frame,
                    p,
                    if free_hot { 9.0 } else { 7.0 },
                    if free_hot {
                        Color::from_rgb(1.0, 0.9, 0.3)
                    } else {
                        Color::from_rgb(0.95, 0.95, 0.95)
                    },
                );
                // Labels
                draw_label(&mut frame, Point::new(x_tip.x + 8.0, x_tip.y - 6.0), "X");
                draw_label(&mut frame, Point::new(y_tip.x + 6.0, y_tip.y + 4.0), "Y");
            }
            Tool::Scale => {
                let x_tip = Point::new(p.x + len, p.y);
                let y_tip = Point::new(p.x, p.y - len);
                let corner = Point::new(p.x + len * 0.75, p.y + len * 0.75);
                // Guides
                stroke_line(&mut frame, p, x_tip, Color::from_rgb(0.95, 0.30, 0.28), 2.0);
                stroke_line(&mut frame, p, y_tip, Color::from_rgb(0.25, 0.85, 0.35), 2.0);
                stroke_line(&mut frame, p, corner, Color::from_rgb(0.4, 0.7, 1.0), 1.5);
                draw_box_handle(
                    &mut frame,
                    x_tip,
                    matches!(active, Some(GizmoHandle::ScaleX)),
                    Color::from_rgb(0.95, 0.30, 0.28),
                );
                draw_box_handle(
                    &mut frame,
                    y_tip,
                    matches!(active, Some(GizmoHandle::ScaleY)),
                    Color::from_rgb(0.25, 0.85, 0.35),
                );
                draw_box_handle(
                    &mut frame,
                    corner,
                    matches!(active, Some(GizmoHandle::ScaleUniform)),
                    Color::from_rgb(0.4, 0.7, 1.0),
                );
                draw_center(&mut frame, p, 6.0, Color::WHITE);
                draw_label(&mut frame, Point::new(x_tip.x + 8.0, x_tip.y - 6.0), "X");
                draw_label(&mut frame, Point::new(y_tip.x + 6.0, y_tip.y + 4.0), "Y");
                draw_label(&mut frame, Point::new(corner.x + 8.0, corner.y + 4.0), "XY");
            }
            Tool::Rotate => {
                let r = len;
                let hot = matches!(active, Some(GizmoHandle::Rotate));
                let ring = Path::circle(p, r);
                frame.stroke(
                    &ring,
                    Stroke::default()
                        .with_color(if hot {
                            Color::from_rgb(1.0, 0.85, 0.2)
                        } else {
                            Color::from_rgb(0.55, 0.75, 1.0)
                        })
                        .with_width(if hot { 3.5 } else { 2.5 }),
                );
                // grab knob at top
                let grab = Point::new(p.x, p.y - r);
                draw_center(
                    &mut frame,
                    grab,
                    if hot { 10.0 } else { 8.0 },
                    if hot {
                        Color::from_rgb(1.0, 0.85, 0.2)
                    } else {
                        Color::from_rgb(0.7, 0.85, 1.0)
                    },
                );
                draw_center(&mut frame, p, 5.0, Color::WHITE);
                draw_label(&mut frame, Point::new(grab.x + 10.0, grab.y - 4.0), "ROT");
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.active.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if let Some(pos) = cursor.position_in(bounds) {
            if self.handle_hit(pos).is_some() {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::default()
    }
}

fn dist(a: Point, b: Point) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

fn near_segment(p: Point, a: Point, b: Point, tol: f32) -> bool {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len2 = abx * abx + aby * aby;
    if len2 < 1.0 {
        return dist(p, a) <= tol;
    }
    let t = ((p.x - a.x) * abx + (p.y - a.y) * aby) / len2;
    let t = t.clamp(0.0, 1.0);
    let proj = Point::new(a.x + abx * t, a.y + aby * t);
    dist(p, proj) <= tol
}

fn stroke_line(frame: &mut Frame, a: Point, b: Point, color: Color, width: f32) {
    let path = Path::line(a, b);
    frame.stroke(&path, Stroke::default().with_color(color).with_width(width));
}

fn draw_axis_arrow(frame: &mut Frame, from: Point, tip: Point, color: Color, hot: bool) {
    let w = if hot { 3.5 } else { 2.5 };
    stroke_line(frame, from, tip, color, w);
    // Arrow head
    let dx = tip.x - from.x;
    let dy = tip.y - from.y;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;
    let back = Point::new(tip.x - ux * 14.0, tip.y - uy * 14.0);
    let left = Point::new(back.x + px * 7.0, back.y + py * 7.0);
    let right = Point::new(back.x - px * 7.0, back.y - py * 7.0);
    let head = Path::new(|b| {
        b.move_to(tip);
        b.line_to(left);
        b.line_to(right);
        b.close();
    });
    frame.fill(&head, color);
}

fn draw_center(frame: &mut Frame, p: Point, r: f32, color: Color) {
    let c = Path::circle(p, r);
    frame.fill(&c, color);
    frame.stroke(
        &c,
        Stroke::default()
            .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.65))
            .with_width(1.5),
    );
}

fn draw_box_handle(frame: &mut Frame, p: Point, hot: bool, color: Color) {
    let s = if hot { 11.0 } else { 9.0 };
    let rect = Path::rectangle(Point::new(p.x - s * 0.5, p.y - s * 0.5), Size::new(s, s));
    frame.fill(&rect, color);
    frame.stroke(
        &rect,
        Stroke::default()
            .with_color(Color::WHITE)
            .with_width(if hot { 2.0 } else { 1.0 }),
    );
}

fn draw_label(frame: &mut Frame, at: Point, s: &str) {
    frame.fill_text(canvas::Text {
        content: s.to_string(),
        position: at,
        color: Color::from_rgb(0.92, 0.92, 0.9),
        size: 12.0.into(),
        ..canvas::Text::default()
    });
}

/// Map ortho pivot → widget point for Contain layout.
pub fn ortho_to_widget(
    ox: f32,
    oy: f32,
    ortho_w: f32,
    ortho_h: f32,
    widget_w: f32,
    widget_h: f32,
) -> Point {
    let s = (widget_w / ortho_w.max(1.0)).min(widget_h / ortho_h.max(1.0));
    let drawn_w = ortho_w * s;
    let drawn_h = ortho_h * s;
    let pad_x = (widget_w - drawn_w) * 0.5;
    let pad_y = (widget_h - drawn_h) * 0.5;
    Point::new(pad_x + ox * s, pad_y + (ortho_h - oy) * s)
}

pub fn screen_scale(ortho_w: f32, ortho_h: f32, widget_w: f32, widget_h: f32) -> f32 {
    (widget_w / ortho_w.max(1.0)).min(widget_h / ortho_h.max(1.0))
}

/// Build overlay element for current editor selection.
pub fn gizmo_element(
    tool: Tool,
    has_selection: bool,
    pivot_ortho: [f32; 2],
    ortho: [f32; 2],
    widget_w: f32,
    widget_h: f32,
) -> Element<'static, EditorMessage> {
    let s = screen_scale(ortho[0], ortho[1], widget_w, widget_h);
    let pivot = ortho_to_widget(
        pivot_ortho[0],
        pivot_ortho[1],
        ortho[0],
        ortho[1],
        widget_w,
        widget_h,
    );
    let axis_len = (72.0_f32).min(widget_w.min(widget_h) * 0.18).max(48.0) * s.max(0.5).min(1.5);
    // axis_len in screen px (not scaled by s again — already screen)
    let axis_len = (72.0_f32).min(widget_w.min(widget_h) * 0.18).max(48.0);

    GizmoOverlay {
        tool,
        has_selection,
        pivot,
        ortho,
        axis_len,
    }
    .view()
}
