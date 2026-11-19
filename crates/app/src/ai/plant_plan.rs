//! A process plant from a brief as one plan item: units, stack items, supports, steel and flues
//! as components of their families, the pipes as one network build with the connectors
//! joined, a storage tank's ring foundation as strip footings. Apply stores the brief and the element of every key in the model (`prop`, kind
//! [`RECIPE`], ref 0), so a later design of the same plant moves, adds and deletes only what
//! differs and keeps the ids of the rest; an element changed by hand since is left alone.

use super::exec::{ModelStep, NewResolved, PropSet};
use super::gen_map::type_ref;
use super::plan::{compile_create, push, CompileCx, NewElem, PlanError};
use super::project_plan::row_hash;
use super::rel_value::Skip;
use ddc_gen::plant::{self, Kit, KitFamily, Plant};
use ddc_model::{CompSpec, MepBuild, Model, RunSpec, SysRef};
use rusqlite::types::Value;
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet};

/// `prop.kind` of the stored plants.
pub const RECIPE: &str = "gen_plant";
/// Slots an item reserves: one per placed element.
pub const SLOTS: u32 = 8192;
/// Type every plant family carries.
const TYPE: &str = "Standard";

#[derive(Clone, Debug)]
pub struct PlantOp {
    /// The brief as the assistant wrote it.
    pub input: J,
    /// Level the hall floor stands on.
    pub level: i64,
    pub slot0: u32,
    /// Stored plant id such as `K1`: a stored one is brought to the brief, not built again.
    pub pid: String,
}

/// One stored element: table, id, what was asked (hash) and the row as written (hash).
#[derive(Clone, Debug, PartialEq)]
struct Rec {
    table: String,
    id: i64,
    intent: u64,
    row: u64,
}

impl Rec {
    fn to_json(&self) -> J {
        json!([self.table, self.id, format!("{:016x}", self.intent), format!("{:016x}", self.row)])
    }
    fn of(v: &J) -> Option<Rec> {
        let h = |i: usize| u64::from_str_radix(v[i].as_str()?, 16).ok();
        Some(Rec { table: v[0].as_str()?.to_string(), id: v[1].as_i64()?, intent: h(2)?, row: h(3)? })
    }
}

#[derive(Default)]
struct Stored {
    head: J,
    comps: BTreeMap<String, Rec>,
    runs: Vec<Rec>,
    /// Columns, beams and walls by key.
    frame: BTreeMap<String, Rec>,
}

fn pid_number(pid: &str) -> u64 {
    pid.trim_start_matches('K').parse().unwrap_or(0)
}

/// Plants stored in the model, oldest first: id and `{input, level, comps, runs}`.
pub fn stored(m: &Model) -> Result<Vec<(String, J)>, PlanError> {
    let sql = "SELECT key, CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=0 AND key LIKE '%|spec'";
    let mut st = m.sql().prepare(sql)?;
    let rows = st
        .query_map([RECIPE], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out: Vec<(String, J)> = rows
        .into_iter()
        .filter_map(|(k, v)| Some((k.strip_suffix("|spec")?.to_string(), serde_json::from_str(&v?).ok()?)))
        .collect();
    out.sort_by_key(|p| pid_number(&p.0));
    Ok(out)
}

/// A plant id not stored and not in `drafted`.
pub fn next_pid(m: &Model, drafted: &[String]) -> Result<String, PlanError> {
    let top = stored(m)?.iter().map(|p| pid_number(&p.0)).chain(drafted.iter().map(|p| pid_number(p))).max().unwrap_or(0);
    Ok(format!("K{}", top + 1))
}

fn load(m: &Model, pid: &str) -> Result<Stored, PlanError> {
    let Some((_, head)) = stored(m)?.into_iter().find(|p| p.0 == pid) else {
        return Ok(Stored::default());
    };
    let comps = head["comps"]
        .as_object()
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), Rec::of(v)?))).collect())
        .unwrap_or_default();
    let runs = head["runs"].as_array().map(|a| a.iter().filter_map(Rec::of).collect()).unwrap_or_default();
    let frame = head["frame"]
        .as_object()
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), Rec::of(v)?))).collect())
        .unwrap_or_default();
    Ok(Stored { head, comps, runs, frame })
}

/// The plant a brief designs.
pub fn design(input: &J) -> Result<Plant, String> {
    plant::design(input)
}

fn skip(cx: &mut CompileCx<'_>, table: &str, id: i64, why: &str) {
    cx.skips.push(Skip { cat: table.to_string(), id, why: why.to_string() });
}

/// A family of this plant is named after the plant; shared families keep their own name.
fn family_name(pid: &str, f: &KitFamily) -> String {
    if f.own {
        format!("{pid}_{}", f.name)
    } else {
        f.name.clone()
    }
}

/// Every family loaded (again only when its text changed) and its type id by kit name.
fn families(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, pid: &str, kit: &Kit) -> Result<BTreeMap<String, i64>, PlanError> {
    let mut types = BTreeMap::new();
    for f in &kit.families {
        let name = family_name(pid, f);
        let mut lines = f.text.lines();
        lines.next();
        let def = format!("family {name}\n{}\n", lines.collect::<Vec<_>>().join("\n"));
        let have: Option<String> = cx
            .ed
            .model
            .sql()
            .query_row("SELECT def FROM comp_family WHERE name=?1", [&name], |r| r.get(0))
            .ok();
        if have.as_deref() != Some(def.as_str()) {
            push(cx, out, ModelStep::Family { def, overwrite: true })?;
        }
        let fam = cx.ed.model.family_id(&name)?.ok_or_else(|| PlanError(format!("family {name} was not loaded")))?;
        let sql = "SELECT id FROM comp_type WHERE family=?1 AND name=?2";
        let id: i64 = cx
            .ed
            .model
            .sql()
            .query_row(sql, rusqlite::params![fam, TYPE], |r| r.get(0))
            .map_err(|_| PlanError(format!("family {name} has no type {TYPE}")))?;
        types.insert(f.name.clone(), id);
    }
    Ok(types)
}

/// What a placement asks for, as a component spec.
fn comp_spec(level: i64, ctype: i64, at: (f64, f64, f64), rotation: f64) -> CompSpec {
    let mut s = CompSpec::at(level, ctype, at.0, at.1);
    s.at.2 = at.2;
    s.rotation = rotation;
    s
}

/// The placed elements: new ones created in runs of slots, stored ones kept, moved in place,
/// or left as a hand edit made them; the ones the plant no longer has deleted.
fn sync_comps(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    op: &PlantOp,
    kit: &Kit,
    types: &BTreeMap<String, i64>,
    prev: &Stored,
    recs: &mut BTreeMap<String, Rec>,
    fresh: &mut BTreeSet<String>,
) -> Result<(), PlanError> {
    let asked: BTreeSet<&str> = kit.comps.iter().map(|c| c.key.as_str()).collect();
    for (k, r) in prev.comps.iter().filter(|(k, _)| !asked.contains(k.as_str())) {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                skip(cx, &r.table, r.id, "changed by hand after Apply: kept although the plant no longer has it");
                recs.insert(k.clone(), r.clone());
            }
            Some(_) => {
                push(cx, out, ModelStep::Delete { table: "comp", id: r.id })?;
                cx.declared.insert((r.table.clone(), r.id));
            }
        }
    }
    let mut run: Vec<(u32, String, NewElem, u64)> = vec![];
    let flush = |cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, run: &mut Vec<(u32, String, NewElem, u64)>, recs: &mut BTreeMap<String, Rec>, fresh: &mut BTreeSet<String>| -> Result<(), PlanError> {
        let Some(first) = run.first() else { return Ok(()) };
        let elems: Vec<NewElem> = run.iter().map(|w| w.2.clone()).collect();
        compile_create(cx, op.slot0 + first.0, &elems, out)?;
        for (rel, key, _, intent) in run.drain(..) {
            let id = cx.slots.get(&(op.slot0 + rel)).copied().unwrap_or(0);
            recs.insert(key.clone(), Rec { table: "comp".into(), id, intent, row: 0 });
            fresh.insert(key);
        }
        Ok(())
    };
    for (i, c) in kit.comps.iter().enumerate() {
        let rel = i as u32;
        let ctype = *types.get(&c.family).ok_or_else(|| PlanError(format!("{}: no family {}", c.key, c.family)))?;
        let at = (c.at.x, c.at.y, c.at.z);
        let elem = NewElem::Comp { level: op.level, ctype: type_ref("comp_type", ctype), at, rotation: c.rotation, host: None, scale: 1.0, min_clear: 0.0 };
        let intent = ddc_model::fnv1a(format!("{elem:?}").as_bytes());
        let prev_rec = prev.comps.get(&c.key).cloned();
        let joins = run.last().is_some_and(|l| l.0 + 1 == rel);
        if prev_rec.is_some() || !joins {
            flush(cx, out, &mut run, recs, fresh)?;
        }
        let Some(r) = prev_rec else {
            run.push((rel, c.key.clone(), elem, intent));
            continue;
        };
        let Some(now) = row_hash(&cx.ed.model, &r.table, r.id)? else {
            skip(cx, &r.table, r.id, "deleted by hand after Apply: the new design does not bring it back");
            recs.insert(c.key.clone(), r);
            continue;
        };
        cx.slots.insert(op.slot0 + rel, r.id);
        if now != r.row {
            if intent != r.intent {
                skip(cx, &r.table, r.id, "changed by hand after Apply: kept as it is, the new design would change it");
            }
            recs.insert(c.key.clone(), r);
        } else if intent == r.intent {
            recs.insert(c.key.clone(), r);
        } else {
            push(cx, out, ModelStep::Comp { id: r.id, spec: comp_spec(op.level, ctype, at, c.rotation) })?;
            cx.declared.insert(("comp".to_string(), r.id));
            recs.insert(c.key.clone(), Rec { table: "comp".into(), id: r.id, intent, row: 0 });
            fresh.insert(c.key.clone());
        }
    }
    flush(cx, out, &mut run, recs, fresh)
}

/// The table a stored frame element lives in.
fn frame_table(t: &str) -> &'static str {
    match t {
        "col" => "col",
        "beam" => "beam",
        "strip_footing" => "strip_footing",
        _ => "wall",
    }
}

type Frame = Vec<(u32, String, &'static str, NewElem, u64)>;

/// Columns, beams and walls of the plant: slots follow the placed elements. One that is kept
/// as it is stays; one that changed is replaced; one changed by hand is left alone.
fn sync_frame(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    op: &PlantOp,
    kit: &Kit,
    prev: &Stored,
    recs: &mut BTreeMap<String, Rec>,
    fresh: &mut BTreeSet<String>,
) -> Result<(), PlanError> {
    let mut want: Vec<(String, &'static str, NewElem)> = vec![];
    for c in &kit.cols {
        let elem = NewElem::Column {
            level: op.level,
            top_level: None,
            mtype: None,
            at: (c.a.x, c.a.y),
            base_offset: c.a.z.min(c.b.z),
            top_offset: c.a.z.max(c.b.z),
            rotation: 0.0,
            structural: true,
            top_at: None,
        };
        want.push((c.key.clone(), "col", elem));
    }
    for b in &kit.beams {
        let elem = NewElem::Beam { level: op.level, mtype: None, a: (b.a.x, b.a.y, b.a.z), b: (b.b.x, b.b.y, b.b.z) };
        want.push((b.key.clone(), "beam", elem));
    }
    for w in &kit.walls {
        let elem = NewElem::Wall {
            level: op.level,
            a: (w.a.x, w.a.y),
            b: (w.b.x, w.b.y),
            bulge: 0.0,
            base_offset: w.a.z,
            height: w.height,
            wtype: None,
        };
        want.push((w.key.clone(), "wall", elem));
    }
    let base = kit.comps.len() as u32;
    let asked: BTreeSet<&str> = want.iter().map(|w| w.0.as_str()).chain(kit.footings.iter().map(|f| f.key.as_str())).collect();
    for (k, r) in prev.frame.iter().filter(|(k, _)| !asked.contains(k.as_str())) {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                skip(cx, &r.table, r.id, "changed by hand after Apply: kept although the plant no longer has it");
                recs.insert(k.clone(), r.clone());
            }
            Some(_) => {
                push(cx, out, ModelStep::Delete { table: frame_table(&r.table), id: r.id })?;
                cx.declared.insert((r.table.clone(), r.id));
            }
        }
    }
    let mut run: Frame = vec![];
    let flush = |cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, run: &mut Frame, recs: &mut BTreeMap<String, Rec>, fresh: &mut BTreeSet<String>| -> Result<(), PlanError> {
        let Some(first) = run.first() else { return Ok(()) };
        let elems: Vec<NewElem> = run.iter().map(|w| w.3.clone()).collect();
        compile_create(cx, op.slot0 + first.0, &elems, out)?;
        for (rel, key, table, _, intent) in run.drain(..) {
            let id = cx.slots.get(&(op.slot0 + rel)).copied().unwrap_or(0);
            recs.insert(key.clone(), Rec { table: table.into(), id, intent, row: 0 });
            fresh.insert(key);
        }
        Ok(())
    };
    for (i, (key, table, elem)) in want.into_iter().enumerate() {
        let rel = base + i as u32;
        let intent = ddc_model::fnv1a(format!("{elem:?}").as_bytes());
        let joins = run.last().is_some_and(|l| l.0 + 1 == rel);
        let mut keep = None;
        if let Some(r) = prev.frame.get(&key).cloned() {
            match row_hash(&cx.ed.model, &r.table, r.id)? {
                None => {
                    skip(cx, &r.table, r.id, "deleted by hand after Apply: the new design does not bring it back");
                    keep = Some(r);
                }
                Some(now) if now != r.row => {
                    if intent != r.intent {
                        skip(cx, &r.table, r.id, "changed by hand after Apply: kept as it is, the new design would change it");
                    }
                    keep = Some(r);
                }
                Some(_) if intent == r.intent => keep = Some(r),
                Some(_) => {
                    push(cx, out, ModelStep::Delete { table: frame_table(&r.table), id: r.id })?;
                    cx.declared.insert((r.table.clone(), r.id));
                }
            }
        }
        if let Some(r) = keep {
            flush(cx, out, &mut run, recs, fresh)?;
            recs.insert(key, r);
            continue;
        }
        if !joins {
            flush(cx, out, &mut run, recs, fresh)?;
        }
        run.push((rel, key, table, elem, intent));
    }
    flush(cx, out, &mut run, recs, fresh)
}

/// Strip footings of the plant (a tank's ring), stored with the frame: one kept as it is stays,
/// one that changed is updated in place, one changed or deleted by hand is left alone. Ones
/// the plant no longer has are deleted with the frame.
fn sync_footings(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    op: &PlantOp,
    kit: &Kit,
    prev: &Stored,
    frame: &mut BTreeMap<String, Rec>,
    fresh: &mut BTreeSet<String>,
) -> Result<(), PlanError> {
    for f in &kit.footings {
        let spec = ddc_model::StripFootingSpec {
            level: op.level,
            wall: None,
            a: (f.a.x, f.a.y),
            b: (f.b.x, f.b.y),
            bulge: f.bulge,
            width: f.width,
            depth: f.depth,
            ecc: 0.0,
            top_offset: f.a.z,
            material: "Concrete".into(),
        };
        let intent = ddc_model::fnv1a(format!("{spec:?}").as_bytes());
        if let Some(r) = prev.frame.get(&f.key).cloned() {
            match row_hash(&cx.ed.model, &r.table, r.id)? {
                None => skip(cx, &r.table, r.id, "deleted by hand after Apply: the new design does not bring it back"),
                Some(now) if now != r.row => {
                    if intent != r.intent {
                        skip(cx, &r.table, r.id, "changed by hand after Apply: kept as it is, the new design would change it");
                    }
                }
                Some(_) if intent == r.intent => {}
                Some(_) => {
                    push(cx, out, ModelStep::Update { id: r.id, elem: Box::new(NewResolved::StripFooting(spec)) })?;
                    cx.declared.insert((r.table.clone(), r.id));
                    frame.insert(f.key.clone(), Rec { table: r.table.clone(), id: r.id, intent, row: 0 });
                    fresh.insert(f.key.clone());
                    continue;
                }
            }
            frame.insert(f.key.clone(), r);
            continue;
        }
        let ids = push(cx, out, ModelStep::Add { elem: Box::new(NewResolved::StripFooting(spec)), expect: 0 })?;
        let id = *ids.first().ok_or_else(|| PlanError(format!("{}: the footing was not built", f.key)))?;
        frame.insert(f.key.clone(), Rec { table: "strip_footing".into(), id, intent, row: 0 });
        fresh.insert(f.key.clone());
    }
    Ok(())
}

/// The pipes as one network build: a system per line, the runs, and every connector joined to
/// its line. Kept while its input and the rows it wrote are unchanged; otherwise the stored
/// runs are replaced, except runs changed by hand since.
fn sync_pipes(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    op: &PlantOp,
    p: &Plant,
    prev: &Stored,
) -> Result<Vec<Rec>, PlanError> {
    let base = super::modify_plan::levels(&cx.ed.model)?
        .iter()
        .find(|l| l.0 == op.level)
        .map(|l| l.2)
        .ok_or_else(|| PlanError(format!("no level {}", op.level)))?;
    let mut build = MepBuild::default();
    for l in &p.lines {
        build.systems.push((format!("{} {}", op.pid, l.number()), plant::system_kind(l.svc)));
    }
    for r in &p.kit.runs {
        let l = &p.lines[r.line];
        let pts = r.points.iter().map(|q| (q.x, q.y, base + q.z)).collect();
        let mut spec = RunSpec::pipe(0, op.level, l.spec.material(), r.dn, pts)?;
        spec.section.insulation = l.insul;
        spec.slope = r.fall;
        build.runs.push((SysRef::New(r.line), spec));
    }
    for (i, c) in p.kit.comps.iter().enumerate() {
        let Some(&id) = cx.slots.get(&(op.slot0 + i as u32)) else { continue };
        for (name, line) in &c.ports {
            build.links.push((id, name.clone(), SysRef::New(*line)));
        }
    }
    let intent = ddc_model::fnv1a(format!("{:?}|{:?}|{:?}", build.systems, build.runs, build.links).as_bytes());
    let mut kept = !prev.runs.is_empty() && prev.runs.len() == build.runs.len();
    for r in &prev.runs {
        kept &= r.intent == intent && row_hash(&cx.ed.model, &r.table, r.id)?.is_some_and(|now| now == r.row);
    }
    if kept {
        return Ok(prev.runs.clone());
    }
    let mut recs = vec![];
    for r in &prev.runs {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                skip(cx, &r.table, r.id, "changed by hand after Apply: kept although the new design rebuilds the pipes");
                recs.push(r.clone());
            }
            Some(_) => {
                push(cx, out, ModelStep::Delete { table: "mep_run", id: r.id })?;
                cx.declared.insert((r.table.clone(), r.id));
            }
        }
    }
    if build.is_empty() {
        return Ok(recs);
    }
    // the links read the placed connectors: settle the quantities the item holds back
    cx.ed.model.settle_derived()?;
    cx.ed.model.hold_derived();
    push(cx, out, ModelStep::Mep { build: Box::new(build), expect: vec![] })?;
    let Some(ModelStep::Mep { expect, .. }) = out.last() else {
        return Err("the pipe network was not built".into());
    };
    let expect = expect.clone();
    for (t, id) in &expect {
        cx.declared.insert((t.to_string(), *id));
    }
    for (_, id) in expect.iter().filter(|k| k.0 == "mep_run") {
        let row = row_hash(&cx.ed.model, "mep_run", *id)?.unwrap_or(0);
        recs.push(Rec { table: "mep_run".into(), id: *id, intent, row });
    }
    Ok(recs)
}

fn prop(key: &str, value: Option<Value>) -> PropSet {
    PropSet { kind: RECIPE.to_string(), id: 0, key: key.to_string(), value: value.map(|v| (v, String::new())) }
}

/// Families, the placed elements, the pipes, then the stored record of every element.
pub fn compile(cx: &mut CompileCx<'_>, op: &PlantOp, out: &mut Vec<ModelStep>) -> Result<(), PlanError> {
    let p = design(&op.input).map_err(PlanError)?;
    let placed = p.kit.comps.len() + p.kit.cols.len() + p.kit.beams.len() + p.kit.walls.len() + p.kit.footings.len();
    if placed > SLOTS as usize {
        return Err(PlanError(format!("the plant places {placed} elements, at most {SLOTS} in one item")));
    }
    let prev = load(&cx.ed.model, &op.pid)?;
    // a plant moved to another level is built there anew; the old one is left to the person
    let prev = if prev.head["level"].as_i64().is_some_and(|l| l != op.level) { Stored::default() } else { prev };
    let types = families(cx, out, &op.pid, &p.kit)?;
    let mut recs = BTreeMap::new();
    let mut fresh = BTreeSet::new();
    // components keep their ids where they can, so an unchanged plant keeps its runs too
    sync_comps(cx, out, op, &p.kit, &types, &prev, &mut recs, &mut fresh)?;
    let mut frame = BTreeMap::new();
    sync_frame(cx, out, op, &p.kit, &prev, &mut frame, &mut fresh)?;
    sync_footings(cx, out, op, &p.kit, &prev, &mut frame, &mut fresh)?;
    let runs = sync_pipes(cx, out, op, &p, &prev)?;
    for k in &fresh {
        if let Some(r) = recs.get_mut(k).or_else(|| frame.get_mut(k)) {
            r.row = row_hash(&cx.ed.model, &r.table, r.id)?.unwrap_or(0);
        }
    }
    let head = json!({
        "input": op.input,
        "level": op.level,
        "comps": recs.iter().map(|(k, r)| (k.clone(), r.to_json())).collect::<serde_json::Map<String, J>>(),
        "runs": runs.iter().map(Rec::to_json).collect::<Vec<_>>(),
        "frame": frame.iter().map(|(k, r)| (k.clone(), r.to_json())).collect::<serde_json::Map<String, J>>(),
    });
    if head != prev.head {
        push(cx, out, ModelStep::Props(vec![prop(&format!("{}|spec", op.pid), Some(Value::Text(head.to_string())))]))?;
    }
    Ok(())
}

/// What the reply says about a plant: kind, hall, counts, checks, totals.
pub fn summary(p: &Plant) -> J {
    let all = p.to_json();
    let r = &p.report;
    json!({
        "kind": all["kind"],
        "hall": all["hall"],
        "elements": {"units": p.equipment.len(), "placed": p.kit.comps.len(), "families": p.kit.families.len(),
                     "columns": p.kit.cols.len(), "beams": p.kit.beams.len(), "walls": p.kit.walls.len(),
                     "footings": p.kit.footings.len(),
                     "pipe_runs": p.kit.runs.len(), "lines": p.lines.len()},
        "report": {"ok": r.ok, "nozzles": r.nozzles, "connected": r.connected, "valves": r.valves,
                   "valves_in_reach": r.valves_in_reach, "min_margin_m": r.min_margin},
        "totals": all["schedules"]["totals"],
    })
}
