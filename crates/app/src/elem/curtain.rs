//! Curtain wall family of the element contract (E3): vertical 3D grips (top, bottom) beside the
//! lifted plan grips, and the panel as a real sub-element with its own properties section.
//! Plan grips live in `grips.rs` (`Kind::Curtain`): ends, move, arc. Every drag is one model
//! operation, so one undo restores it.

use crate::edit3d::grips::{elev, lifted, p3, Axis3, Grip3, Provider, Role3};
use crate::edit3d::pick::{Cat, Target};
use crate::editor::cw::{panel_of, PANEL_KINDS};
use crate::editor::{Editor, Sel};
use crate::grips::Change;
use crate::tr;
use crate::ui_kit;
use crate::units::{edit_length, unit_label};
use ddc_geom::arc::edge_mid;
use ddc_geom::v2;
use ddc_model::{CurtainSpec, Error, Model, PanelKind};
use ddc_render::glam::Vec3;

/// Lowest height a drag may leave, m.
pub const MIN_HEIGHT: f64 = 0.1;

fn bad(key: &str) -> Error {
    Error::Invalid(key.into())
}

/// The wall after its top (`top`) or bottom edge moved by `dz` metres. The bottom keeps the top
/// in place. Refuses a height under [`MIN_HEIGHT`].
pub fn resized(s: &CurtainSpec, top: bool, dz: f64) -> Result<CurtainSpec, Error> {
    let mut s = s.clone();
    if top {
        s.height += dz;
    } else {
        s.base_offset += dz;
        s.height -= dz;
    }
    if !(s.height >= MIN_HEIGHT) {
        return Err(bad("e3d.err.cw_height"));
    }
    Ok(s)
}

/// Provider of the 3D grips of curtain walls.
pub struct CurtainGrips;

impl Provider for CurtainGrips {
    fn key(&self) -> &'static str {
        "curtain"
    }

    fn owns(&self, t: Target) -> bool {
        t.cat == Cat::Curtain
    }

    fn grips(&self, m: &Model, z0: f64, t: Target) -> Result<Vec<Grip3>, Error> {
        let mut out = vec![];
        lifted(m, z0, t, &mut out)?;
        let s = m.curtain_wall(t.id)?;
        // a sloped top is edited as a profile: a height grip would fight it
        if s.top.is_empty() {
            let (a, b) = (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1));
            let c = if s.bulge != 0.0 {
                edge_mid(a, b, s.bulge)
            } else {
                (a + b) * 0.5
            };
            let zb = elev(m, s.level)? + s.base_offset - z0;
            let (base, top) = (p3(c.x, c.y, zb), p3(c.x, c.y, zb + s.height));
            out.push(Grip3 {
                target: t,
                role: Role3::Top,
                pos: top,
                axis: Axis3::Line(Vec3::Z),
                anchor: base,
            });
            out.push(Grip3 {
                target: t,
                role: Role3::Base,
                pos: base,
                axis: Axis3::Line(Vec3::NEG_Z),
                anchor: top,
            });
        }
        Ok(out)
    }

    fn apply(&self, m: &mut Model, _z0: f64, g: &Grip3, to: Vec3) -> Result<(), Error> {
        let d = g.delta(to);
        match &g.role {
            Role3::Plan(p) => crate::grips::apply(m, p, v2(p.pos.x + d.0, p.pos.y + d.1)),
            Role3::Top | Role3::Base => {
                let s = resized(&m.curtain_wall(g.target.id)?, g.role == Role3::Top, d.2)?;
                m.set_curtain_wall(g.target.id, &s).map(|_| ())
            }
            _ => Err(bad("e3d.err.unsupported")),
        }
    }

    fn preview(&self, m: &Model, _z0: f64, g: &Grip3, to: Vec3) -> Result<Vec<Vec<Vec3>>, Error> {
        let d = g.delta(to);
        let (change, z) = match &g.role {
            Role3::Plan(p) => (
                crate::grips::compute(m, p, v2(p.pos.x + d.0, p.pos.y + d.1))?,
                g.pos.z,
            ),
            Role3::Top | Role3::Base => {
                let s = resized(&m.curtain_wall(g.target.id)?, g.role == Role3::Top, d.2)?;
                (Change::Curtain(g.target.id, s), to.z)
            }
            _ => return Err(bad("e3d.err.unsupported")),
        };
        Ok(crate::grips::outline(&change)
            .into_iter()
            .map(|l| l.into_iter().map(|p| p3(p.x, p.y, 0.0).with_z(z)).collect())
            .collect())
    }
}

/// Label of a panel type, from the catalog.
pub fn kind_label(k: PanelKind) -> String {
    tr(&format!("curtain_props.cw.kind.{}", k.as_str()), &[])
}

/// Index of the neighbouring cell, or None at the ends of the list.
pub fn neighbour(n: usize, k: u32, forward: bool) -> Option<u32> {
    if forward {
        ((k as usize) + 1 < n).then_some(k + 1)
    } else {
        k.checked_sub(1)
    }
}

/// Properties of a selected panel: type, name, door swing, size, areas, and the way to the
/// neighbouring panels or the whole wall. Every edit is one undo step. False: no such panel.
pub fn panel_section(ui: &mut egui::Ui, ed: &mut Editor, id: i64, k: u32) -> bool {
    let u = ed.tool.units;
    let Some(n) = ed.curtain(id).map(|c| c.layout.layout.cells.len()) else {
        return false;
    };
    let Some((cell, p)) = ed.curtain(id).and_then(|c| panel_of(c, k)) else {
        return false;
    };
    let (w, h, area, net, i, j) = (
        cell.width,
        cell.height,
        cell.area,
        cell.net_area,
        cell.i,
        cell.j,
    );
    let mut kind = None;
    let mut name = None;
    let mut swing = None;
    let name_key = egui::Id::new(("e3.cw.panel.name", id, k));
    ui_kit::props(ui, "sel", |ui| {
        ui_kit::kv(
            ui,
            &tr("curtain_props.cw.panel", &[]),
            tr(
                "curtain_props.cw.panel_cell",
                &[
                    ("i", &(i + 1).to_string()),
                    ("j", &(j + 1).to_string()),
                    ("id", &id.to_string()),
                ],
            ),
        );
        ui_kit::kv_widget(ui, &tr("curtain_props.cw.kind", &[]), |ui| {
            let th = ddc_ui::theme::of(ui.ctx());
            let names: Vec<String> = PANEL_KINDS.iter().map(|k| kind_label(*k)).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut sel = PANEL_KINDS.iter().position(|k| *k == p.kind).unwrap_or(0);
            let w = Some(ui_kit::WIDGET_W);
            if ddc_ui::widgets::field::dropdown(ui, &th, "sel.panel", &mut sel, &names, w).changed()
            {
                kind = PANEL_KINDS.get(sel).copied();
            }
        });
        let name_label = tr("curtain_props.cw.name", &[]);
        ui_kit::kv_widget(ui, &name_label, |ui| {
            let editing: Option<String> = ui.data(|d| d.get_temp(name_key));
            let mut text = editing.clone().unwrap_or_else(|| p.name.clone());
            let th = ddc_ui::theme::of(ui.ctx());
            let r = ddc_ui::widgets::TextField::new(&mut text)
                .id_salt(name_key)
                .labelled_by(name_key.with("lbl"), &name_label)
                .width(ui_kit::WIDGET_W)
                .show(ui, &th)
                .response;
            if r.has_focus() {
                ui.data_mut(|d| d.insert_temp(name_key, text.clone()));
            } else if editing.is_some() {
                ui.data_mut(|d| d.remove::<String>(name_key));
            }
            if r.lost_focus() && text != p.name {
                name = Some(text);
            }
        });
        if p.kind == PanelKind::Door {
            let (mut side, mut hinge) = (p.swing_side, p.swing_hinge);
            let a = ui_kit::kv_widget(ui, &tr("curtain_props.cw.swing_side", &[]), |ui| {
                ui.add(egui::Checkbox::without_text(&mut side)).changed()
            });
            let b = ui_kit::kv_widget(ui, &tr("curtain_props.cw.swing_hinge", &[]), |ui| {
                ui.add(egui::Checkbox::without_text(&mut hinge)).changed()
            });
            if a || b {
                swing = Some((side, hinge));
            }
        }
        ui_kit::kv_num(
            ui,
            &tr("curtain_props.cw.size", &[]),
            format!(
                "{} \u{d7} {} {}",
                edit_length(w, u),
                edit_length(h, u),
                unit_label(u)
            ),
        );
        ui_kit::kv_num(ui, &tr("curtain_props.cw.cell_area", &[]), crate::unit_text::area_ui(area));
        ui_kit::kv_num(ui, &tr("curtain_props.cw.panel_area", &[]), crate::unit_text::area_ui(net));
    });
    let mut go = None;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let prev = neighbour(n, k, false);
        let next = neighbour(n, k, true);
        if ui
            .add_enabled(prev.is_some(), egui::Button::new("<"))
            .on_hover_text(tr("curtain_props.cw.prev", &[]))
            .clicked()
        {
            go = prev.map(|q| Sel::Panel(id, q));
        }
        if ui
            .add_enabled(next.is_some(), egui::Button::new(">"))
            .on_hover_text(tr("curtain_props.cw.next", &[]))
            .clicked()
        {
            go = next.map(|q| Sel::Panel(id, q));
        }
        if ui.button(tr("curtain_props.cw.wall", &[])).clicked() {
            go = Some(Sel::Curtain(id));
        }
    });
    if let Some(kd) = kind {
        ed.set_panel_kind(id, k, kd);
    }
    if let Some(nm) = name {
        ed.set_panel(id, k, |p| p.name = nm);
    }
    if let Some((side, hinge)) = swing {
        ed.set_panel(id, k, |p| {
            p.swing_side = side;
            p.swing_hinge = hinge;
        });
    }
    if let Some(s) = go {
        ed.selection = vec![s];
    }
    ui_kit::hint(ui, tr("curtain_props.cw.panel_hint", &[]));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::OpFn;
    use crate::grips::{self, Kind, Role};
    use std::sync::Arc;

    fn facade() -> (Editor, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[(v2(0.0, 0.0), v2(6.0, 0.0), 0.0)]);
        let id = e.curtains[0].id;
        (e, id)
    }

    fn drag(e: &mut Editor, id: i64, role: Role, to: ddc_geom::V2) -> Result<(), Error> {
        let g = grips::grips(&e.model, Kind::Curtain, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == role)
            .expect("grip");
        let f = OpFn(Arc::new(move |m: &mut Model| grips::apply(m, &g, to)));
        e.run_op(f)
    }

    #[test]
    fn plan_grips_cover_ends_and_move() {
        let (e, id) = facade();
        let roles: Vec<Role> = grips::grips(&e.model, Kind::Curtain, id)
            .unwrap()
            .into_iter()
            .map(|g| g.role)
            .collect();
        assert_eq!(roles, [Role::End(0), Role::End(1), Role::Move]);
    }

    #[test]
    fn end_grip_stretches_and_one_undo_restores() {
        let (mut e, id) = facade();
        drag(&mut e, id, Role::End(1), v2(8.0, 0.0)).unwrap();
        assert!((e.curtain(id).unwrap().length() - 8.0).abs() < 1e-9);
        e.undo();
        assert!((e.curtain(id).unwrap().length() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn end_grip_refuses_a_zero_length_wall_and_leaves_the_model() {
        let (mut e, id) = facade();
        let head = e.model.change_head();
        let g = grips::grips(&e.model, Kind::Curtain, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == Role::End(1))
            .unwrap();
        let err = grips::compute(&e.model, &g, v2(0.0, 0.0)).unwrap_err();
        assert!(err.to_string().contains("curtain wall"), "{err}");
        assert_eq!(e.model.change_head(), head);
    }

    #[test]
    fn move_grip_keeps_panels_in_their_cells() {
        let (mut e, id) = facade();
        e.set_panel_kind(id, 1, PanelKind::Door);
        // along the axis: a re-fit by position would shift the door out of its cell
        drag(&mut e, id, Role::Move, v2(5.0, 0.0)).unwrap();
        let c = e.curtain(id).unwrap();
        assert_eq!(c.spec().a, (2.0, 0.0));
        assert_eq!(c.layout.panels[1].kind, PanelKind::Door);
        assert_eq!(c.qty.doors, 1);
        e.undo();
        assert_eq!(e.curtain(id).unwrap().spec().a, (0.0, 0.0));
        assert_eq!(e.curtain(id).unwrap().qty.doors, 1);
    }

    #[test]
    fn arc_grip_bends_and_straightens() {
        let (mut e, id) = facade();
        e.edit_curtain(id, |s| s.bulge = 0.3);
        let roles: Vec<Role> = grips::grips(&e.model, Kind::Curtain, id)
            .unwrap()
            .into_iter()
            .map(|g| g.role)
            .collect();
        assert!(roles.contains(&Role::Bulge));
        let a = e.curtain(id).unwrap().spec().a;
        let b = e.curtain(id).unwrap().spec().b;
        let chord_mid = v2((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        drag(&mut e, id, Role::Bulge, chord_mid).unwrap();
        assert_eq!(e.curtain(id).unwrap().spec().bulge, 0.0);
    }

    #[test]
    fn typed_length_reaches_the_end_grip_dimension() {
        let (e, id) = facade();
        let g = grips::grips(&e.model, Kind::Curtain, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == Role::End(1))
            .unwrap();
        let d = grips::dims(&e.model, &g, g.pos).unwrap();
        // the first dimension runs from the other end: typing 4 m sets the length
        assert!((d[0].value - 6.0).abs() < 1e-9);
        let to = d[0].retarget(4.0);
        assert!((to.x - 4.0).abs() < 1e-9 && to.y.abs() < 1e-9);
    }

    #[test]
    fn grips_3d_resize_the_wall_in_one_step() {
        let (mut e, id) = facade();
        let t = Target::new(Cat::Curtain, id);
        let h0 = e.curtain(id).unwrap().spec().height;
        let gs = crate::edit3d::grips::grips_of(&e.model, 0.0, t);
        assert!(gs.iter().any(|g| matches!(g.role, Role3::Plan(_))));
        let top = gs.iter().find(|g| g.role == Role3::Top).unwrap().clone();
        let to = top.pos + Vec3::new(0.0, 0.0, 0.5);
        crate::edit3d::grips::apply(&mut e.model, 0.0, &top, to).unwrap();
        assert!((e.model.curtain_wall(id).unwrap().height - (h0 + 0.5)).abs() < 1e-6);
        e.model.undo().unwrap();
        assert!((e.model.curtain_wall(id).unwrap().height - h0).abs() < 1e-9);

        // the bottom edge moves, the top stays
        let base = gs.iter().find(|g| g.role == Role3::Base).unwrap().clone();
        let to = base.pos + Vec3::new(0.0, 0.0, 0.5);
        crate::edit3d::grips::apply(&mut e.model, 0.0, &base, to).unwrap();
        let s = e.model.curtain_wall(id).unwrap();
        assert!((s.base_offset - 0.5).abs() < 1e-6);
        assert!((s.base_offset + s.height - h0).abs() < 1e-6);
    }

    #[test]
    fn grips_3d_refuse_a_collapsed_wall() {
        let (mut e, id) = facade();
        let t = Target::new(Cat::Curtain, id);
        let h0 = e.curtain(id).unwrap().spec().height;
        let top = crate::edit3d::grips::grips_of(&e.model, 0.0, t)
            .into_iter()
            .find(|g| g.role == Role3::Top)
            .unwrap();
        let to = top.pos - Vec3::new(0.0, 0.0, h0 as f32);
        let head = e.model.change_head();
        let err = crate::edit3d::grips::apply(&mut e.model, 0.0, &top, to).unwrap_err();
        assert!(matches!(&err, Error::Invalid(k) if k == "e3d.err.cw_height"));
        assert_eq!(e.model.change_head(), head);
    }

    #[test]
    fn a_sloped_top_has_no_height_grips() {
        let (mut e, id) = facade();
        e.edit_curtain(id, |s| s.top = vec![(0.0, 1.0), (6.0, 2.0)]);
        let t = Target::new(Cat::Curtain, id);
        let gs = crate::edit3d::grips::grips_of(&e.model, 0.0, t);
        assert!(gs.iter().all(|g| !matches!(g.role, Role3::Top | Role3::Base)));
        assert!(!gs.is_empty());
    }

    #[test]
    fn resized_checks_the_floor() {
        let (e, id) = facade();
        let s = e.curtain(id).unwrap().spec().clone();
        assert!(resized(&s, true, -s.height).is_err());
        assert_eq!(resized(&s, false, 0.5).unwrap().height, s.height - 0.5);
    }

    #[test]
    fn neighbours_stop_at_the_ends() {
        assert_eq!(neighbour(4, 0, false), None);
        assert_eq!(neighbour(4, 0, true), Some(1));
        assert_eq!(neighbour(4, 3, true), None);
        assert_eq!(neighbour(4, 3, false), Some(2));
    }

    #[test]
    fn panel_catalog_has_both_languages_and_no_cyrillic_in_en() {
        let en = ddc_ui::i18n::Catalog::new(ddc_ui::i18n::Lang::En);
        for k in PANEL_KINDS {
            let key = format!("curtain_props.cw.kind.{}", k.as_str());
            let t = en.t_in(ddc_ui::i18n::Lang::En, &key);
            assert!(t != key && t.chars().all(|c| !('\u{400}'..='\u{4ff}').contains(&c)));
            let ru = en.t_in(ddc_ui::i18n::Lang::Ru, &key);
            assert!(ru != key && ru != t);
        }
    }
}
