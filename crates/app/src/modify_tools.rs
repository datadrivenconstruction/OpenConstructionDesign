//! Edit tools (TOOLS_UI §2) over `ddc_model::modify`: move, copy, rotate,
//! mirror (element axis / drawn), array (linear/radial), offset, trim to corner,
//! extend to boundary, split, align. Two flows: pick-first (selection, then command) and
//! tool-first (command, then pick, then Enter). Each commit is one model operation and one undo step
//! (`Editor::run_op`). Also the plan state of these tools (`Lane`): box selection and Tab,
//! filter, temporary isolation, dimensions; `plan_view` calls the input and drawing.

use crate::editor::{Editor, OpFn, Sel};
use crate::isolate::Vis;
use crate::selection::{self, BoxMode, Cycle};
use crate::status::Severity;
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use crate::ui_kit::Tokens;
use crate::interact::sessions::modify;
use ddc_geom::{v2, V2};
use ddc_model::{AlignEdge, Model, Reference, Spacing, Target};
use egui::{self, Color32, Pos2, Stroke};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[path = "tools/dims.rs"]
pub mod dims;
use dims::{DimMode, DimOut, DimTool};
#[path = "tools/distribute.rs"]
pub mod distribute;
use distribute::{DistOut, DistTool};
#[path = "tools/match_type.rs"]
pub mod match_type;
use match_type::{MatchOut, MatchTool};
#[path = "tools/tags.rs"]
pub mod tags;
use tags::{AnnMode, AnnOut, AnnTool, TagAllDlg, TagEdit};
#[path = "tools/sel_panels.rs"]
mod sel_panels;
mod drafting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Move,
    Copy,
    Rotate,
    Mirror,
    MirrorDraw,
    Array,
    Offset,
    Trim,
    Extend,
    Split,
    Align,
    /// Uniform scale about a base point: reference and target clicks, or a typed factor.
    Scale,
}

impl Op {
    pub fn key(self) -> &'static str {
        match self {
            Op::Move => "move",
            Op::Copy => "copy",
            Op::Rotate => "rotate",
            Op::Mirror => "mirror",
            Op::MirrorDraw => "mirror_draw",
            Op::Array => "array",
            Op::Offset => "offset",
            Op::Trim => "trim",
            Op::Extend => "extend",
            Op::Split => "split",
            Op::Align => "align",
            Op::Scale => "scale",
        }
    }
    /// The tool's ribbon command.
    pub fn command(self) -> &'static str {
        match self {
            Op::Move => "mod.move",
            Op::Copy => "mod.copy",
            Op::Rotate => "mod.rotate",
            Op::Mirror => "mod.mirror",
            Op::MirrorDraw => "mod.mirror_draw",
            Op::Array => "mod.array",
            Op::Offset => "mod.offset",
            Op::Trim => "mod.trim",
            Op::Extend => "mod.extend",
            Op::Split => "mod.split",
            Op::Align => "mod.align",
            Op::Scale => "mod.scale",
        }
    }
    /// Works on the selection (otherwise on elements clicked).
    pub fn needs_selection(self) -> bool {
        matches!(
            self,
            Op::Move
                | Op::Copy
                | Op::Rotate
                | Op::Mirror
                | Op::MirrorDraw
                | Op::Array
                | Op::Scale
        )
    }
}

/// Tool options (the options panel).
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    pub copy: bool,
    /// Orthogonal only.
    pub constrain: bool,
    /// Break connections (the model recomputes joints).
    pub disjoin: bool,
    pub multiple: bool,
    /// Array: the instance count including the original.
    pub count: usize,
    /// Array: the second point is the last instance (otherwise the second).
    pub to_last: bool,
    pub radial: bool,
    /// Offset, m.
    pub dist: f64,
    /// Split: 2 and more divides the picked wall into equal parts; 0 splits at the point.
    pub parts: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            copy: false,
            constrain: false,
            disjoin: false,
            multiple: false,
            count: 4,
            to_last: false,
            radial: false,
            dist: 0.5,
            parts: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModTool {
    pub op: Op,
    pub opts: Opts,
    pub targets: Vec<Sel>,
    /// Tool-first: elements are being picked (Enter/"Done" moves on).
    pub selecting: bool,
    pub pts: Vec<V2>,
    /// Picked walls (trim, boundary, align reference, mirror axis).
    pub walls: Vec<i64>,
    pub input: String,
    /// Decimal mark of the language `input` is read in (set by the panel that shows the tool).
    pub mark: crate::units::DecimalMark,
    pub cursor: V2,
    /// Pick radius, m.
    pub r: f64,
    /// Snap candidates for the last hovered point (shared service), the Tab choice and that point.
    pub snaps: Vec<crate::snap::Snap>,
    pub snap_idx: usize,
    snap_raw: Option<V2>,
    /// Ghost preview and refusal of the pending edit (`crate::trial`).
    pub trial: crate::trial::Trial,
    /// Kinds `Lane::start` already left out of `targets`; keeps the final status a warning.
    pub held_back: String,
    /// Drafting view: points are not snapped to the (hidden) plan.
    pub free: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Nothing,
    /// The operation was applied; the tool continues (Multiple/chain).
    Done,
    Exit,
    /// A rule declined the action before anything changed.
    Refused(String),
    /// The operation was tried and failed.
    Error(String),
}

/// Typed canvas length (Move/Copy distance, Offset) read by the length-field rules in the
/// tool's language: `3 000`, `3,5` (ru) and `1,000` (en) mean what they mean everywhere else.
pub(crate) fn typed_length(
    text: &str,
    u: crate::units::Units,
    mark: crate::units::DecimalMark,
) -> Result<f64, crate::units::InputError> {
    crate::units::parse_edit_length_lang(text, u, &crate::expr_field::Scope::none(), mark)
}

/// Typed array count read like the count part of the 3D array bar (`count;`): a whole number
/// from 2 to the array maximum, anything else is no count.
pub(crate) fn typed_count(text: &str, mark: crate::units::DecimalMark) -> Option<usize> {
    let t = text.trim();
    if t.is_empty() || t.contains(crate::units::PAIR_SEP) {
        return None;
    }
    let s = format!("{t}{}", crate::units::PAIR_SEP);
    match crate::edit3d::mirror_array::read_typed(&s, false, crate::units::Units::Metric, mark) {
        Ok((Some(n), None)) => Some(n),
        _ => None,
    }
}

fn bad_length_text(e: &crate::units::InputError, text: &str, mark: crate::units::DecimalMark) -> String {
    let ru = mark == crate::units::DecimalMark::Comma;
    let why = crate::units::typed_error_text(e, text, ru);
    crate::tr_lang(ru, "pair_input.op.bad_length", &[("text", text), ("why", &why)])
}

fn bad_count_text(text: &str, mark: crate::units::DecimalMark) -> String {
    let ru = mark == crate::units::DecimalMark::Comma;
    let (lo, hi) = (crate::modify_opts::COUNT_MIN.to_string(), crate::modify_opts::COUNT_MAX.to_string());
    crate::tr_lang(ru, "pair_input.op.bad_count", &[("text", text.trim()), ("min", &lo), ("max", &hi)])
}

/// A model operation target; masses are separate (moved by their own operation, see `translate`).
/// From `Sel::Ext` a ramp, a divider and a shaft are editable; what is not editable is named by `skipped`.
pub(crate) fn target(s: Sel) -> Option<Target> {
    Some(match s {
        Sel::Wall(id) => Target::Wall(id),
        Sel::Opening(id) => Target::Opening(id),
        Sel::Curtain(id) => Target::Curtain(id),
        Sel::Slab(id) => Target::Slab(id),
        Sel::Room(id) => Target::Room(id),
        Sel::Mep(id) => Target::MepRun(id),
        Sel::Comp(id) => Target::Component(id),
        Sel::Body(k, id) => k.target(id),
        Sel::Ext(c, id) if c == crate::scene_ids::EXT_SOLID => Target::Solid(id),
        Sel::Ext(c, id) if c == crate::scene_ids::EXT_TOPO => Target::Topo(id),
        Sel::Ext(c, id) => match crate::editor::structx::kind_of(c) {
            Some("ramp") => Target::Ramp(id),
            Some("room_sep") => Target::RoomSep(id),
            Some("roof") => Target::Roof(id),
            Some("shaft") => Target::Shaft(id),
            Some("stair") => Target::Stair(id),
            Some("railing") => Target::Railing(id),
            Some("grid") => Target::Grid(id),
            Some("ref_plane") => Target::RefPlane(id),
            Some("footing") => Target::Footing(id),
            Some("strip_footing") => Target::StripFooting(id),
            Some("raft") => Target::Raft(id),
            Some("brace") => Target::Brace(id),
            Some("truss") => Target::Truss(id),
            Some("beam_system") => Target::BeamSystem(id),
            Some("zone") => Target::Zone(id),
            _ => return None,
        },
        // a panel is a sub-element: moving it must never move its wall
        Sel::Mass(_) | Sel::Anno(_) | Sel::Panel(..) => return None,
    })
}

/// Shortest reference or target length the scale tool accepts, m.
const SCALE_EPS: f64 = 1e-6;

/// Factor of the scale tool: new length (base to `c`) over reference length (base to `r`).
fn scale_factor(o: V2, r: V2, c: V2) -> Option<f64> {
    let (l0, l1) = ((r - o).len(), (c - o).len());
    (l0 >= SCALE_EPS && l1 >= SCALE_EPS).then(|| l1 / l0)
}

fn ang(v: V2) -> f64 {
    v.y.atan2(v.x)
}

fn rot(p: V2, c: V2, a: f64) -> V2 {
    let (s, k) = a.sin_cos();
    let d = p - c;
    c + v2(d.x * k - d.y * s, d.x * s + d.y * k)
}

fn reflect(p: V2, a: V2, b: V2) -> V2 {
    let d = b - a;
    let l2 = d.dot(d).max(1e-18);
    let q = a + d * ((p - a).dot(d) / l2);
    q * 2.0 - p
}

/// The nearest wall under a point (by face/axis, pick radius `r`).
pub fn wall_at(ed: &Editor, p: V2, r: f64) -> Option<i64> {
    selection::candidates(ed, p, r, |_| true)
        .into_iter()
        .find_map(|c| match c.as_slice() {
            [Sel::Wall(id)] => Some(*id),
            _ => None,
        })
}

fn axis(ed: &Editor, id: i64) -> Option<(V2, V2)> {
    ed.walls.iter().find(|w| w.id == id).map(|w| (w.a, w.b))
}

fn op_fn(f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> OpFn {
    OpFn(Arc::new(f))
}

impl ModTool {
    pub fn new(op: Op, sel: &[Sel]) -> Self {
        let opts = Opts {
            multiple: op == Op::Copy,
            copy: matches!(op, Op::Mirror | Op::MirrorDraw),
            ..Default::default()
        };
        Self {
            op,
            opts,
            targets: sel.to_vec(),
            selecting: op.needs_selection() && sel.is_empty(),
            pts: vec![],
            walls: vec![],
            input: String::new(),
            mark: crate::units::ui_decimal_mark(),
            cursor: v2(0.0, 0.0),
            r: 0.1,
            snaps: vec![],
            snap_idx: 0,
            snap_raw: None,
            trial: Default::default(),
            held_back: String::new(),
            free: false,
        }
    }

    fn takes_points(&self) -> bool {
        !self.free
            && !self.selecting
            && matches!(
                self.op,
                Op::Move | Op::Copy | Op::Rotate | Op::MirrorDraw | Op::Array | Op::Scale
            )
    }

    /// Cursor snapped like the draw tools; keeps the Tab choice while the pointer stays put.
    fn resolve(&mut self, ed: &Editor, p: V2) -> V2 {
        if !self.takes_points() {
            return p;
        }
        let same = self
            .snap_raw
            .is_some_and(|q| (q - p).len() <= ed.tool.radius * 0.25);
        if !same {
            let list = ed.snap_list(p, self.pts.last().copied(), ed.tool.radius, None);
            self.snaps = crate::snap_refine::gate(list, p, ed.tool.no_snap);
            self.snap_idx = 0;
            self.snap_raw = Some(p);
        }
        self.snaps.get(self.snap_idx).map_or(p, |s| s.p)
    }

    /// The snap the cursor sits on, if it is more than the free point.
    pub fn snap(&self) -> Option<crate::snap::Snap> {
        self.snaps
            .get(self.snap_idx)
            .copied()
            .filter(|s| s.kind != crate::snap::SnapKind::Free)
    }

    /// Tab: next candidate under the cursor.
    pub fn tab(&mut self) {
        if self.snaps.len() > 1 {
            self.snap_idx = (self.snap_idx + 1) % self.snaps.len();
            if let Some(s) = self.snaps.get(self.snap_idx) {
                self.cursor = self.constrained(s.p);
            }
        }
    }

    /// Click from the plan: the point is snapped first, then handled as [`ModTool::click`].
    pub fn click_snapped(&mut self, ed: &mut Editor, p: V2) -> Out {
        let p = if self.selecting { p } else { self.resolve(ed, p) };
        self.snap_raw = None;
        self.click(ed, p)
    }

    /// The selection box center (the rotation center by default).
    fn center(&self, ed: &Editor) -> Option<V2> {
        let pts: Vec<V2> = self
            .targets
            .iter()
            .flat_map(|&s| selection::outlines(ed, s))
            .flatten()
            .collect();
        let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
        for p in &pts {
            lo = v2(lo.x.min(p.x), lo.y.min(p.y));
            hi = v2(hi.x.max(p.x), hi.y.max(p.y));
        }
        (!pts.is_empty()).then(|| (lo + hi) * 0.5)
    }

    /// The step for the hint `hint.<op>.<step>`.
    pub fn step(&self) -> &'static str {
        if self.selecting {
            return "select";
        }
        let n = self.pts.len();
        match self.op {
            Op::Move | Op::Copy | Op::MirrorDraw => ["first", "second"][n.min(1)],
            Op::Array if self.opts.radial => ["center", "first", "second"][n.min(2)],
            Op::Array => ["first", "second"][n.min(1)],
            Op::Rotate => ["first", "second"][n.saturating_sub(1).min(1)],
            Op::Mirror => "pick",
            Op::Split if self.opts.parts >= 2 => "parts",
            Op::Offset | Op::Split => "pick",
            Op::Trim | Op::Align => ["first", "second"][self.walls.len().min(1)],
            Op::Extend => ["boundary", "target"][self.walls.len().min(1)],
            Op::Scale => ["first", "second", "third"][n.min(2)],
        }
    }

    /// The cursor with "Constrain" applied (0/90 degrees from the last point).
    pub fn constrained(&self, p: V2) -> V2 {
        match (self.opts.constrain, self.pts.last()) {
            (true, Some(&o)) => {
                let d = p - o;
                if d.x.abs() >= d.y.abs() {
                    v2(p.x, o.y)
                } else {
                    v2(o.x, p.y)
                }
            }
            _ => p,
        }
    }

    pub fn hover(&mut self, ed: &Editor, p: V2) {
        if self.op == Op::Rotate && self.pts.is_empty() && !self.selecting {
            if let Some(c) = self.center(ed) {
                self.pts.push(c);
            }
        }
        let q = self.resolve(ed, p);
        self.cursor = self.constrained(q);
    }

    /// The edit the tool would commit at the cursor now, for a trial; the key holds the model
    /// head, so any commit or undo asks again. Masses and annotations get no trial.
    pub fn ask(&self, ed: &Editor) -> Option<crate::trial::Ask> {
        use crate::trial::{Ask, Field, Key};
        // no `refused` here: it raises a toast offer, and an empty target list asks nothing
        if self.selecting {
            return None;
        }
        match (self.op, self.pts.as_slice()) {
            (Op::Move | Op::Copy, [p0]) => {
                if self
                    .targets
                    .iter()
                    .any(|s| matches!(s, Sel::Mass(_) | Sel::Anno(_)))
                {
                    return None;
                }
                let ts: Vec<Target> = self.targets.iter().filter_map(|&s| target(s)).collect();
                let v = self.cursor - *p0;
                if ts.is_empty() || v.len() < 1e-9 {
                    return None;
                }
                let copy = self.op == Op::Copy || self.opts.copy;
                let size = ts.len();
                let key = Key::new("move")
                    .h(ed.model.change_head())
                    .h(copy)
                    .h(&self.targets)
                    .p(v)
                    .done();
                let op = if copy {
                    op_fn(move |m| m.copy_elements(&ts, &[(v.x, v.y)]).map(|_| ()))
                } else {
                    op_fn(move |m| m.move_elements(&ts, (v.x, v.y)))
                };
                Some(Ask {
                    key,
                    field: Field::Point,
                    op,
                    size,
                })
            }
            (Op::Offset, _) => {
                let p = self.cursor;
                let w = wall_at(ed, p, self.r)?;
                let (a, b) = axis(ed, w)?;
                let d = b - a;
                let left = d.x * (p.y - a.y) - d.y * (p.x - a.x) > 0.0;
                let dist = self.parse(ed).unwrap_or(self.opts.dist);
                let key = Key::new("offset")
                    .h(ed.model.change_head())
                    .h(w).h(left).f(dist).done();
                Some(Ask {
                    key,
                    field: Field::Distance,
                    op: op_fn(move |m| m.offset_wall(w, dist, left).map(|_| ())),
                    size: 1,
                })
            }
            _ => None,
        }
    }

    /// Once per frame after `hover`: feeds the trial; seconds to the next due trial, if any.
    pub fn pulse(&mut self, ed: &mut Editor, now: f64, frame: u64) -> Option<f64> {
        let ask = self.ask(ed);
        self.trial
            .pulse(&mut ed.model, ed.level, ask.as_ref(), now, frame);
        self.trial.wait(now)
    }

    /// A refusal found for exactly the edit about to be committed: the commit is blocked.
    fn blocked(&self, ed: &Editor) -> Option<Out> {
        let ask = self.ask(ed)?;
        let (field, why) = self.trial.blocks(ask.key, ed.model.serial())?;
        Some(Out::Error(crate::trial::refusal_text(field, &why)))
    }

    /// The current preview transform (point to point) from the points and cursor.
    pub fn xf(&self, ed: &Editor) -> Option<Box<dyn Fn(V2) -> V2>> {
        if self.selecting {
            return None;
        }
        let c = self.cursor;
        match (self.op, self.pts.as_slice()) {
            (Op::Move | Op::Copy, [p0]) => {
                let v = c - *p0;
                Some(Box::new(move |p| p + v))
            }
            (Op::Rotate, [o, r1]) => {
                let (o, a) = (*o, ang(c - *o) - ang(*r1 - *o));
                Some(Box::new(move |p| rot(p, o, a)))
            }
            (Op::MirrorDraw, [a]) => {
                let a = *a;
                Some(Box::new(move |p| reflect(p, a, c)))
            }
            (Op::Mirror, []) => {
                let (a, b) = axis(ed, wall_at(ed, c, self.r)?)?;
                Some(Box::new(move |p| reflect(p, a, b)))
            }
            (Op::Scale, [o, r]) => {
                let (o, k) = (*o, scale_factor(*o, *r, c)?);
                Some(Box::new(move |p| o + (p - o) * k))
            }
            _ => None,
        }
    }

    /// Offsets/angles of array instances in the preview (without the original).
    pub fn array_xfs(&self) -> Vec<Box<dyn Fn(V2) -> V2>> {
        let n = self.opts.count.max(2);
        let c = self.cursor;
        let mut v: Vec<Box<dyn Fn(V2) -> V2>> = vec![];
        match (self.opts.radial, self.pts.as_slice()) {
            (false, [p0]) => {
                let d = c - *p0;
                let step = if self.opts.to_last {
                    d * (1.0 / (n - 1) as f64)
                } else {
                    d
                };
                for k in 1..n {
                    let s = step * k as f64;
                    v.push(Box::new(move |p| p + s));
                }
            }
            (true, [o, r1]) => {
                let (o, total) = (*o, self.sweep(c));
                let step = total / (n - 1) as f64;
                for k in 1..n {
                    let a = step * k as f64;
                    v.push(Box::new(move |p| rot(p, o, a)));
                }
                let _ = r1;
            }
            _ => {}
        }
        v
    }

    /// Radial array: the angle from ray 1 to the cursor, (0, 2pi].
    fn sweep(&self, c: V2) -> f64 {
        let (o, r1) = (self.pts[0], self.pts[1]);
        let mut a = ang(c - o) - ang(r1 - o);
        while a <= 1e-6 {
            a += std::f64::consts::TAU;
        }
        if (a - std::f64::consts::TAU).abs() < 1e-6 {
            a = std::f64::consts::TAU;
        }
        a
    }

    fn parse(&self, ed: &Editor) -> Option<f64> {
        typed_length(&self.input, ed.tool.units, self.mark).ok()
    }

    /// This tool can edit the element (a mass only by a plain move).
    fn handled(&self, s: Sel) -> bool {
        let moves_mass = self.op == Op::Move && !self.opts.copy;
        // annotations follow the transforms only (see `annot_text::tool_step`)
        let annos = self.op.needs_selection() && matches!(s, Sel::Anno(_));
        target(s).is_some() || (moves_mass && matches!(s, Sel::Mass(_))) || annos
    }

    /// Names of picked categories the tool does not edit; without repeats.
    fn skipped(&self, ed: &Editor) -> String {
        let mut names: Vec<String> = vec![];
        let left: Vec<Sel> = self.targets.iter().copied().filter(|s| !self.handled(*s)).collect();
        crate::elem::panel_rule::offer(&left);
        for &s in &left {
            let n = if matches!(s, Sel::Panel(..)) {
                crate::tr("elem.panel", &[])
            } else {
                crate::tr(&format!("selcat.{}", selection::category(ed, s)), &[])
            };
            if !names.contains(&n) {
                names.push(n);
            }
        }
        names.join(", ")
    }

    /// Nothing in the selection is editable by the tool: a refusal naming the categories.
    fn refused(&self, ed: &Editor) -> Option<Out> {
        let none = !self.targets.is_empty() && !self.targets.iter().any(|s| self.handled(*s));
        (self.op.needs_selection() && none).then(|| {
            if let Some(why) = crate::elem::hosted_refusal(&self.targets) {
                return Out::Refused(why);
            }
            // a mass cannot turn: say why, not the generic refusal
            if self.turns_mass() {
                return Out::Refused(crate::tr("elem.no.mass_turn", &[]));
            }
            let kinds = self.skipped(ed);
            Out::Refused(crate::tr("mt.unsupported", &[("kinds", kinds.as_str())]))
        })
    }

    /// The tool turns or mirrors a selection that holds a mass (its form history has no turn).
    fn turns_mass(&self) -> bool {
        matches!(self.op, Op::Rotate | Op::Mirror | Op::MirrorDraw)
            && self.targets.iter().any(|s| matches!(s, Sel::Mass(_)))
    }

    /// Operation outcome: part of the selection stayed in place, and it says so.
    fn done_status(&self, ed: &Editor) -> String {
        let mut kinds = self.skipped(ed);
        if kinds.is_empty() {
            kinds = self.held_back.clone();
        }
        if self.op.needs_selection() && !kinds.is_empty() {
            let said = crate::tr(
                "mt.partial",
                &[("op", &crate::tr(&format!("cmd.{}", self.op.command()), &[])), ("kinds", kinds.as_str())],
            );
            if self.turns_mass() {
                format!("{said} {}", crate::tr("elem.no.mass_turn", &[]))
            } else {
                said
            }
        } else {
            crate::tr("modify_ops.op.done", &[("op", &crate::tr(&format!("cmd.{}", self.op.command()), &[]))])
        }
    }

    /// A partial result is a warning, a full one is news.
    fn say_done(&self, ed: &mut Editor) {
        let sev = if self.op.needs_selection()
            && !(self.skipped(ed).is_empty() && self.held_back.is_empty())
        {
            Severity::Warning
        } else {
            Severity::Info
        };
        let m = self.done_status(ed);
        ed.say(sev, m);
    }

    pub fn click(&mut self, ed: &mut Editor, p: V2) -> Out {
        if self.selecting {
            if let Some(c) = selection::candidates(ed, p, self.r, |_| true).first() {
                for s in c {
                    if let Some(i) = self.targets.iter().position(|x| x == s) {
                        self.targets.remove(i);
                    } else {
                        self.targets.push(*s);
                    }
                }
                ed.selection = self.targets.clone();
            }
            return Out::Nothing;
        }
        if let Some(o) = self.refused(ed) {
            return o;
        }
        let p = self.constrained(p);
        self.cursor = p;
        let commits = match self.op {
            Op::Move | Op::Copy => !self.pts.is_empty(),
            Op::Offset => true,
            _ => false,
        };
        if commits {
            if let Some(o) = self.blocked(ed) {
                return o;
            }
        }
        let ts: Vec<Target> = self.targets.iter().filter_map(|&s| target(s)).collect();
        match self.op {
            Op::Move | Op::Copy => {
                let Some(&p0) = self.pts.first() else {
                    self.pts.push(p);
                    return Out::Nothing;
                };
                self.translate(ed, ts, p - p0)
            }
            Op::Rotate => {
                if self.pts.len() < 2 {
                    self.pts.push(p);
                    return Out::Nothing;
                }
                let (o, r1) = (self.pts[0], self.pts[1]);
                self.rotate(ed, ts, o, ang(p - o) - ang(r1 - o))
            }
            Op::Mirror => {
                let Some((a, b)) = wall_at(ed, p, self.r).and_then(|w| axis(ed, w)) else {
                    return Out::Refused(crate::tr("modify_ops.op.pick_axis", &[]));
                };
                self.mirror(ed, ts, a, b)
            }
            Op::MirrorDraw => {
                let Some(&a) = self.pts.first() else {
                    self.pts.push(p);
                    return Out::Nothing;
                };
                self.mirror(ed, ts, a, p)
            }
            Op::Scale => {
                if self.pts.len() == 1 && (p - self.pts[0]).len() < SCALE_EPS {
                    return Out::Error(crate::tr("scl.zero_ref", &[]));
                }
                if self.pts.len() < 2 {
                    self.pts.push(p);
                    return Out::Nothing;
                }
                let (o, r) = (self.pts[0], self.pts[1]);
                match scale_factor(o, r, p) {
                    Some(k) => self.scale(ed, ts, o, k),
                    None => Out::Error(crate::tr("scl.bad_factor", &[])),
                }
            }
            Op::Array => {
                let need = if self.opts.radial { 2 } else { 1 };
                if self.pts.len() < need {
                    self.pts.push(p);
                    return Out::Nothing;
                }
                let n = self.opts.count;
                let elems = !ts.is_empty();
                let (f, xfs) = if self.opts.radial {
                    let (o, total) = (self.pts[0], self.sweep(p));
                    (
                        op_fn(move |m| m.array_radial(&ts, n, (o.x, o.y), total).map(|_| ())),
                        crate::annot_text::radial_xfs(o, total, n),
                    )
                } else {
                    let d = p - self.pts[0];
                    let sp = if self.opts.to_last {
                        Spacing::Fit((d.x, d.y))
                    } else {
                        Spacing::Step((d.x, d.y))
                    };
                    (
                        op_fn(move |m| m.array_linear(&ts, n, sp).map(|_| ())),
                        crate::annot_text::linear_xfs(d, n, self.opts.to_last),
                    )
                };
                let annos = crate::annot_text::tool_step(&self.targets, xfs, true);
                self.run_with(ed, f, elems, annos, false)
            }
            Op::Offset => {
                if let Some(v) = self.parse(ed) {
                    if !crate::modify_opts::dist_ok(v) {
                        self.input.clear();
                        return Out::Refused(crate::tr(
                            crate::modify_opts::Reject::NotPositive.key(),
                            &[],
                        ));
                    }
                    self.opts.dist = v;
                    self.input.clear();
                }
                let Some(w) = wall_at(ed, p, self.r) else {
                    return Out::Refused(crate::tr("modify_ops.op.pick_wall", &[]));
                };
                let (a, b) = axis(ed, w).expect("wall");
                let d = b - a;
                let left = d.x * (p.y - a.y) - d.y * (p.x - a.x) > 0.0;
                let dist = self.opts.dist;
                self.run(
                    ed,
                    op_fn(move |m| m.offset_wall(w, dist, left).map(|_| ())),
                    true,
                )
            }
            Op::Trim => {
                let Some(w) = wall_at(ed, p, self.r) else {
                    return Out::Nothing;
                };
                if self.walls.is_empty() {
                    self.walls.push(w);
                    self.pts.clear();
                    self.pts.push(p);
                    return Out::Nothing;
                }
                let w1 = self.walls[0];
                let c1 = self.pts.first().map(|q| (q.x, q.y));
                self.walls.clear();
                self.pts.clear();
                let c2 = (p.x, p.y);
                self.run(ed, op_fn(move |m| m.trim_corner_at(w1, w, c1, Some(c2))), true)
            }
            Op::Extend => {
                let Some(w) = wall_at(ed, p, self.r) else {
                    return Out::Nothing;
                };
                let Some(&b) = self.walls.first() else {
                    self.walls.push(w);
                    return Out::Nothing;
                };
                let (Some((a0, a1)), Some((b0, b1))) = (axis(ed, w), axis(ed, b)) else {
                    return Out::Nothing;
                };
                let (d1, d2) = (a1 - a0, b1 - b0);
                if w == b || (d1.x * d2.y - d1.y * d2.x).abs() <= 1e-9 * d1.len() * d2.len() {
                    // the boundary stays selected for the next pick
                    return Out::Error(crate::tr("tr.parallel", &[]));
                }
                let click = (p.x, p.y);
                let out = self.run(
                    ed,
                    op_fn(move |m| m.trim_to_at(w, Reference::Wall(b), Some(click))),
                    true,
                );
                if matches!(out, Out::Error(_)) {
                    self.walls = vec![b];
                }
                out
            }
            Op::Split => {
                let Some(w) = wall_at(ed, p, self.r) else {
                    return Out::Refused(crate::tr("modify_ops.op.pick_wall", &[]));
                };
                if self.opts.parts >= 2 {
                    let n = self.opts.parts;
                    return self.run(ed, op_fn(move |m| m.divide_wall(w, n).map(|_| ())), true);
                }
                let (a, b) = axis(ed, w).expect("wall");
                let at = match self.parse(ed) {
                    Some(v) => {
                        self.input.clear();
                        let (e, o) = if (p - a).len() <= (p - b).len() {
                            (a, b)
                        } else {
                            (b, a)
                        };
                        let d = o - e;
                        e + d * (v / d.len().max(1e-12))
                    }
                    None => crate::snap::closest_on_segment(p, a, b).0,
                };
                self.run(
                    ed,
                    op_fn(move |m| m.split_wall_at(w, (at.x, at.y)).map(|_| ())),
                    true,
                )
            }
            Op::Align => {
                let Some(w) = wall_at(ed, p, self.r) else {
                    return Out::Nothing;
                };
                let Some(&rf) = self.walls.first() else {
                    self.walls.push(w);
                    return Out::Nothing;
                };
                let (a, b) = axis(ed, rf).expect("wall");
                self.walls.clear();
                self.run(
                    ed,
                    op_fn(move |m| m.align(AlignEdge::WallAxis(w), (a.x, a.y), (b.x, b.y))),
                    true,
                )
            }
        }
    }

    fn translate(&mut self, ed: &mut Editor, ts: Vec<Target>, v: V2) -> Out {
        let copy = self.op == Op::Copy || self.opts.copy;
        let masses: Vec<i64> = self
            .targets
            .iter()
            .filter_map(|s| match s {
                Sel::Mass(id) => Some(*id),
                _ => None,
            })
            .collect();
        if !copy && !masses.is_empty() {
            return self.move_masses(ed, ts, masses, v);
        }
        let elems = !ts.is_empty();
        let f = if copy {
            op_fn(move |m| m.copy_elements(&ts, &[(v.x, v.y)]).map(|_| ()))
        } else {
            op_fn(move |m| m.move_elements(&ts, (v.x, v.y)))
        };
        // Multiple: the base stays, each point is a new copy from the original
        let multi = copy && self.opts.multiple;
        let xf = ddc_model::AnnoXf::Move((v.x, v.y));
        let annos = crate::annot_text::tool_step(&self.targets, vec![xf], copy);
        self.run_with(ed, f, elems, annos, multi)
    }

    fn rotate(&mut self, ed: &mut Editor, ts: Vec<Target>, o: V2, a: f64) -> Out {
        let copy = self.opts.copy;
        let elems = !ts.is_empty();
        let xf = ddc_model::AnnoXf::Rotate { o: (o.x, o.y), a };
        let annos = crate::annot_text::tool_step(&self.targets, vec![xf], copy);
        self.run_with(
            ed,
            op_fn(move |m| m.rotate_elements(&ts, (o.x, o.y), a, copy).map(|_| ())),
            elems,
            annos,
            false,
        )
    }

    fn scale(&mut self, ed: &mut Editor, ts: Vec<Target>, o: V2, k: f64) -> Out {
        if !(k.is_finite() && k > 0.0) {
            return Out::Error(crate::tr("scl.bad_factor", &[]));
        }
        let copy = self.opts.copy;
        let elems = !ts.is_empty();
        let xf = ddc_model::AnnoXf::Scale { o: (o.x, o.y), k };
        let annos = crate::annot_text::tool_step(&self.targets, vec![xf], copy);
        self.run_with(
            ed,
            op_fn(move |m| m.scale_elements(&ts, (o.x, o.y), k, copy).map(|_| ())),
            elems,
            annos,
            false,
        )
    }

    fn mirror(&mut self, ed: &mut Editor, ts: Vec<Target>, a: V2, b: V2) -> Out {
        if (b - a).len() < 1e-9 {
            return Out::Refused(crate::tr("modify_ops.op.zero_axis", &[]));
        }
        let copy = self.opts.copy;
        let elems = !ts.is_empty();
        let xf = ddc_model::AnnoXf::Mirror {
            a: (a.x, a.y),
            b: (b.x, b.y),
        };
        let annos = crate::annot_text::tool_step(&self.targets, vec![xf], copy);
        self.run_with(
            ed,
            op_fn(move |m| {
                m.mirror_elements(&ts, (a.x, a.y), (b.x, b.y), copy)
                    .map(|_| ())
            }),
            elems,
            annos,
            false,
        )
    }

    /// Move with masses: elements are one operation, each mass its own (hosts follow),
    /// all is one editor undo step.
    fn move_masses(&mut self, ed: &mut Editor, ts: Vec<Target>, masses: Vec<i64>, v: V2) -> Out {
        use crate::mass_tools::{steps_rec, Step};
        let mut steps: Vec<Step> = vec![];
        if !ts.is_empty() {
            steps.push(Box::new(move |m: &mut Model| {
                m.move_elements(&ts, (v.x, v.y))
            }));
        }
        for id in masses {
            steps.push(Box::new(move |m: &mut Model| {
                m.move_mass(id, (v.x, v.y, 0.0))
            }));
        }
        let xf = ddc_model::AnnoXf::Move((v.x, v.y));
        steps.extend(crate::annot_text::tool_step(&self.targets, vec![xf], false));
        match ed.apply_ext(steps_rec("undo.mass.move", steps)) {
            Ok(()) => {
                self.say_done(ed);
                Out::Exit
            }
            Err(e) => {
                self.pts.clear();
                self.walls.clear();
                Out::Error(crate::tr("modify_ops.op.failed", &[("why", &crate::elem::mep::explain(&e))]))
            }
        }
    }

    /// `run` with selection annotations: elements (if any) and annotations are one undo step.
    fn run_with(
        &mut self,
        ed: &mut Editor,
        f: OpFn,
        elems: bool,
        annos: Option<crate::mass_tools::Step>,
        stay: bool,
    ) -> Out {
        use crate::mass_tools::{steps_rec, Step};
        let Some(annos) = annos else {
            return self.run(ed, f, stay);
        };
        let mut steps: Vec<Step> = vec![];
        if elems {
            steps.push(Box::new(move |m: &mut Model| (f.0)(m)));
        }
        steps.push(annos);
        match ed.apply_ext(steps_rec("undo.anno.modify", steps)) {
            Ok(()) => {
                ed.say(Severity::Info, crate::tr("modify_ops.op.done", &[("op", &crate::tr(&format!("cmd.{}", self.op.command()), &[]))]));
                if stay {
                    Out::Done
                } else {
                    Out::Exit
                }
            }
            Err(e) => {
                self.pts.clear();
                self.walls.clear();
                Out::Error(crate::tr("modify_ops.op.failed", &[("why", &crate::elem::mep::explain(&e))]))
            }
        }
    }

    /// Applies the operation; `stay` keeps the tool (Multiple), otherwise it exits.
    fn run(&mut self, ed: &mut Editor, f: OpFn, stay: bool) -> Out {
        if self.op.needs_selection() && self.targets.is_empty() {
            return Out::Refused(crate::tr("modify_ops.op.nothing_selected", &[]));
        }
        if let Some(o) = self.refused(ed) {
            return o;
        }
        match ed.run_op(f) {
            Ok(()) => {
                self.say_done(ed);
                if stay {
                    Out::Done
                } else {
                    Out::Exit
                }
            }
            // model error: the state does not change, points are reset, the tool stays
            Err(e) => {
                self.pts.clear();
                self.walls.clear();
                Out::Error(crate::tr("modify_ops.op.failed", &[("why", &crate::elem::mep::explain(&e))]))
            }
        }
    }

    /// Enter: end of picking (tool-first) or number entry (distance, angle, count).
    pub fn enter(&mut self, ed: &mut Editor) -> Out {
        if self.selecting {
            if self.targets.is_empty() {
                return Out::Refused(crate::tr("modify_ops.op.nothing_selected", &[]));
            }
            self.selecting = false;
            return Out::Nothing;
        }
        let text = std::mem::take(&mut self.input);
        if text.is_empty() {
            return Out::Nothing;
        }
        if let Some(o) = self.refused(ed) {
            return o;
        }
        let ts: Vec<Target> = self.targets.iter().filter_map(|&s| target(s)).collect();
        match (self.op, self.pts.as_slice()) {
            (Op::Array, _) => match typed_count(&text, self.mark) {
                Some(n) => {
                    self.opts.count = n;
                    Out::Nothing
                }
                None => Out::Refused(bad_count_text(&text, self.mark)),
            },
            (Op::Split, _) if self.opts.parts >= 2 => match typed_count(&text, self.mark) {
                Some(n) if (2..=ddc_model::MAX_DIVIDE).contains(&n) => {
                    self.opts.parts = n;
                    Out::Nothing
                }
                _ => Out::Refused(crate::tr(
                    "create_input.divide.bad_parts",
                    &[("text", &text), ("max", &ddc_model::MAX_DIVIDE.to_string())],
                )),
            },
            (Op::Rotate, [o, r1]) => {
                let Ok(deg) = crate::units::parse_locale_number(text.trim(), self.mark) else {
                    return Out::Refused(crate::tr("modify_ops.op.not_angle", &[("text", &text)]));
                };
                let (o, r1) = (*o, *r1);
                let _ = r1;
                self.rotate(ed, ts, o, deg.to_radians())
            }
            (Op::Scale, [o, ..]) => {
                let o = *o;
                match crate::units::parse_locale_number(text.trim(), self.mark) {
                    Ok(k) if k.is_finite() && k > 0.0 => self.scale(ed, ts, o, k),
                    _ => Out::Error(crate::tr("scl.bad_factor", &[])),
                }
            }
            (Op::Move | Op::Copy, [p0]) => {
                let l = match typed_length(&text, ed.tool.units, self.mark) {
                    Ok(l) => l,
                    Err(e) => return Out::Refused(bad_length_text(&e, &text, self.mark)),
                };
                let d = self.cursor - *p0;
                let u = if d.len() > 1e-9 {
                    d * (1.0 / d.len())
                } else {
                    v2(1.0, 0.0)
                };
                self.translate(ed, ts, u * l)
            }
            (Op::Offset, _) => match typed_length(&text, ed.tool.units, self.mark) {
                Ok(v) if crate::modify_opts::dist_ok(v) => {
                    self.opts.dist = v;
                    Out::Nothing
                }
                Ok(_) | Err(crate::units::InputError::NonPositive) => Out::Refused(crate::tr(
                    crate::modify_opts::Reject::NotPositive.key(),
                    &[],
                )),
                Err(e) => Out::Refused(bad_length_text(&e, &text, self.mark)),
            },
            _ => {
                self.input = text;
                Out::Nothing
            }
        }
    }

    /// Esc: the 1st resets points (the tool stays), the 2nd exits.
    pub fn escape(&mut self) -> Out {
        let auto_center = self.op == Op::Rotate && self.pts.len() == 1;
        if (self.pts.is_empty() || auto_center) && self.walls.is_empty() && self.input.is_empty() {
            return Out::Exit;
        }
        self.pts.clear();
        self.walls.clear();
        self.input.clear();
        Out::Nothing
    }

    /// Space: rotate, the center anew (the next click).
    pub fn space(&mut self) {
        if self.op == Op::Rotate {
            self.pts.clear();
            self.pts.push(self.cursor);
        }
    }

    pub fn char(&mut self, ch: char) {
        // a space inside a number groups thousands (`3 000`); Rotate keeps Space for the centre
        let group = ch == ' ' && !self.input.is_empty() && self.op != Op::Rotate;
        if (ch.is_ascii_digit() || ".,-=+m".contains(ch) || group) && self.input.len() < 32 {
            self.input.push(ch);
        }
    }
}

/// UI labels of the modify tools (taken from the catalog when a command starts).
#[derive(Clone, Debug, Default)]
pub struct Labels(pub HashMap<String, String>);

pub const LABEL_KEYS: &[&str] = &[
    "mt.copy",
    "mt.constrain",
    "mt.disjoin",
    "mt.multiple",
    "mt.count",
    "mt.to_last",
    "mt.radial",
    "mt.dist",
    "mt.done",
    "mt.of",
    "mt.attach_axis",
    "mt.attach_face",
    "iso.isolate",
    "iso.hide",
    "iso.apply",
    "flt.title",
    "flt.only",
    "flt.all",
    "flt.ok",
    "flt.cat.wall",
    "flt.cat.door",
    "flt.cat.window",
    "flt.cat.curtain",
    "flt.cat.floor",
    "flt.cat.room",
    "flt.cat.mass",
    "flt.cat.mep",
    "flt.cat.comp",
    "flt.cat.anno",
    "dt.equal",
    "dt.step",
    "dt.count",
    "dt.margin",
    "dt.stepv",
    "dt.link",
    "dt.release",
    "dt.edit",
    "tg.leader",
    "tg.title",
    "tg.new",
    "tg.cat.door",
    "tg.cat.window",
    "tg.cat.room",
    "tg.cat.wall",
    "tg.cat.comp",
    "tg.num.start",
    "tg.num.cat",
    "tg.drag",
    "flt.cat.structure",
];

impl Labels {
    pub fn from(cat: &ddc_ui::i18n::Catalog) -> Self {
        Self(
            LABEL_KEYS
                .iter()
                .map(|k| (k.to_string(), cat.t(k)))
                .collect(),
        )
    }
    pub fn t(&self, k: &str) -> String {
        self.0.get(k).cloned().unwrap_or_else(|| k.to_string())
    }
}

/// What the transform `op` can do with `sel`, by the tool's own rules and the family rows.
pub fn gate_for(op: Op, sel: &[Sel]) -> crate::elem::caps::Gate {
    use crate::elem::caps::{gate, xf_op, Gate};
    let Some(xo) = xf_op(op) else {
        return Gate::All;
    };
    let probe = ModTool::new(op, &[]);
    gate(sel, xo, |s| probe.handled(s))
}

/// Keys of a canvas that drives the modify session itself (drafting tab, test harness): the
/// same inputs the plan sends through `Editor::handle`.
fn modify_keys(ui: &mut egui::Ui, ed: &mut Editor) {
    use crate::interact::{Input, Key, Mods};
    let tab = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
    let evs = ui.input(|i| i.events.clone());
    if tab {
        ed.input(Input::Key(Key::Tab, Mods::NONE));
    }
    for e in evs {
        match e {
            egui::Event::Text(s) => s.chars().for_each(|c| ed.input(Input::Char(c))),
            egui::Event::Key { key, pressed: true, modifiers, .. } if !modifiers.command => {
                let k = match key {
                    egui::Key::Escape => Key::Escape,
                    egui::Key::Enter => Key::Enter,
                    egui::Key::Backspace => Key::Backspace,
                    _ => continue,
                };
                ed.input(Input::Key(k, Mods::NONE));
            }
            _ => {}
        }
    }
}

/// Plan state of the modify tools.
#[derive(Default)]
pub struct Lane {
    pub dim: Option<DimTool>,
    pub dist: Option<DistTool>,
    pub mt: Option<MatchTool>,
    pub ann: Option<AnnTool>,
    /// Saved measurements on the plan (wall faces), the same records as the 3D tool.
    pub ms: Option<crate::tools::measure::MeasureTool>,
    pub tag_all: Option<TagAllDlg>,
    /// The tag under the cursor (highlight, Delete in the tag tool).
    pub hot_tag: Option<i64>,
    /// Inline edit of a tag head (double click).
    pub tag_edit: Option<TagEdit>,
    pub vis: Vis,
    pub cycle: Cycle,
    /// Selection box: the start (screen, world) and the current point (screen, world).
    pub drag: Option<(Pos2, V2, Pos2, V2)>,
    /// The plan rectangle in the last frame.
    pub rect: Option<egui::Rect>,
    pub filter_open: bool,
    pub filter_keep: HashSet<&'static str>,
    /// `filter_keep` was filled for the current opening of the filter dialog.
    pub filter_init: bool,
    pub labels: Labels,
    /// Points of the drafting-view dimension/distribute tool.
    free: drafting::FState,
}

impl Lane {
    /// A tool the lane itself still drives (dimensions, distribute, match type, tags).
    pub fn active(&self) -> bool {
        self.dim.is_some() || self.dist.is_some() || self.mt.is_some() || self.ann.is_some() || self.ms.is_some()
    }

    /// `active`, or the modify session (`interact::sessions::modify`) runs.
    pub fn busy(&self, ed: &Editor) -> bool {
        self.active() || modify::running(ed)
    }

    fn clear_tools(&mut self, ed: &mut Editor) {
        // the mode change also ends the modify session
        ed.set_mode(crate::wall_tool::Mode::Select);
        self.dim = None;
        self.dist = None;
        self.mt = None;
        self.ann = None;
        self.ms = None;
        self.free = Default::default();
    }

    /// Modify tools run as the editor's session: one route for the plan, 3D and scripts.
    pub fn start(&mut self, op: Op, ed: &mut Editor) {
        self.clear_tools(ed);
        modify::start(ed, op);
    }

    pub fn start_dim(&mut self, mode: DimMode, ed: &mut Editor) {
        self.clear_tools(ed);
        self.dim = Some(DimTool::new(mode));
    }

    pub fn start_dist(&mut self, ed: &mut Editor) {
        self.clear_tools(ed);
        self.dist = Some(DistTool::new(ed));
    }

    pub fn start_match(&mut self, ed: &mut Editor) {
        self.clear_tools(ed);
        self.mt = Some(MatchTool::new(ed));
    }

    pub fn start_ann(&mut self, mode: AnnMode, ed: &mut Editor) {
        self.clear_tools(ed);
        self.ann = Some(AnnTool::new(mode));
    }

    pub fn start_measure(&mut self, mode: crate::edit3d::measure_saved::Mode, ed: &mut Editor) {
        self.clear_tools(ed);
        self.ms = Some(crate::tools::measure::MeasureTool::new(mode));
    }

    fn ms_out(&mut self, ed: &mut Editor, o: crate::tools::measure::MeasureOut) {
        use crate::tools::measure::MeasureOut as O;
        match o {
            O::Exit => self.ms = None,
            O::Refused(m) => ed.say(Severity::Refusal, m),
            O::Error(m) => ed.say(Severity::Error, m),
            O::Done | O::Nothing => {}
        }
    }

    /// The step hint for the status bar.
    pub fn hint(&self, ed: &Editor) -> Option<(String, String)> {
        if let Some(t) = modify::tool(ed) {
            return Some((t.op.key().into(), t.step().into()));
        }
        if let Some(d) = &self.dist {
            return Some(("distribute".into(), d.step_key().into()));
        }
        if let Some(m) = &self.mt {
            return Some(("match_type".into(), m.step_key().into()));
        }
        if let Some(a) = &self.ann {
            return Some((a.hint_tool().into(), "first".into()));
        }
        if let Some(m) = &self.ms {
            let step = if m.picks.anchors.is_empty() { "first" } else { "second" };
            return Some((m.hint_tool().into(), step.into()));
        }
        self.dim
            .as_ref()
            .map(|d| (d.hint_tool().into(), d.step().into()))
    }

    fn dist_out(&mut self, ed: &mut Editor, o: DistOut) {
        match o {
            DistOut::Exit => self.dist = None,
            DistOut::Error(m) => ed.say(Severity::Refusal, m),
            DistOut::Done(m) => {
                ed.status = Some(m);
                self.dist = None;
            }
            DistOut::Nothing => {}
        }
    }

    fn mt_out(&mut self, ed: &mut Editor, o: MatchOut) {
        match o {
            MatchOut::Exit => self.mt = None,
            MatchOut::Error(m) => ed.say(Severity::Refusal, m),
            MatchOut::Done(m) => ed.say(Severity::Info, m),
            MatchOut::Nothing => {}
        }
    }

    /// Double click edits a tag in Select mode or with the tag tool; other tools keep the click.
    fn tag_edit_allowed(&self, ed: &Editor) -> bool {
        if modify::running(ed) || self.dim.is_some() || self.dist.is_some() {
            return false;
        }
        match &self.ann {
            Some(a) => a.mode == AnnMode::Tag,
            None => !ed.tool_armed() && !ed.opening_mode,
        }
    }

    /// Opens the inline edit on the tag under the cursor; a read-only or locked tag says why.
    pub fn begin_tag_edit(&mut self, ed: &mut Editor) -> bool {
        let Some(id) = self.hot_tag else {
            return false;
        };
        match TagEdit::begin(ed, id) {
            Ok(e) => self.tag_edit = Some(e),
            Err(why) => ed.say(Severity::Refusal, why),
        }
        true
    }

    /// Enter (`accept`) writes the edit as one operation, Esc drops it.
    pub fn finish_tag_edit(&mut self, ed: &mut Editor, accept: bool) {
        let Some(e) = self.tag_edit.take() else {
            return;
        };
        if accept {
            let o = e.commit(ed);
            self.ann_out(ed, o);
        }
    }

    /// The field over the tag head: host value, duplicate warning, Enter / Esc.
    fn tag_edit_ui(&mut self, ui: &mut egui::Ui, ed: &mut Editor, to_s: &dyn Fn(V2) -> Pos2) {
        let Some(e) = &mut self.tag_edit else {
            return;
        };
        let (mut enter, mut esc) = (false, false);
        let warn = e.warning(ed);
        let at = to_s(e.head);
        egui::Area::new(egui::Id::new("tg1_edit"))
            .order(ddc_ui::z::Z::Canvas.order())
            .fixed_pos(at - egui::vec2(48.0, 14.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let first = ui.add(
                        egui::TextEdit::singleline(&mut e.mark)
                            .desired_width(96.0)
                            .hint_text(crate::tr("tags.hint_mark", &[])),
                    );
                    if e.fresh {
                        first.request_focus();
                        e.fresh = false;
                    }
                    let mut lost = first.lost_focus();
                    let mut has = first.has_focus();
                    if e.is_room() {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut e.name)
                                .desired_width(160.0)
                                .hint_text(crate::tr("tags.hint_name", &[])),
                        );
                        lost = lost || r.lost_focus();
                        has = has || r.has_focus();
                    }
                    if let Some(w) = &warn {
                        ui.label(egui::RichText::new(w).small());
                    }
                    let (k_enter, k_esc) = ui.input(|i| {
                        (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape))
                    });
                    // Enter and a click elsewhere (blur) both commit; Tab between fields does not
                    enter = (k_enter || !k_esc) && lost && !has;
                    esc = k_esc;
                });
            });
        if esc {
            self.finish_tag_edit(ed, false);
        } else if enter {
            self.finish_tag_edit(ed, true);
        }
    }

    fn ann_out(&mut self, ed: &mut Editor, o: AnnOut) {
        match o {
            AnnOut::Exit => self.ann = None,
            AnnOut::Error(m) => ed.say(Severity::Refusal, m),
            AnnOut::Done(m) => ed.say(Severity::Info, m),
            AnnOut::Nothing => {}
        }
    }

    fn dim_out(&mut self, ed: &mut Editor, o: DimOut) {
        match o {
            DimOut::Exit => self.dim = None,
            DimOut::Error(m) => ed.say(Severity::Refusal, m),
            DimOut::Done(n) => ed.say(Severity::Info, crate::tr("modify_ops.dims.done", &[("n", &n.to_string())])),
            DimOut::Nothing => {}
        }
    }

    /// Plan input by lane B; true means the event was consumed (not passed to the plan).
    pub fn input(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        ed: &mut Editor,
        to_w: &dyn Fn(Pos2) -> V2,
        r: f64,
    ) -> bool {
        let hover = resp.hover_pos();
        self.rect = Some(resp.rect);
        if modify::tool(ed).is_some_and(|t| t.free) {
            // started in a drafting view: its targets are not on this plan
            modify::stop(ed);
        }
        // the modify session gets the pointer and keys through the editor; here only the
        // per-frame trial pulse, which needs the frame clock
        let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_pass_nr());
        if let Some(Some(w)) = modify::with(ed, |t, ed| t.pulse(ed, now, frame)) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(w));
        }
        if let (Some(d), Some(h)) = (&mut self.dim, hover) {
            d.cursor = to_w(h);
        }
        if let (Some(d), Some(h)) = (&mut self.dist, hover) {
            d.cursor = to_w(h);
        }
        if let (Some(m), Some(h)) = (&mut self.mt, hover) {
            m.hover(ed, to_w(h), r);
        }
        if let (Some(a), Some(h)) = (&mut self.ann, hover) {
            a.cursor = to_w(h);
        }
        if let (Some(m), Some(h)) = (&mut self.ms, hover) {
            m.cursor = to_w(h);
        }
        // a tag under the cursor: highlight; in the tag tool, dragging the head
        self.hot_tag = hover
            .and_then(|h| tags::tag_at(ed, to_w(h), r))
            .map(|t| t.id);
        if resp.double_clicked() && self.tag_edit_allowed(ed) && self.begin_tag_edit(ed) {
            return true;
        }
        if let Some(a) = &mut self.ann {
            if a.mode == AnnMode::Tag {
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        if let Some(t) = tags::tag_at(ed, to_w(p), r) {
                            a.drag = Some((t.id, t.spec.offset, to_w(p)));
                        }
                    }
                }
                if resp.drag_stopped_by(egui::PointerButton::Primary) && a.drag.is_some() {
                    let at = hover
                        .or(resp.interact_pointer_pos())
                        .map(to_w)
                        .unwrap_or(a.cursor);
                    let o = a.drop_drag(ed, at);
                    self.ann_out(ed, o);
                    return true;
                }
            }
        }
        if self.active() {
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let p = to_w(p);
                    if let Some(d) = &mut self.dim {
                        let o = d.click(ed, p, r);
                        self.dim_out(ed, o);
                    } else if let Some(d) = &mut self.dist {
                        let o = d.click(ed, p, r);
                        self.dist_out(ed, o);
                    } else if let Some(m) = &mut self.mt {
                        let o = m.click(ed, p, r);
                        self.mt_out(ed, o);
                    } else if let Some(a) = &mut self.ann {
                        let o = a.click(ed, p, r);
                        self.ann_out(ed, o);
                    } else if let Some(m) = &mut self.ms {
                        let o = m.click(ed, p, r);
                        self.ms_out(ed, o);
                    }
                }
            }
            if self.mt.as_ref().is_some_and(|m| m.src.is_some()) {
                self.mt_window(resp, ed, to_w, r);
            }
            if crate::ui_kit::canvas_owns_keys(ui.ctx()) && !ui.ctx().wants_keyboard_input() {
                self.keys(ui, ed);
            }
            return true;
        }
        // picking: a box from empty space, Tab cycles candidates under the cursor
        if ed.tool_armed() || ed.opening_mode || modify::running(ed) {
            return false;
        }
        let vis = |s| self.vis.visible(ed, s);
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let w = to_w(p);
                if selection::candidates(ed, w, r, vis).is_empty()
                    && !crate::annot_text::grabs(ed, w, r)
                {
                    self.drag = Some((p, w, p, w));
                }
            }
        }
        if let Some(d) = &mut self.drag {
            if let Some(h) = hover.or(resp.interact_pointer_pos()) {
                (d.2, d.3) = (h, to_w(h));
            }
            if resp.drag_stopped_by(egui::PointerButton::Primary) {
                let (s0, w0, s1, w1) = *d;
                self.drag = None;
                let mode = BoxMode::from_drag(s0.x, s1.x);
                let found = selection::in_box(ed, w0, w1, mode, |s| self.vis.visible(ed, s));
                let m = ui.input(|i| i.modifiers);
                ed.selection = selection::combine(&ed.selection, &found, m.command, m.shift);
            }
            return true;
        }
        // Tab in Select belongs to the plan_view cycle (plan_cycle.rs); the old `cycle` only serves tests
        if let (Some(h), Some(a)) = (hover, self.cycle.at) {
            if (to_w(h) - a).len() > r {
                self.cycle = Cycle::default();
            }
        }
        false
    }

    /// Input of the drafting tab for the edit tool (move, rotate, copy, mirror, array) on that
    /// view's items; true: the tool took the event. Plan-only tools stay out of the tab.
    pub fn drafting_input(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        ed: &mut Editor,
        to_w: &dyn Fn(Pos2) -> V2,
    ) -> bool {
        if let Some(used) = self.drafting_free(ui, resp, ed, to_w) {
            return used;
        }
        let Some(t) = modify::tool(ed) else {
            return false;
        };
        if !t.selecting && !t.targets.iter().all(|s| crate::annot_text::in_tool_view(ed, *s)) {
            // started on the plan: its targets are hidden behind this tab, never edited unseen
            modify::stop(ed);
            return false;
        }
        let targets = crate::annot_text::plan_targets(ed);
        let Some(t) = modify::tool_mut(ed) else {
            return false;
        };
        t.free = true;
        if t.selecting {
            // pick on the canvas; the tool goes on as soon as an item of the view is picked
            t.targets = targets;
            if t.targets.is_empty() {
                return false;
            }
            t.selecting = false;
        }
        // drafting coordinates are not plan points: the tab drives the tool directly
        let hover = resp.hover_pos().map(to_w);
        let click = resp.clicked().then(|| resp.interact_pointer_pos()).flatten().map(to_w);
        let o = modify::with(ed, |t, ed| {
            if let Some(h) = hover {
                t.hover(ed, h);
            }
            click.map(|p| t.click_snapped(ed, p))
        });
        if let Some(Some(o)) = o {
            modify::out(ed, o);
        }
        if crate::ui_kit::canvas_owns_keys(ui.ctx()) && !ui.ctx().wants_keyboard_input() {
            modify_keys(ui, ed);
        }
        true
    }

    /// Match type: a drag from empty space is a window, released it applies to every hit.
    fn mt_window(
        &mut self,
        resp: &egui::Response,
        ed: &mut Editor,
        to_w: &dyn Fn(Pos2) -> V2,
        r: f64,
    ) {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let (Some(p), Some(m)) = (resp.interact_pointer_pos(), &self.mt) {
                if m.target_at(ed, to_w(p), r).is_none() {
                    self.drag = Some((p, to_w(p), p, to_w(p)));
                }
            }
        }
        if let Some(d) = &mut self.drag {
            if let Some(h) = resp.hover_pos().or(resp.interact_pointer_pos()) {
                (d.2, d.3) = (h, to_w(h));
            }
            if resp.drag_stopped_by(egui::PointerButton::Primary) {
                let (s0, w0, s1, w1) = *d;
                self.drag = None;
                let mode = BoxMode::from_drag(s0.x, s1.x);
                let hits = selection::in_box(ed, w0, w1, mode, |s| self.vis.visible(ed, s));
                if let Some(m) = &mut self.mt {
                    let o = m.apply(ed, &hits);
                    self.mt_out(ed, o);
                }
            }
        }
    }

    pub(crate) fn keys(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        if modify::running(ed) {
            modify_keys(ui, ed);
            return;
        }
        let tab = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
        let evs = ui.input(|i| i.events.clone());
        if tab {
            if let Some(d) = &mut self.dim {
                d.tab();
            }
        }
        for e in evs {
            match e {
                egui::Event::Text(s) => {
                    if let Some(d) = &mut self.dist {
                        d.input
                            .extend(s.chars().filter(|c| c.is_ascii_digit()).take(4));
                        d.input.truncate(4);
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if !modifiers.command => match key {
                    egui::Key::Escape => {
                        if let Some(d) = &mut self.dim {
                            let o = d.escape();
                            self.dim_out(ed, o);
                        } else if let Some(d) = &mut self.dist {
                            let o = d.escape();
                            self.dist_out(ed, o);
                        } else if let Some(m) = &mut self.mt {
                            let o = m.escape();
                            self.mt_out(ed, o);
                        } else if let Some(a) = &mut self.ann {
                            let o = a.escape();
                            self.ann_out(ed, o);
                        } else if let Some(m) = &mut self.ms {
                            let o = m.escape();
                            self.ms_out(ed, o);
                        }
                    }
                    egui::Key::Enter => {
                        if let Some(d) = &mut self.dist {
                            let o = d.enter(ed);
                            self.dist_out(ed, o);
                        } else if let Some(m) = &mut self.ms {
                            m.open_edit(ed, 0.25);
                        }
                    }
                    egui::Key::Backspace => {
                        if let Some(d) = &mut self.dist {
                            d.input.pop();
                        }
                    }
                    egui::Key::Space => {
                        if let Some(d) = &mut self.dist {
                            d.space();
                        }
                    }
                    egui::Key::Delete => {
                        if let (Some(a), Some(id)) = (&mut self.ann, self.hot_tag) {
                            let o = a.delete(ed, id);
                            self.ann_out(ed, o);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    /// Drawing over the plan: the tool preview, box, "2 of 3", the isolation frame.
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        p: &egui::Painter,
        ed: &mut Editor,
        to_s: &dyn Fn(V2) -> Pos2,
        rect: egui::Rect,
        t: &Tokens,
    ) {
        let ov = t.ov();
        let (preview, temp) = (ov.preview, ov.snap);
        let font = egui::FontId::proportional(ddc_ui::theme::type_size::LABEL);
        // selection box
        if let Some((s0, w0, s1, w1)) = self.drag {
            let r = egui::Rect::from_two_pos(s0, s1);
            let mode = BoxMode::from_drag(s0.x, s1.x);
            // live preview: what will enter the selection on release
            let hit = selection::in_box(ed, w0, w1, mode, |s| self.vis.visible(ed, s));
            draw_ghost(p, ed, &hit, &|q| q, to_s, ov.hover);
            crate::overlay::rubber_band(p, r, matches!(mode, BoxMode::Crossing), ov);
        }
        // Tab cycle: "2 of 3" at the cursor
        if let (Some((i, n)), Some(at)) = (self.cycle.label(), self.cycle.at) {
            let s = format!("{i} {} {n}", self.labels.t("mt.of"));
            chip(p, to_s(at) + egui::vec2(16.0, -40.0), &s, t, font.clone());
        }
        if let Some(tool) = modify::tool(ed) {
            draw_tool(p, ed, tool, to_s, preview, temp, t);
            let frame = ui.ctx().cumulative_pass_nr();
            if let Some(g) = tool.trial.ghost(ed.model.serial(), frame) {
                crate::trial::paint(p, g, to_s, ov.ghost());
            }
            if let (true, Some(sn)) = (tool.takes_points(), tool.snap()) {
                let from = tool.pts.last().copied();
                let (all, i) = (&tool.snaps, tool.snap_idx);
                crate::plan_view::snap_marker(p, to_s, sn, all, i, from, ov);
            }
        }
        if let Some(d) = &self.dim {
            draw_dim(p, ed, d, to_s, preview, t);
        }
        draw_dims(p, ed, to_s, t);
        tags::draw_tags(p, ed, to_s, t, self.hot_tag);
        self.tag_edit_ui(ui, ed, to_s);
        if let Some(d) = &self.dist {
            draw_dist(p, ed, d, to_s, preview, t);
        }
        if let Some(m) = &self.mt {
            match_type::draw(p, ed, m, to_s, t);
        }
        if let Some(a) = &self.ann {
            tags::draw_ann(p, ed, a, to_s, preview, t);
        }
        if let Some(m) = &self.ms {
            crate::tools::measure::draw_tool(p, ed, m, to_s, 0.25, t);
        }
        if let Some(o) = self.ms.as_mut().map(|m| m.edit_ui(ui, ed, to_s)) {
            self.ms_out(ed, o);
        }
        // banner: view frame and the temporary isolate/hide badge
        if self.vis.temp_on() {
            p.rect_stroke(
                rect.shrink(1.5),
                0.0,
                Stroke::new(3.0, temp),
                egui::StrokeKind::Inside,
            );
            let th = ddc_ui::theme::of(ui.ctx());
            let isolate = matches!(self.vis.temp, crate::isolate::Temp::Isolate(_));
            let b = crate::modify_opts::banner(isolate, crate::ui_is_ru());
            let at = rect.left_bottom()
                + egui::vec2(ddc_ui::theme::space::MD, -ddc_ui::theme::space::MD);
            let mut act = (false, false);
            egui::Area::new(egui::Id::new("ddc.b.iso"))
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(at)
                .order(ddc_ui::z::Z::Canvas.order())
                .show(ui.ctx(), |ui| {
                    crate::modify_opts::bar_frame(&th).show(ui, |ui| {
                        act = crate::modify_opts::banner_row(ui, &th, &b);
                    });
                });
            if act.0 {
                self.vis.apply(ed);
            }
            if act.1 {
                crate::modify_opts::exit_banner(&mut self.vis);
            }
        }
        self.options(ui, ed, rect, t);
        self.filter_window(ui, ed, crate::ui_is_ru());
        self.tag_all_window(ui, ed);
    }

    /// One frame of the open selection filter in the given language, for the layout contract.
    #[cfg(test)]
    pub(crate) fn layout_filter(&mut self, ctx: &egui::Context, ed: &mut Editor, ru: bool) {
        self.filter_open = true;
        egui::CentralPanel::default().show(ctx, |ui| self.filter_window(ui, ed, ru));
    }

    /// The options panel of the active tool (top of the plan).
    fn options(&mut self, ui: &mut egui::Ui, ed: &mut Editor, rect: egui::Rect, t: &Tokens) {
        let lb = self.labels.clone();
        if self.dist.is_some() || self.ann.is_some() {
            self.ann_options(ui, ed, rect, t, &lb);
            return;
        }
        let th_arc = ddc_ui::theme::of(ui.ctx());
        let th: &ddc_ui::Theme = &th_arc;
        let ru = crate::ui_is_ru();
        use ddc_ui::widgets;
        if !modify::running(ed) {
            // face/axis snapping belongs to the linear dimension only
            if let Some(d) = self.dim.as_mut().filter(|d| d.mode == DimMode::Linear) {
                use dims::Attach;
                let at = rect.left_top() + egui::vec2(ddc_ui::theme::space::SM, ddc_ui::theme::space::SM);
                egui::Area::new(egui::Id::new("ddc.b.opts"))
                    .fixed_pos(at)
                    .show(ui.ctx(), |ui| {
                        crate::modify_opts::bar_frame(th).show(ui, |ui| {
                            crate::modify_opts::row_style(ui, th);
                            let face = lb.t("mt.attach_face");
                            let axis = lb.t("mt.attach_axis");
                            let sel = usize::from(d.attach == Attach::Axis);
                            if let Some(i) = widgets::segmented(ui, th, &[face.as_str(), axis.as_str()], sel) {
                                d.attach = if i == 0 { Attach::Face } else { Attach::Axis };
                            }
                        });
                    });
            }
            return;
        }
        modify::with(ed, |tool, ed| Self::tool_bar(ui, ed, tool, rect, th, &lb, ru));
    }

    /// Option bar of the running modify tool (top of the plan).
    fn tool_bar(
        ui: &mut egui::Ui,
        ed: &mut Editor,
        tool: &mut ModTool,
        rect: egui::Rect,
        th: &ddc_ui::Theme,
        lb: &Labels,
        ru: bool,
    ) {
        use ddc_ui::widgets;
        // a tool with no option, no typed value and no note draws no bar (it was an empty frame)
        let serial = ed.model.serial();
        let has_content = !crate::modify_opts::opts_of(tool.op).is_empty()
            || !tool.input.is_empty()
            || tool.selecting
            || tool.op == Op::Offset
            || tool.trial.refusal(serial).is_some()
            || tool.trial.off_hint().is_some();
        if !has_content {
            return;
        }
        let at = rect.left_top() + egui::vec2(ddc_ui::theme::space::SM, ddc_ui::theme::space::SM);
        egui::Area::new(egui::Id::new("ddc.b.opts"))
            .fixed_pos(at)
            .show(ui.ctx(), |ui| {
                crate::modify_opts::bar_frame(th).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        use crate::modify_opts::{self as mo, OptKind};
                        mo::row_style(ui, th);
                        let o = &mut tool.opts;
                        for k in mo::opts_of(tool.op) {
                            let label = lb.t(k.label_key());
                            match k {
                                OptKind::Copy => {
                                    widgets::checkbox(ui, th, &mut o.copy, &label);
                                }
                                OptKind::Constrain => {
                                    widgets::checkbox(ui, th, &mut o.constrain, &label);
                                }
                                OptKind::Disjoin => {
                                    widgets::checkbox(ui, th, &mut o.disjoin, &label);
                                }
                                OptKind::Multiple => {
                                    widgets::checkbox(ui, th, &mut o.multiple, &label);
                                }
                                OptKind::Radial => {
                                    widgets::checkbox(ui, th, &mut o.radial, &label);
                                }
                                OptKind::ToLast => {
                                    widgets::checkbox(ui, th, &mut o.to_last, &label);
                                }
                                OptKind::Count => {
                                    mo::count_field(ui, th, ru, &mut o.count);
                                }
                                OptKind::Dist => {
                                    mo::dist_field(ui, th, ru, ed.tool.units, &mut o.dist);
                                }
                            }
                        }
                        if !tool.input.is_empty() {
                            widgets::vdivider(ui, th, th.d().control);
                            mo::typed_chip(ui, th, &tool.input);
                        }
                        if tool.selecting {
                            widgets::vdivider(ui, th, th.d().control);
                            if mo::done_button(ui, th, ru, &lb.t("mt.done")) {
                                let o = tool.enter(ed);
                                match o {
                                    Out::Refused(m) => ed.say(Severity::Refusal, m),
                                    Out::Error(m) => ed.say(Severity::Error, m),
                                    _ => {}
                                }
                            }
                        }
                    });
                    let cap = th.font(ddc_ui::theme::Role::Caption);
                    if let Some((f, why)) = tool.trial.refusal(ed.model.serial()) {
                        let line = crate::trial::refusal_text(f, why);
                        widgets::elided_label(ui, &line, cap.clone(), th.sem().error);
                    }
                    if let Some(h) = tool.trial.off_hint() {
                        widgets::elided_label(ui, &h, cap, th.sem().text_secondary);
                    }
                    if tool.op == Op::Offset {
                        crate::modify_opts::reject_line(ui, th, ru);
                    }
                });
            });
    }

    /// Selection filter: categories with counts, checkboxes, "Only this"/"All".
    fn filter_window(&mut self, ui: &mut egui::Ui, ed: &mut Editor, ru: bool) {
        if !self.filter_open {
            return;
        }
        let counts = selection::counts(ed, &ed.selection);
        // filled once per opening: unticking the last category must not refill the set
        if !self.filter_init {
            self.filter_keep = counts.iter().map(|c| c.0).collect();
            self.filter_init = true;
        }
        // only ticks of categories still in the selection count (it may change while open)
        let mut live: HashSet<&'static str> = HashSet::new();
        let lb = self.labels.clone();
        let th_arc = ddc_ui::theme::of(ui.ctx());
        let th: &ddc_ui::Theme = &th_arc;
        let mut open = true;
        let mut ok = false;
        kit::Dialog::new(
            "selection_filter",
            lb.t("flt.title"),
            kit::DialogSize::Confirm,
        )
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            let rows: Vec<sel_panels::CatRow> = counts
                .iter()
                .map(|(c, n)| sel_panels::CatRow {
                    label: lb.t(&format!("flt.cat.{c}")),
                    count: n.to_string(),
                    on: self.filter_keep.contains(c),
                })
                .collect();
            let all = crate::tr_lang(ru, "sel_filter.flt.all_cats", &[]);
            let col = crate::tr_lang(ru, "sel_filter.flt.in_sel", &[]);
            let ev = sel_panels::cat_list(ui, th, "flt", &all, &col, &lb.t("flt.only"), &rows);
            match ev {
                Some(sel_panels::RowEvent::Toggle(i, on)) => {
                    if on {
                        self.filter_keep.insert(counts[i].0);
                    } else {
                        self.filter_keep.remove(counts[i].0);
                    }
                }
                Some(sel_panels::RowEvent::Only(i)) => self.filter_keep = [counts[i].0].into(),
                Some(sel_panels::RowEvent::SetAll(on)) => {
                    self.filter_keep = if on {
                        counts.iter().map(|c| c.0).collect()
                    } else {
                        HashSet::new()
                    };
                }
                None => {}
            }
            live = counts.iter().map(|c| c.0).filter(|c| self.filter_keep.contains(c)).collect();
            let why = sel_panels::filter_ok_block(live.len())
                .map(|k| crate::tr_lang(ru, k, &[]));
            if let Some(w) = &why {
                sel_panels::note(ui, th, w);
            }
            kit::dialog_footer(ui, |ui| {
                ok = kit::primary_action(ui, lb.t("flt.ok"), why.as_deref());
            });
        });
        if ok {
            sel_panels::apply_filter(ed, &live);
        }
        if ok || !open {
            self.filter_open = false;
            self.filter_keep.clear();
            self.filter_init = false;
        }
    }

    /// The panel of distribution parameters and tag tools.
    fn ann_options(
        &mut self,
        ui: &mut egui::Ui,
        ed: &mut Editor,
        rect: egui::Rect,
        _t: &Tokens,
        lb: &Labels,
    ) {
        use ddc_ui::widgets::{self, Button};
        let th_arc = ddc_ui::theme::of(ui.ctx());
        let th: &ddc_ui::Theme = &th_arc;
        let ru = crate::ui_is_ru();
        let units = ed.tool.units;
        let at = rect.left_top() + egui::vec2(ddc_ui::theme::space::SM, ddc_ui::theme::space::SM);
        let mut out = None;
        egui::Area::new(egui::Id::new("ddc.b.opts"))
            .fixed_pos(at)
            .show(ui.ctx(), |ui| {
                crate::modify_opts::bar_frame(th).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        crate::modify_opts::row_style(ui, th);
                        if let Some(d) = &mut self.dist {
                            let (eq, st) = (lb.t("dt.equal"), lb.t("dt.step"));
                            let sel = usize::from(!d.equal);
                            if let Some(i) = widgets::segmented(ui, th, &[eq.as_str(), st.as_str()], sel) {
                                d.equal = i == 0;
                            }
                            widgets::vdivider(ui, th, th.d().control);
                            sel_panels::count_field(ui, th, ru, &mut d.n);
                            let lbl = lb.t("dt.margin");
                            sel_panels::len_field(ui, th, ru, units, "margin", &lbl, &mut d.margin, 50.0, true);
                            if !d.equal {
                                let lbl = lb.t("dt.stepv");
                                sel_panels::len_field(ui, th, ru, units, "step", &lbl, &mut d.step, 50.0, false);
                            }
                            if !d.input.is_empty() {
                                widgets::vdivider(ui, th, th.d().control);
                                crate::modify_opts::typed_chip(ui, th, &d.input);
                            }
                            widgets::vdivider(ui, th, th.d().control);
                            if d.rule.is_some() {
                                if Button::secondary(&lb.t("dt.release")).small().show(ui, th).clicked() {
                                    out = Some(d.release(ed));
                                }
                            } else {
                                widgets::toggle(ui, th, &mut d.link, &lb.t("dt.link"));
                            }
                            if d.src.is_some()
                                && Button::secondary(&lb.t("dt.edit")).small().show(ui, th).clicked()
                            {
                                out = Some(d.commit(ed));
                            }
                        } else if let Some(a) = &mut self.ann {
                            match a.mode {
                                AnnMode::Tag => {
                                    widgets::checkbox(ui, th, &mut a.leader, &lb.t("tg.leader"));
                                }
                                AnnMode::Number => {
                                    use tags::NumCat;
                                    sel_panels::caption(ui, th, &lb.t("tg.num.cat"));
                                    let cats = [NumCat::Door, NumCat::Window, NumCat::Room];
                                    let names = [lb.t("tg.cat.door"), lb.t("tg.cat.window"), lb.t("tg.cat.room")];
                                    let items = [names[0].as_str(), names[1].as_str(), names[2].as_str()];
                                    let sel = cats.iter().position(|c| *c == a.cat).unwrap_or(0);
                                    if let Some(i) = widgets::segmented(ui, th, &items, sel) {
                                        a.cat = cats[i];
                                    }
                                    widgets::vdivider(ui, th, th.d().control);
                                    sel_panels::start_field(ui, th, &lb.t("tg.num.start"), &mut a.next);
                                }
                            }
                        }
                    });
                });
                sel_panels::count_reject_line(ui, th, ru);
            });
        if let Some(o) = out {
            self.dist_out(ed, o);
        }
    }

    /// "Tag all": categories with the count of untagged elements, a leader, OK.
    fn tag_all_window(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let Some(dlg) = &mut self.tag_all else {
            return;
        };
        dlg.refresh(ed);
        let lb = self.labels.clone();
        let (th_arc, ru) = (ddc_ui::theme::of(ui.ctx()), crate::ui_is_ru());
        let th: &ddc_ui::Theme = &th_arc;
        let (mut open, mut ok) = (true, false);
        kit::Dialog::new("tag_all", lb.t("tg.title"), kit::DialogSize::Confirm)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                let rows: Vec<sel_panels::CatRow> = dlg
                    .cats
                    .iter()
                    .zip(&dlg.counts)
                    .map(|((c, on), n)| sel_panels::CatRow {
                        label: lb.t(tags::cat_key(*c)),
                        count: n.to_string(),
                        on: *on,
                    })
                    .collect();
                let all = crate::tr_lang(ru, "sel_filter.flt.all_cats", &[]);
                let ev = sel_panels::cat_list(ui, th, "tg_all", &all, &lb.t("tg.new"), &lb.t("flt.only"), &rows);
                match ev {
                    Some(sel_panels::RowEvent::Toggle(i, on)) => dlg.cats[i].1 = on,
                    Some(sel_panels::RowEvent::Only(i)) => {
                        for (j, c) in dlg.cats.iter_mut().enumerate() {
                            c.1 = j == i;
                        }
                    }
                    Some(sel_panels::RowEvent::SetAll(on)) => {
                        for c in dlg.cats.iter_mut() {
                            c.1 = on;
                        }
                    }
                    None => {}
                }
                ui.add_space(ddc_ui::theme::space::SM);
                ddc_ui::widgets::checkbox(ui, th, &mut dlg.leader, &lb.t("tg.leader"));
                let why = sel_panels::tag_ok_block(dlg.chosen().len())
                    .map(|k| crate::tr_lang(ru, k, &[]));
                if let Some(w) = &why {
                    sel_panels::note(ui, th, w);
                }
                kit::dialog_footer(ui, |ui| {
                    ok = kit::primary_action(ui, lb.t("flt.ok"), why.as_deref());
                });
            });
        if ok {
            let o = dlg.commit(ed);
            self.ann_out(ed, o);
        }
        if ok || !open {
            self.tag_all = None;
        }
    }
}

fn chip(p: &egui::Painter, at: Pos2, s: &str, t: &Tokens, font: egui::FontId) {
    let g = p.layout_no_wrap(s.to_string(), font, t.text);
    let r = egui::Rect::from_min_size(at, g.size()).expand2(egui::vec2(6.0, 3.0));
    p.rect_filled(r, ddc_ui::theme::radius::MD, t.panel);
    p.rect_stroke(r, 4.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    p.galley(at, g, t.text);
}

fn draw_ghost(
    p: &egui::Painter,
    ed: &Editor,
    sel: &[Sel],
    f: &dyn Fn(V2) -> V2,
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
) {
    for &s in sel {
        for o in selection::outlines(ed, s) {
            let pts: Vec<Pos2> = o.iter().map(|&q| to_s(f(q))).collect();
            if pts.len() < 3 {
                continue;
            }
            p.add(egui::Shape::convex_polygon(
                pts.clone(),
                col.gamma_multiply(0.35),
                Stroke::NONE,
            ));
            let mut c = pts.clone();
            c.push(pts[0]);
            p.add(egui::Shape::line(c, Stroke::new(1.5, col)));
        }
    }
}

fn draw_tool(
    p: &egui::Painter,
    ed: &Editor,
    tool: &ModTool,
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
    temp: Color32,
    t: &Tokens,
) {
    let font = egui::FontId::proportional(ddc_ui::theme::type_size::LABEL);
    let st = Stroke::new(1.5, col);
    let dash = |a: V2, b: V2| p.extend(egui::Shape::dashed_line(&[to_s(a), to_s(b)], st, 6.0, 4.0));
    let c = tool.cursor;
    for &q in &tool.pts {
        p.circle_stroke(to_s(q), 4.0, Stroke::new(1.5, temp));
    }
    for &w in &tool.walls {
        if let Some((a, b)) = axis(ed, w) {
            p.line_segment([to_s(a), to_s(b)], Stroke::new(3.0, temp));
        }
    }
    if let Some(f) = tool.xf(ed) {
        draw_ghost(p, ed, &tool.targets, &*f, to_s, col);
    }
    match (tool.op, tool.pts.as_slice()) {
        (Op::Move | Op::Copy, [p0]) => {
            dash(*p0, c);
            let l = display_len(ed, (c - *p0).len());
            chip(p, to_s((*p0 + c) * 0.5) + egui::vec2(6.0, 6.0), &l, t, font);
        }
        (Op::Rotate, [o]) => {
            p.circle_stroke(to_s(*o), 6.0, st);
            dash(*o, c);
        }
        (Op::Scale, [o]) => dash(*o, c),
        (Op::Scale, [o, r]) => {
            dash(*o, *r);
            dash(*o, c);
            if let Some(k) = scale_factor(*o, *r, c) {
                let lab = format!("\u{d7}{k:.3}");
                chip(p, to_s(c) + egui::vec2(8.0, 8.0), &lab, t, font);
            }
        }
        (Op::Rotate, [o, r1]) => {
            dash(*o, *r1);
            dash(*o, c);
            let (a0, a1) = (ang(*r1 - *o), ang(c - *o));
            let mut da = a1 - a0;
            while da > std::f64::consts::PI {
                da -= std::f64::consts::TAU;
            }
            while da < -std::f64::consts::PI {
                da += std::f64::consts::TAU;
            }
            let rr = ((*r1 - *o).len().min((c - *o).len()) * 0.4).max(0.2);
            let arc: Vec<Pos2> = (0..=32)
                .map(|k| {
                    to_s(
                        *o + v2(
                            (a0 + da * k as f64 / 32.0).cos(),
                            (a0 + da * k as f64 / 32.0).sin(),
                        ) * rr,
                    )
                })
                .collect();
            p.add(egui::Shape::line(arc, st));
            let mid = a0 + da / 2.0;
            let lab = format!("{:.1}°", da.to_degrees());
            chip(
                p,
                to_s(*o + v2(mid.cos(), mid.sin()) * rr * 1.3),
                &lab,
                t,
                font,
            );
        }
        (Op::MirrorDraw, [a]) => dash(*a, c),
        (Op::Array, _) => {
            let xs = tool.array_xfs();
            for f in &xs {
                draw_ghost(p, ed, &tool.targets, &**f, to_s, col);
            }
            if let Some(&p0) = tool.pts.first() {
                dash(p0, c);
            }
            if !xs.is_empty() {
                let lab = format!("{}", tool.opts.count);
                let at = to_s(c) + egui::vec2(14.0, -20.0);
                let ink = t.paper;
                let g = p.layout_no_wrap(
                    lab,
                    egui::FontId::proportional(ddc_ui::theme::type_size::TITLE),
                    ink,
                );
                let r = egui::Rect::from_min_size(at, g.size()).expand2(egui::vec2(7.0, 3.0));
                p.rect_filled(r, 9.0, col);
                p.galley(at, g, ink);
            }
        }
        (Op::Offset, _) => {
            if let Some((a, b)) = wall_at(ed, c, tool.r).and_then(|w| axis(ed, w)) {
                let d = b - a;
                let n = v2(-d.y, d.x) * (1.0 / d.len().max(1e-12));
                let left = d.x * (c.y - a.y) - d.y * (c.x - a.x) > 0.0;
                let o = n * if left {
                    tool.opts.dist
                } else {
                    -tool.opts.dist
                };
                dash(a + o, b + o);
            }
        }
        (Op::Split, _) => {
            if let Some((a, b)) = wall_at(ed, c, tool.r).and_then(|w| axis(ed, w)) {
                let d = b - a;
                let n = v2(-d.y, d.x) * (0.3 / d.len().max(1e-12));
                let k = tool.opts.parts;
                // divide: a tick at every cut (arc walls: chord preview)
                let cuts: Vec<V2> = if k >= 2 {
                    (1..k).map(|i| a + d * (i as f64 / k as f64)).collect()
                } else {
                    vec![crate::snap::closest_on_segment(c, a, b).0]
                };
                for q in cuts {
                    p.line_segment([to_s(q - n), to_s(q + n)], Stroke::new(2.0, temp));
                }
            }
        }
        _ => {}
    }
}

fn display_len(ed: &Editor, l: f64) -> String {
    crate::units::display_length(l, ed.tool.units)
}

/// Distribution preview: slots on the wall axis, conflicts are red with a label.
fn draw_dist(
    p: &egui::Painter,
    ed: &Editor,
    d: &DistTool,
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
    t: &Tokens,
) {
    if d.src.is_none() {
        if let Some(id) = distribute::opening_at(ed, d.cursor, 0.15) {
            if let Some(o) = ed.openings.iter().find(|o| o.id == id) {
                if let Some(w) = ed.walls.iter().find(|w| w.id == o.wall) {
                    let u = (w.b - w.a) * (1.0 / (w.b - w.a).len().max(1e-9));
                    let c = w.a + u * (o.along + o.width / 2.0);
                    p.circle_stroke(to_s(c), 8.0, Stroke::new(1.5, col));
                }
            }
        }
        return;
    }
    let Some((wall, width, slots)) = d.preview(ed) else {
        return;
    };
    let Some(w) = ed.walls.iter().find(|w| w.id == wall) else {
        return;
    };
    let l = (w.b - w.a).len().max(1e-9);
    let u = (w.b - w.a) * (1.0 / l);
    let n = v2(-u.y, u.x);
    let ht = ed
        .types
        .iter()
        .find(|x| x.id == w.wall_type)
        .map_or(0.1, |x| x.thickness() / 2.0)
        + 0.05;
    let red = t.ov().error;
    let font = egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION);
    for (i, s) in slots.iter().enumerate() {
        let a = w.a + u * s.along;
        let b = a + u * width;
        let c = if s.conflict.is_some() { red } else { col };
        let quad = vec![
            to_s(a + n * ht),
            to_s(b + n * ht),
            to_s(b - n * ht),
            to_s(a - n * ht),
        ];
        p.add(egui::Shape::convex_polygon(
            quad,
            c.gamma_multiply(0.25),
            Stroke::new(1.5, c),
        ));
        let label = match s.conflict {
            Some(k) => distribute::conflict_text(k),
            None => (i + 1).to_string(),
        };
        chip(
            p,
            to_s(a + u * (width / 2.0) + n * (ht + 0.35)),
            &label,
            t,
            font.clone(),
        );
    }
}

/// Chain preview: the picked references, a line through the cursor, values.
fn draw_dim(
    p: &egui::Painter,
    ed: &Editor,
    d: &DimTool,
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
    t: &Tokens,
) {
    for (_, q) in &d.refs {
        p.circle_filled(to_s(*q), 3.5, col);
    }
    if let Some((_, q)) = d.pick_ref(ed, d.cursor, 0.15) {
        p.circle_stroke(to_s(q), 6.0, Stroke::new(1.5, col));
    }
    if d.mode != DimMode::Linear || d.refs.len() < 2 {
        return;
    }
    for s in d.chain(ed.level, d.cursor) {
        let (a, b) = (s.refs[0], s.refs[1]);
        let pa = d.refs.iter().find(|r| r.0 == a).map(|r| r.1);
        let pb = d.refs.iter().find(|r| r.0 == b).map(|r| r.1);
        let (Some(pa), Some(pb)) = (pa, pb) else {
            continue;
        };
        let dir = v2(s.dir.0, s.dir.1);
        if let Some(l) = ddc_model::DimLinear::new(pa, pb, Some(dir), s.side, None, s.offset) {
            linear(p, &l, to_s, col, &display_len(ed, l.value()), t);
        }
    }
}

/// Placed dimensions of the level, every kind.
fn draw_dims(p: &egui::Painter, ed: &Editor, to_s: &dyn Fn(V2) -> Pos2, t: &Tokens) {
    dims::draw(p, ed, to_s, t);
    crate::tools::measure::draw(p, ed, to_s, t);
}

fn linear(
    p: &egui::Painter,
    l: &ddc_model::DimLinear,
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
    label: &str,
    t: &Tokens,
) {
    let (a, b) = l.line(0.0);
    let st = Stroke::new(1.0, col);
    p.line_segment([to_s(a), to_s(b)], st);
    for (q, m) in [(l.a, a), (l.b, b)] {
        p.line_segment([to_s(q), to_s(m + l.n * 0.15)], Stroke::new(0.75, col));
        let k = (l.dir + l.n) * 0.08;
        p.line_segment([to_s(m - k), to_s(m + k)], Stroke::new(1.5, col));
    }
    let mid = to_s((a + b) * 0.5 + l.n * 0.12);
    let g = p.layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
        col,
    );
    let r = egui::Rect::from_center_size(mid, g.size());
    p.rect_filled(r.expand(1.0), ddc_ui::theme::radius::SM, t.paper);
    p.galley(r.min, g, col);
}

macro_rules! op_handler {
    ($($name:ident => $op:expr),* $(,)?) => {
        $(fn $name(a: &mut EditorApp) {
            // one modify session for the plan and 3D: the 3D view feeds it its pointer;
            // a 3D selection the plan cannot hold keeps the 3D tool and its refusals
            if a.view3d && crate::edit3d::mirror_array::route_3d_only(a, $op) {
                return;
            }
            a.plan.b.labels = Labels::from(&a.sh.cat);
            a.plan.b.start($op, &mut a.ed);
        })*
    };
}

op_handler!(
    h_move => Op::Move,
    h_copy => Op::Copy,
    h_rotate => Op::Rotate,
    h_mirror => Op::Mirror,
    h_mirror_draw => Op::MirrorDraw,
    h_array => Op::Array,
    h_offset => Op::Offset,
    h_trim => Op::Trim,
    h_extend => Op::Extend,
    h_split => Op::Split,
    h_align => Op::Align,
    h_scale => Op::Scale,
);

/// Split in divide mode: the picked wall becomes equal parts (2 until a count is typed).
fn h_divide(a: &mut EditorApp) {
    a.plan.b.labels = Labels::from(&a.sh.cat);
    a.plan.b.start(Op::Split, &mut a.ed);
    if let Some(t) = modify::tool_mut(&mut a.ed) {
        t.opts.parts = 2;
    }
}

fn labels(a: &mut EditorApp) {
    a.plan.b.labels = Labels::from(&a.sh.cat);
}

fn h_distribute(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_dist(&mut a.ed);
}
fn h_match_type(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_match(&mut a.ed);
}
fn h_tag(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_ann(AnnMode::Tag, &mut a.ed);
}
fn start_ms(a: &mut EditorApp, mode: crate::edit3d::measure_saved::Mode) {
    labels(a);
    a.plan.b.start_measure(mode, &mut a.ed);
}
fn h_ms_distance(a: &mut EditorApp) {
    start_ms(a, crate::edit3d::measure_saved::Mode::Distance);
}
fn h_ms_clear(a: &mut EditorApp) {
    start_ms(a, crate::edit3d::measure_saved::Mode::Clear);
}
fn h_ms_angle(a: &mut EditorApp) {
    start_ms(a, crate::edit3d::measure_saved::Mode::Angle);
}
fn h_ms_area(a: &mut EditorApp) {
    start_ms(a, crate::edit3d::measure_saved::Mode::Area);
}
fn h_number(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_ann(AnnMode::Number, &mut a.ed);
}
fn h_tag_all(a: &mut EditorApp) {
    if crate::annot_text::in_drafting(&a.ed) {
        a.ed.say(Severity::Refusal, crate::tr("drafting_tools.free.plan_only", &[]));
        return;
    }
    labels(a);
    a.plan.b.tag_all = Some(TagAllDlg::default());
}
fn h_dim_auto(a: &mut EditorApp) {
    if crate::annot_text::in_drafting(&a.ed) {
        a.ed.say(Severity::Refusal, crate::tr("drafting_tools.free.plan_only", &[]));
        return;
    }
    let o = tags::dim_auto(&mut a.ed);
    a.plan.b.ann_out(&mut a.ed, o);
}

fn h_dim(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_dim(DimMode::Linear, &mut a.ed);
}
fn h_dim_angular(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_dim(DimMode::Angular, &mut a.ed);
}
fn h_dim_radial(a: &mut EditorApp) {
    labels(a);
    a.plan.b.start_dim(DimMode::Radial, &mut a.ed);
}
/// Selection commands that have nothing to act on without a selection.
pub const NEEDS_SELECTION: &[&str] = &[
    "mod.hide",
    "mod.isolate",
    "mod.select_filter",
    "mod.select_similar",
    "mod.select_similar_project",
];

/// Reason a selection command is disabled right now, None when it can run.
pub fn select_first_tip(ed: &Editor, cmd: &str, ru: bool) -> Option<String> {
    if !NEEDS_SELECTION.contains(&cmd) || !ed.selection.is_empty() {
        return None;
    }
    // hide and isolate also take the plan grips in focus
    let by_focus = matches!(cmd, "mod.hide" | "mod.isolate") && !ed.grip.focus.is_empty();
    (!by_focus).then(|| crate::tr_lang(ru, "modify_ops.op.select_first", &[]))
}

/// Toasts the refusal and returns true when `cmd` has no selection to work on.
fn refuse_empty(ed: &mut Editor, cmd: &str) -> bool {
    match select_first_tip(ed, cmd, crate::ui_lang_now()) {
        Some(m) => {
            ed.say(Severity::Refusal, m);
            true
        }
        None => false,
    }
}
fn h_hide(a: &mut EditorApp) {
    if refuse_empty(&mut a.ed, "mod.hide") {
        return;
    }
    labels(a);
    let parts = a.ed.grip.focus.clone();
    a.plan.b.vis.hide_parts(&a.ed.selection, &parts);
    a.ed.selection.clear();
    a.ed.grip.focus.clear();
}
fn h_isolate(a: &mut EditorApp) {
    if refuse_empty(&mut a.ed, "mod.isolate") {
        return;
    }
    labels(a);
    let sel = a.ed.selection.clone();
    let parts = a.ed.grip.focus.clone();
    a.plan.b.vis.isolate_parts(&a.ed, &sel, &parts);
}
fn h_reveal(a: &mut EditorApp) {
    a.plan.b.vis.reset();
}
fn h_apply_hide(a: &mut EditorApp) {
    a.plan.b.vis.apply(&a.ed);
}
fn similar(a: &mut EditorApp, project: bool) {
    let Some(&s) = a.ed.selection.first() else {
        a.ed.say(Severity::Refusal, crate::tr("modify_ops.op.nothing_selected", &[]));
        return;
    };
    let v = &a.plan.b.vis;
    let ed = &a.ed;
    let found = selection::similar(ed, s, project, |x| v.visible(ed, x));
    a.ed.selection = found;
}
fn h_similar(a: &mut EditorApp) {
    similar(a, false);
}
fn h_similar_project(a: &mut EditorApp) {
    similar(a, true);
}
fn h_filter(a: &mut EditorApp) {
    if refuse_empty(&mut a.ed, "mod.select_filter") {
        a.plan.b.filter_open = false;
        return;
    }
    labels(a);
    let was = a.plan.b.filter_open;
    a.plan.b.filter_open = !a.ed.selection.is_empty();
    if !was || !a.plan.b.filter_open {
        // a fresh opening ticks the current categories again
        a.plan.b.filter_init = false;
        a.plan.b.filter_keep.clear();
    }
}
fn h_select_all(a: &mut EditorApp) {
    let v = &a.plan.b.vis;
    let ed = &a.ed;
    let all: Vec<Sel> = selection::all(ed)
        .into_iter()
        .filter(|&s| v.visible(ed, s))
        .collect();
    a.ed.selection = all;
}

/// Commands of the modify tools (`shell::REGISTER`).
pub fn register(a: &mut EditorApp) {
    let list: &[(&str, crate::shell::Handler)] = &[
        ("mod.move", h_move),
        ("mod.copy", h_copy),
        ("mod.rotate", h_rotate),
        ("mod.mirror", h_mirror),
        ("mod.mirror_draw", h_mirror_draw),
        ("mod.array", h_array),
        ("mod.offset", h_offset),
        ("mod.trim", h_trim),
        ("mod.extend", h_extend),
        ("mod.split", h_split),
        ("mod.divide", h_divide),
        ("mod.align", h_align),
        ("mod.scale", h_scale),
        ("ann.dim", h_dim),
        ("ann.dim_angular", h_dim_angular),
        ("ann.dim_radial", h_dim_radial),
        ("ann.dim_auto", h_dim_auto),
        ("ann.tag", h_tag),
        ("ann.tag_all", h_tag_all),
        ("ann.number", h_number),
        ("ann.measure_distance", h_ms_distance),
        ("ann.measure_clear", h_ms_clear),
        ("ann.measure_angle", h_ms_angle),
        ("ann.measure_area", h_ms_area),
        ("mod.distribute", h_distribute),
        ("mod.match_type", h_match_type),
        ("mod.hide", h_hide),
        ("mod.isolate", h_isolate),
        ("mod.reveal", h_reveal),
        ("mod.apply_hide", h_apply_hide),
        ("mod.select_similar", h_similar),
        ("mod.select_similar_project", h_similar_project),
        ("mod.select_filter", h_filter),
        ("edit.select_all", h_select_all),
    ];
    for (id, h) in list {
        a.register_command(id, *h);
    }
    labels(a);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ddc_model::WallSpec;

    /// Distribute, match type, tag and number light their command; Esc through `keys` leaves.
    #[test]
    fn lane_tools_report_active_command_until_esc() {
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        for id in ["mod.distribute", "mod.match_type", "ann.tag", "ann.number"] {
            let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
            assert_eq!(a.active_command(), None, "{id}: idle");
            assert!(a.run(id), "{id}");
            assert_eq!(a.active_command(), Some(id));
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                events: vec![esc.clone()],
                ..Default::default()
            };
            let _ = ctx.run(input, |c| {
                egui::CentralPanel::default().show(c, |ui| a.plan.b.keys(ui, &mut a.ed));
            });
            let b = &a.plan.b;
            assert!(b.dist.is_none() && b.mt.is_none() && b.ann.is_none(), "{id}");
            assert_eq!(a.active_command(), None, "{id} after Esc");
        }
    }

    #[test]
    fn refusal_toasts_as_refusal() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut lane = Lane::default();
        let o = ModTool::new(Op::Copy, &[]).enter(&mut ed);
        assert!(matches!(o, Out::Refused(_)), "{o:?}");
        modify::out(&mut ed, o);
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
        // A refusal at start leaves no older tool waiting for a click.
        // a319e363 gave the stair every transform: a rebar set still refuses alone
        ed.selection = vec![Sel::Ext(crate::scene_ids::EXT_REBAR, 1)];
        modify::begin(&mut ed, ModTool::new(Op::Move, &[]));
        lane.start(Op::Rotate, &mut ed);
        assert!(!modify::running(&ed));
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
    }

    #[test]
    fn selection_commands_are_blocked_when_empty_en_ru() {
        let mut ed = Editor::new_in_memory().unwrap();
        for cmd in NEEDS_SELECTION {
            let en = select_first_tip(&ed, cmd, false).expect(cmd);
            let ru = select_first_tip(&ed, cmd, true).expect(cmd);
            assert_eq!(en, "Select elements first");
            let want = "\u{421}\u{43d}\u{430}\u{447}\u{430}\u{43b}\u{430} \
                \u{432}\u{44b}\u{434}\u{435}\u{43b}\u{438}\u{442}\u{435} \
                \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{44b}";
            assert_eq!(ru, want);
        }
        assert!(select_first_tip(&ed, "mod.move", false).is_none());
        ed.selection = vec![Sel::Wall(1)];
        for cmd in NEEDS_SELECTION {
            assert!(select_first_tip(&ed, cmd, false).is_none(), "{cmd}");
            assert!(select_first_tip(&ed, cmd, true).is_none(), "{cmd}");
        }
    }

    #[test]
    fn empty_selection_refuses_with_a_toast() {
        let mut ed = Editor::new_in_memory().unwrap();
        for cmd in NEEDS_SELECTION {
            ed.status = None;
            assert!(refuse_empty(&mut ed, cmd), "{cmd}");
            assert_eq!(ed.status_severity(), Some(Severity::Refusal));
            assert!(ed.status.is_some());
        }
        ed.status = None;
        ed.selection = vec![Sel::Wall(1)];
        for cmd in NEEDS_SELECTION {
            assert!(!refuse_empty(&mut ed, cmd), "{cmd}");
        }
        assert!(ed.status.is_none());
    }

    #[test]
    fn failed_op_toasts_as_error() {
        let mut ed = Editor::new_in_memory().unwrap();
        modify::out(&mut ed, Out::Error("boom".into()));
        assert_eq!(ed.status_severity(), Some(Severity::Error));
        assert_eq!(ed.status.as_deref(), Some("boom"));
    }

    #[test]
    fn no_cyrillic_literal_in_these_files() {
        for (name, src) in [
            ("modify_tools.rs", include_str!("modify_tools.rs")),
            ("clip_tools.rs", include_str!("clip_tools.rs")),
            ("editor.rs", include_str!("editor.rs")),
        ] {
            let code = src.split("#[cfg(test)]").next().unwrap();
            for (i, line) in code.lines().enumerate() {
                let line = line.split(" // ").next().unwrap();
                let is_comment = line.trim_start().starts_with("//");
                let cyr = line.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
                assert!(is_comment || !cyr, "{name}:{}: {line}", i + 1);
            }
        }
    }

    /// Straight walls of the current type on the current level.
    pub fn walls(ed: &mut Editor, segs: &[((f64, f64), (f64, f64))]) {
        let ty = ed.current_type().expect("type").id;
        for &(a, b) in segs {
            ed.model
                .add_wall(&WallSpec {
                    a,
                    b,
                    level: ed.level,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: ty,
                    offset0: None,
                })
                .unwrap();
        }
        ed.reload().unwrap();
    }

    fn ends(ed: &Editor, id: i64) -> (V2, V2) {
        axis(ed, id).unwrap()
    }

    fn near(a: V2, b: V2) -> bool {
        (a - b).len() < 1e-6
    }

    #[test]
    fn panel_refuses_every_transform_and_the_model_stays() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.add_curtains(&[(v2(0.0, 0.0), v2(6.0, 0.0), 0.0)]);
        let cw = ed.curtains[0].id;
        ed.selection = vec![Sel::Panel(cw, 2)];
        let head = ed.model.change_head();
        let steps = ed.undo_len();
        for op in [Op::Move, Op::Copy, Op::Rotate, Op::Mirror, Op::Array] {
            let mut t = ModTool::new(op, &ed.selection);
            assert!(matches!(t.click(&mut ed, v2(0.0, 0.0)), Out::Refused(_)));
            assert_eq!(ed.model.change_head(), head);
            assert_eq!(ed.undo_len(), steps);
        }
    }

    #[test]
    fn panel_in_a_mixed_move_stays_with_its_wall() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.add_curtains(&[(v2(0.0, 5.0), v2(6.0, 5.0), 0.0)]);
        let cw = ed.curtains[0].id;
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let wall = ed.walls[0].id;
        let before = crate::editor::delete::tests::dump(&ed, "curtain_wall");
        ed.selection = vec![Sel::Panel(cw, 2), Sel::Wall(wall)];
        let mut t = ModTool::new(Op::Move, &ed.selection);
        assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
        assert_eq!(t.click(&mut ed, v2(1.0, 2.0)), Out::Exit);
        assert!(near(ends(&ed, wall).0, v2(1.0, 2.0)));
        assert_eq!(
            crate::editor::delete::tests::dump(&ed, "curtain_wall"),
            before
        );
    }

    #[test]
    fn rotating_a_mass_says_why() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut t = ModTool::new(Op::Rotate, &[Sel::Mass(1)]);
        let want = crate::tr("elem.no.mass_turn", &[]);
        assert_eq!(t.click(&mut ed, v2(1.0, 1.0)), Out::Refused(want));
    }

    #[test]
    fn partial_toast_without_panel_has_no_offer() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.add_curtains(&[(v2(0.0, 5.0), v2(6.0, 5.0), 0.0)]);
        let cw = ed.curtains[0].id;
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let wall = ed.walls[0].id;
        let _ = crate::elem::panel_rule::take_offer();
        let t = ModTool::new(Op::Move, &[Sel::Wall(wall)]);
        assert!(t.skipped(&ed).is_empty());
        assert!(!crate::elem::panel_rule::take_offer());
        let t = ModTool::new(Op::Move, &[Sel::Wall(wall), Sel::Panel(cw, 2)]);
        assert!(!t.skipped(&ed).is_empty());
        assert!(crate::elem::panel_rule::take_offer());
    }

    #[test]
    fn refused_start_clears_tool() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.add_curtains(&[(v2(0.0, 0.0), v2(6.0, 0.0), 0.0)]);
        let cw = ed.curtains[0].id;
        let mut lane = Lane::default();
        modify::begin(&mut ed, ModTool::new(Op::Move, &[]));
        ed.selection = vec![Sel::Panel(cw, 2)];
        lane.start(Op::Move, &mut ed);
        assert!(!modify::running(&ed));
    }

    #[test]
    fn partial_keeps_warning() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.add_curtains(&[(v2(0.0, 5.0), v2(6.0, 5.0), 0.0)]);
        let cw = ed.curtains[0].id;
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        ed.selection = vec![Sel::Panel(cw, 2), Sel::Wall(ed.walls[0].id)];
        let mut lane = Lane::default();
        lane.start(Op::Move, &mut ed);
        let t = modify::with(&mut ed, |t, ed| {
            assert!(!t.held_back.is_empty());
            t.say_done(ed);
        });
        assert!(t.is_some(), "partial start keeps a tool");
        assert_eq!(
            ed.status_sev.as_ref().map(|x| x.0),
            Some(Severity::Warning)
        );
    }

    #[test]
    fn move_pick_first_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let id = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(id)];
        let mut t = ModTool::new(Op::Move, &ed.selection);
        assert!(!t.selecting);
        assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
        assert_eq!(t.click(&mut ed, v2(1.0, 2.0)), Out::Exit);
        assert!(near(ends(&ed, id).0, v2(1.0, 2.0)));
        ed.undo();
        assert!(near(ends(&ed, id).0, v2(0.0, 0.0)));
        ed.redo();
        assert!(near(ends(&ed, id).0, v2(1.0, 2.0)));
    }

    #[test]
    fn copy_tool_first_multiple_and_typed_distance() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let mut t = ModTool::new(Op::Copy, &[]);
        assert!(t.selecting && t.step() == "select");
        assert!(matches!(t.enter(&mut ed), Out::Refused(_)));
        t.click(&mut ed, v2(2.0, 0.0));
        assert_eq!(t.targets.len(), 1);
        t.enter(&mut ed);
        t.click(&mut ed, v2(0.0, 0.0));
        assert_eq!(t.click(&mut ed, v2(0.0, 1.0)), Out::Done);
        // input 2000 toward the cursor direction (up) from the base
        t.hover(&ed, v2(0.0, 5.0));
        "2000".chars().for_each(|c| t.char(c));
        assert_eq!(t.enter(&mut ed), Out::Done);
        assert_eq!(ed.walls.len(), 3);
        assert!(ed.walls.iter().any(|w| near(w.a, v2(0.0, 2.0))));
        ed.undo();
        assert_eq!(ed.walls.len(), 2);
    }

    #[test]
    fn move_points_snap_to_wall_ends() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (5.0, 0.0)), ((10.0, 3.0), (15.0, 3.0))]);
        let id = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(id)];
        let mut t = ModTool::new(Op::Move, &ed.selection);
        assert_eq!(t.click_snapped(&mut ed, v2(0.004, 0.004)), Out::Nothing);
        assert_eq!(t.pts[0], v2(0.0, 0.0));
        t.hover(&ed, v2(9.996, 3.004));
        assert_eq!(t.snap().unwrap().kind, crate::snap::SnapKind::Endpoint);
        assert_eq!(t.click_snapped(&mut ed, v2(9.996, 3.004)), Out::Exit);
        let (a, b) = ends(&ed, id);
        assert!(near(a, v2(10.0, 3.0)) && near(b, v2(15.0, 3.0)));
    }

    #[test]
    fn rotate_base_snaps_to_a_midpoint() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((10.0, 0.0), (10.0, 6.0))]);
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let mut t = ModTool::new(Op::Rotate, &ed.selection);
        t.hover(&ed, v2(10.01, 3.02));
        assert_eq!(t.snap().unwrap().kind, crate::snap::SnapKind::Midpoint);
        assert_eq!(t.click_snapped(&mut ed, v2(10.01, 3.02)), Out::Nothing);
        assert_eq!(t.pts[1], v2(10.0, 3.0));
    }

    #[test]
    fn tab_cycles_modify_snaps() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (5.0, 0.0)), ((5.05, 0.0), (8.0, 0.0))]);
        ed.selection = vec![Sel::Wall(ed.walls[0].id)];
        let mut t = ModTool::new(Op::Move, &ed.selection);
        t.hover(&ed, v2(5.02, 0.01));
        let n = t.snaps.len();
        assert!(n > 2);
        let first = t.cursor;
        t.tab();
        assert_eq!(t.snap_idx, 1);
        assert_eq!(t.cursor, t.snaps[1].p);
        assert_ne!(t.cursor, first);
        for _ in 1..n {
            t.tab();
        }
        assert_eq!((t.snap_idx, t.cursor), (0, first));
    }

    #[test]
    fn rotate_default_center_with_rays_and_typed_angle() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let id = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(id)];
        let mut t = ModTool::new(Op::Rotate, &ed.selection);
        t.hover(&ed, v2(3.0, 0.0));
        assert_eq!(t.pts.len(), 1, "frame centre by default");
        assert!((t.pts[0].x - 2.0).abs() < 1e-6);
        assert_eq!(t.click(&mut ed, v2(4.0, 0.0)), Out::Nothing);
        assert_eq!(t.step(), "second");
        "90".chars().for_each(|c| t.char(c));
        assert_eq!(t.enter(&mut ed), Out::Exit);
        let (a, b) = ends(&ed, id);
        assert!((a.x - 2.0).abs() < 1e-6 && (b.x - 2.0).abs() < 1e-6);
        assert!(((a - b).len() - 4.0).abs() < 1e-6);
    }

    #[test]
    fn scale_tool_typed_factor_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let id = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(id)];
        let steps = ed.undo_len();
        let mut t = ModTool::new(Op::Scale, &ed.selection);
        assert!(!t.selecting);
        assert_eq!(t.step(), "first");
        assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
        assert_eq!(t.step(), "second");
        // a zero or negative factor is refused with the reason and nothing changes
        let bad = Out::Error(crate::tr("scl.bad_factor", &[]));
        for text in ["0", "-2"] {
            text.chars().for_each(|c| t.char(c));
            assert_eq!(t.enter(&mut ed), bad);
            assert_eq!(ed.undo_len(), steps);
        }
        assert!(near(ends(&ed, id).1, v2(4.0, 0.0)));
        "2,5".chars().for_each(|c| t.char(c));
        assert_eq!(t.enter(&mut ed), Out::Exit);
        let (a, b) = ends(&ed, id);
        assert!(near(a, v2(0.0, 0.0)) && near(b, v2(10.0, 0.0)), "{a:?} {b:?}");
        assert_eq!(ed.undo_len(), steps + 1, "one action, one undo step");
        ed.undo();
        assert!(near(ends(&ed, id).1, v2(4.0, 0.0)));
        // clicks: base, reference length 2, new length 3 give a factor of 1.5
        let mut t = ModTool::new(Op::Scale, &ed.selection);
        assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
        assert_eq!(
            t.click(&mut ed, v2(0.0, 0.0)),
            Out::Error(crate::tr("scl.zero_ref", &[]))
        );
        assert_eq!(t.click(&mut ed, v2(2.0, 0.0)), Out::Nothing);
        assert_eq!(t.step(), "third");
        t.hover(&ed, v2(0.0, 3.0));
        let ghost = t.xf(&ed).expect("preview");
        assert!(near(ghost(v2(4.0, 0.0)), v2(6.0, 0.0)));
        assert_eq!(t.click(&mut ed, v2(0.0, 3.0)), Out::Exit);
        assert!(near(ends(&ed, id).1, v2(6.0, 0.0)));
        assert_eq!(ed.undo_len(), steps + 1);
        // the copy option keeps the original and adds a scaled copy in one step
        ed.undo();
        let mut t = ModTool::new(Op::Scale, &ed.selection);
        t.opts.copy = true;
        t.click(&mut ed, v2(0.0, 2.0));
        "0.5".chars().for_each(|c| t.char(c));
        assert_eq!(t.enter(&mut ed), Out::Exit);
        assert_eq!(ed.walls.len(), 2);
        assert!(near(ends(&ed, id).1, v2(4.0, 0.0)));
        assert!(ed.walls.iter().any(|w| near(w.a, v2(0.0, 1.0)) && near(w.b, v2(2.0, 1.0))));
        assert_eq!(ed.undo_len(), steps + 1);
    }

    /// Palette contract: the command is registered, has a handler, a label, a description and
    /// every hint in both languages.
    #[test]
    fn scale_command_in_palette() {
        let a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        assert!(a.sh.reg.get("mod.scale").is_some());
        assert!(a.handlers.contains_key("mod.scale"));
        assert_eq!(Op::Scale.command(), "mod.scale");
        for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
            let c = ddc_ui::Catalog::new(lang);
            for k in [
                "cmd.mod.scale",
                "cmd.mod.scale.desc",
                "hint.scale.select",
                "hint.scale.first",
                "hint.scale.second",
                "hint.scale.third",
                "scl.bad_factor",
                "scl.zero_ref",
            ] {
                assert!(c.table(lang).contains_key(k), "{k}");
            }
        }
    }

    #[test]
    fn mirror_draw_copy_and_mirror_pick_axis() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 1.0), (4.0, 1.0)), ((-1.0, 0.0), (5.0, 0.0))],
        );
        let id = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(id)];
        let mut t = ModTool::new(Op::MirrorDraw, &ed.selection);
        assert!(t.opts.copy);
        t.click(&mut ed, v2(0.0, 3.0));
        assert_eq!(t.click(&mut ed, v2(4.0, 3.0)), Out::Exit);
        assert!(ed
            .walls
            .iter()
            .any(|w| near(w.a, v2(0.0, 5.0)) || near(w.b, v2(0.0, 5.0))));
        let mut t = ModTool::new(Op::Mirror, &[Sel::Wall(id)]);
        t.opts.copy = false;
        assert_eq!(t.click(&mut ed, v2(4.5, 0.0)), Out::Exit);
        let (a, _) = ends(&ed, id);
        assert!((a.y + 1.0).abs() < 1e-6);
    }

    #[test]
    fn array_linear_and_radial_counter() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (0.0, 2.0))]);
        let id = ed.walls[0].id;
        let mut t = ModTool::new(Op::Array, &[Sel::Wall(id)]);
        "5".chars().for_each(|c| t.char(c));
        t.enter(&mut ed);
        assert_eq!(t.opts.count, 5);
        t.opts.to_last = true;
        t.click(&mut ed, v2(0.0, 0.0));
        t.hover(&ed, v2(8.0, 0.0));
        assert_eq!(t.array_xfs().len(), 4);
        assert_eq!(t.click(&mut ed, v2(8.0, 0.0)), Out::Exit);
        assert_eq!(ed.walls.len(), 5);
        assert!(ed.walls.iter().any(|w| near(w.a, v2(2.0, 0.0))));
        ed.undo();
        assert_eq!(ed.walls.len(), 1);
        let mut t = ModTool::new(Op::Array, &[Sel::Wall(id)]);
        t.opts.radial = true;
        t.opts.count = 4;
        assert_eq!(t.step(), "center");
        t.click(&mut ed, v2(0.0, -1.0));
        t.click(&mut ed, v2(1.0, -1.0));
        assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Exit);
        assert_eq!(ed.walls.len(), 4);
    }

    #[test]
    fn tr4_parallel_message() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))]);
        let mut t = ModTool::new(Op::Extend, &[]);
        t.click(&mut ed, v2(2.0, 0.0));
        assert!(matches!(t.click(&mut ed, v2(2.0, 2.0)), Out::Error(_)));
        assert_eq!(t.step(), "target");
    }

    #[test]
    fn offset_trim_extend_split_align() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((5.0, 1.0), (5.0, 4.0))],
        );
        let (w1, w2) = (ed.walls[0].id, ed.walls[1].id);
        // offset: 1 m to the left (up)
        let mut t = ModTool::new(Op::Offset, &[]);
        t.opts.dist = 1.0;
        assert_eq!(t.click(&mut ed, v2(2.0, 0.05)), Out::Done);
        assert!(ed.walls.iter().any(|w| near(w.a, v2(0.0, 1.0))));
        ed.undo();
        // trim to corner: the ends meet at (5, 0)
        let mut t = ModTool::new(Op::Trim, &[]);
        assert_eq!(t.step(), "first");
        t.click(&mut ed, v2(2.0, 0.0));
        assert_eq!(t.click(&mut ed, v2(5.0, 2.0)), Out::Done);
        assert!(near(ends(&ed, w1).1, v2(5.0, 0.0)));
        assert!(near(ends(&ed, w2).0, v2(5.0, 0.0)));
        ed.undo();
        // extend to boundary
        let mut t = ModTool::new(Op::Extend, &[]);
        t.click(&mut ed, v2(5.0, 2.0));
        assert_eq!(t.step(), "target");
        assert_eq!(t.click(&mut ed, v2(2.0, 0.0)), Out::Done);
        assert!(near(ends(&ed, w1).1, v2(5.0, 0.0)));
        ed.undo();
        // split at distance 1000 from the nearer end
        let mut t = ModTool::new(Op::Split, &[]);
        "1000".chars().for_each(|c| t.char(c));
        assert_eq!(t.click(&mut ed, v2(0.5, 0.0)), Out::Done);
        assert_eq!(ed.walls.len(), 3);
        assert!(near(ends(&ed, w1).1, v2(1.0, 0.0)));
        ed.undo();
        assert_eq!(ed.walls.len(), 2);
        // split into 4 equal parts: the number is the count, a click is the wall, one undo step
        let mut t = ModTool::new(Op::Split, &[]);
        t.opts.parts = 2;
        assert_eq!(t.step(), "parts");
        "4".chars().for_each(|c| t.char(c));
        assert_eq!(t.enter(&mut ed), Out::Nothing);
        assert_eq!(t.opts.parts, 4);
        "1".chars().for_each(|c| t.char(c));
        assert!(matches!(t.enter(&mut ed), Out::Refused(_)));
        assert_eq!(t.opts.parts, 4);
        assert_eq!(t.click(&mut ed, v2(0.5, 0.0)), Out::Done);
        assert_eq!(ed.walls.len(), 5);
        ed.undo();
        assert_eq!(ed.walls.len(), 2);
        // align: wall 2 to the axis x = 6 (the reference is a new parallel wall)
        walls(&mut ed, &[((6.0, 0.0), (6.0, 5.0))]);
        let mut t = ModTool::new(Op::Align, &[]);
        t.click(&mut ed, v2(6.0, 2.5));
        assert_eq!(t.click(&mut ed, v2(5.0, 2.0)), Out::Done);
        assert!((ends(&ed, w2).0.x - 6.0).abs() < 1e-6);
    }

    #[test]
    fn escape_twice_and_model_error_keeps_state() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let id = ed.walls[0].id;
        let mut t = ModTool::new(Op::Move, &[Sel::Wall(id)]);
        t.click(&mut ed, v2(0.0, 0.0));
        assert_eq!(t.escape(), Out::Nothing);
        assert!(t.pts.is_empty());
        assert_eq!(t.escape(), Out::Exit);
        // does mirroring over the wall itself give an empty/degenerate result? the axis has zero length
        let mut t = ModTool::new(Op::MirrorDraw, &[Sel::Wall(id)]);
        t.click(&mut ed, v2(1.0, 1.0));
        assert!(matches!(t.click(&mut ed, v2(1.0, 1.0)), Out::Refused(_)));
        assert!(!ed.can_redo());
        // orthogonal constraint
        let mut t = ModTool::new(Op::Move, &[Sel::Wall(id)]);
        t.opts.constrain = true;
        t.click(&mut ed, v2(0.0, 0.0));
        t.hover(&ed, v2(3.0, 0.4));
        assert!(near(t.cursor, v2(3.0, 0.0)));
    }

    /// An 8x5 room with a partition, a door and a window.
    fn scene(ed: &mut Editor) {
        walls(
            ed,
            &[
                ((0.0, 0.0), (8.0, 0.0)),
                ((8.0, 0.0), (8.0, 5.0)),
                ((8.0, 5.0), (0.0, 5.0)),
                ((0.0, 5.0), (0.0, 0.0)),
                ((4.0, 0.0), (4.0, 5.0)),
            ],
        );
        let (s, n) = (ed.walls[0].id, ed.walls[2].id);
        let op = |along: f64, w: f64, h: f64, sill: f64, kind: &str| ddc_model::OpeningSpec {
            along,
            width: w,
            sill,
            height: h,
            kind: kind.into(),
        };
        ed.model
            .add_opening(s, &op(1.2, 0.9, 2.1, 0.0, "door"))
            .unwrap();
        ed.model
            .add_opening(n, &op(2.0, 1.5, 1.5, 0.9, "window"))
            .unwrap();
        ed.reload().unwrap();
    }

    fn render(
        rs: &crate::RenderState,
        app: &mut EditorApp,
        size: (u32, u32),
    ) -> Vec<u8> {
        let ctx = egui::Context::default();
        crate::shot::frame(rs, &ctx, size, 1.0, 8, None, |c, _| app.ui(c, Some(rs)))
    }

    /// World to plan screen by the last frame rectangle.
    fn scr(app: &EditorApp, p: V2) -> Pos2 {
        let (c, pv) = (app.plan.b.rect.unwrap().center(), &app.plan);
        Pos2::new(
            c.x + ((p.x - pv.center.x) * pv.scale) as f32,
            c.y - ((p.y - pv.center.y) * pv.scale) as f32,
        )
    }

    /// Acceptance snapshots B1-B6 (light+dark) in docs/design/shots/tools/.
    /// `cargo test -p ddc-app shots_b -- --ignored`
    #[test]
    #[ignore]
    fn shots_b() {
        let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/shots/tools");
        std::fs::create_dir_all(&dir).unwrap();
        let size = (1440u32, 900u32);
        for theme in ["light", "dark"] {
            for id in ["B1", "B2", "B3", "B4", "B5", "B6"] {
                let mut ed = Editor::new_in_memory().unwrap();
                scene(&mut ed);
                ed.status = None;
                ed.set_mode(crate::wall_tool::Mode::Select);
                let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
                app.prefs.theme = theme.into();
                app.plan.fit(&app.ed);
                render(&rs, &mut app, size); // layout: the plan rectangle
                let w = |app: &EditorApp, i: usize| app.ed.walls[i].id;
                let px = match id {
                    "B1" => {
                        // left: a window (fully inside only), right: a crossing (touch)
                        let mut halves = vec![];
                        for (a, b) in [(v2(-0.6, -0.6), v2(4.4, 5.6)), (v2(8.6, 2.4), v2(3.6, 5.6))]
                        {
                            let (sa, sb) = (scr(&app, a), scr(&app, b));
                            app.plan.b.drag = Some((sa, a, sb, b));
                            halves.push(render(&rs, &mut app, size));
                        }
                        let row = size.0 as usize * 4;
                        let mut out = halves[0].clone();
                        for y in 0..size.1 as usize {
                            let r = y * row;
                            out[r + row / 2..r + row]
                                .copy_from_slice(&halves[1][r + row / 2..r + row]);
                        }
                        out
                    }
                    "B2" => {
                        let at = v2(1.65, 0.0);
                        let ed = &app.ed;
                        app.plan.b.cycle.next(ed, at, 0.1, |_| true);
                        let c = app
                            .plan
                            .b
                            .cycle
                            .next(ed, at, 0.1, |_| true)
                            .unwrap()
                            .to_vec();
                        app.ed.selection = c;
                        render(&rs, &mut app, size)
                    }
                    "B3" => {
                        app.ed.selection = vec![Sel::Wall(w(&app, 4))];
                        h_rotate(&mut app);
                        let t = modify::tool_mut(&mut app.ed).unwrap();
                        t.pts = vec![v2(4.0, 2.5), v2(4.0, 5.0)];
                        t.cursor = v2(2.4, 4.4);
                        render(&rs, &mut app, size)
                    }
                    "B4" => {
                        app.ed.selection = vec![Sel::Wall(w(&app, 4))];
                        h_array(&mut app);
                        let t = modify::tool_mut(&mut app.ed).unwrap();
                        t.opts.count = 4;
                        t.opts.to_last = true;
                        t.pts = vec![v2(4.0, 2.5)];
                        t.cursor = v2(7.0, 2.5);
                        render(&rs, &mut app, size)
                    }
                    "B5" => {
                        app.ed.selection = vec![Sel::Wall(w(&app, 0)), Sel::Wall(w(&app, 4))];
                        h_isolate(&mut app);
                        app.ed.selection.clear();
                        render(&rs, &mut app, size)
                    }
                    _ => {
                        h_dim(&mut app);
                        let d = app.plan.b.dim.as_mut().unwrap();
                        d.attach = dims::Attach::Axis;
                        for x in [0.0, 4.0, 8.0] {
                            let o = d.click(&mut app.ed, v2(x, 0.4), 0.1);
                            assert!(matches!(o, DimOut::Nothing));
                        }
                        assert!(matches!(
                            d.click(&mut app.ed, v2(4.0, -1.0), 0.1),
                            DimOut::Done(2)
                        ));
                        // a second chain in progress: preview through the cursor
                        for y in [0.0, 5.0] {
                            d.click(
                                &mut app.ed,
                                v2(8.0, y + if y > 0.0 { -0.4 } else { 0.4 }),
                                0.1,
                            );
                        }
                        d.cursor = v2(9.2, 2.5);
                        app.ed.status = None;
                        render(&rs, &mut app, size)
                    }
                };
                let out = dir.join(format!("{id}-{theme}.png"));
                crate::shot::save_png(&out, size, &px).unwrap();
            }
        }
    }

    /// Snapshots of distribution, tags and auto dimensions (light+dark) in docs/design/shots/annot/.
    /// `cargo test -p ddc-app shots_annot -- --ignored`
    #[test]
    #[ignore]
    fn shots_annot() {
        let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/shots/annot");
        std::fs::create_dir_all(&dir).unwrap();
        let size = (1440u32, 900u32);
        for theme in ["light", "dark"] {
            for id in ["A1", "A2", "A3", "A4"] {
                let mut ed = Editor::new_in_memory().unwrap();
                scene(&mut ed);
                let s = ed.walls[0].id;
                // a window on the south wall: a distribution slot on it is a conflict
                ed.model
                    .add_opening(
                        s,
                        &ddc_model::OpeningSpec {
                            along: 5.4,
                            width: 1.2,
                            sill: 0.9,
                            height: 1.5,
                            kind: "window".into(),
                        },
                    )
                    .unwrap();
                ed.model
                    .add_room(&ddc_model::RoomSpec {
                        level: ed.level,
                        name: "\u{41a}\u{430}\u{431}\u{438}\u{43d}\u{435}\u{442}".into(),
                        number: "101".into(),
                        point: (2.0, 2.5),
                        height: 3.0,
                    })
                    .unwrap();
                ed.reload().unwrap();
                ed.status = None;
                ed.set_mode(crate::wall_tool::Mode::Select);
                let door = ed
                    .openings
                    .iter()
                    .find(|o| o.kind == crate::opening_tool::Kind::Door)
                    .unwrap()
                    .id;
                let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
                app.prefs.theme = theme.into();
                app.plan.fit(&app.ed);
                render(&rs, &mut app, size);
                let px = match id {
                    "A1" => {
                        app.ed.selection = vec![Sel::Opening(door)];
                        h_distribute(&mut app);
                        let d = app.plan.b.dist.as_mut().unwrap();
                        d.margin = 0.4;
                        d.input = "4".into();
                        render(&rs, &mut app, size)
                    }
                    "A2" => {
                        app.ed.selection = vec![Sel::Opening(door)];
                        h_distribute(&mut app);
                        let d = app.plan.b.dist.as_mut().unwrap();
                        d.n = 3;
                        d.margin = 0.4;
                        let o = d.commit(&mut app.ed);
                        assert!(matches!(o, DistOut::Done(_)));
                        app.plan.b.dist = None;
                        let lv = app.ed.level;
                        let cats = [
                            ddc_model::TagCat::Door,
                            ddc_model::TagCat::Window,
                            ddc_model::TagCat::Room,
                        ];
                        app.ed.model.tag_all(lv, &cats, true).unwrap();
                        app.ed.reload().unwrap();
                        h_number(&mut app);
                        let a = app.plan.b.ann.as_mut().unwrap();
                        a.next = "\u{414}1".into();
                        let doors: Vec<V2> = {
                            let ed = &app.ed;
                            let w = &ed.walls[0];
                            let mut v: Vec<_> = ed
                                .openings
                                .iter()
                                .filter(|o| {
                                    o.wall == w.id && o.kind == crate::opening_tool::Kind::Door
                                })
                                .map(|o| v2(o.along + o.width / 2.0, 0.0))
                                .collect();
                            v.sort_by(|a, b| a.x.total_cmp(&b.x));
                            v
                        };
                        for p in &doors[..2] {
                            let o = a.click(&mut app.ed, *p, 0.1);
                            assert!(matches!(o, AnnOut::Done(_)));
                        }
                        a.cursor = doors[2];
                        app.ed.selection.clear();
                        app.ed.status = None;
                        render(&rs, &mut app, size)
                    }
                    "A3" => {
                        h_tag_all(&mut app);
                        render(&rs, &mut app, size);
                        render(&rs, &mut app, size)
                    }
                    _ => {
                        app.ed
                            .model
                            .add_grid(&ddc_model::GridSpec::line("A", (2.0, -3.0), (2.0, 7.0)))
                            .unwrap();
                        app.ed.reload().unwrap();
                        app.ed.selection = vec![Sel::Wall(s)];
                        h_dim_auto(&mut app);
                        app.ed.selection.clear();
                        app.ed.status = None;
                        render(&rs, &mut app, size)
                    }
                };
                let out = dir.join(format!("{id}-{theme}.png"));
                crate::shot::save_png(&out, size, &px).unwrap();
            }
        }
    }
}
