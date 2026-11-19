// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Undo stack for the "History" panel: a step label key by command kind, jumping to a step.
use super::{Cmd, Editor};
use crate::drawing::history::Step;

/// Primitives kept for undo across all underlay steps: a large drawing is copied per edit.
const UNDERLAY_BUDGET: usize = 4_000_000;

/// i18n key of a step label (`hist.*`; model tools use `tool.<kind>`).
fn key(c: &Cmd) -> String {
    let k = match c {
        Cmd::Add(..) => "hist.walls_add",
        // the wall tool's envelope (`wall_envelope`) is the same step for the person
        Cmd::Ops(env, _) if env.actor == ddc_ops::Actor::User && env.title == "walls" => {
            "hist.walls_add"
        }
        Cmd::Items(..) => "hist.walls_delete",
        Cmd::AddOpening(..) => "hist.opening_add",
        Cmd::UpdateOpening(..) => "hist.opening_edit",
        Cmd::EditWall(..) => "hist.wall_edit",
        Cmd::Import(..) => "hist.import",
        Cmd::AddCurtains(..) => "hist.curtain_add",
        Cmd::EditCurtain(..) => "hist.curtain_edit",
        Cmd::Room(..) => "hist.room_edit",
        Cmd::AddView(..) => "hist.view_add",
        Cmd::EditView(..) => "hist.view_edit",
        Cmd::DeleteView(..) => "hist.view_delete",
        Cmd::LevelAdd(..) => "hist.level_add",
        Cmd::LevelEdit(..) => "hist.level_edit",
        Cmd::LevelDelete(..) => "hist.level_delete",
        Cmd::Xlsx(..) => "hist.bulk",
        Cmd::Op(..) => "hist.modify",
        Cmd::Ext(r) => return format!("tool.{}", r.label),
        Cmd::Underlay(..) => "hist.underlay",
        Cmd::SectionBox(..) => "hist.section_box",
        // other kinds (panels, slope, elevations, commands of other modules) share a generic label
        _ => "hist.edit",
    };
    k.to_string()
}

/// Translated name of a step, lower-cased for use inside a sentence.
pub(super) fn step_name(c: &Cmd) -> String {
    crate::tr(&key(c), &[]).to_lowercase()
}

/// Text of the toast after undo/redo: the action named, e.g. "Undone: walls added".
pub(super) fn done_text(redo: bool, name: &str) -> String {
    let k = if redo { "undo.redone" } else { "undo.undone" };
    crate::tr(k, &[("what", name)])
}

impl Editor {
    /// Label keys: done steps (oldest first) and undone ones (nearest first).
    pub fn history(&self) -> (Vec<String>, Vec<String>) {
        (
            self.undo.iter().map(key).collect(),
            self.redo.iter().rev().map(key).collect(),
        )
    }

    /// Journal operations behind each done step, oldest first (underlay steps own none).
    pub fn step_ops(&self) -> Vec<usize> {
        self.undo
            .iter()
            .map(|c| match c {
                Cmd::Underlay(_) | Cmd::SectionBox(..) => 0,
                Cmd::Ext(r) => r.n,
                Cmd::Ops(_, n) => *n as usize,
                _ => 1,
            })
            .collect()
    }

    /// Name of the step Ctrl+Z (`undo`) or Ctrl+Y would take, `None` when that stack is empty.
    pub fn next_label(&self, undo: bool) -> Option<String> {
        let c = if undo { self.undo.last() } else { self.redo.last() };
        c.map(step_name)
    }

    /// The next Ctrl+Z takes back journal rows: no drag or drawing in progress cancels first,
    /// and the top step is not an underlay or section-box step.
    pub fn top_owns_ops(&self) -> bool {
        if self.drag.is_some() || self.drawing() {
            return false;
        }
        match self.undo.last() {
            None | Some(Cmd::Underlay(_)) | Some(Cmd::SectionBox(..)) => false,
            Some(Cmd::Ext(r)) => r.n > 0,
            Some(Cmd::Ops(_, n)) => *n > 0,
            Some(_) => true,
        }
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Id of the step Ctrl+Z would take back; ids never repeat within a session.
    pub fn top_entry(&self) -> Option<u64> {
        self.undo_ids.last().copied()
    }

    /// Id of the step Ctrl+Y would bring back.
    pub fn redo_entry_top(&self) -> Option<u64> {
        self.redo_ids.last().copied()
    }

    /// The step written since the push count was `pushes`, when it is still the top one:
    /// what a result toast may offer to undo. `None` when nothing was written.
    pub fn entry_since(&self, pushes: u64) -> Option<u64> {
        self.top_entry().filter(|e| *e > pushes)
    }

    /// Takes back entry `id` and nothing else. Refuses, with a status line, when another step
    /// is on top, the entry is gone, or a drag or drawing would take the key first.
    pub fn undo_entry(&mut self, id: u64) -> bool {
        self.step_entry(id, true)
    }

    /// Brings back entry `id` only while it is the next redo step.
    pub fn redo_entry(&mut self, id: u64) -> bool {
        self.step_entry(id, false)
    }

    fn step_entry(&mut self, id: u64, undo: bool) -> bool {
        let top = if undo { self.top_entry() } else { self.redo_entry_top() };
        let key = if self.drag.is_some() || self.drawing() {
            Some("undo.entry_busy")
        } else if top != Some(id) {
            let elsewhere = self.undo_ids.contains(&id) || self.redo_ids.contains(&id);
            Some(if elsewhere { "undo.entry_later" } else { "undo.entry_gone" })
        } else {
            None
        };
        if let Some(k) = key {
            self.say(crate::status::Severity::Refusal, crate::tr(k, &[]));
            return false;
        }
        if undo {
            self.undo();
            self.redo_entry_top() == Some(id)
        } else {
            self.redo();
            self.top_entry() == Some(id)
        }
    }

    pub(super) fn pop_undo(&mut self) -> Option<(u64, Cmd)> {
        let c = self.undo.pop()?;
        Some((self.undo_ids.pop().unwrap_or_default(), c))
    }

    pub(super) fn pop_redo(&mut self) -> Option<(u64, Cmd)> {
        let c = self.redo.pop()?;
        Some((self.redo_ids.pop().unwrap_or_default(), c))
    }

    pub(super) fn put_undo(&mut self, id: u64, c: Cmd) {
        self.undo.push(c);
        self.undo_ids.push(id);
    }

    pub(super) fn put_redo(&mut self, id: u64, c: Cmd) {
        self.redo.push(c);
        self.redo_ids.push(id);
    }

    /// Record a finished underlay edit as one undo step.
    pub fn push_underlay(&mut self, step: Step) {
        self.push(Cmd::Underlay(step));
        self.trim_underlay(UNDERLAY_BUDGET);
    }

    /// Past the budget the oldest underlay steps stop being undoable. Each step stands alone,
    /// so the newer ones and every model step keep working; the newest is always kept. A freed
    /// drawing takes its older steps along: they would spend a Ctrl+Z on nothing.
    pub(crate) fn trim_underlay(&mut self, budget: usize) {
        fn weight(c: &Cmd) -> usize {
            match c {
                Cmd::Underlay(s) => s.weight(),
                _ => 0,
            }
        }
        let mut total: usize = self.undo.iter().map(weight).sum();
        while total > budget {
            let first = self.undo.iter().position(|c| weight(c) > 0);
            let last = self.undo.iter().rposition(|c| weight(c) > 0);
            match first {
                Some(k) if first != last => {
                    let gone = self.undo.remove(k);
                    self.undo_ids.remove(k);
                    total -= weight(&gone);
                    let Cmd::Underlay(gone) = gone else { continue };
                    if let Some(id) = gone.parked() {
                        let keep: Vec<bool> = self
                            .undo
                            .iter()
                            .map(|c| !matches!(c, Cmd::Underlay(s) if s.drawing() == id))
                            .collect();
                        let mut it = keep.iter();
                        self.undo_ids.retain(|_| *it.next().unwrap_or(&true));
                        let mut it = keep.iter();
                        self.undo.retain(|_| *it.next().unwrap_or(&true));
                        total = self.undo.iter().map(weight).sum();
                    }
                }
                _ => break,
            }
        }
    }

    /// Underlay steps that undo and redo reached since the last call, oldest first.
    pub fn take_underlay_due(&mut self) -> Vec<Step> {
        std::mem::take(&mut self.underlay_due)
    }

    /// Record a finished section box face drag as one undo step (no model operation).
    pub fn push_section_box(&mut self, before: ddc_render::Aabb, after: ddc_render::Aabb) {
        self.push(Cmd::SectionBox(before, after));
    }

    /// Section box that the last undo or redo reached, once.
    pub fn take_box_due(&mut self) -> Option<ddc_render::Aabb> {
        self.box_due.take()
    }

    /// Undo the last step and forget it (it does not reach "Redo"): an idle placeholder.
    pub fn drop_last_step(&mut self) -> bool {
        let (u, r) = (self.undo.len(), self.redo.len());
        if u == 0 {
            return false;
        }
        self.undo();
        if self.undo.len() + 1 == u && self.redo.len() == r + 1 {
            self.pop_redo();
            true
        } else {
            false
        }
    }

    /// Undo or redo steps until `depth` steps are done; returns the number of steps taken.
    /// Stops at the first failed step (the stack is not changed).
    pub fn jump_history(&mut self, depth: usize) -> usize {
        let mut n = 0;
        // each undo's status replaces the last: kept-table notes of every step are said once
        let mut kept = vec![];
        while self.undo.len() > depth {
            let before = self.undo.len();
            self.undo();
            kept.extend(self.undo_kept.iter().cloned());
            if self.undo.len() == before {
                break;
            }
            n += 1;
        }
        let last = std::mem::take(&mut self.undo_kept);
        self.status = super::stage::undo_status_all(self.status.take(), &last, &kept);
        self.undo_kept = kept;
        while self.undo.len() < depth && !self.redo.is_empty() {
            let before = self.undo.len();
            self.redo();
            if self.undo.len() == before {
                break;
            }
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    /// A 1.5×1.5 m wall rectangle: one gesture is one undo step.
    fn rect(ed: &mut Editor, x: f64) {
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Press(v2(x, 0.0)));
        ed.handle(Event::Release(v2(x + 1.5, 1.5)));
        ed.set_mode(Mode::Select);
    }

    #[test]
    fn undo_names_the_step_and_offers_redo() {
        let mut ed = Editor::new_in_memory().unwrap();
        rect(&mut ed, 0.0);
        ed.undo();
        let name = crate::tr("hist.walls_add", &[]).to_lowercase();
        assert!(ed.status.as_deref().unwrap().contains(&name), "{:?}", ed.status);
        assert_eq!(ed.status_cmd, Some("edit.redo"));
        ed.redo();
        assert!(ed.status.as_deref().unwrap().contains(&name));
        assert_eq!(ed.status_cmd, Some("edit.undo"));
    }

    #[test]
    fn next_label_names_last_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        rect(&mut ed, 0.0);
        let name = crate::tr("hist.walls_add", &[]).to_lowercase();
        assert_eq!(ed.next_label(true), Some(name.clone()));
        assert_eq!(ed.next_label(false), None);
        ed.undo();
        assert_eq!(ed.next_label(false), Some(name));
    }

    #[test]
    fn empty_says_nothing() {
        let ed = Editor::new_in_memory().unwrap();
        assert_eq!(ed.next_label(true), None);
        assert_eq!(ed.next_label(false), None);
    }

    #[test]
    fn history_labels_and_jump() {
        let mut ed = Editor::new_in_memory().unwrap();
        for x in [0.0, 3.0, 6.0] {
            rect(&mut ed, x);
        }
        let (done, undone) = ed.history();
        assert_eq!(done.len(), 3, "{done:?}");
        assert!(undone.is_empty());
        assert!(done.iter().all(|k| k == "hist.walls_add"));
        assert_eq!(ed.jump_history(1), 2);
        assert_eq!(ed.walls.len(), 4);
        assert_eq!((ed.undo_len(), ed.redo_len()), (1, 2));
        assert_eq!(ed.history().1.len(), 2);
        assert_eq!(ed.jump_history(3), 2);
        assert_eq!(ed.walls.len(), 12);
        assert_eq!(ed.jump_history(0), 3);
        assert!(ed.walls.is_empty());
        // cannot go beyond the stack
        assert_eq!(ed.jump_history(10), 3);
        assert_eq!(ed.undo_len(), 3);
    }

    #[test]
    fn history_jump_shows_kept_table_note() {
        let p = std::env::temp_dir().join(format!("ddc-ad6-{}-jump.ddc", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let path = p.to_string_lossy().into_owned();
        let mut ed = Editor::seeded(ddc_model::Model::open(&path).unwrap()).unwrap();
        let depth = ed.undo_len();
        // the Add is undone first, then a step under it: its status must not hide the note
        rect(&mut ed, 0.0);
        crate::editor::stage::add_created_table(&mut ed, &path, "ad6_jump");
        ed.model
            .sql()
            .execute("INSERT INTO ad6_jump(v) VALUES ('later')", [])
            .unwrap();
        assert_eq!(ed.jump_history(depth), 2);
        let s = ed.status.clone().unwrap_or_default();
        assert_eq!(s.matches("ad6_jump").count(), 1, "said once: {s}");
        assert!(ed.walls.is_empty());
        let rows: i64 = ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM ad6_jump", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "only the later row stays");
        drop(ed);
        let _ = std::fs::remove_file(&p);
    }
}
