// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Conditional wiring of a plan after DIN EN 60617-11: per circuit, its devices on the level
//! joined by bowed lines from the one nearest the panel, and a home run from that device to the
//! panel ending in an arrow, with a tick per conductor and the label "F3 / NYM-J 3x1.5". Derived
//! from the circuits on every call; nothing is stored.

use std::collections::HashMap;

use crate::{Error, Model};

/// Bow of a chain line, share of its chord.
const BOW: f64 = 0.15;
/// Points per chain line after the first.
const STEPS: usize = 8;
/// Length of a home run whose panel stands on another level, m.
const HOME_AWAY: f64 = 1.0;

/// Radius a cable is drawn with in 3D, m: thicker than a NYM-J so the route reads across a floor.
pub const CABLE_DRAWN_R: f64 = 0.025;
/// Sides of the drawn cable.
const CABLE_SIDES: usize = 8;

/// The body a cable piece is drawn and exported with.
pub fn cable_mesh(points: &[(f64, f64, f64)]) -> ddc_geom::mesh::Mesh {
    let p: Vec<ddc_geom::math::V3> = points.iter().map(|q| ddc_geom::math::v3(q.0, q.1, q.2)).collect();
    ddc_geom::rebar::tube(&p, CABLE_DRAWN_R, CABLE_SIDES)
}

/// The drawn wiring of one circuit on one level, plan points in m.
#[derive(Clone, Debug, PartialEq)]
pub struct CircuitWire {
    pub circuit: i64,
    /// Bowed lines from device to device.
    pub arcs: Vec<Vec<(f64, f64)>>,
    /// From the first device to the panel, or toward it when the panel is on another level.
    pub home: [(f64, f64); 2],
    /// Conductors of the cable: 3 single-phase, 5 three-phase.
    pub cores: u32,
    pub label: String,
}

/// A quadratic bow from `a` to `b`, bulging to the left of the chord.
pub fn bow(a: (f64, f64), b: (f64, f64)) -> Vec<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let c = ((a.0 + b.0) / 2.0 - dy * BOW * 2.0, (a.1 + b.1) / 2.0 + dx * BOW * 2.0);
    (0..=STEPS)
        .map(|i| {
            let t = i as f64 / STEPS as f64;
            let u = 1.0 - t;
            (
                u * u * a.0 + 2.0 * u * t * c.0 + t * t * b.0,
                u * u * a.1 + 2.0 * u * t * c.1 + t * t * b.1,
            )
        })
        .collect()
}

/// Cross-section as written on a drawing: 1.5, 2.5, 16.
fn mm2(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map_or(s.clone(), str::to_string)
}

impl Model {
    /// The wiring drawn on a level's plan, by circuit; a circuit with no device there has none.
    pub fn circuit_wires(&self, level: i64) -> Result<Vec<CircuitWire>, Error> {
        let mut at: HashMap<i64, (i64, f64, f64)> = HashMap::new();
        {
            // a hosted component stores its spot along the host: the plan point is the derived one
            let mut st = self.db.prepare(
                "SELECT c.id, c.level, COALESCE(q.wx, c.x), COALESCE(q.wy, c.y) FROM comp c LEFT JOIN comp_qty q ON q.comp = c.id",
            )?;
            let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
            for x in r {
                let (id, p) = x?;
                at.insert(id, p);
            }
        }
        let d2 = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2);
        let mut out = vec![];
        for c in self.circuits(None)? {
            let Some(&(plevel, px, py)) = at.get(&c.spec.panel) else { continue };
            let panel = (px, py);
            let mut left: Vec<(f64, f64)> = vec![];
            for d in &c.spec.devices {
                if let Some(&(l, x, y)) = at.get(&d.comp) {
                    if l == level && !left.iter().any(|q| d2(*q, (x, y)) < 1e-12) {
                        left.push((x, y));
                    }
                }
            }
            if left.is_empty() {
                continue;
            }
            // the chain grows from the device nearest the panel to the nearest one left
            let first = (0..left.len()).min_by(|&i, &j| d2(left[i], panel).total_cmp(&d2(left[j], panel))).unwrap_or(0);
            let mut cur = left.swap_remove(first);
            let start = cur;
            let mut arcs = vec![];
            while !left.is_empty() {
                let k = (0..left.len()).min_by(|&i, &j| d2(left[i], cur).total_cmp(&d2(left[j], cur))).unwrap_or(0);
                let next = left.swap_remove(k);
                arcs.push(bow(cur, next));
                cur = next;
            }
            let end = if plevel == level {
                panel
            } else {
                let l = d2(start, panel).sqrt();
                let (ux, uy) = if l > 1e-9 { ((panel.0 - start.0) / l, (panel.1 - start.1) / l) } else { (0.0, 1.0) };
                (start.0 + ux * HOME_AWAY, start.1 + uy * HOME_AWAY)
            };
            let cores = c.spec.poles + 2;
            let cable = if c.spec.cable_type.is_empty() {
                format!("{cores}x{}", mm2(c.spec.cable))
            } else {
                c.spec.cable_type.clone()
            };
            out.push(CircuitWire {
                circuit: c.id,
                arcs,
                home: [start, end],
                cores,
                label: format!("F{} / {cable}", c.spec.number),
            });
        }
        Ok(out)
    }

    /// Cable pieces of the circuits, (circuit, piece) on one level or on all.
    pub fn circuit_cables(&self, level: Option<i64>) -> Result<Vec<(i64, super::CableTrace)>, Error> {
        let mut out = vec![];
        for c in self.circuits(None)? {
            for t in c.spec.trace {
                if level.is_none_or(|l| l == t.level) {
                    out.push((c.id, t));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bow_runs_from_end_to_end_off_its_chord() {
        let b = bow((0.0, 0.0), (4.0, 0.0));
        assert_eq!(b.len(), STEPS + 1);
        assert_eq!(b[0], (0.0, 0.0));
        assert!((b[STEPS].0 - 4.0).abs() < 1e-12 && b[STEPS].1.abs() < 1e-12);
        let top = b.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        assert!((top - 4.0 * BOW).abs() < 1e-9, "{top}");
    }

    #[test]
    fn sections_print_as_drawn() {
        assert_eq!(mm2(1.5), "1.5");
        assert_eq!(mm2(16.0), "16");
    }
}
