//! Automatic conversion of video files to Y4M via the ffmpeg binary.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// True if the file is already Y4M (by extension or signature).
pub fn is_y4m(path: &Path) -> bool {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("y4m")) {
        return true;
    }
    std::fs::read(path)
        .map(|d| d.starts_with(b"YUV4MPEG2"))
        .unwrap_or(false)
}

/// Spawns ffmpeg to convert `input` into Y4M 4:4:4, 256×286 at 25 fps, aspect
/// preserved by letterboxing onto a 4:3 canvas (H.120 pixels are not square:
/// 256×286 covers a 4:3 image).
///
/// Returns the child process; its stdout is the Y4M stream to read.
pub fn spawn_to_y4m(input: &Path) -> Result<Child> {
    let filter = "fps=25,scale=1024:858:force_original_aspect_ratio=decrease,\
                  pad=1024:858:(ow-iw)/2:(oh-ih)/2:color=black,\
                  scale=256:286:flags=lanczos,format=yuv444p";
    let child = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(input)
        .args(["-vf", filter, "-an", "-f", "yuv4mpegpipe", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .spawn()
        .context(
            "could not launch ffmpeg — install it or provide a .y4m file \
             (see README)",
        )?;
    Ok(child)
}

/// Checks that ffmpeg is available.
pub fn check_available() -> Result<()> {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        bail!(
            "ffmpeg not found: required to read anything other than Y4M. \
             Convert manually (see README) or install ffmpeg."
        );
    }
    Ok(())
}
