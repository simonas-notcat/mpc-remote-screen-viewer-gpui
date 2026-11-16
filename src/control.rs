//! GUI-independent assisted input. A local global toggle authorizes bounded gestures.
//! The session worker alone drives plans and reports successful socket writes.
use crate::{
    framebuffer::Framebuffer,
    input::{self, Control},
    protocol::InputEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

type Screen = Arc<RwLock<Framebuffer>>;
const QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(5);
const FEEDBACK_DURATION: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}
impl Point {
    fn wire(self, size: (u16, u16)) -> Result<(u16, u16), String> {
        if self.x >= u32::from(size.0) || self.y >= u32::from(size.1) {
            return Err("Target is outside the upright device image".into());
        }
        Ok((self.x as u16, self.y as u16))
    }
}

/// Upright device coordinates here; the teacher maps returned-image coordinates.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Gesture {
    Click {
        target: Point,
        hold_ms: u64,
    },
    Drag {
        start: Point,
        end: Point,
        duration_ms: u64,
    },
    Scroll {
        target: Point,
        amount: i16,
    },
    PressKey {
        key: String,
        modifiers: [bool; 4],
    },
    TypeText {
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Running,
    Completed,
    Rejected,
    Cancelled,
    Uncertain,
}
impl Phase {
    pub fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Feedback {
    pub id: u64,
    pub phase: Phase,
    /// Generated summary only: never retain text or model-supplied labels here.
    pub summary: String,
    pub gesture: &'static str,
    pub start: Option<Point>,
    pub end: Option<Point>,
    pub position: Option<Point>,
    pub path: Vec<Point>,
    pub scroll_amount: Option<i16>,
    pub held: bool,
    pub written_steps: usize,
    pub total_steps: usize,
    pub release_written: bool,
    pub cancel_requested: bool,
    pub reason: Option<String>,
    #[serde(skip)]
    pub visible_until: Option<Instant>,
    #[serde(skip)]
    pub overlay_visible: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub human_held: bool,
    pub feedback: Option<Feedback>,
}

struct Step {
    at: Duration,
    events: Vec<InputEvent>,
    position: Option<Point>,
    held: bool,
}
struct Plan {
    steps: Vec<Step>,
    caps: u8,
    feedback: Feedback,
}
impl Plan {
    fn new(gesture: Gesture, caps: u8, size: (u16, u16)) -> Result<Self, String> {
        let mut steps = Vec::new();
        let (kind, summary, start, end, scroll, required) = match gesture {
            Gesture::Click { target, hold_ms } => {
                if !(50..=2000).contains(&hold_ms) {
                    return Err("hold_ms must be in 50..=2000".into());
                }
                let p = target.wire(size)?;
                let (down, up, required) = pointer_kinds(caps)?;
                steps.push(Step {
                    at: Duration::ZERO,
                    events: vec![input::pointer(down, 0, p, 0)],
                    position: Some(target),
                    held: true,
                });
                steps.push(Step {
                    at: Duration::from_millis(hold_ms),
                    events: vec![input::pointer(up, 0, p, 0)],
                    position: Some(target),
                    held: false,
                });
                (
                    "click",
                    format!("Click ({}, {}) · hold {hold_ms} ms", target.x, target.y),
                    Some(target),
                    Some(target),
                    None,
                    required,
                )
            }
            Gesture::Drag {
                start,
                end,
                duration_ms,
            } => {
                if !(100..=3000).contains(&duration_ms) {
                    return Err("duration_ms must be in 100..=3000".into());
                }
                let p = start.wire(size)?;
                end.wire(size)?;
                let (down, up, required) = pointer_kinds(caps)?;
                let count = duration_ms.div_ceil(50);
                steps.push(Step {
                    at: Duration::ZERO,
                    events: vec![input::pointer(down, 0, p, 0)],
                    position: Some(start),
                    held: true,
                });
                for i in 1..=count {
                    let interpolate = |a: u32, b: u32| {
                        (a as f64 + (b as f64 - a as f64) * i as f64 / count as f64).round() as u32
                    };
                    let point = Point {
                        x: interpolate(start.x, end.x),
                        y: interpolate(start.y, end.y),
                    };
                    steps.push(Step {
                        at: Duration::from_millis(duration_ms * i / count),
                        events: vec![input::pointer(
                            if required == 1 { 1 } else { 3 },
                            0,
                            point.wire(size)?,
                            0,
                        )],
                        position: Some(point),
                        held: true,
                    });
                }
                steps
                    .last_mut()
                    .unwrap()
                    .events
                    .push(input::pointer(up, 0, end.wire(size)?, 0));
                steps.last_mut().unwrap().held = false;
                (
                    "drag",
                    format!(
                        "Drag ({}, {}) → ({}, {}) · {duration_ms} ms",
                        start.x, start.y, end.x, end.y
                    ),
                    Some(start),
                    Some(end),
                    None,
                    required,
                )
            }
            Gesture::Scroll { target, amount } => {
                if amount == 0 || !(-120..=120).contains(&amount) {
                    return Err("amount must be nonzero in -120..=120 wire scroll units".into());
                }
                steps.push(Step {
                    at: Duration::ZERO,
                    events: vec![input::pointer(6, 0, target.wire(size)?, amount)],
                    position: Some(target),
                    held: false,
                });
                (
                    "scroll",
                    format!(
                        "Scroll ({}, {}) · {amount:+} wire units",
                        target.x, target.y
                    ),
                    Some(target),
                    Some(target),
                    Some(amount),
                    2,
                )
            }
            Gesture::PressKey { key, modifiers } => {
                if input::key(&key).is_none() {
                    return Err("Unsupported Linux wire key name".into());
                }
                let mut control = Control::default();
                let mut events = control.key(&key, modifiers);
                events.extend(control.modifiers([false; 4]));
                steps.push(Step {
                    at: Duration::ZERO,
                    events,
                    position: None,
                    held: false,
                });
                (
                    "key",
                    "Key press (contents hidden)".into(),
                    None,
                    None,
                    None,
                    4,
                )
            }
            Gesture::TypeText { text } => {
                if text.is_empty()
                    || text.len() > 64
                    || !text.bytes().all(|b| (32..=126).contains(&b))
                {
                    return Err("text must contain 1..=64 printable ASCII characters; no control characters".into());
                }
                for (i, c) in text.chars().enumerate() {
                    steps.push(Step {
                        at: Duration::from_millis(i as u64 * 30),
                        events: Control::default().key(&c.to_string(), [false; 4]),
                        position: None,
                        held: false,
                    });
                }
                (
                    "text",
                    format!("Type {} ASCII characters (contents hidden)", text.len()),
                    None,
                    None,
                    None,
                    4,
                )
            }
        };
        if caps & required == 0 {
            return Err("Device does not advertise the required input capability".into());
        }
        let feedback = Feedback {
            id: 0,
            phase: Phase::Queued,
            summary,
            gesture: kind,
            start,
            end,
            position: start,
            path: Vec::new(),
            scroll_amount: scroll,
            held: false,
            written_steps: 0,
            total_steps: steps.len(),
            release_written: false,
            cancel_requested: false,
            reason: None,
            visible_until: None,
            overlay_visible: true,
        };
        Ok(Self {
            steps,
            caps: required,
            feedback,
        })
    }
}
fn pointer_kinds(caps: u8) -> Result<(u8, u8, u8), String> {
    if caps & 2 != 0 {
        Ok((4, 5, 2))
    } else if caps & 1 != 0 {
        Ok((0, 2, 1))
    } else {
        Err("Device does not advertise mouse or touch input".into())
    }
}
struct Pending {
    screen: Weak<RwLock<Framebuffer>>,
    plan: Plan,
    until: Instant,
    observation_expires_at: Instant,
    cancelled: Arc<AtomicBool>,
}
#[derive(Default)]
struct Inner {
    enabled: bool,
    pending: Option<Pending>,
    feedback: Option<Feedback>,
    cancellation: Option<Arc<AtomicBool>>,
    human: HashSet<(u8, u16)>,
    local_writing: bool,
    epoch: u64,
    next_id: u64,
}
impl Inner {
    fn stop(&mut self, reason: &str, disable: bool) {
        if disable {
            self.enabled = false;
        }
        if let Some(cancel) = &self.cancellation {
            cancel.store(true, Ordering::Release);
        }
        if let Some(feedback) = &mut self.feedback
            && feedback.phase.active()
        {
            feedback.cancel_requested = true;
            feedback.reason = Some(reason.into());
            if self.pending.take().is_some() {
                feedback.phase = Phase::Cancelled;
                feedback.visible_until = Some(Instant::now() + FEEDBACK_DURATION);
                self.epoch = self.epoch.wrapping_add(1);
            }
        }
    }
    fn maintain(&mut self) {
        // Publish one final change at expiry so even a static screen repaints clear.
        if let Some(feedback) = &mut self.feedback
            && feedback
                .visible_until
                .is_some_and(|until| until <= Instant::now())
        {
            feedback.overlay_visible = false;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.until <= Instant::now())
        {
            self.stop("Gesture queue deadline expired", false);
        }
    }
}
#[derive(Default)]
pub struct Controller {
    inner: Mutex<Inner>,
}
impl Controller {
    /// LOCAL UI ONLY. No timer, focus dependency, or per-gesture approval.
    /// Disabling cancels queued/running work and requests worker-owned release.
    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.inner.lock().unwrap();
        if enabled {
            inner.enabled = true;
        } else {
            inner.stop("Allow assistance switched off locally", true);
        }
    }
    pub fn revoke(&self, reason: &str) {
        self.inner.lock().unwrap().stop(reason, true);
    }
    pub fn cancel(&self) {
        self.inner
            .lock()
            .unwrap()
            .stop("Cancelled; release requested (not a rollback)", false);
    }
    pub fn epoch(&self) -> u64 {
        self.inner.lock().unwrap().epoch
    }
    pub fn status(&self) -> Status {
        let mut inner = self.inner.lock().unwrap();
        inner.maintain();
        Status {
            enabled: inner.enabled,
            human_held: inner.local_writing || !inner.human.is_empty(),
            feedback: inner.feedback.clone(),
        }
    }
    pub fn state(&self) -> Value {
        let mut inner = self.inner.lock().unwrap();
        inner.maintain();
        json!({"enabled": inner.enabled,
            "authorization": "The local Allow assistance toggle globally authorizes bounded gestures. No timer or per-gesture approval.",
            "action": inner.feedback, "human_input_held": inner.local_writing || !inner.human.is_empty(),
            "visually_verified": false, "note": "Written means socket write only, not device acknowledgement or visual verification. Never retry uncertain mutations. Physical device takeover cannot be detected."})
    }
    pub fn submit(
        &self,
        screen: &Screen,
        caps: u8,
        size: (u16, u16),
        gesture: Gesture,
        observed_epoch: u64,
        observation_expires_at: Instant,
    ) -> Result<Value, String> {
        let mut plan = Plan::new(gesture, caps, size)?;
        let mut inner = self.inner.lock().unwrap();
        inner.maintain();
        if observation_expires_at <= Instant::now() {
            return Err("Observation expired; capture a fresh screen".into());
        }
        if observed_epoch != inner.epoch {
            return Err(
                "Input changed since this observation; capture a fresh screen after the gesture"
                    .into(),
            );
        }
        if !inner.enabled {
            return Err(
                "Read-only mode: turn on Allow assistance in the viewer's main menu first".into(),
            );
        }
        if inner.local_writing
            || !inner.human.is_empty()
            || inner.feedback.as_ref().is_some_and(|f| f.phase.active())
        {
            return Err(
                "Another gesture/local input owns control; observe again after it ends".into(),
            );
        }
        inner.next_id = inner.next_id.wrapping_add(1);
        plan.feedback.id = inner.next_id;
        let cancel = Arc::new(AtomicBool::new(false));
        inner.cancellation = Some(cancel.clone());
        inner.feedback = Some(plan.feedback.clone());
        inner.pending = Some(Pending {
            screen: Arc::downgrade(screen),
            plan,
            until: (Instant::now() + QUEUE_TIMEOUT).min(observation_expires_at),
            observation_expires_at,
            cancelled: cancel,
        });
        inner.epoch = inner.epoch.wrapping_add(1); // Consumes all pre-proposal observations.
        let queue_remaining = inner
            .pending
            .as_ref()
            .unwrap()
            .until
            .saturating_duration_since(Instant::now());
        Ok(
            json!({"action": inner.feedback, "queued": true, "device_input_sent": false, "queue_expires_in_ms": queue_remaining.as_millis() as u64, "next": "Gesture queued under global authorization. Poll mpc_get_state, then capture a NEW screen after completion. Do not resubmit this gesture."}),
        )
    }
    pub(crate) fn local_input(&self, events: &[InputEvent]) {
        let mut inner = self.inner.lock().unwrap();
        inner.stop(
            "Local viewer input took over; agent release requested",
            false,
        );
        inner.local_writing = true;
        inner.epoch = inner.epoch.wrapping_add(1);
        for event in events {
            match event.kind {
                0 => {
                    inner.human.insert((0, u16::from(event.id)));
                }
                2 => {
                    inner.human.remove(&(0, u16::from(event.id)));
                }
                4 => {
                    inner.human.insert((4, u16::from(event.id)));
                }
                5 => {
                    inner.human.remove(&(4, u16::from(event.id)));
                }
                8 => {
                    inner.human.insert((8, event.x_or_key));
                }
                9 => {
                    inner.human.remove(&(8, event.x_or_key));
                }
                7 => inner.human.clear(),
                _ => {}
            }
        }
    }
    pub(crate) fn local_written(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.local_writing = false;
        inner.epoch = inner.epoch.wrapping_add(1);
    }
    pub(crate) fn disconnected(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.stop("Session disconnected or screen reconfigured", false);
        inner.human.clear();
        inner.local_writing = false;
        if let Some(feedback) = &mut inner.feedback {
            feedback.visible_until = Some(Instant::now());
            feedback.overlay_visible = false;
        }
        inner.epoch = inner.epoch.wrapping_add(1);
    }
    /// Called only by the socket owner. No locks are held across socket writes.
    pub(crate) fn drive<E>(
        self: &Arc<Self>,
        running: &mut Option<Running>,
        screen: &Screen,
        caps: u8,
        mut send: impl FnMut(&[InputEvent]) -> Result<(), E>,
    ) -> Result<(), E> {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.maintain();
            if running.is_none() && inner.pending.is_some() {
                let pending = inner.pending.take().unwrap();
                let valid = pending
                    .screen
                    .upgrade()
                    .is_some_and(|s| Arc::ptr_eq(&s, screen))
                    && caps & pending.plan.caps != 0
                    && inner.enabled
                    && inner.human.is_empty()
                    && !inner.local_writing;
                if valid {
                    *running = Some(Running {
                        controller: self.clone(),
                        id: pending.plan.feedback.id,
                        plan: pending.plan,
                        screen: Arc::downgrade(screen),
                        cancel: pending.cancelled,
                        started: Instant::now(),
                        deadline: (Instant::now() + EXECUTION_TIMEOUT)
                            .min(pending.observation_expires_at),
                        index: 0,
                        attempted: false,
                        finished: false,
                    });
                    inner.feedback.as_mut().unwrap().phase = Phase::Running;
                } else {
                    let feedback = inner.feedback.as_mut().unwrap();
                    feedback.phase = Phase::Rejected;
                    feedback.reason = Some(
                        "Source/capabilities/ownership changed before execution; no input sent"
                            .into(),
                    );
                    feedback.visible_until = Some(Instant::now() + FEEDBACK_DURATION);
                    inner.epoch = inner.epoch.wrapping_add(1);
                }
            }
        }
        let Some(run) = running.as_mut() else {
            return Ok(());
        };
        let cancelled = run.cancel.load(Ordering::Acquire)
            || Instant::now() >= run.deadline
            || caps & run.plan.caps == 0
            || !run
                .screen
                .upgrade()
                .is_some_and(|s| Arc::ptr_eq(&s, screen));
        if cancelled {
            // Never send an old-source release to a replacement framebuffer/session.
            let same = run
                .screen
                .upgrade()
                .is_some_and(|s| Arc::ptr_eq(&s, screen));
            if run.attempted && same {
                send(&[InputEvent::RELEASE_ALL])?;
            }
            run.finish(if run.attempted && !same { Phase::Uncertain } else { Phase::Cancelled }, run.attempted && same, Some("Gesture stopped; release written if the original connection remained usable. No rollback."));
            running.take();
            return Ok(());
        }
        let step = &run.plan.steps[run.index];
        if run.started.elapsed() < step.at {
            return Ok(());
        }
        run.attempted = true; // A failed write may have sent a prefix: never retry it.
        send(&step.events)?;
        {
            let mut inner = self.inner.lock().unwrap();
            let feedback = inner.feedback.as_mut().unwrap();
            feedback.written_steps += 1;
            feedback.position = step.position;
            feedback.held = step.held;
            if let Some(p) = step.position {
                feedback.path.push(p);
            }
        }
        run.index += 1;
        if run.index == run.plan.steps.len() {
            send(&[InputEvent::RELEASE_ALL])?;
            run.finish(Phase::Completed, true, None);
            running.take();
        }
        Ok(())
    }
}
pub(crate) struct Running {
    controller: Arc<Controller>,
    id: u64,
    plan: Plan,
    screen: Weak<RwLock<Framebuffer>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    deadline: Instant,
    index: usize,
    attempted: bool,
    finished: bool,
}
impl Running {
    fn finish(&mut self, phase: Phase, release_written: bool, reason: Option<&str>) {
        let mut inner = self.controller.inner.lock().unwrap();
        if let Some(feedback) = &mut inner.feedback
            && feedback.id == self.id
        {
            feedback.phase = phase;
            feedback.held = false;
            feedback.release_written = release_written;
            if let Some(reason) = reason {
                feedback.reason = Some(reason.into());
            }
            feedback.visible_until = Some(Instant::now() + FEEDBACK_DURATION);
        }
        inner.epoch = inner.epoch.wrapping_add(1); // Captures taken DURING a gesture are not actionable.
        self.finished = true;
    }
}
#[cfg(test)]
mod tests;

impl Drop for Running {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(if self.attempted { Phase::Uncertain } else { Phase::Cancelled }, false, Some("Connection/write failed; input and release may be incomplete. Do NOT retry. Inspect a fresh screen."));
        }
    }
}
