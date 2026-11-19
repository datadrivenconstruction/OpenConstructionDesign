// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Commands and keys reference: a panel docked on the right (or a floating window) with one
//! search over command names and shortcuts. A row runs its command; Enter runs the first hit.
//! Search and ranking: `ddc_ui::cmdref`.
use crate::shell::EditorApp;
use ddc_ui::cmdref::{self, Row};
use ddc_ui::theme::Role;
use std::cell::RefCell;

const ROW_H: f32 = 24.0;

#[derive(Default)]
struct State {
    open: bool,
    /// Floating window instead of the docked panel.
    float: bool,
    query: String,
    focus: bool,
    rows: Vec<Row>,
    /// Open and floating flags were read from the preferences.
    loaded: bool,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

/// What the panel asks the host to do after the frame.
#[derive(Default)]
struct Out {
    run: Option<String>,
    close: bool,
    flip: bool,
    all_keys: bool,
}

pub fn toggle() {
    ST.with(|s| {
        let s = &mut s.borrow_mut();
        s.open = !s.open;
        s.focus = s.open;
    });
}

pub fn is_open() -> bool {
    ST.with(|s| s.borrow().open)
}

/// Keep the preferences equal to the panel state; true when they changed.
fn mirror(open: bool, float: bool, p: &mut crate::prefs::Prefs) -> bool {
    let changed = p.help_open != open || p.help_float != float;
    p.help_open = open;
    p.help_float = float;
    changed
}

/// Keys of a command as shown in the list: chords, then the two-letter code.
fn keys_of(app: &EditorApp, id: &str) -> String {
    let b = &app.quick.bind;
    let mut v: Vec<String> = b.chords_of(id).to_vec();
    if v.is_empty() && !app.quick.built {
        if let Some(c) = app.sh.reg.get(id) {
            v.extend(c.shortcuts.iter().cloned());
        }
    }
    if let Some(s) = b.seqs.seq_of(id).or_else(|| app.keys2.seq_of(id)) {
        v.push(s.to_string());
    }
    v.join(" · ")
}

/// One row per command that has a name in the current language.
fn rows(app: &EditorApp) -> Vec<Row> {
    let c = &app.sh.cat;
    app.sh
        .reg
        .all()
        .iter()
        .filter(|cmd| cmd.alias.is_none())
        .map(|cmd| {
            let key = format!("cmd.{}", cmd.id);
            let label = c.t(&key);
            Row {
                id: cmd.id.clone(),
                label: if label == key { String::new() } else { label },
                keys: keys_of(app, &cmd.id),
            }
        })
        .collect()
}

/// Panel or window for this frame; called with the other side docks.
pub fn dock(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    let mut out = Out::default();
    if !st.loaded {
        st.loaded = true;
        st.open = app.prefs.help_open;
        st.float = app.prefs.help_float;
    }
    if st.open {
        st.rows = rows(app);
        if st.float {
            window(app, ctx, &mut st, &mut out);
        } else {
            side(app, ctx, &mut st, &mut out);
        }
    }
    if out.close {
        st.open = false;
    }
    if out.flip {
        st.float = !st.float;
        st.focus = true;
    }
    if mirror(st.open, st.float, &mut app.prefs) {
        app.prefs.save();
    }
    ST.with(|s| *s.borrow_mut() = st);
    if out.all_keys {
        app.run("mgr.keymap");
    }
    if let Some(id) = out.run {
        app.run(&id);
    }
}

fn side(app: &EditorApp, ctx: &egui::Context, st: &mut State, out: &mut Out) {
    let th = &app.sh.theme;
    egui::SidePanel::right("ddc.help.dock")
        .resizable(true)
        .min_width(240.0)
        .default_width(300.0)
        .frame(
            egui::Frame::new()
                .fill(th.c("bg.panel"))
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show(ctx, |ui| content(ui, app, st, out, true));
}

/// The floating window, open, for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &EditorApp, ctx: &egui::Context) {
    let mut st = State { open: true, float: true, rows: rows(app), ..State::default() };
    window(app, ctx, &mut st, &mut Out::default());
}

fn window(app: &EditorApp, ctx: &egui::Context, st: &mut State, out: &mut Out) {
    let mut open = true;
    crate::ui_kit::dialog::Dialog::new(
        "help.ref",
        app.sh.cat.t("help.ref.title"),
        crate::ui_kit::dialog::DialogSize::Tool,
    )
    .id(egui::Id::new("ddc.help.ref.win"))
    .open(&mut open)
    .size([340.0, 460.0])
    .show(ctx, |ui| content(ui, app, st, out, false));
    if !open {
        out.close = true;
    }
}

fn content(ui: &mut egui::Ui, app: &EditorApp, st: &mut State, out: &mut Out, docked: bool) {
    let c = &app.sh.cat;
    let th = &app.sh.theme;
    if docked {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(c.t("help.ref.title"))
                    .font(th.font(Role::BodyStrong))
                    .color(th.c("text.primary")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ddc_ui::widgets::close_button(ui, &c.t("help.ref.close")).clicked() {
                    out.close = true;
                }
            });
        });
        ui.add_space(4.0);
    }
    let r = ui.add(
        egui::TextEdit::singleline(&mut st.query)
            .hint_text(c.t("help.ref.search"))
            .desired_width(f32::INFINITY),
    );
    if std::mem::take(&mut st.focus) {
        r.request_focus();
    }
    let found = cmdref::search(&st.rows, &st.query);
    // an empty query lists everything: Enter then would run an arbitrary first command
    if !st.query.trim().is_empty()
        && r.lost_focus()
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
    {
        if let Some(first) = found.first() {
            out.run = Some(first.id.clone());
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(c.tf("help.ref.count", &[("n", &found.len().to_string())]))
                .small()
                .color(th.c("text.secondary")),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let flip = if docked {
                "help.ref.float"
            } else {
                "help.ref.dock"
            };
            if ui.link(c.t(flip)).clicked() {
                out.flip = true;
            }
        });
    });
    ui.separator();
    let foot = 28.0;
    let h = (ui.available_height() - foot).max(ROW_H * 3.0);
    if found.is_empty() {
        ui.add_space(8.0);
        ui.add(egui::Label::new(
            egui::RichText::new(c.t("help.ref.empty")).color(th.c("text.secondary")),
        ));
    }
    egui::ScrollArea::vertical()
        .id_salt("ddc.help.ref.list")
        .max_height(h)
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, found.len(), |ui, range| {
            for r in &found[range] {
                if row(ui, app, r) {
                    out.run = Some(r.id.clone());
                }
            }
        });
    ui.add_space(4.0);
    if ui.link(c.t("help.ref.all_keys")).clicked() {
        out.all_keys = true;
    }
}

/// One list row: name left (elided), keys right; true when clicked.
fn row(ui: &mut egui::Ui, app: &EditorApp, r: &Row) -> bool {
    let th = &app.sh.theme;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
    // a painted row still needs its name for screen readers and the layout probe
    let on = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, on, &r.label));
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let keys = p.layout_no_wrap(
        r.keys.clone(),
        th.font(Role::Numeric),
        th.c("text.secondary"),
    );
    let kw = keys.size().x;
    if !r.keys.is_empty() {
        let pos = egui::pos2(
            rect.right() - 8.0 - kw,
            rect.center().y - keys.size().y / 2.0,
        );
        p.galley(pos, keys, th.c("text.secondary"));
    }
    let name_rect = egui::Rect::from_min_max(
        rect.left_top(),
        egui::pos2((rect.right() - kw - 16.0).max(rect.left() + 40.0), rect.bottom()),
    );
    p.with_clip_rect(name_rect).text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &r.label,
        th.font(Role::Body),
        th.c("text.primary"),
    );
    let clicked = resp.clicked();
    resp.on_hover_text(
        app.sh
            .cat
            .tf("help.ref.run_tip", &[("name", r.label.as_str())]),
    );
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> EditorApp {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        EditorApp::new(ed, None, "test".into())
    }

    #[test]
    fn rows_carry_names_and_keys_and_search_finds_both() {
        let a = app();
        let r = rows(&a);
        let wall = r.iter().find(|x| x.id == "arch.wall").unwrap();
        assert!(!wall.label.is_empty());
        assert!(wall.keys.contains("WA"), "{}", wall.keys);
        // a code finds its command, a name finds it too
        assert_eq!(cmdref::search(&r, "WA")[0].id, "arch.wall");
        assert!(cmdref::search(&r, &wall.label).iter().any(|x| x.id == "arch.wall"));
        let undo = r.iter().find(|x| x.id == "edit.undo").unwrap();
        assert!(undo.keys.contains("Ctrl"), "{}", undo.keys);
    }

    #[test]
    fn help_state_persists() {
        let mut p = crate::prefs::Prefs::default();
        assert!(!p.help_open && !p.help_float);
        assert!(!mirror(false, false, &mut p), "nothing to store");
        assert!(mirror(true, true, &mut p));
        assert!(!mirror(true, true, &mut p), "unchanged: no write");
        let back = crate::prefs::Prefs::parse(&p.text());
        assert!(back.help_open && back.help_float);
        assert!(mirror(false, true, &mut p));
        let back = crate::prefs::Prefs::parse(&p.text());
        assert!(!back.help_open && back.help_float);
    }

    #[test]
    fn command_toggles_the_panel() {
        let a = app();
        assert!(a.handlers.contains_key("help.reference"));
        assert!(a.sh.reg.get("help.reference").is_some());
        let was = is_open();
        toggle();
        assert_eq!(is_open(), !was);
        toggle();
        assert_eq!(is_open(), was);
    }
}
