// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `data.classify` ("Classifications"): assigns a classification code to the selected elements
//! or to every element of their categories. A system is a text project parameter named
//! `Classification.<system>` (the name the IFC import gives classification references), bound
//! to the categories it is used on. Assign and clear are one undo step each.

use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_model::{PVal, ParamSet, ParamSpec, ParamType, PARAM_CATEGORIES};
use ddc_ui::theme::space;
use ddc_ui::widgets::{self, TextField};
use ddc_ui::Catalog;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Parameter name prefix of a classification system.
pub const PREFIX: &str = "Classification.";
/// Longest system name: the parameter name holds 120 characters.
const SYSTEM_MAX: usize = 120 - PREFIX.len();

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Scope {
    #[default]
    Selection,
    /// Every element of the categories in the selection.
    Categories,
}

#[derive(Default)]
pub struct Win {
    pub open: bool,
    pub system: String,
    pub code: String,
    pub scope: Scope,
    pub note: Option<String>,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// An element a code can go to: (table, id, category).
pub type Target = (&'static str, i64, &'static str);

/// Systems in the project: classification parameters, without the prefix, sorted.
pub fn systems(ed: &Editor) -> Vec<String> {
    let mut v: Vec<String> = ed
        .model
        .params()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| d.spec.name.strip_prefix(PREFIX).map(str::to_string))
        .filter(|s| !s.is_empty())
        .collect();
    v.sort();
    v
}

/// Elements of `scope`, each once, in a stable order.
pub fn targets(ed: &Editor, scope: Scope) -> Vec<Target> {
    let mut picked: Vec<Target> = vec![];
    for s in &ed.selection {
        let Some((kind, id)) = crate::phase_ui::sel_ref(*s) else {
            continue;
        };
        if let Ok(Some(cat)) = ed.model.category_of(kind, id) {
            let table = PARAM_CATEGORIES.iter().find(|c| c.0 == cat).map_or(kind, |c| c.1);
            picked.push((table, id, cat));
        }
    }
    if scope == Scope::Categories {
        let cats: Vec<&'static str> = picked.iter().map(|t| t.2).collect();
        picked.clear();
        for &(cat, table) in PARAM_CATEGORIES.iter().filter(|c| cats.contains(&c.0)) {
            for id in ids_of(ed, table) {
                if ed.model.category_of(table, id).ok().flatten() == Some(cat) {
                    picked.push((table, id, cat));
                }
            }
        }
    }
    picked.sort();
    picked.dedup();
    picked
}

fn ids_of(ed: &Editor, table: &str) -> Vec<i64> {
    // `table` comes from the fixed category list, never from text a person typed
    let sql = format!("SELECT id FROM {table} ORDER BY id");
    let Ok(mut st) = ed.model.sql().prepare(&sql) else {
        return vec![];
    };
    st.query_map([], |r| r.get(0))
        .map(|r| r.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Codes of `system` on `ts`: (code, count) by count, then the count without a code.
pub fn codes(ed: &Editor, system: &str, ts: &[Target]) -> (Vec<(String, usize)>, usize) {
    let def = ed.model.param_by_name(&format!("{PREFIX}{}", system.trim())).ok().flatten();
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    let mut none = 0;
    for &(table, id, _) in ts {
        let v = def.as_ref().and_then(|d| ed.model.param_value_of(d, table, id).ok().flatten());
        match v.map(|v| v.text()).filter(|t| !t.trim().is_empty()) {
            Some(t) => *by.entry(t).or_default() += 1,
            None => none += 1,
        }
    }
    let mut v: Vec<(String, usize)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    (v, none)
}

/// Why `system` and `code` cannot be written (an i18n key), None when they can.
pub fn refusal(system: &str, code: Option<&str>, ts: &[Target]) -> Option<&'static str> {
    let s = system.trim();
    if ts.is_empty() {
        Some("cls.err.nothing")
    } else if s.is_empty() {
        Some("cls.err.no_system")
    } else if s.chars().count() > SYSTEM_MAX {
        Some("cls.err.long_system")
    } else if code.is_some_and(|c| c.trim().is_empty()) {
        Some("cls.err.no_code")
    } else {
        None
    }
}

/// Writes `code` (None clears it) of `system` on every target as one undo step. Returns how
/// many elements now carry the code. Creates the system, or binds it to new categories.
pub fn assign(
    ed: &mut Editor,
    system: &str,
    code: Option<&str>,
    ts: &[Target],
) -> Result<usize, String> {
    if let Some(k) = refusal(system, code, ts) {
        return Err(k.to_string());
    }
    let name = format!("{PREFIX}{}", system.trim());
    let code = code.map(|c| c.trim().to_string());
    let ts: Vec<Target> = ts.to_vec();
    let mut cats: Vec<String> = ts.iter().map(|t| t.2.to_string()).collect();
    cats.sort();
    cats.dedup();
    if code.is_none() && ed.model.param_by_name(&name).ok().flatten().is_none() {
        return Ok(0);
    }
    let n = ts.len();
    let write = code.is_some();
    ed.run_op(OpFn(Arc::new(move |m| {
        let id = match m.param_by_name(&name)? {
            Some(d) => {
                if d.spec.ptype != ParamType::Text || d.spec.per_type {
                    return Err(ddc_model::Error::Invalid("cls.err.not_text".into()));
                }
                let missing: Vec<String> =
                    cats.iter().filter(|c| !d.spec.categories.contains(c)).cloned().collect();
                if write && !missing.is_empty() {
                    let mut spec = d.spec.clone();
                    spec.categories.extend(missing);
                    m.edit_param(d.id, &spec)?;
                }
                d.id
            }
            None => m.add_param(&ParamSpec {
                name: name.clone(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: "Classification".into(),
                guid: None,
                categories: cats.clone(),
            })?,
        };
        let bound = m.params()?.into_iter().find(|d| d.id == id).map(|d| d.spec.categories);
        let bound = bound.unwrap_or_default();
        let sets: Vec<ParamSet> = ts
            .iter()
            .filter(|t| bound.iter().any(|c| c == t.2))
            .map(|&(kind, id_el, _)| ParamSet {
                param: id,
                kind: kind.to_string(),
                id: id_el,
                value: code.clone().map(PVal::Text),
            })
            .collect();
        m.set_param_values(&sets)
    })))
    .map_err(|e| match e {
        ddc_model::Error::Invalid(k) if k.starts_with("cls.") => k,
        e => e.to_string(),
    })?;
    Ok(if write { n } else { 0 })
}

/// Plural label of a parameter category ("Walls").
pub fn category_label(cat: &Catalog, name: &str) -> String {
    match name {
        "ramp" | "annotation" => cat.t(&format!("find_replace.cat.{name}")),
        _ => cat.t(&format!("view_gfx.cat.{name}")),
    }
}

/// "3 walls, 2 doors": the targets by category.
pub fn summary(cat: &Catalog, ts: &[Target]) -> String {
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for t in ts {
        *by.entry(t.2).or_default() += 1;
    }
    by.into_iter()
        .map(|(c, n)| format!("{} {n}", category_label(cat, c)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn text_row(ui: &mut egui::Ui, th: &ddc_ui::Theme, label: &str, salt: &str, buf: &mut String) {
    kit::labeled_row_id(ui, label, |ui, lid| {
        TextField::new(buf).labelled_by(lid, label).id_salt(salt).show(ui, th);
    });
}

/// Window body (apart from the frame, for the layout probes). `true`: close.
pub fn body(ui: &mut egui::Ui, w: &mut Win, ed: &mut Editor, cat: &Catalog) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    ui.add(egui::Label::new(cat.t("cls.lead")).wrap());
    ui.add_space(space::XS);
    let items = [cat.t("cls.scope.sel"), cat.t("cls.scope.cats")];
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    let at = usize::from(w.scope == Scope::Categories);
    if let Some(i) = widgets::segmented(ui, &th, &items, at) {
        w.scope = if i == 1 { Scope::Categories } else { Scope::Selection };
    }
    let ts = targets(ed, w.scope);
    if ts.is_empty() {
        kit::empty_note(ui, cat.t("cls.empty"), Some(&cat.t("cls.empty_hint")));
    } else {
        let n = ts.len().to_string();
        let v = cat.tf("cls.count", &[("n", &n), ("what", &summary(cat, &ts))]);
        kit::props(ui, "cls.elements", |ui| kit::kv(ui, &cat.t("cls.elements"), v));
    }
    ui.add_space(space::SM);
    text_row(ui, &th, &cat.t("cls.system"), "cls.system", &mut w.system);
    let known = systems(ed);
    if !known.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for s in &known {
                if widgets::Button::quiet(s).small().show(ui, &th).clicked() {
                    w.system = s.clone();
                }
            }
        });
    }
    text_row(ui, &th, &cat.t("cls.code"), "cls.code", &mut w.code);
    ui.add_space(space::SM);
    if !ts.is_empty() && !w.system.trim().is_empty() {
        kit::section_title(ui, cat.t("cls.now"));
        let (list, none) = codes(ed, &w.system, &ts);
        kit::scroll_list(ui, "cls.codes", 6, th.d().control, |ui| {
            kit::props(ui, "cls.codes.grid", |ui| {
                for (c, n) in &list {
                    kit::kv(ui, c, n.to_string());
                }
                if none > 0 {
                    kit::kv(ui, &cat.t("cls.no_code"), none.to_string());
                }
            });
        });
    }
    if let Some(n) = &w.note {
        ui.add(egui::Label::new(n.as_str()).wrap());
    }
    let block = refusal(&w.system, Some(&w.code), &ts).map(|k| cat.t(k));
    let clear_block = refusal(&w.system, None, &ts).is_some();
    let (mut go, mut clear, mut close) = (false, false, false);
    kit::dialog_footer(ui, |ui| {
        go = kit::primary_action(ui, cat.t("cls.assign"), block.as_deref());
        let clear_why = refusal(&w.system, None, &ts).map(|k| cat.t(k));
        clear = ui
            .add_enabled_ui(!clear_block, |ui| kit::secondary_button(ui, cat.t("cls.clear")))
            .inner
            .on_disabled_hover_text(clear_why.unwrap_or_default())
            .clicked();
        close = kit::cancel_button(ui, cat.t("dlg.close")).clicked();
    });
    if go || clear {
        let code = go.then_some(w.code.as_str());
        w.note = Some(match assign(ed, &w.system, code, &ts) {
            Ok(n) if go => cat.tf("cls.done", &[("n", &n.to_string()), ("code", w.code.trim())]),
            Ok(_) => cat.t("cls.cleared"),
            Err(k) if k.starts_with("cls.") => cat.t(&k),
            Err(e) => cat.tf("cls.err.other", &[("e", &e)]),
        });
    }
    close
}

fn run(_: &mut EditorApp) {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.note = None;
    });
}

pub fn open_for_layout(system: &str, code: &str, scope: Scope) {
    WIN.with(|w| {
        *w.borrow_mut() = Win {
            open: true,
            system: system.into(),
            code: code.into(),
            scope,
            note: None,
        }
    });
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !w.open {
        WIN.with(|s| *s.borrow_mut() = w);
        return;
    }
    let (mut open, mut close) = (true, false);
    kit::Dialog::new("classify", a.sh.cat.t("cmd.data.classify"), kit::DialogSize::Form)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| close = body(ui, &mut w, &mut a.ed, &a.sh.cat));
    w.open = open && !close;
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("data.classify", run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Sel;
    use ddc_ui::Lang;

    fn room_editor() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut ed);
        ed
    }

    #[test]
    fn assign_creates_the_system_and_is_one_undo_step() {
        let mut ed = room_editor();
        let w0 = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(w0)];
        let ts = targets(&ed, Scope::Selection);
        assert_eq!(ts, vec![("wall", w0, "wall")]);
        assert_eq!(assign(&mut ed, "Building", Some(" 21.10.20 "), &ts).unwrap(), 1);
        assert_eq!(systems(&ed), vec!["Building".to_string()]);
        assert_eq!(codes(&ed, "Building", &ts).0, vec![("21.10.20".to_string(), 1)]);
        ed.undo();
        assert!(systems(&ed).is_empty(), "the parameter and the value go in one step");
    }

    #[test]
    fn categories_scope_takes_every_element_and_binds_new_categories() {
        let mut ed = room_editor();
        let walls = ed.walls.len();
        assert!(walls >= 4);
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let all = targets(&ed, Scope::Categories);
        assert_eq!(all.len(), walls);
        assert_eq!(assign(&mut ed, "Building", Some("21.10"), &all).unwrap(), walls);
        let (c, none) = codes(&ed, "Building", &all);
        assert_eq!((c, none), (vec![("21.10".to_string(), walls)], 0));
        let level = ed.level;
        let spec = ddc_model::RoomSpec {
            level,
            name: "R".into(),
            number: "1".into(),
            point: (3.0, 2.0),
            height: 2.7,
        };
        let room = ed.model.add_room(&spec).unwrap();
        ed.reload().unwrap();
        ed.selection = vec![Sel::Room(room)];
        let ts = targets(&ed, Scope::Selection);
        assert_eq!(assign(&mut ed, "Building", Some("32.40"), &ts).unwrap(), 1);
        let d = ed.model.param_by_name("Classification.Building").unwrap().unwrap();
        assert!(d.spec.categories.contains(&"room".to_string()));
        assert!(d.spec.categories.contains(&"wall".to_string()));
    }

    #[test]
    fn clear_removes_the_code_and_refusals_name_the_field() {
        let mut ed = room_editor();
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let ts = targets(&ed, Scope::Selection);
        assert_eq!(assign(&mut ed, "S", None, &ts).unwrap(), 0, "nothing to clear yet");
        assign(&mut ed, "S", Some("A1"), &ts).unwrap();
        assign(&mut ed, "S", None, &ts).unwrap();
        assert_eq!(codes(&ed, "S", &ts), (vec![], 1));
        assert_eq!(assign(&mut ed, " ", Some("A"), &ts).unwrap_err(), "cls.err.no_system");
        assert_eq!(assign(&mut ed, "S", Some(" "), &ts).unwrap_err(), "cls.err.no_code");
        assert_eq!(assign(&mut ed, "S", Some("A"), &[]).unwrap_err(), "cls.err.nothing");
        let long = "x".repeat(SYSTEM_MAX + 1);
        assert_eq!(assign(&mut ed, &long, Some("A"), &ts).unwrap_err(), "cls.err.long_system");
    }

    #[test]
    fn a_number_parameter_with_the_name_is_not_overwritten() {
        let mut ed = room_editor();
        ed.model
            .add_param(&ParamSpec {
                name: "Classification.N".into(),
                ptype: ParamType::Number,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let ts = targets(&ed, Scope::Selection);
        let e = assign(&mut ed, "N", Some("A"), &ts).unwrap_err();
        assert!(e.contains("cls.err.not_text"), "{e}");
    }

    #[test]
    fn every_text_is_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "cls.lead", "cls.scope.sel", "cls.scope.cats", "cls.empty", "cls.empty_hint",
                "cls.elements", "cls.count", "cls.system", "cls.code", "cls.now", "cls.no_code",
                "cls.assign", "cls.clear", "cls.done", "cls.cleared", "cls.err.nothing",
                "cls.err.no_system", "cls.err.long_system", "cls.err.no_code",
                "cls.err.not_text", "cls.err.other", "view_gfx.cat.wall", "dlg.close",
            ] {
                assert_ne!(cat.t(k), k, "{lang:?} {k}");
            }
        }
    }
}
