// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A project from a brief as one plan item. The spec is stored and the builder runs on every
//! compile; each storey writes into its own fixed slot block. Apply also stores the spec and
//! the element of every slot in the model (`prop`, kind [`RECIPE`], ref 0), so a later
//! revision changes, adds and deletes only what differs and keeps the ids of the rest.
//! An element changed or deleted by hand since is left alone and listed as a skip.

use super::exec::{ModelStep, NewResolved, PropSet};
use super::gen_map::{free_grid, layout_items, level_taken, new_level, new_type, type_ref, Bound};
use super::plan::{
    compile_create, push, resolve_group, CompileCx, ElRef, IdOrNew, NewElem, PlanError, RoofVault,
};
use super::rel_value::Skip;
use ddc_gen::project::assembly::{self, Assembly, Use, WallUse};
use ddc_gen::project::site::GSplit;
use ddc_gen::project::{self, locks, project_keys, slots, GProject, Gates, PStorey, ProjectSpec};
use ddc_gen::{FireSep, WallFire, WallRole};
use ddc_gen::project::railings::RailOn;
use ddc_gen::project::structure;
use ddc_model::{
    Edit, LayerSpec, Model, OpeningSpec, PVal, ParamSpec, ParamType, RoomSpec, Swing, WallSpec,
};
use rusqlite::types::Value;
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet};

mod dc_plan;
mod timber_plan;

/// `prop.kind` of the stored projects.
pub const RECIPE: &str = "gen_project";
/// Parapet of a flat roof above the deck.
const PARAPET: f64 = ddc_gen::project::PARAPET;
const FINISHES: [&str; 3] = ["Floor Finish", "Wall Finish", "Ceiling Finish"];
/// Property keys the IFC export writes as the common property sets.
const WALL_BEARING: &str = "Pset_WallCommon.LoadBearing";
const WALL_FIRE: &str = "Pset_WallCommon.FireRating";
const DOOR_FIRE: &str = "Pset_DoorCommon.FireRating";
const SLAB_BEARING: &str = "Pset_SlabCommon.LoadBearing";
const SLAB_FIRE: &str = "Pset_SlabCommon.FireRating";
/// Door and window property sets: the mark, the ratings and the exit and access flags.
const DOOR_PSET: &str = "Pset_DoorCommon";
const WINDOW_PSET: &str = "Pset_WindowCommon";
/// Deleted first: hosted and bounded elements before their hosts.
const DELETE_ORDER: [&str; 15] = [
    "railing", "elec_circuit", "mep_run", "comp", "curtain_wall", "beam", "col", "opening", "room", "stair", "shaft", "roof", "grid", "slab", "wall",
];

#[derive(Clone, Debug)]
pub struct ProjectOp {
    /// The spec as the assistant wrote it, with every revision merged in.
    pub input: J,
    /// Level of the ground floor; the other levels are found or made above it.
    pub level: i64,
    pub slot0: u32,
    /// Stored project id such as `P1`: a stored one is revised, not built again.
    pub pid: String,
}

#[derive(Clone)]
pub struct Built {
    pub spec: ProjectSpec,
    pub project: GProject,
    pub gates: Gates,
}

impl From<project::block::Built> for Built {
    fn from(b: project::block::Built) -> Built {
        Built { spec: b.spec, project: b.project, gates: b.gates }
    }
}

/// Buildings a block built in parallel, by the brief text a compile builds: each item's compile
/// takes its building from here instead of building it again on the worker thread.
static PRIMED: std::sync::Mutex<std::collections::VecDeque<(String, Built)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
/// Covers the largest block the tool takes; older ones are built again when compiled.
const PRIMED_MAX: usize = 200;

/// `b` is what [`build`] gives for `input`: the next build of the same text reads it.
pub fn prime(input: &J, b: Built) {
    let key = input.to_string();
    let mut q = PRIMED.lock().unwrap_or_else(|e| e.into_inner());
    q.retain(|e| e.0 != key);
    q.push_back((key, b));
    while q.len() > PRIMED_MAX {
        q.pop_front();
    }
}

fn primed(input: &J) -> Option<Built> {
    let key = input.to_string();
    let q = PRIMED.lock().unwrap_or_else(|e| e.into_inner());
    q.iter().find(|e| e.0 == key).map(|e| e.1.clone())
}

/// Spec, build and gates; a failure is the full text the assistant needs to fix the spec.
pub fn build(input: &J) -> Result<Built, String> {
    if let Some(b) = primed(input) {
        return Ok(b);
    }
    project::brief::set_request_text(&ddc_ai::convo::request_text());
    let spec = project::parse_spec(input).map_err(|e| e.to_string())?;
    // a brief that parses but cannot be built gets the smallest conflicting set of its demands
    // and the nearest relaxations
    let why_not = |e: String| match project::conflict::find(input) {
        Some(c) => format!("{e}\nwhy not: {}", c.to_json()),
        None => e,
    };
    let p = project::build_project(&spec).map_err(|e| why_not(e.to_string()))?;
    let gates = project::check_project(&spec, &p);
    if !gates.passed() {
        return Err(why_not(format!(
            "the project fails its gates: {}",
            gates.failures.join("; ")
        )));
    }
    Ok(Built {
        spec,
        project: p,
        gates,
    })
}

/// [`build`] on the model's terrain when the spec asks for it.
pub fn build_on(m: &Model, input: &J) -> Result<Built, String> {
    build(&with_model_terrain(m, input))
}

/// `site.terrain: "model"` becomes the survey points of the model's first toposolid, moved
/// into the brief's frame (before `origin`); the stored input keeps `"model"`, so a rebuild
/// reads the terrain as it is then. No toposolid leaves the word for the reader to refuse.
pub fn with_model_terrain(m: &Model, input: &J) -> J {
    let input = &super::mep_gate::gated(m, input);
    if input["site"]["terrain"].as_str() != Some("model") {
        return input.clone();
    }
    let Some(id) = m.toposolids().ok().and_then(|t| t.first().map(|x| x.0)) else {
        return input.clone();
    };
    let o = (input["origin"][0].as_f64().unwrap_or(0.0), input["origin"][1].as_f64().unwrap_or(0.0));
    let pts: Vec<J> = m
        .topo_points(id)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, (x, y, z))| json!([x - o.0, y - o.1, z]))
        .collect();
    let mut out = input.clone();
    out["site"]["terrain"] = json!({ "points": pts });
    out
}

/// A brief built from [`with_model_terrain`] goes back to the word `"model"` its source had.
pub fn keep_model_terrain(source: &J, brief: &mut J) {
    if source["site"]["terrain"].as_str() == Some("model") && brief["site"].is_object() {
        brief["site"]["terrain"] = json!("model");
    }
}

/// What the assistant hears: sizes, counts and the gate advice, never the geometry.
pub fn summary(b: &Built) -> J {
    let metrics: serde_json::Map<String, J> = b
        .project
        .metrics
        .iter()
        .map(|m| (m.0.to_string(), json!((m.1 * 100.0).round() / 100.0)))
        .collect();
    let counts: serde_json::Map<String, J> = b
        .gates
        .counts
        .iter()
        .map(|c| (c.0.to_string(), json!(c.1)))
        .collect();
    let warnings: Vec<&String> = b.gates.warnings.iter().take(10).collect();
    let mut out = json!({
        "typology": b.spec.typology_name(),
        // the brief's storeys, area, rooms and features against the build, with a repair patch
        "brief_check": ddc_gen::project::brief::check(&b.spec, &b.project).report,
        // the ten most telling decisions with the rule behind each; explain(ref) has the rest
        "why": ddc_gen::project::why::top(&b.spec, &b.project, 10),
        // the penalty per tier and the goals that cost it; explain("score") words them
        "score": ddc_gen::project::score::score(&b.spec, &b.project).to_json(),
        "metrics": metrics,
        "counts": counts,
        "gates": "passed: walls do not overlap, rooms closed and within 2 % of the asked \
                  area, a door into every room, windows in every habitable room, stairs fit",
        "warnings": warnings,
        "more_warnings": b.gates.warnings.len().saturating_sub(warnings.len()),
    });
    // racks, cooling, power and the redundancy scheme when a room is a data hall
    let dc = ddc_gen::project::dc::design(&b.spec, &b.project);
    if !dc.is_empty() {
        out["data_centre"] = dc.report();
    }
    // glulam beams and CLT floor panels when the structure is timber
    let timber = ddc_gen::project::timber::design(&b.spec, &b.project);
    if !timber.is_empty() {
        out["timber"] = timber.report();
    }
    out
}

// ---- the stored recipe ----

/// The element a slot wrote: `intent` hashes what the builder asked for, `row` the model
/// row right after; a different row later means a hand edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Rec {
    pub table: String,
    pub id: i64,
    pub intent: u64,
    pub row: u64,
}

impl Rec {
    fn line(&self, rel: u32) -> String {
        format!(
            "{rel} {} {} {:016x} {:016x}",
            self.table, self.id, self.intent, self.row
        )
    }

    fn parse(line: &str) -> Option<(u32, Rec)> {
        let mut p = line.split(' ');
        let rel = p.next()?.parse().ok()?;
        let table = p.next()?.to_string();
        let id = p.next()?.parse().ok()?;
        let intent = u64::from_str_radix(p.next()?, 16).ok()?;
        let row = u64::from_str_radix(p.next()?, 16).ok()?;
        Some((
            rel,
            Rec {
                table,
                id,
                intent,
                row,
            },
        ))
    }
}

#[derive(Default)]
pub struct Stored {
    /// `{input, level, roof}` of the last Apply.
    pub head: J,
    pub recs: BTreeMap<u32, Rec>,
    /// Record text by storey block, to write back only what changed.
    blocks: BTreeMap<u32, String>,
}

fn props(m: &Model, like: &str) -> Result<Vec<(String, String)>, PlanError> {
    let sql = "SELECT key, CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=0 AND key LIKE ?2";
    let mut st = m.sql().prepare(sql)?;
    let rows = st
        .query_map([RECIPE, like], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|r| (r.0, r.1.unwrap_or_default()))
        .collect())
}

fn pid_number(pid: &str) -> u64 {
    pid.trim_start_matches('P').parse().unwrap_or(0)
}

/// Projects stored in the model, oldest first: id and `{input, level, roof}`.
pub fn stored(m: &Model) -> Result<Vec<(String, J)>, PlanError> {
    let mut out: Vec<(String, J)> = props(m, "%|spec")?
        .into_iter()
        .filter_map(|(k, v)| {
            let pid = k.strip_suffix("|spec")?.to_string();
            Some((pid, serde_json::from_str(&v).ok()?))
        })
        .collect();
    out.sort_by_key(|p| pid_number(&p.0));
    Ok(out)
}

/// The stored project and slot that wrote a model row, when a project did.
pub fn slot_of(m: &Model, table: &str, id: i64) -> Result<Option<(String, u32)>, PlanError> {
    for (pid, _) in stored(m)? {
        let s = load(m, &pid)?;
        if let Some((rel, _)) = s.recs.iter().find(|(_, r)| r.table == table && r.id == id) {
            return Ok(Some((pid, *rel)));
        }
    }
    Ok(None)
}

/// A project id not stored and not in `drafted`.
pub fn next_pid(m: &Model, drafted: &[String]) -> Result<String, PlanError> {
    let top = stored(m)?
        .iter()
        .map(|p| pid_number(&p.0))
        .chain(drafted.iter().map(|p| pid_number(p)))
        .max()
        .unwrap_or(0);
    Ok(format!("P{}", top + 1))
}

fn load(m: &Model, pid: &str) -> Result<Stored, PlanError> {
    let mut s = Stored::default();
    for (key, value) in props(m, &format!("{pid}|%"))? {
        let Some(rest) = key.strip_prefix(&format!("{pid}|")) else {
            continue;
        };
        if rest == "spec" {
            s.head = serde_json::from_str(&value).unwrap_or(J::Null);
        } else if let Some(b) = rest.strip_prefix('b').and_then(|b| b.parse().ok()) {
            for (rel, r) in value.lines().filter_map(Rec::parse) {
                s.recs.insert(rel, r);
            }
            s.blocks.insert(b, value);
        }
    }
    Ok(s)
}

/// FNV-1a, fed in pieces.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(ddc_model::FNV1A_SEED)
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.0 = ddc_model::fnv1a_more(self.0, bytes);
    }
}

fn fnv(s: &str) -> u64 {
    ddc_model::fnv1a(s.as_bytes())
}

/// The element's row, then its outline rows; prepared once per table for the session.
fn row_sql(table: &str) -> Option<(&'static str, Option<&'static str>)> {
    Some(match table {
        "wall" => ("SELECT * FROM wall WHERE id=?1", None),
        "opening" => ("SELECT * FROM opening WHERE id=?1", None),
        "room" => ("SELECT * FROM room WHERE id=?1", None),
        "stair" => ("SELECT * FROM stair WHERE id=?1", None),
        "grid" => ("SELECT * FROM grid WHERE id=?1", None),
        "col" => ("SELECT * FROM col WHERE id=?1", None),
        "beam" => ("SELECT * FROM beam WHERE id=?1", None),
        "comp" => ("SELECT * FROM comp WHERE id=?1", None),
        "curtain_wall" => ("SELECT * FROM curtain_wall WHERE id=?1", None),
        "railing" => (
            "SELECT * FROM railing WHERE id=?1",
            Some("SELECT * FROM railing_vertex WHERE railing=?1 ORDER BY idx"),
        ),
        "mep_run" => ("SELECT * FROM mep_run WHERE id=?1", None),
        "strip_footing" => ("SELECT * FROM strip_footing WHERE id=?1", None),
        "elec_circuit" => ("SELECT * FROM elec_circuit WHERE id=?1", None),
        "slab" => (
            "SELECT * FROM slab WHERE id=?1",
            Some("SELECT * FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx"),
        ),
        "shaft" => (
            "SELECT * FROM shaft WHERE id=?1",
            Some("SELECT * FROM shaft_vertex WHERE shaft=?1 ORDER BY idx"),
        ),
        "roof" => (
            "SELECT * FROM roof WHERE id=?1",
            Some("SELECT * FROM roof_vertex WHERE roof=?1 ORDER BY idx"),
        ),
        _ => return None,
    })
}

fn feed_rows(m: &Model, sql: &str, id: i64, h: &mut Fnv) -> Result<bool, PlanError> {
    use rusqlite::types::ValueRef;
    let mut st = m.sql().prepare_cached(sql)?;
    let n = st.column_count();
    let mut rows = st.query([id])?;
    let mut any = false;
    while let Some(r) = rows.next()? {
        any = true;
        for i in 0..n {
            match r.get_ref(i)? {
                ValueRef::Null => h.feed(b"n"),
                ValueRef::Integer(v) => h.feed(&v.to_le_bytes()),
                ValueRef::Real(v) => h.feed(&v.to_bits().to_le_bytes()),
                ValueRef::Text(t) | ValueRef::Blob(t) => h.feed(t),
            }
            h.feed(b";");
        }
        h.feed(b"\n");
    }
    Ok(any)
}

/// Hash of the element's row and outline, straight from the rows; `None` when it is gone.
pub(super) fn row_hash(m: &Model, table: &str, id: i64) -> Result<Option<u64>, PlanError> {
    let (main, child) = row_sql(table).ok_or_else(|| format!("no row hash for {table}"))?;
    let mut h = Fnv::new();
    if !feed_rows(m, main, id, &mut h)? {
        return Ok(None);
    }
    if let Some(sql) = child {
        feed_rows(m, sql, id, &mut h)?;
    }
    Ok(Some(h.0))
}

// ---- types and levels ----

/// The type of build-up `a`, found by its name (build-up and total thickness) or made with its
/// layers; a material the model has no row for gets one with the catalogue's physics, so the
/// export carries its conductivity and density. A row the person edited is left as it is.
fn type_id(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    a: &Assembly,
) -> Result<i64, PlanError> {
    let wall = a.use_.is_wall();
    let table = if wall { "wall_type" } else { "slab_type" };
    let name = a.type_name();
    let found = cx.ed.model.sql().query_row(
        &format!("SELECT id FROM {table} WHERE name=?1"),
        [&name],
        |r| r.get::<_, i64>(0),
    );
    match found {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => return Err(e.into()),
    }
    let mut mats: Vec<Edit> = vec![];
    for l in &a.layers {
        let m = &l.material;
        let fresh = cx.ed.model.material(&m.name)?.id == 0;
        if fresh && !mats.iter().any(|e| matches!(e, Edit::Material { name, .. } if *name == m.name)) {
            let mut spec = ddc_model::material_defaults(&m.name);
            spec.density = Some(m.density);
            spec.conductivity = Some(m.lambda);
            mats.push(Edit::Material { name: m.name.clone(), spec });
        }
    }
    if !mats.is_empty() {
        push(cx, out, ModelStep::Edits(mats))?;
    }
    let layers: Vec<LayerSpec> = a
        .layers
        .iter()
        .map(|l| LayerSpec { material: l.material.name.clone(), thickness: l.thickness() })
        .collect();
    let elem = if wall {
        NewResolved::WallType { name, layers }
    } else {
        NewResolved::SlabType { name, layers }
    };
    new_type(cx, out, table, elem)
}

fn elevation_of(cx: &CompileCx<'_>, level: i64) -> Result<f64, PlanError> {
    let sql = "SELECT elevation FROM level WHERE id=?1";
    Ok(cx.ed.model.sql().query_row(sql, [level], |r| r.get(0))?)
}

/// The level at this elevation, or a new one under a free name, added to `own`: the levels
/// the project made, which it renames and deletes as storeys come and go.
fn level_at(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    elevation: f64,
    name: &str,
    own: &mut BTreeSet<i64>,
) -> Result<i64, PlanError> {
    let found = cx.ed.model.sql().query_row(
        "SELECT id FROM level WHERE abs(elevation-?1)<0.001 ORDER BY id LIMIT 1",
        [elevation],
        |r| r.get::<_, i64>(0),
    );
    match found {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => return Err(e.into()),
    }
    let free = ddc_model::free_name(name, |k| format!("{name} ({k})"), |c| level_taken(cx, c))?;
    let id = new_level(cx, out, &free, elevation)?;
    own.insert(id);
    Ok(id)
}

/// After the elements moved: own levels no storey uses go (a level that holds elements
/// the project did not make stays and is named on the card), the rest take their names.
fn tidy_levels(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    own: &mut BTreeSet<i64>,
    wanted: &[(i64, String)],
) -> Result<(), PlanError> {
    let exists = |cx: &CompileCx<'_>, id: i64| -> Result<bool, PlanError> {
        let sql = "SELECT EXISTS(SELECT 1 FROM level WHERE id=?1)";
        Ok(cx.ed.model.sql().query_row(sql, [id], |r| r.get(0))?)
    };
    let unused: Vec<i64> = own
        .iter()
        .copied()
        .filter(|id| !wanted.iter().any(|w| w.0 == *id))
        .collect();
    for id in unused {
        if !exists(cx, id)? {
            own.remove(&id);
            continue;
        }
        if cx.ed.model.level_contents(id)?.is_empty() {
            push(cx, out, ModelStep::Delete { table: "level", id })?;
            cx.declared.insert(("level".to_string(), id));
            own.remove(&id);
        } else {
            let why = "level kept: it holds elements the project did not make";
            skip(cx, "level", id, why);
        }
    }
    for (id, name) in wanted {
        let sql = "SELECT name, elevation FROM level WHERE id=?1";
        let (now, elevation): (String, f64) = cx
            .ed
            .model
            .sql()
            .query_row(sql, [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        // a level the project reuses is renamed only while it carries a generic default name
        if !own.contains(id) && !generic_level_name(&now) {
            continue;
        }
        if now != *name && !level_taken(cx, name)? {
            let edit = Edit::Level {
                id: *id,
                name: name.clone(),
                elevation,
            };
            push(cx, out, ModelStep::Edits(vec![edit]))?;
            cx.declared.insert(("level".to_string(), *id));
        }
    }
    Ok(())
}

/// "Level 3" (or its Russian equivalents for "Level"/"Floor"): names a new model or a new level gets by default.
pub(crate) fn generic_level_name(s: &str) -> bool {
    let s = s.trim();
    let Some((word, num)) = s.rsplit_once(' ') else {
        return false;
    };
    let words = [
        "level",
        "\u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}",
        "\u{44d}\u{442}\u{430}\u{436}",
    ];
    !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
        && words.contains(&word.trim().to_lowercase().as_str())
}

fn ref_id(r: &Option<ElRef>) -> Result<i64, PlanError> {
    match r {
        Some(ElRef {
            id: IdOrNew::Id(i), ..
        }) => Ok(*i),
        _ => Err("a project type is not resolved".into()),
    }
}

// ---- what the builder asks for, slot by slot ----

/// One element the builder asks for; `rel` is its slot relative to the item's first.
struct Want {
    rel: u32,
    table: &'static str,
    elem: NewElem,
    role: Option<WallRole>,
    /// Slot of the host wall of an opening.
    wall_rel: Option<u32>,
}

struct Sync {
    slot0: u32,
    prev: Stored,
    recs: BTreeMap<u32, Rec>,
    /// Slots created or changed now: their rows are hashed at the end.
    fresh: BTreeSet<u32>,
    roles: BTreeMap<u32, WallRole>,
    /// Type, mark, swing and ratings of every opening slot the builder asks for.
    opens: BTreeMap<u32, ddc_gen::GOpeningType>,
    /// Fire resistance in minutes of the rated walls, by slot.
    fires: BTreeMap<u32, u32>,
    /// Fire resistance in minutes of the doors in rated walls, by slot.
    door_fires: BTreeMap<u32, u32>,
}

fn skip(cx: &mut CompileCx<'_>, table: &str, id: i64, why: &str) {
    cx.skips.push(Skip {
        cat: table.to_string(),
        id,
        why: why.to_string(),
    });
}

fn delete(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, r: &Rec) -> Result<(), PlanError> {
    let id = r.id;
    let step = match r.table.as_str() {
        "wall" => ModelStep::Edits(vec![Edit::Delete { table: "wall", id }]),
        "opening" => ModelStep::Edits(vec![Edit::Delete {
            table: "opening",
            id,
        }]),
        "slab" => ModelStep::Edits(vec![Edit::Delete { table: "slab", id }]),
        "room" => ModelStep::Edits(vec![Edit::DeleteRoom(id)]),
        "stair" => ModelStep::Delete { table: "stair", id },
        "shaft" => ModelStep::Delete { table: "shaft", id },
        "roof" => ModelStep::Delete { table: "roof", id },
        "grid" => ModelStep::Delete { table: "grid", id },
        "col" => ModelStep::Edits(vec![Edit::DeleteColumn(id)]),
        "beam" => ModelStep::Edits(vec![Edit::DeleteBeam(id)]),
        "comp" => ModelStep::Delete { table: "comp", id },
        "curtain_wall" => ModelStep::Edits(vec![Edit::DeleteCurtain(id)]),
        "railing" => ModelStep::Delete { table: "railing", id },
        "mep_run" => ModelStep::Delete {
            table: "mep_run",
            id,
        },
        "elec_circuit" => ModelStep::Delete {
            table: "elec_circuit",
            id,
        },
        t => return Err(format!("a project does not delete a {t}").into()),
    };
    push(cx, out, step)?;
    cx.declared.insert((r.table.clone(), id));
    Ok(())
}

/// Changes a stored element to what the builder asks now, in place: the id stays.
fn update(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &Sync,
    w: &Want,
    id: i64,
) -> Result<i64, PlanError> {
    let edits = match &w.elem {
        NewElem::Wall {
            level,
            a,
            b,
            bulge,
            base_offset,
            height,
            wtype,
        } => vec![Edit::Wall {
            id,
            spec: WallSpec {
                a: *a,
                b: *b,
                level: *level,
                base_offset: *base_offset,
                height: *height,
                wall_type: ref_id(wtype)?,
                offset0: None,
            },
            bulge: *bulge,
        }],
        NewElem::Opening {
            along,
            width,
            height,
            sill,
            kind,
            ..
        } => {
            let host = w
                .wall_rel
                .and_then(|r| cx.slots.get(&(sy.slot0 + r)).copied());
            vec![Edit::Opening {
                id,
                wall: host.ok_or("an opening lost its wall")?,
                spec: OpeningSpec {
                    along: *along,
                    width: *width,
                    sill: *sill,
                    height: *height,
                    kind: kind.clone(),
                },
            }]
        }
        NewElem::Room {
            level,
            name,
            number,
            point,
            height,
        } => vec![Edit::Room {
            id,
            spec: RoomSpec {
                level: *level,
                name: name.clone(),
                number: number.clone(),
                point: *point,
                height: *height,
            },
        }],
        NewElem::Slab {
            level,
            stype,
            offset,
            outer,
            holes,
        } => vec![
            Edit::Slab {
                id,
                level: *level,
                slab_type: ref_id(stype)?,
                offset: *offset,
            },
            Edit::SlabBoundary {
                id,
                outer: outer.clone(),
                holes: holes.clone(),
            },
        ],
        NewElem::Beam { level, mtype, a, b } => vec![Edit::Beam {
            id,
            spec: ddc_model::BeamSpec {
                level: *level,
                member_type: ref_id(mtype)?,
                a: *a,
                b: *b,
                bulge: 0.0,
                yjust: ddc_model::Just::Center,
                zjust: ddc_model::Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: ddc_model::BeamJoin::Auto,
                join_end: ddc_model::BeamJoin::Auto,
            },
        }],
        // stairs, shafts, grids and the roof: resolved as for a new one, updated in place
        _ => {
            let ModelStep::Add { elem, .. } = resolve_group(cx, std::slice::from_ref(&w.elem))?
            else {
                return Err(format!("a {} is not updated in place", w.table).into());
            };
            push(cx, out, ModelStep::Update { id, elem })?;
            cx.declared.insert((w.table.to_string(), id));
            return Ok(id);
        }
    };
    push(cx, out, ModelStep::Edits(edits))?;
    cx.declared.insert((w.table.to_string(), id));
    Ok(id)
}

fn flush(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    run: &mut Vec<Want>,
) -> Result<(), PlanError> {
    let Some(first) = run.first() else {
        return Ok(());
    };
    let elems: Vec<NewElem> = run.iter().map(|w| w.elem.clone()).collect();
    compile_create(cx, sy.slot0 + first.rel, &elems, out)?;
    for w in run.drain(..) {
        let id = cx.slots.get(&(sy.slot0 + w.rel)).copied().unwrap_or(0);
        let intent = fnv(&format!("{:?}", w.elem));
        let rec = Rec {
            table: w.table.to_string(),
            id,
            intent,
            row: 0,
        };
        sy.recs.insert(w.rel, rec);
        sy.fresh.insert(w.rel);
    }
    Ok(())
}

/// One category of one storey: new slots are created in runs, stored ones kept or changed.
fn sync_group(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    wants: Vec<Want>,
) -> Result<(), PlanError> {
    let mut run: Vec<Want> = vec![];
    for w in wants {
        if let Some(role) = w.role {
            sy.roles.insert(w.rel, role);
        }
        let host_gone = w
            .wall_rel
            .is_some_and(|r| !cx.slots.contains_key(&(sy.slot0 + r)));
        let prev = sy.prev.recs.get(&w.rel).cloned();
        let joins = run.last().is_some_and(|l| l.rel + 1 == w.rel);
        if prev.is_some() || host_gone || !joins {
            flush(cx, out, sy, &mut run)?;
        }
        if host_gone {
            if let Some(r) = prev {
                sy.recs.insert(w.rel, r);
            }
            continue;
        }
        let Some(r) = prev else {
            run.push(w);
            continue;
        };
        let slot = sy.slot0 + w.rel;
        let intent = fnv(&format!("{:?}", w.elem));
        let Some(now) = row_hash(&cx.ed.model, &r.table, r.id)? else {
            let why = "deleted by hand after Apply: the revision does not bring it back";
            skip(cx, &r.table, r.id, why);
            sy.recs.insert(w.rel, r);
            continue;
        };
        cx.slots.insert(slot, r.id);
        if now != r.row {
            if intent != r.intent {
                let why = "changed by hand after Apply: kept as it is, the revision would \
                           change it";
                skip(cx, &r.table, r.id, why);
            }
            sy.recs.insert(w.rel, r);
        } else if intent == r.intent {
            sy.recs.insert(w.rel, r);
        } else {
            let id = update(cx, out, sy, &w, r.id)?;
            cx.slots.insert(slot, id);
            let rec = Rec {
                table: w.table.to_string(),
                id,
                intent,
                row: 0,
            };
            sy.recs.insert(w.rel, rec);
            sy.fresh.insert(w.rel);
        }
    }
    flush(cx, out, sy, &mut run)
}

/// Stored slots the builder no longer asks for: deleted, unless changed by hand since.
fn delete_gone(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    asked: &BTreeSet<u32>,
) -> Result<(), PlanError> {
    let mut gone: Vec<(u32, Rec)> = sy
        .prev
        .recs
        .iter()
        .filter(|(rel, _)| !asked.contains(rel))
        .map(|(rel, r)| (*rel, r.clone()))
        .collect();
    let order = |t: &str| DELETE_ORDER.iter().position(|d| *d == t).unwrap_or(99);
    gone.sort_by_key(|g| order(&g.1.table));
    for (rel, r) in gone {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                let why = "changed by hand after Apply: kept although the revision removes it";
                skip(cx, &r.table, r.id, why);
                sy.recs.insert(rel, r);
            }
            Some(_) => delete(cx, out, &r)?,
        }
    }
    Ok(())
}

/// Wall type by role, the floor type, and the component type of each fixture of a storey.
type StoreyTypes<'a> = (&'a dyn Fn(WallUse, WallRole, Option<WallFire>) -> Option<ElRef>, i64, Vec<i64>, Vec<i64>);

/// The library family of each fixture loaded once, and the type id each fixture names.
fn fixture_types(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    known: &mut BTreeMap<(&'static str, &'static str), i64>,
    l: &ddc_gen::GLayout,
) -> Result<Vec<i64>, PlanError> {
    let mut ids = Vec::with_capacity(l.fixtures.len());
    for f in &l.fixtures {
        if let Some(id) = known.get(&(f.family, f.ty)) {
            ids.push(*id);
            continue;
        }
        let fam = match cx.ed.model.family_id(f.family)? {
            Some(id) => id,
            None => {
                let head = format!("family {}", f.family);
                let def = ddc_model::library_sources()
                    .into_iter()
                    .find(|s| s.lines().any(|x| x.trim() == head))
                    .ok_or_else(|| PlanError(format!("no library family {}", f.family)))?;
                let step = ModelStep::Family {
                    def: def.to_string(),
                    overwrite: false,
                };
                push(cx, out, step)?;
                cx.ed
                    .model
                    .family_id(f.family)?
                    .ok_or_else(|| PlanError(format!("family {} was not loaded", f.family)))?
            }
        };
        let sql = "SELECT id FROM comp_type WHERE family=?1 AND name=?2";
        let id: i64 = cx
            .ed
            .model
            .sql()
            .query_row(sql, rusqlite::params![fam, f.ty], |r| r.get(0))
            .map_err(|_| PlanError(format!("family {} has no type {}", f.family, f.ty)))?;
        known.insert((f.family, f.ty), id);
        ids.push(id);
    }
    Ok(ids)
}

/// The lower block of a split storey: the split, its level and the top it reaches.
#[derive(Clone, Copy)]
struct Lower<'a> {
    sp: &'a GSplit,
    level: i64,
    top: (i64, f64),
}

/// Moves an element of the lower half-level to its level; a slab parts on the cuts, keeps its
/// first face and gives the other faces back, each on its half-level. A corridor on a cut
/// stays on the upper level, whose room cut meets the walls of both half-levels.
fn lower(
    elem: &mut NewElem,
    lw: Lower<'_>,
    fixture_at: Option<(f64, f64)>,
    room: Option<&[(f64, f64)]>,
) -> Vec<NewElem> {
    let sp = lw.sp;
    if let NewElem::Slab { level, stype, offset, outer, holes } = elem {
        let Some((hi, lo)) = sp.pieces(outer, holes) else { return vec![] };
        let up = *level;
        let mut faces = hi.into_iter().map(|f| (up, f)).chain(lo.into_iter().map(|f| (lw.level, f)));
        let Some((l0, (o0, h0))) = faces.next() else { return vec![] };
        let rest = faces
            .map(|(l, (o, h))| NewElem::Slab { level: l, stype: stype.clone(), offset: *offset, outer: o, holes: h })
            .collect();
        (*level, *outer, *holes) = (l0, o0, h0);
        return rest;
    }
    let is_low = match &*elem {
        NewElem::Wall { a, b, .. } => sp.is_low(((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)),
        NewElem::Room { point, .. } => sp.is_low(*point) && !room.is_some_and(|r| sp.crosses(r)),
        NewElem::Column { at, .. } => sp.is_low(*at),
        NewElem::GenStair { origin, .. } => sp.is_low(*origin),
        NewElem::Shaft { ring, .. } => sp.ring_low(ring),
        NewElem::Comp { .. } => fixture_at.is_some_and(|q| sp.is_low(q)),
        _ => false,
    };
    if is_low {
        match elem {
            NewElem::Wall { level, .. } | NewElem::Room { level, .. } | NewElem::Comp { level, .. } => *level = lw.level,
            NewElem::Column { level, top_level, top_offset, .. } => {
                *level = lw.level;
                *top_level = Some(lw.top.0);
                *top_offset = lw.top.1;
            }
            NewElem::GenStair { level, top_level, top_offset, .. } | NewElem::Shaft { level, top_level, top_offset, .. } => {
                *level = lw.level;
                *top_level = lw.top.0;
                *top_offset = lw.top.1;
            }
            _ => {}
        }
    }
    vec![]
}

/// The builder's elements of one storey in their slots, by category in creation order:
/// the generator mapping makes them, the project picks types by role, numbers and grid names.
#[allow(clippy::too_many_arguments)]
fn storey_wants(
    s: &PStorey,
    bulges: &[f64],
    slot0: u32,
    blk: u32,
    level: i64,
    top: i64,
    parapet: f64,
    types: &StoreyTypes<'_>,
    low: Option<Lower<'_>>,
    grid_name: &mut dyn FnMut(u32, &str) -> String,
) -> Result<Vec<Vec<Want>>, PlanError> {
    let block = slots::block_base(blk);
    let l = &s.layout;
    let (wtype, floor_type) = (types.0, types.1);
    let uses = assembly::wall_uses(s);
    let b = Bound {
        slab_type: Some(floor_type),
        top: Some((top, 0.0)),
        fixture_types: types.2.clone(),
        member_types: types.3.clone(),
        ..Bound::default()
    };
    let fix0 = slots::fixture_base(blk);
    // groups in creation order: walls, openings, slab, rooms, stairs, shafts, grids, columns,
    // fixtures (after their host walls)
    let mut groups: [Vec<Want>; 10] = Default::default();
    let mut extra = 0u32;
    // openings name their wall by the plan slot, so the mapping counts from the item's first
    for mut elem in layout_items(l, level, slot0 + block + slots::WALLS, &b) {
        let (g, at, table) = match &elem {
            NewElem::Wall { .. } => (0, slots::WALLS, "wall"),
            NewElem::Opening { .. } => (1, slots::OPENINGS, "opening"),
            NewElem::Slab { .. } => (2, slots::SLAB, "slab"),
            NewElem::Room { .. } => (3, slots::ROOMS, "room"),
            NewElem::GenStair { .. } => (4, slots::STAIRS, "stair"),
            NewElem::Shaft { .. } => (5, slots::SHAFTS, "shaft"),
            NewElem::Grid { .. } => (6, slots::GRIDS, "grid"),
            NewElem::Column { .. } => (7, slots::COLUMNS, "col"),
            NewElem::Comp { .. } => (8, 0, "comp"),
            other => return Err(format!("no project slot for {other:?}").into()),
        };
        let i = groups[g].len();
        let rel = if g == 8 {
            fix0 + i as u32
        } else if g == 7 {
            slots::column(block / slots::PER_STOREY, i as u32)
        } else {
            block + at + i as u32
        };
        let (mut role, mut wall_rel) = (None, None);
        match &mut elem {
            NewElem::Wall { wtype: t, height, bulge, .. } => {
                role = Some(l.walls[i].role);
                *t = wtype(uses[i], l.walls[i].role, l.walls[i].fire);
                *bulge = bulges.get(i).copied().unwrap_or(l.walls[i].bulge);
                if l.walls[i].role == WallRole::Exterior {
                    *height += parapet;
                }
            }
            NewElem::Opening { .. } => {
                wall_rel = Some(block + slots::WALLS + l.openings[i].wall as u32);
            }
            NewElem::Comp { .. } => {
                wall_rel = l.fixtures[i].wall.map(|w| block + slots::WALLS + w as u32);
            }
            NewElem::Room { number, .. } => *number = s.rooms[i].number.clone(),
            NewElem::Grid { name, .. } => *name = grid_name(rel, &l.grids[i].name),
            _ => {}
        }
        let pieces = match low {
            Some(lw) => lower(
                &mut elem,
                lw,
                (g == 8).then(|| l.fixtures[i].at),
                (g == 3).then(|| l.rooms[i].polygon.as_slice()),
            ),
            None => vec![],
        };
        groups[g].push(Want {
            rel,
            table,
            elem,
            role,
            wall_rel,
        });
        // the slab's other faces, then the half-level flights, take the storey's split slots
        for piece in pieces {
            groups[g].push(Want {
                rel: slots::split_block(blk) + extra,
                table,
                elem: piece,
                role: None,
                wall_rel: None,
            });
            extra += 1;
        }
    }
    // curtain walls: after the fixtures, in the fixture block's tail
    for (i, c) in s.curtains.iter().enumerate() {
        groups[9].push(Want {
            rel: slots::curtain_base(blk) + i as u32,
            table: "curtain_wall",
            elem: NewElem::Curtain {
                level,
                name: c.name.clone(),
                a: c.a,
                b: c.b,
                base_offset: 0.0,
                height: c.height,
                spacing: c.spacing,
                mullion: c.mullion,
                spandrel: c.spandrel,
            },
            role: None,
            wall_rel: None,
        });
    }
    // guard rails: after the curtain walls, a stair's two after the stair they follow
    for (i, r) in s.railings.iter().enumerate() {
        let (path, stair, host) = match &r.on {
            RailOn::Stair { stair, side } => {
                let rel = block + slots::STAIRS + *stair as u32;
                let at = ElRef { cat: "stair".into(), id: IdOrNew::New(slot0 + rel) };
                (vec![], Some((at, *side)), Some(rel))
            }
            RailOn::Path(p) => (p.clone(), None, None),
        };
        groups[9].push(Want {
            rel: slots::railing_base(blk) + i as u32,
            table: "railing",
            elem: NewElem::Railing { level, path, stair, height: r.height },
            role: None,
            wall_rel: host,
        });
    }
    if let Some(lw) = low {
        let k = blk;
        let mine = lw.sp.flights.iter().filter(|f| f.0 == k).map(|f| &f.1);
        for (j, f) in mine.enumerate() {
            groups[4].push(Want {
                rel: slots::split_block(k) + extra + j as u32,
                table: "stair",
                elem: NewElem::GenStair {
                    level: lw.level,
                    top_level: level,
                    top_offset: 0.0,
                    origin: f.origin,
                    angle: f.angle,
                    shape: f.shape,
                    width: f.width,
                    max_riser: f.max_riser,
                    landing: f.landing,
                },
                role: None,
                wall_rel: None,
            });
        }
    }
    Ok(groups.into())
}

// ---- parameters and properties ----

fn write_finishes(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    spec: &ProjectSpec,
    rooms: &[i64],
) -> Result<(), PlanError> {
    let mut edits = vec![];
    let wants = [
        (FINISHES[0], &spec.floor_finish),
        (FINISHES[1], &spec.wall_finish),
        (FINISHES[2], &spec.ceiling_finish),
    ];
    for (name, value) in wants {
        // a parameter the project already has keeps its categories
        let bound = match cx.ed.model.param_by_name(name)? {
            Some(d) => d.spec.categories.iter().any(|c| c == "room"),
            None => {
                edits.push(Edit::NewParam(ParamSpec {
                    name: name.to_string(),
                    ptype: ParamType::Text,
                    per_type: false,
                    unit: String::new(),
                    group: String::new(),
                    guid: None,
                    categories: vec!["room".to_string()],
                }));
                true
            }
        };
        if !bound {
            continue;
        }
        for id in rooms {
            edits.push(Edit::ParamValue {
                param: name.to_string(),
                kind: "room".to_string(),
                id: *id,
                value: Some(PVal::Text(value.to_string())),
            });
        }
    }
    if !edits.is_empty() {
        push(cx, out, ModelStep::Edits(edits))?;
    }
    Ok(())
}

fn prop(kind: &str, id: i64, key: &str, value: Option<Value>) -> PropSet {
    PropSet {
        kind: kind.to_string(),
        id,
        key: key.to_string(),
        value: value.map(|v| (v, String::new())),
    }
}

/// Load bearing and fire rating as the IFC common property sets read them: every wall but
/// the partitions carries the structure and the fire separation, and so does every slab.
fn structure_props(sy: &Sync, fire: &str, slab_fire: Option<u32>, ids: &[(u32, &Rec)]) -> Vec<PropSet> {
    let mut out = vec![];
    for (rel, r) in ids {
        match r.table.as_str() {
            "wall" => {
                let bearing = sy.roles.get(rel).is_some_and(|x| *x != WallRole::Partition);
                let b = Value::Integer(i64::from(bearing));
                out.push(prop("wall", r.id, WALL_BEARING, Some(b)));
                // a rated wall states its own resistance, the rest the project's
                let f = match sy.fires.get(rel) {
                    Some(min) => Some(Value::Text(format!("EI{min}"))),
                    None => bearing.then(|| Value::Text(fire.to_string())),
                };
                out.push(prop("wall", r.id, WALL_FIRE, f));
            }
            "opening" => {
                if let Some(min) = sy.door_fires.get(rel) {
                    out.push(prop("opening", r.id, DOOR_FIRE, Some(Value::Text(format!("EI{min}")))));
                }
            }
            "slab" => {
                out.push(prop("slab", r.id, SLAB_BEARING, Some(Value::Integer(1))));
                let f = Value::Text(slab_fire.map_or_else(|| fire.to_string(), |m| format!("REI{m}")));
                out.push(prop("slab", r.id, SLAB_FIRE, Some(f)));
            }
            _ => {}
        }
    }
    out
}

/// Mark, ratings and flags of every typed opening as the IFC door and window property sets read
/// them: the mark is the reference, the fire class `EI <min>`, the sound class `Rw <dB>`.
fn opening_props(sy: &Sync, ids: &[(u32, &Rec)]) -> Vec<PropSet> {
    let mut out = vec![];
    for (rel, r) in ids.iter().filter(|(_, r)| r.table == "opening") {
        let Some(t) = sy.opens.get(rel) else { continue };
        let set = if t.function == "window" { WINDOW_PSET } else { DOOR_PSET };
        let key = |name: &str| format!("{set}.{name}");
        let text = |v: String| Some(Value::Text(v));
        out.push(prop("opening", r.id, &key("Reference"), text(t.mark.clone())));
        out.push(prop("opening", r.id, &key("AcousticRating"), (t.rw > 0).then(|| Value::Text(format!("Rw {} dB", t.rw)))));
        if t.u > 0.0 {
            out.push(prop("opening", r.id, &key("ThermalTransmittance"), Some(Value::Real(t.u))));
        }
        if t.function != "window" {
            let fire = (t.fire > 0).then(|| Value::Text(format!("EI {}", t.fire)));
            out.push(prop("opening", r.id, &key("FireRating"), fire));
            let flag = |b: bool| Some(Value::Integer(i64::from(b)));
            out.push(prop("opening", r.id, &key("FireExit"), flag(t.exit)));
            out.push(prop("opening", r.id, &key("HandicapAccessible"), flag(t.accessible)));
            out.push(prop("opening", r.id, &key("SelfClosing"), flag(t.fire > 0)));
        }
    }
    out
}

/// Generator key of every slot (`rel`, without the project's slot offset).
fn key_map(p: &GProject, building: u32) -> BTreeMap<u32, String> {
    fn put(m: &mut BTreeMap<u32, String>, at: u32, list: &[String]) {
        for (i, k) in list.iter().enumerate() {
            m.insert(at + i as u32, k.clone());
        }
    }
    let pk = project_keys(p, building);
    let mut m = BTreeMap::new();
    let nb = p.basements.len();
    for (k, s) in pk.storeys.iter().enumerate() {
        let blk = slots::storey_block(k, nb);
        let block = slots::block_base(blk);
        put(&mut m, block + slots::WALLS, &s.walls);
        put(&mut m, block + slots::OPENINGS, &s.openings);
        put(&mut m, block + slots::ROOMS, &s.rooms);
        put(&mut m, block + slots::STAIRS, &s.stairs);
        put(&mut m, block + slots::SHAFTS, &s.shafts);
        put(&mut m, block + slots::GRIDS, &s.grids);
        for (i, c) in s.columns.iter().enumerate() {
            m.insert(slots::column(blk, i as u32), c.clone());
        }
        put(&mut m, slots::fixture_base(blk), &s.fixtures);
        if let Some(sl) = &s.slab {
            m.insert(block + slots::SLAB, sl.clone());
        }
    }
    m.insert(ROOF_REL, pk.roof);
    m
}

/// The key of every element a slot holds, written where the element carries another or none.
/// An element changed by hand keeps its identity, so it is written too unless it already has it.
fn key_props(m: &Model, sy: &Sync, keys: &BTreeMap<u32, String>) -> Vec<PropSet> {
    let mut out = vec![];
    for (rel, r) in &sy.recs {
        let Some(key) = keys.get(rel) else { continue };
        let now: Option<String> = m
            .sql()
            .query_row(
                "SELECT CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                rusqlite::params![r.table, r.id, ddc_model::KEY_PROP],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        if now.as_deref() != Some(key.as_str()) {
            out.push(prop(&r.table, r.id, ddc_model::KEY_PROP, Some(Value::Text(key.clone()))));
        }
    }
    out
}

/// Records by storey block; only blocks whose text changed are written.
fn recipe_props(sy: &Sync, pid: &str, head: &J) -> Vec<PropSet> {
    let text = |v: String| Some(Value::Text(v));
    let mut out = vec![prop(
        RECIPE,
        0,
        &format!("{pid}|spec"),
        text(head.to_string()),
    )];
    let mut blocks: BTreeMap<u32, String> = BTreeMap::new();
    for (rel, r) in &sy.recs {
        let b = blocks.entry(rel / slots::PER_STOREY).or_default();
        b.push_str(&r.line(*rel));
        b.push('\n');
    }
    for (b, t) in &blocks {
        if sy.prev.blocks.get(b) != Some(t) {
            out.push(prop(RECIPE, 0, &format!("{pid}|b{b}"), text(t.clone())));
        }
    }
    for b in sy.prev.blocks.keys().filter(|b| !blocks.contains_key(b)) {
        out.push(prop(RECIPE, 0, &format!("{pid}|b{b}"), None));
    }
    out
}

// ---- compile ----

const ROOF_REL: u32 = slots::ROOF;

/// The roofs in their slots, each with whether it stands on a lower half-level: the main
/// roof, then the wings'; on split levels each roof parts on the cuts, its first piece keeps
/// its slot and the others follow in the split roof slots.
fn split_roofs(p: &GProject) -> Vec<(u32, ddc_gen::project::PRoof, bool)> {
    let roofs = std::iter::once(&p.roof).chain(&p.extra_roofs).enumerate();
    let Some(sp) = &p.site.split else {
        return roofs.map(|(i, r)| (ROOF_REL + i as u32, r.clone(), false)).collect();
    };
    let mut out = vec![];
    let mut extra = 0u32;
    for (i, r) in roofs {
        let (hi, lo) = sp.roofs(r);
        let pieces = hi.into_iter().map(|x| (x, false)).chain(lo.into_iter().map(|x| (x, true)));
        for (j, (x, low)) in pieces.enumerate() {
            let rel = if j == 0 {
                ROOF_REL + i as u32
            } else {
                extra += 1;
                slots::SPLIT_ROOF + extra - 1
            };
            out.push((rel, x, low));
        }
    }
    out
}

/// The stair heads: the builder refuses, with a why, any past the slots the roof block holds.
fn heads(p: &GProject) -> &[ddc_gen::project::GStairHead] {
    &p.fire.stair_heads
}

/// Stair heads on the roof level: four walls rated as the stair, the exit door in the edge the
/// flight arrives at, and the cover; the walls stand on the roof level through the deck. Walls,
/// doors and covers are groups of their own, so a door finds its wall made.
fn head_wants(p: &GProject, slot0: u32, roof_level: i64, deck: f64, wall: &dyn Fn(u32) -> Option<ElRef>, cover: i64) -> [Vec<Want>; 3] {
    let mut out: [Vec<Want>; 3] = Default::default();
    for (h, hd) in heads(p).iter().enumerate() {
        let base = slots::HEADS + h as u32 * slots::HEAD_SLOTS;
        let n = hd.ring.len();
        for j in 0..n {
            out[0].push(Want {
                rel: base + j as u32,
                table: "wall",
                elem: NewElem::Wall {
                    level: roof_level,
                    a: hd.ring[j],
                    b: hd.ring[(j + 1) % n],
                    bulge: 0.0,
                    base_offset: 0.0,
                    height: deck + hd.height,
                    wtype: wall(hd.fire),
                },
                role: Some(WallRole::Exterior),
                wall_rel: None,
            });
        }
        let (edge, along, width, height) = hd.door;
        let host = base + edge as u32;
        out[1].push(Want {
            rel: base + 4,
            table: "opening",
            elem: NewElem::Opening {
                wall: ElRef::new("wall", slot0 + host),
                along,
                width,
                height,
                sill: deck,
                kind: "door".to_string(),
            },
            role: None,
            wall_rel: Some(host),
        });
        out[2].push(Want {
            rel: base + 5,
            table: "slab",
            elem: NewElem::Slab {
                level: roof_level,
                stype: Some(type_ref("slab_type", cover)),
                offset: deck + hd.height,
                outer: hd.ring.clone(),
                holes: vec![],
            },
            role: None,
            wall_rel: None,
        });
    }
    out
}

/// Every slot the project asks for, before anything is deleted or named.
fn asked_slots(p: &GProject, mech: Option<&project::mech::model::Content>) -> BTreeSet<u32> {
    let mut asked: BTreeSet<u32> = (0..=p.extra_roofs.len() as u32).map(|i| ROOF_REL + i).collect();
    for (k, s) in p.blocks() {
        let block = slots::block_base(k);
        let l = &s.layout;
        let ranges = [
            (slots::WALLS, l.walls.len()),
            (slots::OPENINGS, l.openings.len()),
            (slots::SLAB, 1),
            (slots::ROOMS, l.rooms.len()),
            (slots::STAIRS, l.stairs.len()),
            (slots::SHAFTS, l.shafts.len()),
            (slots::GRIDS, l.grids.len()),
        ];
        for (at, n) in ranges {
            asked.extend((0..n as u32).map(|i| block + at + i));
        }
        asked.extend((0..l.columns.len() as u32).map(|i| slots::column(k, i)));
        let fix0 = slots::fixture_base(k);
        asked.extend((0..l.fixtures.len() as u32).map(|i| fix0 + i));
        asked.extend((0..s.curtains.len() as u32).map(|i| slots::curtain_base(k) + i));
        asked.extend((0..s.railings.len() as u32).map(|i| slots::railing_base(k) + i));
        if let Some(sp) = &p.site.split {
            let faces = match sp.pieces(&l.slab, &l.slab_holes) {
                Some((hi, lo)) if l.slab.len() >= 3 => (hi.len() + lo.len()).saturating_sub(1) as u32,
                _ => 0,
            };
            let n = faces + sp.flights.iter().filter(|f| f.0 == k).count() as u32;
            asked.extend((0..n).map(|j| slots::split_block(k) + j));
        }
    }
    asked.extend(split_roofs(p).into_iter().map(|r| r.0));
    for h in 0..heads(p).len() as u32 {
        asked.extend((0..6).map(|j| slots::HEADS + h * slots::HEAD_SLOTS + j));
    }
    asked.extend((0..p.hvac.plant.len() as u32).map(|i| slots::PLANT + i));
    let ducts = mech.map_or(p.hvac.ducts.len(), |c| c.ducts.len());
    asked.extend((0..ducts as u32).map(|i| slots::DUCTS + i));
    if let Some(c) = mech {
        asked.extend((0..mech_units(c).count() as u32).map(|i| slots::MECH + i));
        asked.extend((0..c.pipes.len() as u32).map(|i| slots::MECH_PIPES + i));
    }
    let sv = &p.services;
    asked.extend((0..sv.water.len() as u32).map(|i| slots::WATER + i));
    asked.extend((0..sv.elec.len() as u32).map(|i| slots::ELEC + i));
    let standpipe_runs: usize = p.fire.standpipes.iter().map(|s| s.runs.len()).sum();
    asked.extend((0..p.fire.runs.len() as u32).map(|i| slots::SPRINKLER + i));
    asked.extend((0..standpipe_runs as u32).map(|i| slots::STANDPIPE + i));
    asked.extend((0..sv.circuits.len() as u32).map(|i| slots::CIRCUITS + i));
    asked
}

/// Air handling units on the roof level, kept slot by slot like the storeys' fixtures.
fn plant_wants(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    known: &mut BTreeMap<(&'static str, &'static str), i64>,
    p: &GProject,
    roof_level: i64,
) -> Result<Vec<Want>, PlanError> {
    let l = ddc_gen::GLayout {
        fixtures: p.hvac.plant.clone(),
        ..Default::default()
    };
    let ids = fixture_types(cx, out, known, &l)?;
    Ok(p.hvac
        .plant
        .iter()
        .zip(ids)
        .enumerate()
        .map(|(i, (f, id))| Want {
            rel: slots::PLANT + i as u32,
            table: "comp",
            elem: NewElem::Comp {
                level: roof_level,
                ctype: type_ref("comp_type", id),
                at: (f.at.0, f.at.1, f.z),
                rotation: f.rotation,
                host: None,
                scale: f.scale,
                min_clear: f.min_clear,
            },
            role: None,
            wall_rel: None,
        })
        .collect())
}

/// Units of the mechanical design in their own slot block: all but the air handling unit,
/// which takes the plant slot of the ventilation.
fn mech_units(c: &project::mech::model::Content) -> impl Iterator<Item = &project::mech::model::ModelUnit> {
    c.units.iter().skip(ahus(c))
}

/// The air handling units lead the units of the design, one per plant slot.
fn ahus(c: &project::mech::model::Content) -> usize {
    c.units.iter().take_while(|u| u.kind == project::mech::equip::Kind::Ahu).count()
}

/// The component of unit `i` of the design, once placed.
fn mech_comp(cx: &CompileCx<'_>, sy: &Sync, c: &project::mech::model::Content, i: usize) -> Option<i64> {
    let k = ahus(c);
    let rel = if i < k { slots::PLANT + i as u32 } else { slots::MECH + (i - k) as u32 };
    cx.slots.get(&(sy.slot0 + rel)).copied()
}

/// The generated family of each kind of unit loaded once, and a sized type per unit: found by
/// name (the name carries a hash of the values) or added.
fn mech_types(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    c: &project::mech::model::Content,
) -> Result<Vec<i64>, PlanError> {
    let mut fams: BTreeMap<&str, i64> = BTreeMap::new();
    for f in &c.families {
        let id = match cx.ed.model.family_id(&f.name)? {
            Some(id) => id,
            None => {
                let step = ModelStep::Family {
                    def: f.def.clone(),
                    overwrite: false,
                };
                push(cx, out, step)?;
                cx.ed
                    .model
                    .family_id(&f.name)?
                    .ok_or_else(|| PlanError(format!("family {} was not loaded", f.name)))?
            }
        };
        fams.insert(&f.name, id);
    }
    let mut ids = Vec::with_capacity(c.units.len());
    for u in &c.units {
        let fam = *fams
            .get(u.family.as_str())
            .ok_or_else(|| PlanError(format!("unit {} names no family", u.tag)))?;
        if let Some(id) = cx.ed.model.comp_type_id(fam, &u.ty)? {
            ids.push(id);
            continue;
        }
        let values = u.values.iter().map(|(k, v)| (k.clone(), ddc_model::ParamValue::Num(*v))).collect();
        let elem = NewResolved::CompType {
            family: fam,
            name: u.ty.clone(),
            values,
        };
        let made = push(
            cx,
            out,
            ModelStep::Add {
                elem: Box::new(elem),
                expect: 0,
            },
        )?;
        let id = *made
            .first()
            .ok_or_else(|| PlanError(format!("the type of unit {} was not added", u.tag)))?;
        cx.declared.insert(("comp_type".to_string(), id));
        ids.push(id);
    }
    Ok(ids)
}

/// The units of the design but the air handling unit, slot by slot on their storeys.
fn mech_wants(
    p: &GProject,
    c: &project::mech::model::Content,
    types: &[i64],
    levels: &[i64],
    roof_level: i64,
) -> Vec<Want> {
    let skip = c.units.len() - mech_units(c).count();
    mech_units(c)
        .enumerate()
        .map(|(j, u)| {
            let (level, floor) = match p.storeys.get(u.storey) {
                Some(s) => (levels[u.storey], s.elevation),
                None => (roof_level, p.roof.elevation),
            };
            Want {
                rel: slots::MECH + j as u32,
                table: "comp",
                elem: NewElem::Comp {
                    level,
                    ctype: type_ref("comp_type", types[skip + j]),
                    at: (u.at.0, u.at.1, u.at.2 - floor),
                    rotation: u.rot,
                    host: None,
                    scale: 1.0,
                    min_clear: 0.0,
                },
                role: None,
                wall_rel: None,
            }
        })
        .collect()
}

/// The air ducts as one network build after every terminal and unit exists: systems by
/// name, runs and connector links. With the mechanical design the runs take its sizes and are
/// cut at the VAV boxes, whose ports join them. Kept as they are while nothing they depend on
/// changed; otherwise the stored runs are replaced, except runs changed by hand since.
fn sync_ducts(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    p: &GProject,
    levels: &[i64],
    roof_level: i64,
    mech: Option<&project::mech::model::Content>,
) -> Result<(), PlanError> {
    use ddc_gen::project::AirSys;
    use ddc_model::{MepBuild, MepDomain, MepSection, MepShape, RunSpec, SysRef, SystemKind};
    let h = &p.hvac;
    let n = p.storeys.len();
    let comp = |cx: &CompileCx<'_>, storey: usize, i: usize| {
        let rel = if storey == n {
            slots::PLANT + i as u32
        } else {
            slots::fixture_base(slots::storey_block(storey, p.basements.len())) + i as u32
        };
        cx.slots.get(&(sy.slot0 + rel)).copied()
    };
    let mut links: Vec<(i64, String, SysRef)> = h
        .links
        .iter()
        .filter_map(|l| {
            let s = SysRef::New(usize::from(l.net) * 2 + usize::from(l.sys == AirSys::Extract));
            comp(cx, l.storey, l.fixture).map(|c| (c, l.connector.to_string(), s))
        })
        .collect();
    // (storey, system, points, size, bend, insulation)
    type Piece = (usize, usize, Vec<(f64, f64, f64)>, (f64, f64), Option<f64>, f64);
    let pieces: Vec<Piece> = match mech {
        Some(c) => {
            use project::mech::model::Net;
            for (i, u) in c.units.iter().enumerate() {
                for (port, net) in &u.links {
                    let (Net::Air(s), Some(id)) = (net, mech_comp(cx, sy, c, i)) else {
                        continue;
                    };
                    if !links.iter().any(|l| l.0 == id && l.1 == *port) {
                        links.push((id, port.to_string(), SysRef::New(*s)));
                    }
                }
            }
            c.ducts
                .iter()
                .map(|r| {
                    let s = match r.net {
                        Net::Air(s) => s,
                        Net::Water(_) => 0,
                    };
                    (r.storey, s, r.points.clone(), r.size, r.bend, r.insulation)
                })
                .collect()
        }
        None => h
            .ducts
            .iter()
            .map(|d| (d.storey, usize::from(d.net) * 2 + usize::from(d.sys == AirSys::Extract), d.points.clone(), d.size, d.bend, 0.0))
            .collect(),
    };
    if pieces.len() > slots::DUCTS_MAX as usize {
        return Err("the ducts are larger than their slot block".into());
    }
    let base = elevation_of(cx, levels[0])?;
    let mut runs = vec![];
    for (storey, sys, points, size, bend, insulation) in &pieces {
        let shape = if size.1 > 0.0 {
            MepShape::Rect {
                w: size.0,
                h: size.1,
            }
        } else {
            MepShape::Round { d: size.0 }
        };
        let mut sec = MepSection::new(MepDomain::Duct, shape);
        sec.wall = 0.0008;
        sec.insulation = *insulation;
        let level = levels.get(*storey).copied().unwrap_or(roof_level);
        let pts = points.iter().map(|q| (q.0, q.1, base + q.2)).collect();
        let mut spec = RunSpec::new(0, level, sec, pts);
        spec.bend = *bend;
        runs.push((SysRef::New(*sys), spec));
    }
    // a run is kept, with its id, when the planned run is the stored one (level, system, section,
    // points); only runs that changed are deleted, and only new ones are built
    let keys: Vec<u64> = runs.iter().map(|r| fnv(&format!("{r:?}"))).collect();
    let range = slots::DUCTS..slots::DUCTS + slots::DUCTS_MAX;
    let prev: Vec<(u32, Rec)> = sy
        .prev
        .recs
        .range(range.clone())
        .map(|(r, x)| (*r, x.clone()))
        .collect();
    let mut open = vec![true; keys.len()];
    let mut gone = vec![];
    for (rel, r) in prev {
        let same = row_hash(&cx.ed.model, &r.table, r.id)?.is_some_and(|now| now == r.row);
        match (0..keys.len()).find(|&i| same && open[i] && keys[i] == r.intent) {
            Some(i) => {
                open[i] = false;
                sy.recs.insert(rel, r);
            }
            None => gone.push((rel, r)),
        }
    }
    let old_comps: BTreeSet<i64> = sy.prev.recs.values().filter(|r| r.table == "comp").map(|r| r.id).collect();
    for (rel, r) in gone {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                let why = "changed by hand after Apply: kept although the revision rebuilds the                            ducts";
                skip(cx, &r.table, r.id, why);
                sy.recs.insert(rel, r);
            }
            Some(_) => delete(cx, out, &r)?,
        }
    }
    let new_runs: Vec<usize> = (0..keys.len()).filter(|&i| open[i]).collect();
    if new_runs.is_empty() {
        return Ok(());
    }
    // components that stood before keep the links they have
    links.retain(|l| !old_comps.contains(&l.0));
    let build = MepBuild {
        systems: std::iter::once(&h.names)
            .chain(&h.nets)
            .enumerate()
            // with a dedicated unit the general extract is the return air, the dedicated one is exhausted
            .flat_map(|(i, n)| {
                let out = if i == 0 && !h.nets.is_empty() { SystemKind::ReturnAir } else { SystemKind::ExhaustAir };
                [(n.0.clone(), SystemKind::SupplyAir), (n.1.clone(), out)]
            })
            .collect(),
        runs: new_runs.iter().map(|&i| runs[i].clone()).collect(),
        links,
        ..MepBuild::default()
    };
    // the links read the terminals' placement: settle the quantities the item holds back
    cx.ed.model.settle_derived()?;
    cx.ed.model.hold_derived();
    push(
        cx,
        out,
        ModelStep::Mep {
            build: Box::new(build),
            expect: vec![],
        },
    )?;
    let Some(ModelStep::Mep { expect, .. }) = out.last() else {
        return Err("the duct network was not built".into());
    };
    let expect = expect.clone();
    for (t, id) in &expect {
        cx.declared.insert((t.to_string(), *id));
    }
    let mut spare = range.filter(|r| !sy.recs.contains_key(r)).collect::<Vec<_>>().into_iter();
    let ids = expect.iter().filter(|k| k.0 == "mep_run").map(|k| k.1);
    for (&i, id) in new_runs.iter().zip(ids) {
        let rel = spare.next().ok_or("the ducts are larger than their slot block")?;
        let rec = Rec {
            table: "mep_run".to_string(),
            id,
            intent: keys[i],
            row: 0,
        };
        sy.recs.insert(rel, rec);
        sy.fresh.insert(rel);
    }
    Ok(())
}

/// Water and drainage, then power and circuits: two network builds, so a change to one
/// keeps the other's references.
fn sync_services(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    p: &GProject,
    levels: &[i64],
) -> Result<(), PlanError> {
    use ddc_gen::project::{CircuitUse, GRun, SvcSys};
    use ddc_model::{CircuitDevice, CircuitKind, CircuitSpec, MepBuild, RunSpec, SysRef};
    let sv = &p.services;
    let slot0 = sy.slot0;
    let base = elevation_of(cx, levels[0])?;
    let name = |i: usize| -> Result<String, PlanError> {
        sv.names
            .get(i)
            .cloned()
            .ok_or_else(|| PlanError(format!("the services name no system {i}")))
    };
    let comp = |cx: &CompileCx<'_>, storey: usize, fixture: usize| -> Result<i64, PlanError> {
        let rel = slots::fixture_base(slots::storey_block(storey, p.basements.len())) + fixture as u32;
        cx.slots.get(&(slot0 + rel)).copied().ok_or_else(|| {
            PlanError(format!("service fixture {fixture} of storey {storey} was not placed"))
        })
    };
    let run_spec = |r: &GRun| -> Result<RunSpec, PlanError> {
        let level = *levels
            .get(r.storey)
            .ok_or_else(|| PlanError(format!("a service run names storey {}", r.storey)))?;
        let pts: Vec<(f64, f64, f64)> = r.points.iter().map(|q| (q.0, q.1, base + q.2)).collect();
        let mut spec = match r.pipe {
            Some((material, dn)) => RunSpec::pipe(0, level, material, dn, pts)?,
            None => RunSpec::new(0, level, r.section.clone(), pts),
        };
        spec.bend = r.bend;
        spec.caps = r.caps;
        Ok(spec)
    };
    let wet = [SvcSys::Cold, SvcSys::Hot, SvcSys::Sanitary];
    let widx = |s: SvcSys| SysRef::New(wet.iter().position(|w| *w == s).unwrap_or(0));
    let mut build = MepBuild::default();
    for s in wet {
        let i = SvcSys::ALL.iter().position(|a| *a == s).unwrap_or(0);
        build.systems.push((name(i)?, s.kind()));
    }
    for r in &sv.water {
        build.runs.push((widx(r.sys), run_spec(r)?));
    }
    for l in sv.links.iter().filter(|l| l.sys != SvcSys::Power) {
        let c = comp(cx, l.storey, l.fixture)?;
        build.links.push((c, l.connector.to_string(), widx(l.sys)));
    }
    sync_mep(cx, out, sy, [slots::WATER, slots::CIRCUITS], build)?;
    let mut build = MepBuild::default();
    build.systems.push((name(0)?, SvcSys::Power.kind()));
    for r in &sv.elec {
        build.runs.push((SysRef::New(0), run_spec(r)?));
    }
    for l in sv.links.iter().filter(|l| l.sys == SvcSys::Power) {
        let c = comp(cx, l.storey, l.fixture)?;
        build.links.push((c, l.connector.to_string(), SysRef::New(0)));
    }
    let standard = cx.ed.model.elec_standard()?;
    for (ci, c) in sv.circuits.iter().enumerate() {
        let kind = if matches!(c.use_, CircuitUse::Lighting | CircuitUse::Emergency) {
            CircuitKind::Lighting
        } else {
            CircuitKind::Power
        };
        let devices = c
            .devices
            .iter()
            .map(|d| {
                Ok(CircuitDevice {
                    comp: comp(cx, d.0, d.1)?,
                    connector: d.2.to_string(),
                    load: None,
                })
            })
            .collect::<Result<Vec<_>, PlanError>>()?;
        let mut spec = CircuitSpec::new(comp(cx, c.board.0, c.board.1)?, kind, devices);
        spec.name = c.name.clone();
        spec.number = c.number;
        if c.poles == 3 {
            spec.poles = 3;
            spec.voltage = 400.0;
        }
        let sized = ddc_model::auto_size(standard, &spec, c.load, c.length).ok_or_else(|| {
            PlanError(format!(
                "circuit {} of {:.0} W over {:.0} m has no cable in the table",
                c.name, c.load, c.length
            ))
        })?;
        // the generator's protection and cable stand unless the model's standard asks for more
        spec.breaker = c.breaker.max(sized.breaker);
        spec.cable = c.section.max(sized.section);
        if spec.cable == c.section {
            spec.cable_type = c.cable();
        }
        if let Some(n) = c.rcd {
            let board = sv.boards.iter().find(|b| b.at == c.board);
            if let Some(r) = board.and_then(|b| b.rcds.iter().find(|r| r.number == n)) {
                spec.rcd = r.label();
            }
        }
        for t in sv.traces.iter().filter(|t| t.circuit == ci) {
            if let Some(&level) = levels.get(t.storey) {
                let points = t.points.iter().map(|q| (q.0, q.1, base + q.2)).collect();
                spec.trace.push(ddc_model::CableTrace { level, points });
            }
        }
        build.circuits.push((Some(SysRef::New(0)), spec));
    }
    sync_mep(cx, out, sy, [slots::ELEC, slots::CIRCUITS], build)?;
    // the sprinkler system: its own network, so the services keep their references
    let fire = &p.fire;
    let mut build = MepBuild::default();
    build.systems.push((fire.name.clone(), SvcSys::Fire.kind()));
    for r in &fire.runs {
        build.runs.push((SysRef::New(0), run_spec(&r.run)?));
    }
    for l in &fire.links {
        let c = comp(cx, l.storey, l.fixture)?;
        build.links.push((c, l.connector.to_string(), SysRef::New(0)));
    }
    sync_mep(cx, out, sy, [slots::SPRINKLER, slots::CIRCUITS], build)?;
    // each standpipe is its own network, fed by the fire service apart from the sprinklers,
    // in its own slot block
    let mut build = MepBuild::default();
    for (i, sp) in fire.standpipes.iter().enumerate() {
        build.systems.push((sp.name.clone(), SvcSys::Fire.kind()));
        for r in &sp.runs {
            build.runs.push((SysRef::New(i), run_spec(&r.run)?));
        }
        for l in &sp.links {
            let c = comp(cx, l.storey, l.fixture)?;
            build.links.push((c, l.connector.to_string(), SysRef::New(i)));
        }
    }
    sync_mep(cx, out, sy, [slots::STANDPIPE, slots::CIRCUITS], build)
}

/// The water circuits of the mechanical design: four systems, the pipes cut at the pumps, and
/// every unit port a pipe reaches joined. Without the design its stored pipes go.
#[allow(clippy::too_many_arguments)]
fn sync_mech_pipes(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    p: &GProject,
    ru: bool,
    levels: &[i64],
    roof_level: i64,
    mech: Option<&project::mech::model::Content>,
) -> Result<(), PlanError> {
    use ddc_model::{MepBuild, PipeMaterial, RunSpec, SysRef, SystemKind};
    use project::mech::model::{water_names, Net};
    let mut build = MepBuild::default();
    if let Some(c) = mech.filter(|c| !c.pipes.is_empty()) {
        let kinds = [SystemKind::ChilledWater, SystemKind::ChilledWater, SystemKind::Heating, SystemKind::Heating];
        for (name, kind) in water_names(ru).into_iter().zip(kinds) {
            build.systems.push((name.to_string(), kind));
        }
        let base = elevation_of(cx, levels[0])?;
        for r in &c.pipes {
            let Net::Water(s) = r.net else { continue };
            let level = if r.storey < p.storeys.len() { levels[r.storey] } else { roof_level };
            let pts = r.points.iter().map(|q| (q.0, q.1, base + q.2)).collect();
            let mut spec = RunSpec::pipe(0, level, PipeMaterial::Steel, r.dn, pts)?;
            spec.section.insulation = r.insulation;
            build.runs.push((SysRef::New(s), spec));
        }
        for (i, u) in c.units.iter().enumerate() {
            let Some(id) = mech_comp(cx, sy, c, i) else { continue };
            for (port, net) in &u.links {
                if let Net::Water(s) = net {
                    build.links.push((id, port.to_string(), SysRef::New(*s)));
                }
            }
        }
    }
    sync_mep(cx, out, sy, [slots::MECH_PIPES, slots::CIRCUITS], build)
}

/// One network build kept as it is while its input and the rows it wrote are unchanged;
/// otherwise the stored runs and circuits are replaced, except those changed by hand since.
/// Runs are recorded from slot `at[0]`, circuits from `at[1]`.
fn sync_mep(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    at: [u32; 2],
    build: ddc_model::MepBuild,
) -> Result<(), PlanError> {
    let max = match at[0] {
        slots::WATER => slots::WATER_MAX,
        slots::SPRINKLER => slots::SPRINKLER_RUNS,
        slots::STANDPIPE => slots::STANDPIPE_MAX,
        slots::MECH_PIPES => slots::MECH_PIPES_MAX,
        _ => slots::ELEC_MAX,
    };
    let (nruns, ncirc) = (build.runs.len(), build.circuits.len());
    if nruns > max as usize || ncirc > slots::CIRCUITS_MAX as usize {
        return Err("the services are larger than their slot blocks".into());
    }
    // a run or circuit is kept, with its id, when the planned one is the stored one; only what
    // changed is deleted and only what is new is built
    let run_keys: Vec<u64> = build.runs.iter().map(|r| fnv(&format!("{r:?}"))).collect();
    let circ_keys: Vec<u64> = build.circuits.iter().map(|c| fnv(&format!("{c:?}"))).collect();
    // only the electrical build owns circuit slots
    let circuit_slots = if at[0] == slots::ELEC {
        slots::CIRCUITS..slots::CIRCUITS + slots::CIRCUITS_MAX
    } else {
        0..0
    };
    let run_slots = at[0]..at[0] + max;
    let prev: Vec<(u32, Rec)> = sy
        .prev
        .recs
        .range(run_slots.clone())
        .chain(sy.prev.recs.range(circuit_slots.clone()))
        .map(|(r, x)| (*r, x.clone()))
        .collect();
    let (mut open_runs, mut open_circs) = (vec![true; nruns], vec![true; ncirc]);
    let mut gone = vec![];
    for (rel, r) in prev {
        let same = row_hash(&cx.ed.model, &r.table, r.id)?.is_some_and(|now| now == r.row);
        let (keys, open) = if r.table == "mep_run" { (&run_keys, &mut open_runs) } else { (&circ_keys, &mut open_circs) };
        match (0..keys.len()).find(|&i| same && open[i] && keys[i] == r.intent) {
            Some(i) => {
                open[i] = false;
                sy.recs.insert(rel, r);
            }
            None => gone.push((rel, r)),
        }
    }
    let order = |t: &str| DELETE_ORDER.iter().position(|d| *d == t).unwrap_or(99);
    gone.sort_by_key(|g| order(&g.1.table));
    for (rel, r) in gone {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                let why = "changed by hand after Apply: kept although the revision rebuilds the                            services";
                skip(cx, &r.table, r.id, why);
                sy.recs.insert(rel, r);
            }
            Some(_) => delete(cx, out, &r)?,
        }
    }
    let new_runs: Vec<usize> = (0..nruns).filter(|&i| open_runs[i]).collect();
    let new_circs: Vec<usize> = (0..ncirc).filter(|&i| open_circs[i]).collect();
    if new_runs.is_empty() && new_circs.is_empty() {
        return Ok(());
    }
    // components that stood before keep the links they have
    let old_comps: BTreeSet<i64> = sy.prev.recs.values().filter(|r| r.table == "comp").map(|r| r.id).collect();
    let part = ddc_model::MepBuild {
        systems: build.systems.clone(),
        runs: new_runs.iter().map(|&i| build.runs[i].clone()).collect(),
        links: build.links.iter().filter(|l| !old_comps.contains(&l.0)).cloned().collect(),
        sleeves: build.sleeves.clone(),
        holes: build.holes.clone(),
        circuits: new_circs.iter().map(|&i| build.circuits[i].clone()).collect(),
    };
    cx.ed.model.settle_derived()?;
    cx.ed.model.hold_derived();
    push(
        cx,
        out,
        ModelStep::Mep {
            build: Box::new(part),
            expect: vec![],
        },
    )?;
    let Some(ModelStep::Mep { expect, .. }) = out.last() else {
        return Err("the service network was not built".into());
    };
    let expect = expect.clone();
    for (t, id) in &expect {
        cx.declared.insert((t.to_string(), *id));
    }
    for (table, range, news, keys) in [
        ("mep_run", run_slots, &new_runs, &run_keys),
        ("elec_circuit", at[1]..at[1] + slots::CIRCUITS_MAX, &new_circs, &circ_keys),
    ] {
        let mut spare = range.filter(|r| !sy.recs.contains_key(r)).collect::<Vec<_>>().into_iter();
        let ids = expect.iter().filter(|k| k.0 == table).map(|k| k.1);
        for (&i, id) in news.iter().zip(ids) {
            let rel = spare.next().ok_or("the services are larger than their slot blocks")?;
            let rec = Rec {
                table: table.to_string(),
                id,
                intent: keys[i],
                row: 0,
            };
            sy.recs.insert(rel, rec);
            sy.fresh.insert(rel);
        }
    }
    Ok(())
}

/// A wall type for every distinct rated wall of the project: the build-up of its role with
/// what it separates and its fire resistance in the name, so a schedule or an exchange file reads them.
fn fire_wall_types(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    spec: &ProjectSpec,
    p: &GProject,
) -> Result<Vec<((WallRole, FireSep, u32), i64)>, PlanError> {
    let mut kinds: BTreeSet<(u8, u8, u32)> = BTreeSet::new();
    let roles = [
        WallRole::Exterior,
        WallRole::Party,
        WallRole::Corridor,
        WallRole::Partition,
        WallRole::Retaining,
    ];
    let ix = |r: WallRole| roles.iter().position(|x| *x == r).unwrap_or(0) as u8;
    for st in p.storeys.iter().chain(&p.basements) {
        for w in &st.layout.walls {
            if let Some(f) = w.fire {
                kinds.insert((ix(w.role), u8::from(f.sep == FireSep::Stair), f.minutes));
            }
        }
    }
    for h in &p.fire.stair_heads {
        kinds.insert((ix(WallRole::Exterior), 1, h.fire));
    }
    let mut made = vec![];
    for (r, sep, min) in kinds {
        let (role, sep) = (roles[r as usize], if sep == 1 { FireSep::Stair } else { FireSep::Compartment });
        let what = if sep == FireSep::Stair { "Stair" } else { "Fire" };
        // the layers reach the rating (fire board added inside); the name tells what they reach
        let mut a = assembly::rated_in(&spec.rules, Use::of_role(role), spec.design.structure, min);
        a.name = format!("{what} Wall EI{} {}", a.fire_minutes, a.name);
        made.push(((role, sep, min), type_id(cx, out, &a)?));
    }
    Ok(made)
}

/// The type of every build-up the project holds under its rules, by use and whether its layers
/// run inside first: the walls of each storey, the slab on the ground and the floors above it,
/// and the roofs by slope. Found by name or made.
fn project_types(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    spec: &ProjectSpec,
    p: &GProject,
) -> Result<BTreeMap<(Use, bool), (Assembly, i64)>, PlanError> {
    let mut wanted: BTreeSet<(Use, bool)> = BTreeSet::new();
    for s in p.storeys.iter().chain(&p.basements) {
        wanted.extend(assembly::wall_uses(s).into_iter().map(|w| (w.use_, w.reversed)));
    }
    for u in assembly::uses_of(p).into_iter().filter(|u| !u.is_wall()) {
        wanted.insert((u, false));
    }
    let mut types = BTreeMap::new();
    for (u, rev) in wanted {
        let a = assembly::chosen(spec, u);
        let a = if rev { a.reversed() } else { a };
        let id = type_id(cx, out, &a)?;
        types.insert((u, rev), (a, id));
    }
    Ok(types)
}

/// Names grids already hold, and the name each stored grid slot keeps.
fn grid_names(
    cx: &CompileCx<'_>,
    sy: &Sync,
) -> Result<(BTreeSet<String>, BTreeMap<u32, String>), PlanError> {
    let taken = {
        let mut st = cx.ed.model.sql().prepare_cached("SELECT name FROM grid")?;
        let names = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<BTreeSet<String>, _>>()?;
        names
    };
    let mut kept = BTreeMap::new();
    for (rel, r) in &sy.prev.recs {
        if r.table == "grid" {
            let sql = "SELECT name FROM grid WHERE id=?1";
            if let Ok(n) = cx.ed.model.sql().query_row(sql, [r.id], |x| x.get(0)) {
                kept.insert(*rel, n);
            }
        }
    }
    Ok((taken, kept))
}

/// Finishes, structural properties and the stored recipe. Values go to what changed now,
/// or everywhere when the values themselves changed; never to what a hand edit kept.
#[allow(clippy::too_many_arguments)]
fn write_values(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    spec: &ProjectSpec,
    params_changed: bool,
    pid: &str,
    head: &J,
    keys: &BTreeMap<u32, String>,
) -> Result<(), PlanError> {
    for rel in sy.fresh.clone() {
        if let Some(r) = sy.recs.get_mut(&rel) {
            r.row = row_hash(&cx.ed.model, &r.table, r.id)?.unwrap_or(0);
        }
    }
    let hand_kept: BTreeSet<(&str, i64)> =
        cx.skips.iter().map(|s| (s.cat.as_str(), s.id)).collect();
    let valued: Vec<(u32, &Rec)> = sy
        .recs
        .iter()
        .filter(|(rel, r)| {
            let fresh = params_changed || sy.fresh.contains(rel);
            fresh && !hand_kept.contains(&(r.table.as_str(), r.id))
        })
        .map(|(rel, r)| (*rel, r))
        .collect();
    let rooms: Vec<i64> = valued
        .iter()
        .filter(|v| v.1.table == "room")
        .map(|v| v.1.id)
        .collect();
    let mut sets = structure_props(sy, &spec.fire_rating, project::fire::slab_rating(spec), &valued);
    sets.extend(opening_props(sy, &valued));
    sets.extend(key_props(&cx.ed.model, sy, keys));
    sets.extend(recipe_props(sy, pid, head));
    write_finishes(cx, out, spec, &rooms)?;
    let swings: Vec<Edit> = valued
        .iter()
        .filter(|(_, r)| r.table == "opening")
        .filter_map(|(rel, r)| {
            let t = sy.opens.get(rel)?;
            let swing = Swing {
                side: t.swing.side,
                hinge: t.swing.hinge,
            };
            Some(Edit::OpeningSwing { id: r.id, swing })
        })
        .collect();
    if !swings.is_empty() {
        push(cx, out, ModelStep::Edits(swings))?;
    }
    push(cx, out, ModelStep::Props(sets))?;
    Ok(())
}

/// Levels, types, then every storey in its slot block, the roof, parameters, structural
/// properties and the stored recipe. A stored project is brought to the spec, not rebuilt.
pub fn compile(
    cx: &mut CompileCx<'_>,
    op: &ProjectOp,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let b = build_on(&cx.ed.model, &op.input).map_err(PlanError)?;
    // circular facade runs become arc walls before any slot is counted
    let (merged, curved) = super::gen_arcs::merge(&b.project);
    let (spec, p) = (&b.spec, &merged);
    let mech = project::mech::design(spec, p).map(|m| project::mech::model::content(p, &m));
    let prev = load(&cx.ed.model, &op.pid)?;
    let mut own: BTreeSet<i64> = prev.head["own_levels"]
        .as_array()
        .map(|a| a.iter().filter_map(J::as_i64).collect())
        .unwrap_or_default();
    let params_changed = prev.head["input"]["params"] != op.input["params"];
    let mut sy = Sync {
        slot0: op.slot0,
        prev,
        recs: BTreeMap::new(),
        fresh: BTreeSet::new(),
        roles: BTreeMap::new(),
        opens: BTreeMap::new(),
        fires: BTreeMap::new(),
        door_fires: BTreeMap::new(),
    };
    let base = elevation_of(cx, op.level)?;
    let mut levels = vec![op.level];
    for s in &p.storeys[1..] {
        levels.push(level_at(cx, out, base + s.elevation, &s.name, &mut own)?);
    }
    let roof_level = level_at(cx, out, base + p.roof.elevation, &p.roof.name, &mut own)?;
    let mut down = vec![];
    for s in &p.basements {
        down.push(level_at(cx, out, base + s.elevation, &s.name, &mut own)?);
    }
    // split levels: a level for the lower block of each storey that has one
    let split = p.site.split.as_ref();
    let mut lows: Vec<Option<(i64, String)>> = vec![];
    for s in p.storeys.iter().chain(&p.basements) {
        let low = match split {
            Some(sp) if sp.pieces(&s.layout.slab, &s.layout.slab_holes).is_some_and(|x| !x.1.is_empty()) => {
                let name = GSplit::low_name(&s.name);
                Some((level_at(cx, out, base + s.elevation - sp.drop, &name, &mut own)?, name))
            }
            _ => None,
        };
        lows.push(low);
    }
    let sys = structure::system(spec.design.structure);
    let types = project_types(cx, out, spec, p)?;
    let type_of = |u: Use, rev: bool| types.get(&(u, rev)).map_or(0, |t| t.1);
    // the lowest storey stands on the ground; every slab above it is a floor between storeys
    let fire_types = fire_wall_types(cx, out, spec, p)?;
    let wtype = |w: WallUse, r: WallRole, f: Option<WallFire>| {
        let rated = f.and_then(|f| fire_types.iter().find(|t| t.0 == (r, f.sep, f.minutes)).map(|t| t.1));
        Some(type_ref("wall_type", rated.unwrap_or_else(|| type_of(w.use_, w.reversed))))
    };
    let ground_block = p.basements.len().checked_sub(1).map_or(0, slots::basement_block);
    let roof_type = |slope: f64| type_of(assembly::roof_use(slope), false);
    let flat_deck = types.get(&(Use::FlatRoof, false)).map_or(0.0, |t| t.0.thickness());
    let runs = spline_keys(p, &curved);
    let (kept_splines, absorbed) = drop_splines(cx, out, &mut sy, &runs)?;
    let dc = ddc_gen::project::dc::design(spec, p);
    let mut asked = asked_slots(p, mech.as_ref());
    dc_plan::asked(&dc, &mut asked);
    let timber = ddc_gen::project::timber::design(spec, p);
    timber_plan::asked(&timber, p.site.split.is_none(), &mut asked);
    delete_gone(cx, out, &mut sy, &asked)?;
    let (mut taken, kept_names) = grid_names(cx, &sy)?;
    let mut fam_types = BTreeMap::new();
    // flat roof: the deck sits on the walls, the exterior walls rise past it as a parapet
    let flat = !assembly::pitched(p.roof.slope);
    // (block, storey, level, level above): the storeys up, then each basement under the one
    // above it
    let mut stack: Vec<(u32, &PStorey, i64, i64, Option<i64>)> = p
        .storeys
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let top = levels.get(k + 1).copied().unwrap_or(roof_level);
            (slots::storey_block(k, p.basements.len()), s, levels[k], top, lows[k].as_ref().map(|x| x.0))
        })
        .collect();
    for (b, s) in p.basements.iter().enumerate() {
        let top = if b == 0 { levels[0] } else { down[b - 1] };
        let low = lows[p.storeys.len() + b].as_ref().map(|x| x.0);
        stack.push((slots::basement_block(b), s, down[b], top, low));
    }
    for (k, s, level, top, low) in stack {
        let mut grid_name = |rel: u32, base: &str| match kept_names.get(&rel) {
            Some(n) => n.clone(),
            None => free_grid(&mut taken, base),
        };
        let block = slots::block_base(k);
        for (i, o) in s.layout.openings.iter().enumerate() {
            if let Some(t) = &o.ty {
                sy.opens.insert(block + slots::OPENINGS + i as u32, t.clone());
            }
        }
        let ft = if k == ground_block {
            type_of(Use::GroundSlab, false)
        } else {
            type_of(Use::IntermediateSlab, false)
        };
        let fixtures = fixture_types(cx, out, &mut fam_types, &s.layout)?;
        let members = super::gen_map::member_types(cx, &s.layout, out, sys)?;
        let types = (&wtype as &dyn Fn(WallUse, WallRole, Option<WallFire>) -> Option<ElRef>, ft, fixtures, members);
        let parapet = if flat && top == roof_level {
            flat_deck + PARAPET
        } else {
            0.0
        };
        // the lower block's elements stand on its level and reach half a storey under the top
        let lower = split.zip(low).map(|(sp, l)| Lower {
            sp,
            level: l,
            top: (top, -sp.drop),
        });
        let arcs = curved.get(&k).map_or(&[][..], |c| c.bulges.as_slice());
        let first = slots::block_base(k);
        for (i, w) in s.layout.walls.iter().enumerate() {
            if let Some(f) = w.fire {
                sy.fires.insert(first + slots::WALLS + i as u32, f.minutes);
            }
        }
        for (i, o) in s.layout.openings.iter().enumerate().filter(|(_, o)| o.door) {
            if let Some(f) = s.layout.walls.get(o.wall).and_then(|w| w.fire) {
                sy.door_fires.insert(first + slots::OPENINGS + i as u32, f.minutes);
            }
        }
        for mut g in storey_wants(s, arcs, op.slot0, k, level, top, parapet, &types, lower, &mut grid_name)? {
            // the walls of a kept spline wall are its pieces: carried over as they are, never
            // edited one by one; a chord it absorbed is not made again
            for w in g.iter().filter(|w| absorbed.contains(&w.rel)) {
                if let Some(role) = w.role {
                    sy.roles.insert(w.rel, role);
                }
                if let Some(r) = sy.prev.recs.get(&w.rel) {
                    sy.recs.insert(w.rel, r.clone());
                }
            }
            g.retain(|w| !absorbed.contains(&w.rel));
            if let (true, Some(i)) = (p.site.split.is_none(), p.storeys.iter().position(|x| std::ptr::eq(x, s))) {
                timber_plan::cut_slab(&timber, i, &mut g);
            }
            sync_group(cx, out, &mut sy, g)?;
        }
    }
    // wings and corner halls each carry their own roof after the main one
    let roof_at = |rel: u32, ring: Vec<(f64, f64)>, offset: f64, r: &ddc_gen::project::PRoof, edges: Vec<Option<f64>>| Want {
        rel,
        table: "roof",
        elem: NewElem::Roof {
            level: roof_level,
            stype: Some(type_ref("slab_type", roof_type(r.slope))),
            offset,
            overhang: r.overhang,
            ring,
            slope: Some(r.slope),
            edges,
            vault: r.vault.as_ref().map(|v| RoofVault {
                a: v.a,
                b: v.b,
                profile: v.profile.clone(),
                depth: v.depth,
            }),
            eave: r.eave,
        },
        role: None,
        wall_rel: None,
    };
    let deck = if flat { flat_deck } else { 0.0 };
    // split levels: each roof parts on the cuts, the lower pieces half a storey down
    let drop = split.map_or(0.0, |sp| sp.drop);
    let roofs: Vec<Want> = split_roofs(p)
        .into_iter()
        .map(|(rel, r, low)| roof_at(rel, r.ring.clone(), (if low { deck - drop } else { deck }) + r.lift, &r, r.edges.clone()))
        .collect();
    sync_group(cx, out, &mut sy, roofs)?;
    for (h, hd) in heads(p).iter().enumerate() {
        let base = slots::HEADS + h as u32 * slots::HEAD_SLOTS;
        for j in 0..hd.ring.len() as u32 {
            sy.fires.insert(base + j, hd.fire);
        }
        sy.door_fires.insert(base + 4, hd.fire);
    }
    let head_wall = |min: u32| {
        let rated = fire_types.iter().find(|t| t.0 == (WallRole::Exterior, FireSep::Stair, min));
        rated.map(|t| type_ref("wall_type", t.1))
    };
    for g in head_wants(p, op.slot0, roof_level, deck, &head_wall, roof_type(0.0)) {
        sync_group(cx, out, &mut sy, g)?;
    }
    // head walls keep their full height: the roof trim must not cut them to a roof under them
    for (h, hd) in heads(p).iter().enumerate() {
        let base = slots::HEADS + h as u32 * slots::HEAD_SLOTS;
        for j in 0..hd.ring.len() as u32 {
            if let Some(&id) = cx.slots.get(&(op.slot0 + base + j)) {
                push(cx, out, ModelStep::Release { id })?;
            }
        }
    }
    let mech_types = match &mech {
        Some(c) => mech_types(cx, out, c)?,
        None => vec![],
    };
    let mut plant = plant_wants(cx, out, &mut fam_types, p, roof_level)?;
    if let Some(c) = &mech {
        // the designed units, sized with all their ports, stand where the ventilation put them
        for (i, (w, u)) in plant.iter_mut().zip(c.units.iter().take(ahus(c))).enumerate() {
            w.elem = NewElem::Comp {
                level: roof_level,
                ctype: type_ref("comp_type", mech_types[i]),
                at: (u.at.0, u.at.1, u.at.2 - p.roof.elevation),
                rotation: u.rot,
                host: None,
                scale: 1.0,
                    min_clear: 0.0,
            };
        }
    }
    sync_group(cx, out, &mut sy, plant)?;
    if let Some(c) = &mech {
        let wants = mech_wants(p, c, &mech_types, &levels, roof_level);
        sync_group(cx, out, &mut sy, wants)?;
    }
    sync_ducts(cx, out, &mut sy, p, &levels, roof_level, mech.as_ref())?;
    sync_services(cx, out, &mut sy, p, &levels)?;
    sync_mech_pipes(cx, out, &mut sy, p, spec.ru, &levels, roof_level, mech.as_ref())?;
    dc_plan::sync(cx, out, &mut sy, spec, &dc, &levels)?;
    timber_plan::sync(cx, out, &mut sy, &timber, &levels)?;
    let shell_head = super::shell_plan::compile(
        cx,
        out,
        op.slot0,
        op.level,
        base,
        p.shell.as_deref(),
        &sy.prev.head["shell"],
    )?;
    let mut wanted: Vec<(i64, String)> = p
        .storeys
        .iter()
        .zip(&levels)
        .chain(p.basements.iter().zip(&down))
        .map(|(s, id)| (*id, s.name.clone()))
        .collect();
    wanted.extend(lows.into_iter().flatten());
    wanted.push((roof_level, p.roof.name.clone()));
    tidy_levels(cx, out, &mut own, &wanted)?;
    // after every element: a spline wall takes its chords as pieces, the surplus rows go
    let mut spline_recs = kept_splines;
    spline_recs.extend(splines(cx, out, &mut sy, &runs)?);
    let head = json!({
        "input": op.input,
        "level": op.level,
        "roof": roof_level,
        "own_levels": own.iter().collect::<Vec<_>>(),
        "splines": spline_recs,
        "shell": shell_head,
    });
    let keys = key_map(p, pid_number(&op.pid).max(1) as u32);
    write_values(cx, out, &mut sy, spec, params_changed, &op.pid, &head, &keys)
}

// ---- revisions around locks, and pinning hand edits ----

/// `old` with `patch` merged in. With locks in the brief the change is made around them
/// (worked around where it would break one); what could not be made comes back as refusals.
/// A brief on the model's terrain is regenerated on it and keeps the word `"model"`.
pub fn revise(m: &Model, old: &J, patch: &J) -> Result<(J, Vec<J>), String> {
    let mut next = old.clone();
    // rooms added to named storeys only: every other storey is locked to what it is, so
    // the change is placed on those storeys alone
    let (mut old, given) = (old.clone(), old["locks"].clone());
    // a whole-storey lock: its storey and what it keeps
    let whole = |l: &J| {
        let plain = l.get("room").is_none() && l.get("zone").is_none() && l.get("rule").is_none();
        l["storey"].as_u64().filter(|_| plain).map(|k| (k, l["keep"].as_str().unwrap_or("").to_string()))
    };
    let mut autos: Vec<J> = vec![];
    if let Some(o) = patch["storey_rooms"].as_object() {
        let free: Vec<usize> = o.keys().filter_map(|k| k.parse().ok()).collect();
        old = locks::lock_all_but(&with_model_terrain(m, &old), &free)?;
        keep_model_terrain(&next, &mut old);
        let had: Vec<_> = given.as_array().into_iter().flatten().filter_map(&whole).collect();
        autos = old["locks"].as_array().into_iter().flatten().filter(|l| whole(l).is_some_and(|k| !had.contains(&k))).cloned().collect();
        next = old.clone();
    }
    // the storey locks were for this change only: they come out of the stored brief again, and
    // every other lock, the user's old ones and any the patch gave, stays
    let unlock = |mut b: J| {
        if !autos.is_empty() {
            let mine: Vec<_> = autos.iter().filter_map(&whole).collect();
            let kept: Vec<J> = b["locks"].as_array().into_iter().flatten().filter(|l| !whole(l).is_some_and(|k| mine.contains(&k))).cloned().collect();
            if kept.is_empty() {
                b.as_object_mut().map(|o| o.remove("locks"));
            } else {
                b["locks"] = J::Array(kept);
            }
        }
        b
    };
    let old = &old;
    project::merge_patch(&mut next, patch);
    // a patch that sets `locks` replaces the array: the storey locks of this change go back in
    if !autos.is_empty() {
        let mut all: Vec<J> = next["locks"].as_array().cloned().unwrap_or_default();
        let have: Vec<_> = all.iter().filter_map(&whole).collect();
        all.extend(autos.iter().filter(|l| whole(l).is_some_and(|k| !have.contains(&k))).cloned());
        next["locks"] = J::Array(all);
    }
    let all = locks::of_brief(&next)?;
    if all.is_empty() {
        // rooms kept for locks that are gone are free again
        if let Some(o) = next.as_object_mut() {
            o.remove("pins");
        }
        return Ok((unlock(next), vec![]));
    }
    let r = locks::regenerate(&with_model_terrain(m, old), &with_model_terrain(m, &next), &all)?;
    let mut brief = r.brief;
    keep_model_terrain(&next, &mut brief);
    Ok((unlock(brief), r.refused.iter().map(locks::Refusal::to_json).collect()))
}

/// What of a stored project a pin keeps: one room, or a storey's walls, openings and slab.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PinAt {
    Room { storey: usize, index: usize },
    Storey(usize),
}

fn pin_at(rel: u32, basements: usize) -> Option<PinAt> {
    let (blk, off) = slots::locate(rel)?;
    // a basement is no storey a lock can name
    let slots::Floor::Storey(k) = slots::floor_of(blk, basements)? else { return None };
    if (slots::ROOMS..slots::SLAB).contains(&off) {
        Some(PinAt::Room { storey: k, index: (off - slots::ROOMS) as usize })
    } else if off < slots::ROOMS || off == slots::SLAB {
        Some(PinAt::Storey(k))
    } else {
        None
    }
}

/// Rooms and storeys of the stored projects whose elements were changed by hand since Apply.
pub fn hand_edited(m: &Model) -> Result<BTreeSet<(String, PinAt)>, PlanError> {
    let mut out = BTreeSet::new();
    for (pid, _) in stored(m)? {
        let s = load(m, &pid)?;
        let basements = project::parse_spec(&s.head["input"]).map_or(0, |x| x.basements as usize);
        for (rel, r) in &s.recs {
            let Some(at) = pin_at(*rel, basements) else { continue };
            if out.contains(&(pid.clone(), at.clone())) {
                continue;
            }
            if row_hash(m, &r.table, r.id)?.is_some_and(|h| h != r.row) {
                out.insert((pid.clone(), at));
            }
        }
    }
    Ok(out)
}

/// Pins `at` of project `pid`: a geometry lock sealed on the stored brief's build and written
/// into that brief, one undoable step. A later revision keeps it and places the rest around.
pub fn pin(m: &mut Model, pid: &str, at: &PinAt) -> Result<String, String> {
    let mut head = load(m, pid).map_err(|e| e.to_string())?.head;
    let input = head["input"].clone();
    let b = build_on(m, &input)?;
    let target = match *at {
        PinAt::Room { storey, index } => {
            let st = b.project.storeys.get(storey).ok_or("the storey is no longer in the project")?;
            let r = st.rooms.get(index).ok_or("the room is no longer in the project")?;
            locks::Target::Room { storey, number: r.number.clone() }
        }
        PinAt::Storey(k) => locks::Target::Storey(k),
    };
    let lock = locks::Lock::take(target, locks::Keep::Geometry, &input, &b.spec, &b.project)?;
    let mut all = locks::of_brief(&input)?;
    if all.iter().any(|l| l.target == lock.target && l.keep == lock.keep) {
        return Ok(lock.label());
    }
    all.push(lock.clone());
    head["input"]["locks"] = J::Array(all.iter().map(locks::Lock::to_json).collect());
    m.set_prop(RECIPE, 0, &format!("{pid}|spec"), Some((Value::Text(head.to_string()), "")))
        .map_err(|e| e.to_string())?;
    Ok(lock.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::TopoSpec;

    /// The storey locks a `storey_rooms` revision sets come out of the stored brief again; the
    /// locks the person holds, and those the same patch gives, stay.
    #[test]
    fn storey_room_locks_come_out_and_other_locks_stay() {
        let m = Model::open(":memory:").unwrap();
        let brief = json!({"typology": "hospital_wing", "floors": 4});
        let spec = project::parse_spec(&brief).unwrap();
        let p = project::build_project(&spec).unwrap();
        let mine = locks::Lock::take(locks::Target::Storey(0), locks::Keep::Program, &brief, &spec, &p).unwrap().to_json();
        let mut old = brief.clone();
        old["locks"] = json!([mine]);
        let room = json!({"name": "Operating theatre", "kind": "technical", "area": 48, "tags": ["operating"]});
        let storeys = |b: &J| -> Vec<(u64, String)> {
            b["locks"].as_array().into_iter().flatten().map(|l| (l["storey"].as_u64().unwrap(), l["keep"].as_str().unwrap().to_string())).collect()
        };
        let (out, _) = revise(&m, &old, &json!({"storey_rooms": {"2": [room.clone()]}})).unwrap();
        assert_eq!(storeys(&out), vec![(0, "program".to_string())], "{:?}", out["locks"]);
        // the patch sets its own locks: they stay, and the storey locks still go
        let (out, _) = revise(&m, &brief, &json!({"storey_rooms": {"2": [room]}, "locks": [mine]})).unwrap();
        assert_eq!(storeys(&out), vec![(0, "program".to_string())], "{:?}", out["locks"]);
    }

    /// A model whose toposolid is the plane z = 50 - s * x, surveyed every 5 m.
    fn sloped_model(s: f64) -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let mut points = vec![];
        for i in -12..=24 {
            for j in -12..=16 {
                let (x, y) = (f64::from(i) * 5.0, f64::from(j) * 5.0);
                points.push((x, y, 50.0 - s * x));
            }
        }
        let spec = TopoSpec {
            name: "Ground".into(),
            thickness: 2.0,
            material: "earth".into(),
            points,
            ..Default::default()
        };
        m.add_toposolid(&spec).unwrap();
        m
    }

    #[test]
    fn the_model_terrain_sets_the_ground_level_and_the_volumes() {
        let m = sloped_model(0.1);
        let input = json!({"typology": "office", "floors": 2, "origin": [10.0, 5.0],
            "site": {"terrain": "model", "pad_margin": 0}});
        let b = build_on(&m, &input).unwrap();
        let p = &b.project;
        assert!(p.site.terrain && p.site.split.is_none(), "{}", p.site.split_why);
        // the model frame: the project already stands at its origin
        let slab = &p.storeys[0].layout.slab;
        let (lo, hi) = slab.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), q| {
            ((lo.0.min(q.0), lo.1.min(q.1)), (hi.0.max(q.0), hi.1.max(q.1)))
        });
        let (l, a) = (hi.0 - lo.0, (hi.0 - lo.0) * (hi.1 - lo.1));
        // balanced on a plane: the level at the middle, s * L * A / 8 cut and as much fill
        let mid = 50.0 - 0.1 * (lo.0 + hi.0) / 2.0;
        assert!((p.site.ground_z - mid).abs() <= 0.03, "{} vs {mid}", p.site.ground_z);
        let each = 0.1 * l * a / 8.0;
        assert!((p.site.cut - each).abs() < 0.05 * each + a * 0.03, "cut {} vs {each}", p.site.cut);
        assert!((p.site.fill - each).abs() < 0.05 * each + a * 0.03, "fill {} vs {each}", p.site.fill);
        // the stored input keeps the word, so a rebuild reads the terrain as it is then
        assert_eq!(input["site"]["terrain"], "model");
        // a model without a toposolid leaves the word to the reader, which names the field
        let bare = Model::open(":memory:").unwrap();
        let e = build_on(&bare, &input).err().unwrap();
        assert!(e.contains("site.terrain"), "{e}");
    }

    #[test]
    fn a_split_project_parts_its_slabs_and_roof_on_the_line() {
        let m = sloped_model(0.3);
        // a corridor wide enough for a platform lift beside the half-level flight
        let input = json!({"typology": "office", "floors": 3, "roof": {"kind": "flat"}, "corridor": 2.6,
            "site": {"terrain": "model"}});
        let b = build_on(&m, &input).unwrap();
        let p = &b.project;
        let sp = p.site.split.as_ref().unwrap_or_else(|| panic!("one level: {}", p.site.split_why));
        let asked = asked_slots(p, None);
        for k in 0..p.storeys.len() as u32 {
            assert!(asked.contains(&slots::split_block(k)), "lower slab of storey {k}");
        }
        let roofs = split_roofs(p);
        let extra = roofs.iter().filter(|r| r.0 >= slots::SPLIT_ROOF).count();
        assert!(extra >= 1 && roofs.iter().any(|r| r.2) && roofs.iter().any(|r| !r.2));
        assert!(roofs.iter().all(|r| asked.contains(&r.0)));
        let faces: usize = p
            .storeys
            .iter()
            .map(|s| sp.pieces(&s.layout.slab, &s.layout.slab_holes).map_or(0, |(h, l)| h.len() + l.len() - 1))
            .sum();
        assert_eq!(
            asked.iter().filter(|r| (slots::SPLIT..slots::SPLIT_ROOF).contains(*r)).count(),
            faces + sp.flights.len()
        );
        // the roof pieces cover the roofs exactly, the lower ones on the lower half-level
        let area = |r: &[(f64, f64)]| {
            let n = r.len();
            ((0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>() / 2.0).abs()
        };
        let whole: f64 = std::iter::once(&p.roof).chain(&p.extra_roofs).map(|r| area(&r.ring)).sum();
        assert!((roofs.iter().map(|r| area(&r.1.ring)).sum::<f64>() - whole).abs() < 1e-6);
        for (_, r, low) in &roofs {
            assert_eq!(sp.ring_low(&r.ring), *low);
        }
        // a lift reaches both half-levels: a platform lift beside each flight here
        assert!(sp.access.iter().any(|a| a.kind == ddc_gen::project::site::Access::Platform));
        assert!(ddc_gen::project::site::barrier_free(p).is_empty());
    }
}

/// A spline run of the project: its block, the wall slots in axis order, the intent (what
/// the generator asked of it: axis, walls, their openings and fixtures) and the run.
struct RunKey<'a> {
    rels: Vec<u32>,
    intent: u64,
    run: &'a super::gen_arcs::SplineRun,
}

fn spline_keys<'a>(p: &GProject, curved: &'a BTreeMap<u32, super::gen_arcs::Curved>) -> Vec<RunKey<'a>> {
    let mut out = vec![];
    for (k, s) in p.blocks() {
        let Some(c) = curved.get(&k) else { continue };
        let l = &s.layout;
        for run in &c.splines {
            let rels: Vec<u32> =
                run.walls.iter().map(|&w| slots::block_base(k) + slots::WALLS + w as u32).collect();
            let on = |w: usize| run.walls.contains(&w);
            let walls: Vec<_> = run.walls.iter().map(|&w| &l.walls[w]).collect();
            let openings: Vec<_> = l.openings.iter().filter(|o| on(o.wall)).collect();
            let fixtures: Vec<_> = l.fixtures.iter().filter(|f| f.wall.is_some_and(on)).collect();
            let intent = fnv(&format!("{rels:?}|{:?}|{walls:?}|{openings:?}|{fixtures:?}|{}", run.axis, l.height));
            out.push(RunKey { rels, intent, run });
        }
    }
    out
}

/// Before the storeys are synced: a stored spline wall whose run the generator still asks
/// for unchanged is kept, as is one changed by hand (noted); any other goes whole, with the
/// openings and components on its pieces, and their slots are made again from the chords.
/// Returns the kept records and the wall slots kept spline walls absorbed.
fn drop_splines(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    runs: &[RunKey<'_>],
) -> Result<(Vec<J>, BTreeSet<u32>), PlanError> {
    let stored: Vec<J> = sy.prev.head["splines"].as_array().cloned().unwrap_or_default();
    let (mut kept, mut absorbed) = (vec![], BTreeSet::new());
    for s in stored {
        let (Some(id), Some(intent)) = (s["id"].as_i64(), s["intent"].as_u64()) else { continue };
        let rels: Vec<u32> = s["rels"]
            .as_array()
            .map(|a| a.iter().filter_map(|r| r.as_u64().map(|r| r as u32)).collect())
            .unwrap_or_default();
        // deleted by hand: the revision does not bring it back, as for any element
        let pieces = match cx.ed.model.spline_wall_pieces(id) {
            Ok(p) if !p.is_empty() => p,
            _ => {
                skip(cx, "wall_spline", id, "deleted by hand after Apply: the revision does not bring it back");
                absorbed.extend(rels);
                continue;
            }
        };
        let same = runs.iter().any(|r| r.rels == rels && r.intent == intent);
        let mut by_hand = false;
        for r in sy.prev.recs.values().filter(|r| r.table == "wall" && pieces.contains(&r.id)) {
            by_hand |= row_hash(&cx.ed.model, "wall", r.id)? != Some(r.row);
        }
        if same || by_hand {
            if by_hand && !same {
                skip(cx, "wall_spline", id, "changed by hand after Apply: kept as it is, the revision would change it");
            }
            absorbed.extend(rels);
            kept.push(s);
            continue;
        }
        let mut gone: BTreeSet<(String, i64)> = pieces.iter().map(|&w| ("wall".to_string(), w)).collect();
        for &w in &pieces {
            for (table, sql) in [
                ("opening", "SELECT id FROM opening WHERE wall=?1"),
                ("comp", "SELECT id FROM comp WHERE host_wall=?1"),
            ] {
                let ids: Vec<i64> = cx
                    .ed
                    .model
                    .sql()
                    .prepare(sql)?
                    .query_map([w], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                gone.extend(ids.into_iter().map(|i| (table.to_string(), i)));
            }
        }
        sy.prev.recs.retain(|_, r| !gone.contains(&(r.table.clone(), r.id)));
        push(cx, out, ModelStep::Delete { table: "wall_spline", id })?;
    }
    Ok((kept, absorbed))
}

/// Fitting tolerances tried in turn: the finest whose pieces hold every opening.
const SPLINE_TOLS: [f64; 3] = [0.002, 0.01, 0.05];

/// Each non-circular facade run of every storey as one spline wall on its chords; returns
/// the records stored with the project. The chords' rows become pieces: the surplus rows are
/// gone, so their slots leave the project's records.
fn splines(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    runs: &[RunKey<'_>],
) -> Result<Vec<J>, PlanError> {
    let mut made_recs = vec![];
    for RunKey { rels, intent, run } in runs {
        let ids: Option<Vec<i64>> = rels.iter().map(|&r| cx.slots.get(&(sy.slot0 + r)).copied()).collect();
        // a kept spline wall, a wall kept by hand, or a piece already: the chords stay
        let Some(ids) = ids else { continue };
        if ids.iter().any(|&w| cx.ed.model.spline_of_wall(w).ok().flatten().is_some()) {
            continue;
        }
        {
            let row: (i64, i64, f64, f64, Option<f64>) = cx.ed.model.sql().query_row(
                "SELECT level, wtype, base_offset, height, offset0 FROM wall WHERE id=?1",
                [ids[0]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let mut last = None;
            for tol in SPLINE_TOLS {
                let spec = ddc_model::SplineWallSpec {
                    level: row.0,
                    wall_type: row.1,
                    axis: run.axis.clone(),
                    base_offset: row.2,
                    height: row.3,
                    offset0: row.4,
                    tol,
                };
                let step = ModelStep::Splines { specs: vec![(ids.clone(), spec)], expect: vec![] };
                match push(cx, out, step) {
                    Ok(made) => {
                        for &id in &made {
                            cx.declared.insert(("wall_spline".to_string(), id));
                            made_recs.push(json!({"id": id, "rels": rels, "intent": intent}));
                        }
                        last = None;
                        break;
                    }
                    Err(e) => last = Some(e),
                }
            }
            if let Some(e) = last {
                return Err(PlanError(format!("a curved facade as a spline wall: {}", e.0)));
            }
        }
        for (rel, &w) in rels.iter().zip(&ids) {
            if row_hash(&cx.ed.model, "wall", w)?.is_none() {
                sy.recs.remove(rel);
                sy.fresh.remove(rel);
            }
        }
    }
    Ok(made_recs)
}
