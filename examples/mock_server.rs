//! Local synthetic test peer, not an MPC-side server replacement.
//! Binds loopback only, uses a fixed fake password, and never contacts a device.
use hmac::{Hmac, Mac};
use mpc_remote_screen_viewer_gpui::{auth::Credentials, protocol::*};
use sha2::Sha256;
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

fn send(socket: &mut TcpStream, message: Message) -> io::Result<()> {
    socket.write_all(&message.encode().map_err(io::Error::other)?)
}
fn receive(
    socket: &mut TcpStream,
    assembler: &mut Assembler,
    pending: &mut VecDeque<Message>,
) -> io::Result<Option<Message>> {
    if let Some(message) = pending.pop_front() {
        return Ok(Some(message));
    }
    let mut bytes = [0; 4096];
    match socket.read(&mut bytes) {
        Ok(0) => {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client disconnected",
            ));
        }
        Ok(n) => pending.extend(assembler.feed(&bytes[..n]).map_err(io::Error::other)?),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) => {}
        Err(e) => return Err(e),
    }
    Ok(pending.pop_front())
}
fn serve(mut socket: TcpStream, offer_audio: bool) -> io::Result<()> {
    socket.set_read_timeout(Some(Duration::from_millis(20)))?;
    socket.set_write_timeout(Some(Duration::from_secs(1)))?;
    socket.set_nodelay(true)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let mut hello = b"RSCR\x02\x00\x01\x00".to_vec();
    hello.extend_from_slice(&nonce);
    // Exercise the client's incremental framing on an ordinary TCP socket.
    for byte in Message::new(Kind::Hello, hello).encode().unwrap() {
        socket.write_all(&[byte])?;
    }
    let mut assembler = Assembler::default();
    let mut pending = VecDeque::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let auth = loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Auth deadline"));
        }
        if let Some(message) = receive(&mut socket, &mut assembler, &mut pending)? {
            if message.kind != Kind::Auth {
                return Err(io::Error::other("expected Auth"));
            }
            break Auth::parse(&message.payload).map_err(io::Error::other)?;
        }
    };
    let expected = Credentials::derive("demo").client_auth(&nonce, auth.nonce);
    if auth.proof != expected.proof {
        let mut reason = vec![0];
        put_short_string(&mut reason, "Use the mock password: demo");
        send(&mut socket, Message::new(Kind::AuthResult, reason))?;
        return Ok(());
    }
    let mut key = [0; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(b"demo", b"remote-screen/v1", 10000, &mut key);
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(b"RSCR-server");
    mac.update(&nonce);
    mac.update(&auth.nonce);
    let mut success = vec![1];
    success.extend_from_slice(&mac.finalize().into_bytes());
    let mut coalesced = Message::new(Kind::AuthResult, success).encode().unwrap();
    let mut init = Vec::new();
    init.extend_from_slice(&320u16.to_le_bytes());
    init.extend_from_slice(&200u16.to_le_bytes());
    init.extend_from_slice(&[32, 16, 8, 8, 8, 0, 8]);
    put_short_string(&mut init, "Synthetic loopback screen");
    init.extend_from_slice(&[0, 0, u8::from(offer_audio)]); // No remote input.
    coalesced.extend(Message::new(Kind::ServerInit, init).encode().unwrap());
    socket.write_all(&coalesced)?;
    let mut info = Vec::new();
    for text in ["Mock MPC", "SYNTHETIC", "not real firmware"] {
        put_short_string(&mut info, text);
    }
    send(&mut socket, Message::new(Kind::DeviceInfo, info))?;
    let mut frame_id = 0u32;
    let mut next = Instant::now();
    let mut fps = 15u8;
    let mut awaiting_ack = false;
    let mut audio_on = false;
    let mut audio_index = 0u64;
    let mut audio_due = Instant::now();
    loop {
        if let Some(message) = receive(&mut socket, &mut assembler, &mut pending)? {
            match message.kind {
                Kind::Ping => send(
                    &mut socket,
                    token(
                        Kind::Pong,
                        parse_token(&message.payload).map_err(io::Error::other)?,
                    ),
                )?,
                Kind::AudioControl if offer_audio && message.payload.len() == 2 => {
                    audio_on = message.payload[0] != 0 && message.payload[1] & 1 != 0;
                    if audio_on {
                        audio_index = 0;
                        audio_due = Instant::now();
                        let mut format = 48000u32.to_le_bytes().to_vec();
                        format.extend([1, 0]); // mono PCM16
                        send(&mut socket, Message::new(Kind::AudioFormat, format))?;
                    }
                }
                Kind::Ack => awaiting_ack = false,
                Kind::RequestFullRefresh => {
                    next = Instant::now();
                    awaiting_ack = false;
                }
                Kind::SetOptions if !message.payload.is_empty() => {
                    fps = match message.payload[0] {
                        0 => 15,
                        f => f.clamp(1, 120),
                    }
                }
                _ => {}
            }
        }
        // Explicit --audio offers a quiet synthetic 440 Hz tone. Source positions
        // follow the clock, not the screen ACK rate; never queue unlimited catch-up.
        if audio_on {
            for _ in 0..4 {
                if Instant::now() < audio_due {
                    break;
                }
                let mut packet = audio_index.to_le_bytes().to_vec();
                packet.extend(960u16.to_le_bytes());
                for i in 0..960 {
                    let phase = (audio_index + i) as f64 * 440.0 * std::f64::consts::TAU / 48000.0;
                    packet.extend(((phase.sin() * 3276.0) as i16).to_le_bytes());
                }
                send(&mut socket, Message::new(Kind::AudioPacket, packet))?;
                audio_index += 960;
                audio_due += Duration::from_millis(20);
            }
            if Instant::now() > audio_due {
                let skipped = Instant::now().duration_since(audio_due).as_millis() as u64 / 20 + 1;
                audio_index += skipped * 960;
                audio_due += Duration::from_millis(skipped * 20);
            }
        }
        if !awaiting_ack && Instant::now() >= next {
            let mut packed = Vec::with_capacity(320 * 200 * 4);
            for y in 0..200u32 {
                for x in 0..320u32 {
                    packed.extend_from_slice(&[
                        (x * 255 / 319) as u8,
                        (y * 255 / 199) as u8,
                        frame_id.wrapping_mul(3) as u8,
                        0,
                    ]);
                }
            }
            let compressed = lz4_flex::block::compress(&packed);
            let mut frame = frame_id.to_le_bytes().to_vec();
            frame.push(0);
            frame.extend_from_slice(&1u16.to_le_bytes());
            for value in [0u16, 0, 320, 200] {
                frame.extend_from_slice(&value.to_le_bytes());
            }
            frame.extend_from_slice(&[1, 0]);
            frame.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            frame.extend(compressed);
            send(&mut socket, Message::new(Kind::FrameUpdate, frame))?;
            frame_id = frame_id.wrapping_add(1);
            awaiting_ack = true;
            next = Instant::now() + Duration::from_secs_f64(1.0 / f64::from(fps));
        }
    }
}
fn main() -> io::Result<()> {
    let mut port = 17447;
    let mut offer_audio = false;
    for arg in std::env::args().skip(1) {
        if arg == "--audio" {
            offer_audio = true;
        } else {
            port = arg.parse::<u16>().map_err(io::Error::other)?;
        }
    }
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!(
        "Synthetic loopback peer at {} (fake password: demo)",
        listener.local_addr()?
    );
    if offer_audio {
        println!("Audio opt-in available: mono 48 kHz PCM16, 440 Hz tone at about -20 dBFS.");
    }
    for socket in listener.incoming() {
        match serve(socket?, offer_audio) {
            Ok(()) => {}
            Err(e) => eprintln!("Mock session ended: {e}"),
        }
    }
    Ok(())
}
