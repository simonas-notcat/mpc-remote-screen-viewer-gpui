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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let reservation = listener.try_clone().unwrap();
        let session = start(&listener, "assisted-test");
        let (commands, rx) = mpsc::channel();
        let (tx, inputs) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let auth = authenticate(&mut socket, "assisted-test", true);
            success(&mut socket, "assisted-test", &auth);
            send(&mut socket, fixture("server_init_2x2"));
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
                    }
                    Ok(DeviceCommand::Frame) => send(&mut socket, fixture("frame_raw_and_solid")),
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
        self.inputs
            .recv_timeout(Duration::from_secs(3))
            .expect("Input deadline")
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
fn ipc_proposals_need_local_approval_and_all_five_gestures_report_worker_writes() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    let observation = endpoint.screen();
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: observation.clone(),
                gesture: click(100)
            })
            .is_err()
    );
    control.grant(&frame).unwrap();
    let id = endpoint.propose(&observation, click(100));
    device.no_input();
    assert!(control.approve(id + 1).is_err());
    control.approve(id).unwrap();
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

    let id = endpoint.propose(
        &endpoint.screen(),
        Gesture::Drag {
            start: Point { x: 0, y: 0 },
            end: Point { x: 1, y: 1 },
            duration_ms: 100,
        },
    );
    control.approve(id).unwrap();
    assert_eq!(device.input(), [input::pointer(4, 0, (0, 0), 0)]);
    assert_eq!(device.input()[0].kind, 3);
    let end = device.input();
    assert_eq!(end.last(), Some(&input::pointer(5, 0, (1, 1), 0)));
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);
    assert_eq!(control.status().feedback.unwrap().path.len(), 3);

    let id = endpoint.propose(
        &endpoint.screen(),
        Gesture::Scroll {
            target: Point { x: 1, y: 0 },
            amount: -2,
        },
    );
    control.approve(id).unwrap();
    assert_eq!(device.input(), [input::pointer(6, 0, (1, 0), -2)]);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);

    let id = endpoint.propose(
        &endpoint.screen(),
        Gesture::PressKey {
            key: "A".into(),
            modifiers: [false; 4],
        },
    );
    control.approve(id).unwrap();
    assert_eq!(
        device.input(),
        input::Control::default().key("A", [false; 4])
    );
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);

    let text = "Ab!";
    let id = endpoint.propose(&endpoint.screen(), Gesture::TypeText { text: text.into() });
    assert!(
        !endpoint
            .request(Request::GetState)
            .unwrap()
            .to_string()
            .contains(text)
    );
    control.approve(id).unwrap();
    for c in text.chars() {
        assert_eq!(
            device.input(),
            input::Control::default().key(&c.to_string(), [false; 4])
        );
    }
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Completed);
    assert_eq!(control.status().feedback.unwrap().written_steps, 3);
    endpoint.bridge.request_stop();
    assert!(!control.status().lease_active);
    session.request_stop();
}
#[test]
fn cancellation_works_while_the_session_waits_for_a_framebuffer_reader() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.grant(&frame).unwrap();
    let id = endpoint.propose(&endpoint.screen(), click(2000));
    control.approve(id).unwrap();
    assert_eq!(device.input()[0].kind, 4);
    let guard = frame.read().unwrap();
    device.commands.send(DeviceCommand::Frame).unwrap();
    thread::sleep(Duration::from_millis(100));
    endpoint.request(Request::Cancel).unwrap();
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait_phase(&control, Phase::Cancelled);
    assert!(control.status().feedback.unwrap().release_written);
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
    control.grant(&frame).unwrap();
    teacher
        .handle(Request::Propose {
            observation_id: screen(&teacher),
            gesture: click(2000),
        })
        .unwrap();
    control
        .approve(control.status().feedback.unwrap().id)
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
    assert!(!control.status().lease_active);
    session
        .command(Command::Input {
            screen: frame.clone(),
            events: vec![input::pointer(4, 0, (0, 0), 0)],
        })
        .unwrap();
    assert_eq!(device.input()[0].kind, 4);
    assert!(control.status().human_held);
    assert!(control.grant(&frame).is_err());
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
    control.grant(&frame).unwrap();
    session.request_stop();
}
#[test]
fn lost_connection_reports_uncertainty_and_revokes_instead_of_replaying() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.grant(&frame).unwrap();
    let observation = endpoint.screen();
    let id = endpoint.propose(&observation, click(2000));
    control.approve(id).unwrap();
    assert_eq!(device.input()[0].kind, 4);
    device.commands.send(DeviceCommand::Close).unwrap();
    wait(&session, |s| s.state != State::Streaming);
    wait_phase(&control, Phase::Uncertain);
    assert!(!control.status().lease_active);
    assert!(!control.status().feedback.unwrap().release_written);
    assert!(control.approve(id).is_err());
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
fn reconfiguration_releases_revokes_and_never_replays_approved_input() {
    let (session, device) = Device::start();
    let endpoint = Endpoint::new(&session);
    let control = session.control();
    let frame = session.snapshot().framebuffer.unwrap();
    control.grant(&frame).unwrap();
    let observation = endpoint.screen();
    let id = endpoint.propose(&observation, click(2000));
    control.approve(id).unwrap();
    assert_eq!(device.input()[0].kind, 4);
    device.commands.send(DeviceCommand::Reconfigure).unwrap();
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    assert_eq!(device.input(), [InputEvent::RELEASE_ALL]);
    wait(&session, |s| {
        s.framebuffer
            .as_ref()
            .is_some_and(|f| !Arc::ptr_eq(f, &frame))
    });
    assert!(!control.status().lease_active);
    assert!(control.approve(id).is_err());
    assert!(
        endpoint
            .request(Request::Propose {
                observation_id: observation,
                gesture: click(100)
            })
            .is_err()
    );
    device.no_input();
    session.request_stop();
}
