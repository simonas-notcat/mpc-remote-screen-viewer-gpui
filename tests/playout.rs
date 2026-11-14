use mpc_remote_screen_viewer_gpui::{
    audio::Audio,
    playout::{Playout, cubic, peak_db},
    protocol::AudioFormat,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

struct CountAlloc;
thread_local! {
    static IN_CALLBACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if IN_CALLBACK.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|a| a.set(a.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if IN_CALLBACK.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|a| a.set(a.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
fn no_alloc(f: impl FnOnce()) {
    ALLOCATIONS.set(0);
    IN_CALLBACK.set(true);
    f();
    IN_CALLBACK.set(false);
    assert_eq!(ALLOCATIONS.get(), 0, "allocation inside render callback");
}
fn format(rate: u32, channels: u8) -> AudioFormat {
    AudioFormat {
        sample_rate: rate,
        channels,
        codec: 0,
    }
}
fn render(q: &mut Playout, frames: usize, channels: usize, rate: u32) -> Vec<f32> {
    let mut output = vec![0.0; frames * channels];
    no_alloc(|| q.render(frames, channels, rate, |i, v| output[i] = v));
    output
}
fn packet(first: u64, frames: u16, value: i16) -> Vec<u8> {
    let mut payload = first.to_le_bytes().to_vec();
    payload.extend(frames.to_le_bytes());
    for _ in 0..frames {
        payload.extend(value.to_le_bytes());
    }
    payload
}
#[test]
fn prefill_startup_tail_fades_underflow_and_refill() {
    let mut q = Playout::new(format(8000, 1)).unwrap();
    q.push(0, &[1.0; 639]).unwrap();
    assert!(render(&mut q, 80, 1, 8000).iter().all(|v| *v == 0.0));
    assert!(!q.stats().playing);
    q.push(639, &[1.0]).unwrap();
    let output = render(&mut q, 650, 1, 8000);
    assert_eq!(output[0], 0.0);
    assert!((output[20] - 0.5).abs() < 0.00001);
    assert_eq!(output[40], 1.0);
    assert!((output[620] - 0.5).abs() < 0.00001);
    assert!(output[640..].iter().all(|v| *v == 0.0));
    assert_eq!(q.stats().underflows, 1);
    q.push(640, &[0.5; 640]).unwrap();
    let output = render(&mut q, 80, 1, 8000);
    assert_eq!(output[0], 0.0);
    assert_eq!(output[40], 0.5);
    assert!(q.stats().playing);
}
#[test]
fn duplicates_overlap_reordering_and_gap_markers() {
    let mut q = Playout::new(format(8000, 1)).unwrap();
    assert_eq!(q.push(100, &[1.0; 640]).unwrap(), 640);
    assert_eq!(q.push(100, &[1.0; 640]).unwrap(), 0);
    assert_eq!(q.push(700, &[1.0; 80]).unwrap(), 40);
    assert_eq!(q.push(900, &[-1.0; 640]).unwrap(), 640);
    assert_eq!(q.push(800, &[0.25; 20]).unwrap(), 0);
    let output = render(&mut q, 900, 1, 8000);
    assert_eq!(q.stats().discontinuities, 1);
    assert!(output[660].abs() < 0.6); // fade approaching the marker
    assert!(output[680].abs() < 0.05); // fresh ramp, not a hard +1 -> -1 edge
    assert!(output[730] < -0.99);
}
#[test]
fn ring_overflow_marks_next_accepted_packet_and_marker_queue_is_bounded() {
    let mut q = Playout::new(format(8000, 1)).unwrap();
    assert_eq!(q.push(0, &[0.5; 8188]).unwrap(), 8188);
    assert_eq!(q.push(8188, &[0.5; 10]).unwrap(), 0);
    assert_eq!(q.stats().dropped_packets, 1);
    // Excessive initial backlog starts near the target, not a second behind.
    render(&mut q, 80, 1, 8000);
    assert!((69.0..71.0).contains(&q.stats().buffered_ms));
    assert_eq!(q.push(8198, &[0.5; 800]).unwrap(), 800);
    render(&mut q, 900, 1, 8000);
    assert_eq!(q.stats().discontinuities, 1);

    let mut q = Playout::new(format(8000, 1)).unwrap();
    q.push(0, &[0.5]).unwrap();
    for i in 0..64 {
        assert_eq!(q.push(100 + i * 20, &[0.5; 10]).unwrap(), 10);
    }
    // Hardened behavior: never silently lose a marker when all 64 slots are used.
    assert_eq!(q.push(2000, &[0.5; 10]).unwrap(), 0);
    assert_eq!(q.stats().dropped_packets, 1);
    render(&mut q, 700, 1, 8000);
    assert_eq!(q.stats().discontinuities, 64);
}
#[test]
fn running_latency_trim_fades_and_returns_toward_target() {
    let mut q = Playout::new(format(8000, 1)).unwrap();
    q.push(0, &[1.0; 640]).unwrap();
    render(&mut q, 80, 1, 8000);
    q.push(640, &[1.0; 4000]).unwrap();
    let output = render(&mut q, 80, 1, 8000);
    assert_eq!(q.stats().latency_trims, 1);
    assert!(output[0] > 0.99);
    assert!(output[39] < 0.05);
    assert!(output[41] < 0.05);
    assert!(q.stats().buffered_ms < 81.0);
    assert!(q.stats().buffered_ms > 65.0);
}
#[test]
fn channel_mapping_mono_average_matching_and_extra_outputs_silent() {
    for (source, out, expected) in [
        (vec![0.25], 3, vec![0.25, 0.25, 0.25]),
        (vec![0.2, 0.6], 1, vec![0.4]),
        (vec![0.2, 0.6], 2, vec![0.2, 0.6]),
        (vec![0.2, 0.6], 4, vec![0.2, 0.6, 0.0, 0.0]),
        (vec![0.1, 0.2, 0.3, 0.4], 2, vec![0.1, 0.2]),
    ] {
        let mut q = Playout::new(format(8000, source.len() as u8)).unwrap();
        q.push(0, &source.repeat(640)).unwrap();
        let values = render(&mut q, 81, out, 8000);
        for (a, b) in values[80 * out..].iter().zip(expected) {
            assert!((a - b).abs() < 0.00001);
        }
    }
}
#[test]
fn cubic_is_not_linear_and_clamps_lookup_at_available_edges() {
    assert_eq!(cubic(0.0, 1.0, 0.0, 0.0, 0.5), 0.5625);
    assert_eq!(cubic(5.0, 1.0, 2.0, 7.0, 0.0), 1.0);
    assert_eq!(cubic(5.0, 1.0, 2.0, 7.0, 1.0), 2.0);
    let mut q = Playout::new(format(384000, 1)).unwrap();
    for i in 0..4 {
        q.push(i * 8192, &[0.5; 8192]).unwrap();
    }
    let values = render(&mut q, 2000, 1, 8000);
    assert!(
        values
            .iter()
            .all(|v| v.is_finite() && (0.0..=0.5).contains(v))
    );
    // Large downsampling steps may put fractional read beyond write at underflow.
    assert!(q.push(32768, &[0.5; 8192]).is_ok());
}
#[test]
fn simulated_clock_drift_and_rate_mismatches_stay_bounded() {
    for (source_rate, output_rate) in [
        (44100, 48000),
        (48000, 44100),
        (96000, 48000),
        (48000, 96000),
    ] {
        for clock in [0.999, 1.001] {
            let mut q = Playout::new(format(source_rate, 1)).unwrap();
            let initial = source_rate as usize * 80 / 1000;
            let samples = vec![0.25; 8192];
            q.push(0, &samples[..initial]).unwrap();
            let mut first = initial as u64;
            let mut fractional = 0.0;
            let frames = output_rate as usize / 100;
            let mut output = vec![0.0; frames];
            // 30 seconds per injected clock/rate pair, with no real sleeping/device.
            for _ in 0..3000 {
                fractional +=
                    f64::from(source_rate) * frames as f64 / f64::from(output_rate) * clock;
                let incoming = fractional as usize;
                fractional -= incoming as f64;
                q.push(first, &samples[..incoming]).unwrap();
                first += incoming as u64;
                no_alloc(|| q.render(frames, 1, output_rate, |i, v| output[i] = v));
            }
            let stats = q.stats();
            assert_eq!(
                stats.underflows, 0,
                "{source_rate}/{output_rate}: {stats:?}"
            );
            assert_eq!(stats.latency_trims, 0);
            assert_eq!(stats.dropped_packets, 0);
            assert!((40.0..110.0).contains(&stats.buffered_ms), "{stats:?}");
            assert!((0.996..=1.004).contains(&stats.ratio));
        }
    }
}
#[test]
fn gain_meters_format_reset_invalid_packets_and_callback_allocations() {
    let audio = Audio::default();
    audio.set_enabled(true);
    audio.configure(format(8000, 1)).unwrap();
    audio.packet(&packet(0, 640, 16384)).unwrap();
    audio.set_volume(0.5);
    let mut output = [f32::NAN; 160];
    no_alloc(|| audio.render(80, 2, 8000, |i, v| output[i] = v));
    assert_eq!(output[100], 0.125); // 0.5 signal * 0.5² volume
    assert_eq!(audio.status().input_peak, 0.5);
    assert_eq!(audio.status().output_peak, 0.125);
    assert_eq!(audio.take_peaks(), (0.5, 0.125));
    audio.set_volume(1.0);
    no_alloc(|| audio.render(80, 2, 8000, |i, v| output[i] = v));
    assert_eq!(output[100], 0.5);
    assert_eq!(audio.take_peaks(), (0.0, 0.5));
    audio.set_volume(0.0);
    no_alloc(|| audio.render(80, 2, 8000, |i, v| output[i] = v));
    assert!(output.iter().all(|v| *v == 0.0));
    assert_eq!(peak_db(audio.status().output_peak), -120.0);
    assert_eq!(peak_db(f32::NAN), -120.0);
    assert!(audio.packet(&[0; 5]).is_err());
    assert!(audio.status().error.is_some());
    audio.configure(format(48000, 2)).unwrap();
    assert_eq!(audio.status().stats.buffered_ms, 0.0);
    assert!(audio.status().error.is_none());
    no_alloc(|| audio.render(80, 2, 8000, |i, v| output[i] = v));
    assert!(output.iter().all(|v| *v == 0.0));
    audio.reset();
    assert!(audio.status().format.is_none());
    audio.set_enabled(false);
    no_alloc(|| audio.render(80, 2, 8000, |i, v| output[i] = v));
    assert!(output.iter().all(|v| *v == 0.0));
}
#[test]
fn configurable_target_controls_prefill_and_clamps_to_fade_safe_limits() {
    for target in [10, 20, 80, 300] {
        let mut q = Playout::new(format(8000, 1)).unwrap();
        no_alloc(|| q.set_target_buffer_ms(target));
        assert_eq!(q.stats().target_buffer_ms, target);
        let frames = target as usize * 8;
        q.push(0, &vec![0.5; frames - 1]).unwrap();
        assert!(render(&mut q, 16, 1, 8000).iter().all(|v| *v == 0.0));
        assert!(!q.stats().playing);
        q.push((frames - 1) as u64, &[0.5]).unwrap();
        let output = render(&mut q, 16, 1, 8000);
        assert!(output[10] > 0.0);
        assert!(q.stats().playing);

        // A restored preference must also govern the very first source prefill.
        let audio = Audio::default();
        audio.set_target_buffer_ms(target);
        audio.set_enabled(true);
        audio.configure(format(8000, 1)).unwrap();
        assert_eq!(audio.status().stats.target_buffer_ms, target);
        audio
            .packet(&packet(0, (frames - 1) as u16, 16384))
            .unwrap();
        let mut output = [f32::NAN; 16];
        no_alloc(|| audio.render(16, 1, 8000, |i, v| output[i] = v));
        assert!(output.iter().all(|v| *v == 0.0));
        audio
            .packet(&packet((frames - 1) as u64, 1, 16384))
            .unwrap();
        no_alloc(|| audio.render(16, 1, 8000, |i, v| output[i] = v));
        assert!(output[10] > 0.0);
    }
    let mut q = Playout::new(format(8000, 1)).unwrap();
    no_alloc(|| q.set_target_buffer_ms(0));
    assert_eq!(q.stats().target_buffer_ms, 10);
    no_alloc(|| q.set_target_buffer_ms(u32::MAX));
    assert_eq!(q.stats().target_buffer_ms, 300);
}
#[test]
fn live_target_changes_fade_trim_or_prefill_without_callback_allocation() {
    let audio = Audio::default();
    audio.set_enabled(true);
    audio.configure(format(8000, 1)).unwrap();
    audio.packet(&packet(0, 640, 16384)).unwrap();
    let mut output = [0.0; 80];
    no_alloc(|| audio.render(80, 1, 8000, |i, v| output[i] = v));
    audio.set_target_buffer_ms(20);
    no_alloc(|| audio.render(80, 1, 8000, |i, v| output[i] = v));
    let stats = audio.status().stats;
    assert_eq!(stats.target_buffer_ms, 20);
    assert_eq!(stats.latency_trims, 1);
    assert!((14.0..16.0).contains(&stats.buffered_ms));
    assert!((output[0] - 0.5).abs() < 0.0001);
    assert!(output[39] < 0.02);
    assert_eq!(output[40], 0.0);
    assert!(output[79] > 0.45);
    audio.set_target_buffer_ms(100);
    no_alloc(|| audio.render(80, 1, 8000, |i, v| output[i] = v));
    assert!(output[40..].iter().all(|v| *v == 0.0));
    assert!(!audio.status().stats.playing);
    assert_eq!(audio.status().stats.underflows, 0);
    no_alloc(|| audio.render(80, 1, 8000, |i, v| output[i] = v));
    assert!(output.iter().all(|v| *v == 0.0));
    audio.packet(&packet(640, 800, 16384)).unwrap();
    no_alloc(|| audio.render(80, 1, 8000, |i, v| output[i] = v));
    assert!(audio.status().stats.playing);
    assert!(output[50] > 0.45);
    // A new format or reconnect retains the user's buffering preference.
    audio.configure(format(16000, 2)).unwrap();
    assert_eq!(audio.status().stats.target_buffer_ms, 100);
    audio.reset();
    audio.configure(format(48000, 1)).unwrap();
    assert_eq!(audio.status().stats.target_buffer_ms, 100);
}
#[test]
fn reducing_target_before_startup_discards_excess_buffer_without_a_click() {
    let mut q = Playout::new(format(8000, 1)).unwrap();
    q.push(0, &[0.5; 640]).unwrap();
    no_alloc(|| q.set_target_buffer_ms(20));
    assert_eq!(q.stats().buffered_ms, 20.0);
    let output = render(&mut q, 80, 1, 8000);
    assert_eq!(output[0], 0.0);
    assert!(output[40] > 0.49);
    assert_eq!(q.stats().latency_trims, 0);
}
#[test]
fn source_validation_is_bounded() {
    let mut q = Playout::new(format(8000, 2)).unwrap();
    assert!(q.push(0, &[]).is_err());
    assert!(q.push(0, &[0.0]).is_err());
    assert!(q.push(u64::MAX, &[0.0, 0.0]).is_err());
    assert!(q.push(0, &[f32::NAN, 0.0]).is_err());
    assert!(q.push(0, &[0.0; 8193 * 2]).is_err());
    assert!(Playout::new(format(7999, 1)).is_err());
}
