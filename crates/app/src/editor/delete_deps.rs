// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What else goes with a delete: openings and their fills, hosted components, rebar, tags and
//! dimensions. A few on the view's level are reported in the status; many, or any on another
//! level, wait for the user's answer in a confirm dialog.

use super::delete::{Extra, Item};
use super::Editor;
use rusqlite::Connection;

/// Dependents up to this many, all on the view's level, are deleted without asking.
pub const QUIET_MAX: usize = 5;

/// Kinds in display order; `del.k.<kind>` is the plural message of each.
pub const KINDS: [&str; 10] = [
    "door", "window", "opening", "comp", "port", "rebar", "joist", "railing", "tag", "dim",
];

/// Tables a selection can hold whose rows carry a `level` column.
const LEVELLED: [&str; 6] = ["wall", "slab", "col", "beam", "comp", "room"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deps {
    /// (kind, count) for the kinds that go along, in `KINDS` order.
    pub counts: Vec<(&'static str, usize)>,
    /// Dependents plus selected items that are not on the view's level.
    pub other_level: usize,
    /// Rooms whose boundary uses a deleted wall (exact with `collect_exact`).
    pub rooms: usize,
    /// A dependents query failed, so the counts may be short: the delete always asks.
    pub unknown: bool,
}

/// A delete waiting for the answer; `extra` is replayed as it was asked.
#[derive(Clone, Debug)]
pub struct Pending {
    pub extra: Extra,
    pub deps: Deps,
    /// The selection the dialog describes; the answer deletes this, not a later selection.
    pub sel: Vec<super::Sel>,
    /// `Editor::revision` when asked: an undo, reload, level switch or new model voids the answer.
    pub revision: u64,
    /// The delete of a migrated family: the answer runs `elem::delete_confirmed`.
    pub via_elem: bool,
}

impl Deps {
    /// The rows of a query; a failed query marks the result unknown and yields no rows.
    fn known<T>(&mut self, found: Option<Vec<T>>) -> Vec<T> {
        self.unknown |= found.is_none();
        found.unwrap_or_default()
    }

    fn add(&mut self, kind: &'static str, other: bool) {
        match self.counts.iter_mut().find(|c| c.0 == kind) {
            Some(c) => c.1 += 1,
            None => self.counts.push((kind, 1)),
        }
        self.other_level += usize::from(other);
    }

    pub fn total(&self) -> usize {
        self.counts.iter().map(|c| c.1).sum()
    }

    pub fn needs_confirm(&self) -> bool {
        self.unknown || self.total() > QUIET_MAX || self.other_level > 0
    }

    /// "2 doors, 1 tag", in the UI language.
    pub fn list(&self) -> String {
        self.counts
            .iter()
            .map(|(k, n)| crate::tr_n(&format!("del.k.{k}"), *n as u64))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Tail of the status line, empty when nothing else is affected; starts with ". ".
    pub fn also(&self) -> String {
        let mut t = String::new();
        if self.total() > 0 {
            t = format!(". {}", crate::tr("del.also", &[("list", &self.list())]));
        }
        if self.rooms > 0 {
            t = format!("{t}. {}", crate::tr_n("del.rooms", self.rooms as u64));
        }
        t
    }
}

fn csv(ids: &[i64]) -> String {
    if ids.is_empty() {
        return "-1".into();
    }
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

fn ids(items: &[Item], table: &str) -> Vec<i64> {
    items.iter().filter(|i| i.0 == table).map(|i| i.1).collect()
}

/// Rows of a query; None when it fails (a table or column this file does not have).
fn rows<T>(
    db: &Connection,
    sql: &str,
    f: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Option<Vec<T>> {
    let mut st = db.prepare(sql).ok()?;
    let v = st.query_map([], |r| f(r)).ok()?.collect::<Result<_, _>>().ok()?;
    Some(v)
}

fn level_of(db: &Connection, table: &str, id: i64) -> Option<i64> {
    db.query_row(&format!("SELECT level FROM {table} WHERE id=?1"), [id], |r| r.get(0))
        .ok()
}

/// Does one stored dimension reference ("W 5 0 1", "O 7 0 0", "G 2 5") use a deleted element?
fn ref_hits(r: &str, walls: &[i64], openings: &[i64]) -> bool {
    let v: Vec<&str> = r.split(' ').collect();
    let at = |k: usize, set: &[i64]| {
        v.get(k)
            .and_then(|x| x.parse::<i64>().ok())
            .is_some_and(|x| set.contains(&x))
    };
    match v.first() {
        Some(&"W" | &"X" | &"N" | &"C" | &"Z") => at(1, walls),
        Some(&"O") => at(1, openings),
        Some(&"G") => at(2, walls),
        _ => false,
    }
}

/// Dependents of `items` as the model would remove or detach them; `level` is the view's level.
pub fn collect(db: &Connection, items: &[Item], level: i64) -> Deps {
    let mut d = Deps::default();
    let walls = ids(items, "wall");
    let slabs = ids(items, "slab");
    let rooms_sel = ids(items, "room");
    let mut openings = ids(items, "opening");
    let mut comps = ids(items, "comp");
    for (t, id) in items {
        if LEVELLED.contains(&t.as_str()) && level_of(db, t, *id).is_some_and(|l| l != level) {
            d.other_level += 1;
        }
    }
    if !walls.is_empty() {
        let sql = format!(
            "SELECT o.id, o.kind, w.level FROM opening o JOIN wall w ON w.id=o.wall
             WHERE o.wall IN ({})",
            csv(&walls)
        );
        let found = rows(db, &sql, |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        });
        for (id, kind, l) in d.known(found) {
            if openings.contains(&id) {
                continue;
            }
            openings.push(id);
            let k = match kind.as_str() {
                "door" => "door",
                "window" => "window",
                _ => "opening",
            };
            d.add(k, l != level);
        }
        let sql = format!(
            "SELECT count(*) FROM room WHERE id NOT IN ({}) AND level IN
             (SELECT level FROM wall WHERE id IN ({}))",
            csv(&rooms_sel),
            csv(&walls)
        );
        let n = rows(db, &sql, |r| r.get::<_, i64>(0)).and_then(|v| v.first().copied());
        d.unknown |= n.is_none();
        d.rooms = n.map_or(0, |n| n as usize);
    }
    let hosts: Vec<String> = [("host_wall", &walls), ("host_slab", &slabs)]
        .iter()
        .filter(|h| !h.1.is_empty())
        .map(|h| format!("{} IN ({})", h.0, csv(h.1)))
        .collect();
    if !hosts.is_empty() {
        let sql = format!("SELECT id, level FROM comp WHERE {}", hosts.join(" OR "));
        let found = rows(db, &sql, |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)));
        for (id, l) in d.known(found) {
            if !comps.contains(&id) {
                comps.push(id);
                d.add("comp", l != level);
            }
        }
    }
    // (rebar host kind, host table); the table name is also the item's table
    let hosted = [
        ("wall", "wall"),
        ("slab", "slab"),
        ("column", "col"),
        ("beam", "beam"),
        ("footing", "iso_footing"),
        ("raft", "raft"),
    ];
    for (kind, table) in hosted {
        let host = ids(items, table);
        if host.is_empty() {
            continue;
        }
        for set in ["rebar_set", "rebar_cover"] {
            let with_level = format!(
                "SELECT (SELECT level FROM {table} WHERE id=r.host) FROM {set} r
                 WHERE r.host_kind='{kind}' AND r.host IN ({})",
                csv(&host)
            );
            let plain = format!(
                "SELECT NULL FROM {set} r WHERE r.host_kind='{kind}' AND r.host IN ({})",
                csv(&host)
            );
            let levels = rows(db, &with_level, |r| r.get::<_, Option<i64>>(0))
                .or_else(|| rows(db, &plain, |r| r.get::<_, Option<i64>>(0)));
            for l in d.known(levels) {
                d.add("rebar", l.is_some_and(|l| l != level));
            }
        }
    }
    let comp_ids = ids(items, "comp");
    if !comp_ids.is_empty() {
        let sql = format!(
            "SELECT (SELECT level FROM comp WHERE id=p.comp) FROM mep_port p WHERE p.comp IN ({})",
            csv(&comp_ids)
        );
        let found = rows(db, &sql, |r| r.get::<_, Option<i64>>(0));
        for l in d.known(found) {
            d.add("port", l.is_some_and(|l| l != level));
        }
    }
    let stairs = ids(items, "stair");
    if !stairs.is_empty() {
        let railings = ids(items, "railing");
        let sql = format!("SELECT id, level FROM railing WHERE stair IN ({})", csv(&stairs));
        let found = rows(db, &sql, |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)));
        for (id, l) in d.known(found) {
            if !railings.contains(&id) {
                d.add("railing", l != level);
            }
        }
    }
    let annots = rows(db, "SELECT level, kind, refs FROM dim", |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    });
    for (l, kind, refs) in d.known(annots) {
        let other = l != level;
        if kind == "tag" {
            let v: Vec<&str> = refs.split(' ').collect();
            let host = v.get(2).and_then(|x| x.parse::<i64>().ok());
            let hit = host.is_some_and(|h| match v.get(1) {
                Some(&"w") => walls.contains(&h),
                Some(&"o") => openings.contains(&h),
                Some(&"c") => comps.contains(&h),
                Some(&"r") => rooms_sel.contains(&h),
                _ => false,
            });
            if hit {
                d.add("tag", other);
            }
        } else if refs.split(';').any(|r| ref_hits(r, &walls, &openings)) {
            d.add("dim", other);
        }
    }
    d.counts
        .sort_by_key(|k| KINDS.iter().position(|x| *x == k.0));
    d
}

/// `collect` with the exact room count: rooms outside the selection whose boundary uses a
/// deleted wall, not every room on the wall's level.
pub fn collect_exact(m: &ddc_model::Model, items: &[Item], level: i64) -> Deps {
    collect_exact_with(
        m,
        items,
        level,
        &|id| m.beam_system_joist_count(id).ok(),
        &|sql| rows(m.sql(), sql, |r| r.get::<_, i64>(0)),
    )
}

/// `collect_exact` with the joist count and the room-candidate query passed in; None from
/// either marks the result unknown instead of counting nothing.
fn collect_exact_with(
    m: &ddc_model::Model,
    items: &[Item],
    level: i64,
    joist_count: &dyn Fn(i64) -> Option<usize>,
    room_candidates: &dyn Fn(&str) -> Option<Vec<i64>>,
) -> Deps {
    let mut d = collect(m.sql(), items, level);
    // joists come from the built layout; the stored `count` is 0 for a spacing layout
    for id in ids(items, "beam_system") {
        let other = level_of(m.sql(), "beam_system", id).is_some_and(|l| l != level);
        let n = joist_count(id);
        d.unknown |= n.is_none();
        for _ in 0..n.unwrap_or(0) {
            d.add("joist", other);
        }
    }
    d.counts
        .sort_by_key(|k| KINDS.iter().position(|x| *x == k.0));
    let walls = ids(items, "wall");
    if walls.is_empty() {
        d.rooms = 0;
        return d;
    }
    let rooms_sel = ids(items, "room");
    let on_levels = format!(
        "SELECT id FROM room WHERE id NOT IN ({}) AND level IN (SELECT level FROM wall WHERE id IN ({}))",
        csv(&rooms_sel),
        csv(&walls)
    );
    let cand = room_candidates(&on_levels);
    d.unknown |= cand.is_none();
    let cand = cand.unwrap_or_default();
    let mut failed = false;
    d.rooms = cand
        .into_iter()
        .filter(|r| match m.room_boundaries(*r) {
            Ok(b) => b
                .iter()
                .any(|x| matches!(x, ddc_model::Boundary::Wall(w) if walls.contains(w))),
            Err(_) => {
                failed = true;
                false
            }
        })
        .count();
    d.unknown |= failed;
    d
}

impl Editor {
    /// The user chose "Delete all": the same delete again, without asking.
    pub fn confirm_delete(&mut self) -> bool {
        let Some(p) = self.pending_delete.take() else {
            return false;
        };
        if p.revision != self.revision {
            self.say(crate::status::Severity::Info, crate::tr("del.cancelled", &[]));
            return false;
        }
        self.selection = p.sel;
        if p.via_elem {
            return crate::elem::delete_confirmed(self);
        }
        self.delete_checked(p.extra, true)
    }

    /// The user chose "Cancel": nothing was changed.
    pub fn cancel_delete(&mut self) {
        if self.pending_delete.take().is_some() {
            self.say(crate::status::Severity::Info, crate::tr("del.cancelled", &[]));
        }
    }
}

/// The confirm dialog, shown while a delete is pending. Esc cancels and Enter does nothing:
/// the destructive button is never the default.
pub fn dialog(ed: &mut Editor, ctx: &egui::Context) {
    dialog_in(ed, ctx, crate::ui_is_ru());
}

/// As `dialog`, in an explicit language.
pub(crate) fn dialog_in(ed: &mut Editor, ctx: &egui::Context, ru: bool) {
    use crate::ui_kit::{dialog_footer, Dialog, DialogSize};
    use ddc_ui::sem::space;
    use ddc_ui::theme::Role;
    use ddc_ui::widgets::Button;
    use egui::RichText;
    let Some(p) = ed.pending_delete.as_ref() else {
        return;
    };
    let th = ddc_ui::theme::of(ctx);
    let lead = crate::tr_n_lang(ru, "del.confirm.lead", p.extra.items.len().max(1) as u64);
    let lines: Vec<String> = p
        .deps
        .counts
        .iter()
        .map(|(k, n)| crate::tr_n_lang(ru, &format!("del.k.{k}"), *n as u64))
        .collect();
    let levels = p.deps.other_level;
    let rooms = p.deps.rooms;
    let unknown = p.deps.unknown;
    let mut open = true;
    let mut answer: Option<bool> = None;
    Dialog::new(
        "del.confirm",
        crate::tr_lang(ru, "del.confirm.title", &[]),
        DialogSize::Confirm,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        ui.label(
            RichText::new(lead)
                .font(th.font(Role::Label))
                .color(th.c("text.primary")),
        );
        ui.add_space(space::SM);
        for l in &lines {
            ui.label(RichText::new(format!("\u{2022} {l}")).font(th.font(Role::Numeric)));
        }
        if levels > 0 {
            ui.add_space(space::SM);
            ui.label(
                RichText::new(crate::tr_n_lang(ru, "del.confirm.levels", levels as u64))
                    .font(th.font(Role::Label))
                    .color(th.c("warning")),
            );
        }
        if unknown {
            ui.label(
                RichText::new(crate::tr_lang(ru, "del.confirm.unknown", &[]))
                    .font(th.font(Role::Label))
                    .color(th.c("warning")),
            );
        }
        if rooms > 0 {
            ui.label(
                RichText::new(crate::tr_n_lang(ru, "del.rooms", rooms as u64))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            );
        }
        ui.add_space(space::SM);
        dialog_footer(ui, |ui| {
            if Button::danger(&crate::tr_lang(ru, "del.confirm.all", &[]))
                .show(ui, &th)
                .clicked()
            {
                answer = Some(true);
            }
            if Button::secondary(&crate::tr_lang(ru, "del.confirm.cancel", &[]))
                .tooltip("", "Esc")
                .show(ui, &th)
                .clicked()
            {
                answer = Some(false);
            }
        });
    });
    match answer {
        Some(true) => {
            ed.confirm_delete();
        }
        Some(false) => ed.cancel_delete(),
        None if !open => ed.cancel_delete(),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::delete::tests::dump;
    use super::*;
    use crate::editor::Sel;
    use crate::modify_tools::tests::walls;
    use ddc_model::{DimKind, DimRef, DimSpec, OpeningSpec, TagHost, TagSpec, WallSpec};

    fn door(along: f64) -> OpeningSpec {
        OpeningSpec {
            along,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        }
    }

    /// One wall with `n` doors, selected.
    fn wall_with_doors(n: usize) -> (Editor, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        walls(&mut e, &[((0.0, 0.0), (20.0, 0.0))]);
        let w = e.walls[0].id;
        for i in 0..n {
            e.model.add_opening(w, &door(1.0 + 1.5 * i as f64)).unwrap();
        }
        e.reload().unwrap();
        e.selection = vec![Sel::Wall(w)];
        (e, w)
    }

    fn tables(e: &Editor) -> Vec<Vec<String>> {
        ["wall", "opening", "dim"].iter().map(|t| dump(e, t)).collect()
    }

    #[test]
    fn wall_with_two_doors_reports_them_and_asks_nothing() {
        let (mut e, _) = wall_with_doors(2);
        let steps = e.undo_len();
        e.delete_selection();
        assert!(e.pending_delete.is_none());
        assert!(e.walls.is_empty());
        let s = e.status.clone().unwrap();
        assert!(s.contains(&crate::tr_n("del.k.door", 2)), "{s}");
        assert_eq!(e.status_severity(), Some(crate::status::Severity::Info));
        assert_eq!(e.undo_len(), steps + 1);
    }

    #[test]
    fn dimensions_and_tags_are_counted() {
        let (mut e, w) = wall_with_doors(1);
        let level = e.level;
        e.model
            .add_dim(&DimSpec {
                level,
                kind: DimKind::Linear,
                refs: vec![
                    DimRef::WallEnd { wall: w, face: 0, end: 0 },
                    DimRef::WallEnd { wall: w, face: 0, end: 1 },
                ],
                dir: (0.0, 0.0),
                side: 1.0,
                offset: 0.5,
                tier: 0,
                auto: false,
            })
            .unwrap();
        e.model
            .add_tags(&[TagSpec {
                level,
                host: TagHost::Wall(w),
                offset: (0.0, 0.5),
                leader: false,
            }])
            .unwrap();
        let d = collect(e.model.sql(), &[("wall".to_string(), w)], level);
        assert_eq!(d.counts, vec![("door", 1), ("tag", 1), ("dim", 1)]);
        assert_eq!(d.other_level, 0);
        e.delete_selection();
        let s = e.status.clone().unwrap();
        for k in ["door", "tag", "dim"] {
            assert!(s.contains(&crate::tr_n(&format!("del.k.{k}"), 1)), "{s}");
        }
    }

    #[test]
    fn many_dependents_wait_for_the_answer() {
        let (mut e, w) = wall_with_doors(QUIET_MAX + 1);
        let before = tables(&e);
        let steps = e.undo_len();
        e.delete_selection();
        assert!(e.pending_delete.is_some());
        assert_eq!(tables(&e), before);
        assert_eq!(e.undo_len(), steps);
        assert_eq!(e.selection, vec![Sel::Wall(w)]);
        e.cancel_delete();
        assert!(e.pending_delete.is_none());
        assert_eq!(tables(&e), before);
        assert_eq!(e.undo_len(), steps);
    }

    #[test]
    fn the_answer_deletes_what_was_asked_and_a_stale_one_is_void() {
        let (mut e, w) = wall_with_doors(QUIET_MAX + 1);
        walls(&mut e, &[((0.0, 10.0), (4.0, 10.0))]);
        let other = e.walls.iter().find(|x| x.id != w).unwrap().id;
        e.selection = vec![Sel::Wall(w)];
        e.delete_selection();
        e.selection = vec![Sel::Wall(other)];
        assert!(e.confirm_delete());
        assert!(e.walls.iter().any(|x| x.id == other), "a later selection is not deleted");
        assert!(!e.walls.iter().any(|x| x.id == w));
        e.undo();
        e.selection = vec![Sel::Wall(w)];
        e.delete_selection();
        let (before, steps) = (tables(&e), e.undo_len());
        e.reload().unwrap();
        assert!(!e.confirm_delete(), "the model moved on");
        assert_eq!((tables(&e), e.undo_len()), (before, steps));
    }

    #[test]
    fn another_level_dependent_forces_the_confirm_and_one_undo_restores_all() {
        let (mut e, w) = wall_with_doors(1);
        let upper = e.model.add_level("L2", 3.0).unwrap();
        e.model
            .add_dim(&DimSpec {
                level: upper,
                kind: DimKind::Linear,
                refs: vec![
                    DimRef::WallEnd { wall: w, face: 0, end: 0 },
                    DimRef::WallEnd { wall: w, face: 0, end: 1 },
                ],
                dir: (0.0, 0.0),
                side: 1.0,
                offset: 0.5,
                tier: 0,
                auto: false,
            })
            .unwrap();
        e.reload().unwrap();
        let before = tables(&e);
        let steps = e.undo_len();
        e.selection = vec![Sel::Wall(w)];
        e.delete_selection();
        let p = e.pending_delete.as_ref().expect("asks");
        assert_eq!(p.deps.total(), 2);
        assert_eq!(p.deps.other_level, 1);
        assert_eq!(tables(&e), before);
        assert!(e.confirm_delete());
        assert!(e.pending_delete.is_none());
        assert!(e.walls.is_empty());
        assert_eq!(e.undo_len(), steps + 1);
        assert_eq!(e.status_severity(), Some(crate::status::Severity::Warning));
        e.undo();
        assert_eq!(tables(&e), before);
    }

    #[test]
    fn selected_wall_on_another_level_asks() {
        let mut e = Editor::new_in_memory().unwrap();
        let upper = e.model.add_level("L2", 3.0).unwrap();
        let ty = e.current_type().expect("type").id;
        let w = e
            .model
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: upper,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ty,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
        e.selection = vec![Sel::Wall(w)];
        e.delete_selection();
        assert!(e.pending_delete.is_some());
    }

    #[test]
    fn beam_system_spacing_confirm_counts_joists() {
        use ddc_model::{BeamLayout, BeamSystemSpec, Just, MemberShape};
        let mut e = Editor::new_in_memory().unwrap();
        let level = e.level;
        let jt = e
            .model
            .add_member_type("R 100x200", &MemberShape::Rect { b: 0.1, h: 0.2 }, "Steel")
            .unwrap();
        let id = e
            .model
            .add_beam_system(&BeamSystemSpec {
                level,
                member_type: jt,
                ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                holes: vec![],
                start_edge: None,
                z: 3.0,
                angle: std::f64::consts::FRAC_PI_2,
                spacing: 0.5,
                layout: BeamLayout::FixedStart,
                end_gap: 0.01,
                zjust: Just::Pos,
            })
            .unwrap();
        let n = e.model.beam_system_joist_count(id).unwrap();
        assert!(n > QUIET_MAX, "{n}");
        let d = collect_exact(&e.model, &[("beam_system".to_string(), id)], level);
        assert_eq!(d.counts, vec![("joist", n)]);
        assert!(d.needs_confirm());
    }

    fn spacing_system(e: &mut Editor) -> i64 {
        use ddc_model::{BeamLayout, BeamSystemSpec, Just, MemberShape};
        let level = e.level;
        let jt = e
            .model
            .add_member_type("R 100x200", &MemberShape::Rect { b: 0.1, h: 0.2 }, "Steel")
            .unwrap();
        e.model
            .add_beam_system(&BeamSystemSpec {
                level,
                member_type: jt,
                ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                holes: vec![],
                start_edge: None,
                z: 3.0,
                angle: std::f64::consts::FRAC_PI_2,
                spacing: 0.5,
                layout: BeamLayout::FixedStart,
                end_gap: 0.01,
                zjust: Just::Pos,
            })
            .unwrap()
    }

    #[test]
    fn joist_count_error_still_confirms() {
        let mut e = Editor::new_in_memory().unwrap();
        let id = spacing_system(&mut e);
        let level = e.level;
        let items = [("beam_system".to_string(), id)];
        let ok = |sql: &str| rows(e.model.sql(), sql, |r| r.get::<_, i64>(0));
        let d = collect_exact_with(&e.model, &items, level, &|_| None, &ok);
        assert!(d.unknown);
        assert_eq!(d.total(), 0);
        assert!(d.needs_confirm(), "an uncounted joist set must not delete silently");
    }

    #[test]
    fn room_query_error_still_confirms() {
        let mut e = Editor::new_in_memory().unwrap();
        walls(&mut e, &[((0.0, 0.0), (6.0, 0.0))]);
        let w = e.walls[0].id;
        let level = e.level;
        let items = [("wall".to_string(), w)];
        let d = collect_exact_with(&e.model, &items, level, &|_| Some(0), &|_| None);
        assert!(d.unknown);
        assert_eq!(d.total(), 0);
        assert!(d.needs_confirm());
    }

    #[test]
    fn collect_error_marks_unknown() {
        let db = Connection::open_in_memory().unwrap();
        let items = [("wall".to_string(), 1)];
        let d = collect(&db, &items, 1);
        assert!(d.unknown);
        assert!(d.needs_confirm());
        assert!(!Deps::default().needs_confirm());
    }

    #[test]
    fn no_error_no_unknown() {
        let (e, w) = wall_with_doors(2);
        let level = e.level;
        let d = collect_exact(&e.model, &[("wall".to_string(), w)], level);
        assert!(!d.unknown);
        assert!(!d.needs_confirm());
        let mut e2 = Editor::new_in_memory().unwrap();
        let id = spacing_system(&mut e2);
        let level = e2.level;
        let d = collect_exact(&e2.model, &[("beam_system".to_string(), id)], level);
        assert!(!d.unknown);
    }

    #[test]
    fn collect_exact_touches_one_level() {
        let mut e = Editor::new_in_memory().unwrap();
        let ring = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        let segs: Vec<_> = (0..4).map(|i| (ring[i], ring[(i + 1) % 4])).collect();
        walls(&mut e, &segs);
        let w = e.walls[0].id;
        let lower = e.level;
        let upper = e.model.add_level("L2", 3.0).unwrap();
        let ty = e.current_type().expect("type").id;
        for (a, b) in &segs {
            e.model
                .add_wall(&WallSpec {
                    a: *a,
                    b: *b,
                    level: upper,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: ty,
                    offset0: None,
                })
                .unwrap();
        }
        for level in [lower, upper] {
            e.model
                .add_room(&ddc_model::RoomSpec {
                    level,
                    name: "R".into(),
                    number: "1".into(),
                    point: (3.0, 2.0),
                    height: 3.0,
                })
                .unwrap();
        }
        e.reload().unwrap();
        let d = collect_exact(&e.model, &[("wall".to_string(), w)], lower);
        assert_eq!(d.rooms, 1, "only the room on the wall's level");
    }
}
