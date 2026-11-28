// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Gusset nodes: truss web members on a chord, and braces on a column, a beam or the corner
//! of both. The gusset lies in the plane of the members, is welded to the host faces and
//! takes each web member on a line of bolts along its axis. Members are set back from the
//! node until they clear the host faces and each other; the setbacks come back as trims.

use super::bar::{across, meet, Bar};
use super::catalog::{fmt_mm, BoltSize};
use super::kit::{hull, Kit};
use super::part::Role;
use super::ConnError;
use crate::gsolid::Frame;
use crate::math::{v2, v3, V2, V3};

/// Truss node: `bars[0]` is the chord, the rest are web members ending at the node.
pub(super) fn truss(k: &mut Kit, bars: &[Bar]) -> Result<(), ConnError> {
    let webs: Vec<&Bar> = bars[1..].iter().collect();
    node(k, &[&bars[0]], &webs, 1)
}

/// Bracing node: `bars[0]` is a column or beam; `bars[1]` is a second host when it crosses
/// the first square (a corner), otherwise every further member is a brace.
pub(super) fn bracing(k: &mut Kit, bars: &[Bar]) -> Result<(), ConnError> {
    let corner = bars.len() >= 3 && bars[0].u.dot(bars[1].u).abs() < 0.1;
    let n_hosts = if corner { 2 } else { 1 };
    let hosts: Vec<&Bar> = bars[..n_hosts].iter().collect();
    let braces: Vec<&Bar> = bars[n_hosts..].iter().collect();
    node(k, &hosts, &braces, n_hosts)
}

/// A web member as the gusset sees it.
struct Web<'a> {
    bar: &'a Bar,
    index: usize,
    /// Unit direction away from the node, in the gusset plane (2D) and in the world.
    d: V2,
    dw: V3,
    /// Half width of the member across its axis in the plane (of its knife plate, if any).
    half: f64,
    /// Hollow member: never bolted through, it takes a knife plate in a slotted end.
    knife: Option<Knife>,
}

/// Knife plate of a hollow web member: lapped on the gusset (bolts through both plates
/// only), welded into the slot along the two walls it passes.
struct Knife {
    /// Across the member in the plane, unit, and the outer wall faces along it at the plate.
    perp: V3,
    walls: (f64, f64),
    leg: f64,
}

fn node(k: &mut Kit, hosts: &[&Bar], webs: &[&Bar], first: usize) -> Result<(), ConnError> {
    let mut all: Vec<&Bar> = hosts.to_vec();
    all.extend(webs.iter().copied());
    let node = meet(&all)?;
    let host = hosts[0];
    let node = host.axis(host.local(node).x);
    k.node = node;
    // plane: host axis and the mean web direction
    let away: Vec<V3> = webs.iter().map(|w| w.end_near(node).2).collect();
    let mean = away.iter().fold(V3::default(), |s, d| s + *d);
    let side = across(if mean.len() > 1e-6 { mean } else { away[0] }, host.u);
    if !(side.len() > 0.5) {
        return Err(ConnError::new(
            "the members lie along the host: no gusset plane",
        ));
    }
    let nrm = host.u.cross(side).normalized();
    let x = host.u;
    let y = nrm.cross(x);
    let f = Frame {
        o: node,
        x,
        y,
        z: nrm,
    };
    for i in 0..webs.len() {
        if away[i].dot(nrm).abs() > 0.05 {
            return Err(ConnError(format!(
                "member {} leaves the gusset plane by {:.1} deg",
                first + i + 1,
                away[i].dot(nrm).abs().asin().to_degrees()
            )));
        }
        if away[i].dot(y) < 0.05 {
            return Err(ConnError(format!(
                "member {} is on the other side of the host: connect each side separately",
                first + i + 1
            )));
        }
    }
    // host faces as lines of the plane: y = y0 (first host), x = xf (corner host, gusset on
    // the side sx of it)
    let reach = |b: &Bar, d: V3| b.reach(v2(d.dot(b.ey), d.dot(b.ez)));
    let y0 = reach(host, y);
    let x_face = hosts.get(1).map(|h1| {
        let sx = if away.iter().map(|d| d.dot(x)).sum::<f64>() >= 0.0 {
            1.0
        } else {
            -1.0
        };
        let off = f.local(h1.axis(h1.local(node).x)).x;
        (sx, off + sx * reach(h1, x * sx))
    });
    let gap = k.p.gap.unwrap_or(k.cat.conn.gap);
    let thin = webs
        .iter()
        .map(|w| w.dims.thin())
        .fold(host.dims.thin(), f64::min);
    let tg = k.plate_t(
        webs.iter()
            .map(|w| w.dims.thin())
            .fold(0.008, f64::max)
            .min(0.02),
        thin,
    );
    let leg = k.leg(tg, host.dims.thin());
    let mut ws: Vec<Web> = webs
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let dw = away[i];
            let d = v2(dw.dot(x), dw.dot(y));
            let d = d * (1.0 / d.len().max(1e-12));
            let perp = nrm.cross(dw);
            let ps = v2(perp.dot(w.ey), perp.dot(w.ez));
            let mut half = w.reach(ps).max(w.reach(-ps));
            // the knife plate (gusset thickness) laps the gusset on the +normal side
            let knife = w.dims.hollow().then(|| {
                let q = w.local(node + nrm * tg);
                let ps = ps * (1.0 / ps.len().max(1e-12));
                let cuts = w.cut_line(v2(q.y, q.z), ps);
                let lo = cuts.iter().map(|c| c.0).fold(-w.dims.thin(), f64::min);
                let hi = cuts.iter().map(|c| c.1).fold(w.dims.thin(), f64::max);
                let leg = k.leg(tg, w.dims.thin());
                half = (-lo).max(hi) + 2.0 * leg;
                Knife {
                    perp: perp.normalized(),
                    walls: (lo, hi),
                    leg,
                }
            });
            Web {
                bar: w,
                index: first + i,
                d,
                dw,
                half,
                knife,
            }
        })
        .collect();
    ws.sort_by(|a, b| a.d.y.atan2(a.d.x).total_cmp(&b.d.y.atan2(b.d.x)));
    // bolts for each member on a line whose heads clear its section, then setbacks clear of
    // the faces, the heads and each other
    let mut sizes: Vec<BoltSize> = vec![];
    let mut lines: Vec<f64> = vec![];
    for w in &ws {
        let (size, u) = bolt_line(k, w, nrm, tg)?;
        sizes.push(size);
        lines.push(u);
    }
    let mut sb: Vec<f64> = vec![0.0; ws.len()];
    for (i, w) in ws.iter().enumerate() {
        let r = sizes[i].hex_r().max(sizes[i].w_od / 2.0) + k.cat.spacing.face_clear + lines[i].abs();
        let mut s = (y0 + gap + w.half * w.d.x.abs()) / w.d.y.max(1e-6);
        if let Some((sx, xf)) = x_face {
            let dx = w.d.x * sx;
            if dx > 1e-6 {
                s = s.max((xf * sx + gap + w.half * w.d.y.abs()) / dx);
            }
        }
        for (j, o) in ws.iter().enumerate() {
            if i == j {
                continue;
            }
            let ang =
                w.d.dot(o.d)
                    .clamp(-1.0, 1.0)
                    .acos()
                    .min(std::f64::consts::FRAC_PI_2);
            let ro = sizes[j].hex_r().max(sizes[j].w_od / 2.0) + lines[j].abs();
            let need = w.half.max(r) + o.half.max(ro) + gap;
            s = s.max(need / ang.sin().max(1e-3));
        }
        sb[i] = k.cat.round_up(s);
        // members meeting at a slight angle push the gusset past the member itself
        if sb[i] > (w.bar.t1 - w.bar.t0).abs() / 2.0 {
            return Err(ConnError(format!(
                "member {} meets another at too small an angle for one gusset",
                w.index + 1
            )));
        }
    }
    let mut pts: Vec<V2> = vec![];
    let mut groups = vec![];
    let mut ends = vec![];
    for (i, w) in ws.iter().enumerate() {
        let size = &sizes[i];
        let (e, p) = (k.cat.e_layout(size.hole), k.cat.p_layout(size.hole));
        let n = k.p.rows.unwrap_or(2);
        let s0 = sb[i] + e;
        let ss: Vec<f64> = (0..n).map(|j| s0 + j as f64 * p).collect();
        let lat = v2(-w.d.y, w.d.x);
        for s in [sb[i] - gap, ss[n - 1] + e] {
            for sg in [-1.0, 1.0] {
                pts.push(w.d * s + lat * (lines[i] + e * sg));
            }
        }
        // a hollow member ends past its bolts, clear of the nuts, over its knife plate
        let end = if w.knife.is_some() {
            let r = size.hex_r().max(size.w_od / 2.0) + k.cat.spacing.face_clear;
            k.cat.round_up(ss[n - 1] + e.max(r))
        } else {
            sb[i]
        };
        groups.push(ss);
        ends.push(end);
        k.trim(w.index, w.bar.end_near(node).0, end);
    }
    // the gusset runs along the host faces under the web members
    let mut xa = pts.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let mut xb = pts.iter().map(|p| p.x).fold(f64::MIN, f64::max);
    let yb = pts.iter().map(|p| p.y).fold(f64::MIN, f64::max);
    if let Some((sx, xf)) = x_face {
        if sx > 0.0 {
            xa = xf;
        } else {
            xb = xf;
        }
        pts.push(v2(xf, yb));
    }
    pts.push(v2(xa, y0));
    pts.push(v2(xb, y0));
    for p in pts.iter_mut() {
        p.y = p.y.max(y0);
        if let Some((sx, xf)) = x_face {
            p.x = if sx > 0.0 { p.x.max(xf) } else { p.x.min(xf) };
        }
    }
    let outline = hull(pts);
    let mut holes = vec![];
    for (i, w) in ws.iter().enumerate() {
        for s in &groups[i] {
            holes.push((w.d * *s + v2(-w.d.y, w.d.x) * lines[i], sizes[i].hole));
        }
    }
    let gp = k.plate(Role::Gusset, f, outline.clone(), holes, tg, -tg / 2.0);
    // welds along the host faces, both sides of the gusset
    let on = |p: &V2, q: &V2, test: &dyn Fn(&V2) -> bool| test(p) && test(q);
    let m = outline.len();
    for i in 0..m {
        let (p, q) = (outline[i], outline[(i + 1) % m]);
        let on_y0 = on(&p, &q, &|v: &V2| (v.y - y0).abs() < 1e-6);
        let on_x1 = x_face.is_some_and(|(_, xf)| on(&p, &q, &|v: &V2| (v.x - xf).abs() < 1e-6));
        if !(on_y0 || on_x1) {
            continue;
        }
        let a = f.at(v3(p.x, p.y, 0.0));
        let run = f.at(v3(q.x, q.y, 0.0)) - a;
        let len = run.len();
        if len < 1e-6 {
            continue;
        }
        let dir = run * (1.0 / len);
        let into = if on_y0 {
            y
        } else {
            x * x_face.map_or(1.0, |v| v.0)
        };
        k.weld_both_sides(a, dir, len, nrm, into, tg, leg);
    }
    // bolts across the plane through the gusset and each member
    for (i, w) in ws.iter().enumerate() {
        if let Some(kn) = &w.knife {
            knife(k, f, w, kn, &groups[i], ends[i], (gp, tg), &sizes[i])?;
            continue;
        }
        let perp = nrm.cross(w.dw).normalized();
        for s in &groups[i] {
            let c = node + w.dw * *s + perp * lines[i];
            let q = w.bar.local(c);
            let nn = v2(nrm.dot(w.bar.ey), nrm.dot(w.bar.ez));
            let nn = nn * (1.0 / nn.len().max(1e-12));
            // offset along the normal of the member's section origin, then of the cut points
            let off = (w.bar.at(q.x, 0.0, 0.0) - node).dot(nrm) + v2(q.y, q.z).dot(nn);
            let r = sizes[i].hex_r().max(sizes[i].w_od / 2.0);
            let (a, b) = stack(w.bar, v2(q.y, q.z), nn, r).unwrap_or((0.0, 0.0));
            let (lo, hi) = ((off + a).min(-tg / 2.0), (off + b).max(tg / 2.0));
            let o = c - nrm * (c - node).dot(nrm) + nrm * lo;
            k.bolt(&sizes[i], o, nrm, hi - lo, vec![gp], w.dw)?;
        }
    }
    k.note("web members are modelled on the gusset plane: they lap or straddle it".to_string());
    Ok(())
}

/// Knife plate of a hollow web member on the +normal face of the gusset `gp` (`tg` thick):
/// bolted to the gusset at `ss`, from there into the member's slotted `end`, welded along both
/// walls. The four slot fillets (throat 0.7 leg) develop the plate section at equal strength.
#[allow(clippy::too_many_arguments)]
fn knife(
    k: &mut Kit,
    f: Frame,
    w: &Web,
    kn: &Knife,
    ss: &[f64],
    end: f64,
    (gp, tg): (usize, f64),
    size: &BoltSize,
) -> Result<(), ConnError> {
    let (node, nrm) = (f.o, f.z);
    let flat = |p: V3| p - nrm * p.dot(nrm);
    let (tk, leg) = (tg, kn.leg);
    let need = tk * 2.0 * w.half * 3f64.sqrt() / (4.0 * 0.7 * leg);
    let lw = k.cat.round_up(need.max(k.cat.weld_len_min(leg)));
    let (s0, s1) = (ss[0] - k.cat.e_layout(size.hole), end + lw);
    let lat = v2(-w.d.y, w.d.x) * w.half;
    let outline = vec![
        w.d * s0 - lat,
        w.d * s1 - lat,
        w.d * s1 + lat,
        w.d * s0 + lat,
    ];
    let holes = ss.iter().map(|s| (w.d * *s, size.hole)).collect();
    let kp = k.plate(Role::Plate, f, outline, holes, tk, tg / 2.0);
    for s in ss {
        let o = node + flat(w.dw * *s) - nrm * (tg / 2.0);
        k.bolt(size, o, nrm, tg + tk, vec![gp, kp], w.dw)?;
    }
    for (x, sg) in [(kn.walls.0, -1.0), (kn.walls.1, 1.0)] {
        let c = node + flat(w.dw * end) + nrm * tg + kn.perp * x;
        k.weld_both_sides(c, w.dw, lw, nrm, kn.perp * sg, tk, leg);
    }
    k.note(format!(
        "member {}: hollow, slotted end over a {} mm knife plate, slot welds {} mm",
        w.index + 1,
        fmt_mm(tk),
        fmt_mm(lw)
    ));
    Ok(())
}

/// Bolt size and lateral line (m, along `nrm × dw`) for a web member: the line nearest the
/// axis whose head and nut stand clear of the section, a smaller bolt when none does.
fn bolt_line(k: &Kit, w: &Web, nrm: V3, tg: f64) -> Result<(BoltSize, f64), ConnError> {
    let mut size = member_bolt(k, w.bar, w.half)?;
    if w.knife.is_some() {
        return Ok((size, 0.0));
    }
    let bar = w.bar;
    let sec = |v: V3| {
        let p = v2(v.dot(bar.ey), v.dot(bar.ez));
        p * (1.0 / p.len().max(1e-12))
    };
    let nn = sec(nrm);
    let ls = sec(nrm.cross(w.dw));
    // section point of the line through the node at lateral offset u, and its offset along nrm
    let q = bar.local(k.node);
    let p0 = v2(q.y, q.z) - nn * v2(q.y, q.z).dot(nn);
    let off = (bar.at(q.x, 0.0, 0.0) - k.node).dot(nrm);
    let span = |u: f64, r: f64| stack(bar, p0 + ls * u, nn, r).map(|(a, b)| (off + a, off + b));
    let step = k.cat.spacing.round.max(1e-3);
    loop {
        let r = size.hex_r().max(size.w_od / 2.0);
        let e = k.cat.spacing.e2_min * size.hole;
        // the checker's test: head and nut samples must stay out of the member's material
        let tm = (bar.t0 + bar.t1) / 2.0;
        let fits = |u: f64| -> bool {
            let (Some(c), Some((lo, hi))) = (span(u, 0.0), span(u, r)) else { return false };
            // a face sloping under the head would leave it bearing on an edge: bound the gap
            if [-e, e].iter().any(|d| span(u + d, 0.0).is_none()) || hi - lo > c.1 - c.0 + 2.0 * r {
                return false;
            }
            let (lo, hi) = (lo.min(-tg / 2.0), hi.max(tg / 2.0));
            let heads = [(lo - size.k - size.w_t, lo), (hi, hi + size.w_t + size.m)];
            !heads.iter().any(|(z0, z1)| {
                [*z0, (z0 + z1) / 2.0, *z1].iter().any(|z| {
                    (0..8).any(|i| {
                        let a = 0.98 * r * (std::f64::consts::PI * i as f64 / 4.0).cos();
                        let p = p0 + ls * (u + a) + nn * (z - off);
                        bar.holds(bar.at(tm, p.x, p.y), 2e-4)
                    })
                })
            })
        };
        let n = (w.half / step).ceil() as i64;
        let found = (0..=n)
            .flat_map(|i| [i, -i])
            .map(|i| i as f64 * step)
            .find(|u| fits(*u));
        if let Some(u) = found {
            return Ok((size, u));
        }
        match k.cat.smaller(size.d).filter(|_| k.p.bolt.is_none()) {
            Some(s) => size = s.clone(),
            None => {
                return Err(ConnError(format!(
                    "member {}: no {} bolt line clears its section",
                    w.index + 1,
                    size.name()
                )))
            }
        }
    }
}

/// Material of the section across the band of half-width `r` either side of the line through
/// `p` along unit `nn`, as (min, max) of the line parameter; None when the line misses it.
fn stack(bar: &Bar, p: V2, nn: V2, r: f64) -> Option<(f64, f64)> {
    let ls = v2(-nn.y, nn.x);
    let mut t: Vec<f64> = bar.cut_line(p, nn).into_iter().flat_map(|(a, b)| [a, b]).collect();
    if t.is_empty() {
        return None;
    }
    for d in [-r, r] {
        t.extend(bar.cut_line(p + ls * d, nn).into_iter().flat_map(|(a, b)| [a, b]));
    }
    t.extend(bar.outer.iter().filter(|v| (**v - p).dot(ls).abs() <= r).map(|v| (*v - p).dot(nn)));
    Some((t.iter().cloned().fold(f64::MAX, f64::min), t.iter().cloned().fold(f64::MIN, f64::max)))
}

/// Bolt for a web member: by its depth, small enough for the edge distance across its width.
fn member_bolt(k: &Kit, bar: &Bar, half: f64) -> Result<BoltSize, ConnError> {
    let mut size = k.bolt_size(k.cat.bolt_for_depth(bar.dims.h.max(bar.dims.b)).min(0.024))?;
    loop {
        if half >= k.cat.spacing.e2_min * size.hole || k.p.bolt.is_some() {
            return Ok(size);
        }
        match k.cat.smaller(size.d) {
            Some(s) => size = s.clone(),
            None => return Ok(size),
        }
    }
}
