//! Load expansion (design 1.7, P1 frame-only): stored loads of one case -> solver loads.
//! - point: on a member at station `x` (value `x` or a station key `m:..@x`), or on a node;
//! - line: trapezoidal `q1` at `x1` to `q2` at `x2` over the member stations;
//! - `projected` frame: line load per horizontal length (scaled by the axis' horizontal part);
//! - area: on a slab panel, by the tributary rule ([`crate::tributary`]) to the members and
//!   `line_support` walls along its edges; openings remove load;
//! - self-weight case: members rho A g, slab panels rho t g as an area load (walls carry their
//!   own weight to their footings outside the frame in P1).
//! Stations are along the analytical axis from its first node; a segment with rigid offsets
//! maps them linearly onto its flexible part and keeps the total load.

use ddc_fem::{Axes, Load as FLoad};
use ddc_model::{Analytical, Load, LoadCase};
use serde_json::Value as J;

use crate::adapter::{to_global, CalcInput, CaseLoads, Seg};
use crate::tributary::{self as tr, Piece, PieceLoad, P2};
use crate::{add3, dot3, mul3, norm3, push_resultant, sub3, unit3, Issue, Resolver, P3, G};

struct Ex {
    loads: Vec<FLoad>,
    info: CaseLoads,
    issues: Vec<Issue>,
}

impl Ex {
    fn intend(&mut self, f: P3) {
        for k in 0..3 {
            self.info.intended[k] += f[k];
        }
        self.info.scale += norm3(f);
    }

    /// Concentrated force on segment `si` at flexible distance `a`.
    fn point(&mut self, inp: &CalcInput, si: usize, a: f64, f: P3, axes: Axes) {
        let s = &inp.segs[si];
        let g = to_global(s, f, axes);
        push_resultant(&mut self.info.applied, add3(s.start, mul3(s.x, a)), g, [0.0; 3]);
        self.loads.push(FLoad::Point { member: si, a, f, m: [0.0; 3], axes });
    }

    /// Linear line load on segment `si` from `a` to `b` (flexible distances).
    fn dist(&mut self, inp: &CalcInput, si: usize, a: f64, b: f64, w1: P3, w2: P3, axes: Axes) {
        if b - a <= 1e-12 {
            return;
        }
        let s = &inp.segs[si];
        let (g1, g2) = (to_global(s, w1, axes), to_global(s, w2, axes));
        // Simpson is exact: the force is linear and its moment quadratic along the member
        let h = b - a;
        for (wt, t) in [(1.0 / 6.0, 0.0), (4.0 / 6.0, 0.5), (1.0 / 6.0, 1.0)] {
            let w = add3(mul3(g1, 1.0 - t), mul3(g2, t));
            let p = add3(s.start, mul3(s.x, a + t * h));
            push_resultant(&mut self.info.applied, p, mul3(w, wt * h), [0.0; 3]);
        }
        self.loads.push(FLoad::Dist { member: si, a, b, w1, w2, axes });
    }
}

fn axis(c: &str) -> Option<P3> {
    Some(match c {
        "x" => [1.0, 0.0, 0.0],
        "y" => [0.0, 1.0, 0.0],
        "z" => [0.0, 0.0, 1.0],
        "-x" => [-1.0, 0.0, 0.0],
        "-y" => [0.0, -1.0, 0.0],
        "-z" => [0.0, 0.0, -1.0],
        _ => return None,
    })
}

/// Unit direction and its axes: `x`..`-z` global, `lx`..`-lz` local, a JSON vector global;
/// frame `local` makes a global letter or vector local.
pub fn direction(dir: &str, frame: &str) -> Option<(P3, Axes)> {
    let local = frame == "local";
    let ax = |l: bool| if l || local { Axes::Local } else { Axes::Global };
    if let Some(v) = axis(dir) {
        return Some((v, ax(false)));
    }
    let neg = dir.starts_with('-');
    let body = dir.trim_start_matches('-');
    if let Some(r) = body.strip_prefix('l') {
        let v = axis(r)?;
        return Some((if neg { mul3(v, -1.0) } else { v }, Axes::Local));
    }
    let v: Vec<f64> = serde_json::from_str::<Vec<f64>>(dir).ok()?;
    if v.len() != 3 || v.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let u = unit3([v[0], v[1], v[2]]);
    (norm3(u) > 0.0).then_some((u, ax(false)))
}

fn targets(l: &Load, resolver: Option<Resolver>, iss: &mut Vec<Issue>) -> Vec<String> {
    let key = format!("l:{}", l.id);
    if let Some(q) = l.target.strip_prefix("filter:") {
        return match resolver.map(|r| r(q)) {
            Some(Ok(v)) => {
                if v.is_empty() {
                    iss.push(Issue::warn("filter_empty", key, q.to_string()));
                }
                v
            }
            Some(Err(e)) => {
                iss.push(Issue::warn("filter_failed", key, e));
                vec![]
            }
            None => {
                iss.push(Issue::warn("filter_unresolved", key, "no query resolver"));
                vec![]
            }
        };
    }
    match serde_json::from_str::<J>(&l.target) {
        Ok(J::Array(a)) => a.iter().filter_map(J::as_str).map(str::to_string).collect(),
        _ => vec![l.target.clone()],
    }
}

fn num(v: &J, k: &str) -> Option<f64> {
    v.get(k).and_then(J::as_f64).filter(|x| x.is_finite())
}

/// Segment and flexible distance of station `x` on a member (None past its end).
fn at_station(inp: &CalcInput, segs: &[usize], x: f64) -> Option<(usize, f64)> {
    let total: f64 = segs.iter().map(|&s| inp.segs[s].len).sum();
    if x < -1e-9 || x > total + 1e-9 {
        return None;
    }
    let x = x.clamp(0.0, total);
    for (k, &si) in segs.iter().enumerate() {
        let s: &Seg = &inp.segs[si];
        if x <= s.x0 + s.len || k + 1 == segs.len() {
            return Some((si, ((x - s.x0) * s.flex / s.len).clamp(0.0, s.flex)));
        }
    }
    None
}

/// Expands the loads of one case; the case's resultants go to `CaseLoads`.
pub(crate) fn expand_case(
    a: &Analytical,
    inp: &CalcInput,
    pden: &[f64],
    case: &LoadCase,
    loads: &[&Load],
    resolver: Option<Resolver>,
) -> (Vec<FLoad>, CaseLoads, Vec<Issue>) {
    let mut ex = Ex {
        loads: vec![],
        info: CaseLoads { case_id: case.id, name: case.name.clone(), ..CaseLoads::default() },
        issues: vec![],
    };
    if case.self_weight {
        self_weight(a, inp, pden, &mut ex);
    }
    for l in loads {
        one(a, inp, l, resolver, &mut ex);
    }
    ex.info.count = ex.loads.len();
    if ex.loads.is_empty() && ex.info.walls.iter().all(|x| *x == 0.0) {
        ex.issues.push(Issue::warn("zero_load_case", format!("lc:{}", case.id), case.name.clone()));
    }
    (ex.loads, ex.info, ex.issues)
}

fn self_weight(a: &Analytical, inp: &CalcInput, pden: &[f64], ex: &mut Ex) {
    let mut warned = vec![];
    for (si, s) in inp.segs.iter().enumerate() {
        let fm = &inp.frame.members[si];
        let rho = inp.frame.materials[fm.material].density;
        let area = inp.frame.sections[fm.section].a;
        if rho <= 0.0 {
            if !warned.contains(&s.member) {
                warned.push(s.member);
                let key = a.members[s.member].key.clone();
                ex.issues.push(Issue::warn("no_density", key, "self-weight skipped"));
            }
            continue;
        }
        let w = [0.0, 0.0, -rho * area * G];
        ex.intend(mul3(w, s.flex));
        ex.dist(inp, si, 0.0, s.flex, w, w, Axes::Global);
    }
    for (j, p) in a.panels.iter().enumerate() {
        if p.kind != "slab" || p.role == "excluded" || p.role == "line_support" {
            continue;
        }
        let rho = pden.get(j).copied().unwrap_or(0.0);
        if rho <= 0.0 || p.thickness <= 0.0 {
            ex.issues.push(Issue::warn("no_density", p.key.clone(), "self-weight skipped"));
            continue;
        }
        area(a, inp, j, [0.0, 0.0, -rho * p.thickness * G], &p.key.clone(), ex);
    }
}

fn one(a: &Analytical, inp: &CalcInput, l: &Load, resolver: Option<Resolver>, ex: &mut Ex) {
    let key = format!("l:{}", l.id);
    if l.kind == "temp" {
        ex.issues.push(Issue::warn("temp_later", key, "temperature loads come in P4"));
        return;
    }
    let Some((dir, axes)) = direction(&l.dir, &l.frame) else {
        ex.issues.push(Issue::warn("bad_dir", key, l.dir.clone()));
        return;
    };
    for t in targets(l, resolver, &mut ex.issues) {
        let (mk, sx) = match t.split_once('@') {
            Some((m, x)) if t.starts_with("m:") => (m.to_string(), x.parse::<f64>().ok()),
            _ => (t.clone(), None),
        };
        match (l.kind.as_str(), t.split(':').next()) {
            ("point", Some("n")) => {
                let Some(n) = inp.node_of(&t) else {
                    ex.issues.push(Issue::warn("bad_target", key.clone(), t));
                    continue;
                };
                if axes == Axes::Local {
                    ex.issues.push(Issue::warn("bad_dir", key.clone(), "local axes on a node"));
                    continue;
                }
                let f = mul3(dir, num(&l.value, "f").unwrap_or(0.0));
                ex.intend(f);
                push_resultant(&mut ex.info.applied, inp.frame.nodes[n], f, [0.0; 3]);
                ex.loads.push(FLoad::Node { node: n, f: [f[0], f[1], f[2], 0.0, 0.0, 0.0] });
            }
            ("point", Some("m")) => {
                let segs = inp.segs_of(a, &mk);
                let x = sx.or(num(&l.value, "x")).unwrap_or(0.0);
                let Some((si, at)) = at_station(inp, &segs, x) else {
                    ex.issues.push(Issue::warn("bad_target", key.clone(), t));
                    continue;
                };
                let f = mul3(dir, num(&l.value, "f").unwrap_or(0.0));
                ex.intend(to_global(&inp.segs[si], f, axes));
                ex.point(inp, si, at, f, axes);
            }
            ("line", Some("m")) => line(a, inp, l, &mk, dir, axes, ex),
            ("area", Some("p")) => {
                let Some(j) = a.panel_of(&t) else {
                    ex.issues.push(Issue::warn("bad_target", key.clone(), t));
                    continue;
                };
                let p = &a.panels[j];
                if axes == Axes::Local || p.kind != "slab" {
                    ex.issues.push(Issue::warn("bad_target", key.clone(), "area loads: slabs"));
                    continue;
                }
                if matches!(p.role, "excluded" | "line_support") {
                    ex.issues.push(Issue::warn("panel_excluded", key.clone(), t));
                    continue;
                }
                let v = mul3(dir, num(&l.value, "p").unwrap_or(0.0));
                area(a, inp, j, v, &key, ex);
            }
            _ => ex.issues.push(Issue::warn("bad_target", key.clone(), t)),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn line(a: &Analytical, inp: &CalcInput, l: &Load, mk: &str, dir: P3, axes: Axes, ex: &mut Ex) {
    let key = format!("l:{}", l.id);
    let segs = inp.segs_of(a, mk);
    if segs.is_empty() {
        ex.issues.push(Issue::warn("bad_target", key, mk.to_string()));
        return;
    }
    let total: f64 = segs.iter().map(|&s| inp.segs[s].len).sum();
    let q1 = num(&l.value, "q1").unwrap_or(0.0);
    let q2 = num(&l.value, "q2").unwrap_or(q1);
    let x1 = num(&l.value, "x1").unwrap_or(0.0).max(0.0);
    let x2 = num(&l.value, "x2").unwrap_or(total).min(total);
    if x2 - x1 <= 1e-12 {
        ex.issues.push(Issue::warn("bad_value", key, "empty load range"));
        return;
    }
    let q = |x: f64| q1 + (q2 - q1) * (x - x1) / (x2 - x1);
    for si in segs {
        let s = &inp.segs[si];
        let (lo, hi) = (x1.max(s.x0), x2.min(s.x0 + s.len));
        if hi - lo <= 1e-12 {
            continue;
        }
        let mut fac = s.len / s.flex;
        if l.frame == "projected" {
            fac *= (s.x[0] * s.x[0] + s.x[1] * s.x[1]).sqrt();
        }
        let m = s.flex / s.len;
        let (a1, a2) = ((lo - s.x0) * m, (hi - s.x0) * m);
        let (w1, w2) = (mul3(dir, q(lo) * fac), mul3(dir, q(hi) * fac));
        ex.intend(to_global(s, mul3(add3(w1, w2), 0.5 * (a2 - a1)), axes));
        ex.dist(inp, si, a1, a2, w1, w2, axes);
    }
}

/// Where an edge piece of a panel rests.
enum Rest {
    None,
    /// (segment, station of its first node on the piece, of its second node)
    Members(Vec<(usize, f64, f64)>),
    Wall,
}

fn on_piece(p: P3, a: P3, t: P3, len: f64) -> Option<f64> {
    let d = sub3(p, a);
    let s = dot3(d, t);
    let off = norm3(sub3(d, mul3(t, s)));
    let eps = 1e-6 * len.max(1.0);
    (off <= eps && s >= -eps && s <= len + eps).then_some(s)
}

/// Distributes the area load `v` (force per m2, global) of slab panel `j`.
fn area(a: &Analytical, inp: &CalcInput, j: usize, v: P3, key: &str, ex: &mut Ex) {
    let p = &a.panels[j];
    let n = p.nodes.len();
    if n < 3 {
        return;
    }
    let pos = |k: usize| -> P3 {
        let q = a.nodes[p.nodes[k % n]];
        [q.x, q.y, q.z]
    };
    let ring: Vec<P2> = (0..n).map(|k| [pos(k)[0], pos(k)[1]]).collect();
    let holes: Vec<Vec<P2>> =
        p.holes.iter().map(|h| h.iter().map(|q| [q.x, q.y]).collect()).collect();
    let open: f64 = holes.iter().map(|h| tr::signed_area(h).abs()).sum();
    let net = tr::signed_area(&ring).abs() - open;
    ex.intend(mul3(v, net));
    if !tr::is_convex(&ring) {
        ex.issues.push(Issue::warn("area_nonconvex", key.to_string(), p.key.clone()));
    }
    let mut rests = vec![];
    let mut pieces = vec![];
    for k in 0..n {
        let (pa, pb) = (pos(k), pos(k + 1));
        let len = norm3(sub3(pb, pa));
        let t = unit3(sub3(pb, pa));
        let mut cover = vec![];
        for (si, s) in inp.segs.iter().enumerate() {
            let m = &a.members[s.member];
            let (u, w) = (a.nodes[m.nodes[s.k]], a.nodes[m.nodes[s.k + 1]]);
            let su = on_piece([u.x, u.y, u.z], pa, t, len);
            let sw = on_piece([w.x, w.y, w.z], pa, t, len);
            if let (Some(su), Some(sw)) = (su, sw) {
                if (su - sw).abs() > 1e-9 {
                    cover.push((si, su, sw));
                }
            }
        }
        let (na, nb) = (p.nodes[k], p.nodes[(k + 1) % n]);
        let wall = a.panels.iter().any(|w| {
            w.role == "line_support" && w.nodes.contains(&na) && w.nodes.contains(&nb)
        });
        let rest = if !cover.is_empty() {
            Rest::Members(cover)
        } else if wall {
            Rest::Wall
        } else {
            Rest::None
        };
        let supported = !matches!(rest, Rest::None);
        pieces.push(Piece { a: ring[k], b: ring[(k + 1) % n], supported });
        rests.push((rest, pa, t, len));
    }
    let one = p.span.filter(|s| s.0 == 1).map(|s| s.1);
    let mut pl = match one {
        Some(deg) => tr::one_way(&pieces, deg),
        None => tr::two_way(&pieces),
    };
    let mut lost = tr::holes(&pieces, &mut pl, &holes, one);
    for (load, (rest, pa, t, len)) in pl.iter().zip(&rests) {
        let carried = match rest {
            Rest::None => 0.0,
            Rest::Wall => {
                for (pt, ar) in samples(load, *pa, *t) {
                    push_resultant(&mut ex.info.walls, pt, mul3(v, ar), [0.0; 3]);
                }
                load.area()
            }
            Rest::Members(cover) => to_members(inp, load, cover, *len, v, ex),
        };
        lost += load.area() - carried;
    }
    if lost.abs() > 1e-9 * net.abs().max(1e-12) {
        let note = format!("{lost:.6} m2 of {net:.6} m2 reaches no support");
        ex.issues.push(Issue::warn("area_load_lost", key.to_string(), note));
    }
}

/// Resultant samples (point, area) of a piece load: trapezoids by Simpson, then the points.
fn samples(l: &PieceLoad, pa: P3, t: P3) -> Vec<(P3, f64)> {
    let mut v = vec![];
    for &(s0, d0, s1, d1) in &l.trap {
        let h = s1 - s0;
        for (wt, u) in [(1.0 / 6.0, 0.0), (4.0 / 6.0, 0.5), (1.0 / 6.0, 1.0)] {
            let d = d0 + (d1 - d0) * u;
            v.push((add3(pa, mul3(t, s0 + u * h)), wt * h * d));
        }
    }
    for &(s, ar) in &l.point {
        v.push((add3(pa, mul3(t, s)), ar));
    }
    v
}

/// Puts a piece load on the covering segments; returns the area carried.
fn to_members(
    inp: &CalcInput,
    l: &PieceLoad,
    cover: &[(usize, f64, f64)],
    len: f64,
    v: P3,
    ex: &mut Ex,
) -> f64 {
    let mut carried = 0.0;
    for &(si, su, sw) in cover {
        let s = &inp.segs[si];
        let (lo, hi) = (su.min(sw).max(0.0), su.max(sw).min(len));
        let m = s.flex / s.len;
        // member distance from its first node, on the flexible part
        let at = |x: f64| (if su <= sw { x - su } else { su - x }) * m;
        for &(s0, d0, s1, d1) in &l.trap {
            let (c0, c1) = (s0.max(lo), s1.min(hi));
            if c1 - c0 <= 1e-12 {
                continue;
            }
            let d = |x: f64| d0 + (d1 - d0) * (x - s0) / (s1 - s0);
            let (mut a1, mut a2, mut e1, mut e2) = (at(c0), at(c1), d(c0), d(c1));
            if a1 > a2 {
                std::mem::swap(&mut a1, &mut a2);
                std::mem::swap(&mut e1, &mut e2);
            }
            carried += 0.5 * (e1 + e2) * (c1 - c0);
            let k = 1.0 / m;
            ex.dist(inp, si, a1, a2, mul3(v, e1 * k), mul3(v, e2 * k), Axes::Global);
        }
        for &(x, ar) in &l.point {
            // a point on a shared node goes to the first covering segment only
            let first = cover.iter().find(|c| x >= c.1.min(c.2) - 1e-9 && x <= c.1.max(c.2) + 1e-9);
            if first.map(|c| c.0) != Some(si) {
                continue;
            }
            carried += ar;
            ex.point(inp, si, at(x.clamp(lo, hi)).clamp(0.0, s.flex), mul3(v, ar), Axes::Global);
        }
    }
    carried
}
