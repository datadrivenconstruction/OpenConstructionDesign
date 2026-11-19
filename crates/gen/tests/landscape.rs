//! Landscape of a generated site: hedge, beds and shrubs, lawn, paths to the street, tree
//! setbacks and rows, all from `site.*` rules and `data/landscape.json`.

use ddc_gen::project::{build_project, check_project, parse_spec, GProject};
use ddc_gen::{GFixture, WallRole};
use serde_json::{json, Value as J};

type P = (f64, f64);

const LIB: &str = include_str!("../../model/data/library.ddcf");
const DATA: &str = include_str!("../data/landscape.json");

fn site(rules: J, trees: u32) -> GProject {
    let v = json!({"typology": "office", "floors": 3, "design": {"trees": trees, "paving": 3.0}, "rules": rules});
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    assert!(check_project(&s, &p).passed());
    p
}

fn fixtures<'a>(p: &'a GProject, family: &str, ty: Option<&str>) -> Vec<&'a GFixture> {
    p.storeys[0].layout.fixtures.iter().filter(|f| f.family == family && ty.map_or(true, |t| f.ty == t)).collect()
}

fn at(p: &GProject, family: &str, ty: Option<&str>) -> Vec<P> {
    fixtures(p, family, ty).iter().map(|f| f.at).collect()
}

fn inside(r: &[P], p: P) -> bool {
    let n = r.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            odd = !odd;
        }
    }
    odd
}

fn seg(p: P, a: P, b: P) -> f64 {
    let (d, e) = ((b.0 - a.0, b.1 - a.1), (p.0 - a.0, p.1 - a.1));
    let t = ((d.0 * e.0 + d.1 * e.1) / (d.0 * d.0 + d.1 * d.1).max(1e-12)).clamp(0.0, 1.0);
    (p.0 - a.0 - d.0 * t).hypot(p.1 - a.1 - d.1 * t)
}

fn to_slab(p: &GProject, q: P) -> f64 {
    let s = &p.storeys[0].layout.slab;
    (0..s.len()).map(|i| seg(q, s[i], s[(i + 1) % s.len()])).fold(f64::MAX, f64::min)
}

fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn boxed(v: &[P]) -> (P, P) {
    v.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| {
        ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
    })
}

#[test]
fn the_landscape_data_names_library_types_of_the_sizes_it_assumes() {
    let d: J = serde_json::from_str(DATA).unwrap();
    let has = |fam: &str, ty: &str| {
        let src = LIB.split("\n---").find(|s| s.lines().any(|l| l.trim() == format!("family {fam}"))).unwrap();
        src.lines().any(|l| l.starts_with(&format!("type \"{ty}\"")))
    };
    for (_, ty) in d["hedge"]["types"].as_object().unwrap() {
        assert!(has("Hedge", ty.as_str().unwrap()), "{ty}");
    }
    for ty in d["shrubs"].as_array().unwrap() {
        assert!(has("Shrub", ty.as_str().unwrap()), "{ty}");
    }
    assert!(has("Lawn", d["lawn"]["type"].as_str().unwrap()) && has("Lawn", d["bed"]["type"].as_str().unwrap()));
    assert!(has("Paving Slab", d["path"]["type"].as_str().unwrap()));
    // the tile sizes of the data are the sizes of the library types
    let mm = |x: f64| format!("{}mm", (x * 1000.0).round());
    assert!(LIB.contains(&format!("type \"Lawn\" Size={}", mm(d["lawn"]["tile"].as_f64().unwrap()))));
    assert!(LIB.contains(&format!("type \"Bed\" Size={}", mm(d["bed"]["tile"].as_f64().unwrap()))));
    assert!(LIB.contains(&format!("type \"Path\" Size={}", mm(d["path"]["tile"].as_f64().unwrap()))));
    assert!(LIB.contains(&format!("param Length length type {}", mm(d["hedge"]["length"].as_f64().unwrap()))));
}

#[test]
fn a_site_gets_hedge_beds_shrubs_lawn_and_paths_off_the_building() {
    let p = site(json!({}), 8);
    let l = &p.storeys[0].layout;
    let (hedge, beds, shrubs, lawn, path) = (
        at(&p, "Hedge", None),
        at(&p, "Lawn", Some("Bed")),
        at(&p, "Shrub", None),
        at(&p, "Lawn", Some("Lawn")),
        at(&p, "Paving Slab", Some("Path")),
    );
    for (what, v) in [("hedge", &hedge), ("beds", &beds), ("shrubs", &shrubs), ("lawn", &lawn), ("path", &path)] {
        assert!(v.len() >= 5, "{what}: {}", v.len());
        for q in v {
            assert!(!inside(&l.slab, *q), "{what} stands on the building at {q:?}");
        }
    }
    // every shrub stands in a bed, and the beds, lawn and paths do not overlap
    assert!(shrubs.iter().all(|s| beds.iter().any(|b| dist(*s, *b) < 1e-9)));
    for b in &beds {
        assert!(path.iter().all(|t| (t.0 - b.0).abs() >= 1.5 - 1e-9 || (t.1 - b.1).abs() >= 1.5 - 1e-9), "bed on a path at {b:?}");
    }
    for q in &lawn {
        assert!(beds.iter().all(|b| (b.0 - q.0).abs() >= 3.75 || (b.1 - q.1).abs() >= 3.75), "lawn over a bed at {q:?}");
        assert!(path.iter().all(|t| (t.0 - q.0).abs() >= 3.75 || (t.1 - q.1).abs() >= 3.75), "lawn over a path at {q:?}");
    }
    // the hedge closes the plot: everything else lies inside its box
    let (lo, hi) = boxed(&hedge);
    for q in beds.iter().chain(&shrubs).chain(&lawn).chain(&path).chain(&at(&p, "Tree", None)) {
        assert!(q.0 > lo.0 - 1e-9 && q.0 < hi.0 + 1e-9 && q.1 > lo.1 - 1e-9 && q.1 < hi.1 + 1e-9, "{q:?} outside the hedge");
    }
    // same brief, same landscape
    let again = site(json!({}), 8);
    assert_eq!(fixtures(&p, "Hedge", None).len(), fixtures(&again, "Hedge", None).len());
    assert_eq!(at(&p, "Shrub", None), at(&again, "Shrub", None));
}

fn doors(p: &GProject) -> Vec<P> {
    let l = &p.storeys[0].layout;
    l.openings
        .iter()
        .filter(|o| o.door && l.walls[o.wall].role == WallRole::Exterior)
        .map(|o| {
            let w = &l.walls[o.wall];
            let (d, t) = ((w.b.0 - w.a.0, w.b.1 - w.a.1), o.along + o.width / 2.0);
            let n = d.0.hypot(d.1);
            (w.a.0 + d.0 / n * t, w.a.1 + d.1 / n * t)
        })
        .collect()
}

/// Tiles reached from `from` through neighbours at most `step` apart.
fn reach(tiles: &[P], from: usize, step: f64) -> Vec<bool> {
    let mut seen = vec![false; tiles.len()];
    let mut todo = vec![from];
    seen[from] = true;
    while let Some(i) = todo.pop() {
        for j in 0..tiles.len() {
            if !seen[j] && dist(tiles[i], tiles[j]) <= step {
                seen[j] = true;
                todo.push(j);
            }
        }
    }
    seen
}

#[test]
fn every_entrance_has_a_connected_path_to_a_gap_in_the_hedge_on_the_street() {
    for (side, along_x, sign) in [("south", false, -1.0), ("north", false, 1.0), ("east", true, 1.0), ("west", true, -1.0)] {
        let p = site(json!({"site.street_side": side}), 6);
        let (path, hedge) = (at(&p, "Paving Slab", Some("Path")), at(&p, "Hedge", None));
        let (lo, hi) = boxed(&hedge);
        let key = |q: &P| sign * if along_x { q.0 } else { q.1 };
        // the tile nearest the street
        let end = (0..path.len()).max_by(|a, b| key(&path[*a]).total_cmp(&key(&path[*b]))).unwrap();
        let street = if along_x {
            if sign > 0.0 { hi.0 } else { lo.0 }
        } else if sign > 0.0 {
            hi.1
        } else {
            lo.1
        };
        let off = (key(&path[end]) - sign * street).abs();
        assert!(off < 1.0, "{side}: the path ends {off} m from the street");
        // a gate: no hedge length across the path where it leaves the plot
        assert!(hedge.iter().all(|h| dist(*h, path[end]) > 1.5), "{side}: the hedge closes the gate");
        // all entrances lead to it
        let linked = reach(&path, end, 1.5 * 1.5);
        let ds = doors(&p);
        assert!(!ds.is_empty());
        for d in ds {
            let near: Vec<usize> = (0..path.len()).filter(|i| dist(path[*i], d) < 2.5).collect();
            assert!(!near.is_empty(), "{side}: no path at the door {d:?}");
            assert!(near.iter().any(|i| linked[*i]), "{side}: the path at the door {d:?} does not reach the street");
        }
    }
}

#[test]
fn trees_keep_the_setback_and_clear_the_paths_and_each_other() {
    for rules in [json!({}), json!({"site.tree_setback": 9.0}), json!({"site.tree_path_clear": 3.0, "site.path_width": 3.0})] {
        let p = site(rules.clone(), 10);
        let trees = at(&p, "Tree", None);
        let path = at(&p, "Paving Slab", Some("Path"));
        let setback = rules["site.tree_setback"].as_f64().unwrap_or(3.0);
        let clear = rules["site.tree_path_clear"].as_f64().unwrap_or(1.5);
        assert!(trees.len() >= 4, "{rules}: {} trees", trees.len());
        for (i, t) in trees.iter().enumerate() {
            assert!(to_slab(&p, *t) >= setback - 1e-9, "{rules}: a tree {:.2} m from the building", to_slab(&p, *t));
            assert!(path.iter().all(|q| dist(*q, *t) >= 0.75 + clear - 1e-9), "{rules}: a tree on a path at {t:?}");
            assert!(trees[..i].iter().all(|u| dist(*u, *t) >= 4.0 - 1e-9), "{rules}: trees too close");
        }
    }
}

#[test]
fn tree_rows_follow_the_street_and_the_paths_by_rule() {
    let none = site(json!({"site.tree_rows": "none"}), 8);
    let street = site(json!({"site.tree_rows": "street", "site.row_spacing": 8.0}), 8);
    let along = site(json!({"site.tree_rows": "path"}), 8);
    let both = site(json!({"site.tree_rows": "both"}), 8);
    let n = |p: &GProject| at(p, "Tree", None).len();
    assert!(
        n(&street) >= n(&none) + 3 && n(&along) > n(&none) && n(&both) >= n(&street).max(n(&along)),
        "{} {} {} {}",
        n(&none),
        n(&street),
        n(&along),
        n(&both)
    );
    // the street row: the southernmost trees stand on one line, a row apart
    let trees = at(&street, "Tree", None);
    let south = trees.iter().map(|t| t.1).fold(f64::MAX, f64::min);
    let mut row: Vec<f64> = trees.iter().filter(|t| (t.1 - south).abs() < 1e-6).map(|t| t.0).collect();
    row.sort_by(f64::total_cmp);
    assert!(row.len() >= 3, "{row:?}");
    assert!(row.windows(2).all(|w| (w[1] - w[0]) >= 8.0 - 1e-6), "{row:?}");
    // a path row flanks a path on both sides
    let path = at(&along, "Paving Slab", Some("Path"));
    let extra: Vec<P> = at(&along, "Tree", None).into_iter().filter(|t| !at(&none, "Tree", None).contains(t)).collect();
    assert!(extra.len() >= 2);
    for t in &extra {
        let d = path.iter().map(|q| dist(*q, *t)).fold(f64::MAX, f64::min);
        assert!(d <= 4.5 && d >= 0.75 + 1.5 - 1e-9, "a path-row tree {d:.2} m from the path");
    }
}

#[test]
fn the_landscape_rules_switch_each_part() {
    let count = |p: &GProject| {
        fixtures(p, "Hedge", None).len()
            + fixtures(p, "Shrub", None).len()
            + fixtures(p, "Lawn", None).len()
            + fixtures(p, "Paving Slab", Some("Path")).len()
    };
    let base = site(json!({}), 8);
    let off = site(json!({"site.landscape": false}), 8);
    assert_eq!(count(&off), 0);
    assert!(!fixtures(&off, "Paving Slab", Some("1500")).is_empty() && !fixtures(&off, "Tree", None).is_empty());
    assert!(count(&base) > 40);
    assert!(fixtures(&site(json!({"site.hedge": "none"}), 8), "Hedge", None).is_empty());
    let tall = site(json!({"site.hedge": "tall"}), 8);
    assert!(!fixtures(&tall, "Hedge", None).is_empty() && fixtures(&tall, "Hedge", None).iter().all(|h| h.ty == "Tall 1800"));
    let nobed = site(json!({"site.bed_width": 0.0}), 8);
    assert!(fixtures(&nobed, "Lawn", Some("Bed")).is_empty() && fixtures(&nobed, "Shrub", None).is_empty());
    let wide = site(json!({"site.path_width": 3.0}), 8);
    assert!(fixtures(&wide, "Paving Slab", Some("Path")).len() > fixtures(&base, "Paving Slab", Some("Path")).len());
    // no paving and no trees: no landscape, as before
    let plain = build_project(&parse_spec(&json!({"typology": "office", "floors": 3})).unwrap()).unwrap();
    assert_eq!(fixtures(&plain, "Hedge", None).len() + fixtures(&plain, "Lawn", None).len(), 0);
}

#[test]
fn hedge_lengths_and_shrubs_differ_in_size_by_seed() {
    let p = site(json!({}), 4);
    let hedge: Vec<f64> = fixtures(&p, "Hedge", None).iter().map(|f| f.scale).collect();
    assert!(hedge.len() >= 4, "{}", hedge.len());
    assert!(hedge.iter().all(|s| (0.95 - 1e-9..=1.05 + 1e-9).contains(s)), "{hedge:?}");
    assert!(hedge.iter().any(|s| (s - hedge[0]).abs() > 1e-6), "all lengths alike: {hedge:?}");
    let shrubs = fixtures(&p, "Shrub", None);
    if shrubs.len() >= 3 {
        assert!(shrubs.iter().all(|f| (0.8 - 1e-9..=1.2 + 1e-9).contains(&f.scale)));
        let turns: std::collections::BTreeSet<u64> = shrubs.iter().map(|f| f.rotation.to_bits()).collect();
        assert!(turns.len() >= 2, "shrubs turn alike");
    }
    let q = site(json!({}), 4);
    let again: Vec<f64> = fixtures(&q, "Hedge", None).iter().map(|f| f.scale).collect();
    assert_eq!(hedge, again, "a site keeps its plants");
}
