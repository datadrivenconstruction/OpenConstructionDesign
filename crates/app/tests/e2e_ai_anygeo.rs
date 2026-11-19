//! Any geometry through Apply: a bowed facade becomes one arc wall carrying its windows, and a
//! barrel brief becomes an extrusion roof of the model that the IFC export carries. An
//! elliptical tower is one spline wall per storey, kept or rebuilt by key on a revision.

mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_model::RoofKind;
use e2e_ai_common::*;
use e2e_common::ids;
use serde_json::{json, Value as J};

fn applied(spec: J) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    ed
}

fn openings_on(ed: &Editor, wall: i64) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE wall=?1", [wall], |r| r.get(0))
        .unwrap()
}

/// The wall's axis ends (a, b).
fn ends(ed: &Editor, wall: i64) -> ((f64, f64), (f64, f64)) {
    ed.model
        .sql()
        .query_row("SELECT ax, ay, bx, by FROM wall WHERE id=?1", [wall], |r| {
            Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
        })
        .unwrap()
}

#[test]
fn a_bowed_facade_is_one_arc_wall_per_storey_with_its_windows() {
    let floors = 2;
    let ed = applied(json!({
        "typology": "office", "floors": floors,
        "footprint": {"polygon": [[0, 0, 0.15], [60, 0], [60, 24], [0, 24]]}
    }));
    let arcs: Vec<(i64, f64)> = ids(&ed.model, "wall")
        .into_iter()
        .map(|w| (w, ed.model.wall_bulge(w).unwrap()))
        .filter(|w| w.1 != 0.0)
        .collect();
    assert_eq!(arcs.len(), floors, "one arc wall per storey: {arcs:?}");
    for (w, bulge) in &arcs {
        // the whole bow: chord 60 m, bulge 0.15 as the brief drew it
        assert!((bulge.abs() - 0.15).abs() < 1e-3, "wall {w}: bulge {bulge}");
        let (a, b) = ends(&ed, *w);
        let chord = (b.0 - a.0).hypot(b.1 - a.1);
        assert!((chord - 60.0).abs() < 1e-6, "wall {w}: chord {chord}");
        assert!(openings_on(&ed, *w) > 0, "the bowed facade keeps its windows");
    }
    // no chord of the bow is left as a straight wall with both ends on the arc's circle
    let (w0, bulge) = arcs[0];
    let (a, b) = ends(&ed, w0);
    let half = (b.0 - a.0).hypot(b.1 - a.1) / 2.0;
    let sweep = 4.0 * bulge.atan();
    let (m, d) = (((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), half / (sweep / 2.0).tan());
    let left = ((a.1 - b.1) / (2.0 * half), (b.0 - a.0) / (2.0 * half));
    let c = (m.0 + left.0 * d, m.1 + left.1 * d);
    let r = (half / (sweep / 2.0).sin()).abs();
    let on = |p: (f64, f64)| ((p.0 - c.0).hypot(p.1 - c.1) - r).abs() < 0.05;
    let chords: Vec<i64> = ids(&ed.model, "wall")
        .into_iter()
        .filter(|w| {
            let (p, q) = ends(&ed, *w);
            ed.model.wall_bulge(*w).unwrap() == 0.0 && on(p) && on(q)
        })
        .collect();
    assert!(chords.is_empty(), "chords left on the bow: {chords:?}");
}

#[test]
fn an_elliptical_tower_is_one_spline_wall_per_storey_true_to_the_ellipse() {
    let (floors, a, b) = (2usize, 25.0f64, 17.0f64);
    let mut ed = applied(json!({
        "typology": "office", "floors": floors,
        "footprint": {"shape": "ellipse", "width": 2.0 * a, "depth": 2.0 * b}
    }));
    assert_ellipse_tower(&mut ed, floors, a, b);
}

/// Generator key of every element that has one, by (table, id).
fn generator_keys(ed: &Editor) -> Vec<((String, i64), String)> {
    let mut st = ed.model.sql().prepare("SELECT kind, ref, CAST(value AS TEXT) FROM prop WHERE key=?1").unwrap();
    st.query_map([ddc_model::KEY_PROP], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn a_regenerated_tower_updates_its_spline_walls_by_key() {
    use ddc_ai::fake::{Script, ScriptedProvider};
    use ddc_ai::Mode;
    let floors = 2usize;
    let spec = json!({
        "typology": "office", "floors": floors,
        "footprint": {"shape": "ellipse", "width": 50, "depth": 34}
    });
    let done = || vec![Script::Text("done".to_string())];
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": spec}))],
        done(),
        // nothing of the facade changes: the spline walls stay as they are
        vec![tool("revise_project", json!({"changes": {"lang": "ru"}}))],
        done(),
        // the aspect changes: new spline walls on the new ellipse, nothing doubled or lost
        vec![tool("revise_project", json!({"changes": {"footprint": {"depth": 30}}}))],
        done(),
    ]);
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let mut s = start(&mut ed, cfg(provider.clone()));
    let step = |s: &mut ddc_app::ai::UiSession, ed: &mut Editor, text: &str| {
        turn(s, ed, text, Mode::Propose);
        let results = tool_results(&provider);
        let (last, err) = results.last().unwrap();
        assert!(!err, "{last}");
        apply_and_check(s, ed);
    };
    step(&mut s, &mut ed, "an elliptical tower");
    assert_ellipse_tower(&mut ed, floors, 25.0, 17.0);
    let first = ed.model.spline_walls(None).unwrap();
    let openings0 = ids(&ed.model, "opening");

    step(&mut s, &mut ed, "in Russian");
    let same = ed.model.spline_walls(None).unwrap();
    assert_eq!(same, first, "an unchanged facade keeps its spline walls");
    assert_eq!(ids(&ed.model, "opening"), openings0, "and every opening");

    step(&mut s, &mut ed, "slimmer");
    assert_ellipse_tower(&mut ed, floors, 25.0, 15.0);
    let now = ed.model.spline_walls(None).unwrap();
    assert!(now.iter().all(|x| !first.iter().any(|f| f.1.axis == x.1.axis)), "the axes are new");
    // the openings are the ones a fresh build of the new brief makes, each once, every
    // generated element keyed once
    let fresh = applied(json!({
        "typology": "office", "floors": floors, "lang": "ru",
        "footprint": {"shape": "ellipse", "width": 50, "depth": 30}
    }));
    for t in ["opening", "wall_spline", "room"] {
        assert_eq!(ids(&ed.model, t).len(), ids(&fresh.model, t).len(), "{t} after the revision");
    }
    let keys = generator_keys(&ed);
    let distinct: std::collections::BTreeSet<&String> = keys.iter().map(|k| &k.1).collect();
    assert_eq!(distinct.len(), keys.len(), "keys are unique");
    for o in ids(&ed.model, "opening") {
        assert!(keys.iter().any(|k| k.0 == ("opening".to_string(), o)), "opening {o} has its key");
    }
}

/// One spline wall per storey on the ellipse (a, b): length, enclosed area and the wall's plan
/// area within 0.5 % of the analytic values, windows on its pieces, no chord left.
fn assert_ellipse_tower(ed: &mut Editor, floors: usize, a: f64, b: f64) {
    use ddc_geom::v3;
    use ddc_geom::wall_spline::SplineAxis;
    ed.model.settle_derived().unwrap();
    let ed = &*ed;
    let splines = ed.model.spline_walls(None).unwrap();
    assert_eq!(splines.len(), floors, "one spline wall per storey");
    let levels: std::collections::BTreeSet<i64> = splines.iter().map(|s| s.1.level).collect();
    assert_eq!(levels.len(), floors);
    // Ramanujan's second perimeter, exact to far below the bound here
    let h = ((a - b) / (a + b)).powi(2);
    let perimeter = std::f64::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()));
    let area = std::f64::consts::PI * a * b;
    let near = |x: f64, want: f64| (x / want - 1.0).abs() < 0.005;
    for (id, s) in &splines {
        let ax = SplineAxis::new(s.axis.clone()).unwrap();
        assert!(near(ax.length(), perimeter), "axis {} for {perimeter}", ax.length());
        let ring = ax.offset_polyline(0.0, 1e-3);
        let n = ring.len();
        let enclosed = (0..n)
            .map(|i| ring[i].x * ring[(i + 1) % n].y - ring[(i + 1) % n].x * ring[i].y)
            .sum::<f64>()
            .abs()
            / 2.0;
        assert!(near(enclosed, area), "enclosed {enclosed} for {area}");
        // the wall's plan area: thickness times perimeter for a closed centred wall
        let t: f64 = ed
            .model
            .sql()
            .query_row("SELECT sum(thickness) FROM wall_type_layer WHERE wtype=?1", [s.wall_type], |r| r.get(0))
            .unwrap();
        let plan: f64 = ed.model.spline_wall_quantities(*id).unwrap().iter().map(|q| q.plan_area).sum();
        assert!(near(plan, t * perimeter), "wall plan area {plan} for {}", t * perimeter);
        // the windows moved onto the pieces
        let pieces = ed.model.spline_wall_pieces(*id).unwrap();
        assert!(pieces.iter().map(|&w| openings_on(&ed, w)).sum::<i64>() > 0, "the facade keeps its windows");
    }
    // no chord of the ellipse is left: a wall with both ends on an axis is one of its pieces
    for w in ids(&ed.model, "wall") {
        let (p, q) = ends(&ed, w);
        let on = |x: (f64, f64)| {
            splines.iter().any(|(_, s)| {
                let (_, c) = s.axis.closest(v3(x.0, x.1, 0.0));
                (c.x - x.0).hypot(c.y - x.1) < 0.02
            })
        };
        if on(p) && on(q) {
            assert!(ed.model.spline_of_wall(w).unwrap().is_some(), "wall {w} is a chord left on the ellipse");
        }
    }
}

#[test]
fn a_barrel_roof_is_an_extrusion_and_exports_to_ifc() {
    let ed = applied(json!({
        "typology": "office", "floors": 2,
        "footprint": {"shape": "rectangle", "width": 30, "depth": 12},
        "roof": {"kind": "barrel"}
    }));
    let roofs = ids(&ed.model, "roof");
    assert_eq!(roofs.len(), 1);
    let r = ed.model.roof(roofs[0]).unwrap();
    let RoofKind::Extrusion { a, b, profile, depth } = &r.kind else {
        panic!("a barrel roof is an extrusion: {:?}", r.kind);
    };
    let span = (b.0 - a.0).hypot(b.1 - a.1);
    assert!((span - 12.0).abs() < 1e-6 && (depth - 30.0).abs() < 1e-6, "span {span}, depth {depth}");
    // 12 m is within massing.vault_arc_span: a half circle rising half the span
    let top = profile.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    assert!((top - 6.0).abs() < 0.06, "rise {top}");
    assert!(profile.windows(2).all(|w| w[1].0 > w[0].0));
    assert!(!ed.model.roof_mesh(roofs[0]).unwrap().triangles.is_empty());
    let ifc = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    assert!(ifc.contains("IFCROOF"), "the vault is an IfcRoof");
    assert!(ifc.contains("'extrusion'"), "with its profile kind");
}

/// A tall ground level: the levels, the roof level and the IFC storeys stand at the sums of the
/// storey heights below them, not at index x floor height.
#[test]
fn per_level_storey_heights_set_the_levels_and_the_ifc_storey_elevations() {
    let ed = applied(json!({
        "typology": "office", "floors": 3, "floor_height": 4.0, "storey_heights": [8.0, 4.0, 4.0],
        "footprint": {"shape": "rectangle", "width": 30, "depth": 16}
    }));
    let mut elev: Vec<f64> = ed
        .model
        .sql()
        .prepare("SELECT DISTINCT elevation FROM level ORDER BY elevation")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    elev.retain(|z| *z >= -1e-9);
    let want = [0.0, 8.0, 12.0, 16.0];
    assert_eq!(elev.len(), want.len(), "{elev:?}");
    for (z, w) in elev.iter().zip(want) {
        assert!((z - w).abs() < 1e-6, "levels {elev:?}, want {want:?}");
    }
    let ifc = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    let storeys: Vec<f64> = ifc
        .lines()
        .filter(|l| l.contains("=IFCBUILDINGSTOREY("))
        .map(|l| l.trim_end_matches(");").rsplit(',').next().unwrap().trim_end_matches('.').parse().unwrap())
        .collect();
    let k = storeys.iter().cloned().fold(0.0, f64::max) / 16.0;
    assert!(storeys.len() >= 4, "{storeys:?}");
    for w in want {
        assert!(storeys.iter().any(|s| (s - w * k).abs() < 1e-6), "{w} m missing in {storeys:?}");
    }
}
