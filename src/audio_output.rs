//! Lazy output-only CPAL service. Device enumeration, stream creation/destruction
//! and serialized preference writes stay on one worker, never on GPUI/network/RT.
use crate::{
    audio::Audio,
    preferences::{self, AudioSettings},
};
use cpal::{
    BufferSize, SampleFormat, SampleRate,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub is_default: bool,
    pub rates: Vec<u32>,
    pub buffers: Vec<u32>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub devices: Vec<Device>,
    pub active: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub settings_error: Option<String>,
}
#[derive(Clone)]
struct Request {
    source: Option<Arc<Audio>>,
    settings: AudioSettings,
    scan: u64,
    save: u64,
}
pub struct AudioOutput {
    request: Arc<Mutex<Request>>,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl AudioOutput {
    pub fn new(settings: AudioSettings) -> io::Result<Self> {
        Self::spawn(settings, |settings| {
            preferences::audio_settings_path()
                .and_then(|path| preferences::save_audio(&path, settings))
        })
    }
    fn spawn(
        settings: AudioSettings,
        save: impl Fn(&AudioSettings) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let request = Arc::new(Mutex::new(Request {
            source: None,
            settings,
            scan: 0,
            save: 0,
        }));
        let status = Arc::new(Mutex::new(Status::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (desired, published, cancelled) = (request.clone(), status.clone(), stop.clone());
        let worker = thread::Builder::new()
            .name("device-audio-output".into())
            .spawn(move || {
                run(desired, published, cancelled, save);
            })?;
        Ok(Self {
            request,
            status,
            stop,
            worker: Some(worker),
        })
    }
    pub fn set_source(&self, source: Option<Arc<Audio>>) {
        self.request.lock().unwrap().source = source;
    }
    pub fn set_settings(&self, settings: AudioSettings) {
        let mut request = self.request.lock().unwrap();
        if request.settings != settings {
            request.settings = settings;
            request.save = request.save.wrapping_add(1);
        }
    }
    /// Explicit device rescan and retry, including after a hot-unplug/driver error.
    pub fn refresh(&self) {
        let mut request = self.request.lock().unwrap();
        request.scan = request.scan.wrapping_add(1);
    }
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    /// Drain the latest settings and destroy the device off the UI thread.
    pub fn finish(mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("Audio output worker failed"))?;
        }
        match self.status().settings_error {
            Some(error) => Err(io::Error::other(error)),
            None => Ok(()),
        }
    }
}
impl Drop for AudioOutput {
    fn drop(&mut self) {
        // Do not wait on a potentially slow device driver on the UI thread.
        // This single worker exits and destroys its stream on its next poll.
        self.stop.store(true, Ordering::Release);
    }
}
fn same_source(a: &Option<Arc<Audio>>, b: &Option<Arc<Audio>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
fn same_device(a: &AudioSettings, b: &AudioSettings) -> bool {
    a.device == b.device && a.sample_rate == b.sample_rate && a.buffer_frames == b.buffer_frames
}
fn run(
    request: Arc<Mutex<Request>>,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    save: impl Fn(&AudioSettings) -> io::Result<()>,
) {
    let mut previous = request.lock().unwrap().clone();
    previous.source = None;
    previous.scan = 0;
    previous.save = 0;
    let mut stream = None;
    let failed = Arc::new(AtomicBool::new(false));
    let mut fallback_used = false;
    loop {
        // Read stop before the request: a shutdown must include all preceding changes.
        let stopping = stop.load(Ordering::Acquire);
        let current = request.lock().unwrap().clone();
        let device_changed = !same_device(&previous.settings, &current.settings);
        let source_changed = !same_source(&previous.source, &current.source);
        let rescan = current.scan != previous.scan;
        let device_failed = failed.swap(false, Ordering::AcqRel);
        if let Some(source) = &current.source {
            source.set_volume(current.settings.volume);
            source.set_target_buffer_ms(current.settings.target_buffer_ms);
        }
        if current.save != previous.save {
            let error = save(&current.settings)
                .err()
                .map(|e| format!("Could not save audio settings: {e}"));
            status.lock().unwrap().settings_error = error;
        }
        if stopping {
            break;
        }
        if device_changed || source_changed || rescan {
            fallback_used = false;
        }
        let fallback = device_failed && !fallback_used;
        if device_changed || source_changed || rescan || device_failed {
            drop(stream.take());
            status.lock().unwrap().active = None;
            if current.source.is_none() {
                let mut state = status.lock().unwrap();
                state.error = None;
                state.warning = None;
            }
            if current.source.is_some() || rescan {
                let host = cpal::default_host();
                if rescan || source_changed || device_changed {
                    let devices = devices(&host);
                    status.lock().unwrap().devices = devices;
                }
                if let Some(source) = &current.source {
                    let mut settings = current.settings.clone();
                    if fallback {
                        settings.device = None;
                        settings.sample_rate = None;
                        settings.buffer_frames = None;
                        fallback_used = true;
                    }
                    if !device_failed || fallback {
                        match open(&host, &settings, source.clone(), failed.clone()) {
                            Ok((new_stream, description, warning)) => {
                                stream = Some(new_stream);
                                let mut state = status.lock().unwrap();
                                state.active = Some(description);
                                state.error = None;
                                state.warning = if fallback {
                                    Some("Output device failed; using system default. Rescan to retry saved settings.".into())
                                } else {
                                    warning
                                };
                            }
                            Err(error) => status.lock().unwrap().error = Some(error),
                        }
                    } else {
                        status.lock().unwrap().error = Some(
                            "Audio output device stopped. Check the device and use Rescan / Retry."
                                .into(),
                        );
                    }
                } else {
                    status.lock().unwrap().error = None;
                }
            }
        }
        previous = current;
        thread::sleep(Duration::from_millis(50));
    }
    drop(stream);
}
fn supported(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I16
            | SampleFormat::U16
            | SampleFormat::I32
            | SampleFormat::U32
            | SampleFormat::I8
            | SampleFormat::U8
            | SampleFormat::I64
            | SampleFormat::U64
    )
}
fn devices(host: &cpal::Host) -> Vec<Device> {
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| {
            let name = device.name().ok()?;
            let mut rates = Vec::new();
            let mut buffers = Vec::new();
            for config in device.supported_output_configs().ok()? {
                if !supported(config.sample_format()) || !(1..=8).contains(&config.channels()) {
                    continue;
                }
                rates.extend(
                    [config.min_sample_rate().0, config.max_sample_rate().0]
                        .into_iter()
                        .chain([
                            8000, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 192000, 384000,
                        ])
                        .filter(|r| {
                            (8000..=384000).contains(r)
                                && *r >= config.min_sample_rate().0
                                && *r <= config.max_sample_rate().0
                        }),
                );
                buffers.extend(buffer_choices(config.buffer_size()));
            }
            rates.sort_unstable();
            rates.dedup();
            buffers.sort_unstable();
            buffers.dedup();
            (!rates.is_empty()).then_some(Device {
                is_default: default_name.as_ref() == Some(&name),
                name,
                rates,
                buffers,
            })
        })
        .collect()
}
fn buffer_choices(size: &cpal::SupportedBufferSize) -> Vec<u32> {
    match size {
        cpal::SupportedBufferSize::Range { min, max } => {
            let mut choices: Vec<_> = [*min, *max]
                .into_iter()
                .chain([64, 128, 256, 512, 1024, 2048])
                .filter(|b| (64..=65536).contains(b) && b >= min && b <= max)
                .collect();
            choices.sort_unstable();
            choices.dedup();
            choices
        }
        cpal::SupportedBufferSize::Unknown => Vec::new(),
    }
}
fn open(
    host: &cpal::Host,
    settings: &AudioSettings,
    source: Arc<Audio>,
    failed: Arc<AtomicBool>,
) -> Result<(cpal::Stream, String, Option<String>), String> {
    settings.validate().map_err(|e| e.to_string())?;
    let selected = settings.device.as_ref().and_then(|name| {
        host.output_devices()
            .ok()?
            .find(|d| d.name().ok().as_ref() == Some(name))
    });
    let missing = settings.device.is_some() && selected.is_none();
    let device = with_device_fallback(selected, || host.default_output_device())?;
    let name = device.name().unwrap_or_else(|_| "System output".into());
    let default = device
        .default_output_config()
        .map_err(|e| format!("Audio output configuration: {e}"))?;
    let configs = device
        .supported_output_configs()
        .map_err(|e| format!("Audio output formats: {e}"))?;
    let (stream_config, format, warning) = output_config(configs, &default, settings)?;
    let rate = stream_config.sample_rate.0;
    let warning = if missing {
        Some(format!(
            "Saved output device is unavailable; using system default. {}",
            warning.unwrap_or_default()
        ))
    } else {
        warning
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &stream_config, source, failed),
        SampleFormat::F64 => build::<f64>(&device, &stream_config, source, failed),
        SampleFormat::I16 => build::<i16>(&device, &stream_config, source, failed),
        SampleFormat::U16 => build::<u16>(&device, &stream_config, source, failed),
        SampleFormat::I32 => build::<i32>(&device, &stream_config, source, failed),
        SampleFormat::U32 => build::<u32>(&device, &stream_config, source, failed),
        SampleFormat::I8 => build::<i8>(&device, &stream_config, source, failed),
        SampleFormat::U8 => build::<u8>(&device, &stream_config, source, failed),
        SampleFormat::I64 => build::<i64>(&device, &stream_config, source, failed),
        SampleFormat::U64 => build::<u64>(&device, &stream_config, source, failed),
        _ => return Err("Unsupported audio output sample type".into()),
    }
    .map_err(|e| format!("Could not start audio output: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("Could not play audio: {e}"))?;
    let buffer = match stream_config.buffer_size {
        BufferSize::Default => "default buffer".into(),
        BufferSize::Fixed(n) => format!("{n} frame buffer"),
    };
    Ok((
        stream,
        format!(
            "{name} · {rate} Hz · {} ch · {buffer}",
            stream_config.channels
        ),
        warning,
    ))
}
fn with_device_fallback<T>(
    selected: Option<T>,
    default: impl FnOnce() -> Option<T>,
) -> Result<T, String> {
    selected
        .or_else(default)
        .ok_or_else(|| "No audio output device is available. Screen viewing is unaffected.".into())
}
fn output_config(
    configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    default: &cpal::SupportedStreamConfig,
    settings: &AudioSettings,
) -> Result<(cpal::StreamConfig, SampleFormat, Option<String>), String> {
    let mut configs: Vec<_> = configs
        .filter(|c| supported(c.sample_format()) && (1..=8).contains(&c.channels()))
        .collect();
    // Prefer supported saved settings, then the device's default layout/format.
    configs.sort_by_key(|c| (
        !settings.sample_rate.is_some_and(|r| r >= c.min_sample_rate().0 && r <= c.max_sample_rate().0),
        !settings.buffer_frames.is_some_and(|b| b >= 64 && matches!(c.buffer_size(), cpal::SupportedBufferSize::Range {min,max} if b >= *min && b <= *max)),
        c.channels() != default.channels(),
        c.sample_format() != default.sample_format(), c.channels() != 2,
    ));
    let config = configs.first().ok_or("No supported audio output format")?;
    let requested_rate = settings.sample_rate.unwrap_or(default.sample_rate().0);
    let rate = requested_rate.clamp(config.min_sample_rate().0, config.max_sample_rate().0);
    let mut warning = Vec::new();
    if rate != requested_rate {
        warning.push("Saved sample rate is unavailable; using a supported rate.");
    }
    let mut stream_config = (*config).with_sample_rate(SampleRate(rate)).config();
    if let Some(frames) = settings.buffer_frames {
        if frames >= 64
            && matches!(config.buffer_size(), cpal::SupportedBufferSize::Range {min,max} if frames >= *min && frames <= *max)
        {
            stream_config.buffer_size = BufferSize::Fixed(frames);
        } else {
            warning.push("Saved buffer size is unavailable; using device default.");
        }
    }
    Ok((
        stream_config,
        config.sample_format(),
        (!warning.is_empty()).then(|| warning.join(" ")),
    ))
}
fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    source: Arc<Audio>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = usize::from(config.channels);
    let rate = config.sample_rate.0;
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            // No scratch Vec, locks that wait, strings, files or network work here.
            source.render(data.len() / channels, channels, rate, |i, sample| {
                data[i] = T::from_sample(sample)
            });
        },
        move |_| {
            failed.store(true, Ordering::Release);
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn range(channels: u16, rate: u32, sample: SampleFormat) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            SampleRate(rate),
            SampleRate(rate),
            cpal::SupportedBufferSize::Range { min: 64, max: 2048 },
            sample,
        )
    }
    #[test]
    fn latency_and_volume_changes_do_not_reopen_the_device() {
        let old = AudioSettings::default();
        let mut changed = AudioSettings {
            volume: 0.25,
            target_buffer_ms: 20,
            ..old.clone()
        };
        assert!(same_device(&old, &changed));
        changed.buffer_frames = Some(64);
        assert!(!same_device(&old, &changed));
    }
    #[test]
    fn output_buffer_choices_start_at_64_even_when_device_supports_less() {
        use cpal::SupportedBufferSize::{Range, Unknown};
        assert_eq!(buffer_choices(&Range { min: 8, max: 64 }), [64]);
        assert!(buffer_choices(&Range { min: 16, max: 32 }).is_empty());
        assert_eq!(buffer_choices(&Range { min: 32, max: 64 }), [64]);
        assert_eq!(buffer_choices(&Range { min: 64, max: 128 }), [64, 128]);
        assert!(buffer_choices(&Unknown).is_empty());
        let low = range(2, 48000, SampleFormat::F32);
        let low = cpal::SupportedStreamConfigRange::new(
            low.channels(),
            low.min_sample_rate(),
            low.max_sample_rate(),
            Range { min: 16, max: 2048 },
            low.sample_format(),
        );
        let default = low.with_sample_rate(SampleRate(48000));
        for frames in [16, 32] {
            let settings = AudioSettings {
                buffer_frames: Some(frames),
                ..AudioSettings::default()
            };
            let (config, _, warning) =
                output_config([low].into_iter(), &default, &settings).unwrap();
            assert_eq!(config.buffer_size, BufferSize::Default);
            assert!(warning.unwrap().contains("buffer size is unavailable"));
        }
        let default = range(2, 48000, SampleFormat::F32).with_sample_rate(SampleRate(48000));
        let settings = AudioSettings {
            buffer_frames: Some(64),
            ..AudioSettings::default()
        };
        let (config, _, warning) = output_config(
            [range(2, 48000, SampleFormat::F32)].into_iter(),
            &default,
            &settings,
        )
        .unwrap();
        assert_eq!(config.buffer_size, BufferSize::Fixed(64));
        assert!(warning.is_none());
    }
    #[test]
    fn immediate_shutdown_persists_latest_settings_without_opening_hardware() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory =
            std::env::temp_dir().join(format!("mpc-audio-quit-{:016x}", u64::from_le_bytes(nonce)));
        let path = directory.join("audio.json");
        let destination = path.clone();
        let output = AudioOutput::spawn(AudioSettings::default(), move |settings| {
            preferences::save_audio(&destination, settings)
        })
        .unwrap();
        let mut settings = AudioSettings::default();
        for i in 0..20 {
            settings.volume = i as f32 / 20.0;
            output.set_settings(settings.clone());
        }
        output.finish().unwrap();
        assert_eq!(preferences::load_audio(&path).unwrap(), settings);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn shutdown_drains_a_change_submitted_during_a_save_and_reports_errors() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let saved = writes.clone();
        let (entered, started) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let output = AudioOutput::spawn(AudioSettings::default(), move |settings| {
            if saved.lock().unwrap().is_empty() {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            saved.lock().unwrap().push(settings.clone());
            Ok(())
        })
        .unwrap();
        let mut settings = AudioSettings {
            volume: 0.25,
            ..AudioSettings::default()
        };
        output.set_settings(settings.clone());
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        settings.volume = 0.75;
        output.set_settings(settings.clone());
        let finish = thread::spawn(move || output.finish());
        release.send(()).unwrap();
        finish.join().unwrap().unwrap();
        assert_eq!(writes.lock().unwrap().last(), Some(&settings));
        let output = AudioOutput::spawn(AudioSettings::default(), |_| {
            Err(io::Error::other("disk full"))
        })
        .unwrap();
        output.set_settings(settings);
        assert!(
            output
                .finish()
                .unwrap_err()
                .to_string()
                .contains("disk full")
        );
    }

    #[test]
    fn missing_device_falls_back_or_reports_without_a_driver_call() {
        assert_eq!(
            with_device_fallback(Some("saved"), || Some("default")).unwrap(),
            "saved"
        );
        assert_eq!(
            with_device_fallback(None, || Some("default")).unwrap(),
            "default"
        );
        assert!(
            with_device_fallback::<()>(None, || None)
                .unwrap_err()
                .contains("No audio output device")
        );
    }
    #[test]
    fn settings_select_supported_formats_and_fallback_for_missing_rates_buffers() {
        let configs = [
            range(2, 48000, SampleFormat::F32),
            range(1, 44100, SampleFormat::I16),
        ];
        let default = configs[0].with_sample_rate(SampleRate(48000));
        let settings = AudioSettings {
            sample_rate: Some(44100),
            buffer_frames: Some(256),
            ..AudioSettings::default()
        };
        let (config, sample, warning) =
            output_config(configs.into_iter(), &default, &settings).unwrap();
        assert_eq!(config.channels, 1);
        assert_eq!(config.sample_rate.0, 44100);
        assert_eq!(sample, SampleFormat::I16);
        assert_eq!(config.buffer_size, BufferSize::Fixed(256));
        assert!(warning.is_none());
        let settings = AudioSettings {
            sample_rate: Some(96000),
            buffer_frames: Some(4096),
            ..AudioSettings::default()
        };
        let (config, _, warning) = output_config(configs.into_iter(), &default, &settings).unwrap();
        assert_eq!(config.channels, 2);
        assert_eq!(config.sample_rate.0, 48000);
        assert_eq!(config.buffer_size, BufferSize::Default);
        let warning = warning.unwrap();
        assert!(warning.contains("sample rate"));
        assert!(warning.contains("buffer size"));
        assert!(output_config(std::iter::empty(), &default, &settings).is_err());
        assert!(
            output_config(
                [range(0, 48000, SampleFormat::F32)].into_iter(),
                &default,
                &settings
            )
            .is_err()
        );
    }
}
