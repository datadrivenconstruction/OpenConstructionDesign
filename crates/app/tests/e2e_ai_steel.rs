//! `connect_members` through the chat: a steel gazebo gets every node in one request, applied
//! in one undo step; named members give one connection; a bad request is refused. A truss hall
//! generated as a hangar applies with its outfit.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_geom::member::Shape;
use ddc_model::{BeamJoin, BeamSpec, ColumnSpec};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

/// 3 x 3 m gazebo: pipe columns, IPE ring beams, pipe rafters to an apex ball.
fn gazebo() -> (Editor, i64, Vec<i64>, Vec<i64>) {
    let mut ed = Editor::new_in_memory().unwrap();
    let l0 = add_level(&mut ed, "L0", 0.0);
    let m = &mut ed.model;
    let pipe = |d, t| Shape::Pipe { d, t };
    let col_t = m.add_member_type("CHS 168x8", &pipe(0.1683, 0.008), "S355").unwrap();
    let ipe = Shape::I { h: 0.2, b: 0.1, tw: 0.0056, tf: 0.0085 };
    let ring_t = m.add_member_type("IPE 200", &ipe, "S355").unwrap();
    let raf_t = m.add_member_type("CHS 76x4", &pipe(0.0761, 0.004), "S355").unwrap();
    let corners = [(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)];
    let cs: Vec<ColumnSpec> = corners
        .iter()
        .map(|&at| ColumnSpec {
            level: l0,
            top_level: None,
            member_type: col_t,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 3.2,
            rotation: 0.0,
            top_at: None,
        })
        .collect();
    let cols = m.add_columns(&cs).unwrap();
    let beam = |t: i64, a: (f64, f64, f64), b: (f64, f64, f64)| BeamSpec {
        level: l0,
        member_type: t,
        a,
        b,
        bulge: 0.0,
        yjust: Default::default(),
        zjust: Default::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::None,
        join_end: BeamJoin::None,
    };
    let mut bs = vec![];
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        bs.push(beam(ring_t, (a.0, a.1, 2.4), (b.0, b.1, 2.4)));
    }
    for c in corners {
        bs.push(beam(raf_t, (c.0, c.1, 2.8), (1.5, 1.5, 4.0)));
    }
    let beams = m.add_beams(&bs).unwrap();
    ed.reload().unwrap();
    (ed, l0, cols, beams)
}

#[test]
fn every_gazebo_node_is_placed_in_one_request() {
    let (mut ed, l0, _, _) = gazebo();
    let (mut s, p) = run(
        &mut ed,
        vec![tool(
            "connect_members",
            json!({"title": "Gazebo nodes", "type": "auto", "level": l0}),
        )],
    );
    let results = tool_results(&p);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert!(results[0].0.contains("not_checked"), "{}", results[0].0);
    assert_eq!(count(&ed.model, "steel_conn"), 0, "nothing before Apply");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "steel_conn"), 17);
    let conns = ed.model.steel_conns().unwrap();
    for (id, _) in &conns {
        let r = ed.model.steel_conn_report(*id).unwrap();
        assert!(r.error.is_none(), "{id}: {:?}", r.error);
        assert!(r.findings.iter().all(|f| f.rule.key() != "clash"), "{:?}", r.findings);
    }
    assert!(ed.model.steel_schedule(None).unwrap().bolt_count() > 0);
}

#[test]
fn named_members_give_one_connection_and_bad_ones_are_refused() {
    let (mut ed, _, cols, beams) = gazebo();
    let (mut s, p) = run(
        &mut ed,
        vec![
            tool(
                "connect_members",
                json!({
                    "title": "Fin plate",
                    "type": "pinned",
                    "members": [format!("column:{}", cols[0]), format!("beam:{}", beams[0])],
                    "params": {"bolt": 16, "class": "8.8"}
                }),
            ),
            tool(
                "connect_members",
                json!({"title": "Wrong", "type": "pinned", "members": ["wall:1", "beam:2"]}),
            ),
            tool("connect_members", json!({"title": "Wrong", "type": "weld_all"})),
            tool(
                "connect_members",
                json!({"title": "Wrong", "type": "base_plate", "params": {"bolt": "huge"}}),
            ),
        ],
    );
    let r = tool_results(&p);
    assert_eq!(r.iter().map(|x| x.1).collect::<Vec<_>>(), [false, true, true, true], "{r:?}");
    apply_and_check(&mut s, &mut ed);
    let conns = ed.model.steel_conns().unwrap();
    assert_eq!(conns.len(), 1);
    let spec = &conns[0].1;
    assert_eq!(spec.kind.key(), "pinned");
    assert_eq!(spec.params.bolt, Some(0.016));
}

/// H16 through the tool path: run_generator builds the hangar, Apply puts its outfit in the model.
#[test]
fn a_hangar_is_generated_with_its_outfit_and_applied() {
    let mut ed = Editor::new_in_memory().unwrap();
    add_level(&mut ed, "L0", 0.0);
    let levels = count(&ed.model, "level");
    let params = json!({"kind": "truss_hall", "span": 80, "height": 20, "bays": 10, "spacing": 10, "outfit": "hangar"});
    let (mut s, p) = run(
        &mut ed,
        vec![tool("run_generator", json!({"id": "steel_frame", "params": params, "variants": 1}))],
    );
    let r = tool_results(&p);
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    assert_eq!(count(&ed.model, "roof"), 0, "nothing before Apply");
    apply_and_check(&mut s, &mut ed);
    assert!(count(&ed.model, "beam") + count(&ed.model, "brace") >= 300);
    assert_eq!(count(&ed.model, "roof"), 1, "the roof cladding");
    assert_eq!(count(&ed.model, "opening"), 1, "the hangar door");
    let w: f64 = ed.model.sql().query_row("SELECT width FROM opening", [], |r| r.get(0)).unwrap();
    assert!(w >= 15.0, "{w}");
    let runway: i64 = ed
        .model
        .sql()
        .query_row("SELECT count(*) FROM beam WHERE abs(az - 16.0) < 1e-6 AND abs(bz - 16.0) < 1e-6", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runway, 42, "22 corbels and 20 runway beams at the crane height");
    assert_eq!(count(&ed.model, "room"), 26, "workshops, offices and stair halls");
    assert_eq!(count(&ed.model, "level"), levels + 1, "the annex's upper storey");
    assert_eq!(count(&ed.model, "stair"), 1);
    let cranes = "SELECT count(*) FROM comp c JOIN comp_type t ON t.id = c.ctype JOIN comp_family f ON f.id = t.family WHERE f.name = 'Overhead Crane'";
    let n: i64 = ed.model.sql().query_row(cranes, [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "the overhead crane on the runway");
}
