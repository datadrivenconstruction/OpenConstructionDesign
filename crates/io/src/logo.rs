//! Company logo: checks, downscale and PNG round trip of the picture a user adds. Pixels are
//! 8-bit RGBA with straight alpha; transparency is kept. The file picker (PNG, JPEG) turns
//! its decoded pixels into a `Logo` here, so every format ends in the same stored form.

use std::sync::Arc;

use crate::pdf::PdfImage;

/// Long side of the stored picture, px: sharp at 600 dpi in a 45 mm title block cell, small
/// enough to stay in the project file and in every PDF.
pub const STORE_SIDE: u32 = 640;
/// Largest side accepted from a file, px.
pub const MAX_SOURCE_SIDE: u32 = 8192;
/// Largest file read, bytes.
pub const MAX_SOURCE_BYTES: usize = 20 << 20;
/// Widest and tallest proportions that still make a readable logo (long side : short side).
pub const MAX_ASPECT: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogoError {
    /// File bigger than `MAX_SOURCE_BYTES` or a side beyond `MAX_SOURCE_SIDE`.
    TooLarge,
    /// Not PNG or JPEG.
    Unsupported,
    /// The format is right, the content cannot be read.
    Corrupt,
    /// Every pixel is transparent or the picture has no area.
    Empty,
    /// Proportions beyond `MAX_ASPECT`.
    Aspect,
}

impl LogoError {
    /// Catalog key of the message shown to the user (both languages).
    pub fn key(self) -> &'static str {
        match self {
            LogoError::TooLarge => "branding.err.too_large",
            LogoError::Unsupported => "branding.err.unsupported",
            LogoError::Corrupt => "branding.err.corrupt",
            LogoError::Empty => "branding.err.empty",
            LogoError::Aspect => "branding.err.aspect",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Logo {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Logo {
    /// Checks the pixels and fits them into `STORE_SIDE` (area average on premultiplied colour,
    /// so a soft edge next to transparent pixels does not turn dark or light).
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Logo, LogoError> {
        if width == 0
            || height == 0
            || rgba.len() as u64 != u64::from(width) * u64::from(height) * 4
        {
            return Err(LogoError::Corrupt);
        }
        if width > MAX_SOURCE_SIDE || height > MAX_SOURCE_SIDE {
            return Err(LogoError::TooLarge);
        }
        if rgba.chunks_exact(4).all(|p| p[3] == 0) {
            return Err(LogoError::Empty);
        }
        let (long, short) = (width.max(height), width.min(height));
        if long > short * MAX_ASPECT {
            return Err(LogoError::Aspect);
        }
        if long <= STORE_SIDE {
            return Ok(Logo { width, height, rgba });
        }
        let f = f64::from(STORE_SIDE) / f64::from(long);
        let dw = ((f64::from(width) * f).round() as u32).max(1);
        let dh = ((f64::from(height) * f).round() as u32).max(1);
        Ok(Logo {
            width: dw,
            height: dh,
            rgba: area_average(width, height, &rgba, dw, dh),
        })
    }

    /// Decodes a PNG of any colour type and depth.
    pub fn from_png(bytes: &[u8]) -> Result<Logo, LogoError> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(LogoError::TooLarge);
        }
        if !bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            return Err(LogoError::Unsupported);
        }
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(|_| LogoError::Corrupt)?;
        let (w, h) = {
            let i = reader.info();
            (i.width, i.height)
        };
        if w > MAX_SOURCE_SIDE || h > MAX_SOURCE_SIDE {
            return Err(LogoError::TooLarge);
        }
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let fr = reader.next_frame(&mut buf).map_err(|_| LogoError::Corrupt)?;
        let d = &buf[..fr.buffer_size()];
        let rgba: Vec<u8> = match fr.color_type {
            png::ColorType::Rgba => d.to_vec(),
            png::ColorType::Rgb => d
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::Grayscale => d.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::GrayscaleAlpha => d
                .chunks_exact(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Indexed => return Err(LogoError::Corrupt),
        };
        Logo::from_rgba(w, h, rgba)
    }

    /// PNG bytes, 8-bit RGBA.
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("PNG header into memory");
            w.write_image_data(&self.rgba).expect("PNG data into memory");
        }
        out
    }

    pub fn to_pdf_image(&self) -> Arc<PdfImage> {
        Arc::new(PdfImage {
            width: self.width,
            height: self.height,
            rgba: self.rgba.clone(),
        })
    }

    /// The picture shrunk to fit `max_w`×`max_h` px (same proportions); as is when it fits.
    pub fn fit_within(&self, max_w: u32, max_h: u32) -> Logo {
        if (self.width <= max_w && self.height <= max_h) || max_w == 0 || max_h == 0 {
            return self.clone();
        }
        let k = (f64::from(max_w) / f64::from(self.width))
            .min(f64::from(max_h) / f64::from(self.height));
        let dw = ((f64::from(self.width) * k).round() as u32).max(1);
        let dh = ((f64::from(self.height) * k).round() as u32).max(1);
        Logo {
            width: dw,
            height: dh,
            rgba: area_average(self.width, self.height, &self.rgba, dw, dh),
        }
    }

    /// Largest rectangle of the picture's proportions inside `w`×`h`, centred in it:
    /// (x offset, y offset, width, height).
    pub fn fit(&self, w: f64, h: f64) -> [f64; 4] {
        let k = (w / f64::from(self.width)).min(h / f64::from(self.height));
        let (fw, fh) = (f64::from(self.width) * k, f64::from(self.height) * k);
        [(w - fw) / 2.0, (h - fh) / 2.0, fw, fh]
    }
}

fn area_average(sw: u32, sh: u32, src: &[u8], dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(dw as usize * dh as usize * 4);
    let (kx, ky) = (f64::from(sw) / f64::from(dw), f64::from(sh) / f64::from(dh));
    for y in 0..dh {
        let (y0, y1) = (f64::from(y) * ky, f64::from(y + 1) * ky);
        for x in 0..dw {
            let (x0, x1) = (f64::from(x) * kx, f64::from(x + 1) * kx);
            let (mut r, mut g, mut b, mut a, mut wsum) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for sy in y0.floor() as u32..(y1.ceil() as u32).min(sh) {
                let wy = (f64::from(sy + 1).min(y1) - f64::from(sy).max(y0)).max(0.0);
                for sx in x0.floor() as u32..(x1.ceil() as u32).min(sw) {
                    let wx = (f64::from(sx + 1).min(x1) - f64::from(sx).max(x0)).max(0.0);
                    let wgt = wx * wy;
                    let i = (sy as usize * sw as usize + sx as usize) * 4;
                    let al = f64::from(src[i + 3]) / 255.0 * wgt;
                    r += f64::from(src[i]) * al;
                    g += f64::from(src[i + 1]) * al;
                    b += f64::from(src[i + 2]) * al;
                    a += al;
                    wsum += wgt;
                }
            }
            if a <= 0.0 || wsum <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let q = |v: f64| (v / a).round().clamp(0.0, 255.0) as u8;
                out.extend_from_slice(&[q(r), q(g), q(b), (a / wsum * 255.0).round() as u8]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, p: [u8; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|_| p).collect()
    }

    #[test]
    fn small_picture_is_kept_as_is_and_png_round_trips_with_alpha() {
        let mut px = solid(4, 2, [10, 20, 30, 255]);
        px[3] = 0;
        px[7] = 77;
        let l = Logo::from_rgba(4, 2, px.clone()).unwrap();
        assert_eq!((l.width, l.height, &l.rgba), (4, 2, &px));
        let back = Logo::from_png(&l.to_png()).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn high_dpi_picture_is_downscaled_and_keeps_proportions_and_colour() {
        let l = Logo::from_rgba(2560, 1280, solid(2560, 1280, [200, 40, 40, 255])).unwrap();
        assert_eq!((l.width, l.height), (STORE_SIDE, STORE_SIDE / 2));
        assert!(l.rgba.chunks_exact(4).all(|p| p == [200, 40, 40, 255]));
    }

    #[test]
    fn downscale_does_not_bleed_colour_from_transparent_pixels() {
        // left half opaque red, right half transparent black
        let (w, h) = (1280u32, 1280u32);
        let px: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                if (i % w) < w / 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 0, 0]
                }
            })
            .collect();
        let l = Logo::from_rgba(w, h, px).unwrap();
        let edge = &l.rgba[(l.width as usize / 2 - 1) * 4..][..4];
        assert_eq!(&edge[..3], &[255, 0, 0]);
        let right = &l.rgba[(l.width as usize - 1) * 4..][..4];
        assert_eq!(right[3], 0);
    }

    #[test]
    fn invalid_pictures_get_a_named_error() {
        assert_eq!(Logo::from_rgba(0, 4, vec![]).unwrap_err(), LogoError::Corrupt);
        assert_eq!(Logo::from_rgba(2, 2, vec![0; 3]).unwrap_err(), LogoError::Corrupt);
        assert_eq!(
            Logo::from_rgba(2, 2, solid(2, 2, [1, 2, 3, 0])).unwrap_err(),
            LogoError::Empty
        );
        assert_eq!(
            Logo::from_rgba(200, 10, solid(200, 10, [1, 2, 3, 255])).unwrap_err(),
            LogoError::Aspect
        );
        assert_eq!(Logo::from_png(b"GIF89a").unwrap_err(), LogoError::Unsupported);
        assert_eq!(
            Logo::from_png(&[0x89, b'P', b'N', b'G', 1, 2, 3]).unwrap_err(),
            LogoError::Corrupt
        );
        let huge = vec![0u8; MAX_SOURCE_BYTES + 1];
        assert_eq!(Logo::from_png(&huge).unwrap_err(), LogoError::TooLarge);
        let too_wide = Logo::from_rgba(MAX_SOURCE_SIDE + 1, 1, vec![]);
        assert_eq!(too_wide.unwrap_err(), LogoError::Corrupt);
    }

    #[test]
    fn every_error_has_its_own_key() {
        let all = [
            LogoError::TooLarge,
            LogoError::Unsupported,
            LogoError::Corrupt,
            LogoError::Empty,
            LogoError::Aspect,
        ];
        let keys: std::collections::BTreeSet<_> = all.iter().map(|e| e.key()).collect();
        assert_eq!(keys.len(), all.len());
    }

    #[test]
    fn fit_centres_inside_the_box() {
        let l = Logo::from_rgba(40, 20, solid(40, 20, [0, 0, 0, 255])).unwrap();
        assert_eq!(l.fit(20.0, 20.0), [0.0, 5.0, 20.0, 10.0]);
        assert_eq!(l.fit(100.0, 10.0), [40.0, 0.0, 20.0, 10.0]);
    }
}
