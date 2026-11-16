//! One socket owner serializes writes; a bounded decoder keeps pixel work off its loop.
//! Snapshots replace presentation state; framebuffer deltas are never dropped.
mod decoder;
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

use crate::{
    audio::Audio,
    auth::Credentials,
    control::{Controller, Running},
    framebuffer::{DEFAULT_FRAME_PIXEL_BUDGET, Framebuffer},
    protocol::{
        self, Assembler, AuthResult, DeviceInfo, Hello, InputEvent, Kind, Message, ServerInit,
    },
};

pub use crate::statistics::Statistics;

const POLL: Duration = Duration::from_millis(50);
const MAX_RETRY: Duration = Duration::from_secs(30);
const STABLE_CONNECTION: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
enum SessionError {
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    Authentication(String),
    #[error("{0}")]
    Local(String),
    #[error("Cancelled")]
    Cancelled,
}
impl From<String> for SessionError {
    fn from(message: String) -> Self {
        Self::Network(message)
    }
}
impl From<protocol::Error> for SessionError {
    fn from(error: protocol::Error) -> Self {
        Self::Protocol(error.to_string())
    }
}
impl SessionError {
    fn retryable(&self) -> bool {
        matches!(self, Self::Network(_))
    }
}

fn retry_delay(base: Duration, failures: u32, jitter: u8) -> Duration {
    let exponential = base
        .clamp(POLL, MAX_RETRY)
        .saturating_mul(1 << failures.min(10))
        .min(MAX_RETRY);
    // +/- 25%, capped even after jitter. Randomness failure uses the midpoint.
    exponential
        .mul_f64(0.75 + f64::from(jitter) / 510.0)
        .min(MAX_RETRY)
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub connect: Duration,
    pub handshake: Duration,
    pub ping: Duration,
    pub silence: Duration,
    /// Initial network retry delay; subsequent failures back off with jitter,
    /// capped at 30 seconds. A stable streaming connection resets the backoff.
    pub retry: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            handshake: Duration::from_secs(8),
            ping: Duration::from_secs(2),
            silence: Duration::from_secs(20),
            retry: Duration::from_secs(2),
        }
    }
}
pub struct Config {
    pub host: String,
    pub port: u16,
    password: Zeroizing<String>,
    pub timing: Timing,
    pub audio_enabled: bool,
    pub fps_limit: u8,
    /// Aggregate rectangle pixels attempted per update; excess rectangles are skipped/ACKed.
    /// Zero skips all rectangles. The budget resets for each update and screen configuration.
    pub frame_pixel_budget: u64,
}
impl Config {
    pub fn new(host: &str, port: u16, password: String) -> Result<Self, String> {
        let host = host.trim().to_owned();
        if host.is_empty() {
            return Err("Enter a device IP address or hostname".into());
        }
        if port == 0 {
            return Err("Port must be in 1..=65535".into());
        }
        Ok(Self {
            host,
            port,
            password: Zeroizing::new(password),
            timing: Timing::default(),
            audio_enabled: false,
            fps_limit: 0,
            frame_pixel_budget: DEFAULT_FRAME_PIXEL_BUDGET,
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    Connecting,
    WaitingForHello,
    Authenticating,
    WaitingForScreen,
    Streaming,
    RetryWait,
    /// Permanent authentication/protocol/local failure; reconnect explicitly to retry.
    Failed,
}
#[derive(Clone, Default)]
pub struct Snapshot {
    pub state: State,
    /// Only populated after a valid Hello, never inferred merely from a TCP connection.
    pub protocol_version: Option<u16>,
    pub error: Option<String>,
    pub screen: Option<ServerInit>,
    pub framebuffer: Option<Arc<RwLock<Framebuffer>>>,
    pub device: Option<DeviceInfo>,
    pub statistics: Statistics,
    /// Changes after every frame/configuration, even if the window is not painting.
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub enum Command {
    Refresh,
    FrameRate(u8),
    Audio(bool),
    /// Bound to a particular screen/session; never replay after reconnect/reconfigure.
    Input {
        screen: Arc<RwLock<Framebuffer>>,
        events: Vec<InputEvent>,
    },
}

pub struct Session {
    snapshot: Arc<Mutex<Snapshot>>,
    control: Arc<Controller>,
    audio: Arc<Audio>,
    commands: SyncSender<Command>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Session {
    pub fn start(config: Config) -> io::Result<Self> {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (commands, receiver) = mpsc::sync_channel(128);
        let state = snapshot.clone();
        let audio = Arc::new(Audio::default());
        let sink = audio.clone();
        let control = Arc::new(Controller::default());
        let input_control = control.clone();
        let stop = cancel.clone();
        let worker = thread::Builder::new()
            .name("remote-screen-session".into())
            .spawn(move || {
                let mut worker = Worker {
                    fps: config.fps_limit.min(120),
                    audio_on: config.audio_enabled,
                    statistics_clock: Instant::now(),
                    streaming_since: None,
                    audio: sink,
                    config,
                    state,
                    stop,
                    commands: receiver,
                    control: input_control,
                };
                worker.run();
            })?;
        Ok(Self {
            snapshot,
            control,
            audio,
            commands,
            cancel,
            worker: Some(worker),
        })
    }
    pub fn audio(&self) -> Arc<Audio> {
        self.audio.clone()
    }
    /// Shared input arbitration; only the local global toggle enables assistance.
    pub fn control(&self) -> Arc<Controller> {
        self.control.clone()
    }
    /// Shared latest state for read-only background consumers (recording/teacher).
    pub fn snapshot_source(&self) -> Arc<Mutex<Snapshot>> {
        self.snapshot.clone()
    }
    /// Shared latest state for the background recording capture timer.
    pub fn recording_source(&self) -> Arc<Mutex<Snapshot>> {
        self.snapshot_source()
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }
    pub fn command(&self, command: Command) -> Result<(), mpsc::TrySendError<Command>> {
        self.commands.try_send(command)
    }
    /// Signal cancellation immediately; join/drop may then run off the UI thread.
    pub fn request_stop(&self) {
        self.control.revoke("Session stopped locally");
        self.cancel.store(true, Ordering::Release);
        self.audio.set_enabled(false);
    }
    pub fn stop(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // A terminal worker has already exited, so it cannot publish cancellation.
        let mut snapshot = self.snapshot.lock().unwrap();
        if snapshot.state == State::Failed {
            snapshot.state = State::Idle;
            snapshot.revision = snapshot.revision.wrapping_add(1);
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    config: Config,
    state: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    commands: Receiver<Command>,
    fps: u8,
    audio: Arc<Audio>,
    audio_on: bool,
    statistics_clock: Instant,
    streaming_since: Option<Instant>,
    control: Arc<Controller>,
}
impl Worker {
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    fn publish(&self, f: impl FnOnce(&mut Snapshot)) {
        if !self.cancelled() {
            f(&mut self.state.lock().unwrap());
        }
    }
    fn phase(&self, phase: State) {
        self.publish(|s| s.state = phase);
    }
    fn run(&mut self) {
        let mut failures = 0u32;
        while !self.cancelled() {
            self.statistics_clock = Instant::now();
            self.streaming_since = None;
            self.publish(|s| {
                let revision = s.revision.wrapping_add(1);
                *s = Snapshot {
                    state: State::Connecting,
                    revision,
                    ..Snapshot::default()
                };
            });
            // Commands from the previous connection must not leak into a new login.
            while let Ok(command) = self.commands.try_recv() {
                match command {
                    Command::FrameRate(fps) => self.fps = fps.min(120),
                    Command::Audio(on) => self.audio_on = on,
                    _ => {}
                }
            }
            self.audio.reset();
            let result = self.attempt();
            self.audio.set_enabled(false);
            self.audio.reset();
            if self.cancelled() {
                break;
            }
            let error = result.err();
            let retryable = error.as_ref().is_some_and(SessionError::retryable);
            self.publish(|s| {
                s.state = if retryable {
                    State::RetryWait
                } else {
                    State::Failed
                };
                s.error = error.map(|error| error.to_string());
                s.statistics.disconnected();
                s.screen = None;
                s.framebuffer = None;
                s.device = None;
                s.revision = s.revision.wrapping_add(1);
            });
            if !retryable {
                return; // Keep the terminal error visible until an explicit reconnect.
            }
            if self
                .streaming_since
                .is_some_and(|at| at.elapsed() >= STABLE_CONNECTION)
            {
                failures = 0;
            }
            let mut jitter = [128];
            if getrandom::fill(&mut jitter).is_err() {
                jitter[0] = 128;
            }
            let delay = retry_delay(self.config.timing.retry, failures, jitter[0]);
            failures = failures.saturating_add(1);
            let until = Instant::now() + delay;
            while !self.cancelled() && Instant::now() < until {
                thread::sleep(POLL.min(until.saturating_duration_since(Instant::now())));
            }
        }
        let mut snapshot = self.state.lock().unwrap();
        snapshot.state = State::Idle;
        snapshot.statistics.disconnected();
        snapshot.screen = None;
        snapshot.framebuffer = None;
        snapshot.device = None;
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
    fn attempt(&mut self) -> Result<(), SessionError> {
        self.control.disconnected();
        let addresses = resolve(
            &self.config.host,
            self.config.port,
            &self.stop,
            self.config.timing.connect,
        )?;
        let stream = connect(&addresses, &self.stop, self.config.timing.connect)?;
        let mut wire = Wire {
            stream,
            assembler: Assembler::default(),
            pending: VecDeque::new(),
        };
        self.phase(State::WaitingForHello);
        let hello = Hello::parse(&self.expect(&mut wire, Kind::Hello)?.payload)?;
        self.publish(|s| s.protocol_version = Some(2)); // Hello parser requires v2.
        self.phase(State::Authenticating);
        let credentials = Credentials::derive(&self.config.password);
        if self.cancelled() {
            return Err(SessionError::Cancelled);
        }
        let auth = credentials
            .fresh_auth(&hello.nonce)
            .map_err(|e| SessionError::Local(format!("Secure randomness unavailable: {e}")))?;
        wire.send(auth.message())?;
        let result = self.expect(&mut wire, Kind::AuthResult)?;
        match AuthResult::parse(&result.payload)? {
            AuthResult::Rejected(reason) => {
                return Err(SessionError::Authentication(format!(
                    "Login refused: {reason}. Check the device password and reconnect."
                )));
            }
            AuthResult::Success(proof) => {
                if hello.flags & 1 != 0
                    && !credentials.verify_server(&hello.nonce, &auth.nonce, &proof)
                {
                    return Err(SessionError::Authentication(
                        "The server could not prove it knows the password - not connecting".into(),
                    ));
                }
            }
        }
        self.phase(State::WaitingForScreen);
        let init = ServerInit::parse(&self.expect(&mut wire, Kind::ServerInit)?.payload)
            .map_err(|e| SessionError::Protocol(format!("Invalid screen description: {e}")))?;
        let mut audio_available = init.audio_available;
        let mut image = self.configure(init)?;
        let mut decoder = decoder::Decoder::start()
            .map_err(|e| SessionError::Local(format!("Could not start frame decoder: {e}")))?;
        wire.send(protocol::options(self.fps))?;
        self.audio.set_enabled(self.audio_on && audio_available);
        if self.audio_on && audio_available {
            wire.send(protocol::audio_control(true))?;
        }
        self.phase(State::Streaming);
        self.streaming_since = Some(Instant::now());
        let clock = Instant::now();
        let mut last_message = clock;
        let mut last_ping = clock;
        let mut ping_token = None;
        let mut running = None;
        let mut input_caps = self
            .state
            .lock()
            .unwrap()
            .screen
            .as_ref()
            .map_or(0, |s| s.input_capabilities);
        let result = (|| {
            while !self.cancelled() {
                self.service_commands(
                    &mut wire,
                    &image,
                    input_caps,
                    audio_available,
                    &mut running,
                )?;
                if last_ping.elapsed() >= self.config.timing.ping {
                    let token = clock.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                    wire.send(protocol::token(Kind::Ping, token))?;
                    ping_token = Some((token, Instant::now()));
                    last_ping = Instant::now();
                }
                if last_message.elapsed() >= self.config.timing.silence {
                    return Err(SessionError::Network(
                        "Connection lost: no messages for the liveness deadline; reconnecting"
                            .into(),
                    ));
                }
                let Some(message) = self.poll(&mut wire)? else {
                    continue;
                };
                last_message = Instant::now();
                match message.kind {
                    Kind::ServerInit => {
                        self.control.cancel();
                        self.control
                            .drive(&mut running, &image, input_caps, |events| {
                                send_input(&mut wire, events)
                            })?;
                        self.control.disconnected();
                        // Reconfiguration invalidates every held input and queued old-screen command.
                        for message in InputEvent::messages(&[InputEvent::RELEASE_ALL]).unwrap() {
                            wire.send(message)?;
                        }
                        let init = ServerInit::parse(&message.payload).map_err(|e| {
                            SessionError::Protocol(format!("Invalid screen description: {e}"))
                        })?;
                        self.audio.set_enabled(false);
                        self.audio.reset();
                        audio_available = init.audio_available;
                        input_caps = init.input_capabilities;
                        image = self.configure(init)?;
                        self.audio.set_enabled(self.audio_on && audio_available);
                        if self.audio_on && audio_available {
                            wire.send(protocol::audio_control(true))?;
                        }
                    }
                    Kind::FrameUpdate => {
                        decoder.submit(image.clone(), message.payload)?;
                        let report = loop {
                            // Neither a framebuffer reader nor a single slow codec/conversion
                            // may delay local takeover, assistance Stop, or held-input release.
                            self.service_commands(
                                &mut wire,
                                &image,
                                input_caps,
                                audio_available,
                                &mut running,
                            )?;
                            if let Some(report) = decoder.receive(POLL)? {
                                break report;
                            }
                        };
                        // One complete delta at a time; ACK only after successful application.
                        wire.send(protocol::ack(report.id))?;
                        self.publish(|s| {
                            s.revision = s.revision.wrapping_add(1);
                            s.statistics.processed_frame(report.quality, report.skipped);
                        });
                    }
                    Kind::Ping => wire.send(protocol::token(
                        Kind::Pong,
                        protocol::parse_token(&message.payload)?,
                    ))?,
                    Kind::Pong => {
                        if let Ok(token) = protocol::parse_token(&message.payload)
                            && let Some((sent, at)) = ping_token
                            && sent == token
                        {
                            self.publish(|s| s.statistics.rtt = Some(at.elapsed()));
                            ping_token = None;
                        }
                    }
                    Kind::Bye => {
                        return Err(SessionError::Network(format!(
                            "Server closed the connection: {}",
                            protocol::parse_reason(&message.payload)?
                        )));
                    }
                    Kind::DeviceInfo => {
                        if let Ok(info) = DeviceInfo::parse(&message.payload) {
                            self.publish(|s| s.device = Some(info));
                        }
                    }
                    Kind::AudioFormat if self.audio_on && audio_available => {
                        match protocol::AudioFormat::parse(&message.payload) {
                            Ok(format) => {
                                if let Err(error) = self.audio.configure(format) {
                                    self.audio.invalidate(format!(
                                        "Invalid device audio format: {error}"
                                    ));
                                }
                            }
                            Err(error) => self
                                .audio
                                .invalidate(format!("Invalid device audio format: {error}")),
                        }
                    }
                    Kind::AudioPacket if self.audio_on && audio_available => {
                        // Malformed optional audio must not tear down the screen stream.
                        let _ = self.audio.packet(&message.payload);
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        decoder.request_stop();
        drop(running); // Interrupted writes remain explicitly uncertain, never retried.
        self.control.disconnected();
        self.audio.set_enabled(false);
        if self.audio_on && audio_available && self.cancelled() {
            let _ = wire.send(protocol::audio_control(false));
        }
        let input_capabilities = self
            .state
            .lock()
            .unwrap()
            .screen
            .as_ref()
            .map_or(0, |screen| screen.input_capabilities);
        if input_capabilities != 0 {
            // Best effort on every exit, not an input acknowledgement or a retry.
            let _ = wire.send(
                InputEvent::messages(&[InputEvent::RELEASE_ALL])
                    .unwrap()
                    .remove(0),
            );
        }
        // Release on the socket BEFORE joining even a non-interruptible codec call.
        drop(decoder);
        result
    }
    /// Shared by the normal streaming loop and every wait for pixel decoding.
    fn service_commands(
        &mut self,
        wire: &mut Wire,
        image: &Arc<RwLock<Framebuffer>>,
        caps: u8,
        audio_available: bool,
        running: &mut Option<Running>,
    ) -> Result<(), SessionError> {
        // Bound work so a busy producer cannot starve incoming ACKs/liveness.
        for _ in 0..128 {
            if self.cancelled() {
                return Err(SessionError::Cancelled);
            }
            let command = self.commands.try_recv().ok();
            let events: Vec<_> = match &command {
                Some(Command::Input { screen, events }) if Arc::ptr_eq(screen, image) => events
                    .iter()
                    .copied()
                    .filter(|e| match e.kind {
                        0..=2 => caps & 1 != 0,
                        3..=6 => caps & 2 != 0,
                        7 => caps != 0,
                        8..=9 => caps & 4 != 0,
                        _ => false,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            // Admit human takeover BEFORE advancing an agent step, including queued work.
            if !events.is_empty() {
                self.control.local_input(&events);
            }
            self.control
                .drive(running, image, caps, |events| send_input(wire, events))?;
            match command {
                Some(Command::Refresh) => wire.send(Message::new(Kind::RequestFullRefresh, []))?,
                Some(Command::FrameRate(fps)) => {
                    self.fps = fps.min(120);
                    wire.send(protocol::options(self.fps))?;
                }
                Some(Command::Audio(on)) => {
                    self.audio_on = on;
                    self.audio.set_enabled(on && audio_available);
                    self.audio.reset();
                    if audio_available {
                        wire.send(protocol::audio_control(on))?;
                    }
                }
                Some(Command::Input { .. }) => {
                    if !events.is_empty() {
                        send_input(wire, &events)?;
                        self.control.local_written();
                    }
                }
                None => break,
            }
        }
        Ok(())
    }
    fn configure(&self, init: ServerInit) -> Result<Arc<RwLock<Framebuffer>>, SessionError> {
        let mut framebuffer = Framebuffer::new(init.clone())?;
        framebuffer.set_frame_pixel_budget(self.config.frame_pixel_budget);
        let image = Arc::new(RwLock::new(framebuffer));
        self.publish(|s| {
            s.screen = Some(init);
            s.framebuffer = Some(image.clone());
            s.revision = s.revision.wrapping_add(1);
        });
        Ok(image)
    }
    fn poll(&self, wire: &mut Wire) -> Result<Option<Message>, SessionError> {
        let message = wire.poll()?;
        self.publish(|s| {
            if let Some(message) = &message {
                s.statistics.received_message(message.payload.len());
            }
            // Read timeouts must also sample: a static screen should settle to 0 FPS.
            s.statistics.sample(self.statistics_clock.elapsed());
        });
        Ok(message)
    }
    fn expect(&self, wire: &mut Wire, expected: Kind) -> Result<Message, SessionError> {
        let deadline = Instant::now() + self.config.timing.handshake;
        while !self.cancelled() {
            if Instant::now() >= deadline {
                return Err(SessionError::Network(format!(
                    "Timed out waiting for {expected:?}"
                )));
            }
            if let Some(message) = self.poll(wire)? {
                if message.kind == Kind::Bye {
                    return Err(SessionError::Network(format!(
                        "Server refused connection: {}",
                        protocol::parse_reason(&message.payload)?
                    )));
                }
                if message.kind != expected {
                    return Err(SessionError::Protocol(format!(
                        "Expected {expected:?}, received {:?}; wrong service?",
                        message.kind
                    )));
                }
                return Ok(message);
            }
        }
        Err(SessionError::Cancelled)
    }
}
fn send_input(wire: &mut Wire, events: &[InputEvent]) -> Result<(), SessionError> {
    for message in InputEvent::messages(events)? {
        wire.send(message)?;
    }
    Ok(())
}
struct Wire {
    stream: TcpStream,
    assembler: Assembler,
    pending: VecDeque<Message>,
}
impl Wire {
    fn send(&mut self, message: Message) -> Result<(), SessionError> {
        self.stream
            .write_all(&message.encode()?)
            .map_err(|e| SessionError::Network(network_error("Sending message", e)))
    }
    fn poll(&mut self) -> Result<Option<Message>, SessionError> {
        if let Some(message) = self.pending.pop_front() {
            return Ok(Some(message));
        }
        let mut bytes = [0; 65536];
        match self.stream.read(&mut bytes) {
            Ok(0) => {
                return Err(SessionError::Network(
                    "Connection closed by device; reconnecting".into(),
                ));
            }
            Ok(n) => self.pending.extend(self.assembler.feed(&bytes[..n])?),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(SessionError::Network(network_error("Reading message", e))),
        }
        Ok(self.pending.pop_front())
    }
}

fn network_error(operation: &str, error: io::Error) -> String {
    let hint = match error.kind() {
        io::ErrorKind::ConnectionRefused => {
            "Device reachable, but the service/port is not listening."
        }
        io::ErrorKind::TimedOut => "Check the address, firewall and Wi-Fi client isolation.",
        io::ErrorKind::PermissionDenied => {
            "Check macOS System Settings > Privacy & Security > Local Network."
        }
        io::ErrorKind::NetworkUnreachable | io::ErrorKind::HostUnreachable => {
            "Check that the device is on the same reachable network."
        }
        _ => "Check the device address and network connection.",
    };
    format!("{operation}: {error}. {hint}")
}

// The system resolver has no portable cancellation. A single bounded service thread
// prevents abandoned DNS lookups from spawning unbounded threads. Session workers
// wait interruptibly and numeric addresses bypass the resolver entirely.
struct Lookup {
    host: String,
    port: u16,
    cancel: Arc<AtomicBool>,
    reply: SyncSender<io::Result<Vec<SocketAddr>>>,
}
fn resolver() -> &'static SyncSender<Lookup> {
    static RESOLVER: OnceLock<SyncSender<Lookup>> = OnceLock::new();
    RESOLVER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Lookup>(1);
        thread::Builder::new()
            .name("remote-screen-dns".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if !job.cancel.load(Ordering::Acquire) {
                        let result = (job.host.as_str(), job.port)
                            .to_socket_addrs()
                            .map(|iter| iter.collect());
                        let _ = job.reply.try_send(result);
                    }
                }
            })
            .expect("create system resolver worker");
        tx
    })
}
fn resolve(
    host: &str,
    port: u16,
    cancel: &Arc<AtomicBool>,
    timeout: Duration,
) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let mut job = Lookup {
        host: host.into(),
        port,
        cancel: cancel.clone(),
        reply: tx,
    };
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("DNS lookup timed out. Check spelling or use a numeric IP address.".into());
        }
        match resolver().try_send(job) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(returned)) => {
                job = returned;
                thread::sleep(POLL);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err("System DNS worker unavailable".into());
            }
        }
    }
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("DNS lookup timed out. Check spelling or use a numeric IP address.".into());
        }
        match rx.recv_timeout(POLL) {
            Ok(Ok(addresses)) if addresses.is_empty() => {
                return Err(
                    "DNS returned no addresses; check spelling or use an IP address".into(),
                );
            }
            Ok(Ok(addresses)) => return Ok(addresses),
            Ok(Err(e)) => {
                return Err(format!(
                    "DNS lookup failed: {e}. Check spelling or use an IP address."
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("System DNS lookup cancelled".into()),
        }
    }
}
fn connect(
    addresses: &[SocketAddr],
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<TcpStream, String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = io::Error::new(io::ErrorKind::NotFound, "No device addresses");
    for (index, address) in addresses.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        let socket = Socket::new(
            Domain::for_address(*address),
            Type::STREAM,
            Some(Protocol::TCP),
        )
        .map_err(|e| network_error("Creating socket", e))?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        // Divide the remaining deadline so an unreachable IPv6 address cannot consume
        // the entire budget before trying a reachable IPv4 address (or vice versa).
        let budget =
            deadline.saturating_duration_since(Instant::now()) / (addresses.len() - index) as u32;
        let address_deadline = Instant::now() + budget;
        match socket.connect(&(*address).into()) {
            Ok(()) => {}
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc::EINPROGRESS) => {}
            Err(e) => {
                last_error = e;
                continue;
            }
        }
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("Cancelled".into());
            }
            if let Some(error) = socket.take_error().map_err(|e| e.to_string())? {
                last_error = error;
                break;
            }
            if socket.peer_addr().is_ok() {
                socket.set_nonblocking(false).map_err(|e| e.to_string())?;
                let stream: TcpStream = socket.into();
                stream
                    .set_read_timeout(Some(POLL))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .map_err(|e| e.to_string())?;
                stream.set_nodelay(true).map_err(|e| e.to_string())?;
                return Ok(stream);
            }
            if Instant::now() >= address_deadline {
                last_error = io::Error::new(io::ErrorKind::TimedOut, "TCP connection timed out");
                break;
            }
            thread::sleep(POLL.min(address_deadline.saturating_duration_since(Instant::now())));
        }
    }
    Err(network_error("Connecting", last_error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_human_input_cancels_agent_before_its_first_socket_write() {
        use crate::{
            control::{Gesture, Phase, Point},
            input,
            protocol::PixelFormat,
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut wire = Wire {
            stream,
            assembler: Assembler::default(),
            pending: VecDeque::new(),
        };
        let mut reader = Wire {
            stream: peer,
            assembler: Assembler::default(),
            pending: VecDeque::new(),
        };
        let image = Arc::new(RwLock::new(
            Framebuffer::new(ServerInit {
                width: 2,
                height: 2,
                rotation: 0,
                pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
                name: "synthetic".into(),
                input_capabilities: 7,
                audio_available: false,
            })
            .unwrap(),
        ));
        let control = Arc::new(Controller::default());
        control.set_enabled(true);
        control
            .submit(
                &image,
                7,
                (2, 2),
                Gesture::Click {
                    target: Point { x: 1, y: 1 },
                    hold_ms: 100,
                },
                control.epoch(),
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
        let keys = input::Control::default().key("a", [false; 4]);
        let (send, commands) = mpsc::sync_channel(128);
        send.send(Command::Input {
            screen: image.clone(),
            events: keys.clone(),
        })
        .unwrap();
        let mut worker = Worker {
            config: Config::new(
                "127.0.0.1",
                listener.local_addr().unwrap().port(),
                String::new(),
            )
            .unwrap(),
            state: Arc::new(Mutex::new(Snapshot::default())),
            stop: Arc::new(AtomicBool::new(false)),
            commands,
            fps: 0,
            audio: Arc::new(Audio::default()),
            audio_on: false,
            statistics_clock: Instant::now(),
            streaming_since: None,
            control: control.clone(),
        };
        worker
            .service_commands(&mut wire, &image, 7, false, &mut None)
            .unwrap();
        let message = reader.poll().unwrap().unwrap();
        assert_eq!(message.kind, Kind::Input);
        assert_eq!(InputEvent::parse_batch(&message.payload).unwrap(), keys);
        assert!(reader.poll().unwrap().is_none()); // No agent down or release was written.
        let status = control.status();
        assert!(status.enabled);
        assert!(!status.human_held);
        let feedback = status.feedback.unwrap();
        assert_eq!(feedback.phase, Phase::Cancelled);
        assert_eq!(feedback.written_steps, 0);
    }

    #[test]
    fn network_backoff_is_exponential_jittered_capped_and_overflow_safe() {
        let base = Duration::from_secs(2);
        assert_eq!(retry_delay(base, 0, 0), Duration::from_millis(1500));
        assert_eq!(retry_delay(base, 0, 255), Duration::from_millis(2500));
        assert_eq!(retry_delay(base, 1, 0), Duration::from_secs(3));
        assert_eq!(retry_delay(base, 2, 255), Duration::from_secs(10));
        assert!(retry_delay(Duration::ZERO, 0, 0) > Duration::ZERO);
        for failures in [0, 1, 10, u32::MAX] {
            for jitter in [0, 128, 255] {
                assert!(retry_delay(Duration::MAX, failures, jitter) <= MAX_RETRY);
            }
        }
        assert!(SessionError::Network("EOF".into()).retryable());
        assert!(!SessionError::Authentication("refused".into()).retryable());
        assert!(!SessionError::from(protocol::Error::Version(3)).retryable());
        assert!(!SessionError::Local("randomness unavailable".into()).retryable());
        assert!(!SessionError::Cancelled.retryable());
    }
}
