//! NURBS patches as exact faces: freeform surfaces (an open sheet) and shells (two offset
//! faces and ruled sides stitched into one closed shell). A patch with clamped knots is
//! bounded by its four iso curves; a side whose control row collapses to a point is a pole,
//! written as a degenerate edge (the collapsed curve on one vertex); two coinciding opposite
//! sides form a seam.

use ddc_geom::math::V3;
use ddc_geom::nurbs::{self, Curve as NCurve, Surface as NSurf};

use super::{open_edge, Body, Curve, Edge, Face, Surface};

/// The degenerate edge of a pole side at vertex `p`.
fn pole_edge(b: &mut Body, p: u32, c: &NCurve) -> u32 {
    b.edges.push(Edge {
        v: [p, p],
        curve: Curve::Nurbs(c.clone()),
        sense: true,
    });
    (b.edges.len() - 1) as u32
}

fn clamped(k: &[f64], p: usize) -> bool {
    k.len() > 2 * (p + 1) - 1 && k[..=p].iter().all(|&x| x == k[0]) && k[k.len() - p - 1..].iter().all(|&x| x == k[k.len() - 1])
}

/// Control row (fixed u index `i`) or column (fixed v index `j`) as a curve.
fn side(s: &NSurf, fixed_u: bool, idx: usize) -> Result<NCurve, String> {
    let (nu, nv) = (s.nu(), s.nv());
    let ids: Vec<usize> = if fixed_u { (0..nv).map(|j| idx * nv + j).collect() } else { (0..nu).map(|i| i * nv + idx).collect() };
    let (deg, knots) = if fixed_u { (s.deg_v(), s.knots_v()) } else { (s.deg_u(), s.knots_u()) };
    NCurve::new(
        deg,
        knots.to_vec(),
        ids.iter().map(|&k| s.ctrl()[k]).collect(),
        ids.iter().map(|&k| s.weights()[k]).collect(),
    )
    .map_err(|e: nurbs::NurbsError| e.to_string())
}

fn same_pts(a: &NCurve, b: &NCurve, tol: f64) -> bool {
    a.ctrl().len() == b.ctrl().len()
        && a.ctrl().iter().zip(b.ctrl()).all(|(p, q)| (*p - *q).len() <= tol)
        && a.weights().iter().zip(b.weights()).all(|(p, q)| (p - q).abs() <= 1e-12 * p.abs().max(1.0))
}

fn pole(c: &NCurve, tol: f64) -> bool {
    let p0 = c.ctrl()[0];
    c.ctrl().iter().all(|p| (*p - p0).len() <= tol)
}

/// One patch as an open sheet (closed when seams and poles leave no free edge).
pub fn sheet(s: &NSurf) -> Result<Body, String> {
    sheets(std::slice::from_ref(s))
}

/// Patches as one body, one open shell each.
pub fn sheets(list: &[NSurf]) -> Result<Body, String> {
    let mut b = Body::default();
    for s in list {
        if !clamped(s.knots_u(), s.deg_u()) || !clamped(s.knots_v(), s.deg_v()) {
            return Err("a NURBS patch needs clamped knot vectors to give its boundary".into());
        }
        let size = s.ctrl().iter().map(|p| p.len()).fold(0.0, f64::max);
        let tol = 1e-9 * (1.0 + size);
        let (nu, nv) = (s.nu(), s.nv());
        // sides in loop order: v = v0 (u0 -> u1), u = u1 (v0 -> v1), v = v1, u = u0
        let curves = [side(s, false, 0)?, side(s, true, nu - 1)?, side(s, false, nv - 1)?, side(s, true, 0)?];
        let corner_pts: [V3; 4] = [s.ctrl()[0], s.ctrl()[(nu - 1) * nv], s.ctrl()[nu * nv - 1], s.ctrl()[nv - 1]];
        let mut corner = [0u32; 4];
        for (k, p) in corner_pts.iter().enumerate() {
            corner[k] = match (0..k).find(|&q| (corner_pts[q] - *p).len() <= tol) {
                Some(q) => corner[q],
                None => {
                    b.verts.push(*p);
                    (b.verts.len() - 1) as u32
                }
            };
        }
        let mut edge_of: [Option<(u32, bool)>; 4] = [None; 4];
        for k in 0..4 {
            let fwd = k < 2;
            let (a, z) = (corner[k], corner[(k + 1) % 4]);
            if pole(&curves[k], tol) {
                edge_of[k] = Some((pole_edge(&mut b, a, &curves[k]), fwd));
                continue;
            }
            // a seam: the opposite side is the same curve, used the other way
            let opp = (k + 2) % 4;
            let shared = edge_of[opp].filter(|&(e, _)| !b.edges[e as usize].degenerate() && same_pts(&curves[k], &curves[opp], tol));
            if let Some((e, _)) = shared {
                edge_of[k] = Some((e, fwd));
                continue;
            }
            let v = if fwd { [a, z] } else { [z, a] };
            b.edges.push(Edge {
                v,
                curve: Curve::Nurbs(curves[k].clone()),
                sense: true,
            });
            edge_of[k] = Some(((b.edges.len() - 1) as u32, fwd));
        }
        let lp: Vec<(u32, bool)> = edge_of.iter().flatten().copied().collect();
        if lp.is_empty() {
            return Err("a NURBS patch with no boundary".into());
        }
        b.faces.push(Face {
            surface: Surface::Nurbs(s.clone()),
            sense: true,
            loops: vec![lp],
            rgb: None,
        });
        b.shells.push(vec![(b.faces.len() - 1) as u32]);
    }
    let mut uses = vec![[0u32; 2]; b.edges.len()];
    for f in &b.faces {
        for &(e, fwd) in f.loops.iter().flatten() {
            uses[e as usize][usize::from(!fwd)] += 1;
        }
    }
    b.closed = !uses.is_empty() && open_edge(&b.edges, &uses).is_none();
    if b.closed {
        b.shells = vec![(0..b.faces.len() as u32).collect()];
    }
    Ok(b)
}

/// The four sides of a clamped patch in loop order (each running with its curve parameter)
/// and its corners at (u0, v0), (u1, v0), (u1, v1), (u0, v1).
fn boundary(s: &NSurf) -> Result<([NCurve; 4], [V3; 4]), String> {
    if !clamped(s.knots_u(), s.deg_u()) || !clamped(s.knots_v(), s.deg_v()) {
        return Err("a NURBS patch needs clamped knot vectors to give its boundary".into());
    }
    let (nu, nv) = (s.nu(), s.nv());
    let curves = [side(s, false, 0)?, side(s, true, nu - 1)?, side(s, false, nv - 1)?, side(s, true, 0)?];
    let corners = [s.ctrl()[0], s.ctrl()[(nu - 1) * nv], s.ctrl()[nu * nv - 1], s.ctrl()[nv - 1]];
    Ok((curves, corners))
}

fn lerp(d: (f64, f64), t: f64) -> f64 {
    d.0 + (d.1 - d.0) * t
}

/// The two curves trace the same points at matching normalized parameters (`rev`: one of
/// them backwards). Knot insertion and affine reparameterisation keep this true.
fn same_geom(a: &NCurve, b: &NCurve, rev: bool, tol: f64) -> bool {
    const T: [f64; 7] = [0.0, 0.13, 0.29, 0.5, 0.71, 0.87, 1.0];
    let (da, db) = (a.domain(), b.domain());
    T.iter().all(|&t| {
        let tb = if rev { 1.0 - t } else { t };
        (a.point(lerp(da, t)) - b.point(lerp(db, tb))).len() <= tol
    })
}

fn vert(b: &mut Body, p: V3, tol: f64) -> u32 {
    match b.verts.iter().position(|q| (*q - p).len() <= tol) {
        Some(i) => i as u32,
        None => {
            b.verts.push(p);
            (b.verts.len() - 1) as u32
        }
    }
}

/// An edge already in `b` from `s` to `e` along `c` (true) or from `e` to `s` (false).
fn find_edge(b: &Body, s: u32, e: u32, c: &NCurve, tol: f64) -> Option<(u32, bool)> {
    b.edges.iter().enumerate().find_map(|(k, ed)| {
        let Curve::Nurbs(g) = &ed.curve else { return None };
        if ed.degenerate() {
            return None;
        }
        if ed.v == [s, e] && same_geom(g, c, false, tol) {
            Some((k as u32, true))
        } else if ed.v == [e, s] && same_geom(g, c, true, tol) {
            Some((k as u32, false))
        } else {
            None
        }
    })
}

/// Patches with their face sense stitched into one shell: corners and boundary curves that
/// coincide are shared, a pole side is a degenerate edge of its face. Closed when every edge
/// is used once each way (a degenerate one once).
fn stitch(list: &[(NSurf, bool)]) -> Result<Body, String> {
    let size = list.iter().flat_map(|f| f.0.ctrl()).map(|p| p.len()).fold(0.0, f64::max);
    let tol = 1e-6 * (1.0 + size);
    let mut b = Body::default();
    for (s, sense) in list {
        let (curves, corners) = boundary(s)?;
        let corner = corners.map(|p| vert(&mut b, p, tol));
        let mut lp: Vec<(u32, bool)> = vec![];
        for k in 0..4 {
            // sides 0 and 1 run with their curves in the loop, sides 2 and 3 against them
            let fwd = k < 2;
            let (a, z) = (corner[k], corner[(k + 1) % 4]);
            if pole(&curves[k], tol) {
                if a != z {
                    return Err("a pole side whose corners do not meet".into());
                }
                lp.push((pole_edge(&mut b, a, &curves[k]), fwd));
                continue;
            }
            let (cs, ce) = if fwd { (a, z) } else { (z, a) };
            let used = match find_edge(&b, cs, ce, &curves[k], tol) {
                Some((e, same)) => (e, same == fwd),
                None => {
                    b.edges.push(Edge {
                        v: [cs, ce],
                        curve: Curve::Nurbs(curves[k].clone()),
                        sense: true,
                    });
                    ((b.edges.len() - 1) as u32, fwd)
                }
            };
            lp.push(used);
        }
        if lp.is_empty() {
            return Err("a NURBS patch with no boundary".into());
        }
        if !sense {
            // seen against the reversed normal the loop runs the other way
            lp.reverse();
            lp.iter_mut().for_each(|x| x.1 = !x.1);
        }
        b.faces.push(Face {
            surface: Surface::Nurbs(s.clone()),
            sense: *sense,
            loops: vec![lp],
            rgb: None,
        });
    }
    let mut uses = vec![[0u32; 2]; b.edges.len()];
    for f in &b.faces {
        for &(e, fwd) in f.loops.iter().flatten() {
            uses[e as usize][usize::from(!fwd)] += 1;
        }
    }
    b.closed = !uses.is_empty() && open_edge(&b.edges, &uses).is_none();
    b.shells = vec![(0..b.faces.len() as u32).collect()];
    Ok(b)
}

/// Direction out of the patch across side `k` (loop order) at the side's middle.
fn outward(s: &NSurf, k: usize) -> V3 {
    let ((u0, u1), (v0, v1)) = (s.domain_u(), s.domain_v());
    let (um, vm) = (0.5 * (u0 + u1), 0.5 * (v0 + v1));
    let (u, v, du, dv) = match k {
        0 => (um, v0, 0.0, -1.0),
        1 => (u1, vm, 1.0, 0.0),
        2 => (um, v1, 0.0, 1.0),
        _ => (u0, vm, -1.0, 0.0),
    };
    let d = s.derivs(u, v, 1);
    d[1][0] * du + d[0][1] * dv
}

/// Parameters at the knot breaks and four steps inside every span.
fn grid(knots: &[f64], p: usize, n: usize) -> Vec<f64> {
    let mut br: Vec<f64> = knots[p..=n].to_vec();
    br.dedup();
    let mut out = vec![br[0]];
    for w in br.windows(2) {
        out.extend((1..=4).map(|k| w[0] + (w[1] - w[0]) * k as f64 / 4.0));
    }
    out
}

/// The control net of `s` under the affine map `f`, weights kept: a rational patch maps exactly.
fn mapped(s: &NSurf, f: impl Fn(V3) -> V3) -> Option<NSurf> {
    NSurf::new(
        s.deg_u(),
        s.deg_v(),
        s.knots_u().to_vec(),
        s.knots_v().to_vec(),
        s.nu(),
        s.nv(),
        s.ctrl().iter().map(|&p| f(p)).collect(),
        s.weights().to_vec(),
    )
    .ok()
}

/// The exact offset of `s` by `d` along its normal when the patch lies on a plane, a sphere or
/// a circular cylinder: the offset is then a translation, a homothety about the centre or a
/// scaling across the axis, applied to the control net. A collapsed pole row stays collapsed.
/// Returns the surface and its largest deviation from S + d n on the sample grid; None for
/// any other patch.
fn quadric_offset(s: &NSurf, d: f64) -> Option<(NSurf, f64)> {
    let us = grid(s.knots_u(), s.deg_u(), s.nu());
    let vs = grid(s.knots_v(), s.deg_v(), s.nv());
    let pn: Vec<(f64, f64, V3, V3)> = us
        .iter()
        .flat_map(|&u| vs.iter().map(move |&v| (u, v)))
        .filter_map(|(u, v)| {
            // regular points only: at a pole the normal is taken off the point
            let dd = s.derivs(u, v, 1);
            let (a, b) = (dd[1][0], dd[0][1]);
            let (la, lb, n) = (a.len(), b.len(), a.cross(b));
            let regular = la > 1e-9 * lb && lb > 1e-9 * la && n.len() > 1e-6 * la * lb;
            regular.then(|| (u, v, dd[0][0], n * (1.0 / n.len())))
        })
        .collect();
    let size = s.ctrl().iter().map(|p| p.len()).fold(0.0, f64::max);
    let tol = 1e-9 * (1.0 + size + d.abs());
    let &(_, _, p1, n1) = pn.first()?;
    let &(_, _, p2, n2) = pn.iter().max_by(|a, b| (a.3 - n1).len().total_cmp(&(b.3 - n1).len()))?;
    let dn = n2 - n1;
    let o = if dn.len() <= 1e-12 {
        let t = n1 * d;
        mapped(s, |q| q + t)?
    } else {
        // centre of curvature c = p + r n, the same r at every point of a sphere
        let r = (p1 - p2).dot(dn) / dn.dot(dn);
        let k = (r - d) / r;
        if !(k > 0.0) {
            return None;
        }
        let c = p1 + n1 * r;
        if pn.iter().all(|&(_, _, p, n)| (p + n * r - c).len() <= tol) {
            mapped(s, |q| c + (q - c) * k)?
        } else {
            // a cylinder: every normal is square to the axis, the centres lie on it
            let ax = pn.iter().map(|x| n1.cross(x.3)).max_by(|a, b| a.len().total_cmp(&b.len()))?;
            if ax.len() <= 1e-9 {
                return None;
            }
            let ax = ax * (1.0 / ax.len());
            let perp = |x: V3| x - ax * x.dot(ax);
            let r = perp(p1 - p2).dot(dn) / dn.dot(dn);
            let k = (r - d) / r;
            let c = p1 + n1 * r;
            let on_axis = pn.iter().all(|&(_, _, p, n)| n.dot(ax).abs() <= 1e-9 && perp(p + n * r - c).len() <= tol);
            if !(k > 0.0) || !on_axis {
                return None;
            }
            mapped(s, |q| {
                let x = q - c;
                c + ax * x.dot(ax) + perp(x) * k
            })?
        }
    };
    let dev = pn
        .iter()
        .map(|&(u, v, p, n)| (o.point(u, v) - (p + n * d)).len())
        .fold(0.0, f64::max);
    (dev <= tol).then_some((o, dev))
}

/// A patch thickened between offsets `a` and `b` along its normal as one closed shell: the
/// two offset faces (exact on a plane, sphere or cylinder patch, else refit within `tol` of
/// the true offset or an error; an offset of 0 is the patch itself) and a ruled side face along each free side (none on a pole or a seam). Returns
/// the body and the largest offset deviation, m.
pub fn shell(s: &NSurf, a: f64, b: f64, tol: f64) -> Result<(Body, f64), String> {
    let (lo, hi) = (a.min(b), a.max(b));
    if !(hi - lo > 0.0) {
        return Err("a shell needs a positive thickness".into());
    }
    let ne = |e: nurbs::NurbsError| e.to_string();
    let (base, _) = boundary(s)?;
    let size = s.ctrl().iter().map(|p| p.len()).fold(0.0, f64::max);
    let ptol = 1e-9 * (1.0 + size);
    let mut off: Vec<(NSurf, f64)> = vec![];
    for d in [lo, hi] {
        if d == 0.0 {
            off.push((s.clone(), 0.0));
            continue;
        }
        nurbs::check_offset(s, d).map_err(ne)?;
        // planes, spheres and cylinders offset exactly; other patches are refit
        if let Some(o) = quadric_offset(s, d) {
            off.push(o);
            continue;
        }
        let o = nurbs::offset(s, d, tol).map_err(ne)?;
        if !o.converged {
            return Err(format!("the offset at {d:.4} m stays {:.2} mm off", o.max_dev * 1000.0));
        }
        off.push((o.surface, o.max_dev));
    }
    let dev = off[0].1.max(off[1].1);
    let (cl, _) = boundary(&off[0].0)?;
    let (ch, _) = boundary(&off[1].0)?;
    // the face toward the larger offset has the natural normal outward
    let mut faces = vec![(off[0].0.clone(), false), (off[1].0.clone(), true)];
    for k in 0..4 {
        let seam = same_geom(&base[k], &base[(k + 2) % 4], false, ptol);
        if pole(&base[k], ptol) || seam {
            continue;
        }
        let r = nurbs::ruled(&cl[k], &ch[k]).map_err(ne)?;
        let ((t0, t1), (s0, s1)) = (r.domain_u(), r.domain_v());
        let n = r
            .normal(0.5 * (t0 + t1), 0.5 * (s0 + s1))
            .ok_or("a shell side face is degenerate")?;
        faces.push((r, n.dot(outward(s, k)) > 0.0));
    }
    let body = stitch(&faces)?;
    if !body.closed {
        return Err("the shell faces do not close".into());
    }
    Ok((body, dev))
}
