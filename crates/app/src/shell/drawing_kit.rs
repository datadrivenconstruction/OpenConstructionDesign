//! Drawing underlay window on the design kit: pure helpers (layer toggles, project-unit
//! fields, delete confirm) and the kit-based rows the window draws with.

use crate::drawing::LayerState;
use crate::units::{Units, FOOT};
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, Button, NumberField, NumberFormat};
use ddc_ui::Theme;
use egui::{self, Align, Layout, RichText};

/// Layer rows visible before the list scrolls.
const LAYER_ROWS: f32 = 10.0;
/// Width of the count column and of a number field.
const COUNT_W: f32 = space::XXXL;
const NUM_W: f32 = 96.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    View,
    Frozen,
    Halftone,
}

/// One icon toggle of a layer row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toggle {
    pub flag: Flag,
    pub on: bool,
    pub icon: &'static str,
}

/// The three toggles of a layer row, in column order.
pub fn layer_cells(l: &LayerState) -> [Toggle; 3] {
    [
        Toggle {
            flag: Flag::View,
            on: l.visible,
            // open eye when shown, closed eye when hidden: the state reads without the fill
            icon: if l.visible { "reveal" } else { "hide" },
        },
        Toggle {
            flag: Flag::Frozen,
            on: l.frozen,
            icon: "pin",
        },
        Toggle {
            flag: Flag::Halftone,
            on: l.halftone,
            // half-shaded disc; the x-ray style icon means a 3D view style elsewhere
            icon: "shading",
        },
    ]
}

pub fn flip(l: &mut LayerState, f: Flag) {
    match f {
        Flag::View => l.visible = !l.visible,
        Flag::Frozen => l.frozen = !l.frozen,
        Flag::Halftone => l.halftone = !l.halftone,
    }
}

/// Tooltip and accessible name of a toggle: "<layer>: visible".
pub fn toggle_tip(ru: bool, layer: &str, t: &Toggle) -> String {
    let key = match (t.flag, t.on) {
        (Flag::View, true) => "drawing_kit.tip.view_on",
        (Flag::View, false) => "drawing_kit.tip.view_off",
        (Flag::Frozen, true) => "drawing_kit.tip.frozen_on",
        (Flag::Frozen, false) => "drawing_kit.tip.frozen_off",
        (Flag::Halftone, true) => "drawing_kit.tip.half_on",
        (Flag::Halftone, false) => "drawing_kit.tip.half_off",
    };
    crate::tr_lang(ru, key, &[("layer", layer)])
}

/// A length shown in project units; the stored value stays in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct LenUnit {
    /// Display units per metre.
    pub factor: f64,
    pub unit: String,
    pub decimals: usize,
    pub step: f64,
}

pub fn len_unit(u: Units, ru: bool) -> LenUnit {
    match u {
        Units::Metric => LenUnit {
            factor: 1000.0,
            unit: crate::tr_lang(ru, "unit_fmt.unit.mm", &[]),
            decimals: 1,
            step: 10.0,
        },
        Units::Imperial => LenUnit {
            factor: 1.0 / FOOT,
            unit: crate::tr_lang(ru, "drawing_kit.unit.ft", &[]),
            decimals: 3,
            step: 0.5,
        },
    }
}

pub fn number_format(ru: bool) -> NumberFormat {
    if ru {
        NumberFormat::RU
    } else {
        NumberFormat::EN
    }
}

/// Russian-style "1 250,5 mm" (space grouping, decimal comma) or "1,250.5 mm" for a length in metres.
pub fn fmt_len(m: f64, u: Units, ru: bool) -> String {
    let lu = len_unit(u, ru);
    let v = widgets::field::format_number(m * lu.factor, lu.decimals, number_format(ru));
    format!("{v} {}", lu.unit)
}

/// Move / rotate field: label, unit text and step.
#[derive(Clone, Debug, PartialEq)]
pub struct XfField {
    pub label: String,
    pub unit: String,
    pub decimals: usize,
    pub step: f64,
    pub angle: bool,
}

pub fn xf_fields(u: Units, ru: bool) -> [XfField; 3] {
    let lu = len_unit(u, ru);
    let len = |key: &str| XfField {
        label: crate::tr_lang(ru, key, &[]),
        unit: lu.unit.clone(),
        decimals: lu.decimals,
        step: lu.step,
        angle: false,
    };
    [
        len("drawing_kit.axis_x"),
        len("drawing_kit.axis_y"),
        XfField {
            label: crate::tr_lang(ru, "drawing_kit.angle", &[]),
            unit: crate::tr_lang(ru, "drawing_kit.unit.deg", &[]),
            decimals: 1,
            step: 1.0,
            angle: true,
        },
    ]
}

/// Delete confirmation: armed for the count it was asked for, stale when the pick changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Ask,
    Confirm,
    Cancel,
}

/// Returns the new armed count and whether to delete now.
pub fn gate_step(armed: Option<usize>, n: usize, ev: Option<Gate>) -> (Option<usize>, bool) {
    if n == 0 {
        return (None, false);
    }
    let armed = armed.filter(|a| *a == n);
    match ev {
        Some(Gate::Ask) => (Some(n), false),
        Some(Gate::Confirm) if armed.is_some() => (None, true),
        Some(Gate::Confirm) | Some(Gate::Cancel) => (None, false),
        None => (armed, false),
    }
}

fn icon_toggle(ui: &mut egui::Ui, th: &Theme, icon: &str, on: bool, tip: &str) -> egui::Response {
    let ctx = ui.ctx().clone();
    widgets::with_icons(&ctx, |icons| widgets::icon_button(ui, icons, th, icon, on, tip))
}

/// Length field in project units over a value stored in metres, named by the caller's visible
/// `label` (widget id and text). Returns true when edited.
#[allow(clippy::too_many_arguments)]
pub fn len_field(
    ui: &mut egui::Ui,
    th: &Theme,
    salt: impl std::hash::Hash,
    label: (egui::Id, &str),
    value_m: &mut f64,
    units: Units,
    ru: bool,
    range_m: Option<(f64, f64)>,
) -> bool {
    let lu = len_unit(units, ru);
    let shown = *value_m * lu.factor;
    let mut v = shown;
    let mut f = NumberField::new(salt, &mut v, number_format(ru))
        .labelled_by(label.0, label.1)
        .decimals(lu.decimals)
        .step(lu.step)
        .unit(&lu.unit)
        .width(NUM_W);
    if let Some((lo, hi)) = range_m {
        f = f.range(lo * lu.factor, hi * lu.factor);
    }
    f.show(ui, th);
    // write back only a real edit, so an untouched field never drifts by rounding
    if v != shown {
        *value_m = v / lu.factor;
        true
    } else {
        false
    }
}

/// X / Y / angle fields of the move and rotate row.
pub fn xf_row(
    ui: &mut egui::Ui,
    th: &Theme,
    xf: &mut (f64, f64, f64),
    units: Units,
    ru: bool,
) {
    let fields = xf_fields(units, ru);
    let s = th.sem();
    ui.horizontal(|ui| {
        for (i, f) in fields.iter().enumerate() {
            let lr = ui.label(RichText::new(&f.label).color(s.text_secondary));
            if f.angle {
                NumberField::new(("dwg_xf", i), &mut xf.2, number_format(ru))
                    .labelled_by(lr.id, &f.label)
                    .decimals(f.decimals)
                    .step(f.step)
                    .unit(&f.unit)
                    .width(NUM_W)
                    .show(ui, th);
            } else {
                let v = if i == 0 { &mut xf.0 } else { &mut xf.1 };
                len_field(ui, th, ("dwg_xf", i), (lr.id, &f.label), v, units, ru, None);
            }
        }
    });
}

/// Layer list: header, icon toggles, colour swatch with explicit reset, elided name, count.
pub fn layers_table(ui: &mut egui::Ui, th: &Theme, layers: &mut [LayerState], ru: bool) -> bool {
    let s = th.sem();
    let d = th.d();
    let nf = number_format(ru);
    let mut changed = false;
    if layers.is_empty() {
        let hint = crate::tr_lang(ru, "drawing_kit.no_layers_hint", &[]);
        crate::ui_kit::empty_note(ui, crate::tr_lang(ru, "drawing_kit.no_layers", &[]), Some(&hint));
        return false;
    }
    let gap = ui.spacing().item_spacing.x;
    let slot = d.control + 4.0;
    let lead = 4.0 * slot + ui.spacing().interact_size.x + 4.0 * gap;
    ui.horizontal(|ui| {
        ui.set_min_height(d.row);
        ui.add_space(lead);
        let w = (ui.available_width() - COUNT_W - gap).max(0.0);
        let head = |t: String| RichText::new(t).font(th.font(Role::TableHeader)).color(s.text_secondary);
        ui.allocate_ui_with_layout(
            egui::vec2(w, d.row),
            Layout::left_to_right(Align::Center),
            |ui| ui.label(head(crate::tr_lang(ru, "drawing_kit.col.layer", &[]))),
        );
        ui.allocate_ui_with_layout(
            egui::vec2(COUNT_W, d.row),
            Layout::right_to_left(Align::Center),
            |ui| ui.label(head(crate::tr_lang(ru, "drawing_kit.col.objects", &[]))),
        );
    });
    egui::ScrollArea::vertical()
        .id_salt("dwg_layers")
        .auto_shrink([false, true])
        .max_height(d.row * LAYER_ROWS)
        .show(ui, |ui| {
            for (i, l) in layers.iter_mut().enumerate() {
                ui.push_id(i, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_min_height(d.row);
                        for t in layer_cells(l) {
                            let tip = toggle_tip(ru, &l.name, &t);
                            if icon_toggle(ui, th, t.icon, t.on, &tip).clicked() {
                                flip(l, t.flag);
                                changed = true;
                            }
                        }
                        let mut c = l.color.or(l.file_color).unwrap_or([200, 200, 200]);
                        if ui.color_edit_button_srgb(&mut c).changed() {
                            l.color = Some(c);
                            changed = true;
                        }
                        let tip = crate::tr_lang(ru, "drawing_kit.reset_color", &[("layer", &l.name)]);
                        if widgets::glyph_button(ui, "undo", l.color.is_some(), &tip).clicked() {
                            l.color = None;
                            changed = true;
                        }
                        let w = (ui.available_width() - COUNT_W - gap).max(0.0);
                        let ink = if l.shown() { s.text_primary } else { s.text_secondary };
                        ui.allocate_ui_with_layout(
                            egui::vec2(w, d.row),
                            Layout::left_to_right(Align::Center),
                            |ui| widgets::elided_label(ui, &l.name, th.font(Role::Body), ink),
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(COUNT_W, d.row),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                let n = widgets::field::format_number(l.count as f64, 0, nf);
                                ui.label(
                                    RichText::new(n)
                                        .font(th.font(Role::Body))
                                        .color(s.text_secondary),
                                )
                            },
                        );
                    });
                });
            }
        });
    changed
}

/// "Show all" plus a menu with the other bulk layer actions. Returns true when layers changed.
pub fn layer_actions(ui: &mut egui::Ui, th: &Theme, layers: &mut [LayerState], ru: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if Button::secondary(&crate::tr_lang(ru, "drawing_kit.show_all", &[]))
            .show(ui, th)
            .clicked()
        {
            layers.iter_mut().for_each(|l| l.visible = true);
            changed = true;
        }
        let tip = crate::tr_lang(ru, "drawing_kit.more_layers", &[]);
        let more = icon_toggle(ui, th, "context-menu", false, &tip);
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(control::DROPDOWN_W);
            let item = |ui: &mut egui::Ui, key: &str| {
                widgets::menu_item(
                    ui,
                    th,
                    &crate::tr_lang(ru, key, &[]),
                    "",
                    widgets::MenuTone::Normal,
                    true,
                )
                .clicked()
            };
            if item(ui, "drawing_kit.hide_all") {
                layers.iter_mut().for_each(|l| l.visible = false);
                changed = true;
                ui.close();
            }
            if item(ui, "drawing_kit.thaw_all") {
                layers.iter_mut().for_each(|l| l.frozen = false);
                changed = true;
                ui.close();
            }
        });
    });
    changed
}

/// Destructive "Delete selected": the first click asks, the second confirms. Returns true
/// when the person confirmed.
pub fn delete_row(ui: &mut egui::Ui, th: &Theme, n: usize, ru: bool) -> bool {
    let id = ui.id().with("dwg_delete_gate");
    let armed: Option<usize> = ui.data(|d| d.get_temp::<usize>(id)).filter(|a| *a > 0);
    let mut ev = None;
    ui.horizontal(|ui| {
        if armed.is_some() && armed == Some(n) {
            ui.label(
                RichText::new(crate::tr_lang(ru, "drawing_kit.delete_ask", &[("n", &n.to_string())]))
                    .color(th.sem().text_secondary),
            );
            if Button::danger(&crate::tr_lang(ru, "drawing_kit.delete_confirm", &[]))
                .show(ui, th)
                .clicked()
            {
                ev = Some(Gate::Confirm);
            }
            if Button::quiet(&crate::tr_lang(ru, "drawing_kit.cancel", &[]))
                .show(ui, th)
                .clicked()
            {
                ev = Some(Gate::Cancel);
            }
        } else {
            let label = crate::tr_lang(ru, "drawing_kit.delete_selected", &[("n", &n.to_string())]);
            let r = ui
                .add_enabled_ui(n > 0, |ui| Button::danger(&label).show(ui, th))
                .inner;
            if r.clicked() {
                ev = Some(Gate::Ask);
            }
        }
    });
    let (next, go) = gate_step(armed, n, ev);
    ui.data_mut(|d| d.insert_temp(id, next.unwrap_or(0)));
    go
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str) -> LayerState {
        LayerState {
            name: name.into(),
            visible: true,
            frozen: false,
            file_color: None,
            color: None,
            halftone: false,
            count: 3,
        }
    }

    #[test]
    fn layer_cells_are_three_named_toggles() {
        let mut l = layer("A-WALL");
        l.frozen = true;
        let c = layer_cells(&l);
        assert_eq!(
            c.map(|t| (t.flag, t.on)),
            [(Flag::View, true), (Flag::Frozen, true), (Flag::Halftone, false)]
        );
        for t in c {
            assert!(ddc_ui::icons::source(t.icon).is_some(), "icon {}", t.icon);
        }
        assert_eq!(c[0].icon, "reveal");
        l.visible = false;
        assert_eq!(layer_cells(&l)[0].icon, "hide", "a hidden layer shows the closed eye");
        assert_eq!(layer_cells(&l)[2].icon, "shading");
    }

    #[test]
    fn toggle_tips_carry_the_layer_name_in_both_languages() {
        let l = layer("A-WALL");
        for ru in [false, true] {
            let mut seen = Vec::new();
            for on in [false, true] {
                for mut t in layer_cells(&l) {
                    t.on = on;
                    let tip = toggle_tip(ru, "A-WALL", &t);
                    assert!(tip.contains("A-WALL"), "{tip}");
                    assert!(!tip.contains("drawing_kit."), "missing key: {tip}");
                    seen.push(tip);
                }
            }
            let n = seen.len();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), n, "tips must differ per flag and state");
        }
        let en = toggle_tip(false, "L", &layer_cells(&l)[0]);
        let ru = toggle_tip(true, "L", &layer_cells(&l)[0]);
        assert_ne!(en, ru);
    }

    #[test]
    fn flip_changes_only_its_flag() {
        let mut l = layer("x");
        flip(&mut l, Flag::Halftone);
        assert!(l.halftone && l.visible && !l.frozen);
        flip(&mut l, Flag::View);
        assert!(!l.visible);
    }

    #[test]
    fn lengths_follow_project_units_and_language() {
        assert_eq!(fmt_len(1.2505, Units::Metric, false), "1,250.5 mm");
        assert_eq!(
            fmt_len(1.2505, Units::Metric, true),
            "1\u{a0}250,5 \u{43c}\u{43c}"
        );
        assert_eq!(fmt_len(FOOT * 2.0, Units::Imperial, false), "2.000 ft");
        assert_eq!(
            fmt_len(FOOT * 2.0, Units::Imperial, true),
            "2,000 \u{444}\u{442}"
        );
        // a unit is never the hard-coded metre
        for u in [Units::Metric, Units::Imperial] {
            for ru in [false, true] {
                let unit = len_unit(u, ru).unit;
                assert!(unit.trim() != "m" && unit.trim() != "\u{43c}", "{unit}");
            }
        }
    }

    #[test]
    fn xf_fields_units_match_settings() {
        for ru in [false, true] {
            let m = xf_fields(Units::Metric, ru);
            let i = xf_fields(Units::Imperial, ru);
            assert_eq!(m[0].unit, len_unit(Units::Metric, ru).unit);
            assert_eq!(i[1].unit, len_unit(Units::Imperial, ru).unit);
            assert_ne!(m[0].unit, i[0].unit);
            assert!(m[2].angle && m[2].unit.contains('°'));
            assert!(m.iter().chain(i.iter()).all(|f| !f.label.contains("drawing_kit.")));
            assert!(m.iter().chain(i.iter()).all(|f| !f.unit.contains("drawing_kit.")));
        }
        assert_eq!(
            xf_fields(Units::Metric, true)[2].label,
            "\u{423}\u{433}\u{43e}\u{43b}"
        );
        assert_eq!(xf_fields(Units::Metric, false)[2].label, "Angle");
    }

    #[test]
    fn delete_needs_a_second_click_for_the_same_count() {
        assert_eq!(gate_step(None, 3, Some(Gate::Ask)), (Some(3), false));
        assert_eq!(gate_step(Some(3), 3, None), (Some(3), false));
        assert_eq!(gate_step(Some(3), 3, Some(Gate::Confirm)), (None, true));
        assert_eq!(gate_step(Some(3), 3, Some(Gate::Cancel)), (None, false));
        // the pick changed after asking: the ask is stale and never deletes
        assert_eq!(gate_step(Some(3), 4, Some(Gate::Confirm)), (None, false));
        assert_eq!(gate_step(Some(3), 4, None), (None, false));
        // nothing picked: nothing to arm or delete
        assert_eq!(gate_step(None, 0, Some(Gate::Ask)), (None, false));
        // a confirm without an ask deletes nothing
        assert_eq!(gate_step(None, 3, Some(Gate::Confirm)), (None, false));
    }

    #[test]
    fn confirmed_delete_is_one_undo_step() {
        use crate::editor::Editor;
        use crate::shell::EditorApp;
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        let level = a.ed.level;
        a.plan.drawings.add(
            &ddc_io::drawing_model::fixture::sample_plan(0),
            "p",
            None,
            level,
            None,
            crate::drawing::Origin::AsIs,
            ddc_geom::v2(0.0, 0.0),
        );
        let before = a.plan.drawings.list[0].u.clone();
        let n = before.lines.len();
        assert!(n > 0);
        a.plan.drawings.pick = Some((
            0,
            ddc_io::drawing_model::Pick {
                lines: (0..n).collect(),
                ..Default::default()
            },
        ));
        // ask, then confirm: one deletion
        let (armed, go) = gate_step(None, n, Some(Gate::Ask));
        assert!(!go);
        let (_, go) = gate_step(armed, n, Some(Gate::Confirm));
        assert!(go);
        a.plan.drawings.delete_pick();
        a.plan.drawings.sync_undo(&mut a.ed);
        assert!(a.plan.drawings.list[0].u.lines.len() < n);
        // one undo restores everything the single delete removed
        assert!(a.run("edit.undo"));
        assert_eq!(a.plan.drawings.list[0].u, before);
    }

    #[test]
    fn empty_and_all_strings_exist_in_both_languages() {
        for ru in [false, true] {
            for key in [
                "drawing_kit.no_layers",
                "drawing_kit.no_layers_hint",
                "drawing_kit.no_classes",
                "drawing_kit.no_classes_hint",
                "drawing_kit.show_all",
                "drawing_kit.hide_all",
                "drawing_kit.thaw_all",
                "drawing_kit.more_layers",
                "drawing_kit.delete_confirm",
                "drawing_kit.cancel",
                "drawing_kit.apply_drawing",
                "drawing_kit.apply_picked",
                "drawing_kit.mode_move",
                "drawing_kit.mode_copy",
                "drawing_kit.n_of_m",
                "drawing_kit.accept_class",
                "drawing_kit.reject_class",
            ] {
                let t = crate::tr_lang(ru, key, &[("n", "1"), ("m", "2")]);
                assert!(!t.is_empty() && t != key, "{key} ru={ru}");
            }
        }
    }

    #[test]
    fn deleted_toast_text_names_the_count_and_undo() {
        let en = crate::tr_lang(false, "drawing_kit.deleted", &[("n", "5")]);
        let ru = crate::tr_lang(true, "drawing_kit.deleted", &[("n", "5")]);
        assert!(en.contains('5') && ru.contains('5'));
        assert_ne!(en, ru);
        assert!(!crate::tr_lang(false, "drawing_kit.undo", &[]).is_empty());
    }

    #[test]
    fn window_has_no_raw_widgets() {
        let win = include_str!("drawing_win.rs");
        let kit = include_str!("drawing_kit.rs");
        let a = win.find("fn drawing_layers_window").unwrap();
        let b = win.find("\"dlg.dw.elements\"").unwrap();
        let kit_src = kit.split("#[cfg(test)]").next().unwrap();
        for (name, src) in [("window", &win[a..b]), ("kit", kit_src)] {
            for bad in [
                "egui::ComboBox",
                "egui::DragValue",
                "ui.checkbox(",
                ".small_button(",
                "ui.button(",
                "egui::Grid",
            ] {
                assert!(!src.contains(bad), "{name}: {bad}");
            }
            for (i, _) in src.match_indices("max_height(") {
                let next = src[i + "max_height(".len()..].chars().next().unwrap();
                assert!(!next.is_ascii_digit(), "{name}: literal max_height");
            }
        }
    }
}
