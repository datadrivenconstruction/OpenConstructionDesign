//! Window "Project schedules": define a multi-category schedule, see its rows, stage cell edits
//! as a draft, read the preview, then Apply (one undo step) or Discard. Export and import go
//! through XLSX; an imported book becomes a draft too.

use super::multi::{self, ColSpec, Draft, Multi, MultiDef};
use super::ui::preset::Laid;
use super::{fmt, header, Cat, Val};
use crate::editor::Editor;
use crate::ui_kit as kit;
use crate::units::Units;
use ddc_ui::theme::space;
use ddc_ui::widgets::{self, Button, Cell, Column, RowState, Table, TextField};
use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct WinState {
    open: bool,
    loaded: bool,
    defs: Vec<MultiDef>,
    sel: usize,
    /// Form buffers of the selected definition.
    form: Option<Form>,
    /// Selected row by element (category, id, panel), never by position.
    row: Option<(Cat, i64, u32)>,
    edit_col: usize,
    edit_text: String,
    draft: Draft,
    /// Counts draft changes; keys the preview cache.
    draft_rev: u64,
    /// Journal head the definitions were read at.
    top: Option<i64>,
    gen: u64,
    cache: Option<Rc<Cache>>,
    pv: Option<((u64, u64), Rc<multi::Preview>)>,
    msg: Option<String>,
}

/// The schedule laid out over the model; rebuilt only when the definition, the language, the
/// units or the model change.
struct Cache {
    key: (Option<i64>, MultiDef, bool, Units),
    gen: u64,
    m: Multi,
    laid: Laid,
}

#[derive(Clone, PartialEq)]
struct Form {
    name: String,
    cats: Vec<bool>,
    columns: String,
    group: String,
    sort: String,
    filters: String,
    totals: bool,
}

thread_local! {
    static WIN: RefCell<WinState> = RefCell::new(WinState::default());
}

pub fn open_window() {
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.open = true;
        w.loaded = false;
    });
}

fn form_of(d: &MultiDef) -> Form {
    Form {
        name: d.name.clone(),
        cats: Cat::ALL.iter().map(|c| d.cats.contains(c)).collect(),
        columns: multi::columns_text(&d.columns),
        group: d.group.join(", "),
        sort: multi::sort_text(&d.sort),
        filters: multi::filters_text(&d.filters),
        totals: d.totals,
    }
}

fn def_of(f: &Form) -> MultiDef {
    MultiDef {
        name: f.name.trim().to_string(),
        cats: Cat::ALL
            .iter()
            .zip(&f.cats)
            .filter(|(_, on)| **on)
            .map(|(c, _)| *c)
            .collect(),
        columns: multi::parse_columns(&f.columns),
        filters: multi::parse_filters(&f.filters),
        sort: multi::parse_sort(&f.sort),
        group: multi::parse_keys(&f.group),
        totals: f.totals,
    }
}

/// Window; called every frame.
pub fn window(ctx: &egui::Context, ed: &mut Editor, ru: bool) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !st.open {
        WIN.with(|w| *w.borrow_mut() = st);
        return;
    }
    let mut open = true;
    kit::Dialog::new(
        "schedules_multi",
        crate::tr_str(ru, "schm.ui.title"),
        kit::DialogSize::Wide,
    )
    .open(&mut open)
    .scroll_body()
    .show(ctx, |ui| body(ui, &mut st, ed, ru));
    st.open = open;
    WIN.with(|w| *w.borrow_mut() = st);
}

/// Reads the stored definitions again; a form with unsaved edits keeps them.
fn refresh(st: &mut WinState, ed: &Editor) {
    let dirty = st.form.is_some() && st.form != st.defs.get(st.sel).map(form_of);
    st.defs = multi::load(ed);
    st.sel = st.sel.min(st.defs.len().saturating_sub(1));
    if !dirty || st.defs.is_empty() {
        st.form = st.defs.get(st.sel).map(form_of);
    }
    st.cache = None;
    st.pv = None;
}

/// An undo, a redo or any other model change may have rewritten the definitions: read them
/// again whenever the journal head moved, so a save never writes over what an undo restored.
fn sync(st: &mut WinState, ed: &Editor) {
    let top = ed.model.op_top().ok();
    if !st.loaded || st.top != top {
        refresh(st, ed);
        st.top = top;
        st.loaded = true;
    }
}

impl WinState {
    /// The draft changed: the preview is asked for again.
    fn touch(&mut self) {
        self.draft_rev += 1;
    }
}

fn body(ui: &mut egui::Ui, st: &mut WinState, ed: &mut Editor, ru: bool) {
    let th = ddc_ui::theme::of(ui.ctx());
    let tr = |k: &'static str| crate::tr_str(ru, k);
    sync(st, ed);
    let mut pick = None;
    let mut new = false;
    let mut del = false;
    kit::labeled_row(ui, tr("schm.ui.schedule"), |ui| {
        if !st.defs.is_empty() {
            let names: Vec<String> = st.defs.iter().map(|d| d.name.clone()).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut i = st.sel;
            if widgets::dropdown(ui, &th, "ddc.schm.pick", &mut i, &refs, None).changed() {
                pick = Some(i);
            }
        }
        new = Button::secondary(tr("schm.ui.new")).show(ui, &th).clicked();
        if !st.defs.is_empty() {
            del = Button::quiet(tr("schm.ui.delete")).show(ui, &th).clicked();
        }
    });
    if new {
        let n = (st.defs.len() + 1).to_string();
        st.defs.push(MultiDef {
            name: crate::tr_lang(ru, "schm.ui.default_name", &[("n", &n)]),
            cats: vec![Cat::Walls],
            columns: vec![ColSpec::Param("id".into()), ColSpec::Param("len".into())],
            ..Default::default()
        });
        st.sel = st.defs.len() - 1;
        st.form = Some(form_of(&st.defs[st.sel]));
        st.draft = Draft::default();
        st.touch();
        st.msg = save_defs(ed, st);
    } else if del && st.sel < st.defs.len() {
        st.defs.remove(st.sel);
        st.sel = st.sel.saturating_sub(1);
        st.form = st.defs.get(st.sel).map(form_of);
        st.draft = Draft::default();
        st.touch();
        st.msg = save_defs(ed, st);
    } else if let Some(i) = pick {
        st.sel = i;
        st.form = st.defs.get(i).map(form_of);
        st.draft = Draft::default();
        st.touch();
        st.row = None;
    }
    let Some(mut form) = st.form.clone() else {
        kit::empty_note(ui, tr("schm.ui.none"), Some(tr("schm.ui.none_hint")));
        return;
    };
    definition_form(ui, &th, &mut form, ru);
    let dirty = st.defs.get(st.sel).map(form_of).as_ref() != Some(&form);
    st.form = Some(form.clone());
    ui.horizontal(|ui| {
        if kit::primary_action(
            ui,
            tr("schm.ui.save"),
            (!dirty).then_some(tr("schm.ui.saved")),
        ) {
            st.defs[st.sel] = def_of(&form);
            st.msg = save_defs(ed, st);
            st.draft = Draft::default();
            st.touch();
        }
    });
    ui.separator();
    let def = st.defs[st.sel].clone();
    let u = ed.tool.units;
    let cache = layout_of(st, ed, &def, ru);
    for n in &cache.m.notes {
        ui.label(egui::RichText::new(n).color(th.sem().error));
    }
    grid(ui, &th, st, &cache, u, ru);
    stage_row(ui, &th, st, &cache.m, ru);
    draft_panel(ui, &th, st, ed, &cache, ru);
    if let Some(msg) = &st.msg {
        kit::hint(ui, msg.clone());
    }
}

/// The schedule of `def` laid out over the model, rebuilt only when it, the language, the
/// units or the model changed since the last frame.
fn layout_of(st: &mut WinState, ed: &Editor, def: &MultiDef, ru: bool) -> Rc<Cache> {
    let u = ed.tool.units;
    let key = (ed.model.op_top().ok(), def.clone(), ru, u);
    if let Some(c) = &st.cache {
        if c.key == key {
            return c.clone();
        }
    }
    let m = multi::build(ed, def, ru);
    let laid = multi::lay(&m, u);
    st.gen += 1;
    let c = Rc::new(Cache {
        key,
        gen: st.gen,
        m,
        laid,
    });
    st.cache = Some(c.clone());
    c
}

/// What Apply would change; asked again only when the draft or the schedule changed.
fn preview_of(st: &mut WinState, ed: &Editor, c: &Rc<Cache>) -> Rc<multi::Preview> {
    let key = (st.draft_rev, c.gen);
    if let Some((k, p)) = &st.pv {
        if *k == key {
            return p.clone();
        }
    }
    let p = Rc::new(multi::preview(ed, &c.m, &st.draft, c.key.3));
    st.pv = Some((key, p.clone()));
    p
}

/// Stores the definitions as one undo step; the error text on failure.
fn save_defs(ed: &mut Editor, st: &WinState) -> Option<String> {
    multi::save(ed, &st.defs).err()
}

fn definition_form(ui: &mut egui::Ui, th: &ddc_ui::Theme, f: &mut Form, ru: bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    kit::labeled_row_id(ui, tr("schm.ui.name"), |ui, lid| {
        TextField::new(&mut f.name)
            .id_salt("ddc.schm.name")
            .labelled_by(lid, tr("schm.ui.name"))
            .show(ui, th);
    });
    ui.label(egui::RichText::new(tr("schm.ui.cats")).strong());
    ui.horizontal_wrapped(|ui| {
        for (c, on) in Cat::ALL.iter().zip(f.cats.iter_mut()) {
            widgets::checkbox(ui, th, on, &c.title_in(ru));
        }
    });
    kit::labeled_row_id(ui, tr("schm.ui.columns"), |ui, lid| {
        TextField::new(&mut f.columns)
            .id_salt("ddc.schm.cols")
            .labelled_by(lid, tr("schm.ui.columns"))
            .show(ui, th);
    });
    kit::hint(ui, tr("schm.ui.columns_hint"));
    kit::labeled_row_id(ui, tr("schm.ui.filters"), |ui, lid| {
        TextField::new(&mut f.filters)
            .id_salt("ddc.schm.filters")
            .labelled_by(lid, tr("schm.ui.filters"))
            .show(ui, th);
    });
    kit::hint(ui, tr("schm.ui.filters_hint"));
    kit::labeled_row_id(ui, tr("schm.ui.group"), |ui, lid| {
        TextField::new(&mut f.group)
            .id_salt("ddc.schm.group")
            .labelled_by(lid, tr("schm.ui.group"))
            .show(ui, th);
    });
    kit::labeled_row_id(ui, tr("schm.ui.sort"), |ui, lid| {
        TextField::new(&mut f.sort)
            .id_salt("ddc.schm.sort")
            .labelled_by(lid, tr("schm.ui.sort"))
            .show(ui, th);
    });
    widgets::checkbox(ui, th, &mut f.totals, tr("schm.ui.totals"));
}

const GRID_ROWS: f32 = 10.0;

fn grid(ui: &mut egui::Ui, th: &ddc_ui::Theme, st: &mut WinState, c: &Cache, u: Units, ru: bool) {
    let (m, laid) = (&c.m, &c.laid);
    if laid.ord.is_empty() {
        kit::empty_note(ui, crate::tr_str(ru, "schm.ui.no_rows"), None);
        return;
    }
    let titles: Vec<String> = laid
        .cols
        .iter()
        .map(|&j| header(&m.table.cols[j], u))
        .collect();
    let cols: Vec<Column> = titles
        .iter()
        .map(|t| Column::text(t).sortable(false))
        .collect();
    let cur = st.row;
    let ident = |ti: usize| {
        let (p, i) = m.origin[ti];
        let r = &m.parts[p].rows[i];
        (m.parts[p].cat, r.id, r.sub)
    };
    let mut sort = None;
    let out = Table::new("ddc.schm.grid", &cols, laid.ord.len())
        .max_height(th.d().row * GRID_ROWS)
        .show(
            ui,
            th,
            &mut sort,
            |r| {
                if Some(ident(laid.ord[r] as usize)) == cur {
                    RowState::Selected
                } else {
                    RowState::Normal
                }
            },
            |r, c| {
                let ti = laid.ord[r] as usize;
                let j = laid.cols[c];
                let (cat, id, sub) = ident(ti);
                let (p, _) = m.origin[ti];
                let staged = m.colmap[p][j]
                    .and_then(|pj| st.draft.text(cat, id, sub, &m.parts[p].cols[pj].key));
                match (staged, &m.table.rows[ti].vals[j]) {
                    (Some(s), _) => Cell::text(s.to_string()).with_tone(th.sem().accent),
                    (None, v @ Val::N(_)) => Cell::text(fmt(v, m.table.cols[j].kind, u)),
                    (None, v) => Cell::muted(fmt(v, m.table.cols[j].kind, u)),
                }
            },
        );
    if let Some(r) = out.clicked.or(out.double_clicked) {
        st.row = Some(ident(laid.ord[r] as usize));
    }
}

/// Edit one cell of the selected row: column, new text, Stage.
fn stage_row(ui: &mut egui::Ui, th: &ddc_ui::Theme, st: &mut WinState, m: &Multi, ru: bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    // the selected element may be gone after a delete or an undo
    let Some((cat, id, sub)) = st.row else { return };
    let found = m.parts.iter().position(|t| t.cat == cat).and_then(|p| {
        let i = *m.index[p].get(&(id, sub))?;
        let ti = m.origin.iter().position(|o| *o == (p, i))?;
        Some((p, ti))
    });
    let Some((p, row)) = found else {
        st.row = None;
        return;
    };
    let editable: Vec<usize> = (0..m.table.cols.len())
        .filter(|&j| !multi::is_computed(m, j) && m.colmap[p][j].is_some())
        .collect();
    if editable.is_empty() {
        return;
    }
    let names: Vec<String> = editable
        .iter()
        .map(|&j| m.table.cols[j].title.clone())
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    st.edit_col = st.edit_col.min(editable.len() - 1);
    let mut stage = false;
    kit::labeled_row_id(ui, tr("schm.ui.edit_cell"), |ui, lid| {
        widgets::dropdown(ui, th, "ddc.schm.editcol", &mut st.edit_col, &refs, None);
        TextField::new(&mut st.edit_text)
            .id_salt("ddc.schm.edittext")
            .labelled_by(lid, tr("schm.ui.edit_cell"))
            .show(ui, th);
        stage = Button::secondary(tr("schm.ui.stage"))
            .show(ui, th)
            .clicked();
    });
    if stage {
        let cell = (row, editable[st.edit_col], st.edit_text.clone());
        st.draft.merge(multi::draft_cells(m, &[cell]));
        st.touch();
    }
    ui.add_space(space::XS);
}

fn draft_panel(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    st: &mut WinState,
    ed: &mut Editor,
    c: &Rc<Cache>,
    ru: bool,
) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let (m, u) = (&c.m, c.key.3);
    let pv = preview_of(st, ed, c);
    ui.separator();
    kit::section_title(ui, tr("schm.ui.draft"));
    if st.draft.is_empty() && st.draft.skipped.is_empty() {
        kit::hint(ui, tr("schm.ui.draft_empty"));
    }
    for c in &pv.changes {
        let line = crate::tr_lang(
            ru,
            "schm.ui.change",
            &[
                ("cat", &c.cat.title_in(ru)),
                ("id", &c.id.to_string()),
                ("col", &c.column),
                ("before", &c.before),
                ("after", &c.after),
            ],
        );
        kit::elided_weak(ui, &line);
    }
    for r in &pv.refused {
        ui.label(egui::RichText::new(r).color(th.sem().error));
    }
    let (mut apply, mut discard, mut export, mut import) = (false, false, false, false);
    ui.horizontal(|ui| {
        let blocked = pv.changes.is_empty().then_some(tr("schm.ui.nothing"));
        apply = kit::primary_action(ui, tr("schm.ui.apply"), blocked);
        discard = Button::quiet(tr("schm.ui.discard")).show(ui, th).clicked();
        export = Button::secondary(tr("schm.ui.export"))
            .show(ui, th)
            .clicked();
        import = Button::secondary(tr("schm.ui.import"))
            .show(ui, th)
            .clicked();
    });
    if apply {
        let o = multi::apply(ed, m, &st.draft);
        st.msg = Some(crate::tr_lang(
            ru,
            "schm.ui.applied",
            &[("n", &o.done.to_string())],
        ));
        st.draft = Draft::default();
        st.touch();
    }
    if discard {
        st.draft = Draft::default();
        st.touch();
    }
    if export {
        let name = format!("{}.xlsx", multi::sheet_name(&m.def));
        if let Some(path) = super::ui::save_dialog(&name) {
            st.msg = Some(match multi::export_xlsx(&ed.model, m, &c.laid, u, &path) {
                Ok(()) => crate::tr_lang(
                    ru,
                    "schm.ui.exported",
                    &[("p", &path.display().to_string())],
                ),
                Err(e) => e,
            });
        }
    }
    if import {
        if let Some(path) = super::ui::open_dialog("") {
            st.msg = match multi::import_file(ed, m, u, &path) {
                Ok(imp) => {
                    let n = imp.preview.changes.len();
                    st.draft = imp.draft;
                    st.touch();
                    Some(crate::tr_lang(
                        ru,
                        "schm.ui.imported",
                        &[("n", &n.to_string())],
                    ))
                }
                Err(e) => Some(e),
            };
        }
    }
}

/// Unsaved items of the open window for the project question: staged cells plus the definition
/// form when it differs from the stored one. Memory only; asked every frame.
pub(crate) fn draft_count() -> usize {
    WIN.with(|w| {
        let Ok(st) = w.try_borrow() else { return 0 };
        let form = st.form.is_some() && st.form != st.defs.get(st.sel).map(form_of);
        st.draft.len() + usize::from(form)
    })
}

/// Discard on the project question: staged cells go and the form returns to the stored values.
pub(crate) fn discard_drafts() {
    WIN.with(|w| {
        let Ok(mut st) = w.try_borrow_mut() else { return };
        st.draft = Draft::default();
        st.form = st.defs.get(st.sel).map(form_of);
        st.touch();
    });
}

/// Save on the project question: staged cells go in as one undo step, then the definition form
/// is stored. False when something was refused; the draft stays as it was.
pub(crate) fn save_drafts(ed: &mut Editor) -> bool {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    let ok = commit_drafts(&mut st, ed);
    WIN.with(|w| *w.borrow_mut() = st);
    ok
}

/// Cells first: they were staged against the stored definition, not the edited form.
fn commit_drafts(st: &mut WinState, ed: &mut Editor) -> bool {
    if !st.draft.is_empty() {
        let Some(def) = st.defs.get(st.sel).cloned() else {
            return true;
        };
        let m = multi::build(ed, &def, false);
        let o = multi::apply(ed, &m, &st.draft);
        if o.bad > 0 {
            return false;
        }
        st.draft = Draft::default();
        st.touch();
        st.cache = None;
    }
    let edited = st.form.as_ref().filter(|f| st.defs.get(st.sel).map(form_of).as_ref() != Some(*f));
    if let Some(f) = edited.cloned() {
        st.defs[st.sel] = def_of(&f);
        st.msg = save_defs(ed, st);
        return st.msg.is_none();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::OpeningSpec;

    fn editor() -> Editor {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        e
    }

    fn def(name: &str) -> MultiDef {
        MultiDef {
            name: name.into(),
            cats: vec![Cat::Walls],
            columns: vec![ColSpec::Param("h".into())],
            ..Default::default()
        }
    }

    /// An undo that restores the definitions is seen by the open window, so a save cannot
    /// write the old list over it.
    #[test]
    fn definitions_are_read_again_after_an_undo() {
        let mut e = editor();
        let mut st = WinState::default();
        multi::save(&mut e, &[def("A")]).unwrap();
        sync(&mut st, &e);
        assert_eq!(st.defs.len(), 1);
        st.defs.push(def("B"));
        assert!(save_defs(&mut e, &st).is_none());
        sync(&mut st, &e);
        assert_eq!(st.defs.len(), 2);
        e.undo();
        sync(&mut st, &e);
        assert_eq!(st.defs.len(), 1, "the window follows the undo");
        assert_eq!(multi::load(&e), st.defs);
        assert_eq!(st.form.as_ref().map(|f| f.name.as_str()), Some("A"));
        // unsaved form edits survive a refresh
        st.form.as_mut().unwrap().name = "typing".into();
        e.undo();
        sync(&mut st, &e);
        assert!(st.defs.is_empty());
        let mut st = WinState::default();
        multi::save(&mut e, &[def("A")]).unwrap();
        sync(&mut st, &e);
        st.form.as_mut().unwrap().name = "typing".into();
        multi::save(&mut e, &[def("A"), def("C")]).unwrap();
        sync(&mut st, &e);
        assert_eq!(st.defs.len(), 2);
        assert_eq!(st.form.as_ref().map(|f| f.name.as_str()), Some("typing"));
    }

    /// The schedule is built once per model state, not once per frame.
    #[test]
    fn layout_and_preview_are_cached_between_frames() {
        let mut e = editor();
        let mut st = WinState::default();
        let d = def("A");
        let a = layout_of(&mut st, &e, &d, false);
        let b = layout_of(&mut st, &e, &d, false);
        assert!(
            Rc::ptr_eq(&a, &b),
            "same model, same definition: no rebuild"
        );
        let c = layout_of(&mut st, &e, &d, true);
        assert!(!Rc::ptr_eq(&a, &c), "another language rebuilds");
        let p1 = preview_of(&mut st, &e, &c);
        let p2 = preview_of(&mut st, &e, &c);
        assert!(Rc::ptr_eq(&p1, &p2));
        st.touch();
        assert!(
            !Rc::ptr_eq(&p1, &preview_of(&mut st, &e, &c)),
            "a draft change asks again"
        );
        let wall = e.walls[0].id;
        let spec = OpeningSpec {
            along: 1.0,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
            kind: "door".into(),
        };
        e.run_op(crate::editor::OpFn(std::sync::Arc::new(
            move |m: &mut ddc_model::Model| m.add_opening(wall, &spec).map(|_| ()),
        )))
        .unwrap();
        let after = layout_of(&mut st, &e, &d, true);
        assert!(!Rc::ptr_eq(&c, &after), "a model change rebuilds");
        assert!(after.gen > c.gen);
    }

    fn wall_h(e: &Editor, id: i64) -> f64 {
        e.walls.iter().find(|w| w.id == id).unwrap().height
    }

    /// Staged cells and the edited form are counted for the project question; Discard drops
    /// them, Save puts the cells in as one undo step and stores the form.
    #[test]
    fn staged_cells_and_form_are_project_drafts() {
        let mut e = editor();
        multi::save(&mut e, &[def("A")]).unwrap();
        let mut st = WinState::default();
        sync(&mut st, &e);
        let m = multi::build(&e, &st.defs[0], false);
        let h = m.table.cols.iter().position(|c| c.key == "h").unwrap();
        let id = m.parts[0].rows[0].id;
        let stage = |st: &mut WinState| {
            st.draft.merge(multi::draft_cells(&m, &[(0, h, "3500".to_string())]));
        };
        stage(&mut st);
        WIN.with(|w| *w.borrow_mut() = st);
        assert_eq!(draft_count(), 1);
        assert_eq!(crate::project_ui::drafts::count(&e), 1);
        discard_drafts();
        assert_eq!(draft_count(), 0, "discard drops the staged cells");
        assert_eq!(crate::project_ui::drafts::count(&e), 0);

        let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
        stage(&mut st);
        st.form.as_mut().unwrap().name = "B".into();
        WIN.with(|w| *w.borrow_mut() = st);
        assert_eq!(draft_count(), 2, "a cell and the form");
        let (u0, before) = (e.undo_len(), wall_h(&e, id));
        assert!(save_drafts(&mut e));
        assert_eq!(e.undo_len(), u0 + 2, "cells in one step, then the definition");
        assert!((wall_h(&e, id) - 3.5).abs() < 1e-9 && (before - 3.5).abs() > 1e-9);
        assert_eq!(multi::load(&e)[0].name, "B");
        assert_eq!(draft_count(), 0);
        WIN.with(|w| *w.borrow_mut() = WinState::default());
    }
}
