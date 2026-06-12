//! Quantization laws and variable-length codes (Tables 1 and 2/H.120).
//!
//! Every code in both tables has one of two forms:
//!   - positive side: k zeros followed by a 1          (k = 1..8)
//!   - negative side: a 1, m zeros, then a 1           (m = 0..8)
//!
//! The end-of-cluster code EOC = 1001 corresponds to m = 2 (code no. 11,
//! §1.4.1.3.2) in both tables, which makes the whole set prefix-free.
//!
//! Erratum: Table 2 prints "0 to 22" for level +15; this should read
//! "10 to 22" (otherwise the ranges 0..9 and 0..22 overlap).

use super::bitio::{BitReader, BitWriter};

/// Decoded symbol of a moving area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vlc {
    /// DPCM output level. `extra` only makes sense on subsampled lines
    /// ("extra" element, §1.4.1.4.1).
    Level { level: i16, extra: bool },
    /// End of cluster.
    Eoc,
}

/// Result of reading a VLC code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VlcRead {
    /// Valid symbol.
    Sym(Vlc),
    /// Malformed code (more than 8 zeros): cannot appear in a conforming
    /// stream, so it signals corruption — to be distinguished from a clean end.
    Invalid,
    /// Stream exhausted in the middle of a code.
    Eof,
}

/// Table 1 quantization: (inclusive upper threshold on |diff|, level, run).
const T1_POS: [(i16, i16, u8); 8] = [
    (7, 3, 1),
    (17, 12, 2),
    (30, 23, 3),
    (47, 38, 4),
    (68, 57, 5),
    (93, 80, 6),
    (123, 107, 7),
    (255, 140, 8),
];
const T1_NEG: [(i16, i16, u8); 8] = [
    (8, -4, 0),
    (18, -13, 1),
    (31, -24, 3),
    (48, -39, 4),
    (69, -58, 5),
    (94, -81, 6),
    (124, -108, 7),
    (255, -141, 8),
];

/// Table 2 quantization: (inclusive upper threshold on |diff|, level, run for
/// a normal element, run for an extra element).
const T2_POS: [(i16, i16, u8, u8); 4] = [
    (9, 4, 1, 3),
    (22, 15, 2, 5),
    (39, 30, 4, 7),
    (255, 49, 6, 8),
];
const T2_NEG: [(i16, i16, u8, u8); 4] = [
    (10, -5, 0, 3),
    (23, -16, 1, 5),
    (40, -31, 4, 7),
    (255, -50, 6, 8),
];

/// Table 1 decoding, positive side: index = k − 1.
const T1_DEC_POS: [i16; 8] = [3, 12, 23, 38, 57, 80, 107, 140];
/// Table 1 decoding, negative side: index = m (m = 2 → EOC).
const T1_DEC_NEG: [i16; 9] = [-4, -13, 0, -24, -39, -58, -81, -108, -141];
/// Table 2 decoding, positive side: index = k − 1, (level, extra).
const T2_DEC_POS: [(i16, bool); 8] = [
    (4, false),
    (15, false),
    (4, true),
    (30, false),
    (15, true),
    (49, false),
    (30, true),
    (49, true),
];
/// Table 2 decoding, negative side: index = m (m = 2 → EOC).
const T2_DEC_NEG: [(i16, bool); 9] = [
    (-5, false),
    (-16, false),
    (0, false), // EOC
    (-5, true),
    (-31, false),
    (-16, true),
    (-50, false),
    (-31, true),
    (-50, true),
];

/// Quantizes a prediction error and returns (level, run, negative side).
/// `diff` is in [−255, 255] (§1.4.1.3.2: 511 input levels).
pub fn quantize(diff: i16, subsampled: bool, extra: bool) -> (i16, u8, bool) {
    debug_assert!((-255..=255).contains(&diff));
    if subsampled {
        if diff >= 0 {
            for &(hi, level, kn, kx) in &T2_POS {
                if diff <= hi {
                    return (level, if extra { kx } else { kn }, false);
                }
            }
        } else {
            for &(hi, level, mn, mx) in &T2_NEG {
                if -diff <= hi {
                    return (level, if extra { mx } else { mn }, true);
                }
            }
        }
    } else {
        debug_assert!(!extra, "no extra elements outside subsampling");
        if diff >= 0 {
            for &(hi, level, k) in &T1_POS {
                if diff <= hi {
                    return (level, k, false);
                }
            }
        } else {
            for &(hi, level, m) in &T1_NEG {
                if -diff <= hi {
                    return (level, m, true);
                }
            }
        }
    }
    unreachable!("quantization ranges are exhaustive")
}

/// Writes a VLC code: positive side `0^k 1`, negative side `1 0^m 1`.
pub fn write_code(w: &mut BitWriter, run: u8, negative: bool) {
    if negative {
        w.put_bit(true);
    }
    for _ in 0..run {
        w.put_bit(false);
    }
    w.put_bit(true);
}

/// Writes the end-of-cluster code (1001).
pub fn write_eoc(w: &mut BitWriter) {
    write_code(w, 2, true);
}

/// Reads a VLC symbol. Synchronization codes (≥ 12 zeros) are detected before
/// the call; a code with more than 8 zeros is therefore necessarily a
/// corruption (`Invalid`), distinct from a clean end of stream (`Eof`).
pub fn read_vlc(r: &mut BitReader, subsampled: bool) -> VlcRead {
    let Some(negative) = r.read_bit() else { return VlcRead::Eof };
    let mut run: u8 = 0;
    loop {
        match r.read_bit() {
            None => return VlcRead::Eof,
            Some(true) => break,
            Some(false) => {
                run += 1;
                if run > 8 {
                    return VlcRead::Invalid;
                }
            }
        }
    }
    if negative {
        // `1 1` corresponds to run = 0; `1 0^m 1` to run = m.
        if run == 2 {
            return VlcRead::Sym(Vlc::Eoc);
        }
        if subsampled {
            let (level, extra) = T2_DEC_NEG[run as usize];
            VlcRead::Sym(Vlc::Level { level, extra })
        } else {
            VlcRead::Sym(Vlc::Level { level: T1_DEC_NEG[run as usize], extra: false })
        }
    } else {
        // `0^k 1`: run = k − 1 zeros counted after the first bit read.
        // First bit read = 0 (negative=false), so k = run + 1.
        let k = run + 1;
        if k > 8 {
            return VlcRead::Invalid;
        }
        if subsampled {
            let (level, extra) = T2_DEC_POS[(k - 1) as usize];
            VlcRead::Sym(Vlc::Level { level, extra })
        } else {
            VlcRead::Sym(Vlc::Level { level: T1_DEC_POS[(k - 1) as usize], extra: false })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rebuilds a code's bit string for comparison with the spec.
    fn code_str(run: u8, negative: bool) -> String {
        let mut s = String::new();
        if negative {
            s.push('1');
        }
        for _ in 0..run {
            s.push('0');
        }
        s.push('1');
        s
    }

    #[test]
    fn table1_codes_match_spec() {
        // (representative diff, level, expected code) — Table 1/H.120.
        let cases: &[(i16, i16, &str)] = &[
            (-200, -141, "1000000001"),
            (-100, -108, "100000001"),
            (-80, -81, "10000001"),
            (-60, -58, "1000001"),
            (-40, -39, "100001"),
            (-25, -24, "10001"),
            (-10, -13, "101"),
            (-5, -4, "11"),
            (0, 3, "01"),
            (10, 12, "001"),
            (20, 23, "0001"),
            (40, 38, "00001"),
            (50, 57, "000001"),
            (70, 80, "0000001"),
            (100, 107, "00000001"),
            (200, 140, "000000001"),
        ];
        for &(diff, level, code) in cases {
            let (l, run, neg) = quantize(diff, false, false);
            assert_eq!(l, level, "diff {diff}");
            assert_eq!(code_str(run, neg), code, "diff {diff}");
        }
    }

    #[test]
    fn table2_codes_match_spec() {
        // Normal elements then extra — Table 2/H.120.
        let normal: &[(i16, i16, &str)] = &[
            (-100, -50, "10000001"),
            (-30, -31, "100001"),
            (-15, -16, "101"),
            (-5, -5, "11"),
            (5, 4, "01"),
            (15, 15, "001"),
            (30, 30, "00001"),
            (100, 49, "0000001"),
        ];
        let extra: &[(i16, i16, &str)] = &[
            (-100, -50, "1000000001"),
            (-30, -31, "100000001"),
            (-15, -16, "1000001"),
            (-5, -5, "10001"),
            (5, 4, "0001"),
            (15, 15, "000001"),
            (30, 30, "00000001"),
            (100, 49, "000000001"),
        ];
        for &(diff, level, code) in normal {
            let (l, run, neg) = quantize(diff, true, false);
            assert_eq!(l, level, "diff {diff}");
            assert_eq!(code_str(run, neg), code, "diff {diff}");
        }
        for &(diff, level, code) in extra {
            let (l, run, neg) = quantize(diff, true, true);
            assert_eq!(l, level, "diff extra {diff}");
            assert_eq!(code_str(run, neg), code, "diff extra {diff}");
        }
    }

    /// A corrupted code (more than 8 zeros, impossible in a conforming stream)
    /// must be distinguished from a clean end of stream, otherwise mid-stream
    /// corruption is taken for a normal end and silently swallowed.
    #[test]
    fn invalid_vlc_distinguished_from_eof() {
        // Valid code: `01` = level +3 (Table 1).
        let mut w = BitWriter::new();
        w.put_bits(0b01, 2);
        let bytes = w.finish();
        assert!(matches!(read_vlc(&mut BitReader::new(&bytes), false), VlcRead::Sym(_)));

        // `1` followed by 10 zeros then `1`: run = 10 > 8 → corrupted code.
        let mut w = BitWriter::new();
        w.put_bit(true);
        for _ in 0..10 {
            w.put_bit(false);
        }
        w.put_bit(true);
        let bytes = w.finish();
        assert_eq!(read_vlc(&mut BitReader::new(&bytes), false), VlcRead::Invalid);

        // Empty stream: clean end.
        assert_eq!(read_vlc(&mut BitReader::new(&[]), false), VlcRead::Eof);
    }

    /// Table 2 erratum: the 9/10 boundary between +4 and +15.
    #[test]
    fn table2_erratum_boundary() {
        assert_eq!(quantize(9, true, false).0, 4);
        assert_eq!(quantize(10, true, false).0, 15);
    }

    /// Every encoded diff must read back identically, and the EOC must stay
    /// distinguishable — which in effect verifies the code space is prefix-free.
    #[test]
    fn roundtrip_all_diffs_with_eoc() {
        for subsampled in [false, true] {
            for extra in [false, true] {
                if extra && !subsampled {
                    continue;
                }
                let mut w = BitWriter::new();
                let mut expect = Vec::new();
                for diff in -255i16..=255 {
                    let (level, run, neg) = quantize(diff, subsampled, extra);
                    write_code(&mut w, run, neg);
                    expect.push(Vlc::Level { level, extra });
                    write_eoc(&mut w);
                    expect.push(Vlc::Eoc);
                }
                let bytes = w.finish();
                let mut r = BitReader::new(&bytes);
                for (i, e) in expect.iter().enumerate() {
                    let got = read_vlc(&mut r, subsampled);
                    assert_eq!(got, VlcRead::Sym(*e), "symbol {i} (sub={subsampled} extra={extra})");
                }
            }
        }
    }

    /// The set of valid codes of a table is prefix-free: no string is a prefix
    /// of another.
    #[test]
    fn prefix_freeness_exhaustive() {
        // All possible codes: positive k=1..8, negative m=0..8 (m=2 = EOC).
        let mut codes: Vec<String> = Vec::new();
        for k in 1..=8u8 {
            codes.push(code_str(k, false));
        }
        for m in 0..=8u8 {
            codes.push(code_str(m, true));
        }
        for (i, a) in codes.iter().enumerate() {
            for (j, b) in codes.iter().enumerate() {
                if i != j {
                    assert!(!b.starts_with(a.as_str()), "{a} is a prefix of {b}");
                }
            }
        }
    }
}
