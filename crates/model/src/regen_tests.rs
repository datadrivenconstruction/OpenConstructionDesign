// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Partial regeneration equals full regeneration: random edits of all kinds with undo, and after
//! each one the derived tables are compared with a full regeneration (`set_verify`).
//! `DDC_REGEN_SEQS` is the number of sequences. The single-edit timing on 10k walls / 500
//! columns runs with `--ignored`.

use std::time::Instant;

use super::{diff, near_passes, set_verify, snapshot, stats};
use crate::{
    BeamJoin, BeamSpec, ColumnSpec, DimKind, DimRef, DimSpec, LayerSpec, Model, OpeningSpec,
    RoomSpec, SepSpec, ShaftSpec, SlabSpec, StairSpec, Target, WallSpec,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() % 10_000) as f64 / 10_000.0
    }
}

struct Base {
    lv: [i64; 3],
    wt: i64,
    st: i64,
    ct: i64,
    bt: i64,
}

fn base() -> (Model, Base) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = [
        m.add_level("L1", 0.0).unwrap(),
        m.add_level("L2", 3.0).unwrap(),
        m.add_level("L3", 6.0).unwrap(),
    ];
    let lay = |t| LayerSpec {
        material: "Concrete".into(),
        thickness: t,
    };
    let wt = m.add_wall_type("W", &[lay(0.2), lay(0.05)]).unwrap();
    let st = m.add_slab_type("S", &[lay(0.2)]).unwrap();
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let bt = m.add_catalog_member_type("IPE 300").unwrap();
    (m, Base { lv, wt, st, ct, bt })
}

fn ids(m: &Model, table: &str) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}

fn pick(r: &mut Rng, v: &[i64]) -> Option<i64> {
    (!v.is_empty()).then(|| v[r.below(v.len())])
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

/// A point on a one-metre grid: walls often meet and close rooms.
fn node(r: &mut Rng) -> (f64, f64) {
    (r.below(13) as f64 - 6.0, r.below(13) as f64 - 6.0)
}

fn wall(r: &mut Rng, b: &Base) -> WallSpec {
    let a = node(r);
    let len = (2 + r.below(6)) as f64;
    let bb = if r.below(2) == 0 {
        (a.0 + len, a.1)
    } else {
        (a.0, a.1 + len)
    };
    WallSpec {
        a,
        b: bb,
        level: b.lv[r.below(2)],
        base_offset: 0.0,
        height: 3.0,
        wall_type: b.wt,
        offset0: None,
    }
}

fn column(r: &mut Rng, b: &Base) -> ColumnSpec {
    let k = r.below(2);
    ColumnSpec {
        level: b.lv[k],
        top_level: Some(b.lv[k + 1]),
        member_type: b.ct,
        structural: r.below(2) == 0,
        at: node(r),
        base_offset: 0.0,
        top_offset: r.f(-0.5, 0.0),
        rotation: r.f(0.0, 1.5),
        top_at: None,
    }
}

fn beam(r: &mut Rng, b: &Base) -> BeamSpec {
    let a = node(r);
    // on the grid, endpoints often land on the axes of other beams (T joints)
    let e = match r.below(3) {
        0 => (a.0 + (1 + r.below(5)) as f64, a.1),
        1 => (a.0, a.1 + (1 + r.below(5)) as f64),
        _ => (a.0 + r.f(1.0, 6.0), a.1 + r.f(-2.0, 2.0)),
    };
    BeamSpec {
        level: b.lv[1 + r.below(2)],
        member_type: b.bt,
        a: (a.0, a.1, -0.1),
        b: (e.0, e.1, -0.1),
        bulge: 0.0,
        yjust: Default::default(),
        zjust: Default::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        // trimming the flanges yields a joint deduction that partial regeneration must preserve
        join_end: [BeamJoin::Auto, BeamJoin::Butt, BeamJoin::Cope][r.below(3)],
    }
}

fn slab_ring(r: &mut Rng) -> Vec<(f64, f64)> {
    let (x, y) = node(r);
    rect(x, y, r.f(2.0, 8.0), r.f(2.0, 8.0))
}

fn opening(r: &mut Rng) -> OpeningSpec {
    OpeningSpec {
        along: r.f(0.3, 0.8),
        width: r.f(0.5, 0.9),
        sill: if r.below(2) == 0 { 0.0 } else { 0.9 },
        height: 1.2,
        kind: if r.below(2) == 0 { "door" } else { "window" }.into(),
    }
}

fn room(r: &mut Rng, b: &Base) -> RoomSpec {
    let (x, y) = node(r);
    RoomSpec {
        level: b.lv[r.below(2)],
        name: "R".into(),
        number: format!("{}", r.below(100)),
        point: (x + 0.5, y + 0.5),
        height: r.f(2.5, 3.0),
    }
}

fn dim(r: &mut Rng, b: &Base, w: i64) -> DimSpec {
    DimSpec {
        level: b.lv[0],
        kind: DimKind::Linear,
        refs: vec![
            DimRef::WallEnd {
                wall: w,
                face: 0,
                end: 0,
            },
            DimRef::WallEnd {
                wall: w,
                face: 0,
                end: 1,
            },
        ],
        dir: (1.0, 0.0),
        side: 1.0,
        offset: r.f(0.3, 1.5),
        tier: 0,
        auto: false,
    }
}

/// One random edit (a model rejection is also an outcome: the transaction rolls back).
fn step(r: &mut Rng, m: &mut Model, b: &Base) {
    let walls = ids(m, "wall");
    let any = |r: &mut Rng, m: &Model, t: &str| pick(r, &ids(m, t));
    let _ = match r.below(30) {
        0..=2 => m.add_wall(&wall(r, b)).map(drop),
        3 => match pick(r, &walls) {
            Some(w) => m.delete_wall(w),
            None => Ok(()),
        },
        4 => match pick(r, &walls) {
            Some(w) => {
                let s = wall(r, b);
                m.set_wall_axis(w, s.a, s.b)
            }
            None => Ok(()),
        },
        5 => match pick(r, &walls) {
            Some(w) => m.add_opening(w, &opening(r)).map(drop),
            None => Ok(()),
        },
        6 => match any(r, m, "opening") {
            Some(o) => m.update_opening(o, &opening(r)),
            None => Ok(()),
        },
        7 => match any(r, m, "opening") {
            Some(o) => m.delete_opening(o),
            None => Ok(()),
        },
        8 => match pick(r, &walls) {
            Some(w) => {
                let mut t = vec![Target::Wall(w)];
                if let Some(s) = any(r, m, "slab") {
                    t.push(Target::Slab(s));
                }
                m.move_elements(&t, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))
            }
            None => Ok(()),
        },
        9 | 10 => m.add_column(&column(r, b)).map(drop),
        11 => match any(r, m, "col") {
            Some(c) => m.update_column(c, &column(r, b)),
            None => Ok(()),
        },
        12 => match any(r, m, "col") {
            Some(c) => m.delete_column(c),
            None => Ok(()),
        },
        13 => m.add_beam(&beam(r, b)).map(drop),
        14 => match any(r, m, "beam") {
            Some(c) => m.update_beam(c, &beam(r, b)),
            None => Ok(()),
        },
        15 => match any(r, m, "beam") {
            Some(c) => m.delete_beam(c),
            None => Ok(()),
        },
        16 => m
            .add_slab(&SlabSpec {
                level: b.lv[r.below(3)],
                slab_type: b.st,
                offset: if r.below(2) == 0 { 0.0 } else { -0.1 },
                outer: slab_ring(r),
                holes: vec![],
            })
            .map(drop),
        17 => match any(r, m, "slab") {
            Some(s) => m.set_slab_boundary(s, &slab_ring(r), &[]),
            None => Ok(()),
        },
        18 => match any(r, m, "slab") {
            Some(s) => m.delete_slab(s),
            None => Ok(()),
        },
        19 => m.add_room(&room(r, b)).map(drop),
        20 => match any(r, m, "room") {
            Some(x) => m.update_room(x, &room(r, b)),
            None => Ok(()),
        },
        21 => match any(r, m, "room") {
            Some(x) => m.delete_room(x),
            None => Ok(()),
        },
        22 => {
            let a = node(r);
            m.add_room_sep(&SepSpec {
                level: b.lv[r.below(2)],
                a,
                b: (a.0 + r.f(1.0, 4.0), a.1),
            })
            .map(drop)
        }
        23 => match any(r, m, "room_sep") {
            Some(x) => m.delete_room_sep(x),
            None => Ok(()),
        },
        24 => match any(r, m, "stair") {
            Some(x) if r.below(2) == 0 => m.delete_stair(x),
            _ => m
                .add_stair(&StairSpec::straight(
                    b.lv[0],
                    b.lv[1],
                    node(r),
                    r.f(0.0, 6.0),
                ))
                .map(drop),
        },
        25 => match (any(r, m, "dim"), pick(r, &walls)) {
            (Some(d), _) if r.below(2) == 0 => m.delete_dim(d),
            (_, Some(w)) => m.add_dim(&dim(r, b, w)).map(drop),
            _ => Ok(()),
        },
        // shaft: full regeneration, then partial ones with deduction from beams
        26 if r.below(4) == 0 => {
            let (x, y) = node(r);
            m.add_shaft(&ShaftSpec {
                level: b.lv[0],
                top_level: b.lv[2],
                base_offset: 0.0,
                top_offset: 0.0,
                ring: rect(x, y, 1.0, 1.0),
            })
            .map(drop)
        }
        _ => m.undo(),
    };
}

#[test]
fn incremental_matches_full_on_random_edits() {
    set_verify(true);
    let seqs: u64 = std::env::var("DDC_REGEN_SEQS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let (i0, _) = stats();
    for seed in 0..seqs {
        let (mut m, b) = base();
        let mut r = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed + 1).wrapping_mul(0x2545_F491_4F6C_DD1D));
        for _ in 0..80 {
            step(&mut r, &mut m, &b);
        }
    }
    let (i1, _) = stats();
    assert!(i1 - i0 > seqs * 25, "incremental passes: {}", i1 - i0);
    set_verify(false);
}

#[test]
fn undo_restores_derived_tables() {
    set_verify(true);
    let (mut m, b) = base();
    let mut r = Rng(7);
    for _ in 0..30 {
        let _ = m.add_wall(&wall(&mut r, &b));
        let _ = m.add_column(&column(&mut r, &b));
    }
    let _ = m.add_slab(&SlabSpec {
        level: b.lv[1],
        slab_type: b.st,
        offset: 0.0,
        outer: rect(-6.0, -6.0, 12.0, 12.0),
        holes: vec![],
    });
    let before = snapshot(m.sql()).unwrap();
    let c = ids(&m, "col")[0];
    m.update_column(c, &column(&mut r, &b)).unwrap();
    let w = ids(&m, "wall")[0];
    m.set_wall_axis(w, (-6.0, -6.0), (6.0, -6.0)).unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    let after = snapshot(m.sql()).unwrap();
    assert_eq!(diff(&after, &before), None);
    set_verify(false);
}

#[test]
fn unrelated_ops_keep_index() {
    let (mut m, b) = base();
    let mut r = Rng(11);
    let w = m.add_wall(&wall(&mut r, &b)).unwrap();
    let (i0, f0) = stats();
    m.set_wall_axis(w, (0.0, 0.0), (5.0, 0.0)).unwrap();
    m.add_room(&room(&mut r, &b)).unwrap();
    let (i1, f1) = stats();
    assert_eq!(
        (i1 - i0, f1 - f0),
        (2, 0),
        "wall and room edits are incremental"
    );
    // wall type: full regeneration
    let lay = LayerSpec {
        material: "Brick".into(),
        thickness: 0.3,
    };
    let t = m.add_wall_type("W2", &[lay]).unwrap();
    m.set_wall_type(w, t).unwrap();
    let (_, f2) = stats();
    assert!(f2 > f1);
}

/// Wall neighbourhood equals full regeneration: a grid of thick and thin walls (corners, T joints,
/// 3+ way joints with fillers), columns at joints, rooms in cells; only neighbouring walls are edited.
#[test]
fn neighbourhood_matches_full_on_grid() {
    set_verify(true);
    let near0 = near_passes();
    let seqs: u64 = std::env::var("DDC_REGEN_SEQS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    for seed in 0..seqs {
        let (mut m, b) = base();
        let lay = |t| LayerSpec {
            material: "Brick".into(),
            thickness: t,
        };
        let thick = m.add_wall_type("T", &[lay(0.4)]).unwrap();
        let mut r = Rng(0xD1B5_4A32_D192_ED03 ^ (seed + 1).wrapping_mul(0x9E37_79B9));
        let (n, step) = (5i32, 3.0);
        let spec = |r: &mut Rng, a: (f64, f64), e: (f64, f64)| WallSpec {
            a,
            b: e,
            level: b.lv[0],
            base_offset: 0.0,
            height: 3.0,
            wall_type: if r.below(3) == 0 { thick } else { b.wt },
            offset0: None,
        };
        let mut ws = vec![];
        for i in 0..n {
            for j in 0..=n {
                let (x, y) = (i as f64 * step, j as f64 * step);
                ws.push(spec(&mut r, (x, y), (x + step, y)));
                ws.push(spec(&mut r, (y, x), (y, x + step)));
            }
        }
        m.add_walls(&ws).unwrap();
        for k in 0..12 {
            let at = (
                r.below(n as usize + 1) as f64 * step,
                r.below(n as usize + 1) as f64 * step,
            );
            m.add_column(&ColumnSpec {
                level: b.lv[0],
                top_level: Some(b.lv[1]),
                member_type: b.ct,
                structural: k % 2 == 0,
                at,
                base_offset: 0.0,
                top_offset: 0.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        }
        for k in 0..10 {
            let p = (r.below(n as usize) as f64 + 0.5) * step;
            let q = (r.below(n as usize) as f64 + 0.5) * step;
            m.add_room(&RoomSpec {
                level: b.lv[0],
                name: "R".into(),
                number: format!("{k}"),
                point: (p, q),
                height: 3.0,
            })
            .unwrap();
        }
        // a shift across the axis puts endpoints on other axes (T), shortening frees an endpoint: the
        // level stays free of overlaps, otherwise the edit falls back to regenerating the whole level
        for _ in 0..40 {
            let walls = ids(&m, "wall");
            let w = walls[r.below(walls.len())];
            let (a, e): ((f64, f64), (f64, f64)) = m
                .sql()
                .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [w], |q| {
                    Ok(((q.get(0)?, q.get(1)?), (q.get(2)?, q.get(3)?)))
                })
                .unwrap();
            let (x, y) = (
                r.below(n as usize) as f64 * step,
                r.below(n as usize) as f64 * step,
            );
            let _ = match r.below(8) {
                0 | 1 => {
                    let off = if r.below(2) == 0 { 0.5 } else { -0.5 } * step;
                    let shift = |p: (f64, f64)| {
                        if a.1 == e.1 {
                            (p.0, p.1 + off)
                        } else {
                            (p.0 + off, p.1)
                        }
                    };
                    m.set_wall_axis(w, shift(a), shift(e))
                }
                2 => {
                    let t = r.f(0.4, 0.9);
                    m.set_wall_axis(w, a, (a.0 + (e.0 - a.0) * t, a.1 + (e.1 - a.1) * t))
                }
                3 => m.delete_wall(w),
                4 => {
                    let half = y + step / 2.0;
                    m.add_wall(&spec(&mut r, (x, half), (x + step, half)))
                        .map(drop)
                }
                5 => m.add_opening(w, &opening(&mut r)).map(drop),
                6 => match pick(&mut r, &ids(&m, "col")) {
                    Some(c) => {
                        let mut s = m.column(c).unwrap();
                        s.at = (x + r.f(-0.3, 0.3), y + r.f(-0.3, 0.3));
                        m.update_column(c, &s)
                    }
                    None => Ok(()),
                },
                _ => m.undo(),
            };
        }
    }
    assert!(
        near_passes() - near0 > seqs * 10,
        "local passes: {}",
        near_passes() - near0
    );
    set_verify(false);
}

/// Timing: 10k walls on 20 levels, 500 columns, one slab per level; single edits.
#[test]
#[ignore]
fn bench_single_edits_on_10k_walls() {
    // 20 levels × 544 walls (16×16 grid of 5 m cells), 25 columns per level
    let (levels, n) = (20usize, 16usize);
    let mut m = Model::open(":memory:").unwrap();
    let lay = |t| LayerSpec {
        material: "Concrete".into(),
        thickness: t,
    };
    let wt = m.add_wall_type("W", &[lay(0.2)]).unwrap();
    let st = m.add_slab_type("S", &[lay(0.2)]).unwrap();
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let lv: Vec<i64> = (0..levels)
        .map(|i| m.add_level(&format!("L{i}"), 3.0 * i as f64).unwrap())
        .collect();
    let step = 5.0;
    let mut ws = vec![];
    for &l in &lv {
        for i in 0..n {
            for j in 0..=n {
                let (x, y) = (i as f64 * step, j as f64 * step);
                ws.push(WallSpec {
                    a: (x, y),
                    b: (x + step, y),
                    level: l,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: None,
                });
                ws.push(WallSpec {
                    a: (y, x),
                    b: (y, x + step),
                    level: l,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: None,
                });
            }
        }
    }
    let t = Instant::now();
    m.add_walls(&ws).unwrap();
    eprintln!("{} walls: {:?}", ws.len(), t.elapsed());
    let side = n as f64 * step;
    let slabs: Vec<SlabSpec> = lv
        .iter()
        .map(|&l| SlabSpec {
            level: l,
            slab_type: st,
            offset: 0.0,
            outer: rect(0.0, 0.0, side, side),
            holes: vec![],
        })
        .collect();
    m.add_slabs(&slabs).unwrap();
    let mut cs = vec![];
    for (k, &l) in lv.iter().enumerate() {
        for i in 0..25 {
            let at = ((i % 5) as f64 * 3.0 * step, (i / 5) as f64 * 3.0 * step);
            cs.push(ColumnSpec {
                level: l,
                top_level: lv.get(k + 1).copied(),
                member_type: ct,
                structural: true,
                at,
                base_offset: 0.0,
                top_offset: if k + 1 < lv.len() { 0.0 } else { 3.0 },
                rotation: 0.0,
                top_at: None,
            });
        }
    }
    let t = Instant::now();
    m.add_columns(&cs).unwrap();
    eprintln!("{} columns: {:?}", cs.len(), t.elapsed());
    // 256 rooms on one level: editing a wall there rebuilds its rooms
    for i in 0..n {
        for j in 0..n {
            m.sql()
                .execute(
                    "INSERT INTO room(level,name,number,px,py,height) VALUES (?1,'R','1',?2,?3,3.0)",
                    rusqlite::params![
                        lv[2],
                        (i as f64 + 0.5) * step,
                        (j as f64 + 0.5) * step
                    ],
                )
                .unwrap();
        }
    }
    let t = Instant::now();
    crate::regenerate(m.sql()).unwrap();
    eprintln!("full regenerate: {:?}", t.elapsed());
    // the first edit after a direct write is a full regeneration, later ones are partial
    let w = ids(&m, "wall")[0];
    m.set_wall_axis(w, (0.0, 0.0), (step, 0.0)).unwrap();
    let c = ids(&m, "col")[260];
    let mut moved = cs[260].clone();
    moved.at.0 += 1.0;
    let roomy: i64 = m
        .sql()
        .query_row("SELECT min(id) FROM wall WHERE level=?1", [lv[2]], |r| {
            r.get(0)
        })
        .unwrap();
    let (axis_a, axis_b): ((f64, f64), (f64, f64)) = m
        .sql()
        .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [roomy], |r| {
            Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
        })
        .unwrap();
    let slab: i64 = ids(&m, "slab")[5];
    let hole = OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    let (i0, _) = stats();
    let mut worst = std::time::Duration::ZERO;
    let mut calls = 0u64;
    let mut timed = |name: &str, f: &mut dyn FnMut(&mut Model)| {
        let t = Instant::now();
        f(&mut m);
        let d = t.elapsed();
        worst = worst.max(d);
        calls += 1;
        eprintln!("{name}: {d:?}");
    };
    timed("move column", &mut |m: &mut Model| {
        m.update_column(c, &moved).unwrap()
    });
    timed("undo column", &mut |m: &mut Model| m.undo().unwrap());
    timed("wall axis", &mut |m: &mut Model| {
        m.set_wall_axis(w, (0.0, 0.0), (step - 0.5, 0.0)).unwrap()
    });
    timed("undo wall", &mut |m: &mut Model| m.undo().unwrap());
    let room = RoomSpec {
        level: lv[3],
        name: "R".into(),
        number: "1".into(),
        point: (step / 2.0, step / 2.0),
        height: 3.0,
    };
    timed("add room", &mut |m: &mut Model| {
        m.add_room(&room).unwrap();
    });
    timed("undo room", &mut |m: &mut Model| m.undo().unwrap());
    timed("add opening", &mut |m: &mut Model| {
        m.add_opening(w, &hole).unwrap();
    });
    timed("undo opening", &mut |m: &mut Model| m.undo().unwrap());
    timed("slab boundary", &mut |m: &mut Model| {
        m.set_slab_boundary(slab, &rect(0.0, 0.0, side - 1.0, side), &[])
            .unwrap()
    });
    timed("undo slab", &mut |m: &mut Model| m.undo().unwrap());
    timed("wall axis, 256 rooms", &mut |m: &mut Model| {
        let shorter = (axis_b.0 - 0.5, axis_b.1);
        m.set_wall_axis(roomy, axis_a, shorter).unwrap()
    });
    timed("undo wall, 256 rooms", &mut |m: &mut Model| {
        m.undo().unwrap()
    });
    let (i1, _) = stats();
    eprintln!(
        "incremental passes: {}, worst single edit: {worst:?}",
        i1 - i0
    );
    assert_eq!(i1 - i0, if super::force_full() { 0 } else { calls });
}
