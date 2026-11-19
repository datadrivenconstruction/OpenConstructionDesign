//! Phases in the UI: labels of the standard phases, the view visibility layer (hidden / halftone /
//! dashed demolished), plan overlay, the "Demolish" command and the "Phases" window.

use crate::editor::{Editor, OpFn, Sel};
use crate::ui_kit as kit;
use crate::ui_kit::Tokens;
use ddc_model::{ElemPhase, Phase, PhaseFilter, PhaseShow, ViewKey, ViewPhase, DEFAULT_PHASES};
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, Cell, Column, RowState, Table, TextField, Tone};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

/// Phase name for display: a standard phase with an unchanged name is translated.
pub fn label(p: &Phase, ru: bool) -> String {
    let std_name = p
        .key
        .as_deref()
        .and_then(|k| DEFAULT_PHASES.iter().find(|d| d.0 == k))
        .filter(|d| d.1 == p.name);
    match (std_name, ru) {
        (Some(d), false) => match d.0 {
            "existing" => "Existing".into(),
            "demolition" => "Demolition".into(),
            _ => "New construction".into(),
        },
        _ => p.name.clone(),
    }
}

pub fn filter_name(f: PhaseFilter, ru: bool) -> &'static str {
    crate::tr_str(
        ru,
        match f {
            PhaseFilter::Off => "phui.filter.off",
            PhaseFilter::Complete => "phui.filter.complete",
            PhaseFilter::Existing => "phui.filter.existing",
            PhaseFilter::Demolition => "phui.filter.demolition",
            PhaseFilter::Project => "phui.filter.project",
            PhaseFilter::NewOnly => "phui.filter.new_only",
            PhaseFilter::All => "phui.filter.all",
        },
    )
}

/// Table and id of the selected element; the curtain-wall panel uses the curtain-wall phases.
pub fn sel_ref(s: Sel) -> Option<(&'static str, i64)> {
    Some(match s {
        Sel::Wall(id) => ("wall", id),
        Sel::Opening(id) => ("opening", id),
        Sel::Curtain(id) | Sel::Panel(id, _) => ("curtain_wall", id),
        Sel::Slab(id) => ("slab", id),
        Sel::Room(id) => ("room", id),
        Sel::Mass(id) => ("mass", id),
        Sel::Mep(id) => ("mep_run", id),
        Sel::Comp(id) => ("comp", id),
        Sel::Anno(_) => return None,
        Sel::Body(k, id) => (k.table(), id),
        // structural elements: table chosen by kind (column footing is `iso_footing`)
        Sel::Ext(c, id) => match crate::editor::structx::kind_of(c)? {
            "footing" => ("iso_footing", id),
            // dividers and zones are not building elements: they have no phase
            "room_sep" | "zone" | "grid" | "ref_plane" => return None,
            k => (k, id),
        },
    })
}

pub fn kind_name(kind: &str, ru: bool) -> &str {
    const KINDS: [(&str, &str); 16] = [
        ("wall", "phui.kind.wall"),
        ("opening", "phui.kind.opening"),
        ("curtain_wall", "phui.kind.curtain_wall"),
        ("slab", "phui.kind.slab"),
        ("room", "phui.kind.room"),
        ("col", "phui.kind.col"),
        ("beam", "phui.kind.beam"),
        ("roof", "phui.kind.roof"),
        ("ceiling", "phui.kind.ceiling"),
        ("stair", "phui.kind.stair"),
        ("railing", "phui.kind.railing"),
        ("mep_run", "phui.kind.mep_run"),
        ("comp", "phui.kind.comp"),
        ("shaft", "phui.kind.shaft"),
        ("ramp", "phui.kind.ramp"),
        ("member", "phui.kind.member"),
    ];
    KINDS
        .iter()
        .find(|n| n.0 == kind)
        .map_or(kind, |n| crate::tr_str(ru, n.1))
}

/// Per-phase graphics of view elements; recomputed when the model or the view phase changes.
#[derive(Clone, Debug, Default)]
pub struct PhaseLayer {
    key: Option<(u64, ViewKey, ViewPhase)>,
    pub hide: HashSet<Sel>,
    pub half: HashSet<Sel>,
    pub dashed: HashSet<Sel>,
    /// Parts without `Sel`, hidden by project variants.
    pub parts: HashSet<crate::isolate::Part>,
}

impl PhaseLayer {
    /// Call before `Vis::stash` (needs the editor's full lists).
    pub fn sync(&mut self, ed: &Editor, v: ViewKey) {
        let Ok(vp) = ed.model.view_phase(v) else {
            *self = Self::default();
            return;
        };
        let key = (ed.revision, v, vp);
        if self.key == Some(key) {
            return;
        }
        *self = Self {
            key: Some(key),
            ..Self::default()
        };
        let Ok(ix) = ed.model.phase_index() else {
            return;
        };
        let by_option = ed.model.option_hidden(v).unwrap_or_default();
        self.parts = crate::collab::option_parts(&by_option);
        for s in crate::selection::all(ed) {
            let Some((k, id)) = sel_ref(s) else { continue };
            if by_option.get(k).is_some_and(|ids| ids.contains(&id)) {
                self.hide.insert(s);
                continue;
            }
            match ix.show(k, id, vp) {
                PhaseShow::Hidden => self.hide.insert(s),
                PhaseShow::Halftone => self.half.insert(s),
                PhaseShow::Dashed => self.dashed.insert(s),
                PhaseShow::Normal => false,
            };
        }
    }
}

thread_local! {
    static L3D: RefCell<PhaseLayer> = RefCell::new(PhaseLayer::default());
}

/// 3D view phase layer (cached between frames).
pub fn layer_3d(ed: &Editor) -> PhaseLayer {
    L3D.with(|l| {
        let mut l = l.borrow_mut();
        l.sync(ed, ViewKey::ThreeD);
        l.clone()
    })
}

/// Plan: existing elements under a translucent background fill, demolished ones get the background and a dashed outline.
pub fn paint_plan(
    painter: &egui::Painter,
    ed: &Editor,
    layer: &PhaseLayer,
    to_s: &dyn Fn(ddc_geom::V2) -> egui::Pos2,
    t: &Tokens,
) {
    if layer.half.is_empty() && layer.dashed.is_empty() {
        return;
    }
    let rings = |s: Sel| -> Vec<Vec<egui::Pos2>> {
        crate::selection::outlines(ed, s)
            .iter()
            .map(|o| o.iter().map(|&p| to_s(p)).collect())
            .collect()
    };
    let wash = t.paper.gamma_multiply(0.6);
    // selected elements get no halftone so the selection halo stays visible
    let cur = crate::plan_cull::SelIndex::new(&ed.selection);
    for &s in layer.half.iter().filter(|s| !cur.contains(s)) {
        crate::section_view::fill_even_odd(painter, &rings(s), wash);
    }
    let st = egui::Stroke::new(1.2, t.text_dim);
    for &s in &layer.dashed {
        let rs = rings(s);
        crate::section_view::fill_even_odd(painter, &rs, t.paper);
        for r in rs.iter().filter(|r| r.len() > 1) {
            let mut pts = r.clone();
            pts.push(r[0]);
            painter.extend(egui::Shape::dashed_line(&pts, st, 6.0, 4.0));
        }
    }
}

/// View presets: (label key, phase key, filter).
pub const PRESETS: [(&str, &str, PhaseFilter); 3] = [
    ("phui.preset.survey", "existing", PhaseFilter::Existing),
    ("phui.preset.demolition", "demolition", PhaseFilter::Demolition),
    ("phui.preset.project", "new", PhaseFilter::Project),
];

/// Phase and filter of a preset: phase by key, otherwise the first / second / last by order.
pub fn preset(phases: &[Phase], i: usize) -> Option<ViewPhase> {
    let (_, key, filter) = PRESETS.get(i)?;
    let by_key = phases.iter().find(|p| p.key.as_deref() == Some(key));
    let fallback = match i {
        0 => phases.first(),
        1 => phases.get(1).or(phases.first()),
        _ => phases.last(),
    };
    by_key.or(fallback).map(|p| ViewPhase {
        phase: p.id,
        filter: *filter,
    })
}

/// Model error in the UI language (texts shared with the XLSX report).
pub fn err_text(e: &ddc_model::Error, ru: bool) -> String {
    let lang = if ru {
        crate::xl_text::Lang::Ru
    } else {
        crate::xl_text::Lang::En
    };
    crate::xl_text::tr(&e.to_string(), lang)
}

fn run(
    ed: &mut Editor,
    ru: bool,
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> bool {
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => true,
        Err(e) => {
            let t = crate::tr_str(ru, "phui.title");
            ed.status = Some(format!("{t}: {}", err_text(&e, ru)));
            false
        }
    }
}

/// "Demolish": the selection (with nested openings, components, railings) is demolished in the
/// plan view's phase; the element stays in the model. One operation, Ctrl+Z.
pub fn demolish_selected(ed: &mut Editor, ru: bool) {
    let mut items: Vec<(&'static str, i64)> =
        ed.selection.iter().filter_map(|s| sel_ref(*s)).collect();
    items.sort_unstable();
    items.dedup();
    if items.is_empty() {
        ed.status = Some(
            crate::tr_str(ru, "phui.demolish_pick").into(),
        );
        return;
    }
    let Ok(vp) = ed.model.view_phase(ViewKey::Plan(ed.level)) else {
        return;
    };
    let n = items.len();
    if run(ed, ru, move |m| m.demolish(&items, vp.phase).map(|_| ())) {
        let d = ed.model.demolition(vp.phase).unwrap_or_default();
        let v = vol_text(d.volume, ru);
        let vu = crate::schedule::Kind::Vol.unit(ed.tool.units);
        let (ns, cs) = (n.to_string(), d.count.to_string());
        ed.status = Some(crate::tr_lang(
            ru,
            "phui.demolish_done",
            &[
                ("n", ns.as_str()),
                ("count", cs.as_str()),
                ("v", v.as_str()),
                ("vu", &*vu),
            ],
        ));
    }
}

/// Phase and project-parameter commands.
pub fn register(app: &mut crate::shell::EditorApp) {
    app.register_command("mgr.phases", |_| open_window());
    app.register_command("mgr.schedules", |_| crate::schedule::multi_ui::open_window());
    app.register_command("mgr.project_params", |_| crate::params_ui::open_window());
    app.register_command("mod.demolish", |app| {
        let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
        demolish_selected(&mut app.ed, ru);
    });
}

/// Name edit in a list row.
struct Rename {
    id: i64,
    buf: String,
    /// The field has already had focus: losing focus after that ends the edit.
    armed: bool,
}

/// Delete prompt: the phase and the phase that receives its elements.
struct DelAsk {
    id: i64,
    into: Option<i64>,
}

#[derive(Default)]
struct WinState {
    open: bool,
    new_name: String,
    rename: Option<Rename>,
    del: Option<DelAsk>,
    /// Date fields (start, end) per phase: text while being edited.
    dates: std::collections::HashMap<i64, (String, String)>,
}

/// What a list row or the "Add" field requested.
#[derive(Debug, Clone, PartialEq)]
enum Act {
    Rename(i64, String),
    Move(i64, usize),
    Current(i64),
    AskDelete(i64),
    Add(String),
}

thread_local! {
    static WIN: RefCell<WinState> = RefCell::new(WinState::default());
    static CALENDAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether "Show by calendar" was pressed since the last call (consumed by `reveal::controls`).
pub fn take_calendar_request() -> bool {
    CALENDAR.with(|c| c.replace(false))
}

pub fn open_window() {
    WIN.with(|w| w.borrow_mut().open = true);
}

/// Neighbouring phase: the default receiver of the deleted phase's elements.
fn neighbour(phases: &[Phase], id: i64) -> Option<i64> {
    let i = phases.iter().position(|p| p.id == id)?;
    let q = if i > 0 { phases.get(i - 1) } else { phases.get(1) };
    q.map(|p| p.id)
}

/// Phase list entry; the current phase is marked.
fn item_text(p: &Phase, current: Option<i64>, ru: bool) -> String {
    if current == Some(p.id) {
        format!("{} · {}", label(p, ru), crate::tr_str(ru, "phases.current_mark"))
    } else {
        label(p, ru)
    }
}

/// Entry index → phase; `off` is the number of entries before the phases.
fn pick_sel(idx: usize, off: usize, phases: &[Phase]) -> Option<i64> {
    idx.checked_sub(off).and_then(|i| phases.get(i)).map(|p| p.id)
}

/// Phase dropdown; `none` adds a leading "none" entry (selects `None`).
#[allow(clippy::too_many_arguments)]
fn pick(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    phases: &[Phase],
    current: Option<i64>,
    none: Option<&str>,
    sel: &mut Option<i64>,
    ru: bool,
    width: Option<f32>,
) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let mut items: Vec<String> = Vec::new();
    if let Some(n) = none {
        items.push(n.to_string());
    }
    let off = items.len();
    items.extend(phases.iter().map(|p| item_text(p, current, ru)));
    let mut idx = sel
        .and_then(|id| phases.iter().position(|p| p.id == id))
        .map_or(0, |i| i + off);
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    // the closed list shows the bare name; the «current» mark lives in the popup and tooltip
    let bare = idx.checked_sub(off).and_then(|i| phases.get(i)).map(|p| label(p, ru));
    let r = widgets::dropdown_shown(
        ui, &th, salt, &mut idx, &refs, width, None, None, bare.as_deref(),
    );
    if r.changed() {
        *sel = pick_sel(idx, off, phases);
        true
    } else {
        false
    }
}

/// The two phase rows of the Properties grid: both lists fill the shared value column.
pub(crate) fn phase_rows(
    ui: &mut egui::Ui,
    phases: &[Phase],
    current: Option<i64>,
    c: &mut i64,
    d: &mut Option<i64>,
    ru: bool,
) -> (bool, bool) {
    use crate::ui_kit::kv_widget_w;
    let none = crate::tr_str(ru, "phui.none");
    let name = |id: Option<i64>| {
        let p = id.and_then(|id| phases.iter().find(|p| p.id == id));
        p.map_or_else(|| none.to_string(), |p| label(p, ru))
    };
    let (want_c, want_d) = (closed_w(ui, &name(Some(*c))), closed_w(ui, &name(*d)));
    let mut set_c = false;
    kv_widget_w(ui, crate::tr_str(ru, "phui.created"), want_c, |ui, w| {
        let mut sel = Some(*c);
        set_c = pick(ui, "ddc.props.ph_c", phases, current, None, &mut sel, ru, Some(w));
        *c = sel.unwrap_or(*c);
    });
    let set_d = kv_widget_w(ui, crate::tr_str(ru, "phui.demolished"), want_d, |ui, w| {
        pick(ui, "ddc.props.ph_d", phases, current, Some(none), d, ru, Some(w))
    });
    (set_c, set_d)
}

/// Width a closed list needs to show `text` uncut: the text plus the kit list's frame, arrow
/// and gaps (as `dropdown_shown` counts them).
fn closed_w(ui: &egui::Ui, text: &str) -> f32 {
    let th = ddc_ui::theme::of(ui.ctx());
    let ink = th.sem().text_primary;
    let g = ui.painter().layout_no_wrap(text.to_owned(), th.font(Role::Body), ink);
    g.size().x + 2.0 * space::LG + space::SM
}

/// Width of one of two lists in a row: split evenly, accounting for the gap.
fn half_width(ui: &egui::Ui) -> f32 {
    ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(0.0)
}

fn view_captions(ui: &mut egui::Ui, th: &ddc_ui::Theme, ru: bool) {
    kit::labeled_row(ui, "", |ui| {
        let w = half_width(ui);
        for k in ["phases.col_phase", "phases.col_filter"] {
            kit::fixed_cell(ui, w, |ui| {
                ui.colored_label(th.sem().text_secondary, crate::tr_str(ru, k));
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn view_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    ed: &mut Editor,
    phases: &[Phase],
    cur: Option<i64>,
    v: ViewKey,
    title: &str,
    ru: bool,
) {
    let Ok(mut vp) = ed.model.view_phase(v) else {
        return;
    };
    let before = vp;
    let names: Vec<&str> = PhaseFilter::ALL.iter().map(|f| filter_name(*f, ru)).collect();
    let mut fi = PhaseFilter::ALL.iter().position(|f| *f == vp.filter).unwrap_or(0);
    kit::labeled_row(ui, title, |ui| {
        let w = half_width(ui);
        let mut sel = Some(vp.phase);
        if pick(ui, ("ddc.vp", format!("{v:?}")), phases, cur, None, &mut sel, ru, Some(w)) {
            if let Some(id) = sel {
                vp.phase = id;
            }
        }
        if widgets::dropdown(ui, th, ("ddc.vf", format!("{v:?}")), &mut fi, &names, Some(w)).changed() {
            vp.filter = PhaseFilter::ALL[fi];
        }
    });
    if vp != before {
        run(ed, ru, move |m| m.set_view_phase(v, vp));
    }
}

/// Plan view preset: phase and filter in one operation.
fn apply_preset(ed: &mut Editor, phases: &[Phase], i: usize, ru: bool) -> bool {
    let Some(vp) = preset(phases, i) else {
        return false;
    };
    let v = ViewKey::Plan(ed.level);
    run(ed, ru, move |m| m.set_view_phase(v, vp))
}

/// Phase order rows: number, name (double-click edits in place), "current",
/// up / down, delete (with a prompt). Returns the requested action.
fn rows(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    st: &mut WinState,
    phases: &[Phase],
    cur: Option<i64>,
    ru: bool,
) -> Option<Act> {
    let mut act = None;
    let mut begin: Option<Rename> = None;
    let ctx = ui.ctx().clone();
    for (i, p) in phases.iter().enumerate() {
        let id = p.id;
        widgets::form::inline(ui, th, |ui| {
            kit::fixed_cell(ui, th.d().control, |ui| {
                widgets::form::label(ui, th, &(i + 1).to_string(), th.d().control);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_delete = phases.len() > 1;
                let tip = crate::tr_str(ru, if can_delete { "phui.delete_tip" } else { "phui.delete_last" });
                let del = ui
                    .add_enabled_ui(can_delete, |ui| {
                        widgets::with_icons(&ctx, |icons| {
                            widgets::icon_button_danger(ui, icons, th, "delete", tip)
                        })
                    })
                    .inner;
                if del.clicked() {
                    act = Some(Act::AskDelete(id));
                }
                let down = widgets::glyph_button(
                    ui,
                    "move-down",
                    i + 1 < phases.len(),
                    crate::tr_str(ru, "rebar_list.move_down"),
                );
                if down.clicked() {
                    act = Some(Act::Move(id, i + 1));
                }
                let up = widgets::glyph_button(ui, "move-up", i > 0, crate::tr_str(ru, "rebar_list.move_up"));
                if up.clicked() {
                    act = Some(Act::Move(id, i.saturating_sub(1)));
                }
                if cur == Some(id) {
                    widgets::badge(ui, th, crate::tr_str(ru, "phui.current"), Tone::Accent);
                } else {
                    let b = widgets::Button::secondary(crate::tr_str(ru, "phases.make_current"))
                        .small()
                        .show(ui, th)
                        .on_hover_text(crate::tr_str(ru, "phui.current_tip"));
                    if b.clicked() {
                        act = Some(Act::Current(id));
                    }
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let mut end = false;
                    if let Some(r) = st.rename.as_mut().filter(|r| r.id == id) {
                        let o = TextField::new(&mut r.buf)
                            .labelled_by(
                                egui::Id::new(("ux56d.rename.lbl", id)),
                                crate::tr_str(ru, "phui.rename_hint"),
                            )
                            .id_salt(("ux56d.rename", id))
                            .show(ui, th);
                        if !r.armed {
                            o.response.request_focus();
                        }
                        if o.response.has_focus() {
                            r.armed = true;
                        }
                        if o.committed {
                            let name = r.buf.trim().to_string();
                            if !name.is_empty() && name != p.name {
                                act = Some(Act::Rename(id, name));
                            }
                        }
                        end = o.committed || o.cancelled || (r.armed && !o.response.has_focus());
                    } else {
                        let g = widgets::elided_label(
                            ui,
                            &label(p, ru),
                            th.font(Role::Body),
                            th.sem().text_primary,
                        );
                        let click = ui.interact(g.rect, ui.id().with(("ux56d.name", id)), egui::Sense::click());
                        if click.double_clicked() {
                            begin = Some(Rename { id, buf: p.name.clone(), armed: false });
                        }
                        click.on_hover_text(crate::tr_str(ru, "phui.rename_hint"));
                    }
                    if end {
                        st.rename = None;
                    }
                });
            });
        });
    }
    if begin.is_some() {
        st.rename = begin;
    }
    act
}

/// "Add" field and button (the window's single primary action).
fn add_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    st: &mut WinState,
    renaming: bool,
    ru: bool,
) -> Option<Act> {
    let mut act = None;
    let name = st.new_name.trim().to_string();
    let why = name.is_empty().then_some(crate::tr_str(ru, "phui.name_needed"));
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text = crate::tr_str(ru, "phui.add");
            // Enter that ends a rename must not also add a phase
            let add = if renaming {
                kit::primary_action_table(ui, text, why)
            } else {
                kit::primary_action(ui, text, why)
            };
            if add {
                act = Some(Act::Add(name.clone()));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                TextField::new(&mut st.new_name)
                    .labelled_by(
                        egui::Id::new("ux56d.new_phase.lbl"),
                        crate::tr_str(ru, "phui.new_hint"),
                    )
                    .id_salt("ux56d.new_phase")
                    .hint(crate::tr_str(ru, "phui.new_hint"))
                    .show(ui, th);
            });
        });
    });
    act
}

fn apply(ed: &mut Editor, st: &mut WinState, phases: &[Phase], act: Act, ru: bool) {
    match act {
        Act::Rename(id, name) => {
            run(ed, ru, move |m| m.rename_phase(id, &name));
        }
        Act::Move(id, to) => {
            run(ed, ru, move |m| m.move_phase(id, to));
        }
        Act::Current(id) => {
            if let Err(e) = ed.model.set_current_phase(id) {
                ed.status = Some(err_text(&e, ru));
            }
        }
        Act::AskDelete(id) => {
            if phases.len() > 1 {
                st.del = Some(DelAsk { id, into: neighbour(phases, id) });
            }
        }
        Act::Add(name) => {
            st.new_name.clear();
            run(ed, ru, move |m| m.add_phase(&name, None).map(|_| ()));
        }
    }
}

/// Deletion after confirmation: elements move to `into`; one operation.
fn delete_with(ed: &mut Editor, ru: bool, id: i64, into: Option<i64>) -> bool {
    run(ed, ru, move |m| m.delete_phase(id, into))
}

/// Delete confirmation: phase name, receiver choice, red button not bound to Enter.
fn delete_dialog(ctx: &egui::Context, st: &mut WinState, ed: &mut Editor, ru: bool) {
    let Some(mut ask) = st.del.take() else {
        return;
    };
    let phases = ed.model.phases().unwrap_or_default();
    let Some(target) = phases.iter().find(|p| p.id == ask.id).cloned() else {
        return;
    };
    let cur = ed.model.current_phase().ok();
    let others: Vec<Phase> = phases.iter().filter(|p| p.id != ask.id).cloned().collect();
    if others.is_empty() {
        return;
    }
    if !others.iter().any(|p| Some(p.id) == ask.into) {
        ask.into = others.first().map(|p| p.id);
    }
    let (mut open, mut go, mut cancel) = (true, false, false);
    kit::Dialog::new("phases.delete", crate::tr_str(ru, "phases.del_title"), kit::DialogSize::Confirm)
        .open(&mut open)
        .show(ctx, |ui| {
            let name = label(&target, ru);
            let text = crate::tr_lang(ru, "phases.del_text", &[("name", name.as_str())]);
            ui.add(egui::Label::new(text).wrap());
            ui.add_space(space::SM);
            kit::labeled_row(ui, crate::tr_str(ru, "phases.del_into"), |ui| {
                pick(ui, "phases.del_into", &others, cur, None, &mut ask.into, ru, None);
            });
            kit::dialog_footer(ui, |ui| {
                let ok = crate::tr_str(ru, "phases.del_ok");
                go = kit::confirm_danger_button(ui, ok, true).clicked();
                cancel = kit::cancel_button(ui, crate::tr_lang(ru, "dlg.cancel", &[])).clicked();
            });
        });
    if go {
        delete_with(ed, ru, ask.id, ask.into);
    } else if open && !cancel {
        st.del = Some(ask);
    }
}

/// Layout probe seam: the delete question about phase `ask`, moving its elements into `into`;
/// `ask` turns `None` once the question is answered or closed.
#[cfg(test)]
pub(crate) fn layout_delete(
    ctx: &egui::Context,
    ed: &mut Editor,
    ru: bool,
    ask: &mut Option<(i64, i64)>,
) {
    let mut st = WinState {
        del: ask.map(|(id, into)| DelAsk { id, into: Some(into) }),
        ..WinState::default()
    };
    delete_dialog(ctx, &mut st, ed, ru);
    *ask = st.del.map(|d| (d.id, d.into.unwrap_or_default()));
}

/// "Phases" window; called every frame.
pub fn window(ctx: &egui::Context, ed: &mut Editor, ru: bool) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !st.open {
        WIN.with(|w| *w.borrow_mut() = st);
        return;
    }
    let mut open = true;
    kit::Dialog::new("phases", crate::tr_str(ru, "phui.title"), kit::DialogSize::Tool)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| body(ui, &mut st, ed, ru));
    delete_dialog(ctx, &mut st, ed, ru);
    if !open {
        st.rename = None;
        st.del = None;
    }
    st.open = open;
    WIN.with(|w| *w.borrow_mut() = st);
}

/// Phase dates for calendar playback: ISO `YYYY-MM-DD`; written on focus loss, empty fields
/// clear the dates, unparseable text stays as typed and writes nothing.
fn dates_section(ui: &mut egui::Ui, th: &ddc_ui::Theme, st: &mut WinState, ed: &mut Editor, phases: &[Phase], ru: bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    kit::section_title(ui, tr("phui.dates"));
    for p in phases {
        let id = p.id;
        let shown = ed.model.phase_dates(id).ok().flatten();
        let buf = st.dates.entry(id).or_insert_with(|| {
            shown.map_or((String::new(), String::new()), |(a, b)| (a.to_string(), b.to_string()))
        });
        let mut done = false;
        let name = label(p, ru);
        // the kit width is the frame: the hint needs its own width plus the field padding
        let ink = egui::Color32::PLACEHOLDER;
        let hint_w = ui.painter().layout_no_wrap("YYYY-MM-DD".into(), th.font(Role::Body), ink).size().x;
        let field_w = (hint_w + 2.0 * th.d().pad_x).ceil().max(92.0);
        widgets::form::inline(ui, th, |ui| {
            widgets::form::label(ui, th, &name, th.d().control * 4.0);
            for (k, t) in [&mut buf.0, &mut buf.1].into_iter().enumerate() {
                let o = TextField::new(t)
                    .labelled_by(egui::Id::new(("phui.dates.lbl", id, k)), &name)
                    .id_salt(("phui.dates", id, k))
                    .hint("YYYY-MM-DD")
                    .width(field_w)
                    .show(ui, th);
                done |= o.response.lost_focus();
            }
        });
        if done {
            let (a, b) = (ddc_model::Date::parse(buf.0.trim()), ddc_model::Date::parse(buf.1.trim()));
            let r = match (a, b) {
                (Some(a), Some(b)) => Some(ed.model.set_phase_dates(id, Some((a, b)))),
                _ if buf.0.trim().is_empty() && buf.1.trim().is_empty() && shown.is_some() => {
                    Some(ed.model.set_phase_dates(id, None))
                }
                _ => None,
            };
            if r.is_some_and(|r| r.is_err()) {
                st.dates.remove(&id);
            }
        }
    }
    if widgets::Button::secondary(tr("phui.show_calendar")).show(ui, th).clicked() {
        CALENDAR.with(|c| c.set(true));
    }
}

/// Contents of the "Phases" window (separate from the frame: testable without the UI thread).
fn body(ui: &mut egui::Ui, st: &mut WinState, ed: &mut Editor, ru: bool) {
    let th = ddc_ui::theme::of(ui.ctx());
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let renaming = st.rename.is_some();
    let phases = ed.model.phases().unwrap_or_default();
    let cur = ed.model.current_phase().ok();
    kit::section_title(ui, tr("phui.order"));
    let mut act = rows(ui, &th, st, &phases, cur, ru);
    ui.add_space(space::XS);
    let added = add_row(ui, &th, st, renaming, ru);
    act = act.or(added);
    ui.separator();
    kit::section_title(ui, tr("phui.views"));
    view_captions(ui, &th, ru);
    let lvl = ed.level;
    view_row(ui, &th, ed, &phases, cur, ViewKey::Plan(lvl), tr("phui.level_plan"), ru);
    view_row(ui, &th, ed, &phases, cur, ViewKey::ThreeD, tr("phases.view_3d"), ru);
    if let Ok(vp) = ed.model.view_phase(ViewKey::Plan(lvl)) {
        let names: Vec<&str> = PRESETS.iter().map(|p| tr(p.0)).collect();
        let sel = (0..PRESETS.len())
            .find(|&i| preset(&phases, i) == Some(vp))
            .unwrap_or(names.len());
        let mut chosen = None;
        kit::labeled_row(ui, tr("phases.presets"), |ui| {
            chosen = widgets::segmented(ui, &th, &names, sel);
        });
        if let Some(i) = chosen {
            apply_preset(ed, &phases, i, ru);
        }
    }
    ui.separator();
    dates_section(ui, &th, st, ed, &phases, ru);
    ui.separator();
    demolition_table(ui, &th, ed, &phases, ru);
    if let Some(a) = act {
        apply(ed, st, &phases, a, ru);
    }
}

/// Demolished volume as the table and the status line show it, in the window language.
fn vol_text(v: f64, ru: bool) -> String {
    crate::units::fmt_num_lang(v, 2, ru)
}

fn demolition_table(ui: &mut egui::Ui, th: &ddc_ui::Theme, ed: &Editor, phases: &[Phase], ru: bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let Ok(vp) = ed.model.view_phase(ViewKey::Plan(ed.level)) else {
        return;
    };
    let name = phases
        .iter()
        .find(|p| p.id == vp.phase)
        .map_or(String::new(), |p| label(p, ru));
    kit::section_title(ui, format!("{} «{name}»", tr("phui.demo_in")));
    let d = ed.model.demolition(vp.phase).unwrap_or_default();
    let vu = String::from(&*crate::schedule::Kind::Vol.unit(ed.tool.units));
    if d.count == 0 {
        kit::empty_note(ui, tr("phui.none_demolished"), None);
        return;
    }
    let cols = [
        Column::text(tr("phases.col_kind")).sortable(false),
        Column::number(tr("phases.col_count"), 0.0).fill(0.5).sortable(false),
        Column::number(tr("phases.col_volume"), 0.0).fill(0.8).sortable(false),
    ];
    let total = d.by_kind.len();
    let mut sort = None;
    Table::new("ux56d.demolition", &cols, total + 1).show(
        ui,
        th,
        &mut sort,
        |_| RowState::Normal,
        |r, c| match (c, r == total) {
            (0, false) => Cell::text(kind_name(d.by_kind[r].0, ru)),
            (0, true) => Cell::text(tr("phui.total")),
            (1, false) => Cell::text(d.by_kind[r].1.to_string()),
            (1, true) => Cell::text(d.count.to_string()),
            (_, false) => Cell::num(vol_text(d.by_kind[r].2, ru), vu.clone()),
            (_, true) => Cell::num(vol_text(d.volume, ru), vu.clone()),
        },
    );
}

/// Selected element properties: construction and demolition phases (an edit is one operation).
pub fn props(ui: &mut egui::Ui, ed: &mut Editor, ru: bool) {
    let Some((kind, id)) = ed.selection.first().and_then(|s| sel_ref(*s)) else {
        return;
    };
    // read once per (revision, total_changes): an idle frame runs no query
    let rk = ed.cache_key();
    let ck = egui::Id::new(("ddc.props.phases", kind, id));
    let hit = ui
        .data(|d| d.get_temp::<((u64, u64), Option<(Vec<Phase>, ElemPhase, Option<i64>)>)>(ck))
        .filter(|c| c.0 == rk);
    let got = match hit {
        Some(c) => c.1,
        None => {
            let g = match (ed.model.phases(), ed.model.element_phase(kind, id)) {
                (Ok(p), Ok(e)) => Some((p, e, ed.model.current_phase().ok())),
                _ => None,
            };
            ui.data_mut(|d| d.insert_temp(ck, (rk, g.clone())));
            g
        }
    };
    let Some((phases, e, current)) = got else {
        return;
    };
    if !crate::ui_kit::heading(ui, crate::tr_str(ru, "phui.props")) {
        return;
    }
    let (mut c, mut d) = (e.created, e.demolished);
    let (mut set_c, mut set_d) = (false, false);
    crate::ui_kit::props(ui, "sel.phase", |ui| {
        (set_c, set_d) = phase_rows(ui, &phases, current, &mut c, &mut d, ru);
    });
    if set_c || set_d {
        let items: Vec<(&'static str, i64)> =
            ed.selection.iter().filter_map(|s| sel_ref(*s)).collect();
        let (c, d) = (set_c.then_some(c), set_d.then_some(d));
        run(ed, ru, move |m| m.set_element_phases(&items, c, d));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Editor {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        e
    }

    #[test]
    fn layer_hides_future_and_dashes_demolished() {
        let mut e = room();
        let ps = e.model.phases().unwrap();
        let w = e.walls[0].id;
        e.selection = vec![Sel::Wall(w)];
        // plan is in the "New" phase with the "All" filter: new drawn normally, demolished dashed
        let v = ViewKey::Plan(e.level);
        let vp = ViewPhase {
            phase: ps[2].id,
            filter: PhaseFilter::All,
        };
        e.run_op(OpFn(Arc::new(move |m| m.set_view_phase(v, vp))))
            .unwrap();
        demolish_selected(&mut e, true);
        assert!(
            e.status.as_deref().unwrap_or("").starts_with(&crate::app2_fx::fx("phase_demolished_1")),
            "{:?}",
            e.status
        );
        let mut l = PhaseLayer::default();
        l.sync(&e, v);
        assert!(l.dashed.contains(&Sel::Wall(w)));
        assert!(l.hide.is_empty());
        // the first phase has no new walls yet: hidden
        let first = ViewPhase {
            phase: ps[0].id,
            filter: PhaseFilter::Complete,
        };
        e.run_op(OpFn(Arc::new(move |m| m.set_view_phase(v, first))))
            .unwrap();
        l.sync(&e, v);
        assert!(e.walls.iter().all(|x| l.hide.contains(&Sel::Wall(x.id))));
        // undo: the view, then the demolition
        e.undo();
        e.undo();
        l.sync(&e, v);
        assert!(l.dashed.is_empty());
    }

    #[test]
    fn model_errors_follow_ui_language() {
        let mut e = room();
        let w = e.walls[0].id;
        let ps = e.model.phases().unwrap();
        // created in "New", demolished in "Existing": the model refuses; text is in the UI language
        let items = vec![("wall", w)];
        let first = ps[0].id;
        assert!(!run(&mut e, true, move |m| m.set_element_phases(
            &items,
            None,
            Some(Some(first))
        )));
        let st = e.status.clone().unwrap_or_default();
        assert!(st.starts_with(&crate::app2_fx::fx("phase_stages_prefix")) && st.contains(&crate::app2_fx::fx("phase_earlier")), "{st}");
        let items = vec![("wall", w)];
        assert!(!run(&mut e, false, move |m| m.set_element_phases(
            &items,
            None,
            Some(Some(first))
        )));
        assert!(e.status.as_deref().unwrap_or("").starts_with("Phases: "));
    }

    #[test]
    fn presets_and_labels() {
        let e = room();
        let ps = e.model.phases().unwrap();
        let demo = preset(&ps, 1).unwrap();
        assert_eq!(demo.phase, ps[1].id);
        assert_eq!(demo.filter, PhaseFilter::Demolition);
        assert_eq!(preset(&ps, 2).unwrap().phase, ps[2].id);
        assert_eq!(label(&ps[0], false), "Existing");
        assert_eq!(label(&ps[0], true), crate::app2_fx::fx("phase_existing"));
        // a renamed standard phase is not translated
        let mut p = ps[0].clone();
        p.name = "Survey 2025".into();
        assert_eq!(label(&p, false), "Survey 2025");
    }

    fn key_ev(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Context with the named font weights (section titles, table headers): a bare one panics.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events,
            ..Default::default()
        }
    }

    /// One frame of the window body with an explicit language; returns the shape count.
    fn frame(
        ctx: &egui::Context,
        st: &mut WinState,
        ed: &mut Editor,
        ru: bool,
        events: Vec<egui::Event>,
    ) -> usize {
        let full = ctx.run(input(events), |c| {
            kit::Dialog::new("t.ux56d", "T", kit::DialogSize::Tool)
                .show(c, |ui| body(ui, &mut *st, &mut *ed, ru));
        });
        full.shapes.len()
    }

    /// Rename field of phase `id` open and focused (the field keeps the text it started with).
    fn start_rename(ed: &mut Editor, id: i64, name: &str) -> (egui::Context, WinState) {
        let ctx = font_ctx();
        let mut st = WinState::default();
        st.rename = Some(Rename { id, buf: name.to_string(), armed: false });
        for _ in 0..3 {
            frame(&ctx, &mut st, ed, false, vec![]);
        }
        assert!(st.rename.as_ref().is_some_and(|r| r.armed), "rename field never got focus");
        (ctx, st)
    }

    #[test]
    fn rename_commits_on_enter_as_one_undo_step() {
        let mut ed = room();
        let first = ed.model.phases().unwrap()[0].clone();
        let (ctx, mut st) = start_rename(&mut ed, first.id, &first.name);
        st.rename.as_mut().unwrap().buf = "Survey 2".into();
        frame(&ctx, &mut st, &mut ed, false, vec![key_ev(egui::Key::Enter)]);
        assert_eq!(ed.model.phases().unwrap()[0].name, "Survey 2");
        assert!(st.rename.is_none());
        ed.undo();
        assert_eq!(ed.model.phases().unwrap()[0].name, first.name);
    }

    #[test]
    fn rename_reverts_on_escape_and_writes_nothing() {
        let mut ed = room();
        let first = ed.model.phases().unwrap()[0].clone();
        let (ctx, mut st) = start_rename(&mut ed, first.id, &first.name);
        let rev = ed.revision;
        st.rename.as_mut().unwrap().buf = "Survey 2".into();
        frame(&ctx, &mut st, &mut ed, false, vec![key_ev(egui::Key::Escape)]);
        assert!(st.rename.is_none());
        assert_eq!(ed.model.phases().unwrap()[0].name, first.name);
        assert_eq!(ed.revision, rev);
    }

    #[test]
    fn empty_rename_is_not_committed() {
        let mut ed = room();
        let first = ed.model.phases().unwrap()[0].clone();
        let (ctx, mut st) = start_rename(&mut ed, first.id, &first.name);
        st.rename.as_mut().unwrap().buf = "   ".into();
        frame(&ctx, &mut st, &mut ed, false, vec![key_ev(egui::Key::Enter)]);
        assert_eq!(ed.model.phases().unwrap()[0].name, first.name);
    }

    #[test]
    fn delete_asks_with_the_neighbour_then_deletes_in_one_undo_step() {
        let mut ed = room();
        let ps = ed.model.phases().unwrap();
        let w = ed.walls[0].id;
        let before = ed.model.element_phase("wall", w).unwrap();
        let mut st = WinState::default();
        apply(&mut ed, &mut st, &ps, Act::AskDelete(ps[2].id), false);
        let (id, into) = st.del.as_ref().map(|a| (a.id, a.into)).unwrap();
        assert_eq!((id, into), (ps[2].id, Some(ps[1].id)));
        // asking changes nothing in the model
        assert_eq!(ed.model.phases().unwrap().len(), ps.len());
        // the first phase hands its elements to the next one
        assert_eq!(neighbour(&ps, ps[0].id), Some(ps[1].id));
        assert!(delete_with(&mut ed, false, id, into));
        assert_eq!(ed.model.phases().unwrap().len(), ps.len() - 1);
        assert_ne!(ed.model.element_phase("wall", w).unwrap().created, ps[2].id);
        ed.undo();
        assert_eq!(ed.model.phases().unwrap().len(), ps.len());
        assert_eq!(ed.model.element_phase("wall", w).unwrap().created, before.created);
    }

    #[test]
    fn delete_dialog_renders_and_keeps_the_question_open() {
        for ru in [false, true] {
            let mut ed = room();
            let ps = ed.model.phases().unwrap();
            let mut st = WinState::default();
            st.del = Some(DelAsk { id: ps[1].id, into: None });
            let ctx = font_ctx();
            let mut shapes = 0;
            for _ in 0..3 {
                let full = ctx.run(input(vec![]), |c| delete_dialog(c, &mut st, &mut ed, ru));
                shapes = full.shapes.len();
            }
            assert!(shapes > 0);
            assert!(st.del.is_some());
            assert_eq!(ed.model.phases().unwrap().len(), ps.len());
        }
    }

    #[test]
    fn preset_sets_the_plan_view_in_one_undo_step() {
        let mut ed = room();
        let ps = ed.model.phases().unwrap();
        let v = ViewKey::Plan(ed.level);
        let before = ed.model.view_phase(v).unwrap();
        assert!(apply_preset(&mut ed, &ps, 1, false));
        let now = ed.model.view_phase(v).unwrap();
        assert_eq!((now.phase, now.filter), (ps[1].id, PhaseFilter::Demolition));
        ed.undo();
        assert_eq!(ed.model.view_phase(v).unwrap(), before);
    }

    #[test]
    fn dropdown_index_maps_to_phases_and_current_is_marked() {
        let ed = room();
        let ps = ed.model.phases().unwrap();
        assert_eq!(pick_sel(0, 1, &ps), None);
        assert_eq!(pick_sel(1, 1, &ps), Some(ps[0].id));
        assert_eq!(pick_sel(0, 0, &ps), Some(ps[0].id));
        assert_eq!(pick_sel(ps.len() + 1, 0, &ps), None);
        for ru in [false, true] {
            let mark = crate::tr_str(ru, "phases.current_mark");
            assert!(item_text(&ps[1], Some(ps[1].id), ru).ends_with(mark));
            assert_eq!(item_text(&ps[1], Some(ps[0].id), ru), label(&ps[1], ru));
        }
    }

    #[test]
    fn demolished_volume_follows_the_given_language() {
        assert_eq!(vol_text(1234.5, false), "1,234.50");
        assert_eq!(vol_text(1234.5, true), "1\u{a0}234,50");
        assert_eq!(vol_text(-0.001, false), "0.00");
        assert_eq!(vol_text(-1500.0, true), "-1\u{a0}500,00");
        for ru in [false, true] {
            let mut ed = room();
            ed.selection = vec![Sel::Wall(ed.walls[0].id)];
            let ps = ed.model.phases().unwrap();
            let (v, vp) = (ViewKey::Plan(ed.level), ViewPhase {
                phase: ps[2].id,
                filter: PhaseFilter::All,
            });
            ed.run_op(OpFn(Arc::new(move |m| m.set_view_phase(v, vp)))).unwrap();
            // the thread speaks the other language: the window language still wins
            crate::with_test_lang(!ru, || demolish_selected(&mut ed, ru));
            let vp = ed.model.view_phase(ViewKey::Plan(ed.level)).unwrap();
            let d = ed.model.demolition(vp.phase).unwrap();
            assert!(d.volume > 0.0);
            let s = ed.status.clone().unwrap_or_default();
            assert!(s.contains(&vol_text(d.volume, ru)), "{ru} {s:?}");
            assert!(!s.contains(&vol_text(d.volume, !ru)), "{ru} {s:?}");
        }
    }

    #[test]
    fn window_renders_in_both_languages_and_idle_writes_nothing() {
        for ru in [false, true] {
            let mut ed = room();
            let w = ed.walls[0].id;
            ed.selection = vec![Sel::Wall(w)];
            demolish_selected(&mut ed, ru);
            let rev = ed.revision;
            let ctx = font_ctx();
            let mut st = WinState::default();
            st.new_name = "Phase X".into();
            let mut shapes = 0;
            for _ in 0..3 {
                shapes = frame(&ctx, &mut st, &mut ed, ru, vec![]);
            }
            assert!(shapes > 0, "{ru}");
            assert_eq!(ed.revision, rev, "an idle frame wrote to the model");
            assert_eq!(st.new_name, "Phase X");
        }
    }

    #[test]
    fn window_uses_kit_widgets_only() {
        let src = include_str!("phase_ui.rs");
        // test-only helpers sit among the code: the scan ends at the test module
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        let raw = [
            "egui::ComboBox",
            "selectable_value",
            "text_edit_singleline",
            "TextEdit::",
            "egui::Grid",
            "DragValue",
            "ui.button(",
            "add_enabled(",
            "desired_width(",
            ".size([",
            "\"3D\"",
        ];
        for r in raw {
            assert!(!code.contains(r), "raw widget or literal left: {r}");
        }
        for k in [
            "widgets::dropdown(",
            "TextField::new(",
            "Table::new(",
            "widgets::segmented(",
            "kit::DialogSize::Tool",
            "kit::confirm_danger_button(",
        ] {
            assert!(code.contains(k), "kit widget missing: {k}");
        }
    }

    #[test]
    fn locale_files_are_paired_and_resolve() {
        let en: serde_json::Value =
            serde_json::from_str(include_str!("../../ui/data/i18n/en/phases.json")).unwrap();
        let ru: serde_json::Value =
            serde_json::from_str(include_str!("../../ui/data/i18n/ru/phases.json")).unwrap();
        let keys = |v: &serde_json::Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&en), keys(&ru));
        for k in keys(&en) {
            for lang in [false, true] {
                let t = crate::tr_lang(lang, &k, &[("name", "P")]);
                assert!(!t.is_empty() && t != k, "{k} unresolved");
                assert!(!t.contains('{'), "{k}: placeholder left");
            }
        }
        assert_eq!(crate::tr_lang(false, "phases.view_3d", &[]), "3D");
    }
}
