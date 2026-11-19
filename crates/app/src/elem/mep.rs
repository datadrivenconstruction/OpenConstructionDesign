// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! MEP runs and devices of the element contract (E4): the registry rows that were missing
//! (system, start elevation, slope, material for a run; elevation and rotation for a device)
//! and the properties panel of a selected device. A run keeps its own section panel in
//! `mep3d::props`, which shows these rows below its size fields.
//!
//! Every setter is one model op, so a row edit is one undo step. A refusal returns a sentence
//! that names the cause and the way out; the panel shows it in the status line.

use crate::editor::{Editor, Sel};
use crate::geom_props::{Effect, Get, Row, Set, Unit};
use crate::{tr, ui_kit};
use ddc_geom::mep::apply_slope;
use ddc_geom::v3;
use ddc_model::{Error, MepDomain, MepShape, Model, PipeMaterial};

/// Index 0 is "no material"; the rest follow `PipeMaterial::ALL`.
pub const MATERIALS: &[&str] = &[
    "mep.mat.none",
    "mep.mat.steel",
    "mep.mat.copper",
    "mep.mat.pvc",
    "mep.mat.pp",
];

const ELEVATION: (f64, f64) = (-1e4, 1e4);
const SLOPE: (f64, f64) = (0.0, 100.0);
const DEG: (f64, f64) = (-360.0, 360.0);

fn refuse<T>(msg: String) -> Result<T, Error> {
    Err(Error::Invalid(msg))
}

/// Text of a model error in the interface language: `mep.err.*` keys become sentences.
pub fn explain(e: &Error) -> String {
    match e {
        Error::Invalid(k) if k.starts_with("mep.err.") => tr(k, &[]),
        e => e.to_string(),
    }
}

fn says(e: Error) -> Error {
    Error::Invalid(explain(&e))
}

fn domain_name(d: MepDomain) -> String {
    tr(crate::mep3d::domain_key(d), &[])
}

fn run_system(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.mep_run(id)?.system as f64)
}

/// A run keeps its network kind: a duct system takes no pipe, so the refusal says which kind
/// the run is and the picker stays as it was.
fn set_system(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.mep_run(id)?;
    let Some(sys) = m.mep_systems()?.into_iter().find(|x| x.id == v as i64) else {
        return refuse(tr("mep.row.no_system", &[]));
    };
    if !sys.kind.domains().contains(&s.section.domain) {
        let kind = domain_name(s.section.domain);
        return refuse(tr(
            "mep.row.system_domain",
            &[("system", sys.name.as_str()), ("kind", kind.as_str())],
        ));
    }
    s.system = sys.id;
    m.update_mep_run(id, &s)
}

fn run_elevation(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.mep_run(id)?.points.first().map_or(0.0, |p| p.2))
}

/// Moves the whole run vertically so that its first point lands on `v`; the fall stays.
fn set_elevation(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.mep_run(id)?;
    let Some(z0) = s.points.first().map(|p| p.2) else {
        return refuse(tr("mep.row.no_points", &[]));
    };
    let d = v - z0;
    s.points.iter_mut().for_each(|p| p.2 += d);
    m.mep_edit_run(id, &s).map_err(says)
}

fn run_slope(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.mep_run(id)?.slope)
}

/// The model stores points with the fall already applied, so the row rewrites them: the first
/// point stays, every gentle link falls by `v` percent of its plan length, steep links keep
/// their own rise. Zero levels the gentle links again.
fn set_slope(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.mep_run(id)?;
    let pts: Vec<_> = s.points.iter().map(|p| v3(p.0, p.1, p.2)).collect();
    s.points = apply_slope(&pts, v)
        .into_iter()
        .map(|p| (p.x, p.y, p.z))
        .collect();
    s.slope = v;
    m.mep_edit_run(id, &s).map_err(says)
}

fn run_material(m: &Model, id: i64) -> Result<f64, Error> {
    let s = m.mep_run(id)?;
    Ok(s.material
        .and_then(|x| PipeMaterial::ALL.iter().position(|y| *y == x))
        .map_or(0.0, |i| (i + 1) as f64))
}

/// A material brings its catalog: the run keeps its nominal size when the catalog has it, else
/// takes the nearest outer diameter, and the section follows, so the change shows in 3D.
fn set_material(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.mep_run(id)?;
    let Some(mat) = (v as usize)
        .checked_sub(1)
        .and_then(|i| PipeMaterial::ALL.get(i).copied())
    else {
        s.material = None;
        return m.update_mep_run(id, &s);
    };
    if s.section.domain != MepDomain::Pipe {
        return refuse(tr(
            "mep.row.material_pipes",
            &[("kind", domain_name(s.section.domain).as_str())],
        ));
    }
    let od = s.section.shape.wh().0;
    let size = s
        .dn
        .and_then(|dn| mat.by_dn(dn))
        .or_else(|| {
            mat.sizes()
                .into_iter()
                .min_by(|a, b| (a.od - od).abs().total_cmp(&(b.od - od).abs()))
        })
        .ok_or_else(|| Error::Invalid(tr("mep.row.no_catalog", &[])))?;
    s.material = Some(mat);
    s.dn = Some(size.dn);
    s.section.shape = MepShape::Round { d: size.od };
    s.section.wall = size.wall;
    m.update_mep_run(id, &s)
}

fn comp_elevation(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.component(id)?.at.2)
}

fn set_comp_elevation(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.component(id)?;
    s.at.2 = v;
    m.mep_update_device(id, &s).map_err(says)
}

fn comp_rotation(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.component(id)?.rotation)
}

fn set_comp_rotation(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.component(id)?;
    s.rotation = v;
    m.mep_update_device(id, &s).map_err(says)
}

/// Registry rows of the family; `geom_props::rows` appends them.
pub fn rows() -> Vec<Row> {
    let row = |kind: &'static str,
               key: &'static str,
               unit: Unit,
               range: (f64, f64),
               delta: f64,
               fields: &'static [&'static str],
               get: Get,
               set: Set| Row {
        kind,
        key,
        unit,
        lo: range.0,
        hi: range.1,
        delta,
        effect: Effect::Scene,
        fields,
        in_panel: true,
        get,
        set,
    };
    vec![
        row(
            "mep_run",
            "mep.row.system",
            Unit::Type("mep_system"),
            (0.0, 1e12),
            0.0,
            &["system"],
            run_system,
            set_system,
        ),
        row(
            "mep_run",
            "mep.row.elevation",
            Unit::Len,
            ELEVATION,
            0.5,
            &["points"],
            run_elevation,
            set_elevation,
        ),
        row(
            "mep_run",
            "mep.row.slope",
            Unit::Pct,
            SLOPE,
            1.0,
            &["slope"],
            run_slope,
            set_slope,
        ),
        row(
            "mep_run",
            "mep.row.material",
            Unit::Choice(MATERIALS),
            (0.0, (MATERIALS.len() - 1) as f64),
            1.0,
            &["material"],
            run_material,
            set_material,
        ),
        row(
            "comp",
            "mep.row.dev_elevation",
            Unit::Len,
            ELEVATION,
            0.25,
            &["at"],
            comp_elevation,
            set_comp_elevation,
        ),
        row(
            "comp",
            "geom.rotation",
            Unit::Deg,
            DEG,
            0.25,
            &["rotation"],
            comp_rotation,
            set_comp_rotation,
        ),
    ]
}

/// Rows of a selected run, drawn by the run panel under its size fields. The size fields keep
/// an edit buffer, so a change made here drops it and the next frame reads the new section.
pub fn run_rows(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    let rev = ed.revision;
    crate::geom_props::show(ui, ed, "mep_run", id);
    if ed.revision != rev {
        ed.big.edit = None;
    }
}

/// Properties of a selected device. False: no device is selected.
pub fn comp_props(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    let Some(id) = ed.selection.iter().find_map(|s| match s {
        Sel::Comp(i) => Some(*i),
        _ => None,
    }) else {
        return false;
    };
    if ed.model.component(id).is_err() {
        return false;
    }
    ui_kit::element_header(ui, "component", &tr("elem.comp.one", &[]), "", id);
    crate::geom_props::show(ui, ed, "comp", id);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::OpFn;
    use crate::geom_props::tests::fixture;
    use ddc_model::{RunSpec, SystemKind};
    use std::sync::Arc;

    fn apply(ed: &mut Editor, f: Set, id: i64, v: f64) {
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| f(m, id, v))))
            .unwrap();
    }

    fn fails(ed: &mut Editor, f: Set, id: i64, v: f64) -> String {
        let head = ed.model.change_head();
        let e = ed
            .run_op(OpFn(Arc::new(move |m: &mut Model| f(m, id, v))))
            .expect_err("must be refused");
        assert_eq!(ed.model.change_head(), head, "a refusal must not touch the model");
        e.to_string()
    }

    #[test]
    fn slope_levels_back_and_undoes_in_one_step() {
        let mut fx = fixture();
        let id = fx.ids["mep_run"];
        let z = |ed: &Editor| -> Vec<f64> {
            ed.model.mep_run(id).unwrap().points.iter().map(|p| p.2).collect()
        };
        let flat = z(&fx.ed);
        apply(&mut fx.ed, set_slope, id, 2.0);
        let sloped = z(&fx.ed);
        assert!((flat[0] - sloped[0]).abs() < 1e-9);
        // six metres in plan at two percent
        assert!((flat[1] - sloped[1] - 0.12).abs() < 1e-9, "{sloped:?}");
        apply(&mut fx.ed, set_slope, id, 0.0);
        assert_eq!(z(&fx.ed), flat);
        fx.ed.undo();
        assert_eq!(z(&fx.ed), sloped);
        fx.ed.undo();
        assert_eq!(z(&fx.ed), flat);
    }

    #[test]
    fn elevation_keeps_the_fall() {
        let mut fx = fixture();
        let id = fx.ids["mep_run"];
        apply(&mut fx.ed, set_slope, id, 1.0);
        let before = fx.ed.model.mep_run(id).unwrap().points;
        apply(&mut fx.ed, set_elevation, id, before[0].2 + 0.5);
        let after = fx.ed.model.mep_run(id).unwrap().points;
        for (a, b) in before.iter().zip(&after) {
            assert!((b.2 - a.2 - 0.5).abs() < 1e-9);
        }
        fx.ed.undo();
        assert_eq!(fx.ed.model.mep_run(id).unwrap().points, before);
    }

    #[test]
    fn system_of_another_kind_is_refused_with_a_reason() {
        let mut fx = fixture();
        let id = fx.ids["mep_run"];
        let air = fx.ed.model.add_mep_system("Air", SystemKind::SupplyAir).unwrap();
        let before = fx.ed.model.mep_run(id).unwrap();
        let why = fails(&mut fx.ed, set_system, id, air as f64);
        assert!(why.contains("Air") && why.contains(&domain_name(MepDomain::Pipe)), "{why}");
        assert_eq!(fx.ed.model.mep_run(id).unwrap(), before);
        let other = fx.ed.model.add_mep_system("Hot", SystemKind::HotWater).unwrap();
        apply(&mut fx.ed, set_system, id, other as f64);
        assert_eq!(fx.ed.model.mep_run(id).unwrap().system, other);
    }

    #[test]
    fn material_takes_its_catalog_and_pipes_only() {
        let mut fx = fixture();
        let id = fx.ids["mep_run"];
        let before = fx.ed.model.mep_run(id).unwrap();
        // copper catalog index 2
        apply(&mut fx.ed, set_material, id, 2.0);
        let s = fx.ed.model.mep_run(id).unwrap();
        assert_eq!(s.material, Some(PipeMaterial::Copper));
        assert_eq!(s.dn, before.dn);
        assert_ne!(s.section.shape.wh().0, before.section.shape.wh().0);
        fx.ed.undo();
        assert_eq!(fx.ed.model.mep_run(id).unwrap(), before);

        let level = before.level;
        let air = fx.ed.model.add_mep_system("Air", SystemKind::SupplyAir).unwrap();
        let sec = ddc_model::MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
        let duct = fx
            .ed
            .model
            .add_mep_run(&RunSpec::new(air, level, sec, vec![(0.0, 40.0, 3.0), (4.0, 40.0, 3.0)]))
            .unwrap();
        let why = fails(&mut fx.ed, set_material, duct, 1.0);
        assert!(why.contains(&domain_name(MepDomain::Duct)), "{why}");
    }

    #[test]
    fn device_rows_exist_and_move_the_device() {
        let all = rows();
        assert!(all.iter().any(|r| r.kind == "comp" && r.in_panel));
        let mut fx = fixture();
        let id = fx.ids["comp"];
        apply(&mut fx.ed, set_comp_elevation, id, 1.2);
        assert_eq!(fx.ed.model.component(id).unwrap().at.2, 1.2);
        fx.ed.undo();
        assert_eq!(fx.ed.model.component(id).unwrap().at.2, 0.0);
    }

    #[test]
    fn follow_refusals_read_as_sentences_in_both_languages() {
        for k in ["mep.err.follow_blocked", "mep.err.follow_slope", "mep.err.follow_device", "mep.err.follow_port_lost"] {
            let t = explain(&Error::Invalid(k.into()));
            assert_ne!(t, k, "the key must be translated");
            assert!(t.len() > 30, "{t}");
        }
    }
}
