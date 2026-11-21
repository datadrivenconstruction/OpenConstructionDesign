// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Find and replace window (Ctrl+F find only, Ctrl+H with the replace row): text notes, room names and numbers, marks, comments and text
//! parameter values. The hit list is the preview (hover shows the new text); a click selects the
//! element and brings it into view; replacing is one `run_op`, so one undo step. Pinned elements
//! and locked values are shown as such and refused with a count.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;
use crate::ui_kit::{
    cancel_button, dialog_footer, empty_note, fixed_cell, labeled_row_id, primary_click_tip,
    Dialog, DialogSize, ROW_LABEL_W,
};
use ddc_ui::widgets::type_pick::{Pick, PickItem, PickTexts, TypePick};
use ddc_ui::widgets::{dropdown, TextField};
use ddc_ui::Theme;
use ddc_geom::v2;
use ddc_model::{
    replace_report, replaced_text, FindField, FindHit, FindOpts, HitKey, Model, ReplaceReport,
    PARAM_CATEGORIES,
};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::{icon_size, layout, sem_of, space};
use egui::{self, text::LayoutJob, Sense, TextFormat, TextStyle, Vec2};

/// Everything the window remembers between frames.
#[derive(Default)]
struct FindUi {
    open: bool,
    focus: bool,
    /// Replace row and replace buttons shown (Ctrl+H); Ctrl+F opens with them collapsed.
    replace_mode: bool,
    query: String,
    with: String,
    match_case: bool,
    whole_word: bool,
    category: Option<&'static str>,
    level: Option<i64>,
    hits: Vec<FindHit>,
    /// Inputs and model revision the hit list was made for.
    stamp: Option<Stamp>,
    sel: Option<HitKey>,
    msg: Option<String>,
    err: Option<String>,
}

type Stamp = (String, bool, bool, Option<&'static str>, Option<i64>, u64);

thread_local! {
    static ST: RefCell<FindUi> = RefCell::new(FindUi::default());
}

/// Where to take the view after a click on a hit.
#[derive(Clone, Debug, PartialEq)]
struct Goto {
    table: String,
    id: i64,
    level: Option<i64>,
    pos: Option<(f64, f64)>,
}

enum Act {
    Select,
    All,
    One(HitKey),
}

impl FindUi {
    fn opts(&self) -> FindOpts {
        FindOpts {
            query: self.query.clone(),
            match_case: self.match_case,
            whole_word: self.whole_word,
            category: self.category.map(str::to_string),
            level: self.level,
        }
    }

    fn refresh(&mut self, ed: &Editor) {
        let stamp = (
            self.query.clone(),
            self.match_case,
            self.whole_word,
            self.category,
            self.level,
            ed.revision,
        );
        if self.stamp.as_ref() == Some(&stamp) {
            return;
        }
        match ed.model.find_text(&self.opts()) {
            Ok(h) => {
                self.hits = h;
                self.err = None;
            }
            Err(e) => {
                self.hits.clear();
                self.err = Some(e.to_string());
            }
        }
        if self
            .sel
            .as_ref()
            .is_some_and(|k| !self.hits.iter().any(|h| &h.hit_key() == k))
        {
            self.sel = None;
        }
        self.stamp = Some(stamp);
    }

    fn goto_of(&mut self, i: usize) -> Goto {
        let h = &self.hits[i];
        self.sel = Some(h.hit_key());
        Goto {
            table: h.table.clone(),
            id: h.id,
            level: h.level,
            pos: h.pos,
        }
    }
}

fn open_with(replace_mode: bool) {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.open = true;
        s.focus = true;
        s.replace_mode = replace_mode;
    });
}

/// Ctrl+H: find with the replace row.
fn h_open(_: &mut EditorApp) {
    open_with(true);
}

/// Ctrl+F: find only, focus in the find field.
fn h_find(_: &mut EditorApp) {
    open_with(false);
}

/// Registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("app.find", h_find);
    app.register_command("edit.find_replace", h_open);
}

/// (open, replace row shown, focus requested) for tests.
#[cfg(test)]
fn state() -> (bool, bool, bool) {
    ST.with(|s| {
        let s = s.borrow();
        (s.open, s.replace_mode, s.focus)
    })
}

/// Fresh window state for the layout probes: mode, find text, replacement.
#[cfg(test)]
pub(crate) fn open_for_layout(replace_mode: bool, query: &str, with: &str) {
    ST.with(|s| {
        *s.borrow_mut() = FindUi {
            open: true,
            replace_mode,
            query: query.into(),
            with: with.into(),
            ..FindUi::default()
        };
    });
}

fn skipped(c: &Catalog, r: &ReplaceReport) -> String {
    let mut s = String::new();
    for (k, n) in [
        ("find_replace.skip_pinned", r.pinned),
        ("find_replace.skip_locked", r.locked),
        ("find_replace.skip_invalid", r.invalid),
    ] {
        if n > 0 {
            s += &c.tf(k, &[("k", &n.to_string())]);
        }
    }
    s
}

fn counts(c: &Catalog, key: &str, r: &ReplaceReport) -> String {
    c.tf(
        key,
        &[
            ("n", &r.replaced.to_string()),
            ("m", &r.elements.to_string()),
        ],
    ) + &skipped(c, r)
}

fn field_label(c: &Catalog, h: &FindHit) -> String {
    match h.field {
        FindField::Note => c.t("find_replace.field.note"),
        FindField::RoomName => c.t("find_replace.field.room_name"),
        FindField::RoomNumber => c.t("find_replace.field.room_number"),
        FindField::Prop => match h.key.to_lowercase().as_str() {
            "mark" => c.t("find_replace.field.mark"),
            "comments" => c.t("find_replace.field.comments"),
            _ => h.key.clone(),
        },
        FindField::Param => h.key.clone(),
    }
}

fn category_label(c: &Catalog, name: &str) -> String {
    match name {
        "annotation" | "ramp" => c.t(&format!("find_replace.cat.{name}")),
        _ => c.t(&format!("view_gfx.cat.{name}")),
    }
}

/// Category choices: all, notes, then the element categories once each.
fn categories(c: &Catalog) -> Vec<(Option<&'static str>, String)> {
    let mut out = vec![
        (None, c.t("find_replace.all_categories")),
        (Some("annotation"), category_label(c, "annotation")),
    ];
    for (name, _) in PARAM_CATEGORIES {
        if !out.iter().any(|x| x.0 == Some(name)) {
            out.push((Some(name), category_label(c, name)));
        }
    }
    out
}

/// Search glyph and text slots of the find field, as in the other search fields.
const SEARCH_ICON_X: f32 = space::XS;
const SEARCH_TEXT_X: f32 = space::XS + icon_size::SM + space::SM;

/// Lists longer than this get a search box in the popup.
const SEARCH_ABOVE: usize = 12;

/// One-of-many choice: a plain dropdown, a searchable picker above `SEARCH_ABOVE` items.
fn choice<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    salt: &str,
    cur: &mut T,
    items: &[(T, String)],
) {
    let w = ui.available_width();
    let at = items.iter().position(|x| x.0 == *cur).unwrap_or(0);
    if items.len() > SEARCH_ABOVE {
        let rows: Vec<PickItem> = items
            .iter()
            .enumerate()
            .map(|(i, x)| PickItem {
                id: i as i64,
                name: x.1.clone(),
                detail: String::new(),
            })
            .collect();
        let texts = PickTexts {
            search: c.t("find_replace.pick_search"),
            empty: c.t("find_replace.pick_empty"),
            edit: String::new(),
            mixed: String::new(),
        };
        if let Some(Pick::Id(i)) = TypePick::new(salt, &rows, Some(at as i64))
            .texts(texts)
            .width(w)
            .show(ui, th)
        {
            if let Some(x) = items.get(i as usize) {
                *cur = x.0;
            }
        }
    } else {
        let names: Vec<&str> = items.iter().map(|x| x.1.as_str()).collect();
        let mut sel = at;
        if dropdown(ui, th, salt, &mut sel, &names, Some(w)).changed() {
            if let Some(x) = items.get(sel) {
                *cur = x.0;
            }
        }
    }
}

/// What the footer buttons are, in order from the far right.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Btn {
    SelectAll,
    ShowReplace,
    ReplaceAll,
    ReplaceOne,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Foot {
    btn: Btn,
    primary: bool,
    enabled: bool,
}

/// Footer buttons for the mode: one primary, first. Find mode: Select all; replace mode:
/// Replace all, the selected hit, Select all.
fn footer_plan(replace_mode: bool, matched: usize, replaced: usize, can_one: bool) -> Vec<Foot> {
    let f = |btn, primary, enabled| Foot { btn, primary, enabled };
    if replace_mode {
        vec![
            f(Btn::ReplaceAll, true, replaced > 0),
            f(Btn::ReplaceOne, false, can_one),
            f(Btn::SelectAll, false, matched > 0),
        ]
    } else {
        vec![
            f(Btn::SelectAll, true, matched > 0),
            f(Btn::ShowReplace, false, true),
        ]
    }
}

/// Count line under the list: the outcome of the last step, else the preview of Replace all.
fn status_line(c: &Catalog, st: &FindUi, all: &ReplaceReport) -> Option<String> {
    if let Some(m) = &st.msg {
        Some(m.clone())
    } else if st.replace_mode && !st.hits.is_empty() {
        let key = if all.replaced > 0 {
            "find_replace.will"
        } else {
            "find_replace.nothing"
        };
        Some(counts(c, key, all))
    } else {
        None
    }
}

/// One line of text cut to `width` with an ellipsis.
fn line(
    ui: &egui::Ui,
    text: String,
    style: TextStyle,
    width: f32,
    ink: egui::Color32,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text, style.resolve(ui.style()), ink);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// One line of the hit text, matches marked; line breaks shown as spaces.
fn snippet(
    ui: &egui::Ui,
    h: &FindHit,
    width: f32,
    ink: egui::Color32,
    mark: egui::Color32,
) -> Arc<egui::Galley> {
    let font = TextStyle::Body.resolve(ui.style());
    let plain = TextFormat::simple(font.clone(), ink);
    let marked = TextFormat {
        background: mark,
        ..TextFormat::simple(font, ink)
    };
    let mut job = LayoutJob::default();
    let mut at = 0;
    for &(a, b) in &h.spans {
        job.append(&h.text[at..a].replace('\n', " "), 0.0, plain.clone());
        job.append(&h.text[a..b].replace('\n', " "), 0.0, marked.clone());
        at = b;
    }
    job.append(&h.text[at..].replace('\n', " "), 0.0, plain);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// Tag and tooltip of a value replace leaves alone.
fn refusal(c: &Catalog, h: &FindHit) -> Option<(String, String)> {
    if h.pinned {
        Some((c.t("find_replace.pinned"), c.t("find_replace.pinned_tip")))
    } else if h.locked {
        Some((c.t("find_replace.locked"), c.t("find_replace.locked_tip")))
    } else {
        None
    }
}

fn list(ui: &mut egui::Ui, c: &Catalog, st: &mut FindUi, goto: &mut Option<Goto>) {
    let s = sem_of(ui.ctx());
    let row_h = ui.text_style_height(&TextStyle::Body) + 2.0 * space::XS;
    let label_w = layout::PROP_KEY_MIN + space::XL;
    // room below for the summary line and the footer
    let height = (ui.available_height() - space::XXXL - space::LG).max(row_h * 3.0);
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(height)
        .show_rows(ui, row_h, st.hits.len(), |ui, range| {
            for i in range {
                let h = &st.hits[i];
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_h),
                    Sense::click(),
                );
                let selected = st.sel.as_ref() == Some(&h.hit_key());
                let p = ui.painter();
                if selected {
                    p.rect_filled(rect, space::XXS, s.selection_bg);
                } else if resp.hovered() {
                    p.rect_filled(rect, space::XXS, s.hover);
                }
                let y = rect.center().y;
                let inner = rect.shrink2(Vec2::new(space::SM, 0.0));
                let label = line(
                    ui,
                    field_label(c, h),
                    TextStyle::Small,
                    label_w - space::SM,
                    s.text_secondary,
                );
                p.galley(
                    egui::pos2(inner.left(), y - label.size().y / 2.0),
                    label,
                    s.text_secondary,
                );
                let refused = refusal(c, h);
                let mut right = inner.right();
                if let Some((tag, _)) = &refused {
                    let g = line(ui, tag.clone(), TextStyle::Small, label_w, s.text_secondary);
                    right -= g.size().x;
                    p.galley(egui::pos2(right, y - g.size().y / 2.0), g, s.text_secondary);
                    right -= space::SM;
                }
                let left = inner.left() + label_w;
                let g = snippet(ui, h, right - left, s.text_primary, s.accent_subtle);
                p.galley(egui::pos2(left, y - g.size().y / 2.0), g, s.text_primary);
                let resp = resp.on_hover_ui(|ui| {
                    ui.set_max_width(layout::TOOLTIP_W);
                    match &refused {
                        Some((_, tip)) => {
                            ui.label(tip);
                            ui.label(&h.text);
                        }
                        None => {
                            ui.label(&h.text);
                            ui.label(format!("\u{2192} {}", replaced_text(h, &st.with)));
                        }
                    }
                });
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    if let Some(i) = clicked {
        *goto = Some(st.goto_of(i));
    }
}

fn form(ui: &mut egui::Ui, c: &Catalog, ed: &Editor, st: &mut FindUi, goto: &mut Option<Goto>) {
    let th = ddc_ui::theme::of(ui.ctx());
    let levels: Vec<(Option<i64>, String)> = std::iter::once((None, c.t("find_replace.all_levels")))
        .chain(
            ed.model
                .level_list()
                .unwrap_or_default()
                .into_iter()
                .map(|l| (Some(l.0), l.1)),
        )
        .collect();
    let cats = categories(c);
    let (find_label, find_hint, find_clear) =
        (c.t("find_replace.find"), c.t("find_replace.find_hint"), c.t("find_replace.find_clear"));
    labeled_row_id(ui, &find_label, |ui, label_id| {
        let r = TextField::new(&mut st.query)
            .id_salt("fnd1.query")
            .hint(&find_hint)
            .search("find", SEARCH_ICON_X, SEARCH_TEXT_X, &find_clear)
            .labelled_by(label_id, &find_label)
            .width(ui.available_width())
            .show(ui, &th)
            .response;
        if std::mem::take(&mut st.focus) {
            r.request_focus();
        }
        // Enter in the field steps to the next hit and the focus stays
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            st.refresh(ed);
            let next = st
                .sel
                .as_ref()
                .and_then(|k| st.hits.iter().position(|h| &h.hit_key() == k))
                .map_or(0, |i| i + 1);
            if !st.hits.is_empty() {
                *goto = Some(st.goto_of(next % st.hits.len()));
            }
            r.request_focus();
        }
    });
    if st.replace_mode {
        let (with_label, with_hint) = (c.t("find_replace.replace_with"), c.t("find_replace.replace_hint"));
        labeled_row_id(ui, &with_label, |ui, label_id| {
            TextField::new(&mut st.with)
                .id_salt("fnd1.with")
                .hint(&with_hint)
                .labelled_by(label_id, &with_label)
                .width(ui.available_width())
                .show(ui, &th);
        });
    }
    ui.horizontal(|ui| {
        // value column, no empty caption
        // the same label-to-field gap as the form rows, whatever the item spacing is: the
        // empty cell takes the gap the item spacing adds after it
        let gap = ui.spacing().item_spacing.x;
        fixed_cell(ui, ROW_LABEL_W + space::SM - gap, |_| {});
        ddc_ui::widgets::checkbox(ui, &th, &mut st.match_case, &c.t("find_replace.match_case"));
        ui.add_space(space::MD);
        ddc_ui::widgets::checkbox(ui, &th, &mut st.whole_word, &c.t("find_replace.whole_word"));
    });
    labeled_row_id(ui, &c.t("find_replace.category"), |ui, _| {
        choice(ui, &th, c, "find_replace.cat", &mut st.category, &cats);
    });
    labeled_row_id(ui, &c.t("find_replace.level"), |ui, _| {
        choice(ui, &th, c, "find_replace.level", &mut st.level, &levels);
    });
}

fn body(
    ui: &mut egui::Ui,
    c: &Catalog,
    ed: &Editor,
    st: &mut FindUi,
    acts: &mut Vec<Act>,
    goto: &mut Option<Goto>,
    close: &mut bool,
) {
    form(ui, c, ed, st, goto);
    st.refresh(ed);
    ui.add_space(space::XS);
    ui.separator();
    let s = sem_of(ui.ctx());
    if let Some(e) = &st.err {
        crate::ui_kit::error_note(ui, e);
    } else if st.query.is_empty() {
        empty_note(ui, c.t("find_replace.empty_title"), Some(c.t("find_replace.empty_hint").as_str()));
    } else if st.hits.is_empty() {
        empty_note(ui, c.t("find_replace.none_title"), Some(c.t("find_replace.none_hint").as_str()));
    } else {
        let rows: HashSet<_> = st.hits.iter().map(|h| (h.table.as_str(), h.id)).collect();
        let n = st.hits.len().to_string();
        let m = rows.len().to_string();
        ui.label(
            egui::RichText::new(c.tf("find_replace.found", &[("n", &n), ("m", &m)]))
                .color(s.text_secondary),
        );
        list(ui, c, st, goto);
    }
    let all = replace_report(&st.hits, &st.with, None);
    let one = st.sel.clone().map(|k| {
        let r = replace_report(&st.hits, &st.with, Some(std::slice::from_ref(&k)));
        (k, r)
    });
    if let Some(m) = status_line(c, st, &all) {
        ui.label(egui::RichText::new(m).color(s.text_secondary));
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let n_elems = st
        .hits
        .iter()
        .map(|h| (h.table.as_str(), h.id))
        .collect::<HashSet<_>>()
        .len();
    let can_one = one.as_ref().is_some_and(|x| x.1.replaced > 0);
    let plan = footer_plan(st.replace_mode, n_elems, all.replaced, can_one);
    dialog_footer(ui, |ui| {
        for f in plan {
            let (label, why, tip) = match f.btn {
                Btn::SelectAll => (
                    c.tf("find_replace.select_all", &[("n", &n_elems.to_string())]),
                    c.t("find_replace.none_title"),
                    c.t("find_replace.select_all_tip"),
                ),
                Btn::ShowReplace => (c.t("find_replace.show_replace"), String::new(), String::new()),
                Btn::ReplaceAll => (
                    c.tf("find_replace.replace_all", &[("n", &all.replaced.to_string())]),
                    c.t("find_replace.nothing"),
                    String::new(),
                ),
                Btn::ReplaceOne => (c.t("find_replace.replace"), String::new(), String::new()),
            };
            let clicked = if f.primary {
                primary_click_tip(ui, label, (!f.enabled).then_some(why.as_str()), &tip)
            } else {
                let r = ui
                    .add_enabled_ui(f.enabled, |ui| {
                        ddc_ui::widgets::Button::secondary(&label).show(ui, &th)
                    })
                    .inner;
                let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
                r.clicked()
            };
            if !clicked {
                continue;
            }
            match f.btn {
                Btn::SelectAll => acts.push(Act::Select),
                Btn::ShowReplace => st.replace_mode = true,
                Btn::ReplaceAll => acts.push(Act::All),
                Btn::ReplaceOne => {
                    if let Some((k, _)) = one.clone() {
                        acts.push(Act::One(k));
                    }
                }
            }
        }
        if cancel_button(ui, c.t("find_replace.close")).clicked() {
            *close = true;
        }
    });
}

/// A step that landed is reported as replaced even when the view refresh failed after it.
fn outcome_msg(c: &Catalog, committed: bool, err: Option<String>, r: &ReplaceReport) -> String {
    match (committed, err) {
        (true, None) => counts(c, "find_replace.done", r),
        (true, Some(e)) => c.tf(
            "find_replace.refresh_failed",
            &[
                ("n", &r.replaced.to_string()),
                ("m", &r.elements.to_string()),
                ("e", &e),
            ],
        ),
        (false, e) => c.tf("find_replace.err", &[("e", &e.unwrap_or_default())]),
    }
}

fn replace(ed: &mut Editor, st: &mut FindUi, c: &Catalog, only: Option<Vec<HitKey>>) {
    let pre = replace_report(&st.hits, &st.with, only.as_deref());
    st.msg = None;
    if pre.replaced == 0 {
        // only refused or unchanged values: say why, write nothing
        st.msg = Some(counts(c, "find_replace.nothing", &pre));
        return;
    }
    let (opts, with) = (st.opts(), st.with.clone());
    let cell = Arc::new(Mutex::new(ReplaceReport::default()));
    let out = cell.clone();
    let f = move |m: &mut Model| -> Result<(), ddc_model::Error> {
        let r = m.replace_text(&opts, &with, only.as_deref())?;
        if let Ok(mut g) = out.lock() {
            *g = r;
        }
        Ok(())
    };
    let top = ed.model.op_top().ok();
    let res = ed.run_op(OpFn(Arc::new(f)));
    let committed = res.is_ok() || (top.is_some() && ed.model.op_top().ok() != top);
    let r = if committed {
        cell.lock().map(|g| *g).unwrap_or(pre)
    } else {
        pre
    };
    st.msg = Some(outcome_msg(c, committed, res.err().map(|e| e.to_string()), &r));
    if committed {
        ed.status = st.msg.clone();
    }
    st.stamp = None;
}

/// Per frame from the shell: the window, then the view jump of a clicked hit.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if !st.open {
        ST.with(|s| *s.borrow_mut() = st);
        return;
    }
    let mut open = true;
    let (mut acts, mut goto, mut close) = (vec![], None, false);
    {
        let c = &a.sh.cat;
        let ed = &mut a.ed;
        Dialog::new("find_replace.find", c.t("find_replace.title"), DialogSize::Tool)
            .open(&mut open)
            .show(ctx, |ui| body(ui, c, ed, &mut st, &mut acts, &mut goto, &mut close));
        for act in acts {
            // only the hits the list showed: nothing the user has not seen is written
            let keys = match act {
                Act::Select => {
                    select_matches(ed, &st.hits);
                    continue;
                }
                Act::All => st.hits.iter().map(FindHit::hit_key).collect(),
                Act::One(k) => vec![k],
            };
            replace(ed, &mut st, c, Some(keys));
        }
    }
    st.open = open && !close;
    ST.with(|s| *s.borrow_mut() = st);
    if let Some(g) = goto {
        go(a, g);
    }
}

/// Make every matched element the selection (one entry per element).
fn select_matches(ed: &mut Editor, hits: &[FindHit]) {
    let mut seen = HashSet::new();
    let sel: Vec<_> = hits
        .iter()
        .filter(|h| seen.insert((h.table.clone(), h.id)))
        .filter_map(|h| crate::clip_tools::sel_of(&h.table, h.id))
        .collect();
    if !sel.is_empty() {
        ed.selection = sel;
    }
}

/// Select the hit's element on its level and bring it into view.
fn go(a: &mut EditorApp, g: Goto) {
    if let Some(l) = g.level {
        a.ed.set_level(l);
    }
    let Some(sel) = crate::clip_tools::sel_of(&g.table, g.id) else {
        return;
    };
    a.ed.selection = vec![sel];
    if a.view3d {
        if g.table != "anno" {
            a.run("view.zoom_sel_3d");
        }
    } else if let Some((x, y)) = g.pos {
        a.plan.center = v2(x, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn reset() {
        ST.with(|s| *s.borrow_mut() = FindUi::default());
    }

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    #[test]
    fn ctrl_f_opens_find_mode_with_focus_and_ctrl_h_replace_mode() {
        reset();
        let mut a = app();
        assert!(a.run("app.find"));
        assert_eq!(state(), (true, false, true), "find mode, focus requested");
        reset();
        assert!(a.run("edit.find_replace"));
        assert_eq!(state(), (true, true, true), "replace row shown");
        // Ctrl+H after Ctrl+F reveals the replace row; Ctrl+F after Ctrl+H collapses it
        assert!(a.run("app.find"));
        assert_eq!(state(), (true, false, true));
        reset();
    }

    #[test]
    fn find_and_replace_are_two_palette_entries_with_their_keys() {
        let mut a = app();
        let find = a.sh.reg.get("app.find").unwrap().shortcuts.clone();
        let repl = a.sh.reg.get("edit.find_replace").unwrap().shortcuts.clone();
        assert_eq!(find, vec!["Ctrl+F".to_string()]);
        assert_eq!(repl, vec!["Ctrl+H".to_string()]);
        let (lf, lr) = (a.sh.cat.t("cmd.app.find"), a.sh.cat.t("cmd.edit.find_replace"));
        assert_ne!(lf, "cmd.app.find");
        assert_ne!(lr, "cmd.edit.find_replace");
        assert_ne!(lf, lr, "labels tell the two apart");
        a.sh.palette.live = a.sh.ribbon.available.clone();
        for (id, label) in [("app.find", lf), ("edit.find_replace", lr)] {
            a.sh.palette.query = label;
            let rows = a.sh.palette.rows(&a.sh.reg, &a.sh.cat);
            assert!(rows.contains(&id.to_string()), "{id} in {rows:?}");
        }
    }

    /// Every binding of the keymap belongs to one command.
    #[test]
    fn keymap_has_no_double_binding() {
        let a = app();
        let mut seen = std::collections::BTreeMap::new();
        for c in a.sh.reg.all() {
            for s in &c.shortcuts {
                if let Some(prev) = seen.insert(s.clone(), c.id.clone()) {
                    assert_eq!(prev, c.id, "{s} bound to {prev} and {}", c.id);
                }
            }
        }
        assert_eq!(seen.get("Ctrl+F").map(String::as_str), Some("app.find"));
        assert_eq!(seen.get("Ctrl+H").map(String::as_str), Some("edit.find_replace"));
        assert_eq!(seen.get("Ctrl+Shift+H").map(String::as_str), Some("app.history"));
    }

    fn rep() -> ReplaceReport {
        ReplaceReport {
            replaced: 3,
            elements: 2,
            ..Default::default()
        }
    }

    #[test]
    fn committed_replace_reload_error_says_replaced() {
        let c = Catalog::new(Lang::En);
        let m = outcome_msg(&c, true, Some("reload".into()), &rep());
        assert_eq!(m, c.tf("find_replace.refresh_failed", &[("n", "3"), ("m", "2"), ("e", "reload")]));
        assert!(m.contains("reload") && m.contains('3'));
        assert!(!m.contains(&c.tf("find_replace.err", &[("e", "reload")])));
        assert_eq!(outcome_msg(&c, true, None, &rep()), counts(&c, "find_replace.done", &rep()));
    }

    #[test]
    fn failed_replace_writes_nothing_says_error() {
        let c = Catalog::new(Lang::En);
        let m = outcome_msg(&c, false, Some("boom".into()), &rep());
        assert_eq!(m, c.tf("find_replace.err", &[("e", "boom")]));
    }

    #[test]
    fn refusals_are_reported_by_kind() {
        let c = Catalog::new(Lang::En);
        let r = ReplaceReport {
            replaced: 1,
            elements: 1,
            pinned: 2,
            locked: 1,
            invalid: 0,
        };
        let msg = counts(&c, "find_replace.done", &r);
        assert!(msg.contains(&c.tf("find_replace.skip_pinned", &[("k", "2")])), "{msg}");
        assert!(msg.contains(&c.tf("find_replace.skip_locked", &[("k", "1")])), "{msg}");
        assert!(!msg.contains(&c.tf("find_replace.skip_invalid", &[("k", "0")])), "{msg}");
        for key in ["find_replace.skip_pinned", "find_replace.skip_locked", "find_replace.pinned", "find_replace.locked"] {
            assert_ne!(c.t(key), key, "missing text {key}");
        }
    }

    #[test]
    fn one_replace_through_run_op_is_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let l = ed.level;
        ed.model
            .add_anno(
                ddc_model::AnnoView::Plan(l),
                &ddc_model::AnnoShape::Text(ddc_model::TextNote::new((0.0, 0.0), "Office A")),
                None,
                None,
            )
            .unwrap();
        ed.model
            .add_anno(
                ddc_model::AnnoView::Plan(l),
                &ddc_model::AnnoShape::Text(ddc_model::TextNote::new((1.0, 0.0), "Office B")),
                None,
                None,
            )
            .unwrap();
        let c = Catalog::new(Lang::En);
        let mut st = FindUi {
            query: "Office".into(),
            with: "Lab".into(),
            ..Default::default()
        };
        st.refresh(&ed);
        assert_eq!(st.hits.len(), 2);
        let top = ed.model.op_top().unwrap();
        let keys = st.hits.iter().map(FindHit::hit_key).collect();
        replace(&mut ed, &mut st, &c, Some(keys));
        assert_eq!(ed.model.op_top().unwrap(), top + 1, "one journal step");
        let done = ReplaceReport {
            replaced: 2,
            elements: 2,
            ..Default::default()
        };
        assert_eq!(st.msg.as_deref(), Some(counts(&c, "find_replace.done", &done).as_str()));
        ed.undo();
        let back = ed.model.find_text(&FindOpts {
            query: "Office".into(),
            ..Default::default()
        });
        assert_eq!(back.unwrap().len(), 2, "one undo restores both notes");
        assert_eq!(ed.model.op_top().unwrap(), top);
    }

    fn note(ed: &mut Editor, at: f64, text: &str) {
        let l = ed.level;
        ed.model
            .add_anno(
                ddc_model::AnnoView::Plan(l),
                &ddc_model::AnnoShape::Text(ddc_model::TextNote::new((at, 0.0), text)),
                None,
                None,
            )
            .unwrap();
    }

    /// App with two matching notes and `extra` more levels, speaking `lang`.
    fn app_in(lang: Lang, extra: usize) -> EditorApp {
        let mut a = app();
        a.sh.cat = Catalog::new(lang);
        a.ed.model.sql().execute("UPDATE level SET name='Ground'", []).unwrap();
        for i in 0..extra {
            a.ed.add_level(&format!("L{i}"), 3.0 * (i as f64 + 1.0)).unwrap();
        }
        note(&mut a.ed, 0.0, "Office A");
        note(&mut a.ed, 1.0, "Office B");
        a
    }

    fn shown(lang: Lang, replace_mode: bool, extra: usize) -> crate::layout_contract::Probe {
        let mut a = app_in(lang, extra);
        open_for_layout(replace_mode, "Office", "Lab");
        let p = crate::layout_contract::probe("find_replace.find", |ctx| frame(&mut a, ctx));
        reset();
        p
    }

    #[test]
    fn form_uses_kit_widgets_only() {
        let src = include_str!("find_replace.rs");
        let marker = ["#[cfg(", "test)]\nmod tests"].concat();
        let code = src.split(marker.as_str()).next().unwrap();
        for raw in ["Grid", "TextEdit", "ComboBox"] {
            let needle = format!("egui::{raw}");
            assert!(!code.contains(&needle), "raw {raw} in the form");
        }
        assert!(!code.contains(&["ui.label(", "\"\")"].concat()), "blank caption cell");
    }

    #[test]
    fn exactly_one_primary_and_it_comes_first_in_every_state() {
        for replace_mode in [false, true] {
            for matched in [0, 2] {
                for replaced in [0, 2] {
                    for can_one in [false, true] {
                        let p = footer_plan(replace_mode, matched, replaced, can_one);
                        let n = p.iter().filter(|f| f.primary).count();
                        assert_eq!(n, 1, "{replace_mode} {matched} {replaced} {can_one}");
                        assert!(p[0].primary, "primary is the far right button");
                        let want = if replace_mode { Btn::ReplaceAll } else { Btn::SelectAll };
                        assert_eq!(p[0].btn, want);
                        assert_eq!(p[0].enabled, if replace_mode { replaced > 0 } else { matched > 0 });
                    }
                }
            }
        }
        // Select all is secondary in replace mode, Show replace exists only in find mode
        let r = footer_plan(true, 2, 2, true);
        assert!(r.iter().any(|f| f.btn == Btn::SelectAll && !f.primary));
        assert!(!r.iter().any(|f| f.btn == Btn::ShowReplace));
    }

    #[test]
    fn controls_share_one_left_edge_en_ru_both_modes() {
        use egui::accesskit::Role;
        for lang in [Lang::En, Lang::Ru] {
            for replace_mode in [false, true] {
                for extra in [2, 14] {
                    let p = shown(lang, replace_mode, extra);
                    let tag = format!("{lang:?} replace={replace_mode} levels+{extra}");
                    crate::layout_contract::assert_contract(&tag, &p, false);
                    let of = |r: Role| p.controls.iter().filter(|c| c.role == r).count();
                    assert_eq!(of(Role::TextInput), if replace_mode { 2 } else { 1 }, "{tag}");
                    assert_eq!(of(Role::CheckBox), 2, "{tag}");
                    assert_eq!(of(Role::ComboBox), 2, "{tag}");
                    // accesskit lists nodes in no fixed order: the row's first box is the leftmost
                    let first_box = p
                        .controls
                        .iter()
                        .filter(|c| c.role == Role::CheckBox)
                        .min_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
                    let xs: Vec<f32> = p
                        .controls
                        .iter()
                        .filter(|c| matches!(c.role, Role::TextInput | Role::ComboBox))
                        .chain(first_box)
                        .map(|c| c.rect.left())
                        .collect();
                    let x0 = xs[0];
                    assert!(xs.iter().all(|x| (x - x0).abs() < 0.5), "{tag}: {xs:?}");
                }
            }
        }
    }

    #[test]
    fn primary_is_the_rightmost_footer_button_en_ru_both_modes() {
        use egui::accesskit::Role;
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for replace_mode in [false, true] {
                let p = shown(lang, replace_mode, 2);
                let right = |label: &str| {
                    p.controls
                        .iter()
                        .filter(|x| x.role == Role::Button && x.label == label)
                        .map(|x| x.rect.right())
                        .fold(f32::NAN, f32::max)
                };
                let primary = if replace_mode {
                    c.tf("find_replace.replace_all", &[("n", "2")])
                } else {
                    c.tf("find_replace.select_all", &[("n", "2")])
                };
                let others = if replace_mode {
                    vec![c.t("find_replace.replace"), c.tf("find_replace.select_all", &[("n", "2")]), c.t("find_replace.close")]
                } else {
                    vec![c.t("find_replace.show_replace"), c.t("find_replace.close")]
                };
                let rp = right(&primary);
                assert!(!rp.is_nan(), "{lang:?} replace={replace_mode}: no {primary:?}");
                for o in others {
                    let ro = right(&o);
                    assert!(!ro.is_nan(), "{lang:?}: no {o:?}");
                    assert!(rp > ro, "{lang:?} replace={replace_mode}: {primary:?} right of {o:?}");
                }
            }
        }
    }

    #[test]
    fn replace_all_shows_its_count_before_it_writes() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let a = app_in(lang, 0);
            let mut st = FindUi {
                query: "Office".into(),
                with: "Lab".into(),
                replace_mode: true,
                ..Default::default()
            };
            st.refresh(&a.ed);
            let all = replace_report(&st.hits, &st.with, None);
            assert_eq!(all.replaced, 2);
            let line = status_line(&c, &st, &all).unwrap();
            assert_eq!(line, counts(&c, "find_replace.will", &all), "{lang:?}");
            assert!(line.contains('2'), "{line}");
            // a nothing-to-do preview, then the outcome of a step replaces it
            let none = replace_report(&st.hits, "Office", None);
            assert_eq!(status_line(&c, &st, &none).unwrap(), counts(&c, "find_replace.nothing", &none));
            st.msg = Some("done".into());
            assert_eq!(status_line(&c, &st, &all).as_deref(), Some("done"));
            st.msg = None;
            st.replace_mode = false;
            assert!(status_line(&c, &st, &all).is_none(), "find mode shows no replace preview");
            // the preview wrote nothing
            let left = a.ed.model.find_text(&FindOpts {
                query: "Office".into(),
                ..Default::default()
            });
            assert_eq!(left.unwrap().len(), 2);
        }
    }

    fn shape_texts(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|x| shape_texts(x, out)),
            egui::Shape::Text(t) => {
                let r = egui::Rect::from_min_size(t.pos, t.galley.size());
                out.push((t.galley.text().to_string(), r));
            }
            _ => {}
        }
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    /// One frame of the window at time `t`; returns the text painted.
    fn pass(
        a: &mut EditorApp,
        ctx: &egui::Context,
        t: f64,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Rect)> {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            time: Some(t),
            events,
            ..Default::default()
        };
        let out = ctx.run(raw, |c| {
            let _ = a.sh.theme.apply(c);
            frame(a, c);
        });
        let mut texts = vec![];
        out.shapes.iter().for_each(|cs| shape_texts(&cs.shape, &mut texts));
        texts
    }

    /// Press and release, or egui reports the next press as a repeat.
    fn esc() -> Vec<egui::Event> {
        [true, false]
            .map(|pressed| egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .to_vec()
    }

    fn quiet_frames(a: &mut EditorApp, ctx: &egui::Context, t: &mut f64) {
        for _ in 0..3 {
            *t += 0.05;
            pass(a, ctx, *t, vec![]);
        }
    }

    /// Rule shared by every search field: while it has focus and text, the first Esc clears the
    /// text and the window stays; with an empty query, or focus elsewhere, Esc closes it.
    #[test]
    fn esc_clears_a_focused_query_first_then_closes() {
        for lang in [Lang::En, Lang::Ru] {
            let mut a = app_in(lang, 0);
            let ctx = font_ctx();
            let mut t = 0.0;
            open_for_layout(false, "Office", "");
            ST.with(|s| s.borrow_mut().focus = true);
            quiet_frames(&mut a, &ctx, &mut t);
            assert!(state().0, "{lang:?} open");
            pass(&mut a, &ctx, t + 0.05, esc());
            let (open, query) = ST.with(|s| (s.borrow().open, s.borrow().query.clone()));
            assert!(open && query.is_empty(), "{lang:?} first Esc clears: {open} {query:?}");
            t += 0.1;
            quiet_frames(&mut a, &ctx, &mut t);
            pass(&mut a, &ctx, t + 0.05, esc());
            assert!(!state().0, "{lang:?} second Esc closes");

            // empty query: one Esc closes
            open_for_layout(false, "", "");
            ST.with(|s| s.borrow_mut().focus = true);
            quiet_frames(&mut a, &ctx, &mut t);
            pass(&mut a, &ctx, t + 0.05, esc());
            assert!(!state().0, "{lang:?} empty query: Esc closes");

            // text but no focus in the field: Esc closes and the text stays
            open_for_layout(false, "Office", "");
            quiet_frames(&mut a, &ctx, &mut t);
            ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("fnd1.query")));
            pass(&mut a, &ctx, t + 0.05, esc());
            assert!(!state().0, "{lang:?} unfocused field: Esc closes");
            reset();
        }
    }

    /// Select all is the find-mode primary and keeps its explanation on hover.
    #[test]
    fn select_all_primary_keeps_its_hover_text() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut a = app_in(lang, 0);
            let ctx = font_ctx();
            let mut t = 0.0;
            open_for_layout(false, "Office", "");
            quiet_frames(&mut a, &ctx, &mut t);
            let label = c.tf("find_replace.select_all", &[("n", "2")]);
            let tip = c.t("find_replace.select_all_tip");
            let texts = pass(&mut a, &ctx, t + 0.05, vec![]);
            assert!(!texts.iter().any(|x| x.0 == tip), "{lang:?} no tip before hover");
            let at = texts.iter().find(|x| x.0 == label).expect("select all drawn").1.center();
            t += 0.1;
            pass(&mut a, &ctx, t, vec![egui::Event::PointerMoved(at)]);
            let mut seen = false;
            for dt in [0.6, 1.2, 2.0] {
                seen |= pass(&mut a, &ctx, t + dt, vec![]).iter().any(|x| x.0 == tip);
            }
            assert!(seen, "{lang:?} hover text of the primary is shown");
            reset();
        }
    }

    #[test]
    fn new_keys_exist_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in ["find_replace.find_clear", "find_replace.pick_search", "find_replace.pick_empty"] {
                assert_ne!(c.t(k), k, "{lang:?} {k}");
            }
        }
        assert_ne!(Catalog::new(Lang::En).t("find_replace.pick_search"), Catalog::new(Lang::Ru).t("find_replace.pick_search"));
    }
}
