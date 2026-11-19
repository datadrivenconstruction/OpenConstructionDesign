//! View templates: create from the current view, apply once to selected views, link and unlink.
//! A linked view cannot change the fields controlled by its template.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use ddc_model::{TemplateField, ViewKey, ViewTemplate};
use ddc_ui::theme::{radius, space, stroke, Role};
use ddc_ui::widgets::{self, RowState, TextField, Tone};
use ddc_ui::Theme;

use super::{run, view_label_in};
use crate::editor::Editor;
use crate::ui_kit as kit;

/// Catalog text in the window's own language.
fn t(ru: bool, key: &str) -> String {
    crate::tr_lang(ru, key, &[])
}

/// Id of the new-template name field: the empty state's action focuses it.
const NAME_FIELD: &str = "f5.tpl.new_name_field";

#[derive(Default)]
struct Win {
    open: bool,
    rev: Option<(u64, u64)>,
    /// Language the cached view labels were made in.
    ru: Option<bool>,
    list: Vec<ViewTemplate>,
    views: Vec<ViewKey>,
    /// View label and linked template: the window is drawn every frame, so the model is not queried.
    labels: HashMap<ViewKey, (String, Option<i64>)>,
    sel: Option<i64>,
    name: String,
    fields: HashSet<TemplateField>,
    new_name: String,
    checked: HashSet<ViewKey>,
    /// Template whose deletion waits for the confirmation.
    confirm: Option<i64>,
    /// Event raised by the details pane this frame.
    ask: Option<Ask>,
}

/// Steps of the delete confirmation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ask {
    Open(i64),
    Confirm,
    Cancel,
}

/// New armed template and the one to delete now: only `Confirm` after `Open` deletes.
fn delete_step(armed: Option<i64>, ev: Option<Ask>) -> (Option<i64>, Option<i64>) {
    match ev {
        Some(Ask::Open(i)) => (Some(i), None),
        Some(Ask::Confirm) => (None, armed),
        Some(Ask::Cancel) => (None, None),
        None => (armed, None),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Plural {
    One,
    Few,
    Many,
}

fn plural(ru: bool, n: usize) -> Plural {
    if !ru {
        return if n == 1 { Plural::One } else { Plural::Many };
    }
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 {
        Plural::One
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        Plural::Few
    } else {
        Plural::Many
    }
}

/// "3 views" in the given language.
fn views_text(ru: bool, n: usize) -> String {
    let key = match plural(ru, n) {
        Plural::One => "view_template.views.one",
        Plural::Few => "view_template.views.few",
        Plural::Many => "view_template.views.many",
    };
    crate::tr_lang(ru, key, &[("n", &n.to_string())])
}

/// How many views are linked to template `id`.
fn linked_count(labels: &HashMap<ViewKey, (String, Option<i64>)>, id: i64) -> usize {
    labels.values().filter(|x| x.1 == Some(id)).count()
}

/// Why a name cannot be used: key of the reason. `except` is the template being renamed.
fn name_block(raw: &str, list: &[ViewTemplate], except: Option<i64>) -> Option<&'static str> {
    let n = raw.trim();
    if n.is_empty() {
        Some("view_template.name_blank")
    } else if list.iter().any(|x| x.name == n && Some(x.id) != except) {
        Some("view_template.name_taken")
    } else {
        None
    }
}

/// Confirmation text: the body names the template and the number of views using it.
fn delete_body(ru: bool, name: &str, n: usize) -> String {
    if n == 0 {
        crate::tr_lang(ru, "view_template.delete.unused", &[("name", name)])
    } else {
        crate::tr_lang(
            ru,
            "view_template.delete.body",
            &[("name", name), ("used", &views_text(ru, n))],
        )
    }
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

pub(super) fn open() {
    WIN.with(|w| w.borrow_mut().open = true);
}

/// Test only: whether the window is open.
#[cfg(test)]
pub(super) fn is_open() -> bool {
    WIN.with(|w| w.borrow().open)
}

/// Test only: back to the closed default state of this thread.
#[cfg(test)]
pub(super) fn reset() {
    WIN.with(|w| *w.borrow_mut() = Win::default());
}

/// Opens the window with the first template selected, so the details pane is drawn too.
#[cfg(test)]
pub(super) fn open_for_layout(ed: &Editor, ru: bool) {
    WIN.with(|w| {
        let mut st = w.borrow_mut();
        st.open = true;
        reload(&mut st, ed, ru);
        let first = st.list.first().map(|x| x.id);
        select(&mut st, first);
    });
}

/// All project views: level plans, sections and elevations, 3D.
fn all_views(ed: &Editor) -> Vec<ViewKey> {
    let m = &ed.model;
    let mut v: Vec<ViewKey> = m
        .level_list()
        .unwrap_or_default()
        .into_iter()
        .map(|l| ViewKey::Plan(l.0))
        .collect();
    v.extend(
        m.view_defs()
            .unwrap_or_default()
            .into_iter()
            .map(|d| ViewKey::Section(d.0)),
    );
    v.push(ViewKey::ThreeD);
    v
}

fn field_set(ui: &mut egui::Ui, th: &Theme, ru: bool, set: &mut HashSet<TemplateField>) {
    for f in TemplateField::ALL {
        let mut on = set.contains(&f);
        if widgets::checkbox(ui, th, &mut on, &t(ru, &format!("view_gfx.field.{}", f.key()))).changed() {
            if on {
                set.insert(f);
            } else {
                set.remove(&f);
            }
        }
    }
}

fn ordered(set: &HashSet<TemplateField>) -> Vec<TemplateField> {
    TemplateField::ALL
        .into_iter()
        .filter(|f| set.contains(f))
        .collect()
}

fn select(st: &mut Win, id: Option<i64>) {
    st.sel = id;
    let tpl = id.and_then(|i| st.list.iter().find(|x| x.id == i));
    st.name = tpl.map_or(String::new(), |x| x.name.clone());
    st.fields = tpl.map_or(HashSet::new(), |x| x.fields.iter().copied().collect());
}

fn reload(st: &mut Win, ed: &Editor, ru: bool) {
    st.ru = Some(ru);
    st.list = ed.model.view_templates().unwrap_or_default();
    st.views = all_views(ed);
    st.labels = st
        .views
        .iter()
        .map(|v| {
            let linked = ed.model.view_gfx(*v).ok().and_then(|s| s.template);
            (*v, (view_label_in(ru, ed, *v), linked))
        })
        .collect();
    st.rev = Some(ed.cache_key());
    st.checked.retain(|v| st.views.contains(v));
    if st.sel.is_some_and(|i| !st.list.iter().any(|x| x.id == i)) {
        select(st, None);
    }
}

fn label_of(st: &Win, v: ViewKey) -> &str {
    st.labels.get(&v).map_or("", |x| x.0.as_str())
}

fn create_ui(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win, active: ViewKey, ru: bool) {
    let th = ddc_ui::theme::of(ui.ctx());
    kit::section_title(
        ui,
        crate::tr_lang(ru, "view_gfx.tpl.create_from", &[("view", label_of(st, active))]),
    );
    ui.horizontal(|ui| {
        let hint = t(ru, "view_gfx.tpl.new_name");
        let w = ui.available_width() * 0.6;
        TextField::new(&mut st.new_name)
            .id(egui::Id::new(NAME_FIELD))
            .hint(&hint)
            .width(w)
            .labelled_by(egui::Id::new("f5.tpl.new_name_label"), &hint)
            .show(ui, &th);
        let name = st.new_name.trim().to_string();
        let why = name_block(&name, &st.list, None).map(|k| t(ru, k));
        if kit::primary_click(ui, t(ru, "view_gfx.tpl.create"), why.as_deref()) {
            let fields = TemplateField::ALL;
            let n = name.clone();
            if run(ed, move |m| {
                m.template_from_view(&n, active, &fields).map(|_| ())
            }) {
                st.new_name.clear();
                reload(st, ed, ru);
                let id = st.list.iter().find(|x| x.name == name).map(|x| x.id);
                select(st, id);
            }
        }
    });
}

fn details_ui(ui: &mut egui::Ui, ed: &mut Editor, st: &mut Win, active: ViewKey, ru: bool) {
    let Some(id) = st.sel else {
        ui.weak(t(ru, "view_gfx.tpl.pick_hint"));
        return;
    };
    let th = ddc_ui::theme::of(ui.ctx());
    let why_name = name_block(&st.name, &st.list, Some(id)).map(|k| t(ru, k));
    ui.horizontal(|ui| {
        ui.label(t(ru, "view_gfx.tpl.name"));
        let w = ui.available_width();
        TextField::new(&mut st.name)
            .id_salt("f5.tpl.name_field")
            .width(w)
            .error(why_name.is_some())
            .labelled_by(egui::Id::new("f5.tpl.name_label"), &t(ru, "view_gfx.tpl.name"))
            .show(ui, &th);
    });
    if let Some(w) = &why_name {
        kit::warn_note(ui, w.as_str());
    }
    ui.label(t(ru, "view_gfx.tpl.controls"));
    field_set(ui, &th, ru, &mut st.fields);
    let spec = st.list.iter().find(|x| x.id == id).map(|x| x.spec.clone());
    ui.horizontal(|ui| {
        if kit::primary_click(ui, t(ru, "view_gfx.tpl.save"), why_name.as_deref()) {
            if let Some(sp) = spec.clone() {
                let (name, fields) = (st.name.clone(), ordered(&st.fields));
                run(ed, move |m| m.edit_view_template(id, &name, &fields, &sp));
            }
        }
        if kit::secondary_button(ui, t(ru, "view_gfx.tpl.take"))
            .on_hover_text(label_of(st, active))
            .clicked()
        {
            let sp = ed.model.view_gfx(active).map(|s| s.effective);
            if let Ok(sp) = sp {
                let (name, fields) = (st.name.clone(), ordered(&st.fields));
                run(ed, move |m| m.edit_view_template(id, &name, &fields, &sp));
            }
        }
    });
    ui.separator();
    kit::section_title(ui, t(ru, "view_gfx.tpl.views"));
    let row = th.d().row;
    egui::ScrollArea::vertical()
        .id_salt("view_gfx.tpl.views")
        .max_height(row * 9.0)
        .show(ui, |ui| {
            for v in &st.views {
                let (label, linked) = st.labels.get(v).map_or(("", None), |x| (x.0.as_str(), x.1));
                let chip = linked
                    .and_then(|l| st.list.iter().find(|x| x.id == l))
                    .map(|n| n.name.as_str());
                ui.horizontal(|ui| {
                    let mut on = st.checked.contains(v);
                    if widgets::checkbox(ui, &th, &mut on, label).changed() {
                        if on {
                            st.checked.insert(*v);
                        } else {
                            st.checked.remove(v);
                        }
                    }
                    if let Some(n) = chip {
                        widgets::badge(ui, &th, n, Tone::Accent)
                            .on_hover_text(crate::tr_lang(ru, "view_gfx.tpl.linked_to", &[("name", n)]));
                    }
                });
            }
        });
    let marked: Vec<ViewKey> = st
        .views
        .iter()
        .copied()
        .filter(|v| st.checked.contains(v))
        .collect();
    let why = marked.is_empty().then(|| t(ru, "view_template.pick_blocked"));
    ui.horizontal(|ui| {
        if quiet_action(ui, &t(ru, "view_gfx.tpl.apply"), why.as_deref()) {
            let vs = marked.clone();
            if run(ed, move |m| m.apply_view_template(id, &vs)) {
                ed.status = Some(t(ru, "view_gfx.tpl.applied"));
            }
        }
        if quiet_action(ui, &t(ru, "view_gfx.tpl.link"), why.as_deref()) {
            let vs = marked.clone();
            run(ed, move |m| m.link_view_template(Some(id), &vs));
        }
        if quiet_action(ui, &t(ru, "view_gfx.tpl.unlink"), why.as_deref()) {
            let vs = marked.clone();
            run(ed, move |m| m.link_view_template(None, &vs));
        }
    });
    ui.separator();
    // Delete only asks; the confirmation below runs the op
    if kit::danger_button(ui, t(ru, "view_gfx.delete"), true).clicked() {
        st.ask = Some(Ask::Open(id));
    }
}

/// Outlined action with the reason it is off shown on hover.
fn quiet_action(ui: &mut egui::Ui, text: &str, why: Option<&str>) -> bool {
    let r = ui
        .add_enabled_ui(why.is_none(), |ui| kit::secondary_button(ui, text))
        .inner;
    match why {
        Some(w) => {
            r.on_disabled_hover_text(w);
            false
        }
        None => r.clicked(),
    }
}

/// Deletes the template in one undo step.
fn delete_confirmed(ed: &mut Editor, id: i64) -> bool {
    run(ed, move |m| m.delete_view_template(id))
}

/// Modal asking before a template is deleted; names it and how many views use it.
fn confirm_modal(ctx: &egui::Context, name: &str, n: usize, ru: bool) -> Option<Ask> {
    let th = ddc_ui::theme::of(ctx);
    let mut ev = None;
    let out = kit::modal_dialog("f5.tpl.delete", ctx, |ui| {
        ui.set_width(th.d().row * 14.0);
        kit::modal_title(ui, t(ru, "view_template.delete.title"));
        ui.label(delete_body(ru, name, n));
        if n > 0 {
            ui.label(t(ru, "view_template.delete.keeps"));
        }
        ui.label(egui::RichText::new(t(ru, "view_template.delete.undo")).color(th.sem().text_secondary));
        kit::dialog_footer(ui, |ui| {
            if kit::danger_button(ui, t(ru, "view_template.delete.confirm"), true).clicked() {
                ev = Some(Ask::Confirm);
            }
            if kit::cancel_button(ui, t(ru, "view_gfx.cancel")).clicked() {
                ev = Some(Ask::Cancel);
            }
        });
    });
    if out.should_close() {
        ev = Some(Ask::Cancel);
    }
    ev
}

/// Layout probe seam: the delete question; `Some(true)` confirmed, `Some(false)` cancelled.
#[cfg(test)]
pub(crate) fn layout_confirm(ctx: &egui::Context, name: &str, n: usize, ru: bool) -> Option<bool> {
    confirm_modal(ctx, name, n, ru).map(|a| matches!(a, Ask::Confirm))
}

/// One template row: elided name, muted number of views using it.
fn template_row(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    selected: bool,
    name: &str,
    n: usize,
) -> egui::Response {
    let d = th.d();
    let size = egui::vec2(
        ui.available_width(),
        d.row + th.font(Role::Caption).size + space::XS,
    );
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    // a row of the list is a selectable item named by its template
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, name)
    });
    let state = if selected { RowState::Selected } else { RowState::Normal };
    let (fill, ink, muted) = widgets::table::row_colors(th.sem(), state, resp.hovered());
    ui.painter().rect_filled(rect, radius::SM, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            radius::SM,
            egui::Stroke::new(stroke::FOCUS, th.sem().focus_ring),
            egui::StrokeKind::Inside,
        );
    }
    let left = rect.left() + space::SM;
    let line1 = egui::Rect::from_min_max(
        egui::pos2(left, rect.top()),
        egui::pos2(rect.right() - space::SM, rect.top() + d.row),
    );
    let cut = widgets::text::paint_in(ui, line1, name, th.font(Role::Body), ink, egui::Align::Min);
    let line2 = egui::Rect::from_min_max(
        egui::pos2(left, rect.top() + d.row - space::XS),
        egui::pos2(rect.right() - space::SM, rect.bottom()),
    );
    let count = views_text(ru, n);
    widgets::text::paint_in(ui, line2, &count, th.font(Role::Caption), muted, egui::Align::Min);
    if cut {
        resp.on_hover_text(name)
    } else {
        resp
    }
}

pub(super) fn window(ctx: &egui::Context, ed: &mut Editor, active: ViewKey, ru: bool) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if st.open {
        if st.rev != Some(ed.cache_key()) || st.ru != Some(ru) {
            reload(&mut st, ed, ru);
        }
        let mut open = true;
        crate::ui_kit::Dialog::new("f5.templates", t(ru, "view_gfx.tpl.title"), crate::ui_kit::DialogSize::Wide)
            .id(egui::Id::new("f5.templates.win"))
            .open(&mut open)
            .show(ctx, |ui| {
                create_ui(ui, ed, &mut st, active, ru);
                ui.separator();
                ui.horizontal_top(|ui| {
                    let w = ui.available_width() * 0.34;
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.strong(t(ru, "view_gfx.tpl.list"));
                        if let Some(k) = crate::empty_kind::templates(st.list.len()) {
                            if crate::ui_kit::empty_state_lang(ui, ru, k.what(), k.has_action()) {
                                ui.memory_mut(|m| m.request_focus(egui::Id::new(NAME_FIELD)));
                            }
                        }
                        let th = ddc_ui::theme::of(ui.ctx());
                        let mut pick = None;
                        for x in &st.list {
                            let n = linked_count(&st.labels, x.id);
                            if template_row(ui, &th, ru, st.sel == Some(x.id), &x.name, n).clicked() {
                                pick = Some(x.id);
                            }
                        }
                        if pick.is_some() {
                            select(&mut st, pick);
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| details_ui(ui, ed, &mut st, active, ru));
                });
            });
        let mut ev = st.ask.take();
        if ev.is_none() {
            if let Some(id) = st.confirm {
                ev = match st.list.iter().find(|x| x.id == id) {
                    Some(x) => confirm_modal(ctx, &x.name, linked_count(&st.labels, id), ru),
                    None => Some(Ask::Cancel),
                };
            }
        }
        let (armed, go) = delete_step(st.confirm, ev);
        st.confirm = if open { armed } else { None };
        if let Some(id) = go {
            delete_confirmed(ed, id);
        }
        st.open = open;
    }
    WIN.with(|w| *w.borrow_mut() = st);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_views_include_plans_and_3d() {
        let ed = Editor::new_in_memory().unwrap();
        let v = all_views(&ed);
        assert!(v.contains(&ViewKey::Plan(ed.level)));
        assert_eq!(v.last(), Some(&ViewKey::ThreeD));
    }

    const SRC: &str = include_str!("template_win.rs");

    fn code() -> &'static str {
        &SRC[..SRC.find("#[cfg(test)]\nmod tests").unwrap()]
    }

    fn json_keys(lang: &str) -> std::collections::BTreeSet<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ui/data/i18n")
            .join(lang)
            .join("view_template.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v.as_object().unwrap().keys().cloned().collect()
    }

    /// Editor with one template `name` linked to the first plan and the 3D view.
    fn with_template(name: &'static str) -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let plan = ViewKey::Plan(ed.level);
        assert!(run(&mut ed, move |m| m
            .template_from_view(name, plan, &TemplateField::ALL)
            .map(|_| ())));
        let id = ed.model.view_templates().unwrap()[0].id;
        assert!(run(&mut ed, move |m| m
            .link_view_template(Some(id), &[plan, ViewKey::ThreeD])));
        (ed, id)
    }

    fn linked(ed: &Editor, id: i64) -> usize {
        let mut st = Win::default();
        reload(&mut st, ed, false);
        linked_count(&st.labels, id)
    }

    fn frames(ed: &mut Editor, n: usize) {
        let ctx = egui::Context::default();
        // the empty state paints a named font weight: it exists from the pass after install
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let active = ViewKey::Plan(ed.level);
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |c| window(c, ed, active, false));
        }
    }

    #[test]
    fn delete_needs_a_confirm_for_the_same_template() {
        assert_eq!(delete_step(None, None), (None, None));
        let (armed, go) = delete_step(None, Some(Ask::Open(7)));
        assert_eq!((armed, go), (Some(7), None));
        assert_eq!(delete_step(armed, None), (Some(7), None));
        assert_eq!(delete_step(armed, Some(Ask::Confirm)), (None, Some(7)));
        assert_eq!(delete_step(armed, Some(Ask::Cancel)), (None, None));
        // a confirm with nothing asked deletes nothing
        assert_eq!(delete_step(None, Some(Ask::Confirm)), (None, None));
        assert_eq!(delete_step(Some(7), Some(Ask::Open(8))), (Some(8), None));
    }

    #[test]
    fn cancel_keeps_the_template_and_its_links() {
        let (mut ed, id) = with_template("Plans");
        open();
        frames(&mut ed, 2);
        WIN.with(|w| {
            let mut w = w.borrow_mut();
            w.confirm = Some(id);
            w.ask = Some(Ask::Cancel);
        });
        frames(&mut ed, 2);
        assert_eq!(WIN.with(|w| w.borrow().confirm), None);
        assert_eq!(ed.model.view_templates().unwrap().len(), 1);
        assert_eq!(linked(&ed, id), 2);
    }

    #[test]
    fn opening_the_ask_does_not_delete_and_waits_for_confirm() {
        let (mut ed, id) = with_template("Plans");
        open();
        frames(&mut ed, 2);
        WIN.with(|w| w.borrow_mut().ask = Some(Ask::Open(id)));
        frames(&mut ed, 3);
        assert_eq!(WIN.with(|w| w.borrow().confirm), Some(id));
        assert_eq!(ed.model.view_templates().unwrap().len(), 1);
    }

    #[test]
    fn confirmed_delete_is_one_undo_step() {
        let (mut ed, id) = with_template("Plans");
        open();
        frames(&mut ed, 2);
        WIN.with(|w| {
            let mut w = w.borrow_mut();
            w.confirm = Some(id);
            w.ask = Some(Ask::Confirm);
        });
        frames(&mut ed, 2);
        assert!(ed.model.view_templates().unwrap().is_empty());
        assert_eq!(WIN.with(|w| w.borrow().confirm), None);
        ed.undo();
        assert_eq!(ed.model.view_templates().unwrap().len(), 1);
        assert_eq!(linked(&ed, id), 2);
    }

    #[test]
    fn links_are_counted_per_template() {
        let (ed, id) = with_template("Plans");
        assert_eq!(linked(&ed, id), 2);
        assert_eq!(linked(&ed, id + 1), 0);
    }

    #[test]
    fn name_is_blocked_when_blank_or_taken() {
        let (ed, id) = with_template("Plans");
        let list = ed.model.view_templates().unwrap();
        assert_eq!(name_block("", &list, None), Some("view_template.name_blank"));
        assert_eq!(name_block("  \t ", &list, None), Some("view_template.name_blank"));
        assert_eq!(name_block("Plans", &list, None), Some("view_template.name_taken"));
        assert_eq!(name_block(" Plans ", &list, None), Some("view_template.name_taken"));
        assert_eq!(name_block("Sections", &list, None), None);
        // renaming keeps its own name free
        assert_eq!(name_block("Plans", &list, Some(id)), None);
    }

    #[test]
    fn usage_text_in_both_languages() {
        assert_eq!(views_text(false, 1), "1 view");
        assert_eq!(views_text(false, 3), "3 views");
        assert_eq!(views_text(false, 0), "0 views");
        let (one, few, many) = (views_text(true, 1), views_text(true, 2), views_text(true, 5));
        assert!(one != few && few != many && one != many);
        // 21 reads like 1, 11 and 12 like 5
        assert_eq!(&views_text(true, 21)[2..], &one[1..]);
        assert_eq!(&views_text(true, 22)[2..], &few[1..]);
        assert_eq!(&views_text(true, 11)[2..], &many[1..]);
        assert_eq!(&views_text(true, 12)[2..], &many[1..]);
        let en = delete_body(false, "Plans", 3);
        assert!(en.contains("Plans") && en.contains("3 views"));
        let ru = delete_body(true, "Plans", 3);
        assert!(ru.contains("Plans") && ru.contains(&views_text(true, 3)));
        assert_ne!(en, ru);
        for lang in [false, true] {
            let none = delete_body(lang, "Plans", 0);
            assert!(none.contains("Plans") && !none.contains("{"));
            assert_ne!(none, delete_body(lang, "Plans", 2));
        }
    }

    #[test]
    fn window_renders_headless_with_and_without_the_ask() {
        let (mut ed, id) = with_template("Plans");
        open();
        frames(&mut ed, 3);
        WIN.with(|w| w.borrow_mut().sel = Some(id));
        frames(&mut ed, 3);
        WIN.with(|w| w.borrow_mut().confirm = Some(id));
        frames(&mut ed, 3);
        assert_eq!(ed.model.view_templates().unwrap().len(), 1);
        let mut empty = Editor::new_in_memory().unwrap();
        WIN.with(|w| *w.borrow_mut() = Win::default());
        open();
        frames(&mut empty, 3);
    }

    #[test]
    fn strings_exist_in_both_languages_without_touching_the_global_one() {
        use ddc_ui::i18n::{Catalog, Lang};
        let (en, ru) = (json_keys("en"), json_keys("ru"));
        assert_eq!(en, ru);
        let mut used = std::collections::BTreeSet::new();
        for (at, _) in code().match_indices("\"view_template.") {
            let rest = &code()[at + 1..];
            used.insert(rest[..rest.find('"').unwrap()].to_string());
        }
        assert!(used.len() >= 10, "{used:?}");
        for k in &used {
            assert!(en.contains(k), "missing key {k}");
        }
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in &en {
                assert_ne!(&c.t(k), k, "{k} missing in {lang:?}");
            }
        }
    }

    #[test]
    fn no_raw_widgets_remain_and_delete_only_asks() {
        let s = code();
        for raw in [
            "egui::TextEdit",
            "egui::Button",
            "ui.button(",
            "ui.small_button(",
            "ui.checkbox(",
            "selectable_label(",
            "text_edit_singleline(",
            "desired_width(",
            "add_enabled(",
            "Color32",
        ] {
            assert!(!s.contains(raw), "left in the templates window: {raw}");
        }
        for kit in [
            "TextField::new(",
            "widgets::checkbox(",
            "primary_click(",
            "danger_button(",
            "modal_dialog(",
            "widgets::badge(",
        ] {
            assert!(s.contains(kit), "kit widget missing: {kit}");
        }
        // the Delete button only raises the ask; the op runs from the confirmation alone
        let at = s.find("danger_button(ui, t(ru, \"view_gfx.delete\")").unwrap();
        assert!(s[at..at + 200].contains("Ask::Open("));
        assert_eq!(s.matches("delete_view_template(").count(), 1);
    }

    #[test]
    fn field_order_is_stable() {
        let set: HashSet<_> = [TemplateField::Filters, TemplateField::Detail]
            .into_iter()
            .collect();
        assert_eq!(
            ordered(&set),
            [TemplateField::Detail, TemplateField::Filters]
        );
    }
}
