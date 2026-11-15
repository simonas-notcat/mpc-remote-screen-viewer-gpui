//! Local-only teacher opt-in and non-interactive device-coordinate annotations.
use super::*;
use gpui::{TextRun, fill, outline, rgba, size};
use gpui_component::{Sizable, button::ButtonVariants};
use mpc_remote_screen_viewer_gpui::{teacher as core, viewport::Picture};

impl Viewer {
    pub(super) fn toggle_teacher(&mut self, _: &AiTeacher, _: &mut Window, cx: &mut Context<Self>) {
        self.teacher_highlight = None;
        if let Some(bridge) = self.teacher.take() {
            bridge.request_stop();
            cx.background_executor()
                .spawn(async move { drop(bridge) })
                .detach();
            self.message = Some("AI teacher sharing disabled. Local highlights cleared.".into());
        } else {
            match TeacherBridge::start(core::default_socket_path()) {
                Ok(bridge) => {
                    if let Some(session) = &self.session {
                        bridge.teacher().set_source(Some(session.snapshot_source()));
                    }
                    self.message = Some(format!(
                        "AI teacher enabled (read only). Screen captures can reach your configured model. Click the AI-teacher indicator to disable. Socket: {}",
                        bridge.path().display()
                    ));
                    self.teacher = Some(bridge);
                }
                Err(error) => self.message = Some(format!("Could not enable AI teacher: {error}")),
            }
        }
        cx.notify();
    }
    pub(super) fn teacher_indicator(&self, cx: &mut Context<Self>) -> Button {
        Button::new("disable-ai-teacher")
            .ghost()
            .small()
            .h(px(24.0))
            .label("AI teacher · read only · ×")
            .tooltip("Disable AI-teacher screen sharing and clear its highlights")
            .on_click(
                cx.listener(|view, _, window, cx| view.toggle_teacher(&AiTeacher, window, cx)),
            )
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

    let label: gpui::SharedString = highlight.label.chars().take(90).collect::<String>().into();
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
