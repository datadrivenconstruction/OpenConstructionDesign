// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rule checks of a built connection against the catalogue: bolt series and hole sizes, edge
//! and end distances, pitches, plate thickness, and clashes of bolts with each other, with the
//! joined members and with plates they do not pass through.

use super::bar::{seg_dist, Bar};
use super::catalog::{catalog, fmt_mm};
use super::part::{BoltInfo, PartQty, Role};
use super::Conn;
use crate::math::{v2, v3, V2, V3};
use crate::member::Member;
use crate::poly;

/// Rule kinds, stable keys for messages and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    BoltSeries,
    Hole,
    Edge,
    Pitch,
    /// Spacing over the maximum (a warning: plies may open up).
    SpacingMax,
    PlateThin,
    Clash,
}

impl Rule {
    pub fn key(self) -> &'static str {
        match self {
            Rule::BoltSeries => "bolt_series",
            Rule::Hole => "hole",
            Rule::Edge => "edge",
            Rule::Pitch => "pitch",
            Rule::SpacingMax => "spacing_max",
            Rule::PlateThin => "plate_thin",
            Rule::Clash => "clash",
        }
    }

    /// Errors make the detail wrong; warnings ask for a look.
    pub fn is_error(self) -> bool {
        !matches!(self, Rule::SpacingMax)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: Rule,
    /// Part index in the connection.
    pub part: usize,
    pub text: String,
}

/// All findings; `members` are the joined members the connection was built on.
pub fn check(c: &Conn, members: &[Member]) -> Vec<Finding> {
    let mut out = vec![];
    bolt_rules(c, &mut out);
    plate_rules(c, &mut out);
    // members as the detail leaves them: a trimmed end no longer holds what sits past it
    let mut bars: Vec<Option<Bar>> = members.iter().map(|m| Bar::new(m).ok()).collect();
    for t in &c.trims {
        if let Some(Some(b)) = bars.get_mut(t.member) {
            b.cut_back(c.node, t.end, t.setback);
        }
    }
    let bars: Vec<Bar> = bars.into_iter().flatten().collect();
    clashes(c, &bars, &mut out);
    out
}

/// Number of error findings.
pub fn errors(f: &[Finding]) -> usize {
    f.iter().filter(|x| x.rule.is_error()).count()
}

fn bolt_rules(c: &Conn, out: &mut Vec<Finding>) {
    let cat = catalog();
    for (i, b) in &c.bolts {
        let Some(size) = cat.bolt(b.size.d) else {
            out.push(Finding {
                rule: Rule::BoltSeries,
                part: *i,
                text: format!("M{} is not in the bolt series", fmt_mm(b.size.d)),
            });
            continue;
        };
        let want = if b.anchor {
            size.anchor_hole
        } else {
            size.hole
        };
        if (b.hole - want).abs() > 1e-5 {
            out.push(Finding {
                rule: Rule::Hole,
                part: *i,
                text: format!(
                    "{} hole {} mm, the catalogue gives {} mm",
                    size.name(),
                    fmt_mm(b.hole),
                    fmt_mm(want)
                ),
            });
        }
    }
}

/// Distance from `p` to the outline's edges.
fn edge_dist(outline: &[V2], p: V2) -> f64 {
    let n = outline.len();
    (0..n)
        .map(|i| {
            let (a, b) = (outline[i], outline[(i + 1) % n]);
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.dot(ab).max(1e-18)).clamp(0.0, 1.0);
            (a + ab * t - p).len()
        })
        .fold(f64::MAX, f64::min)
}

fn plate_rules(c: &Conn, out: &mut Vec<Finding>) {
    let cat = catalog();
    let sp = &cat.spacing;
    for (i, part) in c.parts.iter().enumerate() {
        let PartQty::Plate { t, outline, holes } = &part.qty else {
            continue;
        };
        if part.role != Role::Packer && *t < cat.plate.min_t - 1e-9 {
            out.push(Finding {
                rule: Rule::PlateThin,
                part: i,
                text: format!(
                    "{} is thinner than {} mm",
                    part.mark,
                    fmt_mm(cat.plate.min_t)
                ),
            });
        }
        let anchors = c
            .bolts
            .iter()
            .any(|(_, b)| b.anchor && b.plies.contains(&i));
        for (k, (h, d0)) in holes.iter().enumerate() {
            if !poly::contains(&[outline.clone()], *h) {
                out.push(Finding {
                    rule: Rule::Edge,
                    part: i,
                    text: format!("{}: a hole lies outside the plate", part.mark),
                });
                continue;
            }
            let e = edge_dist(outline, *h);
            let e_min = sp.e1_min.min(sp.e2_min) * d0;
            if e < e_min - 1e-6 {
                out.push(Finding {
                    rule: Rule::Edge,
                    part: i,
                    text: format!(
                        "{}: edge distance {} mm under {} mm",
                        part.mark,
                        fmt_mm(e),
                        fmt_mm(e_min)
                    ),
                });
            }
            if !anchors && e > cat.e_max(*t) + 1e-6 && part.role != Role::Gusset {
                out.push(Finding {
                    rule: Rule::SpacingMax,
                    part: i,
                    text: format!(
                        "{}: edge distance {} mm over the maximum",
                        part.mark,
                        fmt_mm(e)
                    ),
                });
            }
            let mut nearest = f64::MAX;
            for (j, (g, d1)) in holes.iter().enumerate() {
                if j == k {
                    continue;
                }
                let p = (*g - *h).len();
                nearest = nearest.min(p);
                // holes set apart mainly across the plate (x) are a gauge, along it a pitch
                let gauge = (g.x - h.x).abs() > (g.y - h.y).abs();
                let min = if gauge { sp.p2_min } else { sp.p1_min } * d0.max(*d1);
                if j > k && p < min - 1e-6 {
                    out.push(Finding {
                        rule: Rule::Pitch,
                        part: i,
                        text: format!(
                            "{}: holes {} mm apart, under {} mm",
                            part.mark,
                            fmt_mm(p),
                            fmt_mm(min)
                        ),
                    });
                }
            }
            if !anchors && nearest < f64::MAX && nearest > cat.p_max(*t) + 1e-6 {
                out.push(Finding {
                    rule: Rule::SpacingMax,
                    part: i,
                    text: format!(
                        "{}: pitch {} mm over the maximum",
                        part.mark,
                        fmt_mm(nearest)
                    ),
                });
            }
        }
    }
}

/// Head and nut sample points of a bolt: rims of its end envelopes.
fn samples(b: &BoltInfo) -> Vec<V3> {
    let env = b.envelopes();
    let z = b.dir.normalized();
    let x = if z.x.abs() < 0.9 {
        v3(1.0, 0.0, 0.0)
    } else {
        v3(0.0, 1.0, 0.0)
    };
    let x = (x - z * z.dot(x)).normalized();
    let y = z.cross(x);
    let mut out = vec![];
    for (k, (s0, s1, r)) in env.iter().enumerate() {
        if k == 1 {
            continue;
        }
        for s in [*s0, (*s0 + *s1) / 2.0, *s1] {
            out.push(b.o + z * s);
            for i in 0..8 {
                let a = std::f64::consts::PI * i as f64 / 4.0;
                out.push(b.o + z * s + (x * a.cos() + y * a.sin()) * (*r * 0.98));
            }
        }
    }
    out
}

/// Point strictly inside a plate part (frame prism with holes).
fn in_plate(c: &Conn, i: usize, p: V3, tol: f64) -> bool {
    let part = &c.parts[i];
    let PartQty::Plate { t, outline, holes } = &part.qty else {
        return false;
    };
    let crate::gsolid::Form::Extrusion { start, .. } = &part.form else {
        return false;
    };
    let q = part.frame.local(p);
    if q.z < start + tol || q.z > start + t - tol {
        return false;
    }
    let q2 = v2(q.x, q.y);
    if !poly::contains(&[outline.clone()], q2) || edge_dist(outline, q2) < tol {
        return false;
    }
    !holes.iter().any(|(h, d)| (*h - q2).len() < d / 2.0 + tol)
}

fn clashes(c: &Conn, bars: &[Bar], out: &mut Vec<Finding>) {
    let tol = 2e-4;
    // bolt against bolt: capsules of head, shank and nut
    for (a, (ia, ba)) in c.bolts.iter().enumerate() {
        for (ib, bb) in c.bolts.iter().skip(a + 1).map(|(i, b)| (*i, b)) {
            let hit = ba.envelopes().iter().any(|ea| {
                bb.envelopes().iter().any(|eb| {
                    let d = seg_dist(
                        ba.o + ba.dir * ea.0,
                        ba.o + ba.dir * ea.1,
                        bb.o + bb.dir * eb.0,
                        bb.o + bb.dir * eb.1,
                    );
                    d < ea.2 + eb.2 - tol
                })
            });
            if hit {
                out.push(Finding {
                    rule: Rule::Clash,
                    part: *ia,
                    text: format!("{} and {} bolts clash", ba.size.name(), bb.size.name())
                        + &format!(" (parts {} and {})", ia, ib),
                });
            }
        }
    }
    // heads and nuts against members and against plates the bolt does not pass through
    let plates: Vec<usize> = c
        .parts
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.qty, PartQty::Plate { .. }))
        .map(|(i, _)| i)
        .collect();
    for (i, b) in &c.bolts {
        let pts = samples(b);
        if let Some(m) = bars
            .iter()
            .position(|bar| pts.iter().any(|p| bar.holds(*p, tol)))
        {
            out.push(Finding {
                rule: Rule::Clash,
                part: *i,
                text: format!("a {} head or nut is inside member {}", b.size.name(), m + 1),
            });
        }
        if let Some(pl) = plates
            .iter()
            .find(|pl| !b.plies.contains(pl) && pts.iter().any(|p| in_plate(c, **pl, *p, tol)))
        {
            out.push(Finding {
                rule: Rule::Clash,
                part: *i,
                text: format!(
                    "a {} head or nut is inside {}",
                    b.size.name(),
                    c.parts[*pl].mark
                ),
            });
        }
    }
}
