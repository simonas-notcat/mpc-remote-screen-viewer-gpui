use super::*;
use crate::{
    audio::Audio,
    framebuffer::Framebuffer,
    protocol::{PixelFormat, ServerInit},
};
use std::io::Read;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let mut n = [0; 8];
        getrandom::fill(&mut n).unwrap();
        let p = std::env::temp_dir().join(format!("mpc-movie-{:x}", u64::from_le_bytes(n)));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> PathBuf {
        self.0.join("test.mov")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn be(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
fn find<'a>(b: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    let p = b.windows(4).position(|w| w == kind).unwrap();
    let size = be(&b[p - 4..]) as usize;
    &b[p + 4..p - 4 + size]
}
fn format(rate: u32, channels: u8) -> AudioFormat {
    AudioFormat {
        sample_rate: rate,
        channels,
        codec: 0,
    }
}
fn picture() -> RgbaImage {
    RgbaImage::from_pixel(64, 32, image::Rgba([240, 10, 20, 255]))
}
#[test]
fn mov_tables_jpeg_pcm_and_variable_durations() {
    let temp = Temp::new();
    let path = temp.path();
    let mut movie = Movie::create(&path, 64, 32).unwrap();
    movie.video(&picture(), 10).unwrap();
    movie.video(&picture(), 80).unwrap();
    movie.video(&picture(), 80).unwrap();
    movie.audio_silence(format(48000, 2), 48).unwrap();
    movie
        .audio(format(48000, 2), &[-1.0, 1.0, 0.5, -0.5])
        .unwrap();
    movie.finish(500).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[4..8], b"ftyp");
    assert_eq!(&bytes[8..12], b"qt  ");
    let mdat = u64::from_be_bytes(bytes[28..36].try_into().unwrap()) as usize;
    assert_eq!(&bytes[20 + mdat + 4..20 + mdat + 8], b"moov");
    let stts = find(&bytes, b"stts");
    assert_eq!(be(&stts[4..]), 3);
    assert_eq!(&stts[8..], nums(&[1, 80, 1, 1, 1, 419]));
    let co64 = find(&bytes, b"co64");
    assert_eq!(be(&co64[4..]), 3);
    let stsz = find(&bytes, b"stsz");
    assert_eq!(be(&stsz[8..]), 3);
    for i in 0..3 {
        let offset = u64::from_be_bytes(co64[8 + i * 8..16 + i * 8].try_into().unwrap()) as usize;
        let size = be(&stsz[12 + i * 4..]) as usize;
        let jpeg = image::load_from_memory(&bytes[offset..offset + size]).unwrap();
        assert_eq!((jpeg.width(), jpeg.height()), (64, 32));
    }
    assert!(bytes.windows(4).any(|w| w == b"sowt"));
    assert!(Movie::create(&path, 64, 32).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}
#[test]
fn resized_sources_are_letterboxed_and_metadata_is_fixed() {
    let temp = Temp::new();
    let mut movie = Movie::create(&temp.path(), 64, 32).unwrap();
    movie
        .video(
            &RgbaImage::from_pixel(10, 40, image::Rgba([255, 255, 255, 255])),
            0,
        )
        .unwrap();
    movie.finish(100).unwrap();
    let bytes = std::fs::read(temp.path()).unwrap();
    let offset = u64::from_be_bytes(find(&bytes, b"co64")[8..16].try_into().unwrap()) as usize;
    let size = be(&find(&bytes, b"stsz")[12..]) as usize;
    let jpeg = image::load_from_memory(&bytes[offset..offset + size])
        .unwrap()
        .to_rgb8();
    assert_eq!(jpeg.dimensions(), (64, 32));
    assert!(jpeg.get_pixel(0, 16)[0] < 10);
    assert!(jpeg.get_pixel(32, 16)[0] > 245);
}
#[test]
fn offsets_beyond_four_gib_are_co64() {
    let temp = Temp::new();
    let mut movie = Movie::create(&temp.path(), 64, 32).unwrap();
    movie
        .file
        .seek(SeekFrom::Start((1u64 << 32) + 128))
        .unwrap();
    movie.video(&picture(), 0).unwrap();
    let end = movie.file.stream_position().unwrap();
    movie.finish(100).unwrap();
    let mut file = File::open(temp.path()).unwrap();
    file.seek(SeekFrom::Start(end)).unwrap();
    let mut metadata = Vec::new();
    file.read_to_end(&mut metadata).unwrap();
    assert_eq!(
        u64::from_be_bytes(find(&metadata, b"co64")[8..16].try_into().unwrap()),
        (1u64 << 32) + 128
    );
}
#[test]
fn audio_format_and_gap_limits_are_explicit() {
    let temp = Temp::new();
    let mut movie = Movie::create(&temp.path(), 64, 32).unwrap();
    movie.video(&picture(), 0).unwrap();
    movie.audio(format(48000, 2), &[0.0; 20]).unwrap();
    assert!(movie.audio(format(44100, 2), &[0.0; 20]).is_err());
    assert!(movie.audio_silence(format(48000, 2), 48000 * 61).is_err());
    movie.finish(100).unwrap();
}
fn source() -> Arc<Mutex<Snapshot>> {
    let init = ServerInit {
        width: 64,
        height: 32,
        pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
        name: "test".into(),
        rotation: 0,
        input_capabilities: 0,
        audio_available: true,
    };
    Arc::new(Mutex::new(Snapshot {
        state: State::Streaming,
        screen: Some(init.clone()),
        framebuffer: Some(Arc::new(std::sync::RwLock::new(
            Framebuffer::new(init).unwrap(),
        ))),
        ..Snapshot::default()
    }))
}
#[test]
fn worker_drains_audio_trims_overlap_fills_gaps_and_records_static_tail() {
    let temp = Temp::new();
    let state = source();
    let recorder = Recorder::start(temp.path(), state.clone()).unwrap();
    // Anchor at zero: packets end beyond elapsed source time.
    let f = format(8000, 1);
    recorder.audio.push(f, 0, 100, &[0.25; 800]);
    recorder.audio.push(f, 0, 100, &[0.25; 800]); // duplicate
    recorder.audio.push(f, 0, 800, &[0.5; 800]); // 100 overlap
    recorder.audio.push(f, 0, 1700, &[-0.5; 800]); // 100 silence
    recorder.audio.push(f, 1, 0, &[0.5; 800]); // reset/re-enable: new source clock
    wait_for_video(&temp.path());
    state.lock().unwrap().revision += 1; // empty/skipped network frame, not image dirtiness
    thread::sleep(Duration::from_millis(85));
    recorder.finish().unwrap();
    let bytes = std::fs::read(temp.path()).unwrap();
    let moov = find(&bytes, b"moov");
    let video = find(moov, b"trak");
    let rest = &moov[video.len() + 8 + find(moov, b"mvhd").len() + 8..];
    let audio = find(rest, b"trak");
    let sizes = find(audio, b"stsz");
    assert_eq!(be(&sizes[4..]), 2);
    assert_eq!(be(&sizes[8..]), 3200);
    let chunks = find(audio, b"co64");
    assert_eq!(be(&chunks[4..]), 5); // first, overlap suffix, gap silence, last, reset
    let video_sizes = find(video, b"stsz");
    assert_eq!(be(&video_sizes[8..]), 1); // no repeated static frames
    assert!(be(&find(video, b"mdhd")[16..]) >= 80);
}
#[test]
fn source_audio_tap_is_independent_of_volume() {
    let temp = Temp::new();
    let recorder = Recorder::start(temp.path(), source()).unwrap();
    let audio = Audio::default();
    audio.set_enabled(true);
    audio.set_volume(0.0);
    audio.configure(format(8000, 1)).unwrap();
    audio.set_recording(Some(recorder.audio.clone()));
    let mut packet = 0u64.to_le_bytes().to_vec();
    packet.extend(800u16.to_le_bytes());
    for _ in 0..800 {
        packet.extend(16384i16.to_le_bytes());
    }
    audio.packet(&packet).unwrap();
    audio.set_recording(None);
    recorder.finish().unwrap();
    let bytes = std::fs::read(temp.path()).unwrap();
    assert!(bytes.windows(4).any(|w| w == [0, 64, 0, 64])); // raw half-level source, despite mute
}
fn wait_for_video(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::fs::metadata(path).map_or(true, |m| m.len() <= 36) {
        assert!(
            Instant::now() < deadline,
            "recorder did not capture initial image"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn disconnect_finalizes_video_only_recording() {
    let temp = Temp::new();
    let state = source();
    let recorder = Recorder::start(temp.path(), state.clone()).unwrap();
    wait_for_video(&temp.path());
    state.lock().unwrap().state = State::RetryWait;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !recorder.completed() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    recorder.finish().unwrap();
    let bytes = std::fs::read(temp.path()).unwrap();
    assert!(bytes.windows(4).any(|w| w == b"moov"));
    assert!(!bytes.windows(4).any(|w| w == b"sowt"));
}
#[test]
fn format_change_surfaces_error_but_finalizes_existing_samples() {
    let temp = Temp::new();
    let recorder = Recorder::start(temp.path(), source()).unwrap();
    wait_for_video(&temp.path());
    recorder.audio.push(format(8000, 1), 0, 0, &[0.5; 800]);
    recorder.audio.push(format(16000, 1), 1, 0, &[0.5; 800]);
    let error = recorder.finish().unwrap_err();
    assert!(error.contains("rate/channels changed"), "{error}");
    let bytes = std::fs::read(temp.path()).unwrap();
    assert!(bytes.windows(4).any(|w| w == b"moov"));
}
#[test]
fn audio_queue_is_bounded_and_overload_is_reported_without_waiting() {
    let (tx, _rx) = mpsc::sync_channel(128);
    let input = AudioInput {
        tx,
        overflow: Arc::new(AtomicBool::new(false)),
        start: Instant::now(),
    };
    for first in 0..128 {
        input.push(format(8000, 1), 0, first, &[0.0]);
    }
    assert!(!input.overflow.load(Ordering::Acquire));
    input.push(format(8000, 1), 0, 128, &[0.0]);
    assert!(input.overflow.load(Ordering::Acquire));
}
#[test]
fn ffprobe_and_ffmpeg_accept_video_pcm_and_high_sample_rates_when_available() {
    if std::process::Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_err()
    {
        return;
    }
    for rate in [48000, 96000, 384000] {
        let temp = Temp::new();
        let path = temp.path();
        let mut movie = Movie::create(&path, 64, 32).unwrap();
        movie.video(&picture(), 0).unwrap();
        movie
            .audio(format(rate, 2), &vec![0.25; rate as usize / 5])
            .unwrap();
        movie.finish(100).unwrap();
        let output = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let streams = json["streams"].as_array().unwrap();
        assert_eq!(streams[0]["codec_name"], "mjpeg");
        assert_eq!(streams[1]["codec_name"], "pcm_s16le");
        assert_eq!(streams[1]["sample_rate"], rate.to_string());
        assert_eq!(streams[1]["channels"], 2);
        if let Ok(output) = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
        {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
