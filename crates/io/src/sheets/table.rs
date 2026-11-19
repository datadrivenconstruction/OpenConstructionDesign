// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tables on a sheet: schedules, revisions, the sheet list, legends. Column width is capped, text
//! wraps by words, row height follows its number of text lines. A schedule is split into
//! equal-width columns with the header repeated within the sheet; what does not fit is counted and
//! printed on the sheet, and the build reports it to the user.

use std::ops::Range;

use super::{hatch_rings, text_w, Pen};

/// Width of the legend symbol cell, mm.
const SWATCH_W: f64 = 16.0;
/// Maximum column width: longer text wraps, mm.
pub const MAX_COL_W: f64 = 80.0;
const MIN_COL_W: f64 = 8.0;
/// Gap between text lines in a cell, as a fraction of text height.
const LEAD: f64 = 0.35;
const MAX_TITLE_LINES: usize = 3;
/// Column count limit: the loop terminates even for a schedule far beyond the sheet.
const MAX_COLUMNS: usize = 256;
const EPS: f64 = 1e-9;

/// Legend symbol in the first cell of a legend row.
#[derive(Clone, Debug, PartialEq)]
pub enum Swatch {
    None,
    Hatch { angles: Vec<f64>, step: f64 },
    Solid { gray: u8 },
    Line { weight: f64 },
}

/// A table on a sheet (schedule, revisions): rows are already formatted text; `swatches` —
/// symbols per row (empty — none).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub title: String,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub swatches: Vec<Swatch>,
}

/// Where a schedule may stand: `left`, `top` — sheet edge; `right`, `bottom` — frame;
/// `stamp` — left edge and top of the title block (with revisions above it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Room {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub stamp: [f64; 2],
}

/// Split: (slot number left to right, rows) and the number of rows with no slot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub parts: Vec<(usize, Range<usize>)>,
    pub hidden: usize,
}

/// Drawing result: frame [x, y, w, h], rows off the sheet, rows with truncated text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Drawn {
    pub rect: [f64; 4],
    pub hidden: usize,
    pub clipped: usize,
}

fn is_number(s: &str) -> bool {
    let t: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

/// Word wrap; a word wider than `w` is split by characters. An empty string yields one empty line.
pub(super) fn wrap(s: &str, h: f64, w: f64) -> Vec<String> {
    // the width was built as text + 2*pad and is taken back by 2*pad: rounding must not split a word that fits
    let w = w + 1e-6;
    let mut out = vec![];
    let space = text_w(" ", h);
    for para in s.split('\n') {
        if text_w(para, h) <= w {
            out.push(para.to_string());
            continue;
        }
        let (mut line, mut lw) = (String::new(), 0.0);
        for word in para.split_whitespace() {
            let ww = text_w(word, h);
            let gap = if line.is_empty() { 0.0 } else { space };
            if lw + gap + ww <= w {
                if gap > 0.0 {
                    line.push(' ');
                }
                line.push_str(word);
                lw += gap + ww;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                lw = 0.0;
            }
            if ww <= w {
                line.push_str(word);
                lw = ww;
                continue;
            }
            for ch in word.chars() {
                let mut b = [0u8; 4];
                let cw = text_w(ch.encode_utf8(&mut b), h);
                if lw + cw > w && !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                    lw = 0.0;
                }
                line.push(ch);
                lw += cw;
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Keeps `max` lines; the last ends with "…" so the truncation is visible on paper.
fn clip_lines(lines: &mut Vec<String>, max: usize, h: f64, w: f64) -> bool {
    if lines.len() <= max {
        return false;
    }
    lines.truncate(max);
    if let Some(last) = lines.last_mut() {
        let mark = text_w("…", h);
        while !last.is_empty() && text_w(last, h) + mark > w {
            last.pop();
        }
        last.push('…');
    }
    true
}

/// Shrinks columns to a total of `max`: the widest first, never narrower than `floor`.
fn shrink(w: &mut [f64], floor: &[f64], max: f64) {
    let (mut lo, mut hi) = (0.0, w.iter().copied().fold(0.0, f64::max));
    let level = |c: f64, x: f64, f: f64| x.min(c).max(f);
    for _ in 0..40 {
        let c = (lo + hi) / 2.0;
        let sum: f64 = w.iter().zip(floor).map(|(x, f)| level(c, *x, *f)).sum();
        if sum > max {
            hi = c;
        } else {
            lo = c;
        }
    }
    for (x, f) in w.iter_mut().zip(floor) {
        *x = level(lo, *x, *f);
    }
}

fn column_widths(t: &Table, title: &str, h: f64, pad: f64, max_w: f64) -> Vec<f64> {
    let n = t
        .header
        .len()
        .max(t.rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut w = vec![0.0f64; n];
    for r in std::iter::once(&t.header).chain(&t.rows) {
        for (j, c) in r.iter().enumerate() {
            for line in c.split('\n') {
                w[j] = w[j].max(text_w(line, h));
            }
        }
    }
    let mut w: Vec<f64> = w
        .into_iter()
        .map(|x| (x + 2.0 * pad).clamp(MIN_COL_W, MAX_COL_W))
        .collect();
    let mut floor = vec![MIN_COL_W; n];
    if !t.swatches.is_empty() {
        if let (Some(first), Some(f)) = (w.first_mut(), floor.first_mut()) {
            *first = first.max(SWATCH_W);
            *f = SWATCH_W;
        }
    }
    // the title widens the last column, but not beyond the room on the sheet; beyond that it wraps
    let title_w = if title.is_empty() {
        0.0
    } else {
        text_w(title, h * 1.2) + 2.0 * pad
    };
    let sum: f64 = w.iter().sum();
    let want = title_w.min(max_w.max(sum));
    if want > sum {
        match w.last_mut() {
            Some(l) => *l += want - sum,
            None => w.push(want),
        }
    }
    if w.len() == floor.len() && w.iter().sum::<f64>() > max_w {
        shrink(&mut w, &floor, max_w);
    }
    w
}

/// Table layout at text height `h`: column widths, text lines per cell, row heights.
pub(super) struct Layout {
    pub w: Vec<f64>,
    pub heights: Vec<f64>,
    /// Table rows whose text did not fit even a whole column.
    pub clipped: usize,
    h: f64,
    pad: f64,
    title_h: f64,
    title_lines: usize,
    header_h: f64,
    header: Vec<Vec<String>>,
    cells: Vec<Vec<Vec<String>>>,
}

fn title_wrap(title: &str, th: f64, w: f64) -> Vec<String> {
    let mut lines = wrap(title, th, w.max(th));
    clip_lines(&mut lines, MAX_TITLE_LINES, th, w.max(th));
    lines
}

/// Height of a block of `n` text lines of height `h`, with padding.
fn block(h: f64, pad: f64, n: usize) -> f64 {
    let n = n.max(1) as f64;
    h * n + h * LEAD * (n - 1.0) + 2.0 * pad
}

/// Room for the "rows not shown" note at the bottom of the last column.
fn notice_h(h: f64) -> f64 {
    h * 1.8
}

impl Layout {
    /// `max_w` — available width of the whole table, `column_h` — maximum column height.
    pub(super) fn new(t: &Table, title: &str, h: f64, max_w: f64, column_h: f64) -> Self {
        let pad = h * 0.6;
        let w = column_widths(t, title, h, pad, max_w);
        let tw: f64 = w.iter().sum();
        let th = h * 1.2;
        let title_lines = if title.is_empty() {
            0
        } else {
            title_wrap(title, th, tw - 2.0 * pad).len().max(1)
        };
        let title_h = if title_lines == 0 {
            0.0
        } else {
            block(h, pad, 1) + (title_lines - 1) as f64 * th * (1.0 + LEAD)
        };
        let lines_of = |cells: &[String]| -> Vec<Vec<String>> {
            cells
                .iter()
                .enumerate()
                .map(|(j, c)| {
                    let cw = w.get(j).copied().unwrap_or(MIN_COL_W);
                    wrap(c, h, (cw - 2.0 * pad).max(h))
                })
                .collect()
        };
        let header = lines_of(&t.header);
        let tallest = |r: &[Vec<String>]| r.iter().map(Vec::len).max().unwrap_or(1);
        let header_h = if t.header.is_empty() {
            0.0
        } else {
            block(h, pad, tallest(&header))
        };
        // a row taller than a whole column is truncated with a visible mark and a message
        let room = column_h - title_h - header_h - notice_h(h);
        let max_lines = ((room - 2.0 * pad + h * LEAD) / (h * (1.0 + LEAD)) + EPS)
            .floor()
            .max(1.0) as usize;
        let mut clipped = 0;
        let mut cells = Vec::with_capacity(t.rows.len());
        let mut heights = Vec::with_capacity(t.rows.len());
        for r in &t.rows {
            let mut row = lines_of(r);
            let mut cut = false;
            for (j, lines) in row.iter_mut().enumerate() {
                let cw = w.get(j).copied().unwrap_or(MIN_COL_W);
                cut |= clip_lines(lines, max_lines, h, (cw - 2.0 * pad).max(h));
            }
            clipped += usize::from(cut);
            heights.push(block(h, pad, tallest(&row)));
            cells.push(row);
        }
        Layout {
            w,
            heights,
            clipped,
            h,
            pad,
            title_h,
            title_lines,
            header_h,
            header,
            cells,
        }
    }

    pub(super) fn width(&self) -> f64 {
        self.w.iter().sum()
    }

    pub(super) fn head_h(&self) -> f64 {
        self.title_h + self.header_h
    }

    fn step(&self, gap: f64) -> f64 {
        (self.width() + gap).max(MIN_COL_W)
    }

    /// Columns with top-left corner `at`. `max_h` 0 — frame only; the limit is not below the header plus the
    /// tallest row. `reserve` — bottom of the column taken by the note about unshown rows.
    fn plan(&self, at: [f64; 2], max_h: f64, gap: f64, room: &Room, reserve: f64) -> Plan {
        let tw = self.width();
        let step = self.step(gap);
        let head_h = self.head_h();
        let tallest = self.heights.iter().copied().fold(0.0, f64::max);
        let limit = if max_h > 0.0 {
            max_h.max(head_h + tallest + reserve)
        } else {
            f64::INFINITY
        };
        let avail = |k: usize| -> Option<f64> {
            if k >= MAX_COLUMNS {
                return None;
            }
            let x = at[0] + k as f64 * step;
            // negation: with NaN there is no room either
            if !(x + tw <= room.right + EPS) || at[1] > room.top + EPS {
                return None;
            }
            if x < room.left - EPS {
                return Some(0.0);
            }
            // a column above the note ends at its top; one starting below its top ends at the frame
            let floor = if x + tw > room.stamp[0] && at[1] > room.stamp[1] {
                room.stamp[1]
            } else {
                room.bottom
            };
            Some((at[1] - floor).min(limit) - reserve)
        };
        split_rows(&self.heights, head_h, &avail)
    }

    fn draw_row(&self, pen: &mut Pen, x0: f64, y: f64, cells: &[Vec<String>], layer: usize) {
        let (h, pad) = (self.h, self.pad);
        let mut x = x0;
        for (j, cw) in self.w.iter().enumerate() {
            if let Some(lines) = cells.get(j) {
                for (i, line) in lines.iter().enumerate() {
                    let tx = if is_number(line) {
                        x + cw - pad - text_w(line, h)
                    } else {
                        x + pad
                    };
                    let ty = y - pad - h - i as f64 * h * (1.0 + LEAD);
                    pen.text([tx, ty], h, 0.0, line, layer);
                }
            }
            x += cw;
        }
    }

    /// A part of the table: header with title `title` and rows `rows`.
    pub(super) fn part(
        &self,
        pen: &mut Pen,
        at: [f64; 2],
        swatches: &[Swatch],
        rows: Range<usize>,
        title: &str,
        layer: usize,
    ) -> [f64; 4] {
        let h = self.h;
        let tw = self.width();
        let (x0, top) = (at[0], at[1]);
        let total = self.head_h() + self.heights[rows.clone()].iter().sum::<f64>();
        pen.rect(x0, top - total, tw, total, 0.5, layer);
        let mut y = top;
        if self.title_h > 0.0 {
            let th = h * 1.2;
            let mut lines = title_wrap(title, th, tw - 2.0 * self.pad);
            lines.truncate(self.title_lines.max(1));
            let n = lines.len().max(1) as f64;
            let text_h = th * n + th * LEAD * (n - 1.0);
            let first = y - (self.title_h - text_h) / 2.0 - th;
            for (i, line) in lines.iter().enumerate() {
                pen.text(
                    [
                        x0 + (tw - text_w(line, th)) / 2.0,
                        first - i as f64 * th * (1.0 + LEAD),
                    ],
                    th,
                    0.0,
                    line,
                    layer,
                );
            }
            y -= self.title_h;
            pen.line(vec![[x0, y], [x0 + tw, y]], false, 0.35, layer);
        }
        let grid_top = y;
        if self.header_h > 0.0 {
            self.draw_row(pen, x0, y, &self.header, layer);
            y -= self.header_h;
            pen.line(vec![[x0, y], [x0 + tw, y]], false, 0.35, layer);
        }
        for i in rows {
            self.draw_row(pen, x0, y, &self.cells[i], layer);
            let row_h = self.heights[i];
            if let (Some(s), Some(cw)) = (swatches.get(i), self.w.first()) {
                let m = 0.8;
                let cell = [x0 + m, y - row_h + m, cw - 2.0 * m, row_h - 2.0 * m];
                draw_swatch(pen, s, cell, layer);
            }
            y -= row_h;
            pen.line(vec![[x0, y], [x0 + tw, y]], false, 0.13, layer);
        }
        let mut x = x0;
        for cw in &self.w[..self.w.len().saturating_sub(1)] {
            x += cw;
            pen.line(vec![[x, grid_top], [x, top - total]], false, 0.18, layer);
        }
        [x0, top - total, tw, total]
    }
}

/// Row ranges per slot left to right: `avail(k)` — height of slot `k`, mm, None — there are no more
/// slots, and the end must eventually come; `head_h` — header in each. A slot where the next
/// row does not fit is skipped; no row is lost silently: those left out are counted in `hidden`.
pub fn split_rows(heights: &[f64], head_h: f64, avail: &dyn Fn(usize) -> Option<f64>) -> Plan {
    if heights.is_empty() {
        return Plan {
            parts: vec![(0, 0..0)],
            hidden: 0,
        };
    }
    let mut parts = vec![];
    let (mut start, mut k) = (0, 0);
    while start < heights.len() {
        let Some(a) = avail(k) else {
            break;
        };
        let (mut used, mut end) = (head_h, start);
        while end < heights.len() && used + heights[end] <= a + EPS {
            used += heights[end];
            end += 1;
        }
        if end > start {
            parts.push((k, start..end));
            start = end;
        }
        k += 1;
    }
    Plan {
        parts,
        hidden: heights.len() - start,
    }
}

/// Schedule columns with top-left corner `at`. `max_h` 0 — frame only.
pub fn plan_columns(t: &Table, h: f64, at: [f64; 2], max_h: f64, gap: f64, room: &Room) -> Plan {
    let l = Layout::new(t, &t.title, h, room.right - at[0], at[1] - room.bottom);
    l.plan(at, max_h, gap, room, 0.0)
}

/// Continuation title of a schedule on subsequent columns.
pub(super) fn continued_title(title: &str, suffix: &str) -> String {
    if title.is_empty() {
        return String::new();
    }
    format!("{title} ({suffix})")
}

fn draw_swatch(pen: &mut Pen, s: &Swatch, r: [f64; 4], layer: usize) {
    let ring = vec![
        [r[0], r[1]],
        [r[0] + r[2], r[1]],
        [r[0] + r[2], r[1] + r[3]],
        [r[0], r[1] + r[3]],
    ];
    match s {
        Swatch::None => {}
        Swatch::Hatch { angles, step } => {
            for a in angles {
                for seg in hatch_rings(std::slice::from_ref(&ring), *a, *step) {
                    pen.line(seg.to_vec(), false, 0.13, layer);
                }
            }
            pen.line(ring, true, 0.25, layer);
        }
        Swatch::Solid { gray } => {
            pen.fill(vec![ring.clone()], *gray, layer);
            pen.line(ring, true, 0.25, layer);
        }
        Swatch::Line { weight } => {
            let y = r[1] + r[3] / 2.0;
            pen.line(vec![[r[0], y], [r[0] + r[2], y]], false, *weight, layer);
        }
    }
}

fn union(bb: Option<[f64; 4]>, r: [f64; 4]) -> Option<[f64; 4]> {
    let b = [r[0], r[1], r[0] + r[2], r[1] + r[3]];
    Some(match bb {
        None => b,
        Some(a) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    })
}

/// A schedule in columns within the sheet. Rows stay linked to the model: the table and the split
/// are recomputed on every sheet build. Rows that do not fit are counted in the result and labelled on
/// the sheet; `text` — catalog strings.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_split(
    pen: &mut Pen,
    at: [f64; 2],
    t: &Table,
    h: f64,
    layer: usize,
    max_h: f64,
    gap: f64,
    room: &Room,
    text: &dyn Fn(&str) -> String,
) -> Drawn {
    let (max_w, column_h) = (room.right - at[0], at[1] - room.bottom);
    let one = Layout::new(t, &t.title, h, max_w, column_h);
    let plan = one.plan(at, max_h, gap, room, 0.0);
    if plan.hidden == 0 && plan.parts.len() <= 1 {
        let (k, rows) = plan.parts.into_iter().next().unwrap_or((0, 0..0));
        let x = at[0] + k as f64 * one.step(gap);
        let rect = one.part(pen, [x, at[1]], &t.swatches, rows, &t.title, layer);
        return Drawn {
            rect,
            hidden: 0,
            clipped: one.clipped,
        };
    }
    // widths account for the continuation title; columns are laid out by these widths
    let cont = continued_title(&t.title, &text("sheets.paper.continued"));
    let wide = Layout::new(t, &cont, h, max_w, column_h);
    let mut plan = wide.plan(at, max_h, gap, room, 0.0);
    if plan.hidden > 0 {
        plan = wide.plan(at, max_h, gap, room, notice_h(h));
    }
    let mut bb = None;
    let mut last = None;
    for (i, (k, rows)) in plan.parts.iter().enumerate() {
        let x = at[0] + *k as f64 * wide.step(gap);
        let title = if i == 0 {
            t.title.as_str()
        } else {
            cont.as_str()
        };
        let r = wide.part(pen, [x, at[1]], &t.swatches, rows.clone(), title, layer);
        last = Some((r[0], r[1]));
        bb = union(bb, r);
    }
    if plan.hidden > 0 {
        let says = text("sheets.paper.rows_hidden").replace("{n}", &plan.hidden.to_string());
        let (s, x, y) = match last {
            Some((x, bottom)) => (says, x, bottom - notice_h(h) + h * 0.4),
            None if t.title.is_empty() => (says, at[0], at[1] - h * 1.4),
            None => (format!("{}: {says}", t.title), at[0], at[1] - h * 1.4),
        };
        let sw = text_w(&s, h);
        let x = x.min(room.right - sw).max(room.left);
        let y = y.min(room.top - h * 1.4).max(room.bottom + h * 0.4);
        pen.text([x, y], h, 0.0, &s, layer);
        bb = union(bb, [x, y - h * 0.4, sw, notice_h(h)]);
    }
    let b = bb.unwrap_or([at[0], at[1], at[0], at[1]]);
    Drawn {
        rect: [b[0], b[1], b[2] - b[0], b[3] - b[1]],
        hidden: plan.hidden,
        clipped: wide.clipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ru_fixtures::ru;
    use crate::sheets::tests_output::ru_text;
    use crate::sheets::Mark;

    fn numbered(rows: usize) -> Table {
        Table {
            title: ru("doors_word").into(),
            header: vec!["№".into(), ru("name_word").into()],
            rows: (0..rows)
                .map(|i| vec![format!("{}", i + 1), format!("row-{i}")])
                .collect(),
            swatches: vec![],
        }
    }

    const ROOM: Room = Room {
        left: 0.0,
        top: 297.0,
        right: 410.0,
        bottom: 10.0,
        stamp: [230.0, 46.0],
    };

    fn texts(marks: &[(Option<usize>, Mark)]) -> Vec<String> {
        marks
            .iter()
            .filter_map(|(_, m)| match m {
                Mark::Text { s, .. } => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn drawn(t: &Table, at: [f64; 2], max_h: f64) -> (Vec<(Option<usize>, Mark)>, Drawn) {
        let mut marks = vec![];
        let mut pen = Pen {
            out: &mut marks,
            vp: None,
        };
        let d = draw_split(&mut pen, at, t, 2.5, 12, max_h, 4.0, &ROOM, &ru_text);
        (marks, d)
    }

    fn rows_of(p: &Plan) -> Vec<Range<usize>> {
        p.parts.iter().map(|x| x.1.clone()).collect()
    }

    fn right_edge(marks: &[(Option<usize>, Mark)]) -> f64 {
        marks
            .iter()
            .map(|(_, m)| match m {
                Mark::Line { pts, .. } => pts.iter().map(|p| p[0]).fold(0.0, f64::max),
                Mark::Text { pos, h, s, .. } => pos[0] + text_w(s, *h),
                Mark::Fill { .. } => 0.0,
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn split_keeps_every_row_once_in_order() {
        let free = |_: usize| Some(60.0);
        // 5 mm row, 10 mm header, 60 mm column: 10 rows per column
        let p = split_rows(&[5.0; 25], 10.0, &free);
        assert_eq!(rows_of(&p), vec![0..10, 10..20, 20..25]);
        assert_eq!(p.hidden, 0);
        assert_eq!(rows_of(&split_rows(&[], 10.0, &free)), vec![0..0]);
        assert_eq!(rows_of(&split_rows(&[5.0; 3], 10.0, &free)), vec![0..3]);
        // an exact fill does not lose a row to rounding
        let tight = split_rows(&[0.1; 2], 0.0, &|_| Some(0.2));
        assert_eq!(rows_of(&tight), vec![0..2]);
    }

    #[test]
    fn split_reports_rows_when_columns_end() {
        let p = split_rows(&[5.0; 25], 10.0, &|k| (k < 2).then_some(60.0));
        assert_eq!(rows_of(&p), vec![0..10, 10..20]);
        assert_eq!(p.hidden, 5);
    }

    #[test]
    fn split_skips_a_column_with_no_room() {
        let p = split_rows(&[5.0; 3], 10.0, &|k| match k {
            0 => Some(1.0),
            1 => Some(60.0),
            _ => None,
        });
        assert_eq!(p.parts, vec![(1, 0..3)]);
        assert_eq!(p.hidden, 0);
    }

    #[test]
    fn split_uses_the_height_of_each_row_and_column() {
        let p = split_rows(&[5.0; 30], 10.0, &|k| {
            Some(if k == 0 { 110.0 } else { 60.0 })
        });
        assert_eq!(rows_of(&p), vec![0..20, 20..30]);
        // a tall row moves to the next column whole
        let p = split_rows(&[5.0, 50.0, 5.0], 10.0, &|k| (k < 8).then_some(30.0));
        assert_eq!(p.parts, vec![(0, 0..1)]);
        assert_eq!(p.hidden, 2, "taller than any column: stays reported");
    }

    #[test]
    fn explicit_height_limits_the_column_and_cannot_go_below_one_row() {
        let t = numbered(30);
        let p = plan_columns(&t, 2.5, [30.0, 250.0], 60.0, 4.0, &ROOM);
        assert!(p.parts.len() > 1);
        let l = Layout::new(&t, &t.title, 2.5, 380.0, 240.0);
        let row_h = l.heights[0];
        let per = ((60.0 - l.head_h()) / row_h + EPS).floor() as usize;
        assert_eq!(p.parts[0].1, 0..per);
        assert_eq!(p.parts.last().unwrap().1.end, 30);
        // a limit smaller than header plus row is raised to it: columns are finite and not beyond the sheet
        let tiny = plan_columns(&numbered(500), 2.5, [30.0, 250.0], 1.0, 4.0, &ROOM);
        assert!(tiny.parts.iter().all(|x| x.1.len() == 1));
        assert!(tiny.hidden > 0);
    }

    #[test]
    fn auto_split_stops_at_frame_and_stamp() {
        let t = numbered(200);
        let l = Layout::new(&t, &t.title, 2.5, 380.0, 190.0);
        let (tw, row_h, head) = (l.width(), l.heights[0], l.head_h());
        let top = 200.0;
        let p = plan_columns(&t, 2.5, [30.0, top], 0.0, 4.0, &ROOM);
        let free = ((top - ROOM.bottom - head) / row_h + EPS).floor() as usize;
        assert_eq!(
            p.parts[0].1.len(),
            free,
            "first column goes down to the frame"
        );
        // a column above the title block stops at its top
        let over = plan_columns(&t, 2.5, [ROOM.stamp[0] - tw / 2.0, top], 0.0, 4.0, &ROOM);
        let short = ((top - ROOM.stamp[1] - head) / row_h + EPS).floor() as usize;
        assert_eq!(over.parts[0].1.len(), short);
        // a short schedule needs only one column
        let few = plan_columns(&numbered(3), 2.5, [30.0, top], 0.0, 4.0, &ROOM);
        assert_eq!(few.parts.len(), 1);
    }

    #[test]
    fn no_room_means_reported_rows_not_a_table_off_the_paper() {
        let t = numbered(50);
        // start exactly on the frame, beyond the right frame, above the sheet
        for at in [[30.0, 11.0], [409.0, 200.0], [30.0, 400.0]] {
            let p = plan_columns(&t, 2.5, at, 0.0, 4.0, &ROOM);
            assert!(p.parts.is_empty(), "{at:?}");
            assert_eq!(p.hidden, 50);
            let (marks, d) = drawn(&t, at, 0.0);
            assert_eq!(d.hidden, 50);
            let got = texts(&marks);
            assert!(got.iter().any(|s| s.contains("50")), "{got:?}");
            assert!(!got.iter().any(|s| s == "row-0"));
        }
        // a column above a note shorter than a row is skipped, not drawn over it
        let (marks, d) = drawn(&numbered(5), [ROOM.stamp[0] - 10.0, 50.0], 0.0);
        assert_eq!(d.hidden, 5);
        assert!(!texts(&marks).iter().any(|s| s == "row-0"));
    }

    #[test]
    fn rows_on_paper_plus_reported_rows_are_all_rows() {
        // A3: a 150 mm wide column, as in the analysis: the third column is already beyond the frame
        let wide = format!("{} ", ru("wide_cell")).repeat(3);
        for total in [10, 76, 300, 2000] {
            let t = Table {
                title: ru("doors_word").into(),
                header: vec!["№".into(), "A".into(), "B".into()],
                rows: (0..total)
                    .map(|i| vec![format!("r{i}"), wide.clone(), wide.clone()])
                    .collect(),
                swatches: vec![],
            };
            let (marks, d) = drawn(&t, [30.0, 250.0], 0.0);
            let got = texts(&marks);
            let on_paper = (0..total)
                .filter(|i| got.iter().any(|s| *s == format!("r{i}")))
                .count();
            assert_eq!(on_paper + d.hidden, total, "{total}");
            let edge = right_edge(&marks);
            assert!(edge <= ROOM.right + 1e-6, "{total}: {edge}");
            assert!(d.rect[1] >= ROOM.bottom - 1e-6, "{:?}", d.rect);
            if d.hidden > 0 {
                let says = format!("{}: {}", ru("rows_not_shown"), d.hidden);
                assert!(got.iter().any(|s| *s == says), "{says}");
            }
        }
    }

    #[test]
    fn drawn_columns_hold_all_rows_and_repeat_the_header() {
        let t = numbered(40);
        let (marks, d) = drawn(&t, [30.0, 200.0], 60.0);
        assert_eq!(d.hidden, 0);
        let got = texts(&marks);
        for i in 0..40 {
            let want = format!("row-{i}");
            assert_eq!(got.iter().filter(|s| **s == want).count(), 1, "{want}");
        }
        let cols = got.iter().filter(|s| *s == ru("name_word")).count();
        assert!(cols > 1, "header in every column: {cols}");
        assert_eq!(got.iter().filter(|s| s.starts_with(ru("doors_word"))).count(), cols);
        assert_eq!(
            got.iter().filter(|s| *s == ru("doors_continued")).count(),
            cols - 1
        );
        let r = d.rect;
        assert!(r[2] > 0.0 && r[3] <= 60.0 + 1e-6, "{r:?}");
        // columns do not overlap: frames left to right with a gap
        let mut lefts: Vec<f64> = marks
            .iter()
            .filter_map(|(_, m)| match m {
                Mark::Line {
                    pts, closed: true, ..
                } => Some(pts[0][0]),
                _ => None,
            })
            .collect();
        lefts.dedup();
        assert_eq!(lefts.len(), cols);
        assert!(lefts.windows(2).all(|p| p[1] - p[0] >= 4.0));
    }

    #[test]
    fn long_text_wraps_inside_the_capped_column() {
        let long = ru("long_layers");
        let t = Table {
            title: String::new(),
            header: vec![ru("layers_word").into()],
            rows: vec![vec![long.into()], vec![ru("briefly_word").into()]],
            swatches: vec![],
        };
        let l = Layout::new(&t, "", 2.5, 380.0, 200.0);
        assert!(l.w[0] <= MAX_COL_W + 1e-9, "{:?}", l.w);
        assert!(l.heights[0] > l.heights[1], "{:?}", l.heights);
        let (marks, d) = drawn(&t, [30.0, 250.0], 0.0);
        assert_eq!((d.hidden, d.clipped), (0, 0));
        // all text is in place and no line is wider than the column
        let got = texts(&marks);
        for word in [ru("brick_word"), ru("mineral_word"), ru("plaster_word"), ru("paint_word")]
        {
            assert!(got.iter().any(|s| s.contains(word)), "{word}: {got:?}");
        }
        assert!(got.iter().all(|s| text_w(s, 2.5) <= l.w[0]), "{got:?}");
        // and a word without spaces is split by characters
        let parts = wrap(&ru("zhe_lc").repeat(200), 2.5, 50.0);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|s| text_w(s, 2.5) <= 50.0));
        assert_eq!(parts.concat(), ru("zhe_lc").repeat(200));
        assert_eq!(wrap("", 2.5, 50.0), vec![String::new()]);
    }

    #[test]
    fn table_is_narrowed_to_the_width_left_on_the_sheet() {
        let t = Table {
            title: String::new(),
            header: vec!["A".into(), "B".into()],
            rows: vec![vec![format!("{} ", ru("word_lc")).repeat(40), format!("{} ", ru("word_lc")).repeat(40)]],
            swatches: vec![],
        };
        // 100 mm remain on the right, two 80 mm columns do not fit
        let (marks, d) = drawn(&t, [310.0, 250.0], 0.0);
        assert_eq!(d.hidden, 0);
        assert!(right_edge(&marks) <= ROOM.right + 1e-6);
    }

    #[test]
    fn row_taller_than_a_column_is_clipped_with_a_mark_and_counted() {
        let t = Table {
            title: "T".into(),
            header: vec![],
            rows: vec![vec![format!("{} ", ru("word_lc")).repeat(4000)], vec![ru("more_word").into()]],
            swatches: vec![],
        };
        let (marks, d) = drawn(&t, [30.0, 100.0], 0.0);
        assert_eq!(d.clipped, 1);
        assert_eq!(d.hidden, 0);
        let got = texts(&marks);
        assert!(got.iter().any(|s| s.ends_with('…')), "{got:?}");
        assert!(got.iter().any(|s| s == ru("more_word")));
        assert!(d.rect[1] >= ROOM.bottom - 1e-6, "{:?}", d.rect);
    }

    #[test]
    fn swatches_widen_the_first_column_and_draw() {
        let mut t = numbered(2);
        t.swatches = vec![
            Swatch::Hatch {
                angles: vec![45.0],
                step: 1.5,
            },
            Swatch::Solid { gray: 64 },
        ];
        let l = Layout::new(&t, &t.title, 2.5, 380.0, 200.0);
        assert!(l.w[0] >= SWATCH_W);
        let (marks, _) = drawn(&t, [30.0, 200.0], 0.0);
        assert!(marks.iter().any(|(_, m)| matches!(m, Mark::Fill { .. })));
        let thin = marks
            .iter()
            .filter(|(_, m)| matches!(m, Mark::Line { weight, .. } if *weight == 0.13))
            .count();
        assert!(thin > 2, "hatch lines: {thin}");
    }
}
