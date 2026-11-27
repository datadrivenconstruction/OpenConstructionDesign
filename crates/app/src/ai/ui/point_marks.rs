// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Marks left where a question was asked: the answer's status, and the model "as it was" before
//! an applied plan. Session-only: nothing here is written to the project or kept for later.

use crate::ai::point::{ChangeToken, PoseResolved, ViewKind, ViewRef};
use ddc_ai::ImageData;
use crate::editor::Editor;
use ddc_model::{Model, RefusalCode};
use egui::{self, Color32, Pos2, Rect};
use std::path::PathBuf;

/// Most marks one session keeps; the oldest go first.
pub const MAX_MARKS: usize = ddc_ai::limits::POINT_MARKS.us();

#[derive(Clone, Debug, PartialEq)]
pub enum MarkState {
    Pending,
    Answered,
    Applied,
    Cancelled,
    /// The turn failed, was cancelled or refused: the code says why, the text is the detail.
    Failed(RefusalCode, String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointMark {
    pub id: u32,
    pub anchor: [f64; 3],
    pub view: ViewRef,
    /// The camera the question was asked from (3D); with `view` it is what "as it was" draws from.
    pub pose: Option<PoseResolved>,
    pub state: MarkState,
    pub msg_id: u64,
    pub applied_token: Option<ChangeToken>,
    /// Journal head before the apply: rolling a copy back to it gives the model "as it was".
    pub before_seq: Option<i64>,
    /// The journal row (seq, stamp) the apply wrote; the state follows it, not any text.
    applied_row: Option<(i64, i64)>,
}

#[derive(Default, Debug)]
pub struct Marks {
    list: Vec<PointMark>,
    next: u32,
}

/// A temp file name no other call shares, even for the same mark.
fn temp_db(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("{tag}-{}-{n}.db", std::process::id()))
}

fn stamp_of(ed: &Editor, seq: i64) -> Option<i64> {
    ed.model
        .sql()
        .query_row("SELECT coalesce(stamp,0) FROM op WHERE seq=?1", [seq], |r| r.get(0))
        .ok()
}

impl Marks {
    pub fn all(&self) -> &[PointMark] {
        &self.list
    }

    pub fn get(&self, id: u32) -> Option<&PointMark> {
        self.list.iter().find(|m| m.id == id)
    }

    /// A question was sent from `anchor`: the mark waits for the answer.
    pub fn add(&mut self, anchor: [f64; 3], view: ViewRef, msg_id: u64) -> u32 {
        self.next += 1;
        self.list.push(PointMark {
            id: self.next,
            anchor,
            view,
            pose: None,
            state: MarkState::Pending,
            msg_id,
            applied_token: None,
            before_seq: None,
            applied_row: None,
        });
        if self.list.len() > MAX_MARKS {
            self.list.remove(0);
        }
        self.next
    }

    pub fn set_pose(&mut self, id: u32, pose: Option<PoseResolved>) {
        if let Some(m) = self.list.iter_mut().find(|m| m.id == id) {
            m.pose = pose;
        }
    }

    fn by_msg(&mut self, msg_id: u64) -> Option<&mut PointMark> {
        self.list.iter_mut().find(|m| m.msg_id == msg_id)
    }

    pub fn answered(&mut self, msg_id: u64) {
        if let Some(m) = self.by_msg(msg_id).filter(|m| m.state == MarkState::Pending) {
            m.state = MarkState::Answered;
        }
    }

    pub fn failed(&mut self, msg_id: u64, why: RefusalCode, detail: &str) {
        if let Some(m) = self.by_msg(msg_id) {
            m.state = MarkState::Failed(why, detail.to_string());
        }
    }

    /// The plan of that answer was applied: `before_seq` is the journal head taken just before,
    /// `row` the (seq, stamp) the apply wrote.
    pub fn applied(&mut self, ed: &Editor, msg_id: u64, before_seq: i64, row: (i64, i64)) {
        let token = ChangeToken::of(ed);
        if let Some(m) = self.by_msg(msg_id) {
            m.state = MarkState::Applied;
            m.before_seq = Some(before_seq);
            m.applied_row = Some(row);
            m.applied_token = Some(token);
        }
    }

    /// The newest mark still waiting for a plan: the one an apply now belongs to.
    pub fn latest_open(&self) -> Option<u64> {
        self.list
            .iter()
            .rev()
            .find(|m| matches!(m.state, MarkState::Answered | MarkState::Pending))
            .map(|m| m.msg_id)
    }

    /// Applied marks follow the journal: the apply's row gone (undo) means cancelled, back
    /// again (redo) means applied.
    pub fn sync(&mut self, ed: &Editor) {
        for m in &mut self.list {
            let Some((seq, stamp)) = m.applied_row else {
                continue;
            };
            let alive = stamp_of(ed, seq).is_some_and(|s| s == stamp || stamp == 0);
            match (&m.state, alive) {
                (MarkState::Applied, false) => m.state = MarkState::Cancelled,
                (MarkState::Cancelled, true) => m.state = MarkState::Applied,
                _ => {}
            }
        }
    }

    /// Marks to draw in a view: in 3D all, in a plan those on the shown level.
    pub fn visible(&self, three_d: bool, level: i64) -> impl Iterator<Item = &PointMark> {
        self.list
            .iter()
            .filter(move |m| three_d || m.view.level.is_none() || m.view.level == Some(level))
    }

    /// "As it was" works while the journal still holds every row after `before_seq`.
    pub fn before_available(&self, ed: &Editor, id: u32) -> bool {
        let Some(b) = self.get(id).and_then(|m| m.before_seq) else {
            return false;
        };
        let top = ed.model.op_top().unwrap_or(0);
        b <= top && (b == top || stamp_of(ed, b + 1).is_some())
    }

    /// A working copy rolled back to before the apply; the live model is not touched.
    pub fn before_copy(&self, ed: &Editor, id: u32) -> Result<BeforeCopy, String> {
        if !self.before_available(ed, id) {
            return Err("the journal no longer holds that step".into());
        }
        let b = self.get(id).and_then(|m| m.before_seq).unwrap_or(0);
        let path = temp_db("ddc-before");
        ed.model.snapshot_to(&path).map_err(|e| e.to_string())?;
        let mut model = Model::open(&path.to_string_lossy()).map_err(|e| e.to_string())?;
        model.rollback_to(b).map_err(|e| e.to_string())?;
        Ok(BeforeCopy { model, path })
    }
}

/// The picture of a model in the view and pose a mark was asked from, drawn without a window
/// and without the marks themselves. Plans are framed as the assistant's own plan picture is;
/// sections and sheets have no picture here.
pub fn picture(ed: &Editor, view: &ViewRef, pose: Option<PoseResolved>, size: (u32, u32)) -> Result<ImageData, String> {
    draw(ed, None, &crate::ai::pose::facts_of(ed), view, pose, size)
}

/// `req_ed` is what 3D draws; a plan is drawn from `owned` when given (no second copy of the
/// database) and `req_ed` is then only the request's carrier.
fn draw(
    req_ed: &Editor,
    owned: Option<Editor>,
    facts: &crate::ai::pose::Facts,
    view: &ViewRef,
    pose: Option<PoseResolved>,
    size: (u32, u32),
) -> Result<ImageData, String> {
    use crate::ai::pose::{self, Pose};
    use crate::ai::snapshot::{HeadlessSource, Req, SnapshotSource};
    let (want, resolved) = match (view.kind, pose) {
        (ViewKind::View3d, Some(p)) => {
            let b = facts.bounds.unwrap_or([-1.0, -1.0, 0.0, 1.0, 1.0, 1.0]);
            let r = pose::from_orbit(size, p.target, p.yaw, p.pitch, p.distance, b);
            (Pose::Iso { az_deg: 0.0 }, r)
        }
        (ViewKind::Plan, _) => {
            let want = Pose::Plan { level: view.level.unwrap_or(req_ed.level), rect: None };
            let r = pose::resolve_in(facts, &want, size).map_err(|e| e.message)?;
            (want, r)
        }
        _ => return Err("this view has no picture".into()),
    };
    let mut src = HeadlessSource::new();
    if let Some(o) = owned {
        src = src.with_owned(o);
    }
    let rendered = suppressed(|| {
        src.render(&Req {
            ed: req_ed,
            units: req_ed.tool.units,
            pose: &want,
            resolved: Some(&resolved),
            size,
            select: &[],
            labels: &[],
        })
    })
    .map_err(|e| e.message)?;
    Ok(rendered.image)
}

impl Marks {
    /// "As it was": the picture of the model before the apply, from the mark's own view and pose.
    pub fn before_picture(&self, ed: &Editor, id: u32, size: (u32, u32)) -> Result<ImageData, String> {
        let m = self.get(id).ok_or("no such mark")?;
        let before = self.before_editor(ed, id)?;
        let facts = crate::ai::pose::facts_of(&before);
        match m.view.kind {
            // 3D reads the scene of the copy; a plan is drawn from the copy itself
            ViewKind::Plan => draw(ed, Some(before), &facts, &m.view, m.pose, size),
            _ => draw(&before, None, &facts, &m.view, m.pose, size),
        }
    }

    /// A private editor over a copy of the live model rolled back to before the apply.
    fn before_editor(&self, ed: &Editor, id: u32) -> Result<Editor, String> {
        if !self.before_available(ed, id) {
            return Err("the journal no longer holds that step".into());
        }
        let b = self.get(id).and_then(|m| m.before_seq).unwrap_or(0);
        let path = temp_db("ddc-before-ed");
        ed.model.snapshot_to(&path).map_err(|e| e.to_string())?;
        let opened = crate::ai::session::open_editor(&path, ed.tool.units).map_err(|e| format!("{e:?}"));
        let _ = std::fs::remove_file(&path);
        let mut copy = opened?;
        copy.model.rollback_to(b).map_err(|e| e.to_string())?;
        copy.reload().map_err(|e| e.to_string())?;
        copy.set_level(ed.level);
        Ok(copy)
    }
}

pub struct BeforeCopy {
    pub model: Model,
    path: PathBuf,
}

impl Drop for BeforeCopy {
    fn drop(&mut self) {
        // a file still held open is left to the temp cleaner
        let _ = std::fs::remove_file(&self.path);
    }
}

thread_local! {
    static MARKS: std::cell::RefCell<Marks> = std::cell::RefCell::new(Marks::default());
    static SUPPRESS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static FOCUS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The session's marks; kept apart from the panel state, which is lent out while it draws.
pub fn with<R>(f: impl FnOnce(&mut Marks) -> R) -> R {
    MARKS.with(|m| f(&mut m.borrow_mut()))
}

/// A click on a bubble asks the panel to open at the answer; read once.
pub fn take_focus() -> bool {
    FOCUS.with(|f| f.replace(false))
}

/// Runs `f` with the bubbles hidden: pictures drawn for the assistant or for "as it was" are of
/// the model, not of the marks on it. Nests.
pub fn suppressed<R>(f: impl FnOnce() -> R) -> R {
    let _hidden = hide();
    f()
}

/// Bubbles stay hidden while this lives.
#[must_use]
pub struct Hidden;

impl Drop for Hidden {
    fn drop(&mut self) {
        SUPPRESS.with(|c| c.set(c.get() - 1));
    }
}

pub fn hide() -> Hidden {
    SUPPRESS.with(|c| c.set(c.get() + 1));
    Hidden
}

/// Why a turn that ended in an error is shown as failed.
pub fn code_of(e: &crate::ai::AiError) -> RefusalCode {
    use crate::ai::AiError as E;
    match e {
        E::Busy => RefusalCode::Busy,
        E::Stale => RefusalCode::PlanStale,
        E::Blocked => RefusalCode::Blocked,
        E::NeedsConfirmation => RefusalCode::NeedsConfirmation,
        _ => RefusalCode::Internal,
    }
}

/// Bubble radius, pixels.
pub const R: f32 = 9.0;

pub fn colour(th: &ddc_ui::theme::Theme, s: &MarkState) -> Color32 {
    match s {
        MarkState::Pending => th.c("warning"),
        MarkState::Answered => th.c("accent"),
        MarkState::Applied => th.c("success"),
        MarkState::Cancelled => th.c("text.secondary"),
        MarkState::Failed(..) => th.c("danger"),
    }
}

fn glyph(s: &MarkState) -> &'static str {
    match s {
        MarkState::Pending => "?",
        MarkState::Answered => "i",
        MarkState::Applied => "\u{2713}",
        MarkState::Cancelled => "\u{21ba}",
        MarkState::Failed(..) => "!",
    }
}

/// Where each visible mark sits on screen: `project` maps an anchor to a pixel; marks off the
/// canvas (or behind the camera) are left out.
pub fn layout(
    marks: &Marks,
    three_d: bool,
    level: i64,
    rect: Rect,
    project: &dyn Fn([f64; 3]) -> Option<Pos2>,
) -> Vec<(u32, Pos2)> {
    marks
        .visible(three_d, level)
        .filter_map(|m| project(m.anchor).map(|p| (m.id, p)))
        .filter(|(_, p)| rect.contains(*p))
        .collect()
}

/// Paints the bubbles over a view and returns the one clicked; a click also asks the panel to
/// show the answer.
pub fn overlay(
    ui: &mut egui::Ui,
    three_d: bool,
    level: i64,
    rect: Rect,
    project: &dyn Fn([f64; 3]) -> Option<Pos2>,
) -> Option<u32> {
    if SUPPRESS.with(|c| c.get()) > 0 {
        return None;
    }
    let shown = with(|k| {
        layout(k, three_d, level, rect, project)
            .into_iter()
            .filter_map(|(id, p)| k.get(id).map(|m| (id, p, m.state.clone())))
            .collect::<Vec<_>>()
    });
    let mut clicked = None;
    for (id, p, st) in shown {
        let hit = Rect::from_center_size(p, egui::Vec2::splat(R * 2.0));
        let resp = ui.interact(hit, egui::Id::new(("ask_mark", three_d, id)), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let th = ddc_ui::theme::of(ui.ctx());
        let on = th.c("accent.text_on");
        painter.circle_filled(p, R, colour(&th, &st));
        painter.circle_stroke(p, R, egui::Stroke::new(1.5, on));
        painter.text(p, egui::Align2::CENTER_CENTER, glyph(&st), ddc_ui::typo::font(ddc_ui::typo::Role::Caption), on);
        if resp.clicked() {
            FOCUS.with(|f| f.set(true));
            clicked = Some(id);
        }
    }
    clicked
}

#[cfg(test)]
#[path = "point_marks_tests.rs"]
mod tests;
