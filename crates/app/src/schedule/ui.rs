// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Schedule widget: category tabs, header with sorting, filter row, virtualized rows
//! (only visible ones are drawn), cell editing on double click, totals;
//! groups with subtotals, computed columns and phase selection are per tab.

use super::extra::PhaseMode;
use super::formula::Calc;
use super::group::Line;
use super::*;
use ddc_ui::Theme;
use egui::{self, Align2, Rect, Sense, Vec2};
use std::collections::{HashMap, HashSet};

mod cols;
mod cursor;
mod toolbar;
mod import;
pub mod preset;
#[cfg(test)]
mod import_tests;
pub(crate) use cursor::grid_focused;

#[derive(Clone, Debug, Default)]
struct TabState {
    /// Dragged column widths by column key; other columns keep the built width.
    widths: std::collections::HashMap<String, f32>,
    sort: Option<(usize, bool)>,
    filters: Vec<String>,
    hidden: Vec<bool>,
    /// Grouping column keys per level.
    group: Vec<String>,
    subtotals: bool,
    calcs: Vec<Calc>,
    phase: PhaseMode,
}

#[derive(Clone, Debug)]
enum Editing {
    Text(usize, usize, String, bool),
    Choice(usize, usize),
}

pub enum Action {
    /// New plan/3D selection (rows that have elements).
    Select(Vec<Sel>),
    Edit(usize, usize, String),
    Export(std::path::PathBuf),
    Copy,
    /// Ctrl+V: a TSV block from the active cell.
    Paste(String),
    /// Ctrl+D: the active cell down the selected rows.
    Fill,
    /// A book was picked for import: read it and show the counts.
    Import(std::path::PathBuf),
    /// The import preview was accepted: write it as one undo step.
    ImportApply,
}

/// A read book waiting in the import preview.
pub struct Pending {
    pub path: std::path::PathBuf,
    pub sheet: ddc_io::xlsx::ReadSheet,
    pub plan: super::xlsx_in::Plan,
    /// The book's export record; Apply plans three-way against it again.
    pub key: Option<super::xlsx_in::Key>,
    /// The file as it was read: a book saved again since is read anew, never applied stale.
    pub stamp: Option<ddc_io::xlsx::Stamp>,
    /// Above the counts: this preview is of a book saved again after the first one.
    pub note: Option<String>,
}

pub struct View {
    pub cat: Cat,
    tabs: Vec<TabState>,
    pub table: Option<Table>,
    key: Option<((u64, u64), Cat, Units, (usize, usize), bool)>,
    /// Language of the shared columns (phases, parameters): set by the shell before `sync`.
    pub ru: bool,
    /// Visible rows (indices into `table.rows`) in display order.
    pub ord: Vec<u32>,
    /// Display rows with groups; empty means no grouping (row = position in `ord`).
    lines: Vec<Line>,
    tot: Vec<Option<f64>>,
    calc_errs: Vec<Option<String>>,
    dirty: bool,
    pub show_filters: bool,
    /// Field picker window opened from the empty-state action.
    fields_open: bool,
    anchor: Option<usize>,
    /// Active cell: (table row index, column).
    active: Option<(usize, usize)>,
    editing: Option<Editing>,
    /// Selection of rows with no element in the view (rooms).
    local: HashSet<(i64, u32)>,
    last_sel: Vec<Sel>,
    sel_set: HashSet<Sel>,
    own_select: bool,
    /// Scroll offset in points for the next frame.
    pub scroll_to: Option<f32>,
    /// Horizontal scroll offset for the next frame (cursor column into view).
    scroll_x: Option<f32>,
    /// Horizontal viewport of the grid last frame: (offset, width).
    hview: (f32, f32),
    visible: std::ops::Range<usize>,
    pub export_path: String,
    /// Reply of the "Save as" dialog running off the UI thread.
    save_rx: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    /// Answer of the import "Open" dialog, off the UI thread.
    open_rx: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    pub import: Option<Pending>,
    /// The import book being read off the UI thread, and a finished read to plan.
    reading: Option<import::Reading>,
    read_done: Option<import::Done>,
    pub counts: Vec<usize>,
    /// Cells applied from each book (by file and project) this session: the base of its next
    /// import, so a value put back in the book after an Apply reads as a change.
    applied: HashMap<(std::path::PathBuf, Option<i64>), super::xlsx_in::Applied>,
}

/// One book, however its path was spelled.
fn book_id(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Default for View {
    fn default() -> Self {
        let dir = std::env::var_os("USERPROFILE")
            .map(|h| std::path::PathBuf::from(h).join("Documents"))
            .filter(|d| d.is_dir())
            .unwrap_or_else(ddc_model::temp_dir);
        Self {
            cat: Cat::Walls,
            tabs: vec![TabState::default(); Cat::ALL.len()],
            table: None,
            key: None,
            ru: true,
            ord: vec![],
            lines: vec![],
            tot: vec![],
            calc_errs: vec![],
            dirty: true,
            show_filters: true,
            fields_open: false,
            anchor: None,
            active: None,
            editing: None,
            local: HashSet::new(),
            last_sel: vec![],
            sel_set: HashSet::new(),
            own_select: false,
            scroll_to: None,
            scroll_x: None,
            hview: (0.0, f32::MAX),
            visible: 0..0,
            export_path: dir.join(crate::tr("sch.ui.file", &[])).to_string_lossy().into_owned(),
            save_rx: None,
            open_rx: None,
            import: None,
            reading: None,
            read_done: None,
            counts: vec![0; Cat::ALL.len()],
            applied: HashMap::new(),
        }
    }
}

impl View {
    /// Rebuild the table if the model, category, units or precision changed.
    pub fn sync(&mut self, ed: &Editor) {
        let rk = ed.cache_key();
        let key = (rk, self.cat, ed.tool.units, precision(), self.ru);
        if self.key == Some(key) {
            return;
        }
        if self.key.map(|k| k.0) != Some(rk) || self.counts.iter().all(|c| *c == 0) {
            self.counts = Cat::ALL.iter().map(|c| count(ed, *c)).collect();
        }
        self.key = Some(key);
        self.set_table(super::build_in(ed, self.cat, self.ru), ed.tool.units);
    }

    pub fn set_table(&mut self, mut tb: Table, u: Units) {
        let t = &mut self.tabs[tb.cat.index()];
        self.calc_errs = super::formula::append(&mut tb, &t.calcs);
        t.filters.resize(tb.cols.len(), String::new());
        t.hidden.resize(tb.cols.len(), false);
        // the active cell follows its element and column across rebuilds; gone or other category -> none
        self.active = self.active.and_then(|(ri, j)| {
            let old = self.table.as_ref().filter(|o| o.cat == tb.cat)?;
            let (r, c) = (old.rows.get(ri)?, old.cols.get(j)?);
            let ni = tb.rows.iter().position(|x| x.id == r.id && x.sub == r.sub)?;
            let nj = tb.cols.iter().position(|x| x.key == c.key)?;
            Some((ni, nj))
        });
        self.cat = tb.cat;
        self.table = Some(tb);
        self.dirty = true;
        self.reorder(u);
    }

    fn reorder(&mut self, u: Units) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let t = &self.tabs[self.cat.index()];
        let Some(tb) = self.table.as_ref() else {
            (self.ord, self.lines, self.tot) = (vec![], vec![], vec![]);
            return;
        };
        let mut ord = order(tb, &t.filters, t.sort, u);
        if t.phase != PhaseMode::All {
            ord.retain(|&i| super::extra::phase_ok(tb, i as usize, t.phase));
        }
        let by: Vec<usize> = t
            .group
            .iter()
            .filter_map(|k| tb.cols.iter().position(|c| &c.key == k))
            .collect();
        super::group::sort(tb, &mut ord, &by);
        self.lines = super::group::lines(tb, &ord, &by, t.subtotals, u);
        self.ord = ord;
        self.tot = self
            .table
            .as_ref()
            .map_or(vec![], |tb| totals(tb, &self.ord));
    }

    pub fn visible_cols(&self) -> Vec<usize> {
        let t = &self.tabs[self.cat.index()];
        let n = self.table.as_ref().map_or(0, |tb| tb.cols.len());
        (0..n)
            .filter(|j| !t.hidden.get(*j).copied().unwrap_or(false))
            .collect()
    }

    fn is_on(&self, r: &Row) -> bool {
        on_row(&self.sel_set, &self.local, r)
    }

    /// Selected visible rows in order; if none are selected, all visible rows.
    pub fn copy_rows(&self) -> Vec<u32> {
        let Some(tb) = &self.table else { return vec![] };
        let on: Vec<u32> = self
            .ord
            .iter()
            .copied()
            .filter(|&i| self.is_on(&tb.rows[i as usize]))
            .collect();
        if on.is_empty() {
            self.ord.clone()
        } else {
            on
        }
    }

    pub fn tsv(&self, u: Units) -> String {
        self.table.as_ref().map_or(String::new(), |tb| {
            tsv(tb, &self.copy_rows(), &self.visible_cols(), u)
        })
    }

    /// Active cell place: (row place in view order, column place among visible).
    fn active_pos(&self) -> Option<(usize, usize)> {
        let (ri, j) = self.active?;
        let p = self.ord.iter().position(|&i| i as usize == ri)?;
        let q = self.visible_cols().iter().position(|&c| c == j)?;
        Some((p, q))
    }

    /// Paste a TSV block from the active cell: rows in view order, columns among visible.
    /// Each cell is checked alone; one op (one undo step). Returns the status and a skipped flag.
    pub fn paste(&self, ed: &mut Editor, text: &str) -> (String, bool) {
        let (Some(tb), Some((p0, q0))) = (&self.table, self.active_pos()) else {
            return (crate::tr("sched_edit.no_cell", &[]), true);
        };
        let rows = parse_tsv(text);
        if rows.is_empty() {
            return (crate::tr("sched_edit.nothing", &[]), true);
        }
        let vis = self.visible_cols();
        let mut cells = vec![];
        for (dr, line) in rows.iter().enumerate() {
            let Some(&ri) = self.ord.get(p0 + dr) else { break };
            for (dc, f) in line.iter().enumerate() {
                if let Some(&j) = vis.get(q0 + dc) {
                    cells.push((ri as usize, j, f.clone()));
                }
            }
        }
        let (o, why) = super::extra::write_cells_why(ed, tb, &cells);
        (super::extra::say_why(&o, &why, tb, false), o.skipped() > 0)
    }

    /// Ctrl+D: the active cell value into all selected rows of the same column.
    pub fn fill_down(&self, ed: &mut Editor, u: Units) -> (String, bool) {
        let (Some(tb), Some((ri, j))) = (&self.table, self.active) else {
            return (crate::tr("sched_edit.no_cell", &[]), true);
        };
        let (Some(c), Some(v)) = (tb.cols.get(j), tb.rows.get(ri).and_then(|r| r.vals.get(j))) else {
            return (crate::tr("sched_edit.no_cell", &[]), true);
        };
        let text = edit_text(v, c.kind, u);
        let cells: Vec<(usize, usize, String)> = self
            .ord
            .iter()
            .map(|&i| i as usize)
            .filter(|&i| i != ri && self.is_on(&tb.rows[i]))
            .map(|i| (i, j, text.clone()))
            .collect();
        if cells.is_empty() {
            return (crate::tr("sched_edit.nothing", &[]), true);
        }
        let (o, why) = super::extra::write_cells_why(ed, tb, &cells);
        (super::extra::say_why(&o, &why, tb, true), o.skipped() > 0)
    }

    fn note_selection(&mut self, sel: &[Sel], rh: f32) {
        if sel == self.last_sel.as_slice() {
            return;
        }
        self.last_sel = sel.to_vec();
        self.sel_set = sel.iter().copied().collect();
        if std::mem::take(&mut self.own_select) || sel.is_empty() {
            return;
        }
        let Some(tb) = &self.table else { return };
        let first = self.ord.iter().position(|&i| {
            tb.rows[i as usize]
                .sel
                .is_some_and(|s| self.sel_set.contains(&s))
        });
        let first = first.map(|p| self.line_of(p));
        if let Some(p) = first.filter(|p| !self.visible.contains(p)) {
            self.scroll_to = Some(((p as f32 - 2.0).max(0.0)) * rh);
        }
    }

    /// Screenshot: walls grouped by construction phase, group totals, a formula.
    pub fn demo_setup(&mut self) {
        self.cat = Cat::Walls;
        let t = &mut self.tabs[Cat::Walls.index()];
        t.group = vec![super::extra::PH_CREATED.into()];
        t.subtotals = true;
        t.calcs = vec![Calc {
            name: crate::tr("sch.ui.demo_calc", &[]),
            expr: "[side] * 2".into(),
            kind: Kind::Area,
        }];
        self.key = None;
    }

    /// Display row for a position in `ord`.
    /// Group headers and subtotals as shown; empty without grouping.
    pub fn group_lines(&self) -> &[Line] {
        &self.lines
    }

    fn line_of(&self, pos: usize) -> usize {
        if self.lines.is_empty() {
            return pos;
        }
        self.lines
            .iter()
            .position(|l| *l == Line::Row(pos))
            .unwrap_or(pos)
    }

    fn click(&mut self, pos: usize, m: egui::Modifiers, sel: &[Sel]) -> Option<Vec<Sel>> {
        let tb = self.table.as_ref()?;
        let row = &tb.rows[self.ord[pos] as usize];
        let range = match (m.shift, self.anchor) {
            (true, Some(a)) => a.min(pos)..=a.max(pos),
            _ => pos..=pos,
        };
        if !m.shift {
            self.anchor = Some(pos);
        }
        let Some(s) = row.sel else {
            let k = (row.id, row.sub);
            if !m.command && !m.shift {
                self.local.clear();
            }
            if m.command && self.local.contains(&k) {
                self.local.remove(&k);
            } else {
                for p in range {
                    let r = &tb.rows[self.ord[p] as usize];
                    self.local.insert((r.id, r.sub));
                }
            }
            return None;
        };
        let mut out: Vec<Sel> = if m.command || m.shift {
            sel.to_vec()
        } else {
            vec![]
        };
        if m.command && !m.shift && out.contains(&s) {
            out.retain(|x| *x != s);
        } else {
            for p in range {
                if let Some(x) = tb.rows[self.ord[p] as usize].sel {
                    if !out.contains(&x) {
                        out.push(x);
                    }
                }
            }
        }
        self.own_select = true;
        Some(out)
    }

    /// Draws the schedule over the whole available area; `sel` is the plan/3D selection.
    pub fn show(&mut self, ui: &mut egui::Ui, th: &Theme, sel: &[Sel], u: Units) -> Vec<Action> {
        let mut acts = vec![];
        if let Some(p) = self.poll_save() {
            acts.push(Action::Export(p));
        }
        if let Some(p) = self.poll_open() {
            acts.push(Action::Import(p));
        }
        self.poll_read(&mut acts);
        if self.save_rx.is_some() || self.open_rx.is_some() || self.reading.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.reading_window(ui.ctx());
        self.import_window(ui.ctx(), &mut acts);
        let body = egui::TextStyle::Body.resolve(ui.style());
        let rh = (ui.fonts(|f| f.row_height(&body)) + 8.0).round();
        self.note_selection(sel, rh);
        let (sec, pri, acc) = (th.c("text.secondary"), th.c("text.primary"), th.c("accent"));

        // tabs and actions
        let new_cat = self.tabs_row(ui, th);
        self.toolbar(ui, th, u, &mut acts);
        if let Some(c) = new_cat {
            self.cat = c;
            self.key = None;
            self.editing = None;
            self.anchor = None;
            self.scroll_to = Some(0.0);
        }
        self.cursor_keys(ui.ctx(), rh, u);
        let Some(tb) = self.table.as_ref().filter(|t| t.cat == self.cat) else {
            return acts;
        };
        let cols = self.visible_cols();
        let ws = self.col_widths();
        let xs = cols::edges(&ws, &cols);
        let total_w = *xs.last().unwrap();
        let ti = self.cat.index();
        let mut edge = cols::EdgeOut {
            width: None,
            fit: None,
        };
        let mut resort = None;
        let mut refilter = false;
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        let mut start_edit = None;
        let mut new_active = None;
        let mut commit = None;
        let mut cancel = false;
        let stripe = th.c("bg.sunken");
        let sel_bg = th.c("accent.subtle");
        let border = th.c("border.subtle");
        let editing = self.editing.clone();
        let active = self.active;
        let tot = &self.tot;
        let mut vis = 0..0;
        let (sel_set, local) = (&self.sel_set, &self.local);
        let (ord, lines) = (&self.ord, &self.lines);

        let mut hs = egui::ScrollArea::horizontal()
            .id_salt("ddc.table.h")
            .auto_shrink(false);
        if let Some(x) = self.scroll_x.take() {
            hs = hs.horizontal_scroll_offset(x);
        }
        let hout = hs.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.set_min_width(total_w);
                // header
                let (hr, _) = ui.allocate_exact_size(Vec2::new(total_w, rh), Sense::hover());
                let p = ui.painter();
                p.line_segment([hr.left_bottom(), hr.right_bottom()], (1.0, border));
                for (k, &j) in cols.iter().enumerate() {
                    let c = &tb.cols[j];
                    let r = Rect::from_x_y_ranges(
                        hr.left() + xs[k]..=hr.left() + xs[k + 1],
                        hr.y_range(),
                    );
                    let resp = ui.interact(r, ui.id().with(("hdr", j)), Sense::click());
                    if resp.clicked() {
                        resort = Some(j);
                    }
                    let arrow = match self.tabs[ti].sort {
                        Some((s, true)) if s == j => " ▲",
                        Some((s, false)) if s == j => " ▼",
                        _ => "",
                    };
                    let label = format!("{}{arrow}", header(c, u));
                    let col = if c.edit == Edit::No { sec } else { acc };
                    let cut = cols::paint_cell(
                        ui,
                        r,
                        0.0,
                        &label,
                        body.clone(),
                        col,
                        c.kind.numeric(),
                    );
                    let hint = match c.edit {
                        Edit::No => crate::tr("sch.ui.hdr_sort", &[("col", &header(c, u))]),
                        _ => crate::tr("sch.ui.hdr_edit", &[("col", &header(c, u))]),
                    };
                    let near_edge = ui
                        .input(|i| i.pointer.hover_pos())
                        .is_some_and(|q| cols::hit_edge(q.x - hr.left(), &xs, cols::SLOP).is_some());
                    if !near_edge {
                        resp.on_hover_text(if cut {
                            format!("{}\n{hint}", header(c, u))
                        } else {
                            hint
                        });
                    }
                }
                for (k, &j) in cols.iter().enumerate() {
                    cols::edge_widget(ui, hr, hr.left() + xs[k + 1], j, ws[j], acc, &mut edge);
                }
                // filters
                if self.show_filters {
                    let fh = th.d().control;
                    let (fr, _) = ui.allocate_exact_size(Vec2::new(total_w, fh), Sense::hover());
                    for (k, &j) in cols.iter().enumerate() {
                        let r = Rect::from_x_y_ranges(
                            fr.left() + xs[k] + 2.0..=fr.left() + xs[k + 1] - 2.0,
                            fr.top()..=fr.bottom(),
                        );
                        let hint = if tb.cols[j].kind.numeric() {
                            ">0  <10  =5".to_string()
                        } else {
                            crate::tr("sch.ui.filter_hint", &[])
                        };
                        let buf = &mut self.tabs[ti].filters[j];
                        let out = ui
                            .scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                                ddc_ui::widgets::TextField::new(buf)
                                    .id_salt(("ddc.sch.filter", j))
                                    .hint(&hint)
                                    .labelled_by(egui::Id::new(("ddc.sch.filter.lbl", j)), &hint)
                                    .width(r.width())
                                    .show(ui, th)
                            })
                            .inner;
                        if out.changed {
                            refilter = true;
                        }
                    }
                }
                // rows: visible only
                let foot = rh + 2.0;
                let mut sa = egui::ScrollArea::vertical()
                    .id_salt("ddc.table.rows")
                    .auto_shrink(false)
                    .max_height((ui.available_height() - foot).max(rh));
                if let Some(off) = self.scroll_to.take() {
                    sa = sa.vertical_scroll_offset(off);
                }
                let n_lines = if lines.is_empty() {
                    ord.len()
                } else {
                    lines.len()
                };
                if n_lines == 0 {
                    let shown_cols = (0..tb.cols.len())
                        .filter(|j| !self.tabs[ti].hidden.get(*j).copied().unwrap_or(false))
                        .count();
                    let (k, act) = empty_action(tb.rows.len(), shown_cols);
                    let clicked = match act {
                        EmptyAction::AddField => {
                            crate::ui_kit::empty_state_action(ui, k, "empty.schedule.action")
                        }
                        EmptyAction::ClearFilter => crate::ui_kit::empty_state(ui, k),
                    };
                    if clicked {
                        match act {
                            EmptyAction::AddField => self.fields_open = true,
                            EmptyAction::ClearFilter => {
                                self.tabs[ti].filters.iter_mut().for_each(String::clear);
                                refilter = true;
                            }
                        }
                    }
                }
                if self.fields_open {
                    let mut open = true;
                    crate::ui_kit::Dialog::new(
                        "sch.fields",
                        crate::tr("sched_toolbar.fields_title", &[]),
                        crate::ui_kit::DialogSize::Form,
                    )
                        .id(egui::Id::new("ddc.table.fields"))
                        .open(&mut open)
                        .scroll_body()
                        .show(ui.ctx(), |ui| {
                            for (j, c) in tb.cols.iter().enumerate() {
                                let mut on = !self.tabs[ti].hidden[j];
                                if ddc_ui::widgets::checkbox(ui, th, &mut on, &header(c, u))
                                    .changed()
                                {
                                    self.tabs[ti].hidden[j] = !on;
                                }
                            }
                        });
                    self.fields_open = open;
                }
                let grid = sa.show_rows(ui, rh, n_lines, |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    vis = range.clone();
                    for li in range {
                        let pos = match lines.get(li) {
                            None => li,
                            Some(Line::Row(p)) => *p,
                            Some(l) => {
                                let (rr, _) =
                                    ui.allocate_exact_size(Vec2::new(total_w, rh), Sense::hover());
                                let colors = (pri, sec, stripe);
                                group_line(ui, rr, l, tb, &cols, &xs, &body, colors, u);
                                continue;
                            }
                        };
                        let ri = ord[pos] as usize;
                        let row = &tb.rows[ri];
                        let (rr, resp) =
                            ui.allocate_exact_size(Vec2::new(total_w, rh), Sense::click());
                        let on = on_row(sel_set, local, row);
                        let p = ui.painter();
                        if on {
                            p.rect_filled(rr, 0.0, sel_bg);
                        } else if pos % 2 == 1 {
                            p.rect_filled(rr, 0.0, stripe);
                        }
                        if resp.hovered() && !on {
                            p.rect_stroke(rr, 0.0, (1.0, border), egui::StrokeKind::Inside);
                        }
                        let px = resp.interact_pointer_pos().map(|q| q.x - rr.left());
                        let col_at = px.and_then(|x| (0..cols.len()).find(|&k| x < xs[k + 1]));
                        if resp.clicked() || resp.double_clicked() {
                            new_active = col_at.map(|k| (ri, cols[k]));
                        }
                        if resp.double_clicked() {
                            if let Some(k) = col_at.filter(|&k| tb.cols[cols[k]].edit != Edit::No) {
                                start_edit = Some((ri, cols[k]));
                            }
                        } else if resp.clicked() {
                            clicked = Some((pos, ui.input(|i| i.modifiers)));
                        }
                        let mut tip = None;
                        let hover = ui.input(|i| i.pointer.hover_pos());
                        for (k, &j) in cols.iter().enumerate() {
                            let c = &tb.cols[j];
                            let r = Rect::from_x_y_ranges(
                                rr.left() + xs[k]..=rr.left() + xs[k + 1],
                                rr.y_range(),
                            );
                            if let Some(Editing::Text(er, ec, _, _)) = &editing {
                                if *er == ri && *ec == j {
                                    continue;
                                }
                            }
                            let s = fmt(&row.vals[j], c.kind, u);
                            let cut = cols::paint_cell(
                                ui,
                                r,
                                0.0,
                                &s,
                                body.clone(),
                                pri,
                                c.kind.numeric(),
                            );
                            if cut && hover.is_some_and(|q| r.contains(q)) {
                                tip = Some(s);
                            }
                        }
                        if let (Some(s), true) = (tip, resp.hovered()) {
                            resp.on_hover_text(s);
                        }
                        cursor::paint_active(ui, rr, &xs, &cols, active, ri, th);
                        // cell editor over the row
                        match &editing {
                            Some(Editing::Text(er, ec, _, first)) if *er == ri => {
                                let k = cols.iter().position(|j| j == ec);
                                if let (Some(k), Some(Editing::Text(_, _, buf, _))) =
                                    (k, self.editing.as_mut())
                                {
                                    let r = Rect::from_x_y_ranges(
                                        rr.left() + xs[k] + 1.0..=rr.left() + xs[k + 1] - 1.0,
                                        rr.top() + 1.0..=rr.bottom() - 1.0,
                                    );
                                    let out = ui.put(
                                        r,
                                        egui::TextEdit::singleline(buf).desired_width(r.width()),
                                    );
                                    if *first {
                                        out.request_focus();
                                    }
                                    let (enter, esc) = ui.input(|i| {
                                        (
                                            i.key_pressed(egui::Key::Enter),
                                            i.key_pressed(egui::Key::Escape),
                                        )
                                    });
                                    if esc {
                                        cancel = true;
                                    } else if out.lost_focus() || enter {
                                        commit = Some((ri, *ec, buf.clone()));
                                    }
                                }
                            }
                            Some(Editing::Choice(er, ec)) if *er == ri => {
                                if let Some(k) = cols.iter().position(|j| j == ec) {
                                    let at = egui::pos2(rr.left() + xs[k], rr.bottom());
                                    let a = egui::Area::new(ui.id().with("ddc.table.choice"))
                                        .order(egui::Order::Foreground)
                                        .fixed_pos(at)
                                        .show(ui.ctx(), |ui| {
                                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                                ui.set_min_width(xs[k + 1] - xs[k]);
                                                let cur = fmt(&row.vals[*ec], Kind::Text, u);
                                                for ch in &tb.cols[*ec].choices {
                                                    if ui.selectable_label(*ch == cur, ch).clicked()
                                                    {
                                                        commit = Some((ri, *ec, ch.clone()));
                                                    }
                                                }
                                            });
                                        });
                                    let outside = ui.input(|i| {
                                        i.pointer.any_pressed()
                                            && i.pointer
                                                .interact_pos()
                                                .is_some_and(|q| !a.response.rect.contains(q))
                                    });
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape))
                                        || (outside && commit.is_none())
                                    {
                                        cancel = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                });
                cursor::register(ui, grid.inner_rect);
                // totals
                let (fr, _) = ui.allocate_exact_size(Vec2::new(total_w, foot), Sense::hover());
                let p = ui.painter();
                p.line_segment([fr.left_top(), fr.right_top()], (1.0, border));
                for (k, &j) in cols.iter().enumerate() {
                    let c = &tb.cols[j];
                    let r = Rect::from_x_y_ranges(
                        fr.left() + xs[k]..=fr.left() + xs[k + 1],
                        fr.y_range(),
                    );
                    let tj = tot.get(j).copied().flatten();
                    let s = match tj {
                        Some(x) => fmt(&Val::N(x), c.kind, u),
                        None if k == 0 => crate::tr("sch.total", &[("n", &fmt_num(ord.len() as f64, 0))]),
                        None => continue,
                    };
                    let f = egui::FontId::new(body.size, body.family.clone());
                    let right = c.kind.numeric() && tj.is_some();
                    cols::paint_cell(ui, r, 0.0, &s, f, pri, right);
                }
            });
        self.hview = (hout.state.offset.x, hout.inner_rect.width());
        crate::ui_kit::mark_hscroll_viewport(ui.ctx(), hout.inner_rect);
        self.visible = vis;
        if let Some(Editing::Text(_, _, _, first)) = self.editing.as_mut() {
            *first = false;
        }
        if let Some((ri, j)) = start_edit {
            let c = &tb.cols[j];
            self.editing = Some(match c.edit {
                Edit::Choice => Editing::Choice(ri, j),
                _ => Editing::Text(ri, j, edit_text(&tb.rows[ri].vals[j], c.kind, u), true),
            });
        }
        if let Some((ri, j, text)) = commit {
            self.editing = None;
            acts.push(Action::Edit(ri, j, text));
            self.after_edit(ui.ctx(), Some((ri, j)), rh);
        } else if cancel {
            self.editing = None;
            self.after_edit(ui.ctx(), None, rh);
        }
        if new_active.is_some() {
            self.active = new_active;
            cursor::take_focus(ui);
        }
        if let Some((pos, m)) = clicked {
            if let Some(s) = self.click(pos, m, sel) {
                acts.push(Action::Select(s));
            }
        }
        if let Some((j, w)) = edge.width {
            self.set_col_width(j, w);
        }
        if let Some(j) = edge.fit {
            let w = self.fit_width(ui, j, u, &body);
            self.set_col_width(j, w);
        }
        if let Some(j) = resort {
            let t = &mut self.tabs[ti];
            t.sort = match t.sort {
                Some((s, true)) if s == j => Some((j, false)),
                Some((s, false)) if s == j => None,
                _ => Some((j, true)),
            };
            self.dirty = true;
        }
        if refilter {
            self.dirty = true;
        }
        if self.dirty {
            self.anchor = None;
            self.reorder(u);
        }
        acts
    }
}

impl View {
    /// Opens "Save as" on a separate thread so the egui frame is not blocked.
    fn start_save(&mut self, ctx: &egui::Context) {
        if self.save_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (cur, ctx) = (self.export_path.clone(), ctx.clone());
        let wake = ctx.clone();
        // a file dialog only; a crash drops the sender, which poll_save reads as "cancelled"
        let _ = crate::jobs::spawn_guarded(
            "sched-save-dialog",
            move || {
                let _ = tx.send(save_dialog(&cur));
                ctx.request_repaint();
            },
            move |_| wake.request_repaint(),
        );
        self.save_rx = Some(rx);
    }

    /// Takes the dialog result if ready; cancellation or a dropped thread clears the pending state.
    fn poll_save(&mut self) -> Option<std::path::PathBuf> {
        use std::sync::mpsc::TryRecvError::{Disconnected, Empty};
        let got = match self.save_rx.as_ref()?.try_recv() {
            Ok(p) => p,
            Err(Empty) => return None,
            Err(Disconnected) => None,
        };
        self.save_rx = None;
        let p = got?;
        self.export_path = p.to_string_lossy().into_owned();
        Some(p)
    }
}

impl View {
    /// Import "Open" dialog in its own thread: the egui frame is not blocked.
    fn start_open(&mut self, ctx: &egui::Context) {
        if self.open_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (cur, ctx) = (self.export_path.clone(), ctx.clone());
        let wake = ctx.clone();
        // a file dialog only; a crash drops the sender, which poll_open reads as "cancelled"
        let _ = crate::jobs::spawn_guarded(
            "sched-open-dialog",
            move || {
                let _ = tx.send(open_dialog(&cur));
                ctx.request_repaint();
            },
            move |_| wake.request_repaint(),
        );
        self.open_rx = Some(rx);
    }

    fn poll_open(&mut self) -> Option<std::path::PathBuf> {
        use std::sync::mpsc::TryRecvError::{Disconnected, Empty};
        let got = match self.open_rx.as_ref()?.try_recv() {
            Ok(p) => p,
            Err(Empty) => return None,
            Err(Disconnected) => None,
        };
        self.open_rx = None;
        got
    }
}

/// TSV block: lines by newline, fields by tab; a trailing newline is not a line.
fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let t = text.trim_end_matches(['\r', '\n']);
    if t.is_empty() {
        return vec![];
    }
    t.split('\n')
        .map(|l| l.trim_end_matches('\r').split('\t').map(str::to_string).collect())
        .collect()
}

fn kind_name(k: Kind) -> String {
    crate::tr(
        match k {
            Kind::Num | Kind::Text => "sch.kind.num",
            Kind::Int => "sch.kind.int",
            Kind::Len => "sch.kind.len",
            Kind::Area => "sch.kind.area",
            Kind::Vol => "sch.kind.vol",
            Kind::Pct => "sch.kind.pct",
            Kind::Mass => "sch.kind.mass",
        },
        &[],
    )
}

/// A group header or its subtotal.
#[allow(clippy::too_many_arguments)]
fn group_line(
    ui: &egui::Ui,
    rr: Rect,
    l: &Line,
    tb: &Table,
    cols: &[usize],
    xs: &[f32],
    body: &egui::FontId,
    (pri, sec, fill): (egui::Color32, egui::Color32, egui::Color32),
    u: Units,
) {
    let p = ui.painter();
    match l {
        Line::Head { depth, text, n } => {
            p.rect_filled(rr, 0.0, fill);
            let x = rr.left() + 6.0 + *depth as f32 * 14.0;
            p.with_clip_rect(rr).text(
                egui::pos2(x, rr.center().y),
                Align2::LEFT_CENTER,
                format!("▾ {text} · {}", fmt_num(*n as f64, 0)),
                body.clone(),
                pri,
            );
        }
        Line::Sub { depth, tot, n } => {
            for (k, &j) in cols.iter().enumerate() {
                let c = &tb.cols[j];
                let r =
                    Rect::from_x_y_ranges(rr.left() + xs[k]..=rr.left() + xs[k + 1], rr.y_range());
                let (s, right, indent) = match tot.get(j).copied().flatten() {
                    Some(v) => (fmt(&Val::N(v), c.kind, u), true, 0.0),
                    None if k == 0 => (
                        crate::tr("sch.group_total", &[("n", &fmt_num(*n as f64, 0))]),
                        false,
                        *depth as f32 * 14.0,
                    ),
                    None => continue,
                };
                cols::paint_cell(ui, r, indent, &s, body.clone(), sec, right);
            }
            p.line_segment(
                [rr.left_bottom(), rr.right_bottom()],
                (0.5, sec.gamma_multiply(0.4)),
            );
        }
        Line::Row(_) => {}
    }
}

fn on_row(sel: &HashSet<Sel>, local: &HashSet<(i64, u32)>, r: &Row) -> bool {
    match r.sel {
        Some(s) => sel.contains(&s),
        None => local.contains(&(r.id, r.sub)),
    }
}

/// System "Save as" window (.xlsx); `None` if cancelled or the window is unavailable.
pub(crate) fn save_dialog(current: &str) -> Option<std::path::PathBuf> {
    let cur = std::path::Path::new(current);
    let mut d = crate::file_pick::Dlg::new()
        .set_title(crate::tr("sch.ui.save_title", &[]))
        .add_filter("XLSX", &["xlsx"]);
    if let Some(dir) = cur.parent().filter(|d| d.is_dir()) {
        d = d.set_directory(dir);
    }
    if let Some(n) = cur.file_name() {
        d = d.set_file_name(n.to_string_lossy());
    }
    d.save_file().map(|p| {
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
        {
            p
        } else {
            p.with_extension("xlsx")
        }
    })
}

/// What the empty schedule offers: no rows or no visible fields -> add a field; rows hidden by a
/// filter -> clear the filter. Returns the empty-state key and the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyAction {
    AddField,
    ClearFilter,
}

fn empty_action(rows: usize, visible_cols: usize) -> (&'static str, EmptyAction) {
    if visible_cols == 0 {
        ("schedule_fields", EmptyAction::AddField)
    } else if rows == 0 {
        ("schedule", EmptyAction::AddField)
    } else {
        ("schedule_filtered", EmptyAction::ClearFilter)
    }
}

/// System "Open" dialog for a schedule book (.xlsx); `None` when cancelled.
pub(crate) fn open_dialog(current: &str) -> Option<std::path::PathBuf> {
    let cur = std::path::Path::new(current);
    let mut d = crate::file_pick::Dlg::new()
        .set_title(crate::tr("sched_import.open_title", &[]))
        .add_filter("XLSX", &["xlsx"]);
    if let Some(dir) = cur.parent().filter(|d| d.is_dir()) {
        d = d.set_directory(dir);
    }
    d.pick_file()
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_schedule_action_per_state() {
        use super::{empty_action, EmptyAction::*};
        assert_eq!(empty_action(5, 0), ("schedule_fields", AddField));
        assert_eq!(empty_action(0, 4), ("schedule", AddField));
        assert_eq!(empty_action(5, 4), ("schedule_filtered", ClearFilter));
    }

    use super::*;

    #[test]
    fn save_dialog_result_is_polled_without_blocking() {
        let mut v = View::default();
        assert_eq!(v.poll_save(), None);
        let (tx, rx) = std::sync::mpsc::channel();
        v.save_rx = Some(rx);
        assert_eq!(v.poll_save(), None);
        assert!(v.save_rx.is_some(), "waiting for a reply");
        tx.send(Some("a.xlsx".into())).unwrap();
        assert_eq!(v.poll_save(), Some("a.xlsx".into()));
        assert!(v.save_rx.is_none());
        assert_eq!(v.export_path, "a.xlsx");
        let (tx, rx) = std::sync::mpsc::channel::<Option<std::path::PathBuf>>();
        v.save_rx = Some(rx);
        drop(tx);
        assert_eq!(v.poll_save(), None);
        assert!(v.save_rx.is_none(), "a dropped channel clears the wait");
    }

    fn synth(n: usize) -> Table {
        use Edit as E;
        let cols = vec![
            col("id", "№", Kind::Int, E::No),
            col("type", "Type", Kind::Text, E::Choice),
            col("len", "Length", Kind::Len, E::Num),
            col("h", "Height", Kind::Len, E::Num),
            col("th", "Thickness", Kind::Len, E::No),
            col("plan", "Plan area", Kind::Area, E::No),
            col("side", "Side face area", Kind::Area, E::No),
            col("vol", "Volume", Kind::Vol, E::No),
            col("ops", "Openings", Kind::Int, E::No),
        ];
        let rows = (0..n)
            .map(|i| {
                let l = 1.0 + (i % 97) as f64 * 0.137;
                Row {
                    sel: Some(Sel::Wall(i as i64 + 1)),
                    id: i as i64 + 1,
                    sub: 0,
                    vals: vec![
                        Val::N(i as f64 + 1.0),
                        Val::T(format!("Masonry {}", 100 + (i % 7) * 50)),
                        Val::N(l),
                        Val::N(3.0),
                        Val::N(0.2),
                        Val::N(l * 0.2),
                        Val::N(l * 3.0),
                        Val::N(l * 0.6),
                        Val::N((i % 3) as f64),
                    ],
                }
            })
            .collect();
        Table {
            cat: Cat::Walls,
            cols,
            rows,
            row_phase: vec![None; n],
            phases: vec![],
        }
    }

    fn input(t: f64) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 900.0),
            )),
            time: Some(t),
            ..Default::default()
        }
    }

    /// 100,000 rows: frame-by-frame scrolling fits in 60 fps (UI + tessellation).
    #[test]
    fn scroll_100k_rows_frame_time() {
        scroll_100k(false);
    }

    /// Strict budget, 16.6 ms at p95; runs in `cargo test --release` (wall-clock budgets mean
    /// nothing in a loaded debug build).
    #[test]
    #[cfg(not(debug_assertions))]
    fn scroll_100k_rows_frame_time_release() {
        scroll_100k(true);
    }

    fn scroll_100k(strict: bool) {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let mut v = View::default();
        let t0 = web_time::Instant::now();
        v.set_table(synth(100_000), Units::Metric);
        let build_ms = t0.elapsed().as_secs_f64() * 1e3;
        let ctx = ddc_ui::theme::headless_ctx();
        let sel = vec![Sel::Wall(5), Sel::Wall(50_000)];
        let mut times = vec![];
        for f in 0..90 {
            v.scroll_to = Some(f as f32 * 1111.0);
            let t = web_time::Instant::now();
            let out = ctx.run(input(f as f64 / 60.0), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    v.show(ui, &th, &sel, Units::Metric);
                });
            });
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
            // rows laid out per frame is O(visible) and independent of machine load
            assert!(v.visible.len() < 80, "frame {f}: laid out {:?}", v.visible);
            if f >= 10 {
                times.push(t.elapsed().as_secs_f64() * 1e3);
            }
        }
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        let p95 = times[times.len() * 95 / 100];
        println!(
            "100k rows: build+order {build_ms:.1} ms · frame mean {mean:.2} ms · p95 {p95:.2} ms"
        );
        assert!(v.visible.len() < 80, "virtualized: {:?}", v.visible);
        assert!(v.visible.start > 0, "scrolled");
        // A debug build is ~3x slower; the median is robust to machine load.
        let budget = if cfg!(debug_assertions) {
            16.6 * 3.0
        } else {
            16.6
        };
        let med = times[times.len() / 2];
        if strict {
            assert!(p95 < 16.6, "p95 frame {p95:.2} ms");
        } else {
            assert!(med < budget, "median frame {med:.2} ms > {budget:.1}");
        }
        // sort 100k rows by text
        let t = web_time::Instant::now();
        v.tabs[0].sort = Some((1, true));
        v.dirty = true;
        v.reorder(Units::Metric);
        println!(
            "sort 100k by text: {:.1} ms",
            t.elapsed().as_secs_f64() * 1e3
        );
        assert_eq!(v.ord.len(), 100_000);
    }

    #[test]
    fn grouped_view_maps_lines_to_rows() {
        let mut v = View::default();
        v.tabs[0].group = vec!["type".into()];
        v.tabs[0].subtotals = true;
        v.tabs[0].calcs = vec![Calc {
            name: "Double".into(),
            expr: "len * 2".into(),
            kind: Kind::Len,
        }];
        v.set_table(synth(21), Units::Metric);
        let tb = v.table.as_ref().unwrap();
        assert_eq!(tb.cols.last().unwrap().title, "Double");
        let groups = v
            .lines
            .iter()
            .filter(|l| matches!(l, Line::Head { .. }))
            .count();
        assert_eq!(groups, 7, "seven types");
        assert_eq!(v.lines.len(), 21 + 2 * 7);
        assert!(matches!(v.lines[v.line_of(0)], Line::Row(0)));
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(input(0.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                v.show(ui, &th, &[], Units::Metric);
            });
        });
        assert!(v.calc_errs.iter().all(Option::is_none));
    }

    #[test]
    fn click_ctrl_shift_selection() {
        let mut v = View::default();
        v.set_table(synth(10), Units::Metric);
        let none = egui::Modifiers::NONE;
        let s = v.click(2, none, &[]).unwrap();
        assert_eq!(s, vec![Sel::Wall(3)]);
        let s = v.click(5, egui::Modifiers::SHIFT, &s).unwrap();
        assert_eq!(s.len(), 4);
        let s = v.click(3, egui::Modifiers::COMMAND, &s).unwrap();
        assert_eq!(s.len(), 3);
        v.note_selection(&s, 20.0);
        let rows = v.copy_rows();
        assert_eq!(rows, vec![2, 4, 5]);
        assert!(v.tsv(Units::Metric).lines().count() == 4);
    }

    /// Walls with a numeric "Price" parameter; only it and the length column are shown.
    pub(super) fn sheet() -> (Editor, View, usize, usize) {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        let pid = e
            .model
            .add_param(&ddc_model::ParamSpec {
                name: "Price".into(),
                ptype: ddc_model::ParamType::Number,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        e.reload().unwrap();
        let mut v = View::default();
        v.sync(&e);
        let tb = v.table.as_ref().unwrap();
        let jp = tb.cols.iter().position(|c| c.key == format!("p:{pid}")).unwrap();
        let jl = tb.cols.iter().position(|c| c.key == "th").unwrap();
        let n = tb.cols.len();
        v.tabs[v.cat.index()].hidden = (0..n).map(|j| j != jp && j != jl).collect();
        (e, v, jp, jl)
    }

    fn prices(e: &Editor, v: &View, jp: usize) -> Vec<Val> {
        let tb = crate::schedule::build(e, Cat::Walls);
        v.ord.iter().map(|&i| tb.rows[i as usize].vals[jp].clone()).collect()
    }

    /// Keep one column visible.
    fn only(v: &mut View, j: usize) {
        let n = v.table.as_ref().unwrap().cols.len();
        v.tabs[v.cat.index()].hidden = (0..n).map(|k| k != j).collect();
    }

    #[test]
    fn table_reads_anew_when_an_older_revision_is_put_back() {
        let (mut e, mut v, jp, _) = sheet();
        only(&mut v, jp);
        v.active = Some((v.ord[0] as usize, jp));
        let rev = e.revision;
        let (msg, warn) = v.paste(&mut e, "7\n");
        assert!(!warn && !msg.starts_with("sched_edit."), "{msg}");
        e.restore_revision(rev);
        v.sync(&e);
        let tb = v.table.as_ref().unwrap();
        assert!(tb.rows.iter().any(|r| r.vals[jp] == Val::N(7.0)), "keyed on cache_key");
    }

    #[test]
    fn table_cache_survives_an_unrelated_frame() {
        let (e, mut v, _, _) = sheet();
        v.sync(&e);
        v.table = None;
        v.sync(&e);
        assert!(v.table.is_none(), "no rebuild without a model change");
    }

    #[test]
    fn paste_block_one_undo() {
        let (mut e, mut v, jp, _) = sheet();
        only(&mut v, jp);
        v.active = Some((v.ord[0] as usize, jp));
        let u0 = e.undo_len();
        let (msg, warn) = v.paste(&mut e, "1\n2\n3\n4\n");
        assert!(!warn && !msg.starts_with("sched_edit."), "{msg}");
        v.sync(&e);
        let want: Vec<Val> = (1..=4).map(|n| Val::N(n as f64)).collect();
        assert_eq!(prices(&e, &v, jp), want);
        assert_eq!(e.undo_len(), u0 + 1, "four cells, one undo step");
        e.undo();
        v.sync(&e);
        assert!(prices(&e, &v, jp).iter().all(|x| *x == Val::T(String::new())));
        e.redo();
        v.sync(&e);
        assert_eq!(prices(&e, &v, jp), want, "redo repeats the whole block");
    }

    #[test]
    fn paste_skips_readonly_and_reports() {
        let (mut e, mut v, jp, jl) = sheet();
        let vis = v.visible_cols();
        v.active = Some((v.ord[0] as usize, vis[0]));
        let line: Vec<&str> = vis.iter().map(|&j| if j == jl { "5" } else { "9" }).collect();
        let text = format!("{0}\n{0}", line.join("\t"));
        let u0 = e.undo_len();
        let (msg, warn) = v.paste(&mut e, &text);
        assert!(warn && !msg.starts_with("sched_edit."), "{msg}");
        v.sync(&e);
        let got = prices(&e, &v, jp);
        assert_eq!(got[0], Val::N(9.0));
        assert_eq!(got[1], Val::N(9.0));
        assert_eq!(got[2], Val::T(String::new()));
        assert_eq!(e.undo_len(), u0 + 1);
        let tb = v.table.as_ref().unwrap();
        let cells: Vec<(usize, usize, String)> = vec![
            (0, jl, "5".into()),
            (1, jl, "5".into()),
            (2, jp, "3".into()),
        ];
        let o = super::super::extra::write_cells(&mut e, tb, &cells);
        assert_eq!((o.done, o.readonly, o.bad), (1, 2, 0));
    }

    #[test]
    fn paste_bad_number_skips_cell_not_block() {
        let (mut e, mut v, jp, _) = sheet();
        only(&mut v, jp);
        v.active = Some((v.ord[0] as usize, jp));
        let u0 = e.undo_len();
        let (_, warn) = v.paste(&mut e, "1\nabc\n3");
        assert!(warn);
        v.sync(&e);
        let got = prices(&e, &v, jp);
        assert_eq!(got[0], Val::N(1.0));
        assert_eq!(got[1], Val::T(String::new()), "bad cell left as it was");
        assert_eq!(got[2], Val::N(3.0));
        assert_eq!(e.undo_len(), u0 + 1);
        let tb = v.table.as_ref().unwrap();
        let o = super::super::extra::write_cells(&mut e, tb, &[(1, jp, "abc".into())]);
        assert_eq!((o.done, o.readonly, o.bad), (0, 0, 1));
    }

    #[test]
    fn fill_down_selected_rows() {
        let (mut e, mut v, jp, _) = sheet();
        let tb = v.table.take().unwrap();
        let (r0, r1, r2) = (v.ord[0] as usize, v.ord[1] as usize, v.ord[2] as usize);
        crate::schedule::apply_edit(&mut e, &tb, r0, jp, "7");
        v.table = Some(tb);
        v.sync(&e);
        v.active = Some((r0, jp));
        let tb = v.table.as_ref().unwrap();
        v.sel_set = [r0, r1, r2].iter().map(|&i| tb.rows[i].sel.unwrap()).collect();
        let u0 = e.undo_len();
        let (msg, warn) = v.fill_down(&mut e, Units::Metric);
        assert!(!warn && !msg.starts_with("sched_edit."), "{msg}");
        assert_eq!(e.undo_len(), u0 + 1);
        v.sync(&e);
        let got = prices(&e, &v, jp);
        assert_eq!(got[..3], [Val::N(7.0), Val::N(7.0), Val::N(7.0)]);
        assert_eq!(got[3], Val::T(String::new()), "unselected row untouched");
        e.undo();
        v.sync(&e);
        let got = prices(&e, &v, jp);
        assert_eq!(got[0], Val::N(7.0));
        assert_eq!(got[1], Val::T(String::new()));
    }

    fn wall_len(e: &Editor, id: i64) -> f64 {
        let w = e.walls.iter().find(|w| w.id == id).unwrap();
        (w.b - w.a).len()
    }

    /// Show only the named columns of the walls sheet.
    fn show(v: &mut View, keys: &[&str]) {
        let tb = v.table.as_ref().unwrap();
        let hidden: Vec<bool> = tb.cols.iter().map(|c| !keys.contains(&c.key.as_str())).collect();
        v.tabs[v.cat.index()].hidden = hidden;
    }

    #[test]
    fn paste_category_columns_written_one_undo() {
        let (mut e, mut v, jp, _) = sheet();
        let tb = v.table.as_ref().unwrap();
        let key_p = tb.cols[jp].key.clone();
        show(&mut v, &["len", "h", key_p.as_str()]);
        let vis = v.visible_cols();
        let col = |k: &str| v.table.as_ref().unwrap().cols.iter().position(|c| c.key == k).unwrap();
        let (jlen, jh) = (col("len"), col("h"));
        v.active = Some((v.ord[0] as usize, vis[0]));
        let line = |len: &str, h: &str, p: &str| -> String {
            let f: Vec<&str> = vis
                .iter()
                .map(|&j| if j == jlen { len } else if j == jh { h } else { p })
                .collect();
            f.join("\t")
        };
        let text = format!("{}\n{}", line("5000", "2500", "11"), line("4000", "2500", "12"));
        let units = e.tool.units;
        let (l5, l4, h25) = (
            crate::units::parse_edit_length("5000", units).unwrap(),
            crate::units::parse_edit_length("4000", units).unwrap(),
            crate::units::parse_edit_length("2500", units).unwrap(),
        );
        let ids: Vec<i64> = {
            let tb = v.table.as_ref().unwrap();
            v.ord.iter().take(2).map(|&i| tb.rows[i as usize].id).collect()
        };
        let before: Vec<(f64, f64)> = ids
            .iter()
            .map(|&id| (wall_len(&e, id), e.walls.iter().find(|w| w.id == id).unwrap().height))
            .collect();
        let u0 = e.undo_len();
        let (msg, warn) = v.paste(&mut e, &text);
        assert!(!warn && !msg.starts_with("sched_edit."), "{msg}");
        assert_eq!(e.undo_len(), u0 + 1, "len, height and price cells: one undo step");
        for (id, (len, p)) in ids.iter().zip([(l5, 11.0), (l4, 12.0)]) {
            assert!((wall_len(&e, *id) - len).abs() < 1e-6);
            assert!((e.walls.iter().find(|w| w.id == *id).unwrap().height - h25).abs() < 1e-9);
            v.sync(&e);
            let tb = v.table.as_ref().unwrap();
            let r = tb.rows.iter().find(|r| r.id == *id).unwrap();
            assert_eq!(r.vals[jp], Val::N(p));
        }
        e.undo();
        for (id, (len, h)) in ids.iter().zip(before.iter()) {
            assert!((wall_len(&e, *id) - len).abs() < 1e-9, "undo restores the length");
            assert!((e.walls.iter().find(|w| w.id == *id).unwrap().height - h).abs() < 1e-9);
        }
        v.sync(&e);
        assert!(prices(&e, &v, jp).iter().all(|x| *x == Val::T(String::new())));
        e.redo();
        assert!((wall_len(&e, ids[0]) - l5).abs() < 1e-6, "redo repeats the whole block");
    }

    #[test]
    fn paste_equal_values_reports_no_changes() {
        let (mut e, mut v, jp, _) = sheet();
        let jlen = v.table.as_ref().unwrap().cols.iter().position(|c| c.key == "len").unwrap();
        let cells = vec![(0, jp, "7".to_string()), (1, jp, "7".to_string()), (0, jlen, "5000".to_string())];
        let o = super::super::extra::write_cells(&mut e, v.table.as_ref().unwrap(), &cells);
        assert_eq!((o.done, o.readonly, o.bad, o.unchanged), (3, 0, 0, 0));
        v.sync(&e);
        let u0 = e.undo_len();
        let o = super::super::extra::write_cells(&mut e, v.table.as_ref().unwrap(), &cells);
        assert_eq!((o.done, o.readonly, o.bad, o.unchanged), (0, 0, 0, 3), "{:?}", e.status);
        assert_eq!(e.undo_len(), u0, "nothing written, no undo step");
        assert!(!o.say(false).starts_with("sched_edit."));
        assert_eq!(o.skipped(), 0, "equal values are not skipped cells");
        // through the view: no warning flag
        only(&mut v, jp);
        v.active = Some((v.ord[0] as usize, jp));
        let (msg, warn) = v.paste(&mut e, "7\n7");
        assert!(!warn && !msg.starts_with("sched_edit."), "{msg}");
        assert_eq!(e.undo_len(), u0);
        // a mixed block writes the new cell and reports the equal one
        let cells = vec![(0, jp, "7".to_string()), (2, jp, "8".to_string())];
        let o = super::super::extra::write_cells(&mut e, v.table.as_ref().unwrap(), &cells);
        assert_eq!((o.done, o.bad, o.unchanged), (1, 0, 1));
        assert_eq!(e.undo_len(), u0 + 1);
    }
}
