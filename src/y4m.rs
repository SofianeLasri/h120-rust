//! Reading and writing the YUV4MPEG2 (Y4M) format, enough to pipe to/from
//! ffmpeg without an external dependency.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};

/// A YCbCr 4:4:4 frame, full planes of `w × h` bytes.
#[derive(Clone)]
pub struct Frame444 {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
}

impl Frame444 {
    pub fn new(w: usize, h: usize) -> Self {
        Frame444 { w, h, y: vec![16; w * h], cb: vec![128; w * h], cr: vec![128; w * h] }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Subsampling {
    C420,
    C422,
    C444,
    Mono,
}

pub struct Y4mReader<R: BufRead> {
    inner: R,
    pub width: usize,
    pub height: usize,
    pub fps_num: u32,
    pub fps_den: u32,
    sub: Subsampling,
    frame_buf: Vec<u8>,
}

impl<R: BufRead> Y4mReader<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let mut header = Vec::new();
        inner.read_until(b'\n', &mut header).context("reading the Y4M header")?;
        let header = String::from_utf8_lossy(&header);
        if !header.starts_with("YUV4MPEG2") {
            bail!("the file is not in YUV4MPEG2 (Y4M) format");
        }
        let (mut w, mut h, mut fn_, mut fd) = (0usize, 0usize, 25u32, 1u32);
        let mut sub = Subsampling::C420;
        for tok in header.split_whitespace().skip(1) {
            let (tag, val) = tok.split_at(1);
            match tag {
                "W" => w = val.parse().context("Y4M width")?,
                "H" => h = val.parse().context("Y4M height")?,
                "F" => {
                    let (n, d) = val.split_once(':').context("Y4M frame rate")?;
                    fn_ = n.parse()?;
                    fd = d.parse()?;
                }
                "C" => {
                    sub = if val.starts_with("420") {
                        Subsampling::C420
                    } else if val.starts_with("422") {
                        Subsampling::C422
                    } else if val.starts_with("444") {
                        Subsampling::C444
                    } else if val.starts_with("mono") {
                        Subsampling::Mono
                    } else {
                        bail!("unsupported Y4M subsampling: C{val}")
                    };
                }
                _ => {}
            }
        }
        if w == 0 || h == 0 {
            bail!("missing Y4M dimensions");
        }
        Ok(Y4mReader { inner, width: w, height: h, fps_num: fn_, fps_den: fd, sub, frame_buf: Vec::new() })
    }

    /// Reads the next frame, converted to 4:4:4. `None` at end of stream.
    pub fn next_frame(&mut self) -> Result<Option<Frame444>> {
        let mut line = Vec::new();
        let n = self.inner.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if !line.starts_with(b"FRAME") {
            bail!("FRAME marker expected in the Y4M stream");
        }
        let (w, h) = (self.width, self.height);
        let (cw, ch) = match self.sub {
            Subsampling::C420 => (w.div_ceil(2), h.div_ceil(2)),
            Subsampling::C422 => (w.div_ceil(2), h),
            Subsampling::C444 => (w, h),
            Subsampling::Mono => (0, 0),
        };
        let total = w * h + 2 * cw * ch;
        self.frame_buf.resize(total, 0);
        std::io::Read::read_exact(&mut self.inner, &mut self.frame_buf)
            .context("reading the Y4M planes")?;
        let mut f = Frame444::new(w, h);
        f.y.copy_from_slice(&self.frame_buf[..w * h]);
        if self.sub == Subsampling::Mono {
            return Ok(Some(f));
        }
        let cb = &self.frame_buf[w * h..w * h + cw * ch];
        let cr = &self.frame_buf[w * h + cw * ch..];
        // Upsampling by replication (good enough: the chroma is reduced to
        // 52 samples/line afterwards anyway).
        for yy in 0..h {
            let sy = match self.sub {
                Subsampling::C420 => yy / 2,
                _ => yy,
            };
            for xx in 0..w {
                let sx = match self.sub {
                    Subsampling::C444 => xx,
                    _ => xx / 2,
                };
                f.cb[yy * w + xx] = cb[sy * cw + sx];
                f.cr[yy * w + xx] = cr[sy * cw + sx];
            }
        }
        Ok(Some(f))
    }
}

pub struct Y4mWriter<W: Write> {
    inner: W,
    wrote_header: bool,
    pub fps_num: u32,
    pub fps_den: u32,
    /// Pixel aspect ratio (Y4M `A` parameter).
    pub par: (u32, u32),
}

impl<W: Write> Y4mWriter<W> {
    pub fn new(inner: W, fps_num: u32, fps_den: u32, par: (u32, u32)) -> Self {
        Y4mWriter { inner, wrote_header: false, fps_num, fps_den, par }
    }

    /// Writes a 4:4:4 frame.
    pub fn write_frame(&mut self, f: &Frame444) -> Result<()> {
        if !self.wrote_header {
            writeln!(
                self.inner,
                "YUV4MPEG2 W{} H{} F{}:{} Ip A{}:{} C444",
                f.w, f.h, self.fps_num, self.fps_den, self.par.0, self.par.1
            )?;
            self.wrote_header = true;
        }
        writeln!(self.inner, "FRAME")?;
        self.inner.write_all(&f.y)?;
        self.inner.write_all(&f.cb)?;
        self.inner.write_all(&f.cr)?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.inner.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_444() {
        let mut f = Frame444::new(4, 2);
        for (i, v) in f.y.iter_mut().enumerate() {
            *v = i as u8 + 20;
        }
        let mut out = Vec::new();
        {
            let mut w = Y4mWriter::new(&mut out, 25, 1, (1, 1));
            w.write_frame(&f).unwrap();
            w.write_frame(&f).unwrap();
        }
        let mut r = Y4mReader::new(std::io::BufReader::new(&out[..])).unwrap();
        assert_eq!((r.width, r.height), (4, 2));
        assert_eq!((r.fps_num, r.fps_den), (25, 1));
        let g = r.next_frame().unwrap().unwrap();
        assert_eq!(g.y, f.y);
        assert_eq!(g.cb, f.cb);
        assert!(r.next_frame().unwrap().is_some());
        assert!(r.next_frame().unwrap().is_none());
    }
}
