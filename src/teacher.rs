//! Device observations, local teaching feedback and locally approved assisted input.
//! The bridge defaults to read-only; IPC cannot grant leases or approve gestures.
mod ipc;
pub use ipc::{Bridge, default_socket_path, request};

use crate::{
    control::{Controller, Gesture, Point},
    framebuffer::Framebuffer,
    session::{Session, Snapshot, State},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, Cursor},
    sync::{Arc, Mutex, RwLock, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const OBSERVATIONS: usize = 16;
const OBSERVATION_TTL: Duration = Duration::from_secs(60);
pub const MAX_IMAGE_SIZE: u32 = 2048;
pub const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
pub type SnapshotSource = Arc<Mutex<Snapshot>>;
type Screen = Arc<RwLock<Framebuffer>>;

/// Integer pixel bounds, with exclusive right/bottom edges.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Rect {
    pub fn fits(self, width: u32, height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && self.x.checked_add(self.width).is_some_and(|n| n <= width)
            && self.y.checked_add(self.height).is_some_and(|n| n <= height)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    GetState,
    GetScreen {
        region: Option<Rect>,
        max_size: Option<u32>,
    },
    Highlight {
        observation_id: String,
        bounds: Rect,
        label: Option<String>,
        duration_ms: Option<u64>,
    },
    ClearHighlight,
    Propose {
        observation_id: String,
        gesture: Gesture,
    },
    Cancel,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    Ok { data: Value, image: Option<String> },
    Error { message: String },
}
impl Reply {
    pub fn data(data: Value) -> Self {
        Self::Ok { data, image: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    /// Upright device pixels, never window coordinates.
    pub bounds: Rect,
    pub label: String,
    pub observation_id: String,
    pub expires_at: Instant,
}
struct Observation {
    id: String,
    generation: u64,
    screen: Weak<RwLock<Framebuffer>>,
    created: Instant,
    region: Rect,
    image_width: u32,
    image_height: u32,
    control_epoch: Option<u64>,
}
struct Inner {
    source: Option<SnapshotSource>,
    control: Option<Arc<Controller>>,
    generation: u64,
    next_id: u64,
    observations: VecDeque<Observation>,
    highlight: Option<Highlight>,
}

pub struct Teacher {
    nonce: String,
    inner: Mutex<Inner>,
}
impl Teacher {
    pub fn new() -> io::Result<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        Ok(Self {
            nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
            inner: Mutex::new(Inner {
                source: None,
                control: None,
                generation: 0,
                next_id: 0,
                observations: VecDeque::new(),
                highlight: None,
            }),
        })
    }
    /// Replace/clear the source before detaching a Session; revoke any attached lease.
    pub fn set_source(&self, source: Option<SnapshotSource>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(control) = inner.control.take() {
            control.revoke("AI teacher sharing revoked or source replaced");
        }
        inner.source = source;
        inner.generation = inner.generation.wrapping_add(1);
        inner.observations.clear();
        inner.highlight = None;
    }
    /// Attach input arbitration, still read-only until a LOCAL lease/approval.
    pub fn set_session(&self, session: &Session) {
        self.set_source(Some(session.snapshot_source()));
        self.inner.lock().unwrap().control = Some(session.control());
    }
    pub fn control(&self) -> Option<Arc<Controller>> {
        self.inner.lock().unwrap().control.clone()
    }
    pub fn handle(&self, request: Request) -> Result<Reply, String> {
        match request {
            Request::GetState => self.state().map(Reply::data),
            Request::GetScreen { region, max_size } => self.capture(region, max_size),
            Request::Highlight {
                observation_id,
                bounds,
                label,
                duration_ms,
            } => self
                .highlight(&observation_id, bounds, label, duration_ms)
                .map(Reply::data),
            Request::Propose {
                observation_id,
                gesture,
            } => self.propose(&observation_id, gesture).map(Reply::data),
            Request::Cancel => {
                let control = self.control().ok_or("No assisted-input session")?;
                control.cancel();
                Ok(Reply::data(control.state()))
            }
            Request::ClearHighlight => {
                self.inner.lock().unwrap().highlight = None;
                Ok(Reply::data(
                    json!({"cleared": true, "device_input_sent": false}),
                ))
            }
        }
    }
    fn source(&self) -> Result<(u64, SnapshotSource), String> {
        let inner = self.inner.lock().unwrap();
        Ok((
            inner.generation,
            inner
                .source
                .clone()
                .ok_or("No device session; connect in the viewer")?,
        ))
    }
    pub fn state(&self) -> Result<Value, String> {
        let (source, control) = {
            let inner = self.inner.lock().unwrap();
            (inner.source.clone(), inner.control.clone())
        };
        let snapshot = match source {
            Some(source) => source
                .try_lock()
                .map_err(|_| "Device state busy; retry")?
                .clone(),
            None => Snapshot::default(),
        };
        let pixels = snapshot.framebuffer.as_ref().and_then(|f| {
            let f = f.try_read().ok()?;
            Some(json!({
                "content_revision": f.content_revision(),
                "has_pixels": f.content_updated_at().is_some(),
                "pixel_update_age_ms": f.content_updated_at().map(|at| at.elapsed().as_millis() as u64),
            }))
        });
        Ok(json!({
            "viewer_available": true,
            "read_only": !control.as_ref().is_some_and(|c| c.status().lease_active),
            "assisted_control": control.map(|c| c.state()),
            "state": format!("{:?}", snapshot.state),
            "error": snapshot.error,
            "protocol_version": snapshot.protocol_version,
            "device": snapshot.device.as_ref().map(|d| json!({"model": d.model, "firmware": d.firmware})),
            "screen": snapshot.screen.as_ref().map(|s| {
                let (w, h) = s.upright_size();
                json!({"name": s.name, "width": w, "height": h, "rotation": s.rotation,
                    "capabilities": {"touch": s.input_capabilities & 1 != 0,
                        "mouse": s.input_capabilities & 2 != 0,
                        "keyboard": s.input_capabilities & 4 != 0,
                        "audio": s.audio_available}})
            }),
            "pixels": pixels,
            "stream_health": {
                "processed_frames": snapshot.statistics.frames,
                "skipped_rectangles_total": snapshot.statistics.skipped_rectangles,
                "latest_frame_quality": (snapshot.statistics.frames > 0).then_some(snapshot.statistics.quality),
                "quality_note": "0 is best; skipped rectangles are cumulative for this connection attempt",
                "rtt_ms": snapshot.statistics.rtt.map(|d| d.as_secs_f64() * 1000.0),
            },
            "note": "Pixel age can grow on a healthy static screen. Screen name is metadata, not the current UI mode."
        }))
    }
    fn capture(&self, region: Option<Rect>, max_size: Option<u32>) -> Result<Reply, String> {
        let max_size = max_size.unwrap_or(1280);
        if !(64..=MAX_IMAGE_SIZE).contains(&max_size) {
            return Err(format!("max_size must be in 64..={MAX_IMAGE_SIZE}"));
        }
        let (generation, source) = self.source()?;
        let control_epoch = self.control().map(|c| c.epoch());
        let screen = streaming_screen(&source)?;
        let observed_at = Instant::now();
        // Only the bounded pixel copy holds the live lock. Rotate/resize/PNG off-lock.
        let frame = screen
            .try_read()
            .map_err(|_| "Framebuffer busy; retry")?
            .clone();
        if frame.content_updated_at().is_none() {
            return Err("No decoded device pixels yet; retry after the first frame".into());
        }
        let (w, h) = frame.configuration().upright_size();
        let region = region.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: w.into(),
            height: h.into(),
        });
        if !region.fits(w.into(), h.into()) {
            return Err("region is outside the upright device image".into());
        }
        let captured_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let content_revision = frame.content_revision();
        let content_updated_at = frame.content_updated_at();
        let upright = frame.upright_snapshot();
        drop(frame);
        let image = if region.x == 0
            && region.y == 0
            && region.width == u32::from(w)
            && region.height == u32::from(h)
        {
            upright
        } else {
            let cropped = image::imageops::crop_imm(
                &upright,
                region.x,
                region.y,
                region.width,
                region.height,
            )
            .to_image();
            drop(upright);
            cropped
        };
        let image = image::DynamicImage::ImageRgba8(image);
        let image = if image.width().max(image.height()) > max_size {
            image.thumbnail(max_size, max_size)
        } else {
            image
        };
        let (iw, ih) = (image.width(), image.height());
        let mut png = Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| format!("PNG capture failed: {e}"))?;
        if png.get_ref().len() > MAX_PNG_BYTES {
            return Err(
                "Encoded screenshot exceeds the image limit; request a smaller max_size".into(),
            );
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.generation != generation || !same_screen(&source, &screen) {
            return Err("Device source changed during capture; get a new screen".into());
        }
        inner.next_id = inner.next_id.wrapping_add(1);
        let id = format!("{}-{}", self.nonce, inner.next_id);
        inner
            .observations
            .retain(|o| o.created.elapsed() < OBSERVATION_TTL);
        while inner.observations.len() >= OBSERVATIONS {
            inner.observations.pop_front();
        }
        inner.observations.push_back(Observation {
            id: id.clone(),
            generation,
            screen: Arc::downgrade(&screen),
            created: observed_at,
            region,
            image_width: iw,
            image_height: ih,
            control_epoch,
        });
        drop(inner);
        Ok(Reply::Ok {
            data: json!({
                "observation_id": id, "generation": generation,
                "content_revision": content_revision, "captured_at_unix_ms": captured_at,
                "pixel_update_age_ms": content_updated_at.map(|at| at.elapsed().as_millis() as u64),
                "device_width": w, "device_height": h, "region": region,
                "image_width": iw, "image_height": ih, "expires_in_ms": OBSERVATION_TTL.saturating_sub(observed_at.elapsed()).as_millis() as u64,
                "coordinates": "returned-image pixels; top-left origin; right/bottom exclusive",
                "device_mapping": {"offset_x": region.x, "offset_y": region.y,
                    "scale_x": region.width as f64 / iw as f64, "scale_y": region.height as f64 / ih as f64},
                "overlay_included": false,
            }),
            image: Some(STANDARD.encode(png.into_inner())),
        })
    }
    fn propose(&self, id: &str, gesture: Gesture) -> Result<Value, String> {
        let inner = self.inner.lock().unwrap();
        let observation = inner
            .observations
            .iter()
            .find(|o| o.id == id)
            .ok_or("Unknown observation; capture a fresh screen")?;
        if !observation_valid(&inner, observation)? {
            return Err("Expired or changed device observation; capture a fresh screen".into());
        }
        let control = inner
            .control
            .as_ref()
            .ok_or("Read-only source; assisted input is unavailable")?;
        let epoch = observation
            .control_epoch
            .ok_or("Capture a fresh screen after attaching input control")?;
        let screen = observation
            .screen
            .upgrade()
            .ok_or("Device source disappeared")?;
        let source = inner.source.as_ref().ok_or("No device session")?;
        let snapshot = source
            .try_lock()
            .map_err(|_| "Device state busy; retry observation (not mutation)")?;
        let init = snapshot.screen.as_ref().ok_or("No screen configuration")?;
        let map = |p: Point| -> Result<Point, String> {
            if p.x >= observation.image_width || p.y >= observation.image_height {
                return Err("Target is outside the returned screenshot".into());
            }
            Ok(Point {
                x: observation.region.x
                    + (u64::from(p.x) * u64::from(observation.region.width)
                        / u64::from(observation.image_width)) as u32,
                y: observation.region.y
                    + (u64::from(p.y) * u64::from(observation.region.height)
                        / u64::from(observation.image_height)) as u32,
            })
        };
        let gesture = match gesture {
            Gesture::Click { target, hold_ms } => Gesture::Click {
                target: map(target)?,
                hold_ms,
            },
            Gesture::Drag {
                start,
                end,
                duration_ms,
            } => Gesture::Drag {
                start: map(start)?,
                end: map(end)?,
                duration_ms,
            },
            Gesture::Scroll { target, amount } => Gesture::Scroll {
                target: map(target)?,
                amount,
            },
            gesture => gesture,
        };
        control.propose(
            &screen,
            init.input_capabilities,
            init.upright_size(),
            gesture,
            epoch,
            observation.created + OBSERVATION_TTL,
        )
    }
    fn highlight(
        &self,
        id: &str,
        bounds: Rect,
        label: Option<String>,
        duration_ms: Option<u64>,
    ) -> Result<Value, String> {
        let duration_ms = duration_ms.unwrap_or(5000);
        if !(100..=10_000).contains(&duration_ms) {
            return Err("duration_ms must be in 100..=10000".into());
        }
        let label = label.unwrap_or_else(|| "Next control".into());
        if label.chars().count() > 120 || label.chars().any(char::is_control) {
            return Err("label must be at most 120 characters without control characters".into());
        }
        let mut inner = self.inner.lock().unwrap();
        let observation = inner
            .observations
            .iter()
            .find(|o| o.id == id)
            .ok_or("Unknown observation; capture the screen again")?;
        if !observation_valid(&inner, observation)? {
            return Err("Expired or changed device observation; capture the screen again".into());
        }
        if !bounds.fits(observation.image_width, observation.image_height) {
            return Err("Highlight bounds are outside the returned screenshot".into());
        }
        let device_bounds = map_bounds(
            bounds,
            observation.region,
            observation.image_width,
            observation.image_height,
        );
        inner.highlight = Some(Highlight {
            bounds: device_bounds,
            label: format!("AI teacher · {label}"),
            observation_id: id.into(),
            expires_at: Instant::now() + Duration::from_millis(duration_ms),
        });
        Ok(
            json!({"highlighted": true, "device_bounds": device_bounds, "duration_ms": duration_ms, "device_input_sent": false}),
        )
    }
    /// Poll from GPUI. Expiry/reconfiguration clears guidance without a queued UI job.
    pub fn overlay(&self) -> Option<Highlight> {
        let mut inner = self.inner.lock().unwrap();
        let valid = inner.highlight.as_ref().is_some_and(|h| {
            h.expires_at > Instant::now()
                && inner
                    .observations
                    .iter()
                    .find(|o| o.id == h.observation_id)
                    .is_some_and(|o| observation_valid(&inner, o).unwrap_or(true))
        });
        if !valid {
            inner.highlight = None;
        }
        inner.highlight.clone()
    }
}
fn streaming_screen(source: &SnapshotSource) -> Result<Screen, String> {
    let snapshot = source.try_lock().map_err(|_| "Device state busy; retry")?;
    if snapshot.state != State::Streaming {
        return Err("Device is not streaming; connect in the viewer".into());
    }
    snapshot
        .framebuffer
        .clone()
        .ok_or_else(|| "No device framebuffer".into())
}
fn same_screen(source: &SnapshotSource, screen: &Screen) -> bool {
    streaming_screen(source).is_ok_and(|current| Arc::ptr_eq(&current, screen))
}
fn observation_valid(inner: &Inner, observation: &Observation) -> Result<bool, String> {
    if observation.generation != inner.generation
        || observation.created.elapsed() >= OBSERVATION_TTL
    {
        return Ok(false);
    }
    let Some(source) = &inner.source else {
        return Ok(false);
    };
    let Some(screen) = observation.screen.upgrade() else {
        return Ok(false);
    };
    let snapshot = source.try_lock().map_err(|_| "Device state busy; retry")?;
    Ok(snapshot.state == State::Streaming
        && snapshot
            .framebuffer
            .as_ref()
            .is_some_and(|f| Arc::ptr_eq(f, &screen)))
}
/// Cover the whole requested returned-image rectangle, including fractional pixels.
fn map_bounds(bounds: Rect, region: Rect, image_width: u32, image_height: u32) -> Rect {
    let sx = region.width as f64 / image_width as f64;
    let sy = region.height as f64 / image_height as f64;
    let x = (bounds.x as f64 * sx).floor() as u32;
    let y = (bounds.y as f64 * sy).floor() as u32;
    let right = ((bounds.x + bounds.width) as f64 * sx).ceil() as u32;
    let bottom = ((bounds.y + bounds.height) as f64 * sy).ceil() as u32;
    Rect {
        x: region.x + x,
        y: region.y + y,
        width: right - x,
        height: bottom - y,
    }
}

/// Same upright transform as the displayed device image, including fit and pan.
pub fn project_bounds(
    bounds: Rect,
    picture: crate::viewport::Picture,
    upright: (u16, u16),
) -> crate::viewport::Picture {
    if upright.0 == 0 || upright.1 == 0 {
        return crate::viewport::Picture::default();
    }
    let sx = picture.width / f32::from(upright.0);
    let sy = picture.height / f32::from(upright.1);
    crate::viewport::Picture {
        left: picture.left + bounds.x as f32 * sx,
        top: picture.top + bounds.y as f32 * sy,
        width: bounds.width as f32 * sx,
        height: bounds.height as f32 * sy,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::protocol::{Bounds, FrameUpdate, PixelFormat, Rectangle, ServerInit};
    pub(crate) fn unique() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    fn source(rotation: u8) -> SnapshotSource {
        let init = ServerInit {
            width: 200,
            height: 100,
            rotation,
            pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
            name: "synthetic".into(),
            input_capabilities: 7,
            audio_available: true,
        };
        let mut frame = Framebuffer::new(init.clone()).unwrap();
        frame.apply(&FrameUpdate {
            id: 1,
            quality: 0,
            rectangles: vec![Rectangle {
                bounds: Bounds {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 100,
                },
                codec: 3,
                drop_bits: 0,
                encoded: &[0, 0, 255, 0],
            }],
        });
        Arc::new(Mutex::new(Snapshot {
            state: State::Streaming,
            screen: Some(init),
            framebuffer: Some(Arc::new(RwLock::new(frame))),
            ..Snapshot::default()
        }))
    }
    fn capture(teacher: &Teacher, region: Option<Rect>, max_size: Option<u32>) -> (Value, String) {
        let Reply::Ok {
            data,
            image: Some(image),
        } = teacher
            .handle(Request::GetScreen { region, max_size })
            .unwrap()
        else {
            panic!("Missing capture");
        };
        (data, image)
    }
    fn mark(teacher: &Teacher, id: &str) -> Result<Value, String> {
        teacher.highlight(
            id,
            Rect {
                x: 1,
                y: 1,
                width: 5,
                height: 5,
            },
            Some("Press here".into()),
            None,
        )
    }
    #[test]
    fn capture_is_upright_bounded_and_overlay_free() {
        for rotation in 0..4 {
            let teacher = Teacher::new().unwrap();
            teacher.set_source(Some(source(rotation)));
            let (data, bytes) = capture(&teacher, None, None);
            let image = image::load_from_memory(&STANDARD.decode(bytes).unwrap())
                .unwrap()
                .to_rgba8();
            let size = if rotation % 2 == 0 {
                (200, 100)
            } else {
                (100, 200)
            };
            assert_eq!(image.dimensions(), size);
            assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
            assert_eq!(data["overlay_included"], false);
            mark(&teacher, data["observation_id"].as_str().unwrap()).unwrap();
            let (_, again) = capture(&teacher, None, None);
            assert_eq!(
                image,
                image::load_from_memory(&STANDARD.decode(again).unwrap())
                    .unwrap()
                    .to_rgba8()
            );
        }
    }
    #[test]
    fn crop_resize_highlight_roundtrip_and_projected_fit_pan() {
        let teacher = Teacher::new().unwrap();
        teacher.set_source(Some(source(0)));
        let region = Rect {
            x: 20,
            y: 10,
            width: 100,
            height: 60,
        };
        let (data, _) = capture(&teacher, Some(region), Some(64));
        let bounds = Rect {
            x: 0,
            y: 0,
            width: data["image_width"].as_u64().unwrap() as u32,
            height: data["image_height"].as_u64().unwrap() as u32,
        };
        teacher
            .highlight(data["observation_id"].as_str().unwrap(), bounds, None, None)
            .unwrap();
        assert_eq!(teacher.overlay().unwrap().bounds, region);
        let picture = crate::viewport::Picture {
            left: -30.0,
            top: 50.0,
            width: 400.0,
            height: 200.0,
        };
        assert_eq!(
            project_bounds(region, picture, (200, 100)),
            crate::viewport::Picture {
                left: 10.0,
                top: 70.0,
                width: 200.0,
                height: 120.0
            }
        );
    }
    #[test]
    fn highlights_expire_and_reconfigure_disconnect_revoke_invalidate() {
        let teacher = Teacher::new().unwrap();
        let source = source(0);
        teacher.set_source(Some(source.clone()));
        let (data, _) = capture(&teacher, None, None);
        let id = data["observation_id"].as_str().unwrap();
        mark(&teacher, id).unwrap();
        // A transient session lock must not erase a valid overlay.
        let guard = source.lock().unwrap();
        assert!(teacher.overlay().is_some());
        drop(guard);
        teacher
            .inner
            .lock()
            .unwrap()
            .highlight
            .as_mut()
            .unwrap()
            .expires_at = Instant::now();
        assert!(teacher.overlay().is_none());
        mark(&teacher, id).unwrap();
        let replacement = self::source(0).lock().unwrap().framebuffer.clone();
        source.lock().unwrap().framebuffer = replacement;
        assert!(teacher.overlay().is_none());
        assert!(mark(&teacher, id).is_err());
        let (data, _) = capture(&teacher, None, None);
        let id = data["observation_id"].as_str().unwrap();
        mark(&teacher, id).unwrap();
        source.lock().unwrap().state = State::RetryWait;
        assert!(teacher.overlay().is_none());
        assert!(mark(&teacher, id).is_err());
        teacher.set_source(None);
        assert!(
            teacher
                .handle(Request::GetScreen {
                    region: None,
                    max_size: None
                })
                .is_err()
        );
    }
    #[test]
    fn assisted_targets_map_crop_resize_upright_and_consume_observations() {
        for rotation in 0..4 {
            let teacher = Teacher::new().unwrap();
            let source = source(rotation);
            let frame = source.lock().unwrap().framebuffer.clone().unwrap();
            let control = Arc::new(Controller::default());
            teacher.set_source(Some(source.clone()));
            teacher.inner.lock().unwrap().control = Some(control.clone());
            let (data, _) = capture(
                &teacher,
                Some(Rect {
                    x: 10,
                    y: 10,
                    width: 80,
                    height: 60,
                }),
                Some(64),
            );
            let id = data["observation_id"].as_str().unwrap();
            assert!(
                teacher
                    .propose(
                        id,
                        Gesture::Click {
                            target: Point { x: 32, y: 24 },
                            hold_ms: 100
                        }
                    )
                    .is_err()
            );
            control.grant(&frame).unwrap();
            assert!(
                teacher
                    .propose(
                        id,
                        Gesture::Click {
                            target: Point { x: 64, y: 0 },
                            hold_ms: 100
                        }
                    )
                    .is_err()
            );
            let result = teacher
                .propose(
                    id,
                    Gesture::Click {
                        target: Point { x: 32, y: 24 },
                        hold_ms: 100,
                    },
                )
                .unwrap();
            assert_eq!(result["action"]["position"], json!({"x": 50, "y": 40}));
            assert_eq!(result["action"]["phase"], "pending_approval");
            assert!(
                teacher
                    .propose(
                        id,
                        Gesture::Click {
                            target: Point { x: 32, y: 24 },
                            hold_ms: 100
                        }
                    )
                    .is_err()
            );
            teacher.handle(Request::Cancel).unwrap();
            assert!(
                teacher
                    .propose(
                        id,
                        Gesture::PressKey {
                            key: "a".into(),
                            modifiers: [false; 4]
                        }
                    )
                    .is_err()
            );
            let (fresh, _) = capture(&teacher, None, None);
            source.lock().unwrap().state = State::RetryWait;
            assert!(
                teacher
                    .propose(
                        fresh["observation_id"].as_str().unwrap(),
                        Gesture::TypeText { text: "abc".into() }
                    )
                    .is_err()
            );
            teacher.set_source(None);
            assert!(!control.status().lease_active);
        }
    }
    #[test]
    fn ipc_cannot_grant_approve_or_assert_confirmation() {
        for request in [
            json!({"op": "grant"}),
            json!({"op": "approve", "id": 1}),
            json!({"op": "propose", "observation_id": "id", "gesture": {"kind": "click", "target": {"x": 1, "y": 1}, "hold_ms": 100}, "confirmed": true}),
            json!({"op": "propose", "observation_id": "id", "gesture": {"kind": "click", "target": {"x": 1, "y": 1}, "hold_ms": 100, "confirmed": true}}),
        ] {
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
    }
    #[test]
    fn observation_cache_ttl_and_arguments_are_bounded() {
        let teacher = Teacher::new().unwrap();
        teacher.set_source(Some(source(0)));
        let (first, _) = capture(&teacher, None, None);
        for _ in 0..OBSERVATIONS {
            capture(&teacher, None, None);
        }
        assert_eq!(
            teacher.inner.lock().unwrap().observations.len(),
            OBSERVATIONS
        );
        assert!(mark(&teacher, first["observation_id"].as_str().unwrap()).is_err());
        let (data, _) = capture(&teacher, None, None);
        let id = data["observation_id"].as_str().unwrap();
        assert!(
            teacher
                .highlight(
                    id,
                    Rect {
                        x: u32::MAX,
                        y: 0,
                        width: 10,
                        height: 1
                    },
                    None,
                    None
                )
                .is_err()
        );
        assert!(
            teacher
                .highlight(
                    id,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1
                    },
                    Some("bad\nlabel".into()),
                    None
                )
                .is_err()
        );
        assert!(
            teacher
                .highlight(
                    id,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1
                    },
                    None,
                    Some(10001)
                )
                .is_err()
        );
        teacher
            .inner
            .lock()
            .unwrap()
            .observations
            .back_mut()
            .unwrap()
            .created = Instant::now() - OBSERVATION_TTL;
        assert!(mark(&teacher, id).is_err());
        assert!(
            teacher
                .handle(Request::GetScreen {
                    region: None,
                    max_size: Some(2049)
                })
                .is_err()
        );
        assert!(
            teacher
                .handle(Request::GetScreen {
                    region: Some(Rect {
                        x: 199,
                        y: 0,
                        width: 2,
                        height: 1
                    }),
                    max_size: None
                })
                .is_err()
        );
    }
}
