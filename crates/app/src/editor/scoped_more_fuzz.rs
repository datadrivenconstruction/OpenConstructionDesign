// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Random edit sequences on doors, windows, slabs and rooms: after every operation (or every
//! `stride`-th one) the lists a scoped reload built equal the lists of a fresh full reload.
//! A failure prints the seed, the step and the operation; it is a product finding.

use crate::editor::scoped::hits;
use crate::editor::{Editor, OpFn, Sel};
use ddc_model::{Edit, LayerSpec, Model, OpeningSpec, RoomSpec, SlabSpec, Swing, WallSpec};
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

/// Operation kinds of `drive`; the index is the kind number.
const KINDS: [&str; 9] = [
    "door width",
    "door offset",
    "door flip",
    "opening add",
    "opening delete",
    "slab edge move",
    "slab type change",
    "room name",
    "room number",
];

/// Kind number of the wall move every `wall_every`-th step.
const WALL_MOVE: usize = 99;

struct Fix {
    ed: Editor,
    slab_types: Vec<i64>,
    /// Room id and its point.
    rooms: Vec<(i64, (f64, f64))>,
}

fn layers(t: f64) -> [LayerSpec; 1] {
    [LayerSpec {
        material: "a".into(),
        thickness: t,
    }]
}

fn opening(along: f64, width: f64, sill: f64, height: f64, kind: &str) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    }
}

/// 97 walls of a 6 x 7 grid of 4 m cells and three walls far away (100 walls), doors and
/// windows, two slabs side by side under the grid, six rooms in closed cells.
fn fixture() -> Fix {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let wt = ed.model.add_wall_type("Z fuzz", &layers(0.2)).unwrap();
    let wall = |a: (f64, f64), b: (f64, f64)| WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    };
    let (mut horiz, mut vert) = (vec![], vec![]);
    for r in 0..=7 {
        for c in 0..6 {
            let (x, y) = (c as f64 * 4.0, r as f64 * 4.0);
            horiz.push(ed.model.add_wall(&wall((x, y), (x + 4.0, y))).unwrap());
        }
    }
    for r in 0..7 {
        for c in 0..=6 {
            let (x, y) = (c as f64 * 4.0, r as f64 * 4.0);
            vert.push(ed.model.add_wall(&wall((x, y), (x, y + 4.0))).unwrap());
        }
    }
    for k in 0..3 {
        let x = 100.0 + k as f64 * 8.0;
        ed.model.add_wall(&wall((x, 0.0), (x + 6.0, 0.0))).unwrap();
    }
    assert_eq!(horiz.len() + vert.len() + 3, 100);
    for &w in horiz.iter().step_by(5) {
        ed.model
            .add_opening(w, &opening(1.0, 0.9, 0.0, 2.1, "door"))
            .unwrap();
    }
    for &w in vert.iter().skip(3).step_by(7) {
        ed.model
            .add_opening(w, &opening(1.0, 1.2, 0.9, 1.2, "window"))
            .unwrap();
    }
    let slab_types: Vec<i64> = [("Z s1", 0.2), ("Z s2", 0.3), ("Z s3", 0.15)]
        .iter()
        .map(|(n, t)| ed.model.add_slab_type(n, &layers(*t)).unwrap())
        .collect();
    for (x0, x1) in [(0.0, 12.0), (12.0, 24.0)] {
        ed.model
            .add_slab(&SlabSpec {
                level,
                slab_type: slab_types[0],
                offset: 0.0,
                outer: vec![(x0, 0.0), (x1, 0.0), (x1, 28.0), (x0, 28.0)],
                holes: vec![],
            })
            .unwrap();
    }
    let mut rooms = vec![];
    for (n, (c, r)) in [(0, 0), (1, 0), (2, 1), (3, 2), (4, 3), (5, 6)]
        .into_iter()
        .enumerate()
    {
        let point = (c as f64 * 4.0 + 2.0, r as f64 * 4.0 + 2.0);
        let id = ed
            .model
            .add_room(&RoomSpec {
                level,
                name: format!("R{n}"),
                number: format!("{n}"),
                point,
                height: 2.7,
            })
            .unwrap();
        rooms.push((id, point));
    }
    ed.reload().unwrap();
    // the first op after a load regenerates everything (and keys the level cache)
    let w = horiz[1];
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        wall_set(m, w, |s| s.height = 3.0)
    })))
    .unwrap();
    assert_eq!(ed.walls.len(), 100);
    assert!(ed.openings.len() >= 17 && ed.slabs.len() == 2 && ed.rooms.len() == 6);
    Fix {
        ed,
        slab_types,
        rooms,
    }
}

fn wall_set(
    m: &mut Model,
    id: i64,
    f: impl FnOnce(&mut WallSpec),
) -> Result<(), ddc_model::Error> {
    let (mut s, bulge) = crate::query::wall_spec(m, id)?;
    f(&mut s);
    m.apply_edits(&[Edit::Wall { id, spec: s, bulge }])
}

/// Lists a reload builds, in a comparable form.
fn lists(ed: &Editor) -> String {
    let snap: Vec<_> = ed.tool.walls.iter().map(|s| (s.a, s.b, s.half)).collect();
    let slabs: Vec<String> = ed
        .slabs
        .iter()
        .map(|s| {
            format!(
                "{} {:?} {:?} {:?} {} {} {} {}",
                s.id, s.rings, s.cuts, s.slope, s.top, s.thickness, s.area, s.volume
            )
        })
        .collect();
    let curtains: Vec<i64> = ed.curtains.iter().map(|c| c.id).collect();
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{}\n{}",
        ed.walls,
        ed.plan,
        ed.otool.walls,
        snap,
        ed.openings,
        ed.otool.openings,
        ed.selection,
        slabs,
        curtains,
        ed.rooms,
        ed.tags,
        ed.elevation,
        ed.plan_joined
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
    let scoped = lists(ed);
    ed.reload().unwrap();
    let full = lists(ed);
    assert!(scoped == full, "{what}\n{}", first_diff(&scoped, &full));
}

/// A selection of one opening, one slab, one room and one wall of the current lists.
fn pick_selection(ed: &mut Editor, rng: &mut Rng) {
    let mut sel = vec![];
    if !ed.openings.is_empty() {
        sel.push(Sel::Opening(ed.openings[rng.below(ed.openings.len())].id));
    }
    if !ed.slabs.is_empty() {
        sel.push(Sel::Slab(ed.slabs[rng.below(ed.slabs.len())].id));
    }
    if !ed.rooms.is_empty() {
        sel.push(Sel::Room(ed.rooms[rng.below(ed.rooms.len())].id));
    }
    sel.push(Sel::Wall(ed.walls[rng.below(ed.walls.len())].id));
    ed.selection = sel;
}

/// Counts of a run: scoped reloads, refused operations, operations done per kind.
#[derive(Default)]
struct Stat {
    scoped: u64,
    refused: usize,
    done: [usize; 9],
    walls_moved: usize,
}

/// Runs `ops` random operations; every `wall_every`-th one (0: never) moves a wall end; the
/// lists are compared after every `stride`-th operation and after the last one.
fn drive(f: &mut Fix, seed: u64, ops: usize, wall_every: usize, stride: usize) -> Stat {
    let mut rng = Rng(seed);
    let mut st = Stat::default();
    pick_selection(&mut f.ed, &mut rng);
    for step in 0..ops {
        let ed = &mut f.ed;
        let v = rng.below(9) as f64 * 0.1 + 0.6;
        let d = rng.below(20) as f64 * 0.1 + 0.5;
        let sign = if rng.below(2) == 0 { 1.0 } else { -1.0 };
        let amount = (rng.below(4) + 1) as f64 * 0.5 * sign;
        let pick = rng.below(1000);
        let wall_op = wall_every > 0 && step % wall_every == wall_every - 1;
        let kind = if wall_op { WALL_MOVE } else { rng.below(9) };
        let o = if ed.openings.is_empty() {
            None
        } else {
            Some(ed.openings[pick % ed.openings.len()])
        };
        let h = hits();
        let r = match (kind, o) {
            (WALL_MOVE, _) => {
                let w = ed.walls[pick % ed.walls.len()].id;
                let (dx, dy, end) = (rng.step(), rng.step(), rng.below(2) == 0);
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    wall_set(m, w, |s| {
                        let p = if end { &mut s.a } else { &mut s.b };
                        *p = (p.0 + dx, p.1 + dy);
                    })
                })))
            }
            (0, Some(o)) => {
                let (id, kd) = (o.id, o.kind.model_name());
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    m.update_opening(id, &opening(o.along, v + 0.3, o.sill, o.height, kd))
                })))
            }
            (1, Some(o)) => {
                let (id, kd) = (o.id, o.kind.model_name());
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    m.update_opening(id, &opening(d, o.width, o.sill, o.height, kd))
                })))
            }
            (2, Some(o)) => {
                let (id, side, hinge) = (o.id, pick % 2 == 0, pick % 3 == 0);
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    m.set_opening_swing(id, Swing { side, hinge })
                })))
            }
            (3, _) => {
                let w = ed.walls[pick % ed.walls.len()].id;
                let kd = if pick % 2 == 0 { "door" } else { "window" };
                let sill = if kd == "door" { 0.0 } else { 0.9 };
                let spec = opening(d, v, sill, 1.2, kd);
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    m.add_opening(w, &spec).map(|_| ())
                })))
            }
            (4, Some(o)) if ed.openings.len() > 8 => {
                let id = o.id;
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_opening(id))))
            }
            (5, _) => {
                let id = ed.slabs[pick % ed.slabs.len()].id;
                let side = pick / 7 % 4;
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    let rings = m.slab_rings(id)?;
                    let mut outer = rings[0].clone();
                    let key = |p: &(f64, f64)| if side < 2 { p.0 } else { p.1 };
                    let lo = outer.iter().map(key).fold(f64::MAX, f64::min);
                    let hi = outer.iter().map(key).fold(f64::MIN, f64::max);
                    // an edge on the low side shrinks the extent when it moves up
                    let toward = if side % 2 == 0 { -amount } else { amount };
                    let dd = if hi - lo + toward < 1.0 { -amount } else { amount };
                    let at = if side % 2 == 0 { lo } else { hi };
                    for p in outer.iter_mut().filter(|p| (key(p) - at).abs() < 1e-9) {
                        if side < 2 {
                            p.0 += dd;
                        } else {
                            p.1 += dd;
                        }
                    }
                    m.set_slab_boundary(id, &outer, &rings[1..])
                })))
            }
            (6, _) => {
                let id = ed.slabs[pick % ed.slabs.len()].id;
                let (level, ty) = (ed.level, f.slab_types[pick / 7 % f.slab_types.len()]);
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    m.apply_edits(&[Edit::Slab {
                        id,
                        level,
                        slab_type: ty,
                        offset: 0.0,
                    }])
                })))
            }
            (7 | 8, _) => {
                let (id, point) = f.rooms[pick % f.rooms.len()];
                let level = ed.level;
                let (name, number) = if kind == 7 {
                    (format!("N{step}"), format!("{}", pick % 7))
                } else {
                    (format!("R{}", pick % 5), format!("{step}"))
                };
                ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                    m.update_room(
                        id,
                        &RoomSpec {
                            level,
                            name: name.clone(),
                            number: number.clone(),
                            point,
                            height: 2.7,
                        },
                    )
                })))
            }
            // a delete with too few openings left, or no opening to edit: nothing to do
            _ => continue,
        };
        if r.is_err() {
            // refused by the model: nothing changed
            st.refused += 1;
            continue;
        }
        st.scoped += hits() - h;
        if kind == WALL_MOVE {
            st.walls_moved += 1;
        } else {
            st.done[kind] += 1;
        }
        if (step + 1) % stride == 0 || step + 1 == ops {
            let what = KINDS.get(kind).copied().unwrap_or("wall move");
            check(&mut f.ed, &format!("seed {seed} step {step} kind {what}"));
        }
        if step % 20 == 19 {
            pick_selection(&mut f.ed, &mut rng);
        }
    }
    st
}

fn all_kinds_done(stats: &[Stat]) {
    for (k, name) in KINDS.iter().enumerate() {
        let n: usize = stats.iter().map(|s| s.done[k]).sum();
        assert!(n > 0, "no `{name}` operation was applied");
    }
}

#[test]
fn scoped_more_fuzz_doors_slabs_rooms() {
    let mut stats = vec![];
    for seed in [0x5eed_u64, 7, 20_261_106] {
        let mut f = fixture();
        let s = drive(&mut f, seed, 200, 0, 1);
        assert!(s.scoped >= 100, "seed {seed}: only {} scoped reloads", s.scoped);
        assert!(s.refused < 100, "seed {seed}: {} operations refused", s.refused);
        assert_eq!(s.walls_moved, 0);
        stats.push(s);
    }
    all_kinds_done(&stats);
}

#[test]
fn scoped_more_fuzz_mixed_with_walls() {
    let mut stats = vec![];
    for seed in [3_u64, 0xdd_c0de, 1_106] {
        let mut f = fixture();
        let s = drive(&mut f, seed, 200, 10, 1);
        assert!(s.scoped >= 80, "seed {seed}: only {} scoped reloads", s.scoped);
        assert!(s.refused < 100, "seed {seed}: {} operations refused", s.refused);
        assert!(s.walls_moved > 0, "seed {seed}: no wall move was applied");
        stats.push(s);
    }
    all_kinds_done(&stats);
}

#[test]
fn scoped_more_fuzz_drift_between_checks() {
    // ten operations in a row before the lists meet a full reload
    let mut f = fixture();
    let s = drive(&mut f, 42, 200, 10, 10);
    assert!(s.scoped >= 80, "only {} scoped reloads", s.scoped);
}
