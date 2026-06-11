//! Encodeur H.120 clause 1 : conditional replenishment + DPCM + VLC.
//!
//! La spec impose le bitstream et les reconstructions ; elle laisse libres
//! le détecteur de mouvement, le contrôle de débit et la stratégie de
//! rafraîchissement (§1.4.1.3, Appendice I). Les choix faits ici sont
//! documentés dans docs/DEVIATIONS.md.

use super::bitio::BitWriter;
use super::tables;
use super::{
    BLANKING, BUFFER_BITS, CHROMA_ADDR_BASE, CHROMA_WIDTH, FieldStore, LINES_PER_FIELD,
    MIN_CLUSTER_GAP, WIDTH, clamp_c, clamp_y, d_value, interpolate_omitted_field, predict_luma,
    spec_line_number,
};
use crate::source::{FieldInput, ingest};
use crate::y4m::Frame444;

/// Coût approximatif d'une ligne PCM : LST + 2 octets de signalisation
/// + 256 octets luma + 52 octets chroma.
const PCM_LINE_BITS: f64 = (20 + 16 + 256 * 8 + 52 * 8) as f64;

/// Seuils de pilotage par l'occupation du buffer (fractions de 96 kbit).
const SUBSAMPLE_AT: f64 = 0.55;
const FIELD_SKIP_AT: f64 = 0.72;
const PANIC_AT: f64 = 0.97;
// Les éléments extra n'existent que sur les lignes sous-échantillonnées
// (buffer > SUBSAMPLE_AT) : on les permet en début de zone de subsampling
// pour adoucir la transition (§1.4.1.4.1), plus quand le buffer se remplit.
const EXTRA_OK_BELOW: f64 = 0.65;
const PCM_REFRESH_BELOW: f64 = 0.45;
const BOOTSTRAP_FILL_TO: f64 = 0.70;

/// Erreur d'interpolation à partir de laquelle un élément « extra » est
/// transmis sur les lignes sous-échantillonnées (§1.4.1.4.1).
const EXTRA_THRESHOLD: i16 = 12;

pub struct EncoderConfig {
    /// Débit de vidange du buffer virtuel, en bit/s.
    pub bitrate: u64,
    /// Chrominance neutre (image monochrome, flux toujours au format couleur).
    pub mono: bool,
}

#[derive(Default, Debug, Clone)]
pub struct EncStats {
    pub frames: u64,
    pub fields_coded: u64,
    pub fields_omitted: u64,
    pub pcm_lines: u64,
    pub empty_lines: u64,
    pub subsampled_lines: u64,
    pub luma_clusters: u64,
    pub chroma_clusters: u64,
    pub extra_elements: u64,
    pub panic_lines: u64,
    pub max_occupancy: f64,
}

pub struct Encoder {
    cfg: EncoderConfig,
    w: BitWriter,
    store: [FieldStore; 2],
    /// Occupation du buffer de transmission virtuel (bits), §1.5.1.
    occupancy: f64,
    drain_per_line: f64,
    /// Pointeur de rafraîchissement cyclique par champ.
    refresh_ptr: [usize; 2],
    /// Lignes jamais rafraîchies (amorçage du décodeur).
    refreshed: [Vec<bool>; 2],
    /// Snapshot du champ 1 (B1) quand le champ 2 précédent a été omis.
    saved_b1: Option<FieldStore>,
    pub stats: EncStats,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Self {
        let drain_per_line = cfg.bitrate as f64 / (50.0 * LINES_PER_FIELD as f64);
        Encoder {
            cfg,
            w: BitWriter::new(),
            store: [FieldStore::new(), FieldStore::new()],
            occupancy: 0.0,
            drain_per_line,
            refresh_ptr: [0, 0],
            refreshed: [vec![false; LINES_PER_FIELD], vec![false; LINES_PER_FIELD]],
            saved_b1: None,
            stats: EncStats::default(),
        }
    }

    fn occ_ratio(&self) -> f64 {
        self.occupancy / BUFFER_BITS as f64
    }

    fn drain(&mut self, line_slots: usize) {
        self.occupancy = (self.occupancy - self.drain_per_line * line_slots as f64).max(0.0);
    }

    fn charge(&mut self, bits: u64) {
        self.occupancy += bits as f64;
        if self.occupancy > self.stats.max_occupancy {
            self.stats.max_occupancy = self.occupancy;
        }
    }

    /// Encode une image (deux champs). L'image est convertie au format
    /// source H.120 par `source::ingest`.
    pub fn encode_frame(&mut self, frame: &Frame444) {
        let fields = ingest(frame, self.cfg.mono);
        self.stats.frames += 1;

        self.encode_field(0, &fields[0]);

        // Si le champ 2 de l'image précédente a été omis, le décodeur
        // l'interpole après avoir reçu ce champ 1 : l'encodeur fait de même
        // pour rester en parfait synchronisme (§1.4.1.4.2).
        if let Some(b1) = self.saved_b1.take() {
            let (f0, f1) = self.store.split_at_mut(1);
            interpolate_omitted_field(&mut f1[0], 1, &b1, &f0[0]);
        }

        if self.occ_ratio() > FIELD_SKIP_AT {
            // Omission du champ 2 : rien n'est émis, le FST-1 suivant
            // (deux FST de même numéro) le signale au décodeur (§1.5.2.2).
            self.saved_b1 = Some(self.store[0].clone());
            self.stats.fields_omitted += 1;
            self.drain(LINES_PER_FIELD);
        } else {
            self.encode_field(1, &fields[1]);
        }
    }

    /// Termine le flux et renvoie les octets.
    pub fn finish(self) -> Vec<u8> {
        self.w.finish()
    }

    pub fn bits_written(&self) -> u64 {
        self.w.bit_len()
    }

    /// État des deux champs reconstruits (pour les tests de synchronisme
    /// encodeur/décodeur).
    pub fn stores(&self) -> &[FieldStore; 2] {
        &self.store
    }

    /// Vrai si l'interpolation d'un champ omis est encore en attente
    /// (elle n'a lieu qu'au codage du champ 1 suivant).
    pub fn has_pending_interpolation(&self) -> bool {
        self.saved_b1.is_some()
    }

    fn encode_field(&mut self, f: usize, input: &FieldInput) {
        self.stats.fields_coded += 1;
        self.store[f].clear_moving();

        let pcm_lines = self.schedule_pcm_lines(f);

        // Masque « zone mobile sous-échantillonnée non transmise » de la
        // ligne précédente, pour la substitution D → C (§1.4.1.4.1).
        let mut prev_not_trans = [false; WIDTH];

        for l in 0..LINES_PER_FIELD {
            let pcm = pcm_lines[l];
            let panic = self.occ_ratio() > PANIC_AT;
            let subsampled = !pcm && !panic && self.occ_ratio() > SUBSAMPLE_AT;

            let before = self.w.bit_len();
            if l == 0 {
                self.write_fst(f, subsampled);
            } else {
                self.write_lst(subsampled, spec_line_number(f, l));
            }

            if pcm {
                self.code_pcm_line(f, l, input);
                prev_not_trans = [false; WIDTH];
            } else if panic {
                // Buffer presque plein : ligne vide (l'image se fige).
                self.stats.panic_lines += 1;
                self.stats.empty_lines += 1;
                prev_not_trans = [false; WIDTH];
            } else {
                prev_not_trans = self.code_moving_line(f, l, input, subsampled, prev_not_trans);
            }

            let bits = self.w.bit_len() - before;
            self.charge(bits);
            self.drain(1);
        }
    }

    /// Choisit les lignes PCM de ce champ (rafraîchissement systématique ou
    /// forcé, §1.5.5). Stratégie : amorçage rapide tant que des lignes n'ont
    /// jamais été transmises, puis une ligne par champ en rotation quand le
    /// buffer le permet.
    fn schedule_pcm_lines(&mut self, f: usize) -> Vec<bool> {
        let mut lines = vec![false; LINES_PER_FIELD];
        let bootstrap = self.refreshed[f].iter().any(|&r| !r);
        let mut budget = if bootstrap {
            let room = (BOOTSTRAP_FILL_TO * BUFFER_BITS as f64 - self.occupancy).max(0.0);
            (room / PCM_LINE_BITS) as usize
        } else if self.occ_ratio() < PCM_REFRESH_BELOW {
            1
        } else {
            0
        };
        let mut ptr = self.refresh_ptr[f];
        let mut visited = 0;
        while budget > 0 && visited < LINES_PER_FIELD {
            if bootstrap {
                // Priorité aux lignes jamais transmises.
                if !self.refreshed[f][ptr] {
                    lines[ptr] = true;
                    budget -= 1;
                }
            } else {
                lines[ptr] = true;
                budget -= 1;
            }
            ptr = (ptr + 1) % LINES_PER_FIELD;
            visited += 1;
        }
        self.refresh_ptr[f] = ptr;
        lines
    }

    /// LST : 0000 0000 0000 1000 + S + 3 bits de poids faible du n° de ligne
    /// (§1.5.2.1).
    fn write_lst(&mut self, s: bool, line_no: usize) {
        self.w.put_bits(0b0000_0000_0000_1000, 16);
        self.w.put_bit(s);
        self.w.put_bits((line_no & 7) as u32, 3);
    }

    /// FST : LST de la ligne 143/287 (F en position S, AAA dans le mot de
    /// synchro), octet 0000F11F, LST de la première ligne du champ suivant
    /// (Figure 4). FST-1 (F=1) précède le champ 1, FST-2 (F=0) le champ 2.
    fn write_fst(&mut self, f: usize, s_first: bool) {
        let fbit = f == 0;
        let a = self.occupancy < 6.0 * 1024.0;
        self.w.put_bits(0b0000_0000_0000_1, 13);
        self.w.put_bits(if a { 0b111 } else { 0b000 }, 3);
        self.w.put_bit(fbit);
        self.w.put_bits(0b111, 3);
        // Octet central 0000F11F.
        self.w.put_bits(0, 4);
        self.w.put_bit(fbit);
        self.w.put_bits(0b11, 2);
        self.w.put_bit(fbit);
        // LST de la ligne 0 ou 144 (3 LSB = 000).
        self.w.put_bits(0b0000_0000_0000_1000, 16);
        self.w.put_bit(s_first);
        self.w.put_bits(0b000, 3);
    }

    /// Ligne PCM (Figure 6) : 0xFF, adresse invalide 0xFF, 255 valeurs PCM,
    /// 10000000 (élément 255 = 128), puis 52 valeurs chroma (§1.5.5).
    fn code_pcm_line(&mut self, f: usize, l: usize, input: &FieldInput) {
        self.stats.pcm_lines += 1;
        self.refreshed[f][l] = true;
        self.w.put_bits(0xFF, 8);
        self.w.put_bits(0xFF, 8);
        for e in 0..WIDTH - 1 {
            self.w.put_bits(input.y[l][e] as u32, 8);
        }
        self.w.put_bits(BLANKING as u32, 8);
        for e in 0..CHROMA_WIDTH {
            self.w.put_bits(input.c[l][e] as u32, 8);
        }
        self.store[f].y[l] = input.y[l];
        self.store[f].y[l][WIDTH - 1] = BLANKING;
        self.store[f].c[l] = input.c[l];
        // Une ligne PCM est non mobile pour l'interpolation de champ (§1.5.5).
        self.store[f].y_moving[l] = [false; WIDTH];
        self.store[f].c_moving[l] = [false; CHROMA_WIDTH];
    }

    /// Détection de mouvement et codage des clusters d'une ligne.
    /// Renvoie le masque des éléments mobiles non transmis (pour D → C).
    fn code_moving_line(
        &mut self,
        f: usize,
        l: usize,
        input: &FieldInput,
        subsampled: bool,
        prev_not_trans: [bool; WIDTH],
    ) -> [bool; WIDTH] {
        let parity = spec_line_number(f, l) & 1;
        let thr = self.motion_threshold();
        let extra_ok = self.occ_ratio() < EXTRA_OK_BELOW;

        // Détection (choix d'implémentation, sans incidence sur l'interop).
        let mut y_clusters =
            detect_clusters(&input.y[l], &self.store[f].y[l], thr, WIDTH - 2, WIDTH - 2);
        let mut c_clusters = detect_clusters(
            &input.c[l],
            &self.store[f].c[l],
            thr + 2,
            CHROMA_WIDTH - 1,
            CHROMA_WIDTH - 2,
        );
        if subsampled {
            adjust_parity(&mut y_clusters, parity, WIDTH - 2);
            adjust_parity(&mut c_clusters, parity, CHROMA_WIDTH - 1);
        }

        if y_clusters.is_empty() && c_clusters.is_empty() {
            self.stats.empty_lines += 1;
            return [false; WIDTH];
        }
        if subsampled {
            self.stats.subsampled_lines += 1;
        }

        // Copies locales pour éviter les double-emprunts sur le store.
        let prev_y: Option<[u8; WIDTH]> = if l > 0 { Some(self.store[f].y[l - 1]) } else { None };
        let mut y_line = self.store[f].y[l];
        let mut c_line = self.store[f].c[l];
        let mut not_trans = [false; WIDTH];

        let n_y = y_clusters.len();
        let has_chroma = !c_clusters.is_empty();
        for (i, &(s0, e1)) in y_clusters.iter().enumerate() {
            self.stats.luma_clusters += 1;
            // PCM du premier élément, puis adresse (Figure sous §1.5.3).
            self.w.put_bits(input.y[l][s0] as u32, 8);
            self.w.put_bits(s0 as u32, 8);
            y_line[s0] = input.y[l][s0];
            if subsampled {
                self.code_dpcm_sub_luma(
                    &input.y[l],
                    &mut y_line,
                    prev_y.as_ref(),
                    &prev_not_trans,
                    s0,
                    e1,
                    extra_ok,
                    &mut not_trans,
                );
            } else {
                for e in s0 + 1..=e1 {
                    let a = y_line[e - 1];
                    let d = d_value(prev_y.as_ref(), &prev_not_trans, e);
                    let pred = predict_luma(a, d);
                    let diff = input.y[l][e] as i16 - pred as i16;
                    let (level, run, neg) = tables::quantize(diff, false, false);
                    tables::write_code(&mut self.w, run, neg);
                    y_line[e] = clamp_y(pred as i16 + level);
                }
            }
            for e in s0..=e1 {
                self.store[f].y_moving[l][e] = true;
            }
            // EOC sauf après le dernier cluster de la ligne ; si des données
            // couleur suivent, le dernier cluster luma garde son EOC (§1.5.4).
            if i + 1 < n_y || has_chroma {
                tables::write_eoc(&mut self.w);
            }
        }

        if has_chroma {
            // Code d'échappement couleur (PCM invalide 00001001, §1.5.4).
            self.w.put_bits(0b0000_1001, 8);
            let n_c = c_clusters.len();
            for (i, &(s0, e1)) in c_clusters.iter().enumerate() {
                self.stats.chroma_clusters += 1;
                self.w.put_bits(input.c[l][s0] as u32, 8);
                self.w.put_bits((s0 + CHROMA_ADDR_BASE) as u32, 8);
                c_line[s0] = input.c[l][s0];
                if subsampled {
                    self.code_dpcm_sub_chroma(&input.c[l], &mut c_line, s0, e1, extra_ok);
                } else {
                    for e in s0 + 1..=e1 {
                        // Prédiction chroma : X = A (§1.4.2.3.1).
                        let pred = c_line[e - 1];
                        let diff = input.c[l][e] as i16 - pred as i16;
                        let (level, run, neg) = tables::quantize(diff, false, false);
                        tables::write_code(&mut self.w, run, neg);
                        c_line[e] = clamp_c(pred as i16 + level);
                    }
                }
                for e in s0..=e1 {
                    self.store[f].c_moving[l][e] = true;
                }
                if i + 1 < n_c {
                    tables::write_eoc(&mut self.w);
                }
            }
        }

        self.store[f].y[l] = y_line;
        self.store[f].y[l][WIDTH - 1] = BLANKING;
        self.store[f].c[l] = c_line;
        not_trans
    }

    /// DPCM d'un cluster luma sous-échantillonné : quinconce, éléments
    /// « extra » facultatifs, interpolation des éléments omis (§1.4.1.4.1).
    /// `s0` et `e1` sont déjà alignés sur la parité de la ligne.
    #[allow(clippy::too_many_arguments)]
    fn code_dpcm_sub_luma(
        &mut self,
        input: &[u8; WIDTH],
        line: &mut [u8; WIDTH],
        prev: Option<&[u8; WIDTH]>,
        prev_not_trans: &[bool; WIDTH],
        s0: usize,
        e1: usize,
        extra_ok: bool,
        not_trans: &mut [bool; WIDTH],
    ) {
        let mut q = s0;
        while q + 2 <= e1 {
            let o = q + 1;
            let t = q + 2;
            // Élément extra si l'interpolation serait trop fausse.
            let interp_est = (line[q] as i16 + input[t] as i16) / 2;
            let mut o_transmitted = false;
            if extra_ok && (input[o] as i16 - interp_est).abs() >= EXTRA_THRESHOLD {
                let pred = predict_luma(line[q], d_value(prev, prev_not_trans, o));
                let diff = input[o] as i16 - pred as i16;
                let (level, run, neg) = tables::quantize(diff, true, true);
                tables::write_code(&mut self.w, run, neg);
                line[o] = clamp_y(pred as i16 + level);
                o_transmitted = true;
                self.stats.extra_elements += 1;
            }
            // Élément normal : A remplacé par AS s'il n'a pas été transmis.
            let a = if o_transmitted { line[o] } else { line[q] };
            let pred = predict_luma(a, d_value(prev, prev_not_trans, t));
            let diff = input[t] as i16 - pred as i16;
            let (level, run, neg) = tables::quantize(diff, true, false);
            tables::write_code(&mut self.w, run, neg);
            line[t] = clamp_y(pred as i16 + level);
            if !o_transmitted {
                // Interpolation des éléments omis, placée dans le store.
                line[o] = ((line[q] as u16 + line[t] as u16) / 2) as u8;
                not_trans[o] = true;
            }
            q = t;
        }
    }

    /// Même chose pour la chroma (prédiction X = A, pas de D).
    fn code_dpcm_sub_chroma(
        &mut self,
        input: &[u8; CHROMA_WIDTH],
        line: &mut [u8; CHROMA_WIDTH],
        s0: usize,
        e1: usize,
        extra_ok: bool,
    ) {
        let mut q = s0;
        while q + 2 <= e1 {
            let o = q + 1;
            let t = q + 2;
            let interp_est = (line[q] as i16 + input[t] as i16) / 2;
            let mut o_transmitted = false;
            if extra_ok && (input[o] as i16 - interp_est).abs() >= EXTRA_THRESHOLD {
                let pred = line[q];
                let diff = input[o] as i16 - pred as i16;
                let (level, run, neg) = tables::quantize(diff, true, true);
                tables::write_code(&mut self.w, run, neg);
                line[o] = clamp_c(pred as i16 + level);
                o_transmitted = true;
                self.stats.extra_elements += 1;
            }
            let pred = if o_transmitted { line[o] } else { line[q] };
            let diff = input[t] as i16 - pred as i16;
            let (level, run, neg) = tables::quantize(diff, true, false);
            tables::write_code(&mut self.w, run, neg);
            line[t] = clamp_c(pred as i16 + level);
            if !o_transmitted {
                line[o] = ((line[q] as u16 + line[t] as u16) / 2) as u8;
            }
            q = t;
        }
    }

    /// Seuil du détecteur de mouvement, durci quand le buffer se remplit.
    fn motion_threshold(&self) -> u8 {
        let r = self.occ_ratio();
        (4.0 + r * 14.0) as u8
    }
}

/// Détection des clusters d'une ligne : segments où |entrée − store| dépasse
/// le seuil, fusionnés lorsque l'écart entre deux segments est inférieur à
/// l'écart minimal autorisé entre clusters (4 éléments, §1.5.3) ou assez
/// petit pour que la fusion coûte moins cher que l'adressage.
/// `max_e` est le dernier élément codable (255 exclu pour la luma, §1.4.1.1) ;
/// `max_start` la dernière adresse de départ autorisée (254 pour la luma,
/// échantillon 50 pour la chroma dont l'adresse 0x37 est interdite, §1.5.4).
fn detect_clusters(
    input: &[u8],
    store: &[u8],
    thr: u8,
    max_e: usize,
    max_start: usize,
) -> Vec<(usize, usize)> {
    const MERGE_GAP: usize = 6;
    let mut clusters: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<(usize, usize)> = None;
    for e in 0..=max_e {
        if input[e].abs_diff(store[e]) > thr {
            cur = match cur {
                None => Some((e.min(max_start), e)),
                Some((s, last)) if e - last <= MERGE_GAP => Some((s, e)),
                Some(done) => {
                    clusters.push(done);
                    Some((e.min(max_start), e))
                }
            };
        }
    }
    if let Some(done) = cur {
        clusters.push(done);
    }
    // Le recul éventuel d'un départ (max_start) peut créer un chevauchement.
    merge_close(&mut clusters);
    clusters
}

/// Fusionne les clusters qui ne respectent plus l'écart minimal de
/// 4 éléments entre fin et début (§1.5.3).
fn merge_close(clusters: &mut Vec<(usize, usize)>) {
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for &(s, e) in clusters.iter() {
        match merged.last_mut() {
            Some((_, pe)) if s <= *pe + MIN_CLUSTER_GAP => {
                *pe = (*pe).max(e);
            }
            _ => merged.push((s, e)),
        }
    }
    *clusters = merged;
}

/// Aligne début et fin de cluster sur la parité de la ligne (transmission en
/// quinconce) : extension d'un élément si nécessaire, raccourcissement au
/// bord de ligne (§1.4.1.4.1), puis re-fusion si l'écart minimal de
/// 4 éléments n'est plus respecté.
fn adjust_parity(clusters: &mut Vec<(usize, usize)>, parity: usize, max_e: usize) {
    for (s, e) in clusters.iter_mut() {
        if *s % 2 != parity {
            *s = if *s > 0 { *s - 1 } else { *s + 1 };
        }
        if *e % 2 != parity {
            *e = if *e + 1 <= max_e { *e + 1 } else { *e - 1 };
        }
        if *e < *s {
            *e = *s;
        }
    }
    // La parité peut réduire l'écart entre clusters sous le minimum : fusion.
    merge_close(clusters);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_simple_cluster() {
        let mut input = [50u8; WIDTH];
        let store = [50u8; WIDTH];
        for e in 100..120 {
            input[e] = 90;
        }
        let c = detect_clusters(&input, &store, 5, WIDTH - 2, WIDTH - 2);
        assert_eq!(c, vec![(100, 119)]);
    }

    #[test]
    fn detect_merges_close_segments() {
        let mut input = [50u8; WIDTH];
        let store = [50u8; WIDTH];
        for e in 10..15 {
            input[e] = 90;
        }
        for e in 18..25 {
            input[e] = 90;
        }
        for e in 60..70 {
            input[e] = 90;
        }
        let c = detect_clusters(&input, &store, 5, WIDTH - 2, WIDTH - 2);
        assert_eq!(c, vec![(10, 24), (60, 69)]);
    }

    #[test]
    fn parity_adjustment_extends() {
        // Ligne paire : éléments pairs transmis.
        let mut c = vec![(11, 21)];
        adjust_parity(&mut c, 0, WIDTH - 2);
        assert_eq!(c, vec![(10, 22)]);
        // Bord de ligne : raccourcissement.
        let mut c = vec![(11, 253)];
        adjust_parity(&mut c, 1, 254);
        assert_eq!(c, vec![(11, 253)]);
        let mut c = vec![(10, 254)];
        adjust_parity(&mut c, 1, 254);
        assert_eq!(c, vec![(9, 253)]);
    }
}
