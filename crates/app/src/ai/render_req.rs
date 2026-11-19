// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Pure side of `render_plan` / `render_3d`: argument parsing, the pixel cap, the explicit scale
//! and the legend. No GPU here; `render_off` draws what these types describe.

use ddc_render::glam::Vec3;
use ddc_render::{Aabb, OrbitCamera};
use serde_json::{json, Value as J};

/// Longest image side the model may ask for.
pub const MAX_SIDE: u32 = 1280;
/// Total pixel budget of one render.
pub const MAX_PIXELS: u64 = 1_000_000;
/// Plan scale limits, pixels per metre.
pub const MIN_SCALE: f64 = 0.5;
pub const MAX_SCALE: f64 = 2000.0;
pub const DEFAULT_SCALE: f64 = 50.0;
/// Legend rows kept; the reply says how many were cut.
pub const MAX_LEGEND: usize = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct PlanReq {
    pub level: i64,
    /// `[x0, y0, x1, y1]` metres; `None` means the whole level.
    pub region: Option<[f64; 4]>,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    All,
    Refs(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Render3d {
    pub target: Target,
    /// Degrees; `None` takes the camera default.
    pub yaw: Option<f64>,
    pub pitch: Option<f64>,
    pub ortho: bool,
    pub max_px: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Legend {
    pub label: String,
    pub reference: String,
    pub kind: String,
    pub at: [f64; 3],
}

/// `level` is an id or a full level name.
pub fn resolve_level(arg: &J, levels: &[(i64, String, f64)]) -> Result<i64, String> {
    let known = || {
        levels
            .iter()
            .map(|l| format!("{}:{}", l.0, l.1))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let found = match arg {
        J::Number(n) => n.as_i64().and_then(|id| levels.iter().find(|l| l.0 == id)),
        J::String(s) => levels.iter().find(|l| l.1 == *s),
        _ => return Err("level must be a level id or a level name".to_string()),
    };
    found
        .map(|l| l.0)
        .ok_or_else(|| format!("unknown level {arg}; levels: {}", known()))
}

fn finite(a: &J, k: &str) -> Result<Option<f64>, String> {
    match &a[k] {
        J::Null => Ok(None),
        v => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{k} must be a number")),
    }
}

pub fn parse_plan(a: &J, levels: &[(i64, String, f64)]) -> Result<PlanReq, String> {
    let level = match &a["level"] {
        J::Null => levels
            .first()
            .map(|l| l.0)
            .ok_or_else(|| "the project has no levels".to_string())?,
        v => resolve_level(v, levels)?,
    };
    let region = match &a["region"] {
        J::Null => None,
        v => {
            let r: Vec<f64> = v
                .as_array()
                .filter(|r| r.len() == 4)
                .map(|r| r.iter().filter_map(J::as_f64).collect())
                .filter(|r: &Vec<f64>| r.len() == 4 && r.iter().all(|x| x.is_finite()))
                .ok_or_else(|| "region must be [x0, y0, x1, y1] in metres".to_string())?;
            let (x0, x1) = (r[0].min(r[2]), r[0].max(r[2]));
            let (y0, y1) = (r[1].min(r[3]), r[1].max(r[3]));
            if x1 - x0 < 0.01 || y1 - y0 < 0.01 {
                return Err("region is empty: it must span at least 0.01 m on both axes".into());
            }
            Some([x0, y0, x1, y1])
        }
    };
    let scale = match finite(a, "scale")? {
        None => DEFAULT_SCALE,
        Some(s) if (MIN_SCALE..=MAX_SCALE).contains(&s) => s,
        Some(s) => {
            return Err(format!(
                "scale {s} is out of range: {MIN_SCALE} to {MAX_SCALE} pixels per metre"
            ))
        }
    };
    Ok(PlanReq {
        level,
        region,
        scale,
    })
}

/// Image size for a region at a scale. When the cap bites, the scale shrinks and `capped` is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub w: u32,
    pub h: u32,
    pub scale: f64,
    pub requested: f64,
    pub capped: bool,
}

pub fn plan_size(region_w: f64, region_h: f64, scale: f64) -> Size {
    let (rw, rh) = (region_w.max(0.01), region_h.max(0.01));
    let px = |s: f64| ((rw * s).ceil().max(1.0), (rh * s).ceil().max(1.0));
    let (w0, h0) = px(scale);
    let over_side = (w0.max(h0) / MAX_SIDE as f64).max(1.0);
    let over_area = ((w0 * h0) / MAX_PIXELS as f64).sqrt().max(1.0);
    let k = over_side.max(over_area);
    let mut s = scale / k;
    // Rounding up can still step past the cap by a pixel.
    loop {
        let (w, h) = px(s);
        if w <= MAX_SIDE as f64 && h <= MAX_SIDE as f64 && w * h <= MAX_PIXELS as f64 {
            return Size {
                w: w as u32,
                h: h as u32,
                scale: s,
                requested: scale,
                capped: k > 1.0,
            };
        }
        s *= 0.999;
    }
}

pub fn parse_3d(a: &J) -> Result<Render3d, String> {
    let target = match &a["target"] {
        J::Null => Target::All,
        J::String(s) if s == "all" => Target::All,
        v => {
            let refs: Vec<String> = v["refs"]
                .as_array()
                .map(|r| {
                    r.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if refs.is_empty() {
                return Err("target must be \"all\" or {\"refs\": [\"wall:12\", ...]}".into());
            }
            Target::Refs(refs)
        }
    };
    let cam = &a["camera"];
    let (mut yaw, mut pitch, mut ortho) = (None, None, false);
    if let Some(p) = cam["preset"].as_str() {
        let (y, pt, o) = preset(p).ok_or_else(|| {
            format!("unknown camera preset {p}; use iso, top, front, back, left or right")
        })?;
        (yaw, pitch, ortho) = (Some(y), Some(pt), o);
    }
    for (k, slot, lo, hi) in [
        ("yaw", &mut yaw, -360.0, 360.0),
        ("pitch", &mut pitch, -89.0, 89.0),
    ] {
        if let Some(v) = finite(cam, k)? {
            if !(lo..=hi).contains(&v) {
                return Err(format!(
                    "camera {k} {v} is out of range {lo} to {hi} degrees"
                ));
            }
            *slot = Some(v);
        }
    }
    let max_px = match &a["max_px"] {
        J::Null => 1024,
        v => match v.as_u64() {
            Some(n) if (16..=MAX_SIDE as u64).contains(&n) => n as u32,
            _ => return Err(format!("max_px must be an integer from 16 to {MAX_SIDE}")),
        },
    };
    Ok(Render3d {
        target,
        yaw,
        pitch,
        ortho,
        max_px,
    })
}

/// (yaw, pitch, ortho) in degrees.
fn preset(name: &str) -> Option<(f64, f64, bool)> {
    Some(match name {
        "iso" => (-126.0, 34.0, false),
        "top" => (-90.0, 89.0, true),
        "front" => (-90.0, 0.0, true),
        "back" => (90.0, 0.0, true),
        "left" => (180.0, 0.0, true),
        "right" => (0.0, 0.0, true),
        _ => return None,
    })
}

/// 4:3 frame whose long side is `max_px`.
pub fn size_3d(max_px: u32) -> (u32, u32) {
    let w = max_px.clamp(16, MAX_SIDE);
    (w, ((w as u64 * 3 / 4) as u32).max(16))
}

/// Camera that looks at `b` from the requested angles and fits it in a `w` x `h` frame.
pub fn camera_for(r: &Render3d, b: &Aabb, (w, h): (u32, u32)) -> OrbitCamera {
    let mut c = OrbitCamera::default();
    c.yaw = r.yaw.map_or(c.yaw, |d| d.to_radians() as f32);
    c.pitch = r.pitch.map_or(c.pitch, |d| d.to_radians() as f32);
    c.ortho = r.ortho;
    c.fit(b, w as f32 / h as f32);
    c
}

/// True when all eight corners of `b` land inside the frame.
pub fn box_in_frame(c: &OrbitCamera, b: &Aabb, aspect: f32) -> bool {
    (0..8).all(|i| {
        let p = Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        );
        let n = c.project(p, aspect);
        n.x.abs() <= 1.0 && n.y.abs() <= 1.0
    })
}

/// Rows labelled 1..n in a stable order (by ref), cut at `MAX_LEGEND`.
pub fn build_legend(mut items: Vec<(String, String, [f64; 3])>) -> (Vec<Legend>, usize) {
    items.sort_by(|a, b| natural(&a.0).cmp(&natural(&b.0)));
    items.dedup_by(|a, b| a.0 == b.0);
    let cut = items.len().saturating_sub(MAX_LEGEND);
    let rows = items
        .into_iter()
        .take(MAX_LEGEND)
        .enumerate()
        .map(|(i, (reference, kind, at))| Legend {
            label: (i + 1).to_string(),
            reference,
            kind,
            at,
        })
        .collect();
    (rows, cut)
}

/// `wall:9` sorts before `wall:10`.
fn natural(s: &str) -> (String, i64) {
    let (cat, id) = s.split_once(':').unwrap_or((s, ""));
    (cat.to_string(), id.parse().unwrap_or(i64::MAX))
}

pub fn legend_json(rows: &[Legend], cut: usize) -> J {
    let v = json!(rows
        .iter()
        .map(|r| json!({"label": r.label, "ref": r.reference, "kind": r.kind,
            "at": [round(r.at[0]), round(r.at[1]), round(r.at[2])]}))
        .collect::<Vec<_>>());
    if cut > 0 {
        json!({"rows": v, "elided": {"rows": cut}})
    } else {
        v
    }
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Scale statement the model reads next to the image.
pub fn scale_json(s: &Size) -> J {
    json!({
        "px_per_m": round(s.scale),
        "m_per_px": (1.0 / s.scale * 10_000.0).round() / 10_000.0,
        "requested_px_per_m": s.requested,
        "capped": s.capped,
        "max_pixels": MAX_PIXELS,
        "max_side": MAX_SIDE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels() -> Vec<(i64, String, f64)> {
        vec![(1, "Level 1".into(), 0.0), (2, "Level 2".into(), 3.0)]
    }

    #[test]
    fn plan_size_is_region_times_scale() {
        let s = plan_size(10.0, 6.0, 50.0);
        assert_eq!((s.w, s.h), (500, 300));
        assert!(!s.capped);
        assert_eq!(s.scale, 50.0);
    }

    #[test]
    fn pixel_cap_is_enforced_and_scale_reported() {
        let s = plan_size(400.0, 300.0, 50.0);
        assert!(s.capped);
        assert!(s.w <= MAX_SIDE && s.h <= MAX_SIDE);
        assert!(s.w as u64 * s.h as u64 <= MAX_PIXELS);
        assert!(s.scale < 50.0);
        assert_eq!(s.requested, 50.0);
        let j = scale_json(&s);
        assert_eq!(j["capped"], true);
        assert!(j["px_per_m"].as_f64().unwrap() < 50.0);
        let big = plan_size(1.0e6, 1.0e6, 2000.0);
        assert!(big.w as u64 * big.h as u64 <= MAX_PIXELS);
    }

    #[test]
    fn square_area_cap_binds_before_side_cap() {
        let s = plan_size(20.0, 20.0, 100.0);
        assert!(s.w as u64 * s.h as u64 <= MAX_PIXELS);
        assert!(s.capped);
    }

    #[test]
    fn bad_level_gives_a_reason_naming_known_levels() {
        let e = parse_plan(&json!({"level": 9}), &levels()).unwrap_err();
        assert!(e.contains("unknown level 9"));
        assert!(e.contains("1:Level 1") && e.contains("2:Level 2"));
        let e = parse_plan(&json!({"level": "Level 3"}), &levels()).unwrap_err();
        assert!(e.contains("unknown level"));
        assert!(parse_plan(&json!({"level": true}), &levels()).is_err());
        assert!(parse_plan(&json!({}), &[])
            .unwrap_err()
            .contains("no levels"));
    }

    #[test]
    fn level_by_name_or_id() {
        let p = |a| parse_plan(&a, &levels()).unwrap().level;
        assert_eq!(p(json!({"level": "Level 2"})), 2);
        assert_eq!(p(json!({"level": 1})), 1);
        assert_eq!(p(json!({})), 1);
    }

    #[test]
    fn region_and_scale_are_validated() {
        let p = parse_plan(
            &json!({"level": 1, "region": [5, 4, 0, 0], "scale": 20}),
            &levels(),
        )
        .unwrap();
        assert_eq!(p.region, Some([0.0, 0.0, 5.0, 4.0]));
        assert_eq!(p.scale, 20.0);
        assert!(parse_plan(&json!({"region": [0, 0, 1]}), &levels()).is_err());
        assert!(parse_plan(&json!({"region": [0, 0, 0, 5]}), &levels()).is_err());
        assert!(parse_plan(&json!({"scale": 0}), &levels())
            .unwrap_err()
            .contains("out of range"));
        assert!(parse_plan(&json!({"scale": "x"}), &levels()).is_err());
    }

    #[test]
    fn parsing_is_deterministic() {
        let a = json!({"level": "Level 2", "region": [0, 0, 8, 6], "scale": 30});
        assert_eq!(parse_plan(&a, &levels()), parse_plan(&a, &levels()));
        let b = json!({"target": {"refs": ["wall:3"]}, "camera": {"preset": "iso"}});
        assert_eq!(parse_3d(&b), parse_3d(&b));
    }

    #[test]
    fn parse_3d_defaults_and_errors() {
        let r = parse_3d(&json!({})).unwrap();
        assert_eq!(r.target, Target::All);
        assert_eq!(r.max_px, 1024);
        assert!(parse_3d(&json!({"target": {"refs": []}})).is_err());
        assert!(parse_3d(&json!({"camera": {"preset": "up"}}))
            .unwrap_err()
            .contains("preset"));
        assert!(parse_3d(&json!({"camera": {"pitch": 120}})).is_err());
        assert!(parse_3d(&json!({"max_px": 5000})).is_err());
        assert!(parse_3d(&json!({"camera": {"preset": "top"}})).unwrap().ortho);
    }

    #[test]
    fn render_3d_target_in_frame() {
        let b = Aabb {
            min: Vec3::new(2.0, 1.0, 0.0),
            max: Vec3::new(7.0, 1.2, 3.0),
        };
        for cam in [
            json!({"preset": "iso"}),
            json!({"preset": "top"}),
            json!({"yaw": 30, "pitch": 10}),
        ] {
            let r = parse_3d(&json!({ "camera": cam })).unwrap();
            let size = size_3d(r.max_px);
            let c = camera_for(&r, &b, size);
            assert!(box_in_frame(&c, &b, size.0 as f32 / size.1 as f32), "{cam}");
        }
    }

    #[test]
    fn legend_is_sorted_numbered_and_cut() {
        let items = vec![
            ("wall:10".to_string(), "wall".to_string(), [0.0; 3]),
            ("wall:9".to_string(), "wall".to_string(), [1.0; 3]),
            ("wall:9".to_string(), "wall".to_string(), [1.0; 3]),
        ];
        let (rows, cut) = build_legend(items);
        assert_eq!(cut, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].label.as_str(), rows[0].reference.as_str()),
            ("1", "wall:9")
        );
        assert_eq!(rows[1].reference, "wall:10");
        let many = (0..MAX_LEGEND as i64 + 5)
            .map(|i| (format!("wall:{i}"), "wall".to_string(), [0.0; 3]))
            .collect();
        let (rows, cut) = build_legend(many);
        assert_eq!((rows.len(), cut), (MAX_LEGEND, 5));
        assert_eq!(legend_json(&rows, cut)["elided"]["rows"], 5);
    }
}
