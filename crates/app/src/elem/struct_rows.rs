// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Geometry rows of the foundation and framing families: footings, strip footings, rafts,
//! braces, trusses, beam systems and ramps. They join `geom_props::rows()`, so the panel, the
//! every-row test and the XLSX sheets read them like any other row.

use crate::geom_props::{Effect, Row, Unit};
use ddc_model::Model;

const LEN: (f64, f64) = (-1e4, 1e4);
const SIZE: (f64, f64) = (0.001, 1e3);
const DEG: (f64, f64) = (-360.0, 360.0);

/// One row over a spec with `read` and `write` methods: `r!(kind, key, unit, range, delta,
/// fields, read, write, |s| s.place)`.
macro_rules! r {
    ($kind:literal, $key:literal, $unit:expr, $range:expr, $delta:expr, $fields:expr,
     $read:ident, $write:ident, |$s:ident| $place:expr) => {
        Row {
            kind: $kind,
            key: $key,
            unit: $unit,
            lo: $range.0,
            hi: $range.1,
            delta: $delta,
            effect: Effect::Scene,
            fields: $fields,
            // the family's own panel (struct_props) already shows these values
            in_panel: false,
            get: |m: &Model, id: i64| {
                let $s = m.$read(id)?;
                Ok($place as f64)
            },
            set: |m: &mut Model, id: i64, v: f64| {
                let mut $s = m.$read(id)?;
                $place = v as _;
                m.$write(id, &$s)
            },
        }
    };
}

/// Row over the first (base) step of a footing. No indexing: a footing with an empty step
/// list reads 0, and writing it back is refused by the model's own check.
macro_rules! step_row {
    ($key:literal, $delta:expr, $f:ident) => {
        Row {
            kind: "iso_footing",
            key: $key,
            unit: Unit::Len,
            lo: SIZE.0,
            hi: SIZE.1,
            delta: $delta,
            effect: Effect::Scene,
            fields: &["steps"],
            in_panel: false,
            get: |m: &Model, id: i64| Ok(m.footing(id)?.steps.first().map_or(0.0, |p| p.$f)),
            set: |m: &mut Model, id: i64, v: f64| {
                let mut s = m.footing(id)?;
                if let Some(p) = s.steps.first_mut() {
                    p.$f = v;
                }
                m.update_footing(id, &s)
            },
        }
    };
}

#[rustfmt::skip]
pub fn rows() -> Vec<Row> {
    let mt = Unit::Type("member_type");
    vec![
        r!("iso_footing", "geom.top_offset", Unit::Len, LEN, 0.25, &["top_offset"],
            footing, update_footing, |s| s.top_offset),
        r!("iso_footing", "geom.rotation", Unit::Deg, DEG, 0.3, &["rotation"],
            footing, update_footing, |s| s.rotation),
        step_row!("struct_rows.base_width", 0.2, w),
        step_row!("struct_rows.base_length", 0.2, l),
        step_row!("struct_rows.base_height", 0.1, h),
        r!("strip_footing", "geom.width", Unit::Len, SIZE, 0.2, &["width"],
            strip_footing, update_strip_footing, |s| s.width),
        r!("strip_footing", "struct_rows.depth", Unit::Len, SIZE, 0.1, &["depth"],
            strip_footing, update_strip_footing, |s| s.depth),
        r!("strip_footing", "geom.top_offset", Unit::Len, LEN, 0.25, &["top_offset"],
            strip_footing, update_strip_footing, |s| s.top_offset),
        r!("strip_footing", "geom.offset", Unit::Len, LEN, 0.2, &["ecc"],
            strip_footing, update_strip_footing, |s| s.ecc),
        r!("raft", "geom.thickness", Unit::Len, SIZE, 0.1, &["thickness"],
            raft, update_raft, |s| s.thickness),
        r!("raft", "geom.top_offset", Unit::Len, LEN, 0.25, &["top_offset"],
            raft, update_raft, |s| s.top_offset),
        r!("brace", "geom.z_start", Unit::Len, LEN, 0.5, &["a"],
            brace, update_brace, |s| s.a.2),
        r!("brace", "geom.z_end", Unit::Len, LEN, 0.5, &["b"],
            brace, update_brace, |s| s.b.2),
        r!("brace", "geom.rotation", Unit::Deg, DEG, 0.3, &["rotation"],
            brace, update_brace, |s| s.rotation),
        r!("brace", "geom.type", mt, LEN, 0.0, &["member_type"],
            brace, update_brace, |s| s.member_type),
        r!("truss", "geom.height", Unit::Len, SIZE, 0.3, &["height"],
            truss, update_truss, |s| s.height),
        r!("truss", "struct_rows.heel", Unit::Len, LEN, 0.1, &["heel"],
            truss, update_truss, |s| s.heel),
        r!("beam_system", "struct_rows.spacing", Unit::Len, SIZE, 0.5, &["spacing"],
            beam_system, update_beam_system, |s| s.spacing),
        r!("beam_system", "struct_rows.angle", Unit::Deg, DEG, 0.3, &["angle"],
            beam_system, update_beam_system, |s| s.angle),
        r!("beam_system", "struct_rows.ref_z", Unit::Len, LEN, 0.25, &["z"],
            beam_system, update_beam_system, |s| s.z),
        r!("ramp", "geom.width", Unit::Len, SIZE, 0.3, &["width"],
            ramp, update_ramp, |s| s.width),
        r!("ramp", "geom.thickness", Unit::Len, SIZE, 0.1, &["thickness"],
            ramp, update_ramp, |s| s.thickness),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{FootingSpec, FootingStep};

    #[test]
    fn footing_without_steps_does_not_panic() {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L0", 0.0).unwrap();
        let id = m
            .add_footing(&FootingSpec {
                level,
                column: None,
                at: (0.0, 0.0),
                rotation: 0.0,
                round: false,
                steps: vec![FootingStep { w: 1.0, l: 1.0, h: 0.5 }],
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        m.sql().execute("UPDATE iso_footing SET steps=''", []).unwrap();
        for r in rows().iter().filter(|r| r.key.starts_with("struct_rows.base_")) {
            assert_eq!((r.get)(&m, id).unwrap(), 0.0, "{}", r.key);
            assert!((r.set)(&mut m, id, 1.0).is_err(), "{}", r.key);
        }
    }
}
