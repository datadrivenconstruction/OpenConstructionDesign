//! Multi-selection properties: shared values, varying values, one undo step, refusals.

use super::tests::{fixture, square, Fx};
use super::*;
use crate::editor::bodies::BodyKind;
use ddc_model::RoofKind;

fn op_count(ed: &Editor) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

/// The fixture roof and two more like it: (30, 30) sloped edges, two gables each.
fn three_roofs(fx: &mut Fx) -> Vec<i64> {
    let base = fx.ids["roof"];
    let mut ids = vec![base];
    for x in [40.0, 50.0] {
        let mut s = fx.ed.model.roof(base).unwrap();
        s.kind = RoofKind::Footprint {
            ring: square(x, 20.0, 6.0),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        };
        ids.push(fx.ed.model.add_roof(&s).unwrap());
    }
    fx.ed.reload().unwrap();
    ids
}

fn three_beams(fx: &mut Fx) -> Vec<i64> {
    let base = fx.ids["beam"];
    let mut ids = vec![base];
    for dy in [5.0, 10.0] {
        let mut s = fx.ed.model.beam(base).unwrap();
        s.a.1 += dy;
        s.b.1 += dy;
        ids.push(fx.ed.model.add_beam(&s).unwrap());
    }
    fx.ed.reload().unwrap();
    ids
}

fn row(kind: &str, key: &str) -> Row {
    multi_rows(kind)
        .into_iter()
        .find(|r| r.key == key)
        .unwrap_or_else(|| panic!("no multi row {kind} {key}"))
}

fn slopes(m: &Model, id: i64) -> Vec<Option<f64>> {
    match m.roof(id).unwrap().kind {
        RoofKind::Footprint { slopes, .. } => slopes,
        _ => panic!("footprint roof expected"),
    }
}

fn common(vals: &[(Row, Option<f64>)], key: &str) -> Option<f64> {
    vals.iter()
        .find(|v| v.0.key == key)
        .unwrap_or_else(|| panic!("row {key} missing"))
        .1
}

#[test]
fn three_roofs_slope_set_one_undo() {
    let mut fx = fixture();
    let ids = three_roofs(&mut fx);
    let r = row("roof", "geom_props.roof_slope");
    let ops = op_count(&fx.ed);
    assert_eq!(commit_many(&mut fx.ed, &r, &ids, 40f64.to_radians()), 3);
    assert_eq!(op_count(&fx.ed), ops + 1, "one journal record for all three");
    for &id in &ids {
        let s = slopes(&fx.ed.model, id);
        assert_eq!(s.len(), 4);
        assert!((s[0].unwrap() - 40.0).abs() < 1e-9 && (s[2].unwrap() - 40.0).abs() < 1e-9);
        assert_eq!((s[1], s[3]), (None, None), "gable edges stay gables");
    }
    let vals = multi_values(&fx.ed.model, "roof", &ids);
    let shared = common(&vals, "geom_props.roof_slope").expect("shared after the edit");
    assert!((shared.to_degrees() - 40.0).abs() < 1e-9);
    fx.ed.undo();
    for &id in &ids {
        let s = slopes(&fx.ed.model, id);
        assert_eq!(s, vec![Some(30.0), None, Some(30.0), None], "one undo restores roof {id}");
    }
}

#[test]
fn mixed_values_show_varies() {
    let mut fx = fixture();
    let ids = three_roofs(&mut fx);
    let mut s = fx.ed.model.roof(ids[1]).unwrap();
    s.overhang = 0.5;
    if let RoofKind::Footprint { slopes, .. } = &mut s.kind {
        slopes[0] = Some(45.0);
    }
    fx.ed.model.update_roof(ids[1], &s).unwrap();
    let vals = multi_values(&fx.ed.model, "roof", &ids);
    assert_eq!(common(&vals, "geom.overhang"), None, "0.25 / 0.5 / 0.25 varies");
    assert_eq!(common(&vals, "geom_props.roof_slope"), None, "edges 45 and 30 on one roof vary");
    assert_eq!(common(&vals, "geom.offset"), Some(0.0), "the same offset is shown");
    let st = fx.ed.model.roof(ids[0]).unwrap().slab_type as f64;
    assert_eq!(common(&vals, "geom.type"), Some(st), "the same type is shown");

    // mixed categories: each table gets its own group, sub-elements and order kept
    let roof = crate::elem::by_table("roof").unwrap().sel(ids[0]).unwrap();
    let sels = [
        Sel::Body(BodyKind::Beam, fx.ids["beam"]),
        roof,
        Sel::Body(BodyKind::Column, fx.ids["col"]),
        crate::elem::by_table("roof").unwrap().sel(ids[2]).unwrap(),
    ];
    let groups = tables_of(&sels);
    let tables: Vec<&str> = groups.iter().map(|g| g.0).collect();
    assert_eq!(tables, ["beam", "roof", "col"]);
    assert_eq!(groups[1].1, vec![ids[0], ids[2]]);
    let mixed = multi_values(&fx.ed.model, "roof", &groups[1].1);
    assert_eq!(common(&mixed, "geom.overhang"), Some(0.25), "roofs 1 and 3 share it");
    assert!(multi_values(&fx.ed.model, "beam", &[]).is_empty());
}

#[test]
fn out_of_range_refuses_all_nothing_written() {
    let mut fx = fixture();
    let ids = three_roofs(&mut fx);
    let ops = op_count(&fx.ed);
    let over = row("roof", "geom.overhang");
    assert_eq!(commit_many(&mut fx.ed, &over, &ids, over.hi + 1.0), 0);
    let slope = row("roof", "geom_props.roof_slope");
    assert_eq!(commit_many(&mut fx.ed, &slope, &ids, 89f64.to_radians()), 0);
    assert!(fx.ed.status.as_deref().is_some_and(|s| s.contains("88")), "reason names the bound");
    for &id in &ids {
        assert!((fx.ed.model.roof(id).unwrap().overhang - 0.25).abs() < 1e-12);
        assert_eq!(slopes(&fx.ed.model, id), vec![Some(30.0), None, Some(30.0), None]);
    }
    assert_eq!(op_count(&fx.ed), ops, "nothing written");

    // a pinned element refuses the whole edit too
    fx.ed.model.pin(&[("roof", ids[1])]).unwrap();
    fx.ed.reload().unwrap();
    let ops = op_count(&fx.ed);
    assert_eq!(commit_many(&mut fx.ed, &over, &ids, 0.5), 0);
    let why = fx.ed.status.clone().unwrap_or_default();
    assert!(why.contains(&format!("#{}", ids[1])), "reason names the pinned roof: {why}");
    for &id in &ids {
        assert!((fx.ed.model.roof(id).unwrap().overhang - 0.25).abs() < 1e-12, "roof {id}");
    }
    assert_eq!(op_count(&fx.ed), ops, "nothing written");
}

#[test]
fn beams_type_change_all_selected() {
    let mut fx = fixture();
    let ids = three_beams(&mut fx);
    let r = row("beam", "geom.type");
    let old = fx.ed.model.beam(ids[0]).unwrap().member_type;
    let alt = type_options(&fx.ed.model, "member_type")
        .into_iter()
        .map(|o| o.0)
        .find(|&t| t != old)
        .expect("a second member type");
    let ops = op_count(&fx.ed);
    assert_eq!(commit_many(&mut fx.ed, &r, &ids, alt as f64), 3);
    assert_eq!(op_count(&fx.ed), ops + 1, "one journal record");
    for &id in &ids {
        assert_eq!(fx.ed.model.beam(id).unwrap().member_type, alt, "beam {id}");
    }
    let vals = multi_values(&fx.ed.model, "beam", &ids);
    assert_eq!(common(&vals, "geom.type"), Some(alt as f64));
    // an unknown type is refused before anything is written
    assert_eq!(commit_many(&mut fx.ed, &r, &ids, 1e6), 0);
    fx.ed.undo();
    for &id in &ids {
        assert_eq!(fx.ed.model.beam(id).unwrap().member_type, old, "one undo restores {id}");
    }
}
