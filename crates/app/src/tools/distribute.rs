// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Distribute along wall" (W2): the selected door/window is the template; N slots at equal pitch
//! between end offsets, or at a fixed pitch. Live preview: free slots use the preview colour,
//! conflicts (past the end, on another opening, overlap) are red until committed. Digits — N
//! (Enter applies the input, empty Enter commits), Space toggles equal/fixed pitch. Linked
//! distribution (EQ) is the default: lengthening the wall or editing N redistributes; a member
//! of a linked distribution opens its editing, "Break" removes the link. One operation.

use crate::editor::{Editor, OpFn};
use ddc_geom::{v2, V2};
use ddc_model::{DistMember, DistMode, DistSlot, SlotConflict, Target};
use std::sync::Arc;

pub enum DistOut {
    Nothing,
    Done(String),
    Exit,
    Error(String),
}

#[derive(Clone, Debug)]
pub struct DistTool {
    /// Template (opening).
    pub src: Option<i64>,
    /// Editing an existing linked distribution.
    pub rule: Option<i64>,
    pub n: usize,
    pub equal: bool,
    pub margin: f64,
    pub step: f64,
    pub link: bool,
    pub input: String,
    pub cursor: V2,
}

/// Opening under the point: centre on the axis within half-width (or pick radius `r`).
pub fn opening_at(ed: &Editor, p: V2, r: f64) -> Option<i64> {
    ed.openings
        .iter()
        .filter_map(|o| {
            let w = ed.walls.iter().find(|w| w.id == o.wall)?;
            let d = w.b - w.a;
            let l = d.len();
            if l < 1e-9 {
                return None;
            }
            let u = d * (1.0 / l);
            let q = w.a + u * (o.along + o.width / 2.0);
            let along = (p - q).dot(u).abs();
            let across = (p - q).dot(v2(-u.y, u.x)).abs();
            let half_t = ed
                .types
                .iter()
                .find(|t| t.id == w.wall_type)
                .map_or(0.1, |t| t.thickness() / 2.0);
            (along <= o.width / 2.0 + r * 0.5 && across <= half_t + r).then_some((across, o.id))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
}

impl DistTool {
    pub fn new(ed: &Editor) -> Self {
        let mut t = Self {
            src: None,
            rule: None,
            n: 4,
            equal: true,
            margin: 0.6,
            step: 1.5,
            link: true,
            input: String::new(),
            cursor: v2(0.0, 0.0),
        };
        if let [crate::editor::Sel::Opening(id)] = ed.selection[..] {
            t.pick(ed, id);
        }
        t
    }

    /// Template; for a linked-distribution member, its parameters.
    pub fn pick(&mut self, ed: &Editor, id: i64) {
        self.src = Some(id);
        self.rule = None;
        if let Ok(Some(r)) = ed.model.distribution_of(Target::Opening(id)) {
            self.rule = Some(r.id);
            self.n = r.members.len();
            match r.mode {
                DistMode::Equal { margin } => {
                    self.equal = true;
                    self.margin = margin;
                }
                DistMode::Step { margin, step } => {
                    self.equal = false;
                    self.margin = margin;
                    self.step = step;
                }
            }
        }
    }

    pub fn mode(&self) -> DistMode {
        if self.equal {
            DistMode::Equal {
                margin: self.margin,
            }
        } else {
            DistMode::Step {
                margin: self.margin,
                step: self.step,
            }
        }
    }

    pub fn step_key(&self) -> &'static str {
        if self.src.is_none() {
            "pick"
        } else {
            "set"
        }
    }

    /// Wall, width and slots for the current parameters.
    pub fn preview(&self, ed: &Editor) -> Option<(i64, f64, Vec<DistSlot>)> {
        ed.model
            .distribution_preview(Target::Opening(self.src?), self.n, self.mode())
            .ok()
    }

    pub fn click(&mut self, ed: &mut Editor, p: V2, r: f64) -> DistOut {
        match opening_at(ed, p, r) {
            Some(id) => {
                self.pick(ed, id);
                ed.selection = vec![crate::editor::Sel::Opening(id)];
                DistOut::Nothing
            }
            None if self.src.is_none() => DistOut::Error(crate::tr("sketch_edit.dist_pick", &[])),
            None => DistOut::Nothing,
        }
    }

    pub fn char(&mut self, c: char) {
        if c.is_ascii_digit() && self.input.len() < 4 {
            self.input.push(c);
        }
    }

    /// Enter: typed number is N; empty input commits.
    pub fn enter(&mut self, ed: &mut Editor) -> DistOut {
        if !self.input.is_empty() {
            let t = std::mem::take(&mut self.input);
            return match t.parse::<usize>() {
                Ok(n) if (1..=200).contains(&n) => {
                    self.n = n;
                    DistOut::Nothing
                }
                _ => DistOut::Error(crate::tr("sketch_edit.dist_count", &[("t", &t)])),
            };
        }
        self.commit(ed)
    }

    pub fn commit(&mut self, ed: &mut Editor) -> DistOut {
        let Some(src) = self.src else {
            return DistOut::Error(crate::tr("sketch_edit.dist_pick", &[]));
        };
        let (n, mode, link) = (self.n, self.mode(), self.link);
        let skipped = self
            .preview(ed)
            .map_or(0, |p| p.2.iter().filter(|s| s.conflict.is_some()).count());
        let f = match self.rule {
            Some(rule) => OpFn(Arc::new(move |m| m.set_distribution(rule, n, mode))),
            None => OpFn(Arc::new(move |m| {
                m.distribute(Target::Opening(src), n, mode, link)
                    .map(|_| ())
            })),
        };
        match ed.run_op(f) {
            Ok(()) => {
                // editing the rule leaves conflicting slots empty — count live members
                let placed = match self.rule {
                    Some(rule) => ed.model.distribution(rule).ok().flatten().map_or(n, |r| {
                        r.members
                            .iter()
                            .filter(|m| **m != DistMember::Empty)
                            .count()
                    }),
                    None => n - skipped,
                };
                let p = placed.to_string();
                DistOut::Done(if placed < n {
                    crate::tr(
                        "sketch_edit.dist_partial",
                        &[
                            ("placed", &p),
                            ("n", &n.to_string()),
                            ("skipped", &(n - placed).to_string()),
                        ],
                    )
                } else {
                    crate::tr("sketch_edit.dist_done", &[("placed", &p)])
                })
            }
            Err(e) => DistOut::Error(crate::tr("sketch_edit.dist_fail", &[("e", &e.to_string())])),
        }
    }

    /// Break the distribution link.
    pub fn release(&mut self, ed: &mut Editor) -> DistOut {
        let Some(rule) = self.rule else {
            return DistOut::Nothing;
        };
        match ed.run_op(OpFn(Arc::new(move |m| m.release_distribution(rule)))) {
            Ok(()) => {
                self.rule = None;
                DistOut::Done(crate::tr("sketch_edit.dist_released", &[]))
            }
            Err(e) => DistOut::Error(format!("{e}")),
        }
    }

    pub fn space(&mut self) {
        self.equal = !self.equal;
    }

    /// Esc: input, then template, then exit.
    pub fn escape(&mut self) -> DistOut {
        if !self.input.is_empty() {
            self.input.clear();
            DistOut::Nothing
        } else if self.src.is_some() {
            self.src = None;
            self.rule = None;
            DistOut::Nothing
        } else {
            DistOut::Exit
        }
    }
}

/// Slot conflict label.
pub fn conflict_text(c: SlotConflict) -> String {
    conflict_text_in(c, crate::ui_is_ru())
}

pub fn conflict_text_in(c: SlotConflict, ru: bool) -> String {
    match c {
        SlotConflict::OffWall => crate::tr_lang(ru, "sketch_edit.conflict_off", &[]),
        SlotConflict::Overlap => crate::tr_lang(ru, "sketch_edit.conflict_overlap", &[]),
        SlotConflict::Opening(id) => {
            crate::tr_lang(ru, "sketch_edit.conflict_opening", &[("id", &id.to_string())])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    fn door_on_wall(ed: &mut Editor) -> (i64, i64) {
        walls(ed, &[((0.0, 0.0), (10.0, 0.0))]);
        let w = ed.walls[0].id;
        let d = ed
            .model
            .add_opening(
                w,
                &ddc_model::OpeningSpec {
                    along: 0.3,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        ed.reload().unwrap();
        (w, d)
    }

    #[test]
    fn typed_count_commit_is_one_undo_and_edit_relays() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (_, d) = door_on_wall(&mut ed);
        let mut t = DistTool::new(&ed);
        assert_eq!(t.step_key(), "pick");
        assert!(matches!(
            t.click(&mut ed, v2(0.75, 0.05), 0.1),
            DistOut::Nothing
        ));
        assert_eq!(t.src, Some(d));
        "5".chars().for_each(|c| t.char(c));
        assert!(matches!(t.enter(&mut ed), DistOut::Nothing));
        assert_eq!(t.n, 5);
        let p = t.preview(&ed).unwrap();
        assert_eq!(p.2.len(), 5);
        assert!(matches!(t.enter(&mut ed), DistOut::Done(_)));
        assert_eq!(ed.openings.len(), 5);
        ed.undo();
        assert_eq!(ed.openings.len(), 1);
        ed.redo();
        assert_eq!(ed.openings.len(), 5);
        // a distribution member opens editing: N = 3 redistributes in one operation
        let mut t = DistTool::new(&ed);
        t.pick(&ed, d);
        assert!(t.rule.is_some() && t.n == 5);
        t.n = 3;
        assert!(matches!(t.commit(&mut ed), DistOut::Done(_)));
        assert_eq!(ed.openings.len(), 3);
        ed.undo();
        assert_eq!(ed.openings.len(), 5);
    }

    #[test]
    fn space_toggles_mode_and_escape_steps_back() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (_, d) = door_on_wall(&mut ed);
        ed.selection = vec![crate::editor::Sel::Opening(d)];
        let mut t = DistTool::new(&ed);
        assert_eq!(t.src, Some(d));
        t.space();
        assert!(matches!(t.mode(), DistMode::Step { .. }));
        t.char('7');
        assert!(matches!(t.escape(), DistOut::Nothing));
        assert!(t.input.is_empty());
        assert!(matches!(t.escape(), DistOut::Nothing));
        assert!(t.src.is_none());
        assert!(matches!(t.escape(), DistOut::Exit));
    }
}
