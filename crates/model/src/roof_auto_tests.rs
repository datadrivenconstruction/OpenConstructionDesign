// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rule-based wall tops under any roof: touch the roof underside, do not pierce it, and follow
//! roof edits and undo.

use ddc_geom::roof::Roof;
use ddc_geom::v2;

use crate::{LayerSpec, Model, RoofKind, RoofSpec, WallSpec};

const T: f64 = 0.2;

struct Fix {
    m: Model,
    level: i64,
    stype: i64,
    wtype: i64,
}

fn fix() -> Fix {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L0", 0.0).unwrap();
    let layer = |n: &str, t: f64| LayerSpec { material: n.into(), thickness: t };
    let stype = m.add_slab_type("Roof", &[layer("Tiles", 0.05), layer("Rafters", 0.15)]).unwrap();
    let wtype = m.add_wall_type("W", &[layer("Brick", 0.2)]).unwrap();
    Fix { m, level, stype, wtype }
}

impl Fix {
    fn wall(&mut self, a: (f64, f64), b: (f64, f64), h: f64) -> i64 {
        self.m
            .add_wall(&WallSpec { a, b, level: self.level, base_offset: 0.0, height: h, wall_type: self.wtype, offset0: None })
            .unwrap()
    }

    fn box_walls(&mut self, w: f64, d: f64, h: f64) -> Vec<i64> {
        let p = [(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)];
        (0..4).map(|i| self.wall(p[i], p[(i + 1) % 4], h)).collect()
    }

    fn roof(&mut self, kind: RoofKind, overhang: f64) -> i64 {
        self.m
            .add_roof(&RoofSpec { level: self.level, slab_type: self.stype, offset: 3.0, overhang, fascia: 0.0, kind })
            .unwrap()
    }
}

fn rect(w: f64, d: f64) -> Vec<(f64, f64)> {
    vec![(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)]
}

fn footprint(slopes: Vec<Option<f64>>) -> RoofKind {
    RoofKind::Footprint { ring: rect(10.0, 6.0), slopes, mansard: None }
}

fn at(top: &[(f64, f64)], u: f64) -> f64 {
    for w in top.windows(2) {
        if u >= w[0].0 - 1e-9 && u <= w[1].0 + 1e-9 && w[1].0 - w[0].0 > 1e-9 {
            return w[0].1 + (w[1].1 - w[0].1) * (u - w[0].0) / (w[1].0 - w[0].0);
        }
    }
    top.last().unwrap().1
}

/// Every wall's top follows the underside of `roof` (touches it, never above it).
fn touches(f: &Fix, roof: i64, walls: &[i64], what: &str) {
    let g: Roof = f.m.roof_geometry(roof).unwrap();
    for &w in walls {
        let top = f.m.wall_top_profile(w).unwrap().unwrap_or_else(|| panic!("{what}: wall {w} has no top"));
        let (a, b, base): ((f64, f64), (f64, f64), f64) = f
            .m
            .sql()
            .query_row(
                "SELECT w.ax,w.ay,w.bx,w.by,l.elevation + w.base_offset FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
                [w],
                |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?), r.get(4)?)),
            )
            .unwrap();
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let (mut seen, mut total) = (0, 0);
        let mut u = 0.0;
        while u <= len + 1e-9 {
            total += 1;
            let s = (u / len).clamp(0.0, 1.0);
            let p = v2(a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s);
            if let Some(z) = g.underside_at(p, T) {
                seen += 1;
                let wz = base + at(&top, u);
                // at a kink between two faces the surface is the lower (ridge) or higher (valley) plane
                let near = |du: f64| {
                    let s2 = ((u + du) / len).clamp(0.0, 1.0);
                    g.underside_at(v2(a.0 + (b.0 - a.0) * s2, a.1 + (b.1 - a.1) * s2), T).unwrap_or(z)
                };
                let (lo, hi) = (z.min(near(1e-3)).min(near(-1e-3)), z.max(near(1e-3)).max(near(-1e-3)));
                assert!(wz <= hi + 1e-4, "{what}: wall {w} pokes through the roof at u={u}: {wz} > {hi}");
                assert!(lo - wz <= 0.02, "{what}: wall {w} stops short of the roof at u={u}: {wz} < {lo}");
            }
            u += 0.25;
        }
        assert!(seen * 10 >= total * 7, "{what}: wall {w}: only {seen} of {total} samples under the roof");
    }
}

#[test]
fn every_wall_of_each_roof_preset_touches_the_underside() {
    let mansard = RoofKind::Footprint {
        ring: rect(10.0, 6.0),
        slopes: vec![Some(60.0); 4],
        mansard: Some((1.0, Some(20.0))),
    };
    let shed = RoofKind::Arrow {
        ring: vec![(-0.4, -0.4), (10.4, -0.4), (10.4, 6.4), (-0.4, 6.4)],
        tail: (0.0, 0.0),
        head: (0.0, 6.0),
        rise: 1.5,
    };
    let vault = RoofKind::Extrusion {
        a: (0.0, 0.0),
        b: (10.0, 0.0),
        profile: vec![(0.0, 0.0), (1.0, 1.2), (3.0, 2.4), (5.0, 2.8), (7.0, 2.4), (9.0, 1.2), (10.0, 0.0)],
        depth: 6.0,
    };
    let presets = [
        ("gable", footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0),
        ("hip", footprint(vec![Some(30.0); 4]), 0.4),
        ("pitched", footprint(vec![Some(35.0); 4]), 0.0),
        ("mansard", mansard, 0.3),
        ("shed", shed, 0.0),
        ("vault", vault, 0.0),
        (
            "gambrel",
            RoofKind::Footprint { ring: rect(10.0, 6.0), slopes: vec![Some(60.0), None, Some(60.0), None], mansard: Some((1.0, Some(25.0))) },
            0.0,
        ),
        // north lights: steep glazed faces and shallow backs, valleys inside the span
        (
            "sawtooth",
            RoofKind::Extrusion {
                a: (0.0, 0.0),
                b: (10.0, 0.0),
                profile: vec![(0.0, 0.2), (3.0, 1.2), (3.4, 0.2), (6.5, 1.4), (6.9, 0.2), (10.0, 1.4)],
                depth: 6.0,
            },
            0.0,
        ),
        (
            "butterfly",
            RoofKind::Extrusion { a: (0.0, 0.0), b: (10.0, 0.0), profile: vec![(0.0, 1.5), (5.0, 0.3), (10.0, 1.5)], depth: 6.0 },
            0.0,
        ),
    ];
    for (name, kind, overhang) in presets {
        let mut f = fix();
        let mut walls = f.box_walls(10.0, 6.0, 3.0);
        walls.push(f.wall((0.0, 3.0), (10.0, 3.0), 3.0)); // interior wall
        let r = f.roof(kind, overhang);
        for &w in &walls {
            assert_eq!(f.m.wall_top_roof(w).unwrap(), Some(r), "{name}: wall {w} not attached");
        }
        touches(&f, r, &walls, name);
        openings_stay_under(&mut f, &walls, name);
        f.m.assert_integrity();
    }
}

/// A door near the start and a window near the end of every trimmed wall: both heads fit under
/// the trimmed top, and adding them leaves the trim as it was.
fn openings_stay_under(f: &mut Fix, walls: &[i64], what: &str) {
    for &w in walls {
        let Some(top) = f.m.wall_top_profile(w).unwrap() else { continue };
        let len = top.last().unwrap().0;
        for (kind, along, sill, h) in [("door", 0.3, 0.0, 2.0), ("window", len - 1.2, 0.8, 1.0)] {
            let lowest = (0..=9).map(|i| at(&top, along + 0.1 * i as f64)).fold(f64::MAX, f64::min);
            assert!(sill + h + 0.05 < lowest, "{what}: wall {w}: the {kind} head {} does not fit under the top {lowest}", sill + h);
            f.m.add_opening(w, &crate::OpeningSpec { along, width: 0.9, sill, height: h, kind: kind.into() }).unwrap();
            assert_eq!(f.m.wall_top_profile(w).unwrap().as_ref(), Some(&top), "{what}: wall {w}: the {kind} changed the trim");
        }
    }
}

/// A flat roof drawn inside its parapets: the parapet walls keep their full height, the wall
/// under the deck is trimmed to it and its openings stay under.
#[test]
fn flat_roof_inside_parapets_keeps_them_and_trims_the_inner_wall() {
    let mut f = fix();
    let parapets = f.box_walls(10.0, 6.0, 4.0);
    let inner = f.wall((0.1, 3.0), (9.9, 3.0), 4.0);
    let deck = RoofKind::Extrusion { a: (0.1, 0.1), b: (9.9, 0.1), profile: vec![(0.0, 0.0), (9.8, 0.0)], depth: 5.8 };
    let r = f.roof(deck, 0.0);
    for &w in &parapets {
        assert_eq!(f.m.wall_top_profile(w).unwrap(), None, "parapet {w} was cut down");
    }
    touches(&f, r, &[inner], "deck");
    openings_stay_under(&mut f, &[inner], "deck");
}

/// A true arc barrel (a bulged extrusion) trims the walls under it as soon as it is drawn; undo
/// takes the trim away with the roof.
#[test]
fn arc_barrel_trims_the_walls_at_once_and_undo_releases_them() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.m.add_barrel_roof(f.level, f.stype, 3.0, (0.0, 0.0), (10.0, 0.0), 2.0, 6.0).unwrap();
    touches(&f, r, &walls, "arc barrel");
    openings_stay_under(&mut f, &walls, "arc barrel");
    for _ in 0..walls.len() * 2 {
        f.m.undo().unwrap();
    }
    f.m.undo().unwrap();
    for &w in &walls {
        assert_eq!(f.m.wall_top_profile(w).unwrap(), None);
    }
}

/// Raising the arc of a barrel retrims the wall under its crown; undo restores the top.
#[test]
fn barrel_arc_edit_retrims_and_undo_restores() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.m.add_barrel_roof(f.level, f.stype, 3.0, (0.0, 0.0), (10.0, 0.0), 2.0, 6.0).unwrap();
    let before = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    f.m.set_roof_arcs(r, &[-2.0 * 3.0 / 10.0], 0.0).unwrap();
    let after = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    assert!(at(&after, 5.0) > at(&before, 5.0) + 0.5, "{before:?} -> {after:?}");
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_top_profile(walls[0]).unwrap().unwrap(), before);
}

/// Joining a wing to the main roof carries its far end up the main slope: the wall under the
/// new part is trimmed; undo gives the old top back.
#[test]
fn roof_join_retrims_the_wall_under_the_wing_and_undo_restores() {
    let mut f = fix();
    let deg = 0.5f64.atan().to_degrees();
    let main = f.roof(RoofKind::Footprint { ring: rect(20.0, 10.0), slopes: vec![Some(deg), None, Some(deg), None], mansard: None }, 0.0);
    let wing = f
        .m
        .add_roof(&RoofSpec {
            level: f.level,
            slab_type: f.stype,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Extrusion { a: (8.0, -4.0), b: (12.0, -4.0), profile: vec![(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)], depth: 1.0 },
        })
        .unwrap();
    // along the wing, off its ridge: the crown over it is 4.5 m once joined
    let w = f.wall((9.5, -4.0), (9.5, -0.2), 6.0);
    let before = f.m.wall_top_profile(w).unwrap();
    f.m.join_roof(wing, main).unwrap();
    let after = f.m.wall_top_profile(w).unwrap().expect("the joined wing trims the wall");
    assert!(at(&after, 2.0) < 4.6, "{after:?}");
    assert_ne!(Some(after), before);
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_top_profile(w).unwrap(), before);
}

fn shed_over_the_box() -> RoofKind {
    RoofKind::Arrow { ring: vec![(-0.4, -0.4), (10.4, -0.4), (10.4, 6.4), (-0.4, 6.4)], tail: (0.0, 0.0), head: (0.0, 6.0), rise: 1.5 }
}

/// Turning a shed half round swaps its low and high sides: the eave wall rises, undo lowers it.
#[test]
fn rotating_the_roof_retrims_and_undo_restores() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 4.0);
    let r = f.roof(shed_over_the_box(), 0.0);
    let low = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    f.m.rotate_elements(&[crate::Target::Roof(r)], (5.0, 3.0), std::f64::consts::PI, false).unwrap();
    let high = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    assert!(at(&high, 5.0) > at(&low, 5.0) + 1.0, "{low:?} -> {high:?}");
    touches(&f, r, &walls, "turned shed");
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_top_profile(walls[0]).unwrap().unwrap(), low);
}

/// Mirroring a shed across its middle swaps its low and high sides the same way.
#[test]
fn mirroring_the_roof_retrims_and_undo_restores() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 4.0);
    let r = f.roof(shed_over_the_box(), 0.0);
    let low = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    f.m.mirror_elements(&[crate::Target::Roof(r)], (0.0, 3.0), (10.0, 3.0), false).unwrap();
    let high = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    assert!(at(&high, 5.0) > at(&low, 5.0) + 1.0, "{low:?} -> {high:?}");
    touches(&f, r, &walls, "mirrored shed");
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_top_profile(walls[0]).unwrap().unwrap(), low);
}

fn rows(m: &Model, table: &str) -> Vec<String> {
    // a set: derived rows are rewritten by undo, their storage order is not part of the model
    let mut st = m.sql().prepare(&format!("SELECT * FROM {table}")).unwrap();
    let n = st.column_count();
    let mut out: Vec<String> = st
        .query_map([], |r| (0..n).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect::<Result<Vec<_>, _>>())
        .unwrap()
        .map(|r| format!("{:?}", r.unwrap()))
        .collect();
    out.sort();
    out
}

/// A copy under a roof is trimmed in the same step; undo of a copy, an array or a rotated copy
/// removes the new walls (the trim's snapshot of a new wall must not bring it back).
#[test]
fn undo_of_copies_under_a_roof_restores_the_exact_rows() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 4.0);
    f.roof(shed_over_the_box(), 0.0);
    let t = [crate::Target::Wall(walls[0])];
    let ops: [(&str, Box<dyn Fn(&mut Model)>); 3] = [
        ("copy", Box::new(move |m: &mut Model| drop(m.copy_elements(&t, &[(0.0, 1.0)]).unwrap()))),
        ("array", Box::new(move |m: &mut Model| drop(m.array_linear(&t, 2, crate::Spacing::Step((0.0, 1.5))).unwrap()))),
        ("rotate", Box::new(move |m: &mut Model| drop(m.rotate_elements(&t, (5.0, 3.0), 0.3, true).unwrap()))),
    ];
    for (what, op) in ops {
        let before: Vec<_> = ["wall", "wall_qty"].map(|t| rows(&f.m, t)).into();
        op(&mut f.m);
        assert!(rows(&f.m, "wall").len() > before[0].len(), "{what}: no copy");
        f.m.undo().unwrap();
        let after: Vec<_> = ["wall", "wall_qty"].map(|t| rows(&f.m, t)).into();
        assert_eq!(after, before, "{what}: undo left rows");
    }
}

/// Moving walls that no roof reaches leaves every top alone (the retrim is skipped).
#[test]
fn moving_walls_far_from_the_roof_keeps_their_flat_tops() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let far = f.wall((30.0, 0.0), (40.0, 0.0), 3.0);
    let tops: Vec<_> = walls.iter().map(|&w| f.m.wall_top_profile(w).unwrap()).collect();
    f.m.move_elements(&[crate::Target::Wall(far)], (0.0, 5.0)).unwrap();
    assert_eq!(f.m.wall_top_profile(far).unwrap(), None);
    assert_eq!(walls.iter().map(|&w| f.m.wall_top_profile(w).unwrap()).collect::<Vec<_>>(), tops);
}

#[test]
fn walls_drawn_after_the_roof_attach_too() {
    let mut f = fix();
    let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let walls = f.box_walls(10.0, 6.0, 3.0);
    touches(&f, r, &walls, "after");
}

#[test]
fn wall_that_does_not_reach_the_roof_stays_flat() {
    let mut f = fix();
    let w = f.wall((1.0, 1.0), (9.0, 1.0), 2.0);
    f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    assert_eq!(f.m.wall_top_roof(w).unwrap(), None);
    assert_eq!(f.m.wall_top_profile(w).unwrap(), None);
}

fn vol(m: &Model, w: i64) -> f64 {
    m.sql().query_row("SELECT sum(volume) FROM wall_qty WHERE wall=?1", [w], |r| r.get(0)).unwrap()
}

#[test]
fn roof_edit_retrims_and_undo_restores() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    touches(&f, r, &walls, "30");
    let gable = walls[1]; // x = 10, gable end
    let v30 = vol(&f.m, gable);
    let mut s = f.m.roof(r).unwrap();
    s.kind = footprint(vec![Some(45.0), None, Some(45.0), None]);
    f.m.update_roof(r, &s).unwrap();
    touches(&f, r, &walls, "45");
    assert!(vol(&f.m, gable) > v30 + 0.3, "steeper gable fills more wall");
    f.m.undo().unwrap();
    touches(&f, r, &walls, "undo");
    assert!((vol(&f.m, gable) - v30).abs() < 1e-9);
    // slab-type thickness is part of the roof: the eave wall is shorter than the flat wall by it
    let flat = 10.0 * 0.2 * 3.0;
    let eave = vol(&f.m, walls[0]);
    assert!(eave < flat - 0.2 * 0.2 * 3.0 && eave > flat * 0.9, "eave wall volume {eave} vs flat {flat}");
}

#[test]
fn moving_the_roof_retrims_and_undo_restores() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let gable = walls[1]; // x = 10, from y 0 to 6: ridge over y = 3
    let p0 = f.m.wall_top_profile(gable).unwrap().unwrap();
    f.m.move_elements(&[crate::Target::Roof(r)], (0.0, 1.0)).unwrap();
    let p1 = f.m.wall_top_profile(gable).unwrap().unwrap();
    assert!(at(&p1, 4.0) > at(&p1, 2.0) + 0.5, "the ridge moved to y = 4: {p1:?}");
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_top_profile(gable).unwrap().unwrap(), p0);
}

#[test]
fn delete_roof_releases_walls_and_undo_returns_them() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    f.m.delete_roof(r).unwrap();
    for &w in &walls {
        assert_eq!(f.m.wall_top_profile(w).unwrap(), None);
    }
    f.m.assert_integrity();
    f.m.undo().unwrap();
    touches(&f, r, &walls, "restored");
}

#[test]
fn manual_detach_is_respected_by_later_roof_edits() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    f.m.detach_wall_top(walls[0]).unwrap();
    let mut s = f.m.roof(r).unwrap();
    s.overhang = 0.5;
    f.m.update_roof(r, &s).unwrap();
    assert_eq!(f.m.wall_top_profile(walls[0]).unwrap(), None);
    assert!(f.m.wall_top_profile(walls[1]).unwrap().is_some());
}

#[test]
fn wall_under_two_joined_roofs_follows_both() {
    let mut f = fix();
    let w = f.wall((0.0, 0.0), (20.0, 0.0), 3.0);
    let left = f.roof(footprint(vec![Some(30.0); 4]), 0.0);
    let right = f.roof(
        RoofKind::Footprint { ring: vec![(10.0, 0.0), (20.0, 0.0), (20.0, 6.0), (10.0, 6.0)], slopes: vec![Some(20.0); 4], mansard: None },
        0.0,
    );
    let top = f.m.wall_top_profile(w).unwrap().unwrap();
    let (gl, gr) = (f.m.roof_geometry(left).unwrap(), f.m.roof_geometry(right).unwrap());
    for u in [1.0, 4.0, 8.0, 12.0, 15.0, 19.0] {
        let p = v2(u, 0.0);
        let want = [gl.underside_at(p, T), gr.underside_at(p, T)].into_iter().flatten().fold(f64::MAX, f64::min);
        assert!((at(&top, u) - want).abs() < 0.03, "u={u}: {} vs {want}", at(&top, u));
    }
}

#[test]
fn wall_core_stops_at_the_roof_structure_and_the_finish_follows() {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L0", 0.0).unwrap();
    let layer = |n: &str, t: f64| LayerSpec { material: n.into(), thickness: t };
    // tiles, rafters (the structure), plasterboard lining below it
    let stype = m
        .add_slab_type("Roof", &[layer("Tiles", 0.05), layer("Rafters", 0.15), layer("Board", 0.02)])
        .unwrap();
    let wtype = m.add_wall_type("W", &[layer("Brick", 0.2), layer("Plaster", 0.02)]).unwrap();
    assert_eq!(m.wall_core_layers(wtype).unwrap(), vec![true, false]);
    let w = m
        .add_wall(&WallSpec { a: (0.0, 0.0), b: (10.0, 0.0), level, base_offset: 0.0, height: 3.0, wall_type: wtype, offset0: None })
        .unwrap();
    let r = m
        .add_roof(&RoofSpec {
            level,
            slab_type: stype,
            offset: 3.0,
            overhang: 0.4,
            fascia: 0.0,
            kind: footprint(vec![Some(30.0); 4]),
        })
        .unwrap();
    let (finish, core) = (m.wall_top_profile(w).unwrap().unwrap(), m.wall_top_core(w).unwrap().unwrap());
    let g = m.roof_geometry(r).unwrap();
    let p = v2(5.0, 0.0);
    // finish touches the underside of the whole roof, the core the underside of the structure
    assert!((at(&finish, 5.0) - g.underside_at(p, 0.22).unwrap()).abs() < 0.03);
    assert!((at(&core, 5.0) - g.underside_at(p, 0.20).unwrap()).abs() < 0.03);
    let lift = 0.02 / 30f64.to_radians().cos();
    assert!((at(&core, 5.0) - at(&finish, 5.0) - lift).abs() < 1e-6, "core sits {lift} above the finish");
    // quantities: each layer integrates its own top
    let avg = |pos: i64| -> f64 {
        m.sql()
            .query_row("SELECT volume / plan_area FROM wall_qty WHERE wall=?1 AND pos=?2", [w, pos], |r| r.get(0))
            .unwrap()
    };
    let d = avg(0) - avg(1);
    assert!((d - lift).abs() < 0.01, "core layer is {d} taller on average, expected {lift}");
    m.assert_integrity();
}

#[test]
fn insulation_threshold_is_a_project_setting() {
    let mut m = Model::open(":memory:").unwrap();
    assert!((m.insulation_lambda() - 0.1).abs() < 1e-12);
    m.set_insulation_lambda(0.3).unwrap();
    assert!((m.insulation_lambda() - 0.3).abs() < 1e-12);
    assert!(m.set_insulation_lambda(0.0).is_err() && m.set_insulation_lambda(5.0).is_err());
    m.undo().unwrap();
    assert!((m.insulation_lambda() - 0.1).abs() < 1e-12);
}

#[test]
fn undo_of_a_moved_wall_restores_axis_and_top() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let w = walls[0];
    let before = f.m.wall_top_profile(w).unwrap();
    f.m.set_wall_axis(w, (0.0, 0.5), (10.0, 0.5)).unwrap();
    f.m.undo().unwrap();
    let axis: (f64, f64, f64, f64) = f
        .m
        .sql()
        .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [w], |q| Ok((q.get(0)?, q.get(1)?, q.get(2)?, q.get(3)?)))
        .unwrap();
    assert_eq!(axis, (0.0, 0.0, 10.0, 0.0));
    assert_eq!(f.m.wall_top_profile(w).unwrap(), before);
    touches(&f, r, &walls, "after undo of move");
    f.m.assert_integrity();
}

#[test]
fn undo_of_a_new_wall_removes_it() {
    let mut f = fix();
    f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    let n: i64 = f.m.sql().query_row("SELECT count(*) FROM wall", [], |q| q.get(0)).unwrap();
    f.wall((1.0, 1.0), (9.0, 1.0), 3.0);
    f.m.undo().unwrap();
    let k: i64 = f.m.sql().query_row("SELECT count(*) FROM wall", [], |q| q.get(0)).unwrap();
    assert_eq!(n, k);
}

#[test]
fn level_attach_after_auto_is_not_overwritten_by_a_roof_edit() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    let up = f.m.add_level("L1", 3.0).unwrap();
    f.m.attach_wall_to_level(walls[0], up, 0.0).unwrap();
    let pinned = f.m.wall_top_profile(walls[0]).unwrap();
    let mut s = f.m.roof(r).unwrap();
    s.overhang = 0.6;
    f.m.update_roof(r, &s).unwrap();
    assert_eq!(f.m.wall_top_roof(walls[0]).unwrap(), None);
    assert_eq!(f.m.wall_top_profile(walls[0]).unwrap(), pinned);
}

#[test]
fn walls_of_a_file_made_before_the_rule_are_left_alone() {
    let mut f = fix();
    let w = f.wall((1.0, 1.0), (9.0, 1.0), 3.0);
    f.m.sql().execute("UPDATE wall SET top_auto=2 WHERE id=?1", [w]).unwrap();
    f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    assert_eq!(f.m.wall_top_profile(w).unwrap(), None);
}

#[test]
fn changing_a_rule_retrims_walls_in_one_undo_step() {
    let mut f = fix();
    let w = f.wall((1.0, 1.0), (9.0, 1.0), 2.0);
    f.roof(footprint(vec![Some(30.0); 4]), 0.4);
    assert_eq!(f.m.wall_top_profile(w).unwrap(), None, "too low for the default reach");
    f.m.set_roof_rule("reach", 3.0).unwrap();
    assert!(f.m.wall_top_profile(w).unwrap().is_some(), "wider reach attaches it");
    assert!(f.m.set_roof_rule("reach", 9.0).is_err() && f.m.set_roof_rule("nope", 1.0).is_err());
    f.m.undo().unwrap();
    assert_eq!(f.m.wall_top_profile(w).unwrap(), None);
}

#[test]
fn insulation_threshold_change_moves_the_wall_core() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let before = f.m.wall_top_core(walls[0]).unwrap();
    f.m.set_insulation_lambda(1.0).unwrap();
    f.m.set_insulation_lambda(0.1).unwrap();
    assert_eq!(f.m.wall_top_core(walls[0]).unwrap(), before);
}

#[test]
fn flat_roof_trims_walls_to_its_underside() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let flat = RoofKind::Extrusion {
        a: (0.0, 0.0),
        b: (10.0, 0.0),
        profile: vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)],
        depth: 6.0,
    };
    let r = f.roof(flat, 0.0);
    touches(&f, r, &walls, "flat");
}

/// A roof piece set on a lifted plane height with its profile hanging down to the eaves: its
/// plane height is far over the walls, its contour is not, and the gable ends rise to it.
#[test]
fn a_lifted_roof_piece_is_found_by_its_contour_and_the_gable_ends_rise_to_it() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let r = f
        .m
        .add_roof(&RoofSpec {
            level: f.level,
            slab_type: f.stype,
            offset: 4.5,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Extrusion { a: (0.0, 0.0), b: (10.0, 0.0), profile: vec![(0.0, -1.5), (5.0, 0.0), (10.0, -1.5)], depth: 6.0 },
        })
        .unwrap();
    for &w in &walls {
        assert_eq!(f.m.wall_top_roof(w).unwrap(), Some(r), "wall {w} not attached to the lifted piece");
    }
    touches(&f, r, &walls, "lifted");
    // the gable end along y = 0 rises to the ridge underside in its middle
    let top = f.m.wall_top_profile(walls[0]).unwrap().unwrap();
    assert!(at(&top, 5.0) > 4.0, "{top:?}");
}

/// Fifty gable houses of forty walls each: a full sync measures each wall against the roofs over
/// it, not against all fifty, and moving one roof retrims no more walls than stand under it.
/// Work is counted, not timed.
#[test]
fn a_block_of_houses_retrims_only_the_walls_under_a_moved_roof() {
    use crate::Edit;
    let mut f = fix();
    let origin = |k: usize| ((k % 10) as f64 * 16.0, (k / 10) as f64 * 12.0);
    let spec = |a, b| WallSpec { a, b, level: f.level, base_offset: 0.0, height: 3.0, wall_type: f.wtype, offset0: None };
    let mut edits = vec![];
    for k in 0..50 {
        let (x, y) = origin(k);
        let p = [(x, y), (x + 10.0, y), (x + 10.0, y + 6.0), (x, y + 6.0)];
        for i in 0..4 {
            edits.push(Edit::NewWall { spec: spec(p[i], p[(i + 1) % 4]), bulge: 0.0 });
        }
        // partitions: six rows of six short walls, apart from each other and the outer walls
        for r in 0..6 {
            for s in 0..6 {
                let (x0, y0) = (x + 0.4 + s as f64 * 1.6, y + 0.6 + r as f64 * 0.95);
                edits.push(Edit::NewWall { spec: spec((x0, y0), (x0 + 1.2, y0)), bulge: 0.0 });
            }
        }
    }
    f.m.apply_edits(&edits).unwrap();
    let roofs: Vec<i64> = (0..50)
        .map(|k| {
            let (x, y) = origin(k);
            let ring = rect(10.0, 6.0).into_iter().map(|q| (q.0 + x, q.1 + y)).collect();
            f.roof(RoofKind::Footprint { ring, slopes: vec![Some(30.0), None, Some(30.0), None], mansard: None }, 0.0)
        })
        .collect();
    let walls: i64 = f.m.sql().query_row("SELECT count(*) FROM wall", [], |q| q.get(0)).unwrap();
    assert_eq!(walls, 2000);
    let under = |r: i64| -> i64 { f.m.sql().query_row("SELECT count(*) FROM wall WHERE top_roof=?1", [r], |q| q.get(0)).unwrap() };
    assert!(roofs.iter().all(|&r| under(r) == 40), "every house's walls follow its own roof");
    // a full sync (a rule change): each wall is measured against its own roof only
    crate::roof_auto::take_counts();
    f.m.set_roof_rule("reach", 0.7).unwrap();
    let (measured, pairs) = crate::roof_auto::take_counts();
    assert!(measured >= 2000, "{measured}");
    assert!(pairs <= measured, "{pairs} wall-roof pairs for {measured} wall measures under 50 roofs");
    // moving one roof a little: its forty walls, nothing else
    let r = roofs[23];
    f.m.move_elements(&[crate::Target::Roof(r)], (0.0, 0.5)).unwrap();
    let (measured, pairs) = crate::roof_auto::take_counts();
    assert!(measured <= 40 && pairs <= 40, "moving one roof measured {measured} walls, {pairs} pairs");
    assert!(measured >= 4, "its walls were retrimmed: {measured}");
}

#[test]
fn undo_of_a_bulk_edit_restores_walls_and_tops() {
    use crate::Edit;
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let w = walls[0];
    let before = f.m.wall_top_profile(w).unwrap();
    let n: i64 = f.m.sql().query_row("SELECT count(*) FROM wall", [], |q| q.get(0)).unwrap();
    let spec = |a, b| WallSpec { a, b, level: f.level, base_offset: 0.0, height: 3.0, wall_type: f.wtype, offset0: None };
    let edits = [
        Edit::Wall { id: w, spec: spec((0.0, 0.5), (10.0, 0.5)), bulge: 0.0 },
        Edit::NewWall { spec: spec((1.0, 1.0), (9.0, 1.0)), bulge: 0.0 },
    ];
    f.m.apply_edits(&edits).unwrap();
    f.m.undo().unwrap();
    let axis: (f64, f64, f64, f64) = f
        .m
        .sql()
        .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [w], |q| Ok((q.get(0)?, q.get(1)?, q.get(2)?, q.get(3)?)))
        .unwrap();
    assert_eq!(axis, (0.0, 0.0, 10.0, 0.0));
    assert_eq!(f.m.wall_top_profile(w).unwrap(), before);
    let k: i64 = f.m.sql().query_row("SELECT count(*) FROM wall", [], |q| q.get(0)).unwrap();
    assert_eq!(n, k);
    f.m.assert_integrity();
}

#[test]
fn wall_over_the_void_of_an_l_shaped_roof_stays_flat_there() {
    let mut f = fix();
    let w = f.wall((0.0, 6.0), (8.0, 6.0), 3.0);
    let ring = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 4.0), (5.0, 4.0), (5.0, 8.0), (0.0, 8.0)];
    f.roof(RoofKind::Footprint { ring, slopes: vec![Some(30.0); 6], mansard: None }, 0.0);
    let top = f.m.wall_top_profile(w).unwrap().expect("covered part follows the roof");
    assert!((at(&top, 6.5) - 3.0).abs() < 1e-6, "no roof over x 5..8: flat at the wall height, got {}", at(&top, 6.5));
    assert!((at(&top, 7.9) - 3.0).abs() < 1e-6);
}

#[test]
fn wall_along_a_ridge_of_unequal_slopes_takes_the_lower_offset_plane() {
    let mut f = fix();
    let (t0, t1) = (20f64.to_radians().tan(), 40f64.to_radians().tan());
    let y = 6.0 * t1 / (t0 + t1);
    let w = f.wall((0.0, y), (10.0, y), 3.0);
    let r = f.roof(footprint(vec![Some(20.0), None, Some(40.0), None]), 0.0);
    let top = f.m.wall_top_profile(w).unwrap().expect("ridge wall is trimmed");
    let want = 3.0 + y * t0 - T / 40f64.to_radians().cos();
    assert!((at(&top, 5.0) - want).abs() < 0.02, "ridge underside {} vs {want}", at(&top, 5.0));
    let _ = r;
}

#[test]
fn low_eave_keeps_the_wall_at_the_least_height_instead_of_dropping_it() {
    let mut f = fix();
    let w = f
        .m
        .add_wall(&WallSpec { a: (0.0, 0.0), b: (0.0, 6.0), level: f.level, base_offset: 2.8, height: 0.5, wall_type: f.wtype, offset0: None })
        .unwrap();
    f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let top = f.m.wall_top_profile(w).unwrap().expect("a low eave must not drop the wall");
    assert!(top.iter().all(|p| p.1 >= 0.05 - 1e-9), "clamped at the least height: {top:?}");
    assert!((top[0].1 - 0.05).abs() < 1e-6);
}

#[test]
fn deleting_one_of_two_roofs_keeps_the_wall_on_the_other() {
    let mut f = fix();
    let walls = f.box_walls(10.0, 6.0, 3.0);
    let a = f.roof(footprint(vec![Some(30.0); 4]), 0.0);
    let b = f
        .m
        .add_roof(&RoofSpec { level: f.level, slab_type: f.stype, offset: 3.5, overhang: 0.0, fascia: 0.0, kind: footprint(vec![Some(30.0); 4]) })
        .unwrap();
    assert_eq!(f.m.wall_top_roof(walls[0]).unwrap(), Some(a), "the lower roof leads");
    f.m.delete_roof(a).unwrap();
    assert_eq!(f.m.wall_top_roof(walls[0]).unwrap(), Some(b));
    touches(&f, b, &walls, "second roof");
}

fn wall_on(f: &mut Fix, level: i64, a: (f64, f64), b: (f64, f64), h: f64) -> i64 {
    f.m.add_wall(&WallSpec { a, b, level, base_offset: 0.0, height: h, wall_type: f.wtype, offset0: None }).unwrap()
}

fn square(x0: f64, y0: f64, s: f64) -> ddc_geom::sketch::Sketch {
    use ddc_geom::sketch::{Sketch, SketchCurve};
    let p = [v2(x0, y0), v2(x0 + s, y0), v2(x0 + s, y0 + s), v2(x0, y0 + s)];
    Sketch { curves: (0..4).map(|i| SketchCurve::Line { a: p[i], b: p[(i + 1) % 4] }).collect() }
}

#[test]
fn wall_through_a_dormer_opening_follows_the_slopes_and_stays_flat_in_the_opening() {
    let mut f = fix();
    let w = f.wall((3.0, 1.0), (7.0, 1.0), 3.0);
    let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let slope = at(&f.m.wall_top_profile(w).unwrap().unwrap(), 0.5);
    f.m.add_host_opening(crate::OpeningHost::Roof(r), &square(4.0, 0.5, 1.0), true).unwrap();
    let top = f.m.wall_top_profile(w).unwrap().expect("still trimmed beside the dormer");
    assert!((at(&top, 0.5) - slope).abs() < 0.02, "beside the dormer the wall follows the slope");
    assert!((at(&top, 3.0) - slope).abs() < 0.02);
    assert!((at(&top, 1.5) - 3.0).abs() < 1e-6, "in the opening there is no roof: flat, got {}", at(&top, 1.5));
    f.m.undo().unwrap();
    assert!((at(&f.m.wall_top_profile(w).unwrap().unwrap(), 1.5) - slope).abs() < 0.05, "undo returns the full slope");
}

#[test]
fn interior_walls_under_a_gable_roof_are_trimmed_like_exterior_ones() {
    let mut f = fix();
    let mut walls = f.box_walls(10.0, 6.0, 3.0);
    walls.push(f.wall((0.0, 3.0), (10.0, 3.0), 3.0)); // along the ridge
    walls.push(f.wall((5.0, 0.0), (5.0, 6.0), 3.0)); // across the slopes
    walls.push(f.wall((2.0, 1.0), (2.0, 5.0), 3.0)); // inside, short
    let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    touches(&f, r, &walls, "interior");
    // a wall drawn afterwards is trimmed too
    let late = f.wall((7.0, 1.0), (7.0, 5.0), 3.0);
    touches(&f, r, &[late], "interior late");
}

#[test]
fn mono_pitch_over_several_storeys_trims_each_storey_up_to_its_own_floor() {
    let mut f = fix();
    let (l1, l2) = (f.m.add_level("L1", 3.0).unwrap(), f.m.add_level("L2", 6.0).unwrap());
    let slab_type = f.m.add_slab_type("Floor", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }]).unwrap();
    f.m.add_slab(&crate::SlabSpec { level: l2, slab_type, offset: 0.0, outer: rect(10.0, 6.0), holes: vec![] }).unwrap();
    let low = wall_on(&mut f, l1, (1.0, 0.0), (1.0, 6.0), 3.0);
    let high = wall_on(&mut f, l2, (1.0, 4.0), (1.0, 6.0), 3.0);
    let ring = rect(10.0, 6.0);
    let r = f
        .m
        .add_roof(&RoofSpec {
            level: f.level,
            slab_type: f.stype,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Arrow { ring, tail: (0.0, 0.0), head: (0.0, 6.0), rise: 6.0 },
        })
        .unwrap();
    let t_low = f.m.wall_top_profile(low).unwrap().expect("middle storey is trimmed");
    assert!(t_low.iter().all(|p| p.1 <= 3.0 + 1e-6), "stops at the floor above: {t_low:?}");
    assert!((at(&t_low, 5.5) - 3.0).abs() < 1e-6);
    touches(&f, r, &[high], "top storey");
    let t_high = f.m.wall_top_profile(high).unwrap().unwrap();
    assert!(at(&t_high, 2.0) > 2.0, "the top storey reaches up to the roof");
}

#[test]
fn gable_end_of_the_upper_storey_fills_to_the_ridge_and_the_ground_floor_stays_flat() {
    let mut f = fix();
    let l1 = f.m.add_level("L1", 3.0).unwrap();
    let ground_end = f.wall((10.0, 0.0), (10.0, 6.0), 3.0);
    let ground_inner = f.wall((5.0, 0.0), (5.0, 6.0), 3.0);
    let up_end = wall_on(&mut f, l1, (10.0, 0.0), (10.0, 6.0), 3.0);
    let up_inner = wall_on(&mut f, l1, (5.0, 0.0), (5.0, 6.0), 3.0);
    let r = f
        .m
        .add_roof(&RoofSpec {
            level: l1,
            slab_type: f.stype,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: footprint(vec![Some(30.0), None, Some(30.0), None]),
        })
        .unwrap();
    touches(&f, r, &[up_end, up_inner], "upper storey");
    let top = f.m.wall_top_profile(up_end).unwrap().unwrap();
    assert!(at(&top, 3.0) > at(&top, 0.2) + 1.2, "gable fills up to the ridge");
    assert_eq!(f.m.wall_top_profile(ground_end).unwrap(), None);
    assert_eq!(f.m.wall_top_profile(ground_inner).unwrap(), None);
}

#[test]
fn wall_capped_down_to_its_base_keeps_the_least_height_and_the_trim() {
    let mut f = fix();
    let l1 = f.m.add_level("L1", 3.0).unwrap();
    let slab_type = f.m.add_slab_type("Floor", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }]).unwrap();
    f.m.add_slab(&crate::SlabSpec { level: l1, slab_type, offset: 0.0, outer: rect(10.0, 6.0), holes: vec![] }).unwrap();
    let w = wall_on(&mut f, l1, (2.0, 3.0), (8.0, 3.0), 0.4);
    f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    let top = f.m.wall_top_profile(w).unwrap().expect("a floor at the wall base must not drop the trim");
    assert!(top.iter().all(|p| (p.1 - 0.05).abs() < 1e-9), "{top:?}");
    f.m.set_roof_rule("min_height", 0.0).unwrap();
    let top = f.m.wall_top_profile(w).unwrap().expect("min height 0 keeps the trim");
    assert!(top.iter().all(|p| p.1 < 1e-5), "{top:?}");
}

#[test]
fn wall_lying_on_a_slab_edge_is_capped_by_that_slab() {
    let mut f = fix();
    let (l1, l2) = (f.m.add_level("L1", 3.0).unwrap(), f.m.add_level("L2", 6.0).unwrap());
    let slab_type = f.m.add_slab_type("Floor", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }]).unwrap();
    f.m.add_slab(&crate::SlabSpec { level: l2, slab_type, offset: 0.0, outer: rect(10.0, 6.0), holes: vec![] }).unwrap();
    let w = wall_on(&mut f, l1, (1.0, 6.0), (9.0, 6.0), 3.0);
    f.m
        .add_roof(&RoofSpec {
            level: f.level,
            slab_type: f.stype,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Arrow {
                ring: vec![(-1.0, -1.0), (11.0, -1.0), (11.0, 7.0), (-1.0, 7.0)],
                tail: (0.0, 0.0),
                head: (0.0, 6.0),
                rise: 6.0,
            },
        })
        .unwrap();
    let top = f.m.wall_top_profile(w).unwrap().expect("trimmed");
    assert!(top.iter().all(|p| p.1 <= 3.0 + 1e-6), "the slab whose edge carries the wall caps it: {top:?}");
    assert!((at(&top, 4.0) - 3.0).abs() < 1e-6);
}

mod outputs {
    use super::*;
    use crate::views::{LineClass, PlanViewSpec, ViewPrim};
    use crate::{SectionSpec, ViewKind};

    /// 10 x 6 box, gable roof (30 deg, ridge along x at y = 3), one wall layer 0.2 Brick.
    fn gable_house() -> (Fix, Vec<i64>, i64) {
        let mut f = fix();
        let walls = f.box_walls(10.0, 6.0, 3.0);
        let r = f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
        (f, walls, r)
    }

    fn ridge() -> f64 {
        3.0 + 3.0 * 30f64.to_radians().tan() - T / 30f64.to_radians().cos()
    }

    fn eave() -> f64 {
        3.0 - T / 30f64.to_radians().cos()
    }

    #[test]
    fn plan_cut_above_a_low_part_shows_it_as_beyond() {
        let (f, walls, _) = gable_house();
        let v = f.m.plan_view(&PlanViewSpec { level: f.level, cut_height: 3.5, grids: vec![] }).unwrap();
        let cut_y = |w: i64| -> Option<(f64, f64)> {
            let ys: Vec<f64> = v
                .prims
                .iter()
                .filter_map(|p| match p {
                    ViewPrim::Cut { wall, ring, .. } if *wall == w => Some(ring.iter().map(|q| q.1).collect::<Vec<_>>()),
                    _ => None,
                })
                .flatten()
                .collect();
            (!ys.is_empty()).then(|| (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max)))
        };
        let beyond = |w: i64| {
            v.prims.iter().any(|p| matches!(p, ViewPrim::Edge { element, class: LineClass::Projection, .. } if *element == w))
        };
        // eave walls (top at the eave, below the plane) are seen from above, not cut
        assert!(cut_y(walls[0]).is_none() && beyond(walls[0]), "south eave wall is beyond");
        assert!(cut_y(walls[2]).is_none() && beyond(walls[2]), "north eave wall is beyond");
        // the gable end is cut only where it is higher than the plane
        let (lo, hi) = cut_y(walls[1]).expect("gable end is cut in the middle");
        let y0 = (3.5 - eave()) / 30f64.to_radians().tan();
        assert!((lo - y0).abs() < 0.03, "cut starts near y={y0}, got {lo}");
        assert!((hi - (6.0 - y0)).abs() < 0.03, "cut ends near y={}, got {hi}", 6.0 - y0);
        assert!(beyond(walls[1]), "its low ends are beyond");
    }

    fn section_cuts(f: &Fix, a: (f64, f64), b: (f64, f64), kind: ViewKind) -> Vec<(i64, f64)> {
        let spec = SectionSpec { kind, name: "s".into(), a, b, depth: 10.0, far_clip: false };
        let v = f.m.section_view(&spec, &[]).unwrap();
        v.prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::SectionCut { table: "wall", element, rings, .. } => {
                    Some((*element, rings.iter().map(|q| q.1).fold(f64::MIN, f64::max)))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn section_wall_top_follows_the_roof_slope() {
        let (f, walls, _) = gable_house();
        // along the ridge line across the gable ends: both end walls reach the ridge underside
        let cuts = section_cuts(&f, (-1.0, 3.0), (11.0, 3.0), ViewKind::Section);
        for w in [walls[1], walls[3]] {
            let z = cuts.iter().filter(|c| c.0 == w).map(|c| c.1).fold(f64::MIN, f64::max);
            assert!((z - ridge()).abs() < 0.02, "gable end {w}: cut top {z} vs ridge {}", ridge());
        }
        // across the slopes: the eave walls stop at the eave underside, not at the flat height
        let cuts = section_cuts(&f, (5.0, -1.0), (5.0, 7.0), ViewKind::Section);
        for w in [walls[0], walls[2]] {
            let z = cuts.iter().filter(|c| c.0 == w).map(|c| c.1).fold(f64::MIN, f64::max);
            assert!((z - eave()).abs() < 0.02, "eave wall {w}: cut top {z} vs {}", eave());
        }
    }

    #[test]
    fn elevation_shows_the_gable_wall_up_to_the_ridge() {
        let (f, walls, _) = gable_house();
        let spec = |a, b| SectionSpec { kind: ViewKind::Elevation, name: "e".into(), a, b, depth: 4.0, far_clip: false };
        let mut top = f64::MIN;
        for s in [spec((12.0, -1.0), (12.0, 7.0)), spec((12.0, 7.0), (12.0, -1.0))] {
            let v = f.m.section_view(&s, &[]).unwrap();
            for p in &v.prims {
                if let ViewPrim::Edge { element, a, b, .. } = p {
                    if *element == walls[1] {
                        top = top.max(a.1).max(b.1);
                    }
                }
            }
        }
        assert!((top - ridge()).abs() < 0.02, "elevation top of the gable end {top} vs ridge {}", ridge());
    }

    #[test]
    fn wall_quantities_use_the_trimmed_profile() {
        let mut f = fix();
        let w = f.wall((0.0, 0.0), (10.0, 0.0), 3.0);
        f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
        let top = f.m.wall_top_profile(w).unwrap().expect("trimmed");
        let area: f64 = top.windows(2).map(|p| (p[1].0 - p[0].0) * (p[0].1 + p[1].1) / 2.0).sum();
        assert!((area - 10.0 * eave()).abs() < 1e-6, "profile area {area} vs {}", 10.0 * eave());
        assert!((vol(&f.m, w) - 0.2 * 10.0 * eave()).abs() < 1e-6, "volume {}", vol(&f.m, w));
        let gable = {
            let mut g = fix();
            let e = g.wall((10.0, 0.0), (10.0, 6.0), 3.0);
            g.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
            vol(&g.m, e)
        };
        let want = 0.2 * 3.0 * (eave() + ridge());
        assert!((gable - want).abs() < 1e-3, "gable end volume {gable} vs {want}");
    }
}

/// Walls under curved roofs: a dome shell, trimmed and following edits and undo.
mod curved {
    use super::*;
    use crate::{FreeformSpec, ShellKind, ShellLayer, ShellSide};
    use ddc_geom::nurbs::{self, Trim};
    use ddc_geom::v3;

    const R: f64 = 4.5;
    const RISE: f64 = 2.0;

    fn dome_roof(f: &mut Fix, rise: f64) -> (i64, i64) {
        let surface = nurbs::dome(v3(5.0, 3.0, 3.0), R, rise).unwrap();
        let spec = FreeformSpec {
            level: f.level,
            name: "Dome".into(),
            material: String::new(),
            surface,
            thickness: 0.0,
            side: ShellSide::Normal,
            trim: Trim::default(),
            origin: "points".into(),
        };
        let h = f.m.add_freeform(&spec).unwrap();
        let layers = vec![ShellLayer { name: "Cover".into(), material: "Cover".into(), thickness: 0.2 }];
        let s = f.m.shell_on_host(h, ShellKind::Roof, layers, ShellSide::Normal, vec![], "Dome roof").unwrap();
        (h, f.m.add_shell_elem(&s).unwrap())
    }

    /// Height of the dome surface over x on the line y = 3.
    fn dome_z(x: f64, rise: f64) -> f64 {
        let rho = (R * R + rise * rise) / (2.0 * rise);
        3.0 + rise - rho + (rho * rho - (x - 5.0).powi(2)).sqrt()
    }

    #[test]
    fn wall_under_a_dome_follows_its_underside() {
        let mut f = fix();
        let w = f.wall((1.0, 3.0), (9.0, 3.0), 6.0);
        dome_roof(&mut f, RISE);
        let top = f.m.wall_top_profile(w).unwrap().expect("a wall under the dome is trimmed");
        for u in [0.0, 1.0, 2.5, 4.0, 5.5, 7.0, 8.0] {
            let want = dome_z(1.0 + u, RISE);
            assert!((at(&top, u) - want).abs() < 0.01, "u={u}: {} vs {want}", at(&top, u));
        }
        assert!(at(&top, 4.0) > at(&top, 0.0) + 1.0, "the top rises to the crown");
    }

    #[test]
    fn dome_edit_delete_and_undo_move_the_wall_top() {
        let mut f = fix();
        let w = f.wall((1.0, 3.0), (9.0, 3.0), 6.0);
        let (_, id) = dome_roof(&mut f, RISE);
        let before = f.m.wall_top_profile(w).unwrap().unwrap();
        // a taller crown host edit re-trims the wall
        let host = f.m.shell_elem(id).unwrap().host.unwrap();
        let mut spec = f.m.freeform(host).unwrap();
        spec.surface = nurbs::dome(v3(5.0, 3.0, 3.0), R, 3.0).unwrap();
        f.m.update_freeform(host, &spec).unwrap();
        let taller = f.m.wall_top_profile(w).unwrap().unwrap();
        assert!((at(&taller, 4.0) - dome_z(5.0, 3.0)).abs() < 0.01, "{}", at(&taller, 4.0));
        f.m.undo().unwrap();
        assert_eq!(f.m.wall_top_profile(w).unwrap().unwrap(), before, "undo restores the earlier top");
        // the roof is deleted: the rule lets the wall go
        f.m.delete_shell_elem(id).unwrap();
        assert!(f.m.wall_top_profile(w).unwrap().is_none(), "no roof, no trim");
        f.m.undo().unwrap();
        assert_eq!(f.m.wall_top_profile(w).unwrap().unwrap(), before);
    }

    #[test]
    fn wall_outside_the_dome_stays_full_height() {
        let mut f = fix();
        let w = f.wall((20.0, 3.0), (28.0, 3.0), 6.0);
        dome_roof(&mut f, RISE);
        assert!(f.m.wall_top_profile(w).unwrap().is_none());
    }
}

/// Arc walls: the profile runs along the axis length.
mod arcs {
    use super::*;
    use crate::views::{PlanViewSpec, ViewPrim};
    use crate::{SectionSpec, ViewKind};
    use ddc_geom::arc::ArcAxis;
    use ddc_geom::Tol;

    const A: (f64, f64) = (2.0, 1.5);
    const B: (f64, f64) = (8.0, 1.5);
    const BULGE: f64 = 0.3;

    fn axis() -> ArcAxis {
        ArcAxis::new(v2(A.0, A.1), v2(B.0, B.1), BULGE, Tol::default()).unwrap()
    }

    fn arc_wall(f: &mut Fix, h: f64) -> i64 {
        f.m
            .add_arc_wall(&WallSpec { a: A, b: B, level: f.level, base_offset: 0.0, height: h, wall_type: f.wtype, offset0: None }, BULGE)
            .unwrap()
    }

    fn gable(f: &mut Fix) {
        f.roof(footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0);
    }

    fn eave() -> f64 {
        3.0 - T / 30f64.to_radians().cos()
    }

    #[test]
    fn arc_wall_under_a_gable_roof_follows_the_slope_along_its_length() {
        let mut f = fix();
        let w = arc_wall(&mut f, 3.0);
        gable(&mut f);
        let top = f.m.wall_top_profile(w).unwrap().expect("an arc wall is trimmed by the rule");
        let ax = axis();
        for k in 1..20 {
            let u = ax.length() * k as f64 / 20.0;
            let p = ax.point_at(u);
            let want = eave() + p.y * 30f64.to_radians().tan();
            assert!((at(&top, u) - want).abs() < 0.01, "u={u}: {} vs {want}", at(&top, u));
        }
        assert!(top.last().unwrap().0 > ax.length() - 1e-6, "profile spans the arc length, not the chord");
    }

    #[test]
    fn a_new_bulge_retrims_the_wall_and_undo_restores_it() {
        let mut f = fix();
        let w = f.wall(A, B, 3.0);
        gable(&mut f);
        let straight = f.m.wall_top_profile(w).unwrap().unwrap();
        f.m.set_wall_bulge(w, BULGE).unwrap();
        let arc = f.m.wall_top_profile(w).unwrap().unwrap();
        assert!(arc.last().unwrap().0 > 6.0 + 0.05, "the length is the arc's");
        let ax = axis();
        let u = ax.length() / 2.0;
        assert!((at(&arc, u) - (eave() + ax.point_at(u).y * 30f64.to_radians().tan())).abs() < 0.01);
        f.m.undo().unwrap();
        assert_eq!(f.m.wall_top_profile(w).unwrap().unwrap(), straight);
    }

    #[test]
    fn an_arc_wall_takes_a_manual_profile() {
        let mut f = fix();
        let w = arc_wall(&mut f, 3.0);
        let l = axis().length();
        let prof = [(0.0, 1.0), (l / 2.0, 2.5), (l, 1.0)];
        f.m.set_wall_top_profile(w, Some(&prof)).unwrap();
        let got = f.m.wall_top_profile(w).unwrap().unwrap();
        assert!(got.iter().zip(&prof).all(|(g, p)| (g.0 - p.0).abs() < 1e-8 && (g.1 - p.1).abs() < 1e-8), "{got:?}");
        f.m.undo().unwrap();
        assert!(f.m.wall_top_profile(w).unwrap().is_none());
    }

    #[test]
    fn plan_cuts_an_arc_wall_only_where_it_is_high() {
        let mut f = fix();
        let w = arc_wall(&mut f, 3.0);
        let ax = axis();
        let l = ax.length();
        f.m.set_wall_top_profile(w, Some(&[(0.0, 1.0), (l / 2.0, 3.0), (l, 1.0)])).unwrap();
        // the wall is higher than 2.0 over the middle half of the axis
        let v = f.m.plan_view(&PlanViewSpec { level: f.level, cut_height: 2.0, grids: vec![] }).unwrap();
        let mut us = vec![];
        for p in &v.prims {
            if let ViewPrim::Cut { wall, ring, .. } = p {
                if *wall == w {
                    us.extend(ring.iter().map(|q| ax.param_of(v2(q.0, q.1))));
                }
            }
        }
        assert!(!us.is_empty(), "cut in the middle");
        let (lo, hi) = (us.iter().cloned().fold(f64::MAX, f64::min), us.iter().cloned().fold(f64::MIN, f64::max));
        assert!((lo - l / 4.0).abs() < 0.03 && (hi - 3.0 * l / 4.0).abs() < 0.03, "cut spans {lo}..{hi} of {l}");
        assert!(
            v.prims.iter().any(|p| matches!(p, ViewPrim::Edge { element, class: crate::views::LineClass::Projection, .. } if *element == w)),
            "the low ends are seen from above"
        );
    }

    #[test]
    fn section_cuts_an_arc_wall_to_its_profile() {
        let mut f = fix();
        let w = arc_wall(&mut f, 3.0);
        let l = axis().length();
        f.m.set_wall_top_profile(w, Some(&[(0.0, 1.0), (l / 2.0, 2.5), (l, 1.0)])).unwrap();
        let spec = SectionSpec { kind: ViewKind::Section, name: "s".into(), a: (5.0, -1.0), b: (5.0, 5.0), depth: 10.0, far_clip: false };
        let v = f.m.section_view(&spec, &[]).unwrap();
        let top = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::SectionCut { table: "wall", element, rings, .. } if *element == w => Some(rings.iter().map(|q| q.1).fold(f64::MIN, f64::max)),
                _ => None,
            })
            .fold(f64::MIN, f64::max);
        assert!((top - 2.5).abs() < 0.03, "section top {top}");
    }

    #[test]
    fn arc_wall_volume_is_thickness_times_the_profile_integral_along_the_arc() {
        let mut f = fix();
        let w = arc_wall(&mut f, 3.0);
        let l = axis().length();
        let prof = [(0.0, 1.0), (l / 2.0, 2.5), (l, 1.0)];
        f.m.set_wall_top_profile(w, Some(&prof)).unwrap();
        let area: f64 = prof.windows(2).map(|p| (p[1].0 - p[0].0) * (p[0].1 + p[1].1) / 2.0).sum();
        // a centred wall: the layer's mean circle is the axis, so the integral is along the axis
        assert!((vol(&f.m, w) - 0.2 * area).abs() < 1e-3, "volume {} vs {}", vol(&f.m, w), 0.2 * area);
        // by the roof rule too
        let mut g = fix();
        let rw = arc_wall(&mut g, 3.0);
        gable(&mut g);
        let top = g.m.wall_top_profile(rw).unwrap().unwrap();
        let a2: f64 = top.windows(2).map(|p| (p[1].0 - p[0].0) * (p[0].1 + p[1].1) / 2.0).sum();
        assert!((vol(&g.m, rw) - 0.2 * a2).abs() < 1e-3, "volume {} vs {}", vol(&g.m, rw), 0.2 * a2);
    }
}

/// Every wall under any roof type reaches the underside within 50 mm and nothing pokes through:
/// planar presets (straight and arc walls) and curved shells (dome, barrel, free-form).
mod reach {
    use super::*;
    use crate::{FreeformSpec, ShellKind, ShellLayer, ShellSide};
    use ddc_geom::arc::ArcAxis;
    use ddc_geom::nurbs::{self, Surface, Trim};
    use ddc_geom::{v3, Tol, V2};

    const ARC_A: (f64, f64) = (2.0, 1.5);
    const ARC_B: (f64, f64) = (8.0, 1.5);
    const BULGE: f64 = 0.3;
    const SHORT: f64 = 0.05;
    const POKE: f64 = 5e-3;

    /// Samples every wall along its axis (arc length on arcs) against `under`.
    fn check(f: &Fix, walls: &[i64], what: &str, under: &dyn Fn(V2) -> Option<f64>) {
        for &w in walls {
            let top = f.m.wall_top_profile(w).unwrap().unwrap_or_else(|| panic!("{what}: wall {w} has no top"));
            let (a, b, bulge): ((f64, f64), (f64, f64), f64) = f
                .m
                .sql()
                .query_row("SELECT ax,ay,bx,by,bulge FROM wall WHERE id=?1", [w], |r| {
                    Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?), r.get(4)?))
                })
                .unwrap();
            let arc = (bulge != 0.0).then(|| ArcAxis::new(v2(a.0, a.1), v2(b.0, b.1), bulge, Tol::default()).unwrap());
            let len = arc.as_ref().map_or(((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt(), |x| x.length());
            let pt = |u: f64| match &arc {
                Some(x) => x.point_at(u.clamp(0.0, len)),
                None => {
                    let s = (u / len).clamp(0.0, 1.0);
                    v2(a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s)
                }
            };
            let (mut seen, mut total) = (0, 0);
            let mut u = 0.0;
            while u <= len + 1e-9 {
                total += 1;
                if let Some(z) = under(pt(u)) {
                    seen += 1;
                    let wz = at(&top, u);
                    let near: Vec<f64> = [1e-3, -1e-3].iter().filter_map(|d| under(pt(u + d))).collect();
                    let lo = near.iter().cloned().fold(z, f64::min);
                    let hi = near.iter().cloned().fold(z, f64::max);
                    assert!(wz <= hi + POKE, "{what}: wall {w} pokes through the roof at u={u:.2}: {wz} > {hi}");
                    assert!(lo - wz <= SHORT, "{what}: wall {w} stops {} m short of the roof at u={u:.2}", lo - wz);
                }
                u += 0.1;
            }
            assert!(seen * 10 >= total * 7, "{what}: wall {w}: only {seen} of {total} samples under the roof");
        }
    }

    #[test]
    fn arc_and_straight_walls_reach_every_planar_roof() {
        let shed = RoofKind::Arrow {
            ring: vec![(-0.4, -0.4), (10.4, -0.4), (10.4, 6.4), (-0.4, 6.4)],
            tail: (0.0, 0.0),
            head: (0.0, 6.0),
            rise: 1.5,
        };
        let barrel = RoofKind::Extrusion {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            profile: vec![(0.0, 0.0), (1.0, 1.2), (3.0, 2.4), (5.0, 2.8), (7.0, 2.4), (9.0, 1.2), (10.0, 0.0)],
            depth: 6.0,
        };
        let presets = [
            ("gable", footprint(vec![Some(30.0), None, Some(30.0), None]), 0.0),
            ("hip", footprint(vec![Some(30.0); 4]), 0.4),
            ("shed", shed, 0.0),
            ("barrel", barrel, 0.0),
            (
                "mansard",
                RoofKind::Footprint { ring: rect(10.0, 6.0), slopes: vec![Some(60.0); 4], mansard: Some((1.0, Some(20.0))) },
                0.3,
            ),
        ];
        for (name, kind, overhang) in presets {
            let mut f = fix();
            let mut walls = f.box_walls(10.0, 6.0, 3.0);
            walls.push(
                f.m
                    .add_arc_wall(&WallSpec { a: ARC_A, b: ARC_B, level: f.level, base_offset: 0.0, height: 3.0, wall_type: f.wtype, offset0: None }, BULGE)
                    .unwrap(),
            );
            let r = f.roof(kind, overhang);
            let g: Roof = f.m.roof_geometry(r).unwrap();
            check(&f, &walls, name, &|p| g.underside_at(p, T));
        }
    }

    fn roof_shell(f: &mut Fix, surface: Surface, side: ShellSide) -> i64 {
        let spec = FreeformSpec {
            level: f.level,
            name: "Host".into(),
            material: String::new(),
            surface,
            thickness: 0.0,
            side: ShellSide::Normal,
            trim: Trim::default(),
            origin: "points".into(),
        };
        let h = f.m.add_freeform(&spec).unwrap();
        let layers = vec![ShellLayer { name: "Cover".into(), material: "Cover".into(), thickness: 0.2 }];
        let s = f.m.shell_on_host(h, ShellKind::Roof, layers, side, vec![], "Shell roof").unwrap();
        f.m.add_shell_elem(&s).unwrap()
    }

    fn walls_in_the_span(f: &mut Fix) -> Vec<i64> {
        let mut w = vec![
            f.wall((1.0, 3.0), (9.0, 3.0), 6.0),
            f.wall((5.0, 1.5), (5.0, 4.5), 6.0),
            f.wall((3.0, 2.0), (7.0, 4.0), 6.0),
        ];
        w.push(
            f.m
                .add_arc_wall(&WallSpec { a: ARC_A, b: ARC_B, level: f.level, base_offset: 0.0, height: 6.0, wall_type: f.wtype, offset0: None }, BULGE)
                .unwrap(),
        );
        w
    }

    #[test]
    fn walls_reach_a_dome_shell() {
        let (r, rise) = (4.5, 2.0);
        let mut f = fix();
        let walls = walls_in_the_span(&mut f);
        roof_shell(&mut f, nurbs::dome(v3(5.0, 3.0, 3.0), r, rise).unwrap(), ShellSide::Normal);
        let rho = (r * r + rise * rise) / (2.0 * rise);
        let z = move |p: V2| {
            let d2 = (p.x - 5.0).powi(2) + (p.y - 3.0).powi(2);
            (d2 < (r - 0.05).powi(2)).then(|| 3.0 + rise - rho + (rho * rho - d2).sqrt())
        };
        check(&f, &walls, "dome", &z);
        openings_stay_under(&mut f, &walls, "dome");
    }

    /// The views draw a wall trimmed by a dome to its curve: the section cut and the elevation
    /// stop at the shell, the plan cuts the wall only where it is higher than the plane.
    #[test]
    fn views_draw_the_walls_under_a_dome_to_its_curve() {
        use crate::views::{LineClass, PlanViewSpec, ViewPrim};
        use crate::{SectionSpec, ViewKind};
        let (r, rise) = (4.5, 2.0);
        let rho = (r * r + rise * rise) / (2.0 * rise);
        let dome_z = |x: f64, y: f64| 3.0 + rise - rho + (rho * rho - (x - 5.0).powi(2) - (y - 3.0).powi(2)).sqrt();
        let mut f = fix();
        let walls = walls_in_the_span(&mut f);
        roof_shell(&mut f, nurbs::dome(v3(5.0, 3.0, 3.0), r, rise).unwrap(), ShellSide::Normal);
        // section across x = 4: the wall along y = 3 and the diagonal one are cut at the dome
        let spec = SectionSpec { kind: ViewKind::Section, name: "s".into(), a: (4.0, -1.0), b: (4.0, 7.0), depth: 10.0, far_clip: false };
        let v = f.m.section_view(&spec, &[]).unwrap();
        let cut_top = |w: i64| {
            v.prims
                .iter()
                .filter_map(|p| match p {
                    ViewPrim::SectionCut { table: "wall", element, rings, .. } if *element == w => Some(rings.iter().map(|q| q.1).fold(f64::MIN, f64::max)),
                    _ => None,
                })
                .fold(f64::MIN, f64::max)
        };
        for (w, y) in [(walls[0], 3.0), (walls[2], 2.5)] {
            let (got, want) = (cut_top(w), dome_z(4.0, y));
            assert!((got - want).abs() < 0.05, "wall {w}: section top {got} vs the dome {want}");
        }
        // elevation from the south: the wall along y = 3 rises to the apex, not to its 6 m
        let mut top = f64::MIN;
        for (a, b) in [((-1.0, -2.0), (11.0, -2.0)), ((11.0, -2.0), (-1.0, -2.0))] {
            let s = SectionSpec { kind: ViewKind::Elevation, name: "e".into(), a, b, depth: 10.0, far_clip: false };
            for p in &f.m.section_view(&s, &[]).unwrap().prims {
                if let ViewPrim::Edge { element, a, b, .. } = p {
                    if *element == walls[0] {
                        top = top.max(a.1).max(b.1);
                    }
                }
            }
        }
        assert!((top - dome_z(5.0, 3.0)).abs() < 0.03, "elevation top {top} vs apex {}", dome_z(5.0, 3.0));
        // plan cut at 4.95 m: the wall along y = 3 is cut only round the apex, the rest is beyond
        let cut = 4.95;
        // the dome reaches the plane at this distance from the apex
        let half = (rho * rho - (cut - 3.0 - rise + rho).powi(2)).sqrt();
        let v = f.m.plan_view(&PlanViewSpec { level: f.level, cut_height: cut, grids: vec![] }).unwrap();
        let xs: Vec<f64> = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::Cut { wall, ring, .. } if *wall == walls[0] => Some(ring.iter().map(|q| q.0).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(!xs.is_empty(), "the wall is cut round the apex");
        let (lo, hi) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
        assert!((lo - (5.0 - half)).abs() < 0.1 && (hi - (5.0 + half)).abs() < 0.1, "cut x {lo}..{hi}, apex span +-{half}");
        assert!(
            v.prims.iter().any(|p| matches!(p, ViewPrim::Edge { element, class: LineClass::Projection, .. } if *element == walls[0])),
            "its low ends are drawn beyond"
        );
    }

    #[test]
    fn walls_reach_a_free_form_saddle_shell() {
        // bilinear patch over the plan, not a dome: z = bilinear of the four corners
        let (z00, z10, z01, z11) = (3.0, 4.6, 4.0, 3.4);
        let mut f = fix();
        let walls = walls_in_the_span(&mut f);
        let rows = vec![
            vec![v3(0.0, 0.0, z00), v3(0.0, 6.0, z01)],
            vec![v3(10.0, 0.0, z10), v3(10.0, 6.0, z11)],
        ];
        let ctrl: Vec<_> = rows.into_iter().flatten().collect();
        let k = vec![0.0, 0.0, 1.0, 1.0];
        let surf = nurbs::Surface::new(1, 1, k.clone(), k, 2, 2, ctrl, vec![1.0; 4]).unwrap();
        assert!(surf.point(1.0, 0.0).x > surf.point(0.0, 0.0).x, "u runs along x");
        roof_shell(&mut f, surf, ShellSide::Normal);
        let z = move |p: V2| {
            let (s, t) = (p.x / 10.0, p.y / 6.0);
            (p.x > 0.05 && p.x < 9.95 && p.y > 0.05 && p.y < 5.95)
                .then(|| z00 * (1.0 - s) * (1.0 - t) + z10 * s * (1.0 - t) + z01 * (1.0 - s) * t + z11 * s * t)
        };
        check(&f, &walls, "saddle", &z);
        openings_stay_under(&mut f, &walls, "saddle");
    }

    /// Low museum shell: the ceiling over the wall ring is about 3 m, a little over the heads.
    const MUSEUM_NET: [[f64; 3]; 3] = [[2.6, 3.2, 2.7], [3.6, 4.4, 3.6], [2.7, 3.2, 2.6]];
    const MUSEUM_RING: [(f64, f64); 8] = [(1.0, 3.0), (2.5, 1.0), (5.0, 0.6), (7.5, 1.0), (9.0, 3.0), (7.5, 5.0), (5.0, 5.4), (2.5, 5.0)];

    /// The biquadratic shell over 10 x 6 m moved by `d`, and its height over a plan point.
    fn museum_shell(d: (f64, f64, f64)) -> (Surface, impl Fn(V2) -> Option<f64>) {
        let net = MUSEUM_NET;
        let bern = |t: f64| [(1.0 - t).powi(2), 2.0 * t * (1.0 - t), t * t];
        let z = move |p: V2| {
            let (x, y) = (p.x - d.0, p.y - d.1);
            let (bx, by) = (bern(x / 10.0), bern(y / 6.0));
            (x > 0.05 && x < 9.95 && y > 0.05 && y < 5.95)
                .then(|| d.2 + (0..3).flat_map(|i| (0..3).map(move |j| (i, j))).map(|(i, j)| bx[i] * by[j] * net[i][j]).sum::<f64>())
        };
        let ctrl: Vec<_> = (0..3)
            .flat_map(|i| (0..3).map(move |j| (i, j)))
            .map(|(i, j)| v3(5.0 * i as f64 + d.0, 3.0 * j as f64 + d.1, net[i][j] + d.2))
            .collect();
        let k = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let surf = nurbs::Surface::new(2, 2, k.clone(), k, 3, 3, ctrl, vec![1.0; 9]).unwrap();
        assert!(surf.point(1.0, 0.0).x > surf.point(0.0, 0.0).x, "u runs along x");
        (surf, z)
    }

    fn museum_walls(f: &mut Fix) -> Vec<i64> {
        let r = MUSEUM_RING;
        (0..r.len()).map(|i| f.wall(r[i], r[(i + 1) % r.len()], 7.0)).collect()
    }

    /// The museum: a doubly curved biquadratic shell over a closed ring of facet walls; the
    /// openings' heads stay under the trimmed tops, which are only a little higher.
    #[test]
    fn facet_walls_of_a_museum_reach_a_doubly_curved_shell_and_their_openings_stay_under_it() {
        let (surf, z) = museum_shell((0.0, 0.0, 0.0));
        let mut f = fix();
        let walls = museum_walls(&mut f);
        roof_shell(&mut f, surf, ShellSide::Normal);
        check(&f, &walls, "museum", &z);
        for &w in &walls {
            let (a, b): ((f64, f64), (f64, f64)) = f
                .m
                .sql()
                .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [w], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))))
                .unwrap();
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            for (kind, sill, h) in [("door", 0.0, 2.4), ("window", 1.2, 1.4)] {
                let along = if kind == "door" { 0.3 } else { len - 1.2 };
                f.m.add_opening(w, &crate::OpeningSpec { along, width: 0.9, sill, height: h, kind: kind.into() }).unwrap();
                let top = f.m.wall_top_profile(w).unwrap().unwrap();
                let lowest = (0..=9).map(|i| at(&top, along + 0.1 * i as f64)).fold(f64::MAX, f64::min);
                assert!(sill + h + 0.05 < lowest, "wall {w}: {kind} head {} meets the top {lowest}", sill + h);
                // the ceiling is low: an untrimmed or misplaced top would not pass here
                assert!(lowest < sill + h + 1.5, "wall {w}: {kind} top {lowest} is not trimmed down to the shell");
            }
        }
    }

    /// Moving the free-form host carries the shell roof, and the walls under it follow; undo
    /// brings the old tops back.
    #[test]
    fn museum_walls_follow_a_moved_free_form_shell() {
        let (surf, _) = museum_shell((0.0, 0.0, 0.0));
        let mut f = fix();
        let walls = museum_walls(&mut f);
        roof_shell(&mut f, surf, ShellSide::Normal);
        let before: Vec<_> = walls.iter().map(|&w| f.m.wall_top_profile(w).unwrap()).collect();
        let (host, mut spec) = f.m.freeforms(None).unwrap().remove(0);
        let d = (0.5, 0.0, 0.6);
        let (moved, z) = museum_shell(d);
        spec.surface = moved;
        f.m.update_freeform(host, &spec).unwrap();
        check(&f, &walls, "moved museum", &z);
        for (&w, old) in walls.iter().zip(&before) {
            let (old, new) = (old.as_ref().unwrap(), f.m.wall_top_profile(w).unwrap().unwrap());
            let (o, n) = (at(old, 0.5), at(&new, 0.5));
            assert!(n > o + 0.3, "wall {w}: top {o} -> {n} did not rise with the shell");
        }
        f.m.undo().unwrap();
        let after: Vec<_> = walls.iter().map(|&w| f.m.wall_top_profile(w).unwrap()).collect();
        assert_eq!(after, before);
    }

    #[test]
    fn walls_reach_a_barrel_shell() {
        let (span, rise) = (6.0, 2.0);
        let mut f = fix();
        let walls = walls_in_the_span(&mut f);
        roof_shell(&mut f, nurbs::vault(v3(0.0, 3.0, 3.0), v3(10.0, 3.0, 3.0), span, rise).unwrap(), ShellSide::Normal);
        let rho = (span * span / 4.0 + rise * rise) / (2.0 * rise);
        let z = move |p: V2| {
            let d = (p.y - 3.0).abs();
            (d < span / 2.0 - 0.05 && p.x > 0.05 && p.x < 9.95).then(|| 3.0 + rise - rho + (rho * rho - d * d).sqrt())
        };
        check(&f, &walls, "barrel", &z);
        openings_stay_under(&mut f, &walls, "barrel");
    }

    #[test]
    fn walls_reach_a_reversed_shell_stack() {
        // the stack hangs below the reference surface: the underside is 0.2 m under it
        let (r, rise) = (4.5, 2.0);
        let mut f = fix();
        let walls = walls_in_the_span(&mut f);
        roof_shell(&mut f, nurbs::dome(v3(5.0, 3.0, 3.0), r, rise).unwrap(), ShellSide::Reverse);
        let rho = (r * r + rise * rise) / (2.0 * rise);
        let z = move |p: V2| {
            let d2 = (p.x - 5.0).powi(2) + (p.y - 3.0).powi(2);
            let nz = (rho * rho - d2).sqrt() / rho;
            (d2 < (r - 0.05).powi(2)).then(|| 3.0 + rise - rho + (rho * rho - d2).sqrt() - 0.2 / nz)
        };
        check(&f, &walls, "reversed dome", &z);
    }
}
