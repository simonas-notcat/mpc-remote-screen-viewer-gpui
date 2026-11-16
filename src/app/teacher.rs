//! MCP sharing/assistance state and non-interactive device-coordinate feedback.
use super::*;
use gpui::{TextRun, fill, outline, rgba, size};
use mpc_remote_screen_viewer_gpui::{
    control::{Feedback, Phase},
    teacher as core,
    viewport::Picture,
};

impl Viewer {
    pub(super) fn set_assistance_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        // A stale switch callback must never enable input after MCP was switched off.
        let enabled = enabled && self.teacher.is_some();
        self.assistance_on = enabled;
        if let Some(session) = &self.session {
            session.control().set_enabled(enabled);
        }
        self.teacher_control = self
            .teacher
            .as_ref()
            .and_then(|b| b.teacher().control())
            .map(|c| c.status());
        cx.notify();
    }
    pub(super) fn toggle_mcp(&mut self, _: &ToggleMcp, _: &mut Window, cx: &mut Context<Self>) {
        self.teacher_highlight = None;
        self.teacher_control = None;
        if let Some(bridge) = self.teacher.take() {
            self.set_assistance_enabled(false, cx);
            bridge.request_stop();
            cx.background_executor()
                .spawn(async move { drop(bridge) })
                .detach();
        } else {
            match TeacherBridge::start(core::default_socket_path()) {
                Ok(bridge) => {
                    if let Some(session) = &self.session {
                        bridge.teacher().set_session(session);
                    }
                    self.teacher = Some(bridge);
                }
                Err(error) => self.message = Some(format!("Could not enable MCP sharing: {error}")),
            }
        }
        cx.notify();
    }
}

/// Paint inside the existing image canvas: no hitbox, no mutation of device pixels.
pub(super) fn paint_highlight(
    highlight: &TeacherHighlight,
    image_bounds: Bounds<Pixels>,
    canvas_bounds: Bounds<Pixels>,
    upright: (u16, u16),
    window: &mut Window,
    cx: &mut App,
) {
    let projected = core::project_bounds(
        highlight.bounds,
        Picture {
            left: image_bounds.origin.x.into(),
            top: image_bounds.origin.y.into(),
            width: image_bounds.size.width.into(),
            height: image_bounds.size.height.into(),
        },
        upright,
    );
    let bounds = Bounds::new(
        point(px(projected.left), px(projected.top)),
        size(px(projected.width), px(projected.height)),
    );
    if bounds.right() < canvas_bounds.left()
        || bounds.left() > canvas_bounds.right()
        || bounds.bottom() < canvas_bounds.top()
        || bounds.top() > canvas_bounds.bottom()
    {
        return;
    }
    // Dark backing + cyan edge stay legible over light and dark device controls.
    let mut backing = outline(bounds, rgb(0x06131b), Default::default());
    backing.border_widths = px(5.0).into();
    window.paint_quad(backing);
    let mut edge = outline(bounds, rgb(0x59dcff), Default::default());
    edge.border_widths = px(2.0).into();
    window.paint_quad(edge);

    paint_label(&highlight.label, bounds, canvas_bounds, window, cx);
}

/// Distinct agent cursor/path. This canvas has no event handlers or hitboxes.
pub(super) fn paint_feedback(
    feedback: &Feedback,
    picture: Bounds<Pixels>,
    canvas: Bounds<Pixels>,
    upright: (u16, u16),
    window: &mut Window,
    cx: &mut App,
) {
    if !feedback.overlay_visible
        || feedback
            .visible_until
            .is_some_and(|until| until <= Instant::now())
    {
        return;
    }
    let color = match feedback.phase {
        Phase::Queued => 0xffc45c,
        Phase::Running => 0x59dcff,
        Phase::Completed => 0x72ffa6,
        Phase::Rejected | Phase::Cancelled | Phase::Uncertain => 0xff7373,
    };
    let alpha = feedback
        .visible_until
        .map(|until| {
            (until
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
                / 2.0
                * 255.0) as u32
        })
        .unwrap_or(255);
    let color = rgba((color << 8) | alpha);
    let map = |p: mpc_remote_screen_viewer_gpui::control::Point| {
        let projected = core::project_bounds(
            core::Rect {
                x: p.x,
                y: p.y,
                width: 1,
                height: 1,
            },
            Picture {
                left: picture.origin.x.into(),
                top: picture.origin.y.into(),
                width: picture.size.width.into(),
                height: picture.size.height.into(),
            },
            upright,
        );
        point(px(projected.left), px(projected.top))
    };
    let mut path = PathBuilder::stroke(px(2.0));
    // Only positions actually written by the worker become the traversed path.
    if let Some(start) = feedback.path.first() {
        path.move_to(map(*start));
        for p in feedback.path.iter().skip(1) {
            path.line_to(map(*p));
        }
    }
    for marker in [feedback.start, feedback.end].into_iter().flatten() {
        let p = map(marker);
        window.paint_quad(outline(
            Bounds::new(p - point(px(4.0), px(4.0)), size(px(8.0), px(8.0))),
            color,
            Default::default(),
        ));
    }
    let anchor = if let Some(position) = feedback.position {
        let p = map(position);
        path.move_to(p - point(px(10.0), px(0.0)));
        path.line_to(p + point(px(10.0), px(0.0)));
        path.move_to(p - point(px(0.0), px(10.0)));
        path.line_to(p + point(px(0.0), px(10.0)));
        if feedback.held || (feedback.gesture == "click" && feedback.written_steps > 0) {
            window.paint_quad(
                outline(
                    Bounds::new(p - point(px(13.0), px(13.0)), size(px(26.0), px(26.0))),
                    color,
                    Default::default(),
                )
                .corner_radii(px(13.0))
                .border_widths(px(if feedback.held { 3.0 } else { 1.0 })),
            );
        }
        if let Some(amount) = feedback.scroll_amount {
            let dy = if amount > 0 { -22.0 } else { 22.0 };
            let tip = p + point(px(0.0), px(dy));
            path.move_to(p);
            path.line_to(tip);
            path.move_to(tip + point(px(-5.0), px(-dy.signum() * 6.0)));
            path.line_to(tip);
            path.line_to(tip + point(px(5.0), px(-dy.signum() * 6.0)));
        }
        Bounds::new(p, size(px(1.0), px(1.0)))
    } else {
        Bounds::new(
            canvas.origin + point(px(12.0), px(40.0)),
            size(px(1.0), px(1.0)),
        )
    };
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
    let state = if feedback.cancel_requested && feedback.phase.active() {
        "STOP requested · release pending".into()
    } else {
        format!("{:?}", feedback.phase)
    };
    paint_label(
        &format!(
            "AI agent · {state} · {}{}",
            feedback.summary,
            if feedback.held {
                " · HELD"
            } else if feedback.release_written {
                " · released"
            } else {
                ""
            }
        ),
        anchor,
        canvas,
        window,
        cx,
    );
}

fn paint_label(
    label: &str,
    bounds: Bounds<Pixels>,
    canvas_bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let label: gpui::SharedString = label.chars().take(110).collect::<String>().into();
    let run = TextRun {
        len: label.len(),
        font: window.text_style().font(),
        color: rgb(0xa5edff).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(label, px(12.0), &[run], None);
    let width = line.width + px(12.0);
    let x = bounds.left().clamp(
        canvas_bounds.left(),
        (canvas_bounds.right() - width).max(canvas_bounds.left()),
    );
    let y = (bounds.top() - px(25.0)).clamp(
        canvas_bounds.top(),
        (canvas_bounds.bottom() - px(24.0)).max(canvas_bounds.top()),
    );
    window.paint_quad(fill(
        Bounds::new(point(x, y), size(width, px(24.0))),
        rgba(0x07131bf0),
    ));
    let _ = line.paint(point(x + px(6.0), y + px(3.0)), px(18.0), window, cx);
}
