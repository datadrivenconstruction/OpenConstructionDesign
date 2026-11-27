// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project wall type library: types with their use counts, copy types (and their materials)
//! from another project file with a confirmed conflict rule, purge unused types into the
//! archive (never deleted, restorable) and swap the type of the selected walls.
use super::replace_type::{self, Inst};
use super::wall_type_editor;
use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use crate::units::{edit_length, unit_label_in};
use ddc_model::wall_types::{ArchivedType, ImportFate, ImportReport, ImportRow};
use ddc_model::{Align, Conflict, Model, TypePack, TypeUse};
use ddc_ui::theme::{layout, space, Role};
use ddc_ui::widgets::{self, Tone};
use ddc_ui::Theme;
use egui::{self, RichText};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use crate::ui_kit::KitScroll;

/// A file read for import, its preview and the choices made.
#[derive(Clone, Debug)]
pub struct Import {
    pub pack: TypePack,
    pub rows: Vec<ImportRow>,
    pub pick: Vec<bool>,
    pub rule: Conflict,
    pub align: Align,
}

#[derive(Clone, Debug, Default)]
pub enum Mode {
    #[default]
    List,
    Import(Import),
    /// Unused types offered for the archive, each with its tick.
    Purge(Vec<(i64, String, bool)>),
}

#[derive(Clone, Debug, Default)]
pub struct Library {
    pub mode: Mode,
    pub error: Option<String>,
    /// What the last action did.
    pub note: Option<String>,
}

/// The swap dialog: the type picked for the selected walls.
#[derive(Clone, Debug, Default)]
pub struct Swap {
    pub to: Option<i64>,
    pub error: Option<String>,
}

thread_local! {
    static LIB: RefCell<Option<Library>> = const { RefCell::new(None) };
    static SWAP: RefCell<Option<Swap>> = const { RefCell::new(None) };
}

pub fn with<R>(f: impl FnOnce(&mut Option<Library>) -> R) -> R {
    LIB.with(|o| f(&mut o.borrow_mut()))
}

pub fn with_swap<R>(f: impl FnOnce(&mut Option<Swap>) -> R) -> R {
    SWAP.with(|o| f(&mut o.borrow_mut()))
}

pub fn open_library() {
    with(|o| {
        o.get_or_insert_with(Library::default);
    });
}

pub fn open_swap() {
    with_swap(|o| *o = Some(Swap::default()));
}

fn cmd_library(_: &mut EditorApp) {
    open_library();
}

fn cmd_swap(_: &mut EditorApp) {
    open_swap();
}

pub fn register(app: &mut EditorApp) {
    app.register_command("wall_types.library", cmd_library);
    app.register_command("wall_types.swap", cmd_swap);
}

/// Windows of the library, the swap dialog and the wall type editor; every frame.
pub fn windows(ctx: &egui::Context, app: &mut EditorApp) {
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    library_window(ctx, &app.sh.theme, ru, &mut app.ed);
    swap_window(ctx, &app.sh.theme, ru, &mut app.ed);
    wall_type_editor::show(ctx, &app.sh.theme, &app.sh.cat, ru, &mut app.ed);
}

/// Runs `f` as one undo step and hands back what it returned.
fn run<T: Send + 'static>(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<T, ddc_model::Error> + Send + Sync + 'static,
) -> Result<T, String> {
    let out: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let o2 = out.clone();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let v = f(m)?;
        if let Ok(mut g) = o2.lock() {
            *g = Some(v);
        }
        Ok(())
    })))
    .map_err(|e| e.to_string())?;
    let v = out.lock().ok().and_then(|mut g| g.take());
    v.ok_or_else(|| "no result".to_string())
}

/// Reads a project file and plans its import (nothing is written).
pub fn load_pack(m: &Model, path: &std::path::Path) -> Result<Import, String> {
    let pack = ddc_model::read_type_pack(path).map_err(|e| e.to_string())?;
    let rows = m.plan_type_import(&pack).map_err(|e| e.to_string())?;
    // a same-named type with other layers is left alone until the rule says otherwise
    let pick = rows.iter().map(|r| r.fate != ImportFate::Same).collect();
    Ok(Import {
        pack,
        rows,
        pick,
        rule: Conflict::Rename,
        align: Align::Core,
    })
}

impl Import {
    pub fn picked(&self) -> Vec<String> {
        self.rows
            .iter()
            .zip(&self.pick)
            .filter(|(_, p)| **p)
            .map(|(r, _)| r.name.clone())
            .collect()
    }

    pub fn conflicts(&self) -> usize {
        self.rows
            .iter()
            .zip(&self.pick)
            .filter(|(r, p)| **p && r.fate == ImportFate::Conflict)
            .count()
    }

    /// Placed walls an overwrite would resize.
    pub fn walls_hit(&self) -> usize {
        if self.rule != Conflict::Overwrite {
            return 0;
        }
        self.rows
            .iter()
            .zip(&self.pick)
            .filter(|(r, p)| **p && r.fate == ImportFate::Conflict)
            .map(|(r, _)| r.walls)
            .sum()
    }
}

/// Copies the picked types in one undo step.
pub fn run_import(ed: &mut Editor, im: &Import) -> Result<ImportReport, String> {
    let picked = im.picked();
    if picked.is_empty() {
        return Err("nothing picked".into());
    }
    let (pack, rule, align) = (im.pack.clone(), im.rule, im.align);
    run(ed, move |m| m.import_wall_types(&pack, &picked, rule, align))
}

pub fn import_summary(r: &ImportReport, ru: bool) -> String {
    let n = |v: usize| v.to_string();
    crate::tr_lang(
        ru,
        "wall_type.lib.imported",
        &[
            ("added", n(r.added.len()).as_str()),
            ("renamed", n(r.renamed.len()).as_str()),
            ("overwritten", n(r.overwritten.len()).as_str()),
            ("skipped", n(r.skipped.len() + r.same.len()).as_str()),
            ("materials", n(r.materials.len()).as_str()),
        ],
    )
}

/// Moves the ticked types into the archive (one undo step); their names.
pub fn run_purge(ed: &mut Editor, ids: Vec<i64>) -> Result<Vec<String>, String> {
    if ids.is_empty() {
        return Err("nothing picked".into());
    }
    run(ed, move |m| m.archive_wall_types(&ids))
}

pub fn run_restore(ed: &mut Editor, archive_id: i64) -> Result<i64, String> {
    run(ed, move |m| m.restore_archived_wall_type(archive_id))
}

/// Selected walls as instances for the type swap.
pub fn selected_walls(ed: &Editor) -> Vec<Inst> {
    ed.selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(id) => Some(Inst {
                table: "wall",
                id: *id,
                level: ed.level,
            }),
            _ => None,
        })
        .collect()
}

/// (walls that change, walls selected) for swapping to `to`.
pub fn swap_counts(ed: &Editor, to: i64) -> (usize, usize) {
    let sel = selected_walls(ed);
    let ids: Vec<i64> = sel.iter().map(|i| i.id).collect();
    let n = ed.walls.iter().filter(|w| ids.contains(&w.id) && w.wall_type != to).count();
    (n, sel.len())
}

/// Swaps the type of the selected walls in one undo step; the walls stay selected.
pub fn apply_swap(ed: &mut Editor, to: i64, ru: bool) -> Result<usize, String> {
    let sel = selected_walls(ed);
    let edits = replace_type::type_edits(&ed.model, &sel, to)?;
    let n = edits.len();
    if n == 0 {
        return Ok(0);
    }
    ed.apply_xlsx(edits, "")?;
    ed.selection = replace_type::as_selection(&sel);
    let (a, b) = (n.to_string(), sel.len().to_string());
    ed.status = Some(crate::tr_lang(ru, "wall_type.swap.done", &[("n", a.as_str()), ("m", b.as_str())]));
    Ok(n)
}

fn muted(ui: &mut egui::Ui, th: &Theme, s: &str) {
    ui.label(
        RichText::new(s)
            .font(th.font(Role::Label))
            .color(th.c("text.secondary")),
    );
}

fn danger(ui: &mut egui::Ui, th: &Theme, s: &str) {
    ui.label(RichText::new(s).font(th.font(Role::Label)).color(th.c("danger")));
}

/// What the list view asked for this frame.
enum Ask {
    Edit(i64),
    Import,
    Purge,
    Restore(i64),
}

fn type_row(ui: &mut egui::Ui, th: &Theme, ru: bool, t: &TypeUse, u: crate::units::Units) -> bool {
    let mut edit = false;
    ui.horizontal(|ui| {
        let thick = format!("{} {}", edit_length(t.thickness, u), unit_label_in(u, crate::units::lang_of(ru)));
        let name_w = ui.available_width() - layout::PROP_VALUE * 2.5;
        ui.allocate_ui_with_layout(
            egui::vec2(name_w, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(name_w);
                kit::name_with_tip(ui, &t.name, &t.name, false);
            },
        );
        let n = t.walls.to_string();
        let uses = crate::tr_lang(ru, "wall_type.lib.walls_n", &[("n", n.as_str())]);
        let tone = if t.walls == 0 { Tone::Neutral } else { Tone::Accent };
        widgets::badge(ui, th, &uses, tone);
        ui.label(RichText::new(thick).font(th.font(Role::Numeric)));
        let b = widgets::Button::quiet(crate::tr_str(ru, "wall_type.lib.edit")).small().show(ui, th);
        edit = b.clicked();
    });
    edit
}

fn list_view(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    ed: &Editor,
    used: &[TypeUse],
    archive: &[ArchivedType],
) -> Option<Ask> {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let mut ask = None;
    ui.horizontal(|ui| {
        if widgets::Button::secondary(tr("wall_type.lib.import")).show(ui, th).clicked() {
            ask = Some(Ask::Import);
        }
        let b = widgets::Button::secondary(tr("wall_type.lib.purge"))
            .tooltip(tr("wall_type.lib.purge_tip"), "")
            .show(ui, th);
        if b.clicked() {
            ask = Some(Ask::Purge);
        }
    });
    ui.add_space(space::SM);
    let total: usize = used.iter().map(|t| t.walls).sum();
    let (a, b) = (used.len().to_string(), total.to_string());
    muted(ui, th, &crate::tr_lang(ru, "wall_type.lib.summary", &[("n", a.as_str()), ("w", b.as_str())]));
    if used.is_empty() {
        widgets::empty_state_compact(ui, th, tr("wall_type.lib.empty"), tr("wall_type.lib.empty_hint"));
    }
    egui::ScrollArea::vertical()
        .id_salt("ag3.lib.types")
        .max_height(layout::DIALOG_TOOL_H / 2.0)
        .kit_show(ui, |ui| {
            for t in used {
                if type_row(ui, th, ru, t, ed.tool.units) {
                    ask = Some(Ask::Edit(t.id));
                }
            }
        });
    if !archive.is_empty() {
        ui.add_space(space::SM);
        ui.label(RichText::new(tr("wall_type.lib.archive")).font(th.font(Role::Section)));
        for a in archive {
            ui.horizontal(|ui| {
                kit::name_with_tip(ui, &a.name, &a.at, false);
                let b = widgets::Button::quiet(tr("wall_type.lib.restore")).small().show(ui, th);
                if b.clicked() {
                    ask = Some(Ask::Restore(a.id));
                }
            });
        }
    }
    ask
}

fn fate_label(ru: bool, f: ImportFate) -> &'static str {
    crate::tr_str(
        ru,
        match f {
            ImportFate::New => "wall_type.lib.fate_new",
            ImportFate::Same => "wall_type.lib.fate_same",
            ImportFate::Conflict => "wall_type.lib.fate_conflict",
        },
    )
}

/// The import preview; true: confirmed.
fn import_view(ui: &mut egui::Ui, th: &Theme, ru: bool, im: &mut Import) -> (bool, bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let src = std::path::Path::new(&im.pack.source)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    kit::labeled_row(ui, tr("wall_type.lib.from"), |ui| {
        kit::name_with_tip(ui, &src, &im.pack.source, true);
    });
    if im.rows.is_empty() {
        widgets::empty_state_compact(ui, th, tr("wall_type.lib.no_types"), tr("wall_type.lib.no_types_hint"));
    }
    egui::ScrollArea::vertical()
        .id_salt("wall_type.lib.import")
        .max_height(layout::DIALOG_TOOL_H / 2.0)
        .kit_show(ui, |ui| {
            for (r, p) in im.rows.iter().zip(im.pick.iter_mut()) {
                ui.horizontal(|ui| {
                    widgets::checkbox(ui, th, p, &r.name);
                    let tone = match r.fate {
                        ImportFate::New => Tone::Success,
                        ImportFate::Same => Tone::Neutral,
                        ImportFate::Conflict => Tone::Warning,
                    };
                    widgets::badge(ui, th, fate_label(ru, r.fate), tone);
                });
            }
        });
    let conflicts = im.conflicts();
    if conflicts > 0 {
        kit::labeled_row(ui, tr("wall_type.lib.on_conflict"), |ui| {
            let items = [tr("wall_type.lib.rename"), tr("wall_type.lib.skip"), tr("wall_type.lib.overwrite")];
            let at = match im.rule {
                Conflict::Rename => 0,
                Conflict::Skip => 1,
                Conflict::Overwrite => 2,
            };
            if let Some(i) = widgets::segmented(ui, th, &items, at) {
                im.rule = [Conflict::Rename, Conflict::Skip, Conflict::Overwrite][i];
            }
        });
        let hit = im.walls_hit();
        if hit > 0 {
            let n = hit.to_string();
            kit::warn_note(ui, crate::tr_lang(ru, "wall_type.lib.overwrite_walls", &[("n", n.as_str())]));
        }
    }
    let picked = im.pick.iter().filter(|p| **p).count();
    let (a, b) = (picked.to_string(), im.rows.len().to_string());
    muted(ui, th, &crate::tr_lang(ru, "wall_type.lib.picked", &[("n", a.as_str()), ("m", b.as_str())]));
    let (mut ok, mut back) = (false, false);
    kit::dialog_footer(ui, |ui| {
        let why = (picked == 0).then(|| tr("wall_type.lib.pick_some"));
        ok = kit::primary_action(ui, tr("wall_type.lib.copy_n"), why);
        back = kit::cancel_button(ui, tr("wall_type.lib.back")).clicked();
    });
    (ok, back)
}

/// The purge confirmation; (confirmed, back).
fn purge_view(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    list: &mut [(i64, String, bool)],
) -> (bool, bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    if list.is_empty() {
        widgets::empty_state_compact(ui, th, tr("wall_type.lib.nothing_unused"), tr("wall_type.lib.last_kept"));
    } else {
        muted(ui, th, tr("wall_type.lib.purge_note"));
    }
    egui::ScrollArea::vertical()
        .id_salt("wall_type.lib.purge")
        .max_height(layout::DIALOG_TOOL_H / 2.0)
        .kit_show(ui, |ui| {
            for (_, name, on) in list.iter_mut() {
                widgets::checkbox(ui, th, on, name);
            }
        });
    let n = list.iter().filter(|t| t.2).count();
    let (mut ok, mut back) = (false, false);
    kit::dialog_footer(ui, |ui| {
        let why = (n == 0).then(|| tr("wall_type.lib.pick_some"));
        let k = n.to_string();
        let label = crate::tr_lang(ru, "wall_type.lib.archive_n", &[("n", k.as_str())]);
        ok = kit::primary_action(ui, label, why);
        back = kit::cancel_button(ui, tr("wall_type.lib.back")).clicked();
    });
    (ok, back)
}

pub fn library_window(ctx: &egui::Context, th: &Theme, ru: bool, ed: &mut Editor) {
    let Some(mut lib) = with(|o| o.take()) else {
        return;
    };
    let keep = library_frame(ctx, th, ru, ed, &mut lib);
    if keep {
        with(|o| *o = Some(lib));
    }
}

/// One frame of the library; false: it closed.
pub fn library_frame(
    ctx: &egui::Context,
    th: &Theme,
    ru: bool,
    ed: &mut Editor,
    lib: &mut Library,
) -> bool {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let used = ed.model.wall_type_usage().unwrap_or_default();
    let archive = ed.model.archived_wall_types().unwrap_or_default();
    let mut open = true;
    let (mut ask, mut confirm, mut back) = (None, false, false);
    kit::Dialog::new("wall_type_library", tr("wall_type.lib.title"), kit::DialogSize::Tool)
        .open(&mut open)
        .show(ctx, |ui| {
            match &mut lib.mode {
                Mode::List => ask = list_view(ui, th, ru, ed, &used, &archive),
                Mode::Import(im) => (confirm, back) = import_view(ui, th, ru, im),
                Mode::Purge(list) => (confirm, back) = purge_view(ui, th, ru, list),
            }
            if let Some(n) = &lib.note {
                muted(ui, th, n);
            }
            if let Some(e) = &lib.error {
                danger(ui, th, e);
            }
        });
    if back {
        lib.mode = Mode::List;
    }
    if confirm {
        lib.error = None;
        match std::mem::take(&mut lib.mode) {
            Mode::Import(im) => match run_import(ed, &im) {
                Ok(r) => lib.note = Some(import_summary(&r, ru)),
                Err(e) => {
                    lib.error = Some(e);
                    lib.mode = Mode::Import(im);
                }
            },
            Mode::Purge(list) => {
                let ids = list.iter().filter(|t| t.2).map(|t| t.0).collect();
                match run_purge(ed, ids) {
                    Ok(names) => {
                        let n = names.len().to_string();
                        let msg = crate::tr_lang(ru, "wall_type.lib.archived", &[("n", n.as_str())]);
                        lib.note = Some(msg);
                    }
                    Err(e) => {
                        lib.error = Some(e);
                        lib.mode = Mode::Purge(list);
                    }
                }
            }
            Mode::List => {}
        }
    }
    match ask {
        Some(Ask::Edit(id)) => {
            if !wall_type_editor::open(&ed.model, id, ed.tool.units) {
                lib.note = Some(tr("wall_type.lib.editor_busy").into());
            }
        }
        Some(Ask::Import) => {
            let path = crate::file_pick::Dlg::new()
                .set_title(tr("wall_type.lib.pick_file"))
                .add_filter("DDC", &["ddc"])
                .pick_file();
            if let Some(p) = path {
                match load_pack(&ed.model, &p) {
                    Ok(im) => {
                        lib.error = None;
                        lib.mode = Mode::Import(im);
                    }
                    Err(e) => lib.error = Some(e),
                }
            }
        }
        Some(Ask::Purge) => {
            let list = ed.model.archivable_wall_types().unwrap_or_default();
            lib.mode = Mode::Purge(list.into_iter().map(|(id, n)| (id, n, true)).collect());
        }
        Some(Ask::Restore(id)) => match run_restore(ed, id) {
            Ok(_) => lib.note = Some(tr("wall_type.lib.restored").into()),
            Err(e) => lib.error = Some(e),
        },
        None => {}
    }
    open
}

pub fn swap_window(ctx: &egui::Context, th: &Theme, ru: bool, ed: &mut Editor) {
    let Some(mut s) = with_swap(|o| o.take()) else {
        return;
    };
    let keep = swap_frame(ctx, th, ru, ed, &mut s);
    if keep {
        with_swap(|o| *o = Some(s));
    }
}

/// One frame of the swap dialog; false: it closed.
pub fn swap_frame(
    ctx: &egui::Context,
    th: &Theme,
    ru: bool,
    ed: &mut Editor,
    s: &mut Swap,
) -> bool {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let types: Vec<(i64, String)> = ed.types.iter().map(|t| (t.id, t.name.clone())).collect();
    let selected = selected_walls(ed).len();
    let (mut open, mut ok, mut cancel) = (true, false, false);
    kit::Dialog::new("wall_type_swap", tr("wall_type.swap.title"), kit::DialogSize::Form)
        .open(&mut open)
        .show(ctx, |ui| {
            if selected == 0 {
                widgets::empty_state_compact(ui, th, tr("wall_type.swap.none"), tr("wall_type.swap.none_hint"));
            } else {
                kit::labeled_row(ui, tr("wall_type.swap.to"), |ui| {
                    let mut names: Vec<&str> = vec![tr("wall_type.swap.pick")];
                    names.extend(types.iter().map(|t| t.1.as_str()));
                    let mut sel = s
                        .to
                        .and_then(|id| types.iter().position(|t| t.0 == id))
                        .map_or(0, |i| i + 1);
                    let w = Some(layout::PROP_VALUE * 2.5);
                    if widgets::dropdown(ui, th, "wall_type.swap.to", &mut sel, &names, w).changed() {
                        s.to = sel.checked_sub(1).and_then(|i| types.get(i)).map(|t| t.0);
                    }
                });
                let (n, m) = s.to.map_or((0, selected), |to| swap_counts(ed, to));
                let (a, b) = (n.to_string(), m.to_string());
                let args = [("n", a.as_str()), ("m", b.as_str())];
                let pv = crate::tr_lang(ru, "wall_type.swap.preview", &args);
                kit::labeled_row(ui, tr("wall_type.swap.will_change"), |ui| {
                    ui.label(RichText::new(pv).font(th.font(Role::Numeric)));
                });
            }
            if let Some(e) = &s.error {
                danger(ui, th, e);
            }
            kit::dialog_footer(ui, |ui| {
                let why = if selected == 0 {
                    Some(tr("wall_type.swap.none"))
                } else if s.to.is_none() {
                    Some(tr("wall_type.swap.pick"))
                } else if s.to.is_some_and(|to| swap_counts(ed, to).0 == 0) {
                    Some(tr("wall_type.swap.already"))
                } else {
                    None
                };
                ok = kit::primary_action(ui, tr("wall_type.swap.apply"), why);
                cancel = kit::cancel_button(ui, tr("wall_type.wt.cancel")).clicked();
            });
        });
    if ok {
        if let Some(to) = s.to {
            match apply_swap(ed, to, ru) {
                Ok(_) => return false,
                Err(e) => s.error = Some(e),
            }
        }
    }
    open && !cancel
}

/// Which view the layout probe draws.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Case {
    List,
    Import,
    Purge,
    Swap,
}

#[cfg(test)]
pub(crate) fn source_file(tag: &str) -> std::path::PathBuf {
    use ddc_model::{LayerFn, LayerSpec, TypeLayer};
    let ns = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let p = std::env::temp_dir().join(format!("ddc-ag3-lib-{tag}-{ns}.ddc"));
    let mut s = Model::open(p.to_str().unwrap()).unwrap();
    let ls = vec![
        TypeLayer::new("render", 0.02, LayerFn::Finish1),
        TypeLayer::new("aerated block", 0.3, LayerFn::Structure),
    ];
    let specs: Vec<LayerSpec> = ls.iter().map(TypeLayer::spec).collect();
    let a = s.add_wall_type("Ext 320", &specs).unwrap();
    s.edit_wall_type(a, "Ext 320", &ls, Align::Core).unwrap();
    drop(s);
    p
}

/// The library drawn headless in an explicit language, for the layout contract.
#[cfg(test)]
pub(crate) fn layout_probe(ru: bool, case: Case) -> crate::layout_contract::Probe {
    let mut ed = wall_type_editor::test_editor();
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    if case == Case::Swap {
        ed.selection = ed.walls.iter().map(|w| Sel::Wall(w.id)).collect();
        let mut s = Swap {
            to: ed.types.last().map(|t| t.id),
            error: None,
        };
        return crate::layout_contract::probe("wall_type_swap", |ctx| {
            swap_frame(ctx, &th, ru, &mut ed, &mut s);
        });
    }
    let mode = match case {
        Case::Import => Mode::Import(load_pack(&ed.model, &source_file("probe")).unwrap()),
        Case::Purge => {
            let list = ed.model.archivable_wall_types().unwrap();
            // seed names follow the global language: neutral ones keep data out of the check
            Mode::Purge(list.into_iter().map(|(id, _)| (id, format!("Type {id}"), true)).collect())
        }
        _ => Mode::List,
    };
    let mut lib = Library {
        mode,
        ..Default::default()
    };
    crate::layout_contract::probe("wall_type_library", |ctx| {
        library_frame(ctx, &th, ru, &mut ed, &mut lib);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::panels::wall_type_editor::test_editor;

    fn n_types(ed: &Editor) -> usize {
        ed.model.wall_type_usage().unwrap().len()
    }

    #[test]
    fn import_preview_then_one_undo_step() {
        let mut ed = test_editor();
        let p = source_file("imp");
        let mut im = load_pack(&ed.model, &p).unwrap();
        assert!(im.rows.iter().any(|r| r.name == "Ext 320" && r.fate == ImportFate::New));
        let before = n_types(&ed);
        let undo0 = ed.can_undo();
        let r = run_import(&mut ed, &im).unwrap();
        assert_eq!(r.added, vec!["Ext 320".to_string()]);
        assert_eq!(n_types(&ed), before + 1);
        // the same file again: no conflict, nothing to write
        im = load_pack(&ed.model, &p).unwrap();
        assert!(im.rows.iter().all(|r| r.fate == ImportFate::Same));
        assert!(im.picked().is_empty());
        ed.undo();
        assert_eq!(n_types(&ed), before);
        assert_eq!(ed.can_undo(), undo0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn conflicts_follow_the_rule_and_count_resized_walls() {
        let mut ed = test_editor();
        let id = ed.walls[0].wall_type;
        let p = source_file("conf");
        run(&mut ed, move |m| m.rename_wall_type(id, "Ext 320")).unwrap();
        let mut im = load_pack(&ed.model, &p).unwrap();
        assert_eq!(im.conflicts(), 1);
        assert_eq!(im.walls_hit(), 0, "rename touches no placed wall");
        im.rule = Conflict::Overwrite;
        assert_eq!(im.walls_hit(), 2);
        im.rule = Conflict::Rename;
        let r = run_import(&mut ed, &im).unwrap();
        assert_eq!(r.renamed.len(), 1);
        assert_ne!(r.renamed[0].1, "Ext 320");
        ed.undo();
        im.rule = Conflict::Overwrite;
        let r = run_import(&mut ed, &im).unwrap();
        assert_eq!(r.overwritten, vec!["Ext 320".to_string()]);
        assert_eq!(r.walls, 2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn purge_archives_and_restore_brings_back() {
        let mut ed = test_editor();
        let unused = ed.model.archivable_wall_types().unwrap();
        assert!(!unused.is_empty());
        let used = ed.walls[0].wall_type;
        assert!(unused.iter().all(|t| t.0 != used));
        let n0 = n_types(&ed);
        let names = run_purge(&mut ed, vec![unused[0].0]).unwrap();
        assert_eq!(names, vec![unused[0].1.clone()]);
        assert_eq!(n_types(&ed), n0 - 1);
        let arch = ed.model.archived_wall_types().unwrap();
        assert_eq!(arch.len(), 1);
        run_restore(&mut ed, arch[0].id).unwrap();
        assert_eq!(n_types(&ed), n0);
        assert!(ed.model.archived_wall_types().unwrap().is_empty());
        assert!(run_purge(&mut ed, vec![]).is_err());
    }

    #[test]
    fn swap_counts_and_one_undo_step() {
        let mut ed = test_editor();
        let first = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(first)];
        let from = ed.walls[0].wall_type;
        let to = ed.types.iter().find(|t| t.id != from).unwrap().id;
        assert_eq!(swap_counts(&ed, to), (1, 1));
        assert_eq!(swap_counts(&ed, from), (0, 1));
        let undo0 = ed.can_undo();
        assert_eq!(apply_swap(&mut ed, to, false).unwrap(), 1);
        assert_eq!(ed.walls.iter().find(|w| w.id == first).unwrap().wall_type, to);
        assert!(ed.walls.iter().any(|w| w.id != first && w.wall_type == from));
        assert_eq!(ed.selection, vec![Sel::Wall(first)]);
        ed.undo();
        assert_eq!(ed.can_undo(), undo0);
        assert_eq!(ed.walls.iter().find(|w| w.id == first).unwrap().wall_type, from);
    }
}
