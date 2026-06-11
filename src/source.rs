//! Pré- et post-traitement : conversion entre images Y4M et le format
//! source H.120 (luma 256×143/champ, chroma 52 éch./ligne en alternance
//! B'−Y' / R'−Y', §1.4.1 et §1.4.2).
//!
//! Les caractéristiques exactes des pré/post-filtres sont laissées libres
//! par la spec ; on utilise un redimensionnement bilinéaire.

use crate::codec::{
    BLANKING, C_MAX, C_MIN, CHROMA_WIDTH, ChromaComp, FieldStore, LINES_PER_FIELD,
    LINES_PER_FRAME, WIDTH, Y_MAX, Y_MIN, chroma_comp,
};
use crate::scale::resize_plane;
use crate::y4m::Frame444;

/// Hauteur de l'image affichée : 286 lignes tissées + 2 lignes de bourrage
/// (pour une hauteur paire, pratique pour les conversions en aval).
pub const OUT_HEIGHT: usize = 288;
/// Rapport d'aspect des pixels : l'image 256×286 couvre un écran 4:3,
/// soit (4/3) / (256/286) = 1144/768 = 143/96.
pub const PAR: (u32, u32) = (143, 96);

/// Un champ d'entrée prêt à être codé.
pub struct FieldInput {
    pub y: Vec<[u8; WIDTH]>,
    /// La composante transmise sur chaque ligne (Cb ou Cr selon la ligne).
    pub c: Vec<[u8; CHROMA_WIDTH]>,
}

/// Convertit une image en deux champs au format source H.120.
/// Champ 1 = lignes paires de l'image, champ 2 = lignes impaires (Figure 3).
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
            // Le dernier élément de chaque ligne active est fixé à 128
            // dans l'encodeur et le décodeur (§1.4.1.1).
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

/// Reconstruit une image affichable à partir des deux champs décodés :
/// tissage des champs, interpolation de la composante chroma absente de
/// chaque ligne (§1.4.2.1) puis sur-échantillonnage horizontal 52 → 256.
pub fn egress(store: &[FieldStore; 2]) -> Frame444 {
    let mut out = Frame444::new(WIDTH, OUT_HEIGHT);
    for line in out.y.iter_mut() {
        *line = Y_MIN;
    }
    for f in 0..2 {
        for i in 0..LINES_PER_FIELD {
            let dst_line = 2 * i + f;
            out.y[dst_line * WIDTH..(dst_line + 1) * WIDTH].copy_from_slice(&store[f].y[i]);

            // Composante présente sur cette ligne + interpolation de l'autre
            // à partir des lignes voisines du même champ (qui la portent).
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

/// Agrandissement entier (plus proche voisin) pour l'affichage/export.
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
        // Champ 1 ligne 0 porte Cb, ligne 1 porte Cr.
        assert_eq!(fields[0].c[0][10], 90);
        assert_eq!(fields[0].c[1][10], 160);
        // Champ 2 ligne 0 porte Cr.
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
