// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! State machine of the Wall tool (no UI): mouse/key events -> draft -> commit.

use crate::snap::{candidates, Snap, SnapCtx, SnapKind, SnapWall};
use crate::units::{parse_typed, DecimalMark, Typed, Units};
use ddc_geom::arc::{ArcAxis, BulgeRing, CHORD_SAG};
use ddc_geom::{v2, v3, Tol, V2, V3};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Select,
    Polyline,
    Rect,
    /// Wall on a picked drawing line (`wall_pick`).
    PickLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    Backspace,
    Tab,
    /// `C`: close the contour.
    Close,
    /// `A`: toggle "Line/Arc".
    Arc,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Move(V2),
    Click(V2),
    Press(V2),
    Release(V2),
    Key(Key),
    Char(char),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Nothing,
    /// Wall axis segments of one gesture (a single model operation).
    Commit(Vec<(V2, V2)>),
    /// Click in selection mode.
    Pick(V2),
    /// Arc wall: chord a→b and bulge (a single undo step).
    Arc {
        a: V2,
        b: V2,
        bulge: f64,
    },
    /// Spline wall through the clicked points (three or more; one undo step).
    Spline(Vec<V2>),
    Error(String),
}

/// Point coincidence tolerance, m.
const SAME: f64 = 1e-6;
/// Minimum wall length, m.
pub const MIN_LEN: f64 = 0.01;

/// One snap query's chain: the draft segments, the point the segment starts from
/// and whether the wall's refinements (touch points, inference, hints) apply. The session that
/// draws owns the points; the engine keeps the last query as it keeps `cursor` and `snaps`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chain {
    pub pts: Vec<V2>,
    pub from: Option<V3>,
    pub refine: bool,
    /// The query of the wall tool itself (`track`): its start point follows its draft live.
    pub own: bool,
    /// What the session draws: a session's chain is a polyline (`of`), the wall's its mode.
    pub kind: Mode,
    /// The wall draft's angle lock is on: a session's chain has none.
    pub locked: bool,
}

impl Chain {
    /// A session's chain of points in the plan at `z`: the segment starts at the last one.
    pub fn of(pts: &[V2], z: f64, refine: bool) -> Chain {
        let from = pts.last().map(|p| v3(p.x, p.y, z));
        Chain { pts: pts.to_vec(), from, refine, own: false, kind: Mode::Polyline, locked: false }
    }

    pub fn from_xy(&self) -> Option<V2> {
        self.from.map(|f| v2(f.x, f.y))
    }
}

/// The draft of the drawing tools: typed text, chain points, rectangle corner, angle lock.
#[derive(Clone, Debug, Default)]
pub struct WallDraft {
    pub points: Vec<V2>,
    pub input: String,
    pub rect_start: Option<V2>,
    /// Direction set by typing `<angle`.
    pub lock_angle: Option<f64>,
}

/// The draft of no drawing session: a session's chain has no wall points, input or lock.
pub static NO_DRAFT: WallDraft = WallDraft {
    points: Vec::new(),
    input: String::new(),
    rect_start: None,
    lock_angle: None,
};

pub struct WallTool {
    pub mode: Mode,
    /// The chain of the last snap query.
    pub last: Chain,
    pub units: Units,
    /// Decimal mark of the language the typed text is read in (the plan view sets it).
    pub mark: DecimalMark,
    pub cursor: V2,
    pub snaps: Vec<Snap>,
    pub snap_idx: usize,
    pub walls: Vec<SnapWall>,
    /// Snaps to linked models: its own list, untouched by an editor reload.
    pub link_walls: Vec<SnapWall>,
    pub grid: f64,
    pub radius: f64,
    /// Drawing aids from the status bar: grid points, object snaps, horizontal/vertical only.
    pub grid_on: bool,
    pub snap_on: bool,
    pub ortho: bool,
    /// Arc mode in the Wall tool: start, point on the arc, end.
    pub arc: bool,
    /// Spline wall: clicks are points the axis passes through; Enter or Esc ends it.
    pub spline: bool,
    /// Curtain wall tool: the axis is drawn as a wall's (the wall tool's draft state, which
    /// its session takes over with the rest of `WallTool`).
    pub curtain: bool,
    /// Layer of model tool elements in the plan (cached by revision).
    pub layer: crate::tools::Layer,
    /// Visible lines of underlay drawings on the current level: snap targets.
    pub underlay_segs: std::sync::Arc<Vec<(V2, V2)>>,
    /// Remote points for alignment guides (rebuilt when the wall set changes).
    pub align: crate::snap_align::AlignIndex,
    /// No-snap modifier held: the point goes exactly under the cursor.
    pub no_snap: bool,
    /// Held hit, kind override and the hysteresis between moves (the Alt hold is `no_snap`).
    pub snap_state: crate::snap_core::SnapState,
    /// Extension and parallel guides from the walls: the profile of the drawing tools.
    pub guides: bool,
    /// Kinds the family being drawn offers (the session profile).
    pub profile: crate::snap::SnapProfile,
    /// The hit the user pinned with its chip: the next commit writes its rule with the element.
    pub pin_armed: Option<crate::snap_core::SnapHit>,
    /// Half thickness of the wall being placed, m (face location in `PickLine`).
    pub half: f64,
    /// `PickLine`: wall face on the line instead of the axis (`F`).
    pub pick_face: bool,
    /// Ids of `walls` in the same order (refusal names the wall).
    pub wall_ids: Vec<i64>,
    /// `PickLine` for grids and beams (`line_pick`): kind, layers of the lines, taken axes.
    pub lp: crate::line_pick::State,
}

impl Default for WallTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Ortho: the point moved onto the horizontal or vertical through `from`, whichever is nearer.
pub fn ortho_point(from: V2, p: V2) -> V2 {
    let d = p - from;
    if d.x.abs() >= d.y.abs() {
        v2(p.x, from.y)
    } else {
        v2(from.x, p.y)
    }
}

fn dir_deg(a: f64) -> V2 {
    let r = a.to_radians();
    v2(r.cos(), r.sin())
}

impl WallTool {
    pub fn new() -> Self {
        Self {
            mode: Mode::Polyline,
            last: Chain::default(),
            units: Units::Metric,
            mark: crate::units::ui_decimal_mark(),
            cursor: v2(0.0, 0.0),
            snaps: vec![],
            snap_idx: 0,
            walls: vec![],
            link_walls: vec![],
            grid: 1.0,
            radius: 0.15,
            grid_on: true,
            snap_on: true,
            ortho: false,
            arc: false,
            spline: false,
            curtain: false,
            layer: Default::default(),
            underlay_segs: Default::default(),
            align: Default::default(),
            no_snap: false,
            snap_state: Default::default(),
            guides: true,
            profile: crate::snap::SnapProfile::ALL,
            pin_armed: None,
            half: 0.0,
            pick_face: false,
            wall_ids: vec![],
            lp: Default::default(),
        }
    }

    /// No chain, typed number or rectangle in progress: Esc has nothing left to cancel.
    pub fn is_idle_in(&self, dr: &WallDraft) -> bool {
        dr.points.is_empty()
            && dr.input.is_empty()
            && dr.rect_start.is_none()
    }

    /// The wall tool's own arm request (its mode); the wall session begins from it. Whether
    /// any tool draws is `Editor::tool_armed` (the session).
    pub fn armed(&self) -> bool {
        self.mode != Mode::Select
    }

    /// What the last query draws: a session's chain or the wall tool's own mode.
    /// Previews and refinements read this, so a session draws without arming the wall.
    pub fn kind(&self) -> Mode {
        if self.last.own || self.last.kind == Mode::Select {
            self.mode
        } else {
            self.last.kind
        }
    }

    /// The draft is the session's: the editor clears it with the mode (`Editor::set_mode`).
    pub fn set_mode(&mut self, m: Mode) {
        self.reset_state();
        self.mode = m;
    }

    pub fn reset_in(&mut self, dr: &mut WallDraft) {
        *dr = WallDraft::default();
        self.reset_state();
    }

    /// The tool's own drawing state (snaps, pin, pick set, last query) without a draft.
    pub fn reset_state(&mut self) {
        self.lp.pick_chain = false;
        self.snap_idx = 0;
        // a candidate from the last hover would draw its marker after the tool is gone
        self.snaps.clear();
        self.snap_state.acquired.clear();
        self.pin_armed = None;
        // a session's chain from before would keep drawing its preview
        self.last = Chain::default();
    }

    pub fn is_drawing_in(&self, dr: &WallDraft) -> bool {
        !dr.points.is_empty() || dr.rect_start.is_some()
    }

    fn from_in(&self, dr: &WallDraft) -> Option<V2> {
        match self.mode {
            Mode::Rect => dr.rect_start,
            _ => dr.points.last().copied(),
        }
    }

    /// Current point with snapping and angle lock applied.
    pub fn snapped_in(&self, dr: &WallDraft) -> V2 {
        let p = self.snaps.get(self.snap_idx).map_or(self.cursor, |s| s.p);
        match (dr.lock_angle, self.query_from_in(dr)) {
            (Some(a), Some(f)) => {
                let d = dir_deg(a);
                f + d * crate::snap::dot(p - f, d).max(0.0)
            }
            // a rectangle needs both axes: ortho would flatten it
            (None, Some(f)) if self.ortho && self.kind() != Mode::Rect => ortho_point(f, p),
            _ => p,
        }
    }

    pub fn snap(&self) -> Option<Snap> {
        self.snaps
            .get(self.snap_idx)
            .copied()
            .filter(|s| s.kind != SnapKind::Free)
    }

    /// Cursor and snap candidates for the wall draft.
    pub fn track_in(&mut self, dr: &mut WallDraft, p: V2) {
        let from = self.from_in(dr).map(|f| v3(f.x, f.y, self.snap_state.z));
        let pts = dr.points.clone();
        let locked = dr.lock_angle.is_some();
        self.track_chain(p, Chain { pts, from, refine: true, own: true, kind: self.mode, locked });
    }

    /// Cursor and snap candidates for the chain of session `c`.
    pub fn track_chain(&mut self, p: V2, c: Chain) {
        self.last = c;
        self.update(p);
    }

    /// Aim at `p` for the chain `c` without new candidates (a tool that does not snap its moves).
    pub fn aim(&mut self, p: V2, c: Chain) {
        self.cursor = p;
        self.last = c;
    }

    /// Draft of a session's chain `pts` with the rubber segment to the snapped point.
    pub fn chain_preview(&self, pts: &[V2]) -> Vec<(V2, V2)> {
        if self.kind() != Mode::Polyline || self.arc || self.spline {
            return vec![];
        }
        let e = self.snapped_in(&NO_DRAFT);
        let mut s: Vec<(V2, V2)> = pts.windows(2).map(|w| (w[0], w[1])).collect();
        if let Some(&l) = pts.last() {
            if (e - l).len() >= MIN_LEN {
                s.push((l, e));
            }
        }
        s
    }

    /// The start point of the segment being drawn: the wall's live, a session's from its query.
    pub fn query_from_in(&self, dr: &WallDraft) -> Option<V2> {
        match self.last.own {
            true => self.from_in(dr),
            false => self.last.from_xy(),
        }
    }

    fn update(&mut self, p: V2) {
        self.cursor = p;
        let grid = if self.grid_on { self.grid } else { 0.0 };
        if !self.snap_on {
            // object snaps off: the grid (its own toggle) and the free point stay
            let ctx = SnapCtx {
                walls: &[],
                draft: &[],
                from: None,
                grid,
                radius: self.radius,
            };
            self.snaps = candidates(&ctx, p);
            self.snap_idx = 0;
            return;
        }
        let all: std::borrow::Cow<[SnapWall]> = if self.link_walls.is_empty() {
            std::borrow::Cow::Borrowed(&self.walls)
        } else {
            self.walls.iter().chain(&self.link_walls).copied().collect()
        };
        let ctx = SnapCtx {
            walls: &all,
            draft: &self.last.pts,
            from: self.last.from_xy(),
            grid,
            radius: self.radius,
        };
        self.snaps = candidates(&ctx, p);
        self.align.refresh(&all);
        let extra = self.align.candidates(&self.last.pts, p, self.radius);
        if !extra.is_empty() {
            crate::snap::merge(&mut self.snaps, extra, p);
        }
        if !self.underlay_segs.is_empty() {
            let extra = crate::snap::underlay_candidates(&self.underlay_segs, p, self.radius);
            crate::snap::merge(&mut self.snaps, extra, p);
        }
        if self.guides {
            let near: Vec<(V2, V2)> = all
                .iter()
                .filter(|w| (crate::snap::closest_on_segment(p, w.a, w.b).0 - p).len() <= crate::snap::EXT_REACH)
                .map(|w| (w.a, w.b))
                .collect();
            let mut extra = crate::snap::extension_candidates(&near, p, self.radius);
            if let Some(f) = self.last.from_xy() {
                extra.extend(crate::snap::parallel_candidates(&near, f, p, self.radius));
            }
            if !extra.is_empty() {
                crate::snap::merge(&mut self.snaps, extra, p);
            }
        }
        if !self.snap_state.acquired.is_empty() {
            let extra = crate::snap::tracking_candidates(&self.snap_state.acquired_points(), p, self.radius);
            crate::snap::merge(&mut self.snaps, extra, p);
        }
        // the step lies on the cursor ray: under an angle lock it would move the point along the lock
        let step = crate::snap::equal_step_candidate(&self.last.pts, p, self.radius);
        if let Some(s) = step.filter(|_| !self.last.locked) {
            crate::snap::merge(&mut self.snaps, vec![s], p);
        }
        crate::snap_refine::refine_tool(self, p);
        self.snaps = self.profile.gate(std::mem::take(&mut self.snaps));
        self.hold_best(p);
    }

    /// Pick the answer through `SnapState` (hysteresis, override, Alt hold) and point `snap_idx` at it;
    /// a held hit that left the radius but not 1.5 r is put back at the front of the list.
    fn hold_best(&mut self, p: V2) {
        use crate::snap_core::{Hold, SnapHit};
        let hits: Vec<SnapHit> = self.snaps.iter().map(|s| SnapHit::at(*s, self.snap_state.z)).collect();
        let hold = Hold { free: self.no_snap };
        let hit = self.snap_state.resolve(p, self.radius, &hits, hold);
        let at = self.snaps.iter().position(|s| s.kind == hit.kind && s.p == hit.xy());
        self.snap_idx = match at {
            Some(i) => i,
            None => {
                self.snaps.insert(0, Snap { p: hit.xy(), kind: hit.kind });
                0
            }
        };
    }

    /// Centre of the pin chip next to the current hit, when its kind can be pinned. The chip sits
    /// inside the 1.5 r hold, so reaching it keeps the hit.
    pub fn pin_chip(&self) -> Option<V2> {
        let s = self.snap()?;
        (self.profile.pin && crate::snap_pin::pin_kind(s.kind).is_some())
            .then(|| s.p + ddc_geom::v2(0.7 * self.radius, 0.7 * self.radius))
    }

    /// A click on the chip arms (or disarms) the pin of the current hit; true when absorbed.
    fn pin_click(&mut self, p: V2) -> bool {
        match (self.pin_chip(), self.snap()) {
            (Some(c), Some(s)) if (c - p).len() <= 0.5 * self.radius => {
                self.pin_armed = match self.pin_armed {
                    Some(_) => None,
                    None => Some(crate::snap_core::SnapHit::at(s, self.snap_state.z)),
                };
                true
            }
            _ => false,
        }
    }

    /// Clock tick (ms): a rest of 300 ms on a point captures it for guides, or releases it.
    pub fn tick(&mut self, now_ms: u64) {
        let hit = self.snap().map(|s| crate::snap_core::SnapHit::at(s, self.snap_state.z));
        if self.snap_state.dwell(hit.as_ref(), now_ms) {
            self.update(self.cursor);
        }
    }

    /// Shift+M and friends: only this kind answers until the next click (`None` clears it).
    pub fn set_snap_override(&mut self, kind: Option<SnapKind>) {
        self.snap_state.over = kind.map(crate::snap_core::Override);
    }

    pub fn handle_in(&mut self, dr: &mut WallDraft, e: Event) -> Outcome {
        if self.mode == Mode::PickLine {
            return self.pick_handle(e);
        }
        match e {
            Event::Move(p) => {
                self.track_in(dr, p);
                Outcome::Nothing
            }
            Event::Click(p) => {
                if self.pin_click(p) {
                    return Outcome::Nothing;
                }
                self.track_in(dr, p);
                self.click_in(dr)
            }
            Event::Press(p) => {
                self.track_in(dr, p);
                if self.mode == Mode::Rect && dr.rect_start.is_none() {
                    dr.rect_start = Some(self.snapped_in(dr));
                }
                Outcome::Nothing
            }
            Event::Release(p) => {
                self.track_in(dr, p);
                match dr.rect_start {
                    Some(s) if self.mode == Mode::Rect && self.rect_ok(s, self.snapped_in(dr)) => {
                        let e = self.snapped_in(dr);
                        self.commit_rect_in(dr, s, e)
                    }
                    _ => Outcome::Nothing,
                }
            }
            Event::Char(c) => {
                // Shift+M: only midpoints answer, until the next click
                if c == 'M' && dr.input.is_empty() {
                    let on = self.snap_state.over.is_none();
                    self.set_snap_override(on.then_some(SnapKind::Midpoint));
                    self.track_in(dr, self.cursor);
                    return Outcome::Nothing;
                }
                if (c == 'c' || c == 'C') && dr.input.is_empty() {
                    return self.handle_in(dr, Event::Key(Key::Close));
                }
                if !c.is_control() && self.mode != Mode::Select && dr.input.len() < 64 {
                    dr.input.push(c);
                }
                Outcome::Nothing
            }
            Event::Key(k) => self.key_in(dr, k),
        }
    }

    fn click_in(&mut self, dr: &mut WallDraft) -> Outcome {
        let p = self.snapped_in(dr);
        self.snap_state.clicked();
        match self.mode {
            Mode::Select => Outcome::Pick(p),
            Mode::Rect => match dr.rect_start {
                None => {
                    dr.rect_start = Some(p);
                    Outcome::Nothing
                }
                Some(s) if self.rect_ok(s, p) => self.commit_rect_in(dr, s, p),
                Some(_) => Outcome::Error("rect zero size".into()),
            },
            Mode::Polyline if self.arc => self.arc_click_in(dr, p),
            Mode::Polyline => self.add_point_in(dr, p),
            Mode::PickLine => self.pick_at(self.cursor),
        }
    }

    /// Arc by points or by typed radius (after start and end).
    fn arc_from_in(&self, dr: &WallDraft, end: V2, radius: Option<f64>) -> Result<ArcAxis, String> {
        self.arc_of_in(dr, &dr.points, end, radius)
    }

    fn arc_of_in(&self, dr: &WallDraft, pts: &[V2], end: V2, radius: Option<f64>) -> Result<ArcAxis, String> {
        let tol = Tol::default();
        let a = pts[0];
        let r = match radius {
            Some(r) => {
                let ccw = (end - a).cross(self.snapped_in(dr) - a) < 0.0;
                ArcAxis::with_radius(a, end, r, ccw, tol)
            }
            None => ArcAxis::through(a, pts[1], end, tol),
        };
        r.map_err(|e| crate::tr("edit_panels.wt.arc_fail", &[("e", format!("{e:?}").as_str())]))
    }

    fn arc_click_in(&mut self, dr: &mut WallDraft, p: V2) -> Outcome {
        if let Some(&last) = dr.points.last() {
            if (p - last).len() < MIN_LEN {
                return Outcome::Error("wall too short".into());
            }
        }
        if dr.points.len() < 2 {
            dr.points.push(p);
            dr.input.clear();
            self.track_in(dr, self.cursor);
            return Outcome::Nothing;
        }
        match self.arc_from_in(dr, p, None) {
            Ok(ax) => {
                let a = dr.points[0];
                self.reset_in(dr);
                Outcome::Arc {
                    a,
                    b: p,
                    bulge: ax.bulge,
                }
            }
            Err(m) => Outcome::Error(m),
        }
    }

    /// Arc or spline draft of the chain `pts` (the wall's, a form's or a solid's).
    pub fn arc_preview_of_in(&self, dr: &WallDraft, pts: &[V2]) -> Vec<V2> {
        if self.spline && self.kind() == Mode::Polyline {
            return self.spline_preview_in(dr, pts);
        }
        if !(self.arc && self.kind() == Mode::Polyline) || pts.is_empty() {
            return vec![];
        }
        let e = self.snapped_in(dr);
        if pts.len() == 1 {
            return vec![pts[0], e];
        }
        let radius = match parse_typed(&dr.input, self.units, self.mark) {
            Ok(Typed::Polar(Some(r), None)) => Some(r),
            _ => None,
        };
        let end = if radius.is_some() { pts[1] } else { e };
        match self.arc_of_in(dr, pts, end, radius) {
            Ok(ax) => BulgeRing {
                pts: vec![pts[0], end],
                bulges: vec![ax.bulge, 0.0],
            }
            .tessellate(CHORD_SAG),
            Err(_) => vec![pts[0], pts[1], end],
        }
    }

    pub fn toggle_arc_in(&mut self, dr: &mut WallDraft) {
        self.arc = !self.arc;
        self.spline = false;
        self.reset_in(dr);
    }

    pub fn toggle_spline_in(&mut self, dr: &mut WallDraft) {
        self.spline = !self.spline;
        self.arc = false;
        self.reset_in(dr);
    }

    /// Axis through the points and the rubber point, as a polyline (straight with two points).
    fn spline_preview_in(&self, dr: &WallDraft, pts: &[V2]) -> Vec<V2> {
        if pts.is_empty() {
            return vec![];
        }
        let mut pts = pts.to_vec();
        let e = self.rubber_end_in(dr);
        if (e - pts[pts.len() - 1]).len() >= MIN_LEN {
            pts.push(e);
        }
        if pts.len() < 3 {
            return pts;
        }
        match ddc_geom::wall_spline::SplineAxis::through(&pts) {
            Ok(ax) => ax.offset_polyline(0.0, CHORD_SAG),
            Err(_) => pts,
        }
    }

    /// End of a spline gesture: three points or more make the wall, fewer are dropped.
    fn finish_spline_in(&mut self, dr: &mut WallDraft) -> Outcome {
        let pts = std::mem::take(&mut dr.points);
        self.reset_in(dr);
        if pts.len() >= 3 {
            Outcome::Spline(pts)
        } else if pts.len() == 2 {
            Outcome::Error(crate::tr("spline.wt.three_points", &[]))
        } else {
            Outcome::Nothing
        }
    }

    fn add_point_in(&mut self, dr: &mut WallDraft, p: V2) -> Outcome {
        if let Some(&last) = dr.points.last() {
            if (p - last).len() < MIN_LEN {
                return Outcome::Error("wall too short".into());
            }
        }
        if self.spline {
            dr.points.push(p);
            dr.lock_angle = None;
            dr.input.clear();
            self.track_in(dr, self.cursor);
            return Outcome::Nothing;
        }
        if dr.points.len() >= 3 && (p - dr.points[0]).len() < SAME {
            return self.finish_in(dr, true);
        }
        dr.points.push(p);
        dr.lock_angle = None;
        dr.input.clear();
        self.track_in(dr, self.cursor);
        Outcome::Nothing
    }

    fn key_in(&mut self, dr: &mut WallDraft, k: Key) -> Outcome {
        match k {
            Key::Tab => {
                if !self.snaps.is_empty() {
                    self.snap_idx = (self.snap_idx + 1) % self.snaps.len();
                }
                Outcome::Nothing
            }
            Key::Backspace => {
                if dr.input.pop().is_none() {
                    if self.mode == Mode::Rect {
                        dr.rect_start = None;
                    } else {
                        dr.points.pop();
                    }
                    dr.lock_angle = None;
                }
                Outcome::Nothing
            }
            Key::Escape => {
                if !dr.input.is_empty() {
                    dr.input.clear();
                    Outcome::Nothing
                } else if self.mode == Mode::Polyline && self.spline {
                    self.finish_spline_in(dr)
                } else if self.mode == Mode::Polyline && !self.arc {
                    self.finish_in(dr, false)
                } else {
                    self.reset_in(dr);
                    Outcome::Nothing
                }
            }
            Key::Arc => {
                self.toggle_arc_in(dr);
                Outcome::Nothing
            }
            Key::Close if self.arc || self.spline => Outcome::Nothing,
            Key::Close => match self.mode {
                Mode::Polyline if dr.points.len() >= 3 => self.finish_in(dr, true),
                Mode::Polyline => Outcome::Error("close needs three points".into()),
                _ => Outcome::Nothing,
            },
            Key::Enter => {
                if dr.input.trim().is_empty() {
                    dr.input.clear();
                    return match self.mode {
                        Mode::Polyline if self.spline => self.finish_spline_in(dr),
                        Mode::Polyline if !self.arc => self.finish_in(dr, false),
                        _ => Outcome::Nothing,
                    };
                }
                let r = parse_typed(&dr.input, self.units, self.mark);
                let out = match r {
                    Err(e) => Outcome::Error(crate::units::typed_error_text(
                        &e,
                        &dr.input,
                        self.mark == DecimalMark::Comma,
                    )),
                    Ok(t) => self.apply_typed_in(dr, t),
                };
                // a rejected entry leaves nothing behind: the next digits start a new number
                if matches!(out, Outcome::Error(_)) {
                    dr.input.clear();
                }
                out
            }
        }
    }

    fn apply_typed_in(&mut self, dr: &mut WallDraft, t: Typed) -> Outcome {
        if self.mode == Mode::Rect {
            let Some(s) = dr.rect_start else {
                return Outcome::Error(crate::tr("edit_panels.wt.rect_corner", &[]));
            };
            let Typed::Pair(w, h) = t else {
                return Outcome::Error(crate::tr("edit_panels.wt.rect_size", &[]));
            };
            let c = self.snapped_in(dr) - s;
            let sx = if c.x < 0.0 { -1.0 } else { 1.0 };
            let sy = if c.y < 0.0 { -1.0 } else { 1.0 };
            let p = s + v2(sx * w, sy * h);
            dr.input.clear();
            if !self.rect_ok(s, p) {
                return Outcome::Error("rect zero size".into());
            }
            return self.commit_rect_in(dr, s, p);
        }
        if self.mode == Mode::Select {
            return Outcome::Nothing;
        }
        if self.arc {
            let (Typed::Polar(Some(r), None), 2) = (t, dr.points.len()) else {
                return Outcome::Error(crate::tr("edit_panels.wt.arc_pts", &[]));
            };
            let end = dr.points[1];
            return match self.arc_from_in(dr, end, Some(r)) {
                Ok(ax) => {
                    let a = dr.points[0];
                    self.reset_in(dr);
                    Outcome::Arc {
                        a,
                        b: end,
                        bulge: ax.bulge,
                    }
                }
                Err(m) => Outcome::Error(m),
            };
        }
        let last = dr.points.last().copied();
        let p = match (t, last) {
            (Typed::Pair(x, y), None) => v2(x, y),
            (Typed::Pair(dx, dy), Some(l)) => l + v2(dx, dy),
            (Typed::Polar(_, _), None) => {
                return Outcome::Error(crate::tr("edit_panels.wt.first_pt", &[]))
            }
            (Typed::Polar(None, Some(a)), Some(_)) => {
                dr.lock_angle = Some(a);
                dr.input.clear();
                return Outcome::Nothing;
            }
            (Typed::Polar(len, ang), Some(l)) => {
                let d = match ang.or(dr.lock_angle) {
                    Some(a) => dir_deg(a),
                    None => {
                        let v = self.snapped_in(dr) - l;
                        if v.len() < SAME {
                            v2(1.0, 0.0)
                        } else {
                            v * (1.0 / v.len())
                        }
                    }
                };
                l + d * len.unwrap_or(0.0)
            }
        };
        self.add_point_in(dr, p)
    }

    fn rect_ok(&self, a: V2, b: V2) -> bool {
        (a.x - b.x).abs() >= MIN_LEN && (a.y - b.y).abs() >= MIN_LEN
    }

    fn commit_rect_in(&mut self, dr: &mut WallDraft, a: V2, b: V2) -> Outcome {
        let c = [a, v2(b.x, a.y), b, v2(a.x, b.y)];
        self.reset_in(dr);
        Outcome::Commit((0..4).map(|i| (c[i], c[(i + 1) % 4])).collect())
    }

    fn finish_in(&mut self, dr: &mut WallDraft, close: bool) -> Outcome {
        let mut segs: Vec<(V2, V2)> = dr.points.windows(2).map(|w| (w[0], w[1])).collect();
        if close {
            if let (Some(&f), Some(&l)) = (dr.points.first(), dr.points.last()) {
                if (f - l).len() >= MIN_LEN {
                    segs.push((l, f));
                }
            }
        }
        self.reset_in(dr);
        if segs.is_empty() {
            Outcome::Nothing
        } else {
            Outcome::Commit(segs)
        }
    }

    /// Rubber-band point: typed length (if it parses) or the snapped cursor.
    pub fn rubber_end_in(&self, dr: &WallDraft) -> V2 {
        let s = self.snapped_in(dr);
        let Some(l) = self.from_in(dr) else { return s };
        if self.mode == Mode::Polyline {
            if let Ok(Typed::Polar(Some(len), ang)) = parse_typed(&dr.input, self.units, self.mark) {
                let v = s - l;
                let d = match ang.or(dr.lock_angle) {
                    Some(a) => dir_deg(a),
                    None if v.len() > SAME => v * (1.0 / v.len()),
                    None => v2(1.0, 0.0),
                };
                return l + d * len;
            }
        }
        s
    }

    /// Draft segments for preview (including the rubber-band one).
    pub fn preview_in(&self, dr: &WallDraft) -> Vec<(V2, V2)> {
        let e = self.rubber_end_in(dr);
        match self.mode {
            Mode::Select => vec![],
            Mode::PickLine => crate::wall_pick::preview(self),
            Mode::Rect => match dr.rect_start {
                Some(a) if self.rect_ok(a, e) => {
                    let c = [a, v2(e.x, a.y), e, v2(a.x, e.y)];
                    (0..4).map(|i| (c[i], c[(i + 1) % 4])).collect()
                }
                _ => vec![],
            },
            Mode::Polyline if self.arc || self.spline => vec![],
            Mode::Polyline => {
                let mut s: Vec<(V2, V2)> = dr.points.windows(2).map(|w| (w[0], w[1])).collect();
                if let Some(&l) = dr.points.last() {
                    if (e - l).len() >= MIN_LEN {
                        s.push((l, e));
                    }
                }
                s
            }
        }
    }
}

/// The engine with a draft of its own, for tests that drive it without a session.
#[cfg(test)]
pub struct Rig {
    pub t: WallTool,
    pub draft: WallDraft,
}

#[cfg(test)]
impl Rig {
    pub fn new() -> Self {
        Self { t: WallTool::new(), draft: WallDraft::default() }
    }
    pub fn handle(&mut self, e: Event) -> Outcome {
        self.t.handle_in(&mut self.draft, e)
    }
    pub fn track(&mut self, p: V2) {
        self.t.track_in(&mut self.draft, p)
    }
    pub fn reset(&mut self) {
        self.t.reset_in(&mut self.draft)
    }
    pub fn toggle_arc(&mut self) {
        self.t.toggle_arc_in(&mut self.draft)
    }
    pub fn toggle_spline(&mut self) {
        self.t.toggle_spline_in(&mut self.draft)
    }
    pub fn snapped(&self) -> V2 {
        self.t.snapped_in(&self.draft)
    }
    pub fn preview(&self) -> Vec<(V2, V2)> {
        self.t.preview_in(&self.draft)
    }
    pub fn arc_preview(&self) -> Vec<V2> {
        self.t.arc_preview_of_in(&self.draft, &self.draft.points)
    }
    pub fn arc_preview_of(&self, pts: &[V2]) -> Vec<V2> {
        self.t.arc_preview_of_in(&self.draft, pts)
    }
    pub fn rubber_end(&self) -> V2 {
        self.t.rubber_end_in(&self.draft)
    }
    pub fn is_idle(&self) -> bool {
        self.t.is_idle_in(&self.draft)
    }
    pub fn is_drawing(&self) -> bool {
        self.t.is_drawing_in(&self.draft)
    }
}

#[cfg(test)]
impl std::ops::Deref for Rig {
    type Target = WallTool;
    fn deref(&self) -> &WallTool {
        &self.t
    }
}

#[cfg(test)]
impl std::ops::DerefMut for Rig {
    fn deref_mut(&mut self) -> &mut WallTool {
        &mut self.t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(t: &mut Rig, s: &str) -> Outcome {
        for c in s.chars() {
            t.handle(Event::Char(c));
        }
        t.handle(Event::Key(Key::Enter))
    }

    #[test]
    fn arc_three_points_bulge_one() {
        let mut t = Rig::new();
        t.handle(Event::Key(Key::Arc));
        assert!(t.arc);
        t.handle(Event::Click(v2(3.0, 0.0)));
        t.handle(Event::Click(v2(0.0, 3.0)));
        let Outcome::Arc { a, b, bulge } = t.handle(Event::Click(v2(-3.0, 0.0))) else {
            panic!("no arc")
        };
        close(a, v2(3.0, 0.0));
        close(b, v2(-3.0, 0.0));
        assert!((bulge - 1.0).abs() < 1e-9, "{bulge}");
        assert!(!t.is_drawing());
    }

    #[test]
    fn arc_collinear_is_error_and_radius_typed() {
        let mut t = Rig::new();
        t.toggle_arc();
        t.handle(Event::Click(v2(0.0, 0.0)));
        t.handle(Event::Click(v2(1.0, 0.0)));
        assert!(matches!(
            t.handle(Event::Click(v2(2.0, 0.0))),
            Outcome::Error(_)
        ));
        assert!(t.is_drawing());
        t.reset();
        t.handle(Event::Click(v2(0.0, 0.0)));
        t.handle(Event::Click(v2(6.0, 0.0)));
        t.handle(Event::Move(v2(3.0, -1.0)));
        let Outcome::Arc { bulge, .. } = typed(&mut t, "3000") else {
            panic!("no arc")
        };
        assert!((bulge.abs() - 1.0).abs() < 1e-9);
        t.handle(Event::Click(v2(0.0, 0.0)));
        t.handle(Event::Click(v2(6.0, 0.0)));
        assert!(matches!(typed(&mut t, "1000"), Outcome::Error(_)));
    }

    fn close(a: V2, b: V2) {
        assert!((a - b).len() < 1e-12, "{a:?} != {b:?}");
    }

    #[test]
    fn polyline_typed_room_and_close() {
        let mut t = Rig::new();
        t.handle(Event::Click(v2(0.0, 0.0)));
        t.handle(Event::Move(v2(2.0, 0.05)));
        assert_eq!(typed(&mut t, "6000"), Outcome::Nothing);
        t.handle(Event::Move(v2(6.02, 1.0)));
        typed(&mut t, "4000");
        t.handle(Event::Move(v2(3.0, 4.03)));
        typed(&mut t, "6000");
        close(t.draft.points[3], v2(0.0, 4.0));
        let Outcome::Commit(s) = t.handle(Event::Char('c')) else {
            panic!("no commit")
        };
        assert_eq!(s.len(), 4);
        close(s[3].1, v2(0.0, 0.0));
        assert!(!t.is_drawing());
    }

    #[test]
    fn leaving_a_tool_clears_the_snap_candidates() {
        let mut t = Rig::new();
        t.track(v2(0.0, 0.0));
        t.handle(Event::Click(v2(1.0, 0.0)));
        t.track(v2(1.0, 0.0));
        t.set_mode(Mode::Select);
        assert!(t.snaps.is_empty() && t.snap().is_none());
    }

    #[test]
    fn a_rejected_length_clears_the_buffer() {
        let mut t = Rig::new();
        t.handle(Event::Click(v2(0.0, 0.0)));
        for bad in ["0", "-500", "1000000000000", "abc"] {
            assert!(matches!(typed(&mut t, bad), Outcome::Error(_)), "{bad}");
            assert!(t.draft.input.is_empty(), "{bad}: left {:?}", t.draft.input);
            assert_eq!(t.draft.points.len(), 1, "{bad}: the start point stays");
        }
        t.handle(Event::Char('5'));
        assert_eq!(t.draft.input, "5", "digits after a rejection start fresh");
    }

    #[test]
    fn backspace_escape_and_errors() {
        let mut t = Rig::new();
        assert!(matches!(typed(&mut t, "3000"), Outcome::Error(_)));
        t.draft.input.clear();
        t.handle(Event::Click(v2(0.0, 0.0)));
        t.handle(Event::Click(v2(3.0, 0.0)));
        t.handle(Event::Click(v2(3.0, 2.0)));
        t.handle(Event::Key(Key::Backspace));
        assert_eq!(t.draft.points.len(), 2);
        assert!(matches!(typed(&mut t, "abc"), Outcome::Error(m) if m.contains(&crate::app2_fx::fx("wall_position_1"))));
        assert_eq!(t.draft.points.len(), 2, "bad input keeps the draft");
        // a rejected entry clears itself; Esc clears an entry still being typed
        assert!(t.draft.input.is_empty());
        t.handle(Event::Char('1'));
        t.handle(Event::Char('2'));
        t.handle(Event::Key(Key::Escape));
        assert!(t.draft.input.is_empty() && t.draft.points.len() == 2);
        assert!(matches!(t.handle(Event::Char('c')), Outcome::Error(_)));
        let Outcome::Commit(s) = t.handle(Event::Key(Key::Escape)) else {
            panic!()
        };
        assert_eq!(s, vec![(v2(0.0, 0.0), v2(3.0, 0.0))]);
        // one point + Esc: cancel without commit
        t.handle(Event::Click(v2(1.0, 1.0)));
        assert_eq!(t.handle(Event::Key(Key::Escape)), Outcome::Nothing);
        assert!(!t.is_drawing());
        // double click on the same point: an error, not a zero-length wall
        t.handle(Event::Click(v2(1.0, 1.0)));
        assert!(matches!(
            t.handle(Event::Click(v2(1.0, 1.0))),
            Outcome::Error(_)
        ));
    }

    #[test]
    fn angle_typed_and_locked() {
        let mut t = Rig::new();
        t.handle(Event::Click(v2(0.0, 0.0)));
        typed(&mut t, "2000<90");
        close(t.draft.points[1], v2(0.0, 2.0));
        typed(&mut t, "<0");
        assert_eq!(t.draft.lock_angle, Some(0.0));
        // 2.14 m off, within reach of the 2 m equal step: the lock still owns the point
        t.handle(Event::Move(v2(1.3, 3.7)));
        close(t.snapped(), v2(1.3, 2.0));
        assert!(t.snap().is_none_or(|s| s.kind != SnapKind::EqualStep));
        // 3 m from the last point: clear of the equal step (2 m) and of the 45-degree rays
        t.handle(Event::Move(v2(1.3, 4.7)));
        close(t.snapped(), v2(1.3, 2.0));
        typed(&mut t, "1500");
        close(t.draft.points[2], v2(1.5, 2.0));
        typed(&mut t, "@-1500;500");
        close(t.draft.points[3], v2(0.0, 2.5));
    }

    #[test]
    fn imperial_typed() {
        let mut t = Rig::new();
        t.units = Units::Imperial;
        t.handle(Event::Click(v2(0.0, 0.0)));
        t.handle(Event::Move(v2(5.0, 0.0)));
        typed(&mut t, "12'6\"");
        close(t.draft.points[1], v2(3.81, 0.0));
    }

    #[test]
    fn tab_cycles_snaps_and_click_closes_on_start() {
        let mut t = Rig::new();
        t.walls = vec![SnapWall {
            a: v2(0.0, 0.0),
            b: v2(4.0, 0.0),
            half: 0.1,
        }];
        t.handle(Event::Move(v2(2.02, 0.05)));
        assert_eq!(t.snap().unwrap().kind, SnapKind::Midpoint);
        t.handle(Event::Key(Key::Tab));
        let k = t.snap().map(|s| s.kind);
        assert_ne!(k, Some(SnapKind::Midpoint));
        for _ in 0..t.snaps.len() - 1 {
            t.handle(Event::Key(Key::Tab));
        }
        assert_eq!(t.snap().unwrap().kind, SnapKind::Midpoint);
        // closing by clicking the start point
        let mut t = Rig::new();
        for p in [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)] {
            t.handle(Event::Click(v2(p.0, p.1)));
        }
        assert!(
            matches!(t.handle(Event::Click(v2(0.03, 0.02))), Outcome::Commit(s) if s.len() == 3)
        );
    }

    #[test]
    fn rect_drag_and_typed() {
        let mut t = Rig::new();
        t.set_mode(Mode::Rect);
        t.handle(Event::Press(v2(0.0, 0.0)));
        assert_eq!(t.preview().len(), 0);
        t.handle(Event::Move(v2(3.0, 2.0)));
        assert_eq!(t.preview().len(), 4);
        let Outcome::Commit(s) = t.handle(Event::Release(v2(3.0, 2.0))) else {
            panic!()
        };
        assert_eq!(s.len(), 4);
        close(s[1].1, v2(3.0, 2.0));
        // click-click, then typed dimensions toward the cursor
        t.handle(Event::Press(v2(1.0, 1.0)));
        t.handle(Event::Release(v2(1.0, 1.0)));
        t.handle(Event::Move(v2(-2.0, 0.5)));
        // a comma is a decimal or a group in either language, never the size pair
        for mark in [DecimalMark::Point, DecimalMark::Comma] {
            t.mark = mark;
            assert!(matches!(typed(&mut t, "6000,4000"), Outcome::Error(_)), "{mark:?}");
            t.draft.input.clear();
        }
        let Outcome::Commit(s) = typed(&mut t, "6000;4000") else {
            panic!()
        };
        close(s[1].1, v2(-5.0, -3.0));
        t.handle(Event::Press(v2(1.0, 1.0)));
        assert!(matches!(typed(&mut t, "0;4000"), Outcome::Error(_)));
        assert!(matches!(typed(&mut t, "4000"), Outcome::Error(_)));
    }
}
