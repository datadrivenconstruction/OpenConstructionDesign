// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tool options row under the ribbon, built from the design-kit widgets only. `opt_groups` decides
//! which groups a tool shows (fixed order, so nothing reflows when the tool changes); the field
//! helpers keep the old semantics: model units, lower floor and upper limit per value.
use crate::editor::{CurtainDefaults, Editor};
use crate::opening_tool::{Kind, OpeningType};
use crate::units::{edit_length, parse_edit_length, unit_label, Units};
use crate::wall_tool::Mode;
use ddc_ui::icons::IconCache;
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::type_pick::{Pick, TypePick};
use ddc_ui::widgets::{self, field::TextField};
use ddc_ui::Theme;

/// Numeric values of the options row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Field {
    CwU,
    CwV,
    MullionW,
    MullionD,
    OpW,
    OpH,
    OpSill,
    WallH,
}

impl Field {
    pub(super) const ALL: [Field; 8] = [
        Field::CwU,
        Field::CwV,
        Field::MullionW,
        Field::MullionD,
        Field::OpW,
        Field::OpH,
        Field::OpSill,
        Field::WallH,
    ];

    /// Short caption before the field.
    pub(super) fn label_key(self) -> &'static str {
        match self {
            Field::CwU => "opt.cw_u",
            Field::CwV => "opt.cw_v",
            Field::MullionW => "opt.mullion",
            Field::MullionD => "length_input.opt.times",
            Field::OpW => "type_dialog.panels.w_short",
            Field::OpH | Field::WallH => "type_dialog.panels.h_short",
            Field::OpSill => "type_dialog.panels.sill",
        }
    }

    pub(super) fn tip_key(self) -> &'static str {
        match self {
            Field::CwU => "opt.cw_u_tip",
            Field::CwV => "opt.cw_v_tip",
            Field::MullionW => "opt.mullion_w",
            Field::MullionD => "opt.mullion_d",
            Field::OpW => "type_dialog.panels.width",
            Field::OpH => "type_dialog.panels.height",
            Field::OpSill => "type_dialog.panels.sill",
            Field::WallH => "type_dialog.panels.new_wall_height",
        }
    }

    /// (floor, limit) in model metres: the value never leaves this range.
    pub(super) fn range(self) -> (f64, f64) {
        match self {
            Field::CwU | Field::CwV => (0.1, 20.0),
            Field::MullionW => (0.01, 0.5),
            Field::MullionD => (0.01, 1.0),
            Field::OpW | Field::OpH => (0.05, 20.0),
            Field::OpSill => (0.0, 20.0),
            Field::WallH => (0.1, 100.0),
        }
    }

    /// The value this field edits.
    pub(super) fn slot<'a>(
        self,
        cw: &'a mut CurtainDefaults,
        ty: &'a mut OpeningType,
        wall_h: &'a mut f64,
    ) -> &'a mut f64 {
        match self {
            Field::CwU => &mut cw.u,
            Field::CwV => &mut cw.v,
            Field::MullionW => &mut cw.mullion.0,
            Field::MullionD => &mut cw.mullion.1,
            Field::OpW => &mut ty.width,
            Field::OpH => &mut ty.height,
            Field::OpSill => &mut ty.sill,
            Field::WallH => wall_h,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Item {
    Arc,
    /// Spline wall (walls only: a curtain wall has no spline axis).
    Spline,
    Num(Field),
    Level,
    WallType,
    Units,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GroupId {
    Arc,
    Curtain,
    Opening,
    Level,
    Wall,
    Units,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Group {
    pub id: GroupId,
    pub items: Vec<Item>,
}

/// What the active tool is, for the options row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolKind {
    /// Selection, or a tool with its own option bar elsewhere.
    Select,
    Wall,
    Curtain,
    /// `sill`: the opening has a sill height (windows and voids, not doors).
    Opening { sill: bool },
}

pub(super) fn tool_kind(
    mode: Mode,
    curtain: bool,
    opening: Option<Kind>,
    wall_opts: bool,
) -> ToolKind {
    if let Some(k) = opening {
        return ToolKind::Opening {
            sill: k != Kind::Door,
        };
    }
    if !wall_opts {
        return ToolKind::Select;
    }
    if curtain {
        ToolKind::Curtain
    } else if mode == Mode::Select {
        ToolKind::Select
    } else {
        ToolKind::Wall
    }
}

/// Groups in fixed order: arc, curtain, opening, level, wall, units. Empty ones are left out.
pub(super) fn opt_groups(
    mode: Mode,
    curtain: bool,
    opening: Option<Kind>,
    wall_opts: bool,
) -> Vec<Group> {
    let kind = tool_kind(mode, curtain, opening, wall_opts);
    let mut v = Vec::new();
    let mut push = |id, items: Vec<Item>| v.push(Group { id, items });
    if matches!(kind, ToolKind::Wall | ToolKind::Curtain) {
        let mut items = vec![Item::Arc];
        if kind == ToolKind::Wall {
            items.push(Item::Spline);
        }
        push(GroupId::Arc, items);
    }
    if kind == ToolKind::Curtain {
        push(
            GroupId::Curtain,
            [Field::CwU, Field::CwV, Field::MullionW, Field::MullionD]
                .map(Item::Num)
                .to_vec(),
        );
    }
    if let ToolKind::Opening { sill } = kind {
        let mut items = vec![Item::Num(Field::OpW), Item::Num(Field::OpH)];
        if sill {
            items.push(Item::Num(Field::OpSill));
        }
        push(GroupId::Opening, items);
    }
    push(GroupId::Level, vec![Item::Level]);
    if matches!(kind, ToolKind::Wall | ToolKind::Curtain) {
        push(
            GroupId::Wall,
            vec![Item::WallType, Item::Num(Field::WallH)],
        );
    }
    push(GroupId::Units, vec![Item::Units]);
    v
}

/// Text of a length in the field.
pub(super) fn format_value(v: f64, u: Units) -> String {
    edit_length(v, u)
}

/// Typed text to a value inside the field range; `None` when it is not a length.
pub(super) fn parse_value(s: &str, u: Units, f: Field) -> Option<f64> {
    let (lo, hi) = f.range();
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let zero = has_digit && s.chars().filter(char::is_ascii_digit).all(|c| c == '0');
    if zero && lo <= 0.0 {
        return Some(0.0);
    }
    parse_edit_length(s, u).map(|x| x.clamp(lo, hi))
}

/// Unit shown muted inside a length field: empty in feet-inches (the text carries it).
pub(super) fn unit_text(u: Units, ru: bool) -> String {
    match u {
        Units::Metric => crate::tr_lang(ru, "unit_fmt.unit.mm", &[]),
        Units::Imperial => unit_label(u).to_string(),
    }
}

/// Level rows top-down: (id, «name   +3,000»).
pub(super) fn level_items(
    levels: &[(i64, String, f64)],
    units: Units,
    ru: bool,
) -> Vec<(i64, String)> {
    levels
        .iter()
        .rev()
        .map(|(id, n, z)| {
            let mark = crate::editor::levels::elevation_mark(*z, units, ru);
            (*id, format!("{n}   {mark}"))
        })
        .collect()
}

fn tr(ru: bool, key: &str) -> String {
    crate::tr_lang(ru, key, &[])
}

/// Width of a length field.
fn field_w() -> f32 {
    control::FIELD_MIN_W + space::XXL
}

/// Room a unit takes beside its field (the kit puts it outside the frame), so the frame keeps
/// the width of the longest value.
fn unit_w(ui: &egui::Ui, th: &Theme, unit: &str) -> f32 {
    if unit.is_empty() {
        0.0
    } else {
        widgets::form::text_w(ui, th, unit) + space::SM
    }
}

/// Field caption of the bar: the kit form label, in the field's own font and centre line.
fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    let max = ui.available_width();
    widgets::form::label(ui, th, text, max)
}

/// Active tool name: plain strong text, no fill and no frame, so it never reads as a button.
pub(super) fn name(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    widgets::elided_label(ui, text, th.font(Role::Label), th.sem().text_primary)
}

/// Label and a length field; true when the value was committed.
fn num(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    u: Units,
    f: Field,
    value: &mut f64,
) -> bool {
    let label = tr(ru, f.label_key());
    let tip = tr(ru, f.tip_key());
    let err = tr(ru, "length_input.opt.bad_length");
    let unit = unit_text(u, ru);
    let mut changed = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        let lr = caption(ui, th, &label);
        widgets::rich_tooltip(lr.clone(), th, &tip, "", "");
        let buf_id = ui.id().with(("length_input.opt", f)).with("buf");
        let stored = ui.data(|d| d.get_temp::<String>(buf_id));
        let mut text = stored
            .clone()
            .unwrap_or_else(|| format_value(*value, u));
        let bad = stored.is_some() && parse_value(&text, u, f).is_none();
        let out = TextField::new(&mut text)
            .id_salt(("length_input.opt", f))
            .labelled_by(lr.id, &label)
            .error_text(&err)
            .error(bad)
            .unit(&unit)
            .right_aligned()
            .width(field_w() + unit_w(ui, th, &unit))
            .show(ui, th);
        widgets::rich_tooltip(out.response.clone(), th, &tip, "", "");
        if out.committed && !out.cancelled {
            if let Some(x) = parse_value(&text, u, f) {
                if x != *value {
                    *value = x;
                    changed = true;
                }
            }
        }
        if out.response.has_focus() {
            ui.data_mut(|d| d.insert_temp(buf_id, text));
        } else {
            ui.data_mut(|d| d.remove_temp::<String>(buf_id));
        }
    });
    changed
}

/// Square toggle at control height with an icon; `desc` is the tooltip line under the title.
fn icon_toggle(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut IconCache,
    icon: &str,
    on: bool,
    tip: &str,
    desc: &str,
) -> egui::Response {
    let c = th.d().control;
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(c), egui::Sense::click());
    let col = widgets::paint_state(ui, th, rect, &resp, on);
    let ic = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(th.d().icon));
    icons.paint(ui, &widgets::icon_name(icon, on), th.icon_px(), ic, col);
    let (title, keys) = widgets::split_tip(tip);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, title));
    widgets::rich_tooltip(resp, th, title, desc, keys)
}

fn divider(ui: &mut egui::Ui, th: &Theme) {
    widgets::vdivider(ui, th, th.d().control);
}

/// Draws the groups of `opt_groups` for the editor's current tool.
pub(super) fn show(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut IconCache,
    ru: bool,
    ed: &mut Editor,
    groups: &[Group],
) {
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            divider(ui, th);
        }
        for item in &g.items {
            match *item {
                Item::Arc => {
                    let on = ed.tool.arc && ed.wall_mode() == Mode::Polyline && !ed.opening_mode;
                    let tip = tr(ru, "opt.arc_tip");
                    if icon_toggle(ui, th, icons, "fillet", on, &tip, "").clicked() {
                        let cw = ed.tool.curtain;
                        ed.set_mode(Mode::Polyline);
                        ed.tool.curtain = cw;
                        ed.tool.arc = !on;
                        ed.tool.spline &= on;
                    }
                }
                Item::Spline => {
                    let on = ed.tool.spline && ed.wall_mode() == Mode::Polyline && !ed.opening_mode;
                    let (name, tip) = (tr(ru, "spline.opt.spline"), tr(ru, "spline.opt.spline_tip"));
                    if icon_toggle(ui, th, icons, "freeform", on, &name, &tip).clicked() {
                        ed.set_mode(Mode::Polyline);
                        ed.tool.spline = !on;
                        ed.tool.arc &= on;
                    }
                }
                Item::Num(f) => {
                    let u = ed.tool.units;
                    let slot = f.slot(&mut ed.cw_defaults, &mut ed.otool.ty, &mut ed.height);
                    num(ui, th, ru, u, f, &mut *slot);
                    let (lo, hi) = f.range();
                    *slot = slot.clamp(lo, hi);
                }
                Item::Level => level(ui, th, ru, ed),
                Item::WallType => wall_type(ui, th, ru, ed),
                Item::Units => units(ui, th, ru, ed),
            }
        }
    }
}

pub(super) fn level(ui: &mut egui::Ui, th: &Theme, ru: bool, ed: &mut Editor) {
    let rows = level_items(&ed.levels(), ed.tool.units, ru);
    let names: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    let mut sel = rows.iter().position(|r| r.0 == ed.level).unwrap_or(0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        caption(ui, th, &tr(ru, "opt.level"));
        let r = widgets::field::dropdown(ui, th, "ux46a.level", &mut sel, &names, None);
        r.clone().on_hover_text(tr(ru, "type_dialog.panels.level_tip"));
        if r.changed() {
            if let Some(row) = rows.get(sel) {
                ed.set_level(row.0);
            }
        }
    });
}

fn wall_type(ui: &mut egui::Ui, th: &Theme, ru: bool, ed: &mut Editor) {
    let rows = super::type_items::items(&ed.types, ed.tool.units, ru);
    let cur = ed.types.get(ed.current_type).map(|t| t.id);
    let mut pick = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        caption(ui, th, &tr(ru, "opt.type"));
        pick = TypePick::new("ux48b.wtype", &rows, cur)
            .texts(super::type_items::texts(ru))
            .hover(tr(ru, "type_dialog.panels.wall_type"))
            .show(ui, th);
    });
    match pick {
        Some(Pick::Id(id)) => {
            if let Some(i) = ed.types.iter().position(|t| t.id == id) {
                ed.current_type = i;
            }
        }
        Some(Pick::EditType) => {
            if let Some(id) = cur {
                super::with_type_dialog(ed, super::type_dialog::Fam::Wall, id);
            }
        }
        None => {}
    }
}

fn units(ui: &mut egui::Ui, th: &Theme, ru: bool, ed: &mut Editor) {
    let mm = tr(ru, "type_dialog.units.mm");
    let ft = tr(ru, "type_dialog.units.ftin");
    let sel = usize::from(ed.tool.units == Units::Imperial);
    let mut u = ed.tool.units;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        caption(ui, th, &tr(ru, "sel_chip.opt.units"));
        if let Some(i) = widgets::segmented(ui, th, &[mm.as_str(), ft.as_str()], sel) {
            u = if i == 1 {
                Units::Imperial
            } else {
                Units::Metric
            };
        }
    });
    ed.set_units(u);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::{Catalog, Lang};
    use ddc_ui::{Density, Mode as ThemeMode};

    fn ids(g: &[Group]) -> Vec<GroupId> {
        g.iter().map(|g| g.id).collect()
    }

    fn all_items(g: &[Group]) -> Vec<Item> {
        g.iter().flat_map(|g| g.items.clone()).collect()
    }

    #[test]
    fn select_shows_level_and_units_only() {
        for wall_opts in [true, false] {
            let g = opt_groups(Mode::Select, false, None, wall_opts);
            assert_eq!(ids(&g), [GroupId::Level, GroupId::Units]);
        }
        let g = opt_groups(Mode::Polyline, false, None, false);
        assert_eq!(ids(&g), [GroupId::Level, GroupId::Units]);
    }

    #[test]
    fn wall_has_arc_level_type_height_units() {
        for m in [Mode::Polyline, Mode::Rect, Mode::PickLine] {
            let g = opt_groups(m, false, None, true);
            assert_eq!(
                ids(&g),
                [GroupId::Arc, GroupId::Level, GroupId::Wall, GroupId::Units]
            );
            assert_eq!(
                g[2].items,
                [Item::WallType, Item::Num(Field::WallH)].to_vec()
            );
        }
    }

    #[test]
    fn curtain_adds_four_spacing_and_mullion_fields() {
        let g = opt_groups(Mode::Polyline, true, None, true);
        assert_eq!(
            ids(&g),
            [
                GroupId::Arc,
                GroupId::Curtain,
                GroupId::Level,
                GroupId::Wall,
                GroupId::Units
            ]
        );
        assert_eq!(g[1].items.len(), 4);
    }

    #[test]
    fn sill_only_for_windows_and_voids() {
        let door = opt_groups(Mode::Select, false, Some(Kind::Door), true);
        let win = opt_groups(Mode::Select, false, Some(Kind::Window), true);
        let void = opt_groups(Mode::Select, false, Some(Kind::Void), true);
        assert_eq!(
            door[0].items,
            [Item::Num(Field::OpW), Item::Num(Field::OpH)].to_vec()
        );
        for g in [&win, &void] {
            assert_eq!(g[0].id, GroupId::Opening);
            assert!(g[0].items.contains(&Item::Num(Field::OpSill)));
        }
        assert_eq!(ids(&door), [GroupId::Opening, GroupId::Level, GroupId::Units]);
        assert!(!all_items(&door).contains(&Item::Arc));
    }

    #[test]
    fn group_order_is_fixed_for_every_tool() {
        let order = [
            GroupId::Arc,
            GroupId::Curtain,
            GroupId::Opening,
            GroupId::Level,
            GroupId::Wall,
            GroupId::Units,
        ];
        let rank = |id: GroupId| order.iter().position(|x| *x == id).unwrap();
        for m in [Mode::Select, Mode::Polyline, Mode::Rect, Mode::PickLine] {
            for cw in [false, true] {
                for op in [None, Some(Kind::Door), Some(Kind::Window), Some(Kind::Void)] {
                    for wo in [false, true] {
                        let g = opt_groups(m, cw, op, wo);
                        let r: Vec<usize> = g.iter().map(|g| rank(g.id)).collect();
                        assert!(r.windows(2).all(|w| w[0] < w[1]), "{m:?} {cw} {op:?} {wo}");
                        assert!(g.iter().all(|g| !g.items.is_empty()));
                        assert!(ids(&g).contains(&GroupId::Units));
                    }
                }
            }
        }
    }

    #[test]
    fn ranges_keep_the_old_floors_and_limits() {
        let want = [
            (Field::CwU, (0.1, 20.0)),
            (Field::CwV, (0.1, 20.0)),
            (Field::MullionW, (0.01, 0.5)),
            (Field::MullionD, (0.01, 1.0)),
            (Field::OpW, (0.05, 20.0)),
            (Field::OpH, (0.05, 20.0)),
            (Field::OpSill, (0.0, 20.0)),
            (Field::WallH, (0.1, 100.0)),
        ];
        assert_eq!(want.len(), Field::ALL.len());
        for (f, r) in want {
            assert_eq!(f.range(), r, "{f:?}");
        }
    }

    #[test]
    fn slots_address_the_tool_values() {
        let mut cw = CurtainDefaults::default();
        let mut ty = OpeningType::window();
        let mut h = 3.0;
        for (i, f) in Field::ALL.iter().enumerate() {
            *f.slot(&mut cw, &mut ty, &mut h) = 1.0 + i as f64;
        }
        assert_eq!((cw.u, cw.v, cw.mullion), (1.0, 2.0, (3.0, 4.0)));
        assert_eq!((ty.width, ty.height, ty.sill, h), (5.0, 6.0, 7.0, 8.0));
    }

    #[test]
    fn typed_values_clamp_into_the_range() {
        let m = Units::Metric;
        let near = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        assert!(near(parse_value("1", m, Field::WallH), 0.1));
        assert!(near(parse_value("500000", m, Field::WallH), 100.0));
        assert!(near(parse_value("3000", m, Field::WallH), 3.0));
        assert_eq!(parse_value("0", m, Field::OpSill), Some(0.0));
        assert_eq!(parse_value("0", m, Field::OpW), None);
        assert_eq!(parse_value("abc", m, Field::OpW), None);
        assert_eq!(parse_value("", m, Field::OpW), None);
        assert!(near(parse_value("1 500,5", m, Field::CwU), 1.5005));
    }

    #[test]
    fn value_round_trips_through_format_and_parse() {
        for f in Field::ALL {
            let (lo, hi) = f.range();
            for v in [lo.max(0.05), 0.9, 1.5, hi.min(2.1)] {
                let v = v.clamp(lo, hi);
                let back = parse_value(&format_value(v, Units::Metric), Units::Metric, f).unwrap();
                assert!((back - v).abs() < 1e-9, "{f:?} {v} {back}");
                let ft = parse_value(&format_value(v, Units::Imperial), Units::Imperial, f)
                    .unwrap_or_else(|| panic!("{f:?} {v} imperial"));
                assert!((ft - v).abs() <= crate::units::INCH, "{f:?} {v} {ft}");
            }
        }
    }

    #[test]
    fn unit_is_mm_in_metric_and_empty_in_feet_inches() {
        assert_eq!(unit_text(Units::Metric, false), "mm");
        assert_ne!(unit_text(Units::Metric, true), "mm");
        assert_eq!(unit_text(Units::Imperial, false), "");
    }

    #[test]
    fn level_rows_are_top_down_with_elevation() {
        let lv = vec![
            (1, "L0".to_string(), 0.0),
            (2, "L1".to_string(), 3.0),
            (3, "L2".to_string(), 6.0),
        ];
        let rows = level_items(&lv, Units::Metric, true);
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [3, 2, 1]);
        assert!(rows[0].1.starts_with("L2   "));
        assert!(rows[0].1.ends_with("+6,000"));
        assert!(rows[2].1.ends_with("0,000"));
    }

    #[test]
    fn labels_exist_in_en_and_ru() {
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        let mut keys = vec![
            "opt.arc_tip",
            "spline.opt.spline",
            "spline.opt.spline_tip",
            "opt.level",
            "opt.type",
            "type_dialog.panels.level_tip",
            "type_dialog.panels.wall_type",
            "type_dialog.units.mm",
            "type_dialog.units.ftin",
            "length_input.opt.bad_length",
            "unit_fmt.unit.mm",
            "sel_chip.opt.units",
        ];
        for f in Field::ALL {
            keys.push(f.label_key());
            keys.push(f.tip_key());
        }
        for k in keys {
            let (e, r) = (en.t(k), ru.t(k));
            assert_ne!(e, k, "missing en {k}");
            assert_ne!(r, k, "missing ru {k}");
            assert!(!e.is_empty() && !r.is_empty(), "{k}");
        }
        for k in ["length_input.opt.bad_length", "opt.level", "opt.type", "opt.cw_u", "sel_chip.opt.units"] {
            assert_ne!(en.t(k), ru.t(k), "{k} not translated");
        }
    }

    #[test]
    fn source_uses_no_raw_egui_widgets_or_literal_colors() {
        let src = include_str!("tool_opts.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for bad in [
            "ComboBox",
            "DragValue",
            "selectable_label",
            "ui.label(",
            "ui.add(",
            "ui.button(",
            "Slider",
            "TextEdit",
            "Color32::",
            "from_rgb",
        ] {
            assert!(!code.contains(bad), "raw widget or colour: {bad}");
        }
    }

    #[test]
    fn length_field_is_control_height_in_both_densities() {
        for dens in [Density::Compact, Density::Comfortable] {
            let th = Theme::new(ThemeMode::Light, dens);
            let ctx = ddc_ui::theme::headless_ctx();
            let mut h = 0.0;
            let mut v = 1.5;
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    h = ui
                        .horizontal(|ui| {
                            num(ui, &th, false, Units::Metric, Field::CwU, &mut v);
                        })
                        .response
                        .rect
                        .height();
                });
            });
            assert!(h >= th.d().control - 0.5, "{dens:?}: {h}");
            assert!(h <= th.d().control + 0.5, "{dens:?}: {h}");
            assert_eq!(v, 1.5);
        }
    }
}
