//! The verification view: how the assistant sees the result of its own work when the project
//! has not allowed snapshots to leave the editor. Rule `egress.verify_view` of the project
//! (`off`, `metrics` = the default, `image`): with `metrics` it gets measured facts of the
//! render (how much is drawn, where, how many colours, what the model holds) and no picture;
//! with `image` it also gets a downscaled picture, to the model in use only. A model on this
//! computer gets the full picture: nothing leaves the machine. `egress.snapshots` still
//! allows the full picture to any model.

use super::session::Core;
use ddc_ai::ImageData;
use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const POLICY: &str = "egress.verify_view";
/// Longest side of the picture the `image` rule sends.
pub const REDUCED_PX: u32 = 384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Full,
    Reduced,
    MetricsOnly,
}

/// The access a render gets; `None`: the person switched the view off.
pub fn decide(snapshots: bool, local_model: bool, rule: Option<&str>) -> Option<Access> {
    if snapshots || local_model {
        return Some(Access::Full);
    }
    match rule.map(|r| r.trim().to_lowercase()).as_deref() {
        Some("off") => None,
        Some("image") => Some(Access::Reduced),
        _ => Some(Access::MetricsOnly),
    }
}

pub const OFF: &str = "not_allowed: the project's AI policy egress.verify_view is off and egress.snapshots \
    does not allow images; the person sets egress.verify_view to metrics or image in the connection settings";

pub fn access(c: &Core) -> Result<Access, String> {
    let rule = c.ed.model.ai_policy(POLICY).ok().flatten();
    let local = ddc_ai::egress::is_local_url(c.endpoint());
    let snaps = c.allow_snapshots.load(std::sync::atomic::Ordering::Relaxed);
    decide(snaps, local, rule.as_deref()).ok_or_else(|| OFF.to_string())
}

fn decode(png_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut r = png::Decoder::new(png_bytes).read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("the render is not 8-bit RGBA".into());
    }
    buf.truncate(info.buffer_size());
    Ok((buf, info.width, info.height))
}

/// Facts read from the pixels of a render: the share drawn on the background, the box they
/// span (0..1 of the image), how many distinct colours there are, and whether it is blank.
pub fn metrics(png_bytes: &[u8]) -> Result<J, String> {
    let (px, w, h) = decode(png_bytes)?;
    let bg = [px[0], px[1], px[2]];
    let far = |p: &[u8]| (0..3).any(|k| p[k].abs_diff(bg[k]) > 12);
    let (mut n, mut x0, mut y0, mut x1, mut y1) = (0usize, w, h, 0, 0);
    let mut colours = std::collections::HashSet::new();
    for (i, p) in px.chunks_exact(4).enumerate() {
        if far(p) {
            let (x, y) = (i as u32 % w, i as u32 / w);
            n += 1;
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            colours.insert((p[0] >> 5, p[1] >> 5, p[2] >> 5));
        }
    }
    let share = n as f64 / (w as f64 * h as f64).max(1.0);
    let r = |v: f64| (v * 1000.0).round() / 1000.0;
    let bbox = if n == 0 {
        J::Null
    } else {
        json!([
            r(x0 as f64 / w as f64),
            r(y0 as f64 / h as f64),
            r((x1 + 1) as f64 / w as f64),
            r((y1 + 1) as f64 / h as f64)
        ])
    };
    Ok(json!({"pixels": [w, h], "drawn_share": r(share), "drawn_box": bbox,
              "colours": colours.len(), "blank": share < 0.0005}))
}

/// The picture reduced so its longest side is `max_px`; a smaller one is returned as it is.
pub fn downscale(img: &ImageData, max_px: u32) -> Result<ImageData, String> {
    let (px, w, h) = decode(&img.bytes)?;
    let long = w.max(h);
    if long <= max_px {
        return Ok(img.clone());
    }
    let (nw, nh) = ((w * max_px / long).max(1), (h * max_px / long).max(1));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        let (ya, yb) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1));
        for x in 0..nw {
            let (xa, xb) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1));
            let mut s = [0u32; 4];
            for yy in ya..yb.min(h) {
                for xx in xa..xb.min(w) {
                    let i = ((yy * w + xx) * 4) as usize;
                    (0..4).for_each(|k| s[k] += px[i + k] as u32);
                }
            }
            let cnt = ((yb.min(h) - ya) * (xb.min(w) - xa)).max(1);
            out.extend(s.iter().map(|v| (v / cnt) as u8));
        }
    }
    Ok(ImageData {
        mime: "image/png",
        bytes: Arc::new(super::session::encode_png(&out, nw, nh)?),
        width: nw,
        height: nh,
    })
}

/// What a render sends on: the pictures and the facts, by access.
pub fn deliver(acc: Access, img: ImageData) -> Result<(Vec<ImageData>, J), String> {
    let mut m = metrics(&img.bytes)?;
    Ok(match acc {
        Access::Full => (vec![img], m),
        Access::Reduced => (vec![downscale(&img, REDUCED_PX)?], m),
        Access::MetricsOnly => {
            m["picture"] = json!(
                "withheld by the project's rule egress.verify_view; the facts above are measured on the render"
            );
            (vec![], m)
        }
    })
}

/// What the model holds, by category: the check that a script made what it was meant to.
pub fn model_facts(c: &Core) -> J {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for r in super::rows::rows(&c.ed).iter() {
        let k = json!(r.cat).as_str().unwrap_or("?").to_string();
        *by.entry(k).or_default() += 1;
    }
    json!({"elements_by_category": by})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_png(w: u32, h: u32, side: u32) -> ImageData {
        let mut px = vec![255u8; (w * h * 4) as usize];
        for y in 10..10 + side {
            for x in 20..20 + side {
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
        }
        ImageData {
            mime: "image/png",
            bytes: Arc::new(super::super::session::encode_png(&px, w, h).unwrap()),
            width: w,
            height: h,
        }
    }

    #[test]
    fn the_rule_decides_what_leaves() {
        assert_eq!(decide(true, false, Some("off")), Some(Access::Full), "consent wins");
        assert_eq!(decide(false, true, None), Some(Access::Full), "a model on this computer");
        assert_eq!(decide(false, false, None), Some(Access::MetricsOnly));
        assert_eq!(decide(false, false, Some("metrics")), Some(Access::MetricsOnly));
        assert_eq!(decide(false, false, Some("Image")), Some(Access::Reduced));
        assert_eq!(decide(false, false, Some("off")), None);
    }

    #[test]
    fn metrics_measure_what_is_drawn_and_a_blank_view_says_so() {
        let m = metrics(&square_png(200, 100, 40).bytes).unwrap();
        assert_eq!(m["pixels"], json!([200, 100]));
        let share = m["drawn_share"].as_f64().unwrap();
        assert!((share - 1600.0 / 20000.0).abs() < 1e-3, "{share}");
        assert_eq!(m["drawn_box"], json!([0.1, 0.1, 0.3, 0.5]));
        assert_eq!(m["blank"], json!(false));
        let blank = metrics(&square_png(200, 100, 0).bytes).unwrap();
        assert_eq!(blank["blank"], json!(true));
        assert_eq!(blank["drawn_box"], J::Null);
    }

    #[test]
    fn only_the_reduced_rule_sends_a_small_picture_and_metrics_send_none() {
        let img = square_png(800, 400, 100);
        let (none, facts) = deliver(Access::MetricsOnly, img.clone()).unwrap();
        assert!(none.is_empty() && facts["picture"].as_str().unwrap().contains("withheld"));
        let (small, _) = deliver(Access::Reduced, img.clone()).unwrap();
        assert_eq!((small[0].width, small[0].height), (REDUCED_PX, REDUCED_PX / 2));
        let again = metrics(&small[0].bytes).unwrap();
        assert!(again["drawn_share"].as_f64().unwrap() > 0.0);
        let (full, _) = deliver(Access::Full, img).unwrap();
        assert_eq!(full[0].width, 800);
    }
}
