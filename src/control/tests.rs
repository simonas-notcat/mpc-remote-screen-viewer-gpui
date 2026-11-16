use super::*;
use crate::protocol::{PixelFormat, ServerInit};

fn screen() -> Screen {
    Arc::new(RwLock::new(
        Framebuffer::new(ServerInit {
            width: 200,
            height: 100,
            rotation: 0,
            pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
            name: "synthetic".into(),
            input_capabilities: 7,
            audio_available: false,
        })
        .unwrap(),
    ))
}
fn expires() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn click() -> Gesture {
    Gesture::Click {
        target: Point { x: 12, y: 34 },
        hold_ms: 100,
    }
}
fn setup() -> (Arc<Controller>, Screen) {
    let controller = Arc::new(Controller::default());
    controller.set_enabled(true);
    (controller, screen())
}
fn submit(controller: &Controller, screen: &Screen, gesture: Gesture) -> u64 {
    let data = controller
        .submit(
            screen,
            7,
            (200, 100),
            gesture,
            controller.epoch(),
            expires(),
        )
        .unwrap();
    assert_eq!(data["device_input_sent"], false);
    assert_eq!(data["queued"], true);
    assert_eq!(data["action"]["phase"], "queued");
    data["action"]["id"].as_u64().unwrap()
}
fn drive(
    controller: &Arc<Controller>,
    running: &mut Option<Running>,
    screen: &Screen,
) -> Vec<InputEvent> {
    let mut sent = Vec::new();
    controller
        .drive(running, screen, 7, |events| {
            sent.extend_from_slice(events);
            Ok::<_, ()>(())
        })
        .unwrap();
    sent
}
fn finish(
    controller: &Arc<Controller>,
    running: &mut Option<Running>,
    screen: &Screen,
) -> Vec<InputEvent> {
    let mut sent = Vec::new();
    while running.is_some() {
        running.as_mut().unwrap().started = Instant::now() - Duration::from_secs(4);
        sent.extend(drive(controller, running, screen));
    }
    sent
}
#[test]
fn plans_enforce_capabilities_bounds_sizes_and_release_pairs() {
    for (caps, down, up) in [(1, 0, 2), (2, 4, 5), (7, 4, 5)] {
        let plan = Plan::new(click(), caps, (200, 100)).unwrap();
        assert_eq!(plan.steps[0].events, [input::pointer(down, 0, (12, 34), 0)]);
        assert_eq!(plan.steps[1].events, [input::pointer(up, 0, (12, 34), 0)]);
    }
    assert!(Plan::new(click(), 4, (200, 100)).is_err());
    assert!(Plan::new(click(), 7, (0, 0)).is_err());
    for target in [
        Point { x: 200, y: 0 },
        Point { x: 0, y: 100 },
        Point { x: u32::MAX, y: 1 },
    ] {
        assert!(
            Plan::new(
                Gesture::Click {
                    target,
                    hold_ms: 100
                },
                7,
                (200, 100)
            )
            .is_err()
        );
    }
    for hold_ms in [0, 49, 2001, u64::MAX] {
        assert!(
            Plan::new(
                Gesture::Click {
                    target: Point { x: 0, y: 0 },
                    hold_ms
                },
                7,
                (200, 100)
            )
            .is_err()
        );
    }
    for duration_ms in [99, 3001, u64::MAX] {
        assert!(
            Plan::new(
                Gesture::Drag {
                    start: Point { x: 0, y: 0 },
                    end: Point { x: 199, y: 99 },
                    duration_ms
                },
                7,
                (200, 100)
            )
            .is_err()
        );
    }
    for amount in [0, -121, 121, i16::MIN, i16::MAX] {
        assert!(
            Plan::new(
                Gesture::Scroll {
                    target: Point { x: 0, y: 0 },
                    amount
                },
                2,
                (200, 100)
            )
            .is_err()
        );
    }
    assert!(
        Plan::new(
            Gesture::Scroll {
                target: Point { x: 0, y: 0 },
                amount: 1
            },
            1,
            (200, 100)
        )
        .is_err()
    );
}
#[test]
fn keyboard_text_are_bounded_fully_released_and_never_echoed() {
    for text in ["", "has\nnewline", "\t", "é", &"a".repeat(65)] {
        assert!(Plan::new(Gesture::TypeText { text: text.into() }, 4, (200, 100)).is_err());
    }
    assert!(
        Plan::new(
            Gesture::PressKey {
                key: "unknown".into(),
                modifiers: [false; 4]
            },
            4,
            (200, 100)
        )
        .is_err()
    );
    assert!(Plan::new(Gesture::TypeText { text: "a".into() }, 2, (200, 100)).is_err());
    let plan = Plan::new(
        Gesture::PressKey {
            key: "a".into(),
            modifiers: [true; 4],
        },
        4,
        (200, 100),
    )
    .unwrap();
    let events = &plan.steps[0].events;
    assert_eq!(events.iter().filter(|e| e.kind == 8).count(), 5);
    assert_eq!(events.iter().filter(|e| e.kind == 9).count(), 5);
    let text = "NotASecret!";
    let plan = Plan::new(Gesture::TypeText { text: text.into() }, 4, (200, 100)).unwrap();
    assert_eq!(plan.steps.len(), text.len());
    assert!(
        !serde_json::to_string(&plan.feedback)
            .unwrap()
            .contains(text)
    );
    for step in plan.steps {
        assert_eq!(
            step.events.iter().filter(|e| e.kind == 8).count(),
            step.events.iter().filter(|e| e.kind == 9).count()
        );
        assert!(!step.held);
    }
}
#[test]
fn global_toggle_is_off_by_default_and_on_runs_without_per_gesture_approval() {
    let screen = screen();
    let controller = Arc::new(Controller::default());
    assert!(!controller.status().enabled);
    assert!(
        controller
            .submit(&screen, 7, (200, 100), click(), 0, expires())
            .is_err()
    );
    controller.set_enabled(true);
    submit(&controller, &screen, click());
    let mut running = None;
    assert_eq!(
        drive(&controller, &mut running, &screen),
        [input::pointer(4, 0, (12, 34), 0)]
    );
    let feedback = controller.status().feedback.unwrap();
    assert_eq!(feedback.phase, Phase::Running);
    assert!(feedback.held);
    assert_eq!(feedback.written_steps, 1);
    assert!(!feedback.release_written);
    let sent = finish(&controller, &mut running, &screen);
    assert_eq!(sent.last(), Some(&InputEvent::RELEASE_ALL));
    let feedback = controller.status().feedback.unwrap();
    assert_eq!(feedback.phase, Phase::Completed);
    assert_eq!(feedback.written_steps, 2);
    assert!(!feedback.held);
    assert!(feedback.release_written);
    assert!(controller.status().enabled);
    assert_eq!(controller.state()["visually_verified"], false);
    assert!(controller.state().get("lease_remaining_ms").is_none());
    // The same global authorization covers the next fresh gesture.
    submit(&controller, &screen, click());
    controller.set_enabled(false);
    assert!(drive(&controller, &mut running, &screen).is_empty());
    assert!(!controller.status().enabled);
    assert!(
        controller
            .submit(
                &screen,
                7,
                (200, 100),
                click(),
                controller.epoch(),
                expires()
            )
            .is_err()
    );
}
#[test]
fn permission_survives_completion_cancel_and_source_changes_until_explicitly_off() {
    let (controller, screen) = setup();
    submit(&controller, &screen, click());
    controller.cancel();
    assert!(controller.status().enabled);
    assert!(drive(&controller, &mut None, &screen).is_empty());
    submit(&controller, &screen, click());
    controller.disconnected();
    assert!(controller.status().enabled);
    let replacement = self::screen();
    assert!(drive(&controller, &mut None, &replacement).is_empty()); // No old-source replay.
    submit(&controller, &replacement, click());
    let mut running = None;
    assert!(!drive(&controller, &mut running, &replacement).is_empty());
    finish(&controller, &mut running, &replacement);
    assert!(controller.status().enabled);
    controller.set_enabled(false);
    assert!(!controller.status().enabled);
}
#[test]
fn observations_before_and_during_gestures_are_consumed_and_no_sequences_queue() {
    let (controller, screen) = setup();
    let before = controller.epoch();
    submit(&controller, &screen, click());
    assert!(
        controller
            .submit(&screen, 7, (200, 100), click(), before, expires())
            .is_err()
    );
    assert!(
        controller
            .submit(
                &screen,
                7,
                (200, 100),
                click(),
                controller.epoch(),
                expires()
            )
            .is_err()
    );
    let mut running = None;
    drive(&controller, &mut running, &screen);
    let during = controller.epoch();
    finish(&controller, &mut running, &screen);
    assert!(
        controller
            .submit(&screen, 7, (200, 100), click(), during, expires())
            .is_err()
    );
    submit(&controller, &screen, click());
}
#[test]
fn observation_expiry_caps_queue_and_execution_but_not_global_permission() {
    let (controller, screen) = setup();
    assert!(
        controller
            .submit(
                &screen,
                7,
                (200, 100),
                click(),
                controller.epoch(),
                Instant::now()
            )
            .is_err()
    );
    let expiry = Instant::now() + Duration::from_secs(4);
    controller
        .submit(&screen, 7, (200, 100), click(), controller.epoch(), expiry)
        .unwrap();
    assert_eq!(
        controller
            .inner
            .lock()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .until,
        expiry
    );
    let mut running = None;
    drive(&controller, &mut running, &screen);
    assert_eq!(running.as_ref().unwrap().deadline, expiry);
    controller.cancel();
    drive(&controller, &mut running, &screen);
    assert!(controller.status().enabled);
}
#[test]
fn running_cancel_toggle_off_and_deadline_release_without_continuing() {
    for stop in 0..3 {
        let (controller, screen) = setup();
        submit(&controller, &screen, click());
        let mut running = None;
        drive(&controller, &mut running, &screen);
        match stop {
            0 => controller.cancel(),
            1 => controller.set_enabled(false),
            _ => running.as_mut().unwrap().deadline = Instant::now(),
        }
        assert!(controller.status().feedback.unwrap().held);
        assert_eq!(
            drive(&controller, &mut running, &screen),
            [InputEvent::RELEASE_ALL]
        );
        assert!(running.is_none());
        let feedback = controller.status().feedback.unwrap();
        assert_eq!(feedback.phase, Phase::Cancelled);
        assert!(feedback.release_written);
        assert_eq!(feedback.written_steps, 1);
        assert_eq!(controller.status().enabled, stop != 1);
    }
}
#[test]
fn queued_work_expires_and_source_capabilities_are_rechecked() {
    let (controller, screen) = setup();
    submit(&controller, &screen, click());
    controller
        .inner
        .lock()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .until = Instant::now();
    assert_eq!(
        controller.status().feedback.unwrap().phase,
        Phase::Cancelled
    );
    assert!(controller.status().enabled);
    assert!(drive(&controller, &mut None, &screen).is_empty());
    for caps in [0, 4] {
        submit(&controller, &screen, click());
        controller
            .drive(&mut None, &screen, caps, |_| {
                panic!("Unsupported input must not be written")
            })
            .unwrap_or_else(|_: ()| unreachable!());
        assert_eq!(controller.status().feedback.unwrap().phase, Phase::Rejected);
    }
    submit(&controller, &screen, click());
    let replacement = self::screen();
    assert!(drive(&controller, &mut None, &replacement).is_empty());
    assert_eq!(controller.status().feedback.unwrap().phase, Phase::Rejected);
    submit(&controller, &replacement, click()); // Fresh input may use the new source, not old queued input.
}
#[test]
fn terminal_overlay_expiry_publishes_a_clear_but_keeps_action_results() {
    let (controller, screen) = setup();
    submit(&controller, &screen, click());
    let mut running = None;
    drive(&controller, &mut running, &screen);
    finish(&controller, &mut running, &screen);
    let before = controller.status();
    assert!(before.feedback.as_ref().unwrap().overlay_visible);
    controller
        .inner
        .lock()
        .unwrap()
        .feedback
        .as_mut()
        .unwrap()
        .visible_until = Some(Instant::now());
    let after = controller.status();
    assert_ne!(before, after);
    assert!(!after.feedback.as_ref().unwrap().overlay_visible);
    assert_eq!(after.feedback.unwrap().phase, Phase::Completed);
    assert_eq!(controller.status(), controller.status());
}
#[test]
fn drag_path_tracks_only_actual_writes_and_stays_bounded() {
    let (controller, screen) = setup();
    submit(
        &controller,
        &screen,
        Gesture::Drag {
            start: Point { x: 199, y: 99 },
            end: Point { x: 0, y: 0 },
            duration_ms: 3000,
        },
    );
    assert!(controller.status().feedback.unwrap().path.is_empty());
    let mut running = None;
    drive(&controller, &mut running, &screen);
    assert_eq!(controller.status().feedback.unwrap().path.len(), 1);
    let sent = finish(&controller, &mut running, &screen);
    assert!(
        sent.iter()
            .any(|e| e.kind == 5 && e.x_or_key == 0 && e.y == 0)
    );
    let feedback = controller.status().feedback.unwrap();
    assert_eq!(feedback.path.len(), 61);
    assert_eq!(feedback.path.last(), Some(&Point { x: 0, y: 0 }));
    assert_eq!(feedback.phase, Phase::Completed);
}
#[test]
fn worker_write_failure_is_uncertain_never_retried_and_does_not_reset_global_toggle() {
    let (controller, screen) = setup();
    submit(&controller, &screen, click());
    let mut running = None;
    assert_eq!(
        controller.drive(&mut running, &screen, 7, |_| Err("partial write")),
        Err("partial write")
    );
    drop(running);
    let feedback = controller.status().feedback.unwrap();
    assert_eq!(feedback.phase, Phase::Uncertain);
    assert!(!feedback.release_written);
    assert!(controller.status().enabled);
    assert!(drive(&controller, &mut None, &screen).is_empty());
}
#[test]
fn off_then_on_during_write_cannot_resurrect_the_cancelled_gesture() {
    let (controller, screen) = setup();
    submit(&controller, &screen, click());
    let mut running = None;
    controller
        .drive(&mut running, &screen, 7, |_| {
            assert_eq!(controller.status().feedback.unwrap().written_steps, 0);
            controller.set_enabled(false);
            controller.set_enabled(true);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(
        drive(&controller, &mut running, &screen),
        [InputEvent::RELEASE_ALL]
    );
    assert_eq!(
        controller.status().feedback.unwrap().phase,
        Phase::Cancelled
    );
    assert!(controller.status().enabled);
}
#[test]
fn local_takeover_preserves_permission_but_blocks_new_input_until_released() {
    let (controller, screen) = setup();
    submit(&controller, &screen, click());
    let mut running = None;
    drive(&controller, &mut running, &screen);
    controller.local_input(&[input::pointer(4, 0, (1, 1), 0)]);
    assert_eq!(
        drive(&controller, &mut running, &screen),
        [InputEvent::RELEASE_ALL]
    );
    assert!(controller.status().enabled);
    assert!(
        controller
            .submit(
                &screen,
                7,
                (200, 100),
                click(),
                controller.epoch(),
                expires()
            )
            .is_err()
    );
    controller.local_written();
    assert!(
        controller
            .submit(
                &screen,
                7,
                (200, 100),
                click(),
                controller.epoch(),
                expires()
            )
            .is_err()
    );
    controller.local_input(&[input::pointer(5, 0, (1, 1), 0)]);
    assert!(
        controller
            .submit(
                &screen,
                7,
                (200, 100),
                click(),
                controller.epoch(),
                expires()
            )
            .is_err()
    );
    controller.local_written();
    submit(&controller, &screen, click());
}
