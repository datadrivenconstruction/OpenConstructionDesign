//! Change list of the workbook preview: every refused, skipped, changed, created, deleted,
//! conflicting, locked and ignored cell of the report in one virtual table (only the visible
//! rows are built), with kind chips, sheet and text filters, sort by column and old -> new values.
//! Counts always describe the full report; the filters only narrow the list.
use crate::xl_text::{self, Lang};
use ddc_io::xlsx::{cell_ref, Report};
use ddc_ui::diff_kind::DiffKind;
use ddc_ui::theme::{control, radius, space, stroke};
use ddc_ui::widgets::{dropdown, Button, Cell, Column, RowState, Sort, Table, TextField};
use ddc_ui::Theme;
use egui::{self, Color32, RichText, Sense, Stroke};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Blocks the import (book-level problem).
    Refused,
    /// A cell or row left out, with its reason.
    Skipped,
    Conflict,
    Changed,
    Added,
    Deleted,
    /// Accepted, but flagged.
    Warning,
    /// Pinned element or locked parameter value: not written.
    Locked,
    Ignored,
}

const N_KINDS: usize = 9;

impl Kind {
    pub const ALL: [Kind; N_KINDS] = [
        Kind::Refused,
        Kind::Skipped,
        Kind::Conflict,
        Kind::Changed,
        Kind::Added,
        Kind::Deleted,
        Kind::Warning,
        Kind::Locked,
        Kind::Ignored,
    ];

    fn key(self) -> &'static str {
        match self {
            Kind::Refused => "xlw.kind_refused",
            Kind::Skipped => "xlw.skipped",
            Kind::Conflict => "xlw.conflicts",
            Kind::Changed => "xlw.changed",
            Kind::Added => "xlw.added",
            Kind::Deleted => "xlw.deleted",
            Kind::Warning => "xlw.kind_warning",
            Kind::Locked => "xlw.kind_locked",
            Kind::Ignored => "xlw.kind_ignored",
        }
    }
}

/// A pinned element or a locked parameter value: the exact import templates, since
/// `Ignored` carries no structured kind.
fn is_lock_reason(reason: &str) -> bool {
    use ddc_io::xlsx::match_template;
    match_template("{} {} is pinned: skipped", reason).is_some()
        || match_template("value of '{}' is locked: skipped", reason).is_some()
}

#[derive(Clone, Copy, Debug)]
enum Src {
    Rej(u32),
    Chg(u32),
    Con(u32),
    Wrn(u32),
    Ign(u32),
}

/// One report entry, borrowed.
pub struct Item<'a> {
    pub kind: Kind,
    pub sheet: &'a str,
    pub row: u32,
    pub col: u32,
    pub id: Option<i64>,
    pub column: &'a str,
    pub was: &'a str,
    pub now: &'a str,
    pub reason: &'a str,
    /// Index into `Report::changes` of a whole-row create or delete (its label names the row).
    pub label_of: Option<u32>,
}

fn base<'a>(kind: Kind, sheet: &'a str, row: u32, col: u32, id: Option<i64>, column: &'a str) -> Item<'a> {
    Item {
        kind,
        sheet,
        row,
        col,
        id,
        column,
        was: "",
        now: "",
        reason: "",
        label_of: None,
    }
}

fn item(rep: &Report, s: Src) -> Item<'_> {
    match s {
        Src::Rej(i) => {
            let r = &rep.rejected[i as usize];
            let kind = if r.blocks() { Kind::Refused } else { Kind::Skipped };
            Item {
                reason: &r.reason,
                ..base(kind, &r.sheet, r.row, r.col, None, &r.column)
            }
        }
        Src::Wrn(i) => {
            let r = &rep.warnings[i as usize];
            Item {
                reason: &r.reason,
                ..base(Kind::Warning, &r.sheet, r.row, r.col, None, &r.column)
            }
        }
        Src::Chg(i) => {
            let c = &rep.changes[i as usize];
            let kind = match c.kind() {
                "added" => Kind::Added,
                "deleted" => Kind::Deleted,
                _ => Kind::Changed,
            };
            let mut it = base(kind, &c.sheet, c.row, c.col, c.id, &c.column);
            if kind == Kind::Changed {
                it.was = &c.old;
                it.now = &c.new;
            } else if c.col == 0 {
                it.column = "";
                it.label_of = Some(i);
            }
            it
        }
        Src::Con(i) => {
            let c = &rep.conflicts[i as usize];
            Item {
                was: &c.model,
                now: &c.book,
                ..base(Kind::Conflict, &c.sheet, c.row, c.col, c.id, &c.column)
            }
        }
        Src::Ign(i) => {
            let g = &rep.ignored[i as usize];
            let kind = if is_lock_reason(&g.reason) {
                Kind::Locked
            } else {
                Kind::Ignored
            };
            Item {
                was: &g.old,
                now: &g.new,
                reason: &g.reason,
                ..base(kind, &g.sheet, g.row, g.col, g.id, &g.column)
            }
        }
    }
}

/// Address without the sheet: `D7`, `7:7`, or empty.
pub(super) fn short_address(it: &Item<'_>) -> String {
    let full = cell_ref(it.sheet, it.row, it.col);
    match full.rsplit_once('!') {
        Some((_, a)) => a.to_string(),
        None => String::new(),
    }
}

fn matches(it: &Item<'_>, needle: &str) -> bool {
    let id = it.id.map(|i| i.to_string()).unwrap_or_default();
    let addr = short_address(it);
    [it.sheet, &addr, it.column, it.was, it.now, it.reason, &id]
        .iter()
        .any(|f| !f.is_empty() && f.to_lowercase().contains(needle))
}

fn compare(col: usize, a: &Item<'_>, b: &Item<'_>) -> Ordering {
    match col {
        0 => (a.kind as usize).cmp(&(b.kind as usize)),
        1 => a.sheet.cmp(b.sheet),
        2 => (a.row, a.col).cmp(&(b.row, b.col)),
        3 => a.id.cmp(&b.id),
        4 => a.column.cmp(b.column),
        5 => a.was.cmp(b.was),
        _ => a.now.cmp(b.now),
    }
}

/// What the list is drawn from.
pub struct Ctx<'a> {
    pub rep: &'a Report,
    pub model: &'a ddc_model::Model,
    /// `XlsxPreview::report_gen`: a new report starts a fresh list.
    pub gen: u64,
    /// `Editor::cache_key`: looked-up labels live as long as it does.
    pub rev: (u64, u64),
    pub lang: Lang,
}

/// What a frame asks the window to do.
#[derive(Default)]
pub struct Out {
    /// Sheet and id of the element the picked row names.
    pub show: Option<(String, i64)>,
    /// The quiet Show button was pressed: bring that element into view.
    pub zoom: bool,
    /// Conflict cells whose choice was flipped: (sheet, row, col), take the book value.
    pub flips: Vec<((String, u32, u32), bool)>,
    /// Rows of a repeated id the user turned into new elements: (sheet, row).
    pub dup_new: Vec<(String, u32)>,
}

#[derive(Default)]
struct Labels {
    map: HashMap<u32, String>,
    runs: u32,
    /// Users of a type row (`type_use::users`), per (type table, id).
    users: HashMap<(&'static str, i64), Option<usize>>,
}

pub struct DiffState {
    srcs: Vec<Src>,
    sheets: Vec<String>,
    counts: [usize; N_KINDS],
    kinds: [bool; N_KINDS],
    /// 0 - all sheets, else index + 1 into `sheets`.
    sheet: usize,
    search: String,
    errors_only: bool,
    sort: Option<Sort>,
    /// Indices into `srcs`: filtered, then sorted.
    view: Vec<u32>,
    selected: Option<u32>,
    scroll_to: Option<usize>,
    stamp: Option<(u64, [usize; 5])>,
    dirty: bool,
    labels: RefCell<Labels>,
    labels_key: Option<(u64, (u64, u64))>,
    /// Cells built by the last frame (the table builds visible rows only).
    pub built: usize,
    /// Where the last counter row drew each chip.
    pub chip_rects: Vec<(Kind, egui::Rect)>,
    /// Tone of each kind, as the chips draw it; transparent until the first frame.
    tones: [Color32; N_KINDS],
    /// Least body height of the table: the window sets it so its footer sits at the bottom.
    pub fill_h: f32,
    /// Body height the last frame drew.
    pub body_h: f32,
}

/// Icon of a kind cell. Only kinds the chips name as a change share their icon; the others
/// keep their own, so a skipped or locked cell never looks like a refused one.
fn kind_icon(k: Kind) -> Option<&'static str> {
    Some(match k {
        Kind::Refused => DiffKind::Refused.icon(),
        Kind::Conflict => DiffKind::Conflict.icon(),
        Kind::Changed => DiffKind::Changed.icon(),
        Kind::Added => DiffKind::Added.icon(),
        Kind::Deleted => DiffKind::Deleted.icon(),
        Kind::Skipped | Kind::Warning => "warning",
        Kind::Locked => "lock",
        Kind::Ignored => return None,
    })
}

/// The kind column: the word plus the icon that leads it.
fn kind_col_w() -> f32 {
    control::DROPDOWN_W * 0.6 + ddc_ui::theme::icon_size::SM + space::XS
}

impl Default for DiffState {
    fn default() -> Self {
        Self {
            srcs: vec![],
            sheets: vec![],
            counts: [0; N_KINDS],
            kinds: [true; N_KINDS],
            sheet: 0,
            search: String::new(),
            errors_only: false,
            sort: None,
            view: vec![],
            selected: None,
            scroll_to: None,
            stamp: None,
            dirty: true,
            labels: RefCell::new(Labels::default()),
            labels_key: None,
            built: 0,
            chip_rects: vec![],
            tones: [Color32::TRANSPARENT; N_KINDS],
            fill_h: 0.0,
            body_h: 0.0,
        }
    }
}

impl DiffState {
    /// Entries of the full report of this kind, regardless of filters.
    pub fn count(&self, k: Kind) -> usize {
        self.counts[k as usize]
    }

    pub fn total(&self) -> usize {
        self.srcs.len()
    }

    /// Entries passing the filters.
    pub fn shown(&self) -> usize {
        self.view.len()
    }

    /// How many row labels were looked up in the model.
    pub fn label_runs(&self) -> u32 {
        self.labels.borrow().runs
    }

    /// Only refused and skipped cells (the window's "errors only" switch).
    pub fn set_errors_only(&mut self, on: bool) {
        if self.errors_only != on {
            self.errors_only = on;
            self.dirty = true;
        }
    }

    pub fn set_kind(&mut self, k: Kind, on: bool) {
        self.kinds[k as usize] = on;
        self.dirty = true;
    }

    pub fn set_search(&mut self, s: &str) {
        self.search = s.to_string();
        self.dirty = true;
    }

    pub fn set_sheet(&mut self, name: Option<&str>) {
        self.sheet = name
            .and_then(|n| self.sheets.iter().position(|s| s == n))
            .map_or(0, |i| i + 1);
        self.dirty = true;
    }

    pub fn set_sort(&mut self, s: Option<Sort>) {
        self.sort = s;
        self.dirty = true;
    }

    fn filtered(&self) -> bool {
        self.kinds.iter().any(|on| !on) || self.sheet > 0 || !self.search.trim().is_empty()
    }

    fn clear_filters(&mut self) {
        self.kinds = [true; N_KINDS];
        self.sheet = 0;
        self.search.clear();
        self.dirty = true;
    }

    /// Rebuild the list when the report changed and the view when a filter or the sort changed.
    pub fn ensure(&mut self, cx: &Ctx<'_>) {
        let rep = cx.rep;
        let stamp = (
            cx.gen,
            [
                rep.rejected.len(),
                rep.changes.len(),
                rep.conflicts.len(),
                rep.warnings.len(),
                rep.ignored.len(),
            ],
        );
        if self.stamp != Some(stamp) {
            self.rebuild(rep);
            self.stamp = Some(stamp);
            self.dirty = true;
        }
        let lk = Some((cx.gen, cx.rev));
        if self.labels_key != lk {
            let mut l = self.labels.borrow_mut();
            l.map.clear();
            l.users.clear();
            drop(l);
            self.labels_key = lk;
        }
        if self.dirty {
            self.refresh(rep);
            self.dirty = false;
        }
    }

    fn rebuild(&mut self, rep: &Report) {
        let keep = (self.sheet > 0)
            .then(|| self.sheets.get(self.sheet - 1).cloned())
            .flatten();
        // a taken conflict is listed once, as a conflict row with its choice
        let taken: HashSet<(&str, u32, u32)> = rep
            .conflicts
            .iter()
            .filter(|c| c.take_book)
            .map(|c| (c.sheet.as_str(), c.row, c.col))
            .collect();
        self.srcs.clear();
        self.srcs.extend((0..rep.rejected.len() as u32).map(Src::Rej));
        self.srcs.extend(
            (0..rep.changes.len() as u32)
                .filter(|i| {
                    let c = &rep.changes[*i as usize];
                    taken.is_empty() || !taken.contains(&(c.sheet.as_str(), c.row, c.col))
                })
                .map(Src::Chg),
        );
        self.srcs.extend((0..rep.conflicts.len() as u32).map(Src::Con));
        self.srcs.extend((0..rep.warnings.len() as u32).map(Src::Wrn));
        self.srcs.extend((0..rep.ignored.len() as u32).map(Src::Ign));
        self.counts = [0; N_KINDS];
        let mut names: HashSet<&str> = HashSet::new();
        for s in &self.srcs {
            let it = item(rep, *s);
            self.counts[it.kind as usize] += 1;
            names.insert(it.sheet);
        }
        let mut names: Vec<String> = names.into_iter().map(str::to_string).collect();
        names.sort();
        self.sheets = names;
        self.sheet = keep
            .and_then(|k| self.sheets.iter().position(|s| *s == k))
            .map_or(0, |i| i + 1);
        self.selected = None;
    }

    fn refresh(&mut self, rep: &Report) {
        let needle = self.search.trim().to_lowercase();
        let sheet = (self.sheet > 0)
            .then(|| self.sheets.get(self.sheet - 1))
            .flatten();
        let errors_only = self.errors_only;
        let mut view: Vec<u32> = (0..self.srcs.len() as u32)
            .filter(|i| {
                let it = item(rep, self.srcs[*i as usize]);
                self.kinds[it.kind as usize]
                    && (!errors_only || matches!(it.kind, Kind::Refused | Kind::Skipped))
                    && sheet.map_or(true, |s| it.sheet == s)
                    && (needle.is_empty() || matches(&it, &needle))
            })
            .collect();
        if let Some(s) = self.sort {
            // stable: equal keys keep the report order in both directions
            view.sort_by(|a, b| {
                let o = compare(
                    s.col,
                    &item(rep, self.srcs[*a as usize]),
                    &item(rep, self.srcs[*b as usize]),
                );
                if s.desc {
                    o.reverse()
                } else {
                    o
                }
            });
        }
        self.view = view;
        // keep the selection only while its row is still listed
        if let Some(s) = self.selected {
            if !self.view.contains(&s) {
                self.selected = None;
            }
        }
    }

    /// Report entry of a table row.
    fn at<'a>(&self, rep: &'a Report, row: usize) -> Item<'a> {
        item(rep, self.srcs[self.view[row] as usize])
    }

    /// Entries passing the filters, in table order.
    pub fn view_items<'a>(&'a self, rep: &'a Report) -> impl Iterator<Item = Item<'a>> + 'a {
        (0..self.view.len()).map(move |r| self.at(rep, r))
    }

    /// Sheet and id of the element named by a table row, when the row has one.
    pub fn element_of_row(&self, rep: &Report, row: usize) -> Option<(String, i64)> {
        let it = self.at(rep, row);
        // a created row names the type it will make, not an element that exists
        if it.kind == Kind::Added {
            return None;
        }
        let id = it.id?;
        super::xlsx_win::sheet_sel(it.sheet, id)?;
        Some((it.sheet.to_string(), id))
    }

    /// Card key of the element behind the selected row, for the outline in the views.
    pub fn hot_key(&self, rep: &Report) -> Option<ddc_model::ElKey> {
        let (sheet, id) = self.element_of_row(rep, self.row_of_selected()?)?;
        crate::ai::preview::key_of(super::xlsx_win::sheet_sel(&sheet, id)?)
    }

    /// The tone of every kind, from the same tokens as the chips.
    fn set_tones(&mut self, th: &Theme) {
        for k in Kind::ALL {
            self.tones[k as usize] = match k {
                Kind::Refused => DiffKind::Refused.color(th),
                Kind::Conflict => DiffKind::Conflict.color(th),
                Kind::Changed => DiffKind::Changed.color(th),
                Kind::Added => DiffKind::Added.color(th),
                Kind::Deleted => DiffKind::Deleted.color(th),
                _ => th.c("warning"),
            };
        }
    }

    fn row_of_selected(&self) -> Option<usize> {
        let s = self.selected?;
        self.view.iter().position(|v| *v == s)
    }

    /// Click or keyboard on a row: remember it; the element to select in the model, if any.
    pub fn pick(&mut self, rep: &Report, row: usize) -> Option<(String, i64)> {
        let row = row.min(self.view.len().checked_sub(1)?);
        self.selected = Some(self.view[row]);
        self.scroll_to = Some(row);
        self.element_of_row(rep, row)
    }

    /// "Walls . type . level" of a created or deleted row, looked up once per report and revision.
    fn label(&self, cx: &Ctx<'_>, idx: u32) -> String {
        let mut l = self.labels.borrow_mut();
        if let Some(s) = l.map.get(&idx) {
            return s.clone();
        }
        l.runs += 1;
        let s = super::xlsx_win::change_label(cx.model, cx.rep, idx as usize);
        l.map.insert(idx, s.clone());
        s
    }

    /// "affects 124 walls" on a changed type or layer row: every element of the type takes it.
    /// None on element rows, on a conflict that keeps the model value and on a type nothing uses.
    fn reach(&self, cx: &Ctx<'_>, it: &Item<'_>, row: usize) -> Option<String> {
        match it.kind {
            Kind::Changed => {}
            // a kept conflict writes nothing, as `changed_types` and the tick count it
            Kind::Conflict if cx.rep.conflicts[self.conflict_index(row)].take_book => {}
            _ => return None,
        }
        let (table, id) = ddc_io::xlsx::type_row(it.sheet, it.id?)?;
        let n = *self
            .labels
            .borrow_mut()
            .users
            .entry((table, id))
            .or_insert_with(|| crate::type_use::users(cx.model, table, id));
        crate::type_use::affects_text(cx.lang == Lang::Ru, table, n?)
    }

    /// Was/Now columns are number columns: lengths follow them (right, unit beside the number),
    /// every other value stays left.
    fn cell_text(&self, cx: &Ctx<'_>, row: usize, col: usize) -> Cell {
        let c = self.cell_text_raw(cx, row, col);
        if col >= 5 && c.unit.is_empty() {
            c.with_align(egui::Align::Min)
        } else {
            c
        }
    }

    fn cell_text_raw(&self, cx: &Ctx<'_>, row: usize, col: usize) -> Cell {
        let it = self.at(cx.rep, row);
        let lang = cx.lang;
        let t = |k: &'static str| crate::tr_str(lang == Lang::Ru, k);
        let val = |v: &str| shown_in(it.sheet, it.column, v, lang == Lang::Ru, crate::units::active_units());
        let empty = |c: Cell, v: &str| {
            if v.trim().is_empty() {
                Cell::muted(t("xl_diff.empty"))
            } else {
                c
            }
        };
        match col {
            0 => {
                let tone = self.tones[it.kind as usize];
                let tone = Some(tone).filter(|c| *c != Color32::TRANSPARENT);
                let edge = tone.filter(|_| matches!(it.kind, Kind::Refused | Kind::Conflict));
                Cell::text(t(it.kind.key()))
                    .with_icon(kind_icon(it.kind), tone)
                    .with_edge(edge)
                    .with_tip(t(it.kind.key()))
            }
            1 => Cell::text(xl_text::sheet_name(it.sheet, lang)),
            // a whole row has no cell: its book row is all there is to point at
            2 if it.label_of.is_some() => Cell::muted(crate::tr_lang(
                lang == Lang::Ru,
                "xl_diff.book_row",
                &[("n", &it.row.to_string())],
            )),
            2 => Cell::text(short_address(&it)),
            3 => match it.id {
                Some(i) => Cell::num(i.to_string(), ""),
                None if it.kind == Kind::Added => Cell::muted(t("xlw.new")),
                None => Cell::text(""),
            },
            4 => match it.label_of {
                // the type and level of a created or deleted row run over the Was column
                Some(i) => Cell::text(self.label(cx, i)).with_span(1),
                None if it.column.is_empty() => Cell::text(xl_text::sheet_name(it.sheet, lang)),
                None => {
                    let head = column_head(it.sheet, it.column, lang == Lang::Ru, crate::units::active_units());
                    match self.reach(cx, &it, row) {
                        Some(r) => Cell::text(format!("{head} \u{b7} {r}")).with_tip(it.column),
                        None => Cell::text(head).with_tip(it.column),
                    }
                }
            },
            5 => match it.kind {
                Kind::Changed | Kind::Conflict | Kind::Locked | Kind::Ignored => {
                    let len = super::xlsx_win::is_length(it.sheet, it.column);
                    empty(value_cell(val(it.was), len, true), it.was)
                }
                _ => Cell::muted(""),
            },
            _ => match it.kind {
                Kind::Added => Cell::text(t("xlw.create")),
                Kind::Deleted => Cell::text(t("xlw.delete")),
                Kind::Refused | Kind::Skipped | Kind::Warning => Cell::text(xl_text::tr(it.reason, lang)),
                Kind::Locked | Kind::Ignored => {
                    let why = xl_text::tr(it.reason, lang);
                    if it.now.is_empty() {
                        Cell::text(why)
                    } else {
                        Cell::text(format!("{} \u{2014} {why}", val(it.now)))
                    }
                }
                Kind::Conflict => {
                    let c = &cx.rep.conflicts[self.conflict_index(row)];
                    let key = if c.take_book { "xlw.kind_book_taken" } else { "xlw.kind_book_value" };
                    Cell::text(crate::tr_lang(lang == Lang::Ru, key, &[("book", &val(it.now))]))
                }
                Kind::Changed => {
                    let len = super::xlsx_win::is_length(it.sheet, it.column);
                    empty(value_cell(val(it.now), len, false), it.now)
                }
            },
        }
    }

    /// Text of the Column cell of a listed row, as the table draws it.
    #[cfg(test)]
    pub(crate) fn column_text(&self, cx: &Ctx<'_>, row: usize) -> String {
        self.cell_text_raw(cx, row, 4).text
    }

    fn conflict_index(&self, row: usize) -> usize {
        match self.srcs[self.view[row] as usize] {
            Src::Con(i) => i as usize,
            _ => 0,
        }
    }

    fn row_state(&self, row: usize, rep: &Report) -> RowState {
        let v = self.view[row];
        if self.selected == Some(v) {
            return RowState::Selected;
        }
        match item(rep, self.srcs[v as usize]).kind {
            Kind::Refused => RowState::Error,
            Kind::Skipped | Kind::Conflict | Kind::Warning | Kind::Locked | Kind::Ignored => RowState::Warning,
            _ => RowState::Normal,
        }
    }

    /// Chips of the window's one counter row, in order: added, changed, deleted (always, a zero
    /// one is disabled), then every other kind that has entries. Each number is the count of list
    /// entries the chip hides (changed = changed cells), so a click removes exactly that many.
    /// Needs `ensure` first.
    pub fn chip_plan(&self) -> Vec<(Kind, usize)> {
        const MAIN: [Kind; 3] = [Kind::Added, Kind::Changed, Kind::Deleted];
        let n = |k: Kind| (k, self.counts[k as usize]);
        let mut v: Vec<(Kind, usize)> = MAIN.iter().map(|k| n(*k)).collect();
        v.extend(
            Kind::ALL
                .iter()
                .filter(|k| !MAIN.contains(k))
                .map(|k| n(*k))
                .filter(|(_, c)| *c > 0),
        );
        v
    }

    /// The counter row: each chip shows its count and toggles that kind in the list below.
    pub fn kind_row(&mut self, ui: &mut egui::Ui, th: &Theme, ru: bool) {
        self.kind_row_in(ui, th, ru, false);
    }

    /// `no_base`: the book has no comparison data, so nothing counts as deleted; a zero
    /// "deleted" chip would sit beside "not removed" and read as a contradiction.
    pub fn kind_row_in(&mut self, ui: &mut egui::Ui, th: &Theme, ru: bool, no_base: bool) {
        use super::xlsx_counts::{chip_label, Chip};
        let dk = |k: Kind| match k {
            Kind::Refused => Some(DiffKind::Refused),
            Kind::Changed => Some(DiffKind::Changed),
            Kind::Added => Some(DiffKind::Added),
            Kind::Deleted => Some(DiffKind::Deleted),
            Kind::Conflict => Some(DiffKind::Conflict),
            _ => None,
        };
        let tone = |k: Kind| dk(k).map_or_else(|| th.c("warning"), |d| d.color(th));
        let (sec, prim) = (th.c("text.secondary"), th.c("text.primary"));
        self.chip_rects.clear();
        for (k, n) in self.chip_plan() {
            if no_base && k == Kind::Deleted && n == 0 {
                continue;
            }
            let label = match k {
                Kind::Conflict => chip_label(ru, Chip::Conflicts, n),
                Kind::Skipped => chip_label(ru, Chip::Skipped, n),
                Kind::Refused => chip_label(ru, Chip::Errors, n),
                _ => crate::tr_str(ru, k.key()).to_string(),
            };
            let on = self.kinds[k as usize];
            let (clicked, rect) = chip(ui, on, n, &label, (tone(k), dk(k)), prim, sec);
            self.chip_rects.push((k, rect));
            if clicked && n > 0 {
                self.set_kind(k, !on);
            }
        }
    }

    /// Chips, filters, the table and the choice strip of a selected conflict.
    pub fn show(&mut self, ui: &mut egui::Ui, th: &Theme, cx: &Ctx<'_>, max_height: f32) -> Out {
        self.ensure(cx);
        self.set_tones(th);
        let ru = cx.lang == Lang::Ru;
        let t = |k: &'static str| crate::tr_str(ru, k);
        let sec = th.c("text.secondary");
        let mut out = Out::default();
        ui.horizontal(|ui| {
            let mut names: Vec<String> = vec![t("xlw.all_sheets").to_string()];
            names.extend(self.sheets.iter().map(|s| xl_text::sheet_name(s, cx.lang)));
            let items: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut sel = self.sheet;
            if dropdown(ui, th, "xl-diff-sheet", &mut sel, &items, None).changed() {
                self.sheet = sel;
                self.dirty = true;
            }
            let f = TextField::new(&mut self.search)
                .id_salt("xl-diff-search")
                .hint(t("xlw.search_hint"))
                .width(control::DROPDOWN_W * 1.5)
                .show(ui, th);
            if f.changed {
                self.dirty = true;
            }
            let n = crate::tr_lang(
                ru,
                "xlw.shown_of",
                &[
                    ("shown", &self.view.len().to_string()),
                    ("total", &self.srcs.len().to_string()),
                ],
            );
            ui.label(RichText::new(n).color(sec));
            if self.filtered() && ui.small_button(t("xlw.clear_filters")).clicked() {
                self.clear_filters();
            }
        });
        ui.add_space(space::XS);
        self.ensure(cx);
        self.built = 0;
        if let Some(k) = crate::empty_kind::xl_preview(self.srcs.len(), self.view.len()) {
            if crate::ui_kit::empty_state_lang(ui, ru, k.what(), k.has_action()) {
                self.clear_filters();
            }
            return out;
        }
        let cols = [
            Column::text(t("xlw.kind_col")).fixed(kind_col_w()),
            Column::text(t("xlw.head_sheet")).fill(0.8),
            Column::text(t("xlw.head_cell")).fixed(space::XXL * 2.25),
            Column::number(t("xlw.head_id"), space::XXL * 2.0),
            Column::text(t("xlw.head_column")),
            Column::number(t("xlw.head_was"), 0.0).fill(1.0),
            Column::number(t("xlw.head_now"), 0.0).fill(1.4),
        ];
        let strip = if self.selected_conflict().is_some() || self.selected_dup(cx.rep).is_some() {
            space::XXL
        } else {
            0.0
        };
        let mut sort = self.sort;
        let mut built = 0usize;
        let scroll = self.scroll_to.take();
        let res = Table::new("xl-diff-table", &cols, self.view.len())
            // the room measured under the footer last frame beats the caller's estimate of
            // what follows the table: a short tail leaves no band under the buttons
            .max_height((max_height.max(self.fill_h) - strip).max(space::XXL))
            .min_height(self.fill_h - strip)
            .scroll_to(scroll)
            .show(
                ui,
                th,
                &mut sort,
                |r| self.row_state(r, cx.rep),
                |r, c| {
                    built += 1;
                    self.cell_text(cx, r, c)
                },
            );
        self.built = built;
        self.body_h = res.body_h + strip;
        if res.sort_changed {
            self.set_sort(sort);
            self.ensure(cx);
        }
        let mut pick = res.clicked;
        if let Some(d) = res.moved {
            let cur = self.row_of_selected().map_or(-1, |r| r as isize);
            let next = if cur < 0 && d > 0 { d - 1 } else { cur + d };
            pick = Some(next.clamp(0, self.view.len() as isize - 1) as usize);
        }
        if res.activated {
            pick = pick.or(self.row_of_selected());
        }
        if let Some(r) = pick {
            out.show = self.pick(cx.rep, r);
        }
        crate::ai::preview::set_row_hot(ui.ctx(), self.hot_key(cx.rep));
        let element = self.row_of_selected().and_then(|r| self.element_of_row(cx.rep, r));
        if element.is_some() {
            let tip = t("xlw.show_in_model");
            let go = Button::quiet(t("xl_diff.show")).small().show(ui, th).on_hover_text(tip);
            if go.clicked() {
                out.show = element;
                out.zoom = true;
            }
        }
        if let Some(i) = self.selected_conflict() {
            let c = &cx.rep.conflicts[i];
            let book = shown_in(&c.sheet, &c.column, &c.book, ru, crate::units::active_units());
            let key = (c.sheet.clone(), c.row, c.col);
            let take = crate::tr_lang(ru, "xlw.take_book", &[("book", &book)]);
            ui.horizontal(|ui| {
                if ui.selectable_label(!c.take_book, t("xlw.keep_model")).clicked() && c.take_book {
                    out.flips.push((key.clone(), false));
                }
                if ui.selectable_label(c.take_book, take).clicked() && !c.take_book {
                    out.flips.push((key.clone(), true));
                }
            });
        }
        if let Some((sheet, id, rows)) = self.selected_dup(cx.rep) {
            let list = rows.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
            let tip = crate::tr_lang(ru, "xlsx_dup.dup_new_tip", &[("rows", &list), ("id", &id.to_string())]);
            if ui.button(t("xlsx_dup.dup_new")).on_hover_text(tip).clicked() {
                out.dup_new = rows.into_iter().map(|r| (sheet.clone(), r)).collect();
            }
        }
        out
    }

    /// Sheet, id and rows of the selected row, when it refuses a repeated id with edits.
    fn selected_dup(&self, rep: &Report) -> Option<(String, i64, Vec<u32>)> {
        let Src::Rej(i) = self.srcs[self.selected? as usize] else {
            return None;
        };
        let r = rep.rejected.get(i as usize)?;
        let (id, rows) = ddc_io::xlsx::dup_conflict_rows(&r.reason)?;
        Some((r.sheet.clone(), id, rows))
    }

    /// Index into `Report::conflicts` of the selected row, when it is a conflict.
    fn selected_conflict(&self) -> Option<usize> {
        match self.srcs[self.selected? as usize] {
            Src::Con(i) => Some(i as usize),
            _ => None,
        }
    }
}

/// Column cell of a row: the field caption in the UI language with the unit of the values shown
/// beside it. Lengths of the format are shown in project units (`shown_value`), so their header
/// says so; other units stay as the book writes them. The raw book header is the cell tooltip.
pub(super) fn column_head(sheet: &str, column: &str, ru: bool, u: crate::units::Units) -> String {
    let (name, unit) = match column.split_once('[') {
        Some((n, rest)) => (n.trim(), rest.split(']').next().unwrap_or("").trim()),
        None => (column.trim(), ""),
    };
    let key = format!("xlc.{}", name.to_lowercase().replace(' ', "_"));
    let cap = crate::tr_lang(ru, &key, &[]);
    let cap = if cap.is_empty() || cap == key { name.to_string() } else { cap };
    let unit = if super::xlsx_win::is_length(sheet, column) {
        match u {
            crate::units::Units::Metric => crate::tr_str(ru, "unit_fmt.unit.mm"),
            crate::units::Units::Imperial => crate::tr_str(ru, "xlsx_delete.unit_ft_in"),
        }
    } else {
        unit
    };
    if unit.is_empty() {
        cap
    } else {
        format!("{cap} [{unit}]")
    }
}

/// `xlsx_win::shown_value` with the unit word in the table's language and the units the header
/// names, so a value never spells its unit differently from the column header.
pub(super) fn shown_in(sheet: &str, column: &str, v: &str, ru: bool, u: crate::units::Units) -> String {
    if !super::xlsx_win::is_length(sheet, column) {
        return v.to_string();
    }
    // num-lint: raw book cell in metres, machine format (as xlsx_win::shown_value)
    match v.trim().parse::<f64>() {
        Ok(m) => crate::units::display_length_lang(m, u, ru),
        Err(_) => v.to_string(),
    }
}

/// A length value as number plus muted unit (the table right-aligns the unit); other text as is.
pub(super) fn value_cell(shown: String, is_length: bool, muted: bool) -> Cell {
    let (n, u) = super::xlsx_head::split_unit(&shown);
    if !is_length || u.is_empty() {
        return if muted { Cell::muted(shown) } else { Cell::text(shown) };
    }
    let mut c = Cell::num(n, u);
    c.muted = muted;
    c
}

/// Toggle chip with the count of its kind; (clicked, rect). A zero count is muted and inert.
fn chip(
    ui: &mut egui::Ui,
    on: bool,
    n: usize,
    label: &str,
    tone: (Color32, Option<DiffKind>),
    prim: Color32,
    sec: Color32,
) -> (bool, egui::Rect) {
    let (col, kind) = tone;
    let on = on && n > 0;
    let fill = if on { col.gamma_multiply(0.12) } else { Color32::TRANSPARENT };
    let line = Stroke::new(stroke::HAIRLINE, col.gamma_multiply(if on { 0.6 } else { 0.3 }));
    let r = egui::Frame::new()
        .fill(fill)
        .stroke(line)
        .corner_radius(radius::MD)
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::XS as i8))
        .show(ui, |ui| {
            let (a, b) = if on { (col, prim) } else { (sec, sec) };
            if let Some(k) = kind {
                ddc_ui::diff_kind::badge(ui, k, a);
            }
            ui.add(egui::Label::new(RichText::new(n.to_string()).strong().color(a)).selectable(false));
            ui.add(egui::Label::new(RichText::new(label).color(b)).selectable(false));
        });
    let rect = r.response.rect;
    if n == 0 {
        return (false, rect);
    }
    let hit = ui
        .interact(rect, ui.id().with(("xl-chip", label)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked();
    (hit, rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_io::xlsx::{Change, Conflict, Ignored, Rejection};
    use ddc_ui::theme::{Density, Mode};

    fn change(i: usize, sheet: &str, col: &str, new: &str) -> Change {
        let whole = col == "id";
        Change {
            sheet: sheet.into(),
            row: 2 + i as u32,
            col: if whole { 0 } else { 5 },
            column: col.into(),
            id: Some(i as i64),
            old: "1".into(),
            new: new.into(),
        }
    }

    fn ignored(reason: &str) -> Ignored {
        Ignored {
            sheet: "Quantities".into(),
            row: 3,
            col: 2,
            column: "area".into(),
            id: None,
            old: "1".into(),
            new: "2".into(),
            reason: reason.into(),
        }
    }

    fn report(n: usize) -> Report {
        let mut r = Report::default();
        for i in 0..n {
            let sheet = if i % 2 == 0 { "Walls" } else { "Openings" };
            r.changes.push(match i % 5 {
                3 => change(i, sheet, "id", "created"),
                4 => change(i, sheet, "id", "deleted"),
                _ => change(i, sheet, "height [m]", &format!("{}", i % 7)),
            });
        }
        r.rejected.push(Rejection {
            sheet: "Walls".into(),
            row: 9,
            col: 4,
            column: "ax [m]".into(),
            reason: "not a number".into(),
        });
        r.ignored.push(ignored("read-only"));
        r
    }

    fn ctx<'a>(r: &'a Report, ed: &'a Editor) -> Ctx<'a> {
        Ctx {
            rep: r,
            model: &ed.model,
            gen: 0,
            rev: ed.cache_key(),
            lang: Lang::En,
        }
    }

    fn shape_texts(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|x| shape_texts(x, out)),
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            _ => {}
        }
    }

    /// One frame of the counter row followed by the list; returns the text painted and the events.
    fn frame(
        ctx: &egui::Context,
        th: &Theme,
        s: &mut DiffState,
        cx: &Ctx<'_>,
        list: bool,
        events: Vec<egui::Event>,
    ) -> Vec<String> {
        let ru = cx.lang == Lang::Ru;
        let raw = egui::RawInput { events, ..Default::default() };
        let out = ctx.run(raw, |c| {
            egui::CentralPanel::default().show(c, |ui| {
                s.ensure(cx);
                ui.horizontal_wrapped(|ui| s.kind_row(ui, th, ru));
                if list {
                    s.show(ui, th, cx, 300.0);
                }
            });
        });
        let mut texts = vec![];
        out.shapes.iter().for_each(|cs| shape_texts(&cs.shape, &mut texts));
        texts
    }

    fn themed(ctx: &egui::Context) -> Theme {
        let mut th = Theme::new(Mode::Light, Density::Comfortable);
        let _ = ctx.run(Default::default(), |c| {
            th.apply(c);
        });
        th
    }

    #[test]
    fn counter_row_counts_equal_list_entries_and_zero_is_disabled() {
        let mut r = report(10);
        r.changes.retain(|c| c.kind() != "deleted");
        let ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        s.ensure(&ctx(&r, &ed));
        let plan = s.chip_plan();
        let c = r.counts();
        assert_eq!(plan[0], (Kind::Added, c.added));
        // changed counts the list entries (cells), not the rows of the totals
        assert_eq!(plan[1], (Kind::Changed, s.count(Kind::Changed)));
        assert!(plan[1].1 >= c.changed);
        assert_eq!(plan[2], (Kind::Deleted, 0), "a zero total is still a chip");
        let kinds: Vec<Kind> = plan.iter().map(|p| p.0).collect();
        let mut uniq = kinds.clone();
        uniq.dedup();
        assert_eq!(kinds.len(), uniq.len(), "each kind appears once");
        // a bad cell value skips its row; only `Rejection::blocks` items are Refused
        assert!(plan.contains(&(Kind::Skipped, 1)) && plan.contains(&(Kind::Ignored, 1)));
        assert!(plan[3..].iter().all(|p| p.1 > 0), "other kinds only with entries");
    }

    #[test]
    fn counter_row_paints_each_label_once_and_chips_filter_the_list_en_ru() {
        let r = report(10);
        let ed = Editor::new_in_memory().unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let ru = lang == Lang::Ru;
            let ctx_ui = font_ctx();
            let th = themed(&ctx_ui);
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let mut s = DiffState::default();
            let mut texts = vec![];
            for _ in 0..3 {
                texts = frame(&ctx_ui, &th, &mut s, &cx, false, vec![]);
            }
            for k in [Kind::Added, Kind::Changed, Kind::Deleted] {
                let label = crate::tr_str(ru, k.key());
                let n = texts.iter().filter(|t| t.as_str() == label).count();
                assert_eq!(n, 1, "{lang:?}: label {label:?} painted {n} times");
            }
            assert_eq!(s.chip_rects.len(), s.chip_plan().len());
            let before = s.shown();
            assert_eq!(before, s.total());
            let (_, rect) = *s.chip_rects.iter().find(|(k, _)| *k == Kind::Changed).unwrap();
            let at = rect.center();
            let btn = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            for ev in [vec![egui::Event::PointerMoved(at)], vec![btn(true)], vec![btn(false)], vec![]] {
                let _ = frame(&ctx_ui, &th, &mut s, &cx, true, ev);
            }
            assert_eq!(s.shown(), before - s.count(Kind::Changed), "{lang:?}: changed rows hidden");
            assert!(s.count(Kind::Changed) > 0);
            // the counts never follow the filter
            assert_eq!(s.count(Kind::Changed) + s.shown(), before);
        }
    }

    #[test]
    fn zero_count_chip_ignores_clicks() {
        let mut r = report(10);
        r.changes.retain(|c| c.kind() != "deleted");
        let ed = Editor::new_in_memory().unwrap();
        let ctx_ui = font_ctx();
        let th = themed(&ctx_ui);
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        for _ in 0..3 {
            let _ = frame(&ctx_ui, &th, &mut s, &cx, true, vec![]);
        }
        let (_, rect) = *s.chip_rects.iter().find(|(k, _)| *k == Kind::Deleted).unwrap();
        let at = rect.center();
        let btn = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let total = s.shown();
        for ev in [vec![egui::Event::PointerMoved(at)], vec![btn(true)], vec![btn(false)], vec![]] {
            let _ = frame(&ctx_ui, &th, &mut s, &cx, true, ev);
        }
        assert_eq!(s.shown(), total);
        assert!(s.kinds[Kind::Deleted as usize], "the disabled chip stays on");
    }

    #[test]
    fn length_cells_split_into_number_and_muted_unit() {
        let c = value_cell("2800\u{a0}mm".into(), true, false);
        assert_eq!((c.text.as_str(), c.unit.as_str(), c.muted), ("2800", "mm", false));
        let w = value_cell("2700\u{a0}mm".into(), true, true);
        assert!(w.muted && w.unit == "mm");
    }

    #[test]
    fn column_label_has_display_unit() {
        use crate::units::Units;
        // explicit language: other tests may switch the UI language at any time
        for ru in [false, true] {
            let height = crate::tr_lang(ru, "xlc.height", &[]);
            let mm = crate::tr_str(ru, "unit_fmt.unit.mm");
            // a length of the format is shown in project units, and its header says so
            assert_eq!(column_head("Walls", "height [m]", ru, Units::Metric), format!("{height} [{mm}]"));
            let base = crate::tr_lang(ru, "xlc.base_offset", &[]);
            assert_eq!(column_head("Walls", "base_offset [m]", ru, Units::Metric), format!("{base} [{mm}]"));
            let ft = column_head("Walls", "height [m]", ru, Units::Imperial);
            assert_eq!(ft, format!("{height} [{}]", crate::tr_str(ru, "xlsx_delete.unit_ft_in")));
            assert!(!ft.contains("[m]"));
            // values left as the book writes them keep the book unit
            let off = crate::tr_lang(ru, "xlc.offset", &[]);
            assert_eq!(column_head("Quantities", "offset [m]", ru, Units::Metric), format!("{off} [m]"));
            let area = crate::tr_lang(ru, "xlc.area", &[]);
            assert_eq!(column_head("Rooms", "area [m2]", ru, Units::Metric), format!("{area} [m2]"));
            // a column without a caption keeps its own name
            assert_eq!(column_head("Walls", "fire_rating", ru, Units::Metric), "fire_rating");
        }
        assert_eq!(crate::tr_str(false, "xlsx_delete.unit_ft_in"), "ft-in");
        assert_ne!(crate::tr_str(true, "xlsx_delete.unit_ft_in"), "xlsx_delete.unit_ft_in");
        // the header unit is the unit of the value beside it
        let v = crate::units::display_length_lang(3.2, Units::Metric, false);
        let (_, unit) = super::super::xlsx_head::split_unit(&v);
        assert!(column_head("Walls", "height [m]", false, Units::Metric).ends_with(&format!("[{unit}]")));
        // the table cell carries the raw book header as its tooltip
        let mut r = Report::default();
        r.changes.push(change(1, "Walls", "height [m]", "3.2"));
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        let c = s.cell_text(&cx, 0, 4);
        assert_eq!(c.tip, "height [m]");
        assert!(!c.text.contains("[m]"), "{}", c.text);
    }

    /// Context with the named font weights bound: fonts installed in a pass bind on the next one.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    #[test]
    fn grouped_values_keep_their_digits_in_one_cell() {
        for (shown, num, unit) in [
            ("3 000 mm", "3 000", "mm"),
            ("3\u{a0}000 mm", "3\u{a0}000", "mm"),
            ("1 200\u{a0}mm", "1 200", "mm"),
            ("3\u{a0}000\u{a0}\u{43c}\u{43c}", "3\u{a0}000", "\u{43c}\u{43c}"),
            ("1 250,5 m\u{b2}", "1 250,5", "m\u{b2}"),
        ] {
            let c = value_cell(shown.into(), true, false);
            assert_eq!((c.text.as_str(), c.unit.as_str()), (num, unit), "{shown}");
        }
        for whole in ["7", "3000", "12 ft 6 in", "10'-6 1/2\""] {
            let c = value_cell(whole.into(), true, true);
            assert_eq!((c.text.as_str(), c.unit.as_str(), c.muted), (whole, "", true));
        }
    }

    #[test]
    fn header_unit_is_the_unit_of_the_values() {
        use crate::units::Units;
        for ru in [false, true] {
            // metric: "3 200 mm" under "Height [mm]", in either language
            let c = value_cell(shown_in("Walls", "height [m]", "3.2", ru, Units::Metric), true, false);
            let digits: String = c.text.chars().filter(|ch| ch.is_ascii_digit()).collect();
            assert_eq!(digits, "3200", "{ru}: {}", c.text);
            assert_eq!(c.unit, crate::tr_str(ru, "unit_fmt.unit.mm"));
            let head = column_head("Walls", "height [m]", ru, Units::Metric);
            assert!(head.ends_with(&format!("[{}]", c.unit)), "{head} vs {}", c.unit);
            // imperial: feet-inches values under a feet-inches header, never "[m]" or "[mm]"
            let v = shown_in("Walls", "base_offset [m]", "3.2004", ru, Units::Imperial);
            assert!(v.contains('\'') && v.ends_with('"'), "{v}");
            let c = value_cell(v.clone(), true, false);
            assert_eq!((c.text.as_str(), c.unit.as_str()), (v.as_str(), ""));
            let head = column_head("Walls", "base_offset [m]", ru, Units::Imperial);
            assert!(head.ends_with(&format!("[{}]", crate::tr_str(ru, "xlsx_delete.unit_ft_in"))), "{head}");
            // values the book keeps as written keep the book unit on both sides
            assert_eq!(shown_in("Rooms", "area [m2]", "12.5", ru, Units::Metric), "12.5");
        }
    }

    #[test]
    fn preview_renders_grouped_lengths_whole_in_en_and_ru() {
        let mut r = Report::default();
        let mut ch = change(1, "Walls", "height [m]", "3.2");
        ch.old = "3".into();
        r.changes.push(ch);
        let ed = Editor::new_in_memory().unwrap();
        let mut th = Theme::new(Mode::Light, Density::Comfortable);
        let ui_ctx = font_ctx();
        let _ = ui_ctx.run(Default::default(), |c| {
            th.apply(c);
        });
        for lang in [Lang::En, Lang::Ru] {
            let ru = lang == Lang::Ru;
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let mut s = DiffState::default();
            for _ in 0..2 {
                let _ = ui_ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        s.show(ui, &th, &cx, 300.0);
                    });
                });
            }
            let u = crate::units::active_units();
            let head = s.cell_text(&cx, 0, 4).text;
            for (col, metres) in [(5, 3.0), (6, 3.2)] {
                let c = s.cell_text(&cx, 0, col);
                let want = crate::units::display_length_lang(metres, u, ru);
                let (num, unit) = super::super::xlsx_head::split_unit(&want);
                assert_eq!((c.text.as_str(), c.unit.as_str()), (num, unit), "{lang:?} col {col}");
                if !unit.is_empty() {
                    assert!(head.ends_with(&format!("[{unit}]")), "{head} vs {unit}");
                }
            }
            assert_eq!(head, column_head("Walls", "height [m]", ru, u));
        }
    }

    #[test]
    fn was_now_lengths_right_align_with_unit_beside_and_text_stays_left() {
        use crate::layout_contract as lc;
        let mut r = Report::default();
        let mut ch = change(1, "Walls", "height [m]", "3.2");
        ch.old = "3".into();
        r.changes.push(ch);
        let mut add = change(2, "Walls", "id", "created");
        add.old = String::new();
        r.changes.push(add);
        let ed = Editor::new_in_memory().unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let ru = lang == Lang::Ru;
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let (ui_ctx, mut th) = lc::driven_ctx();
            let show_th = Theme::new(Mode::Dark, Density::Compact);
            let mut s = DiffState::default();
            let mut p = lc::Probe::default();
            for _ in 0..4 {
                let mut f = |c: &egui::Context| {
                    egui::CentralPanel::default().show(c, |ui| {
                        s.show(ui, &show_th, &cx, 400.0);
                    });
                };
                p = lc::pass(&ui_ctx, &mut th, egui::Id::new("xl_diff_geo"), vec![], &mut f);
            }
            let u = crate::units::active_units();
            let span = |t: &str| {
                let hit = p.spans.iter().find(|(s, _)| s == t);
                hit.unwrap_or_else(|| panic!("{lang:?}: {t:?} not painted in {:?}", p.text)).1
            };
            let mut unit_left = vec![];
            for metres in [3.0, 3.2] {
                let shown = crate::units::display_length_lang(metres, u, ru);
                let (num, unit) = super::super::xlsx_head::split_unit(&shown);
                let n = span(num);
                // the same unit is painted in both columns: take the one beside this number
                let un = p
                    .spans
                    .iter()
                    .filter(|(s, r)| s == unit && r.left() >= n.right() - 0.5)
                    .map(|(_, r)| *r)
                    .min_by(|a, b| a.left().total_cmp(&b.left()))
                    .unwrap_or_else(|| panic!("{lang:?}: unit {unit:?} not painted"));
                let gap = un.left() - n.right();
                assert!(gap >= -0.5 && gap <= space::XS + 1.0, "{lang:?} {num}: gap {gap}");
                unit_left.push(un.left());
            }
            let create = span(crate::tr_str(ru, "xlw.create"));
            assert!(create.right() < unit_left[1], "{lang:?}: text value stays left of units");
        }
    }

    #[test]
    fn text_cells_in_was_now_columns_align_left_and_lengths_follow_the_column() {
        let mut r = Report::default();
        let mut ch = change(1, "Walls", "height [m]", "3.2");
        ch.old = "3".into();
        r.changes.push(ch);
        let mut add = change(2, "Walls", "id", "created");
        add.old = String::new();
        r.changes.push(add);
        let ed = Editor::new_in_memory().unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let (ui_ctx, mut th) = (font_ctx(), Theme::new(Mode::Light, Density::Comfortable));
            let mut s = DiffState::default();
            for _ in 0..2 {
                let _ = ui_ctx.run(Default::default(), |c| {
                    th.apply(c);
                    egui::CentralPanel::default().show(c, |ui| {
                        s.show(ui, &th, &cx, 300.0);
                    });
                });
            }
            let kinds: Vec<_> =
                (0..2).map(|row| (s.cell_text(&cx, row, 5), s.cell_text(&cx, row, 6))).collect();
            let mut seen_len = false;
            let mut seen_text = false;
            for (was, now) in kinds {
                for c in [was, now] {
                    if c.unit.is_empty() {
                        assert_eq!(c.align, Some(egui::Align::Min), "{lang:?} {:?}", c.text);
                        seen_text |= !c.text.is_empty();
                    } else {
                        assert_eq!(c.align, None, "{lang:?}: lengths follow the number column");
                        seen_len = true;
                    }
                }
            }
            assert!(seen_len && seen_text, "{lang:?}");
        }
    }

    #[test]
    fn non_length_or_unitless_cells_stay_text() {
        let a = value_cell("Wall A".into(), false, false);
        assert!(a.unit.is_empty() && a.text == "Wall A");
        let b = value_cell("12".into(), true, true);
        assert!(b.unit.is_empty() && b.muted);
        let c = value_cell("Basic wall 200".into(), false, false);
        assert!(c.unit.is_empty());
    }

    #[test]
    fn diff_table_lists_all_5000_changes() {
        let r = report(5000);
        let ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        s.ensure(&ctx(&r, &ed));
        assert_eq!(s.total(), 5002);
        assert_eq!(s.shown(), 5002);
        assert_eq!(s.at(&r, 5001).kind, Kind::Ignored);
    }

    #[test]
    fn filter_by_kind_sheet_and_search_counts_match_report() {
        let r = report(5000);
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        let c = r.counts();
        assert_eq!(s.count(Kind::Added), c.added);
        assert_eq!(s.count(Kind::Deleted), c.deleted);
        assert_eq!(s.count(Kind::Skipped), 1);
        assert_eq!(s.count(Kind::Ignored), 1);
        let changed = r.changes.iter().filter(|c| c.kind() == "changed").count();
        assert_eq!(s.count(Kind::Changed), changed);
        assert_eq!(Kind::ALL.iter().map(|k| s.count(*k)).sum::<usize>(), s.total());
        // filters narrow the view, never the counts
        s.set_kind(Kind::Changed, false);
        s.set_kind(Kind::Ignored, false);
        s.ensure(&cx);
        assert_eq!(s.shown(), c.added + c.deleted + 1);
        assert_eq!(s.count(Kind::Changed), changed);
        assert!(s.filtered());
        s.set_sheet(Some("Openings"));
        s.ensure(&cx);
        assert!(s.shown() > 0 && s.shown() < c.added + c.deleted);
        s.set_search("zzz-none");
        s.ensure(&cx);
        assert_eq!(s.shown(), 0);
        assert_eq!(s.total(), 5002);
        s.clear_filters();
        s.ensure(&cx);
        assert_eq!(s.shown(), 5002);
        assert!(!s.filtered());
    }

    #[test]
    fn locked_and_pinned_cells_have_their_own_kind() {
        let mut r = report(10);
        r.ignored.push(ignored("wall 4 is pinned: skipped"));
        r.ignored.push(ignored("value of 'Mark' is locked: skipped"));
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        assert_eq!(s.count(Kind::Locked), 2);
        assert_eq!(s.count(Kind::Ignored), 1);
        for k in Kind::ALL {
            s.set_kind(k, k == Kind::Locked);
        }
        s.ensure(&cx);
        assert_eq!(s.shown(), 2);
    }

    #[test]
    fn search_and_errors_only() {
        let r = report(50);
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        s.set_search("AX [m]");
        s.ensure(&cx);
        assert_eq!(s.shown(), 1);
        s.set_search("");
        s.set_errors_only(true);
        s.ensure(&cx);
        assert_eq!(s.shown(), 1);
        s.set_errors_only(false);
        s.ensure(&cx);
        assert_eq!(s.shown(), 52);
    }

    #[test]
    fn sort_is_stable_and_reversible() {
        let r = report(100);
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        for desc in [false, true] {
            s.set_sort(Some(Sort { col: 1, desc }));
            s.ensure(&cx);
            let sheets: Vec<&str> = (0..s.shown()).map(|i| s.at(&r, i).sheet).collect();
            assert!(sheets.windows(2).all(|w| if desc { w[0] >= w[1] } else { w[0] <= w[1] }));
            // equal sheets keep the report order
            let mut last = (String::new(), -1i64);
            for i in 0..s.shown() {
                let key = s.view[i] as i64;
                if s.at(&r, i).sheet == last.0 {
                    assert!(key > last.1, "ties keep report order (desc {desc})");
                }
                last = (s.at(&r, i).sheet.to_string(), key);
            }
        }
        s.set_sort(None);
        s.ensure(&cx);
        assert!(s.view.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn diff_row_click_names_the_element() {
        let r = report(10);
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        // row 0 is the refused cell (no element), row 1 is the first change: Walls id 0
        assert_eq!(s.pick(&r, 0), None);
        assert_eq!(s.pick(&r, 2), Some(("Openings".to_string(), 1)));
        assert_eq!(s.pick(&r, 1), Some(("Walls".to_string(), 0)));
        assert_eq!(s.row_of_selected(), Some(1));
        // the model is not edited by looking
        let mut e = Editor::new_in_memory().unwrap();
        let rev = e.revision;
        assert!(super::super::xlsx_win::pick_row(&mut e, "Walls", 0));
        assert_eq!(e.revision, rev);
    }

    fn kinds_report() -> Report {
        let mut r = report(10);
        r.conflicts.push(Conflict {
            sheet: "Walls".into(),
            row: 6,
            col: 5,
            column: "height [m]".into(),
            id: Some(4),
            base: "1".into(),
            book: "2".into(),
            model: "3".into(),
            take_book: false,
        });
        // `report` holds a bad cell (Skipped); a book-level problem is the Refused row
        r.rejected.push(Rejection {
            sheet: "Walls".into(),
            row: 0,
            col: 0,
            column: String::new(),
            reason: "required column missing: id".into(),
        });
        r.ignored.push(Ignored { id: Some(5), ..ignored("wall 5 is pinned: skipped") });
        let mut blank = change(6, "Walls", "height [m]", "");
        blank.old = String::new();
        r.changes.push(blank);
        r
    }

    #[test]
    fn picked_row_lights_its_element_and_a_created_row_none() {
        let r = report(10);
        let ed = Editor::new_in_memory().unwrap();
        let cx = ctx(&r, &ed);
        let mut s = DiffState::default();
        s.ensure(&cx);
        assert_eq!(s.hot_key(&r), None, "nothing picked");
        s.pick(&r, 1);
        let key = ("wall".to_string(), 0);
        assert_eq!(s.hot_key(&r), Some(key.clone()));
        let c = egui::Context::default();
        let _ = c.run(Default::default(), |c| {
            crate::ai::preview::set_row_hot(c, s.hot_key(&r));
        });
        assert!(crate::ai::preview::lit(&c).contains(&key), "the picked row is in the hot set");
        for _ in 0..4 {
            let _ = c.run(Default::default(), |_| {});
        }
        assert!(crate::ai::preview::lit(&c).is_empty(), "the outline lapses with the window");
        let made = (0..s.view.len()).find(|v| s.at(&r, *v).kind == Kind::Added).unwrap();
        s.pick(&r, made);
        assert_eq!(s.hot_key(&r), None, "a created row names no element");
        assert_eq!(s.element_of_row(&r, made), None);
    }

    #[test]
    fn kind_cells_have_icon_label_and_tone_en_ru() {
        let r = kinds_report();
        let ed = Editor::new_in_memory().unwrap();
        let ui_ctx = font_ctx();
        let th = themed(&ui_ctx);
        for lang in [Lang::En, Lang::Ru] {
            let ru = lang == Lang::Ru;
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let mut s = DiffState::default();
            s.ensure(&cx);
            s.set_tones(&th);
            let mut seen = HashSet::new();
            for row in 0..s.view.len() {
                let kind = s.at(&r, row).kind;
                seen.insert(kind as usize);
                let c = s.cell_text(&cx, row, 0);
                assert_eq!(c.text, crate::tr_str(ru, kind.key()), "{lang:?} {kind:?} name");
                assert!(!c.text.is_empty() && c.tip == c.text, "{lang:?} {kind:?} name");
                assert_eq!(c.icon, kind_icon(kind));
                assert_eq!(c.icon.is_some(), kind != Kind::Ignored, "{kind:?} icon");
                assert!(c.lead && c.icon_tone.is_some(), "{kind:?} tone");
                let edged = matches!(kind, Kind::Refused | Kind::Conflict);
                assert_eq!(c.edge.is_some(), edged, "{kind:?}: left tint only if blocked");
                if let Some(n) = c.icon {
                    assert!(ddc_ui::icons::source(n).is_some(), "{n} is a known icon");
                }
            }
            for k in [Kind::Refused, Kind::Skipped, Kind::Conflict, Kind::Changed, Kind::Added, Kind::Deleted] {
                assert!(seen.contains(&(k as usize)), "{k:?} row present");
            }
            assert!(seen.contains(&(Kind::Locked as usize)), "locked row present");
        }
    }

    #[test]
    fn created_and_deleted_rows_span_the_name_and_empty_values_are_named() {
        let r = kinds_report();
        let ed = Editor::new_in_memory().unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let ru = lang == Lang::Ru;
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let mut s = DiffState::default();
            s.ensure(&cx);
            for row in 0..s.view.len() {
                let it = s.at(&r, row);
                let (addr, label) = (s.cell_text(&cx, row, 2), s.cell_text(&cx, row, 4));
                match it.kind {
                    Kind::Added | Kind::Deleted => {
                        let n = it.row.to_string();
                        let book = crate::tr_lang(ru, "xl_diff.book_row", &[("n", &n)]);
                        assert_eq!((label.span, addr.text.as_str()), (1, book.as_str()));
                        let act = if it.kind == Kind::Added { "xlw.create" } else { "xlw.delete" };
                        assert_eq!(s.cell_text(&cx, row, 6).text, crate::tr_str(ru, act));
                    }
                    Kind::Changed if it.now.is_empty() => {
                        let word = crate::tr_str(ru, "xl_diff.empty");
                        assert_eq!(s.cell_text(&cx, row, 6).text, word);
                        assert_eq!(s.cell_text(&cx, row, 5).text, word);
                    }
                    _ => assert_eq!(label.span, 0, "{:?} spans nothing", it.kind),
                }
            }
        }
    }

    #[test]
    fn table_with_all_kinds_draws_in_a_narrow_ui_en_ru() {
        let r = kinds_report();
        let ed = Editor::new_in_memory().unwrap();
        let ui_ctx = font_ctx();
        let th = themed(&ui_ctx);
        for lang in [Lang::En, Lang::Ru] {
            let cx = Ctx { lang, ..ctx(&r, &ed) };
            let mut s = DiffState::default();
            for _ in 0..3 {
                let raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(420.0, 600.0),
                    )),
                    ..Default::default()
                };
                let _ = ui_ctx.run(raw, |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        s.show(ui, &th, &cx, 400.0);
                    });
                });
            }
            assert!(s.built >= s.view.len() - 1, "{lang:?}: rows are built in a narrow window");
        }
    }

    #[test]
    fn rebuilds_only_when_the_report_changes() {
        let mut r = report(10);
        let ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        s.ensure(&ctx(&r, &ed));
        s.set_kind(Kind::Added, false);
        s.ensure(&ctx(&r, &ed));
        let shown = s.shown();
        s.ensure(&ctx(&r, &ed));
        assert_eq!(s.shown(), shown, "filters survive a repeated frame");
        r.changes.push(change(99, "Walls", "height [m]", "3"));
        s.ensure(&ctx(&r, &ed));
        assert_eq!(s.total(), 13);
        assert_eq!(s.shown(), shown + 1, "filters survive a new report");
    }

    #[test]
    fn taken_conflict_is_listed_once() {
        let mut r = Report::default();
        r.changes.push(change(1, "Walls", "height [m]", "4"));
        let mut cf = Conflict {
            sheet: "Walls".into(),
            row: 3,
            col: 5,
            column: "height [m]".into(),
            id: Some(1),
            base: "3".into(),
            book: "4".into(),
            model: "3.5".into(),
            take_book: true,
        };
        r.conflicts.push(cf.clone());
        let ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        s.ensure(&ctx(&r, &ed));
        assert_eq!((s.count(Kind::Changed), s.count(Kind::Conflict)), (0, 1));
        cf.take_book = false;
        r.conflicts[0] = cf;
        let mut s = DiffState::default();
        s.ensure(&ctx(&r, &ed));
        assert_eq!((s.count(Kind::Changed), s.count(Kind::Conflict)), (1, 1));
    }

    #[test]
    fn labels_are_looked_up_once_per_report_and_revision() {
        let r = report(20);
        let ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        let cx = ctx(&r, &ed);
        s.ensure(&cx);
        let row = (0..s.shown()).find(|i| s.at(&r, *i).label_of.is_some()).unwrap();
        for _ in 0..30 {
            let _ = s.cell_text(&cx, row, 4);
        }
        assert_eq!(s.label_runs(), 1, "frames reuse the cache");
        let cx2 = Ctx { gen: 1, ..ctx(&r, &ed) };
        s.ensure(&cx2);
        let _ = s.cell_text(&cx2, row, 4);
        assert_eq!(s.label_runs(), 2, "a new report looks up again");
        let cx3 = Ctx { gen: 1, rev: (cx2.rev.0 + 1, cx2.rev.1), ..ctx(&r, &ed) };
        s.ensure(&cx3);
        let _ = s.cell_text(&cx3, row, 4);
        assert_eq!(s.label_runs(), 3, "a model edit looks up again");
    }

    #[test]
    fn labels_look_up_again_when_an_older_revision_is_put_back() {
        let r = report(20);
        let mut ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        let cx = ctx(&r, &ed);
        s.ensure(&cx);
        let row = (0..s.shown()).find(|i| s.at(&r, *i).label_of.is_some()).unwrap();
        let _ = s.cell_text(&cx, row, 4);
        assert_eq!(s.label_runs(), 1);
        let rev = ed.revision;
        ed.model.add_level("Zz Walk", 9.0).unwrap();
        ed.reload().unwrap();
        ed.restore_revision(rev);
        let cx = ctx(&r, &ed);
        s.ensure(&cx);
        let _ = s.cell_text(&cx, row, 4);
        assert_eq!(s.label_runs(), 2, "keyed on cache_key");
        s.ensure(&cx);
        let _ = s.cell_text(&cx, row, 4);
        assert_eq!(s.label_runs(), 2, "an unrelated frame keeps the labels");
    }

    #[test]
    fn hundred_thousand_rows_build_only_visible_ones() {
        let r = report(100_000);
        let ed = Editor::new_in_memory().unwrap();
        let mut s = DiffState::default();
        let mut th = Theme::new(Mode::Light, Density::Comfortable);
        let ctx_ui = egui::Context::default();
        let _ = ctx_ui.run(Default::default(), |c| {
            th.apply(c);
        });
        let started = web_time::Instant::now();
        for _ in 0..2 {
            let _ = ctx_ui.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    s.show(ui, &th, &ctx(&r, &ed), 300.0);
                });
            });
        }
        assert_eq!(s.total(), 100_002);
        assert!(s.built > 0 && s.built < 7 * 60, "built {} cells", s.built);
        assert!(s.label_runs() < 60, "labels only for visible rows");
        assert!(started.elapsed().as_secs() < 10, "first frames of 100k rows: {:?}", started.elapsed());
        // filter and sort on the large report
        s.set_kind(Kind::Changed, false);
        s.set_sort(Some(Sort { col: 3, desc: true }));
        s.ensure(&ctx(&r, &ed));
        assert!(s.shown() < s.total() && s.shown() > 0);
    }

    #[test]
    fn empty_way_out_resets_every_filter() {
        let mut d = DiffState::default();
        d.search = "abc".into();
        d.sheet = 1;
        d.kinds[0] = false;
        assert!(d.filtered());
        d.clear_filters();
        assert!(!d.filtered());
        assert!(d.search.is_empty() && d.sheet == 0 && d.kinds.iter().all(|k| *k));
    }
}
