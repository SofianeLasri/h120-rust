//! Pre- and post-processing: conversion between Y4M frames and the H.120
//! source format (luma 256×143/field, chroma 52 samples/line alternating
//! B'−Y' / R'−Y', §1.4.1 and §1.4.2).
//!
//! The exact characteristics of the pre/post filters are left free by the
//! spec; we use bilinear resizing.

use crate::codec::{
    BLANKING, C_MAX, C_MIN, CHROMA_WIDTH, ChromaComp, FieldStore, LINES_PER_FIELD,
    LINES_PER_FRAME, WIDTH, Y_MAX, Y_MIN, chroma_comp,
};
use crate::scale::resize_plane;
use crate::y4m::Frame444;

/// Height of the displayed image: 286 woven lines + 2 padding lines (to make
/// an even height, convenient for downstream conversions).
pub const OUT_HEIGHT: usize = 288;
/// Pixel aspect ratio: the 256×286 image covers a 4:3 screen, i.e.
/// (4/3) / (256/286) = 1144/768 = 143/96.
pub const PAR: (u32, u32) = (143, 96);

/// One input field ready to be coded.
pub struct FieldInput {
    pub y: Vec<[u8; WIDTH]>,
    /// The component transmitted on each line (Cb or Cr depending on the line).
    pub c: Vec<[u8; CHROMA_WIDTH]>,
}

/// Converts a frame into two fields in the H.120 source format.
/// Field 1 = even lines of the image, field 2 = odd lines (Figure 3).
pub fn ingest(frame: &Frame444, mono: bool) -> [FieldInput; 2] {
    let h = LINES_PER_FRAME;
    let y = if (frame.w, frame.h) == (WIDTH, h) {
        frame.y.clone()
    } else {
        resize_plane(&frame.y, frame.w, frame.h, WIDTH, h)
    };
    let (cb, cr) = if mono {
        (vec![BLANKING; CHROMA_WIDTH * h], vec![BLANKING; CHROMA_WIDTH * h])
    } else {
        (
            resize_plane(&frame.cb, frame.w, frame.h, CHROMA_WIDTH, h),
            resize_plane(&frame.cr, frame.w, frame.h, CHROMA_WIDTH, h),
        )
    };
    let mut fields = [
        FieldInput {
            y: vec![[BLANKING; WIDTH]; LINES_PER_FIELD],
            c: vec![[BLANKING; CHROMA_WIDTH]; LINES_PER_FIELD],
        },
        FieldInput {
            y: vec![[BLANKING; WIDTH]; LINES_PER_FIELD],
            c: vec![[BLANKING; CHROMA_WIDTH]; LINES_PER_FIELD],
        },
    ];
    for f in 0..2 {
        for i in 0..LINES_PER_FIELD {
            let src_line = 2 * i + f;
            for e in 0..WIDTH {
                fields[f].y[i][e] = y[src_line * WIDTH + e].clamp(Y_MIN, Y_MAX);
            }
            // The last element of each active line is fixed to 128 in both the
            // encoder and the decoder (§1.4.1.1).
            fields[f].y[i][WIDTH - 1] = BLANKING;
            let plane = match chroma_comp(f, i) {
                ChromaComp::Cb => &cb,
                ChromaComp::Cr => &cr,
            };
            for e in 0..CHROMA_WIDTH {
                fields[f].c[i][e] = plane[src_line * CHROMA_WIDTH + e].clamp(C_MIN, C_MAX);
            }
        }
    }
    fields
}

/// Reconstructs a displayable frame from the two decoded fields: weaving the
/// fields, interpolating the chroma component missing from each line
/// (§1.4.2.1), then horizontal upsampling 52 → 256.
pub fn egress(store: &[FieldStore; 2]) -> Frame444 {
    let mut out = Frame444::new(WIDTH, OUT_HEIGHT);
    for line in out.y.iter_mut() {
        *line = Y_MIN;
    }
    for f in 0..2 {
        for i in 0..LINES_PER_FIELD {
            let dst_line = 2 * i + f;
            out.y[dst_line * WIDTH..(dst_line + 1) * WIDTH].copy_from_slice(&store[f].y[i]);

            // Component present on this line + interpolation of the other one
            // from the neighbouring lines of the same field (which carry it).
            let own = &store[f].c[i];
            let mut other = [BLANKING; CHROMA_WIDTH];
            let above = i.checked_sub(1).map(|j| &store[f].c[j]);
            let below = if i + 1 < LINES_PER_FIELD { Some(&store[f].c[i + 1]) } else { None };
            for e in 0..CHROMA_WIDTH {
                other[e] = match (above, below) {
                    (Some(a), Some(b)) => ((a[e] as u16 + b[e] as u16) / 2) as u8,
                    (Some(a), None) => a[e],
                    (None, Some(b)) => b[e],
                    (None, None) => BLANKING,
                };
            }
            let (cb52, cr52) = match chroma_comp(f, i) {
                ChromaComp::Cb => (own as &[u8], &other as &[u8]),
                ChromaComp::Cr => (&other as &[u8], own as &[u8]),
            };
            let cb = resize_plane(cb52, CHROMA_WIDTH, 1, WIDTH, 1);
            let cr = resize_plane(cr52, CHROMA_WIDTH, 1, WIDTH, 1);
            out.cb[dst_line * WIDTH..(dst_line + 1) * WIDTH].copy_from_slice(&cb);
            out.cr[dst_line * WIDTH..(dst_line + 1) * WIDTH].copy_from_slice(&cr);
        }
    }
    out
}

/// Integer upscaling (nearest neighbour) for display/export.
pub fn upscale(frame: &Frame444, n: usize) -> Frame444 {
    if n <= 1 {
        return frame.clone();
    }
    let (w, h) = (frame.w * n, frame.h * n);
    let mut out = Frame444::new(w, h);
    for yy in 0..h {
        for xx in 0..w {
            let s = (yy / n) * frame.w + xx / n;
            out.y[yy * w + xx] = frame.y[s];
            out.cb[yy * w + xx] = frame.cb[s];
            out.cr[yy * w + xx] = frame.cr[s];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_egress_roundtrip_uniform() {
        let mut frame = Frame444::new(WIDTH, 286);
        frame.y.fill(100);
        frame.cb.fill(90);
        frame.cr.fill(160);
        let fields = ingest(&frame, false);
        assert_eq!(fields[0].y[0][0], 100);
        assert_eq!(fields[0].y[0][WIDTH - 1], BLANKING);
        // Field 1 line 0 carries Cb, line 1 carries Cr.
        assert_eq!(fields[0].c[0][10], 90);
        assert_eq!(fields[0].c[1][10], 160);
        // Field 2 line 0 carries Cr.
        assert_eq!(fields[1].c[0][10], 160);

        let mut store = [FieldStore::new(), FieldStore::new()];
        for f in 0..2 {
            for i in 0..LINES_PER_FIELD {
                store[f].y[i] = fields[f].y[i];
                store[f].c[i] = fields[f].c[i];
            }
        }
        let out = egress(&store);
        assert_eq!(out.y[5 * WIDTH + 5], 100);
        assert!(out.cb[10 * WIDTH + 128].abs_diff(90) <= 1);
        assert!(out.cr[11 * WIDTH + 128].abs_diff(160) <= 1);
    }
}
