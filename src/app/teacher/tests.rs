use super::*;
use mpc_remote_screen_viewer_gpui::{
    control::{Controller, Gesture, Point},
    framebuffer::Framebuffer,
    protocol::{PixelFormat, ServerInit},
};
use std::sync::RwLock;

fn snapshot() -> Snapshot {
    let screen = ServerInit {
        width: 200,
        height: 100,
        rotation: 0,
        pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
        name: "Synthetic device".into(),
        input_capabilities: 7,
        audio_available: false,
    };
    Snapshot {
        state: State::Streaming,
        framebuffer: Some(Arc::new(RwLock::new(
            Framebuffer::new(screen.clone()).unwrap(),
        ))),
        screen: Some(screen),
        ..Snapshot::default()
    }
}
fn status() -> ControlStatus {
    Controller::default().status()
}

#[test]
fn settings_require_sharing_a_streaming_source_and_supported_input() {
    let mut snapshot = snapshot();
    let status = status();
    let read_only = assisted_settings(&snapshot, false, false, Some(&status));
    assert!(!read_only.lease_active);
    assert!(read_only.switch_disabled());
    assert_eq!(
        read_only.unavailable,
        Some("Enable AI teacher sharing first.")
    );

    assert!(
        assisted_settings(&snapshot, true, false, None)
            .unavailable
            .is_some()
    );
    snapshot.state = State::Connecting;
    assert_eq!(
        assisted_settings(&snapshot, true, false, Some(&status)).unavailable,
        Some("Connect to a streaming device first.")
    );
    snapshot.state = State::Streaming;
    let frame = snapshot.framebuffer.take();
    assert!(
        assisted_settings(&snapshot, true, false, Some(&status))
            .unavailable
            .is_some()
    );
    snapshot.framebuffer = frame;
    for caps in [0, 8, 128] {
        snapshot.screen.as_mut().unwrap().input_capabilities = caps;
        assert!(
            assisted_settings(&snapshot, true, false, Some(&status))
                .unavailable
                .is_some()
        );
    }
    snapshot.screen = None;
    assert!(
        assisted_settings(&snapshot, true, false, Some(&status))
            .unavailable
            .is_some()
    );
}

#[test]
fn settings_allow_supported_devices_but_block_held_local_input() {
    let mut snapshot = snapshot();
    let mut status = status();
    for caps in [1, 2, 4, 7] {
        snapshot.screen.as_mut().unwrap().input_capabilities = caps;
        let access = assisted_settings(&snapshot, true, false, Some(&status));
        assert!(!access.lease_active);
        assert!(access.unavailable.is_none());
        assert!(!access.switch_disabled());
    }
    assert!(
        assisted_settings(&snapshot, true, true, Some(&status))
            .unavailable
            .is_some()
    );
    status.human_held = true;
    assert!(
        assisted_settings(&snapshot, true, false, Some(&status))
            .unavailable
            .is_some()
    );
}

#[test]
fn settings_reflect_live_lease_and_keep_off_available_for_pending_gestures() {
    let snapshot = snapshot();
    let frame = snapshot.framebuffer.as_ref().unwrap();
    let control = Controller::default();
    // Merely rendering Settings must not grant anything.
    assert!(!assisted_settings(&snapshot, true, false, Some(&control.status())).lease_active);
    control.grant(frame).unwrap();
    let access = assisted_settings(&snapshot, true, false, Some(&control.status()));
    assert!(access.lease_active);
    assert!(access.unavailable.is_none());
    control
        .propose(
            frame,
            7,
            (200, 100),
            Gesture::Click {
                target: Point { x: 1, y: 1 },
                hold_ms: 100,
            },
            control.epoch(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    let access = assisted_settings(&snapshot, true, false, Some(&control.status()));
    assert!(access.lease_active);
    assert!(access.unavailable.is_some());
    assert!(!access.switch_disabled()); // OFF stays available while a new grant is blocked.
    assert_eq!(
        control.status().feedback.unwrap().phase,
        Phase::PendingApproval
    );
    assert_eq!(control.status().feedback.unwrap().written_steps, 0);
    control.revoke("Settings switch off");
    let access = assisted_settings(&snapshot, true, false, Some(&control.status()));
    assert!(!access.lease_active);
    assert!(access.unavailable.is_none());
    assert_eq!(control.status().feedback.unwrap().phase, Phase::Cancelled);
    // Stale cached controller state must not show enabled after sharing is disabled.
    let stale = ControlStatus {
        lease_active: true,
        human_held: false,
        feedback: None,
    };
    assert!(!assisted_settings(&snapshot, false, false, Some(&stale)).lease_active);
}
