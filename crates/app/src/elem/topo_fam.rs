//! Topography family of the element contract: the plan outline that makes a terrain selectable
//! by a click inside it, and the properties part for editing its survey points in the plan.

use crate::editor::bodies::hull;
use crate::editor::Editor;
use crate::topo::{self, Tool};
use crate::units::display_length;
use crate::{tr, ui_kit};
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, TopoSpec};

use super::Rings;

/// The boundary when the terrain has one, otherwise the hull of its points.
pub fn outline(s: &TopoSpec) -> Vec<V2> {
    if s.boundary.len() >= 3 {
        return s.boundary.iter().map(|p| v2(p.0, p.1)).collect();
    }
    hull(s.points.iter().map(|p| v2(p.0, p.1)).collect())
}

/// Plan outlines of all terrains (they belong to the site, not to a level), for `StructFp`.
pub fn outlines(m: &Model) -> Vec<(i64, Vec<V2>)> {
    m.toposolids()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, _)| m.toposolid(id).ok().map(|s| (id, outline(&s))))
        .filter(|x| x.1.len() >= 3)
        .collect()
}

pub fn footprints(m: &Model, _level: i64) -> Result<Vec<(i64, Rings)>, Error> {
    Ok(outlines(m)
        .into_iter()
        .map(|(id, r)| (id, vec![r.iter().map(|p| (p.x, p.y)).collect()]))
        .collect())
}

/// Elevation shown for the picked points: the common value, or the first one when they differ.
fn picked_z(ed: &Editor, id: i64) -> Option<(f64, bool)> {
    let pts = topo::points_of(ed, id);
    let mut zs = ed
        .topo
        .picked
        .iter()
        .filter_map(|i| pts.iter().find(|p| p.0 == *i).map(|p| p.1 .2));
    let first = zs.next()?;
    let same = zs.all(|z| (z - first).abs() < 1e-9);
    Some((first, same))
}

/// Survey points: count and range, the switch into point editing, and the elevation of the
/// picked points. Picking and dragging happen in the plan; the value is typed here or in the plan.
pub fn panel(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    if !ui_kit::heading(ui, &tr("elem.toposolid.points", &[])) {
        return;
    }
    let pts = topo::points_of(ed, id);
    let u = ed.tool.units;
    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| {
        (a.0.min(p.1 .2), a.1.max(p.1 .2))
    });
    ui_kit::props(ui, &format!("elem.topo.points.{id}"), |ui| {
        ui_kit::kv_num(ui, &tr("elem.toposolid.count", &[]), pts.len().to_string());
        if !pts.is_empty() {
            ui_kit::kv_num(ui, &tr("elem.toposolid.low", &[]), display_length(lo, u));
            ui_kit::kv_num(ui, &tr("elem.toposolid.high", &[]), display_length(hi, u));
        }
    });
    let editing = ed.topo.tool == Some(Tool::Edit) && topo::active(ed) == Some(id);
    ui.add_space(4.0);
    if !editing {
        let b = ui
            .button(tr("elem.toposolid.edit", &[]))
            .on_hover_text(tr("elem.toposolid.edit_tip", &[]));
        if b.clicked() {
            topo::edit_points(ed, id);
        }
        return;
    }
    let note = |ui: &mut egui::Ui, text: String| {
        ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
    };
    match picked_z(ed, id) {
        None => note(ui, tr("elem.toposolid.pick_hint", &[])),
        Some((z, same)) => {
            let key = egui::Id::new(("elem.topo.z", id));
            let sig = (ed.revision, ed.topo.picked.clone());
            let mut v = ui
                .data(|d| d.get_temp::<(u64, Vec<i64>, f64)>(key))
                .filter(|b| b.0 == sig.0 && b.1 == sig.1)
                .map_or(z, |b| b.2);
            let mut done = false;
            let label = tr("elem.toposolid.z", &[("n", ed.topo.picked.len().to_string().as_str())]);
            ui_kit::props(ui, &format!("elem.topo.z.{id}"), |ui| {
                let kind = ui_kit::NumKind::Len(u);
                done = ui_kit::kv_field(ui, key, &label, &mut v, kind, -1e4, 1e4, crate::units::unit_label(u));
            });
            ui.data_mut(|d| d.insert_temp(key, (sig.0, sig.1, v)));
            if !same {
                note(ui, tr("elem.toposolid.mixed", &[]));
            }
            // focus lost without an edit must not flatten mixed points to the first value
            if done && (v - z).abs() > 1e-9 {
                topo::set_picked_z(ed, v);
            }
        }
    }
    note(ui, tr("elem.toposolid.edit_help", &[]));
    ui.add_space(4.0);
    if ui.button(tr("elem.toposolid.done", &[])).clicked() {
        topo::disarm(ed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(points: Vec<(f64, f64, f64)>, boundary: Vec<(f64, f64)>) -> TopoSpec {
        TopoSpec {
            name: "t".into(),
            thickness: 1.0,
            material: "earth".into(),
            points,
            breaklines: vec![],
            boundary,
        }
    }

    #[test]
    fn outline_is_the_boundary_or_the_hull() {
        let pts = vec![(0.0, 0.0, 1.0), (4.0, 0.0, 1.0), (4.0, 4.0, 1.0), (0.0, 4.0, 1.0), (2.0, 2.0, 5.0)];
        assert_eq!(outline(&spec(pts.clone(), vec![])).len(), 4);
        let b = vec![(-1.0, -1.0), (9.0, -1.0), (9.0, 9.0)];
        assert_eq!(outline(&spec(pts, b.clone())), vec![v2(-1.0, -1.0), v2(9.0, -1.0), v2(9.0, 9.0)]);
    }
}
