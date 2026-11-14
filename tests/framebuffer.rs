use mpc_remote_screen_viewer_gpui::{framebuffer::*, protocol::*};

const BGRA: PixelFormat = PixelFormat([32, 16, 8, 8, 8, 0, 8]);
fn init(rotation: u8) -> ServerInit {
    ServerInit {
        width: 3,
        height: 2,
        rotation,
        pixel_format: BGRA,
        name: "test".into(),
        input_capabilities: 0,
        audio_available: false,
    }
}
fn rect<'a>(codec: u8, encoded: &'a [u8]) -> Rectangle<'a> {
    Rectangle {
        bounds: Bounds {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        },
        codec,
        encoded,
        drop_bits: 0,
    }
}

#[test]
fn raw_lz4_block_zstd_solid_and_overlap() {
    let packed: Vec<u8> = (0..6).flat_map(|n| [n, 2 * n, 3 * n, 0]).collect();
    let expected = packed_to_rgba(&packed, BGRA, 0).unwrap();
    let lz4 = lz4_flex::block::compress(&packed);
    let zstd = zstd::bulk::compress(&packed, 1).unwrap();
    for (codec, encoded) in [(0, &packed), (1, &lz4), (2, &zstd)] {
        assert_eq!(
            decode_rectangle(&rect(codec, encoded), BGRA).unwrap(),
            expected
        );
        let mut wrong = rect(codec, encoded);
        wrong.bounds.width = 2;
        assert!(decode_rectangle(&wrong, BGRA).is_err());
    }
    let mut fb = Framebuffer::new(init(0)).unwrap();
    let mut one = rect(3, &[0, 0, 255, 0]);
    one.bounds.width = 2;
    let mut two = rect(3, &[255, 0, 0, 0]);
    two.bounds.x = 1;
    two.bounds.width = 2;
    let report = fb.apply(&FrameUpdate {
        id: u32::MAX,
        quality: 2,
        rectangles: vec![one, two],
    });
    assert_eq!(report.applied, 2);
    assert_eq!(
        report.dirty,
        Some(Bounds {
            x: 0,
            y: 0,
            width: 3,
            height: 2
        })
    );
    assert_eq!(
        &fb.native_rgba()[..12],
        &[255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255]
    );
}
#[test]
fn jpeg_dimensions_channels_and_corruption() {
    let rgb = image::RgbImage::from_pixel(3, 2, image::Rgb([250, 10, 20]));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
        .encode_image(&rgb)
        .unwrap();
    for format in [BGRA, PixelFormat([32, 0, 8, 8, 8, 16, 8])] {
        let decoded = decode_rectangle(&rect(4, &jpeg), format).unwrap();
        for p in decoded.as_chunks::<4>().0 {
            assert!(p[0] > 240 && p[1] < 20 && p[2] < 30 && p[3] == 255);
        }
    }
    let mut wrong = rect(4, &jpeg);
    wrong.bounds.height = 3;
    assert!(decode_rectangle(&wrong, BGRA).is_err());
    assert!(decode_rectangle(&rect(4, &jpeg[..20]), BGRA).is_err());
    assert!(decode_rectangle(&rect(4, &jpeg), PixelFormat([24, 16, 8, 8, 8, 0, 8])).is_err());
}
#[test]
fn pixel_layout_bit_repetition_and_opaque_alpha() {
    for format in [PixelFormat([24, 16, 8, 8, 8, 0, 8]), BGRA] {
        assert_eq!(
            packed_to_rgba(&[3, 2, 1, 0][..format.bytes_per_pixel()], format, 0).unwrap(),
            [1, 2, 3, 255]
        );
    }
    assert_eq!(
        packed_to_rgba(&[1, 2, 3, 0], PixelFormat([32, 0, 8, 8, 8, 16, 8]), 0).unwrap(),
        [1, 2, 3, 255]
    );
    assert_eq!(
        packed_to_rgba(&[0xff, 0xff], PixelFormat([16, 11, 5, 5, 6, 0, 5]), 0).unwrap(),
        [255; 4]
    );
    assert_eq!(
        packed_to_rgba(&[128, 128, 128, 0], BGRA, 7).unwrap(),
        [192, 192, 192, 255]
    );
    assert_eq!(expand_quantized(128, 8, 255), 192);
    for bits in 1..=8 {
        let f = PixelFormat([32, 0, bits, 8, bits, 16, bits]);
        let value = ((1u16 << bits) - 1) as u8;
        assert_eq!(
            packed_to_rgba(&[value, value, value, 0], f, 0).unwrap(),
            [255; 4]
        );
    }
}
#[test]
fn rotations_asymmetric_screen_and_png() {
    let encoded: Vec<u8> = (1..=6).flat_map(|n| [0, 0, n, 0]).collect();
    let expected = [
        vec![1, 2, 3, 4, 5, 6],
        vec![4, 1, 5, 2, 6, 3],
        vec![6, 5, 4, 3, 2, 1],
        vec![3, 6, 2, 5, 1, 4],
    ];
    for rotation in 0..4 {
        let mut fb = Framebuffer::new(init(rotation)).unwrap();
        fb.apply(&FrameUpdate {
            id: 0,
            quality: 0,
            rectangles: vec![rect(0, &encoded)],
        });
        let image = fb.upright_snapshot();
        assert_eq!(
            image.pixels().map(|p| p[0]).collect::<Vec<_>>(),
            expected[rotation as usize]
        );
        assert_eq!(
            image.dimensions(),
            if rotation % 2 == 0 { (3, 2) } else { (2, 3) }
        );
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert_eq!(
            image::load_from_memory(png.get_ref()).unwrap().to_rgba8(),
            image
        );
    }
}
#[test]
fn aggregate_pixel_budget_bounds_overlaps_failed_decodes_and_resets_each_frame() {
    let mut fb = Framebuffer::new(init(0)).unwrap();
    fb.set_frame_pixel_budget(12); // two 3x2 decode attempts
    let report = fb.apply(&FrameUpdate {
        id: 1,
        quality: 0,
        rectangles: vec![
            rect(3, &[0, 0, 255, 0]),
            rect(1, &[255]),
            rect(3, &[255, 0, 0, 0]),
        ],
    });
    assert_eq!((report.applied, report.skipped), (1, 2));
    assert_eq!(fb.content_revision(), 1);
    assert_eq!(&fb.native_rgba()[..4], &[255, 0, 0, 255]);
    let report = fb.apply(&FrameUpdate {
        id: 2,
        quality: 0,
        rectangles: vec![rect(3, &[255, 0, 0, 0])],
    });
    assert_eq!((report.applied, report.skipped), (1, 0));
    assert_eq!(fb.content_revision(), 2);
    fb.set_frame_pixel_budget(0);
    let report = fb.apply(&FrameUpdate {
        id: 3,
        quality: 0,
        rectangles: vec![rect(3, &[0; 4])],
    });
    assert_eq!((report.applied, report.skipped, report.dirty), (0, 1, None));
    assert_eq!(fb.content_revision(), 2);
}

#[test]
fn malformed_structure_vs_skipped_rectangles() {
    assert!(FrameUpdate::parse(&[0; 6]).is_err());
    let mut bytes = vec![0; 7];
    bytes[5] = 1;
    assert!(FrameUpdate::parse(&bytes).is_err());
    bytes.extend_from_slice(&[0; 14]);
    bytes[7 + 10] = 1; // Claim a missing encoded byte.
    assert!(FrameUpdate::parse(&bytes).is_err());
    bytes[7 + 10] = 0;
    bytes.extend_from_slice(&[9; 3]); // Trailing extension bytes are legal.
    assert!(FrameUpdate::parse(&bytes).is_ok());
    let mut outside = rect(3, &[0; 4]);
    outside.bounds.x = u16::MAX;
    let mut zero = rect(3, &[0; 4]);
    zero.bounds.width = 0;
    let mut fb = Framebuffer::new(init(0)).unwrap();
    let report = fb.apply(&FrameUpdate {
        id: 0,
        quality: 0,
        rectangles: vec![outside, zero, rect(1, &[255]), rect(99, &[])],
    });
    assert_eq!((report.applied, report.skipped, report.dirty), (0, 4, None));
}
