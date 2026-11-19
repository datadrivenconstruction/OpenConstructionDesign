// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sections and elevations in the editor: model view definitions (`view_def`), the "Section" tool
//! (two clicks for the line, a third for depth and view side), plan mark grips, elevations by
//! compass sides. Each change is one model operation (undo/redo like other commands).

use super::{Cmd, Editor};
use crate::status::Severity;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::{v2, V2};
use ddc_model::{PlanView, SectionSpec, ViewKind};

/// Smallest section depth, m.
pub const MIN_DEPTH: f64 = 0.3;
/// Depth while the line is being set (before the third click), m.
pub const DEFAULT_DEPTH: f64 = 4.0;
/// Offset of compass-side elevations from the model bounds, m.
pub const COMPASS_MARGIN: f64 = 3.0;

/// The "Section" tool: line start, end and cursor (world coordinates).
#[derive(Clone, Debug, Default)]
pub struct SectionTool {
    pub a: Option<V2>,
    pub b: Option<V2>,
    pub cursor: Option<V2>,
}

/// A view mark grip on the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    A,
    B,
    Depth,
    Flip,
}

/// Dragging a mark grip: the spec before and the current one.
#[derive(Clone, Debug)]
pub struct ViewDrag {
    pub id: i64,
    pub grip: Grip,
    pub before: SectionSpec,
    pub now: SectionSpec,
}

/// View name for the UI: compass-side elevations are in the UI language.
pub fn display_name(s: &SectionSpec) -> String {
    match s.name.as_str() {
        "North" => crate::tr("edit_panels.sec.north", &[]),
        "South" => crate::tr("edit_panels.sec.south", &[]),
        "East" => crate::tr("edit_panels.sec.east", &[]),
        "West" => crate::tr("edit_panels.sec.west", &[]),
        n => n.into(),
    }
}

pub fn ends(s: &SectionSpec) -> (V2, V2) {
    (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1))
}

/// View direction (unit, to the left of a->b).
pub fn look(s: &SectionSpec) -> V2 {
    let (a, b) = ends(s);
    let d = b - a;
    let l = d.len().max(1e-9);
    v2(-d.y / l, d.x / l)
}

/// Mark grips; `px` is meters per pixel (the flip arrow is 18 px behind the line).
pub fn grips(s: &SectionSpec, px: f64) -> [(Grip, V2); 4] {
    let (a, b) = ends(s);
    let n = look(s);
    let m = (a + b) * 0.5;
    let depth = if s.kind == ViewKind::Section {
        s.depth
    } else {
        // an elevation looks far: no depth grip is needed, keep it at the line
        24.0 * px
    };
    [
        (Grip::A, a),
        (Grip::B, b),
        (Grip::Depth, m + n * depth),
        (Grip::Flip, m - n * (18.0 * px)),
    ]
}

/// A view from line a->b and depth point `c`: the view side is toward `c`, the depth is the distance to the line.
pub fn spec_from(name: &str, a: V2, b: V2, c: Option<V2>) -> Option<SectionSpec> {
    if (b - a).len() < 1e-3 {
        return None;
    }
    let mut s = SectionSpec {
        kind: ViewKind::Section,
        name: name.into(),
        a: (a.x, a.y),
        b: (b.x, b.y),
        depth: DEFAULT_DEPTH,
        far_clip: true,
    };
    if let Some(c) = c {
        let n = look(&s);
        let d = (c - a).x * n.x + (c - a).y * n.y;
        if d < 0.0 {
            s = s.flipped();
        }
        s.depth = d.abs().max(MIN_DEPTH);
    }
    Some(s)
}

impl Editor {
    pub fn start_section(&mut self) {
        self.set_mode(Mode::Select);
        self.ix.begin(Box::new(crate::interact::sessions::section::SectionSession::default()));
        self.say(Severity::Info, crate::tr("edit_panels.sec.start", &[]));
    }

    /// The section tool's points while its session runs.
    pub fn section_tool(&self) -> Option<&SectionTool> {
        crate::interact::sessions::section::tool(self)
    }

    pub fn view_spec(&self, id: i64) -> Option<&SectionSpec> {
        self.views.iter().find(|(i, _)| *i == id).map(|(_, s)| s)
    }

    /// The next free default section name (Russian "Section N").
    pub fn next_section_name(&self) -> String {
        (1..)
            .map(|k| format!("\u{420}\u{430}\u{437}\u{440}\u{435}\u{437} {k}"))
            .find(|n| !self.views.iter().any(|(_, s)| &s.name == n))
            .expect("free name")
    }

    /// Draft of the "Section" tool for the plan.
    pub fn section_preview(&self) -> Option<SectionSpec> {
        self.section_spec(self.section_tool()?)
    }

    /// The view the section tool `t` would add now.
    pub fn section_spec(&self, t: &SectionTool) -> Option<SectionSpec> {
        let a = t.a?;
        let name = self.next_section_name();
        match t.b {
            None => spec_from(&name, a, t.cursor?, None),
            Some(b) => spec_from(&name, a, b, t.cursor),
        }
    }

    fn snap_pt(&self, p: V2) -> V2 {
        let g = self.tool.grid.max(1e-3);
        v2((p.x / g).round() * g, (p.y / g).round() * g)
    }

    /// Input of the "Section" tool (its session keeps `t`); false when Esc ends the tool.
    pub(crate) fn handle_section(&mut self, t: &mut SectionTool, e: Event) -> bool {
        match e {
            Event::Move(p) => t.cursor = Some(self.snap_pt(p)),
            Event::Click(p) => {
                let p = self.snap_pt(p);
                match (t.a, t.b) {
                    (None, _) => {
                        t.a = Some(p);
                        self.say(Severity::Info, crate::tr("edit_panels.sec.end", &[]));
                    }
                    (Some(a), None) if (p - a).len() > 1e-3 => {
                        t.b = Some(p);
                        self.say(Severity::Info, crate::tr("edit_panels.sec.depth", &[]));
                    }
                    (Some(_), None) => {}
                    (Some(_), Some(_)) => {
                        if let Some(s) = self.section_spec(t) {
                            self.add_view(&s);
                        }
                        *t = SectionTool::default();
                    }
                }
            }
            Event::Key(Key::Escape) => match (t.a, t.b) {
                (Some(_), Some(_)) => t.b = None,
                (Some(_), None) => *t = SectionTool::default(),
                _ => return false,
            },
            Event::Key(Key::Backspace) => {
                if t.b.take().is_none() {
                    t.a = None;
                }
            }
            _ => {}
        }
        true
    }

    /// Mark grips in select mode: dragging ends and depth, a click on the arrow flips.
    /// Returns true if the event was consumed.
    pub(crate) fn handle_view_grips(&mut self, e: Event) -> bool {
        let px = self.tool.radius / 12.0;
        let r = self.tool.radius;
        let hit = |ed: &Self, p: V2| -> Option<(i64, Grip)> {
            ed.views
                .iter()
                .filter(|(_, s)| ed.mark_visible(s))
                .flat_map(|(id, s)| grips(s, px).map(|(g, q)| (*id, g, (q - p).len())))
                .filter(|x| x.2 <= r)
                .min_by(|x, y| x.2.total_cmp(&y.2))
                .map(|x| (x.0, x.1))
        };
        match e {
            Event::Press(p) => {
                let Some((id, g)) = hit(self, p).filter(|h| h.1 != Grip::Flip) else {
                    return false;
                };
                let s = self.view_spec(id).cloned().expect("view");
                self.vdrag = Some(ViewDrag {
                    id,
                    grip: g,
                    before: s.clone(),
                    now: s,
                });
                true
            }
            Event::Move(p) => {
                let p = self.snap_pt(p);
                let Some(d) = &mut self.vdrag else {
                    return false;
                };
                let s = &d.before;
                let (a, b) = ends(s);
                let next = match d.grip {
                    Grip::A => (p - b).len() > 1e-3,
                    Grip::B => (p - a).len() > 1e-3,
                    _ => true,
                };
                if next {
                    let mut n = s.clone();
                    match d.grip {
                        Grip::A => n.a = (p.x, p.y),
                        Grip::B => n.b = (p.x, p.y),
                        Grip::Depth => {
                            let nn = look(s);
                            n.depth = ((p - a).x * nn.x + (p - a).y * nn.y).max(MIN_DEPTH);
                        }
                        Grip::Flip => {}
                    }
                    d.now = n;
                }
                true
            }
            Event::Release(_) => {
                let Some(d) = self.vdrag.take() else {
                    return false;
                };
                if d.now != d.before {
                    self.edit_view(d.id, &d.now);
                }
                true
            }
            Event::Click(p) => match hit(self, p) {
                Some((id, Grip::Flip)) => {
                    let s = self.view_spec(id).cloned().expect("view").flipped();
                    self.edit_view(id, &s);
                    self.say(Severity::Info, crate::tr("edit_panels.sec.flipped", &[]));
                    true
                }
                Some(_) => true,
                None => false,
            },
            _ => false,
        }
    }

    /// Whether the mark is shown on the plan: sections always, elevations too (they follow the bounds).
    pub fn mark_visible(&self, _s: &SectionSpec) -> bool {
        true
    }

    /// View spec accounting for a drag in progress (for the plan mark).
    pub fn live_view(&self, id: i64) -> Option<&SectionSpec> {
        match &self.vdrag {
            Some(d) if d.id == id => Some(&d.now),
            _ => self.view_spec(id),
        }
    }

    pub fn add_view(&mut self, s: &SectionSpec) -> Option<i64> {
        match self
            .model
            .add_view_def(s)
            .and_then(|id| self.reload().map(|_| id))
        {
            Ok(id) => {
                self.say(Severity::Info, crate::tr("edit_panels.sec.added", &[("name", display_name(s).as_str())]));
                self.push(Cmd::AddView(s.clone(), id));
                Some(id)
            }
            Err(e) => {
                self.say(Severity::Error, crate::tr("edit_panels.sec.add_fail", &[("e", e.to_string().as_str())]));
                None
            }
        }
    }

    pub fn edit_view(&mut self, id: i64, s: &SectionSpec) {
        match self
            .model
            .update_view_def(id, s)
            .and_then(|_| self.reload())
        {
            Ok(()) => self.push(Cmd::EditView(id, s.clone())),
            Err(e) => self.say(Severity::Error, crate::tr("edit_panels.sec.edit_fail", &[("e", e.to_string().as_str())])),
        }
    }

    pub fn delete_view(&mut self, id: i64) {
        match self.model.delete_view_def(id).and_then(|_| self.reload()) {
            Ok(()) => {
                self.say(Severity::Info, crate::tr("edit_panels.sec.deleted", &[]));
                self.push(Cmd::DeleteView(id));
            }
            Err(e) => self.say(Severity::Error, crate::tr("edit_panels.sec.del_fail", &[("e", e.to_string().as_str())])),
        }
    }

    /// "Compass-side elevations": the missing ones of four; returns the number of new ones.
    pub fn add_compass(&mut self) -> usize {
        match self
            .model
            .add_compass_elevations(COMPASS_MARGIN)
            .and_then(|ids| self.reload().map(|_| ids))
        {
            Ok(ids) if ids.is_empty() => {
                self.say(Severity::Info, if self.walls.is_empty() {
                    crate::tr("edit_panels.sec.compass_empty", &[])
                } else {
                    crate::tr("edit_panels.sec.compass_exist", &[])
                });
                0
            }
            Ok(ids) => {
                let n = ids.len();
                self.say(Severity::Info, crate::tr("edit_panels.sec.compass_added", &[("n", n.to_string().as_str())]));
                self.push(Cmd::Compass(ids));
                n
            }
            Err(e) => {
                self.say(Severity::Error, crate::tr("edit_panels.sec.compass_fail", &[("e", e.to_string().as_str())]));
                0
            }
        }
    }

    /// Drawing of a view (sections, projections, levels, axes, dimensions) in (s, z) coordinates.
    pub fn draw_view(&self, id: i64) -> Option<PlanView> {
        let s = self.view_spec(id)?;
        self.model.section_view(s, &self.grid_axes()).ok()
    }

    /// Axes by walls: vertical axes are digits (by x), horizontal ones are letters (by y).
    pub fn grid_axes(&self) -> Vec<ddc_model::GridAxis> {
        let (mut xs, mut ys) = (vec![], vec![]);
        let mut bb = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for w in self.walls.iter().filter(|w| w.bulge == 0.0) {
            for p in [w.a, w.b] {
                bb = [
                    bb[0].min(p.x),
                    bb[1].min(p.y),
                    bb[2].max(p.x),
                    bb[3].max(p.y),
                ];
            }
            let d = w.b - w.a;
            if d.x.abs() < 1e-6 {
                xs.push(w.a.x);
            } else if d.y.abs() < 1e-6 {
                ys.push(w.a.y);
            }
        }
        let uniq = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
            v.truncate(26);
        };
        uniq(&mut xs);
        uniq(&mut ys);
        let m = 1.5;
        let mut out = vec![];
        for (k, x) in xs.iter().enumerate() {
            out.push(ddc_model::GridAxis {
                label: (k + 1).to_string(),
                a: (*x, bb[1] - m),
                b: (*x, bb[3] + m),
            });
        }
        for (k, y) in ys.iter().enumerate() {
            out.push(ddc_model::GridAxis {
                label: char::from(b'A' + k as u8).to_string(),
                a: (bb[0] - m, *y),
                b: (bb[2] + m, *y),
            });
        }
        out
    }

    /// Replay of view commands; None means the command is not from here.
    pub(super) fn sec_redo(&mut self, c: &Cmd) -> Option<Result<Cmd, ddc_model::Error>> {
        Some(match c {
            Cmd::AddView(s, old) => self
                .model
                .add_view_def(s)
                .and_then(|id| self.reload().map(|_| id))
                .map(|new| {
                    for cmd in &mut self.redo {
                        if let Cmd::EditView(id, _) | Cmd::DeleteView(id) = cmd {
                            if id == old {
                                *id = new;
                            }
                        }
                    }
                    Cmd::AddView(s.clone(), new)
                }),
            Cmd::EditView(id, s) => self
                .model
                .update_view_def(*id, s)
                .and_then(|_| self.reload())
                .map(|_| Cmd::EditView(*id, s.clone())),
            Cmd::DeleteView(id) => self
                .model
                .delete_view_def(*id)
                .and_then(|_| self.reload())
                .map(|_| Cmd::DeleteView(*id)),
            Cmd::Compass(old) => self
                .model
                .add_compass_elevations(COMPASS_MARGIN)
                .and_then(|ids| self.reload().map(|_| ids))
                .map(|new| {
                    for cmd in &mut self.redo {
                        if let Cmd::EditView(id, _) | Cmd::DeleteView(id) = cmd {
                            if let Some(k) = old.iter().position(|o| o == id) {
                                *id = new.get(k).copied().unwrap_or(*id);
                            }
                        }
                    }
                    Cmd::Compass(new)
                }),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(e: &mut Editor) {
        e.set_mode(Mode::Rect);
        e.handle(Event::Move(v2(0.0, 0.0)));
        e.handle(Event::Press(v2(0.0, 0.0)));
        e.handle(Event::Move(v2(6.0, 4.0)));
        e.handle(Event::Release(v2(6.0, 4.0)));
        e.set_mode(Mode::Select);
    }

    #[test]
    fn section_tool_three_clicks_and_undo_redo() {
        let mut e = Editor::new_in_memory().unwrap();
        room(&mut e);
        e.tool.grid = 0.1;
        e.start_section();
        e.handle(Event::Click(v2(3.0, -1.0)));
        e.handle(Event::Click(v2(3.0, 5.0)));
        // the cursor is on the right (x > 3): looking east, the view is flipped, depth 2
        e.handle(Event::Move(v2(5.0, 2.0)));
        let p = e.section_preview().unwrap();
        assert_eq!(p.a, (3.0, 5.0));
        assert!((p.depth - 2.0).abs() < 1e-9);
        e.handle(Event::Click(v2(5.0, 2.0)));
        assert_eq!(e.views.len(), 1);
        assert_eq!(e.views[0].1.name, crate::app2_fx::fx("sec_name1"));
        let id = e.views[0].0;
        let pv = e.draw_view(id).unwrap();
        assert!(pv.count(|p| matches!(p, ddc_model::ViewPrim::SectionCut { .. })) >= 2);
        assert!(pv.count(|p| matches!(p, ddc_model::ViewPrim::Level { .. })) >= 1);
        e.undo();
        assert!(e.views.is_empty());
        e.redo();
        assert_eq!(e.views.len(), 1);
    }

    #[test]
    fn grips_drag_depth_and_flip_click() {
        let mut e = Editor::new_in_memory().unwrap();
        room(&mut e);
        e.tool.grid = 0.1;
        e.tool.radius = 0.15;
        let s = spec_from("Section 1", v2(3.0, -1.0), v2(3.0, 5.0), Some(v2(1.0, 2.0))).unwrap();
        let id = e.add_view(&s).unwrap();
        let g = grips(&s, e.tool.radius / 12.0);
        e.handle(Event::Press(g[2].1));
        e.handle(Event::Move(v2(0.0, 2.0)));
        e.handle(Event::Release(v2(0.0, 2.0)));
        assert!((e.view_spec(id).unwrap().depth - 3.0).abs() < 1e-9);
        let g = grips(e.view_spec(id).unwrap(), e.tool.radius / 12.0);
        e.handle(Event::Click(g[3].1));
        assert_eq!(e.view_spec(id).unwrap().a, (3.0, 5.0));
        e.undo();
        assert_eq!(e.view_spec(id).unwrap().a, (3.0, -1.0));
    }

    #[test]
    fn compass_elevations_once() {
        let mut e = Editor::new_in_memory().unwrap();
        assert_eq!(e.add_compass(), 0);
        room(&mut e);
        assert_eq!(e.add_compass(), 4);
        assert_eq!(e.add_compass(), 0);
        let names: Vec<String> = e.views.iter().map(|(_, s)| display_name(s)).collect();
        assert!(names.contains(&crate::tr("edit_panels.sec.south", &[])));
        e.undo();
        assert!(e.views.is_empty());
    }
}
