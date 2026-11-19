//! Sheets in a window: the sheet canvas (format, title block, view viewports and scaled schedules),
//! dragging with alignment guides, scale selection, the active viewport with cropping,
//! the "Sheets" section of the browser, sheet sets, printing a set to PDF (single or per sheet) and DXF.

use crate::ui_kit as kit;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ddc_io::sheets::{self as io, Composed, Ctx, LengthUnits, Mark, PlacedKind};
use ddc_model::{
    Revision, SchedulePlace, SheetDef, ViewPlacement, ViewRef, ViewportDef, DEFAULT_CUT, PAPERS,
    SCALES,
};
use ddc_ui::notify::Level;
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{menu_divider, menu_item_icon, MenuTone};
use ddc_ui::{Catalog, Lang, Theme};
use egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::editor::{Editor, OpFn};
use crate::mass_tools::Step;
use crate::schedule::{self, Cat};
use crate::shell::EditorApp;
use crate::status::Severity;
use crate::ui_kit::Tokens;
use crate::units::Units;

mod dset;
#[cfg(test)]
mod dset_tests;
mod issue;
mod print;
mod print_job;
mod print_view;
mod props;
#[cfg(test)]
mod ux56c_tests;

pub use dset::{
    build_drawing_set, mark_prefix, marks_to_assign, plan_drawing_set, DsetDialog,
};
use print::print_window;
pub use print::{print_to, Print, PrintDone, PrintRun};
use props::props_window;
pub use props::Props;

/// What is dragged from the browser onto a sheet.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    View(ViewRef, String),
    Schedule(String),
}

struct Drag {
    start: [f64; 2],
    delta: [f64; 2],
    items: Vec<PlacedKind>,
}

struct CropDrag {
    vp: i64,
    edge: usize,
    crop: [f64; 4],
}

#[derive(Default)]
pub struct Place {
    pick: usize,
    /// 0 means fit to the sheet format.
    scale: f64,
}

pub struct Sets {
    cur: Option<i64>,
    name: String,
    members: Vec<i64>,
    err: String,
    /// «Delete set» was pressed: the footer asks before the set goes.
    ask_delete: bool,
}

/// Sheet state (UI thread).
#[derive(Default)]
pub struct Sheets {
    pub active: Option<i64>,
    opened: bool,
    cam: Option<(i64, [f64; 2], f32)>,
    /// Key: (sheet, revision, units, language); labels are formatted in the UI units and language.
    cache: Option<(i64, (u64, u64), Units, bool, Rc<Composed>)>,
    /// Canvas picture built off the UI thread: (sheet, revision, units, language).
    bg: crate::bg_latest::Latest<(i64, (u64, u64), Units, bool), Composed>,
    /// View bounds without cropping: (viewport, revision); recomputing a view is expensive for a large model.
    crop_cache: Option<(i64, (u64, u64), [f64; 4])>,
    pub sel: Vec<PlacedKind>,
    drag: Option<Drag>,
    pan: bool,
    guides: Vec<(bool, f64)>,
    pub active_vp: Option<i64>,
    crop: Option<CropDrag>,
    title_buf: Option<(i64, String)>,
    open_view: Option<ViewRef>,
    pub props: Option<Props>,
    pub place: Option<Place>,
    pub sets: Option<Sets>,
    pub print: Option<Print>,
    stamp: Option<issue::StampEdit>,
    issue: Option<issue::Issue>,
    pub dset: Option<DsetDialog>,
    /// Open the project info window: the only editor of these fields.
    open_project: bool,
    split_buf: Option<(i64, ddc_model::ScheduleSplit)>,
    detail_buf: Option<(i64, String)>,
    /// View placements for plan labels: (revision, placements).
    refs: Option<((u64, u64), Vec<ViewPlacement>)>,
    /// Sheet delete waiting for the user's answer.
    del_ask: Option<DelAsk>,
}

/// Confirmation card of a sheet delete: what goes away with the sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct DelAsk {
    pub id: i64,
    pub label: String,
    pub viewports: usize,
    pub schedules: usize,
}

/// Asks before deleting: the sheet takes its views and placed schedules with it.
pub fn ask_delete(ed: &Editor, st: &mut Sheets, id: i64) {
    let Ok(d) = ed.model.sheet(id) else {
        return;
    };
    st.del_ask = Some(DelAsk {
        id,
        label: format!("{}  {}", d.number, d.name).trim().to_string(),
        viewports: ed.model.viewports(id).map(|v| v.len()).unwrap_or(0),
        schedules: ed.model.schedule_places(id).map(|v| v.len()).unwrap_or(0),
    });
}

/// The user answered "Delete": one undo step, then a status that names the undo key.
pub fn confirm_delete(ed: &mut Editor, st: &mut Sheets, ru: bool) -> bool {
    let Some(a) = st.del_ask.take() else {
        return false;
    };
    let id = a.id;
    match op_in(ed, ru, move |m| m.delete_sheet(id)) {
        Ok(()) => {
            if st.active == Some(id) {
                st.active = None;
                st.active_vp = None;
                st.sel.clear();
            }
            ed.say(
                crate::status::Severity::Info,
                msg(ru, "shv.del.done", &[("name", &a.label)]),
            );
            ed.offer_undo();
            true
        }
        Err(e) => {
            ed.say(crate::status::Severity::Error, e);
            false
        }
    }
}

/// The user answered "Cancel": nothing changed.
pub fn cancel_delete(st: &mut Sheets) {
    st.del_ask = None;
}

fn plural_msg(ru: bool, key: &str, n: usize) -> String {
    thread_local! {
        static CAT: [Catalog; 2] = [Catalog::new(Lang::En), Catalog::new(Lang::Ru)];
    }
    CAT.with(|c| c[usize::from(ru)].plural(key, n as u64))
}

/// Lines of the confirm card: one per kind of content that goes with the sheet.
fn delete_lines(ru: bool, a: &DelAsk) -> Vec<String> {
    let mut v = Vec::new();
    if a.viewports > 0 {
        v.push(plural_msg(ru, "shv.del.viewports", a.viewports));
    }
    if a.schedules > 0 {
        v.push(plural_msg(ru, "shv.del.schedules", a.schedules));
    }
    v
}

fn delete_window(ctx: &egui::Context, ed: &mut Editor, ru: bool) {
    use ddc_ui::widgets::Button;
    let Some(a) = with(|s| s.del_ask.clone()) else {
        return;
    };
    let th = ddc_ui::theme::of(ctx);
    let lines = delete_lines(ru, &a);
    let mut open = true;
    let mut answer: Option<bool> = None;
    kit::Dialog::new("sheet_delete", tr(ru, "shv.del.title"), kit::DialogSize::Confirm)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(msg(ru, "shv.del.lead", &[("name", &a.label)]))
                    .font(th.font(Role::Label))
                    .color(th.c("text.primary")),
            );
            ui.add_space(ddc_ui::sem::space::SM);
            if lines.is_empty() {
                ui.label(RichText::new(tr(ru, "shv.del.empty")).color(th.c("text.secondary")));
            }
            for l in &lines {
                ui.label(RichText::new(format!("\u{2022} {l}")).font(th.font(Role::Numeric)));
            }
            ui.add_space(ddc_ui::sem::space::SM);
            kit::dialog_footer(ui, |ui| {
                if Button::danger(tr(ru, "shv.del.confirm")).show(ui, &th).clicked() {
                    answer = Some(true);
                }
                if Button::secondary(tr(ru, "shv.del.cancel"))
                    .tooltip("", "Esc")
                    .show(ui, &th)
                    .clicked()
                {
                    answer = Some(false);
                }
            });
        });
    match answer {
        Some(true) => {
            with(|s| confirm_delete(ed, s, ru));
        }
        Some(false) => with(cancel_delete),
        None if !open => with(cancel_delete),
        None => {}
    }
}

thread_local! {
    static STATE: RefCell<Sheets> = RefCell::new(Sheets::default());
}

pub fn with<R>(f: impl FnOnce(&mut Sheets) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Open sheet (the canvas replaces the plan).
pub fn active() -> Option<i64> {
    with(|s| s.active)
}

/// Switching to plan/3D/section closes the sheet canvas.
pub fn deactivate() {
    with(|s| {
        s.active = None;
        s.active_vp = None;
        s.sel.clear();
        s.drag = None;
        s.crop = None;
    });
}

/// Shows sheet `id` in the view area (go to view); a drafting view gives way to it.
pub fn open(id: i64) {
    with(|s| open_sheet(s, id));
    crate::drafting_view::deactivate();
}

fn open_sheet(st: &mut Sheets, id: i64) {
    if st.active != Some(id) {
        st.sel.clear();
        st.active_vp = None;
    }
    st.active = Some(id);
    st.opened = true;
}

fn tr(ru: bool, key: &'static str) -> &'static str {
    crate::tr_str(ru, key)
}

/// Catalog string (`i18n/<lang>/sheets.json`) in the UI language; catalogs are parsed once per thread.
fn msg(ru: bool, key: &str, args: &[(&str, &str)]) -> String {
    thread_local! {
        static CAT: [Catalog; 2] = [Catalog::new(Lang::En), Catalog::new(Lang::Ru)];
    }
    CAT.with(|c| c[usize::from(ru)].tf(key, args))
}

/// Plural catalog string for `n` (`#` is the number) on the UI language.
fn msg_n(ru: bool, key: &str, n: u64) -> String {
    thread_local! {
        static CAT: [Catalog; 2] = [Catalog::new(Lang::En), Catalog::new(Lang::Ru)];
    }
    CAT.with(|c| c[usize::from(ru)].plural(key, n))
}

fn lu(u: Units) -> LengthUnits {
    match u {
        Units::Imperial => LengthUnits::Imperial,
        Units::Metric => LengthUnits::Metric,
    }
}

pub fn schedule_key(c: Cat) -> String {
    format!("{c:?}").to_lowercase()
}

/// Model schedule for a sheet: the same columns and format as the schedule window; rooms, doors
/// and windows of the whole building ([`schedule::build_building`]).
pub fn schedule_table(ed: &Editor, key: &str) -> Option<io::Table> {
    let c = Cat::ALL.into_iter().find(|c| schedule_key(*c) == key)?;
    let u = ed.tool.units;
    let tb = schedule::build_building(ed, c);
    Some(io::Table {
        title: c.title().to_string(),
        header: tb.cols.iter().map(|k| schedule::header(k, u)).collect(),
        rows: tb
            .rows
            .iter()
            .map(|r| {
                r.vals
                    .iter()
                    .zip(&tb.cols)
                    .map(|(v, k)| schedule::fmt(v, k.kind, u))
                    .collect()
            })
            .collect(),
        swatches: vec![],
    })
}

fn with_ctx<R>(
    ed: &Editor,
    ru: bool,
    index: Option<(usize, usize)>,
    paper: Option<(&str, bool)>,
    f: impl FnOnce(&Ctx) -> R,
) -> R {
    with_ctx_refs(ed, ru, index, paper, None, f)
}

/// Same, with view placements collected once for the whole issue.
fn with_ctx_refs<R>(
    ed: &Editor,
    ru: bool,
    index: Option<(usize, usize)>,
    paper: Option<(&str, bool)>,
    refs: Option<&io::SheetRefs>,
    f: impl FnOnce(&Ctx) -> R,
) -> R {
    let sched = |k: &str| schedule_table(ed, k);
    let text = |k: &str| msg(ru, k, &[]);
    let ctx = Ctx {
        units: lu(ed.tool.units),
        schedules: &sched,
        index,
        paper,
        text: &text,
        refs,
    };
    f(&ctx)
}

fn composed(st: &mut Sheets, ed: &Editor, ru: bool, id: i64) -> Result<Rc<Composed>, String> {
    let key = ed.cache_key();
    if let Some((s, r, u, l, c)) = &st.cache {
        if *s == id && *r == key && *u == ed.tool.units && *l == ru {
            return Ok(c.clone());
        }
    }
    let c = with_ctx(ed, ru, None, None, |ctx| {
        io::compose_sheet(&ed.model, id, ctx)
    })
    .map_err(|e| e.0)?;
    let c = Rc::new(c);
    st.cache = Some((id, key, ed.tool.units, ru, c.clone()));
    Ok(c)
}

/// The canvas picture: a new sheet is composed in place, a new revision of the shown sheet on a
/// worker over a copy of the model while the last finished picture stays on screen.
fn composed_live(
    st: &mut Sheets,
    ed: &Editor,
    ru: bool,
    id: i64,
    repaint: &egui::Context,
) -> Result<Arc<Composed>, String> {
    let key = (id, ed.cache_key(), ed.tool.units, ru);
    st.bg.poll();
    let same_sheet = st.bg.shown().is_some_and(|s| s.key.0 == id && s.key.2 == key.2 && s.key.3 == ru);
    if !same_sheet {
        let c = with_ctx(ed, ru, None, None, |ctx| io::compose_sheet(&ed.model, id, ctx))
            .map_err(|e| e.0)?;
        return Ok(st.bg.put(key, c));
    }
    if st.bg.wanted() != Some(&key) {
        let copy = ed.model.worker_copy().map_err(|e| e.to_string())?;
        // schedules need the editor: their tables are made here, the drawing on the worker
        let mut tables = std::collections::HashMap::new();
        for (_, p) in ed.model.schedule_places(id).map_err(|e| e.to_string())? {
            let k = p.schedule.split('@').next().unwrap_or_default().to_string();
            if !tables.contains_key(&k) {
                let t = schedule_table(ed, &k);
                tables.insert(k, t);
            }
        }
        let units = lu(ed.tool.units);
        let ctx = repaint.clone();
        st.bg.request(key, move |_stop| {
            let sched = |k: &str| tables.get(k).cloned().flatten();
            let text = |k: &str| msg(ru, k, &[]);
            let c = Ctx {
                units,
                schedules: &sched,
                index: None,
                paper: None,
                text: &text,
                refs: None,
            };
            let m = copy.open().map_err(|e| e.to_string())?;
            let r = io::compose_sheet(&m, id, &c).map_err(|e| e.0);
            ctx.request_repaint();
            r
        });
        // a small sheet lands within the frame; a large one shows the last picture meanwhile
        let budget = if cfg!(test) { None } else { Some(std::time::Duration::from_millis(8)) };
        st.bg.wait_for(budget);
    }
    Ok(st.bg.shown().expect("shown").value.clone())
}

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> Result<(), String> {
    op_in(ed, false, f)
}

/// Like [`op`], the refusal text in the UI language.
fn op_in(
    ed: &mut Editor,
    ru: bool,
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> Result<(), String> {
    ed.run_op(OpFn(Arc::new(f))).map_err(|e| err_text(ru, &e))
}

/// User text of a model error: sheet refusals come from `sheets.err.*`, never with row ids.
pub(crate) fn err_text(ru: bool, e: &ddc_model::Error) -> String {
    let ddc_model::Error::Sheet(s) = e else {
        return e.to_string();
    };
    let mut args: Vec<(&str, String)> = s.args();
    if let Some(k) = s.what_key() {
        args.push(("what", msg(ru, k, &[])));
    }
    let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    msg(ru, s.key(), &args)
}

/// Sheet copy as one undo step; returns the new sheet id.
fn dup_op(ed: &mut Editor, ru: bool, src: i64, with_views: bool) -> Result<i64, String> {
    let out = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let o = out.clone();
    let copy = msg(ru, "sheets.dup.copy", &[]);
    op_in(ed, ru, move |m| {
        o.store(
            m.duplicate_sheet(src, with_views, &copy)?,
            std::sync::atomic::Ordering::SeqCst,
        );
        Ok(())
    })?;
    Ok(out.load(std::sync::atomic::Ordering::SeqCst))
}

fn paper_of(ed: &Editor, id: i64) -> (f64, f64) {
    ed.model
        .sheet(id)
        .ok()
        .and_then(|s| io::paper_size(&s.paper, s.landscape).ok())
        .unwrap_or((420.0, 297.0))
}

/// Centre of the sheet field above the title block.
fn free_center(w: f64, h: f64) -> (f64, f64) {
    (w * 0.42, h * 0.58)
}

/// Largest standard scale at which the view fits the sheet field.
pub fn auto_scale(ed: &Editor, view: ViewRef, paper: (f64, f64)) -> f64 {
    let v = ViewportDef {
        sheet: 0,
        view,
        scale: 100.0,
        center: (0.0, 0.0),
        crop: None,
        title: String::new(),
    };
    let Ok(d) = io::view_drawing(&ed.model, &v) else {
        return 100.0;
    };
    let e = io::extent(&d);
    let (w, h) = ((e[2] - e[0]) * 1000.0, (e[3] - e[1]) * 1000.0);
    let (aw, ah) = (paper.0 * 0.62, paper.1 * 0.58);
    SCALES
        .into_iter()
        .filter(|s| *s >= 20.0)
        .find(|s| w / s <= aw && h / s <= ah)
        .unwrap_or(1000.0)
}

/// Place a view/schedule on a sheet at point `at` (mm); `scale` 0 means auto-fit.
pub fn place(
    ed: &mut Editor,
    sheet: i64,
    p: &Payload,
    at: (f64, f64),
    scale: f64,
) -> Result<(), String> {
    match p {
        Payload::View(view, title) => {
            let scale = if scale > 0.0 {
                scale
            } else {
                auto_scale(ed, *view, paper_of(ed, sheet))
            };
            let v = ViewportDef {
                sheet,
                view: *view,
                scale,
                center: at,
                crop: None,
                title: title.clone(),
            };
            op(ed, move |m| m.add_viewport(&v).map(|_| ()))
        }
        Payload::Schedule(key) => {
            let s = SchedulePlace {
                sheet,
                schedule: key.clone(),
                pos: at,
            };
            op(ed, move |m| m.add_schedule_place(&s).map(|_| ()))
        }
    }
}

/// Everything that can be placed: level plans, sections/elevations, schedules.
pub fn place_items(ed: &Editor, ru: bool) -> Vec<(Payload, String, String)> {
    let mut out = vec![];
    let sheets = ed.model.sheets().unwrap_or_default();
    let on = |v: ViewRef| -> String {
        let p = ed.model.placements(v).unwrap_or_default();
        p.iter()
            .filter_map(|(_, s)| {
                sheets
                    .iter()
                    .find(|x| x.0 == *s)
                    .map(|x| x.1.number.clone())
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    for (id, name, _) in ed.levels() {
        let v = ViewRef::Plan {
            level: id,
            cut: DEFAULT_CUT,
        };
        let label = format!("{} · {name}", tr(ru, "shv.plan"));
        out.push((Payload::View(v, label.clone()), label, on(v)));
    }
    for (id, spec) in &ed.views {
        let v = ViewRef::View(*id);
        let label = crate::editor::sec::display_name(spec);
        out.push((Payload::View(v, label.clone()), label, on(v)));
    }
    for c in Cat::ALL {
        let label = format!("{}: {}", tr(ru, "shv.schedule"), c.title_in(ru));
        out.push((Payload::Schedule(schedule_key(c)), label, String::new()));
    }
    let keys = std::iter::once(io::SHEET_LIST.to_string())
        .chain(
            ed.model
                .sheet_sets()
                .unwrap_or_default()
                .into_iter()
                .map(|s| format!("{}:{}", io::SHEET_LIST, s.0)),
        )
        .chain(io::LEGENDS.iter().map(|k| k.to_string()));
    for key in keys {
        let label = item_title(ed, &key, ru);
        out.push((Payload::Schedule(key), label, String::new()));
    }
    out
}

/// Name of a placeable table: schedule, sheet list or legend.
pub fn item_title(ed: &Editor, key: &str, ru: bool) -> String {
    if let Some(c) = Cat::ALL.into_iter().find(|c| schedule_key(*c) == key) {
        return c.title_in(ru);
    }
    let list = msg(ru, "sheets.item.sheet_list", &[]);
    if key == io::SHEET_LIST {
        return list;
    }
    if let Some(id) = key
        .strip_prefix("sheet_list:")
        .and_then(|x| x.parse::<i64>().ok())
    {
        let sets = ed.model.sheet_sets().unwrap_or_default();
        return match sets.iter().find(|s| s.0 == id) {
            Some(s) => format!("{list}: {}", s.1),
            None => list,
        };
    }
    match key {
        io::LEGEND_MATERIALS => msg(ru, "sheets.item.legend_materials", &[]),
        io::LEGEND_WALL_TYPES => msg(ru, "sheets.item.legend_wall_types", &[]),
        io::LEGEND_LINES => msg(ru, "sheets.item.legend_lines", &[]),
        _ => key.to_string(),
    }
}

/// "View number/sheet" reference for a plan mark; None if the view is not placed.
pub fn xref_label(ed: &Editor, view: i64) -> Option<String> {
    with(|st| {
        let key = ed.cache_key();
        if st.refs.as_ref().is_none_or(|r| r.0 != key) {
            let v = ed.model.view_placements().unwrap_or_default();
            st.refs = Some((key, v));
        }
        let (_, refs) = st.refs.as_ref()?;
        let p = io::view_reference(refs, view)?;
        Some(format!("{}/{}", p.detail, p.sheet_number))
    })
}

fn layer_color(t: &Tokens, layer: usize) -> Color32 {
    match io::LAYERS.get(layer).copied().unwrap_or("") {
        "Hatch" | "Grid" | "Levels" | "Dims" => t.text_dim,
        "Projection" => t.projected,
        "Beyond" => t.beyond,
        _ => t.cut,
    }
}

/// The active sheet canvas over the whole view area.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor, t: &Tokens, ru: bool) {
    with(|st| canvas(ui, ed, t, ru, st));
}

fn canvas(ui: &mut egui::Ui, ed: &mut Editor, t: &Tokens, ru: bool, st: &mut Sheets) {
    let Some(id) = st.active else { return };
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.viewport);
    let Ok(def) = ed.model.sheet(id) else {
        st.active = None;
        return;
    };
    let c = match composed_live(st, ed, ru, id, ui.ctx()) {
        Ok(c) => c,
        Err(e) => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                e,
                egui::FontId::proportional(ddc_ui::theme::type_size::TITLE),
                t.text_dim,
            );
            return;
        }
    };
    let d = &c.drawing;
    let (pw, ph) = (d.width, d.height);
    const BAR: f32 = 40.0;
    let fit = || {
        let k = ((rect.width() - 48.0) / pw as f32).min((rect.height() - BAR - 40.0) / ph as f32);
        let k = k.max(0.05);
        // field under the panel: the sheet centre sits slightly below the middle of the area
        let cy = ph / 2.0 + (BAR / 2.0 / k) as f64;
        (id, [pw / 2.0, cy], k)
    };
    if st.cam.is_none_or(|c| c.0 != id) {
        st.cam = Some(fit());
    }
    let (_, mut cen, mut k) = st.cam.unwrap_or_else(fit);
    // wheel zooms to the cursor, middle button pans
    if let Some(h) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let before = [
                cen[0] + ((h.x - rect.center().x) / k) as f64,
                cen[1] - ((h.y - rect.center().y) / k) as f64,
            ];
            let k2 = (k * (scroll * 0.0025).exp()).clamp(0.2, 60.0);
            let f = 1.0 - (k / k2) as f64;
            cen = [
                cen[0] + (before[0] - cen[0]) * f,
                cen[1] + (before[1] - cen[1]) * f,
            ];
            k = k2;
        }
    }
    if resp.dragged_by(egui::PointerButton::Middle)
        || (st.pan && resp.dragged_by(egui::PointerButton::Primary))
    {
        let dd = resp.drag_delta();
        cen = [cen[0] - (dd.x / k) as f64, cen[1] + (dd.y / k) as f64];
    }
    st.cam = Some((id, cen, k));
    let c0 = rect.center();
    let to_s = move |p: [f64; 2]| -> Pos2 {
        Pos2::new(
            c0.x + ((p[0] - cen[0]) as f32) * k,
            c0.y - ((p[1] - cen[1]) as f32) * k,
        )
    };
    let to_m = move |p: Pos2| -> [f64; 2] {
        [
            cen[0] + ((p.x - c0.x) / k) as f64,
            cen[1] - ((p.y - c0.y) / k) as f64,
        ]
    };
    let srect = |r: [f64; 4], off: [f64; 2]| {
        Rect::from_two_pos(
            to_s([r[0] + off[0], r[1] + r[3] + off[1]]),
            to_s([r[0] + r[2] + off[0], r[1] + off[1]]),
        )
    };
    let paper = srect([0.0, 0.0, pw, ph], [0.0, 0.0]);
    painter.rect_filled(
        paper.translate(Vec2::new(2.0, 3.0)),
        0.0,
        Color32::from_black_alpha(40),
    );
    painter.rect_filled(paper, 0.0, t.paper);

    let vps: Vec<(i64, ViewportDef)> = ed.model.viewports(id).unwrap_or_default();
    let vp_index = |vid: i64| vps.iter().position(|v| v.0 == vid);
    let moving: Vec<PlacedKind> = st
        .drag
        .as_ref()
        .map(|g| g.items.clone())
        .unwrap_or_default();
    let delta = st.drag.as_ref().map_or([0.0, 0.0], |g| g.delta);
    let moving_k: Vec<usize> = moving
        .iter()
        .filter_map(|m| match m {
            PlacedKind::Viewport(v) => vp_index(*v),
            _ => None,
        })
        .collect();
    let act_k = st.active_vp.and_then(vp_index);

    // sheet drawing
    for (vp, m) in &d.marks {
        let off = if vp.is_some_and(|k| moving_k.contains(&k)) {
            delta
        } else {
            [0.0, 0.0]
        };
        let faded = act_k.is_some() && *vp != act_k;
        let tone = |c: Color32| if faded { c.gamma_multiply(0.3) } else { c };
        let sp = |p: &[f64; 2]| to_s([p[0] + off[0], p[1] + off[1]]);
        match m {
            Mark::Line {
                pts,
                closed,
                weight,
                rgb,
                layer,
            } => {
                let w = (*weight as f32 * k).clamp(0.5, 8.0);
                let pts: Vec<Pos2> = pts.iter().map(sp).collect();
                let bb = Rect::from_points(&pts);
                if !bb.expand(w).intersects(rect) {
                    continue;
                }
                let c = rgb.map_or_else(
                    || layer_color(t, *layer),
                    |c| Color32::from_rgb(c[0], c[1], c[2]),
                );
                let s = Stroke::new(w, tone(c));
                painter.add(if *closed {
                    egui::Shape::closed_line(pts, s)
                } else {
                    egui::Shape::line(pts, s)
                });
            }
            Mark::Fill { rings, rgb, .. } => {
                let rs: Vec<Vec<Pos2>> = rings.iter().map(|r| r.iter().map(sp).collect()).collect();
                let c = rgb.map_or(t.poche, |c| Color32::from_rgb(c[0], c[1], c[2]));
                crate::section_view::fill_even_odd(&painter, &rs, tone(c));
            }
            Mark::Text {
                pos,
                h,
                rot,
                s,
                layer,
            } => {
                let px = *h as f32 * k;
                if px < 3.0 {
                    continue;
                }
                let p = sp(pos);
                if !rect.expand(px * 20.0).contains(p) {
                    continue;
                }
                let g = painter.layout_no_wrap(
                    s.clone(),
                    egui::FontId::proportional(px),
                    tone(layer_color(t, *layer)),
                );
                let a = -(*rot as f32).to_radians();
                let asc = px * 0.9;
                let tl = p + Vec2::new(asc * a.sin(), -asc * a.cos());
                painter.add(egui::epaint::TextShape::new(tl, g, t.cut).with_angle(a));
            }
        }
    }

    // interaction
    let mods = ui.input(|i| i.modifiers);
    let hover = resp.hover_pos();
    let hit = |p: Pos2| -> Option<PlacedKind> {
        c.placed
            .iter()
            .rev()
            .find(|q| srect(q.rect, [0.0, 0.0]).contains(p))
            .map(|q| q.kind)
    };
    let placed_of = |kind: PlacedKind| c.placed.iter().find(|q| q.kind == kind);
    // active viewport: crop frame and side handles
    let act = st.active_vp.and_then(|v| {
        let p = placed_of(PlacedKind::Viewport(v))?;
        let def = vps.iter().find(|x| x.0 == v)?.1.clone();
        Some((v, p.clone(), def))
    });
    let crop_key = ed.cache_key();
    let act_crop: Option<[f64; 4]> = act.as_ref().map(|(v, p, def)| match def.crop {
        Some(c) => c,
        None => match st.crop_cache {
            Some((cv, r, c)) if cv == *v && r == crop_key => c,
            _ => {
                let c = io::view_drawing(&ed.model, def)
                    .map(|d| io::extent(&d))
                    .unwrap_or([
                        p.model_c[0] - 1.0,
                        p.model_c[1] - 1.0,
                        p.model_c[0] + 1.0,
                        p.model_c[1] + 1.0,
                    ]);
                st.crop_cache = Some((*v, crop_key, c));
                c
            }
        },
    });
    let to_sheet = |p: &io::Placed, def: &ViewportDef, m: [f64; 2]| -> [f64; 2] {
        [
            def.center.0 + (m[0] - p.model_c[0]) * 1000.0 / p.scale,
            def.center.1 + (m[1] - p.model_c[1]) * 1000.0 / p.scale,
        ]
    };
    let to_model = |p: &io::Placed, def: &ViewportDef, s: [f64; 2]| -> [f64; 2] {
        [
            p.model_c[0] + (s[0] - def.center.0) * p.scale / 1000.0,
            p.model_c[1] + (s[1] - def.center.1) * p.scale / 1000.0,
        ]
    };
    let grips = |p: &io::Placed, def: &ViewportDef, cr: [f64; 4]| -> [Pos2; 4] {
        let (a, b) = (
            to_sheet(p, def, [cr[0], cr[1]]),
            to_sheet(p, def, [cr[2], cr[3]]),
        );
        let (mx, my) = ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
        [
            to_s([a[0], my]),
            to_s([mx, a[1]]),
            to_s([b[0], my]),
            to_s([mx, b[1]]),
        ]
    };

    let press = ui.input(|i| i.pointer.press_origin());
    if resp.drag_started_by(egui::PointerButton::Primary) {
        st.pan = false;
        let p = press
            .or(resp.interact_pointer_pos())
            .unwrap_or(rect.center());
        if let Some((v, pl, def)) = &act {
            let cr = st
                .crop
                .as_ref()
                .filter(|c| c.vp == *v)
                .map(|c| c.crop)
                .or(act_crop)
                .unwrap_or_default();
            let g = grips(pl, def, cr);
            match g.iter().position(|q| q.distance(p) < 10.0) {
                Some(edge) => {
                    st.crop = Some(CropDrag {
                        vp: *v,
                        edge,
                        crop: cr,
                    })
                }
                None => st.pan = true,
            }
        } else if let Some(kind) = hit(p) {
            if !st.sel.contains(&kind) {
                st.sel = vec![kind];
            }
            st.drag = Some(Drag {
                start: to_m(p),
                delta: [0.0, 0.0],
                items: st.sel.clone(),
            });
        } else {
            st.pan = true;
        }
    }
    st.guides.clear();
    if resp.dragged_by(egui::PointerButton::Primary) {
        if let Some(p) = resp.interact_pointer_pos() {
            if let (Some(cd), Some((_, pl, def))) = (st.crop.as_mut(), &act) {
                let m = to_model(pl, def, to_m(p));
                let mut c = cd.crop;
                match cd.edge {
                    0 => c[0] = m[0].min(c[2] - 0.2),
                    1 => c[1] = m[1].min(c[3] - 0.2),
                    2 => c[2] = m[0].max(c[0] + 0.2),
                    _ => c[3] = m[1].max(c[1] + 0.2),
                }
                cd.crop = c;
            } else if let Some(g) = st.drag.as_mut() {
                let pm = to_m(p);
                let raw = [pm[0] - g.start[0], pm[1] - g.start[1]];
                let (dl, guides) = if mods.alt {
                    (raw, vec![])
                } else {
                    snap(&c.placed, &g.items, raw, [pw, ph], 8.0 / k as f64)
                };
                g.delta = dl;
                st.guides = guides;
            }
        }
    }
    if resp.drag_stopped() {
        st.pan = false;
        if let Some(cd) = st.crop.take() {
            if let Some((v, pl, def)) = &act {
                let cc = [
                    (cd.crop[0] + cd.crop[2]) / 2.0,
                    (cd.crop[1] + cd.crop[3]) / 2.0,
                ];
                let s = to_sheet(pl, def, cc);
                let mut nd = def.clone();
                nd.crop = Some(cd.crop);
                nd.center = (s[0], s[1]);
                let v = *v;
                if let Err(e) = op_in(ed, ru, move |m| m.update_viewport(v, &nd)) {
                    ed.say(Severity::Error, e);
                }
            }
        }
        if let Some(g) = st.drag.take() {
            if g.delta[0].abs() + g.delta[1].abs() > 1e-3 {
                commit_move(ed, id, &g.items, g.delta);
            }
        }
    }
    if resp.clicked() && act.is_none() {
        if let Some(p) = resp.interact_pointer_pos() {
            match hit(p) {
                Some(kind) if mods.shift || mods.command => {
                    if let Some(i) = st.sel.iter().position(|x| *x == kind) {
                        st.sel.remove(i);
                    } else {
                        st.sel.push(kind);
                    }
                }
                Some(kind) => st.sel = vec![kind],
                None => st.sel.clear(),
            }
        }
    }
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            match (&act, hit(p)) {
                (Some((_, pl, _)), _) if srect(pl.rect, [0.0, 0.0]).contains(p) => {}
                (Some(_), _) => st.active_vp = None,
                (None, k) => {
                    if let Some(e) = issue::stamp_edit_at(ed, id, &def, to_m(p), ru) {
                        st.stamp = Some(e);
                    } else if let Some(PlacedKind::Viewport(v)) = k {
                        st.active_vp = Some(v);
                        st.sel = vec![PlacedKind::Viewport(v)];
                    }
                }
            }
        }
    }
    let keys_free = crate::ui_kit::canvas_owns_keys(ui.ctx()) && !ui.ctx().wants_keyboard_input();
    if keys_free && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if st.active_vp.take().is_none() {
            st.sel.clear();
        }
        st.crop = None;
    }
    if keys_free && act.is_none() && !st.sel.is_empty() && (hover.is_some() || resp.has_focus()) {
        if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            let (mut vs, mut ss, mut legs) = (vec![], vec![], vec![]);
            for kind in std::mem::take(&mut st.sel) {
                match kind {
                    PlacedKind::Viewport(v) => vs.push(v),
                    PlacedKind::Schedule(s) => ss.push(s),
                    PlacedKind::Legend(l) => legs.push(l),
                }
            }
            let mut steps: Vec<Step> = vec![];
            if !(vs.is_empty() && ss.is_empty()) {
                steps.push(Box::new(move |m: &mut ddc_model::Model| {
                    m.delete_sheet_items(&vs, &ss)
                }));
            }
            for l in legs {
                steps.push(Box::new(move |m: &mut ddc_model::Model| {
                    m.remove_legend_place(l)
                }));
            }
            run_steps(ed, steps);
        }
        let step = if mods.shift { 10.0 } else { 1.0 };
        let mut nd = [0.0, 0.0];
        ui.input(|i| {
            for (key, v) in [
                (egui::Key::ArrowLeft, [-step, 0.0]),
                (egui::Key::ArrowRight, [step, 0.0]),
                (egui::Key::ArrowUp, [0.0, step]),
                (egui::Key::ArrowDown, [0.0, -step]),
            ] {
                if i.key_pressed(key) {
                    nd = [nd[0] + v[0], nd[1] + v[1]];
                }
            }
        });
        if nd != [0.0, 0.0] {
            commit_move(ed, id, &st.sel.clone(), nd);
        }
    }

    // selection, hover, active viewport
    for q in &c.placed {
        let off = if moving.contains(&q.kind) {
            delta
        } else {
            [0.0, 0.0]
        };
        let r = srect(q.rect, off);
        let selected = st.sel.contains(&q.kind);
        if act.is_some() {
            continue;
        }
        if selected {
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.5, t.accent),
                egui::StrokeKind::Outside,
            );
            for p in [
                r.left_top(),
                r.right_top(),
                r.left_bottom(),
                r.right_bottom(),
            ] {
                painter.rect_filled(Rect::from_center_size(p, Vec2::splat(6.0)), 1.0, t.accent);
            }
        } else if st.drag.is_none() && hover.is_some_and(|h| r.contains(h)) {
            let s = Stroke::new(1.0, t.accent.gamma_multiply(0.7));
            let pts = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            painter.extend(egui::Shape::dashed_line(&pts, s, 5.0, 3.0));
        }
        if moving.contains(&q.kind)
            && matches!(q.kind, PlacedKind::Schedule(_) | PlacedKind::Legend(_))
        {
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        }
    }
    for (vertical, v) in &st.guides {
        let s = Stroke::new(1.0, t.accent);
        let pts = if *vertical {
            [to_s([*v, 0.0]), to_s([*v, ph])]
        } else {
            [to_s([0.0, *v]), to_s([pw, *v])]
        };
        painter.extend(egui::Shape::dashed_line(&pts, s, 6.0, 4.0));
    }
    if let Some((v, pl, def)) = &act {
        let cr = st
            .crop
            .as_ref()
            .filter(|c| c.vp == *v)
            .map(|c| c.crop)
            .or(act_crop)
            .unwrap_or_default();
        let (a, b) = (
            to_sheet(pl, def, [cr[0], cr[1]]),
            to_sheet(pl, def, [cr[2], cr[3]]),
        );
        let r = Rect::from_two_pos(to_s(a), to_s(b));
        painter.rect_stroke(r, 0.0, Stroke::new(1.5, t.accent), egui::StrokeKind::Middle);
        for g in grips(pl, def, cr) {
            let hot = hover.is_some_and(|h| h.distance(g) < 10.0);
            let gr = Rect::from_center_size(g, Vec2::splat(if hot { 10.0 } else { 8.0 }));
            painter.rect_filled(gr, ddc_ui::theme::radius::SM, t.paper);
            painter.rect_stroke(
                gr,
                2.0,
                Stroke::new(1.5, t.accent),
                egui::StrokeKind::Middle,
            );
        }
        painter.text(
            r.left_top() - Vec2::new(0.0, 6.0),
            egui::Align2::LEFT_BOTTOM,
            tr(ru, "shv.active_view_drag_the_frame_sides_t"),
            egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
            t.accent,
        );
    }

    // drag from the browser
    if let (Some(pl), Some(h)) = (resp.dnd_hover_payload::<Payload>(), hover) {
        let s = Stroke::new(1.5, t.accent);
        let r = Rect::from_center_size(h, Vec2::new(120.0, 80.0));
        let pts = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ];
        painter.extend(egui::Shape::dashed_line(&pts, s, 6.0, 4.0));
        let name = match &*pl {
            Payload::View(_, n) => n.clone(),
            Payload::Schedule(k) => item_title(ed, k, ru),
        };
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
            t.accent,
        );
    }
    if let (Some(pl), Some(h)) = (resp.dnd_release_payload::<Payload>(), hover) {
        let m = to_m(h);
        match place(ed, id, &pl, (m[0], m[1]), 0.0) {
            Ok(()) => {
                if let Ok(v) = ed.model.viewports(id) {
                    if matches!(&*pl, Payload::View(..)) {
                        if let Some(last) = v.iter().map(|x| x.0).max() {
                            st.sel = vec![PlacedKind::Viewport(last)];
                        }
                    }
                }
            }
            Err(e) => ed.say(Severity::Error, e),
        }
    }

    toolbar(
        ui,
        ed,
        t,
        ru,
        st,
        rect,
        &def,
        &vps,
        &act.as_ref().map(|a| a.0),
        &c,
    );
}

/// Moving the selection (viewports and schedules) is one undo step.
/// Sheet edits form one undo step; legend placements are separate model operations.
fn run_steps(ed: &mut Editor, steps: Vec<Step>) {
    if steps.is_empty() {
        return;
    }
    if let Err(e) = ed.apply_ext(crate::mass_tools::steps_rec("undo.sheet.items", steps)) {
        ed.say(Severity::Error, e.to_string());
    }
}

fn commit_move(ed: &mut Editor, sheet: i64, items: &[PlacedKind], d: [f64; 2]) {
    let (mut moves, mut sched, mut legs) = (vec![], vec![], vec![]);
    for kind in items {
        match kind {
            PlacedKind::Viewport(v) => {
                if let Ok(def) = ed.model.viewport(*v) {
                    moves.push((*v, (def.center.0 + d[0], def.center.1 + d[1])));
                }
            }
            PlacedKind::Schedule(s) => {
                let pos = ed
                    .model
                    .schedule_places(sheet)
                    .ok()
                    .and_then(|v| v.into_iter().find(|x| x.0 == *s))
                    .map(|x| x.1.pos);
                if let Some(p) = pos {
                    sched.push((*s, (p.0 + d[0], p.1 + d[1])));
                }
            }
            PlacedKind::Legend(l) => {
                let pos = ed
                    .model
                    .legend_places(sheet)
                    .ok()
                    .and_then(|v| v.into_iter().find(|x| x.id == *l))
                    .map(|x| x.pos);
                if let Some(p) = pos {
                    legs.push((*l, (p.0 + d[0], p.1 + d[1])));
                }
            }
        }
    }
    let mut steps: Vec<Step> = vec![];
    if !(moves.is_empty() && sched.is_empty()) {
        steps.push(Box::new(move |m: &mut ddc_model::Model| {
            m.move_sheet_items(&moves, &sched)
        }));
    }
    for (l, p) in legs {
        steps.push(Box::new(move |m: &mut ddc_model::Model| {
            m.move_legend_place(l, p)
        }));
    }
    run_steps(ed, steps);
}

/// Move snapping: edges and centres of the moved item to edges/centres of the others and to the sheet middle.
/// Returns the adjusted offset and the guides (vertical?, coordinate in mm).
fn snap(
    placed: &[io::Placed],
    items: &[PlacedKind],
    raw: [f64; 2],
    paper: [f64; 2],
    tol: f64,
) -> ([f64; 2], Vec<(bool, f64)>) {
    let Some(m) = placed.iter().find(|p| items.first() == Some(&p.kind)) else {
        return (raw, vec![]);
    };
    let lines = |r: [f64; 4], ax: usize| {
        let (a, l) = (r[ax], r[ax + 2]);
        [a, a + l / 2.0, a + l]
    };
    let mut out = raw;
    let mut guides = vec![];
    for ax in 0..2 {
        let mut targets: Vec<f64> = vec![paper[ax] / 2.0];
        for p in placed.iter().filter(|p| !items.contains(&p.kind)) {
            targets.extend(lines(p.rect, ax));
        }
        let mut best: Option<(f64, f64)> = None;
        for c in lines(m.rect, ax) {
            for tg in &targets {
                let e = tg - (c + raw[ax]);
                if e.abs() <= tol && best.is_none_or(|b| e.abs() < b.0.abs()) {
                    best = Some((e, *tg));
                }
            }
        }
        if let Some((e, tg)) = best {
            out[ax] = raw[ax] + e;
            guides.push((ax == 0, tg));
        }
    }
    (out, guides)
}

#[allow(clippy::too_many_arguments)]
fn toolbar(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    t: &Tokens,
    ru: bool,
    st: &mut Sheets,
    rect: Rect,
    def: &SheetDef,
    vps: &[(i64, ViewportDef)],
    act: &Option<i64>,
    c: &Composed,
) {
    let bar = Rect::from_min_size(
        rect.min + Vec2::new(8.0, 8.0),
        Vec2::new(rect.width() - 16.0, 32.0),
    );
    ui.painter().rect(
        bar,
        t.radius as f32,
        t.panel,
        Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );
    let sel_vp = match st.sel.as_slice() {
        [PlacedKind::Viewport(v)] => vps.iter().find(|x| x.0 == *v).cloned(),
        _ => None,
    };
    let sel_schedule = match st.sel.as_slice() {
        [PlacedKind::Schedule(p)] => Some(*p),
        _ => None,
    };
    if sel_vp.is_none() {
        st.title_buf = None;
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let items = sheet_bar(sel_vp.is_some(), act.is_some(), sel_schedule.is_some());
    let cx = BarCx { def, vps, c, act: *act, sel_vp: sel_vp.as_ref(), sel_schedule };
    let inner = bar.shrink2(Vec2::new(10.0, 4.0));
    // the icon group is laid out first and owns its width: nothing on the left runs under it
    let rw = bar_w(ui.ctx(), "right").unwrap_or(4.0 * (th.d().control + space::SM));
    let right = Rect::from_min_max(egui::pos2(inner.right() - rw, inner.top()), inner.max);
    let mut r_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(right)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    for it in items.iter().filter(|i| i.right()).rev() {
        bar_item(&mut r_ui, &th, ed, ru, st, *it, &cx);
    }
    let used = inner.right() - r_ui.min_rect().left();
    set_bar_w(ui.ctx(), "right", used);
    let l_right = inner.right() - used - space::SM;
    let left = Rect::from_min_max(inner.min, egui::pos2(l_right, inner.bottom()));
    let mut l_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(left)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    l_ui.set_clip_rect(left.intersect(ui.clip_rect()));
    let ui = &mut l_ui;
    let orient = if def.landscape { tr(ru, "shv.landscape") } else { tr(ru, "shv.portrait") };
    let name = format!("{}  {}", def.number, def.name);
    let paper = format!("{} · {orient}", def.paper);
    let body = ddc_ui::typo::font(Role::Body);
    let text_w = |s: &str, f: &egui::FontId| {
        ui.fonts(|x| x.layout_no_wrap(s.to_owned(), f.clone(), Color32::PLACEHOLDER).size().x)
    };
    let sp = ui.spacing().item_spacing.x;
    let (name_w, paper_w) = (text_w(&name, &body), text_w(&paper, &body) + sp);
    // what did not fit the sheet is seen before printing
    let warn = (!c.warnings.is_empty()).then(|| {
        let n = c.warnings.len().to_string();
        msg(ru, "sheets.print.warn_count", &[("n", &n)])
    });
    let warn_w = warn.as_deref().map_or(0.0, |w| text_w(w, &body) + sp);
    let div_w = space::SM + sp;
    let more_w = th.d().control + sp;
    let lefts: Vec<(Item, f32)> = items
        .iter()
        .filter(|i| !i.right())
        .map(|i| (*i, bar_w(ui.ctx(), &format!("{i:?}")).unwrap_or(BAR_ITEM_GUESS)))
        .collect();
    let fixed = warn_w + div_w + sp;
    let kept = bar_keep(left.width() - fixed, name_w.min(BAR_NAME_MIN), &lefts, more_w);
    let items_w: f32 = lefts.iter().filter(|(i, _)| kept.contains(i)).map(|(_, w)| w).sum();
    let gone = kept.len() < lefts.len();
    let head = left.width() - fixed - items_w - if gone { more_w } else { 0.0 };
    let show_paper = head >= name_w + paper_w;
    let name_room = if show_paper { name_w } else { head.max(BAR_NAME_MIN.min(name_w)) };
    ui.allocate_ui(Vec2::new(name_room, inner.height()), |ui| {
        ui.centered_and_justified(|ui| {
            ddc_ui::widgets::elided_label(ui, &name, body.clone(), t.text);
        });
    });
    if show_paper {
        ui.label(RichText::new(paper).color(t.text_dim));
    }
    if let Some(w) = warn {
        ui.label(RichText::new(w).color(ui.visuals().warn_fg_color))
            .on_hover_text(c.warnings.join("\n"));
    }
    ddc_ui::widgets::vdivider(ui, &th, th.d().control);
    for (it, _) in lefts.iter().filter(|(i, _)| kept.contains(i)) {
        let x0 = ui.cursor().min.x;
        bar_item(ui, &th, ed, ru, st, *it, &cx);
        set_bar_w(ui.ctx(), &format!("{it:?}"), ui.cursor().min.x - x0);
    }
    if gone {
        let tip = tr(ru, "sheet_tools.sheet.more");
        let ctx = ui.ctx().clone();
        let more = ddc_ui::widgets::with_icons(&ctx, |icons| {
            ddc_ui::widgets::icon_button(ui, icons, &th, "context-menu", false, &tip)
        });
        more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tip));
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(control::DROPDOWN_W);
            for (it, _) in lefts.iter().filter(|(i, _)| !kept.contains(i)) {
                if *it != Item::Divider {
                    bar_item(ui, &th, ed, ru, st, *it, &cx);
                }
            }
        });
    }
}

/// Width given to a toolbar item before it was ever drawn.
const BAR_ITEM_GUESS: f32 = 120.0;
/// The sheet name keeps at least this much (elided) before items leave for the overflow menu.
const BAR_NAME_MIN: f32 = 96.0;

fn bar_w(ctx: &egui::Context, k: &str) -> Option<f32> {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::new(("ddc.sheet.bar.w", k))))
}

fn set_bar_w(ctx: &egui::Context, k: &str, w: f32) {
    if bar_w(ctx, k).is_none_or(|o| (o - w).abs() > 0.5) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(("ddc.sheet.bar.w", k)), w));
        ctx.request_repaint();
    }
}

/// Left toolbar items that fit `avail` beside a sheet name of at least `name_min`: Open view
/// leaves first, then Activate, Detail, Title and Scale; once one leaves, the overflow button
/// (`more`) needs its room too. Order of the kept items is unchanged.
fn bar_keep(avail: f32, name_min: f32, items: &[(Item, f32)], more: f32) -> Vec<Item> {
    const LEAVE: [Item; 5] =
        [Item::OpenView, Item::Activate, Item::Detail, Item::Title, Item::Scale];
    let mut kept: Vec<Item> = items.iter().map(|(i, _)| *i).collect();
    let need = |kept: &[Item]| {
        let w: f32 = items.iter().filter(|(i, _)| kept.contains(i)).map(|(_, w)| w).sum();
        name_min + w + if kept.len() < items.len() { more } else { 0.0 }
    };
    for leave in LEAVE {
        if need(&kept) <= avail {
            break;
        }
        kept.retain(|i| *i != leave);
    }
    if kept.iter().all(|i| *i == Item::Divider) {
        kept.clear();
    }
    kept
}

/// One control of the sheet toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// Accent banner: a view is being edited on the sheet.
    Banner,
    ResetCrop,
    Done,
    Scale,
    Title,
    Detail,
    Activate,
    OpenView,
    /// Rest of the selected schedule: column height and gap.
    Split,
    Divider,
    PlaceView,
    Properties,
    Print,
    Fit,
}

impl Item {
    /// Icon-only buttons sit at the right edge.
    fn right(self) -> bool {
        matches!(self, Item::PlaceView | Item::Properties | Item::Print | Item::Fit)
    }

    /// Catalog key of the tooltip, or `None` for items that need none.
    fn tip_key(self) -> Option<&'static str> {
        Some(match self {
            Item::PlaceView => "sheet_place.place_view_tip",
            Item::Properties => "sheet_place.properties_tip",
            Item::Print => "sheet_place.print_tip",
            Item::Fit => "sheet_place.fit_tip",
            Item::Activate => "sheet_place.activate_tip",
            Item::OpenView => "sheet_place.open_view_tip",
            Item::Done => "sheet_place.done_tip",
            Item::ResetCrop => "sheet_place.reset_crop_tip",
            Item::Scale => "sheet_place.scale_tip",
            Item::Title => "sheet_place.title_tip",
            Item::Detail => "sheets.detail.hint",
            Item::Split => "sheets.split.height_hint",
            Item::Banner | Item::Divider => return None,
        })
    }

    /// Icon of a button, or `None` for plain controls.
    fn icon(self) -> Option<&'static str> {
        Some(match self {
            Item::PlaceView => "place-view",
            Item::Properties => "properties",
            Item::Print => "print",
            Item::Fit => "zoom-fit",
            Item::Activate => "edit-points",
            Item::OpenView => "open",
            Item::Done => "check",
            _ => return None,
        })
    }

    /// Shortcut shown in the tooltip.
    fn keys(self) -> &'static str {
        match self {
            Item::Done => "Esc",
            _ => "",
        }
    }
}

/// What the sheet toolbar shows, left to right. The edit group (banner, reset crop, Done) replaces
/// everything while a view is active; scale, title and the rest appear only with a viewport
/// selected, the schedule split only with a schedule.
fn sheet_bar(sel_vp: bool, active_vp: bool, sel_schedule: bool) -> Vec<Item> {
    if active_vp {
        return vec![Item::Banner, Item::ResetCrop, Item::Done];
    }
    let mut v = Vec::new();
    if sel_vp {
        v.extend([Item::Scale, Item::Title, Item::Detail, Item::Activate, Item::OpenView]);
        v.push(Item::Divider);
    }
    if sel_schedule {
        v.extend([Item::Split, Item::Divider]);
    }
    v.extend([Item::PlaceView, Item::Properties, Item::Print, Item::Fit]);
    v
}

/// Height of the scrolling lists in the place-view and sheet-set dialogs.
const LIST_H: f32 = 288.0;

/// Verb (see `ctx_menu::verb_icon`) of a sheet row menu item.
fn sheet_menu_verb(k: u8) -> &'static str {
    match k {
        0 => "open",
        1 => "edit",
        2 => "move_up",
        3 => "move_down",
        4 => "delete",
        _ => "duplicate",
    }
}

/// Sheet row menu: only Delete (4) is destructive.
fn sheet_menu_tone(k: u8) -> MenuTone {
    if k == 4 {
        MenuTone::Danger
    } else {
        MenuTone::Normal
    }
}

/// Facts the toolbar items read.
struct BarCx<'a> {
    def: &'a SheetDef,
    vps: &'a [(i64, ViewportDef)],
    c: &'a Composed,
    act: Option<i64>,
    sel_vp: Option<&'a (i64, ViewportDef)>,
    sel_schedule: Option<i64>,
}

fn bar_tip(resp: egui::Response, th: &Theme, ru: bool, it: Item) -> egui::Response {
    match it.tip_key() {
        Some(k) => ddc_ui::widgets::rich_tooltip(resp, th, tr(ru, k), "", it.keys()),
        None => resp,
    }
}

fn bar_item(
    ui: &mut egui::Ui,
    th: &Theme,
    ed: &mut Editor,
    ru: bool,
    st: &mut Sheets,
    it: Item,
    cx: &BarCx,
) {
    use ddc_ui::widgets::{self, Button};
    let ctx = ui.ctx().clone();
    match it {
        Item::Banner => {
            widgets::badge(ui, th, tr(ru, "sheet_place.banner"), widgets::Tone::Accent);
        }
        Item::Divider => widgets::vdivider(ui, th, th.d().control),
        Item::ResetCrop => {
            let r = Button::quiet(tr(ru, "shv.reset_crop")).small().show(ui, th);
            if bar_tip(r, th, ru, it).clicked() {
                if let Some(v) = cx.act {
                    reset_crop(ed, ru, cx, v);
                }
            }
        }
        Item::Done => {
            let r = widgets::with_icons(&ctx, |icons| {
                Button::primary(tr(ru, "shv.done"))
                    .small()
                    .show_icon(ui, th, icons, "check")
            });
            if bar_tip(r, th, ru, it).clicked() {
                st.active_vp = None;
            }
        }
        Item::Scale => {
            if let Some((vid, v)) = cx.sel_vp {
                let vals = scale_choices(v.scale, false);
                let labels: Vec<String> = vals.iter().map(|s| scale_label(*s, ru)).collect();
                let names: Vec<&str> = labels.iter().map(String::as_str).collect();
                let mut i = vals.iter().position(|s| *s == v.scale).unwrap_or(0);
                widgets::form::label(ui, th, tr(ru, "shv.scale"), control::DROPDOWN_W);
                let r = widgets::dropdown(
                    ui,
                    th,
                    ("sheet.scale", *vid),
                    &mut i,
                    &names,
                    Some(control::BUTTON_MIN_W + space::LG),
                );
                let changed = r.changed();
                bar_tip(r, th, ru, it);
                if changed && vals[i] != v.scale {
                    let mut nd = v.clone();
                    nd.scale = vals[i];
                    let vid = *vid;
                    if let Err(e) = op_in(ed, ru, move |m| m.update_viewport(vid, &nd)) {
                        ed.say(Severity::Error, e);
                    }
                }
            }
        }
        Item::Title => {
            if let Some((vid, v)) = cx.sel_vp {
                if st.title_buf.as_ref().is_none_or(|b| b.0 != *vid) {
                    st.title_buf = Some((*vid, v.title.clone()));
                }
                let mut buf = st
                    .title_buf
                    .as_ref()
                    .map(|b| b.1.clone())
                    .unwrap_or_default();
                let caption = tr(ru, "shv.view_title");
                let lab = widgets::form::label(ui, th, caption, control::DROPDOWN_W);
                let out = widgets::TextField::new(&mut buf)
                    .labelled_by(lab.id, caption)
                    .id_salt(("sheet.vp_title", *vid))
                    .hint(tr(ru, "shv.view_title"))
                    .width(control::DROPDOWN_W)
                    .show(ui, th);
                st.title_buf = Some((*vid, buf.clone()));
                bar_tip(out.response, th, ru, it);
                if out.committed && buf != v.title {
                    let mut nd = v.clone();
                    nd.title = buf;
                    let vid = *vid;
                    if let Err(e) = op_in(ed, ru, move |m| m.update_viewport(vid, &nd)) {
                        ed.say(Severity::Error, e);
                    }
                }
            }
        }
        Item::Detail => {
            if let Some((vid, v)) = cx.sel_vp {
                detail_field(ui, th, ed, ru, st, *vid, v.sheet);
            }
        }
        Item::Activate => {
            if let Some((vid, _)) = cx.sel_vp {
                let r = widgets::with_icons(&ctx, |icons| {
                    Button::secondary(tr(ru, "shv.activate"))
                        .small()
                        .show_icon(ui, th, icons, "edit-points")
                });
                if bar_tip(r, th, ru, it).clicked() {
                    st.active_vp = Some(*vid);
                }
            }
        }
        Item::OpenView => {
            if let Some((_, v)) = cx.sel_vp {
                let r = widgets::with_icons(&ctx, |icons| {
                    Button::secondary(tr(ru, "shv.open_view"))
                        .small()
                        .show_icon(ui, th, icons, "open")
                });
                if bar_tip(r, th, ru, it).clicked() {
                    st.open_view = Some(v.view);
                }
            }
        }
        Item::Split => {
            if let Some(pid) = cx.sel_schedule {
                schedule_bar(ui, th, ed, ru, st, pid);
            }
        }
        Item::PlaceView | Item::Properties | Item::Print | Item::Fit => {
            let icon = it.icon().unwrap_or_default();
            let r = widgets::with_icons(&ctx, |icons| {
                widgets::icon_button(ui, icons, th, icon, false, "")
            });
            if !bar_tip(r, th, ru, it).clicked() {
                return;
            }
            match it {
                Item::PlaceView => st.place = Some(Place::default()),
                Item::Properties => {
                    st.props = Some(Props::load(ed, cx.def.clone(), st.active));
                }
                Item::Print => st.print = Some(Print::new(st.active)),
                _ => st.cam = None,
            }
        }
    }
}

/// Moves the active view's centre back to the middle of its drawing and drops the crop.
fn reset_crop(ed: &mut Editor, ru: bool, cx: &BarCx, v: i64) {
    if let (Some((_, d)), Some(p)) = (
        cx.vps.iter().find(|x| x.0 == v),
        cx.c.placed.iter().find(|p| p.kind == PlacedKind::Viewport(v)),
    ) {
        if let Ok(view) = io::view_drawing(&ed.model, d) {
            let e = io::extent(&view);
            let ec = [(e[0] + e[2]) / 2.0, (e[1] + e[3]) / 2.0];
            let mut nd = d.clone();
            nd.center = (
                d.center.0 + (ec[0] - p.model_c[0]) * 1000.0 / p.scale,
                d.center.1 + (ec[1] - p.model_c[1]) * 1000.0 / p.scale,
            );
            nd.crop = None;
            if let Err(e) = op_in(ed, ru, move |m| m.update_viewport(v, &nd)) {
                ed.say(Severity::Error, e);
            }
        }
    }
}

/// Scales offered by a scale dropdown: the standard ones, the current custom one first, and
/// «fit to sheet» (0) first when `with_fit`.
fn scale_choices(cur: f64, with_fit: bool) -> Vec<f64> {
    let mut v: Vec<f64> = Vec::new();
    if with_fit {
        v.push(0.0);
    }
    if cur > 0.0 && !SCALES.contains(&cur) {
        v.push(cur);
    }
    v.extend(SCALES);
    v
}

fn scale_label(s: f64, ru: bool) -> String {
    if s > 0.0 {
        format!("1:{s}")
    } else {
        tr(ru, "shv.fit_to_sheet").to_string()
    }
}

/// View number on a sheet: an empty value restores automatic numbering.
fn detail_field(
    ui: &mut egui::Ui,
    th: &Theme,
    ed: &mut Editor,
    ru: bool,
    st: &mut Sheets,
    vid: i64,
    sheet: i64,
) {
    let cur = ed
        .model
        .viewport_details(sheet)
        .unwrap_or_default()
        .into_iter()
        .find(|x| x.0 == vid)
        .map(|x| x.1)
        .unwrap_or_default();
    let mut buf = match &st.detail_buf {
        Some((i, b)) if *i == vid => b.clone(),
        _ => cur.clone(),
    };
    let caption = tr(ru, "shv.number");
    let lab = ddc_ui::widgets::form::label(ui, th, caption, control::DROPDOWN_W);
    let out = ddc_ui::widgets::TextField::new(&mut buf)
        .labelled_by(lab.id, caption)
        .id_salt(("sheet.vp_detail", vid))
        .width(control::FIELD_MIN_W)
        .show(ui, th);
    bar_tip(out.response.clone(), th, ru, Item::Detail);
    if out.committed && buf.trim() != cur {
        let value = buf.trim().to_string();
        if let Err(e) = op_in(ed, ru, move |m| m.set_viewport_detail(vid, &value)) {
            ed.say(Severity::Error, e);
        }
    }
    st.detail_buf = out.response.has_focus().then_some((vid, buf));
}

/// Splitting the selected schedule: column height (0 means down to the frame bottom) and gap.
fn schedule_bar(ui: &mut egui::Ui, th: &Theme, ed: &mut Editor, ru: bool, st: &mut Sheets, pid: i64) {
    use ddc_ui::widgets::{NumberField, NumberFormat};
    let cur = ed.model.schedule_split(pid).unwrap_or_default();
    let mut b = match st.split_buf {
        Some((i, s)) if i == pid => s,
        _ => cur,
    };
    let mm = msg(ru, "sheets.split.mm", &[]);
    let mm = mm.trim();
    let nf = NumberFormat::for_lang(if ru { Lang::Ru } else { Lang::En });
    let width = control::FIELD_MIN_W + space::XL;
    let hl_text = msg(ru, "sheets.split.height", &[]);
    let hl = ui.label(&hl_text);
    let h = NumberField::new(("sheet.split.h", pid), &mut b.max_h, nf)
        .labelled_by(hl.id, &hl_text)
        .range(0.0, 1000.0)
        .unit(mm)
        .width(width)
        .show(ui, th);
    bar_tip(h.response.clone(), th, ru, Item::Split);
    let gl_text = msg(ru, "sheets.split.gap", &[]);
    let gl = ui.label(&gl_text);
    let g = NumberField::new(("sheet.split.g", pid), &mut b.gap, nf)
        .labelled_by(gl.id, &gl_text)
        .decimals(1)
        .step(0.5)
        .range(0.0, 100.0)
        .unit(mm)
        .width(width)
        .show(ui, th);
    if (h.committed || g.committed) && b != cur {
        if let Err(e) = op_in(ed, ru, move |m| m.set_schedule_split(pid, b)) {
            ed.say(Severity::Error, e);
        }
    }
    let editing = h.response.has_focus() || g.response.has_focus();
    st.split_buf = editing.then_some((pid, b));
}

fn place_window(ctx: &egui::Context, ed: &mut Editor, ru: bool, sheet: i64, p: &mut Place) -> bool {
    let items = place_items(ed, ru);
    let th = ddc_ui::theme::of(ctx);
    let mut keep = true;
    let mut open = true;
    kit::Dialog::new(
        "sheet_place_view",
        tr(ru, "shv.place_view"),
        kit::DialogSize::Confirm,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        if items.is_empty() {
            kit::empty_note(
                ui,
                tr(ru, "shv.no_views_to_place"),
                Some(tr(ru, "shv.create_a_plan_a_section_or_a_sched")),
            );
        }
        egui::ScrollArea::vertical()
            .max_height(LIST_H)
            .show(ui, |ui| {
                for (i, (_, label, on)) in items.iter().enumerate() {
                    let right = if on.is_empty() {
                        String::new()
                    } else {
                        format!("{} {on}", tr(ru, "shv.on_sheet"))
                    };
                    if row(ui, &th, label, &right, p.pick == i, space::SM, Sense::click()).clicked()
                    {
                        p.pick = i;
                    }
                }
            });
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.label(tr(ru, "shv.scale"));
            let vals = scale_choices(0.0, true);
            let labels: Vec<String> = vals.iter().map(|s| scale_label(*s, ru)).collect();
            let names: Vec<&str> = labels.iter().map(String::as_str).collect();
            let mut i = vals.iter().position(|s| *s == p.scale).unwrap_or(0);
            let r = ddc_ui::widgets::dropdown(ui, &th, "sheet.place.scale", &mut i, &names, None);
            if r.changed() {
                p.scale = vals[i];
            }
        });
        kit::dialog_footer(ui, |ui| {
            let why =
                items
                    .is_empty()
                    .then_some(tr(ru, "shv.no_views_to_place"));
            if kit::primary_action(ui, tr(ru, "shv.place"), why) {
                if let Some((pl, ..)) = items.get(p.pick) {
                    let (w, h) = paper_of(ed, sheet);
                    if let Err(e) = place(ed, sheet, pl, free_center(w, h), p.scale) {
                        ed.say(Severity::Error, e);
                    }
                }
            }
            if kit::cancel_button(ui, tr(ru, "shv.close")).clicked() {
                keep = false;
            }
        });
    });
    keep && open
}

impl Sets {
    fn new(ed: &Editor, ru: bool) -> Self {
        let first = ed
            .model
            .sheet_sets()
            .ok()
            .and_then(|v| v.into_iter().next());
        match first {
            Some((id, name, members)) => Sets {
                cur: Some(id),
                name,
                members,
                err: String::new(),
                ask_delete: false,
            },
            None => Sets::blank(ed, ru),
        }
    }
    fn blank(ed: &Editor, ru: bool) -> Self {
        Sets {
            cur: None,
            name: msg(ru, "sheets.default_set", &[]),
            members: ed
                .model
                .sheets()
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.0)
                .collect(),
            err: String::new(),
            ask_delete: false,
        }
    }
}

fn sets_window(ctx: &egui::Context, ed: &mut Editor, ru: bool, s: &mut Sets) -> bool {
    let sets = ed.model.sheet_sets().unwrap_or_default();
    let sheets = ed.model.sheets().unwrap_or_default();
    let th = ddc_ui::theme::of(ctx);
    let mut keep = true;
    let mut open = true;
    kit::Dialog::new(
        "sheet_sets",
        tr(ru, "shv.sheet_sets"),
        kit::DialogSize::Form,
    )
    .size([480.0, 0.0])
    .open(&mut open)
    .show(ctx, |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(control::DROPDOWN_W);
                for (id, name, m) in &sets {
                    if row(ui, &th, name, &m.len().to_string(), s.cur == Some(*id), space::SM, Sense::click())
                        .clicked()
                    {
                        *s = Sets {
                            cur: Some(*id),
                            name: name.clone(),
                            members: m.clone(),
                            err: String::new(),
                            ask_delete: false,
                        };
                    }
                }
                if ddc_ui::widgets::Button::quiet(tr(ru, "shv.new_set"))
                    .small()
                    .tooltip(tr(ru, "sheet_place.new_set_tip"), "")
                    .show(ui, &th)
                    .clicked()
                {
                    *s = Sets::blank(ed, ru);
                }
            });
            ddc_ui::widgets::vdivider(ui, &th, LIST_H);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let nl = ui.label(tr(ru, "shv.name"));
                    ddc_ui::widgets::TextField::new(&mut s.name)
                        .labelled_by(nl.id, tr(ru, "shv.name"))
                        .id_salt("sheet.set.name")
                        .show(ui, &th);
                });
                egui::ScrollArea::vertical()
                    .max_height(LIST_H)
                    .show(ui, |ui| {
                        if sheets.is_empty() {
                            kit::empty_note(
                                ui,
                                tr(ru, "shv.the_project_has_no_sheets"),
                                None,
                            );
                        }
                        for (id, d) in &sheets {
                            let mut on = s.members.contains(id);
                            if ddc_ui::widgets::checkbox(
                                ui,
                                &th,
                                &mut on,
                                &format!("{}  {}", d.number, d.name),
                            )
                            .changed()
                            {
                                if on {
                                    s.members.push(*id);
                                } else {
                                    s.members.retain(|x| x != id);
                                }
                            }
                        }
                    });
            });
        });
        if !s.err.is_empty() {
            kit::error_note(ui, &s.err);
        }
        let mut del = None;
        if s.ask_delete && s.cur.is_some() {
            // the destructive step asks first and names what goes
            let q = msg(ru, "sheet_tools.sets.delete_ask", &[("name", s.name.trim())]);
            kit::warn_note(ui, q);
        }
        let (cur, asking) = (s.cur, s.ask_delete);
        let mut ask = asking;
        let left = |ui: &mut egui::Ui| {
            let Some(id) = cur else { return };
            if asking {
                if kit::confirm_danger_button(ui, tr(ru, "shv.delete_set"), true).clicked() {
                    del = Some(id);
                }
                if kit::cancel_button(ui, tr(ru, "sheet_tools.keep")).clicked() {
                    ask = false;
                }
            } else if kit::danger_button(ui, tr(ru, "shv.delete_set"), true).clicked() {
                ask = true;
            }
        };
        kit::dialog_footer_split(ui, left, |ui| {
            let why = s.name.trim().is_empty().then_some(tr(ru, "shv.enter_a_set_name"));
            if kit::primary_action(ui, tr(ru, "shv.save"), why) {
                // set order follows the sheet order in the project
                let members: Vec<i64> = sheets
                    .iter()
                    .map(|x| x.0)
                    .filter(|i| s.members.contains(i))
                    .collect();
                let name = s.name.trim().to_string();
                let r = match s.cur {
                    Some(id) => op_in(ed, ru, move |m| m.update_sheet_set(id, &name, &members)),
                    None => op_in(ed, ru, move |m| m.add_sheet_set(&name, &members).map(|_| ())),
                };
                match r {
                    Ok(()) => {
                        s.err.clear();
                        if s.cur.is_none() {
                            s.cur = ed.model.sheet_sets().ok().and_then(|v| {
                                v.into_iter().find(|x| x.1 == s.name.trim()).map(|x| x.0)
                            });
                        }
                    }
                    Err(e) => s.err = e,
                }
            }
            if kit::cancel_button(ui, tr(ru, "shv.close")).clicked() {
                keep = false;
            }
        });
        s.ask_delete = ask && s.cur == cur;
        if let Some(id) = del {
            match op_in(ed, ru, move |m| m.delete_sheet_set(id)) {
                Ok(()) => *s = Sets::new(ed, ru),
                Err(e) => s.err = e,
            }
        }
    });
    keep && open
}

/// "Sheets" section of the browser: sheets, viewports of the active sheet, what to place, sets.
pub fn browser(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    cat: &Catalog,
    ed: &mut Editor,
    query: &str,
) {
    with(|st| browser_in(ui, th, icons, cat, ed, st, query));
}

/// Row of the project browser on the shared row system (one height, one icon and label x).
fn brow(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    icon: Option<&str>,
    (text, right): (&str, &str),
    (on, depth, sense): (bool, u16, Sense),
) -> egui::Response {
    crate::shell::panels::browser_sec::row(ui, th, icons, icon, text, right, on, depth, sense)
}

fn row(
    ui: &mut egui::Ui,
    th: &Theme,
    text: &str,
    right: &str,
    on: bool,
    indent: f32,
    sense: Sense,
) -> egui::Response {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, control::HIT_MIN), sense);
    let fg = ddc_ui::widgets::paint_state(ui, th, r, &resp, on);
    let p = ui.painter();
    p.text(
        r.left_center() + egui::vec2(indent, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        th.font(if on { Role::BodyStrong } else { Role::Body }),
        if on { th.c("accent") } else { fg },
    );
    if !right.is_empty() {
        p.text(
            r.right_center() - egui::vec2(space::SM, 0.0),
            egui::Align2::RIGHT_CENTER,
            right,
            th.font(Role::Numeric),
            th.c("text.secondary"),
        );
    }
    resp
}

fn browser_in(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    cat: &Catalog,
    ed: &mut Editor,
    st: &mut Sheets,
    query: &str,
) {
    let ru = cat.lang == Lang::Ru;
    // sheet list and viewport counts are read once per (revision, total_changes), not every frame
    let rk = ed.cache_key();
    let ck = egui::Id::new("ddc.browser.sheets");
    type Cached = ((u64, u64), Vec<(i64, ddc_model::SheetDef)>, Vec<(i64, usize)>);
    let (mut sheets, counts) = match ui.data(|d| d.get_temp::<Cached>(ck)).filter(|c| c.0 == rk) {
        Some(c) => (c.1, c.2),
        None => {
            let s = ed.model.sheets().unwrap_or_default();
            let n: Vec<(i64, usize)> = s
                .iter()
                .map(|(id, _)| (*id, ed.model.viewports(*id).map(|v| v.len()).unwrap_or(0)))
                .collect();
            ui.data_mut(|d| d.insert_temp(ck, (rk, s.clone(), n.clone())));
            (s, n)
        }
    };
    let q = query.trim().to_lowercase();
    if !q.is_empty() {
        sheets.retain(|(_, d)| {
            format!("{} {}", d.number, d.name)
                .to_lowercase()
                .contains(&q)
        });
        if sheets.is_empty() {
            return;
        }
    }
    let head = cat.t("browser.sheets");
    let sec = crate::shell::panels::browser_sec::section(
        ui,
        th,
        &head,
        &head,
        Some(sheets.len()),
        Some(tr(ru, "sheet_place.new_sheet_tip")),
    );
    if sec.add {
        st.props = Some(Props::new_with(ed, ru, st.active));
    }
    if sec.open {
        // 0 open, 1 properties, 2 up, 3 down, 4 delete
        let mut act: Option<(i64, u8)> = None;
        for (id, d) in &sheets {
            let on = st.active == Some(*id);
            let n = counts.iter().find(|c| c.0 == *id).map_or(0, |c| c.1);
            let r = brow(
                ui,
                th,
                icons,
                Some("sheet"),
                (format!("{}  {}", d.number, d.name).as_str(), n.to_string().as_str()),
                (on, 0, Sense::click()),
            );
            if r.clicked() || r.double_clicked() {
                act = Some((*id, 0));
            }
            r.context_menu(|ui| {
                for (k, label) in [
                    (0, tr(ru, "shv.open").to_string()),
                    (1, tr(ru, "shv.properties").to_string()),
                    (2, tr(ru, "shv.move_up").to_string()),
                    (3, tr(ru, "shv.move_down").to_string()),
                    (5, msg(ru, "sheets.dup.menu_views", &[])),
                    (6, msg(ru, "sheets.dup.menu_title", &[])),
                    (4, tr(ru, "shv.delete").to_string()),
                ] {
                    let tone = sheet_menu_tone(k);
                    if tone == MenuTone::Danger {
                        // destructive item: error ink, set apart from the neutral ones
                        menu_divider(ui, th);
                    }
                    let icon = crate::ctx_menu::verb_icon(sheet_menu_verb(k));
                    if menu_item_icon(ui, th, icon, &label, "", tone, true).clicked() {
                        act = Some((*id, k));
                        ui.close();
                    }
                }
            });
            if on {
                for (vid, v) in ed.model.viewports(*id).unwrap_or_default() {
                    let kind = PlacedKind::Viewport(vid);
                    let name = if v.title.is_empty() {
                        tr(ru, "shv.view").to_string()
                    } else {
                        v.title.clone()
                    };
                    let r = brow(
                        ui,
                        th,
                        icons,
                        Some("plan-view"),
                        (name.as_str(), format!("1:{}", v.scale).as_str()),
                        (st.sel.contains(&kind), 1, Sense::click()),
                    );
                    if r.clicked() {
                        st.sel = vec![kind];
                    }
                    if r.double_clicked() {
                        st.active_vp = Some(vid);
                    }
                }
                for (pid, p) in ed.model.schedule_places(*id).unwrap_or_default() {
                    let kind = PlacedKind::Schedule(pid);
                    let name = item_title(ed, &p.schedule, ru);
                    let sel = st.sel.contains(&kind);
                    let icon = Some("table");
                    let it = (&*name, tr(ru, "shv.schedule_2"));
                    if brow(ui, th, icons, icon, it, (sel, 1, Sense::click())).clicked() {
                        st.sel = vec![kind];
                    }
                }
            }
        }
        // empty or not: one quiet add row, never a block
        let tip = tr(ru, "sheet_place.new_sheet_tip");
        if crate::shell::panels::browser_sec::add_row(ui, th, tr(ru, "shv.new_sheet"), tip) {
            st.props = Some(Props::new_with(ed, ru, st.active));
        }
        match act {
            Some((id, 0)) => open_sheet(st, id),
            Some((id, 1)) => {
                if let Ok(d) = ed.model.sheet(id) {
                    st.props = Some(Props::load(ed, d, Some(id)));
                }
            }
            Some((id, k @ (2 | 3))) => {
                let dl = if k == 2 { -1 } else { 1 };
                if let Err(e) = op_in(ed, ru, move |m| m.move_sheet(id, dl)) {
                    ed.say(Severity::Error, e);
                }
            }
            Some((id, k @ (5 | 6))) => match dup_op(ed, ru, id, k == 5) {
                Ok(n) => {
                    open_sheet(st, n);
                    let m = dup_message(ed, ru, id, n);
                    ed.say(Severity::Info, m);
                }
                Err(e) => ed.say(Severity::Error, e),
            },
            Some((id, _)) => ask_delete(ed, st, id),
            None => {}
        }
    }
    if let Some(sheet) = st.active {
        let head = cat.t("browser.place");
        let items = place_items(ed, ru);
        let sec = crate::shell::panels::browser_sec::section(
            ui,
            th,
            &head,
            &head,
            Some(items.len()),
            None,
        );
        if sec.open {
            for (pl, label, on) in items {
                let it = (&*label, &*on);
                let r = brow(ui, th, icons, None, it, (false, 0, Sense::click_and_drag()));
                r.dnd_set_drag_payload(pl.clone());
                if r.dragged() {
                    if let Some(p) = ui.ctx().pointer_latest_pos() {
                        egui::Area::new(egui::Id::new("sheet.dnd"))
                            .order(egui::Order::Tooltip)
                            .fixed_pos(p + egui::vec2(14.0, 8.0))
                            .interactable(false)
                            .show(ui.ctx(), |ui| {
                                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(&label));
                            });
                    }
                }
                let r = r.on_hover_text(tr(ru, "shv.drag_onto_the_sheet_or_double_clic"));
                if r.double_clicked() {
                    let (w, h) = paper_of(ed, sheet);
                    if let Err(e) = place(ed, sheet, &pl, free_center(w, h), 0.0) {
                        ed.say(Severity::Error, e);
                    }
                }
            }
        }
    }
    let sets = ed.model.sheet_sets().unwrap_or_default();
    let head = cat.t("browser.sheet_sets");
    let open = !sheets.is_empty()
        && crate::shell::panels::browser_sec::section(
            ui,
            th,
            &head,
            &head,
            Some(sets.len()),
            None,
        )
        .open;
    if open {
        for (id, name, m) in &sets {
            let r = brow(
                ui,
                th,
                icons,
                None,
                (name.as_str(), m.len().to_string().as_str()),
                (false, 0, Sense::click()),
            );
            if r.double_clicked() {
                st.sets = Some(Sets {
                    cur: Some(*id),
                    name: name.clone(),
                    members: m.clone(),
                    err: String::new(),
                    ask_delete: false,
                });
            }
            r.context_menu(|ui| {
                let print_icon = crate::ctx_menu::verb_icon("print");
                if menu_item_icon(ui, th, print_icon, tr(ru, "shv.print"), "", MenuTone::Normal, true)
                    .clicked()
                {
                    let mut p = Print::new(st.active);
                    p.range = 3;
                    p.set = Some(*id);
                    st.print = Some(p);
                    ui.close();
                }
                let edit_icon = crate::ctx_menu::verb_icon("edit");
                if menu_item_icon(ui, th, edit_icon, tr(ru, "shv.edit"), "", MenuTone::Normal, true)
                    .clicked()
                {
                    st.sets = Some(Sets {
                        cur: Some(*id),
                        name: name.clone(),
                        members: m.clone(),
                        err: String::new(),
                        ask_delete: false,
                    });
                    ui.close();
                }
            });
        }
        let r = ddc_ui::widgets::Button::quiet(tr(ru, "shv.new_set")).small().show(ui, th);
        if ddc_ui::widgets::rich_tooltip(r, th, tr(ru, "sheet_place.new_set_tip"), "", "").clicked() {
            st.sets = Some(Sets::blank(ed, ru));
        }
    }
}

/// "Sheets per level": a sheet for each level that has no plan on any sheet, modelled on the open sheet.
fn sheets_for_levels(app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let ed = &mut app.ed;
    let levels: Vec<i64> = ed
        .levels()
        .into_iter()
        .map(|l| l.0)
        .filter(|l| {
            ed.model
                .placements(ViewRef::Plan {
                    level: *l,
                    cut: DEFAULT_CUT,
                })
                .map(|p| p.is_empty())
                .unwrap_or(true)
        })
        .collect();
    if levels.is_empty() {
        let m = tr(ru, "shv.every_level_plan_is_already_on_a_s");
        app.sh.toasts.push(Level::Info, m, None);
        return;
    }
    let mut tpl = Props::new_with(ed, ru, active()).def;
    tpl.name = tr(ru, "shv.plan").into();
    let paper = io::paper_size(&tpl.paper, tpl.landscape).unwrap_or((420.0, 297.0));
    let scale = levels
        .iter()
        .map(|l| {
            auto_scale(
                ed,
                ViewRef::Plan {
                    level: *l,
                    cut: DEFAULT_CUT,
                },
                paper,
            )
        })
        .fold(0.0, f64::max);
    let center = free_center(paper.0, paper.1);
    let n = levels.len();
    let before: Vec<i64> = ed
        .model
        .sheets()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.0)
        .collect();
    let set_name = msg(ru, "sheets.levels_set", &[]);
    match op_in(ed, ru, move |m| {
        m.add_plan_sheet_set(&set_name, &tpl, &levels, scale, center)
            .map(|_| ())
    }) {
        Ok(()) => {
            let sheets = ed.model.sheets().unwrap_or_default();
            let first = sheets
                .into_iter()
                .map(|s| s.0)
                .find(|i| !before.contains(i));
            if let Some(id) = first {
                with(|st| open_sheet(st, id));
            }
            let m = format!(
                "{}: {n} · 1:{scale}",
                tr(ru, "shv.sheets_created")
            );
            app.sh.toasts.push(Level::Success, m, None);
        }
        Err(e) => app.sh.toasts.push(Level::Error, e, None),
    }
}

fn cmd_new(app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let p = Props::new_with(&app.ed, ru, active());
    with(|s| s.props = Some(p));
}

/// The open sheet; if there is none, a hint and the new-sheet window.
fn active_or_new(app: &mut EditorApp) -> Option<i64> {
    let id = active();
    if id.is_none() {
        // a project with sheets: take the first, nothing to ask; without: the "new sheet"
        // window is the answer, the hint goes to the status line instead of a second toast
        if let Some((first, _)) = app.ed.model.sheets().ok().and_then(|v| v.into_iter().next()) {
            open(first);
            return Some(first);
        }
        let ru = app.sh.cat.lang == Lang::Ru;
        app.ed.status = Some(tr(ru, "shv.open_a_sheet_or_create_one").to_string());
        cmd_new(app);
    }
    id
}

fn cmd_place(app: &mut EditorApp) {
    if active_or_new(app).is_some() {
        with(|s| s.place = Some(Place::default()));
    }
}

/// Top-left corner of a new table: the first top-left spot that does not overlap placed items.
fn free_spot(w: f64, h: f64, placed: &[[f64; 4]]) -> (f64, f64) {
    const SIZE: (f64, f64) = (100.0, 60.0);
    let hits = |x: f64, y: f64| {
        placed
            .iter()
            .any(|r| x < r[0] + r[2] && x + SIZE.0 > r[0] && y - SIZE.1 < r[1] + r[3] && y > r[1])
    };
    let mut y = h - 15.0;
    while y - SIZE.1 >= 15.0 {
        let mut x = 30.0;
        while x + SIZE.0 <= w - 10.0 {
            if !hits(x, y) {
                return (x, y);
            }
            x += 40.0;
        }
        y -= 40.0;
    }
    (30.0, h - 15.0)
}

fn cmd_place_sheet_list(app: &mut EditorApp) {
    let Some(sheet) = active_or_new(app) else {
        return;
    };
    let ru = app.sh.cat.lang == Lang::Ru;
    let ed = &mut app.ed;
    let paper = paper_of(ed, sheet);
    let placed: Vec<[f64; 4]> = with(|st| composed(st, ed, ru, sheet))
        .map(|c| c.placed.iter().map(|p| p.rect).collect())
        .unwrap_or_default();
    let at = free_spot(paper.0, paper.1, &placed);
    let item = Payload::Schedule(io::SHEET_LIST.to_string());
    if let Err(e) = place(ed, sheet, &item, at, 0.0) {
        app.sh.toasts.push(Level::Error, e, None);
    }
}

fn cmd_place_legend(app: &mut EditorApp) {
    if active_or_new(app).is_none() {
        return;
    }
    let ru = app.sh.cat.lang == Lang::Ru;
    let pick = place_items(&app.ed, ru)
        .iter()
        .position(|i| matches!(&i.0, Payload::Schedule(k) if io::LEGENDS.contains(&k.as_str())))
        .unwrap_or(0);
    with(|s| s.place = Some(Place { pick, scale: 0.0 }));
}

fn cmd_issue(app: &mut EditorApp) {
    if app.ed.model.sheets().map(|v| v.is_empty()).unwrap_or(true) {
        let ru = app.sh.cat.lang == Lang::Ru;
        let m = tr(ru, "shv.the_project_has_no_sheets");
        app.sh.toasts.push(Level::Warning, m, None);
        return;
    }
    let i = issue::Issue::new(&app.ed, active());
    with(|s| s.issue = Some(i));
}

/// The copy opens; the message says revisions are not copied.
fn duplicate_active(app: &mut EditorApp, with_views: bool) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let Some(src) = active() else {
        let m = msg(ru, "sheets.dup.none", &[]);
        app.sh.toasts.push(Level::Warning, m, None);
        return;
    };
    match dup_op(&mut app.ed, ru, src, with_views) {
        Ok(n) => {
            with(|s| open_sheet(s, n));
            let m = dup_message(&app.ed, ru, src, n);
            app.sh.toasts.push(Level::Success, m, None);
        }
        Err(e) => app.sh.toasts.push(Level::Error, e, None),
    }
}

fn dup_message(ed: &Editor, ru: bool, src: i64, new: i64) -> String {
    let m = &ed.model;
    let views = m.viewports(new).map_or(0, |v| v.len());
    let sched = m.schedule_places(new).map_or(0, |v| v.len());
    let legends: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM anno_legend_place WHERE sheet=?1",
            [new],
            |r| r.get(0),
        )
        .unwrap_or(0);
    msg(
        ru,
        "sheets.dup.done",
        &[
            ("number", &m.sheet(new).map(|s| s.number).unwrap_or_default()),
            ("src", &m.sheet(src).map(|s| s.number).unwrap_or_default()),
            ("views", &views.to_string()),
            ("sched", &(sched as i64 + legends).to_string()),
        ],
    )
}

fn cmd_duplicate(app: &mut EditorApp) {
    duplicate_active(app, true);
}

fn cmd_duplicate_title(app: &mut EditorApp) {
    duplicate_active(app, false);
}

fn cmd_sets(app: &mut EditorApp) {
    let s = Sets::new(&app.ed, app.sh.cat.lang == Lang::Ru);
    with(|st| st.sets = Some(s));
}

fn cmd_print(app: &mut EditorApp) {
    if app.ed.model.sheets().map(|v| v.is_empty()).unwrap_or(true) {
        let ru = app.sh.cat.lang == Lang::Ru;
        let m = tr(ru, "shv.the_project_has_no_sheets");
        app.sh.toasts.push(Level::Warning, m, None);
        return;
    }
    with(|s| s.print = Some(Print::new(s.active)));
}

pub fn register(app: &mut EditorApp) {
    app.register_command("view.sheet", cmd_new);
    app.register_command("view.place", cmd_place);
    app.register_command("view.sheet_set", cmd_sets);
    app.register_command("view.sheet_duplicate", cmd_duplicate);
    app.register_command("view.sheet_duplicate_title", cmd_duplicate_title);
    app.register_command("view.sheets_for_levels", sheets_for_levels);
    app.register_command("view.drawing_set", dset::cmd_drawing_set);
    app.register_command("view.print_sheets", cmd_print);
    app.register_command("data.export_pdf", cmd_print);
    app.register_command("view.issue_revision", cmd_issue);
    app.register_command("view.place_sheet_list", cmd_place_sheet_list);
    app.register_command("view.place_legend", cmd_place_legend);
}

/// Sheet windows and deferred navigation; called every frame.
pub fn windows(ctx: &egui::Context, app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let (opened, open_view) = with(|s| (std::mem::take(&mut s.opened), s.open_view.take()));
    if opened {
        app.view3d = false;
        app.section = None;
        app.ed.selection.clear();
    }
    match open_view {
        Some(ViewRef::Plan { level, .. }) => {
            deactivate();
            app.ed.set_level(level);
            app.view3d = false;
            app.section = None;
        }
        Some(ViewRef::View(id)) => {
            deactivate();
            app.open_view(id);
        }
        None => {}
    }
    if let Some(mut p) = with(|s| s.props.take()) {
        match props_window(ctx, &mut app.ed, ru, &mut p) {
            None => with(|s| s.props = Some(p)),
            Some(Some(id)) => with(|s| open_sheet(s, id)),
            Some(None) => {}
        }
    }
    delete_window(ctx, &mut app.ed, ru);
    if let Some(mut p) = with(|s| s.place.take()) {
        if let Some(sheet) = active() {
            if place_window(ctx, &mut app.ed, ru, sheet, &mut p) {
                with(|s| s.place = Some(p));
            }
        }
    }
    if let Some(mut s) = with(|s| s.sets.take()) {
        if sets_window(ctx, &mut app.ed, ru, &mut s) {
            with(|st| st.sets = Some(s));
        }
    }
    if let Some(mut p) = with(|s| s.print.take()) {
        if print_window(ctx, app, ru, &mut p) {
            with(|s| s.print = Some(p));
        }
    }
    if let Some(mut e) = with(|s| s.stamp.take()) {
        if issue::stamp_window(ctx, &mut app.ed, ru, &mut e) {
            with(|s| s.stamp = Some(e));
        }
    }
    if let Some(mut i) = with(|s| s.issue.take()) {
        if issue::issue_window(ctx, app, ru, &mut i) {
            with(|s| s.issue = Some(i));
        }
    }
    if let Some(mut d) = with(|s| s.dset.take()) {
        if dset::dset_window(ctx, app, ru, &mut d) {
            with(|s| s.dset = Some(d));
        }
    }
    if with(|s| std::mem::take(&mut s.open_project)) && !app.run("mgr.project_info") {
        let m = msg(ru, "sheets.project.missing", &[]);
        app.sh.toasts.push(Level::Warning, m, None);
    }
}

/// Screenshot scenarios `--sheet-demo canvas|active|print|props|sets`: a sheet with a plan, a section,
/// a door schedule and a revision; the second sheet is a 1:200 plan.
/// Scene of `--sheet-demo [canvas|active|print|props|sets]`; a bare flag (or one followed by
/// another flag) is the sheet canvas, not a silently empty plan.
pub fn demo_arg(argv: &[String]) -> Option<&str> {
    let i = argv.iter().position(|s| s == "--sheet-demo")?;
    Some(
        argv.get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .map_or("canvas", |s| s.as_str()),
    )
}

pub fn demo(app: &mut EditorApp, what: &str) -> Result<(), String> {
    let names = crate::create_names::SheetDemoNames::new(&app.sh.cat);
    let (a, set_id, first_vp) = build_demo(&mut app.ed, &names)?;
    with(|st| {
        open_sheet(st, a);
        st.sel = first_vp.map(PlacedKind::Viewport).into_iter().collect();
        match what {
            "active" => st.active_vp = first_vp,
            "print" => {
                let mut p = Print::new(Some(a));
                p.range = 3;
                p.set = set_id;
                st.print = Some(p);
            }
            "props" => {}
            "sets" => {}
            _ => {}
        }
    });
    match what {
        "props" => {
            let d = app.ed.model.sheet(a).map_err(|e| e.to_string())?;
            let p = Props::load(&app.ed, d, Some(a));
            with(|st| st.props = Some(p));
        }
        "sets" => {
            let s = Sets::new(&app.ed, app.sh.cat.lang == Lang::Ru);
            with(|st| st.sets = Some(s));
        }
        _ => {}
    }
    Ok(())
}

/// Builds demo sheets with names from `names`; returns the first sheet, the set and the first view.
pub(crate) fn build_demo(
    ed: &mut Editor,
    names: &crate::create_names::SheetDemoNames,
) -> Result<(i64, Option<i64>, Option<i64>), String> {
    let lv = ed
        .levels()
        .first()
        .map(|l| l.0)
        .ok_or_else(|| names.no_levels.clone())?;
    let mut s1 = SheetDef::new("A-101", &names.plan_section);
    s1.paper = "A2".into();
    for (k, v) in names.title_fields() {
        s1.fields.insert(k.into(), v.into());
    }
    let d1 = s1.clone();
    op(ed, move |m| m.add_sheet(&d1).map(|_| ()))?;
    let a = ed
        .model
        .sheets()
        .map_err(|e| e.to_string())?
        .last()
        .map(|s| s.0)
        .ok_or_else(|| names.no_sheet.clone())?;
    let paper = paper_of(ed, a);
    let plan = ViewRef::Plan {
        level: lv,
        cut: DEFAULT_CUT,
    };
    let scale = auto_scale(ed, plan, (paper.0 * 0.6, paper.1));
    place(
        ed,
        a,
        &Payload::View(plan, names.plan_view.clone()),
        (paper.0 * 0.3, paper.1 * 0.6),
        scale,
    )?;
    if let Some((sec, spec)) = ed.views.first().cloned() {
        let name = crate::editor::sec::display_name(&spec);
        place(
            ed,
            a,
            &Payload::View(ViewRef::View(sec), name),
            (paper.0 * 0.72, paper.1 * 0.68),
            scale,
        )?;
    }
    place(
        ed,
        a,
        &Payload::Schedule("doors".into()),
        (paper.0 * 0.58, paper.1 * 0.42),
        0.0,
    )?;
    let revs = vec![Revision {
        mark: "1".into(),
        date: names.revision_date.clone(),
        description: names.revision_description.clone(),
        author: names.revision_author.clone(),
    }];
    op(ed, move |m| m.set_revisions(a, &revs))?;
    let mut s2 = SheetDef::new("A-102", &names.site_plan);
    s2.standard = "gost".into();
    op(ed, move |m| m.add_sheet(&s2).map(|_| ()))?;
    let b = ed
        .model
        .sheets()
        .map_err(|e| e.to_string())?
        .last()
        .map(|s| s.0)
        .ok_or_else(|| names.no_sheet.clone())?;
    let pb = paper_of(ed, b);
    place(
        ed,
        b,
        &Payload::View(plan, names.site_plan.clone()),
        free_center(pb.0, pb.1),
        200.0,
    )?;
    let all = ed
        .model
        .sheets()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|s| s.0)
        .collect::<Vec<_>>();
    let set_name = names.set_name.clone();
    op(ed, move |m| m.add_sheet_set(&set_name, &all).map(|_| ()))?;
    ed.status = None;
    let set_id = ed
        .model
        .sheet_sets()
        .ok()
        .and_then(|v| v.first().map(|x| x.0));
    let first_vp = ed
        .model
        .viewports(a)
        .ok()
        .and_then(|v| v.first().map(|x| x.0));
    Ok((a, set_id, first_vp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(kind: PlacedKind, rect: [f64; 4]) -> io::Placed {
        io::Placed {
            kind,
            rect,
            model_c: [0.0, 0.0],
            scale: 100.0,
        }
    }

    #[test]
    fn snap_aligns_edges_and_centres() {
        let a = PlacedKind::Viewport(1);
        let ps = [
            placed(a, [10.0, 10.0, 50.0, 40.0]),
            placed(PlacedKind::Viewport(2), [100.0, 100.0, 60.0, 30.0]),
        ];
        // left edge to the left edge of the second viewport, 2 mm within tolerance
        let (d, g) = snap(&ps, &[a], [88.0, 0.4], [420.0, 297.0], 3.0);
        assert!((d[0] - 90.0).abs() < 1e-9, "{d:?}");
        assert!(g.iter().any(|x| x.0 && (x.1 - 100.0).abs() < 1e-9));
        // outside tolerance: no snapping ((70, 70) aligned the right edge with the other centre)
        let (d, g) = snap(&ps, &[a], [73.0, 67.0], [420.0, 297.0], 1.0);
        assert_eq!(d, [73.0, 67.0]);
        assert!(g.is_empty());
    }

    #[test]
    fn print_range_follows_choice() {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let b = ed.model.add_sheet(&SheetDef::new("A-102", "b")).unwrap();
        let s = ed.model.add_sheet_set("S", &[b]).unwrap();
        let mut p = Print::new(None);
        assert_eq!(p.range, 1);
        assert_eq!(p.ids(&ed), vec![a, b]);
        p.range = 2;
        p.chosen = vec![b, a];
        assert_eq!(p.ids(&ed), vec![a, b]);
        p.range = 3;
        p.set = Some(s);
        assert_eq!(p.ids(&ed), vec![b]);
    }

    #[test]
    fn place_items_offer_sheet_lists_and_legends() {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let s = ed.model.add_sheet_set("Issue", &[a]).unwrap();
        let items = place_items(&ed, true);
        let has = |key: &str| {
            items
                .iter()
                .any(|i| matches!(&i.0, Payload::Schedule(k) if k == key))
        };
        assert!(has(io::SHEET_LIST));
        assert!(has(&format!("sheet_list:{s}")));
        assert!(io::LEGENDS.iter().all(|k| has(k)));
        // labels contain no raw keys or missing catalog strings
        assert!(!items
            .iter()
            .any(|i| i.1.contains("legend:") || i.1.contains("sheets.item")));
        assert_eq!(
            item_title(&ed, &format!("sheet_list:{s}"), true),
            format!("{}Issue", crate::app2_fx::fx("sheets_list_prefix"))
        );
        assert_eq!(item_title(&ed, "unknown", true), "unknown");
    }

    #[test]
    fn schedule_on_sheet_follows_the_model() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.model.add_level("L1", 0.0).unwrap();
        // the schedule lists the walls of the level on screen
        ed.level = lv;
        let wt = ed
            .model
            .add_wall_type(
                "W",
                &[ddc_model::LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        ed.reload().unwrap();
        let s = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        place(
            &mut ed,
            s,
            &Payload::Schedule("walls".into()),
            (40.0, 250.0),
            0.0,
        )
        .unwrap();
        let rows = |ed: &Editor| {
            with_ctx(ed, true, None, None, |c| io::compose_sheet(&ed.model, s, c))
                .unwrap()
                .drawing
                .marks
                .iter()
                .filter(|(_, m)| matches!(m, Mark::Text { .. }))
                .count()
        };
        let before = rows(&ed);
        ed.model
            .add_walls(&[ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            }])
            .unwrap();
        ed.reload().unwrap();
        assert!(rows(&ed) > before, "new wall must reach the sheet");
    }

    #[test]
    fn free_spot_avoids_what_is_placed() {
        assert_eq!(free_spot(420.0, 297.0, &[]), (30.0, 282.0));
        let (x, y) = free_spot(420.0, 297.0, &[[20.0, 200.0, 150.0, 90.0]]);
        let clear = x >= 170.0 || y - 60.0 >= 290.0 || y <= 200.0;
        assert!(clear, "{x} {y}");
        // the sheet is fully occupied: there is still somewhere to place it
        assert_eq!(
            free_spot(420.0, 297.0, &[[0.0, 0.0, 420.0, 297.0]]),
            (30.0, 282.0)
        );
    }

    #[test]
    fn printing_without_a_format_is_refused() {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let dir = std::env::temp_dir().join(format!("ddc_sheets_nofmt_{}", std::process::id()));
        let mut p = Print::new(None);
        p.pdf = false;
        assert!(print_to(&ed, true, &p, &[a], &dir).is_err());
    }

    #[test]
    fn printing_writes_pdf_per_sheet() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.model.add_level("L1", 0.0).unwrap();
        ed.reload().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
        let b = ed
            .model
            .add_sheet(&SheetDef::new("A-102", "Schedules"))
            .unwrap();
        let plan = ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        };
        place(
            &mut ed,
            a,
            &Payload::View(plan, "Plan".into()),
            (200.0, 150.0),
            100.0,
        )
        .unwrap();
        place(
            &mut ed,
            b,
            &Payload::Schedule("walls".into()),
            (40.0, 250.0),
            0.0,
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("ddc_sheets_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = Print::new(None);
        p.separate = true;
        let msg = print_to(&ed, true, &p, &[a, b], &dir).unwrap();
        assert!(msg.contains(&crate::app2_fx::fx("print_files_2")), "{msg}");
        for f in ["A-101 - Plan.pdf", "A-102 - Schedules.pdf"] {
            assert!(dir.join(f).is_file(), "{f}");
        }
        p.separate = false;
        let one = dir.join("all.pdf");
        print_to(&ed, true, &p, &[a, b], &one).unwrap();
        let bytes = std::fs::read(&one).unwrap();
        let n = ddc_io::pdf::import::PdfFile::parse(&bytes)
            .unwrap()
            .page_count();
        assert_eq!(n, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn taken_number_disables_ok_with_localized_reason_ru_en() {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
        let en = props::number_block(&ed, false, None, " A-101 ").expect("taken blocks OK");
        let ru = props::number_block(&ed, true, None, "A-101").expect("taken blocks OK");
        assert!(en.contains("A-101 — Plan"), "{en}");
        assert!(ru.contains("A-101 — Plan"), "{ru}");
        assert_ne!(en, ru);
        assert!(!en.contains("sheet number"), "{en}");
        // the sheet's own number and a free number do not block
        assert!(props::number_block(&ed, false, Some(a), "A-101").is_none());
        assert!(props::number_block(&ed, true, None, "A-102").is_none());
        // empty number blocks with its own reason
        assert!(props::number_block(&ed, false, None, "  ").is_some());
    }

    #[test]
    fn renumber_conflict_message_has_no_row_id() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.model.add_sheet(&SheetDef::new("Qa", "Plan")).unwrap();
        let b = ed.model.add_sheet(&SheetDef::new("Qb", "Section")).unwrap();
        ed.model.add_sheet(&SheetDef::new("Qc", "Detail")).unwrap();
        let e = ed.model.renumber_sheets(&[b], "Qc").unwrap_err();
        assert!(matches!(&e, ddc_model::Error::Sheet(s) if s.kind() == ddc_model::SheetKind::Conflict));
        for ru in [false, true] {
            let t = err_text(ru, &e);
            assert!(t.contains("Qc") && t.contains("Detail"), "{t}");
            assert!(!t.chars().any(|c| c.is_ascii_digit()), "row id leaked: {t}");
        }
        assert!(!e.to_string().chars().any(|c| c.is_ascii_digit()), "{e}");
    }

    #[test]
    fn sheets_err_keys_in_en_and_ru() {
        use ddc_model::SheetErr as E;
        let all = [
            E::NumberEmpty,
            E::NumberTaken { number: "N1".into(), name: "Nm".into() },
            E::UnknownPaper("P9".into()),
            E::UnknownStandard("S9".into()),
            E::NoSuch("sheet"),
            E::NoSuch("sheet set"),
            E::NoSuch("viewport"),
            E::NoSuch("schedule placement"),
            E::NoSuch("level"),
            E::NoSuch("view"),
            E::BadScale(0.0),
            E::BadCut,
            E::BadPosition,
            E::BadCrop,
            E::NoSingleView,
        ];
        for e in all {
            let err = ddc_model::Error::Sheet(e.clone());
            let (en, ru) = (err_text(false, &err), err_text(true, &err));
            for t in [&en, &ru] {
                assert!(!t.starts_with("sheets.err"), "missing key for {e:?}: {t}");
                assert!(!t.contains('{'), "unfilled placeholder: {t}");
            }
            assert_ne!(en, ru, "{e:?}");
            if let Some(k) = e.what_key() {
                for r in [false, true] {
                    assert_ne!(msg(r, k, &[]), k);
                }
            }
        }
    }

    #[test]
    fn viewport_scale_refusal_localized() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.model.add_level("L1", 0.0).unwrap();
        let s = ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
        let mut v = ViewportDef {
            sheet: s,
            view: ViewRef::Plan { level: lv, cut: DEFAULT_CUT },
            scale: 100.0,
            center: (100.0, 100.0),
            crop: None,
            title: "t".into(),
        };
        let id = ed.model.add_viewport(&v).unwrap();
        v.scale = 0.0;
        let en = op_in(&mut ed, false, move |m| m.update_viewport(id, &v)).unwrap_err();
        assert!(en.contains("1:0") && !en.starts_with("invalid"), "{en}");
        let mut v2 = ed.model.viewport(id).unwrap();
        v2.scale = -5.0;
        let ru = op_in(&mut ed, true, move |m| m.update_viewport(id, &v2)).unwrap_err();
        assert!(ru.contains("1:-5") && !ru.contains("bad scale"), "{ru}");
        assert_ne!(en, ru);
    }

    /// A sheet with two views and one placed schedule.
    fn loaded() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.model.add_level("L1", 0.0).unwrap();
        let s = ed.model.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
        for t in ["a", "b"] {
            let v = ViewportDef {
                sheet: s,
                view: ViewRef::Plan { level: lv, cut: DEFAULT_CUT },
                scale: 100.0,
                center: (100.0, 100.0),
                crop: None,
                title: t.into(),
            };
            ed.model.add_viewport(&v).unwrap();
        }
        let p = ddc_model::SchedulePlace {
            sheet: s,
            schedule: "door".into(),
            pos: (10.0, 10.0),
        };
        ed.model.add_schedule_place(&p).unwrap();
        (ed, s)
    }

    #[test]
    fn sheet_delete_asks_confirmation_with_counts() {
        let (ed, s) = loaded();
        let mut st = Sheets::default();
        ask_delete(&ed, &mut st, s);
        let a = st.del_ask.clone().expect("a card is pending");
        assert_eq!((a.viewports, a.schedules), (2, 1));
        assert_eq!(delete_lines(false, &a).len(), 2);
        assert!(delete_lines(false, &a)[0].contains('2'));
        // nothing is deleted before the answer
        assert!(ed.model.sheet(s).is_ok());
        assert_eq!(ed.model.viewports(s).unwrap().len(), 2);
    }

    #[test]
    fn sheet_delete_reports_status_with_undo_hint() {
        let (mut ed, s) = loaded();
        let mut st = Sheets::default();
        st.active = Some(s);
        ask_delete(&ed, &mut st, s);
        assert!(confirm_delete(&mut ed, &mut st, false));
        let t = ed.status.clone().expect("status is set");
        assert!(t.contains("Ctrl+Z") && t.contains("A-101"), "{t}");
        assert!(ed.undo_offer.is_some() && ed.undo_offer == ed.top_entry());
        assert!(st.active.is_none() && st.del_ask.is_none());
        assert!(ed.model.sheet(s).is_err());
    }

    #[test]
    fn sheet_delete_undo_restores_viewports() {
        let (mut ed, s) = loaded();
        let mut st = Sheets::default();
        ask_delete(&ed, &mut st, s);
        assert!(confirm_delete(&mut ed, &mut st, false));
        ed.undo();
        assert!(ed.model.sheet(s).is_ok(), "one undo brings the sheet back");
        assert_eq!(ed.model.viewports(s).unwrap().len(), 2);
        assert_eq!(ed.model.schedule_places(s).unwrap().len(), 1);
    }

    #[test]
    fn sheet_delete_cancel_keeps_sheet() {
        let (mut ed, s) = loaded();
        let mut st = Sheets::default();
        ask_delete(&ed, &mut st, s);
        cancel_delete(&mut st);
        assert!(st.del_ask.is_none());
        assert!(!confirm_delete(&mut ed, &mut st, false), "no pending card");
        assert!(ed.model.sheet(s).is_ok());
        assert_eq!(ed.model.viewports(s).unwrap().len(), 2);
    }

    const ALL_ITEMS: [Item; 14] = [
        Item::Banner,
        Item::ResetCrop,
        Item::Done,
        Item::Scale,
        Item::Title,
        Item::Detail,
        Item::Activate,
        Item::OpenView,
        Item::Split,
        Item::Divider,
        Item::PlaceView,
        Item::Properties,
        Item::Print,
        Item::Fit,
    ];

    #[test]
    fn bar_without_selection_is_the_four_sheet_buttons() {
        assert_eq!(
            sheet_bar(false, false, false),
            vec![Item::PlaceView, Item::Properties, Item::Print, Item::Fit]
        );
    }

    #[test]
    fn done_only_in_active_mode_and_it_replaces_the_rest() {
        for (vp, sch) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(!sheet_bar(vp, false, sch).contains(&Item::Done));
        }
        let b = sheet_bar(true, true, true);
        assert_eq!(b, vec![Item::Banner, Item::ResetCrop, Item::Done]);
        assert!(b.iter().all(|i| !i.right()), "nothing right-aligned while editing");
    }

    #[test]
    fn viewport_group_only_with_a_viewport() {
        let group = [Item::Scale, Item::Title, Item::Detail, Item::Activate, Item::OpenView];
        let with = sheet_bar(true, false, false);
        let without = sheet_bar(false, false, true);
        for g in group {
            assert!(with.contains(&g), "{g:?}");
            assert!(!without.contains(&g), "{g:?}");
        }
        // the group is set apart from the sheet buttons by one divider
        let pos = with.iter().position(|i| *i == Item::OpenView).unwrap();
        assert_eq!(with[pos + 1], Item::Divider);
        assert_eq!(with.iter().filter(|i| **i == Item::Divider).count(), 1);
    }

    #[test]
    fn split_only_with_a_schedule() {
        assert!(sheet_bar(false, false, true).contains(&Item::Split));
        assert!(!sheet_bar(true, false, false).contains(&Item::Split));
        assert!(!sheet_bar(false, true, true).contains(&Item::Split));
    }

    #[test]
    fn print_button_names_what_it_saves() {
        assert_eq!(msg_n(false, "sheet_tools.print.save", 1), "Save 1 sheet");
        assert_eq!(msg_n(false, "sheet_tools.print.save", 2), "Save 2 sheets");
        let ru = |n| msg_n(true, "sheet_tools.print.save", n);
        let save = "\u{421}\u{43e}\u{445}\u{440}\u{430}\u{43d}\u{438}\u{442}\u{44c}";
        assert_eq!(ru(1), format!("{save} 1 \u{43b}\u{438}\u{441}\u{442}"));
        assert!(ru(2).ends_with("2 \u{43b}\u{438}\u{441}\u{442}\u{430}"), "{}", ru(2));
        assert!(ru(5).ends_with("5 \u{43b}\u{438}\u{441}\u{442}\u{43e}\u{432}"), "{}", ru(5));
        let src = include_str!("sheet_view/print.rs");
        assert!(src.contains("msg_n(ru, \"sheet_tools.print.save\""), "footer uses the counted name");
        assert!(!src.contains("tr(ru, \"shv.print\")"), "the bare Print verb left the footer");
    }

    #[test]
    fn narrow_sheet_bar_sends_open_view_to_the_overflow_first() {
        let items: Vec<(Item, f32)> = [
            (Item::Scale, 80.0),
            (Item::Title, 160.0),
            (Item::Detail, 50.0),
            (Item::Activate, 110.0),
            (Item::OpenView, 120.0),
            (Item::Divider, 10.0),
        ]
        .to_vec();
        let all: Vec<Item> = items.iter().map(|i| i.0).collect();
        assert_eq!(bar_keep(1000.0, 96.0, &items, 30.0), all);
        let k = bar_keep(600.0, 96.0, &items, 30.0);
        assert!(!k.contains(&Item::OpenView) && k.contains(&Item::Activate), "{k:?}");
        let k = bar_keep(300.0, 96.0, &items, 30.0);
        assert_eq!(k, [Item::Scale, Item::Divider], "{k:?}");
        assert!(bar_keep(50.0, 96.0, &items, 30.0).is_empty(), "a lone divider goes too");
    }

    #[test]
    fn sheet_buttons_are_right_aligned_icon_buttons() {
        for i in [Item::PlaceView, Item::Properties, Item::Print, Item::Fit] {
            assert!(i.right() && i.icon().is_some(), "{i:?}");
        }
        for i in [Item::Scale, Item::Title, Item::Activate, Item::Done, Item::Banner] {
            assert!(!i.right(), "{i:?}");
        }
    }

    #[test]
    fn every_icon_exists() {
        for i in ALL_ITEMS {
            if let Some(n) = i.icon() {
                assert!(ddc_ui::icons::source(n).is_some(), "{i:?}: icon {n}");
            }
        }
    }

    #[test]
    fn controls_have_tooltips_in_both_languages() {
        let c = crate::catalog();
        for i in ALL_ITEMS {
            let needs = !matches!(i, Item::Banner | Item::Divider);
            assert_eq!(i.tip_key().is_some(), needs, "{i:?}");
            let Some(k) = i.tip_key() else { continue };
            for lang in [Lang::En, Lang::Ru] {
                let t = c.table(lang).get(k).and_then(|v| v.as_str()).unwrap_or("");
                assert!(!t.trim().is_empty(), "{i:?}: {k} missing in {lang:?}");
            }
        }
        for k in ["sheet_place.banner", "sheet_place.new_set_tip", "sheet_place.new_sheet_tip"] {
            for lang in [Lang::En, Lang::Ru] {
                assert!(c.table(lang).get(k).is_some(), "{k} missing in {lang:?}");
            }
        }
    }

    #[test]
    fn done_tooltip_shows_esc_and_others_none() {
        assert_eq!(Item::Done.keys(), "Esc");
        for i in ALL_ITEMS.into_iter().filter(|i| *i != Item::Done) {
            assert_eq!(i.keys(), "", "{i:?}");
        }
    }

    #[test]
    fn scale_choices_keep_a_custom_scale_and_fit() {
        let std = scale_choices(100.0, false);
        assert_eq!(std, SCALES.to_vec());
        let custom = scale_choices(333.0, false);
        assert_eq!(custom[0], 333.0);
        assert_eq!(custom.len(), SCALES.len() + 1);
        let fit = scale_choices(0.0, true);
        assert_eq!(fit[0], 0.0);
        assert_eq!(fit.len(), SCALES.len() + 1);
        assert_eq!(scale_label(0.0, false), tr(false, "shv.fit_to_sheet"));
        assert_eq!(scale_label(50.0, true), "1:50");
    }

    #[test]
    fn only_delete_is_destructive_in_the_sheet_menu() {
        for k in 0..=6u8 {
            assert_eq!(sheet_menu_tone(k) == MenuTone::Danger, k == 4, "{k}");
        }
    }

    #[test]
    fn this_file_has_no_raw_egui_widgets() {
        let src = include_str!("sheet_view.rs");
        let prod = &src[..src.find("#[cfg(test)]").unwrap()];
        for pat in [
            "ui.button(",
            ".small_button(",
            "egui::Button",
            "ComboBox",
            "selectable_label",
            "selectable_value",
            "TextEdit",
            "text_edit_singleline",
            "DragValue",
            "ui.checkbox(",
            ".separator()",
        ] {
            assert!(!prod.contains(pat), "raw widget {pat} in sheet_view.rs");
        }
    }
}
