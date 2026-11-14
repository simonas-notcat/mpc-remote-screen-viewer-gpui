use mpc_remote_screen_viewer_gpui::{audio, protocol::*};

fn decode(codec: u8, channels: u8, frames: u16, encoded: &[u8]) -> Result<Vec<f32>> {
    let format = AudioFormat {
        sample_rate: 48000,
        channels,
        codec,
    };
    let mut payload = vec![0; 8];
    payload.extend_from_slice(&frames.to_le_bytes());
    payload.extend_from_slice(encoded);
    audio::decode(format, &AudioPacket::parse(&payload, format)?)
}
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    position: usize,
}
impl BitWriter {
    fn put(&mut self, value: u32, n: usize) {
        for bit in 0..n {
            if self.position.is_multiple_of(8) {
                self.bytes.push(0);
            }
            self.bytes[self.position / 8] |= (((value >> bit) & 1) as u8) << (self.position % 8);
            self.position += 1;
        }
    }
    fn residual(&mut self, residual: i32, k: u8, force_escape: bool) {
        let u = ((residual << 1) ^ (residual >> 31)) as u32;
        let q = u >> k;
        if force_escape || q >= 24 {
            self.put(0xffffff, 24);
            self.put(u, 21);
        } else {
            self.put((1 << q) - 1, q as usize);
            self.put(0, 1);
            self.put(u & ((1 << k) - 1), k as usize);
        }
    }
}
fn channel(samples: &[i16], order: usize, k: u8, force_escape: bool) -> Vec<u8> {
    let mut block = Vec::new();
    for s in &samples[..order] {
        block.extend_from_slice(&s.to_le_bytes());
    }
    let mut bits = BitWriter::default();
    for i in order..samples.len() {
        let predicted = match order {
            0 => 0,
            1 => i32::from(samples[i - 1]),
            2 => 2 * i32::from(samples[i - 1]) - i32::from(samples[i - 2]),
            3 => {
                3 * i32::from(samples[i - 1]) - 3 * i32::from(samples[i - 2])
                    + i32::from(samples[i - 3])
            }
            _ => panic!(),
        };
        bits.residual(
            i32::from(samples[i]) - predicted,
            k,
            force_escape && i == order,
        );
    }
    block.extend(bits.bytes);
    let mut out = vec![(order as u8) << 4 | k];
    out.extend_from_slice(&(block.len() as u16).to_le_bytes());
    out.extend(block);
    out
}
#[test]
fn synthetic_rice_all_orders_parameters_signed_residuals_and_escape() {
    let samples = [-2, -1, 0, 1, 2, 0, -1, -2, 3, 2, 0, -3, 4, 3, 2, 1];
    for order in 0..=3 {
        for k in 0..=15 {
            let encoded = channel(&samples, order, k, false);
            let decoded = decode(2, 1, samples.len() as u16, &encoded).unwrap();
            assert_eq!(decoded, samples.map(|s| f32::from(s) / 32768.0));
        }
    }
    let escaped = channel(&[100, 0, 0, 0], 0, 0, true);
    assert_eq!(
        decode(2, 1, 4, &escaped).unwrap(),
        [100.0 / 32768.0, 0.0, 0.0, 0.0]
    );
    let left = channel(&[1, 2, 3, 4], 1, 0, false);
    let right = channel(&[-1, -2, -3, -4], 2, 0, false);
    let mut stereo = left;
    stereo.extend(right);
    assert_eq!(
        decode(2, 2, 4, &stereo).unwrap(),
        [1, -1, 2, -2, 3, -3, 4, -4].map(|s| s as f32 / 32768.0)
    );
}
#[test]
fn lossless_truncated_controls_overflow_and_unused_bytes() {
    for control in [0x40, 0x41, 0x81, 0xff, 0x10] {
        // order 1 is invalid for a one-frame packet.
        assert!(decode(2, 1, 1, &[control, 1, 0, 0]).is_err());
    }
    assert!(decode(2, 1, 4, &[0, 1, 0, 255]).is_err()); // Unterminated unary/escape.
    assert!(decode(2, 1, 1, &[0, 2, 0, 0, 0]).is_err()); // Entire unused residual byte.
    assert!(decode(2, 1, 1, &[0x80, 1, 0, 0]).is_err()); // Short raw block.
    let mut overflow = channel(&[0; 4], 0, 0, false);
    let mut bits = BitWriter::default();
    bits.residual(32768, 0, true);
    for _ in 0..3 {
        bits.residual(0, 0, false);
    }
    overflow.truncate(1);
    overflow.extend_from_slice(&(bits.bytes.len() as u16).to_le_bytes());
    overflow.extend(bits.bytes);
    assert!(decode(2, 1, 4, &overflow).is_err());
    let valid = channel(&[1, 2, 3, 4], 1, 1, false);
    for n in 0..valid.len() {
        assert!(decode(2, 1, 4, &valid[..n]).is_err());
    }
}
#[test]
fn adpcm_padding_packet_state_and_predictor_saturation() {
    // Three scalar nibbles, so the unused high nibble must be ignored.
    let one = decode(
        1,
        3,
        2,
        &[0, 0, 0, 0, 100, 0, 0, 0, 200, 0, 0, 0, 0x11, 0xf1],
    )
    .unwrap();
    let two = decode(
        1,
        3,
        2,
        &[0, 0, 0, 0, 100, 0, 0, 0, 200, 0, 0, 0, 0x11, 0x01],
    )
    .unwrap();
    assert_eq!(one, two);
    assert_eq!(one, [0, 100, 200, 1, 101, 201].map(|s| s as f32 / 32768.0));
    assert_eq!(
        decode(1, 1, 2, &[255, 127, 88, 0, 7]).unwrap(),
        [32767.0 / 32768.0; 2]
    );
    assert_eq!(decode(1, 1, 2, &[0, 128, 88, 0, 15]).unwrap(), [-1.0; 2]);
    assert!(decode(1, 1, 1, &[0, 0, 89, 0]).is_err());
    assert_eq!(
        decode(1, 1, 2, &[0, 0, 0, 255, 1]).unwrap(),
        decode(1, 1, 2, &[0, 0, 0, 0, 1]).unwrap()
    );
}
#[test]
fn audio_limits_and_source_position_overflow() {
    assert!(decode(0, 0, 1, &[]).is_err());
    assert!(decode(0, 9, 1, &[]).is_err());
    assert!(decode(0, 1, 0, &[]).is_err());
    assert!(decode(0, 1, 8193, &[]).is_err());
    let mut p = u64::MAX.to_le_bytes().to_vec();
    p.extend_from_slice(&[1, 0, 0, 0]);
    assert!(
        AudioPacket::parse(
            &p,
            AudioFormat {
                sample_rate: 8000,
                channels: 1,
                codec: 0
            }
        )
        .is_err()
    );
}
