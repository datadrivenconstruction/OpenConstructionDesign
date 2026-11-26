// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `Editor::run_ops`: model edits as data (docs/design/AI_NATIVE.md section 2, "UI tools").
//!
//! An envelope is applied, previewed and replayed through one seam, [`EnvelopeExec`]. The
//! lowering itself (`Model::apply_envelope`, model/src/ops_apply.rs) lives in the model crate;
//! until it is linked the default executor refuses every envelope, so nothing changes.
//! Contract of an executor: on `Err` the model is unchanged; on `Ok` it wrote journal entries
//! (one, when it runs inside `Model::atomic`). The editor checks both and repairs a breach.
//!
//! Tool builders (`wall_envelope`, `array_*_envelope`) emit the envelope a tool would run.
//! The wall and arc wall tools commit through `run_ops`; the others keep their `run_op` closures for now.

use super::{Cmd, Editor, Gesture};
use ddc_geom::V2;
use ddc_model::{Model, Spacing as ModelSpacing, Target, WallSpec};
use ddc_ops::{
    Actor, Angle, Cat, Dir3, Envelope, Field, Group, Len, NewEl, Op, Pattern, PatternOut, Place,
    Ref, Spacing, Targets, Unit, Value, P3,
};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

/// What an applied envelope did, in stable refs: the model's own receipt (with `copied`,
/// `loss` and `journal_seq`).
pub type OpsReceipt = ddc_model::Receipt;

/// The seam to the model's envelope lowering.
pub trait EnvelopeExec: Send + Sync {
    fn apply(&self, m: &mut Model, env: &Envelope) -> Result<OpsReceipt, ddc_model::Error>;
}

/// Default until the model lowering is linked: refuses, the model stays as it is.
struct Unlinked;

impl EnvelopeExec for Unlinked {
    fn apply(&self, _: &mut Model, _: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
        Err(ddc_model::Error::Invalid(
            "ops: no envelope executor is linked".into(),
        ))
    }
}

static EXEC: OnceLock<Arc<dyn EnvelopeExec>> = OnceLock::new();

thread_local! {
    // tests install their own executor per thread, so parallel tests do not see each other
    static LOCAL: std::cell::RefCell<Option<Arc<dyn EnvelopeExec>>> =
        const { std::cell::RefCell::new(None) };
}

/// Link the process-wide executor once (at start-up). A second call is ignored.
pub fn link_exec(x: Arc<dyn EnvelopeExec>) {
    let _ = EXEC.set(x);
}

/// Executor for the current thread only (tests, headless runs); `None` removes it.
pub fn set_thread_exec(x: Option<Arc<dyn EnvelopeExec>>) {
    LOCAL.with(|l| *l.borrow_mut() = x);
}

fn exec() -> Arc<dyn EnvelopeExec> {
    LOCAL
        .with(|l| l.borrow().clone())
        .or_else(|| EXEC.get().cloned())
        .unwrap_or_else(|| Arc::new(Unlinked))
}

/// A dry run: what the envelope would do on this model. The model was restored before this
/// value exists; ghosts and the card are drawn from it. `apply_preview` or `cancel_preview`.
#[derive(Clone, Debug)]
pub struct OpsPreview {
    pub env: Arc<Envelope>,
    pub receipt: OpsReceipt,
    /// Tables whose rows the envelope wrote (from the change journal, not from the op).
    pub touched: BTreeSet<String>,
    base_top: i64,
    base_head: u64,
}

impl Editor {
    /// Apply an envelope as one undo step; redo replays the same envelope.
    /// On error the model, the undo and the redo stacks are unchanged.
    pub fn run_ops(&mut self, env: Envelope) -> Result<OpsReceipt, ddc_model::Error> {
        let env = Arc::new(self.frozen(env)?);
        let dups = crate::room_watch::snapshot(&self.model);
        let (receipt, n) = self.exec_ops(&env)?;
        if n == 0 {
            // nothing written: an undo step here would undo an earlier edit
            self.status = Some(crate::tr("edst.no_changes", &[]));
            return Ok(receipt);
        }
        // the envelope is in the model journal: the undo entry goes first, a failed reload keeps it
        crate::script_rec::note(self.undo_entries().0 + 1, &env, &receipt.created);
        self.push(Cmd::Ops(env, n));
        self.reload()?;
        crate::room_watch::note(&mut self.status, &self.model, &dups);
        self.settle_status();
        Ok(receipt)
    }

    /// Dry run on a private scratch copy: the live file is never written without Apply, so a
    /// crash during a preview cannot leave an un-applied edit on disk. Undo and redo stay as is.
    pub fn preview_ops(&mut self, env: Envelope) -> Result<OpsPreview, ddc_model::Error> {
        // frozen here: Apply runs these refs, never the filter read again
        let env = Arc::new(self.frozen(env)?);
        let top = self.model.op_top()?;
        let head = self.model.change_head();
        let mut scratch = Scratch::of(&self.model)?;
        let m = scratch.model();
        let shead = m.change_head();
        let (receipt, _) = exec_on(m, &env)?;
        let ch = m.changes_since(shead);
        let mut touched: BTreeSet<String> = ch.rows.keys().cloned().collect();
        if ch.all {
            touched.insert("*".into());
        }
        Ok(OpsPreview {
            env,
            receipt,
            touched,
            base_top: top,
            base_head: head,
        })
    }

    /// Apply a preview the person accepted. Refused when the model changed since the preview.
    pub fn apply_preview(&mut self, p: OpsPreview) -> Result<OpsReceipt, ddc_model::Error> {
        if self.model.op_top()? != p.base_top || self.model.change_head() != p.base_head {
            return Err(ddc_model::Error::Invalid(
                "ops: the model changed since the preview".into(),
            ));
        }
        self.run_ops(Arc::unwrap_or_clone(p.env))
    }

    /// Drop a preview. The model was already restored by `preview_ops`.
    pub fn cancel_preview(&mut self, p: OpsPreview) {
        drop(p);
    }

    /// Redo of `Cmd::Ops`: the same envelope on the same base.
    pub(super) fn replay_ops(&mut self, env: &Arc<Envelope>) -> Result<Cmd, ddc_model::Error> {
        let (_, n) = self.exec_ops(env)?;
        self.reload()?;
        Ok(Cmd::Ops(env.clone(), n))
    }

    /// Filter and set targets as the refs they match now (ai/freeze.rs). The frozen copy is
    /// what runs, is journaled and is replayed by redo; nothing matched refuses the envelope.
    fn frozen(&self, env: Envelope) -> Result<Envelope, ddc_model::Error> {
        if !crate::ai::freeze::needs_freeze(&env) {
            return Ok(env);
        }
        crate::ai::freeze::freeze(self, None, &env)
            .map_err(|r| ddc_model::Error::Invalid(r.to_string()))
    }

    /// Run the executor and count its journal entries; an error leaves no entry behind.
    fn exec_ops(&mut self, env: &Envelope) -> Result<(OpsReceipt, u32), ddc_model::Error> {
        exec_on(&mut self.model, env)
    }

    /// Envelope of the wall tool for straight segments of the current type, level and height.
    pub fn wall_envelope(&self, segs: &[(V2, V2)]) -> Option<Envelope> {
        self.tool_walls_envelope(segs, &[])
    }

    /// Envelope of the arc wall tool: one wall a->b with a non-zero `bulge` field.
    pub fn arc_wall_envelope(&self, a: V2, b: V2, bulge: f64) -> Option<Envelope> {
        self.tool_walls_envelope(&[(a, b)], &[bulge])
    }

    fn tool_walls_envelope(&self, segs: &[(V2, V2)], bulges: &[f64]) -> Option<Envelope> {
        let t = self.current_type()?;
        let specs: Vec<WallSpec> = segs
            .iter()
            .map(|(a, b)| WallSpec {
                a: (a.x, a.y),
                b: (b.x, b.y),
                level: self.level,
                base_offset: 0.0,
                height: self.height,
                wall_type: t.id,
                offset0: None,
            })
            .collect();
        let z = self.elevation;
        walls_envelope(&specs, bulges, |_| z)
    }

    /// Arc wall of the current type through `run_ops` (actor User, one journal entry);
    /// `Cmd::Add` only when the envelope is refused, with the reason in the status.
    pub fn commit_arc_wall(&mut self, a: V2, b: V2, bulge: f64) {
        let Some(t) = self.current_type() else {
            self.status = Some(crate::tr("edst.no_wall_type", &[]));
            return;
        };
        let wall_type = t.id;
        let mut refused = None;
        if let Some(env) = self.arc_wall_envelope(a, b, bulge) {
            let steps = self.undo.len();
            match self.run_ops(env) {
                Ok(_) => {
                    self.status = Some(crate::tr("edst.arc_wall_added", &[]));
                    return;
                }
                // the step is recorded and only the reload failed: a fallback would add twice
                Err(e) if self.undo.len() > steps => {
                    self.status =
                        Some(crate::tr("edst.wall_not_built", &[("e", &e.to_string())]));
                    return;
                }
                Err(e) => refused = Some(e.to_string()),
            }
        }
        let g = Gesture {
            bulges: vec![bulge],
            specs: vec![WallSpec {
                a: (a.x, a.y),
                b: (b.x, b.y),
                level: self.level,
                base_offset: 0.0,
                height: self.height,
                wall_type,
                offset0: None,
            }],
        };
        match self.apply_add(&g) {
            Ok(ids) => {
                self.status = Some(match &refused {
                    Some(why) => {
                        crate::tr("edst.walls_added_unjournaled", &[("n", "1"), ("e", why)])
                    }
                    None => crate::tr("edst.arc_wall_added", &[]),
                });
                self.push(Cmd::Add(g, ids));
            }
            Err(e) => {
                self.status = Some(crate::tr("edst.wall_not_built", &[("e", &e.to_string())]))
            }
        }
    }

    /// Envelope of the spline wall tool: one wall from the first to the last point with the
    /// points between as `through`, of the current type, level and height.
    pub fn spline_wall_envelope(&self, pts: &[V2]) -> Option<Envelope> {
        let t = self.current_type()?;
        let (a, b) = (pts.first()?, pts.last()?);
        let z = self.elevation;
        let mid: Vec<[f64; 2]> = pts[1..pts.len() - 1].iter().map(|p| [p.x, p.y]).collect();
        let op = Op::Create {
            slot: 0,
            el: NewEl::Wall {
                level: Ref::id(Cat::Level, self.level),
                a: Place::At { p: P3::m(a.x, a.y, z) },
                b: Place::At { p: P3::m(b.x, b.y, z) },
                height: Len::m(self.height),
                top: None,
                ty: Some(Ref::id(Cat::WallType, t.id)),
                fields: vec![Field::new("through", Value::plain(serde_json::json!(mid)))],
            },
        };
        Some(envelope("spline wall", vec![op]))
    }

    /// Spline wall of the current type through `run_ops` (actor User, one journal entry).
    /// A refused envelope adds nothing: the reason goes to the status.
    pub fn commit_spline_wall(&mut self, pts: &[V2]) {
        let Some(env) = self.spline_wall_envelope(pts) else {
            self.status = Some(crate::tr("edst.no_wall_type", &[]));
            return;
        };
        self.status = Some(match self.run_ops(env) {
            Ok(_) => crate::tr("spline.wt.added", &[]),
            Err(e) => crate::tr("edst.wall_not_built", &[("e", &e.to_string())]),
        });
    }
}

fn exec_on(m: &mut Model, env: &Envelope) -> Result<(OpsReceipt, u32), ddc_model::Error> {
    let top = m.op_top()?;
    match exec().apply(m, env) {
        Ok(r) => {
            let n = m.journal_len_since(top)?;
            Ok((r, n))
        }
        Err(e) => {
            if m.op_top()? > top {
                m.rollback_to(top)?;
            }
            Err(e)
        }
    }
}

/// A throw-away copy of the model in the temp dir, removed on drop.
struct Scratch {
    model: Option<Model>,
    path: std::path::PathBuf,
}

impl Scratch {
    fn of(live: &Model) -> Result<Self, ddc_model::Error> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("ddc-ops-preview-{}-{n}.db", ddc_model::process_id());
        let path = ddc_model::temp_dir().join(name);
        let mut s = Scratch { model: None, path };
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

trait JournalSince {
    fn journal_len_since(&self, top: i64) -> Result<u32, ddc_model::Error>;
}

impl JournalSince for Model {
    fn journal_len_since(&self, top: i64) -> Result<u32, ddc_model::Error> {
        let n: i64 = self.sql().query_row(
            "SELECT count(*) FROM op WHERE seq>?1",
            [top],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }
}

fn envelope(title: &str, ops: Vec<Op>) -> Envelope {
    let mut env = Envelope::new(Actor::User, title);
    env.groups.push(Group {
        id: 0,
        title: title.to_owned(),
        ops,
        depends_on: vec![],
        why: String::new(),
    });
    env
}

fn len_m(v: f64) -> Value {
    Value::num(v, Unit::M)
}

/// Walls as `create` ops, slot k for spec k. Points are absolute: z = level elevation
/// (`elev`) + base offset, which the lowering turns back into the base offset. `offset0` and
/// a non-zero `bulge` (per spec, empty = all straight) travel as fields.
pub fn walls_envelope(
    specs: &[WallSpec],
    bulges: &[f64],
    elev: impl Fn(i64) -> f64,
) -> Option<Envelope> {
    if specs.is_empty() {
        return None;
    }
    let ops = specs
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let mut fields = vec![];
            if let Some(o) = s.offset0 {
                fields.push(Field::new("offset0", len_m(o)));
            }
            if let Some(&b) = bulges.get(k).filter(|b| **b != 0.0) {
                fields.push(Field::new("bulge", Value::plain(serde_json::json!(b))));
            }
            let z = elev(s.level) + s.base_offset;
            Op::Create {
                slot: k as u32,
                el: NewEl::Wall {
                    level: Ref::id(Cat::Level, s.level),
                    a: Place::At { p: P3::m(s.a.0, s.a.1, z) },
                    b: Place::At { p: P3::m(s.b.0, s.b.1, z) },
                    height: Len::m(s.height),
                    top: None,
                    ty: Some(Ref::id(Cat::WallType, s.wall_type)),
                    fields,
                },
            }
        })
        .collect();
    Some(envelope("walls", ops))
}

/// Stable ref of a modify target; `None` for kinds the op language has no category for yet.
pub fn target_ref(t: &Target) -> Option<Ref> {
    let (cat, id) = match t {
        Target::Wall(i) => (Cat::Wall, *i),
        Target::Opening(i) => (Cat::Opening, *i),
        Target::Curtain(i) => (Cat::CurtainWall, *i),
        Target::Slab(i) => (Cat::Slab, *i),
        Target::Room(i) => (Cat::Room, *i),
        Target::Component(i) => (Cat::Component, *i),
        Target::MepRun(i) => (Cat::MepRun, *i),
        Target::Solid(i) => (Cat::Solid, *i),
        Target::Mesh(i) => (Cat::Mesh, *i),
        Target::Column(i) => (Cat::Column, *i),
        Target::Beam(i) => (Cat::Beam, *i),
        Target::Ceiling(i) => (Cat::Ceiling, *i),
        Target::Ramp(i) => (Cat::Ramp, *i),
        Target::Roof(i) => (Cat::Roof, *i),
        Target::Shaft(i) => (Cat::Shaft, *i),
        Target::Grid(i) => (Cat::Grid, *i),
        Target::RefPlane(i) => (Cat::RefPlane, *i),
        Target::Footing(i) => (Cat::Footing, *i),
        Target::StripFooting(i) => (Cat::StripFooting, *i),
        Target::Raft(i) => (Cat::Raft, *i),
        Target::Brace(i) => (Cat::Brace, *i),
        Target::Truss(i) => (Cat::Truss, *i),
        Target::BeamSystem(i) => (Cat::BeamSystem, *i),
        Target::Mass(i) => (Cat::Mass, *i),
        Target::Topo(i) => (Cat::Topography, *i),
        _ => return None,
    };
    Some(Ref::id(cat, id))
}

fn source(ts: &[Target]) -> Option<Targets> {
    let refs = ts.iter().map(target_ref).collect::<Option<Vec<_>>>()?;
    (!refs.is_empty()).then_some(Targets::Refs { refs })
}

/// Linear array of the modify tool as one `pattern` op. `count` includes the source, as in
/// `Model::array_linear`; `Step` is the vector between neighbours, `Fit` first to last.
pub fn array_linear_envelope(ts: &[Target], count: usize, sp: ModelSpacing) -> Option<Envelope> {
    let (v, fit) = match sp {
        ModelSpacing::Step(v) => (v, false),
        ModelSpacing::Fit(v) => (v, true),
    };
    let l = v.0.hypot(v.1);
    if count < 2 || l <= 1e-12 || !l.is_finite() {
        return None;
    }
    let dir = Dir3 { x: v.0 / l, y: v.1 / l, z: 0.0 };
    let spacing = if fit {
        Spacing::Fit { total: Len::m(l) }
    } else {
        Spacing::Step { d: Len::m(l) }
    };
    let op = Op::Pattern {
        source: source(ts)?,
        pattern: Pattern::Linear { count: count as u32, dir, spacing },
        out: PatternOut::Copies,
    };
    Some(envelope("array", vec![op]))
}

/// Radial array about the plan point `c` (vertical axis): `count` with the source over the
/// sweep `total` in radians, a full turn spacing them evenly, as in `Model::array_radial`.
pub fn array_radial_envelope(
    ts: &[Target],
    count: usize,
    c: (f64, f64),
    total: f64,
) -> Option<Envelope> {
    if count < 2 || !total.is_finite() || total == 0.0 {
        return None;
    }
    let op = Op::Pattern {
        source: source(ts)?,
        pattern: Pattern::Radial {
            center: P3::m(c.0, c.1, 0.0),
            axis: Dir3::Z,
            count: count as u32,
            angle: Angle::rad(total),
        },
        out: PatternOut::Copies,
    };
    Some(envelope("array", vec![op]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;
    use ddc_ops::LevelOp;

    /// Test executor over real model calls: `level create` only, one atomic journal entry.
    struct Levels;

    impl EnvelopeExec for Levels {
        fn apply(&self, m: &mut Model, env: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
            m.atomic("ops", |m| {
                let mut r = OpsReceipt::default();
                for op in env.ops() {
                    match op {
                        Op::Level(LevelOp::Create { slot, name, elevation }) => {
                            let id = m.add_level(name, elevation.to_m())?;
                            r.created.push((*slot, Ref::id(Cat::Level, id)));
                        }
                        _ => return Err(ddc_model::Error::Invalid("test: op".into())),
                    }
                }
                Ok(r)
            })
        }
    }

    /// Breaks the contract: writes, then fails outside a transaction.
    struct Leaky;

    impl EnvelopeExec for Leaky {
        fn apply(&self, m: &mut Model, _: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
            m.add_level("Leak", 7.0)?;
            Err(ddc_model::Error::Invalid("test: failed after a write".into()))
        }
    }

    fn levels_env(names: &[(&str, f64)]) -> Envelope {
        let ops = names
            .iter()
            .enumerate()
            .map(|(k, (n, z))| {
                Op::Level(LevelOp::Create {
                    slot: k as u32,
                    name: (*n).to_owned(),
                    elevation: Len::m(*z),
                })
            })
            .collect();
        envelope("levels", ops)
    }

    fn ed(x: Arc<dyn EnvelopeExec>) -> Editor {
        set_thread_exec(Some(x));
        Editor::new_in_memory().unwrap()
    }

    fn hash(e: &Editor) -> u64 {
        e.model.content_hash().unwrap()
    }

    /// Model content, journal and both stacks.
    fn state(e: &Editor) -> (u64, i64, usize, usize, usize) {
        (
            hash(e),
            e.model.op_top().unwrap(),
            e.model.journal_len().unwrap(),
            e.undo.len(),
            e.redo.len(),
        )
    }

    #[test]
    fn unlinked_executor_refuses_and_changes_nothing() {
        set_thread_exec(None);
        let mut e = Editor::new_in_memory().unwrap();
        let s0 = state(&e);
        assert!(e.run_ops(levels_env(&[("L2", 3.0)])).is_err());
        assert_eq!(state(&e), s0);
    }

    #[test]
    fn cmd_ops_undo_redo_exact() {
        let mut e = ed(Arc::new(Levels));
        let (h0, j0) = (hash(&e), e.model.journal_len().unwrap());
        let r = e.run_ops(levels_env(&[("L2", 3.0), ("L3", 6.0)])).unwrap();
        assert_eq!(r.created.len(), 2);
        let h1 = hash(&e);
        assert_ne!(h1, h0);
        assert_eq!(e.model.journal_len().unwrap(), j0 + 1, "one envelope, one entry");
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(_, 1))));
        e.undo();
        assert_eq!(hash(&e), h0, "undo restores the base exactly");
        assert_eq!(e.model.journal_len().unwrap(), j0);
        assert_eq!(e.redo.len(), 1);
        e.redo();
        assert_eq!(hash(&e), h1, "redo replays the envelope exactly");
        assert_eq!(e.redo.len(), 0);
        e.undo();
        assert_eq!(hash(&e), h0);
    }

    #[test]
    fn failed_envelope_leaves_model_and_history_unchanged() {
        let mut e = ed(Arc::new(Leaky));
        let s0 = state(&e);
        assert!(e.run_ops(levels_env(&[("L2", 3.0)])).is_err());
        assert_eq!(state(&e), s0, "a write before the error is rolled back");
    }

    #[test]
    fn preview_then_cancel_leaves_model_and_history_unchanged() {
        let mut e = ed(Arc::new(Levels));
        // a redo entry must survive a preview: push clears redo, a preview must not push
        e.run_ops(levels_env(&[("L2", 3.0)])).unwrap();
        e.undo();
        let s0 = state(&e);
        assert_eq!(s0.4, 1);
        let p = e.preview_ops(levels_env(&[("L9", 9.0)])).unwrap();
        assert_eq!(p.receipt.created.len(), 1);
        assert!(p.touched.contains("level"), "touched: {:?}", p.touched);
        assert_eq!(state(&e), s0, "the preview never touches the live model");
        e.cancel_preview(p);
        assert_eq!(state(&e), s0);
    }

    /// Records which model it ran on, then writes to it.
    struct Spy(std::sync::Mutex<Vec<String>>);

    impl EnvelopeExec for Spy {
        fn apply(&self, m: &mut Model, env: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
            self.0.lock().unwrap().push(m.sql().path().unwrap_or("").to_owned());
            Levels.apply(m, env)
        }
    }

    #[test]
    fn preview_runs_on_a_scratch_copy_never_on_the_live_model() {
        let spy = Arc::new(Spy(Default::default()));
        let mut e = ed(spy.clone());
        let (s0, head) = (state(&e), e.model.change_head());
        let p = e.preview_ops(levels_env(&[("L9", 9.0)])).unwrap();
        assert_eq!((state(&e), e.model.change_head()), (s0, head), "live model untouched");
        let seen = spy.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("ddc-ops-preview-"), "ran on {seen:?}");
        assert!(!std::path::Path::new(&seen[0]).exists(), "scratch removed");
        e.apply_preview(p).unwrap();
        assert_eq!(spy.0.lock().unwrap().len(), 2, "apply runs once on the live model");
        assert_ne!(hash(&e), s0.0);

        let mut l = ed(Arc::new(Leaky));
        let s0 = state(&l);
        assert!(l.preview_ops(levels_env(&[("L2", 3.0)])).is_err());
        assert_eq!(state(&l), s0);
    }

    #[test]
    fn preview_apply_equals_direct_run_and_stale_preview_is_refused() {
        let env = levels_env(&[("L2", 3.0)]);
        let mut a = ed(Arc::new(Levels));
        let p = a.preview_ops(env.clone()).unwrap();
        a.apply_preview(p).unwrap();
        let mut b = ed(Arc::new(Levels));
        b.run_ops(env.clone()).unwrap();
        assert_eq!(hash(&a), hash(&b));
        assert_eq!(a.undo.len(), b.undo.len());

        // level names are unique: the stale preview adds a name not yet in the model
        let p = b.preview_ops(levels_env(&[("L3", 4.0)])).unwrap();
        b.run_ops(levels_env(&[("L5", 5.0)])).unwrap();
        let s = state(&b);
        assert!(b.apply_preview(p).is_err(), "the base moved");
        assert_eq!(state(&b), s);
    }

    #[test]
    fn replay_same_envelope_same_base_same_diff() {
        let env = levels_env(&[("L2", 3.0), ("L3", 6.0)]);
        let mut a = ed(Arc::new(Levels));
        let mut b = ed(Arc::new(Levels));
        assert_eq!(hash(&a), hash(&b), "same base");
        let pa = a.preview_ops(env.clone()).unwrap();
        let pb = b.preview_ops(env.clone()).unwrap();
        assert_eq!(pa.receipt, pb.receipt);
        assert_eq!(pa.touched, pb.touched);
        let ra = a.run_ops(env.clone()).unwrap();
        let rb = b.run_ops(env).unwrap();
        assert_eq!(ra, rb);
        assert_eq!(hash(&a), hash(&b), "same envelope, same base: same result");
        // replay by redo on the same model lands on the same content
        let h1 = hash(&a);
        a.undo();
        a.redo();
        assert_eq!(hash(&a), h1);
    }

    #[test]
    fn wall_tool_emits_envelope() {
        set_thread_exec(None);
        let mut e = Editor::new_in_memory().unwrap();
        e.current_type = 1;
        let t = e.current_type().unwrap().id;
        let segs = [
            (v2(0.0, 0.0), v2(6.0, 0.0)),
            (v2(6.0, 0.0), v2(6.0, 4.0)),
        ];
        let env = e.wall_envelope(&segs).unwrap();
        assert_eq!(env.actor, Actor::User);
        assert_eq!(env.op_count(), 2);
        let first = env.ops().next().unwrap();
        let Op::Create { slot, el: NewEl::Wall { level, a, b, height, ty, fields, .. } } = first
        else {
            panic!("not a wall create: {first:?}");
        };
        assert_eq!(*slot, 0);
        assert_eq!(*level, Ref::id(Cat::Level, e.level));
        assert_eq!(*a, Place::At { p: P3::m(0.0, 0.0, 0.0) });
        assert_eq!(*b, Place::At { p: P3::m(6.0, 0.0, 0.0) });
        assert_eq!(*height, Len::m(e.height));
        assert_eq!(*ty, Some(Ref::id(Cat::WallType, t)));
        assert!(fields.is_empty(), "the base offset rides on z: {fields:?}");
        let back = Envelope::from_json(&env.to_json().unwrap()).unwrap();
        assert_eq!(back, env, "the wire form round-trips");
        assert!(e.wall_envelope(&[]).is_none());
        let bent = walls_envelope(
            &[WallSpec {
                a: (0.0, 0.0),
                b: (2.0, 0.0),
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: Some(0.1),
            }],
            &[0.5],
            |_| 0.0,
        )
        .unwrap();
        let Some(Op::Create { el: NewEl::Wall { fields, .. }, .. }) = bent.ops().next() else {
            panic!("wall");
        };
        assert!(fields.iter().any(|f| f.name == "bulge"));
        assert!(fields.iter().any(|f| f.name == "offset0"));
    }

    /// Refuses every envelope, as the model lowering does for ops it cannot lower.
    struct Refuse;

    impl EnvelopeExec for Refuse {
        fn apply(&self, _: &mut Model, _: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
            Err(ddc_model::Error::Invalid("test: refused".into()))
        }
    }

    const SEGS: [(f64, f64, f64, f64); 2] = [(0.0, 0.0, 6.0, 0.0), (6.0, 0.0, 6.0, 4.0)];

    fn segs() -> Vec<(V2, V2)> {
        SEGS.iter().map(|s| (v2(s.0, s.1), v2(s.2, s.3))).collect()
    }

    fn wall_editor(x: Arc<dyn EnvelopeExec>) -> Editor {
        let mut e = ed(x);
        e.current_type = 1;
        e
    }

    /// Wall rows as the person sees them, ids included.
    fn wall_rows(e: &Editor) -> Vec<String> {
        let mut st = e
            .model
            .sql()
            .prepare("SELECT id,level,wtype,ax,ay,bx,by,base_offset,height,offset0 FROM wall ORDER BY id")
            .unwrap();
        let rows = st
            .query_map([], |r| {
                Ok(format!(
                    "{} {} {} {} {} {} {} {} {} {:?}",
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, f64>(5)?,
                    r.get::<_, f64>(6)?,
                    r.get::<_, f64>(7)?,
                    r.get::<_, f64>(8)?,
                    r.get::<_, Option<f64>>(9)?,
                ))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn wall_tool_commit_journal_has_envelope() {
        let mut e = wall_editor(Arc::new(crate::ops_exec::ModelExec));
        let j0 = e.model.journal_len().unwrap();
        e.commit_walls(segs());
        assert_eq!(e.walls.len(), 2);
        assert_eq!(e.model.journal_len().unwrap(), j0 + 1, "one journal entry");
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(env, 1)) if env.actor == Actor::User));
        let src = e.model.journal_src(e.model.op_top().unwrap()).unwrap();
        assert_eq!(src, e.wall_envelope(&segs()), "the forward envelope is journaled");
        assert_eq!(e.status, Some(crate::tr("edst.walls_added", &[("n", "2")])));
        assert_eq!(e.history().0.last().map(String::as_str), Some("hist.walls_add"));
        // the same walls as the plain add path
        let mut f = wall_editor(Arc::new(Refuse));
        f.commit_walls(segs());
        assert_eq!(wall_rows(&e), wall_rows(&f));
        assert_eq!(e.selection.len(), f.selection.len());
    }

    #[test]
    fn wall_tool_undo_redo_one_step() {
        let mut e = wall_editor(Arc::new(crate::ops_exec::ModelExec));
        let s0 = state(&e);
        e.commit_walls(segs());
        let (rows, h1) = (wall_rows(&e), hash(&e));
        assert_eq!(e.undo.len(), s0.3 + 1, "one undo step for the whole gesture");
        e.undo();
        assert_eq!(e.walls.len(), 0);
        assert_eq!(hash(&e), s0.0);
        assert_eq!(e.undo.len(), s0.3);
        e.redo();
        assert_eq!(e.walls.len(), 2);
        assert_eq!(wall_rows(&e), rows, "redo gives back the same walls and ids");
        assert_eq!(hash(&e), h1);
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(_, 1))));
    }

    #[test]
    fn refused_envelope_falls_back_to_add() {
        let mut e = wall_editor(Arc::new(Refuse));
        e.commit_walls(segs());
        assert_eq!(e.walls.len(), 2);
        assert!(matches!(e.undo.last(), Some(Cmd::Add(..))));
        let why = ddc_model::Error::Invalid("test: refused".into()).to_string();
        let want = crate::tr("edst.walls_added_unjournaled", &[("n", "2"), ("e", &why)]);
        assert_eq!(e.status, Some(want), "the status says why");
        e.undo();
        assert_eq!(e.walls.len(), 0);
        e.redo();
        assert_eq!(e.walls.len(), 2);
    }

    const ARC: (f64, f64, f64, f64, f64) = (0.0, 0.0, 4.0, 0.0, 0.5);

    fn arc(e: &mut Editor) {
        e.commit_arc_wall(v2(ARC.0, ARC.1), v2(ARC.2, ARC.3), ARC.4);
    }

    fn bulges(e: &Editor) -> Vec<f64> {
        let mut st = e.model.sql().prepare("SELECT bulge FROM wall ORDER BY id").unwrap();
        let rows = st.query_map([], |r| r.get::<_, f64>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn arc_wall_journal_has_envelope_with_bulge() {
        let mut e = wall_editor(Arc::new(crate::ops_exec::ModelExec));
        let j0 = e.model.journal_len().unwrap();
        arc(&mut e);
        assert_eq!(e.walls.len(), 1);
        assert_eq!(e.model.journal_len().unwrap(), j0 + 1, "one journal entry");
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(env, 1)) if env.actor == Actor::User));
        let env = e.arc_wall_envelope(v2(ARC.0, ARC.1), v2(ARC.2, ARC.3), ARC.4).unwrap();
        let Some(Op::Create { el: NewEl::Wall { fields, .. }, .. }) = env.ops().next() else {
            panic!("wall");
        };
        assert!(fields.iter().any(|f| f.name == "bulge"), "{fields:?}");
        let src = e.model.journal_src(e.model.op_top().unwrap()).unwrap();
        assert_eq!(src, Some(env), "the forward envelope is journaled");
        assert_eq!(e.status, Some(crate::tr("edst.arc_wall_added", &[])));
        assert_eq!(bulges(&e), vec![ARC.4]);
        // the same wall as the plain add path
        let mut f = wall_editor(Arc::new(Refuse));
        arc(&mut f);
        assert_eq!(wall_rows(&e), wall_rows(&f));
        assert_eq!(bulges(&e), bulges(&f));
    }

    #[test]
    fn arc_wall_undo_redo_one_step() {
        let mut e = wall_editor(Arc::new(crate::ops_exec::ModelExec));
        let s0 = state(&e);
        arc(&mut e);
        let (rows, h1) = (wall_rows(&e), hash(&e));
        assert_eq!(e.undo.len(), s0.3 + 1, "one undo step for the arc wall");
        e.undo();
        assert_eq!(e.walls.len(), 0);
        assert_eq!(hash(&e), s0.0);
        assert_eq!(e.undo.len(), s0.3);
        e.redo();
        assert_eq!(e.walls.len(), 1);
        assert_eq!(wall_rows(&e), rows, "redo gives back the same wall and id");
        assert_eq!(bulges(&e), vec![ARC.4]);
        assert_eq!(hash(&e), h1);
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(_, 1))));
    }

    #[test]
    fn arc_refused_envelope_falls_back_to_add() {
        let mut e = wall_editor(Arc::new(Refuse));
        arc(&mut e);
        assert_eq!(e.walls.len(), 1);
        assert_eq!(bulges(&e), vec![ARC.4]);
        assert!(matches!(e.undo.last(), Some(Cmd::Add(..))));
        let why = ddc_model::Error::Invalid("test: refused".into()).to_string();
        let want = crate::tr("edst.walls_added_unjournaled", &[("n", "1"), ("e", &why)]);
        assert_eq!(e.status, Some(want), "the status says why");
        e.undo();
        assert_eq!(e.walls.len(), 0);
        e.redo();
        assert_eq!(e.walls.len(), 1);
        assert_eq!(bulges(&e), vec![ARC.4]);
    }

    /// Applies through the real executor, then breaks what `reload` reads: the journal step
    /// is recorded and only the reload fails.
    struct BreakReload;

    impl EnvelopeExec for BreakReload {
        fn apply(&self, m: &mut Model, env: &Envelope) -> Result<OpsReceipt, ddc_model::Error> {
            let r = crate::ops_exec::ModelExec.apply(m, env)?;
            m.sql().execute_batch("DROP TABLE opening").expect("drop opening");
            Ok(r)
        }
    }

    fn wall_count(e: &Editor) -> i64 {
        e.model.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn refused_fallback_adds_exactly_once() {
        let mut e = wall_editor(Arc::new(Refuse));
        let (u0, j0) = (e.undo.len(), e.model.journal_len().unwrap());
        e.commit_walls(segs());
        assert_eq!(wall_count(&e), 2, "two segments, two walls, no duplicates");
        assert_eq!(e.undo.len(), u0 + 1, "one undo step");
        assert_eq!(e.model.journal_len().unwrap(), j0 + 1, "one journal entry");
        assert!(matches!(e.undo.last(), Some(Cmd::Add(..))));
        e.undo();
        assert_eq!(wall_count(&e), 0);
    }

    #[test]
    fn arc_refused_fallback_adds_exactly_once() {
        let mut e = wall_editor(Arc::new(Refuse));
        let (u0, j0) = (e.undo.len(), e.model.journal_len().unwrap());
        arc(&mut e);
        assert_eq!(wall_count(&e), 1);
        assert_eq!(e.undo.len(), u0 + 1);
        assert_eq!(e.model.journal_len().unwrap(), j0 + 1);
        assert!(matches!(e.undo.last(), Some(Cmd::Add(..))));
    }

    #[test]
    fn walls_reload_failure_after_journal_step_never_adds_again() {
        let mut e = wall_editor(Arc::new(BreakReload));
        let u0 = e.undo.len();
        e.commit_walls(segs());
        assert_eq!(wall_count(&e), 2, "no second copy of the walls");
        assert_eq!(e.undo.len(), u0 + 1, "the recorded step is kept");
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(..))), "no Cmd::Add fallback");
        let st = e.status.clone().unwrap_or_default();
        let head = crate::tr("edst.walls_not_built", &[("e", "")]);
        assert!(st.starts_with(&head) && st.len() > head.len(), "{st}");
    }

    #[test]
    fn arc_reload_failure_after_journal_step_never_adds_again() {
        let mut e = wall_editor(Arc::new(BreakReload));
        let u0 = e.undo.len();
        arc(&mut e);
        assert_eq!(wall_count(&e), 1, "no second copy of the wall");
        assert_eq!(e.undo.len(), u0 + 1, "the recorded step is kept");
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(..))), "no Cmd::Add fallback");
        let st = e.status.clone().unwrap_or_default();
        let head = crate::tr("edst.wall_not_built", &[("e", "")]);
        assert!(st.starts_with(&head) && st.len() > head.len(), "{st}");
    }

    #[test]
    fn arc_wall_without_type_changes_nothing() {
        let mut e = ed(Arc::new(crate::ops_exec::ModelExec));
        e.current_type = usize::MAX;
        assert!(e.current_type().is_none());
        let s0 = state(&e);
        arc(&mut e);
        assert_eq!(state(&e), s0);
        assert_eq!(e.status, Some(crate::tr("edst.no_wall_type", &[])));
    }

    #[test]
    fn modify_array_emits_pattern_op() {
        let ts = [Target::Wall(5), Target::Column(7)];
        let env = array_linear_envelope(&ts, 4, ModelSpacing::Step((2.0, 0.0))).unwrap();
        assert_eq!(env.op_count(), 1);
        assert_eq!(
            env.ops().next().unwrap(),
            &Op::Pattern {
                source: Targets::Refs {
                    refs: vec![Ref::id(Cat::Wall, 5), Ref::id(Cat::Column, 7)]
                },
                pattern: Pattern::Linear {
                    count: 4,
                    dir: Dir3::X,
                    spacing: Spacing::Step { d: Len::m(2.0) },
                },
                out: PatternOut::Copies,
            }
        );
        let fit = array_linear_envelope(&ts, 3, ModelSpacing::Fit((0.0, 6.0))).unwrap();
        let Some(Op::Pattern { pattern: Pattern::Linear { dir, spacing, .. }, .. }) =
            fit.ops().next()
        else {
            panic!("linear pattern");
        };
        assert_eq!(*dir, Dir3::Y);
        assert_eq!(*spacing, Spacing::Fit { total: Len::m(6.0) });
        let rad = array_radial_envelope(&ts, 6, (1.0, 2.0), std::f64::consts::TAU).unwrap();
        let Some(Op::Pattern { pattern: Pattern::Radial { center, axis, count, angle }, .. }) =
            rad.ops().next()
        else {
            panic!("radial pattern");
        };
        assert_eq!((*center, *axis, *count), (P3::m(1.0, 2.0, 0.0), Dir3::Z, 6));
        assert_eq!(*angle, Angle::rad(std::f64::consts::TAU));
        assert!(Envelope::from_json(&rad.to_json().unwrap()).is_ok());
        // refused shapes fall back to the closure path
        assert!(array_linear_envelope(&ts, 1, ModelSpacing::Step((1.0, 0.0))).is_none());
        assert!(array_linear_envelope(&ts, 3, ModelSpacing::Step((0.0, 0.0))).is_none());
        assert!(array_linear_envelope(&[Target::View(3)], 3, ModelSpacing::Step((1.0, 0.0)))
            .is_none());
        assert!(array_linear_envelope(&[], 3, ModelSpacing::Step((1.0, 0.0))).is_none());
    }

    #[test]
    fn spline_wall_tool_draws_one_journaled_spline_wall() {
        use crate::wall_tool::{Event, Key, Outcome};
        let mut e = wall_editor(Arc::new(crate::ops_exec::ModelExec));
        e.tool.toggle_spline_in(e.ix.draft_mut());
        for p in [v2(0.0, 0.0), v2(4.0, 1.0), v2(8.0, -1.0), v2(12.0, 0.0)] {
            assert_eq!(e.tool.handle_in(e.ix.draft_mut(), Event::Click(p)), Outcome::Nothing);
        }
        assert!(e.tool.preview_in(e.wall_draft()).is_empty(), "no straight draft in spline mode");
        assert!(e.draft_arc().len() > 4, "the axis is previewed as a curve");
        let Outcome::Spline(pts) = e.tool.handle_in(e.ix.draft_mut(), Event::Key(Key::Enter)) else {
            panic!("spline outcome");
        };
        assert_eq!(pts.len(), 4);
        assert!(e.wall_draft().points.is_empty());
        // two points are not a spline wall
        e.tool.handle_in(e.ix.draft_mut(), Event::Click(v2(0.0, 5.0)));
        e.tool.handle_in(e.ix.draft_mut(), Event::Click(v2(3.0, 5.0)));
        assert!(matches!(e.tool.handle_in(e.ix.draft_mut(), Event::Key(Key::Enter)), Outcome::Error(_)));
        let j0 = e.model.journal_len().unwrap();
        e.commit_spline_wall(&pts);
        assert_eq!(e.status, Some(crate::tr("spline.wt.added", &[])));
        assert_eq!(e.model.journal_len().unwrap(), j0 + 1, "one journal entry");
        assert!(matches!(e.undo.last(), Some(Cmd::Ops(env, 1)) if env.actor == Actor::User));
        let all = e.model.spline_walls(None).unwrap();
        assert_eq!(all.len(), 1);
        let (id, s) = &all[0];
        // the current type (`current_type` is an index into the list, not an id)
        let ty = e.current_type().unwrap().id;
        assert_eq!((s.level, s.wall_type, s.height), (e.level, ty, e.height));
        let n = e.model.spline_wall_pieces(*id).unwrap().len();
        assert!(n >= 2 && e.walls.len() == n, "{n} pieces, {} walls listed", e.walls.len());
        e.undo();
        assert!(e.model.spline_walls(None).unwrap().is_empty());
        assert!(e.walls.is_empty());
    }
}
