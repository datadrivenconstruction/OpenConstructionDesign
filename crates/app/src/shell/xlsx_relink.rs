// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The linked workbook is kept per project in `meta`, so a reopened project still knows it, and
//! a book that was renamed or moved is found again safely. "Synchronize" on a link whose file is
//! gone opens a card (never the export offer): a book in the old folder (or one level below)
//! that names this project and carries the same export fingerprint is offered by name, never
//! linked by itself; "Find it" lets the user pick the book, and the pick goes through the normal
//! preview read, where a book of another project is refused (xlsx_project).
use super::xlsx_project::model_uid;
use super::xlsx_recover::{interrupted, Leftover};
use super::EditorApp;
use crate::ui_kit as kit;
use calamine::{open_workbook, Reader, Xlsx};
use ddc_model::Model;
use ddc_ui::widgets::Tone;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use web_time::{Duration, Instant, SystemTime};

/// Link state, not model content: the `view:` prefix keeps it out of model comparisons.
pub const KEY: &str = "view:xlsx.link";

/// Books looked at in the old folder and its subfolders; a larger folder needs "Find it".
pub const MAX_LOOK: usize = 64;

/// A spreadsheet program saving the book can leave its file away for a moment: only a book
/// gone this long counts as moved.
pub const GRACE: Duration = Duration::from_secs(3);

/// The project's linked book as last remembered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub path: PathBuf,
    /// Project uid the book names (Info "project"); None for a book naming none.
    pub project: Option<i64>,
    /// Fingerprint of the book's Info sheet; None when the book could not be read.
    pub id: Option<u64>,
}

/// Project uid and fingerprint of the book's Info sheet (units, export revision, scope,
/// project, ...). Export writes the sheet and cell edits leave it alone, so the fingerprint
/// names one export across renames, moves and edits. None: no readable book or no Info.
pub fn book_id(path: &Path) -> Option<(Option<i64>, u64)> {
    let mut wb: Xlsx<_> = open_workbook(path).ok()?;
    // io's Info sheet name (ddc_io::xlsx keeps the constant crate-private)
    let info = wb.worksheet_range("Info").ok()?;
    let mut project = None;
    // FNV-1a: stable across builds, the value is stored in the project
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for r in info.rows() {
        let k = r.first().map(|d| d.to_string()).unwrap_or_default();
        let v = r.get(1).map(|d| d.to_string()).unwrap_or_default();
        if k == "project" {
            project = v.trim().parse().ok();
        }
        for b in k.bytes().chain([0x1f]).chain(v.bytes()).chain([0x1e]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    Some((project, h))
}

/// Project uid and Info fingerprint of a book, as [`book_id`] reads them.
pub type BookId = (Option<i64>, u64);

/// What a cached fingerprint holds for: the file's modified time and size.
pub type Stamp = (SystemTime, u64);

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Fingerprints by path, each valid while the file keeps its stamp. Opening a book parses its
/// whole package, so a link write or a repeated look reads each unchanged book once. The lock
/// is never held during a read: a link write never waits behind a background search.
pub struct IdCache(Mutex<BTreeMap<PathBuf, (Stamp, BookId)>>);

impl IdCache {
    pub const CAP: usize = 256;

    pub const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<PathBuf, (Stamp, BookId)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cached id for `path` at `stamp`, else `read` once. No stamp (no file): no caching.
    pub fn get(&self, path: &Path, stamp: Option<Stamp>, read: impl FnOnce(&Path) -> Option<BookId>) -> Option<BookId> {
        let Some(st) = stamp else {
            return read(path);
        };
        let hit = self.map().get(path).filter(|(s, _)| *s == st).map(|(_, id)| *id);
        if hit.is_some() {
            return hit;
        }
        let got = read(path);
        let mut m = self.map();
        match got {
            Some(id) => {
                if m.len() >= Self::CAP && !m.contains_key(path) {
                    m.clear();
                }
                m.insert(path.to_path_buf(), (st, id));
            }
            // unreadable now (locked while saved): read again next time
            None => {
                m.remove(path);
            }
        }
        got
    }
}

static IDS: IdCache = IdCache::new();

/// [`book_id`], read once per path and stamp (shared by the UI thread and the workers).
pub fn book_id_cached(path: &Path) -> Option<BookId> {
    // stamped before the read: a save during the read gives a new stamp, read again next time
    IDS.get(path, stamp(path), book_id)
}

/// Called by the workers right after they read or wrote `path`: the link write that follows on
/// the UI thread then finds the fingerprint cached.
pub fn warm(path: &Path) {
    let _ = book_id_cached(path);
}

/// Remembers `path` as the project's linked book. A book that cannot be read now (locked)
/// keeps the fingerprint already stored for the same path.
pub fn store(model: &Model, path: &Path) {
    let (project, id) = match book_id_cached(path) {
        Some((p, h)) => (p, Some(h)),
        None => match stored(model).filter(|l| l.path.as_path() == path) {
            Some(l) => (l.project, l.id),
            None => (None, None),
        },
    };
    let v = serde_json::json!({
        "path": path.to_string_lossy(),
        "project": project,
        "id": id.map(|h| format!("{h:016x}")),
    })
    .to_string();
    if let Err(e) = model
        .sql()
        .execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", [KEY, v.as_str()])
    {
        eprintln!("xlsx link not stored: {e}");
    }
}

/// The linked book this project remembers; None when it never had one.
pub fn stored(model: &Model) -> Option<Link> {
    let s: String = model
        .sql()
        .query_row("SELECT value FROM meta WHERE key=?1", [KEY], |r| r.get(0))
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let path = PathBuf::from(v.get("path")?.as_str()?);
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(Link {
        path,
        project: v.get("project").and_then(|p| p.as_i64()),
        id: v.get("id").and_then(|x| x.as_str()).and_then(|x| u64::from_str_radix(x, 16).ok()),
    })
}

/// "Synchronize" target after a reopen: the stored link; the delete flag always starts off.
pub fn stored_target(model: &Model) -> Option<(PathBuf, bool)> {
    stored(model).map(|l| (l.path, false))
}

fn is_book(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
        && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("~$"))
        && !p.to_string_lossy().ends_with(".export-part.xlsx")
        && p.is_file()
}

/// Books in the link's old folder and its direct subfolders that name this project and carry
/// the link's export fingerprint, sorted. Never linked by themselves: the user picks.
pub fn candidates(model_uid: Option<i64>, link: &Link) -> Vec<PathBuf> {
    candidates_until(model_uid, link, &AtomicBool::new(false))
}

/// [`candidates`] that stops before the next book once `stop` is set.
pub fn candidates_until(model_uid: Option<i64>, link: &Link, stop: &AtomicBool) -> Vec<PathBuf> {
    let (Some(uid), Some(id)) = (model_uid, link.id) else {
        return vec![];
    };
    // a link that names another project offers nothing of it
    if link.project.is_some_and(|p| p != uid) {
        return vec![];
    }
    let Some(dir) = link.path.parent().filter(|d| d.is_dir()) else {
        return vec![];
    };
    let list = |d: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(d)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        v.sort();
        v
    };
    let top = list(dir);
    let subs = top.iter().filter(|p| p.is_dir()).flat_map(|d| list(d.as_path()));
    let mut found: Vec<PathBuf> = top
        .iter()
        .cloned()
        .chain(subs)
        .filter(|p| *p != link.path && is_book(p))
        .take(MAX_LOOK)
        .take_while(|_| !stop.load(Ordering::Relaxed))
        .filter(|p| book_id_cached(p) == Some((Some(uid), id)))
        .collect();
    found.sort();
    found
}

/// One step of the vanished-file watch: true once `path` has been missing for `GRACE`.
pub fn gone_step(seen: &mut Option<(PathBuf, Instant)>, path: &Path, exists: bool, now: Instant) -> bool {
    if exists {
        *seen = None;
        return false;
    }
    if let Some((p, t)) = seen.as_ref() {
        if p.as_path() == path {
            return now.duration_since(*t) >= GRACE;
        }
    }
    *seen = Some((path.to_path_buf(), now));
    false
}

/// The look near the old place, on a worker: the card opens at once and shows "searching"
/// until the result is in. Another project or a closed card stops it.
struct Search {
    old: PathBuf,
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<(Vec<PathBuf>, Option<Leftover>)>,
}

thread_local! {
    static GONE: RefCell<Option<(PathBuf, Instant)>> = const { RefCell::new(None) };
    static CARD: RefCell<Option<Missing>> = const { RefCell::new(None) };
    static SEARCH: RefCell<Option<Search>> = const { RefCell::new(None) };
    /// What an interrupted export left beside the card's missing book.
    static LEFT: RefCell<Option<Leftover>> = const { RefCell::new(None) };
}

/// The interrupted export's leftovers the search found for the open card.
pub fn leftover() -> Option<Leftover> {
    LEFT.with(|l| l.borrow().clone())
}

fn stop_search() {
    if let Some(s) = SEARCH.with(|s| s.borrow_mut().take()) {
        s.stop.store(true, Ordering::Relaxed);
    }
}

fn start_search(uid: Option<i64>, link: Link) {
    stop_search();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let (s, old) = (stop.clone(), link.path.clone());
    let spawned = crate::jobs::spawn_guarded(
        "xl-relink",
        move || {
            // the part and the .bak of an export cut off between its two moves come first
            let left = interrupted(&link.path, uid);
            let found = candidates_until(uid, &link, &s);
            if !s.load(Ordering::Relaxed) {
                let _ = tx.send((found, left));
            }
        },
        |msg| eprintln!("xlsx relink search: {msg}"),
    );
    match spawned {
        Ok(_) => SEARCH.with(|x| *x.borrow_mut() = Some(Search { old, stop, rx })),
        // no worker: the card stays without candidates and "Find it" is the way
        Err(e) => eprintln!("xlsx relink search thread: {e}"),
    }
}

/// Takes the search result into the card once it is in (waiting up to `wait`); true while the
/// search still runs.
fn poll_search(wait: Option<Duration>) -> bool {
    let done = SEARCH.with(|s| {
        let s = s.borrow();
        let cur = s.as_ref()?;
        let got = match wait {
            Some(t) => cur.rx.recv_timeout(t).map_err(|e| e == mpsc::RecvTimeoutError::Disconnected),
            None => cur.rx.try_recv().map_err(|e| e == mpsc::TryRecvError::Disconnected),
        };
        match got {
            Ok(found) => Some(Some(found)),
            // the worker ended without a result: nothing found, "Find it" stays
            Err(true) => Some(Some((vec![], None))),
            Err(false) => Some(None),
        }
    });
    let (found, left) = match done {
        None => return false,
        Some(None) => return true,
        Some(Some(f)) => f,
    };
    if let Some(s) = SEARCH.with(|s| s.borrow_mut().take()) {
        CARD.with(|c| {
            if let Some(m) = c.borrow_mut().as_mut().filter(|m| m.old == s.old) {
                m.found = found;
                LEFT.with(|l| *l.borrow_mut() = left);
            }
        });
    }
    false
}

/// True while the look for a moved book runs.
pub fn searching() -> bool {
    SEARCH.with(|s| s.borrow().is_some())
}

/// Tests: waits for the running search and puts its result into the card.
#[cfg(test)]
pub(crate) fn settle() {
    let _ = poll_search(Some(Duration::from_secs(60)));
}

/// Tests: the running search's stop flag.
#[cfg(test)]
pub(crate) fn search_stop() -> Option<Arc<AtomicBool>> {
    SEARCH.with(|s| s.borrow().as_ref().map(|s| s.stop.clone()))
}

/// The open preview's book is gone for good; never while an export may be replacing it.
pub(super) fn vanished(path: &Path, busy: bool, now: Instant) -> bool {
    if busy {
        return false;
    }
    let exists = path.exists();
    GONE.with(|g| {
        let mut g = g.borrow_mut();
        let out = gone_step(&mut g, path, exists, now);
        if out {
            *g = None;
        }
        out
    })
}

/// The relink card: the old book and the same-export books found near it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub old: PathBuf,
    pub found: Vec<PathBuf>,
}

/// Another project was opened: its card and watch are not this one's (the stored link lives
/// in the old project's `meta` and goes with it).
pub fn forget() {
    stop_search();
    GONE.with(|g| *g.borrow_mut() = None);
    CARD.with(|c| *c.borrow_mut() = None);
    LEFT.with(|l| *l.borrow_mut() = None);
}

/// The card waiting for the user, if any.
pub fn pending() -> Option<Missing> {
    CARD.with(|c| c.borrow().clone())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    Link(PathBuf),
    Find,
    ExportNew,
    Close,
    /// Put the interrupted export's new book in place (the old one stays as `.bak`).
    Finish(Leftover),
    /// Move the old book back from its `.bak` (the new part stays).
    Restore(Leftover),
}

fn file_of(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

fn folder_of(p: &Path) -> String {
    p.parent().map(|d| d.display().to_string()).unwrap_or_default()
}

pub fn body(old: &Path, ru: bool) -> String {
    crate::tr_lang(ru, "xlsx_relink.body", &[("file", &file_of(old)), ("folder", &folder_of(old))])
}

/// What the quick look found, in words.
pub fn found_line(found: &[PathBuf], ru: bool) -> String {
    match found {
        [] => crate::tr_lang(ru, "xlsx_relink.found_none", &[]),
        [one] => crate::tr_lang(ru, "xlsx_relink.found_one", &[("file", &file_of(one)), ("folder", &folder_of(one))]),
        many => crate::tr_lang(ru, "xlsx_relink.found_many", &[("n", &many.len().to_string())]),
    }
}

pub fn link_label(p: &Path, ru: bool) -> String {
    crate::tr_lang(ru, "xlsx_relink.link", &[("file", &kit::shorten(&file_of(p), 28))])
}

/// What the card shows, as far as its buttons are concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardState {
    pub found: usize,
    pub part: bool,
    pub bak: bool,
}

/// The card's buttons in reading order with their tone: `Accent` is the one primary action,
/// everything else is quiet. An interrupted export is settled first; one found book is the
/// primary otherwise; several books are rows (the click on a row links it), so "Find it" leads.
pub fn actions(s: &CardState) -> Vec<(&'static str, Tone)> {
    let recover = s.part || s.bak;
    let lead = |on: bool| if on && !recover { Tone::Accent } else { Tone::Neutral };
    let mut v = Vec::new();
    if s.part {
        v.push(("xlsx_recover.finish", Tone::Accent));
    }
    if s.bak {
        v.push(("xlsx_recover.restore", if s.part { Tone::Neutral } else { Tone::Accent }));
    }
    if s.found == 1 {
        v.push(("xlsx_relink.link", lead(true)));
    }
    v.push(("xlsx_relink.find", lead(s.found != 1)));
    v.push(("xlsx_relink.export_new", Tone::Neutral));
    v.push(("xlsx_relink.close", Tone::Neutral));
    v
}

/// What the card's button named by its i18n key does (`Link` of a row is built where the row is).
pub(crate) fn click_of(key: &str, m: &Missing, left: Option<&Leftover>) -> Option<Click> {
    match key {
        "xlsx_recover.finish" => left.map(|l| Click::Finish(l.clone())),
        "xlsx_recover.restore" => left.map(|l| Click::Restore(l.clone())),
        "xlsx_relink.link" => match m.found.as_slice() {
            [one] => Some(Click::Link(one.clone())),
            _ => None,
        },
        "xlsx_relink.find" => Some(Click::Find),
        "xlsx_relink.export_new" => Some(Click::ExportNew),
        "xlsx_relink.close" => Some(Click::Close),
        _ => None,
    }
}

fn tone_of(list: &[(&'static str, Tone)], key: &str) -> Tone {
    list.iter().find(|(k, _)| *k == key).map_or(Tone::Neutral, |(_, t)| *t)
}

/// A button of the card: accent for the primary, quiet otherwise.
fn action(ui: &mut egui::Ui, label: impl Into<String>, tone: Tone) -> egui::Response {
    let label: String = label.into();
    if tone == Tone::Accent {
        kit::primary_button(ui, label, true)
    } else {
        let th = ddc_ui::theme::of(ui.ctx());
        ddc_ui::widgets::Button::quiet(&label).show(ui, &th)
    }
}

/// Text of the "found" line without the folder (the folder is a row of its own).
fn found_text(found: &[PathBuf], ru: bool) -> String {
    match found {
        [one] => crate::tr_lang(ru, "xlsx_relink.found_one_short", &[("file", &file_of(one))]),
        _ => found_line(found, ru),
    }
}

/// The card drawn alone with `found` candidates, and an interrupted save when `left`, for the
/// layout contract.
#[cfg(test)]
pub(crate) fn layout_probe(ru: bool, found: usize, left: bool) -> crate::layout_contract::Probe {
    let old = PathBuf::from("C:/data/Rooms.xlsx");
    let found = (1..=found).map(|i| PathBuf::from(format!("C:/data/copy {i}/Rooms.xlsx"))).collect();
    let m = Missing { old: old.clone(), found };
    let left = left.then(|| Leftover {
        dest: old,
        part: Some(PathBuf::from("C:/data/Rooms.xlsx.part")),
        bak: Some(PathBuf::from("C:/data/Rooms.xlsx.bak")),
    });
    crate::layout_contract::probe_id(egui::Id::new("xlsx_relink"), |ctx| {
        let _ = card(ctx, &m, left.as_ref(), ru, false);
    })
}

/// The card: one candidate is the primary action; several are equal choices, none preselected.
fn card(ctx: &egui::Context, m: &Missing, left: Option<&Leftover>, ru: bool, busy: bool) -> Option<Click> {
    use ddc_ui::theme::{layout, space};
    let th = ddc_ui::theme::of(ctx);
    let list = actions(&CardState {
        found: m.found.len(),
        part: left.is_some_and(|l| l.part.is_some()),
        bak: left.is_some_and(|l| l.bak.is_some()),
    });
    let width = layout::DIALOG_FORM.min(ctx.screen_rect().width() - 2.0 * space::XL).max(240.0);
    let mut out = None;
    let title = if left.is_some() { "xlsx_recover.title" } else { "xlsx_relink.title" };
    kit::Dialog::new("xlsx_relink", crate::tr_str(ru, title), kit::DialogSize::Form)
        .id(egui::Id::new("xlsx_relink"))
        .show(ctx, |ui| {
            ui.set_width(width);
            let muted = th.sem().text_secondary;
            ui.add(egui::Label::new(crate::tr_lang(ru, "xlsx_relink.gone", &[("file", &file_of(&m.old))])).wrap());
            kit::banner::path_label(ui, &folder_of(&m.old), muted);
            ui.add_space(space::SM);
            // an export cut off between its two moves: the way back is offered first
            if let Some(l) = left {
                ui.add(egui::Label::new(super::xlsx_recover::line(l, ru)).wrap());
                ui.add_space(space::XS);
                ui.horizontal_wrapped(|ui| {
                    if l.part.is_some()
                        && action(ui, crate::tr_str(ru, "xlsx_recover.finish"), tone_of(&list, "xlsx_recover.finish")).clicked()
                    {
                        out = click_of("xlsx_recover.finish", m, left);
                    }
                    if let Some(b) = &l.bak {
                        let r = action(ui, crate::tr_str(ru, "xlsx_recover.restore"), tone_of(&list, "xlsx_recover.restore"));
                        if r.on_hover_text(b.display().to_string()).clicked() {
                            out = click_of("xlsx_recover.restore", m, left);
                        }
                    }
                });
                ui.add_space(space::SM);
            }
            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(crate::tr_str(ru, "xlsx_relink.searching"));
                });
            } else {
                ui.add(egui::Label::new(found_text(&m.found, ru)).wrap());
            }
            if let [one] = m.found.as_slice() {
                kit::banner::path_label(ui, &folder_of(one), muted);
            }
            if m.found.len() > 1 {
                ui.add_space(space::XS);
                for p in &m.found {
                    let full = p.display().to_string();
                    if kit::banner::file_row(ui, &th, false, &file_of(p), &folder_of(p), &full).clicked() {
                        out = Some(Click::Link(p.clone()));
                    }
                }
            }
            ui.add_space(space::MD);
            ui.horizontal_wrapped(|ui| {
                if let [one] = m.found.as_slice() {
                    let b = action(ui, link_label(one, ru), tone_of(&list, "xlsx_relink.link"));
                    if b.on_hover_text(one.display().to_string()).clicked() {
                        out = click_of("xlsx_relink.link", m, left);
                    }
                }
                if action(ui, crate::tr_str(ru, "xlsx_relink.find"), tone_of(&list, "xlsx_relink.find")).clicked() {
                    out = click_of("xlsx_relink.find", m, left);
                }
                if action(ui, crate::tr_str(ru, "xlsx_relink.export_new"), tone_of(&list, "xlsx_relink.export_new")).clicked() {
                    out = click_of("xlsx_relink.export_new", m, left);
                }
                if kit::cancel_button(ui, crate::tr_str(ru, "xlsx_relink.close")).clicked() {
                    out = click_of("xlsx_relink.close", m, left);
                }
            });
        });
    if out.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        out = Some(Click::Close);
    }
    out
}

/// File dialog in the old folder, or the nearest folder above it that still exists.
fn find_dialog(old: &Path, ru: bool) -> Option<PathBuf> {
    let mut d = crate::file_pick::Dlg::new()
        .add_filter("XLSX",&["xlsx"])
        .set_title(crate::tr_str(ru, "xlsx_relink.find_title"));
    if let Some(dir) = old.ancestors().skip(1).find(|p| p.is_dir()) {
        d = d.set_directory(dir);
    }
    d.pick_file()
}

impl EditorApp {
    fn relink_ru(&self) -> bool {
        matches!(self.sh.cat.lang, ddc_ui::Lang::Ru)
    }

    /// The linked book's file is gone: the card with what the quick look found replaces any
    /// export offer and any stale preview. The stored link stays until another book is linked.
    pub(super) fn xlsx_missing(&mut self, old: PathBuf) {
        self.xlsx_offer = false;
        let link = stored(&self.ed.model)
            .filter(|l| l.path == old)
            .unwrap_or(Link { path: old.clone(), project: None, id: None });
        GONE.with(|g| *g.borrow_mut() = None);
        LEFT.with(|l| *l.borrow_mut() = None);
        CARD.with(|c| *c.borrow_mut() = Some(Missing { old, found: vec![] }));
        // up to MAX_LOOK books are opened: never on the UI thread
        start_search(model_uid(&self.ed.model), link);
    }

    pub(super) fn xlsx_relink_card(&mut self, ctx: &egui::Context) {
        let busy = poll_search(None);
        let Some(m) = pending() else {
            return;
        };
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let left = leftover().filter(|l| l.dest == m.old);
        let Some(click) = card(ctx, &m, left.as_ref(), self.relink_ru(), busy) else {
            return;
        };
        CARD.with(|c| *c.borrow_mut() = None);
        self.xlsx_relink_click(&m, click);
    }

    /// A linked or picked book goes through the normal preview read: the project gate refuses
    /// another project's book, and only a read that succeeds moves the link to it.
    pub(crate) fn xlsx_relink_click(&mut self, m: &Missing, click: Click) {
        // a cancelled dialog keeps the card, and its search goes on
        if click != Click::Find {
            stop_search();
        }
        match click {
            Click::Link(p) => self.xlsx_open(&p, false),
            Click::Find => match find_dialog(&m.old, self.relink_ru()) {
                Some(p) => {
                    stop_search();
                    self.xlsx_open(&p, false)
                }
                // the dialog was cancelled: the card stays
                None => CARD.with(|c| *c.borrow_mut() = Some(m.clone())),
            },
            // the old link is kept until the new book is written (export Done remembers it)
            Click::ExportNew => self.xlsx_export(),
            Click::Close => {}
            Click::Finish(l) => self.xlsx_recover(m, &l, true),
            Click::Restore(l) => self.xlsx_recover(m, &l, false),
        }
    }

    /// Finish or undo the interrupted export the user picked; the book in place is then read as
    /// usual. A failure keeps both files and the card, and says where they are.
    fn xlsx_recover(&mut self, m: &Missing, l: &Leftover, finish: bool) {
        let ru = self.relink_ru();
        let r = if finish {
            super::xlsx_recover::finish(l, ru)
        } else {
            super::xlsx_recover::restore(l, ru)
        };
        LEFT.with(|x| *x.borrow_mut() = None);
        match r {
            Ok(t) => {
                self.ed.status = Some(t);
                self.xlsx_open(&l.dest, false);
            }
            Err(t) => {
                self.ed.status = Some(t);
                // what is on the disk now decides what the card offers
                let uid = model_uid(&self.ed.model);
                LEFT.with(|x| *x.borrow_mut() = interrupted(&l.dest, uid));
                CARD.with(|c| *c.borrow_mut() = Some(m.clone()));
            }
        }
    }
}

#[cfg(test)]
#[path = "xlsx_relink_tests.rs"]
mod tests;
