// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Edits of many selected elements at once (multi panel, paste parameters, quick HUD, query,
//! AI): the same opening checks as one element, one undo step and exact counts in one line.
use crate::editor::Editor;
use crate::opening_tool::PlaceError;
use crate::status::Severity;
use ddc_model::Edit;

/// What a batch did: applied, refused by the checks, pinned and locked left alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub changed: usize,
    pub refused: usize,
    pub pinned: usize,
    pub locked: usize,
    /// Wall edits refused because a hosted opening would no longer fit.
    pub refused_walls: usize,
}

/// Splits the edits that pass the opening checks (size > 0, inside the wall, below its top,
/// no overlap; a wall edit keeps its hosted openings fitting, see `fit_check::screen`) from
/// the refused ones. Each opening is checked against the accepted ones at their new size and
/// the rest at their current size, so every pair that ends up in the model was checked as it
/// will stand, and a refused one keeps its size. A refused wall reads `PlaceError::Hosts`.
pub fn screen_openings(ed: &Editor, edits: Vec<Edit>) -> (Vec<Edit>, Vec<(i64, PlaceError)>) {
    if !crate::fit_check::touches(&edits) {
        return (edits, vec![]);
    }
    let fit = crate::fit_check::Fit::load(&ed.model)
        .unwrap_or_else(|_| crate::fit_check::Fit::of_editor(ed));
    let refused = crate::fit_check::screen(&fit, &edits);
    let out: std::collections::HashSet<usize> = refused.iter().map(|r| r.index).collect();
    let kept = edits
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !out.contains(i))
        .map(|(_, e)| e)
        .collect();
    (kept, refused.into_iter().map(|r| (r.id, r.why)).collect())
}

/// Applies the checked edits in one undo step and says in one line what changed, what was
/// refused (the first reason named), pinned or locked. `done_key` takes `{n}`.
pub fn apply(
    ed: &mut Editor,
    edits: Vec<Edit>,
    refused: &[(i64, PlaceError)],
    done_key: &str,
) -> Result<Counts, String> {
    let (edits, pinned) = ed.model.split_pinned(&edits).map_err(|e| e.to_string())?;
    let (edits, locked) = ed.model.split_locked(&edits).map_err(|e| e.to_string())?;
    let is_wall = |e: &PlaceError| matches!(e, PlaceError::Hosts { .. });
    let walls = refused.iter().filter(|(_, e)| is_wall(e)).count();
    let c = Counts {
        changed: edits.len(),
        refused: refused.len() - walls,
        pinned: pinned.len(),
        locked: locked.len(),
        refused_walls: walls,
    };
    if c.changed > 0 {
        if let Err(e) = ed.apply_xlsx(edits, "") {
            ed.say(Severity::Error, crate::tr("type_dialog.status.not_changed", &[("e", &e)]));
            return Err(e);
        }
    }
    let units = ed.tool.units;
    let why_of = |wall: bool| {
        refused.iter().find(|(_, e)| is_wall(e) == wall).map(|(id, e)| {
            let t = e.text(crate::ui_is_ru(), units);
            crate::tr("lnp.why_at", &[("id", &id.to_string()), ("e", &t)])
        })
    };
    let (why, why_walls) = (why_of(false), why_of(true));
    let (sev, msg) = counts_text_with(&c, done_key, why.as_deref(), why_walls.as_deref());
    ed.say(sev, msg);
    Ok(c)
}

/// One status line for the counts; nothing applied reads as a refusal.
pub fn counts_text(c: &Counts, done_key: &str, why: Option<&str>) -> (Severity, String) {
    counts_text_with(c, done_key, why, None)
}

/// `counts_text` with the first reason of a refused wall (`why_walls`).
pub fn counts_text_with(
    c: &Counts,
    done_key: &str,
    why: Option<&str>,
    why_walls: Option<&str>,
) -> (Severity, String) {
    let mut parts = vec![crate::tr(done_key, &[("n", &c.changed.to_string())])];
    if c.refused > 0 {
        let n = c.refused.to_string();
        parts.push(crate::tr("lnp.openings_refused", &[("n", &n), ("why", why.unwrap_or(""))]));
    }
    if c.refused_walls > 0 {
        let (n, why) = (c.refused_walls.to_string(), why_walls.unwrap_or(""));
        parts.push(crate::tr("xlval.walls_refused", &[("n", &n), ("why", why)]));
    }
    if c.pinned > 0 {
        parts.push(crate::tr("edst.spreadsheet_pinned", &[("n", &c.pinned.to_string())]));
    }
    if c.locked > 0 {
        parts.push(crate::tr("locked_values.kept", &[("n", &c.locked.to_string())]));
    }
    let sev = if c.changed == 0 {
        Severity::Refusal
    } else if c.refused + c.refused_walls + c.pinned + c.locked > 0 {
        Severity::Warning
    } else {
        Severity::Info
    };
    (sev, parts.join(" \u{b7} "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Sel;
    use crate::shell::panels::{edit_openings, edit_walls};
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    /// A 6 x 4 m room; doors 0.9 m wide at 1 m and 3 m along a long wall.
    fn room_with_doors() -> (Editor, i64, Vec<i64>) {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 4.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.set_mode(Mode::Select);
        let len = |ed: &Editor, id: i64| ed.host_of(id).map_or(0.0, |h| h.len());
        let w = ed.walls.iter().map(|w| w.id).find(|&id| len(&ed, id) > 5.5).unwrap();
        assert!(len(&ed, w) < 6.5, "the long wall is about 6 m");
        for along in [1.0, 3.0] {
            ed.model
                .add_opening(
                    w,
                    &ddc_model::OpeningSpec {
                        along,
                        width: 0.9,
                        height: 2.1,
                        sill: 0.0,
                        kind: "door".into(),
                    },
                )
                .unwrap();
        }
        ed.reload().unwrap();
        let ids: Vec<i64> = ed.openings.iter().map(|o| o.id).collect();
        assert_eq!(ids.len(), 2);
        (ed, w, ids)
    }

    fn widths(ed: &Editor) -> Vec<f64> {
        ed.openings.iter().map(|o| o.width).collect()
    }

    #[test]
    fn multi_opening_width_zero_refused() {
        let (mut ed, _, ids) = room_with_doors();
        let d0 = ed.undo_len();
        for bad in [0.0, -0.005, f64::NAN] {
            assert!(!edit_openings(&mut ed, &ids, [Some(bad), None, None]), "{bad}");
            assert_eq!(widths(&ed), vec![0.9, 0.9], "{bad} reached the model");
            assert_eq!(ed.undo_len(), d0, "{bad}: a refused batch left an undo step");
            assert_eq!(ed.status_severity(), Some(Severity::Refusal));
            let st = ed.status.clone().unwrap_or_default();
            let c = Counts { refused: 2, ..Counts::default() };
            let why = crate::tr(
                "lnp.why_at",
                &[
                    ("id", &ids[0].to_string()),
                    ("e", &PlaceError::NonPositive.text(crate::ui_is_ru(), ed.tool.units)),
                ],
            );
            let want = counts_text(&c, "type_dialog.status.openings_changed", Some(&why)).1;
            assert_eq!(st, want);
        }
    }

    #[test]
    fn multi_opening_valid_edit_is_one_step_and_undoes() {
        let (mut ed, _, ids) = room_with_doors();
        let d0 = ed.undo_len();
        assert!(edit_openings(&mut ed, &ids, [Some(1.0), None, None]));
        assert_eq!(widths(&ed), vec![1.0, 1.0]);
        // mixed sills stay: only the edited field is written
        assert!(ed.openings.iter().all(|o| (o.height - 2.1).abs() < 1e-9 && o.sill == 0.0));
        assert_eq!(ed.undo_len(), d0 + 1);
        assert_eq!(ed.status_severity(), Some(Severity::Info));
        let want = crate::tr("type_dialog.status.openings_changed", &[("n", "2")]);
        assert_eq!(ed.status.as_deref(), Some(want.as_str()));
        ed.undo();
        assert_eq!(widths(&ed), vec![0.9, 0.9]);
    }

    #[test]
    fn multi_opening_overlap_refuses_only_the_one_that_hits() {
        let (mut ed, _, ids) = room_with_doors();
        // 1.0 + 2.1 runs into the door at 3.0; the one at 3.0 still fits with 2.1
        assert!(edit_openings(&mut ed, &ids, [Some(2.1), None, None]));
        assert_eq!(widths(&ed), vec![0.9, 2.1]);
        assert_eq!(ed.status_severity(), Some(Severity::Warning));
        let st = ed.status.clone().unwrap_or_default();
        assert!(st.starts_with(&crate::tr("type_dialog.status.openings_changed", &[("n", "1")])), "{st}");
        assert!(st.contains(&format!("#{}", ids[0])), "{st}");
        ed.undo();
        assert_eq!(widths(&ed), vec![0.9, 0.9]);
    }

    #[test]
    fn paste_params_opening_wider_than_wall_refused() {
        let (mut ed, _, ids) = room_with_doors();
        let d0 = ed.undo_len();
        ed.selection = ids.iter().map(|i| Sel::Opening(*i)).collect();
        let clip = crate::quick::Clip::Opening {
            kind: ed.openings[0].kind,
            dims: [7.0, 2.1, 0.0],
        };
        assert!(!crate::quick::paste_params(&mut ed, &clip));
        assert_eq!(widths(&ed), vec![0.9, 0.9]);
        assert_eq!(ed.undo_len(), d0);
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
        // a fitting paste goes through, mixed values replaced as pasted
        let ok = crate::quick::Clip::Opening { kind: ed.openings[0].kind, dims: [0.8, 2.0, 0.1] };
        assert!(crate::quick::paste_params(&mut ed, &ok));
        assert!(ed.openings.iter().all(|o| o.width == 0.8 && o.height == 2.0 && o.sill == 0.1));
        assert_eq!(ed.undo_len(), d0 + 1);
    }

    #[test]
    fn opening_too_tall_for_wall_refused() {
        let (mut ed, _, ids) = room_with_doors();
        assert!(!edit_openings(&mut ed, &ids, [None, Some(9.0), None]));
        assert!(ed.openings.iter().all(|o| (o.height - 2.1).abs() < 1e-9));
    }

    #[test]
    fn pinned_wall_multi_height_reports_zero_changed_one_pinned() {
        let (mut ed, w, _) = room_with_doors();
        ed.model.pin(&[("wall", w)]).unwrap();
        ed.reload().unwrap();
        let before = ed.can_undo();
        let d0 = ed.undo_len();
        assert!(!edit_walls(&mut ed, &[w], None, Some(2.5)));
        assert_eq!(ed.undo_len(), d0, "an all-pinned set leaves no undo step");
        assert_eq!(ed.can_undo(), before);
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
        let c = Counts { pinned: 1, ..Counts::default() };
        let want = counts_text(&c, "type_dialog.status.walls_changed", None).1;
        assert_eq!(ed.status.as_deref(), Some(want.as_str()));
        assert!(want.contains(&crate::tr("edst.spreadsheet_pinned", &[("n", "1")])), "{want}");
        // the other walls still change, the pinned one is counted
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        assert!(edit_walls(&mut ed, &ids, None, Some(2.5)));
        let c = Counts { changed: 3, pinned: 1, ..Counts::default() };
        let want = counts_text(&c, "type_dialog.status.walls_changed", None).1;
        assert_eq!(ed.status.as_deref(), Some(want.as_str()));
        assert_eq!(ed.undo_len(), d0 + 1);
    }

    #[test]
    fn hud_opening_commit_goes_through_the_same_checks() {
        let (mut ed, _, ids) = room_with_doors();
        ed.selection = ids.iter().map(|i| Sel::Opening(*i)).collect();
        let h = crate::quick::hud::build(&ed).unwrap();
        let (hud, u) = (crate::quick::hud::Target::Opening(0), crate::units::Units::Metric);
        let d0 = ed.undo_len();
        assert!(!crate::quick::hud::commit(&mut ed, &h, hud, "0", u));
        assert_eq!(widths(&ed), vec![0.9, 0.9]);
        assert!(ed.openings.iter().all(|o| (o.height - 2.1).abs() < 1e-9 && o.sill == 0.0));
        assert_eq!(ed.undo_len(), d0);
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
        assert!(crate::quick::hud::commit(&mut ed, &h, hud, "1000", u));
        assert_eq!(widths(&ed), vec![1.0, 1.0]);
        assert_eq!(ed.undo_len(), d0 + 1);
    }

    #[test]
    fn ai_assign_of_an_opening_size_uses_the_same_checks() {
        use crate::ai::plan::{compile_one, CompileCx, ElRef, PlanOp, Targets};
        use crate::query::{Assign, Val};
        let (mut ed, _, ids) = room_with_doors();
        let sets = crate::ai::sets::Sets::default();
        let mut slots = std::collections::HashMap::new();
        let mut cx = CompileCx {
            ed: &mut ed,
            slots: &mut slots,
            sets: &sets,
            gens: &[],
            rules: &[],
            declared: std::collections::HashSet::new(),
            skips: vec![],
        };
        let op = |w: &str| PlanOp::Assign {
            target: Targets::Refs(
                ids.iter().map(|i| ElRef::parse(&format!("door:{i}")).unwrap()).collect(),
            ),
            set: vec![Assign { key: "width".into(), val: Val::Lit(w.into()) }],
        };
        let Err(crate::ai::plan::PlanError(err)) = compile_one(&mut cx, &op("7000")) else {
            panic!("a 7 m door on a 6 m wall compiled");
        };
        assert!(err.contains("cannot take this size"), "{err}");
        assert!(err.contains(&ids[0].to_string()), "{err}");
        assert_eq!(widths(cx.ed), vec![0.9, 0.9], "a refused item leaves the draft as it was");
        // compile runs the item on the AI scratch copy: an accepted size is drafted there
        assert!(compile_one(&mut cx, &op("1000")).is_ok());
        assert_eq!(widths(&ed), vec![1.0, 1.0]);
    }

    #[test]
    fn ai_relative_width_skips_the_door_that_does_not_fit() {
        use crate::ai::plan::{compile_one, CompileCx, ElRef, PlanOp, Targets};
        use crate::query::{Assign, Val};
        let (mut ed, _, ids) = room_with_doors();
        let sets = crate::ai::sets::Sets::default();
        let mut slots = std::collections::HashMap::new();
        let mut cx = CompileCx {
            ed: &mut ed,
            slots: &mut slots,
            sets: &sets,
            gens: &[],
            rules: &[],
            declared: std::collections::HashSet::new(),
            skips: vec![],
        };
        let op = |v: Val| PlanOp::Assign {
            target: Targets::Refs(
                ids.iter().map(|i| ElRef::parse(&format!("door:{i}")).unwrap()).collect(),
            ),
            set: vec![Assign { key: "width".into(), val: v }],
        };
        // each item is drafted on the scratch copy; every case starts from the 0.9 m doors
        let top = cx.ed.model.op_top().unwrap();
        let fresh = |cx: &mut CompileCx| {
            cx.ed.model.rollback_to(top).unwrap();
            cx.ed.reload().unwrap();
            cx.skips.clear();
            assert_eq!(widths(cx.ed), vec![0.9, 0.9]);
        };
        // three times wider: the door 1 m from the wall end no longer fits, the other does
        assert!(compile_one(&mut cx, &op(Val::Rel('*', "3".into()))).is_ok());
        assert_eq!(cx.skips.len(), 1, "{:?}", cx.skips);
        assert_eq!(cx.skips[0].id, ids[0]);
        assert!(!cx.skips[0].why.is_empty());
        let w = widths(cx.ed);
        assert_eq!(w[0], 0.9, "the skipped door keeps its width");
        assert!((w[1] - 2.7).abs() < 1e-9, "{w:?}");
        // +0.5 m fits both: nothing is skipped
        fresh(&mut cx);
        assert!(compile_one(&mut cx, &op(Val::Rel('+', "0.5 m".into()))).is_ok());
        assert!(cx.skips.is_empty(), "{:?}", cx.skips);
        // a percent change reads the same way
        fresh(&mut cx);
        assert!(compile_one(&mut cx, &op(Val::Rel('%', "10".into()))).is_ok());
        assert!(cx.skips.is_empty());
    }

    #[test]
    fn query_assign_of_an_opening_size_counts_refusals() {
        let (mut ed, _, ids) = room_with_doors();
        let sels: Vec<Sel> = ids.iter().map(|i| Sel::Opening(*i)).collect();
        let a = crate::query::Assign {
            key: "width".into(),
            val: crate::query::Val::Lit("2100".into()),
        };
        let d0 = ed.undo_len();
        assert_eq!(crate::query::assign(&mut ed, &sels, &[a]), Ok(1));
        assert_eq!(widths(&ed), vec![0.9, 2.1]);
        assert_eq!(ed.undo_len(), d0 + 1);
        assert_eq!(ed.status_severity(), Some(Severity::Warning));
    }

    #[test]
    fn refusal_texts_exist_in_both_languages() {
        for key in ["lnp.openings_refused", "lnp.why_at"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert_ne!(en, key);
            assert_ne!(ru, key);
            assert!(en.contains("{n}") || en.contains("{id}"), "{en}");
            assert!(ru.contains("{n}") || ru.contains("{id}"), "{ru}");
        }
        assert_ne!(
            crate::tr_str(false, "lnp.openings_refused"),
            crate::tr_str(true, "lnp.openings_refused")
        );
    }
}
