//! Span, support, depth, timber and air-delivery oracles on hand-built kernels.

use super::case::Case;
use super::facts::{Comp, Facts, Member, Room, Run, Shaft, P3};
use super::oracle::{evaluate, EvalCx, OracleResult};
use super::tests::{editor, level, sql};
use crate::editor::Editor;

fn check_in(ed: &Editor, f: &Facts, kind: &str, params: &str) -> OracleResult {
    let c = Case::parse(&format!("id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"{kind}\"\n{params}\n")).unwrap();
    let cx = EvalCx { ed, facts: f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
    evaluate(&c, &cx).remove(0)
}

fn check(f: &Facts, kind: &str, params: &str) -> OracleResult {
    check_in(&editor(), f, kind, params)
}

fn mem(kind: &'static str, a: P3, b: P3) -> Member {
    Member { kind, a, b, mtype: None, bulge: 0.0 }
}

/// A hangar: columns on the two long walls 80 m apart, roof beams between them at 20 m, a low
/// office column in the middle.
fn hangar(mid_column: bool) -> Facts {
    let mut f = Facts::default();
    for i in 0..=10 {
        let y = i as f64 * 10.0;
        f.members.push(mem("col", [0.0, y, 0.0], [0.0, y, 20.0]));
        f.members.push(mem("col", [80.0, y, 0.0], [80.0, y, 20.0]));
        f.members.push(mem("beam", [0.0, y, 20.0], [80.0, y, 20.0]));
    }
    f.members.push(mem("col", [40.0, 95.0, 0.0], [40.0, 95.0, 4.0]));
    if mid_column {
        f.members.push(mem("col", [40.0, 50.0, 0.0], [40.0, 50.0, 20.0]));
    }
    f
}

#[test]
fn a_clear_span_is_the_gap_between_neighbouring_supports_that_reach_the_roof() {
    let r = check(&hangar(false), "clear_span", "target=80.0");
    assert!(r.pass && r.measured == "80.0 m between supports at x = 0.0 and 80.0", "{r:?}");
    // the low office column does not break the span; a roof-high one does
    let r = check(&hangar(true), "clear_span", "target=80.0");
    assert!(!r.pass && r.measured.starts_with("40.0 m"), "{r:?}");
    assert!(!check(&Facts::default(), "clear_span", "target=80.0").pass);
}

/// Arches of eight chords every 10 m: 80 m span, 16 m rise, landing on the ground.
fn arch_hangar() -> Facts {
    let mut f = Facts::default();
    let at = |t: f64, y: f64| [80.0 * t, y, 64.0 * t * (1.0 - t)];
    for i in 0..=6 {
        let y = i as f64 * 10.0;
        for k in 0..8 {
            f.members.push(mem("beam", at(k as f64 / 8.0, y), at((k + 1) as f64 / 8.0, y)));
        }
        if i > 0 {
            f.members.push(mem("beam", at(0.5, y - 10.0), at(0.5, y)));
        }
    }
    f
}

#[test]
fn an_arch_stands_on_its_springings() {
    let f = arch_hangar();
    let r = check(&f, "clear_span", "target=80.0");
    assert!(r.pass && r.measured == "80.0 m between supports at x = 0.0 and 80.0", "{r:?}");
    let r = check(&f, "supports_connected", "");
    assert!(r.pass && r.measured == "14 of 14 supports touch it (1.00)", "{r:?}");
    // a truss lying on the ground rests on its chord, its diagonals land on nothing new
    let mut t = Facts::default();
    t.members.push(mem("beam", [0.0, 0.0, 0.0], [80.0, 0.0, 0.0]));
    for k in 0..8 {
        let (x0, x1) = (k as f64 * 10.0, k as f64 * 10.0 + 10.0);
        t.members.push(mem("brace", [x0, 0.0, 0.0], [x1, 0.0, 4.0]));
    }
    assert!(check(&t, "clear_span", "target=80.0").measured.contains("0 support(s)"));
}

#[test]
fn a_bridge_span_is_measured_between_named_piers() {
    let mut f = Facts::default();
    f.members.push(mem("beam", [0.0, 0.0, 6.0], [90.0, 0.0, 6.0]));
    let pier = |id, x| Comp { id, level: 1, at: [x, 0.0, 0.0], family: "Pier".into(), category: "structure".into() };
    f.comps = vec![pier(1, 0.0), pier(2, 90.0)];
    assert!(check(&f, "clear_span", "target=90.0").pass);
    f.comps.push(pier(3, 45.0));
    let r = check(&f, "clear_span", "target=90.0");
    assert!(!r.pass && r.measured.starts_with("45.0 m"), "{r:?}");
}

#[test]
fn the_reach_is_the_farthest_roof_point_from_a_support() {
    let mut f = Facts::default();
    f.members.push(mem("col", [0.0, 0.0, 0.0], [0.0, 0.0, 20.0]));
    f.members.push(mem("truss", [0.0, 0.0, 20.0], [35.0, 0.0, 20.0]));
    let r = check(&f, "cantilever_reach", "min=30.0");
    assert!(r.pass && r.measured == "35.0 m", "{r:?}");
    assert!(!check(&f, "cantilever_reach", "min=40.0").pass);
    f.members.remove(0);
    assert!(check(&f, "cantilever_reach", "min=1.0").measured.contains("no support"));
}

#[test]
fn every_support_must_touch_the_structure() {
    let mut f = hangar(false);
    let r = check(&f, "supports_connected", "");
    assert!(r.pass && r.measured == "22 of 22 supports touch it (1.00)", "{r:?}");
    f.members.push(mem("col", [200.0, 200.0, 0.0], [200.0, 200.0, 20.0]));
    let r = check(&f, "supports_connected", "");
    assert!(!r.pass && r.measured.contains("22 of 23") && r.measured.contains("(200.0, 200.0) touches nothing"), "{r:?}");
}

fn room(id: i64, level: i64, name: &str) -> Room {
    Room { id, level, name: name.into(), at: [0.0, 0.0] }
}

fn station() -> Facts {
    let mut f = Facts::default();
    f.level_z = [(1, -18.0), (2, -9.0), (3, 0.0)].into_iter().collect();
    f.rooms = vec![room(1, 1, "Platform"), room(2, 2, "Ticket hall"), room(3, 3, "Entrance")];
    f.shafts = vec![
        Shaft { level: 1, top: 3, bbox: Some([0.0, 0.0, 4.0, 4.0]) },
        Shaft { level: 1, top: 3, bbox: Some([146.0, 0.0, 150.0, 4.0]) },
    ];
    f
}

#[test]
fn the_platform_lies_at_the_depth_with_the_concourse_above_and_shafts_at_both_ends() {
    let f = station();
    assert!(check(&f, "room_depth", "room=\"platform\"\ndepth=18.0").pass);
    let r = check(&f, "room_above", "upper=\"ticket|concourse\"\nlower=\"platform\"\ngap_min=3.0");
    assert!(r.pass && r.measured.contains("9.0 m above"), "{r:?}");
    let r = check(&f, "shaft_spread", "apart_min=100.0");
    assert!(r.pass && r.measured.contains("146.0 m apart"), "{r:?}");
    let mut g = station();
    g.level_z.insert(1, -4.0);
    let r = check(&g, "room_depth", "room=\"platform\"\ndepth=18.0");
    assert!(!r.pass && r.measured == "1 room(s) at z -4.0..-4.0 m", "{r:?}");
    g.level_z.insert(1, -9.0);
    assert!(!check(&g, "room_above", "upper=\"ticket\"\nlower=\"platform\"").pass);
    g.shafts.pop();
    assert!(!check(&g, "shaft_spread", "").pass);
    assert!(!check(&f, "room_depth", "room=\"nonexistent\"\ndepth=18.0").pass);
}

fn timber_model(slab2_wood: bool) -> Editor {
    let ed = editor();
    let lv = level(&ed);
    sql(&ed, "INSERT INTO wall_type(id, name) VALUES(101, 'CLT'), (102, 'Core')");
    sql(&ed, "INSERT INTO wall_type_layer(wtype, pos, material, thickness) VALUES(101, 0, 'CLT 140', 0.14), (102, 0, 'Concrete', 0.3)");
    sql(&ed, "INSERT INTO slab_type(id, name) VALUES(101, 'CLT floor'), (102, 'RC floor')");
    sql(&ed, "INSERT INTO slab_type_layer(stype, pos, material, thickness) VALUES(101, 0, 'Cross-laminated timber', 0.2), (102, 0, 'Concrete', 0.2)");
    for (id, wt) in [(1, 101), (2, 102)] {
        sql(&ed, &format!("INSERT INTO wall(id, level, wtype, ax, ay, bx, by, base_offset, height) VALUES({id}, {lv}, {wt}, 0, 0, 10, 0, 0, 3)"));
    }
    for (id, st) in [(1, 101), (2, if slab2_wood { 101 } else { 102 })] {
        sql(&ed, &format!("INSERT INTO slab(id, level, stype, offset) VALUES({id}, {lv}, {st}, 0)"));
        for (i, (x, y)) in [(0, 0), (10, 0), (10, 10), (0, 10)].iter().enumerate() {
            sql(&ed, &format!("INSERT INTO slab_vertex(slab, ring, idx, x, y) VALUES({id}, 0, {i}, {x}, {y})"));
        }
    }
    ed
}

fn frame(glulam: usize, steel: usize) -> Facts {
    let mut f = Facts::default();
    f.mtypes.insert(1, ("GL24h".into(), "rect".into(), "Glulam".into()));
    f.mtypes.insert(2, ("HEA".into(), "i".into(), "Steel".into()));
    for (n, t) in [(glulam, 1), (steel, 2)] {
        for _ in 0..n {
            f.members.push(Member { kind: "col", a: [0.0; 3], b: [0.0, 0.0, 3.0], mtype: Some(t), bulge: 0.0 });
        }
    }
    f
}

#[test]
fn timber_walls_slabs_and_frame_are_measured_by_area_and_count() {
    let ed = timber_model(true);
    let r = check_in(&ed, &frame(9, 1), "timber_share", "");
    assert!(r.pass && r.measured == "walls 0.50 of 60 m2, slabs 1.00 of 200 m2, frame 0.90 of 10", "{r:?}");
    // a concrete second floor: half the slab area is timber
    let ed = timber_model(false);
    let r = check_in(&ed, &frame(9, 1), "timber_share", "");
    assert!(!r.pass && r.measured.contains("slabs 0.50"), "{r:?}");
    // a steel frame fails, a higher wall bar fails, and a model with no walls is not a pass
    let ed = timber_model(true);
    assert!(!check_in(&ed, &frame(1, 9), "timber_share", "").pass);
    assert!(!check_in(&ed, &frame(9, 1), "timber_share", "walls_min=0.6").pass);
    assert!(!check(&frame(9, 1), "timber_share", "").pass);
}

fn pipe(id: i64, pts: &[P3]) -> Run {
    Run { id, system: "supply".into(), level: 1, domain: "duct".into(), shape: "rect".into(), w: 0.4, h: 0.3, dn: None, slope: 0.0, pts: pts.to_vec(), cap: [false, false] }
}

fn part(id: i64, family: &str, at: P3) -> Comp {
    Comp { id, level: 1, at, family: family.into(), category: "equipment".into() }
}

#[test]
fn a_terminal_is_fed_only_when_its_duct_network_reaches_an_air_handler() {
    let mut f = Facts::default();
    f.runs = vec![pipe(1, &[[0.0, 0.0, 3.0], [10.0, 0.0, 3.0]]), pipe(2, &[[10.0, 0.0, 3.0], [20.0, 0.0, 3.0]]), pipe(3, &[[0.0, 30.0, 3.0], [10.0, 30.0, 3.0]])];
    f.comps = vec![part(1, "AHU-1", [0.0, 0.0, 3.0]), part(2, "Diffuser", [15.0, 0.5, 3.0]), part(3, "Diffuser", [5.0, 30.5, 3.0])];
    let r = check(&f, "terminals_fed", "");
    assert!(!r.pass && r.measured == "1 of 2 terminals fed (0.50); unfed: Diffuser 3", "{r:?}");
    f.comps.remove(2);
    let r = check(&f, "terminals_fed", "");
    assert!(r.pass && r.measured == "1 of 1 terminals fed (1.00)", "{r:?}");
    // the same duct with the handler taken away feeds nothing
    f.comps.remove(0);
    assert!(!check(&f, "terminals_fed", "").pass);
    assert!(check(&Facts::default(), "terminals_fed", "").measured == "no air terminals");
}

#[test]
fn the_remaining_hard_cases_carry_their_oracles() {
    let dir = super::case::evals_dir().join("cases");
    let get = |id: &str| Case::load(&dir.join(format!("{id}.toml"))).unwrap_or_else(|e| panic!("{e}"));
    let kinds = |id: &str| get(id).oracle.geom.iter().map(|g| g.kind.clone()).collect::<Vec<_>>();
    for (id, want) in [
        ("H02_hospital_hvac", vec!["terminals_fed", "served_rooms", "mep_clashes"]),
        ("H12_de_hospital_hvac", vec!["terminals_fed", "served_rooms", "mep_clashes"]),
        ("H15_hospital_add_theatre", vec!["terminals_fed", "served_rooms", "mep_clashes"]),
        ("H04_stadium_roof", vec!["cantilever_reach", "supports_connected"]),
        ("H10_footbridge_arch", vec!["clear_span", "supports_connected"]),
        ("H16_hangar_space_frame", vec!["clear_span", "supports_connected"]),
        ("H08_metro_station", vec!["room_depth", "room_above", "shaft_spread"]),
        ("H09_timber_multistorey", vec!["timber_share"]),
    ] {
        let k = kinds(id);
        for w in want {
            assert!(k.contains(&w.to_string()), "{id} lacks {w}");
        }
    }
}

/// Generic checks that say nothing about what a scenario is.
const GENERIC: [&str; 3] = ["ifc_wellformed", "no_dup_members", "free_ends"];

fn real(c: &Case) -> bool {
    let o = &c.oracle;
    o.geom.iter().any(|g| !GENERIC.contains(&g.kind.as_str()))
        || !(o.query.is_empty() && o.qty.is_empty() && o.receipt.is_empty() && o.grow.is_empty() && o.text.is_empty() && o.claim.is_empty())
        || o.count.iter().any(|k| k.min.unwrap_or(0.0) >= 2.0)
}

#[test]
fn every_scenario_in_every_language_has_a_real_oracle() {
    let cases = super::case::load_dir(&super::case::evals_dir().join("cases")).unwrap();
    // scenarios still measured only by one-off counts; the list may only shrink
    const GAPS: [&str; 0] = [];
    let mut bad = vec![];
    for c in &cases {
        let family = c.id.split("__").next().unwrap_or(&c.id);
        if !real(c) && !GAPS.contains(&family) {
            bad.push(c.id.clone());
        }
        // the hard cases named by letter H are judged on what they are, in every language
        if c.id.starts_with('H') && c.tier == "hard" && !c.oracle.geom.iter().any(|g| !GENERIC.contains(&g.kind.as_str())) {
            bad.push(format!("{} (no scenario geometry oracle)", c.id));
        }
    }
    assert!(bad.is_empty(), "cases without a real oracle: {bad:?}");
}

#[test]
fn language_variants_carry_the_same_geometry_oracles_as_their_scenario() {
    let cases = super::case::load_dir(&super::case::evals_dir().join("cases")).unwrap();
    let kinds = |c: &Case| {
        let mut k: Vec<String> = c.oracle.geom.iter().map(|g| g.kind.clone()).collect();
        k.sort();
        k.dedup();
        k
    };
    for c in &cases {
        let Some((stem, _)) = c.id.split_once("__") else { continue };
        if !stem.starts_with('H') {
            continue;
        }
        let Some(base) = cases.iter().find(|b| b.id == stem) else { continue };
        for k in kinds(base) {
            assert!(kinds(c).contains(&k), "{} lacks {k} that {stem} has", c.id);
        }
    }
}
