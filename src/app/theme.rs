//! Shared workspace tokens and embedded line icons. No runtime asset downloads.
use gpui::{App, AssetSource, SharedString, px, rgb};
use gpui_component::Theme;
use std::borrow::Cow;

pub const BACKGROUND: u32 = 0x111214;
pub const CHROME: u32 = 0x1b1d20;
pub const PANEL: u32 = 0x222429;
pub const BORDER: u32 = 0x34373d;
pub const TEXT: u32 = 0xe8eaed;
pub const MUTED: u32 = 0xa0a6b0;
pub const ACCENT: u32 = 0x669df1;
pub const DANGER: u32 = 0xe57373;
pub const SUCCESS: u32 = 0x81b89a;
pub const WARNING: u32 = 0xe4bc79;

pub fn apply(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.font_size = px(14.0);
    theme.radius = px(5.0);
    theme.radius_lg = px(10.0);
    theme.background = rgb(CHROME).into();
    theme.foreground = rgb(TEXT).into();
    theme.border = rgb(BORDER).into();
    theme.input = rgb(BACKGROUND).into();
    theme.muted = rgb(PANEL).into();
    theme.muted_foreground = rgb(MUTED).into();
    theme.primary = rgb(0x477bc4).into();
    theme.primary_hover = rgb(0x568bd5).into();
    theme.primary_active = rgb(0x3d6aaa).into();
    theme.primary_foreground = rgb(0xffffff).into();
    theme.ring = rgb(ACCENT).into();
    theme.caret = rgb(ACCENT).into();
    theme.secondary = rgb(PANEL).into();
    theme.secondary_hover = rgb(0x30343b).into();
    theme.secondary_active = rgb(0x393e47).into();
    theme.secondary_foreground = rgb(TEXT).into();
    theme.accent = rgb(0x30343b).into();
    theme.accent_foreground = rgb(TEXT).into();
    theme.popover = rgb(PANEL).into();
    theme.popover_foreground = rgb(TEXT).into();
    theme.list = rgb(PANEL).into();
    theme.list_hover = rgb(0x30343b).into();
    theme.list_active = rgb(0x324765).into();
    theme.list_active_border = rgb(ACCENT).into();
    theme.switch = rgb(0x484d56).into();
    theme.slider_bar = rgb(ACCENT).into();
}

// SVGs are authored here, including the small set required by the component controls.
macro_rules! icon {
    ($body:literal) => {
        concat!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">"##, $body, "</svg>")
    };
}
const ICONS: &[(&str, &str)] = &[
    (
        "monitor",
        icon!(r#"<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8m-4-4v4"/>"#),
    ),
    ("pointer", icon!(r#"<path d="m5 3 14 9-7 1-3 7Z"/>"#)),
    (
        "volume",
        icon!(r#"<path d="M11 4 6 8H3v8h3l5 4Zm4 4a6 6 0 0 1 0 8m3-11a10 10 0 0 1 0 14"/>"#),
    ),
    (
        "volume-off",
        icon!(r#"<path d="M11 4 6 8H3v8h3l5 4Zm5 5 5 6m0-6-5 6"/>"#),
    ),
    (
        "analyzer",
        icon!(r#"<path d="M4 15v4m4-9v9m4-14v14m4-11v11m4-7v7"/>"#),
    ),
    (
        "spectrogram",
        icon!(
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 8v3m0 3v2m5-9v2m0 3v5m5-9v5m0 3v1"/>"#
        ),
    ),
    (
        "waterfall",
        icon!(r#"<path d="m3 9 5-5 4 4 4-3 5 4M3 14l5-5 4 4 4-3 5 4M3 19l5-5 4 4 4-3 5 4"/>"#),
    ),
    (
        "waveform",
        icon!(r#"<path d="M2 12h2l2-5 3 12 3-16 3 13 3-7 2 3h2"/>"#),
    ),
    (
        "stereo-width",
        icon!(
            r#"<path d="m12 3 9 9-9 9-9-9ZM12 3v18M3 12h18"/><ellipse cx="12" cy="12" rx="4" ry="7"/>"#
        ),
    ),
    (
        "camera",
        icon!(
            r#"<path d="m8 5 1-2h6l1 2h4a1 1 0 0 1 1 1v14H3V6a1 1 0 0 1 1-1Z"/><circle cx="12" cy="12" r="4"/>"#
        ),
    ),
    (
        "record",
        icon!(r#"<circle cx="12" cy="12" r="7" fill="black" stroke="none"/>"#),
    ),
    (
        "stop",
        icon!(r#"<rect x="5" y="5" width="14" height="14" rx="2" fill="black" stroke="none"/>"#),
    ),
    (
        "refresh",
        icon!(r#"<path d="M20 8a8 8 0 1 0 0 8M20 3v5h-5"/>"#),
    ),
    (
        "settings",
        icon!(
            r#"<path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3" fill="white"/><circle cx="15" cy="17" r="3" fill="white"/>"#
        ),
    ),
    (
        "maximize",
        icon!(r#"<path d="M9 3H3v6m12-6h6v6M3 15v6h6m12-6v6h-6"/>"#),
    ),
    (
        "ellipsis",
        icon!(
            r#"<circle cx="5" cy="12" r="1" fill="black"/><circle cx="12" cy="12" r="1" fill="black"/><circle cx="19" cy="12" r="1" fill="black"/>"#
        ),
    ),
    (
        "info",
        icon!(r#"<circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10v.1"/>"#),
    ),
    ("close", icon!(r#"<path d="m6 6 12 12M6 18 18 6"/>"#)),
    ("check", icon!(r#"<path d="m4 12 5 5L20 6"/>"#)),
    ("chevron-down", icon!(r#"<path d="m6 9 6 6 6-6"/>"#)),
    ("chevron-right", icon!(r#"<path d="m9 6 6 6-6 6"/>"#)),
    ("chevron-up", icon!(r#"<path d="m6 15 6-6 6 6"/>"#)),
    ("chevron-left", icon!(r#"<path d="m15 6-6 6 6 6"/>"#)),
    (
        "eye",
        icon!(
            r#"<path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7S2 12 2 12Z"/><circle cx="12" cy="12" r="3"/>"#
        ),
    ),
    (
        "eye-off",
        icon!(
            r#"<path d="M3 3 21 21M10 5h2c7 0 10 7 10 7a22 22 0 0 1-3 4M6 6a22 22 0 0 0-4 6s3 7 10 7c2 0 3-.4 4-1"/>"#
        ),
    ),
    ("folder", icon!(r#"<path d="M3 6V4h6l3 3h9v13H3Z"/>"#)),
    (
        "loader-circle",
        icon!(r#"<path d="M20 12a8 8 0 1 1-8-8"/>"#),
    ),
    ("loader", icon!(r#"<path d="M20 12a8 8 0 1 1-8-8"/>"#)),
    (
        "triangle-alert",
        icon!(r#"<path d="m12 3 10 18H2Zm0 6v5m0 3v.1"/>"#),
    ),
];

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"));
        Ok(ICONS
            .iter()
            .find(|(key, _)| Some(*key) == name)
            .map(|(_, svg)| Cow::Borrowed(svg.as_bytes())))
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(if path == "icons" || path == "icons/" {
            ICONS
                .iter()
                .map(|(name, _)| format!("icons/{name}.svg").into())
                .collect()
        } else {
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icons_are_available_without_a_working_directory() {
        for path in Assets.list("icons").unwrap() {
            let data = Assets.load(&path).unwrap().unwrap();
            assert!(data.starts_with(b"<svg"));
            assert!(data.ends_with(b"</svg>"));
        }
        assert!(Assets.load("icons/analyzer.svg").unwrap().is_some());
        assert!(Assets.load("icons/waveform.svg").unwrap().is_some());
        assert!(Assets.load("../secret").unwrap().is_none());
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
