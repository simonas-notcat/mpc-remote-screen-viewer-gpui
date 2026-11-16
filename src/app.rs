//! GPUI workspace and action/controller integration.
mod analyzer;
mod teacher;
pub mod theme;
mod ui;
use gpui::{
    AnyElement, App, Bounds, Context, Corners, Entity, FocusHandle, KeyBinding, KeyDownEvent, Menu,
    MenuItem, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, Pixels, Point, RenderImage,
    ScrollWheelEvent, Window, actions, canvas, div, point, prelude::*, px, rgb,
};
use gpui_component::{
    Disableable,
    button::Button,
    input::{Input, InputEvent as FieldEvent, InputState},
    slider::{Slider, SliderEvent, SliderState},
};
use mpc_remote_screen_viewer_gpui::{
    audio,
    audio_output::{self, AudioOutput},
    cli::Launch,
    control::Status as TeacherControlStatus,
    information,
    input::{self, Control},
    playout::{DEFAULT_TARGET_BUFFER_MS, MAX_TARGET_BUFFER_MS, MIN_TARGET_BUFFER_MS, peak_db},
    preferences,
    protocol::{DEFAULT_PORT, InputEvent},
    recording::Recorder,
    session::{Command, Config, Session, Snapshot, State},
    statistics::{HISTORY_LIMIT, Sample},
    teacher::{Bridge as TeacherBridge, Highlight as TeacherHighlight},
    viewport::Picture,
    viewport::Viewport,
};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use ui::SettingsTab;

type SharedFramebuffer =
    Arc<std::sync::RwLock<mpc_remote_screen_viewer_gpui::framebuffer::Framebuffer>>;

fn image_needs_update(
    rendered: Option<&(SharedFramebuffer, u64)>,
    framebuffer: &SharedFramebuffer,
) -> bool {
    let Ok(frame) = framebuffer.try_read() else {
        return false; // Retry on the next poll rather than blocking GPUI.
    };
    !rendered.is_some_and(|(old, revision)| {
        Arc::ptr_eq(old, framebuffer) && *revision == frame.content_revision()
    })
}

// Unit actions integrate with GPUI's native macOS menu and key dispatch.
actions!(
    viewer,
    [
        Quit,
        Connect,
        Refresh,
        Fit,
        ActualSize,
        Fullscreen,
        ExitFullscreen,
        Statistics,
        SaveScreenshot,
        DesktopScreenshot,
        Settings,
        RemoteControl,
        PlayAudio,
        AudioAnalyzer,
        AiTeacher,
        AllowAssistance,
        AudioSettings,
        DeviceInformation,
        Recording,
        SettingsNextFocus,
        SettingsPreviousFocus
    ]
);

pub fn register_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("f12", RemoteControl, Some("Viewer")),
        KeyBinding::new("f11", Fullscreen, Some("Viewer")),
        KeyBinding::new("escape", ExitFullscreen, Some("Viewer")),
        KeyBinding::new("cmd-,", Settings, Some("Viewer")),
        KeyBinding::new("cmd-,", Settings, Some("ViewerModal")),
        KeyBinding::new("tab", SettingsNextFocus, Some("ViewerModal")),
        KeyBinding::new("shift-tab", SettingsPreviousFocus, Some("ViewerModal")),
        KeyBinding::new("f", Fullscreen, Some("Viewer && !Input && !RemoteControl")),
        KeyBinding::new("s", Statistics, Some("Viewer && !Input && !RemoteControl")),
        KeyBinding::new("r", Refresh, Some("Viewer && !Input && !RemoteControl")),
        KeyBinding::new(
            "p",
            DesktopScreenshot,
            Some("Viewer && !Input && !RemoteControl"),
        ),
    ]);
    cx.set_menus(vec![
        Menu {
            name: "MPC Remote Screen Viewer GPUI".into(),
            items: vec![
                MenuItem::action("Settings…", Settings),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Connect / Disconnect", Connect),
                MenuItem::action("Save Screenshot…", SaveScreenshot),
                MenuItem::action("Start / Stop Recording…", Recording),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Fit to Window", Fit),
                MenuItem::action("Actual Size (1:1)", ActualSize),
                MenuItem::action("Fullscreen", Fullscreen),
                MenuItem::action("Show Statistics", Statistics),
                MenuItem::action("Device Information…", DeviceInformation),
                MenuItem::action("Refresh Screen", Refresh),
                MenuItem::action("Remote Control (F12)", RemoteControl),
                MenuItem::action("Allow assistance", AllowAssistance),
                MenuItem::action("Play Device Audio", PlayAudio),
                MenuItem::action("Audio Analyzer", AudioAnalyzer),
                MenuItem::action("AI Teacher sharing", AiTeacher),
                MenuItem::action("Audio Settings…", AudioSettings),
            ],
        },
    ]);
}

pub struct Viewer {
    host: Entity<InputState>,
    password: Entity<InputState>,
    port: Entity<InputState>,
    focus: FocusHandle,
    settings_focus: FocusHandle,
    settings_dialog_focus: Option<FocusHandle>,
    session: Option<Session>,
    connection_address: Option<(String, u16)>,
    snapshot: Snapshot,
    teacher: Option<TeacherBridge>,
    teacher_highlight: Option<TeacherHighlight>,
    teacher_control: Option<TeacherControlStatus>,
    assistance_on: bool,
    viewer_settings: preferences::ViewerSettings,
    settings_writer: Option<preferences::ViewerSettingsWriter>,
    settings_error: Option<String>,
    generation: u64,
    rendered_frame: Option<(SharedFramebuffer, u64)>,
    image: Option<Arc<RenderImage>>,
    viewport: Viewport,
    drag: Option<Point<Pixels>>,
    control_on: bool,
    control: Control,
    audio_settings: preferences::AudioSettings,
    audio_output: Option<AudioOutput>,
    audio_status: audio::Status,
    output_status: audio_output::Status,
    volume: Entity<SliderState>,
    buffering: Entity<SliderState>,
    show_audio_advanced: bool,
    analyzer: Option<mpc_remote_screen_viewer_gpui::analyzer::Analyzer>,
    analysis: mpc_remote_screen_viewer_gpui::analyzer::Snapshot,
    analyzer_height: f32,
    analyzer_mode: analyzer::Mode,
    show_vectorscope: bool,
    vectorscope_image: Option<Arc<RenderImage>>,
    spectrogram_image: Option<Arc<RenderImage>>,
    spectrogram_history: Option<Arc<mpc_remote_screen_viewer_gpui::spectrogram::History>>,
    resizing_analyzer: bool,
    picture: Picture,
    show_port: bool,
    show_stats: bool,
    show_information: bool,
    show_settings: bool,
    show_password: bool,
    capture_pending: bool,
    recorder: Option<Recorder>,
    recording_pending: bool,
    recording_started: Option<Instant>,
    recording_seconds: u64,
    screenshot_folder: Option<PathBuf>,
    message: Option<String>,
}
impl Viewer {
    pub fn new(
        launch: Launch,
        settings: preferences::ViewerSettings,
        settings_error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let writer =
            preferences::viewer_settings_path().and_then(preferences::ViewerSettingsWriter::new);
        let settings_error = settings_error.or_else(|| {
            writer
                .as_ref()
                .err()
                .map(|e| format!("Could not start settings worker: {e}"))
        });
        let saved_audio =
            preferences::audio_settings_path().and_then(|path| preferences::load_audio(&path));
        let audio_error = saved_audio
            .as_ref()
            .err()
            .map(|e| format!("Could not load audio settings: {e}"));
        let mut audio_settings = saved_audio.unwrap_or_default();
        audio_settings.enabled |= launch.audio;
        let output = AudioOutput::new(audio_settings.clone());
        let output_error = output
            .as_ref()
            .err()
            .map(|e| format!("Could not start audio worker: {e}"));
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.01)
                .default_value(audio_settings.volume)
        });
        cx.subscribe(&volume, |view, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(value) = event;
            view.audio_settings.volume = value.start();
            view.update_audio_settings(cx);
        })
        .detach();
        let buffering = cx.new(|_| {
            SliderState::new()
                .min(MIN_TARGET_BUFFER_MS as f32)
                .max(MAX_TARGET_BUFFER_MS as f32)
                .step(1.0)
                .default_value(audio_settings.target_buffer_ms as f32)
        });
        cx.subscribe(&buffering, |view, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(value) = event;
            view.audio_settings.target_buffer_ms = value.start().round() as u32;
            view.update_audio_settings(cx);
        })
        .detach();
        let host = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("device IP or name");
            input.set_value(launch.host.clone(), window, cx);
            input
        });
        let password = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("optional password")
                .masked(true);
            input.set_value(launch.password.to_string(), window, cx);
            input
        });
        let port = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("7447");
            input.set_value(launch.port.to_string(), window, cx);
            input
        });
        for field in [&host, &port] {
            cx.subscribe(field, |view: &mut Self, _, event: &FieldEvent, cx| {
                if matches!(event, FieldEvent::Change) {
                    view.remember_settings(cx);
                }
            })
            .detach();
        }
        for field in [&host, &password, &port] {
            cx.subscribe_in(
                field,
                window,
                |view: &mut Self, _, event: &FieldEvent, window, cx| {
                    if matches!(event, FieldEvent::PressEnter { .. })
                        && !view.has_active_session()
                        && !view.show_settings
                    {
                        view.toggle_connection(&Connect, window, cx);
                    }
                    cx.notify();
                },
            )
            .detach();
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        cx.on_app_quit(|view, cx| {
            view.remember_settings(cx);
            let session = view.session.take();
            let recorder = view.recorder.take();
            let settings_writer = view.settings_writer.take();
            let audio_output = view.audio_output.take();
            let teacher = view.teacher.take();
            if let Some(teacher) = &teacher {
                teacher.request_stop();
            }
            if let Some(session) = &session {
                session.audio().set_recording(None);
                session.request_stop();
            }
            cx.background_executor().spawn(async move {
                drop(teacher);
                if let Some(recorder) = recorder
                    && let Err(error) = recorder.finish()
                {
                    eprintln!("{error}");
                }
                drop(session);
                if let Some(output) = audio_output
                    && let Err(error) = output.finish()
                {
                    eprintln!("{error}");
                }
                if let Some(writer) = settings_writer
                    && let Err(error) = writer.finish()
                {
                    eprintln!("{error}");
                }
            })
        })
        .detach();
        cx.on_blur(&focus, window, |view: &mut Self, _, cx| {
            view.release_input(cx)
        })
        .detach();
        cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active() {
                view.release_input(cx);
            }
        })
        .detach();
        // One polling/preparation task, one newest snapshot. Image rotation/conversion
        // stays off the main thread; no full-screen repaint queue accumulates.
        cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let Ok(job) = view.update_in(cx, |view, window, cx| view.poll(window, cx)) else {
                    break;
                };
                let Some((generation, framebuffer)) = job else {
                    continue;
                };
                let prepared = cx
                    .background_executor()
                    .spawn(async move {
                        let frame = framebuffer.try_read().ok()?;
                        let mut image = frame.upright_snapshot();
                        let revision = frame.content_revision();
                        drop(frame);
                        // GPUI RenderImage explicitly expects BGRA, unlike core snapshots.
                        for p in image.pixels_mut() {
                            p.0.swap(0, 2);
                        }
                        Some((
                            Arc::new(RenderImage::new([image::Frame::new(image)])),
                            (framebuffer, revision),
                        ))
                    })
                    .await;
                if view
                    .update_in(cx, |view, window, cx| {
                        if view.generation == generation
                            && let Some((image, stamp)) = prepared
                            && view.session.as_ref().is_some_and(|session| {
                                session
                                    .snapshot()
                                    .framebuffer
                                    .as_ref()
                                    .is_some_and(|current| Arc::ptr_eq(current, &stamp.0))
                            })
                        {
                            if let Some(old) = view.image.replace(image) {
                                cx.drop_image(old, Some(window));
                            }
                            view.rendered_frame = Some(stamp);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        if launch.auto_connect {
            cx.defer_in(window, |view, window, cx| {
                view.toggle_connection(&Connect, window, cx)
            });
        }
        if !launch.auto_connect {
            let host = host.clone();
            cx.defer_in(window, move |_, window, cx| {
                host.update(cx, |input, cx| input.focus(window, cx));
            });
        }
        let saved_folder = preferences::screenshot_settings_path()
            .and_then(|path| preferences::load_folder(&path));
        let screenshot_settings_error = saved_folder
            .as_ref()
            .err()
            .map(|e| format!("Could not load screenshot settings: {e}"));
        let restored_analyzer = settings
            .analyzer_open
            .then(mpc_remote_screen_viewer_gpui::analyzer::Analyzer::new)
            .transpose();
        let analyzer_error = restored_analyzer
            .as_ref()
            .err()
            .map(|e| format!("Could not restore analyzer: {e}"));
        let mut viewer = Self {
            host,
            password,
            port,
            focus,
            settings_focus: cx.focus_handle(),
            settings_dialog_focus: None,
            session: None,
            connection_address: None,
            snapshot: Snapshot::default(),
            teacher: None,
            teacher_highlight: None,
            teacher_control: None,
            assistance_on: false,
            settings_writer: writer.ok(),
            settings_error,
            generation: 0,
            rendered_frame: None,
            image: None,
            viewport: Viewport {
                actual_size: settings.actual_size,
                ..Viewport::default()
            },
            drag: None,
            control_on: settings.control_on,
            control: Control::default(),
            audio_settings,
            audio_output: output.ok(),
            audio_status: audio::Status::default(),
            output_status: audio_output::Status::default(),
            volume,
            buffering,
            show_audio_advanced: false,
            analyzer: restored_analyzer.ok().flatten(),
            analysis: Default::default(),
            analyzer_height: settings.analyzer_height,
            analyzer_mode: settings.analyzer_mode,
            show_vectorscope: settings.show_vectorscope,
            vectorscope_image: None,
            spectrogram_image: None,
            spectrogram_history: None,
            resizing_analyzer: false,
            picture: Picture::default(),
            show_port: settings.show_port || launch.port != DEFAULT_PORT,
            show_stats: settings.show_stats,
            show_information: false,
            viewer_settings: settings,
            show_settings: false,
            show_password: false,
            capture_pending: false,
            recorder: None,
            recording_pending: false,
            recording_started: None,
            recording_seconds: 0,
            screenshot_folder: saved_folder.ok().flatten(),
            message: screenshot_settings_error
                .or(audio_error)
                .or(output_error)
                .or(analyzer_error),
        };
        if launch.teacher {
            viewer.toggle_teacher(&AiTeacher, window, cx);
        }
        viewer
    }
    fn remember_settings(&mut self, cx: &mut Context<Self>) {
        let port = self
            .port
            .read(cx)
            .value()
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)
            .unwrap_or(self.viewer_settings.port);
        let _ = self
            .viewer_settings
            .remember_address(self.host.read(cx).value().as_str(), port);
        self.viewer_settings.actual_size = self.viewport.actual_size;
        self.viewer_settings.show_port = self.show_port;
        self.viewer_settings.show_stats = self.show_stats;
        self.viewer_settings.control_on = self.control_on;
        self.viewer_settings.analyzer_open = self.analyzer.is_some();
        self.viewer_settings.analyzer_mode = self.analyzer_mode;
        self.viewer_settings.analyzer_height = self.analyzer_height;
        self.viewer_settings.show_vectorscope = self.show_vectorscope;
        if let Some(writer) = &self.settings_writer {
            writer.request(self.viewer_settings.clone());
        }
        cx.notify();
    }
    fn has_active_session(&self) -> bool {
        self.session.is_some() && self.snapshot.state != State::Failed
    }
    fn select_recent(
        &mut self,
        row: &preferences::RecentDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_active_session() {
            return;
        }
        self.host.update(cx, |input, cx| {
            input.set_value(row.host.clone(), window, cx)
        });
        self.port.update(cx, |input, cx| {
            input.set_value(row.port.to_string(), window, cx)
        });
        // Never carry another device's secret into the selected address.
        self.password.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.show_port |= row.port != DEFAULT_PORT;
        self.message = None;
        self.remember_settings(cx);
    }
    fn set_frame_rate(&mut self, fps: u8, cx: &mut Context<Self>) {
        self.viewer_settings.fps_limit = fps.min(120);
        // Unlike refresh/input, this desired option also applies during retry/login.
        if let Some(session) = &self.session
            && session
                .command(Command::FrameRate(self.viewer_settings.fps_limit))
                .is_err()
        {
            self.message = Some("Connection command queue is full; Stop and reconnect to apply the saved frame rate.".into());
        }
        self.remember_settings(cx);
    }
    fn poll(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(u64, SharedFramebuffer)> {
        let seconds = self
            .recording_started
            .map(|start| start.elapsed().as_secs())
            .unwrap_or(0);
        if seconds != self.recording_seconds {
            self.recording_seconds = seconds;
            cx.notify();
        }
        if self.recorder.as_ref().is_some_and(Recorder::completed) {
            self.stop_recording(cx);
        }
        if let Some(output) = &self.audio_output {
            let status = output.status();
            if status != self.output_status {
                self.output_status = status;
                cx.notify();
            }
        }
        if let Some(result) = self
            .settings_writer
            .as_ref()
            .and_then(|writer| writer.result())
        {
            let error = result.err();
            if error != self.settings_error {
                self.settings_error = error;
                cx.notify();
            }
        }
        if let Some(analyzer) = &self.analyzer {
            let analysis = analyzer.snapshot();
            if analysis != self.analysis {
                self.analysis = analysis;
                self.sync_spectrogram_image(window, cx);
                self.sync_vectorscope_image(window, cx);
                cx.notify();
            }
        }
        let highlight = self
            .teacher
            .as_ref()
            .and_then(|bridge| bridge.teacher().overlay());
        if highlight != self.teacher_highlight {
            self.teacher_highlight = highlight;
            cx.notify();
        }
        let control = self
            .teacher
            .as_ref()
            .and_then(|b| b.teacher().control())
            .map(|c| c.status());
        let animate = control
            .as_ref()
            .and_then(|s| s.feedback.as_ref())
            .is_some_and(|f| {
                f.phase.active() || f.visible_until.is_some_and(|until| until > Instant::now())
            });
        if control != self.teacher_control || animate {
            self.teacher_control = control;
            cx.notify();
        }
        let session = self.session.as_ref()?;
        let state = session.snapshot();
        let sink = session.audio();
        let mut status = sink.status();
        (status.input_peak, status.output_peak) = sink.take_peaks();
        if status != self.audio_status {
            self.audio_status = status;
            cx.notify();
        }
        if let Some(output) = &self.audio_output {
            output.set_source(
                (self.audio_settings.enabled
                    && state.state == State::Streaming
                    && state.screen.as_ref().is_some_and(|s| s.audio_available))
                .then_some(sink),
            );
        }
        let title_changed =
            state.state != self.snapshot.state || state.screen != self.snapshot.screen;
        let changed = title_changed
            || state.error != self.snapshot.error
            || state.statistics != self.snapshot.statistics
            || state.protocol_version != self.snapshot.protocol_version
            || state.revision != self.snapshot.revision
            || state.device != self.snapshot.device;
        if state.framebuffer.is_none() {
            if let Some(old) = self.image.take() {
                cx.drop_image(old, Some(window));
            }
            self.rendered_frame = None;
        }
        let job = state
            .framebuffer
            .as_ref()
            .filter(|framebuffer| image_needs_update(self.rendered_frame.as_ref(), framebuffer))
            .map(|framebuffer| (self.generation, framebuffer.clone()));
        let same_screen = match (&self.snapshot.framebuffer, &state.framebuffer) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if state.state == State::Streaming
            && (!same_screen || state.device != self.snapshot.device)
            && let (Some((host, port)), Some(screen)) = (&self.connection_address, &state.screen)
        {
            // Only authenticated streaming sessions enter recents. Metadata can arrive later.
            let _ = self.viewer_settings.remember_device(
                host,
                *port,
                &screen.name,
                state.device.as_ref(),
            );
            self.remember_settings(cx);
        }
        if !same_screen {
            self.control.clear();
            self.drag = None;
        }
        self.snapshot = state;
        if title_changed {
            let title = if self.snapshot.state == State::Streaming {
                format!(
                    "Remote Screen - {}",
                    self.snapshot
                        .screen
                        .as_ref()
                        .map(|s| s.name.as_str())
                        .unwrap_or("MPC")
                )
            } else {
                "MPC Remote Screen Viewer GPUI".into()
            };
            window.set_window_title(&title);
        }
        if changed {
            cx.notify();
        }
        job
    }
    fn toggle_connection(&mut self, _: &Connect, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            use gpui_component::WindowExt;
            window.close_dialog(cx);
            self.show_settings = false;
            self.settings_dialog_focus = None;
        }
        self.release_input(cx);
        self.stop_recording(cx);
        if let Some(teacher) = &self.teacher {
            teacher.teacher().set_source(None);
        }
        self.teacher_highlight = None;
        self.generation = self.generation.wrapping_add(1);
        window.set_window_title("MPC Remote Screen Viewer GPUI");
        self.message = None;
        self.rendered_frame = None;
        self.viewport.pan = (0.0, 0.0);
        self.remember_settings(cx);
        if let Some(output) = &self.audio_output {
            output.set_source(None);
        }
        self.audio_status = audio::Status::default();
        // A terminal session has no running worker. Connect retries with the edited
        // address/password in one action rather than requiring Disconnect first.
        if self.snapshot.state == State::Failed {
            if let Some(session) = self.session.take() {
                cx.background_executor()
                    .spawn(async move { drop(session) })
                    .detach();
            }
            self.connection_address = None;
            self.snapshot = Snapshot::default();
        }
        if let Some(old) = self.image.take() {
            cx.drop_image(old, Some(window));
        }
        if let Some(session) = self.session.take() {
            self.connection_address = None;
            session.request_stop();
            cx.background_executor()
                .spawn(async move {
                    drop(session);
                })
                .detach();
            self.snapshot = Snapshot::default();
        } else {
            let port = self
                .port
                .read(cx)
                .value()
                .parse::<u16>()
                .ok()
                .filter(|&p| p > 0);
            let Some(port) = port else {
                self.message = Some("Port must be in 1..=65535".into());
                cx.notify();
                return;
            };
            match Config::new(
                self.host.read(cx).value().as_str(),
                port,
                self.password.read(cx).value().to_string(),
            )
            .and_then(|mut config| {
                config.audio_enabled = self.audio_settings.enabled || self.analyzer.is_some();
                config.fps_limit = self.viewer_settings.fps_limit;
                let address = (config.host.clone(), config.port);
                Session::start(config)
                    .map(|session| (session, address))
                    .map_err(|e| format!("Could not start connection worker: {e}"))
            }) {
                Ok((session, address)) => {
                    session
                        .audio()
                        .set_target_buffer_ms(self.audio_settings.target_buffer_ms);
                    session.audio().set_volume(self.audio_settings.volume);
                    session
                        .audio()
                        .set_analyzer(self.analyzer.as_ref().map(|a| a.input.clone()));
                    session.control().set_enabled(self.assistance_on);
                    if let Some(teacher) = &self.teacher {
                        teacher.teacher().set_session(&session);
                    }
                    self.session = Some(session);
                    self.connection_address = Some(address);
                    self.snapshot.state = State::Connecting;
                }
                Err(error) => self.message = Some(error),
            }
        }
        window.focus(&self.focus);
        cx.notify();
    }
    fn play_audio(&mut self, _: &PlayAudio, _: &mut Window, cx: &mut Context<Self>) {
        self.audio_settings.enabled = !self.audio_settings.enabled;
        if let Some(session) = &self.session {
            // Immediate mute; negotiation/reset is serialized by the session worker.
            if !self.audio_settings.enabled {
                if let Some(output) = &self.audio_output {
                    output.set_source(None);
                }
                if self.analyzer.is_none() {
                    session.audio().set_enabled(false);
                }
            }
            if session
                .command(Command::Audio(
                    self.audio_settings.enabled || self.analyzer.is_some(),
                ))
                .is_err()
            {
                session.request_stop();
                self.message = Some(
                    "Audio command queue full; connection stopped. Reconnect to retry.".into(),
                );
            }
        }
        self.update_audio_settings(cx);
    }
    fn update_audio_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(output) = &self.audio_output {
            output.set_settings(self.audio_settings.clone());
        }
        if let Some(session) = &self.session {
            let sink = session.audio();
            sink.set_volume(self.audio_settings.volume);
            sink.set_target_buffer_ms(self.audio_settings.target_buffer_ms);
        }
        cx.notify();
    }
    fn audio_settings(&mut self, _: &AudioSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(SettingsTab::Audio, window, cx);
    }
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.command(Command::Refresh, cx);
    }
    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.snapshot.state == State::Streaming
            && let Some(session) = &self.session
            && session.command(command).is_err()
        {
            self.message = Some("Connection command queue is full; try again.".into());
        }
        cx.notify();
    }
    fn capabilities(&self) -> u8 {
        if self.snapshot.state != State::Streaming || !self.control_on || self.show_settings {
            return 0;
        }
        self.snapshot
            .screen
            .as_ref()
            .map_or(0, |s| s.input_capabilities)
    }
    fn send_input(&mut self, events: Vec<InputEvent>, cx: &mut Context<Self>) {
        if events.is_empty() {
            return;
        }
        if let (Some(session), Some(screen)) = (&self.session, &self.snapshot.framebuffer) {
            for batch in events.chunks(64) {
                if session
                    .command(Command::Input {
                        screen: screen.clone(),
                        events: batch.to_vec(),
                    })
                    .is_err()
                {
                    session.request_stop();
                    self.control_on = false;
                    self.remember_settings(cx);
                    self.control.clear();
                    self.message = Some("Input queue full; connection stopped for safety.".into());
                    break;
                }
            }
        }
        cx.notify();
    }
    fn release_input(&mut self, cx: &mut Context<Self>) {
        if self.capabilities() != 0 && self.control.is_held() {
            self.send_input(vec![InputEvent::RELEASE_ALL], cx);
        }
        self.control.clear();
        self.drag = None;
    }
    fn remote_control(&mut self, _: &RemoteControl, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            return;
        }
        self.release_input(cx);
        self.control_on = !self.control_on
            && self.snapshot.state == State::Streaming
            && self
                .snapshot
                .screen
                .as_ref()
                .is_some_and(|s| s.input_capabilities != 0);
        window.focus(&self.focus);
        self.remember_settings(cx);
    }
    fn remote_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || self.capabilities() & 4 == 0 {
            return;
        }
        let k = &event.keystroke;
        if matches!(k.key.as_str(), "f11" | "f12")
            || (k.key == "escape" && window.is_fullscreen())
            || (k.modifiers.platform && k.key == ",")
        {
            return;
        }
        let m = k.modifiers;
        let name = if m.control || m.alt || m.platform || k.key.chars().count() != 1 {
            k.key.as_str()
        } else {
            k.key_char.as_deref().unwrap_or(&k.key)
        };
        let events = self
            .control
            .key(name, [m.shift, m.control, m.alt, m.platform]);
        self.send_input(events, cx);
        cx.stop_propagation();
    }
    fn pointer_position(&self, position: Point<Pixels>, clamp: bool) -> Option<(u16, u16)> {
        self.picture.pointer(
            (position.x.into(), position.y.into()),
            self.snapshot.screen.as_ref()?.upright_size(),
            clamp,
        )
    }
    fn pointer_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        if self.capabilities() != 0 {
            let Some(p) = self.pointer_position(event.position, false) else {
                return;
            };
            let id = match event.button {
                MouseButton::Left if event.modifiers.control => 1,
                MouseButton::Left => 0,
                MouseButton::Right => 1,
                MouseButton::Middle => 2,
                _ => return,
            };
            let event = if self.capabilities() & 2 != 0 {
                if event.button == MouseButton::Left {
                    self.control.left_button = id as u8;
                }
                self.control.buttons[id] = true;
                input::pointer(4, id as u8, p, 0)
            } else if self.capabilities() & 1 != 0 && id == 0 {
                self.control.touch = true;
                input::pointer(0, 0, p, 0)
            } else {
                return;
            };
            self.control.last = Some(p);
            self.send_input(vec![event], cx);
        } else if !self.control_on {
            if event.click_count == 2 {
                self.viewport.actual_size = !self.viewport.actual_size;
                self.remember_settings(cx);
            } else if self.viewport.actual_size {
                self.drag = Some(event.position);
            }
        }
        cx.notify();
    }
    fn pointer_up(&mut self, event: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag = None;
        if let Some(p) = self.pointer_position(event.position, true) {
            let id = match event.button {
                MouseButton::Left => self.control.left_button as usize,
                MouseButton::Right => 1,
                MouseButton::Middle => 2,
                _ => return,
            };
            let mut events = Vec::new();
            if self.control.buttons[id] {
                events.push(input::pointer(5, id as u8, p, 0));
                self.control.buttons[id] = false;
            }
            if self.control.touch && event.button == MouseButton::Left {
                events.push(input::pointer(2, 0, p, 0));
                self.control.touch = false;
            }
            self.send_input(events, cx);
        }
    }
    fn fit(&mut self, _: &Fit, _: &mut Window, cx: &mut Context<Self>) {
        self.viewport.actual_size = false;
        self.remember_settings(cx);
    }
    fn actual_size(&mut self, _: &ActualSize, _: &mut Window, cx: &mut Context<Self>) {
        self.viewport.actual_size = true;
        self.remember_settings(cx);
    }
    fn fullscreen(&mut self, _: &Fullscreen, window: &mut Window, cx: &mut Context<Self>) {
        window.toggle_fullscreen();
        cx.notify();
    }
    fn exit_fullscreen(&mut self, _: &ExitFullscreen, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_fullscreen() {
            window.toggle_fullscreen();
            cx.notify();
        }
    }
    fn statistics(&mut self, _: &Statistics, _: &mut Window, cx: &mut Context<Self>) {
        self.show_stats = !self.show_stats;
        self.remember_settings(cx);
    }
    fn device_information(
        &mut self,
        _: &DeviceInformation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Opening an informational surface must not leave a remote drag/modifier held.
        self.release_input(cx);
        self.show_information = !self.show_information;
        cx.notify();
    }
    fn settings(&mut self, _: &Settings, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::WindowExt;
        if self.show_settings {
            window.close_dialog(cx);
            self.show_settings = false;
            self.settings_dialog_focus = None;
            cx.notify();
        } else {
            self.open_settings(SettingsTab::General, window, cx);
        }
    }
    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        if let Some(session) = &self.session {
            session.audio().set_recording(None);
        }
        self.recording_started = None;
        self.recording_seconds = 0;
        self.recording_pending = true;
        self.message = Some("Finalizing recording…".into());
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { recorder.finish() })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.recording_pending = false;
                view.message = Some(
                    result
                        .map(|p| format!("Saved recording: {}", p.display()))
                        .unwrap_or_else(|e| e),
                );
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn recording(&mut self, _: &Recording, _: &mut Window, cx: &mut Context<Self>) {
        if self.recorder.is_some() {
            self.stop_recording(cx);
            return;
        }
        if self.recording_pending {
            return;
        }
        let Some(session) = &self.session else { return };
        if self.snapshot.state != State::Streaming || self.snapshot.framebuffer.is_none() {
            self.message = Some("No screen to record yet".into());
            cx.notify();
            return;
        }
        let state = session.recording_source();
        let generation = self.generation;
        let folder = self
            .screenshot_folder
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let filename = format!(
            "Remote Screen {}.mov",
            chrono::Local::now().format("%Y-%m-%d %H.%M.%S")
        );
        let picker = cx.prompt_for_new_path(&folder, Some(&filename));
        self.recording_pending = true;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = match picker.await {
                Ok(Ok(Some(path))) => Some(
                    cx.background_executor()
                        .spawn(async move {
                            Recorder::start(path, state)
                                .map_err(|e| format!("Could not start recording: {e}"))
                        })
                        .await,
                ),
                Ok(Ok(None)) => None,
                _ => Some(Err("Could not open recording save dialog".into())),
            };
            let _ = view.update(cx, |view, cx| {
                view.recording_pending = false;
                match result {
                    Some(Ok(recorder))
                        if view.generation == generation && view.session.is_some() =>
                    {
                        view.session
                            .as_ref()
                            .unwrap()
                            .audio()
                            .set_recording(Some(recorder.audio.clone()));
                        view.recorder = Some(recorder);
                        view.recording_started = Some(Instant::now());
                        view.recording_seconds = 0;
                        view.message = Some(
                            if view.audio_settings.enabled {
                                "Recording device screen and available device audio"
                            } else {
                                "Recording screen — no sound (device audio is off)"
                            }
                            .into(),
                        );
                    }
                    Some(Ok(recorder)) => {
                        cx.background_executor()
                            .spawn(async move {
                                let _ = recorder.finish();
                            })
                            .detach();
                    }
                    Some(Err(error)) => view.message = Some(error),
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn save_screenshot(&mut self, _: &SaveScreenshot, window: &mut Window, cx: &mut Context<Self>) {
        self.capture(true, window, cx);
    }
    fn desktop_screenshot(
        &mut self,
        _: &DesktopScreenshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture(false, window, cx);
    }
    fn choose_screenshot_folder(
        &mut self,
        capture_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.capture_pending {
            return;
        }
        self.capture_pending = true;
        self.message = Some("Choose the default folder for PNG screenshots.".into());
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose screenshot folder".into()),
        });
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let selection = match picker.await {
                Ok(Ok(Some(paths))) => Ok(paths.into_iter().next()),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(format!("Could not open folder dialog: {error}")),
                Err(_) => Err("Folder dialog closed unexpectedly".into()),
            };
            let result = match selection {
                Ok(Some(folder)) => {
                    cx.background_executor()
                        .spawn(async move {
                            let settings = preferences::screenshot_settings_path()
                                .map_err(|e| e.to_string())?;
                            preferences::save_folder(&settings, &folder)
                                .map_err(|e| format!("Could not save screenshot folder: {e}"))?;
                            Ok(Some(folder))
                        })
                        .await
                }
                other => other,
            };
            let _ = view.update_in(cx, |view, window, cx| {
                view.capture_pending = false;
                match result {
                    Ok(Some(folder)) => {
                        view.message = Some(format!("Screenshot folder: {}", folder.display()));
                        view.screenshot_folder = Some(folder);
                        if capture_after {
                            view.capture(false, window, cx);
                        }
                    }
                    Ok(None) => view.message = None,
                    Err(error) => view.message = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn capture(&mut self, choose: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.capture_pending {
            self.message = Some("A screenshot is already in progress".into());
            cx.notify();
            return;
        }
        let Some(framebuffer) = self.snapshot.framebuffer.clone() else {
            self.message = Some("No screen to capture yet".into());
            cx.notify();
            return;
        };
        if !choose && !self.screenshot_folder.as_ref().is_some_and(|p| p.is_dir()) {
            self.choose_screenshot_folder(true, window, cx);
            return;
        }
        let folder = self
            .screenshot_folder
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let now = chrono::Local::now();
        let filename = if choose {
            format!("remote-screen-{}.png", now.format("%Y%m%d-%H%M%S"))
        } else {
            format!("Remote Screen {}.png", now.format("%Y-%m-%d %H.%M.%S"))
        };
        let picker = choose.then(|| cx.prompt_for_new_path(&folder, Some(&filename)));
        self.capture_pending = true;
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let path = if let Some(picker) = picker {
                match picker.await {
                    Ok(Ok(Some(path))) => Some(Ok(path)),
                    Ok(Ok(None)) => None,
                    Ok(Err(error)) => Some(Err(format!("Could not open save dialog: {error}"))),
                    Err(_) => Some(Err("Save dialog closed unexpectedly".into())),
                }
            } else {
                Some(Ok(folder.join(filename)))
            };
            let Some(path) = path else {
                let _ = view.update(cx, |view, cx| {
                    view.capture_pending = false;
                    cx.notify();
                });
                return;
            };
            let result = match path {
                Err(error) => Err(error),
                Ok(path) => {
                    cx.background_executor()
                        .spawn(async move {
                            let image = framebuffer
                                .read()
                                .map_err(|_| "Framebuffer unavailable".to_owned())?
                                .upright_snapshot();
                            let (file, path) = create_capture(path, !choose)
                                .map_err(|e| format!("Could not create screenshot: {e}"))?;
                            let mut writer = BufWriter::new(file);
                            let result = image
                                .write_to(&mut writer, image::ImageFormat::Png)
                                .map_err(|e| e.to_string())
                                .and_then(|_| writer.flush().map_err(|e| e.to_string()));
                            result.map_err(|e| {
                                format!(
                                    "Could not finish PNG at {} (partial file retained): {e}",
                                    path.display()
                                )
                            })?;
                            Ok(format!("Saved {}", path.display()))
                        })
                        .await
                }
            };
            let _ = view.update(cx, |view, cx| {
                view.capture_pending = false;
                let saved = result.as_ref().ok().cloned();
                view.message = Some(result.unwrap_or_else(|e| e));
                if let Some(saved) = saved {
                    cx.spawn(async move |view, cx| {
                        cx.background_executor().timer(Duration::from_secs(3)).await;
                        let _ = view.update(cx, |view, cx| {
                            // Do not clear an error or another notification that replaced this one.
                            if view.message.as_ref() == Some(&saved) {
                                view.message = None;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn information_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::{IconName, Sizable, button::ButtonVariants};
        let sections = information::device_information(
            &self.snapshot,
            self.connection_address
                .as_ref()
                .map(|(host, port)| (host.as_str(), *port)),
            &self.audio_status,
            self.output_status.active.as_deref(),
        );
        div()
            .id("information-panel")
            .occlude()
            .absolute().right_0().top_0().bottom_0()
            .w(px(440.0)).max_w_full()
            .bg(rgb(theme::CHROME)).border_l_1().border_color(rgb(theme::BORDER))
            .flex().flex_col()
            .child(div().px_4().py_3().border_b_1().border_color(rgb(theme::BORDER)).flex().items_center().justify_between()
                .child("Device information")
                .child(Button::new("close-information").small().ghost().icon(IconName::Close).tooltip("Close inspector")
                    .on_click(cx.listener(|view, _, window, cx| view.device_information(&DeviceInformation, window, cx)))))
            .child(div().id("information-scroll").p_3().flex_1().min_h_0().overflow_y_scroll()
                .flex().flex_col().gap_3()
                .children(sections.into_iter().map(|section| {
                    div().flex().flex_col().gap_2().flex_shrink_0().pb_3().border_b_1().border_color(rgb(theme::BORDER))
                        .child(div().text_color(rgb(theme::ACCENT)).child(section.title))
                        .children(section.rows.into_iter().map(|(label, value)| {
                            div().flex().items_start().gap_3()
                                .child(div().w(px(120.0)).flex_shrink_0().text_size(px(12.0)).text_color(rgb(theme::MUTED)).child(label))
                                .child(div().flex_1().min_w_0().text_sm().child(value))
                        }))
                }))
                .child(div().text_color(rgb(theme::MUTED)).child(format!("Rate history · latest {HISTORY_LIMIT} samples")))
                .child("Intervals ≥1 s, using actual elapsed time. Includes all received messages, not GUI paints.")
                .when(self.snapshot.state != State::Streaming, |this| this.child("No live stream; any history below is from the last attempt."))
                .when(self.snapshot.statistics.history().is_empty(), |this| this.child("No rate samples yet."))
                .when(!self.snapshot.statistics.history().is_empty(), |this| this
                    .child(self.rate_chart("Frame rate", "fps", 0x66bbcc, |s| s.rates.frames_per_second))
                    .child(self.rate_chart("Receive bandwidth", "kbit/s", 0xccaa66, |s| s.rates.kilobits_per_second))))
            .into_any_element()
    }
    fn rate_chart(
        &self,
        label: &str,
        unit: &str,
        color: u32,
        value: fn(&Sample) -> f64,
    ) -> AnyElement {
        let history = self.snapshot.statistics.history();
        let start = history
            .front()
            .map(|s| s.at - s.interval)
            .unwrap_or_default();
        let samples: Vec<_> = history
            .iter()
            .map(|s| {
                (
                    (s.at - s.interval - start).as_secs_f64(),
                    (s.at - start).as_secs_f64(),
                    value(s),
                )
            })
            .collect();
        let duration = samples.last().map(|s| s.1).unwrap_or(1.0).max(1.0);
        let peak = samples.iter().map(|s| s.2).fold(0.0_f64, f64::max);
        let ceiling = peak.max(1.0); // Silence/static video must not divide by zero.
        div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_shrink_0()
            .child(format!("{label} · peak {peak:.1} {unit} · {duration:.1} s"))
            .child(
                div().h(px(64.0)).bg(rgb(0x101010)).child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            if samples.is_empty() {
                                return;
                            }
                            let width = (f32::from(bounds.size.width) - 4.0).max(0.0);
                            let height = (f32::from(bounds.size.height) - 4.0).max(0.0);
                            let mut path = PathBuilder::stroke(px(1.5));
                            for (index, (from, to, value)) in samples.iter().enumerate() {
                                let y = px(2.0 + height * (1.0 - (value / ceiling) as f32));
                                let left = bounds.origin
                                    + point(px(2.0 + width * (from / duration) as f32), y);
                                let right = bounds.origin
                                    + point(px(2.0 + width * (to / duration) as f32), y);
                                if index == 0 {
                                    path.move_to(left);
                                } else {
                                    path.line_to(left);
                                }
                                path.line_to(right);
                            }
                            if let Ok(path) = path.build() {
                                window.paint_path(path, rgb(color));
                            }
                        },
                    )
                    .size_full(),
                ),
            )
            .into_any_element()
    }
    fn screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let image = self.image.clone();
        let highlight = self.teacher_highlight.clone();
        let feedback = self
            .teacher_control
            .as_ref()
            .and_then(|s| s.feedback.clone());
        let size = self
            .snapshot
            .screen
            .as_ref()
            .map(|s| s.upright_size())
            .unwrap_or((1, 1));
        let view = cx.weak_entity();
        let empty = image.is_none();
        div()
            .id("screen")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(rgb(0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                    view.pointer_down(event, window, cx);
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(Self::pointer_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::pointer_up))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::pointer_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::pointer_down))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::pointer_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::pointer_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::pointer_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::pointer_up))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                if view.control_on {
                    let held = view.control.touch || view.control.buttons.iter().any(|v| *v);
                    if !held && view.assistance_on {
                        return; // Hover alone is not takeover; press/scroll/key or Stop is.
                    }
                    if let Some(p) = view.pointer_position(event.position, held)
                        && view.control.last != Some(p)
                    {
                        view.control.last = Some(p);
                        let kind = if view.control.touch {
                            1
                        } else if view.capabilities() & 2 != 0 {
                            3
                        } else {
                            return;
                        };
                        view.send_input(vec![input::pointer(kind, 0, p, 0)], cx);
                    }
                    return;
                }
                if !event.dragging() {
                    view.drag = None;
                    return;
                }
                if let Some(previous) = view.drag.replace(event.position)
                    && view.viewport.actual_size
                {
                    view.viewport.pan.0 -= f32::from(event.position.x - previous.x);
                    view.viewport.pan.1 -= f32::from(event.position.y - previous.y);
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| {
                if view.control_on {
                    if view.capabilities() & 2 != 0
                        && let Some(p) = view.pointer_position(event.position, false)
                    {
                        let delta = event.delta.pixel_delta(px(1.0));
                        let steps = (f32::from(delta.y) * 3.0)
                            .round()
                            .clamp(i16::MIN as f32, i16::MAX as f32)
                            as i16;
                        if steps != 0 {
                            view.send_input(vec![input::pointer(6, 0, p, steps)], cx);
                        }
                    }
                    return;
                }
                if view.viewport.actual_size {
                    let delta = event.delta.pixel_delta(px(40.0));
                    if event.modifiers.shift {
                        view.viewport.pan.0 -= f32::from(delta.y);
                    } else {
                        view.viewport.pan.0 -= f32::from(delta.x);
                        view.viewport.pan.1 -= f32::from(delta.y);
                    }
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        view.update(cx, |view, _| {
                            let picture = view.viewport.picture(
                                (bounds.size.width.into(), bounds.size.height.into()),
                                size,
                            );
                            view.picture = Picture {
                                left: f32::from(bounds.origin.x) + picture.left,
                                top: f32::from(bounds.origin.y) + picture.top,
                                ..picture
                            };
                            Bounds::new(
                                bounds.origin + point(px(picture.left), px(picture.top)),
                                gpui::size(px(picture.width), px(picture.height)),
                            )
                        })
                        .ok()
                    },
                    move |canvas_bounds, picture, window, cx| {
                        if let (Some(image), Some(picture)) = (image, picture) {
                            let _ =
                                window.paint_image(picture, Corners::default(), image, 0, false);
                            if let Some(feedback) = feedback {
                                teacher::paint_feedback(
                                    &feedback,
                                    picture,
                                    canvas_bounds,
                                    size,
                                    window,
                                    cx,
                                );
                            }
                            if let Some(highlight) = highlight {
                                teacher::paint_highlight(
                                    &highlight,
                                    picture,
                                    canvas_bounds,
                                    size,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            )
            .when(empty, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(rgb(0xaaaaaa))
                        .child("Preparing device screen…"),
                )
            })
            .into_any_element()
    }
}
impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::Root;
        let fullscreen = window.is_fullscreen();
        let content = if self.snapshot.framebuffer.is_some() {
            self.screen(cx)
        } else {
            self.connection_card(cx)
        };
        let information_panel = self.show_information.then(|| self.information_panel(cx));
        let toolbar = (!fullscreen).then(|| self.toolbar(window, cx));
        let status_bar = (!fullscreen).then(|| self.status_bar(window, cx));
        let notifications = self.notifications(cx);
        // Layers must be rendered after the workspace. The settings body is a
        // separate observing entity, never a recursive render of Viewer.
        let dialogs = Root::render_dialog_layer(window, cx);
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(theme::BACKGROUND))
            .text_color(rgb(theme::TEXT))
            .key_context(if self.show_settings {
                "ViewerModal"
            } else if self.capabilities() & 4 != 0 {
                "Viewer RemoteControl"
            } else {
                "Viewer"
            })
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::remote_key))
            .on_modifiers_changed(cx.listener(
                |view, event: &gpui::ModifiersChangedEvent, window, cx| {
                    if view.focus.is_focused(window) && view.capabilities() & 4 != 0 {
                        let m = event.modifiers;
                        let events = view
                            .control
                            .modifiers([m.shift, m.control, m.alt, m.platform]);
                        view.send_input(events, cx);
                    }
                },
            ))
            .on_action(cx.listener(Self::remote_control))
            .on_action(cx.listener(Self::play_audio))
            .on_action(cx.listener(Self::audio_settings))
            .on_action(cx.listener(Self::toggle_analyzer))
            .on_action(cx.listener(Self::toggle_teacher))
            .on_action(cx.listener(Self::toggle_assistance))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    view.resizing_analyzer = false;
                }
                if view.resizing_analyzer {
                    view.analyzer_height = (f32::from(window.viewport_size().height)
                        - f32::from(event.position.y)
                        - 28.0)
                        .clamp(
                            120.0,
                            (f32::from(window.viewport_size().height) * 0.65).max(120.0),
                        );
                    view.remember_settings(cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.resizing_analyzer = false),
            )
            .on_action(cx.listener(Self::toggle_connection))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::fit))
            .on_action(cx.listener(Self::actual_size))
            .on_action(cx.listener(Self::fullscreen))
            .on_action(cx.listener(Self::exit_fullscreen))
            .on_action(cx.listener(Self::statistics))
            .on_action(cx.listener(Self::settings))
            .on_action(cx.listener(|view, _: &SettingsNextFocus, window, cx| {
                view.cycle_settings_focus(false, window, cx)
            }))
            .on_action(cx.listener(|view, _: &SettingsPreviousFocus, window, cx| {
                view.cycle_settings_focus(true, window, cx)
            }))
            .on_action(cx.listener(Self::device_information))
            .on_action(cx.listener(Self::save_screenshot))
            .on_action(cx.listener(Self::desktop_screenshot))
            .on_action(cx.listener(Self::recording))
            .when_some(toolbar, |this, toolbar| this.child(toolbar))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(content)
                    .when_some(information_panel, |this, panel| this.child(panel))
                    .when(self.analyzer.is_some(), |this| {
                        this.child(self.analyzer_panel(cx))
                    })
                    .child(notifications),
            )
            .when(fullscreen, |this| {
                this.child(
                    div().absolute().top_2().right_2().occlude().child(
                        Button::new("exit-fullscreen")
                            .label("Exit fullscreen · Esc / F11")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.exit_fullscreen(&ExitFullscreen, window, cx)
                            })),
                    ),
                )
            })
            .when(fullscreen && self.teacher.is_some(), |this| {
                this.child(
                    div()
                        .absolute()
                        .top_2()
                        .left_2()
                        .occlude()
                        .child(self.teacher_indicator(cx)),
                )
            })
            // Reserve a safety row OUTSIDE the workspace: neither its analyzer nor
            // its bottom-anchored notifications can obscure or intercept Stop.
            .when(
                self.teacher.is_some()
                    && !self.show_settings
                    && (self.assistance_on
                        || self
                            .teacher_control
                            .as_ref()
                            .and_then(|s| s.feedback.as_ref())
                            .is_some_and(|f| f.phase.active() || f.overlay_visible)),
                |this| this.child(self.teacher_controls(cx)),
            )
            .when_some(status_bar, |this, bar| this.child(bar))
            .children(dialogs)
    }
}

fn create_capture(
    path: PathBuf,
    avoid_collision: bool,
) -> std::io::Result<(std::fs::File, PathBuf)> {
    for suffix in 0..10000 {
        let candidate = if suffix == 0 {
            path.clone()
        } else {
            path.with_file_name(format!(
                "{} ({suffix}).png",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ))
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if avoid_collision && error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Too many same-name screenshots; choose another path",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_tracks_pixels_and_screen_identity_not_network_revisions() {
        use mpc_remote_screen_viewer_gpui::{framebuffer::Framebuffer, protocol::*};
        let init = ServerInit {
            width: 1,
            height: 1,
            rotation: 0,
            pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
            name: "test".into(),
            input_capabilities: 0,
            audio_available: false,
        };
        let framebuffer = Arc::new(std::sync::RwLock::new(
            Framebuffer::new(init.clone()).unwrap(),
        ));
        assert!(image_needs_update(None, &framebuffer));
        let rendered = (framebuffer.clone(), 0);
        assert!(!image_needs_update(Some(&rendered), &framebuffer));
        let mut frame = FrameUpdate {
            id: 1,
            quality: 0,
            rectangles: vec![],
        };
        framebuffer.write().unwrap().apply(&frame);
        assert!(!image_needs_update(Some(&rendered), &framebuffer));
        frame.rectangles.push(Rectangle {
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            codec: 99,
            drop_bits: 0,
            encoded: &[],
        });
        framebuffer.write().unwrap().apply(&frame);
        assert!(!image_needs_update(Some(&rendered), &framebuffer));
        frame.rectangles[0].codec = 3;
        frame.rectangles[0].encoded = &[0, 0, 255, 0];
        framebuffer.write().unwrap().apply(&frame);
        assert!(image_needs_update(Some(&rendered), &framebuffer));
        let replacement = Arc::new(std::sync::RwLock::new(Framebuffer::new(init).unwrap()));
        assert!(image_needs_update(Some(&rendered), &replacement));
        let held = replacement.write().unwrap();
        assert!(!image_needs_update(Some(&rendered), &replacement));
        drop(held);
        assert!(image_needs_update(Some(&rendered), &replacement));
    }

    #[test]
    fn captures_never_overwrite_and_propagate_filesystem_errors() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mpc-capture-test-{:016x}",
            u64::from_le_bytes(nonce)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("screen.png");
        std::fs::write(&path, b"original").unwrap();
        assert_eq!(
            create_capture(path.clone(), false).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        let (file, alternate) = create_capture(path.clone(), true).unwrap();
        drop(file);
        assert_eq!(alternate.file_name().unwrap(), "screen (1).png");
        assert_eq!(std::fs::read(path).unwrap(), b"original");
        assert_eq!(
            create_capture(directory.join("missing/screen.png"), true)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
