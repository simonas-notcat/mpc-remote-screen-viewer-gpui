use mpc_remote_screen_viewer_gpui::{
    preferences::{self, MAX_RECENT_DEVICES, ViewerSettings, ViewerSettingsWriter},
    protocol::{DEFAULT_PORT, DeviceInfo},
};
use std::{fs, path::PathBuf};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "mpc-viewer-settings-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn settings(&self) -> PathBuf {
        self.0.join("viewer-settings.json")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn visualizer_state_roundtrips_and_old_settings_keep_defaults() {
    use preferences::VisualizerMode;
    let directory = Directory::new();
    let path = directory.settings();
    fs::write(&path, b"{}").unwrap();
    let defaults = preferences::load_viewer(&path).unwrap();
    assert!(!defaults.analyzer_open);
    assert_eq!(defaults.analyzer_mode, VisualizerMode::Spectrum);
    assert_eq!(defaults.analyzer_height, 220.0);
    assert!(!defaults.show_vectorscope);

    for mode in [
        VisualizerMode::Spectrum,
        VisualizerMode::Spectrogram,
        VisualizerMode::Waterfall,
        VisualizerMode::Waveform,
    ] {
        for open in [false, true] {
            let settings = ViewerSettings {
                analyzer_open: open,
                analyzer_mode: mode,
                analyzer_height: 345.5,
                show_vectorscope: true,
                ..ViewerSettings::default()
            };
            preferences::save_viewer(&path, &settings).unwrap();
            assert_eq!(preferences::load_viewer(&path).unwrap(), settings);
        }
    }
    for height in [0.0, 119.0, f32::NAN, f32::INFINITY] {
        let settings = ViewerSettings {
            analyzer_height: height,
            ..ViewerSettings::default()
        };
        assert!(preferences::save_viewer(&path, &settings).is_err());
    }
}

fn metadata() -> DeviceInfo {
    DeviceInfo {
        model: "Synthetic MPC".into(),
        serial: "TEST-ONLY".into(),
        firmware: "1.2.3".into(),
    }
}

#[test]
fn viewer_defaults_partial_schema_and_atomic_roundtrip_are_separate_from_other_settings() {
    let dir = Directory::new();
    let path = dir.settings();
    let defaults = ViewerSettings::default();
    assert_eq!(preferences::load_viewer(&path).unwrap(), defaults);
    assert!(defaults.host.is_empty());
    assert_eq!(defaults.port, DEFAULT_PORT);
    assert!(!defaults.actual_size && defaults.show_stats && !defaults.show_port);
    assert!(!defaults.control_on);
    assert_eq!(defaults.fps_limit, 0);
    assert!(defaults.recent_devices.is_empty());

    fs::write(&path, br#"{"host":" saved-device ","port":17447}"#).unwrap();
    let mut settings = preferences::load_viewer(&path).unwrap();
    assert_eq!(settings.host, "saved-device");
    assert_eq!(settings.port, 17447);
    assert!(settings.show_stats);
    assert_eq!(settings.version, defaults.version);
    assert!(!settings.control_on);
    settings.actual_size = true;
    settings.show_stats = false;
    settings.show_port = true;
    settings.control_on = true;
    settings.fps_limit = 15;
    settings
        .remember_device("saved-device", 17447, "Screen", Some(&metadata()))
        .unwrap();
    let screenshots = dir.0.join("screenshot-settings.json");
    let audio = dir.0.join("audio-settings.json");
    preferences::save_folder(&screenshots, &dir.0).unwrap();
    preferences::save_audio(&audio, &preferences::AudioSettings::default()).unwrap();
    let other_files = [fs::read(&screenshots).unwrap(), fs::read(&audio).unwrap()];
    preferences::save_viewer(&path, &settings).unwrap();
    assert_eq!(preferences::load_viewer(&path).unwrap(), settings);
    settings.fps_limit = 5;
    preferences::save_viewer(&path, &settings).unwrap();
    assert_eq!(preferences::load_viewer(&path).unwrap(), settings);
    assert_eq!(fs::read(&screenshots).unwrap(), other_files[0]);
    assert_eq!(fs::read(&audio).unwrap(), other_files[1]);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 3);

    let bytes = fs::read(&path).unwrap();
    settings.port = 0;
    assert!(preferences::save_viewer(&path, &settings).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    // A rename failure must retain the destination and remove the temporary file.
    let blocked = dir.0.join("blocked.json");
    fs::create_dir(&blocked).unwrap();
    assert!(preferences::save_viewer(&blocked, &defaults).is_err());
    assert!(blocked.is_dir());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 4);
}

#[test]
fn viewer_rejects_corruption_unknown_schemas_credentials_and_unbounded_files() {
    let dir = Directory::new();
    let path = dir.settings();
    for bytes in [
        "not json",
        "[]",
        "null",
        r#"{"version":2}"#,
        r#"{"version":0}"#,
        r#"{"port":0}"#,
        r#"{"port":65536}"#,
        r#"{"fps_limit":121}"#,
        r#"{"show_stats":"true"}"#,
        r#"{"host":"bad host"}"#,
        r#"{"host":"bad\nhost"}"#,
        r#"{"password":"DO-NOT-ECHO"}"#,
        r#"{"recent_devices":[{"host":"mpc","port":7447,"password":"DO-NOT-ECHO"}]}"#,
        r#"{"recent_devices":[{"host":"","port":7447}]}"#,
        r#"{"recent_devices":[{"host":"mpc","port":0}]}"#,
        r#"{"recent_devices":[{"host":"mpc"}]}"#,
        r#"{"recent_devices":[["mpc",7447,"","","",""]]}"#,
    ] {
        fs::write(&path, bytes).unwrap();
        let error = preferences::load_viewer(&path).unwrap_err();
        assert!(!error.to_string().contains("DO-NOT-ECHO"));
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    }
    fs::write(&path, format!(r#"{{"host":"{}"}}"#, "a".repeat(1025))).unwrap();
    assert!(preferences::load_viewer(&path).is_err());
    fs::File::create(&path)
        .unwrap()
        .set_len(512 * 1024 + 1)
        .unwrap();
    assert!(preferences::load_viewer(&path).is_err());
}

#[test]
fn recent_devices_are_bounded_success_ordered_and_deduplicated_by_address_and_port() {
    let mut settings = ViewerSettings::default();
    settings.remember_address(" typed-only ", 17447).unwrap();
    assert_eq!(settings.host, "typed-only");
    assert!(settings.recent_devices.is_empty()); // typing/attempting is not success
    let before = settings.clone();
    assert!(settings.remember_address("bad host", 7447).is_err());
    assert!(settings.remember_address("other", 0).is_err());
    assert_eq!(settings, before);
    for n in 0..100 {
        settings
            .remember_device(&format!("device-{n}"), 7447, "Screen", None)
            .unwrap();
    }
    assert_eq!(settings.recent_devices.len(), MAX_RECENT_DEVICES);
    assert_eq!(settings.recent_devices[0].host, "device-99");
    assert_eq!(settings.recent_devices[9].host, "device-90");
    settings
        .remember_device(" DEVICE-94 ", 7447, "Updated", Some(&metadata()))
        .unwrap();
    assert_eq!(settings.recent_devices.len(), MAX_RECENT_DEVICES);
    assert_eq!(settings.recent_devices[0].host, "DEVICE-94");
    assert_eq!(settings.recent_devices[0].screen_name, "Updated");
    assert_eq!(settings.recent_devices[0].model, metadata().model);
    assert_eq!(settings.recent_devices[0].serial, metadata().serial);
    assert_eq!(settings.recent_devices[0].firmware, metadata().firmware);
    settings
        .remember_device(
            "device-94",
            7447,
            "",
            Some(&DeviceInfo {
                model: String::new(),
                serial: String::new(),
                firmware: "2.0".into(),
            }),
        )
        .unwrap();
    assert_eq!(settings.recent_devices[0].screen_name, "Updated");
    assert_eq!(settings.recent_devices[0].model, metadata().model);
    assert_eq!(settings.recent_devices[0].firmware, "2.0");
    settings
        .remember_device("device-94", 17447, "", None)
        .unwrap();
    assert_eq!(settings.recent_devices[0].port, 17447);
    assert_eq!(settings.recent_devices[1].port, 7447);
    settings
        .remember_device("2001:db8::1", 7447, "", None)
        .unwrap();
    settings
        .remember_device("2001:0db8:0:0:0:0:0:1", 7447, "", None)
        .unwrap();
    assert_eq!(settings.recent_devices[1].host, "device-94");
    assert!(
        settings.recent_devices[0]
            .label()
            .starts_with("[2001:0db8:0:0:0:0:0:1]:7447")
    );
    settings.validate().unwrap();
}

#[test]
fn recent_load_normalizes_and_labels_remain_bounded_valid_unicode() {
    let dir = Directory::new();
    let path = dir.settings();
    let mut settings = ViewerSettings::default();
    let info = DeviceInfo {
        model: "\nTest\tmodel\r".into(),
        ..metadata()
    };
    settings
        .remember_device("mpc", 7447, &"界".repeat(500), Some(&info))
        .unwrap();
    assert_eq!(settings.recent_devices[0].model, "Test model");
    assert!(settings.recent_devices[0].screen_name.len() <= 1024);
    let mut json = serde_json::to_value(&settings).unwrap();
    let rows = json["recent_devices"].as_array_mut().unwrap();
    let mut duplicate = rows[0].clone();
    duplicate["host"] = " MPC ".into();
    duplicate["model"] = "Should not replace newest".into();
    rows.push(duplicate);
    for n in 0..20 {
        rows.push(serde_json::json!({"host":format!("other-{n}"),"port":7447}));
    }
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let normalized = preferences::load_viewer(&path).unwrap();
    assert_eq!(normalized.recent_devices.len(), MAX_RECENT_DEVICES);
    assert_eq!(normalized.recent_devices[0], settings.recent_devices[0]);
    assert_eq!(normalized.recent_devices[1].host, "other-0");
    preferences::save_viewer(&path, &normalized).unwrap();
    assert_eq!(preferences::load_viewer(&path).unwrap(), normalized);
}

#[test]
fn normal_quit_drains_a_pending_save_without_touching_other_files() {
    let dir = Directory::new();
    let path = dir.settings();
    fs::write(&path, b"untouched malformed settings").unwrap();
    ViewerSettingsWriter::new(path.clone())
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"untouched malformed settings");
    let writer = ViewerSettingsWriter::new(path.clone()).unwrap();
    let settings = ViewerSettings {
        host: "last-edit".into(),
        control_on: true,
        ..ViewerSettings::default()
    };
    writer.request(settings.clone());
    writer.finish().unwrap();
    assert_eq!(preferences::load_viewer(&path).unwrap(), settings);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}
