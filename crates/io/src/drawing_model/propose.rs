// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Underlay proposals: walls (ddc-geom recognition + noise filters), openings (wall gaps),
//! rooms (labels inside closed regions). Everything is deterministic, with no writes to the model.

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::PI;

use ddc_geom::layered::{join, layer_footprints, Strip};
use ddc_geom::recognize::{recognize_walls, IssueKind, Seg};
use ddc_geom::rooms::{arrange, interior_point};
use ddc_geom::{v2, Tol, V2};
use ddc_model::Model;

use crate::num_file::{detect_mark, numeric_like, parse_num, Mark};

use super::source::{UArc, UText, Underlay};
use super::{
    hash_key, q, Ambiguity, Memory, OpeningCand, Proposals, ProposeOptions, Report, RoomCand,
    WallCand, WallTypeInfo,
};

/// Result of the first stage; holds intermediate data for openings and rooms.
#[derive(Clone, Debug)]
pub struct WallProposal {
    pub walls: Vec<WallCand>,
    pub report: Report,
    /// All walls after filters (including remembered ones): rooms are built from them.
    strips: Vec<Strip>,
    /// Index of the candidate wall for each wall in `strips`.
    cand_of: Vec<Option<usize>>,
    gaps: Vec<Gap>,
    lines: Vec<(V2, V2)>,
    door_arcs: Vec<UArc>,
}

#[derive(Clone, Debug)]
struct Gap {
    strip: usize,
    t0: f64,
    t1: f64,
    /// Number of thin "walls" (glass) inside a gap.
    glass: usize,
}

struct Rect {
    a: V2,
    d: V2,
    n: V2,
    len: f64,
    hw: f64,
    lo: V2,
    hi: V2,
}

impl Rect {
    fn new(s: &Strip, pad: f64) -> Rect {
        let (d, n) = (s.dir(), s.normal());
        let hw: f64 = s.layers.iter().map(|l| l.thickness).sum::<f64>() / 2.0;
        let e = hw + pad;
        let (b, a) = (s.b, s.a);
        Rect {
            a,
            d,
            n,
            len: s.length(),
            hw,
            lo: v2(a.x.min(b.x) - e, a.y.min(b.y) - e),
            hi: v2(a.x.max(b.x) + e, a.y.max(b.y) + e),
        }
    }
    fn has(&self, p: V2, tol: f64) -> bool {
        if p.x < self.lo.x || p.x > self.hi.x || p.y < self.lo.y || p.y > self.hi.y {
            return false;
        }
        let rel = p - self.a;
        let (t, lat) = (rel.dot(self.d), rel.dot(self.n));
        // End caps: the neighbouring wall outline protrudes half a thickness past the axis.
        let end = self.hw + tol;
        t >= -end && t <= self.len + end && lat.abs() <= self.hw + tol
    }
}

/// A layer is skipped if it contains a word from the list. Element-category words (columns, axes,
/// rooms, furniture) do not apply to a layer that is also named for walls (e.g. "Walls and columns").
fn ignored_layers(names: impl Iterator<Item = String>, words: &[String]) -> HashMap<String, bool> {
    const ELEMENT: [&str; 14] = [
        "column",
        "colu",
        "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}",
        "grid",
        "axis",
        "\u{43e}\u{441}\u{44c}",
        "\u{43e}\u{441}\u{438}",
        "\u{441}\u{435}\u{442}\u{43a}\u{430}",
        "area",
        "room",
        "\u{43f}\u{43e}\u{43c}\u{435}\u{449}",
        "\u{43f}\u{43b}\u{43e}\u{449}\u{430}\u{434}",
        "furn",
        "\u{43c}\u{435}\u{431}\u{435}\u{43b}",
    ];
    let w: Vec<String> = words.iter().map(|s| s.to_lowercase()).collect();
    names
        .map(|n| {
            let l = n.to_lowercase();
            let walls = super::extra::has_word(&l, &super::extra::WALL_WORDS);
            let hit = w
                .iter()
                .any(|x| l.contains(x.as_str()) && !(walls && ELEMENT.contains(&x.as_str())));
            (n, hit)
        })
        .collect()
}

fn lex_less(a: V2, b: V2) -> bool {
    (q(b.x), q(b.y), b.x, b.y) < (q(a.x), q(a.y), a.x, a.y)
}

/// The type with the nearest thickness; the second result is ambiguity. Of equally near types
/// the newest wins: a type made for the project, not the default seeded before it.
fn match_type(types: &[WallTypeInfo], w: f64, tol: f64) -> (Option<i64>, Option<f64>, bool) {
    let mut d: Vec<(f64, i64)> = types
        .iter()
        .map(|t| ((t.thickness - w).abs(), t.id))
        .collect();
    d.sort_by(|x, y| x.0.total_cmp(&y.0).then(y.1.cmp(&x.1)));
    match d.as_slice() {
        [] => (None, None, false),
        [b] => (Some(b.1), Some(b.0), b.0 > tol),
        [b, s, ..] => (Some(b.1), Some(b.0), b.0 > tol || s.0 - b.0 < 0.005),
    }
}

/// Hatch lines: 5+ parallel lines with equal spacing.
fn hatch_mask(lines: &[(V2, V2)], opt: &ProposeOptions) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let min = opt.hatch_min_lines;
    if min < 2 {
        return mask;
    }
    let mut keyed: Vec<(f64, usize)> = vec![];
    for (i, (a, b)) in lines.iter().enumerate() {
        let v = *b - *a;
        if v.len() < 1e-9 {
            continue;
        }
        let mut th = v.y.atan2(v.x);
        if th < 0.0 {
            th += PI;
        }
        if th >= PI - 1e-3 {
            th -= PI;
        }
        keyed.push((th, i));
    }
    keyed.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    let mut s = 0;
    while s < keyed.len() {
        let mut e = s + 1;
        while e < keyed.len() && keyed[e].0 - keyed[e - 1].0 <= 1e-3 {
            e += 1;
        }
        if e - s >= min {
            let (a, b) = lines[keyed[s].1];
            let u = (b - a) * (1.0 / (b - a).len());
            let n = v2(-u.y, u.x);
            let mut offs: Vec<(f64, usize)> = keyed[s..e]
                .iter()
                .map(|&(_, i)| (((lines[i].0 + lines[i].1) * 0.5).dot(n), i))
                .collect();
            offs.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
            mark_runs(&offs, opt, &mut mask);
        }
        s = e;
    }
    mask
}

fn mark_runs(offs: &[(f64, usize)], opt: &ProposeOptions, mask: &mut [bool]) {
    let m = offs.len();
    let (mut s, mut e, mut r) = (0usize, 0usize, 0.0f64);
    for i in 1..=m {
        let step = if i < m {
            offs[i].0 - offs[i - 1].0
        } else {
            f64::NAN
        };
        let ok = i < m
            && step >= 0.01
            && step <= opt.hatch_max_step
            && (e == s || (step - r).abs() <= 0.1 * r);
        if ok {
            if e == s {
                r = step;
            }
            e = i;
        } else {
            if e - s + 1 >= opt.hatch_min_lines {
                for o in &offs[s..=e] {
                    mask[o.1] = true;
                }
            }
            s = i;
            e = i;
        }
    }
}

/// Closed shapes (all line ends connected) entirely within a `max` m square are furniture, columns,
/// symbols, not walls: walls form a large network or have free ends. A shape that a line from
/// outside it crosses is no free symbol either: a wall piece drawn as its own rectangle runs into
/// the wall it meets (one touching a face, as a cabinet does, stays a symbol).
pub(super) fn closed_symbols(lines: &[(V2, V2)], max: f64) -> Vec<bool> {
    let n = lines.len();
    let key = |p: V2| ((p.x * 1000.0).round() as i64, (p.y * 1000.0).round() as i64);
    let mut ids: HashMap<(i64, i64), usize> = HashMap::new();
    let mut parent: Vec<usize> = vec![];
    let mut deg: Vec<u32> = vec![];
    let mut ends = Vec::with_capacity(n);
    for (a, b) in lines {
        let mut e = [0usize; 2];
        for (k, p) in [*a, *b].into_iter().enumerate() {
            let next = parent.len();
            let id = *ids.entry(key(p)).or_insert(next);
            if id == next {
                parent.push(next);
                deg.push(0);
            }
            deg[id] += 1;
            e[k] = id;
        }
        ends.push(e);
    }
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for e in &ends {
        let (a, b) = (find(&mut parent, e[0]), find(&mut parent, e[1]));
        parent[a.max(b)] = a.min(b);
    }
    // Root -> (is closed, bbox).
    let mut comp: HashMap<usize, (bool, [f64; 4])> = HashMap::new();
    for (i, (a, b)) in lines.iter().enumerate() {
        let r = find(&mut parent, ends[i][0]);
        let c = comp
            .entry(r)
            .or_insert((true, [f64::MAX, f64::MAX, f64::MIN, f64::MIN]));
        c.0 &= deg[ends[i][0]] >= 2 && deg[ends[i][1]] >= 2;
        for p in [a, b] {
            c.1 = [
                c.1[0].min(p.x),
                c.1[1].min(p.y),
                c.1[2].max(p.x),
                c.1[3].max(p.y),
            ];
        }
    }
    let root: Vec<usize> = (0..n).map(|i| find(&mut parent, ends[i][0])).collect();
    let small = |r: usize| {
        let (closed, bb) = comp[&r];
        closed && bb[2] - bb[0] <= max && bb[3] - bb[1] <= max
    };
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        if small(root[i]) {
            members.entry(root[i]).or_default().push(i);
        }
    }
    let mut crossed: HashMap<usize, bool> = HashMap::new();
    for (&r, own) in &members {
        let bb = comp[&r].1;
        let hit = (0..n).any(|j| {
            let (c, d) = lines[j];
            if root[j] == r || c.x.max(d.x) < bb[0] || c.x.min(d.x) > bb[2] || c.y.max(d.y) < bb[1] || c.y.min(d.y) > bb[3] {
                return false;
            }
            own.iter().any(|&k| crosses(lines[k], (c, d)))
        });
        crossed.insert(r, hit);
    }
    (0..n).map(|i| small(root[i]) && !crossed[&root[i]]).collect()
}

/// The segments cross at a point inside both (not at an end, not along a shared line).
fn crosses((a, b): (V2, V2), (c, d): (V2, V2)) -> bool {
    let (u, v) = (b - a, d - c);
    let eps = 1e-9 * (u.len() * v.len()).max(1e-12);
    let (o1, o2) = (u.cross(c - a), u.cross(d - a));
    let (o3, o4) = (v.cross(a - c), v.cross(b - c));
    ((o1 > eps && o2 < -eps) || (o1 < -eps && o2 > eps)) && ((o3 > eps && o4 < -eps) || (o3 < -eps && o4 > eps))
}

fn is_dim_number(t: &str) -> bool {
    let t = t.trim();
    t.chars().filter(|c| c.is_ascii_digit()).count() >= 2
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '+' | '-'))
}

/// A line pair is a dimension chain if a numeric label is nearby or it carries at least two
/// short oblique ticks (dimension arrows).
fn is_dimension_pair(s: &Strip, lines: &[(V2, V2)], texts: &[UText]) -> bool {
    let (dir, nrm, len) = (s.dir(), s.normal(), s.length());
    let hw = s.layers.iter().map(|l| l.thickness).sum::<f64>() * 0.5;
    let local = |p: V2| {
        let r = p - s.a;
        (r.dot(dir), r.dot(nrm))
    };
    for t in texts {
        if !is_dim_number(&t.text) {
            continue;
        }
        let (u, v) = local(v2(t.pos.0, t.pos.1));
        if u >= -0.1 && u <= len + 0.1 && v.abs() <= hw + 2.0 * t.height + 0.1 {
            return true;
        }
    }
    let ticks = lines
        .iter()
        .filter(|(a, b)| {
            let d = *b - *a;
            let l = d.len();
            if !(1e-9..=0.4).contains(&l) {
                return false;
            }
            let c = (d.dot(dir) / l).abs();
            if !(0.4..=0.92).contains(&c) {
                return false;
            }
            let (u0, v0) = local(*a);
            let (u1, v1) = local(*b);
            let band = hw + 0.15;
            v0.abs() <= band && v1.abs() <= band && u0.min(u1) >= -0.1 && u0.max(u1) <= len + 0.1
        })
        .count();
    ticks >= 2
}

fn arc_ends(a: &UArc) -> (V2, V2) {
    (
        v2(a.c.0 + a.r * a.a0.cos(), a.c.1 + a.r * a.a0.sin()),
        v2(a.c.0 + a.r * a.a1.cos(), a.c.1 + a.r * a.a1.sin()),
    )
}

/// Model wall types with full thickness, in ascending id order.
pub fn wall_types(model: &Model) -> Result<Vec<WallTypeInfo>, ddc_model::Error> {
    let mut st = model.sql().prepare(
        "SELECT t.id, t.name, COALESCE(SUM(l.thickness),0) FROM wall_type t \
         LEFT JOIN wall_type_layer l ON l.wtype=t.id GROUP BY t.id ORDER BY t.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(WallTypeInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            thickness: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn propose_walls(
    u: &Underlay,
    types: &[WallTypeInfo],
    opt: &ProposeOptions,
    mem: &Memory,
) -> WallProposal {
    let mut rep = Report::default();
    let skip_geom = ignored_layers(
        u.lines
            .iter()
            .map(|l| l.layer.clone())
            .chain(u.arcs.iter().map(|a| a.layer.clone())),
        &opt.ignore_geom_layers,
    );
    let mut lines: Vec<(V2, V2)> = vec![];
    for l in &u.lines {
        if skip_geom[&l.layer] {
            rep.ignored_layer_lines += 1;
        } else if l.dashed && opt.skip_dashed {
            rep.dashed_lines += 1;
        } else {
            let (a, b) = (v2(l.a.0, l.a.1), v2(l.b.0, l.b.1));
            if (b - a).len() > 1e-9 {
                lines.push((a, b));
            }
        }
    }

    // Door arcs and their leaves.
    let (wmin, wmax) = (opt.door_width.0 * 0.8, opt.door_width.1 * 1.15);
    let door_arcs: Vec<UArc> = u
        .arcs
        .iter()
        .filter(|a| {
            let sw = a.a1 - a.a0;
            !skip_geom[&a.layer]
                && a.r.is_finite()
                && (wmin..=wmax).contains(&a.r)
                && (PI / 3.0..=PI * 2.0 / 3.0 + 0.05).contains(&sw)
        })
        .cloned()
        .collect();
    let mut leaf = vec![false; lines.len()];
    for a in &door_arcs {
        let c = v2(a.c.0, a.c.1);
        let (e0, e1) = arc_ends(a);
        let tol = 0.02 + 0.02 * a.r;
        for (i, (p, r)) in lines.iter().enumerate() {
            if leaf[i] || ((*r - *p).len() - a.r).abs() > tol {
                continue;
            }
            let hit = |x: V2, y: V2| {
                (x - c).len() <= tol && ((y - e0).len() <= tol || (y - e1).len() <= tol)
            };
            if hit(*p, *r) || hit(*r, *p) {
                leaf[i] = true;
            }
        }
    }
    rep.door_leaf_lines = leaf.iter().filter(|&&b| b).count();
    let lines: Vec<(V2, V2)> = lines
        .iter()
        .zip(&leaf)
        .filter(|(_, &l)| !l)
        .map(|(x, _)| *x)
        .collect();
    let hatch = hatch_mask(&lines, opt);
    rep.hatch_lines = hatch.iter().filter(|&&b| b).count();
    let lines: Vec<(V2, V2)> = lines
        .iter()
        .zip(&hatch)
        .filter(|(_, &h)| !h)
        .map(|(x, _)| *x)
        .collect();

    let symbol = closed_symbols(&lines, opt.symbol_max_size);
    rep.symbol_lines = symbol.iter().filter(|&&b| b).count();
    let lines: Vec<(V2, V2)> = lines
        .iter()
        .zip(&symbol)
        .filter(|(_, &b)| !b)
        .map(|(x, _)| *x)
        .collect();

    let segs: Vec<Seg> = lines.iter().map(|&(a, b)| Seg { a, b }).collect();
    let rec = recognize_walls(&segs, &opt.walls);
    let mut strips: Vec<Strip> = rec
        .strips
        .into_iter()
        .filter(|s| {
            [s.a.x, s.a.y, s.b.x, s.b.y].iter().all(|v| v.is_finite()) && (s.b - s.a).len() > 0.0
        })
        .collect();
    let before = strips.len();
    strips.retain(|s| !is_dimension_pair(s, &lines, &u.texts));
    rep.dimension_pairs = before - strips.len();
    for s in &mut strips {
        if lex_less(s.a, s.b) {
            std::mem::swap(&mut s.a, &mut s.b);
        }
    }

    // Gaps (opening candidates).
    let mut gaps: Vec<Gap> = vec![];
    for is in &rec.issues {
        let IssueKind::Opening { width } = is.kind else {
            continue;
        };
        // window glass on the wall axis matches as well as the wall: the thicker strip hosts
        let mut best: Option<(f64, f64, usize, f64)> = None;
        for (i, s) in strips.iter().enumerate() {
            let rel = is.at - s.a;
            let (t, lat) = (rel.dot(s.dir()), rel.dot(s.normal()).abs());
            let w: f64 = s.layers.iter().map(|l| l.thickness).sum();
            let better = |b: &(f64, f64, usize, f64)| {
                lat < b.0 - 1e-6 || (lat <= b.0 + 1e-6 && w > b.1 + 1e-9)
            };
            if t >= -0.02
                && t <= s.length() + 0.02
                && lat <= 0.02
                && best.is_none_or(|b| better(&b))
            {
                best = Some((lat, w, i, t));
            }
        }
        if let Some((_, _, i, t)) = best {
            gaps.push(Gap {
                strip: i,
                t0: t - width / 2.0,
                t1: t + width / 2.0,
                glass: 0,
            });
        }
    }
    // Thin "walls" inside a neighbouring wall's gap are window glass, not a wall.
    let rects: Vec<Rect> = strips.iter().map(|s| Rect::new(s, 0.0)).collect();
    let mut remove = vec![false; strips.len()];
    for g in &mut gaps {
        let host = &rects[g.strip];
        for (j, s) in strips.iter().enumerate() {
            if j == g.strip || remove[j] || host.d.cross(s.dir()).abs() > 0.02 {
                continue;
            }
            let inside = |p: V2| {
                let rel = p - host.a;
                let (t, lat) = (rel.dot(host.d), rel.dot(host.n));
                t >= g.t0 - 0.03 && t <= g.t1 + 0.03 && lat.abs() <= host.hw + 0.03
            };
            if inside(s.a) && inside(s.b) {
                remove[j] = true;
                g.glass += 2;
            }
        }
    }
    let mut remap = vec![usize::MAX; strips.len()];
    let mut kept = vec![];
    for (i, s) in strips.into_iter().enumerate() {
        if !remove[i] {
            remap[i] = kept.len();
            kept.push(s);
        }
    }
    let strips = kept;
    let gaps: Vec<Gap> = gaps
        .into_iter()
        .filter(|g| remap[g.strip] != usize::MAX)
        .map(|g| Gap {
            strip: remap[g.strip],
            ..g
        })
        .collect();

    // Candidate walls.
    let mut walls = vec![];
    let mut cand_of = vec![];
    for s in &strips {
        let w: f64 = s.layers.iter().map(|l| l.thickness).sum();
        let id = hash_key(
            1,
            &[
                q(s.a.x),
                q(s.a.y),
                q(s.b.x),
                q(s.b.y),
                (w * 1000.0).round() as i64,
            ],
            "",
        );
        if mem.contains(id) {
            rep.remembered_skipped += 1;
            cand_of.push(None);
            continue;
        }
        let (ty, delta, amb) = match_type(types, w, opt.type_tol);
        let mut c: f64 = 0.55;
        if s.length() >= 1.0 {
            c += 0.15;
        }
        if (0.1..=0.5).contains(&w) {
            c += 0.1;
        }
        c += if types.is_empty() {
            0.1
        } else if amb {
            0.0
        } else {
            0.2
        };
        if amb {
            rep.ambiguous.push(Ambiguity {
                id,
                reason: "wall type: nearest type differs or two types are equally close",
            });
        } else if c < 0.6 {
            rep.ambiguous.push(Ambiguity {
                id,
                reason: "low confidence wall",
            });
        }
        cand_of.push(Some(walls.len()));
        walls.push(WallCand {
            id,
            a: (s.a.x, s.a.y),
            b: (s.b.x, s.b.y),
            thickness: w,
            height: opt.walls.height,
            confidence: c.min(1.0),
            wall_type: ty,
            type_delta: delta,
        });
    }

    // Lines that joined no wall.
    let rects: Vec<Rect> = strips.iter().map(|s| Rect::new(s, 0.02)).collect();
    for (a, b) in &lines {
        if rects.iter().any(|r| r.has(*a, 0.02) && r.has(*b, 0.02)) {
            continue;
        }
        if (*b - *a).len() < opt.walls.min_length {
            rep.short_lines += 1;
        } else {
            rep.unmatched.push(((a.x, a.y), (b.x, b.y)));
        }
    }
    rep.walls = walls.len();
    WallProposal {
        walls,
        report: rep,
        strips,
        cand_of,
        gaps,
        lines,
        door_arcs,
    }
}

fn openings_impl(
    wp: &WallProposal,
    opt: &ProposeOptions,
    mem: &Memory,
) -> (Vec<OpeningCand>, usize) {
    let mut out = vec![];
    let mut skipped = 0;
    let mut used = vec![false; wp.door_arcs.len()];
    for g in &wp.gaps {
        let Some(ci) = wp.cand_of[g.strip] else {
            continue;
        };
        let s = &wp.strips[g.strip];
        let (d, n, len) = (s.dir(), s.normal(), s.length());
        let w: f64 = s.layers.iter().map(|l| l.thickness).sum();
        let (along, end) = (g.t0.max(0.0), g.t1.min(len));
        let width = end - along;
        if width < 0.05 {
            continue;
        }
        let (p0, p1) = (s.a + d * along, s.a + d * end);

        // Door: an arc centred at the gap edge with a radius near the width.
        let mut best: Option<(f64, usize, bool)> = None;
        for (k, a) in wp.door_arcs.iter().enumerate() {
            if used[k] || (a.r - width).abs() > 0.15 * width + 0.05 {
                continue;
            }
            let c = v2(a.c.0, a.c.1);
            let (d0, d1) = ((c - p0).len(), (c - p1).len());
            let (dist, hinge) = if d1 < d0 { (d1, true) } else { (d0, false) };
            if dist > w / 2.0 + 0.1 {
                continue;
            }
            let score = dist + (a.r - width).abs();
            if best.is_none_or(|b| score < b.0) {
                best = Some((score, k, hinge));
            }
        }
        let (kind, conf, swing) = if let Some((_, k, hinge)) = best {
            used[k] = true;
            let a = &wp.door_arcs[k];
            let m = (a.a0 + a.a1) / 2.0;
            let mid = v2(a.c.0 + a.r * m.cos(), a.c.1 + a.r * m.sin());
            let side = (mid - s.a).dot(n) < 0.0;
            let ok = width >= opt.door_width.0 && width <= opt.door_width.1;
            ("door", if ok { 0.95 } else { 0.75 }, (side, hinge))
        } else {
            // Window: glass lines along the axis inside the gap.
            let glass = wp
                .lines
                .iter()
                .filter(|(a, b)| {
                    let v = *b - *a;
                    let inside = |p: V2| {
                        let rel = p - s.a;
                        let (t, lat) = (rel.dot(d), rel.dot(n));
                        t >= along - 0.03 && t <= end + 0.03 && lat.abs() <= w / 2.0 + 0.03
                    };
                    v.len() >= 0.6 * width
                        && d.cross(v).abs() <= 0.02 * v.len()
                        && inside(*a)
                        && inside(*b)
                })
                .count()
                + g.glass;
            if glass >= 2 {
                ("window", 0.9, (false, false))
            } else if glass == 1 {
                ("window", 0.75, (false, false))
            } else if width >= opt.door_width.0 && width <= opt.door_width.1 {
                ("door", 0.4, (false, false))
            } else {
                ("window", 0.3, (false, false))
            }
        };
        let wid = wp.walls[ci].id;
        let id = hash_key(2, &[wid as i64, q(along), q(width)], kind);
        if mem.contains(id) {
            skipped += 1;
            continue;
        }
        let (sill, height) = if kind == "door" {
            (0.0, opt.door_height)
        } else {
            (opt.window_sill, opt.window_height)
        };
        out.push(OpeningCand {
            id,
            wall: wid,
            kind: kind.into(),
            along,
            width,
            sill,
            height,
            swing_side: swing.0,
            swing_hinge: swing.1,
            confidence: conf,
            family: String::new(),
        });
    }
    (out, skipped)
}

/// Doors and windows from the gaps of the proposed walls.
pub fn propose_openings(wp: &WallProposal, opt: &ProposeOptions, mem: &Memory) -> Vec<OpeningCand> {
    openings_impl(wp, opt, mem).0
}

enum Label {
    Area(f64),
    /// Area label whose number reads two ways or not at all: never taken as a value.
    BadArea,
    Number(String),
    Name(String),
}

const AREA_UNITS: [&str; 5] = ["m2", "\u{43c}2", "m\u{b2}", "\u{43c}\u{b2}", "sqm"];

/// Number part of an area label (`12,5 m2` -> `12,5 `), None for other text.
pub(super) fn area_number(t: &str) -> Option<String> {
    let low = t.trim().to_lowercase();
    AREA_UNITS
        .iter()
        .find_map(|u| low.strip_suffix(*u).map(str::to_string))
}

/// Decimal mark of a drawing (area labels and block attributes), once per drawing; the rule is
/// [`crate::num_file`].
pub(super) fn drawing_mark(u: &Underlay) -> Option<Mark> {
    let areas = u.texts.iter().filter_map(|t| area_number(&t.text));
    let attrs = u
        .inserts
        .iter()
        .flat_map(|i| i.attribs.iter().map(|(_, v)| v.clone()));
    detect_mark(areas.chain(attrs))
}

fn classify(t: &str, mark: Option<Mark>) -> Option<Label> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(num) = area_number(t) {
        match parse_num(&num, mark) {
            Ok(n) => return Some(Label::Area(n.value)),
            Err(_) if numeric_like(&num) => return Some(Label::BadArea),
            Err(_) => {}
        }
    }
    let n = t.chars().count();
    let numeric = n <= 6
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| c.is_alphanumeric() || "-./".contains(c));
    Some(if numeric {
        Label::Number(t.into())
    } else {
        Label::Name(t.split_whitespace().collect::<Vec<_>>().join(" "))
    })
}

fn rooms_impl(
    u: &Underlay,
    wp: &WallProposal,
    opt: &ProposeOptions,
    mem: &Memory,
) -> (Vec<RoomCand>, usize, usize, usize) {
    if wp.strips.is_empty() {
        return (vec![], 0, 0, 0);
    }
    let mark = drawing_mark(u);
    let tol = Tol {
        length: opt.walls.tol.max(1e-6),
        angle: opt.walls.angle_tol.max(1e-6),
    };
    let Ok(j) = join(&wp.strips, tol) else {
        return (vec![], 0, 0, 0);
    };
    let mut polys: Vec<Vec<V2>> = vec![];
    for (i, s) in wp.strips.iter().enumerate() {
        let Ok(fp) = layer_footprints(s, &j.pieces[i], i, tol) else {
            return (vec![], 0, 0, 0);
        };
        polys.extend(fp.iter().flatten().map(|p| p.outer.clone()));
    }
    polys.extend(j.fillers.iter().cloned());
    let arr = arrange(&polys, tol);
    let boxes: Vec<(V2, V2)> = arr
        .rooms
        .iter()
        .map(|r| {
            let mut lo = v2(f64::MAX, f64::MAX);
            let mut hi = v2(f64::MIN, f64::MIN);
            for p in &r.outer {
                lo = v2(lo.x.min(p.x), lo.y.min(p.y));
                hi = v2(hi.x.max(p.x), hi.y.max(p.y));
            }
            (lo, hi)
        })
        .collect();

    let skip_text = ignored_layers(
        u.texts.iter().map(|t| t.layer.clone()),
        &opt.ignore_text_layers,
    );
    let mut by_room: BTreeMap<usize, Vec<(V2, Label)>> = BTreeMap::new();
    for t in &u.texts {
        if skip_text[&t.layer] {
            continue;
        }
        let Some(lab) = classify(&t.text, mark) else {
            continue;
        };
        // Line midpoint: glyph width is about 0.6 of the height.
        let w = 0.6 * t.height * t.text.trim().chars().count() as f64;
        let (c, s) = (t.rotation.cos(), t.rotation.sin());
        let (dx, dy) = (w / 2.0, t.height / 2.0);
        let p = v2(t.pos.0 + dx * c - dy * s, t.pos.1 + dx * s + dy * c);
        let hit = arr.rooms.iter().enumerate().position(|(i, r)| {
            let (lo, hi) = boxes[i];
            p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && r.contains(p)
        });
        if let Some(i) = hit {
            by_room.entry(i).or_default().push((p, lab));
        }
    }
    let mut out = vec![];
    let (mut skipped, mut labeled, mut bad_numbers) = (0, 0, 0);
    for (i, mut labs) in by_room {
        let reg = &arr.rooms[i];
        labs.sort_by(|a, b| b.0.y.total_cmp(&a.0.y).then(a.0.x.total_cmp(&b.0.x)));
        let names: Vec<&str> = labs
            .iter()
            .filter_map(|(_, l)| {
                if let Label::Name(s) = l {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        let nums: Vec<&str> = labs
            .iter()
            .filter_map(|(_, l)| {
                if let Label::Number(s) = l {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        let areas: Vec<f64> = labs
            .iter()
            .filter_map(|(_, l)| {
                if let Label::Area(a) = l {
                    Some(*a)
                } else {
                    None
                }
            })
            .collect();
        let bad = labs
            .iter()
            .filter(|(_, l)| matches!(l, Label::BadArea))
            .count();
        bad_numbers += bad;
        if names.is_empty() && nums.is_empty() {
            continue;
        }
        labeled += 1;
        let mut conf: f64 = if names.is_empty() { 0.6 } else { 0.9 };
        if names.len() > 1 || nums.len() > 1 {
            conf = conf.min(0.7);
        }
        if bad > 0 || areas.iter().any(|a| (a - reg.area).abs() > 0.1 * reg.area) {
            conf -= 0.2;
        }
        let name = if names.is_empty() {
            "Room".to_string()
        } else {
            names.join(" ")
        };
        let number = nums.first().map_or(String::new(), |s| s.to_string());
        let rings: Vec<&[V2]> = std::iter::once(reg.outer.as_slice())
            .chain(reg.holes.iter().map(|h| h.as_slice()))
            .collect();
        let pt = interior_point(&rings).unwrap_or(labs[0].0);
        let (lo, hi) = boxes[i];
        let id = hash_key(
            3,
            &[q(lo.x), q(lo.y), q(hi.x), q(hi.y)],
            &format!("{name}\0{number}"),
        );
        if mem.contains(id) {
            skipped += 1;
            continue;
        }
        out.push(RoomCand {
            id,
            name,
            number,
            point: (pt.x, pt.y),
            area: reg.area,
            confidence: conf,
            boundary: vec![],
        });
    }
    (out, skipped, arr.rooms.len().saturating_sub(labeled), bad_numbers)
}

/// Rooms: labels (name, number, area) inside closed regions between walls.
pub fn propose_rooms(
    u: &Underlay,
    wp: &WallProposal,
    opt: &ProposeOptions,
    mem: &Memory,
) -> Vec<RoomCand> {
    rooms_impl(u, wp, opt, mem).0
}

/// All three stages and a summary report.
pub fn propose(
    u: &Underlay,
    types: &[WallTypeInfo],
    opt: &ProposeOptions,
    mem: &Memory,
) -> Proposals {
    let wp = propose_walls(u, types, opt, mem);
    let (mut openings, sk_o) = openings_impl(&wp, opt, mem);
    let blocks = super::extra::openings_from_blocks_checked(
        u,
        &opt.rules,
        &wp.walls,
        &mut openings,
        (opt.door_height, opt.window_sill, opt.window_height),
        mem,
    );
    let (mut rooms, sk_r, unlabeled, bad_numbers) = rooms_impl(u, &wp, opt, mem);
    super::extra::rooms_from_outlines(u, &mut rooms, mem, &opt.lang);
    let grids = super::extra::propose_grids(u, mem);
    let columns = super::extra::propose_columns(u, &grids, &wp.walls, mem);
    let mut report = wp.report.clone();
    report.remembered_skipped += sk_o + sk_r;
    report.unlabeled_regions = unlabeled;
    report.unreadable_numbers = bad_numbers + blocks.unreadable_widths;
    report.doors = openings.iter().filter(|o| o.kind == "door").count();
    report.windows = openings.len() - report.doors;
    report.rooms = rooms.len();
    report.grids = grids.len();
    report.columns = columns.len();
    report.block_openings = blocks.added;
    for o in &openings {
        if blocks.review.contains(&o.id) {
            report.ambiguous.push(Ambiguity {
                id: o.id,
                reason: "block width attribute reads two ways or not at all: width not taken from it",
            });
        } else if o.confidence < 0.5 {
            report.ambiguous.push(Ambiguity {
                id: o.id,
                reason: "gap without door arc or glass lines",
            });
        }
    }
    for r in &rooms {
        if r.confidence < 0.8 {
            report.ambiguous.push(Ambiguity {
                id: r.id,
                reason: "room label: several names or numbers, or area mismatch",
            });
        }
    }
    Proposals {
        walls: wp.walls,
        openings,
        rooms,
        grids,
        columns,
        report,
    }
}

#[cfg(test)]
mod num_tests {
    use super::*;

    fn area(t: &str, mark: Option<Mark>) -> Option<f64> {
        match classify(t, mark) {
            Some(Label::Area(v)) => Some(v),
            _ => None,
        }
    }

    fn label(text: &str) -> UText {
        UText {
            layer: "0".into(),
            pos: (0.0, 0.0),
            height: 0.3,
            rotation: 0.0,
            text: text.into(),
            pen: 0,
            fmt: Default::default(),
        }
    }

    #[test]
    fn area_labels_follow_the_drawing_rule() {
        assert_eq!(area("1,5 m2", None), Some(1.5));
        assert_eq!(area("1,234.5 m2", None), Some(1234.5));
        assert_eq!(area("1.234,5 \u{43c}\u{b2}", None), Some(1234.5));
        assert_eq!(area("1 250,5 m2", None), Some(1250.5));
        // ambiguous without a drawing mark: flagged, never 1.25 and never a room name
        assert!(matches!(classify("1,250 m2", None), Some(Label::BadArea)));
        assert_eq!(area("1,250 m2", Some(Mark::Comma)), Some(1.25));
        assert_eq!(area("1,250 m2", Some(Mark::Point)), Some(1250.0));
        assert!(matches!(classify("Hall m2", None), Some(Label::Name(_))));
    }

    #[test]
    fn drawing_mark_from_area_labels_only() {
        let with = |t: &[&str]| Underlay {
            texts: t.iter().map(|s| label(s)).collect(),
            ..Default::default()
        };
        // the room number `2.15` does not vote; `12,75` proves the comma
        let u = with(&["1,250 m2", "2.15", "12,75 \u{43c}2"]);
        assert_eq!(drawing_mark(&u), Some(Mark::Comma));
        assert_eq!(drawing_mark(&with(&["1,250 m2", "Office"])), None);
    }
}
