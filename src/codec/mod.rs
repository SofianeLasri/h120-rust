//! Shared types and constants of the H.120 codec (clause 1).
//!
//! All "§x.y" references point to Recommendation ITU-T H.120 (03/93).

pub mod bitio;
pub mod decoder;
pub mod encoder;
pub mod tables;

/// Luminance samples per active line (§1.4.1.1).
pub const WIDTH: usize = 256;
/// Chrominance samples per active line (§1.4.2.1).
pub const CHROMA_WIDTH: usize = 52;
/// Address of the first chrominance sample (§1.5.4).
pub const CHROMA_ADDR_BASE: usize = 4;
/// Active lines per field (§1.4.1.2).
pub const LINES_PER_FIELD: usize = 143;
/// Active lines per frame (2 fields).
pub const LINES_PER_FRAME: usize = 2 * LINES_PER_FIELD;

/// Black level (§1.4.1.1).
pub const Y_MIN: u8 = 16;
/// White level (§1.4.1.1).
pub const Y_MAX: u8 = 239;
/// Legal chrominance range: 128 ± 111 (§1.4.2.1).
pub const C_MIN: u8 = 17;
pub const C_MAX: u8 = 239;
/// Assumed line/frame blanking level used for prediction (§1.4.1.3.1).
pub const BLANKING: u8 = 128;

/// Transmission buffer size: 96 kbit, with 1 K = 1024 bits (§1.5.1).
pub const BUFFER_BITS: usize = 96 * 1024;

/// Minimum gap between the end of a cluster and the start of the next (§1.5.3).
pub const MIN_CLUSTER_GAP: usize = 4;

/// A reconstructed field, identical in the encoder and the decoder.
///
/// Each chrominance line stores only the component transmitted on that line:
/// (B'−Y') or (R'−Y') alternately (§1.4.2.1).
#[derive(Clone)]
pub struct FieldStore {
    /// Luminance, `LINES_PER_FIELD` lines of `WIDTH` samples.
    pub y: Vec<[u8; WIDTH]>,
    /// Chrominance, `LINES_PER_FIELD` lines of `CHROMA_WIDTH` samples.
    pub c: Vec<[u8; CHROMA_WIDTH]>,
    /// Moving luminance areas of the last coded field (for interpolating
    /// omitted fields, §1.4.1.4.2).
    pub y_moving: Vec<[bool; WIDTH]>,
    /// Moving chrominance areas (§1.4.2.4).
    pub c_moving: Vec<[bool; CHROMA_WIDTH]>,
}

impl FieldStore {
    pub fn new() -> Self {
        FieldStore {
            y: vec![[BLANKING; WIDTH]; LINES_PER_FIELD],
            c: vec![[BLANKING; CHROMA_WIDTH]; LINES_PER_FIELD],
            y_moving: vec![[false; WIDTH]; LINES_PER_FIELD],
            c_moving: vec![[false; CHROMA_WIDTH]; LINES_PER_FIELD],
        }
    }

    pub fn clear_moving(&mut self) {
        for l in &mut self.y_moving {
            *l = [false; WIDTH];
        }
        for l in &mut self.c_moving {
            *l = [false; CHROMA_WIDTH];
        }
    }
}

/// Chroma component carried by a given line of a given field.
///
/// 1st active line of field 1: (B'−Y'), 1st line of field 2: (R'−Y'), then
/// alternating (§1.4.2.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromaComp {
    /// E'B − E'Y (Cb)
    Cb,
    /// E'R − E'Y (Cr)
    Cr,
}

pub fn chroma_comp(field: usize, line: usize) -> ChromaComp {
    match (field, line % 2) {
        (0, 0) | (1, 1) => ChromaComp::Cb,
        _ => ChromaComp::Cr,
    }
}

#[inline]
pub fn clamp_y(v: i16) -> u8 {
    v.clamp(Y_MIN as i16, Y_MAX as i16) as u8
}

#[inline]
pub fn clamp_c(v: i16) -> u8 {
    v.clamp(C_MIN as i16, C_MAX as i16) as u8
}

/// "Spec" line number (0..142 for field 1, 144..286 for field 2), §1.5.2.1.
#[inline]
pub fn spec_line_number(field: usize, line: usize) -> usize {
    if field == 0 { line } else { 144 + line }
}

/// Value of element D (upper-right neighbour of X on the previous line of the
/// same field, Figure 1). If D belongs to a subsampled moving area and was not
/// transmitted in the current frame, it is replaced by C, the element directly
/// above X (§1.4.1.4.1). The first line of a field predicts from the 128
/// blanking level.
#[inline]
pub fn d_value(prev_line: Option<&[u8; WIDTH]>, prev_not_transmitted: &[bool; WIDTH], e: usize) -> u8 {
    let Some(prev) = prev_line else {
        return BLANKING;
    };
    let ed = e + 1;
    if ed >= WIDTH {
        return BLANKING;
    }
    if prev_not_transmitted[ed] { prev[e] } else { prev[ed] }
}

/// Luminance DPCM prediction X = (A + D) / 2, truncated division (§1.4.1.3.1).
#[inline]
pub fn predict_luma(a: u8, d: u8) -> u8 {
    ((a as u16 + d as u16) / 2) as u8
}

/// Interpolates an omitted field (§1.4.1.4.2 and §1.4.2.4).
///
/// `b1` is the transmitted field preceding the omitted field, `a1` the
/// transmitted field following it (both of opposite parity to the omitted
/// field). An element x of the omitted field is considered moving if any of
/// the four neighbouring elements a/b/c/d (above and below, in b1 and a1) is
/// moving; only then is it interpolated, otherwise it is left unchanged.
/// This function is applied identically by the encoder and the decoder.
pub fn interpolate_omitted_field(omitted: &mut FieldStore, omitted_parity: usize, b1: &FieldStore, a1: &FieldStore) {
    // Lines of the transmitted field bracketing line j of the omitted field:
    // field 2 omitted: above = line j of field 1, below = line j+1;
    // field 1 omitted: above = line j−1 of field 2, below = line j.
    let bracket = |j: usize| -> (Option<usize>, Option<usize>) {
        if omitted_parity == 1 {
            (Some(j), if j + 1 < LINES_PER_FIELD { Some(j + 1) } else { None })
        } else {
            (j.checked_sub(1), Some(j))
        }
    };
    for j in 0..LINES_PER_FIELD {
        let (above, below) = bracket(j);
        // Luminance: x = ((a+b)/2 + (c+d)/2) / 2, truncated divisions.
        for e in 0..WIDTH {
            let get = |st: &FieldStore, l: Option<usize>| -> (u8, bool) {
                match l {
                    Some(l) => (st.y[l][e], st.y_moving[l][e]),
                    None => (BLANKING, false),
                }
            };
            let (a, ma) = get(b1, above);
            let (b, mb) = get(b1, below);
            let (c, mc) = get(a1, above);
            let (d, md) = get(a1, below);
            if ma || mb || mc || md {
                let x = ((a as u16 + b as u16) / 2 + (c as u16 + d as u16) / 2) / 2;
                omitted.y[j][e] = x as u8;
                omitted.y_moving[j][e] = true;
            } else {
                omitted.y_moving[j][e] = false;
            }
        }
        omitted.y[j][WIDTH - 1] = BLANKING;
        // Chrominance: x = (a+c)/2 (field 1) or (b+d)/2 (field 2), the chosen
        // lines carrying the same component as the omitted line.
        for e in 0..CHROMA_WIDTH {
            let get = |st: &FieldStore, l: Option<usize>| -> (u8, bool) {
                match l {
                    Some(l) => (st.c[l][e], st.c_moving[l][e]),
                    None => (BLANKING, false),
                }
            };
            let (a, ma) = get(b1, above);
            let (b, mb) = get(b1, below);
            let (c, mc) = get(a1, above);
            let (d, md) = get(a1, below);
            if ma || mb || mc || md {
                let x = if omitted_parity == 0 {
                    (a as u16 + c as u16) / 2
                } else {
                    (b as u16 + d as u16) / 2
                };
                omitted.c[j][e] = x as u8;
                omitted.c_moving[j][e] = true;
            } else {
                omitted.c_moving[j][e] = false;
            }
        }
    }
}
