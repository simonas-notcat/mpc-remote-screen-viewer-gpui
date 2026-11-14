//! Background Photo-JPEG / PCM16 QuickTime recording. R1 remains a comparison gate.
use crate::{
    protocol::AudioFormat,
    session::{Snapshot, State},
};
use image::{RgbaImage, codecs::jpeg::JpegEncoder, imageops};
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 1_000_000;
fn invalid(s: &str) -> io::Error {
    io::Error::other(s)
}
#[derive(Clone)]
pub struct AudioInput {
    tx: SyncSender<AudioItem>,
    overflow: Arc<AtomicBool>,
    start: Instant,
}
struct AudioItem {
    format: AudioFormat,
    epoch: u64,
    first: u64,
    samples: Vec<f32>,
    elapsed: Duration,
}
impl AudioInput {
    /// Called by the network decoder, never the output callback. No disk I/O/wait.
    pub(crate) fn push(&self, format: AudioFormat, epoch: u64, first: u64, samples: &[f32]) {
        if self
            .tx
            .try_send(AudioItem {
                format,
                epoch,
                first,
                samples: samples.to_vec(),
                elapsed: self.start.elapsed(),
            })
            .is_err()
        {
            self.overflow.store(true, Ordering::Release);
        }
    }
}
pub struct Recorder {
    stop: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    worker: Option<JoinHandle<()>>,
    pub audio: AudioInput,
}
impl Recorder {
    /// Refuses overwrite. File creation, snapshot copying, JPEG and muxing happen on the worker.
    pub fn start(path: PathBuf, state: Arc<Mutex<Snapshot>>) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::sync_channel(128);
        let audio = AudioInput {
            tx,
            overflow: Arc::new(AtomicBool::new(false)),
            start: Instant::now(),
        };
        let (done, status, input) = (stop.clone(), result.clone(), audio.clone());
        let worker = thread::Builder::new()
            .name("movie-recorder".into())
            .spawn(move || {
                let run = || -> io::Result<()> {
                    let first = state.lock().unwrap().clone();
                    let size = first
                        .screen
                        .as_ref()
                        .ok_or_else(|| invalid("No screen to record yet"))?
                        .upright_size();
                    let framebuffer = first
                        .framebuffer
                        .ok_or_else(|| invalid("No screen to record yet"))?;
                    let (initial, content_revision) = {
                        let frame = framebuffer
                            .read()
                            .map_err(|_| invalid("Framebuffer unavailable"))?;
                        (frame.upright_snapshot(), frame.content_revision())
                    };
                    let mut movie = Movie::create(&path, size.0.into(), size.1.into())?;
                    movie.video(&initial, 0)?;
                    drop(initial);
                    let mut revision = Some(content_revision);
                    let mut last_frame = Some(framebuffer);
                    let mut next = Instant::now() + Duration::from_millis(66);
                    let mut audio_clock: Option<(u64, u64, u64)> = None; // epoch, expected source, written track position
                    let capture = (|| -> io::Result<()> {
                        loop {
                            // Bounded batches keep video and cancellation responsive under audio load.
                            let mut drained = false;
                            for _ in 0..128 {
                                let Ok(item) = rx.try_recv() else {
                                    drained = true;
                                    break;
                                };
                                let frames =
                                    item.samples.len() as u64 / u64::from(item.format.channels);
                                let rate = u64::from(item.format.sample_rate);
                                let anchor = (item.elapsed.as_nanos() * u128::from(rate)
                                    / 1_000_000_000)
                                    as u64;
                                let (epoch, expected, position) =
                                    audio_clock.get_or_insert((item.epoch, item.first, 0));
                                let target = if *epoch != item.epoch {
                                    *epoch = item.epoch;
                                    *expected = item.first;
                                    anchor.saturating_sub(frames).max(*position)
                                } else if *position == 0 {
                                    anchor.saturating_sub(frames)
                                } else {
                                    position.saturating_add(item.first.saturating_sub(*expected))
                                };
                                let trim = expected.saturating_sub(item.first).min(frames);
                                if trim == frames {
                                    continue;
                                }
                                movie
                                    .audio_silence(item.format, target.saturating_sub(*position))?;
                                *position = (*position).max(target);
                                movie.audio(
                                    item.format,
                                    &item.samples
                                        [trim as usize * usize::from(item.format.channels)..],
                                )?;
                                *position += frames - trim;
                                *expected = item
                                    .first
                                    .checked_add(frames)
                                    .ok_or_else(|| invalid("Audio sample index overflow"))?;
                            }
                            if input.overflow.load(Ordering::Acquire) {
                                return Err(invalid(
                                    "Recording audio queue overflow; partial file retained",
                                ));
                            }
                            if Instant::now() >= next || done.load(Ordering::Acquire) {
                                let snapshot = state.lock().unwrap().clone();
                                if snapshot.state != State::Streaming {
                                    done.store(true, Ordering::Release);
                                }
                                if let Some(framebuffer) = snapshot.framebuffer
                                    && let Ok(frame) = framebuffer.try_read()
                                {
                                    let content_revision = frame.content_revision();
                                    if revision != Some(content_revision)
                                        || !last_frame
                                            .as_ref()
                                            .is_some_and(|old| Arc::ptr_eq(old, &framebuffer))
                                    {
                                        let image = frame.upright_snapshot();
                                        drop(frame);
                                        movie.video(
                                            &image,
                                            input.start.elapsed().as_millis() as u64,
                                        )?;
                                        revision = Some(content_revision);
                                        last_frame = Some(framebuffer.clone());
                                    }
                                }
                                next += Duration::from_millis(66);
                                // No image backlog: slow encoding skips capture slots.
                                if next < Instant::now() {
                                    next = Instant::now();
                                }
                            }
                            if done.load(Ordering::Acquire) && drained {
                                // The application detaches the producer before setting stop.
                                break;
                            }
                            thread::sleep(Duration::from_millis(2));
                        }
                        Ok(())
                    })();
                    // Even format/queue failures finalize the samples already written, if possible.
                    let finalized = movie.finish(input.start.elapsed().as_millis() as u64);
                    capture.and(finalized)
                };
                let outcome = run().map(|()| path.clone()).map_err(|e| {
                    format!(
                        "Recording at {} failed (partial file retained): {e}",
                        path.display()
                    )
                });
                *status.lock().unwrap() = Some(outcome);
            })?;
        Ok(Self {
            stop,
            result,
            worker: Some(worker),
            audio,
        })
    }
    pub fn completed(&self) -> bool {
        self.result.lock().unwrap().is_some()
    }
    pub fn finish(mut self) -> Result<PathBuf, String> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "Recording worker panicked".to_owned())?;
        }
        self.result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err("Recording worker stopped unexpectedly".into()))
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Chunk {
    offset: u64,
    size: u32,
    frames: u32,
}
/// Minimal nonfragmented MOV, 64-bit mdat/co64, metadata finalized after samples.
struct Movie {
    file: File,
    width: u32,
    height: u32,
    video: Vec<Chunk>,
    timestamps: Vec<u64>,
    audio: Vec<Chunk>,
    format: Option<AudioFormat>,
    audio_frames: u64,
}
impl Movie {
    fn create(path: &Path, width: u32, height: u32) -> io::Result<Self> {
        if width == 0
            || height == 0
            || width > 65535
            || height > 65535
            || u64::from(width) * u64::from(height) * 4 > 64 * 1024 * 1024
        {
            return Err(invalid("Invalid movie dimensions"));
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&atom(b"ftyp", b"qt  \0\0\0\0qt  ".to_vec()))?;
        file.write_all(&1u32.to_be_bytes())?;
        file.write_all(b"mdat")?;
        file.write_all(&0u64.to_be_bytes())?;
        Ok(Self {
            file,
            width,
            height,
            video: vec![],
            timestamps: vec![],
            audio: vec![],
            format: None,
            audio_frames: 0,
        })
    }
    fn chunk(&mut self, bytes: &[u8], frames: u32) -> io::Result<Chunk> {
        if self.video.len() + self.audio.len() >= MAX_ENTRIES {
            return Err(invalid(
                "Recording metadata limit reached; stop and start a new recording",
            ));
        }
        let offset = self.file.stream_position()?;
        let size = u32::try_from(bytes.len()).map_err(|_| invalid("Movie sample too large"))?;
        self.file.write_all(bytes)?;
        Ok(Chunk {
            offset,
            size,
            frames,
        })
    }
    fn video(&mut self, image: &RgbaImage, ms: u64) -> io::Result<()> {
        let mut canvas =
            RgbaImage::from_pixel(self.width, self.height, image::Rgba([0, 0, 0, 255]));
        // Fit source changes into the initial canvas, preserving aspect ratio.
        let ratio = (self.width as f64 / image.width() as f64)
            .min(self.height as f64 / image.height() as f64);
        let w = ((image.width() as f64 * ratio).round() as u32).clamp(1, self.width);
        let h = ((image.height() as f64 * ratio).round() as u32).clamp(1, self.height);
        let scaled = imageops::resize(image, w, h, imageops::FilterType::Triangle);
        imageops::overlay(
            &mut canvas,
            &scaled,
            i64::from((self.width - w) / 2),
            i64::from((self.height - h) / 2),
        );
        let rgb = image::DynamicImage::ImageRgba8(canvas).into_rgb8();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 80)
            .encode_image(&rgb)
            .map_err(io::Error::other)?;
        let chunk = self.chunk(&jpeg, 1)?;
        let time = self.timestamps.last().map_or(ms, |t| ms.max(t + 1));
        self.video.push(chunk);
        self.timestamps.push(time);
        Ok(())
    }
    fn check_format(&mut self, format: AudioFormat) -> io::Result<()> {
        format.validate().map_err(io::Error::other)?;
        if let Some(old) = self.format
            && (old.sample_rate != format.sample_rate || old.channels != format.channels)
        {
            return Err(invalid(
                "Device audio rate/channels changed during recording; start a new recording",
            ));
        }
        self.format = Some(format);
        Ok(())
    }
    fn audio_silence(&mut self, format: AudioFormat, mut frames: u64) -> io::Result<()> {
        self.check_format(format)?;
        // Avoid unbounded work from malicious source-index gaps.
        if frames > u64::from(format.sample_rate) * 60 {
            return Err(invalid("Device audio gap exceeds 60 seconds"));
        }
        let silence = vec![0; 8192 * usize::from(format.channels) * 2];
        while frames > 0 {
            let n = frames.min(8192) as u32;
            self.pcm(&silence[..n as usize * usize::from(format.channels) * 2], n)?;
            frames -= u64::from(n);
        }
        Ok(())
    }
    fn audio(&mut self, format: AudioFormat, samples: &[f32]) -> io::Result<()> {
        self.check_format(format)?;
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
            .collect();
        self.pcm(
            &bytes,
            (samples.len() / usize::from(format.channels)) as u32,
        )
    }
    fn pcm(&mut self, bytes: &[u8], frames: u32) -> io::Result<()> {
        if self.audio_frames + u64::from(frames) > u64::from(u32::MAX) {
            return Err(invalid(
                "Audio track duration limit reached; start a new recording",
            ));
        }
        let chunk = self.chunk(bytes, frames)?;
        self.audio.push(chunk);
        self.audio_frames += u64::from(frames);
        Ok(())
    }
    fn finish(mut self, end: u64) -> io::Result<()> {
        if self.video.is_empty() {
            return Err(invalid("No video frames captured"));
        }
        let end = end.max(self.timestamps.last().unwrap() + 1);
        // Include initial delay in the first sample; static final frame lasts through Stop.
        let mut times = self.timestamps.clone();
        times[0] = 0;
        times.push(end);
        let durations: Vec<u32> = times
            .windows(2)
            .map(|p| u32::try_from(p[1] - p[0]).map_err(|_| invalid("Video duration overflow")))
            .collect::<io::Result<_>>()?;
        let audio_ms = self
            .format
            .map_or(0, |f| self.audio_frames * 1000 / u64::from(f.sample_rate));
        let duration = end.max(audio_ms);
        let mut moov = full(0);
        moov.extend([0; 8]);
        u32b(&mut moov, 1000);
        u32b(
            &mut moov,
            u32::try_from(duration).map_err(|_| invalid("Movie duration exceeds 49 days"))?,
        );
        u32b(&mut moov, 0x10000);
        moov.extend(0x100u16.to_be_bytes());
        moov.extend([0; 10]);
        matrix(&mut moov);
        moov.extend([0; 24]);
        u32b(&mut moov, 3);
        let mut children = atom(b"mvhd", moov);
        children.extend(track(
            1,
            (self.width, self.height),
            1000,
            end,
            &self.video,
            &durations,
            None,
        )?);
        if let Some(format) = self.format
            && self.audio_frames > 0
        {
            children.extend(track(
                2,
                (0, 0),
                format.sample_rate,
                self.audio_frames,
                &self.audio,
                &[],
                Some(format),
            )?);
        }
        let offset = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(28))?;
        self.file.write_all(&(offset - 20).to_be_bytes())?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&atom(b"moov", children))?;
        self.file.flush()?;
        self.file.sync_all()
    }
}
fn u32b(v: &mut Vec<u8>, n: u32) {
    v.extend(n.to_be_bytes());
}
fn full(flags: u32) -> Vec<u8> {
    flags.to_be_bytes().to_vec()
}
fn atom(kind: &[u8; 4], data: Vec<u8>) -> Vec<u8> {
    let mut v = Vec::with_capacity(data.len() + 8);
    u32b(&mut v, (data.len() + 8) as u32);
    v.extend(kind);
    v.extend(data);
    v
}
fn matrix(v: &mut Vec<u8>) {
    for n in [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000] {
        u32b(v, n);
    }
}
fn table(kind: &[u8; 4], entries: Vec<Vec<u8>>) -> Vec<u8> {
    let mut v = full(0);
    u32b(&mut v, entries.len() as u32);
    for e in entries {
        v.extend(e);
    }
    atom(kind, v)
}
fn nums(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|n| n.to_be_bytes()).collect()
}
fn track(
    id: u32,
    (width, height): (u32, u32),
    rate: u32,
    duration: u64,
    chunks: &[Chunk],
    durations: &[u32],
    audio: Option<AudioFormat>,
) -> io::Result<Vec<u8>> {
    let duration =
        u32::try_from(duration).map_err(|_| invalid("Track duration exceeds 32-bit timescale"))?;
    let mut tk = full(7);
    tk.extend([0; 8]);
    u32b(&mut tk, id);
    u32b(&mut tk, 0);
    u32b(
        &mut tk,
        (u64::from(duration) * 1000 / u64::from(rate)) as u32,
    );
    tk.extend([0; 12]);
    tk.extend(if audio.is_some() { 0x100u16 } else { 0u16 }.to_be_bytes());
    tk.extend([0; 2]);
    matrix(&mut tk);
    u32b(&mut tk, width << 16);
    u32b(&mut tk, height << 16);
    let mut md = full(0);
    md.extend([0; 8]);
    u32b(&mut md, rate);
    u32b(&mut md, duration);
    md.extend([0; 4]);
    let mut handler = full(0);
    handler.extend(b"mhlr");
    handler.extend(if audio.is_some() { b"soun" } else { b"vide" });
    handler.extend([0; 12]);
    handler.extend(b"\0");
    let mut entry = vec![0; 6];
    entry.extend(1u16.to_be_bytes());
    let codec = if let Some(f) = audio {
        let high = f.sample_rate > 65535;
        entry.extend(if high { 2u16 } else { 0u16 }.to_be_bytes());
        entry.extend([0; 6]);
        entry.extend(if high { 3u16 } else { u16::from(f.channels) }.to_be_bytes());
        entry.extend(16u16.to_be_bytes());
        entry.extend(if high { (-2i16).to_be_bytes() } else { [0; 2] });
        entry.extend([0; 2]);
        u32b(&mut entry, if high { 65536 } else { f.sample_rate << 16 });
        if high {
            u32b(&mut entry, 72);
            entry.extend(f64::from(f.sample_rate).to_be_bytes());
            u32b(&mut entry, u32::from(f.channels));
            u32b(&mut entry, 0x7f000000);
            u32b(&mut entry, 16);
            u32b(&mut entry, 12);
            u32b(&mut entry, u32::from(f.channels) * 2);
            u32b(&mut entry, 1);
        }
        b"sowt"
    } else {
        entry.extend([0; 16]);
        entry.extend((width as u16).to_be_bytes());
        entry.extend((height as u16).to_be_bytes());
        u32b(&mut entry, 72 << 16);
        u32b(&mut entry, 72 << 16);
        u32b(&mut entry, 0);
        entry.extend(1u16.to_be_bytes());
        entry.extend([0; 32]);
        entry.extend(24u16.to_be_bytes());
        entry.extend((-1i16).to_be_bytes());
        b"jpeg"
    };
    let mut stbl = table(b"stsd", vec![atom(codec, entry)]);
    let mut runs: Vec<(u32, u32)> = Vec::new();
    if audio.is_some() {
        runs.push((duration, 1));
    } else {
        for &d in durations {
            if let Some(last) = runs.last_mut()
                && last.1 == d
            {
                last.0 += 1;
                continue;
            }
            runs.push((1, d));
        }
    }
    stbl.extend(table(
        b"stts",
        runs.into_iter().map(|(n, d)| nums(&[n, d])).collect(),
    ));
    let mut sc = Vec::new();
    let mut previous = 0;
    for (i, c) in chunks.iter().enumerate() {
        if c.frames != previous {
            sc.push(nums(&[i as u32 + 1, c.frames, 1]));
            previous = c.frames;
        }
    }
    stbl.extend(table(b"stsc", sc));
    let mut sizes = full(0);
    u32b(&mut sizes, audio.map_or(0, |f| u32::from(f.channels) * 2));
    u32b(
        &mut sizes,
        if audio.is_some() {
            duration
        } else {
            chunks.len() as u32
        },
    );
    if audio.is_none() {
        for c in chunks {
            u32b(&mut sizes, c.size);
        }
    }
    stbl.extend(atom(b"stsz", sizes));
    stbl.extend(table(
        b"co64",
        chunks
            .iter()
            .map(|c| c.offset.to_be_bytes().to_vec())
            .collect(),
    ));
    let mut minf = if audio.is_some() {
        atom(b"smhd", vec![0; 8])
    } else {
        let mut v = full(1);
        v.extend([0; 8]);
        atom(b"vmhd", v)
    };
    minf.extend(atom(b"dinf", table(b"dref", vec![atom(b"url ", full(1))])));
    minf.extend(atom(b"stbl", stbl));
    let mut mdia = atom(b"mdhd", md);
    mdia.extend(atom(b"hdlr", handler));
    mdia.extend(atom(b"minf", minf));
    let mut trak = atom(b"tkhd", tk);
    trak.extend(atom(b"mdia", mdia));
    Ok(atom(b"trak", trak))
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;
