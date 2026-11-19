// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The plan: items as intents, compiled against the scratch copy into exact model steps.
//! Intents survive a re-plan; steps and ids are rebuilt by `replay` every time.

use super::exec::{self, ModelStep, NewResolved};
use super::preview::PreviewSet;
use super::rel_value::{self, Skip};
use super::rows;
use super::sets::{norm_cat, table_of, Key, Sets};
use super::validate::PlanIssue;
use crate::editor::Editor;
use crate::query;
use crate::shell::panels::replace_type::{type_edits, Inst};
use crate::units::Units;
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_gen::{GenOut, Generator};
use ddc_model::checks::{CheckRule, CheckScope, Finding, Fix};
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, CompHost, CompSpec, CurtainSpec, Edit, ElKey,
    GridLines, GridSpec, Just, LayerSpec, MemberShape, ModelDiff, MullionSection, OpeningSpec,
    ParamDef, ParamSpec, ParamType, ParamValue, RoofKind, RoofSpec, RoomSpec,
    RowKind, RunSpec, ShaftSpec, SlabSpec, StairShape, StairSpec, Swing, WallSpec,
};
use serde_json::Value as J;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError(pub String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ddc_model::Error> for PlanError {
    fn from(e: ddc_model::Error) -> Self {
        PlanError(e.to_string())
    }
}

impl From<rusqlite::Error> for PlanError {
    fn from(e: rusqlite::Error) -> Self {
        PlanError(e.to_string())
    }
}

impl From<String> for PlanError {
    fn from(e: String) -> Self {
        PlanError(e)
    }
}

impl From<&str> for PlanError {
    fn from(e: &str) -> Self {
        PlanError(e.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdOrNew {
    Id(i64),
    New(u32),
}

/// Most references one range may stand for.
const MAX_RANGE: u32 = 10_000;

/// `door:55`, `wall:n7`, `wall_type:3`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ElRef {
    pub cat: String,
    pub id: IdOrNew,
}

impl ElRef {
    pub fn parse(s: &str) -> Result<ElRef, PlanError> {
        let bad = || PlanError(format!("bad reference: {}", super::fmt::text(s)));
        let (cat, id) = s.trim().split_once(':').ok_or_else(bad)?;
        let id = match id.strip_prefix('n') {
            Some(n) => IdOrNew::New(n.parse().map_err(|_| bad())?),
            None => IdOrNew::Id(id.parse().map_err(|_| bad())?),
        };
        Ok(ElRef {
            cat: cat.trim().to_string(),
            id,
        })
    }

    /// A reference or a run of new ones, `column:n1..n4` (the form results use for three or
    /// more neighbours); the end is the number alone, as in the result. Ids have no ranges.
    pub fn parse_many(s: &str) -> Result<Vec<ElRef>, PlanError> {
        let Some((head, last)) = s.trim().split_once("..") else {
            return Ok(vec![ElRef::parse(s)?]);
        };
        let bad = || PlanError(format!("bad reference range: {}", super::fmt::text(s)));
        let first = ElRef::parse(head)?;
        let IdOrNew::New(a) = first.id else {
            return Err(bad());
        };
        let b: u32 = last.trim().strip_prefix('n').unwrap_or(last).parse().map_err(|_| bad())?;
        if b < a || b - a >= MAX_RANGE {
            return Err(bad());
        }
        Ok((a..=b).map(|n| ElRef::new(&first.cat, n)).collect())
    }

    pub fn new(cat: &str, slot: u32) -> ElRef {
        ElRef {
            cat: cat.to_string(),
            id: IdOrNew::New(slot),
        }
    }

    pub fn text(&self) -> String {
        match self.id {
            IdOrNew::Id(i) => format!("{}:{i}", self.cat),
            IdOrNew::New(n) => format!("{}:n{n}", self.cat),
        }
    }

    pub fn slot(&self) -> Option<u32> {
        match self.id {
            IdOrNew::New(n) => Some(n),
            IdOrNew::Id(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Targets {
    Filter(String),
    Set(u32),
    Refs(Vec<ElRef>),
    /// Frozen into `Refs` when the tool is called: the selection may change before a replay.
    Selection,
}

/// A roof made by drawing a profile: `(u, z)` over the segment `a`->`b`, drawn `depth` to
/// its left (an extrusion roof of the model).
#[derive(Clone, Debug, PartialEq)]
pub struct RoofVault {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub profile: Vec<(f64, f64)>,
    pub depth: f64,
}

/// One new element; foreign keys are references, so an element of this plan can be named.
#[derive(Clone, Debug, PartialEq)]
pub enum NewElem {
    Wall {
        level: i64,
        a: (f64, f64),
        b: (f64, f64),
        /// Arc axis a -> b: tan(sweep/4), > 0 counter-clockwise; 0 straight.
        bulge: f64,
        base_offset: f64,
        height: f64,
        wtype: Option<ElRef>,
    },
    Opening {
        wall: ElRef,
        along: f64,
        width: f64,
        height: f64,
        sill: f64,
        kind: String,
    },
    Slab {
        level: i64,
        stype: Option<ElRef>,
        offset: f64,
        outer: Vec<(f64, f64)>,
        holes: Vec<Vec<(f64, f64)>>,
    },
    Room {
        level: i64,
        name: String,
        number: String,
        point: (f64, f64),
        height: f64,
    },
    Column {
        level: i64,
        top_level: Option<i64>,
        mtype: Option<ElRef>,
        at: (f64, f64),
        base_offset: f64,
        top_offset: f64,
        rotation: f64,
        structural: bool,
        /// Top point of a slanted column (V and raked supports); `None`: vertical.
        top_at: Option<(f64, f64)>,
    },
    Beam {
        level: i64,
        mtype: Option<ElRef>,
        a: (f64, f64, f64),
        b: (f64, f64, f64),
    },
    Comp {
        level: i64,
        ctype: ElRef,
        at: (f64, f64, f64),
        rotation: f64,
        host: Option<ElRef>,
        /// Size of the instance against its type (the `Scale` value of a family that has it).
        scale: f64,
        /// Least clear trunk under the crown (the `MinClear` value of a tree family), m.
        min_clear: f64,
    },
    Ceiling {
        level: i64,
        stype: Option<ElRef>,
        height: f64,
        outer: Vec<(f64, f64)>,
        holes: Vec<Vec<(f64, f64)>>,
    },
    Roof {
        level: i64,
        stype: Option<ElRef>,
        offset: f64,
        overhang: f64,
        ring: Vec<(f64, f64)>,
        slope: Option<f64>,
        /// Degrees per ring edge, `None` a vertical gable; empty means `slope` on every edge.
        edges: Vec<Option<f64>>,
        /// The roof drawn as a vault over its ring instead of sloped by its edges.
        vault: Option<RoofVault>,
    },
    Stair {
        level: i64,
        top_level: i64,
        origin: (f64, f64),
        angle: f64,
    },
    Pipe {
        system: ElRef,
        level: i64,
        material: String,
        dn: u32,
        points: Vec<(f64, f64, f64)>,
        /// A duct, cable tray or conduit instead of a catalog pipe.
        section: Option<RunSection>,
    },
    Curtain {
        level: i64,
        name: String,
        a: (f64, f64),
        b: (f64, f64),
        base_offset: f64,
        height: f64,
        spacing: (f64, f64),
        mullion: (f64, f64),
        /// Height of the opaque band above the base, 0 for none: the first row is solid.
        spandrel: f64,
    },
    /// A stair of a generator: any shape, the storey made exact by `top_offset`.
    GenStair {
        level: i64,
        top_level: i64,
        top_offset: f64,
        origin: (f64, f64),
        angle: f64,
        shape: StairShape,
        width: f64,
        max_riser: f64,
        landing: f64,
    },
    Shaft {
        level: i64,
        top_level: i64,
        top_offset: f64,
        ring: Vec<(f64, f64)>,
    },
    Grid {
        name: String,
        a: (f64, f64),
        b: (f64, f64),
    },
    /// Steel connection (g5): type key, joined columns and beams, `key=value;...` params.
    SteelConn {
        level: i64,
        kind: String,
        members: Vec<ElRef>,
        params: String,
    },
    /// Guard rail along `path` (x, y, z above the level), or along side `.1` (0 left, 1 right
    /// going up) of a stair, the path then unused.
    Railing {
        level: i64,
        path: Vec<(f64, f64, f64)>,
        stair: Option<(ElRef, u8)>,
        height: f64,
    },
}

impl NewElem {
    /// The category word a created element answers to in references.
    pub fn cat(&self) -> &'static str {
        match self {
            NewElem::Wall { .. } => "wall",
            NewElem::Opening { kind, .. } if kind == "window" => "window",
            NewElem::Opening { .. } => "door",
            NewElem::Slab { .. } => "floor",
            NewElem::Room { .. } => "room",
            NewElem::Column { .. } => "column",
            NewElem::Beam { .. } => "beam",
            NewElem::Comp { .. } => "comp",
            NewElem::Ceiling { .. } => "ceiling",
            NewElem::Roof { .. } => "roof",
            NewElem::Stair { .. } => "stair",
            NewElem::Pipe { .. } => "mep",
            NewElem::Curtain { .. } => "curtain",
            NewElem::GenStair { .. } => "stair",
            NewElem::Shaft { .. } => "shaft",
            NewElem::Grid { .. } => "grid",
            NewElem::SteelConn { .. } => "steel_conn",
            NewElem::Railing { .. } => "railing",
        }
    }

    fn refs(&self) -> Vec<&ElRef> {
        let mut out = vec![];
        match self {
            NewElem::Wall { wtype: Some(r), .. }
            | NewElem::Slab { stype: Some(r), .. }
            | NewElem::Column { mtype: Some(r), .. }
            | NewElem::Beam { mtype: Some(r), .. }
            | NewElem::Ceiling { stype: Some(r), .. }
            | NewElem::Roof { stype: Some(r), .. }
            | NewElem::Opening { wall: r, .. }
            | NewElem::Pipe { system: r, .. } => out.push(r),
            NewElem::Comp { ctype, host, .. } => {
                out.push(ctype);
                out.extend(host.iter());
            }
            NewElem::SteelConn { members, .. } => out.extend(members.iter()),
            NewElem::Railing { stair: Some((r, _)), .. } => out.push(r),
            _ => {}
        }
        out
    }

    /// The same references as `refs`, for renumbering slots.
    pub(crate) fn refs_mut(&mut self) -> Vec<&mut ElRef> {
        let mut out = vec![];
        match self {
            NewElem::Wall { wtype: Some(r), .. }
            | NewElem::Slab { stype: Some(r), .. }
            | NewElem::Column { mtype: Some(r), .. }
            | NewElem::Beam { mtype: Some(r), .. }
            | NewElem::Ceiling { stype: Some(r), .. }
            | NewElem::Roof { stype: Some(r), .. }
            | NewElem::Opening { wall: r, .. }
            | NewElem::Pipe { system: r, .. } => out.push(r),
            NewElem::Comp { ctype, host, .. } => {
                out.push(ctype);
                out.extend(host.iter_mut());
            }
            NewElem::SteelConn { members, .. } => out.extend(members.iter_mut()),
            NewElem::Railing { stair: Some((r, _)), .. } => out.push(r),
            _ => {}
        }
        out
    }
}

/// Section of a run that is not a catalog pipe: duct, tray or conduit, width or diameter and
/// height (0 for round), m.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSection {
    pub domain: ddc_model::MepDomain,
    pub w: f64,
    pub h: f64,
}

impl RunSection {
    pub fn section(&self) -> Result<ddc_model::MepSection, PlanError> {
        use ddc_model::{MepDomain, MepSection, MepShape};
        let ok = |v: f64| v.is_finite() && v > 0.0;
        if !ok(self.w) || !(self.h == 0.0 || ok(self.h)) {
            return Err(PlanError("run size must be above zero".into()));
        }
        let shape = if self.h == 0.0 {
            MepShape::Round { d: self.w }
        } else {
            MepShape::Rect {
                w: self.w,
                h: self.h,
            }
        };
        let mut s = MepSection::new(self.domain, shape);
        if self.domain == MepDomain::Duct {
            s.wall = 0.0008;
        }
        Ok(s)
    }
}

/// What `create_type` adds: the type table follows from the category.
#[derive(Clone, Debug, PartialEq)]
pub struct NewType {
    pub slot: u32,
    pub cat: String,
    pub name: String,
    pub layers: Vec<LayerSpec>,
    pub shape: Option<String>,
    pub material: String,
    pub family: Option<String>,
    pub values: Vec<(String, ParamValue)>,
}

/// A finding kept by reference: the fix is taken again from a re-run of its rule on replay.
#[derive(Clone, Debug)]
pub struct FindingRef {
    pub id: String,
    pub rule: String,
    pub key: String,
    pub elements: Vec<ElKey>,
    pub fix: Fix,
}

#[derive(Clone, Debug)]
pub enum PlanOp {
    Assign {
        target: Targets,
        set: Vec<query::Assign>,
    },
    ReplaceType {
        target: Targets,
        to: ElRef,
    },
    /// Item `i` gets slot `slot0 + i`, stable across replays.
    Create {
        slot0: u32,
        items: Vec<NewElem>,
    },
    Delete {
        target: Targets,
    },
    NewType(NewType),
    NewParam(ParamSpec),
    LoadFamily {
        def: String,
    },
    /// A family from structured JSON (`define_family`); it lands as a draft.
    DefineFamily {
        def: J,
    },
    /// Geometry ops (`create_geometry`); op `i` that creates a row gets slot `slot0 + i`.
    Geometry {
        slot0: u32,
        ops: Vec<J>,
    },
    /// A sandboxed geometry script (`run_script`, `rerun_recipe`); local slot `k` is `slot0 + k`.
    Script {
        slot0: u32,
        item: Arc<super::script_tools::ScriptItem>,
    },
    /// `slot0`..: room for the largest variant, whichever seed is chosen later.
    Generate {
        id: String,
        params: J,
        seed: u64,
        level: i64,
        slot0: u32,
    },
    Fix(FindingRef),
    /// A wall type layer change or a copy of wall types from another file.
    WallType(super::type_lib_plan::WallTypeOp),
    /// Networks routed, fixtures placed, devices powered: planned again on each compile.
    Mep(super::mep_plan::MepOp),
    /// A whole building from a spec, rebuilt on every compile in fixed slot blocks.
    Project(super::project_plan::ProjectOp),
    /// A process plant from a brief: its elements and pipes, brought in place on each compile.
    Plant(super::plant_plan::PlantOp),
    /// Sheets of a drawing set: tags first, then the planned sheets on the tagged model.
    /// The flag: sheet words in Russian.
    DrawingSet(ddc_io::sheets::DsetOptions, bool),
    /// A modify tool on a target: move, copy, rotate, mirror, scale, array, offset, split, pin.
    Modify {
        target: Targets,
        act: super::modify_plan::ModifyAct,
    },
    /// A level or automatic-rooms tool: no element target.
    Tool(super::modify_plan::ToolAct),
}

impl PlanOp {
    /// Slots this op reads: where `depends_on` comes from.
    pub fn slots_used(&self) -> Vec<u32> {
        let of_target = |t: &Targets| -> Vec<u32> {
            match t {
                Targets::Refs(r) => r.iter().filter_map(ElRef::slot).collect(),
                _ => vec![],
            }
        };
        match self {
            PlanOp::Assign { target, .. }
            | PlanOp::Delete { target }
            | PlanOp::Modify { target, .. } => of_target(target),
            PlanOp::ReplaceType { target, to } => {
                let mut v = of_target(target);
                v.extend(to.slot());
                v
            }
            PlanOp::Create { slot0, items } => {
                let own = *slot0..*slot0 + items.len() as u32;
                items
                    .iter()
                    .flat_map(NewElem::refs)
                    .filter_map(ElRef::slot)
                    .filter(|s| !own.contains(s))
                    .collect()
            }
            PlanOp::NewType(_)
            | PlanOp::NewParam(_)
            | PlanOp::LoadFamily { .. }
            | PlanOp::DefineFamily { .. }
            | PlanOp::Geometry { .. }
            | PlanOp::Script { .. }
            | PlanOp::Generate { .. }
            | PlanOp::Fix(_)
            | PlanOp::WallType(_)
            | PlanOp::Mep(_)
            | PlanOp::Project(_)
            | PlanOp::Plant(_)
            | PlanOp::DrawingSet(..)
            | PlanOp::Tool(_) => vec![],
        }
    }
}

/// Audit text of a plan step: the first line is the history title, the rest the reason beside it.
pub fn audit_text(title: &str, why: &str) -> Option<String> {
    let why = super::fmt::clip(why, ddc_ai::catalog::MAX_WHY_CHARS);
    match (title.is_empty(), why.is_empty()) {
        (true, true) => None,
        (_, true) => Some(title.to_string()),
        (_, false) => Some(format!("{title}
{why}")),
    }
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub id: u32,
    pub title: String,
    /// Why the model proposed it; empty for plans made before the field existed.
    pub why: String,
    pub op: PlanOp,
    pub checked: bool,
    pub depends_on: Vec<u32>,
    pub actor: String,
    pub conflict: Option<String>,
    /// What the item read and wrote; shared, since the card clones items on every publish.
    pub sets: Arc<super::readset::ItemSets>,
}

#[derive(Clone)]
pub struct CompiledPlan {
    pub base_head: u64,
    pub steps: Arc<Vec<ModelStep>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ElemDiff {
    pub key: ElKey,
    pub kind: RowKind,
    pub item: Option<u32>,
    pub level: Option<i64>,
    pub label: String,
    pub cells: Vec<ddc_model::CellDiff>,
}

/// `key`: a total (`walls`, `slab_area`) or `volume:<material>` / `area:<material>`.
#[derive(Clone, Debug, PartialEq)]
pub struct QtyDelta {
    pub key: String,
    pub level: Option<i64>,
    pub before: f64,
    pub after: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantInfo {
    pub id: u32,
    pub seed: u64,
    /// What sets this variant apart, as the generator states it.
    pub criterion: String,
    pub metrics: Vec<(String, f64)>,
}

/// What the card shows. `ack` is the last plan-affecting message the worker has processed: the
/// UI keeps Apply off until it catches up, so a just-unticked item can never be applied.
pub struct PlanView {
    pub items: Vec<PlanItem>,
    pub diff: Vec<ElemDiff>,
    pub secondary: usize,
    pub qty: Vec<QtyDelta>,
    pub issues: Vec<PlanIssue>,
    /// What the main model's caps say about a plan gated as a draft; empty for a main plan.
    pub main_issues: Vec<PlanIssue>,
    pub preview: Arc<PreviewSet>,
    pub compiled: CompiledPlan,
    pub stale: bool,
    pub variants: Vec<VariantInfo>,
    pub variant: u32,
    pub truncated: bool,
    pub flagged: u32,
    pub ack: u64,
    /// The target `issues` were gated with: a draft skips the main model's caps.
    pub gated: super::validate::Target,
    pub allow_large: bool,
    /// Agent items that joined the card after the person last looked: Apply stays off until
    /// the person confirms the review.
    pub unreviewed: Vec<u32>,
    /// Time to the expiry of the agent's waiting items when the card was published.
    pub expires_in: Option<std::time::Duration>,
    /// Files to write once the card is applied; the card lets the person change them.
    pub deliver: Option<crate::deliver::Outputs>,
}

/// The agents that proposed the ticked items, each named once (the person's own items excluded).
pub fn proposers(items: &[PlanItem]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for i in items.iter().filter(|i| i.checked) {
        if let Some(n) = i.actor.strip_prefix(super::session::ACTOR_PREFIX) {
            if !out.iter().any(|o| o == n) {
                out.push(n.to_string());
            }
        }
    }
    out
}

impl PlanView {
    pub fn blocked(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.level == super::validate::IssueLevel::Block)
    }

    /// The main model's caps for a plan gated as a draft: Apply is its only way into the main model.
    pub fn main_gate_blocks(&self) -> bool {
        self.main_issues
            .iter()
            .any(|i| i.level == super::validate::IssueLevel::Block)
    }
}

/// Owner item of every reserved slot, kept as runs: a project reserves one run, not one entry
/// per slot, so memory follows the number of reservations.
#[derive(Default, Clone, Debug)]
pub struct SlotOwners {
    /// `(first slot, length, owner)`, sorted by first slot, never overlapping.
    runs: Vec<(u32, u32, u32)>,
}

impl SlotOwners {
    pub fn reserve(&mut self, first: u32, n: u32, owner: u32) {
        if n == 0 {
            return;
        }
        let at = self.runs.partition_point(|r| r.0 < first);
        self.runs.insert(at, (first, n, owner));
    }

    pub fn get(&self, slot: &u32) -> Option<&u32> {
        let at = self.runs.partition_point(|r| r.0 <= *slot).checked_sub(1)?;
        let r = &self.runs[at];
        (*slot - r.0 < r.1).then_some(&r.2)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&u32, &mut u32) -> bool) {
        self.runs.retain_mut(|r| keep(&r.0, &mut r.2));
    }

    /// Runs kept, not slots.
    pub fn runs(&self) -> usize {
        self.runs.len()
    }

    /// Slots covered by all runs.
    pub fn slots(&self) -> u64 {
        self.runs.iter().map(|r| r.1 as u64).sum()
    }

    /// Keeps the runs whose owner passes `keep`.
    pub fn filtered(&self, keep: impl Fn(u32) -> bool) -> SlotOwners {
        SlotOwners { runs: self.runs.iter().filter(|r| keep(r.2)).copied().collect() }
    }

    /// Moves every slot up by `slots` and every owner up by `items`.
    pub fn shift(&mut self, slots: u32, items: u32) {
        for r in self.runs.iter_mut() {
            r.0 += slots;
            r.2 += items;
        }
    }
}

/// Live state of a plan on the scratch copy.
#[derive(Default)]
pub struct PlanState {
    pub items: Vec<PlanItem>,
    pub next_item: u32,
    pub next_slot: u32,
    pub slots: HashMap<u32, i64>,
    pub slot_owner: SlotOwners,
    pub steps: Vec<(u32, Vec<ModelStep>)>,
    pub declared: HashMap<u32, HashSet<ElKey>>,
    pub skips: HashMap<u32, Vec<Skip>>,
    pub variants: Vec<VariantInfo>,
    pub variant: u32,
    pub allow_large: bool,
    /// The project units each item was written in: its numbers mean the same on every replay.
    pub units: HashMap<u32, Units>,
    /// Files `deliver_project` asked for, written after the person's Apply.
    pub deliver: Option<crate::deliver::Outputs>,
    /// Items a live change hit (`conflict::conflicts`): not compiled again, the conflict stays
    /// until the person unticks the item or the plan ends.
    pub held: HashMap<u32, String>,
    /// Places freed by `replan_items`, in order: (the item to insert before, attempts so far,
    /// the actor who freed them). That actor's next items take them, so the plan keeps its order.
    pub refill: std::collections::VecDeque<(Option<u32>, u8, String)>,
    /// Replans each item descends from: the automatic turn stops at `replan::MAX_TRIES`.
    pub tries: HashMap<u32, u8>,
}

impl PlanState {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn compiled(&self, base_head: u64) -> CompiledPlan {
        let steps: Vec<ModelStep> = self
            .steps
            .iter()
            .flat_map(|s| s.1.iter().cloned())
            .collect();
        CompiledPlan {
            base_head,
            steps: Arc::new(steps),
        }
    }

    pub fn reserve_slots(&mut self, n: u32, item: u32) -> u32 {
        let s0 = self.next_slot + 1;
        self.next_slot += n;
        self.slot_owner.reserve(s0, n, item);
        s0
    }

    /// Dependencies from slots plus parameters this item writes by name.
    pub fn depends_of(&self, op: &PlanOp) -> Vec<u32> {
        let mut d: Vec<u32> = op
            .slots_used()
            .iter()
            .filter_map(|s| self.slot_owner.get(s).copied())
            .collect();
        if let PlanOp::Assign { set, .. } = op {
            for it in &self.items {
                if let PlanOp::NewParam(p) = &it.op {
                    if set
                        .iter()
                        .any(|a| a.key.to_lowercase() == p.name.to_lowercase())
                    {
                        d.push(it.id);
                    }
                }
            }
        }
        d.sort_unstable();
        d.dedup();
        d
    }

    /// Items that created an element this op names by its raw id: a set frozen while the plan
    /// was on the copy, or a reference read from it. Slots do not show these.
    pub fn depends_on_created(&self, op: &PlanOp, sets: &Sets) -> Vec<u32> {
        let mut made: HashMap<ElKey, u32> = HashMap::new();
        for (id, steps) in &self.steps {
            for k in steps.iter().flat_map(exec::created_keys) {
                made.insert(k, *id);
            }
        }
        if made.is_empty() {
            return vec![];
        }
        let names = named_keys(op, sets);
        let mut d: Vec<u32> = names.iter().filter_map(|k| made.get(k).copied()).collect();
        d.sort_unstable();
        d.dedup();
        d
    }
}

/// [`named_keys`] as point reads.
pub fn named_reads(op: &PlanOp, sets: &Sets) -> super::readset::ReadSet {
    let mut r = super::readset::ReadSet::default();
    named_keys(op, sets).iter().for_each(|k| r.el(k));
    r
}

/// Elements an op names by raw id: a frozen set, references, a type, a finding's elements.
pub fn named_keys(op: &PlanOp, sets: &Sets) -> Vec<ElKey> {
    let mut names: Vec<ElKey> = vec![];
    {
        match op {
            PlanOp::Assign { target, .. }
            | PlanOp::Delete { target }
            | PlanOp::Modify { target, .. } => target_names(target, sets, &mut names),
            PlanOp::ReplaceType { target, to } => {
                target_names(target, sets, &mut names);
                names.extend(raw_name(to));
            }
            PlanOp::Create { items, .. } => {
                names.extend(items.iter().flat_map(NewElem::refs).filter_map(raw_name))
            }
            PlanOp::Fix(f) => names.extend(f.elements.iter().cloned()),
            PlanOp::WallType(super::type_lib_plan::WallTypeOp::Layer { ty, .. }) => {
                names.push(("wall_type".to_string(), *ty))
            }
            PlanOp::NewType(_)
            | PlanOp::NewParam(_)
            | PlanOp::LoadFamily { .. }
            | PlanOp::DefineFamily { .. }
            | PlanOp::Geometry { .. }
            | PlanOp::Script { .. }
            | PlanOp::Generate { .. }
            | PlanOp::WallType(_)
            | PlanOp::Project(_)
            | PlanOp::Plant(_)
            | PlanOp::DrawingSet(..)
            | PlanOp::Tool(_) => {}
            PlanOp::Mep(m) => names.extend(m.elements()),
        }
    }
    names
}

fn raw_name(r: &ElRef) -> Option<ElKey> {
    let IdOrNew::Id(id) = r.id else {
        return None;
    };
    let table = type_table(&r.cat)
        .ok()
        .or_else(|| norm_cat(&r.cat).map(table_of))?;
    Some((table.to_string(), id))
}

fn target_names(t: &Targets, sets: &Sets, out: &mut Vec<ElKey>) {
    match t {
        Targets::Set(n) => {
            if let Some(s) = sets.get(*n) {
                out.extend(s.keys.iter().map(Key::el));
            }
        }
        Targets::Refs(r) => out.extend(r.iter().filter_map(raw_name)),
        Targets::Filter(_) | Targets::Selection => {}
    }
}

/// Everything compiling needs: scratch editor plus read-only session data.
pub struct CompileCx<'a> {
    pub ed: &'a mut Editor,
    pub slots: &'a mut HashMap<u32, i64>,
    pub sets: &'a Sets,
    pub gens: &'a [&'static dyn Generator],
    pub rules: &'a [&'static dyn CheckRule],
    /// Elements the item announces; everything else in the diff is a side effect.
    pub declared: HashSet<ElKey>,
    /// Elements the item left out with the reason; the rest of the item still applies.
    pub skips: Vec<Skip>,
}

/// Compiles and executes one item on the scratch copy; a failure leaves the copy as it was.
pub fn compile_one(cx: &mut CompileCx<'_>, op: &PlanOp) -> Result<Vec<ModelStep>, PlanError> {
    let top = cx.ed.model.op_top()?;
    let saved = cx.slots.clone();
    // derived tables once per item, as the Apply recomputes them once (`exec::exec_all`)
    cx.ed.model.hold_derived();
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compile(cx, op)));
    let settled = cx.ed.model.settle_derived();
    let res = match run {
        Ok(r) => r.and_then(|steps| settled.map(|()| steps).map_err(PlanError::from)),
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if res.is_err() {
        cx.ed.model.rollback_to(top)?;
        *cx.slots = saved;
    }
    // the editor reads level data: a stale copy would misreport later items
    let _s = ddc_model::perf::span("draft.reload");
    cx.ed.reload()?;
    res
}

pub(super) fn push(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    mut s: ModelStep,
) -> Result<Vec<i64>, PlanError> {
    let ids = exec::fill(&mut cx.ed.model, &mut s)?;
    out.push(s);
    Ok(ids)
}

fn compile(cx: &mut CompileCx<'_>, op: &PlanOp) -> Result<Vec<ModelStep>, PlanError> {
    let mut out = vec![];
    match op {
        PlanOp::Assign { target, set } => compile_assign(cx, target, set, &mut out)?,
        PlanOp::ReplaceType { target, to } => compile_replace(cx, target, to, &mut out)?,
        PlanOp::Create { slot0, items } => compile_create(cx, *slot0, items, &mut out)?,
        PlanOp::Delete { target } => {
            let keys = resolve(cx, target)?;
            compile_delete(cx, &keys.iter().map(Key::el).collect::<Vec<_>>(), &mut out)?;
        }
        PlanOp::NewType(t) => compile_type(cx, t, &mut out)?,
        PlanOp::NewParam(p) => {
            push(
                cx,
                &mut out,
                ModelStep::Edits(vec![Edit::NewParam(p.clone())]),
            )?;
        }
        PlanOp::LoadFamily { def } => {
            push(
                cx,
                &mut out,
                ModelStep::Family {
                    def: def.clone(),
                    overwrite: false,
                },
            )?;
        }
        PlanOp::DefineFamily { def } => {
            push(cx, &mut out, ModelStep::DefineFamily { json: def.to_string() })?;
        }
        PlanOp::Geometry { slot0, ops } => super::geo_tools::compile(cx, *slot0, ops, &mut out)?,
        PlanOp::Script { slot0, item } => {
            super::script_tools::compile(cx, *slot0, item, &mut out)?
        }
        PlanOp::Generate {
            id,
            params,
            seed,
            level,
            slot0,
        } => {
            let g = cx
                .gens
                .iter()
                .find(|g| g.id() == id)
                .ok_or_else(|| PlanError(format!("no generator {id}")))?;
            match g
                .build(params, *seed)
                .map_err(|e| PlanError(e.to_string()))?
            {
                GenOut::Layout(l) => {
                    super::gen_map::compile_layout(cx, &l, *level, *slot0, &mut out)?
                }
                GenOut::Family { def } => {
                    push(
                        cx,
                        &mut out,
                        ModelStep::Family {
                            def,
                            overwrite: false,
                        },
                    )?;
                }
                GenOut::Steel { frame, hall, joints, .. } => {
                    let step = ModelStep::SteelFrame {
                        level: *level,
                        frame: std::sync::Arc::new(*frame),
                        expect: vec![],
                        joints,
                    };
                    push(cx, &mut out, step)?;
                    if let Some(h) = hall {
                        super::gen_map::compile_hall(cx, &h, *level, *slot0, &mut out)?;
                    }
                }
            }
        }
        PlanOp::Fix(f) => compile_fix(cx, f, &mut out)?,
        PlanOp::WallType(w) => super::type_lib_plan::compile(cx, w, &mut out)?,
        PlanOp::Mep(m) => super::mep_plan::compile(cx, m, &mut out)?,
        PlanOp::Project(p) => super::project_plan::compile(cx, p, &mut out)?,
        PlanOp::Plant(p) => super::plant_plan::compile(cx, p, &mut out)?,
        PlanOp::DrawingSet(o, ru) => {
            let lv = ddc_io::sheets::tag_levels(&cx.ed.model).map_err(|e| e.0)?;
            let prefix = crate::sheet_view::mark_prefix(*ru);
            push(cx, &mut out, ModelStep::DrawingTags(lv, prefix))?;
            let p = crate::sheet_view::plan_drawing_set(cx.ed, o, *ru)?;
            push(cx, &mut out, ModelStep::DrawingSet(Box::new(p.plan)))?;
        }
        PlanOp::Modify { target, act } => {
            super::modify_plan::compile(cx, target, act, &mut out)?
        }
        PlanOp::Tool(a) => super::modify_plan::compile_tool(cx, a, &mut out)?,
    }
    if out.is_empty() {
        return Err(PlanError(CmdError::wire(ErrorCode::NoEffect, "nothing to change")));
    }
    Ok(out)
}

/// Keys of a target on the current scratch state; a named element that is gone is a conflict.
pub fn resolve(cx: &CompileCx<'_>, t: &Targets) -> Result<Vec<Key>, PlanError> {
    let keys = match t {
        Targets::Filter(s) => filter_keys(&*cx.ed, s)?,
        Targets::Set(n) => {
            let set = cx
                .sets
                .get(*n)
                .ok_or_else(|| PlanError(format!("no set s{n}")))?;
            if let Some(k) = set.replaced {
                return Err(PlanError(format!(
                    "element was changed or replaced: {}",
                    k.text()
                )));
            }
            let all = rows::rows(&*cx.ed);
            let have: HashMap<(&'static str, i64), &'static str> = all
                .iter()
                .map(|r| ((rows::key_of(r).table(), r.id()), r.cat))
                .collect();
            for k in set.keys.iter() {
                match have.get(&(k.table(), k.id)) {
                    None => return Err(PlanError(format!("element is gone: {}", k.text()))),
                    Some(cat) if *cat != k.cat => {
                        return Err(PlanError(format!(
                            "element was changed or replaced: {}",
                            k.text()
                        )))
                    }
                    Some(_) => {}
                }
            }
            super::readset::note(|r| r.keys(cx.ed.model.sql(), &set.keys));
            set.keys.to_vec()
        }
        Targets::Refs(refs) => {
            let mut out = Vec::with_capacity(refs.len());
            for r in refs {
                out.push(resolve_ref(cx, r)?);
            }
            super::readset::note(|r| r.keys(cx.ed.model.sql(), &out));
            out
        }
        Targets::Selection => return Err("selection was not frozen".into()),
    };
    if keys.is_empty() {
        let none = "no element matches the target";
        return Err(PlanError(CmdError::wire(ErrorCode::NotFound, none)));
    }
    Ok(keys)
}

pub fn filter_keys(ed: &Editor, text: &str) -> Result<Vec<Key>, PlanError> {
    query::check_tool_numbers(text).map_err(PlanError)?;
    let q = query::parse(text).map_err(|e| PlanError(e.to_string()))?;
    if !q.set.is_empty() {
        return Err("a filter cannot assign values".into());
    }
    let env = query::Env::tool(ed);
    let f = query::Filter::new(&q.expr, &env);
    let keys: Vec<Key> = rows::rows(ed)
        .iter()
        .filter(|r| f.test(r))
        .map(rows::key_of)
        .collect();
    super::readset::note(|r| r.filter(ed.model.sql(), &q.expr, &keys));
    Ok(keys)
}

fn resolve_ref(cx: &CompileCx<'_>, r: &ElRef) -> Result<Key, PlanError> {
    let cat = norm_cat(&r.cat)
        .ok_or_else(|| PlanError(format!("unknown category: {}", super::fmt::text(&r.cat))))?;
    let id = id_of(cx.slots, r)?;
    let table = table_of(cat);
    let exists: bool = cx
        .ed
        .model
        .sql()
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
            [id],
            |x| x.get(0),
        )
        .unwrap_or(false);
    if !exists {
        return Err(PlanError(format!("element is gone: {}", r.text())));
    }
    // the table holds doors and windows alike: `door:55` must not reach a window
    if table == "opening" && cat != "opening" {
        let actual = if cx.ed.model.opening_kind(id)? == "window" {
            "window"
        } else {
            "door"
        };
        if actual != cat {
            return Err(PlanError(format!(
                "element was changed or replaced: {} is a {actual}",
                r.text()
            )));
        }
    }
    Ok(Key { cat, id })
}

fn id_of(slots: &HashMap<u32, i64>, r: &ElRef) -> Result<i64, PlanError> {
    match r.id {
        IdOrNew::Id(i) => Ok(i),
        IdOrNew::New(n) => slots
            .get(&n)
            .copied()
            .ok_or_else(|| PlanError(format!("no new element n{n} in this plan"))),
    }
}

fn param_cat(table: &str, cat: &str) -> String {
    match table {
        "opening" => cat.to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn find_param<'p>(defs: &'p [ParamDef], key: &str) -> Option<&'p ParamDef> {
    let k = key.trim().to_lowercase();
    defs.iter().find(|d| d.spec.name.to_lowercase() == k)
}

/// Type table and column of an element table, for type parameters and `replace_type`.
fn type_of_table(table: &str) -> Option<(&'static str, &'static str)> {
    Some(match table {
        "wall" => ("wall_type", "wtype"),
        "slab" | "roof" | "ceiling" => ("slab_type", "stype"),
        "col" | "beam" => ("member_type", "mtype"),
        "comp" => ("comp_type", "ctype"),
        _ => return None,
    })
}

fn compile_assign(
    cx: &mut CompileCx<'_>,
    target: &Targets,
    set: &[query::Assign],
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let keys = resolve(cx, target)?;
    let defs = cx.ed.model.params()?;
    let (ps, fields): (Vec<&query::Assign>, Vec<&query::Assign>) = set
        .iter()
        .partition(|a| find_param(&defs, &a.key).is_some());
    let mut edits: Vec<Edit> = vec![];
    for a in ps {
        let Some(d) = find_param(&defs, &a.key) else {
            continue;
        };
        let (value, relative) = match &a.val {
            query::Val::Rel(op, by) if rel_value::numeric(d.spec.ptype) => {
                (None, Some((*op, by.as_str())))
            }
            query::Val::Rel(..) => {
                return Err(PlanError(format!(
                    "parameter {} takes a value, not a formula",
                    super::fmt::text(&d.spec.name)
                )));
            }
            query::Val::Lit(text) => {
                let v = if text.trim().is_empty() {
                    None
                } else if matches!(
                    d.spec.ptype,
                    ddc_model::ParamType::Text | ddc_model::ParamType::YesNo
                ) {
                    Some(ddc_model::parse_value(d.spec.ptype, text)?)
                } else {
                    // machine text: a lone comma is ambiguous here, not a legacy decimal
                    query::check_tool_numbers(text).map_err(PlanError)?;
                    Some(ddc_model::parse_value(d.spec.ptype, text)?)
                };
                (v, None)
            }
        };
        // a relative value reads the value the element has now: one that cannot follow is skipped
        type Pv = Result<Option<ddc_model::PVal>, String>;
        let value_at = |cx: &CompileCx<'_>, kind: &str, id: i64| -> Pv {
            let Some((op, by)) = relative else {
                return Ok(value.clone());
            };
            let cur = cx.ed.model.param_value_of(d, kind, id).map_err(|e| e.to_string())?;
            rel_value::param_value(d.spec.ptype, cur, op, by, cx.ed.tool.units).map(Some)
        };
        let mut types_seen: HashSet<(&'static str, i64)> = HashSet::new();
        for k in &keys {
            let table = k.table();
            let cat = param_cat(table, k.cat);
            let cat = if table == "opening" && cat == "opening" {
                cx.ed.model.opening_kind(k.id)?
            } else {
                cat
            };
            if !d.spec.categories.iter().any(|c| *c == cat) {
                return Err(PlanError(format!(
                    "parameter {} is not bound to {cat}",
                    super::fmt::text(&d.spec.name)
                )));
            }
            if d.spec.per_type {
                let (tt, col) =
                    type_of_table(table).ok_or_else(|| PlanError(format!("{cat} has no types")))?;
                let t: i64 = cx
                    .ed
                    .model
                    .sql()
                    .query_row(
                        &format!("SELECT {col} FROM {table} WHERE id=?1"),
                        [k.id],
                        |r| r.get(0),
                    )
                    .map_err(|e| PlanError(e.to_string()))?;
                let v = match value_at(cx, tt, t) {
                    Ok(v) => v,
                    Err(why) => {
                        cx.skips.push(Skip { cat: cat.to_string(), id: k.id, why });
                        continue;
                    }
                };
                // the type row is what changes: the card shows it under this item
                cx.declared.insert((tt.to_string(), t));
                if types_seen.insert((tt, t)) {
                    edits.push(Edit::ParamValue {
                        param: d.spec.name.clone(),
                        kind: tt.to_string(),
                        id: t,
                        value: v,
                    });
                }
            } else {
                let v = match value_at(cx, table, k.id) {
                    Ok(v) => v,
                    Err(why) => {
                        cx.skips.push(Skip { cat: cat.to_string(), id: k.id, why });
                        continue;
                    }
                };
                edits.push(Edit::ParamValue {
                    param: d.spec.name.clone(),
                    kind: table.to_string(),
                    id: k.id,
                    value: v,
                });
            }
            cx.declared.insert(k.el());
        }
    }
    if !fields.is_empty() {
        let mut sels = Vec::with_capacity(keys.len());
        for k in &keys {
            let Some(s) = rows::sel_of(*k) else { continue };
            if !matches!(
                k.table(),
                "wall" | "opening" | "slab" | "room" | "curtain_wall"
            ) {
                return Err(PlanError(format!(
                    "field {} cannot be set on {}: use a parameter or change its type",
                    super::fmt::text(&fields[0].key),
                    k.cat
                )));
            }
            sels.push(s);
            cx.declared.insert(k.el());
        }
        let owned: Vec<query::Assign> = fields.into_iter().cloned().collect();
        let field_edits =
            query::assign_edits_in(&cx.ed.model, cx.ed.tool.units, query::NumIn::Tool, &sels, &owned)
                .map_err(|e| PlanError(e.to_string()))?;
        // an opening size passes the checks of the panel: an element that fails is skipped with
        // its reason; when every one fails, the item stops and names the first
        let (kept, refused) = crate::multi_edit::screen_openings(&cx.ed, field_edits);
        let what = |why: &crate::opening_tool::PlaceError| match why {
            // a lower or shorter wall that its openings would no longer fit
            crate::opening_tool::PlaceError::Hosts { .. } => "wall",
            _ => "opening",
        };
        if kept.is_empty() {
            if let Some((id, why)) = refused.first() {
                return Err(PlanError(format!(
                    "{} {id} cannot take this size: {}",
                    what(why),
                    why.text(false, cx.ed.tool.units)
                )));
            }
        }
        for (id, why) in &refused {
            let text = why.text(crate::ui_is_ru(), cx.ed.tool.units);
            cx.skips.push(Skip { cat: what(why).to_string(), id: *id, why: text });
        }
        edits.extend(kept);
    }
    // a locked value is never changed by AI apply: the item stops and names it
    let (_, locked) = cx.ed.model.split_locked(&edits)?;
    if let Some((kind, id, name)) = locked.first() {
        let more = match locked.len() {
            1 => String::new(),
            n => format!(" and {} more", n - 1),
        };
        return Err(PlanError(format!(
            "parameter {} of {kind} {id}{more} is locked: unlock it or leave it out",
            super::fmt::text(name)
        )));
    }
    if !edits.is_empty() {
        push(cx, out, ModelStep::Edits(edits))?;
    }
    Ok(())
}

fn compile_replace(
    cx: &mut CompileCx<'_>,
    target: &Targets,
    to: &ElRef,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let keys = resolve(cx, target)?;
    let to_id = id_of(cx.slots, to)?;
    let to_table = type_table(&to.cat)?;
    let mut insts = vec![];
    let mut comps = vec![];
    for k in &keys {
        let table = k.table();
        let Some((tt, _)) = type_of_table(table) else {
            return Err(PlanError(format!(
                "{} has no types: use a parameter or set its size",
                k.cat
            )));
        };
        if tt != to_table {
            return Err(PlanError(format!(
                "{} is a {to_table}, {} needs a {tt}",
                to.text(),
                k.cat
            )));
        }
        cx.declared.insert(k.el());
        if table == "comp" {
            comps.push(k.id);
        } else {
            let level = cx
                .ed
                .model
                .sql()
                .query_row(
                    &format!("SELECT level FROM {table} WHERE id=?1"),
                    [k.id],
                    |r| r.get(0),
                )
                .map_err(|e| PlanError(e.to_string()))?;
            insts.push(Inst {
                table,
                id: k.id,
                level,
            });
        }
    }
    if !insts.is_empty() {
        let edits = type_edits(&cx.ed.model, &insts, to_id).map_err(PlanError)?;
        if !edits.is_empty() {
            push(cx, out, ModelStep::Edits(edits))?;
        }
    }
    for id in comps {
        let mut spec = cx.ed.model.component(id)?;
        if spec.ctype != to_id {
            spec.ctype = to_id;
            push(cx, out, ModelStep::Comp { id, spec })?;
        }
    }
    Ok(())
}

/// Table of a type reference; `door_type` and the like are family types.
pub fn type_table(cat: &str) -> Result<&'static str, PlanError> {
    Ok(match cat.trim() {
        "wall_type" => "wall_type",
        "slab_type" | "floor_type" | "roof_type" | "ceiling_type" => "slab_type",
        "member_type" | "column_type" | "beam_type" => "member_type",
        "comp_type" | "door_type" | "window_type" | "family_type" => "comp_type",
        other => {
            return Err(PlanError(format!(
                "not a type reference: {}",
                super::fmt::text(other)
            )))
        }
    })
}

fn first_id(cx: &CompileCx<'_>, table: &str) -> Result<i64, PlanError> {
    cx.ed
        .model
        .sql()
        .query_row(&format!("SELECT min(id) FROM {table}"), [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .map_err(|e| PlanError(e.to_string()))?
        .ok_or_else(|| PlanError(format!("no {table} in the model: create one first")))
}

/// A type reference, or the first type of the table when none was named.
fn type_or_first(cx: &CompileCx<'_>, r: &Option<ElRef>, table: &str) -> Result<i64, PlanError> {
    match r {
        Some(r) => {
            let got = type_table(&r.cat)?;
            if got != table {
                return Err(PlanError(format!("{} is not a {table}", r.text())));
            }
            id_of(cx.slots, r)
        }
        None => first_id(cx, table),
    }
}

pub(super) fn compile_create(
    cx: &mut CompileCx<'_>,
    slot0: u32,
    items: &[NewElem],
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let mut i = 0;
    while i < items.len() {
        let same =
            |a: &NewElem, b: &NewElem| std::mem::discriminant(a) == std::mem::discriminant(b);
        let start = i;
        i += 1;
        // a batch is one model call: the wall tool joins its walls the same way
        if matches!(
            items[start],
            NewElem::Wall { .. }
                | NewElem::Slab { .. }
                | NewElem::Column { .. }
                | NewElem::Beam { .. }
                | NewElem::Comp { .. }
                | NewElem::Opening { .. }
                | NewElem::Room { .. }
        ) {
            while i < items.len() && same(&items[start], &items[i]) {
                i += 1;
            }
        }
        let group = &items[start..i];
        let step = resolve_group(cx, group)?;
        let ids = push(cx, out, step)?;
        if ids.len() != group.len() {
            return Err(PlanError(format!(
                "created {} of {}",
                ids.len(),
                group.len()
            )));
        }
        for (k, id) in ids.iter().enumerate() {
            cx.slots.insert(slot0 + (start + k) as u32, *id);
            cx.declared
                .insert((table_of(group[k].cat()).to_string(), *id));
        }
        // arc walls are made with their bulge in the same call (ModelStep::ArcWalls)
    }
    Ok(())
}

pub(super) fn resolve_group(
    cx: &CompileCx<'_>,
    group: &[NewElem],
) -> Result<ModelStep, PlanError> {
    let mut walls = vec![];
    let mut bulges = vec![];
    let mut slabs = vec![];
    let mut cols = vec![];
    let mut beams = vec![];
    let mut comps = vec![];
    let mut opens = vec![];
    let mut rooms = vec![];
    for e in group {
        match e {
            NewElem::Wall {
                level,
                a,
                b,
                bulge,
                base_offset,
                height,
                wtype,
            } => {
                bulges.push(*bulge);
                walls.push(WallSpec {
                    a: *a,
                    b: *b,
                    level: *level,
                    base_offset: *base_offset,
                    height: *height,
                    wall_type: type_or_first(cx, wtype, "wall_type")?,
                    offset0: None,
                })
            }
            NewElem::Slab {
                level,
                stype,
                offset,
                outer,
                holes,
            } => slabs.push(SlabSpec {
                level: *level,
                slab_type: type_or_first(cx, stype, "slab_type")?,
                offset: *offset,
                outer: outer.clone(),
                holes: holes.clone(),
            }),
            NewElem::Column {
                level,
                top_level,
                mtype,
                at,
                base_offset,
                top_offset,
                rotation,
                structural,
                top_at,
            } => cols.push(ColumnSpec {
                level: *level,
                top_level: *top_level,
                member_type: type_or_first(cx, mtype, "member_type")?,
                structural: *structural,
                at: *at,
                base_offset: *base_offset,
                top_offset: *top_offset,
                rotation: *rotation,
                top_at: *top_at,
            }),
            NewElem::Beam { level, mtype, a, b } => beams.push(BeamSpec {
                level: *level,
                member_type: type_or_first(cx, mtype, "member_type")?,
                a: *a,
                b: *b,
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            }),
            NewElem::Comp {
                level,
                ctype,
                at,
                rotation,
                host,
                scale,
                min_clear,
            } => {
                let got = type_table(&ctype.cat)?;
                if got != "comp_type" {
                    return Err(PlanError(format!("{} is not a family type", ctype.text())));
                }
                let host = match host {
                    None => CompHost::None,
                    Some(r) => {
                        let k = resolve_ref(cx, r)?;
                        match k.table() {
                            "wall" => CompHost::Wall(k.id),
                            "slab" => CompHost::Floor(k.id),
                            "ceiling" => CompHost::Ceiling(k.id),
                            _ => {
                                return Err(PlanError(format!(
                                    "{} cannot host a component",
                                    r.text()
                                )))
                            }
                        }
                    }
                };
                let mut spec = CompSpec::at(*level, id_of(cx.slots, ctype)?, at.0, at.1);
                spec.at = *at;
                spec.rotation = *rotation;
                spec.host = host;
                if *scale != 1.0 {
                    spec.values.push(("Scale".into(), ddc_geom::component::Value::Num(*scale)));
                }
                if *min_clear > 0.0 {
                    spec.values.push(("MinClear".into(), ddc_geom::component::Value::Num(*min_clear)));
                }
                comps.push(spec);
            }
            NewElem::Opening {
                wall,
                along,
                width,
                height,
                sill,
                kind,
            } => {
                let k = resolve_ref(cx, wall)?;
                if k.table() != "wall" {
                    return Err(PlanError(format!("{} is not a wall", wall.text())));
                }
                let spec = OpeningSpec {
                    along: *along,
                    width: *width,
                    sill: *sill,
                    height: *height,
                    kind: kind.clone(),
                };
                opens.push((k.id, spec, Swing::default()));
            }
            NewElem::Room {
                level,
                name,
                number,
                point,
                height,
            } => rooms.push(RoomSpec {
                level: *level,
                name: name.clone(),
                number: number.clone(),
                point: *point,
                height: *height,
            }),
            NewElem::Ceiling {
                level,
                stype,
                height,
                outer,
                holes,
            } => {
                return add(NewResolved::Ceiling(CeilingSpec {
                    level: *level,
                    slab_type: type_or_first(cx, stype, "slab_type")?,
                    height: *height,
                    outer: outer.clone(),
                    holes: holes.clone(),
                }));
            }
            NewElem::Roof {
                level,
                stype,
                offset,
                overhang,
                ring,
                slope,
                edges,
                vault,
            } => {
                let slopes = if edges.len() == ring.len() { edges.clone() } else { vec![*slope; ring.len()] };
                let kind = match vault {
                    Some(v) => RoofKind::Extrusion {
                        a: v.a,
                        b: v.b,
                        profile: v.profile.clone(),
                        depth: v.depth,
                    },
                    None => RoofKind::Footprint {
                        ring: ring.clone(),
                        slopes,
                        mansard: None,
                    },
                };
                return add(NewResolved::Roof(RoofSpec {
                    level: *level,
                    slab_type: type_or_first(cx, stype, "slab_type")?,
                    offset: *offset,
                    overhang: if vault.is_some() { 0.0 } else { *overhang },
                    fascia: 0.0,
                    kind,
                }));
            }
            NewElem::Stair {
                level,
                top_level,
                origin,
                angle,
            } => {
                return add(NewResolved::Stair(StairSpec::straight(
                    *level, *top_level, *origin, *angle,
                )));
            }
            NewElem::GenStair {
                level,
                top_level,
                top_offset,
                origin,
                angle,
                shape,
                width,
                max_riser,
                landing,
            } => {
                let mut s = StairSpec::straight(*level, *top_level, *origin, *angle);
                s.top_offset = *top_offset;
                s.shape = *shape;
                s.width = *width;
                s.max_riser = *max_riser;
                s.landing = *landing;
                if let ddc_geom::stair::Shape::Spiral { turn, .. } = s.shape {
                    let elev = |lv: i64| -> Result<f64, PlanError> {
                        cx.ed
                            .model
                            .sql()
                            .query_row("SELECT elevation FROM level WHERE id=?1", [lv], |r| {
                                r.get::<_, f64>(0)
                            })
                            .map_err(|e| PlanError(e.to_string()))
                    };
                    let height = elev(*top_level)? + *top_offset - elev(*level)? - s.base_offset;
                    let rise = height * std::f64::consts::TAU / turn;
                    let need = ddc_gen::project::rules::rule("stair.spiral_headroom");
                    if rise < need {
                        return Err(PlanError(format!(
                            "spiral: rise per turn {rise:.2} m is under stair.spiral_headroom {need:.2} m"
                        )));
                    }
                }
                return add(NewResolved::Stair(s));
            }
            NewElem::Shaft {
                level,
                top_level,
                top_offset,
                ring,
            } => {
                return add(NewResolved::Shaft(ShaftSpec {
                    level: *level,
                    top_level: *top_level,
                    base_offset: 0.0,
                    top_offset: *top_offset,
                    ring: ring.clone(),
                }));
            }
            NewElem::Grid { name, a, b } => {
                return add(NewResolved::Grid(GridSpec::line(name, *a, *b)));
            }
            NewElem::Railing { level, path, stair, height } => {
                let host = match stair {
                    Some((r, side)) => {
                        let k = resolve_ref(cx, r)?;
                        if k.table() != "stair" {
                            return Err(PlanError(format!("{} is not a stair", r.text())));
                        }
                        ddc_model::RailingHost::Stair { stair: k.id, side: *side }
                    }
                    None if path.len() >= 2 => ddc_model::RailingHost::Path(path.clone()),
                    None => return Err(PlanError("a railing needs a stair or two points".into())),
                };
                let mut s = ddc_model::RailingSpec::new(*level, host);
                s.height = *height;
                return add(NewResolved::Railing(s));
            }
            NewElem::SteelConn { level, kind, members, params } => {
                let kind = ddc_geom::steel::ConnKind::parse(kind)
                    .ok_or_else(|| PlanError(format!("unknown connection type {kind}")))?;
                let members = members
                    .iter()
                    .map(|r| Ok((r.cat.clone(), id_of(cx.slots, r)?)))
                    .collect::<Result<Vec<_>, PlanError>>()?;
                let params = ddc_geom::steel::Params::parse(params).map_err(PlanError)?;
                return add(NewResolved::SteelConn(ddc_model::SteelConnSpec {
                    level: *level,
                    kind,
                    members,
                    params,
                }));
            }
            NewElem::Pipe {
                system,
                level,
                material,
                dn,
                points,
                section,
            } => {
                let sys = id_of(cx.slots, system)?;
                if let Some(s) = section {
                    let spec = RunSpec::new(sys, *level, s.section()?, points.clone());
                    return add(NewResolved::Mep(spec));
                }
                let mat = super::materials::pipe_material(material).ok_or_else(|| {
                    PlanError(format!(
                        "unknown pipe material: {}",
                        super::fmt::text(material)
                    ))
                })?;
                return add(NewResolved::Mep(RunSpec::pipe(
                    sys,
                    *level,
                    mat,
                    *dn,
                    points.clone(),
                )?));
            }
            NewElem::Curtain {
                level,
                name,
                a,
                b,
                base_offset,
                height,
                spacing,
                mullion,
                spandrel,
            } => {
                let sec = MullionSection::Rect {
                    w: mullion.0,
                    d: mullion.1,
                };
                let spandrel = *spandrel;
                let v = if spandrel > 0.0 && spandrel < *height {
                    GridLines::At(vec![spandrel])
                } else {
                    GridLines::Spacing(spacing.1)
                };
                return add(NewResolved::Curtain(CurtainSpec {
                    level: *level,
                    name: name.clone(),
                    a: *a,
                    b: *b,
                    bulge: 0.0,
                    base_offset: *base_offset,
                    height: *height,
                    u: GridLines::Spacing(spacing.0),
                    v,
                    interior: sec,
                    border: sec,
                    lines: Default::default(),
                    top: vec![],
                }, if spandrel < *height { spandrel } else { 0.0 }));
            }
        }
    }
    Ok(if bulges.iter().any(|k| *k != 0.0) {
        ModelStep::ArcWalls {
            specs: walls,
            bulges,
            expect: vec![],
        }
    } else if !walls.is_empty() {
        ModelStep::Walls {
            specs: walls,
            expect: vec![],
        }
    } else if !slabs.is_empty() {
        ModelStep::Slabs {
            specs: slabs,
            expect: vec![],
        }
    } else if !cols.is_empty() {
        ModelStep::Columns {
            specs: cols,
            expect: vec![],
        }
    } else if !beams.is_empty() {
        ModelStep::Beams {
            specs: beams,
            expect: vec![],
        }
    } else if !opens.is_empty() {
        ModelStep::Openings {
            specs: opens,
            expect: vec![],
        }
    } else if !rooms.is_empty() {
        ModelStep::Rooms {
            specs: rooms,
            expect: vec![],
        }
    } else {
        ModelStep::Comps {
            specs: comps,
            expect: vec![],
        }
    })
}

fn add(elem: NewResolved) -> Result<ModelStep, PlanError> {
    Ok(ModelStep::Add {
        elem: Box::new(elem),
        expect: 0,
    })
}

fn compile_delete(
    cx: &mut CompileCx<'_>,
    keys: &[ElKey],
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let mut edits = vec![];
    // openings first: deleting a wall takes its openings with it
    for (t, id) in keys.iter().filter(|k| k.0 == "opening") {
        edits.push(Edit::Delete { table: "opening", id: *id });
        cx.declared.insert((t.clone(), *id));
    }
    let mut late = vec![];
    for (t, id) in keys.iter().filter(|k| k.0 != "opening") {
        cx.declared.insert((t.clone(), *id));
        match t.as_str() {
            "wall" => edits.push(Edit::Delete { table: "wall", id: *id }),
            "room" => edits.push(Edit::DeleteRoom(*id)),
            "col" => edits.push(Edit::DeleteColumn(*id)),
            "beam" => edits.push(Edit::DeleteBeam(*id)),
            "slab" => edits.push(Edit::Delete { table: "slab", id: *id }),
            "curtain_wall" => edits.push(Edit::DeleteCurtain(*id)),
            "ceiling" | "comp" | "roof" | "stair" | "railing" | "mep_run" | "mesh" => {
                let table: &'static str = rows::ELEMENT_TABLES
                    .iter()
                    .copied()
                    .find(|x| x == t)
                    .unwrap_or("comp");
                late.push(ModelStep::Delete { table, id: *id });
            }
            other => return Err(PlanError(format!("cannot delete from {other}"))),
        }
    }
    if !edits.is_empty() {
        push(cx, out, ModelStep::Edits(edits))?;
    }
    for s in late {
        push(cx, out, s)?;
    }
    Ok(())
}

fn compile_type(
    cx: &mut CompileCx<'_>,
    t: &NewType,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let cat = norm_cat(&t.cat)
        .ok_or_else(|| PlanError(format!("unknown category: {}", super::fmt::text(&t.cat))))?;
    let (elem, table) = match cat {
        "wall" => (
            NewResolved::WallType {
                name: t.name.clone(),
                layers: t.layers.clone(),
            },
            "wall_type",
        ),
        "floor" | "roof" | "ceiling" => (
            NewResolved::SlabType {
                name: t.name.clone(),
                layers: t.layers.clone(),
            },
            "slab_type",
        ),
        "column" | "beam" => {
            let text = t
                .shape
                .as_deref()
                .ok_or("a member type needs a shape such as rect 0.3 0.3")?;
            let shape = MemberShape::parse(text)
                .ok_or_else(|| PlanError(format!("bad shape: {}", super::fmt::text(text))))?;
            (
                NewResolved::MemberType {
                    name: t.name.clone(),
                    shape,
                    material: t.material.clone(),
                },
                "member_type",
            )
        }
        "comp" => {
            let want = t
                .family
                .as_deref()
                .ok_or("a family type needs a family name")?
                .trim()
                .to_lowercase();
            let fam = cx
                .ed
                .model
                .families()?
                .into_iter()
                .find(|f| f.1.to_lowercase() == want)
                .ok_or_else(|| PlanError(format!("no family {}", super::fmt::text(&want))))?;
            (
                NewResolved::CompType {
                    family: fam.0,
                    name: t.name.clone(),
                    values: t.values.clone(),
                },
                "comp_type",
            )
        }
        other => return Err(PlanError(format!("{other} has no types"))),
    };
    let ids = push(
        cx,
        out,
        ModelStep::Add {
            elem: Box::new(elem),
            expect: 0,
        },
    )?;
    if let Some(id) = ids.first() {
        cx.slots.insert(t.slot, *id);
        cx.declared.insert((table.to_string(), *id));
    }
    Ok(())
}

fn compile_fix(
    cx: &mut CompileCx<'_>,
    f: &FindingRef,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let rule = all_rules(cx.rules).into_iter().find(|r| r.id() == f.rule);
    let fix = match rule {
        Some(r) => {
            let scope = if f.elements.is_empty() {
                CheckScope::All
            } else {
                CheckScope::Elements(f.elements.clone())
            };
            let mut found: Vec<Finding> = vec![];
            r.run(&cx.ed.model, &scope, &mut found)?;
            found
                .into_iter()
                .find(|x| x.elements == f.elements && x.key() == f.key)
                .map(|x| x.fix)
                .ok_or_else(|| PlanError(format!("finding {} no longer holds", f.id)))?
        }
        None => f.fix.clone(),
    };
    for k in &f.elements {
        cx.declared.insert(k.clone());
    }
    match fix {
        Fix::Edits(e) if !e.is_empty() => {
            push(cx, out, ModelStep::Edits(e))?;
        }
        Fix::Clash(c) => {
            push(cx, out, ModelStep::ClashFix(c))?;
        }
        Fix::Delete(keys) => {
            for k in &keys {
                cx.declared.insert(k.clone());
            }
            compile_delete(cx, &keys, out)?;
        }
        Fix::Edits(_) | Fix::None => {
            return Err(PlanError(format!("finding {} has no automatic fix", f.id)))
        }
    }
    Ok(())
}

/// Registry rules plus the session's own; a rule id from the session wins.
pub fn all_rules(extra: &[&'static dyn CheckRule]) -> Vec<&'static dyn CheckRule> {
    let mut v: Vec<&'static dyn CheckRule> = extra.to_vec();
    for r in ddc_model::checks::rules() {
        if !v.iter().any(|x| x.id() == r.id()) {
            v.push(*r);
        }
    }
    v
}

/// Recompiles every ticked item in order. Rolling back is the caller's job. An item depending
/// on an unticked or failed one is unticked with the reason; a failed item keeps its tick and a
/// conflict, which blocks Apply until the person decides. A held item (`PlanState::held`) keeps
/// its conflict and is not compiled.
pub fn replay(
    st: &mut PlanState,
    ed: &mut Editor,
    sets: &Sets,
    gens: &[&'static dyn Generator],
    rules: &[&'static dyn CheckRule],
) -> Result<(), PlanError> {
    replay_with(st, ed, sets, gens, rules, None)
}

/// [`replay`] after a live change that hit none of the compiled items: each item must leave its
/// rows as before (`conflict::written_fp`), or its sets missed something and it is held as a
/// conflict; the others are based on `base` from now on.
pub fn replay_checked(
    st: &mut PlanState,
    ed: &mut Editor,
    sets: &Sets,
    gens: &[&'static dyn Generator],
    rules: &[&'static dyn CheckRule],
    base: super::readset::ChangeToken,
) -> Result<(), PlanError> {
    replay_with(st, ed, sets, gens, rules, Some(base))
}

fn replay_with(
    st: &mut PlanState,
    ed: &mut Editor,
    sets: &Sets,
    gens: &[&'static dyn Generator],
    rules: &[&'static dyn CheckRule],
    check: Option<super::readset::ChangeToken>,
) -> Result<(), PlanError> {
    st.slots.clear();
    st.steps.clear();
    st.declared.clear();
    st.skips.clear();
    let mut done: HashSet<u32> = HashSet::new();
    let mut slots = std::mem::take(&mut st.slots);
    let units = ed.tool.units;
    for it in st.items.iter_mut() {
        // an unticked item keeps the reason it was unticked for
        if !it.checked {
            continue;
        }
        if let Some(c) = st.held.get(&it.id) {
            it.conflict = Some(c.clone());
            continue;
        }
        if let Some(d) = it.depends_on.iter().find(|d| st.held.contains_key(d)) {
            let why = super::conflict::via_text(*d);
            st.held.insert(it.id, why.clone());
            it.conflict = Some(why);
            continue;
        }
        it.conflict = None;
        if let Some(d) = it.depends_on.iter().find(|d| !done.contains(d)) {
            it.checked = false;
            it.conflict = Some(format!("depends on item {d}"));
            continue;
        }
        let own = st.units.get(&it.id).copied().unwrap_or(units);
        if ed.tool.units != own {
            ed.set_units(own);
        }
        let mut cx = CompileCx {
            ed: &mut *ed,
            slots: &mut slots,
            sets,
            gens,
            rules,
            declared: HashSet::new(),
            skips: vec![],
        };
        let head = cx.ed.model.change_head();
        let (res, reads) = super::readset::record(|| compile_one(&mut cx, &it.op));
        match res {
            Ok(steps) => {
                // reads only grow: what the agent's read tools saw is not read again here
                let mut own = (*it.sets).clone();
                own.reads.merge(reads);
                own.reads.merge(named_reads(&it.op, sets));
                let made: HashSet<ElKey> = steps.iter().flat_map(exec::created_keys).collect();
                own.writes = super::readset::ReadSet::from_changes_except(
                    &cx.ed.model.changes_since(head),
                    &made,
                );
                if let Some(base) = check {
                    let fp = super::conflict::written_fp(cx.ed.model.sql(), &own.writes);
                    if own.fp != 0 && own.fp != fp {
                        let why = super::conflict::plan_conflict(
                            &[it.id],
                            &[],
                            "the result changed although nothing it read did",
                        );
                        st.held.insert(it.id, why.clone());
                        it.conflict = Some(why);
                    }
                    own.fp = fp;
                    own.base = base;
                }
                it.sets = Arc::new(own);
                if it.conflict.is_some() {
                    // the copy has the item's rows: compiled steps go with them, never applied
                    // while the conflict holds the item
                    st.steps.push((it.id, steps));
                    continue;
                }
                let declared = std::mem::take(&mut cx.declared);
                let skips = std::mem::take(&mut cx.skips);
                if !skips.is_empty() {
                    st.skips.insert(it.id, skips);
                }
                st.steps.push((it.id, steps));
                st.declared.insert(it.id, declared);
                done.insert(it.id);
            }
            Err(e) => it.conflict = Some(e.0),
        }
    }
    if ed.tool.units != units {
        ed.set_units(units);
    }
    st.slots = slots;
    Ok(())
}

fn primary(r: &ddc_model::RowDiff, key: &ElKey) -> bool {
    r.table == key.0 && r.rowid == key.1
}

/// Row differences grouped by owning element: `kind` comes from the element's own row.
pub fn group_diff(d: &ModelDiff) -> Vec<ElemDiff> {
    let mut at: HashMap<ElKey, usize> = HashMap::new();
    let mut out: Vec<ElemDiff> = vec![];
    for r in &d.rows {
        let key = r
            .owner
            .clone()
            .unwrap_or_else(|| (r.table.clone(), r.rowid));
        let i = *at.entry(key.clone()).or_insert_with(|| {
            out.push(ElemDiff {
                key: key.clone(),
                kind: RowKind::Changed,
                item: None,
                level: None,
                label: String::new(),
                cells: vec![],
            });
            out.len() - 1
        });
        let own = primary(r, &key);
        if own {
            out[i].kind = r.kind;
        }
        for c in &r.cells {
            let mut c = c.clone();
            if !own {
                c.col = format!("{}.{}", r.table, c.col);
            }
            if own && c.col == "level" {
                let v = c.after.as_deref().or(c.before.as_deref());
                out[i].level = v.and_then(|x| x.parse().ok());
            }
            out[i].cells.push(c);
        }
    }
    out
}

/// Every row the plan created. One that reads as changed against the base took a freed id:
/// without AUTOINCREMENT a new row may reuse the top one, after a delete by name or a cascade.
pub fn reused_ids(st: &PlanState) -> HashSet<ElKey> {
    st.steps
        .iter()
        .flat_map(|x| x.1.iter())
        .flat_map(exec::created_keys)
        .collect()
}

/// What one new item did, for the reply to the model. `diff` covers the rows it touched but
/// compares with the base, so earlier items of the plan have to be told apart here.
pub fn item_counts(
    st: &PlanState,
    steps: &[ModelStep],
    diff: &ModelDiff,
    touched: &ddc_model::Changes,
) -> (usize, usize, usize) {
    let own: HashSet<ElKey> = steps.iter().flat_map(exec::created_keys).collect();
    let grouped = group_diff(diff);
    let mut c = (0, 0, 0);
    for e in grouped
        .iter()
        .filter(|e| rows::ELEMENT_TABLES.contains(&e.key.0.as_str()))
    {
        match e.kind {
            _ if own.contains(&e.key) => c.0 += 1,
            RowKind::Created | RowKind::Changed => c.1 += 1,
            RowKind::Deleted => c.2 += 1,
        }
    }
    if !touched.all {
        let seen: HashSet<&ElKey> = grouped.iter().map(|e| &e.key).collect();
        let earlier: HashSet<ElKey> = reused_ids(st);
        c.2 += earlier
            .iter()
            .filter(|k| {
                rows::ELEMENT_TABLES.contains(&k.0.as_str())
                    && !seen.contains(k)
                    && touched.rows.get(&k.0).is_some_and(|ids| ids.contains(&k.1))
            })
            .count();
    }
    c
}

/// `diff_from` reads a reused id as one changed row; the person should see one gone, one new.
pub fn split_reused(diff: Vec<ElemDiff>, reused: &HashSet<ElKey>) -> Vec<ElemDiff> {
    if reused.is_empty() {
        return diff;
    }
    let mut out = Vec::with_capacity(diff.len());
    for e in diff {
        if e.kind != RowKind::Changed || !reused.contains(&e.key) {
            out.push(e);
            continue;
        }
        let mut gone = e.clone();
        gone.kind = RowKind::Deleted;
        gone.cells.iter_mut().for_each(|c| c.after = None);
        let mut new = e;
        new.kind = RowKind::Created;
        new.cells.iter_mut().for_each(|c| c.before = None);
        out.push(gone);
        out.push(new);
    }
    out
}

/// A stable, order-free form: the test of "preview equals applied" compares these. A reused id
/// the card split into gone and new is one changed row again; cell texts as the card shows them.
pub fn fingerprint(
    diff: &[ElemDiff],
) -> Vec<(
    ElKey,
    RowKind,
    Vec<(String, Option<String>, Option<String>)>,
)> {
    let clean = |s: &Option<String>| s.as_deref().map(super::fmt::visible);
    let mut merged: Vec<ElemDiff> = Vec::with_capacity(diff.len());
    for e in diff {
        let split = merged
            .last_mut()
            .filter(|p| e.kind == RowKind::Created && p.kind == RowKind::Deleted && p.key == e.key);
        if let Some(p) = split {
            p.kind = RowKind::Changed;
            for (c, n) in p.cells.iter_mut().zip(&e.cells) {
                c.after.clone_from(&n.after);
            }
        } else {
            merged.push(e.clone());
        }
    }
    let mut v: Vec<_> = merged
        .iter()
        .map(|e| {
            let mut c: Vec<_> = e
                .cells
                .iter()
                .map(|c| {
                    (
                        super::fmt::visible(&c.col),
                        clean(&c.before),
                        clean(&c.after),
                    )
                })
                .collect();
            c.sort();
            (e.key.clone(), e.kind, c)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Item attribution and labels. Returns the side-effect count: elements nobody announced.
pub fn attribute(diff: &mut [ElemDiff], st: &PlanState, ed: &Editor) -> usize {
    let rows = rows::rows(ed);
    let by_key: HashMap<(&'static str, i64), &query::Row> = rows
        .iter()
        .map(|r| ((rows::key_of(r).table(), r.id()), r))
        .collect();
    let mut creators: HashMap<ElKey, u32> = HashMap::new();
    for (id, steps) in &st.steps {
        for k in steps.iter().flat_map(exec::created_keys) {
            creators.insert(k, *id);
        }
    }
    let declaring = |key: &ElKey, not: Option<u32>| {
        st.steps
            .iter()
            .map(|s| s.0)
            .find(|id| Some(*id) != not && st.declared.get(id).is_some_and(|d| d.contains(key)))
    };
    let mut secondary = 0;
    for e in diff.iter_mut() {
        // both halves of a reused id: the new element is its creator's, the old one its remover's
        e.item = match (e.kind, creators.get(&e.key).copied()) {
            (RowKind::Created, Some(c)) => Some(c),
            (RowKind::Deleted, Some(c)) => declaring(&e.key, Some(c)),
            _ => declaring(&e.key, None),
        };
        let table = rows::ELEMENT_TABLES.iter().copied().find(|t| *t == e.key.0);
        let row = table.and_then(|t| by_key.get(&(t, e.key.1)));
        if let Some(r) = row {
            e.level = e.level.or(Some(r.level));
        }
        let name = row
            .map(|r| {
                if r.name.is_empty() {
                    r.ty.clone()
                } else {
                    r.name.clone()
                }
            })
            .unwrap_or_default();
        let id = e.key.1;
        e.label = super::fmt::text(&if name.is_empty() {
            format!("{} {id}", e.key.0)
        } else {
            format!("{} {id} · {name}", e.key.0)
        });
        if let Some(l) = super::apply_verify::type_row_label(&ed.model, &e.key) {
            e.label = l;
        }
        if e.item.is_none() && table.is_some() {
            secondary += 1;
        }
    }
    secondary
}

/// `+created ~changed −deleted` over element tables: what the model hears after a write tool.
pub fn counts(diff: &[ElemDiff]) -> (usize, usize, usize) {
    let mut c = (0, 0, 0);
    for e in diff
        .iter()
        .filter(|e| rows::ELEMENT_TABLES.contains(&e.key.0.as_str()))
    {
        match e.kind {
            RowKind::Created => c.0 += 1,
            RowKind::Changed => c.1 += 1,
            RowKind::Deleted => c.2 += 1,
        }
    }
    c
}

/// A project-unit text for a length given in metres: the assign logic reads project units.
pub fn length_text(metres: f64, u: Units) -> String {
    match u {
        Units::Metric => format!("{}", (metres * 1_000_000.0).round() / 1000.0),
        Units::Imperial => format!("{}", metres / crate::units::INCH),
    }
}

/// Parameter types the JSON value of `add_param` names.
pub fn param_type(key: &str) -> Option<ParamType> {
    ParamType::ALL.iter().copied().find(|t| t.key() == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{CellDiff, RowDiff};

    #[test]
    fn block_of_fifty_reserves_runs_not_slots() {
        let mut st = PlanState::default();
        let firsts: Vec<u32> = (1..=50).map(|i| st.reserve_slots(ddc_gen::project::slots::TOTAL, i)).collect();
        assert!(st.slot_owner.runs() <= 50, "{} runs", st.slot_owner.runs());
        assert_eq!(st.slot_owner.slots(), 50 * ddc_gen::project::slots::TOTAL as u64);
        for (i, f) in firsts.iter().enumerate() {
            let t = ddc_gen::project::slots::TOTAL;
            assert_eq!(st.slot_owner.get(f), Some(&(i as u32 + 1)));
            assert_eq!(st.slot_owner.get(&(f + t - 1)), Some(&(i as u32 + 1)));
        }
        assert_eq!(st.slot_owner.get(&0), None);
        assert_eq!(st.slot_owner.get(&(st.next_slot + 1)), None);
        st.slot_owner.retain(|_, o| *o != 7);
        assert_eq!(st.slot_owner.runs(), 49);
    }

    #[test]
    fn audit_text_carries_title_and_capped_why() {
        assert_eq!(audit_text("", ""), None);
        assert_eq!(audit_text("T", "").as_deref(), Some("T"));
        assert_eq!(audit_text("T", "because").as_deref(), Some("T\nbecause"));
        let long = "x".repeat(ddc_ai::catalog::MAX_WHY_CHARS * 3);
        let t = audit_text("T", &long).unwrap();
        let why = t.split_once('\n').unwrap().1;
        assert!(why.chars().count() <= ddc_ai::catalog::MAX_WHY_CHARS + 1, "{}", why.len());
    }

    #[test]
    fn references_parse_and_print() {
        let r = ElRef::parse("door:55").unwrap();
        assert_eq!(r.id, IdOrNew::Id(55));
        assert_eq!(r.text(), "door:55");
        let n = ElRef::parse("wall:n7").unwrap();
        assert_eq!(n.slot(), Some(7));
        assert_eq!(n.text(), "wall:n7");
        assert!(ElRef::parse("door").is_err());
        assert!(ElRef::parse("door:x").is_err());
    }

    #[test]
    fn a_run_of_new_references_expands_where_lists_are_read() {
        let r = ElRef::parse_many("column:n2..n4").unwrap();
        let texts: Vec<String> = r.iter().map(ElRef::text).collect();
        assert_eq!(texts, ["column:n2", "column:n3", "column:n4"]);
        // a run of one is fine, and one reference is a list of one
        assert_eq!(ElRef::parse_many("beam:n1..n1").unwrap().len(), 1);
        assert_eq!(ElRef::parse_many("door:55").unwrap(), vec![ElRef::parse("door:55").unwrap()]);
        for bad in ["column:n4..n2", "door:5..9", "column:n1..x", "column:n1..n99999", "..n3"] {
            assert!(ElRef::parse_many(bad).is_err(), "{bad}");
        }
        // a single reference stays a single reference
        assert!(ElRef::parse("column:n1..n3").is_err());
    }

    #[test]
    fn type_references_name_a_table() {
        assert_eq!(type_table("door_type").unwrap(), "comp_type");
        assert_eq!(type_table("wall_type").unwrap(), "wall_type");
        assert!(type_table("door").is_err());
    }

    #[test]
    fn dependencies_come_from_slots() {
        let mut st = PlanState::default();
        let s0 = st.reserve_slots(2, 1);
        let op = PlanOp::Create {
            slot0: st.next_slot + 1,
            items: vec![NewElem::Opening {
                wall: ElRef::new("wall", s0),
                along: 1.0,
                width: 0.9,
                height: 2.1,
                sill: 0.0,
                kind: "door".into(),
            }],
        };
        assert_eq!(st.depends_of(&op), vec![1]);
    }

    #[test]
    fn diff_groups_by_owner_and_takes_kind_from_the_own_row() {
        let cell = |c: &str| CellDiff {
            col: c.into(),
            before: None,
            after: Some("1".into()),
        };
        let d = ModelDiff {
            rows: vec![
                RowDiff {
                    table: "wall".into(),
                    rowid: 5,
                    kind: RowKind::Created,
                    owner: Some(("wall".into(), 5)),
                    cells: vec![cell("level")],
                },
                RowDiff {
                    table: "wall_qty".into(),
                    rowid: 9,
                    kind: RowKind::Created,
                    owner: Some(("wall".into(), 5)),
                    cells: vec![cell("volume")],
                },
            ],
            truncated: false,
        };
        let g = group_diff(&d);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].kind, RowKind::Created);
        assert_eq!(g[0].level, Some(1));
        assert_eq!(g[0].cells[1].col, "wall_qty.volume");
        assert_eq!(counts(&g), (1, 0, 0));
    }

    #[test]
    fn a_reused_id_reads_as_one_gone_and_one_new() {
        let mut st = PlanState::default();
        st.steps
            .push((1, vec![ModelStep::Edits(vec![Edit::Delete { table: "wall", id: 5 }])]));
        st.steps.push((
            2,
            vec![ModelStep::Walls {
                specs: vec![],
                expect: vec![5, 6],
            }],
        ));
        let reused = reused_ids(&st);
        assert!(reused.contains(&("wall".to_string(), 5)));

        let cell = CellDiff {
            col: "ax".into(),
            before: Some("1".into()),
            after: Some("2".into()),
        };
        let diff = vec![ElemDiff {
            key: ("wall".into(), 5),
            kind: RowKind::Changed,
            item: None,
            level: None,
            label: String::new(),
            cells: vec![cell],
        }];
        let got = split_reused(diff, &reused);
        assert_eq!(counts(&got), (1, 0, 1));
        assert_eq!(got[0].cells[0].after, None);
        assert_eq!(got[1].cells[0].before, None);
        // the card shows two rows, the change is still one
        let once = vec![ElemDiff {
            key: ("wall".into(), 5),
            kind: RowKind::Changed,
            item: None,
            level: None,
            label: String::new(),
            cells: vec![CellDiff {
                col: "ax".into(),
                before: Some("1".into()),
                after: Some("2".into()),
            }],
        }];
        assert_eq!(fingerprint(&got), fingerprint(&once));
    }

    #[test]
    fn a_new_item_reports_its_own_work_not_the_earlier_items() {
        let row = |id: i64, kind: RowKind| RowDiff {
            table: "wall".into(),
            rowid: id,
            kind,
            owner: Some(("wall".into(), id)),
            cells: vec![],
        };
        let mut st = PlanState::default();
        st.steps.push((
            1,
            vec![ModelStep::Walls {
                specs: vec![],
                expect: vec![5, 6],
            }],
        ));
        let steps = vec![ModelStep::Walls {
            specs: vec![],
            expect: vec![7],
        }];
        let diff = ModelDiff {
            rows: vec![
                row(5, RowKind::Created),
                row(7, RowKind::Created),
                row(1, RowKind::Deleted),
            ],
            truncated: false,
        };
        let touched = ddc_model::Changes {
            head: 0,
            all: false,
            rows: [("wall".to_string(), [1, 5, 6, 7].into_iter().collect())]
                .into_iter()
                .collect(),
        };
        // 7 is new; 5 was created earlier and edited now; 1 and the earlier 6 are deleted
        assert_eq!(item_counts(&st, &steps, &diff, &touched), (1, 1, 2));
    }

    #[test]
    fn lengths_become_project_units() {
        assert_eq!(length_text(0.9, Units::Metric), "900");
        let i: f64 = length_text(0.0254, Units::Imperial).parse().unwrap();
        assert!((i - 1.0).abs() < 1e-9);
    }

    /// The worker's `SetChecked`: marks only, nothing else of the plan moves.
    fn set_checked(st: &mut PlanState, list: Vec<(u32, bool)>) {
        for (id, on) in list {
            if let Some(i) = st.items.iter_mut().find(|i| i.id == id) {
                i.checked = on;
            }
        }
    }

    #[test]
    fn all_off_then_one_on_compiles_only_that_item() {
        use crate::ai::ui::logic::{toggle_all, toggled};
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = PlanState::default();
        for id in 1..=3u32 {
            let s = st.reserve_slots(1, id);
            let y = f64::from(id) * 4.0;
            let wall = NewElem::Wall {
                level: ed.level,
                a: (0.0, y),
                b: (6.0, y),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                wtype: None,
            };
            st.items.push(PlanItem {
                id,
                title: format!("item {id}"),
                why: String::new(),
                op: PlanOp::Create { slot0: s, items: vec![wall] },
                checked: true,
                depends_on: vec![],
                actor: "ai".into(),
                conflict: None,
                sets: Default::default(),
            });
        }
        st.next_item = 3;
        let off = toggle_all(&st.items, false);
        set_checked(&mut st, off);
        let one = toggled(&st.items, 2);
        assert_eq!(one, vec![(2, true)]);
        set_checked(&mut st, one);
        replay(&mut st, &mut ed, &Sets::default(), &[], &[]).unwrap();
        let compiled: Vec<u32> = st.steps.iter().map(|s| s.0).collect();
        assert_eq!(compiled, vec![2], "Apply carries only the one checked item");
        assert!(!st.compiled(0).steps.is_empty());
        let on: Vec<u32> = st.items.iter().filter(|i| i.checked).map(|i| i.id).collect();
        assert_eq!(on, vec![2]);
        assert!(st.items.iter().all(|i| i.conflict.is_none()));
    }
}
