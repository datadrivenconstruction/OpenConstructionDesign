//! Column base: a base plate under the column end, anchor rods outside the section with
//! plate washers and nuts, welds all round the section and, for larger sections, stiffeners
//! in the anchor gaps. The plate lies in the column's end plane, so an inclined or rotated
//! column gets the same detail turned with it.

use super::bar::{Bar, End};
use super::kit::{rect, Kit};
use super::part::{frame_from, Role};
use super::ConnError;
use crate::math::{v2, v3, V2};

pub(super) fn base_plate(k: &mut Kit, col: &Bar) -> Result<(), ConnError> {
    let (za, zb) = (col.axis(col.t0).z, col.axis(col.t1).z);
    let end = if za <= zb { End::Start } else { End::End };
    let cap = col.cap(end);
    let up = -cap.n.normalized();
    if up.z < 0.5 {
        return Err(ConnError::new(
            "a column base needs the column end facing down",
        ));
    }
    let t_end = if end == End::Start { col.t0 } else { col.t1 };
    k.node = col.axis(t_end);
    let f = frame_from(k.node, up, col.ey);
    // section footprint on the plate (stretched for an inclined column)
    let foot: Vec<V2> = col
        .outer
        .iter()
        .map(|p| {
            let w = col.at(0.0, p.x, p.y);
            let s = super::bar::hit_plane(w, col.u, k.node, up).unwrap_or(0.0);
            let q = f.local(w + col.u * s);
            v2(q.x, q.y)
        })
        .collect();
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in &foot {
        x0 = x0.min(p.x);
        x1 = x1.max(p.x);
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    let (fw, fh) = (x1 - x0, y1 - y0);
    let cat = k.cat;
    let (d_def, n_def) = cat.anchors_for(col.dims.h.max(col.dims.b));
    let size = k.bolt_size(d_def)?;
    let count = k.p.anchors.unwrap_or(n_def);
    if !matches!(count, 4 | 6 | 8) {
        return Err(ConnError::new("a column base takes 4, 6 or 8 anchors"));
    }
    let per_side = count / 2;
    let hole = size.anchor_hole;
    let thin = col.dims.thin();
    let t = k.plate_t((1.5 * col.dims.tf.max(col.dims.tw)).max(0.02), thin);
    let leg = k.leg(t, thin);
    let stiff =
        k.p.stiffeners
            .unwrap_or(col.dims.h.max(col.dims.b) >= 0.3 && !col.dims.round());
    let ts = cat
        .plate_t((cat.plate.stiff_ratio * col.dims.tf).max(0.008))
        .min(t);
    let pw = size.plate_washer;
    // anchors stand outside the faces across y, clear of the weld and their plate washers
    let c = pw / 2.0 + leg + cat.spacing.face_clear;
    let e = cat.e_layout(hole).max(pw / 2.0 + cat.spacing.face_clear);
    // anchors across x: gauge from the section width, spaced for stiffeners between them
    let gap_min = if stiff {
        pw / 2.0 + ts / 2.0 + leg + cat.spacing.face_clear
    } else {
        0.0
    };
    let p2 = (cat.spacing.p2_min * hole).max(pw + cat.spacing.face_clear);
    let mut gx = cat.round_up(fw / 2.0);
    let xs_of = |gx: f64| -> Vec<f64> {
        (0..per_side)
            .map(|i| -gx + 2.0 * gx * i as f64 / (per_side as f64 - 1.0))
            .collect()
    };
    loop {
        let xs = xs_of(gx);
        let step = xs[1] - xs[0];
        if step >= p2 - 1e-9 && step / 2.0 >= gap_min - 1e-9 {
            break;
        }
        gx += cat.spacing.round;
    }
    let xs = xs_of(gx);
    let ya = (y1 + c).max(-y0 + c);
    let ys = [-ya, ya];
    let w = cat
        .size_up(2.0 * gx + 2.0 * e)
        .max(cat.size_up(fw + 2.0 * (leg + 0.02)));
    let h = cat.size_up(2.0 * ya + 2.0 * e);
    let mut holes = vec![];
    for y in ys {
        for x in &xs {
            holes.push((v2(*x, y), hole));
        }
    }
    let pl = k.plate(Role::Plate, f, rect(0.0, 0.0, w, h), holes.clone(), t, -t);
    let below = cat.conn.grout + k.p.embed.unwrap_or(cat.conn.anchor_embed * size.d);
    for (p, _) in &holes {
        k.anchor(&size, f.at(v3(p.x, p.y, 0.0)), t, below, pl, f.x);
    }
    k.weld_section(col, k.node, up, leg);
    if stiff {
        let mids: Vec<f64> = xs.windows(2).map(|w| (w[0] + w[1]) / 2.0).collect();
        for x in mids {
            for side in [1.0, -1.0] {
                stiffener(k, col, &f, x, side, h / 2.0, ts, leg)?;
            }
        }
    }
    if col.u.dot(up) < 0.999 {
        k.note(format!(
            "inclined column: the base plate follows the column end plane ({:.1} deg off vertical)",
            col.u.dot(up).abs().min(1.0).acos().to_degrees()
        ));
    }
    Ok(())
}

/// Vertical stiffener in the plane x = `x` of the plate frame, from the section face out to
/// the plate edge on `side`, welded to the plate and the column face.
#[allow(clippy::too_many_arguments)]
fn stiffener(
    k: &mut Kit,
    col: &Bar,
    f: &crate::gsolid::Frame,
    x: f64,
    side: f64,
    edge: f64,
    ts: f64,
    leg: f64,
) -> Result<(), ConnError> {
    // face of the section along +-y at this x: march from the plate edge inward
    let probe = |y: f64| {
        let w = f.at(v3(x, y, 0.001));
        let q = col.local(w);
        col.contains(v2(q.y, q.z))
    };
    let mut yf = 0.0;
    let step = 0.0005;
    let mut y = side * edge;
    while y.abs() > 0.0 {
        if probe(y) {
            yf = y;
            break;
        }
        y -= side * step;
        if y * side <= 0.0 {
            return Ok(());
        }
    }
    let len = edge - yf.abs();
    if len < 0.03 {
        return Ok(());
    }
    let hs = (1.5 * len).clamp(0.1, 0.4);
    let v1 = (0.2 * hs).max(0.025);
    let snip = k.cat.plate.snip;
    let sx = v3(0.0, side, 0.0);
    let o = f.at(v3(x, yf, 0.0));
    let along = f.dir(sx);
    let outline = vec![
        v2(snip, 0.0),
        v2(len, 0.0),
        v2(len, v1),
        v2(0.0, hs),
        v2(0.0, snip),
    ];
    // the stiffener frame: x outward, y up, z across
    let sf = crate::gsolid::Frame {
        o,
        x: along,
        y: f.z,
        z: along.cross(f.z),
    };
    k.plate(Role::Stiffener, sf, outline, vec![], ts, -ts / 2.0);
    // to the base plate along its bottom edge, to the column face along its inner edge
    k.weld_both_sides(o + along * snip, along, len - snip, sf.z, f.z, ts, leg);
    k.weld_both_sides(o + f.z * snip, f.z, hs - snip, sf.z, along, ts, leg);
    Ok(())
}
