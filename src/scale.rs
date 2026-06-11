//! Redimensionnement bilinéaire de plans 8 bits.
//!
//! Sert de pré-filtre spatial : la spec laisse les pré/post-filtres au choix
//! de l'implémentation (§1.4.1.2 — seule la géométrie 256×143/champ est
//! imposée).

/// Redimensionne `src` (sw × sh) vers (dw × dh) en bilinéaire.
pub fn resize_plane(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    assert_eq!(src.len(), sw * sh);
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let mut dst = vec![0u8; dw * dh];
    // Coordonnées en virgule fixe 16.16, échantillonnage au centre du pixel.
    const F: u64 = 1 << 16;
    let xr = (sw as u64 * F) / dw as u64;
    let yr = (sh as u64 * F) / dh as u64;
    for dy in 0..dh {
        let fy = (dy as u64 * yr + yr / 2).saturating_sub(F / 2);
        let sy = (fy / F) as usize;
        let wy = fy % F;
        let sy1 = (sy + 1).min(sh - 1);
        for dx in 0..dw {
            let fx = (dx as u64 * xr + xr / 2).saturating_sub(F / 2);
            let sx = (fx / F) as usize;
            let wx = fx % F;
            let sx1 = (sx + 1).min(sw - 1);
            let p00 = src[sy * sw + sx] as u64;
            let p01 = src[sy * sw + sx1] as u64;
            let p10 = src[sy1 * sw + sx] as u64;
            let p11 = src[sy1 * sw + sx1] as u64;
            let top = p00 * (F - wx) + p01 * wx;
            let bot = p10 * (F - wx) + p11 * wx;
            let v = (top * (F - wy) + bot * wy + F * F / 2) >> 32;
            dst[dy * dw + dx] = v.min(255) as u8;
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        let src: Vec<u8> = (0..64).collect();
        assert_eq!(resize_plane(&src, 8, 8, 8, 8), src);
    }

    #[test]
    fn constant_plane_stays_constant() {
        let src = vec![100u8; 32 * 20];
        let dst = resize_plane(&src, 32, 20, 256, 286);
        assert!(dst.iter().all(|&v| (99..=101).contains(&v)));
        let dst2 = resize_plane(&src, 32, 20, 13, 7);
        assert!(dst2.iter().all(|&v| (99..=101).contains(&v)));
    }

    #[test]
    fn gradient_monotone() {
        let mut src = vec![0u8; 256];
        for x in 0..256 {
            src[x] = x as u8;
        }
        let dst = resize_plane(&src, 256, 1, 52, 1);
        for w in dst.windows(2) {
            assert!(w[1] >= w[0]);
        }
        assert!(dst[0] < 10 && dst[51] > 240);
    }
}
