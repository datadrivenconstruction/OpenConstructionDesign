// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! After an import: one calm strip under the ribbon (counts, Review, Make editable, close) and a
//! docked review panel. The strip folds into the status line after [`FOLD_AFTER`]; a failure
//! uses the same strip in the warning colour and stays until closed. Never a modal.
use crate::editor::bodies::BodyKind;
use crate::editor::Sel;
use crate::shell::EditorApp;
use crate::tr;
use ddc_io::import_summary::ImportSummary;
use ddc_model::Model;
use ddc_ui::theme::{self, space, Role};
use ddc_ui::widgets::{self, shell_kit, Button};
use egui::{self, Align, Layout, Sense, Vec2};
use web_time::{Duration, Instant};

const FOLD_AFTER: Duration = Duration::from_secs(10);
const STRIP_H: f32 = 32.0;
const MAKE_W: f32 = 132.0;
const REVIEW_W: f32 = 84.0;
const CLOSE_W: f32 = 28.0;
const PANEL_W: f32 = 340.0;
const ROW_H: f32 = 24.0;
/// Elements selected by one review row; more is a count, not a selection.
const SELECT_MAX: usize = 20_000;

/// Native categories counted from the model: (key of `open_summary.cat.<key>`, table).
const NATIVE: [(&str, &str); 12] = [
    ("wall", "wall"),
    ("opening", "opening"),
    ("slab", "slab"),
    ("curtain_wall", "curtain_wall"),
    ("room", "room"),
    ("grid", "grid"),
    ("col", "col"),
    ("beam", "beam"),
    ("roof", "roof"),
    ("ceiling", "ceiling"),
    ("mep_run", "mep_run"),
    ("comp", "comp"),
];

pub enum Kind {
    Done(ImportSummary),
    Failed { file: String, message: String },
}

pub struct Strip {
    pub kind: Kind,
    since: Instant,
}

#[derive(Default)]
pub struct State {
    pub strip: Option<Strip>,
    pub review: Option<ImportSummary>,
    /// The review panel is open.
    pub panel: bool,
}

// ---- building the summary ----------------------------------------------------------------

fn has_table(db: &rusqlite::Connection, t: &str) -> bool {
    db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |r| r.get::<_, i64>(0),
    )
    .map_or(false, |n| n > 0)
}

/// Counts what the model holds: native elements by table, meshes by category.
pub fn from_model(m: &Model, file: &str) -> ImportSummary {
    let db = m.sql();
    let mut native = vec![];
    for (key, table) in NATIVE {
        if !has_table(db, table) {
            continue;
        }
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or(0);
        if n > 0 {
            native.push((key.to_string(), n as usize));
        }
    }
    let mesh = m
        .mesh_categories()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.1 > 0)
        .map(|c| (c.0, c.1 as usize))
        .collect();
    ImportSummary {
        file: file.to_string(),
        native,
        mesh,
        ..Default::default()
    }
}

/// On the build worker, before the model is cached: counts the model, adds what the reader
/// skipped and noted, and stores the report in the project's source rows.
pub fn record(
    m: &mut Model,
    file: &std::path::Path,
    skipped: impl IntoIterator<Item = (String, usize)>,
    notes: &[String],
    notes_total: usize,
) {
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut s = from_model(m, &name);
    s.set_skipped(skipped);
    s.set_notes(notes, notes_total);
    let json = s.to_json();
    for f in m.source_files().unwrap_or_default() {
        let _ = m.set_source_report(&f.source, &json);
    }
}

/// The report stored in the project, newest import first.
pub fn stored(m: &Model) -> Option<ImportSummary> {
    m.source_reports()
        .ok()?
        .into_iter()
        .find_map(|r| ImportSummary::from_json(&r.1))
}

// ---- entry points ------------------------------------------------------------------------

fn put(a: &mut EditorApp, kind: Kind) {
    if let Kind::Done(s) = &kind {
        a.open.sum.review = Some(s.clone());
    }
    a.open.sum.strip = Some(Strip {
        kind,
        since: Instant::now(),
    });
}

/// The result strip put up at or after `t`: the summary of a done import, or the failure text.
pub fn strip_since(a: &EditorApp, t: Instant) -> Option<Result<ImportSummary, String>> {
    let s = a.open.sum.strip.as_ref().filter(|s| s.since >= t)?;
    Some(match &s.kind {
        Kind::Done(sum) => Ok(sum.clone()),
        Kind::Failed { message, .. } => Err(message.clone()),
    })
}

/// A built project was adopted: shows the report stored in it.
pub fn show_stored(a: &mut EditorApp) {
    if let Some(s) = stored(&a.ed.model) {
        put(a, Kind::Done(s));
    }
}

/// A module import into the current project finished.
pub fn show_bridge(a: &mut EditorApp, input: &std::path::Path, r: &ddc_io::bridge::Report) {
    let name = input
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    put(a, Kind::Done(ImportSummary::from_bridge(&name, r)));
}

/// An IFC import into the current project finished.
#[allow(dead_code)]
pub fn show_ifc(a: &mut EditorApp, input: &std::path::Path, r: &ddc_io::ifc::ImportReport) {
    let name = input
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    put(a, Kind::Done(ImportSummary::from_ifc(&name, r)));
}

pub fn show_failed(a: &mut EditorApp, input: &std::path::Path, message: String) {
    let file = input
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    a.open.sum.strip = Some(Strip {
        kind: Kind::Failed { file, message },
        since: Instant::now(),
    });
}

/// Command `file.import_review`: the panel of the last report.
pub fn h_review(a: &mut EditorApp) {
    if a.open.sum.review.is_none() {
        a.open.sum.review = stored(&a.ed.model);
    }
    if a.open.sum.review.is_none() {
        a.ed.status = Some(tr("open_summary.no_report", &[]));
    } else {
        a.open.sum.strip = None;
        a.open.sum.panel = true;
    }
}

// ---- text --------------------------------------------------------------------------------

/// 2006 -> "2,006" (English) or "2 006" with a no-break space (Russian).
pub fn grp(n: usize, ru: bool) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(if ru { '\u{a0}' } else { ',' });
        }
        out.push(c);
    }
    out
}

/// The strip line: the plain answer to "what came editable and what did not".
pub fn line(s: &ImportSummary, ru: bool) -> String {
    tr(
        "open_summary.strip",
        &[
            ("file", s.file.as_str()),
            ("native", grp(s.native_total(), ru).as_str()),
            ("mesh", grp(s.mesh_total(), ru).as_str()),
            ("skipped", grp(s.skipped_total(), ru).as_str()),
            ("notes", grp(s.notes_total, ru).as_str()),
        ],
    )
}

fn category_label(key: &str) -> String {
    let k = format!("open_summary.cat.{key}");
    let t = tr(&k, &[]);
    if t == k {
        key.to_string()
    } else {
        t
    }
}

// ---- selecting what a row names ----------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Pick {
    Native(String),
    Mesh(String),
}

fn sel_of(table: &str, id: i64) -> Option<Sel> {
    Some(match table {
        "wall" => Sel::Wall(id),
        "opening" => Sel::Opening(id),
        "slab" => Sel::Slab(id),
        "curtain_wall" => Sel::Curtain(id),
        "room" => Sel::Room(id),
        "col" => Sel::Body(BodyKind::Column, id),
        "beam" => Sel::Body(BodyKind::Beam, id),
        "ceiling" => Sel::Body(BodyKind::Ceiling, id),
        "mep_run" => Sel::Mep(id),
        "comp" => Sel::Comp(id),
        "mesh" => Sel::Body(BodyKind::Mesh, id),
        _ => return None,
    })
}

/// (level, id) of the elements of a row; a table without a level column gives level None.
fn rows_of(m: &Model, pick: &Pick) -> Vec<(Option<i64>, i64)> {
    let db = m.sql();
    let (table, cat) = match pick {
        Pick::Native(k) => match NATIVE.iter().find(|n| n.0 == k) {
            Some(n) => (n.1, None),
            None => return vec![],
        },
        Pick::Mesh(c) => ("mesh", Some(c.as_str())),
    };
    let filter = if cat.is_some() {
        " WHERE category=?1"
    } else {
        ""
    };
    let run = |cols: &str| -> rusqlite::Result<Vec<(Option<i64>, i64)>> {
        let mut st = db.prepare(&format!(
            "SELECT {cols} FROM {table}{filter} ORDER BY id LIMIT {SELECT_MAX}"
        ))?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<(Option<i64>, i64)> {
            if cols == "id" {
                Ok((None, r.get(0)?))
            } else {
                Ok((r.get(0)?, r.get(1)?))
            }
        };
        let rows = match cat {
            Some(c) => st.query_map([c], map)?.collect::<Result<Vec<_>, _>>()?,
            None => st.query_map([], map)?.collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    };
    run("level, id").or_else(|_| run("id")).unwrap_or_default()
}

/// The level holding most of `rows`, and the ids on it (all ids when there are no levels).
pub fn on_busiest_level(rows: &[(Option<i64>, i64)]) -> (Option<i64>, Vec<i64>) {
    let mut count: Vec<(Option<i64>, usize)> = vec![];
    for r in rows {
        match count.iter_mut().find(|c| c.0 == r.0) {
            Some(c) => c.1 += 1,
            None => count.push((r.0, 1)),
        }
    }
    let best = count
        .iter()
        .fold(None::<(Option<i64>, usize)>, |b, &c| match b {
            Some(x) if x.1 >= c.1 => Some(x),
            _ => Some(c),
        });
    let Some((level, _)) = best else {
        return (None, vec![]);
    };
    (
        level,
        rows.iter().filter(|r| r.0 == level).map(|r| r.1).collect(),
    )
}

fn select(a: &mut EditorApp, pick: &Pick) {
    let table = match pick {
        Pick::Native(k) => NATIVE.iter().find(|n| n.0 == k).map(|n| n.1),
        Pick::Mesh(_) => Some("mesh"),
    };
    let Some(table) = table else { return };
    let (level, ids) = on_busiest_level(&rows_of(&a.ed.model, pick));
    if let Some(l) = level {
        a.ed.set_level(l);
    }
    let sel: Vec<Sel> = ids.into_iter().filter_map(|id| sel_of(table, id)).collect();
    if sel.is_empty() {
        return;
    }
    a.ed.selection = sel;
    frame_selection(a);
}

pub(crate) fn frame_selection(a: &mut EditorApp) {
    if a.view3d {
        a.run("view.zoom_sel_3d");
        return;
    }
    frame_plan(a);
}

/// Centres and scales the plan on the selection (walls and bodies); the view is not switched.
pub(crate) fn frame_plan(a: &mut EditorApp) -> bool {
    let mut pts = vec![];
    for s in &a.ed.selection {
        match s {
            Sel::Room(id) => pts.extend(
                a.ed.rooms
                    .iter()
                    .filter(|r| r.id == *id)
                    .flat_map(|r| r.rings.first().into_iter().flatten().copied()),
            ),
            Sel::Slab(id) => pts.extend(
                a.ed.slabs
                    .iter()
                    .filter(|s| s.id == *id)
                    .flat_map(|s| s.rings.first().into_iter().flatten().copied()),
            ),
            Sel::Curtain(id) | Sel::Panel(id, _) => pts.extend(
                a.ed.curtain_lines
                    .iter()
                    .filter(|l| l.0 == *id)
                    .flat_map(|l| [l.1, l.2]),
            ),
            Sel::Wall(id) => {
                pts.extend(
                    a.ed.walls
                        .iter()
                        .filter(|w| w.id == *id)
                        .flat_map(|w| [w.a, w.b]),
                );
            }
            Sel::Body(k, id) => pts.extend(
                a.ed.bodies
                    .iter()
                    .filter(|b| b.kind == *k && b.id == *id)
                    .flat_map(|b| b.hull.iter().copied()),
            ),
            _ => {}
        }
    }
    let Some((lo, hi)) = super::first_view::percentile_bounds(&pts) else {
        return false;
    };
    a.plan.center = (lo + hi) * 0.5;
    a.plan.scale = super::first_view::scale_for(lo, hi);
    true
}

// ---- drawing -----------------------------------------------------------------------------

enum Act {
    Review,
    Make,
    Close,
    ClosePanel,
    Pick(Pick),
}

/// Per frame, from `bigfile::frame`: remembers the view, folds the strip, draws strip and panel.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    super::first_view::track(a);
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let panel_open = a.open.sum.panel;
    // a finished import folds into the status line; a failure stays until closed
    let folded = match &a.open.sum.strip {
        Some(Strip {
            kind: Kind::Done(s),
            since,
        }) if !panel_open => {
            if since.elapsed() >= FOLD_AFTER {
                Some(line(s, ru))
            } else {
                ctx.request_repaint_after(FOLD_AFTER.saturating_sub(since.elapsed()));
                None
            }
        }
        _ => None,
    };
    if let Some(text) = folded {
        a.ed.status = Some(text);
        a.open.sum.strip = None;
    }
    let mut act = None;
    if let Some(st) = &a.open.sum.strip {
        act = strip(ctx, st, ru);
    }
    if panel_open {
        if let Some(s) = a.open.sum.review.clone() {
            act = act.or(panel(ctx, &s, ru));
        } else {
            a.open.sum.panel = false;
        }
    }
    match act {
        Some(Act::Review) => {
            a.open.sum.strip = None;
            a.open.sum.panel = true;
        }
        Some(Act::Make) => {
            a.run("data.mesh_convert");
        }
        Some(Act::Close) => a.open.sum.strip = None,
        Some(Act::ClosePanel) => a.open.sum.panel = false,
        Some(Act::Pick(p)) => select(a, &p),
        None => {}
    }
}

fn strip(ctx: &egui::Context, st: &Strip, ru: bool) -> Option<Act> {
    let th = theme::of(ctx);
    let sem = th.sem();
    let failed = matches!(st.kind, Kind::Failed { .. });
    let (fill, ink) = if failed {
        (sem.warning_subtle, sem.text_primary)
    } else {
        (sem.neutral_subtle, sem.text_primary)
    };
    let (text, mesh) = match &st.kind {
        Kind::Done(s) => (line(s, ru), s.mesh_total() > 0),
        Kind::Failed { file, message } => (
            tr(
                "open_summary.failed",
                &[("file", file.as_str()), ("e", message.as_str())],
            ),
            false,
        ),
    };
    let mut act = None;
    egui::TopBottomPanel::top("ddc.import_strip")
        .frame(
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(10, 0)),
        )
        .exact_height(STRIP_H)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                let sp = ui.spacing().item_spacing.x;
                let buttons = CLOSE_W
                    + sp
                    + if failed { 0.0 } else { REVIEW_W + sp }
                    + if mesh { MAKE_W + sp } else { 0.0 };
                let w = (ui.available_width() - buttons - sp).max(80.0);
                let used = ui
                    .allocate_ui_with_layout(
                        Vec2::new(w, STRIP_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let g = shell_kit::elided(
                                ui,
                                &text,
                                th.font(Role::Body),
                                ink,
                                ui.available_width(),
                            );
                            let (r, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
                            ui.painter().galley(r.min, g, ink);
                            resp.on_hover_text(&text);
                        },
                    )
                    .response
                    .rect
                    .width();
                ui.add_space((w - used).max(0.0));
                if mesh
                    && Button::secondary(&tr("open_summary.make", &[]))
                        .min_width(MAKE_W)
                        .show(ui, &th)
                        .on_hover_text(tr("open_summary.make_tip", &[]))
                        .clicked()
                {
                    act = Some(Act::Make);
                }
                if !failed
                    && Button::quiet(&tr("open_summary.review", &[]))
                        .min_width(REVIEW_W)
                        .show(ui, &th)
                        .clicked()
                {
                    act = Some(Act::Review);
                }
                if ddc_ui::widgets::close_button(ui, &tr("dlg.close", &[])).clicked() {
                    act = Some(Act::Close);
                }
            });
        });
    act
}

fn row(
    ui: &mut egui::Ui,
    th: &theme::Theme,
    label: &str,
    n: usize,
    ru: bool,
    clickable: bool,
) -> bool {
    let sem = th.sem();
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), ROW_H),
        if clickable {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if clickable && resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, sem.bg_hover);
    }
    let num = grp(n, ru);
    let ng = shell_kit::elided(ui, &num, th.font(Role::Numeric), sem.text_secondary, 80.0);
    let nx = rect.right() - 8.0 - ng.size().x;
    ui.painter().galley(
        egui::pos2(nx, rect.center().y - ng.size().y * 0.5),
        ng,
        sem.text_secondary,
    );
    let lg = shell_kit::elided(
        ui,
        label,
        th.font(Role::Body),
        sem.text_primary,
        (nx - rect.left() - 16.0).max(40.0),
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 8.0, rect.center().y - lg.size().y * 0.5),
        lg,
        sem.text_primary,
    );
    clickable && resp.on_hover_text(label).clicked()
}

fn panel(ctx: &egui::Context, s: &ImportSummary, ru: bool) -> Option<Act> {
    let th = theme::of(ctx);
    let sem = th.sem();
    let mut act = None;
    egui::SidePanel::right("ddc.import_review")
        .default_width(PANEL_W)
        .resizable(true)
        .frame(
            egui::Frame::new()
                .fill(sem.bg_panel)
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let w = ui.available_width() - CLOSE_W - ui.spacing().item_spacing.x;
                let g =
                    shell_kit::elided(ui, &s.file, th.font(Role::BodyStrong), sem.text_primary, w);
                let (r, resp) =
                    ui.allocate_exact_size(Vec2::new(w, g.size().y.max(ROW_H)), Sense::hover());
                ui.painter().galley(
                    egui::pos2(r.left(), r.center().y - g.size().y * 0.5),
                    g,
                    sem.text_primary,
                );
                resp.on_hover_text(&s.file);
                if ddc_ui::widgets::close_button(ui, &tr("dlg.close", &[])).clicked() {
                    act = Some(Act::ClosePanel);
                }
            });
            ui.add_space(space::XS);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    shell_kit::section_title(
                        ui,
                        &th,
                        &tr(
                            "open_summary.sec.native",
                            &[("n", grp(s.native_total(), ru).as_str())],
                        ),
                    );
                    for (k, n) in &s.native {
                        let clickable = !matches!(k.as_str(), "grid" | "roof" | "stair" | "railing" | "ramp");
                        if row(ui, &th, &category_label(k), *n, ru, clickable) {
                            act = Some(Act::Pick(Pick::Native(k.clone())));
                        }
                    }
                    ui.add_space(space::SM);
                    shell_kit::section_title(
                        ui,
                        &th,
                        &tr("open_summary.sec.mesh", &[("n", grp(s.mesh_total(), ru).as_str())]),
                    );
                    if s.mesh_total() > 0 {
                        widgets::elided_label(
                            ui,
                            &tr("open_summary.mesh_hint", &[]),
                            th.font(Role::Caption),
                            sem.text_secondary,
                        );
                        ui.add_space(space::XS);
                        if Button::secondary(&tr("open_summary.make", &[]))
                            .min_width(MAKE_W)
                            .show(ui, &th)
                            .on_hover_text(tr("open_summary.make_tip", &[]))
                            .clicked()
                        {
                            act = Some(Act::Make);
                        }
                        ui.add_space(space::XS);
                    }
                    for (k, n) in &s.mesh {
                        let label = if k == "mesh" {
                            tr("open_summary.cat.mesh", &[])
                        } else {
                            k.clone()
                        };
                        if row(ui, &th, &label, *n, ru, k != "mesh") {
                            act = Some(Act::Pick(Pick::Mesh(k.clone())));
                        }
                    }
                    ui.add_space(space::SM);
                    shell_kit::section_title(
                        ui,
                        &th,
                        &tr(
                            "open_summary.sec.skipped",
                            &[("n", grp(s.skipped_total(), ru).as_str())],
                        ),
                    );
                    for (k, n) in &s.skipped {
                        row(ui, &th, k, *n, ru, false);
                    }
                    ui.add_space(space::SM);
                    shell_kit::section_title(
                        ui,
                        &th,
                        &tr("open_summary.sec.notes", &[("n", grp(s.notes_total, ru).as_str())]),
                    );
                    for n in &s.notes {
                        widgets::elided_label(ui, n, th.font(Role::Caption), sem.text_secondary);
                    }
                    if s.notes_total > s.notes.len() {
                        let more = grp(s.notes_total - s.notes.len(), ru);
                        widgets::elided_label(
                            ui,
                            &tr("open_summary.notes_more", &[("n", more.as_str())]),
                            th.font(Role::Caption),
                            sem.text_secondary,
                        );
                    }
                });
        });
    act
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    #[test]
    fn summary_counts_equal_model_counts() {
        let mut e = Editor::start_project().unwrap();
        for k in 0..3 {
            let spec = ddc_model::WallSpec {
                a: (0.0, k as f64),
                b: (5.0, k as f64),
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: e.types[0].id,
                offset0: None,
            };
            e.model.add_wall(&spec).unwrap();
        }
        for cat in ["Furniture", "Furniture", "Plumbing"] {
            e.model
                .sql()
                .execute(
                    "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,?2,'m',x'',x'')",
                    rusqlite::params![e.level, cat],
                )
                .unwrap();
        }
        let s = from_model(&e.model, "House.rvt");
        assert_eq!(s.native, vec![("wall".to_string(), 3)]);
        assert_eq!(s.mesh_total(), 3);
        assert_eq!(
            s.mesh,
            vec![("Furniture".to_string(), 2), ("Plumbing".to_string(), 1)]
        );
        assert_eq!(s.native_total() + s.mesh_total(), 6);
    }

    #[test]
    fn report_is_stored_and_found_again() {
        let mut m = Model::open(":memory:").unwrap();
        m.sql()
            .execute(
                "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,first_import,imported_at)                  VALUES ('ifc:A','k','C:/a.ifc','ifc','r','1',100,100)",
                [],
            )
            .unwrap();
        record(
            &mut m,
            std::path::Path::new("C:/a.ifc"),
            [("IfcSpace".to_string(), 4)],
            &["w".to_string()],
            7,
        );
        let s = stored(&m).expect("report stored");
        assert_eq!(s.file, "a.ifc");
        assert_eq!((s.skipped_total(), s.notes_total), (4, 7));
    }

    #[test]
    fn click_selects_the_busiest_level() {
        let rows = [(Some(1), 10), (Some(2), 11), (Some(2), 12), (None, 13)];
        assert_eq!(on_busiest_level(&rows), (Some(2), vec![11, 12]));
        assert_eq!(on_busiest_level(&[]), (None, vec![]));
        assert_eq!(
            on_busiest_level(&[(None, 5), (None, 6)]),
            (None, vec![5, 6])
        );
    }

    #[test]
    fn rows_map_to_selections() {
        assert_eq!(sel_of("wall", 4), Some(Sel::Wall(4)));
        assert_eq!(sel_of("mesh", 9), Some(Sel::Body(BodyKind::Mesh, 9)));
        assert_eq!(sel_of("grid", 1), None);
    }

    #[test]
    fn numbers_are_grouped_by_language() {
        assert_eq!(grp(2006, false), "2,006");
        assert_eq!(grp(3212, true), "3\u{a0}212");
        assert_eq!(grp(0, false), "0");
        assert_eq!(grp(999, false), "999");
        assert_eq!(grp(1_234_567, false), "1,234,567");
    }
}
