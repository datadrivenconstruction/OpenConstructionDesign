//! Door/Window tool without UI: cursor over a wall -> snapped position along the axis,
//! checks (outside the wall, above the wall, overlap), plan symbol.

use crate::snap::{closest_on_segment, dot};
use crate::units::{parse_length, Units};
use crate::wall_tool::{Event, Key};
use ddc_geom::detail::opening::{DoorType, Placement};
use ddc_geom::detail::plan::{door_symbol, PlanPrim};
use ddc_geom::detail::rules::DetailRules;
use ddc_geom::{v2, V2};
use std::fmt;

/// Position comparison tolerance, m.
const EPS: f64 = 1e-9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Door,
    Window,
    /// Empty opening: no infill.
    Void,
}

impl Kind {
    pub fn model_name(self) -> &'static str {
        match self {
            Kind::Door => "door",
            Kind::Window => "window",
            Kind::Void => "void",
        }
    }

    pub fn from_model(s: &str) -> Self {
        match s {
            "door" => Kind::Door,
            "void" => Kind::Void,
            _ => Kind::Window,
        }
    }
}

/// Parameters of the opening being placed, m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpeningType {
    pub kind: Kind,
    pub width: f64,
    pub height: f64,
    pub sill: f64,
}

impl OpeningType {
    pub fn door() -> Self {
        Self {
            kind: Kind::Door,
            width: 0.9,
            height: 2.1,
            sill: 0.0,
        }
    }

    pub fn window() -> Self {
        Self {
            kind: Kind::Window,
            width: 1.2,
            height: 1.4,
            sill: 0.9,
        }
    }

    pub fn void() -> Self {
        Self {
            kind: Kind::Void,
            width: 1.0,
            height: 2.1,
            sill: 0.0,
        }
    }
}

/// Host wall: axis a→b, height, half thickness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostWall {
    pub id: i64,
    pub a: V2,
    pub b: V2,
    pub height: f64,
    pub half: f64,
    /// Axis length along the arc: the opening's `along` is measured along it, as in the model.
    pub run: f64,
}

impl HostWall {
    pub fn len(&self) -> f64 {
        self.run
    }

    /// Chord length per unit of axis length; 1 for a straight wall.
    fn chord_per_run(&self) -> f64 {
        let c = (self.b - self.a).len();
        if self.run > EPS && c > EPS {
            c / self.run
        } else {
            1.0
        }
    }

    /// Position along the axis (from a) for a plan point.
    pub fn along_of(&self, p: V2) -> f64 {
        dot(p - self.a, self.dir()) / self.chord_per_run()
    }

    pub fn dir(&self) -> V2 {
        let l = (self.b - self.a).len();
        if l > EPS {
            (self.b - self.a) * (1.0 / l)
        } else {
            v2(1.0, 0.0)
        }
    }

    /// Left normal to the axis.
    pub fn normal(&self) -> V2 {
        let d = self.dir();
        v2(-d.y, d.x)
    }

    /// Point: `s` along the axis from a, `o` along the normal.
    pub fn at(&self, s: f64, o: f64) -> V2 {
        self.a + self.dir() * (s * self.chord_per_run()) + self.normal() * o
    }
}

/// Nearest empty opening along the ray, if closer than `nearer_than`: it has no body (you can
/// see through it), so picking uses the opening volume. Returns the opening's scene element.
pub fn void_under_ray(
    ed: &crate::editor::Editor,
    o: [f64; 3],
    d: [f64; 3],
    nearer_than: f64,
) -> Option<u32> {
    let mut best: Option<(f64, u32)> = None;
    for (k, op) in ed.openings.iter().enumerate() {
        if op.kind != Kind::Void {
            continue;
        }
        let Some(w) = ed.host_of(op.wall) else {
            continue;
        };
        let (p, v) = (v2(o[0], o[1]) - w.a, v2(d[0], d[1]));
        let (dir, nor, c) = (w.dir(), w.normal(), w.chord_per_run());
        let slabs = [
            (
                dot(p, dir),
                dot(v, dir),
                op.along * c,
                (op.along + op.width) * c,
            ),
            (dot(p, nor), dot(v, nor), -w.half, w.half),
            (o[2], d[2], op.sill, op.sill + op.height),
        ];
        let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
        for (at, step, lo, hi) in slabs {
            if step.abs() < 1e-12 {
                if at < lo || at > hi {
                    t1 = -1.0;
                }
                continue;
            }
            let (a, b) = ((lo - at) / step, (hi - at) / step);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
        if t0 <= t1 && t0 < nearer_than && best.is_none_or(|b| t0 < b.0) {
            best = Some((t0, (ed.walls.len() + k) as u32));
        }
    }
    best.map(|b| b.1)
}

/// 3D click: an empty opening in front of the hit body takes the selection.
pub fn void_click(
    ed: &crate::editor::Editor,
    view: &mut crate::viewport::Viewport,
    click: Option<Option<u32>>,
) -> Option<Option<u32>> {
    let (Some(_), Some((o, d))) = (click, view.click_ray) else {
        return click;
    };
    let wide = |v: [f32; 3]| v.map(f64::from);
    let hit_at = view.selected.map_or(f64::INFINITY, |h| f64::from(h.t));
    match void_under_ray(ed, wide(o), wide(d), hit_at) {
        Some(el) => {
            view.selected = None;
            view.highlight_one(Some((el, el + 1)));
            Some(Some(el))
        }
        None => click,
    }
}

/// Existing opening (from the model) and its side/hinges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub id: i64,
    pub wall: i64,
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
    pub kind: Kind,
    pub flip: Flip,
}

/// Swing side (Space) and hinge side (F).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flip {
    pub side: bool,
    pub hinge: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlongSnap {
    Center,
    FromStart(f64),
    FromEnd(f64),
    Grid,
    Typed,
}

impl AlongSnap {
    pub fn label(self) -> String {
        self.label_in(crate::ui_is_ru(), crate::units::active_units())
    }

    pub fn label_in(self, ru: bool, u: Units) -> String {
        let at = |k: &str, o: f64| {
            crate::tr_lang(ru, k, &[("o", &crate::unit_text::len(o, ru, u))])
        };
        match self {
            AlongSnap::Center => crate::tr_lang(ru, "sketch_edit.along_center", &[]),
            AlongSnap::FromStart(o) => at("sketch_edit.along_start", o),
            AlongSnap::FromEnd(o) => at("sketch_edit.along_end", o),
            AlongSnap::Grid => crate::tr_lang(ru, "sketch_edit.along_grid", &[]),
            AlongSnap::Typed => crate::tr_lang(ru, "sketch_edit.along_typed", &[]),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaceError {
    NoWall,
    NonPositive,
    Outside { len: f64, from: f64, to: f64 },
    TooTall { top: f64, height: f64 },
    Overlap { other: i64 },
    /// A wall edit after which hosted opening `opening` no longer fits (`err` says how).
    Hosts { opening: i64, err: Box<PlaceError> },
}

impl PlaceError {
    /// Error text in the UI language; lengths in project units.
    pub fn text(&self, ru: bool, u: Units) -> String {
        let l = |m: f64| crate::unit_text::len(m, ru, u);
        match self {
            PlaceError::NoWall => crate::tr_lang(ru, "sketch_edit.pe_nowall", &[]),
            PlaceError::NonPositive => crate::tr_lang(ru, "sketch_edit.pe_nonpos", &[]),
            PlaceError::Outside { len, from, to } => crate::tr_lang(
                ru,
                "sketch_edit.pe_outside",
                &[
                    ("wall", &format!("{}…{}", l(0.0), l(*len))),
                    ("need", &format!("{}…{}", l(*from), l(*to))),
                ],
            ),
            PlaceError::TooTall { top, height } => {
                crate::tr_lang(ru, "sketch_edit.pe_tall", &[("top", &l(*top)), ("h", &l(*height))])
            }
            PlaceError::Overlap { other } => {
                crate::tr_lang(ru, "sketch_edit.pe_overlap", &[("other", &other.to_string())])
            }
            PlaceError::Hosts { opening, err } => crate::tr_lang(
                ru,
                "xlval.pe_hosts",
                &[("id", &opening.to_string()), ("e", &err.text(ru, u))],
            ),
        }
    }
}

impl fmt::Display for PlaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.text(crate::ui_is_ru(), crate::units::active_units())
        )
    }
}

/// Validate an opening [along, along+width] × [sill, sill+height] on a wall; `skip` is the opening itself when editing.
pub fn validate(
    w: &HostWall,
    others: &[Placed],
    skip: Option<i64>,
    along: f64,
    t: &OpeningType,
) -> Result<(), PlaceError> {
    let finite = [along, t.width, t.height, t.sill]
        .iter()
        .all(|v| v.is_finite());
    if !finite || t.width <= 0.0 || t.height <= 0.0 || t.sill < 0.0 {
        return Err(PlaceError::NonPositive);
    }
    let len = w.len();
    if along < -EPS || along + t.width > len + EPS {
        return Err(PlaceError::Outside {
            len,
            from: along,
            to: along + t.width,
        });
    }
    if t.sill + t.height > w.height + EPS {
        return Err(PlaceError::TooTall {
            top: t.sill + t.height,
            height: w.height,
        });
    }
    let hit = others.iter().find(|o| {
        o.wall == w.id
            && Some(o.id) != skip
            && along < o.along + o.width - EPS
            && o.along < along + t.width - EPS
    });
    match hit {
        Some(o) => Err(PlaceError::Overlap { other: o.id }),
        None => Ok(()),
    }
}

/// Nearest wall within the radius: (index, projection along the axis from a).
pub fn nearest_wall(walls: &[HostWall], p: V2, radius: f64) -> Option<(usize, f64)> {
    walls
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let (q, _) = closest_on_segment(p, w.a, w.b);
            (i, (q - p).len() - w.half, w.along_of(q))
        })
        .filter(|(_, d, _)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _, s)| (i, s))
}

/// Position along the axis for a cursor at `s`: center > from the end with an offset > `step` grid.
pub fn snap_along(
    len: f64,
    s: f64,
    width: f64,
    offset: f64,
    step: f64,
    radius: f64,
) -> (f64, AlongSnap) {
    let free = s - width / 2.0;
    let center = (len - width) / 2.0;
    let cands = [
        (center, AlongSnap::Center),
        (offset, AlongSnap::FromStart(offset)),
        (len - offset - width, AlongSnap::FromEnd(offset)),
    ];
    let best = cands
        .iter()
        .map(|&(a, k)| ((a - free).abs(), a, k))
        .filter(|c| c.0 <= radius)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    match best {
        Some((_, a, k)) => (a, k),
        None if step > 0.0 => ((free / step).round() * step, AlongSnap::Grid),
        None => (free, AlongSnap::Grid),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub wall: usize,
    pub along: f64,
    pub snap: AlongSnap,
    pub error: Option<PlaceError>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpOutcome {
    Nothing,
    /// Place an opening on wall `wall` (id) at `along`.
    Place {
        wall: i64,
        along: f64,
        flip: Flip,
    },
    /// Esc on empty input exits the tool.
    Exit,
    Error(String),
}

pub struct OpeningTool {
    pub ty: OpeningType,
    pub units: Units,
    pub input: String,
    pub cursor: V2,
    /// "From end" snap offset, m.
    pub end_offset: f64,
    pub step: f64,
    pub radius: f64,
    pub flip: Flip,
    pub walls: Vec<HostWall>,
    pub openings: Vec<Placed>,
    pub preview: Option<Preview>,
}

impl Default for OpeningTool {
    fn default() -> Self {
        Self::new()
    }
}

impl OpeningTool {
    pub fn new() -> Self {
        Self {
            ty: OpeningType::door(),
            units: Units::Metric,
            input: String::new(),
            cursor: v2(0.0, 0.0),
            end_offset: 0.25,
            step: 0.1,
            radius: 0.15,
            flip: Flip::default(),
            walls: vec![],
            openings: vec![],
            preview: None,
        }
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.preview = None;
    }

    /// Recompute the preview under the cursor; `skip` is the opening being dragged.
    pub fn update(&mut self, p: V2, skip: Option<i64>) {
        self.cursor = p;
        self.preview = nearest_wall(&self.walls, p, self.radius).map(|(i, s)| {
            let w = &self.walls[i];
            let (along, snap) = snap_along(
                w.len(),
                s,
                self.ty.width,
                self.end_offset,
                self.step,
                self.radius,
            );
            let error = validate(w, &self.openings, skip, along, &self.ty).err();
            Preview {
                wall: i,
                along,
                snap,
                error,
            }
        });
    }

    fn commit(&self) -> OpOutcome {
        match &self.preview {
            None => OpOutcome::Error(PlaceError::NoWall.to_string()),
            Some(Preview { error: Some(e), .. }) => OpOutcome::Error(e.to_string()),
            Some(pv) => OpOutcome::Place {
                wall: self.walls[pv.wall].id,
                along: pv.along,
                flip: self.flip,
            },
        }
    }

    /// Typing `1000` Enter: offset of the opening edge from the wall end nearest the cursor.
    fn apply_typed(&mut self) -> OpOutcome {
        let parsed = parse_length(&self.input, self.units);
        let Some(pv) = self.preview.clone() else {
            return OpOutcome::Error(PlaceError::NoWall.to_string());
        };
        let off = match parsed {
            Ok(v) => v,
            Err(e) => return OpOutcome::Error(e.to_string()),
        };
        self.input.clear();
        let w = self.walls[pv.wall];
        let s = w.along_of(self.cursor);
        let along = if s <= w.len() / 2.0 {
            off
        } else {
            w.len() - off - self.ty.width
        };
        let error = validate(&w, &self.openings, None, along, &self.ty).err();
        self.preview = Some(Preview {
            wall: pv.wall,
            along,
            snap: AlongSnap::Typed,
            error,
        });
        self.commit()
    }

    pub fn handle(&mut self, e: Event) -> OpOutcome {
        match e {
            Event::Move(p) | Event::Press(p) | Event::Release(p) => {
                self.update(p, None);
                OpOutcome::Nothing
            }
            Event::Click(p) => {
                self.update(p, None);
                self.commit()
            }
            Event::Char(' ') => {
                self.flip.side = !self.flip.side;
                OpOutcome::Nothing
            }
            Event::Char('f' | 'F' | '\u{430}' | '\u{410}') if self.input.is_empty() => {
                self.flip.hinge = !self.flip.hinge;
                OpOutcome::Nothing
            }
            Event::Char(c) => {
                if !c.is_control() && self.input.len() < 64 {
                    self.input.push(c);
                }
                OpOutcome::Nothing
            }
            Event::Key(Key::Backspace) => {
                self.input.pop();
                OpOutcome::Nothing
            }
            Event::Key(Key::Escape) if self.input.is_empty() => OpOutcome::Exit,
            Event::Key(Key::Escape) => {
                self.input.clear();
                OpOutcome::Nothing
            }
            Event::Key(Key::Enter) if !self.input.is_empty() => self.apply_typed(),
            Event::Key(_) => OpOutcome::Nothing,
        }
    }
}

/// Opening symbol in plan: closed opening outline in the wall (`gap`) and symbol lines.
pub struct Symbol {
    pub gap: Vec<V2>,
    pub lines: Vec<Vec<V2>>,
}

/// Door: leaf perpendicular to the wall from the hinge + 90° arc; window: two glass lines.
pub fn symbol(w: &HostWall, along: f64, width: f64, kind: Kind, flip: Flip, rules: &DetailRules) -> Symbol {
    let h = w.half;
    let (s0, s1) = (along, along + width);
    let gap = vec![w.at(s0, -h), w.at(s1, -h), w.at(s1, h), w.at(s0, h)];
    let mut lines = vec![
        vec![w.at(s0, -h), w.at(s0, h)],
        vec![w.at(s1, -h), w.at(s1, h)],
    ];
    match kind {
        Kind::Void => {}
        Kind::Window => {
            lines.push(vec![w.at(s0, -h / 3.0), w.at(s1, -h / 3.0)]);
            lines.push(vec![w.at(s0, h / 3.0), w.at(s1, h / 3.0)]);
        }
        Kind::Door => {
            // the same type the 3D fill is built from: leaves open at 90° from the frame, arcs
            let t = DoorType::for_width(width, rules);
            let p = Placement {
                width,
                height: 2.1,
                wall_t: 2.0 * h,
                swing_flip: flip.side,
                hinge_flip: flip.hinge,
            };
            let at = |q: V2| w.at(s0 + q.x, q.y);
            for g in door_symbol(&t, &p) {
                lines.push(match g {
                    PlanPrim::Line(a, b) => vec![at(a), at(b)],
                    PlanPrim::Arc { c, r, a0, a1 } => (0..=24)
                        .map(|k| {
                            let a = a0 + (a1 - a0) * k as f64 / 24.0;
                            at(c + v2(r * a.cos(), r * a.sin()))
                        })
                        .collect(),
                });
            }
        }
    }
    Symbol { gap, lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall() -> HostWall {
        HostWall {
            id: 7,
            a: v2(0.0, 0.0),
            b: v2(6.0, 0.0),
            height: 3.0,
            half: 0.1,
            run: 6.0,
        }
    }

    #[test]
    fn arc_host_is_measured_along_the_arc() {
        let w = HostWall { run: 7.0, ..wall() };
        assert_eq!(w.len(), 7.0);
        assert!((w.at(7.0, 0.0) - w.b).len() < 1e-12);
        assert!((w.along_of(w.b) - 7.0).abs() < 1e-12);
        let ty = OpeningType {
            width: 0.5,
            ..OpeningType::void()
        };
        assert!(
            validate(&w, &[], None, 6.2, &ty).is_ok(),
            "past the chord, on the arc"
        );
        assert!(matches!(
            validate(&w, &[], None, 6.6, &ty),
            Err(PlaceError::Outside { .. })
        ));
    }

    fn tool() -> OpeningTool {
        let mut t = OpeningTool::new();
        t.walls = vec![wall()];
        t
    }

    #[test]
    fn snaps_center_end_and_grid() {
        let (a, k) = snap_along(6.0, 3.05, 0.9, 0.25, 0.1, 0.15);
        assert_eq!(k, AlongSnap::Center);
        assert!((a - 2.55).abs() < 1e-12);
        let (a, k) = snap_along(6.0, 0.75, 0.9, 0.25, 0.1, 0.15);
        assert_eq!(k, AlongSnap::FromStart(0.25));
        assert!((a - 0.25).abs() < 1e-12);
        let (a, k) = snap_along(6.0, 5.3, 0.9, 0.25, 0.1, 0.15);
        assert_eq!(k, AlongSnap::FromEnd(0.25));
        assert!((a - 4.85).abs() < 1e-12);
        let (a, k) = snap_along(6.0, 2.03, 0.9, 0.25, 0.1, 0.15);
        assert_eq!(k, AlongSnap::Grid);
        assert!((a - 1.6).abs() < 1e-12, "{a}");
    }

    #[test]
    fn validate_errors_name_numbers() {
        let w = wall();
        let d = OpeningType::door();
        assert!(validate(&w, &[], None, 0.0, &d).is_ok());
        assert!(validate(&w, &[], None, 5.1, &d).is_ok());
        let e = validate(&w, &[], None, 5.2, &d).unwrap_err();
        assert!(matches!(e, PlaceError::Outside { .. }));
        assert!(e.text(false, Units::Metric).contains("6,000"), "{e}");
        assert!(e.text(true, Units::Metric).contains("6\u{a0}000"), "{e}");
        assert!(validate(&w, &[], None, -0.01, &d).is_err());
        let tall = OpeningType {
            height: 2.5,
            sill: 0.6,
            ..d
        };
        let e = validate(&w, &[], None, 1.0, &tall).unwrap_err();
        assert_eq!(
            e,
            PlaceError::TooTall {
                top: 3.1,
                height: 3.0
            }
        );
        let bad = OpeningType { width: 0.0, ..d };
        assert_eq!(
            validate(&w, &[], None, 1.0, &bad),
            Err(PlaceError::NonPositive)
        );
        let nan = OpeningType {
            width: f64::NAN,
            ..d
        };
        assert_eq!(
            validate(&w, &[], None, 1.0, &nan),
            Err(PlaceError::NonPositive)
        );
        let other = Placed {
            id: 3,
            wall: 7,
            along: 2.0,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
            kind: Kind::Door,
            flip: Flip::default(),
        };
        let os = [other];
        assert_eq!(
            validate(&w, &os, None, 2.5, &d),
            Err(PlaceError::Overlap { other: 3 })
        );
        // touching edges is not an overlap; the opening itself is skipped when editing
        assert!(validate(&w, &os, None, 3.0, &d).is_ok());
        assert!(validate(&w, &os, None, 1.1, &d).is_ok());
        assert!(validate(&w, &os, Some(3), 2.5, &d).is_ok());
        // an opening on another wall does not interfere
        let other_wall = Placed { wall: 8, ..other };
        assert!(validate(&w, &[other_wall], None, 2.5, &d).is_ok());
    }

    #[test]
    fn tool_click_typed_flip_and_no_wall() {
        let mut t = tool();
        assert_eq!(
            t.handle(Event::Click(v2(3.0, 2.0))),
            OpOutcome::Error(PlaceError::NoWall.to_string())
        );
        t.handle(Event::Char(' '));
        t.handle(Event::Char('F'));
        assert_eq!(
            t.flip,
            Flip {
                side: true,
                hinge: true
            }
        );
        match t.handle(Event::Click(v2(3.02, 0.05))) {
            OpOutcome::Place { wall, along, flip } => {
                assert_eq!(wall, 7);
                assert!((along - 2.55).abs() < 1e-12);
                assert!(flip.side && flip.hinge);
            }
            o => panic!("{o:?}"),
        }
        // number measured from the nearest end: cursor is at end b
        t.handle(Event::Move(v2(5.5, 0.0)));
        for c in "1000".chars() {
            t.handle(Event::Char(c));
        }
        match t.handle(Event::Key(Key::Enter)) {
            OpOutcome::Place { along, .. } => assert!((along - (6.0 - 1.0 - 0.9)).abs() < 1e-12),
            o => panic!("{o:?}"),
        }
        assert!(t.input.is_empty());
        for c in "abc".chars() {
            t.handle(Event::Char(c));
        }
        assert!(matches!(
            t.handle(Event::Key(Key::Enter)),
            OpOutcome::Error(_)
        ));
        t.handle(Event::Key(Key::Escape));
        assert!(t.input.is_empty());
        assert_eq!(t.handle(Event::Key(Key::Escape)), OpOutcome::Exit);
    }

    #[test]
    fn door_symbol_swings_to_flipped_side() {
        let w = wall();
        let r = DetailRules::default();
        // the leaf hangs in the frame: hinge 45 mm in from the jamb, leaf as wide as the clear opening
        let ends = |l: &Vec<V2>, a: V2, b: V2| {
            let (p, q) = (l[0], *l.last().unwrap());
            ((p - a).len() < 1e-9 && (q - b).len() < 1e-9) || ((p - b).len() < 1e-9 && (q - a).len() < 1e-9)
        };
        let s = symbol(&w, 1.0, 0.9, Kind::Door, Flip::default(), &r);
        assert_eq!(s.gap.len(), 4);
        assert!(ends(&s.lines[2], v2(1.045, 0.1), v2(1.045, 0.91)), "{:?}", s.lines[2]);
        assert!(ends(&s.lines[3], v2(1.045, 0.91), v2(1.855, 0.1)));
        // a true quarter circle about the hinge, not a chord
        for p in &s.lines[3] {
            assert!(((*p - v2(1.045, 0.1)).len() - 0.81).abs() < 1e-9);
        }
        let d = symbol(&w, 1.0, 1.6, Kind::Door, Flip::default(), &r);
        assert_eq!(d.lines.len(), 6, "jambs, two leaves, two arcs");
        assert!(ends(&d.lines[3], v2(1.045, 0.855), v2(1.8, 0.1)));
        assert!(ends(&d.lines[5], v2(2.555, 0.855), v2(1.8, 0.1)));
        let f = symbol(&w, 1.0, 0.9, Kind::Door, Flip { side: true, hinge: true }, &r);
        assert!(ends(&f.lines[2], v2(1.855, -0.1), v2(1.855, -0.91)));
        assert!(ends(&f.lines[3], v2(1.855, -0.91), v2(1.045, -0.1)));
        // the leaf count follows the rule set
        let wide = DetailRules { door_double_from: 2.0, ..r };
        assert_eq!(symbol(&w, 1.0, 1.6, Kind::Door, Flip::default(), &wide).lines.len(), 4);
        let win = symbol(&w, 1.0, 1.2, Kind::Window, Flip::default(), &r);
        assert_eq!(win.lines.len(), 4);
    }

    #[test]
    fn degenerate_wall_no_panic() {
        let mut t = tool();
        t.walls.push(HostWall {
            id: 9,
            a: v2(10.0, 10.0),
            b: v2(10.0, 10.0),
            height: 3.0,
            half: 0.1,
            run: 0.0,
        });
        t.handle(Event::Move(v2(10.0, 10.0)));
        assert!(t.preview.as_ref().is_some_and(|p| p.error.is_some()));
    }
}
