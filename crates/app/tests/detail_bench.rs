//! Bench model for detailed element geometry: a storey of rooms with a door and a window in
//! every wall, a stair with its railings and a guard. Per level of detail the 3D scene is
//! composed and measured: time, triangles, shared opening prototypes; no door leaf, railing
//! or stair is a lone block.

use ddc_app::edit3d::scene3d;
use ddc_app::editor::Editor;
use ddc_geom::detail::{cache, DOOR_BUDGET, WINDOW_BUDGET};
use ddc_model::{
    DetailLevel, OpeningSpec, RailingHost, RailingSpec, StairSpec, Swing, ViewGfx, ViewKey, WallSpec,
};
use ddc_render::{TAG_DOOR, TAG_GLASS};
use std::time::Instant;

const CELL: f64 = 6.0;
const K: usize = 6;

fn bench() -> (Editor, usize, usize) {
    let mut ed = Editor::new_in_memory().unwrap();
    let (level, wt) = (ed.level, ed.types[0].id);
    let top = ed.model.add_level("L2", 3.5).unwrap();
    let w = |a: (f64, f64), b: (f64, f64)| WallSpec { a, b, level, base_offset: 0.0, height: 3.5, wall_type: wt, offset0: None };
    let mut ws = vec![];
    for i in 0..=K {
        let c = i as f64 * CELL;
        for j in 0..K {
            let (a, b) = (j as f64 * CELL, (j + 1) as f64 * CELL);
            ws.push(w((a, c), (b, c)));
            ws.push(w((c, a), (c, b)));
        }
    }
    let walls = ed.model.add_walls(&ws).unwrap();
    let mut os = vec![];
    for (i, &id) in walls.iter().enumerate() {
        let door = OpeningSpec { along: 0.8, width: if i % 7 == 0 { 1.8 } else { 0.9 }, sill: 0.0, height: 2.1, kind: "door".into() };
        let window = OpeningSpec { along: 3.2, width: 1.5, sill: 0.9, height: 1.5, kind: "window".into() };
        os.push((id, door, Swing { side: i % 2 == 0, hinge: i % 3 == 0 }));
        os.push((id, window, Swing::default()));
    }
    let n = os.len();
    ed.model.add_openings_with(&os).unwrap();
    let mut s = StairSpec::straight(level, top, (1.5, 1.0), std::f64::consts::FRAC_PI_2);
    s.width = 1.2;
    let stair = ed.model.add_stair(&s).unwrap();
    ed.model.add_stair_railings(stair, &RailingSpec::new(level, RailingHost::Stair { stair, side: 0 })).unwrap();
    let guard = RailingHost::Path(vec![(0.5, 8.0, 0.0), (5.5, 8.0, 0.0), (5.5, 11.0, 0.0)]);
    ed.model.add_railing(&RailingSpec { height: 1.1, ..RailingSpec::new(level, guard) }).unwrap();
    ed.reload().unwrap();
    (ed, walls.len(), n)
}

fn set_lod(ed: &mut Editor, d: DetailLevel) {
    ed.model.set_view_gfx(ViewKey::ThreeD, &ViewGfx { detail: d, ..Default::default() }).unwrap();
    ed.reload().unwrap();
}

#[test]
fn detailed_storey_composes_within_budget_at_every_level_of_detail() {
    let (mut ed, walls, openings) = bench();
    let mut last = 0usize;
    let mut rows = vec![];
    for (d, lod) in [
        (DetailLevel::Coarse, ddc_geom::detail::Lod::Coarse),
        (DetailLevel::Medium, ddc_geom::detail::Lod::Medium),
        (DetailLevel::Fine, ddc_geom::detail::Lod::Fine),
    ] {
        set_lod(&mut ed, d);
        let built0 = cache::stats().0;
        let t = Instant::now();
        let (sc, _) = scene3d::compose(&mut ed);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let built = cache::stats().0 - built0;
        let tris: usize = sc.meshes.iter().map(|m| m.triangles.len()).sum();
        let door_tris: usize = sc.meshes.iter().filter(|m| m.tag == TAG_DOOR).map(|m| m.triangles.len()).sum();
        rows.push(format!("{d:?}: {ms:.0} ms, {tris} triangles, {built} prototypes built for {openings} openings"));
        // a hundred-odd openings of three types and two swings build a handful of prototypes
        assert!(built <= 16, "{d:?}: {built} prototypes");
        assert!(tris > last, "{d:?} adds no detail");
        last = tris;
        // every opening's fill within its budget: doors (≤ 2 leaves) and windows (2 fields)
        let budget = walls * (2 * DOOR_BUDGET.at(lod) + 2 * WINDOW_BUDGET.at(lod)) + 400_000;
        assert!(tris <= budget, "{d:?}: {tris} > {budget}");
        assert!(door_tris > 0);
        // no door leaf and no glazing is a single block per element
        for m in sc.meshes.iter().filter(|m| m.tag == TAG_DOOR) {
            assert!(m.positions.len() > 8, "{d:?}: a door leaf of {} vertices", m.positions.len());
        }
        assert!(sc.meshes.iter().any(|m| m.tag == TAG_GLASS));
        assert!(ms < 20_000.0, "{d:?}: {ms:.0} ms");
    }
    eprintln!("detail bench, {walls} walls:\n  {}", rows.join("\n  "));
}

#[test]
fn railings_and_stairs_are_not_blocks() {
    let (mut ed, _, _) = bench();
    set_lod(&mut ed, DetailLevel::Medium);
    let (sc, targets) = scene3d::compose(&mut ed);
    let mut seen = 0;
    for m in &sc.meshes {
        let Some(cat) = scene3d::category_of(m.element, &targets) else { continue };
        if cat == "railing" || cat == "stair" {
            seen += 1;
            assert!(m.positions.len() > 8 * 20, "{cat}: {} vertices", m.positions.len());
            // round handrails and balusters: many more corners than a set of blocks
            if cat == "railing" {
                assert!(m.triangles.len() > 12 * 30);
            }
        }
    }
    assert!(seen >= 3, "stair, its railing and the guard: {seen}");
}

/// The T01 tower (lattice_tower_300 at detail 3, 10 000+ members with their plates and bolts)
/// composes its 3D scene within budget (s): a debug build is held to the looser one.
#[test]
#[ignore = "heavy: cargo test -p ddc-app --test detail_bench -- --ignored t01"]
fn t01_tower_scene_composes_within_budget() {
    let g = ddc_gen::registry().iter().find(|g| g.id() == "steel_frame").unwrap();
    let p = serde_json::json!({"kind": "lattice_tower_300", "pattern": "x", "tower_height": 300, "base_width": 125,
        "platform_1": 57, "platform_2": 115, "platform_3": 276, "detail": 3});
    let ddc_gen::GenOut::Steel { frame, .. } = g.build(&p, 1).unwrap() else { panic!("not steel") };
    let mut ed = Editor::new_in_memory().unwrap();
    let ids = ed.model.add_steel_frame(ed.level, &frame).unwrap();
    let conns = ed.model.connect_steel_frame(&ids).unwrap().len();
    ed.reload().unwrap();
    let t = Instant::now();
    let (sc, _) = scene3d::compose(&mut ed);
    let s = t.elapsed().as_secs_f64();
    let tris: usize = sc.meshes.iter().map(|m| m.triangles.len()).sum();
    eprintln!("t01 scene: {conns} joints, {} meshes, {tris} triangles, {s:.2} s", sc.meshes.len());
    assert!(conns > 0 && tris > 0);
    let budget = if cfg!(debug_assertions) { 300.0 } else { 30.0 };
    assert!(s <= budget, "scene took {s:.2} s, budget {budget} s");
}
