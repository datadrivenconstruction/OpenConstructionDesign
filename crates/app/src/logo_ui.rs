//! Company logo, app side: reads a PNG or JPEG file into the stored form, keeps the
//! optional default logo of new projects in the user's data folder, and stamps the logo on the
//! recent-project thumbnail.

use std::path::{Path, PathBuf};

use ddc_io::logo::{Logo, LogoError, MAX_SOURCE_BYTES};

/// File name of the default logo inside the user's data folder.
pub const DEFAULT_FILE: &str = "default_logo.png";
/// Decodes the bytes of a picture file: PNG or JPEG (sniffed from the content, the file
/// name does not matter).
pub fn decode(bytes: &[u8]) -> Result<Logo, LogoError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(LogoError::TooLarge);
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Logo::from_png(bytes);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let (w, h, rgba) = crate::mat_tex::decode_jpeg(bytes).ok_or(LogoError::Corrupt)?;
        return Logo::from_rgba(w, h, rgba);
    }
    Err(LogoError::Unsupported)
}

/// Reads and decodes a picture file; a file over the size limit is refused before it is read.
pub fn read_file(path: &Path) -> Result<Logo, LogoError> {
    let len = std::fs::metadata(path).map_err(|_| LogoError::Corrupt)?.len();
    if len > MAX_SOURCE_BYTES as u64 {
        return Err(LogoError::TooLarge);
    }
    decode(&std::fs::read(path).map_err(|_| LogoError::Corrupt)?)
}

/// The default logo of new projects, as PNG bytes.
pub fn load_default(dir: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(dir.join(DEFAULT_FILE)).ok()?;
    ddc_model::png_size(&bytes).map(|_| bytes)
}

pub fn default_exists(dir: &Path) -> bool {
    dir.join(DEFAULT_FILE).is_file()
}

pub fn save_default(dir: &Path, png: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp: PathBuf = dir.join(format!("{DEFAULT_FILE}.tmp"));
    std::fs::write(&tmp, png)?;
    std::fs::rename(&tmp, dir.join(DEFAULT_FILE))
}

pub fn clear_default(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(DEFAULT_FILE));
}

/// A new project starts with the default logo when there is one. Not an undo step of its own
/// worth keeping: the model is fresh, so it is written before the project is marked clean.
pub fn apply_default(model: &mut ddc_model::Model, dir: Option<&Path>) {
    if let Some(png) = dir.and_then(load_default) {
        let _ = model.set_project_logo(Some(&png));
    }
}

/// The project's logo, decoded; a stored picture that does not decode counts as none.
pub fn project_logo(model: &ddc_model::Model) -> Option<Logo> {
    let bytes = model.project_logo().ok()??;
    Logo::from_png(&bytes).ok()
}

/// Box (px) of the logo in the thumbnail corner and its margin.
const THUMB_BOX: (u32, u32) = (72, 36);
const THUMB_PAD: u32 = 4;
const THUMB_MARGIN: u32 = 8;

/// Puts the logo on a light plate in the top-left corner of thumbnail pixels `px` (RGBA,
/// `size` px): the plate keeps a dark logo readable on a dark card and a light one on a light
/// card. Nothing changes when the thumbnail is too small for it.
pub fn stamp_thumb(px: &mut [u8], (w, h): (u32, u32), logo: &Logo) {
    if px.len() != (w as usize) * (h as usize) * 4 || w < 4 * THUMB_BOX.0 || h < 4 * THUMB_BOX.1 {
        return;
    }
    let small = logo.fit_within(THUMB_BOX.0, THUMB_BOX.1);
    let (pw, ph) = (small.width + 2 * THUMB_PAD, small.height + 2 * THUMB_PAD);
    let blend = |bg: u8, fg: u8, a: u8| -> u8 {
        ((u32::from(fg) * u32::from(a) + u32::from(bg) * (255 - u32::from(a)) + 127) / 255) as u8
    };
    for y in 0..ph {
        for x in 0..pw {
            let at = (((THUMB_MARGIN + y) * w + THUMB_MARGIN + x) * 4) as usize;
            px[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            let (lx, ly) = (x.wrapping_sub(THUMB_PAD), y.wrapping_sub(THUMB_PAD));
            if lx < small.width && ly < small.height {
                let s = ((ly * small.width + lx) * 4) as usize;
                let a = small.rgba[s + 3];
                for c in 0..3 {
                    px[at + c] = blend(255, small.rgba[s + c], a);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_png(w: u32, h: u32) -> Vec<u8> {
        let px = (0..w * h).flat_map(|_| [30u8, 120, 200, 255]).collect();
        Logo::from_rgba(w, h, px).unwrap().to_png()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-logo1-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn png_and_garbage_are_told_apart_by_content() {
        let l = decode(&rgba_png(30, 10)).unwrap();
        assert_eq!((l.width, l.height), (30, 10));
        assert_eq!(decode(b"GIF89a-not-supported").unwrap_err(), LogoError::Unsupported);
        assert_eq!(decode(b"<svg xmlns=\"x\"></svg>").unwrap_err(), LogoError::Unsupported);
        assert_eq!(decode(&[0xFF, 0xD8, 0xFF, 0, 0]).unwrap_err(), LogoError::Corrupt);
    }

    #[test]
    fn oversize_file_is_refused_unread() {
        let d = scratch("big");
        let f = d.join("big.png");
        let file = std::fs::File::create(&f).unwrap();
        file.set_len(MAX_SOURCE_BYTES as u64 + 1).unwrap();
        assert_eq!(read_file(&f).unwrap_err(), LogoError::TooLarge);
        assert_eq!(read_file(&d.join("missing.png")).unwrap_err(), LogoError::Corrupt);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_logo_round_trips_and_clears() {
        let d = scratch("def");
        assert!(!default_exists(&d) && load_default(&d).is_none());
        let png = rgba_png(8, 4);
        save_default(&d, &png).unwrap();
        assert!(default_exists(&d));
        assert_eq!(load_default(&d).as_deref(), Some(png.as_slice()));
        clear_default(&d);
        assert!(!default_exists(&d));
        std::fs::write(d.join(DEFAULT_FILE), b"not a png").unwrap();
        assert!(load_default(&d).is_none(), "a damaged file is no default");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_project_takes_the_default_logo_only_when_there_is_one() {
        let d = scratch("new");
        let mut m = ddc_model::Model::open(":memory:").unwrap();
        apply_default(&mut m, Some(&d));
        assert!(m.project_logo().unwrap().is_none());
        save_default(&d, &rgba_png(8, 4)).unwrap();
        apply_default(&mut m, Some(&d));
        assert!(project_logo(&m).is_some());
        let mut other = ddc_model::Model::open(":memory:").unwrap();
        apply_default(&mut other, None);
        assert!(other.project_logo().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn thumbnail_gets_a_plate_in_the_corner_and_nothing_elsewhere() {
        let (w, h) = (320u32, 200u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        let logo = Logo::from_rgba(40, 20, (0..800).flat_map(|_| [0u8, 0, 0, 255]).collect()).unwrap();
        stamp_thumb(&mut px, (w, h), &logo);
        let at = |x: u32, y: u32| (((y * w + x) * 4) as usize);
        // plate edge is white and opaque, logo ink is dark, the far corner is untouched
        assert_eq!(&px[at(THUMB_MARGIN, THUMB_MARGIN)..][..4], &[255, 255, 255, 255]);
        let ink = at(THUMB_MARGIN + THUMB_PAD + 5, THUMB_MARGIN + THUMB_PAD + 5);
        assert_eq!(&px[ink..ink + 4], &[0, 0, 0, 255]);
        assert_eq!(&px[at(w - 1, h - 1)..][..4], &[0, 0, 0, 0]);
        // too small a thumbnail is left alone
        let mut tiny = vec![0u8; 64 * 40 * 4];
        stamp_thumb(&mut tiny, (64, 40), &logo);
        assert!(tiny.iter().all(|&b| b == 0));
    }
}
