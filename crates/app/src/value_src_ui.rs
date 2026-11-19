//! Who set a parameter value (`value_src`): a badge beside the value with the source in its
//! tooltip, and "Mark verified" on one value or on every unverified one of the selection.
//! Each verify is one `run_op`: one undo step.

use crate::editor::{Editor, OpFn};
use ddc_model::{Model, SrcKind, ValueSrc};
use ddc_ui::theme::Role;
use std::sync::Arc;

/// (table, id, parameter): the row that holds the value, as `LockKey`.
pub type Cell = (String, i64, String);

pub fn kind_key(k: SrcKind) -> &'static str {
    match k {
        SrcKind::User => "value_src.src.user",
        SrcKind::Ai => "value_src.src.ai",
        SrcKind::Xlsx => "value_src.src.spreadsheet",
        SrcKind::Ifc => "value_src.src.ifc",
        SrcKind::Generator => "value_src.src.generator",
        SrcKind::RuleFix => "value_src.src.rule_fix",
    }
}

fn badge_key(s: &ValueSrc) -> &'static str {
    match (s.kind.by_ai(), s.verified()) {
        (true, false) => "value_src.badge.ai",
        (true, true) => "value_src.badge.ai_ok",
        (false, _) => "value_src.badge.import",
    }
}

/// The tooltip: who, when, which request and prompt, and whether a person verified it.
pub fn tip(s: &ValueSrc, ru: bool) -> String {
    let t = |k: &'static str| crate::tr_str(ru, k);
    let at = crate::source_ui::date(s.at);
    let mut lines = vec![crate::tr_lang(
        ru,
        "value_src.tip.set_by",
        &[("who", t(kind_key(s.kind))), ("at", &at)],
    )];
    if let Some(r) = &s.request {
        lines.push(crate::tr_lang(ru, "value_src.tip.request", &[("id", r)]));
    }
    if let Some(p) = &s.prompt {
        lines.push(crate::tr_lang(ru, "value_src.tip.prompt", &[("text", p)]));
    }
    lines.push(match s.verified_at {
        Some(v) => crate::tr_lang(ru, "value_src.tip.verified", &[("at", &crate::source_ui::date(v))]),
        None if s.kind.by_ai() => t("value_src.tip.unverified").to_string(),
        None => t("value_src.tip.not_reviewed").to_string(),
    });
    if s.needs_review() {
        lines.push(t("value_src.tip.click").to_string());
    }
    lines.join("\n")
}

/// The badge beside a value; `true` when the person asked to mark it verified (a click on an
/// unverified one, or the menu item).
pub fn badge(ui: &mut egui::Ui, s: &ValueSrc, ru: bool) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let color = if s.needs_review() { th.c("warning") } else { crate::ui_kit::weak(ui) };
    let text = th.rich(Role::Caption, crate::tr_str(ru, badge_key(s))).color(color);
    let r = ui
        .add(egui::Label::new(text).sense(egui::Sense::click()))
        .on_hover_text(tip(s, ru));
    let mut ask = s.needs_review() && r.clicked();
    if s.needs_review() {
        r.context_menu(|ui| {
            let label = crate::tr_str(ru, "value_src.verify");
            let tone = ddc_ui::widgets::MenuTone::Normal;
            if ddc_ui::widgets::menu_item(ui, &th, &label, "", tone, true).clicked() {
                ask = true;
                ui.close();
            }
        });
    }
    ask
}

/// Marks the unverified values among `cells` verified now: one undo step, the status says how
/// many. Nothing to mark writes nothing and says so.
pub fn verify(ed: &mut Editor, cells: &[Cell]) -> usize {
    verify_at(ed, cells, ddc_model::value_src::now_s())
}

pub fn verify_at(ed: &mut Editor, cells: &[Cell], at: i64) -> usize {
    let todo: Vec<Cell> = cells
        .iter()
        .filter(|c| {
            ed.model
                .value_src(&c.0, c.1, &c.2)
                .ok()
                .flatten()
                .is_some_and(|s| !s.verified())
        })
        .cloned()
        .collect();
    if todo.is_empty() {
        ed.status = Some(crate::tr("value_src.verify_none", &[]));
        return 0;
    }
    let n = todo.len();
    let f = move |m: &mut Model| {
        let it: Vec<(&str, i64, &str)> =
            todo.iter().map(|c| (c.0.as_str(), c.1, c.2.as_str())).collect();
        m.verify_values(&it, at).map(|_| ())
    };
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => {
            let count = n.to_string();
            ed.status = Some(crate::tr("value_src.verified_n", &[("n", &count)]));
            n
        }
        Err(e) => {
            ed.status = Some(e.to_string());
            0
        }
    }
}

/// The rows a selection's values live on: each element and, for type parameters, its type.
fn selected_rows(ed: &Editor) -> Vec<(String, i64)> {
    let mut rows: Vec<(String, i64)> = vec![];
    for s in &ed.selection {
        let Some((kind, id)) = crate::phase_ui::sel_ref(*s) else {
            continue;
        };
        let mut push = |r: (String, i64)| {
            if !rows.contains(&r) {
                rows.push(r);
            }
        };
        push((kind.to_string(), id));
        if let Ok(Some((tk, tid))) = ed.model.type_of(kind, id) {
            push((tk.to_string(), tid));
        }
    }
    rows
}

/// Unverified assistant values of the selection, in model order.
pub fn selection_unverified(ed: &Editor) -> Vec<Cell> {
    let rows = selected_rows(ed);
    if rows.is_empty() {
        return vec![];
    }
    ed.model
        .value_srcs_of(&rows)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, s)| s.needs_review())
        .map(|(k, _)| k)
        .collect()
}

/// A bar over the properties: "N values filled by AI are not verified" and one button that
/// verifies them all. Read once per cache key; absent when there is nothing to verify.
pub fn bulk_bar(ui: &mut egui::Ui, ed: &mut Editor, ru: bool) {
    let rk = ed.cache_key();
    let ck = egui::Id::new("ag2.verify.sel");
    let hit = ui.data(|d| d.get_temp::<((u64, u64), Vec<Cell>)>(ck)).filter(|c| c.0 == rk);
    let cells = match hit {
        Some(c) => c.1,
        None => {
            let c = selection_unverified(ed);
            ui.data_mut(|d| d.insert_temp(ck, (rk, c.clone())));
            c
        }
    };
    if cells.is_empty() {
        return;
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let n = cells.len().to_string();
    let mut go = false;
    ui.horizontal_wrapped(|ui| {
        let msg = crate::tr_lang(ru, "value_src.bulk_note", &[("n", &n)]);
        ui.label(th.rich(Role::Caption, &msg).color(th.c("warning")));
        let label = crate::tr_lang(ru, "value_src.bulk_verify", &[("n", &n)]);
        go = ddc_ui::widgets::Button::secondary(&label)
            .show(ui, &th)
            .on_hover_text(crate::tr_str(ru, "value_src.bulk_tip"))
            .clicked();
    });
    if go {
        verify(ed, &cells);
    }
}

/// `props.verify_ai`: every unverified assistant value of the selection, one undo step.
fn verify_cmd(a: &mut crate::shell::EditorApp) {
    verify_selection(&mut a.ed);
}

/// Marks the selection's assistant-set values as checked; with none, says so instead of
/// doing nothing silently. Returns how many were marked.
pub fn verify_selection(ed: &mut Editor) -> usize {
    let cells = selection_unverified(ed);
    if cells.is_empty() {
        ed.status = Some(crate::tr("sketch_hints.verify_none", &[]));
        return 0;
    }
    verify(ed, &cells)
}

pub fn register(app: &mut crate::shell::EditorApp) {
    app.register_command("props.verify_ai", verify_cmd);
}

/// Two walls of a test room whose "Fire" value the assistant set, unverified.
#[cfg(test)]
pub(crate) fn test_editor() -> (Editor, i64, i64) {
    tests::editor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{Edit, PVal, ParamSpec, ParamType};

    const T0: i64 = 1_700_000_000;

    #[test]
    fn verify_with_nothing_to_check_says_so() {
        let mut e = Editor::new_in_memory().unwrap();
        assert_eq!(verify_selection(&mut e), 0);
        let st = e.status.clone().unwrap_or_default();
        assert!(!st.is_empty() && !st.starts_with("sketch_hints."), "{st}");
    }

    pub(super) fn editor() -> (Editor, i64, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        e.model
            .add_param(&ParamSpec {
                name: "Fire".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        let (w1, w2) = (e.walls[0].id, e.walls[1].id);
        let set = |id: i64| Edit::ParamValue {
            param: "Fire".into(),
            kind: "wall".into(),
            id,
            value: Some(PVal::Text("EI60".into())),
        };
        e.model.apply_edits(&[set(w1), set(w2)]).unwrap();
        let cells = [("wall", w1, "Fire"), ("wall", w2, "Fire")];
        let src = ValueSrc::ai(SrcKind::Ai, T0, "s", "s#1", "fill fire ratings");
        e.model.record_value_src(&cells, &src).unwrap();
        e.reload().unwrap();
        (e, w1, w2)
    }

    fn cell(id: i64) -> Cell {
        ("wall".into(), id, "Fire".into())
    }

    #[test]
    fn verify_is_one_undo_step_and_says_how_many() {
        let (mut e, w1, w2) = editor();
        let pushes = e.pushes;
        assert_eq!(verify_at(&mut e, &[cell(w1), cell(w2)], T0 + 60), 2);
        assert_eq!(e.pushes, pushes + 1, "one verify, one undo step");
        assert!(e.model.unverified_ai().unwrap().is_empty());
        assert!(e.status.as_deref().unwrap_or("").contains('2'));
        e.undo();
        assert_eq!(e.model.unverified_ai().unwrap().len(), 2, "undo takes both back");
        e.redo();
        assert!(e.model.unverified_ai().unwrap().is_empty());
        // nothing left to verify: no step, a note instead
        let pushes = e.pushes;
        assert_eq!(verify_at(&mut e, &[cell(w1)], T0 + 90), 0);
        assert_eq!(e.pushes, pushes);
    }

    #[test]
    fn bulk_takes_the_unverified_values_of_the_selection() {
        let (mut e, w1, _) = editor();
        assert!(selection_unverified(&e).is_empty(), "nothing selected");
        e.selection = vec![crate::editor::Sel::Wall(w1)];
        assert_eq!(selection_unverified(&e), vec![cell(w1)]);
        let pushes = e.pushes;
        let sel = selection_unverified(&e);
        assert_eq!(verify_at(&mut e, &sel, T0 + 60), 1);
        assert_eq!(e.pushes, pushes + 1);
        assert_eq!(e.model.unverified_ai().unwrap().len(), 1, "the other wall stays");
    }

    #[test]
    fn the_tooltip_names_the_source_in_both_languages() {
        let s = ValueSrc::ai(SrcKind::Ai, T0, "s", "s#1", "fill fire ratings");
        for ru in [false, true] {
            let t = tip(&s, ru);
            assert!(t.contains("s#1") && t.contains("fill fire ratings"), "{t}");
            assert!(t.contains("2023"), "the date: {t}");
            assert!(!t.contains("value_src."), "every key is in the catalog: {t}");
        }
        assert_ne!(tip(&s, false), tip(&s, true));
        let ok = ValueSrc {
            verified_at: Some(T0),
            ..s
        };
        assert!(!ok.needs_review());
        assert!(!tip(&ok, false).contains(crate::tr_str(false, "value_src.tip.click")));
        for k in SrcKind::ALL {
            assert_ne!(crate::tr_str(false, kind_key(k)), kind_key(k));
            assert_ne!(crate::tr_str(true, kind_key(k)), kind_key(k));
        }
    }
}
