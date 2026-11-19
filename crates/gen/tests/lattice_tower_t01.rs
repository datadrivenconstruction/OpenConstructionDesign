//! T01, the owner's lattice tower "at excellent detail": one steel_frame call (lattice_tower_300 at
//! detail 3) lands in the model with its joints and meets the deep checks of the live run: member,
//! brace, joint and part counts, height, taper and curved legs, four legs, section tiers and shapes,
//! platforms, long low girders, piers, stairs and lifts, no duplicate and no free member ends.
//! The second test holds the build to its time budget.

use ddc_gen::{registry, GenOut};
use ddc_model::Model;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

type P3 = [f64; 3];

/// The params the recipe gives for the owner's brief (detail 3; joints follow by default).
fn params() -> serde_json::Value {
    json!({"kind": "lattice_tower_300", "pattern": "x", "tower_height": 300, "base_width": 125,
           "platform_1": 57, "platform_2": 115, "platform_3": 276, "detail": 3})
}

struct Built {
    m: Model,
    /// Seconds: generator, frame into the model, joints.
    secs: [f64; 3],
}

fn build() -> Built {
    let g = registry().iter().find(|g| g.id() == "steel_frame").unwrap();
    let t0 = Instant::now();
    let GenOut::Steel { frame, joints, .. } = g.build(&params(), 1).unwrap() else { panic!("not steel") };
    assert!(joints, "detail 3 joins its nodes in the same call");
    let t1 = Instant::now();
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let ids = m.add_steel_frame(lv, &frame).unwrap();
    let t2 = Instant::now();
    let conns = m.connect_steel_frame(&ids).unwrap();
    assert!(!conns.is_empty());
    let secs = [t1 - t0, t2 - t1, t2.elapsed()].map(|d| d.as_secs_f64());
    Built { m, secs }
}

fn count(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Every member as (table, foot, head) in world z, the way the deep check reads the file.
fn members(m: &Model) -> Vec<(&'static str, P3, P3, Option<i64>)> {
    let db = m.sql();
    let z: HashMap<i64, f64> = db
        .prepare("SELECT id, elevation FROM level")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let lz = |l: Option<i64>| l.and_then(|l| z.get(&l).copied()).unwrap_or(0.0);
    let mut out = vec![];
    let mut st = db.prepare("SELECT level, top_level, mtype, x, y, base_offset, top_offset FROM col").unwrap();
    let rows = st
        .query_map([], |r| {
            let (lv, tl): (Option<i64>, Option<i64>) = (r.get(0)?, r.get(1)?);
            let (x, y): (f64, f64) = (r.get(3)?, r.get(4)?);
            let (bo, to): (Option<f64>, Option<f64>) = (r.get(5)?, r.get(6)?);
            Ok(("col", [x, y, lz(lv) + bo.unwrap_or(0.0)], [x, y, lz(tl) + to.unwrap_or(0.0)], r.get(2)?))
        })
        .unwrap();
    out.extend(rows.map(Result::unwrap));
    for t in ["beam", "brace"] {
        let mut st = db.prepare(&format!("SELECT level, ax, ay, az, bx, by, bz, mtype FROM {t}")).unwrap();
        let rows = st
            .query_map([], |r| {
                let e = lz(r.get(0)?);
                Ok((t, [r.get(1)?, r.get(2)?, e + r.get::<_, f64>(3)?], [r.get(4)?, r.get(5)?, e + r.get::<_, f64>(6)?], r.get(7)?))
            })
            .unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out
}

/// Half-width per sixth of the height: (bottom / top ratio, largest departure from the straight
/// taper relative to the bottom half-width).
fn profile(pts: &[P3]) -> (Vec<f64>, f64, f64) {
    const B: usize = 6;
    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p[2]), a.1.max(p[2])));
    let mid = |i: usize| {
        let (a, b) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p[i]), a.1.max(p[i])));
        (a + b) / 2.0
    };
    let (cx, cy) = (mid(0), mid(1));
    let r: Vec<f64> = (0..B)
        .map(|b| {
            let (zl, zh) = (lo + (hi - lo) * b as f64 / B as f64, lo + (hi - lo) * (b + 1) as f64 / B as f64);
            pts.iter().filter(|p| p[2] >= zl && p[2] <= zh + 1e-9).map(|p| (p[0] - cx).abs().max((p[1] - cy).abs())).fold(0.0, f64::max)
        })
        .collect();
    let zs: Vec<f64> = (0..B).map(|b| lo + (hi - lo) * (b as f64 + 0.5) / B as f64).collect();
    let dev = (0..B)
        .map(|i| (r[i] - (r[0] + (r[B - 1] - r[0]) * (zs[i] - zs[0]) / (zs[B - 1] - zs[0]))).abs())
        .fold(0.0, f64::max)
        / r[0];
    (r.clone(), r[0] / r[B - 1], dev)
}

/// Member ends above the ground that touch no other member within `tol` (members sampled
/// every 0.5 m), and all ends.
fn free_ends(ms: &[(&str, P3, P3, Option<i64>)], tol: f64) -> (usize, usize) {
    let cell = |p: &P3| p.map(|v| v.floor() as i64);
    let mut grid: HashMap<[i64; 3], Vec<(P3, usize)>> = HashMap::new();
    for (i, m) in ms.iter().enumerate() {
        let len = ((m.2[0] - m.1[0]).powi(2) + (m.2[1] - m.1[1]).powi(2) + (m.2[2] - m.1[2]).powi(2)).sqrt();
        let n = (len / 0.5).ceil().max(1.0) as usize;
        for k in 0..=n {
            let t = k as f64 / n as f64;
            let p = [0, 1, 2].map(|j| m.1[j] + (m.2[j] - m.1[j]) * t);
            grid.entry(cell(&p)).or_default().push((p, i));
        }
    }
    let zmin = ms.iter().flat_map(|m| [m.1[2], m.2[2]]).fold(f64::MAX, f64::min);
    let mut free = 0;
    for (i, m) in ms.iter().enumerate() {
        for p in [m.1, m.2] {
            if p[2] <= zmin + 0.5 {
                continue;
            }
            let c = cell(&p);
            let mut near = false;
            'scan: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for (q, j) in grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]).map_or(&[][..], |v| &v[..]) {
                            if *j != i && (0..3).map(|k| (q[k] - p[k]).powi(2)).sum::<f64>() <= tol * tol {
                                near = true;
                                break 'scan;
                            }
                        }
                    }
                }
            }
            free += usize::from(!near);
        }
    }
    (free, ms.len() * 2)
}

#[test]
fn t01_tower_from_one_call_meets_every_deep_check() {
    let Built { m, .. } = build();
    let n = |t: &str| count(&m, &format!("SELECT count(*) FROM {t}"));
    let (col, beam, brace) = (n("col"), n("beam"), n("brace"));
    assert!(col + beam + brace >= 10_000, "members col {col} beam {beam} brace {brace}");
    assert!(brace >= 2_000, "{brace} braces");
    assert!(n("steel_conn") >= 500, "{} connections", n("steel_conn"));
    assert!(n("steel_conn_part") >= 1_000, "{} connection parts", n("steel_conn_part"));
    assert!(n("slab") >= 3 && n("level") >= 3, "{} slabs, {} levels", n("slab"), n("level"));
    assert!(n("stair") + n("comp") >= 2, "{} stairs, {} lifts", n("stair"), n("comp"));
    assert!(n("stair") >= 1 && n("comp") >= 1, "stairs and lifts both");
    let foot = n("iso_footing") + n("strip_footing") + n("raft");
    assert!(foot >= 4, "{foot} piers or footings");

    let ms = members(&m);
    let pts: Vec<P3> = ms.iter().flat_map(|x| [x.1, x.2]).collect();
    let top = pts.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
    let low = pts.iter().map(|p| p[2]).fold(f64::MAX, f64::min);
    assert!((300.0..=330.0).contains(&top) && low >= -1e-6, "top {top:.1} m, foot {low:.1} m");
    let (r, taper, dev) = profile(&pts);
    assert!(taper >= 3.0, "base / top half-width {taper:.2}; bands {r:?}");
    assert!(dev >= 0.03, "legs depart {dev:.3} of the base half-width from a straight taper; bands {r:?}");
    let feet: HashSet<(i64, i64)> =
        pts.iter().filter(|p| p[2] <= low + 0.04 * (top - low)).map(|p| ((p[0] / 8.0).round() as i64, (p[1] / 8.0).round() as i64)).collect();
    assert!(feet.len() >= 4, "{} foot clusters", feet.len());

    // sections: at least four types in at least three shapes beyond a plain rectangle
    let shapes: Vec<(String, String)> = m
        .sql()
        .prepare("SELECT name, shape FROM member_type")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let kinds: HashSet<&str> = shapes.iter().filter_map(|s| s.1.split_whitespace().next()).collect();
    assert!(shapes.len() >= 4, "{shapes:?}");
    assert!(kinds.len() >= 3 && kinds.iter().any(|k| *k != "rect"), "{kinds:?}");
    let untyped: Vec<_> = ms.iter().filter(|x| x.3.is_none()).map(|x| (x.0, x.1)).take(5).collect();
    assert!(untyped.is_empty(), "members without a section type (NULL mtype): {untyped:?}");
    let used: HashSet<i64> = ms.iter().filter_map(|x| x.3).collect();
    assert!(used.len() >= 4, "{} section types in use", used.len());
    // the sections get lighter up the tower: the heaviest leg column of the lowest sixth
    // outweighs the heaviest of the top sixth
    let ids: HashMap<i64, String> = m
        .sql()
        .prepare("SELECT id, name FROM member_type")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let area = |t: Option<i64>| {
        let t = t.expect("member without a section type (NULL mtype)");
        let name = ids.get(&t).unwrap_or_else(|| panic!("mtype {t} is not a member_type row"));
        ddc_geom::steel::tower_size::props(name).map_or(0.0, |p| p.0)
    };
    let band = |lo: f64, hi: f64| ms.iter().filter(|x| x.0 == "col" && x.1[2] >= lo && x.1[2] < hi).map(|x| area(x.3)).fold(0.0, f64::max);
    let (foot_a, head_a) = (band(low, low + (top - low) / 6.0), band(top - (top - low) / 6.0, top + 1.0));
    assert!(head_a > 0.0 && foot_a > 2.0 * head_a, "leg section area at the foot {foot_a:.5} m2, at the top {head_a:.5} m2");

    // platforms near 57, 115 and 276 m: a level or a deck slab within 12 m
    let mut zs: Vec<f64> = m.sql().prepare("SELECT elevation FROM level").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    zs.extend(m.sql().prepare("SELECT l.elevation + s.offset FROM slab s JOIN level l ON l.id = s.level").unwrap().query_map([], |r| r.get::<_, f64>(0)).unwrap().map(Result::unwrap));
    for h in [57.0, 115.0, 276.0] {
        assert!(zs.iter().any(|z| (z - h).abs() <= 12.0), "no platform near {h} m in {zs:?}");
    }
    // long low members linking the legs (base arches and first-platform girders)
    let arch = ms.iter().filter(|x| x.0 == "beam" && x.1[2] < 80.0 && x.2[2] < 80.0 && (x.1[0] - x.2[0]).hypot(x.1[1] - x.2[1]) > 10.0).count();
    assert!(arch >= 8, "{arch} long low beams");

    let mut seen = HashSet::new();
    let key = |p: &P3| p.map(|v| (v * 100.0).round() as i64);
    let mut dup = 0;
    for x in &ms {
        let (a, b) = (key(&x.1), key(&x.2));
        dup += usize::from(seen.contains(&(x.0, a, b)) || seen.contains(&(x.0, b, a)));
        seen.insert((x.0, a, b));
    }
    assert_eq!(dup, 0, "duplicate members");
    let (free, ends) = free_ends(&ms, 0.3);
    assert_eq!(free, 0, "{free} free of {ends} member ends");
}

/// A tower without `service` is a plain lattice tower, not a power pylon; joints follow the
/// detail unless asked for all or none.
#[test]
fn a_tower_is_plain_by_default_and_joints_follow_the_detail() {
    let g = registry().iter().find(|g| g.id() == "steel_frame").unwrap();
    let GenOut::Steel { frame, joints, .. } = g.build(&json!({"kind": "lattice_tower_300"}), 1).unwrap() else { panic!() };
    assert!(frame.insulators.is_empty() && !joints);
    let j = |p| ddc_gen::steel::joints(&p).unwrap();
    assert!(j(json!({"kind": "lattice_tower_300", "detail": 2})));
    assert!(!j(json!({"kind": "lattice_tower_300", "detail": 3, "joints": "none"})));
    assert!(j(json!({"kind": "portal_frame", "joints": "all"})));
    assert!(!j(json!({"kind": "lattice_tower_300", "detail": 3, "service": "power_line"})));
}

/// Generator, model rows and joints of the 10 000+ member tower within budget (s): a debug
/// build is held to the looser one.
#[test]
fn t01_tower_builds_within_its_budget() {
    let Built { m, secs } = build();
    let members = count(&m, "SELECT (SELECT count(*) FROM col) + (SELECT count(*) FROM beam) + (SELECT count(*) FROM brace)");
    eprintln!("t01 tower: {members} members, generator {:.2} s, model {:.2} s, joints {:.2} s", secs[0], secs[1], secs[2]);
    let budget = if cfg!(debug_assertions) { [120.0, 300.0, 600.0] } else { [10.0, 30.0, 60.0] };
    for (what, (s, b)) in ["generator", "model", "joints"].iter().zip(secs.iter().zip(budget)) {
        assert!(*s <= b, "{what} took {s:.2} s, budget {b} s");
    }
}

/// The joints of a second tower in the same model cost what the first did: candidates come from
/// the members around the frame and the existing connections are read once, so the work does
/// not grow with the model (a scan of the whole model per node would be four times the first).
#[test]
fn joints_of_a_second_tower_cost_what_the_first_did() {
    let g = registry().iter().find(|g| g.id() == "steel_frame").unwrap();
    let p = json!({"kind": "lattice_tower_300", "tower_height": 120, "base_width": 50, "detail": 2});
    let GenOut::Steel { frame, .. } = g.build(&p, 1).unwrap() else { panic!("not steel") };
    // the lattice only: decks, stairs and piers do not take joints
    let mut one = *frame;
    one.piers.clear();
    one.guards.clear();
    one.ramps.clear();
    one.stairs.clear();
    one.decks.clear();
    one.flights.clear();
    one.lifts.clear();
    one.insulators.clear();
    let mut two = one.clone();
    for n in &mut two.nodes {
        n.x += 200.0;
    }
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let mut secs = vec![];
    let mut counts = vec![];
    for f in [&one, &two] {
        let ids = m.add_steel_frame(lv, f).unwrap();
        let t = Instant::now();
        counts.push(m.connect_steel_frame(&ids).unwrap().len());
        secs.push(t.elapsed().as_secs_f64());
    }
    eprintln!("joints: first tower {} in {:.2} s, second {} in {:.2} s", counts[0], secs[0], counts[1], secs[1]);
    assert!(counts[0] > 100 && counts[1] == counts[0], "joints {counts:?}");
    let budget = 2.0 * secs[0] + 0.5;
    assert!(secs[1] <= budget, "second tower joints {:.2} s, budget {budget:.2} s (first {:.2} s)", secs[1], secs[0]);
}
