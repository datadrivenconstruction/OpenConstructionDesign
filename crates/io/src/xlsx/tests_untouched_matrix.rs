// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An untouched book writes nothing: {metric, imperial} x {point, comma book mark} x {plain,
//! professional book} x {numbers, every numeric cell saved as text} over the element sheets and
//! the project parameter columns. Export, re-read, preview -> no change, apply -> no operation.
//! The schedule book and the imported properties are covered in the app crate
//! (`untouched_matrix_tests.rs`). Marks and units are passed explicitly; no global is set.

use std::collections::HashMap;

use calamine::Data;
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, Just, LayerSpec, MemberShape, Model, OpeningSpec,
    PVal, ParamSet, ParamSpec, ParamType, RoofKind, RoofSpec, WallSpec,
};

use super::book::Style;
use super::tests::{edit, tmp};
use super::*;

const UNITS: [Units; 2] = [Units::Metric, Units::Imperial];
/// Book decimal mark: false = point, true = comma.
const MARKS: [bool; 2] = [false, true];
/// Professional book (captions, key row, dropdowns) or the plain one.
const PRO: [bool; 2] = [false, true];

fn ring() -> Vec<(f64, f64)> {
    vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)]
}

fn spec(name: &str, t: ParamType, per_type: bool, cats: &[&str]) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: t,
        per_type,
        unit: if t == ParamType::Number { "kg".into() } else { String::new() },
        group: String::new(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

fn pv(param: i64, kind: &str, id: i64, v: PVal) -> ParamSet {
    ParamSet { param, kind: kind.into(), id, value: Some(v) }
}

/// Walls with a door, a column, a beam, a roof and a ceiling; with fractional dimensions so
/// that neither unit system shows round numbers. `params` adds one parameter per type.
fn model(params: bool) -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layers = [("brick", 0.2), ("insulation", 0.1)].map(|(n, t)| LayerSpec {
        material: n.into(),
        thickness: t,
    });
    let wt = m.add_wall_type("ext", &layers).unwrap();
    let p = [(0.0, 0.0), (5.3, 0.0), (5.3, 4.1), (0.0, 4.1)];
    let mut walls = vec![];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[(i + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.07,
            wall_type: wt,
            offset0: None,
        };
        walls.push(m.add_wall(&w).unwrap());
    }
    let door = OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
    m.add_opening(walls[0], &door).unwrap();
    let st = m
        .add_slab_type("deck", &[LayerSpec { material: "concrete".into(), thickness: 0.2 }])
        .unwrap();
    let mt = m
        .add_member_type("C400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "concrete")
        .unwrap();
    let col = m
        .add_column(&ColumnSpec {
            level: lv,
            top_level: None,
            member_type: mt,
            structural: true,
            at: (1.0, 2.0),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    m.add_beam(&BeamSpec {
        level: lv,
        member_type: mt,
        a: (0.0, 0.0, 3.0),
        b: (5.0, 0.0, 3.0),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Pos,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    })
    .unwrap();
    m.add_roof(&RoofSpec {
        level: lv,
        slab_type: st,
        offset: 3.0,
        overhang: 0.3,
        fascia: 0.2,
        kind: RoofKind::Footprint { ring: ring(), slopes: vec![Some(30.0); 4], mansard: None },
    })
    .unwrap();
    m.add_ceiling(&CeilingSpec { level: lv, slab_type: st, height: 2.7, outer: ring(), holes: vec![] })
        .unwrap();
    if params {
        let len = m.add_param(&spec("Offset", ParamType::Length, false, &["wall", "col"])).unwrap();
        let area = m.add_param(&spec("Paint", ParamType::Area, false, &["wall"])).unwrap();
        let vol = m.add_param(&spec("Fill", ParamType::Volume, false, &["wall"])).unwrap();
        let num = m.add_param(&spec("Load", ParamType::Number, false, &["wall", "col"])).unwrap();
        let int = m.add_param(&spec("Count", ParamType::Int, false, &["wall"])).unwrap();
        let txt = m.add_param(&spec("Zone", ParamType::Text, false, &["wall"])).unwrap();
        let yes = m.add_param(&spec("Rated", ParamType::YesNo, false, &["wall"])).unwrap();
        let tlen = m.add_param(&spec("Cover", ParamType::Length, true, &["col", "beam"])).unwrap();
        let mut sets = vec![
            pv(len, "col", col, PVal::Num(0.1234)),
            pv(num, "col", col, PVal::Num(2.5)),
            pv(tlen, "member_type", mt, PVal::Num(0.0375)),
        ];
        for (i, w) in walls.iter().enumerate() {
            let x = i as f64 + 1.0;
            sets.push(pv(len, "wall", *w, PVal::Num(0.3048 * x + 0.0101)));
            sets.push(pv(area, "wall", *w, PVal::Num(12.3456 * x)));
            sets.push(pv(vol, "wall", *w, PVal::Num(7.891 * x)));
            sets.push(pv(num, "wall", *w, PVal::Num(1234.5678 * x)));
            sets.push(pv(int, "wall", *w, PVal::Int(3 * (i as i64 + 1))));
            sets.push(pv(txt, "wall", *w, PVal::Text(format!("Z-{}", i + 1))));
            sets.push(pv(yes, "wall", *w, PVal::Bool(i % 2 == 0)));
        }
        m.set_param_values(&sets).unwrap();
    }
    m
}

fn ops(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0)).unwrap()
}

/// Numbers typed as text the way a spreadsheet shows them in the book's mark.
fn as_text(x: f64, comma: bool) -> String {
    let t = format!("{x}");
    if comma {
        t.replace('.', ",")
    } else {
        t
    }
}

/// Every numeric cell of the data sheets becomes text; the id columns and the machine sheets
/// stay as they are (an id saved as text is a different story: the row key).
fn all_numbers_as_text(sheet: &str, g: &mut Vec<Vec<Data>>, comma: bool) {
    if [INFO, BASELINE, KEYS, LISTS].contains(&sheet) || g.is_empty() {
        return;
    }
    let skip: Vec<bool> = g[0]
        .iter()
        .enumerate()
        .map(|(j, h)| {
            let h = h.to_string().to_lowercase();
            let h = h.trim().trim_end_matches("(read-only)").trim().to_string();
            j == 0 || h == "id" || h.ends_with(" id") || ["pos", "wall", "wtype", "stype"].contains(&h.as_str())
        })
        .collect();
    for row in g.iter_mut().skip(1) {
        for (j, d) in row.iter_mut().enumerate() {
            if skip.get(j).copied().unwrap_or(false) {
                continue;
            }
            match d {
                Data::Float(x) => *d = Data::String(as_text(*x, comma)),
                Data::Int(i) => *d = Data::String(i.to_string()),
                _ => {}
            }
        }
    }
}

fn noop(rep: &Report) -> Result<(), String> {
    if rep.changes.is_empty()
        && rep.rejected.is_empty()
        && rep.ignored.is_empty()
        && rep.conflicts.is_empty()
        && rep.proposals.is_empty()
        && rep.edits.is_empty()
        && rep.props.is_empty()
        && rep.removed.is_empty()
        && rep.model_newer == 0
    {
        return Ok(());
    }
    Err(format!(
        "changes {:?}; rejected {:?}; ignored {:?}; conflicts {:?}; proposals {}; edits {}; model_newer {}",
        rep.changes.iter().take(3).collect::<Vec<_>>(),
        rep.rejected.iter().take(3).collect::<Vec<_>>(),
        rep.ignored.iter().take(3).collect::<Vec<_>>(),
        rep.conflicts.iter().take(3).collect::<Vec<_>>(),
        rep.proposals.len(),
        rep.edits.len(),
        rep.model_newer,
    ))
}

fn one(params: bool, u: Units, comma: bool, pro: bool, text: bool) -> Result<(), String> {
    let mut m = model(params);
    let tag = format!(
        "mx_{}{}{}{}{}",
        u8::from(params),
        u8::from(u == Units::Imperial),
        u8::from(comma),
        u8::from(pro),
        u8::from(text)
    );
    let p = tmp(&tag);
    let style = if pro { Style::professional(HashMap::new()) } else { Style::default() }
        .with_number_format(comma, "");
    export_xlsx_styled(&m, &p, &p, u, &[], &style)?;
    let src = if text {
        let q = tmp(&format!("{tag}_text"));
        edit(&p, &q, |sheet, g| all_numbers_as_text(sheet, g, comma));
        q
    } else {
        p
    };
    let before = ops(&m);
    let preview = ImportOptions { apply: false, delete_missing: false, new_params: false };
    let rep = import_xlsx_with(&mut m, &src, preview)?;
    noop(&rep).map_err(|e| format!("preview: {e}"))?;
    let rep = import_xlsx(&mut m, &src)?;
    if !rep.applied {
        return Err(format!("apply: not applied: {rep:?}"));
    }
    noop(&rep).map_err(|e| format!("apply: {e}"))?;
    let after = ops(&m);
    if after != before {
        return Err(format!("apply wrote {} operations", after - before));
    }
    Ok(())
}

fn matrix(params: bool) {
    let mut bad = vec![];
    for u in UNITS {
        for comma in MARKS {
            for pro in PRO {
                for text in [false, true] {
                    if let Err(e) = one(params, u, comma, pro, text) {
                        bad.push(format!(
                            "units {u:?} comma {comma} professional {pro} as_text {text}: {e}"
                        ));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} of 16 cells fail:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn untouched_element_sheets_write_nothing_in_every_units_and_mark() {
    matrix(false);
}

#[test]
fn untouched_project_parameters_write_nothing_in_every_units_and_mark() {
    matrix(true);
}
