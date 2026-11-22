// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Conditional wiring on the plan (DIN EN 60617-11): dashed bows from device to device, a home
//! run with an arrow at the panel, a tick per conductor and the circuit label. Its own layer:
//! the view category `elec_circuit` turns it off and on.

use std::sync::Arc;

use ddc_geom::V2;
use ddc_model::CircuitWire;
use egui::{Color32, Pos2, Stroke};

use crate::editor::Editor;
use crate::ui_kit::Tokens;

/// View category of the wiring layer.
pub const CATEGORY: &str = "elec_circuit";
/// Line width, px.
const LINE_PX: f32 = 1.2;
/// Dash and gap of a concealed line, px.
const DASH: (f32, f32) = (6.0, 3.0);
/// Arrow length and half width, px.
const ARROW: (f32, f32) = (10.0, 4.0);
/// Tick length and spacing, and how far back from the arrow they start, px.
const TICK: (f32, f32, f32) = (8.0, 4.0, 18.0);
/// Label size, px.
const LABEL_PX: f32 = 11.0;
/// Label anchor from the device end of the home run and step along it per retry, px; the home
/// runs of a panel meet there, so labels stay by their first devices.
const LABEL_AT: (f32, f32) = (14.0, 16.0);
/// Spots tried per label, along the line on either side, before it is left out.
const LABEL_TRIES: usize = 6;

/// Wiring of the level, rebuilt when the model or the level changes.
#[derive(Default)]
pub struct WireCache(Option<((u64, i64), Arc<Vec<CircuitWire>>)>);

impl WireCache {
    pub fn get(&mut self, ed: &Editor) -> Arc<Vec<CircuitWire>> {
        let key = (ed.revision, ed.level);
        match &self.0 {
            Some((k, w)) if *k == key => w.clone(),
            _ => {
                let w = Arc::new(ed.model.circuit_wires(ed.level).unwrap_or_default());
                self.0 = Some((key, w.clone()));
                w
            }
        }
    }
}

/// Whether the plan of the level shows the wiring.
pub fn shown(hidden_cats: &[String]) -> bool {
    !hidden_cats.iter().any(|c| c == CATEGORY)
}

fn dashed(painter: &egui::Painter, pts: &[Pos2], st: Stroke) {
    painter.extend(egui::Shape::dashed_line(pts, st, DASH.0, DASH.1));
}

/// Ticks across the line `a`→`b` near `b`, one per conductor.
pub fn ticks(a: Pos2, b: Pos2, n: u32) -> Vec<[Pos2; 2]> {
    let d = b - a;
    let len = d.length();
    if len < 1e-3 {
        return vec![];
    }
    let u = d / len;
    let nrm = egui::vec2(-u.y, u.x);
    // oblique ticks, as drawn by hand
    let slant = (nrm + u * 0.5).normalized() * (TICK.0 / 2.0);
    let back = TICK.2.min(len * 0.5);
    (0..n)
        .map(|i| {
            let c = b - u * (back + i as f32 * TICK.1);
            [c - slant, c + slant]
        })
        .collect()
}

/// Top-left corners of labels of `size` px set beside the home runs `a`→`b`, near `a`: the first
/// spot clear of the labels already set, else none.
pub fn place_labels(runs: &[(Pos2, Pos2, egui::Vec2)]) -> Vec<Option<Pos2>> {
    let mut set: Vec<egui::Rect> = vec![];
    runs.iter()
        .map(|&(a, b, size)| {
            let d = b - a;
            let len = d.length();
            let u = if len > 1e-3 { d / len } else { egui::vec2(1.0, 0.0) };
            let n = egui::vec2(-u.y, u.x);
            let spot = (0..LABEL_TRIES * 2).find_map(|i| {
                let along = (LABEL_AT.0 + (i / 2) as f32 * LABEL_AT.1).min(len.max(LABEL_AT.0));
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                let q = a + u * along + n * side * (size.y * 0.5 + 2.0);
                let r = egui::Rect::from_center_size(q + n * side * (size.x * 0.5 * n.x.abs()), size);
                (!set.iter().any(|o| o.expand(1.0).intersects(r))).then_some(r)
            })?;
            set.push(spot);
            Some(spot.min)
        })
        .collect()
}

pub fn paint_plan(
    painter: &egui::Painter,
    wires: &[CircuitWire],
    hidden_cats: &[String],
    to_s: &dyn Fn(V2) -> Pos2,
    t: &Tokens,
) {
    if wires.is_empty() || !shown(hidden_cats) {
        return;
    }
    let col: Color32 = t.accent;
    let st = Stroke::new(LINE_PX, col);
    let p = |q: (f64, f64)| to_s(ddc_geom::v2(q.0, q.1));
    for w in wires {
        for arc in &w.arcs {
            let pts: Vec<Pos2> = arc.iter().map(|&q| p(q)).collect();
            dashed(painter, &pts, st);
        }
        let (a, b) = (p(w.home[0]), p(w.home[1]));
        painter.line_segment([a, b], st);
        let d = b - a;
        if d.length() > ARROW.0 {
            let u = d.normalized();
            let n = egui::vec2(-u.y, u.x);
            let base = b - u * ARROW.0;
            painter.add(egui::Shape::convex_polygon(
                vec![b, base + n * ARROW.1, base - n * ARROW.1],
                col,
                Stroke::NONE,
            ));
            for tk in ticks(a, base, w.cores) {
                painter.line_segment(tk, st);
            }
        }
    }
    let galleys: Vec<_> = wires
        .iter()
        .map(|w| painter.layout_no_wrap(w.label.clone(), egui::FontId::proportional(LABEL_PX), t.text))
        .collect();
    let runs: Vec<(Pos2, Pos2, egui::Vec2)> =
        wires.iter().zip(&galleys).map(|(w, g)| (p(w.home[0]), p(w.home[1]), g.size())).collect();
    for (at, g) in place_labels(&runs).into_iter().zip(galleys) {
        if let Some(at) = at {
            painter.galley(at, g, t.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_tick_per_conductor_across_the_line() {
        let k = ticks(Pos2::new(0.0, 0.0), Pos2::new(100.0, 0.0), 5);
        assert_eq!(k.len(), 5);
        for [a, b] in &k {
            assert!((a.y - b.y).abs() > 1.0, "a tick crosses the line");
            assert!(a.y.signum() != b.y.signum());
        }
        assert!(ticks(Pos2::new(0.0, 0.0), Pos2::new(0.0, 0.0), 3).is_empty());
    }

    #[test]
    fn labels_never_overlap_and_stay_by_their_devices() {
        let size = egui::vec2(90.0, 12.0);
        // home runs of one panel: their far ends meet, their devices lie apart or together
        let panel = Pos2::new(0.0, 0.0);
        let mut runs: Vec<(Pos2, Pos2, egui::Vec2)> = (0..8).map(|i| (Pos2::new(200.0, -60.0 + 15.0 * i as f32), panel, size)).collect();
        runs.extend((0..30).map(|_| (Pos2::new(-150.0, 40.0), panel, size)));
        let at = place_labels(&runs);
        let set: Vec<egui::Rect> = at.iter().flatten().map(|p| egui::Rect::from_min_size(*p, size)).collect();
        for (i, a) in set.iter().enumerate() {
            for b in &set[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
            }
        }
        assert!(at[..8].iter().all(Option::is_some), "spread labels all set");
        assert!(at[8..].iter().any(Option::is_none), "a pile of labels is thinned");
        for (r, (a, _, _)) in set.iter().zip(&runs) {
            assert!(r.center().distance(*a) < 150.0, "label far from its device");
        }
    }

    #[test]
    fn the_category_hides_the_layer() {
        assert!(shown(&[]));
        assert!(!shown(&[CATEGORY.to_string()]));
        assert!(ddc_model::GFX_CATEGORIES.iter().any(|c| c.0 == CATEGORY));
    }
}
