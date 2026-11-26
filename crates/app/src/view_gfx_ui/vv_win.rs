// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! VV "Visibility and Graphics" window of the current view: detail level, thin lines, categories, filters.
//! Edits accumulate in a draft and are applied with one button (a single undo step); fields under a
//! linked template are locked. Also hosts the filter rule editor and the project object styles window.

use std::cell::RefCell;
use std::collections::BTreeMap;

use ddc_model::{
    CatGfx, DetailLevel, FilterRule, FilterUse, GfxOverride, ParamDef, RuleOp, TemplateField,
    ViewFilter, ViewFilterSpec, ViewGfx, ViewGfxState, ViewKey, ViewRange, GFX_CATEGORIES,
};
use ddc_ui::widgets::{self, Button};

use super::vv_rows::{self, Toggle};
use super::{
    category_label, over_cells, over_cells_marked, rule_value, run, t, view_label, view_label_in,
};
use super::vv_table;
use crate::editor::Editor;
use crate::units::{active_units, edit_length, parse_edit_length, unit_label, Units};

/// Table rows per height.
const TABLE_ROWS: f32 = 14.0;
/// Rows of the category table before it scrolls, and of the shorter filter table.
const CAT_ROWS: f32 = TABLE_ROWS;
const FILTER_ROWS: f32 = 10.0;
/// Built-in rule parameters (the model understands these names).
const BUILTIN_PARAMS: [&str; 7] = [
    "category", "type", "level", "name", "number", "mark", "material",
];

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Categories,
    Filters,
}

struct FilterDraft {
    id: Option<i64>,
    /// Rule values as shown on screen, in project units.
    spec: ViewFilterSpec,
    /// The spec as opened: a draft only once it differs.
    orig: ViewFilterSpec,
    known: rule_value::Known,
    error: Option<String>,
    /// First click on Delete arms it; the second deletes.
    armed: bool,
}

#[derive(Default)]
struct Win {
    open: bool,
    view: Option<ViewKey>,
    /// The active view while the window stays on `view` because its edits are not applied.
    switched: Option<ViewKey>,
    /// The edited view no longer exists (level deleted, undo of its creation): nothing can be
    /// written, only Discard is offered. Cleared when the view comes back (redo).
    gone: bool,
    /// `Editor::cache_key` the view state was read under.
    rev: Option<(u64, u64)>,
    state: ViewGfxState,
    draft: ViewGfx,
    dirty: bool,
    /// An edited row of `draft` changed in the model under it: keep the edits or reload.
    conflict: bool,
    /// Thin lines shown now (display toggle, not part of the draft) and a click waiting.
    thin: bool,
    thin_set: Option<bool>,
    tab: Tab,
    filters: Vec<ViewFilter>,
    /// View label, template name and project parameters: the window is drawn every frame, so the model is not queried.
    title: String,
    /// The view's name in English and Russian, for the project close question.
    names: [String; 2],
    template_name: String,
    defs: Vec<ParamDef>,
    editor: Option<FilterDraft>,
    styles_open: bool,
    /// `Editor::cache_key` the stored object styles were read under.
    styles_rev: Option<(u64, u64)>,
    styles: BTreeMap<String, GfxOverride>,
    styles_saved: BTreeMap<String, GfxOverride>,
    /// A category with edits changed in the model under them: keep them or reload.
    styles_conflict: bool,
    /// View range fields as typed (cut, bottom, depth) and why they cannot be applied.
    range_text: [String; 3],
    range_err: Option<String>,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

pub(super) fn open() {
    WIN.with(|w| w.borrow_mut().open = true);
}

pub(super) fn open_styles() {
    WIN.with(|w| w.borrow_mut().styles_open = true);
}

fn locked(st: &Win, f: TemplateField) -> bool {
    st.state.locked.contains(&f)
}

/// Follows the active view `active`, except that edits are never dropped by a view switch: the
/// window stays on the edited view and asks (`switched`) until they are applied or discarded.
/// A model change under edits lays them over the new state (`rebase_draft`).
fn sync(st: &mut Win, ed: &Editor, active: ViewKey) {
    let v = match st.view {
        Some(old) if st.dirty && old != active => old,
        _ => active,
    };
    let key = ed.cache_key();
    let new_view = st.view != Some(v);
    if new_view || st.rev != Some(key) {
        st.gone = st.dirty && !new_view && !view_exists(ed, v);
    }
    // a gone view takes no edits: the Discard-only question replaces `switched`
    st.switched = (v != active && !st.gone).then_some(active);
    if st.gone {
        // `state` stays as read before, so a redo that brings the view back rebases from it
        st.rev = Some(key);
        return;
    }
    if !new_view && st.rev == Some(key) {
        st.thin = super::thin_of(ed, v, st.state.effective.thin_lines);
        return;
    }
    let old = std::mem::replace(&mut st.state, ed.model.view_gfx(v).unwrap_or_default());
    st.filters = ed.model.view_filters().unwrap_or_default();
    st.title = view_label(ed, v);
    st.names = [view_label_in(false, ed, v), view_label_in(true, ed, v)];
    st.template_name = st
        .state
        .template
        .and_then(|id| {
            let all = ed.model.view_templates().unwrap_or_default();
            all.into_iter().find(|x| x.id == id)
        })
        .map_or(String::new(), |x| x.name);
    st.defs = ed.model.params().unwrap_or_default();
    st.rev = Some(key);
    st.thin = super::thin_of(ed, v, st.state.effective.thin_lines);
    if new_view || !st.dirty {
        st.draft = st.state.effective.clone();
        st.dirty = false;
        st.conflict = false;
        st.view = Some(v);
        st.range_text = range_texts(st.draft.range, active_units());
        st.range_err = None;
    } else {
        rebase_draft(st, &old.effective);
    }
}

/// The view is still in the model: the 3D view always, a plan while its level exists, a section
/// while its definition exists. A failed read counts as gone, so nothing is written blind.
fn view_exists(ed: &Editor, v: ViewKey) -> bool {
    match v {
        ViewKey::ThreeD => true,
        ViewKey::Plan(l) => ed.model.level_list().is_ok_and(|ls| ls.iter().any(|x| x.0 == l)),
        ViewKey::Section(id) => ed.model.view_def(id).is_ok(),
    }
}

/// Discard of the view draft: the next sync follows the active view.
fn drop_view_draft(st: &mut Win) {
    st.dirty = false;
    st.conflict = false;
    st.gone = false;
    st.view = None;
}

/// The model changed under a draft with edits: they are laid over the new state row by row,
/// a changed edited row asks (`conflict`), and a draft the model now holds is no draft.
fn rebase_draft(st: &mut Win, base: &ViewGfx) {
    let typed = st.range_err.is_some();
    let range_edited = typed || st.draft.range != base.range;
    let (draft, conflict) = rebase_gfx(base, &st.draft, &st.state.effective, typed);
    st.draft = draft;
    st.conflict |= conflict;
    if !range_edited {
        st.range_text = range_texts(st.draft.range, active_units());
    }
    if !typed && st.draft == st.state.effective {
        st.dirty = false;
        st.conflict = false;
    }
}

/// The edits of `draft` against `base` laid over `stored`: detail, thin lines, range, each
/// category, and the filter list as one row (its order is the priority). True on a conflict.
/// `typed`: the range fields hold text that is an edit even while `draft.range` is unchanged.
fn rebase_gfx(base: &ViewGfx, draft: &ViewGfx, stored: &ViewGfx, typed: bool) -> (ViewGfx, bool) {
    let (detail, c1) = rebase_one(&base.detail, &draft.detail, &stored.detail);
    let (thin_lines, c2) = rebase_one(&base.thin_lines, &draft.thin_lines, &stored.thin_lines);
    let (categories, c3) = rebase_map(&base.categories, &draft.categories, &stored.categories);
    let (filters, c4) = rebase_one(&base.filters, &draft.filters, &stored.filters);
    let (range, c5) = if typed && draft.range == base.range {
        (draft.range, stored.range != base.range)
    } else {
        rebase_one(&base.range, &draft.range, &stored.range)
    };
    let g = ViewGfx {
        detail,
        thin_lines,
        categories,
        filters,
        range,
    };
    (g, c1 || c2 || c3 || c4 || c5)
}

/// One edited value laid over the model: the edit when there is one, else the model's value;
/// true when the model changed it to something that is neither the old value nor the edit.
fn rebase_one<T: PartialEq + Clone>(base: &T, draft: &T, stored: &T) -> (T, bool) {
    if draft == base {
        return (stored.clone(), false);
    }
    (draft.clone(), stored != base && stored != draft)
}

/// [`rebase_one`] per key; a missing key is a row at its default (removed or never set).
fn rebase_map<K: Ord + Clone, V: PartialEq + Clone>(
    base: &BTreeMap<K, V>,
    draft: &BTreeMap<K, V>,
    stored: &BTreeMap<K, V>,
) -> (BTreeMap<K, V>, bool) {
    let mut out = stored.clone();
    let mut conflict = false;
    for k in base.keys().chain(draft.keys()) {
        let (b, d) = (base.get(k), draft.get(k));
        if b == d {
            continue;
        }
        let (v, c) = rebase_one(&b, &d, &stored.get(k));
        conflict |= c;
        match v {
            Some(v) => {
                out.insert(k.clone(), v.clone());
            }
            None => {
                out.remove(k);
            }
        }
    }
    (out, conflict)
}

/// Reload: the draft is the view's stored graphics again, the edits go.
fn reload_view(st: &mut Win) {
    st.draft = st.state.effective.clone();
    st.dirty = false;
    st.conflict = false;
    st.range_text = range_texts(st.draft.range, active_units());
    st.range_err = None;
}

/// A filter deleted from this window leaves the view's uses in the model too: the draft and
/// the state it was read from both lose it, so the next read is no conflict.
fn drop_filter(st: &mut Win, id: i64) {
    st.draft.filters.retain(|u| u.filter != id);
    st.state.effective.filters.retain(|u| u.filter != id);
}

/// Signed length for a range field: zero and negative offsets are allowed.
fn parse_offset(s: &str, u: Units) -> Option<f64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, s),
    };
    let digits: Vec<char> = body.chars().filter(char::is_ascii_digit).collect();
    let mark = |c: char| c.is_ascii_digit() || " .,'\"-\u{a0}\u{202f}".contains(c);
    let zero = !digits.is_empty() && digits.iter().all(|&c| c == '0') && body.chars().all(mark);
    let v = if zero {
        0.0
    } else {
        parse_edit_length(body, u)?
    };
    Some(if neg { -v } else { v })
}

fn offset_text(m: f64, u: Units) -> String {
    if m < 0.0 {
        format!("-{}", edit_length(-m, u))
    } else {
        edit_length(m, u)
    }
}

fn range_texts(r: ViewRange, u: Units) -> [String; 3] {
    [r.cut, r.bottom, r.depth].map(|m| offset_text(m, u))
}

/// Range from the three fields, or the i18n key of the reason it is refused.
fn parse_range(text: &[String; 3], u: Units) -> Result<ViewRange, String> {
    let mut v = [0.0; 3];
    for (out, s) in v.iter_mut().zip(text) {
        *out = parse_offset(s, u).ok_or_else(|| "view_range.err.number".to_string())?;
    }
    let r = ViewRange {
        cut: v[0],
        bottom: v[1],
        depth: v[2],
        top: None,
    };
    match r.issue() {
        Some(i) => Err(format!("view_range.err.{}", i.key())),
        None => Ok(r),
    }
}

/// The three fields leave the top plane (set in the drawing quality window) as it was; a cut
/// raised past it pushes it up, as dragging the cut there does.
fn keep_top(r: ViewRange, top: Option<f64>) -> ViewRange {
    ViewRange {
        top: top.map(|t| t.max(r.cut)),
        ..r
    }
}

/// Frame of one view range field: «−12 000» whole at the field font.
const RANGE_FIELD_W: f32 = 72.0;

/// Plan view range row: cut plane, bottom and view depth in project units.
fn range_ui(ui: &mut egui::Ui, st: &mut Win) {
    if !matches!(st.view, Some(ViewKey::Plan(_))) {
        return;
    }
    let u = active_units();
    let closed = locked(st, TemplateField::ViewRange);
    let th = ddc_ui::theme::of(ui.ctx());
    // the unit stands outside the frame: the slot holds a five-digit offset and the unit
    let unit = unit_label(u);
    let unit_w = widgets::form::text_w(ui, &th, &unit) + ddc_ui::theme::space::SM;
    let width = RANGE_FIELD_W + unit_w;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(t("view_range.range"));
        ui.add_enabled_ui(!closed, |ui| {
            for (i, key) in ["view_range.cut", "view_range.bottom", "view_range.depth"].into_iter().enumerate() {
                let tip = t(&format!("{key}.hint"));
                let name = t(key);
                let lab = ui.label(name.as_str()).on_hover_text(tip.as_str());
                let out = widgets::TextField::new(&mut st.range_text[i])
                    .id_salt(("vv.range", i))
                    .labelled_by(lab.id, name.as_str())
                    .unit(&unit)
                    .width(width)
                    .right_aligned()
                    .show(ui, &th);
                changed |= out.changed;
                out.response.on_hover_text(tip.as_str());
            }
        });
    });
    if changed {
        st.dirty = true;
        match parse_range(&st.range_text, u) {
            Ok(r) => {
                st.draft.range = keep_top(r, st.draft.range.top);
                st.range_err = None;
            }
            Err(key) => st.range_err = Some(t(&key)),
        }
    }
    if let Some(e) = &st.range_err {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
}

fn head_ui(ui: &mut egui::Ui, st: &mut Win) {
    if st.state.template.is_some() {
        let fields = st
            .state
            .locked
            .iter()
            .map(|f| t(&format!("view_gfx.field.{}", f.key())))
            .collect::<Vec<_>>()
            .join(", ");
        ui.label(crate::tr(
            "view_gfx.vv.template_banner",
            &[("name", &st.template_name), ("fields", &fields)],
        ));
    }
    let th = ddc_ui::theme::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(t("view_gfx.vv.detail"));
        let closed = locked(st, TemplateField::Detail);
        ui.add_enabled_ui(!closed, |ui| {
            let names: Vec<String> = DetailLevel::ALL
                .iter()
                .map(|d| t(&format!("view_gfx.detail.{}", d.key())))
                .collect();
            let items: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut sel = DetailLevel::ALL
                .iter()
                .position(|d| *d == st.draft.detail)
                .unwrap_or(0);
            let r = widgets::dropdown(ui, &th, "view_gfx.vv.detail", &mut sel, &items, None);
            // the list is named by the label beside it, so a screen reader says what it picks
            let name = t("view_gfx.vv.detail");
            ui.ctx().accesskit_node_builder(r.id, |n| n.set_label(name));
            if r.changed() {
                st.draft.detail = DetailLevel::ALL[sel];
                st.dirty = true;
            }
        });
        let closed = locked(st, TemplateField::ThinLines);
        ui.add_enabled_ui(!closed, |ui| {
            let mut on = st.thin;
            if widgets::checkbox(ui, &th, &mut on, &t("view_gfx.vv.thin")).changed() {
                st.thin = on;
                st.thin_set = Some(on);
            }
        });
    });
}

/// Header strip of a table: one strong cell per column key.
fn header(ui: &mut egui::Ui, cols: &[&str]) {
    for k in cols {
        ui.strong(t(k));
    }
    ui.end_row();
}

/// A toggle drawn by the kit: label (the row's name for categories), tooltip, change flag.
fn toggle(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    kind: Toggle,
    on: &mut bool,
    name: &str,
    lock: Option<&str>,
) -> bool {
    let label = match kind.label_key() {
        Some(k) => t(k),
        None => name.to_string(),
    };
    let mut r = widgets::checkbox(ui, th, on, &label).on_hover_text(t(kind.tip_key()));
    if let Some(why) = lock {
        r = r.on_disabled_hover_text(why);
    }
    r.changed()
}

/// Muted "controlled by template" line above a locked tab; Edit template opens that window.
fn lock_line(ui: &mut egui::Ui, st: &Win, closed: bool, ru: bool) -> Option<String> {
    if !closed {
        return None;
    }
    if vv_table::lock_note(ui, &st.template_name, ru) {
        super::template_win::open();
    }
    Some(vv_table::lock_tip(&st.template_name, ru))
}

fn categories_ui(ui: &mut egui::Ui, st: &mut Win) {
    let closed = locked(st, TemplateField::Categories);
    let ru = crate::ui_is_ru();
    let why = lock_line(ui, st, closed, ru);
    let max_h = vv_table::rows_h(ui, CAT_ROWS);
    ui.add_enabled_ui(!closed, |ui| {
        let th = ddc_ui::theme::of(ui.ctx());
        let slot = th.d().control_sm;
        vv_table::show(ui, "f5.vv.cats", &vv_rows::CAT_COLS, ru, max_h, |ui, rec| {
            for row in vv_rows::cat_rows(&st.draft) {
                let mut c = row.gfx;
                rec.at(ui, 0);
                let name = category_label(row.cat);
                toggle(ui, &th, Toggle::CategoryVisible, &mut c.visible, &name, why.as_deref());
                let mut mark = |ui: &egui::Ui, k: usize| rec.at(ui, 1 + k);
                over_cells_marked(ui, &mut c.over, &mut mark);
                rec.at(ui, 5);
                if row.show_reset() {
                    let r = Button::quiet(&t("view_gfx.reset")).small().show(ui, &th);
                    if r.on_hover_text(t("view_gfx.vv.reset_tip")).clicked() {
                        c = CatGfx::default();
                    }
                } else {
                    ui.allocate_space(egui::vec2(slot, slot));
                }
                rec.end_row(ui);
                st.dirty |= vv_rows::set_cat(&mut st.draft, row.cat, c);
            }
        });
    });
}

fn filters_ui(ui: &mut egui::Ui, st: &mut Win) {
    let closed = locked(st, TemplateField::Filters);
    let row = ui.spacing().interact_size.y;
    let ru = crate::ui_is_ru();
    let why = lock_line(ui, st, closed, ru);
    let names: BTreeMap<i64, String> = st
        .filters
        .iter()
        .map(|f| (f.id, f.spec.name.clone()))
        .collect();
    enum Act {
        Up(usize),
        Remove(usize),
        Edit(i64),
    }
    let mut act = None;
    let mut add_new = false;
    let th = ddc_ui::theme::of(ui.ctx());
    if st.draft.filters.is_empty() {
        ui.add_enabled_ui(!closed, |ui| {
            let (title, hint) = (t("view_template.no_filters"), t("view_gfx.vv.no_filters"));
            let add = t("view_gfx.vv.new_filter");
            add_new = widgets::empty_state(ui, &th, None, &title, &hint, Some(&add));
        });
    } else {
        let max_h = vv_table::rows_h(ui, FILTER_ROWS);
        ui.add_enabled_ui(!closed, |ui| {
            let id = "f5.vv.filters";
            vv_table::show(ui, id, &vv_rows::FILTER_COLS, ru, max_h, |ui, rec| {
                for (i, u) in st.draft.filters.iter_mut().enumerate() {
                    let name = names.get(&u.filter).map_or("", String::as_str);
                    rec.at(ui, 0);
                    let edit = ui
                        .scope(|ui| {
                            ui.set_max_width(ddc_ui::sem::layout::PROP_KEY_MAX);
                            Button::quiet(name).show(ui, &th)
                        })
                        .inner;
                    let tip = format!("{name}\n{}", t("view_gfx.vv.edit_filter"));
                    // the filter's name is the person's: cut to the column, whole in the tip
                    if crate::ui_kit::user_text_tip(edit, name, tip).clicked() {
                        act = Some(Act::Edit(u.filter));
                    }
                    let lock = why.as_deref();
                    rec.at(ui, 1);
                    st.dirty |= toggle(ui, &th, Toggle::FilterOn, &mut u.enabled, name, lock);
                    rec.at(ui, 2);
                    st.dirty |= toggle(ui, &th, Toggle::FilterVisible, &mut u.visible, name, lock);
                    let mut mark = |ui: &egui::Ui, k: usize| rec.at(ui, 3 + k);
                    st.dirty |= over_cells_marked(ui, &mut u.over, &mut mark);
                    rec.at(ui, 7);
                    ui.horizontal(|ui| {
                        let tip = t("view_gfx.vv.priority");
                        let up = ddc_ui::widgets::glyph_button(ui, "move-up", i > 0, &tip);
                        if up.clicked() {
                            act = Some(Act::Up(i));
                        }
                        if ddc_ui::widgets::close_button(ui, &t("view_gfx.vv.detach")).clicked() {
                            act = Some(Act::Remove(i));
                        }
                    });
                    rec.end_row(ui);
                }
            });
        });
    }
    if add_new {
        st.editor = Some(new_filter_draft());
    }
    match act {
        Some(Act::Up(i)) => {
            st.draft.filters.swap(i, i - 1);
            st.dirty = true;
        }
        Some(Act::Remove(i)) => {
            st.draft.filters.remove(i);
            st.dirty = true;
        }
        Some(Act::Edit(id)) => {
            let spec = st
                .filters
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.spec.clone());
            st.editor = spec.map(|spec| {
                let (spec, known) = rule_value::open(spec, &st.defs, active_units());
                FilterDraft {
                    id: Some(id),
                    orig: spec.clone(),
                    spec,
                    known,
                    error: None,
                    armed: false,
                }
            });
        }
        None => {}
    }
    ui.add_space(row / 4.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!closed, |ui| {
            let free: Vec<(i64, String)> = st
                .filters
                .iter()
                .filter(|f| !st.draft.filters.iter().any(|u| u.filter == f.id))
                .map(|f| (f.id, f.spec.name.clone()))
                .collect();
            // item 0 is the caption; picking another attaches that filter
            let th = ddc_ui::theme::of(ui.ctx());
            let caption = if free.is_empty() {
                t("view_gfx.vv.attach_none")
            } else {
                t("view_gfx.vv.attach")
            };
            let mut items: Vec<&str> = vec![caption.as_str()];
            items.extend(free.iter().map(|(_, n)| n.as_str()));
            let mut sel = 0;
            // the closed list shows its caption whole: frame padding, chevron slot and gap
            let font = egui::TextStyle::Button.resolve(ui.style());
            let col = ui.visuals().text_color();
            let text_w = ui.fonts(|f| f.layout_no_wrap(caption.clone(), font, col).size().x);
            use ddc_ui::theme::space::{SM, XS};
            let w = (text_w + SM * 2.0 + 16.0 + XS * 2.0)
                .max(ddc_ui::theme::control::DROPDOWN_W)
                .min(ui.available_width());
            let r = widgets::dropdown(ui, &th, "view_gfx.vv.attach", &mut sel, &items, Some(w));
            // named by what it does; the caption says whether anything is left to attach
            let name = t("view_gfx.vv.attach");
            ui.ctx().accesskit_node_builder(r.id, |n| n.set_label(name));
            if r.changed() && sel > 0 {
                st.draft.filters.push(new_use(free[sel - 1].0));
                st.dirty = true;
            }
        });
        let th = ddc_ui::theme::of(ui.ctx());
        // an empty open tab already offers this in its empty note
        let shown = !st.draft.filters.is_empty() || closed;
        if shown && Button::secondary(&t("view_gfx.vv.new_filter")).show(ui, &th).clicked() {
            st.editor = Some(new_filter_draft());
        }
    });
}

fn new_filter_draft() -> FilterDraft {
    let spec = ViewFilterSpec {
        rules: vec![FilterRule {
            param: String::new(),
            op: RuleOp::Eq,
            value: String::new(),
        }],
        ..Default::default()
    };
    FilterDraft {
        id: None,
        orig: spec.clone(),
        spec,
        known: vec![],
        error: None,
        armed: false,
    }
}

fn new_use(filter: i64) -> FilterUse {
    FilterUse {
        filter,
        enabled: true,
        visible: true,
        over: GfxOverride::default(),
    }
}

fn editor_ui(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win) {
    let Some(mut d) = st.editor.take() else {
        return;
    };
    let mut close = false;
    ui.strong(t(if d.id.is_some() {
        "view_gfx.flt.edit"
    } else {
        "view_gfx.flt.new"
    }));
    let th = ddc_ui::theme::of(ui.ctx());
    ui.horizontal(|ui| {
        let name = t("view_gfx.flt.name");
        let lab = ui.label(name.as_str());
        widgets::TextField::new(&mut d.spec.name)
            .id_salt("vv.flt.name")
            .labelled_by(lab.id, name.as_str())
            .show(ui, &th);
    });
    egui::CollapsingHeader::new(if d.spec.categories.is_empty() {
        t("view_gfx.flt.all_categories")
    } else {
        crate::tr(
            "view_gfx.flt.n_categories",
            &[("n", &d.spec.categories.len().to_string())],
        )
    })
    .id_salt("f5.flt.cats")
    .show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (cat, _) in GFX_CATEGORIES {
                let mut on = d.spec.categories.iter().any(|c| c == cat);
                if widgets::checkbox(ui, &th, &mut on, &category_label(cat)).changed() {
                    if on {
                        d.spec.categories.push(cat.to_string());
                    } else {
                        d.spec.categories.retain(|c| c != cat);
                    }
                }
            }
        });
    });
    let modes = [t("view_gfx.flt.all_rules"), t("view_gfx.flt.any_rule")];
    let mode_items = [modes[0].as_str(), modes[1].as_str()];
    if let Some(i) = widgets::segmented(ui, &th, &mode_items, usize::from(d.spec.any)) {
        d.spec.any = i == 1;
    }
    let mut drop_rule = None;
    let units = active_units();
    let pick_params: Vec<String> = ed
        .model
        .params()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.spec.name)
        .collect();
    egui::Grid::new("f5.flt.rules")
        .num_columns(4)
        .show(ui, |ui| {
            for (i, r) in d.spec.rules.iter_mut().enumerate() {
                let hint = rule_value::unit(r, &st.defs, units)
                    .map_or_else(|| t("view_gfx.flt.value_hint"), str::to_string);
                ui.horizontal(|ui| {
                    let pname = t("view_gfx.flt.param_hint");
                    widgets::TextField::new(&mut r.param)
                        .id_salt(("vv.flt.param", i))
                        .labelled_by(egui::Id::new(("vv.flt.param.label", i)), pname.as_str())
                        .hint(pname.as_str())
                        .width(ui.spacing().interact_size.x * 3.0)
                        .show(ui, &th);
                    // item 0 is the caption; the project parameters follow, then the built-in ones
                    let mut keys: Vec<String> = pick_params.clone();
                    keys.extend(BUILTIN_PARAMS.iter().map(|b| b.to_string()));
                    let mut names: Vec<String> = vec![t("view_gfx.flt.param_pick")];
                    names.extend(pick_params.iter().cloned());
                    names.extend(BUILTIN_PARAMS.iter().map(|b| t(&format!("view_gfx.flt.param.{b}"))));
                    let items: Vec<&str> = names.iter().map(String::as_str).collect();
                    let mut sel = 0;
                    let pick = widgets::dropdown(ui, &th, ("f5.flt.pick", i), &mut sel, &items, None);
                    let name = pname.clone();
                    ui.ctx().accesskit_node_builder(pick.id, |n| n.set_label(name));
                    if pick.changed() && sel > 0 {
                        r.param = keys[sel - 1].clone();
                    }
                });
                let op_names: Vec<String> = RuleOp::ALL
                    .iter()
                    .map(|op| t(&format!("view_gfx.op.{}", op.key())))
                    .collect();
                let op_items: Vec<&str> = op_names.iter().map(String::as_str).collect();
                let mut op_sel = RuleOp::ALL.iter().position(|o| *o == r.op).unwrap_or(0);
                if widgets::dropdown(ui, &th, ("f5.flt.op", i), &mut op_sel, &op_items, None)
                    .changed()
                {
                    r.op = RuleOp::ALL[op_sel];
                }
                ui.add_enabled_ui(r.op.takes_value(), |ui| {
                    let vname = t("view_gfx.flt.value_hint");
                    widgets::TextField::new(&mut r.value)
                        .id_salt(("vv.flt.value", i))
                        .labelled_by(egui::Id::new(("vv.flt.value.label", i)), vname.as_str())
                        .hint(hint.as_str())
                        .width(ui.spacing().interact_size.x * 2.5)
                        .show(ui, &th);
                });
                if ddc_ui::widgets::close_button(ui, &t("view_gfx.delete")).clicked() {
                    drop_rule = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = drop_rule {
        d.spec.rules.remove(i);
    }
    if d.spec.rules.is_empty() {
        ui.weak(t("view_gfx.flt.no_rules"));
    }
    if Button::secondary(&t("view_gfx.flt.add_rule")).show(ui, &th).clicked() {
        d.spec.rules.push(FilterRule {
            param: String::new(),
            op: RuleOp::Eq,
            value: String::new(),
        });
    }
    if let Some(e) = &d.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    let valid = filter_valid(&d);
    let blocked = t("view_gfx.flt.save_blocked");
    crate::ui_kit::dialog_footer(ui, |ui| {
        let why = if valid { None } else { Some(blocked.as_str()) };
        if crate::ui_kit::primary_click(ui, t("view_gfx.flt.save"), why) && save_filter(ed, st, &mut d)
        {
            close = true;
        }
        if crate::ui_kit::cancel_button(ui, t("view_gfx.cancel")).clicked() {
            close = true;
        }
        if let Some(id) = d.id {
            let label = if d.armed {
                t("view_gfx.flt.confirm_delete")
            } else {
                t("view_gfx.delete")
            };
            if crate::ui_kit::danger_button(ui, label, true).clicked() {
                if !d.armed {
                    d.armed = true;
                } else if run(ed, move |m| m.delete_view_filter(id)) {
                    drop_filter(st, id);
                    close = true;
                }
            }
        }
    });
    if !close {
        st.editor = Some(d);
    }
}

fn filter_valid(d: &FilterDraft) -> bool {
    !d.spec.name.trim().is_empty() && d.spec.rules.iter().all(|r| !r.param.trim().is_empty())
}

/// Writes the filter as one undo step; a new one joins the view draft unless a template holds
/// the filters. False: the reason is in `d.error`.
fn save_filter(ed: &mut Editor, st: &mut Win, d: &mut FilterDraft) -> bool {
    let id = d.id;
    let saved = match rule_value::to_model(&d.spec, &d.known, &st.defs, active_units()) {
        Some(spec) => {
            let done = run(ed, move |m| match id {
                Some(id) => m.edit_view_filter(id, &spec),
                None => m.add_view_filter(&spec).map(|_| ()),
            });
            d.error = if done { None } else { ed.status.clone() };
            done
        }
        None => {
            d.error = Some(t("view_gfx.flt.bad_value"));
            false
        }
    };
    // the filter library is shared; attaching to a view under a template is not allowed
    if saved && id.is_none() && st.view.is_some() && !locked(st, TemplateField::Filters) {
        let name = d.spec.name.trim();
        let all = ed.model.view_filters().unwrap_or_default();
        if let Some(f) = all.iter().find(|f| f.spec.name == name) {
            st.draft.filters.push(new_use(f.id));
            st.dirty = true;
        }
    }
    saved
}

/// The question while the window stays on an edited view the user left: Apply in the footer
/// writes the edits to that view, Discard here drops them. True on Discard.
fn switched_ui(ui: &mut egui::Ui, title: &str) -> bool {
    crate::ui_kit::warn_note(ui, crate::tr("vv_conflict.vv.switched", &[("view", title)]));
    crate::ui_kit::danger_button(ui, t("vv_conflict.vv.discard"), true).clicked()
}

/// The question once the edited view is gone: no Apply, Discard is the only way on. True on it.
fn gone_ui(ui: &mut egui::Ui, title: &str) -> bool {
    crate::ui_kit::warn_note(ui, crate::tr("vv_conflict.vv.view_gone", &[("view", title)]));
    crate::ui_kit::danger_button(ui, t("vv_conflict.vv.discard"), true).clicked()
}

pub(super) fn window(ctx: &egui::Context, ed: &mut Editor, v: ViewKey) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if st.open {
        sync(&mut st, ed, v);
        let mut open = true;
        crate::ui_kit::Dialog::new("view.vv", crate::tr("view_gfx.vv.title", &[("view", &st.title)]), crate::ui_kit::DialogSize::Wide)
            .open(&mut open)
            .show(ctx, |ui| {
                // before the filter editor: a refused Save opens here, the reason and Discard first
                if st.gone {
                    if !gone_ui(ui, &st.title) {
                        return;
                    }
                    drop_view_draft(&mut st);
                    sync(&mut st, ed, v);
                }
                if st.editor.is_some() {
                    editor_ui(ui, ed, &mut st);
                    return;
                }
                if st.switched.is_some() && switched_ui(ui, &st.title) {
                    drop_view_draft(&mut st);
                    // this frame already shows the active view, not the dropped draft
                    sync(&mut st, ed, v);
                }
                if st.conflict {
                    match conflict_ui(ui, "vv_conflict.vv.conflict") {
                        Some(Answer::Keep) => st.conflict = false,
                        Some(Answer::Reload) => reload_view(&mut st),
                        None => {}
                    }
                }
                // the edited view, which is not the active one while `switched` asks
                let v = st.view.unwrap_or(v);
                head_ui(ui, &mut st);
                if let Some(on) = st.thin_set.take() {
                    super::set_thin(ed, v, on);
                }
                range_ui(ui, &mut st);
                ui.separator();
                let th = ddc_ui::theme::of(ui.ctx());
                let tabs = [t("view_gfx.vv.tab.categories"), t("view_gfx.vv.tab.filters")];
                let mut sel = usize::from(st.tab == Tab::Filters);
                widgets::tab_bar(ui, &th, &[tabs[0].as_str(), tabs[1].as_str()], &mut sel);
                st.tab = if sel == 1 { Tab::Filters } else { Tab::Categories };
                match st.tab {
                    Tab::Categories => categories_ui(ui, &mut st),
                    Tab::Filters => filters_ui(ui, &mut st),
                }
                let nothing = t("view_gfx.vv.nothing_to_apply");
                // a conflict is answered first: Apply would overwrite the changed rows
                let first = t("vv_conflict.styles.answer_first");
                let why = if let Some(e) = &st.range_err {
                    Some(e.as_str())
                } else if !st.dirty {
                    Some(nothing.as_str())
                } else if st.conflict {
                    Some(first.as_str())
                } else {
                    None
                };
                let (apply, revert) = crate::ui_kit::dialog_footer(ui, |ui| {
                    let apply = crate::ui_kit::primary_click(ui, t("view_gfx.apply"), why);
                    let revert = ui
                        .add_enabled_ui(st.dirty, |ui| {
                            crate::ui_kit::cancel_button(ui, t("view_gfx.cancel")).clicked()
                        })
                        .inner;
                    (apply, revert)
                });
                if apply && !st.conflict {
                    let mut g = st.draft.clone();
                    // thin lines is a display toggle, never written to the file here
                    g.thin_lines = st.state.effective.thin_lines;
                    if run(ed, move |m| m.set_view_gfx(v, &g)) {
                        st.dirty = false;
                        st.view = None;
                    }
                }
                if revert {
                    st.dirty = false;
                    st.conflict = false;
                    st.view = None;
                }
            });
        st.open = open;
    }
    WIN.with(|w| *w.borrow_mut() = st);
}

pub(super) fn styles_window(ctx: &egui::Context, ed: &mut Editor) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    // a closed window may still hold edits: the close question compares them to the model
    if st.styles_open || styles_edited(&st) {
        sync_styles(&mut st, ed);
    }
    if st.styles_open {
        let mut open = true;
        let mut answer = None;
        crate::ui_kit::Dialog::new("view.styles", t("view_gfx.styles.title"), crate::ui_kit::DialogSize::Tool)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.weak(t("view_gfx.styles.hint"));
                if st.styles_conflict {
                    answer = conflict_ui(ui, "vv_conflict.styles.conflict");
                }
                let row = ui.spacing().interact_size.y;
                let list = egui::ScrollArea::vertical()
                    .id_salt("f5.styles.list")
                    .max_height(row * TABLE_ROWS)
                    .show(ui, |ui| {
                        egui::Grid::new("f5.styles.grid")
                            .num_columns(5)
                            .striped(true)
                            .show(ui, |ui| {
                                header(ui, &vv_rows::CAT_COLS[..5]);
                                for (cat, _) in GFX_CATEGORIES {
                                    let mut o = st.styles.get(cat).copied().unwrap_or_default();
                                    ui.label(category_label(cat));
                                    if over_cells(ui, &mut o) {
                                        if o.is_empty() {
                                            st.styles.remove(cat);
                                        } else {
                                            st.styles.insert(cat.to_string(), o);
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                crate::ui_kit::mark_scroll_viewport(ui.ctx(), list.inner_rect);
                let changes = style_changes(&st.styles_saved, &st.styles);
                let nothing = t("view_gfx.vv.nothing_to_apply");
                // a conflict is answered first: Apply would overwrite the changed styles
                let first = t("vv_conflict.styles.answer_first");
                let why = if changes.is_empty() {
                    Some(nothing.as_str())
                } else if st.styles_conflict {
                    Some(first.as_str())
                } else {
                    None
                };
                let apply = crate::ui_kit::dialog_footer(ui, |ui| {
                    crate::ui_kit::primary_click(ui, t("view_gfx.apply"), why)
                });
                if apply {
                    apply_styles(ed, &mut st);
                }
            });
        match answer {
            Some(Answer::Keep) => st.styles_conflict = false,
            Some(Answer::Reload) => reload_styles(&mut st),
            None => {}
        }
        st.styles_open = open;
    }
    WIN.with(|w| *w.borrow_mut() = st);
}

/// The user's answer to a conflict between a window's edits and the model.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Answer {
    Keep,
    Reload,
}

/// The conflict note `note` (i18n key) with its two answers; the one clicked this frame.
fn conflict_ui(ui: &mut egui::Ui, note: &str) -> Option<Answer> {
    crate::ui_kit::warn_note(ui, t(note));
    let mut answer = None;
    ui.horizontal(|ui| {
        if crate::ui_kit::secondary_button(ui, t("vv_conflict.styles.keep")).clicked() {
            answer = Some(Answer::Keep);
        }
        if crate::ui_kit::secondary_button(ui, t("vv_conflict.styles.reload")).clicked() {
            answer = Some(Answer::Reload);
        }
    });
    answer
}

fn styles_edited(st: &Win) -> bool {
    !style_changes(&st.styles_saved, &st.styles).is_empty()
}

/// The model changed (undo, another window, a project swap): the stored styles are read anew
/// and the edits are laid over them. Edits stay; when a category with edits changed under them
/// to something else, the window asks (`styles_conflict`) instead of dropping or writing them.
fn sync_styles(st: &mut Win, ed: &Editor) {
    let key = ed.cache_key();
    if st.styles_rev == Some(key) {
        return;
    }
    st.styles_rev = Some(key);
    let stored = ed.model.object_styles().unwrap_or_default();
    let (draft, conflict) = rebase_map(&st.styles_saved, &st.styles, &stored);
    st.styles_conflict |= conflict;
    st.styles = draft;
    st.styles_saved = stored;
    if !styles_edited(st) {
        st.styles_conflict = false;
    }
}

/// Reload: the window shows the stored styles again, the edits go.
fn reload_styles(st: &mut Win) {
    st.styles = st.styles_saved.clone();
    st.styles_conflict = false;
}

/// Writes the edits in one undo step; own Apply is no conflict on the next read.
fn apply_styles(ed: &mut Editor, st: &mut Win) -> bool {
    if st.styles_conflict {
        return false;
    }
    let changes = style_changes(&st.styles_saved, &st.styles);
    if changes.is_empty() {
        return true;
    }
    if !run(ed, move |m| m.set_object_styles(&changes)) {
        return false;
    }
    st.styles_saved = ed.model.object_styles().unwrap_or_default();
    st.styles = st.styles_saved.clone();
    st.styles_rev = Some(ed.cache_key());
    true
}

/// A project swap: the view graphics, filter and object styles drafts of the old model are
/// neither shown nor asked about.
pub(crate) fn on_model_reset() {
    WIN.with(|w| {
        let Ok(mut st) = w.try_borrow_mut() else {
            return;
        };
        st.rev = None;
        st.view = None;
        st.switched = None;
        st.gone = false;
        st.dirty = false;
        st.conflict = false;
        st.range_err = None;
        st.editor = None;
        st.styles_rev = None;
        st.styles.clear();
        st.styles_saved.clear();
        st.styles_conflict = false;
    });
}

/// Differences between the style draft and the saved styles; a cleared style is an empty override.
fn style_changes(
    saved: &BTreeMap<String, GfxOverride>,
    draft: &BTreeMap<String, GfxOverride>,
) -> Vec<(String, GfxOverride)> {
    let mut out: Vec<(String, GfxOverride)> = draft
        .iter()
        .filter(|(k, v)| saved.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    for k in saved.keys().filter(|k| !draft.contains_key(*k)) {
        out.push((k.clone(), GfxOverride::default()));
    }
    out
}

/// Drafts a project close or swap would lose: the view whose graphics hold edits (its name),
/// the edited filter (its name, empty while unnamed) and whether the object styles hold edits.
pub(crate) fn drafts(ru: bool) -> (Option<String>, Option<String>, bool) {
    WIN.with(|w| {
        let Ok(st) = w.try_borrow() else {
            return (None, None, false);
        };
        let view = (st.dirty && st.view.is_some()).then(|| st.names[usize::from(ru)].clone());
        let filter = st
            .editor
            .as_ref()
            .filter(|d| d.spec != d.orig)
            .map(|d| d.spec.name.trim().to_string());
        let styles = !style_changes(&st.styles_saved, &st.styles).is_empty();
        (view, filter, styles)
    })
}

/// Discard on the project question: the edits go as Cancel in these windows drops them.
pub(crate) fn discard_drafts() {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        drop_view_draft(&mut st);
        st.editor = None;
        reload_styles(&mut st);
    });
}

/// Save on the project question: the filter, then the view graphics, then the object styles
/// go into the model. A refusal stops there and opens that window with the draft kept.
pub(crate) fn save_drafts(ed: &mut Editor) -> bool {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    let ok = commit(ed, &mut st);
    WIN.with(|w| *w.borrow_mut() = st);
    ok
}

fn commit(ed: &mut Editor, st: &mut Win) -> bool {
    if let Some(mut d) = st.editor.take() {
        if d.spec == d.orig {
            st.editor = Some(d);
        } else if !filter_valid(&d) || !save_filter(ed, st, &mut d) {
            if !filter_valid(&d) {
                d.error = Some(t("view_gfx.flt.save_blocked"));
            }
            st.editor = Some(d);
            st.open = true;
            return false;
        }
    }
    // the model may have changed since the window's last frame: the edits go over it first,
    // and a conflict (seen now or not yet answered) is never written over
    if let (true, Some(v)) = (st.dirty, st.view) {
        sync(st, ed, v);
        // a gone view is never written to: the window opens on its Discard-only question
        if st.conflict || st.gone {
            st.open = true;
            return false;
        }
    }
    if let (true, Some(v)) = (st.dirty, st.view) {
        let mut g = st.draft.clone();
        // thin lines is a display toggle, never written to the file here
        g.thin_lines = st.state.effective.thin_lines;
        if st.range_err.is_some() || !run(ed, move |m| m.set_view_gfx(v, &g)) {
            st.open = true;
            return false;
        }
        st.dirty = false;
        st.view = None;
    }
    // a conflict (seen now or not yet answered) is never written over: the window shows it
    if styles_edited(st) {
        sync_styles(st, ed);
    }
    if !apply_styles(ed, st) {
        st.styles_open = true;
        return false;
    }
    true
}

/// Gives this thread's window an unsaved detail level on the plan of `ed.level`.
#[cfg(test)]
pub(crate) fn seed_draft_for_test(ed: &Editor) {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        sync(&mut st, ed, ViewKey::Plan(ed.level));
        let cur = st.draft.detail;
        st.draft.detail = *DetailLevel::ALL.iter().find(|d| **d != cur).unwrap();
        st.dirty = true;
    });
}

/// Gives this thread's window a new, edited filter named `name`.
#[cfg(test)]
pub(crate) fn seed_filter_draft_for_test(name: &str) {
    let spec = ViewFilterSpec {
        name: name.into(),
        rules: vec![FilterRule {
            param: "category".into(),
            op: RuleOp::Eq,
            value: "wall".into(),
        }],
        ..Default::default()
    };
    WIN.with(|w| {
        w.borrow_mut().editor = Some(FilterDraft {
            id: None,
            spec,
            orig: ViewFilterSpec::default(),
            known: vec![],
            error: None,
            armed: false,
        });
    });
}

/// Gives this thread's window an unsaved line colour on the first object style category.
#[cfg(test)]
pub(crate) fn seed_styles_draft_for_test(ed: &Editor) {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        st.styles_saved = ed.model.object_styles().unwrap_or_default();
        st.styles_rev = Some(ed.cache_key());
        st.styles_conflict = false;
        st.styles = st.styles_saved.clone();
        let o = GfxOverride {
            line: Some(0x12_34_56),
            ..Default::default()
        };
        st.styles.insert(GFX_CATEGORIES[0].0.to_string(), o);
    });
}

/// Opens the window on the categories or the filters tab for the layout contract.
#[cfg(test)]
pub(super) fn open_for_layout(filters: bool) {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        *st = Win::default();
        st.open = true;
        st.tab = if filters { Tab::Filters } else { Tab::Categories };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_use_the_sticky_frame_and_the_row_token() {
        let src = include_str!("vv_win.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for (i, _) in body.match_indices("TABLE_ROWS *") {
            let rest = body[i + "TABLE_ROWS *".len()..].trim_start();
            assert!(!rest.starts_with(|c: char| c.is_ascii_digit()), "literal factor on rows");
        }
        for cols in ["CAT_COLS", "FILTER_COLS"] {
            let inline = format!("header(ui, &vv_rows::{cols});");
            assert!(!body.contains(&inline), "{cols} header is drawn inside the scroll area");
            assert!(body.contains(&format!("&vv_rows::{cols}, ru")), "{cols} not in the frame");
        }
        assert!(!body.contains("ui.weak(t(\"view_gfx.vv.no_filters\"))"), "empty filters need a note");
    }

    #[test]
    fn esc_closes_the_view_windows_and_keeps_the_view_unchanged() {
        let mut ed = Editor::new_in_memory().unwrap();
        let before = ed.model.view_gfx(ViewKey::ThreeD).unwrap_or_default();
        open();
        open_styles();
        crate::ui_kit::run_esc_frames(|c| {
            window(c, &mut ed, ViewKey::ThreeD);
            styles_window(c, &mut ed);
        });
        // one Esc goes to one window; the second press closes the other
        let (a, b) = WIN.with(|w| (w.borrow().open, w.borrow().styles_open));
        assert!(!(a && b));
        crate::ui_kit::run_esc_frames(|c| {
            window(c, &mut ed, ViewKey::ThreeD);
            styles_window(c, &mut ed);
        });
        assert_eq!(ed.model.view_gfx(ViewKey::ThreeD).unwrap_or_default(), before);
    }

    #[test]
    fn style_changes_lists_edits_and_removals() {
        let red = GfxOverride {
            fill: Some(0xFF0000),
            ..Default::default()
        };
        let saved: BTreeMap<String, GfxOverride> =
            [("wall".to_string(), red), ("door".to_string(), red)]
                .into_iter()
                .collect();
        let mut draft = saved.clone();
        assert!(style_changes(&saved, &draft).is_empty());
        draft.remove("door");
        let blue = GfxOverride {
            fill: Some(0x0000FF),
            ..Default::default()
        };
        draft.insert("slab".to_string(), blue);
        let ch = style_changes(&saved, &draft);
        assert_eq!(ch.len(), 2);
        assert!(ch.contains(&("slab".to_string(), blue)));
        assert!(ch.contains(&("door".to_string(), GfxOverride::default())));
    }

    fn texts(a: &str, b: &str, c: &str) -> [String; 3] {
        [a.to_string(), b.to_string(), c.to_string()]
    }

    #[test]
    fn range_fields_keep_the_top_plane() {
        let r = parse_range(&texts("900", "0", "0"), Units::Metric).unwrap();
        assert_eq!(keep_top(r, None).top, None);
        assert_eq!(keep_top(r, Some(2.5)).top, Some(2.5));
        let high = parse_range(&texts("3000", "0", "0"), Units::Metric).unwrap();
        let k = keep_top(high, Some(2.5));
        assert_eq!(k.top, Some(3.0), "a cut above the top pushes it");
        assert!(k.issue().is_none());
    }

    #[test]
    fn range_fields_parse_signed_lengths_and_refuse_with_reason() {
        let u = Units::Metric;
        let r = parse_range(&texts("1 200", "0", "0"), u).unwrap();
        assert_eq!(r, ViewRange::default());
        let r = parse_range(&texts("900", "-300", "1500"), u).unwrap();
        assert!((r.cut - 0.9).abs() < 1e-9);
        assert!((r.bottom + 0.3).abs() < 1e-9);
        assert!((r.depth - 1.5).abs() < 1e-9);
        // the shown text reads back as the same range
        assert_eq!(parse_range(&range_texts(r, u), u).unwrap(), r);
        let bad = [
            (texts("", "0", "0"), "view_range.err.number"),
            (texts("abc", "0", "0"), "view_range.err.number"),
            (texts("500", "500", "0"), "view_range.err.cut_not_above_bottom"),
            (texts("500", "800", "0"), "view_range.err.cut_not_above_bottom"),
            (texts("1200", "0", "-10"), "view_range.err.negative_depth"),
        ];
        for (x, key) in bad {
            assert_eq!(parse_range(&x, u), Err(key.to_string()), "{x:?}");
        }
        for (lang, src) in [
            ("en", include_str!("../../../ui/data/i18n/en/view_range.json")),
            ("ru", include_str!("../../../ui/data/i18n/ru/view_range.json")),
        ] {
            let j: serde_json::Value = serde_json::from_str(src).unwrap();
            let keys = [
                "view_range.err.number",
                "view_range.err.not_finite",
                "view_range.err.cut_not_above_bottom",
                "view_range.err.negative_depth",
                "view_range.err.top_below_cut",
                "view_gfx.field.view_range",
            ];
            for k in keys {
                assert!(j[k].is_string(), "{lang}: {k}");
            }
        }
    }

    #[test]
    fn bad_range_in_window_blocks_apply_and_writes_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut st = Win::default();
        let v = ViewKey::Plan(e.level);
        sync(&mut st, &e, v);
        assert_eq!(
            st.range_text,
            range_texts(ViewRange::default(), active_units())
        );
        st.range_text[0] = "0".into();
        assert_eq!(
            parse_range(&st.range_text, Units::Metric),
            Err("view_range.err.cut_not_above_bottom".to_string())
        );
        // the model refuses the same range on its own: nothing written, no undo step
        let steps = e.undo_len();
        let bad = ViewRange {
            cut: 0.0,
            ..Default::default()
        };
        assert!(!run(&mut e, move |m| m.set_view_range(v, bad)));
        assert_eq!(e.undo_len(), steps);
        assert_eq!(e.model.plan_view_range(e.level), ViewRange::default());
    }

    #[test]
    fn cut_height_changes_plan_section() {
        let mut e = Editor::new_in_memory().unwrap();
        let l = e.level;
        let id = e
            .model
            .add_mass(&ddc_model::MassSpec {
                level: l,
                name: "Box".into(),
                void: false,
                inplace: false,
                form: ddc_model::Form::Cuboid {
                    lo: (0., 0., 0.),
                    hi: (2., 2., 2.),
                },
            })
            .unwrap();
        e.reload().unwrap();
        assert!(e.mass_plan.iter().any(|m| m.0 == id), "1.2 m cuts the box");
        let steps = e.undo_len();
        let high = ViewRange {
            cut: 2.5,
            ..Default::default()
        };
        assert!(run(&mut e, move |m| m.set_view_range(ViewKey::Plan(l), high)));
        assert_eq!(e.undo_len(), steps + 1, "one undo step");
        assert_eq!(e.model.plan_view_range(l).cut, 2.5);
        assert!(!e.mass_plan.iter().any(|m| m.0 == id), "cut above the box");
        e.undo();
        assert_eq!(e.model.plan_view_range(l), ViewRange::default());
        assert!(e.mass_plan.iter().any(|m| m.0 == id), "undo restores the cut");
    }

    #[test]
    fn draft_follows_the_active_view_and_keeps_edits() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut st = Win::default();
        let v = ViewKey::Plan(e.level);
        sync(&mut st, &e, v);
        assert_eq!(st.view, Some(v));
        st.draft.thin_lines = true;
        st.dirty = true;
        assert!(run(&mut e, move |m| m
            .edit_view_gfx(v, |g| g.detail = DetailLevel::Fine)));
        sync(&mut st, &e, v);
        assert!(st.draft.thin_lines, "unsaved edits survive a model change");
        assert_eq!(st.state.effective.detail, DetailLevel::Fine);
        sync(&mut st, &e, ViewKey::ThreeD);
        assert!(st.draft.thin_lines && st.dirty, "a view switch with edits keeps the draft");
        assert_eq!((st.view, st.switched), (Some(v), Some(ViewKey::ThreeD)), "and asks");
        // Discard on the question: the next frame follows the active view
        st.dirty = false;
        st.view = None;
        sync(&mut st, &e, ViewKey::ThreeD);
        assert!(!st.draft.thin_lines, "another view starts from its own settings");
        assert_eq!((st.view, st.switched), (Some(ViewKey::ThreeD), None));
    }

    #[test]
    fn view_switch_with_edits_keeps_the_draft_and_apply_writes_the_edited_view() {
        let mut e = Editor::new_in_memory().unwrap();
        let plan = ViewKey::Plan(e.level);
        seed_draft_for_test(&e);
        let want = state(|s| s.draft.detail);
        let three = e.model.view_gfx(ViewKey::ThreeD).unwrap().effective.detail;
        state(|s| s.open = true);
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(egui::RawInput::default(), |c| window(c, &mut e, ViewKey::ThreeD));
        assert!(state(|s| s.dirty && s.view == Some(plan) && s.switched.is_some()));
        assert_eq!(drafts(false).0, Some(view_label_in(false, &e, plan)), "close still asks");
        assert!(save_drafts(&mut e));
        assert_eq!(e.model.view_gfx(plan).unwrap().effective.detail, want, "the edited view");
        assert_eq!(e.model.view_gfx(ViewKey::ThreeD).unwrap().effective.detail, three);
        state(|s| *s = Win::default());
    }

    #[test]
    fn view_switch_without_edits_follows_the_active_view() {
        let e = Editor::new_in_memory().unwrap();
        let mut st = Win::default();
        sync(&mut st, &e, ViewKey::Plan(e.level));
        sync(&mut st, &e, ViewKey::ThreeD);
        assert_eq!((st.view, st.switched), (Some(ViewKey::ThreeD), None));
    }

    #[test]
    fn view_state_reads_anew_when_an_older_revision_is_put_back() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut st = Win::default();
        let v = ViewKey::Plan(e.level);
        let rev = e.revision;
        sync(&mut st, &e, v);
        assert!(run(&mut e, move |m| m.edit_view_gfx(v, |g| g.detail = DetailLevel::Fine)));
        e.restore_revision(rev);
        sync(&mut st, &e, v);
        assert_eq!(st.state.effective.detail, DetailLevel::Fine, "keyed on cache_key");
        assert_eq!(st.draft.detail, DetailLevel::Fine, "a clean draft follows the model");
    }

    #[test]
    fn switched_question_strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for k in ["vv_conflict.vv.switched", "vv_conflict.vv.discard"] {
                assert_ne!(c.t(k), k, "missing {k}");
            }
        }
    }

    #[test]
    fn view_gone_strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            let k = "vv_conflict.vv.view_gone";
            assert_ne!(c.t(k), k, "missing {k}");
        }
    }

    /// A second level whose plan holds an edited detail level while 3D is active.
    fn edited_new_level(e: &mut Editor) -> (Win, ViewKey) {
        assert!(run(e, |m| m.add_level("L-r93a", 7.0).map(|_| ())));
        let ls = e.model.level_list().unwrap();
        let l = ls.iter().find(|x| x.1 == "L-r93a").unwrap().0;
        let v = ViewKey::Plan(l);
        let mut st = Win::default();
        sync(&mut st, e, v);
        let cur = st.draft.detail;
        st.draft.detail = *DetailLevel::ALL.iter().find(|d| **d != cur).unwrap();
        st.dirty = true;
        sync(&mut st, e, ViewKey::ThreeD);
        assert_eq!((st.view, st.switched), (Some(v), Some(ViewKey::ThreeD)), "asks");
        assert!(!st.gone);
        (st, v)
    }

    fn assert_gone_writes_nothing(e: &mut Editor, mut st: Win, v: ViewKey) {
        assert!(st.gone && st.dirty, "the edits are kept for Discard");
        assert_eq!((st.view, st.switched), (Some(v), None), "no Apply question");
        let rev = e.revision;
        assert!(!commit(e, &mut st), "project Save refuses");
        assert!(st.open && st.gone, "and shows the Discard-only question");
        assert_eq!(e.revision, rev, "nothing written");
        drop_view_draft(&mut st);
        sync(&mut st, e, ViewKey::ThreeD);
        assert_eq!((st.view, st.switched, st.gone), (Some(ViewKey::ThreeD), None, false));
        assert!(!st.dirty);
        assert!(commit(e, &mut st));
        assert_eq!(e.revision, rev, "Discard writes nothing either");
    }

    #[test]
    fn deleted_level_drops_the_switched_question_and_writes_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = edited_new_level(&mut e);
        let ViewKey::Plan(l) = v else { unreachable!() };
        assert!(run(&mut e, move |m| m.delete_level(l)));
        sync(&mut st, &e, ViewKey::ThreeD);
        assert_gone_writes_nothing(&mut e, st, v);
    }

    #[test]
    fn undo_of_the_view_creation_drops_the_question_and_redo_brings_it_back() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = edited_new_level(&mut e);
        let want = st.draft.detail;
        e.undo();
        sync(&mut st, &e, ViewKey::ThreeD);
        assert!(st.gone && st.switched.is_none(), "undo of the creation: gone");
        // the same frame again: still gone, the cache key did not move
        sync(&mut st, &e, ViewKey::ThreeD);
        assert!(st.gone && st.switched.is_none());
        e.redo();
        sync(&mut st, &e, ViewKey::ThreeD);
        assert!(!st.gone && st.dirty, "redo: the view is back with the edits");
        assert_eq!((st.view, st.switched), (Some(v), Some(ViewKey::ThreeD)));
        assert_eq!(st.draft.detail, want);
        e.undo();
        sync(&mut st, &e, ViewKey::ThreeD);
        assert_gone_writes_nothing(&mut e, st, v);
    }

    #[test]
    fn view_still_present_keeps_the_switched_question_and_apply() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = edited_new_level(&mut e);
        let want = st.draft.detail;
        // an unrelated model change: the view is still there
        assert!(run(&mut e, |m| m.add_level("L-other", 11.0).map(|_| ())));
        sync(&mut st, &e, ViewKey::ThreeD);
        assert!(!st.gone && st.dirty);
        assert_eq!((st.view, st.switched), (Some(v), Some(ViewKey::ThreeD)));
        assert!(commit(&mut e, &mut st));
        assert_eq!(e.model.view_gfx(v).unwrap().effective.detail, want, "the edited view");
    }

    #[test]
    fn window_on_a_gone_view_shows_no_apply_and_writes_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = edited_new_level(&mut e);
        e.undo();
        st.open = true;
        state(|s| *s = st);
        let rev = e.revision;
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(egui::RawInput::default(), |c| window(c, &mut e, ViewKey::ThreeD));
        assert!(state(|s| s.gone && s.dirty && s.view == Some(v) && s.switched.is_none()));
        assert!(!save_drafts(&mut e), "project Save refuses");
        assert_eq!(e.revision, rev, "nothing written");
        discard_drafts();
        assert!(state(|s| !s.gone && !s.dirty && s.view.is_none()));
        state(|s| *s = Win::default());
    }

    fn state<R>(f: impl FnOnce(&mut Win) -> R) -> R {
        WIN.with(|w| f(&mut w.borrow_mut()))
    }

    #[test]
    fn project_save_commits_the_view_draft_or_opens_the_window() {
        let mut e = Editor::new_in_memory().unwrap();
        let v = ViewKey::Plan(e.level);
        seed_draft_for_test(&e);
        let want = state(|s| s.draft.detail);
        assert_eq!(drafts(false).0, Some(view_label_in(false, &e, v)));
        assert_eq!(drafts(true).0, Some(view_label_in(true, &e, v)));
        // an unreadable range field refuses: the window opens, the model and draft stay
        state(|s| s.range_err = Some("bad".into()));
        assert!(!save_drafts(&mut e));
        assert!(state(|s| s.open), "the refusal shows the window");
        assert!(drafts(false).0.is_some(), "the draft is kept");
        assert_ne!(e.model.view_gfx(v).unwrap().effective.detail, want);
        state(|s| s.range_err = None);
        assert!(save_drafts(&mut e));
        assert_eq!(e.model.view_gfx(v).unwrap().effective.detail, want);
        assert_eq!(drafts(false), (None, None, false));
        e.undo();
        assert_ne!(e.model.view_gfx(v).unwrap().effective.detail, want, "one undo step");
    }

    #[test]
    fn project_save_commits_the_filter_or_keeps_its_editor() {
        let mut e = Editor::new_in_memory().unwrap();
        seed_filter_draft_for_test("Zz F");
        assert_eq!(drafts(false).1.as_deref(), Some("Zz F"));
        state(|s| s.editor.as_mut().unwrap().spec.rules[0].param.clear());
        assert!(!save_drafts(&mut e), "a rule without a parameter is refused");
        assert!(state(|s| s.open && s.editor.as_ref().is_some_and(|d| d.error.is_some())));
        assert!(!e.model.view_filters().unwrap().iter().any(|f| f.spec.name == "Zz F"));
        state(|s| s.editor.as_mut().unwrap().spec.rules[0].param = "category".into());
        assert!(save_drafts(&mut e));
        assert!(e.model.view_filters().unwrap().iter().any(|f| f.spec.name == "Zz F"));
        assert_eq!(drafts(false), (None, None, false));
    }

    #[test]
    fn project_save_and_discard_of_object_styles() {
        let mut e = Editor::new_in_memory().unwrap();
        let cat = GFX_CATEGORIES[0].0;
        seed_styles_draft_for_test(&e);
        assert!(drafts(false).2);
        discard_drafts();
        assert_eq!(drafts(false), (None, None, false), "Discard drops the edit");
        seed_styles_draft_for_test(&e);
        assert!(save_drafts(&mut e));
        assert_eq!(e.model.object_styles().unwrap().get(cat).and_then(|o| o.line), Some(0x12_34_56));
        assert!(!drafts(false).2);
    }

    #[test]
    fn discard_drops_view_and_filter_drafts() {
        let e = Editor::new_in_memory().unwrap();
        seed_draft_for_test(&e);
        seed_filter_draft_for_test("Zz F");
        assert!(matches!(drafts(false), (Some(_), Some(_), false)));
        discard_drafts();
        assert_eq!(drafts(false), (None, None, false));
    }

    fn col(c: u32) -> GfxOverride {
        GfxOverride {
            line: Some(c),
            ..Default::default()
        }
    }

    fn line_of(e: &Editor, cat: &str) -> Option<u32> {
        e.model.object_styles().unwrap().get(cat).and_then(|o| o.line)
    }

    fn draft_line(cat: &str) -> Option<u32> {
        state(|s| s.styles.get(cat).and_then(|o| o.line))
    }

    /// One frame of the object styles window, open or closed.
    fn styles_frame(e: &mut Editor) {
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(egui::RawInput::default(), |c| styles_window(c, &mut *e));
    }

    /// The first category's colour applied, a new colour typed over it, then the apply undone.
    fn conflicted(e: &mut Editor) {
        let a = GFX_CATEGORIES[0].0;
        seed_styles_draft_for_test(e);
        assert!(save_drafts(e));
        state(|s| {
            s.styles.insert(a.to_string(), col(0x65_43_21));
        });
        e.undo();
        assert_ne!(line_of(e, a), Some(0x12_34_56));
        styles_frame(e);
    }

    #[test]
    fn unrelated_model_changes_keep_object_style_edits() {
        let mut e = Editor::new_in_memory().unwrap();
        let (a, b) = (GFX_CATEGORIES[0].0, GFX_CATEGORIES[1].0);
        seed_styles_draft_for_test(&e);
        assert!(e.add_level("Zz L", 9.0).is_some());
        assert!(run(&mut e, move |m| m.set_object_styles(&[(b.to_string(), col(0x0a_0b_0c))])));
        // the window is closed, but it holds edits: a frame reads the model anew
        styles_frame(&mut e);
        let key = e.cache_key();
        assert!(state(|s| s.styles_rev == Some(key) && !s.styles_conflict));
        assert_eq!(draft_line(a), Some(0x12_34_56), "the edit is kept");
        assert_eq!(draft_line(b), Some(0x0a_0b_0c), "other categories follow the model");
        assert!(drafts(false).2);
        assert!(save_drafts(&mut e));
        assert_eq!(line_of(&e, a), Some(0x12_34_56));
        assert_eq!(line_of(&e, b), Some(0x0a_0b_0c), "the other change is not written back");
        assert!(!drafts(false).2);
        state(|s| *s = Win::default());
    }

    #[test]
    fn undo_of_an_edited_style_asks_keeps_edits_and_blocks_apply() {
        let mut e = Editor::new_in_memory().unwrap();
        let a = GFX_CATEGORIES[0].0;
        conflicted(&mut e);
        assert!(state(|s| s.styles_conflict));
        assert_eq!(draft_line(a), Some(0x65_43_21), "nothing is dropped silently");
        assert!(drafts(false).2);
        let all = crate::project_ui::drafts::list(&e.model, false);
        assert!(all.contains(&crate::project_ui::drafts::Draft::ObjectStyles), "close asks");
        let rev = e.revision;
        assert!(!save_drafts(&mut e), "Save from the close question stops on the conflict");
        assert_eq!(e.revision, rev);
        assert_ne!(line_of(&e, a), Some(0x65_43_21), "nothing written over the change");
        assert!(state(|s| s.styles_open && s.styles_conflict), "the window shows it first");
        // drawing the note and its buttons does not answer it
        styles_frame(&mut e);
        styles_frame(&mut e);
        assert!(state(|s| s.styles_conflict));
        assert_eq!(e.revision, rev, "Apply stays blocked");
        // Keep my edits answers it: Save then writes them in one undo step
        state(|s| s.styles_conflict = false);
        assert!(save_drafts(&mut e));
        assert_eq!(line_of(&e, a), Some(0x65_43_21));
        assert!(!drafts(false).2);
        e.undo();
        assert_ne!(line_of(&e, a), Some(0x65_43_21));
        state(|s| *s = Win::default());
    }

    #[test]
    fn reload_shows_the_stored_styles_and_drops_edits() {
        let mut e = Editor::new_in_memory().unwrap();
        let a = GFX_CATEGORIES[0].0;
        conflicted(&mut e);
        state(reload_styles);
        assert!(!state(|s| s.styles_conflict));
        assert_eq!(draft_line(a), line_of(&e, a));
        assert!(!drafts(false).2);
        state(|s| *s = Win::default());
    }

    #[test]
    fn own_apply_and_undo_of_a_clean_window_ask_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let a = GFX_CATEGORIES[0].0;
        seed_styles_draft_for_test(&e);
        state(|s| s.styles_open = true);
        assert!(save_drafts(&mut e));
        styles_frame(&mut e);
        assert!(!state(|s| s.styles_conflict), "own Apply is no conflict");
        e.undo();
        styles_frame(&mut e);
        assert!(!state(|s| s.styles_conflict), "a window without edits just follows");
        assert_eq!(draft_line(a), line_of(&e, a));
        assert!(!drafts(false).2);
        state(|s| *s = Win::default());
    }

    #[test]
    fn swap_model_drops_object_style_edits_and_conflicts() {
        let mut e = Editor::new_in_memory().unwrap();
        conflicted(&mut e);
        assert!(drafts(false).2);
        let fresh = Editor::new_in_memory().unwrap().model;
        e.replace_model(fresh).unwrap();
        assert!(state(|s| s.styles_rev.is_none() && !s.styles_conflict));
        assert_eq!(drafts(false), (None, None, false));
        assert_eq!(crate::project_ui::drafts::count(&e), 0, "no stale draft after the swap");
        state(|s| *s = Win::default());
    }

    #[test]
    fn object_styles_read_anew_when_an_older_revision_is_put_back() {
        let mut e = Editor::new_in_memory().unwrap();
        let (a, b) = (GFX_CATEGORIES[0].0, GFX_CATEGORIES[1].0);
        seed_styles_draft_for_test(&e);
        let rev = e.revision;
        assert!(run(&mut e, move |m| m.set_object_styles(&[(b.to_string(), col(0x0a_0b_0c))])));
        e.restore_revision(rev);
        styles_frame(&mut e);
        assert_eq!(draft_line(b), Some(0x0a_0b_0c), "keyed on cache_key");
        assert_eq!(draft_line(a), Some(0x12_34_56));
        assert!(!state(|s| s.styles_conflict));
        state(|s| *s = Win::default());
    }

    #[test]
    fn rebase_asks_only_when_an_edited_category_changed_to_something_else() {
        let a = GFX_CATEGORIES[0].0.to_string();
        let m = |v: &[(String, GfxOverride)]| v.iter().cloned().collect::<BTreeMap<_, _>>();
        let saved = m(&[(a.clone(), col(1))]);
        let removed = BTreeMap::new();
        let (out, conflict) = rebase_map(&saved, &removed, &m(&[(a.clone(), col(2))]));
        assert!(conflict && out.is_empty(), "a removal kept over a changed style");
        let (_, conflict) = rebase_map(&saved, &removed, &BTreeMap::new());
        assert!(!conflict, "the model already holds the edit");
        let typed = m(&[(a.clone(), col(3))]);
        let (out, conflict) = rebase_map(&saved, &typed, &saved);
        assert!(!conflict);
        assert_eq!(out, typed);
        let other = m(&[(a.clone(), col(1)), ("door".to_string(), col(4))]);
        let (out, conflict) = rebase_map(&saved, &typed, &other);
        assert!(!conflict, "only an unrelated row changed");
        assert_eq!(out.get("door"), Some(&col(4)));
        assert_eq!(out.get(&a), Some(&col(3)));
    }

    fn hidden() -> CatGfx {
        CatGfx {
            visible: false,
            ..Default::default()
        }
    }

    /// A window on the plan of `e.level` whose first category is hidden in the draft.
    fn edited_plan(e: &Editor) -> (Win, ViewKey) {
        let mut st = Win::default();
        let v = ViewKey::Plan(e.level);
        sync(&mut st, e, v);
        assert!(vv_rows::set_cat(&mut st.draft, GFX_CATEGORIES[0].0, hidden()));
        st.dirty = true;
        (st, v)
    }

    /// The plan's detail applied as Fine, Coarse typed over it, then the apply undone.
    fn detail_conflict(e: &mut Editor) -> (Win, ViewKey) {
        let v = ViewKey::Plan(e.level);
        assert_eq!(e.model.view_gfx(v).unwrap().effective.detail, DetailLevel::Medium);
        assert!(run(e, move |m| m.edit_view_gfx(v, |g| g.detail = DetailLevel::Fine)));
        let mut st = Win::default();
        sync(&mut st, e, v);
        st.draft.detail = DetailLevel::Coarse;
        st.dirty = true;
        e.undo();
        sync(&mut st, e, v);
        (st, v)
    }

    fn detail_of(e: &Editor, v: ViewKey) -> DetailLevel {
        e.model.view_gfx(v).unwrap().effective.detail
    }

    #[test]
    fn unrelated_model_changes_keep_view_graphics_edits() {
        let mut e = Editor::new_in_memory().unwrap();
        let (a, b) = (GFX_CATEGORIES[0].0, GFX_CATEGORIES[1].0);
        let (mut st, v) = edited_plan(&e);
        assert!(e.add_level("Zz L", 9.0).is_some());
        assert!(run(&mut e, move |m| m.edit_view_gfx(v, |g| {
            g.detail = DetailLevel::Fine;
            g.categories.insert(b.to_string(), hidden());
        })));
        sync(&mut st, &e, v);
        assert!(st.dirty && !st.conflict, "nothing asked");
        assert_eq!(st.draft.categories.get(a), Some(&hidden()), "the edit is kept");
        assert_eq!(st.draft.categories.get(b), Some(&hidden()), "other rows follow the model");
        assert_eq!(st.draft.detail, DetailLevel::Fine);
        assert!(commit(&mut e, &mut st));
        let g = e.model.view_gfx(v).unwrap().effective;
        assert_eq!(g.categories.get(a), Some(&hidden()));
        assert_eq!(g.categories.get(b), Some(&hidden()), "the other change is not written back");
        assert_eq!(g.detail, DetailLevel::Fine);
    }

    #[test]
    fn undo_of_an_edited_view_row_asks_keeps_edits_and_blocks_save() {
        let mut e = Editor::new_in_memory().unwrap();
        let (st, v) = detail_conflict(&mut e);
        assert!(st.conflict && st.dirty);
        assert_eq!(st.draft.detail, DetailLevel::Coarse, "nothing is dropped silently");
        state(|s| *s = st);
        assert!(drafts(false).0.is_some(), "close asks");
        let rev = e.revision;
        assert!(!save_drafts(&mut e), "Save from the close question stops on the conflict");
        assert_eq!(e.revision, rev);
        assert_eq!(detail_of(&e, v), DetailLevel::Medium, "nothing written over the change");
        assert!(state(|s| s.open && s.conflict), "the window shows it first");
        // drawing the note and its buttons does not answer it, and Apply stays blocked
        let ctx = ddc_ui::theme::headless_ctx();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |c| window(c, &mut e, v));
        }
        assert!(state(|s| s.conflict && s.draft.detail == DetailLevel::Coarse));
        assert_eq!(e.revision, rev);
        // Keep my edits answers it: Save then writes them in one undo step
        state(|s| s.conflict = false);
        assert!(save_drafts(&mut e));
        assert_eq!(detail_of(&e, v), DetailLevel::Coarse);
        assert_eq!(drafts(false), (None, None, false));
        e.undo();
        assert_eq!(detail_of(&e, v), DetailLevel::Medium);
        state(|s| *s = Win::default());
    }

    #[test]
    fn reload_shows_the_stored_view_graphics_and_drops_edits() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = detail_conflict(&mut e);
        st.range_text[0] = "abc".into();
        st.range_err = Some("bad".into());
        reload_view(&mut st);
        assert!(!st.conflict && !st.dirty && st.range_err.is_none());
        assert_eq!(st.draft, e.model.view_gfx(v).unwrap().effective);
        assert_eq!(st.range_text, range_texts(st.draft.range, active_units()));
    }

    #[test]
    fn own_apply_and_undo_of_a_clean_view_window_ask_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = edited_plan(&e);
        assert!(commit(&mut e, &mut st));
        sync(&mut st, &e, v);
        assert!(!st.conflict && !st.dirty, "own Apply is no conflict");
        e.undo();
        sync(&mut st, &e, v);
        assert!(!st.conflict && !st.dirty, "a window without edits just follows");
        assert_eq!(st.draft, e.model.view_gfx(v).unwrap().effective);
    }

    fn filter_spec(name: &str) -> ViewFilterSpec {
        ViewFilterSpec {
            name: name.into(),
            rules: vec![FilterRule {
                param: "category".into(),
                op: RuleOp::Eq,
                value: "wall".into(),
            }],
            ..Default::default()
        }
    }

    /// Adds a project filter named `name` and returns its id.
    fn add_filter(e: &mut Editor, name: &str) -> i64 {
        let s = filter_spec(name);
        assert!(run(e, move |m| m.add_view_filter(&s).map(|_| ())));
        let all = e.model.view_filters().unwrap();
        all.into_iter().find(|f| f.spec.name == name).unwrap().id
    }

    #[test]
    fn the_filter_list_is_one_row_and_filter_rules_are_unrelated() {
        let mut e = Editor::new_in_memory().unwrap();
        let (fa, fb) = (add_filter(&mut e, "Zz A"), add_filter(&mut e, "Zz B"));
        let v = ViewKey::Plan(e.level);
        let mut st = Win::default();
        sync(&mut st, &e, v);
        st.draft.filters.push(new_use(fa));
        st.dirty = true;
        let renamed = filter_spec("Zz B2");
        assert!(run(&mut e, move |m| m.edit_view_filter(fb, &renamed)));
        sync(&mut st, &e, v);
        assert!(!st.conflict, "a filter's rules are not this view's rows");
        assert_eq!(st.draft.filters, vec![new_use(fa)]);
        assert!(st.filters.iter().any(|f| f.spec.name == "Zz B2"), "the library follows");
        assert!(run(&mut e, move |m| m.edit_view_gfx(v, |g| g.filters.push(new_use(fb)))));
        sync(&mut st, &e, v);
        assert!(st.conflict, "the view's filter list changed under the edit");
        assert_eq!(st.draft.filters, vec![new_use(fa)], "kept until answered");
    }

    #[test]
    fn deleting_a_filter_from_this_window_is_no_conflict() {
        let mut e = Editor::new_in_memory().unwrap();
        let fa = add_filter(&mut e, "Zz A");
        let v = ViewKey::Plan(e.level);
        assert!(run(&mut e, move |m| m.edit_view_gfx(v, |g| g.filters.push(new_use(fa)))));
        let mut st = Win::default();
        sync(&mut st, &e, v);
        st.draft.detail = DetailLevel::Coarse;
        st.draft.filters[0].visible = false;
        st.dirty = true;
        assert!(run(&mut e, move |m| m.delete_view_filter(fa)));
        drop_filter(&mut st, fa);
        sync(&mut st, &e, v);
        assert!(!st.conflict && st.dirty);
        assert!(st.draft.filters.is_empty());
        assert_eq!(st.draft.detail, DetailLevel::Coarse, "the other edit is kept");
    }

    #[test]
    fn view_range_follows_the_model_unless_typed() {
        let mut e = Editor::new_in_memory().unwrap();
        let (mut st, v) = edited_plan(&e);
        let high = ViewRange {
            cut: 2.5,
            ..Default::default()
        };
        assert!(run(&mut e, move |m| m.set_view_range(v, high)));
        sync(&mut st, &e, v);
        assert!(!st.conflict && st.dirty);
        assert_eq!(st.draft.range.cut, 2.5);
        assert_eq!(st.range_text, range_texts(st.draft.range, active_units()), "fields follow");
        st.range_text[0] = "abc".into();
        st.range_err = Some("bad".into());
        let low = ViewRange {
            cut: 1.5,
            ..Default::default()
        };
        assert!(run(&mut e, move |m| m.set_view_range(v, low)));
        sync(&mut st, &e, v);
        assert!(st.conflict, "typed fields over a changed range ask");
        assert_eq!(st.range_text[0], "abc", "the typed text stays");
    }

    #[test]
    fn swap_model_drops_view_graphics_edits_and_conflicts() {
        let mut e = Editor::new_in_memory().unwrap();
        let (st, _) = detail_conflict(&mut e);
        state(|s| *s = st);
        seed_filter_draft_for_test("Zz F");
        assert!(matches!(drafts(false), (Some(_), Some(_), false)));
        let fresh = Editor::new_in_memory().unwrap().model;
        e.replace_model(fresh).unwrap();
        assert!(state(|s| s.rev.is_none() && !s.conflict && !s.dirty && s.view.is_none()));
        assert_eq!(drafts(false), (None, None, false));
        assert_eq!(crate::project_ui::drafts::count(&e), 0, "no stale draft after the swap");
        state(|s| *s = Win::default());
    }

    #[test]
    fn view_graphics_conflict_strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for k in [
                "vv_conflict.vv.conflict",
                "vv_conflict.styles.keep",
                "vv_conflict.styles.reload",
                "vv_conflict.styles.answer_first",
            ] {
                assert_ne!(c.t(k), k, "missing {k}");
            }
        }
    }

    #[test]
    fn object_styles_conflict_strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for k in [
                "vv_conflict.styles.conflict",
                "vv_conflict.styles.keep",
                "vv_conflict.styles.reload",
                "vv_conflict.styles.answer_first",
            ] {
                assert_ne!(c.t(k), k, "missing {k}");
            }
        }
    }
}
