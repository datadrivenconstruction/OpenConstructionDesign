//! The modify tools as one session: `ModTool` (move, copy, rotate, mirror, array,
//! scale, offset, trim, extend, split, align) owns the input under `Scope::Moving`, in any view
//! that gives a plan point. Pick-first and tool-first are the same session: with a selection
//! the `select` step is skipped. Bridge: `ModTool` commits through the editor itself.
use crate::editor::Editor;
use crate::interact::{Btn, Cx, End, Hint, Input, Interaction, Key, Preview, Scope, Step, ToolId, ToolSession};
use crate::modify_tools::{gate_for, ModTool, Op, Out};
use crate::status::Severity;
use crate::tools::Rec;
use ddc_model::refusal::{Refusal, RefusalCode};

/// Pick radius of the tool, screen pixels (the plan's `8 / scale`).
const PICK_PX: f64 = 8.0;

pub struct ModifySession {
    pub t: ModTool,
}

impl ModifySession {
    /// Feed one input to the tool; its outcome as a session step.
    fn feed(&mut self, i: &Input, ed: &mut Editor) -> Step {
        let t = &mut self.t;
        if let Some(p) = i.pointer() {
            if p.px_per_m.is_finite() && p.px_per_m > 0.0 {
                t.r = PICK_PX / p.px_per_m;
            }
        }
        let o = match *i {
            Input::Move(p) => {
                if let Some(q) = p.plan() {
                    t.hover(ed, q);
                }
                Out::Nothing
            }
            Input::Click(p, Btn::Left) => match p.plan() {
                Some(q) => t.click_snapped(ed, q),
                // a ray along the work plane gives no point: say so instead of dropping the click
                None => return Step::Refuse(Refusal::new(RefusalCode::Blocked, "ix.refuse.no_plan_point")),
            },
            Input::Key(Key::Escape, _) => t.escape(),
            Input::Key(Key::Enter, _) => t.enter(ed),
            Input::Key(Key::Backspace, _) => {
                t.input.pop();
                Out::Nothing
            }
            Input::Key(Key::Tab, _) => {
                t.tab();
                Out::Nothing
            }
            Input::Char(c) => {
                // a view sends Space as a character: it groups digits or re-picks the rotate centre
                t.char(c);
                if c == ' ' {
                    t.space();
                }
                Out::Nothing
            }
            _ => Out::Nothing,
        };
        match o {
            Out::Exit => Step::Done,
            Out::Refused(m) => {
                ed.say(Severity::Refusal, m);
                Step::Continue
            }
            Out::Error(m) => {
                ed.say(Severity::Error, m);
                Step::Continue
            }
            Out::Nothing | Out::Done => Step::Continue,
        }
    }
}

impl ToolSession for ModifySession {
    fn on_input_ed(&mut self, i: &Input, ed: &mut Editor, _now_ms: u64) -> Step {
        self.feed(i, ed)
    }
    fn id(&self) -> ToolId {
        ToolId(self.t.op.command())
    }
    fn step(&self) -> &'static str {
        self.t.step()
    }
    fn hints(&self) -> &[Hint] {
        &[]
    }
    fn on_input(&mut self, _i: &Input, _cx: &Cx) -> Step {
        // never called: `on_input_ed` takes every input
        Step::Continue
    }
    fn preview(&self, _cx: &Cx, _out: &mut Preview) {}
    fn commit(&mut self, _cx: &Cx) -> Result<Rec, Refusal> {
        Err(Refusal::new(RefusalCode::Blocked, "ix.refuse.bridge"))
    }
    fn scope(&self) -> Scope {
        Scope::Moving { op: self.t.op }
    }
}

/// Start `op` on the selection (the targets the rule gate lets through); a refusal is said and
/// nothing starts. True when the session runs.
pub fn start(ed: &mut Editor, op: Op) -> bool {
    stop(ed);
    let targets = crate::annot_text::plan_targets(ed);
    let mut run = targets.clone();
    let mut held = String::new();
    if crate::elem::caps::xf_op(op).is_some() {
        use crate::elem::caps::Gate;
        match gate_for(op, &targets) {
            Gate::Refused { why, kinds } => {
                let k = crate::elem::panel_rule::kinds(ed, &kinds);
                ed.refuse(crate::refusals::xform_not_allowed(why, &k));
                return false;
            }
            Gate::Partial { allowed, skipped, .. } => {
                let kinds = crate::elem::panel_rule::kinds(ed, &skipped);
                ed.say(Severity::Warning, crate::tr("mt.partial", &[("op", op.key()), ("kinds", kinds.as_str())]));
                run = allowed;
                held = kinds;
            }
            Gate::All => {}
        }
    }
    let mut t = ModTool::new(op, &run);
    t.held_back = held;
    t.free = crate::annot_text::in_drafting(ed);
    begin(ed, t);
    true
}

/// Run `t` as the session (tests and the drafting tab build their own tool).
pub fn begin(ed: &mut Editor, t: ModTool) {
    ed.ix.begin(Box::new(ModifySession { t }));
}

/// The running modify tool.
pub fn tool(ed: &Editor) -> Option<&ModTool> {
    ed.ix.get::<ModifySession>().map(|s| &s.t)
}

pub fn tool_mut(ed: &mut Editor) -> Option<&mut ModTool> {
    ed.ix.get_mut::<ModifySession>().map(|s| &mut s.t)
}

pub fn running(ed: &Editor) -> bool {
    tool(ed).is_some()
}

/// `f` on the running tool with the editor free to borrow.
pub fn with<R>(ed: &mut Editor, f: impl FnOnce(&mut ModTool, &mut Editor) -> R) -> Option<R> {
    Interaction::with::<ModifySession, R>(ed, |s, ed| f(&mut s.t, ed))
}

/// Outcome of a call made on the tool outside the input route (option bar, drafting tab).
pub fn out(ed: &mut Editor, o: Out) {
    match o {
        Out::Exit => stop(ed),
        Out::Refused(m) => ed.say(Severity::Refusal, m),
        Out::Error(m) => ed.say(Severity::Error, m),
        Out::Nothing | Out::Done => {}
    }
}

pub fn stop(ed: &mut Editor) {
    if running(ed) {
        ed.ix.end(End::Cancel);
    }
}
