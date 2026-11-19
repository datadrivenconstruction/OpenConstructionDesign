//! Ramps, room separators and zones outside the tools: plan layer, pick outlines, property
//! rows; the empty-opening command.

use super::layer::{Cls, Layer};
use crate::opening_tool::OpeningType;
use crate::shell::EditorApp;
use crate::ui_kit;
use crate::unit_text::{area_ui, vol_ui};
use crate::units::{display_length, Units};
use ddc_geom::{v2, V2};
use ddc_model::{Model, PhaseShow, RampSpec, ShaftSpec, ZoneSpec};
use ddc_ui::i18n::Catalog;

/// Plan cut-plane elevation above the level, m.
const CUT: f64 = 1.2;
/// Zone tag half-width and its offset from the room point, m.
pub(crate) const MARK: f64 = 0.25;
const MARK_DROP: f64 = 0.6;

pub fn register(app: &mut EditorApp) {
    app.register_command("arch.opening", |a| {
        a.ed.start_opening(OpeningType::void());
    });
}

fn room_point(m: &Model, room: i64, level: i64) -> Option<(f64, f64)> {
    m.sql()
        .query_row(
            "SELECT px,py FROM room WHERE id=?1 AND level=?2",
            [room, level],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

/// Zone tag point on a level: under the zone's first room on that level.
pub(crate) fn zone_anchor(m: &Model, z: &ZoneSpec, level: i64) -> Option<(f64, f64)> {
    let (x, y) = z.rooms.iter().find_map(|r| room_point(m, *r, level))?;
    Some((x, y - MARK_DROP))
}

/// Pick outlines in the plan: same rows as structural elements (`StructFp`).
pub fn footprints(m: &Model, level: i64) -> Vec<(&'static str, i64, Vec<Vec<(f64, f64)>>)> {
    let pairs = |r: &[V2]| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    let mut out = vec![];
    for id in m.ramps(level).unwrap_or_default() {
        let Ok(l) = m.ramp_layout(id) else {
            // not built, but must be pickable: outline along the axis
            let rings = m.ramp_axis_rings(id).unwrap_or_default();
            out.push(("ramp", id, rings.iter().map(|r| pairs(r)).collect()));
            continue;
        };
        let mut rings: Vec<Vec<(f64, f64)>> = l
            .flights
            .iter()
            .map(|f| pairs(&f.ring(l.def.width)))
            .collect();
        rings.extend(l.landings.iter().map(|g| pairs(&g.ring)));
        out.push(("ramp", id, rings));
    }
    for (id, s) in m.room_seps(level).unwrap_or_default() {
        out.push(("room_sep", id, vec![vec![s.a, s.b]]));
    }
    for (id, rings) in crate::elem::roof::footprints(m, level) {
        out.push(("roof", id, rings));
    }
    for (id, z) in m.zones().unwrap_or_default() {
        if let Some((x, y)) = zone_anchor(m, &z, level) {
            let h = MARK;
            let ring = vec![
                (x - h, y - h),
                (x + h, y - h),
                (x + h, y + h),
                (x - h, y + h),
            ];
            out.push(("zone", id, vec![ring]));
        }
    }
    out.extend(crate::elem::stairs::footprints(m, level));
    // a shaft is picked on every level it passes through, as it is drawn there
    for id in m.shafts_on_level(level).unwrap_or_default() {
        if let Ok(r) = m.shaft_ring(id) {
            out.push(("shaft", id, vec![r]));
        }
    }
    // a line is a chain of two-point rings: it is picked on the line only, never inside an arc
    let seen: Vec<String> = m
        .grid_axes(Some(level))
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.label)
        .collect();
    for g in m.grids().unwrap_or_default() {
        if seen.contains(&g.name) {
            let pts = g.spec.curve().points(0.25);
            let rings = pts
                .windows(2)
                .map(|w| vec![(w[0].x, w[0].y), (w[1].x, w[1].y)])
                .collect();
            out.push(("grid", g.id, rings));
        }
    }
    for (id, a, b) in m.ref_planes().unwrap_or_default() {
        out.push(("ref_plane", id, vec![vec![a, b]]));
    }
    out
}

/// Extend the plan layer.
pub fn load_layer(
    m: &Model,
    level: i64,
    l: &mut Layer,
    show: &dyn Fn(&str, i64) -> Option<PhaseShow>,
) {
    for id in m.ramps(level).unwrap_or_default() {
        let Some(sh) = show("ramp", id) else { continue };
        let Ok(plan) = m.ramp_plan(id, CUT) else {
            let rings = m.ramp_axis_rings(id).unwrap_or_default();
            if let Some(c) = rings.first().and_then(|r| r.first()) {
                l.labels.push((*c, crate::tr("core_tools.ramp.broken_mark", &[])));
            }
            for o in rings {
                l.polys.push((Cls::Stair, o, true, sh));
            }
            continue;
        };
        if let (Some(a), Some(b), Ok(s)) = (plan.run_line.first(), plan.run_line.get(1), m.ramp(id))
        {
            let mid = *a + (*b - *a) * 0.5;
            l.labels.push((mid, format!("{:.1} %", s.slope * 100.0)));
        }
        for o in plan.outlines {
            l.polys.push((Cls::Stair, o, true, sh));
        }
        l.polys.push((Cls::Stair, plan.run_line, false, sh));
        for (a, b) in plan.head {
            l.polys.push((Cls::Stair, vec![a, b], false, sh));
        }
        if let Some((a, b)) = plan.cut {
            l.polys.push((Cls::Stair, vec![a, b], false, sh));
        }
    }
    for (_, s) in m.room_seps(level).unwrap_or_default() {
        let (a, b) = (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1));
        l.seps.push((a, b));
        l.polys
            .push((Cls::RoomSep, vec![a, b], false, PhaseShow::Normal));
    }
    for (_, z) in m.zones().unwrap_or_default() {
        let Some((x, y)) = zone_anchor(m, &z, level) else {
            continue;
        };
        l.labels.push((v2(x, y), z.name.clone()));
        for room in &z.rooms {
            if room_point(m, *room, level).is_none() {
                continue;
            }
            let Ok(Some(q)) = m.room_quantities(*room) else {
                continue;
            };
            if let Some(o) = q.rings.first() {
                let ring = o.iter().map(|p| v2(p.0, p.1)).collect();
                l.polys.push((Cls::Zone, ring, true, PhaseShow::Normal));
            }
        }
    }
}

/// Edit from property rows; the caller commits it as a single operation.
pub enum CoreEdit {
    Zone(ZoneSpec),
    Ramp(RampSpec),
    Shaft(ShaftSpec),
    /// New name of a grid line or a reference plane.
    Name(String),
}

/// Rooms outside zones: (id, label).
pub(super) fn free_rooms(m: &Model) -> Vec<(i64, String)> {
    let q = "SELECT r.id, r.number, r.name FROM room r LEFT JOIN zone_room z ON z.room=r.id \
             WHERE z.room IS NULL ORDER BY r.id";
    let Ok(mut st) = m.sql().prepare(q) else {
        return vec![];
    };
    st.query_map([], |r| {
        let (n, name): (String, String) = (r.get(1)?, r.get(2)?);
        Ok((r.get(0)?, format!("{n} {name}").trim().to_string()))
    })
    .map(|it| it.flatten().collect())
    .unwrap_or_default()
}

fn room_label(m: &Model, room: i64) -> String {
    m.sql()
        .query_row("SELECT number, name FROM room WHERE id=?1", [room], |r| {
            let (n, name): (String, String) = (r.get(0)?, r.get(1)?);
            Ok(format!("{n} {name}").trim().to_string())
        })
        .unwrap_or_else(|_| format!("#{room}"))
}

/// Levels bottom to top: (id, name).
fn level_names(m: &Model) -> Vec<(i64, String)> {
    let Ok(mut st) = m
        .sql()
        .prepare("SELECT id, name FROM level ORDER BY elevation")
    else {
        return vec![];
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// Level dropdown of the kit. `none` adds a first row for «no level»; returns the new choice
/// only when the user moved it.
pub(crate) fn level_dropdown(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    levels: &[(i64, String)],
    none: Option<&str>,
    cur: Option<i64>,
) -> Option<Option<i64>> {
    let th = ddc_ui::theme::of(ui.ctx());
    let mut items: Vec<&str> = none.into_iter().collect();
    items.extend(levels.iter().map(|l| l.1.as_str()));
    let mut ix = level_row(levels, none.is_some(), cur);
    let r = ddc_ui::widgets::dropdown(ui, &th, salt, &mut ix, &items, Some(crate::ui_kit::WIDGET_W));
    if !r.changed() {
        return None;
    }
    Some(level_at(levels, none.is_some(), ix))
}

/// Dropdown row of a level (row 0 is «none» when `has_none`).
fn level_row(levels: &[(i64, String)], has_none: bool, cur: Option<i64>) -> usize {
    let skip = usize::from(has_none);
    cur.and_then(|c| levels.iter().position(|l| l.0 == c))
        .map_or(0, |p| p + skip)
}

/// Level of a dropdown row; the «none» row gives None.
fn level_at(levels: &[(i64, String)], has_none: bool, ix: usize) -> Option<i64> {
    ix.checked_sub(usize::from(has_none))
        .and_then(|p| levels.get(p))
        .map(|l| l.0)
}

/// Ramp bottom and top levels.
fn ramp_levels(ui: &mut egui::Ui, cat: &Catalog, m: &Model, id: i64) -> Option<CoreEdit> {
    let s = m.ramp(id).ok()?;
    let levels = level_names(m);
    let mut next = s.clone();
    ui_kit::kv_widget(ui, &cat.t("opt.level"), |ui| {
        if let Some(Some(l)) =
            level_dropdown(ui, ("ddc.f2.ramp_level", id), &levels, None, Some(s.level))
        {
            next.level = l;
        }
    });
    let none = cat.t("core_tools.ramp.top_none");
    ui_kit::kv_widget(ui, &cat.t("core_tools.ramp.top"), |ui| {
        if let Some(t) = level_dropdown(ui, ("ddc.f2.ramp_top", id), &levels, Some(&none), s.top_level)
        {
            next.top_level = t;
        }
    });
    (next != s).then_some(CoreEdit::Ramp(next))
}

/// Property rows beyond the editable fields; `Some` — edit of ramp levels or zone membership.
pub fn core_rows(
    ui: &mut egui::Ui,
    cat: &Catalog,
    m: &Model,
    kind: &str,
    id: i64,
    u: Units,
) -> Option<CoreEdit> {
    match kind {
        "ramp" => {
            if let Ok(Some(why)) = m.ramp_problem(id) {
                let text = cat.tf("warn.ramp_broken", &[("why", why.as_str())]);
                ui.colored_label(ui.visuals().warn_fg_color, text);
            }
            if let Ok(q) = m.ramp_qty(id) {
                ui_kit::kv_num(ui, &cat.t("core_tools.run"), display_length(q.run, u));
                ui_kit::kv_num(ui, &cat.t("core_tools.rise"), display_length(q.rise, u));
                ui_kit::kv_num(ui, &cat.t("core_tools.landings"), q.landings.to_string());
                ui_kit::kv_num(ui, &cat.t("sp.volume"), vol_ui(q.volume));
                ui_kit::kv_num(ui, &cat.t("sp.area"), area_ui(q.area));
            }
            ramp_levels(ui, cat, m, id)
        }
        "room_sep" => {
            if let Ok(s) = m.room_sep(id) {
                let len = (v2(s.b.0, s.b.1) - v2(s.a.0, s.a.1)).len();
                ui_kit::kv_num(ui, &cat.t("sp.length"), display_length(len, u));
            }
            None
        }
        "zone" => zone_rows(ui, cat, m, id).map(CoreEdit::Zone),
        "shaft" => shaft_rows(ui, cat, m, id, u),
        "grid" => {
            let name = m.grid(id).ok()?.name;
            name_row(ui, cat, kind, id, &name)
        }
        "ref_plane" => {
            let name = m.ref_plane(id).ok()?.0;
            name_row(ui, cat, kind, id, &name)
        }
        _ => None,
    }
}

/// Top level (a combo) and what the shaft cuts; offsets are numeric fields of `struct_props`.
fn shaft_rows(ui: &mut egui::Ui, cat: &Catalog, m: &Model, id: i64, u: Units) -> Option<CoreEdit> {
    let s = m.shaft(id).ok()?;
    let cuts = m.shaft_cuts(id).unwrap_or_default();
    let area: f64 = cuts.iter().map(|c| c.area).sum();
    let volume: f64 = cuts.iter().map(|c| c.volume).sum();
    ui_kit::kv_num(ui, &cat.t("shaft_tool.shaft.cuts"), cuts.len().to_string());
    ui_kit::kv_num(ui, &cat.t("shaft_tool.shaft.cut_area"), area_ui(area));
    ui_kit::kv_num(ui, &cat.t("shaft_tool.shaft.cut_volume"), vol_ui(volume));
    if let Ok(h) = m.sql().query_row(
        "SELECT t.elevation + ?2 - b.elevation - ?3 FROM level t, level b WHERE t.id=?1 AND b.id=?4",
        rusqlite::params![s.top_level, s.top_offset, s.base_offset, s.level],
        |r| r.get::<_, f64>(0),
    ) {
        ui_kit::kv_num(ui, &cat.t("opt.height"), display_length(h, u));
    }
    let levels = level_names(m);
    let mut next = s.clone();
    ui_kit::kv_widget(ui, &cat.t("opt.level"), |ui| {
        if let Some(Some(l)) =
            level_dropdown(ui, ("ddc.d3x.shaft_level", id), &levels, None, Some(s.level))
        {
            next.level = l;
        }
    });
    ui_kit::kv_widget(ui, &cat.t("shaft_tool.shaft.top"), |ui| {
        if let Some(Some(l)) =
            level_dropdown(ui, ("ddc.d3x.shaft_top", id), &levels, None, Some(s.top_level))
        {
            next.top_level = l;
        }
    });
    (next != s).then_some(CoreEdit::Shaft(next))
}

/// Name field of a datum: the edit is taken when the field loses focus.
fn name_row(ui: &mut egui::Ui, cat: &Catalog, kind: &str, id: i64, now: &str) -> Option<CoreEdit> {
    let bid = egui::Id::new(("ddc.d4.name", kind, id));
    let mut buf = ui
        .data(|d| d.get_temp::<String>(bid))
        .unwrap_or_else(|| now.to_string());
    let r = ui_kit::kv_widget(ui, &cat.t("core_name.name"), |ui| ui.text_edit_singleline(&mut buf));
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(bid));
        let name = buf.trim().to_string();
        return (!name.is_empty() && name != now).then_some(CoreEdit::Name(name));
    }
    ui.data_mut(|d| d.insert_temp(bid, buf));
    None
}

fn zone_rows(ui: &mut egui::Ui, cat: &Catalog, m: &Model, id: i64) -> Option<ZoneSpec> {
    let z = m.zone(id).ok()?;
    ui_kit::kv_num(ui, &cat.t("core_tools.rooms"), z.rooms.len().to_string());
    if let Ok(a) = m.zone_area(id) {
        ui_kit::kv_num(ui, &cat.t("sp.area"), area_ui(a));
    }
    let mut edit = None;
    for room in &z.rooms {
        ui.horizontal(|ui| {
            // a zone without rooms does not exist: the last room goes with the zone
            let last = z.rooms.len() <= 1;
            let tip = cat.t(if last { "rebar_list.zone_remove_last" } else { "core_tools.zone.remove" });
            let remove = ddc_ui::widgets::glyph_button(ui, "delete", !last, &tip);
            if remove.clicked() {
                let mut n = z.clone();
                n.rooms.retain(|r| r != room);
                edit = Some(n);
            }
            ui.label(room_label(m, *room));
        });
    }
    let free = free_rooms(m);
    if !free.is_empty() {
        let th = ddc_ui::theme::of(ui.ctx());
        let add = ddc_ui::widgets::Button::secondary(&cat.t("core_tools.zone.add")).small().show(ui, &th);
        egui::Popup::menu(&add).show(|ui| {
            for (r, label) in &free {
                let tone = ddc_ui::widgets::MenuTone::Normal;
                if ddc_ui::widgets::menu_item(ui, &th, label, "", tone, true).clicked() {
                    let mut n = z.clone();
                    n.rooms.push(*r);
                    n.rooms.sort_unstable();
                    edit = Some(n);
                    ui.close();
                }
            }
        });
    }
    if edit.is_some() {
        return edit;
    }
    let bid = egui::Id::new(("ddc.f2.zone_name", id));
    let mut buf = ui
        .data(|d| d.get_temp::<String>(bid))
        .unwrap_or_else(|| z.name.clone());
    let r = ui_kit::kv_widget(ui, &cat.t("core_tools.zone.name"), |ui| {
        ui.text_edit_singleline(&mut buf)
    });
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(bid));
        let name = buf.trim().to_string();
        if name.is_empty() || name == z.name {
            return None;
        }
        return Some(ZoneSpec {
            name,
            rooms: z.rooms,
        });
    }
    ui.data_mut(|d| d.insert_temp(bid, buf));
    None
}

#[cfg(test)]
mod ux54d_tests {
    use super::*;
    use ddc_ui::i18n::{Catalog, Lang};

    const THIS: &str = include_str!("core_items.rs");
    const MEP_COLOR: &str = include_str!("mep_color.rs");
    const DWG_EDIT: &str = include_str!("../drawing/edit.rs");

    /// Source above the test modules, so this file's own needles do not count.
    fn code(s: &str) -> &str {
        s.split("#[cfg(test)]").next().unwrap()
    }

    fn levels() -> Vec<(i64, String)> {
        vec![(7, "L0".into()), (9, "L1".into()), (12, "L2".into())]
    }

    #[test]
    fn level_rows_map_back_to_the_same_ids() {
        let l = levels();
        for has_none in [false, true] {
            for (id, _) in &l {
                let ix = level_row(&l, has_none, Some(*id));
                assert_eq!(level_at(&l, has_none, ix), Some(*id));
            }
        }
        assert_eq!(level_row(&l, true, None), 0);
        assert_eq!(level_at(&l, true, 0), None);
        assert_eq!(level_at(&l, true, 3), Some(12));
        assert_eq!(level_at(&l, false, 0), Some(7));
    }

    #[test]
    fn no_raw_combo_box_left_in_the_touched_files() {
        let needle = ["egui::Combo", "Box"].concat();
        for (name, s) in [("core_items", THIS), ("mep_color", MEP_COLOR), ("edit", DWG_EDIT)] {
            assert!(!code(s).contains(&needle), "{name} still has a raw combo box");
        }
        assert!(!code(DWG_EDIT).contains("ui.button("));
    }

    fn render(lang: Lang) {
        let cat = Catalog::new(lang);
        let ctx = egui::Context::default();
        let l = levels();
        let none = cat.t("core_tools.ramp.top_none");
        let mut seen = 0;
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                assert_eq!(level_dropdown(ui, "t1", &l, None, Some(9)), None);
                assert_eq!(level_dropdown(ui, "t2", &l, Some(&none), None), None);
                let names: Vec<String> = vec!["A".into(), "B".into()];
                let head = cat.t("dwg_edit.layer_pick");
                assert_eq!(
                    crate::drawing::edit::layer_dropdown(ui, "t3", &names, "B", Some(&head)),
                    None
                );
                assert_eq!(crate::drawing::edit::layer_dropdown(ui, "t4", &names, "A", None), None);
                seen += 1;
            });
        });
        assert_eq!(seen, 1);
        assert_ne!(none, "core_tools.ramp.top_none");
        assert_ne!(cat.t("dwg_edit.layer_pick"), "dwg_edit.layer_pick");
    }

    #[test]
    fn headless_render_en_and_ru() {
        render(Lang::En);
        render(Lang::Ru);
    }
}
