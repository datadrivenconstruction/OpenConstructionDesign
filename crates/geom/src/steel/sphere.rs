// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ball node of a space frame or dome: a solid ball, and on each member a hexagon sleeve, a cone
//! welded to the tube end and a bolt screwed into the ball. The ball is the smallest of the
//! series on which the sleeves of the closest pair of members keep a free angle.

use super::bar::{meet, Bar};
use super::catalog::fmt_mm;
use super::kit::Kit;
use super::part::{self, frame_from, PartQty, Role};
use super::ConnError;
use crate::math::{v2, v3, V2, V3};
use std::f64::consts::FRAC_PI_2;

pub(super) fn ball(k: &mut Kit, bars: &[Bar]) -> Result<(), ConnError> {
    let refs: Vec<&Bar> = bars.iter().collect();
    let node = meet(&refs)?;
    k.node = node;
    let far = bars
        .iter()
        .map(|b| (b.axis(b.local(node).x) - node).len())
        .fold(0.0, f64::max);
    if far > 0.05 {
        return Err(ConnError(format!(
            "the member axes miss a common point by {} mm",
            fmt_mm(far)
        )));
    }
    let dirs: Vec<V3> = bars.iter().map(|b| b.end_near(node).2).collect();
    let sb: Vec<_> = bars
        .iter()
        .map(|b| k.cat.sphere_bolt(b.dims.h.max(b.dims.b)).clone())
        .collect();
    // sleeve corner radius; the sleeve footprints on the ball must keep the free angle
    let rs: Vec<f64> = sb.iter().map(|s| s.sleeve_s / 3f64.sqrt()).collect();
    let gap = k.cat.conn.ball_gap;
    let mut need = 0.0f64;
    for i in 0..bars.len() {
        for j in i + 1..bars.len() {
            let th = dirs[i].dot(dirs[j]).clamp(-1.0, 1.0).acos();
            if th < gap + 1e-3 {
                return Err(ConnError(format!(
                    "members {} and {} meet at {:.1} deg: too close for a ball node",
                    i + 1,
                    j + 1,
                    th.to_degrees()
                )));
            }
            // smallest R with asin(ri/R) + asin(rj/R) + gap <= th, by bisection
            let fits = |r: f64| {
                rs[i] < r && rs[j] < r && (rs[i] / r).asin() + (rs[j] / r).asin() + gap <= th
            };
            let (mut lo, mut hi) = (rs[i].max(rs[j]), 10.0);
            if !fits(hi) {
                return Err(ConnError::new("no ball size separates the sleeves"));
            }
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if fits(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            need = need.max(hi);
        }
    }
    // the deepest thread must stay inside the ball
    let thread = sb
        .iter()
        .map(|s| k.cat.conn.ball_thread * s.d)
        .fold(0.0, f64::max);
    need = need.max(thread * 1.6);
    let d_ball = match k.p.ball {
        Some(d) if d / 2.0 + 1e-9 >= need => d,
        Some(d) => {
            return Err(ConnError(format!(
                "a ball of {} mm is too small: the sleeves need {} mm",
                fmt_mm(d),
                fmt_mm(k.cat.size_up(2.0 * need))
            )))
        }
        None => k.cat.ball_d(2.0 * need).ok_or_else(|| {
            ConnError(format!(
                "no ball of the series reaches {} mm",
                fmt_mm(2.0 * need)
            ))
        })?,
    };
    let r = d_ball / 2.0;
    // ball: a half circle turned about the vertical, kept a hair off the axis
    let rho = 0.0005;
    let n = 24;
    let top = (r * r - rho * rho).sqrt();
    let mut prof: Vec<V2> = (0..=n)
        .map(|i| {
            let a = -FRAC_PI_2 + std::f64::consts::PI * i as f64 / n as f64;
            v2((r * a.cos()).max(rho), r * a.sin())
        })
        .collect();
    prof[0] = v2(rho, -top);
    prof[n] = v2(rho, top);
    let fb = frame_from(node, v3(0.0, 0.0, 1.0), v3(1.0, 0.0, 0.0));
    let grade = k.cat.plate.grade.clone();
    let ball_mark = format!("ball D{}", fmt_mm(d_ball));
    k.add(part::revolve(
        Role::Ball,
        fb,
        prof,
        format!("{grade};ball"),
        ball_mark,
    ));
    for (i, b) in bars.iter().enumerate() {
        let s = &sb[i];
        let size = k.bolt_size(s.d)?;
        let dir = dirs[i];
        let f = frame_from(
            node,
            dir,
            if dir.z.abs() < 0.9 {
                v3(0.0, 0.0, 1.0)
            } else {
                b.ey
            },
        );
        // sleeve seated on the ball with its corners on the surface
        let s0 = (r * r - rs[i] * rs[i]).max(0.0).sqrt();
        let s1 = s0 + s.sleeve_len;
        let hole = size.hole;
        let mark = format!("sleeve {} S{}", size.name(), fmt_mm(s.sleeve_s));
        let sleeve_mat = format!("{grade};sleeve");
        k.add(part::hex_prism(
            Role::Sleeve,
            f,
            s.sleeve_s,
            s0,
            s1,
            hole,
            sleeve_mat,
            mark,
        ));
        let tube_r = b.dims.h.max(b.dims.b) / 2.0;
        let r0 = (s.sleeve_s / 2.0).min(tube_r);
        let s2 = s1 + k.cat.conn.cone_len * 2.0 * tube_r;
        let cone_mark = format!("cone D{} L{}", fmt_mm(2.0 * tube_r), fmt_mm(s2 - s1));
        k.add(part::cone(
            f,
            r0,
            tube_r,
            s1,
            s2,
            format!("{grade};cone"),
            cone_mark,
        ));
        // bolt: thread in the ball, shank through the sleeve, head behind the cone end
        let depth = k.cat.conn.ball_thread * size.d;
        let need = depth + (s1 - s0) + (r - s0) + size.d;
        let length = k.cat.bolt_length(need).ok_or_else(|| {
            ConnError(format!(
                "no stock length for {} of {} mm",
                size.name(),
                fmt_mm(need)
            ))
        })?;
        let z0 = r - depth;
        let shank = part::section(part::ngon(v2(0.0, 0.0), size.d / 2.0, 12, 0.0), vec![]);
        k.add(part::Part {
            role: Role::Bolt,
            frame: f,
            form: part::prism(shank, z0, z0 + length),
            material: format!("{};bolt={};hole={}", k.class, size.name(), fmt_mm(hole)),
            mark: format!("{}x{} {}", size.name(), fmt_mm(length), k.class),
            qty: PartQty::Bolt {
                d: size.d,
                class: k.class.clone(),
                length,
                anchor: false,
            },
        });
        let leg = k.leg(b.dims.thin(), b.dims.thin());
        k.weld_section(b, node + dir * s2, dir, leg);
        k.trim(i, b.end_near(node).0, s2);
        if !b.dims.round() {
            k.note(format!(
                "member {} is not a round tube: the cone is round",
                i + 1
            ));
        }
    }
    Ok(())
}
