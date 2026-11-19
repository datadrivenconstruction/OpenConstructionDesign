// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Keys window: command search, chord recording (Esc cancels, Backspace clears),
//! two-letter input, reset, conflict list, export and import of the keys file.
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::commands::Shortcut;
use ddc_ui::keymap::{
    chord_ok, chord_text, reserved_reason, Bindings, KeyConflict, Resolution, UserKeys, OS_CHORDS,
};
use ddc_ui::Registry;
use ddc_ui::i18n::Catalog;
use ddc_ui::notify::Level;
use ddc_ui::shortcuts2::SeqConflict;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, Button, TextField};
use ddc_ui::Theme;

/// Widest key chip, px; longer text is elided with a tooltip.
const CHORD_MAX_W: f32 = 176.0;
const SEQ_W: f32 = 44.0;

/// Chip look: the state tells whether the key is being recorded or clashes with another command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChipState {
    Idle,
    Recording,
    Warn,
}

/// One list row, resolved before painting.
struct Row {
    id: String,
    name: String,
    /// Muted context: the ribbon group of the command.
    cat: String,
    chords: String,
    seq: String,
    own: bool,
    /// Warning line naming the other command, when the row's key clashes.
    clash: Option<String>,
}

/// What a row asks for this frame.
#[derive(Default)]
struct RowOut {
    chord_clicked: bool,
    seq_clicked: bool,
    act: Option<Act>,
}

/// Text on the chord chip: the prompt while recording, a dash when unbound.
pub fn chord_label(cat: &Catalog, recording: bool, chords: &str) -> String {
    if recording {
        cat.t("quick.keys.press")
    } else if chords.is_empty() {
        "—".into()
    } else {
        chords.to_string()
    }
}

/// Second line of a row that clashes: names the other command (None when the row is not in
/// this conflict).
pub fn conflict_line(app: &EditorApp, id: &str, k: &KeyConflict) -> Option<String> {
    let c = &app.sh.cat;
    match k {
        KeyConflict::Chord { key, kept, dropped }
        | KeyConflict::Seq(SeqConflict::Duplicate {
            seq: key,
            kept,
            dropped,
        }) => {
            if kept == id {
                let o = super::label(app, dropped);
                Some(c.tf("quick.keys.row_took", &[("key", key), ("cmd", &o)]))
            } else if dropped == id {
                let o = super::label(app, kept);
                Some(c.tf("quick.keys.row_lost", &[("key", key), ("cmd", &o)]))
            } else {
                None
            }
        }
        KeyConflict::Reserved { cmd, .. }
        | KeyConflict::BadChord { cmd, .. }
        | KeyConflict::Seq(SeqConflict::Bad { cmd, .. })
            if cmd == id =>
        {
            Some(conflict_text(app, k))
        }
        _ => None,
    }
}

/// Muted category of a command: its ribbon group caption, else the id prefix.
fn category(cat: &Catalog, cmd: &ddc_ui::Command) -> String {
    let key = format!("rgroup.{}", widgets::ribbon::group_name(cmd));
    let t = cat.t(&key);
    if t == key {
        cmd.id.split('.').next().unwrap_or_default().to_string()
    } else {
        t
    }
}

/// Back to the default key of one command: one keymap change, one save.
pub fn reset_cmd(app: &mut EditorApp, id: &str) {
    app.quick.keys.reset(id);
    apply(app);
}

/// Key chip: rounded, right-aligned by the caller through `rect`.
fn chip(
    ui: &mut egui::Ui,
    th: &Theme,
    rect: egui::Rect,
    id: egui::Id,
    text: &str,
    state: ChipState,
    muted: bool,
) -> egui::Response {
    let s = th.sem();
    let resp = ui.interact(rect, id, egui::Sense::click());
    let (fill, line, ink) = match state {
        ChipState::Recording => (s.accent_subtle, s.accent, s.accent),
        ChipState::Warn => (s.warning_subtle, s.warning, s.text_primary),
        ChipState::Idle => {
            let fill = if resp.hovered() { s.bg_hover } else { s.bg_sunken };
            let ink = if muted { s.text_secondary } else { s.text_primary };
            (fill, s.border_subtle, ink)
        }
    };
    let r = th.radius("sm");
    ui.painter().rect_filled(rect, r, fill);
    ui.painter()
        .rect_stroke(rect, r, egui::Stroke::new(1.0, line), egui::StrokeKind::Inside);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            r,
            egui::Stroke::new(1.0, s.focus),
            egui::StrokeKind::Inside,
        );
    }
    widgets::text::paint_in(
        ui,
        rect.shrink2(egui::vec2(space::SM, 0.0)),
        text,
        th.font(Role::Label),
        ink,
        egui::Align::Center,
    );
    resp
}

/// Width of a chip for `text`: padded text, between a minimum and `max`.
fn chip_w(ui: &egui::Ui, th: &Theme, text: &str, min: f32, max: f32) -> f32 {
    let w = ui
        .painter()
        .layout_no_wrap(text.to_owned(), th.font(Role::Label), th.sem().text_primary)
        .size()
        .x;
    (w + 2.0 * space::SM).clamp(min, max)
}

/// One list row: command name and muted category on the left, key chips and the hover-only
/// reset button on the right.
fn key_row(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    r: &Row,
    recording: bool,
    seq_buf: Option<&mut String>,
) -> RowOut {
    let s = th.sem();
    let row_h = th.d().row;
    let mut out = RowOut::default();
    let (rect, row_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::hover(),
    );
    row_resp.on_hover_text(r.id.as_str());
    if !ui.is_rect_visible(rect) {
        return out;
    }
    let hot = ui.rect_contains_pointer(rect);
    if recording {
        ui.painter().rect_filled(rect, 0.0, s.accent_subtle);
    } else if hot {
        ui.painter().rect_filled(rect, 0.0, s.bg_hover);
    }
    let chip_h = th.d().control_sm.min(row_h - 2.0);
    let cy = rect.center().y;
    let mut x = rect.right() - space::SM;
    // reset: only for changed rows, drawn while the row is hot or recording
    let reset_side = chip_h;
    let reset_rect = egui::Rect::from_min_size(
        egui::pos2(x - reset_side, cy - reset_side / 2.0),
        egui::Vec2::splat(reset_side),
    );
    x = reset_rect.left() - space::XS;
    if r.own {
        let resp = ui.interact(reset_rect, ui.id().with(("keys.reset", &r.id)), egui::Sense::click());
        if hot || recording || resp.has_focus() {
            let tip = c.t("quick.keys.reset_tip");
            let col = widgets::paint_state(ui, th, reset_rect, &resp, false);
            let ic = egui::Rect::from_center_size(reset_rect.center(), egui::Vec2::splat(14.0));
            let ctx = ui.ctx().clone();
            widgets::with_icons(&ctx, |icons| icons.paint(ui, "undo", 14, ic, col));
            if widgets::rich_tooltip(resp.clone(), th, &tip, "", "").clicked() {
                out.act = Some(Act::Reset);
            }
        }
    }
    // two-letter key
    let seq_rect = egui::Rect::from_min_size(
        egui::pos2(x - SEQ_W, cy - chip_h / 2.0),
        egui::vec2(SEQ_W, chip_h),
    );
    x = seq_rect.left() - space::XS;
    if let Some(buf) = seq_buf {
        let eid = ui.id().with(("keys.seq", &r.id));
        let seen_id = eid.with("seen");
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(seq_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                let seq_lbl = c.t("quick.keys.seq_tip");
                let o = TextField::new(&mut *buf)
                    .id(eid)
                    .labelled_by(eid.with("lbl"), &seq_lbl)
                    .width(SEQ_W)
                    .show(ui, th);
                if buf.chars().count() > 2 {
                    *buf = buf.chars().take(2).collect();
                }
                let focused = ui.memory(|m| m.has_focus(eid));
                let seen = ui.data(|d| d.get_temp::<bool>(seen_id)).unwrap_or(false);
                if o.cancelled {
                    out.act = Some(Act::Cancel);
                } else if o.committed {
                    out.act = Some(Act::Seq(buf.clone()));
                } else if focused {
                    ui.data_mut(|d| d.insert_temp(seen_id, true));
                } else if seen {
                    out.act = Some(Act::Cancel);
                } else {
                    ui.memory_mut(|m| m.request_focus(eid));
                }
            },
        );
        if out.act.is_some() {
            ui.data_mut(|d| d.remove_temp::<bool>(seen_id));
        }
    } else {
        let t = if r.seq.is_empty() { "··" } else { r.seq.as_str() };
        let cid = ui.id().with(("keys.seqchip", &r.id));
        let resp = chip(
            ui,
            th,
            seq_rect,
            cid,
            t,
            ChipState::Idle,
            r.seq.is_empty(),
        );
        out.seq_clicked = widgets::rich_tooltip(resp, th, &c.t("quick.keys.seq_tip"), "", "")
            .clicked();
    }
    // chord chip
    let text = chord_label(c, recording, &r.chords);
    let w = chip_w(ui, th, &text, 56.0, CHORD_MAX_W.min((x - rect.left()) * 0.5).max(56.0));
    let chord_rect = egui::Rect::from_min_size(
        egui::pos2(x - w, cy - chip_h / 2.0),
        egui::vec2(w, chip_h),
    );
    let state = if recording {
        ChipState::Recording
    } else if r.clash.is_some() {
        ChipState::Warn
    } else {
        ChipState::Idle
    };
    let cid = ui.id().with(("keys.chord", &r.id));
    let resp = chip(
        ui,
        th,
        chord_rect,
        cid,
        &text,
        state,
        r.chords.is_empty() && !recording,
    );
    let tip = r.clash.clone().unwrap_or_else(|| c.t("quick.keys.capture_tip"));
    out.chord_clicked = resp.on_hover_text(tip).clicked();
    x = chord_rect.left() - space::MD;
    // name and muted line
    let left = rect.left() + space::SM;
    let area = egui::Rect::from_min_max(
        egui::pos2(left, rect.top()),
        egui::pos2(x.max(left + 1.0), rect.bottom()),
    );
    let name_w = (area.width() * 0.6).max(1.0);
    let name_rect = egui::Rect::from_min_size(area.min, egui::vec2(name_w, row_h));
    widgets::text::paint_in(
        ui,
        name_rect,
        &r.name,
        th.font(Role::Body),
        s.text_primary,
        egui::Align::Min,
    );
    let side = egui::Rect::from_min_max(
        egui::pos2(name_rect.right() + space::SM, rect.top()),
        egui::pos2(area.right(), rect.bottom()),
    );
    let (line, ink) = if recording {
        (c.t("quick.keys.rec_hint"), s.accent)
    } else if let Some(w) = &r.clash {
        (w.clone(), s.warning)
    } else {
        (r.cat.clone(), s.text_secondary)
    };
    if side.width() > 8.0 {
        widgets::text::paint_in(ui, side, &line, th.font(Role::Caption), ink, egui::Align::Min);
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct Ui {
    /// Command a chord is being recorded for.
    pub capture: Option<String>,
    pub filter: String,
    /// Two-letter input edits: command -> text.
    pub seq_edit: Option<(String, String)>,
    pub hint: Option<String>,
    /// Chord already used by another command: (command, chord, owner) awaiting Swap/Take/Keep.
    pub pending: Option<(String, String, String)>,
    /// Read key file awaiting Import/Cancel; nothing is changed while it waits.
    pub preview: Option<ImportPreview>,
    /// The keymap before the last applied import (one step back).
    pub undo: Option<ImportUndo>,
}

/// Command whose toast button takes back the last key import.
pub const UNDO_CMD: &str = "keys.undo_import";
/// Lines listed per preview section; the rest is counted.
const PREVIEW_LINES: usize = 8;

/// One binding an import would change: effective keys before and after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyChange {
    pub cmd: String,
    /// Two-letter key, not a chord: one command may change both in one file.
    pub seq: bool,
    pub from: String,
    pub to: String,
}

/// What importing a key file would do, computed before anything is written.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportPreview {
    pub file: String,
    /// The parsed file.
    pub incoming: UserKeys,
    /// Own keys the preview was computed from; Apply refuses a stale preview.
    pub base: UserKeys,
    /// Own keys after Apply: `base` with the file merged over it.
    pub next: UserKeys,
    /// Commands without own keys that the file binds differently.
    pub added: Vec<KeyChange>,
    /// The person's own bindings the file overwrites.
    pub changed: Vec<KeyChange>,
    /// Conflicts that appear only after the import.
    pub conflicts: Vec<KeyConflict>,
}

impl ImportPreview {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty()
    }
}

/// Keymap before and right after an applied import.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportUndo {
    pub before: UserKeys,
    pub after: UserKeys,
}

/// Preview of merging `incoming` over `cur`: only bindings whose effective keys change count.
pub fn preview_keys(reg: &Registry, cur: &UserKeys, file: &str, incoming: UserKeys) -> ImportPreview {
    let mut next = cur.clone();
    next.merge(incoming.clone());
    let before = Bindings::build(reg, cur);
    let after = Bindings::build(reg, &next);
    let (mut added, mut changed) = (vec![], vec![]);
    let chords = incoming.chords.keys().map(|c| (c, true));
    let seqs = incoming.seqs.keys().map(|c| (c, false));
    for (cmd, chord) in chords.chain(seqs) {
        // unknown commands surface as conflicts
        if reg.get(cmd).is_none() {
            continue;
        }
        let (from, to, own) = if chord {
            (
                before.chords_of(cmd).join(" / "),
                after.chords_of(cmd).join(" / "),
                cur.chords.contains_key(cmd),
            )
        } else {
            (
                before.seqs.seq_of(cmd).unwrap_or_default().to_string(),
                after.seqs.seq_of(cmd).unwrap_or_default().to_string(),
                cur.seqs.contains_key(cmd),
            )
        };
        if from == to {
            continue;
        }
        let ch = KeyChange {
            cmd: cmd.clone(),
            seq: !chord,
            from,
            to,
        };
        if own {
            changed.push(ch);
        } else {
            added.push(ch);
        }
    }
    let conflicts = after
        .conflicts
        .iter()
        .filter(|k| !before.conflicts.contains(k))
        .cloned()
        .collect();
    ImportPreview {
        file: file.to_string(),
        incoming,
        base: cur.clone(),
        next,
        added,
        changed,
        conflicts,
    }
}

/// "Command (chord | two letters): old → new" in the catalog's language; an unbound side
/// reads "none".
pub fn change_line(c: &Catalog, ch: &KeyChange) -> String {
    let none = c.t("keys_import.unbound");
    let side = |s: &str| if s.is_empty() { none.clone() } else { s.to_string() };
    let name = c.t(&format!("cmd.{}", ch.cmd));
    let kind = c.t(if ch.seq { "keys_import.kind_seq" } else { "keys_import.kind_chord" });
    c.tf(
        "keys_import.line",
        &[
            ("cmd", &name),
            ("kind", &kind),
            ("from", &side(&ch.from)),
            ("to", &side(&ch.to)),
        ],
    )
}

/// Texts of the preview card, resolved before painting.
#[derive(Clone, Debug, Default)]
pub struct PreviewCard {
    pub title: String,
    /// (heading, lines, warning look).
    pub sections: Vec<(String, Vec<String>, bool)>,
    pub empty: bool,
}

fn capped(c: &Catalog, mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > PREVIEW_LINES {
        let more = (lines.len() - PREVIEW_LINES).to_string();
        lines.truncate(PREVIEW_LINES);
        lines.push(c.tf("keys_import.more", &[("n", &more)]));
    }
    lines
}

/// Card texts in the language of the app's catalog.
pub fn preview_card(app: &EditorApp, p: &ImportPreview) -> PreviewCard {
    let c = &app.sh.cat;
    let title = c.tf(
        "keys_import.title",
        &[
            ("file", &p.file),
            ("new", &p.added.len().to_string()),
            ("changed", &p.changed.len().to_string()),
            ("conflicts", &p.conflicts.len().to_string()),
        ],
    );
    let mut sections = vec![];
    let mut add = |head: &str, lines: Vec<String>, warn: bool| {
        if !lines.is_empty() {
            sections.push((c.t(head), capped(c, lines), warn));
        }
    };
    add(
        "keys_import.changed_head",
        p.changed.iter().map(|ch| change_line(c, ch)).collect(),
        false,
    );
    add(
        "keys_import.new_head",
        p.added.iter().map(|ch| change_line(c, ch)).collect(),
        false,
    );
    add(
        "keys_import.conflicts_head",
        p.conflicts.iter().map(|k| conflict_text(app, k)).collect(),
        true,
    );
    PreviewCard {
        title,
        sections,
        empty: p.is_empty(),
    }
}

/// Conflict text for the list.
pub fn conflict_text(app: &EditorApp, k: &KeyConflict) -> String {
    let c = &app.sh.cat;
    let l = |id: &str| super::label(app, id);
    match k {
        KeyConflict::Chord { key, kept, dropped } => c.tf(
            "quick.keys.c_chord",
            &[("key", key), ("kept", &l(kept)), ("dropped", &l(dropped))],
        ),
        KeyConflict::Reserved { cmd, key } => {
            c.tf("quick.keys.c_reserved", &[("key", key), ("cmd", &l(cmd))])
        }
        KeyConflict::BadChord { cmd, key } => {
            c.tf("quick.keys.c_bad", &[("key", key), ("cmd", &l(cmd))])
        }
        KeyConflict::UnknownCommand(id) | KeyConflict::Seq(SeqConflict::UnknownCommand(id)) => {
            c.tf("quick.keys.c_unknown", &[("cmd", id)])
        }
        KeyConflict::Seq(SeqConflict::Duplicate { seq, kept, dropped }) => c.tf(
            "quick.keys.c_chord",
            &[("key", seq), ("kept", &l(kept)), ("dropped", &l(dropped))],
        ),
        KeyConflict::Seq(SeqConflict::Bad { cmd, seq }) => {
            c.tf("quick.keys.c_seq_bad", &[("key", seq), ("cmd", &l(cmd))])
        }
        KeyConflict::Seq(SeqConflict::BadFile(e)) => c.tf("quick.keys.c_file", &[("err", e)]),
    }
}

/// Save the keys and rebuild the bindings.
pub fn apply(app: &mut EditorApp) {
    super::rebuild_keys(app);
    if let Some(d) = super::dir() {
        if let Err(e) = app.quick.keys.save(&d) {
            let e = e.to_string();
            super::toast(app, Level::Warning, "quick.keys.save_err", &[("err", &e)]);
        }
    }
}

/// Record chord `chord` for command `cmd`: if another command holds it, warn (the user's own
/// chord wins; it is removed from the other command and added to the conflicts).
pub fn bind(app: &mut EditorApp, cmd: &str, chord: Option<&str>) {
    let other = chord.and_then(|ch| app.quick.bind.owner_of(ch, cmd).map(String::from));
    app.quick.keys.set_chord(cmd, chord);
    apply(app);
    if let (Some(o), Some(ch)) = (other, chord) {
        let name = super::label(app, &o);
        super::toast(
            app,
            Level::Warning,
            "quick.keys.taken",
            &[("key", ch), ("cmd", &name)],
        );
    }
}

/// Resolve a pending conflict: one keymap change, one save.
pub fn resolve_pending(app: &mut EditorApp, how: Resolution) {
    let Some((cmd, chord, other)) = app.quick.keys_ui.pending.take() else {
        return;
    };
    if how == Resolution::Keep {
        return;
    }
    let b = std::mem::take(&mut app.quick.bind);
    app.quick.keys.resolve(&b, &cmd, &other, &chord, how);
    app.quick.bind = b;
    apply(app);
}

/// Export only the user's own changes (not all default keys).
pub fn export_to(app: &EditorApp, path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, app.quick.keys.overrides(&app.sh.reg).to_json())
}

/// Read a key file into the preview; nothing changes until [`apply_import`]. An unreadable or
/// invalid file is refused with the reason and leaves keys and preview as they were. An opened
/// preview ends any recording, pending question or two-letter edit: capture() skips them while
/// the preview waits, so a recording started earlier would hang.
pub fn import_from(app: &mut EditorApp, path: &std::path::Path) -> Result<(), String> {
    let t = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let k = UserKeys::from_json(&t)?;
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ku = &mut app.quick.keys_ui;
    ku.capture = None;
    ku.pending = None;
    ku.seq_edit = None;
    ku.preview = Some(preview_keys(&app.sh.reg, &app.quick.keys, &file, k));
    ku.hint = None;
    Ok(())
}

/// Import the previewed file: the keymap is replaced in one step and the previous one kept for
/// [`undo_import`]. A preview made before a later key edit is recomputed instead (false).
pub fn apply_import(app: &mut EditorApp) -> bool {
    let Some(p) = app.quick.keys_ui.preview.take() else {
        return false;
    };
    if app.quick.keys != p.base {
        let fresh = preview_keys(&app.sh.reg, &app.quick.keys, &p.file, p.incoming);
        app.quick.keys_ui.preview = Some(fresh);
        app.quick.keys_ui.hint = Some(app.sh.cat.t("keys_import.stale"));
        return false;
    }
    if p.is_empty() {
        return false;
    }
    let before = std::mem::replace(&mut app.quick.keys, p.next);
    apply(app);
    app.quick.keys_ui.undo = Some(ImportUndo {
        before,
        after: app.quick.keys.clone(),
    });
    app.quick.keys_ui.hint = None;
    let n = p.conflicts.len();
    let level = if n > 0 { Level::Warning } else { Level::Success };
    let text = if n > 0 {
        app.sh.cat.tf("keys_import.imported_conflicts", &[("n", &n.to_string())])
    } else {
        app.sh.cat.t("keys_import.imported")
    };
    let undo = app.sh.cat.t("keys_import.undo");
    app.sh.toasts.push_cmd(level, text, undo, UNDO_CMD);
    true
}

/// Handler of [`UNDO_CMD`]: back to the keymap before the last import, exactly. Refused when
/// keys were edited after the import (those edits would be lost silently); the refusal keeps
/// the undo slot, so putting the keys back as imported makes Undo work again.
pub fn undo_import(app: &mut EditorApp) {
    let Some(after) = app.quick.keys_ui.undo.as_ref().map(|u| &u.after) else {
        super::toast(app, Level::Info, "keys_import.undo_none", &[]);
        return;
    };
    if app.quick.keys != *after {
        super::toast(app, Level::Warning, "keys_import.undo_later", &[]);
        return;
    }
    let Some(u) = app.quick.keys_ui.undo.take() else {
        return;
    };
    app.quick.keys = u.before;
    apply(app);
    super::toast(app, Level::Success, "keys_import.undone", &[]);
}

/// Chord capture: the first key press in this frame.
fn capture(app: &mut EditorApp, ctx: &egui::Context) {
    // no recording while an import waits: its preview would go stale
    if app.quick.keys_ui.preview.is_some() {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.quick.keys_ui.preview = None;
        }
        return;
    }
    if app.quick.keys_ui.pending.is_some() {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            resolve_pending(app, Resolution::Keep);
        }
        return;
    }
    let Some(cmd) = app.quick.keys_ui.capture.clone() else {
        return;
    };
    let ev = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => Some((*key, *modifiers)),
            _ => None,
        })
    });
    let Some((key, m)) = ev else { return };
    if m.is_none() && key == egui::Key::Escape {
        app.quick.keys_ui.capture = None;
        return;
    }
    if m.is_none() && matches!(key, egui::Key::Backspace | egui::Key::Delete) {
        app.quick.keys_ui.capture = None;
        bind(app, &cmd, None);
        return;
    }
    let sc = Shortcut {
        ctrl: m.command,
        shift: m.shift,
        alt: m.alt,
        key,
    };
    if !chord_ok(&sc) {
        app.quick.keys_ui.hint = Some(app.sh.cat.t("quick.keys.need_mod"));
        return;
    }
    if sc.os_reserved() {
        let t = chord_text(&sc);
        app.quick.keys_ui.hint = Some(app.sh.cat.tf("quick.keys.reserved", &[("key", &t)]));
        return;
    }
    let text = chord_text(&sc);
    let own_default = app
        .sh
        .reg
        .get(&cmd)
        .is_some_and(|c| c.parsed_shortcuts().contains(&sc));
    if let (Some(why), false) = (reserved_reason(&sc), own_default) {
        let why = app.sh.cat.t(&format!("quick.keys.res_{why}"));
        app.quick.keys_ui.hint = Some(
            app.sh
                .cat
                .tf("quick.keys.reserved_refused", &[("key", &text), ("why", &why)]),
        );
        return;
    }
    app.quick.keys_ui.capture = None;
    app.quick.keys_ui.hint = None;
    if let Some(o) = app.quick.bind.owner_of(&text, &cmd).map(String::from) {
        app.quick.keys_ui.pending = Some((cmd, text, o));
        return;
    }
    bind(app, &cmd, Some(&text));
}

fn file_dialog(save: bool) -> Option<std::path::PathBuf> {
    let d = crate::file_pick::Dlg::new()
        .add_filter("JSON", &["json"])
        .set_file_name("keymap.json");
    if save {
        d.save_file()
    } else {
        d.pick_file()
    }
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context) {
    capture(app, ctx);
    let mut open = app.quick.keys_open;
    let mut act: Option<(String, Act)> = None;
    let mut io: Option<bool> = None;
    let mut decide: Option<Resolution> = None;
    let mut import_go: Option<bool> = None;
    let card = app
        .quick
        .keys_ui
        .preview
        .as_ref()
        .map(|p| preview_card(app, p));
    let pending_owner = app
        .quick
        .keys_ui
        .pending
        .as_ref()
        .map(|p| super::label(app, &p.2))
        .unwrap_or_default();
    let conflicts: Vec<(Vec<String>, String)> = app
        .quick
        .bind
        .conflicts
        .iter()
        .map(|k| {
            (
                k.commands().iter().map(|s| s.to_string()).collect(),
                conflict_text(app, k),
            )
        })
        .collect();
    let f = app.quick.keys_ui.filter.to_lowercase();
    let rows: Vec<Row> = app
        .sh
        .reg
        .all()
        .iter()
        .filter(|cmd| cmd.alias.is_none())
        .map(|cmd| {
            let id = cmd.id.clone();
            let name = super::label(app, &id);
            let chords = app.quick.bind.chords_of(&id).join(" / ");
            let seq = app
                .quick
                .bind
                .seqs
                .seq_of(&id)
                .unwrap_or_default()
                .to_string();
            let own =
                app.quick.keys.chords.contains_key(&id) || app.quick.keys.seqs.contains_key(&id);
            let clash = app
                .quick
                .bind
                .conflicts
                .iter()
                .find_map(|k| conflict_line(app, &id, k));
            Row {
                cat: category(&app.sh.cat, cmd),
                id,
                name,
                chords,
                seq,
                own,
                clash,
            }
        })
        .filter(|r| {
            f.is_empty()
                || r.name.to_lowercase().contains(&f)
                || r.id.contains(&f)
                || r.chords.to_lowercase().contains(&f)
                || r.seq.to_lowercase() == f
        })
        .collect();
    let th = ddc_ui::theme::of(ctx);
    let th = &*th;
    let c = &app.sh.cat;
    let ku = &mut app.quick.keys_ui;
    kit::Dialog::new("shortcuts", c.t("quick.keys.title"), kit::DialogSize::Wide)
        .size([640.0, 520.0])
        .esc_closes(
            ku.capture.is_none()
                && ku.seq_edit.is_none()
                && ku.pending.is_none()
                && ku.preview.is_none(),
        )
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                let hint = c.t("quick.keys.filter");
                let w = (ui.available_width() - 2.0 * 110.0 - 2.0 * space::SM).max(160.0);
                TextField::new(&mut ku.filter)
                    .id_salt("keys.filter")
                    .hint(&hint)
                    .labelled_by(egui::Id::new("keys.filter.lbl"), &hint)
                    .width(w)
                    .show(ui, th);
                if Button::quiet(&c.t("quick.keys.export")).show(ui, th).clicked() {
                    io = Some(true);
                }
                if Button::quiet(&c.t("quick.keys.import")).show(ui, th).clicked() {
                    io = Some(false);
                }
            });
            if let Some(h) = &ku.hint {
                kit::warn_note(ui, h.clone());
            }
            if let Some((_, chord, _)) = &ku.pending {
                egui::Frame::new()
                    .fill(th.c("warning.subtle"))
                    .stroke(egui::Stroke::new(1.0, th.c("warning")))
                    .corner_radius(th.radius("md"))
                    .inner_margin(egui::Margin::same(space::SM as i8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let t = c.tf(
                            "quick.keys.reserved_conflict",
                            &[("key", chord), ("cmd", &pending_owner)],
                        );
                        ui.add(egui::Label::new(egui::RichText::new(t).color(th.c("text.primary"))).wrap());
                        ui.horizontal(|ui| {
                            if kit::primary_click(ui, c.t("quick.keys.reserved_swap"), None) {
                                decide = Some(Resolution::Swap);
                            }
                            if kit::secondary_button(ui, c.t("quick.keys.reserved_take")).clicked() {
                                decide = Some(Resolution::Take);
                            }
                            if kit::cancel_button(ui, c.t("quick.keys.reserved_keep")).clicked() {
                                decide = Some(Resolution::Keep);
                            }
                        });
                    });
            }
            if let Some(card) = &card {
                let s = th.sem();
                let warn = card.sections.iter().any(|x| x.2);
                let (fill, line) = if warn {
                    (s.warning_subtle, s.warning)
                } else {
                    (s.accent_subtle, s.accent)
                };
                egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, line))
                    .corner_radius(th.radius("md"))
                    .inner_margin(egui::Margin::same(space::SM as i8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let t = egui::RichText::new(&card.title).color(s.text_primary).strong();
                        ui.add(egui::Label::new(t).wrap());
                        if card.empty {
                            ui.weak(c.t("keys_import.none"));
                        }
                        for (head, lines, warn) in &card.sections {
                            ui.add_space(space::XS);
                            ui.label(egui::RichText::new(head).color(s.text_secondary));
                            for l in lines {
                                if *warn {
                                    kit::warn_note(ui, l.clone());
                                } else {
                                    kit::elided(ui, l);
                                }
                            }
                        }
                        ui.add_space(space::XS);
                        ui.horizontal(|ui| {
                            let tip = c.t("keys_import.nothing_tip");
                            let blocked = card.empty.then_some(tip.as_str());
                            if kit::primary_click(ui, c.t("keys_import.apply"), blocked) {
                                import_go = Some(true);
                            }
                            if kit::cancel_button(ui, c.t("keys_import.cancel")).clicked() {
                                import_go = Some(false);
                            }
                        });
                    });
            }
            ui.horizontal_wrapped(|ui| {
                ui.weak(c.t("quick.keys.reserved_os"));
                for k in OS_CHORDS {
                    ui.label(egui::RichText::new(k).strikethrough().weak());
                }
            });
            if !conflicts.is_empty() {
                let title = c.tf(
                    "quick.keys.conflicts",
                    &[("n", &conflicts.len().to_string())],
                );
                if kit::heading_keyed(ui, "keys.conflicts", &title, None) {
                    for (_, t) in &conflicts {
                        kit::warn_note(ui, t.clone());
                    }
                }
            }
            ui.separator();
            if rows.is_empty() {
                kit::empty_note(ui, c.t("dlg.keys.none"), Some(&c.t("dlg.keys.none_hint")));
            }
            kit::no_gap(ui, kit::Gap::Rows);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, th.d().row, rows.len(), |ui, range| {
                    for r in &rows[range] {
                        let rec = ku.capture.as_deref() == Some(r.id.as_str());
                        let editing = ku.seq_edit.as_ref().is_some_and(|(e, _)| *e == r.id);
                        let buf = if editing {
                            ku.seq_edit.as_mut().map(|(_, b)| b)
                        } else {
                            None
                        };
                        let o = key_row(ui, th, c, r, rec, buf);
                        // a waiting import preview owns the keys until Import or Cancel
                        let free = ku.preview.is_none();
                        if o.chord_clicked && free {
                            ku.capture = if rec { None } else { Some(r.id.clone()) };
                            ku.hint = None;
                        }
                        if o.seq_clicked && free {
                            ku.seq_edit = Some((r.id.clone(), r.seq.clone()));
                        }
                        if let Some(a) = o.act {
                            act = Some((r.id.clone(), a));
                        }
                    }
                });
        });
    app.quick.keys_open = open;
    if !open {
        app.quick.keys_ui.capture = None;
        app.quick.keys_ui.pending = None;
        app.quick.keys_ui.preview = None;
    }
    if let Some(how) = decide {
        resolve_pending(app, how);
    }
    match import_go {
        Some(true) => {
            apply_import(app);
        }
        Some(false) => app.quick.keys_ui.preview = None,
        None => {}
    }
    match act {
        Some((id, Act::Seq(s))) => {
            app.quick.keys_ui.seq_edit = None;
            app.quick.keys.set_seq(&id, &s);
            apply(app);
        }
        Some((id, Act::Reset)) => {
            reset_cmd(app, &id);
        }
        Some((_, Act::Cancel)) => app.quick.keys_ui.seq_edit = None,
        None => {}
    }
    match io {
        Some(true) => {
            if let Some(p) = file_dialog(true) {
                match export_to(app, &p) {
                    Ok(()) => {
                        let s = p.display().to_string();
                        super::toast(app, Level::Success, "quick.keys.exported", &[("path", &s)]);
                    }
                    Err(e) => {
                        let e = e.to_string();
                        super::toast(app, Level::Warning, "quick.keys.io_err", &[("err", &e)]);
                    }
                }
            }
        }
        Some(false) => {
            if let Some(p) = file_dialog(false) {
                if let Err(e) = import_from(app, &p) {
                    super::toast(app, Level::Warning, "keys_import.refused", &[("err", &e)]);
                }
            }
        }
        None => {}
    }
}

enum Act {
    Seq(String),
    Reset,
    /// Esc in the two-letter field: no change.
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebinding_conflicts_and_file_round_trip() {
        let mut a = EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        );
        let k = egui::Key::K;
        let cmd = egui::Modifiers::COMMAND;
        assert_eq!(
            super::super::chord(&a, k, cmd).as_deref(),
            Some("app.palette")
        );
        // the user's chord takes over another's: it is removed from the palette and shown in conflicts
        bind(&mut a, "app.history", Some("Ctrl+K"));
        assert_eq!(
            super::super::chord(&a, k, cmd).as_deref(),
            Some("app.history")
        );
        assert!(a.quick.bind.conflicts.iter().any(|c| matches!(c,
            KeyConflict::Chord { kept, dropped, .. } if kept == "app.history" && dropped == "app.palette")));
        let k0 = a
            .quick
            .bind
            .conflicts
            .iter()
            .find(|c| matches!(c, KeyConflict::Chord { .. }))
            .unwrap();
        let txt = conflict_text(&a, k0);
        assert!(txt.contains("Ctrl+K"), "{txt}");
        // commands do not launch while a chord is being recorded
        a.quick.keys_ui.capture = Some("app.history".into());
        assert_eq!(super::super::chord(&a, k, cmd), None);
        a.quick.keys_ui.capture = None;
        // two-letter input goes to the shell
        a.quick.keys.set_seq("app.history", "QH");
        apply(&mut a);
        assert_eq!(a.keys2.command("QH"), Some("app.history"));
        // export -> reset -> import restores the same state
        let p = std::env::temp_dir().join(format!("ddc-keys-{}.json", std::process::id()));
        export_to(&a, &p).unwrap();
        a.quick.keys.reset("app.history");
        apply(&mut a);
        assert_eq!(
            super::super::chord(&a, k, cmd).as_deref(),
            Some("app.palette")
        );
        import_from(&mut a, &p).unwrap();
        // the file is only previewed until Import
        assert_eq!(
            super::super::chord(&a, k, cmd).as_deref(),
            Some("app.palette")
        );
        assert!(apply_import(&mut a));
        assert_eq!(
            super::super::chord(&a, k, cmd).as_deref(),
            Some("app.history")
        );
        assert_eq!(a.keys2.command("QH"), Some("app.history"));
        assert!(import_from(&mut a, std::path::Path::new("missing.json")).is_err());
        // the file holds only the user's changes, not all default keys
        let own = UserKeys::from_json(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(own.chords.keys().collect::<Vec<_>>(), ["app.history"]);
        assert_eq!(own.seqs.keys().collect::<Vec<_>>(), ["app.history"]);
        // two Cyrillic letters are matched by key position
        a.quick.keys.set_seq("app.history", "\u{439}\u{440}");
        assert_eq!(a.quick.keys.seqs["app.history"], "QH");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn pending_resolution_applies_once() {
        let mut a = EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        );
        let k = egui::Key::K;
        let cmd = egui::Modifiers::COMMAND;
        a.quick.keys_ui.pending = Some(("app.history".into(), "Ctrl+K".into(), "app.palette".into()));
        resolve_pending(&mut a, Resolution::Keep);
        assert_eq!(super::super::chord(&a, k, cmd).as_deref(), Some("app.palette"));
        assert!(a.quick.keys_ui.pending.is_none());
        a.quick.keys_ui.pending = Some(("app.history".into(), "Ctrl+K".into(), "app.palette".into()));
        resolve_pending(&mut a, Resolution::Take);
        assert_eq!(super::super::chord(&a, k, cmd).as_deref(), Some("app.history"));
        assert!(a.quick.bind.conflicts.is_empty());
    }

    fn test_app() -> EditorApp {
        EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        )
    }

    /// Context with the named font weights (labels, chips): a bare one panics.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// One frame of the window; returns the number of painted shapes.
    fn frame(ctx: &egui::Context, a: &mut EditorApp, events: Vec<egui::Event>) -> usize {
        ctx.run(input(events), |ctx| ui(a, ctx)).shapes.len()
    }

    fn saved(a: &EditorApp) -> String {
        a.quick.keys.overrides(&a.sh.reg).to_json()
    }

    const FREE: &str = "Ctrl+Alt+J";

    #[test]
    fn rebinding_saves_the_same_binding_data() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        assert_eq!(a.quick.keys.chords["app.history"], vec![FREE.to_string()]);
        assert_eq!(a.quick.bind.chords_of("app.history"), [FREE.to_string()]);
        let own = UserKeys::from_json(&saved(&a)).unwrap();
        assert_eq!(own.chords["app.history"], vec![FREE.to_string()]);
        assert_eq!(own.chords.len(), 1);
    }

    #[test]
    fn taken_chord_waits_for_a_decision() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        let before = saved(&a);
        let ctx = font_ctx();
        a.quick.keys_ui.capture = Some("app.palette".into());
        let m = egui::Modifiers {
            ctrl: true,
            alt: true,
            command: true,
            ..Default::default()
        };
        ctx.run(input(vec![key_event(egui::Key::J, m)]), |ctx| capture(&mut a, ctx));
        let p = a.quick.keys_ui.pending.clone().expect("conflict is pending");
        assert_eq!((p.0.as_str(), p.2.as_str()), ("app.palette", "app.history"));
        assert!(a.quick.keys_ui.capture.is_none());
        // nothing is saved until the user decides
        assert_eq!(saved(&a), before);
        assert!(!a.quick.keys.chords.contains_key("app.palette"));
        resolve_pending(&mut a, Resolution::Keep);
        assert_eq!(saved(&a), before);
        assert!(a.quick.keys_ui.pending.is_none());
    }

    #[test]
    fn conflict_line_names_the_other_command() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some("Ctrl+K"));
        let all = std::mem::take(&mut a.quick.bind.conflicts);
        let k = all
            .iter()
            .find(|c| matches!(c, KeyConflict::Chord { .. }))
            .expect("Ctrl+K clashes");
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            a.sh.cat.lang = lang;
            let palette = super::super::label(&a, "app.palette");
            let history = super::super::label(&a, "app.history");
            let kept = conflict_line(&a, "app.history", k).unwrap();
            let lost = conflict_line(&a, "app.palette", k).unwrap();
            assert!(kept.contains(&palette) && kept.contains("Ctrl+K"), "{kept}");
            assert!(lost.contains(&history) && lost.contains("Ctrl+K"), "{lost}");
            assert_eq!(conflict_line(&a, "view.zoom_fit", k), None);
        }
    }

    #[test]
    fn reset_restores_the_default() {
        let mut a = test_app();
        let default = a.quick.bind.chords_of("app.history").to_vec();
        let clean = saved(&a);
        bind(&mut a, "app.history", Some(FREE));
        assert_ne!(a.quick.bind.chords_of("app.history"), default.as_slice());
        reset_cmd(&mut a, "app.history");
        assert_eq!(a.quick.bind.chords_of("app.history"), default.as_slice());
        assert!(!a.quick.keys.chords.contains_key("app.history"));
        assert_eq!(saved(&a), clean);
    }

    #[test]
    fn idle_frame_writes_nothing() {
        let mut a = test_app();
        a.quick.keys_open = true;
        let ctx = font_ctx();
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            a.sh.cat.lang = lang;
            frame(&ctx, &mut a, vec![]);
            let (keys, ui_state) = (saved(&a), format!("{:?}", a.quick.keys_ui));
            let (own, conflicts) = (a.quick.keys.clone(), a.quick.bind.conflicts.len());
            let toasts = a.sh.toasts.live.len();
            frame(&ctx, &mut a, vec![]);
            frame(&ctx, &mut a, vec![]);
            assert_eq!(saved(&a), keys);
            assert_eq!(format!("{:?}", a.quick.keys_ui), ui_state);
            assert_eq!(a.quick.keys.chords, own.chords);
            assert_eq!(a.quick.keys.seqs, own.seqs);
            assert_eq!(a.quick.bind.conflicts.len(), conflicts);
            assert_eq!(a.sh.toasts.live.len(), toasts);
        }
    }

    #[test]
    fn renders_in_both_languages_and_states() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let mut a = test_app();
            a.sh.cat.lang = lang;
            a.quick.keys_open = true;
            let ctx = font_ctx();
            // fonts and the theme bind on the pass after they are installed
            frame(&ctx, &mut a, vec![]);
            assert!(frame(&ctx, &mut a, vec![]) > 0);
            // conflict, recording row, pending question, two-letter edit, search text
            bind(&mut a, "app.history", Some("Ctrl+K"));
            a.quick.keys_ui.capture = Some("app.palette".into());
            assert!(frame(&ctx, &mut a, vec![]) > 0);
            a.quick.keys_ui.capture = None;
            a.quick.keys_ui.pending =
                Some(("app.palette".into(), "Ctrl+K".into(), "app.history".into()));
            assert!(frame(&ctx, &mut a, vec![]) > 0);
            a.quick.keys_ui.pending = None;
            a.quick.keys_ui.seq_edit = Some(("app.history".into(), "QH".into()));
            frame(&ctx, &mut a, vec![]);
            frame(&ctx, &mut a, vec![]);
            a.quick.keys_ui.seq_edit = None;
            a.quick.keys_ui.filter = "zzzz-no-such-command".into();
            assert!(frame(&ctx, &mut a, vec![]) > 0);
            // the recording prompt differs from the idle chip in each language
            assert_ne!(
                chord_label(&a.sh.cat, true, "Ctrl+K"),
                chord_label(&a.sh.cat, false, "Ctrl+K")
            );
            assert_eq!(chord_label(&a.sh.cat, false, ""), "—");
        }
    }

    #[test]
    fn window_uses_only_kit_widgets() {
        let src = include_str!("keys_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for raw in [
            "ui.button(",
            "small_button(",
            "TextEdit::",
            "egui::Grid",
            "selectable_label",
            "colored_label",
            "CollapsingHeader",
            "ui.checkbox(",
        ] {
            assert!(!code.contains(raw), "raw widget {raw}");
        }
    }

    #[test]
    fn new_strings_are_paired_and_valid() {
        let en = include_str!("../../../ui/data/i18n/en/quick_keys.json");
        let ru = include_str!("../../../ui/data/i18n/ru/quick_keys.json");
        let en: std::collections::BTreeMap<String, String> = serde_json::from_str(en).unwrap();
        let ru: std::collections::BTreeMap<String, String> = serde_json::from_str(ru).unwrap();
        assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
        assert!(en.values().chain(ru.values()).all(|v| !v.is_empty()));
    }

    fn key_file(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ddc-xs57-{}-{name}.json", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn file_of(k: &UserKeys) -> String {
        k.to_json()
    }

    fn one_chord(cmd: &str, keys: &[&str]) -> UserKeys {
        let mut k = UserKeys::default();
        k.chords
            .insert(cmd.into(), keys.iter().map(|s| s.to_string()).collect());
        k
    }

    #[test]
    fn untouched_import_has_zero_changes() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        a.quick.keys.set_seq("app.history", "QH");
        apply(&mut a);
        let (before, keys) = (saved(&a), a.quick.keys.clone());
        let own = key_file("same", &saved(&a));
        let empty = key_file("empty", "{}");
        for p in [&own, &empty] {
            import_from(&mut a, p).unwrap();
            let pv = a.quick.keys_ui.preview.clone().expect("previewed");
            assert!(pv.is_empty(), "{pv:?}");
            assert!(pv.conflicts.is_empty(), "{:?}", pv.conflicts);
            assert!(!apply_import(&mut a));
            assert_eq!(saved(&a), before);
            assert_eq!(a.quick.keys, keys);
            assert!(a.quick.keys_ui.undo.is_none());
        }
        let _ = std::fs::remove_file(own);
        let _ = std::fs::remove_file(empty);
    }

    #[test]
    fn preview_lists_an_overwritten_own_binding_and_cancel_keeps_it() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        let before = saved(&a);
        let mut file = one_chord("app.history", &[]);
        file.seqs.insert("app.history".into(), "QH".into());
        let p = key_file("own", &file_of(&file));
        import_from(&mut a, &p).unwrap();
        let pv = a.quick.keys_ui.preview.clone().unwrap();
        assert_eq!(
            pv.changed,
            [KeyChange {
                cmd: "app.history".into(),
                seq: false,
                from: FREE.into(),
                to: String::new(),
            }]
        );
        assert_eq!(
            pv.added,
            [KeyChange {
                cmd: "app.history".into(),
                seq: true,
                from: String::new(),
                to: "QH".into(),
            }]
        );
        // nothing written while the preview waits
        assert_eq!(saved(&a), before);
        assert_eq!(a.quick.bind.chords_of("app.history"), [FREE.to_string()]);
        // Esc cancels: the own binding stays
        let ctx = font_ctx();
        ctx.run(input(vec![key_event(egui::Key::Escape, Default::default())]), |ctx| {
            capture(&mut a, ctx)
        });
        assert!(a.quick.keys_ui.preview.is_none());
        assert_eq!(saved(&a), before);
        assert_eq!(a.quick.keys.chords["app.history"], vec![FREE.to_string()]);
        assert_eq!(a.keys2.command("QH"), None);
        assert!(a.quick.keys_ui.undo.is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn conflict_is_previewed_and_warned() {
        let mut a = test_app();
        let p = key_file("clash", &file_of(&one_chord("app.history", &["Ctrl+K"])));
        import_from(&mut a, &p).unwrap();
        let pv = a.quick.keys_ui.preview.clone().unwrap();
        assert_eq!(pv.added.len(), 1);
        assert_eq!(pv.added[0].to, "Ctrl+K");
        assert!(pv.changed.is_empty());
        assert!(pv.conflicts.iter().any(|c| matches!(c,
            KeyConflict::Chord { kept, dropped, .. } if kept == "app.history" && dropped == "app.palette")));
        let card = preview_card(&a, &pv);
        assert!(card.sections.iter().any(|s| s.2 && !s.1.is_empty()));
        assert!(apply_import(&mut a));
        assert_eq!(
            super::super::chord(&a, egui::Key::K, egui::Modifiers::COMMAND).as_deref(),
            Some("app.history")
        );
        let t = a.sh.toasts.live.last().expect("import toast");
        assert_eq!(t.level, Level::Warning);
        assert_eq!(t.command.as_deref(), Some(UNDO_CMD));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn invalid_file_is_refused_and_changes_nothing() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        let ok = key_file("ok", &file_of(&one_chord("app.history", &["Ctrl+K"])));
        import_from(&mut a, &ok).unwrap();
        let (before, keys) = (saved(&a), a.quick.keys.clone());
        let waiting = a.quick.keys_ui.preview.clone();
        let bad = [
            ("text", "not json at all"),
            ("array", "[1, 2]"),
            ("newer", "{\"version\": 99}"),
            ("shape", "{\"chords\": 5}"),
        ];
        for (name, body) in bad {
            let p = key_file(name, body);
            let e = import_from(&mut a, &p).expect_err(name);
            assert!(!e.trim().is_empty(), "{name}: reason given");
            assert_eq!(saved(&a), before, "{name}");
            assert_eq!(a.quick.keys, keys, "{name}");
            assert_eq!(a.quick.keys_ui.preview, waiting, "{name}");
            let _ = std::fs::remove_file(p);
        }
        let missing = std::env::temp_dir().join("ddc-xs57-no-such-file.json");
        assert!(import_from(&mut a, &missing).is_err());
        assert_eq!(saved(&a), before);
        let _ = std::fs::remove_file(ok);
    }

    #[test]
    fn undo_import_restores_the_previous_keymap_exactly() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        a.quick.keys.set_seq("app.history", "QH");
        apply(&mut a);
        let (before, keys) = (saved(&a), a.quick.keys.clone());
        let chords = a.quick.bind.chords_of("app.history").to_vec();
        let mut file = one_chord("app.history", &["Ctrl+K"]);
        file.seqs.insert("app.history".into(), String::new());
        let p = key_file("undo", &file_of(&file));
        import_from(&mut a, &p).unwrap();
        assert!(apply_import(&mut a));
        assert_ne!(saved(&a), before);
        undo_import(&mut a);
        assert_eq!(a.quick.keys, keys);
        assert_eq!(saved(&a), before);
        assert_eq!(a.quick.bind.chords_of("app.history"), chords.as_slice());
        assert_eq!(a.keys2.command("QH"), Some("app.history"));
        assert!(a.quick.keys_ui.undo.is_none());
        // a second undo has nothing to take back
        undo_import(&mut a);
        assert_eq!(a.quick.keys, keys);
        // an edit after the import is never thrown away by undo
        import_from(&mut a, &p).unwrap();
        assert!(apply_import(&mut a));
        bind(&mut a, "app.palette", Some(FREE));
        let edited = a.quick.keys.clone();
        undo_import(&mut a);
        assert_eq!(a.quick.keys, edited);
        assert_eq!(a.sh.toasts.live.last().unwrap().level, Level::Warning);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn stale_preview_is_recomputed_not_applied() {
        let mut a = test_app();
        let p = key_file("stale", &file_of(&one_chord("app.history", &["Ctrl+K"])));
        import_from(&mut a, &p).unwrap();
        bind(&mut a, "app.history", Some(FREE));
        let keys = a.quick.keys.clone();
        assert!(!apply_import(&mut a));
        assert_eq!(a.quick.keys, keys);
        let pv = a.quick.keys_ui.preview.clone().expect("recomputed");
        assert_eq!(pv.base, keys);
        assert_eq!(pv.changed.len(), 1);
        assert!(a.quick.keys_ui.hint.is_some());
        assert!(apply_import(&mut a));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn preview_text_follows_the_given_language() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        let p = key_file("loc", &file_of(&one_chord("app.history", &[])));
        import_from(&mut a, &p).unwrap();
        let pv = a.quick.keys_ui.preview.clone().unwrap();
        a.quick.keys_open = true;
        let ctx = font_ctx();
        let mut seen = vec![];
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            a.sh.cat.lang = lang;
            let card = preview_card(&a, &pv);
            assert!(card.title.contains(&pv.file), "{}", card.title);
            let line = change_line(&a.sh.cat, &pv.changed[0]);
            let name = super::super::label(&a, "app.history");
            assert!(line.contains(&name) && line.contains(FREE), "{line}");
            assert!(line.contains(&a.sh.cat.t("keys_import.unbound")), "{line}");
            assert!(!card.empty);
            // the card paints and an idle frame changes nothing
            let before = saved(&a);
            frame(&ctx, &mut a, vec![]);
            assert!(frame(&ctx, &mut a, vec![]) > 0);
            assert_eq!(saved(&a), before);
            assert!(a.quick.keys_ui.preview.is_some());
            seen.push((card.title, line));
        }
        assert_ne!(seen[0], seen[1]);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn import_ends_a_recording_started_before_it() {
        let mut a = test_app();
        let before = saved(&a);
        a.quick.keys_ui.capture = Some("app.history".into());
        a.quick.keys_ui.seq_edit = Some(("app.history".into(), "Q".into()));
        a.quick.keys_ui.pending =
            Some(("app.history".into(), "Ctrl+K".into(), "app.palette".into()));
        let p = key_file("rec", &file_of(&one_chord("app.palette", &[FREE])));
        import_from(&mut a, &p).unwrap();
        assert!(a.quick.keys_ui.preview.is_some());
        assert!(a.quick.keys_ui.capture.is_none());
        assert!(a.quick.keys_ui.seq_edit.is_none());
        assert!(a.quick.keys_ui.pending.is_none());
        // after Cancel a key press records nothing: the old recording is gone
        let ctx = font_ctx();
        let m = egui::Modifiers::COMMAND | egui::Modifiers::ALT;
        let esc = key_event(egui::Key::Escape, Default::default());
        ctx.run(input(vec![esc]), |ctx| capture(&mut a, ctx));
        assert!(a.quick.keys_ui.preview.is_none());
        ctx.run(input(vec![key_event(egui::Key::J, m)]), |ctx| capture(&mut a, ctx));
        assert_eq!(saved(&a), before);
        assert!(a.quick.keys_ui.pending.is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_failed_import_keeps_the_recording() {
        let mut a = test_app();
        a.quick.keys_ui.capture = Some("app.history".into());
        let p = key_file("rec-bad", "not json at all");
        assert!(import_from(&mut a, &p).is_err());
        assert_eq!(a.quick.keys_ui.capture.as_deref(), Some("app.history"));
        assert!(a.quick.keys_ui.preview.is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn refused_undo_keeps_the_slot_and_restores_exactly_later() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        a.quick.keys.set_seq("app.history", "QH");
        apply(&mut a);
        let (before, keys) = (saved(&a), a.quick.keys.clone());
        let p = key_file("undo-slot", &file_of(&one_chord("app.history", &["Ctrl+K"])));
        import_from(&mut a, &p).unwrap();
        assert!(apply_import(&mut a));
        let imported = a.quick.keys.clone();
        bind(&mut a, "app.palette", Some("Ctrl+Alt+M"));
        undo_import(&mut a);
        assert_eq!(a.sh.toasts.live.last().unwrap().level, Level::Warning);
        assert!(a.quick.keys_ui.undo.is_some(), "a refusal must not use up the undo");
        // the edit taken back by hand: Undo import works again, to the exact old keymap
        a.quick.keys = imported;
        apply(&mut a);
        undo_import(&mut a);
        assert_eq!(a.sh.toasts.live.last().unwrap().level, Level::Success);
        assert_eq!(a.quick.keys, keys);
        assert_eq!(saved(&a), before);
        assert_eq!(a.keys2.command("QH"), Some("app.history"));
        assert!(a.quick.keys_ui.undo.is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn preview_rows_tell_chord_from_two_letters() {
        let mut a = test_app();
        bind(&mut a, "app.history", Some(FREE));
        let mut file = one_chord("app.history", &[]);
        file.seqs.insert("app.history".into(), "QH".into());
        let p = key_file("kind", &file_of(&file));
        import_from(&mut a, &p).unwrap();
        let pv = a.quick.keys_ui.preview.clone().unwrap();
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            a.sh.cat.lang = lang;
            let c = &a.sh.cat;
            let (chord, seq) = (c.t("keys_import.kind_chord"), c.t("keys_import.kind_seq"));
            assert_ne!(chord, seq);
            let removed = change_line(c, &pv.changed[0]);
            let added = change_line(c, &pv.added[0]);
            assert!(removed.contains(&chord) && !removed.contains(&seq), "{removed}");
            assert!(added.contains(&seq) && added.contains("QH"), "{added}");
            let card = preview_card(&a, &pv);
            assert!(card.sections.iter().any(|s| s.1.contains(&removed)));
            assert!(card.sections.iter().any(|s| s.1.contains(&added)));
        }
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn import_toast_uses_its_own_strings() {
        let mut a = test_app();
        a.sh.cat.lang = ddc_ui::Lang::En;
        let quiet = key_file("toast-ok", &file_of(&one_chord("app.history", &[FREE])));
        import_from(&mut a, &quiet).unwrap();
        assert!(apply_import(&mut a));
        let t = a.sh.toasts.live.last().unwrap().clone();
        assert_eq!(t.level, Level::Success);
        assert_eq!(t.text, a.sh.cat.t("keys_import.imported"));
        assert_eq!(t.action.as_deref(), Some(a.sh.cat.t("keys_import.undo").as_str()));
        a.sh.cat.lang = ddc_ui::Lang::Ru;
        let clash = key_file("toast-clash", &file_of(&one_chord("app.history", &["Ctrl+K"])));
        import_from(&mut a, &clash).unwrap();
        let n = a.quick.keys_ui.preview.as_ref().unwrap().conflicts.len();
        assert!(n > 0);
        assert!(apply_import(&mut a));
        let t = a.sh.toasts.live.last().unwrap().clone();
        let want = a.sh.cat.tf("keys_import.imported_conflicts", &[("n", &n.to_string())]);
        assert_eq!((t.level, t.text), (Level::Warning, want));
        let _ = std::fs::remove_file(quiet);
        let _ = std::fs::remove_file(clash);
    }

    #[test]
    fn xs57_strings_are_paired_and_valid() {
        let en = include_str!("../../../ui/data/i18n/en/keys_import.json");
        let ru = include_str!("../../../ui/data/i18n/ru/keys_import.json");
        let en: std::collections::BTreeMap<String, String> = serde_json::from_str(en).unwrap();
        let ru: std::collections::BTreeMap<String, String> = serde_json::from_str(ru).unwrap();
        assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
        assert!(en.values().chain(ru.values()).all(|v| !v.is_empty()));
    }
}
