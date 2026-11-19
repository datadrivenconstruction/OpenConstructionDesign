//! Detailed element geometry: no placeholder blocks, closed outward shells, triangle budgets per
//! level of detail, plan swing symbols, shared prototypes, rules in step with the rule set.

use ddc_geom::detail::opening::{
    DoorOp, DoorType, LeafStyle, OpeningType, Placement, SashLayout, WindowType,
};
use ddc_geom::detail::plan::{door_symbol, PlanPrim};
use ddc_geom::detail::rules::{DetailRules, RULES};
use ddc_geom::detail::{
    cache, door, rail, roof, section, stair, window, Detail, Lod, Mat, DOOR_BUDGET, RAIL_BUDGET,
    WINDOW_BUDGET,
};
use ddc_geom::member::Shape;
use ddc_geom::mesh::Mesh;
use ddc_geom::stair::{Railing, Shape as StairShape, StairDef};
use ddc_geom::{v2, v3};
use std::sync::Arc;

const LODS: [Lod; 3] = [Lod::Coarse, Lod::Medium, Lod::Fine];

fn place(w: f64, h: f64) -> Placement {
    Placement {
        width: w,
        height: h,
        wall_t: 0.3,
        swing_flip: false,
        hinge_flip: false,
    }
}

fn sound(d: &Detail, what: &str) {
    assert!(!d.is_plain_box(), "{what}: a plain block");
    assert!(d.shells() >= 3, "{what}: {} pieces", d.shells());
    assert!(d.is_closed(), "{what}: a shell is open or inside out");
}

fn doors() -> Vec<(String, DoorType, f64)> {
    let r = DetailRules::default();
    let mut v = vec![];
    for w in [0.8, 0.9, 1.2, 1.6, 2.4] {
        v.push((format!("door {w}"), DoorType::for_width(w, &r), w));
    }
    let mut flush = DoorType::for_width(0.9, &r);
    flush.leaf = LeafStyle::Flush;
    v.push(("flush".into(), flush, 0.9));
    let mut four = DoorType::for_width(0.9, &r);
    four.leaf = LeafStyle::Panels { cols: 2, rows: 2 };
    v.push(("four panels".into(), four, 0.9));
    let mut slide = DoorType::for_width(1.8, &r);
    slide.op = DoorOp::Sliding;
    v.push(("sliding".into(), slide, 1.8));
    v
}

#[test]
fn doors_are_detailed_closed_and_within_budget_at_every_level() {
    for (name, t, w) in doors() {
        let mut last = 0;
        for lod in LODS {
            let d = door::build(&t, &place(w, 2.1), lod);
            sound(&d, &format!("{name} {lod:?}"));
            let budget = DOOR_BUDGET.at(lod) * t.leaves.max(1) as usize;
            assert!(d.triangles() <= budget, "{name} {lod:?}: {} > {budget}", d.triangles());
            assert!(d.triangles() > last, "{name}: {lod:?} adds no detail");
            last = d.triangles();
            let (lo, hi) = d.bounds().unwrap();
            assert!(lo.z > -1e-9 && hi.z < 2.1 + 0.08, "{name} {lod:?}: z {lo:?} {hi:?}");
            assert!(lo.x > -0.08 && hi.x < w + 0.08, "{name} {lod:?}: x {lo:?} {hi:?}");
        }
        let fine = door::build(&t, &place(w, 2.1), Lod::Fine);
        assert!(fine.parts.iter().any(|p| p.0 == Mat::Metal), "{name}: no hardware");
        assert!(fine.parts.iter().any(|p| p.0 == Mat::Stone), "{name}: no threshold");
    }
}

#[test]
fn glazed_entrance_doors_have_glass_and_panelled_doors_recessed_fields() {
    let r = DetailRules::default();
    let entrance = door::build(&DoorType::for_width(1.6, &r), &place(1.6, 2.1), Lod::Medium);
    assert!(entrance.parts.iter().any(|p| p.0 == Mat::Glass));
    let room = door::build(&DoorType::for_width(0.9, &r), &place(0.9, 2.1), Lod::Medium);
    assert!(!room.parts.iter().any(|p| p.0 == Mat::Glass));
    // a recessed field is thinner than the leaf: some leaf vertices lie inside its faces
    let leaf = &room.parts.iter().find(|p| p.0 == Mat::Leaf).unwrap().1;
    let ys: Vec<f64> = leaf.positions.iter().map(|p| (p.y * 1e4).round() / 1e4).collect();
    let mut u = ys.clone();
    u.sort_by(f64::total_cmp);
    u.dedup();
    assert!(u.len() >= 4, "leaf faces and field faces: {u:?}");
}

#[test]
fn flips_mirror_the_leaf_and_keep_shells_outward() {
    let r = DetailRules::default();
    let t = DoorType::for_width(0.9, &r);
    let mut p = place(0.9, 2.1);
    let a = door::build(&t, &p, Lod::Fine);
    p.swing_flip = true;
    p.hinge_flip = true;
    let b = door::build(&t, &p, Lod::Fine);
    assert!(b.is_closed());
    assert_eq!(a.triangles(), b.triangles());
    let leaf_y = |d: &Detail| {
        let m = &d.parts.iter().find(|q| q.0 == Mat::Leaf).unwrap().1;
        m.positions.iter().map(|q| q.y).sum::<f64>() / m.positions.len() as f64
    };
    assert!(leaf_y(&a) > 0.0 && leaf_y(&b) < 0.0, "the leaf moves to the other face");
}

#[test]
fn windows_have_frames_fields_sills_and_stay_in_budget() {
    let r = DetailRules::default();
    let t = WindowType::standard(&r);
    for (w, h) in [(0.6, 0.6), (1.2, 1.4), (1.8, 1.5), (2.4, 2.1), (4.8, 2.4)] {
        let fields = window::fields(&t, w, h, &r);
        assert!(!fields.is_empty());
        let mut last = 0;
        for lod in LODS {
            let d = window::build(&t, &place(w, h), lod, &r);
            sound(&d, &format!("window {w}x{h} {lod:?}"));
            let budget = WINDOW_BUDGET.at(lod) * fields.len();
            assert!(d.triangles() <= budget, "window {w}x{h} {lod:?}: {} > {budget}", d.triangles());
            assert!(d.triangles() > last);
            last = d.triangles();
            let glass = d.parts.iter().filter(|p| p.0 == Mat::Glass).map(|p| p.1.face_of.last().map_or(0, |f| *f + 1)).sum::<u32>();
            assert_eq!(glass as usize, fields.len(), "one pane per field");
            assert!(d.parts.iter().any(|p| p.0 == Mat::Stone), "outside sill");
        }
    }
    // tall windows get a transom: two rows of fields
    let tall = window::fields(&t, 1.2, 2.0, &r);
    let rows: std::collections::BTreeSet<i64> = tall.iter().map(|f| (f.z.0 * 1e3) as i64).collect();
    assert_eq!(rows.len(), 2);
    let short = window::fields(&t, 1.2, 1.4, &r);
    assert!(short.iter().all(|f| f.z == short[0].z));
    // opening sashes only where the layout says
    let mut fixed = t;
    fixed.sashes = SashLayout::AllFixed;
    assert!(window::fields(&fixed, 2.4, 1.5, &r).iter().all(|f| !f.opens));
    let wide = window::fields(&t, 2.7, 1.5, &r);
    assert_eq!(wide.iter().filter(|f| f.opens).count(), 2);
}

#[test]
fn door_plan_shows_each_leaf_open_with_its_swing_arc() {
    let r = DetailRules::default();
    for (w, leaves) in [(0.9, 1usize), (1.6, 2)] {
        let t = DoorType::for_width(w, &r);
        for (sf, hf) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut p = place(w, 2.1);
            p.swing_flip = sf;
            p.hinge_flip = hf;
            let s = door_symbol(&t, &p);
            let arcs: Vec<_> = s.iter().filter_map(|g| match g { PlanPrim::Arc { c, r, a0, a1 } => Some((*c, *r, *a0, *a1)), _ => None }).collect();
            let lines = s.iter().filter(|g| matches!(g, PlanPrim::Line(..))).count();
            assert_eq!(arcs.len(), leaves, "{w} {sf} {hf}");
            assert_eq!(lines, leaves);
            let open = (w - 2.0 * t.frame.face) / leaves as f64;
            for (c, rad, a0, a1) in arcs {
                assert!((rad - open).abs() < 1e-9);
                assert!((a1 - a0 - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
                // the arc sweeps to the swing side
                let mid = (a0 + a1) / 2.0;
                let y = c.y + rad * mid.sin();
                assert_eq!(y > 0.0, !sf, "{w} {sf} {hf}");
            }
        }
    }
}

#[test]
fn prototypes_are_shared_between_identical_openings() {
    let r = DetailRules::default();
    let t = OpeningType::Window(WindowType::standard(&r));
    let p = place(1.5, 1.5);
    let a = cache::opening(&t, &p, Lod::Medium, &r);
    let (built, _) = cache::stats();
    for _ in 0..50 {
        let b = cache::opening(&t, &p, Lod::Medium, &r);
        assert!(Arc::ptr_eq(&a, &b));
    }
    assert_eq!(cache::stats().0, built, "no rebuilds for the same type");
    let c = cache::opening(&t, &place(1.6, 1.5), Lod::Medium, &r);
    assert!(!Arc::ptr_eq(&a, &c));
}

fn railing(path: Vec<ddc_geom::V3>) -> Railing {
    Railing {
        path,
        height: 0.9,
        post_spacing: 1.2,
        baluster_spacing: 0.12,
        post: 0.05,
        baluster: 0.02,
        rail: (0.045, 0.045),
    }
}

#[test]
fn railings_are_round_bars_not_blocks_and_stay_in_budget_per_metre() {
    let flat = railing(vec![v3(0.0, 0.0, 0.0), v3(4.0, 0.0, 0.0), v3(4.0, 3.0, 0.0)]);
    let stair = railing(vec![v3(0.0, 0.0, 0.17), v3(3.0, 0.0, 1.87), v3(4.0, 0.0, 1.87)]);
    for rl in [flat, stair] {
        let lay = rl.layout();
        let mut last = 0.0;
        for lod in LODS {
            let d = rail::build(&rl, lod);
            sound(&d, &format!("railing {lod:?}"));
            let pm = rail::per_metre(&d, &rl);
            assert!(pm <= RAIL_BUDGET.at(lod) as f64, "{lod:?}: {pm:.0} per metre");
            assert!(pm > last);
            last = pm;
        }
        // medium: handrail, bottom rail, every post and every baluster as its own piece
        let d = rail::build(&rl, Lod::Medium);
        assert_eq!(d.shells(), 2 + lay.posts.len() + lay.balusters.len());
    }
    let r = DetailRules::default();
    let (h, ps, bs, _, b, d) = rail::sizes(&r, false);
    assert_eq!((h, ps), (r.guard_height, r.post_spacing));
    assert!((bs - b - r.baluster_gap).abs() < 1e-12 && d == r.handrail_d);
}

#[test]
fn member_sections_keep_their_shape_and_volume() {
    let len = 3.0;
    let cases = [
        Shape::I { h: 0.3, b: 0.15, tw: 0.007, tf: 0.011 },
        Shape::Tube { h: 0.2, b: 0.1, t: 0.008 },
        Shape::Pipe { d: 0.168, t: 0.008 },
        Shape::Circle { d: 0.4 },
        Shape::Rect { b: 0.3, h: 0.5 },
    ];
    for s in &cases {
        for lod in LODS {
            let mut m = Mesh::default();
            section::bar(&mut m, s, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, len), v3(1.0, 0.0, 0.0), lod);
            assert!(m.is_watertight() && m.volume() > 0.0, "{s:?} {lod:?}");
            let round = matches!(s, Shape::Circle { .. } | Shape::Pipe { .. });
            let tol = if round { 0.12 } else { 1e-6 };
            let exact = s.area() * len;
            let hollow_coarse = lod == Lod::Coarse && matches!(s, Shape::Tube { .. } | Shape::Pipe { .. });
            if !hollow_coarse {
                assert!((m.volume() / exact - 1.0).abs() < tol, "{s:?} {lod:?}: {} vs {exact}", m.volume());
            }
        }
        let mut m = Mesh::default();
        section::bar(&mut m, s, v3(0.0, 0.0, 0.0), v3(len, 0.0, 0.0), v3(0.0, 0.0, 1.0), Lod::Medium);
        if !matches!(s, Shape::Rect { .. }) {
            assert!(m.positions.len() > 8, "{s:?} drawn as a block");
        }
    }
}

#[test]
fn every_tread_gets_a_nosing_past_its_riser() {
    let def = StairDef {
        origin: v2(0.0, 0.0),
        angle: 0.0,
        base_z: 0.0,
        height: 3.0,
        width: 1.2,
        max_riser: 0.18,
        tread: None,
        landing: 1.2,
        thickness: 0.15,
        stringer: None,
        shape: StairShape::U { left: false, first: None, gap: 0.1 },
    };
    let s = def.layout().unwrap();
    let r = DetailRules::default();
    let treads: usize = s.flights.iter().map(|f| f.treads()).sum();
    assert!(stair::nosings(&s, r.nosing, Lod::Coarse).parts.is_empty());
    for lod in [Lod::Medium, Lod::Fine] {
        let d = stair::nosings(&s, r.nosing, lod);
        assert!(d.is_closed());
        assert_eq!(d.shells(), treads);
    }
}

#[test]
fn roof_edges_have_copings_gutters_and_spaced_downpipes() {
    let r = DetailRules::default();
    for lod in LODS {
        let mut d = Detail::default();
        roof::coping(&mut d, v2(0.0, 0.0), v2(10.0, 0.0), 9.6, 0.3, r.coping_overhang, v2(0.0, 1.0), lod);
        roof::gutter(&mut d, v3(0.0, -0.4, 6.0), v3(10.0, -0.4, 6.0), v2(0.0, -1.0), lod);
        roof::downpipe(&mut d, v3(5.0, -0.46, 6.0), v2(5.0, -0.15), v2(0.0, -1.0), 0.0, lod);
        roof::rain_head(&mut d, v2(2.0, -0.15), v2(0.0, -1.0), 9.3, lod);
        sound(&d, &format!("roof edge {lod:?}"));
        let (lo, hi) = d.bounds().unwrap();
        assert!(lo.z < 0.3 && hi.z > 9.6, "the downpipe reaches the ground: {lo:?} {hi:?}");
    }
    // 46 m of eaves, a downpipe per 20 m: three, inside the edges
    let edges = [
        (v2(0.0, 0.0), v2(15.0, 0.0), v2(0.0, -1.0)),
        (v2(15.0, 0.0), v2(15.0, 8.0), v2(1.0, 0.0)),
        (v2(15.0, 8.0), v2(0.0, 8.0), v2(0.0, 1.0)),
        (v2(0.0, 8.0), v2(0.0, 0.0), v2(-1.0, 0.0)),
    ];
    let at = roof::spread(&edges, r.downpipe_spacing);
    assert_eq!(at.len(), 3);
    assert!(at.iter().all(|(p, _)| p.x >= -1e-9 && p.x <= 15.0 + 1e-9 && p.y >= -1e-9 && p.y <= 8.0 + 1e-9));
}

#[test]
fn detail_rules_are_rules_of_the_rule_set_with_the_same_defaults() {
    let text = include_str!("../../gen/data/rules.json");
    let doc: serde_json::Value = serde_json::from_str(text).unwrap();
    let rules = doc["rules"].as_array().unwrap();
    for (id, default) in RULES {
        let r = rules
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from rules.json"));
        assert_eq!(r["group"], "detail", "{id}");
        assert_eq!(r["default"].as_f64(), Some(default), "{id}");
        let (lo, hi) = (r["min"].as_f64().unwrap(), r["max"].as_f64().unwrap());
        assert!(lo <= default && default <= hi, "{id}");
        assert!(r["why"].as_str().is_some_and(|s| s.len() > 20), "{id}: why");
    }
    // a rule set's values reach the builders
    let r = DetailRules::from_lookup(|id| (id == "detail.window.pane").then_some(0.6));
    let t = WindowType::standard(&r);
    assert_eq!(window::fields(&t, 2.52, 1.4, &r).len(), 4);
    assert_eq!(window::fields(&t, 2.52, 1.4, &DetailRules::default()).len(), 3);
}

#[test]
fn a_hundred_storey_windows_cost_one_build() {
    // the bench: a ten-storey facade, ten windows a storey, all one type
    let r = DetailRules::default();
    let t = OpeningType::Window(WindowType::standard(&r));
    let p = place(1.4, 1.5);
    let t0 = std::time::Instant::now();
    let protos: Vec<Arc<Detail>> = (0..100).map(|_| cache::opening(&t, &p, Lod::Fine, &r)).collect();
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    assert!(protos.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
    assert!(ms < 50.0, "{ms:.1} ms for 100 placements");
}

#[test]
fn no_library_family_is_a_lone_block_at_any_level_of_detail() {
    use ddc_geom::component::Family;
    let lib = include_str!("../../model/data/library.ddcf");
    let mut n = 0;
    for src in lib.split("\n---") {
        if !src.lines().any(|l| l.trim_start().starts_with("family ")) {
            continue;
        }
        let f = Family::parse(src.trim_start_matches('\n')).unwrap();
        // the one family that is a block by definition: a placeholder the user sizes
        if f.name == "Generic Box" {
            continue;
        }
        let mut types: Vec<Vec<(String, ddc_geom::component::Value)>> = f.types.iter().map(|t| t.values.clone()).collect();
        if types.is_empty() {
            types.push(vec![]);
        }
        for tv in &types {
            let r = f.resolve(tv, &[]).unwrap_or_else(|e| panic!("{}: {e}", f.name));
            for lod in LODS {
                let b = f.build(&r, lod).unwrap_or_else(|e| panic!("{} {lod:?}: {e}", f.name));
                let v = b.mesh.positions.len();
                assert!(v > 8, "{} {lod:?}: a block of {v} vertices", f.name);
            }
        }
        n += 1;
    }
    assert!(n >= 60, "{n} families");
}

#[test]
fn opening_type_round_trips_through_its_key() {
    let r = DetailRules::default();
    let a = OpeningType::Door(DoorType::for_width(0.9, &r));
    let b = OpeningType::Door(DoorType::for_width(0.9, &r));
    let p = place(0.9, 2.1);
    use ddc_geom::detail::opening::proto_key;
    assert_eq!(proto_key(&a, &p, Lod::Fine), proto_key(&b, &p, Lod::Fine));
    assert_ne!(proto_key(&a, &p, Lod::Fine), proto_key(&a, &p, Lod::Medium));
    let mut q = p;
    q.hinge_flip = true;
    assert_ne!(proto_key(&a, &p, Lod::Fine), proto_key(&a, &q, Lod::Fine));
}
