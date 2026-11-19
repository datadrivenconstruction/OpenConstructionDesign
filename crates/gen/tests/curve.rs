//! Curved facades: a plain bar's long facades bow out as one arc wall each.

use ddc_gen::project::{brief, build_project, check_project, merge_patch, parse_spec, GProject, ProjectSpec};
use ddc_gen::WallRole;
use serde_json::{json, Value as J};

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    (s, p)
}

fn gfa(p: &GProject) -> f64 {
    p.metrics.iter().find(|m| m.0 == "gross_floor_area").unwrap().1
}

#[test]
fn the_south_facade_bows_out_as_one_arc_wall_and_the_rooms_grow() {
    let (_, flat) = built(&json!({"typology": "office", "floors": 3}));
    let (_, p) = built(&json!({"typology": "office", "floors": 3, "design": {"curve": 3.0}}));
    let (w, _) = p.footprint;
    for s in &p.storeys {
        let l = &s.layout;
        let south: Vec<_> = l
            .walls
            .iter()
            .filter(|x| x.role == WallRole::Exterior && x.bulge != 0.0)
            .collect();
        assert_eq!(south.len(), 1, "one arc wall: {south:?}");
        let arc = south[0];
        assert!(arc.a.0.abs() < 1e-9 && (arc.b.0 - w).abs() < 1e-9 && arc.a.1.abs() < 1e-9);
        // the middle of the arc lies the sagitta out; no straight wall is left on the old line
        let mid = arc.at(arc.length() / 2.0);
        assert!((mid.1 + 3.0).abs() < 1e-6, "{mid:?}");
        assert!(!l.walls.iter().any(|x| x.bulge == 0.0 && x.a.1.abs() < 1e-6 && x.b.1.abs() < 1e-6));
        let low = l.slab.iter().map(|q| q.1).fold(f64::MAX, f64::min);
        assert!(low > -3.0 - 1e-9 && low < -2.5, "{low}");
        // the corners stay where they were
        assert!(l.slab.contains(&(0.0, 0.0)) && l.slab.iter().any(|q| (q.0 - w).abs() < 1e-9 && q.1.abs() < 1e-9));
        // every opening still sits within its wall
        for o in &l.openings {
            let x = &l.walls[o.wall];
            let len = x.length();
            assert!(o.along >= -1e-9 && o.along + o.width <= len + 1e-9, "{o:?} on {x:?}");
        }
    }
    assert!(gfa(&p) > gfa(&flat) + 3.0 * 2.0 / 3.0 * w * 3.0 * 0.8, "{} vs {}", gfa(&p), gfa(&flat));
    assert!(p.roof.ring.iter().any(|q| q.1 < -2.5));
}

#[test]
fn both_facades_bow_with_any_facade_system() {
    for (t, fac) in [("apartment_section", "punched"), ("office", "fins"), ("school", "curtain"), ("office", "panels")] {
        built(&json!({"typology": t, "floors": 4,
            "design": {"curve": 2.5, "curve_north": 1.5, "facade": fac, "balconies": "balcony"}}));
    }
}

#[test]
fn a_curve_needs_a_plain_bar_and_room_to_bow() {
    let e = parse_spec(&json!({"typology": "office", "design": {"massing": "l", "curve": 2.0}}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("design.curve"), "{e}");
    let s = parse_spec(&json!({"typology": "house", "floors": 2, "design": {"curve": 6.0}})).unwrap();
    let e = build_project(&s).unwrap_err().to_string();
    assert!(e.contains("bows out at most"), "{e}");
}

#[test]
fn the_brief_check_repairs_curved_into_a_bowed_facade() {
    let mut v = json!({"typology": "office", "floors": 3, "features": ["curved"]});
    let (s, p) = built(&v);
    let fix = brief::check(&s, &p).repair.expect("a repair");
    assert_eq!(fix, json!({"design": {"curve": 3.0}}));
    merge_patch(&mut v, &fix);
    let (s, p) = built(&v);
    let c = brief::check(&s, &p);
    assert!(c.repair.is_none(), "{}", c.report);
    assert!(c.report["features"]["met"].as_array().unwrap().iter().any(|f| f == "curved"), "{}", c.report);
}

#[test]
fn the_strip_under_a_curved_facade_follows_the_arc() {
    use ddc_gen::project::plan_foundation;
    use ddc_gen::GWall;
    let (s, p) = built(&json!({"typology": "office", "floors": 3, "design": {"curve": 3.0}, "foundation": "strip"}));
    let f = plan_foundation(&s, &p, 1).unwrap();
    let arc = p.storeys[0].layout.walls.iter().find(|x| x.bulge != 0.0).unwrap();
    let st: Vec<_> = f.strips.iter().filter(|st| st.bulge != 0.0).collect();
    assert_eq!(st.len(), 1, "one curved strip: {:?}", f.strips);
    let band = GWall { a: st[0].a, b: st[0].b, role: WallRole::Exterior, bulge: st[0].bulge, fire: None };
    assert!((band.length() - arc.length()).abs() < 1e-9);
    for k in 0..=20 {
        let t = arc.length() * k as f64 / 20.0;
        let (u, v) = (band.at(t), arc.at(t));
        assert!((u.0 - v.0).hypot(u.1 - v.1) < 1e-9, "{t}: {u:?} vs {v:?}");
    }
    // the middle sits the sagitta out, where the chord would not reach
    assert!((band.at(band.length() / 2.0).1 + 3.0).abs() < 1e-6);
}
