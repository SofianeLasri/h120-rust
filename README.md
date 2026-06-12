> 🇬🇧 English version (this document) · 🇫🇷 **Version française : [README.fr.md](README.fr.md)**

# h120 — reference implementation of the ITU-T H.120 video codec

H.120 (CCITT, 1984) is **the first standardized digital video codec** in
history. Designed for videoconferencing over 2048 kbit/s links, it never saw
commercial deployment nor a public implementation: until now it existed only
in the form of its specification
([Rec. ITU-T H.120 (03/93)](https://www.itu.int/rec/T-REC-H.120)).

This project offers a reference implementation in Rust: encoder, decoder,
stream analyzer and graphical player. It covers **clause 1** of the
Recommendation — the historical European codec (COST 211) using
*conditional replenishment*: 625 lines / 50 fields/s, luminance of 256 samples
over 143 active lines per field, DPCM and variable-length codes, 2048 kbit/s
channel.

> See [docs/FORMAT.md](docs/FORMAT.md) for how the codec works and
> [docs/DEVIATIONS.md](docs/DEVIATIONS.md) for the precise list of
> implementation choices and deviations from the spec.

## Building

Prerequisites:

- **Rust** (2024 edition, tested with rustc 1.95);
- **GTK 4 ≥ 4.14** and **libadwaita ≥ 1.5** (development packages) for the
  built-in player — on Debian/Ubuntu: `sudo apt install libgtk-4-dev libadwaita-1-dev`;
- **ffmpeg** (binary on the PATH), optional but recommended: it is used to read
  formats other than Y4M and to work with decoded files.

```bash
cargo build --release
```

Two binaries are produced, deliberately kept separate for portability:

- `target/release/h120` — encoder, decoder and analyzer. **No graphical
  dependency** (only libc is linked): it can be copied as-is onto a server or a
  machine with no desktop environment;
- `target/release/h120-play` — the graphical player, the only one depending on
  GTK4/libadwaita.

To build only the CLI (without even having GTK installed):

```bash
cargo build --release --no-default-features
```

## Usage

### Encode a video

```bash
h120 encode input.mp4 output.h120
```

The input can be any video file readable by ffmpeg (MP4, MKV, WebM…): it is
automatically converted to 256×286 at 25 fps, with letterboxing to preserve the
proportions on the codec's 4:3 screen. A `.y4m` file is read natively, without
ffmpeg.

Options:

| Option | Effect |
|---|---|
| `--bitrate 1600k` | Simulated video bitrate (default 1600k, max 2048k). The lower it is, the more the codec subsamples and stutters — authentically. |
| `--mono` | Encode in monochrome (neutral chrominance). |
| `--frames N` | Encode only the first N frames. |

At the end, the encoder prints its statistics: actual bitrate, PCM refresh
lines, subsampled lines, omitted fields, peak occupancy of the 96 kbit buffer.

### Play a stream in a window

```bash
h120-play output.h120
```

Opens a GTK4/libadwaita window and plays the stream at 25 fps, at the original
4:3 ratio (H.120 pixels are not square). Header bar: pause button (or the space
key), loop playback, frame counter.

You can observe the codec's characteristic behaviours there: the image building
up in ~1 second at startup (progressive PCM refresh), loss of horizontal
definition in moving areas, stutter when the channel saturates.

### Decode to a standard video file

```bash
h120 decode output.h120 output.y4m          # Y4M 4:4:4, 256×288, 25 fps
h120 decode output.h120 output.y4m --scale 2 # upscaled 2× (512×576)
```

The Y4M file plays with mpv or VLC, and converts with ffmpeg — correcting the
aspect (Y4M already carries the right pixel ratio, which mpv and VLC respect;
for a square-pixel MP4, we resample):

```bash
mpv output.y4m                                # direct playback
ffmpeg -i output.y4m -vf "scale=768:576:flags=lanczos" -pix_fmt yuv420p output.mp4
```

### Analyze a stream

```bash
h120 info output.h120
```

Prints size, duration, bitrate, and the breakdown of lines (PCM, moving,
subsampled, empty), clusters, extra elements and omitted fields.

## Full example

```bash
h120 encode movie.mp4 movie.h120 --bitrate 1600k
h120 info movie.h120
h120-play movie.h120
h120 decode movie.h120 movie.y4m --scale 2
ffmpeg -i movie.y4m -pix_fmt yuv420p movie_h120.mp4
```

## What is implemented

- The complete **clause 1** codec: conditional replenishment, DPCM ((A+D)/2
  prediction in luminance, A in chrominance), quantization and variable-length
  codes of Tables 1 and 2, cluster addressing, color escape, PCM refresh lines,
  horizontal quincunx subsampling with "extra" elements, field
  omission/interpolation, LST/FST codes with the A bit, rate control via a
  96 kbit buffer.
- The `.h120` file is the **raw video multiplex of the spec**, bit for bit — no
  proprietary container.
- The encoder and the decoder maintain strictly identical frame memories
  (closed loop): this is verified bit for bit by the integration tests
  (`cargo test`).

## What is not

- Clauses 2 (525-line/1544 kbit/s variant) and 3 (the 1988 motion-compensated
  codec);
- the transmission layer (G.704/H.130 frame, A-law audio, BCH FEC,
  codec-to-codec signalling): the produced stream is the video multiplex alone,
  the channel rate constraint remaining simulated — see
  [docs/DEVIATIONS.md](docs/DEVIATIONS.md);
- the annex options (graphics mode, encryption, multipoint).

## License and status

A project with a historical and educational purpose. Recommendation ITU-T H.120
remains the normative reference; any divergence not documented in
docs/DEVIATIONS.md is a bug — reports are welcome.

## Personal note

This project was made entirely by artificial intelligence, using Anthropic's
Claude Code tool. The Claude Opus 4.8 and Fable 5 models were used, Fable 5
having served for the initial implementation and the first code review.

The reason for this is that my level of skill in Rust is not sufficient to
build such a project in a reasonable amount of time. As I wanted to make a
Proof of Concept rather than a complete and functional implementation, using AI
seemed relevant to me.

Now, regarding whether this project qualifies as AI Slop, I leave you as the
sole judges. The viewpoint is understandable, all the more so since I have no
particular skill when it comes to building video encoders and decoders. I am
merely passionate about the subject and wanted to recreate what I would call
"Lost Media", the original implementation of the codec being nowhere to be
found on the internet.

You are absolutely free to fork this project. It would be wonderful to have
support for this codec in FFmpeg and VLC. Probably not based on the work done
in this repository, for the reasons given above, but I hope this initiative
will open the door to a more serious implementation in the future.

I ask for no credit; all the work done is attributable to the ITU and to
Anthropic.
