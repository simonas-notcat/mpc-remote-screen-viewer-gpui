use hmac::{Hmac, Mac};
use mpc_remote_screen_viewer_gpui::{auth::Credentials, protocol::*, session::*};
use sha2::Sha256;
#[cfg(unix)]
#[path = "support/assisted.rs"]
mod assisted;
use std::{
    io::{Read, Write},
    net::{TcpListener as SystemListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

/// Keep unrelated local HTTP/service-discovery probes out of the mock protocol.
/// A real viewer sends nothing until Hello. The short peek only delays accepting
/// silent clients; the client's real handshake deadlines remain in effect.
struct TcpListener(SystemListener);
impl TcpListener {
    fn bind(address: &str) -> std::io::Result<Self> {
        SystemListener::bind(address).map(Self)
    }
    fn accept(&self) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
        loop {
            let (socket, peer) = self.0.accept()?;
            socket.set_nonblocking(false)?;
            socket.set_read_timeout(Some(Duration::from_millis(100)))?;
            let mut first = [0];
            match socket.peek(&mut first) {
                // Eager data (e.g. GET /) or a closed connection is not a viewer.
                Ok(_) => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    socket.set_read_timeout(None)?;
                    return Ok((socket, peer));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }
}
impl std::ops::Deref for TcpListener {
    type Target = SystemListener;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn fixture(name: &str) -> Message {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/protocol-v2-vectors.json")).unwrap();
    let v = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap();
    let bytes = v["payload_hex"]
        .as_str()
        .unwrap()
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    Message::new(
        Kind::try_from(v["type"].as_u64().unwrap() as u8).unwrap(),
        bytes,
    )
}
fn receive(socket: &mut TcpStream) -> Message {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut header = [0; 5];
    socket.read_exact(&mut header).unwrap();
    let n = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    assert!(
        n <= MAX_PAYLOAD,
        "Invalid header {header:02x?} from {:?} to {:?}",
        socket.peer_addr(),
        socket.local_addr()
    );
    let mut payload = vec![0; n];
    socket.read_exact(&mut payload).unwrap();
    Message::new(Kind::try_from(header[0]).unwrap(), payload)
}
fn send(socket: &mut TcpStream, message: Message) {
    socket.write_all(&message.encode().unwrap()).unwrap();
}
fn authenticate(socket: &mut TcpStream, password: &str, verify: bool) -> Auth {
    let mut hello = fixture("hello_password");
    if !verify {
        hello.payload[6] = 0;
    }
    for byte in hello.encode().unwrap() {
        socket.write_all(&[byte]).unwrap();
    }
    let message = receive(socket);
    assert_eq!(message.kind, Kind::Auth);
    assert_eq!(message.payload.len(), 50);
    let auth = Auth::parse(&message.payload).unwrap();
    let hello = Hello::parse(&hello.payload).unwrap();
    assert_eq!(
        Credentials::derive(password)
            .client_auth(&hello.nonce, auth.nonce)
            .message(),
        message
    );
    auth
}
fn success(socket: &mut TcpStream, password: &str, auth: &Auth) {
    let mut key = [0; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), b"remote-screen/v1", 10000, &mut key);
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(b"RSCR-server");
    mac.update(
        &Hello::parse(&fixture("hello_password").payload)
            .unwrap()
            .nonce,
    );
    mac.update(&auth.nonce);
    let mut result = vec![1];
    result.extend_from_slice(&mac.finalize().into_bytes());
    send(socket, Message::new(Kind::AuthResult, result));
}
// Each server test retains a cloned listener (`_reservation`) on the test thread.
// A server exiting must not release its port while its client could still retry
// into a different parallel test that subsequently binds the same ephemeral port.
fn start(listener: &TcpListener, password: &str) -> Session {
    start_with_audio(listener, password, false)
}
fn start_with_audio(listener: &TcpListener, password: &str, audio: bool) -> Session {
    let mut config = Config::new(
        " 127.0.0.1 ",
        listener.local_addr().unwrap().port(),
        password.into(),
    )
    .unwrap();
    config.audio_enabled = audio;
    config.timing = Timing {
        connect: Duration::from_millis(500),
        handshake: Duration::from_secs(5),
        ping: Duration::from_secs(1),
        silence: Duration::from_secs(10),
        retry: Duration::from_millis(200),
    };
    Session::start(config).unwrap()
}
fn wait(session: &Session, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let state = session.snapshot();
        if predicate(&state) {
            return state;
        }
        assert!(
            Instant::now() < until,
            "last state {:?}, error {:?}",
            state.state,
            state.error
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn non_ping(socket: &mut TcpStream) -> Message {
    loop {
        let message = receive(socket);
        if message.kind != Kind::Ping {
            return message;
        }
        send(
            socket,
            token(Kind::Pong, parse_token(&message.payload).unwrap()),
        );
    }
}
fn audio_init(available: bool) -> Message {
    let mut init = fixture("server_init_2x2");
    let n = init.payload.len();
    init.payload[n - 2] = 0; // no input; stop has no release-all
    init.payload[n - 1] = u8::from(available);
    init
}
fn pcm(first: u64, frames: u16) -> Message {
    let mut bytes = first.to_le_bytes().to_vec();
    bytes.extend(frames.to_le_bytes());
    for _ in 0..frames {
        bytes.extend(16384i16.to_le_bytes());
    }
    Message::new(Kind::AudioPacket, bytes)
}
#[test]
fn saved_frame_rate_is_in_the_first_options_packet_and_survives_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut config = Config::new(
        "127.0.0.1",
        listener.local_addr().unwrap().port(),
        "".into(),
    )
    .unwrap();
    config.fps_limit = 15;
    config.timing.retry = Duration::from_millis(50);
    let mut client = Session::start(config).unwrap();
    let (notice, notices) = std::sync::mpsc::channel();
    let (advance, advances) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            let auth = authenticate(&mut socket, "", true);
            success(&mut socket, "", &auth);
            send(&mut socket, audio_init(false));
            assert_eq!(
                non_ping(&mut socket),
                options(if attempt == 0 { 15 } else { 5 })
            );
            notice.send(()).unwrap();
            if attempt == 0 {
                assert_eq!(non_ping(&mut socket), options(5));
            } else {
                advances.recv_timeout(Duration::from_secs(5)).unwrap();
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 0);
            }
        }
    });
    notices.recv_timeout(Duration::from_secs(5)).unwrap();
    client.command(Command::FrameRate(5)).unwrap();
    notices.recv_timeout(Duration::from_secs(5)).unwrap();
    client.stop();
    advance.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn audio_negotiation_corruption_toggle_and_capability_reconfiguration() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start_with_audio(&listener, "", true);
    let sink = client.audio();
    let (notice, notices) = std::sync::mpsc::channel();
    let (advance, advances) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        send(&mut socket, audio_init(true));
        assert_eq!(non_ping(&mut socket), options(0));
        assert_eq!(non_ping(&mut socket), audio_control(true));
        send(&mut socket, pcm(0, 640)); // cannot decode without a format
        send(&mut socket, Message::new(Kind::AudioFormat, [0; 6]));
        notice.send(()).unwrap();
        advances.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut format = 8000u32.to_le_bytes().to_vec();
        format.extend([1, 0]);
        send(&mut socket, Message::new(Kind::AudioFormat, format.clone()));
        send(&mut socket, pcm(0, 640));
        send(&mut socket, Message::new(Kind::AudioPacket, [0; 5]));
        // Screen ACKs continue even when optional audio is corrupt.
        send(&mut socket, fixture("frame_raw_and_solid"));
        assert_eq!(non_ping(&mut socket), ack(42));
        assert_eq!(non_ping(&mut socket), audio_control(false));
        send(&mut socket, Message::new(Kind::AudioFormat, format.clone()));
        send(&mut socket, pcm(0, 640)); // unsolicited after disabling is ignored
        send(&mut socket, fixture("frame_raw_and_solid"));
        assert_eq!(non_ping(&mut socket), ack(42));
        assert_eq!(non_ping(&mut socket), audio_control(true));
        send(&mut socket, Message::new(Kind::AudioFormat, format));
        send(&mut socket, pcm(0, 640));
        notice.send(()).unwrap();
        advances.recv_timeout(Duration::from_secs(3)).unwrap();
        send(&mut socket, audio_init(false));
        send(&mut socket, fixture("frame_raw_and_solid"));
        let release = non_ping(&mut socket);
        assert_eq!(release.kind, Kind::Input);
        assert_eq!(
            InputEvent::parse_batch(&release.payload).unwrap(),
            [InputEvent::RELEASE_ALL]
        );
        assert_eq!(non_ping(&mut socket), ack(42));
        // No AudioControl is sent to a server that no longer offers audio.
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    notices.recv_timeout(Duration::from_secs(3)).unwrap();
    wait(&client, |_| sink.status().error.is_some());
    assert!(sink.status().format.is_none());
    advance.send(()).unwrap();
    wait(&client, |s| s.statistics.frames == 1);
    assert_eq!(sink.status().format.unwrap().sample_rate, 8000);
    assert_eq!(sink.status().stats.buffered_ms, 80.0);
    assert!(sink.status().error.is_some());
    client.command(Command::Audio(false)).unwrap();
    wait(&client, |s| s.statistics.frames == 2);
    assert!(sink.status().format.is_none());
    client.command(Command::Audio(true)).unwrap();
    notices.recv_timeout(Duration::from_secs(3)).unwrap();
    wait(&client, |_| sink.status().format.is_some());
    advance.send(()).unwrap();
    wait(&client, |s| s.statistics.frames == 3);
    assert!(!client.snapshot().screen.unwrap().audio_available);
    assert!(sink.status().format.is_none());
    client.stop();
    server.join().unwrap();
}
#[test]
fn unavailable_audio_never_requests_or_decodes_and_still_acks() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start_with_audio(&listener, "", true);
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        send(&mut socket, audio_init(false));
        assert_eq!(non_ping(&mut socket), options(0));
        send(&mut socket, fixture("audio_format_pcm16"));
        send(&mut socket, fixture("audio_pcm16"));
        send(&mut socket, fixture("frame_raw_and_solid"));
        assert_eq!(non_ping(&mut socket), ack(42));
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    wait(&client, |s| s.statistics.frames == 1);
    assert!(client.audio().status().format.is_none());
    client.stop();
    server.join().unwrap();
}
#[test]
fn audio_opt_in_survives_reconnect_but_format_samples_and_indices_do_not() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start_with_audio(&listener, "", true);
    let sink = client.audio();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        send(&mut socket, audio_init(true));
        assert_eq!(non_ping(&mut socket), options(0));
        assert_eq!(non_ping(&mut socket), audio_control(true));
        let mut format = 8000u32.to_le_bytes().to_vec();
        format.extend([1, 0]);
        send(&mut socket, Message::new(Kind::AudioFormat, format));
        send(&mut socket, pcm(90000, 640));
        assert_eq!(non_ping(&mut socket).kind, Kind::RequestFullRefresh);
        drop(socket);
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        send(&mut socket, audio_init(true));
        assert_eq!(non_ping(&mut socket), options(0));
        assert_eq!(non_ping(&mut socket), audio_control(true));
        assert_eq!(non_ping(&mut socket), audio_control(false)); // stop
    });
    let old = wait(&client, |_| sink.status().format.is_some())
        .framebuffer
        .unwrap();
    client.command(Command::Refresh).unwrap();
    wait(&client, |s| {
        s.state == State::Streaming
            && s.framebuffer
                .as_ref()
                .is_some_and(|f| !std::sync::Arc::ptr_eq(f, &old))
    });
    assert!(sink.status().format.is_none());
    let mut output = [f32::NAN; 128];
    sink.render(64, 2, 48000, |i, v| output[i] = v);
    assert!(output.iter().all(|v| *v == 0.0));
    client.stop();
    server.join().unwrap();
}
#[test]
fn input_capabilities_and_stale_screen_commands() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start(&listener, "");
    let (processed, received) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        let mut init = fixture("server_init_2x2");
        let n = init.payload.len();
        init.payload[n - 2] = 4; // keyboard only
        send(&mut socket, init);
        assert_eq!(receive(&mut socket), options(0));
        loop {
            let message = receive(&mut socket);
            match message.kind {
                Kind::Ping => send(
                    &mut socket,
                    token(Kind::Pong, parse_token(&message.payload).unwrap()),
                ),
                Kind::Input => {
                    let events = InputEvent::parse_batch(&message.payload).unwrap();
                    if events == [InputEvent::RELEASE_ALL] {
                        break;
                    }
                    assert_eq!(events.len(), 2);
                    assert_eq!(events[0].kind, 8);
                    assert_eq!(events[0].x_or_key, 30);
                    assert_eq!(events[1].kind, 9);
                    processed.send(()).unwrap();
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    });
    let snapshot = wait(&client, |s| s.state == State::Streaming);
    let screen = snapshot.framebuffer.unwrap();
    let stale = std::sync::Arc::new(std::sync::RwLock::new(
        mpc_remote_screen_viewer_gpui::framebuffer::Framebuffer::new(snapshot.screen.unwrap())
            .unwrap(),
    ));
    client
        .command(Command::Input {
            screen: stale,
            events: vec![InputEvent::RELEASE_ALL],
        })
        .unwrap();
    let mut events = mpc_remote_screen_viewer_gpui::input::Control::default().key("a", [false; 4]);
    events.insert(
        0,
        mpc_remote_screen_viewer_gpui::input::pointer(4, 0, (0, 0), 0),
    );
    client.command(Command::Input { screen, events }).unwrap();
    // Stale release-all must not have reached the server before the valid key pair.
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    client.stop();
    server.join().unwrap();
}

#[test]
fn actual_tcp_fragmented_auth_coalesced_frames_ack_ping_and_stop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start(&listener, "example-password");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "example-password", true);
        success(&mut socket, "example-password", &auth);
        let init = fixture("server_init_2x2");
        let frame = fixture("frame_raw_and_solid");
        let mut coalesced = init.encode().unwrap();
        coalesced.extend(frame.encode().unwrap());
        coalesced.extend(fixture("device_info").encode().unwrap());
        coalesced.extend(Message::new(Kind::DeviceInfo, [255]).encode().unwrap()); // Optional bad metadata is ignored.
        coalesced.extend(token(Kind::Ping, 987).encode().unwrap());
        socket.write_all(&coalesced).unwrap();
        assert_eq!(receive(&mut socket), options(0));
        assert_eq!(receive(&mut socket), ack(42));
        assert_eq!(receive(&mut socket), token(Kind::Pong, 987));
        let mut refresh = false;
        let mut changed_fps = false;
        loop {
            let message = receive(&mut socket);
            match message.kind {
                Kind::Ping => send(
                    &mut socket,
                    token(Kind::Pong, parse_token(&message.payload).unwrap()),
                ),
                Kind::RequestFullRefresh => refresh = true,
                Kind::SetOptions => {
                    assert_eq!(message, options(15));
                    changed_fps = true;
                }
                Kind::Input => {
                    assert_eq!(
                        InputEvent::parse_batch(&message.payload).unwrap(),
                        [InputEvent::RELEASE_ALL]
                    );
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(refresh && changed_fps);
        auth.nonce
    });
    let state = wait(&client, |s| s.statistics.frames == 1 && s.device.is_some());
    assert_eq!(state.state, State::Streaming);
    let framebuffer = state.framebuffer.unwrap();
    assert_eq!(
        &framebuffer.read().unwrap().native_rgba()[..4],
        &[255, 0, 0, 255]
    );
    let expected_bytes = [
        "hello_password",
        "auth_success",
        "server_init_2x2",
        "frame_raw_and_solid",
        "device_info",
    ]
    .into_iter()
    .map(|name| fixture(name).payload.len() as u64 + 5)
    .sum::<u64>()
        + 6
        + 13;
    wait(&client, |s| s.statistics.received_bytes >= expected_bytes);
    client.command(Command::Refresh).unwrap();
    client.command(Command::FrameRate(15)).unwrap();
    wait(&client, |s| s.statistics.rtt.is_some());
    let at = Instant::now();
    client.stop();
    assert!(at.elapsed() < Duration::from_millis(700));
    assert_eq!(client.snapshot().state, State::Idle);
    server.join().unwrap();
}
#[test]
fn worker_samples_all_received_bytes_without_ui_and_resets_history_on_retry() {
    use mpc_remote_screen_viewer_gpui::statistics::SAMPLE_INTERVAL;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut config = Config::new(
        "127.0.0.1",
        listener.local_addr().unwrap().port(),
        "".into(),
    )
    .unwrap();
    config.timing.ping = Duration::from_secs(60);
    config.timing.silence = Duration::from_secs(60);
    config.timing.retry = Duration::from_millis(50);
    let mut client = Session::start(config).unwrap();
    let (notice, notices) = std::sync::mpsc::channel();
    let (advance, advances) = std::sync::mpsc::channel();
    let mut skipped = fixture("frame_raw_and_solid");
    skipped.payload[15] = 99; // Structurally valid, unsupported rectangle codec.
    let messages = vec![
        audio_init(false),
        fixture("device_info"),
        Message::new(Kind::DeviceInfo, [255]), // Bad optional data is still traffic.
        token(Kind::Ping, 123),
        Message::new(Kind::AudioFormat, [0; 6]), // Unsolicited optional audio, ignored.
        pcm(0, 1),
        fixture("frame_raw_and_solid"),
        skipped,
        Message::new(Kind::FrameUpdate, [0; 7]), // Empty frames still count and ACK.
    ];
    let expected_bytes = [fixture("hello_password"), fixture("auth_success")]
        .iter()
        .chain(messages.iter())
        .map(|m| m.payload.len() as u64 + 5)
        .sum::<u64>();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        for message in messages {
            send(&mut socket, message);
        }
        assert_eq!(receive(&mut socket), options(0));
        assert_eq!(receive(&mut socket), token(Kind::Pong, 123));
        for id in [42, 42, 0] {
            assert_eq!(receive(&mut socket), ack(id));
        }
        notice.send(()).unwrap();
        advances.recv_timeout(Duration::from_secs(10)).unwrap();
        send(&mut socket, fixture("bye"));
        drop(socket);
        let (mut socket, _) = listener.accept().unwrap();
        notice.send(()).unwrap(); // Hold Hello so the new-attempt reset is observable.
        advances.recv_timeout(Duration::from_secs(5)).unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        send(&mut socket, audio_init(false));
        assert_eq!(receive(&mut socket), options(0));
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    notices.recv_timeout(Duration::from_secs(5)).unwrap();
    // No snapshot requests or painting during either frame application/ACK or sampling.
    thread::sleep(SAMPLE_INTERVAL + Duration::from_millis(150));
    let snapshot = client.snapshot();
    assert_eq!(snapshot.protocol_version, Some(2));
    assert_eq!(snapshot.statistics.frames, 3);
    assert_eq!(snapshot.statistics.skipped_rectangles, 1);
    assert_eq!(snapshot.statistics.received_bytes, expected_bytes);
    assert!(snapshot.device.is_some());
    assert!(!snapshot.statistics.history().is_empty());
    let sample = snapshot.statistics.history()[0];
    assert!((sample.rates.frames_per_second * sample.interval.as_secs_f64() - 3.0).abs() < 1e-9);
    assert!(
        (sample.rates.kilobits_per_second * sample.interval.as_secs_f64()
            - expected_bytes as f64 * 0.008)
            .abs()
            < 1e-9
    );
    let idle = wait(&client, |s| s.statistics.history().len() >= 2);
    let rates = idle.statistics.rates.unwrap();
    assert_eq!(rates.frames_per_second, 0.0);
    assert_eq!(rates.kilobits_per_second, 0.0);
    advance.send(()).unwrap();
    notices.recv_timeout(Duration::from_secs(5)).unwrap();
    let reset = wait(&client, |s| s.state == State::WaitingForHello);
    assert!(reset.statistics.history().is_empty());
    assert_eq!(reset.statistics.rates, None);
    assert_eq!(reset.statistics.frames, 0);
    assert_eq!(reset.statistics.received_bytes, 0);
    assert_eq!(reset.protocol_version, None);
    assert!(reset.device.is_none());
    advance.send(()).unwrap();
    wait(&client, |s| s.state == State::Streaming);
    client.stop();
    assert_eq!(client.snapshot().statistics.rates, None);
    server.join().unwrap();
}

#[test]
fn authentication_refusal_is_terminal_until_explicit_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start(&listener, "bad");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        authenticate(&mut socket, "bad", true);
        send(&mut socket, fixture("auth_failure"));
        listener
    });
    let state = wait(&client, |s| s.state == State::Failed);
    assert!(state.error.unwrap().contains("authentication failed"));
    // Wait beyond the initial retry window before stopping: rejection must not retry.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(client.snapshot().state, State::Failed);
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let at = Instant::now();
    client.stop();
    assert!(at.elapsed() < Duration::from_millis(700));
}
#[test]
fn reconnect_uses_new_nonce_and_new_framebuffer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start(&listener, "");
    let (notice, notices) = std::sync::mpsc::channel();
    let (advance, advances) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let mut nonces = Vec::new();
        for i in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            let auth = authenticate(&mut socket, "", true);
            nonces.push(auth.nonce);
            success(&mut socket, "", &auth);
            send(&mut socket, fixture("server_init_2x2"));
            assert_eq!(receive(&mut socket), options(0));
            if i == 0 {
                send(&mut socket, fixture("frame_raw_and_solid"));
                assert_eq!(non_ping(&mut socket), ack(42));
                notice.send(()).unwrap();
                advances.recv_timeout(Duration::from_secs(5)).unwrap();
                send(&mut socket, fixture("bye"));
            } else {
                loop {
                    let m = receive(&mut socket);
                    if m.kind == Kind::Input {
                        break;
                    }
                    if m.kind == Kind::Ping {
                        send(
                            &mut socket,
                            token(Kind::Pong, parse_token(&m.payload).unwrap()),
                        );
                    }
                }
            }
        }
        assert_ne!(nonces[0], nonces[1]);
    });
    notices.recv_timeout(Duration::from_secs(5)).unwrap();
    let old = wait(&client, |s| s.statistics.frames == 1)
        .framebuffer
        .unwrap();
    advance.send(()).unwrap();
    let state = wait(&client, |s| {
        s.state == State::Streaming
            && s.statistics.frames == 0
            && s.framebuffer
                .as_ref()
                .is_some_and(|frame| !std::sync::Arc::ptr_eq(frame, &old))
    });
    assert_eq!(
        &state.framebuffer.unwrap().read().unwrap().native_rgba()[..4],
        &[0, 0, 0, 255]
    );
    client.stop();
    server.join().unwrap();
}
#[test]
fn server_proof_flag_zero_still_requires_complete_proof() {
    for (verify, proof_bytes, connected) in
        [(true, 32, false), (false, 32, true), (false, 0, false)]
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _reservation = listener.try_clone().unwrap();
        let mut client = start(&listener, "p");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            authenticate(&mut socket, "p", verify);
            let mut payload = vec![1];
            payload.extend(vec![0; proof_bytes]);
            send(&mut socket, Message::new(Kind::AuthResult, payload));
            if connected {
                send(&mut socket, fixture("server_init_2x2"));
                assert_eq!(receive(&mut socket), options(0));
                loop {
                    let m = receive(&mut socket);
                    if m.kind == Kind::Input {
                        break;
                    }
                    if m.kind == Kind::Ping {
                        send(
                            &mut socket,
                            token(Kind::Pong, parse_token(&m.payload).unwrap()),
                        );
                    }
                }
            }
        });
        let state = wait(&client, |s| {
            s.state
                == if connected {
                    State::Streaming
                } else {
                    State::Failed
                }
        });
        assert_eq!(state.screen.is_some(), connected);
        if verify {
            assert!(state.error.unwrap().contains("could not prove"));
        }
        client.stop();
        server.join().unwrap();
    }
}
#[test]
fn reconfiguration_empty_frames_and_failed_rectangles_still_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start(&listener, "p");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "p", true);
        success(&mut socket, "p", &auth);
        send(&mut socket, fixture("server_init_2x2"));
        assert_eq!(receive(&mut socket), options(0));
        let mut frame = u32::MAX.to_le_bytes().to_vec();
        frame.extend_from_slice(&[1, 1, 0]);
        for value in [0u16, 0, 2, 2] {
            frame.extend_from_slice(&value.to_le_bytes());
        }
        frame.extend_from_slice(&[99, 0]); // Unknown per-rectangle codec is skipped.
        frame.extend_from_slice(&0u32.to_le_bytes());
        send(&mut socket, Message::new(Kind::FrameUpdate, frame));
        assert_eq!(receive(&mut socket), ack(u32::MAX));
        let mut init = fixture("server_init_2x2").payload;
        let rotation = init.len() - 3;
        init[rotation] = 1;
        send(&mut socket, Message::new(Kind::ServerInit, init));
        send(&mut socket, Message::new(Kind::FrameUpdate, [0; 7]));
        assert_eq!(
            InputEvent::parse_batch(&receive(&mut socket).payload).unwrap(),
            [InputEvent::RELEASE_ALL]
        );
        assert_eq!(receive(&mut socket), ack(0));
        loop {
            let message = receive(&mut socket);
            if message.kind == Kind::Input {
                break;
            }
            if message.kind == Kind::Ping {
                send(
                    &mut socket,
                    token(Kind::Pong, parse_token(&message.payload).unwrap()),
                );
            }
        }
    });
    let state = wait(&client, |s| s.statistics.frames == 2);
    assert_eq!(state.statistics.skipped_rectangles, 1);
    assert_eq!(state.screen.unwrap().rotation, 1);
    assert_eq!(
        &state.framebuffer.unwrap().read().unwrap().native_rgba()[..4],
        &[0, 0, 0, 255]
    );
    client.stop();
    server.join().unwrap();
}

#[test]
fn hello_deadline_and_streaming_silence_are_enforced() {
    for login in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _reservation = listener.try_clone().unwrap();
        let mut config = Config::new(
            "127.0.0.1",
            listener.local_addr().unwrap().port(),
            "p".into(),
        )
        .unwrap();
        config.timing.handshake = Duration::from_millis(if login { 5000 } else { 500 });
        config.timing.silence = Duration::from_millis(300);
        config.timing.retry = Duration::from_secs(1);
        let mut client = Session::start(config).unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            if login {
                let auth = authenticate(&mut socket, "p", true);
                success(&mut socket, "p", &auth);
                send(&mut socket, fixture("server_init_2x2"));
                assert_eq!(receive(&mut socket), options(0));
            } else {
                socket.write_all(&[1, 24, 0]).unwrap(); // Incomplete Hello header cannot defeat the deadline.
            }
            thread::sleep(Duration::from_secs(1));
        });
        let state = wait(&client, |s| s.state == State::RetryWait);
        assert!(state.error.unwrap().contains(if login {
            "no messages"
        } else {
            "waiting for Hello"
        }));
        client.stop();
        server.join().unwrap();
    }
}

#[test]
fn stop_does_not_wait_for_a_framebuffer_reader() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut client = start(&listener, "p");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "p", true);
        success(&mut socket, "p", &auth);
        send(&mut socket, fixture("server_init_2x2"));
        assert_eq!(receive(&mut socket), options(0));
        loop {
            let message = receive(&mut socket);
            match message.kind {
                Kind::RequestFullRefresh => send(&mut socket, fixture("frame_raw_and_solid")),
                Kind::Ping => send(
                    &mut socket,
                    token(Kind::Pong, parse_token(&message.payload).unwrap()),
                ),
                Kind::Input => break,
                _ => {}
            }
        }
    });
    let state = wait(&client, |s| s.state == State::Streaming);
    let framebuffer = state.framebuffer.unwrap();
    let held_reader = framebuffer.read().unwrap();
    client.command(Command::Refresh).unwrap();
    thread::sleep(Duration::from_millis(100));
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = thread::spawn(move || {
        client.stop();
        tx.send(()).unwrap();
    });
    let stopped = rx.recv_timeout(Duration::from_millis(700));
    drop(held_reader);
    stop.join().unwrap();
    assert!(stopped.is_ok(), "Stop was blocked by a framebuffer reader");
    server.join().unwrap();
}

#[test]
fn frame_work_budget_skips_suffix_but_still_acks_and_accepts_later_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _reservation = listener.try_clone().unwrap();
    let mut config = Config::new(
        "127.0.0.1",
        listener.local_addr().unwrap().port(),
        "".into(),
    )
    .unwrap();
    config.frame_pixel_budget = 4;
    let mut client = Session::start(config).unwrap();
    let (notice, notices) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let auth = authenticate(&mut socket, "", true);
        success(&mut socket, "", &auth);
        send(&mut socket, fixture("server_init_2x2"));
        assert_eq!(non_ping(&mut socket), options(0));
        let mut bytes = 123u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0, 3, 0]);
        for pixel in [[0, 0, 255, 0], [255, 0, 0, 0], [0, 255, 0, 0]] {
            for value in [0u16, 0, 2, 2] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.extend_from_slice(&[3, 0]);
            bytes.extend_from_slice(&4u32.to_le_bytes());
            bytes.extend_from_slice(&pixel);
        }
        send(&mut socket, Message::new(Kind::FrameUpdate, bytes));
        assert_eq!(non_ping(&mut socket), ack(123));
        send(&mut socket, Message::new(Kind::FrameUpdate, [0; 7]));
        assert_eq!(non_ping(&mut socket), ack(0));
        notice.send(()).unwrap();
        assert_eq!(non_ping(&mut socket).kind, Kind::Input); // release-all on stop
    });
    notices.recv_timeout(Duration::from_secs(5)).unwrap();
    let state = wait(&client, |state| state.statistics.frames == 2);
    assert_eq!(state.statistics.skipped_rectangles, 2);
    let frame = state.framebuffer.unwrap();
    assert_eq!(&frame.read().unwrap().native_rgba()[..4], &[255, 0, 0, 255]);
    assert_eq!(frame.read().unwrap().content_revision(), 1);
    client.stop();
    server.join().unwrap();
}

#[test]
fn incompatible_protocol_is_terminal_and_new_session_can_retry_explicitly() {
    for wrong_kind in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _reservation = listener.try_clone().unwrap();
        let mut client = start(&listener, "");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            if wrong_kind {
                send(&mut socket, token(Kind::Pong, 0));
            } else {
                let mut hello = fixture("hello_password");
                hello.payload[4] = 3;
                send(&mut socket, hello);
            }
            listener
        });
        let state = wait(&client, |s| s.state == State::Failed);
        assert!(state.error.unwrap().contains(if wrong_kind {
            "Expected Hello"
        } else {
            "Unsupported protocol"
        }));
        let listener = server.join().unwrap();
        thread::sleep(Duration::from_millis(500));
        listener.set_nonblocking(true).unwrap();
        match listener.accept() {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock),
            Ok((socket, _)) => {
                let mut header = [0; 5];
                socket
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let bytes = (&socket).read(&mut header);
                panic!(
                    "Unexpected connection {:?} -> {:?}: {bytes:?}, {header:02x?}",
                    socket.peer_addr(),
                    socket.local_addr()
                );
            }
        }
        listener.set_nonblocking(false).unwrap();
        client.stop();
        assert_eq!(client.snapshot().state, State::Idle);
        let mut retry = start(&listener, "");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let auth = authenticate(&mut socket, "", true);
            success(&mut socket, "", &auth);
            send(&mut socket, fixture("server_init_2x2"));
            assert_eq!(non_ping(&mut socket), options(0));
            assert_eq!(non_ping(&mut socket).kind, Kind::Input);
        });
        wait(&retry, |s| s.state == State::Streaming);
        retry.stop();
        server.join().unwrap();
    }
}

#[test]
fn mock_listener_ignores_unrelated_http_probes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut probe = TcpStream::connect(address).unwrap();
    probe
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let viewer = TcpStream::connect(address).unwrap();
    let expected = viewer.local_addr().unwrap();
    assert_eq!(listener.accept().unwrap().1, expected);
    drop(viewer);
    let mut probe = TcpStream::connect(address).unwrap();
    probe.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancellation_during_network_retry_prevents_another_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = Config::new(
        "127.0.0.1",
        listener.local_addr().unwrap().port(),
        "".into(),
    )
    .unwrap();
    config.timing.retry = Duration::from_secs(30); // no narrow observation window
    let mut client = Session::start(config).unwrap();
    let (socket, _) = listener.accept().unwrap();
    drop(socket); // transient EOF during Hello
    wait(&client, |s| s.state == State::RetryWait);
    client.stop();
    assert_eq!(client.snapshot().state, State::Idle);
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn stop_while_waiting_for_hello_and_invalid_configuration() {
    assert!(Config::new(" ", 7447, "".into()).is_err());
    assert!(Config::new("localhost", 0, "".into()).is_err());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = start(&listener, "");
    let (_socket, _) = listener.accept().unwrap();
    wait(&client, |s| s.state == State::WaitingForHello);
    let at = Instant::now();
    client.stop();
    assert!(at.elapsed() < Duration::from_millis(700));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(client.snapshot().state, State::Idle);
    assert!(client.snapshot().framebuffer.is_none());
}
