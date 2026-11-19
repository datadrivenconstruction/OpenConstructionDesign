use super::*;
use crate::{LayerSpec, OpeningSpec};

/// S-curve through four plan points, one layer 0.3 centred, h = 3.
fn s_wall(m: &mut Model) -> (i64, SplineWallSpec) {
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("c", &[LayerSpec { material: "concrete".into(), thickness: 0.3 }])
        .unwrap();
    let s = SplineWallSpec::through(lv, t, &[(0., 0.), (4., 1.), (8., -1.), (12., 0.)], 3.0).unwrap();
    (m.add_spline_wall(&s).unwrap(), s)
}

fn count(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn ends(m: &Model, w: i64) -> (V2, V2) {
    m.sql()
        .query_row("SELECT ax, ay, bx, by FROM wall WHERE id=?1", [w], |r| {
            Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?)))
        })
        .unwrap()
}

#[test]
fn spline_wall_is_a_joined_chain_with_exact_quantities() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, s) = s_wall(&mut m);
    let ps = m.spline_wall_pieces(id).unwrap();
    assert!(ps.len() >= 2, "{}", ps.len());
    for w in ps.windows(2) {
        assert!((ends(&m, w[0]).1 - ends(&m, w[1]).0).len() < 1e-12);
    }
    assert!((ends(&m, ps[0]).0 - v2(0., 0.)).len() < 1e-9);
    assert!((ends(&m, *ps.last().unwrap()).1 - v2(12., 0.)).len() < 1e-9);
    for &w in &ps {
        assert_eq!(m.spline_of_wall(w).unwrap(), Some(id));
    }
    // inner ends only are kept from joining; the chain's joins all succeed
    assert_eq!(count(&m, "SELECT count(*) FROM wall_join WHERE allowed=0"), 2 * (ps.len() as i64 - 1));
    assert!(m.wall_warnings().unwrap().is_empty(), "{:?}", m.wall_warnings().unwrap());
    // exact: the spline's own layer area; the arc chain is within tolerance of it
    let ax = SplineAxis::new(s.axis.clone()).unwrap();
    let exact = ax.layer_area(-0.15, 0.15);
    let q = m.spline_wall_quantities(id).unwrap();
    assert_eq!(q.len(), 1);
    assert!((q[0].plan_area - exact).abs() < 1e-9, "{} {exact}", q[0].plan_area);
    assert!((q[0].volume - exact * 3.0).abs() < 1e-8);
    let chain: f64 = count_area(&m);
    assert!((chain - exact).abs() < 2.0 * s.tol * ax.length() * 0.3, "{chain} {exact}");
    assert_eq!(m.spline_walls(None).unwrap(), vec![(id, s)]);
}

fn count_area(m: &Model) -> f64 {
    m.sql().query_row("SELECT SUM(plan_area) FROM wall_qty", [], |r| r.get(0)).unwrap()
}

/// The same wall on the level and type `s_wall` made.
fn s_wall_again(m: &mut Model) -> (i64, SplineWallSpec) {
    let lv: i64 = count(m, "SELECT min(id) FROM level");
    let t: i64 = count(m, "SELECT min(id) FROM wall_type");
    let s = SplineWallSpec::through(lv, t, &[(0., 0.), (4., 1.), (8., -1.), (12., 0.)], 3.0).unwrap();
    (m.add_spline_wall(&s).unwrap(), s)
}

#[test]
fn add_and_delete_are_one_undo_step_each() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, _) = s_wall(&mut m);
    let n = m.spline_wall_pieces(id).unwrap().len() as i64;
    m.undo().unwrap();
    assert_eq!(count(&m, "SELECT count(*) FROM wall"), 0);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_spline"), 0);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_spline_piece"), 0);
    let (id, _) = s_wall_again(&mut m);
    assert_eq!(count(&m, "SELECT count(*) FROM wall"), n);
    m.delete_spline_wall(id).unwrap();
    assert_eq!(count(&m, "SELECT count(*) FROM wall") + count(&m, "SELECT count(*) FROM wall_join"), 0);
    m.undo().unwrap();
    assert_eq!(count(&m, "SELECT count(*) FROM wall"), n);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_join"), 2 * (n - 1));
    assert_eq!(m.spline_wall_pieces(id).unwrap().len() as i64, n);
}

#[test]
fn update_moves_the_pieces_and_keeps_openings_along_the_axis() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, s) = s_wall(&mut m);
    let ps = m.spline_wall_pieces(id).unwrap();
    let lens: Vec<f64> = ps.iter().map(|&w| m.wall_length(w).unwrap()).collect();
    let k = (0..ps.len()).max_by(|&a, &b| lens[a].total_cmp(&lens[b])).unwrap();
    assert!(lens[k] > 1.0, "{lens:?}");
    let along = (lens[k] - 0.9) / 2.0;
    let before: f64 = lens[..k].iter().sum::<f64>() + along;
    let o = m
        .add_opening(ps[k], &OpeningSpec { along, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() })
        .unwrap();
    let vol0 = m.spline_wall_quantities(id).unwrap()[0].volume;
    // moved 1 m east and raised to 3.5 m: same chain, opening at the same distance
    let c = &s.axis;
    let ctrl = c.ctrl().iter().map(|p| v3(p.x + 1.0, p.y, 0.0)).collect();
    let s2 = SplineWallSpec {
        axis: Curve::new(c.degree(), c.knots().to_vec(), ctrl, c.weights().to_vec()).unwrap(),
        height: 3.5,
        ..s.clone()
    };
    m.update_spline_wall(id, &s2).unwrap();
    assert_eq!(m.spline_wall_pieces(id).unwrap(), ps);
    assert!((ends(&m, ps[0]).0 - v2(1., 0.)).len() < 1e-9);
    let (host, a): (i64, f64) =
        m.sql().query_row("SELECT wall, along FROM opening WHERE id=?1", [o], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let pos = ps.iter().position(|&w| w == host).unwrap();
    let after: f64 = ps[..pos].iter().map(|&w| m.wall_length(w).unwrap()).sum::<f64>() + a;
    assert!((after - before).abs() < 1e-9, "{after} {before}");
    let vol1 = m.spline_wall_quantities(id).unwrap()[0].volume;
    assert!((vol1 - (vol0 + 0.5 * (vol0 + 0.9 * 2.1 * 0.3) / 3.0)).abs() < 1e-8, "{vol0} {vol1}");
    assert_eq!(m.spline_wall(id).unwrap(), s2);
    m.undo().unwrap();
    assert_eq!(m.spline_wall(id).unwrap(), s);
    assert!((ends(&m, ps[0]).0 - v2(0., 0.)).len() < 1e-9);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_join WHERE allowed=0"), 2 * (ps.len() as i64 - 1));
    assert_eq!(count(&m, "SELECT count(*) FROM opening"), 1);
}

#[test]
fn a_looser_tolerance_takes_fewer_pieces_and_drops_the_rest() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, s) = s_wall(&mut m);
    let n = m.spline_wall_pieces(id).unwrap().len();
    m.update_spline_wall(id, &SplineWallSpec { tol: 0.05, ..s.clone() }).unwrap();
    let n2 = m.spline_wall_pieces(id).unwrap().len();
    assert!(n2 < n, "{n2} {n}");
    assert_eq!(count(&m, "SELECT count(*) FROM wall") as usize, n2);
    assert!(m.wall_warnings().unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(count(&m, "SELECT count(*) FROM wall") as usize, n);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_spline_piece") as usize, n);
}

#[test]
fn deleting_a_piece_takes_it_out_of_the_chain() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, _) = s_wall(&mut m);
    let ps = m.spline_wall_pieces(id).unwrap();
    m.delete_walls(&[ps[0]]).unwrap();
    assert_eq!(m.spline_of_wall(ps[0]).unwrap(), None);
    assert_eq!(m.spline_wall_pieces(id).unwrap(), ps[1..].to_vec());
    m.undo().unwrap();
    assert_eq!(m.spline_wall_pieces(id).unwrap(), ps);
}

#[test]
fn bad_spline_walls_are_refused() {
    let mut m = Model::open(":memory:").unwrap();
    let (_, s) = s_wall(&mut m);
    // a 4 m thick wall folds on the bends
    let t = m.add_wall_type("thick", &[LayerSpec { material: "c".into(), thickness: 4.0 }]).unwrap();
    assert!(m.add_spline_wall(&SplineWallSpec { wall_type: t, ..s.clone() }).is_err());
    assert!(m.add_spline_wall(&SplineWallSpec { tol: 1.0, ..s.clone() }).is_err());
    assert!(m.add_spline_wall(&SplineWallSpec { height: 0.0, ..s.clone() }).is_err());
    assert!(m.add_spline_wall(&SplineWallSpec { level: 999, ..s }).is_err());
    assert_eq!(m.spline_walls(None).unwrap().len(), 1);
}

/// Rows of the chain, its joins and the axis, as text: an undo must give them back exactly.
fn chain_state(m: &Model) -> String {
    m.sql()
        .query_row(
            "SELECT (SELECT group_concat(v, ';') FROM (SELECT id||' '||ax||' '||ay||' '||bx||' '||by||' '||bulge||' '||wtype AS v \
                FROM wall ORDER BY id)) \
             ||'|'||(SELECT group_concat(v, ';') FROM (SELECT wall||' '||end_||' '||allowed AS v FROM wall_join ORDER BY wall, end_)) \
             ||'|'||(SELECT group_concat(v, ';') FROM (SELECT wall||' '||spline||' '||pos AS v FROM wall_spline_piece ORDER BY wall)) \
             ||'|'||(SELECT group_concat(v, ';') FROM (SELECT id||' '||axis AS v FROM wall_spline ORDER BY id)) \
             ||'|'||COALESCE((SELECT group_concat(v, ';') FROM (SELECT id||' '||wall||' '||along AS v FROM opening ORDER BY id)), '')",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn single_pieces_cannot_be_edited_directly() {
    use ddc_geom::wall_edit::EndPrefs;
    let mut m = Model::open(":memory:").unwrap();
    let (id, _) = s_wall(&mut m);
    let ps = m.spline_wall_pieces(id).unwrap();
    let (w, last) = (ps[0], *ps.last().unwrap());
    let t2 = m.add_wall_type("d", &[LayerSpec { material: "brick".into(), thickness: 0.2 }]).unwrap();
    let before = chain_state(&m);
    let (a, b) = ends(&m, w);
    let refused = |r: Result<(), Error>| matches!(r, Err(Error::Invalid(ref e)) if e.contains("spline wall"));
    assert!(refused(m.set_wall_bulge(w, 0.1)));
    assert!(refused(m.set_wall_axis(w, (a.x, a.y), (b.x + 1.0, b.y))));
    assert!(refused(m.flip_wall(w)));
    assert!(refused(m.set_wall_type(w, t2)));
    assert!(refused(m.set_wall_top_profile(w, Some(&[(0.0, 3.0), (0.5, 2.0)]))));
    assert!(refused(m.set_wall_lean(w, crate::Lean::new(0.05, 1.0))));
    assert!(m.split_wall(w, 0.5).is_err());
    assert!(m.offset_wall(w, 1.0, true).is_err());
    assert!(m.split_wall_at(w, ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)).is_err());
    // a piece alone does not move, turn or copy: the spline wall goes as a whole
    assert!(m.move_elements(&[crate::Target::Wall(w)], (1.0, 0.0)).is_err());
    assert!(m.copy_elements(&[crate::Target::Wall(last)], &[(0.0, 5.0)]).is_err());
    // the inner ends stay unjoined; the two outer ends join like any wall end
    assert!(refused(m.set_wall_join(w, 1, EndPrefs::default())));
    assert!(refused(m.set_wall_join(last, 0, EndPrefs::default())));
    assert_eq!(chain_state(&m), before);
    m.set_wall_join(w, 0, EndPrefs::default()).unwrap();
    m.undo().unwrap();
    // a bulk write passes a piece row back unchanged and refuses a change
    let edit = |m: &Model, w: i64, dh: f64| {
        let (a, b) = ends(m, w);
        let (level, wall_type, base_offset, height, offset0, bulge): (i64, i64, f64, f64, Option<f64>, f64) = m
            .sql()
            .query_row("SELECT level, wtype, base_offset, height, offset0, bulge FROM wall WHERE id=?1", [w], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .unwrap();
        let spec =
            crate::WallSpec { a: (a.x, a.y), b: (b.x, b.y), level, base_offset, height: height + dh, wall_type, offset0 };
        crate::Edit::Wall { id: w, spec, bulge }
    };
    m.apply_edits(&[edit(&m, w, 0.0)]).unwrap();
    assert!(refused(m.apply_edits(&[edit(&m, w, 0.5)])));
    assert!(refused(m.apply_edits(&[crate::Edit::WallShape { id: w, lean: 0.05, top_scale: 1.0 }])));
    assert_eq!(chain_state(&m), before);
}

#[test]
fn whole_spline_walls_move_turn_and_mirror_with_their_axis() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, s) = s_wall(&mut m);
    let ps = m.spline_wall_pieces(id).unwrap();
    let lens: Vec<f64> = ps.iter().map(|&w| m.wall_length(w).unwrap()).collect();
    let total: f64 = lens.iter().sum();
    let k = (0..ps.len()).max_by(|&a, &b| lens[a].total_cmp(&lens[b])).unwrap();
    let along = (lens[k] - 0.9) / 2.0;
    let dist: f64 = lens[..k].iter().sum::<f64>() + along;
    m.add_opening(ps[k], &OpeningSpec { along, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() }).unwrap();
    let ts: Vec<crate::Target> = ps.iter().map(|&w| crate::Target::Wall(w)).collect();
    let base = chain_state(&m);
    let first = |m: &Model| ends(m, m.spline_wall_pieces(id).unwrap()[0]).0;
    let last = |m: &Model| ends(m, *m.spline_wall_pieces(id).unwrap().last().unwrap()).1;
    let door = |m: &Model| -> f64 {
        let ps = m.spline_wall_pieces(id).unwrap();
        let (host, a): (i64, f64) =
            m.sql().query_row("SELECT wall, along FROM opening", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let pos = ps.iter().position(|&w| w == host).unwrap();
        ps[..pos].iter().map(|&w| m.wall_length(w).unwrap()).sum::<f64>() + a
    };
    m.move_elements(&ts, (2.0, 3.0)).unwrap();
    assert!((first(&m) - v2(2., 3.)).len() < 1e-9 && (last(&m) - v2(14., 3.)).len() < 1e-9);
    let c = m.spline_wall(id).unwrap().axis;
    assert!((c.point(c.knots()[0]) - v3(2., 3., 0.)).len() < 1e-9);
    assert!((door(&m) - dist).abs() < 1e-9);
    m.undo().unwrap();
    assert_eq!(chain_state(&m), base);
    // a quarter turn about the origin
    m.rotate_elements(&ts, (0.0, 0.0), std::f64::consts::FRAC_PI_2, false).unwrap();
    assert!((last(&m) - v2(0., 12.)).len() < 1e-9);
    let n = m.spline_wall_pieces(id).unwrap().len() as i64;
    assert_eq!(count(&m, "SELECT count(*) FROM wall_join WHERE allowed=0"), 2 * (n - 1));
    m.undo().unwrap();
    assert_eq!(chain_state(&m), base);
    // a mirror about y = -5 reverses the axis; the door keeps its place on the wall
    m.mirror_elements(&ts, (0.0, -5.0), (1.0, -5.0), false).unwrap();
    assert!((first(&m) - v2(12., -10.)).len() < 1e-9 && (last(&m) - v2(0., -10.)).len() < 1e-9);
    let c = m.spline_wall(id).unwrap().axis;
    assert!((c.point(c.knots()[0]) - v3(12., -10., 0.)).len() < 1e-9);
    assert!((door(&m) - (total - dist - 0.9)).abs() < 1e-6, "{} {}", door(&m), total - dist - 0.9);
    assert!(m.wall_warnings().unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(chain_state(&m), base);
    assert_eq!(m.spline_wall(id).unwrap(), s);
}

#[test]
fn spline_walls_copy_and_paste_as_a_whole() {
    let mut m = Model::open(":memory:").unwrap();
    let (id, s) = s_wall(&mut m);
    let ps = m.spline_wall_pieces(id).unwrap();
    let n = ps.len();
    let ts: Vec<crate::Target> = ps.iter().map(|&w| crate::Target::Wall(w)).collect();
    let base = chain_state(&m);
    let out = m.copy_elements(&ts, &[(0.0, 5.0)]).unwrap();
    let all = m.spline_walls(None).unwrap();
    assert_eq!(all.len(), 2);
    let (cid, cs) = all.iter().find(|x| x.0 != id).unwrap();
    let cp = m.spline_wall_pieces(*cid).unwrap();
    assert_eq!(cp.len(), n);
    assert!(cp.iter().all(|w| out.contains(&crate::Target::Wall(*w))));
    assert!((cs.axis.point(cs.axis.knots()[0]) - v3(0., 5., 0.)).len() < 1e-9);
    assert_eq!(cs.wall_type, s.wall_type);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_join WHERE allowed=0"), 4 * (n as i64 - 1));
    m.undo().unwrap();
    assert_eq!(chain_state(&m), base);
    // the clipboard takes the spline wall and every piece for one selected piece
    let clip = m.clip_copy(&[("wall", ps[1])]).unwrap();
    let rep = m.clip_paste(&clip, &crate::PasteOpts { dx: 0.0, dy: -6.0, level: crate::PlaceLevel::Same }).unwrap();
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    assert_eq!(count(&m, "SELECT count(*) FROM wall"), 2 * n as i64);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_spline_piece"), 2 * n as i64);
    let all = m.spline_walls(None).unwrap();
    let (pid, psp) = all.iter().find(|x| x.0 != id).unwrap();
    assert!((psp.axis.point(psp.axis.knots()[0]) - v3(0., -6., 0.)).len() < 1e-9);
    let pp = m.spline_wall_pieces(*pid).unwrap();
    assert_eq!(pp.len(), n);
    assert!((ends(&m, pp[0]).0 - v2(0., -6.)).len() < 1e-9);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_join WHERE allowed=0"), 4 * (n as i64 - 1));
    m.undo().unwrap();
    assert_eq!(chain_state(&m), base);
}

#[test]
fn chords_become_one_spline_wall_with_their_openings_moved_by_curve_length() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("c", &[LayerSpec { material: "concrete".into(), thickness: 0.3 }])
        .unwrap();
    // 24 chords of a gentle wave, a window on the 13th
    let pts: Vec<(f64, f64)> = (0..=24).map(|i| (i as f64 * 0.5, (i as f64 * 0.25).sin())).collect();
    let specs: Vec<crate::WallSpec> = pts
        .windows(2)
        .map(|w| crate::WallSpec { a: w[0], b: w[1], level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None })
        .collect();
    let walls = m.add_walls(&specs).unwrap();
    let chord = |k: usize| (pts[k + 1].0 - pts[k].0).hypot(pts[k + 1].1 - pts[k].1);
    let o = m
        .add_opening(walls[12], &OpeningSpec { along: 0.05, width: 0.4, sill: 0.9, height: 1.2, kind: "window".into() })
        .unwrap();
    let mid_chain = (0..12).map(chord).sum::<f64>() + 0.25;
    let total: f64 = (0..24).map(chord).sum();
    let s = SplineWallSpec::through(lv, t, &pts, 3.0).unwrap();
    let id = m.spline_from_walls(&walls, &s).unwrap();
    let ps = m.spline_wall_pieces(id).unwrap();
    assert!(!ps.is_empty() && ps.iter().zip(&walls).all(|(p, w)| p == w), "the chords' rows are the pieces");
    assert_eq!(count(&m, "SELECT count(*) FROM wall"), ps.len() as i64);
    assert!((ends(&m, ps[0]).0 - v2(0., 0.)).len() < 1e-9);
    // the window's centre sits at the same share of the curve's length
    let (host, along): (i64, f64) =
        m.sql().query_row("SELECT wall, along FROM opening WHERE id=?1", [o], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let k = ps.iter().position(|&w| w == host).expect("hosted by a piece");
    let len = |w: i64| {
        let (a, b) = ends(&m, w);
        let bulge: f64 = m.sql().query_row("SELECT bulge FROM wall WHERE id=?1", [w], |r| r.get(0)).unwrap();
        ddc_geom::arc::axis_length(a, b, bulge)
    };
    let at = ps[..k].iter().map(|&w| len(w)).sum::<f64>() + along + 0.2;
    let axis = SplineAxis::new(s.axis.clone()).unwrap().length();
    assert!((at / axis - mid_chain / total).abs() < 1e-3, "{at} of {axis}, chain {mid_chain} of {total}");
    // one undo step back to the chords
    m.undo().unwrap();
    assert_eq!(count(&m, "SELECT count(*) FROM wall"), 24);
    assert_eq!(count(&m, "SELECT count(*) FROM wall_spline"), 0);
    let back: i64 = m.sql().query_row("SELECT wall FROM opening WHERE id=?1", [o], |r| r.get(0)).unwrap();
    assert_eq!(back, walls[12]);
    // a piece cannot be turned again
    let id = m.spline_from_walls(&walls, &s).unwrap();
    let piece = m.spline_wall_pieces(id).unwrap()[0];
    assert!(m.spline_from_walls(&[piece], &s).is_err());
}
