use super::*;
use gpui_component::{Icon, Selectable, Sizable, button::ButtonVariants, tooltip::Tooltip};
use mpc_remote_screen_viewer_gpui::{
    analyzer::{Analyzer, BANDS},
    spectrogram::{self, SECONDS},
    waveform,
};

const WAVEFORM_SCALE_WIDTH: f32 = 38.0;

pub(super) use preferences::VisualizerMode as Mode;

impl Viewer {
    pub(super) fn toggle_analyzer(
        &mut self,
        _: &AudioAnalyzer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.analyzer.is_some() {
            self.analyzer = None;
        } else {
            match Analyzer::new() {
                Ok(analyzer) => self.analyzer = Some(analyzer),
                Err(error) => {
                    self.message = Some(format!("Could not start analyzer: {error}"));
                    cx.notify();
                    return;
                }
            }
        }
        if let Some(old) = self.spectrogram_image.take() {
            cx.drop_image(old, Some(window));
        }
        if let Some(old) = self.vectorscope_image.take() {
            cx.drop_image(old, Some(window));
        }
        self.spectrogram_history = None;
        self.analysis = Default::default();
        self.resizing_analyzer = false;
        if let Some(session) = &self.session {
            session
                .audio()
                .set_analyzer(self.analyzer.as_ref().map(|a| a.input.clone()));
            if session
                .command(Command::Audio(
                    self.audio_settings.enabled || self.analyzer.is_some(),
                ))
                .is_err()
            {
                session.request_stop();
                self.message = Some("Audio command queue full; reconnect to retry.".into());
            }
        }
        self.remember_settings(cx);
    }

    pub(super) fn analyzer_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let upper = if self.analysis.sample_rate != 0 {
            (self.analysis.sample_rate as f32 / 2.0).min(20000.0)
        } else {
            20000.0
        };
        div()
            .h(px(self.analyzer_height))
            .max_h(px(500.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(rgb(theme::BACKGROUND))
            .px_3()
            .pb_2()
            .gap_1()
            .child(
                div()
                    .h(px(7.0))
                    .flex_shrink_0()
                    .cursor_ns_resize()
                    .bg(rgb(0x444444))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.resizing_analyzer = true;
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .flex_shrink_0()
                    .gap_1()
                    .justify_between()
                    .items_center()
                    .child(
                        div().flex().flex_wrap().gap_1().children(
                            [
                                (Mode::Spectrum, "analyzer", "Spectrum"),
                                (Mode::Spectrogram, "spectrogram", "2D Spectrogram"),
                                (Mode::Waterfall, "waterfall", "3D Waterfall"),
                                (Mode::Waveform, "waveform", "Waveform"),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(index, (mode, icon, label))| {
                                Button::new(("analyzer-mode", index))
                                    .ghost()
                                    .small()
                                    .size(px(32.0))
                                    .icon(Icon::default().path(format!("icons/{icon}.svg")))
                                    .tooltip(label)
                                    .selected(self.analyzer_mode == mode)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.analyzer_mode = mode;
                                        view.sync_spectrogram_image(window, cx);
                                        view.remember_settings(cx);
                                    }))
                            }),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Button::new("toggle-vectorscope")
                                    .ghost()
                                    .small()
                                    .size(px(32.0))
                                    .icon(Icon::default().path("icons/stereo-width.svg"))
                                    .tooltip("Stereo width")
                                    .selected(self.show_vectorscope)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.show_vectorscope = !view.show_vectorscope;
                                        view.sync_vectorscope_image(window, cx);
                                        view.remember_settings(cx);
                                    })),
                            )
                            .child(
                                Button::new("close-analyzer")
                                    .ghost()
                                    .small()
                                    .size(px(32.0))
                                    .icon(Icon::default().path("icons/close.svg"))
                                    .tooltip("Close analyzer")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.toggle_analyzer(&AudioAnalyzer, window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_h_0()
                                    .when(self.analyzer_mode == Mode::Spectrogram, |this| {
                                        this.child(
                                            div()
                                                .w(px(42.0))
                                                .flex_shrink_0()
                                                .flex()
                                                .flex_col()
                                                .justify_between()
                                                .text_xs()
                                                .child(frequency_label(upper))
                                                .child(frequency_label((upper * 20.0).sqrt()))
                                                .child("20 Hz"),
                                        )
                                    })
                                    .when(self.analyzer_mode == Mode::Waveform, |this| {
                                        this.child(self.waveform_scale())
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .child(self.analyzer_chart()),
                                    ),
                            )
                            .child(match self.analyzer_mode {
                                Mode::Spectrogram => div()
                                    .text_xs()
                                    .flex()
                                    .justify_between()
                                    .pl(px(42.0))
                                    .child("−10 s")
                                    .child("−5 s")
                                    .child("Now"),
                                Mode::Waveform => div()
                                    .text_xs()
                                    .flex()
                                    .justify_between()
                                    .pl(px(WAVEFORM_SCALE_WIDTH))
                                    .child("−1 s")
                                    .child("−0.5 s")
                                    .child("Now"),
                                _ => {
                                    div()
                                        .text_xs()
                                        .flex()
                                        .justify_between()
                                        .children((0..=4).map(|tick| {
                                            frequency_label(
                                                20.0 * (upper / 20.0).powf(tick as f32 / 4.0),
                                            )
                                        }))
                                }
                            }),
                    )
                    .when(self.show_vectorscope, |this| {
                        this.child(self.vectorscope_panel())
                    })
                    .child(self.analyzer_meters()),
            )
            .into_any_element()
    }

    fn waveform_scale(&self) -> impl IntoElement {
        let channels = self.analysis.channels;
        let count = if channels == 1 { 1 } else { 2 };
        div()
            .w(px(WAVEFORM_SCALE_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .text_size(px(10.0))
            .text_color(rgb(theme::MUTED))
            .text_right()
            .pr_1()
            .children((0..count).map(|channel| {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(format!("{} +1", channel_label(channels, channel)))
                    .child("0")
                    .child("−1")
            }))
    }

    /// Density rasterization/upload happens only while the scope is visible.
    /// Every replacement releases the previous GPU image, including on stalls.
    pub(super) fn sync_vectorscope_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let image = (self.show_vectorscope && !self.analysis.vectorscope.is_empty()).then(|| {
            Arc::new(RenderImage::new([image::Frame::new(
                self.analysis
                    .vectorscope
                    .image(self.analysis.vectorscope_age),
            )]))
        });
        if let Some(old) = std::mem::replace(&mut self.vectorscope_image, image) {
            cx.drop_image(old, Some(window));
        }
    }

    fn vectorscope_panel(&self) -> AnyElement {
        let image = self.vectorscope_image.clone();
        div()
            .id("vectorscope")
            .w(px(self.analyzer_height.clamp(140.0, 280.0)))
            .max_w(gpui::relative(0.3))
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .flex_col()
            .text_size(px(10.0))
            .text_color(rgb(theme::MUTED))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                let width = f32::from(bounds.size.width).max(1.0);
                                let height = f32::from(bounds.size.height).max(1.0);
                                let project = |side, mid| {
                                    let (x, y) = vectorscope_position(side, mid, width, height);
                                    bounds.origin + point(px(x), px(y))
                                };
                                // Internal reference guides, without an outer border.
                                let mut grid = PathBuilder::stroke(px(1.0));
                                grid.move_to(project(0.0, 0.5));
                                grid.line_to(project(0.5, 0.0));
                                grid.line_to(project(0.0, -0.5));
                                grid.line_to(project(-0.5, 0.0));
                                grid.close();
                                for (from, to) in [
                                    ([0.0, -1.0], [0.0, 1.0]),
                                    ([-1.0, 0.0], [1.0, 0.0]),
                                    ([-0.5, -0.5], [0.5, 0.5]),
                                    ([-0.5, 0.5], [0.5, -0.5]),
                                ] {
                                    grid.move_to(project(from[0], from[1]));
                                    grid.line_to(project(to[0], to[1]));
                                }
                                if let Ok(grid) = grid.build() {
                                    window.paint_path(grid, rgb(0x333333));
                                }
                                if let Some(image) = image {
                                    let square = Bounds::from_corners(
                                        project(-1.0, 1.0),
                                        project(1.0, -1.0),
                                    );
                                    let clip =
                                        vectorscope_image_clip(square, window.scale_factor());
                                    if clip.size.width > px(0.0) && clip.size.height > px(0.0) {
                                        window.with_content_mask(
                                            Some(gpui::ContentMask { bounds: clip }),
                                            |window| {
                                                let _ = window.paint_image(
                                                    square,
                                                    Corners::default(),
                                                    image,
                                                    0,
                                                    false,
                                                );
                                            },
                                        );
                                    }
                                }
                            },
                        )
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .w_full()
                            .flex()
                            .justify_between()
                            .px_1()
                            .child("L")
                            .child("Mono")
                            .child("R"),
                    )
                    .when(self.analysis.sample_rate == 0, |this| {
                        this.child(
                            div()
                                .absolute()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child("Waiting for audio"),
                        )
                    }),
            )
            .into_any_element()
    }

    fn analyzer_meters(&self) -> AnyElement {
        let channels = self.analysis.channels;
        // Keep an idle stereo pair visible until the source format arrives.
        let count = if channels == 1 { 1 } else { 2 };
        let peak = self.analysis.peak;
        let rms = self.analysis.rms;
        let available = self.analysis.sample_rate != 0;
        let drops = self.analysis.statistics.dropped_blocks;
        let gaps = self.analysis.statistics.gaps;
        div()
            .id("analyzer-meters")
            .flex()
            .flex_shrink_0()
            .min_h_0()
            .gap_2()
            .text_size(px(10.0))
            .text_color(rgb(theme::MUTED))
            .tooltip(move |window, cx| {
                if !available {
                    return Tooltip::new("Waiting for device audio").build(window, cx);
                }
                let readings = (0..count)
                    .map(|c| {
                        format!(
                            "{}  Peak {:.1} / RMS {:.1} dBFS",
                            channel_label(channels, c),
                            peak[c],
                            rms[c]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Tooltip::new(format!(
                    "RMS fill · peak marker · pre-volume\n{readings}\nDrops: {drops} · Gaps: {gaps}"
                ))
                .build(window, cx)
            })
            .child(
                div()
                    .w(px(22.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .justify_between()
                            .text_right()
                            .child("0")
                            .child("−30")
                            .child("−60"),
                    )
                    // Match the channel captions below the meter tracks.
                    .child(div().h(px(16.0)).flex_shrink_0()),
            )
            .children((0..count).map(|c| {
                let peak_level = meter_level(peak[c]);
                div()
                    .w(px(14.0))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .rounded(px(2.0))
                            .bg(rgb(theme::PANEL))
                            .child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .w_full()
                                    .h(gpui::relative(meter_level(rms[c])))
                                    .bg(rgb(0x66bb6a)),
                            )
                            .when(peak_level > 0.0, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top(gpui::relative(1.0 - peak_level))
                                        .w_full()
                                        .h(px(2.0))
                                        .bg(rgb(if peak[c] >= -0.1 {
                                            theme::DANGER
                                        } else {
                                            theme::TEXT
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .h(px(16.0))
                            .flex_shrink_0()
                            .line_height(px(16.0))
                            .text_center()
                            .child(channel_label(channels, c)),
                    )
            }))
            .into_any_element()
    }

    /// Heatmap rasterization and GPU uploads are only needed in the 2D view.
    /// Keep a source-history key so changing modes also refreshes a quiet stream.
    pub(super) fn sync_spectrogram_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let history = (self.analyzer_mode == Mode::Spectrogram
            && !self.analysis.history.columns.is_empty())
        .then(|| self.analysis.history.clone());
        if match (&self.spectrogram_history, &history) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        } {
            return;
        }
        let image = history
            .as_ref()
            .map(|h| Arc::new(RenderImage::new([image::Frame::new(h.image())])));
        if let Some(old) = std::mem::replace(&mut self.spectrogram_image, image) {
            cx.drop_image(old, Some(window));
        }
        self.spectrogram_history = history;
    }

    fn analyzer_chart(&self) -> AnyElement {
        if self.analysis.sample_rate == 0 {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(rgb(theme::MUTED))
                .child("Waiting for device audio")
                .into_any_element();
        }
        let mode = self.analyzer_mode;
        let spectrum = self.analysis.spectrum;
        let history = self.analysis.history.clone();
        let image = self.spectrogram_image.clone();
        let waveform = self.analysis.waveform.clone();
        let channels = self.analysis.channels;
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let width = f32::from(bounds.size.width).max(1.0);
                let height = f32::from(bounds.size.height).max(1.0);
                match mode {
                    Mode::Waveform => paint_waveform(bounds, channels, &waveform, window),
                    Mode::Spectrogram => {
                        if let Some(image) = image {
                            let _ = window.paint_image(bounds, Corners::default(), image, 0, false);
                        }
                    }
                    Mode::Spectrum => {
                        let mut grid = PathBuilder::stroke(px(1.0));
                        for db in [-72.0, -48.0, -24.0, 0.0] {
                            let y = height * (-db / 90.0);
                            grid.move_to(bounds.origin + point(px(0.0), px(y)));
                            grid.line_to(bounds.origin + point(px(width), px(y)));
                        }
                        for tick in 0..=4 {
                            let x = width * tick as f32 / 4.0;
                            grid.move_to(bounds.origin + point(px(x), px(0.0)));
                            grid.line_to(bounds.origin + point(px(x), px(height)));
                        }
                        if let Ok(grid) = grid.build() {
                            window.paint_path(grid, rgb(0x333333));
                        }
                        let mut path =
                            PathBuilder::stroke(px((width / BANDS as f32 - 1.0).max(1.0)));
                        for (i, db) in spectrum.iter().enumerate() {
                            let x = width * (i as f32 + 0.5) / BANDS as f32;
                            path.move_to(bounds.origin + point(px(x), px(height)));
                            path.line_to(
                                bounds.origin + point(px(x), px(height * (1.0 - level(*db)))),
                            );
                        }
                        if let Ok(path) = path.build() {
                            window.paint_path(path, rgb(0x64b5f6));
                        }
                    }
                    Mode::Waterfall => {
                        let project = |frequency, age, db| {
                            let (x, y) = project(frequency, age, db);
                            bounds.origin + point(px(x * width), px(y * height))
                        };
                        let mut grid = PathBuilder::stroke(px(1.0));
                        for i in 0..=4 {
                            let t = i as f32 / 4.0;
                            grid.move_to(project(0.0, t, -90.0));
                            grid.line_to(project(1.0, t, -90.0));
                            grid.move_to(project(t, 0.0, -90.0));
                            grid.line_to(project(t, 1.0, -90.0));
                        }
                        if let Ok(grid) = grid.build() {
                            window.paint_path(grid, rgb(0x444444));
                        }
                        // Fixed source-time buckets preserve old ridge identity
                        // as the deque rolls and peak-merge rather than discard FFTs.
                        for column in history.waterfall() {
                            let age =
                                ((history.end() - column.at) / SECONDS).clamp(0.0, 1.0) as f32;
                            let mut fill = PathBuilder::fill();
                            let mut line = PathBuilder::stroke(px(1.2));
                            fill.move_to(project(0.0, age, -90.0));
                            for (band, db) in column.spectrum.iter().enumerate() {
                                let p = project(band as f32 / (BANDS - 1) as f32, age, *db);
                                fill.line_to(p);
                                if band == 0 {
                                    line.move_to(p);
                                } else {
                                    line.line_to(p);
                                }
                            }
                            fill.line_to(project(1.0, age, -90.0));
                            fill.close();
                            if let Ok(fill) = fill.build() {
                                window.paint_path(fill, rgb(0x101018));
                            }
                            let peak = column.spectrum.iter().copied().fold(-90.0f32, f32::max);
                            if let Ok(line) = line.build() {
                                window.paint_path(line, rgb(packed_color(peak)));
                            }
                        }
                    }
                }
            },
        )
        .size_full()
        .into_any_element()
    }
}
fn paint_waveform(
    bounds: Bounds<Pixels>,
    channels: u8,
    snapshot: &waveform::Snapshot,
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width).max(1.0);
    let height = f32::from(bounds.size.height).max(1.0);
    let count = if channels == 1 { 1 } else { 2 };
    let mut grid = PathBuilder::stroke(px(1.0));
    for channel in 0..count {
        for amplitude in [-1.0, 0.0, 1.0] {
            let y = waveform_y(amplitude, channel, count, height);
            grid.move_to(bounds.origin + point(px(0.0), px(y)));
            grid.line_to(bounds.origin + point(px(width), px(y)));
        }
    }
    for tick in 0..=4 {
        let x = width * tick as f32 / 4.0;
        grid.move_to(bounds.origin + point(px(x), px(0.0)));
        grid.line_to(bounds.origin + point(px(x), px(height)));
    }
    if let Ok(grid) = grid.build() {
        window.paint_path(grid, rgb(0x333333));
    }
    let columns = snapshot.columns(width.ceil() as usize);
    for (channel, color) in [0x64b5f6, 0x81c784].into_iter().enumerate().take(count) {
        let mut path = PathBuilder::stroke(px(1.0));
        for (index, column) in columns.iter().enumerate() {
            let Some(column) = column else { continue };
            let x = width * (index as f32 + 0.5) / columns.len() as f32;
            let (top, bottom) = waveform_span(
                column.min[channel],
                column.max[channel],
                channel,
                count,
                height,
            );
            // Independent min/max spans, never lines through missing audio.
            // A one-pixel minimum also makes observed silence/DC visible.
            path.move_to(bounds.origin + point(px(x), px(top)));
            path.line_to(bounds.origin + point(px(x), px(bottom)));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, rgb(color));
        }
    }
}
fn waveform_y(amplitude: f32, channel: usize, count: usize, height: f32) -> f32 {
    let lane = height / count as f32;
    let radius = (lane * 0.5 - 2.0).max(0.0);
    lane * (channel as f32 + 0.5) - amplitude.clamp(-1.0, 1.0) * radius
}
fn waveform_span(min: f32, max: f32, channel: usize, count: usize, height: f32) -> (f32, f32) {
    let top = waveform_y(max, channel, count, height);
    let bottom = waveform_y(min, channel, count, height);
    let center = (top + bottom) * 0.5;
    let half = (bottom - top).max((height / count as f32).min(1.0)) * 0.5;
    (center - half, center + half)
}
/// Use the same scale on both axes, even in a short or narrow dock.
fn vectorscope_position(side: f32, mid: f32, width: f32, height: f32) -> (f32, f32) {
    let radius = (width.min(height) * 0.5 - 6.0).max(0.0);
    (
        width * 0.5 + side.clamp(-1.0, 1.0) * radius,
        height * 0.5 - mid.clamp(-1.0, 1.0) * radius,
    )
}
/// GPUI linearly filters atlas tiles without padding. Exclude one source texel
/// plus one device pixel on all four sides, including paint_image's rounding,
/// so adjacent atlas contents cannot appear as a flickering rectangular frame.
fn vectorscope_image_clip(square: Bounds<Pixels>, scale_factor: f32) -> Bounds<Pixels> {
    let side = f32::from(square.size.width.min(square.size.height));
    let inset =
        side / mpc_remote_screen_viewer_gpui::vectorscope::IMAGE_SIZE as f32 + 1.0 / scale_factor;
    square.inset(px(inset.min(side * 0.5)))
}
fn frequency_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}
fn level(db: f32) -> f32 {
    ((db + 90.0) / 90.0).clamp(0.0, 1.0)
}
fn meter_level(db: f32) -> f32 {
    if db.is_finite() {
        ((db + 60.0) / 60.0).clamp(0.0, 1.0)
    } else {
        0.0
    }
}
fn channel_label(channels: u8, channel: usize) -> &'static str {
    if channels == 1 {
        "M"
    } else if channel == 0 {
        "L"
    } else {
        "R"
    }
}
fn packed_color(db: f32) -> u32 {
    let [r, g, b] = spectrogram::color(db);
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}
fn project(frequency: f32, age: f32, db: f32) -> (f32, f32) {
    (
        0.03 + frequency * 0.75 + age * 0.18,
        0.95 - age * 0.45 - level(db) * 0.4,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waveform_lanes_have_a_fixed_bipolar_scale_and_stay_inside_resized_docks() {
        for count in [1, 2] {
            for height in [1.0, 3.0, 80.0, 350.0] {
                let lane = height / count as f32;
                for channel in 0..count {
                    let center = lane * (channel as f32 + 0.5);
                    assert_eq!(waveform_y(0.0, channel, count, height), center);
                    assert!(waveform_y(1.0, channel, count, height) <= center);
                    assert!(waveform_y(-1.0, channel, count, height) >= center);
                    for (min, max) in [(-1.0, 1.0), (-0.1, 0.2), (0.0, 0.0), (1.0, 1.0)] {
                        let (top, bottom) = waveform_span(min, max, channel, count, height);
                        assert!(bottom > top);
                        assert!(top >= channel as f32 * lane);
                        assert!(bottom <= (channel + 1) as f32 * lane);
                    }
                }
            }
        }
    }
    #[test]
    fn vectorscope_stays_square_and_inside_resized_docks() {
        for (width, height) in [(220.0, 140.0), (140.0, 400.0), (1.0, 1.0)] {
            let center = vectorscope_position(0.0, 0.0, width, height);
            assert_eq!(center, (width * 0.5, height * 0.5));
            let mono = vectorscope_position(0.0, 1.0, width, height);
            let side = vectorscope_position(1.0, 0.0, width, height);
            assert_eq!(mono.0, center.0);
            assert_eq!(side.1, center.1);
            assert_eq!(side.0 - center.0, center.1 - mono.1);
            for x in [-2.0, -1.0, 0.0, 1.0, 2.0] {
                for y in [-2.0, -1.0, 0.0, 1.0, 2.0] {
                    let (x, y) = vectorscope_position(x, y, width, height);
                    assert!((0.0..=width).contains(&x));
                    assert!((0.0..=height).contains(&y));
                }
            }
        }
    }
    #[test]
    fn vectorscope_clips_all_four_texture_edges_across_sizes_and_display_scales() {
        for side in [0.0, 1.0, 4.0, 12.5, 100.0, 280.5] {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                for origin in [0.0, 0.1, 0.9, 13.75] {
                    let square = Bounds::new(
                        point(px(origin), px(origin + 5.3)),
                        gpui::size(px(side), px(side)),
                    );
                    let clip = vectorscope_image_clip(square, scale);
                    assert!(clip.size.width >= px(0.0));
                    assert_eq!(clip.size.width, clip.size.height);
                    if clip.size.width == px(0.0) {
                        continue;
                    }
                    for (tile_origin, clip_origin) in [
                        (square.origin.x, clip.origin.x),
                        (square.origin.y, clip.origin.y),
                    ] {
                        // Match paint_image's device-pixel rounding and the
                        // atlas sampler's half-texel footprint on each edge.
                        let left = (f32::from(tile_origin) * scale).floor();
                        let width = (side * scale).ceil();
                        let half_texel = width
                            / (2.0 * mpc_remote_screen_viewer_gpui::vectorscope::IMAGE_SIZE as f32);
                        let clip_left = f32::from(clip_origin) * scale;
                        let clip_right = f32::from(clip_origin + clip.size.width) * scale;
                        assert!(clip_left >= left + half_texel);
                        assert!(clip_right <= left + width - half_texel);
                    }
                }
            }
        }
    }
    #[test]
    fn meter_levels_use_a_clamped_sixty_db_range() {
        for (db, expected) in [
            (-90.0, 0.0),
            (-60.0, 0.0),
            (-30.0, 0.5),
            (0.0, 1.0),
            (6.0, 1.0),
        ] {
            assert_eq!(meter_level(db), expected);
        }
        for db in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(meter_level(db), 0.0);
        }
    }
    #[test]
    fn channel_labels_stay_compact_for_mono_stereo_and_multichannel() {
        assert_eq!(channel_label(1, 0), "M");
        for channels in [0, 2, 8] {
            assert_eq!(channel_label(channels, 0), "L");
            assert_eq!(channel_label(channels, 1), "R");
        }
    }
    #[test]
    fn waterfall_projects_old_audio_back_and_loud_audio_up() {
        let front = project(0.0, 0.0, -90.0);
        let back = project(0.0, 1.0, -90.0);
        assert!(back.0 > front.0 && back.1 < front.1);
        assert!(project(0.0, 0.0, 0.0).1 < front.1);
        for frequency in [0.0, 1.0] {
            for age in [0.0, 1.0] {
                for db in [-90.0, 0.0] {
                    let (x, y) = project(frequency, age, db);
                    assert!((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y));
                }
            }
        }
    }
}
