// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fittings of the MEP network as a derived family. The network computes them from its runs, so
//! a fitting has no row: it is selectable in the plan and in the property panel, its properties
//! route to the runs, and delete and move refuse with a reason. The selection is
//! `Sel::Ext(EXT_FITTING, key)` with the stable key of `ddc_model::fitting_key`.

use crate::editor::{Editor, Sel};
use crate::scene_ids::EXT_FITTING;
use crate::shell::EditorApp;
use crate::ui_kit;
use crate::wall_tool::Mode;
use ddc_geom::{v2, V2};
use ddc_model::{FittingKind, FittingRef, Model};
use ddc_ui::Lang;
use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

/// Half side of the plan pick square around a fitting, m.
const HALF: f64 = 0.15;

type Cached = Option<((usize, u64), Arc<Vec<FittingRef>>)>;

thread_local! {
    static CACHE: RefCell<Cached> = const { RefCell::new(None) };
}

/// The network is rebuilt once per model change, never per frame or per lookup.
fn all(m: &Model) -> Arc<Vec<FittingRef>> {
    let key = (m as *const Model as usize, m.change_head());
    if let Some(v) = CACHE.with_borrow(|c| c.as_ref().filter(|c| c.0 == key).map(|c| c.1.clone())) {
        return v;
    }
    let v = Arc::new(m.mep_fittings(None).unwrap_or_default());
    CACHE.with_borrow_mut(|c| *c = Some((key, v.clone())));
    v
}

pub type Rings = Vec<Vec<V2>>;

/// Plan pick squares of the fittings whose first run is on `level`, as (key, rings).
pub fn footprints(m: &Model, level: i64) -> Vec<(i64, Rings)> {
    all(m)
        .iter()
        .filter(|f| f.level == level)
        .map(|f| {
            let (x, y) = (f.at.0, f.at.1);
            let ring = vec![
                v2(x - HALF, y - HALF),
                v2(x + HALF, y - HALF),
                v2(x + HALF, y + HALF),
                v2(x - HALF, y + HALF),
            ];
            (f.key, vec![ring])
        })
        .collect()
}

/// Plan outlines of the selected fittings: they have no row, so nothing else draws their selection.
pub fn selected_rings(ed: &Editor) -> Vec<Vec<V2>> {
    ed.selection
        .iter()
        .filter(|s| key_of(**s).is_some())
        .flat_map(|s| crate::selection::outlines(ed, *s))
        .collect()
}

pub fn key_of(s: Sel) -> Option<i64> {
    match s {
        Sel::Ext(c, key) if c == EXT_FITTING => Some(key),
        _ => None,
    }
}

pub fn sel(key: i64) -> Sel {
    Sel::Ext(EXT_FITTING, key)
}

pub fn lookup(m: &Model, key: i64) -> Option<FittingRef> {
    all(m).iter().find(|f| f.key == key).cloned()
}

fn tr(lang: Lang, key: &str) -> String {
    static CAT: OnceLock<ddc_ui::Catalog> = OnceLock::new();
    CAT.get_or_init(|| ddc_ui::Catalog::new(Lang::En))
        .t_in(lang, key)
}

fn kind_key(k: &FittingKind) -> String {
    format!("elem.mep_fitting.kind.{}", k.name())
}

/// A fitting is derived from its runs and is never deleted by itself. A selection of fittings
/// alone is refused with a reason (true: the caller stops, the model is untouched); in a mixed
/// selection the fittings drop out and the rest is deleted as usual (false).
pub fn refuse_delete(ed: &mut Editor) -> bool {
    if !ed.selection.iter().any(|s| key_of(*s).is_some()) {
        return false;
    }
    ed.selection.retain(|s| key_of(*s).is_none());
    if !ed.selection.is_empty() {
        return false;
    }
    let lang = ed.big.lang.unwrap_or(Lang::Ru);
    ed.status = Some(tr(lang, "elem.mep_fitting.no_delete"));
    true
}

/// Property panel of a selected fitting: kind, size, position, and the runs that make it, each
/// one a step to the run's own panel. False when no fitting is selected.
pub fn props(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    let Some(key) = ed.selection.iter().find_map(|s| key_of(*s)) else {
        return false;
    };
    let Some(f) = lookup(&ed.model, key) else {
        return false;
    };
    let lang = ed.big.lang.unwrap_or(Lang::Ru);
    let t = |k: &str| tr(lang, k);
    let u = ed.tool.units;
    let mut go: Option<Vec<Sel>> = None;
    ui_kit::element_header(ui, "fitting", &t(&kind_key(&f.kind)), "", key);
    {
        ui_kit::props(ui, "mep_fitting", |ui| {
            ui_kit::kv(ui, &t("elem.mep_fitting.size"), f.size.clone());
            let at = [f.at.0, f.at.1, f.at.2].map(|x| crate::units::edit_length(x, u));
            ui_kit::kv(ui, &t("elem.mep_fitting.at"), at.join("; "));
            for (i, run) in f.runs.iter().enumerate() {
                let label = t("elem.mep_fitting.runs").replace("{n}", &(i + 1).to_string());
                ui_kit::kv_widget(ui, &label, |ui| {
                    if ui.button(t("elem.mep_fitting.edit_run")).clicked() {
                        go = Some(vec![Sel::Mep(*run)]);
                    }
                });
            }
            if f.runs.len() > 1 && ui.button(t("elem.mep_fitting.select_runs")).clicked() {
                go = Some(f.runs.iter().map(|r| Sel::Mep(*r)).collect());
            }
        });
        ui_kit::hint(ui, t("elem.mep_fitting.derived"));
    }
    if let Some(s) = go {
        ed.selection = s;
    }
    true
}

/// Fittings (as selections) that touch any of `runs`, on the editor level.
fn touching(ed: &Editor, runs: &[i64]) -> Vec<Sel> {
    all(&ed.model)
        .iter()
        .filter(|f| f.level == ed.level && f.runs.iter().any(|r| runs.contains(r)))
        .map(|f| sel(f.key))
        .collect()
}

/// `mep.fitting`: the select tool for fittings. With runs selected it selects their fittings.
fn run_tool(a: &mut EditorApp) {
    let runs: Vec<i64> = a
        .ed
        .selection
        .iter()
        .filter_map(|s| match s {
            Sel::Mep(id) => Some(*id),
            _ => None,
        })
        .collect();
    a.ed.set_mode(Mode::Select);
    // no run selected: take every fitting of the level, so the press always does something
    let picked = if runs.is_empty() {
        all(&a.ed.model)
            .iter()
            .filter(|f| f.level == a.ed.level)
            .map(|f| sel(f.key))
            .collect()
    } else {
        touching(&a.ed, &runs)
    };
    if picked.is_empty() {
        a.ed.status = Some(a.sh.cat.t("elem.mep_fitting.hint"));
    } else {
        a.ed.status = Some(
            a.sh.cat
                .tf("elem.mep_fitting.picked", &[("n", &picked.len().to_string())]),
        );
        a.ed.selection = picked;
    }
}

pub fn register(a: &mut EditorApp) {
    a.register_command("mep.fitting", run_tool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::OpFn;
    use crate::selection::{candidates, in_box, BoxMode};
    use crate::wall_tool::Event;
    use ddc_model::{PipeMaterial, RunSpec, SystemKind};

    /// Editor with one pipe bent at (3, 0): one elbow; returns the run id.
    fn bent() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        let s = ed.model.add_mep_system("HW", SystemKind::Heating).unwrap();
        let spec = RunSpec::pipe(
            s,
            level,
            PipeMaterial::Steel,
            100,
            vec![(0.0, 0.0, 1.0), (3.0, 0.0, 1.0), (3.0, 4.0, 1.0)],
        )
        .unwrap();
        let run = ed.model.add_mep_run(&spec).unwrap();
        ed.reload().unwrap();
        ed.set_mode(Mode::Select);
        (ed, run)
    }

    fn the_fitting(ed: &Editor) -> FittingRef {
        let v = all(&ed.model);
        assert_eq!(v.len(), 1, "one elbow");
        v[0].clone()
    }

    #[test]
    fn selected_fitting_has_a_plan_outline() {
        let (mut ed, _) = bent();
        assert!(selected_rings(&ed).is_empty());
        let f = the_fitting(&ed);
        ed.selection = vec![sel(f.key)];
        let rings = selected_rings(&ed);
        assert_eq!(rings.len(), 1);
        let n = rings[0].len() as f64;
        let c = rings[0].iter().fold((0.0, 0.0), |a, p| (a.0 + p.x, a.1 + p.y));
        assert!((c.0 / n - f.at.0).abs() < 1e-6 && (c.1 / n - f.at.1).abs() < 1e-6);
    }

    #[test]
    fn click_box_and_tab_select_the_fitting() {
        let (mut ed, _) = bent();
        let f = the_fitting(&ed);
        let at = v2(f.at.0, f.at.1);
        assert!(ed.struct_fp.contains(EXT_FITTING, f.key));
        ed.handle(Event::Click(at));
        assert_eq!(ed.selection, vec![sel(f.key)]);
        let boxed = in_box(&ed, at - v2(1.0, 1.0), at + v2(1.0, 1.0), BoxMode::Crossing, |_| true);
        assert!(boxed.contains(&sel(f.key)));
        let tab = candidates(&ed, at, 0.25, |_| true);
        assert!(tab.iter().flatten().any(|s| *s == sel(f.key)));
    }

    #[test]
    fn delete_refuses_and_leaves_the_model_alone() {
        let (mut ed, run) = bent();
        let f = the_fitting(&ed);
        let head = ed.model.change_head();
        ed.selection = vec![sel(f.key)];
        ed.delete_selection();
        assert_eq!(ed.model.change_head(), head);
        assert!(ed.model.mep_run(run).is_ok());
        assert!(ed.status.as_deref().is_some_and(|s| !s.is_empty()));
        // mixed with its run: the fitting drops out, the run goes in one undo step
        ed.selection = vec![sel(f.key), Sel::Mep(run)];
        ed.delete_selection();
        assert!(ed.model.mep_run(run).is_err());
        ed.undo();
        assert!(ed.model.mep_run(run).is_ok());
    }

    #[test]
    fn fitting_goes_with_its_run_and_returns_with_the_same_key() {
        let (mut ed, run) = bent();
        let f = the_fitting(&ed);
        ed.selection = vec![sel(f.key)];
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_mep_run(run))))
            .unwrap();
        assert!(!ed.struct_fp.contains(EXT_FITTING, f.key));
        assert!(ed.selection.is_empty(), "stale fitting dropped from selection");
        ed.undo();
        assert_eq!(the_fitting(&ed).key, f.key, "same key after undo");
    }

    #[test]
    fn touching_finds_fittings_of_selected_runs() {
        let (ed, run) = bent();
        let f = the_fitting(&ed);
        assert_eq!(touching(&ed, &[run]), vec![sel(f.key)]);
        assert!(touching(&ed, &[run + 100]).is_empty());
    }

    #[test]
    fn strings_exist_in_both_locales_and_en_has_no_cyrillic() {
        let mut keys = vec![];
        for k in ["elbow", "tee", "cross", "transition", "cap"] {
            keys.push(format!("elem.mep_fitting.kind.{k}"));
        }
        for k in [
            "one",
            "many",
            "size",
            "at",
            "runs",
            "edit_run",
            "select_runs",
            "derived",
            "no_delete",
            "hint",
            "picked",
        ] {
            keys.push(format!("elem.mep_fitting.{k}"));
        }
        for key in keys {
            for l in [Lang::En, Lang::Ru] {
                let c = ddc_ui::Catalog::new(l);
                assert!(c.table(l).contains_key(&key), "{key}");
            }
            let en = tr(Lang::En, &key);
            assert!(!en.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c)), "{key}");
        }
    }
}
