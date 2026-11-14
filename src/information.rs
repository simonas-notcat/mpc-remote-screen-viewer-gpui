//! Read-only Device Info presentation model without credentials.
//! Optional/late metadata never prevents showing useful details.
use crate::{
    audio,
    playout::peak_db,
    session::{Snapshot, State},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<(&'static str, String)>,
}

pub fn state_label(state: State) -> &'static str {
    match state {
        State::Idle => "Disconnected",
        State::Connecting => "Connecting…",
        State::WaitingForHello => "Waiting for Hello…",
        State::Authenticating => "Authenticating…",
        State::WaitingForScreen => "Waiting for screen…",
        State::Streaming => "Connected",
        State::RetryWait => "Connection lost · retrying…",
        State::Failed => "Connection failed · reconnect to retry",
    }
}
pub fn quality_text(snapshot: &Snapshot) -> String {
    if snapshot.statistics.frames > 0 {
        format!("{} (0 is best)", snapshot.statistics.quality)
    } else {
        "—".into()
    }
}
pub fn rtt_text(snapshot: &Snapshot) -> String {
    snapshot
        .statistics
        .rtt
        .map(|r| format!("{:.1} ms", r.as_secs_f64() * 1000.0))
        .unwrap_or_else(|| "—".into())
}
fn metadata(value: Option<&str>) -> String {
    match value.filter(|v| !v.trim().is_empty()) {
        Some(value) => value
            .chars()
            .map(|c| if c.is_control() { '�' } else { c })
            .collect(),
        None => "Not supplied".into(),
    }
}

pub fn device_information(
    snapshot: &Snapshot,
    endpoint: Option<(&str, u16)>,
    audio: &audio::Status,
    output: Option<&str>,
) -> Vec<Section> {
    let endpoint = endpoint
        .map(|(host, port)| match host.parse::<std::net::IpAddr>() {
            Ok(ip) => std::net::SocketAddr::new(ip, port).to_string(),
            Err(_) => format!("{host}:{port}"),
        })
        .unwrap_or_else(|| "—".into());
    let mut sections = vec![Section {
        title: "Connection",
        rows: vec![
            ("State", state_label(snapshot.state).into()),
            ("Address", endpoint),
            (
                "Protocol",
                snapshot
                    .protocol_version
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "Not negotiated".into()),
            ),
            (
                "Transport",
                "TCP · unencrypted; use a trusted LAN / VPN".into(),
            ),
        ],
    }];
    let device = snapshot.device.as_ref();
    sections.push(Section {
        title: "Device",
        rows: vec![
            ("Model", metadata(device.map(|d| d.model.as_str()))),
            ("Serial number", metadata(device.map(|d| d.serial.as_str()))),
            ("Firmware", metadata(device.map(|d| d.firmware.as_str()))),
        ],
    });
    let mut rows = vec![(
        "Screen name",
        metadata(snapshot.screen.as_ref().map(|s| s.name.as_str())),
    )];
    if let Some(screen) = &snapshot.screen {
        let (w, h) = screen.upright_size();
        let [bpp, rs, rb, gs, gb, bs, bb] = screen.pixel_format.0;
        let caps = screen.input_capabilities;
        let mut inputs = Vec::new();
        if caps & 1 != 0 {
            inputs.push("Touch");
        }
        if caps & 2 != 0 {
            inputs.push("Mouse / wheel");
        }
        if caps & 4 != 0 {
            inputs.push("Keyboard");
        }
        rows.extend([
            (
                "Native pixels",
                format!("{} × {}", screen.width, screen.height),
            ),
            ("Upright pixels", format!("{w} × {h}")),
            (
                "Rotation",
                format!("{}° clockwise", u16::from(screen.rotation) * 90),
            ),
            (
                "Pixel format",
                format!("{bpp} bpp · R {rb}@{rs} · G {gb}@{gs} · B {bb}@{bs}"),
            ),
            (
                "Input capabilities",
                format!(
                    "{} (0x{caps:02x})",
                    if inputs.is_empty() {
                        "View only / no recognized input".into()
                    } else {
                        inputs.join(", ")
                    }
                ),
            ),
            (
                "Device audio",
                if screen.audio_available {
                    "Available"
                } else {
                    "Unavailable"
                }
                .into(),
            ),
        ]);
    } else {
        rows.push(("Screen description", "Not available".into()));
    }
    sections.push(Section {
        title: "Screen",
        rows,
    });
    let stats = &snapshot.statistics;
    let (fps, kbps) = stats
        .rates
        .map(|r| {
            (
                format!("{:.1} fps", r.frames_per_second),
                format!("{:.1} kbit/s", r.kilobits_per_second),
            )
        })
        .unwrap_or_else(|| ("—".into(), "—".into()));
    sections.push(Section {
        title: "Network / screen updates",
        rows: vec![
            ("Frame rate", fps),
            ("Receive rate", kbps),
            ("Processed FrameUpdates", stats.frames.to_string()),
            (
                "Received bytes (with envelopes)",
                stats.received_bytes.to_string(),
            ),
            ("Skipped rectangles", stats.skipped_rectangles.to_string()),
            ("Quality level", quality_text(snapshot)),
            ("Ping round-trip", rtt_text(snapshot)),
        ],
    });
    let mut rows = vec![("Local output", output.unwrap_or("Not active").into())];
    if let Some(format) = audio.format {
        let codec = match format.codec {
            0 => "PCM16",
            1 => "IMA ADPCM",
            2 => "Predictive / Rice",
            _ => "Unknown",
        };
        rows.extend([
            (
                "Source format",
                format!(
                    "{} Hz · {} ch · {codec}",
                    format.sample_rate, format.channels
                ),
            ),
            (
                "Playout",
                if audio.stats.playing {
                    "Playing"
                } else {
                    "Prefilling / stopped"
                }
                .into(),
            ),
            (
                "Buffered / target",
                format!(
                    "{:.1} / {} ms",
                    audio.stats.buffered_ms, audio.stats.target_buffer_ms
                ),
            ),
            (
                "Input / output peak",
                format!(
                    "{:.1} / {:.1} dBFS",
                    peak_db(audio.input_peak),
                    peak_db(audio.output_peak)
                ),
            ),
            ("Underflows", audio.stats.underflows.to_string()),
            (
                "Playback lock contention",
                audio.contention_callbacks.to_string(),
            ),
            ("Discontinuities", audio.stats.discontinuities.to_string()),
            ("Latency trims", audio.stats.latency_trims.to_string()),
            (
                "Dropped audio packets",
                audio.stats.dropped_packets.to_string(),
            ),
        ]);
    } else {
        rows.push(("Source format", "Not negotiated / audio off".into()));
    }
    sections.push(Section {
        title: "Audio",
        rows,
    });
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        playout::Stats,
        protocol::{AudioFormat, DeviceInfo, PixelFormat, ServerInit},
    };
    use std::time::Duration;

    fn value<'a>(sections: &'a [Section], label: &str) -> &'a str {
        sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|(key, _)| *key == label)
            .unwrap()
            .1
            .as_str()
    }
    #[test]
    fn disconnected_and_missing_metadata_are_explicit_not_fabricated() {
        let sections =
            device_information(&Snapshot::default(), None, &audio::Status::default(), None);
        for (label, expected) in [
            ("State", "Disconnected"),
            ("Protocol", "Not negotiated"),
            ("Address", "—"),
            ("Model", "Not supplied"),
            ("Screen description", "Not available"),
            ("Frame rate", "—"),
            ("Quality level", "—"),
            ("Ping round-trip", "—"),
            ("Local output", "Not active"),
            ("Source format", "Not negotiated / audio off"),
        ] {
            assert_eq!(value(&sections, label), expected);
        }
        let snapshot = Snapshot {
            state: State::WaitingForHello,
            ..Snapshot::default()
        };
        let sections = device_information(
            &snapshot,
            Some(("::1", 7447)),
            &audio::Status::default(),
            None,
        );
        assert_eq!(value(&sections, "Address"), "[::1]:7447");
        assert_eq!(value(&sections, "Protocol"), "Not negotiated");
    }
    #[test]
    fn information_tracks_metadata_rotation_capabilities_and_audio_statistics() {
        let mut snapshot = Snapshot {
            state: State::Streaming,
            protocol_version: Some(2),
            screen: Some(ServerInit {
                width: 800,
                height: 480,
                pixel_format: PixelFormat([16, 11, 5, 5, 6, 0, 5]),
                name: "Screen\nname".into(),
                rotation: 1,
                input_capabilities: 0x87,
                audio_available: true,
            }),
            device: Some(DeviceInfo {
                model: "Test MPC".into(),
                serial: "".into(),
                firmware: "1.0".into(),
            }),
            ..Snapshot::default()
        };
        snapshot.statistics.processed_frame(0, 2);
        snapshot.statistics.received_message(995);
        snapshot.statistics.rtt = Some(Duration::from_millis(12));
        snapshot.statistics.sample(Duration::from_secs(1));
        let audio = audio::Status {
            format: Some(AudioFormat {
                sample_rate: 48000,
                channels: 2,
                codec: 2,
            }),
            stats: Stats {
                buffered_ms: 79.5,
                target_buffer_ms: 80,
                underflows: 2,
                discontinuities: 3,
                latency_trims: 4,
                dropped_packets: 5,
                playing: true,
                ..Stats::default()
            },
            ..audio::Status::default()
        };
        let sections = device_information(
            &snapshot,
            Some(("mpc.local", 7447)),
            &audio,
            Some("Test output · 48000 Hz"),
        );
        for (label, expected) in [
            ("Protocol", "2"),
            ("Address", "mpc.local:7447"),
            ("Model", "Test MPC"),
            ("Serial number", "Not supplied"),
            ("Screen name", "Screen�name"),
            ("Native pixels", "800 × 480"),
            ("Upright pixels", "480 × 800"),
            ("Rotation", "90° clockwise"),
            ("Pixel format", "16 bpp · R 5@11 · G 6@5 · B 5@0"),
            (
                "Input capabilities",
                "Touch, Mouse / wheel, Keyboard (0x87)",
            ),
            ("Frame rate", "1.0 fps"),
            ("Receive rate", "8.0 kbit/s"),
            ("Skipped rectangles", "2"),
            ("Quality level", "0 (0 is best)"),
            ("Ping round-trip", "12.0 ms"),
            ("Buffered / target", "79.5 / 80 ms"),
            ("Source format", "48000 Hz · 2 ch · Predictive / Rice"),
            ("Input / output peak", "-120.0 / -120.0 dBFS"),
            ("Underflows", "2"),
            ("Discontinuities", "3"),
            ("Latency trims", "4"),
            ("Dropped audio packets", "5"),
        ] {
            assert_eq!(value(&sections, label), expected);
        }
        snapshot.screen.as_mut().unwrap().rotation = 2;
        snapshot.screen.as_mut().unwrap().input_capabilities = 0;
        snapshot.device = Some(DeviceInfo {
            model: "Updated".into(),
            serial: "serial".into(),
            firmware: "2.0".into(),
        });
        let sections = device_information(&snapshot, None, &audio, None);
        assert_eq!(value(&sections, "Upright pixels"), "800 × 480");
        assert_eq!(
            value(&sections, "Input capabilities"),
            "View only / no recognized input (0x00)"
        );
        assert_eq!(value(&sections, "Model"), "Updated");
    }
}
