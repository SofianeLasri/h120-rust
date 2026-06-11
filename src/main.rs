//! h120 — implémentation de référence du codec vidéo ITU-T H.120 (clause 1).

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use h120::codec::decoder::Decoder;
use h120::codec::encoder::{Encoder, EncoderConfig};
use h120::{codec, ffmpeg, source, y4m};
#[cfg(feature = "player")]
use h120::player;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "h120",
    version,
    about = "Encodeur/décodeur de référence du codec vidéo ITU-T H.120 (1984, clause 1)",
    long_about = "Implémentation de référence du premier codec vidéo numérique standardisé :\n\
                  conditional replenishment, DPCM et codes à longueur variable,\n\
                  625 lignes / 50 champs/s, canal 2048 kbit/s."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Encode une vidéo (Y4M natif, ou tout format via ffmpeg) en flux .h120
    Encode {
        /// Fichier d'entrée (.y4m, .mp4, .mkv, …)
        input: PathBuf,
        /// Fichier de sortie .h120
        output: PathBuf,
        /// Débit vidéo simulé en bit/s (suffixes k/M acceptés)
        #[arg(long, default_value = "1600k", value_parser = parse_bitrate)]
        bitrate: u64,
        /// Encode en monochrome (chrominance neutre)
        #[arg(long)]
        mono: bool,
        /// Limite le nombre d'images encodées
        #[arg(long)]
        frames: Option<u64>,
    },
    /// Décode un flux .h120 vers un fichier Y4M (lisible par mpv/VLC/ffmpeg)
    Decode {
        /// Fichier .h120
        input: PathBuf,
        /// Fichier de sortie .y4m
        output: PathBuf,
        /// Facteur d'agrandissement entier de l'image de sortie
        #[arg(long, default_value_t = 1)]
        scale: usize,
    },
    /// Lit un flux .h120 dans une fenêtre (GTK4 + libadwaita)
    #[cfg(feature = "player")]
    Play {
        /// Fichier .h120
        input: PathBuf,
    },
    /// Analyse un flux .h120 et affiche ses statistiques
    Info {
        /// Fichier .h120
        input: PathBuf,
    },
}

fn parse_bitrate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], 1_000u64),
        Some('m' | 'M') => (&s[..s.len() - 1], 1_000_000u64),
        _ => (s, 1),
    };
    let v: f64 = num.parse().map_err(|_| format!("débit invalide : {s}"))?;
    let bps = (v * mult as f64) as u64;
    if !(100_000..=2_048_000).contains(&bps) {
        return Err("le débit doit être entre 100k et 2048k (canal 2 Mbit/s)".into());
    }
    Ok(bps)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Encode { input, output, bitrate, mono, frames } => {
            cmd_encode(&input, &output, bitrate, mono, frames)
        }
        Command::Decode { input, output, scale } => cmd_decode(&input, &output, scale),
        #[cfg(feature = "player")]
        Command::Play { input } => player::run(&input),
        Command::Info { input } => cmd_info(&input),
    }
}

/// Ouvre l'entrée vidéo : Y4M lu directement, tout autre format converti à
/// la volée par ffmpeg (mise à l'échelle 256×286, 25 i/s, letterbox 4:3).
fn open_video_input(input: &Path) -> Result<(y4m::Y4mReader<Box<dyn std::io::BufRead>>, Option<std::process::Child>)> {
    if ffmpeg::is_y4m(input) {
        let file = std::fs::File::open(input)
            .with_context(|| format!("ouverture de {}", input.display()))?;
        let reader: Box<dyn std::io::BufRead> = Box::new(BufReader::new(file));
        Ok((y4m::Y4mReader::new(reader)?, None))
    } else {
        ffmpeg::check_available()?;
        let mut child = ffmpeg::spawn_to_y4m(input)?;
        let stdout = child.stdout.take().expect("stdout de ffmpeg");
        let reader: Box<dyn std::io::BufRead> = Box::new(BufReader::new(stdout));
        Ok((y4m::Y4mReader::new(reader).context(
            "ffmpeg n'a pas produit de flux Y4M (fichier d'entrée illisible ?)",
        )?, Some(child)))
    }
}

fn cmd_encode(
    input: &Path,
    output: &Path,
    bitrate: u64,
    mono: bool,
    max_frames: Option<u64>,
) -> Result<()> {
    let (mut reader, mut child) = open_video_input(input)?;
    if (reader.fps_num, reader.fps_den) != (25, 1) {
        eprintln!(
            "attention : cadence d'entrée {}/{} ≠ 25 i/s — les images seront \
             traitées comme du 25 i/s (utilisez ffmpeg pour rééchantillonner)",
            reader.fps_num, reader.fps_den
        );
    }

    let mut enc = Encoder::new(EncoderConfig { bitrate, mono });
    let mut n: u64 = 0;
    while let Some(frame) = reader.next_frame()? {
        enc.encode_frame(&frame);
        n += 1;
        if n % 50 == 0 {
            eprint!("\r{n} images encodées…");
            std::io::stderr().flush().ok();
        }
        if max_frames.is_some_and(|m| n >= m) {
            break;
        }
    }
    if n == 0 {
        bail!("aucune image dans l'entrée");
    }
    if let Some(c) = child.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }

    let stats = enc.stats.clone();
    let bits = enc.bits_written();
    let data = enc.finish();
    std::fs::write(output, &data).with_context(|| format!("écriture de {}", output.display()))?;

    let dur = n as f64 / 25.0;
    eprintln!("\r{n} images encodées ({dur:.1} s de vidéo)");
    eprintln!("──────────────────────────────────────────");
    eprintln!("flux           : {} octets ({:.0} kbit/s vidéo)", data.len(), bits as f64 / dur / 1000.0);
    eprintln!("champs codés   : {} (+ {} omis)", stats.fields_coded, stats.fields_omitted);
    eprintln!("lignes PCM     : {}", stats.pcm_lines);
    eprintln!("lignes vides   : {}", stats.empty_lines);
    eprintln!("lignes sous-éch.: {}", stats.subsampled_lines);
    eprintln!("clusters       : {} luma, {} chroma", stats.luma_clusters, stats.chroma_clusters);
    eprintln!("éléments extra : {}", stats.extra_elements);
    eprintln!(
        "buffer max     : {:.0} kbit / 96 kbit{}",
        stats.max_occupancy / 1024.0,
        if stats.panic_lines > 0 {
            format!(" ({} lignes sacrifiées)", stats.panic_lines)
        } else {
            String::new()
        }
    );
    Ok(())
}

fn cmd_decode(input: &Path, output: &Path, scale: usize) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("lecture de {}", input.display()))?;
    let mut dec = Decoder::new(&data);
    let file = std::fs::File::create(output)
        .with_context(|| format!("création de {}", output.display()))?;
    let mut writer = y4m::Y4mWriter::new(BufWriter::new(file), 25, 1, source::PAR);
    let mut n = 0u64;
    while let Some(fields) = dec.next_frame()? {
        let frame = source::egress(&fields);
        let frame = source::upscale(&frame, scale.max(1));
        writer.write_frame(&frame)?;
        n += 1;
        if n % 50 == 0 {
            eprint!("\r{n} images décodées…");
            std::io::stderr().flush().ok();
        }
    }
    writer.flush()?;
    if n == 0 {
        bail!("aucune image décodable dans {}", input.display());
    }
    eprintln!("\r{n} images décodées → {}", output.display());
    Ok(())
}

fn cmd_info(input: &Path) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("lecture de {}", input.display()))?;
    let mut dec = Decoder::new(&data);
    while dec.next_frame()?.is_some() {}
    let s = &dec.stats;
    if s.fields_decoded == 0 {
        bail!("aucun champ H.120 décodable dans {}", input.display());
    }
    let dur = s.frames as f64 / 25.0;
    println!("Flux H.120 (clause 1) : {}", input.display());
    println!("──────────────────────────────────────────");
    println!("taille          : {} octets", data.len());
    println!("images          : {} ({dur:.1} s à 25 i/s)", s.frames);
    println!("champs décodés  : {} (+ {} omis/interpolés)", s.fields_decoded, s.fields_omitted);
    println!("débit vidéo     : {:.0} kbit/s", s.total_bits as f64 / dur.max(0.04) / 1000.0);
    let total_lines = s.fields_decoded * codec::LINES_PER_FIELD as u64;
    println!(
        "lignes          : {} PCM, {} mobiles ({} sous-éch.), {} vides (sur {})",
        s.pcm_lines, s.moving_lines, s.subsampled_lines, s.empty_lines, total_lines
    );
    println!("clusters        : {} luma, {} chroma", s.luma_clusters, s.chroma_clusters);
    println!("éléments extra  : {}", s.extra_elements);
    println!("champs avec A=1 : {} (buffer émetteur < 6 kbit)", s.a_flag_fields);
    Ok(())
}
