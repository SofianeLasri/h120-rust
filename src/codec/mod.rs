//! Types et constantes partagés du codec H.120 (clause 1).
//!
//! Toutes les références « §x.y » renvoient à la Recommandation ITU-T H.120 (03/93).

pub mod bitio;
pub mod decoder;
pub mod encoder;
pub mod tables;

/// Échantillons de luminance par ligne active (§1.4.1.1).
pub const WIDTH: usize = 256;
/// Échantillons de chrominance par ligne active (§1.4.2.1).
pub const CHROMA_WIDTH: usize = 52;
/// Adresse du premier échantillon de chrominance (§1.5.4).
pub const CHROMA_ADDR_BASE: usize = 4;
/// Lignes actives par champ (§1.4.1.2).
pub const LINES_PER_FIELD: usize = 143;
/// Lignes actives par image (2 champs).
pub const LINES_PER_FRAME: usize = 2 * LINES_PER_FIELD;

/// Niveau du noir (§1.4.1.1).
pub const Y_MIN: u8 = 16;
/// Niveau du blanc (§1.4.1.1).
pub const Y_MAX: u8 = 239;
/// Plage légale de la chrominance : 128 ± 111 (§1.4.2.1).
pub const C_MIN: u8 = 17;
pub const C_MAX: u8 = 239;
/// Niveau supposé du blanking ligne/trame pour la prédiction (§1.4.1.3.1).
pub const BLANKING: u8 = 128;

/// Taille du buffer de transmission : 96 kbit, 1 K = 1024 bits (§1.5.1).
pub const BUFFER_BITS: usize = 96 * 1024;

/// Écart minimal entre la fin d'un cluster et le début du suivant (§1.5.3).
pub const MIN_CLUSTER_GAP: usize = 4;

/// Champ reconstruit, identique dans l'encodeur et le décodeur.
///
/// Chaque ligne de chrominance ne stocke que la composante transmise sur
/// cette ligne : (B'−Y') ou (R'−Y') en alternance (§1.4.2.1).
#[derive(Clone)]
pub struct FieldStore {
    /// Luminance, `LINES_PER_FIELD` lignes de `WIDTH` échantillons.
    pub y: Vec<[u8; WIDTH]>,
    /// Chrominance, `LINES_PER_FIELD` lignes de `CHROMA_WIDTH` échantillons.
    pub c: Vec<[u8; CHROMA_WIDTH]>,
    /// Zones mobiles de luminance du dernier champ codé (pour
    /// l'interpolation des champs omis, §1.4.1.4.2).
    pub y_moving: Vec<[bool; WIDTH]>,
    /// Zones mobiles de chrominance (§1.4.2.4).
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

/// Composante chroma portée par une ligne donnée d'un champ donné.
///
/// 1re ligne active du champ 1 : (B'−Y'), 1re ligne du champ 2 : (R'−Y'),
/// puis alternance (§1.4.2.1).
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

/// Numéro de ligne « spec » (0..142 champ 1, 144..286 champ 2), §1.5.2.1.
#[inline]
pub fn spec_line_number(field: usize, line: usize) -> usize {
    if field == 0 { line } else { 144 + line }
}

/// Valeur de l'élément D (voisin haut-droit de X sur la ligne précédente du
/// même champ, Figure 1). Si D appartient à une zone mobile
/// sous-échantillonnée et n'a pas été transmis dans la trame courante, il
/// est remplacé par C, l'élément directement au-dessus de X (§1.4.1.4.1).
/// La première ligne d'un champ prédit depuis le blanking à 128.
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

/// Prédiction DPCM de luminance X = (A + D) / 2, division tronquée (§1.4.1.3.1).
#[inline]
pub fn predict_luma(a: u8, d: u8) -> u8 {
    ((a as u16 + d as u16) / 2) as u8
}

/// Interpole un champ omis (§1.4.1.4.2 et §1.4.2.4).
///
/// `b1` est le champ transmis précédant le champ omis, `a1` le champ
/// transmis le suivant (tous deux de parité opposée au champ omis).
/// Un élément x du champ omis est estimé mobile si l'un des quatre éléments
/// voisins a/b/c/d (au-dessus et en dessous, dans b1 et a1) est mobile ;
/// dans ce cas seulement il est interpolé, sinon il reste inchangé.
/// Cette fonction est appliquée à l'identique par l'encodeur et le décodeur.
pub fn interpolate_omitted_field(omitted: &mut FieldStore, omitted_parity: usize, b1: &FieldStore, a1: &FieldStore) {
    // Lignes du champ transmis encadrant la ligne j du champ omis :
    // champ 2 omis : au-dessus = ligne j du champ 1, en dessous = ligne j+1 ;
    // champ 1 omis : au-dessus = ligne j−1 du champ 2, en dessous = ligne j.
    let bracket = |j: usize| -> (Option<usize>, Option<usize>) {
        if omitted_parity == 1 {
            (Some(j), if j + 1 < LINES_PER_FIELD { Some(j + 1) } else { None })
        } else {
            (j.checked_sub(1), Some(j))
        }
    };
    for j in 0..LINES_PER_FIELD {
        let (above, below) = bracket(j);
        // Luminance : x = ((a+b)/2 + (c+d)/2) / 2, divisions tronquées.
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
        // Chrominance : x = (a+c)/2 (champ 1) ou (b+d)/2 (champ 2), les
        // lignes choisies portant la même composante que la ligne omise.
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
