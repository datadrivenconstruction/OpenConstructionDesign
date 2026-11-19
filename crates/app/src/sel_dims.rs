// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Temporary dimensions of a selected wall: the clear distance from its axis to the nearest
//! parallel wall on each side. Click the value to type an exact distance (the wall moves), click
//! the lock to keep that distance as a constraint (`LockDist`). Shown while no grip is in use.
use crate::editor::{Editor, OpFn, Sel};
use crate::wall_tool::{Event, Key};
use ddc_geom::V2;
use ddc_model::{ConstraintKind, ConstraintSpec, Model, Target};
use std::cell::RefCell;
use std::sync::Arc;

/// Parallel within this angle (radians) counts as a neighbour.
const PARALLEL: f64 = 1e-3;
/// Lock glyph sits this share along the dimension line from the selected wall.
const LOCK_AT: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelDim {
    pub wall: i64,
    pub other: i64,
    /// On the selected wall's axis → foot on the neighbour's axis.
    pub a: V2,
    pub b: V2,
    pub value: f64,
}

impl SelDim {
    pub fn mid(&self) -> V2 {
        self.a + (self.b - self.a) * 0.5
    }
    pub fn lock_pos(&self) -> V2 {
        self.a + (self.b - self.a) * LOCK_AT
    }
    /// Move of the selected wall that makes the distance `v`.
    pub fn shift_for(&self, v: f64) -> V2 {
        let d = self.b - self.a;
        let l = d.len().max(1e-12);
        d * ((l - v) / l)
    }
}

thread_local! {
    /// (dimension index, typed text) while an exact distance is being typed.
    static TYPING: RefCell<Option<(usize, String)>> = const { RefCell::new(None) };
}

pub fn typing() -> Option<(usize, String)> {
    TYPING.with(|t| t.borrow().clone())
}
fn set_typing(v: Option<(usize, String)>) {
    TYPING.with(|t| *t.borrow_mut() = v);
}

/// Dimensions for the current selection: exactly one straight wall, no grip in use.
pub fn for_selection(ed: &Editor) -> Vec<SelDim> {
    if ed.grip.active.is_some() || ed.grip.last.is_some() {
        return vec![];
    }
    match ed.selection.as_slice() {
        [Sel::Wall(w)] => for_wall(ed, *w),
        _ => vec![],
    }
}

/// Nearest parallel straight wall on each side whose axis overlaps along this one.
pub fn for_wall(ed: &Editor, id: i64) -> Vec<SelDim> {
    let Some(w) = ed.walls.iter().find(|w| w.id == id && w.bulge == 0.0) else {
        return vec![];
    };
    let d = w.b - w.a;
    let l = d.len();
    if l < 1e-9 {
        return vec![];
    }
    let u = d * (1.0 / l);
    let n = ddc_geom::v2(-u.y, u.x);
    let m = w.a + d * 0.5;
    let mut best: [Option<(f64, i64, V2, f64)>; 2] = [None, None];
    for o in ed.walls.iter().filter(|o| o.id != id && o.bulge == 0.0) {
        let od = o.b - o.a;
        let ol = od.len();
        if ol < 1e-9 || (u.x * od.y - u.y * od.x).abs() / ol > PARALLEL {
            continue;
        }
        let (s0, s1) = ((o.a - w.a).dot(u), (o.b - w.a).dot(u));
        let (lo, hi) = (s0.min(s1), s0.max(s1));
        if hi <= 0.0 || lo >= l {
            continue;
        }
        let off = (o.a - m).dot(n);
        if off.abs() < 1e-6 || off.abs() > crate::temp_dims::REACH {
            continue;
        }
        // the foot sits at the midpoint of the shared stretch so the line hits both walls
        let s = (lo.max(0.0) + hi.min(l)) * 0.5;
        let a = w.a + u * s;
        let side = usize::from(off < 0.0);
        if best[side].is_none_or(|b| off.abs() < b.0) {
            best[side] = Some((off.abs(), o.id, a, off));
        }
    }
    best.into_iter()
        .flatten()
        .map(|(v, other, a, off)| SelDim { wall: id, other, a, b: a + n * off, value: v })
        .collect()
}

fn lock_op(d: SelDim) -> OpFn {
    OpFn(Arc::new(move |m: &mut Model| {
        let spec = ConstraintSpec {
            kind: ConstraintKind::LockDist,
            walls: vec![d.other, d.wall],
            value: None,
        };
        m.add_constraint(&spec).map(|_| ())
    }))
}

fn move_op(d: SelDim, v: f64) -> OpFn {
    let s = d.shift_for(v);
    OpFn(Arc::new(move |m: &mut Model| m.move_elements(&[Target::Wall(d.wall)], (s.x, s.y))))
}

/// Locks dimension `k`: one undo step; the badge then shows on the plan.
pub fn lock(ed: &mut Editor, k: usize) -> bool {
    let Some(d) = for_selection(ed).get(k).copied() else {
        return false;
    };
    ed.status = Some(match ed.run_op(lock_op(d)) {
        Ok(()) => crate::tr("create_input.seldim.locked", &[("v", &len_text(ed, d.value))]),
        Err(e) => crate::constraints_ui::report(&e),
    });
    true
}

/// Sets dimension `k` to `v` by moving the selected wall: one undo step.
pub fn set_value(ed: &mut Editor, k: usize, v: f64) -> bool {
    let Some(d) = for_selection(ed).get(k).copied() else {
        return false;
    };
    if !(v.is_finite() && v > 0.0) {
        ed.status = Some(crate::tr("create_input.seldim.positive", &[]));
        return true;
    }
    if let Err(e) = ed.run_op(move_op(d, v)) {
        ed.status = Some(crate::constraints_ui::report(&e));
    }
    true
}

fn len_text(ed: &Editor, v: f64) -> String {
    crate::units::format_length(v, ed.tool.units)
}

/// Plan input routed before the grips: lock and value clicks, typing keys. True when used.
pub fn handle(ed: &mut Editor, e: &Event) -> bool {
    match e {
        Event::Click(p) => {
            let ds = for_selection(ed);
            let r = ed.tool.radius;
            let near = |q: V2| (q - *p).len() <= r;
            if let Some(k) = ds.iter().position(|d| near(d.lock_pos())) {
                set_typing(None);
                return lock(ed, k);
            }
            if let Some(k) = ds.iter().position(|d| near(d.mid())) {
                set_typing(Some((k, String::new())));
                return true;
            }
            set_typing(None);
            false
        }
        Event::Char(c) if typing().is_some() => {
            TYPING.with(|t| {
                if let Some((_, s)) = t.borrow_mut().as_mut() {
                    if s.len() < 32 && (c.is_ascii_alphanumeric() || ".,-+'\"/ ".contains(*c)) {
                        s.push(*c);
                    }
                }
            });
            true
        }
        Event::Key(Key::Backspace) if typing().is_some() => {
            TYPING.with(|t| {
                if let Some((_, s)) = t.borrow_mut().as_mut() {
                    s.pop();
                }
            });
            true
        }
        Event::Key(Key::Escape) if typing().is_some() => {
            set_typing(None);
            true
        }
        Event::Key(Key::Enter) if typing().is_some() => {
            let (k, s) = typing().expect("typing");
            match crate::units::parse_signed_len(&s, ed.tool.units) {
                Ok(v) => {
                    set_typing(None);
                    set_value(ed, k, v)
                }
                Err(_) => {
                    ed.status = Some(crate::tr("modify_ops.grip.not_number", &[("text", &s)]));
                    true
                }
            }
        }
        _ => false,
    }
}

/// Dashed lines, value plates and lock glyphs of the selection dimensions.
pub fn paint(
    painter: &egui::Painter,
    ed: &Editor,
    to_s: &dyn Fn(V2) -> egui::Pos2,
    t: &crate::ui_kit::Tokens,
) {
    use crate::overlay::{self, Plate};
    use ddc_ui::theme::icon_size;
    use egui::{pos2, Rect, Shape, Stroke};
    let ov = t.ov();
    let typed = typing();
    for (k, d) in for_selection(ed).iter().enumerate() {
        let (a, b) = (to_s(d.a), to_s(d.b));
        painter.extend(Shape::dashed_line(&[a, b], Stroke::new(overlay::THIN_PX, t.text_dim), 4.0, 3.0));
        let (txt, kind) = match typed.as_ref().filter(|x| x.0 == k) {
            Some((_, s)) => (format!("{s}▏"), Plate::Editing),
            None => (len_text(ed, d.value), Plate::Temp),
        };
        overlay::dim_label(painter, to_s(d.mid()), txt, kind, ov);
        let r = Rect::from_center_size(to_s(d.lock_pos()), egui::vec2(icon_size::SM, icon_size::SM));
        let tex = ddc_ui::widgets::with_icons(painter.ctx(), |icons| {
            icons.get(painter.ctx(), "lock", icon_size::SM as u32)
        });
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.rect_filled(r.expand(2.0), ddc_ui::theme::radius::SM, t.paper);
        painter.image(tex.id(), r, uv, t.text_dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    fn ed3() -> (Editor, [i64; 3]) {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(
            &mut ed,
            &[((0.0, 0.0), (6.0, 0.0)), ((0.0, 3.0), (6.0, 3.0)), ((1.0, -2.0), (5.0, -2.0))],
        );
        let ids = [ed.walls[0].id, ed.walls[1].id, ed.walls[2].id];
        (ed, ids)
    }

    #[test]
    fn nearest_parallel_wall_on_each_side() {
        let (mut ed, [w, up, down]) = ed3();
        ed.selection = vec![Sel::Wall(w)];
        let ds = for_selection(&ed);
        assert_eq!(ds.len(), 2);
        let by = |o: i64| ds.iter().find(|d| d.other == o).copied().unwrap();
        assert!((by(up).value - 3.0).abs() < 1e-9);
        assert!((by(down).value - 2.0).abs() < 1e-9);
        assert!((by(down).b.y + 2.0).abs() < 1e-9 && (by(down).a.y).abs() < 1e-9);
        ed.selection = vec![Sel::Wall(w), Sel::Wall(up)];
        assert!(for_selection(&ed).is_empty(), "one wall only");
    }

    #[test]
    fn typed_value_moves_the_wall_in_one_undo() {
        let (mut ed, [w, up, _]) = ed3();
        ed.selection = vec![Sel::Wall(w)];
        let k = for_selection(&ed).iter().position(|d| d.other == up).unwrap();
        let steps = ed.undo_len();
        let at = for_selection(&ed)[k].mid();
        assert!(handle(&mut ed, &Event::Click(at)));
        for c in "1000".chars() {
            assert!(handle(&mut ed, &Event::Char(c)));
        }
        assert!(handle(&mut ed, &Event::Key(Key::Enter)));
        let wall = ed.walls.iter().find(|x| x.id == w).unwrap();
        assert!((wall.a.y - 2.0).abs() < 1e-6, "{:?}", wall.a);
        assert_eq!(ed.undo_len(), steps + 1);
        ed.undo();
        assert!(ed.walls.iter().find(|x| x.id == w).unwrap().a.y.abs() < 1e-6);
        assert!(typing().is_none());
    }

    #[test]
    fn lock_adds_one_constraint_and_undo_removes_it() {
        let (mut ed, [w, _, down]) = ed3();
        ed.selection = vec![Sel::Wall(w)];
        let d = for_selection(&ed).into_iter().find(|d| d.other == down).unwrap();
        let n0 = crate::constraints_ui::markers(&ed.model).len();
        assert!(handle(&mut ed, &Event::Click(d.lock_pos())));
        assert_eq!(crate::constraints_ui::markers(&ed.model).len(), n0 + 1);
        ed.undo();
        assert_eq!(crate::constraints_ui::markers(&ed.model).len(), n0);
    }

    #[test]
    fn shift_for_keeps_direction() {
        let d = SelDim { wall: 1, other: 2, a: v2(0.0, 0.0), b: v2(0.0, 3.0), value: 3.0 };
        let s = d.shift_for(1.0);
        assert!((s.y - 2.0).abs() < 1e-12 && s.x.abs() < 1e-12);
    }
}
