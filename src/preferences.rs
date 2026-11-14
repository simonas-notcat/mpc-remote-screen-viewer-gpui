//! Separate Rust-only preferences; never contains credentials.
mod viewer;
pub use viewer::{
    MAX_RECENT_DEVICES, RecentDevice, ViewerSettings, ViewerSettingsWriter, VisualizerMode,
    load_viewer, save_viewer, viewer_settings_path,
};

use crate::playout::{DEFAULT_TARGET_BUFFER_MS, MAX_TARGET_BUFFER_MS, MIN_TARGET_BUFFER_MS};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn screenshot_settings_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home.map(|p| p.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.map(|p| p.join(".config")))
    }
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Could not locate the application settings directory",
        )
    })?;
    // Retain the existing settings directory across the project rename.
    Ok(base.join("mpc-remote-screen-viewer-rs/screenshot-settings.json"))
}

pub fn load_folder(settings: &Path) -> io::Result<Option<PathBuf>> {
    let bytes = match fs::read(settings) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    match value.get("screenshot_folder") {
        Some(serde_json::Value::String(path))
            if !path.is_empty() && Path::new(path).is_absolute() =>
        {
            Ok(Some(PathBuf::from(path)))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid screenshot folder setting",
        )),
    }
}

pub fn save_folder(settings: &Path, folder: &Path) -> io::Result<()> {
    if !folder.is_absolute() || !folder.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Choose an existing screenshot folder",
        ));
    }
    let folder = folder.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Folder path is not valid UTF-8",
        )
    })?;
    save_json(settings, &serde_json::json!({"screenshot_folder": folder}))
}

fn save_json(settings: &Path, value: &serde_json::Value) -> io::Result<()> {
    let parent = settings
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid settings path"))?;
    fs::create_dir_all(parent)?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let temporary = parent.join(format!(".settings-{:032x}.tmp", u128::from_le_bytes(nonce)));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, settings)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    pub enabled: bool,
    pub volume: f32,
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_frames: Option<u32>,
    pub target_buffer_ms: u32,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            volume: 1.0,
            device: None,
            sample_rate: None,
            buffer_frames: None,
            target_buffer_ms: DEFAULT_TARGET_BUFFER_MS,
        }
    }
}
impl AudioSettings {
    pub fn validate(&self) -> io::Result<()> {
        if !self.volume.is_finite()
            || !(0.0..=1.0).contains(&self.volume)
            || !(MIN_TARGET_BUFFER_MS..=MAX_TARGET_BUFFER_MS).contains(&self.target_buffer_ms)
            || self
                .sample_rate
                .is_some_and(|r| !(8000..=384000).contains(&r))
            || self
                .buffer_frames
                .is_some_and(|b| !(64..=65536).contains(&b))
            || self
                .device
                .as_ref()
                .is_some_and(|d| d.is_empty() || d.len() > 4096)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid audio settings",
            ));
        }
        Ok(())
    }
}
pub fn audio_settings_path() -> io::Result<PathBuf> {
    Ok(screenshot_settings_path()?.with_file_name("audio-settings.json"))
}
pub fn load_audio(settings: &Path) -> io::Result<AudioSettings> {
    let bytes = match fs::read(settings) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AudioSettings::default()),
        Err(e) => return Err(e),
    };
    let mut value: AudioSettings = serde_json::from_slice(&bytes)?;
    // Retired small-buffer selections must not discard volume/latency preferences
    // or silently keep requesting a 16/32-frame buffer on the next launch.
    if value
        .buffer_frames
        .is_some_and(|frames| (16..64).contains(&frames))
    {
        value.buffer_frames = None;
    }
    value.validate()?;
    Ok(value)
}
pub fn save_audio(settings: &Path, value: &AudioSettings) -> io::Result<()> {
    value.validate()?;
    save_json(settings, &serde_json::to_value(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_roundtrip_validation_and_separate_settings() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "mpc-audio-settings-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        let path = root.join("audio-settings.json");
        assert_eq!(load_audio(&path).unwrap(), AudioSettings::default());
        let mut value = AudioSettings {
            enabled: true,
            volume: 0.5,
            device: Some("Test output".into()),
            sample_rate: Some(44100),
            buffer_frames: Some(256),
            target_buffer_ms: 25,
        };
        save_audio(&path, &value).unwrap();
        assert_eq!(load_audio(&path).unwrap(), value);
        value.device = None;
        save_audio(&path, &value).unwrap();
        assert_eq!(load_audio(&path).unwrap(), value);
        value.volume = f32::NAN;
        assert!(save_audio(&path, &value).is_err());
        assert_eq!(load_audio(&path).unwrap().volume, 0.5);
        // Existing settings written before the latency option retain the 80 ms default.
        fs::write(
            &path,
            br#"{"enabled":true,"volume":0.5,"buffer_frames":32}"#,
        )
        .unwrap();
        let old = load_audio(&path).unwrap();
        assert_eq!(old.target_buffer_ms, DEFAULT_TARGET_BUFFER_MS);
        assert_eq!(old.buffer_frames, None);
        assert!(old.enabled);
        assert_eq!(old.volume, 0.5);
        for frames in [16, 32] {
            fs::write(&path, format!(r#"{{"enabled":true,"volume":0.4,"device":"Test output","sample_rate":44100,"buffer_frames":{frames},"target_buffer_ms":25}}"#)).unwrap();
            let migrated = load_audio(&path).unwrap();
            assert_eq!(migrated.buffer_frames, None);
            assert_eq!(migrated.target_buffer_ms, 25);
            assert_eq!(migrated.volume, 0.4);
            assert_eq!(migrated.device.as_deref(), Some("Test output"));
            assert_eq!(migrated.sample_rate, Some(44100));
            assert!(migrated.enabled);
            save_audio(&path, &migrated).unwrap();
            assert_eq!(load_audio(&path).unwrap(), migrated);
        }
        for target in [10, 80, 300] {
            let settings = AudioSettings {
                target_buffer_ms: target,
                buffer_frames: Some(64),
                ..AudioSettings::default()
            };
            save_audio(&path, &settings).unwrap();
            assert_eq!(load_audio(&path).unwrap(), settings);
        }
        for frames in [16, 32] {
            let retired = AudioSettings {
                buffer_frames: Some(frames),
                ..AudioSettings::default()
            };
            assert!(save_audio(&path, &retired).is_err());
            assert_eq!(load_audio(&path).unwrap().buffer_frames, Some(64));
        }
        for target in [0, 9, 301, u32::MAX] {
            fs::write(&path, format!("{{\"target_buffer_ms\":{target}}}")).unwrap();
            assert!(load_audio(&path).is_err());
        }
        fs::write(&path, br#"{"volume":2}"#).unwrap();
        assert!(load_audio(&path).is_err());
        fs::write(&path, br#"{"password":"never accept credentials"}"#).unwrap();
        assert!(load_audio(&path).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn folder_roundtrip_replacement_and_errors() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root =
            std::env::temp_dir().join(format!("mpc-settings-{:032x}", u128::from_le_bytes(nonce)));
        fs::create_dir_all(root.join("shots")).unwrap();
        let settings = root.join("prefs/settings.json");
        assert_eq!(load_folder(&settings).unwrap(), None);
        save_folder(&settings, &root).unwrap();
        assert_eq!(load_folder(&settings).unwrap(), Some(root.clone()));
        save_folder(&settings, &root.join("shots")).unwrap();
        assert_eq!(load_folder(&settings).unwrap(), Some(root.join("shots")));
        assert!(save_folder(&settings, &root.join("missing")).is_err());
        assert_eq!(load_folder(&settings).unwrap(), Some(root.join("shots")));
        fs::write(&settings, b"not json").unwrap();
        assert!(load_folder(&settings).is_err());
        assert_eq!(fs::read_dir(settings.parent().unwrap()).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
