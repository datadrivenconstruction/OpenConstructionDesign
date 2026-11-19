// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rebar in section views: a bar crossing the section plane is a filled circle, and the bars of
//! those sets that run through the view band are lines. Elevations show no rebar: it sits inside
//! the concrete.

use std::collections::BTreeSet;

use ddc_geom::{v2, V2};

use crate::views::{LineClass, ViewPrim};
use crate::{Error, Model};

const RING_SIDES: usize = 12;
const EPS: f64 = 1e-9;
const MATERIAL: &str = "Steel";

/// Parameter interval of the segment whose depth stays inside [0, far].
fn band(dp: f64, dq: f64, far: f64) -> Option<(f64, f64)> {
    let dd = dq - dp;
    if dd.abs() < EPS {
        return (dp >= 0.0 && dp <= far).then_some((0.0, 1.0));
    }
    let t0 = -dp / dd;
    let t1 = if far.is_finite() {
        (far - dp) / dd
    } else if dd > 0.0 {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    };
    let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
    (hi - lo > EPS).then_some((lo, hi))
}

fn circle(c: V2, r: f64) -> Vec<(f64, f64)> {
    (0..RING_SIDES)
        .map(|k| {
            let a = k as f64 * std::f64::consts::TAU / RING_SIDES as f64;
            (c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

impl Model {
    /// Section primitives of the rebar and their points for the view extent. `o`, `u` is the view
    /// line, `far` the view depth.
    pub(crate) fn rebar_section_prims(
        &self,
        o: V2,
        u: V2,
        far: Option<f64>,
        is_cut: bool,
    ) -> Result<(Vec<ViewPrim>, Vec<V2>), Error> {
        if !is_cut {
            return Ok((vec![], vec![]));
        }
        // a rebar layout that fails to build must not take the whole section view with it
        let Ok(built) = self.rebar_built() else {
            return Ok((vec![], vec![]));
        };
        if built.bars.is_empty() {
            return Ok((vec![], vec![]));
        }
        let n = v2(-u.y, u.x);
        let far = far.unwrap_or(f64::INFINITY);
        let mut dots: Vec<(i64, V2, f64)> = vec![];
        let mut lines: Vec<(i64, V2, V2)> = vec![];
        let mut hit = BTreeSet::new();
        for b in &built.bars {
            let pts = b.bar.points(b.d);
            for w in pts.windows(2) {
                let (p, q) = (w[0], w[1]);
                let (rp, rq) = (v2(p.x, p.y) - o, v2(q.x, q.y) - o);
                let (sp, dp) = (rp.dot(u), rp.dot(n));
                let (sq, dq) = (rq.dot(u), rq.dot(n));
                if (dp < 0.0 && dq >= 0.0) || (dp > 0.0 && dq <= 0.0) {
                    let t = -dp / (dq - dp);
                    dots.push((b.set, v2(sp + (sq - sp) * t, p.z + (q.z - p.z) * t), b.d / 2.0));
                    hit.insert(b.set);
                }
                if let Some((t0, t1)) = band(dp, dq, far) {
                    let at = |t: f64| v2(sp + (sq - sp) * t, p.z + (q.z - p.z) * t);
                    lines.push((b.set, at(t0), at(t1)));
                }
            }
        }
        let mut prims = vec![];
        let mut pts = vec![];
        for (set, c, r) in dots {
            pts.push(c);
            prims.push(ViewPrim::SectionCut {
                table: "rebar_set",
                element: set,
                material: MATERIAL.to_string(),
                rings: circle(c, r),
                holes: vec![],
            });
        }
        // a set the plane misses is hidden behind the concrete: no lines for it
        for (set, a, b) in lines.into_iter().filter(|l| hit.contains(&l.0)) {
            pts.push(a);
            pts.push(b);
            prims.push(ViewPrim::Edge {
                table: "rebar_set",
                element: set,
                class: LineClass::Projection,
                a: (a.x, a.y),
                b: (b.x, b.y),
            });
        }
        Ok((prims, pts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sections::{SectionSpec, ViewKind};
    use crate::MemberShape as Shape;
    use crate::{BeamJoin, BeamSpec, Just, RebarHook, RebarLayout, RebarPath, RebarSetSpec};

    fn model_with_stirrups() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let t = m
            .add_member_type("RC 300x500", &Shape::Rect { b: 0.3, h: 0.5 }, "Concrete")
            .unwrap();
        let beam = m
            .add_beam(&BeamSpec {
                level: l0,
                member_type: t,
                a: (0.0, 0.0, 3.0),
                b: (6.0, 0.0, 3.0),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .unwrap();
        let ty = m
            .rebar_types()
            .unwrap()
            .into_iter()
            .find(|x| (x.d * 1000.0).round() as i64 == 8)
            .unwrap();
        let ids = m
            .add_rebar_sets(&[RebarSetSpec {
                host_kind: "beam".into(),
                host: beam,
                rtype: ty.id,
                layout: RebarLayout::Stirrup {
                    rule: RebarPath::MaxSpacing(0.2),
                    hook: RebarHook::H135,
                },
                mark: None,
            }])
            .unwrap();
        (m, ids[0])
    }

    fn spec(kind: ViewKind, b: (f64, f64)) -> SectionSpec {
        SectionSpec {
            kind,
            name: "S".into(),
            a: (0.0, 0.0),
            b,
            depth: 1.0,
            far_clip: false,
        }
    }

    #[test]
    fn band_keeps_only_the_visible_part() {
        assert_eq!(band(-1.0, 1.0, f64::INFINITY), Some((0.5, 1.0)));
        assert_eq!(band(1.0, -1.0, f64::INFINITY), Some((0.0, 0.5)));
        assert_eq!(band(-2.0, -1.0, 5.0), None);
        assert_eq!(band(0.5, 0.5, 1.0), Some((0.0, 1.0)));
        assert_eq!(band(0.5, 3.0, 1.0), Some((0.0, 0.2)));
    }

    #[test]
    fn a_section_along_the_beam_cuts_every_stirrup() {
        let (m, set) = model_with_stirrups();
        let v = m.section_view(&spec(ViewKind::Section, (6.0, 0.0)), &[]).unwrap();
        let cuts = v.count(|p| {
            matches!(p, ViewPrim::SectionCut { table: "rebar_set", element, .. } if *element == set)
        });
        // 31 stirrups, the top and the bottom leg of each cross the plane
        assert!(cuts >= 62, "{cuts} bar cuts");
        assert!(v.count(|p| matches!(p, ViewPrim::Edge { table: "rebar_set", .. })) > 0);
    }

    #[test]
    fn an_elevation_and_a_miss_show_no_rebar() {
        let (m, _) = model_with_stirrups();
        let v = m.section_view(&spec(ViewKind::Elevation, (6.0, 0.0)), &[]).unwrap();
        assert_eq!(v.count(|p| matches!(p, ViewPrim::SectionCut { table: "rebar_set", .. })), 0);
        // a plane 5 m beside the beam crosses no bar
        let mut s = spec(ViewKind::Section, (6.0, 5.0));
        s.a = (0.0, 5.0);
        let v = m.section_view(&s, &[]).unwrap();
        assert_eq!(v.count(|p| matches!(p, ViewPrim::Edge { table: "rebar_set", .. })), 0);
    }
}
