//! Exact IFC bodies of smooth sweeps. A centred circle or tube profile -> IfcSweptDiskSolid
//! (twist and roll do not change it); a profile whose placement against the directrix keeps
//! one reference direction (fixed frame, or the rotation-minimizing frame of a planar path)
//! -> IfcFixedReferenceSweptAreaSolid. Directrix: lines and planar arcs -> IfcIndexedPolyCurve
//! (exact); one spline or NURBS -> IfcBSplineCurveWithKnots or its rational form (exact);
//! a path with a helix -> C1 cubic B-spline, 16 pieces per turn (deviation ~1e-7 of the
//! radius; IFC4 has no helix curve). Kinks, varying twist or scale on a non-circular profile,
//! mirrored planes and IFC2X3 output -> None: the caller tessellates.

use std::cell::Cell;
use std::f64::consts::{PI, TAU};

use ddc_geom::arc::BulgeRing;
use ddc_geom::frame3::{self, FrameMode, Geo as PGeo, Smooth};
use ddc_geom::{v2, V2, V3};
use ddc_model::{SolidFrame, SolidSection};

use super::super::{list, r, real, reals, s, Ctx, NONE};

thread_local! {
    static TESS_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with exact sweeps off (`on`): schemas without these entities get tessellation.
pub(crate) fn tessellated<R>(on: bool, f: impl FnOnce() -> R) -> R {
    let old = TESS_ONLY.with(|t| t.replace(on));
    let out = f();
    TESS_ONLY.with(|t| t.set(old));
    out
}

/// True inside [`tessellated`]`(true, ..)`: no advanced geometry is written.
pub(crate) fn tess_only() -> bool {
    TESS_ONLY.with(Cell::get)
}

/// Station sampling for the frame checks, metres and radians.
const CHORD: f64 = 1e-3;
const ANGLE: f64 = PI / 36.0;
/// Constant-frame tolerance on unit vectors.
const SAME: f64 = 1e-7;

/// Radius of a ring that is a full circle centred at the profile origin.
fn circle_r(ring: &BulgeRing) -> Option<f64> {
    let n = ring.pts.len();
    if n < 2 || ring.bulges.len() != n {
        return None;
    }
    let r0 = ring.pts[0].len();
    let mut turn = 0.0;
    for i in 0..n {
        let (a, b, bu) = (ring.pts[i], ring.pts[(i + 1) % n], ring.bulges[i]);
        if bu.abs() < 1e-12 || (a.len() - r0).abs() > 1e-9 * r0.max(1.0) {
            return None;
        }
        let th = 4.0 * bu.atan();
        let ch = (b - a).len();
        let rr = ch / (2.0 * (th / 2.0).sin().abs());
        let h = rr * (th / 2.0).cos();
        let d = (b - a) * (1.0 / ch);
        let c = (a + b) * 0.5 + v2(-d.y, d.x) * (h * th.signum());
        if c.len() > 1e-9 * r0.max(1.0) || (rr - r0).abs() > 1e-9 * r0.max(1.0) {
            return None;
        }
        turn += th;
    }
    ((turn.abs() - TAU).abs() < 1e-6 && r0 > 0.0).then_some(r0)
}

/// Exact body item of a smooth sweep, or None to tessellate.
pub(super) fn smooth_body(
    c: &mut Ctx,
    f: &SolidFrame,
    profile: &SolidSection,
    sm: &Smooth,
) -> Option<u32> {
    if TESS_ONLY.with(Cell::get) {
        return None;
    }
    let sts = frame3::stations(sm, CHORD, ANGLE).ok()?;
    if sts.st.iter().any(|q| q.kink) {
        return None;
    }
    let geos = sm.geos().ok()?;
    if let (Some(ro), true) = (circle_r(&profile.outer), sm.scale.is_const()) {
        let k = sm.scale.at(0.0);
        let inner = match profile.holes.as_slice() {
            [] => Some(None),
            [h] => circle_r(h).map(Some),
            _ => None,
        };
        if let Some(ri) = inner {
            let curve = directrix(c, f, &geos)?;
            let ri = ri.map_or(NONE.to_string(), |x| real(x * k));
            return Some(c.s.add(
                "IFCSWEPTDISKSOLID",
                &[r(curve), real(ro * k), ri, s(NONE), s(NONE)],
            ));
        }
    }
    if f.det() <= 0.0 || !sm.twist.is_const() || !sm.scale.is_const() {
        return None;
    }
    let (reference, m) = fixed_reference(sm, &sts.st)?;
    let map = |q: V2| v2(m[0] * q.x + m[1] * q.y, m[2] * q.x + m[3] * q.y);
    let ring = |b: &BulgeRing| BulgeRing {
        pts: b.pts.iter().map(|q| map(*q)).collect(),
        bulges: b.bulges.clone(),
    };
    let placed = SolidSection {
        outer: ring(&profile.outer),
        holes: profile.holes.iter().map(ring).collect(),
    };
    let curve = directrix(c, f, &geos)?;
    let prof = super::profile(c, &placed);
    let o = c.point3(0.0, 0.0, 0.0);
    let pos = c.axis3(o, None);
    let rd = super::dir(c, f.dir(reference));
    Some(c.s.add(
        "IFCFIXEDREFERENCESWEPTAREASOLID",
        &[r(prof), r(pos), r(curve), s(NONE), s(NONE), r(rd)],
    ))
}

/// Reference direction R and the constant map M (row-major 2×2) from profile (u, v) to the
/// IFC profile axes x = R projected on the section plane, y = t × x; None when M varies.
fn fixed_reference(sm: &Smooth, st: &[frame3::Station]) -> Option<(V3, [f64; 4])> {
    let first = st.first()?;
    let mut cand: Vec<V3> = vec![];
    if let FrameMode::Fixed(up) = sm.frame {
        cand.push(up);
    }
    // a planar path: its normal is transported by the rotation-minimizing frame unchanged
    let mut best = (0.0, None);
    for q in st {
        for w in [first.t.cross(q.t), (q.p - first.p).cross(first.t)] {
            let l = w.len();
            if l > best.0 {
                best = (l, Some(w * (1.0 / l)));
            }
        }
    }
    if let (l, Some(nrm)) = best {
        if l > 1e-9 {
            cand.push(nrm);
        }
    }
    cand.extend([first.v, first.u]);
    cand.into_iter().find_map(|rf| {
        let mut m0: Option<[f64; 4]> = None;
        for q in st {
            let x = rf - q.t * rf.dot(q.t);
            let lx = x.len();
            if lx < 1e-6 {
                return None;
            }
            let x = x * (1.0 / lx);
            let y = q.t.cross(x);
            let k = q.scale;
            let m = [
                q.u.dot(x) * k,
                q.v.dot(x) * k,
                q.u.dot(y) * k,
                q.v.dot(y) * k,
            ];
            match m0 {
                None => m0 = Some(m),
                Some(a) if a.iter().zip(&m).all(|(a, b)| (a - b).abs() < SAME) => {}
                Some(_) => return None,
            }
        }
        let m = m0?;
        (m[0] * m[3] - m[1] * m[2] > 0.0).then_some((rf, m))
    })
}

/// Directrix in storey coordinates.
fn directrix(c: &mut Ctx, f: &SolidFrame, geos: &[PGeo]) -> Option<u32> {
    let at = |p: V3| f.at(p);
    let planar = |g: &PGeo| match g {
        PGeo::Line { .. } => true,
        PGeo::Circ { rise, .. } => rise.abs() < 1e-12,
        PGeo::Nurbs { .. } => false,
    };
    if geos.iter().all(planar) {
        return Some(poly(c, geos, &at));
    }
    if let [PGeo::Nurbs { cu, u0, du }] = geos {
        let k = cu.knots();
        let d = cu.degree();
        let (lo, hi) = (k[d], k[k.len() - d - 1]);
        if (lo - u0).abs() < 1e-12 && (hi - (u0 + du)).abs() < 1e-12 {
            let ctrl: Vec<V3> = cu.ctrl().iter().map(|p| at(*p)).collect();
            let w = cu.weights();
            let rational = w.iter().any(|x| (x - 1.0).abs() > 1e-15);
            return Some(bspline(c, d, &ctrl, k, rational.then_some(w)));
        }
    }
    hermite(c, geos, &at)
}

fn poly(c: &mut Ctx, geos: &[PGeo], at: &dyn Fn(V3) -> V3) -> u32 {
    let mut pts: Vec<V3> = vec![];
    let mut segs: Vec<String> = vec![];
    let push = |p: V3, pts: &mut Vec<V3>| -> usize {
        if let Some(i) = pts.iter().position(|q| (*q - p).len() < 1e-9) {
            if i == 0 || i + 1 == pts.len() {
                return i + 1;
            }
        }
        pts.push(p);
        pts.len()
    };
    let mut i = push(at(geos[0].at(0.0)[0]), &mut pts);
    for g in geos {
        match g {
            PGeo::Circ { theta, .. } => {
                let n = (theta.abs() / (0.75 * PI)).ceil().max(1.0) as usize;
                for k in 0..n {
                    let t = |j: f64| (k as f64 + j) / n as f64;
                    let m = push(at(g.at(t(0.5))[0]), &mut pts);
                    let j = push(at(g.at(t(1.0))[0]), &mut pts);
                    segs.push(format!("IFCARCINDEX(({i},{m},{j}))"));
                    i = j;
                }
            }
            _ => {
                let j = push(at(g.at(1.0)[0]), &mut pts);
                segs.push(format!("IFCLINEINDEX(({i},{j}))"));
                i = j;
            }
        }
    }
    let coords = pts
        .iter()
        .map(|p| reals(&[p.x, p.y, p.z]))
        .collect::<Vec<_>>()
        .join(",");
    let pl = c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({coords})"), s(NONE)]);
    c.s.add(
        "IFCINDEXEDPOLYCURVE",
        &[r(pl), format!("({})", segs.join(",")), s(".F.")],
    )
}

/// B-spline with explicit knots (one entry per distinct knot plus multiplicities).
fn bspline(c: &mut Ctx, degree: usize, ctrl: &[V3], knots: &[f64], w: Option<&[f64]>) -> u32 {
    let mut ks: Vec<f64> = vec![];
    let mut mult: Vec<usize> = vec![];
    for k in knots {
        match ks.last() {
            Some(l) if (k - l).abs() < 1e-15 => *mult.last_mut().unwrap() += 1,
            _ => {
                ks.push(*k);
                mult.push(1);
            }
        }
    }
    let pts: Vec<u32> = ctrl.iter().map(|p| c.point3(p.x, p.y, p.z)).collect();
    let ints = |v: &[usize]| {
        format!("({})", v.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(","))
    };
    let mut args = vec![
        degree.to_string(),
        list(&pts),
        s(".UNSPECIFIED."),
        s(".F."),
        s(".F."),
        ints(&mult),
        reals(&ks),
        s(".UNSPECIFIED."),
    ];
    match w {
        Some(w) => {
            args.push(reals(w));
            c.s.add("IFCRATIONALBSPLINECURVEWITHKNOTS", &args)
        }
        None => c.s.add("IFCBSPLINECURVEWITHKNOTS", &args),
    }
}

/// Cubic Hermite pieces as one C1 B-spline (Bezier pieces, triple inner knots).
fn hermite(c: &mut Ctx, geos: &[PGeo], at: &dyn Fn(V3) -> V3) -> Option<u32> {
    let mut ctrl: Vec<V3> = vec![];
    let mut pieces = 0usize;
    for g in geos {
        let ts: Vec<f64> = match g {
            PGeo::Line { .. } => vec![0.0, 1.0],
            PGeo::Circ { theta, .. } => {
                let n = (theta.abs() / (TAU / 16.0)).ceil().max(1.0) as usize;
                (0..=n).map(|k| k as f64 / n as f64).collect()
            }
            PGeo::Nurbs { .. } => g.params(CHORD, PI / 16.0),
        };
        for w in ts.windows(2) {
            let h = w[1] - w[0];
            let [p0, d0, _] = g.at(w[0]);
            let [p1, d1, _] = g.at(w[1]);
            if ctrl.is_empty() {
                ctrl.push(at(p0));
            }
            ctrl.push(at(p0 + d0 * (h / 3.0)));
            ctrl.push(at(p1 - d1 * (h / 3.0)));
            ctrl.push(at(p1));
            pieces += 1;
        }
    }
    if pieces == 0 {
        return None;
    }
    let mut knots = vec![0.0; 4];
    for k in 1..pieces {
        knots.extend([k as f64; 3]);
    }
    knots.extend([pieces as f64; 4]);
    Some(bspline(c, 3, &ctrl, &knots, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    #[test]
    fn circle_rings_are_recognised() {
        let full = BulgeRing {
            pts: vec![v2(0.5, 0.0), v2(-0.5, 0.0)],
            bulges: vec![1.0, 1.0],
        };
        assert_eq!(circle_r(&full), Some(0.5));
        let off = BulgeRing {
            pts: vec![v2(1.5, 0.0), v2(0.5, 0.0)],
            bulges: vec![1.0, 1.0],
        };
        assert_eq!(circle_r(&off), None);
        let square = BulgeRing {
            pts: vec![v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0)],
            bulges: vec![0.0; 3],
        };
        assert_eq!(circle_r(&square), None);
    }
}
