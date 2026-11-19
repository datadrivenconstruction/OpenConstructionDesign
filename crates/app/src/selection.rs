// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan selection: "window" box (left-to-right, fully inside only) and
//! "crossing" box (right-to-left, any touch), Ctrl adds, Shift removes, Tab cycles
//! candidates under the cursor ("2 of 3"), category filter with counters, "Select similar".
//! Pure functions over `Editor`; input and drawing live in `plan_view`.

use crate::editor::{Editor, Sel};
use crate::opening_tool::Kind;
use ddc_geom::{v2, V2};
use std::collections::HashSet;

/// Box mode by on-screen drag direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxMode {
    Window,
    Crossing,
}

impl BoxMode {
    /// Left-to-right is window, right-to-left is crossing.
    pub fn from_drag(x0: f32, x1: f32) -> Self {
        if x1 >= x0 {
            BoxMode::Window
        } else {
            BoxMode::Crossing
        }
    }
}

/// Element outlines in the plan (world, m).
pub fn outlines(ed: &Editor, s: Sel) -> Vec<Vec<V2>> {
    match s {
        Sel::Wall(id) => ed
            .walls
            .iter()
            .zip(&ed.plan)
            .find(|(w, _)| w.id == id)
            .map(|(_, o)| o.clone())
            .unwrap_or_default(),
        Sel::Opening(id) => opening_quad(ed, id).into_iter().collect(),
        Sel::Slab(id) => ed
            .slabs
            .iter()
            .find(|x| x.id == id)
            .and_then(|x| x.rings.first().cloned())
            .into_iter()
            .collect(),
        Sel::Room(id) => ed
            .rooms
            .iter()
            .find(|x| x.id == id)
            .and_then(|x| x.rings.first().cloned())
            .into_iter()
            .collect(),
        Sel::Curtain(id) | Sel::Panel(id, _) => ed
            .curtain_lines
            .iter()
            .filter(|l| l.0 == id)
            .map(|&(_, a, b)| quad(a, b, 0.05))
            .collect(),
        Sel::Mass(id) => ed
            .mass_plan
            .iter()
            .find(|m| m.0 == id)
            .map(|m| m.1.clone())
            .unwrap_or_default(),
        Sel::Mep(_) | Sel::Comp(_) => ed.big.outlines(s),
        Sel::Ext(c, id) => ed.struct_fp.rings(c, id).to_vec(),
        Sel::Anno(id) => crate::annot_text::outlines(ed, id),
        Sel::Body(k, id) => ed
            .bodies
            .iter()
            .find(|b| b.kind == k && b.id == id)
            .map(|b| b.hull.clone())
            .into_iter()
            .collect(),
    }
}

fn quad(a: V2, b: V2, half: f64) -> Vec<V2> {
    let d = b - a;
    let l = d.len().max(1e-12);
    let n = v2(-d.y / l * half, d.x / l * half);
    vec![a + n, b + n, b - n, a - n]
}

/// An opening as a rectangle within the host's thickness.
fn opening_quad(ed: &Editor, id: i64) -> Option<Vec<V2>> {
    let o = ed.opening(id)?;
    let w = ed.walls.iter().find(|w| w.id == o.wall)?;
    let t = ed
        .types
        .iter()
        .find(|t| t.id == w.wall_type)
        .map_or(0.2, |t| t.thickness());
    let d = w.b - w.a;
    let l = d.len().max(1e-12);
    let u = v2(d.x / l, d.y / l);
    let p0 = w.a + u * o.along;
    Some(quad(p0, p0 + u * o.width, t / 2.0 + 0.02))
}

/// All elements of the view (order: walls, openings, curtain walls, slabs, rooms, shapes in
/// the plan cut).
pub fn all(ed: &Editor) -> Vec<Sel> {
    let mut v: Vec<Sel> = ed.walls.iter().map(|w| Sel::Wall(w.id)).collect();
    v.extend(ed.openings.iter().map(|o| Sel::Opening(o.id)));
    v.extend(ed.curtains.iter().map(|c| Sel::Curtain(c.id)));
    v.extend(ed.slabs.iter().map(|s| Sel::Slab(s.id)));
    v.extend(ed.rooms.iter().map(|r| Sel::Room(r.id)));
    v.extend(ed.mass_plan.iter().map(|m| Sel::Mass(m.0)));
    v.extend(ed.big.plan_sels(ed.level));
    v.extend(ed.struct_fp.sels());
    v.extend(ed.bodies.iter().map(|b| Sel::Body(b.kind, b.id)));
    v
}

pub fn inside_poly(p: V2, poly: &[V2]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

fn seg_cross(a: V2, b: V2, c: V2, d: V2) -> bool {
    let o = |p: V2, q: V2, r: V2| (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
    let (d1, d2, d3, d4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

/// Elements in the box `a`-`b` (world). Window: all vertices inside; crossing: any touch.
pub fn in_box(ed: &Editor, a: V2, b: V2, mode: BoxMode, visible: impl Fn(Sel) -> bool) -> Vec<Sel> {
    let (lo, hi) = (
        v2(a.x.min(b.x), a.y.min(b.y)),
        v2(a.x.max(b.x), a.y.max(b.y)),
    );
    let inr = |p: V2| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y;
    let r = [lo, v2(hi.x, lo.y), hi, v2(lo.x, hi.y)];
    // bounds from the plan index: outlines are computed only for elements touching the box
    let near: std::collections::HashSet<Sel> = ed
        .big
        .plan_index(ed)
        .rect(lo, hi, 1e-9)
        .into_iter()
        .collect();
    all(ed)
        .into_iter()
        .filter(|s| near.contains(s))
        // annotations are not in the plan index: their bounds are already in the view cache
        .chain(crate::annot_text::plan_ids(ed).into_iter().map(Sel::Anno))
        .filter(|&s| visible(s))
        .filter(|&s| {
            let os = outlines(ed, s);
            if os.is_empty() {
                return false;
            }
            // a box inside a terrain must not take the terrain: only its outline counts
            let fill = !matches!(s, Sel::Ext(c, _) if c == crate::scene_ids::EXT_TOPO);
            match mode {
                BoxMode::Window => os.iter().flatten().all(|&p| inr(p)),
                BoxMode::Crossing => os.iter().any(|o| {
                    o.iter().any(|&p| inr(p))
                        || (fill && r.iter().any(|&c| inside_poly(c, o)))
                        || (0..o.len()).any(|i| {
                            let (p, q) = (o[i], o[(i + 1) % o.len()]);
                            (0..4).any(|k| seg_cross(p, q, r[k], r[(k + 1) % 4]))
                        })
                }),
            }
        })
        .collect()
}

/// Ctrl adds to the selection, Shift removes, otherwise replaces.
pub fn combine(cur: &[Sel], new: &[Sel], ctrl: bool, shift: bool) -> Vec<Sel> {
    if shift {
        return cur.iter().copied().filter(|s| !new.contains(s)).collect();
    }
    let mut v = if ctrl { cur.to_vec() } else { vec![] };
    for s in new {
        if !v.contains(s) {
            v.push(*s);
        }
    }
    v
}

/// Walls connected end-to-end to wall `id` (a chain), including it.
pub fn wall_chain(ed: &Editor, id: i64) -> Vec<Sel> {
    let near = |p: V2, q: V2| (p - q).len() < 1e-3;
    let mut seen = vec![id];
    let mut i = 0;
    while i < seen.len() {
        let Some(w) = ed.walls.iter().find(|w| w.id == seen[i]) else {
            break;
        };
        let (a, b) = (w.a, w.b);
        for o in &ed.walls {
            if !seen.contains(&o.id) && [o.a, o.b].iter().any(|&e| near(e, a) || near(e, b)) {
                seen.push(o.id);
            }
        }
        i += 1;
    }
    seen.into_iter().map(Sel::Wall).collect()
}

/// Tab candidates under the cursor: element -> wall chain -> host -> enclosing (room,
/// slab). `r` is the pick radius, m.
pub fn candidates(ed: &Editor, p: V2, r: f64, visible: impl Fn(Sel) -> bool) -> Vec<Vec<Sel>> {
    let hit = |s: Sel| {
        // a roof is picked by its edges only, as by a click
        let edge_only = matches!(s, Sel::Ext(c, _) if crate::editor::structx::edge_only(c));
        outlines(ed, s).iter().any(|o| {
            (!edge_only && inside_poly(p, o))
                || (0..o.len()).any(|i| {
                    let (a, b) = (o[i], o[(i + 1) % o.len()]);
                    (crate::snap::closest_on_segment(p, a, b).0 - p).len() <= r
                })
        })
    };
    let mut v: Vec<Vec<Sel>> = vec![];
    for id in crate::annot_text::hits(ed, p, r) {
        if visible(Sel::Anno(id)) {
            v.push(vec![Sel::Anno(id)]);
        }
    }
    for o in &ed.openings {
        if visible(Sel::Opening(o.id)) && hit(Sel::Opening(o.id)) {
            v.push(vec![Sel::Opening(o.id)]);
            v.push(vec![Sel::Wall(o.wall)]);
        }
    }
    for w in &ed.walls {
        let s = Sel::Wall(w.id);
        if visible(s) && hit(s) {
            if !v.contains(&vec![s]) {
                v.push(vec![s]);
            }
            let ch = wall_chain(ed, w.id);
            if ch.len() > 1 && !v.contains(&ch) {
                v.push(ch);
            }
        }
    }
    for s in all(ed) {
        let kind = matches!(
            s,
            Sel::Curtain(_)
                | Sel::Room(_)
                | Sel::Slab(_)
                | Sel::Mass(_)
                | Sel::Mep(_)
                | Sel::Comp(_)
                | Sel::Ext(..)
                | Sel::Body(..)
        );
        if kind && visible(s) && hit(s) {
            v.push(vec![s]);
        }
    }
    v
}

/// Tab cycle: the point where it started and the current index.
#[derive(Clone, Debug, Default)]
pub struct Cycle {
    pub at: Option<V2>,
    pub list: Vec<Vec<Sel>>,
    pub idx: usize,
}

impl Cycle {
    /// Next candidate; a new point (farther than `r`) restarts the cycle.
    pub fn next(
        &mut self,
        ed: &Editor,
        p: V2,
        r: f64,
        visible: impl Fn(Sel) -> bool,
    ) -> Option<&[Sel]> {
        if self.at.is_none_or(|a| (a - p).len() > r) {
            self.list = candidates(ed, p, r, visible);
            self.at = Some(p);
            self.idx = 0;
        } else if !self.list.is_empty() {
            self.idx = (self.idx + 1) % self.list.len();
        }
        self.list.get(self.idx).map(|v| v.as_slice())
    }
    /// "2 of 3" (1-based); None if fewer than two candidates.
    pub fn label(&self) -> Option<(usize, usize)> {
        (self.list.len() > 1).then_some((self.idx + 1, self.list.len()))
    }
    pub fn current(&self) -> Option<&[Sel]> {
        self.list.get(self.idx).map(|v| v.as_slice())
    }
}

/// Filter category key.
pub fn category(ed: &Editor, s: Sel) -> &'static str {
    match s {
        Sel::Wall(_) => "wall",
        Sel::Opening(id) => match ed.opening(id).map(|o| o.kind) {
            Some(Kind::Void) => "opening",
            Some(Kind::Window) => "window",
            _ => "door",
        },
        Sel::Curtain(_) | Sel::Panel(..) => "curtain",
        Sel::Slab(_) => "floor",
        Sel::Room(_) => "room",
        Sel::Mass(_) => "mass",
        Sel::Mep(_) => "mep",
        Sel::Comp(_) => "comp",
        Sel::Anno(_) => "anno",
        Sel::Ext(c, _) => crate::editor::structx::category_of(c),
        Sel::Body(k, _) => crate::editor::bodies::category_key(k),
    }
}

pub const CATEGORIES: [&str; 25] = [
    "wall",
    "door",
    "window",
    "opening",
    "curtain",
    "floor",
    "room",
    "imported",
    "column",
    "beam",
    "ceiling",
    "mass",
    "mep",
    "comp",
    "anno",
    crate::editor::structx::CATEGORY,
    "ramp",
    "room_sep",
    "zone",
    "roof",
    "stair",
    "railing",
    "shaft",
    "grid",
    "ref_plane",
];

/// Selection categories with counts (non-empty only, in `CATEGORIES` order).
pub fn counts(ed: &Editor, sel: &[Sel]) -> Vec<(&'static str, usize)> {
    CATEGORIES
        .iter()
        .map(|&c| (c, sel.iter().filter(|&&s| category(ed, s) == c).count()))
        .filter(|x| x.1 > 0)
        .collect()
}

/// Keep only the checked categories.
pub fn filter(ed: &Editor, sel: &[Sel], keep: &HashSet<&str>) -> Vec<Sel> {
    sel.iter()
        .copied()
        .filter(|&s| keep.contains(category(ed, s)))
        .collect()
}

/// "Select similar": same type (walls by type, openings by kind and size) in the view or in the
/// whole project (all model levels).
pub fn similar(ed: &Editor, s: Sel, project: bool, visible: impl Fn(Sel) -> bool) -> Vec<Sel> {
    if project
        && matches!(
            s,
            Sel::Opening(_) | Sel::Slab(_) | Sel::Room(_) | Sel::Curtain(_)
        )
    {
        return crate::query::same_type_project(ed, s);
    }
    match s {
        Sel::Wall(id) => {
            let Some(ty) = ed.walls.iter().find(|w| w.id == id).map(|w| w.wall_type) else {
                return vec![];
            };
            if project {
                let mut st = match ed
                    .model
                    .sql()
                    .prepare("SELECT id FROM wall WHERE wtype=?1 ORDER BY id")
                {
                    Ok(st) => st,
                    Err(_) => return vec![],
                };
                let ids = st.query_map([ty], |r| r.get::<_, i64>(0));
                return ids
                    .map(|it| it.flatten().map(Sel::Wall).collect())
                    .unwrap_or_default();
            }
            ed.walls
                .iter()
                .filter(|w| w.wall_type == ty)
                .map(|w| Sel::Wall(w.id))
                .filter(|&s| visible(s))
                .collect()
        }
        Sel::Opening(id) => {
            let Some(o) = ed.opening(id).copied() else {
                return vec![];
            };
            ed.openings
                .iter()
                .filter(|q| {
                    q.kind == o.kind
                        && (q.width - o.width).abs() < 1e-6
                        && (q.height - o.height).abs() < 1e-6
                })
                .map(|q| Sel::Opening(q.id))
                .filter(|&s| project || visible(s))
                .collect()
        }
        Sel::Anno(_) => crate::annot_text::plan_ids(ed)
            .into_iter()
            .map(Sel::Anno)
            .filter(|&x| project || visible(x))
            .collect(),
        Sel::Ext(c, _) => all(ed)
            .into_iter()
            .filter(|&x| matches!(x, Sel::Ext(k, _) if k == c))
            .filter(|&x| project || visible(x))
            .collect(),
        other => all(ed)
            .into_iter()
            .filter(|&x| std::mem::discriminant(&x) == std::mem::discriminant(&other))
            .filter(|&x| project || visible(x))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    #[test]
    fn window_needs_whole_element_crossing_touches() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((6.0, 0.0), (10.0, 0.0))],
        );
        let all = |_| true;
        let w = in_box(&ed, v2(-1.0, -1.0), v2(5.0, 1.0), BoxMode::Window, all);
        assert_eq!(w, vec![Sel::Wall(ed.walls[0].id)]);
        let c = in_box(&ed, v2(-1.0, -1.0), v2(7.0, 1.0), BoxMode::Crossing, all);
        assert_eq!(c.len(), 2);
        // a box fully inside a wall: crossing catches it, window does not
        let inner = (v2(1.0, -0.01), v2(2.0, 0.01));
        assert_eq!(
            in_box(&ed, inner.0, inner.1, BoxMode::Crossing, all).len(),
            1
        );
        assert!(in_box(&ed, inner.0, inner.1, BoxMode::Window, all).is_empty());
        assert_eq!(BoxMode::from_drag(10.0, 2.0), BoxMode::Crossing);
    }

    #[test]
    fn ctrl_adds_shift_removes() {
        let (a, b, c) = (Sel::Wall(1), Sel::Wall(2), Sel::Wall(3));
        assert_eq!(combine(&[a], &[b], true, false), vec![a, b]);
        assert_eq!(combine(&[a, b], &[b, c], false, true), vec![a]);
        assert_eq!(combine(&[a], &[c], false, false), vec![c]);
    }

    #[test]
    fn tab_cycles_wall_chain_and_counts() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((4.0, 0.0), (4.0, 3.0))],
        );
        let mut cy = Cycle::default();
        let first = cy.next(&ed, v2(2.0, 0.0), 0.1, |_| true).unwrap().to_vec();
        assert_eq!(first, vec![Sel::Wall(ed.walls[0].id)]);
        assert_eq!(cy.label(), Some((1, 2)));
        let second = cy.next(&ed, v2(2.0, 0.0), 0.1, |_| true).unwrap().to_vec();
        assert_eq!(second.len(), 2);
        assert_eq!(cy.label(), Some((2, 2)));
        cy.next(&ed, v2(2.0, 0.0), 0.1, |_| true);
        assert_eq!(cy.label(), Some((1, 2)));
    }

    #[test]
    fn filter_and_similar() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))],
        );
        let s = all(&ed);
        assert_eq!(counts(&ed, &s), vec![("wall", 2)]);
        let keep: HashSet<&str> = ["door"].into();
        assert!(filter(&ed, &s, &keep).is_empty());
        let sim = similar(&ed, s[0], false, |_| true);
        assert_eq!(sim.len(), 2);
        assert_eq!(similar(&ed, s[0], true, |_| true).len(), 2);
        assert_eq!(similar(&ed, s[0], false, |x| x != s[1]).len(), 1);
    }
}
