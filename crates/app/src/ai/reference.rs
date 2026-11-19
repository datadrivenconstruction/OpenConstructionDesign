//! A picture or the first page of a PDF the person attaches for comparison. It is downscaled
//! to a small image and kept on the worker; `get_reference` hands it to the model.

use super::fmt::Out;
use super::session::{encode_png, Core, Done};
use crate::mat_tex::decode_image;
use ddc_ai::{CallCx, ImageData};
use ddc_io::pdf::import::{PathSeg, PdfFile, Shape};
use serde_json::json;
use std::sync::Arc;

/// Long side of the stored image: enough to compare shapes, cheap to send.
pub const MAX_SIDE: u32 = 768;
/// Files above this are refused before decoding.
const MAX_BYTES: u64 = 20 * 1024 * 1024;

pub fn get_reference(
    c: &mut Core,
    _a: &serde_json::Value,
    _cx: &CallCx<'_>,
) -> Result<Done, String> {
    if c.reference.is_empty() {
        return Err("no reference image is attached".to_string());
    }
    let mut o = Out::new(c.max_chars);
    o.put("count", &json!(c.reference.len()));
    let sizes: Vec<_> = c.reference.iter().map(|i| json!([i.width, i.height])).collect();
    o.put("sizes_px", &json!(sizes));
    Ok(Done {
        reply: o.finish(),
        images: c.reference.clone(),
    })
}

/// Reads a PNG, a JPEG or a PDF (first page) and returns it as a small PNG image.
pub fn load(path: &std::path::Path) -> Result<ImageData, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_BYTES {
        return Err("file is too big".to_string());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    from_bytes(&bytes)
}

pub fn from_bytes(bytes: &[u8]) -> Result<ImageData, String> {
    let (w, h, rgba) = if bytes.starts_with(b"%PDF-") {
        raster_pdf(bytes, 0)?
    } else {
        decode_image(bytes).ok_or("not a PNG, JPEG or PDF file")?
    };
    small_png(w, h, rgba)
}

/// One page (0-based) of a PDF as a small PNG image.
pub fn pdf_page(bytes: &[u8], page: usize) -> Result<ImageData, String> {
    let (w, h, rgba) = raster_pdf(bytes, page)?;
    small_png(w, h, rgba)
}

fn small_png(w: u32, h: u32, rgba: Vec<u8>) -> Result<ImageData, String> {
    let (w, h, rgba) = fit(w, h, rgba);
    let png = encode_png(&rgba, w, h)?;
    Ok(ImageData {
        mime: "image/png",
        bytes: Arc::new(png),
        width: w,
        height: h,
    })
}

/// Box-filter downscale so the long side is at most `MAX_SIDE`.
fn fit(w: u32, h: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let long = w.max(h);
    if long <= MAX_SIDE || w == 0 || h == 0 {
        return (w, h, rgba);
    }
    let k = MAX_SIDE as f64 / long as f64;
    let (nw, nh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1).min(h));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1).min(w));
            let mut acc = [0u32; 4];
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = ((yy * w + xx) * 4) as usize;
                    for (a, v) in acc.iter_mut().zip(&rgba[i..i + 4]) {
                        *a += *v as u32;
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((y * nw + x) * 4) as usize;
            for (d, a) in out[o..o + 4].iter_mut().zip(acc) {
                *d = (a / n) as u8;
            }
        }
    }
    (nw, nh, out)
}

/// One page as strokes on white: outlines and fills' edges, no text, no raster content.
fn raster_pdf(bytes: &[u8], index: usize) -> Result<(u32, u32, Vec<u8>), String> {
    let doc = PdfFile::parse(bytes).map_err(|e| e.to_string())?;
    let page = doc.page(index).map_err(|e| e.to_string())?;
    if page.width_mm <= 0.0 || page.height_mm <= 0.0 {
        return Err("the PDF page is empty".to_string());
    }
    let k = MAX_SIDE as f64 / page.width_mm.max(page.height_mm);
    let (w, h) = ((page.width_mm * k).ceil() as u32 + 1, (page.height_mm * k).ceil() as u32 + 1);
    let mut img = vec![255u8; (w * h * 4) as usize];
    for s in &page.shapes {
        draw_shape(&mut img, w, h, k, s);
    }
    Ok((w, h, img))
}

fn draw_shape(img: &mut [u8], w: u32, h: u32, k: f64, s: &Shape) {
    if !s.stroke && !s.fill {
        return;
    }
    let px = |p: [f64; 2]| [p[0] * k, (h as f64 - 1.0) - p[1] * k];
    for sp in &s.subpaths {
        let mut pts = vec![px(sp.start)];
        for seg in &sp.segs {
            let last = *pts.last().unwrap_or(&px(sp.start));
            match seg {
                PathSeg::Line(p) => pts.push(px(*p)),
                PathSeg::Cubic(a, b, e) => {
                    let (a, b, e) = (px(*a), px(*b), px(*e));
                    for i in 1..=12 {
                        let t = i as f64 / 12.0;
                        let u = 1.0 - t;
                        let f = |j: usize| {
                            u * u * u * last[j] + 3.0 * u * u * t * a[j] + 3.0 * u * t * t * b[j]
                                + t * t * t * e[j]
                        };
                        pts.push([f(0), f(1)]);
                    }
                }
            }
        }
        if sp.closed {
            pts.push(pts[0]);
        }
        for p in pts.windows(2) {
            line(img, w, h, p[0], p[1], s.rgb);
        }
    }
}

fn line(img: &mut [u8], w: u32, h: u32, a: [f64; 2], b: [f64; 2], rgb: [u8; 3]) {
    let n = ((b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil() as usize).max(1);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let (x, y) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
        if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
            continue;
        }
        let o = ((y as u32 * w + x as u32) * 4) as usize;
        img[o..o + 3].copy_from_slice(&rgb);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_and_shrinks() {
        let (w, h) = (1600u32, 800u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|_| [10u8, 20, 30, 255]).collect();
        let png = encode_png(&rgba, w, h).unwrap();
        let img = from_bytes(&png).unwrap();
        assert_eq!((img.width, img.height), (MAX_SIDE, MAX_SIDE / 2));
        assert_eq!(img.mime, "image/png");
    }

    #[test]
    fn garbage_is_refused() {
        assert!(from_bytes(b"not an image").is_err());
    }
}
