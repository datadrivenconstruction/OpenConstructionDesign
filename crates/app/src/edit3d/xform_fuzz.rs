// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Seeded property tests for 3D mirror and array: a selection of plan kinds gives the same rows
//! as the plan edit (`mirror_elements` / `copy_elements`), a selection with any other kind is
//! refused whole with the reason in the status and no rows; hosted openings stay inside their
//! copied host; one undo restores the whole table hash. A failing seed is printed in every
//! assertion message. `DDC_FUZZ_SEEDS=200` widens the run.

use super::pick::{Cat, Target};
use super::xform::Xform;
use super::*;
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, Just, LayerSpec, Model, OpeningSpec, RoofKind,
    RoofSpec, SlabSpec, StairSpec, WallSpec,
};
use rusqlite::types::Value;

/// Seeds of every test unless `DDC_FUZZ_SEEDS` asks for more.
fn seeds() -> u64 {
    std::env::var("DDC_FUZZ_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(24)
}
/// Tables the plan edit owns: 3D rows of these kinds must equal the plan result.
const PLAN_TABLES: [&str; 4] = ["wall", "opening", "slab", "slab_vertex"];

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn coin(&mut self, per_cent: u64) -> bool {
        self.below(100) < per_cent
    }
}

struct Fx {
    ed: Editor,
    walls: Vec<i64>,
    openings: Vec<i64>,
    cols: Vec<i64>,
    beam: i64,
    slab: i64,
    roof: i64,
    ceiling: i64,
    stair: i64,
}

/// The same seed always builds the same rows with the same ids.
impl Fx {
    fn new(seed: u64) -> Fx {
        let mut r = Rng::new(seed);
        let mut ed = Editor::new_in_memory().unwrap();
        let layer = [LayerSpec {
            material: "c".into(),
            thickness: 0.2,
        }];
        let wt = ed.model.add_wall_type("W fuzz", &layer).unwrap();
        let st = ed.model.add_slab_type("S fuzz", &layer).unwrap();
        let mt = ed.model.add_catalog_member_type("R 400x400").unwrap();
        let level = ed.level;
        let top = ed.model.add_level("L2 fuzz", 3.0).unwrap();
        let (mut walls, mut openings, mut cols) = (vec![], vec![], vec![]);
        for _ in 0..2 + r.below(3) {
            let a = (r.range(-10.0, 10.0), r.range(-10.0, 10.0));
            let (ang, len) = (r.range(0.0, 6.28), r.range(4.0, 8.0));
            let b = (a.0 + len * ang.cos(), a.1 + len * ang.sin());
            let w = ed
                .model
                .add_wall(&WallSpec {
                    a,
                    b,
                    level,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: None,
                })
                .unwrap();
            let mut along = 0.4 + r.range(0.0, 0.4);
            for _ in 0..1 + r.below(2) {
                let door = r.coin(50);
                let o = OpeningSpec {
                    along,
                    width: 0.9,
                    sill: if door { 0.0 } else { 0.9 },
                    height: if door { 2.1 } else { 1.2 },
                    kind: if door { "door" } else { "window" }.into(),
                };
                openings.push(ed.model.add_opening(w, &o).unwrap());
                along += 0.9 + 0.5;
            }
            walls.push(w);
        }
        for _ in 0..1 + r.below(2) {
            let at = (r.range(-10.0, 10.0), r.range(-10.0, 10.0));
            cols.push(
                ed.model
                    .add_column(&ColumnSpec {
                        level,
                        top_level: None,
                        member_type: mt,
                        structural: true,
                        at,
                        base_offset: 0.0,
                        top_offset: 3.0,
                        rotation: r.range(-3.0, 3.0),
                        top_at: None,
                    })
                    .unwrap(),
            );
        }
        let p = (r.range(-10.0, 10.0), r.range(-10.0, 10.0));
        let beam = ed
            .model
            .add_beam(&BeamSpec {
                level,
                member_type: mt,
                a: (p.0, p.1, 3.0),
                b: (p.0 + r.range(2.0, 5.0), p.1 + r.range(-2.0, 2.0), 3.0),
                bulge: r.range(-0.2, 0.2),
                yjust: Just::Pos,
                zjust: Just::Center,
                rotation: r.range(-0.5, 0.5),
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::default(),
                join_end: BeamJoin::default(),
            })
            .unwrap();
        let rect = |r: &mut Rng| {
            let (x, y) = (r.range(-10.0, 6.0), r.range(-10.0, 6.0));
            let (w, h) = (r.range(2.0, 4.0), r.range(2.0, 4.0));
            vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
        };
        let slab = ed
            .model
            .add_slab(&SlabSpec {
                level,
                slab_type: st,
                offset: 0.0,
                outer: rect(&mut r),
                holes: vec![],
            })
            .unwrap();
        let roof = ed
            .model
            .add_roof(&RoofSpec {
                level,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: rect(&mut r),
                    slopes: vec![Some(30.0); 4],
                    mansard: None,
                },
            })
            .unwrap();
        let ceiling = ed
            .model
            .add_ceiling(&CeilingSpec {
                level,
                slab_type: st,
                height: 2.7,
                outer: rect(&mut r),
                holes: vec![],
            })
            .unwrap();
        let stair = ed
            .model
            .add_stair(&StairSpec::straight(level, top, (12.0, 12.0), 0.0))
            .unwrap();
        ed.reload().unwrap();
        Fx {
            ed,
            walls,
            openings,
            cols,
            beam,
            slab,
            roof,
            ceiling,
            stair,
        }
    }

    /// A random non-empty selection (never the stair): half of the time plan kinds only,
    /// otherwise mixed with columns, beams, roofs and ceilings.
    fn pick(&self, r: &mut Rng) -> Vec<Target> {
        let mut all = vec![];
        all.extend(self.walls.iter().map(|&i| Target::new(Cat::Wall, i)));
        all.extend(self.openings.iter().map(|&i| Target::new(Cat::Opening, i)));
        all.push(Target::new(Cat::Slab, self.slab));
        if r.coin(50) {
            all.extend(self.cols.iter().map(|&i| Target::new(Cat::Column, i)));
            all.push(Target::new(Cat::Beam, self.beam));
            all.push(Target::new(Cat::Roof, self.roof));
            all.push(Target::new(Cat::Ceiling, self.ceiling));
        }
        let mut sel: Vec<Target> = all.iter().copied().filter(|_| r.coin(50)).collect();
        if sel.is_empty() {
            sel.push(all[0]);
        }
        sel
    }

    /// Walls (and sometimes the slab): the hosts of the openings.
    fn pick_hosts(&self, r: &mut Rng) -> Vec<Target> {
        let mut sel: Vec<Target> = self
            .walls
            .iter()
            .filter(|_| r.coin(60))
            .map(|&i| Target::new(Cat::Wall, i))
            .collect();
        if sel.is_empty() {
            sel.push(Target::new(Cat::Wall, self.walls[0]));
        }
        if r.coin(30) {
            sel.push(Target::new(Cat::Slab, self.slab));
        }
        sel
    }
}

/// The plan targets of the selection, `None` when a member has no plan operation.
fn plan_ts(sel: &[Target]) -> Option<Vec<ddc_model::Target>> {
    sel.iter().map(|t| t.modify()).collect()
}

/// The status of a refused 3D edit, as the editor writes it.
fn refusal(ru: bool, key: &str) -> String {
    let e = ddc_model::Error::Invalid(key.into());
    format!("{}: {}", tr(ru, "e3d.err"), err_text(ru, &e))
}

fn refused(ed: &Editor) -> bool {
    matches!(ed.status_sev, Some((crate::status::Severity::Error, _)))
}

fn mirror_line(r: &mut Rng) -> (Vec3, Vec3) {
    let (x, y) = (r.range(-8.0, 8.0) as f32, r.range(-8.0, 8.0) as f32);
    let (ang, len) = (r.range(0.0, 6.28), r.range(1.0, 5.0));
    let q = (x + (len * ang.cos()) as f32, y + (len * ang.sin()) as f32);
    (Vec3::new(x, y, 0.0), Vec3::new(q.0, q.1, 0.0))
}

fn array_step(r: &mut Rng) -> (Vec3, u32) {
    let (ang, len) = (r.range(0.0, 6.28), r.range(0.8, 3.0));
    let step = Vec3::new((len * ang.cos()) as f32, (len * ang.sin()) as f32, 0.0);
    (step, 2 + r.below(5) as u32)
}

/// One 3D action through the editor, as the gizmo and the mirror mode do it.
fn act3d(fx: &mut Fx, sel: &[Target], x: Xform, copy: bool) {
    fx.ed.status = None;
    fx.ed.status_sev = None;
    let mut e = Edit3d::default();
    e.set_sel(&mut fx.ed, sel.to_vec());
    e.commit_xform(&mut fx.ed, x, copy);
}

fn dump(m: &Model, tables: &[&str]) -> Vec<String> {
    let mut out = vec![];
    for t in tables {
        // project_uid is random per model: two builds of one seed never share it
        let skip = if *t == "meta" { " WHERE key<>'project_uid'" } else { "" };
        let mut st = m.sql().prepare(&format!("SELECT * FROM {t}{skip}")).unwrap();
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
        out.push(format!("{t}:{}", rows.join(";")));
    }
    out
}

/// Hash of every table except the journal.
fn table_hash(m: &Model) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut names: Vec<String> = {
        let mut st = m
            .sql()
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name<>'op' \
                 AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap();
        let v = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    names.sort();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dump(m, &refs).hash(&mut h);
    h.finish()
}

fn mirror_equals_plan(seed: u64) {
    let (mut a, mut b) = (Fx::new(seed), Fx::new(seed));
    let mut r = Rng::new(seed ^ 0xA5A5);
    let sel = a.pick(&mut r);
    let (p, q) = mirror_line(&mut r);
    let copy = r.coin(50);
    act3d(&mut a, &sel, Xform::Mirror { a: p, b: q }, copy);
    let ctx = format!("seed {seed} copy {copy} sel {sel:?} status {:?}", a.ed.status);
    let Some(plan) = plan_ts(&sel) else {
        assert_eq!(a.ed.status, Some(refusal(false, "e3d.err.unsupported")), "{ctx}");
        assert_eq!(table_hash(&a.ed.model), table_hash(&b.ed.model), "{ctx}: rows changed");
        return;
    };
    let (la, lb) = ((p.x as f64, p.y as f64), (q.x as f64, q.y as f64));
    let rb = b.ed.model.mirror_elements(&plan, la, lb, copy);
    assert_eq!(refused(&a.ed), rb.is_err(), "{ctx}: {rb:?}");
    assert_eq!(
        dump(&a.ed.model, &PLAN_TABLES),
        dump(&b.ed.model, &PLAN_TABLES),
        "{ctx}"
    );
}

fn array_equals_plan(seed: u64) {
    let (mut a, mut b) = (Fx::new(seed), Fx::new(seed));
    let mut r = Rng::new(seed ^ 0x5A5A);
    let sel = a.pick(&mut r);
    let (step, count) = array_step(&mut r);
    act3d(&mut a, &sel, Xform::Array { step, count }, false);
    let ctx = format!("seed {seed} count {count} sel {sel:?} status {:?}", a.ed.status);
    let Some(plan) = plan_ts(&sel) else {
        assert_eq!(a.ed.status, Some(refusal(false, "e3d.err.unsupported")), "{ctx}");
        assert_eq!(table_hash(&a.ed.model), table_hash(&b.ed.model), "{ctx}: rows changed");
        return;
    };
    let offs: Vec<(f64, f64)> = (1..count)
        .map(|k| (step.x as f64 * k as f64, step.y as f64 * k as f64))
        .collect();
    let rb = b.ed.model.copy_elements(&plan, &offs);
    assert_eq!(refused(&a.ed), rb.is_err(), "{ctx}: {rb:?}");
    assert_eq!(
        dump(&a.ed.model, &PLAN_TABLES),
        dump(&b.ed.model, &PLAN_TABLES),
        "{ctx}"
    );
}

/// Opening signature by host wall: kind, width, sill, height (position-free).
fn signatures(m: &Model) -> Vec<(i64, Vec<String>)> {
    let mut by: std::collections::BTreeMap<i64, Vec<String>> = Default::default();
    let mut st = m
        .sql()
        .prepare("SELECT wall, kind, width, sill, height FROM opening")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                format!(
                    "{}|{:.6}|{:.6}|{:.6}",
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?
                ),
            ))
        })
        .unwrap();
    for row in rows {
        let (w, s) = row.unwrap();
        by.entry(w).or_default().push(s);
    }
    by.into_iter()
        .map(|(w, mut v)| {
            v.sort();
            (w, v)
        })
        .collect()
}

/// Every opening has an existing host and lies inside its length.
fn assert_inside_hosts(m: &Model, ctx: &str) {
    let mut st = m
        .sql()
        .prepare(
            "SELECT o.id, o.along, o.width, w.ax, w.ay, w.bx, w.by \
             FROM opening o LEFT JOIN wall w ON w.id=o.wall",
        )
        .unwrap();
    let rows: Vec<_> = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<f64>>(6)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    for (id, along, width, ax, ay, bx, by) in rows {
        let (Some(ax), Some(ay), Some(bx), Some(by)) = (ax, ay, bx, by) else {
            panic!("{ctx}: opening {id} lost its host");
        };
        let len = (bx - ax).hypot(by - ay);
        assert!(
            along >= -1e-6 && along + width <= len + 1e-6,
            "{ctx}: opening {id} at {along} + {width} leaves its wall of length {len}"
        );
    }
}

fn hosted_stay_in_copied_host(seed: u64) {
    let mut a = Fx::new(seed);
    let mut r = Rng::new(seed ^ 0x3C3C);
    let sel = a.pick_hosts(&mut r);
    let before = signatures(&a.ed.model);
    let hosts: Vec<i64> = sel
        .iter()
        .filter(|t| t.cat == Cat::Wall)
        .map(|t| t.id)
        .collect();
    let max_wall = *a.walls.iter().max().unwrap();
    let (x, copies, copy) = if r.coin(50) {
        let (p, q) = mirror_line(&mut r);
        (Xform::Mirror { a: p, b: q }, 1usize, r.coin(70))
    } else {
        let (step, count) = array_step(&mut r);
        (Xform::Array { step, count }, count as usize - 1, true)
    };
    act3d(&mut a, &sel, x, copy);
    let ctx = format!("seed {seed} {x:?} copy {copy} hosts {hosts:?}");
    assert!(!refused(&a.ed), "{ctx}: {:?}", a.ed.status);
    assert_inside_hosts(&a.ed.model, &ctx);
    let after = signatures(&a.ed.model);
    let sig_of = |v: &[(i64, Vec<String>)], w: i64| -> Vec<String> {
        v.iter()
            .find(|e| e.0 == w)
            .map(|e| e.1.clone())
            .unwrap_or_default()
    };
    // originals keep their own openings
    for &w in &a.walls {
        assert_eq!(sig_of(&before, w), sig_of(&after, w), "{ctx}: wall {w}");
    }
    // each copied host carries a full set of its source's openings
    let mut want: Vec<Vec<String>> = vec![];
    if copy {
        for &h in &hosts {
            want.extend(std::iter::repeat(sig_of(&before, h)).take(copies));
        }
    }
    let mut got: Vec<Vec<String>> = after
        .iter()
        .filter(|e| e.0 > max_wall)
        .map(|e| e.1.clone())
        .collect();
    want.retain(|s| !s.is_empty());
    want.sort();
    got.sort();
    assert_eq!(got, want, "{ctx}");
}

fn mirror_array_one_undo(seed: u64) {
    let mut a = Fx::new(seed);
    let mut r = Rng::new(seed ^ 0xC3C3);
    let sel = a.pick(&mut r);
    let x = if r.coin(50) {
        let (p, q) = mirror_line(&mut r);
        Xform::Mirror { a: p, b: q }
    } else {
        let (step, count) = array_step(&mut r);
        Xform::Array { step, count }
    };
    let copy = r.coin(50);
    let h0 = table_hash(&a.ed.model);
    assert!(!a.ed.can_undo(), "fixture leaves no undo step");
    act3d(&mut a, &sel, x, copy);
    let ctx = format!("seed {seed} {x:?} copy {copy} sel {sel:?} status {:?}", a.ed.status);
    if refused(&a.ed) {
        assert_eq!(table_hash(&a.ed.model), h0, "{ctx}: refusal wrote rows");
        assert!(!a.ed.can_undo(), "{ctx}: refusal left an undo step");
        return;
    }
    assert_ne!(table_hash(&a.ed.model), h0, "{ctx}: nothing changed");
    a.ed.undo();
    assert_eq!(table_hash(&a.ed.model), h0, "{ctx}: one undo");
    assert!(!a.ed.can_undo(), "{ctx}: more than one step");
}

fn refused_kind_named_in_status(seed: u64) {
    let mut a = Fx::new(seed);
    let mut r = Rng::new(seed ^ 0x7E7E);
    let mut sel = a.pick(&mut r);
    let at = r.below(sel.len() as u64 + 1) as usize;
    sel.insert(at, Target::new(Cat::Stair, a.stair));
    let (p, q) = mirror_line(&mut r);
    let (step, count) = array_step(&mut r);
    let h0 = table_hash(&a.ed.model);
    for ru in [false, true] {
        for x in [Xform::Mirror { a: p, b: q }, Xform::Array { step, count }] {
            for copy in [false, true] {
                a.ed.status = None;
                a.ed.status_sev = None;
                let mut e = Edit3d::default();
                e.ru = ru;
                e.set_sel(&mut a.ed, sel.clone());
                e.commit_xform(&mut a.ed, x, copy);
                let want = refusal(ru, "e3d.err.unsupported");
                let ctx = format!("seed {seed} ru {ru} {x:?} copy {copy}");
                assert_eq!(a.ed.status.as_deref(), Some(want.as_str()), "{ctx}");
                assert_eq!(table_hash(&a.ed.model), h0, "{ctx}: rows changed");
                assert!(!a.ed.can_undo(), "{ctx}: undo step left");
            }
        }
    }
}

#[test]
fn mirror3d_equals_plan_mirror_seeded() {
    (0..seeds()).for_each(mirror_equals_plan);
}

#[test]
fn array3d_equals_plan_copies_seeded() {
    (0..seeds()).for_each(array_equals_plan);
}

#[test]
fn hosted_stay_in_copied_host_seeded() {
    (0..seeds()).for_each(hosted_stay_in_copied_host);
}

#[test]
fn mirror_array_one_undo_restores_hash() {
    (0..seeds()).for_each(mirror_array_one_undo);
}

#[test]
fn stair_in_selection_refused_with_status() {
    (0..seeds()).for_each(refused_kind_named_in_status);
}
