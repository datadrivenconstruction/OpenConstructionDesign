//! Collaboration: merging with a colleague's file, version comparison, project options.
//! The logic is in `ddc_model` (`merge`, `compare`, `options`); this module holds windows, background jobs and display.

pub mod diff_view;
mod job;
mod merge_win;
pub(crate) mod options_win;
mod workdir;

use std::collections::HashSet;
use std::path::PathBuf;

use ddc_model::HiddenByOption;
use ddc_ui::i18n::Lang;

use crate::grips::Kind;
use crate::isolate::Part;
use crate::shell::EditorApp;
pub(crate) use workdir::work_dir;

#[derive(Default)]
pub struct State {
    merge: merge_win::MergeWin,
    diff: diff_view::DiffWin,
    options: options_win::OptionsWin,
}

pub fn register(a: &mut EditorApp) {
    a.register_command("data.merge_file", merge_win::start);
    a.register_command("data.compare_version", diff_view::start);
    a.register_command("mgr.design_options", options_win::open);
}

/// What a collab window shows right now (read-only view for e2e runs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinProbe {
    Closed,
    Working,
    /// Refused or failed, with the worded reason.
    Refused(String),
    /// Result on screen: the other file, the conflicts (merge) or differences (compare).
    Shown { source: PathBuf, count: usize },
}

/// State of the merge window and of the compare window.
pub fn probe(a: &EditorApp) -> (WinProbe, WinProbe) {
    (merge_win::probe(&a.collab.merge), diff_view::probe(&a.collab.diff))
}

pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = std::mem::take(&mut a.collab);
    merge_win::frame(&mut st.merge, a, ctx);
    diff_view::frame(&mut st.diff, a, ctx);
    options_win::frame(&mut st.options, a, ctx);
    a.collab = st;
    crate::reveal::controls(a, ctx);
}

fn pick_project(a: &EditorApp, title: &str) -> Option<PathBuf> {
    crate::file_pick::Dlg::new()
        .set_title(a.sh.cat.t(title))
        .add_filter("DDC", &["ddc"])
        .pick_file()
}

fn file_label(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn kind_label(a: &EditorApp, table: &str) -> String {
    let ru = a.sh.cat.lang == Lang::Ru;
    crate::phase_ui::kind_name(table, ru).to_string()
}

/// A number in a table column: right-aligned.
fn num(ui: &mut egui::Ui, n: usize) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(n.to_string());
    });
}

/// Long text: truncated, with the full version in a tooltip.
fn elided(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(text).truncate())
        .on_hover_text(text);
}

/// Parts without `Sel` (columns, roofs...) hidden by options.
pub fn option_parts(hidden: &HiddenByOption) -> HashSet<Part> {
    let kinds = [
        ("wall", Kind::Wall),
        ("slab", Kind::Slab),
        ("ceiling", Kind::Ceiling),
        ("roof", Kind::Roof),
        ("column", Kind::Column),
        ("beam", Kind::Beam),
        ("stair", Kind::Stair),
        ("railing", Kind::Railing),
        ("comp", Kind::Comp),
        ("mep_run", Kind::Mep),
    ];
    let mut out = HashSet::new();
    for (t, k) in kinds {
        if let Some(ids) = hidden.get(t) {
            out.extend(ids.iter().map(|i| (k, *i)));
        }
    }
    out
}

/// Select a model element by table and id, if it is among the selectable ones.
fn select(a: &mut EditorApp, table: &str, id: i64) -> bool {
    let found = crate::selection::all(&a.ed)
        .into_iter()
        .find(|s| crate::phase_ui::sel_ref(*s) == Some((table, id)));
    if let Some(s) = found {
        a.ed.selection = vec![s];
    }
    found.is_some()
}
