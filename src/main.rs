//! h120 — reference implementation of the ITU-T H.120 video codec (clause 1).

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use h120::codec::decoder::Decoder;
use h120::codec::encoder::{Encoder, EncoderConfig};
use h120::{codec, ffmpeg, source, y4m};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "h120",
    version,
    about = "Reference encoder/decoder for the ITU-T H.120 video codec (1984, clause 1)",
    long_about = "Reference implementation of the first standardized digital video codec:\n\
                  conditional replenishment, DPCM and variable-length codes,\n\
                  625 lines / 50 fields/s, 2048 kbit/s channel."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Encode a video (native Y4M, or any format via ffmpeg) into an .h120 stream
    Encode {
        /// Input file (.y4m, .mp4, .mkv, …)
        input: PathBuf,
        /// Output .h120 file
        output: PathBuf,
        /// Simulated video bitrate in bit/s (k/M suffixes accepted)
        #[arg(long, default_value = "1600k", value_parser = parse_bitrate)]
        bitrate: u64,
        /// Encode in monochrome (neutral chrominance)
        #[arg(long)]
        mono: bool,
        /// Limit the number of encoded frames
        #[arg(long)]
        frames: Option<u64>,
    },
    /// Decode an .h120 stream into a Y4M file (playable by mpv/VLC/ffmpeg)
    Decode {
        /// .h120 file
        input: PathBuf,
        /// Output .y4m file
        output: PathBuf,
        /// Integer upscaling factor for the output image
        #[arg(long, default_value_t = 1)]
        scale: usize,
    },
    /// Analyze an .h120 stream and print its statistics
    Info {
        /// .h120 file
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
    let v: f64 = num.parse().map_err(|_| format!("invalid bitrate: {s}"))?;
    let bps = (v * mult as f64) as u64;
    if !(100_000..=2_048_000).contains(&bps) {
        return Err("bitrate must be between 100k and 2048k (2 Mbit/s channel)".into());
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
        Command::Info { input } => cmd_info(&input),
    }
}

/// Opens the video input: Y4M is read directly, any other format is converted
/// on the fly by ffmpeg (scaling to 256×286, 25 fps, 4:3 letterbox).
fn open_video_input(input: &Path) -> Result<(y4m::Y4mReader<Box<dyn std::io::BufRead>>, Option<std::process::Child>)> {
    if ffmpeg::is_y4m(input) {
        let file = std::fs::File::open(input)
            .with_context(|| format!("opening {}", input.display()))?;
        let reader: Box<dyn std::io::BufRead> = Box::new(BufReader::new(file));
        Ok((y4m::Y4mReader::new(reader)?, None))
    } else {
        ffmpeg::check_available()?;
        let mut child = ffmpeg::spawn_to_y4m(input)?;
        let stdout = child.stdout.take().expect("ffmpeg stdout");
        let reader: Box<dyn std::io::BufRead> = Box::new(BufReader::new(stdout));
        Ok((y4m::Y4mReader::new(reader).context(
            "ffmpeg did not produce a Y4M stream (unreadable input file?)",
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
            "warning: input frame rate {}/{} ≠ 25 fps — frames will be \
             treated as 25 fps (use ffmpeg to resample)",
            reader.fps_num, reader.fps_den
        );
    }

    let mut enc = Encoder::new(EncoderConfig { bitrate, mono });
    let mut n: u64 = 0;
    while let Some(frame) = reader.next_frame()? {
        enc.encode_frame(&frame);
        n += 1;
        if n % 50 == 0 {
            eprint!("\r{n} frames encoded…");
            std::io::stderr().flush().ok();
        }
        if max_frames.is_some_and(|m| n >= m) {
            break;
        }
    }
    if n == 0 {
        bail!("no frame in the input");
    }
    if let Some(c) = child.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }

    let stats = enc.stats.clone();
    let bits = enc.bits_written();
    let data = enc.finish();
    std::fs::write(output, &data).with_context(|| format!("writing {}", output.display()))?;

    let dur = n as f64 / 25.0;
    eprintln!("\r{n} frames encoded ({dur:.1} s of video)");
    eprintln!("──────────────────────────────────────────");
    eprintln!("stream         : {} bytes ({:.0} kbit/s video)", data.len(), bits as f64 / dur / 1000.0);
    eprintln!("coded fields   : {} (+ {} omitted)", stats.fields_coded, stats.fields_omitted);
    eprintln!("PCM lines      : {}", stats.pcm_lines);
    eprintln!("empty lines    : {}", stats.empty_lines);
    eprintln!("subsampled lines: {}", stats.subsampled_lines);
    eprintln!("clusters       : {} luma, {} chroma", stats.luma_clusters, stats.chroma_clusters);
    eprintln!("extra elements : {}", stats.extra_elements);
    eprintln!(
        "buffer peak    : {:.0} kbit / 96 kbit{}",
        stats.max_occupancy / 1024.0,
        if stats.panic_lines > 0 {
            format!(" ({} lines sacrificed)", stats.panic_lines)
        } else {
            String::new()
        }
    );
    Ok(())
}

fn cmd_decode(input: &Path, output: &Path, scale: usize) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let mut dec = Decoder::new(&data);
    let file = std::fs::File::create(output)
        .with_context(|| format!("creating {}", output.display()))?;
    let mut writer = y4m::Y4mWriter::new(BufWriter::new(file), 25, 1, source::PAR);
    let mut n = 0u64;
    while let Some(fields) = dec.next_frame()? {
        let frame = source::egress(&fields);
        let frame = source::upscale(&frame, scale.max(1));
        writer.write_frame(&frame)?;
        n += 1;
        if n % 50 == 0 {
            eprint!("\r{n} frames decoded…");
            std::io::stderr().flush().ok();
        }
    }
    writer.flush()?;
    if n == 0 {
        bail!("no decodable frame in {}", input.display());
    }
    eprintln!("\r{n} frames decoded → {}", output.display());
    Ok(())
}

fn cmd_info(input: &Path) -> Result<()> {
    let data =
        std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let mut dec = Decoder::new(&data);
    while dec.next_frame()?.is_some() {}
    let s = &dec.stats;
    if s.fields_decoded == 0 {
        bail!("no decodable H.120 field in {}", input.display());
    }
    let dur = s.frames as f64 / 25.0;
    println!("H.120 stream (clause 1): {}", input.display());
    println!("──────────────────────────────────────────");
    println!("size            : {} bytes", data.len());
    println!("frames          : {} ({dur:.1} s at 25 fps)", s.frames);
    println!("decoded fields  : {} (+ {} omitted/interpolated)", s.fields_decoded, s.fields_omitted);
    println!("video bitrate   : {:.0} kbit/s", s.total_bits as f64 / dur.max(0.04) / 1000.0);
    let total_lines = s.fields_decoded * codec::LINES_PER_FIELD as u64;
    println!(
        "lines           : {} PCM, {} moving ({} subsampled), {} empty (of {})",
        s.pcm_lines, s.moving_lines, s.subsampled_lines, s.empty_lines, total_lines
    );
    println!("clusters        : {} luma, {} chroma", s.luma_clusters, s.chroma_clusters);
    println!("extra elements  : {}", s.extra_elements);
    println!("fields with A=1 : {} (transmitter buffer < 6 kbit)", s.a_flag_fields);
    Ok(())
}
