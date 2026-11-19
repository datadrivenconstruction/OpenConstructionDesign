// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Executor for `ddc-ops` envelopes (docs/design/AI_NATIVE.md section 2, "Lowering").
//!
//! `apply_envelope` lowers every op to an existing model function inside `Model::atomic`: the
//! whole envelope is one transaction and one journal entry, so a failing op in any group leaves
//! the model as it was, and one undo removes all of it. No geometry code lives here; op kinds
//! without a model function yet are rejected by name, never approximated.

use std::collections::HashMap;
use std::time::Duration;

use ddc_ops::{
    Cat, Dim, End, Envelope, Field, GridOp, LevelOp, NewEl, Op, Place, Ref, Targets,
    Xform,
};
use rusqlite::backup::Backup;
use rusqlite::{Connection, OptionalExtension};

use crate::{
    Error, GridSpec, Lower, Model, OpeningSpec, OptionRec, OptionStatus, Replay, Target, WallSpec,
};

/// Journal kind of an applied envelope.
pub const ENVELOPE_KIND: &str = "ops_envelope";
const EPS: f64 = 1e-9;
const COPY_PAGES: i32 = 4096;

/// What an applied envelope did. Refs are `cat:id` of rows in the model after the apply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Receipt {
    /// Slot of the creating op and the row it made, in op order.
    pub created: Vec<(u32, Ref)>,
    /// Rows made without a slot: copies of `transform` and `pattern`, the tool body and the
    /// result of a boolean, the body of a void. The last three also stay in `changed`.
    pub copied: Vec<Ref>,
    pub changed: Vec<Ref>,
    pub deleted: Vec<Ref>,
    /// `what: measure` lines of everything a conversion dropped or approximated; empty: lossless.
    pub loss: Vec<String>,
    /// The single journal entry holding the envelope (undo removes exactly this).
    pub journal_seq: i64,
}

/// Why nothing was applied: the one rejection of the ops crate (stable code, path, refs).
pub use ddc_ops::Rejection;
use ddc_ops::Code;

fn model_rejection(e: &Error) -> Rejection {
    e.rejection()
}

/// Why the user's Apply of an option recipe changed nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionApplyError {
    Missing,
    /// Already accepted (its Apply is in the journal) or rejected.
    NotDraft(OptionStatus),
    /// The model moved past the recipe's base: the option must be regenerated.
    Stale,
    /// The stored envelope does not parse.
    Envelope(String),
    Rejected(Rejection),
    Model(String),
}

/// Failure of one op before it becomes a `Rejection`.
enum Fail {
    Code(Code, String),
    /// Like `Code`, at a path inside the op (`/params/2`).
    At(String, Code, String),
    Model(Error),
    /// The failure lies inside the op at this JSON pointer suffix (`/fields/2`).
    Under(String, Box<Fail>),
}

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        Fail::Model(e)
    }
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        Fail::Model(e.into())
    }
}

impl Fail {
    /// The rejection with its path relative to the op.
    fn rejection(self) -> Rejection {
        match self {
            Fail::Code(c, s) => Rejection::new(c).got(s),
            Fail::At(p, c, s) => Rejection::new(c).got(s).field(&p),
            Fail::Model(e) => model_rejection(&e),
            Fail::Under(p, f) => f.rejection().under(&p),
        }
    }

    fn at(self, suffix: impl Into<String>) -> Fail {
        Fail::Under(suffix.into(), Box::new(self))
    }
}

fn unsupported(s: impl Into<String>) -> Fail {
    Fail::Code(Code::Unsupported, s.into())
}

fn bad_ref(s: impl Into<String>) -> Fail {
    Fail::Code(Code::NotFound, s.into())
}

fn bad_value(s: impl Into<String>) -> Fail {
    Fail::Code(Code::Invalid, s.into())
}

/// Slots resolved so far and the receipt being built.
#[derive(Default)]
struct Run {
    slots: HashMap<u32, (Cat, i64)>,
    receipt: Receipt,
}

impl Run {
    fn made(&mut self, slot: u32, cat: Cat, id: i64) {
        self.slots.insert(slot, (cat, id));
        self.receipt.created.push((slot, Ref::id(cat, id)));
    }
}

impl Model {
    /// Applies the envelope all-or-nothing as one journal entry (one undo step).
    pub fn apply_envelope(&mut self, env: &Envelope) -> Result<Receipt, Rejection> {
        let issues = env.check();
        if let Some(first) = issues.first() {
            return Err(Rejection::from_issue(env, first));
        }
        let mut failed: Option<Rejection> = None;
        // actor, title, why and the forward envelope go into the same journal entry
        let prov = crate::journal::Provenance::of(env);
        let res = self.atomic_with(ENVELOPE_KIND, &prov, |m| {
            let mut run = Run::default();
            for (gi, g) in env.groups.iter().enumerate() {
                for (oi, op) in g.ops.iter().enumerate() {
                    if let Err(f) = lower(m, &mut run, op) {
                        failed = Some(f.rejection().in_op(env, gi, oi));
                        return Err(Error::Invalid("envelope rejected".into()));
                    }
                }
            }
            Ok(run.receipt)
        });
        match res {
            Ok((mut r, seq)) => {
                r.journal_seq = seq;
                tidy(&mut r);
                Ok(r)
            }
            Err(e) => Err(failed.unwrap_or_else(|| model_rejection(&e))),
        }
    }

    /// A plan step's envelope: its own undo step on a scratch copy (no transaction open), inside
    /// the Apply transaction otherwise.
    pub fn apply_envelope_step(&mut self, env: &Envelope) -> Result<Receipt, Rejection> {
        if self.db.is_autocommit() {
            self.apply_envelope(env)
        } else {
            self.lower_envelope_in_atomic(env)
        }
    }

    /// Lowers the envelope inside the transaction the caller holds (`atomic_in`), writing no
    /// journal entry of its own: the caller's entry holds it (design option Apply). Refused
    /// outside a transaction, where a failing op would leave earlier ops on disk.
    pub fn lower_envelope_in_atomic(&mut self, env: &Envelope) -> Result<Receipt, Rejection> {
        if self.db.is_autocommit() {
            return Err(Rejection::new(Code::Internal).got("lower_envelope_in_atomic: no open transaction"));
        }
        if let Some(first) = env.check().first() {
            return Err(Rejection::from_issue(env, first));
        }
        let mut run = Run::default();
        for (gi, g) in env.groups.iter().enumerate() {
            for (oi, op) in g.ops.iter().enumerate() {
                lower(self, &mut run, op).map_err(|f| f.rejection().in_op(env, gi, oi))?;
            }
        }
        let mut r = run.receipt;
        tidy(&mut r);
        Ok(r)
    }

    /// The user's Apply of an option recipe: `ai_option_apply` (one `atomic_in` entry with the
    /// accepted mark and the AI provenance) around `lower_envelope_in_atomic`; one undo step.
    pub fn ai_option_apply_ops(&mut self, id: i64) -> Result<Receipt, OptionApplyError> {
        let model = |e: Error| OptionApplyError::Model(e.to_string());
        let rec = self.ai_option_get(id).map_err(model)?.ok_or(OptionApplyError::Missing)?;
        if rec.status != OptionStatus::Draft {
            return Err(OptionApplyError::NotDraft(rec.status));
        }
        if self.ai_option_head().map_err(model)? != (rec.base_head, rec.base_stamp) {
            return Err(OptionApplyError::Stale);
        }
        self.option_lowered(|m, lower| m.ai_option_apply(id, lower)).map(|(r, ())| r)
    }

    /// [`Model::ai_option_apply_ops`] of a recipe not in this model yet: every gate runs before
    /// anything is written and the row goes in with the Apply step, so a refusal leaves the model
    /// as it was. Returns the receipt and the new row's id.
    pub fn ai_option_apply_new_ops(
        &mut self,
        rec: &OptionRec,
    ) -> Result<(Receipt, i64), OptionApplyError> {
        let model = |e: Error| OptionApplyError::Model(e.to_string());
        if rec.status != OptionStatus::Draft {
            return Err(OptionApplyError::NotDraft(rec.status));
        }
        if self.ai_option_head().map_err(model)? != (rec.base_head, rec.base_stamp) {
            return Err(OptionApplyError::Stale);
        }
        Envelope::from_json(&rec.envelope).map_err(|e| OptionApplyError::Envelope(e.to_string()))?;
        self.option_lowered(|m, lower| m.ai_option_apply_new(rec, lower))
    }

    /// Runs `apply` with the lowering of the recipe envelope and maps its outcome.
    fn option_lowered<T>(
        &mut self,
        apply: impl FnOnce(&mut Model, &mut Lower<'_>) -> Result<T, Error>,
    ) -> Result<(Receipt, T), OptionApplyError> {
        let model = |e: Error| OptionApplyError::Model(e.to_string());
        // read before the write: a failed read after the commit must not turn it into an error
        let before = self.op_top().map_err(model)?;
        let mut out: Option<Result<Receipt, OptionApplyError>> = None;
        let mut low = |m: &mut Model, rp: &Replay<'_>| -> Result<(), Error> {
            let env = match Envelope::from_json(rp.envelope) {
                Ok(e) => e,
                Err(e) => {
                    out = Some(Err(OptionApplyError::Envelope(e.to_string())));
                    return Err(Error::Invalid("option envelope unreadable".into()));
                }
            };
            match m.lower_envelope_in_atomic(&env) {
                Ok(r) => {
                    out = Some(Ok(r));
                    Ok(())
                }
                Err(r) => {
                    out = Some(Err(OptionApplyError::Rejected(r)));
                    Err(Error::Invalid("envelope rejected".into()))
                }
            }
        };
        let lower: &mut Lower<'_> = &mut low;
        let res = apply(self, lower);
        match (res, out) {
            (Ok(v), Some(Ok(mut r))) => {
                // committed as one merged entry: `before + 1` when the journal cannot be read
                r.journal_seq = self.op_top().unwrap_or(before + 1);
                Ok((r, v))
            }
            (Err(_), Some(Err(e))) => Err(e),
            (Err(e), _) => Err(model(e)),
            (Ok(_), _) => Err(OptionApplyError::Model("option apply gave no receipt".into())),
        }
    }

    /// Applies the envelope to an in-memory copy: the same receipt (ids, journal seq) the real
    /// apply would give, the model untouched. Large files should go through `PlanCopies`.
    pub fn dry_run_envelope(&self, env: &Envelope) -> Result<Receipt, Rejection> {
        let mut copy = self
            .memory_copy()
            .map_err(|e| Rejection::new(Code::Internal).got(format!("dry run copy: {e}")))?;
        copy.apply_envelope(env)
    }

    /// Row-for-row copy of the model in memory (online backup, the source keeps working).
    pub(crate) fn memory_copy(&self) -> Result<Model, Error> {
        self.worker_copy()?.open()
    }

    /// In-memory copy to hand to another thread; the worker opens it there with
    /// [`WorkerCopy::open`] (the model itself stays on its own thread).
    pub fn worker_copy(&self) -> Result<WorkerCopy, Error> {
        let mut db = Connection::open_in_memory()?;
        {
            let b = Backup::new(&self.db, &mut db)?;
            b.run_to_completion(COPY_PAGES, Duration::ZERO, None)?;
        }
        Ok(WorkerCopy(db))
    }
}

/// A row-for-row copy of a model not yet opened: a bare connection, safe to move to a worker.
pub struct WorkerCopy(pub(crate) Connection);

impl WorkerCopy {
    pub fn open(self) -> Result<Model, Error> {
        Model::adopt_conn(self.0, None, false)
    }
}

/// Lists without repeats, first occurrence kept; a deleted row is not also reported changed.
fn tidy(r: &mut Receipt) {
    fn dedup(v: &mut Vec<Ref>) {
        let mut out: Vec<Ref> = Vec::with_capacity(v.len());
        for x in v.drain(..) {
            if !out.contains(&x) {
                out.push(x);
            }
        }
        *v = out;
    }
    dedup(&mut r.copied);
    dedup(&mut r.changed);
    dedup(&mut r.deleted);
    let deleted = r.deleted.clone();
    r.changed.retain(|x| !deleted.contains(x));
    r.copied.retain(|x| !deleted.contains(x));
}

fn lower(m: &mut Model, run: &mut Run, op: &Op) -> Result<(), Fail> {
    match op {
        Op::Create { slot, el } => create(m, run, *slot, el),
        Op::Set { target, fields } => set::set_fields(m, run, target, fields),
        Op::SetType { target, to } => set_type(m, run, target, to),
        Op::Delete { target } => delete(m, run, target),
        Op::Transform { target, xf, copy } => transform(m, run, target, xf, *copy),
        Op::Level(l) => level(m, run, l),
        Op::Grid(g) => grid(m, run, g),
        Op::Geometry(g) => ops_geo::lower(m, run, g),
        Op::Pattern { source, pattern, out } => ops_pattern::pattern(m, run, source, pattern, *out),
        Op::Family(f) => ops_pattern::family(m, run, f),
        Op::Type(t) => set::type_op(m, run, t),
        other => Err(unsupported(format!(
            "op kind `{}` is not lowered by the executor yet",
            other.kind()
        ))),
    }
}

// ---- refs ----

/// Table of rows a `cat:id` ref names. `Type` (v1) reads as a wall type.
fn table(cat: Cat) -> Option<&'static str> {
    Some(match cat {
        Cat::Wall => "wall",
        Cat::Door | Cat::Window | Cat::Opening => "opening",
        Cat::CurtainWall => "curtain_wall",
        Cat::Slab => "slab",
        Cat::Room => "room",
        Cat::Component => "comp",
        Cat::MepRun => "mep_run",
        Cat::Solid => "solid",
        Cat::Mesh => "mesh",
        Cat::Column => "col",
        Cat::Beam => "beam",
        Cat::Ceiling => "ceiling",
        Cat::Ramp => "ramp",
        Cat::Roof => "roof",
        Cat::Shaft => "shaft",
        Cat::Grid => "grid",
        Cat::RefPlane => "ref_plane",
        Cat::Footing => "iso_footing",
        Cat::StripFooting => "strip_footing",
        Cat::Raft => "raft",
        Cat::Brace => "brace",
        Cat::Truss => "truss",
        Cat::BeamSystem => "beam_system",
        Cat::Mass => "mass",
        Cat::Topography => "toposolid",
        Cat::Freeform => "freeform",
        Cat::PanelSystem => "panel_system",
        Cat::SurfaceShell => "shell_elem",
        Cat::Gridshell => "gridshell",
        Cat::Level => "level",
        Cat::Type | Cat::WallType => "wall_type",
        Cat::SlabType => "slab_type",
        Cat::MemberType => "member_type",
        _ => return None,
    })
}

/// The table a `cat:id` ref names, for readers of a receipt.
pub fn ref_table(cat: Cat) -> Option<&'static str> {
    table(cat)
}

/// Tables whose `name` column is unique and may be used by `Ref::Name`.
fn named_table(cat: Cat) -> Option<&'static str> {
    match cat {
        Cat::Level => Some("level"),
        Cat::Grid => Some("grid"),
        Cat::Type | Cat::WallType => Some("wall_type"),
        Cat::SlabType => Some("slab_type"),
        Cat::MemberType => Some("member_type"),
        _ => None,
    }
}

fn is_opening(c: Cat) -> bool {
    matches!(c, Cat::Door | Cat::Window | Cat::Opening)
}

fn is_wall_type(c: Cat) -> bool {
    matches!(c, Cat::Type | Cat::WallType)
}

fn same_cat(a: Cat, b: Cat) -> bool {
    a == b || (is_opening(a) && is_opening(b)) || (is_wall_type(a) && is_wall_type(b))
}

fn row_exists(db: &Connection, table: &str, id: i64) -> Result<bool, Fail> {
    let hit: Option<i64> = db
        .query_row(&format!("SELECT 1 FROM {table} WHERE id=?1"), [id], |r| r.get(0))
        .optional()?;
    Ok(hit.is_some())
}

fn resolve(m: &Model, run: &Run, r: &Ref) -> Result<(Cat, i64), Fail> {
    match r {
        Ref::Id { cat, id } => {
            let t = table(*cat)
                .ok_or_else(|| unsupported(format!("refs to `{}` are not resolved yet", cat.key())))?;
            if !row_exists(&m.db, t, *id)? {
                return Err(bad_ref(format!("{} does not exist", r.label())));
            }
            Ok((*cat, *id))
        }
        Ref::New { slot } => run
            .slots
            .get(slot)
            .copied()
            .ok_or_else(|| bad_ref(format!("slot {slot} was not created by an earlier op"))),
        Ref::Name { cat, name } => {
            let t = named_table(*cat)
                .ok_or_else(|| unsupported(format!("`{}` is not looked up by name", cat.key())))?;
            let mut st = m.db.prepare(&format!("SELECT id FROM {t} WHERE name=?1 LIMIT 2"))?;
            let ids: Vec<i64> = st
                .query_map([name], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            match ids.as_slice() {
                [id] => Ok((*cat, *id)),
                [] => Err(bad_ref(format!("no {} named \"{name}\"", cat.key()))),
                _ => Err(bad_ref(format!("several {} rows named \"{name}\"", cat.key()))),
            }
        }
        Ref::Set { name } => Err(unsupported(format!(
            "selection set `{name}` is not resolved by the executor yet"
        ))),
        Ref::Part { .. } => Err(unsupported(format!(
            "part ref {} needs the placement solver",
            r.label()
        ))),
    }
}

/// Resolves and checks the category; returns the row id.
fn resolve_as(m: &Model, run: &Run, r: &Ref, want: Cat) -> Result<i64, Fail> {
    let (cat, id) = resolve(m, run, r)?;
    if !same_cat(cat, want) {
        return Err(bad_ref(format!("{} is not a {}", r.label(), want.key())));
    }
    Ok(id)
}

fn targets(m: &Model, run: &Run, t: &Targets) -> Result<Vec<(Cat, i64)>, Fail> {
    match t {
        Targets::Refs { refs } => {
            if refs.is_empty() {
                return Err(bad_value("no targets"));
            }
            refs.iter().map(|r| resolve(m, run, r)).collect()
        }
        Targets::Set { name } => Err(unsupported(format!(
            "selection set `{name}` is not resolved by the executor yet"
        ))),
        Targets::Filter { .. } => Err(unsupported("filter targets are not resolved by the executor yet")),
    }
}

fn target_of(cat: Cat, id: i64) -> Option<Target> {
    Some(match cat {
        Cat::Wall => Target::Wall(id),
        Cat::Door | Cat::Window | Cat::Opening => Target::Opening(id),
        Cat::CurtainWall => Target::Curtain(id),
        Cat::Slab => Target::Slab(id),
        Cat::Room => Target::Room(id),
        Cat::Component => Target::Component(id),
        Cat::MepRun => Target::MepRun(id),
        Cat::Solid => Target::Solid(id),
        Cat::Mesh => Target::Mesh(id),
        Cat::Column => Target::Column(id),
        Cat::Beam => Target::Beam(id),
        Cat::Ceiling => Target::Ceiling(id),
        Cat::Ramp => Target::Ramp(id),
        Cat::Roof => Target::Roof(id),
        Cat::Shaft => Target::Shaft(id),
        Cat::Grid => Target::Grid(id),
        Cat::RefPlane => Target::RefPlane(id),
        Cat::Footing => Target::Footing(id),
        Cat::StripFooting => Target::StripFooting(id),
        Cat::Raft => Target::Raft(id),
        Cat::Brace => Target::Brace(id),
        Cat::Truss => Target::Truss(id),
        Cat::BeamSystem => Target::BeamSystem(id),
        Cat::Mass => Target::Mass(id),
        Cat::Topography => Target::Topo(id),
        Cat::Zone => Target::Zone(id),
        Cat::Stair => Target::Stair(id),
        Cat::Railing => Target::Railing(id),
        _ => return None,
    })
}

fn ref_of(t: Target) -> Ref {
    let (cat, id) = match t {
        Target::Wall(i) => (Cat::Wall, i),
        Target::Opening(i) => (Cat::Opening, i),
        Target::Curtain(i) => (Cat::CurtainWall, i),
        Target::Slab(i) => (Cat::Slab, i),
        Target::Room(i) => (Cat::Room, i),
        Target::RoomSep(i) | Target::View(i) | Target::Dim(i) | Target::Anno(i) => {
            (Cat::Annotation, i)
        }
        Target::Component(i) => (Cat::Component, i),
        Target::MepRun(i) => (Cat::MepRun, i),
        Target::Solid(i) => (Cat::Solid, i),
        Target::Mesh(i) => (Cat::Mesh, i),
        Target::Column(i) => (Cat::Column, i),
        Target::Beam(i) => (Cat::Beam, i),
        Target::Ceiling(i) => (Cat::Ceiling, i),
        Target::Ramp(i) => (Cat::Ramp, i),
        Target::Roof(i) => (Cat::Roof, i),
        Target::Shaft(i) => (Cat::Shaft, i),
        Target::Grid(i) => (Cat::Grid, i),
        Target::RefPlane(i) => (Cat::RefPlane, i),
        Target::Footing(i) => (Cat::Footing, i),
        Target::StripFooting(i) => (Cat::StripFooting, i),
        Target::Raft(i) => (Cat::Raft, i),
        Target::Brace(i) => (Cat::Brace, i),
        Target::Truss(i) => (Cat::Truss, i),
        Target::BeamSystem(i) => (Cat::BeamSystem, i),
        Target::Mass(i) => (Cat::Mass, i),
        Target::Topo(i) => (Cat::Topography, i),
        Target::Zone(i) => (Cat::Zone, i),
        Target::Stair(i) => (Cat::Stair, i),
        Target::Railing(i) => (Cat::Railing, i),
    };
    Ref::id(cat, id)
}

// ---- values ----

fn place_name(p: &Place) -> &'static str {
    match p {
        Place::At { .. } => "at",
        Place::Offset { .. } => "offset",
        Place::OnHost { .. } => "on_host",
        Place::Crossing { .. } => "crossing",
        Place::Center { .. } => "center",
        Place::AlignTo { .. } => "align_to",
        Place::Between { .. } => "between",
        Place::Snap { .. } => "snap",
    }
}

/// Absolute point in metres; intent placements wait for the placement solver.
fn at_point(p: &Place) -> Result<[f64; 3], Fail> {
    match p {
        Place::At { p } => Ok(p.to_m()),
        other => Err(unsupported(format!(
            "placement `{}` needs the placement solver",
            place_name(other)
        ))),
    }
}

fn el_name(el: &NewEl) -> &'static str {
    match el {
        NewEl::Wall { .. } => "wall",
        NewEl::Door { .. } => "door",
        NewEl::Window { .. } => "window",
        NewEl::Opening { .. } => "opening",
        NewEl::Slab { .. } => "slab",
        NewEl::Roof { .. } => "roof",
        NewEl::Ceiling { .. } => "ceiling",
        NewEl::Column { .. } => "column",
        NewEl::Beam { .. } => "beam",
        NewEl::Stair { .. } => "stair",
        NewEl::Railing { .. } => "railing",
        NewEl::Room { .. } => "room",
        NewEl::Component { .. } => "component",
        NewEl::MepRun { .. } => "mep_run",
        NewEl::Generic { .. } => "generic",
    }
}

fn no_fields<'a>(fields: impl IntoIterator<Item = &'a Field>) -> Result<(), Fail> {
    let names: Vec<&str> = fields.into_iter().map(|f| f.name.as_str()).collect();
    if names.is_empty() {
        return Ok(());
    }
    Err(unsupported(format!(
        "extra fields on create are not lowered yet: {}",
        names.join(", ")
    )))
}

/// Create-time wall fields: `bulge`, a plain number (0 = straight axis), and the lean
/// (`lean` angle, `top_scale` plain number), set right after the wall is made.
fn wall_create_bulge(m: &Model, fields: &[Field]) -> Result<(f64, Option<crate::Lean>), Fail> {
    let (shape, rest): (Vec<&Field>, Vec<&Field>) =
        fields.iter().partition(|f| f.name == "lean" || f.name == "top_scale");
    let lean = set::lean_fields(m, None, &shape)?;
    let (bulge, rest): (Vec<&Field>, Vec<&Field>) =
        rest.into_iter().partition(|f| f.name == "bulge");
    no_fields(rest)?;
    Ok((wall_bulge_of(&bulge)?, lean))
}

fn wall_bulge_of(bulge: &[&Field]) -> Result<f64, Fail> {
    match bulge {
        [] => Ok(0.0),
        [f] => match (f.value.unit, f.value.v.as_f64()) {
            (None, Some(x)) if x.is_finite() => Ok(x),
            _ => Err(bad_value("`bulge` needs a plain number without a unit")),
        },
        _ => Err(bad_value("`bulge` is given more than once")),
    }
}

fn text_of(f: &Field) -> Result<String, Fail> {
    match (f.value.v.as_str(), f.value.unit) {
        (Some(s), None) if !s.trim().is_empty() => Ok(s.to_owned()),
        _ => Err(bad_value(format!("`{}` needs non-empty text without a unit", f.name))),
    }
}

fn length_of(f: &Field) -> Result<f64, Fail> {
    match (f.value.unit, f.value.si()) {
        (Some(u), Some(v)) if u.dim() == Dim::Length && v.is_finite() => Ok(v),
        _ => Err(bad_value(format!("`{}` needs a length with a unit", f.name))),
    }
}

fn level_row(m: &Model, id: i64) -> Result<(String, f64), Fail> {
    Ok(m.db.query_row("SELECT name, elevation FROM level WHERE id=?1", [id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?)
}

// ---- ops ----

fn create(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    match el {
        NewEl::Wall { level, a, b, height, top, ty, fields } => {
            if top.is_some() {
                return Err(unsupported("wall `top` level is not lowered yet; give `height` only"));
            }
            if ops_spline::is_spline(fields) {
                return ops_spline::create(m, run, slot, level, (a, b), height, ty.as_ref(), fields);
            }
            let (bulge, lean) = wall_create_bulge(m, fields)?;
            let lv = resolve_as(m, run, level, Cat::Level)?;
            let (_, elev) = level_row(m, lv)?;
            let (pa, pb) = (at_point(a)?, at_point(b)?);
            if (pa[2] - pb[2]).abs() > EPS {
                return Err(bad_value("wall ends have different z; sloped bases are not supported"));
            }
            let wall_type = match ty {
                Some(t) => resolve_as(m, run, t, Cat::WallType)?,
                None => default_wall_type(&m.db)?,
            };
            // z is absolute (project coordinates): the base offset is measured from the level
            let spec = WallSpec {
                a: (pa[0], pa[1]),
                b: (pb[0], pb[1]),
                level: lv,
                base_offset: pa[2] - elev,
                height: height.to_m(),
                wall_type,
                offset0: None,
            };
            let id = if bulge != 0.0 { m.add_arc_wall(&spec, bulge)? } else { m.add_wall(&spec)? };
            if let Some(l) = lean {
                m.set_wall_lean(id, l)?;
            }
            run.made(slot, Cat::Wall, id);
            Ok(())
        }
        NewEl::Opening { host, at, w, h, sill } => {
            let wall = resolve_as(m, run, host, Cat::Wall)?;
            let width = w.to_m();
            let along = along_host(m, run, wall, at, width, sill.to_m())?;
            let spec = OpeningSpec {
                along,
                width,
                sill: sill.to_m(),
                height: h.to_m(),
                kind: "void".into(),
            };
            m.opening_fits(wall, &spec)?;
            let id = m.add_opening(wall, &spec)?;
            run.made(slot, Cat::Opening, id);
            Ok(())
        }
        other => ops_create::create(m, run, slot, other),
    }
}

fn default_wall_type(db: &Connection) -> Result<i64, Fail> {
    let id: Option<i64> = db.query_row("SELECT min(id) FROM wall_type", [], |r| r.get(0))?;
    id.ok_or_else(|| bad_value("the model has no wall type; give `type`"))
}

/// Start of the opening along its wall. The wire `along` places the opening centre (a place
/// is a point), measured from the named end; the model stores the start. `z` is 0 or the sill.
fn along_host(m: &Model, run: &Run, wall: i64, at: &Place, width: f64, sill: f64) -> Result<f64, Fail> {
    match at {
        Place::OnHost { host, along, from_end, side: _, z } => {
            if resolve_as(m, run, host, Cat::Wall)? != wall {
                return Err(bad_ref(format!("placement host {} is not the opening host", host.label())));
            }
            let z = z.to_m();
            if z.abs() > EPS && (z - sill).abs() > EPS {
                return Err(bad_value("opening height comes from `sill`; `z` must be 0 or the sill"));
            }
            let c = along.to_m();
            let centre = match from_end {
                End::A => c,
                End::B => m.wall_length(wall)? - c,
            };
            Ok(centre - width / 2.0)
        }
        other => Err(unsupported(format!(
            "opening placement `{}` is not lowered yet; use `on_host`",
            place_name(other)
        ))),
    }
}

fn set_type(m: &mut Model, run: &mut Run, t: &Targets, to: &Ref) -> Result<(), Fail> {
    let (ty_cat, ty) = resolve(m, run, to)?;
    let all = targets(m, run, t)?;
    // every target must take this type before the first write
    let mut walls = vec![];
    let mut slabs = vec![];
    let mut cols = vec![];
    let mut beams = vec![];
    for &(cat, id) in &all {
        let slot = match (cat, ty_cat) {
            (Cat::Wall, Cat::Type | Cat::WallType) => &mut walls,
            (Cat::Slab, Cat::SlabType) => &mut slabs,
            (Cat::Column, Cat::MemberType) => &mut cols,
            (Cat::Beam, Cat::MemberType) => &mut beams,
            (Cat::Wall | Cat::Slab | Cat::Column | Cat::Beam, _) => {
                return Err(bad_value(format!(
                    "{} cannot take type {}",
                    Ref::id(cat, id).label(),
                    to.label()
                )))
            }
            _ => {
                return Err(unsupported(format!(
                    "`set_type` is not lowered for {} yet",
                    Ref::id(cat, id).label()
                )))
            }
        };
        slot.push(id);
    }
    let (plain, splines) = ops_spline::split_walls(m, &walls)?;
    for &id in &plain {
        m.set_wall_type(id, ty)?;
    }
    for s in splines {
        ops_spline::set_type(m, s, ty)?;
    }
    if !slabs.is_empty() {
        m.set_elements_type("slab", &slabs, ty)?;
    }
    if !cols.is_empty() {
        m.set_elements_type("col", &cols, ty)?;
    }
    if !beams.is_empty() {
        m.set_elements_type("beam", &beams, ty)?;
    }
    run.receipt.changed.extend(all.into_iter().map(|(c, id)| Ref::id(c, id)));
    Ok(())
}

fn delete(m: &mut Model, run: &mut Run, t: &Targets) -> Result<(), Fail> {
    let all = targets(m, run, t)?;
    let (mut walls, mut grids, mut openings, mut meshes) = (vec![], vec![], vec![], vec![]);
    let (mut solids, mut masses, mut freeforms) = (vec![], vec![], vec![]);
    let (mut panels, mut shells, mut lattices) = (vec![], vec![], vec![]);
    for &(cat, id) in &all {
        match cat {
            Cat::Wall => walls.push(id),
            Cat::Grid => grids.push(id),
            c if is_opening(c) => openings.push(id),
            Cat::Mesh => meshes.push(id),
            Cat::Solid => solids.push(id),
            Cat::Mass => masses.push(id),
            Cat::Freeform => freeforms.push(id),
            Cat::PanelSystem => panels.push(id),
            Cat::SurfaceShell => shells.push(id),
            Cat::Gridshell => lattices.push(id),
            _ => {
                return Err(unsupported(format!(
                    "`delete` is not lowered for {} yet",
                    Ref::id(cat, id).label()
                )))
            }
        }
    }
    // a piece of a spline wall deletes the whole spline wall
    let (plain, splines) = ops_spline::split_walls(m, &walls)?;
    // openings first: deleting a wall takes its openings with it
    for &id in &openings {
        let host: i64 = m.db.query_row("SELECT wall FROM opening WHERE id=?1", [id], |r| r.get(0))?;
        let gone = walls.contains(&host) || m.spline_of_wall(host)?.is_some_and(|s| splines.contains(&s));
        if !gone {
            m.delete_opening(id)?;
        }
    }
    for &s in &splines {
        m.delete_spline_wall(s)?;
    }
    if !plain.is_empty() {
        m.delete_walls(&plain)?;
    }
    for &id in &grids {
        m.delete_grid(id)?;
    }
    if !meshes.is_empty() {
        m.delete_meshes(&meshes)?;
    }
    for &id in &solids {
        m.delete_solid(id)?;
    }
    for &id in &masses {
        m.delete_mass(id)?;
    }
    // panel systems before their hosts: deleting a host takes its systems with it
    for &id in &panels {
        m.delete_panel_system(id)?;
    }
    for &id in &shells {
        m.delete_shell_elem(id)?;
    }
    for &id in &lattices {
        m.delete_struct("gridshell", id)?;
    }
    // roofs and slabs on a deleted surface stay, flagged orphan
    let mut hosted = vec![];
    for &id in &freeforms {
        hosted.extend(m.panel_systems_on(id)?);
        let kept = m.shells_on(id)?;
        m.delete_freeform(id)?;
        if !kept.is_empty() {
            run.receipt.loss.push(format!("freeform {id}: {} roofs and slabs kept as orphans", kept.len()));
        }
    }
    run.receipt.deleted.extend(all.into_iter().map(|(c, id)| Ref::id(c, id)));
    run.receipt.deleted.extend(hosted.into_iter().map(|id| Ref::id(Cat::PanelSystem, id)));
    Ok(())
}

fn transform(m: &mut Model, run: &mut Run, t: &Targets, xf: &Xform, copy: bool) -> Result<(), Fail> {
    let refs = targets(m, run, t)?;
    let mut ts = Vec::with_capacity(refs.len());
    for &(cat, id) in &refs {
        ts.push(target_of(cat, id).ok_or_else(|| {
            unsupported(format!("{} cannot be transformed", Ref::id(cat, id).label()))
        })?);
    }
    ops_spline::whole(m, &mut ts)?;
    let out = match xf {
        Xform::Move { by } => {
            let [x, y, z] = by.to_m();
            let plan = x.abs() > EPS || y.abs() > EPS;
            let lift = z.abs() > EPS;
            if copy {
                m.copy_elements_z(&ts, &[(x, y, z)])?
            } else {
                if !plan && !lift {
                    return Err(bad_value("move by zero"));
                }
                if plan {
                    m.move_elements(&ts, (x, y))?;
                }
                if lift {
                    m.lift_elements(&ts, z)?;
                }
                vec![]
            }
        }
        Xform::Rotate { center, axis, angle } => {
            if axis.x.abs() > EPS || axis.y.abs() > EPS || axis.z.abs() <= EPS {
                return Err(unsupported("rotation about a non-vertical axis is not lowered yet"));
            }
            let c = center.to_m();
            let a = angle.to_rad() * axis.z.signum();
            m.rotate_elements(&ts, (c[0], c[1]), a, copy)?
        }
        Xform::Mirror { a, b } => {
            let (pa, pb) = (a.to_m(), b.to_m());
            m.mirror_elements(&ts, (pa[0], pa[1]), (pb[0], pb[1]), copy)?
        }
    };
    if copy {
        run.receipt.copied.extend(out.into_iter().map(ref_of));
    } else {
        run.receipt.changed.extend(refs.into_iter().map(|(c, id)| Ref::id(c, id)));
    }
    Ok(())
}

fn level(m: &mut Model, run: &mut Run, l: &LevelOp) -> Result<(), Fail> {
    match l {
        LevelOp::Create { slot, name, elevation } => {
            if name.trim().is_empty() {
                return Err(bad_value("level needs a name"));
            }
            let id = m.add_level(name, elevation.to_m())?;
            run.made(*slot, Cat::Level, id);
        }
        LevelOp::Rename { target, name } => {
            let id = resolve_as(m, run, target, Cat::Level)?;
            let (_, z) = level_row(m, id)?;
            m.update_level(id, name, z)?;
            run.receipt.changed.push(Ref::id(Cat::Level, id));
        }
        LevelOp::SetElevation { target, elevation } => {
            let id = resolve_as(m, run, target, Cat::Level)?;
            let (name, _) = level_row(m, id)?;
            m.update_level(id, &name, elevation.to_m())?;
            run.receipt.changed.push(Ref::id(Cat::Level, id));
        }
    }
    Ok(())
}

fn grid(m: &mut Model, run: &mut Run, g: &GridOp) -> Result<(), Fail> {
    match g {
        GridOp::Create { slot, name, a, b, bulge } => {
            let (pa, pb) = (a.to_m(), b.to_m());
            let mut s = GridSpec::line(name, (pa[0], pa[1]), (pb[0], pb[1]));
            s.bulge = *bulge;
            if name.trim().is_empty() {
                // empty name: the model picks the next one (1 -> 2, A -> B)
                s.name = None;
            }
            let id = m.add_grid(&s)?;
            run.made(*slot, Cat::Grid, id);
        }
        GridOp::Rename { target, name } => {
            if name.trim().is_empty() {
                return Err(bad_value("grid needs a name"));
            }
            let id = resolve_as(m, run, target, Cat::Grid)?;
            let mut s = m.grid(id)?.spec;
            s.name = Some(name.clone());
            m.update_grid(id, &s)?;
            run.receipt.changed.push(Ref::id(Cat::Grid, id));
        }
    }
    Ok(())
}

#[path = "ops_set.rs"]
mod set;
#[path = "ops_geo.rs"]
mod ops_geo;
#[path = "ops_freeform.rs"]
mod ops_freeform;
#[path = "ops_lattice.rs"]
mod ops_lattice;
#[path = "ops_member.rs"]
mod ops_member;
#[path = "ops_pattern.rs"]
mod ops_pattern;
#[path = "ops_create.rs"]
mod ops_create;
#[path = "ops_spline.rs"]
mod ops_spline;
#[cfg(test)]
#[path = "ops_spline_tests.rs"]
mod spline_tests;

#[cfg(test)]
#[path = "ops_apply_tests.rs"]
mod tests;
