//! A generated house: every wall under the roof, exterior and interior, ends at the roof
//! underside - touching it, never above it - for a gable and a hip roof.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::ids;
use serde_json::json;

fn at(top: &[(f64, f64)], u: f64) -> f64 {
    for w in top.windows(2) {
        if u >= w[0].0 - 1e-9 && u <= w[1].0 + 1e-9 && w[1].0 - w[0].0 > 1e-9 {
            return w[0].1 + (w[1].1 - w[0].1) * (u - w[0].0) / (w[1].0 - w[0].0);
        }
    }
    top.last().unwrap().1
}

fn check(kind: &str) {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let spec = json!({"typology": "house", "floors": 2, "roof": {"kind": kind}, "lang": "en"});
    let (mut s, _p) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    apply_and_check(&mut s, &mut ed);
    let m = &ed.model;
    let roofs: Vec<_> = ids(m, "roof")
        .into_iter()
        .map(|r| (m.roof_geometry(r).unwrap(), m.roof_quantities(r).unwrap().thickness, r))
        .collect();
    assert!(!roofs.is_empty(), "{kind}: no roof");
    let walls: Vec<(i64, f64, f64, f64, f64, f64, f64, i64)> = {
        let mut st = m
            .sql()
            .prepare(
                "SELECT w.id, w.ax, w.ay, w.bx, w.by, l.elevation + w.base_offset, w.height, w.bulge FROM wall w \
                 JOIN level l ON l.id = w.level ORDER BY w.id",
            )
            .unwrap();
        st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get::<_, f64>(7)? as i64))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let (mut under, mut gable_fill) = (0, false);
    for (id, ax, ay, bx, by, base, h, bulge) in walls {
        if bulge != 0 {
            continue;
        }
        let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
        let under_at = |u: f64| {
            let s = u / len;
            let p = ddc_geom::v2(ax + (bx - ax) * s, ay + (by - ay) * s);
            roofs.iter().filter_map(|(g, t, _)| g.underside_at(p, *t)).fold(None, |a: Option<f64>, z| Some(a.map_or(z, |a| a.min(z))))
        };
        // a wall whose flat top reaches the roof zone and lies under it
        let covered: Vec<f64> = (0..=(len / 0.25) as usize).map(|i| i as f64 * 0.25).filter(|u| under_at(*u).is_some()).collect();
        let reaches = roofs.iter().any(|(g, _, _)| base + h >= g.eave_z - 0.6);
        if !reaches || covered.len() * 2 < (len / 0.25) as usize {
            continue;
        }
        under += 1;
        let top = m.wall_top_profile(id).unwrap().unwrap_or_else(|| panic!("{kind}: wall {id} is not trimmed by the roof"));
        for u in covered {
            let (wz, z) = (base + at(&top, u), under_at(u).unwrap());
            assert!(wz <= z + 1e-3, "{kind}: wall {id} pokes through the roof at u={u}: {wz} > {z}");
            assert!(z - wz <= 0.03, "{kind}: wall {id} stops short of the roof at u={u}: {wz} < {z}");
            gable_fill |= wz > base + h + 0.3;
        }
    }
    assert!(under >= 6, "{kind}: only {under} walls under the roof");
    if kind == "gable" {
        assert!(gable_fill, "a gable end wall fills up to the ridge");
    }
}

#[test]
fn gable_house_walls_meet_the_roof() {
    check("gable");
}

#[test]
fn hip_house_walls_meet_the_roof() {
    check("hip");
}
