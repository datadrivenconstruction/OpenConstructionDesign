// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ribbon geometry and keyboard state as pure functions: button sizes, three-high stacks,
//! balanced two-line labels, adaptive collapse (large, medium, small, then one drop-down per
//! group), split-button memory and key tips. No egui context, no fonts: callers pass widths.
use crate::theme::ribbon as k;
use egui::{pos2, vec2, Rect};
use serde::Deserialize;
use std::collections::HashMap;

/// Button size of a command (`size` in the commands json).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    /// 16 px icon only, label in the tooltip; stacks three high.
    Small,
    /// 16 px icon with the label on the right; stacks three high.
    #[default]
    Medium,
    /// 24 px icon over a label of at most two lines; full band height.
    Large,
}

impl Size {
    /// Collapse priority when the command sets none: 1 keeps its size longest, 9 shrinks first.
    pub fn default_priority(self) -> u8 {
        match self {
            Size::Large => 2,
            Size::Medium => 5,
            Size::Small => 7,
        }
    }
}

/// Collapse stage of one group; each step only makes the group narrower or keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// Every button at its own size; icon-only (small) buttons get their label while the tab has
    /// room.
    Full,
    /// Icon-only buttons are icon-only again; everything else as before. First step of a
    /// narrowing window, so labels of small buttons go before any large button shrinks.
    Bare,
    /// Large buttons below priority 1 become medium.
    Hero,
    /// No large buttons.
    NoLarge,
    /// Icons only.
    IconOnly,
    /// The whole group is one drop-down button.
    Collapsed,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Full,
        Stage::Bare,
        Stage::Hero,
        Stage::NoLarge,
        Stage::IconOnly,
        Stage::Collapsed,
    ];
}

/// Geometry in points; `Default` reads the `theme::ribbon` tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub band_h: f32,
    pub row_h: f32,
    pub rows: usize,
    pub large_min_w: f32,
    pub large_pad_x: f32,
    pub one_line_max: f32,
    pub medium_icon: f32,
    pub medium_pad_l: f32,
    pub medium_gap: f32,
    pub medium_pad_r: f32,
    pub small_w: f32,
    pub caret_w: f32,
    pub col_gap: f32,
    pub group_pad_x: f32,
    pub group_gap: f32,
    pub launcher_w: f32,
    pub title_pad_x: f32,
    pub overflow_w: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            band_h: k::BAND_H,
            row_h: k::ROW_H,
            rows: k::ROWS,
            large_min_w: k::LARGE_MIN_W,
            large_pad_x: k::LARGE_PAD_X,
            one_line_max: k::ONE_LINE_MAX,
            medium_icon: k::MEDIUM_ICON,
            medium_pad_l: k::MEDIUM_PAD_L,
            medium_gap: k::MEDIUM_GAP,
            medium_pad_r: k::MEDIUM_PAD_R,
            small_w: k::SMALL_W,
            caret_w: k::CARET_W,
            col_gap: k::COL_GAP,
            group_pad_x: k::GROUP_PAD_X,
            group_gap: k::GROUP_GAP,
            launcher_w: k::LAUNCHER_W,
            title_pad_x: k::TITLE_PAD_X,
            overflow_w: k::OVERFLOW_W,
        }
    }
}

/// A label set in at most two lines: words `[..split]` on line 1, the rest on line 2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wrap {
    pub split: usize,
    pub line1: f32,
    pub line2: f32,
    /// Block width, with the caret of a split button after the last line.
    pub w: f32,
}

impl Wrap {
    pub fn lines(&self) -> usize {
        if self.line2 > 0.0 {
            2
        } else {
            1
        }
    }
}

/// Two-line label of a large button. A label up to `one_line_max` stays on one line; a longer one
/// breaks at the word boundary that gives the narrowest block (balanced, not greedy). A split
/// button puts its caret after the last line, or alone on line 2 under a one-line label.
pub fn wrap2(words: &[f32], space: f32, split_btn: bool, m: &Metrics) -> Wrap {
    let line = |ws: &[f32]| -> f32 {
        if ws.is_empty() {
            0.0
        } else {
            ws.iter().sum::<f32>() + space * (ws.len() - 1) as f32
        }
    };
    let caret = if split_btn { m.caret_w } else { 0.0 };
    let one = line(words);
    if words.len() < 2 || one <= m.one_line_max {
        return Wrap {
            split: words.len(),
            line1: one,
            line2: 0.0,
            w: one.max(caret),
        };
    }
    let mut best: Option<Wrap> = None;
    for s in 1..words.len() {
        let (a, b) = (line(&words[..s]), line(&words[s..]));
        let w = a.max(b + caret);
        // ties go to the longer first line: the block reads top-heavy, like a title
        if best.is_none_or(|x| w <= x.w + 0.01) {
            best = Some(Wrap {
                split: s,
                line1: a,
                line2: b,
                w,
            });
        }
    }
    best.expect("two or more words")
}

/// One command as the layout sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemIn {
    pub size: Size,
    pub priority: u8,
    pub split: bool,
    /// One-line label width (medium buttons).
    pub label_w: f32,
    /// Two-line label block width (large buttons), from [`wrap2`].
    pub large_w: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupIn {
    pub title_w: f32,
    pub launcher: bool,
    /// Label block of the collapsed drop-down (the title, wrapped by [`wrap2`] with a caret).
    pub collapsed_w: f32,
    pub items: Vec<ItemIn>,
    /// Rarely used group (see [`LOW_USE_PRIORITY`]): it is either shown in full or collapsed to
    /// one labelled drop-down, never as bare icons, and collapses before any other group loses
    /// a label.
    pub low_use: bool,
}

/// Groups whose most important command has at least this priority number are low-use.
pub const LOW_USE_PRIORITY: u8 = 6;

impl GroupIn {
    /// True for a group of two or more commands that all rank [`LOW_USE_PRIORITY`] or lower.
    pub fn is_low_use(items: &[ItemIn]) -> bool {
        items.len() >= 2 && items.iter().all(|i| i.priority >= LOW_USE_PRIORITY)
    }
    /// Stages this group may be drawn at. Never icons only: a narrowing window folds a whole
    /// group into its labelled drop-down instead, so no command loses its caption (only icons
    /// everyone reads, sized `Small` in the data, go bare at [`Stage::Bare`]).
    pub fn allows(&self, s: Stage) -> bool {
        s != Stage::IconOnly && (!self.low_use || matches!(s, Stage::Full | Stage::Collapsed))
    }
    /// Most important priority in the group: the group shrinks when its turn comes.
    pub fn priority(&self) -> u8 {
        self.items.iter().map(|i| i.priority).min().unwrap_or(9)
    }
}

/// A placed button, relative to the band's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub item: usize,
    pub size: Size,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupOut {
    /// Stage the group is drawn at.
    pub stage: Stage,
    /// Last stage the group was offered; a step that would have widened it is drawn at the
    /// earlier stage, so `step >= stage`.
    pub step: Stage,
    /// Group frame, band coordinates: `x` from the band's left edge.
    pub x: f32,
    pub w: f32,
    /// Buttons; empty when collapsed (the drop-down is `collapsed_rect`).
    pub items: Vec<Placed>,
    pub collapsed_rect: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub groups: Vec<GroupOut>,
    /// Groups from this index on did not fit even collapsed and go to the trailing menu.
    pub overflow_from: usize,
    /// Width used, without the trailing menu.
    pub width: f32,
}

/// Size of an item at a stage.
pub fn effective(it: &ItemIn, stage: Stage) -> Size {
    match stage {
        Stage::Full if it.size == Size::Small => Size::Medium,
        Stage::Full | Stage::Bare => it.size,
        Stage::Hero if it.size == Size::Large && it.priority > 1 => Size::Medium,
        Stage::Hero => it.size,
        Stage::NoLarge => it.size.min(Size::Medium),
        Stage::IconOnly | Stage::Collapsed => Size::Small,
    }
}

/// Button width at a size; whole points so edges stay on pixels.
pub fn button_w(it: &ItemIn, size: Size, m: &Metrics) -> f32 {
    let caret = if it.split { m.caret_w } else { 0.0 };
    match size {
        Size::Large => (it.large_w + 2.0 * m.large_pad_x).max(m.large_min_w),
        Size::Medium => {
            m.medium_pad_l + m.medium_icon + m.medium_gap + it.label_w + m.medium_pad_r + caret
        }
        Size::Small => m.small_w + caret,
    }
    .ceil()
}

/// Columns of a group: a large button alone, medium or small ones stacked `rows` high; a run of
/// one size never shares a column with another size.
pub fn columns(sizes: &[Size], rows: usize) -> Vec<(Size, Vec<usize>)> {
    let mut out: Vec<(Size, Vec<usize>)> = Vec::new();
    for (i, &s) in sizes.iter().enumerate() {
        match out.last_mut() {
            Some((ls, v)) if s != Size::Large && *ls == s && v.len() < rows => v.push(i),
            _ => out.push((s, vec![i])),
        }
    }
    out
}

/// Minimum frame width for the title row (title centred, launcher at the right end).
fn title_need(g: &GroupIn, m: &Metrics) -> f32 {
    let launcher = if g.launcher { 2.0 * m.launcher_w } else { 0.0 };
    (g.title_w + 2.0 * m.title_pad_x + launcher).ceil()
}

/// Group content at a stage: frame width and the buttons, x from the frame's left edge.
pub fn group_at(g: &GroupIn, stage: Stage, m: &Metrics) -> (f32, Vec<Placed>, Option<Rect>) {
    if stage == Stage::Collapsed {
        // a drop-down with a caption is never as narrow as an icon-only pair
        let bw = (g.collapsed_w + 2.0 * m.large_pad_x)
            .max(m.large_min_w + m.large_pad_x)
            .ceil();
        let w = bw + 2.0 * m.group_pad_x;
        let r = Rect::from_min_size(pos2(m.group_pad_x, 0.0), vec2(bw, m.band_h));
        return (w, vec![], Some(r));
    }
    let sizes: Vec<Size> = g.items.iter().map(|it| effective(it, stage)).collect();
    let cols = columns(&sizes, m.rows);
    let mut placed = Vec::new();
    let mut x = 0.0;
    for (ci, (s, idx)) in cols.iter().enumerate() {
        if ci > 0 {
            x += m.col_gap;
        }
        let cw = idx
            .iter()
            .map(|&i| button_w(&g.items[i], *s, m))
            .fold(0.0, f32::max);
        for (row, &i) in idx.iter().enumerate() {
            let rect = if *s == Size::Large {
                Rect::from_min_size(pos2(x, 0.0), vec2(cw, m.band_h))
            } else {
                Rect::from_min_size(pos2(x, row as f32 * m.row_h), vec2(cw, m.row_h))
            };
            placed.push(Placed {
                item: i,
                size: *s,
                rect,
            });
        }
        x += cw;
    }
    let content = x + 2.0 * m.group_pad_x;
    let w = content.max(title_need(g, m)).ceil();
    // content centred in a frame widened by its title, on whole points
    let dx = ((w - x) / 2.0).floor();
    for p in &mut placed {
        p.rect = p.rect.translate(vec2(dx, 0.0));
    }
    (w, placed, None)
}

fn total(widths: &[f32], m: &Metrics) -> f32 {
    widths.iter().sum::<f32>() + m.group_gap * widths.len().saturating_sub(1) as f32
}

/// Order in which groups give up width at each stage: least important first (largest priority
/// number), then the rightmost.
pub fn shrink_order(groups: &[GroupIn]) -> Vec<usize> {
    let mut v: Vec<usize> = (0..groups.len()).collect();
    v.sort_by(|&a, &b| {
        groups[b]
            .priority()
            .cmp(&groups[a].priority())
            .then(b.cmp(&a))
    });
    v
}

/// When a step happens: its stage plus half a point per level of importance, so a priority-1
/// group gives up its large buttons only after priority-3 groups went icon-only and priority-7
/// groups collapsed. Ties: less important, then rightmost, first.
fn step_key(g: &GroupIn, s: Stage) -> f32 {
    let rank = (9 - g.priority().min(9)) as f32 * 0.5;
    if g.low_use && s == Stage::Collapsed {
        // before the first label of any group goes (keys of the other steps start at 1)
        rank * 0.1
    } else {
        s as u8 as f32 + rank
    }
}

/// Adaptive ribbon layout for `avail` points. Collapse steps (one group, one stage) run in
/// [`step_key`] order until the band fits; then width is given back, most important group first,
/// without any group ending larger than a more important one. `min` raises the starting stage
/// (small-buttons mode). Groups that still do not fit collapsed go to the trailing menu.
pub fn layout(groups: &[GroupIn], avail: f32, min: Stage, m: &Metrics) -> Layout {
    let min = min.min(Stage::IconOnly);
    let mut stage: Vec<Stage> = vec![min; groups.len()];
    let mut step = stage.clone();
    let width_of = |i: usize, s: Stage| group_at(&groups[i], s, m).0;
    let mut widths: Vec<f32> = (0..groups.len()).map(|i| width_of(i, stage[i])).collect();
    let mut steps: Vec<(usize, Stage)> = (0..groups.len())
        .flat_map(|i| Stage::ALL[1..].iter().map(move |&s| (i, s)))
        .collect();
    steps.sort_by(|&(a, sa), &(b, sb)| {
        step_key(&groups[a], sa)
            .total_cmp(&step_key(&groups[b], sb))
            .then(groups[b].priority().cmp(&groups[a].priority()))
            .then(b.cmp(&a))
    });
    for (i, s) in steps {
        if total(&widths, m) <= avail {
            break;
        }
        if step[i] >= s || !groups[i].allows(s) {
            continue;
        }
        step[i] = s;
        let w = width_of(i, s);
        // a step that would widen the group (restacking, a long title) keeps the earlier
        // geometry; a one-button group never turns into a menu
        let collapse_useless =
            s == Stage::Collapsed && (groups[i].items.len() < 2 || w >= widths[i]);
        // dropping labels that were never promoted gains nothing: stay at Full
        if w > widths[i] || collapse_useless || (s == Stage::Bare && w >= widths[i]) {
            continue;
        }
        stage[i] = s;
        widths[i] = w;
    }
    let mut cap = min;
    for i in shrink_order(groups).into_iter().rev() {
        let ok = |s: &&Stage| **s >= cap && **s < stage[i] && groups[i].allows(**s);
        for &s in Stage::ALL.iter().filter(ok) {
            let w = width_of(i, s);
            if total(&widths, m) - widths[i] + w <= avail {
                stage[i] = s;
                widths[i] = w;
                break;
            }
        }
        cap = cap.max(stage[i]);
    }
    let mut overflow_from = groups.len();
    if total(&widths, m) > avail {
        overflow_from = fitting(&widths, avail, m);
    }
    place(groups, &stage, &step, overflow_from, m)
}

/// Groups `[..n]` that fit beside the trailing menu.
fn fitting(widths: &[f32], avail: f32, m: &Metrics) -> usize {
    let room = avail - m.overflow_w - m.group_gap;
    let mut used = 0.0;
    let mut n = 0;
    for (i, w) in widths.iter().enumerate() {
        let next = used + if i > 0 { m.group_gap } else { 0.0 } + w;
        if next > room {
            break;
        }
        used = next;
        n = i + 1;
    }
    n
}

/// Every group's widest input over the shipped languages: `sets[l][g]` is group `g` measured in
/// language `l` (same commands, same order). Stages chosen on it hold in every language.
pub fn widest(sets: &[Vec<GroupIn>]) -> Vec<GroupIn> {
    let mut out = sets.first().cloned().unwrap_or_default();
    for set in sets.iter().skip(1) {
        for (o, g) in out.iter_mut().zip(set) {
            o.title_w = o.title_w.max(g.title_w);
            o.collapsed_w = o.collapsed_w.max(g.collapsed_w);
            for (a, b) in o.items.iter_mut().zip(&g.items) {
                a.label_w = a.label_w.max(b.label_w);
                a.large_w = a.large_w.max(b.large_w);
            }
        }
    }
    out
}

/// As [`layout`], with the stages chosen on `basis` (see [`widest`]) and the buttons measured
/// from `groups`: at one window width every language draws each group at the same stage, and a
/// shorter language is not padded to the longer one. `basis` is never narrower than `groups`.
pub fn layout_by(
    basis: &[GroupIn],
    groups: &[GroupIn],
    avail: f32,
    min: Stage,
    m: &Metrics,
) -> Layout {
    let b = layout(basis, avail, min, m);
    let stage: Vec<Stage> = b.groups.iter().map(|g| g.stage).collect();
    let step: Vec<Stage> = b.groups.iter().map(|g| g.step).collect();
    place(groups, &stage, &step, b.overflow_from, m)
}

/// Frames and buttons of groups `[..overflow_from]` at their stages, left to right.
fn place(
    groups: &[GroupIn],
    stage: &[Stage],
    step: &[Stage],
    overflow_from: usize,
    m: &Metrics,
) -> Layout {
    let mut out = Vec::new();
    let mut x = 0.0;
    for (i, g) in groups.iter().enumerate().take(overflow_from) {
        let (w, items, collapsed_rect) = group_at(g, stage[i], m);
        let shift = vec2(x, 0.0);
        out.push(GroupOut {
            stage: stage[i],
            step: step[i],
            x,
            w,
            items: items
                .into_iter()
                .map(|p| Placed {
                    rect: p.rect.translate(shift),
                    ..p
                })
                .collect(),
            collapsed_rect: collapsed_rect.map(|r| r.translate(shift)),
        });
        x += w + m.group_gap;
    }
    let width = (x - m.group_gap).max(0.0);
    Layout {
        groups: out,
        overflow_from,
        width,
    }
}

/// Split buttons remember the variant last chosen from the caret menu; the main part runs it.
#[derive(Clone, Debug, Default)]
pub struct SplitState {
    last: HashMap<String, String>,
}

impl SplitState {
    /// Command the main part runs: the remembered variant while it is still offered, else the
    /// parent itself.
    pub fn current<'a>(&'a self, parent: &'a str, variants: &[String]) -> &'a str {
        match self.last.get(parent) {
            Some(v) if variants.iter().any(|x| x == v) => v.as_str(),
            _ => parent,
        }
    }
    /// Records a pick from the caret menu; the parent itself resets the memory, and so does a
    /// `once` command (its definition says a pick must never repeat unasked). Unknown ids
    /// change nothing and return false.
    pub fn choose(&mut self, parent: &str, picked: &str, variants: &[String], once: bool) -> bool {
        if picked == parent || (variants.iter().any(|x| x == picked) && once) {
            self.last.remove(parent);
            true
        } else if variants.iter().any(|x| x == picked) {
            self.last.insert(parent.to_string(), picked.to_string());
            true
        } else {
            false
        }
    }
    /// Caret menu entries: the parent first, then its variants in json order.
    pub fn menu<'a>(parent: &'a str, variants: &'a [String]) -> Vec<&'a str> {
        std::iter::once(parent)
            .chain(variants.iter().map(String::as_str))
            .collect()
    }
}

/// Key tips of one level, all `len` characters long (so none is a prefix of another): the
/// preferred tip when it is free (a command's mnemonic), else letters taken from `hint` in order,
/// else the first free combination.
pub fn assign_tips(len: usize, items: &[(Option<&str>, &str)]) -> Vec<String> {
    const ALPHA: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ123456789";
    let mut taken: Vec<String> = Vec::new();
    let ok = |t: &str, taken: &[String]| {
        t.chars().count() == len
            && t.chars().all(|c| ALPHA.contains(c))
            && !taken.iter().any(|x| x == t)
    };
    for (pref, hint) in items {
        let mut cands: Vec<String> = Vec::new();
        if let Some(p) = pref {
            cands.push(p.to_ascii_uppercase());
        }
        let letters: Vec<char> = hint
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        if let Some(&first) = letters.first() {
            if len == 1 {
                cands.extend(letters.iter().map(|c| c.to_string()));
            } else {
                // initials of words first, then the first letter with each later one
                let initials: String = hint
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter_map(|w| w.chars().next())
                    .map(|c| c.to_ascii_uppercase())
                    .take(len)
                    .collect();
                cands.push(initials);
                cands.extend(letters[1..].iter().map(|c| format!("{first}{c}")));
                cands.extend(ALPHA.chars().map(|c| format!("{first}{c}")));
            }
        }
        let pick = cands
            .into_iter()
            .find(|t| ok(t, &taken))
            .or_else(|| combos(ALPHA, len).find(|t| ok(t, &taken)))
            .unwrap_or_default();
        taken.push(pick);
    }
    taken
}

fn combos(alpha: &'static str, len: usize) -> Box<dyn Iterator<Item = String>> {
    if len <= 1 {
        Box::new(alpha.chars().map(|c| c.to_string()))
    } else {
        Box::new(alpha.chars().flat_map(move |c| {
            combos(alpha, len - 1).map(move |rest| format!("{c}{rest}"))
        }))
    }
}

/// Key tip level: Alt shows tab letters, a tab letter opens its tab and shows command letters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TipLevel {
    #[default]
    Off,
    Tabs,
    Commands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipHit {
    /// Typed so far is the start of a tip.
    Pending,
    /// Tip `i` is complete.
    Hit(usize),
    /// No tip starts like this; the typed text is dropped.
    Miss,
}

#[derive(Clone, Debug, Default)]
pub struct KeyTips {
    pub level: TipLevel,
    pub typed: String,
    alt_down: bool,
    alt_solo: bool,
}

impl KeyTips {
    pub fn active(&self) -> bool {
        self.level != TipLevel::Off
    }
    /// Feeds the Alt state of a frame and whether any other key or button went down; Alt pressed
    /// and released alone toggles the tips. Returns true when it toggled.
    pub fn alt(&mut self, alt_now: bool, other_pressed: bool) -> bool {
        let mut toggled = false;
        if alt_now && !self.alt_down {
            self.alt_solo = !other_pressed;
        } else if alt_now && other_pressed {
            self.alt_solo = false;
        } else if !alt_now && self.alt_down && self.alt_solo {
            self.level = if self.active() {
                TipLevel::Off
            } else {
                TipLevel::Tabs
            };
            self.typed.clear();
            toggled = true;
        }
        self.alt_down = alt_now;
        toggled
    }
    /// One typed character against the tips of the current level.
    pub fn feed(&mut self, c: char, tips: &[String]) -> TipHit {
        self.typed.push(c.to_ascii_uppercase());
        if let Some(i) = tips.iter().position(|t| *t == self.typed) {
            self.typed.clear();
            return TipHit::Hit(i);
        }
        if tips.iter().any(|t| t.starts_with(&self.typed)) {
            TipHit::Pending
        } else {
            self.typed.clear();
            TipHit::Miss
        }
    }
    /// Esc: half-typed tip first, then one level up.
    pub fn escape(&mut self) {
        if !self.typed.is_empty() {
            self.typed.clear();
            return;
        }
        self.level = match self.level {
            TipLevel::Commands => TipLevel::Tabs,
            _ => TipLevel::Off,
        };
    }
    pub fn off(&mut self) {
        self.level = TipLevel::Off;
        self.typed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(size: Size, prio: u8, label_w: f32) -> ItemIn {
        let m = Metrics::default();
        ItemIn {
            size,
            priority: prio,
            split: false,
            label_w,
            large_w: wrap2(&[label_w], 4.0, false, &m).w,
        }
    }

    fn grp(items: Vec<ItemIn>) -> GroupIn {
        GroupIn {
            title_w: 40.0,
            launcher: false,
            collapsed_w: 40.0,
            low_use: GroupIn::is_low_use(&items),
            items,
        }
    }

    #[test]
    fn balanced_two_lines() {
        let m = Metrics::default();
        // short: one line
        let w = wrap2(&[30.0], 3.0, false, &m);
        assert_eq!((w.lines(), w.w), (1, 30.0));
        // greedy would put 3 words on line 1; balanced splits 40+3+20 | 40
        let w = wrap2(&[40.0, 20.0, 40.0], 3.0, false, &m);
        assert_eq!((w.split, w.line1, w.line2), (2, 63.0, 40.0));
        // two words: one per line
        let w = wrap2(&[50.0, 30.0], 3.0, false, &m);
        assert_eq!((w.split, w.w), (1, 50.0));
        // split button: caret after line 2, or alone under one line
        let w = wrap2(&[50.0, 45.0], 3.0, true, &m);
        assert_eq!(w.w, 57.0);
        let w = wrap2(&[30.0], 3.0, true, &m);
        assert_eq!(w.w, 30.0);
    }

    #[test]
    fn stacks_three_high_without_mixing() {
        use Size::*;
        let cols = columns(&[Large, Medium, Medium, Medium, Medium, Small, Small, Large], 3);
        let shape: Vec<(Size, usize)> = cols.iter().map(|(s, v)| (*s, v.len())).collect();
        assert_eq!(
            shape,
            vec![(Large, 1), (Medium, 3), (Medium, 1), (Small, 2), (Large, 1)]
        );
    }

    #[test]
    fn group_steps_down_through_the_stages() {
        let m = Metrics::default();
        let g = grp(vec![
            it(Size::Large, 1, 30.0),
            it(Size::Large, 3, 50.0),
            it(Size::Medium, 5, 70.0),
            it(Size::Medium, 5, 40.0),
            it(Size::Small, 7, 40.0),
        ]);
        let gs = vec![g];
        let mut seen = vec![];
        for avail in (0..400).rev().step_by(4) {
            let l = layout(&gs, avail as f32, Stage::Full, &m);
            assert!(l.width <= avail as f32, "avail {avail}: {}", l.width);
            seen.push(l.groups.first().map(|g| g.stage));
        }
        assert!(seen.contains(&Some(Stage::Hero)) && seen.contains(&Some(Stage::Collapsed)));
        // a group never drops its captions; it collapses into its labelled menu
        assert!(!seen.contains(&Some(Stage::IconOnly)), "{seen:?}");
    }

    #[test]
    fn collapse_order_least_important_first() {
        let m = Metrics::default();
        let hero = grp(vec![it(Size::Large, 1, 30.0), it(Size::Large, 1, 30.0)]);
        let side = grp(vec![
            it(Size::Large, 4, 40.0),
            it(Size::Medium, 6, 60.0),
            it(Size::Medium, 6, 60.0),
        ]);
        let tail = grp(vec![
            it(Size::Medium, 8, 80.0),
            it(Size::Medium, 8, 80.0),
            it(Size::Medium, 8, 80.0),
            it(Size::Medium, 8, 80.0),
        ]);
        let gs = vec![hero, tail, side];
        assert_eq!(shrink_order(&gs), vec![1, 2, 0]);
        let full = layout(&gs, 10_000.0, Stage::Full, &m);
        assert!(full.groups.iter().all(|g| g.stage == Stage::Full));
        let mut prev: Vec<Stage> = vec![Stage::Full; 3];
        let mut w = full.width;
        while w > 0.0 {
            w -= 8.0;
            let l = layout(&gs, w, Stage::Full, &m);
            let st: Vec<Stage> = (0..3)
                .map(|i| l.groups.get(i).map_or(Stage::Collapsed, |g| g.step))
                .collect();
            // narrower never un-collapses anything
            for i in 0..3 {
                assert!(st[i] >= prev[i], "w {w}: {st:?} after {prev:?}");
            }
            // the hero group is never further along than the less important ones
            assert!(st[0] <= st[1] && st[0] <= st[2], "w {w}: {st:?}");
            if l.overflow_from == 3 {
                assert!(l.width <= w, "w {w}: {}", l.width);
            }
            prev = st;
        }
    }

    #[test]
    fn hero_stays_large_longest() {
        let m = Metrics::default();
        let g = grp(vec![it(Size::Large, 1, 30.0), it(Size::Large, 3, 30.0)]);
        let (_, p, _) = group_at(&g, Stage::Hero, &m);
        assert_eq!((p[0].size, p[1].size), (Size::Large, Size::Medium));
        let (_, p, _) = group_at(&g, Stage::NoLarge, &m);
        assert_eq!(p[0].size, Size::Medium);
    }

    #[test]
    fn small_buttons_get_labels_while_there_is_room() {
        let m = Metrics::default();
        let g = grp(vec![
            it(Size::Large, 2, 40.0),
            it(Size::Small, 7, 50.0),
            it(Size::Small, 7, 50.0),
        ]);
        let sizes = |s: Stage| -> Vec<Size> { group_at(&g, s, &m).1.iter().map(|p| p.size).collect() };
        assert_eq!(sizes(Stage::Full), vec![Size::Large, Size::Medium, Size::Medium]);
        assert_eq!(sizes(Stage::Bare), vec![Size::Large, Size::Small, Size::Small]);
        assert!(group_at(&g, Stage::Bare, &m).0 < group_at(&g, Stage::Full, &m).0);
        let gs = vec![g];
        let wide = layout(&gs, 10_000.0, Stage::Full, &m);
        assert_eq!(wide.groups[0].stage, Stage::Full);
        // small-buttons mode never promotes
        let small = layout(&gs, 10_000.0, Stage::NoLarge, &m);
        assert!(small.groups[0].items.iter().all(|p| p.size != Size::Medium || p.item == 0));
        // narrowing: labels go first, the large button keeps its size
        let need = wide.width;
        let l = layout(&gs, need - 1.0, Stage::Full, &m);
        let g0 = &l.groups[0];
        assert_eq!(g0.stage, Stage::Bare);
        assert_eq!(g0.items[0].size, Size::Large);
        assert!(g0.items[1..].iter().all(|p| p.size == Size::Small));
    }

    #[test]
    fn low_use_group_collapses_before_any_label_goes() {
        let m = Metrics::default();
        let main = grp(vec![
            it(Size::Large, 1, 40.0),
            it(Size::Small, 7, 50.0),
            it(Size::Small, 7, 50.0),
        ]);
        let rare = grp(vec![
            it(Size::Medium, 6, 60.0),
            it(Size::Medium, 7, 60.0),
            it(Size::Small, 8, 60.0),
        ]);
        assert!(!main.low_use && rare.low_use);
        let gs = vec![main, rare];
        let wide = layout(&gs, 10_000.0, Stage::Full, &m);
        assert!(wide.groups.iter().all(|g| g.stage == Stage::Full));
        let mut collapsed = false;
        let mut w = wide.width;
        while w > 0.0 {
            w -= 3.0;
            let l = layout(&gs, w, Stage::Full, &m);
            if l.overflow_from < 2 {
                break;
            }
            // the low-use group is never icon-only, whatever the width
            assert!(matches!(l.groups[1].stage, Stage::Full | Stage::Collapsed), "w {w}");
            collapsed |= l.groups[1].stage == Stage::Collapsed;
            if l.groups[0].stage > Stage::Full {
                assert_eq!(l.groups[1].stage, Stage::Collapsed, "w {w}: label lost first");
            }
        }
        assert!(collapsed);
    }

    #[test]
    fn small_mode_starts_without_large() {
        let m = Metrics::default();
        let gs = vec![grp(vec![it(Size::Large, 1, 30.0), it(Size::Medium, 5, 30.0)])];
        let l = layout(&gs, 10_000.0, Stage::NoLarge, &m);
        assert!(l.groups[0].items.iter().all(|p| p.size != Size::Large));
    }

    #[test]
    fn overflow_when_nothing_else_helps() {
        let m = Metrics::default();
        let gs: Vec<GroupIn> = (0..6)
            .map(|_| grp(vec![it(Size::Medium, 5, 60.0), it(Size::Medium, 5, 60.0)]))
            .collect();
        let l = layout(&gs, 200.0, Stage::Full, &m);
        assert!(l.overflow_from < 6);
        assert!(l.width + m.group_gap + m.overflow_w <= 200.0, "{}", l.width);
    }

    #[test]
    fn split_remembers_the_last_variant() {
        let vars = vec!["arch.wall_room".to_string(), "arch.curtain_wall".to_string()];
        let mut s = SplitState::default();
        assert_eq!(s.current("arch.wall", &vars), "arch.wall");
        assert!(s.choose("arch.wall", "arch.curtain_wall", &vars, false));
        assert_eq!(s.current("arch.wall", &vars), "arch.curtain_wall");
        assert!(!s.choose("arch.wall", "arch.door", &vars, false));
        assert_eq!(s.current("arch.wall", &vars), "arch.curtain_wall");
        // a variant that is no longer offered falls back to the parent
        assert_eq!(s.current("arch.wall", &vars[..1]), "arch.wall");
        assert!(s.choose("arch.wall", "arch.wall", &vars, false));
        assert_eq!(s.current("arch.wall", &vars), "arch.wall");
        assert_eq!(
            SplitState::menu("arch.wall", &vars),
            vec!["arch.wall", "arch.wall_room", "arch.curtain_wall"]
        );
    }

    #[test]
    fn key_tips_unique_and_prefix_free() {
        let tabs = assign_tips(
            1,
            &[
                (None, "arch"),
                (None, "struct"),
                (None, "systems"),
                (None, "annotate"),
                (None, "modify"),
                (None, "view"),
                (None, "manage"),
                (None, "data"),
                (None, "mass"),
            ],
        );
        assert_eq!(tabs[..3], ["A", "S", "Y"]);
        let cmds = assign_tips(
            2,
            &[
                (Some("WA"), "wall"),
                (Some("WA"), "wall_room"),
                (None, "door"),
                (None, "door frame"),
                (Some("x"), "x"),
            ],
        );
        assert_eq!(cmds[..3], ["WA", "WR", "DO"]);
        assert_eq!(cmds[3], "DF");
        for set in [&tabs, &cmds] {
            for (i, a) in set.iter().enumerate() {
                assert!(!a.is_empty());
                for b in &set[i + 1..] {
                    assert!(a != b && !b.starts_with(a.as_str()), "{set:?}");
                }
            }
        }
    }

    #[test]
    fn key_tips_flow() {
        let mut t = KeyTips::default();
        // Alt with another key is a shortcut, not the tips
        assert!(!t.alt(true, false));
        assert!(!t.alt(true, true));
        assert!(!t.alt(false, false));
        assert!(!t.active());
        // Alt alone shows the tab letters
        t.alt(true, false);
        assert!(t.alt(false, false));
        assert_eq!(t.level, TipLevel::Tabs);
        let tips = vec!["WA".to_string(), "WR".to_string(), "DO".to_string()];
        assert_eq!(t.feed('w', &tips), TipHit::Pending);
        assert_eq!(t.feed('r', &tips), TipHit::Hit(1));
        assert_eq!(t.feed('q', &tips), TipHit::Miss);
        assert!(t.typed.is_empty());
        t.level = TipLevel::Commands;
        t.feed('d', &tips);
        t.escape();
        assert_eq!((t.level, t.typed.as_str()), (TipLevel::Commands, ""));
        t.escape();
        assert_eq!(t.level, TipLevel::Tabs);
        t.escape();
        assert_eq!(t.level, TipLevel::Off);
        // Alt alone again while shown hides them
        t.alt(true, false);
        t.alt(false, false);
        t.alt(true, false);
        assert!(t.alt(false, false));
        assert!(!t.active());
    }
}
