//! Import sources: provenance panel (file, version, date, kinds), parameters of the selected
//! element with a "changed here" mark and the diff against the last import, re-import of an
//! updated file (3-way merge), conflict dialog, export of local changes
//! (XLSX delta, IFC with the original GlobalIds). Model edits go through `Editor::run_op`.

use crate::ui_kit as kit;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ddc_io::bridge::{self, ImportAs, Report};
use ddc_model::source::{
    show, Conflict, JsonValue as J, Pick, Policy, ResolveStats, SourceInfo, SourceParam,
    SqlValue as Sql, MASS_CHANGE,
};
use ddc_model::{Error, Model};
use crate::source_cf::{self, CfRow};
use ddc_ui::widgets::{self, Button};
use egui::{self, RichText};

use crate::editor::{Editor, OpFn, Sel};
use ddc_ui::numparse::{self, DecimalMark, NumError};
use crate::shell::EditorApp;

/// How many conflict rows to draw at once; the rest is shown as a counter.
const SHOWN: usize = 500;
/// Fits a source title on one header row of the Tool class.
const TITLE_CHARS: usize = 64;

#[derive(Default)]
pub struct State {
    pub panel: bool,
    pub conflicts: bool,
    /// Source the windows operate on (None — the last imported one).
    pub source: Option<String>,
    /// Background read of the updated file: (task, source key).
    job: Option<(crate::import::Job, String)>,
    /// Conflict choices (id → mine/theirs) before applying.
    picks: BTreeMap<i64, Pick>,
    /// Parameter value edit: (key, text).
    edit: Option<(String, String)>,
    /// Summary of the last re-import.
    pub last: Option<Vec<(&'static str, usize)>>,
    /// Keep unedited elements that disappeared from the file (deleted by default).
    pub keep_removed: bool,
    /// Merge even if the file does not recognize most of the elements.
    pub allow_mass: bool,
    /// Sources and conflicts read at this model revision (windows do not query the database every frame).
    cache: Option<Cache>,
    /// Check of source files on disk: "file changed" banner, relocation, IFC refresh.
    pub watch: crate::source_watch::Watch,
}

struct Cache {
    revision: u64,
    source: Option<String>,
    sources: Vec<SourceInfo>,
    conflicts: Vec<Conflict>,
}

/// Sources and conflicts of the current source; re-read only after a model edit.
fn cached(a: &mut EditorApp) -> &Cache {
    let rev = a.ed.revision;
    let fresh = a
        .src
        .cache
        .as_ref()
        .is_some_and(|c| c.revision == rev && c.source == a.src.source);
    if !fresh {
        let sources = a.ed.model.sources().unwrap_or_default();
        let cur = pick_source(&sources, a.src.source.as_deref()).map(|s| s.source.clone());
        let conflicts = a.ed.model.conflicts(cur.as_deref()).unwrap_or_default();
        a.src.cache = Some(Cache {
            revision: rev,
            source: a.src.source.clone(),
            sources,
            conflicts,
        });
    }
    a.src.cache.as_ref().expect("filled above")
}

fn pick_source<'s>(all: &'s [SourceInfo], want: Option<&str>) -> Option<&'s SourceInfo> {
    match want {
        Some(s) => all.iter().find(|i| i.source == s),
        None => all.first(),
    }
}

/// Re-import policy from the window checkboxes.
pub fn policy(s: &State) -> Policy {
    Policy {
        delete_removed: !s.keep_removed,
        allow_mass_change: s.allow_mass,
    }
}

pub fn register(a: &mut EditorApp) {
    a.register_command("data.src_panel", |a| a.src.panel = !a.src.panel);
    a.register_command("data.src_reimport", |a| reimport(a, None));
    a.register_command("data.src_conflicts", |a| a.src.conflicts = true);
    a.register_command("data.src_delta", export_delta);
    a.register_command("data.src_ifc", export_ifc);
}

/// Table and id of the selected element (None — a selection kind without a source row).
pub fn sel_ref(s: &Sel) -> Option<(&'static str, i64)> {
    #[allow(unreachable_patterns)]
    Some(match *s {
        Sel::Wall(i) => ("wall", i),
        Sel::Opening(i) => ("opening", i),
        Sel::Curtain(i) | Sel::Panel(i, _) => ("curtain_wall", i),
        Sel::Slab(i) => ("slab", i),
        Sel::Room(i) => ("room", i),
        Sel::Mep(i) => ("mep_run", i),
        Sel::Comp(i) => ("comp", i),
        Sel::Body(k, i) => (k.table(), i),
        _ => return None,
    })
}

/// `YYYY-MM-DD HH:MM` (UTC) from unix seconds; 0 — empty.
pub fn date(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let (days, sec) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // civil from days (proleptic Gregorian)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        sec / 3600,
        sec % 3600 / 60
    )
}

/// A typed value that is refused; nothing is written.
#[derive(Clone, Debug, PartialEq)]
pub enum BadValue {
    /// Not a number where the parameter holds a number.
    Number(String),
    /// A comma that groups no thousands where the decimal mark is a point (`1,5` in English).
    Comma(String),
}

impl BadValue {
    pub fn message(&self, cat: &ddc_ui::i18n::Catalog) -> String {
        match self {
            BadValue::Number(s) => cat.tf("source_input.err.number", &[("text", s)]),
            BadValue::Comma(s) => cat.tf("source_input.err.comma", &[("text", s)]),
        }
    }
}

/// Largest integer an f64 holds exactly.
const EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// Field text -> value of the same kind as the stored one, numbers read in the locale of
/// `mark`. A number parameter never becomes text: input that is not a number is refused.
pub fn parse_value(text: &str, like: Option<&J>, mark: DecimalMark) -> Result<Sql, BadValue> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Sql::Null);
    }
    let int = match like {
        Some(J::Number(n)) => n.is_i64(),
        // Unknown kind: a number if it reads as one, otherwise text.
        None => false,
        _ => return Ok(Sql::Text(t.into())),
    };
    let c = match numparse::canonical(t, mark) {
        Ok(c) => c,
        Err(NumError::CommaDecimal) => return Err(BadValue::Comma(t.into())),
        Err(_) if like.is_none() => return Ok(Sql::Text(t.into())),
        Err(_) => return Err(BadValue::Number(t.into())),
    };
    if int {
        if let Ok(i) = c.parse::<i64>() {
            return Ok(Sql::Integer(i));
        }
    }
    match c.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(if int && v.fract() == 0.0 && v.abs() < EXACT_INT {
            Sql::Integer(v as i64)
        } else {
            Sql::Real(v)
        }),
        _ if like.is_none() => Ok(Sql::Text(t.into())),
        _ => Err(BadValue::Number(t.into())),
    }
}

/// One edited parameter value as one model operation (one undo step).
pub fn set_param(
    ed: &mut Editor,
    kind: &'static str,
    id: i64,
    key: String,
    v: Sql,
    unit: String,
) -> Result<(), Error> {
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.set_source_prop(kind, id, &key, v.clone(), &unit)
    })))
}

pub(crate) fn current_source(a: &mut EditorApp) -> Option<SourceInfo> {
    let want = a.src.source.clone();
    pick_source(&cached(a).sources, want.as_deref()).cloned()
}

fn status(a: &mut EditorApp, key: &str, args: &[(&str, &str)]) {
    a.ed.status = Some(a.sh.cat.tf(key, args));
}

/// "Re-import": pick the updated file (the previous path by default) and read it
/// in the background; the merge runs on the UI thread after reading.
fn reimport(a: &mut EditorApp, source: Option<&str>) {
    if a.src.job.is_some() {
        return status(a, "src.status.busy", &[]);
    }
    if let Some(s) = source {
        a.src.source = Some(s.to_string());
    }
    let Some(info) = current_source(a) else {
        return status(a, "src.status.none", &[]);
    };
    let old = PathBuf::from(&info.path);
    let mut d = crate::file_pick::Dlg::new().set_title(a.sh.cat.t("src.pick"));
    if let Some(dir) = old.parent() {
        d = d.set_directory(dir);
    }
    if let Some(n) = old.file_name() {
        d = d.set_file_name(n.to_string_lossy());
    }
    if !info.format.is_empty() {
        d = d.add_filter(info.format.to_ascii_uppercase(), &[info.format.as_str()]);
    }
    let Some(path) = d.pick_file() else { return };
    if crate::source_watch::is_ifc(&path) {
        return crate::source_watch::reimport_ifc(a, &path, &info.source);
    }
    let Some((dir, trust)) = a.bridge_for(&path) else {
        return status(a, "src.status.no_reader", &[]);
    };
    let job = crate::import::Job::start(dir, trust, path);
    a.src.job = Some((job, info.source.clone()));
}

/// Frames read: merge as a single operation (undo), summary goes to the status line.
fn apply_reimport(
    a: &mut EditorApp,
    input: &Path,
    source: &str,
    frames: Vec<bridge::proto::Frame>,
) {
    let Some(info) = a.ed.model.source_info(source).ok().flatten() else {
        return status(a, "src.status.none", &[]);
    };
    let how = ImportAs {
        file_key: Some(info.file_key),
        source: Some(info.source),
        policy: policy(&a.src),
    };
    let out: Arc<Mutex<Option<Report>>> = Arc::default();
    let (o, input2, frames) = (out.clone(), input.to_path_buf(), Arc::new(frames));
    let f = OpFn(Arc::new(move |m: &mut Model| {
        let r = bridge::import_frames_as(m, &input2, frames.as_ref().clone(), &how)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        if let Ok(mut g) = o.lock() {
            *g = Some(r);
        }
        Ok(())
    }));
    match a.ed.run_op(f) {
        Ok(()) => {
            let r = out.lock().ok().and_then(|mut g| g.take());
            let pairs = r
                .as_ref()
                .map(|r| r.merge.pairs().to_vec())
                .unwrap_or_default();
            let text: Vec<String> = pairs
                .iter()
                .filter(|p| p.1 > 0)
                .map(|(k, n)| format!("{} {n}", a.sh.cat.t(&format!("src.stat.{k}"))))
                .collect();
            let conflicts = r.as_ref().map_or(0, |r| r.merge.conflicts);
            let line = if text.is_empty() {
                a.sh.cat.t("src.status.no_changes")
            } else {
                text.join(", ")
            };
            status(a, "src.status.merged", &[("stats", &line)]);
            a.src.last = Some(pairs);
            if conflicts > 0 {
                a.src.conflicts = true;
                a.src.picks.clear();
            }
        }
        Err(e) if e.to_string().contains(MASS_CHANGE) => status(a, "src.status.mass_change", &[]),
        Err(e) => status(a, "src.status.failed", &[("error", &e.to_string())]),
    }
}

fn save_path(a: &EditorApp, ext: &str, stem: &str) -> Option<PathBuf> {
    let p = crate::file_pick::Dlg::new()
        .set_title(a.sh.cat.t(&format!("src.save.{ext}")))
        .add_filter(ext.to_ascii_uppercase(), &[ext])
        .set_file_name(format!("{stem}.{ext}"))
        .save_file()?;
    Some(
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            p
        } else {
            p.with_extension(ext)
        },
    )
}

/// One default-name rule for every copy: `<name>-edited` (see `open::names`).
fn stem(info: &SourceInfo) -> String {
    let s = Path::new(&info.path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::open::names::copy_stem(&s)
}

/// Local changes relative to the source as an XLSX workbook (source guid, GlobalId, field,
/// before/after).
fn export_delta(a: &mut EditorApp) {
    let Some(info) = current_source(a) else {
        return status(a, "src.status.none", &[]);
    };
    let Some(path) = save_path(a, "xlsx", &stem(&info)) else {
        return;
    };
    match ddc_io::xlsx::export_delta(&a.ed.model, &info.source, true, &path) {
        Ok(n) => status(
            a,
            "src.status.delta",
            &[("n", &n.to_string()), ("path", &path.display().to_string())],
        ),
        Err(e) => status(a, "src.status.failed", &[("error", &e)]),
    }
}

/// Model as IFC with the original GlobalIds of the source elements.
fn export_ifc(a: &mut EditorApp) {
    let Some(info) = current_source(a) else {
        return status(a, "src.status.none", &[]);
    };
    let Some(path) = save_path(a, "ifc", &stem(&info)) else {
        return;
    };
    crate::save::export_copy(a, path);
}

pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    poll(a, ctx);
    crate::source_watch::frame(a, ctx);
    if a.src.panel {
        panel(a, ctx);
    }
    if a.src.conflicts {
        conflicts(a, ctx);
    }
}

fn poll(a: &mut EditorApp, ctx: &egui::Context) {
    let Some((job, _)) = &mut a.src.job else {
        return;
    };
    match job.poll() {
        Some(Ok(frames)) => {
            let Some((job, source)) = a.src.job.take() else {
                return;
            };
            apply_reimport(a, &job.input, &source, frames);
        }
        Some(Err(e)) => {
            a.src.job = None;
            status(a, "src.status.failed", &[("error", &e)]);
        }
        None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
    }
}

/// Action from the window (called after drawing, once `a` is free again).
enum Act {
    Source(String),
    Reimport(String),
    Conflicts(String),
    Delta(String),
    Ifc(String),
    SetParam(&'static str, i64, String, Sql, String),
    Refused(BadValue),
    Detach(&'static str, i64),
    Cancel,
}

fn panel(a: &mut EditorApp, ctx: &egui::Context) {
    let cur = current_source(a).map(|s| s.source);
    let sources = cached(a).sources.clone();
    let cat = &a.sh.cat;
    let th = ddc_ui::theme::of(ctx);
    let (mut keep_removed, mut allow_mass) = (a.src.keep_removed, a.src.allow_mass);
    let selected = a.ed.selection.first().and_then(sel_ref);
    let mut open = a.src.panel;
    let mut act: Option<Act> = None;
    let mut edit = a.src.edit.take();
    kit::Dialog::new("sources", cat.t("src.panel.title"), kit::DialogSize::Tool)
        .stable()
        .open(&mut open)
        .esc_closes(a.src.job.is_none())
        .scroll_body()
        .show(ctx, |ui| {
            if sources.is_empty() {
                kit::empty_note(ui, cat.t("src.panel.empty"), None);
            } else {
                let mut delete = !keep_removed;
                widgets::checkbox(ui, &th, &mut delete, &cat.t("src.opt.delete_removed"))
                    .on_hover_text(cat.t("src.opt.delete_removed_tip"));
                keep_removed = !delete;
                widgets::checkbox(ui, &th, &mut allow_mass, &cat.t("src.opt.allow_mass"))
                    .on_hover_text(cat.t("src.opt.allow_mass_tip"));
                if allow_mass {
                    kit::warn_note(ui, cat.t("src.opt.allow_mass_warn"));
                }
            }
            if let Some((job, _)) = &a.src.job {
                let (d, t) = job.progress;
                let f = if t > 0 { d as f32 / t as f32 } else { 0.0 };
                if kit::progress_row(ui, f, "", cat.t("src.cancel")) {
                    act = Some(Act::Cancel);
                }
            }
            for s in &sources {
                let here = cur.as_deref() == Some(s.source.as_str());
                let title = Path::new(&s.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.path.clone());
                // this header takes no eliding text and its width reaches the window
                egui::CollapsingHeader::new(
                    RichText::new(kit::shorten(&title, TITLE_CHARS)).strong(),
                )
                .id_salt(("src", &s.source))
                .default_open(here)
                .show(ui, |ui| {
                    source_grid(ui, cat, s);
                    ui.horizontal_wrapped(|ui| {
                        // one primary per source; the rest is quiet, rare exports sit in the menu
                        if Button::primary(&cat.t("cmd.data.src_reimport"))
                            .show(ui, &th)
                            .clicked()
                        {
                            act = Some(Act::Reimport(s.source.clone()));
                        }
                        if !here && Button::quiet(&cat.t("src.use")).show(ui, &th).clicked() {
                            act = Some(Act::Source(s.source.clone()));
                        }
                        let c = cat.tf("src.panel.conflicts", &[("n", &s.conflicts.to_string())]);
                        if ui
                            .add_enabled_ui(s.conflicts > 0, |ui| Button::quiet(&c).show(ui, &th))
                            .inner
                            .on_disabled_hover_text(cat.t("src.cf.none"))
                            .clicked()
                        {
                            act = Some(Act::Conflicts(s.source.clone()));
                        }
                        let more = Button::quiet(&cat.t("src.more")).show(ui, &th);
                        egui::Popup::menu(&more).show(|ui| {
                            let rows = [
                                ("cmd.data.src_delta", Act::Delta(s.source.clone())),
                                ("cmd.data.src_ifc", Act::Ifc(s.source.clone())),
                            ];
                            for (key, go) in rows {
                                if widgets::menu_item(
                                    ui,
                                    &th,
                                    &cat.t(key),
                                    "",
                                    widgets::MenuTone::Normal,
                                    true,
                                )
                                .clicked()
                                {
                                    act = Some(go);
                                    ui.close();
                                }
                            }
                        });
                    });
                    let views = a.ed.model.source_views(&s.source).unwrap_or_default();
                    if !views.is_empty() {
                        ui.collapsing(
                            cat.tf("src.panel.views", &[("n", &views.len().to_string())]),
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(160.0)
                                    .show(ui, |ui| {
                                        for v in &views {
                                            ui.label(format!(
                                                "{} · {}",
                                                v.name,
                                                cat.t(&format!("src.view.{}", v.kind))
                                            ));
                                        }
                                    });
                            },
                        );
                    }
                });
            }
            if let Some(last) = &a.src.last {
                ui.separator();
                ui.label(RichText::new(cat.t("src.panel.last")).strong());
                egui::Grid::new("src_last").num_columns(2).show(ui, |ui| {
                    for (k, n) in last {
                        ui.label(cat.t(&format!("src.stat.{k}")));
                        ui.label(n.to_string());
                        ui.end_row();
                    }
                });
            }
            ui.separator();
            match selected {
                None => {
                    ui.weak(cat.t("src.panel.select"));
                }
                Some((kind, id)) => {
                    element(ui, cat, &a.ed.model, kind, id, &mut edit, &mut act);
                }
            }
        });
    a.src.panel = open;
    a.src.edit = edit;
    a.src.keep_removed = keep_removed;
    a.src.allow_mass = allow_mass;
    if let Some(x) = act {
        run(a, x);
    }
}

fn source_grid(ui: &mut egui::Ui, cat: &ddc_ui::i18n::Catalog, s: &SourceInfo) {
    let mut rows: Vec<(String, String)> = vec![
        (cat.t("src.f.path"), s.path.clone()),
        (cat.t("src.f.format"), s.format.to_ascii_uppercase()),
        (
            cat.t("src.f.reader"),
            format!("{} {}", s.reader, s.reader_version)
                .trim()
                .to_string(),
        ),
    ];
    for (k, v) in &s.info {
        rows.push((or_raw(cat, &format!("src.info.{k}"), k), v.clone()));
    }
    rows.extend([
        (cat.t("src.f.imported"), date(s.imported_at)),
        (cat.t("src.f.first"), date(s.first_import)),
        (cat.t("src.f.imports"), s.imports.to_string()),
        (cat.t("src.f.elements"), s.elements.to_string()),
    ]);
    for (k, v) in rows.into_iter().filter(|r| !r.1.is_empty()) {
        kit::labeled_row(ui, &k, |ui| {
            kit::elided(ui, &v);
        });
    }
}

fn element(
    ui: &mut egui::Ui,
    cat: &ddc_ui::i18n::Catalog,
    m: &Model,
    kind: &'static str,
    id: i64,
    edit: &mut Option<(String, String)>,
    act: &mut Option<Act>,
) {
    let prov = m.provenance(kind, id).ok().flatten();
    match &prov {
        None => {
            ui.weak(cat.t("src.el.native"));
        }
        Some(p) => {
            egui::Grid::new("src_prov").num_columns(2).show(ui, |ui| {
                ui.label(cat.t("src.el.id"));
                ui.label(p.guid.as_str());
                ui.end_row();
                if let Some(g) = &p.ifc_guid {
                    ui.label(cat.t("src.el.ifc"));
                    ui.label(g.as_str());
                    ui.end_row();
                }
                ui.label(cat.t("src.f.imported"));
                ui.label(date(p.imported_at));
                ui.end_row();
                ui.label(cat.t("src.el.state"));
                let st = match (p.tracked, p.edited) {
                    (false, _) => cat.t("src.el.untracked"),
                    (true, true) => cat.t("src.el.edited"),
                    (true, false) => cat.t("src.el.same"),
                };
                if p.edited {
                    ui.colored_label(ui.visuals().warn_fg_color, st);
                } else {
                    ui.label(st);
                }
                ui.end_row();
            });
            if p.edited {
                let d = m.element_diff(kind, id).unwrap_or_default();
                ui.collapsing(
                    cat.tf("src.el.diff", &[("n", &d.len().to_string())]),
                    |ui| {
                        egui::Grid::new("src_diff")
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label(RichText::new(cat.t("src.c.field")).strong());
                                ui.label(RichText::new(cat.t("src.c.base")).strong());
                                ui.label(RichText::new(cat.t("src.c.mine")).strong());
                                ui.end_row();
                                let rows = m.kids_count(kind, id);
                                for f in &d {
                                    ui.label(field_label(cat, &f.field));
                                    ui.label(value_text(cat, &f.field, Side::Base, &f.base, rows));
                                    ui.label(value_text(cat, &f.field, Side::Mine, &f.mine, rows));
                                    ui.end_row();
                                }
                            });
                    },
                );
            }
            if ui
                .button(cat.t("src.el.detach"))
                .on_hover_text(cat.t("src.el.detach_tip"))
                .clicked()
            {
                *act = Some(Act::Detach(kind, id));
            }
        }
    }
    let params = m.source_params(kind, id).unwrap_or_default();
    if params.is_empty() {
        return;
    }
    ui.label(RichText::new(cat.tf("src.el.params", &[("n", &params.len().to_string())])).strong());
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .id_salt("src_params")
        .show(ui, |ui| {
            let mut groups: BTreeMap<&str, Vec<&SourceParam>> = BTreeMap::new();
            for p in &params {
                groups.entry(p.group.as_str()).or_default().push(p);
            }
            for (g, ps) in groups {
                let title = if g.is_empty() {
                    cat.t("src.el.no_group")
                } else {
                    g.to_string()
                };
                egui::CollapsingHeader::new(title)
                    .id_salt(("src_g", g))
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::Grid::new(("src_pg", g))
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                for p in ps {
                                    param_row(ui, cat, kind, id, p, edit, act);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
}

fn param_row(
    ui: &mut egui::Ui,
    cat: &ddc_ui::i18n::Catalog,
    kind: &'static str,
    id: i64,
    p: &SourceParam,
    edit: &mut Option<(String, String)>,
    act: &mut Option<Act>,
) {
    let mut name = RichText::new(&p.key);
    if p.edited {
        name = name.color(ui.visuals().warn_fg_color);
    }
    let mut tip = format!(
        "{}: {}",
        cat.t("src.c.scope"),
        cat.t(&format!("src.scope.{}", p.scope))
    );
    match p.shared {
        Some(true) => tip += &format!("\n{}", cat.t("src.shared.yes")),
        Some(false) => tip += &format!("\n{}", cat.t("src.shared.no")),
        None => {}
    }
    if let Some(b) = &p.base {
        if p.edited {
            tip += &format!("\n{}: {}", cat.t("src.c.base"), show(b));
        }
    }
    ui.label(name).on_hover_text(tip);
    let editing = edit.as_ref().is_some_and(|(k, _)| k == &p.key);
    if editing {
        let mut done = false;
        if let Some((_, text)) = edit.as_mut() {
            let mark = DecimalMark::for_lang(cat.lang);
            let like = p.base.as_ref().or(Some(&p.value));
            let parsed = parse_value(text, like, mark);
            let mut field = egui::TextEdit::singleline(text);
            if parsed.is_err() {
                field = field.text_color(ui.visuals().error_fg_color);
            }
            let mut r = ui.add(field);
            if let Err(b) = &parsed {
                r = r.on_hover_text(b.message(cat));
            }
            if r.lost_focus() {
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                match parsed {
                    Ok(v) if enter => {
                        *act = Some(Act::SetParam(kind, id, p.key.clone(), v, p.unit.clone()));
                        done = true;
                    }
                    // Refused: the field stays open with its error, nothing is written.
                    Err(b) if enter => *act = Some(Act::Refused(b)),
                    _ => done = true,
                }
            } else {
                r.request_focus();
            }
        }
        if done {
            *edit = None;
        }
    } else if ui
        .add(egui::Label::new(show(&p.value)).sense(egui::Sense::click()))
        .on_hover_text(cat.t("src.el.edit_tip"))
        .double_clicked()
    {
        *edit = Some((p.key.clone(), show(&p.value)));
    }
    ui.label(p.unit.as_str());
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Base,
    Mine,
    Theirs,
}

/// Cell text of a field value; child-row fields show a sentence, never the stored hash.
fn value_text(
    cat: &ddc_ui::i18n::Catalog,
    field: &str,
    side: Side,
    v: &J,
    rows: Option<i64>,
) -> String {
    if field != "kids" {
        return show(v);
    }
    match (side, rows) {
        (Side::Base, _) => String::new(),
        (Side::Mine, Some(n)) => cat.tf("src.kids.mine", &[("n", &n.to_string())]),
        (Side::Mine, None) => cat.t("src.kids.changed"),
        (Side::Theirs, _) => cat.t("src.kids.theirs"),
    }
}

fn field_label(cat: &ddc_ui::i18n::Catalog, f: &str) -> String {
    match f.strip_prefix("p:") {
        Some(k) => k.to_string(),
        None => or_raw(cat, &format!("src.field.{f}"), f),
    }
}

/// Translate a key; without a translation, the original name (source fields and details are open-ended).
fn or_raw(cat: &ddc_ui::i18n::Catalog, key: &str, raw: &str) -> String {
    let t = cat.t(key);
    if t == key {
        raw.to_string()
    } else {
        t
    }
}

fn run(a: &mut EditorApp, x: Act) {
    match x {
        Act::Source(s) => a.src.source = Some(s),
        Act::Reimport(s) => reimport(a, Some(&s)),
        Act::Conflicts(s) => {
            a.src.source = Some(s);
            a.src.conflicts = true;
        }
        Act::Delta(s) => {
            a.src.source = Some(s);
            export_delta(a);
        }
        Act::Ifc(s) => {
            a.src.source = Some(s);
            export_ifc(a);
        }
        Act::Cancel => {
            if let Some((j, _)) = &a.src.job {
                j.cancel();
            }
        }
        Act::SetParam(kind, id, key, v, unit) => {
            if let Err(e) = set_param(&mut a.ed, kind, id, key, v, unit) {
                status(a, "src.status.failed", &[("error", &e.to_string())]);
            }
        }
        Act::Refused(b) => a.ed.status = Some(b.message(&a.sh.cat)),
        Act::Detach(kind, id) => {
            let f = OpFn(Arc::new(move |m: &mut Model| {
                m.detach_from_source(kind, id)
            }));
            if let Err(e) = a.ed.run_op(f) {
                status(a, "src.status.failed", &[("error", &e.to_string())]);
            }
        }
    }
}

/// Conflict dialog: per element or parameter, "keep mine" / "take from file".
fn conflicts(a: &mut EditorApp, ctx: &egui::Context) {
    cached(a);
    // taken out for the frame (no copy of a long list), put back below
    let Some(cache) = a.src.cache.take() else {
        return;
    };
    let list: &[Conflict] = &cache.conflicts;
    let cat = &a.sh.cat;
    let th = ddc_ui::theme::of(ctx);
    let mut open = a.src.conflicts;
    let mut apply = false;
    let model = &a.ed.model;
    let picks = &mut a.src.picks;
    if picks.keys().next().is_some() {
        let ids: HashSet<i64> = list.iter().map(|c| c.id).collect();
        picks.retain(|id, _| ids.contains(id));
    }
    kit::Dialog::new(
        "source_conflicts",
        cat.tf("src.cf.title", &[("n", &list.len().to_string())]),
        kit::DialogSize::Wide,
    )
    .stable()
    .open(&mut open)
    .show(ctx, |ui| {
        if list.is_empty() {
            kit::empty_note(ui, cat.t("src.cf.none"), None);
            return;
        }
        ui.horizontal(|ui| {
            if Button::quiet(&cat.t("src.cf.all_mine")).show(ui, &th).clicked() {
                source_cf::pick_all(picks, list.iter().map(|c| c.id), Pick::Mine);
            }
            if Button::quiet(&cat.t("src.cf.all_theirs")).show(ui, &th).clicked() {
                source_cf::pick_all(picks, list.iter().map(|c| c.id), Pick::Theirs);
            }
        });
        let rows: Vec<CfRow> = list
            .iter()
            .take(SHOWN)
            .map(|c| {
                let kids = c.field == "kids";
                let n = kids
                    .then(|| model.kids_count(&c.kind, c.element))
                    .flatten();
                let f = c.field.as_str();
                CfRow {
                    id: c.id,
                    element: format!("{} {}", cat.t(&format!("src.kind.{}", c.kind)), c.element),
                    tip: format!("{}\n{}", c.guid, cat.t(&format!("src.case.{}", c.case.key()))),
                    field: if f == "*" {
                        cat.t(&format!("src.case.{}", c.case.key()))
                    } else {
                        field_label(cat, f)
                    },
                    base: value_text(cat, f, Side::Base, &c.base, n),
                    mine: value_text(cat, f, Side::Mine, &c.mine, n),
                    theirs: value_text(cat, f, Side::Theirs, &c.theirs, n),
                    theirs_tip: if kids { cat.t("src.kids.take_tip") } else { String::new() },
                }
            })
            .collect();
        // room for the footer below the table
        let footer_h = th.d().control_lg + 3.0 * ddc_ui::theme::space::LG;
        let table_h = ui.available_height() - footer_h;
        source_cf::conflict_table(ui, &th, cat, &rows, picks, table_h);
        if list.len() > SHOWN {
            kit::elided_weak(ui, &cat.tf("src.cf.more", &[("n", &(list.len() - SHOWN).to_string())]));
        }
        let why = source_cf::apply_blocked(picks).map(|k| cat.t(k));
        kit::dialog_footer(ui, |ui| {
            let label = cat.tf("src.cf.apply", &[("n", &picks.len().to_string())]);
            if kit::primary_action(ui, label, why.as_deref()) {
                apply = true;
            }
            let chosen = cat.tf(
                "src.cf.chosen",
                &[("n", &picks.len().to_string()), ("m", &list.len().to_string())],
            );
            kit::elided_weak(ui, &chosen);
        });
    });
    a.src.conflicts = open;
    a.src.cache = Some(cache);
    if apply {
        let chosen: Vec<(i64, Pick)> = a.src.picks.iter().map(|(k, v)| (*k, *v)).collect();
        a.src.picks.clear();
        resolve(a, chosen);
    }
}

fn resolve(a: &mut EditorApp, chosen: Vec<(i64, Pick)>) {
    let out: Arc<Mutex<Option<ResolveStats>>> = Arc::default();
    let o = out.clone();
    let chosen = Arc::new(chosen);
    let f = OpFn(Arc::new(move |m: &mut Model| {
        let r = m.resolve_conflicts(&chosen)?;
        if let Ok(mut g) = o.lock() {
            *g = Some(r);
        }
        Ok(())
    }));
    match a.ed.run_op(f) {
        Ok(()) => {
            let r = out
                .lock()
                .ok()
                .and_then(|mut g| g.take())
                .unwrap_or_default();
            status(
                a,
                "src.status.resolved",
                &[
                    ("mine", &r.kept_mine.to_string()),
                    ("theirs", &r.took_theirs.to_string()),
                    ("deleted", &r.deleted.to_string()),
                    ("restored", &r.restored.to_string()),
                    ("deferred", &r.deferred.to_string()),
                    ("refused", &r.refused.len().to_string()),
                ],
            );
        }
        Err(e) => status(a, "src.status.failed", &[("error", &e.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_civil() {
        assert_eq!(date(0), "");
        assert_eq!(date(1), "1970-01-01 00:00");
        assert_eq!(date(951_782_400), "2000-02-29 00:00");
        assert_eq!(date(1_762_258_380), "2025-11-04 12:13");
    }

    #[test]
    fn edited_value_keeps_the_base_kind() {
        let pv = |t: &str, like: Option<&J>| parse_value(t, like, DecimalMark::Point).unwrap();
        assert_eq!(pv("3.5", Some(&J::from(3.0))), Sql::Real(3.5));
        assert_eq!(pv("4", Some(&J::from(2))), Sql::Integer(4));
        assert_eq!(pv("EI 90", Some(&J::from("EI 60"))), Sql::Text("EI 90".into()));
        assert_eq!(pv("12", Some(&J::from("A"))), Sql::Text("12".into()));
        assert_eq!(pv("  ", None), Sql::Null);
        assert_eq!(pv("x", None), Sql::Text("x".into()));
    }

    #[test]
    fn numbers_follow_the_explicit_mark() {
        use DecimalMark::{Comma as RU, Point as EN};
        let int = J::from(7);
        let real = J::from(0.5);
        assert_eq!(parse_value("1,5", Some(&real), RU), Ok(Sql::Real(1.5)));
        assert_eq!(parse_value("1 200,5", Some(&real), RU), Ok(Sql::Real(1200.5)));
        assert_eq!(parse_value("1,5", Some(&int), RU), Ok(Sql::Real(1.5)));
        assert_eq!(parse_value("1 200", Some(&int), RU), Ok(Sql::Integer(1200)));
        assert_eq!(parse_value("1,250", Some(&int), EN), Ok(Sql::Integer(1250)));
        assert_eq!(parse_value("1,250", Some(&real), EN), Ok(Sql::Real(1250.0)));
        assert_eq!(parse_value("1e3", Some(&int), EN), Ok(Sql::Integer(1000)));
        assert_eq!(
            parse_value("9007199254740993", Some(&int), EN),
            Ok(Sql::Integer(9_007_199_254_740_993))
        );
        assert_eq!(parse_value("1,5", Some(&real), EN), Err(BadValue::Comma("1,5".into())));
        assert_eq!(parse_value("1,5", None, EN), Err(BadValue::Comma("1,5".into())));
        for bad in ["abc", "inf", "NaN", "12x", "1e400"] {
            for mark in [EN, RU] {
                assert!(
                    matches!(parse_value(bad, Some(&real), mark), Err(BadValue::Number(_))),
                    "{bad} {mark:?}"
                );
                assert!(parse_value(bad, Some(&int), mark).is_err(), "{bad}");
            }
        }
        // a text parameter keeps exactly what was typed, in every locale
        let text = J::from("A");
        for mark in [EN, RU] {
            assert_eq!(parse_value("1,5", Some(&text), mark), Ok(Sql::Text("1,5".into())));
        }
    }

    #[test]
    fn refusal_messages_exist_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for b in [BadValue::Number("abc".into()), BadValue::Comma("1,5".into())] {
                let m = b.message(&cat);
                assert!(!m.starts_with("source_input."), "missing key: {m}");
                assert!(m.contains(match &b {
                    BadValue::Number(s) | BadValue::Comma(s) => s.as_str(),
                }));
            }
        }
    }

    fn mesh(ed: &mut Editor) -> i64 {
        use ddc_geom::mesh::Mesh;
        use ddc_geom::v3;
        let m = Mesh {
            positions: vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)],
            triangles: vec![[0, 1, 2]],
            face_of: vec![0],
        };
        let (pb, ib) = ddc_model::mesh_blobs(&m);
        let level = ed.level;
        ed.model
            .batch_walls("wall_xs38_test_mesh", |db| {
                db.execute(
                    "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Furniture','m',?2,?3)",
                    (level, pb, ib),
                )?;
                Ok(db.last_insert_rowid())
            })
            .unwrap()
    }

    fn stored(ed: &Editor, id: i64, key: &str) -> Option<Sql> {
        ed.model
            .props("mesh", id)
            .unwrap()
            .into_iter()
            .find(|p| p.0 == key)
            .map(|p| p.1)
    }

    fn ops(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    }

    /// The UI path: parse with the mark, then write only an accepted value.
    fn commit(ed: &mut Editor, id: i64, key: &str, text: &str, mark: DecimalMark) -> bool {
        let like = match stored(ed, id, key) {
            Some(Sql::Integer(i)) => J::from(i),
            Some(Sql::Real(x)) => J::from(x),
            Some(Sql::Text(s)) => J::from(s),
            other => panic!("fixture: {other:?}"),
        };
        match parse_value(text, Some(&like), mark) {
            Ok(v) => {
                set_param(ed, "mesh", id, key.into(), v, String::new()).unwrap();
                true
            }
            Err(_) => false,
        }
    }

    #[test]
    fn numeric_param_edit_one_undo_and_refusal_writes_nothing() {
        use DecimalMark::{Comma as RU, Point as EN};
        let mut ed = Editor::new_in_memory().unwrap();
        let id = mesh(&mut ed);
        let put = |ed: &Editor, key: &str, v: Sql| {
            ed.model
                .sql()
                .execute(
                    "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,?2,?3,'')",
                    rusqlite::params![id, key, v],
                )
                .unwrap();
        };
        put(&ed, "P.Ratio", Sql::Real(0.5));
        put(&ed, "P.Count", Sql::Integer(7));
        put(&ed, "P.Mark", Sql::Text("A".into()));

        let n0 = ops(&ed);
        assert!(commit(&mut ed, id, "P.Ratio", "1,5", RU));
        assert_eq!(ops(&ed), n0 + 1, "one journal entry");
        assert_eq!(stored(&ed, id, "P.Ratio"), Some(Sql::Real(1.5)));
        ed.undo();
        assert_eq!(stored(&ed, id, "P.Ratio"), Some(Sql::Real(0.5)));

        assert!(commit(&mut ed, id, "P.Count", "1,250", EN));
        assert_eq!(stored(&ed, id, "P.Count"), Some(Sql::Integer(1250)));
        ed.undo();
        assert_eq!(stored(&ed, id, "P.Count"), Some(Sql::Integer(7)));

        // refused: no operation, the value and its kind stay
        let n1 = ops(&ed);
        for (key, text) in [("P.Ratio", "1,5"), ("P.Count", "1,5"), ("P.Ratio", "abc")] {
            assert!(!commit(&mut ed, id, key, text, EN), "{key} {text}");
        }
        assert_eq!(ops(&ed), n1, "nothing written");
        assert_eq!(stored(&ed, id, "P.Ratio"), Some(Sql::Real(0.5)));
        assert_eq!(stored(&ed, id, "P.Count"), Some(Sql::Integer(7)));

        // a text parameter stays text
        assert!(commit(&mut ed, id, "P.Mark", "1,5", RU));
        assert_eq!(stored(&ed, id, "P.Mark"), Some(Sql::Text("1,5".into())));
        ed.undo();
        assert_eq!(stored(&ed, id, "P.Mark"), Some(Sql::Text("A".into())));
    }

    #[test]
    fn reimport_policy_follows_the_options() {
        let mut s = State::default();
        assert_eq!(
            policy(&s),
            Policy {
                delete_removed: true,
                allow_mass_change: false
            }
        );
        s.keep_removed = true;
        s.allow_mass = true;
        assert_eq!(
            policy(&s),
            Policy {
                delete_removed: false,
                allow_mass_change: true
            }
        );
    }

    #[test]
    fn source_is_picked_by_key_or_latest() {
        let info = |s: &str| SourceInfo {
            source: s.into(),
            ..Default::default()
        };
        let all = vec![info("a"), info("b")];
        assert_eq!(
            pick_source(&all, None).map(|s| s.source.as_str()),
            Some("a")
        );
        assert_eq!(
            pick_source(&all, Some("b")).map(|s| s.source.as_str()),
            Some("b")
        );
        assert!(pick_source(&all, Some("c")).is_none());
    }

    #[test]
    fn kids_conflict_text_has_no_hash() {
        use ddc_ui::i18n::{Catalog, Lang};
        let h = J::String("0123456789abcdef".into());
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for side in [Side::Base, Side::Mine, Side::Theirs] {
                for rows in [None, Some(12)] {
                    let t = value_text(&cat, "kids", side, &h, rows);
                    assert!(!t.contains("0123456789abcdef"), "{t}");
                    assert!(!t.starts_with("src.kids"), "missing key: {t}");
                }
            }
            let mine = value_text(&cat, "kids", Side::Mine, &h, Some(12));
            assert!(mine.contains("12"), "{mine}");
            assert_eq!(
                value_text(&cat, "height", Side::Mine, &J::from(3.5), None),
                "3.5"
            );
        }
    }

    #[test]
    fn selection_maps_to_tables() {
        assert_eq!(sel_ref(&Sel::Panel(7, 2)), Some(("curtain_wall", 7)));
        assert_eq!(sel_ref(&Sel::Slab(3)), Some(("slab", 3)));
    }
}
