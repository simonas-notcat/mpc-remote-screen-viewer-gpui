use mpc_remote_screen_viewer_gpui::{
    audio,
    auth::Credentials,
    framebuffer::{Framebuffer, expand_quantized, packed_to_rgba},
    protocol::*,
};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/protocol-v2-vectors.json")).unwrap()
}
fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2));
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn fixture(name: &str) -> Message {
    let v = vectors();
    let v = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap();
    Message::new(
        Kind::try_from(v["type"].as_u64().unwrap() as u8).unwrap(),
        hex(v["payload_hex"].as_str().unwrap()),
    )
}

#[test]
fn every_fixture_message_and_fragment_boundary() {
    let v = vectors();
    let mut combined = Vec::new();
    let mut expected = Vec::new();
    for v in v["messages"].as_array().unwrap() {
        let bytes = hex(v["message_hex"].as_str().unwrap());
        let message = fixture(v["name"].as_str().unwrap());
        assert_eq!(message.encode().unwrap(), bytes);
        for split in 0..=bytes.len() {
            let mut assembler = Assembler::default();
            let mut messages = assembler.feed(&bytes[..split]).unwrap();
            messages.extend(assembler.feed(&bytes[split..]).unwrap());
            assert_eq!(messages, std::slice::from_ref(&message));
            assert_eq!(assembler.buffered_bytes(), 0);
        }
        combined.extend(bytes);
        expected.push(message);
    }
    assert_eq!(Assembler::default().feed(&combined).unwrap(), expected);
    let mut assembler = Assembler::default();
    let bytewise: Vec<_> = combined
        .iter()
        .flat_map(|b| assembler.feed(&[*b]).unwrap())
        .collect();
    assert_eq!(bytewise, expected);
}
#[test]
fn envelope_poison_and_unreceived_allocation() {
    for bad in [
        vec![0],
        vec![17],
        vec![5, 1, 0, 0, 4],
        vec![5, 255, 255, 255, 255],
    ] {
        let mut assembler = Assembler::default();
        assert!(assembler.feed(&bad).is_err());
        assert_eq!(assembler.feed(&[7, 0, 0, 0, 0]), Err(Error::Poisoned));
    }
    let mut assembler = Assembler::default();
    assert!(assembler.feed(&[5, 0, 0, 0, 4]).unwrap().is_empty());
    assert_eq!(assembler.buffered_bytes(), 5);
}
#[test]
fn hello_auth_proofs_and_legacy_auth() {
    let hello = Hello::parse(&fixture("hello_password").payload).unwrap();
    assert_eq!(hello.flags, 1);
    let credentials = Credentials::derive("example-password");
    let a = fixture("auth_all_codecs");
    let auth = Auth::parse(&a.payload).unwrap();
    assert_eq!(
        credentials.client_auth(&hello.nonce, auth.nonce).message(),
        a
    );
    let AuthResult::Success(proof) = AuthResult::parse(&fixture("auth_success").payload).unwrap()
    else {
        panic!()
    };
    assert!(credentials.verify_server(&hello.nonce, &auth.nonce, &proof));
    assert!(!credentials.verify_server(&auth.nonce, &hello.nonce, &proof));
    assert!(!Credentials::derive("wrong").verify_server(&hello.nonce, &auth.nonce, &proof));
    let mut altered = proof;
    altered[17] ^= 1;
    assert!(!credentials.verify_server(&hello.nonce, &auth.nonce, &altered));
    assert_eq!(
        Auth::parse(&a.payload[..48]).unwrap().codec_mask,
        VIDEO_CODECS
    );
    assert_eq!(Auth::parse(&a.payload[..49]).unwrap().extension, 0);
    for n in 0..48 {
        assert!(Auth::parse(&a.payload[..n]).is_err());
    }
    assert!(AuthResult::parse(&[1]).is_err());
    assert!(
        matches!(AuthResult::parse(&fixture("auth_failure").payload).unwrap(), AuthResult::Rejected(s) if s == "authentication failed")
    );
    let mut wrong = fixture("hello_password").payload;
    wrong[4] = 1;
    assert_eq!(Hello::parse(&wrong), Err(Error::Version(1)));
    wrong[0] = 0;
    assert!(Hello::parse(&wrong).is_err());
    assert_ne!(
        Credentials::derive("")
            .client_auth(&hello.nonce, auth.nonce)
            .proof,
        auth.proof
    );
    let first = credentials.fresh_auth(&hello.nonce).unwrap();
    let second = credentials.fresh_auth(&hello.nonce).unwrap();
    assert_ne!(first.nonce, second.nonce);
    let debug = format!("{a:?}");
    assert!(!debug.contains("99d286"));
    assert!(!debug.contains("153, 210"));
}
#[test]
fn screen_fixture_composition_and_configuration() {
    let m = fixture("server_init_2x2");
    let init = ServerInit::parse(&m.payload).unwrap();
    assert_eq!(
        (
            init.width,
            init.height,
            init.rotation,
            init.input_capabilities,
            init.audio_available
        ),
        (2, 2, 0, 7, true)
    );
    assert!(
        !ServerInit::parse(&m.payload[..m.payload.len() - 1])
            .unwrap()
            .audio_available
    );
    assert!(ServerInit::parse(&m.payload[..m.payload.len() - 2]).is_err());
    let mut framebuffer = Framebuffer::new(init).unwrap();
    let message = fixture("frame_raw_and_solid");
    let mut frame = FrameUpdate::parse(&message.payload).unwrap();
    let report = framebuffer.apply(&frame);
    assert_eq!(
        (report.id, report.quality, report.applied, report.skipped),
        (42, 0, 2, 0)
    );
    let expected = hex(vectors()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "frame_raw_and_solid")
        .unwrap()["expected"]["rgba_hex"]
        .as_str()
        .unwrap());
    assert_eq!(framebuffer.native_rgba(), expected);
    frame.rectangles.clear();
    assert_eq!(framebuffer.apply(&frame).applied, 0);
    assert_eq!(framebuffer.native_rgba(), expected);
    assert_eq!(ack(frame.id), fixture("ack_42"));
    let mut invalid = m.payload.clone();
    for index in [0, 1, 2, 3] {
        invalid[index] = 0;
    }
    assert!(ServerInit::parse(&invalid).is_err());
    let mut invalid = m.payload.clone();
    invalid[5] = 255;
    assert!(ServerInit::parse(&invalid).is_err());
    let mut huge = framebuffer.configuration().clone();
    huge.width = u16::MAX;
    huge.height = u16::MAX;
    assert!(Framebuffer::new(huge).is_err());
}
#[test]
fn scalar_strings_options_input_and_metadata() {
    assert_eq!(options(15), fixture("set_options_15fps"));
    assert_eq!(options(255).payload, [120, 31]);
    assert_eq!(audio_control(true), fixture("audio_enable_all"));
    for kind in [Kind::Ping, Kind::Pong] {
        let msg = fixture(if kind == Kind::Ping { "ping" } else { "pong" });
        assert_eq!(parse_token(&msg.payload).unwrap(), 123456789);
        assert_eq!(token(kind, 123456789), msg);
    }
    assert_eq!(parse_reason(&fixture("bye").payload).unwrap(), "closing");
    assert!(parse_reason(&[2, b'x']).is_err());
    assert_eq!(parse_reason(&[1, 255]).unwrap(), "\u{fffd}");
    let mut text = Vec::new();
    put_short_string(&mut text, &"é".repeat(200));
    assert_eq!(text.len(), 256); // Byte limit, not Unicode scalar count.
    let info = DeviceInfo::parse(&fixture("device_info").payload).unwrap();
    assert_eq!(
        (
            info.model.as_str(),
            info.serial.as_str(),
            info.firmware.as_str()
        ),
        ("MPC", "TEST-SERIAL", "test-firmware")
    );
    for name in [
        "mouse_move_down_up_release",
        "uppercase_A",
        "wheel_negative_one",
    ] {
        let msg = fixture(name);
        let events = InputEvent::parse_batch(&msg.payload).unwrap();
        assert_eq!(InputEvent::messages(&events).unwrap(), [msg]);
    }
    assert_eq!(
        InputEvent::parse_batch(&fixture("wheel_negative_one").payload).unwrap()[0].value,
        -1
    );
    let batches = InputEvent::messages(&[InputEvent::RELEASE_ALL; 129]).unwrap();
    assert_eq!(
        batches.iter().map(|m| m.payload[0]).collect::<Vec<_>>(),
        [64, 64, 1]
    );
    assert!(InputEvent::parse_batch(&[65]).is_err());
    let mut invalid = InputEvent::RELEASE_ALL;
    invalid.kind = 10;
    assert!(InputEvent::messages(&[invalid]).is_err());
}
#[test]
fn audio_formats_packets_and_decoded_fixture_samples() {
    for (name, codec) in [
        ("audio_format_pcm16", 0),
        ("audio_format_ima_adpcm", 1),
        ("audio_format_lossless", 2),
    ] {
        let f = AudioFormat::parse(&fixture(name).payload).unwrap();
        assert_eq!(
            f,
            AudioFormat {
                sample_rate: 48000,
                channels: 2,
                codec
            }
        );
    }
    for v in vectors()["messages"].as_array().unwrap() {
        if v["type"] != 15 {
            continue;
        }
        let e = &v["expected"];
        let format = AudioFormat {
            sample_rate: 48000,
            channels: e["channels"].as_u64().unwrap() as u8,
            codec: e["codec"].as_u64().unwrap() as u8,
        };
        let message = fixture(v["name"].as_str().unwrap());
        let packet = AudioPacket::parse(&message.payload, format).unwrap();
        assert_eq!(
            packet.first_sample_index,
            e["first_sample_index"].as_u64().unwrap()
        );
        assert_eq!(u64::from(packet.frames), e["frames"].as_u64().unwrap());
        let expected: Vec<f32> = e["samples_f32"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        assert_eq!(audio::decode(format, &packet).unwrap(), expected);
        for n in 0..10 {
            assert!(AudioPacket::parse(&message.payload[..n], format).is_err());
        }
    }
    for payload in [&[0; 6][..], &[0; 5], &[0; 7]] {
        assert!(AudioFormat::parse(payload).is_err());
    }
}
#[test]
fn pixel_and_quantization_vectors() {
    for v in vectors()["pixel_cases"].as_array().unwrap() {
        if let Some(format) = v["format"].as_array() {
            let f = PixelFormat(
                format
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(
                packed_to_rgba(&hex(v["pixel_hex"].as_str().unwrap()), f, 0).unwrap(),
                hex(v["rgba_hex"].as_str().unwrap())
            );
        } else {
            assert_eq!(
                expand_quantized(
                    v["input_channel"].as_u64().unwrap() as u32,
                    v["channel_bits"].as_u64().unwrap() as u8,
                    v["drop_bits"].as_u64().unwrap() as u8
                ),
                v["expanded_channel"].as_u64().unwrap() as u32
            );
        }
    }
}
