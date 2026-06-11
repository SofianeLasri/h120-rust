//! Décodeur H.120 clause 1 : parseur du multiplex vidéo et reconstruction.
//!
//! Le décodeur est strictement déterminé par la spec : LST/FST (§1.5.2),
//! adressage des clusters (§1.5.3), données couleur (§1.5.4), lignes PCM
//! (§1.5.5), DPCM (§1.4.1.3), sous-échantillonnages (§1.4.1.4).

use super::bitio::BitReader;
use super::tables::{self, Vlc};
use super::{
    BLANKING, CHROMA_ADDR_BASE, CHROMA_WIDTH, FieldStore, LINES_PER_FIELD, WIDTH, clamp_c,
    clamp_y, d_value, interpolate_omitted_field, predict_luma, spec_line_number,
};
use anyhow::{Result, bail};

#[derive(Default, Debug, Clone)]
pub struct DecStats {
    pub frames: u64,
    pub fields_decoded: u64,
    pub fields_omitted: u64,
    pub pcm_lines: u64,
    pub empty_lines: u64,
    pub moving_lines: u64,
    pub subsampled_lines: u64,
    pub luma_clusters: u64,
    pub chroma_clusters: u64,
    pub extra_elements: u64,
    pub a_flag_fields: u64,
    pub total_bits: u64,
}

/// LST brut : 12 zéros + 1 + AAA + S + code de ligne (Figure 4 et §1.5.2.1).
struct RawLst {
    aaa: u32,
    s: bool,
    line_code: u32,
}

enum LstRead {
    Eof,
    Invalid,
    Lst(RawLst),
}

struct FstInfo {
    /// 0 = FST-1 (champ 1), 1 = FST-2 (champ 2).
    field: usize,
    /// Bit A : buffer émetteur < 6 kbit (Figure 4).
    a_flag: bool,
    /// Bit S de la première ligne du champ.
    s_first: bool,
}

/// Fin de cluster : code EOC explicite ou synchronisation (EOC omis sur le
/// dernier cluster de la ligne, §1.4.1.3.2).
enum ClusterEnd {
    Eoc,
    Sync,
}

struct EndInfo {
    span_end: usize,
    kind: ClusterEnd,
}

pub struct Decoder<'a> {
    r: BitReader<'a>,
    pub store: [FieldStore; 2],
    /// Numéro (0/1) du dernier FST vu, pour détecter les champs omis.
    last_fst: Option<usize>,
    synced: bool,
    /// Éléments mobiles non transmis de la ligne précédente (D → C).
    prev_not_trans: [bool; WIDTH],
    pub stats: DecStats,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Decoder {
            r: BitReader::new(data),
            store: [FieldStore::new(), FieldStore::new()],
            last_fst: None,
            synced: false,
            prev_not_trans: [false; WIDTH],
            stats: DecStats::default(),
        }
    }

    /// Décode jusqu'à la prochaine image complète (paire de champs 1 + 2).
    /// Renvoie `None` à la fin du flux.
    pub fn next_frame(&mut self) -> Result<Option<[FieldStore; 2]>> {
        loop {
            if !self.synced {
                if !self.sync_to_fst() {
                    return Ok(None);
                }
                self.synced = true;
            }
            let Some(fst) = self.read_fst()? else {
                self.stats.total_bits = self.r.bit_pos();
                return Ok(None);
            };
            if fst.a_flag {
                self.stats.a_flag_fields += 1;
            }
            let f = fst.field;
            // Deux FST consécutifs de même numéro : le champ opposé a été
            // omis et doit être interpolé (§1.5.2.2).
            let omitted = self.last_fst == Some(f);
            self.last_fst = Some(f);

            // Snapshot B1 : état du champ de même parité avant ce décodage,
            // nécessaire à l'interpolation du champ omis et à l'émission.
            let b1 = self.store[f].clone();
            let complete = self.decode_field(f, fst.s_first)?;
            self.stats.total_bits = self.r.bit_pos();
            if !complete {
                return Ok(None);
            }
            self.stats.fields_decoded += 1;

            if omitted {
                self.stats.fields_omitted += 1;
                let (s0, s1) = self.store.split_at_mut(1);
                let (omitted_store, a1) =
                    if f == 0 { (&mut s1[0], &s0[0]) } else { (&mut s0[0], &s1[0]) };
                interpolate_omitted_field(omitted_store, 1 - f, &b1, a1);
            }

            // Émission : après un champ 2 (image complète), ou après un
            // champ 1 révélant l'omission du champ 2 précédent — l'image
            // émise est alors (B1, champ 2 interpolé).
            if f == 1 {
                self.stats.frames += 1;
                return Ok(Some([self.store[0].clone(), self.store[1].clone()]));
            } else if omitted {
                self.stats.frames += 1;
                return Ok(Some([b1, self.store[1].clone()]));
            }
        }
    }

    /// Cherche le premier FST du flux (synchronisation initiale).
    fn sync_to_fst(&mut self) -> bool {
        let mut start = self.r.bit_pos();
        loop {
            if self.r.remaining() < 48 {
                return false;
            }
            self.r.seek(start);
            if self.looks_like_fst() {
                self.r.seek(start);
                return true;
            }
            start += 1;
        }
    }

    /// Vrai si la position courante porte un FST plausible (consomme).
    fn looks_like_fst(&mut self) -> bool {
        let LstRead::Lst(lst1) = self.read_raw_lst() else { return false };
        if lst1.line_code != 0b111 || !(lst1.aaa == 0 || lst1.aaa == 0b111) {
            return false;
        }
        let Some(mid) = self.r.read_bits(8) else { return false };
        if !(mid == 0b0000_1111 || mid == 0b0000_0110) {
            return false;
        }
        // Le bit F apparaît en position S du premier LST et dans l'octet.
        if ((mid >> 3) & 1 == 1) != lst1.s {
            return false;
        }
        matches!(self.read_raw_lst(), LstRead::Lst(l) if l.line_code == 0 && l.aaa == 0)
    }

    /// Lit 20 bits de LST à la position courante.
    fn read_raw_lst(&mut self) -> LstRead {
        let Some(prefix) = self.r.read_bits(13) else { return LstRead::Eof };
        if prefix != 1 {
            return LstRead::Invalid;
        }
        let (Some(aaa), Some(s), Some(line_code)) =
            (self.r.read_bits(3), self.r.read_bits(1), self.r.read_bits(3))
        else {
            return LstRead::Eof;
        };
        LstRead::Lst(RawLst { aaa, s: s == 1, line_code })
    }

    /// Lit un FST complet (Figure 4). `None` en fin de flux.
    fn read_fst(&mut self) -> Result<Option<FstInfo>> {
        if self.r.remaining() < 48 {
            return Ok(None);
        }
        let lst1 = match self.read_raw_lst() {
            LstRead::Eof => return Ok(None),
            LstRead::Invalid => bail!("FST attendu au bit {}", self.r.bit_pos() - 13),
            LstRead::Lst(l) => l,
        };
        if lst1.line_code != 0b111 {
            bail!("FST : code de ligne 111 attendu, trouvé {:03b}", lst1.line_code);
        }
        let a_flag = lst1.aaa == 0b111;
        let Some(mid) = self.r.read_bits(8) else { return Ok(None) };
        let field = match mid {
            0b0000_1111 => 0,
            0b0000_0110 => 1,
            _ => bail!("octet central de FST invalide : {mid:08b}"),
        };
        let lst2 = match self.read_raw_lst() {
            LstRead::Eof => return Ok(None),
            LstRead::Invalid => bail!("second LST du FST invalide"),
            LstRead::Lst(l) => l,
        };
        if lst2.line_code != 0 {
            bail!("second LST du FST : code de ligne 000 attendu");
        }
        Ok(Some(FstInfo { field, a_flag, s_first: lst2.s }))
    }

    /// Décode les 143 lignes d'un champ. `false` si le flux s'épuise.
    fn decode_field(&mut self, f: usize, s_first: bool) -> Result<bool> {
        self.store[f].clear_moving();
        self.prev_not_trans = [false; WIDTH];
        let mut s = s_first;
        for l in 0..LINES_PER_FIELD {
            if l > 0 {
                let lst = match self.read_raw_lst() {
                    LstRead::Eof => return Ok(false),
                    LstRead::Invalid => bail!(
                        "LST de la ligne {} introuvable (désynchronisation)",
                        spec_line_number(f, l)
                    ),
                    LstRead::Lst(lst) => lst,
                };
                let expected = (spec_line_number(f, l) & 7) as u32;
                if lst.aaa != 0 || lst.line_code != expected {
                    bail!(
                        "LST inattendu (ligne {}, code {:03b} au lieu de {:03b})",
                        spec_line_number(f, l),
                        lst.line_code,
                        expected
                    );
                }
                s = lst.s;
            }
            if !self.decode_line(f, l, s)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Décode le contenu d'une ligne (après son LST).
    fn decode_line(&mut self, f: usize, l: usize, subsampled: bool) -> Result<bool> {
        let Some(first) = self.r.peek_bits(8) else {
            // Fin de flux : seul reste le bourrage (< 8 bits). La ligne est
            // vide, le champ peut se terminer normalement.
            self.stats.empty_lines += 1;
            self.prev_not_trans = [false; WIDTH];
            return Ok(true);
        };

        if first == 0xFF {
            return self.decode_pcm_line(f, l);
        }
        if self.at_sync() {
            self.stats.empty_lines += 1;
            self.prev_not_trans = [false; WIDTH];
            return Ok(true);
        }
        self.stats.moving_lines += 1;
        if subsampled {
            self.stats.subsampled_lines += 1;
        }

        let parity = spec_line_number(f, l) & 1;
        let prev_y: Option<[u8; WIDTH]> = if l > 0 { Some(self.store[f].y[l - 1]) } else { None };
        let prev_not_trans = self.prev_not_trans;
        let mut not_trans = [false; WIDTH];

        let mut in_chroma = first == 0b0000_1001;
        if in_chroma {
            self.r.read_bits(8);
        }

        loop {
            if self.at_sync() {
                break;
            }
            if !in_chroma {
                // Cluster de luminance : PCM, adresse, codes VLC (§1.5.3).
                let Some(pcm) = self.r.read_bits(8) else { return Ok(false) };
                let Some(addr) = self.r.read_bits(8) else { return Ok(false) };
                let addr = addr as usize;
                if addr >= WIDTH - 1 {
                    bail!("adresse de cluster luma invalide : {addr}");
                }
                self.stats.luma_clusters += 1;
                let mut line = self.store[f].y[l];
                line[addr] = clamp_y(pcm as i16);
                let Some(end) = self.decode_luma_codes(
                    addr,
                    parity,
                    subsampled,
                    &mut line,
                    prev_y.as_ref(),
                    &prev_not_trans,
                    &mut not_trans,
                )?
                else {
                    return Ok(false);
                };
                self.store[f].y[l] = line;
                for e in addr..=end.span_end {
                    self.store[f].y_moving[l][e] = true;
                }
                match end.kind {
                    ClusterEnd::Eoc => {
                        if self.r.peek_bits(8) == Some(0b0000_1001) {
                            self.r.read_bits(8);
                            in_chroma = true;
                        }
                    }
                    ClusterEnd::Sync => break,
                }
            } else {
                // Cluster de chrominance après l'échappement couleur (§1.5.4).
                let Some(pcm) = self.r.read_bits(8) else { return Ok(false) };
                let Some(addr) = self.r.read_bits(8) else { return Ok(false) };
                let addr = addr as usize;
                if !(CHROMA_ADDR_BASE..CHROMA_ADDR_BASE + CHROMA_WIDTH - 1).contains(&addr) {
                    bail!("adresse de cluster chroma invalide : {addr}");
                }
                let sample = addr - CHROMA_ADDR_BASE;
                self.stats.chroma_clusters += 1;
                let mut line = self.store[f].c[l];
                line[sample] = clamp_c(pcm as i16);
                let Some(end) = self.decode_chroma_codes(sample, parity, subsampled, &mut line)?
                else {
                    return Ok(false);
                };
                self.store[f].c[l] = line;
                for e in sample..=end.span_end {
                    self.store[f].c_moving[l][e] = true;
                }
                if matches!(end.kind, ClusterEnd::Sync) {
                    break;
                }
            }
        }

        self.store[f].y[l][WIDTH - 1] = BLANKING;
        self.prev_not_trans = not_trans;
        Ok(true)
    }

    /// Codes VLC d'un cluster de luminance. Prédiction X = (A+D)/2 avec les
    /// substitutions A→AS et D→C du sous-échantillonnage (§1.4.1.3.1,
    /// §1.4.1.4.1). Renvoie `None` si le flux s'épuise.
    #[allow(clippy::too_many_arguments)]
    fn decode_luma_codes(
        &mut self,
        start: usize,
        parity: usize,
        subsampled: bool,
        line: &mut [u8; WIDTH],
        prev: Option<&[u8; WIDTH]>,
        prev_not_trans: &[bool; WIDTH],
        not_trans: &mut [bool; WIDTH],
    ) -> Result<Option<EndInfo>> {
        let max_e = WIDTH - 2;
        let mut q = start; // dernière position « normale »
        let mut last = start; // dernière position transmise
        loop {
            if self.at_sync() {
                return Ok(Some(EndInfo { span_end: last, kind: ClusterEnd::Sync }));
            }
            let Some(sym) = tables::read_vlc(&mut self.r, subsampled) else {
                return Ok(None);
            };
            match sym {
                Vlc::Eoc => {
                    return Ok(Some(EndInfo { span_end: last, kind: ClusterEnd::Eoc }));
                }
                Vlc::Level { level, extra: true } => {
                    let o = q + 1;
                    if o > max_e || last == o {
                        bail!("élément extra hors cluster (position {o})");
                    }
                    self.stats.extra_elements += 1;
                    let pred = predict_luma(line[q], d_value(prev, prev_not_trans, o));
                    line[o] = clamp_y(pred as i16 + level);
                    last = o;
                }
                Vlc::Level { level, extra: false } => {
                    let (t, omitted) = if subsampled {
                        if q % 2 == parity {
                            // L'élément intermédiaire a-t-il été transmis
                            // comme « extra » ?
                            (q + 2, if last == q + 1 { None } else { Some(q + 1) })
                        } else {
                            (q + 1, None)
                        }
                    } else {
                        (q + 1, None)
                    };
                    if t > max_e {
                        bail!("cluster luma au-delà de la ligne (position {t})");
                    }
                    // Substitution A → AS si A n'a pas été transmis.
                    let a = if omitted.is_some() { line[q] } else { line[t - 1] };
                    let pred = predict_luma(a, d_value(prev, prev_not_trans, t));
                    line[t] = clamp_y(pred as i16 + level);
                    if let Some(o) = omitted {
                        // Interpolation des éléments omis (§1.4.1.4.1).
                        line[o] = ((line[q] as u16 + line[t] as u16) / 2) as u8;
                        not_trans[o] = true;
                    }
                    q = t;
                    last = t;
                }
            }
        }
    }

    /// Codes VLC d'un cluster de chrominance. Prédiction X = A (§1.4.2.3.1).
    fn decode_chroma_codes(
        &mut self,
        start: usize,
        parity: usize,
        subsampled: bool,
        line: &mut [u8; CHROMA_WIDTH],
    ) -> Result<Option<EndInfo>> {
        let max_e = CHROMA_WIDTH - 1;
        let mut q = start;
        let mut last = start;
        loop {
            if self.at_sync() {
                return Ok(Some(EndInfo { span_end: last, kind: ClusterEnd::Sync }));
            }
            let Some(sym) = tables::read_vlc(&mut self.r, subsampled) else {
                return Ok(None);
            };
            match sym {
                Vlc::Eoc => {
                    return Ok(Some(EndInfo { span_end: last, kind: ClusterEnd::Eoc }));
                }
                Vlc::Level { level, extra: true } => {
                    let o = q + 1;
                    if o > max_e || last == o {
                        bail!("élément extra chroma hors cluster (position {o})");
                    }
                    self.stats.extra_elements += 1;
                    let pred = line[q];
                    line[o] = clamp_c(pred as i16 + level);
                    last = o;
                }
                Vlc::Level { level, extra: false } => {
                    let (t, omitted) = if subsampled {
                        if q % 2 == parity {
                            (q + 2, if last == q + 1 { None } else { Some(q + 1) })
                        } else {
                            (q + 1, None)
                        }
                    } else {
                        (q + 1, None)
                    };
                    if t > max_e {
                        bail!("cluster chroma au-delà de la ligne (position {t})");
                    }
                    let pred = if omitted.is_some() { line[q] } else { line[t - 1] };
                    line[t] = clamp_c(pred as i16 + level);
                    if let Some(o) = omitted {
                        line[o] = ((line[q] as u16 + line[t] as u16) / 2) as u8;
                    }
                    q = t;
                    last = t;
                }
            }
        }
    }

    /// Ligne PCM (Figure 6) : marqueurs 0xFF 0xFF, 256 octets de luminance,
    /// 52 octets de chrominance (§1.5.5).
    fn decode_pcm_line(&mut self, f: usize, l: usize) -> Result<bool> {
        self.r.read_bits(8); // 0xFF
        let Some(marker) = self.r.read_bits(8) else { return Ok(false) };
        if marker != 0xFF {
            bail!("ligne PCM : adresse invalide 0xFF attendue, trouvé {marker:08b}");
        }
        self.stats.pcm_lines += 1;
        for e in 0..WIDTH {
            let Some(v) = self.r.read_bits(8) else { return Ok(false) };
            self.store[f].y[l][e] = v as u8;
        }
        self.store[f].y[l][WIDTH - 1] = BLANKING;
        for e in 0..CHROMA_WIDTH {
            let Some(v) = self.r.read_bits(8) else { return Ok(false) };
            self.store[f].c[l][e] = v as u8;
        }
        // Les lignes PCM sont non mobiles (§1.5.5).
        self.store[f].y_moving[l] = [false; WIDTH];
        self.store[f].c_moving[l] = [false; CHROMA_WIDTH];
        self.prev_not_trans = [false; WIDTH];
        Ok(true)
    }

    /// Vrai si la position courante est un code de synchronisation
    /// (≥ 12 zéros : aucun code VLC ni valeur PCM légale ne commence ainsi).
    fn at_sync(&mut self) -> bool {
        match self.r.peek_bits(12) {
            Some(v) => v == 0,
            None => true,
        }
    }
}
