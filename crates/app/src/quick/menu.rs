// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Right-click context menu: commands for the category of the current (or empty) selection,
//! the last command repeated first. A right-click never changes the selection: the items under
//! the cursor are listed on top, and choosing one selects it with the menu kept open.
use crate::editor::Editor;
use crate::shell::EditorApp;
use ddc_ui::frecency;

pub const SEP: &str = "-";
/// Rows of the "items here" block: its heading and each item (`@here:<n>`).
const HDR: &str = "@here";
const HERE: &str = "@here:";

/// Menu rows (command ids, [`SEP`] is the separator) without availability checks.
pub fn items(
    ed: &Editor,
    reg: &ddc_ui::Registry,
    last: Option<&str>,
    has_clip: bool,
    sets: usize,
    live: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    let mut push = |s: &str| v.push(s.to_string());
    if last.is_some() {
        push("app.repeat");
    }
    let Some(first) = ed.selection.first().copied() else {
        for id in [
            SEP,
            "app.palette",
            "edit.select_all",
            "sel.prev",
            "mod.reveal",
            "view.zoom_fit",
            SEP,
            "edit.undo",
            "edit.redo",
            "app.history",
        ] {
            push(id);
        }
        if sets > 0 {
            push("sel.sets");
        }
        if has_clip {
            push("edit.paste_params");
        }
        return v;
    };
    let cat = crate::selection::category(ed, first);
    push(SEP);
    push("mod.select_similar");
    push("mod.select_similar_project");
    if cat == "wall" {
        push("sel.connected");
        push("sel.run");
    }
    push("sel.invert");
    push("mod.select_filter");
    push("sel.set_save");
    if sets > 0 {
        push("sel.sets");
    }
    push(SEP);
    if matches!(cat, "wall" | "floor") {
        push("mgr.types");
    }
    if matches!(cat, "wall" | "door" | "window" | "opening") {
        push("edit.copy_params");
        if has_clip {
            push("edit.paste_params");
        }
    }
    push(SEP);
    push("edit.copy");
    push("edit.cut");
    push(SEP);
    push("mod.hide");
    push("mod.isolate");
    push("mod.reveal");
    push("view.zoom_sel_3d");
    let shown: Vec<String> = v.clone();
    let extra: Vec<String> = reg
        .contextual(cat)
        .into_iter()
        .map(|c| c.id.clone())
        .filter(|id| !shown.contains(id) && id != "mod.delete" && live(id.as_str()))
        .take(8)
        .collect();
    if !extra.is_empty() {
        v.push(SEP.into());
        v.extend(extra);
    }
    v.push(SEP.into());
    v.push("mod.delete".into());
    v
}

/// Right-click in a view at rest: list the items under the cursor (pure query) and open the menu.
pub fn right_click(app: &mut EditorApp, ctx: &egui::Context, area: egui::Rect) {
    let p = ctx.input(|i| {
        i.pointer
            .button_clicked(egui::PointerButton::Secondary)
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    let Some(p) = p else { return };
    if !area.contains(p)
        || !menu_allowed(app)
        || ctx
            .layer_id_at(p)
            .is_some_and(|l| l.order != egui::Order::Background)
    {
        return;
    }
    let on_plan =
        !app.view3d && app.section.is_none() && app.plan.b.rect.is_some_and(|r| r.contains(p));
    if let (true, Some(w)) = (on_plan, app.plan.cursor) {
        let r = 8.0 / app.plan.scale.max(1e-6);
        let v = &app.plan.b.vis;
        let ed = &app.ed;
        crate::pick_rules::set_here(crate::pick_rules::items_here(ed, v, w, r));
    } else if let (true, Some(s)) = (app.view3d, app.view.e3.hover.and_then(|t| t.sel())) {
        crate::pick_rules::set_here(vec![crate::pick_rules::here_of(&app.ed, s)]);
    } else {
        crate::pick_rules::set_here(vec![]);
    }
    app.quick.menu = Some(p);
    app.quick.menu_fresh = true;
    app.quick.menu_sets = false;
}

/// The menu opens when idle, and over an armed tool with no chain in progress (with one, the
/// right button finishes the chain: `Interaction::update`).
fn menu_allowed(app: &EditorApp) -> bool {
    let ed = &app.ed;
    let armed_idle = ed.tool_armed()
        && ed.chain().is_empty()
        && !app.plan.b.active()
        && !app.sh.palette.open
        && app.recover.is_none()
        && app.blocking.is_none()
        && app.quick.keys_ui.capture.is_none();
    super::idle(app) || armed_idle
}

fn available(app: &EditorApp, id: &str) -> bool {
    app.sh.reg.get(id).is_some()
        && app
            .sh
            .ribbon
            .available
            .as_ref()
            .is_none_or(|a| a.contains(id))
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context, fire: &mut Vec<String>) {
    let Some(at) = app.quick.menu else { return };
    let last = frecency::with(|u| u.last_repeatable(&app.sh.reg).map(String::from));
    let c = &app.sh.cat;
    let mut rows: Vec<(String, String, String)> = vec![];
    if app.quick.menu_sets {
        for (i, s) in app.quick.sets.list.iter().enumerate() {
            let name = format!("{} ({})", s.name, s.items.len());
            rows.push((format!("@set:{i}"), name, String::new()));
        }
    } else {
        let here = crate::pick_rules::here();
        if !here.is_empty() {
            rows.push((HDR.into(), c.t("pick_rules.here"), String::new()));
            for (i, h) in here.iter().enumerate() {
                let on = app.ed.selection.contains(&h.sel);
                let keys = if on { c.t("pick_rules.here.selected") } else { String::new() };
                rows.push((
                    format!("{HERE}{i}"),
                    crate::pick_rules::describe(&app.ed, h),
                    keys,
                ));
            }
            rows.push((SEP.into(), String::new(), String::new()));
        }
        let ids = items(
            &app.ed,
            &app.sh.reg,
            last.as_deref(),
            app.quick.clip.is_some(),
            app.quick.sets.list.len(),
            &|id: &str| available(app, id) && app.handlers.contains_key(app.sh.reg.resolve(id)),
        );
        for id in ids {
            if id == SEP {
                if rows.last().is_some_and(|r| r.0 != SEP) {
                    rows.push((id, String::new(), String::new()));
                }
                continue;
            }
            if !available(app, &id) {
                continue;
            }
            let name = match (id.as_str(), &last) {
                ("app.repeat", Some(l)) => {
                    c.tf("quick.menu.repeat", &[("cmd", &super::label(app, l))])
                }
                ("edit.undo", _) => app.undo_redo_name(true),
                ("edit.redo", _) => app.undo_redo_name(false),
                _ => super::label(app, &id),
            };
            let keys = if id == "app.repeat" {
                c.t("quick.menu.enter")
            } else {
                super::keys_text(app, &id)
            };
            rows.push((id, name, keys));
        }
        if rows.last().is_some_and(|r| r.0 == SEP) {
            rows.pop();
        }
    }
    let head = match app.ed.selection.first() {
        Some(s) if !app.quick.menu_sets => {
            let cat = crate::selection::category(&app.ed, *s);
            Some(c.tf(
                "quick.menu.head",
                &[
                    ("cat", &c.t(&format!("quick.cat.{cat}"))),
                    ("n", &app.ed.selection.len().to_string()),
                ],
            ))
        }
        _ if app.quick.menu_sets => Some(c.t("cmd.sel.sets")),
        _ => None,
    };
    let mut pick: Option<String> = None;
    let resp = egui::Area::new(egui::Id::new("ddc.quick.menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(at)
        .constrain(true)
        .show(ctx, |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_min_width(220.0);
                if let Some(h) = &head {
                    ui.label(egui::RichText::new(h).weak().small());
                }
                for (id, name, keys) in &rows {
                    if id == SEP {
                        ui.separator();
                        continue;
                    }
                    if id == HDR {
                        ui.label(egui::RichText::new(name).weak().small());
                        continue;
                    }
                    let b = egui::Button::new(name.as_str())
                        .shortcut_text(keys.as_str())
                        .frame(false)
                        .min_size(egui::vec2(ui.available_width(), 0.0));
                    if ui.add(b).clicked() {
                        pick = Some(id.clone());
                    }
                }
            });
        });
    let rect = resp.response.rect;
    let fresh = std::mem::take(&mut app.quick.menu_fresh);
    // Esc closes the menu and does not reach the plan (where it would clear the selection)
    let outside = ctx.input_mut(|i| {
        (i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p)))
            || i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
    });
    let here_pick = pick
        .as_deref()
        .and_then(|id| id.strip_prefix(HERE))
        .and_then(|n| n.parse::<usize>().ok());
    if let Some(i) = here_pick {
        // the menu stays open: its commands now act on the chosen item
        if let Some(h) = crate::pick_rules::here().get(i) {
            app.ed.selection = vec![h.sel];
        }
    } else if let Some(id) = pick {
        app.quick.menu = None;
        app.quick.menu_sets = false;
        if let Some(i) = id
            .strip_prefix("@set:")
            .and_then(|n| n.parse::<usize>().ok())
        {
            // set from another level: switch to its level first
            if let Some(lv) = app.quick.sets.list.get(i).map(|s| s.level) {
                if lv != app.ed.level && app.ed.levels().iter().any(|l| l.0 == lv) {
                    app.ed.set_level(lv);
                }
            }
            let sel = app.quick.sets.recall(i, &app.ed);
            app.ed.selection = sel;
        } else {
            fire.push(id);
        }
    } else if outside && !fresh {
        app.quick.menu = None;
        app.quick.menu_sets = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Sel;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    #[test]
    fn menu_per_category() {
        let reg = ddc_ui::Registry::builtin();
        let mut ed = Editor::new_in_memory().unwrap();
        let empty = items(&ed, &reg, None, false, 0, &|_| true);
        assert!(empty.contains(&"edit.select_all".to_string()));
        assert!(!empty.contains(&"app.repeat".to_string()));
        assert!(!empty.contains(&"mod.delete".to_string()));
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Release(v2(4.0, 3.0)));
        ed.set_mode(Mode::Select);
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let w = items(&ed, &reg, Some("arch.wall"), true, 1, &|_| true);
        for id in [
            "app.repeat",
            "mod.select_similar",
            "mod.select_similar_project",
            "sel.connected",
            "mgr.types",
            "edit.copy_params",
            "edit.paste_params",
            "edit.copy",
            "edit.cut",
            "view.zoom_sel_3d",
            "mod.hide",
            "mod.isolate",
            "sel.sets",
            "mod.delete",
        ] {
            assert!(w.contains(&id.to_string()), "{id}");
        }
        assert_eq!(w[0], "app.repeat");
        assert_eq!(w.last().map(String::as_str), Some("mod.delete"));
        let n = w.len();
        let mut d = w.clone();
        d.dedup();
        assert_eq!(d.len(), n, "no repeated rows");
        ed.selection = vec![Sel::Room(1)];
        let r = items(&ed, &reg, None, false, 0, &|_| true);
        assert!(!r.contains(&"sel.connected".to_string()));
        assert!(!r.contains(&"edit.copy_params".to_string()));
        assert!(r.contains(&"mod.isolate".to_string()));
    }

    #[test]
    fn selection_has_frame_copy_cut() {
        let reg = ddc_ui::Registry::builtin();
        let mut ed = Editor::new_in_memory().unwrap();
        ed.selection = vec![Sel::Room(1)];
        let v = items(&ed, &reg, None, false, 0, &|_| true);
        let at = |id: &str| v.iter().position(|x| x == id).unwrap();
        assert!(at("edit.copy") < at("mod.hide"));
        assert!(at("edit.cut") < at("mod.hide"));
        assert!(at("view.zoom_sel_3d") > at("mod.reveal"));
        assert!(at("view.zoom_sel_3d") < at("mod.delete"));
    }

    #[test]
    fn extras_skip_unavailable_before_cap() {
        let reg = ddc_ui::Registry::builtin();
        let mut ed = Editor::new_in_memory().unwrap();
        ed.selection = vec![Sel::Room(1)];
        let all = items(&ed, &reg, None, false, 0, &|_| true);
        let none = items(&ed, &reg, None, false, 0, &|_| false);
        assert!(none.len() <= all.len());
        let extras = |v: &[String]| {
            let from = v.iter().position(|x| x == "view.zoom_sel_3d").unwrap();
            v[from + 1..].iter().filter(|x| *x != SEP && *x != "mod.delete").count()
        };
        assert_eq!(extras(&none), 0, "dead ids take no slot");
        assert!(extras(&all) <= 8);
        // only the first live ids fill the cap
        let first_live: Vec<String> = all
            .iter()
            .skip_while(|x| *x != "view.zoom_sel_3d")
            .skip(1)
            .filter(|x| *x != SEP && *x != "mod.delete")
            .cloned()
            .collect();
        let dead = first_live.first().cloned();
        if let Some(d) = dead {
            let v = items(&ed, &reg, None, false, 0, &|id: &str| id != d.as_str());
            assert!(!v.contains(&d));
        }
    }
}
