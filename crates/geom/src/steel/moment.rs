//! Moment connections: an extended end plate of a beam on a column face (with column web
//! stiffeners at the beam flanges), and splices of beams or columns, by cover plates (I
//! sections) or by a pair of bolted end plates (any section).

use super::bar::{across, hit_plane, meet, Bar, Kind};
use super::catalog::{fmt_mm, BoltSize};
use super::kit::{centred, footprint, holes_at, rect2, Kit};
use super::part::{frame_from, Role};
use super::shear::{meet_face, web_of};
use super::ConnError;
use crate::gsolid::Frame;
use crate::math::{v2, v3, V2, V3};

/// Head or nut reach round a bolt axis plus wrench room.
fn reach(k: &Kit, b: &BoltSize) -> f64 {
    b.hex_r().max(b.w_od / 2.0) + k.cat.spacing.face_clear
}

/// Steps down the series until `fits` accepts a bolt (an asked bolt is never changed).
fn first_fit<T>(
    k: &Kit,
    d: f64,
    what: &str,
    mut fits: impl FnMut(&BoltSize) -> Option<T>,
) -> Result<T, ConnError> {
    let mut size = k.bolt_size(d)?;
    loop {
        if let Some(t) = fits(&size) {
            return Ok(t);
        }
        match (k.p.bolt, k.cat.smaller(size.d)) {
            (None, Some(s)) => size = s.clone(),
            _ => return Err(ConnError(format!("{} does not fit {what}", size.name()))),
        }
    }
}

/// Extended end plate of `beam` on the face of `col`, bolted through the column flange.
pub(super) fn rigid(k: &mut Kit, col: &Bar, beam: &Bar) -> Result<(), ConnError> {
    let (_, wf) = meet_face(k, col, beam)?;
    let (end, _, _) = beam.end_near(k.node);
    let s_face = hit_plane(beam.c0, beam.u, wf.p, wf.n)
        .ok_or_else(|| ConnError::new("the beam is parallel to the column face"))?;
    let o = beam.c0 + beam.u * s_face;
    let o = o + wf.n * wf.n.dot(wf.p - o);
    // plate frame: z off the face toward the beam, y along the beam's section z (up)
    let y = across(beam.ez, wf.n);
    let f = Frame {
        o,
        x: y.cross(wf.n),
        y,
        z: wf.n,
    };
    let d0 = k.cat.bolt_for_depth(beam.dims.h).max(0.016);
    let tp0 = k.cat.end_plate_t(d0);
    let (bx0, bx1, by0, by1) = footprint(beam, &f, tp0);
    // flange thickness as cut by the plate plane (a sloping beam cuts it longer)
    let tfp = beam.dims.tf * (by1 - by0) / beam.dims.h.max(1e-9);
    let web = web_of(&beam.dims);
    let xw = f.local(o + beam.ey * web.y).x;
    let i_col = col.dims.kind == Kind::I;
    let leg_guess = k.leg(tp0, beam.dims.tf);
    let lay = first_fit(k, d0, "the beam depth", |b| {
        let r = reach(k, b);
        let (e, p) = (k.cat.e_layout(b.hole), k.cat.p_layout(b.hole));
        let room = k.cat.round_up(leg_guess + r);
        let y_out = by1 + room;
        let y_in = by1 - tfp - room;
        let y_lo = by0 + tfp + room;
        (y_in >= y_lo - 1e-9).then_some((b.clone(), e, p, y_out, y_in, y_lo, r))
    })?;
    let (size, e, p, y_out, y_in, y_lo, r) = lay;
    let tp = k.plate_t(k.cat.end_plate_t(size.d), beam.dims.thin().min(wf.wall));
    let leg = k.leg(tp, beam.dims.tf);
    // rows between the flanges: from under the top flange to over the bottom one
    let mut ys = vec![y_out, y_in];
    let run = y_in - y_lo;
    let fit = (run / p + 1e-9).floor() as usize + 1;
    let auto = if run >= p {
        ((run / k.cat.p_max(tp)).ceil() as usize + 1).max(2)
    } else {
        1
    };
    let inner = match k.p.rows {
        Some(n) if n > fit => {
            return Err(ConnError(format!(
                "{n} bolt rows of {} do not fit between the flanges (at most {fit})",
                size.name()
            )))
        }
        Some(n) => n,
        None => auto.min(fit),
    };
    if inner >= 2 {
        let step = run / (inner - 1) as f64;
        ys.extend((1..inner).map(|i| y_in - i as f64 * step));
    } else {
        ys[1] = (y_in + y_lo) / 2.0;
    }
    let mut half = (web.t / 2.0 + leg + r).max(k.cat.spacing.p2_min * size.hole / 2.0);
    if i_col {
        half = half.max(col.dims.tw / 2.0 + r);
    }
    let g = 2.0 * k.cat.round_up(half);
    let w = k
        .cat
        .size_up((g + 2.0 * e).max(bx1 - bx0 + 2.0 * (leg + 0.01)));
    let face_w = (wf.face.b - wf.face.a).len();
    if g / 2.0 + k.cat.spacing.e2_min * size.hole > face_w / 2.0 + 1e-9 {
        k.note(format!(
            "bolt gauge {} mm is too wide for the {} mm column face",
            fmt_mm(g),
            fmt_mm(face_w)
        ));
    }
    let top = y_out + e;
    let bot = by0 - k.cat.size_up(leg + 0.01);
    let holes: Vec<(V2, f64)> = ys
        .iter()
        .flat_map(|y| [-1.0, 1.0].map(|s| (v2(xw + s * g / 2.0, *y), size.hole)))
        .collect();
    let outline = rect2(xw - w / 2.0, bot, xw + w / 2.0, top);
    let pl = k.plate(Role::Plate, f, outline, holes.clone(), tp, 0.0);
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
    k.weld_section(beam, f.at(v3(0.0, 0.0, tp)), f.z, leg);
    k.trim(1, end, tp);
    k.note(format!(
        "the beam end should stop {} mm short of the column face",
        fmt_mm(tp)
    ));
    let flange_face = wf.face.n.y.abs() > 0.9;
    if i_col && flange_face && k.p.stiffeners.unwrap_or(true) {
        let ts = k
            .cat
            .plate_t(k.cat.plate.stiff_ratio * beam.dims.tf)
            .min(tp);
        let lg = k.leg(ts, col.dims.tw);
        for yl in [by1 - tfp / 2.0, by0 + tfp / 2.0] {
            let t = col.local(f.at(v3(xw, yl, 0.0))).x;
            if t > col.t0 + ts && t < col.t1 - ts {
                for side in [1.0, -1.0] {
                    col_stiffener(k, col, t, side, ts, lg);
                }
            }
        }
    } else if i_col && !flange_face {
        k.note("beam on the column web: no column stiffeners".to_string());
    }
    Ok(())
}

/// Stiffener across an I column between its flanges at axis position `t`, on one web side.
fn col_stiffener(k: &mut Kit, col: &Bar, t: f64, side: f64, ts: f64, leg: f64) {
    let d = col.dims;
    let snip = k.cat.plate.snip;
    let hc = d.h / 2.0 - d.tf;
    let (y0, y1) = (d.tw / 2.0, d.b / 2.0 - 0.002);
    if y1 - y0 < snip + 0.02 {
        return;
    }
    let pts = [
        v2(y0 + snip, -hc),
        v2(y1, -hc),
        v2(y1, hc),
        v2(y0 + snip, hc),
        v2(y0, hc - snip),
        v2(y0, -hc + snip),
    ];
    let outline: Vec<V2> = pts.iter().map(|p| v2(p.x * side, p.y)).collect();
    let f = Frame {
        o: col.axis(t),
        x: col.ey,
        y: col.ez,
        z: col.u,
    };
    k.plate(Role::Stiffener, f, outline, vec![], ts, -ts / 2.0);
    let ey = col.ey * side;
    let web_root = f.at(v3(side * y0, -hc + snip, 0.0));
    k.weld_both_sides(web_root, col.ez, 2.0 * (hc - snip), col.u, ey, ts, leg);
    for zs in [1.0, -1.0] {
        let root = f.at(v3(side * (y0 + snip), zs * hc, 0.0));
        k.weld_both_sides(root, ey, y1 - y0 - snip, col.u, -col.ez * zs, ts, leg);
    }
}

/// Beam or column splice; `a` is the first (lower) member.
pub(super) fn splice(k: &mut Kit, a: &Bar, b: &Bar) -> Result<(), ConnError> {
    if a.u.dot(b.u).abs() < 0.98 {
        return Err(ConnError::new(
            "a splice joins members in line (within 11 degrees)",
        ));
    }
    let node = meet(&[a, b])?;
    let mid = |m: &Bar| m.axis((m.t0 + m.t1) / 2.0);
    let n = if (mid(b) - mid(a)).dot(a.u) >= 0.0 {
        a.u
    } else {
        -a.u
    };
    let node = a.axis(a.local(node).x);
    k.node = node;
    let i_both = a.dims.kind == Kind::I && b.dims.kind == Kind::I;
    let default = if i_both { "cover" } else { "end_plate" };
    match k.p.variant.clone().as_deref().unwrap_or(default) {
        "cover" if i_both => cover(k, a, b, n),
        "cover" => Err(ConnError::new(
            "cover plates splice I sections; use variant=end_plate",
        )),
        "end_plate" => end_plates(k, a, b, n),
        other => Err(ConnError(format!(
            "a splice takes variant cover or end_plate, not {other}"
        ))),
    }
}

/// Section of a spliced member at the joint, in the joint frame (y along the first member's
/// section y, z along its section z).
struct Side {
    dy: f64,
    top: f64,
    bot: f64,
    tf: f64,
    tw: f64,
}

fn side_of(m: &Bar, node: V3, ey: V3, ez: V3) -> Side {
    let c = m.axis(m.local(node).x) - node;
    let (dy, dz) = (c.dot(ey), c.dot(ez));
    Side {
        dy,
        top: dz + m.dims.h / 2.0,
        bot: dz - m.dims.h / 2.0,
        tf: m.dims.tf,
        tw: m.dims.tw,
    }
}

fn cover(k: &mut Kit, a: &Bar, b: &Bar, n: V3) -> Result<(), ConnError> {
    if b.ez.dot(a.ez).abs() < 0.95 {
        return Err(ConnError::new(
            "the spliced sections are turned against each other",
        ));
    }
    let node = k.node;
    let (ey, ez) = (a.ey, a.ez);
    let (sa, sb) = (side_of(a, node, ey, ez), side_of(b, node, ey, ez));
    let gap = k.p.gap.unwrap_or(k.cat.conn.gap);
    let bmin = a.dims.b.min(b.dims.b);
    let tw = sa.tw.max(sb.tw);
    let tcf = k.plate_t(
        sa.tf.max(sb.tf).max(0.008),
        a.dims.thin().min(b.dims.thin()),
    );
    let tcw = k.cat.plate_t((tw / 2.0).max(k.cat.plate.min_t));
    let top_in = (sa.top - sa.tf).min(sb.top - sb.tf);
    let bot_in = (sa.bot + sa.tf).max(sb.bot + sb.tf);
    let hwc = top_in - bot_in;
    let cols = k.p.cols.unwrap_or(2);
    let d0 = k.cat.bolt_for_depth(a.dims.h.min(b.dims.h));
    let lay = first_fit(k, d0, "the flange width and web depth", |s| {
        let r = reach(k, s) - k.cat.spacing.face_clear + 0.003;
        let (e, p) = (k.cat.e_layout(s.hole), k.cat.p_layout(s.hole));
        let gf = 2.0
            * k.cat
                .round_up((tw / 2.0 + tcw + r).max(k.cat.spacing.p2_min * s.hole / 2.0));
        if gf / 2.0 + k.cat.spacing.e2_min * s.hole > bmin / 2.0 + 1e-9 {
            return None;
        }
        // web bolts stay clear of the flange bolt nuts under the flanges
        let nut = s.w_t + s.m + k.cat.spacing.protrusion * s.pitch;
        let edge = (nut + r + 0.002).max(e + gap);
        let span = hwc - 2.0 * edge;
        if span < -1e-9 {
            return None;
        }
        let fit = (span.max(0.0) / p + 1e-9).floor() as usize + 1;
        let want = ((0.6 * hwc - 2.0 * e) / p).floor().max(1.0) as usize + 1;
        let rows = k.p.rows.unwrap_or(fit.min(want));
        (rows <= fit).then(|| (s.clone(), e, p, gf, rows))
    })?;
    let (size, e, p, gf, rows) = lay;
    let xs: Vec<f64> = (0..cols).map(|i| gap / 2.0 + e + i as f64 * p).collect();
    let half_len = xs[cols - 1] + e;
    let at = |x: f64, y: f64, z: f64| node + n * x + ey * y + ez * z;
    let side_x = |s: f64| xs.iter().map(move |x| x * s);
    let dyw = (sa.dy + sb.dy) / 2.0;
    if (sa.dy - sb.dy).abs() > 0.001 {
        k.note("the spliced webs are not in line".to_string());
    }
    if (sa.tw - sb.tw).abs() > 0.004 {
        k.note("the webs differ by more than 4 mm: add web packers".to_string());
    }
    for zs in [1.0, -1.0] {
        let (fa, fb) = if zs > 0.0 {
            (sa.top, sb.top)
        } else {
            (-sa.bot, -sb.bot)
        };
        let face = fa.max(fb) * zs;
        let dz = (fa - fb).abs();
        let f = frame_from(at(0.0, dyw, face), ez * zs, n);
        let mut pts = vec![];
        for s in [-1.0, 1.0] {
            for x in side_x(s) {
                for y in [-gf / 2.0, gf / 2.0] {
                    pts.push(at(x, dyw + y, face));
                }
            }
        }
        let holes = holes_at(&f, &pts, size.hole);
        let corners = [
            at(-half_len, dyw - bmin / 2.0, face),
            at(half_len, dyw + bmin / 2.0, face),
        ];
        let outline = box_of(&holes_at(&f, &corners, 0.0));
        let cov = k.plate(Role::Plate, f, outline, holes.clone(), tcf, 0.0);
        // packer under the cover on the member with the lower flange face
        let low = if fa < fb { -1.0 } else { 1.0 };
        let mut packer = None;
        if dz > 0.002 {
            let ph: Vec<_> = holes
                .iter()
                .zip(&pts)
                .filter(|(_, p)| (**p - node).dot(n) * low > 0.0)
                .map(|(h, _)| *h)
                .collect();
            let ends = [
                at(low * gap / 2.0, dyw - bmin / 2.0, face),
                at(low * half_len, dyw + bmin / 2.0, face),
            ];
            let po = box_of(&holes_at(&f, &ends, 0.0));
            packer = Some(k.plate(Role::Packer, f, po, ph, dz, -dz));
        } else if dz > 1e-4 {
            k.note(format!(
                "flange faces differ by {} mm: no packer below 2 mm",
                fmt_mm(dz)
            ));
        }
        for (h, p) in holes.iter().zip(&pts) {
            let on_low = (*p - node).dot(n) * low > 0.0;
            let tf = if (*p - node).dot(n) < 0.0 {
                sa.tf
            } else {
                sb.tf
            };
            let pk = if on_low && packer.is_some() { dz } else { 0.0 };
            let plies = packer
                .filter(|_| on_low)
                .map_or(vec![cov], |q| vec![cov, q]);
            let o = f.at(v3(h.0.x, h.0.y, tcf));
            k.bolt(&size, o, -f.z, tcf + pk + tf, plies, n)?;
        }
    }
    // web covers on both faces of the web
    let zc = (top_in + bot_in) / 2.0;
    let hcw = (rows - 1) as f64 * p + 2.0 * e;
    let mut pts = vec![];
    for s in [-1.0, 1.0] {
        for x in side_x(s) {
            for z in centred(rows, p) {
                pts.push(at(x, dyw + tw / 2.0, zc + z));
            }
        }
    }
    let mut covers = vec![];
    for ys in [1.0, -1.0] {
        let f = frame_from(at(0.0, dyw + ys * tw / 2.0, zc), ey * ys, n);
        let holes = holes_at(&f, &pts, size.hole);
        let corners = [
            at(-half_len, 0.0, zc - hcw / 2.0),
            at(half_len, 0.0, zc + hcw / 2.0),
        ];
        let outline = box_of(&holes_at(&f, &corners, 0.0));
        covers.push((k.plate(Role::Plate, f, outline, holes, tcw, 0.0), f));
    }
    let (c0, f0) = (covers[0].0, covers[0].1);
    for p in &pts {
        let q = f0.local(*p);
        let o = f0.at(v3(q.x, q.y, tcw));
        k.bolt(&size, o, -ey, 2.0 * tcw + tw, vec![c0, covers[1].0], n)?;
    }
    if (a.dims.h - b.dims.h).abs() > 1e-4 {
        k.note(format!(
            "sections of {} and {} mm deep: covers on the outer faces, packers under",
            fmt_mm(a.dims.h),
            fmt_mm(b.dims.h)
        ));
    }
    Ok(())
}

/// Rectangle spanning two corner points (as returned by `holes_at`).
fn box_of(c: &[(V2, f64)]) -> Vec<V2> {
    let (p, q) = (c[0].0, c[1].0);
    rect2(p.x.min(q.x), p.y.min(q.y), p.x.max(q.x), p.y.max(q.y))
}

/// Two end plates welded to the member ends and bolted together outside the sections.
fn end_plates(k: &mut Kit, a: &Bar, b: &Bar, n: V3) -> Result<(), ConnError> {
    let node = k.node;
    let f = frame_from(node, n, a.ey);
    let fa = footprint(a, &f, 0.0);
    let fb = footprint(b, &f, 0.0);
    let (x0, x1, y0, y1) = (
        fa.0.min(fb.0),
        fa.1.max(fb.1),
        fa.2.min(fb.2),
        fa.3.max(fb.3),
    );
    let thin = a.dims.thin().min(b.dims.thin());
    let d0 = k.cat.bolt_for_depth(a.dims.h.max(a.dims.b)).min(0.024);
    let size = k.bolt_size(d0)?;
    let tp = k.plate_t(k.cat.end_plate_t(size.d), thin);
    let leg = k.leg(tp, thin);
    let (e, p) = (k.cat.e_layout(size.hole), k.cat.p_layout(size.hole));
    let c = k.cat.round_up(leg + reach(k, &size));
    let round = a.dims.round() && b.dims.round();
    let mut pts: Vec<V2> = vec![];
    if round {
        let rb = (x1 - x0).max(y1 - y0) / 2.0 + c;
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let count = ((2.0 * std::f64::consts::PI * rb / (2.0 * p)).ceil() as usize).clamp(4, 8);
        let count = count + count % 2;
        pts.extend(super::part::ngon(
            v2(cx, cy),
            rb,
            count,
            std::f64::consts::PI / count as f64,
        ));
    } else {
        let i_like = matches!(a.dims.kind, Kind::I | Kind::Channel | Kind::Tee);
        let line = |lo: f64, hi: f64| -> Vec<f64> {
            let span = hi - lo;
            let least = if i_like || span > 2.0 * p { 2 } else { 1 };
            let m = ((span / (2.5 * p)).floor() as usize + 1).max(least);
            let step = if m > 1 { span / (m - 1) as f64 } else { 0.0 };
            (0..m)
                .map(|i| {
                    if m > 1 {
                        lo + i as f64 * step
                    } else {
                        (lo + hi) / 2.0
                    }
                })
                .collect()
        };
        for yb in [y0 - c, y1 + c] {
            for x in line(x0, x1) {
                pts.push(v2(x, yb));
            }
        }
        if !i_like {
            for xb in [x0 - c, x1 + c] {
                for y in line(y0, y1) {
                    pts.push(v2(xb, y));
                }
            }
        }
    }
    let (mut px0, mut px1, mut py0, mut py1) = (x0, x1, y0, y1);
    for q in &pts {
        px0 = px0.min(q.x - e);
        px1 = px1.max(q.x + e);
        py0 = py0.min(q.y - e);
        py1 = py1.max(q.y + e);
    }
    let outline = if round {
        let c = v2((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let r = pts.iter().map(|q| (*q - c).len()).fold(0.0, f64::max);
        // circumscribed 24-gon so the edge distance holds on the flats too
        let r = k.cat.size_up(r + e) / (std::f64::consts::PI / 24.0).cos();
        super::part::ngon(c, r, 24, 0.0)
    } else {
        rect2(px0, py0, px1, py1)
    };
    let holes: Vec<(V2, f64)> = pts.iter().map(|q| (*q, size.hole)).collect();
    let pa = k.plate(Role::Plate, f, outline.clone(), holes.clone(), tp, -tp);
    let pb = k.plate(Role::Plate, f, outline, holes.clone(), tp, 0.0);
    for (h, _) in &holes {
        k.bolt(
            &size,
            f.at(v3(h.x, h.y, tp)),
            -n,
            2.0 * tp,
            vec![pa, pb],
            f.x,
        )?;
    }
    k.weld_section(a, f.at(v3(0.0, 0.0, -tp)), -n, leg);
    k.weld_section(b, f.at(v3(0.0, 0.0, tp)), n, leg);
    for (i, m) in [a, b].into_iter().enumerate() {
        let (end, _, _) = m.end_near(node);
        k.trim(i, end, tp);
    }
    k.note(format!(
        "each member end should stop {} mm short of the joint",
        fmt_mm(tp)
    ));
    Ok(())
}
