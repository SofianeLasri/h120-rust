//! Lois de quantification et codes à longueur variable (Tables 1 et 2/H.120).
//!
//! Tous les codes des deux tables ont l'une des deux formes :
//!   - côté positif : k zéros suivis d'un 1          (k = 1..8)
//!   - côté négatif : un 1, m zéros, puis un 1       (m = 0..8)
//!
//! Le code de fin de cluster EOC = 1001 correspond à m = 2 (code n° 11,
//! §1.4.1.3.2) dans les deux tables, ce qui rend l'ensemble préfixe-libre.
//!
//! Erratum : la Table 2 imprime « 0 to 22 » pour le niveau +15 ; il faut
//! lire « 10 to 22 » (sinon les plages 0..9 et 0..22 se chevauchent).

use super::bitio::{BitReader, BitWriter};

/// Symbole décodé d'une zone mobile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vlc {
    /// Niveau de sortie DPCM. `extra` n'a de sens que sur les lignes
    /// sous-échantillonnées (élément « extra », §1.4.1.4.1).
    Level { level: i16, extra: bool },
    /// Fin de cluster.
    Eoc,
}

/// Quantification Table 1 : (seuil haut inclus sur |diff|, niveau, run).
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

/// Quantification Table 2 : (seuil haut inclus sur |diff|, niveau, run
/// élément normal, run élément extra).
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

/// Décodage Table 1, côté positif : indice = k − 1.
const T1_DEC_POS: [i16; 8] = [3, 12, 23, 38, 57, 80, 107, 140];
/// Décodage Table 1, côté négatif : indice = m (m = 2 → EOC).
const T1_DEC_NEG: [i16; 9] = [-4, -13, 0, -24, -39, -58, -81, -108, -141];
/// Décodage Table 2, côté positif : indice = k − 1, (niveau, extra).
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
/// Décodage Table 2, côté négatif : indice = m (m = 2 → EOC).
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

/// Quantifie une erreur de prédiction et renvoie (niveau, run, côté négatif).
/// `diff` est dans [−255, 255] (§1.4.1.3.2 : 511 niveaux d'entrée).
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
        debug_assert!(!extra, "pas d'éléments extra hors sous-échantillonnage");
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
    unreachable!("plages de quantification exhaustives")
}

/// Écrit un code VLC : côté positif `0^k 1`, côté négatif `1 0^m 1`.
pub fn write_code(w: &mut BitWriter, run: u8, negative: bool) {
    if negative {
        w.put_bit(true);
    }
    for _ in 0..run {
        w.put_bit(false);
    }
    w.put_bit(true);
}

/// Écrit le code de fin de cluster (1001).
pub fn write_eoc(w: &mut BitWriter) {
    write_code(w, 2, true);
}

/// Lit un symbole VLC. Renvoie `None` sur fin de flux ou code invalide
/// (run > 8, ce qui ne peut pas se produire dans un flux conforme : la
/// détection des codes de synchronisation se fait avant l'appel).
pub fn read_vlc(r: &mut BitReader, subsampled: bool) -> Option<Vlc> {
    let negative = r.read_bit()?;
    let mut run: u8 = 0;
    loop {
        if r.read_bit()? {
            break;
        }
        run += 1;
        if run > 8 {
            return None;
        }
    }
    if negative {
        // `1 1` correspond à run = 0 ; `1 0^m 1` à run = m.
        if run == 2 {
            return Some(Vlc::Eoc);
        }
        if subsampled {
            let (level, extra) = T2_DEC_NEG[run as usize];
            Some(Vlc::Level { level, extra })
        } else {
            Some(Vlc::Level { level: T1_DEC_NEG[run as usize], extra: false })
        }
    } else {
        // `0^k 1` : run = k − 1 zéros comptés après le premier bit lu.
        // Premier bit lu = 0 (negative=false), donc k = run + 1.
        let k = run + 1;
        if k > 8 {
            return None;
        }
        if subsampled {
            let (level, extra) = T2_DEC_POS[(k - 1) as usize];
            Some(Vlc::Level { level, extra })
        } else {
            Some(Vlc::Level { level: T1_DEC_POS[(k - 1) as usize], extra: false })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconstruit la chaîne de bits d'un code pour comparaison avec la spec.
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
        // (diff représentatif, niveau, code attendu) — Table 1/H.120.
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
        // Éléments normaux puis extra — Table 2/H.120.
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

    /// Erratum Table 2 : frontière 9/10 entre +4 et +15.
    #[test]
    fn table2_erratum_boundary() {
        assert_eq!(quantize(9, true, false).0, 4);
        assert_eq!(quantize(10, true, false).0, 15);
    }

    /// Tout diff encodé doit se relire à l'identique, et l'EOC doit rester
    /// distinguable — vérifie de fait que l'espace de codes est préfixe-libre.
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
                    assert_eq!(got.as_ref(), Some(e), "symbole {i} (sub={subsampled} extra={extra})");
                }
            }
        }
    }

    /// L'ensemble des codes valides d'une table est préfixe-libre :
    /// aucune chaîne n'est préfixe d'une autre.
    #[test]
    fn prefix_freeness_exhaustive() {
        // Tous les codes possibles : positifs k=1..8, négatifs m=0..8 (m=2 = EOC).
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
                    assert!(!b.starts_with(a.as_str()), "{a} préfixe de {b}");
                }
            }
        }
    }
}
