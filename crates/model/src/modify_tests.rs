// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::{
    CurtainSpec, DimKind, DimSpec, LayerSpec, OpeningSpec, RoomSpec, SectionSpec, SlabSpec, Swing,
    ViewKind, WallSpec,
};
use std::f64::consts::{FRAC_PI_2, TAU};

/// Hash of the contents of all tables except the journal (rows sorted, rowid excluded).
fn db_hash(m: &Model) -> u64 {
    use std::hash::{Hash, Hasher};
    let db = m.sql();
    let mut tables: Vec<String> = {
        let mut st = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name<>'op'")
            .unwrap();
        let v = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    tables.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tables {
        let mut st = db.prepare(&format!("SELECT * FROM {t}")).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort();
        t.hash(&mut h);
        rows.hash(&mut h);
    }
    h.finish()
}

struct Fx {
    m: Model,
    lv: i64,
    wt: i64,
}

fn fx() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "C".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    Fx { m, lv, wt }
}

impl Fx {
    fn wall(&mut self, a: P, b: P) -> i64 {
        self.m
            .add_wall(&WallSpec {
                a,
                b,
                level: self.lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: self.wt,
                offset0: None,
            })
            .unwrap()
    }
    fn door(&mut self, wall: i64, along: f64) -> i64 {
        self.m
            .add_opening(
                wall,
                &OpeningSpec {
                    along,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap()
    }
    fn axis(&self, id: i64) -> (P, P) {
        self.m
            .sql()
            .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [id], |r| {
                Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
            })
            .unwrap()
    }
    fn along(&self, o: i64) -> (i64, f64) {
        self.m
            .sql()
            .query_row("SELECT wall,along FROM opening WHERE id=?1", [o], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }
    fn plan_area(&self, w: i64) -> f64 {
        self.m
            .sql()
            .query_row(
                "SELECT SUM(plan_area) FROM wall_qty WHERE wall=?1",
                [w],
                |r| r.get(0),
            )
            .unwrap()
    }
    /// A 6×4 room of four walls (axes) with a room space inside.
    fn room(&mut self) -> (Vec<i64>, i64) {
        let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        let ws: Vec<i64> = (0..4).map(|k| self.wall(c[k], c[(k + 1) % 4])).collect();
        let r = self
            .m
            .add_room(&RoomSpec {
                level: self.lv,
                name: "R".into(),
                number: "1".into(),
                point: (3.0, 2.0),
                height: 3.0,
            })
            .unwrap();
        (ws, r)
    }
    fn room_area(&self, r: i64) -> f64 {
        self.m.room_quantities(r).unwrap().unwrap().area
    }
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}
fn closep(a: P, b: P) {
    close(a.0, b.0);
    close(a.1, b.1);
}

#[test]
fn move_ten_walls_undo_restores_hash() {
    let mut f = fx();
    let ws: Vec<Target> = (0..10)
        .map(|k| Target::Wall(f.wall((0.0, k as f64), (5.0, k as f64))))
        .collect();
    f.door(1, 1.0);
    let h0 = db_hash(&f.m);
    f.m.move_elements(&ws, (1.25, -0.5)).unwrap();
    closep(f.axis(1).0, (1.25, -0.5));
    assert_ne!(db_hash(&f.m), h0);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn mirror_flips_door_hinge_geometrically() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (6.0, 0.0));
    let d = f.door(w, 1.0);
    f.m.set_opening_swing(
        d,
        Swing {
            side: false,
            hinge: false,
        },
    )
    .unwrap();
    // hinges at the opening start: point (1, 0); mirror about x = 10
    let out =
        f.m.mirror_elements(&[Target::Wall(w)], (10.0, 0.0), (10.0, 1.0), true)
            .unwrap();
    let Target::Wall(nw) = out[0] else { panic!() };
    assert_eq!(f.axis(nw), ((14.0, 0.0), (20.0, 0.0)));
    let nd: i64 =
        f.m.sql()
            .query_row("SELECT id FROM opening WHERE wall=?1", [nw], |r| r.get(0))
            .unwrap();
    let (_, along) = f.along(nd);
    close(along, 4.1); // 6 − 1 − 0.9
    let sw = f.m.opening_swing(nd).unwrap();
    assert_eq!(
        sw,
        Swing {
            side: false,
            hinge: true
        }
    );
    // hinges at the end: 14 + 4.1 + 0.9 = 19 = mirror of point 1
    close(14.0 + along + 0.9, 19.0);
    // the leaf opened toward +y (left of +x); the copy's axis is +x again and side is the same, so +y too
    let (a, b) = f.axis(nw);
    assert_eq!(left(unit(sub(b, a))), (-0.0, 1.0));
}

#[test]
fn rotate_90_keeps_areas() {
    let mut f = fx();
    let (ws, r) = f.room();
    let a0 = f.room_area(r);
    let wa0: f64 = ws.iter().map(|w| f.plan_area(*w)).sum();
    let mut ts: Vec<Target> = ws.iter().map(|w| Target::Wall(*w)).collect();
    ts.push(Target::Room(r));
    f.m.rotate_elements(&ts, (0.0, 0.0), FRAC_PI_2, false)
        .unwrap();
    let (a, b) = f.axis(ws[0]);
    closep(a, (0.0, 0.0));
    closep(b, (0.0, 6.0));
    close(f.room_area(r), a0);
    close(a0, 5.8 * 3.8);
    let wa: f64 = ws.iter().map(|w| f.plan_area(*w)).sum();
    close(wa, wa0);
}

#[test]
fn linear_array_five_exact_spacing() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (0.0, 3.0));
    let out =
        f.m.array_linear(&[Target::Wall(w)], 5, Spacing::Step((1.2, 0.0)))
            .unwrap();
    assert_eq!(out.len(), 4);
    for (k, t) in out.iter().enumerate() {
        let Target::Wall(id) = t else { panic!() };
        assert_eq!(f.axis(*id).0, ((k + 1) as f64 * 1.2, 0.0));
    }
    let fit =
        f.m.array_linear(&[Target::Wall(w)], 5, Spacing::Fit((0.0, 10.0)))
            .unwrap();
    let Target::Wall(last) = fit[3] else { panic!() };
    assert_eq!(f.axis(last).0, (0.0, 10.0));
    assert!(matches!(
        f.m.array_linear(&[Target::Wall(w)], 1, Spacing::Step((1.0, 0.0))),
        Err(Error::Modify(ModifyError::BadCount))
    ));
}

#[test]
fn radial_array_eight() {
    let mut f = fx();
    let w = f.wall((1.0, 0.0), (3.0, 0.0));
    let out =
        f.m.array_radial(&[Target::Wall(w)], 8, (0.0, 0.0), TAU)
            .unwrap();
    assert_eq!(out.len(), 7);
    for (k, t) in out.iter().enumerate() {
        let Target::Wall(id) = t else { panic!() };
        let ang = TAU / 8.0 * (k + 1) as f64;
        let (a, b) = f.axis(*id);
        closep(a, (ang.cos(), ang.sin()));
        closep(b, (3.0 * ang.cos(), 3.0 * ang.sin()));
        close(f.plan_area(*id), f.plan_area(w));
    }
}

#[test]
fn trim_corner_makes_l_join() {
    let mut f = fx();
    let a = f.wall((0.0, 0.0), (4.0, 0.0));
    let b = f.wall((5.0, 1.0), (5.0, 4.0));
    let h0 = db_hash(&f.m);
    f.m.trim_corner(a, b).unwrap();
    assert_eq!(f.axis(a), ((0.0, 0.0), (5.0, 0.0)));
    assert_eq!(f.axis(b), ((5.0, 0.0), (5.0, 4.0)));
    // L along axes 5 + 4, thickness 0.2: union area 5.1·0.2 + 3.9·0.2
    close(f.plan_area(a) + f.plan_area(b), 1.8);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
    let c = f.wall((0.0, 2.0), (4.0, 2.0));
    assert!(matches!(
        f.m.trim_corner(a, c),
        Err(Error::Modify(ModifyError::Parallel))
    ));
}

#[test]
fn trim_to_reference_and_opening_guard() {
    let mut f = fx();
    let r = f.wall((0.0, -1.0), (0.0, 5.0));
    let w = f.wall((1.0, 2.0), (6.0, 2.0));
    let d = f.door(w, 1.0);
    f.m.trim_to(w, Reference::Wall(r)).unwrap();
    assert_eq!(f.axis(w), ((0.0, 2.0), (6.0, 2.0)));
    close(f.along(d).1, 2.0); // the start moved by −1
                              // a trim that cuts off the door is an error with no partial state
    let h = db_hash(&f.m);
    let e = f.m.trim_to(w, Reference::Line((2.5, 0.0), (2.5, 1.0)));
    assert_eq!(
        e.unwrap_err().to_string(),
        ModifyError::OpeningOutside(d).to_string()
    );
    assert_eq!(db_hash(&f.m), h);
}

#[test]
fn split_at_point_keeps_door_on_right_and_rebinds_dims() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (6.0, 0.0));
    let d = f.door(w, 4.0);
    let dim =
        f.m.add_dim(&DimSpec {
            level: f.lv,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::AxisEnd { wall: w, end: 0 },
                DimRef::AxisEnd { wall: w, end: 1 },
            ],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 1.0,
            tier: 0,
            auto: false,
        })
        .unwrap();
    let h0 = db_hash(&f.m);
    let n = f.m.split_wall_at(w, (2.5, 0.05)).unwrap();
    close(f.m.wall_length(w).unwrap(), 2.5);
    close(f.m.wall_length(n).unwrap(), 3.5);
    assert_eq!(f.along(d).0, n);
    close(f.along(d).1, 1.5);
    let refs: String =
        f.m.sql()
            .query_row("SELECT refs FROM dim WHERE id=?1", [dim], |r| r.get(0))
            .unwrap();
    assert_eq!(refs, format!("X {w} 0;X {n} 1"));
    let v = f.m.dims(f.lv).unwrap();
    close(v.iter().find(|a| a.id == dim).unwrap().geom.value(), 6.0);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn offset_and_align() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (5.0, 0.0));
    let n = f.m.offset_wall(w, 1.5, true).unwrap();
    assert_eq!(f.axis(n), ((0.0, 1.5), (5.0, 1.5)));
    let n2 = f.m.offset_wall(w, 0.5, false).unwrap();
    assert_eq!(f.axis(n2), ((0.0, -0.5), (5.0, -0.5)));
    // face 0 (left, +0.1) onto the line y = 3
    f.m.align(
        AlignEdge::WallFace { wall: w, face: 0 },
        (0.0, 3.0),
        (1.0, 3.0),
    )
    .unwrap();
    closep(f.axis(w).0, (0.0, 2.9));
    assert!(matches!(
        f.m.align(AlignEdge::WallAxis(w), (0.0, 0.0), (1.0, 1.0)),
        Err(Error::Modify(ModifyError::NotParallel))
    ));
    let st =
        f.m.add_slab_type(
            "S",
            &[LayerSpec {
                material: "C".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let s =
        f.m.add_slab(&SlabSpec {
            level: f.lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)],
            holes: vec![],
        })
        .unwrap();
    f.m.offset_slab(s, 0.5).unwrap();
    let q = f.m.slab_quantities(s).unwrap();
    close(q[0].area, 5.0 * 4.0);
    f.m.align(
        AlignEdge::SlabEdge { slab: s, idx: 0 },
        (0.0, 10.0),
        (1.0, 10.0),
    )
    .unwrap();
    closep(f.m.slab_rings(s).unwrap()[0][0], (-0.5, 10.0));
    assert!(f.m.offset_slab(s, -2.5).is_err());
}

#[test]
fn arc_wall_rotate_mirror_and_offset() {
    let mut f = fx();
    let w = f.wall((1.0, 0.0), (-1.0, 0.0));
    f.m.set_wall_bulge(w, 1.0).unwrap(); // semicircle r = 1, through (0, 1)
    let l0 = f.m.wall_length(w).unwrap();
    let out =
        f.m.mirror_elements(&[Target::Wall(w)], (0.0, 0.0), (1.0, 0.0), true)
            .unwrap();
    let Target::Wall(m) = out[0] else { panic!() };
    close(f.m.wall_length(m).unwrap(), l0);
    close(f.plan_area(m), f.plan_area(w));
    let n = f.m.offset_wall(w, 0.5, true).unwrap(); // left of a counter-clockwise arc faces the centre
    close(f.m.wall_length(n).unwrap(), std::f64::consts::PI * 0.5);
}

#[test]
fn other_kinds_follow_transform() {
    let mut f = fx();
    let cw =
        f.m.add_curtain_wall(&CurtainSpec {
            level: f.lv,
            name: "CW".into(),
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: crate::curtain::GridLines::Spacing(1.0),
            v: crate::curtain::GridLines::Spacing(1.5),
            interior: crate::parse_section("r 0.05 0.1").unwrap(),
            border: crate::parse_section("r 0.05 0.1").unwrap(),
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    let v =
        f.m.add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "A".into(),
            a: (0.0, -1.0),
            b: (4.0, -1.0),
            depth: 5.0,
            far_clip: false,
        })
        .unwrap();
    let h0 = db_hash(&f.m);
    let ts = [Target::Curtain(cw), Target::View(v)];
    f.m.mirror_elements(&ts, (0.0, 2.0), (1.0, 2.0), false)
        .unwrap();
    let s = f.m.view_def(v).unwrap();
    assert_eq!((s.a, s.b), ((4.0, 5.0), (0.0, 5.0)));
    let c = f.m.curtain_wall(cw).unwrap();
    assert_eq!((c.a, c.b), ((0.0, 4.0), (4.0, 4.0)));
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
    let copies = f.m.copy_elements(&ts, &[(0.0, 10.0), (0.0, 20.0)]).unwrap();
    assert_eq!(copies.len(), 4);
}

/// Random operation sequences followed by a full undo give the original hash;
/// a failed operation does not change the state.
#[test]
fn random_sequences_undo_to_original() {
    let mut f = fx();
    let (ws, r) = f.room();
    f.door(ws[0], 1.0);
    f.door(ws[2], 2.0);
    let st =
        f.m.add_slab_type(
            "S",
            &[LayerSpec {
                material: "C".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let s =
        f.m.add_slab(&SlabSpec {
            level: f.lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            holes: vec![],
        })
        .unwrap();
    let h0 = db_hash(&f.m);
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let all: Vec<Target> = ws
        .iter()
        .map(|w| Target::Wall(*w))
        .chain([Target::Room(r), Target::Slab(s), Target::Opening(1)])
        .collect();
    for _ in 0..2000 {
        let mut done = 0;
        for _ in 0..3 {
            let k = rnd();
            let t = all[(k % all.len() as u64) as usize];
            let x = ((k >> 8) % 7) as f64 - 3.0;
            let y = ((k >> 16) % 5) as f64 - 2.0;
            let w = ws[((k >> 24) % 4) as usize];
            let w2 = ws[((k >> 28) % 4) as usize];
            let before = db_hash(&f.m);
            let res = match (k >> 32) % 9 {
                0 => f.m.move_elements(&[t], (x * 0.1, y * 0.1)).map(|_| ()),
                1 => f.m.copy_elements(&[t], &[(x, y)]).map(|_| ()),
                2 => {
                    f.m.rotate_elements(&[t], (x, y), x * 0.3, k & 1 == 0)
                        .map(|_| ())
                }
                3 => {
                    f.m.mirror_elements(&[t], (x, 0.0), (x + y, 1.0), k & 1 == 0)
                        .map(|_| ())
                }
                4 => f.m.array_linear(&[t], 3, Spacing::Step((x, y))).map(|_| ()),
                5 => f.m.trim_corner(w, w2),
                6 => f.m.split_wall_at(w, (x.abs(), y.abs())).map(|_| ()),
                7 => f.m.offset_wall(w, 0.3 + x.abs(), k & 1 == 0).map(|_| ()),
                _ => f.m.array_radial(&[t], 3, (x, y), TAU).map(|_| ()),
            };
            match res {
                Ok(()) => done += 1,
                Err(_) => assert_eq!(db_hash(&f.m), before, "failed op left state"),
            }
        }
        for _ in 0..done {
            f.m.undo().unwrap();
        }
        assert_eq!(db_hash(&f.m), h0);
    }
}

fn hosted(f: &Fx, wall: i64, x: f64) -> i64 {
    f.m.sql()
        .execute(
            "INSERT INTO comp(level,ctype,x,y,z,rotation,mirror,host_wall,vals) \
             VALUES (?1,1,?2,0,0,0,0,?3,'{}')",
            rusqlite::params![f.lv, x, wall],
        )
        .unwrap();
    f.m.sql().last_insert_rowid()
}

fn comp_at(f: &Fx, c: i64) -> (i64, f64) {
    f.m.sql()
        .query_row("SELECT host_wall,x FROM comp WHERE id=?1", [c], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
}

#[test]
fn split_moves_hosted_components_and_undo_restores() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (6.0, 0.0));
    let (c1, c2) = (hosted(&f, w, 1.0), hosted(&f, w, 4.5));
    let h0 = db_hash(&f.m);
    let n = f.m.split_wall_at(w, (2.5, 0.0)).unwrap();
    assert_eq!(comp_at(&f, c1), (w, 1.0));
    let (hw, x) = comp_at(&f, c2);
    assert_eq!(hw, n);
    close(x, 2.0);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn trim_rebases_hosted_components_and_guards_bounds() {
    let mut f = fx();
    let r = f.wall((0.0, -1.0), (0.0, 5.0));
    let w = f.wall((1.0, 2.0), (6.0, 2.0));
    let c = hosted(&f, w, 3.0);
    f.m.trim_to(w, Reference::Wall(r)).unwrap();
    close(comp_at(&f, c).1, 4.0); // the start moved by −1
    let h = db_hash(&f.m);
    let e = f.m.trim_to(w, Reference::Line((3.5, 0.0), (3.5, 1.0)));
    assert_eq!(
        e.unwrap_err().to_string(),
        ModifyError::ComponentOutside(c).to_string()
    );
    assert_eq!(db_hash(&f.m), h);
}

fn wall_row_of(f: &Fx, w: i64) -> (i64, f64, f64, Option<f64>, f64) {
    f.m.sql()
        .query_row(
            "SELECT wtype,height,base_offset,offset0,bulge FROM wall WHERE id=?1",
            [w],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
}

fn slab_fx(f: &mut Fx, outer: Vec<P>) -> i64 {
    let st =
        f.m.add_slab_type(
            "S",
            &[LayerSpec {
                material: "C".into(),
                thickness: 0.25,
            }],
        )
        .unwrap();
    f.m.add_slab(&SlabSpec {
        level: f.lv,
        slab_type: st,
        offset: 0.1,
        outer,
        holes: vec![],
    })
    .unwrap()
}

#[test]
fn scale_walls_keeps_thickness() {
    let mut f = fx();
    let w = f.wall((1.0, 1.0), (5.0, 1.0));
    let arc = f.wall((1.0, 3.0), (5.0, 3.0));
    f.m.set_wall_bulge(arc, 0.25).unwrap();
    let l0 = f.m.wall_length(arc).unwrap();
    let t0 = wall_thickness(f.m.sql(), w).unwrap();
    let (r0, a0) = (wall_row_of(&f, w), wall_row_of(&f, arc));
    let h0 = db_hash(&f.m);
    let out =
        f.m.scale_elements(&[Target::Wall(w), Target::Wall(arc)], (1.0, 1.0), 2.0, false)
            .unwrap();
    assert!(out.is_empty());
    assert_eq!(f.axis(w), ((1.0, 1.0), (9.0, 1.0)));
    assert_eq!(f.axis(arc), ((1.0, 5.0), (9.0, 5.0)));
    // thickness, type, height, base offset and location line stay; the bulge is scale free,
    // so the arc radius doubles with its chord
    assert_eq!(wall_thickness(f.m.sql(), w).unwrap(), t0);
    assert_eq!(wall_row_of(&f, w), r0);
    assert_eq!(wall_row_of(&f, arc), a0);
    close(f.m.wall_length(arc).unwrap(), 2.0 * l0);
    // one gesture, one undo step
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn scale_slab_ring_about_point() {
    let mut f = fx();
    let s = slab_fx(&mut f, vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)]);
    let ty = |f: &Fx| -> (i64, f64) {
        f.m.sql()
            .query_row("SELECT stype,offset FROM slab WHERE id=?1", [s], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    };
    let before = ty(&f);
    let h0 = db_hash(&f.m);
    f.m.scale_elements(&[Target::Slab(s)], (2.0, 1.0), 0.5, false)
        .unwrap();
    let rings = f.m.slab_rings(s).unwrap();
    let want = [(1.0, 0.5), (3.0, 0.5), (3.0, 2.0), (1.0, 2.0)];
    assert_eq!(rings[0].len(), 4);
    for (p, q) in rings[0].iter().zip(want) {
        closep(*p, q);
    }
    // the type (thickness) and the height offset stay
    assert_eq!(ty(&f), before);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn scale_openings_follow_host() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (6.0, 0.0));
    let d = f.door(w, 1.0);
    let c = hosted(&f, w, 3.0);
    let size = |f: &Fx| -> (f64, f64, f64) {
        f.m.sql()
            .query_row(
                "SELECT width,sill,height FROM opening WHERE id=?1",
                [d],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    let s0 = size(&f);
    let h0 = db_hash(&f.m);
    f.m.scale_elements(&[Target::Wall(w)], (0.0, 0.0), 1.5, false)
        .unwrap();
    assert_eq!(f.axis(w), ((0.0, 0.0), (9.0, 0.0)));
    let (host, along) = f.along(d);
    assert_eq!(host, w);
    close(along, 1.5);
    assert_eq!(size(&f), s0, "opening sizes are type sizes");
    let (ch, cx) = comp_at(&f, c);
    assert_eq!(ch, w);
    close(cx, 4.5);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
    // shrunk below the door: refused with the reason, nothing changes
    let e = f.m.scale_elements(&[Target::Wall(w)], (0.0, 0.0), 0.15, false);
    assert_eq!(
        e.unwrap_err().to_string(),
        Error::Modify(ModifyError::OpeningOutside(d)).to_string()
    );
    assert_eq!(db_hash(&f.m), h0);
    // an opening or a hosted component alone has no scale of its own
    for t in [Target::Opening(d), Target::Component(c)] {
        assert!(matches!(
            f.m.scale_elements(&[t], (0.0, 0.0), 2.0, false),
            Err(Error::Modify(ModifyError::Unsupported(..)))
        ));
    }
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn scale_copy_returns_targets() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (4.0, 0.0));
    let d = f.door(w, 1.0);
    let s = slab_fx(&mut f, vec![(0.0, 1.0), (2.0, 1.0), (2.0, 2.0), (0.0, 2.0)]);
    let h0 = db_hash(&f.m);
    let out =
        f.m.scale_elements(&[Target::Wall(w), Target::Slab(s)], (0.0, -1.0), 3.0, true)
            .unwrap();
    assert_eq!(out.len(), 2);
    let Some(&Target::Wall(nw)) = out.iter().find(|t| matches!(t, Target::Wall(_))) else {
        panic!("{out:?}")
    };
    let Some(&Target::Slab(ns)) = out.iter().find(|t| matches!(t, Target::Slab(_))) else {
        panic!("{out:?}")
    };
    assert!(nw != w && ns != s);
    // originals stay where they were
    assert_eq!(f.axis(w), ((0.0, 0.0), (4.0, 0.0)));
    close(f.along(d).1, 1.0);
    assert_eq!(f.axis(nw), ((0.0, 2.0), (12.0, 2.0)));
    let nd: i64 =
        f.m.sql()
            .query_row("SELECT id FROM opening WHERE wall=?1", [nw], |r| r.get(0))
            .unwrap();
    close(f.along(nd).1, 3.0);
    closep(f.m.slab_rings(ns).unwrap()[0][2], (6.0, 8.0));
    closep(f.m.slab_rings(s).unwrap()[0][2], (2.0, 2.0));
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn scale_refuses_nonpositive_factor() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (4.0, 0.0));
    let h0 = db_hash(&f.m);
    for k in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY] {
        for copy in [false, true] {
            let e = f.m.scale_elements(&[Target::Wall(w)], (0.0, 0.0), k, copy);
            assert!(matches!(e, Err(Error::Invalid(_))), "{k}: {e:?}");
        }
    }
    assert_eq!(db_hash(&f.m), h0);
    assert!(f.m.scale_elements(&[], (0.0, 0.0), 2.0, false).is_err());
    assert_eq!(db_hash(&f.m), h0);
    // the refusals left no undo step: one undo takes back the wall itself
    f.m.undo().unwrap();
    assert_eq!(
        f.m.sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn scale_refuses_groups_and_pinned() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (4.0, 0.0));
    let g = f.wall((0.0, 2.0), (4.0, 2.0));
    f.m.group_create("G", &[("wall", g)]).unwrap();
    f.m.pin(&[("wall", w)]).unwrap();
    let h0 = db_hash(&f.m);
    let e = f.m.scale_elements(&[Target::Wall(g)], (0.0, 0.0), 2.0, false);
    assert!(e.unwrap_err().to_string().contains("ungroup"));
    assert!(f.m.scale_elements(&[Target::Wall(w)], (0.0, 0.0), 2.0, false).is_err());
    // a mixed selection is refused whole
    let both = [Target::Wall(w), Target::Wall(g)];
    assert!(f.m.scale_elements(&both, (0.0, 0.0), 2.0, false).is_err());
    assert_eq!(db_hash(&f.m), h0);
    assert_eq!(f.axis(w), ((0.0, 0.0), (4.0, 0.0)));
    // a scaled copy of a group member is a free element
    let out = f.m.scale_elements(&[Target::Wall(g)], (0.0, 0.0), 2.0, true).unwrap();
    assert_eq!(out.len(), 1);
}

#[test]
fn scale_detail_line_points_keep_sizes() {
    let line = crate::AnnoShape::Line {
        pts: vec![(1.0, 1.0), (3.0, 2.0)],
        closed: false,
    };
    let s = line.transformed(crate::AnnoXf::Scale { o: (1.0, 1.0), k: 2.0 });
    let crate::AnnoShape::Line { pts, .. } = s else { panic!() };
    closep(pts[0], (1.0, 1.0));
    closep(pts[1], (5.0, 3.0));
    let bad = line.transformed(crate::AnnoXf::Scale { o: (0.0, 0.0), k: 0.0 });
    let crate::AnnoShape::Line { pts, .. } = bad else { panic!() };
    assert!(pts.iter().all(|p| p.0.is_nan()), "the shape check refuses it");
}

#[test]
fn tr1_trim_keeps_clicked_side() {
    let mut f = fx();
    let r = f.wall((3.0, -1.0), (3.0, 5.0));
    let w = f.wall((0.0, 2.0), (6.0, 2.0));
    let d = f.door(w, 1.0);
    f.m.trim_to_at(w, Reference::Wall(r), Some((1.0, 2.0))).unwrap();
    assert_eq!(f.axis(w), ((0.0, 2.0), (3.0, 2.0)));
    close(f.along(d).1, 1.0);
    f.m.undo().unwrap();
    assert_eq!(f.axis(w), ((0.0, 2.0), (6.0, 2.0)));
}

#[test]
fn tr1_click_far_side_keeps_far_part() {
    let mut f = fx();
    let r = f.wall((3.0, -1.0), (3.0, 5.0));
    let w = f.wall((0.0, 2.0), (6.0, 2.0));
    // click nearer to end a than the boundary would move the near end: the far part must stay
    f.m.trim_to_at(w, Reference::Wall(r), Some((5.0, 2.0))).unwrap();
    assert_eq!(f.axis(w), ((3.0, 2.0), (6.0, 2.0)));
    // click exactly on the boundary is refused and changes nothing
    let h = db_hash(&f.m);
    let e = f.m.trim_to_at(w, Reference::Line((4.0, 0.0), (4.0, 1.0)), Some((4.0, 2.0)));
    assert_eq!(e.unwrap_err().to_string(), ModifyError::OnBoundary.to_string());
    assert_eq!(db_hash(&f.m), h);
}

#[test]
fn tr2_extend_to_boundary() {
    let mut f = fx();
    let r = f.wall((0.0, -1.0), (0.0, 5.0));
    let w = f.wall((1.0, 2.0), (6.0, 2.0));
    f.m.trim_to_at(w, Reference::Wall(r), Some((1.5, 2.0))).unwrap();
    assert_eq!(f.axis(w), ((0.0, 2.0), (6.0, 2.0)));
}

#[test]
fn trim_corner_keeps_clicked_parts_when_crossing() {
    let mut f = fx();
    let a = f.wall((0.0, 2.0), (6.0, 2.0));
    let b = f.wall((3.0, 0.0), (3.0, 5.0));
    f.m.trim_corner_at(a, b, Some((1.0, 2.0)), Some((3.0, 4.0))).unwrap();
    assert_eq!(f.axis(a), ((0.0, 2.0), (3.0, 2.0)));
    assert_eq!(f.axis(b), ((3.0, 2.0), (3.0, 5.0)));
    f.m.undo().unwrap();
    assert_eq!(f.axis(a), ((0.0, 2.0), (6.0, 2.0)));
    assert_eq!(f.axis(b), ((3.0, 0.0), (3.0, 5.0)));
}

#[test]
fn trim_corner_extends_short_walls_unchanged() {
    let mut f = fx();
    let a = f.wall((0.0, 0.0), (2.0, 0.0));
    let b = f.wall((4.0, 1.0), (4.0, 3.0));
    f.m.trim_corner_at(a, b, Some((1.0, 0.0)), Some((4.0, 2.0))).unwrap();
    assert_eq!(f.axis(a), ((0.0, 0.0), (4.0, 0.0)));
    assert_eq!(f.axis(b), ((4.0, 0.0), (4.0, 3.0)));
    let c = f.wall((0.0, 5.0), (2.0, 5.0));
    let d = f.wall((4.0, 6.0), (4.0, 8.0));
    f.m.trim_corner(c, d).unwrap();
    assert_eq!(f.axis(c), ((0.0, 5.0), (4.0, 5.0)));
    assert_eq!(f.axis(d), ((4.0, 5.0), (4.0, 8.0)));
}

#[test]
fn trim_corner_click_on_corner_refused() {
    let mut f = fx();
    let a = f.wall((0.0, 2.0), (6.0, 2.0));
    let b = f.wall((3.0, 0.0), (3.0, 5.0));
    let h = db_hash(&f.m);
    let e = f.m.trim_corner_at(a, b, Some((1.0, 2.0)), Some((3.0, 2.0)));
    assert_eq!(e.unwrap_err().to_string(), ModifyError::OnBoundary.to_string());
    assert_eq!(db_hash(&f.m), h);
}

#[test]
fn divide_wall_into_equal_parts_is_one_undo() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (6.0, 0.0));
    let d = f.door(w, 4.5);
    let h0 = db_hash(&f.m);
    let parts = f.m.divide_wall(w, 3).unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], w);
    for p in &parts {
        close(f.m.wall_length(*p).unwrap(), 2.0);
    }
    assert_eq!(f.along(d).0, parts[2]);
    close(f.along(d).1, 0.5);
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), h0);
}

#[test]
fn divide_wall_rejects_bad_counts_without_change() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (6.0, 0.0));
    let h0 = db_hash(&f.m);
    assert!(f.m.divide_wall(w, 1).is_err());
    assert!(f.m.divide_wall(w, MAX_DIVIDE + 1).is_err());
    assert_eq!(db_hash(&f.m), h0);
}

fn level_of(f: &Fx, wall: i64) -> (i64, f64) {
    f.m.sql()
        .query_row("SELECT level, base_offset FROM wall WHERE id=?1", [wall], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
}

#[test]
fn a_copy_that_rises_lands_on_the_level_there_or_gains_the_offset_and_undoes() {
    let mut f = fx();
    let up = f.m.add_level("L2", 3.0).unwrap();
    let w = f.wall((0.0, 0.0), (5.0, 0.0));
    let d = f.door(w, 2.0);
    let before = db_hash(&f.m);
    // one storey up: onto the level at 3 m, hosted door along
    let made = f.m.copy_elements_z(&[Target::Wall(w)], &[(0.0, 0.0, 3.0)]).unwrap();
    let Some(Target::Wall(c)) = made.iter().copied().find(|t| matches!(t, Target::Wall(_))) else {
        panic!("{made:?}")
    };
    assert_eq!(level_of(&f, c), (up, 0.0));
    assert_eq!(level_of(&f, w), (f.lv, 0.0), "the original stays");
    assert_eq!(f.axis(c), f.axis(w));
    assert_eq!(f.m.sql().query_row("SELECT count(*) FROM opening", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    let _ = d;
    // no level at 1.5 m: the copy keeps its level and gains the offset
    let made = f.m.copy_elements_z(&[Target::Wall(w)], &[(1.0, 0.0, 1.5)]).unwrap();
    let Some(Target::Wall(c2)) = made.iter().copied().find(|t| matches!(t, Target::Wall(_))) else {
        panic!("{made:?}")
    };
    assert_eq!(level_of(&f, c2), (f.lv, 1.5));
    f.m.undo().unwrap();
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), before, "each rising copy is one undo step");
}

#[test]
fn a_rising_copy_refuses_a_lone_door_and_lifts_by_offset_without_a_level() {
    let mut f = fx();
    let w = f.wall((0.0, 0.0), (5.0, 0.0));
    let d = f.door(w, 2.0);
    // a door without its wall would land on the same wall again
    let e = f.m.copy_elements_z(&[Target::Opening(d)], &[(0.0, 0.0, 3.0)]).unwrap_err();
    assert!(e.to_string().contains("select the wall"), "{e}");
    // no level above: the wall gains its offsets
    let made = f.m.copy_elements_z(&[Target::Wall(w)], &[(0.0, 2.0, 3.0)]).unwrap();
    let Some(Target::Wall(c)) = made.iter().copied().find(|t| matches!(t, Target::Wall(_))) else {
        panic!("{made:?}")
    };
    assert_eq!(level_of(&f, c), (f.lv, 3.0));
}

fn one_f(f: &Fx, sql: &str, id: i64) -> f64 {
    f.m.sql().query_row(sql, [id], |r| r.get(0)).unwrap()
}

#[test]
fn a_rising_copy_of_a_curtain_wall_lands_on_the_level_or_gains_the_offset_and_undoes() {
    use crate::{CurtainSpec, GridLines};
    let mut f = fx();
    let up = f.m.add_level("L2", 3.0).unwrap();
    let sec = crate::parse_section("r 0.05 0.1").unwrap();
    let cw = f
        .m
        .add_curtain_wall(&CurtainSpec {
            level: f.lv,
            name: "CW".into(),
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: GridLines::Spacing(1.0),
            v: GridLines::Spacing(1.5),
            interior: sec,
            border: sec,
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    let before = db_hash(&f.m);
    // two offsets, only +3 has a level: one copy on L2, one on L1 raised by 6
    let made = f.m.copy_elements_z(&[Target::Curtain(cw)], &[(0.0, 5.0, 3.0), (0.0, 6.0, 6.0)]).unwrap();
    let ids: Vec<i64> = made.iter().filter_map(|t| if let Target::Curtain(i) = t { Some(*i) } else { None }).collect();
    assert_eq!(ids.len(), 2);
    let row = |id| -> (i64, f64) {
        f.m.sql()
            .query_row("SELECT level, base_offset FROM curtain_wall WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    };
    assert_eq!(row(ids[0]), (up, 0.0));
    assert_eq!(row(ids[1]), (f.lv, 6.0));
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), before);
}

#[test]
fn a_rising_copy_lifts_beams_runs_and_components() {
    use crate::{BeamJoin, BeamSpec, CompSpec, Just, MemberShape, PipeMaterial, RunSpec, SystemKind};
    let mut f = fx();
    let mt = f.m.add_member_type("B", &MemberShape::Rect { b: 0.2, h: 0.4 }, "Concrete").unwrap();
    let beam = f
        .m
        .add_beam(&BeamSpec {
            level: f.lv,
            member_type: mt,
            a: (0.0, 0.0, 3.0),
            b: (4.0, 0.0, 3.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    let sys = f.m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let run = f
        .m
        .add_mep_run(&RunSpec::pipe(sys, f.lv, PipeMaterial::Copper, 15, vec![(0.0, 2.0, 1.0), (2.0, 2.0, 1.0)]).unwrap())
        .unwrap();
    let family = f.m.load_library_family("Sink").unwrap();
    let ctype = f.m.comp_types(family).unwrap()[0].0;
    let comp = f.m.add_component(&CompSpec::at(f.lv, ctype, 1.0, 3.0)).unwrap();
    let z0 = one_f(&f, "SELECT z FROM comp WHERE id=?1", comp);
    let before = db_hash(&f.m);
    let made = f
        .m
        .copy_elements_z(&[Target::Beam(beam), Target::MepRun(run), Target::Component(comp)], &[(0.0, 4.0, 2.5)])
        .unwrap();
    assert_eq!(made.len(), 3);
    for t in made {
        match t {
            Target::Beam(b) => {
                assert_eq!(one_f(&f, "SELECT az FROM beam WHERE id=?1", b), 5.5);
                assert_eq!(one_f(&f, "SELECT bz FROM beam WHERE id=?1", b), 5.5);
            }
            Target::MepRun(r) => {
                let text: String = f.m.sql().query_row("SELECT points FROM mep_run WHERE id=?1", [r], |x| x.get(0)).unwrap();
                let pts = crate::mep::parse_points(&text).unwrap();
                assert!(pts.iter().all(|p| (p.2 - 3.5).abs() < 1e-9), "{pts:?}");
            }
            Target::Component(c) => assert!((one_f(&f, "SELECT z FROM comp WHERE id=?1", c) - z0 - 2.5).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
    }
    f.m.undo().unwrap();
    assert_eq!(db_hash(&f.m), before);
}
