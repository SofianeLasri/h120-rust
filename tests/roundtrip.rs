//! Tests d'intégration : l'encodeur et le décodeur doivent rester en
//! parfait synchronisme (boucle DPCM fermée), et le flux doit se décoder
//! de bout en bout.

use h120::codec::decoder::Decoder;
use h120::codec::encoder::{Encoder, EncoderConfig};
use h120::codec::{FieldStore, LINES_PER_FIELD, WIDTH};
use h120::source::ingest;
use h120::y4m::Frame444;

/// Image de test : fond dégradé + un carré qui se déplace avec l'index.
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

/// Scène statique : après l'amorçage PCM, le store décodé doit être
/// EXACTEMENT l'entrée (les lignes PCM copient les échantillons).
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
    while let Some(fields) = dec.next_frame().expect("décodage") {
        last = Some(fields);
    }
    let fields = last.expect("au moins une image");
    let reference = ingest(&frame, false);
    for f in 0..2 {
        for l in 0..LINES_PER_FIELD {
            assert_eq!(fields[f].y[l], reference[f].y[l], "luma champ {f} ligne {l}");
            assert_eq!(fields[f].c[l], reference[f].c[l], "chroma champ {f} ligne {l}");
        }
    }
    assert_eq!(dec.stats.frames, 40);
}

/// Mouvement modéré à haut débit : le store de l'encodeur et celui du
/// décodeur doivent être identiques bit à bit après chaque image
/// (boucle fermée). C'est LE test de conformité interne du codec.
#[test]
fn encoder_decoder_lockstep() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 2_000_000, mono: false });
    let n = 30;
    let mut snapshots: Vec<[FieldStore; 2]> = Vec::new();
    for t in 0..n {
        enc.encode_frame(&test_frame(t, true));
        assert!(
            !enc.has_pending_interpolation(),
            "pas d'omission de champ attendue à ce débit (image {t})"
        );
        snapshots.push(enc.stores().clone());
    }
    let data = enc.finish();

    let mut dec = Decoder::new(&data);
    for (t, snap) in snapshots.iter().enumerate() {
        let fields = dec
            .next_frame()
            .expect("décodage")
            .unwrap_or_else(|| panic!("image {t} manquante"));
        for f in 0..2 {
            assert!(
                stores_equal(&fields[f], &snap[f]),
                "désynchronisation encodeur/décodeur : image {t}, champ {f}"
            );
        }
    }
}

/// Le sous-échantillonnage horizontal (Table 2, éléments extra, quinconce)
/// doit lui aussi préserver le synchronisme bit à bit des stores.
#[test]
fn lockstep_with_horizontal_subsampling() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_400_000, mono: false });
    let n = 40;
    // Snapshot par image, ignoré quand une interpolation de champ omis est
    // en attente : le store encodeur est alors en avance d'un champ sur ce
    // que le décodeur émettra (l'alignement revient à l'image suivante).
    let mut snapshots: Vec<Option<[FieldStore; 2]>> = Vec::new();
    let mut subsampled_seen = false;
    for t in 0..n {
        // Deux carrés pour maintenir le buffer dans la zone de subsampling.
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
    assert!(subsampled_seen, "le test doit exercer le sous-échantillonnage");
    let compared = snapshots.iter().filter(|s| s.is_some()).count();
    assert!(compared >= 10, "trop peu d'images comparables ({compared})");
    let data = enc.finish();
    let mut dec = Decoder::new(&data);
    for (t, snap) in snapshots.iter().enumerate() {
        let Some(fields) = dec.next_frame().expect("décodage") else {
            // La toute dernière image peut rester en attente d'émission si
            // son champ 2 a été omis et que le flux s'arrête là.
            assert!(t >= n - 1, "image {t} manquante");
            break;
        };
        if let Some(snap) = snap {
            for f in 0..2 {
                assert!(
                    stores_equal(&fields[f], &snap[f]),
                    "désynchronisation en mode sous-échantillonné : image {t}, champ {f}"
                );
            }
        }
    }
    assert!(dec.stats.subsampled_lines > 0);
    assert!(dec.stats.extra_elements > 0, "les éléments extra doivent être exercés");
}

/// Fort mouvement à débit réduit : le contrôle de débit doit déclencher le
/// sous-échantillonnage (Table 2) puis l'omission de champs, et le flux
/// doit rester décodable de bout en bout avec une qualité plancher.
#[test]
fn heavy_motion_survives_rate_control() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_000_000, mono: false });
    let n = 50;
    let mut last_input = None;
    for t in 0..n {
        // Deux carrés en mouvement soutenu pour saturer le buffer.
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
    assert!(stats.subsampled_lines > 0, "le sous-échantillonnage doit s'enclencher");
    assert!(
        stats.max_occupancy <= 96.0 * 1024.0,
        "le buffer de 96 kbit ne doit jamais déborder (max {:.0})",
        stats.max_occupancy
    );

    let mut dec = Decoder::new(&data);
    let mut frames = 0u64;
    let mut last = None;
    while let Some(fields) = dec.next_frame().expect("décodage sous charge") {
        frames += 1;
        last = Some(fields);
    }
    // La dernière image peut rester en attente si son champ 2 a été omis.
    assert!(frames >= n as u64 - 1, "{frames} images décodées sur {n}");
    assert_eq!(dec.stats.subsampled_lines as u64 > 0, true);

    // Qualité plancher sur la luminance du dernier état décodé.
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
    assert!(p > 22.0, "PSNR luma trop bas sous charge : {p:.1} dB");
}

/// Régression : du mouvement sur la toute dernière ligne codée du flux
/// (image rangée 285 → champ 2, ligne 142) ne doit pas être avalé par le
/// bourrage de fin de flux. Si la dernière ligne se termine sur des codes VLC
/// laissant moins de 12 bits avant le bourrage à zéro, le décodeur les
/// prenait pour un mot de synchro et abandonnait la ligne, désynchronisant la
/// dernière image. On balaie plusieurs longueurs : sans le correctif, au moins
/// l'une d'elles tombe sur cet alignement.
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
        // Damier mobile sur les trois dernières rangées (dont la 285).
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
        // Ce contenu reste à un débit modéré : aucune omission de champ.
        assert!(!enc.has_pending_interpolation(), "n={n}");
        let snap = enc.stores().clone();
        let data = enc.finish();

        let mut dec = Decoder::new(&data);
        let mut last = None;
        while let Some(fields) = dec.next_frame().expect("décodage") {
            last = Some(fields);
        }
        let fields = last.unwrap_or_else(|| panic!("aucune image décodée (n={n})"));
        for f in 0..2 {
            assert!(
                stores_equal(&fields[f], &snap[f]),
                "désync sur la dernière image (n={n}, champ {f})"
            );
        }
    }
}

/// Le flux doit se décoder à l'identique même tronqué proprement à une
/// frontière d'image (robustesse du parseur en fin de flux).
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
        while let Some(_) = dec.next_frame().expect("le flux tronqué ne doit pas être une erreur") {
            frames += 1;
        }
        assert!(frames < 10);
    }
}

/// Un flux monochrome reste un flux couleur (chrominance neutre) : aucun
/// cluster chroma ne doit être émis.
#[test]
fn mono_emits_no_chroma_clusters() {
    let mut enc = Encoder::new(EncoderConfig { bitrate: 1_600_000, mono: true });
    for t in 0..10 {
        enc.encode_frame(&test_frame(t, true));
    }
    assert_eq!(enc.stats.chroma_clusters, 0);
    let data = enc.finish();
    let mut dec = Decoder::new(&data);
    while dec.next_frame().expect("décodage mono").is_some() {}
    assert_eq!(dec.stats.chroma_clusters, 0);
    assert!(dec.stats.frames > 0);
}
