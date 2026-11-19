//! A fix as plain data: a small envelope of serialisable ops. Until the shared op crate lands,
//! each `FixOp` maps one-to-one onto what `apply_check_fix` already runs:
//! `Delete` -> `Fix::Delete`; `SetOpening`/`SetParam`/`NewParam` -> `Edit::{Opening, ParamValue,
//! NewParam}`; `Clash*` -> `ClashFix::{Reroute, Opening, SlabHole}`. The envelope is applied as
//! one journal row, after a dry run that is rolled back.

use serde::{Deserialize, Serialize};

use super::Fix;
use crate::{ClashFix, Edit, Error, Issue, Model, OpeningSpec, PVal, ParamSpec, ParamType};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FixVal {
    Text(String),
    Int(i64),
    Num(f64),
    Bool(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixParamType {
    Text,
    Int,
    Number,
    Length,
    Area,
    Volume,
    YesNo,
}

impl From<ParamType> for FixParamType {
    fn from(t: ParamType) -> Self {
        match t {
            ParamType::Text => Self::Text,
            ParamType::Int => Self::Int,
            ParamType::Number => Self::Number,
            ParamType::Length => Self::Length,
            ParamType::Area => Self::Area,
            ParamType::Volume => Self::Volume,
            ParamType::YesNo => Self::YesNo,
        }
    }
}

impl From<FixParamType> for ParamType {
    fn from(t: FixParamType) -> Self {
        match t {
            FixParamType::Text => Self::Text,
            FixParamType::Int => Self::Int,
            FixParamType::Number => Self::Number,
            FixParamType::Length => Self::Length,
            FixParamType::Area => Self::Area,
            FixParamType::Volume => Self::Volume,
            FixParamType::YesNo => Self::YesNo,
        }
    }
}

impl From<&PVal> for FixVal {
    fn from(v: &PVal) -> Self {
        match v {
            PVal::Text(s) => Self::Text(s.clone()),
            PVal::Int(i) => Self::Int(*i),
            PVal::Num(x) => Self::Num(*x),
            PVal::Bool(b) => Self::Bool(*b),
        }
    }
}

impl From<&FixVal> for PVal {
    fn from(v: &FixVal) -> Self {
        match v {
            FixVal::Text(s) => Self::Text(s.clone()),
            FixVal::Int(i) => Self::Int(*i),
            FixVal::Num(x) => Self::Num(*x),
            FixVal::Bool(b) => Self::Bool(*b),
        }
    }
}

/// Lengths in metres, same as the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FixOp {
    Delete {
        table: String,
        id: i64,
    },
    SetOpening {
        id: i64,
        wall: i64,
        along: f64,
        width: f64,
        sill: f64,
        height: f64,
        kind: String,
    },
    SetParam {
        param: String,
        kind: String,
        id: i64,
        value: Option<FixVal>,
    },
    NewParam {
        name: String,
        ptype: FixParamType,
        per_type: bool,
        unit: String,
        group: String,
        guid: Option<String>,
        categories: Vec<String>,
    },
    /// New axis points of an existing network run; the other run settings stay as they are.
    ClashReroute {
        run: i64,
        points: Vec<[f64; 3]>,
    },
    ClashOpening {
        wall: i64,
        along: f64,
        width: f64,
        sill: f64,
        height: f64,
        kind: String,
    },
    ClashSlabHole {
        slab: i64,
        outer: Vec<[f64; 2]>,
        holes: Vec<Vec<[f64; 2]>>,
    },
}

/// One user action: all ops apply together or not at all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixEnvelope {
    pub v: u32,
    /// Locale key of the one-line title shown on the preview.
    pub title_key: String,
    pub ops: Vec<FixOp>,
}

pub const ENVELOPE_VERSION: u32 = 1;

fn pt2(p: &[(f64, f64)]) -> Vec<[f64; 2]> {
    p.iter().map(|q| [q.0, q.1]).collect()
}

fn from_pt2(p: &[[f64; 2]]) -> Vec<(f64, f64)> {
    p.iter().map(|q| (q[0], q[1])).collect()
}

fn edit_op(e: &Edit) -> Option<FixOp> {
    Some(match e {
        Edit::Opening { id, wall, spec } => FixOp::SetOpening {
            id: *id,
            wall: *wall,
            along: spec.along,
            width: spec.width,
            sill: spec.sill,
            height: spec.height,
            kind: spec.kind.clone(),
        },
        Edit::ParamValue {
            param,
            kind,
            id,
            value,
        } => FixOp::SetParam {
            param: param.clone(),
            kind: kind.clone(),
            id: *id,
            value: value.as_ref().map(FixVal::from),
        },
        Edit::NewParam(s) => FixOp::NewParam {
            name: s.name.clone(),
            ptype: s.ptype.into(),
            per_type: s.per_type,
            unit: s.unit.clone(),
            group: s.group.clone(),
            guid: s.guid.clone(),
            categories: s.categories.clone(),
        },
        _ => return None,
    })
}

fn opening_spec(along: f64, width: f64, sill: f64, height: f64, kind: &str) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.to_string(),
    }
}

impl Fix {
    /// The fix as data; `None` when there is nothing to run or it has no serialisable form.
    pub fn envelope(&self) -> Option<FixEnvelope> {
        let (title_key, ops) = match self {
            Fix::None => return None,
            Fix::Delete(keys) => (
                "ai.fix.delete",
                keys.iter()
                    .map(|k| FixOp::Delete {
                        table: k.0.clone(),
                        id: k.1,
                    })
                    .collect::<Vec<_>>(),
            ),
            Fix::Edits(list) => (
                "ai.fix.edit",
                list.iter().map(edit_op).collect::<Option<Vec<_>>>()?,
            ),
            Fix::Clash(c) => (
                "ai.fix.clash",
                vec![match c {
                    ClashFix::Reroute { run, spec } => FixOp::ClashReroute {
                        run: *run,
                        points: spec.points.iter().map(|p| [p.0, p.1, p.2]).collect(),
                    },
                    ClashFix::Opening { wall, spec } => FixOp::ClashOpening {
                        wall: *wall,
                        along: spec.along,
                        width: spec.width,
                        sill: spec.sill,
                        height: spec.height,
                        kind: spec.kind.clone(),
                    },
                    ClashFix::SlabHole { slab, outer, holes } => FixOp::ClashSlabHole {
                        slab: *slab,
                        outer: pt2(outer),
                        holes: holes.iter().map(|h| pt2(h)).collect(),
                    },
                }],
            ),
        };
        if ops.is_empty() {
            return None;
        }
        Some(FixEnvelope {
            v: ENVELOPE_VERSION,
            title_key: title_key.to_string(),
            ops,
        })
    }
}

impl FixEnvelope {
    /// Back to the executable form. One envelope holds one family of ops.
    pub fn to_fix(&self, m: &Model) -> Result<Fix, Error> {
        if self.v != ENVELOPE_VERSION {
            return Err(Error::Invalid(format!("fix envelope version {}", self.v)));
        }
        let two = || Error::Invalid("fix envelope: two clash ops".into());
        let (mut edits, mut dels, mut clash) = (vec![], vec![], None);
        for op in &self.ops {
            match op {
                FixOp::Delete { table, id } => dels.push((table.clone(), *id)),
                FixOp::SetOpening {
                    id,
                    wall,
                    along,
                    width,
                    sill,
                    height,
                    kind,
                } => edits.push(Edit::Opening {
                    id: *id,
                    wall: *wall,
                    spec: opening_spec(*along, *width, *sill, *height, kind),
                }),
                FixOp::SetParam {
                    param,
                    kind,
                    id,
                    value,
                } => edits.push(Edit::ParamValue {
                    param: param.clone(),
                    kind: kind.clone(),
                    id: *id,
                    value: value.as_ref().map(PVal::from),
                }),
                FixOp::NewParam {
                    name,
                    ptype,
                    per_type,
                    unit,
                    group,
                    guid,
                    categories,
                } => edits.push(Edit::NewParam(ParamSpec {
                    name: name.clone(),
                    ptype: (*ptype).into(),
                    per_type: *per_type,
                    unit: unit.clone(),
                    group: group.clone(),
                    guid: guid.clone(),
                    categories: categories.clone(),
                })),
                FixOp::ClashReroute { run, points } => {
                    let mut spec = m.mep_run(*run)?;
                    spec.points = points.iter().map(|p| (p[0], p[1], p[2])).collect();
                    if clash.replace(ClashFix::Reroute { run: *run, spec }).is_some() {
                        return Err(two());
                    }
                }
                FixOp::ClashOpening {
                    wall,
                    along,
                    width,
                    sill,
                    height,
                    kind,
                } => {
                    let spec = opening_spec(*along, *width, *sill, *height, kind);
                    if clash
                        .replace(ClashFix::Opening { wall: *wall, spec })
                        .is_some()
                    {
                        return Err(two());
                    }
                }
                FixOp::ClashSlabHole { slab, outer, holes } => {
                    let fix = ClashFix::SlabHole {
                        slab: *slab,
                        outer: from_pt2(outer),
                        holes: holes.iter().map(|h| from_pt2(h)).collect(),
                    };
                    if clash.replace(fix).is_some() {
                        return Err(two());
                    }
                }
            }
        }
        let families = usize::from(!edits.is_empty())
            + usize::from(!dels.is_empty())
            + usize::from(clash.is_some());
        if families != 1 {
            return Err(Error::Invalid("fix envelope: empty or mixed ops".into()));
        }
        Ok(match clash {
            Some(c) => Fix::Clash(c),
            None if !dels.is_empty() => Fix::Delete(dels),
            None => Fix::Edits(edits),
        })
    }
}

/// What a fix would do, measured on a scratch copy of the model.
#[derive(Clone, Debug, PartialEq)]
pub struct FixDryRun {
    /// Journal rows the fix wrote on the copy (one per fix).
    pub rows: usize,
    /// Integrity problems the fix would introduce; any of them blocks the apply.
    pub regressions: Vec<Issue>,
}

/// A throw-away copy of the model in the temp dir, removed on drop.
struct Scratch {
    model: Option<Model>,
    path: std::path::PathBuf,
}

impl Scratch {
    fn of(live: &Model) -> Result<Self, Error> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("ddc-fix-dry-{}-{n}.db", std::process::id());
        let mut s = Scratch {
            model: None,
            path: std::env::temp_dir().join(name),
        };
        s.remove_files();
        live.snapshot_to(&s.path)?;
        s.model = Some(Model::open_copy(&s.path.to_string_lossy())?);
        Ok(s)
    }

    fn model(&mut self) -> &mut Model {
        self.model.as_mut().expect("scratch model is open")
    }

    fn remove_files(&self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // close the connection first: an open file cannot be removed on Windows
        self.model = None;
        self.remove_files();
    }
}

impl Model {
    /// Runs the envelope on a scratch copy and checks integrity against the live state.
    /// The live file is never written, so a crash here cannot leave an un-applied fix on disk.
    pub fn dry_run_fix(&self, env: &FixEnvelope) -> Result<FixDryRun, Error> {
        let fix = env.to_fix(self)?;
        let base = self.check_integrity()?;
        let mut scratch = Scratch::of(self)?;
        let m = scratch.model();
        let top = m.op_top()?;
        m.apply_check_fix(&fix)?;
        let regressions = m.integrity_regressions(&base)?;
        let rows: i64 =
            m.sql()
                .query_row("SELECT count(*) FROM op WHERE seq>?1", [top], |r| r.get(0))?;
        Ok(FixDryRun {
            rows: rows as usize,
            regressions,
        })
    }

    /// Dry run first; applies as one journal row only when the dry run is clean.
    pub fn apply_fix(&mut self, env: &FixEnvelope) -> Result<FixDryRun, Error> {
        let dry = self.dry_run_fix(env)?;
        if let Some(i) = dry.regressions.first() {
            return Err(Error::Invalid(format!(
                "fix would break {} {}: {}",
                i.table, i.id, i.problem
            )));
        }
        let fix = env.to_fix(self)?;
        self.apply_check_fix(&fix)?;
        Ok(dry)
    }
}
