//! Versioned viewer preferences and bounded recent-device history.
use super::{save_json, screenshot_settings_path};
use crate::protocol::{DEFAULT_PORT, DeviceInfo};
use std::{
    fs::File,
    io::{self, Read},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

pub const MAX_RECENT_DEVICES: usize = 10;
const MAX_TEXT_BYTES: usize = 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentDevice {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub screen_name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub firmware: String,
}
impl RecentDevice {
    pub fn label(&self) -> String {
        let address = if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        };
        let mut parts = vec![address];
        if !self.model.is_empty() {
            parts.push(self.model.clone());
        } else if !self.screen_name.is_empty() {
            parts.push(self.screen_name.clone());
        }
        if !self.serial.is_empty() {
            parts.push(format!("S/N {}", self.serial));
        }
        if !self.firmware.is_empty() {
            parts.push(format!("FW {}", self.firmware));
        }
        parts.join(" · ")
    }
    fn validate(&self) -> io::Result<()> {
        validate_host(&self.host, false)?;
        if self.port == 0
            || [&self.screen_name, &self.model, &self.serial, &self.firmware]
                .iter()
                .any(|s| s.len() > MAX_TEXT_BYTES || s.chars().any(char::is_control))
        {
            return Err(invalid_settings());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualizerMode {
    #[default]
    Spectrum,
    Spectrogram,
    Waterfall,
    Waveform,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewerSettings {
    pub version: u8,
    pub host: String,
    pub port: u16,
    pub actual_size: bool,
    pub show_stats: bool,
    pub show_port: bool,
    pub control_on: bool,
    pub analyzer_open: bool,
    pub analyzer_mode: VisualizerMode,
    pub analyzer_height: f32,
    pub show_vectorscope: bool,
    /// Saved frame-rate limit; zero requests the server default.
    pub fps_limit: u8,
    pub recent_devices: Vec<RecentDevice>,
}
impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            version: VERSION,
            host: String::new(),
            port: DEFAULT_PORT,
            actual_size: false,
            show_stats: true,
            show_port: false,
            control_on: false,
            analyzer_open: false,
            analyzer_mode: VisualizerMode::Spectrum,
            analyzer_height: 220.0,
            show_vectorscope: false,
            fps_limit: 0,
            recent_devices: Vec::new(),
        }
    }
}
impl ViewerSettings {
    pub fn validate(&self) -> io::Result<()> {
        validate_host(&self.host, true)?;
        if self.version != VERSION
            || self.port == 0
            || self.fps_limit > 120
            || !self.analyzer_height.is_finite()
            || self.analyzer_height < 120.0
            || self.recent_devices.len() > MAX_RECENT_DEVICES
        {
            return Err(invalid_settings());
        }
        for (i, device) in self.recent_devices.iter().enumerate() {
            device.validate()?;
            if self.recent_devices[..i]
                .iter()
                .any(|other| same_address(other, &device.host, device.port))
            {
                return Err(invalid_settings());
            }
        }
        Ok(())
    }
    /// Invalid editor contents must not replace the last valid address.
    pub fn remember_address(&mut self, host: &str, port: u16) -> io::Result<()> {
        let host = host.trim();
        validate_host(host, true)?;
        if port == 0 {
            return Err(invalid_settings());
        }
        self.host = host.to_owned();
        self.port = port;
        Ok(())
    }
    /// Call only after a successful handshake. Late optional metadata enriches the
    /// same row; absent/empty fields do not erase labels from an earlier connection.
    pub fn remember_device(
        &mut self,
        host: &str,
        port: u16,
        screen_name: &str,
        info: Option<&DeviceInfo>,
    ) -> io::Result<()> {
        let host = host.trim();
        validate_host(host, false)?;
        if port == 0 {
            return Err(invalid_settings());
        }
        let mut row = self
            .recent_devices
            .iter()
            .find(|row| same_address(row, host, port))
            .cloned()
            .unwrap_or_else(|| RecentDevice {
                host: host.into(),
                port,
                screen_name: String::new(),
                model: String::new(),
                serial: String::new(),
                firmware: String::new(),
            });
        row.host = host.into();
        update_label(&mut row.screen_name, screen_name);
        if let Some(info) = info {
            update_label(&mut row.model, &info.model);
            update_label(&mut row.serial, &info.serial);
            update_label(&mut row.firmware, &info.firmware);
        }
        self.recent_devices
            .retain(|row| !same_address(row, host, port));
        self.recent_devices.insert(0, row);
        self.recent_devices.truncate(MAX_RECENT_DEVICES);
        Ok(())
    }
}
fn invalid_settings() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid viewer settings")
}
fn validate_host(host: &str, allow_empty: bool) -> io::Result<()> {
    if (!allow_empty && host.is_empty())
        || host.len() > MAX_TEXT_BYTES
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(invalid_settings());
    }
    Ok(())
}
fn same_address(row: &RecentDevice, host: &str, port: u16) -> bool {
    row.port == port
        && (row.host.eq_ignore_ascii_case(host)
            || matches!((row.host.parse::<IpAddr>(), host.parse::<IpAddr>()),
                (Ok(a), Ok(b)) if a == b))
}
fn update_label(target: &mut String, value: &str) {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut value = cleaned.trim();
    if value.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value = &value[..end];
    }
    if !value.is_empty() {
        *target = value.into();
    }
}

pub fn viewer_settings_path() -> io::Result<PathBuf> {
    Ok(screenshot_settings_path()?.with_file_name("viewer-settings.json"))
}
pub fn load_viewer(path: &Path) -> io::Result<ViewerSettings> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ViewerSettings::default()),
        Err(e) => return Err(e),
    };
    // Bound actual reads as well as parsing, including a concurrently growing file.
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid_settings());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid_settings())?;
    if !value.is_object()
        || value
            .get("recent_devices")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| !row.is_object()))
    {
        return Err(invalid_settings());
    }
    let mut settings: ViewerSettings =
        serde_json::from_value(value).map_err(|_| invalid_settings())?;
    settings.host = settings.host.trim().into();
    // Most-recent-first order is retained; keep the first copy of each address.
    let rows = std::mem::take(&mut settings.recent_devices);
    for mut row in rows {
        row.host = row.host.trim().into();
        row.validate()?;
        if settings.recent_devices.len() < MAX_RECENT_DEVICES
            && !settings
                .recent_devices
                .iter()
                .any(|other| same_address(other, &row.host, row.port))
        {
            settings.recent_devices.push(row);
        }
    }
    settings.validate()?;
    Ok(settings)
}
pub fn save_viewer(path: &Path, settings: &ViewerSettings) -> io::Result<()> {
    settings.validate()?;
    save_json(path, &serde_json::to_value(settings)?)
}

#[derive(Default)]
struct SaveState {
    latest: Option<ViewerSettings>,
    dirty: bool,
    stopping: bool,
    result: Option<Result<(), String>>,
}
/// One serialized writer, with only the newest pending value. No filesystem work
/// on the UI thread and no task/queue per keystroke. `finish` drains on normal quit.
pub struct ViewerSettingsWriter {
    shared: Arc<(Mutex<SaveState>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl ViewerSettingsWriter {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        Self::spawn(move |settings| save_viewer(&path, settings))
    }
    fn spawn(
        save: impl Fn(&ViewerSettings) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new((Mutex::new(SaveState::default()), Condvar::new()));
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("viewer-settings".into())
            .spawn(move || {
                let (lock, wake) = &*state;
                loop {
                    let mut state = lock.lock().unwrap();
                    while !state.dirty && !state.stopping {
                        state = wake.wait(state).unwrap();
                    }
                    if !state.dirty {
                        break;
                    }
                    if !state.stopping {
                        // Coalesce requests in a fixed window. Quit interrupts it.
                        state = wake
                            .wait_timeout_while(state, Duration::from_millis(150), |s| !s.stopping)
                            .unwrap()
                            .0;
                    }
                    let settings = state.latest.clone().unwrap();
                    state.dirty = false;
                    drop(state);
                    let result =
                        save(&settings).map_err(|e| format!("Could not save viewer settings: {e}"));
                    lock.lock().unwrap().result = Some(result);
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub fn request(&self, settings: ViewerSettings) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        if !state.stopping
            && (state.latest.as_ref() != Some(&settings)
                || state.result.as_ref().is_some_and(Result::is_err))
        {
            state.latest = Some(settings);
            state.dirty = true;
            wake.notify_one();
        }
    }
    pub fn result(&self) -> Option<Result<(), String>> {
        self.shared.0.lock().unwrap().result.clone()
    }
    fn stop(&self) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap().stopping = true;
        wake.notify_one();
    }
    pub fn finish(mut self) -> io::Result<()> {
        self.stop();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("Viewer settings worker failed"))?;
        }
        match self.result() {
            Some(Err(error)) => Err(io::Error::other(error)),
            _ => Ok(()),
        }
    }
}
impl Drop for ViewerSettingsWriter {
    fn drop(&mut self) {
        // A view disappearing must not block GPUI. Normal app quit calls finish
        // off-thread; other drops still signal the worker to drain and exit.
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn writer_serializes_coalesces_and_drains_the_latest_request() {
        let (entered, started) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let writes = saved.clone();
        let writer = ViewerSettingsWriter::spawn(move |settings| {
            let first = writes.lock().unwrap().is_empty();
            if first {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            writes.lock().unwrap().push(settings.clone());
            Ok(())
        })
        .unwrap();
        writer.request(ViewerSettings::default());
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut newest = ViewerSettings::default();
        for i in 0..100 {
            newest.host = format!("device-{i}");
            writer.request(newest.clone());
        }
        release.send(()).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            *saved.lock().unwrap(),
            vec![ViewerSettings::default(), newest]
        );
    }

    #[test]
    fn writer_surfaces_errors_and_retries_identical_settings() {
        let attempts = Arc::new(Mutex::new(0));
        let count = attempts.clone();
        let writer = ViewerSettingsWriter::spawn(move |_| {
            let mut count = count.lock().unwrap();
            *count += 1;
            if *count == 1 {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test denial",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap();
        writer.request(ViewerSettings::default());
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while writer.result().is_none() {
            assert!(std::time::Instant::now() < until);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            writer
                .result()
                .unwrap()
                .unwrap_err()
                .contains("test denial")
        );
        writer.request(ViewerSettings::default());
        writer.finish().unwrap();
        assert_eq!(*attempts.lock().unwrap(), 2);
        let writer = ViewerSettingsWriter::spawn(|_| Err(io::Error::other("disk full"))).unwrap();
        writer.request(ViewerSettings::default());
        assert!(
            writer
                .finish()
                .unwrap_err()
                .to_string()
                .contains("disk full")
        );
    }
}
