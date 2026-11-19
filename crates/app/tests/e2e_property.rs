// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Seeded property tests over whole-editor action sequences: create, edit property, move, copy,
//! rotate, mirror, array, delete, undo, redo, level change, type change, save and reopen,
//! XLSX export-edit-import and IFC export, on a model that holds every element kind.
//!
//! Invariants checked after every step:
//! - an accepted op writes one journal entry and truncates redo; a rejected op changes nothing;
//! - undo then redo is the identity; the undo/redo availability matches the accepted-op history;
//! - undo of everything returns the starting model exactly, redo of everything the final one;
//! - save then reopen is the identity (data, journal length, integrity);
//! - `check_integrity` is clean, `PRAGMA foreign_key_check` is empty, no orphan rows;
//! - quantities agree with geometry (slab and room areas never exceed their rings);
//! - an unchanged XLSX book gives zero edits; IFC export is valid STEP and deterministic;
//! - no panic.
//!
//! A failing sequence is shrunk to a minimal one and printed with its seed and a replay string.
//! Actions carry only a kind and a seed: targets are resolved against the live model, so any
//! subsequence of a sequence is a valid sequence and shrinking by removal is sound.
//!
//! Env: `DDC_PROP_SEEDS` (fast mode seed count, default 4), `DDC_PROP_STEPS` (steps per run,
//! default 200 fast / 2000 night), `DDC_PROP_SEED` (night: first seed), `DDC_PROP_SECS` (night
//! time budget, default 60), `DDC_PROP_REPLAY` ("Move#12,Create#7": run exactly these actions).

mod e2e_common;

use std::f64::consts::{PI, TAU};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ddc_app::editor::{Editor, OpFn};
use ddc_io::ifc::{check_step, export_ifc};
use ddc_io::xlsx::{self, ImportOptions, Report, TableCell as C};
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, CompSpec, CurtainSpec, Error, Form, GridLines,
    GridSpec, LayerSpec, MassSpec, MepDomain, MepSection, MepShape, Model, MullionSection,
    OpeningSpec, PVal, ParamSet, ParamSpec, ParamType, RailingHost, RailingSpec, RoofKind,
    RoofSpec, RoomSpec, RunSpec, ShaftSpec, SlabSpec, Spacing, StairSpec, SystemKind, Target,
    WallSpec,
};
use e2e_common::{self as ec, Rng, TableRows};

type P = (f64, f64);
type Job = Arc<dyn Fn(&mut Model) -> Result<(), Error> + Send + Sync>;

/// Id that never exists: feeds the rejection path.
const MISSING: i64 = 9_999;
/// Outside the journal (clash statuses): not part of the undo contract.
const SKIP: &[&str] = &["clash_item"];

fn snap(m: &Model) -> Vec<TableRows> {
    ec::dump_except(m, SKIP)
}

fn job<F>(f: F) -> Job
where
    F: Fn(&mut Model) -> Result<(), Error> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// Opening a file adds a registry row for every material text in use (additive by design);
/// those rows are not a change of the saved data. Changed or missing rows still show.
fn without_added_materials(now: Vec<TableRows>, want: &[TableRows]) -> Vec<TableRows> {
    let known: Vec<&String> = want
        .iter()
        .filter(|t| t.0 == "material")
        .flat_map(|t| t.1.iter())
        .collect();
    now.into_iter()
        .map(|(t, rows)| {
            if t == "material" {
                (t, rows.into_iter().filter(|r| known.contains(&r)).collect())
            } else {
                (t, rows)
            }
        })
        .collect()
}

fn same(now: &[TableRows], want: &[TableRows], ctx: &str) -> Result<(), String> {
    if ec::hash_of(now) != ec::hash_of(want) {
        return Err(format!("{ctx}: model differs{}", ec::diff(now, want)));
    }
    Ok(())
}

// ---------------------------------------------------------------- actions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Create,
    EditProp,
    Move,
    Copy,
    Rotate,
    Mirror,
    Array,
    Delete,
    Undo,
    Redo,
    LevelChange,
    TypeChange,
    SaveReopen,
    Xlsx,
    Ifc,
}

/// Kinds with their weight in a generated sequence.
const MIX: &[(Kind, u64)] = &[
    (Kind::Create, 24),
    (Kind::EditProp, 10),
    (Kind::Move, 8),
    (Kind::Copy, 6),
    (Kind::Rotate, 6),
    (Kind::Mirror, 4),
    (Kind::Array, 4),
    (Kind::Delete, 8),
    (Kind::Undo, 7),
    (Kind::Redo, 4),
    (Kind::LevelChange, 4),
    (Kind::TypeChange, 4),
    (Kind::SaveReopen, 1),
    (Kind::Xlsx, 2),
    (Kind::Ifc, 2),
];

#[derive(Clone, Copy, Debug)]
struct Act {
    kind: Kind,
    seed: u64,
}

impl std::fmt::Display for Act {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}#{}", self.kind, self.seed)
    }
}

fn replay_string(acts: &[Act]) -> String {
    acts.iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_replay(s: &str) -> Vec<Act> {
    s.split(',')
        .filter(|t| !t.trim().is_empty())
        .map(|t| {
            let (k, n) = t
                .trim()
                .split_once('#')
                .unwrap_or_else(|| panic!("replay item {t:?} is not Kind#seed"));
            let kind = MIX
                .iter()
                .map(|m| m.0)
                .find(|m| format!("{m:?}") == k)
                .unwrap_or_else(|| panic!("unknown action kind {k:?}"));
            Act {
                kind,
                seed: n.parse().unwrap_or_else(|_| panic!("bad seed {n:?}")),
            }
        })
        .collect()
}

fn gen_seq(seed: u64, steps: usize) -> Vec<Act> {
    let mut r = Rng::new(seed);
    let total: u64 = MIX.iter().map(|m| m.1).sum();
    (0..steps)
        .map(|_| {
            let mut x = r.next() % total;
            let mut kind = MIX[0].0;
            for (k, w) in MIX {
                if x < *w {
                    kind = *k;
                    break;
                }
                x -= *w;
            }
            Act {
                kind,
                seed: r.next() % 1_000_000_007,
            }
        })
        .collect()
}

// ---------------------------------------------------------------- fixture

#[derive(Clone, Copy)]
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
}

/// Every element kind, written straight into the model (not on the Editor undo stack).
fn fixture(ed: &mut Editor) -> Result<Fx, String> {
    let l1 = ed.level;
    let wt = ed.types[1].id;
    let wt2 = ed.types[0].id;
    let m = &mut ed.model;
    let e = |what: &str, e: Error| format!("fixture {what}: {e}");
    let l2 = m.add_level("L2", 3.0).map_err(|x| e("level", x))?;
    let lay = LayerSpec {
        material: "concrete".into(),
        thickness: 0.2,
    };
    let st = m
        .add_slab_type("Slab 200", &[lay])
        .map_err(|x| e("slab type", x))?;
    let ct = m
        .add_catalog_member_type("R 400x400")
        .map_err(|x| e("column type", x))?;
    let bt = m
        .add_catalog_member_type("IPE 300")
        .map_err(|x| e("beam type", x))?;
    let mut ctype = |fam: &str, ty: &str| -> Result<i64, String> {
        let f = m.load_library_family(fam).map_err(|x| e("family", x))?;
        m.comp_type_id(f, ty)
            .map_err(|x| e("comp type", x))?
            .ok_or_else(|| format!("fixture: no type {ty}"))
    };
    let table = ctype("Table", "1600x800")?;
    let chair = ctype("Chair", "450x500")?;
    m.add_mep_system("Water", SystemKind::ColdWater)
        .map_err(|x| e("system", x))?;
    m.add_mep_system("Air", SystemKind::SupplyAir)
        .map_err(|x| e("system", x))?;
    let fx = Fx {
        l1,
        l2,
        wt,
        wt2,
        st,
        ct,
        bt,
        table,
        chair,
    };

    // required kinds: a failure here is a broken fixture
    let c = [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)];
    let ws: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    let w = m.add_walls(&ws).map_err(|x| e("walls", x))?;
    let door = OpeningSpec {
        along: 2.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    m.add_opening(w[0], &door).map_err(|x| e("opening", x))?;
    let mut r = Rng::new(7);
    m.add_column(&column(&mut r, &fx, l1))
        .map_err(|x| e("column", x))?;
    m.add_beam(&beam(&mut r, &fx, l2))
        .map_err(|x| e("beam", x))?;
    m.add_slab(&slab_spec(&mut r, &fx, l1))
        .map_err(|x| e("slab", x))?;
    m.add_slab(&slab_spec(&mut r, &fx, l2))
        .map_err(|x| e("slab", x))?;
    m.add_room(&RoomSpec {
        level: l1,
        name: "Room".into(),
        number: "1".into(),
        point: (4.0, 3.0),
        height: 2.7,
    })
    .map_err(|x| e("room", x))?;
    m.add_component(&CompSpec::at(l1, table, 3.0, 2.0))
        .map_err(|x| e("component", x))?;

    // optional kinds: missing ones are reported, not fatal
    let mut missing = vec![];
    let mut note = |what: &str, r: Result<(), Error>| {
        if let Err(x) = r {
            missing.push(format!("{what}: {x}"));
        }
    };
    note("roof", m.add_roof(&roof_spec(&mut r, &fx)).map(drop));
    note("shaft", m.add_shaft(&shaft_spec(&mut r, &fx)).map(drop));
    note(
        "ceiling",
        m.add_ceiling(&ceiling_spec(&mut r, &fx, l1)).map(drop),
    );
    note(
        "stair",
        m.add_stair(&StairSpec::straight(l1, l2, (10.0, 0.0), 3.0))
            .map(drop),
    );
    note("railing", m.add_railing(&railing_spec(&mut r, &fx)).map(drop));
    note("mass", m.add_mass(&mass_spec(&mut r, &fx)).map(drop));
    note("curtain", m.add_curtain_wall(&cw_spec(&mut r, &fx)).map(drop));
    note("grid", m.add_grid(&grid_spec(&mut r)).map(drop));
    if let Some(run) = mep_run(&mut r, m, &fx) {
        note("mep run", m.add_mep_run(&run).map(drop));
    }
    if !missing.is_empty() {
        eprintln!("property fixture: optional kinds not created: {missing:?}");
    }
    ed.reload().map_err(|x| e("reload", x))?;
    Ok(fx)
}

// ---------------------------------------------------------------- spec builders

fn pt(r: &mut Rng) -> P {
    (r.f(-12.0, 12.0), r.f(-12.0, 12.0))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<P> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn level_of(r: &mut Rng, fx: &Fx) -> i64 {
    if r.below(2) == 0 {
        fx.l1
    } else {
        fx.l2
    }
}

/// An existing id of `table`, or now and then a missing one (rejection path).
fn pid(r: &mut Rng, m: &Model, table: &str) -> i64 {
    let v = ec::ids(m, table);
    if r.below(16) == 0 {
        MISSING
    } else {
        r.pick(&v).unwrap_or(MISSING)
    }
}

fn wall_spec(r: &mut Rng, fx: &Fx, level: i64) -> WallSpec {
    let a = pt(r);
    let (len, ang) = (r.f(2.0, 8.0), r.f(0.0, TAU));
    WallSpec {
        a,
        b: (a.0 + len * ang.cos(), a.1 + len * ang.sin()),
        level,
        base_offset: 0.0,
        height: r.f(2.5, 4.0),
        wall_type: if r.below(3) == 0 { fx.wt2 } else { fx.wt },
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

fn column(r: &mut Rng, fx: &Fx, level: i64) -> ColumnSpec {
    ColumnSpec {
        level,
        top_level: (level == fx.l1).then_some(fx.l2),
        member_type: if r.below(2) == 0 { fx.ct } else { fx.bt },
        structural: r.below(2) == 0,
        at: pt(r),
        base_offset: 0.0,
        top_offset: r.f(-0.5, 0.0),
        rotation: r.f(0.0, 1.5),
        top_at: None,
    }
}

fn beam(r: &mut Rng, fx: &Fx, level: i64) -> BeamSpec {
    let a = pt(r);
    BeamSpec {
        level,
        member_type: if r.below(2) == 0 { fx.bt } else { fx.ct },
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

fn ceiling_spec(r: &mut Rng, fx: &Fx, level: i64) -> CeilingSpec {
    let (x, y) = (r.f(-2.0, 1.0), r.f(-2.0, 1.0));
    CeilingSpec {
        level,
        slab_type: fx.st,
        height: r.f(2.4, 2.9),
        outer: rect(x, y, r.f(3.0, 8.0), r.f(3.0, 8.0)),
        holes: if r.below(3) == 0 {
            vec![rect(x + 1.0, y + 1.0, 0.5, 0.5)]
        } else {
            vec![]
        },
    }
}

fn room_spec(r: &mut Rng, level: i64) -> RoomSpec {
    RoomSpec {
        level,
        name: ["Office", "Hall", "Store"][r.below(3)].into(),
        number: format!("{}", r.below(20)),
        point: (r.f(0.5, 7.5), r.f(0.5, 5.5)),
        height: r.f(2.4, 3.0),
    }
}

fn railing_spec(r: &mut Rng, fx: &Fx) -> RailingSpec {
    let (mut x, mut y) = pt(r);
    let n = 2 + r.below(3);
    let path = (0..n)
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
        .collect();
    let mut s = RailingSpec::new(fx.l1, RailingHost::Path(path));
    s.height = r.f(0.8, 1.1);
    s
}

fn mass_spec(r: &mut Rng, fx: &Fx) -> MassSpec {
    let (x, y) = (r.f(-6.0, 4.0), r.f(-6.0, 4.0));
    let (w, d, h) = (r.f(2.0, 8.0), r.f(2.0, 8.0), r.f(2.0, 9.0));
    MassSpec {
        level: fx.l1,
        name: format!("M{}", r.below(50)),
        void: false,
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

fn cw_spec(r: &mut Rng, fx: &Fx) -> CurtainSpec {
    let a = pt(r);
    let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
    CurtainSpec {
        level: fx.l1,
        name: format!("CW{}", r.below(100)),
        a,
        b: (a.0 + r.f(3.0, 8.0), a.1 + r.f(-1.0, 1.0)),
        bulge: 0.0,
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

fn mep_run(r: &mut Rng, m: &Model, fx: &Fx) -> Option<RunSpec> {
    let systems = m.mep_systems().unwrap_or_default();
    let sys = systems.get(r.below(systems.len().max(1)))?;
    let dom = sys.kind.domains()[0];
    let shape = match dom {
        MepDomain::Duct => MepShape::Rect {
            w: r.f(0.2, 0.6),
            h: r.f(0.2, 0.4),
        },
        _ => MepShape::Round { d: r.f(0.02, 0.15) },
    };
    let (mut x, mut y) = pt(r);
    let z = r.f(2.4, 2.8);
    let pts = (0..2 + r.below(3))
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
}

fn param_spec(r: &mut Rng) -> ParamSpec {
    let cats = ["wall", "room", "door"];
    ParamSpec {
        name: ["Cost", "Fire rating", "Code", "Count"][r.below(4)].into(),
        ptype: ParamType::ALL[r.below(ParamType::ALL.len())],
        per_type: false,
        unit: String::new(),
        group: "Data".into(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

fn pval(r: &mut Rng, t: ParamType) -> Option<PVal> {
    if r.below(8) == 0 {
        return None;
    }
    Some(match t {
        ParamType::Text => PVal::Text(format!("v{}", r.below(10))),
        ParamType::Int => PVal::Int(r.below(100) as i64),
        ParamType::YesNo => PVal::Bool(r.below(2) == 0),
        _ => PVal::Num(r.f(0.0, 50.0)),
    })
}

// ---------------------------------------------------------------- op construction

/// Elements the transform actions work on.
fn pool(m: &Model) -> Vec<Target> {
    let mut v = vec![];
    let mut add = |t: &str, f: fn(i64) -> Target| v.extend(ec::ids(m, t).into_iter().map(f));
    add("wall", Target::Wall);
    add("slab", Target::Slab);
    add("room", Target::Room);
    add("col", Target::Column);
    add("beam", Target::Beam);
    add("comp", Target::Component);
    add("mep_run", Target::MepRun);
    add("curtain_wall", Target::Curtain);
    add("ceiling", Target::Ceiling);
    v
}

fn pick_targets(r: &mut Rng, m: &Model) -> Option<Vec<Target>> {
    let p = pool(m);
    (0..1 + r.below(2)).map(|_| r.pick(&p)).collect()
}

fn vec2(r: &mut Rng) -> P {
    (r.f(-6.0, 6.0), r.f(-6.0, 6.0))
}

fn make_job(kind: Kind, seed: u64, m: &Model, fx: &Fx) -> Option<Job> {
    let r = &mut Rng::new(seed);
    let fx = *fx;
    match kind {
        Kind::Create => create(r, m, &fx),
        Kind::EditProp => edit_prop(r, m, &fx),
        Kind::TypeChange => type_change(r, m, &fx),
        Kind::LevelChange => level_change(r, m, &fx),
        Kind::Delete => delete(r, m, &fx),
        Kind::Move => {
            let (t, v) = (pick_targets(r, m)?, vec2(r));
            Some(job(move |m| m.move_elements(&t, v)))
        }
        Kind::Copy => {
            let t = pick_targets(r, m)?;
            let vs: Vec<P> = (0..1 + r.below(2)).map(|_| vec2(r)).collect();
            Some(job(move |m| m.copy_elements(&t, &vs).map(drop)))
        }
        Kind::Rotate => {
            let (t, c, a, cp) = (pick_targets(r, m)?, pt(r), r.f(-PI, PI), r.below(2) == 0);
            Some(job(move |m| m.rotate_elements(&t, c, a, cp).map(drop)))
        }
        Kind::Mirror => {
            let (t, a, b, cp) = (pick_targets(r, m)?, pt(r), pt(r), r.below(2) == 0);
            Some(job(move |m| m.mirror_elements(&t, a, b, cp).map(drop)))
        }
        Kind::Array => {
            let t = pick_targets(r, m)?;
            let n = 1 + r.below(3);
            if r.below(3) == 0 {
                let (c, tot) = (pt(r), [PI / 2.0, PI, TAU][r.below(3)]);
                Some(job(move |m| m.array_radial(&t, n + 1, c, tot).map(drop)))
            } else {
                let v = vec2(r);
                let s = if r.below(2) == 0 {
                    Spacing::Step(v)
                } else {
                    Spacing::Fit(v)
                };
                Some(job(move |m| m.array_linear(&t, n, s).map(drop)))
            }
        }
        Kind::Undo | Kind::Redo | Kind::SaveReopen | Kind::Xlsx | Kind::Ifc => None,
    }
}

fn create(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Job> {
    let fx = *fx;
    match r.below(17) {
        0 | 1 => {
            let l = level_of(r, &fx);
            let s = wall_spec(r, &fx, l);
            Some(job(move |m| m.add_wall(&s).map(drop)))
        }
        2 => {
            let (w, o) = (pid(r, m, "wall"), opening(r));
            Some(job(move |m| m.add_opening(w, &o).map(drop)))
        }
        3 => {
            let l = level_of(r, &fx);
            let s = column(r, &fx, l);
            Some(job(move |m| m.add_column(&s).map(drop)))
        }
        4 => {
            let l = level_of(r, &fx);
            let s = beam(r, &fx, l);
            Some(job(move |m| m.add_beam(&s).map(drop)))
        }
        5 => {
            let l = level_of(r, &fx);
            let s = slab_spec(r, &fx, l);
            Some(job(move |m| m.add_slab(&s).map(drop)))
        }
        6 => {
            let s = roof_spec(r, &fx);
            Some(job(move |m| m.add_roof(&s).map(drop)))
        }
        7 => {
            let s = shaft_spec(r, &fx);
            Some(job(move |m| m.add_shaft(&s).map(drop)))
        }
        8 => {
            let l = level_of(r, &fx);
            let s = ceiling_spec(r, &fx, l);
            Some(job(move |m| m.add_ceiling(&s).map(drop)))
        }
        9 => {
            let l = level_of(r, &fx);
            let s = room_spec(r, l);
            Some(job(move |m| m.add_room(&s).map(drop)))
        }
        10 => {
            let (a, len) = (pt(r), r.f(0.0, 6.0));
            Some(job(move |m| {
                m.add_stair(&StairSpec::straight(fx.l1, fx.l2, a, len))
                    .map(drop)
            }))
        }
        11 => {
            let s = railing_spec(r, &fx);
            Some(job(move |m| m.add_railing(&s).map(drop)))
        }
        12 => {
            let (x, y) = pt(r);
            let mut s = CompSpec::at(
                level_of(r, &fx),
                if r.below(2) == 0 { fx.table } else { fx.chair },
                x,
                y,
            );
            s.rotation = r.f(0.0, TAU);
            s.mirror = r.below(3) == 0;
            Some(job(move |m| m.add_component(&s).map(drop)))
        }
        13 => {
            let s = mass_spec(r, &fx);
            Some(job(move |m| m.add_mass(&s).map(drop)))
        }
        14 => {
            let s = cw_spec(r, &fx);
            Some(job(move |m| m.add_curtain_wall(&s).map(drop)))
        }
        15 => {
            let s = mep_run(r, m, &fx)?;
            Some(job(move |m| m.add_mep_run(&s).map(drop)))
        }
        _ => {
            let s = grid_spec(r);
            Some(job(move |m| m.add_grid(&s).map(drop)))
        }
    }
}

fn edit_prop(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Job> {
    let fx = *fx;
    match r.below(10) {
        0 => {
            let (w, a) = (pid(r, m, "wall"), pt(r));
            let b = (a.0 + r.f(-6.0, 6.0), a.1 + r.f(-6.0, 6.0));
            Some(job(move |m| m.set_wall_axis(w, a, b)))
        }
        1 => {
            let (i, o) = (pid(r, m, "opening"), opening(r));
            Some(job(move |m| m.update_opening(i, &o)))
        }
        2 => {
            let defs = m.params().unwrap_or_default();
            if defs.is_empty() {
                let s = param_spec(r);
                return Some(job(move |m| m.add_param(&s).map(drop)));
            }
            let d = &defs[r.below(defs.len())];
            let set = ParamSet {
                param: d.id,
                kind: "wall".into(),
                id: pid(r, m, "wall"),
                value: pval(r, d.spec.ptype),
            };
            Some(job(move |m| m.set_param_values(std::slice::from_ref(&set))))
        }
        3 => {
            let (i, l) = (pid(r, m, "col"), level_of(r, &fx));
            let s = column(r, &fx, l);
            Some(job(move |m| m.update_column(i, &s)))
        }
        4 => {
            let (i, l) = (pid(r, m, "beam"), level_of(r, &fx));
            let s = beam(r, &fx, l);
            Some(job(move |m| m.update_beam(i, &s)))
        }
        5 => {
            let (i, l) = (pid(r, m, "room"), level_of(r, &fx));
            let s = room_spec(r, l);
            Some(job(move |m| m.update_room(i, &s)))
        }
        6 => {
            let (i, o) = (
                pid(r, m, "slab"),
                rect(r.f(-4.0, 0.0), r.f(-4.0, 0.0), r.f(5.0, 9.0), r.f(5.0, 9.0)),
            );
            Some(job(move |m| m.set_slab_boundary(i, &o, &[])))
        }
        7 => {
            let (i, b) = (pid(r, m, "wall"), r.f(-0.3, 0.3));
            Some(job(move |m| m.set_wall_bulge(i, b)))
        }
        8 => {
            let i = pid(r, m, "comp");
            let mut s = m
                .component(i)
                .unwrap_or_else(|_| CompSpec::at(fx.l1, fx.table, 1.0, 1.0));
            s.at.0 += r.f(-1.0, 1.0);
            s.rotation = r.f(0.0, TAU);
            Some(job(move |m| m.update_component(i, &s)))
        }
        _ => {
            let (i, l) = (pid(r, m, "ceiling"), level_of(r, &fx));
            let s = ceiling_spec(r, &fx, l);
            Some(job(move |m| m.update_ceiling(i, &s)))
        }
    }
}

fn type_change(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Job> {
    let fx = *fx;
    match r.below(5) {
        0 => {
            let (w, t) = (pid(r, m, "wall"), pid(r, m, "wall_type"));
            Some(job(move |m| m.set_wall_type(w, t)))
        }
        1 => {
            let (i, l) = (pid(r, m, "col"), level_of(r, &fx));
            let mut s = column(r, &fx, l);
            s.member_type = if r.below(2) == 0 { fx.ct } else { fx.bt };
            Some(job(move |m| m.update_column(i, &s)))
        }
        2 => {
            let (i, l) = (pid(r, m, "beam"), level_of(r, &fx));
            let mut s = beam(r, &fx, l);
            s.member_type = if r.below(2) == 0 { fx.ct } else { fx.bt };
            Some(job(move |m| m.update_beam(i, &s)))
        }
        3 => {
            let i = pid(r, m, "comp");
            let (x, y) = pt(r);
            let s = CompSpec::at(
                fx.l1,
                if r.below(2) == 0 { fx.table } else { fx.chair },
                x,
                y,
            );
            Some(job(move |m| m.update_component(i, &s)))
        }
        _ => {
            let t = pid(r, m, "wall_type");
            Some(job(move |m| m.delete_wall_type(t)))
        }
    }
}

fn level_change(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Job> {
    let fx = *fx;
    match r.below(6) {
        0 | 1 => {
            let l = pid(r, m, "level");
            let name = m
                .level_list()
                .ok()?
                .into_iter()
                .find(|x| x.0 == l)
                .map_or_else(|| "L9".to_string(), |x| x.1);
            let z = r.f(-1.0, 9.0);
            Some(job(move |m| m.update_level(l, &name, z)))
        }
        2 => {
            let (i, l) = (pid(r, m, "col"), level_of(r, &fx));
            let s = column(r, &fx, l);
            Some(job(move |m| m.update_column(i, &s)))
        }
        3 => {
            let (i, l) = (pid(r, m, "beam"), level_of(r, &fx));
            let s = beam(r, &fx, l);
            Some(job(move |m| m.update_beam(i, &s)))
        }
        4 => {
            let (i, l) = (pid(r, m, "room"), level_of(r, &fx));
            let s = room_spec(r, l);
            Some(job(move |m| m.update_room(i, &s)))
        }
        _ => {
            let (i, l) = (pid(r, m, "ceiling"), level_of(r, &fx));
            let s = ceiling_spec(r, &fx, l);
            Some(job(move |m| m.update_ceiling(i, &s)))
        }
    }
}

const DELETABLE: &[&str] = &[
    "wall",
    "opening",
    "col",
    "beam",
    "slab",
    "room",
    "comp",
    "mep_run",
    "curtain_wall",
    "ceiling",
    "roof",
    "shaft",
    "stair",
    "railing",
    "mass",
    "level",
];

fn delete(r: &mut Rng, m: &Model, fx: &Fx) -> Option<Job> {
    let tables: Vec<&str> = DELETABLE
        .iter()
        .copied()
        .filter(|t| !ec::ids(m, t).is_empty())
        .collect();
    // levels go rarely: most later ops need them
    let mut t = *tables.get(r.below(tables.len().max(1)))?;
    if t == "level" && r.below(8) != 0 {
        t = "wall";
    }
    let mut id = pid(r, m, t);
    if t == "level" && id == fx.l1 {
        id = fx.l2;
    }
    Some(match t {
        "wall" => job(move |m| m.delete_wall(id)),
        "opening" => job(move |m| m.delete_opening(id)),
        "col" => job(move |m| m.delete_column(id)),
        "beam" => job(move |m| m.delete_beam(id)),
        "slab" => job(move |m| m.delete_slab(id)),
        "room" => job(move |m| m.delete_room(id)),
        "comp" => job(move |m| m.delete_component(id)),
        "mep_run" => job(move |m| m.delete_mep_run(id)),
        "curtain_wall" => job(move |m| m.delete_curtain_wall(id)),
        "ceiling" => job(move |m| m.delete_ceiling(id)),
        "roof" => job(move |m| m.delete_roof(id)),
        "shaft" => job(move |m| m.delete_shaft(id)),
        "stair" => job(move |m| m.delete_stair(id)),
        "railing" => job(move |m| m.delete_railing(id)),
        "mass" => job(move |m| m.delete_mass(id)),
        _ => job(move |m| m.delete_level(id)),
    })
}

// ---------------------------------------------------------------- invariants

fn shoelace(ring: &[P]) -> f64 {
    let n = ring.len();
    let mut a = 0.0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        a += x1 * y2 - x2 * y1;
    }
    (a * 0.5).abs()
}

/// Child table, column, parent table: rows must not point at nothing. Checked only when the
/// tables and the column exist in this schema.
const LINKS: &[(&str, &str, &str)] = &[
    ("opening", "wall", "wall"),
    ("wall", "level", "level"),
    ("wall", "wtype", "wall_type"),
    ("col", "level", "level"),
    ("beam", "level", "level"),
    ("slab", "level", "level"),
    ("slab", "stype", "slab_type"),
    ("room", "level", "level"),
    ("slab_vertex", "slab", "slab"),
    ("slab_qty", "slab", "slab"),
];

fn has_column(m: &Model, table: &str, col: &str) -> bool {
    m.sql()
        .query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
            [table, col],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
}

fn orphans(m: &Model) -> Vec<String> {
    let mut out = vec![];
    for (child, col, parent) in LINKS {
        if !has_column(m, child, col) || !has_column(m, parent, "id") {
            continue;
        }
        let sql = format!(
            "SELECT count(*) FROM \"{child}\" WHERE \"{col}\" IS NOT NULL \
             AND \"{col}\" NOT IN (SELECT id FROM \"{parent}\")"
        );
        let n: i64 = m.sql().query_row(&sql, [], |r| r.get(0)).unwrap_or(0);
        if n > 0 {
            out.push(format!("{n} {child}.{col} rows point at no {parent}"));
        }
    }
    out
}

fn fk_violations(m: &Model) -> Vec<String> {
    let Ok(mut st) = m.sql().prepare("PRAGMA foreign_key_check") else {
        return vec![];
    };
    let rows = st.query_map([], |r| {
        Ok(format!(
            "{} row {:?} -> {}",
            r.get::<_, String>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, String>(2)?
        ))
    });
    rows.map(|it| it.filter_map(Result::ok).take(5).collect())
        .unwrap_or_default()
}

fn quantity_problems(m: &Model) -> Vec<String> {
    let mut out = vec![];
    for id in ec::ids(m, "slab") {
        let Ok(rings) = m.slab_rings(id) else {
            continue;
        };
        let Some(outer) = rings.first() else {
            continue;
        };
        let ring_area = shoelace(outer) - rings.iter().skip(1).map(|h| shoelace(h)).sum::<f64>();
        let Ok(qs) = m.slab_quantities(id) else {
            continue;
        };
        for q in qs {
            let ok = q.area.is_finite()
                && q.volume.is_finite()
                && q.area >= -1e-9
                && q.volume >= -1e-9
                && q.area <= ring_area.max(0.0) * (1.0 + 1e-6) + 1e-6;
            if !ok {
                out.push(format!(
                    "slab {id} layer {}: area {} volume {} but rings give {ring_area}",
                    q.material, q.area, q.volume
                ));
            }
        }
    }
    for id in ec::ids(m, "room") {
        if let Ok(Some(q)) = m.room_quantities(id) {
            let ring_area = q.rings.first().map_or(0.0, |o| {
                shoelace(o) - q.rings.iter().skip(1).map(|h| shoelace(h)).sum::<f64>()
            });
            let ok = q.area.is_finite()
                && q.perimeter.is_finite()
                && q.volume.is_finite()
                && q.area >= -1e-9
                && q.area <= ring_area.max(0.0) * (1.0 + 1e-6) + 1e-6;
            if !ok {
                out.push(format!(
                    "room {id}: area {} but rings give {ring_area}",
                    q.area
                ));
            }
        }
    }
    for (t, id) in ec::ids(m, "col")
        .into_iter()
        .map(|i| ("col", i))
        .chain(ec::ids(m, "beam").into_iter().map(|i| ("beam", i)))
    {
        let q = if t == "col" {
            m.column_quantities(id)
        } else {
            m.beam_quantities(id)
        };
        if let Ok(q) = q {
            if !(q.length.is_finite() && q.volume.is_finite() && q.length >= 0.0 && q.volume >= 0.0)
            {
                out.push(format!("{t} {id}: length {} volume {}", q.length, q.volume));
            }
        }
    }
    for id in ec::ids(m, "wall") {
        if let Ok(len) = m.wall_length(id) {
            if !len.is_finite() || len < 0.0 {
                out.push(format!("wall {id}: length {len}"));
            }
        }
    }
    out
}

fn check_model(m: &Model) -> Result<(), String> {
    let iss = m.check_integrity().map_err(|e| format!("check_integrity: {e}"))?;
    if !iss.is_empty() {
        return Err(format!("integrity issues {iss:?}"));
    }
    let fk = fk_violations(m);
    if !fk.is_empty() {
        return Err(format!("foreign_key_check: {fk:?}"));
    }
    let o = orphans(m);
    if !o.is_empty() {
        return Err(format!("orphan rows: {o:?}"));
    }
    let q = quantity_problems(m);
    if !q.is_empty() {
        return Err(format!("quantities disagree with geometry: {q:?}"));
    }
    Ok(())
}

// ---------------------------------------------------------------- XLSX helpers

fn walls_rows(m: &Model) -> Vec<Vec<C>> {
    let sql = "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
               JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id";
    let Ok(mut st) = m.sql().prepare(sql) else {
        return vec![];
    };
    let rows = st.query_map([], |r| {
        let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64)];
        for i in 1..9 {
            match r.get::<_, rusqlite::types::Value>(i)? {
                rusqlite::types::Value::Text(s) => v.push(C::Text(s)),
                rusqlite::types::Value::Integer(x) => v.push(C::Num(x as f64)),
                rusqlite::types::Value::Real(x) => v.push(C::Num(x)),
                _ => v.push(C::Text(String::new())),
            }
        }
        Ok(v)
    });
    rows.map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Preview only: the model is not touched.
fn preview(m: &mut Model, p: &std::path::Path) -> Result<Report, String> {
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    xlsx::import_xlsx_with(m, p, o).map_err(|e| format!("import_xlsx preview: {e:?}"))
}

// ---------------------------------------------------------------- session

struct Sess {
    ed: Editor,
    fx: Fx,
    /// Model state after each accepted step since the start or the last reopen.
    states: Vec<Vec<TableRows>>,
    pos: usize,
    accepted: usize,
    /// Undo-then-redo identity is checked on every `every`-th accepted op.
    every: usize,
    files: Vec<PathBuf>,
}

impl Sess {
    fn new(every: usize) -> Result<Sess, String> {
        let mut ed = Editor::new_in_memory().map_err(|e| format!("new editor: {e}"))?;
        let fx = fixture(&mut ed)?;
        check_model(&ed.model).map_err(|e| format!("fixture: {e}"))?;
        if ed.can_undo() || ed.can_redo() {
            return Err("fixture is on the undo stack".into());
        }
        let first = snap(&ed.model);
        Ok(Sess {
            ed,
            fx,
            states: vec![first],
            pos: 0,
            accepted: 0,
            every: every.max(1),
            files: vec![],
        })
    }

    fn cur(&self) -> &[TableRows] {
        &self.states[self.pos]
    }

    fn step(&mut self, a: Act) -> Result<(), String> {
        match a.kind {
            Kind::Undo => self.undo(),
            Kind::Redo => self.redo(),
            Kind::SaveReopen => self.reopen(),
            Kind::Xlsx => self.xlsx(a.seed),
            Kind::Ifc => self.ifc(),
            k => match make_job(k, a.seed, &self.ed.model, &self.fx) {
                Some(j) => self.apply(j),
                None => Ok(()),
            },
        }
    }

    fn record(&mut self) {
        let now = snap(&self.ed.model);
        self.states.truncate(self.pos + 1);
        self.states.push(now);
        self.pos += 1;
        self.accepted += 1;
    }

    fn apply(&mut self, j: Job) -> Result<(), String> {
        let j0 = ec::journal_len(&self.ed.model);
        let redo0 = self.ed.can_redo();
        let res = self.ed.run_op(OpFn(j));
        match res {
            Ok(()) => {
                let n = ec::journal_len(&self.ed.model);
                if n != j0 + 1 {
                    return Err(format!("accepted op wrote {} journal entries", n - j0));
                }
                if !self.ed.can_undo() {
                    return Err("accepted op left no undo step".into());
                }
                if self.ed.can_redo() {
                    return Err("a new op must truncate redo".into());
                }
                self.record();
                if self.accepted % self.every == 0 {
                    self.undo_redo_identity()?;
                }
            }
            Err(e) => {
                same(
                    &snap(&self.ed.model),
                    self.cur(),
                    &format!("rejected op ({e}) changed the model"),
                )?;
                if ec::journal_len(&self.ed.model) != j0 {
                    return Err(format!("rejected op ({e}) wrote the journal"));
                }
                if self.ed.can_redo() != redo0 {
                    return Err(format!("rejected op ({e}) touched the redo stack"));
                }
            }
        }
        Ok(())
    }

    fn undo_redo_identity(&mut self) -> Result<(), String> {
        if self.pos == 0 {
            return Ok(());
        }
        self.ed.undo();
        same(
            &snap(&self.ed.model),
            &self.states[self.pos - 1],
            "undo of the last step",
        )?;
        check_model(&self.ed.model).map_err(|e| format!("after undo: {e}"))?;
        self.ed.redo();
        same(
            &snap(&self.ed.model),
            self.cur(),
            "redo right after undo is not the identity",
        )?;
        check_model(&self.ed.model).map_err(|e| format!("after redo: {e}"))
    }

    fn history_matches(&self) -> Result<(), String> {
        let (u, r) = (self.pos > 0, self.pos + 1 < self.states.len());
        if self.ed.can_undo() != u {
            return Err(format!("can_undo is {} but {} steps are done", !u, self.pos));
        }
        if self.ed.can_redo() != r {
            return Err(format!("can_redo is {} but {} steps are undone", !r, self.states.len() - 1 - self.pos));
        }
        Ok(())
    }

    fn undo(&mut self) -> Result<(), String> {
        self.history_matches()?;
        if self.pos == 0 {
            return Ok(());
        }
        self.ed.undo();
        self.pos -= 1;
        same(&snap(&self.ed.model), self.cur(), "undo")
    }

    fn redo(&mut self) -> Result<(), String> {
        self.history_matches()?;
        if self.pos + 1 >= self.states.len() {
            return Ok(());
        }
        self.ed.redo();
        self.pos += 1;
        same(&snap(&self.ed.model), self.cur(), "redo")
    }

    fn reopen(&mut self) -> Result<(), String> {
        let data = ec::dump_data(&self.ed.model);
        let jlen = ec::journal_len(&self.ed.model);
        let p = ec::temp_file("prop.ddc");
        self.files.push(p.clone());
        self.ed
            .model
            .snapshot_to(&p)
            .map_err(|e| format!("snapshot_to: {e}"))?;
        let r = Model::open(p.to_str().ok_or("temp path is not UTF-8")?)
            .map_err(|e| format!("reopen: {e}"))?;
        same(&without_added_materials(ec::dump_data(&r), &data), &data, "save then reopen")?;
        if ec::journal_len(&r) != jlen {
            return Err(format!(
                "reopen: journal has {} entries, was {jlen}",
                ec::journal_len(&r)
            ));
        }
        check_model(&r).map_err(|e| format!("reopened file: {e}"))?;
        self.ed
            .replace_model(r)
            .map_err(|e| format!("replace_model: {e}"))?;
        same(
            &without_added_materials(ec::dump_data(&self.ed.model), &data),
            &data,
            "editor reload of the reopened file changed data",
        )?;
        // the editor starts a new history on the reopened file
        self.states = vec![snap(&self.ed.model)];
        self.pos = 0;
        self.history_matches()
    }

    fn xlsx(&mut self, seed: u64) -> Result<(), String> {
        let mut r = Rng::new(seed);
        let before = snap(&self.ed.model);
        let p = ec::temp_file("prop.xlsx");
        self.files.push(p.clone());
        xlsx::export_xlsx(&self.ed.model, &p).map_err(|e| format!("export_xlsx: {e}"))?;
        let rep = preview(&mut self.ed.model, &p)?;
        same(
            &snap(&self.ed.model),
            &before,
            "xlsx export or preview changed the model",
        )?;
        if !rep.rejected.is_empty() || !rep.edits.is_empty() || !rep.changes.is_empty() {
            return Err(format!(
                "unchanged XLSX book is not neutral: rejected {:?} changes {:?}",
                rep.rejected, rep.changes
            ));
        }

        // edit one wall height in a walls sheet and apply it
        let mut rows = walls_rows(&self.ed.model);
        if rows.is_empty() {
            return Ok(());
        }
        let k = r.below(rows.len());
        rows[k][8] = C::Num(r.f(2.0, 4.5));
        for row in rows.iter_mut() {
            row.push(C::Text(String::new()));
        }
        let heads: Vec<String> = [
            "id",
            "level",
            "type",
            "ax [m]",
            "ay [m]",
            "bx [m]",
            "by [m]",
            "base_offset [m]",
            "height [m]",
            "delete",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p2 = ec::temp_file("prop_walls.xlsx");
        self.files.push(p2.clone());
        xlsx::export_table(&p2, "Walls", &heads, &[], &rows, None)
            .map_err(|e| format!("export_table: {e}"))?;
        let rep = preview(&mut self.ed.model, &p2)?;
        same(
            &snap(&self.ed.model),
            &before,
            "xlsx preview changed the model",
        )?;
        let batch = rep.batch();
        if !rep.rejected.is_empty() || batch.is_empty() {
            return Ok(());
        }
        match self.ed.apply_xlsx(batch, "property") {
            Ok(()) => {
                self.record();
                self.undo_redo_identity()
            }
            Err(e) => same(
                &snap(&self.ed.model),
                &before,
                &format!("rejected xlsx batch ({e}) changed the model"),
            ),
        }
    }

    fn ifc(&mut self) -> Result<(), String> {
        let before = snap(&self.ed.model);
        let a = export_ifc(&self.ed.model).map_err(|e| format!("export_ifc: {e}"))?;
        check_step(&a).map_err(|e| format!("exported STEP is invalid: {e}"))?;
        let b = export_ifc(&self.ed.model).map_err(|e| format!("export_ifc again: {e}"))?;
        if a != b {
            return Err("IFC export is not deterministic".into());
        }
        same(&snap(&self.ed.model), &before, "IFC export changed the model")
    }

    fn finish(&mut self) -> Result<(), String> {
        self.history_matches()?;
        let top = self.states.len() - 1;
        while self.pos > 0 {
            self.ed.undo();
            self.pos -= 1;
            same(
                &snap(&self.ed.model),
                self.cur(),
                &format!("final undo to state {}", self.pos),
            )?;
        }
        if self.ed.can_undo() {
            return Err("undo stack holds more steps than accepted ops".into());
        }
        check_model(&self.ed.model).map_err(|e| format!("after undo of everything: {e}"))?;
        while self.pos < top {
            self.ed.redo();
            self.pos += 1;
            same(
                &snap(&self.ed.model),
                self.cur(),
                &format!("final redo to state {}", self.pos),
            )?;
        }
        if self.ed.can_redo() {
            return Err("redo stack holds more steps than undone ops".into());
        }
        check_model(&self.ed.model).map_err(|e| format!("after redo of everything: {e}"))
    }
}

impl Drop for Sess {
    fn drop(&mut self) {
        for p in &self.files {
            for ext in ["", "-wal", "-shm"] {
                let mut s = p.as_os_str().to_owned();
                s.push(ext);
                let _ = std::fs::remove_file(s);
            }
        }
    }
}

// ---------------------------------------------------------------- driver

struct Fail {
    /// Index of the failing action; `None` when the end-of-run checks failed.
    step: Option<usize>,
    msg: String,
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "panic".into()
    }
}

fn run_inner(acts: &[Act], every: usize) -> Result<(), Fail> {
    let mut s = Sess::new(every).map_err(|msg| Fail { step: None, msg })?;
    for (i, a) in acts.iter().enumerate() {
        let fail = move |msg: String| Fail {
            step: Some(i),
            msg: format!("{a}: {msg}"),
        };
        s.step(*a).map_err(fail)?;
        check_model(&s.ed.model).map_err(fail)?;
        s.history_matches().map_err(fail)?;
    }
    s.finish().map_err(|msg| Fail { step: None, msg })
}

/// Runs a sequence; a panic in the editor or model is a failure too.
fn run(acts: &[Act], every: usize) -> Result<(), Fail> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| run_inner(acts, every))) {
        Ok(r) => r,
        Err(p) => Err(Fail {
            step: None,
            msg: format!("panic: {}", panic_text(p)),
        }),
    }
}

/// Smallest failing subsequence found by chunk removal (bounded number of re-runs).
fn shrink(mut acts: Vec<Act>, every: usize) -> Vec<Act> {
    let mut budget = 300;
    let mut n = (acts.len() / 2).max(1);
    loop {
        let (mut i, mut progress) = (0, false);
        while i < acts.len() && budget > 0 {
            let mut cand = acts.clone();
            cand.drain(i..(i + n).min(acts.len()));
            budget -= 1;
            if run(&cand, every).is_err() {
                acts = cand;
                progress = true;
            } else {
                i += n;
            }
        }
        if budget == 0 || (n == 1 && !progress) {
            break;
        }
        n = (n / 2).max(1);
    }
    acts
}

/// Runs one generated sequence; on failure shrinks it and returns the report.
fn check_seed(seed: u64, steps: usize, every: usize) -> Result<(), String> {
    let acts = gen_seq(seed, steps);
    let Err(f) = run(&acts, every) else {
        return Ok(());
    };
    let cut = f.step.map_or(acts.len(), |i| i + 1);
    let min = shrink(acts[..cut].to_vec(), every);
    let last = run(&min, every).err().map_or(f.msg.clone(), |x| x.msg);
    Err(format!(
        "PROPERTY FAIL seed={seed} steps={steps} first failure: {}\n\
         minimal sequence ({} of {} actions): {}\n\
         failure of the minimal sequence: {last}\n\
         replay: DDC_PROP_REPLAY=\"{}\" cargo test -p ddc-app --test e2e_property e2e_property_replay",
        f.msg,
        min.len(),
        cut,
        replay_string(&min),
        replay_string(&min),
    ))
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------- tests

#[test]
fn e2e_property_fixture_has_every_kind() {
    let mut ed = Editor::new_in_memory().unwrap();
    fixture(&mut ed).unwrap();
    for t in [
        "wall", "opening", "col", "beam", "slab", "room", "comp", "level",
    ] {
        assert!(ec::count(&ed.model, t) > 0, "fixture lacks {t}");
    }
    check_model(&ed.model).unwrap();
}

#[test]
fn e2e_property_sequences_are_reproducible() {
    // pure logic: generation and the replay string are deterministic
    let acts = gen_seq(3, 40);
    assert_eq!(acts.len(), 40);
    assert_eq!(parse_replay(&replay_string(&acts)).len(), 40);
    assert_eq!(
        gen_seq(3, 40).iter().map(|a| a.seed).collect::<Vec<_>>(),
        acts.iter().map(|a| a.seed).collect::<Vec<_>>(),
        "same seed, same sequence"
    );
}

/// Fixed seeds, short sequences: part of the normal suite.
#[test]
fn e2e_property_fixed_seeds() {
    let seeds = env_num("DDC_PROP_SEEDS", 4);
    let steps = env_num("DDC_PROP_STEPS", 200) as usize;
    let mut bad = vec![];
    for seed in 1..=seeds {
        if let Err(e) = check_seed(seed, steps, 1) {
            bad.push(e);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// Reruns the actions of `DDC_PROP_REPLAY` (a failure report prints the string).
#[test]
fn e2e_property_replay() {
    let Ok(s) = std::env::var("DDC_PROP_REPLAY") else {
        return;
    };
    let acts = parse_replay(&s);
    if let Err(f) = run(&acts, 1) {
        panic!("replay failed at {:?}: {}", f.step, f.msg);
    }
}

/// Night mode: thousands of steps, random seeds, until `DDC_PROP_SECS` is spent.
/// Prints one `PROP_RESULT {json}` line for tools/fuzz_night.ps1.
#[test]
#[ignore]
fn e2e_property_night() {
    let start = Instant::now();
    let budget = env_num("DDC_PROP_SECS", 60) as f64;
    let steps = env_num("DDC_PROP_STEPS", 2000) as usize;
    let first = env_num(
        "DDC_PROP_SEED",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64 % 1_000_000_007),
    );
    let (mut runs, mut seed) = (0u64, first);
    let mut failure: Option<(u64, String)> = None;
    while start.elapsed().as_secs_f64() < budget || runs == 0 {
        // the undo/redo identity every 7th accepted op keeps long runs affordable
        if let Err(e) = check_seed(seed, steps, 7) {
            failure = Some((seed, e));
            break;
        }
        runs += 1;
        seed += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    match &failure {
        None => println!(
            "PROP_RESULT {{\"status\":\"ok\",\"runs\":{runs},\"steps\":{steps},\"first_seed\":{first},\"secs\":{secs:.1}}}"
        ),
        Some((s, e)) => {
            eprintln!("{e}");
            let line = e.lines().nth(1).unwrap_or("").replace('"', "'");
            println!(
                "PROP_RESULT {{\"status\":\"fail\",\"runs\":{runs},\"steps\":{steps},\"seed\":{s},\"first_seed\":{first},\"secs\":{secs:.1},\"minimal\":\"{line}\"}}"
            );
        }
    }
    assert!(failure.is_none(), "{}", failure.map(|f| f.1).unwrap_or_default());
}
