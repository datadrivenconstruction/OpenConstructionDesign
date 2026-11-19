//! Roof family of the element contract (D1): plan outline for selection, the move grip, and the
//! non-table part of the properties panel: live measured geometry and the shape fields that
//! depend on the roof form (edge slopes, break, depth, rise).

use crate::editor::{Editor, OpFn};
use crate::units::display_length;
use crate::{tr, ui_kit};
use ddc_model::{Error, Model, RoofKind, RoofSpec};
use std::sync::Arc;

type P = (f64, f64);

/// Closed plan outline: the support ring, or the plan rectangle of an extrusion.
pub fn outline(s: &RoofSpec) -> Vec<P> {
    match &s.kind {
        RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => ring.clone(),
        RoofKind::Extrusion { a, b, depth, .. } => {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy).max(1e-9);
            let n = (-dy / l * depth, dx / l * depth);
            vec![*a, *b, (b.0 + n.0, b.1 + n.1), (a.0 + n.0, a.1 + n.1)]
        }
    }
}

/// Mean of the outline: where the move grip sits.
pub fn center(s: &RoofSpec) -> P {
    let o = outline(s);
    let n = o.len().max(1) as f64;
    (
        o.iter().map(|p| p.0).sum::<f64>() / n,
        o.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

pub fn translated(s: &RoofSpec, d: P) -> RoofSpec {
    fn sh(p: &mut P, d: P) {
        p.0 += d.0;
        p.1 += d.1;
    }
    let mut s = s.clone();
    match &mut s.kind {
        RoofKind::Footprint { ring, .. } => ring.iter_mut().for_each(|p| sh(p, d)),
        RoofKind::Extrusion { a, b, .. } => {
            sh(a, d);
            sh(b, d);
        }
        RoofKind::Arrow {
            ring, tail, head, ..
        } => {
            ring.iter_mut().for_each(|p| sh(p, d));
            sh(tail, d);
            sh(head, d);
        }
    }
    s
}

/// Plan outlines of the roofs of a level, for `StructFp`.
pub fn footprints(m: &Model, level: i64) -> Vec<(i64, Vec<Vec<P>>)> {
    let ids: Vec<i64> = m
        .sql()
        .prepare("SELECT id FROM roof WHERE level=?1 ORDER BY id")
        .and_then(|mut st| st.query_map([level], |r| r.get(0))?.collect())
        .unwrap_or_default();
    ids.into_iter()
        .filter_map(|id| m.roof(id).ok().map(|s| (id, vec![outline(&s)])))
        .collect()
}

/// A number of the roof that depends on its form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// Slope of an edge of the ring, degrees; None in the model is a gable.
    Slope(usize),
    MansardH,
    MansardDeg,
    Depth,
    Rise,
}

/// Fields of the roof form with their values (degrees or metres); a gable edge has none.
pub fn fields(s: &RoofSpec) -> Vec<(Field, Option<f64>)> {
    match &s.kind {
        RoofKind::Footprint {
            slopes, mansard, ..
        } => {
            let mut v: Vec<_> = slopes
                .iter()
                .enumerate()
                .map(|(i, x)| (Field::Slope(i), *x))
                .collect();
            if let Some((h, deg)) = mansard {
                v.push((Field::MansardH, Some(*h)));
                if let Some(d) = deg {
                    v.push((Field::MansardDeg, Some(*d)));
                }
            }
            v
        }
        RoofKind::Extrusion { depth, .. } => vec![(Field::Depth, Some(*depth))],
        RoofKind::Arrow { rise, .. } => vec![(Field::Rise, Some(*rise))],
    }
}

/// False when the roof has no such field.
pub fn set_field(s: &mut RoofSpec, f: Field, v: f64) -> bool {
    match (f, &mut s.kind) {
        (Field::Slope(i), RoofKind::Footprint { slopes, .. }) => {
            slopes.get_mut(i).map(|x| *x = Some(v)).is_some()
        }
        (Field::MansardH, RoofKind::Footprint { mansard: Some(m), .. }) => {
            m.0 = v;
            true
        }
        (Field::MansardDeg, RoofKind::Footprint { mansard: Some(m), .. }) if m.1.is_some() => {
            m.1 = Some(v);
            true
        }
        (Field::Depth, RoofKind::Extrusion { depth, .. }) => {
            *depth = v;
            true
        }
        (Field::Rise, RoofKind::Arrow { rise, .. }) => {
            *rise = v;
            true
        }
        _ => false,
    }
}

/// Slope given to a gable edge when the user asks for one.
const NEW_SLOPE: f64 = 30.0;

/// What the model measures on the built roof.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub plan_area: f64,
    pub area: f64,
    pub volume: f64,
    pub thickness: f64,
    /// Ridge above the eave plane.
    pub height: f64,
    pub eave_z: f64,
    pub ridge: f64,
    pub hip: f64,
    pub valley: f64,
    pub eave: f64,
    pub rake: f64,
    pub gable: f64,
}

pub fn measure(m: &Model, id: i64) -> Result<Live, Error> {
    let q = m.roof_quantities(id)?;
    let g = m.roof_geometry(id)?;
    let top = g
        .faces
        .iter()
        .flat_map(|f| f.pts.iter().map(|p| p.z))
        .fold(g.eave_z, f64::max);
    Ok(Live {
        plan_area: q.geom.plan_area,
        area: q.geom.area,
        volume: q.layers.iter().map(|l| l.3).sum(),
        thickness: q.thickness,
        height: top - g.eave_z,
        eave_z: g.eave_z,
        ridge: q.geom.ridge,
        hip: q.geom.hip,
        valley: q.geom.valley,
        eave: q.geom.eave,
        rake: q.geom.rake,
        gable: q.geom.gable_area,
    })
}

fn kind_key(s: &RoofSpec) -> &'static str {
    match s.kind {
        RoofKind::Footprint { .. } => "elem.roof.kind.footprint",
        RoofKind::Extrusion { .. } => "elem.roof.kind.extrusion",
        RoofKind::Arrow { .. } => "elem.roof.kind.arrow",
    }
}

fn label(f: Field) -> String {
    match f {
        Field::Slope(i) => tr("elem.roof.slope", &[("n", (i + 1).to_string().as_str())]),
        Field::MansardH => tr("elem.roof.mansard_h", &[]),
        Field::MansardDeg => tr("elem.roof.mansard_deg", &[]),
        Field::Depth => tr("elem.roof.depth", &[]),
        Field::Rise => tr("elem.roof.rise", &[]),
    }
}

fn is_angle(f: Field) -> bool {
    matches!(f, Field::Slope(_) | Field::MansardDeg)
}

/// Panel part after the table rows (offset, overhang, type): geometry and shape.
pub fn panel(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    live(ui, ed, id);
    shape(ui, ed, id);
}

fn live(ui: &mut egui::Ui, ed: &Editor, id: i64) {
    // the measure rebuilds the roof: once per model revision, not per frame
    let key = egui::Id::new(("elem.roof.live", id));
    let rev = ed.revision;
    let cached = ui
        .data(|d| d.get_temp::<(u64, Result<Live, String>)>(key))
        .filter(|c| c.0 == rev);
    let got = match cached {
        Some(c) => c.1,
        None => {
            let r = measure(&ed.model, id).map_err(|e| e.to_string());
            ui.data_mut(|d| d.insert_temp(key, (rev, r.clone())));
            r
        }
    };
    if !ui_kit::heading(ui, &tr("elem.roof.geometry", &[])) {
        return;
    }
    let l = match got {
        Ok(l) => l,
        Err(e) => {
            ui.label(tr("elem.roof.not_built", &[("e", e.as_str())]));
            return;
        }
    };
    let u = ed.tool.units;
    let len = |v: f64| display_length(v, u);
    ui_kit::props(ui, &format!("elem.roof.live.{id}"), |ui| {
        ui_kit::kv_num(ui, &tr("elem.roof.plan_area", &[]), crate::unit_text::area_ui(l.plan_area));
        ui_kit::kv_num(ui, &tr("elem.roof.area", &[]), crate::unit_text::area_ui(l.area));
        ui_kit::kv_num(ui, &tr("elem.roof.volume", &[]), crate::unit_text::vol_ui(l.volume));
        ui_kit::kv_num(ui, &tr("elem.roof.thickness", &[]), len(l.thickness));
        ui_kit::kv_num(ui, &tr("elem.roof.height", &[]), len(l.height));
        for (key, v) in [
            ("elem.roof.ridge", l.ridge),
            ("elem.roof.hip", l.hip),
            ("elem.roof.valley", l.valley),
            ("elem.roof.eave", l.eave),
            ("elem.roof.rake", l.rake),
        ] {
            if v > 1e-9 {
                ui_kit::kv_num(ui, &tr(key, &[]), len(v));
            }
        }
        if l.gable > 1e-9 {
            ui_kit::kv_num(ui, &tr("elem.roof.gable", &[]), crate::unit_text::area_ui(l.gable));
        }
    });
}

fn shape(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    let Ok(spec) = ed.model.roof(id) else { return };
    if !ui_kit::heading(ui, &tr("elem.roof.shape", &[])) {
        return;
    }
    let (u, rev) = (ed.tool.units, ed.revision);
    let mut pending: Option<(Field, f64)> = None;
    ui_kit::props(ui, &format!("elem.roof.shape.{id}"), |ui| {
        ui_kit::kv(ui, &tr("elem.roof.kind", &[]), tr(kind_key(&spec), &[]));
        for (f, cur) in fields(&spec) {
            let name = label(f);
            let Some(val) = cur else {
                // a gable edge: offer to give it a slope
                let set = ui_kit::kv_widget(ui, &name, |ui| {
                    ui.button(tr("elem.roof.set_slope", &[])).clicked()
                });
                if set {
                    pending = Some((f, NEW_SLOPE));
                }
                continue;
            };
            let key = egui::Id::new(("elem.roof.field", id, f));
            let mut v = ui
                .data(|d| d.get_temp::<(u64, f64)>(key))
                .filter(|b| b.0 == rev)
                .map_or(val, |b| b.1);
            let done = if is_angle(f) {
                ui_kit::kv_field(ui, key, &name, &mut v, ui_kit::NumKind::DEG, 1.0, 88.0, "\u{b0}")
            } else {
                let kind = ui_kit::NumKind::Len(u);
                ui_kit::kv_field(ui, key, &name, &mut v, kind, 0.05, 1000.0, crate::units::unit_label(u))
            };
            ui.data_mut(|d| d.insert_temp(key, (rev, v)));
            if done && (v - val).abs() > 1e-9 {
                pending = Some((f, v));
            }
        }
    });
    if let Some((f, v)) = pending {
        commit_field(ed, id, f, v);
    }
}

/// One form field as one undo step; a refusal reaches the status line.
pub fn commit_field(ed: &mut Editor, id: i64, f: Field, v: f64) {
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let mut s = m.roof(id)?;
        if !set_field(&mut s, f, v) {
            return Err(Error::Invalid("roof: no such field".into()));
        }
        m.update_roof(id, &s)
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::fixture;

    fn spec(kind: RoofKind) -> RoofSpec {
        RoofSpec {
            level: 1,
            slab_type: 1,
            offset: 0.0,
            overhang: 0.0,
            fascia: 0.0,
            kind,
        }
    }

    fn gable_roof() -> RoofSpec {
        spec(RoofKind::Footprint {
            ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: Some((1.0, Some(60.0))),
        })
    }

    #[test]
    fn fields_follow_the_form_and_set_roundtrips() {
        let mut s = gable_roof();
        let f = fields(&s);
        assert_eq!(f.len(), 4 + 2);
        assert_eq!(f[1], (Field::Slope(1), None), "a gable edge has no value");
        assert!(set_field(&mut s, Field::Slope(1), 40.0));
        assert!(set_field(&mut s, Field::MansardH, 1.5));
        assert!(set_field(&mut s, Field::MansardDeg, 70.0));
        assert!(!set_field(&mut s, Field::Rise, 1.0), "no rise on a footprint");
        assert!(!set_field(&mut s, Field::Slope(9), 40.0));
        let f = fields(&s);
        assert_eq!(f[1].1, Some(40.0));
        assert_eq!(f[4], (Field::MansardH, Some(1.5)));
        assert_eq!(f[5], (Field::MansardDeg, Some(70.0)));

        let mut e = spec(RoofKind::Extrusion {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            profile: vec![(0.0, 0.0), (3.0, 2.0), (6.0, 0.0)],
            depth: 4.0,
        });
        assert_eq!(fields(&e), vec![(Field::Depth, Some(4.0))]);
        assert!(set_field(&mut e, Field::Depth, 5.0));
        let mut a = spec(RoofKind::Arrow {
            ring: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)],
            tail: (0.0, 0.0),
            head: (4.0, 0.0),
            rise: 1.0,
        });
        assert!(set_field(&mut a, Field::Rise, 2.0));
        assert_eq!(fields(&a), vec![(Field::Rise, Some(2.0))]);
    }

    #[test]
    fn extrusion_outline_is_the_plan_rectangle() {
        let e = spec(RoofKind::Extrusion {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            profile: vec![(0.0, 0.0), (3.0, 2.0), (6.0, 0.0)],
            depth: 4.0,
        });
        let o = outline(&e);
        assert_eq!(o.len(), 4);
        assert!((o[2].1 - 4.0).abs() < 1e-9 && (o[3].1 - 4.0).abs() < 1e-9, "left of a -> b");
        assert_eq!(center(&e), (3.0, 2.0));
    }

    #[test]
    fn translation_moves_every_point() {
        let a = spec(RoofKind::Arrow {
            ring: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)],
            tail: (0.0, 0.0),
            head: (4.0, 0.0),
            rise: 1.0,
        });
        let t = translated(&a, (1.0, 2.0));
        let RoofKind::Arrow {
            ring, tail, head, rise,
        } = t.kind
        else {
            panic!()
        };
        assert_eq!((ring[0], tail, head, rise), ((1.0, 2.0), (1.0, 2.0), (5.0, 2.0), 1.0));
    }

    #[test]
    fn measure_gives_live_numbers_and_they_follow_an_edit() {
        let mut fx = fixture();
        let id = fx.ids["roof"];
        let before = measure(&fx.ed.model, id).unwrap();
        assert!(before.area >= before.plan_area && before.height > 0.0 && before.thickness > 0.0);
        let mut s = fx.ed.model.roof(id).unwrap();
        let (f, cur) = fields(&s).into_iter().find_map(|(f, v)| v.map(|v| (f, v))).unwrap();
        let nv = if is_angle(f) { cur + 5.0 } else { cur + 0.5 };
        commit_field(&mut fx.ed, id, f, nv);
        s = fx.ed.model.roof(id).unwrap();
        assert!(fields(&s).iter().any(|x| *x == (f, Some(nv))), "field written");
        let after = measure(&fx.ed.model, id).unwrap();
        assert_ne!(before, after, "live geometry follows the field");
        fx.ed.undo();
        assert_eq!(measure(&fx.ed.model, id).unwrap(), before, "one undo restores it");
    }

    #[test]
    fn selected_roof_shows_a_move_grip_and_dragging_it_is_one_undo_step() {
        use crate::editor::{structx, Sel};
        use crate::grips::{self, Kind, Role};
        let mut fx = fixture();
        let id = fx.ids["roof"];
        let ed = &mut fx.ed;
        let sel = crate::elem::by_table("roof").unwrap().sel(id).unwrap();
        assert_eq!(sel, Sel::Ext(structx::ROOF_CODE, id));
        assert!(grips::visible(ed).iter().all(|g| g.kind != Kind::Roof), "no grips unselected");
        ed.selection = vec![sel];
        let g = grips::visible(ed)
            .into_iter()
            .find(|g| g.kind == Kind::Roof && g.role == Role::Move)
            .expect("move grip of the selected roof");
        let before = ed.model.roof(id).unwrap();
        let to = g.pos + ddc_geom::v2(1.5, -0.5);
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| grips::apply(m, &g, to))))
            .unwrap();
        let after = ed.model.roof(id).unwrap();
        let (c0, c1) = (center(&before), center(&after));
        assert!((c1.0 - c0.0 - 1.5).abs() < 1e-9 && (c1.1 - c0.1 + 0.5).abs() < 1e-9);
        assert_eq!(outline(&after).len(), outline(&before).len(), "moved whole, not reshaped");
        ed.undo();
        assert_eq!(ed.model.roof(id).unwrap(), before);
    }

    #[test]
    fn delete_takes_roof_alone_or_mixed_in_one_step_and_refuses_panels() {
        use crate::editor::Sel;
        let mut fx = fixture();
        let (id, wall) = (fx.ids["roof"], fx.ids["wall"]);
        let ed = &mut fx.ed;
        let sel = crate::elem::by_table("roof").unwrap().sel(id).unwrap();
        let count = |ed: &Editor, t: &str, i: i64| -> i64 {
            ed.model
                .sql()
                .query_row(&format!("SELECT count(*) FROM {t} WHERE id=?1"), [i], |r| r.get(0))
                .unwrap()
        };
        ed.selection = vec![sel];
        ed.delete_selection();
        assert_eq!(count(ed, "roof", id), 0);
        assert!(ed.selection.is_empty());
        ed.undo();
        assert_eq!(count(ed, "roof", id), 1, "one undo restores the roof");

        ed.selection = vec![sel, Sel::Wall(wall)];
        ed.delete_selection();
        assert_eq!((count(ed, "roof", id), count(ed, "wall", wall)), (0, 0));
        ed.undo();
        assert_eq!((count(ed, "roof", id), count(ed, "wall", wall)), (1, 1), "one step");

        ed.add_curtains(&[(ddc_geom::v2(0.0, 30.0), ddc_geom::v2(6.0, 30.0), 0.0)]);
        let cw = ed.curtains[0].id;
        ed.selection = vec![sel, Sel::Panel(cw, 2)];
        ed.delete_selection();
        assert_eq!(count(ed, "roof", id), 0, "the roof goes, the panel stays");
        assert_eq!(count(ed, "curtain_wall", cw), 1, "the panel never takes its wall");
        assert_eq!(ed.selection, vec![Sel::Panel(cw, 2)]);
        ed.undo();
        assert_eq!(count(ed, "roof", id), 1, "one undo step");
        assert_eq!(count(ed, "curtain_wall", cw), 1);
    }

    #[test]
    fn panel_declines_when_no_migrated_item_is_selected() {
        use crate::editor::Sel;
        let mut fx = fixture();
        fx.ed.selection = vec![Sel::Wall(fx.ids["wall"])];
        let ctx = egui::Context::default();
        let mut shown = true;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                shown = crate::elem::show(ui, &mut fx.ed);
            });
        });
        assert!(!shown);
    }

    #[test]
    fn plan_footprint_lists_each_roof_of_the_level() {
        let fx = fixture();
        let id = fx.ids["roof"];
        let got = footprints(&fx.ed.model, fx.ed.level);
        let one = got.iter().find(|x| x.0 == id).expect("roof listed");
        assert!(one.1[0].len() >= 2);
    }
}
