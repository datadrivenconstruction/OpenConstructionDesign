//! Randomized (seeded) op sequences per model op family, driven through the Editor
//! (`run_op` / `apply_xlsx`, `undo`, `redo`). Invariants: integrity clean after every step;
//! a rejected op changes nothing and pushes no undo step; an accepted op writes exactly one
//! journal entry and truncates redo; undo-all restores every recorded state exactly (journal
//! included); redo-all reproduces them exactly; a random undo/redo walk with new ops agrees.
//! `DDC_E2E_SEQS` sets the sequence count (default 8), `DDC_E2E_SEED` reruns one seed.
//!
//! not covered: `set_current_phase` writes `meta` outside the journal (not an undo step).
//! not covered: `Placement` is a value type with no mutating API (placement goes via `CompSpec`).
//! not covered: `set_distribution` / `release_distribution` need a distribution rule fixture.
//! not covered: clash statuses — `clash_item` lives outside the journal (skipped in dumps).
//! not covered: Editor gesture commands (wall tool, curtain/section/level Cmd variants) need UI
//! events; `run_op` and `apply_xlsx` exercise the same undo/redo stack.

mod e2e_common;

use std::f64::consts::{PI, TAU};
use std::fmt::Debug;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;

use ddc_app::editor::{Editor, OpFn};
use ddc_geom::shape_edit::SlabShape;
use ddc_geom::sketch::{ElevationProfile, Sketch, SketchCurve};
use ddc_geom::v2;
use ddc_model::{
    AlignEdge, BeamJoin, BeamSpec, BoolOp, CeilingSpec, ColumnSpec, CompHost, CompSpec,
    ConstraintKind, ConstraintSpec, CurtainSpec, DimKind, DimRef, DimSpec, Edit, Error, FaceClass,
    Form, GridLines, GridSpec, LayerSpec, MassSpec, MepDomain, MepSection, MepShape, Model,
    MullionSection, OpeningSpec, PVal, PanelKind, PanelSpec, ParamSet, ParamSpec, ParamType,
    PhaseFilter, RailingHost, RailingSpec, Reference, Revision, RoofKind, RoofSpec, RoomSpec,
    RunSpec, SepSpec, ShaftSpec, SheetDef, SketchHost, SlabSpec, Spacing, StairSpec, Swing,
    SystemKind, TagCat, TagHost, TagSpec, Target, ViewKey, ViewPhase, ViewRef, ViewportDef,
    WallSide, WallSpec,
};
use e2e_common::{self as ec, Rng, TableRows};

type P = (f64, f64);

/// Steps of the linear phase per sequence (the walk phase runs twice as many actions).
const STEPS: usize = 10;
/// Id that never exists: feeds the rejection path.
const MISSING: i64 = 9_999;
/// Outside the journal (clash statuses): not part of the undo contract.
const SKIP: &[&str] = &["clash_item"];

fn snap(m: &Model) -> Vec<TableRows> {
    ec::dump_except(m, SKIP)
}

// ---------------------------------------------------------------- fixture

struct Fx {
    l1: i64,
    l2: i64,
    wt: i64,
    wt2: i64,
    st: i64,
    ct: i64,
    bt: i64,
    table: i64,
    chair: i64,
    basin: i64,
}

/// Shared base written straight into the model (not on the Editor undo stack).
fn fixture(ed: &mut Editor) -> Fx {
    let l1 = ed.level;
    let wt = ed.types[1].id;
    let wt2 = ed.types[0].id;
    let m = &mut ed.model;
    let l2 = m.add_level("L2", 3.0).unwrap();
    let lay = LayerSpec {
        material: "concrete".into(),
        thickness: 0.2,
    };
    let st = m.add_slab_type("Slab 200", &[lay]).unwrap();
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let bt = m.add_catalog_member_type("IPE 300").unwrap();
    let ctype = |m: &mut Model, fam: &str, ty: &str| {
        let f = m.load_library_family(fam).unwrap();
        m.comp_type_id(f, ty).unwrap().unwrap()
    };
    let table = ctype(m, "Table", "1600x800");
    let chair = ctype(m, "Chair", "450x500");
    let basin = ctype(m, "Washbasin", "500");
    m.add_mep_system("Water", SystemKind::ColdWater).unwrap();
    m.add_mep_system("Air", SystemKind::SupplyAir).unwrap();
    ed.reload().unwrap();
    Fx {
        l1,
        l2,
        wt,
        wt2,
        st,
        ct,
        bt,
        table,
        chair,
        basin,
    }
}

type Prep = fn(&mut Model, &Fx);

fn prep_none(_: &mut Model, _: &Fx) {}

/// 8×6 m room of four walls on L1, a door in the first wall, a room inside.
fn prep_box(m: &mut Model, fx: &Fx) {
    let c = [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)];
    let ws: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: fx.l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: fx.wt,
            offset0: None,
        })
        .collect();
    let w = m.add_walls(&ws).unwrap();
    m.add_opening(
        w[0],
        &OpeningSpec {
            along: 2.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    m.add_room(&RoomSpec {
        level: fx.l1,
        name: "Room".into(),
        number: "1".into(),
        point: (4.0, 3.0),
        height: 2.7,
    })
    .unwrap();
}

/// Box plus a floor slab on L1 and a slab on L2 over it.
fn prep_slabs(m: &mut Model, fx: &Fx) {
    prep_box(m, fx);
    for (level, ring) in [
        (fx.l1, rect(0.0, 0.0, 8.0, 6.0)),
        (fx.l2, rect(-0.5, -0.5, 9.0, 7.0)),
    ] {
        m.add_slab(&SlabSpec {
            level,
            slab_type: fx.st,
            offset: 0.0,
            outer: ring,
            holes: vec![],
        })
        .unwrap();
    }
}

fn prep_stair(m: &mut Model, fx: &Fx) {
    m.add_stair(&StairSpec::straight(fx.l1, fx.l2, (0.0, 0.0), 0.0))
        .unwrap();
}

fn prep_sheet(m: &mut Model, _: &Fx) {
    m.add_sheet(&SheetDef::new("A-101", "Plans")).unwrap();
}

fn prep_mass(m: &mut Model, fx: &Fx) {
    m.add_mass(&MassSpec {
        level: fx.l1,
        name: "Tower".into(),
        void: false,
        inplace: false,
        form: Form::Extrude {
            ring: rect(0.0, 0.0, 10.0, 6.0),
            z0: 0.0,
            h: 9.0,
            ids: vec![],
        },
    })
    .unwrap();
}

// ---------------------------------------------------------------- runner

/// One model operation replayable by the Editor (`Cmd::Op`): exactly one journal entry.
trait ModelOp: Clone + Debug + Send + Sync + 'static {
    fn apply(&self, m: &mut Model) -> Result<(), Error>;
}

fn via_op<O: ModelOp>(ed: &mut Editor, o: &O) -> Result<(), String> {
    let o = o.clone();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| o.apply(m))))
        .map_err(|e| e.to_string())
}

enum Step {
    Nothing,
    Rejected,
    Accepted(Vec<TableRows>),
}

/// Generates and applies one op; checks the accepted/rejected contract against `cur`.
fn try_op<O, G, A>(
    r: &mut Rng,
    ed: &mut Editor,
    fx: &Fx,
    gen: &G,
    apply: &A,
    cur: &[TableRows],
    log: &mut Vec<String>,
) -> Step
where
    O: Debug,
    G: Fn(&mut Rng, &Model, &Fx) -> Option<O>,
    A: Fn(&mut Editor, &O) -> Result<(), String>,
{
    let Some(op) = (0..32).find_map(|_| gen(r, &ed.model, fx)) else {
        return Step::Nothing;
    };
    let j0 = ec::journal_len(&ed.model);
    let redo0 = ed.can_redo();
    let res = apply(ed, &op);
    log.push(format!(
        "{:>3} {} {op:?}",
        log.len(),
        match &res {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("rejected ({e})"),
        }
    ));
    let now = snap(&ed.model);
    let out = match res {
        Ok(()) => {
            assert_eq!(
                ec::journal_len(&ed.model),
                j0 + 1,
                "accepted op must write exactly one journal entry: {op:?}"
            );
            assert!(ed.can_undo(), "accepted op left no undo step: {op:?}");
            assert!(!ed.can_redo(), "a new op must truncate redo: {op:?}");
            Step::Accepted(now)
        }
        Err(_) => {
            ec::assert_same(&now, cur, &format!("rejected {op:?} changed the model"));
            assert_eq!(
                ec::journal_len(&ed.model),
                j0,
                "rejected {op:?} wrote the journal"
            );
            assert_eq!(
                ed.can_redo(),
                redo0,
                "rejected {op:?} touched the redo stack"
            );
            Step::Rejected
        }
    };
    ec::assert_clean(&ed.model, &format!("after {op:?}"));
    out
}

fn one_seq<O, G, A>(
    seed: u64,
    prep: Prep,
    gen: &G,
    apply: &A,
    log: &mut Vec<String>,
) -> (usize, usize)
where
    O: Debug,
    G: Fn(&mut Rng, &Model, &Fx) -> Option<O>,
    A: Fn(&mut Editor, &O) -> Result<(), String>,
{
    let mut r = Rng::new(seed);
    let mut ed = Editor::new_in_memory().unwrap();
    let fx = fixture(&mut ed);
    prep(&mut ed.model, &fx);
    ed.reload().unwrap();
    ec::assert_clean(&ed.model, "fixture");
    assert!(
        !ed.can_undo() && !ed.can_redo(),
        "fixture is not on the undo stack"
    );
    let mut hist = vec![snap(&ed.model)];
    let mut rejected = 0;

    // linear phase: N steps, then undo all, then redo all
    for _ in 0..STEPS {
        let cur = hist.last().unwrap().clone();
        match try_op(&mut r, &mut ed, &fx, gen, apply, &cur, log) {
            Step::Accepted(d) => hist.push(d),
            Step::Rejected => rejected += 1,
            Step::Nothing => {}
        }
    }
    let accepted = hist.len() - 1;
    for k in (0..accepted).rev() {
        assert!(
            ed.can_undo(),
            "undo stack shorter than accepted ops (state {k})"
        );
        ed.undo();
        ec::assert_same(
            &snap(&ed.model),
            &hist[k],
            &format!("undo back to state {k}"),
        );
        ec::assert_clean(&ed.model, &format!("undo to state {k}"));
    }
    assert!(
        !ed.can_undo(),
        "undo stack holds more steps than accepted ops"
    );
    let base = ec::hash_of(&hist[0]);
    assert_eq!(ec::hash_of(&snap(&ed.model)), base);
    for k in 1..=accepted {
        assert!(
            ed.can_redo(),
            "redo stack shorter than accepted ops (state {k})"
        );
        ed.redo();
        ec::assert_same(&snap(&ed.model), &hist[k], &format!("redo to state {k}"));
        ec::assert_clean(&ed.model, &format!("redo to state {k}"));
    }
    assert!(
        !ed.can_redo(),
        "redo stack holds more steps than accepted ops"
    );
    let mut want = hist[accepted].clone();
    want.retain(|t| t.0 != "op");
    let mut now = ec::dump_data(&ed.model);
    now.retain(|t| !SKIP.contains(&t.0.as_str()));
    assert_eq!(
        ec::hash_of(&now),
        ec::hash_of(&want),
        "final data after redo-all"
    );

    // walk phase: random undo ×k / redo ×j / new op (truncates redo)
    let mut pos = accepted;
    let mut walked = 0;
    for _ in 0..2 * STEPS {
        match r.below(3) {
            0 => {
                let k = 1 + r.below(3);
                for _ in 0..k.min(pos) {
                    ed.undo();
                    pos -= 1;
                    ec::assert_same(&snap(&ed.model), &hist[pos], &format!("walk undo to {pos}"));
                }
                log.push(format!("    undo x{k} -> state {pos}"));
            }
            1 => {
                let j = 1 + r.below(3);
                for _ in 0..j.min(hist.len() - 1 - pos) {
                    ed.redo();
                    pos += 1;
                    ec::assert_same(&snap(&ed.model), &hist[pos], &format!("walk redo to {pos}"));
                }
                log.push(format!("    redo x{j} -> state {pos}"));
            }
            _ => {
                let cur = hist[pos].clone();
                match try_op(&mut r, &mut ed, &fx, gen, apply, &cur, log) {
                    Step::Accepted(d) => {
                        hist.truncate(pos + 1);
                        hist.push(d);
                        pos += 1;
                        walked += 1;
                    }
                    Step::Rejected => rejected += 1,
                    Step::Nothing => {}
                }
            }
        }
        assert_eq!(ed.can_undo(), pos > 0, "walk: can_undo at state {pos}");
        assert_eq!(
            ed.can_redo(),
            pos + 1 < hist.len(),
            "walk: can_redo at state {pos}"
        );
        ec::assert_clean(&ed.model, &format!("walk at state {pos}"));
    }
    while pos > 0 {
        ed.undo();
        pos -= 1;
        ec::assert_same(
            &snap(&ed.model),
            &hist[pos],
            &format!("walk unwind to {pos}"),
        );
    }
    assert!(!ed.can_undo(), "walk: extra undo steps after unwinding");
    assert_eq!(
        ec::hash_of(&snap(&ed.model)),
        base,
        "walk: base state not restored"
    );
    (accepted + walked, rejected)
}

/// Runs `DDC_E2E_SEQS` seeded sequences of `gen`/`apply`; prints seed and op list on failure.
fn run_family<O, G, A>(family: &str, prep: Prep, gen: G, apply: A)
where
    O: Debug,
    G: Fn(&mut Rng, &Model, &Fx) -> Option<O>,
    A: Fn(&mut Editor, &O) -> Result<(), String>,
{
    let only: Option<u64> = std::env::var("DDC_E2E_SEED")
        .ok()
        .and_then(|s| s.parse().ok());
    let salt = family
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(131).wrapping_add(u64::from(b)));
    let t = Instant::now();
    let (mut acc, mut rej) = (0, 0);
    for seed in 1..=ec::seqs(8) {
        if only.is_some_and(|o| o != seed) {
            continue;
        }
        let mut log: Vec<String> = vec![];
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| {
            one_seq(seed.wrapping_add(salt), prep, &gen, &apply, &mut log)
        }));
        match out {
            Ok((a, b)) => {
                acc += a;
                rej += b;
            }
            Err(p) => {
                eprintln!(
                    "e2e_rand_{family}: seed {seed} failed (rerun with DDC_E2E_SEED={seed}); ops:\n{}",
                    log.join("\n")
                );
                std::panic::resume_unwind(p);
            }
        }
    }
    eprintln!(
        "e2e_rand_{family}: accepted {acc} rejected {rej} in {:.1}s",
        t.elapsed().as_secs_f64()
    );
    assert!(
        acc > 0 || only.is_some(),
        "e2e_rand_{family}: no op was accepted, the generator is broken"
    );
}

// ---------------------------------------------------------------- generator helpers

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<P> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn pt(r: &mut Rng) -> P {
    (r.f(-12.0, 12.0), r.f(-12.0, 12.0))
}

/// An existing id, or now and then a missing one (rejection path).
fn target(r: &mut Rng, v: &[i64]) -> Option<i64> {
    if r.below(12) == 0 {
        Some(MISSING)
    } else {
        r.pick(v)
    }
}

fn pick_n(r: &mut Rng, v: &[i64], n: usize) -> Option<Vec<i64>> {
    (0..n).map(|_| r.pick(v)).collect()
}

/// First column of a query as ids.
fn sel(m: &Model, sql: &str) -> Vec<i64> {
    let Ok(mut st) = m.sql().prepare(sql) else {
        return vec![];
    };
    let v: Vec<i64> = st
        .query_map([], |r| r.get::<_, i64>(0))
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default();
    v
}

fn axis(m: &Model, w: i64) -> Option<(P, P)> {
    m.sql()
        .query_row("SELECT ax, ay, bx, by FROM wall WHERE id=?1", [w], |r| {
            Ok((
                (r.get::<_, f64>(0)?, r.get::<_, f64>(1)?),
                (r.get::<_, f64>(2)?, r.get::<_, f64>(3)?),
            ))
        })
        .ok()
}

fn wall_spec(r: &mut Rng, fx: &Fx, level: i64) -> WallSpec {
    let a = pt(r);
    let (len, ang) = (r.f(2.0, 8.0), r.f(0.0, TAU));
    WallSpec {
        a,
        b: (a.0 + len * ang.cos(), a.1 + len * ang.sin()),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: if r.below(3) == 0 { fx.wt2 } else { fx.wt },
        offset0: None,
    }
}

/// Axis-parallel wall (parallel/perpendicular constraints and trims hit more often).
fn ortho_wall(r: &mut Rng, fx: &Fx) -> WallSpec {
    let a = pt(r);
    let len = r.f(2.0, 8.0);
    let b = if r.below(2) == 0 {
        (a.0 + len, a.1)
    } else {
        (a.0, a.1 + len)
    };
    WallSpec {
        a,
        b,
        level: fx.l1,
        base_offset: 0.0,
        height: 3.0,
        wall_type: fx.wt,
        offset0: None,
    }
}

/// Partition across the 8×6 box of `prep_box`.
fn box_wall(r: &mut Rng, fx: &Fx) -> WallSpec {
    let (a, b) = if r.below(2) == 0 {
        let x = r.f(1.0, 7.0);
        ((x, 0.0), (x, 6.0))
    } else {
        let y = r.f(1.0, 5.0);
        ((0.0, y), (8.0, y))
    };
    WallSpec {
        a,
        b,
        level: fx.l1,
        base_offset: 0.0,
        height: 3.0,
        wall_type: fx.wt2,
        offset0: None,
    }
}

fn opening(r: &mut Rng) -> OpeningSpec {
    let door = r.below(2) == 0;
    OpeningSpec {
        along: r.f(0.5, 2.5),
        width: r.f(0.6, 1.0),
        sill: if door { 0.0 } else { 0.9 },
        height: if door { 2.1 } else { 1.2 },
        kind: if door { "door" } else { "window" }.into(),
    }
}

fn swing(r: &mut Rng) -> Swing {
    Swing {
        side: r.below(2) == 0,
        hinge: r.below(2) == 0,
    }
}

fn column(r: &mut Rng, fx: &Fx) -> ColumnSpec {
    ColumnSpec {
        level: fx.l1,
        top_level: Some(fx.l2),
        member_type: fx.ct,
        structural: r.below(2) == 0,
        at: pt(r),
        base_offset: 0.0,
        top_offset: r.f(-0.5, 0.0),
        rotation: r.f(0.0, 1.5),
        top_at: None,
    }
}

fn beam(r: &mut Rng, fx: &Fx) -> BeamSpec {
    let a = pt(r);
    BeamSpec {
        level: fx.l2,
        member_type: fx.bt,
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
}

/// Slab that usually covers the 0..2 × 0..2 square (holes and shapes land inside).
fn slab_spec(r: &mut Rng, fx: &Fx, level: i64) -> SlabSpec {
    SlabSpec {
        level,
        slab_type: fx.st,
        offset: 0.0,
        outer: rect(
            r.f(-4.0, 0.0),
            r.f(-4.0, 0.0),
            r.f(6.0, 10.0),
            r.f(6.0, 10.0),
        ),
        holes: vec![],
    }
}

fn roof_spec(r: &mut Rng, fx: &Fx) -> RoofSpec {
    let (x, y) = pt(r);
    RoofSpec {
        level: fx.l2,
        slab_type: fx.st,
        offset: r.f(0.0, 1.0),
        overhang: r.f(0.0, 0.6),
        fascia: 0.0,
        kind: RoofKind::Footprint {
            ring: rect(x, y, r.f(3.0, 8.0), r.f(3.0, 8.0)),
            slopes: vec![Some(0.5), None, Some(0.5), None],
            mansard: None,
        },
    }
}

fn shaft_spec(r: &mut Rng, fx: &Fx) -> ShaftSpec {
    ShaftSpec {
        level: fx.l1,
        top_level: fx.l2,
        base_offset: 0.0,
        top_offset: 0.0,
        ring: rect(r.f(0.0, 2.0), r.f(0.0, 2.0), r.f(0.8, 2.0), r.f(0.8, 2.0)),
    }
}

fn grid_spec(r: &mut Rng) -> GridSpec {
    let a = pt(r);
    let len = r.f(2.0, 10.0);
    GridSpec {
        name: None,
        a,
        b: if r.below(2) == 0 {
            (a.0 + len, a.1)
        } else {
            (a.0, a.1 + len)
        },
        bulge: 0.0,
        base_level: None,
        top_level: None,
        bubble_start: true,
        bubble_end: r.below(2) == 0,
    }
}

fn dim_spec(r: &mut Rng, fx: &Fx, w: i64) -> DimSpec {
    DimSpec {
        level: fx.l1,
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

fn room_spec(r: &mut Rng, fx: &Fx) -> RoomSpec {
    RoomSpec {
        level: fx.l1,
        name: ["Office", "Hall", "Store"][r.below(3)].into(),
        number: format!("{}", r.below(20)),
        point: (r.f(0.5, 7.5), r.f(0.5, 5.5)),
        height: if r.below(10) == 0 {
            -1.0
        } else {
            r.f(2.4, 3.0)
        },
    }
}

fn param_spec(r: &mut Rng) -> ParamSpec {
    let cats = ["wall", "room", "door"];
    ParamSpec {
        name: ["Cost", "Fire rating", "Code", "Count"][r.below(4)].into(),
        ptype: ParamType::ALL[r.below(ParamType::ALL.len())],
        per_type: r.below(6) == 0,
        unit: String::new(),
        group: "Data".into(),
        guid: None,
        categories: cats
            .iter()
            .filter(|_| r.below(2) == 0)
            .map(|c| c.to_string())
            .collect(),
    }
}

fn pval(r: &mut Rng, t: ParamType) -> Option<PVal> {
    if r.below(8) == 0 {
        return None;
    }
    if r.below(8) == 0 {
        return Some(PVal::Text("mismatch".into()));
    }
    Some(match t {
        ParamType::Text => PVal::Text(format!("v{}", r.below(10))),
        ParamType::Int => PVal::Int(r.below(100) as i64),
        ParamType::YesNo => PVal::Bool(r.below(2) == 0),
        _ => PVal::Num(r.f(0.0, 50.0)),
    })
}

fn rect_sketch(x: f64, y: f64, w: f64, h: f64) -> Sketch {
    let c = rect(x, y, w, h);
    Sketch {
        curves: (0..4)
            .map(|k| SketchCurve::Line {
                a: v2(c[k].0, c[k].1),
                b: v2(c[(k + 1) % 4].0, c[(k + 1) % 4].1),
            })
            .collect(),
    }
}

fn circle_sketch(c: P, rad: f64) -> Sketch {
    Sketch {
        curves: vec![SketchCurve::Ellipse {
            c: v2(c.0, c.1),
            rx: rad,
            ry: rad,
            rot: 0.0,
            t0: 0.0,
            sweep: TAU,
        }],
    }
}

// ---------------------------------------------------------------- walls / openings / move

#[derive(Clone, Debug)]
enum WallOp {
    Add(WallSpec),
    Arc(WallSpec, f64),
    Axis(i64, P, P),
    Type(i64, i64),
    Del(i64),
    Split(i64, f64),
    Opening(i64, OpeningSpec, Swing),
    EditOpening(i64, OpeningSpec, Swing),
    Swing(i64, Swing),
    DelOpening(i64),
    Move(Vec<Target>, P),
}

impl ModelOp for WallOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            WallOp::Add(s) => m.add_wall(s).map(drop),
            WallOp::Arc(s, b) => m.add_arc_wall(s, *b).map(drop),
            WallOp::Axis(i, a, b) => m.set_wall_axis(*i, *a, *b),
            WallOp::Type(i, t) => m.set_wall_type(*i, *t),
            WallOp::Del(i) => m.delete_wall(*i),
            WallOp::Split(i, t) => m.split_wall(*i, *t).map(drop),
            WallOp::Opening(w, s, sw) => m.add_opening_with(*w, s, *sw).map(drop),
            WallOp::EditOpening(i, s, sw) => m.update_opening_with(*i, s, *sw),
            WallOp::Swing(i, sw) => m.set_opening_swing(*i, *sw),
            WallOp::DelOpening(i) => m.delete_opening(*i),
            WallOp::Move(t, v) => m.move_elements(t, *v),
        }
    }
}

fn gen_walls(r: &mut Rng, m: &Model, fx: &Fx) -> Option<WallOp> {
    let walls = ec::ids(m, "wall");
    let ops = ec::ids(m, "opening");
    match r.below(13) {
        0 | 1 => Some(WallOp::Add(wall_spec(r, fx, fx.l1))),
        2 => Some(WallOp::Arc(wall_spec(r, fx, fx.l1), r.f(-0.4, 0.4))),
        3 => target(r, &walls).map(|w| {
            let a = pt(r);
            WallOp::Axis(w, a, (a.0 + r.f(-6.0, 6.0), a.1 + r.f(-6.0, 6.0)))
        }),
        4 => {
            target(r, &walls).map(|w| WallOp::Type(w, if r.below(2) == 0 { fx.wt } else { fx.wt2 }))
        }
        5 => target(r, &walls).map(WallOp::Del),
        6 => target(r, &walls).map(|w| WallOp::Split(w, r.f(0.3, 4.0))),
        7 | 8 => target(r, &walls).map(|w| WallOp::Opening(w, opening(r), swing(r))),
        9 => target(r, &ops).map(|o| WallOp::EditOpening(o, opening(r), swing(r))),
        10 => target(r, &ops).map(|o| WallOp::Swing(o, swing(r))),
        11 => target(r, &ops).map(WallOp::DelOpening),
        _ => r.pick(&walls).map(|w| {
            let mut t = vec![Target::Wall(w)];
            if let Some(o) = r.pick(&ops) {
                t.push(Target::Opening(o));
            }
            WallOp::Move(t, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))
        }),
    }
}

// ---------------------------------------------------------------- columns / beams

#[derive(Clone, Debug)]
enum StructOp {
    Col(ColumnSpec),
    EditCol(i64, ColumnSpec),
    DelCol(i64),
    Beam(BeamSpec),
    EditBeam(i64, BeamSpec),
    DelBeam(i64),
}

impl ModelOp for StructOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            StructOp::Col(s) => m.add_column(s).map(drop),
            StructOp::EditCol(i, s) => m.update_column(*i, s),
            StructOp::DelCol(i) => m.delete_column(*i),
            StructOp::Beam(s) => m.add_beam(s).map(drop),
            StructOp::EditBeam(i, s) => m.update_beam(*i, s),
            StructOp::DelBeam(i) => m.delete_beam(*i),
        }
    }
}

fn gen_struct(r: &mut Rng, m: &Model, fx: &Fx) -> Option<StructOp> {
    let (cols, beams) = (ec::ids(m, "col"), ec::ids(m, "beam"));
    match r.below(8) {
        0 | 1 => Some(StructOp::Col(column(r, fx))),
        2 => target(r, &cols).map(|i| StructOp::EditCol(i, column(r, fx))),
        3 => target(r, &cols).map(StructOp::DelCol),
        4 | 5 => Some(StructOp::Beam(beam(r, fx))),
        6 => target(r, &beams).map(|i| StructOp::EditBeam(i, beam(r, fx))),
        _ => target(r, &beams).map(StructOp::DelBeam),
    }
}

// ---------------------------------------------------------------- slabs / roofs / shafts

#[derive(Clone, Debug)]
enum FloorOp {
    Slab(SlabSpec),
    Boundary(i64, Vec<P>),
    DelSlab(i64),
    Hole(i64, Vec<P>),
    DelHole(i64, usize),
    Roof(RoofSpec),
    EditRoof(i64, RoofSpec),
    DelRoof(i64),
    Shaft(ShaftSpec),
    EditShaft(i64, ShaftSpec),
    DelShaft(i64),
}

impl ModelOp for FloorOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            FloorOp::Slab(s) => m.add_slab(s).map(drop),
            FloorOp::Boundary(i, o) => m.set_slab_boundary(*i, o, &[]),
            FloorOp::DelSlab(i) => m.delete_slab(*i),
            FloorOp::Hole(i, ring) => m.add_slab_opening(*i, ring),
            FloorOp::DelHole(i, k) => m.remove_slab_opening(*i, *k),
            FloorOp::Roof(s) => m.add_roof(s).map(drop),
            FloorOp::EditRoof(i, s) => m.update_roof(*i, s),
            FloorOp::DelRoof(i) => m.delete_roof(*i),
            FloorOp::Shaft(s) => m.add_shaft(s).map(drop),
            FloorOp::EditShaft(i, s) => m.update_shaft(*i, s),
            FloorOp::DelShaft(i) => m.delete_shaft(*i),
        }
    }
}

fn gen_floors(r: &mut Rng, m: &Model, fx: &Fx) -> Option<FloorOp> {
    let (slabs, roofs, shafts) = (ec::ids(m, "slab"), ec::ids(m, "roof"), ec::ids(m, "shaft"));
    let level = |r: &mut Rng| if r.below(2) == 0 { fx.l1 } else { fx.l2 };
    match r.below(14) {
        0 | 1 => {
            let l = level(r);
            Some(FloorOp::Slab(slab_spec(r, fx, l)))
        }
        2 => target(r, &slabs).map(|s| {
            FloorOp::Boundary(
                s,
                rect(r.f(-4.0, 0.0), r.f(-4.0, 0.0), r.f(5.0, 9.0), r.f(5.0, 9.0)),
            )
        }),
        3 => target(r, &slabs).map(FloorOp::DelSlab),
        4 | 5 => target(r, &slabs)
            .map(|s| FloorOp::Hole(s, rect(r.f(0.5, 1.5), r.f(0.5, 1.5), 0.4, 0.4))),
        6 => target(r, &slabs).map(|s| FloorOp::DelHole(s, r.below(2))),
        7 | 8 => Some(FloorOp::Roof(roof_spec(r, fx))),
        9 => target(r, &roofs).map(|i| FloorOp::EditRoof(i, roof_spec(r, fx))),
        10 => target(r, &roofs).map(FloorOp::DelRoof),
        11 => Some(FloorOp::Shaft(shaft_spec(r, fx))),
        12 => target(r, &shafts).map(|i| FloorOp::EditShaft(i, shaft_spec(r, fx))),
        _ => target(r, &shafts).map(FloorOp::DelShaft),
    }
}

// ---------------------------------------------------------------- grids / stairs / dims

#[derive(Clone, Debug)]
enum DatumOp {
    Grid(GridSpec),
    EditGrid(i64, GridSpec),
    DelGrid(i64),
    Stair(StairSpec),
    EditStair(i64, StairSpec),
    DelStair(i64),
    Wall(WallSpec),
    Dim(DimSpec),
    EditDim(i64, DimSpec),
    DelDim(i64),
    AutoDims(i64, Vec<i64>),
}

impl ModelOp for DatumOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            DatumOp::Grid(s) => m.add_grid(s).map(drop),
            DatumOp::EditGrid(i, s) => m.update_grid(*i, s),
            DatumOp::DelGrid(i) => m.delete_grid(*i),
            DatumOp::Stair(s) => m.add_stair(s).map(drop),
            DatumOp::EditStair(i, s) => m.update_stair(*i, s),
            DatumOp::DelStair(i) => m.delete_stair(*i),
            DatumOp::Wall(s) => m.add_wall(s).map(drop),
            DatumOp::Dim(s) => m.add_dim(s).map(drop),
            DatumOp::EditDim(i, s) => m.update_dim(*i, s),
            DatumOp::DelDim(i) => m.delete_dim(*i),
            DatumOp::AutoDims(l, w) => m.auto_dimension_walls(*l, w).map(drop),
        }
    }
}

fn gen_datum(r: &mut Rng, m: &Model, fx: &Fx) -> Option<DatumOp> {
    let (grids, stairs, walls) = (ec::ids(m, "grid"), ec::ids(m, "stair"), ec::ids(m, "wall"));
    let dims = sel(m, "SELECT id FROM dim WHERE kind<>'tag' ORDER BY id");
    let stair = |r: &mut Rng| StairSpec::straight(fx.l1, fx.l2, pt(r), r.f(0.0, 6.0));
    match r.below(13) {
        0 | 1 => Some(DatumOp::Grid(grid_spec(r))),
        2 => target(r, &grids).map(|i| DatumOp::EditGrid(i, grid_spec(r))),
        3 => target(r, &grids).map(DatumOp::DelGrid),
        4 => Some(DatumOp::Stair(stair(r))),
        5 => target(r, &stairs).map(|i| DatumOp::EditStair(i, stair(r))),
        6 => target(r, &stairs).map(DatumOp::DelStair),
        7 | 8 => Some(DatumOp::Wall(ortho_wall(r, fx))),
        9 => r.pick(&walls).map(|w| DatumOp::Dim(dim_spec(r, fx, w))),
        10 => target(r, &dims).and_then(|d| {
            r.pick(&walls)
                .map(|w| DatumOp::EditDim(d, dim_spec(r, fx, w)))
        }),
        11 => target(r, &dims).map(DatumOp::DelDim),
        _ => (!walls.is_empty()).then(|| DatumOp::AutoDims(fx.l1, walls.clone())),
    }
}

// ---------------------------------------------------------------- rooms / separators

#[derive(Clone, Debug)]
enum RoomOp {
    Wall(WallSpec),
    DelWall(i64),
    Room(RoomSpec),
    Edit(i64, RoomSpec),
    Del(i64),
    Sep(SepSpec),
    EditSep(i64, SepSpec),
    DelSep(i64),
}

impl ModelOp for RoomOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            RoomOp::Wall(s) => m.add_wall(s).map(drop),
            RoomOp::DelWall(i) => m.delete_wall(*i),
            RoomOp::Room(s) => m.add_room(s).map(drop),
            RoomOp::Edit(i, s) => m.update_room(*i, s),
            RoomOp::Del(i) => m.delete_room(*i),
            RoomOp::Sep(s) => m.add_room_sep(s).map(drop),
            RoomOp::EditSep(i, s) => m.update_room_sep(*i, s),
            RoomOp::DelSep(i) => m.delete_room_sep(*i),
        }
    }
}

fn sep_spec(r: &mut Rng, fx: &Fx) -> SepSpec {
    let w = box_wall(r, fx);
    SepSpec {
        level: if r.below(10) == 0 { MISSING } else { fx.l1 },
        a: w.a,
        b: w.b,
    }
}

fn gen_rooms(r: &mut Rng, m: &Model, fx: &Fx) -> Option<RoomOp> {
    let (walls, rooms, seps) = (
        ec::ids(m, "wall"),
        ec::ids(m, "room"),
        ec::ids(m, "room_sep"),
    );
    match r.below(10) {
        0 => Some(RoomOp::Wall(box_wall(r, fx))),
        1 => target(r, &walls).map(RoomOp::DelWall),
        2 | 3 => Some(RoomOp::Room(room_spec(r, fx))),
        4 => target(r, &rooms).map(|i| RoomOp::Edit(i, room_spec(r, fx))),
        5 => target(r, &rooms).map(RoomOp::Del),
        6 | 7 => Some(RoomOp::Sep(sep_spec(r, fx))),
        8 => target(r, &seps).map(|i| RoomOp::EditSep(i, sep_spec(r, fx))),
        _ => target(r, &seps).map(RoomOp::DelSep),
    }
}

// ---------------------------------------------------------------- tags

#[derive(Clone, Debug)]
enum TagOp {
    Add(Vec<TagSpec>),
    Element(i64, TagHost, bool),
    All(i64, Vec<TagCat>, bool),
    Move(i64, P),
    Leader(i64, bool),
    Del(i64),
    Mark(TagHost, String),
    Wall(WallSpec),
    Opening(i64, OpeningSpec),
    DelWall(i64),
    MoveWall(i64, P),
}

impl ModelOp for TagOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            TagOp::Add(v) => m.add_tags(v).map(drop),
            TagOp::Element(l, h, ld) => m.tag_element(*l, *h, *ld).map(drop),
            TagOp::All(l, c, ld) => m.tag_all(*l, c, *ld).map(drop),
            TagOp::Move(i, o) => m.move_tag(*i, *o),
            TagOp::Leader(i, b) => m.set_tag_leader(*i, *b),
            TagOp::Del(i) => m.delete_tag(*i),
            TagOp::Mark(h, t) => m.set_mark(*h, t),
            TagOp::Wall(s) => m.add_wall(s).map(drop),
            TagOp::Opening(w, s) => m.add_opening(*w, s).map(drop),
            TagOp::DelWall(i) => m.delete_wall(*i),
            TagOp::MoveWall(i, v) => m.move_elements(&[Target::Wall(*i)], *v),
        }
    }
}

fn gen_tags(r: &mut Rng, m: &Model, fx: &Fx) -> Option<TagOp> {
    let (walls, ops, rooms) = (
        ec::ids(m, "wall"),
        ec::ids(m, "opening"),
        ec::ids(m, "room"),
    );
    let tags = sel(m, "SELECT id FROM dim WHERE kind='tag' ORDER BY id");
    let mut hosts: Vec<TagHost> = walls.iter().map(|&i| TagHost::Wall(i)).collect();
    hosts.extend(ops.iter().map(|&i| TagHost::Opening(i)));
    hosts.extend(rooms.iter().map(|&i| TagHost::Room(i)));
    let off = |r: &mut Rng| (r.f(-1.5, 1.5), r.f(-1.5, 1.5));
    match r.below(12) {
        0 | 1 => {
            let n = 1 + r.below(2);
            let v: Option<Vec<TagSpec>> = (0..n)
                .map(|_| {
                    r.pick(&hosts).map(|host| TagSpec {
                        level: fx.l1,
                        host,
                        offset: off(r),
                        leader: r.below(2) == 0,
                    })
                })
                .collect();
            v.map(TagOp::Add)
        }
        2 => r
            .pick(&hosts)
            .map(|h| TagOp::Element(fx.l1, h, r.below(2) == 0)),
        3 => {
            let all = [TagCat::Door, TagCat::Window, TagCat::Room, TagCat::Wall];
            let cats: Vec<TagCat> = all.iter().copied().filter(|_| r.below(2) == 0).collect();
            // tag_all with nothing to tag returns Ok without an op: not an undo step
            let some = !m.untagged(fx.l1, &cats).unwrap_or_default().is_empty();
            some.then(|| TagOp::All(fx.l1, cats, r.below(2) == 0))
        }
        4 => target(r, &tags).map(|t| TagOp::Move(t, off(r))),
        5 => target(r, &tags).map(|t| TagOp::Leader(t, r.below(2) == 0)),
        6 => target(r, &tags).map(TagOp::Del),
        7 => {
            let mut v: Vec<TagHost> = ops.iter().map(|&i| TagHost::Opening(i)).collect();
            v.extend(rooms.iter().map(|&i| TagHost::Room(i)));
            if r.below(8) == 0 {
                v.extend(walls.iter().map(|&i| TagHost::Wall(i)));
            }
            r.pick(&v).map(|h| {
                let text = if r.below(5) == 0 {
                    String::new()
                } else {
                    format!("M{}", r.below(10))
                };
                TagOp::Mark(h, text)
            })
        }
        8 => Some(TagOp::Wall(box_wall(r, fx))),
        9 => target(r, &walls).map(|w| TagOp::Opening(w, opening(r))),
        10 => target(r, &walls).map(TagOp::DelWall),
        _ => target(r, &walls).map(|w| TagOp::MoveWall(w, (r.f(-0.5, 0.5), r.f(-0.5, 0.5)))),
    }
}

// ---------------------------------------------------------------- curtain walls

#[derive(Clone, Debug)]
enum CurtainOp {
    Add(CurtainSpec),
    Set(i64, CurtainSpec),
    Panel(i64, u32, u32, PanelSpec),
    Del(i64),
    Move(i64, P),
}

impl ModelOp for CurtainOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            CurtainOp::Add(s) => m.add_curtain_wall(s).map(drop),
            CurtainOp::Set(i, s) => m.set_curtain_wall(*i, s).map(drop),
            CurtainOp::Panel(i, a, b, p) => m.set_curtain_panel(*i, *a, *b, p),
            CurtainOp::Del(i) => m.delete_curtain_wall(*i),
            CurtainOp::Move(i, v) => m.move_elements(&[Target::Curtain(*i)], *v),
        }
    }
}

fn cw_spec(r: &mut Rng, fx: &Fx) -> CurtainSpec {
    let a = pt(r);
    let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
    CurtainSpec {
        level: fx.l1,
        name: format!("CW{}", r.below(100)),
        a,
        b: (a.0 + r.f(3.0, 8.0), a.1 + r.f(-1.0, 1.0)),
        bulge: if r.below(4) == 0 { r.f(-0.3, 0.3) } else { 0.0 },
        base_offset: 0.0,
        height: r.f(2.5, 4.0),
        u: GridLines::Spacing(r.f(0.8, 2.0)),
        v: GridLines::Spacing(r.f(0.8, 2.0)),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    }
}

fn gen_curtain(r: &mut Rng, m: &Model, fx: &Fx) -> Option<CurtainOp> {
    let cws = ec::ids(m, "curtain_wall");
    let kinds = [
        PanelKind::Glazed,
        PanelKind::Solid,
        PanelKind::Door,
        PanelKind::Window,
        PanelKind::Empty,
    ];
    match r.below(7) {
        0 | 1 => Some(CurtainOp::Add(cw_spec(r, fx))),
        2 => target(r, &cws).map(|i| CurtainOp::Set(i, cw_spec(r, fx))),
        3 | 4 => target(r, &cws).map(|i| {
            let p = PanelSpec {
                kind: kinds[r.below(kinds.len())],
                name: "Panel".into(),
                swing_side: r.below(2) == 0,
                swing_hinge: r.below(2) == 0,
            };
            CurtainOp::Panel(i, r.below(4) as u32, r.below(3) as u32, p)
        }),
        5 => target(r, &cws).map(CurtainOp::Del),
        _ => target(r, &cws).map(|i| CurtainOp::Move(i, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))),
    }
}

// ---------------------------------------------------------------- ceilings

#[derive(Clone, Debug)]
enum CeilOp {
    Add(CeilingSpec),
    Edit(i64, CeilingSpec),
    Del(i64),
    Shaft(ShaftSpec),
    DelShaft(i64),
}

impl ModelOp for CeilOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            CeilOp::Add(s) => m.add_ceiling(s).map(drop),
            CeilOp::Edit(i, s) => m.update_ceiling(*i, s),
            CeilOp::Del(i) => m.delete_ceiling(*i),
            CeilOp::Shaft(s) => m.add_shaft(s).map(drop),
            CeilOp::DelShaft(i) => m.delete_shaft(*i),
        }
    }
}

fn ceiling_spec(r: &mut Rng, fx: &Fx) -> CeilingSpec {
    let (x, y) = (r.f(-2.0, 1.0), r.f(-2.0, 1.0));
    CeilingSpec {
        level: fx.l1,
        slab_type: fx.st,
        height: if r.below(10) == 0 {
            f64::NAN
        } else {
            r.f(2.4, 2.9)
        },
        outer: rect(x, y, r.f(3.0, 8.0), r.f(3.0, 8.0)),
        holes: if r.below(3) == 0 {
            vec![rect(x + 1.0, y + 1.0, 0.5, 0.5)]
        } else {
            vec![]
        },
    }
}

fn gen_ceilings(r: &mut Rng, m: &Model, fx: &Fx) -> Option<CeilOp> {
    let (ceils, shafts) = (ec::ids(m, "ceiling"), ec::ids(m, "shaft"));
    match r.below(7) {
        0 | 1 => Some(CeilOp::Add(ceiling_spec(r, fx))),
        2 | 3 => target(r, &ceils).map(|i| CeilOp::Edit(i, ceiling_spec(r, fx))),
        4 => target(r, &ceils).map(CeilOp::Del),
        5 => Some(CeilOp::Shaft(shaft_spec(r, fx))),
        _ => target(r, &shafts).map(CeilOp::DelShaft),
    }
}

// ---------------------------------------------------------------- railings

#[derive(Clone, Debug)]
enum RailOp {
    Stair(StairSpec),
    DelStair(i64),
    Path(RailingSpec),
    StairRails(i64, RailingSpec),
    SetPath(i64, Vec<(f64, f64, f64)>),
    Del(i64),
}

impl ModelOp for RailOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            RailOp::Stair(s) => m.add_stair(s).map(drop),
            RailOp::DelStair(i) => m.delete_stair(*i),
            RailOp::Path(s) => m.add_railing(s).map(drop),
            RailOp::StairRails(st, t) => m.add_stair_railings(*st, t).map(drop),
            RailOp::SetPath(i, p) => m.set_railing_path(*i, p),
            RailOp::Del(i) => m.delete_railing(*i),
        }
    }
}

fn rail_path(r: &mut Rng) -> Vec<(f64, f64, f64)> {
    let (mut x, mut y) = pt(r);
    let n = if r.below(10) == 0 { 1 } else { 2 + r.below(3) };
    (0..n)
        .map(|k| {
            if k > 0 {
                if k % 2 == 1 {
                    x += r.f(1.0, 4.0);
                } else {
                    y += r.f(1.0, 4.0);
                }
            }
            (x, y, 0.0)
        })
        .collect()
}

fn gen_railings(r: &mut Rng, m: &Model, fx: &Fx) -> Option<RailOp> {
    let (stairs, rails) = (ec::ids(m, "stair"), ec::ids(m, "railing"));
    match r.below(8) {
        0 => Some(RailOp::Stair(StairSpec::straight(
            fx.l1,
            fx.l2,
            pt(r),
            r.f(0.0, 6.0),
        ))),
        1 => target(r, &stairs).map(RailOp::DelStair),
        2 | 3 => {
            let mut s = RailingSpec::new(fx.l1, RailingHost::Path(rail_path(r)));
            s.height = r.f(0.8, 1.1);
            Some(RailOp::Path(s))
        }
        4 => target(r, &stairs)
            .map(|st| RailOp::StairRails(st, RailingSpec::new(fx.l1, RailingHost::Path(vec![])))),
        5 | 6 => target(r, &rails).map(|i| RailOp::SetPath(i, rail_path(r))),
        _ => target(r, &rails).map(RailOp::Del),
    }
}

// ---------------------------------------------------------------- components

#[derive(Clone, Debug)]
enum CompOp {
    Add(CompSpec),
    Edit(i64, CompSpec),
    Del(i64),
    Load(String),
    DelFamily(i64),
    DelType(i64),
    Move(i64, P),
    Rotate(i64, P, f64),
    Wall(WallSpec),
}

impl ModelOp for CompOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            CompOp::Add(s) => m.add_component(s).map(drop),
            CompOp::Edit(i, s) => m.update_component(*i, s),
            CompOp::Del(i) => m.delete_component(*i),
            CompOp::Load(n) => m.load_library_family(n).map(drop),
            CompOp::DelFamily(i) => m.delete_family(*i),
            CompOp::DelType(i) => m.delete_comp_type(*i),
            CompOp::Move(i, v) => m.move_elements(&[Target::Component(*i)], *v),
            CompOp::Rotate(i, c, a) => m
                .rotate_elements(&[Target::Component(*i)], *c, *a, false)
                .map(drop),
            CompOp::Wall(s) => m.add_wall(s).map(drop),
        }
    }
}

fn gen_comps(r: &mut Rng, m: &Model, fx: &Fx) -> Option<CompOp> {
    let (comps, walls, types) = (
        ec::ids(m, "comp"),
        ec::ids(m, "wall"),
        ec::ids(m, "comp_type"),
    );
    let fams: Vec<i64> = m
        .families()
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.0)
        .collect();
    let free = |r: &mut Rng| {
        let (x, y) = pt(r);
        let mut s = CompSpec::at(
            fx.l1,
            if r.below(2) == 0 { fx.table } else { fx.chair },
            x,
            y,
        );
        s.rotation = r.f(0.0, TAU);
        s.mirror = r.below(3) == 0;
        s
    };
    match r.below(11) {
        0 | 1 => Some(CompOp::Add(free(r))),
        2 => r.pick(&walls).map(|w| {
            let mut s = CompSpec::at(fx.l1, fx.basin, r.f(0.3, 6.0), 0.1);
            s.host = CompHost::Wall(w);
            CompOp::Add(s)
        }),
        3 => target(r, &comps).map(|i| {
            let mut s = m.component(i).unwrap_or_else(|_| free(r));
            s.at.0 += r.f(-1.0, 1.0);
            s.rotation = r.f(0.0, TAU);
            CompOp::Edit(i, s)
        }),
        4 => target(r, &comps).map(CompOp::Del),
        5 => {
            // an already loaded family returns Ok without an op: not an undo step
            let name = "Generic Box";
            m.family_id(name)
                .ok()
                .flatten()
                .is_none()
                .then(|| CompOp::Load(name.into()))
        }
        6 => target(r, &fams).map(CompOp::DelFamily),
        7 => target(r, &types).map(CompOp::DelType),
        8 => target(r, &comps).map(|i| CompOp::Move(i, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))),
        9 => target(r, &comps).map(|i| CompOp::Rotate(i, pt(r), r.f(-PI, PI))),
        _ => Some(CompOp::Wall(wall_spec(r, fx, fx.l1))),
    }
}

// ---------------------------------------------------------------- masses

#[derive(Clone, Debug)]
enum MassOp {
    Add(MassSpec),
    Edit(i64, MassSpec),
    Move(i64, (f64, f64, f64)),
    Del(i64),
    Combine(i64, i64, BoolOp),
    WallByFace(i64, u32, i64),
    SlabByFace(i64, u32, i64),
    FloorSlabs(i64, i64),
    Paint(i64, u32, Option<String>),
}

impl ModelOp for MassOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            MassOp::Add(s) => m.add_mass(s).map(drop),
            MassOp::Edit(i, s) => m.update_mass(*i, s),
            MassOp::Move(i, d) => m.move_mass(*i, *d),
            MassOp::Del(i) => m.delete_mass(*i),
            MassOp::Combine(a, b, op) => m.combine_masses(*a, *b, *op),
            MassOp::WallByFace(i, t, wt) => m.wall_by_face(*i, *t, *wt).map(drop),
            MassOp::SlabByFace(i, t, st) => m.slab_by_face(*i, *t, *st).map(drop),
            MassOp::FloorSlabs(i, st) => m.floor_slabs(*i, *st).map(drop),
            MassOp::Paint(i, t, mat) => m.paint_face(*i, *t, mat.as_deref()),
        }
    }
}

fn mass_spec(r: &mut Rng, fx: &Fx) -> MassSpec {
    let (x, y) = (r.f(-6.0, 4.0), r.f(-6.0, 4.0));
    let (w, d, h) = (r.f(2.0, 8.0), r.f(2.0, 8.0), r.f(2.0, 9.0));
    MassSpec {
        level: fx.l1,
        name: format!("M{}", r.below(50)),
        void: r.below(6) == 0,
        inplace: r.below(4) == 0,
        form: if r.below(2) == 0 {
            Form::Extrude {
                ring: rect(x, y, w, d),
                z0: 0.0,
                h,
                ids: vec![],
            }
        } else {
            Form::Cuboid {
                lo: (x, y, 0.0),
                hi: (x + w, y + d, h),
            }
        },
    }
}

fn gen_masses(r: &mut Rng, m: &Model, fx: &Fx) -> Option<MassOp> {
    let masses = ec::ids(m, "mass");
    let face = |r: &mut Rng, id: i64, class: FaceClass| -> Option<u32> {
        let tags: Vec<u32> = m
            .mass_faces(id)
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.class == class)
            .map(|f| f.tag)
            .collect();
        r.pick(&tags)
    };
    match r.below(11) {
        0 | 1 => Some(MassOp::Add(mass_spec(r, fx))),
        2 => target(r, &masses).map(|i| MassOp::Edit(i, mass_spec(r, fx))),
        3 => target(r, &masses).map(|i| MassOp::Move(i, (r.f(-1.0, 1.0), r.f(-1.0, 1.0), 0.0))),
        4 => target(r, &masses).map(MassOp::Del),
        5 => pick_n(r, &masses, 2).map(|v| {
            let op = [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect][r.below(3)];
            MassOp::Combine(v[0], v[1], op)
        }),
        6 | 7 => r
            .pick(&masses)
            .and_then(|i| face(r, i, FaceClass::Vertical).map(|t| MassOp::WallByFace(i, t, fx.wt))),
        8 => r
            .pick(&masses)
            .and_then(|i| face(r, i, FaceClass::Up).map(|t| MassOp::SlabByFace(i, t, fx.st))),
        9 => target(r, &masses).map(|i| MassOp::FloorSlabs(i, fx.st)),
        _ => r.pick(&masses).and_then(|i| {
            let class = if r.below(2) == 0 {
                FaceClass::Vertical
            } else {
                FaceClass::Up
            };
            let mat = (r.below(4) != 0).then(|| "Glass".to_string());
            face(r, i, class).map(|t| MassOp::Paint(i, t, mat))
        }),
    }
}

// ---------------------------------------------------------------- MEP runs / systems

#[derive(Clone, Debug)]
enum MepOp {
    System(String, SystemKind),
    DelSystem(i64),
    Run(RunSpec),
    EditRun(i64, RunSpec),
    DelRun(i64),
    Riser(RunSpec, P, (i64, f64), (i64, f64)),
    Move(i64, P),
}

impl ModelOp for MepOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            MepOp::System(n, k) => m.add_mep_system(n, *k).map(drop),
            MepOp::DelSystem(i) => m.delete_mep_system(*i),
            MepOp::Run(s) => m.add_mep_run(s).map(drop),
            MepOp::EditRun(i, s) => m.update_mep_run(*i, s),
            MepOp::DelRun(i) => m.delete_mep_run(*i),
            MepOp::Riser(s, p, a, b) => m.add_mep_riser(s.clone(), p.0, p.1, *a, *b).map(drop),
            MepOp::Move(i, v) => m.move_elements(&[Target::MepRun(*i)], *v),
        }
    }
}

fn gen_mep(r: &mut Rng, m: &Model, fx: &Fx) -> Option<MepOp> {
    let systems = m.mep_systems().unwrap_or_default();
    let runs = ec::ids(m, "mep_run");
    let sys_ids: Vec<i64> = systems.iter().map(|s| s.id).collect();
    let run = |r: &mut Rng| -> Option<RunSpec> {
        let sys = systems.get(r.below(systems.len().max(1)))?;
        let dom = if r.below(6) == 0 {
            MepDomain::Conduit
        } else {
            sys.kind.domains()[0]
        };
        let shape = match dom {
            MepDomain::Duct => MepShape::Rect {
                w: r.f(0.2, 0.6),
                h: r.f(0.2, 0.4),
            },
            _ => MepShape::Round { d: r.f(0.02, 0.15) },
        };
        let (mut x, mut y) = pt(r);
        let z = r.f(2.4, 2.8);
        let n = 2 + r.below(3);
        let pts = (0..n)
            .map(|k| {
                if k > 0 {
                    if k % 2 == 1 {
                        x += r.f(1.0, 5.0);
                    } else {
                        y += r.f(1.0, 5.0);
                    }
                }
                (x, y, z)
            })
            .collect();
        Some(RunSpec::new(
            sys.id,
            fx.l1,
            MepSection::new(dom, shape),
            pts,
        ))
    };
    match r.below(10) {
        0 => {
            let kinds = [
                SystemKind::ColdWater,
                SystemKind::HotWater,
                SystemKind::SupplyAir,
                SystemKind::Sanitary,
            ];
            let name = if r.below(8) == 0 {
                " ".to_string()
            } else {
                format!("Sys{}", r.below(10))
            };
            Some(MepOp::System(name, kinds[r.below(kinds.len())]))
        }
        1 => target(r, &sys_ids).map(MepOp::DelSystem),
        2..=4 => run(r).map(MepOp::Run),
        5 => target(r, &runs).and_then(|i| run(r).map(|s| MepOp::EditRun(i, s))),
        6 => target(r, &runs).map(MepOp::DelRun),
        7 => run(r).map(|s| MepOp::Riser(s, pt(r), (fx.l1, 0.3), (fx.l2, 0.3))),
        _ => target(r, &runs).map(|i| MepOp::Move(i, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))),
    }
}

// ---------------------------------------------------------------- levels

#[derive(Clone, Debug)]
enum LevelOp {
    Add(String, f64),
    Edit(i64, String, f64),
    Del(i64),
    Wall(WallSpec),
    Slab(SlabSpec),
    DelWall(i64),
}

impl ModelOp for LevelOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            LevelOp::Add(n, z) => m.add_level(n, *z).map(drop),
            LevelOp::Edit(i, n, z) => m.update_level(*i, n, *z),
            LevelOp::Del(i) => m.delete_level(*i),
            LevelOp::Wall(s) => m.add_wall(s).map(drop),
            LevelOp::Slab(s) => m.add_slab(s).map(drop),
            LevelOp::DelWall(i) => m.delete_wall(*i),
        }
    }
}

fn gen_levels(r: &mut Rng, m: &Model, fx: &Fx) -> Option<LevelOp> {
    let levels = ec::ids(m, "level");
    // L1 stays: with no level left Editor::reload adds one, a second journal entry per step
    let others: Vec<i64> = levels.iter().copied().filter(|&l| l != fx.l1).collect();
    let walls = ec::ids(m, "wall");
    let names = ["L2", "L3", "L4", "Roof", "Attic"];
    match r.below(8) {
        0 | 1 => Some(LevelOp::Add(
            names[r.below(names.len())].into(),
            r.f(-3.0, 12.0),
        )),
        2 | 3 => target(r, &levels).map(|l| {
            let n = if r.below(8) == 0 {
                " ".to_string()
            } else {
                names[r.below(names.len())].to_string()
            };
            LevelOp::Edit(l, n, r.f(-3.0, 12.0))
        }),
        4 => target(r, &others).map(LevelOp::Del),
        5 => r.pick(&levels).map(|l| LevelOp::Wall(wall_spec(r, fx, l))),
        6 => r.pick(&levels).map(|l| LevelOp::Slab(slab_spec(r, fx, l))),
        _ => target(r, &walls).map(LevelOp::DelWall),
    }
}

// ---------------------------------------------------------------- phases

#[derive(Clone, Debug)]
enum PhaseOp {
    Add(String, Option<i64>),
    Rename(i64, String),
    Move(i64, usize),
    Del(i64, Option<i64>),
    Set(Vec<(&'static str, i64)>, Option<i64>, Option<Option<i64>>),
    Demolish(Vec<(&'static str, i64)>, i64),
    View(ViewKey, ViewPhase),
    Wall(WallSpec),
}

impl ModelOp for PhaseOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            PhaseOp::Add(n, after) => m.add_phase(n, *after).map(drop),
            PhaseOp::Rename(i, n) => m.rename_phase(*i, n),
            PhaseOp::Move(i, to) => m.move_phase(*i, *to),
            PhaseOp::Del(i, into) => m.delete_phase(*i, *into),
            PhaseOp::Set(items, c, d) => m.set_element_phases(items, *c, *d),
            PhaseOp::Demolish(items, p) => m.demolish(items, *p).map(drop),
            PhaseOp::View(k, vp) => m.set_view_phase(*k, *vp),
            PhaseOp::Wall(s) => m.add_wall(s).map(drop),
        }
    }
}

fn gen_phases(r: &mut Rng, m: &Model, fx: &Fx) -> Option<PhaseOp> {
    let ph: Vec<i64> = m
        .phases()
        .unwrap_or_default()
        .iter()
        .map(|p| p.id)
        .collect();
    let mut els: Vec<(&'static str, i64)> = ec::ids(m, "wall")
        .into_iter()
        .map(|i| ("wall", i))
        .collect();
    els.extend(ec::ids(m, "room").into_iter().map(|i| ("room", i)));
    els.extend(ec::ids(m, "opening").into_iter().map(|i| ("opening", i)));
    let names = ["Stage A", "Stage B", "Stage C"];
    // set_element_phases / demolish with no items return Ok without an op
    let items = |r: &mut Rng| -> Option<Vec<(&'static str, i64)>> {
        (0..1 + r.below(2)).map(|_| r.pick(&els)).collect()
    };
    match r.below(10) {
        0 => {
            let after = if r.below(2) == 0 { None } else { r.pick(&ph) };
            Some(PhaseOp::Add(names[r.below(3)].into(), after))
        }
        1 => target(r, &ph).map(|p| PhaseOp::Rename(p, names[r.below(3)].into())),
        2 => target(r, &ph).map(|p| PhaseOp::Move(p, r.below(4))),
        3 => target(r, &ph).map(|p| {
            let into = if r.below(3) == 0 { None } else { r.pick(&ph) };
            PhaseOp::Del(p, into)
        }),
        4 | 5 => items(r).map(|it| {
            let created = if r.below(4) == 0 { None } else { r.pick(&ph) };
            let demolished = match r.below(3) {
                0 => None,
                1 => Some(None),
                _ => Some(r.pick(&ph)),
            };
            PhaseOp::Set(it, created, demolished)
        }),
        6 => items(r).and_then(|it| target(r, &ph).map(|p| PhaseOp::Demolish(it, p))),
        7 => target(r, &ph).map(|p| {
            let filters = [
                PhaseFilter::Off,
                PhaseFilter::Complete,
                PhaseFilter::Existing,
                PhaseFilter::Demolition,
            ];
            let key = if r.below(2) == 0 {
                ViewKey::Plan(fx.l1)
            } else {
                ViewKey::ThreeD
            };
            PhaseOp::View(
                key,
                ViewPhase {
                    phase: p,
                    filter: filters[r.below(filters.len())],
                },
            )
        }),
        _ => Some(PhaseOp::Wall(box_wall(r, fx))),
    }
}

// ---------------------------------------------------------------- project parameters

#[derive(Clone, Debug)]
enum ParamOp {
    Add(ParamSpec),
    Edit(i64, ParamSpec),
    Del(i64),
    Set(Vec<ParamSet>),
    Wall(WallSpec),
    DelWall(i64),
}

impl ModelOp for ParamOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            ParamOp::Add(s) => m.add_param(s).map(drop),
            ParamOp::Edit(i, s) => m.edit_param(*i, s),
            ParamOp::Del(i) => m.delete_param(*i),
            ParamOp::Set(v) => m.set_param_values(v),
            ParamOp::Wall(s) => m.add_wall(s).map(drop),
            ParamOp::DelWall(i) => m.delete_wall(*i),
        }
    }
}

fn gen_params(r: &mut Rng, m: &Model, fx: &Fx) -> Option<ParamOp> {
    let defs = m.params().unwrap_or_default();
    let ids: Vec<i64> = defs.iter().map(|d| d.id).collect();
    let (walls, rooms) = (ec::ids(m, "wall"), ec::ids(m, "room"));
    match r.below(9) {
        0 | 1 => Some(ParamOp::Add(param_spec(r))),
        2 => target(r, &ids).map(|i| ParamOp::Edit(i, param_spec(r))),
        3 => target(r, &ids).map(ParamOp::Del),
        4..=6 => {
            if defs.is_empty() {
                return None;
            }
            // set_param_values with no sets returns Ok without an op
            let sets: Option<Vec<ParamSet>> = (0..1 + r.below(2))
                .map(|_| {
                    let d = &defs[r.below(defs.len())];
                    let (kind, id) = if r.below(2) == 0 {
                        ("wall", r.pick(&walls)?)
                    } else {
                        ("room", r.pick(&rooms)?)
                    };
                    Some(ParamSet {
                        param: d.id,
                        kind: kind.into(),
                        id,
                        value: pval(r, d.spec.ptype),
                    })
                })
                .collect();
            sets.map(ParamOp::Set)
        }
        7 => Some(ParamOp::Wall(box_wall(r, fx))),
        _ => target(r, &walls).map(ParamOp::DelWall),
    }
}

// ---------------------------------------------------------------- sheets / viewports / revisions

#[derive(Clone, Debug)]
enum SheetOp {
    Add(SheetDef),
    Edit(i64, SheetDef),
    Del(i64),
    Move(i64, i64),
    Renumber(Vec<i64>, String),
    Revs(i64, Vec<Revision>),
    Vp(ViewportDef),
    EditVp(i64, ViewportDef),
    MoveVps(Vec<(i64, P)>),
    DelVp(i64),
    Set(String, Vec<i64>),
    DelSet(i64),
}

impl ModelOp for SheetOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            SheetOp::Add(s) => m.add_sheet(s).map(drop),
            SheetOp::Edit(i, s) => m.update_sheet(*i, s),
            SheetOp::Del(i) => m.delete_sheet(*i),
            SheetOp::Move(i, d) => m.move_sheet(*i, *d),
            SheetOp::Renumber(ids, first) => m.renumber_sheets(ids, first),
            SheetOp::Revs(i, v) => m.set_revisions(*i, v),
            SheetOp::Vp(v) => m.add_viewport(v).map(drop),
            SheetOp::EditVp(i, v) => m.update_viewport(*i, v),
            SheetOp::MoveVps(v) => m.move_viewports(v),
            SheetOp::DelVp(i) => m.delete_viewport(*i),
            SheetOp::Set(n, ids) => m.add_sheet_set(n, ids).map(drop),
            SheetOp::DelSet(i) => m.delete_sheet_set(*i),
        }
    }
}

fn sheet_def(r: &mut Rng) -> SheetDef {
    let mut s = SheetDef::new(
        &format!("A-10{}", r.below(6)),
        ["Plans", "Sections", "Details"][r.below(3)],
    );
    s.paper = ["A3", "A1", "B9"][r.below(3)].into();
    s.landscape = r.below(2) == 0;
    s
}

fn viewport(r: &mut Rng, fx: &Fx, sheet: i64) -> ViewportDef {
    ViewportDef {
        sheet,
        view: ViewRef::Plan {
            level: if r.below(2) == 0 { fx.l1 } else { fx.l2 },
            cut: 1.2,
        },
        scale: [50.0, 100.0, 200.0, -1.0][r.below(4)],
        center: (r.f(50.0, 350.0), r.f(50.0, 250.0)),
        crop: None,
        title: format!("View {}", r.below(10)),
    }
}

fn gen_sheets(r: &mut Rng, m: &Model, fx: &Fx) -> Option<SheetOp> {
    let (sheets, vps, sets) = (
        ec::ids(m, "sheet"),
        ec::ids(m, "viewport"),
        ec::ids(m, "sheet_set"),
    );
    match r.below(13) {
        0 | 1 => Some(SheetOp::Add(sheet_def(r))),
        2 => target(r, &sheets).map(|i| SheetOp::Edit(i, sheet_def(r))),
        3 => target(r, &sheets).map(SheetOp::Del),
        4 => target(r, &sheets).map(|i| SheetOp::Move(i, r.below(5) as i64 - 2)),
        5 => {
            let ids: Vec<i64> = sheets.iter().copied().filter(|_| r.below(2) == 0).collect();
            (!ids.is_empty()).then(|| SheetOp::Renumber(ids, format!("B-{}", 200 + r.below(5))))
        }
        6 => target(r, &sheets).map(|i| {
            let revs = (0..r.below(3))
                .map(|k| Revision {
                    mark: format!("{}", k + 1),
                    date: "2026-11-04".into(),
                    description: "Issued".into(),
                    author: "e2e".into(),
                })
                .collect();
            SheetOp::Revs(i, revs)
        }),
        7 | 8 => target(r, &sheets).map(|s| SheetOp::Vp(viewport(r, fx, s))),
        9 => target(r, &vps).and_then(|v| {
            r.pick(&sheets)
                .map(|s| SheetOp::EditVp(v, viewport(r, fx, s)))
        }),
        10 => target(r, &vps)
            .map(|v| SheetOp::MoveVps(vec![(v, (r.f(50.0, 350.0), r.f(50.0, 250.0)))])),
        11 => target(r, &vps).map(SheetOp::DelVp),
        _ => {
            if r.below(2) == 0 {
                let ids: Vec<i64> = sheets.iter().copied().filter(|_| r.below(2) == 0).collect();
                Some(SheetOp::Set(["Issue", "Review"][r.below(2)].into(), ids))
            } else {
                target(r, &sets).map(SheetOp::DelSet)
            }
        }
    }
}

// ---------------------------------------------------------------- constraints

#[derive(Clone, Debug)]
enum ConsOp {
    Add(ConstraintSpec),
    Del(i64),
    Axis(i64, P, P),
    MoveWall(i64, P),
    Grid(GridSpec),
    Level(i64, String, f64),
    Wall(WallSpec),
}

impl ModelOp for ConsOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            ConsOp::Add(s) => m.add_constraint(s).map(drop),
            ConsOp::Del(i) => m.delete_constraint(*i),
            ConsOp::Axis(w, a, b) => m.set_wall_axis(*w, *a, *b),
            ConsOp::MoveWall(w, v) => m.move_elements(&[Target::Wall(*w)], *v),
            ConsOp::Grid(s) => m.add_grid(s).map(drop),
            ConsOp::Level(i, n, z) => m.update_level(*i, n, *z),
            ConsOp::Wall(s) => m.add_wall(s).map(drop),
        }
    }
}

fn gen_constraints(r: &mut Rng, m: &Model, fx: &Fx) -> Option<ConsOp> {
    use ConstraintKind as K;
    let (walls, grids, rules) = (
        ec::ids(m, "wall"),
        ec::ids(m, "grid"),
        ec::ids(m, "constraint_rule"),
    );
    match r.below(10) {
        0..=2 => {
            let kinds = [
                K::LockLength,
                K::LockDist,
                K::Equal,
                K::Align,
                K::Parallel,
                K::Perp,
                K::AlignGrid,
                K::LockDistGrid,
                K::LevelGap,
                K::TopToLevel,
            ];
            let kind = kinds[r.below(kinds.len())];
            let refs = match kind {
                K::LockLength => pick_n(r, &walls, 1),
                K::LockDist | K::Align | K::Parallel | K::Perp => pick_n(r, &walls, 2),
                K::Equal => pick_n(r, &walls, 3),
                K::AlignGrid | K::LockDistGrid => r
                    .pick(&walls)
                    .and_then(|w| r.pick(&grids).map(|g| vec![w, g])),
                K::LevelGap => Some(vec![fx.l1, fx.l2]),
                K::TopToLevel => r.pick(&walls).map(|w| vec![w, fx.l2]),
            };
            let value = (r.below(3) == 0).then(|| r.f(1.0, 6.0));
            refs.map(|walls| ConsOp::Add(ConstraintSpec { kind, walls, value }))
        }
        3 => target(r, &rules).map(ConsOp::Del),
        4 => target(r, &walls).and_then(|w| {
            let (a, b) = axis(m, w).unwrap_or(((0.0, 0.0), (MISSING as f64, 0.0)));
            let v = (r.f(-1.0, 1.0), r.f(-1.0, 1.0));
            let stretch = r.f(-1.0, 1.0);
            Some(ConsOp::Axis(
                w,
                (a.0 + v.0, a.1 + v.1),
                (b.0 + v.0 + stretch, b.1 + v.1),
            ))
        }),
        5 => target(r, &walls).map(|w| ConsOp::MoveWall(w, (r.f(-1.0, 1.0), r.f(-1.0, 1.0)))),
        6 => Some(ConsOp::Grid(grid_spec(r))),
        7 => Some(ConsOp::Level(fx.l2, "L2".into(), r.f(2.5, 4.5))),
        _ => Some(ConsOp::Wall(ortho_wall(r, fx))),
    }
}

// ---------------------------------------------------------------- modify (copy/rotate/mirror/array/offset/trim/split/align)

#[derive(Clone, Debug)]
enum ModOp {
    Move(Vec<Target>, P),
    Copy(Vec<Target>, Vec<P>),
    Rotate(Vec<Target>, P, f64, bool),
    Mirror(Vec<Target>, P, P, bool),
    ArrayLin(Vec<Target>, usize, Spacing),
    ArrayRad(Vec<Target>, usize, P, f64),
    OffsetWall(i64, f64, bool),
    OffsetSlab(i64, f64),
    TrimCorner(i64, i64),
    TrimTo(i64, Reference),
    Split(i64, P),
    Align(AlignEdge, P, P),
    Wall(WallSpec),
    Slab(SlabSpec),
    DelWall(i64),
}

impl ModelOp for ModOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            ModOp::Move(t, v) => m.move_elements(t, *v),
            ModOp::Copy(t, vs) => m.copy_elements(t, vs).map(drop),
            ModOp::Rotate(t, c, a, cp) => m.rotate_elements(t, *c, *a, *cp).map(drop),
            ModOp::Mirror(t, a, b, cp) => m.mirror_elements(t, *a, *b, *cp).map(drop),
            ModOp::ArrayLin(t, n, s) => m.array_linear(t, *n, *s).map(drop),
            ModOp::ArrayRad(t, n, c, tot) => m.array_radial(t, *n, *c, *tot).map(drop),
            ModOp::OffsetWall(w, d, l) => m.offset_wall(*w, *d, *l).map(drop),
            ModOp::OffsetSlab(s, d) => m.offset_slab(*s, *d),
            ModOp::TrimCorner(a, b) => m.trim_corner(*a, *b),
            ModOp::TrimTo(w, rf) => m.trim_to(*w, *rf),
            ModOp::Split(w, p) => m.split_wall_at(*w, *p).map(drop),
            ModOp::Align(e, a, b) => m.align(*e, *a, *b),
            ModOp::Wall(s) => m.add_wall(s).map(drop),
            ModOp::Slab(s) => m.add_slab(s).map(drop),
            ModOp::DelWall(i) => m.delete_wall(*i),
        }
    }
}

fn gen_modify(r: &mut Rng, m: &Model, fx: &Fx) -> Option<ModOp> {
    let walls = ec::ids(m, "wall");
    let slabs = ec::ids(m, "slab");
    let mut pool: Vec<Target> = walls.iter().map(|&i| Target::Wall(i)).collect();
    pool.extend(slabs.iter().map(|&i| Target::Slab(i)));
    pool.extend(ec::ids(m, "room").into_iter().map(Target::Room));
    pool.extend(ec::ids(m, "room_sep").into_iter().map(Target::RoomSep));
    if r.below(6) == 0 {
        pool.extend(ec::ids(m, "opening").into_iter().map(Target::Opening));
    }
    let ts = |r: &mut Rng| -> Option<Vec<Target>> {
        (0..1 + r.below(2)).map(|_| r.pick(&pool)).collect()
    };
    let v = |r: &mut Rng| (r.f(-6.0, 6.0), r.f(-6.0, 6.0));
    match r.below(16) {
        0 => ts(r).map(|t| ModOp::Move(t, v(r))),
        1 => ts(r).map(|t| {
            let vs = (0..1 + r.below(2)).map(|_| v(r)).collect();
            ModOp::Copy(t, vs)
        }),
        2 => ts(r).map(|t| ModOp::Rotate(t, pt(r), r.f(-PI, PI), r.below(2) == 0)),
        3 => ts(r).map(|t| ModOp::Mirror(t, pt(r), pt(r), r.below(2) == 0)),
        4 => ts(r).map(|t| {
            let s = if r.below(2) == 0 {
                Spacing::Step(v(r))
            } else {
                Spacing::Fit(v(r))
            };
            ModOp::ArrayLin(t, 1 + r.below(3), s)
        }),
        5 => ts(r)
            .map(|t| ModOp::ArrayRad(t, 2 + r.below(2), pt(r), [PI / 2.0, PI, TAU][r.below(3)])),
        6 => target(r, &walls).map(|w| {
            let d = if r.below(8) == 0 { -1.0 } else { r.f(0.3, 2.0) };
            ModOp::OffsetWall(w, d, r.below(2) == 0)
        }),
        7 => target(r, &slabs).map(|s| ModOp::OffsetSlab(s, r.f(-0.5, 0.8))),
        8 => pick_n(r, &walls, 2).map(|w| ModOp::TrimCorner(w[0], w[1])),
        9 => target(r, &walls).map(|w| {
            let rf = match r.pick(&walls) {
                Some(o) if r.below(2) == 0 => Reference::Wall(o),
                _ => Reference::Line(pt(r), pt(r)),
            };
            ModOp::TrimTo(w, rf)
        }),
        10 => target(r, &walls).map(|w| {
            let (a, b) = axis(m, w).unwrap_or(((0.0, 0.0), (1.0, 0.0)));
            let t = r.f(0.2, 0.8);
            ModOp::Split(w, (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t))
        }),
        11 => r.pick(&walls).and_then(|w| {
            let (a, b) = axis(m, w)?;
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt().max(1e-9);
            let d = r.f(-1.0, 1.0);
            let n = (-dy / len * d, dx / len * d);
            Some(ModOp::Align(
                AlignEdge::WallAxis(w),
                (a.0 + n.0, a.1 + n.1),
                (b.0 + n.0, b.1 + n.1),
            ))
        }),
        12 | 13 => Some(ModOp::Wall(ortho_wall(r, fx))),
        14 => Some(ModOp::Slab(slab_spec(r, fx, fx.l1))),
        _ => target(r, &walls).map(ModOp::DelWall),
    }
}

// ---------------------------------------------------------------- bulk Edit (apply_edits via Editor::apply_xlsx)

#[derive(Clone, Debug)]
struct Bulk(Vec<Edit>);

fn apply_bulk(ed: &mut Editor, b: &Bulk) -> Result<(), String> {
    ed.apply_xlsx(b.0.clone(), "e2e")
}

fn gen_bulk(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Bulk> {
    let (walls, ops, rooms, cols, slabs) = (
        ec::ids(m, "wall"),
        ec::ids(m, "opening"),
        ec::ids(m, "room"),
        ec::ids(m, "col"),
        ec::ids(m, "slab"),
    );
    let mut edits = vec![];
    for _ in 0..1 + r.below(3) {
        let e = match r.below(15) {
            0 | 1 => Some(Edit::NewWall {
                spec: wall_spec(r, fx, fx.l1),
                bulge: if r.below(4) == 0 { 0.2 } else { 0.0 },
            }),
            2 => target(r, &walls).map(|id| Edit::Wall {
                id,
                spec: wall_spec(r, fx, fx.l1),
                bulge: 0.0,
            }),
            3 => target(r, &walls).map(|id| Edit::Delete { table: "wall", id }),
            4 => target(r, &walls).map(|wall| Edit::NewOpening {
                wall,
                spec: opening(r),
                swing: swing(r),
            }),
            5 => target(r, &ops).map(|id| Edit::Delete { table: "opening", id }),
            6 => Some(Edit::NewRoom(room_spec(r, fx))),
            7 => target(r, &rooms).map(Edit::DeleteRoom),
            8 => Some(Edit::NewColumn(column(r, fx))),
            9 => target(r, &cols).map(Edit::DeleteColumn),
            10 => Some(Edit::NewLevel {
                name: ["L3", "L4", "L2"][r.below(3)].into(),
                elevation: r.f(3.0, 12.0),
            }),
            11 => Some(Edit::Level {
                id: fx.l2,
                name: "L2".into(),
                elevation: r.f(2.8, 4.0),
            }),
            12 => Some(Edit::NewSlab(slab_spec(r, fx, fx.l1))),
            13 => target(r, &slabs).map(|id| Edit::Delete { table: "slab", id }),
            _ => {
                if r.below(2) == 0 {
                    Some(Edit::NewParam(param_spec(r)))
                } else {
                    r.pick(&walls).map(|id| Edit::ParamValue {
                        param: ["Cost", "Code"][r.below(2)].into(),
                        kind: "wall".into(),
                        id,
                        value: Some(PVal::Text(format!("v{}", r.below(10)))),
                    })
                }
            }
        };
        edits.extend(e);
    }
    // apply_xlsx with no edits returns Ok without an undo step
    (!edits.is_empty()).then_some(Bulk(edits))
}

// ---------------------------------------------------------------- sketches / slab shape / wall profile / attach

#[derive(Clone, Debug)]
enum SketchOp {
    Finish(SketchHost, Sketch),
    Shape(i64, Option<SlabShape>),
    Attach(i64, WallSide, i64),
    Detach(i64, WallSide),
    Profile(i64, Option<ElevationProfile>),
    Shaft(ShaftSpec),
    Slab(SlabSpec),
}

impl ModelOp for SketchOp {
    fn apply(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            SketchOp::Finish(h, sk) => m.finish_sketch(*h, sk),
            SketchOp::Shape(s, sh) => m.set_slab_shape(*s, sh.as_ref()),
            SketchOp::Attach(w, side, s) => m.attach_wall_to_slab(*w, *side, *s),
            SketchOp::Detach(w, side) => m.detach_wall_from_slab(*w, *side),
            SketchOp::Profile(w, p) => m.set_wall_profile(*w, p.as_ref()),
            SketchOp::Shaft(s) => m.add_shaft(s).map(drop),
            SketchOp::Slab(s) => m.add_slab(s).map(drop),
        }
    }
}

fn gen_sketches(r: &mut Rng, m: &Model, fx: &Fx) -> Option<SketchOp> {
    let (walls, slabs, shafts) = (ec::ids(m, "wall"), ec::ids(m, "slab"), ec::ids(m, "shaft"));
    let side = |r: &mut Rng| {
        if r.below(2) == 0 {
            WallSide::Top
        } else {
            WallSide::Base
        }
    };
    match r.below(11) {
        0 | 1 => target(r, &slabs).map(|s| {
            let sk = match r.below(4) {
                0 => circle_sketch((4.0, 3.0), r.f(1.0, 3.0)),
                1 => {
                    let mut k = rect_sketch(-0.5, -0.5, 9.0, 7.0);
                    k.curves.pop(); // open loop: rejected
                    k
                }
                _ => rect_sketch(r.f(-1.0, 0.5), r.f(-1.0, 0.5), r.f(6.0, 9.0), r.f(5.0, 7.0)),
            };
            SketchOp::Finish(SketchHost::Slab(s), sk)
        }),
        2 => target(r, &shafts).map(|s| {
            SketchOp::Finish(
                SketchHost::Shaft(s),
                circle_sketch((r.f(1.0, 3.0), r.f(1.0, 3.0)), r.f(0.4, 1.0)),
            )
        }),
        3 | 4 => target(r, &slabs).map(|s| {
            let shape = (r.below(3) != 0).then(|| SlabShape {
                points: vec![(v2(r.f(1.0, 7.0), r.f(1.0, 5.0)), r.f(-0.08, 0.0))],
                ..Default::default()
            });
            SketchOp::Shape(s, shape)
        }),
        5 | 6 => {
            target(r, &walls).and_then(|w| r.pick(&slabs).map(|s| SketchOp::Attach(w, side(r), s)))
        }
        7 => target(r, &walls).map(|w| SketchOp::Detach(w, side(r))),
        8 => target(r, &walls).map(|w| {
            let p = axis(m, w).map(|(a, b)| {
                let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
                ElevationProfile {
                    outer: vec![
                        (0.0, 0.0),
                        (len, 0.0),
                        (len, 3.0),
                        (len * 0.5, 3.0 + r.f(0.0, 1.0)),
                        (0.0, 3.0),
                    ],
                    holes: vec![],
                }
            });
            SketchOp::Profile(w, if r.below(4) == 0 { None } else { p })
        }),
        9 => Some(SketchOp::Shaft(shaft_spec(r, fx))),
        _ => Some(SketchOp::Slab(slab_spec(r, fx, fx.l1))),
    }
}

// ---------------------------------------------------------------- mixed families

macro_rules! mixed {
    ($($v:ident($t:ty, $gen:path, $apply:path)),* $(,)?) => {
        #[derive(Clone, Debug)]
        enum Mixed {
            $($v($t)),*
        }

        const MIXED_N: usize = [$(stringify!($v)),*].len();

        #[allow(unused_assignments)]
        fn gen_mixed(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Mixed> {
            let mut k = r.below(MIXED_N);
            $(
                if k == 0 {
                    return $gen(r, m, fx).map(Mixed::$v);
                }
                k -= 1;
            )*
            None
        }

        fn apply_mixed(ed: &mut Editor, op: &Mixed) -> Result<(), String> {
            match op {
                $(Mixed::$v(o) => $apply(ed, o)),*
            }
        }
    };
}

mixed! {
    Walls(WallOp, gen_walls, via_op),
    Struct(StructOp, gen_struct, via_op),
    Floors(FloorOp, gen_floors, via_op),
    Datum(DatumOp, gen_datum, via_op),
    Rooms(RoomOp, gen_rooms, via_op),
    Tags(TagOp, gen_tags, via_op),
    Curtain(CurtainOp, gen_curtain, via_op),
    Ceilings(CeilOp, gen_ceilings, via_op),
    Railings(RailOp, gen_railings, via_op),
    Comps(CompOp, gen_comps, via_op),
    Masses(MassOp, gen_masses, via_op),
    Mep(MepOp, gen_mep, via_op),
    Levels(LevelOp, gen_levels, via_op),
    Phases(PhaseOp, gen_phases, via_op),
    Params(ParamOp, gen_params, via_op),
    Sheets(SheetOp, gen_sheets, via_op),
    Constraints(ConsOp, gen_constraints, via_op),
    Modify(ModOp, gen_modify, via_op),
    Bulk(Bulk, gen_bulk, apply_bulk),
    Sketches(SketchOp, gen_sketches, via_op),
}

// ---------------------------------------------------------------- tests

#[test]
fn e2e_rand_walls() {
    run_family("walls", prep_none, gen_walls, via_op::<WallOp>);
}

#[test]
fn e2e_rand_structure() {
    run_family("structure", prep_none, gen_struct, via_op::<StructOp>);
}

#[test]
fn e2e_rand_slabs_roofs_shafts() {
    run_family(
        "slabs_roofs_shafts",
        prep_none,
        gen_floors,
        via_op::<FloorOp>,
    );
}

#[test]
fn e2e_rand_grids_stairs_dims() {
    run_family("grids_stairs_dims", prep_none, gen_datum, via_op::<DatumOp>);
}

#[test]
fn e2e_rand_rooms() {
    run_family("rooms", prep_box, gen_rooms, via_op::<RoomOp>);
}

#[test]
fn e2e_rand_tags() {
    run_family("tags", prep_box, gen_tags, via_op::<TagOp>);
}

#[test]
fn e2e_rand_curtain() {
    run_family("curtain", prep_none, gen_curtain, via_op::<CurtainOp>);
}

#[test]
fn e2e_rand_ceilings() {
    run_family("ceilings", prep_box, gen_ceilings, via_op::<CeilOp>);
}

#[test]
fn e2e_rand_railings() {
    run_family("railings", prep_stair, gen_railings, via_op::<RailOp>);
}

#[test]
fn e2e_rand_components() {
    run_family("components", prep_box, gen_comps, via_op::<CompOp>);
}

#[test]
fn e2e_rand_masses() {
    run_family("masses", prep_mass, gen_masses, via_op::<MassOp>);
}

#[test]
fn e2e_rand_mep() {
    run_family("mep", prep_none, gen_mep, via_op::<MepOp>);
}

#[test]
fn e2e_rand_levels() {
    run_family("levels", prep_none, gen_levels, via_op::<LevelOp>);
}

#[test]
fn e2e_rand_phases() {
    run_family("phases", prep_box, gen_phases, via_op::<PhaseOp>);
}

#[test]
fn e2e_rand_params() {
    run_family("params", prep_box, gen_params, via_op::<ParamOp>);
}

#[test]
fn e2e_rand_sheets() {
    run_family("sheets", prep_sheet, gen_sheets, via_op::<SheetOp>);
}

#[test]
fn e2e_rand_constraints() {
    run_family("constraints", prep_box, gen_constraints, via_op::<ConsOp>);
}

#[test]
fn e2e_rand_modify() {
    run_family("modify", prep_slabs, gen_modify, via_op::<ModOp>);
}

#[test]
fn e2e_rand_bulk() {
    run_family("bulk", prep_box, gen_bulk, apply_bulk);
}

#[test]
fn e2e_rand_sketches() {
    run_family("sketches", prep_slabs, gen_sketches, via_op::<SketchOp>);
}

#[test]
fn e2e_rand_mixed() {
    run_family("mixed", prep_box, gen_mixed, apply_mixed);
}
