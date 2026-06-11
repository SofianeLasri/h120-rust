//! Conversion automatique des fichiers vidéo vers Y4M via le binaire ffmpeg.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Vrai si le fichier est déjà du Y4M (extension ou signature).
pub fn is_y4m(path: &Path) -> bool {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("y4m")) {
        return true;
    }
    std::fs::read(path)
        .map(|d| d.starts_with(b"YUV4MPEG2"))
        .unwrap_or(false)
}

/// Lance ffmpeg pour convertir `input` en Y4M 4:4:4, 256×286 à 25 i/s,
/// aspect préservé par letterbox sur un canevas 4:3 (les pixels H.120 ne
/// sont pas carrés : 256×286 couvre une image 4:3).
///
/// Renvoie le processus enfant ; son stdout est le flux Y4M à lire.
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
            "impossible de lancer ffmpeg — installez-le ou fournissez un fichier .y4m \
             (voir README)",
        )?;
    Ok(child)
}

/// Vérifie que ffmpeg est disponible.
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
            "ffmpeg introuvable : nécessaire pour lire autre chose que du Y4M. \
             Convertissez manuellement (voir README) ou installez ffmpeg."
        );
    }
    Ok(())
}
