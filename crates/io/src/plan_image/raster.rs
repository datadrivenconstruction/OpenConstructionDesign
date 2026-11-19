//! Pixel side of plan reading: grey image -> ink mask -> one-pixel skeleton. Thick ink
//! (filled walls) keeps only its outline, thin strokes are thinned to their centre line, so a
//! poche wall and an outlined wall both end up as two edge lines.

/// 8-bit grey image, rows top to bottom, 0 is black.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    /// A white image.
    pub fn new(w: usize, h: usize) -> Gray {
        Gray { w, h, px: vec![255; w * h] }
    }

    /// RGBA rows top to bottom, composited on white.
    pub fn from_rgba(w: u32, h: u32, rgba: &[u8]) -> Option<Gray> {
        let (w, h) = (w as usize, h as usize);
        if w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return None;
        }
        let px = rgba
            .chunks_exact(4)
            .take(w * h)
            .map(|p| {
                let l = (299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32) / 1000;
                let a = p[3] as u32;
                ((l * a + 255 * (255 - a)) / 255) as u8
            })
            .collect();
        Some(Gray { w, h, px })
    }

    /// 8-bit greyscale PNG (test pictures, saved reads).
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w as u32, self.h as u32);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().map_err(|e| e.to_string())?;
            w.write_image_data(&self.px).map_err(|e| e.to_string())?;
        }
        Ok(out)
    }
}

/// Otsu threshold: grey values at or below it are ink.
pub(crate) fn otsu(g: &Gray) -> u8 {
    let mut hist = [0u64; 256];
    for &v in &g.px {
        hist[v as usize] += 1;
    }
    let total = g.px.len() as f64;
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let (mut w0, mut sum0) = (0.0, 0.0);
    let (mut best, mut best_t) = (-1.0, 127u8);
    for t in 0..255usize {
        w0 += hist[t] as f64;
        sum0 += t as f64 * hist[t] as f64;
        let w1 = total - w0;
        if w0 == 0.0 || w1 == 0.0 {
            continue;
        }
        let (m0, m1) = (sum0 / w0, (sum_all - sum0) / w1);
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best {
            best = between;
            best_t = t as u8;
        }
    }
    best_t
}

/// Chessboard distance to the nearest pixel where `seed` is false (outside counts as false).
fn chess_dt(w: usize, h: usize, seed: &[bool]) -> Vec<u16> {
    let mut d = vec![0u16; w * h];
    let at = |d: &[u16], x: isize, y: isize| -> u16 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            0
        } else {
            d[y as usize * w + x as usize]
        }
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !seed[i] {
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            let m = at(&d, xi - 1, yi)
                .min(at(&d, xi - 1, yi - 1))
                .min(at(&d, xi, yi - 1))
                .min(at(&d, xi + 1, yi - 1));
            d[i] = m.saturating_add(1);
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if !seed[i] {
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            let m = at(&d, xi + 1, yi)
                .min(at(&d, xi + 1, yi + 1))
                .min(at(&d, xi, yi + 1))
                .min(at(&d, xi - 1, yi + 1))
                .saturating_add(1);
            d[i] = d[i].min(m);
        }
    }
    d
}

/// Ink classified for line finding.
pub(crate) struct Masks {
    pub ink: Vec<bool>,
    /// Ink of strokes at least [`THICK_PX`] wide (filled walls, columns).
    pub thick: Vec<bool>,
    /// One-pixel lines: the outline of thick ink plus thin strokes thinned.
    pub skel: Vec<bool>,
}

/// Strokes this wide or wider are filled areas; narrower ones are lines.
pub const THICK_PX: u16 = 5;

pub(crate) fn masks(g: &Gray) -> Masks {
    let (w, h) = (g.w, g.h);
    let t = otsu(g);
    let ink: Vec<bool> = g.px.iter().map(|&v| v <= t && v < 250).collect();
    // a pixel THICK_PX/2+1 deep inside ink sits in a stroke at least THICK_PX wide
    let depth = THICK_PX / 2 + 1;
    let d = chess_dt(w, h, &ink);
    let not_core: Vec<bool> = d.iter().map(|&v| v < depth).collect();
    let to_core = chess_dt(w, h, &not_core);
    let thick: Vec<bool> = (0..w * h).map(|i| ink[i] && to_core[i] < depth).collect();
    let mut skel = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !thick[i] {
                continue;
            }
            let bg = |dx: isize, dy: isize| {
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize || !ink[ny as usize * w + nx as usize]
            };
            skel[i] = bg(-1, 0) || bg(1, 0) || bg(0, -1) || bg(0, 1);
        }
    }
    let mut thin: Vec<bool> = (0..w * h).map(|i| ink[i] && !thick[i]).collect();
    zhang_suen(w, h, &mut thin);
    for i in 0..w * h {
        skel[i] |= thin[i];
    }
    Masks { ink, thick, skel }
}

/// Zhang-Suen thinning in place; pixels outside the image are background.
fn zhang_suen(w: usize, h: usize, m: &mut [bool]) {
    let mut live: Vec<usize> = (0..w * h).filter(|&i| m[i]).collect();
    let get = |m: &[bool], x: isize, y: isize| -> bool {
        x >= 0 && y >= 0 && x < w as isize && y < h as isize && m[y as usize * w + x as usize]
    };
    loop {
        let mut changed = false;
        for step in 0..2 {
            let mut del = vec![];
            let mm: &[bool] = &*m;
            for &i in &live {
                if !mm[i] {
                    continue;
                }
                let (x, y) = ((i % w) as isize, (i / w) as isize);
                // P2..P9 clockwise from north
                let p = [
                    get(mm, x, y - 1),
                    get(mm, x + 1, y - 1),
                    get(mm, x + 1, y),
                    get(mm, x + 1, y + 1),
                    get(mm, x, y + 1),
                    get(mm, x - 1, y + 1),
                    get(mm, x - 1, y),
                    get(mm, x - 1, y - 1),
                ];
                let b = p.iter().filter(|&&v| v).count();
                if !(2..=6).contains(&b) {
                    continue;
                }
                let a = (0..8).filter(|&k| !p[k] && p[(k + 1) % 8]).count();
                if a != 1 {
                    continue;
                }
                let (p2, p4, p6, p8) = (p[0], p[2], p[4], p[6]);
                let ok = if step == 0 {
                    !(p2 && p4 && p6) && !(p4 && p6 && p8)
                } else {
                    !(p2 && p4 && p8) && !(p2 && p6 && p8)
                };
                if ok {
                    del.push(i);
                }
            }
            for &i in &del {
                m[i] = false;
            }
            changed |= !del.is_empty();
        }
        live.retain(|&i| m[i]);
        if !changed {
            break;
        }
    }
}

/// Bounding box (x0, y0, x1, y1) of the set pixels, inclusive.
pub(crate) fn bbox(w: usize, m: &[bool]) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for (i, _) in m.iter().enumerate().filter(|(_, &v)| v) {
        let (x, y) = (i % w, i / w);
        b = Some(match b {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }
    b
}
