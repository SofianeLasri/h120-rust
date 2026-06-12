//! Integration tests: the encoder and the decoder must stay in perfect
//! lockstep (closed DPCM loop), and the stream must decode end to end.

use h120::codec::decoder::Decoder;
use h120::codec::encoder::{Encoder, EncoderConfig};
use h120::codec::{FieldStore, LINES_PER_FIELD, WIDTH};
use h120::source::ingest;
use h120::y4m::Frame444;

/// Test frame: a gradient background + a square that moves with the index.
fn test_frame(t: usize, square: bool) -> Frame444 {
    let (w, h) = (256, 286);
    let mut f = Frame444::new(w, h);
    for y in 0..h {
        for x in 0..w {
            f.y[y * w + x] = (30 + (x / 4) + (y / 8)) as u8;
            f.cb[y * w + x] = 110;
            f.cr[y * w + x] = 140;
        }
    }
    if square {
        let cx = 40 + (t * 6) % 150;
        let cy = 60 + (t * 4) % 140;
        for y in cy..(cy + 48).min(h) {
            for x in cx..(cx + 48).min(w) {
                f.y[y * w + x] = 200;
                f.cb[y * w + x] = 80;
                f.cr[y * w + x] = 180;
            }
        }
    }
    f
}

fn stores_equal(a: &FieldStore, b: &FieldStore) -> bool {
    a.y == b.y && a.c == b.c
}

fn psnr_y(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

/// Static scene: after the PCM bootstrap, the decoded store must be EXACTLY
/// the input (PCM lines copy the samples).
#[test]
fn static_scene_becomes_exact() {
    let frame = test_frame(0, false);
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_600_000, mono: false });
    for _ in 0..40 {
        enc.encode_frame(&frame);
    }
    let data = enc.finish();

    let mut dec = Decoder::new(&data);
    let mut last = None;
    while let Some(fields) = dec.next_frame().expect("decoding") {
        last = Some(fields);
    }
    let fields = last.expect("at least one frame");
    let reference = ingest(&frame, false);
    for f in 0..2 {
        for l in 0..LINES_PER_FIELD {
            assert_eq!(fields[f].y[l], reference[f].y[l], "luma field {f} line {l}");
            assert_eq!(fields[f].c[l], reference[f].c[l], "chroma field {f} line {l}");
        }
    }
    assert_eq!(dec.stats.frames, 40);
}

/// Moderate motion at high bitrate: the encoder store and the decoder store
/// must be bit-for-bit identical after each frame (closed loop). This is THE
/// internal conformance test of the codec.
#[test]
fn encoder_decoder_lockstep() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 2_000_000, mono: false });
    let n = 30;
    let mut snapshots: Vec<[FieldStore; 2]> = Vec::new();
    for t in 0..n {
        enc.encode_frame(&test_frame(t, true));
        assert!(
            !enc.has_pending_interpolation(),
            "no field omission expected at this bitrate (frame {t})"
        );
        snapshots.push(enc.stores().clone());
    }
    let data = enc.finish();

    let mut dec = Decoder::new(&data);
    for (t, snap) in snapshots.iter().enumerate() {
        let fields = dec
            .next_frame()
            .expect("decoding")
            .unwrap_or_else(|| panic!("frame {t} missing"));
        for f in 0..2 {
            assert!(
                stores_equal(&fields[f], &snap[f]),
                "encoder/decoder desynchronization: frame {t}, field {f}"
            );
        }
    }
}

/// Horizontal subsampling (Table 2, extra elements, quincunx) must also
/// preserve the bit-for-bit lockstep of the stores.
#[test]
fn lockstep_with_horizontal_subsampling() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_400_000, mono: false });
    let n = 40;
    // One snapshot per frame, ignored when an omitted-field interpolation is
    // pending: the encoder store is then one field ahead of what the decoder
    // will emit (alignment returns on the next frame).
    let mut snapshots: Vec<Option<[FieldStore; 2]>> = Vec::new();
    let mut subsampled_seen = false;
    for t in 0..n {
        // Two squares to keep the buffer in the subsampling region.
        let mut frame = test_frame(t, true);
        let f2 = test_frame(t + 40, true);
        for i in 0..frame.y.len() {
            if f2.y[i] == 200 {
                frame.y[i] = 170;
                frame.cr[i] = 90;
            }
        }
        enc.encode_frame(&frame);
        subsampled_seen = subsampled_seen || enc.stats.subsampled_lines > 0;
        snapshots.push(if enc.has_pending_interpolation() {
            None
        } else {
            Some(enc.stores().clone())
        });
    }
    assert!(subsampled_seen, "the test must exercise subsampling");
    let compared = snapshots.iter().filter(|s| s.is_some()).count();
    assert!(compared >= 10, "too few comparable frames ({compared})");
    let data = enc.finish();
    let mut dec = Decoder::new(&data);
    for (t, snap) in snapshots.iter().enumerate() {
        let Some(fields) = dec.next_frame().expect("decoding") else {
            // The very last frame may stay pending emission if its field 2
            // was omitted and the stream stops there.
            assert!(t >= n - 1, "frame {t} missing");
            break;
        };
        if let Some(snap) = snap {
            for f in 0..2 {
                assert!(
                    stores_equal(&fields[f], &snap[f]),
                    "desynchronization in subsampled mode: frame {t}, field {f}"
                );
            }
        }
    }
    assert!(dec.stats.subsampled_lines > 0);
    assert!(dec.stats.extra_elements > 0, "extra elements must be exercised");
}

/// Heavy motion at reduced bitrate: rate control must trigger subsampling
/// (Table 2) then field omission, and the stream must stay decodable end to
/// end with a quality floor.
#[test]
fn heavy_motion_survives_rate_control() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_000_000, mono: false });
    let n = 50;
    let mut last_input = None;
    for t in 0..n {
        // Two squares in sustained motion to saturate the buffer.
        let mut frame = test_frame(t * 2, true);
        let f2 = test_frame(t * 3 + 31, true);
        for i in 0..frame.y.len() {
            if f2.y[i] == 200 {
                frame.y[i] = 220;
                frame.cb[i] = f2.cb[i];
            }
        }
        enc.encode_frame(&frame);
        last_input = Some(frame);
    }
    let stats = enc.stats.clone();
    let data = enc.finish();
    assert!(stats.subsampled_lines > 0, "subsampling must kick in");
    assert!(
        stats.max_occupancy <= 96.0 * 1024.0,
        "the 96 kbit buffer must never overflow (max {:.0})",
        stats.max_occupancy
    );

    let mut dec = Decoder::new(&data);
    let mut frames = 0u64;
    let mut last = None;
    while let Some(fields) = dec.next_frame().expect("decoding under load") {
        frames += 1;
        last = Some(fields);
    }
    // The last frame may stay pending if its field 2 was omitted.
    assert!(frames >= n as u64 - 1, "{frames} frames decoded of {n}");
    assert_eq!(dec.stats.subsampled_lines as u64 > 0, true);

    // Quality floor on the luminance of the last decoded state.
    let fields = last.unwrap();
    let reference = ingest(&last_input.unwrap(), false);
    let mut dec_y = Vec::new();
    let mut ref_y = Vec::new();
    for f in 0..2 {
        for l in 0..LINES_PER_FIELD {
            dec_y.extend_from_slice(&fields[f].y[l][..WIDTH - 1]);
            ref_y.extend_from_slice(&reference[f].y[l][..WIDTH - 1]);
        }
    }
    let p = psnr_y(&dec_y, &ref_y);
    assert!(p > 22.0, "luma PSNR too low under load: {p:.1} dB");
}

/// Regression: motion on the very last coded line of the stream (image row
/// 285 → field 2, line 142) must not be swallowed by the end-of-stream
/// padding. If the last line ends on VLC codes leaving fewer than 12 bits
/// before the zero padding, the decoder used to take them for a sync word and
/// abandon the line, desynchronizing the last frame. We sweep several lengths:
/// without the fix, at least one of them lands on that alignment.
#[test]
fn final_line_motion_survives_padding() {
    let (w, h) = (256, 286);
    let make = |t: usize| {
        let mut f = Frame444::new(w, h);
        for y in 0..h {
            for x in 0..w {
                f.y[y * w + x] = (40 + (x / 3) % 60) as u8;
                f.cb[y * w + x] = 120;
                f.cr[y * w + x] = 130;
            }
        }
        // Moving checkerboard on the last three rows (including row 285).
        for y in (h - 3)..h {
            for x in 0..w {
                let on = ((x + t * 7) / 9) % 2 == 0;
                f.y[y * w + x] = if on { 210 } else { 28 };
                f.cr[y * w + x] = if on { 180 } else { 80 };
            }
        }
        f
    };
    for n in 3..=12 {
        let mut enc = Encoder::new(EncoderConfig { bitrate: 1_200_000, mono: false });
        for t in 0..n {
            enc.encode_frame(&make(t));
        }
        // This content stays at a moderate bitrate: no field omission.
        assert!(!enc.has_pending_interpolation(), "n={n}");
        let snap = enc.stores().clone();
        let data = enc.finish();

        let mut dec = Decoder::new(&data);
        let mut last = None;
        while let Some(fields) = dec.next_frame().expect("decoding") {
            last = Some(fields);
        }
        let fields = last.unwrap_or_else(|| panic!("no frame decoded (n={n})"));
        for f in 0..2 {
            assert!(
                stores_equal(&fields[f], &snap[f]),
                "desync on the last frame (n={n}, field {f})"
            );
        }
    }
}

/// The stream must decode identically even when cleanly truncated at a frame
/// boundary (parser robustness at end of stream).
#[test]
fn truncated_stream_is_graceful() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_600_000, mono: false });
    for t in 0..10 {
        enc.encode_frame(&test_frame(t, true));
    }
    let data = enc.finish();
    for cut in [data.len() / 3, data.len() / 2, data.len() - 7] {
        let mut dec = Decoder::new(&data[..cut]);
        let mut frames = 0;
        while let Some(_) = dec.next_frame().expect("a truncated stream must not be an error") {
            frames += 1;
        }
        assert!(frames < 10);
    }
}

/// A monochrome stream stays a color stream (neutral chrominance): no chroma
/// cluster must be emitted.
#[test]
fn mono_emits_no_chroma_clusters() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_600_000, mono: true });
    for t in 0..10 {
        enc.encode_frame(&test_frame(t, true));
    }
    assert_eq!(enc.stats.chroma_clusters, 0);
    let data = enc.finish();
    let mut dec = Decoder::new(&data);
    while dec.next_frame().expect("mono decoding").is_some() {}
    assert_eq!(dec.stats.chroma_clusters, 0);
    assert!(dec.stats.frames > 0);
}
