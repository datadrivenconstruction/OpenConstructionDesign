// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model answers as formatted text: a small, safe Markdown subset (headings, bold/italic, inline
//! code, bullet and numbered lists with nesting, quotes, fenced code, pipe tables, links).
//! Nothing is executed or opened: HTML stays text, a link only shows its address and copies it on
//! click. The parse is pure (`md_blocks`) and cached per entry, so a long answer costs one parse per
//! change and off-screen blocks are skipped while drawing.

use super::kit::{self, space};
use super::Cx;
use ddc_ui::theme::Role;
use ddc_ui::widgets;
use egui::{
    self,
    text::{LayoutJob, TextFormat},
    Align, Color32, Frame, Label, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const MAX_DEPTH: u8 = ddc_ai::limits::MD_DEPTH.u8v();
/// Blocks this far outside the visible area are not laid out again.
const CULL_PAD: f32 = 240.0;
const COPIED_SECS: f64 = 1.5;
const MARK_W: f32 = space::XL;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
    /// An element reference such as `wall:12`: drawn as a pill that selects the element.
    pub pill: Option<String>,
}

impl Span {
    fn same_style(&self, o: &Span) -> bool {
        self.bold == o.bold
            && self.italic == o.italic
            && self.code == o.code
            && self.link == o.link
            && self.pill == o.pill
    }
}

/// Categories whose references become pills in an answer.
const PILL_CATS: [&str; 22] = [
    "wall", "floor", "slab", "column", "beam", "door", "window", "opening", "room", "stair",
    "railing", "roof", "ceiling", "solid", "mass", "comp", "curtain", "mep", "level", "grid",
    "shaft", "imported",
];

/// `wall:12` or `solid:n3`: a known category, a colon, a number or `n` and a number.
pub fn is_pill(s: &str) -> bool {
    let Some((cat, id)) = s.split_once(':') else { return false };
    let digits = id.strip_prefix('n').unwrap_or(id);
    PILL_CATS.contains(&cat)
        && !digits.is_empty()
        && digits.len() <= 12
        && digits.bytes().all(|b| b.is_ascii_digit())
}

fn pill_span(base: &Span, text: &str) -> Span {
    Span { text: text.to_string(), pill: Some(text.to_string()), ..base.clone() }
}

/// Splits plain and code runs at element references.
fn pillify(spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        if s.link.is_some() || s.pill.is_some() {
            out.push(s);
        } else if s.code {
            if is_pill(&s.text) {
                let t = s.text.clone();
                out.push(pill_span(&s, &t));
            } else {
                out.push(s);
            }
        } else {
            let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == ':';
            let mut rest = s.text.as_str();
            let mut plain = String::new();
            while !rest.is_empty() {
                let end = rest.find(|c: char| !word(c)).unwrap_or(rest.len());
                let (tok, tail) = rest.split_at(end);
                if is_pill(tok) {
                    put(&mut out, &s, &std::mem::take(&mut plain));
                    out.push(pill_span(&s, tok));
                } else {
                    plain.push_str(tok);
                }
                let step = tail.chars().next().map_or(0, char::len_utf8);
                plain.push_str(&tail[..step]);
                rest = &tail[step..];
            }
            put(&mut out, &s, &plain);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColAlign {
    Auto,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Para(Vec<Span>),
    Item {
        depth: usize,
        num: Option<u64>,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    Code {
        lang: String,
        text: String,
    },
    Table {
        head: Vec<Vec<Span>>,
        align: Vec<ColAlign>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
}

// ---------------------------------------------------------------- inline

/// One line or paragraph: `**bold**`, `*italic*`, `_italic_`, `` `code` ``, `[text](url)`.
/// Unterminated markers stay literal text.
pub fn inline(s: &str) -> Vec<Span> {
    let mut out = vec![];
    parse_inline(s, &Span::default(), &mut out, 0);
    pillify(merge(out))
}

fn put(out: &mut Vec<Span>, base: &Span, text: &str) {
    if !text.is_empty() {
        let mut s = base.clone();
        s.text = text.to_string();
        out.push(s);
    }
}

fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(l) if l.same_style(&s) => l.text.push_str(&s.text),
            _ => out.push(s),
        }
    }
    out
}

/// Longest span a marker may enclose; keeps a wall of unmatched markers linear.
const SPAN_WINDOW: usize = 2048;

fn window(s: &str) -> &str {
    if s.len() <= SPAN_WINDOW {
        return s;
    }
    let mut end = SPAN_WINDOW;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Byte offset of the first backtick run of exactly `n` backticks.
fn find_run(s: &str, n: usize) -> Option<usize> {
    let s = window(s);
    let mut from = 0;
    while let Some(p) = s[from..].find('`') {
        let st = from + p;
        let run = s[st..].len() - s[st..].trim_start_matches('`').len();
        if run == n {
            return Some(st);
        }
        from = st + run;
    }
    None
}

fn strip_pad(code: &str) -> &str {
    if code.len() >= 2 && code.starts_with(' ') && code.ends_with(' ') && !code.trim().is_empty() {
        &code[1..code.len() - 1]
    } else {
        code
    }
}

/// Closing delimiter of an emphasis run: (content length, offset after the delimiter).
fn find_close(inner: &str, c: char, double: bool) -> Option<(usize, usize)> {
    let d = if double { 2 } else { 1 };
    let inner = window(inner);
    let mut from = 0;
    while let Some(p) = inner[from..].find(c) {
        let st = from + p;
        let run = inner[st..].len() - inner[st..].trim_start_matches(c).len();
        let content = &inner[..st];
        let after = inner[st + run..].chars().next();
        let prev_ok = content.chars().last().is_some_and(|l| !l.is_whitespace());
        let run_ok = if double { run >= 2 } else { run == 1 };
        let word_ok = c == '*' || !after.is_some_and(char::is_alphanumeric);
        if prev_ok && run_ok && word_ok {
            return Some((st + run - d, st + run));
        }
        from = st + run;
    }
    None
}

fn parse_inline(s: &str, base: &Span, out: &mut Vec<Span>, depth: u8) {
    if depth > MAX_DEPTH {
        put(out, base, s);
        return;
    }
    let mut buf = String::new();
    let mut prev: Option<char> = None;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let tail = &rest[c.len_utf8()..];
        match c {
            '\\' => {
                if let Some(n) = tail.chars().next().filter(char::is_ascii_punctuation) {
                    buf.push(n);
                    prev = Some(n);
                    rest = &tail[n.len_utf8()..];
                    continue;
                }
            }
            '`' => {
                let n = rest.len() - rest.trim_start_matches('`').len();
                let after = &rest[n..];
                if let Some(end) = find_run(after, n) {
                    let code = &after[..end];
                    if !code.trim().is_empty() {
                        put(out, base, &std::mem::take(&mut buf));
                        let mut b = base.clone();
                        b.code = true;
                        put(out, &b, strip_pad(code));
                        rest = &after[end + n..];
                        prev = Some(' ');
                        continue;
                    }
                }
                buf.push_str(&rest[..n]);
                prev = Some('`');
                rest = after;
                continue;
            }
            '*' | '_' => {
                let double = tail.starts_with(c);
                let inner = &rest[if double { 2 } else { 1 }..];
                let opens = inner
                    .chars()
                    .next()
                    .is_some_and(|n| !n.is_whitespace() && (n != c || double))
                    && (c == '*' || !prev.is_some_and(char::is_alphanumeric));
                if opens {
                    if let Some((len, close)) = find_close(inner, c, double) {
                        put(out, base, &std::mem::take(&mut buf));
                        let mut b = base.clone();
                        if double {
                            b.bold = true;
                        } else {
                            b.italic = true;
                        }
                        parse_inline(&inner[..len], &b, out, depth + 1);
                        rest = &inner[close..];
                        prev = Some(' ');
                        continue;
                    }
                }
            }
            '[' if base.link.is_none() => {
                if let Some(p) = window(tail).find("](") {
                    let label = &tail[..p];
                    let after = &tail[p + 2..];
                    if let Some(q) = after.find(')') {
                        let url = after[..q].split_whitespace().next().unwrap_or("");
                        if !url.is_empty() && !label.is_empty() && !label.contains('\n') {
                            put(out, base, &std::mem::take(&mut buf));
                            let mut b = base.clone();
                            b.link = Some(url.to_string());
                            parse_inline(label, &b, out, depth + 1);
                            rest = &after[q + 1..];
                            prev = Some(' ');
                            continue;
                        }
                    }
                }
            }
            _ => {}
        }
        buf.push(c);
        prev = Some(c);
        rest = tail;
    }
    put(out, base, &buf);
}

pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

// ---------------------------------------------------------------- blocks

enum Pending {
    None,
    Para(String),
    Quote(String),
    Item {
        depth: usize,
        num: Option<u64>,
        raw: String,
    },
}

fn flush(p: &mut Pending, out: &mut Vec<MdBlock>) {
    match std::mem::replace(p, Pending::None) {
        Pending::None => {}
        Pending::Para(s) => out.push(MdBlock::Para(inline(&s))),
        Pending::Quote(s) => out.push(MdBlock::Quote(inline(&s))),
        Pending::Item { depth, num, raw } => out.push(MdBlock::Item {
            depth,
            num,
            spans: inline(&raw),
        }),
    }
}

/// Opening fence: (fence char, run length, info string).
fn fence_open(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    let ch = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = t.len() - t.trim_start_matches(ch).len();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim();
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, n, info))
}

fn fence_close(line: &str, ch: char, n: usize) -> bool {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return false;
    }
    let run = t.len() - t.trim_start_matches(ch).len();
    run >= n && t[run..].trim().is_empty()
}

fn heading(t: &str) -> Option<(u8, &str)> {
    let n = t.len() - t.trim_start_matches('#').len();
    if n == 0 || n > 6 {
        return None;
    }
    let r = &t[n..];
    if !(r.is_empty() || r.starts_with(' ') || r.starts_with('\t')) {
        return None;
    }
    let r = r.trim();
    let stripped = r.trim_end_matches('#');
    let txt = if stripped.is_empty() || stripped.ends_with(' ') {
        stripped.trim()
    } else {
        r
    };
    Some((n as u8, txt))
}

fn is_rule(t: &str) -> bool {
    let mut chars = t.chars().filter(|c| !c.is_whitespace());
    let Some(f) = chars.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    let mut n = 1;
    for c in chars {
        if c != f {
            return false;
        }
        n += 1;
    }
    n >= 3
}

/// (indent width, number, content) of a list line.
fn list_marker(line: &str) -> Option<(usize, Option<u64>, &str)> {
    let t = line.trim_start_matches([' ', '\t']);
    let lead = &line[..line.len() - t.len()];
    let indent: usize = lead.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
    let first = t.chars().next()?;
    if matches!(first, '-' | '*' | '+') {
        let r = &t[1..];
        if r.is_empty() {
            return Some((indent, None, ""));
        }
        if r.starts_with(' ') || r.starts_with('\t') {
            return Some((indent, None, r.trim_start()));
        }
        return None;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let r = &t[digits..];
        if let Some(r2) = r.strip_prefix('.').or_else(|| r.strip_prefix(')')) {
            if r2.is_empty() || r2.starts_with(' ') || r2.starts_with('\t') {
                return Some((indent, t[..digits].parse().ok(), r2.trim_start()));
            }
        }
    }
    None
}

fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") {
        &t[..t.len() - 1]
    } else {
        t
    };
    let mut cells = vec![];
    let mut cur = String::new();
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && it.peek() == Some(&'|') {
            cur.push('|');
            it.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// Alignment row under a table header (`|---|:--:|--:|`), or None when the line is not one.
fn table_sep(line: &str) -> Option<Vec<ColAlign>> {
    if !line.contains('|') || !line.contains('-') {
        return None;
    }
    let mut out = vec![];
    for cell in split_row(line) {
        let c = cell.trim();
        let core = c.trim_start_matches(':').trim_end_matches(':');
        if core.is_empty() || !core.chars().all(|x| x == '-') {
            return None;
        }
        out.push(match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => ColAlign::Center,
            (false, true) => ColAlign::Right,
            (true, false) => ColAlign::Left,
            _ => ColAlign::Auto,
        });
    }
    Some(out)
}

/// Splits an answer into blocks. Never panics and never fails: input it cannot read as a block is
/// a paragraph, so a half-streamed answer renders as plain text until the marker is complete.
pub fn md_blocks(src: &str) -> Vec<MdBlock> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = vec![];
    let mut p = Pending::None;
    let mut stack: Vec<usize> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            flush(&mut p, &mut out);
            i += 1;
            continue;
        }
        if let Some((ch, n, info)) = fence_open(line) {
            flush(&mut p, &mut out);
            stack.clear();
            let close = (i + 1..lines.len()).find(|j| fence_close(lines[*j], ch, n));
            match close {
                Some(j) => {
                    out.push(MdBlock::Code {
                        lang: info.split_whitespace().next().unwrap_or("").to_string(),
                        text: lines[i + 1..j].join("\n"),
                    });
                    i = j + 1;
                    continue;
                }
                None => {
                    // Unterminated fence (a streamed answer): the rest stays plain text.
                    out.push(MdBlock::Para(vec![Span {
                        text: lines[i..].join("\n"),
                        ..Span::default()
                    }]));
                    break;
                }
            }
        }
        if let Some((level, txt)) = heading(t) {
            flush(&mut p, &mut out);
            stack.clear();
            out.push(MdBlock::Heading {
                level,
                spans: inline(txt),
            });
            i += 1;
            continue;
        }
        if is_rule(t) {
            flush(&mut p, &mut out);
            stack.clear();
            out.push(MdBlock::Rule);
            i += 1;
            continue;
        }
        if line.contains('|') && i + 1 < lines.len() {
            if let Some(align) = table_sep(lines[i + 1]) {
                let head = split_row(line);
                if head.len() == align.len() {
                    flush(&mut p, &mut out);
                    stack.clear();
                    let mut rows = vec![];
                    let mut j = i + 2;
                    while j < lines.len() && lines[j].contains('|') && !lines[j].trim().is_empty() {
                        let mut cells = split_row(lines[j]);
                        cells.resize(head.len(), String::new());
                        rows.push(cells.iter().map(|c| inline(c)).collect());
                        j += 1;
                    }
                    out.push(MdBlock::Table {
                        head: head.iter().map(|c| inline(c)).collect(),
                        align,
                        rows,
                    });
                    i = j;
                    continue;
                }
            }
        }
        if let Some(q) = t.strip_prefix('>') {
            let q = q.strip_prefix(' ').unwrap_or(q);
            match &mut p {
                Pending::Quote(s) => {
                    s.push('\n');
                    s.push_str(q);
                }
                _ => {
                    flush(&mut p, &mut out);
                    stack.clear();
                    p = Pending::Quote(q.to_string());
                }
            }
            i += 1;
            continue;
        }
        if let Some((indent, num, content)) = list_marker(line) {
            flush(&mut p, &mut out);
            while stack.last().is_some_and(|&x| x > indent) {
                stack.pop();
            }
            if stack.last() != Some(&indent) {
                stack.push(indent);
            }
            p = Pending::Item {
                depth: stack.len() - 1,
                num,
                raw: content.to_string(),
            };
            i += 1;
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        match &mut p {
            Pending::Item { raw, .. } if indented => {
                raw.push('\n');
                raw.push_str(t);
            }
            Pending::Para(s) => {
                s.push('\n');
                s.push_str(t);
            }
            _ => {
                flush(&mut p, &mut out);
                stack.clear();
                p = Pending::Para(t.to_string());
            }
        }
        i += 1;
    }
    flush(&mut p, &mut out);
    out
}

fn is_numeric(s: &str) -> bool {
    let t = s.trim();
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '+' | '-' | '%' | ' '))
}

/// Column alignment as drawn: an explicit marker wins, otherwise a column of numbers is right-aligned.
pub fn column_align(align: &[ColAlign], rows: &[Vec<Vec<Span>>], col: usize) -> ColAlign {
    match align.get(col).copied().unwrap_or(ColAlign::Auto) {
        ColAlign::Auto => {
            let cells: Vec<String> = rows
                .iter()
                .filter_map(|r| r.get(col))
                .map(|c| plain(c))
                .filter(|s| !s.trim().is_empty())
                .collect();
            if !cells.is_empty() && cells.iter().all(|s| is_numeric(s)) {
                ColAlign::Right
            } else {
                ColAlign::Left
            }
        }
        a => a,
    }
}

/// What the Copy button puts on the clipboard: the answer exactly as the model wrote it.
pub fn copy_payload(text: &str) -> String {
    text.to_string()
}

// ---------------------------------------------------------------- drawing

// Default: IdTypeMap::remove_temp requires it
#[derive(Clone, Default)]
struct Cache {
    key: u64,
    width: f32,
    blocks: Arc<Vec<MdBlock>>,
    heights: Vec<f32>,
}

fn hash_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Draws one assistant answer with a quiet Copy button on hover. `slot` is the entry's index.
pub fn answer(ui: &mut Ui, cx: &Cx<'_>, text: &str, slot: usize) {
    let ctx = ui.ctx().clone();
    let id = egui::Id::new(("ai.md", slot));
    let key = hash_of(text);
    let width = ui.available_width();
    let old = ctx.data_mut(|d| d.remove_temp::<Cache>(id));
    let mut c = match old {
        Some(c) if c.key == key => c,
        old => {
            let blocks = Arc::new(md_blocks(text));
            // Blocks that did not change keep their measured height (streaming appends at the end).
            let heights = blocks
                .iter()
                .enumerate()
                .map(|(n, b)| match &old {
                    Some(o) if o.blocks.get(n) == Some(b) => o.heights.get(n).copied().unwrap_or(0.0),
                    _ => 0.0,
                })
                .collect();
            Cache {
                key,
                width,
                blocks,
                heights,
            }
        }
    };
    if (c.width - width).abs() > 0.5 {
        c.width = width;
        c.heights.iter_mut().for_each(|h| *h = 0.0);
    }
    let blocks = c.blocks.clone();
    let shown = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        for (n, b) in blocks.iter().enumerate() {
            let h = c.heights[n];
            if h > 0.0 {
                let r = egui::Rect::from_min_size(ui.cursor().min, Vec2::new(width, h));
                if !r.expand(CULL_PAD).intersects(ui.clip_rect()) {
                    ui.allocate_space(Vec2::new(width, h));
                    continue;
                }
            }
            let r = ui.scope(|ui| block(ui, cx, b, (slot, n))).response;
            c.heights[n] = r.rect.height();
        }
    });
    ctx.data_mut(|d| d.insert_temp(id, c));
    copy_button(ui, cx, text, shown.response.rect, id);
}

fn copy_button(ui: &mut Ui, cx: &Cx<'_>, text: &str, rect: egui::Rect, id: egui::Id) {
    let th = cx.th;
    let ctx = ui.ctx().clone();
    let at = id.with("copied");
    let now = ctx.input(|i| i.time);
    let copied = ctx
        .data(|d| d.get_temp::<f64>(at))
        .is_some_and(|t| now - t < COPIED_SECS);
    if !(copied || ui.rect_contains_pointer(rect)) {
        return;
    }
    let size = th.d().control + 4.0;
    let r = egui::Rect::from_min_size(
        egui::pos2((rect.right() - size).max(rect.left()), rect.top()),
        Vec2::splat(size),
    );
    ui.painter()
        .rect_filled(r, th.radius("md"), th.c("bg.panel"));
    let tip = cx.cat.t(if copied { "ai.md.copied" } else { "ai.md.copy" });
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
    if widgets::glyph_button(&mut child, "copy", true, &tip).clicked() {
        ctx.copy_text(copy_payload(text));
        ctx.data_mut(|d| d.insert_temp(at, now));
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS + 0.1));
    }
}

fn rich(cx: &Cx<'_>, s: &Span, role: Role, color: Color32) -> RichText {
    let th = cx.th;
    let mut r = RichText::new(&s.text).color(color);
    r = if s.code {
        r.font(th.font(Role::Code)).background_color(th.c("bg.sunken"))
    } else if s.bold {
        r.font(th.font(Role::BodyStrong))
    } else {
        r.font(th.font(role))
    };
    if s.italic {
        r = r.italics();
    }
    if s.link.is_some() {
        r = r.color(th.c("accent")).underline();
    }
    r
}

/// The selection a pill asks for; a reference to something only in the draft (`n3`) selects
/// nothing yet.
pub fn pill_action(reference: &str) -> Option<super::Action> {
    let (cat, id) = reference.split_once(':')?;
    let table = match cat {
        "solid" | "mass" => cat,
        other => crate::ai::sets::table_of(other),
    };
    if table.is_empty() {
        return None;
    }
    Some(super::Action::SelectEl((table.to_string(), id.parse().ok()?)))
}

fn pill_id() -> egui::Id {
    egui::Id::new("ai.md.pill.click")
}

/// The reference of the pill clicked since the last call; the panel turns it into a selection.
pub fn take_pill_click(ctx: &egui::Context) -> Option<String> {
    ctx.data_mut(|d| d.remove_temp::<String>(pill_id()))
}

/// An element reference as a small quiet chip; a click asks the panel to select the element.
fn pill(ui: &mut Ui, cx: &Cx<'_>, reference: &str) {
    let th = cx.th;
    let out = Frame::new()
        .fill(th.c("bg.sunken"))
        .stroke(egui::Stroke::new(1.0, th.c("border.subtle")))
        .corner_radius(th.radius("md"))
        .inner_margin(Margin::symmetric(space::XS as i8, 0))
        .show(ui, |ui| {
            let rt = RichText::new(reference).font(th.font(Role::Code)).color(th.c("accent"));
            ui.add(Label::new(rt).sense(Sense::click()))
        });
    let r = out
        .inner
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(cx.cat.tf("ai.md.pill", &[("ref", reference)]));
    if r.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(pill_id(), reference.to_string()));
    }
}

/// A run of spans. Without links it is one selectable, wrapped label; with links each link is its
/// own label whose tooltip shows the address and whose click copies it.
fn spans_ui(ui: &mut Ui, cx: &Cx<'_>, spans: &[Span], role: Role, color: Color32) {
    let th = cx.th;
    if spans.iter().any(|s| s.link.is_some() || s.pill.is_some()) {
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::no_gap(ui, crate::ui_kit::Gap::Cells);
            for s in spans {
                if let Some(r) = &s.pill {
                    pill(ui, cx, r);
                    continue;
                }
                let rt = rich(cx, s, role, color);
                match &s.link {
                    Some(url) => {
                        let tip = cx.cat.tf("ai.md.link", &[("url", url.as_str())]);
                        let r = ui
                            .add(Label::new(rt).sense(Sense::click()))
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(tip);
                        if r.clicked() {
                            ui.ctx().copy_text(url.clone());
                        }
                    }
                    None => {
                        ui.add(Label::new(rt).selectable(true));
                    }
                }
            }
        });
        return;
    }
    let mut job = LayoutJob::default();
    for s in spans {
        let (font_id, bg) = if s.code {
            (th.font(Role::Code), th.c("bg.sunken"))
        } else if s.bold {
            (th.font(Role::BodyStrong), Color32::TRANSPARENT)
        } else {
            (th.font(role), Color32::TRANSPARENT)
        };
        job.append(
            &s.text,
            0.0,
            TextFormat {
                font_id,
                color,
                italics: s.italic,
                background: bg,
                underline: Stroke::NONE,
                ..Default::default()
            },
        );
    }
    ui.add(Label::new(job).selectable(true).wrap());
}

fn block(ui: &mut Ui, cx: &Cx<'_>, b: &MdBlock, id: (usize, usize)) {
    let th = cx.th;
    let primary = th.c("text.primary");
    let secondary = th.c("text.secondary");
    match b {
        MdBlock::Heading { level, spans } => {
            let role = if *level == 1 { Role::Title } else { Role::BodyStrong };
            let col = if *level >= 3 { secondary } else { primary };
            spans_ui(ui, cx, spans, role, col);
        }
        MdBlock::Para(spans) => spans_ui(ui, cx, spans, Role::Body, primary),
        MdBlock::Item { depth, num, spans } => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                ui.add_space(*depth as f32 * space::LG);
                let font = th.font(Role::Body);
                let row_h = ui.fonts(|f| f.row_height(&font));
                let (r, _) = ui.allocate_exact_size(Vec2::new(MARK_W, row_h), Sense::hover());
                let mark = match num {
                    Some(n) => format!("{n}."),
                    None => "\u{2022}".to_string(),
                };
                ui.painter().text(
                    r.right_top(),
                    egui::Align2::RIGHT_TOP,
                    mark,
                    font,
                    secondary,
                );
                ui.vertical(|ui| spans_ui(ui, cx, spans, Role::Body, primary));
            });
        }
        MdBlock::Quote(spans) => {
            let shown = Frame::new()
                .inner_margin(Margin {
                    left: space::MD as i8,
                    right: 0,
                    top: 0,
                    bottom: 0,
                })
                .show(ui, |ui| spans_ui(ui, cx, spans, Role::Body, secondary));
            let r = shown.response.rect;
            let bar = egui::Rect::from_min_max(r.left_top(), egui::pos2(r.left() + 2.0, r.bottom()));
            ui.painter().rect_filled(bar, 1.0, th.c("border.subtle"));
        }
        MdBlock::Code { lang, text } => {
            Frame::new()
                .fill(th.c("bg.sunken"))
                .stroke(Stroke::new(1.0, th.c("border.subtle")))
                .corner_radius(th.radius("md"))
                .inner_margin(Margin::same(space::SM as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !lang.is_empty() {
                        kit::caption(ui, th, lang);
                    }
                    egui::ScrollArea::horizontal()
                        .id_salt(("ai.md.code", id))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(
                                Label::new(
                                    RichText::new(text)
                                        .font(th.font(Role::Code))
                                        .color(primary),
                                )
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        });
                });
        }
        MdBlock::Table { head, align, rows } => {
            egui::ScrollArea::horizontal()
                .id_salt(("ai.md.tbl", id))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new(("ai.md.grid", id))
                        .striped(true)
                        .spacing(Vec2::new(space::MD, space::XS))
                        .show(ui, |ui| {
                            for (col, h) in head.iter().enumerate() {
                                let al = column_align(align, rows, col);
                                cell(ui, cx, h, al, Role::BodyStrong);
                            }
                            ui.end_row();
                            for r in rows {
                                for (col, c) in r.iter().enumerate() {
                                    let al = column_align(align, rows, col);
                                    cell(ui, cx, c, al, Role::Body);
                                }
                                ui.end_row();
                            }
                        });
                });
        }
        MdBlock::Rule => {
            ui.separator();
        }
    }
}

fn cell(ui: &mut Ui, cx: &Cx<'_>, spans: &[Span], al: ColAlign, role: Role) {
    let layout = match al {
        ColAlign::Right => Layout::right_to_left(Align::Min),
        ColAlign::Center => Layout::top_down(Align::Center),
        _ => Layout::left_to_right(Align::Min),
    };
    let col = cx.th.c("text.primary");
    ui.with_layout(layout, |ui| spans_ui(ui, cx, spans, role, col));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Span {
        Span {
            text: s.into(),
            ..Span::default()
        }
    }

    #[test]
    fn headings_levels_and_closing_hashes() {
        let v = md_blocks("# One\n## Two ##\n###### Six\n####### seven\n#tag");
        assert_eq!(
            v[0],
            MdBlock::Heading {
                level: 1,
                spans: vec![t("One")]
            }
        );
        assert_eq!(
            v[1],
            MdBlock::Heading {
                level: 2,
                spans: vec![t("Two")]
            }
        );
        assert!(matches!(v[2], MdBlock::Heading { level: 6, .. }));
        // Seven hashes and "#tag" are not headings.
        assert!(matches!(&v[3], MdBlock::Para(s) if plain(s).starts_with("####### seven")));
    }

    #[test]
    fn bullet_and_numbered_lists() {
        let v = md_blocks("- a\n* b\n+ c\n\n1. one\n2) two\n10. ten");
        let items: Vec<_> = v
            .iter()
            .map(|x| match x {
                MdBlock::Item { num, spans, .. } => (*num, plain(spans)),
                _ => panic!("not an item"),
            })
            .collect();
        assert_eq!(items[0], (None, "a".to_string()));
        assert_eq!(items[2], (None, "c".to_string()));
        assert_eq!(items[3], (Some(1), "one".to_string()));
        assert_eq!(items[4], (Some(2), "two".to_string()));
        assert_eq!(items[5], (Some(10), "ten".to_string()));
    }

    #[test]
    fn nested_lists_and_continuation() {
        let v = md_blocks("- top\n  - mid\n    - deep\n  - mid2\n- top2\n  continued");
        let d: Vec<usize> = v
            .iter()
            .map(|x| match x {
                MdBlock::Item { depth, .. } => *depth,
                _ => 99,
            })
            .collect();
        assert_eq!(d, vec![0, 1, 2, 1, 0]);
        assert!(matches!(&v[4], MdBlock::Item { spans, .. } if plain(spans) == "top2\ncontinued"));
    }

    #[test]
    fn four_space_nesting_and_tabs() {
        let v = md_blocks("1. a\n    - b\n\t- c");
        let d: Vec<usize> = v
            .iter()
            .map(|x| match x {
                MdBlock::Item { depth, .. } => *depth,
                _ => 99,
            })
            .collect();
        assert_eq!(d, vec![0, 1, 1]);
    }

    #[test]
    fn number_with_decimal_is_not_a_list() {
        let v = md_blocks("1.5 m wide\n-5 is negative\n**bold** start");
        assert_eq!(v.len(), 1);
        assert!(matches!(&v[0], MdBlock::Para(_)));
    }

    #[test]
    fn inline_bold_italic_code() {
        let s = inline("a **b** _c_ *d* `e` f");
        assert_eq!(plain(&s), "a b c d e f");
        assert!(s.iter().any(|x| x.bold && x.text == "b"));
        assert_eq!(s.iter().filter(|x| x.italic).count(), 2);
        assert!(s.iter().any(|x| x.code && x.text == "e"));
    }

    #[test]
    fn inline_nested_and_triple() {
        let s = inline("**bold *and italic* text**");
        assert!(s.iter().any(|x| x.bold && x.italic && x.text == "and italic"));
        let s = inline("***both***");
        assert_eq!(s.len(), 1);
        assert!(s[0].bold && s[0].italic && s[0].text == "both");
    }

    #[test]
    fn inline_code_keeps_markers_literal() {
        let s = inline("run `a **b** [c](d)` now");
        assert_eq!(plain(&s), "run a **b** [c](d) now");
        assert!(s.iter().any(|x| x.code && x.text == "a **b** [c](d)"));
        let s = inline("``a`b``");
        assert_eq!(s.len(), 1);
        assert!(s[0].code && s[0].text == "a`b");
    }

    #[test]
    fn snake_case_is_not_italic() {
        let s = inline("use wall_type_id and my_var_x");
        assert_eq!(plain(&s), "use wall_type_id and my_var_x");
        assert!(s.iter().all(|x| !x.italic));
    }

    #[test]
    fn links_keep_text_and_url() {
        let s = inline("see [the docs](https://example.org/a_b) now");
        assert_eq!(plain(&s), "see the docs now");
        let l = s.iter().find(|x| x.link.is_some()).unwrap();
        assert_eq!(l.text, "the docs");
        assert_eq!(l.link.as_deref(), Some("https://example.org/a_b"));
        // No title text in the address, no empty links.
        let s = inline("[x](http://h \"title\") [](u) [y]()");
        assert_eq!(s.iter().filter(|x| x.link.is_some()).count(), 1);
        assert_eq!(s[0].link.as_deref(), Some("http://h"));
    }

    #[test]
    fn html_and_script_stay_text() {
        let v = md_blocks("<script>alert(1)</script>\n<b>x</b>");
        assert_eq!(v.len(), 1);
        assert!(matches!(&v[0], MdBlock::Para(s) if plain(s) == "<script>alert(1)</script>\n<b>x</b>"));
    }

    #[test]
    fn escapes_make_markers_literal() {
        let s = inline(r"\*not italic\* and \`tick");
        assert_eq!(plain(&s), "*not italic* and `tick");
        assert!(s.iter().all(|x| !x.italic && !x.code));
    }

    #[test]
    fn fenced_code_with_language() {
        let v = md_blocks("text\n```rust\nfn main() {}\n\n  let x = 1;\n```\nafter");
        assert_eq!(v.len(), 3);
        assert_eq!(
            v[1],
            MdBlock::Code {
                lang: "rust".into(),
                text: "fn main() {}\n\n  let x = 1;".into()
            }
        );
        assert!(matches!(&v[2], MdBlock::Para(s) if plain(s) == "after"));
    }

    #[test]
    fn tilde_fence_and_markers_inside_code_are_literal() {
        let v = md_blocks("~~~\n# not a heading\n- not a list\n~~~");
        assert_eq!(
            v,
            vec![MdBlock::Code {
                lang: String::new(),
                text: "# not a heading\n- not a list".into()
            }]
        );
    }

    #[test]
    fn unterminated_fence_stays_text() {
        let v = md_blocks("intro\n```rust\nlet a = **1;");
        assert_eq!(v.len(), 2);
        assert!(matches!(&v[1], MdBlock::Para(s)
            if s.len() == 1 && s[0].text == "```rust\nlet a = **1;"));
    }

    #[test]
    fn unterminated_markers_stay_text() {
        for src in ["**bold", "*it", "`code", "[a](b", "[a]", "__x", "a ** b ** c"] {
            let s = inline(src);
            assert_eq!(plain(&s), src, "{src}");
            assert!(s.iter().all(|x| !x.bold && !x.italic && !x.code && x.link.is_none()));
        }
    }

    #[test]
    fn table_with_alignment() {
        let src = "| Name | Area | Note |\n|:--|--:|:-:|\n| A | 12.5 | ok |\n| B | 7 | `x` |\n\nafter";
        let v = md_blocks(src);
        assert_eq!(v.len(), 2);
        match &v[0] {
            MdBlock::Table { head, align, rows } => {
                assert_eq!(head.len(), 3);
                assert_eq!(plain(&head[1]), "Area");
                assert_eq!(rows.len(), 2);
                assert_eq!(
                    align,
                    &vec![ColAlign::Left, ColAlign::Right, ColAlign::Center]
                );
                assert!(rows[1][2][0].code);
            }
            _ => panic!("not a table"),
        }
    }

    #[test]
    fn table_numbers_right_align_automatically() {
        let v = md_blocks("| a | b |\n|---|---|\n| x | 1,5 |\n| y | -20 % |");
        let MdBlock::Table { align, rows, .. } = &v[0] else {
            panic!("not a table")
        };
        assert_eq!(column_align(align, rows, 0), ColAlign::Left);
        assert_eq!(column_align(align, rows, 1), ColAlign::Right);
    }

    #[test]
    fn table_short_rows_are_padded_and_escaped_pipe_kept() {
        let v = md_blocks("a | b\n--|--\none |\ntwo | x \\| y | extra");
        let MdBlock::Table { rows, .. } = &v[0] else {
            panic!("not a table")
        };
        assert_eq!(rows[0].len(), 2);
        assert_eq!(plain(&rows[1][1]), "x | y");
    }

    #[test]
    fn broken_table_falls_back_to_text() {
        // Separator cell count differs from the header: plain paragraph, no panic.
        let v = md_blocks("| a | b |\n|---|\n| 1 | 2 |");
        assert!(v.iter().all(|x| !matches!(x, MdBlock::Table { .. })));
        let v = md_blocks("a | b without separator");
        assert!(matches!(&v[0], MdBlock::Para(_)));
    }

    #[test]
    fn quote_and_rule() {
        let v = md_blocks("> one\n> two\n\n---\n* * *\ntext");
        assert!(matches!(&v[0], MdBlock::Quote(s) if plain(s) == "one\ntwo"));
        assert_eq!(v[1], MdBlock::Rule);
        assert_eq!(v[2], MdBlock::Rule);
        assert!(matches!(&v[3], MdBlock::Para(_)));
    }

    #[test]
    fn paragraphs_split_on_blank_lines_and_keep_line_breaks() {
        let v = md_blocks("first\nsecond\n\nthird\r\nfourth");
        assert_eq!(v.len(), 2);
        assert!(matches!(&v[0], MdBlock::Para(s) if plain(s) == "first\nsecond"));
        assert!(matches!(&v[1], MdBlock::Para(s) if plain(s) == "third\nfourth"));
    }

    #[test]
    fn cyrillic_and_multibyte_do_not_break_slicing() {
        let src = crate::app2_fx::fx("md_src");
        let v = md_blocks(&src);
        assert!(matches!(&v[0], MdBlock::Heading { level: 2, .. }));
        assert!(matches!(&v[1], MdBlock::Item { spans, .. }
            if spans.iter().any(|s| s.bold && s.text == crate::app2_fx::fx("md_walls"))));
        assert!(v.iter().any(|x| matches!(x, MdBlock::Table { .. })));
    }

    #[test]
    fn every_prefix_of_an_answer_parses() {
        let src = "# Ä\n**b `c` *d*** [l](u)\n- x\n  - y\n```r\nz\n```\n| a | b |\n|--|--|\n| 1 | 2 |\n> q\n---\n";
        for (i, _) in src.char_indices() {
            let _ = md_blocks(&src[..i]);
        }
        assert!(!md_blocks(src).is_empty());
    }

    #[test]
    fn hostile_input_is_bounded() {
        let deep = "*a ".repeat(2000) + &"*".repeat(2000);
        let _ = inline(&deep);
        let _ = inline(&"[".repeat(5000));
        let _ = inline(&"`".repeat(5000));
        let _ = md_blocks(&"- ".repeat(3000));
        let _ = md_blocks(&"> ".repeat(3000));
        assert!(md_blocks("").is_empty());
        assert!(md_blocks("\n\n  \n").is_empty());
    }

    #[test]
    fn copy_text_is_the_original_markdown() {
        let src = "# T\n- **a** `b`\n```\ncode\n```\n| x |\n|---|\n| 1 |\n";
        assert_eq!(copy_payload(src), src);
    }

    #[test]
    fn same_blocks_compare_equal_for_height_reuse() {
        let a = md_blocks("para\n\n- item");
        let grown = md_blocks("para\n\n- item\n\nnew");
        assert_eq!(a[0], grown[0]);
        assert_eq!(a[1], grown[1]);
        assert_eq!(grown.len(), 3);
    }

    #[test]
    fn marker_window_does_not_cut_inside_a_char() {
        let s = "*".to_string() + &"\u{44f}".repeat(3000) + "*";
        let v = inline(&s);
        assert_eq!(plain(&v), s);
    }

    #[test]
    fn element_references_become_pills_in_prose_and_code() {
        let v = inline("Gap between wall:12 and `solid:n3`, not note:5 or wall:.");
        let pills: Vec<_> = v.iter().filter_map(|s| s.pill.as_deref()).collect();
        assert_eq!(pills, ["wall:12", "solid:n3"]);
        assert_eq!(plain(&v), "Gap between wall:12 and solid:n3, not note:5 or wall:.");
        assert!(v.iter().filter(|s| s.pill.is_some()).all(|s| s.text == *s.pill.as_ref().unwrap()));
    }

    #[test]
    fn pill_shape_is_strict() {
        for ok in ["wall:1", "door:55", "solid:n7", "room:123456"] {
            assert!(is_pill(ok), "{ok}");
        }
        for bad in ["wall:", "wall:n", "wall:1a", "wal:1", ":1", "wall:1234567890123", "x"] {
            assert!(!is_pill(bad), "{bad}");
        }
    }

    #[test]
    fn a_pill_selects_its_element_unless_it_is_only_in_the_draft() {
        let got = pill_action("wall:12");
        let want = ("wall".to_string(), 12);
        assert!(matches!(got, Some(crate::ai::ui::Action::SelectEl(k)) if k == want));
        assert!(matches!(pill_action("solid:5"), Some(crate::ai::ui::Action::SelectEl(_))));
        assert!(pill_action("solid:n3").is_none());
    }
}
