// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Version comparison: per-table counters (shared table with the restore window) and a diff window
//! with an element list; a click shows the element, highlighted in the plan and in 3D (the same
//! markers as clashes). The other version is a project file or a revision package; elements pair by
//! element key. Filters narrow the list; added and changed elements can be tinted in the views
//! (removed ones have no body here and stay boxes), and the views can show the changes only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ddc_geom::v2;
use ddc_io::revision;
use ddc_model::{
    compare_file, prepare_compare, DiffKind, ElementDiff, FieldChange, FieldGroup, Spot,
    TableDiff, VersionDiff,
};
use ddc_render::glam::Vec3;
use ddc_render::Aabb;
use ddc_ui::i18n::Catalog;
use ddc_ui::notify::Level;
use egui::{self, Color32, RichText};

use super::job::{working_body, Cancel, Job, Polled};
use super::{elided, file_label, kind_label, num, select, work_dir};
use crate::ai::sets::Key;
use crate::ai::view_tools::{Bucket, Legend, LegendRow, ViewCmd};
use crate::shell::clash_win::{box_mesh, MARK};
use crate::shell::EditorApp;
use crate::ui_kit as kit;

/// 3D markers for the whole list only up to this count: the scene must not grow unbounded.
const MARKS_3D: usize = 500;
/// 3D box height when the element's height is unknown.
const BOX_H: f64 = 3.0;
/// `Legend::by` of the change tint: tells it from other colorings.
pub const DIFF_BY: &str = "ddc.diff";
const GROUPS: [FieldGroup; 4] = [
    FieldGroup::Column,
    FieldGroup::Relation,
    FieldGroup::Param,
    FieldGroup::Part,
];
/// Filter slots of the change kinds: added, changed, removed.
const KINDS: [DiffKind; 3] = [DiffKind::HereOnly, DiffKind::Differs, DiffKind::ThereOnly];

fn group_key(g: FieldGroup) -> &'static str {
    match g {
        FieldGroup::Column => "collab_diff.diff.group.column",
        FieldGroup::Relation => "collab_diff.diff.group.relation",
        FieldGroup::Param => "collab_diff.diff.group.param",
        FieldGroup::Part => "collab_diff.diff.group.part",
    }
}

/// What the list shows: kinds, categories, kinds of change and a text.
#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    /// Added, changed, removed ([`KINDS`] order).
    pub kinds: [bool; 3],
    /// [`GROUPS`] order; a changed element shows when one of its changes is in an enabled group.
    pub groups: [bool; 4],
    pub table: Option<String>,
    /// Case-insensitive text in the category, id, key, field names or values.
    pub text: String,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            kinds: [true; 3],
            groups: [true; 4],
            table: None,
            text: String::new(),
        }
    }
}

fn slot<T: PartialEq>(all: &[T], x: &T) -> usize {
    all.iter().position(|y| y == x).unwrap_or(0)
}

pub fn passes(e: &ElementDiff, f: &Filter) -> bool {
    if !f.kinds[slot(&KINDS, &e.kind)] || f.table.as_ref().is_some_and(|t| *t != e.table) {
        return false;
    }
    if e.kind == DiffKind::Differs
        && !e.changes.is_empty()
        && !e.changes.iter().any(|c| f.groups[slot(&GROUPS, &c.group)])
    {
        return false;
    }
    let q = f.text.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let has = |s: &str| s.to_lowercase().contains(&q);
    has(&e.table)
        || has(&e.id.to_string())
        || e.key.as_deref().is_some_and(has)
        || e.changes.iter().any(|c| {
            has(&c.field) || c.old.as_deref().is_some_and(has) || c.new.as_deref().is_some_and(has)
        })
}

/// Indices of the elements the filter lets through, in list order.
pub fn visible(d: &VersionDiff, f: &Filter) -> Vec<usize> {
    (0..d.elements.len()).filter(|&i| passes(&d.elements[i], f)).collect()
}

fn rgb(c: Color32) -> u32 {
    (u32::from(c.r()) << 16) | (u32::from(c.g()) << 8) | u32::from(c.b())
}

/// The change tint: one row for the added and one for the changed elements of `shown` that
/// the views draw (`model` is every element key of the open model). Removed elements are not
/// in this model and have no row.
pub fn tint_legend(
    d: &VersionDiff,
    shown: &[usize],
    model: &[Key],
    rows: [(String, u32); 2],
) -> Legend {
    let mut slot_of: HashMap<(&str, i64), usize> = HashMap::new();
    for &i in shown {
        let e = &d.elements[i];
        match e.kind {
            DiffKind::HereOnly => slot_of.insert((e.table.as_str(), e.id), 0),
            DiffKind::Differs => slot_of.insert((e.table.as_str(), e.id), 1),
            DiffKind::ThereOnly => None,
        };
    }
    let mut keys: [Vec<Key>; 2] = Default::default();
    for k in model {
        if let Some(&s) = slot_of.get(&(k.table(), k.id)) {
            keys[s].push(*k);
        }
    }
    let rows = rows
        .into_iter()
        .zip(keys)
        .filter(|(_, k)| !k.is_empty())
        .map(|((value, rgb), mut k)| {
            k.sort();
            k.dedup();
            LegendRow {
                value,
                bucket: Bucket::Value,
                rgb,
                keys: Arc::new(k),
            }
        })
        .collect();
    Legend {
        by: DIFF_BY.to_string(),
        rows,
        folded: 0,
    }
}

/// The "table / added / removed / changed" table.
pub fn table_grid(ui: &mut egui::Ui, c: &Catalog, v: &[TableDiff]) {
    egui::Grid::new("ddc.recover.diff")
        .num_columns(4)
        .show(ui, |ui| {
            for k in [
                "recover.diff.table",
                "recover.diff.added",
                "recover.diff.removed",
                "recover.diff.changed",
            ] {
                ui.label(RichText::new(c.t(k)).strong());
            }
            ui.end_row();
            for t in v {
                ui.label(&t.table);
                num(ui, t.added as usize);
                num(ui, t.removed as usize);
                num(ui, t.changed as usize);
                ui.end_row();
            }
        });
}

/// The diff (or the prepared copy when the model is in memory) and the revision label.
type Ready = Result<(VersionDiff, Option<PathBuf>, Option<String>), String>;

#[derive(Default)]
enum Stage {
    #[default]
    Closed,
    Working(PathBuf, Job<Ready>),
    Failed(String),
    Ready(Box<Shown>),
}

struct Shown {
    source: PathBuf,
    /// Label of a revision package.
    label: Option<String>,
    diff: VersionDiff,
    pick: Option<usize>,
    all: bool,
    level: i64,
    filter: Filter,
    /// Elements the filter lets through.
    visible: Vec<usize>,
    /// Added and changed elements tinted in the views.
    tint: bool,
    /// The views show the added and changed elements only.
    only: bool,
    /// The tint or the isolation is on the views (Reset view takes it off).
    applied: bool,
}

#[derive(Default)]
pub struct DiffWin {
    stage: Stage,
    pending: Option<PathBuf>,
}

pub(super) fn probe(w: &DiffWin) -> super::WinProbe {
    use super::WinProbe as P;
    match &w.stage {
        Stage::Closed if w.pending.is_some() => P::Working,
        Stage::Closed => P::Closed,
        Stage::Working(..) => P::Working,
        Stage::Failed(m) => P::Refused(m.clone()),
        Stage::Ready(s) => P::Shown { source: s.source.clone(), count: s.diff.total },
    }
}

pub fn start(a: &mut EditorApp) {
    let picked = crate::file_pick::Dlg::new()
        .set_title(a.sh.cat.t("diff.pick"))
        .add_filter(&a.sh.cat.t("collab_diff.diff.versions"), &["ddc", revision::EXT])
        .add_filter("DDC", &["ddc"])
        .add_filter(&a.sh.cat.t("collab_diff.diff.revisions"), &[revision::EXT])
        .pick_file();
    if let Some(p) = picked {
        a.collab.diff.pending = Some(p);
    }
}

fn begin(w: &mut DiffWin, a: &EditorApp, path: PathBuf) {
    let model = a.ed.model.db_path();
    let source = path.clone();
    let work = work_dir("cmp");
    let dir = work.clone();
    let job = Job::spawn(work, move |c| worker(&source, &dir, model.as_deref(), c));
    w.stage = Stage::Working(path, job);
}

/// Background part: reads the chosen file and the model file only. None once cancelled.
fn worker(source: &Path, work: &Path, model: Option<&str>, c: &Cancel) -> Option<Ready> {
    if c.is_set() {
        return None;
    }
    let (file, label) = if revision::is_revision(source) {
        match revision::unpack_model(source, &work.join("rev")) {
            Ok((p, info)) => (p, info.label),
            Err(e) => return Some(Err(e.to_string())),
        }
    } else {
        (source.to_path_buf(), None)
    };
    if c.is_set() {
        return None;
    }
    let copy = match prepare_compare(&file, work) {
        Ok(p) => p,
        Err(e) => return Some(Err(e.to_string())),
    };
    if c.is_set() {
        return None;
    }
    Some(match model {
        Some(m) => compare_file(m, &copy)
            .map(|d| (d, None, label))
            .map_err(|e| e.to_string()),
        None => Ok((VersionDiff::default(), Some(copy), label)),
    })
}

/// Cancel during Working: the job stops and the window closes.
fn cancel_work(w: &mut DiffWin) -> Option<Job<Ready>> {
    if !matches!(w.stage, Stage::Working(..)) {
        return None;
    }
    let Stage::Working(_, mut job) = std::mem::take(&mut w.stage) else {
        return None;
    };
    job.cancel();
    Some(job)
}

fn poll(w: &mut DiffWin, a: &EditorApp, ctx: &egui::Context) {
    let Stage::Working(path, job) = &mut w.stage else {
        return;
    };
    ctx.request_repaint_after(Duration::from_millis(100));
    let got = match job.poll() {
        Polled::Done(v) => v,
        Polled::Pending => return,
        Polled::Lost => Err(crate::jobs::internal_error()),
    };
    let source = path.clone();
    w.stage = match got {
        Ok((diff, None, label)) => shown(a, source, label, diff),
        // in-memory model: compare on its own connection
        Ok((_, Some(copy), label)) => match a.ed.model.compare_version(&copy) {
            Ok(d) => shown(a, source, label, d),
            Err(e) => Stage::Failed(a.sh.cat.tf("diff.failed", &[("e", &e.to_string())])),
        },
        Err(e) => Stage::Failed(a.sh.cat.tf("diff.failed", &[("e", &e)])),
    };
}

fn shown(a: &EditorApp, source: PathBuf, label: Option<String>, diff: VersionDiff) -> Stage {
    let filter = Filter::default();
    let visible = visible(&diff, &filter);
    Stage::Ready(Box::new(Shown {
        source,
        label,
        diff,
        pick: None,
        all: false,
        level: a.ed.level,
        filter,
        visible,
        tint: false,
        only: false,
        applied: false,
    }))
}

/// Puts the tint and the isolation of `s` on the views, after taking off the previous ones.
fn retint(a: &mut EditorApp, s: &mut Shown) {
    use crate::ai::view_state::apply;
    if s.applied {
        apply(a, &ViewCmd::Reset);
        s.applied = false;
    }
    if !s.tint && !s.only {
        return;
    }
    let model: Vec<Key> = crate::ai::rows::rows(&a.ed).iter().map(crate::ai::rows::key_of).collect();
    let c = &a.sh.cat;
    let th = &a.sh.theme;
    let row = |k: DiffKind| {
        let ck = change_kind(k);
        (c.t(ck.label_key()), rgb(ck.color(th)))
    };
    let lg = tint_legend(&s.diff, &s.visible, &model, [row(DiffKind::HereOnly), row(DiffKind::Differs)]);
    if s.only {
        let keys: Vec<Key> = lg.rows.iter().flat_map(|r| r.keys.iter().copied()).collect();
        apply(a, &ViewCmd::Isolate { keys: Arc::new(keys), hide: false });
    }
    if s.tint {
        apply(a, &ViewCmd::Color(Arc::new(lg)));
    }
    s.applied = true;
}

/// Takes the tint off when it is still ours (Reset view may have done it already).
fn untint(a: &mut EditorApp, s: &mut Shown) {
    let ours = crate::ai::view_state::legend().is_some_and(|l| l.by == DIFF_BY);
    if s.applied && (ours || s.only) {
        crate::ai::view_state::apply(a, &ViewCmd::Reset);
    }
    s.applied = false;
}

fn clear_marks(a: &mut EditorApp) {
    if !a.plan.clash_marks.is_empty() || !a.clash.marks.is_empty() {
        a.plan.clash_marks.clear();
        a.clash.marks.clear();
        a.clash.dirty3d = true;
    }
}

fn role(k: DiffKind) -> u8 {
    match k {
        DiffKind::Differs => 0,
        DiffKind::ThereOnly => 1,
        DiffKind::HereOnly => 2,
    }
}

fn corners(s: &Spot) -> ([f32; 3], [f32; 3]) {
    let pad = 0.1;
    (
        [(s.min[0] - pad) as f32, (s.min[1] - pad) as f32, s.z as f32],
        [
            (s.max[0] + pad) as f32,
            (s.max[1] + pad) as f32,
            (s.z + BOX_H) as f32,
        ],
    )
}

/// Markers: the selected row or the whole (filtered) list; the plan shows only the current level.
/// With the tint on, removed elements (no body in this model) are always boxes.
fn set_marks(a: &mut EditorApp, s: &Shown) {
    let mut idx: Vec<usize> = if s.all {
        s.visible.clone()
    } else {
        s.pick.into_iter().collect()
    };
    if s.tint || s.only {
        idx.extend(s.visible.iter().filter(|&&i| s.diff.elements[i].kind == DiffKind::ThereOnly));
        idx.sort_unstable();
        idx.dedup();
    }
    let picked: Vec<&ElementDiff> = idx.iter().filter_map(|&i| s.diff.elements.get(i)).collect();
    let level = a.ed.level;
    a.plan.clash_marks = picked
        .iter()
        .filter_map(|e| e.spot.map(|sp| (e, sp)))
        .filter(|(_, sp)| sp.level.is_none_or(|l| l == level))
        .map(|(e, sp)| {
            (
                v2(sp.min[0], sp.min[1]),
                v2(sp.max[0], sp.max[1]),
                role(e.kind),
            )
        })
        .collect();
    a.clash.marks = picked
        .iter()
        .filter_map(|e| e.spot.map(|sp| (e, sp)))
        .take(MARKS_3D)
        .map(|(e, sp)| {
            let (lo, hi) = corners(&sp);
            box_mesh(MARK + u32::from(role(e.kind)), 0, lo, hi)
        })
        .collect();
    a.clash.dirty3d = true;
}

fn zoom(a: &mut EditorApp, sp: &Spot) {
    if let Some(l) = sp.level {
        a.ed.set_level(l);
    }
    let c = [(sp.min[0] + sp.max[0]) / 2.0, (sp.min[1] + sp.max[1]) / 2.0];
    let span = (sp.max[0] - sp.min[0]).max(sp.max[1] - sp.min[1]).max(1.0);
    a.plan.center = v2(c[0], c[1]);
    a.plan.scale = (320.0 / span).clamp(20.0, 2000.0);
    let (lo, hi) = corners(sp);
    a.clash.fit = Some(Aabb {
        min: Vec3::from(lo),
        max: Vec3::from(hi),
    });
}

/// Shared change language: new here = added, missing here = deleted.
fn change_kind(k: DiffKind) -> ddc_ui::diff_kind::DiffKind {
    use ddc_ui::diff_kind::DiffKind as Change;
    match k {
        DiffKind::HereOnly => Change::Added,
        DiffKind::ThereOnly => Change::Deleted,
        DiffKind::Differs => Change::Changed,
    }
}

fn kind_text(a: &EditorApp, k: DiffKind) -> String {
    a.sh.cat.t(match k {
        DiffKind::HereOnly => "diff.here_only",
        DiffKind::ThereOnly => "diff.there_only",
        DiffKind::Differs => "diff.differs",
    })
}

/// What a frame of the window asks for.
#[derive(Default)]
struct Acts {
    clicked: Option<usize>,
    /// Marks follow the list again.
    remark: bool,
    /// The tint or the isolation follows the list again.
    retint: bool,
    /// Play the differences as a build-up in the 3D view.
    reveal: bool,
}

/// A field as the change table shows it: a property by its name, a part group by its table.
fn field_text(a: &EditorApp, ch: &FieldChange) -> String {
    match ch.field.strip_prefix('#') {
        Some(t) => a.sh.cat.tf("collab_diff.diff.part_rows", &[("part", t)]),
        None => ch.field.split_once('.').filter(|_| ch.group == FieldGroup::Param).map_or_else(
            || ch.field.clone(),
            |(_, name)| name.to_string(),
        ),
    }
}

fn filter_bar(ui: &mut egui::Ui, a: &EditorApp, s: &mut Shown) -> bool {
    use ddc_ui::widgets::{self, Button, TextField};
    let (c, th) = (&a.sh.cat, &a.sh.theme);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (i, k) in KINDS.iter().enumerate() {
            let ck = change_kind(*k);
            ddc_ui::diff_kind::badge(ui, ck, ck.color(th));
            changed |= widgets::checkbox(ui, th, &mut s.filter.kinds[i], &c.t(ck.label_key())).changed();
        }
        ui.separator();
        for (i, g) in GROUPS.iter().enumerate() {
            changed |= widgets::checkbox(ui, th, &mut s.filter.groups[i], &c.t(group_key(*g))).changed();
        }
    });
    ui.horizontal(|ui| {
        let mut tables: Vec<&str> = s.diff.elements.iter().map(|e| e.table.as_str()).collect();
        tables.sort_unstable();
        tables.dedup();
        let mut names = vec![c.t("collab_diff.diff.all_categories")];
        names.extend(tables.iter().map(|t| kind_label(a, t)));
        let items: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut sel = s
            .filter
            .table
            .as_deref()
            .and_then(|t| tables.iter().position(|x| *x == t))
            .map_or(0, |i| i + 1);
        if widgets::dropdown(ui, th, "ddc.diff.table", &mut sel, &items, None).changed() {
            s.filter.table = sel.checked_sub(1).and_then(|i| tables.get(i)).map(|t| t.to_string());
            changed = true;
        }
        let hint = c.t("collab_diff.diff.search");
        changed |= TextField::new(&mut s.filter.text)
            .labelled_by(egui::Id::new("ddc.diff.search.lbl"), &hint)
            .hint(&hint)
            .width(220.0)
            .show(ui, th)
            .changed;
        if s.filter != Filter::default() && Button::quiet(&c.t("collab_diff.diff.clear")).small().show(ui, th).clicked() {
            s.filter = Filter::default();
            changed = true;
        }
    });
    if changed {
        s.visible = visible(&s.diff, &s.filter);
        if s.pick.is_some_and(|p| !s.visible.contains(&p)) {
            s.pick = None;
        }
    }
    changed
}

/// Old and new value of every change of the picked element.
fn change_table(ui: &mut egui::Ui, a: &EditorApp, e: &ElementDiff) {
    let c = &a.sh.cat;
    let weak = a.sh.theme.c("text.secondary");
    let head = match &e.key {
        Some(k) => format!("{} #{} \u{b7} {k}", kind_label(a, &e.table), e.id),
        None => format!("{} #{}", kind_label(a, &e.table), e.id),
    };
    ui.label(RichText::new(head).strong());
    if e.changes.is_empty() {
        ui.colored_label(weak, c.t(match e.kind {
            DiffKind::HereOnly => "collab_diff.diff.added_whole",
            DiffKind::ThereOnly => "collab_diff.diff.removed_whole",
            DiffKind::Differs => "collab_diff.diff.no_fields",
        }));
        return;
    }
    let none = c.t("collab_diff.diff.no_value");
    egui::ScrollArea::vertical()
        .id_salt("ddc.diff.changes")
        .max_height(220.0)
        .show(ui, |ui| {
            egui::Grid::new("ddc.diff.changes.grid")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    for k in [
                        "collab_diff.diff.col.group",
                        "collab_diff.diff.col.field",
                        "collab_diff.diff.col.old",
                        "collab_diff.diff.col.new",
                    ] {
                        ui.label(RichText::new(c.t(k)).strong());
                    }
                    ui.end_row();
                    for ch in &e.changes {
                        ui.colored_label(weak, c.t(group_key(ch.group)));
                        ui.label(field_text(a, ch));
                        match &ch.old {
                            Some(v) => elided(ui, v),
                            None => {
                                ui.colored_label(weak, &none);
                            }
                        }
                        match &ch.new {
                            Some(v) => elided(ui, v),
                            None => {
                                ui.colored_label(weak, &none);
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}

fn body(ui: &mut egui::Ui, a: &EditorApp, s: &mut Shown) -> Acts {
    let c = &a.sh.cat;
    let mut acts = Acts::default();
    elided(ui, &s.source.display().to_string());
    if let Some(l) = &s.label {
        ui.label(c.tf("collab_diff.diff.revision", &[("label", l)]));
    }
    if let Some(k) = crate::empty_kind::version_compare(s.diff.total) {
        crate::ui_kit::empty_state_lang(ui, crate::ui_lang_now(), k.what(), k.has_action());
        return acts;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(c.tf("diff.total", &[("n", &s.diff.total.to_string())]));
        acts.remark |= ui.checkbox(&mut s.all, c.t("diff.highlight_all")).changed();
        let th = &a.sh.theme;
        let t = ddc_ui::widgets::checkbox(ui, th, &mut s.tint, &c.t("collab_diff.diff.tint")).changed();
        let o = ddc_ui::widgets::checkbox(ui, th, &mut s.only, &c.t("collab_diff.diff.only")).changed();
        acts.retint |= t || o;
        acts.remark |= t || o;
        if ddc_ui::widgets::Button::quiet(&c.t("collab_diff.reveal.start")).small().show(ui, th).clicked() {
            acts.reveal = true;
        }
    });
    if s.diff.total > s.diff.elements.len() {
        ui.colored_label(
            a.sh.theme.c("text.secondary"),
            c.tf("diff.capped", &[("n", &s.diff.elements.len().to_string())]),
        );
    }
    if filter_bar(ui, a, s) {
        acts.remark = true;
        acts.retint |= s.tint || s.only;
    }
    if s.visible.len() < s.diff.elements.len() {
        let n = [("n", s.visible.len().to_string()), ("total", s.diff.elements.len().to_string())];
        let n: Vec<(&str, &str)> = n.iter().map(|(k, v)| (*k, v.as_str())).collect();
        ui.colored_label(a.sh.theme.c("text.secondary"), c.tf("collab_diff.diff.shown", &n));
    }
    egui::CollapsingHeader::new(c.t("diff.by_table"))
        .id_salt("ddc.diff.tables")
        .show(ui, |ui| table_grid(ui, c, &s.diff.tables));
    ui.separator();
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
    let list_h = if s.pick.is_some() { (ui.available_height() - 260.0).max(120.0) } else { f32::INFINITY };
    egui::ScrollArea::vertical()
        .id_salt("ddc.diff.rows")
        .max_height(list_h)
        .auto_shrink([false, s.pick.is_some()])
        .show_rows(ui, row_h, s.visible.len(), |ui, range| {
            for i in s.visible[range].iter().copied() {
                let e = &s.diff.elements[i];
                let text = format!(
                    "{} #{} · {}",
                    kind_label(a, &e.table),
                    e.id,
                    kind_text(a, e.kind)
                );
                ui.horizontal(|ui| {
                    let ck = change_kind(e.kind);
                    ddc_ui::diff_kind::badge(ui, ck, ck.color(&a.sh.theme));
                    if ui.selectable_label(s.pick == Some(i), &text).clicked() {
                        acts.clicked = Some(i);
                    }
                    if let Some(k) = &e.key {
                        ui.colored_label(a.sh.theme.c("text.secondary"), k);
                    }
                    if !e.changes.is_empty() {
                        let names: Vec<String> = e.changes.iter().map(|ch| field_text(a, ch)).collect();
                        elided(ui, &names.join(", "));
                    }
                    if e.spot.is_none() {
                        ui.colored_label(a.sh.theme.c("text.secondary"), c.t("diff.no_place"));
                    }
                });
            }
        });
    if let Some(e) = s.pick.and_then(|i| s.diff.elements.get(i)) {
        ui.separator();
        change_table(ui, a, e);
    }
    acts
}

pub fn frame(w: &mut DiffWin, a: &mut EditorApp, ctx: &egui::Context) {
    if let Some(p) = w.pending.take() {
        begin(w, a, p);
    }
    poll(w, a, ctx);
    let title = a.sh.cat.t("diff.title");
    let id = egui::Id::new("ddc_diff");
    let mut open = !matches!(w.stage, Stage::Closed);
    let mut act = Acts::default();
    let mut cancel = false;
    match &mut w.stage {
        Stage::Closed => return,
        Stage::Working(path, job) => {
            let text = a.sh.cat.tf("diff.working", &[("file", &file_label(path))]);
            let mut alive = true;
            kit::Dialog::new("collab.diff.work", title.as_str(), kit::DialogSize::Confirm)
                .id(id)
                .open(&mut alive)
                .show(ctx, |ui| {
                    cancel = working_body(ui, &a.sh.cat, &text, job.elapsed());
                });
            cancel |= !alive;
        }
        Stage::Failed(msg) => {
            kit::Dialog::new("collab.diff.failed", title.as_str(), kit::DialogSize::Confirm)
                .id(id)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.colored_label(a.sh.theme.c("warning"), msg.as_str());
                });
        }
        Stage::Ready(s) => {
            kit::Dialog::new("collab.diff", title.as_str(), kit::DialogSize::Wide)
                .id(id)
                .open(&mut open)
                .show(ctx, |ui| act = body(ui, a, s));
        }
    }
    if cancel && cancel_work(w).is_some() {
        a.sh.toasts.push(Level::Info, a.sh.cat.t("collab.cancelled"), None);
        return;
    }
    if let Stage::Ready(s) = &mut w.stage {
        let mut remark = act.remark;
        if act.retint {
            retint(a, s);
        }
        if act.reveal && !crate::reveal::start_version(a, &s.diff) {
            a.sh.toasts.push(Level::Info, a.sh.cat.t("collab_diff.reveal.nothing"), None);
        }
        if let Some(i) = act.clicked {
            s.pick = Some(i);
            remark = true;
            if let Some(e) = s.diff.elements.get(i).cloned() {
                if e.kind != DiffKind::ThereOnly {
                    select(a, &e.table, e.id);
                }
                if let Some(sp) = &e.spot {
                    zoom(a, sp);
                }
            }
        }
        if s.level != a.ed.level {
            s.level = a.ed.level;
            remark = true;
        }
        if remark {
            set_marks(a, s);
        }
    }
    if !open {
        if let Stage::Ready(s) = &mut w.stage {
            clear_marks(a);
            untint(a, s);
        }
        w.stage = Stage::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::job::JobState;
    use ddc_model::Model;
    use web_time::{Instant, SystemTime};

    fn scratch(name: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!("ddc-dv-test-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn version(d: &Path) -> (PathBuf, PathBuf) {
        let mine = d.join("mine.ddc");
        let other = d.join("other.ddc");
        let mut m = Model::open(mine.to_str().unwrap()).unwrap();
        m.add_level("L1", 0.0).unwrap();
        m.snapshot_to(&other).unwrap();
        drop(m);
        (mine, other)
    }

    #[test]
    fn diff_cancel_while_working_closes_and_ignores_result() {
        let d = scratch("cancel");
        let (mine, other) = version(&d);
        let before = std::fs::read(&mine).unwrap();
        let work = d.join("ddc-collab-1-0-cmp");
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let (m, wk, src) = (mine.to_string_lossy().into_owned(), work.clone(), other.clone());
        let job = Job::spawn(work.clone(), move |c| {
            let _ = gate.recv();
            worker(&src, &wk, Some(&m), c)
        });
        let mut w = DiffWin {
            stage: Stage::Working(other, job),
            pending: None,
        };
        let mut job = cancel_work(&mut w).expect("was working");
        assert!(matches!(w.stage, Stage::Closed));
        assert_eq!(job.state(), JobState::Cancelled);
        go.send(()).unwrap();
        job.join();
        assert!(matches!(job.poll(), Polled::Pending));
        assert_eq!(std::fs::read(&mine).unwrap(), before);
        assert!(!work.exists());
    }

    #[test]
    fn diff_job_finishes_normally() {
        let d = scratch("done");
        let (_, other) = version(&d);
        let work = d.join("ddc-collab-1-1-cmp");
        let wk = work.clone();
        let mut job = Job::spawn(work.clone(), move |c| worker(&other, &wk, None, c));
        let end = Instant::now() + Duration::from_secs(30);
        let got = loop {
            match job.poll() {
                Polled::Done(v) => break v,
                Polled::Lost => panic!("worker lost"),
                Polled::Pending => {
                    assert!(Instant::now() < end, "compare job timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        assert_eq!(job.state(), JobState::Done);
        let (_, copy, label) = got.expect("readable copy");
        assert!(copy.is_some_and(|p| p.starts_with(&work) && p.is_file()));
        assert_eq!(label, None);
    }

    fn el(kind: DiffKind, table: &str, id: i64, changes: &[(&str, FieldGroup, &str, &str)]) -> ElementDiff {
        let changes: Vec<FieldChange> = changes
            .iter()
            .map(|(f, g, o, n)| FieldChange {
                field: f.to_string(),
                group: *g,
                old: Some(o.to_string()),
                new: Some(n.to_string()),
            })
            .collect();
        ElementDiff {
            table: table.into(),
            id,
            kind,
            key: Some(format!("key-{table}-{id}")),
            other: (kind == DiffKind::Differs).then_some(id + 100),
            fields: changes.iter().map(|c| c.field.clone()).collect(),
            changes,
            spot: None,
        }
    }

    fn sample() -> VersionDiff {
        VersionDiff {
            elements: vec![
                el(DiffKind::HereOnly, "wall", 1, &[]),
                el(DiffKind::Differs, "wall", 2, &[("bx", FieldGroup::Column, "5", "6")]),
                el(DiffKind::Differs, "opening", 3, &[("prop.Fire", FieldGroup::Param, "EI30", "EI60")]),
                el(DiffKind::ThereOnly, "wall", 4, &[]),
            ],
            total: 4,
            tables: vec![],
        }
    }

    #[test]
    fn filters_narrow_by_kind_category_change_and_text() {
        let d = sample();
        let mut f = Filter::default();
        assert_eq!(visible(&d, &f), vec![0, 1, 2, 3]);
        f.kinds = [false, true, false];
        assert_eq!(visible(&d, &f), vec![1, 2]);
        f.groups = [false, false, true, false];
        assert_eq!(visible(&d, &f), vec![2], "only parameter changes");
        f = Filter {
            table: Some("wall".into()),
            ..Filter::default()
        };
        assert_eq!(visible(&d, &f), vec![0, 1, 3]);
        f = Filter {
            text: " ei60 ".into(),
            ..Filter::default()
        };
        assert_eq!(visible(&d, &f), vec![2], "a new value matches");
        f.text = "KEY-WALL-4".into();
        assert_eq!(visible(&d, &f), vec![3], "the element key matches");
        f.text = "bx".into();
        assert_eq!(visible(&d, &f), vec![1], "a field name matches");
    }

    #[test]
    fn the_tint_colours_added_and_changed_elements_of_the_list() {
        let d = sample();
        let model = [
            Key { cat: "wall", id: 1 },
            Key { cat: "wall", id: 2 },
            Key { cat: "wall", id: 9 },
            Key { cat: "door", id: 3 },
        ];
        let rows = [("added".to_string(), 0x00ff00), ("changed".to_string(), 0xffaa00)];
        let lg = tint_legend(&d, &[0, 1, 2, 3], &model, rows.clone());
        assert_eq!(lg.by, DIFF_BY);
        assert_eq!(lg.rows.len(), 2);
        assert_eq!(*lg.rows[0].keys, vec![Key { cat: "wall", id: 1 }]);
        assert_eq!(lg.rows[0].rgb, 0x00ff00);
        let changed: Vec<i64> = lg.rows[1].keys.iter().map(|k| k.id).collect();
        assert_eq!(changed.len(), 2);
        assert!(changed.contains(&2) && changed.contains(&3));
        // a filtered list tints only what it shows; an empty group has no row
        let lg = tint_legend(&d, &[1], &model, rows);
        assert_eq!(lg.rows.len(), 1);
        assert_eq!(lg.rows[0].value, "changed");
    }

    #[test]
    fn a_revision_package_compares_by_its_model() {
        use std::io::Write;
        let d = scratch("rev");
        let (_, other) = version(&d);
        let pkg = d.join("r1.ddcrev");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&pkg).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("manifest.json", o).unwrap();
        z.write_all(br#"{"format":"ddc-revision/1","label":"R7"}"#).unwrap();
        z.start_file("model.ddc", o).unwrap();
        z.write_all(&std::fs::read(&other).unwrap()).unwrap();
        z.finish().unwrap();
        let work = d.join("ddc-collab-1-2-cmp");
        let got = worker(&pkg, &work, None, &Cancel::default());
        let (_, copy, label) = got.expect("not cancelled").expect("readable revision");
        assert_eq!(label.as_deref(), Some("R7"));
        assert!(copy.is_some_and(|p| p.is_file()));
    }
}
