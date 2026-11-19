//! Materials browser: searchable list, material card, replace a material across the whole model.
//! Edits accumulate in a draft and are applied with one button (one undo step).

use std::cell::RefCell;

use ddc_model::{Material, MaterialSpec, LIB_HATCH_PREFIX, MATERIAL_HATCHES};
use ddc_ui::theme::{radius, space, stroke, Role};
use ddc_ui::widgets::{self, Button, RowState, TextField};
use egui::{self, Stroke};

use super::{color_of, opt_color, paint_hatch, run, t};
use crate::editor::Editor;
use crate::ui_kit;

/// Swatch hatch spacing, px.
const SWATCH_HATCH: f32 = 4.0;

/// Label of a built-in material; a custom name is shown as is.
pub(super) fn label(name: &str) -> String {
    let key = format!("mat.{name}");
    let s = t(&key);
    if s == key {
        name.to_owned()
    } else {
        s
    }
}

#[derive(Default)]
struct Draft {
    orig: String,
    name: String,
    color: Option<u32>,
    hatch: String,
    density: String,
    conductivity: String,
    specific_heat: String,
    note: String,
    error: Option<String>,
    /// Destructive or wide action waiting for the user's confirmation.
    pending: Option<Pending>,
    /// The stored material changed under the card's edits: keep them or reload.
    conflict: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Pending {
    Delete,
    /// Replace this material with the named one in the whole model.
    Replace(String),
}

/// Which empty state the list shows.
#[derive(Debug, PartialEq)]
enum Empty {
    No,
    /// The library has no materials yet.
    Library,
    /// Materials exist, the search hides all of them.
    Search,
}

fn empty_kind(query: &str, total: usize, visible: usize) -> Empty {
    if visible > 0 {
        Empty::No
    } else if total == 0 || query.trim().is_empty() {
        Empty::Library
    } else {
        Empty::Search
    }
}

/// Materials whose name or shown label contains the query (case-insensitive).
fn filter<'a>(list: &'a [Material], query: &str) -> Vec<&'a Material> {
    let needle = query.trim().to_lowercase();
    list.iter()
        .filter(|m| {
            needle.is_empty()
                || m.spec.name.to_lowercase().contains(&needle)
                || label(&m.spec.name).to_lowercase().contains(&needle)
        })
        .collect()
}

/// Index of the row an Up/Down key moves to; none selected starts at the first/last row.
fn step_pick(names: &[&str], cur: Option<&str>, down: bool) -> Option<usize> {
    if names.is_empty() {
        return None;
    }
    let at = cur.and_then(|c| names.iter().position(|n| *n == c));
    Some(match (at, down) {
        (None, true) => 0,
        (None, false) => names.len() - 1,
        (Some(i), true) => (i + 1).min(names.len() - 1),
        (Some(i), false) => i.saturating_sub(1),
    })
}

fn usage_total(usage: &[(String, usize)]) -> usize {
    usage.iter().map(|u| u.1).sum()
}

/// Delete asks first; a material in use is refused with the number of places using it.
fn request_delete(d: &mut Draft, total: usize) -> bool {
    if total > 0 {
        d.error = Some(crate::tr(
            "materials.mat.delete_refused",
            &[("n", &total.to_string())],
        ));
        d.pending = None;
        false
    } else {
        d.error = None;
        d.pending = Some(Pending::Delete);
        true
    }
}

/// One undo step; the model refuses a material in use even if the UI let it through.
fn confirm_delete(ed: &mut Editor, d: &mut Draft) -> bool {
    d.pending = None;
    let name = d.orig.clone();
    let done = run(ed, move |m| m.delete_material(&name));
    if !done {
        d.error = ed.status.clone();
    }
    done
}

/// One undo step: every reference moves to `to`, the old material goes away.
fn confirm_replace(ed: &mut Editor, d: &mut Draft, to: &str) -> bool {
    d.pending = None;
    let (from, to) = (d.orig.clone(), to.to_string());
    let (shown_from, shown_to) = (label(&from), label(&to));
    let done = run(ed, move |m| m.replace_material(&from, &to).map(|_| ()));
    if done {
        ed.status = Some(crate::tr(
            "view_gfx.mat.replaced",
            &[("from", &shown_from), ("to", &shown_to)],
        ));
    } else {
        d.error = ed.status.clone();
    }
    done
}

/// Saves the draft (rename, colour, properties) as one undo step.
fn apply_draft(ed: &mut Editor, d: &mut Draft) -> bool {
    if d.name.trim().is_empty() {
        d.error = Some(t("materials.mat.name_empty"));
        return false;
    }
    match d.spec() {
        Err(e) => {
            d.error = Some(e);
            false
        }
        Ok(spec) => {
            let orig = d.orig.clone();
            let name = spec.name.clone();
            if run(ed, move |m| m.set_material(&orig, &spec).map(|_| ())) {
                d.orig = name;
                d.name = d.orig.clone();
                d.error = None;
                d.conflict = false;
                true
            } else {
                d.error = ed.status.clone();
                false
            }
        }
    }
}

/// Hatch choices as (stored id, shown text): built-ins, then the project library; a stored
/// value that is in neither list stays selectable.
fn hatch_options(current: &str, lib: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = MATERIAL_HATCHES
        .iter()
        .map(|h| (h.to_string(), t(&format!("view_gfx.hatch.{h}"))))
        .collect();
    for n in lib {
        out.push((
            format!("{LIB_HATCH_PREFIX}{n}"),
            crate::tr("draw_quality.lib.hatch_of", &[("name", n.as_str())]),
        ));
    }
    if !out.iter().any(|o| o.0 == current) {
        let shown = match current.strip_prefix(LIB_HATCH_PREFIX) {
            Some(n) => crate::tr("draw_quality.lib.hatch_of", &[("name", n)]),
            None => t(&format!("view_gfx.hatch.{current}")),
        };
        out.push((current.to_string(), shown));
    }
    out
}

fn num_text(v: Option<f64>) -> String {
    v.map_or(String::new(), |x| x.to_string())
}

/// Empty — unset; a comma is accepted as the decimal separator.
fn parse_num(s: &str) -> Result<Option<f64>, ()> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    crate::units::parse_locale_number(s, crate::units::ui_decimal_mark())
        .ok()
        .filter(|x| x.is_finite() && *x >= 0.0)
        .map(Some)
        .ok_or(())
}

impl Draft {
    fn of(m: &Material) -> Self {
        Draft {
            orig: m.spec.name.clone(),
            name: m.spec.name.clone(),
            color: m.spec.color,
            hatch: m.spec.hatch.clone(),
            density: num_text(m.spec.density),
            conductivity: num_text(m.spec.conductivity),
            specific_heat: num_text(m.spec.specific_heat),
            note: m.spec.note.clone(),
            error: None,
            pending: None,
            conflict: false,
        }
    }

    fn spec(&self) -> Result<MaterialSpec, String> {
        let bad = |_| t("view_gfx.mat.bad_number");
        Ok(MaterialSpec {
            // an imported name with surrounding spaces is kept until it is edited
            name: if self.name.trim() == self.orig.trim() {
                self.orig.clone()
            } else {
                self.name.trim().to_string()
            },
            color: self.color,
            hatch: self.hatch.clone(),
            density: parse_num(&self.density).map_err(bad)?,
            conductivity: parse_num(&self.conductivity).map_err(bad)?,
            specific_heat: parse_num(&self.specific_heat).map_err(bad)?,
            note: self.note.trim().to_string(),
        })
    }

    /// The card differs from the stored material it was opened on; a material gone from the
    /// list counts as edited, so the question still asks.
    fn changed(&self, list: &[Material]) -> bool {
        list.iter()
            .find(|m| m.spec.name == self.orig)
            .is_none_or(|m| {
                let b = Draft::of(m);
                (&b.name, b.color, &b.hatch, &b.density, &b.conductivity, &b.specific_heat, &b.note)
                    != (
                        &self.name,
                        self.color,
                        &self.hatch,
                        &self.density,
                        &self.conductivity,
                        &self.specific_heat,
                        &self.note,
                    )
            })
    }
}

#[derive(Default)]
struct Win {
    open: bool,
    /// `Editor::cache_key` the list was read under.
    rev: Option<(u64, u64)>,
    list: Vec<Material>,
    search: String,
    new_name: String,
    add_error: Option<String>,
    /// Row to scroll into view on the next frame (keyboard move, new material).
    reveal: Option<String>,
    draft: Option<Draft>,
    usage: Vec<(String, usize)>,
    replace_to: String,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

pub(super) fn open() {
    WIN.with(|w| w.borrow_mut().open = true);
}

fn select(st: &mut Win, ed: &Editor, name: &str) {
    st.draft = st.list.iter().find(|m| m.spec.name == name).map(Draft::of);
    st.usage = ed.model.material_usage(name).unwrap_or_default();
    st.replace_to.clear();
}

/// The model changed (undo, another window, a project swap): the list follows it. A clean card
/// shows the stored material again; edits stay while their material is unchanged, otherwise
/// they stay too and the card asks (`conflict`) instead of dropping them.
fn sync(st: &mut Win, ed: &Editor) {
    let old = std::mem::replace(&mut st.list, ed.model.materials().unwrap_or_default());
    st.rev = Some(ed.cache_key());
    let Some(d) = st.draft.as_mut() else {
        return;
    };
    let orig = d.orig.clone();
    if !d.changed(&st.list) || !d.changed(&old) {
        let err = d.error.clone();
        select(st, ed, &orig);
        if let Some(d) = &mut st.draft {
            d.error = err;
        }
        return;
    }
    let stored = |l: &[Material]| l.iter().find(|m| m.spec.name == orig).map(|m| m.spec.clone());
    if stored(&old) != stored(&st.list) {
        d.conflict = true;
        d.pending = None;
    }
    st.usage = ed.model.material_usage(&orig).unwrap_or_default();
}

/// Keep my edits: the card stays as typed; Apply then writes it over the stored material.
fn keep_mine(st: &mut Win) {
    if let Some(d) = &mut st.draft {
        d.conflict = false;
    }
}

/// Reload: the card shows the stored material (none when it is gone), the edits go.
fn reload_card(st: &mut Win, ed: &Editor) {
    if let Some(orig) = st.draft.as_ref().map(|d| d.orig.clone()) {
        select(st, ed, &orig);
    }
}

/// Swatch: material colour with its hatch; without a colour — the class colour from the theme.
fn paint_swatch(ui: &egui::Ui, rect: egui::Rect, color: Option<u32>, hatch: &str) {
    let v = ui.visuals();
    let fill = color.map_or(v.widgets.inactive.bg_fill, color_of);
    let line = Stroke::new(1.0, v.text_color());
    ui.painter().rect_filled(rect, 0.0, fill);
    let ring = vec![
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    paint_hatch(ui.painter(), &[ring], hatch, SWATCH_HATCH, line);
    ui.painter()
        .rect_stroke(rect, 0.0, line, egui::StrokeKind::Inside);
}

/// One list row: swatch, elided name (full name on hover), selected/hover fill from the kit.
fn list_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    selected: bool,
    name: &str,
    color: Option<u32>,
    hatch: &str,
) -> egui::Response {
    let size = egui::vec2(ui.available_width(), th.d().row);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let state = if selected {
        RowState::Selected
    } else {
        RowState::Normal
    };
    let (fill, text_c, _) = widgets::table::row_colors(th.sem(), state, resp.hovered());
    ui.painter().rect_filled(rect, radius::SM, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            radius::SM,
            Stroke::new(stroke::FOCUS, th.sem().focus_ring),
            egui::StrokeKind::Inside,
        );
    }
    let sh = rect.height() - 2.0 * space::XS;
    let sw = egui::Rect::from_min_size(
        egui::pos2(rect.left() + space::SM, rect.top() + space::XS),
        egui::vec2(sh * 1.5, sh),
    );
    paint_swatch(ui, sw, color, hatch);
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(sw.right() + space::SM, rect.top()),
        egui::pos2(rect.right() - space::SM, rect.bottom()),
    );
    let cut = widgets::text::paint_in(
        ui,
        text_rect,
        name,
        th.font(Role::Body),
        text_c,
        egui::Align::Min,
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, name)
    });
    if cut {
        resp.on_hover_text(name)
    } else {
        resp
    }
}

/// Adds the typed material with defaults; empty or taken names are refused with a note.
fn add_named(ed: &mut Editor, st: &mut Win) -> bool {
    let name = st.new_name.trim().to_string();
    if name.is_empty() {
        st.add_error = Some(t("materials.mat.name_empty"));
        return false;
    }
    let mut spec = ddc_model::material_defaults(&name);
    spec.name = name.clone();
    if run(ed, move |m| m.add_material(&spec).map(|_| ())) {
        st.new_name.clear();
        st.add_error = None;
        st.list = ed.model.materials().unwrap_or_default();
        st.rev = Some(ed.cache_key());
        select(st, ed, &name);
        st.reveal = Some(name);
        true
    } else {
        st.add_error = ed.status.clone();
        false
    }
}

fn list_ui(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win) {
    let th = ddc_ui::theme::of(ui.ctx());
    let hint = t("view_gfx.mat.search");
    let search = TextField::new(&mut st.search)
        .id_salt("ux48e.mat.search")
        .hint(&hint)
        .labelled_by(egui::Id::new("ux48e.mat.search.lbl"), &hint)
        .show(ui, &th);
    ui.add_space(space::XS);
    let cur = st.draft.as_ref().map(|d| d.orig.clone());
    let mut pick: Option<String> = None;
    let mut clear = false;
    let mut row_focus = false;
    let reveal = st.reveal.take();
    let visible = filter(&st.list, &st.search);
    let list_h = th.d().row * 14.0;
    egui::ScrollArea::vertical()
        .id_salt("f5.mat.list")
        .max_height(list_h)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::XXS;
            match empty_kind(&st.search, st.list.len(), visible.len()) {
                Empty::Library => {
                    widgets::icon_empty_state(
                        ui,
                        "material",
                        &t("materials.mat.empty_title"),
                        &t("materials.mat.empty_hint"),
                        None,
                    );
                }
                Empty::Search => {
                    clear = widgets::icon_empty_state(
                        ui,
                        "find",
                        &t("view_gfx.mat.nothing"),
                        &t("materials.mat.nothing_hint"),
                        Some(&t("materials.mat.clear_search")),
                    );
                }
                Empty::No => {}
            }
            for m in &visible {
                let on = cur.as_deref() == Some(m.spec.name.as_str());
                let r = list_row(ui, &th, on, &label(&m.spec.name), m.spec.color, &m.spec.hatch);
                row_focus |= r.has_focus();
                if reveal.as_deref() == Some(m.spec.name.as_str()) {
                    r.scroll_to_me(None);
                }
                if r.clicked() {
                    pick = Some(m.spec.name.clone());
                }
            }
        });
    if search.response.has_focus() || row_focus {
        let (down, up) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
            )
        });
        if down || up {
            let names: Vec<&str> = visible.iter().map(|m| m.spec.name.as_str()).collect();
            if let Some(i) = step_pick(&names, cur.as_deref(), down) {
                pick = Some(names[i].to_string());
            }
        }
    }
    if clear {
        st.search.clear();
    }
    if let Some(n) = pick {
        select(st, ed, &n);
        st.reveal = Some(n);
    }
    ui.add_space(space::SM);
    ui.separator();
    let (hint, add_label) = (t("view_gfx.mat.new_name"), t("view_gfx.mat.add"));
    let can_add = !st.new_name.trim().is_empty();
    let mut go = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let b = ui
            .add_enabled_ui(can_add, |ui| Button::secondary(&add_label).show(ui, &th))
            .inner;
        go = b.clicked();
        let out = TextField::new(&mut st.new_name)
            .id_salt("ux48e.mat.new")
            .hint(&hint)
            .labelled_by(egui::Id::new("ux48e.mat.new.lbl"), &hint)
            .width(ui.available_width())
            .error(st.add_error.is_some())
            .show(ui, &th);
        if out.changed {
            st.add_error = None;
        }
        go |= out.committed && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    if go {
        add_named(ed, st);
    }
    if let Some(e) = &st.add_error {
        ui_kit::error_note(ui, e);
    }
}

/// Label on the left, numbers on the right: values are right-aligned and fit in both languages.
fn num_row(ui: &mut egui::Ui, key: &str, unit: &str, v: &mut String) {
    let name = t(key);
    ui_kit::kv_text(ui, &name, v, &t(unit), &name, "", egui::Align::Max);
}

/// The user's answer to a card conflict.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Answer {
    Keep,
    Reload,
}

/// The card; returns the conflict answer clicked this frame, if any.
fn details_ui(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win) -> Option<Answer> {
    let Win {
        draft,
        usage,
        list,
        replace_to,
        ..
    } = st;
    let th = ddc_ui::theme::of(ui.ctx());
    let Some(d) = draft else {
        ui_kit::empty_note(ui, t("view_gfx.mat.pick_hint"), None);
        return None;
    };
    let total = usage_total(usage);
    ui.horizontal(|ui| {
        let sh = th.d().control_lg;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(sh * 1.5, sh), egui::Sense::hover());
        paint_swatch(ui, rect, d.color, &d.hatch);
        ui.vertical(|ui| {
            ui_kit::elided(ui, &label(&d.orig));
            if total == 0 {
                ui_kit::elided_weak(ui, &t("view_gfx.mat.unused"));
            } else {
                ui_kit::elided_weak(ui, &crate::tr("view_gfx.mat.used", &[("n", &total.to_string())]));
            }
        });
    });
    ui.add_space(space::SM);
    // the project hatch library: any entry can be a material hatch
    let lib_names = ed.model.drawq_hatches().map(|l| l.names()).unwrap_or_default();
    let opts = hatch_options(&d.hatch, &lib_names);
    let mut hatch_at = opts.iter().position(|o| o.0 == d.hatch).unwrap_or(0);
    let shown: Vec<&str> = opts.iter().map(|o| o.1.as_str()).collect();
    ui_kit::props(ui, "ux48e.mat.card", |ui| {
        let name = t("view_gfx.mat.name");
        ui_kit::kv_text(ui, &name, &mut d.name, "", &name, "", egui::Align::Min);
        ui_kit::kv_widget(ui, &t("view_gfx.mat.color"), |ui| {
            opt_color(ui, &mut d.color);
            if d.color.is_none() {
                ui.weak(t("view_gfx.mat.color_theme"));
            }
        });
        ui_kit::kv_widget(ui, &t("view_gfx.mat.hatch"), |ui| {
            let pick = widgets::dropdown(
                ui,
                &th,
                "ux48e.mat.hatch",
                &mut hatch_at,
                &shown,
                Some(ui_kit::WIDGET_W),
            );
            if pick.changed() {
                d.hatch = opts[hatch_at].0.clone();
            }
        });
        num_row(ui, "view_gfx.mat.density", "view_gfx.unit.density", &mut d.density);
        num_row(
            ui,
            "view_gfx.mat.conductivity",
            "view_gfx.unit.conductivity",
            &mut d.conductivity,
        );
        num_row(
            ui,
            "view_gfx.mat.specific_heat",
            "view_gfx.unit.specific_heat",
            &mut d.specific_heat,
        );
        let note = t("view_gfx.mat.note");
        ui_kit::kv_text(ui, &note, &mut d.note, "", &note, "", egui::Align::Min);
    });
    if let Some(e) = &d.error {
        ui_kit::error_note(ui, e);
    }
    let mut answer = None;
    if d.conflict {
        ui_kit::warn_note(ui, t("materials.mat.conflict"));
        ui.horizontal(|ui| {
            if ui_kit::secondary_button(ui, t("materials.mat.keep")).clicked() {
                answer = Some(Answer::Keep);
            }
            if ui_kit::secondary_button(ui, t("materials.mat.reload")).clicked() {
                answer = Some(Answer::Reload);
            }
        });
    }
    super::mat_texture_ui::section(ui, ed, &d.orig);
    super::mat_mesh_ui::section(ui, ed, &d.orig);
    ui.separator();
    if total > 0 {
        let others: Vec<&Material> = list.iter().filter(|m| m.spec.name != d.orig).collect();
        let mut items: Vec<String> = vec![t("view_gfx.mat.replace_pick")];
        items.extend(others.iter().map(|m| label(&m.spec.name)));
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        let mut at = others
            .iter()
            .position(|m| m.spec.name == *replace_to)
            .map_or(0, |i| i + 1);
        ui_kit::props(ui, "materials.mat.replace", |ui| {
            ui_kit::kv_widget(ui, &t("materials.mat.replace_with"), |ui| {
                let pick = widgets::dropdown(
                    ui,
                    &th,
                    "ux48e.mat.replace_to",
                    &mut at,
                    &items,
                    Some(ui_kit::WIDGET_W),
                );
                if pick.changed() {
                    *replace_to = at
                        .checked_sub(1)
                        .map_or(String::new(), |i| others[i].spec.name.clone());
                }
            });
        });
        let can = !replace_to.is_empty();
        let label_replace = t("view_gfx.mat.replace");
        let go = ui
            .add_enabled_ui(can, |ui| Button::secondary(&label_replace).show(ui, &th))
            .inner;
        if go.clicked() {
            d.error = None;
            d.pending = Some(Pending::Replace(replace_to.clone()));
        }
        ui.separator();
    }
    let mut apply = false;
    let mut ask_delete = false;
    let mut delete = false;
    let mut replace: Option<String> = None;
    match d.pending.clone() {
        Some(p) => {
            let msg = match &p {
                Pending::Delete => {
                    crate::tr("materials.mat.delete_confirm", &[("name", &label(&d.orig))])
                }
                Pending::Replace(to) => crate::tr(
                    "materials.mat.replace_confirm",
                    &[
                        ("from", &label(&d.orig)),
                        ("to", &label(to)),
                        ("n", &total.to_string()),
                    ],
                ),
            };
            ui_kit::warn_note(ui, msg);
            ui.horizontal(|ui| {
                match &p {
                    Pending::Delete => {
                        delete = ui_kit::danger_button(ui, t("view_gfx.delete"), true).clicked();
                    }
                    Pending::Replace(to) => {
                        if Button::secondary(&t("view_gfx.mat.replace")).show(ui, &th).clicked() {
                            replace = Some(to.clone());
                        }
                    }
                }
                if ui_kit::cancel_button(ui, t("view_gfx.cancel")).clicked() {
                    d.pending = None;
                }
            });
        }
        None => {
            ui.horizontal(|ui| {
                // a conflict is answered first: Apply would overwrite the changed material
                apply = ui_kit::primary_button(ui, t("view_gfx.apply"), !d.conflict).clicked();
                let del = ui_kit::danger_button(ui, t("view_gfx.delete"), total == 0);
                if total > 0 {
                    del.on_disabled_hover_text(crate::tr(
                        "materials.mat.delete_refused",
                        &[("n", &total.to_string())],
                    ));
                } else {
                    ask_delete = del.clicked();
                }
            });
        }
    }
    if apply {
        apply_draft(ed, d);
    }
    if ask_delete {
        request_delete(d, total);
    }
    if let Some(to) = replace {
        confirm_replace(ed, d, &to);
    }
    if delete && confirm_delete(ed, d) {
        *draft = None;
        return None;
    }
    answer
}

pub(super) fn window(ctx: &egui::Context, ed: &mut Editor) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    // a closed window may still hold card edits: the close question compares them to the model
    if st.rev != Some(ed.cache_key()) && (st.open || st.draft.is_some()) {
        sync(&mut st, ed);
    }
    if st.open {
        let mut open = true;
        let mut answer = None;
        crate::ui_kit::Dialog::new("view.materials", t("view_gfx.mat.title"), crate::ui_kit::DialogSize::Wide)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let w = ui.available_width() * 0.42;
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        list_ui(ui, ed, &mut st);
                    });
                    ui.separator();
                    ui.vertical(|ui| answer = details_ui(ui, ed, &mut st));
                });
            });
        match answer {
            Some(Answer::Keep) => keep_mine(&mut st),
            Some(Answer::Reload) => reload_card(&mut st, ed),
            None => {}
        }
        st.open = open;
    }
    WIN.with(|w| *w.borrow_mut() = st);
}

/// The material whose card holds edits a project close or swap would lose (its stored name).
pub(crate) fn draft_name() -> Option<String> {
    WIN.with(|w| {
        let st = w.try_borrow().ok()?;
        let d = st.draft.as_ref()?;
        d.changed(&st.list).then(|| d.orig.clone())
    })
}

/// Discard on the project question: the card shows the stored material again.
pub(crate) fn discard_drafts() {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        let st = &mut *st;
        if let Some(orig) = st.draft.as_ref().map(|d| d.orig.clone()) {
            st.draft = st.list.iter().find(|m| m.spec.name == orig).map(Draft::of);
        }
    });
}

/// Save on the project question: applies the card as Apply does; a refusal opens the window
/// with the card and its reason kept. A conflict (seen now or not yet answered) is never
/// written over: the window opens on it first.
pub(crate) fn save_drafts(ed: &mut Editor) -> bool {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if st.draft.is_some() && st.rev != Some(ed.cache_key()) {
        sync(&mut st, ed);
    }
    let ok = match st.draft.as_mut() {
        Some(d) if d.changed(&st.list) && d.conflict => false,
        Some(d) if d.changed(&st.list) => apply_draft(ed, d),
        _ => true,
    };
    if ok {
        st.list = ed.model.materials().unwrap_or_default();
        st.rev = Some(ed.cache_key());
    } else {
        st.open = true;
    }
    WIN.with(|w| *w.borrow_mut() = st);
    ok
}

/// Opens this thread's window with the first material selected, so the card is drawn too.
#[cfg(test)]
pub(super) fn open_for_layout(ed: &Editor) {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        st.open = true;
        st.list = ed.model.materials().unwrap_or_default();
        st.rev = Some(ed.cache_key());
        let first = st.list.first().expect("a project material").spec.name.clone();
        select(&mut st, ed, &first);
    });
}

/// Gives this thread's window an edited note on the first material of the project.
#[cfg(test)]
pub(crate) fn seed_draft_for_test(ed: &Editor) {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        st.list = ed.model.materials().unwrap_or_default();
        st.rev = Some(ed.cache_key());
        let name = st.list.first().expect("a project material").spec.name.clone();
        select(&mut st, ed, &name);
        st.draft.as_mut().unwrap().note = "Zz note".into();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_the_materials_window() {
        let mut ed = Editor::new_in_memory().unwrap();
        open();
        crate::ui_kit::run_esc_frames(|c| window(c, &mut ed));
        assert!(!WIN.with(|w| w.borrow().open));
    }

    #[test]
    fn numbers_accept_comma_and_empty() {
        assert_eq!(parse_num(" 1,5 "), Ok(Some(1.5)));
        assert_eq!(parse_num(""), Ok(None));
        assert!(parse_num("abc").is_err());
        assert!(parse_num("-1").is_err());
    }

    #[test]
    fn draft_round_trips_spec() {
        let spec = ddc_model::material_defaults("concrete");
        let m = Material {
            id: 1,
            spec: spec.clone(),
        };
        let d = Draft::of(&m);
        assert_eq!(d.spec().unwrap().density, spec.density);
        assert_eq!(d.spec().unwrap().hatch, spec.hatch);
    }

    #[test]
    fn imported_name_survives_until_edited() {
        let mut spec = ddc_model::material_defaults("concrete");
        spec.name = " Concrete ".into();
        let mut d = Draft::of(&Material { id: 1, spec });
        assert_eq!(d.spec().unwrap().name, " Concrete ");
        d.name = " Brick ".into();
        assert_eq!(d.spec().unwrap().name, "Brick");
    }

    fn named(name: &str) -> Material {
        let mut spec = ddc_model::material_defaults("concrete");
        spec.name = name.into();
        Material { id: 1, spec }
    }

    fn has(ed: &Editor, name: &str) -> bool {
        ed.model
            .materials()
            .unwrap()
            .iter()
            .any(|m| m.spec.name == name)
    }

    fn color_of_named(ed: &Editor, name: &str) -> Option<u32> {
        ed.model
            .materials()
            .unwrap()
            .iter()
            .find(|m| m.spec.name == name)
            .and_then(|m| m.spec.color)
    }

    /// Window state with `name` added through the list's own Add path and selected.
    fn with_added(ed: &mut Editor, name: &str) -> Win {
        let mut st = Win {
            list: ed.model.materials().unwrap(),
            rev: Some(ed.cache_key()),
            new_name: name.into(),
            ..Win::default()
        };
        assert!(add_named(ed, &mut st));
        st
    }

    #[test]
    fn search_filters_by_name_ignoring_case_and_blanks() {
        let list = vec![named("Concrete C30"), named("Brick"), named("Steel")];
        assert_eq!(filter(&list, "").len(), 3);
        assert_eq!(filter(&list, "  ").len(), 3);
        let hit = filter(&list, " CONC ");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].spec.name, "Concrete C30");
        assert!(filter(&list, "zzz").is_empty());
    }

    #[test]
    fn empty_state_tells_empty_library_from_empty_search() {
        assert_eq!(empty_kind("", 0, 0), Empty::Library);
        assert_eq!(empty_kind("abc", 0, 0), Empty::Library);
        assert_eq!(empty_kind("abc", 5, 0), Empty::Search);
        assert_eq!(empty_kind("abc", 5, 2), Empty::No);
        assert_eq!(empty_kind("", 5, 5), Empty::No);
    }

    #[test]
    fn arrow_keys_step_and_clamp() {
        let names = ["a", "b", "c"];
        assert_eq!(step_pick(&names, None, true), Some(0));
        assert_eq!(step_pick(&names, None, false), Some(2));
        assert_eq!(step_pick(&names, Some("a"), true), Some(1));
        assert_eq!(step_pick(&names, Some("c"), true), Some(2));
        assert_eq!(step_pick(&names, Some("a"), false), Some(0));
        assert_eq!(step_pick(&names, Some("gone"), true), Some(0));
        assert_eq!(step_pick(&[], Some("a"), true), None);
    }

    #[test]
    fn delete_unused_waits_for_confirm_and_is_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Spare");
        let d = st.draft.as_mut().unwrap();
        assert!(request_delete(d, 0));
        assert_eq!(d.pending, Some(Pending::Delete));
        assert!(has(&ed, "Zz Spare"), "asking must not delete");
        assert!(confirm_delete(&mut ed, d));
        assert!(d.pending.is_none());
        assert!(!has(&ed, "Zz Spare"));
        ed.undo();
        assert!(has(&ed, "Zz Spare"), "one undo restores the material");
    }

    #[test]
    fn delete_in_use_is_refused_with_the_count() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Used");
        let layer = || ddc_model::LayerSpec {
            material: "Zz Used".into(),
            thickness: 0.1,
        };
        ed.model.add_wall_type("Zz W", &[layer(), layer()]).unwrap();
        select(&mut st, &ed, "Zz Used");
        let total = usage_total(&st.usage);
        assert_eq!(total, 2);
        let d = st.draft.as_mut().unwrap();
        assert!(!request_delete(d, total));
        assert!(d.pending.is_none());
        let msg = d.error.clone().unwrap();
        assert!(msg.contains('2') && !msg.contains("{n}"), "{msg}");
        // the model refuses too, even if the UI let a click through
        assert!(!confirm_delete(&mut ed, d));
        assert!(has(&ed, "Zz Used"));
        assert!(d.error.is_some());
    }

    #[test]
    fn replace_moves_references_in_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Old");
        let layer = ddc_model::LayerSpec {
            material: "Zz Old".into(),
            thickness: 0.1,
        };
        ed.model.add_wall_type("Zz W", &[layer.clone()]).unwrap();
        ed.model.add_wall_type("Zz W2", &[layer.clone(), layer]).unwrap();
        select(&mut st, &ed, "Zz Old");
        let before = usage_total(&st.usage);
        assert_eq!(before, 3);
        let d = st.draft.as_mut().unwrap();
        assert!(confirm_replace(&mut ed, d, "Zz New"));
        assert!(!has(&ed, "Zz Old"));
        let moved = ed.model.material_usage("Zz New").unwrap();
        assert_eq!(usage_total(&moved), before);
        // a single undo restores every element and the material itself
        ed.undo();
        assert!(has(&ed, "Zz Old"));
        assert!(!has(&ed, "Zz New"));
        assert_eq!(usage_total(&ed.model.material_usage("Zz Old").unwrap()), before);
        assert_eq!(ed.model.material_usage("Zz New").map_or(0, |u| usage_total(&u)), 0);
    }

    #[test]
    fn rename_rejects_empty_and_taken_names() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz A");
        st.new_name = "Zz B".into();
        assert!(add_named(&mut ed, &mut st));
        // adding a taken name is refused with a note
        st.new_name = "Zz A".into();
        assert!(!add_named(&mut ed, &mut st));
        assert!(st.add_error.is_some());
        st.new_name = "   ".into();
        assert!(!add_named(&mut ed, &mut st));
        select(&mut st, &ed, "Zz B");
        let d = st.draft.as_mut().unwrap();
        d.name = "  ".into();
        assert!(!apply_draft(&mut ed, d));
        assert!(d.error.is_some());
        d.name = "Zz A".into();
        assert!(!apply_draft(&mut ed, d));
        assert!(d.error.is_some());
        assert!(has(&ed, "Zz A") && has(&ed, "Zz B"));
        d.name = "Zz C".into();
        assert!(apply_draft(&mut ed, d));
        assert!(d.error.is_none());
        assert!(has(&ed, "Zz C") && !has(&ed, "Zz B"));
    }

    #[test]
    fn colour_edit_round_trips_through_the_model() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Paint");
        let d = st.draft.as_mut().unwrap();
        d.color = Some(0x33_66_99);
        assert!(apply_draft(&mut ed, d));
        assert_eq!(color_of_named(&ed, "Zz Paint"), Some(0x33_66_99));
        // the list is cached: a frame's `sync` reloads it from the model before the card reopens
        sync(&mut st, &ed);
        select(&mut st, &ed, "Zz Paint");
        let d = st.draft.as_mut().unwrap();
        assert_eq!(d.color, Some(0x33_66_99));
        d.color = None;
        assert!(apply_draft(&mut ed, d));
        assert_eq!(color_of_named(&ed, "Zz Paint"), None);
    }

    #[test]
    fn project_question_sees_saves_and_discards_the_card() {
        let mut ed = Editor::new_in_memory().unwrap();
        let st = with_added(&mut ed, "Zz Card");
        WIN.with(|w| *w.borrow_mut() = st);
        assert_eq!(draft_name(), None, "a card just opened is no draft");
        fn edit(f: impl FnOnce(&mut Draft)) {
            WIN.with(|w| f(w.borrow_mut().draft.as_mut().unwrap()));
        }
        edit(|d| d.note = "Zz note".into());
        assert_eq!(draft_name().as_deref(), Some("Zz Card"));
        discard_drafts();
        assert_eq!(draft_name(), None, "Discard drops the edit");
        assert!(WIN.with(|w| w.borrow().draft.as_ref().is_some_and(|d| d.note.is_empty())));
        // an empty name refuses: the window opens, the card keeps its edits and reason
        edit(|d| {
            d.note = "Zz note".into();
            d.name = " ".into();
        });
        assert!(!save_drafts(&mut ed));
        assert!(WIN.with(|w| {
            let st = w.borrow();
            st.open && st.draft.as_ref().is_some_and(|d| d.error.is_some() && d.note == "Zz note")
        }));
        assert_eq!(draft_name().as_deref(), Some("Zz Card"));
        edit(|d| d.name = "Zz Card".into());
        assert!(save_drafts(&mut ed));
        assert_eq!(draft_name(), None);
        let note = |ed: &Editor| {
            let all = ed.model.materials().unwrap();
            all.iter().find(|m| m.spec.name == "Zz Card").map(|m| m.spec.note.clone())
        };
        assert_eq!(note(&ed).as_deref(), Some("Zz note"));
        ed.undo();
        assert_eq!(note(&ed).as_deref(), Some(""), "one undo step");
    }

    fn card(st: &Win) -> &Draft {
        st.draft.as_ref().expect("a card")
    }

    #[test]
    fn unrelated_model_edit_keeps_card_edits() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Keep");
        st.draft.as_mut().unwrap().note = "Zz note".into();
        let rev = ed.revision;
        let _ = with_added(&mut ed, "Zz Other");
        assert_ne!(ed.revision, rev, "the other window's add is a model change");
        sync(&mut st, &ed);
        assert_eq!(st.rev, Some(ed.cache_key()));
        assert!(st.list.iter().any(|m| m.spec.name == "Zz Other"), "the list follows the model");
        let d = card(&st);
        assert_eq!(d.note, "Zz note", "edits survive a change to another material");
        assert!(!d.conflict);
        assert!(d.changed(&st.list), "still a draft for the close question");
    }

    #[test]
    fn clean_card_follows_the_model_without_a_conflict() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Clean");
        assert!(apply_draft(&mut ed, {
            let d = st.draft.as_mut().unwrap();
            d.color = Some(0x11_22_33);
            d
        }));
        sync(&mut st, &ed);
        assert!(!card(&st).conflict, "own Apply is no conflict");
        ed.undo();
        sync(&mut st, &ed);
        let d = card(&st);
        assert!(!d.conflict, "an unedited card just shows the stored material");
        assert_eq!(d.color, color_of_named(&ed, "Zz Clean"));
    }

    /// A card with a note typed after its colour was applied, then the apply undone.
    fn conflicted(ed: &mut Editor) -> Win {
        let mut st = with_added(ed, "Zz Mat");
        let d = st.draft.as_mut().unwrap();
        d.color = Some(0x11_22_33);
        assert!(apply_draft(ed, d));
        sync(&mut st, ed);
        st.draft.as_mut().unwrap().note = "Zz note".into();
        ed.undo();
        assert_ne!(color_of_named(ed, "Zz Mat"), Some(0x11_22_33));
        sync(&mut st, ed);
        st
    }

    #[test]
    fn undo_touching_the_material_shows_a_conflict_and_keeps_edits() {
        let mut ed = Editor::new_in_memory().unwrap();
        let st = conflicted(&mut ed);
        let d = card(&st);
        assert!(d.conflict);
        assert_eq!(d.note, "Zz note", "nothing is dropped silently");
        assert_eq!(d.color, Some(0x11_22_33));
        WIN.with(|w| *w.borrow_mut() = st);
        assert_eq!(draft_name().as_deref(), Some("Zz Mat"), "the close question still asks");
        // the window draws the conflict note and its buttons
        WIN.with(|w| w.borrow_mut().open = true);
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(egui::RawInput::default(), |c| window(c, &mut ed));
        assert!(WIN.with(|w| card(&w.borrow()).conflict), "drawing does not answer it");
        WIN.with(|w| *w.borrow_mut() = Win::default());
    }

    #[test]
    fn keep_my_edits_clears_the_note_and_apply_writes_them() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = conflicted(&mut ed);
        keep_mine(&mut st);
        let d = st.draft.as_mut().unwrap();
        assert!(!d.conflict);
        assert_eq!(d.note, "Zz note");
        assert!(apply_draft(&mut ed, d));
        assert_eq!(color_of_named(&ed, "Zz Mat"), Some(0x11_22_33));
        let all = ed.model.materials().unwrap();
        let m = all.iter().find(|m| m.spec.name == "Zz Mat").unwrap();
        assert_eq!(m.spec.note, "Zz note");
        sync(&mut st, &ed);
        assert!(!card(&st).conflict && !card(&st).changed(&st.list));
    }

    #[test]
    fn reload_shows_the_stored_material_and_drops_edits() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = conflicted(&mut ed);
        reload_card(&mut st, &ed);
        let d = card(&st);
        assert!(!d.conflict);
        assert!(d.note.is_empty());
        assert_eq!(d.color, color_of_named(&ed, "Zz Mat"));
        assert!(!d.changed(&st.list));
    }

    #[test]
    fn edited_card_of_a_removed_material_asks_too() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Gone");
        st.draft.as_mut().unwrap().note = "Zz note".into();
        ed.undo();
        assert!(!has(&ed, "Zz Gone"));
        sync(&mut st, &ed);
        assert!(card(&st).conflict, "the material went away under the edits");
        reload_card(&mut st, &ed);
        assert!(st.draft.is_none(), "Reload of a removed material closes the card");
    }

    #[test]
    fn conflict_strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for k in ["materials.mat.conflict", "materials.mat.keep", "materials.mat.reload"] {
                assert_ne!(c.t(k), k, "missing {k}");
            }
        }
    }

    /// The note stored for `name`.
    fn note_of(ed: &Editor, name: &str) -> String {
        let all = ed.model.materials().unwrap();
        all.iter().find(|m| m.spec.name == name).map(|m| m.spec.note.clone()).unwrap_or_default()
    }

    #[test]
    fn project_save_over_an_unanswered_conflict_shows_it_and_writes_nothing() {
        let mut ed = Editor::new_in_memory().unwrap();
        let st = conflicted(&mut ed);
        WIN.with(|w| *w.borrow_mut() = st);
        let rev = ed.revision;
        assert!(!save_drafts(&mut ed), "Save from the close question stops on the conflict");
        assert_eq!(ed.revision, rev);
        assert!(note_of(&ed, "Zz Mat").is_empty(), "nothing written over the change");
        assert!(WIN.with(|w| w.borrow().open && card(&w.borrow()).conflict), "shown first");
        assert_eq!(draft_name().as_deref(), Some("Zz Mat"), "edits kept");
        // Keep my edits answers it: Save then writes the card
        WIN.with(|w| keep_mine(&mut w.borrow_mut()));
        assert!(save_drafts(&mut ed));
        assert_eq!(note_of(&ed, "Zz Mat"), "Zz note");
        assert_eq!(color_of_named(&ed, "Zz Mat"), Some(0x11_22_33));
        WIN.with(|w| *w.borrow_mut() = Win::default());
    }

    #[test]
    fn project_save_finds_a_conflict_the_card_has_not_seen_yet() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Late");
        let d = st.draft.as_mut().unwrap();
        d.color = Some(0x11_22_33);
        assert!(apply_draft(&mut ed, d));
        sync(&mut st, &ed);
        st.draft.as_mut().unwrap().note = "Zz note".into();
        let rev = ed.revision;
        ed.undo();
        // no frame drew the window since: the undo is not synced, and the revision is put back
        ed.restore_revision(rev);
        WIN.with(|w| *w.borrow_mut() = st);
        assert!(!save_drafts(&mut ed));
        assert!(WIN.with(|w| w.borrow().open && card(&w.borrow()).conflict));
        assert!(note_of(&ed, "Zz Late").is_empty());
        WIN.with(|w| *w.borrow_mut() = Win::default());
    }

    #[test]
    fn card_list_reads_anew_when_an_older_revision_is_put_back() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Keep");
        st.draft.as_mut().unwrap().note = "Zz note".into();
        let rev = ed.revision;
        let _ = with_added(&mut ed, "Zz Other");
        ed.restore_revision(rev);
        WIN.with(|w| *w.borrow_mut() = st);
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |c| window(c, &mut ed));
        WIN.with(|w| {
            let st = w.borrow();
            assert!(st.list.iter().any(|m| m.spec.name == "Zz Other"), "keyed on cache_key");
            assert_eq!(card(&st).note, "Zz note");
        });
        WIN.with(|w| *w.borrow_mut() = Win::default());
    }

    #[test]
    fn hatch_options_keep_library_and_unknown_values() {
        let lib = vec!["Zz Lib".to_string()];
        let o = hatch_options(MATERIAL_HATCHES[0], &lib);
        assert_eq!(o.len(), MATERIAL_HATCHES.len() + 1);
        assert!(o.iter().any(|x| x.0 == format!("{LIB_HATCH_PREFIX}Zz Lib")));
        let o = hatch_options("lib:gone", &lib);
        assert_eq!(o.last().unwrap().0, "lib:gone");
    }

    #[test]
    fn window_draws_every_state_without_panicking() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = with_added(&mut ed, "Zz Shown");
        let layer = ddc_model::LayerSpec {
            material: "Zz Shown".into(),
            thickness: 0.1,
        };
        ed.model.add_wall_type("Zz W", &[layer]).unwrap();
        select(&mut st, &ed, "Zz Shown");
        st.draft.as_mut().unwrap().pending = Some(Pending::Replace("concrete".into()));
        st.open = true;
        st.search = "no such material".into();
        WIN.with(|w| *w.borrow_mut() = st);
        let ctx = ddc_ui::theme::headless_ctx();
        let raw = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            ..Default::default()
        };
        for _ in 0..3 {
            let _ = ctx.run(raw(), |c| window(c, &mut ed));
        }
        WIN.with(|w| w.borrow_mut().search.clear());
        let _ = ctx.run(raw(), |c| window(c, &mut ed));
    }

    #[test]
    fn window_source_uses_only_kit_widgets() {
        let src = include_str!("materials_win.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for raw in [
            "ComboBox",
            "selectable_label",
            "colored_label",
            "egui::Button::new",
            "egui::Grid",
        ] {
            assert!(!body.contains(raw), "raw widget {raw} in the window code");
        }
    }
}
