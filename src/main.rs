mod app;

use gpui::{prelude::*, *};
use gpui_component::{Root, Theme, ThemeMode};
use mpc_remote_screen_viewer_gpui::{
    cli::{HELP, Launch},
    preferences,
};

fn main() {
    let mut launch = match Launch::parse(
        std::env::args().skip(1),
        std::env::var("REMOTE_SCREEN_PASSWORD").ok(),
    ) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if launch.help {
        println!("{HELP}");
        return;
    }
    let saved =
        preferences::viewer_settings_path().and_then(|path| preferences::load_viewer(&path));
    let settings_error = saved
        .as_ref()
        .err()
        .map(|e| format!("Could not load viewer settings: {e}"));
    let mut settings = saved.unwrap_or_default();
    launch.apply_preferences(&mut settings);
    Application::new()
        .with_assets(app::theme::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            app::theme::apply(cx);
            app::register_actions(cx);
            let bounds = Bounds::centered(None, size(px(1000.0), px(700.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.0), px(320.0))),
                titlebar: Some(TitlebarOptions {
                    title: Some("MPC Remote Screen Viewer GPUI".into()),
                    ..Default::default()
                }),
                app_id: Some("dev.mpc-remote-screen-viewer".into()),
                ..Default::default()
            };
            if let Err(error) = cx.open_window(options, move |window, cx| {
                let view =
                    cx.new(|cx| app::Viewer::new(launch, settings, settings_error, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            }) {
                eprintln!("Could not open GPUI window: {error}");
                cx.quit();
            }
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
