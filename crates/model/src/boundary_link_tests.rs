use super::*;
use crate::{LayerSpec, SlabSpec, Target, WallSpec};

struct Fx {
    m: Model,
    walls: Vec<i64>,
    slab: i64,
    lv: i64,
    wt: i64,
}

const C: [(f64, f64); 4] = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];

fn square() -> Sketch {
    Sketch {
        curves: (0..4)
            .map(|k| SketchCurve::Line {
                a: v2(C[k].0, C[k].1),
                b: v2(C[(k + 1) % 4].0, C[(k + 1) % 4].1),
            })
            .collect(),
    }
}

/// Room 6 x 4 of four walls (axes), a floor whose four edges are picked from the wall axes.
fn fx() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = |t| LayerSpec {
        material: "c".into(),
        thickness: t,
    };
    let wt = m.add_wall_type("w", &[layer(0.2)]).unwrap();
    let st = m.add_slab_type("f", &[layer(0.2)]).unwrap();
    let walls: Vec<i64> = (0..4)
        .map(|k| wall(&mut m, lv, wt, C[k], C[(k + 1) % 4]))
        .collect();
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: C.to_vec(),
            holes: vec![],
        })
        .unwrap();
    let links: Vec<EdgeLink> = walls
        .iter()
        .enumerate()
        .map(|(k, &w)| EdgeLink {
            edge: k,
            wall: w,
            face: LinkFace::Axis,
            offset: 0.0,
        })
        .collect();
    let released = m
        .finish_linked_sketch(SketchHost::Slab(slab), &square(), &links)
        .unwrap();
    assert_eq!(released, 0);
    Fx {
        m,
        walls,
        slab,
        lv,
        wt,
    }
}

fn wall(m: &mut Model, lv: i64, wt: i64, a: (f64, f64), b: (f64, f64)) -> i64 {
    m.add_wall(&WallSpec {
        a,
        b,
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    })
    .unwrap()
}

impl Fx {
    fn host(&self) -> SketchHost {
        SketchHost::Slab(self.slab)
    }
    fn has_corner(&self, x: f64, y: f64) -> bool {
        let rings = self.m.slab_rings(self.slab).unwrap();
        rings[0]
            .iter()
            .any(|p| (p.0 - x).abs() < 1e-9 && (p.1 - y).abs() < 1e-9)
    }
    fn states(&self) -> Vec<(usize, LinkState)> {
        self.m
            .boundary_links(self.host())
            .unwrap()
            .iter()
            .map(|e| (e.link.edge, e.state))
            .collect()
    }
    fn edge(&self, i: usize) -> (V2, V2) {
        match self.m.sketch_of(self.host()).unwrap().unwrap().curves[i] {
            SketchCurve::Line { a, b } => (a, b),
            _ => panic!("edge {i} is not straight"),
        }
    }
}

fn near(p: V2, x: f64, y: f64) -> bool {
    (p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9
}

#[test]
fn linked_edge_follows_wall_move() {
    let mut f = fx();
    let ops = |m: &Model| -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    };
    let before = ops(&f.m);
    f.m.move_elements(&[Target::Wall(f.walls[1])], (1.0, 0.0))
        .unwrap();
    // one user action, one journal entry: the floor followed inside the wall move
    assert_eq!(ops(&f.m), before + 1);
    assert!(f.has_corner(7.0, 0.0) && f.has_corner(7.0, 4.0));
    assert!(!f.has_corner(6.0, 0.0));
    let (a, b) = f.edge(1);
    assert!(near(a, 7.0, 0.0) && near(b, 7.0, 4.0), "{a:?} {b:?}");
    // the links (wall, face, offset) survive the rebuild
    let links = f.m.boundary_links(f.host()).unwrap();
    assert_eq!(links.len(), 4);
    assert!(links.iter().all(|e| e.state == LinkState::Linked));
    assert_eq!(links[1].link.wall, f.walls[1]);
    f.m.undo().unwrap();
    assert!(f.has_corner(6.0, 0.0) && f.has_corner(6.0, 4.0));
    assert!(!f.has_corner(7.0, 0.0));
}

#[test]
fn face_link_with_offset_follows_thickness_side() {
    let mut f = fx();
    // edge 0 (bottom, wall 0 runs +x): outer face is the right one, 0.1 from the axis
    f.m.relink_boundary_edge(
        f.host(),
        EdgeLink {
            edge: 0,
            wall: f.walls[0],
            face: LinkFace::Right,
            offset: -0.05,
        },
    )
    .unwrap();
    assert!(f.has_corner(0.0, -0.15) && f.has_corner(6.0, -0.15));
    f.m.move_elements(&[Target::Wall(f.walls[0])], (0.0, -1.0))
        .unwrap();
    assert!(f.has_corner(0.0, -1.15) && f.has_corner(6.0, -1.15));
    let l = f.m.boundary_links(f.host()).unwrap()[0];
    assert_eq!((l.link.face, l.state), (LinkFace::Right, LinkState::Linked));
    assert!((l.link.offset + 0.05).abs() < 1e-12);
}

#[test]
fn deleted_source_marks_edge_yellow() {
    let mut f = fx();
    f.m.delete_wall(f.walls[1]).unwrap();
    let st = f.states();
    assert_eq!(st[1], (1, LinkState::SourceLost));
    assert!(st
        .iter()
        .filter(|s| s.0 != 1)
        .all(|s| s.1 == LinkState::Linked));
    // the edge keeps its last place and is a plan warning on its level only
    assert!(f.has_corner(6.0, 0.0) && f.has_corner(6.0, 4.0));
    let w = f.m.boundary_warnings(f.lv).unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!((w[0].edge, w[0].state), (1, LinkState::SourceLost));
    assert!(near(w[0].a, 6.0, 0.0) && near(w[0].b, 6.0, 4.0));
    assert!(f.m.boundary_warnings(f.lv + 1).unwrap().is_empty());
    f.m.undo().unwrap();
    assert!(f.states().iter().all(|s| s.1 == LinkState::Linked));

    // lost again, then re-picked from a new wall: the edge moves onto it and follows it
    f.m.delete_wall(f.walls[1]).unwrap();
    let w = wall(&mut f.m, f.lv, f.wt, (8.0, -1.0), (8.0, 5.0));
    f.m.relink_boundary_edge(
        f.host(),
        EdgeLink {
            edge: 1,
            wall: w,
            face: LinkFace::Axis,
            offset: 0.0,
        },
    )
    .unwrap();
    assert!(f.has_corner(8.0, 0.0) && f.has_corner(8.0, 4.0));
    assert_eq!(f.states()[1], (1, LinkState::Linked));
    f.m.undo().unwrap();
    assert_eq!(f.states()[1], (1, LinkState::SourceLost));
    assert!(f.has_corner(6.0, 0.0));

    // accepted: an ordinary free edge, nothing left to accept
    assert_eq!(f.m.accept_lost_edges(f.host()).unwrap(), 1);
    assert_eq!(f.states().len(), 3);
    assert!(f.m.accept_lost_edges(f.host()).is_err());
}

#[test]
fn trim_releases_only_that_edge() {
    let mut f = fx();
    f.m.trim_boundary_edge(f.host(), 0, 1, (6.0, 0.5)).unwrap();
    let st = f.states();
    assert_eq!(st.len(), 3);
    assert!(st.iter().all(|s| s.0 != 0 && s.1 == LinkState::Linked));
    assert!(f.has_corner(6.0, 0.5));
    // the free edge keeps its line, the linked neighbour still follows its wall
    f.m.move_elements(&[Target::Wall(f.walls[1])], (1.0, 0.0))
        .unwrap();
    let (a, b) = f.edge(0);
    assert!(near(a, 0.0, 0.0) && near(b, 7.0, 0.5 * 7.0 / 6.0), "{b:?}");
    assert!(f.has_corner(7.0, 4.0));
    f.m.undo().unwrap();
    f.m.undo().unwrap();
    assert_eq!(f.states().len(), 4);
    assert!(f.has_corner(6.0, 0.0));
}

#[test]
fn edge_moved_off_its_wall_in_sketch_is_released_alone() {
    let mut f = fx();
    let mut sk = square();
    sk.curves[3] = SketchCurve::Line {
        a: v2(-0.5, 4.0),
        b: v2(-0.5, 0.0),
    };
    sk.curves[2] = SketchCurve::Line {
        a: v2(6.0, 4.0),
        b: v2(-0.5, 4.0),
    };
    sk.curves[0] = SketchCurve::Line {
        a: v2(-0.5, 0.0),
        b: v2(6.0, 0.0),
    };
    let released = f.m.finish_linked_sketch(f.host(), &sk, &[]).unwrap();
    assert_eq!(released, 1);
    let st = f.states();
    assert_eq!(
        st.iter().map(|s| s.0).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    // plain «Done» (no new picks) keeps the existing links too
    f.m.finish_sketch(f.host(), &sk).unwrap();
    assert_eq!(f.states().len(), 3);
    // «Unlink» while sketching: only the named edge goes, in the same «Done»
    let n = f.m.finish_sketch_links(f.host(), &sk, &[], &[2]).unwrap();
    assert_eq!(n, 1);
    assert_eq!(f.states().iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 1]);
    f.m.undo().unwrap();
    assert_eq!(f.states().len(), 3);
}

#[test]
fn unlink_detaches_and_wall_move_leaves_edge() {
    let mut f = fx();
    assert_eq!(f.m.unlink_boundary_edges(f.host(), &[1]).unwrap(), 1);
    assert!(f.m.unlink_boundary_edges(f.host(), &[1]).is_err());
    f.m.move_elements(&[Target::Wall(f.walls[1])], (1.0, 0.0))
        .unwrap();
    assert!(f.has_corner(6.0, 0.0) && !f.has_corner(7.0, 0.0));
    assert_eq!(f.m.unlink_boundary_edges(f.host(), &[]).unwrap(), 3);
    assert!(f.states().is_empty());
}

#[test]
fn deleted_host_drops_links_and_undo_returns_them() {
    let mut f = fx();
    f.m.delete_slab(f.slab).unwrap();
    let n: i64 = f
        .m
        .sql()
        .query_row("SELECT count(*) FROM boundary_link", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    f.m.undo().unwrap();
    assert_eq!(f.states().len(), 4);
}

#[test]
fn rebuild_is_pure_and_refuses_flips() {
    let sk = square();
    let mut lines = vec![None; 4];
    lines[1] = Some((v2(9.0, 0.0), v2(0.0, 1.0)));
    let out = rebuild(&sk, &lines).unwrap();
    assert_eq!(
        out.curves[0],
        SketchCurve::Line {
            a: v2(0.0, 0.0),
            b: v2(9.0, 0.0)
        }
    );
    // the linked edge crosses its free neighbour's far end: refused, nothing half-moved
    lines[1] = Some((v2(-1.0, 0.0), v2(0.0, 1.0)));
    assert!(rebuild(&sk, &lines).is_none());
    // no links: unchanged
    assert_eq!(rebuild(&sk, &[None; 4]).unwrap().curves, sk.curves);
}
