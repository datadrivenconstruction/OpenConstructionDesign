// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan symbols of a door from its type: each swing leaf drawn open at 90° with its swing arc,
//! sliding panels with their travel; in the opening's local plan frame (x along, y across).

use super::opening::{DoorOp, DoorType, Placement};
use crate::math::{v2, V2};
use std::f64::consts::{FRAC_PI_2, PI};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlanPrim {
    Line(V2, V2),
    /// Arc about `c` of radius `r` from angle `a0` counter-clockwise to `a1` (radians).
    Arc { c: V2, r: f64, a0: f64, a1: f64 },
}

pub fn door_symbol(t: &DoorType, p: &Placement) -> Vec<PlanPrim> {
    let w = p.width.max(0.2);
    let half = (p.wall_t / 2.0).max(0.03);
    let f = t.frame.face.min(w / 6.0).max(0.01);
    let (x0, x1) = (f, w - f);
    let mut out = vec![];
    match t.op {
        DoorOp::Swing => {
            let leaves: Vec<(f64, f64, bool)> = if t.leaves >= 2 {
                let m = (x0 + x1) / 2.0;
                vec![(x0, m - x0, false), (x1, x1 - m, true)]
            } else {
                vec![(x0, x1 - x0, false)]
            };
            for (hx, lw, hi) in leaves {
                let c = v2(hx, half);
                out.push(PlanPrim::Line(c, c + v2(0.0, lw)));
                let (a0, a1) = if hi { (FRAC_PI_2, PI) } else { (0.0, FRAC_PI_2) };
                out.push(PlanPrim::Arc { c, r: lw, a0, a1 });
            }
        }
        DoorOp::Sliding => {
            let m = (x0 + x1) / 2.0;
            let y = half * 0.3;
            out.push(PlanPrim::Line(v2(x0, y), v2(m, y)));
            out.push(PlanPrim::Line(v2(m, -y), v2(x1, -y)));
            // travel arrows
            out.push(PlanPrim::Line(v2(m - 0.1, y + 0.08), v2(m + 0.1, y + 0.08)));
            out.push(PlanPrim::Line(v2(m + 0.1, -y - 0.08), v2(m - 0.1, -y - 0.08)));
        }
    }
    let mx = |q: V2| if p.hinge_flip { v2(w - q.x, q.y) } else { q };
    let my = |q: V2| if p.swing_flip { v2(q.x, -q.y) } else { q };
    out.into_iter()
        .map(|g| match g {
            PlanPrim::Line(a, b) => PlanPrim::Line(my(mx(a)), my(mx(b))),
            PlanPrim::Arc { c, r, a0, a1 } => {
                // a mirror reverses the sweep: the angles swap after reflecting
                let (mut a0, mut a1) = (a0, a1);
                if p.hinge_flip {
                    (a0, a1) = (PI - a1, PI - a0);
                }
                if p.swing_flip {
                    (a0, a1) = (-a1, -a0);
                }
                PlanPrim::Arc { c: my(mx(c)), r, a0, a1 }
            }
        })
        .collect()
}
