//! Life cycle of every element family, driven by the `FAMILIES` table alone. Per family and per
//! capability the table claims: select in plan and 3D, edit every property row, move, copy, rotate,
//! mirror, array, grip drag, delete, and a chained run of them. After each step:
//! - rows: a whole-file row dump (every table but derived caches) is compared, so an undo that
//!   leaves one column behind is named by table, row id and column;
//! - undo restores the exact prior state, redo reapplies it (a step that creates rows may get new
//!   ids on redo: then table counts and 3D size must match);
//! - no dangling or orphan rows (`check_integrity` against the fixture's own baseline);
//! - no non-finite value in rows, 3D or plan outlines, no zero-extent outline;
//! - the 3D fingerprint and the plan signature change when the step moves geometry, and a pure
//!   selection changes nothing;
//! - the model is saved, reopened and compared row by row.
//!
//! A capability whose table claim fails is a failure naming family, step and difference. `GAPS` of
//! the matrix are respected by (family, step): a listed cell must fail, and one that passes fails the
//! test so the list shrinks. `lifecycle_fast` runs in CI (one variant per step, one reopen per
//! step); `lifecycle_full` (`--ignored`) runs every variant and reopens after every case.

use super::matrix::{self, ctx, op, row_exists, Ctx, GAPS};
use super::{Cap, Family, FAMILIES};
use crate::edit3d::pick::Target as Pick3d;
use crate::geom_props::tests::{plan_sig, scene_fp};
use crate::geom_props::{rows, type_options, Effect, Unit};
use crate::grips::{self, Role};
use crate::selection::outlines;
use crate::editor::Sel;
use ddc_geom::v2;
use ddc_model::{Error, Issue, Model, Spacing, Target};
use rusqlite::types::ValueRef;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

type Probe = Result<(), String>;
/// table -> rowid -> "column=value" cells
type Dump = BTreeMap<String, BTreeMap<i64, Vec<String>>>;

/// Steps in the order they run; the names are the probe names of `GAPS`.
const STEPS: [&str; 10] = [
    "plan", "3d", "props", "grips", "move", "copy", "rotate", "mirror", "array", "delete",
];
const CHAIN: &str = "chain";

/// Families whose rows are not meshes of the composed 3D scene: rooms and separators are pick
/// bodies only, rebar and datum planes have their own overlays, a shaft cuts slabs it may not meet.
const NOT_COMPOSED: [&str; 7] = ["room", "zone", "rebar_set", "analytical_adj", "grid", "ref_plane", "shaft"];

fn composed(f: &Family) -> bool {
    f.cat3d.is_some() && !NOT_COMPOSED.contains(&f.table)
}

fn xf_geom(f: &Family) -> Geom {
    if composed(f) { Geom::Both } else { Geom::SceneOrPlan }
}

/// Derived caches and bookkeeping that undo does not own.
const SKIP_TABLES: [&str; 10] = [
    "mesh_proto",
    "mesh_geo",
    "mesh_epoch",
    "mesh_rev",
    "mass_cache",
    "join_cache",
    "meta",
    "ai_audit",
    "ai_policy",
    "ai_chat",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Fast,
    Full,
}

/// What a step must do to geometry. Rows must change in every case but `Any`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Geom {
    Both,
    Scene,
    SceneOrPlan,
    Rows,
    Any,
}

#[derive(Clone, Copy)]
struct Exp {
    geom: Geom,
    creates: bool,
    /// A clean refusal (nothing changed) passes: used at the edges of a row's range.
    refusable: bool,
}

struct State {
    dump: Dump,
    scene: Vec<String>,
    plan: String,
}

struct Life {
    c: Ctx,
    base: Vec<Issue>,
    mode: Mode,
    /// Cases run, over the whole family.
    cases: usize,
    /// Cases run in the current cell.
    cell: usize,
    seq: usize,
}

fn life(table: &str, mode: Mode, cases: usize) -> Life {
    let c = ctx(table);
    let base = c.ed.model.check_integrity().unwrap_or_default();
    Life { c, base, mode, cases, cell: 0, seq: 0 }
}

fn es<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---- row dump and diff --------------------------------------------------------------------

fn dump(m: &Model) -> Result<Dump, String> {
    let db = m.sql();
    let names: Vec<String> = {
        let mut st = db
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(es)?;
        let it = st.query_map([], |r| r.get::<_, String>(0)).map_err(es)?;
        it.collect::<Result<_, _>>().map_err(es)?
    };
    let mut out = Dump::new();
    for t in names {
        if SKIP_TABLES.contains(&t.as_str()) {
            continue;
        }
        // virtual and without-rowid tables cannot be listed this way: nothing to compare there
        let Ok(mut st) = db.prepare(&format!("SELECT rowid, * FROM \"{t}\"")) else {
            continue;
        };
        let cols: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
        let mut table = BTreeMap::new();
        let mut q = st.query([]).map_err(es)?;
        while let Some(r) = q.next().map_err(es)? {
            let key: i64 = r.get(0).map_err(es)?;
            let mut cells = Vec::with_capacity(cols.len());
            for (i, col) in cols.iter().enumerate().skip(1) {
                // the journal stamp is a nonce: a redo writes a fresh one
                if t == "op" && col == "stamp" {
                    continue;
                }
                let v = match r.get_ref(i).map_err(es)? {
                    ValueRef::Null => "NULL".to_string(),
                    ValueRef::Integer(n) => n.to_string(),
                    ValueRef::Real(x) if x.is_finite() => format!("{x:?}"),
                    ValueRef::Real(x) => {
                        return Err(format!("non-finite value {x} in {t}#{key}.{col}"));
                    }
                    ValueRef::Text(s) => String::from_utf8_lossy(s).into_owned(),
                    ValueRef::Blob(b) => {
                        let sum = b.iter().fold(0u64, |a, x| a.wrapping_mul(131).wrapping_add(*x as u64));
                        format!("blob[{}]#{sum:x}", b.len())
                    }
                };
                cells.push(format!("{col}={v}"));
            }
            table.insert(key, cells);
        }
        // recomputed tables get fresh row ids after undo: compare their content only
        if t.ends_with("_qty") || t.ends_with("_cut") || t == "struct_issue" || t == "dim_geo" {
            let mut rows: Vec<Vec<String>> = table.into_values().collect();
            rows.sort();
            table = rows.into_iter().enumerate().map(|(i, r)| (i as i64, r)).collect();
        }
        if !table.is_empty() {
            out.insert(t, table);
        }
    }
    Ok(out)
}

/// Readable row-level difference: changed columns, added and removed rows. At most `LINES` lines.
fn diff(a: &Dump, b: &Dump) -> Vec<String> {
    const LINES: usize = 10;
    let empty = BTreeMap::new();
    let mut out = vec![];
    let mut total = 0usize;
    let tables: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for t in tables {
        let (x, y) = (a.get(t).unwrap_or(&empty), b.get(t).unwrap_or(&empty));
        let keys: BTreeSet<&i64> = x.keys().chain(y.keys()).collect();
        for k in keys {
            let line = match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) if p != q => {
                    let ch: Vec<String> = p
                        .iter()
                        .zip(q.iter())
                        .filter(|(u, v)| u != v)
                        .map(|(u, v)| format!("{u} -> {v}"))
                        .collect();
                    format!("{t}#{k}: {}", ch.join(", "))
                }
                (Some(p), None) => format!("-{t}#{k} {}", short(p)),
                (None, Some(q)) => format!("+{t}#{k} {}", short(q)),
                _ => continue,
            };
            total += 1;
            if out.len() < LINES {
                out.push(line);
            }
        }
    }
    if total > LINES {
        out.push(format!("... and {} more rows", total - LINES));
    }
    out
}

fn short(cells: &[String]) -> String {
    let s = cells.join(" ");
    if s.chars().count() > 70 {
        format!("{}...", s.chars().take(70).collect::<String>())
    } else {
        s
    }
}

fn counts(d: &Dump) -> BTreeMap<&str, usize> {
    d.iter().map(|(t, r)| (t.as_str(), r.len())).collect()
}

// ---- state, invariants ---------------------------------------------------------------------

fn state(l: &mut Life) -> Result<State, String> {
    Ok(State {
        dump: dump(&l.c.ed.model)?,
        scene: scene_fp(&mut l.c.ed, &mut l.c.cache),
        plan: plan_sig(&l.c.ed),
    })
}

fn same(a: &State, b: &State) -> bool {
    a.dump == b.dump && a.scene == b.scene && a.plan == b.plan
}

/// Exact equality with a message that names what differs.
fn exact(label: &str, want: &State, got: &State) -> Probe {
    if want.dump != got.dump {
        return Err(format!(
            "{label}: rows differ\n    {}",
            diff(&want.dump, &got.dump).join("\n    ")
        ));
    }
    if want.scene != got.scene {
        return Err(format!("{label}: rows equal but the 3D scene fingerprint differs"));
    }
    if want.plan != got.plan {
        return Err(format!("{label}: rows equal but the plan signature differs"));
    }
    Ok(())
}

fn loose(a: &State, b: &State) -> bool {
    counts(&a.dump) == counts(&b.dump) && a.scene.len() == b.scene.len()
}

/// A token is non-finite when it is exactly the debug text of a NaN or an infinity.
fn non_finite(scene: &[String]) -> Option<&str> {
    scene.iter().map(String::as_str).find(|s| {
        s.split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '-' | '+')))
            .any(|t| matches!(t, "NaN" | "inf" | "-inf" | "+inf"))
    })
}

fn invariants(l: &mut Life, f: &Family, st: &State) -> Probe {
    let bad = l
        .c
        .ed
        .model
        .integrity_regressions(&l.base)
        .map_err(es)?;
    // an overlap is a design warning the user sees (a copy beside its original), not a broken row
    let bad: Vec<_> = bad.into_iter().filter(|i| !i.problem.contains(" overlaps ")).collect();
    if !bad.is_empty() {
        let lines: Vec<String> = bad
            .iter()
            .take(6)
            .map(|i| format!("{}#{}: {}", i.table, i.id, i.problem))
            .collect();
        return Err(format!(
            "orphan or dangling rows ({}):\n    {}",
            bad.len(),
            lines.join("\n    ")
        ));
    }
    if let Some(t) = non_finite(&st.scene) {
        return Err(format!(
            "non-finite number in the 3D scene: {}",
            t.chars().take(80).collect::<String>()
        ));
    }
    if let Some(sel) = f.sel(l.c.id).filter(|_| row_exists(&l.c.ed, f.table, l.c.id)) {
        for ring in outlines(&l.c.ed, sel) {
            if ring.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
                return Err("non-finite point in the plan outline".into());
            }
            let first = match ring.first() {
                Some(p) => *p,
                None => continue,
            };
            if ring.len() >= 2 && ring.iter().all(|p| (p.x - first.x).abs() < 1e-12 && (p.y - first.y).abs() < 1e-12) {
                return Err("degenerate plan outline: all points coincide".into());
            }
        }
    }
    Ok(())
}

// ---- save and reopen -----------------------------------------------------------------------

fn reopen(l: &mut Life, f: &Family, want: &Dump) -> Probe {
    l.seq += 1;
    let dir = std::env::temp_dir().join("ddc_elem_life");
    std::fs::create_dir_all(&dir).map_err(es)?;
    let stem = format!("{}_{}_{}", std::process::id(), f.table, l.seq);
    let (snap, work) = (dir.join(format!("{stem}.snap")), dir.join(format!("{stem}.work")));
    l.c.ed.model.snapshot_to(&snap).map_err(|e| format!("save: {e}"))?;
    let r = (|| -> Probe {
        let m = Model::open_snapshot(&snap, &work).map_err(|e| format!("reopen: {e}"))?;
        let mut got = dump(&m)?;
        // opening adds a registry row for each material text in use; that is not a data change
        if let Some(g) = got.get_mut("material") {
            g.retain(|k, _| want.get("material").is_some_and(|w| w.contains_key(k)));
        }
        if &got != want {
            return Err(format!(
                "save and reopen changed rows\n    {}",
                diff(want, &got).join("\n    ")
            ));
        }
        for r in rows().iter().filter(|r| r.kind == f.table) {
            let (a, b) = ((r.get)(&l.c.ed.model, l.c.id), (r.get)(&m, l.c.id));
            if let (Ok(a), Ok(b)) = (a, b) {
                if a != b {
                    return Err(format!("reopened row {}: {b}, live {a}", r.key));
                }
            }
        }
        let issues = m.integrity_regressions(&l.base).map_err(es)?;
        if let Some(i) = issues.iter().find(|i| !i.problem.contains(" overlaps ")) {
            return Err(format!("reopened file: {}#{}: {}", i.table, i.id, i.problem));
        }
        Ok(())
    })();
    for ext in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{ext}", snap.display()));
        let _ = std::fs::remove_file(format!("{}{ext}", work.display()));
    }
    r
}

// ---- one step ------------------------------------------------------------------------------

/// Runs the op and checks the step's own expectations. None: refused cleanly where that is allowed.
fn apply(
    l: &mut Life,
    f: &Family,
    label: &str,
    exp: Exp,
    run: impl Fn(&mut Model) -> Result<(), Error> + Send + Sync + 'static,
) -> Result<Option<(State, State)>, String> {
    let before = state(l)?;
    let r = op(&mut l.c.ed, run);
    l.cases += 1;
    l.cell += 1;
    if let Err(e) = r {
        if exp.refusable {
            let now = state(l)?;
            exact(&format!("{label}: refused ({e}) but changed the model"), &before, &now)?;
            return Ok(None);
        }
        return Err(format!("{label}: refused: {e}"));
    }
    let after = state(l)?;
    let at = |e: String| format!("{label}: {e}");
    invariants(l, f, &after).map_err(at)?;
    let rows_moved = after.dump != before.dump;
    let (scene, plan) = (after.scene != before.scene, after.plan != before.plan);
    if exp.geom != Geom::Any && !rows_moved {
        return Err(at("no row changed".into()));
    }
    let missing = match exp.geom {
        Geom::Both if !scene || !plan => Some(match (scene, plan) {
            (false, false) => "neither the 3D fingerprint nor the plan changed",
            (false, true) => "the 3D fingerprint did not change",
            _ => "the plan signature did not change",
        }),
        Geom::Scene if !scene => Some("the 3D fingerprint did not change"),
        Geom::SceneOrPlan if !scene && !plan => Some("neither the 3D fingerprint nor the plan changed"),
        _ => None,
    };
    if let Some(m) = missing {
        return Err(at(format!("{m}\n    {}", diff(&before.dump, &after.dump).join("\n    "))));
    }
    if !rows_moved && (scene || plan) {
        return Err(at("scene or plan changed while no row did".into()));
    }
    if l.mode == Mode::Full || l.cell == 1 {
        reopen(l, f, &after.dump).map_err(at)?;
    }
    Ok(Some((before, after)))
}

/// Undo is exact, redo reapplies, a second undo is exact again.
fn roundtrip(l: &mut Life, f: &Family, label: &str, before: &State, after: &State, creates: bool) -> Probe {
    l.c.ed.undo();
    let u = state(l)?;
    exact(&format!("{label}: undo"), before, &u)?;
    l.c.ed.redo();
    let r = state(l)?;
    if let Err(e) = exact(&format!("{label}: redo"), after, &r) {
        if !(creates && loose(after, &r)) {
            return Err(e);
        }
    }
    invariants(l, f, &r).map_err(|e| format!("{label}: after redo: {e}"))?;
    l.c.ed.undo();
    let u2 = state(l)?;
    exact(&format!("{label}: undo after redo"), before, &u2)
}

// ---- cells ---------------------------------------------------------------------------------

fn select_cell(l: &mut Life, f: &Family, probe: fn(&mut Ctx, &Family) -> Probe) -> Probe {
    let before = state(l)?;
    l.c.ed.selection.clear();
    probe(&mut l.c, f)?;
    l.cases += 1;
    let after = state(l)?;
    exact("selection must not touch rows, 3D or plan", &before, &after)?;
    invariants(l, f, &after)
}

fn props_cell(l: &mut Life, f: &Family) -> Probe {
    matrix::props(f)?;
    let id = l.c.id;
    for r in rows().into_iter().filter(|r| r.kind == f.table) {
        let who = format!("row {}", r.key);
        if r.key == "geom.landing" {
            // the landing depth only exists on a U stair (an L landing is as wide as the flight)
            let mut st = l.c.ed.model.stair(id).map_err(|e| format!("{who}: {e}"))?;
            st.shape = ddc_model::StairShape::U { left: true, first: None, gap: 0.1 };
            l.c.ed.model.update_stair(id, &st).map_err(|e| format!("{who}: {e}"))?;
        }
        let v0 = match (r.get)(&l.c.ed.model, id) {
            Ok(v) => v,
            // the fixture's layout has no such number (rebar spacing on a bar-count rule)
            Err(e) if e.to_string().contains("no such number") => continue,
            Err(e) => return Err(format!("{who}: read: {e}")),
        };
        let v1 = match r.unit {
            Unit::Type(t) => type_options(&l.c.ed.model, t)
                .last()
                .ok_or_else(|| format!("{who}: no other type to switch to"))?
                .0 as f64,
            _ => v0 + r.delta,
        };
        if v1 == v0 {
            return Err(format!("{who}: the fixture offers no other value"));
        }
        let main = match r.effect {
            Effect::Scene if !composed(f) => Geom::Rows,
            Effect::ScenePlan if !composed(f) => Geom::SceneOrPlan,
            Effect::Scene => Geom::Scene,
            Effect::ScenePlan => Geom::Both,
            Effect::Qty => Geom::Rows,
        };
        // (value, geometry rule, may be refused)
        let mut vals = vec![(v1, main, false)];
        if l.mode == Mode::Full {
            match r.unit {
                Unit::Len | Unit::Deg | Unit::Pct | Unit::Count => {
                    for b in [r.lo, r.hi] {
                        if b.is_finite() && b != v0 && b != v1 {
                            vals.push((b, Geom::Any, true));
                        }
                    }
                }
                Unit::Choice(keys) => {
                    for i in 0..keys.len() {
                        let v = i as f64;
                        if v != v0 && v != v1 {
                            vals.push((v, Geom::Rows, false));
                        }
                    }
                }
                Unit::Type(t) => {
                    for (tid, _) in type_options(&l.c.ed.model, t) {
                        let v = tid as f64;
                        if v != v0 && v != v1 {
                            vals.push((v, Geom::Rows, false));
                        }
                    }
                }
            }
        }
        for (val, geom, refusable) in vals {
            let label = format!("{who} {v0} -> {val}");
            let set = r.set;
            let exp = Exp { geom, creates: false, refusable };
            let Some((b, a)) = apply(l, f, &label, exp, move |m| set(m, id, val))? else {
                continue;
            };
            if !refusable {
                let got = (r.get)(&l.c.ed.model, id).map_err(|e| format!("{label}: read back: {e}"))?;
                if (got - val).abs() > 1e-9 {
                    return Err(format!("{label}: stored {got}"));
                }
            }
            roundtrip(l, f, &label, &b, &a, false)?;
            let back = (r.get)(&l.c.ed.model, id).map_err(es)?;
            if back != v0 {
                return Err(format!("{label}: after undo the row reads {back}, wanted {v0}"));
            }
        }
    }
    Ok(())
}

fn grip_cell(l: &mut Life, f: &Family) -> Probe {
    let kind = match f.grips {
        Cap::Yes(k) => k,
        Cap::No(r) => return matrix::no_route(r),
    };
    let id = l.c.id;
    let all = grips::grips(&l.c.ed.model, kind, id).map_err(es)?;
    let mut drag: Vec<_> = all
        .into_iter()
        .filter(|g| matches!(g.role, Role::Move | Role::End(_) | Role::Vertex(_) | Role::Edge(_)))
        .collect();
    if drag.is_empty() {
        return Err("no draggable plan grip".into());
    }
    if f.cat3d.and_then(|cat| Pick3d::new(cat, id).grip_kind()) != Some(kind) {
        return Err("no 3D grips".into());
    }
    let offsets: &[(f64, f64)] = if l.mode == Mode::Full {
        &[(0.5, 0.25), (-1.0, 0.75)]
    } else {
        // one grip in the fast run, the move grip when there is one
        let at = drag.iter().position(|g| g.role == Role::Move).unwrap_or(0);
        drag = vec![drag.swap_remove(at)];
        &[(0.5, 0.25)]
    };
    for g in drag {
        for (dx, dy) in offsets {
            let label = format!("grip {:?} {:?} by ({dx}, {dy})", g.kind, g.role);
            let to = g.pos + v2(*dx, *dy);
            let g2 = g.clone();
            let exp = Exp { geom: Geom::SceneOrPlan, creates: false, refusable: false };
            let Some((b, a)) = apply(l, f, &label, exp, move |m| grips::apply(m, &g2, to))? else {
                continue;
            };
            roundtrip(l, f, &label, &b, &a, false)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Xf {
    Move(f64, f64),
    Copy(f64, f64),
    Rotate(f64, bool),
    Mirror(bool, bool),
    Array(usize),
}

impl Xf {
    fn creates(self) -> bool {
        matches!(self, Xf::Copy(..) | Xf::Array(_) | Xf::Rotate(_, true) | Xf::Mirror(_, true))
    }
}

fn variants(step: &str, mode: Mode) -> Vec<Xf> {
    let full = mode == Mode::Full;
    let mut v = match step {
        "move" => vec![Xf::Move(0.5, 0.25)],
        "copy" => vec![Xf::Copy(0.5, 0.25)],
        "rotate" => vec![Xf::Rotate(0.5, false)],
        "mirror" => vec![Xf::Mirror(true, false)],
        _ => vec![Xf::Array(3)],
    };
    if full {
        v.extend(match step {
            "move" => vec![Xf::Move(-3.0, 2.0), Xf::Move(0.0, 40.0), Xf::Move(1000.0, -1000.0)],
            "copy" => vec![Xf::Copy(-3.0, 2.0), Xf::Copy(0.0, 40.0)],
            "rotate" => vec![
                Xf::Rotate(std::f64::consts::FRAC_PI_2, false),
                Xf::Rotate(std::f64::consts::PI, false),
                Xf::Rotate(-0.5, false),
                Xf::Rotate(0.5, true),
            ],
            "mirror" => vec![Xf::Mirror(false, false), Xf::Mirror(true, true)],
            _ => vec![Xf::Array(2), Xf::Array(5)],
        });
    }
    v
}

fn run_xf(m: &mut Model, t: Target, x: Xf) -> Result<usize, Error> {
    match x {
        Xf::Move(dx, dy) => m.move_elements(&[t], (dx, dy)).map(|_| 0),
        Xf::Copy(dx, dy) => m.copy_elements(&[t], &[(dx, dy)]).map(|v| v.len()),
        Xf::Rotate(a, copy) => m.rotate_elements(&[t], (0.0, 0.0), a, copy).map(|v| v.len()),
        Xf::Mirror(vertical, copy) => {
            let (a, b) = if vertical {
                ((-1.0, 0.0), (-1.0, 1.0))
            } else {
                ((0.0, -1.0), (1.0, -1.0))
            };
            m.mirror_elements(&[t], a, b, copy).map(|v| v.len())
        }
        Xf::Array(n) => m
            .array_linear(&[t], n, Spacing::Step((0.5, 0.25)))
            .map(|v| v.len()),
    }
}

fn xf_cap(f: &Family, step: &str) -> Cap<()> {
    match step {
        "move" => f.xf.mv,
        "copy" => f.xf.copy,
        "rotate" => f.xf.rotate,
        "mirror" => f.xf.mirror,
        _ => f.xf.array,
    }
}

/// The table's target constructor for a step, or the way the step is not available.
fn xf_target(f: &Family, step: &str) -> Result<fn(i64) -> Target, Probe> {
    match (f.target, xf_cap(f, step)) {
        (_, Cap::No(r)) if r == super::NOT_YET => Err(Err("not yet".into())),
        (Cap::No(_), Cap::No(r)) => Err(matrix::no_route(r)),
        (Cap::No(r), Cap::Yes(())) => Err(Err(format!("no Target: {r}"))),
        (Cap::Yes(t), _) => Ok(t),
    }
}

fn xf_cell(l: &mut Life, f: &Family, step: &str) -> Probe {
    let target = match xf_target(f, step) {
        Ok(t) => t,
        Err(done) => return done,
    };
    let t = target(l.c.id);
    let first = variants(step, l.mode)[0];
    if let Cap::No(r) = xf_cap(f, step) {
        matrix::no_route(r)?;
        let before = state(l)?;
        let refused = op(&mut l.c.ed, move |m| run_xf(m, t, first).map(|_| ())).is_err();
        l.cases += 1;
        let after = state(l)?;
        exact(&format!("{step}: a refused action changed the model"), &before, &after)?;
        return if refused { Ok(()) } else { Err(format!("{step} is declared refused but ran")) };
    }
    for x in variants(step, l.mode) {
        let label = format!("{step} {x:?}");
        let made = Arc::new(AtomicUsize::new(0));
        let out = made.clone();
        let exp = Exp { geom: xf_geom(f), creates: x.creates(), refusable: false };
        let Some((b, a)) = apply(l, f, &label, exp, move |m| {
            out.store(run_xf(m, t, x)?, Ordering::Relaxed);
            Ok(())
        })?
        else {
            continue;
        };
        let n = made.load(Ordering::Relaxed);
        let ok = match x {
            Xf::Copy(..) => n == 1,
            Xf::Array(k) => n >= k - 1 && n <= k,
            Xf::Rotate(_, true) | Xf::Mirror(_, true) => n >= 1,
            _ => true,
        };
        if !ok {
            return Err(format!("{label}: wrong number of new elements: {n}"));
        }
        roundtrip(l, f, &label, &b, &a, x.creates())?;
    }
    Ok(())
}

fn delete_once(l: &mut Life, f: &Family, label: &str, sels: Vec<Sel>, gone: &[(&'static str, i64)]) -> Result<(State, State), String> {
    let before = state(l)?;
    l.c.ed.selection = sels;
    l.c.ed.delete_selection();
    l.cases += 1;
    l.cell += 1;
    let after = state(l)?;
    for (table, id) in gone {
        if row_exists(&l.c.ed, table, *id) {
            return Err(format!("{label}: the {table} row {id} is still there"));
        }
    }
    if composed(f) && f.table != "room_sep" && after.scene == before.scene {
        return Err(format!("{label}: the 3D fingerprint did not change: the body stays in 3D"));
    }
    // annotations and rooms are not part of the plan signature (matrix gap E8); their row is checked above
    if !matches!(f.table, "anno" | "room" | "room_sep" | "analytical_adj") && after.scene == before.scene && after.plan == before.plan {
        return Err(format!("{label}: neither the 3D fingerprint nor the plan changed"));
    }
    invariants(l, f, &after).map_err(|e| format!("{label}: {e}"))?;
    if l.mode == Mode::Full || l.cell == 1 {
        reopen(l, f, &after.dump).map_err(|e| format!("{label}: {e}"))?;
    }
    Ok((before, after))
}

fn delete_cell(l: &mut Life, f: &Family) -> Probe {
    if let Cap::No(r) = f.xf.delete {
        return matrix::no_route(r);
    }
    let key = super::rebar::axis_key(&l.c.ed.model, l.c.id).filter(|_| f.table == "analytical_adj");
    let sel = f.sel(key.unwrap_or(l.c.id)).ok_or("no selection")?;
    let (table, id, wall) = (f.table, l.c.id, l.c.free_wall);
    let (b, a) = delete_once(l, f, "delete", vec![sel], &[(table, id)])?;
    roundtrip(l, f, "delete", &b, &a, false)?;
    if l.mode == Mode::Full && table != "wall" {
        let gone = [(table, id), ("wall", wall)];
        let (b, a) = delete_once(l, f, "delete mixed with a wall", vec![sel, Sel::Wall(wall)], &gone)?;
        roundtrip(l, f, "delete mixed with a wall", &b, &a, false)?;
    }
    if table == "curtain_wall" {
        // a panel never takes its wall with it, and what it does take comes back with one undo
        let before = state(l)?;
        l.c.ed.selection = vec![Sel::Panel(id, 0)];
        l.c.ed.delete_selection();
        l.cases += 1;
        if !row_exists(&l.c.ed, table, id) {
            return Err("deleting a sub-element removed its parent".into());
        }
        let now = state(l)?;
        if !same(&before, &now) {
            invariants(l, f, &now).map_err(|e| format!("sub-element delete: {e}"))?;
            l.c.ed.undo();
            let u = state(l)?;
            exact("sub-element delete: undo", &before, &u)?;
        }
    }
    Ok(())
}

/// All of move, rotate, copy, mirror, array and delete stacked, then undone and redone step by step.
fn chain_cell(l: &mut Life, f: &Family) -> Probe {
    for step in ["move", "rotate", "copy", "mirror", "array"] {
        if xf_target(f, step).is_err() || !matches!(xf_cap(f, step), Cap::Yes(())) {
            return Ok(());
        }
    }
    let Ok(target) = xf_target(f, "move") else {
        return Ok(());
    };
    let t = target(l.c.id);
    let script = [
        Xf::Move(0.5, 0.25),
        Xf::Rotate(0.5, false),
        Xf::Copy(1.0, 1.0),
        Xf::Mirror(true, false),
        Xf::Array(3),
    ];
    let mut stack: Vec<(String, State, State, bool)> = vec![];
    for x in script {
        let label = format!("chain step {} {x:?}", stack.len() + 1);
        let exp = Exp { geom: xf_geom(f), creates: x.creates(), refusable: false };
        let Some((b, a)) = apply(l, f, &label, exp, move |m| run_xf(m, t, x).map(|_| ()))? else {
            continue;
        };
        stack.push((label, b, a, x.creates()));
    }
    if matches!(f.xf.delete, Cap::Yes(())) {
        if let Some(sel) = f.sel(l.c.id) {
            let (b, a) = delete_once(l, f, "chain delete", vec![sel], &[(f.table, l.c.id)])?;
            stack.push(("chain delete".into(), b, a, false));
        }
    }
    for (label, before, _, _) in stack.iter().rev() {
        l.c.ed.undo();
        let u = state(l)?;
        exact(&format!("{label}: undo in the chain"), before, &u)?;
    }
    for (label, _, after, creates) in stack.iter() {
        l.c.ed.redo();
        let r = state(l)?;
        if let Err(e) = exact(&format!("{label}: redo in the chain"), after, &r) {
            if !(*creates && loose(after, &r)) {
                return Err(e);
            }
        }
        invariants(l, f, &r).map_err(|e| format!("{label}: after redo: {e}"))?;
    }
    for (label, before, _, _) in stack.iter().rev() {
        l.c.ed.undo();
        let u = state(l)?;
        exact(&format!("{label}: second undo in the chain"), before, &u)?;
    }
    Ok(())
}

fn cell(l: &mut Life, f: &Family, step: &str) -> Probe {
    match step {
        "plan" => select_cell(l, f, matrix::plan),
        "3d" => select_cell(l, f, matrix::three_d),
        "props" => props_cell(l, f),
        "grips" => grip_cell(l, f),
        "delete" => delete_cell(l, f),
        CHAIN => chain_cell(l, f),
        other => xf_cell(l, f, other),
    }
}

// ---- driver --------------------------------------------------------------------------------

/// Failures, and the number of cases each family ran.
fn run_mode(mode: Mode) -> (Vec<String>, BTreeMap<&'static str, usize>) {
    let mut fails = vec![];
    let mut cases = BTreeMap::new();
    for f in FAMILIES {
        let mut l = life(f.table, mode, 0);
        let steps = STEPS.iter().copied().chain((mode == Mode::Full).then_some(CHAIN));
        for step in steps {
            l.cell = 0;
            let r = catch_unwind(AssertUnwindSafe(|| cell(&mut l, f, step)))
                .unwrap_or_else(|_| Err("panicked".into()));
            let listed = GAPS.iter().any(|g| g.0 == f.table && g.1 == step);
            match (&r, listed) {
                (Ok(()), true) => fails.push(format!(
                    "[{}] {step}: listed in GAPS but the full life-cycle check passes now: remove it",
                    f.table
                )),
                (Err(e), false) => fails.push(format!("[{}] {step}: {e}", f.table)),
                _ => {}
            }
            if r.is_err() {
                // a failed step may leave the model changed: the next one starts clean
                l = life(f.table, mode, l.cases);
            }
        }
        cases.insert(f.table, l.cases);
    }
    (fails, cases)
}

fn report(mode: Mode) {
    let (fails, cases) = run_mode(mode);
    let total: usize = cases.values().sum();
    println!("life cycle {mode:?}: {total} cases, per family {cases:?}");
    assert!(
        fails.is_empty(),
        "{} life-cycle failures ({mode:?}):\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// Every family, one variant per step, one save and reopen per step.
#[test]
fn lifecycle_fast() {
    report(Mode::Fast);
}

/// Every variant, a save and reopen after every case, plus the chained run.
#[test]
#[ignore = "full life cycle of every family: cargo test lifecycle_full -- --ignored"]
fn lifecycle_full() {
    report(Mode::Full);
}

#[test]
fn step_names_match_the_gap_probes() {
    for g in GAPS {
        assert!(
            STEPS.contains(&g.1) || ["schedule", "xlsx", "strings"].contains(&g.1),
            "GAPS names an unknown step {}",
            g.1
        );
    }
}

/// The harness itself: a change made behind the undo stack is reported with table, row and column.
#[test]
fn a_change_outside_undo_is_named() {
    let f = super::by_table("wall").unwrap();
    let mut l = life("wall", Mode::Fast, 0);
    let before = state(&mut l).unwrap();
    let t = Target::Wall(l.c.id);
    l.c.ed.model.move_elements(&[t], (0.5, 0.25)).unwrap();
    l.c.ed.reload().unwrap();
    let after = state(&mut l).unwrap();
    let e = exact("probe", &before, &after).unwrap_err();
    assert!(e.contains("wall#"), "{e}");
    assert!(e.contains(" -> "), "{e}");
    assert!(invariants(&mut l, f, &after).is_ok());
}

#[test]
fn non_finite_tokens_are_found_exactly() {
    assert!(non_finite(&["m:[1.0, NaN, 2.0]".into()]).is_some());
    assert!(non_finite(&["m:[1.0, -inf]".into()]).is_some());
    assert!(non_finite(&["infill:[1.0, 2.0]".into()]).is_none());
}
