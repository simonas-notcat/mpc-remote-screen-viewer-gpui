//! Workspace presentation. Session, capture and input ownership stay in Viewer.
use super::*;
use gpui::{Corner, Div, WeakEntity};
use gpui_component::{
    Icon, IconName, Selectable, Sizable, StyledExt, WindowExt,
    button::ButtonVariants,
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
};
use theme::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsTab {
    General,
    Audio,
    Streaming,
    Teacher,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToolbarLayout {
    capture: bool,
    labels: bool,
    fullscreen: bool,
}
impl ToolbarLayout {
    fn for_width(width: f32) -> Self {
        Self {
            capture: width >= 680.0,
            labels: width >= 960.0,
            fullscreen: width >= 800.0,
        }
    }
}

fn glyph(name: &str) -> Icon {
    Icon::default().path(format!("icons/{name}.svg"))
}
fn tool(id: &'static str, name: &str, tooltip: &'static str) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(32.0))
        .w(px(32.0))
        .icon(glyph(name))
        .tooltip(tooltip)
}
fn separator() -> Div {
    div()
        .flex_shrink_0()
        .w(px(1.0))
        .h(px(20.0))
        .mx_1()
        .bg(rgb(BORDER))
}
fn caption(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(12.0))
        .text_color(rgb(MUTED))
        .child(text.into())
}
fn heading(title: &'static str, description: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .mb_2()
        .child(div().font_semibold().child(title))
        .child(caption(description))
}
fn setting_row(title: &'static str, description: &'static str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_3()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(title)
                .when(!description.is_empty(), |this| {
                    this.child(caption(description))
                }),
        )
        .child(control)
}
fn status_dot(color: u32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(6.0))
        .rounded_full()
        .bg(rgb(color))
}
fn endpoint(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
fn settings_dimensions(width: f32, height: f32) -> (f32, f32) {
    (
        (width - 32.0).min(640.0),
        (height - 120.0).clamp(160.0, 560.0),
    )
}
fn elapsed_label(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// A separate entity lets dialog contents observe the viewer without borrowing the
/// viewer recursively from its own render method. Root owns focus/Tab/Escape/overlay.
struct SettingsPane {
    viewer: WeakEntity<Viewer>,
    tab: SettingsTab,
    focus: FocusHandle,
}
impl Render for SettingsPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;
        let page = self
            .viewer
            .update(cx, |view, cx| view.settings_page(tab, window, cx))
            .ok();
        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .pb_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .children(
                        [
                            ("general", "General", SettingsTab::General),
                            ("audio", "Audio", SettingsTab::Audio),
                            ("streaming", "Streaming", SettingsTab::Streaming),
                            ("teacher", "AI Teacher", SettingsTab::Teacher),
                        ]
                        .into_iter()
                        .map(|(id, label, value)| {
                            Button::new(id)
                                .small()
                                .ghost()
                                .label(label)
                                .selected(tab == value)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.tab = value;
                                    if value == SettingsTab::Audio {
                                        let _ = view.viewer.update(cx, |viewer, _| {
                                            if let Some(output) = &viewer.audio_output {
                                                output.refresh();
                                            }
                                        });
                                    }
                                    cx.notify();
                                }))
                        }),
                    ),
            )
            .when_some(page, |this, page| this.child(page))
            .child(caption(if tab == SettingsTab::Teacher {
                "AI-teacher sharing and control are session-only, never saved or automatically restored."
            } else {
                "Changes are saved automatically. Passwords are never saved."
            }))
    }
}

impl Viewer {
    pub(super) fn open_settings(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_assisted("Settings opened locally");
        self.release_input(cx);
        if self.show_settings {
            window.close_dialog(cx);
        }
        self.show_settings = true;
        if tab == SettingsTab::Audio
            && let Some(output) = &self.audio_output
        {
            output.refresh();
        }
        let viewer = cx.entity();
        let weak = viewer.downgrade();
        let pane = cx.new(|cx| {
            cx.observe(&viewer, |_, _, cx| cx.notify()).detach();
            SettingsPane {
                viewer: weak.clone(),
                tab,
                focus: self.settings_focus.clone(),
            }
        });
        window.open_dialog(cx, move |dialog, window, _| {
            let (width, height) = settings_dimensions(
                window.viewport_size().width.into(),
                window.viewport_size().height.into(),
            );
            let weak = weak.clone();
            dialog
                .title("Settings")
                .w(px(width))
                .h(px(height))
                .margin_top(px(24.0))
                .child(pane.clone())
                .on_close(move |_, _, cx| {
                    let _ = weak.update(cx, |view, cx| {
                        view.show_settings = false;
                        view.settings_dialog_focus = None;
                        cx.notify();
                    });
                })
        });
        self.settings_dialog_focus = window.focused(cx);
        cx.notify();
    }

    pub(super) fn cycle_settings_focus(
        &self,
        backwards: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self
            .settings_dialog_focus
            .as_ref()
            .filter(|_| self.show_settings)
        else {
            return;
        };
        // GPUI 0.2.2 tab_group orders stops, but does not confine navigation.
        // Skip background stops; cap work even if no dialog controls are mounted yet.
        for _ in 0..128 {
            if backwards {
                window.focus_prev();
            } else {
                window.focus_next();
            }
            if dialog.contains_focused(window, cx) {
                return;
            }
        }
        window.focus(dialog);
    }

    pub(super) fn toolbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let connected = self.snapshot.state == State::Streaming;
        let attempting = self.has_active_session();
        let layout = ToolbarLayout::for_width(window.viewport_size().width.into());
        let name = self
            .snapshot
            .screen
            .as_ref()
            .map(|s| s.name.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.connection_address
                    .as_ref()
                    .map(|(host, _)| host.as_str())
            })
            .unwrap_or("Remote Screen")
            .to_owned();
        let focus = self.focus.clone();
        let supports_control = connected
            && self
                .snapshot
                .screen
                .as_ref()
                .is_some_and(|s| s.input_capabilities != 0);
        let supports_audio = !connected
            || self
                .snapshot
                .screen
                .as_ref()
                .is_some_and(|s| s.audio_available);
        div()
            .h(px(48.0))
            .flex_shrink_0()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(CHROME))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    Button::new("device-menu")
                        .ghost()
                        .small()
                        .h(px(32.0))
                        .max_w_full()
                        .icon(glyph("monitor"))
                        .child(
                            div()
                                .truncate()
                                .max_w(px(if layout.labels { 180.0 } else { 96.0 }))
                                .child(name),
                        )
                        .dropdown_caret(true)
                        .tooltip("Device and connection")
                        .dropdown_menu(move |menu, _, _| {
                            menu.action_context(focus.clone())
                                .menu("Device information", Box::new(DeviceInformation))
                                .separator()
                                .menu(
                                    if connected {
                                        "Disconnect"
                                    } else if attempting {
                                        "Cancel connection"
                                    } else {
                                        "Connect"
                                    },
                                    Box::new(Connect),
                                )
                        }),
                ),
            )
            .when(attempting, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .p_1()
                        .rounded_md()
                        .bg(rgb(BACKGROUND))
                        .child(
                            Button::new("fit")
                                .ghost()
                                .small()
                                .label("Fit")
                                .selected(!self.viewport.actual_size)
                                .disabled(!connected)
                                .tooltip("Fit to window")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.fit(&Fit, window, cx)),
                                ),
                        )
                        .child(
                            Button::new("actual-size")
                                .ghost()
                                .small()
                                .label("1:1")
                                .selected(self.viewport.actual_size)
                                .disabled(!connected)
                                .tooltip("Actual size")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.actual_size(&ActualSize, window, cx)
                                })),
                        ),
                )
                .child(separator())
                .child(
                    tool(
                        "control",
                        "pointer",
                        "Remote control · F12 to toggle / release",
                    )
                    .selected(self.control_on && supports_control)
                    .disabled(!supports_control)
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.remote_control(&RemoteControl, window, cx)
                    })),
                )
                .child(
                    tool(
                        "audio",
                        if self.audio_settings.enabled {
                            "volume"
                        } else {
                            "volume-off"
                        },
                        "Play device audio · output options in Settings",
                    )
                    .selected(self.audio_settings.enabled)
                    .disabled(!supports_audio)
                    .on_click(
                        cx.listener(|view, _, window, cx| view.play_audio(&PlayAudio, window, cx)),
                    ),
                )
            })
            .child(
                tool("analyzer", "analyzer", "Audio visualizers")
                    .selected(self.analyzer.is_some())
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.toggle_analyzer(&AudioAnalyzer, window, cx)
                    })),
            )
            .when(attempting, |this| {
                this.when(layout.capture, |this| {
                    this.child(separator())
                        .child(
                            tool("screenshot", "camera", "Save screenshot · P")
                                .disabled(!connected || self.capture_pending)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.desktop_screenshot(&DesktopScreenshot, window, cx)
                                })),
                        )
                        .child(
                            tool(
                                "record",
                                if self.recorder.is_some() {
                                    "stop"
                                } else {
                                    "record"
                                },
                                "Start / stop recording",
                            )
                            .when(self.recorder.is_some(), |this| this.danger())
                            .disabled(
                                self.recording_pending || (!connected && self.recorder.is_none()),
                            )
                            .on_click(cx.listener(
                                |view, _, window, cx| view.recording(&Recording, window, cx),
                            )),
                        )
                })
                .when(layout.fullscreen, |this| {
                    this.child(tool("fullscreen", "maximize", "Fullscreen · F11").on_click(
                        cx.listener(|view, _, window, cx| view.fullscreen(&Fullscreen, window, cx)),
                    ))
                })
            })
            .child(self.overflow_menu(layout, connected, cx))
            .into_any_element()
    }

    fn overflow_menu(
        &self,
        layout: ToolbarLayout,
        connected: bool,
        _: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus = self.focus.clone();
        let stats = self.show_stats;
        let analyzer = self.analyzer.is_some();
        let teacher = self.teacher.is_some();
        let recording = self.recorder.is_some();
        let recording_pending = self.recording_pending;
        let capture_pending = self.capture_pending;
        tool("more", "ellipsis", "More actions").dropdown_menu_with_anchor(
            Corner::TopRight,
            move |menu, _, _| {
                menu.action_context(focus.clone())
                    .min_w(px(220.0))
                    .when(!layout.capture, |menu| {
                        menu.menu_with_disabled(
                            "Screenshot",
                            Box::new(DesktopScreenshot),
                            !connected || capture_pending,
                        )
                        .menu_with_disabled(
                            if recording {
                                "Stop recording"
                            } else {
                                "Record…"
                            },
                            Box::new(Recording),
                            recording_pending || (!connected && !recording),
                        )
                        .separator()
                    })
                    .menu_with_disabled("Refresh screen", Box::new(Refresh), !connected)
                    .menu("Fullscreen", Box::new(Fullscreen))
                    .item(
                        PopupMenuItem::new("Show statistics")
                            .checked(stats)
                            .action(Box::new(Statistics)),
                    )
                    .menu("Device information", Box::new(DeviceInformation))
                    .item(
                        PopupMenuItem::new("AI teacher (read only)")
                            .checked(teacher)
                            .action(Box::new(AiTeacher)),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Audio analyzer")
                            .checked(analyzer)
                            .action(Box::new(AudioAnalyzer)),
                    )
                    .menu("Audio settings…", Box::new(AudioSettings))
                    .menu("Settings…", Box::new(Settings))
            },
        )
    }

    pub(super) fn connection_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let attempting = self.has_active_session();
        let valid = !self.host.read(cx).value().trim().is_empty()
            && self
                .port
                .read(cx)
                .value()
                .parse::<u16>()
                .is_ok_and(|p| p > 0);
        div()
            .id("connection-scroll")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .bg(rgb(BACKGROUND))
            .child(
                div()
                    .min_h_full()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .child(
                        div()
                            .w(px(440.0))
                            .max_w_full()
                            .flex_shrink_0()
                            .p_6()
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(CHROME))
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(glyph("monitor").size(px(28.0)).text_color(rgb(ACCENT)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_lg().font_semibold().child(if attempting {
                                        "Connecting to your MPC"
                                    } else {
                                        "Connect to your MPC"
                                    }))
                                    .child(caption("View and control a device on your network.")),
                            )
                            .when(attempting, |this| {
                                this.child(
                                    div()
                                        .p_3()
                                        .rounded_md()
                                        .bg(rgb(PANEL))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(information::state_label(self.snapshot.state))
                                        .when_some(
                                            self.connection_address.as_ref(),
                                            |this, (host, port)| {
                                                this.child(caption(endpoint(host, *port)))
                                            },
                                        ),
                                )
                                .child(
                                    Button::new("cancel-connection")
                                        .label("Cancel connection")
                                        .w_full()
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.toggle_connection(&Connect, window, cx)
                                        })),
                                )
                            })
                            .when(!attempting, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child("Device address")
                                        .child(Input::new(&self.host)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child("Password (optional)")
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .child(Input::new(&self.password)),
                                                )
                                                .child(
                                                    tool(
                                                        "password-toggle",
                                                        if self.show_password {
                                                            "eye-off"
                                                        } else {
                                                            "eye"
                                                        },
                                                        "Show / hide password",
                                                    )
                                                    .selected(self.show_password)
                                                    .on_click(cx.listener(|view, _, window, cx| {
                                                        view.show_password = !view.show_password;
                                                        view.password.update(cx, |input, cx| {
                                                            input.set_masked(
                                                                !view.show_password,
                                                                window,
                                                                cx,
                                                            )
                                                        });
                                                        cx.notify();
                                                    })),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .child(
                                            Button::new("advanced-connection")
                                                .ghost()
                                                .small()
                                                .icon(if self.show_port {
                                                    IconName::ChevronDown
                                                } else {
                                                    IconName::ChevronRight
                                                })
                                                .label("Advanced")
                                                .on_click(cx.listener(|view, _, _, cx| {
                                                    view.show_port = !view.show_port;
                                                    view.remember_settings(cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("connect")
                                                .primary()
                                                .label("Connect")
                                                .disabled(!valid)
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.toggle_connection(&Connect, window, cx)
                                                })),
                                        ),
                                )
                                .when(self.show_port, |this| {
                                    this.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_3()
                                            .child(caption("TCP port"))
                                            .child(
                                                div().w(px(100.0)).child(Input::new(&self.port)),
                                            ),
                                    )
                                })
                                .when(!self.viewer_settings.recent_devices.is_empty(), |this| {
                                    this.child(self.recent_devices(cx))
                                })
                            })
                            .child(caption(
                                "Use a trusted LAN or VPN. The device connection is not encrypted.",
                            )),
                    ),
            )
            .into_any_element()
    }

    fn recent_devices(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_3()
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(caption("RECENT DEVICES"))
                    .child(
                        Button::new("clear-recents")
                            .ghost()
                            .small()
                            .label("Clear")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.viewer_settings.recent_devices.clear();
                                view.remember_settings(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("recent-device-list")
                    .max_h(px(144.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.viewer_settings.recent_devices.iter().enumerate().map(
                        |(i, row)| {
                            let row = row.clone();
                            let name = if !row.model.is_empty() {
                                &row.model
                            } else if !row.screen_name.is_empty() {
                                &row.screen_name
                            } else {
                                &row.host
                            };
                            Button::new(("recent-device", i))
                                .ghost()
                                .small()
                                .h_auto()
                                .py_2()
                                .w_full()
                                .justify_start()
                                .tooltip(row.label())
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .items_start()
                                        .gap_1()
                                        .child(
                                            div().truncate().max_w(px(300.0)).child(name.clone()),
                                        )
                                        .child(caption(endpoint(&row.host, row.port))),
                                )
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.select_recent(&row, window, cx)
                                }))
                        },
                    )),
            )
    }

    pub(super) fn status_bar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let connected = self.snapshot.state == State::Streaming;
        let wide = window.viewport_size().width >= px(680.0);
        div()
            .h(px(30.0))
            .flex_shrink_0()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(CHROME))
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_size(px(12.0))
            .text_color(rgb(MUTED))
            .child(status_dot(if connected {
                SUCCESS
            } else if self.session.is_some() {
                WARNING
            } else {
                MUTED
            }))
            .child(information::state_label(self.snapshot.state))
            .when_some(self.snapshot.screen.as_ref(), |this, screen| {
                let (w, h) = screen.upright_size();
                this.child(format!("· {w} × {h}"))
            })
            .child(div().flex_1())
            .when(self.teacher.is_some(), |this| {
                this.child(self.teacher_indicator(cx))
            })
            .when(self.recorder.is_some(), |this| {
                this.child(status_dot(DANGER))
                    .child(format!("REC {}", elapsed_label(self.recording_seconds)))
            })
            .when(self.show_stats && connected && wide, |this| {
                this.when_some(self.snapshot.statistics.rates, |this, rates| {
                    this.child(format!("{:.1} fps", rates.frames_per_second))
                        .child(format!("{:.1} kbit/s", rates.kilobits_per_second))
                })
                .child(information::rtt_text(&self.snapshot))
            })
            .child(
                tool(
                    "status-information",
                    "info",
                    "Device information and performance",
                )
                .w(px(24.0))
                .h(px(24.0))
                .selected(self.show_information)
                .on_click(cx.listener(|view, _, window, cx| {
                    view.device_information(&DeviceInformation, window, cx)
                })),
            )
    }

    pub(super) fn notifications(&self, cx: &mut Context<Self>) -> AnyElement {
        let audio_error = self
            .audio_settings
            .enabled
            .then(|| {
                self.audio_status
                    .error
                    .clone()
                    .or_else(|| self.output_status.error.clone())
            })
            .flatten();
        let errors: Vec<_> = [
            self.settings_error.clone(),
            self.snapshot.error.clone(),
            audio_error,
        ]
        .into_iter()
        .flatten()
        .collect();
        div()
            .absolute()
            .bottom(px(40.0))
            .right_3()
            .w(px(420.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(errors.into_iter().map(|error| {
                div()
                    .occlude()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0x38282a))
                    .border_1()
                    .border_color(rgb(0x795052))
                    .text_sm()
                    .child(error)
            }))
            .when_some(self.message.clone(), |this, message| {
                this.child(
                    div()
                        .occlude()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .shadow_md()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(div().flex_1().min_w_0().text_sm().child(message))
                        .child(
                            tool("dismiss-message", "close", "Dismiss notification")
                                .w(px(24.0))
                                .h(px(24.0))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.message = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    fn settings_page(
        &self,
        tab: SettingsTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match tab {
            SettingsTab::General => div()
                .flex()
                .flex_col()
                .child(heading(
                    "General",
                    "Keep the workspace focused on the device screen.",
                ))
                .child(setting_row(
                    "Show statistics",
                    "Live frame rate, receive bandwidth and ping in the status bar.",
                    Switch::new("settings-stats")
                        .checked(self.show_stats)
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.statistics(&Statistics, window, cx)
                        })),
                ))
                .child(setting_row(
                    "Show connection port",
                    "Expand advanced options in the connection form.",
                    Switch::new("settings-port")
                        .checked(self.show_port)
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.show_port = !view.show_port;
                            view.remember_settings(cx);
                        })),
                ))
                .child(
                    div()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child("Screenshot folder")
                        .child(caption(
                            self.screenshot_folder
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| {
                                    "Not set — choose a folder on your first screenshot.".into()
                                }),
                        ))
                        .child(
                            Button::new("screenshot-folder")
                                .small()
                                .label("Choose folder…")
                                .icon(IconName::Folder)
                                .disabled(self.capture_pending)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.choose_screenshot_folder(false, window, cx)
                                })),
                        ),
                )
                .into_any_element(),
            SettingsTab::Audio => self.audio_settings_page(cx),
            SettingsTab::Teacher => self.teacher_settings_page(cx),
            SettingsTab::Streaming => {
                let focus = self.settings_focus.clone();
                let selected = self.viewer_settings.fps_limit;
                let viewer = cx.weak_entity();
                div().flex().flex_col()
                    .child(heading("Streaming", "These options apply to the current connection and future sessions."))
                    .child(setting_row("Frame-rate limit", "Ask the device to limit screen updates.",
                        Button::new("fps-limit").small().label(match selected { 0 => "Server default".into(), fps => format!("{fps} fps") })
                            .dropdown_caret(true)
                            .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
                                [0, 15, 5].into_iter().fold(menu.action_context(focus.clone()), |menu, fps| {
                                    let viewer = viewer.clone();
                                    menu.item(PopupMenuItem::new(if fps == 0 { "Server default".into() } else { format!("{fps} fps") })
                                        .checked(selected == fps)
                                        .on_click(move |_, _, cx| { let _ = viewer.update(cx, |view, cx| view.set_frame_rate(fps, cx)); }))
                                })
                            })))
                    .child(div().pt_3().child(caption("The device protocol is unencrypted. Only connect over a trusted local network or external VPN.")))
                    .into_any_element()
            }
        }
    }

    fn teacher_settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let access = self.assisted_settings();
        div()
            .flex()
            .flex_col()
            .child(heading("AI Teacher", "Explicit local permission for MCP screen sharing and assisted input."))
            .child(setting_row(
                "AI teacher sharing",
                "Share requested screens with same-user MCP clients and their configured model. Enables local teaching highlights; no device control by itself.",
                Switch::new("settings-teacher-sharing")
                    .checked(self.teacher.is_some())
                    .on_click(cx.listener(|view, enabled, window, cx| {
                        if *enabled != view.teacher.is_some() {
                            view.toggle_teacher(&AiTeacher, window, cx);
                        }
                    })),
            ))
            .child(setting_row(
                "Allow assisted proposals · 60 s",
                "Grant a fresh 60-second control lease. Every click, drag, scroll, key or text gesture still requires separate local approval.",
                Switch::new("settings-teacher-assisted")
                    .checked(access.lease_active)
                    .disabled(access.switch_disabled())
                    .on_click(cx.listener(|view, enabled, _, cx| view.set_assisted_enabled(*enabled, cx))),
            ))
            .child(div().pt_3().child(caption(if access.lease_active {
                "Lease active. Close Settings to review and approve a proposal in the teacher panel. Turning this switch off requests cancellation/release."
            } else {
                access.unavailable.unwrap_or("Read only. Turn on assisted proposals to grant a new lease; this switch turns off when the lease expires or is revoked.")
            })))
            .child(div().pt_3().child(caption(
                "Keep the viewer focused after enabling: window deactivation, opening Settings again, Stop/F12, disconnect or reconfiguration revoke control. No automatic approval or renewal.",
            )))
            .into_any_element()
    }

    fn audio_settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let device_name = self.audio_settings.device.clone();
        let label = device_name
            .clone()
            .unwrap_or_else(|| "System default".into());
        let devices = self.output_status.devices.clone();
        let selected_device = devices.iter().find(|d| {
            device_name
                .as_ref()
                .map_or(d.is_default, |name| name == &d.name)
        });
        let rates = selected_device.map(|d| d.rates.clone()).unwrap_or_default();
        let buffers = selected_device
            .map(|d| d.buffers.clone())
            .unwrap_or_default();
        let viewer = cx.weak_entity();
        let focus = self.settings_focus.clone();
        div()
            .flex()
            .flex_col()
            .child(heading(
                "Audio",
                "Device playback only. No microphone is used.",
            ))
            .child(setting_row(
                "Play device audio",
                "Playback starts when a connected device offers audio.",
                Switch::new("settings-audio")
                    .checked(self.audio_settings.enabled)
                    .disabled(
                        self.snapshot.state == State::Streaming
                            && !self
                                .snapshot
                                .screen
                                .as_ref()
                                .is_some_and(|s| s.audio_available),
                    )
                    .on_click(
                        cx.listener(|view, _, window, cx| view.play_audio(&PlayAudio, window, cx)),
                    ),
            ))
            .child(
                div()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child("Output device")
                    .child(
                        Button::new("output-device")
                            .small()
                            .label(label)
                            .dropdown_caret(true)
                            .w_full()
                            .dropdown_menu(move |menu, _, _| {
                                let view = viewer.clone();
                                let menu = menu
                                    .min_w(px(240.0))
                                    .max_w(px(400.0))
                                    .scrollable(true)
                                    .action_context(focus.clone())
                                    .item(
                                        PopupMenuItem::new("System default")
                                            .checked(device_name.is_none())
                                            .on_click(move |_, _, cx| {
                                                let _ = view.update(cx, |view, cx| {
                                                    view.audio_settings.device = None;
                                                    view.audio_settings.sample_rate = None;
                                                    view.audio_settings.buffer_frames = None;
                                                    view.update_audio_settings(cx);
                                                });
                                            }),
                                    );
                                devices.iter().fold(menu, |menu, device| {
                                    let name = device.name.clone();
                                    let view = viewer.clone();
                                    menu.item(
                                        PopupMenuItem::new(name.clone())
                                            .checked(device_name.as_ref() == Some(&name))
                                            .on_click(move |_, _, cx| {
                                                let _ = view.update(cx, |view, cx| {
                                                    view.audio_settings.device = Some(name.clone());
                                                    view.audio_settings.sample_rate = None;
                                                    view.audio_settings.buffer_frames = None;
                                                    view.update_audio_settings(cx);
                                                });
                                            }),
                                    )
                                })
                            }),
                    )
                    .child(
                        Button::new("audio-rescan")
                            .ghost()
                            .small()
                            .label("Rescan / retry")
                            .icon(glyph("refresh"))
                            .on_click(cx.listener(|view, _, _, _| {
                                if let Some(output) = &view.audio_output {
                                    output.refresh();
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child("Volume")
                            .child(caption(format!(
                                "{:.0}%",
                                self.audio_settings.volume * 100.0
                            ))),
                    )
                    .child(
                        div()
                            .capture_any_mouse_down(cx.listener(
                                |view, event: &MouseDownEvent, window, cx| {
                                    if event.button == MouseButton::Left && event.click_count == 2 {
                                        cx.stop_propagation();
                                        view.audio_settings.volume = 1.0;
                                        view.volume.update(cx, |slider, cx| {
                                            slider.set_value(1.0, window, cx)
                                        });
                                        view.update_audio_settings(cx);
                                    }
                                },
                            ))
                            .child(Slider::new(&self.volume).horizontal()),
                    )
                    .child(caption(format!(
                        "Input {:.0} dBFS · Output {:.0} dBFS",
                        peak_db(self.audio_status.input_peak),
                        peak_db(self.audio_status.output_peak)
                    )))
                    .child(
                        Button::new("volume-reset")
                            .ghost()
                            .small()
                            .label("Reset volume")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.audio_settings.volume = 1.0;
                                view.volume
                                    .update(cx, |slider, cx| slider.set_value(1.0, window, cx));
                                view.update_audio_settings(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child("Target buffering")
                            .child(caption(format!(
                                "{} ms",
                                self.audio_settings.target_buffer_ms
                            ))),
                    )
                    .child(Slider::new(&self.buffering).horizontal())
                    .child(caption(
                        "Lower buffering reduces latency but increases dropout risk.",
                    ))
                    .child(
                        Button::new("buffering-reset")
                            .ghost()
                            .small()
                            .label("Reset to 80 ms")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.audio_settings.target_buffer_ms = DEFAULT_TARGET_BUFFER_MS;
                                view.buffering.update(cx, |slider, cx| {
                                    slider.set_value(DEFAULT_TARGET_BUFFER_MS as f32, window, cx)
                                });
                                view.update_audio_settings(cx);
                            })),
                    ),
            )
            .child(
                Button::new("audio-advanced")
                    .ghost()
                    .small()
                    .label("Advanced output options")
                    .icon(if self.show_audio_advanced {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.show_audio_advanced = !view.show_audio_advanced;
                        cx.notify();
                    })),
            )
            .when(self.show_audio_advanced, |this| {
                this.child(self.audio_format_choice(
                    "sample-rate",
                    "Sample rate",
                    rates,
                    self.audio_settings.sample_rate,
                    true,
                    cx,
                ))
                .child(self.audio_format_choice(
                    "buffer-size",
                    "Output buffer",
                    buffers,
                    self.audio_settings.buffer_frames,
                    false,
                    cx,
                ))
            })
            .when_some(self.output_status.active.clone(), |this, active| {
                this.child(div().pt_3().child(caption(active)))
            })
            .when_some(self.output_status.warning.clone(), |this, warning| {
                this.child(div().pt_3().text_color(rgb(WARNING)).child(warning))
            })
            .when_some(self.output_status.settings_error.clone(), |this, error| {
                this.child(div().pt_3().text_color(rgb(DANGER)).child(error))
            })
            .when_some(
                self.output_status
                    .error
                    .clone()
                    .or_else(|| self.audio_status.error.clone()),
                |this, error| this.child(div().pt_3().text_color(rgb(DANGER)).child(error)),
            )
            .into_any_element()
    }

    fn audio_format_choice(
        &self,
        id: &'static str,
        title: &'static str,
        values: Vec<u32>,
        selected: Option<u32>,
        rate: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let viewer = cx.weak_entity();
        let focus = self.settings_focus.clone();
        let label = selected
            .map(|n| format!("{n} {}", if rate { "Hz" } else { "frames" }))
            .unwrap_or_else(|| "Default".into());
        setting_row(
            title,
            "",
            Button::new(id)
                .small()
                .label(label)
                .dropdown_caret(true)
                .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
                    std::iter::once(None)
                        .chain(values.iter().copied().map(Some))
                        .fold(menu.action_context(focus.clone()), |menu, value| {
                            let viewer = viewer.clone();
                            menu.item(
                                PopupMenuItem::new(
                                    value
                                        .map(|n| {
                                            format!("{n} {}", if rate { "Hz" } else { "frames" })
                                        })
                                        .unwrap_or_else(|| "Default".into()),
                                )
                                .checked(selected == value)
                                .on_click(move |_, _, cx| {
                                    let _ = viewer.update(cx, |view, cx| {
                                        if rate {
                                            view.audio_settings.sample_rate = value;
                                        } else {
                                            view.audio_settings.buffer_frames = value;
                                        }
                                        view.update_audio_settings(cx);
                                    });
                                }),
                            )
                        })
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_breakpoints_move_secondary_actions_to_overflow() {
        assert_eq!(
            ToolbarLayout::for_width(480.0),
            ToolbarLayout {
                capture: false,
                labels: false,
                fullscreen: false
            }
        );
        assert!(!ToolbarLayout::for_width(679.0).capture);
        assert!(ToolbarLayout::for_width(680.0).capture);
        assert!(ToolbarLayout::for_width(800.0).fullscreen);
        assert!(!ToolbarLayout::for_width(959.0).labels);
        assert!(ToolbarLayout::for_width(960.0).labels);
    }

    #[test]
    fn settings_dialog_fits_small_windows_and_caps_large_ones() {
        assert_eq!(settings_dimensions(480.0, 320.0), (448.0, 200.0));
        assert_eq!(settings_dimensions(1000.0, 700.0), (640.0, 560.0));
        assert_eq!(settings_dimensions(8192.0, 8192.0), (640.0, 560.0));
    }

    #[test]
    fn endpoints_and_recording_elapsed_time_are_unambiguous() {
        assert_eq!(endpoint("::1", 7447), "[::1]:7447");
        assert_eq!(endpoint("mpc.local", 7447), "mpc.local:7447");
        assert_eq!(elapsed_label(0), "00:00");
        assert_eq!(elapsed_label(125), "02:05");
        assert_eq!(elapsed_label(3600), "60:00");
    }
}
