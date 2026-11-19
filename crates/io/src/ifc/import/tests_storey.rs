// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Storey elevation source: placement, Elevation attribute, flat placements.

use super::*;
use std::fmt::Write as _;

/// One storey: (placement z in mm or None, Elevation in mm or None, wall z relative to the
/// storey placement, or to the project when the storey has no placement).
type St = (Option<f64>, Option<f64>, f64);

fn fixture(storeys: &[St]) -> String {
    let mut s = String::from(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('x'),'2;1');\nFILE_NAME('t.ifc','2026-01-01T00:00:00',(''),(''),'','','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n#2=IFCUNITASSIGNMENT((#1));\n\
#3=IFCPROJECT('0000000000000000000P01',$,'P',$,$,$,$,$,#2);\n\
#10=IFCCARTESIANPOINT((0.,0.,0.));\n#11=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
#21=IFCDIRECTION((0.,0.,1.));\n\
#30=IFCCARTESIANPOINT((0.,0.));\n#35=IFCAXIS2PLACEMENT2D(#30,$);\n\
#36=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,250.);\n\
#37=IFCEXTRUDEDAREASOLID(#36,#11,#21,2800.);\n\
#39=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#37));\n\
#40=IFCPRODUCTDEFINITIONSHAPE($,$,(#39));\n",
    );
    let mut walls = vec![];
    for (i, (pz, el, wz)) in storeys.iter().enumerate() {
        let b = 100 + 10 * i as u32;
        let place = match pz {
            Some(z) => {
                write!(
                    s,
                    "#{b}=IFCCARTESIANPOINT((0.,0.,{z:?}));\n#{}=IFCAXIS2PLACEMENT3D(#{b},$,$);\n#{}=IFCLOCALPLACEMENT($,#{});\n",
                    b + 1,
                    b + 2,
                    b + 1
                )
                .unwrap();
                format!("#{}", b + 2)
            }
            None => "$".to_string(),
        };
        let elev = el.map_or("$".to_string(), |v| format!("{v:?}"));
        write!(
            s,
            "#{}=IFCBUILDINGSTOREY('00000000000000000S{i:04}',$,'L{i}',$,$,{place},$,$,.ELEMENT.,{elev});\n",
            b + 3
        )
        .unwrap();
        let parent = if pz.is_some() { place.clone() } else { "$".into() };
        write!(
            s,
            "#{}=IFCCARTESIANPOINT((0.,{},{wz:?}));\n#{}=IFCAXIS2PLACEMENT3D(#{},$,$);\n#{}=IFCLOCALPLACEMENT({parent},#{});\n#{}=IFCWALL('00000000000000000W{i:04}',$,'W{i}',$,$,#{},#40,$,$);\n",
            b + 4,
            i as f64 * 10000.0,
            b + 5,
            b + 4,
            b + 6,
            b + 5,
            b + 7,
            b + 6
        )
        .unwrap();
        walls.push((b + 3, b + 7));
    }
    for (k, (st, w)) in walls.iter().enumerate() {
        writeln!(
            s,
            "#{}=IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000R{k:04}',$,$,$,(#{w}),#{st});",
            900 + k
        )
        .unwrap();
    }
    s.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    s
}

fn levels(p: &Plan) -> Vec<f64> {
    p.levels.iter().map(|l| l.elevation).collect()
}

/// Absolute base z (metres) of every wall: level elevation + base offset.
fn wall_z(p: &Plan) -> Vec<f64> {
    p.walls
        .iter()
        .map(|w| p.levels[w.level].elevation + w.base_offset)
        .collect()
}

fn near(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

fn has_storey_warning(p: &Plan) -> bool {
    p.report.warnings.iter().any(|w| w.starts_with("storeys:"))
}

#[test]
fn storey_placement_only() {
    let p = plan_bytes(
        fixture(&[(Some(0.0), None, 0.0), (Some(3000.0), None, 0.0)]).as_bytes(),
    )
    .unwrap();
    assert!(near(&levels(&p), &[0.0, 3.0]), "{:?}", levels(&p));
    assert!(near(&wall_z(&p), &[0.0, 3.0]));
    assert!(!has_storey_warning(&p));
}

#[test]
fn storey_elevation_only() {
    let p = plan_bytes(
        fixture(&[(None, Some(0.0), 0.0), (None, Some(3000.0), 3000.0)]).as_bytes(),
    )
    .unwrap();
    assert!(near(&levels(&p), &[0.0, 3.0]), "{:?}", levels(&p));
    assert!(near(&wall_z(&p), &[0.0, 3.0]));
    assert!(!has_storey_warning(&p));
}

#[test]
fn storey_elevation_attr_when_placements_flat() {
    let p = plan_bytes(
        fixture(&[
            (Some(0.0), Some(0.0), 0.0),
            (Some(0.0), Some(3000.0), 3000.0),
            (Some(0.0), Some(6000.0), 6000.0),
        ])
        .as_bytes(),
    )
    .unwrap();
    assert!(near(&levels(&p), &[0.0, 3.0, 6.0]), "{:?}", levels(&p));
    // walls keep their absolute z and sit on their own level, no 3/6 m offsets
    assert!(near(&wall_z(&p), &[0.0, 3.0, 6.0]), "{:?}", wall_z(&p));
    assert!(p.walls.iter().all(|w| w.base_offset.abs() < 1e-6));
    assert!(has_storey_warning(&p));
}

#[test]
fn storey_placement_wins_when_consistent() {
    let p = plan_bytes(
        fixture(&[
            (Some(0.0), Some(0.0), 0.0),
            (Some(3000.0), Some(3000.0), 500.0),
        ])
        .as_bytes(),
    )
    .unwrap();
    assert!(near(&levels(&p), &[0.0, 3.0]));
    assert!(near(&wall_z(&p), &[0.0, 3.5]), "{:?}", wall_z(&p));
    assert!(!has_storey_warning(&p));
}

#[test]
fn storey_placement_wins_with_warning_when_disagreeing() {
    let p = plan_bytes(
        fixture(&[
            (Some(0.0), Some(0.0), 0.0),
            (Some(3000.0), Some(3500.0), 0.0),
        ])
        .as_bytes(),
    )
    .unwrap();
    assert!(near(&levels(&p), &[0.0, 3.0]), "{:?}", levels(&p));
    assert!(near(&wall_z(&p), &[0.0, 3.0]));
    assert!(has_storey_warning(&p), "{:?}", p.report.warnings);
}

#[test]
fn levels_distinct_after_flat_placement() {
    let p = plan_bytes(
        fixture(&[
            (Some(0.0), Some(0.0), 0.0),
            (Some(0.0), Some(3000.0), 3200.0),
        ])
        .as_bytes(),
    )
    .unwrap();
    let lv = levels(&p);
    assert!(lv[1] - lv[0] > 2.9, "{lv:?}");
    // wall above its level by 0.2 m keeps absolute z 3.2
    assert!(near(&wall_z(&p), &[0.0, 3.2]), "{:?}", wall_z(&p));
    assert!((p.walls[1].base_offset - 0.2).abs() < 1e-6);
}
