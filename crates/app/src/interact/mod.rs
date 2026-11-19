// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Interaction spine: one answer to "what is happening now".
//! scope — `Scope`, `ToolId`, `Target` · input — abstract `Input` · session — `ToolSession`,
//! `Cx`, `Step` · preview — `Prim`s in model coordinates, drawn by every view · hotset / overlay — pure functions of the scope (`SCOPE_TABLE`) ·
//! sessions — the tool groups and `select`, the input with no session · script — event
//! scripts: parse, run without a window, record · pointer — `Pointer`, the cursor in model
//! coordinates · adapter — `ViewAdapter`: plan, 3D and section turn the screen into a `Pointer` ·
//! parity — the same script through every view leaves the same journal.
//! No UI toolkit here: the module is driven by `Input` and read through `Interaction::scope`.

pub mod adapter;
pub mod hints;
pub mod host;
pub mod hotset;
pub mod input;
pub mod overlay;
pub mod parity;
pub mod pointer;
pub mod preview;
pub mod scope;
pub mod script;
pub mod session;
pub mod sessions;
#[cfg(test)]
mod tests;

pub use adapter::{PlanAdapter, SectionAdapter, View3dAdapter, ViewAdapter};
pub use hotset::{HotSet, SCOPE_TABLE};
pub use input::{Btn, Input, Key, Mods, P};
pub use overlay::OverlaySet;
pub use parity::ViewMask;
pub use pointer::{Feat, HostHit, Pointer, Ray3, ViewKind, WorkPlane};
pub use scope::{ModOp, Scope, ScopeKind, Target, ToolId};
pub use hints::{Chord, Hint, HintValue};
pub use preview::{Draw, Glyph, Preview, Prim, Style};
pub use session::{Cx, Step, ToolSession};

use crate::editor::Editor;
use crate::status::Severity;
use crate::wall_tool::WallDraft;

/// Why a session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The session said it is finished.
    Done,
    /// The session or the user cancelled it.
    Cancel,
    /// The second Esc in a row.
    Esc,
    /// Another session began.
    Replaced,
}

/// Owner of the scope: `Editor.ix`. The scope changes only through `begin`, `update`, `end`
/// and `end_if`; outside this module it is read-only.
#[derive(Default)]
pub struct Interaction {
    scope: Scope,
    session: Option<Box<dyn ToolSession + Send>>,
    /// The session `session` was begun inside (`begin_child`): its end returns here. Depth ≤ 2.
    parent: Option<Box<dyn ToolSession + Send>>,
    /// Esc presses in a row (pointer motion and time do not break the run).
    esc_run: u8,
    /// Time of the last `Tick`, ms: sessions see time only through it.
    now_ms: u64,
    /// Refusal shown for the last input (scripts and the AI read it).
    last_refusal: Option<ddc_model::refusal::Refusal>,
    /// Refusal with fixes the card offers until the next click or key (pointer motion keeps it).
    offer: Option<ddc_model::refusal::Refusal>,
    /// Inputs since `script::record_start`, with the tool active then.
    rec: Option<(Option<ToolId>, Vec<Input>)>,
    /// `draws` of the session taken out for its input (`drive`, `with`); a disarm inside clears it.
    held: Option<bool>,
    /// The draft of the session taken out for its input, while it runs (`drive`, `with`).
    parked: Option<WallDraft>,
    /// The draft of a tool armed under a session without one (`arch::route_live` in 3D).
    routed: WallDraft,
}

impl Interaction {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// A drawing session holds the plan input (`ToolSession::draws`), the one at its input too.
    pub fn draws(&self) -> bool {
        match &self.session {
            Some(s) => s.draws(),
            None => self.held.unwrap_or(false),
        }
    }

    /// The session at its input disarmed (`Editor::set_mode`): it no longer draws.
    pub(crate) fn disarm(&mut self) {
        if self.held.is_some() {
            self.held = Some(false);
        }
        self.routed = WallDraft::default();
    }

    /// The draft of the drawing session (`ToolSession::draft_mut`), the one at its input too;
    /// else the routed tool's. No session draws: empty.
    pub fn draft(&self) -> &WallDraft {
        match &self.session {
            Some(s) => s.draft().unwrap_or(&self.routed),
            None => self.parked.as_ref().unwrap_or(&self.routed),
        }
    }

    pub fn draft_mut(&mut self) -> &mut WallDraft {
        match &mut self.session {
            Some(s) => match s.draft_mut() {
                Some(d) => d,
                None => &mut self.routed,
            },
            None => self.parked.as_mut().unwrap_or(&mut self.routed),
        }
    }

    /// Lend the draft of `s` (taken out for its input) to the editor; `unpark` gives it back.
    fn park(&mut self, s: &mut (dyn ToolSession + Send)) -> Option<WallDraft> {
        std::mem::replace(&mut self.parked, s.draft_mut().map(std::mem::take))
    }

    fn unpark(&mut self, s: &mut (dyn ToolSession + Send), outer: Option<WallDraft>) {
        let p = std::mem::replace(&mut self.parked, outer);
        if let (Some(d), Some(p)) = (s.draft_mut(), p) {
            *d = p;
        }
    }

    /// Refusal whose fixes are on offer now.
    pub fn offer(&self) -> Option<&ddc_model::refusal::Refusal> {
        self.offer.as_ref()
    }

    pub fn dismiss_offer(&mut self) {
        self.offer = None;
    }

    /// Id of the running session, if a new-style one owns the input.
    pub fn session_id(&self) -> Option<ToolId> {
        self.session.as_ref().map(|s| s.id())
    }

    /// The running session as `T`.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.session.as_deref().and_then(|s| s.as_any().downcast_ref::<T>())
    }

    /// The running session as `T`, mutable (option bars and dialogs of the view).
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.session.as_deref_mut().and_then(|s| s.as_any_mut().downcast_mut::<T>())
    }

    /// Run `f` on the session `T` with the editor free to borrow (per-frame work of a view:
    /// the trial pulse); the scope follows the session after it.
    pub fn with<T: 'static, R>(ed: &mut Editor, f: impl FnOnce(&mut T, &mut Editor) -> R) -> Option<R> {
        let mut s = ed.ix.session.take()?;
        let held = ed.ix.held.replace(s.draws());
        let outer = ed.ix.park(&mut *s);
        let r = (*s).as_any_mut().downcast_mut::<T>().map(|t| f(t, ed));
        ed.ix.unpark(&mut *s, outer);
        ed.ix.held = held;
        if ed.ix.session.is_none() {
            ed.ix.scope = s.scope();
            ed.ix.session = Some(s);
        }
        r
    }

    /// Start `s`; a running session ends with `End::Replaced`.
    pub fn begin(&mut self, s: Box<dyn ToolSession + Send>) {
        // a new top-level session replaces the nested pair as a whole
        self.parent = None;
        if self.session.is_some() {
            self.end(End::Replaced);
        }
        self.scope = s.scope();
        self.session = Some(s);
        self.esc_run = 0;
    }

    /// Start `s` inside the running session (a sketch inside the tool that opened it); its end
    /// returns to that session through `ToolSession::on_child`. A running child is replaced.
    pub fn begin_child(&mut self, s: Box<dyn ToolSession + Send>) {
        match (self.parent.is_some(), self.session.take()) {
            (false, Some(p)) => self.parent = Some(p),
            // already nested: the old child goes, the parent stays (depth ≤ 2)
            (true, Some(_)) | (_, None) => {}
        }
        self.scope = s.scope();
        self.session = Some(s);
        self.esc_run = 0;
    }

    /// Drop the session: back to its parent if it was nested, else to `Idle`.
    pub fn end(&mut self, why: End) {
        self.session = None;
        self.scope = Scope::Idle;
        self.esc_run = 0;
        if let Some(mut p) = self.parent.take() {
            p.on_child(why);
            self.scope = p.scope();
            self.session = Some(p);
        }
    }

    /// Depth of the running sessions: 0, 1, or 2 when one is nested.
    pub fn depth(&self) -> usize {
        usize::from(self.session.is_some()) + usize::from(self.parent.is_some())
    }

    /// Take the scope the running session reports now (a bridge whose view changed it outside
    /// input); a new scope starts a new Esc run.
    pub(crate) fn follow(&mut self) {
        if let Some(s) = &self.session {
            let n = s.scope();
            if n != self.scope {
                self.esc_run = 0;
            }
            self.scope = n;
        }
    }

    /// `end(Cancel)` when the scope matches `f` (a view switch, a model reload, a level change).
    pub fn end_if(&mut self, f: impl Fn(&Scope) -> bool) {
        if f(&self.scope) {
            self.end(End::Cancel);
        }
    }

    /// Route one input: to the session if one runs, else to selection (`sessions::select`); then
    /// recompute the scope. The second Esc in a row always reaches `Idle`.
    pub fn update(ed: &mut Editor, i: Input) {
        ed.ix.last_refusal = None;
        if !matches!(i, Input::Move(_) | Input::Tick(_) | Input::Mods(_)) {
            ed.ix.offer = None;
        }
        if let Some((_, v)) = &mut ed.ix.rec {
            v.push(i);
        }
        // the right button finishes the chain in progress, as Enter does; with none it is the
        // view's context menu and never reaches the tools
        let i = match i {
            Input::Click(_, Btn::Right) if !ed.chain().is_empty() => Input::Key(Key::Enter, Mods::NONE),
            i => i,
        };
        if let Input::Tick(ms) = i {
            ed.ix.now_ms = ms;
            // the snap dwell (a rest on a point captures it for guides) runs on this clock
            ed.tool.tick(ms);
        }
        let esc = matches!(i, Input::Key(Key::Escape, _));
        if esc {
            ed.ix.esc_run = ed.ix.esc_run.saturating_add(1);
        } else if !matches!(i, Input::Move(_) | Input::Tick(_) | Input::Mods(_)) {
            ed.ix.esc_run = 0;
        }
        let run = ed.ix.esc_run;
        // arming begins the bridge (`refresh`); this catches flags written outside a command
        sessions::special::adopt(ed);
        sessions::arch::adopt(ed);
        if let Some(s) = ed.ix.session.take() {
            Self::drive(ed, s, &i);
        } else {
            if let Some(e) = i.event() {
                sessions::select::route(ed, e);
            }
            ed.ix.scope = Scope::from_legacy(ed);
        }
        if esc && ed.ix.session.is_none() {
            // an Esc that left its tool still counts: the begin/end it caused do not restart the run
            ed.ix.esc_run = run;
        }
        if ed.ix.esc_run >= 2 && ed.ix.scope != Scope::Idle {
            // the old flags only when no session owns the scope; a grip bridge drops its drag
            // (the architecture and special-mode bridges keep their tools in those flags)
            // (as does a model tool: its drawing mode is the wall tool's flag)
            if ed.ix.session.is_none()
                || sessions::arch::running(ed)
                || sessions::special::running(ed)
                || sessions::model_tool::running(ed)
            {
                sessions::select::reset(ed);
            } else if sessions::grip::running(ed) {
                sessions::grip::drop_flags(ed);
            }
            ed.ix.end(End::Esc);
        }
    }

    /// Recompute the scope from the editor flags after a change outside `update` (ribbon command).
    /// A new scope starts a new Esc run: an Esc before the command does not count toward its reset.
    pub fn refresh(ed: &mut Editor) {
        if let Some(s) = sessions::arch::refresh(ed).or_else(|| sessions::special::refresh(ed)) {
            if s != ed.ix.scope {
                ed.ix.esc_run = 0;
            }
            ed.ix.scope = s;
        }
        // a tool armed by a command or `set_mode` is its session from the arm, not the first input
        sessions::special::adopt(ed);
        sessions::arch::adopt(ed);
        if ed.ix.session.is_none() {
            let s = Scope::from_legacy(ed);
            if s != ed.ix.scope {
                ed.ix.esc_run = 0;
            }
            ed.ix.scope = s;
        }
    }

    fn drive(ed: &mut Editor, mut s: Box<dyn ToolSession + Send>, i: &Input) {
        let now_ms = ed.ix.now_ms;
        let held = ed.ix.held.replace(s.draws());
        let outer = ed.ix.park(&mut *s);
        let step = s.on_input_ed(i, ed, now_ms);
        ed.ix.unpark(&mut *s, outer);
        ed.ix.held = held;
        if ed.ix.session.is_some() {
            // the input began another session (a bridge fell through to selection)
            return;
        }
        let end = match step {
            Step::Continue => None,
            Step::Commit => {
                match s.commit(&Cx { ed: &*ed, now_ms }) {
                    Ok(rec) => {
                        if let Err(e) = ed.apply_ext(rec) {
                            ed.say(Severity::Error, e.to_string());
                        }
                    }
                    Err(r) => refuse(ed, &r),
                }
                None
            }
            Step::Refuse(r) => {
                refuse(ed, &r);
                None
            }
            Step::Done => Some(End::Done),
            Step::Cancel => Some(End::Cancel),
        };
        match end {
            None => {
                ed.ix.scope = s.scope();
                ed.ix.session = Some(s);
            }
            Some(why) => ed.ix.end(why),
        }
    }
}

impl Interaction {
    /// Preview of the running session in model coordinates; empty when none runs.
    pub fn preview(ed: &Editor) -> Preview {
        let mut out = Preview::default();
        if let Some(s) = &ed.ix.session {
            s.preview(&Cx { ed, now_ms: ed.ix.now_ms }, &mut out);
        }
        out
    }
}

/// Show a refusal in the status line (the AI reads the same value from the session).
/// Code catalogue (`refusals`).
pub(crate) fn refuse(ed: &mut Editor, r: &ddc_model::refusal::Refusal) {
    let args: Vec<(&str, &str)> = r.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    ed.say(Severity::Refusal, crate::tr(r.msg_key, &args));
    ed.ix.last_refusal = Some(r.clone());
    ed.ix.offer = (!r.fixes.is_empty()).then(|| r.clone());
}
