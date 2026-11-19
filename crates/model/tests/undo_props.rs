//! Undo property test across element kinds: random create/edit/modify/delete sequences,
//! integrity after every step, full undo back to each recorded state, replay ("redo")
//! reproduces the final state. `DDC_UNDO_SEQS` sets the count (default 40; 5000 in release gates).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ddc_model::{
    BeamJoin, BeamSpec, ColumnSpec, DimKind, DimRef, DimSpec, GridSpec, LayerSpec, Model,
    OpeningSpec, RoofKind, RoofSpec, ShaftSpec, SlabSpec, StairSpec, Target, WallSpec,
};
use rusqlite::types::Value;

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
    l1: i64,
    l2: i64,
    wt: i64,
    st: i64,
    ct: i64,
    bt: i64,
}

fn base() -> (Model, Base) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let lay = |t| LayerSpec {
        material: "Concrete".into(),
        thickness: t,
    };
    let wt = m.add_wall_type("W", &[lay(0.2)]).unwrap();
    let st = m.add_slab_type("S", &[lay(0.2)]).unwrap();
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let bt = m.add_catalog_member_type("IPE 300").unwrap();
    (
        m,
        Base {
            l1,
            l2,
            wt,
            st,
            ct,
            bt,
        },
    )
}

/// Hash of every row of every table (journal included), in rowid order.
fn content_hash(m: &Model) -> u64 {
    let mut h = DefaultHasher::new();
    dump(m).hash(&mut h);
    h.finish()
}

/// Rows as text per table; used for failure diffs.
fn dump(m: &Model) -> Vec<(String, Vec<String>)> {
    let db = m.sql();
    let mut st = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    let mut out = vec![];
    for t in tables {
        let mut v = vec![];
        let mut q = db
            .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))
            .unwrap();
        let n = q.column_count();
        // the stamp of a journal row is a nonce: a replay writes another one
        let skip = if t == "op" {
            q.column_names().position(|c| c == "stamp")
        } else {
            None
        };
        let mut rows = q.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            let cells: Vec<String> = (0..n)
                .filter(|i| Some(*i) != skip)
                .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                .collect();
            // the project identity is random per project, not content
            if t == "meta" && cells.first().is_some_and(|c| c.contains("project_uid")) {
                continue;
            }
            v.push(cells.join("|"));
        }
        // Row order of quantity tables depends on the incremental history; values do not.
        if t.ends_with("_qty") {
            v.sort();
        }
        out.push((t, v));
    }
    out
}

fn diff(a: &[(String, Vec<String>)], b: &[(String, Vec<String>)]) -> String {
    let mut s = String::new();
    for (x, y) in a.iter().zip(b) {
        if x != y {
            s += &format!(
                "\n[{}]\n  now: {:?}\n  want: {:?}",
                x.0, x.1, y.1
            );
        }
    }
    s
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

#[derive(Clone, Debug)]
enum Op {
    Wall(WallSpec),
    DelWall(i64),
    Opening(i64, OpeningSpec),
    EditOpening(i64, OpeningSpec),
    Move(Vec<Target>, (f64, f64)),
    Column(ColumnSpec),
    EditColumn(i64, ColumnSpec),
    DelColumn(i64),
    Beam(BeamSpec),
    EditBeam(i64, BeamSpec),
    DelBeam(i64),
    Slab(SlabSpec),
    DelSlab(i64),
    Roof(RoofSpec),
    EditRoof(i64, RoofSpec),
    DelRoof(i64),
    Grid(GridSpec),
    EditGrid(i64, GridSpec),
    DelGrid(i64),
    Stair(StairSpec),
    EditStair(i64, StairSpec),
    DelStair(i64),
    Shaft(ShaftSpec),
    EditShaft(i64, ShaftSpec),
    DelShaft(i64),
    Dim(DimSpec),
    EditDim(i64, DimSpec),
    DelDim(i64),
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn pick(r: &mut Rng, v: &[i64]) -> Option<i64> {
    (!v.is_empty()).then(|| v[r.below(v.len())])
}

fn gen(r: &mut Rng, m: &Model, b: &Base) -> Op {
    let p = |r: &mut Rng| (r.f(-20.0, 20.0), r.f(-20.0, 20.0));
    let col = |r: &mut Rng| ColumnSpec {
        level: b.l1,
        top_level: Some(b.l2),
        member_type: b.ct,
        structural: r.below(2) == 0,
        at: p(r),
        base_offset: 0.0,
        top_offset: r.f(-0.5, 0.0),
        rotation: r.f(0.0, 1.5),
        top_at: None,
    };
    let beam = |r: &mut Rng| {
        let a = p(r);
        BeamSpec {
            level: b.l2,
            member_type: b.bt,
            a: (a.0, a.1, 0.0),
            b: (a.0 + r.f(1.0, 6.0), a.1 + r.f(-2.0, 2.0), 0.0),
            bulge: 0.0,
            yjust: Default::default(),
            zjust: Default::default(),
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        }
    };
    let roof = |r: &mut Rng| {
        let (x, y) = p(r);
        RoofSpec {
            level: b.l2,
            slab_type: b.st,
            offset: r.f(0.0, 1.0),
            overhang: r.f(0.0, 0.6),
            fascia: 0.0,
            kind: RoofKind::Footprint {
                ring: rect(x, y, r.f(3.0, 8.0), r.f(3.0, 8.0)),
                slopes: vec![Some(0.5), None, Some(0.5), None],
                mansard: None,
            },
        }
    };
    let grid = |r: &mut Rng| {
        let a = p(r);
        GridSpec {
            name: None,
            a,
            b: (a.0 + r.f(2.0, 10.0), a.1),
            bulge: 0.0,
            base_level: None,
            top_level: None,
            bubble_start: true,
            bubble_end: r.below(2) == 0,
        }
    };
    let stair = |r: &mut Rng| StairSpec::straight(b.l1, b.l2, p(r), r.f(0.0, 6.0));
    let shaft = |r: &mut Rng| {
        let (x, y) = p(r);
        ShaftSpec {
            level: b.l1,
            top_level: b.l2,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: rect(x, y, r.f(0.8, 2.0), r.f(0.8, 2.0)),
        }
    };
    let opening = |r: &mut Rng| OpeningSpec {
        along: r.f(0.5, 1.5),
        width: r.f(0.6, 1.0),
        sill: if r.below(2) == 0 { 0.0 } else { 0.9 },
        height: 1.2,
        kind: if r.below(2) == 0 { "door" } else { "window" }.into(),
    };
    let walls = ids(m, "wall");
    let dim = |r: &mut Rng, w: i64| DimSpec {
        level: b.l1,
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
    };
    let any = |r: &mut Rng, t: &str| pick(r, &ids(m, t));
    loop {
        let op = match r.below(28) {
            0..=2 => {
                let a = p(r);
                let len = r.f(2.0, 8.0);
                Some(Op::Wall(WallSpec {
                    a,
                    b: (a.0 + len, a.1 + r.f(-1.0, 1.0)),
                    level: b.l1,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: b.wt,
                    offset0: Some(0.0),
                }))
            }
            3 => pick(r, &walls).map(Op::DelWall),
            4 => pick(r, &walls).map(|w| Op::Opening(w, opening(r))),
            5 => any(r, "opening").map(|o| Op::EditOpening(o, opening(r))),
            6 => pick(r, &walls).map(|w| {
                let mut t = vec![Target::Wall(w)];
                if let Some(s) = any(r, "slab") {
                    t.push(Target::Slab(s));
                }
                Op::Move(t, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))
            }),
            7 | 8 => Some(Op::Column(col(r))),
            9 => any(r, "col").map(|c| Op::EditColumn(c, col(r))),
            10 => any(r, "col").map(Op::DelColumn),
            11 | 12 => Some(Op::Beam(beam(r))),
            13 => any(r, "beam").map(|c| Op::EditBeam(c, beam(r))),
            14 => any(r, "beam").map(Op::DelBeam),
            15 => {
                let (x, y) = p(r);
                Some(Op::Slab(SlabSpec {
                    level: b.l1,
                    slab_type: b.st,
                    offset: 0.0,
                    outer: rect(x, y, r.f(2.0, 6.0), r.f(2.0, 6.0)),
                    holes: vec![],
                }))
            }
            16 => any(r, "slab").map(Op::DelSlab),
            17 => Some(Op::Roof(roof(r))),
            18 => any(r, "roof").map(|x| Op::EditRoof(x, roof(r))),
            19 => any(r, "roof").map(Op::DelRoof),
            20 => Some(Op::Grid(grid(r))),
            21 => any(r, "grid").map(|x| Op::EditGrid(x, grid(r))),
            22 => any(r, "grid").map(Op::DelGrid),
            23 => Some(Op::Stair(stair(r))),
            24 => any(r, "stair").map(|x| match r.below(2) {
                0 => Op::EditStair(x, stair(r)),
                _ => Op::DelStair(x),
            }),
            25 => Some(Op::Shaft(shaft(r))),
            26 => any(r, "shaft").map(|x| match r.below(2) {
                0 => Op::EditShaft(x, shaft(r)),
                _ => Op::DelShaft(x),
            }),
            _ => match any(r, "dim") {
                Some(d) if r.below(2) == 0 => Some(match pick(r, &walls) {
                    Some(w) if r.below(2) == 0 => Op::EditDim(d, dim(r, w)),
                    _ => Op::DelDim(d),
                }),
                _ => pick(r, &walls).map(|w| Op::Dim(dim(r, w))),
            },
        };
        if let Some(op) = op {
            return op;
        }
    }
}

/// Applies one op; `Ok(true)` — the model changed (one journal entry), `Ok(false)` — rejected.
fn apply(m: &mut Model, op: &Op) -> bool {
    let r: Result<(), ddc_model::Error> = match op {
        Op::Wall(s) => m.add_wall(s).map(drop),
        Op::DelWall(i) => m.delete_wall(*i),
        Op::Opening(w, s) => m.add_opening(*w, s).map(drop),
        Op::EditOpening(i, s) => m.update_opening(*i, s),
        Op::Move(t, v) => m.move_elements(t, *v),
        Op::Column(s) => m.add_column(s).map(drop),
        Op::EditColumn(i, s) => m.update_column(*i, s),
        Op::DelColumn(i) => m.delete_column(*i),
        Op::Beam(s) => m.add_beam(s).map(drop),
        Op::EditBeam(i, s) => m.update_beam(*i, s),
        Op::DelBeam(i) => m.delete_beam(*i),
        Op::Slab(s) => m.add_slab(s).map(drop),
        Op::DelSlab(i) => m.delete_slab(*i),
        Op::Roof(s) => m.add_roof(s).map(drop),
        Op::EditRoof(i, s) => m.update_roof(*i, s),
        Op::DelRoof(i) => m.delete_roof(*i),
        Op::Grid(s) => m.add_grid(s).map(drop),
        Op::EditGrid(i, s) => m.update_grid(*i, s),
        Op::DelGrid(i) => m.delete_grid(*i),
        Op::Stair(s) => m.add_stair(s).map(drop),
        Op::EditStair(i, s) => m.update_stair(*i, s),
        Op::DelStair(i) => m.delete_stair(*i),
        Op::Shaft(s) => m.add_shaft(s).map(drop),
        Op::EditShaft(i, s) => m.update_shaft(*i, s),
        Op::DelShaft(i) => m.delete_shaft(*i),
        Op::Dim(s) => m.add_dim(s).map(drop),
        Op::EditDim(i, s) => m.update_dim(*i, s),
        Op::DelDim(i) => m.delete_dim(*i),
    };
    r.is_ok()
}

fn journal_len(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

fn run_seq(seed: u64, steps: usize) -> usize {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let (mut m, b) = base();
    let mut hashes = vec![content_hash(&m)];
    let mut dumps = vec![dump(&m)];
    let mut done: Vec<Op> = vec![];
    for _ in 0..steps {
        let op = gen(&mut r, &m, &b);
        let before = (journal_len(&m), *hashes.last().unwrap());
        let ok = apply(&mut m, &op);
        let h = content_hash(&m);
        if std::env::var("DDC_UNDO_SEED").is_ok() {
            eprintln!("{ok} {op:?}");
        }
        if ok {
            assert_eq!(
                journal_len(&m),
                before.0 + 1,
                "seed {seed}: {op:?} not one op"
            );
            hashes.push(h);
            dumps.push(dump(&m));
            done.push(op.clone());
        } else {
            assert_eq!(
                h, before.1,
                "seed {seed}: rejected {op:?} changed the model"
            );
        }
        let iss = m.check_integrity().unwrap();
        assert!(iss.is_empty(), "seed {seed} after {op:?}: {iss:?}");
    }
    let last = *hashes.last().unwrap();
    for k in (0..done.len()).rev() {
        m.undo().unwrap();
        if content_hash(&m) != hashes[k] {
            panic!(
                "seed {seed}: undo of {:?}{}",
                done[k],
                diff(&dump(&m), &dumps[k])
            );
        }
        assert!(
            m.check_integrity().unwrap().is_empty(),
            "seed {seed}: undo {k}"
        );
    }
    // redo = replay of the recorded ops from the base state
    let (mut re, _) = base();
    for (k, op) in done.iter().enumerate() {
        assert!(apply(&mut re, op), "seed {seed}: redo {op:?} failed");
        if content_hash(&re) != hashes[k + 1] {
            panic!("seed {seed}: redo {k}{}", diff(&dump(&re), &dumps[k + 1]));
        }
    }
    assert_eq!(content_hash(&re), last);
    done.len()
}

#[test]
fn random_sequences_undo_and_redo_exactly() {
    let n: u64 = std::env::var("DDC_UNDO_SEQS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let t = std::time::Instant::now();
    let mut applied = 0;
    let only: Option<u64> = std::env::var("DDC_UNDO_SEED")
        .ok()
        .and_then(|s| s.parse().ok());
    for s in 0..n {
        if only.is_some_and(|o| o != s + 1) {
            continue;
        }
        applied += run_seq(s + 1, 12);
    }
    eprintln!(
        "UNDO seqs={n} applied_ops={applied} secs={:.1}",
        t.elapsed().as_secs_f64()
    );
}
