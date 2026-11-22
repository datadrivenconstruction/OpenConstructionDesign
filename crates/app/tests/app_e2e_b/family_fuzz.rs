// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Matrix-driven fuzz over `FAMILIES`: random add, move, rotate, copy, delete, undo, redo per
//! family. Invariants after every step: integrity clean, `foreign_key_check` clean, no NaN or
//! infinity in any cell, hosted openings inside their wall, a refused action leaves the model
//! untouched, and every undo restores the table contents exactly.
//!
//! Fast mode (default): seeds 1 and 2, 100 steps per family (a replay defaults to 200).
//! Replay one run: DDC_FUZZ_SEED=<seed> DDC_FUZZ_TABLE=<table> [DDC_FUZZ_STEPS=<n>]
//! Long mode: cargo test -p ddc-app --release --test app_e2e_b family_fuzz:: -- --ignored --nocapture
//!   (DDC_FUZZ_SEEDS runs per family, default 40; DDC_FUZZ_STEPS steps, default 1500)
#![allow(dead_code)]

use crate::e2e_common;

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_app::elem::{Cap, Family, FAMILIES, NOT_YET};
use ddc_geom::v2;
use ddc_model::{
    AnnoShape, AnnoView, BeamJoin, BeamLayout, BeamSpec, BeamSystemSpec, BraceSpec, CeilingSpec,
    ColumnSpec, CompSpec, Error, FootingSpec, FootingStep, Form, GridSpec, Just, LayerSpec,
    MassSpec, MepDomain, MepSection, MepShape, Model, OpeningSpec, RaftSpec, RailingHost,
    RailingSpec, RampSpec, RebarHook, RebarLayout, RebarPath, RebarSetSpec, RoofKind, RoofSpec,
    RoomSpec, RunSpec, SepSpec, ShaftSpec, SlabSpec, SolidForm, SolidFrame, SolidSection,
    SolidSpec, StairSpec, StripFootingSpec, SystemKind, TopoSpec, TrussChord, TrussSpec, TrussWeb,
    WallSpec, ZoneSpec,
};
use e2e_common::{assert_same, dump_data, hash_of, ids, Rng, TableRows};

/// Families the fuzz cannot create from public specs. They are listed, never silently skipped.
const NO_SPAWN: &[(&str, &str)] = &[
    ("curtain_wall", "needs a grid and profile fixture"),
    ("mesh", "imported geometry has no spec"),
    ("analytical_adj", "row of an empty table, no id"),
];

/// Cells (table, action) where a row that says No may still succeed, or that are known to
/// misbehave. An Ok result there is accepted and still checked by the invariants. Empty on
/// purpose: add a cell only with the reason.
const GAPS: &[(&str, &str)] = &[];

const ACTIONS: [&str; 7] = ["add", "move", "rotate", "copy", "delete", "undo", "redo"];

#[derive(Clone, Copy)]
struct Setup {
    level: i64,
    top: i64,
    wt: i64,
    st: i64,
    mt: i64,
    ctype: i64,
    rtype: i64,
    room: i64,
    beam: i64,
}

fn square(x: f64, y: f64, s: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
}

fn setup(ed: &mut Editor) -> Setup {
    let level = ed.level;
    let layer = |t: f64| {
        [LayerSpec {
            material: "c".into(),
            thickness: t,
        }]
    };
    let m = &mut ed.model;
    let wt = m.add_wall_type("W fuzz", &layer(0.2)).unwrap();
    let st = m.add_slab_type("S fuzz", &layer(0.2)).unwrap();
    let mt = m.add_catalog_member_type("R 400x400").unwrap();
    let top = m.add_level("Fuzz top", 3.0).unwrap();
    for (a, b) in [
        ((0.0, 0.0), (8.0, 0.0)),
        ((8.0, 0.0), (8.0, 6.0)),
        ((8.0, 6.0), (0.0, 6.0)),
        ((0.0, 6.0), (0.0, 0.0)),
    ] {
        m.add_wall(&WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    }
    let room = m
        .add_room(&RoomSpec {
            level,
            name: "r".into(),
            number: "1".into(),
            point: (4.0, 3.0),
            height: 2.75,
        })
        .unwrap();
    let beam = m.add_beam(&beam_spec(level, mt, 44.0, 0.0)).unwrap();
    let family = m.load_library_family("Sink").unwrap();
    let ctype = m.comp_types(family).unwrap()[0].0;
    let rtype = m.add_rebar_type("T8", 0.008, "A500", 0.032).unwrap();
    ed.reload().unwrap();
    Setup {
        level,
        top,
        wt,
        st,
        mt,
        ctype,
        rtype,
        room,
        beam,
    }
}

fn beam_spec(level: i64, mt: i64, x: f64, y: f64) -> BeamSpec {
    BeamSpec {
        level,
        member_type: mt,
        a: (x, y, 2.75),
        b: (x + 6.0, y, 2.75),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Center,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    }
}

/// Creates one element of `table` at (x, y), far from the base room. None: not creatable here.
fn spawn(
    table: &str,
    m: &mut Model,
    s: &Setup,
    x: f64,
    y: f64,
    n: usize,
) -> Option<Result<i64, Error>> {
    let (lv, concrete) = (s.level, || "Concrete".to_string());
    Some(match table {
        "wall" => m.add_wall(&WallSpec {
            a: (x, y),
            b: (x + 6.0, y),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: s.wt,
            offset0: None,
        }),
        // always on a fresh wall of its own, so the host is valid by construction
        "opening" => m
            .add_wall(&WallSpec {
                a: (x, y),
                b: (x + 8.0, y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: s.wt,
                offset0: None,
            })
            .and_then(|w| {
                m.add_opening(
                    w,
                    &OpeningSpec {
                        along: 2.0 + (n % 3) as f64,
                        width: 1.0,
                        sill: 0.75,
                        height: 1.25,
                        kind: "window".into(),
                    },
                )
            }),
        "room" => m.add_room(&RoomSpec {
            level: lv,
            name: format!("r{n}"),
            number: format!("{n}"),
            point: (4.0, 3.0),
            height: 2.75,
        }),
        "room_sep" => m.add_room_sep(&SepSpec {
            level: lv,
            a: (x, y),
            b: (x + 4.0, y),
        }),
        "zone" => m.add_zone(&ZoneSpec {
            name: format!("z{n}"),
            rooms: vec![s.room],
        }),
        "slab" => m.add_slab(&SlabSpec {
            level: lv,
            slab_type: s.st,
            offset: 0.0,
            outer: square(x, y, 6.0),
            holes: vec![],
        }),
        "ceiling" => m.add_ceiling(&CeilingSpec {
            level: lv,
            slab_type: s.st,
            height: 2.75,
            outer: square(x, y, 6.0),
            holes: vec![],
        }),
        "roof" => m.add_roof(&RoofSpec {
            level: lv,
            slab_type: s.st,
            offset: 0.0,
            overhang: 0.25,
            fascia: 0.2,
            kind: RoofKind::Footprint {
                ring: square(x, y, 6.0),
                slopes: vec![Some(30.0), None, Some(30.0), None],
                mansard: None,
            },
        }),
        "col" => m.add_column(&ColumnSpec {
            level: lv,
            top_level: None,
            member_type: s.mt,
            structural: true,
            at: (x, y),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        }),
        "beam" => m.add_beam(&beam_spec(lv, s.mt, x, y)),
        "stair" => m.add_stair(&StairSpec::straight(lv, s.top, (x, y), 0.0)),
        "railing" => m.add_railing(&RailingSpec::new(
            lv,
            RailingHost::Path(vec![
                (x, y, 0.0),
                (x + 6.0, y, 0.0),
                (x + 6.0, y + 6.0, 0.0),
            ]),
        )),
        "mep_run" => {
            let sys = m
                .add_mep_system(&format!("S{n}"), SystemKind::SupplyAir)
                .unwrap();
            let mut duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.5, h: 0.25 });
            duct.wall = 0.001;
            m.add_mep_run(&RunSpec::new(
                sys,
                lv,
                duct,
                vec![(x, y, 2.75), (x + 6.0, y, 2.75)],
            ))
        }
        "comp" => m.add_component(&CompSpec::at(lv, s.ctype, x, y)),
        "iso_footing" => m.add_footing(&FootingSpec {
            level: lv,
            column: None,
            at: (x, y),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep {
                w: 1.2,
                l: 1.2,
                h: 0.5,
            }],
            top_offset: 0.0,
            material: concrete(),
        }),
        "strip_footing" => m
            .add_strip_footings(&[StripFootingSpec {
                level: lv,
                wall: None,
                a: (x, y),
                b: (x + 6.0, y),
                bulge: 0.0,
                width: 0.6,
                depth: 0.4,
                ecc: 0.0,
                top_offset: 0.0,
                material: concrete(),
            }])
            .map(|v| v[0]),
        "raft" => m.add_raft(&RaftSpec {
            level: lv,
            ring: square(x, y, 6.0),
            thickness: 0.4,
            edge_width: 0.0,
            edge_depth: 0.0,
            top_offset: 0.0,
            material: concrete(),
        }),
        "brace" => m.add_brace(&BraceSpec {
            level: lv,
            member_type: s.mt,
            a: (x, y, 0.0),
            b: (x + 4.0, y, 3.0),
            start: None,
            end: None,
            rotation: 0.0,
        }),
        "truss" => m.add_truss(&TrussSpec {
            level: lv,
            web: TrussWeb::parse("pratt"),
            chord: TrussChord::parse("flat"),
            a: (x, y, 3.0),
            b: (x + 8.0, y, 3.0),
            height: 1.2,
            heel: 0.2,
            panels: 4,
            chord_type: s.mt,
            web_type: s.mt,
        }),
        "beam_system" => m.add_beam_system(&BeamSystemSpec {
            level: lv,
            member_type: s.mt,
            ring: square(x, y, 6.0),
            holes: vec![],
            start_edge: None,
            z: 2.75,
            angle: 0.0,
            spacing: 1.0,
            layout: BeamLayout::parse_with("spacing", 0),
            end_gap: 0.0,
            zjust: Just::Center,
        }),
        "ramp" => m.add_ramp(&RampSpec::new(
            lv,
            Some(s.top),
            vec![(x, y), (x + 12.0, y)],
        )),
        "rebar_set" => m
            .add_rebar_sets(&[RebarSetSpec {
                host_kind: "beam".into(),
                host: s.beam,
                rtype: s.rtype,
                layout: RebarLayout::Stirrup {
                    rule: RebarPath::MaxSpacing(0.2),
                    hook: RebarHook::H135,
                },
                mark: None,
            }])
            .map(|v| v[0]),
        "shaft" => m.add_shaft(&ShaftSpec {
            level: lv,
            top_level: s.top,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: square(x, y, 2.0),
        }),
        "grid" => m.add_grid(&GridSpec::line(&format!("G{n}"), (x, y), (x, y + 10.0))),
        "ref_plane" => m.add_ref_plane(&format!("P{n}"), (x, y), (x, y + 10.0)),
        "mass" => m.add_mass(&MassSpec {
            level: lv,
            name: format!("m{n}"),
            void: false,
            inplace: false,
            form: Form::Cuboid {
                lo: (x, y, 0.0),
                hi: (x + 6.0, y + 6.0, 3.0),
            },
        }),
        "toposolid" => m.add_toposolid(&TopoSpec {
            name: format!("t{n}"),
            thickness: 2.0,
            material: "earth".into(),
            points: vec![
                (x, y, 0.0),
                (x + 6.0, y, 0.0),
                (x + 6.0, y + 6.0, 0.0),
                (x, y + 6.0, 0.0),
            ],
            ..Default::default()
        }),
        "anno" => m.add_anno(
            AnnoView::Plan(lv),
            &AnnoShape::Line {
                pts: vec![(x, y), (x + 4.0, y)],
                closed: false,
            },
            None,
            None,
        ),
        "solid" => m.add_solid(&SolidSpec {
            level: lv,
            name: format!("s{n}"),
            void: false,
            material: concrete(),
            ifc: None,
            frame: SolidFrame::default(),
            form: SolidForm::Extrusion {
                profile: SolidSection::polygon(
                    square(x, y, 4.0)
                        .into_iter()
                        .map(|p| v2(p.0, p.1))
                        .collect(),
                ),
                start: 0.0,
                end: 3.0,
            },
        }),
        _ => return None,
    })
}

type State = Vec<TableRows>;

fn counts(s: &State) -> Vec<(String, usize)> {
    s.iter().map(|t| (t.0.clone(), t.1.len())).collect()
}

fn row_exists(ed: &Editor, table: &str, id: i64) -> bool {
    ed.model
        .clip_copy(&[(table, id)])
        .is_ok_and(|c| !c.is_empty())
}

/// Everything that must hold after every step.
fn invariants(ed: &Editor, state: &State) -> Result<(), String> {
    let iss = ed.model.check_integrity().map_err(|e| e.to_string())?;
    if !iss.is_empty() {
        return Err(format!("integrity issues {iss:?}"));
    }
    let db = ed.model.sql();
    let mut st = db
        .prepare("PRAGMA foreign_key_check")
        .map_err(|e| e.to_string())?;
    let bad = st
        .query_map([], |_| Ok(()))
        .map_err(|e| e.to_string())?
        .count();
    if bad > 0 {
        return Err(format!("foreign_key_check: {bad} violations"));
    }
    for (table, rows) in state {
        for r in rows {
            if r.contains("Real(NaN)") || r.contains("Real(inf)") || r.contains("Real(-inf)") {
                return Err(format!("non-finite number in {table}: {r}"));
            }
        }
    }
    // hosted: an opening keeps its wall and stays within the wall length
    let mut st = db
        .prepare(
            "SELECT o.id, o.along, w.id, w.ax, w.ay, w.bx, w.by FROM opening o \
             LEFT JOIN wall w ON w.id = o.wall",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                [
                    r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                ],
            ))
        })
        .map_err(|e| e.to_string())?;
    for r in rows {
        let (id, along, wall, p) = r.map_err(|e| e.to_string())?;
        if wall.is_none() {
            return Err(format!("opening {id} lost its wall"));
        }
        let len = ((p[2] - p[0]).powi(2) + (p[3] - p[1]).powi(2)).sqrt();
        if along < -1e-6 || along > len + 1e-6 {
            return Err(format!(
                "opening {id} outside its wall: along {along}, wall {len}"
            ));
        }
    }
    Ok(())
}

#[derive(Default)]
struct Stats {
    done: [usize; 7],
    refused: [usize; 7],
    skipped: [usize; 7],
}

struct Fuzz {
    f: &'static Family,
    ed: Editor,
    s: Setup,
    rng: Rng,
    known: Vec<i64>,
    states: Vec<State>,
    redo: Vec<State>,
    n: usize,
    stats: Stats,
}

impl Fuzz {
    fn new(f: &'static Family, seed: u64) -> Self {
        let mut ed = Editor::new_in_memory().unwrap();
        let s = setup(&mut ed);
        let init = dump_data(&ed.model);
        Fuzz {
            f,
            ed,
            s,
            rng: Rng::new(seed),
            known: vec![],
            states: vec![init],
            redo: vec![],
            n: 0,
            stats: Stats::default(),
        }
    }

    /// Live ids of the family: those seen so far that still exist, plus the table rows.
    fn refresh(&mut self) {
        let t = self.f.table;
        let mut all = std::mem::take(&mut self.known);
        all.extend(ids(&self.ed.model, t));
        all.sort_unstable();
        all.dedup();
        self.known = all
            .into_iter()
            .filter(|id| row_exists(&self.ed, t, *id))
            .collect();
    }

    fn pick(&mut self) -> Option<i64> {
        self.refresh();
        let v = self.known.clone();
        self.rng.pick(&v)
    }

    /// Runs a mutating action; checks refusal and success invariants and updates the history.
    fn mutate(
        &mut self,
        ai: usize,
        expect_refusal: bool,
        act: impl FnOnce(&mut Fuzz) -> Result<(), String>,
    ) -> Result<(), String> {
        let before = dump_data(&self.ed.model);
        let head = self.ed.model.change_head();
        let res = act(self);
        let after = dump_data(&self.ed.model);
        let same = hash_of(&before) == hash_of(&after);
        if let Err(why) = res {
            self.stats.refused[ai] += 1;
            if !same {
                return Err(format!("a refused action changed the model ({why})"));
            }
            return Ok(());
        }
        if expect_refusal {
            return Err("the action should have been refused".into());
        }
        self.stats.done[ai] += 1;
        invariants(&self.ed, &after)?;
        if !same || self.ed.model.change_head() != head {
            self.states.push(after);
            self.redo.clear();
        }
        Ok(())
    }

    fn add(&mut self) -> Result<(), String> {
        let x = self.rng.f(100.0, 400.0).round();
        let y = self.rng.f(100.0, 400.0).round();
        let (table, s, n) = (self.f.table, self.s, self.n);
        let made = Arc::new(AtomicI64::new(0));
        let out = made.clone();
        self.mutate(0, false, move |z| {
            z.ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                match spawn(table, m, &s, x, y, n) {
                    Some(r) => r.map(|id| out.store(id, Ordering::Relaxed)),
                    None => Ok(()),
                }
            })))
            .map_err(|e| e.to_string())
        })?;
        let id = made.load(Ordering::Relaxed);
        if id != 0 {
            self.known.push(id);
        }
        Ok(())
    }

    fn xf(&mut self, ai: usize, name: &'static str) -> Result<(), String> {
        let Some(id) = self.pick() else {
            self.stats.skipped[ai] += 1;
            return Ok(());
        };
        let cap = match name {
            "move" => self.f.xf.mv,
            "rotate" => self.f.xf.rotate,
            _ => self.f.xf.copy,
        };
        let target = match (self.f.target, cap) {
            (_, Cap::No(r)) if r == NOT_YET => {
                self.stats.skipped[ai] += 1;
                return Ok(());
            }
            (Cap::No(_), _) => {
                self.stats.skipped[ai] += 1;
                return Ok(());
            }
            (Cap::Yes(t), _) => t(id),
        };
        // a design refusal (hosted elements and the like) must refuse and change nothing
        let expect_refusal = matches!(cap, Cap::No(_)) && !GAPS.contains(&(self.f.table, name));
        let v = (self.rng.f(-5.0, 5.0), self.rng.f(-5.0, 5.0));
        let c = (self.rng.f(0.0, 10.0), self.rng.f(0.0, 10.0));
        let ang = self.rng.f(-3.0, 3.0);
        self.mutate(ai, expect_refusal, move |z| {
            z.ed.run_op(OpFn(Arc::new(move |m: &mut Model| match name {
                "move" => m.move_elements(&[target], v),
                "rotate" => m.rotate_elements(&[target], c, ang, false).map(|_| ()),
                _ => m.copy_elements(&[target], &[v]).map(|_| ()),
            })))
            .map_err(|e| e.to_string())
        })
    }

    fn delete(&mut self) -> Result<(), String> {
        let Some(id) = self.pick() else {
            self.stats.skipped[4] += 1;
            return Ok(());
        };
        let sel = self.f.sel(id);
        if sel.is_none() || matches!(self.f.xf.delete, Cap::No(_)) {
            self.stats.skipped[4] += 1;
            return Ok(());
        }
        let (sel, table) = (sel.unwrap(), self.f.table);
        self.mutate(4, false, move |z| {
            z.ed.selection = vec![sel];
            z.ed.delete_selection();
            // a delete the family refuses keeps the row and must not touch the model
            if row_exists(&z.ed, table, id) {
                Err("the row is still there".into())
            } else {
                Ok(())
            }
        })
    }

    fn undo(&mut self) -> Result<(), String> {
        if self.states.len() < 2 {
            self.stats.skipped[5] += 1;
            return Ok(());
        }
        self.ed.undo();
        let gone = self.states.pop().unwrap();
        self.redo.push(gone);
        let now = dump_data(&self.ed.model);
        assert_same(&now, self.states.last().unwrap(), "undo");
        invariants(&self.ed, &now)?;
        self.stats.done[5] += 1;
        Ok(())
    }

    fn redo(&mut self) -> Result<(), String> {
        let Some(want) = self.redo.pop() else {
            self.stats.skipped[6] += 1;
            return Ok(());
        };
        self.ed.redo();
        let now = dump_data(&self.ed.model);
        // redo may hand out new row ids: sizes must match, the rest is the invariants
        if counts(&now) != counts(&want) {
            return Err(format!(
                "redo changed the row counts: {:?} vs {:?}",
                counts(&now),
                counts(&want)
            ));
        }
        invariants(&self.ed, &now)?;
        self.states.push(now);
        self.stats.done[6] += 1;
        Ok(())
    }

    fn step(&mut self) -> Result<(), String> {
        self.n += 1;
        let roll = if self.n == 1 { 0 } else { self.rng.below(100) };
        match roll {
            0..=29 => self.add(),
            30..=44 => self.xf(1, "move"),
            45..=54 => self.xf(2, "rotate"),
            55..=64 => self.xf(3, "copy"),
            65..=74 => self.delete(),
            75..=89 => self.undo(),
            _ => self.redo(),
        }
    }
}

fn mix(seed: u64, table: &str) -> u64 {
    table.bytes().fold(seed.wrapping_mul(0x0100_0000_01B3), |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01B3)
    })
}

/// One family, one seed. A failure prints the replay line first.
fn fuzz_one(f: &'static Family, seed: u64, steps: usize) {
    let replay = format!(
        "DDC_FUZZ_SEED={seed} DDC_FUZZ_TABLE={} DDC_FUZZ_STEPS={steps}",
        f.table
    );
    println!("family_fuzz table={} seed={seed} steps={steps}", f.table);
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut z = Fuzz::new(f, mix(seed, f.table));
        for _ in 0..steps {
            if let Err(e) = z.step() {
                panic!("step {}: {e}", z.n);
            }
        }
        let st = &z.stats;
        let row = |k: usize| {
            format!(
                "{} {}/{}/{}",
                ACTIONS[k], st.done[k], st.refused[k], st.skipped[k]
            )
        };
        let all: Vec<String> = (0..ACTIONS.len()).map(row).collect();
        println!("  {} ok/refused/skipped: {}", f.table, all.join(", "));
    }));
    if let Err(e) = r {
        println!("FAILED. replay: {replay}");
        resume_unwind(e);
    }
}

fn env_u64(k: &str) -> Option<u64> {
    std::env::var(k).ok().and_then(|s| s.parse().ok())
}

fn run(seeds: &[u64], steps: usize) {
    let only = std::env::var("DDC_FUZZ_TABLE").ok();
    let mut fuzzed = 0;
    for f in FAMILIES {
        if only.as_deref().is_some_and(|t| t != f.table) {
            continue;
        }
        if let Some((_, why)) = NO_SPAWN.iter().find(|x| x.0 == f.table) {
            println!("family_fuzz table={} not fuzzed: {why}", f.table);
            continue;
        }
        fuzzed += 1;
        for &seed in seeds {
            fuzz_one(f, seed, steps);
        }
    }
    assert!(fuzzed > 0, "no family matched DDC_FUZZ_TABLE");
}

#[test]
fn every_family_has_a_spawner_or_a_reason() {
    let mut ed = Editor::new_in_memory().unwrap();
    let s = setup(&mut ed);
    for f in FAMILIES {
        if NO_SPAWN.iter().any(|x| x.0 == f.table) {
            continue;
        }
        let r = spawn(f.table, &mut ed.model, &s, 100.0, 100.0, 0);
        assert!(
            r.is_some(),
            "family {} has no spawner and no NO_SPAWN reason",
            f.table
        );
    }
}

#[test]
fn family_fuzz_fast() {
    if let Some(seed) = env_u64("DDC_FUZZ_SEED") {
        let steps = env_u64("DDC_FUZZ_STEPS").unwrap_or(200) as usize;
        return run(&[seed], steps);
    }
    run(&[1, 2], 100);
}

#[test]
#[ignore = "long mode, many seeds"]
fn family_fuzz_long() {
    let steps = env_u64("DDC_FUZZ_STEPS").unwrap_or(1500) as usize;
    let n = env_u64("DDC_FUZZ_SEEDS").unwrap_or(40);
    let base = env_u64("DDC_FUZZ_SEED").unwrap_or(1000);
    let seeds: Vec<u64> = (0..n).map(|i| base + i).collect();
    run(&seeds, steps);
}
