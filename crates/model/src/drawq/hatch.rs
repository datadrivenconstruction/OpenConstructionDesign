// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Hatch families: parallel line sets with angle, origin, per-line shift, spacing and dashes.
//! A pattern is one or more families. Drafting patterns keep their spacing in mm on paper (the
//! result scales with the view scale); model patterns keep it in metres and follow the object.
//! Regions are rings filled by the even-odd rule, so holes work. A work budget stops dense
//! patterns before they hang the drawing.

use serde_json::{json, Value as J};

type P = (f64, f64);

/// Most hatch lines one call may produce per family.
pub const MAX_LINES: usize = 20_000;
/// Most segments one call may return.
pub const MAX_SEGMENTS: usize = 200_000;

#[derive(Clone, Debug, PartialEq)]
pub struct HatchFamily {
    /// Degrees, counter-clockwise from +x.
    pub angle: f64,
    pub origin: P,
    /// Shift along the line direction between neighbouring lines, same unit as `spacing`.
    pub shift: f64,
    /// Distance between neighbouring lines.
    pub spacing: f64,
    /// Alternating drawn and gap lengths; empty means solid lines.
    pub dashes: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HatchPattern {
    pub name: String,
    pub families: Vec<HatchFamily>,
    /// true: lengths are metres on the model; false: mm on paper at the view scale.
    pub model: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HatchError {
    /// The pattern has a bad number (zero spacing, NaN, negative dash).
    Invalid,
    /// Too many lines for the region: draw a solid fill instead.
    TooDense,
}

impl HatchFamily {
    fn check(&self) -> bool {
        self.angle.is_finite()
            && self.origin.0.is_finite()
            && self.origin.1.is_finite()
            && self.shift.is_finite()
            && self.spacing.is_finite()
            && self.spacing > 1e-9
            && self.dashes.len() <= 16
            && self.dashes.iter().all(|d| d.is_finite() && *d >= 0.0)
            && (self.dashes.is_empty() || self.dashes.iter().sum::<f64>() > 1e-9)
    }
}

impl HatchPattern {
    pub fn check(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.families.is_empty()
            && self.families.len() <= 8
            && self.families.iter().all(HatchFamily::check)
    }

    /// Hatch segments in model metres. `denom` is the view scale; it only matters for drafting
    /// patterns. Regions are rings (outer and holes together, even-odd).
    pub fn strokes(&self, rings: &[Vec<P>], denom: f64) -> Result<Vec<(P, P)>, HatchError> {
        if !self.check() || !(denom.is_finite() && denom > 0.0) {
            return Err(HatchError::Invalid);
        }
        let k = if self.model { 1.0 } else { denom / 1000.0 };
        let mut out = vec![];
        for f in &self.families {
            family_strokes(f, k, rings, &mut out)?;
        }
        Ok(out)
    }

    pub fn to_json(&self) -> J {
        let fams: Vec<J> = self
            .families
            .iter()
            .map(|f| {
                json!({
                    "angle": f.angle,
                    "origin": [f.origin.0, f.origin.1],
                    "shift": f.shift,
                    "spacing": f.spacing,
                    "dashes": f.dashes,
                })
            })
            .collect();
        json!({"name": self.name, "model": self.model, "families": fams})
    }

    pub fn from_json(j: &J) -> Option<Self> {
        let mut families = vec![];
        for f in j["families"].as_array()? {
            families.push(HatchFamily {
                angle: f["angle"].as_f64()?,
                origin: (f["origin"][0].as_f64()?, f["origin"][1].as_f64()?),
                shift: f["shift"].as_f64().unwrap_or(0.0),
                spacing: f["spacing"].as_f64()?,
                dashes: f["dashes"]
                    .as_array()
                    .map(|a| a.iter().filter_map(J::as_f64).collect())
                    .unwrap_or_default(),
            });
        }
        let p = HatchPattern {
            name: j["name"].as_str()?.into(),
            families,
            model: j["model"].as_bool().unwrap_or(false),
        };
        p.check().then_some(p)
    }
}

fn family_strokes(
    f: &HatchFamily,
    k: f64,
    rings: &[Vec<P>],
    out: &mut Vec<(P, P)>,
) -> Result<(), HatchError> {
    let (sp, shift) = (f.spacing * k, f.shift * k);
    let dashes: Vec<f64> = f.dashes.iter().map(|d| d * k).collect();
    let period: f64 = dashes.iter().sum();
    let (s, c) = f.angle.to_radians().sin_cos();
    let (d, n) = ((c, s), (-s, c));
    let o = (f.origin.0, f.origin.1);
    let proj = |p: P| -> (f64, f64) {
        let v = (p.0 - o.0, p.1 - o.1);
        (v.0 * d.0 + v.1 * d.1, v.0 * n.0 + v.1 * n.1)
    };
    let pts: Vec<(f64, f64)> = rings.iter().flatten().map(|p| proj(*p)).collect();
    if pts.is_empty() {
        return Ok(());
    }
    let (lo, hi) = pts
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let (k0, k1) = ((lo / sp).ceil(), (hi / sp).floor());
    if !(k0.is_finite() && k1.is_finite()) || k1 - k0 + 1.0 > MAX_LINES as f64 {
        return Err(HatchError::TooDense);
    }
    let local: Vec<Vec<(f64, f64)>> = rings
        .iter()
        .map(|r| r.iter().map(|p| proj(*p)).collect())
        .collect();
    let to_world = |u: f64, t: f64| -> P { (o.0 + d.0 * u + n.0 * t, o.1 + d.1 * u + n.1 * t) };
    let mut ki = k0 as i64;
    while ki as f64 <= k1 {
        let t = ki as f64 * sp;
        let mut us: Vec<f64> = vec![];
        for r in &local {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                // half-open rule: a vertex on the line counts for exactly one of its edges
                if (a.1 < t) != (b.1 < t) {
                    let w = (t - a.1) / (b.1 - a.1);
                    us.push(a.0 + (b.0 - a.0) * w);
                }
            }
        }
        us.sort_by(f64::total_cmp);
        for pair in us.chunks_exact(2) {
            let (u0, u1) = (pair[0], pair[1]);
            if dashes.is_empty() {
                out.push((to_world(u0, t), to_world(u1, t)));
            } else {
                dash_run(u0, u1, ki as f64 * shift, &dashes, period, |a, b| {
                    out.push((to_world(a, t), to_world(b, t)));
                });
            }
            if out.len() > MAX_SEGMENTS {
                return Err(HatchError::TooDense);
            }
        }
        ki += 1;
    }
    Ok(())
}

/// Calls `emit` for the drawn parts of `u0..u1`. The dash cycle is anchored at `anchor`.
fn dash_run(u0: f64, u1: f64, anchor: f64, dashes: &[f64], period: f64, mut emit: impl FnMut(f64, f64)) {
    let mut pos = anchor + ((u0 - anchor) / period).floor() * period;
    let mut guard = 0usize;
    while pos < u1 && guard < MAX_SEGMENTS {
        for (i, d) in dashes.iter().enumerate() {
            let (a, b) = (pos.max(u0), (pos + d).min(u1));
            if i % 2 == 0 && b > a {
                emit(a, b);
            }
            pos += d;
            guard += 1;
            if pos >= u1 {
                break;
            }
        }
    }
}

/// Built-in drafting patterns (mm on paper). Every entry passes [`HatchPattern::check`].
pub fn builtin() -> Vec<HatchPattern> {
    let fam = |angle: f64, spacing: f64, dashes: &[f64]| HatchFamily {
        angle,
        origin: (0.0, 0.0),
        shift: 0.0,
        spacing,
        dashes: dashes.to_vec(),
    };
    let pat = |name: &str, families: Vec<HatchFamily>| HatchPattern {
        name: name.into(),
        families,
        model: false,
    };
    vec![
        pat("diagonal", vec![fam(45.0, 3.0, &[])]),
        pat("diagonal_back", vec![fam(135.0, 3.0, &[])]),
        pat("diagonal_wide", vec![fam(45.0, 6.0, &[])]),
        pat("horizontal", vec![fam(0.0, 3.0, &[])]),
        pat("vertical", vec![fam(90.0, 3.0, &[])]),
        pat("cross", vec![fam(0.0, 3.0, &[]), fam(90.0, 3.0, &[])]),
        pat("cross_diagonal", vec![fam(45.0, 3.0, &[]), fam(135.0, 3.0, &[])]),
        pat("dashed_diagonal", vec![fam(45.0, 3.0, &[2.0, 1.0])]),
        pat(
            "dots",
            vec![
                HatchFamily {
                    shift: 1.5,
                    ..fam(0.0, 3.0, &[0.1, 2.9])
                },
            ],
        ),
        pat(
            "brick",
            vec![
                fam(0.0, 6.0, &[]),
                HatchFamily {
                    shift: 6.0,
                    ..fam(90.0, 12.0, &[6.0, 6.0])
                },
            ],
        ),
    ]
}

/// Built-in patterns with the user's on top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HatchLib {
    pub user: Vec<HatchPattern>,
}

impl HatchLib {
    pub fn get(&self, name: &str) -> Option<HatchPattern> {
        self.user
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .or_else(|| builtin().into_iter().find(|p| p.name == name))
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = builtin().into_iter().map(|p| p.name).collect();
        for u in &self.user {
            if !v.contains(&u.name) {
                v.push(u.name.clone());
            }
        }
        v
    }

    pub fn set(&mut self, p: HatchPattern) -> bool {
        if !p.check() {
            return false;
        }
        match self.user.iter_mut().find(|u| u.name == p.name) {
            Some(u) => *u = p,
            None => self.user.push(p),
        }
        true
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.user.len();
        self.user.retain(|p| p.name != name);
        self.user.len() != n
    }

    pub fn to_json(&self) -> J {
        J::Array(self.user.iter().map(HatchPattern::to_json).collect())
    }

    pub fn from_json(j: &J) -> Self {
        Self {
            user: j
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(HatchPattern::from_json)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<P> {
        vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
    }

    fn lib(name: &str) -> HatchPattern {
        HatchLib::default().get(name).unwrap()
    }

    fn total_len(v: &[(P, P)]) -> f64 {
        v.iter().map(|s| (s.1 .0 - s.0 .0).hypot(s.1 .1 - s.0 .1)).sum()
    }

    #[test]
    fn builtin_hatches_are_valid() {
        for p in builtin() {
            assert!(p.check(), "{}", p.name);
        }
    }

    #[test]
    fn horizontal_lines_fill_a_square() {
        let p = HatchPattern {
            name: "h".into(),
            model: true,
            families: vec![HatchFamily {
                angle: 0.0,
                origin: (0.0, 0.0),
                shift: 0.0,
                spacing: 0.25,
                dashes: vec![],
            }],
        };
        let s = p.strokes(&[square(0.0, 0.0, 1.0)], 100.0).unwrap();
        // t = 0.25, 0.5, 0.75 (1.0 is on the top edge: half-open, not drawn twice) and 0.0 on the base
        assert!(s.len() >= 4 && s.len() <= 5, "{}", s.len());
        assert!(s.iter().all(|seg| (seg.0 .1 - seg.1 .1).abs() < 1e-9));
        assert!(s.iter().all(|seg| (seg.1 .0 - seg.0 .0 - 1.0).abs() < 1e-9));
    }

    #[test]
    fn hole_is_left_empty() {
        let p = HatchPattern {
            name: "h".into(),
            model: true,
            families: vec![HatchFamily {
                angle: 0.0,
                origin: (0.0, 0.01),
                shift: 0.0,
                spacing: 0.5,
                dashes: vec![],
            }],
        };
        let solid = p.strokes(&[square(0.0, 0.0, 4.0)], 1.0).unwrap();
        let holed = p
            .strokes(&[square(0.0, 0.0, 4.0), square(1.0, 1.0, 2.0)], 1.0)
            .unwrap();
        // the hole is 2 m wide and crossed by 4 lines
        assert!((total_len(&solid) - total_len(&holed) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn drafting_pattern_scales_with_the_view() {
        let p = lib("horizontal");
        let a = p.strokes(&[square(0.0, 0.0, 10.0)], 100.0).unwrap();
        let b = p.strokes(&[square(0.0, 0.0, 10.0)], 200.0).unwrap();
        // 3 mm on paper is 0.3 m at 1:100 and 0.6 m at 1:200: twice the spacing, half the lines
        assert!(a.len() > b.len() * 3 / 2, "{} {}", a.len(), b.len());
        let m = HatchPattern { model: true, ..p };
        let c = m.strokes(&[square(0.0, 0.0, 10.0)], 100.0).unwrap();
        let d = m.strokes(&[square(0.0, 0.0, 10.0)], 500.0).unwrap();
        assert_eq!(c.len(), d.len());
    }

    #[test]
    fn dashes_and_shift_make_a_running_pattern() {
        let p = HatchPattern {
            name: "b".into(),
            model: true,
            families: vec![HatchFamily {
                angle: 0.0,
                origin: (0.0, 0.0),
                shift: 0.5,
                spacing: 1.0,
                dashes: vec![0.5, 0.5],
            }],
        };
        let s = p.strokes(&[square(0.0, 0.0, 2.0)], 1.0).unwrap();
        let row = |t: f64| -> Vec<f64> {
            let mut v: Vec<f64> = s
                .iter()
                .filter(|g| (g.0 .1 - t).abs() < 1e-9)
                .map(|g| g.0 .0)
                .collect();
            v.sort_by(f64::total_cmp);
            v
        };
        assert_eq!(row(1.0), vec![0.5, 1.5], "row 1 is shifted by half a dash");
        assert_eq!(row(2.0), vec![0.0, 1.0], "row 2 is shifted by a full dash cycle");
    }

    #[test]
    fn dense_pattern_is_refused_not_hung() {
        let p = HatchPattern {
            name: "d".into(),
            model: true,
            families: vec![HatchFamily {
                angle: 30.0,
                origin: (0.0, 0.0),
                shift: 0.0,
                spacing: 0.001,
                dashes: vec![],
            }],
        };
        assert_eq!(
            p.strokes(&[square(0.0, 0.0, 1000.0)], 100.0),
            Err(HatchError::TooDense)
        );
    }

    #[test]
    fn bad_numbers_are_invalid() {
        let mut p = lib("diagonal");
        p.families[0].spacing = 0.0;
        assert_eq!(p.strokes(&[square(0.0, 0.0, 1.0)], 100.0), Err(HatchError::Invalid));
        let q = lib("diagonal");
        assert_eq!(q.strokes(&[square(0.0, 0.0, 1.0)], 0.0), Err(HatchError::Invalid));
        assert_eq!(q.strokes(&[], 100.0), Ok(vec![]));
        let mut n = lib("dashed_diagonal");
        n.families[0].dashes = vec![-1.0, 1.0];
        assert!(!n.check());
    }

    #[test]
    fn diagonal_segments_stay_inside() {
        let p = lib("cross_diagonal");
        let s = p.strokes(&[square(0.0, 0.0, 2.0)], 50.0).unwrap();
        assert!(!s.is_empty());
        for seg in s {
            for q in [seg.0, seg.1] {
                assert!(q.0 >= -1e-9 && q.0 <= 2.0 + 1e-9 && q.1 >= -1e-9 && q.1 <= 2.0 + 1e-9);
            }
        }
    }

    #[test]
    fn library_round_trip() {
        let mut l = HatchLib::default();
        let mut mine = lib("cross");
        mine.name = "mine".into();
        mine.families[0].shift = 0.4;
        assert!(l.set(mine.clone()));
        assert_eq!(HatchLib::from_json(&l.to_json()), l);
        assert_eq!(l.get("mine"), Some(mine));
        assert!(l.names().contains(&"mine".to_string()));
        let mut bad = lib("cross");
        bad.families.clear();
        assert!(!l.set(bad));
        assert!(l.remove("mine") && l.get("mine").is_none());
    }
}
