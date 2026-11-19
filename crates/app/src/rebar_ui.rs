// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Commands and windows for reinforcement and the analytical model: a bar set for the selected hosts
//! (longitudinal, stirrups, meshes, cover), a bending schedule with CSV export, deletion,
//! analytics display, its warnings, element supports/offsets, and JSON export.
//! Edits go through `Editor::run_op` (one action is one undo step); lengths in windows are mm.

use std::cell::RefCell;
use std::sync::Arc;

use ddc_model::{
    AnRelease, Analytical, AnalyticalAdj, Model, RebarFaces, RebarHook, RebarLayout, RebarPath,
    RebarPos, RebarRow, RebarSetSpec, RebarShape,
};
use ddc_ui::widgets::type_pick::{Pick, PickItem, PickTexts, TypePick};
use ddc_ui::widgets::{self, Button, NumberField, NumberFormat, TextField};
use ddc_ui::Theme;

use crate::edit3d::pick::Cat;
use crate::topo::LenUnit;
use crate::editor::bodies::BodyKind;
use crate::editor::{OpFn, Sel};
use crate::scene_ids::EXT_REBAR;
use crate::shell::EditorApp;
use crate::status::Severity;

fn t(key: &str) -> String {
    #[cfg(test)]
    if let Some(ru) = TEST_RU.with(|c| c.get()) {
        return crate::tr_lang(ru, key, &[]);
    }
    crate::tr(key, &[])
}

/// Language of this test thread for `t`, so layout probes never touch the global one.
#[cfg(test)]
thread_local! {
    static TEST_RU: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// One frame of the add-rebar form for the layout contract: hosts as (kind, id) with type names,
/// one of the tab indices 0..4, and the language of `t` for this thread.
#[cfg(test)]
pub(crate) fn layout_frame(
    app: &mut EditorApp,
    ctx: &egui::Context,
    hosts: &[(&str, &str)],
    tab: usize,
) {
    TEST_RU.with(|c| c.set(Some(matches!(app.sh.cat.lang, ddc_ui::i18n::Lang::Ru))));
    let mut f = Form::default();
    f.hosts = hosts.iter().enumerate().map(|(i, h)| (h.0.to_string(), i as i64 + 1)).collect();
    f.names = hosts.iter().map(|h| h.1.to_string()).collect();
    f.tab = TABS[tab].0;
    f.rtype = app.ed.model.rebar_types().unwrap_or_default().first().map_or(0, |x| x.id);
    f.covers = common_faces(&f.hosts).into_iter().map(|k| (k.to_string(), 25.0)).collect();
    let mut last = Form::default();
    let _ = form_window(app, ctx, f, &mut last);
}

const HOOKS: [RebarHook; 4] = [
    RebarHook::None,
    RebarHook::H90,
    RebarHook::H135,
    RebarHook::H180,
];
const SHAPES: [RebarShape; 3] = [RebarShape::Straight, RebarShape::L, RebarShape::U];

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Tab {
    #[default]
    Long,
    Stirrup,
    Area,
    Cover,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Long, "rebar.tab.long"),
    (Tab::Stirrup, "rebar.tab.stirrup"),
    (Tab::Area, "rebar.tab.area"),
    (Tab::Cover, "rebar.tab.cover"),
];

/// Host geometry the model builds bars in: linear (bars, stirrups) or plate (mesh).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fam {
    Linear,
    Plate,
}

/// The model's host rule (`ddc_model::rebar_linear_host`), unknown kinds excluded.
fn fam(kind: &str) -> Option<Fam> {
    if !ddc_model::REBAR_HOSTS.iter().any(|h| h.0 == kind) {
        None
    } else if ddc_model::rebar_linear_host(kind) {
        Some(Fam::Linear)
    } else {
        Some(Fam::Plate)
    }
}

/// The family every host shares; None for an empty or mixed pick.
fn hosts_fam(hosts: &[(String, i64)]) -> Option<Fam> {
    let first = fam(&hosts.first()?.0)?;
    hosts
        .iter()
        .all(|h| fam(&h.0) == Some(first))
        .then_some(first)
}

fn mixed(hosts: &[(String, i64)]) -> bool {
    !hosts.is_empty() && hosts_fam(hosts).is_none()
}

/// Whether the model accepts `layout` in a host of `kind`.
fn layout_fits(layout: &RebarLayout, kind: &str) -> bool {
    ddc_model::rebar_layout_fits(layout, kind)
}

/// Cover faces every host kind of the pick has, in the first host's order.
fn common_faces(hosts: &[(String, i64)]) -> Vec<&'static str> {
    let Some(first) = hosts.first() else {
        return vec![];
    };
    ddc_model::rebar_default_covers(&first.0)
        .iter()
        .map(|c| c.0)
        .filter(|f| {
            hosts
                .iter()
                .all(|h| ddc_model::rebar_default_covers(&h.0).iter().any(|c| c.0 == *f))
        })
        .collect()
}

/// Covers of the first host on the faces all hosts share, millimetres.
fn form_covers(m: &Model, hosts: &[(String, i64)]) -> Vec<(String, f64)> {
    let Some((k, id)) = hosts.first() else {
        return vec![];
    };
    let common = common_faces(hosts);
    m.rebar_covers(k, *id)
        .unwrap_or_else(|_| ddc_model::rebar_default_covers(k).to_vec())
        .into_iter()
        .filter(|(n, _)| common.contains(n))
        .map(|(n, v)| (n.to_string(), v * 1000.0))
        .collect()
}

/// Why `tab` cannot be used for these hosts (catalog key); None when it can.
fn tab_block(tab: Tab, hosts: &[(String, i64)]) -> Option<&'static str> {
    if tab == Tab::Cover {
        return common_faces(hosts)
            .is_empty()
            .then_some("rebar_hosts.why.no_common_face");
    }
    match (hosts_fam(hosts), tab) {
        _ if hosts.is_empty() => Some("rebar.no_host"),
        (None, _) => Some("rebar_hosts.why.mixed"),
        (Some(Fam::Linear), Tab::Area) => Some("rebar_hosts.why.area_linear"),
        (Some(Fam::Plate), Tab::Long | Tab::Stirrup) => Some("rebar_hosts.why.bars_plate"),
        _ => None,
    }
}

/// `tab` when it is open for these hosts, else the first open tab (Cover when none is).
fn fit_tab(tab: Tab, hosts: &[(String, i64)]) -> Tab {
    if tab_block(tab, hosts).is_none() {
        return tab;
    }
    TABS.iter()
        .map(|t| t.0)
        .find(|t| tab_block(*t, hosts).is_none())
        .unwrap_or(Tab::Cover)
}

/// Why Add cannot run (shown on the disabled button); None when every set it adds can be built.
fn add_block(ru: bool, f: &Form, types: &[ddc_model::RebarType]) -> Option<String> {
    if let Some(k) = tab_block(f.tab, &f.hosts) {
        return Some(crate::tr_lang(ru, k, &[]));
    }
    let need_minor = f.tab == Tab::Area;
    let has = |id: i64| types.iter().any(|ty| ty.id == id);
    if !has(f.rtype) || (need_minor && !has(f.minor_type)) {
        return Some(crate::tr_lang(ru, "rebar_hosts.why.no_type", &[]));
    }
    None
}

/// The hosts summary in one elided line; hover lists every host by name (and the whole
/// summary first when it was cut).
fn hosts_line(ui: &mut egui::Ui, th: &Theme, summary: &str, detail: &str) -> egui::Response {
    let col = th.sem().text_primary;
    let font = th.font(ddc_ui::theme::Role::Body);
    let g = widgets::elide(ui, summary, font, col, ui.available_width());
    let tip = if g.elided {
        format!("{summary}\n{detail}")
    } else {
        detail.to_string()
    };
    let (r, resp) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
    ui.painter().galley(r.min, g, col);
    resp.on_hover_text(tip)
}

/// Adds the form's sets in one step; refuses (no step) when Add is blocked or nothing fits.
fn add_sets(app: &mut EditorApp, f: &Form, types: &[ddc_model::RebarType]) -> bool {
    let ru = crate::ui_is_ru();
    let ss = specs(f);
    if let Some(why) = add_block(ru, f, types) {
        app.ed.say(Severity::Refusal, why);
        return false;
    }
    if ss.is_empty() || ss.len() != f.hosts.len() {
        app.ed.say(Severity::Refusal, crate::tr_lang(ru, "rebar_hosts.why.mixed", &[]));
        return false;
    }
    let n = ss.len();
    if run(app, move |m| m.add_rebar_sets(&ss).map(|_| ())) {
        app.ed.say(Severity::Info, crate::tr("rebar.added", &[("n", &n.to_string())]));
        return true;
    }
    false
}

/// Plural catalog key of a host kind count ("2 columns").
fn kind_count_key(kind: &str) -> String {
    format!("rebar_hosts.n.{kind}")
}

/// One line for the hosts: count per kind in a fixed order ("2 columns, 1 beam").
fn hosts_summary(ru: bool, hosts: &[(String, i64)]) -> String {
    let parts: Vec<String> = ddc_model::REBAR_HOSTS
        .iter()
        .filter_map(|(k, _)| {
            let n = hosts.iter().filter(|h| h.0 == *k).count();
            (n > 0).then(|| crate::tr_n_lang(ru, &kind_count_key(k), n as u64))
        })
        .collect();
    crate::tr_lang(ru, "rebar_hosts.hosts", &[("list", &parts.join(", "))])
}

/// Hover list: one row per kind and type name with its count ("Wall \u{b7} Basic 200 \u{d7}2").
fn hosts_detail(ru: bool, hosts: &[(String, i64)], names: &[String]) -> String {
    let mut rows: Vec<(String, usize)> = vec![];
    for (i, (k, _)) in hosts.iter().enumerate() {
        let kind = crate::tr_lang(ru, &format!("rebar_hosts.kind.{k}"), &[]);
        let row = match names.get(i).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            Some(ty) => crate::tr_lang(ru, "rebar_hosts.kind_type", &[("kind", &kind), ("type", ty)]),
            None => kind,
        };
        match rows.iter_mut().find(|r| r.0 == row) {
            Some(r) => r.1 += 1,
            None => rows.push((row, 1)),
        }
    }
    rows.into_iter()
        .map(|(r, n)| if n > 1 { format!("{r} \u{d7}{n}") } else { r })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reinforcement window parameters (live per session; lengths in mm).
#[derive(Clone)]
struct Form {
    hosts: Vec<(String, i64)>,
    /// Type name per host (same order), empty when unknown.
    names: Vec<String>,
    tab: Tab,
    rtype: i64,
    pos: usize,
    count: usize,
    shape: usize,
    hook_a: usize,
    hook_b: usize,
    leg: f64,
    rule: usize,
    n: usize,
    spacing: f64,
    end_zone: f64,
    end_spacing: f64,
    s_hook: usize,
    faces: usize,
    angle: f64,
    a_spacing: f64,
    minor_type: i64,
    minor_spacing: f64,
    mark: String,
    covers: Vec<(String, f64)>,
}

impl Default for Form {
    fn default() -> Form {
        Form {
            hosts: vec![],
            names: vec![],
            tab: Tab::Long,
            rtype: 0,
            pos: 0,
            count: 3,
            shape: 0,
            hook_a: 0,
            hook_b: 0,
            leg: 0.0,
            rule: 2,
            n: 10,
            spacing: 200.0,
            end_zone: 600.0,
            end_spacing: 100.0,
            s_hook: 2,
            faces: 0,
            angle: 0.0,
            a_spacing: 200.0,
            minor_type: 0,
            minor_spacing: 200.0,
            mark: String::new(),
            covers: vec![],
        }
    }
}

#[derive(Default)]
struct UiState {
    /// Last parameters: the window reopens with them.
    last: Form,
    form: Option<Form>,
    schedule: bool,
    analytical: bool,
    /// (journal head, analytics): recomputed after an edit.
    an: Option<(u64, Arc<Analytical>)>,
    /// Analytical edit of the selected element: (kind, id, edit).
    adj: Option<(String, i64, AnalyticalAdj)>,
    /// Waiting for the user to pick hosts (Reinforcement was run without a usable selection).
    pick: bool,
}

thread_local! {
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
}

/// Rebar host kind of a structural extension category code: footing or raft.
fn ext_host(code: u16) -> Option<&'static str> {
    ["footing", "raft"]
        .into_iter()
        .find(|k| crate::editor::structx::code_of(k) == Some(code))
}

/// Reinforcement hosts in the plan and 3D selection: (kind from `REBAR_HOSTS`, id).
fn hosts(app: &EditorApp) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = vec![];
    let mut add = |k: &str, id: i64| {
        if !v.iter().any(|h| h.0 == k && h.1 == id) {
            v.push((k.to_string(), id));
        }
    };
    for s in &app.ed.selection {
        match *s {
            Sel::Wall(id) => add("wall", id),
            Sel::Slab(id) => add("slab", id),
            Sel::Ext(c, id) => {
                if let Some(k) = ext_host(c) {
                    add(k, id)
                }
            }
            Sel::Body(BodyKind::Column, id) => add("column", id),
            Sel::Body(BodyKind::Beam, id) => add("beam", id),
            _ => {}
        }
    }
    for tg in &app.view.e3.sel {
        match tg.cat {
            Cat::Column => add("column", tg.id),
            Cat::Beam => add("beam", tg.id),
            Cat::Wall => add("wall", tg.id),
            Cat::Slab => add("slab", tg.id),
            Cat::Ext(c) => {
                if let Some(k) = ext_host(c) {
                    add(k, tg.id)
                }
            }
            _ => {}
        }
    }
    v
}

/// Analytical element in the selection (the first): (kind from `analytical::ADJ_KINDS`, id).
fn an_target(app: &EditorApp) -> Option<(String, i64)> {
    let brace = crate::editor::structx::code_of("brace");
    for tg in &app.view.e3.sel {
        let k = match tg.cat {
            Cat::Column => "column",
            Cat::Beam => "beam",
            Cat::Wall => "wall",
            Cat::Slab => "slab",
            Cat::Ext(c) if Some(c) == brace => "brace",
            _ => continue,
        };
        return Some((k.into(), tg.id));
    }
    app.ed.selection.iter().find_map(|s| match *s {
        Sel::Wall(id) => Some(("wall".into(), id)),
        Sel::Slab(id) => Some(("slab".into(), id)),
        Sel::Ext(c, id) if Some(c) == brace => Some(("brace".into(), id)),
        Sel::Ext(c, id) if c == crate::scene_ids::EXT_ANALYTICAL => {
            crate::rebar_scene::an_split(id).map(|(k, h)| (k.to_string(), h))
        }
        Sel::Body(BodyKind::Column, id) => Some(("column".into(), id)),
        Sel::Body(BodyKind::Beam, id) => Some(("beam".into(), id)),
        _ => None,
    })
}

/// Bar sets selected in 3D.
fn picked_sets(app: &EditorApp) -> Vec<i64> {
    app.view
        .e3
        .sel
        .iter()
        .filter(|tg| tg.cat == Cat::Ext(EXT_REBAR))
        .map(|tg| tg.id)
        .collect()
}

fn run(
    app: &mut EditorApp,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> bool {
    match app.ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => true,
        Err(e) => {
            app.ed.say(Severity::Error, format!("{}: {e}", t("rebar.failed")));
            false
        }
    }
}

/// Rebuild 3D and the plan layer after a display toggle (the model did not change).
fn refresh_views(app: &mut EditorApp) {
    app.view.e3.cache = Default::default();
    app.ed.revision += 1;
}

/// The form for `hs`, starting from the last used parameters.
fn form_for(a: &EditorApp, last: &Form, hs: Vec<(String, i64)>) -> Form {
    let types = a.ed.model.rebar_types().unwrap_or_default();
    let mut f = last.clone();
    if !types.iter().any(|ty| ty.id == f.rtype) {
        f.rtype = types
            .iter()
            .find(|ty| (ty.d - 0.012).abs() < 1e-9)
            .or(types.first())
            .map_or(0, |ty| ty.id);
    }
    if !types.iter().any(|ty| ty.id == f.minor_type) {
        f.minor_type = f.rtype;
    }
    f.tab = fit_tab(f.tab, &hs);
    f.covers = form_covers(&a.ed.model, &hs);
    f.names = hs
        .iter()
        .map(|(k, id)| match k.as_str() {
            "wall" => crate::plan_hovertip::type_name(&a.ed, Sel::Wall(*id)),
            _ => String::new(),
        })
        .collect();
    f.hosts = hs;
    f
}

/// What the pick-hosts bar asks for this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BarOut {
    Wait,
    Open,
    Cancel,
}

/// Catalog key of the pick bar's first line: nothing selected, a selection that cannot hold
/// reinforcement, or the picked hosts.
fn pick_line_key(any_selected: bool, n_hosts: usize) -> &'static str {
    match (n_hosts, any_selected) {
        (0, false) => "rebar_pick.pick.empty",
        (0, true) => "rebar_pick.pick.unsupported",
        _ => "rebar_hosts.hosts",
    }
}

fn pick_line(ru: bool, any_selected: bool, hs: &[(String, i64)]) -> String {
    match pick_line_key(any_selected, hs.len()) {
        "rebar_hosts.hosts" => hosts_summary(ru, hs),
        k => crate::tr_lang(ru, k, &[]),
    }
}

const VIEW_AREA: &str = "rebar.view_area";

/// The view area of this frame; the pick bar of the next frame keeps to its corner.
pub fn note_view_area(ctx: &egui::Context, area: egui::Rect) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(VIEW_AREA), area));
}

/// Bar over the viewport while Reinforcement waits for hosts; the user picks in the model as
/// usual. Enter (hosts picked) or the primary button opens the form, Esc or Cancel leaves.
fn pick_bar(ctx: &egui::Context, ru: bool, hs: &[(String, i64)], any_selected: bool) -> BarOut {
    use ddc_ui::theme::{layout, space, Role};
    use ddc_ui::widgets::Tone;
    let th = ddc_ui::theme::of(ctx);
    let ready = !hs.is_empty();
    let mut out = BarOut::Wait;
    if !ctx.wants_keyboard_input() {
        let none = egui::Modifiers::NONE;
        if ctx.input_mut(|i| i.consume_key(none, egui::Key::Escape)) {
            out = BarOut::Cancel;
        } else if ready && ctx.input_mut(|i| i.consume_key(none, egui::Key::Enter)) {
            out = BarOut::Open;
        }
    }
    let line = pick_line(ru, any_selected, hs);
    let keys = crate::tr_lang(ru, "rebar_pick.pick.keys", &[]);
    let open = crate::tr_lang(ru, "rebar_pick.pick.open", &[]);
    let cancel = crate::tr_lang(ru, "rebar_pick.pick.cancel", &[]);
    // bottom-right corner of the view: the bottom centre holds grid bubbles of a fitted plan
    let view = ctx
        .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(VIEW_AREA)))
        .filter(|r| r.is_positive())
        .unwrap_or_else(|| ctx.screen_rect());
    let width = layout::DIALOG_FORM
        .min(view.width() - 2.0 * space::LG)
        .max(240.0);
    let corner = egui::pos2(view.right() - space::LG, view.bottom() - 2.0 * space::XL);
    egui::Area::new(egui::Id::new("rebar.pick_bar"))
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(corner)
        .constrain(true)
        .order(ddc_ui::z::Z::Canvas.order())
        .show(ctx, |ui| {
            let tone = if ready { Tone::Accent } else { Tone::Neutral };
            crate::ui_kit::banner::banner(ui, &th, tone, |ui| {
                // the banner lays its icon and content in a row: the lines stack in a column
                // beside the icon, or they run side by side past the bar's width
                let icon = ddc_ui::theme::icon_size::SM + ui.spacing().item_spacing.x;
                ui.vertical(|ui| {
                    ui.set_width((width - 2.0 * space::MD - icon).max(0.0));
                    let s = th.sem();
                    ui.label(th.rich(Role::BodyStrong, &line).color(s.text_primary));
                    ui.label(th.rich(Role::Caption, &keys).color(s.text_secondary));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::ui_kit::primary_button(ui, open, ready).clicked() {
                            out = BarOut::Open;
                        }
                        if Button::quiet(&cancel).show(ui, &th).clicked() {
                            out = BarOut::Cancel;
                        }
                    });
                });
            });
        });
    out
}

pub fn register(app: &mut EditorApp) {
    crate::rebar_scene::register();
    app.register_command("str.rebar", |a| {
        let hs = hosts(a);
        UI.with(|s| {
            let s = &mut *s.borrow_mut();
            // no usable host: wait for the pick instead of refusing
            s.pick = hs.is_empty();
            if !hs.is_empty() {
                s.form = Some(form_for(a, &s.last, hs));
            }
        });
    });
    app.register_command("str.rebar_schedule", |_| {
        UI.with(|s| s.borrow_mut().schedule = true);
    });
    app.register_command("str.rebar_delete", |a| {
        let sets = picked_sets(a);
        if !sets.is_empty() {
            if run(a, move |m| m.delete_rebar(&sets)) {
                a.ed.say(Severity::Info, t("rebar.deleted"));
            }
            return;
        }
        let hs = hosts(a);
        if hs.is_empty() {
            a.ed.say(Severity::Refusal, t("rebar.no_host"));
            return;
        }
        let none = !hs.iter().any(|(k, h)| {
            a.ed.model
                .rebar_sets_of(k, Some(*h))
                .is_ok_and(|v| !v.is_empty())
        });
        if none {
            a.ed.say(Severity::Refusal, t("rebar.no_rebar"));
            return;
        }
        if run(a, move |m| m.delete_host_rebar(&hs).map(|_| ())) {
            a.ed.say(Severity::Info, t("rebar.deleted"));
        }
    });
    app.register_command("str.analytical", |a| {
        let on = !crate::rebar_scene::analytical_shown();
        crate::rebar_scene::set_analytical_shown(on);
        UI.with(|s| s.borrow_mut().analytical = on);
        refresh_views(a);
        a.ed.say(Severity::Info, t(if on { "rebar.an.shown" } else { "rebar.an.hidden" }));
    });
    app.register_command("str.analytical_export", export_json);
    app.register_command("str.rebar_toggle", |a| {
        crate::rebar_scene::set_rebar_shown(!crate::rebar_scene::rebar_shown());
        refresh_views(a);
    });
}

fn export_json(a: &mut EditorApp) {
    let js = match a.ed.model.analytical_json() {
        Ok(j) => j,
        Err(e) => {
            a.ed.say(Severity::Error, format!("{}: {e}", t("rebar.failed")));
            return;
        }
    };
    let Some(p) = crate::file_pick::Dlg::new()
        .add_filter("JSON", &["json"])
        .set_file_name("analytical.json")
        .save_file()
    else {
        return;
    };
    match std::fs::write(&p, js) {
        Ok(()) => a.ed.say(
            Severity::Info,
            crate::tr("rebar.saved", &[("path", &p.display().to_string())]),
        ),
        Err(e) => a
            .ed
            .say(Severity::Error, format!("{}: {e}", t("rebar.failed"))),
    }
}

fn export_csv(a: &mut EditorApp) {
    export_csv_with(
        a,
        || {
            crate::file_pick::Dlg::new()
                .add_filter("CSV", &["csv"])
                .set_file_name("rebar_schedule.csv")
                .save_file()
        },
        |p, b| ddc_io::fsutil::replace_durably_bytes(p, b),
        |a| {
            run(a, |m| m.freeze_rebar_marks().map(drop));
        },
    );
}

/// Path first, write second, freeze last: a cancelled or failed export changes nothing.
/// Pending marks in the built schedule equal the ones the freeze stores.
fn export_csv_with<E: std::fmt::Display>(
    a: &mut EditorApp,
    pick: impl FnOnce() -> Option<std::path::PathBuf>,
    write: impl FnOnce(&std::path::Path, &[u8]) -> Result<(), E>,
    freeze: impl FnOnce(&mut EditorApp),
) {
    let Some(p) = pick() else {
        return;
    };
    let rows = a.ed.model.rebar_schedule(None).unwrap_or_default();
    let s = schedule_csv(&rows, crate::ui_is_ru());
    match write(&p, s.as_bytes()) {
        Ok(()) => {
            // an issued schedule pins the marks: later edits will not shift them
            freeze(a);
            a.ed.say(
                Severity::Info,
                crate::tr("rebar.saved", &[("path", &p.display().to_string())]),
            );
        }
        Err(e) => a
            .ed
            .say(Severity::Error, format!("{}: {e}", t("rebar.failed"))),
    }
}

/// Bending schedule as a .csv file through the shared writer (separator and decimal mark of the
/// interface language, byte order mark, CRLF).
fn schedule_csv(rows: &[RebarRow], ru: bool) -> String {
    let st = ddc_geom::csv::Style::for_lang(ru);
    let head = [
        "rebar.col.mark",
        "rebar.col.shape",
        "rebar.col.d",
        "rebar.col.grade",
        "rebar.col.dims",
        "rebar.col.hooks",
        "rebar.col.count",
        "rebar.col.length",
        "rebar.col.total",
        "rebar.col.unit_mass",
        "rebar.col.weight",
    ];
    let head = head.map(|k| crate::tr_lang(ru, k, &[]));
    let body = rows.iter().map(|r| {
        [
            r.mark.clone(),
            r.shape.to_string(),
            st.num(r.d * 1000.0, 0),
            r.grade.clone(),
            r.dims.clone(),
            r.hooks.clone(),
            r.count.to_string(),
            st.num(r.length, 3),
            st.num(r.total_length, 3),
            st.num(r.unit_mass, 3),
            st.num(r.weight, 2),
        ]
    });
    ddc_geom::csv::file(std::iter::once(head).chain(body), st)
}

/// The form's set for every host; none when the tab does not fit the pick.
fn specs(f: &Form) -> Vec<RebarSetSpec> {
    if f.tab == Tab::Cover || tab_block(f.tab, &f.hosts).is_some() {
        return vec![];
    }
    let mm = |v: f64| v / 1000.0;
    let layout = match f.tab {
        Tab::Long => RebarLayout::Long {
            pos: RebarPos::ALL[f.pos.min(RebarPos::ALL.len() - 1)],
            count: f.count.max(1),
            shape: SHAPES[f.shape.min(SHAPES.len() - 1)],
            hooks: (HOOKS[f.hook_a % 4], HOOKS[f.hook_b % 4]),
            leg: mm(f.leg),
        },
        Tab::Stirrup => RebarLayout::Stirrup {
            rule: match f.rule {
                0 => RebarPath::Fixed(f.n.max(1)),
                1 => RebarPath::MaxSpacing(mm(f.spacing)),
                3 => RebarPath::Zones {
                    end: mm(f.end_zone),
                    end_s: mm(f.end_spacing),
                    mid_s: mm(f.spacing),
                },
                _ => RebarPath::Spacing(mm(f.spacing)),
            },
            hook: HOOKS[f.s_hook % 4],
        },
        Tab::Area | Tab::Cover => RebarLayout::Area {
            faces: RebarFaces::ALL[f.faces.min(2)],
            angle: f.angle.to_radians(),
            spacing: mm(f.a_spacing),
            minor_type: f.minor_type,
            minor_spacing: mm(f.minor_spacing),
        },
    };
    let mark = Some(f.mark.trim().to_string()).filter(|m| !m.is_empty());
    f.hosts
        .iter()
        .filter(|(k, _)| layout_fits(&layout, k))
        .map(|(k, h)| RebarSetSpec {
            host_kind: k.clone(),
            host: *h,
            rtype: f.rtype,
            layout: layout.clone(),
            mark: mark.clone(),
        })
        .collect()
}

/// Width of every number field in the rebar dialogs.
const NUM_W: f32 = ddc_ui::theme::control::FIELD_MIN_W * 2.0;
/// Schedule rows visible before the list scrolls.
const SCHED_ROWS: f32 = 14.0;
/// Largest analytical end offset, millimetres.
const OFFSET_MAX_MM: f64 = 10_000.0;
/// Analytical issue rows visible before the list scrolls.
const ISSUE_ROWS: f32 = 7.0;

/// What the dialog widgets need: theme, language and the project length unit.
#[derive(Clone, Copy)]
struct Kit<'a> {
    th: &'a Theme,
    ru: bool,
    lu: LenUnit,
}

impl Kit<'_> {
    fn nf(&self) -> NumberFormat {
        if self.ru {
            NumberFormat::RU
        } else {
            NumberFormat::EN
        }
    }
    fn tr(&self, key: &str) -> String {
        crate::tr_lang(self.ru, key, &[])
    }
}

/// A length kept in millimetres as the project unit shows it: (value, unit text key).
fn len_field(lu: LenUnit, mm: f64) -> (f64, &'static str) {
    (lu.to_disp(mm / 1000.0), lu.unit_key())
}

/// Inverse of `len_field`: a shown value back to millimetres.
fn len_back(lu: LenUnit, v: f64) -> f64 {
    lu.from_disp(v) * 1000.0
}

/// Whole millimetres, three decimals for metres and feet.
fn len_decimals(lu: LenUnit) -> usize {
    match lu {
        LenUnit::Mm => 0,
        _ => 3,
    }
}

/// Picker row of a bar type: (title, muted detail).
fn type_row(ty: &ddc_model::RebarType) -> (String, String) {
    (
        ty.name.clone(),
        format!("\u{d8}{:.0} \u{b7} {}", ty.d * 1000.0, ty.grade),
    )
}

fn type_items(types: &[ddc_model::RebarType]) -> Vec<PickItem> {
    types
        .iter()
        .map(|ty| {
            let (name, detail) = type_row(ty);
            PickItem {
                id: ty.id,
                name,
                detail,
            }
        })
        .collect()
}

/// Label of the primary button: names how many sets one click adds.
fn add_label(ru: bool, f: &Form) -> String {
    match f.hosts.len() {
        1 => crate::tr_lang(ru, "rebar_sets.add_one", &[]),
        n => crate::tr_lang(ru, "rebar_sets.add_n", &[("n", &n.to_string())]),
    }
}

/// Dropdown row over translated items.
fn pick_row(ui: &mut egui::Ui, k: &Kit, label: &str, salt: &str, cur: &mut usize, items: &[String]) {
    let label = t(label);
    crate::ui_kit::labeled_row_id(ui, &label, |ui, _| {
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        widgets::dropdown(ui, k.th, ("r1.pick", salt), cur, &refs, None);
    });
}

/// Bar type picker row: name with a muted diameter and grade.
fn type_pick_row(ui: &mut egui::Ui, k: &Kit, label: &str, salt: &str, cur: &mut i64, types: &[ddc_model::RebarType]) {
    let label = t(label);
    let rows = type_items(types);
    crate::ui_kit::labeled_row_id(ui, &label, |ui, _| {
        let texts = PickTexts {
            search: k.tr("rebar_sets.pick.search"),
            empty: k.tr("rebar_sets.pick.empty"),
            edit: String::new(),
            mixed: String::new(),
        };
        if let Some(Pick::Id(id)) = TypePick::new(("r1.type", salt), &rows, Some(*cur))
            .texts(texts)
            .show(ui, k.th)
        {
            *cur = id;
        }
    });
}

/// Length field in the project unit over a millimetre value.
#[allow(clippy::too_many_arguments)]
fn len_input(
    ui: &mut egui::Ui,
    k: &Kit,
    salt: impl std::hash::Hash,
    label: (egui::Id, &str),
    v: &mut f64,
    (lo, hi): (f64, f64),
    step_mm: f64,
) {
    let (mut d, unit_key) = len_field(k.lu, *v);
    let before = d;
    let unit = k.tr(unit_key);
    NumberField::new(salt, &mut d, k.nf())
        .labelled_by(label.0, label.1)
        .decimals(len_decimals(k.lu))
        .step(k.lu.to_disp(step_mm / 1000.0))
        .range(len_field(k.lu, lo).0, len_field(k.lu, hi).0)
        .unit(&unit)
        .width(NUM_W)
        .show(ui, k.th);
    if d != before {
        *v = len_back(k.lu, d).clamp(lo, hi);
    }
}

fn len_row(ui: &mut egui::Ui, k: &Kit, label: &str, v: &mut f64, lo: f64, hi: f64) {
    let text = t(label);
    crate::ui_kit::labeled_row_id(ui, &text, |ui, lid| {
        len_input(ui, k, ("r1.len", label), (lid, text.as_str()), v, (lo, hi), 5.0);
    });
}

fn count_row(ui: &mut egui::Ui, k: &Kit, label: &str, salt: &str, v: &mut usize, lo: usize, hi: usize) {
    let text = t(label);
    crate::ui_kit::labeled_row_id(ui, &text, |ui, lid| {
        let mut x = *v as f64;
        let before = x;
        NumberField::new(("r1.count", salt), &mut x, k.nf())
            .labelled_by(lid, &text)
            .decimals(0)
            .step(1.0)
            .range(lo as f64, hi as f64)
            .width(NUM_W)
            .show(ui, k.th);
        if x != before {
            *v = (x.round() as usize).clamp(lo, hi);
        }
    });
}

fn angle_row(ui: &mut egui::Ui, k: &Kit, label: &str, v: &mut f64) {
    let text = t(label);
    crate::ui_kit::labeled_row_id(ui, &text, |ui, lid| {
        let unit = k.tr("rebar_sets.unit.deg");
        NumberField::new("r1.angle", v, k.nf())
            .labelled_by(lid, &text)
            .decimals(1)
            .step(5.0)
            .range(-180.0, 180.0)
            .unit(&unit)
            .width(NUM_W)
            .show(ui, k.th);
    });
}

fn hook_names() -> Vec<String> {
    HOOKS
        .iter()
        .map(|h| match h.deg() {
            0 => t("rebar.hook.none"),
            d => format!("{d}°"),
        })
        .collect()
}

/// Reinforcement window; None means closed.
fn form_window(
    app: &mut EditorApp,
    ctx: &egui::Context,
    mut f: Form,
    last: &mut Form,
) -> Option<Form> {
    let mut open = true;
    let mut apply = false;
    let mut cover = false;
    let types = app.ed.model.rebar_types().unwrap_or_default();
    let (ru, lu) = (
        matches!(app.sh.cat.lang, ddc_ui::i18n::Lang::Ru),
        LenUnit::of(app.ed.tool.units),
    );
    f.tab = fit_tab(f.tab, &f.hosts);
    let summary = hosts_summary(ru, &f.hosts);
    let detail = hosts_detail(ru, &f.hosts, &f.names);
    let tab_why: Vec<Option<String>> = TABS
        .iter()
        .map(|(tab, _)| tab_block(*tab, &f.hosts).map(|key| crate::tr_lang(ru, key, &[])))
        .collect();
    crate::ui_kit::Dialog::new("rebar.form", t("rebar.title"), crate::ui_kit::DialogSize::Form)
        // the rebar.* combo grid and the RU hosts row do not wrap: 460 clips them
        .min_size([ddc_ui::theme::layout::DIALOG_TOOL, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            let th = ddc_ui::theme::of(ui.ctx());
            let k = Kit { th: &th, ru, lu };
            hosts_line(ui, &th, &summary, &detail);
            if mixed(&f.hosts) {
                crate::ui_kit::warn_note(ui, k.tr("rebar_hosts.mixed_note"));
            }
            let names: Vec<String> = TABS.iter().map(|(_, key)| k.tr(key)).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let why: Vec<Option<&str>> = tab_why.iter().map(|w| w.as_deref()).collect();
            let mut sel = TABS.iter().position(|(tab, _)| *tab == f.tab).unwrap_or(0);
            widgets::tabs::tab_bar_gated(ui, &th, &refs, &why, &mut sel);
            f.tab = TABS[sel].0;
            ui.separator();
            if f.tab != Tab::Cover {
                type_pick_row(ui, &k, "rebar.f.type", "main", &mut f.rtype, &types);
            }
            match f.tab {
                Tab::Long => {
                    let pos: Vec<String> = RebarPos::ALL
                        .iter()
                        .map(|p| t(&format!("rebar.pos.{}", p.name())))
                        .collect();
                    pick_row(ui, &k, "rebar.f.pos", "pos", &mut f.pos, &pos);
                    count_row(ui, &k, "rebar.f.count", "long", &mut f.count, 1, 60);
                    let sh: Vec<String> = SHAPES
                        .iter()
                        .map(|s| t(&format!("rebar.shape.{}", s.code())))
                        .collect();
                    pick_row(ui, &k, "rebar.f.shape", "shape", &mut f.shape, &sh);
                    if f.shape > 0 {
                        len_row(ui, &k, "rebar.f.leg", &mut f.leg, 0.0, 5000.0);
                    }
                    let hk = hook_names();
                    pick_row(ui, &k, "rebar.f.hook_a", "hook_a", &mut f.hook_a, &hk);
                    pick_row(ui, &k, "rebar.f.hook_b", "hook_b", &mut f.hook_b, &hk);
                }
                Tab::Stirrup => {
                    let rules: Vec<String> = ["fixed", "max", "spacing", "zones"]
                        .iter()
                        .map(|r| t(&format!("rebar.rule.{r}")))
                        .collect();
                    pick_row(ui, &k, "rebar.f.rule", "rule", &mut f.rule, &rules);
                    if f.rule == 0 {
                        count_row(ui, &k, "rebar.f.count", "stirrup", &mut f.n, 1, 500);
                    } else {
                        len_row(ui, &k, "rebar.f.spacing", &mut f.spacing, 25.0, 1000.0);
                    }
                    if f.rule == 3 {
                        len_row(ui, &k, "rebar.f.end_zone", &mut f.end_zone, 0.0, 5000.0);
                        len_row(ui, &k, "rebar.f.end_spacing", &mut f.end_spacing, 25.0, 1000.0);
                    }
                    pick_row(ui, &k, "rebar.f.hook", "s_hook", &mut f.s_hook, &hook_names());
                }
                Tab::Area => {
                    let fs: Vec<String> = RebarFaces::ALL
                        .iter()
                        .map(|x| t(&format!("rebar.faces.{}", x.name())))
                        .collect();
                    pick_row(ui, &k, "rebar.f.faces", "faces", &mut f.faces, &fs);
                    angle_row(ui, &k, "rebar.f.angle", &mut f.angle);
                    len_row(ui, &k, "rebar.f.spacing", &mut f.a_spacing, 50.0, 1000.0);
                    type_pick_row(ui, &k, "rebar.f.minor_type", "minor", &mut f.minor_type, &types);
                    len_row(ui, &k, "rebar.f.minor_spacing", &mut f.minor_spacing, 50.0, 1000.0);
                }
                Tab::Cover => {
                    for (face, v) in f.covers.iter_mut() {
                        let key = format!("rebar.face.{face}");
                        len_row(ui, &k, &key, v, 5.0, 200.0);
                    }
                }
            }
            if f.tab != Tab::Cover {
                let label = t("rebar.f.mark");
                // too long for the label column of a row: the caption stands above the field
                let lid = crate::ui_kit::elided_weak(ui, &label).id;
                TextField::new(&mut f.mark)
                    .labelled_by(lid, &label)
                    .id_salt("r1.mark")
                    .show(ui, &th);
            }
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if f.tab == Tab::Cover {
                    let why = tab_block(Tab::Cover, &f.hosts).map(|key| k.tr(key));
                    cover =
                        crate::ui_kit::primary_action(ui, k.tr("rebar.apply_cover"), why.as_deref());
                } else {
                    let why = add_block(ru, &f, &types);
                    apply = crate::ui_kit::primary_action(ui, add_label(ru, &f), why.as_deref());
                }
            });
        });
    if apply && add_sets(app, &f, &types) {
        *last = f;
        return None;
    }
    if cover && tab_block(Tab::Cover, &f.hosts).is_none() {
        let hs = f.hosts.clone();
        let cv: Vec<(String, f64)> = f
            .covers
            .iter()
            .map(|(k, v)| (k.clone(), v / 1000.0))
            .collect();
        if run(app, move |m| m.set_rebar_covers(&hs, &cv)) {
            app.ed.say(Severity::Info, t("rebar.cover_set"));
        }
    }
    *last = f.clone();
    open.then_some(f)
}

fn schedule_window(app: &mut EditorApp, ctx: &egui::Context) -> bool {
    let mut open = true;
    let mut csv_out = false;
    let mut place = false;
    let rows = app.ed.model.rebar_schedule(None).unwrap_or_default();
    let issues = app.ed.model.rebar_issues().unwrap_or_default();
    crate::ui_kit::Dialog::new("rebar.schedule", t("rebar.schedule"), crate::ui_kit::DialogSize::Wide)
        .open(&mut open)
        .show(ctx, |ui| {
            let o = schedule_ui(ui, &rows, &issues);
            csv_out = o.csv;
            place = o.place;
        });
    if csv_out {
        export_csv(app);
    }
    if place {
        PLACE.with(|p| p.set(true));
    }
    open
}

#[derive(Default)]
struct SchedOut {
    csv: bool,
    place: bool,
    /// The columns grid was drawn (never for an empty schedule).
    grid: bool,
}

thread_local! {
    /// Set by the empty schedule's action; `ui` starts the rebar command once its state is restored.
    static PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn schedule_ui(ui: &mut egui::Ui, rows: &[RebarRow], issues: &[(i64, &'static str)]) -> SchedOut {
    let mut o = SchedOut::default();
    let th = ddc_ui::theme::of(ui.ctx());
    if rows.is_empty() {
        o.place = ddc_ui::widgets::icon_empty_state(
            ui,
            "rebar",
            &t("rebar_list.rebar_empty_title"),
            &t("rebar_list.rebar_empty_hint"),
            Some(&t("rebar_list.rebar_empty_action")),
        );
    } else {
        o.grid = true;
            let list_h = th.d().row * SCHED_ROWS;
            egui::ScrollArea::both().max_height(list_h).show(ui, |ui| {
                egui::Grid::new("r1.sched")
                    .striped(true)
                    .num_columns(11)
                    .show(ui, |ui| {
                        for k in [
                            "rebar.col.mark",
                            "rebar.col.shape",
                            "rebar.col.d",
                            "rebar.col.grade",
                            "rebar.col.dims",
                            "rebar.col.hooks",
                            "rebar.col.count",
                            "rebar.col.length",
                            "rebar.col.total",
                            "rebar.col.unit_mass",
                            "rebar.col.weight",
                        ] {
                            ui.strong(t(k));
                        }
                        ui.end_row();
                        for r in rows {
                            ui.label(r.mark.as_str());
                            ui.label(r.shape);
                            ui.label(format!("{:.0}", r.d * 1000.0));
                            ui.label(r.grade.as_str());
                            ui.label(r.dims.as_str());
                            ui.label(r.hooks.as_str());
                            ui.label(r.count.to_string());
                            ui.label(format!("{:.3}", r.length));
                            ui.label(format!("{:.2}", r.total_length));
                            ui.label(format!("{:.3}", r.unit_mass));
                            ui.label(format!("{:.1}", r.weight));
                            ui.end_row();
                        }
                    });
            });
            let n: usize = rows.iter().map(|r| r.count).sum();
            let w: f64 = rows.iter().map(|r| r.weight).sum();
            ui.label(crate::tr(
                "rebar.schedule.total",
                &[("n", &n.to_string()), ("kg", &format!("{w:.1}"))],
            ));
    }
    if !issues.is_empty() {
        ui.separator();
        ui.label(t("rebar.issues"));
        for (set, code) in issues.iter().take(50) {
            ui.label(format!("#{set}: {}", t(&format!("rebar.issue.{code}"))));
        }
    }
    ui.separator();
    o.csv = Button::secondary(&t("rebar.export_csv")).show(ui, &th).clicked();
    o
}

const RELEASES: [Option<AnRelease>; 3] = [None, Some(AnRelease::Fixed), Some(AnRelease::Pinned)];

fn release_row(ui: &mut egui::Ui, k: &Kit, label: &str, salt: &str, r: &mut Option<AnRelease>) {
    let items: Vec<String> = RELEASES
        .iter()
        .map(|x| match x {
            None => t("rebar.rel.default"),
            Some(x) => t(&format!("rebar.rel.{}", x.name())),
        })
        .collect();
    let mut cur = RELEASES.iter().position(|x| x == r).unwrap_or(0);
    pick_row(ui, k, label, salt, &mut cur, &items);
    *r = RELEASES[cur];
}

fn analytical_window(app: &mut EditorApp, ctx: &egui::Context, st: &mut UiState) -> bool {
    let head = app.ed.model.change_head();
    if st.an.as_ref().is_none_or(|a| a.0 != head) {
        st.an = app.ed.model.analytical().ok().map(|a| (head, a));
    }
    let tgt = an_target(app);
    if st.adj.as_ref().map(|a| (a.0.clone(), a.1)) != tgt {
        st.adj = tgt.map(|(k, id)| {
            let cur = app
                .ed
                .model
                .analytical_adj(&k, id)
                .ok()
                .flatten()
                .unwrap_or_default();
            (k, id, cur)
        });
    }
    let a = st.an.as_ref().map(|x| x.1.clone());
    let mut open = true;
    let (mut json, mut apply, mut clear) = (false, false, false);
    let mut shown = crate::rebar_scene::analytical_shown();
    let (ru, lu) = (
        matches!(app.sh.cat.lang, ddc_ui::i18n::Lang::Ru),
        LenUnit::of(app.ed.tool.units),
    );
    crate::ui_kit::Dialog::new("rebar.analytical", t("rebar.an.title"), crate::ui_kit::DialogSize::Tool)
        .open(&mut open)
        .show(ctx, |ui| {
            let th = ddc_ui::theme::of(ui.ctx());
            let k = Kit { th: &th, ru, lu };
            widgets::checkbox(ui, &th, &mut shown, &t("rebar.an.show"));
            let Some(a) = &a else {
                ui.label(t("rebar.failed"));
                return;
            };
            ui.label(crate::tr(
                "rebar.an.counts",
                &[
                    ("nodes", &a.nodes.len().to_string()),
                    ("members", &a.members.len().to_string()),
                    ("panels", &a.panels.len().to_string()),
                    ("supports", &a.supports.len().to_string()),
                ],
            ));
            ui.separator();
            if a.issues.is_empty() {
                ui.label(t("rebar.an.ok"));
            } else {
                ui.label(crate::tr(
                    "rebar.an.issues",
                    &[("n", &a.issues.len().to_string())],
                ));
                let list_h = th.d().row * ISSUE_ROWS;
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .show(ui, |ui| {
                        for i in &a.issues {
                            let who = format!("{} {}", t(&format!("rebar.host.{}", i.kind)), i.id);
                            ui.label(format!("{who}: {}", t(&format!("rebar.an.issue.{}", i.code))));
                        }
                    });
            }
            if let Some((kind, id, adj)) = st.adj.as_mut() {
                ui.separator();
                ui.strong(format!("{} {id}", t(&format!("rebar.host.{kind}"))));
                release_row(ui, &k, "rebar.an.rel_start", "rel_a", &mut adj.release_start);
                release_row(ui, &k, "rebar.an.rel_end", "rel_b", &mut adj.release_end);
                for (lab, p) in [
                    ("rebar.an.off_start", &mut adj.start),
                    ("rebar.an.off_end", &mut adj.end),
                ] {
                    let text = t(lab);
                    crate::ui_kit::labeled_row_id(ui, &text, |ui, lid| {
                        for (axis, v) in [&mut p.0, &mut p.1, &mut p.2].into_iter().enumerate() {
                            let mut x = *v * 1000.0;
                            let before = x;
                            len_input(
                                ui,
                                &k,
                                ("r1.off", lab, axis),
                                (lid, text.as_str()),
                                &mut x,
                                (-OFFSET_MAX_MM, OFFSET_MAX_MM),
                                5.0,
                            );
                            if x != before {
                                *v = x / 1000.0;
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    apply = Button::primary(&t("rebar.an.apply")).show(ui, &th).clicked();
                    clear = Button::quiet(&t("rebar.an.clear")).show(ui, &th).clicked();
                });
            }
            ui.separator();
            json = Button::secondary(&t("rebar.an.export")).show(ui, &th).clicked();
        });
    if shown != crate::rebar_scene::analytical_shown() {
        crate::rebar_scene::set_analytical_shown(shown);
        refresh_views(app);
    }
    if apply || clear {
        if let Some((k, id, adj)) = st.adj.clone() {
            let adj = if clear { AnalyticalAdj::default() } else { adj };
            if run(app, move |m| m.set_analytical_adj(&k, &[id], adj)) && clear {
                if let Some(x) = st.adj.as_mut() {
                    x.2 = AnalyticalAdj::default();
                }
            }
        }
    }
    if json {
        export_json(app);
    }
    open
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = UI.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if st.form.is_none() && !st.schedule && !st.analytical && !st.pick {
        UI.with(|s| *s.borrow_mut() = st);
        return;
    }
    if st.pick {
        let hs = hosts(app);
        let any = !app.ed.selection.is_empty() || !app.view.e3.sel.is_empty();
        match pick_bar(ctx, crate::ui_is_ru(), &hs, any) {
            BarOut::Wait => {}
            BarOut::Cancel => st.pick = false,
            BarOut::Open => {
                st.pick = false;
                st.form = Some(form_for(app, &st.last, hs));
            }
        }
    }
    if let Some(f) = st.form.take() {
        st.form = form_window(app, ctx, f, &mut st.last);
    }
    if st.schedule {
        st.schedule = schedule_window(app, ctx);
    }
    if st.analytical {
        st.analytical = analytical_window(app, ctx, &mut st);
    }
    UI.with(|s| *s.borrow_mut() = st);
    if PLACE.with(|p| p.replace(false)) {
        app.run("str.rebar");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> EditorApp {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        EditorApp::new(ed, None, "test".into())
    }

    fn csv_run(
        pick: Option<&str>,
        write_ok: bool,
    ) -> (usize, usize) {
        let mut app = test_app();
        let (mut writes, mut freezes) = (0usize, 0usize);
        export_csv_with(
            &mut app,
            || pick.map(std::path::PathBuf::from),
            |_, _| {
                writes += 1;
                if write_ok {
                    Ok(())
                } else {
                    Err("disk full")
                }
            },
            |_| freezes += 1,
        );
        (writes, freezes)
    }

    #[test]
    fn rebar_csv_cancel_does_not_freeze_marks() {
        assert_eq!(csv_run(None, true), (0, 0));
    }

    #[test]
    fn rebar_csv_write_failure_keeps_marks_unfrozen() {
        assert_eq!(csv_run(Some("x.csv"), false), (1, 0));
    }

    #[test]
    fn rebar_csv_success_freezes_marks_once() {
        assert_eq!(csv_run(Some("x.csv"), true), (1, 1));
    }

    #[test]
    fn esc_closes_the_form_without_adding_rebar() {
        let mut app = test_app();
        let mut last = Form::default();
        let mut out = Some(Form::default());
        crate::ui_kit::run_esc_frames(|c| {
            if let Some(f) = out.take() {
                out = form_window(&mut app, c, f, &mut last);
            }
        });
        assert!(out.is_none());
        assert!(app.ed.status.is_none());
    }

    #[test]
    fn esc_closes_schedule_and_analytical_windows() {
        let mut app = test_app();
        let mut st = UiState::default();
        let (mut sched, mut an) = (true, true);
        crate::ui_kit::run_esc_frames(|c| {
            if sched {
                sched = schedule_window(&mut app, c);
            }
            if an {
                an = analytical_window(&mut app, c, &mut st);
            }
        });
        // one Esc per frame goes to one window only
        assert!(!(sched && an));
        crate::ui_kit::run_esc_frames(|c| {
            if sched {
                sched = schedule_window(&mut app, c);
            }
            if an {
                an = analytical_window(&mut app, c, &mut st);
            }
        });
        assert!(!sched && !an);
    }

    fn frame(rows: &[RebarRow]) -> SchedOut {
        let ctx = egui::Context::default();
        ctx.set_fonts(ddc_ui::theme::font_definitions());
        let mut out = SchedOut::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = schedule_ui(ui, rows, &[]);
            });
        });
        out
    }

    #[test]
    fn empty_schedule_shows_empty_state_without_grid() {
        let o = frame(&[]);
        assert!(!o.grid, "no column grid for an empty schedule");
        assert!(!o.place && !o.csv, "nothing clicked in a still frame");
    }

    #[test]
    fn schedule_with_rows_keeps_grid() {
        let row = RebarRow {
            mark: "1".into(),
            shape: "straight",
            d: 0.012,
            grade: "B500".into(),
            dims: String::new(),
            hooks: String::new(),
            count: 2,
            length: 1.0,
            total_length: 2.0,
            unit_mass: 0.9,
            weight: 1.8,
        };
        assert!(frame(&[row]).grid);
    }

    #[test]
    fn empty_state_strings_exist_in_both_languages() {
        for k in [
            "rebar_list.rebar_empty_title",
            "rebar_list.rebar_empty_hint",
            "rebar_list.rebar_empty_action",
        ] {
            for ru in [false, true] {
                assert_ne!(crate::tr_lang(ru, k, &[]), k, "{k} ru={ru}");
            }
        }
    }

    #[test]
    fn form_specs_cover_every_host() {
        let f = Form {
            hosts: vec![("beam".into(), 3), ("column".into(), 5)],
            tab: Tab::Stirrup,
            rtype: 7,
            rule: 3,
            mark: " S1 ".into(),
            ..Form::default()
        };
        let ss = specs(&f);
        assert_eq!(ss.len(), 2);
        assert_eq!(ss[1].host_kind, "column");
        assert_eq!(ss[0].mark.as_deref(), Some("S1"));
        match &ss[0].layout {
            RebarLayout::Stirrup {
                rule: RebarPath::Zones { end, end_s, mid_s },
                ..
            } => {
                assert!((end - 0.6).abs() < 1e-12 && (end_s - 0.1).abs() < 1e-12);
                assert!((mid_s - 0.2).abs() < 1e-12);
            }
            l => panic!("{l:?}"),
        }
    }

    #[test]
    fn raft_rebar_host_pick() {
        use crate::editor::structx::code_of;
        assert_eq!(ext_host(code_of("raft").unwrap()), Some("raft"));
        assert_eq!(ext_host(code_of("footing").unwrap()), Some("footing"));
        assert_eq!(ext_host(code_of("brace").unwrap()), None);
    }

    #[test]
    fn len_field_round_trips_each_unit() {
        for lu in [LenUnit::Mm, LenUnit::M, LenUnit::Ft] {
            for mm in [0.0, 5.0, 150.0, 4200.5] {
                let (v, unit) = len_field(lu, mm);
                assert!(!unit.is_empty());
                assert!((len_back(lu, v) - mm).abs() < 1e-9, "{lu:?} {mm}");
            }
        }
        assert_eq!(len_field(LenUnit::Mm, 150.0).0, 150.0);
        assert!((len_field(LenUnit::M, 1500.0).0 - 1.5).abs() < 1e-12);
        assert!((len_field(LenUnit::Ft, 304.8).0 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn type_row_splits_name_and_muted_detail() {
        let ty = ddc_model::RebarType {
            id: 4,
            name: "Main bar".into(),
            d: 0.012,
            grade: "B500".into(),
            mandrel: 0.0,
            unit_mass: 0.0,
        };
        let (title, sub) = type_row(&ty);
        assert_eq!(title, "Main bar");
        assert_eq!(sub, "\u{d8}12 \u{b7} B500");
    }

    #[test]
    fn add_label_names_the_set_count() {
        let one = Form {
            hosts: vec![("beam".into(), 1)],
            ..Form::default()
        };
        let three = Form {
            hosts: vec![("beam".into(), 1), ("beam".into(), 2), ("column".into(), 3)],
            ..Form::default()
        };
        for ru in [false, true] {
            assert_eq!(add_label(ru, &one), crate::tr_lang(ru, "rebar_sets.add_one", &[]));
            assert!(add_label(ru, &three).contains('3'), "ru={ru}");
        }
        assert_ne!(add_label(false, &three), add_label(true, &three));
    }

    #[test]
    fn every_form_tab_draws_on_the_kit() {
        let mut app = test_app();
        let mut last = Form::default();
        for tab in [Tab::Long, Tab::Stirrup, Tab::Area, Tab::Cover] {
            for rule in [0, 3] {
                let mut f = Form {
                    hosts: vec![("beam".into(), 1)],
                    tab,
                    rule,
                    shape: 1,
                    ..Form::default()
                };
                f.covers = vec![("top".into(), 25.0)];
                let ctx = ddc_ui::theme::headless_ctx();
                let mut out = Some(f);
                let _ = ctx.run(egui::RawInput::default(), |c| {
                    if let Some(f) = out.take() {
                        out = form_window(&mut app, c, f, &mut last);
                    }
                });
                assert!(out.is_some());
            }
        }
    }

    #[test]
    fn new_strings_exist_in_both_languages() {
        for k in [
            "rebar_sets.add_one",
            "rebar_sets.add_n",
            "rebar_sets.pick.search",
            "rebar_sets.pick.empty",
            "rebar_sets.unit.deg",
            "topo.unit.mm",
            "topo.unit.ft",
        ] {
            for ru in [false, true] {
                assert_ne!(crate::tr_lang(ru, k, &[]), k, "{k} ru={ru}");
            }
        }
    }

    #[test]
    fn dialogs_use_no_raw_egui_widgets() {
        let src = include_str!("rebar_ui.rs");
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for banned in [
            "egui::ComboBox",
            "egui::DragValue",
            "ui.checkbox(",
            "ui.button(",
            "text_edit_singleline",
        ] {
            assert!(!code.contains(banned), "raw widget {banned} in a draw fn");
        }
        for (i, _) in code.match_indices("max_height(") {
            let next = code[i + "max_height(".len()..].chars().next().unwrap();
            assert!(!next.is_ascii_digit(), "literal max_height at byte {i}");
        }
    }

    fn bar(mark: &str) -> RebarRow {
        RebarRow {
            mark: mark.into(),
            shape: "11",
            d: 0.012,
            grade: "A500; \"C\"".into(),
            dims: "A=1000 B=300".into(),
            hooks: "90/90".into(),
            count: 4,
            length: 12.345,
            total_length: 49.38,
            unit_mass: 0.888,
            weight: 43.85,
        }
    }

    #[test]
    fn russian_schedule_has_bom_semicolons_and_decimal_commas() {
        let s = schedule_csv(&[bar("=1+2")], true);
        assert_eq!(&s.as_bytes()[..3], &[0xEF, 0xBB, 0xBF]);
        let lines: Vec<&str> = s.trim_start_matches('\u{FEFF}').split("\r\n").collect();
        assert_eq!(lines.len(), 3, "header, one row, empty tail: {s:?}");
        assert_eq!(lines[0].matches(';').count(), 10);
        assert_eq!(
            lines[1],
            "'=1+2;11;12;\"A500; \"\"C\"\"\";A=1000 B=300;90/90;4;12,345;49,380;0,888;43,85"
        );
    }

    #[test]
    fn english_schedule_has_commas_and_decimal_points() {
        let s = schedule_csv(&[bar("K-1")], false);
        let row = s.trim_start_matches('\u{FEFF}').split("\r\n").nth(1).unwrap().to_string();
        assert_eq!(row, "K-1,11,12,\"A500; \"\"C\"\"\",A=1000 B=300,90/90,4,12.345,49.380,0.888,43.85");
        let s = schedule_csv(&[bar("-5")], false);
        assert!(s.contains("\r\n-5,11,"), "a negative number stays a number: {s:?}");
    }

    #[test]
    fn rebar_export_uses_the_shared_writer() {
        let src = include_str!("rebar_ui.rs");
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        assert!(code.contains("ddc_geom::csv::file("));
        assert!(!code.contains("fn csv("), "own quoting is back");
    }

    fn hs(kinds: &[(&str, i64)]) -> Vec<(String, i64)> {
        kinds.iter().map(|(k, id)| (k.to_string(), *id)).collect()
    }

    const ALL_TABS: [Tab; 4] = [Tab::Long, Tab::Stirrup, Tab::Area, Tab::Cover];

    fn open_tabs(h: &[(String, i64)]) -> Vec<Tab> {
        ALL_TABS.into_iter().filter(|t| tab_block(*t, h).is_none()).collect()
    }

    fn font_ctx() -> egui::Context {
        ddc_ui::theme::headless_ctx()
    }

    #[test]
    fn column_and_beam_offer_bars_stirrups_and_cover_only() {
        for k in ["column", "beam"] {
            let h = hs(&[(k, 12)]);
            assert_eq!(open_tabs(&h), vec![Tab::Long, Tab::Stirrup, Tab::Cover], "{k}");
            assert_eq!(tab_block(Tab::Area, &h), Some("rebar_hosts.why.area_linear"));
            assert_eq!(fit_tab(Tab::Area, &h), Tab::Long);
        }
        // a column and a beam share every column face
        let h = hs(&[("column", 1), ("beam", 2)]);
        assert_eq!(common_faces(&h), vec!["side", "bottom", "top"]);
        assert!(!mixed(&h));
    }

    #[test]
    fn slab_and_wall_offer_mesh_and_cover_only() {
        for k in ["slab", "wall", "footing", "raft"] {
            let h = hs(&[(k, 7)]);
            assert_eq!(open_tabs(&h), vec![Tab::Area, Tab::Cover], "{k}");
            for t in [Tab::Long, Tab::Stirrup] {
                assert_eq!(tab_block(t, &h), Some("rebar_hosts.why.bars_plate"), "{k}");
                assert_eq!(fit_tab(t, &h), Tab::Area);
            }
        }
        assert_eq!(common_faces(&hs(&[("wall", 1)])), vec!["face", "edge"]);
    }

    #[test]
    fn mixed_pick_blocks_add_and_keeps_only_shared_cover_faces() {
        let types = test_app().ed.model.rebar_types().unwrap();
        let col_slab = hs(&[("column", 1), ("slab", 2)]);
        assert!(mixed(&col_slab));
        for t in [Tab::Long, Tab::Stirrup, Tab::Area] {
            assert_eq!(tab_block(t, &col_slab), Some("rebar_hosts.why.mixed"));
        }
        assert_eq!(common_faces(&col_slab), vec!["bottom", "top"]);
        assert_eq!(fit_tab(Tab::Long, &col_slab), Tab::Cover);
        // a column and a wall share no face: every tab is closed
        let col_wall = hs(&[("column", 1), ("wall", 2)]);
        assert!(open_tabs(&col_wall).is_empty());
        assert_eq!(tab_block(Tab::Cover, &col_wall), Some("rebar_hosts.why.no_common_face"));
        for ru in [false, true] {
            let f = Form {
                hosts: col_slab.clone(),
                tab: Tab::Long,
                rtype: types[0].id,
                ..Form::default()
            };
            let why = add_block(ru, &f, &types).expect("mixed add is blocked");
            assert_eq!(why, crate::tr_lang(ru, "rebar_hosts.why.mixed", &[]));
            assert_ne!(why, "rebar_hosts.why.mixed", "ru={ru}");
        }
    }

    #[test]
    fn invalid_option_is_disabled_with_a_translated_reason() {
        let types = test_app().ed.model.rebar_types().unwrap();
        let f = Form {
            hosts: hs(&[("column", 3)]),
            tab: Tab::Area,
            rtype: types[0].id,
            minor_type: types[0].id,
            ..Form::default()
        };
        for ru in [false, true] {
            let why = add_block(ru, &f, &types).unwrap();
            assert_eq!(why, crate::tr_lang(ru, "rebar_hosts.why.area_linear", &[]));
        }
        assert_ne!(add_block(false, &f, &types), add_block(true, &f, &types));
        // the same host with an open tab and a known bar type adds freely
        let ok = Form { tab: Tab::Long, ..f.clone() };
        assert_eq!(add_block(false, &ok, &types), None);
        let no_type = Form { rtype: -1, ..ok };
        assert_eq!(
            add_block(false, &no_type, &types),
            Some(crate::tr_lang(false, "rebar_hosts.why.no_type", &[]))
        );
    }

    #[test]
    fn specs_never_pair_a_layout_with_a_host_it_cannot_hold() {
        let kinds = ["column", "beam", "wall", "slab", "footing", "raft"];
        let mut picks: Vec<Vec<(String, i64)>> = kinds.iter().map(|k| hs(&[(*k, 1)])).collect();
        for a in kinds {
            for b in kinds {
                picks.push(hs(&[(a, 1), (b, 2)]));
            }
        }
        for h in &picks {
            for tab in ALL_TABS {
                let f = Form { hosts: h.clone(), tab, ..Form::default() };
                let ss = specs(&f);
                for s in &ss {
                    assert!(layout_fits(&s.layout, &s.host_kind), "{tab:?} {}", s.host_kind);
                }
                // a tab open for the pick gives one set per host; a closed one gives none
                let want = match tab_block(tab, h) {
                    None if tab != Tab::Cover => h.len(),
                    _ => 0,
                };
                assert_eq!(ss.len(), want, "{tab:?} {h:?}");
            }
        }
    }

    #[test]
    fn add_on_a_blocked_form_creates_no_set_and_no_undo_step() {
        let mut app = test_app();
        let types = app.ed.model.rebar_types().unwrap();
        let n0 = app.ed.undo_len();
        for (hosts, tab) in [
            (hs(&[("column", 1), ("slab", 2)]), Tab::Long),
            (hs(&[("slab", 2)]), Tab::Stirrup),
            (hs(&[("column", 1)]), Tab::Area),
            (hs(&[("column", 1), ("wall", 2)]), Tab::Cover),
        ] {
            let f = Form { hosts, tab, rtype: types[0].id, ..Form::default() };
            assert!(!add_sets(&mut app, &f, &types), "{tab:?}");
            assert!(app.ed.status.is_some(), "the refusal is said");
        }
        assert_eq!(app.ed.undo_len(), n0);
        for k in ["column", "slab", "wall"] {
            assert!(app.ed.model.rebar_sets_of(k, None).unwrap().is_empty(), "{k}");
        }
    }

    #[test]
    fn hosts_are_shown_by_kind_count_and_name_without_ids() {
        let h = hs(&[("column", 345), ("column", 346), ("beam", 9012), ("wall", 7781)]);
        let names = vec![String::new(), String::new(), String::new(), "Basic 200".into()];
        for ru in [false, true] {
            let line = hosts_summary(ru, &h);
            let list = [
                crate::tr_n_lang(ru, "rebar_hosts.n.column", 2),
                crate::tr_n_lang(ru, "rebar_hosts.n.beam", 1),
                crate::tr_n_lang(ru, "rebar_hosts.n.wall", 1),
            ]
            .join(", ");
            assert_eq!(line, crate::tr_lang(ru, "rebar_hosts.hosts", &[("list", &list)]));
            let tip = hosts_detail(ru, &h, &names);
            for id in ["345", "346", "9012", "7781"] {
                assert!(!line.contains(id) && !tip.contains(id), "raw id {id} shown, ru={ru}");
            }
            assert!(tip.contains("Basic 200"), "{tip}");
            assert!(tip.contains("\u{d7}2"), "two columns fold into one row: {tip}");
            assert_eq!(tip.lines().count(), 3, "{tip}");
        }
        assert_ne!(hosts_summary(false, &h), hosts_summary(true, &h));
        assert_eq!(hosts_summary(false, &h), "Hosts: 2 columns, 1 beam, 1 wall");
        // ru plural forms differ between 2 and 5
        assert_ne!(
            crate::tr_n_lang(true, "rebar_hosts.n.column", 2),
            crate::tr_n_lang(true, "rebar_hosts.n.column", 5)
        );
    }

    #[test]
    fn form_draws_for_each_pick_in_both_languages_and_writes_nothing() {
        let picks = [
            hs(&[("column", 1)]),
            hs(&[("slab", 2)]),
            hs(&[("wall", 3)]),
            hs(&[("column", 1), ("slab", 2)]),
            hs(&[("column", 1), ("wall", 3)]),
        ];
        for ru in [false, true] {
            let mut app = test_app();
            app.sh.cat.lang = if ru { ddc_ui::i18n::Lang::Ru } else { ddc_ui::i18n::Lang::En };
            // form_window runs without an app frame, so crate::tr needs the language set here
            crate::lang_on_this_thread(ru);
            let n0 = app.ed.undo_len();
            let mut last = Form::default();
            for h in &picks {
                let ctx = font_ctx();
                let mut out = Some(Form {
                    covers: form_covers(&app.ed.model, h),
                    hosts: h.clone(),
                    tab: Tab::Long,
                    ..Form::default()
                });
                for _ in 0..3 {
                    let _ = ctx.run(Default::default(), |c| {
                        if let Some(f) = out.take() {
                            out = form_window(&mut app, c, f, &mut last);
                        }
                    });
                }
                let f = out.expect("the form stays open");
                assert_eq!(f.tab, fit_tab(Tab::Long, h), "the shown tab is open, {h:?}");
                assert!(app.ed.status.is_none(), "drawing says nothing");
            }
            assert_eq!(app.ed.undo_len(), n0);
        }
    }

    #[test]
    fn ux62f_strings_exist_in_both_languages() {
        let mut keys = vec![
            "rebar_hosts.hosts".to_string(),
            "rebar_hosts.kind_type".into(),
            "rebar_hosts.mixed_note".into(),
            "rebar_hosts.why.mixed".into(),
            "rebar_hosts.why.area_linear".into(),
            "rebar_hosts.why.bars_plate".into(),
            "rebar_hosts.why.no_common_face".into(),
            "rebar_hosts.why.no_type".into(),
        ];
        for (k, _) in ddc_model::REBAR_HOSTS {
            keys.push(format!("rebar_hosts.kind.{k}"));
        }
        for k in &keys {
            for ru in [false, true] {
                assert_ne!(&crate::tr_lang(ru, k, &[]), k, "{k} ru={ru}");
            }
            if k != "rebar_hosts.kind_type" {
                assert_ne!(crate::tr_lang(false, k, &[]), crate::tr_lang(true, k, &[]), "{k}");
            }
        }
        for (k, _) in ddc_model::REBAR_HOSTS {
            let key = kind_count_key(k);
            for ru in [false, true] {
                let s = crate::tr_n_lang(ru, &key, 3);
                assert!(s.contains('3') && s != key, "{key} ru={ru}");
            }
        }
    }

    fn pick_input(w: f32, key: Option<egui::Key>) -> egui::RawInput {
        let events = key
            .map(|key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .into_iter()
            .collect();
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 720.0))),
            events,
            ..Default::default()
        }
    }

    /// Two warm-up frames, then one frame with `key`; returns the bar's answer on that frame.
    fn bar_answer(
        ru: bool,
        w: f32,
        h: &[(String, i64)],
        any: bool,
        key: Option<egui::Key>,
    ) -> (BarOut, Option<egui::Rect>) {
        let ctx = font_ctx();
        let mut out = BarOut::Wait;
        for k in [None, None, key] {
            let _ = ctx.run(pick_input(w, k), |c| out = pick_bar(c, ru, h, any));
        }
        let rect = ctx.memory(|m| m.area_rect(egui::Id::new("rebar.pick_bar")));
        (out, rect)
    }

    fn pick_state() -> (bool, bool) {
        UI.with(|s| {
            let s = s.borrow();
            (s.pick, s.form.is_some())
        })
    }

    fn reset_ui() {
        UI.with(|s| *s.borrow_mut() = UiState::default());
    }

    #[test]
    fn pick_bar_names_the_case_in_both_languages_and_fits_narrow_windows() {
        let col = hs(&[("column", 1)]);
        let cases: [(bool, &[(String, i64)], &str); 3] = [
            (false, &[], "rebar_pick.pick.empty"),
            (true, &[], "rebar_pick.pick.unsupported"),
            (true, &col, "rebar_hosts.hosts"),
        ];
        for ru in [false, true] {
            for (any, h, key) in cases {
                let line = pick_line(ru, any, h);
                let want = if h.is_empty() {
                    crate::tr_lang(ru, key, &[])
                } else {
                    hosts_summary(ru, h)
                };
                assert_eq!(line, want, "{key} ru={ru}");
                assert!(!line.contains("ux62"), "{line}");
                for w in [1280.0_f32, 360.0] {
                    let (out, rect) = bar_answer(ru, w, h, any, None);
                    assert_eq!(out, BarOut::Wait);
                    let r = rect.expect("the bar is drawn");
                    assert!(r.min.x >= -0.5 && r.max.x <= w + 0.5, "{r:?} in {w} {key} ru={ru}");
                }
            }
        }
        assert_ne!(pick_line(false, false, &[]), pick_line(true, false, &[]));
        assert_ne!(pick_line(false, true, &[]), pick_line(true, true, &[]));
        assert_ne!(pick_line(false, true, &[]), pick_line(false, false, &[]));
    }

    /// b16 shot: the bar covered the bubble of a fitted grid at the bottom centre. It now keeps
    /// to the bottom-right corner of the view area, inside it, in en and ru.
    #[test]
    fn pick_bar_keeps_to_the_view_corner_clear_of_the_bottom_centre() {
        // the b16 shot: view area and the bottom bubble of grid A
        let view = egui::Rect::from_min_max(egui::pos2(236.0, 200.0), egui::pos2(1260.0, 700.0));
        let bubble = egui::Rect::from_center_size(egui::pos2(418.0, 625.0), egui::vec2(26.0, 26.0));
        for ru in [false, true] {
            let ctx = font_ctx();
            for _ in 0..3 {
                let _ = ctx.run(pick_input(1280.0, None), |c| {
                    note_view_area(c, view);
                    let _ = pick_bar(c, ru, &[], false);
                });
            }
            let r = ctx.memory(|m| m.area_rect(egui::Id::new("rebar.pick_bar"))).expect("bar");
            assert!(view.contains_rect(r), "{r:?} in {view:?} ru={ru}");
            assert!(r.min.x > view.center().x, "bottom centre left free: {r:?} ru={ru}");
            assert!(!r.intersects(bubble), "{r:?} covers the grid bubble ru={ru}");
        }
    }

    #[test]
    fn pick_bar_keys_enter_needs_hosts_esc_always_leaves() {
        let col = hs(&[("column", 1)]);
        for ru in [false, true] {
            let enter = Some(egui::Key::Enter);
            let esc = Some(egui::Key::Escape);
            assert_eq!(bar_answer(ru, 1280.0, &[], false, enter).0, BarOut::Wait);
            assert_eq!(bar_answer(ru, 1280.0, &[], true, enter).0, BarOut::Wait);
            assert_eq!(bar_answer(ru, 1280.0, &col, true, enter).0, BarOut::Open);
            assert_eq!(bar_answer(ru, 1280.0, &[], false, esc).0, BarOut::Cancel);
            assert_eq!(bar_answer(ru, 1280.0, &col, true, esc).0, BarOut::Cancel);
        }
    }

    #[test]
    fn pick_strings_exist_in_both_languages() {
        for k in [
            "rebar_pick.pick.empty",
            "rebar_pick.pick.unsupported",
            "rebar_pick.pick.keys",
            "rebar_pick.pick.open",
            "rebar_pick.pick.cancel",
        ] {
            for ru in [false, true] {
                assert_ne!(&crate::tr_lang(ru, k, &[]), k, "{k} ru={ru}");
            }
            assert_ne!(crate::tr_lang(false, k, &[]), crate::tr_lang(true, k, &[]), "{k}");
        }
    }

    /// Runs `ui` for a few frames with `key` on the last one.
    fn run_ui(app: &mut EditorApp, key: Option<egui::Key>) {
        let ctx = font_ctx();
        for k in [None, None, key] {
            let _ = ctx.run(pick_input(1280.0, k), |c| ui(app, c));
        }
    }

    #[test]
    fn nothing_selected_arms_the_pick_without_a_toast_then_a_column_continues() {
        reset_ui();
        let mut app = test_app();
        register(&mut app);
        let n0 = app.ed.undo_len();
        assert!(app.run("str.rebar"));
        assert!(app.ed.status.is_none(), "no refusal toast");
        assert_eq!(pick_state(), (true, false));
        run_ui(&mut app, Some(egui::Key::Enter));
        assert_eq!(pick_state(), (true, false), "Enter without a host waits");
        app.ed.selection = vec![Sel::Body(BodyKind::Column, 7)];
        run_ui(&mut app, Some(egui::Key::Enter));
        assert_eq!(pick_state(), (false, true), "the form opens");
        let hosts = UI.with(|s| s.borrow().form.as_ref().map(|f| f.hosts.clone()));
        assert_eq!(hosts, Some(hs(&[("column", 7)])));
        assert!(app.ed.status.is_none());
        assert_eq!(app.ed.undo_len(), n0, "nothing written");
        reset_ui();
    }

    #[test]
    fn unsupported_selection_arms_the_pick_and_esc_cancels() {
        reset_ui();
        let mut app = test_app();
        register(&mut app);
        app.ed.selection = vec![Sel::Opening(3)];
        assert!(hosts(&app).is_empty());
        assert!(app.run("str.rebar"));
        assert!(app.ed.status.is_none(), "no refusal toast");
        assert_eq!(pick_state(), (true, false));
        run_ui(&mut app, Some(egui::Key::Enter));
        assert_eq!(pick_state(), (true, false), "a door is not a host");
        run_ui(&mut app, Some(egui::Key::Escape));
        assert_eq!(pick_state(), (false, false));
        assert!(app.ed.status.is_none());
        reset_ui();
    }

    #[test]
    fn valid_selection_skips_the_pick_and_opens_the_form() {
        reset_ui();
        let mut app = test_app();
        register(&mut app);
        app.ed.selection = vec![Sel::Slab(5)];
        assert!(app.run("str.rebar"));
        assert_eq!(pick_state(), (false, true));
        reset_ui();
    }

    #[test]
    fn pick_bar_buttons_are_kit_widgets() {
        let src = include_str!("rebar_ui.rs");
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        let bar = &code[code.find("fn pick_bar").unwrap()..code.find("pub fn register").unwrap()];
        assert!(bar.contains("primary_button") && bar.contains("Button::quiet"));
        assert!(!bar.contains("ui.button("));
    }

    #[test]
    fn tab_arrows_skip_blocked_tabs() {
        use ddc_ui::widgets::tabs::next_open;
        let b = [None, Some("why"), None, Some("why")];
        assert_eq!(next_open(&b, 4, 0, 1), Some(2));
        assert_eq!(next_open(&b, 4, 2, 1), None);
        assert_eq!(next_open(&b, 4, 2, -1), Some(0));
        assert_eq!(next_open(&[], 3, 1, 1), Some(2));
    }
}
