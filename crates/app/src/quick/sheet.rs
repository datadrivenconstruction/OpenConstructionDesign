// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Keyboard shortcut sheet: a centred card (`?` or Help > Shortcut reference) built from the
//! command registry and the effective bindings, so rebinding shows at once. Sections by
//! context, type-to-filter, Esc or a click outside closes, "Customize" opens the key editor.
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::icons::IconCache;
use ddc_ui::keymap::Bindings;
use ddc_ui::theme::{self, icon_size, layout, space, Role};
use ddc_ui::widgets::{self, Button, TextField};
use ddc_ui::{Registry, Theme};
use std::collections::HashSet;

pub const COMMAND: &str = "help.shortcuts";
const FILTER_ID: &str = "ddc.quick.sheet.filter";
/// Narrowest column that still fits a Russian label and a two-key chord.
const COL_W: f32 = 300.0;
const COLS_MAX: usize = 3;

#[derive(Clone, Debug, Default)]
pub struct Ui {
    pub open: bool,
    pub filter: String,
    /// Put the caret into the filter on the next frame.
    pub focus: bool,
}

/// Context a command belongs to; the order of `ORDER` is the order on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    General,
    Plan,
    View3d,
    Schedule,
    Sheets,
    Ai,
}

pub const ORDER: [Section; 6] = [
    Section::General,
    Section::Plan,
    Section::View3d,
    Section::Schedule,
    Section::Sheets,
    Section::Ai,
];

impl Section {
    pub fn title_key(self) -> &'static str {
        match self {
            Section::General => "sheet.sec.general",
            Section::Plan => "sheet.sec.plan",
            Section::View3d => "sheet.sec.view3d",
            Section::Schedule => "sheet.sec.schedule",
            Section::Sheets => "sheet.sec.sheets",
            Section::Ai => "sheet.sec.ai",
        }
    }
}

/// Context of a command from its id and ribbon tab.
pub fn section_of(c: &ddc_ui::Command) -> Section {
    let id = c.id.as_str();
    if id.starts_with("ai.") {
        Section::Ai
    } else if id.contains("sheet") || id.contains("legend") || id.contains("revision") {
        Section::Sheets
    } else if id.contains("sched") {
        Section::Schedule
    } else if id == "view.3d"
        || id.starts_with("view.style")
        || id.starts_with("view.gfx")
        || id.starts_with("view.section")
        || id.starts_with("view.walk")
        || id.starts_with("e3d.")
        || id.starts_with("mass.")
    {
        Section::View3d
    } else {
        match c.tab.as_deref() {
            Some("arch" | "struct" | "systems" | "annotate" | "modify") => Section::Plan,
            _ => Section::General,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub icon: String,
    pub label: String,
    /// Alternatives, each ready for keycaps: "Ctrl+Z", or "O H" for a two-letter sequence.
    pub keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SheetSection {
    pub section: Section,
    pub rows: Vec<Row>,
}

/// Chords and the two-letter sequence of `id` as the user has them now.
pub fn key_alts(bind: &Bindings, id: &str) -> Vec<String> {
    let mut v: Vec<String> = bind.chords_of(id).to_vec();
    if let Some(s) = bind.seqs.seq_of(id).filter(|s| !s.is_empty()) {
        v.push(
            s.chars()
                .map(String::from)
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    v
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '+')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The filter text names the label or any of the keys ("undo", "ctrl z", "ctrl+z").
pub fn row_matches(r: &Row, filter: &str) -> bool {
    let f = filter.trim();
    if f.is_empty() {
        return true;
    }
    let q = norm(f);
    r.label.to_lowercase().contains(&f.to_lowercase())
        || norm(&r.label).contains(&q)
        || r.keys.iter().any(|k| norm(k).contains(&q))
}

/// Sheet content: bound commands only, grouped by context, no alias or repeated id.
pub fn sheet_sections(
    reg: &Registry,
    bind: &Bindings,
    label: &dyn Fn(&str) -> String,
    filter: &str,
) -> Vec<SheetSection> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut by: Vec<Vec<Row>> = vec![vec![]; ORDER.len()];
    for c in reg.all() {
        if c.alias.is_some() || !seen.insert(c.id.as_str()) {
            continue;
        }
        let keys = key_alts(bind, &c.id);
        if keys.is_empty() {
            continue;
        }
        let row = Row {
            id: c.id.clone(),
            icon: c.icon.clone(),
            label: label(&c.id),
            keys,
        };
        if !row_matches(&row, filter) {
            continue;
        }
        let at = ORDER.iter().position(|s| *s == section_of(c)).unwrap_or(0);
        by[at].push(row);
    }
    ORDER
        .iter()
        .zip(by)
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(s, mut rows)| {
            rows.sort_by_cached_key(|r| (r.label.to_lowercase(), r.id.clone()));
            SheetSection { section: *s, rows }
        })
        .collect()
}

/// Section indices per column, filled greedily into the shortest column.
pub fn columns(sections: &[SheetSection], n: usize) -> Vec<Vec<usize>> {
    let mut cols: Vec<(usize, Vec<usize>)> = vec![(0, vec![]); n.max(1)];
    for (i, s) in sections.iter().enumerate() {
        let at = (0..cols.len()).min_by_key(|k| cols[*k].0).unwrap_or(0);
        cols[at].0 += s.rows.len() + 2;
        cols[at].1.push(i);
    }
    cols.into_iter().map(|c| c.1).collect()
}

/// `?` typed (any layout) or Shift+/ pressed, without Ctrl or Alt.
pub fn question_in(events: &[egui::Event], m: egui::Modifiers) -> bool {
    if m.command || m.alt {
        return false;
    }
    events.iter().any(|e| match e {
        egui::Event::Text(t) => t == "?",
        egui::Event::Key {
            key: egui::Key::Slash,
            pressed: true,
            modifiers,
            ..
        } => modifiers.shift && !modifiers.command && !modifiers.alt,
        _ => false,
    })
}

/// The key may toggle the sheet: nothing is being typed and no other window owns the keys.
pub fn may_toggle(app: &EditorApp, typing: bool) -> bool {
    !typing
        && !app.sh.palette.open
        && app.recover.is_none()
        && app.blocking.is_none()
        && app.quick.keys_ui.capture.is_none()
        && app.quick.menu.is_none()
}

/// Once per frame before the other quick keys.
pub fn keys(app: &EditorApp, ctx: &egui::Context, fire: &mut Vec<String>) {
    let hit = ctx.input(|i| question_in(&i.events, i.modifiers));
    if hit && may_toggle(app, ctx.wants_keyboard_input()) {
        fire.push(COMMAND.into());
    }
}

pub fn toggle(app: &mut EditorApp) {
    let s = &mut app.quick.sheet;
    s.open = !s.open;
    s.filter.clear();
    s.focus = s.open;
}

fn key_row(ui: &mut egui::Ui, th: &Theme, icons: &mut IconCache, r: &Row) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, th.d().row), egui::Sense::hover());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, th.radius("sm"), th.hover_fill());
    }
    let mut x = rect.right() - space::SM;
    for k in r.keys.iter().rev() {
        x = widgets::paint_keycaps(p, th, egui::pos2(x, rect.center().y), k) - space::SM;
    }
    let mut left = rect.left() + space::SM;
    if !r.icon.is_empty() {
        let ic = egui::Rect::from_min_size(
            egui::pos2(left, rect.center().y - icon_size::SM / 2.0),
            egui::Vec2::splat(icon_size::SM),
        );
        icons.paint(
            ui,
            &widgets::icon_name(&r.icon, false),
            th.icon_px(),
            ic,
            th.c("text.secondary"),
        );
    }
    left += icon_size::SM + space::SM;
    let mut job = egui::text::LayoutJob::simple(
        r.label.clone(),
        th.font(Role::Body),
        th.c("text.primary"),
        (x - left).max(24.0),
    );
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let g = ui.painter().layout_job(job);
    let at = egui::pos2(left, rect.center().y - g.size().y / 2.0);
    let elided = g.elided;
    ui.painter().galley(at, g, th.c("text.primary"));
    if elided {
        resp.on_hover_text(&r.label);
    }
}

fn section_block(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut IconCache,
    title: &str,
    rows: &[Row],
) {
    ui.add_space(space::SM);
    ui.label(
        egui::RichText::new(title.to_uppercase())
            .font(th.font(Role::Section))
            .color(th.c("text.secondary")),
    );
    ui.add_space(space::XS);
    for r in rows {
        key_row(ui, th, icons, r);
    }
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context) {
    if !app.quick.sheet.open {
        return;
    }
    let th = theme::of(ctx);
    let sections = {
        let a = &*app;
        sheet_sections(
            &a.sh.reg,
            &a.quick.bind,
            &|id| super::label(a, id),
            &a.quick.sheet.filter,
        )
    };
    let screen = ctx.screen_rect();
    let width = (screen.width() - 64.0).clamp(320.0, layout::DIALOG_WIDE);
    let body_h = (screen.height() - 220.0).clamp(160.0, 520.0);
    let n = ((width / COL_W).floor() as usize).clamp(1, COLS_MAX);
    let cols = columns(&sections, n);
    let c = &app.sh.cat;
    let st = &mut app.quick.sheet;
    let (mut close, mut customize) = (false, false);
    let m = kit::modal_dialog("shortcut_sheet", ctx, |ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            kit::modal_title(ui, c.t("sheet.title"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if widgets::close_button(ui, &c.t("sheet.close")).clicked() {
                    close = true;
                }
            });
        });
        let hint = c.t("sheet.filter");
        let label = c.t("sheet.filter_label");
        let fid = egui::Id::new(FILTER_ID);
        TextField::new(&mut st.filter)
            .id(fid)
            .hint(&hint)
            .width(width)
            .labelled_by(egui::Id::new("ddc.quick.sheet.filter.label"), &label)
            .show(ui, &th);
        if std::mem::take(&mut st.focus) {
            ui.memory_mut(|mem| mem.request_focus(fid));
        }
        ui.add_space(space::XS);
        egui::ScrollArea::vertical()
            .max_height(body_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if sections.is_empty() {
                    kit::empty_note(ui, c.t("sheet.none"), Some(&c.t("sheet.none_hint")));
                    return;
                }
                let ictx = ui.ctx().clone();
                widgets::with_icons(&ictx, |icons| {
                    ui.columns(cols.len(), |uis| {
                        for (ui, idx) in uis.iter_mut().zip(&cols) {
                            for &i in idx {
                                let s = &sections[i];
                                let title = c.t(s.section.title_key());
                                section_block(ui, &th, icons, &title, &s.rows);
                            }
                        }
                    });
                });
            });
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            widgets::keycap(ui, &th, "Esc");
            ui.label(
                egui::RichText::new(c.t("sheet.esc_close"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::secondary(&c.t("sheet.customize"))
                    .small()
                    .show(ui, &th)
                    .clicked()
                {
                    customize = true;
                }
            });
        });
    });
    if close || customize || m.should_close() {
        st.open = false;
    }
    if customize {
        app.quick.keys_open = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::keymap::UserKeys;
    use ddc_ui::{Catalog, Lang};

    fn en() -> Catalog {
        Catalog::new(Lang::En)
    }

    fn sheet(user: &UserKeys, filter: &str) -> Vec<SheetSection> {
        let reg = Registry::builtin();
        let bind = Bindings::build(&reg, user);
        let c = en();
        sheet_sections(&reg, &bind, &|id| c.t(&format!("cmd.{id}")), filter)
    }

    fn keys_of(s: &[SheetSection], id: &str) -> Option<Vec<String>> {
        s.iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.id == id)
            .map(|r| r.keys.clone())
    }

    fn app() -> EditorApp {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        EditorApp::new(ed, None, "test".into())
    }

    #[test]
    fn rebinding_shows_in_the_rows() {
        let before = keys_of(&sheet(&UserKeys::default(), ""), "file.save").unwrap();
        assert_eq!(before, vec!["Ctrl+S".to_string()]);
        let mut u = UserKeys::default();
        u.set_chord("file.save", Some("Ctrl+Alt+Q"));
        let after = keys_of(&sheet(&u, ""), "file.save").unwrap();
        assert_eq!(after, vec!["Ctrl+Alt+Q".to_string()]);
    }

    #[test]
    fn unbound_command_is_omitted() {
        let mut u = UserKeys::default();
        u.set_chord("file.save", None);
        assert!(keys_of(&sheet(&u, ""), "file.save").is_none());
        assert!(keys_of(&sheet(&UserKeys::default(), ""), "help.shortcuts").is_none());
    }

    #[test]
    fn two_letter_sequence_is_two_keycaps() {
        let mut u = UserKeys::default();
        u.set_seq("mod.delete", "zq");
        let k = keys_of(&sheet(&u, ""), "mod.delete").unwrap();
        assert!(k.contains(&"Z Q".to_string()), "{k:?}");
        let mut u = UserKeys::default();
        u.set_chord("mod.delete", None);
        u.set_seq("mod.delete", "zq");
        assert_eq!(keys_of(&sheet(&u, ""), "mod.delete").unwrap(), ["Z Q"]);
    }

    #[test]
    fn sections_follow_context_in_order_without_duplicates() {
        let s = sheet(&UserKeys::default(), "");
        let order: Vec<usize> = s
            .iter()
            .map(|x| ORDER.iter().position(|o| *o == x.section).unwrap())
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert!(s.iter().all(|x| !x.rows.is_empty()));
        let ids: Vec<&str> = s.iter().flat_map(|x| &x.rows).map(|r| r.id.as_str()).collect();
        let uniq: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), uniq.len());
        let at = |id: &str| s.iter().find(|x| x.rows.iter().any(|r| r.id == id)).map(|x| x.section);
        assert_eq!(at("file.save"), Some(Section::General));
        assert_eq!(at("mod.delete"), Some(Section::Plan));
        assert_eq!(at("view.style_xray"), Some(Section::View3d));
        assert_eq!(at("ai.ask"), Some(Section::Ai));
        for x in &s {
            assert!(x.rows.windows(2).all(|w| w[0].label.to_lowercase() <= w[1].label.to_lowercase()));
            assert!(x.rows.iter().all(|r| !r.keys.is_empty()));
        }
    }

    #[test]
    fn section_of_covers_sheets_and_schedules() {
        let reg = Registry::builtin();
        let of = |id: &str| section_of(reg.get(id).unwrap());
        assert_eq!(of("view.sheet_set"), Section::Sheets);
        assert_eq!(of("view.place_legend"), Section::Sheets);
        assert_eq!(of("ai.ask"), Section::Ai);
    }

    #[test]
    fn filter_matches_label_and_key() {
        let all = sheet(&UserKeys::default(), "");
        let n_all: usize = all.iter().map(|s| s.rows.len()).sum();
        let by_label = sheet(&UserKeys::default(), "UNDO");
        assert!(keys_of(&by_label, "edit.undo").is_some());
        let by_key = sheet(&UserKeys::default(), "ctrl z");
        assert!(keys_of(&by_key, "edit.undo").is_some());
        let plus = sheet(&UserKeys::default(), "ctrl+z");
        assert!(keys_of(&plus, "edit.undo").is_some());
        assert!(keys_of(&by_key, "file.save").is_none());
        let n_key: usize = by_key.iter().map(|s| s.rows.len()).sum();
        assert!(n_key < n_all);
        assert!(sheet(&UserKeys::default(), "zzzqqq").is_empty());
        let blank: usize = sheet(&UserKeys::default(), "   ").iter().map(|s| s.rows.len()).sum();
        assert_eq!(blank, n_all);
    }

    #[test]
    fn filter_sees_a_rebound_key() {
        let mut u = UserKeys::default();
        u.set_chord("file.save", Some("Ctrl+Alt+Q"));
        assert!(keys_of(&sheet(&u, "alt q"), "file.save").is_some());
        assert!(keys_of(&sheet(&UserKeys::default(), "alt q"), "file.save").is_none());
    }

    #[test]
    fn columns_place_every_section_once_and_balance() {
        let s = sheet(&UserKeys::default(), "");
        for n in [1usize, 2, 3] {
            let cols = columns(&s, n);
            assert_eq!(cols.len(), n);
            let mut all: Vec<usize> = cols.iter().flatten().copied().collect();
            all.sort_unstable();
            assert_eq!(all, (0..s.len()).collect::<Vec<_>>());
        }
        assert_eq!(columns(&s, 0).len(), 1);
        let cost = |c: &Vec<usize>| c.iter().map(|i| s[*i].rows.len() + 2).sum::<usize>();
        let cols = columns(&s, 3);
        let big = s.iter().map(|x| x.rows.len() + 2).max().unwrap();
        let (lo, hi) = cols.iter().map(cost).fold((usize::MAX, 0), |a, c| (a.0.min(c), a.1.max(c)));
        assert!(hi - lo <= big, "{lo} {hi} {big}");
    }

    #[test]
    fn question_mark_is_recognised() {
        let none = egui::Modifiers::NONE;
        let shift = egui::Modifiers::SHIFT;
        let text = |t: &str| vec![egui::Event::Text(t.into())];
        let slash = |m: egui::Modifiers| {
            vec![egui::Event::Key {
                key: egui::Key::Slash,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: m,
            }]
        };
        assert!(question_in(&text("?"), shift));
        assert!(!question_in(&text("/"), none));
        assert!(!question_in(&text("a"), none));
        assert!(question_in(&slash(shift), shift));
        assert!(!question_in(&slash(none), none));
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        };
        assert!(!question_in(&slash(ctrl_shift), ctrl_shift));
        assert!(!question_in(&[], none));
    }

    #[test]
    fn question_is_ignored_while_a_text_field_has_focus() {
        let a = app();
        assert!(may_toggle(&a, false));
        assert!(!may_toggle(&a, true));
        let ctx = egui::Context::default();
        let mut buf = String::new();
        let mut typing = Vec::new();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.text_edit_singleline(&mut buf);
                    r.request_focus();
                });
                typing.push(ctx.wants_keyboard_input());
            });
        }
        assert!(typing.last().copied().unwrap_or(false));
        assert!(!may_toggle(&a, *typing.last().unwrap()));
        let idle = egui::Context::default();
        let mut free = true;
        let _ = idle.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            free = ctx.wants_keyboard_input();
        });
        assert!(!free && may_toggle(&a, free));
    }

    #[test]
    fn other_windows_block_the_key() {
        let mut a = app();
        a.sh.palette.open = true;
        assert!(!may_toggle(&a, false));
        a.sh.palette.open = false;
        a.quick.keys_ui.capture = Some("file.save".into());
        assert!(!may_toggle(&a, false));
    }

    #[test]
    fn toggle_opens_clears_the_filter_and_closes() {
        let mut a = app();
        assert!(a.handlers.contains_key(COMMAND) && a.sh.reg.get(COMMAND).is_some());
        assert!(!a.quick.sheet.open);
        a.quick.sheet.filter = "old".into();
        toggle(&mut a);
        assert!(a.quick.sheet.open && a.quick.sheet.focus && a.quick.sheet.filter.is_empty());
        a.quick.sheet.filter = "undo".into();
        toggle(&mut a);
        assert!(!a.quick.sheet.open && !a.quick.sheet.focus && a.quick.sheet.filter.is_empty());
    }

    #[test]
    fn command_is_not_a_repeatable_last_command() {
        assert!(!ddc_ui::frecency::recordable(COMMAND));
        let reg = Registry::builtin();
        let c = reg.get(COMMAND).unwrap();
        assert!(c.shortcuts.is_empty() && c.mnemonic.is_none());
    }

    #[test]
    fn strings_exist_in_both_languages() {
        for lang in [Lang::Ru, Lang::En] {
            let c = Catalog::new(lang);
            let mut keys = vec![
                "cmd.help.shortcuts",
                "sheet.title",
                "sheet.filter",
                "sheet.filter_label",
                "sheet.customize",
                "sheet.close",
                "sheet.esc_close",
                "sheet.none",
                "sheet.none_hint",
            ];
            keys.extend(ORDER.iter().map(|s| s.title_key()));
            for k in keys {
                assert_ne!(c.t_in(lang, k), k, "{lang:?} {k}");
            }
            let mine = c.t_in(lang, "cmd.help.shortcuts");
            let keymap = c.t_in(lang, "cmd.mgr.keymap");
            assert_ne!(mine, keymap, "{lang:?}: same label as the key editor");
        }
    }
}
