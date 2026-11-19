//! Random edit sequences on a big model: after every operation the lists a scoped reload built
//! equal the lists of a fresh full reload. A failure prints the seed, the step and the operation.

use super::{cut_walls, hits};
use crate::edit3d::scene3d::{compose, compose_with, Cache};
use crate::editor::{Editor, OpFn};
use ddc_model::{Edit, LayerSpec, Model, OpeningSpec, RoomSpec, WallSpec};
use rusqlite::types::Value;
use std::sync::Arc;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// Multiple of 0.5 in [-2, 2].
    fn step(&mut self) -> f64 {
        self.below(9) as f64 * 0.5 - 2.0
    }
}

struct Fix {
    ed: Editor,
    ids: Vec<i64>,
    /// Wall far from the grid, the arc wall of the cut-and-arc test.
    spare: i64,
}

/// 400 walls: 16 rows of 20 segments, 80 connectors between row pairs (L, T and cross joins),
/// one connector in the middle of a segment (T-join); a door per 20 walls, a room, five walls on
/// another level above the same footprint, a spare wall far away. `cut`: a form join on the
/// T-join pair.
fn fixture(cut: bool) -> Fix {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let layers = |a: f64, b: f64| {
        vec![
            LayerSpec {
                material: "a".into(),
                thickness: a,
            },
            LayerSpec {
                material: "b".into(),
                thickness: b,
            },
        ]
    };
    let t1 = ed.model.add_wall_type("F thin", &layers(0.1, 0.1)).unwrap();
    ed.model.add_wall_type("F thick", &layers(0.3, 0.2)).unwrap();
    ed.model.add_wall_type("F mid", &layers(0.15, 0.1)).unwrap();
    let other = ed.model.add_level("L2", 3.0).unwrap();
    let mut ids = vec![];
    let add = |ed: &mut Editor, lv: i64, a: (f64, f64), b: (f64, f64)| {
        ed.model
            .add_wall(&WallSpec {
                a,
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t1,
                offset0: None,
            })
            .unwrap()
    };
    for r in 0..16 {
        let y = r as f64 * 6.0;
        for i in 0..20 {
            let x = i as f64 * 5.0;
            ids.push(add(&mut ed, level, (x, y), (x + 5.0, y)));
        }
    }
    let mut t_pair = (0, 0);
    for band in 0..8 {
        for col in 0..10 {
            let mid = band == 0 && col == 1;
            let x = if mid { 7.5 } else { col as f64 * 10.0 };
            let y = band as f64 * 12.0;
            let id = add(&mut ed, level, (x, y), (x, y + 6.0));
            if mid {
                t_pair = (id, ids[1]);
            }
            ids.push(id);
        }
    }
    assert_eq!(ids.len(), 400);
    for k in 0..5 {
        let x = k as f64 * 5.0;
        add(&mut ed, other, (x, 0.0), (x + 5.0, 0.0));
    }
    let spare = add(&mut ed, level, (200.0, 0.0), (206.0, 0.0));
    for &w in ids.iter().step_by(20) {
        ed.model
            .add_opening(
                w,
                &OpeningSpec {
                    along: 1.0,
                    width: 0.9,
                    sill: 0.0,
                    // below the lowest wall height the edits set (2.0): the model refuses a door
                    // that rises above its wall
                    height: 1.9,
                    kind: "door".into(),
                },
            )
            .unwrap();
    }
    ed.model
        .add_room(&RoomSpec {
            level,
            name: "R".into(),
            number: "1".into(),
            point: (3.0, 3.0),
            height: 2.7,
        })
        .unwrap();
    if cut {
        ed.model
            .join_geometry(("wall", t_pair.0), ("wall", t_pair.1))
            .unwrap();
        let c = cut_walls(&ed.model).unwrap();
        assert!(
            c.contains(&t_pair.0) && c.contains(&t_pair.1),
            "the form join exists"
        );
    }
    ed.reload().unwrap();
    // the first op after a load regenerates everything (and keys the level cache)
    let w = ids[2];
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, w, |s| s.height = 3.0))))
        .unwrap();
    Fix { ed, ids, spare }
}

fn set(m: &mut Model, id: i64, f: impl FnOnce(&mut WallSpec)) -> Result<(), ddc_model::Error> {
    let (mut s, bulge) = crate::query::wall_spec(m, id)?;
    f(&mut s);
    m.apply_edits(&[Edit::Wall { id, spec: s, bulge }])
}

/// Lists a reload builds, in a comparable form.
fn lists(ed: &Editor) -> String {
    let snap: Vec<_> = ed.tool.walls.iter().map(|s| (s.a, s.b, s.half)).collect();
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        ed.walls, ed.plan, ed.otool.walls, snap, ed.openings, ed.rooms
    )
}

fn first_diff(a: &str, b: &str) -> String {
    let i = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let cut = |s: &str| {
        let lo = i.saturating_sub(120);
        let hi = (i + 120).min(s.len());
        s.get(lo..hi).unwrap_or("").to_string()
    };
    format!(
        "first difference at byte {i}\nscoped: {}\nfull:   {}",
        cut(a),
        cut(b)
    )
}

fn check(ed: &mut Editor, what: &str) {
    check_with(ed, what, &mut None);
}

/// Element of a scene mesh in a form that does not depend on the slot map of a cache.
fn element_key(e: u32, targets: &[impl std::fmt::Debug]) -> String {
    let base = crate::scene_ids::EXT_BASE;
    match e.checked_sub(base).map(|k| k as usize) {
        Some(k) if k < targets.len() => format!("t{:?}", targets[k]),
        _ => format!("e{e}"),
    }
}

/// Element ids, transforms and mesh hashes of a 3D scene, sorted.
pub(super) fn scene_fp(sc: &ddc_render::Scene, targets: &[impl std::fmt::Debug]) -> Vec<String> {
    use std::hash::{Hash, Hasher};
    let hash = |m: &ddc_render::SceneMesh| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        m.tag.hash(&mut h);
        for p in &m.positions {
            p.map(f32::to_bits).hash(&mut h);
        }
        m.triangles.hash(&mut h);
        h.finish()
    };
    let mut out: Vec<String> = sc
        .meshes
        .iter()
        .map(|m| format!("{} {:016x}", element_key(m.element, targets), hash(m)))
        .collect();
    for i in &sc.instanced {
        let p = hash(&i.proto);
        out.extend(i.inst.iter().map(|q| {
            format!("{} {p:016x} {:?}", element_key(q.element, targets), q.m)
        }));
    }
    out.sort();
    out
}

/// `check`, and with `scene` the 3D scene built incrementally after the scoped reload equals
/// a scene built from scratch after the full reload.
fn check_with(ed: &mut Editor, what: &str, scene: &mut Option<Cache>) {
    let scoped = lists(ed);
    let inc = scene.as_mut().map(|c| {
        let (s, t) = compose_with(ed, c);
        scene_fp(&s, &t)
    });
    ed.reload().unwrap();
    let full = lists(ed);
    assert!(scoped == full, "{what}\n{}", first_diff(&scoped, &full));
    if let Some(inc) = inc {
        let (s, t) = compose(ed);
        let fresh = scene_fp(&s, &t);
        if inc != fresh {
            let a: Vec<&String> = inc.iter().filter(|x| !fresh.contains(x)).take(5).collect();
            let b: Vec<&String> = fresh.iter().filter(|x| !inc.contains(x)).take(5).collect();
            panic!("{what}: 3D scene differs\nscoped only: {a:?}\nfull only: {b:?}");
        }
    }
}

/// Runs `ops` random operations; returns (scoped reloads, refused operations).
fn drive(ed: &mut Editor, seed: u64, ops: usize, deletes: bool) -> (u64, usize) {
    drive_with(ed, seed, ops, deletes, &mut None)
}

/// `drive`, comparing the 3D scene too when `scene` holds a cache.
fn drive_with(
    ed: &mut Editor,
    seed: u64,
    ops: usize,
    deletes: bool,
    scene: &mut Option<Cache>,
) -> (u64, usize) {
    let mut rng = Rng(seed);
    let (mut scoped, mut refused) = (0, 0);
    for step in 0..ops {
        let id = ed.walls[rng.below(ed.walls.len())].id;
        let kind = rng.below(if deletes { 8 } else { 7 });
        let (dx, dy, end) = (rng.step(), rng.step(), rng.below(2) == 0);
        let v = 2.0 + rng.below(20) as f64 * 0.25;
        let ty = ed.types[rng.below(ed.types.len())].id;
        let ang = (rng.below(11) + 1) as f64 * 15.0_f64.to_radians();
        let h = hits();
        let r = match kind {
            0 => ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                set(m, id, |s| {
                    let p = if end { &mut s.a } else { &mut s.b };
                    *p = (p.0 + dx, p.1 + dy);
                })
            }))),
            1 => ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                set(m, id, |s| s.height = v)
            }))),
            2 => ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                set(m, id, |s| s.wall_type = ty)
            }))),
            3 => ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                m.set_prop("wall", id, "fz_mark", Some((Value::Real(v), "")))
            }))),
            4 => ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                set(m, id, |s| {
                    let c = ((s.a.0 + s.b.0) / 2.0, (s.a.1 + s.b.1) / 2.0);
                    let (hx, hy) = ((s.b.0 - s.a.0) / 2.0, (s.b.1 - s.a.1) / 2.0);
                    let (sn, cs) = ang.sin_cos();
                    let r = |x: f64| (x * 1000.0).round() / 1000.0;
                    let (rx, ry) = (hx * cs - hy * sn, hx * sn + hy * cs);
                    s.a = (r(c.0 - rx), r(c.1 - ry));
                    s.b = (r(c.0 + rx), r(c.1 + ry));
                })
            }))),
            5 => {
                ed.undo();
                Ok(())
            }
            6 => {
                ed.redo();
                Ok(())
            }
            _ if ed.walls.len() > 300 => {
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_wall(id))))
            }
            _ => Ok(()),
        };
        if r.is_err() {
            // refused by the model: nothing changed
            refused += 1;
            continue;
        }
        scoped += hits() - h;
        check_with(ed, &format!("seed {seed} step {step} kind {kind} wall {id}"), scene);
    }
    (scoped, refused)
}

#[test]
fn scoped_equals_full_random_200() {
    for seed in [0x5eed_u64, 7, 20_261_106] {
        let mut f = fixture(false);
        let (scoped, refused) = drive(&mut f.ed, seed, 200, true);
        assert!(scoped >= 60, "seed {seed}: only {scoped} scoped reloads");
        assert!(refused < 100, "seed {seed}: {refused} operations refused");
    }
}

#[test]
fn scoped_equals_full_with_cut_and_arc() {
    let mut f = fixture(true);
    // a form join makes the model regenerate every derived row (regen `Plan::new`), more rows
    // than the scoped limit: these edits take the full reload, and its lists must still hold
    let c: Vec<i64> = cut_walls(&f.ed.model).unwrap().into_iter().collect();
    for (k, &w) in c.iter().enumerate() {
        let v = 2.5 + k as f64;
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            set(m, w, |s| s.height = v)
        })))
        .unwrap();
        check(&mut f.ed, &format!("cut wall {w} height"));
    }
    drive(&mut f.ed, 11, 80, true);
    // an arc wall in the level: every reload must still equal the full one
    let spare = f.spare;
    let bend = move |bulge: f64| {
        OpFn(Arc::new(move |m: &mut Model| {
            let (spec, _) = crate::query::wall_spec(m, spare)?;
            m.apply_edits(&[Edit::Wall {
                id: spare,
                spec,
                bulge,
            }])
        }))
    };
    f.ed.run_op(bend(0.4)).unwrap();
    check(&mut f.ed, "arc wall made");
    drive(&mut f.ed, 12, 80, true);
    f.ed.run_op(bend(0.0)).unwrap();
    check(&mut f.ed, "arc wall straightened");
    drive(&mut f.ed, 13, 40, true);
}

#[test]
fn scoped_hits_counted() {
    let mut f = fixture(false);
    for (k, &id) in f.ids.iter().step_by(37).enumerate() {
        let h = hits();
        let v = 2.0 + k as f64 * 0.1;
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            set(m, id, |s| s.height = v)
        })))
        .unwrap();
        assert_eq!(hits(), h + 1, "height edit of wall {id} is one scoped reload");
        let h = hits();
        f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            set(m, id, |s| s.a = (s.a.0 + 0.5, s.a.1))
        })))
        .unwrap();
        assert_eq!(hits(), h + 1, "end move of wall {id} is one scoped reload");
        check(&mut f.ed, &format!("wall {id}"));
    }
    // a deleted wall changes the lists' shape: full reload, no hit
    let (h, id) = (hits(), f.ids[5]);
    f.ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_wall(id))))
        .unwrap();
    assert_eq!(hits(), h, "a delete takes the full reload");
    check(&mut f.ed, "delete");
}

/// The 3D scene, rebuilt part by part from the change journal after each scoped reload, equals
/// a scene built from scratch after a full reload: same element ids, transforms, mesh hashes.
#[test]
fn scoped_scene_equals_full_random() {
    let mut f = fixture(true);
    let mut cache = Some(Cache::default());
    // the first build fills the cache
    check_with(&mut f.ed, "first build", &mut cache);
    let mut scoped = 0;
    for seed in [0x3d_u64, 404] {
        scoped += drive_with(&mut f.ed, seed, 40, true, &mut cache).0;
    }
    assert!(scoped >= 10, "only {scoped} scoped reloads");
}

#[test]
#[ignore = "long: 5000 random operations"]
fn scoped_equals_full_random_5000() {
    let mut f = fixture(true);
    let (scoped, _) = drive(&mut f.ed, 0xdd_c0de, 5000, true);
    assert!(scoped > 1000, "only {scoped} scoped reloads");
}
