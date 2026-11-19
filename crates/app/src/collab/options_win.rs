//! Project options window: sets and options, what the current plan and 3D show, enabling selection,
//! accept and delete (with confirmation: they delete elements).

use std::sync::{Arc, Mutex};

use ddc_model::{Error, Model, OptionSet, ViewKey};
use ddc_ui::notify::Level;
use ddc_ui::theme::{control, space, stroke, Role};
use ddc_ui::widgets::{self, Button, MenuTone, TextField};
use ddc_ui::Theme;
use egui::{self, Align, Key, Layout};

use crate::editor::OpFn;
use crate::shell::EditorApp;

/// Column widths of the option table; the name column takes what is left.
const W_NUM: f32 = 72.0;
const W_SHOW: f32 = 72.0;
const W_PRIMARY: f32 = 80.0;
const W_ACT: f32 = 184.0;
const W_NAME_MIN: f32 = 96.0;
const W_NEW_OPTION: f32 = 220.0;
const W_NEW_SET: f32 = 240.0;

/// Rows of the overflow menu of an option: (label key, tone). The destructive row is last,
/// quiet, and never the default.
const MENU: [(&str, MenuTone); 2] = [
    ("options.accept_more", MenuTone::Normal),
    ("options.delete_more", MenuTone::Danger),
];

fn name_w(avail: f32) -> f32 {
    (avail - W_NUM - 2.0 * W_SHOW - W_PRIMARY - W_ACT).max(W_NAME_MIN)
}

/// A typed name is added by the button or by Enter in its field, and only when it is not blank.
fn submit(ready: bool, clicked: bool, enter: bool) -> bool {
    ready && (clicked || enter)
}

/// Round radio mark with a hit area of its own, named for assistive tools and in its tooltip.
fn radio_dot(ui: &mut egui::Ui, th: &Theme, on: bool, name: &str, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(control::HIT_MIN, th.d().control),
        egui::Sense::click(),
    );
    let enabled = ui.is_enabled();
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, on, name)
    });
    let s = th.sem();
    let (c, r) = (rect.center(), control::CHECK / 2.0);
    let line = if on {
        s.accent
    } else if resp.hovered() {
        s.focus
    } else {
        s.border_control
    };
    let fill = if on { s.accent } else { s.bg_input };
    let p = ui.painter();
    p.circle(c, r, fill, egui::Stroke::new(stroke::CONTROL, line));
    if on {
        p.circle_filled(c, r * 0.4, s.accent_text_on);
    }
    if resp.has_focus() {
        p.circle_stroke(c, r + space::XXS, egui::Stroke::new(stroke::FOCUS, s.focus_ring));
    }
    resp.on_hover_text(tip)
}

fn head_cell(ui: &mut egui::Ui, th: &Theme, w: f32, text: &str, right: bool) {
    crate::ui_kit::fixed_cell(ui, w, |ui| {
        let draw = |ui: &mut egui::Ui| {
            widgets::elided_label(ui, text, th.font(Role::TableHeader), th.sem().text_secondary);
        };
        if right {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(space::MD);
                draw(ui);
            });
        } else {
            draw(ui);
        }
    });
}

#[derive(Default)]
pub struct OptionsWin {
    open: bool,
    new_set: String,
    new_option: Vec<(i64, String)>,
    confirm: Option<Confirm>,
    error: Option<String>,
    /// Revision the list was read at.
    rev: Option<(u64, u64)>,
    sets: Vec<OptionSet>,
}

const PRIMARY_TIP: &str = "options.primary_tip";

/// Why "Add selected" is unavailable (locale key), or None.
fn add_selection_why(has_sel: bool) -> Option<&'static str> {
    (!has_sel).then_some("options.select_first")
}

#[derive(Clone)]
enum Confirm {
    Accept(i64, String, usize),
    Delete(i64, String, usize),
}

enum Act {
    NewSet(String, String),
    NewOption(i64, String),
    Show(ViewKey, i64, i64),
    Primary(i64),
    AddSelection(i64),
    RemoveSelection,
    Accept(i64),
    Delete(i64),
}

pub fn open(a: &mut EditorApp) {
    a.collab.options.open = true;
}

fn selection_items(a: &EditorApp) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> =
        a.ed.selection
            .iter()
            .filter_map(|s| crate::phase_ui::sel_ref(*s))
            .map(|(k, id)| (k.to_string(), id))
            .collect();
    v.sort();
    v.dedup();
    v
}

fn run<T: Default + Send + 'static>(
    a: &mut EditorApp,
    f: impl Fn(&mut Model) -> Result<T, Error> + Send + Sync + 'static,
) -> Result<T, Error> {
    let out: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let slot = out.clone();
    a.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let v = f(m)?;
        if let Ok(mut g) = slot.lock() {
            *g = Some(v);
        }
        Ok(())
    })))?;
    let v = out.lock().ok().and_then(|mut g| g.take());
    Ok(v.unwrap_or_default())
}

fn perform(w: &mut OptionsWin, a: &mut EditorApp, act: Act) -> Result<(), Error> {
    match act {
        Act::NewSet(name, first) => {
            run(a, move |m| m.add_option_set(&name, &first))?;
        }
        Act::NewOption(set, name) => {
            run(a, move |m| m.add_option(set, &name))?;
        }
        Act::Show(v, set, opt) => {
            run(a, move |m| m.set_view_option(v, set, opt))?;
        }
        Act::Primary(id) => {
            run(a, move |m| m.set_primary_option(id))?;
        }
        Act::AddSelection(opt) => {
            let items = selection_items(a);
            run(a, move |m| m.add_to_option(opt, &items))?;
        }
        Act::RemoveSelection => {
            let items = selection_items(a);
            run(a, move |m| m.remove_from_option(&items))?;
        }
        Act::Accept(id) => {
            let rep = run(a, move |m| m.accept_option(id))?;
            let msg = a.sh.cat.tf(
                "options.accepted",
                &[
                    ("kept", &rep.accepted.to_string()),
                    ("removed", &rep.removed.to_string()),
                ],
            );
            a.sh.toasts.push(Level::Info, msg, None);
        }
        Act::Delete(id) => {
            run(a, move |m| m.delete_option(id))?;
        }
    }
    w.rev = None;
    Ok(())
}

fn set_group(
    ui: &mut egui::Ui,
    a: &EditorApp,
    set: &OptionSet,
    typed: &mut String,
    has_sel: bool,
    acts: &mut Vec<Act>,
    confirm: &mut Option<Confirm>,
) {
    let c = &a.sh.cat;
    let th = ddc_ui::theme::of(ui.ctx());
    let th: &Theme = &th;
    let shown = Shown {
        plan: ViewKey::Plan(a.ed.level),
        three: ViewKey::ThreeD,
        in_plan: a.ed.model.view_option(ViewKey::Plan(a.ed.level), set.id).ok().flatten(),
        in_3d: a.ed.model.view_option(ViewKey::ThreeD, set.id).ok().flatten(),
    };
    widgets::card(ui, th, |ui| {
        widgets::elided_label(ui, &set.name, th.font(Role::BodyStrong), th.sem().text_primary);
        ui.add_space(space::XS);
        crate::ui_kit::fixed_row(ui, &format!("ddc.options.head.{}", set.id), |ui| {
            crate::ui_kit::no_gap(ui, crate::ui_kit::Gap::Cells);
            let nw = name_w(ui.available_width());
            head_cell(ui, th, nw, &c.t("options.option"), false);
            head_cell(ui, th, W_NUM, &c.t("options.members"), true);
            head_cell(ui, th, W_SHOW, &c.t("options.in_plan"), false);
            head_cell(ui, th, W_SHOW, &c.t("options.in_3d"), false);
            head_cell(ui, th, W_PRIMARY, &c.t("options.primary"), false);
        });
        ui.separator();
        for o in &set.options {
            option_row(ui, th, c, set, o, &shown, has_sel, acts, confirm);
        }
        ui.add_space(space::XS);
        crate::ui_kit::fixed_row(ui, &format!("ddc.options.new.{}", set.id), |ui| {
            ui.spacing_mut().item_spacing.x = space::SM;
            let hint = c.t("options.new_option");
            let out = TextField::new(&mut *typed)
                .labelled_by(egui::Id::new(("ddc.options.new_option.lbl", set.id)), &hint)
                .hint(&hint)
                .width(W_NEW_OPTION)
                .id_salt(("ddc.options.new_option", set.id))
                .show(ui, th);
            out.response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, hint.as_str()));
            let enter = out.committed && ui.input(|i| i.key_pressed(Key::Enter));
            let ready = !typed.trim().is_empty();
            let label = c.t("options.add");
            let clicked = ui
                .add_enabled_ui(ready, |ui| Button::secondary(&label).show(ui, th))
                .inner
                .clicked();
            if submit(ready, clicked, enter) {
                acts.push(Act::NewOption(set.id, typed.trim().to_string()));
                typed.clear();
            }
        });
    });
}

/// Which option of the set each view shows.
struct Shown {
    plan: ViewKey,
    three: ViewKey,
    in_plan: Option<i64>,
    in_3d: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
fn option_row(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &ddc_ui::Catalog,
    set: &OptionSet,
    o: &ddc_model::DesignOption,
    shown: &Shown,
    has_sel: bool,
    acts: &mut Vec<Act>,
    confirm: &mut Option<Confirm>,
) {
    crate::ui_kit::fixed_row(ui, &format!("ddc.options.row.{}", o.id), |ui| {
        crate::ui_kit::no_gap(ui, crate::ui_kit::Gap::Cells);
        let nw = name_w(ui.available_width());
        crate::ui_kit::fixed_cell(ui, nw, |ui| {
            crate::ui_kit::elided(ui, &o.name);
        });
        crate::ui_kit::fixed_cell(ui, W_NUM, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(space::MD);
                ui.label(th.rich(Role::Numeric, &o.members.to_string()));
            });
        });
        let name = [("name", o.name.as_str())];
        let in_plan = c.tf("options.show_plan", &name);
        crate::ui_kit::fixed_cell(ui, W_SHOW, |ui| {
            if radio_dot(ui, th, shown.in_plan == Some(o.id), &in_plan, &in_plan).clicked() {
                acts.push(Act::Show(shown.plan, set.id, o.id));
            }
        });
        let in_3d = c.tf("options.show_3d", &name);
        crate::ui_kit::fixed_cell(ui, W_SHOW, |ui| {
            if radio_dot(ui, th, shown.in_3d == Some(o.id), &in_3d, &in_3d).clicked() {
                acts.push(Act::Show(shown.three, set.id, o.id));
            }
        });
        let prim = c.tf("options.primary_named", &name);
        crate::ui_kit::fixed_cell(ui, W_PRIMARY, |ui| {
            let tip = if o.is_primary { prim.clone() } else { c.t(PRIMARY_TIP) };
            if radio_dot(ui, th, o.is_primary, &prim, &tip).clicked() && !o.is_primary {
                acts.push(Act::Primary(o.id));
            }
        });
        crate::ui_kit::fixed_cell(ui, W_ACT, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let tip = c.tf("options.more", &name);
                let ctx = ui.ctx().clone();
                let more = widgets::with_icons(&ctx, |icons| {
                    widgets::icon_button(ui, icons, th, "context-menu", false, &tip)
                });
                more.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip.as_str())
                });
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_min_width(control::DROPDOWN_W);
                    for (i, (key, tone)) in MENU.iter().enumerate() {
                        if i > 0 && *tone == MenuTone::Danger {
                            widgets::menu_divider(ui, th);
                        }
                        if !widgets::menu_item(ui, th, &c.t(key), "", *tone, true).clicked() {
                            continue;
                        }
                        *confirm = Some(if *tone == MenuTone::Danger {
                            Confirm::Delete(o.id, o.name.clone(), o.members)
                        } else {
                            let others = set
                                .options
                                .iter()
                                .filter(|x| x.id != o.id)
                                .map(|x| x.members)
                                .sum();
                            Confirm::Accept(o.id, o.name.clone(), others)
                        });
                        ui.close();
                    }
                });
                let why = add_selection_why(has_sel);
                let label = c.t("options.add_selection");
                let add = ui
                    .add_enabled_ui(why.is_none(), |ui| Button::quiet(&label).show(ui, th))
                    .inner;
                let add = match why {
                    Some(k) => add.on_disabled_hover_text(c.t(k)),
                    None => add,
                };
                if add.clicked() {
                    acts.push(Act::AddSelection(o.id));
                }
            });
        });
    });
}

fn confirm_window(w: &mut OptionsWin, a: &EditorApp, ctx: &egui::Context, acts: &mut Vec<Act>) {
    let Some(cf) = w.confirm.clone() else {
        return;
    };
    let c = &a.sh.cat;
    let (title, text, label, act) = match &cf {
        Confirm::Accept(id, name, others) => (
            c.t("options.accept_title"),
            c.tf(
                "options.accept_text",
                &[("name", name), ("n", &others.to_string())],
            ),
            c.t("options.accept"),
            Act::Accept(*id),
        ),
        Confirm::Delete(id, name, n) => (
            c.t("options.delete_title"),
            c.tf(
                "options.delete_text",
                &[("name", name), ("n", &n.to_string())],
            ),
            c.t("options.delete"),
            Act::Delete(*id),
        ),
    };
    let mut done = false;
    let mut chosen = None;
    let destructive = matches!(cf, Confirm::Delete(..));
    let mut open = true;
    crate::ui_kit::Dialog::new("collab.options.confirm", title, crate::ui_kit::DialogSize::Confirm)
        .id(egui::Id::new("ddc_options_confirm"))
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(text);
            ui.add_space(a.sh.theme.d().gap);
            // a destructive action is never bound to Enter; accepting an option changes the
            // model, so it answers clicks only
            crate::ui_kit::dialog_footer(ui, |ui| {
                let go = if destructive {
                    crate::ui_kit::confirm_danger_button(ui, label, true).clicked()
                } else {
                    crate::ui_kit::primary_click(ui, label, None)
                };
                if go {
                    chosen = Some(());
                    done = true;
                }
                if crate::ui_kit::cancel_button(ui, c.t("options.cancel")).clicked() {
                    done = true;
                }
            });
        });
    done |= !open;
    if chosen.is_some() {
        acts.push(act);
    }
    if done {
        w.confirm = None;
    }
}

pub fn frame(w: &mut OptionsWin, a: &mut EditorApp, ctx: &egui::Context) {
    if !w.open {
        return;
    }
    let key = a.ed.cache_key();
    if w.rev != Some(key) {
        w.rev = Some(key);
        match a.ed.model.option_sets() {
            Ok(v) => w.sets = v,
            Err(e) => w.error = Some(e.to_string()),
        }
    }
    let mut acts = vec![];
    let mut open = w.open;
    let has_sel = !a.ed.selection.is_empty();
    let c = &a.sh.cat;
    crate::ui_kit::Dialog::new("collab.options", c.t("options.title"), crate::ui_kit::DialogSize::Wide)
        .id(egui::Id::new("ddc_options"))
        .open(&mut open)
        .show(ctx, |ui| {
            let th = ddc_ui::theme::of(ui.ctx());
            let th: &Theme = &th;
            if w.sets.is_empty() {
                crate::ui_kit::empty_note(
                    ui,
                    c.t("options.empty_title"),
                    Some(c.t("options.empty").as_str()),
                );
            }
            let (sets, typed, confirm) = (&w.sets, &mut w.new_option, &mut w.confirm);
            egui::ScrollArea::vertical()
                .id_salt("ddc.options.sets")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for set in sets {
                        let at = typed.iter().position(|x| x.0 == set.id).unwrap_or_else(|| {
                            typed.push((set.id, String::new()));
                            typed.len() - 1
                        });
                        set_group(ui, a, set, &mut typed[at].1, has_sel, &mut acts, confirm);
                        ui.add_space(space::SM);
                    }
                });
            ui.separator();
            crate::ui_kit::fixed_row(ui, "ddc.options.new_set_row", |ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                let hint = c.t("options.new_set");
                let out = TextField::new(&mut w.new_set)
                    .labelled_by(egui::Id::new("ddc.options.new_set.lbl"), &hint)
                    .hint(&hint)
                    .width(W_NEW_SET)
                    .id_salt("ddc.options.new_set")
                    .show(ui, th);
                out.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, hint.as_str())
                });
                let enter = out.committed && ui.input(|i| i.key_pressed(Key::Enter));
                let ready = !w.new_set.trim().is_empty();
                let label = c.t("options.add_set");
                let clicked = ui
                    .add_enabled_ui(ready, |ui| Button::primary(&label).show(ui, th))
                    .inner
                    .clicked();
                if submit(ready, clicked, enter) {
                    acts.push(Act::NewSet(
                        w.new_set.trim().to_string(),
                        c.t("options.first_name"),
                    ));
                    w.new_set.clear();
                }
                let label = c.t("options.to_main");
                let moved = ui
                    .add_enabled_ui(has_sel, |ui| Button::secondary(&label).show(ui, th))
                    .inner;
                let moved = if has_sel {
                    moved.on_hover_text(c.t("options.to_main_hint"))
                } else {
                    moved.on_disabled_hover_text(c.t("options.select_first"))
                };
                if moved.clicked() {
                    acts.push(Act::RemoveSelection);
                }
            });
            ui.add_space(space::XS);
            ui.label(th.rich(Role::Caption, &c.t("options.limit")));
            if let Some(e) = &w.error {
                crate::ui_kit::error_note(ui, e);
            }
        });
    w.open = open;
    confirm_window(w, a, ctx, &mut acts);
    for act in acts {
        w.error = perform(w, a, act).err().map(|e| e.to_string());
    }
}

/// One frame of the window for the headless layout probe; the window state is put back.
#[cfg(test)]
pub(crate) fn layout_frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = std::mem::take(&mut a.collab.options);
    frame(&mut w, a, ctx);
    a.collab.options = w;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_selection_reason() {
        assert_eq!(add_selection_why(false), Some("options.select_first"));
        assert_eq!(add_selection_why(true), None);
    }

    const NEW_KEYS: [&str; 9] = [
        PRIMARY_TIP,
        "options.select_first",
        "options.show_plan",
        "options.show_3d",
        "options.primary_named",
        "options.more",
        "options.accept_more",
        "options.delete_more",
        "options.empty_title",
    ];

    #[test]
    fn option_keys_in_en_and_ru() {
        let en = include_str!("../../../ui/data/i18n/en/options.json");
        let ru = include_str!("../../../ui/data/i18n/ru/options.json");
        for k in NEW_KEYS {
            let pat = format!("\"{k}\":");
            assert!(en.contains(&pat) && ru.contains(&pat), "{k}");
        }
        // the old caption of the radio is gone with the radio
        let old = format!("options.{}", "primary_mark");
        assert!(!en.contains(&old) && !ru.contains(&old));
    }

    #[test]
    fn named_controls_carry_the_option_name_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for k in ["options.show_plan", "options.show_3d", "options.primary_named", "options.more"] {
                let s = c.tf(k, &[("name", "Variant A")]);
                assert!(s.contains("Variant A"), "{k} {lang:?}: {s}");
            }
        }
    }

    #[test]
    fn window_is_built_on_kit_widgets_only() {
        let src = include_str!("options_win.rs");
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for raw in [
            "egui::Button",
            "egui::TextEdit",
            "egui::Grid",
            "colored_label",
            ".radio(",
            ".button(",
            "ui.group(",
            "desired_width(",
        ] {
            assert!(!code.contains(raw), "raw widget in options_win.rs: {raw}");
        }
    }

    #[test]
    fn overflow_menu_ends_with_the_destructive_row_only() {
        let danger: Vec<usize> = MENU
            .iter()
            .enumerate()
            .filter(|(_, (_, t))| *t == MenuTone::Danger)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(danger, vec![MENU.len() - 1]);
        assert_eq!(MENU[danger[0]].0, "options.delete_more");
    }

    #[test]
    fn enter_and_button_add_only_a_named_entry() {
        assert!(submit(true, true, false));
        assert!(submit(true, false, true));
        assert!(!submit(false, true, true), "blank name adds nothing");
        assert!(!submit(true, false, false));
    }

    #[test]
    fn name_column_never_collapses() {
        assert!(name_w(0.0) >= W_NAME_MIN);
        assert!(name_w(1000.0) > name_w(700.0));
    }
}
