// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Keyboard cursor of the schedule table: a painted active cell, arrows/Tab/Home/End moves,
//! Enter/F2/typing start the edit, Enter commits and steps down, Esc cancels.
//! The pure parts (`cell_target`, `scroll_for`, `edit_start`, `post_edit`, `keys_enabled`) are
//! tested without a frame; `View` methods only glue them to the grid state.

use super::*;
use ddc_ui::sem::stroke;

/// Seconds the "read-only" hint stays next to the cell.
const HINT_SECS: f64 = 1.5;

/// Id of the focusable widget that stands for the grid body.
pub(super) fn focus_id() -> egui::Id {
    egui::Id::new("ddc.table.cursor")
}

/// The grid body owns the keyboard (not a text field): Ctrl+C/V/Z must still reach the
/// table and the command keymap, plain letters must not run shortcuts.
pub(crate) fn grid_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.has_focus(focus_id()))
}

fn hint_id() -> egui::Id {
    egui::Id::new("ddc.table.cursor.readonly")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Left,
    Right,
    Up,
    Down,
    /// Tab: next cell, wraps to the first cell of the next row.
    Tab,
    /// Shift+Tab: previous cell, wraps to the last cell of the previous row.
    BackTab,
    /// Home: first visible column of the row.
    Home,
    /// End: last visible column of the row.
    End,
    /// Ctrl+Home: first cell of the table.
    Top,
    /// Ctrl+End: last cell of the table.
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Intent {
    Nav(NavKey),
    /// Enter or F2.
    Edit,
    /// A printable character: starts the edit with that text.
    Type(String),
}

/// Where a key press moves the cursor. `cur` is (row place in view order, table column);
/// `vis` lists the visible table columns in order, so hidden columns are never landed on.
/// Edges clamp. No cursor yet: the first key lands on the first cell. Empty table: `None`.
pub fn cell_target(
    cur: Option<(usize, usize)>,
    key: NavKey,
    rows: usize,
    vis: &[usize],
) -> Option<(usize, usize)> {
    if rows == 0 || vis.is_empty() {
        return None;
    }
    let Some((p, j)) = cur else {
        return Some((0, vis[0]));
    };
    let (last_r, last_c) = (rows - 1, vis.len() - 1);
    let p = p.min(last_r);
    let q = vis.iter().position(|&c| c == j).unwrap_or(0);
    let (p, q) = match key {
        NavKey::Left => (p, q.saturating_sub(1)),
        NavKey::Right => (p, (q + 1).min(last_c)),
        NavKey::Up => (p.saturating_sub(1), q),
        NavKey::Down => ((p + 1).min(last_r), q),
        NavKey::Tab if q < last_c => (p, q + 1),
        NavKey::Tab if p < last_r => (p + 1, 0),
        NavKey::Tab => (p, q),
        NavKey::BackTab if q > 0 => (p, q - 1),
        NavKey::BackTab if p > 0 => (p - 1, last_c),
        NavKey::BackTab => (p, q),
        NavKey::Home => (p, 0),
        NavKey::End => (p, last_c),
        NavKey::Top => (0, 0),
        NavKey::Bottom => (last_r, last_c),
    };
    Some((p, vis[q]))
}

/// Scroll offset that brings display line `line` into view, `None` when it already is.
/// `seen` is the line range drawn last frame; its last line may be cut off.
pub fn scroll_for(line: usize, seen: std::ops::Range<usize>, rh: f32) -> Option<f32> {
    if line <= seen.start {
        return Some(line as f32 * rh);
    }
    let whole = seen.len().saturating_sub(1).max(1);
    if line + 1 >= seen.end {
        return Some((line + 1).saturating_sub(whole) as f32 * rh);
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditStart {
    /// Read-only column: no edit, show the hint.
    Blocked,
    Choice,
    /// Text editor with the given text, or the cell's own text when `None`.
    Text(Option<String>),
}

pub fn edit_start(edit: Edit, seed: Option<String>) -> EditStart {
    match edit {
        Edit::No => EditStart::Blocked,
        Edit::Choice => EditStart::Choice,
        _ => EditStart::Text(seed),
    }
}

/// After an edit ended: (give the keyboard back to the grid, step to the next row).
/// Only Enter and Esc hand the keyboard back, a click elsewhere keeps its own focus.
pub fn post_edit(enter: bool, esc: bool, committed: bool) -> (bool, bool) {
    (enter || esc, enter && committed)
}

/// Keys act only while the grid owns the keyboard: no text field, dialog or popup, no edit.
pub fn keys_enabled(
    grid_focus: bool,
    table_clicked: bool,
    text_input: bool,
    canvas_owns: bool,
    busy: bool,
) -> bool {
    grid_focus && table_clicked && !text_input && canvas_owns && !busy
}

/// The hint is shown while `now` is before the deadline.
pub fn hint_live(deadline: Option<f64>, now: f64) -> bool {
    deadline.is_some_and(|d| now < d)
}

fn note_readonly(ctx: &egui::Context) {
    let until = ctx.input(|i| i.time) + HINT_SECS;
    ctx.data_mut(|d| d.insert_temp(hint_id(), until));
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(HINT_SECS));
}

fn read_keys(ctx: &egui::Context) -> Option<Intent> {
    use egui::{Key, Modifiers as M};
    ctx.input_mut(|i| {
        // order matters: the modified forms first
        let nav = [
            (M::COMMAND, Key::Home, NavKey::Top),
            (M::COMMAND, Key::End, NavKey::Bottom),
            (M::SHIFT, Key::Tab, NavKey::BackTab),
            (M::NONE, Key::Tab, NavKey::Tab),
            (M::NONE, Key::ArrowLeft, NavKey::Left),
            (M::NONE, Key::ArrowRight, NavKey::Right),
            (M::NONE, Key::ArrowUp, NavKey::Up),
            (M::NONE, Key::ArrowDown, NavKey::Down),
            (M::NONE, Key::Home, NavKey::Home),
            (M::NONE, Key::End, NavKey::End),
        ];
        for (m, k, n) in nav {
            if i.consume_key(m, k) {
                return Some(Intent::Nav(n));
            }
        }
        if i.consume_key(M::NONE, Key::Enter) || i.consume_key(M::NONE, Key::F2) {
            return Some(Intent::Edit);
        }
        let typed = i.events.iter().find_map(|e| match e {
            egui::Event::Text(s) => s
                .chars()
                .next()
                .filter(|c| !c.is_control() && !c.is_whitespace())
                .map(|_| s.clone()),
            _ => None,
        });
        if typed.is_some() {
            // the editor opens this frame; the same text must not be typed twice
            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
        }
        typed.map(Intent::Type)
    })
}

/// Active cell outline on the focus token, inside the cell, plus the read-only hint.
pub(super) fn paint_active(
    ui: &egui::Ui,
    rr: Rect,
    xs: &[f32],
    cols: &[usize],
    active: Option<(usize, usize)>,
    ri: usize,
    th: &Theme,
) {
    let Some((r, j)) = active else { return };
    let Some(k) = cols.iter().position(|&c| c == j).filter(|_| r == ri) else {
        return;
    };
    let cell = Rect::from_x_y_ranges(rr.left() + xs[k]..=rr.left() + xs[k + 1], rr.y_range());
    ui.painter().rect_stroke(
        cell,
        0.0,
        egui::Stroke::new(stroke::FOCUS, th.c("focus.ring")),
        egui::StrokeKind::Inside,
    );
    let ctx = ui.ctx();
    let until: Option<f64> = ctx.data(|d| d.get_temp(hint_id()));
    if hint_live(until, ctx.input(|i| i.time)) {
        egui::Area::new(ui.id().with("ddc.table.cursor.hint"))
            .order(egui::Order::Foreground)
            .fixed_pos(cell.left_bottom())
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(crate::tr("sch.ui.readonly_cell", &[]));
                });
            });
    }
}

/// The grid body as one focusable widget: it keeps Tab and arrows from moving egui focus.
pub(super) fn register(ui: &mut egui::Ui, body: Rect) {
    let id = focus_id();
    let _ = ui.interact(body, id, Sense::focusable_noninteractive());
    if ui.memory(|m| m.has_focus(id)) {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            )
        });
    }
}

/// A click in the grid gives it the keyboard.
pub(super) fn take_focus(ui: &egui::Ui) {
    ui.memory_mut(|m| m.request_focus(focus_id()));
}

impl View {
    /// Keyboard moves and edit start; runs before the grid is drawn.
    pub(super) fn cursor_keys(&mut self, ctx: &egui::Context, rh: f32, u: Units) {
        let busy = self.editing.is_some() || self.fields_open || self.import.is_some()
            || self.reading.is_some();
        let on = keys_enabled(
            ctx.memory(|m| m.has_focus(focus_id())),
            crate::clip_tools::table_focused(),
            // wants_keyboard_input is true for any focused widget, the grid's own one too
            ctx.wants_keyboard_input() && !grid_focused(ctx),
            crate::ui_kit::canvas_owns_keys(ctx),
            busy,
        );
        if !on {
            return;
        }
        match read_keys(ctx) {
            Some(Intent::Nav(k)) => {
                let cur = self.active_pos().zip(self.active).map(|((p, _), (_, j))| (p, j));
                self.go(cur, k, rh);
            }
            Some(Intent::Edit) => self.begin_edit(ctx, None, u),
            Some(Intent::Type(s)) => self.begin_edit(ctx, Some(s), u),
            None => {}
        }
    }

    fn go(&mut self, cur: Option<(usize, usize)>, k: NavKey, rh: f32) {
        let vis = self.visible_cols();
        let Some((p, j)) = cell_target(cur, k, self.ord.len(), &vis) else {
            return;
        };
        self.active = Some((self.ord[p] as usize, j));
        self.reveal_col(j);
        if let Some(off) = scroll_for(self.line_of(p), self.visible.clone(), rh) {
            self.scroll_to = Some(off);
        }
    }

    fn begin_edit(&mut self, ctx: &egui::Context, seed: Option<String>, u: Units) {
        let Some((ri, j)) = self.active else { return };
        let Some(tb) = self.table.as_ref() else { return };
        let (Some(row), Some(c)) = (tb.rows.get(ri), tb.cols.get(j)) else {
            return;
        };
        match edit_start(c.edit, seed) {
            EditStart::Blocked => note_readonly(ctx),
            EditStart::Choice => self.editing = Some(Editing::Choice(ri, j)),
            EditStart::Text(s) => {
                let text = s.unwrap_or_else(|| edit_text(&row.vals[j], c.kind, u));
                self.editing = Some(Editing::Text(ri, j, text, true));
            }
        }
    }

    /// Called when an edit ended: `done` is the committed cell. Enter steps down a row.
    pub(super) fn after_edit(&mut self, ctx: &egui::Context, done: Option<(usize, usize)>, rh: f32) {
        let (enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let (refocus, down) = post_edit(enter, esc, done.is_some());
        if refocus {
            ctx.memory_mut(|m| m.request_focus(focus_id()));
        }
        if let (true, Some((ri, j))) = (down, done) {
            let cur = self
                .ord
                .iter()
                .position(|&i| i as usize == ri)
                .map(|p| (p, j));
            self.go(cur, NavKey::Down, rh);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [usize; 3] = [0, 1, 2];

    fn t(cur: (usize, usize), k: NavKey, rows: usize, vis: &[usize]) -> (usize, usize) {
        cell_target(Some(cur), k, rows, vis).unwrap()
    }

    #[test]
    fn arrows_clamp_at_edges() {
        assert_eq!(t((0, 0), NavKey::Up, 4, &ALL), (0, 0));
        assert_eq!(t((0, 0), NavKey::Left, 4, &ALL), (0, 0));
        assert_eq!(t((3, 2), NavKey::Down, 4, &ALL), (3, 2));
        assert_eq!(t((3, 2), NavKey::Right, 4, &ALL), (3, 2));
        assert_eq!(t((1, 1), NavKey::Up, 4, &ALL), (0, 1));
        assert_eq!(t((1, 1), NavKey::Down, 4, &ALL), (2, 1));
        assert_eq!(t((1, 1), NavKey::Left, 4, &ALL), (1, 0));
        assert_eq!(t((1, 1), NavKey::Right, 4, &ALL), (1, 2));
    }

    #[test]
    fn tab_wraps_to_next_and_previous_row() {
        assert_eq!(t((0, 2), NavKey::Tab, 3, &ALL), (1, 0));
        assert_eq!(t((0, 0), NavKey::Tab, 3, &ALL), (0, 1));
        assert_eq!(t((1, 0), NavKey::BackTab, 3, &ALL), (0, 2));
        assert_eq!(t((1, 2), NavKey::BackTab, 3, &ALL), (1, 1));
        // the ends of the table stay put
        assert_eq!(t((2, 2), NavKey::Tab, 3, &ALL), (2, 2));
        assert_eq!(t((0, 0), NavKey::BackTab, 3, &ALL), (0, 0));
    }

    #[test]
    fn home_end_and_corners() {
        assert_eq!(t((2, 1), NavKey::Home, 4, &ALL), (2, 0));
        assert_eq!(t((2, 1), NavKey::End, 4, &ALL), (2, 2));
        assert_eq!(t((2, 1), NavKey::Top, 4, &ALL), (0, 0));
        assert_eq!(t((2, 1), NavKey::Bottom, 4, &ALL), (3, 2));
    }

    #[test]
    fn empty_table_has_no_target() {
        assert_eq!(cell_target(None, NavKey::Down, 0, &ALL), None);
        assert_eq!(cell_target(Some((0, 0)), NavKey::Tab, 0, &ALL), None);
        assert_eq!(cell_target(None, NavKey::Down, 3, &[]), None);
    }

    #[test]
    fn first_key_lands_on_first_cell() {
        assert_eq!(cell_target(None, NavKey::Down, 3, &[4, 7]), Some((0, 4)));
        assert_eq!(cell_target(None, NavKey::Bottom, 3, &[4, 7]), Some((0, 4)));
    }

    #[test]
    fn hidden_columns_are_skipped() {
        let vis = [0usize, 2, 5];
        assert_eq!(t((0, 0), NavKey::Right, 2, &vis), (0, 2));
        assert_eq!(t((0, 2), NavKey::Right, 2, &vis), (0, 5));
        assert_eq!(t((0, 5), NavKey::Tab, 2, &vis), (1, 0));
        assert_eq!(t((1, 0), NavKey::BackTab, 2, &vis), (0, 5));
        assert_eq!(t((0, 0), NavKey::End, 2, &vis), (0, 5));
        // the cursor column was hidden meanwhile: counts from the first visible one
        assert_eq!(t((0, 1), NavKey::Right, 2, &vis), (0, 2));
    }

    #[test]
    fn cursor_row_past_the_end_is_clamped() {
        assert_eq!(t((9, 0), NavKey::Up, 3, &ALL), (1, 0));
    }

    #[test]
    fn scroll_brings_cursor_into_view() {
        let rh = 20.0;
        // drawn 10..18, last line cut off
        assert_eq!(scroll_for(12, 10..18, rh), None);
        assert_eq!(scroll_for(9, 10..18, rh), Some(180.0));
        assert_eq!(scroll_for(10, 10..18, rh), Some(200.0));
        assert_eq!(scroll_for(17, 10..18, rh), Some(220.0));
        assert_eq!(scroll_for(30, 10..18, rh), Some(480.0));
        assert_eq!(scroll_for(0, 0..0, rh), Some(0.0));
    }

    #[test]
    fn edit_starts_on_editable_columns_only() {
        assert_eq!(edit_start(Edit::No, None), EditStart::Blocked);
        assert_eq!(edit_start(Edit::No, Some("5".into())), EditStart::Blocked);
        assert_eq!(edit_start(Edit::Choice, None), EditStart::Choice);
        assert_eq!(edit_start(Edit::Text, None), EditStart::Text(None));
        assert_eq!(
            edit_start(Edit::Num, Some("5".into())),
            EditStart::Text(Some("5".into()))
        );
    }

    #[test]
    fn enter_commits_and_steps_down_esc_does_not() {
        assert_eq!(post_edit(true, false, true), (true, true));
        assert_eq!(post_edit(false, true, false), (true, false));
        // Enter without a value (choice dismissed) keeps the row
        assert_eq!(post_edit(true, false, false), (true, false));
        // a click elsewhere: no focus grab, no step
        assert_eq!(post_edit(false, false, true), (false, false));
    }

    #[test]
    fn keys_wait_for_text_fields_dialogs_and_edits() {
        assert!(keys_enabled(true, true, false, true, false));
        assert!(!keys_enabled(false, true, false, true, false));
        assert!(!keys_enabled(true, false, false, true, false));
        assert!(!keys_enabled(true, true, true, true, false));
        assert!(!keys_enabled(true, true, false, false, false));
        assert!(!keys_enabled(true, true, false, true, true));
    }

    #[test]
    fn hint_expires() {
        assert!(!hint_live(None, 1.0));
        assert!(hint_live(Some(2.5), 1.0));
        assert!(!hint_live(Some(2.5), 2.5));
    }

    fn key(k: egui::Key, shift: bool, ctrl: bool) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                shift,
                ctrl,
                command: ctrl,
                ..Default::default()
            },
        }
    }

    fn read(events: Vec<egui::Event>) -> Option<Intent> {
        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| out = read_keys(ctx),
        );
        out
    }

    #[test]
    fn keys_map_to_intents() {
        use egui::Key;
        assert_eq!(
            read(vec![key(Key::Tab, false, false)]),
            Some(Intent::Nav(NavKey::Tab))
        );
        assert_eq!(
            read(vec![key(Key::Tab, true, false)]),
            Some(Intent::Nav(NavKey::BackTab))
        );
        assert_eq!(
            read(vec![key(Key::Home, false, true)]),
            Some(Intent::Nav(NavKey::Top))
        );
        assert_eq!(
            read(vec![key(Key::Home, false, false)]),
            Some(Intent::Nav(NavKey::Home))
        );
        assert_eq!(read(vec![key(Key::Enter, false, false)]), Some(Intent::Edit));
        assert_eq!(read(vec![key(Key::F2, false, false)]), Some(Intent::Edit));
        assert_eq!(read(vec![key(Key::Escape, false, false)]), None);
    }

    #[test]
    fn typing_starts_edit_but_whitespace_does_not() {
        assert_eq!(
            read(vec![egui::Event::Text("7".into())]),
            Some(Intent::Type("7".into()))
        );
        assert_eq!(read(vec![egui::Event::Text(" ".into())]), None);
        assert_eq!(read(vec![]), None);
    }
}
