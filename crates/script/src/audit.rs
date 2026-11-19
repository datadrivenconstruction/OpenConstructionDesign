// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What each part is (role and section, for the element name) and where it is (bounds and
//! member axis), and `audit(...)`: the structure-wide checks a script runs before it ends
//! (members meeting at nodes, no duplicates, no members crossing away from a node, every
//! part connected to the ground). The model's realism critic checks the applied result again.

use std::collections::{HashMap, HashSet};

use ddc_ops::Profile;
use mlua::{Lua, Table, Value};

use crate::stdlib::builder;
use crate::values::{err, LResult, Shape, FormV, V3};

/// Ends closer than this meet (the node rule of the critic).
pub(crate) const TOL: f64 = 0.001;
/// An extrusion this many section depths long is a member.
const MEMBER_ASPECT: f64 = 3.0;
const CELL: f64 = 2.0;
const MAX_CELLS: i64 = 4096;
/// Problems named in a strict audit's refusal.
const SHOWN: usize = 6;

/// Where a created part is.
#[derive(Clone, Debug)]
pub(crate) struct Geo {
    pub bbox: [f64; 6],
    /// Straight member: axis ends and half the section depth.
    pub axis: Option<(V3, V3, f64)>,
}

fn mm(v: f64) -> String {
    let r = (v * 10000.0).round() / 10.0;
    if (r - r.round()).abs() < 1e-9 {
        format!("{}", r.round() as i64)
    } else {
        format!("{r}")
    }
}

/// Local extents (x0, x1, y0, y1) of a profile; catalogue sections are centred.
pub(crate) fn extents(p: &Profile) -> [f64; 4] {
    let c = |w: f64, h: f64| [-w / 2.0, w / 2.0, -h / 2.0, h / 2.0];
    let pts = |v: &[ddc_ops::P2]| {
        let mut e = [f64::MAX, f64::MIN, f64::MAX, f64::MIN];
        for p in v {
            let [x, y] = p.to_m();
            e = [e[0].min(x), e[1].max(x), e[2].min(y), e[3].max(y)];
        }
        if e[0] > e[1] {
            [0.0; 4]
        } else {
            e
        }
    };
    match p {
        Profile::Rect { w, h } | Profile::Tube { w, h, .. } => c(w.to_m(), h.to_m()),
        Profile::Circle { r } => c(2.0 * r.to_m(), 2.0 * r.to_m()),
        Profile::Ring { r_out, .. } => c(2.0 * r_out.to_m(), 2.0 * r_out.to_m()),
        Profile::I { h, b, .. }
        | Profile::H { h, b, .. }
        | Profile::C { h, b, .. }
        | Profile::T { h, b, .. }
        | Profile::L { h, b, .. }
        | Profile::Z { h, b, .. } => c(b.to_m(), h.to_m()),
        Profile::Polygon { pts: v } | Profile::Polyline { pts: v, .. } => pts(v),
        Profile::Rings { outer, .. } => pts(outer),
        Profile::Stored { .. } => [0.0; 4],
    }
}

fn flat(p: &Profile) -> bool {
    matches!(
        p,
        Profile::Polygon { .. } | Profile::Polyline { .. } | Profile::Rings { .. }
    )
}

/// Section text of a profile in millimetres: "CHS 508x20", "RHS 300x200x10", "I 500x200".
pub(crate) fn section_label(p: &Profile) -> String {
    match p {
        Profile::Rect { w, h } => format!("{}x{}", mm(w.to_m()), mm(h.to_m())),
        Profile::Circle { r } => format!("D{}", mm(2.0 * r.to_m())),
        Profile::Ring { r_out, r_in } => format!(
            "CHS {}x{}",
            mm(2.0 * r_out.to_m()),
            mm(r_out.to_m() - r_in.to_m())
        ),
        Profile::Tube { w, h, t } if (w.to_m() - h.to_m()).abs() < 1e-9 => {
            format!("SHS {}x{}", mm(w.to_m()), mm(t.to_m()))
        }
        Profile::Tube { w, h, t } => {
            format!("RHS {}x{}x{}", mm(h.to_m()), mm(w.to_m()), mm(t.to_m()))
        }
        Profile::I { h, b, .. } => format!("I {}x{}", mm(h.to_m()), mm(b.to_m())),
        Profile::H { h, b, .. } => format!("H {}x{}", mm(h.to_m()), mm(b.to_m())),
        Profile::C { h, b, .. } => format!("C {}x{}", mm(h.to_m()), mm(b.to_m())),
        Profile::T { h, b, .. } => format!("T {}x{}", mm(h.to_m()), mm(b.to_m())),
        Profile::L { h, b, t } => format!("L {}x{}x{}", mm(h.to_m()), mm(b.to_m()), mm(t.to_m())),
        Profile::Z { h, b, t } => format!("Z {}x{}x{}", mm(h.to_m()), mm(b.to_m()), mm(t.to_m())),
        Profile::Polygon { .. } | Profile::Polyline { .. } => "polygon".into(),
        Profile::Rings { .. } => "perforated".into(),
        Profile::Stored { .. } => "stored".into(),
    }
}

/// What the part is: its section, or the thickness of a plate, panel or slab.
pub(crate) fn label(f: &FormV) -> String {
    match &f.0 {
        Shape::Extrude { prof, len, .. } if flat(prof) => {
            let e = extents(prof);
            let span = (e[1] - e[0]).max(e[3] - e[2]);
            match prof {
                Profile::Rings { .. } => format!("perforated {} mm", mm(*len)),
                _ if *len * MEMBER_ASPECT <= span => format!("{} mm", mm(*len)),
                _ => "polygon".into(),
            }
        }
        Shape::Extrude { prof, .. } | Shape::Sweep { prof, .. } => section_label(prof),
        Shape::Loft { .. } => "loft".into(),
    }
}

fn grow(b: &mut [f64; 6], p: V3, r: f64) {
    b[0] = b[0].min(p.x - r);
    b[1] = b[1].min(p.y - r);
    b[2] = b[2].min(p.z - r);
    b[3] = b[3].max(p.x + r);
    b[4] = b[4].max(p.y + r);
    b[5] = b[5].max(p.z + r);
}

const EMPTY: [f64; 6] = [f64::MAX, f64::MAX, f64::MAX, f64::MIN, f64::MIN, f64::MIN];

/// Bounds of a part the model builds from points (a grid structure), padded by `r`.
pub(crate) fn geo_of_points(pts: &[V3], r: f64) -> Geo {
    let mut b = EMPTY;
    for p in pts {
        grow(&mut b, *p, r);
    }
    Geo { bbox: b, axis: None }
}

/// A catalogue member between two points: its axis takes part in the node checks.
pub(crate) fn geo_of_member(a: V3, b: V3, r: f64) -> Geo {
    let mut g = geo_of_points(&[a, b], r);
    g.axis = Some((a, b, r));
    g
}

/// Bounds and member axis of a form.
pub(crate) fn geo_of(f: &FormV) -> Geo {
    let mut b = EMPTY;
    match &f.0 {
        Shape::Extrude { prof, o, x, z, len } => {
            let y = z.cross(*x);
            let e = extents(prof);
            for (u, v) in [(e[0], e[2]), (e[1], e[2]), (e[0], e[3]), (e[1], e[3])] {
                let p = o.add(x.scale(u)).add(y.scale(v));
                grow(&mut b, p, 0.0);
                grow(&mut b, p.add(z.scale(*len)), 0.0);
            }
            let depth = (e[1] - e[0]).max(e[3] - e[2]);
            let axis = (!flat(prof) && *len >= MEMBER_ASPECT * depth)
                .then(|| (*o, o.add(z.scale(*len)), depth / 2.0));
            Geo { bbox: b, axis }
        }
        Shape::Sweep { prof, path, scale, .. } => {
            let e = extents(prof);
            let r = e.iter().fold(0.0f64, |m, v| m.max(v.abs()))
                * std::f64::consts::SQRT_2
                * scale.max(1.0);
            let n = path.pts.len();
            let segs = if path.closed { n } else { n.saturating_sub(1) };
            for i in 0..n {
                grow(&mut b, path.pts[i], r);
            }
            for i in 0..segs {
                let (a, c) = (path.pts[i], path.pts[(i + 1) % n]);
                let bu = path.bulges.get(i).copied().unwrap_or(0.0);
                if bu != 0.0 {
                    // the arc's sagitta point, off the chord in plan
                    let d = c.sub(a);
                    let mid = a.add(d.scale(0.5));
                    let chord = (d.x * d.x + d.y * d.y).sqrt();
                    let s = bu * chord / 2.0;
                    let nrm = V3::new(d.y, -d.x, 0.0).unit().unwrap_or(V3::new(0.0, 0.0, 0.0));
                    grow(&mut b, mid.add(nrm.scale(s)), r);
                    grow(&mut b, mid.sub(nrm.scale(s)), r);
                }
            }
            let straight = segs == 1 && path.bulges.iter().all(|x| *x == 0.0);
            let axis = straight.then(|| (path.pts[0], path.pts[1], r / std::f64::consts::SQRT_2));
            Geo { bbox: b, axis }
        }
        Shape::Loft { secs } => {
            for (p, at) in secs {
                let e = extents(p);
                grow(&mut b, at.add(V3::new(e[0], e[2], 0.0)), 0.0);
                grow(&mut b, at.add(V3::new(e[1], e[3], 0.0)), 0.0);
            }
            Geo { bbox: b, axis: None }
        }
    }
}

/// Closest distance of `p` to segment `a`-`b`.
pub(crate) fn dist_seg(p: V3, a: V3, b: V3) -> f64 {
    let d = b.sub(a);
    let l2 = d.dot(d);
    let t = if l2 < 1e-18 { 0.0 } else { (p.sub(a).dot(d) / l2).clamp(0.0, 1.0) };
    p.sub(a.add(d.scale(t))).len()
}

/// Closest points of two segments: distance and the parameters on each.
fn seg_seg(p1: V3, q1: V3, p2: V3, q2: V3) -> (f64, f64, f64) {
    let (d1, d2, r) = (q1.sub(p1), q2.sub(p2), p1.sub(p2));
    let (a, e, f) = (d1.dot(d1), d2.dot(d2), d2.dot(r));
    let (s, t);
    if a <= 1e-18 && e <= 1e-18 {
        return (r.len(), 0.0, 0.0);
    }
    if a <= 1e-18 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-18 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let mut s0 = if den > 1e-18 { ((b * f - c * e) / den).clamp(0.0, 1.0) } else { 0.0 };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    let c1 = p1.add(d1.scale(s));
    let c2 = p2.add(d2.scale(t));
    (c1.sub(c2).len(), s, t)
}

fn cell(v: f64) -> i64 {
    (v / CELL).floor() as i64
}

/// Boxes hashed into cells; boxes spanning too many cells are checked against all.
struct Grid {
    cells: HashMap<(i64, i64, i64), Vec<usize>>,
    wide: Vec<usize>,
}

impl Grid {
    fn new(boxes: &[[f64; 6]], pad: f64) -> Grid {
        let mut g = Grid { cells: HashMap::new(), wide: vec![] };
        for (i, b) in boxes.iter().enumerate() {
            let lo = (cell(b[0] - pad), cell(b[1] - pad), cell(b[2] - pad));
            let hi = (cell(b[3] + pad), cell(b[4] + pad), cell(b[5] + pad));
            let n = (hi.0 - lo.0 + 1) * (hi.1 - lo.1 + 1) * (hi.2 - lo.2 + 1);
            if !(1..=MAX_CELLS).contains(&n) {
                g.wide.push(i);
                continue;
            }
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        g.cells.entry((x, y, z)).or_default().push(i);
                    }
                }
            }
        }
        g
    }

    /// Indices whose padded box may reach `b`, each once.
    fn near(&self, b: &[f64; 6], out: &mut Vec<usize>) {
        out.clear();
        let lo = (cell(b[0]), cell(b[1]), cell(b[2]));
        let hi = (cell(b[3]), cell(b[4]), cell(b[5]));
        let n = (hi.0 - lo.0 + 1) * (hi.1 - lo.1 + 1) * (hi.2 - lo.2 + 1);
        if (1..=MAX_CELLS).contains(&n) {
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        if let Some(v) = self.cells.get(&(x, y, z)) {
                            out.extend_from_slice(v);
                        }
                    }
                }
            }
        } else {
            out.extend(self.cells.values().flatten());
        }
        out.extend_from_slice(&self.wide);
        out.sort_unstable();
        out.dedup();
    }
}

fn gap(a: &[f64; 6], b: &[f64; 6]) -> f64 {
    let d = |i: usize| (a[i] - b[i + 3]).max(b[i] - a[i + 3]).max(0.0);
    (d(0).powi(2) + d(1).powi(2) + d(2).powi(2)).sqrt()
}

fn find(uf: &mut [usize], mut i: usize) -> usize {
    while uf[i] != i {
        uf[i] = uf[uf[i]];
        i = uf[i];
    }
    i
}

#[derive(Debug, Default)]
pub(crate) struct Report {
    pub parts: usize,
    pub members: usize,
    pub free_ends: usize,
    /// (part, part, gap): an end near another member's axis that misses it.
    pub near_miss: Vec<(usize, usize, f64)>,
    pub duplicates: Vec<(usize, usize)>,
    /// Members crossing each other away from any node.
    pub clashes: Vec<(usize, usize)>,
    /// Groups of parts touching nothing that reaches the ground: (lowest part, count).
    pub floating: Vec<(usize, usize)>,
}

/// The checks over every part created so far; `ground` is the z of the ground.
pub(crate) fn check(geo: &[Geo], ground: f64) -> Report {
    let mut r = Report { parts: geo.len(), ..Report::default() };
    let mem: Vec<usize> = (0..geo.len()).filter(|&i| geo[i].axis.is_some()).collect();
    r.members = mem.len();
    let ax = |i: usize| geo[i].axis.unwrap_or((V3::new(0.0, 0.0, 0.0), V3::new(0.0, 0.0, 0.0), 0.0));
    let boxes: Vec<[f64; 6]> = mem.iter().map(|&i| geo[i].bbox).collect();
    let grid = Grid::new(&boxes, 0.05);
    let mut near = vec![];
    let mut pairs = HashSet::new();
    let all_boxes: Vec<[f64; 6]> = geo.iter().map(|g| g.bbox).collect();
    let pgrid = Grid::new(&all_boxes, TOL);
    for (mi, &i) in mem.iter().enumerate() {
        let (a, b, ri) = ax(i);
        for e in [a, b] {
            let pt = [e.x, e.y, e.z, e.x, e.y, e.z];
            grid.near(&pt, &mut near);
            let mut touching = e.z <= ground + 0.005;
            let mut miss: Option<(usize, f64)> = None;
            for &mj in &near {
                if mj == mi {
                    continue;
                }
                let j = mem[mj];
                let (c, d, rj) = ax(j);
                let g = dist_seg(e, c, d);
                if g <= TOL {
                    touching = true;
                    break;
                }
                if g <= rj + 0.05 && miss.is_none_or(|m| g < m.1) {
                    miss = Some((j, g));
                }
            }
            if !touching {
                match miss {
                    Some((j, g)) => {
                        if pairs.insert((i.min(j), i.max(j), 0u8)) {
                            r.near_miss.push((i, j, g));
                        }
                    }
                    None => {
                        // resting on a panel, slab or other part counts as supported
                        let mut held = false;
                        pgrid.near(&pt, &mut near);
                        for &k in &near {
                            if k != i && geo[k].axis.is_none() && gap(&geo[k].bbox, &pt) <= TOL {
                                held = true;
                                break;
                            }
                        }
                        if !held {
                            r.free_ends += 1;
                        }
                    }
                }
            }
        }
        // crossings and duplicates with later members
        let bb = geo[i].bbox;
        let probe = [bb[0] - ri, bb[1] - ri, bb[2] - ri, bb[3] + ri, bb[4] + ri, bb[5] + ri];
        grid.near(&probe, &mut near);
        for &mj in &near {
            if mj <= mi {
                continue;
            }
            let j = mem[mj];
            let (c, d, rj) = ax(j);
            let same = (a.sub(c).len() <= TOL && b.sub(d).len() <= TOL)
                || (a.sub(d).len() <= TOL && b.sub(c).len() <= TOL);
            if same {
                r.duplicates.push((i, j));
                continue;
            }
            let (dist, _, _) = seg_seg(a, b, c, d);
            if dist >= 0.5 * (ri + rj) {
                continue;
            }
            let joint = [a, b].iter().any(|e| dist_seg(*e, c, d) <= ri.max(rj) + TOL)
                || [c, d].iter().any(|e| dist_seg(*e, a, b) <= ri.max(rj) + TOL);
            let (u, v) = (b.sub(a), d.sub(c));
            let parallel = u.unit().zip(v.unit()).is_some_and(|(p, q)| p.dot(q).abs() > 0.9999);
            let overlap = parallel && {
                let ul = u.len();
                let p = u.scale(1.0 / ul);
                let (t0, t1) = (c.sub(a).dot(p), d.sub(a).dot(p));
                t0.max(t1).min(ul) - t0.min(t1).max(0.0) > ri.max(rj)
            };
            if !joint || overlap {
                r.clashes.push((i, j));
            }
        }
    }
    // connected to the ground through touching bounds
    let n = geo.len();
    let mut uf: Vec<usize> = (0..n).collect();
    for i in 0..n {
        pgrid.near(&geo[i].bbox, &mut near);
        for &j in &near {
            if j > i && gap(&geo[i].bbox, &geo[j].bbox) <= TOL {
                let (x, y) = (find(&mut uf, i), find(&mut uf, j));
                uf[x] = y;
            }
        }
    }
    let mut grounded = vec![false; n];
    for i in 0..n {
        if geo[i].bbox[2] <= ground + 0.005 {
            let k = find(&mut uf, i);
            grounded[k] = true;
        }
    }
    let mut comps: HashMap<usize, (usize, usize)> = HashMap::new();
    for i in 0..n {
        let k = find(&mut uf, i);
        if grounded[k] {
            continue;
        }
        let e = comps.entry(k).or_insert((i, 0));
        e.1 += 1;
        if geo[i].bbox[2] < geo[e.0].bbox[2] {
            e.0 = i;
        }
    }
    let mut f: Vec<(usize, usize)> = comps.into_values().collect();
    f.sort_unstable();
    r.floating = f;
    r
}

/// `audit([{strict = true, ground = 0}])`: the checks over all parts so far, as a table.
pub(crate) fn audit(lua: &Lua, opts: Option<Table>) -> LResult<Table> {
    let (mut strict, mut ground) = (false, 0.0);
    if let Some(t) = &opts {
        for kv in t.clone().pairs::<String, Value>() {
            let (k, v) = kv?;
            match (k.as_str(), &v) {
                ("strict", Value::Boolean(b)) => strict = *b,
                ("ground", Value::Integer(_) | Value::Number(_)) => {
                    ground = crate::values::num(&v).unwrap_or(0.0)
                }
                ("ground", Value::String(s)) => {
                    ground = crate::units::parse_len(&s.to_str()?)
                        .map_err(|e| mlua::Error::RuntimeError(format!("audit: ground: {e}")))?
                }
                _ => return err(format!("audit: unknown option `{k}` (options: strict, ground)")),
            }
        }
    }
    let (rep, keys) = {
        let b = builder(lua)?;
        (check(&b.geo, ground), b.keys())
    };
    let key = |i: usize| keys.get(i).map_or("?".to_string(), |k| k.key.clone());
    let out = lua.create_table()?;
    out.set("parts", rep.parts)?;
    out.set("members", rep.members)?;
    out.set("free_ends", rep.free_ends)?;
    out.set("near_miss", rep.near_miss.len())?;
    out.set("duplicates", rep.duplicates.len())?;
    out.set("clashes", rep.clashes.len())?;
    out.set("floating", rep.floating.len())?;
    let mut problems: Vec<String> = vec![];
    for (i, j, g) in &rep.near_miss {
        problems.push(format!("{} misses {} by {} mm", key(*i), key(*j), mm(*g)));
    }
    for (i, j) in &rep.duplicates {
        problems.push(format!("{} duplicates {}", key(*i), key(*j)));
    }
    for (i, j) in &rep.clashes {
        problems.push(format!("{} crosses {} away from a node", key(*i), key(*j)));
    }
    for (i, c) in &rep.floating {
        problems.push(format!("{c} parts around {} do not reach the ground", key(*i)));
    }
    let line = format!(
        "audit: {} parts, {} members, {} free ends, {} near misses, {} duplicates, {} clashes, {} floating groups",
        rep.parts,
        rep.members,
        rep.free_ends,
        rep.near_miss.len(),
        rep.duplicates.len(),
        rep.clashes.len(),
        rep.floating.len()
    );
    builder(lua)?.print(line);
    if strict && !problems.is_empty() {
        let more = problems.len().saturating_sub(SHOWN);
        let mut msg = problems.into_iter().take(SHOWN).collect::<Vec<_>>().join("; ");
        if more > 0 {
            msg.push_str(&format!("; and {more} more"));
        }
        return err(format!("audit: {msg}"));
    }
    Ok(out)
}
