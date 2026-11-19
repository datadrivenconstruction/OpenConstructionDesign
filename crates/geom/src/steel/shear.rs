//! Shear connections: beam to column or girder (fin plate or flexible end plate), stair
//! stringer to its support, purlin cleat on a rafter. Plates follow the member frames, so a
//! skewed or sloping member gets a plate cut to the support face, not a square one.

use super::bar::{across, hit_plane, meet, Bar, Dims, Face, Kind};
use super::catalog::{fmt_mm, BoltSize};
use super::kit::{centred, rect, rect2, Kit};
use super::part::{frame_from, Role};
use super::ConnError;
use crate::gsolid::Frame;
use crate::math::{v2, v3, V3};

/// Where a member is bolted through: web centre (section y), thickness through, centre and
/// clear depth of the bolting zone (section z), and the side without flanges, if any.
#[derive(Clone, Copy, Debug)]
pub(super) struct Web {
    pub y: f64,
    pub t: f64,
    pub zc: f64,
    pub hw: f64,
    pub open: Option<i8>,
}

pub(super) fn web_of(d: &Dims) -> Web {
    let side = -d.b / 2.0 + d.tw / 2.0;
    match d.kind {
        Kind::I => Web {
            y: 0.0,
            t: d.tw,
            zc: 0.0,
            hw: d.h - 2.0 * d.tf,
            open: None,
        },
        Kind::Channel => Web {
            y: side,
            t: d.tw,
            zc: 0.0,
            hw: d.h - 2.0 * d.tf,
            open: Some(-1),
        },
        Kind::Angle => Web {
            y: side,
            t: d.tw,
            zc: d.tw / 2.0,
            hw: d.h - d.tw,
            open: Some(-1),
        },
        Kind::Tee => Web {
            y: 0.0,
            t: d.tw,
            zc: -d.tf / 2.0,
            hw: d.h - d.tf,
            open: Some(1),
        },
        Kind::Tube | Kind::Pipe => Web {
            y: 0.0,
            t: d.b,
            zc: 0.0,
            hw: d.h - 2.0 * d.tw,
            open: Some(1),
        },
        Kind::Rect | Kind::Circle | Kind::Poly => Web {
            y: 0.0,
            t: d.b,
            zc: 0.0,
            hw: d.h,
            open: Some(1),
        },
    }
}

/// A support face in the world: point, unit normal toward the joined member, section face and
/// the wall behind it (what a bolt through it grips).
pub(super) struct WFace {
    pub p: V3,
    pub n: V3,
    pub face: Face,
    pub wall: f64,
}

/// Face of `sup` toward the direction `d` (a plate goes on it).
pub(super) fn face_of(sup: &Bar, d: V3) -> WFace {
    let face = sup.face_toward(across(d, sup.u));
    let n = sup.sdir(face.n);
    let mid = (face.a + face.b) * 0.5;
    let p = sup.at(0.0, mid.x, mid.y);
    let wall = sup
        .cut_line(mid - face.n * 1e-6, -face.n)
        .into_iter()
        .find(|iv| iv.0 <= 1e-4)
        .map_or(sup.dims.thin(), |iv| iv.1 - iv.0.max(0.0));
    WFace { p, n, face, wall }
}

/// Bolt rows in a plate that stays `gap` inside a clear depth `hw`: (bolt, rows, pitch, edge
/// distance). Without an asked bolt, a bolt too big for two rows steps down the series.
pub(super) fn rows_in(
    k: &Kit,
    size0: BoltSize,
    hw: f64,
    gap: f64,
    want: f64,
    rows: Option<usize>,
) -> Result<(BoltSize, usize, f64, f64), ConnError> {
    let mut size = size0;
    loop {
        let (e, p) = (k.cat.e_layout(size.hole), k.cat.p_layout(size.hole));
        let edge = (e + gap).max(k.cat.face_clear(&size));
        let span = hw - 2.0 * edge;
        if span >= -1e-9 {
            let fit = (span.max(0.0) / p + 1e-9).floor() as usize + 1;
            let target = (((want - 2.0 * e) / p).floor().max(0.0) as usize + 1).max(2);
            let n = rows.unwrap_or_else(|| target.min(fit));
            if n <= fit && (n >= 2 || rows.is_some() || k.p.bolt.is_some()) {
                return Ok((size, n, p, e));
            }
        }
        match (k.p.bolt, k.cat.smaller(size.d)) {
            (None, Some(s)) => size = s.clone(),
            _ => {
                return Err(ConnError(format!(
                    "{} rows of {} do not fit the clear depth {} mm",
                    rows.unwrap_or(2),
                    size.name(),
                    fmt_mm(hw)
                )))
            }
        }
    }
}

/// Fin plate (or flexible end plate) of `beam` on `sup`.
pub(super) fn pinned(k: &mut Kit, sup: &Bar, beam: &Bar) -> Result<(), ConnError> {
    match k.p.variant.clone().as_deref().unwrap_or("fin") {
        "fin" => fin(k, sup, beam),
        "end_plate" => header(k, sup, beam),
        other => Err(ConnError(format!(
            "pinned takes variant fin or end_plate, not {other}"
        ))),
    }
}

/// Stair stringer on its support: a fin plate in the stringer web, cut to the support face.
pub(super) fn stringer(k: &mut Kit, sup: &Bar, st: &Bar) -> Result<(), ConnError> {
    if st.u.z.abs().min(1.0).asin() < 10f64.to_radians() {
        k.note("the stringer is nearly level; a pinned connection may suit better".to_string());
    }
    fin(k, sup, st)
}

/// The joined member's end at the node and the support face it meets.
pub(super) fn meet_face(k: &mut Kit, sup: &Bar, beam: &Bar) -> Result<(V3, WFace), ConnError> {
    let node = meet(&[sup, beam])?;
    let (_, _, into) = beam.end_near(node);
    let wf = face_of(sup, into);
    if into.dot(wf.n) < 0.2 {
        return Err(ConnError::new("the member does not meet the support face"));
    }
    k.node = node;
    if into.dot(wf.n) < 0.995 {
        k.note(format!(
            "skewed {:.1} deg to the support face: the plate edge follows the face",
            into.dot(wf.n).min(1.0).acos().to_degrees()
        ));
    }
    Ok((into, wf))
}

fn fin(k: &mut Kit, sup: &Bar, beam: &Bar) -> Result<(), ConnError> {
    let (into, wf) = meet_face(k, sup, beam)?;
    let (_, t_end, _) = beam.end_near(k.node);
    let web = web_of(&beam.dims);
    let side = f64::from(k.p.side.unwrap_or(web.open.unwrap_or(1)));
    let thin = beam.dims.thin().min(wf.wall);
    let tp = k.plate_t(web.t.clamp(0.008, 0.02), thin);
    let leg = k.leg(tp, wf.wall);
    let gap = k.p.gap.unwrap_or(k.cat.conn.gap);
    let size0 = k.bolt_size(k.cat.bolt_for_depth(beam.dims.h))?;
    let want = k.cat.conn.fin_depth_share * beam.dims.h;
    let (size, rows, p, e) = rows_in(k, size0, web.hw, gap, want, k.p.rows)?;
    let cols = k.p.cols.unwrap_or(if beam.dims.h >= 0.5 { 2 } else { 1 });
    // plate frame: x along the member away from the support, y along its section z
    let s_face = hit_plane(beam.c0, beam.u, wf.p, wf.n)
        .ok_or_else(|| ConnError::new("the member is parallel to the support face"))?;
    let web_off = web.y + side * (web.t / 2.0 + tp / 2.0);
    let o = beam.c0 + beam.u * s_face + beam.ey * web_off + beam.ez * web.zc;
    let f = frame_from(o, into.cross(beam.ez), into);
    let x_face = |y: f64, z: f64| hit_plane(f.at(v3(0.0, y, z)), f.x, wf.p, wf.n).unwrap_or(0.0);
    let hp = (rows - 1) as f64 * p + 2.0 * e;
    let (y0, y1) = (-hp / 2.0, hp / 2.0);
    let xb = |y: f64| x_face(y, -tp / 2.0).max(x_face(y, tp / 2.0));
    let x_back = xb(y0).max(xb(y1));
    // first bolt line: past the member end by the end distance, clear of the support face
    let x_end = (beam.axis(t_end) - o).dot(f.x);
    let x1 = k.cat.round_up(
        (x_end + e)
            .max(x_back + k.cat.face_clear(&size))
            .max(x_back + e),
    );
    let xs: Vec<f64> = (0..cols).map(|i| x1 + i as f64 * p).collect();
    let x_far = xs[cols - 1] + e;
    let outline = vec![v2(xb(y0), y0), v2(x_far, y0), v2(x_far, y1), v2(xb(y1), y1)];
    let holes: Vec<_> = xs
        .iter()
        .flat_map(|x| {
            centred(rows, p)
                .into_iter()
                .map(move |y| (v2(*x, y), size.hole))
        })
        .collect();
    let pl = k.plate(Role::Plate, f, outline, holes.clone(), tp, -tp / 2.0);
    // bolts from the plate's outer face through the web
    let out = beam.ey * side;
    for (h, _) in &holes {
        let c = f.at(v3(h.x, h.y, 0.0)) + out * (tp / 2.0);
        k.bolt(&size, c, -out, tp + web.t, vec![pl], f.x)?;
    }
    // one fillet each side of the plate along the support face
    for s in [1.0, -1.0] {
        let z = s * tp / 2.0;
        let a = f.at(v3(x_face(y0, z), y0, z));
        let run = f.at(v3(x_face(y1, z), y1, z)) - a;
        let len = run.len();
        let dir = run * (1.0 / len.max(1e-12));
        k.weld(a, dir, len, across(f.z * s, wf.n), across(f.x, dir), leg);
    }
    Ok(())
}

/// Flexible end plate on the support face, welded to the member web, two bolt lines.
fn header(k: &mut Kit, sup: &Bar, beam: &Bar) -> Result<(), ConnError> {
    let (_, wf) = meet_face(k, sup, beam)?;
    let (end, _, _) = beam.end_near(k.node);
    let web = web_of(&beam.dims);
    let size0 = k.bolt_size(k.cat.bolt_for_depth(beam.dims.h))?;
    let gap = k.p.gap.unwrap_or(k.cat.conn.gap);
    let want = k.cat.conn.fin_depth_share * beam.dims.h;
    let (size, rows, p, e) = rows_in(k, size0, web.hw, gap, want, k.p.rows)?;
    let thin = beam.dims.thin().min(wf.wall);
    let tp = k.plate_t(k.cat.end_plate_t(size.d).clamp(0.008, 0.015), thin);
    let leg = k.leg(tp, web.t);
    // plate on the face: z toward the member, x across its web, y along its section z
    let s_face = hit_plane(beam.c0, beam.u, wf.p, wf.n).unwrap_or(0.0);
    let o = beam.c0 + beam.u * s_face;
    let o = o + wf.n * wf.n.dot(wf.p - o);
    let f = frame_from(o, wf.n, across(beam.ey, wf.n));
    let wc = f.local(o + beam.ey * web.y + beam.ez * web.zc);
    let (xw, yw) = (wc.x, wc.y);
    let reach = size.hex_r().max(size.w_od / 2.0) + k.cat.spacing.face_clear;
    // a skewed web leans over the plate: heads sit clear of it at their full height
    let cosa = beam.end_near(k.node).2.dot(wf.n).clamp(0.2, 1.0);
    let lean = (size.k + size.w_t) * (1.0 - cosa * cosa).sqrt() / cosa;
    let half = (web.t / 2.0 + leg + reach + lean).max(k.cat.spacing.p2_min * size.hole / 2.0);
    let g = 2.0 * k.cat.round_up(half);
    let hp = (rows - 1) as f64 * p + 2.0 * e;
    let holes: Vec<_> = centred(rows, p)
        .into_iter()
        .flat_map(|y| [-1.0, 1.0].map(|s| (v2(xw + s * g / 2.0, yw + y), size.hole)))
        .collect();
    let pl = k.plate(
        Role::Plate,
        f,
        rect(xw, yw, g + 2.0 * e, hp),
        holes.clone(),
        tp,
        0.0,
    );
    for (h, _) in &holes {
        k.bolt(
            &size,
            f.at(v3(h.x, h.y, tp)),
            -f.z,
            tp + wf.wall,
            vec![pl],
            f.x,
        )?;
    }
    // web to plate on both faces of the web, stopped short of the plate ends
    let len = hp - 2.0 * leg;
    for s in [1.0, -1.0] {
        let root = f.at(v3(xw + s * web.t / 2.0, yw - len / 2.0, tp));
        k.weld(root, f.y, len, f.x * s, f.z, leg);
    }
    k.trim(1, end, tp);
    k.note(format!(
        "the member end should stop {} mm short of the support face for the end plate",
        fmt_mm(tp)
    ));
    Ok(())
}

/// Angle cleat welded on the rafter's top face, bolted through the purlin web on its open side.
pub(super) fn purlin_cleat(k: &mut Kit, raf: &Bar, pur: &Bar) -> Result<(), ConnError> {
    let node = meet(&[raf, pur])?;
    let pc = pur.axis(pur.local(node).x);
    let to_pur = pc - raf.axis(raf.local(node).x);
    let wf = face_of(raf, if to_pur.len() > 1e-6 { to_pur } else { raf.ez });
    let nf = wf.n;
    let a = across(pur.u, nf);
    if pur.ez.dot(nf).abs() < 0.95 {
        k.note("the purlin web is not square to the rafter face".to_string());
    }
    let web = web_of(&pur.dims);
    let Some(open) = k.p.side.or(web.open) else {
        return Err(ConnError::new(
            "a purlin cleat bolts to an open web side: use a channel, angle, tee or hollow \
             purlin, or give side=1 or side=-1",
        ));
    };
    let side = f64::from(open);
    // heights above the rafter face: the purlin axis and its clear web zone
    let s_axis = nf.dot(pc - wf.p);
    let s_bot = s_axis - pur.dims.h / 2.0;
    if s_bot < -0.001 {
        k.note(format!(
            "the purlin sinks {} mm into the rafter",
            fmt_mm(-s_bot)
        ));
    }
    let tc = k.plate_t(0.008, pur.dims.thin().min(wf.wall));
    let leg = k.leg(tc, wf.wall);
    let size0 = k.bolt_size(k.cat.bolt_for_depth(pur.dims.h).min(0.016))?;
    let (size, _, p, e) = rows_in(k, size0, web.hw, 0.0, 0.0, Some(1))?;
    let clear = k.cat.face_clear(&size);
    let z_lo = s_axis + web.zc - web.hw / 2.0 + clear;
    let z_hi = s_axis + web.zc + web.hw / 2.0 - clear;
    let z1 = k.cat.round_up(z_lo.max(tc + clear));
    let rows =
        k.p.rows
            .unwrap_or(if z1 + p <= z_hi + 1e-9 { 2 } else { 1 });
    let zs: Vec<f64> = (0..rows).map(|i| z1 + i as f64 * p).collect();
    let hv = k.cat.size_up(zs[rows - 1] + e);
    if zs[rows - 1] > z_hi + 1e-9 || hv > s_bot + pur.dims.h + 1e-9 {
        return Err(ConnError(format!(
            "a cleat with {} {} does not fit the purlin depth {} mm",
            rows,
            size.name(),
            fmt_mm(pur.dims.h)
        )));
    }
    let cols = k.p.cols.unwrap_or(1);
    let lc = k.cat.size_up(2.0 * e + (cols - 1) as f64 * p).max(0.06);
    let out = across(across(pur.ey * side, nf), a);
    // the web face on the open side, dropped onto the rafter face
    let wpt = pc + pur.ey * (web.y + side * web.t / 2.0);
    let wpt = wpt - nf * nf.dot(wpt - wf.p);
    // leg: x along the purlin, y up from the rafter face, z across (sign toward `out`)
    let f = Frame {
        o: wpt - a * (lc / 2.0),
        x: a,
        y: nf,
        z: a.cross(nf),
    };
    let z0 = if f.z.dot(out) > 0.0 { 0.0 } else { -tc };
    let xs: Vec<f64> = centred(cols, p).into_iter().map(|x| x + lc / 2.0).collect();
    let holes: Vec<_> = xs
        .iter()
        .flat_map(|x| zs.iter().map(move |z| (v2(*x, *z), size.hole)))
        .collect();
    let leg_pl = k.plate(
        Role::Cleat,
        f,
        rect2(0.0, tc, lc, hv),
        holes.clone(),
        tc,
        z0,
    );
    // foot on the rafter face, out from the web; the leg stands on it
    let bl = k.cat.size_up(tc + 2.0 * leg + clear).max(0.05);
    let ff = Frame {
        o: f.o,
        x: a,
        y: out,
        z: a.cross(out),
    };
    let fz0 = if ff.z.dot(nf) > 0.0 { 0.0 } else { -tc };
    k.plate(Role::Cleat, ff, rect2(0.0, 0.0, lc, bl), vec![], tc, fz0);
    let cleat_leg = k.leg(tc, tc);
    k.weld(f.o + out * tc + nf * tc, a, lc, out, nf, cleat_leg);
    k.weld(f.o + out * bl, a, lc, out, nf, leg);
    for (x, s) in [(0.0, -1.0), (lc, 1.0)] {
        k.weld(f.o + a * x, out, bl, a * s, nf, leg);
    }
    for (h, _) in &holes {
        let c = f.o + a * h.x + nf * h.y + out * tc;
        k.bolt(&size, c, -out, tc + web.t, vec![leg_pl], a)?;
    }
    k.node = wpt;
    Ok(())
}
