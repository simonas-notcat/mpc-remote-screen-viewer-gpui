//! Loopback only: these tests never connect to a physical MPC/Force.
use super::*;
use mpc_remote_screen_viewer_gpui::{
    control::{Controller, Gesture, Phase, Point},
    input,
    teacher::{self, Bridge, Reply, Request, Teacher},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
};

enum DeviceCommand {
    Reconfigure,
    Frame,
    Update(Message),
    Close,
}
struct Device {
    commands: mpsc::Sender<DeviceCommand>,
    inputs: mpsc::Receiver<Vec<InputEvent>>,
    worker: Option<thread::JoinHandle<()>>,
    _reservation: SystemListener,
}
impl Device {
    fn start() -> (Session, Self) {
        Self::start_with_size(2)
    }
    fn start_with_size(edge: u16) -> (Session, Self) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let reservation = listener.try_clone().unwrap();
        let session = start(&listener, "assisted-test");
        let (commands, rx) = mpsc::channel();
        let (tx, inputs) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let auth = authenticate(&mut socket, "assisted-test", true);
            success(&mut socket, "assisted-test", &auth);
            let mut init = fixture("server_init_2x2");
            init.payload[..2].copy_from_slice(&edge.to_le_bytes());
            init.payload[2..4].copy_from_slice(&edge.to_le_bytes());
            send(&mut socket, init);
            assert_eq!(receive(&mut socket).kind, Kind::SetOptions);
            send(&mut socket, fixture("frame_raw_and_solid"));
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            let mut assembler = Assembler::default();
            let mut bytes = [0; 4096];
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until {
                match rx.try_recv() {
                    Ok(DeviceCommand::Reconfigure) => {
                        let mut init = fixture("server_init_2x2");
                        let rotation = init.payload.len() - 3;
                        init.payload[rotation] = 2; // Creates a new framebuffer identity.
                        send(&mut socket, init);
                        send(&mut socket, fixture("frame_raw_and_solid"));
                    }
                    Ok(DeviceCommand::Frame) => send(&mut socket, fixture("frame_raw_and_solid")),
                    Ok(DeviceCommand::Update(message)) => send(&mut socket, message),
                    Ok(DeviceCommand::Close) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match socket.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => {
                        for message in assembler.feed(&bytes[..n]).unwrap() {
                            match message.kind {
                                Kind::Input => {
                                    let _ =
                                        tx.send(InputEvent::parse_batch(&message.payload).unwrap());
                                }
                                Kind::Ping => send(
                                    &mut socket,
                                    token(Kind::Pong, parse_token(&message.payload).unwrap()),
                                ),
                                Kind::Ack => {}
                                other => panic!(
                                    "Unexpected protocol command from assisted input: {other:?}"
                                ),
                            }
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(e) => panic!("Mock read failed: {e}"),
                }
            }
        });
        wait(&session, |s| {
            s.state == State::Streaming && s.statistics.frames > 0
        });
        (
            session,
            Self {
                commands,
                inputs,
                worker: Some(worker),
                _reservation: reservation,
            },
        )
    }
    fn input(&self) -> Vec<InputEvent> {
        self.input_within(Duration::from_secs(3))
    }
    fn input_within(&self, timeout: Duration) -> Vec<InputEvent> {
        self.inputs.recv_timeout(timeout).expect("Input deadline")
    }
    fn no_input(&self) {
        assert!(matches!(
            self.inputs.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.commands.send(DeviceCommand::Close);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
struct Endpoint {
    bridge: Bridge,
    directory: PathBuf,
}
impl Endpoint {
    fn new(session: &Session) -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mpc-assisted-test-{}-{}",
            std::process::id(),
            u64::from_le_bytes(nonce)
        ));
        let bridge = Bridge::start(directory.join("viewer.sock")).unwrap();
        bridge.teacher().set_session(session);
        Self { bridge, directory }
    }
    fn request(&self, request: Request) -> std::result::Result<serde_json::Value, String> {
        match teacher::request(self.bridge.path(), &request)? {
            Reply::Ok { data, .. } => Ok(data),
            Reply::Error { message } => Err(message),
        }
    }
    fn screen(&self) -> String {
        self.request(Request::GetScreen {
            region: None,
            max_size: None,
        })
        .unwrap()["observation_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn propose(&self, observation_id: &str, gesture: Gesture) -> u64 {
        let value = self
            .request(Request::Propose {
                observation_id: observation_id.into(),
                gesture,
            })
            .unwrap();
        assert_eq!(value["device_input_sent"], false);
        assert_eq!(value["queued"], true);
        value["action"]["id"].as_u64().unwrap()
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.bridge.request_stop();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn click(hold_ms: u64) -> Gesture {
    Gesture::Click {
        target: Point { x: 1, y: 1 },
        hold_ms,
    }
}
fn wait_phase(control: &Controller, phase: Phase) {
    let until = Instant::now() + Duration::from_secs(3);
    while control
        .status()
        .feedback
        .as_ref()
        .is_none_or(|f| f.phase != phase)
    {
        assert!(
            Instant::now() < until,
            "Expected {phase:?}, got {:?}",
            control.status()
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn screen(teacher: &Teacher) -> String {
    let Reply::Ok { data, .. } = teacher
        .handle(Request::GetScreen {
            region: None,
            max_size: None,
        })
        .unwrap()
    else {
        panic!("Capture failed");
    };
    data["observation_id"].as_str().unwrap().into()
}

#[test]
fn one_global_toggle_runs_all_six_gestures_without_per_gesture_approval() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let observation = endpoint.screen();
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: observation.clone(),
                gesture: click(100)
            })
            .is_err()
    );
    control.set_enabled(true);
    endpoint.propose(&observation, click(100));
    assert_eq!(device.input(), [input::pointer(4, 0, (1, 1), 0)]);
    assert_eq!(device.input(), [input::pointer(5, 0, (1, 1), 0)]);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: observation,
                gesture: click(100)
            })
            .is_err()
    );

    endpoint.propose(
        &endpoint.screen(),
        Gesture::DoubleTap {
            target: Point { x: 1, y: 1 },
        },
    );
    for kind in [4, 5, 4, 5] {
        assert_eq!(device.input(), [input::pointer(kind, 0, (1, 1), 0)]);
    }
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);
    let feedback = control.status().feedback.unwrap();
    assert_eq!(feedback.gesture, "double_tap");
    assert_eq!(feedback.written_steps, 4);
    assert!(feedback.release_written);

    endpoint.propose(
        &endpoint.screen(),
        Gesture::Drag {
            start: Point { x: 0, y: 0 },
            end: Point { x: 1, y: 1 },
            duration_ms: 100,
        },
    );
    assert_eq!(device.input(), [input::pointer(4, 0, (0, 0), 0)]);
    assert_eq!(device.input()[0].kind, 3);
    let end = device.input();
    assert_eq!(end.last(), Some(&input::pointer(5, 0, (1, 1), 0)));
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);
    assert_eq!(control.status().feedback.unwrap().path.len(), 3);

    endpoint.propose(
        &endpoint.screen(),
        Gesture::Scroll {
            target: Point { x: 1, y: 0 },
            amount: -2,
        },
    );
    assert_eq!(device.input(), [input::pointer(6, 0, (1, 0), -2)]);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);

    endpoint.propose(
        &endpoint.screen(),
        Gesture::PressKey {
            key: "A".into(),
            modifiers: [false; 4],
        },
    );
    assert_eq!(
        device.input(),
        input::Control::default().key("A", [false; 4])
    );
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);

    let text = "Ab!";
    endpoint.propose(&endpoint.screen(), Gesture::TypeText { text: text.into() });
    assert!(
        !endpoint
            .request(Request::GetState)
            .unwrap()
            .to_string()
            .contains(text)
    );
    for c in text.chars() {
        assert_eq!(
            device.input(),
            input::Control::default().key(&c.to_string(), [false; 4])
        );
    }
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);
    assert_eq!(control.status().feedback.unwrap().written_steps, 3);
    assert!(control.status().enabled);
    endpoint.bridge.request_stop();
    assert!(!control.status().enabled);
    session.request_stop();
}
/// Valid compressed updates within the default byte/pixel budgets. Preparing the
/// payload before holding an agent input keeps compression time out of the test.
fn expensive_update(edge: u16, rectangles: u16) -> Message {
    let compressed = lz4_flex::block::compress(&vec![0; usize::from(edge).pow(2) * 4]);
    let mut payload = 99u32.to_le_bytes().to_vec();
    payload.push(0);
    payload.extend(rectangles.to_le_bytes());
    for _ in 0..rectangles {
        for v in [0u16, 0, edge, edge] {
            payload.extend(v.to_le_bytes());
        }
        payload.extend([1, 0]);
        payload.extend((compressed.len() as u32).to_le_bytes());
        payload.extend_from_slice(&compressed);
    }
    Message::new(Kind::FrameUpdate, payload)
}

#[test]
fn slow_single_and_many_rectangle_frames_do_not_block_stop_takeover_or_deadlines() {
    #[derive(Clone, Copy, Debug)]
    enum Interrupt {
        Off,
        SessionStop,
        Cancel,
        Human,
        Deadline,
    }
    for (edge, rectangles, interrupt) in [
        (4096, 1, Interrupt::Off),
        (4096, 1, Interrupt::SessionStop),
        (1024, 64, Interrupt::Cancel),
        (1024, 64, Interrupt::Human),
        (1024, 64, Interrupt::Deadline),
    ] {
        let (session, device) = Device::start_with_size(edge);
        let control = session.control();
        let frame = session.snapshot().framebuffer.unwrap();
        let update = expensive_update(edge, rectangles);
        control.set_enabled(true);
        // Direct controller admission supplies a short observation expiry for the
        // deadline case; all device writes still go through the real socket owner.
        let ttl = if matches!(interrupt, Interrupt::Deadline) {
            Duration::from_millis(700)
        } else {
            Duration::from_secs(60)
        };
        control
            .submit(
                &frame,
                7,
                (edge, edge),
                click(2000),
                control.epoch(),
                Instant::now() + ttl,
            )
            .unwrap();
        assert_eq!(device.input()[0].kind, 4);
        device.commands.send(DeviceCommand::Update(update)).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if matches!(frame.try_read(), Err(std::sync::TryLockError::WouldBlock)) {
                break;
            }
            assert!(
                Instant::now() < until,
                "Decoder did not start: {interrupt:?}"
            );
            thread::sleep(Duration::from_millis(2));
        }
        let keys = input::Control::default().key("a", [false; 4]);
        match interrupt {
            Interrupt::Off => control.set_enabled(false),
            Interrupt::SessionStop => session.request_stop(),
            Interrupt::Cancel => control.cancel(),
            Interrupt::Human => session
                .command(Command::Input {
                    screen: frame.clone(),
                    events: keys.clone(),
                })
                .unwrap(),
            Interrupt::Deadline => {}
        }
        // A whole-frame or even a per-rectangle cooperative callback is too late
        // for the single-large-rectangle case. Release must not wait for decoding.
        assert_eq!(
            device.input_within(Duration::from_secs(1)),
            [InputEvent::RELEASE_ALL],
            "{interrupt:?}"
        );
        if matches!(interrupt, Interrupt::Human) {
            assert_eq!(device.input_within(Duration::from_secs(1)), keys);
        }
        if matches!(interrupt, Interrupt::SessionStop) {
            // Teardown may report uncertainty, but the release packet above MUST
            // arrive before joining a codec which is still finishing this rectangle.
            assert!(!control.status().enabled);
        } else {
            wait_phase(&control, Phase::Cancelled);
            let status = control.status();
            assert!(status.feedback.unwrap().release_written);
            assert_eq!(status.enabled, !matches!(interrupt, Interrupt::Off));
            assert_eq!(session.snapshot().state, State::Streaming);
        }
        session.request_stop();
    }
}

#[test]
fn human_takeover_and_held_ownership_work_while_a_framebuffer_reader_blocks_decoding() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.set_enabled(true);
    endpoint.propose(&endpoint.screen(), click(2000));
    assert_eq!(device.input()[0].kind, 4);
    let guard = frame.read().unwrap();
    device.commands.send(DeviceCommand::Frame).unwrap();
    thread::sleep(Duration::from_millis(100));
    let down = input::pointer(4, 0, (0, 0), 0);
    session
        .command(Command::Input {
            screen: frame.clone(),
            events: vec![down],
        })
        .unwrap();
    assert_eq!(
        device.input_within(Duration::from_secs(1)),
        [InputEvent::RELEASE_ALL]
    );
    assert_eq!(device.input_within(Duration::from_secs(1)), [down]);
    wait_phase(&control, Phase::Cancelled);
    assert!(control.status().human_held);
    assert!(control.status().enabled);
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: endpoint.screen(),
                gesture: click(100),
            })
            .is_err()
    );
    session
        .command(Command::Input {
            screen: frame.clone(),
            events: vec![InputEvent::RELEASE_ALL],
        })
        .unwrap();
    assert_eq!(
        device.input_within(Duration::from_secs(1)),
        [InputEvent::RELEASE_ALL]
    );
    let until = Instant::now() + Duration::from_secs(1);
    while control.status().human_held {
        assert!(Instant::now() < until);
        thread::yield_now();
    }
    drop(guard);
    wait(&session, |s| s.statistics.frames == 2); // No delta/ACK was lost to takeover.
    session.request_stop();
}

#[test]
fn cancellation_works_while_the_session_waits_for_a_framebuffer_reader() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.set_enabled(true);
    endpoint.propose(&endpoint.screen(), click(2000));
    assert_eq!(device.input()[0].kind, 4);
    let guard = frame.read().unwrap();
    device.commands.send(DeviceCommand::Frame).unwrap();
    thread::sleep(Duration::from_millis(100));
    endpoint.request(Request::Cancel).unwrap();
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Cancelled);
    assert!(control.status().feedback.unwrap().release_written);
    assert!(control.status().enabled);
    device.no_input();
    drop(guard);
    session.request_stop();
}
#[test]
fn local_input_takes_over_after_release_and_human_holds_block_agent_ownership() {
    let (session, device) = Device::start();
    let teacher = Teacher::new().unwrap();
    teacher.set_session(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.set_enabled(true);
    teacher
        .handle(Request::Propose {
            observation_id: screen(&teacher),
            gesture: click(2000),
        })
        .unwrap();
    assert_eq!(device.input()[0].kind, 4);
    let keys = input::Control::default().key("a", [false; 4]);
    session
        .command(Command::Input {
            screen: frame.clone(),
            events: keys.clone(),
        })
        .unwrap();
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    assert_eq!(device.input(), keys);
    wait_phase(&control, Phase::Cancelled);
    assert!(control.status().enabled);
    session
        .command(Command::Input {
            screen: frame.clone(),
            events: vec![input::pointer(4, 0, (0, 0), 0)],
        })
        .unwrap();
    assert_eq!(device.input()[0].kind, 4);
    assert!(control.status().human_held);
    assert!(
        teacher
            .handle(Request::Propose {
                observation_id: screen(&teacher),
                gesture: click(100)
            })
            .is_err()
    );
    session
        .command(Command::Input {
            screen: frame.clone(),
            events: vec![InputEvent::RELEASE_ALL],
        })
        .unwrap();
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    let until = Instant::now() + Duration::from_secs(1);
    while control.status().human_held {
        assert!(Instant::now() < until);
        thread::yield_now();
    }
    assert!(control.status().enabled);
    teacher
        .handle(Request::Propose {
            observation_id: screen(&teacher),
            gesture: click(100),
        })
        .unwrap();
    assert_eq!(device.input()[0].kind, 4);
    assert_eq!(device.input()[0].kind, 5);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    session.request_stop();
}
#[test]
fn lost_connection_reports_uncertainty_without_resetting_global_permission() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    control.set_enabled(true);
    let observation = endpoint.screen();
    endpoint.propose(&observation, click(2000));
    assert_eq!(device.input()[0].kind, 4);
    device.commands.send(DeviceCommand::Close).unwrap();
    wait(&session, |s| s.state != State::Streaming);
    wait_phase(&control, Phase::Uncertain);
    assert!(control.status().enabled);
    assert!(!control.status().feedback.unwrap().release_written);
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: observation,
                gesture: click(100)
            })
            .is_err()
    );
    session.request_stop();
}
#[test]
fn reconfiguration_releases_old_input_and_preserves_global_authorization() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.set_enabled(true);
    let observation = endpoint.screen();
    endpoint.propose(&observation, click(2000));
    assert_eq!(device.input()[0].kind, 4);
    device.commands.send(DeviceCommand::Reconfigure).unwrap();
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait(&session, |s| {
        s.framebuffer
            .as_ref()
            .is_some_and(|f| !Arc::ptr_eq(f, &frame))
    });
    assert!(control.status().enabled);
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: observation,
                gesture: click(100)
            })
            .is_err()
    );
    device.no_input();
    wait(&session, |s| s.statistics.frames > 1);
    endpoint.propose(&endpoint.screen(), click(100));
    assert_eq!(device.input()[0].kind, 4);
    assert_eq!(device.input()[0].kind, 5);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    session.request_stop();
}
#[test]
fn turning_global_assistance_off_releases_running_input_and_rejects_more() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    control.set_enabled(true);
    endpoint.propose(&endpoint.screen(), click(2000));
    assert_eq!(device.input()[0].kind, 4);
    control.set_enabled(false);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Cancelled);
    assert!(!control.status().enabled);
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: endpoint.screen(),
                gesture: click(100)
            })
            .is_err()
    );
    device.no_input();
    session.request_stop();
}
