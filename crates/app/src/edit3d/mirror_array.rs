//! Mirror and Array in the 3D view. Both reuse the plan operations of the model
//! (`mirror_elements`, `array_linear`, `array_radial`), so hosted-element rules, groups and
//! undo are those of the plan: one gesture is one undo step, and a family without the
//! operation is refused with the reason from the element table.
//!
//! Mirror: click a grid line or a straight wall to use it as the axis, or click two points on
//! the work plane (hold Alt to skip the grid and wall pick). Array: click the base point (linear)
//! or the centre (radial), then move or type. Typed grammar: `value`, `count value` or
//! `count;value` ([`read_typed`]); the value is the step, the total length (option "Total
//! length") or the sweep angle in degrees. Refused text shows why and applies nothing.

use std::f64::consts::TAU;
use std::sync::Arc;

use ddc_model::{Error, Model, Spacing};
use ddc_render::glam::Vec3;
use ddc_render::Scene;
use ddc_ui::numparse::{self, DecimalMark, NumError};
use egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use super::grips::{opening_row, wall_spec};
use super::math::Cam;
use super::pick::{Bodies, Cat, Target};
use super::snap::SnapKind;
use super::xform::Xform;
use super::{fmt_len, paint, parse_value, tr, Edit3d, Tool};
use crate::editor::{Editor, OpFn};
use crate::modify_opts as mo;
use crate::units::Units;
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, field::TextField, Button};
use ddc_ui::Theme;
use crate::elem::{self, Cap};
use crate::shell::EditorApp;

/// Most placements an array takes (the same limit as in the plan).
pub const MAX_COUNT: usize = 200;
/// Ghost copies drawn at most; the last copy is always among them.
const GHOST_MAX: usize = 48;
/// Edge budget of one ghost frame.
const GHOST_EDGES: usize = 40_000;
/// Screen distance at which a grid line is picked as the mirror axis.
const PICK_PX: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayKind {
    Linear,
    Radial,
}

/// What the user sees refused or applied: targets the operation takes and those it leaves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub ok: Vec<(Target, ddc_model::Target)>,
    /// Category and the i18n key of the reason.
    pub skipped: Vec<(Cat, &'static str)>,
}

struct Cache {
    rev: u64,
    mirror: bool,
    sel: Vec<Target>,
    plan: Arc<Plan>,
}

/// Tool state kept between frames; options survive between uses.
pub struct State {
    pub kind: ArrayKind,
    pub count: usize,
    /// Linear: the second point is the last placement (total length) instead of the next one.
    pub fit: bool,
    /// Mirror: keep the original.
    pub keep: bool,
    pts: Vec<Vec3>,
    /// Value typed in the option bar.
    val: String,
    last: Option<Vec3>,
    cache: Option<Cache>,
    bar: Option<Rect>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            kind: ArrayKind::Linear,
            count: 4,
            fit: false,
            keep: true,
            pts: vec![],
            val: String::new(),
            last: None,
            cache: None,
            bar: None,
        }
    }
}

impl State {
    /// Drops points and typed text, keeps the options.
    pub fn clear(&mut self) {
        self.pts.clear();
        self.val.clear();
        self.last = None;
        self.bar = None;
    }

    /// Esc: forget the first point or the typed text; false when there was nothing.
    pub fn clear_points(&mut self) -> bool {
        let had = !self.pts.is_empty() || !self.val.is_empty();
        self.pts.clear();
        self.val.clear();
        had
    }

    fn refresh(&mut self, m: &Model, sel: &[Target], mirror: bool, rev: u64) -> Arc<Plan> {
        if let Some(c) = &self.cache {
            if c.rev == rev && c.mirror == mirror && c.sel == sel {
                return c.plan.clone();
            }
        }
        let plan = Arc::new(classify(m, sel, mirror));
        self.cache = Some(Cache {
            rev,
            mirror,
            sel: sel.to_vec(),
            plan: plan.clone(),
        });
        plan
    }
}

// ---- which targets take part ----

/// Plan target of a 3D target, as the plan tools see it. A curtain panel never stands for its
/// wall.
pub fn model_target(t: Target) -> Option<ddc_model::Target> {
    if t.cat == Cat::Panel {
        return None;
    }
    t.modify()
        .or_else(|| t.sel().and_then(crate::modify_tools::target))
}

fn host_selected(m: &Model, t: Target, sel: &[Target]) -> bool {
    let has_wall = |w: i64| sel.iter().any(|s| s.cat == Cat::Wall && s.id == w);
    match t.cat {
        Cat::Opening => opening_row(m, t.id).is_ok_and(|(w, _)| has_wall(w)),
        Cat::Comp => matches!(
            m.component(t.id).map(|c| c.host),
            Ok(ddc_model::CompHost::Wall(w)) if has_wall(w)
        ),
        _ => false,
    }
}

/// Splits a selection into targets the operation takes and those it leaves with a reason.
/// A hosted element goes with its host wall when that wall is in the same selection; the
/// model then keeps it on the copy.
pub fn classify(m: &Model, sel: &[Target], mirror: bool) -> Plan {
    let mut p = Plan::default();
    for &t in sel {
        if t.cat == Cat::Panel {
            p.skipped.push((t.cat, "x3d.no.panel"));
            continue;
        }
        let Some(mt) = model_target(t) else {
            p.skipped.push((t.cat, elem::NOT_YET));
            continue;
        };
        let cap = elem::of_cat(t.cat).map(|f| if mirror { f.xf.mirror } else { f.xf.array });
        match cap {
            Some(Cap::No(r)) if !host_selected(m, t, sel) => p.skipped.push((t.cat, r)),
            _ => p.ok.push((t, mt)),
        }
    }
    p
}

/// "Category: reason" for every skipped category, without repeats.
pub fn why(ru: bool, skipped: &[(Cat, &'static str)]) -> String {
    let mut out: Vec<String> = vec![];
    for (c, r) in skipped {
        let s = format!(
            "{}: {}",
            tr(ru, &format!("e3d.cat.{}", c.key())),
            tr(ru, r)
        );
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.join("; ")
}

// ---- model request ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    Mirror {
        a: (f64, f64),
        b: (f64, f64),
        copy: bool,
    },
    /// `v` is the step, or the path to the last placement when `fit`.
    Linear {
        count: usize,
        v: (f64, f64),
        fit: bool,
    },
    Radial {
        count: usize,
        c: (f64, f64),
        total: f64,
    },
}

/// One model operation (one journal entry, so one undo step).
pub fn run(m: &mut Model, ts: &[ddc_model::Target], r: &Request) -> Result<(), Error> {
    let count_ok = |n: usize| -> Result<(), Error> {
        if n < 2 {
            return Err(Error::Invalid("e3d.err.array_count".into()));
        }
        if n > MAX_COUNT {
            return Err(Error::Invalid("e3d.err.array_max".into()));
        }
        Ok(())
    };
    match *r {
        Request::Mirror { a, b, copy } => m.mirror_elements(ts, a, b, copy).map(|_| ()),
        Request::Linear { count, v, fit } => {
            count_ok(count)?;
            let sp = if fit {
                Spacing::Fit(v)
            } else {
                Spacing::Step(v)
            };
            m.array_linear(ts, count, sp).map(|_| ())
        }
        Request::Radial { count, c, total } => {
            count_ok(count)?;
            m.array_radial(ts, count, c, total).map(|_| ())
        }
    }
}

/// Top-left of the option bar: the shared canvas bar row 1, below the view bar in row 0.
fn bar_origin(rect: Rect) -> Pos2 {
    crate::ui_kit::canvas_bar_pos(rect, 1)
}

/// Why the value typed in the bar cannot be used, in the UI language; `None` when it is empty
/// or readable. Same reading as `ma_typed`: `value` or `count value`.
fn reason_for(
    val: &str,
    radial: bool,
    ru: bool,
    parse: &dyn Fn(&str, bool) -> Option<f32>,
) -> Option<String> {
    if val.trim().is_empty() {
        return None;
    }
    let (n, rest) = split_typed(val);
    let count_ok = |t: &str| {
        !t.is_empty()
            && t.bytes().all(|b| b.is_ascii_digit())
            && t.parse::<usize>().is_ok_and(|c| (2..=MAX_COUNT).contains(&c))
    };
    if n.is_some_and(|t| !count_ok(t)) {
        let (lo, hi) = (2.to_string(), MAX_COUNT.to_string());
        return Some(crate::tr_lang(ru, "offset_opts.count_bad", &[("min", &lo), ("max", &hi)]));
    }
    if !rest.is_empty() && parse(rest, radial).is_none() {
        let key = if radial { "x3d.bad_angle" } else { "length_input.opt.bad_length" };
        return Some(crate::tr_lang(ru, key, &[]));
    }
    None
}

/// What the option bar reports for one frame.
struct BarOut {
    apply: bool,
    cancel: bool,
    /// Apply can be pressed: the typed value is readable.
    apply_ok: bool,
    reason: Option<String>,
}

/// The option bar from kit widgets, laid out like the plan option bars (same frame, row
/// spacing, control height, field width and button kinds).
fn bar_ui(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    units: Units,
    mirror: bool,
    ma: &mut State,
    parse: &dyn Fn(&str, bool) -> Option<f32>,
) -> BarOut {
    let t = |k: &str| tr(ru, k);
    let (mut apply, mut cancel, mut enter) = (false, false, false);
    let mut reason: Option<String> = None;
    let shown = reason_for(&ma.val, ma.kind == ArrayKind::Radial, ru, parse);
    mo::bar_frame(th).show(ui, |ui| {
        ui.horizontal(|ui| {
            mo::row_style(ui, th);
            if mirror {
                widgets::checkbox(ui, th, &mut ma.keep, &t("x3d.keep"));
            } else {
                let (lin, rad) = (t("x3d.linear"), t("mt.radial"));
                let sel = usize::from(ma.kind == ArrayKind::Radial);
                if let Some(i) = widgets::segmented(ui, th, &[lin.as_str(), rad.as_str()], sel) {
                    ma.kind = if i == 0 { ArrayKind::Linear } else { ArrayKind::Radial };
                }
                widgets::vdivider(ui, th, th.d().control);
                mo::count_field(ui, th, ru, &mut ma.count);
                let radial = ma.kind == ArrayKind::Radial;
                if !radial {
                    widgets::checkbox(ui, th, &mut ma.fit, &t("x3d.to_last"));
                }
                let label = match (radial, ma.fit) {
                    (true, _) => t("x3d.angle"),
                    (false, true) => t("x3d.total"),
                    (false, false) => t("x3d.step"),
                };
                let unit = match (radial, units) {
                    (false, Units::Metric) => crate::tr_lang(ru, "unit_fmt.unit.mm", &[]),
                    _ => String::new(),
                };
                let lr = widgets::elided_label(
                    ui,
                    &label,
                    th.font(Role::Label),
                    th.sem().text_secondary,
                );
                let err = shown.clone().unwrap_or_default();
                let out = TextField::new(&mut ma.val)
                    .id_salt("ma_val_field")
                    .labelled_by(lr.id, &label)
                    .error_text(&err)
                    .error(shown.is_some())
                    .unit(&unit)
                    .right_aligned()
                    .width(control::FIELD_MIN_W + space::XXL)
                    .show(ui, th);
                enter = out.committed
                    && !out.cancelled
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            // read again after this frame's edit: Apply must not take text it cannot read
            reason = reason_for(&ma.val, ma.kind == ArrayKind::Radial, ru, parse);
            let ok = reason.is_none();
            apply = enter && ok;
            widgets::vdivider(ui, th, th.d().control);
            let primary = ui
                .add_enabled_ui(ok, |ui| {
                    Button::primary(&t("x3d.apply")).small().tooltip("", "Enter").show(ui, th)
                })
                .inner;
            apply |= primary.clicked();
            if let Some(why) = &reason {
                primary.on_disabled_hover_text(why);
            }
            cancel = Button::quiet(&t("x3d.cancel"))
                .small()
                .tooltip("", "Esc")
                .show(ui, th)
                .clicked();
        });
        if let Some(why) = &reason {
            widgets::elided_label(ui, why, th.font(Role::Caption), th.sem().error);
        }
    });
    BarOut { apply, cancel, apply_ok: reason.is_none(), reason }
}

/// True when the hovered wall is one of the elements being mirrored.
fn axis_wall_selected(hover: Option<Target>, plan: &Plan) -> bool {
    hover.is_some_and(|h| h.cat == Cat::Wall && plan.ok.iter().any(|(t, _)| t.cat == h.cat && t.id == h.id))
}

// ---- geometry of the preview (pure) ----

pub struct Geo {
    pub req: Request,
    /// One transform per placement after the original.
    pub ghosts: Vec<Xform>,
    /// Step or total length in metres, or the sweep in radians.
    pub value: f64,
}

/// "value", "count value" or "count;value", split into the count text and the value text.
/// A 1-3 digit head followed by a space and exactly three digits groups thousands (`3 000`,
/// `1 200 mm`): that is one value, never a count; `4;300` still types a count with such a step.
pub fn split_typed(s: &str) -> (Option<&str>, &str) {
    let s = s.trim();
    if let Some((a, b)) = s.split_once(';') {
        return (Some(a.trim()), b.trim());
    }
    let gap = |c: char| c.is_whitespace() || numparse::is_group_space(c);
    let Some((a, b)) = s.split_once(gap) else {
        return (None, s);
    };
    let b = b.trim_matches(gap);
    let lead = b.bytes().take_while(u8::is_ascii_digit).count();
    let digits = !a.is_empty() && a.bytes().all(|c| c.is_ascii_digit());
    let group = digits && a.len() <= 3 && !a.starts_with('0') && lead == 3;
    // `6 1/2"` is six and a half inches, not a count of 6: a fraction follows
    let fraction = b.split(gap).next().is_some_and(|w| w.contains('/'));
    // only number-shaped heads before a number are counts: `1.5 3000` is a bad count,
    // `12 ft 6 in` and `2' 6"` stay one value
    let numeric = a.chars().any(|c| c.is_ascii_digit())
        && a.chars().all(|c| c.is_ascii_digit() || ".,+-\u{2212}".contains(c));
    let next_is_number = b.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == ',');
    if numeric && next_is_number && !group && !fraction {
        return (Some(a), b);
    }
    (None, s)
}

/// Why typed array input is refused; nothing is applied while it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedErr {
    /// The count is not a whole number from 2 to [`MAX_COUNT`].
    Count,
    /// English comma that is not a thousands group (`1,5`).
    CommaDecimal,
    /// The step or total is not a positive length.
    Length,
    /// The sweep is not an angle in (0, 360] degrees (either sign).
    Angle,
}

impl TypedErr {
    pub fn key(self) -> &'static str {
        match self {
            Self::Count => "mirror_array.err.count",
            Self::CommaDecimal => "decimal_input.comma_decimal",
            Self::Length => "mirror_array.err.length",
            Self::Angle => "mirror_array.err.angle",
        }
    }
}

/// Typed array input read with an explicit decimal mark: the count and the value (metres, or
/// degrees when `radial`). Empty text is no input; any invalid part refuses the whole text.
pub fn read_typed(
    s: &str,
    radial: bool,
    u: crate::units::Units,
    mark: DecimalMark,
) -> Result<(Option<usize>, Option<f64>), TypedErr> {
    let s = s.trim();
    if s.is_empty() {
        return Ok((None, None));
    }
    let (n, rest) = split_typed(s);
    let count = match n {
        None => None,
        Some(t) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => match t.parse::<usize>() {
            Ok(c) if (2..=MAX_COUNT).contains(&c) => Some(c),
            _ => return Err(TypedErr::Count),
        },
        Some(_) => return Err(TypedErr::Count),
    };
    if rest.is_empty() {
        return Ok((count, None));
    }
    let v = if radial {
        let (num, unit) = crate::ui_kit::split_unit(rest);
        let num = match unit {
            "" => num.trim_end_matches('\u{b0}'),
            "\u{b0}" => num,
            _ => return Err(TypedErr::Angle),
        };
        let d = numparse::parse(num, mark).map_err(|e| match e {
            NumError::CommaDecimal => TypedErr::CommaDecimal,
            _ => TypedErr::Angle,
        })?;
        if d.abs() < 1e-9 || d.abs() > 360.0 {
            return Err(TypedErr::Angle);
        }
        d
    } else {
        let sc = crate::expr_field::Scope::none();
        crate::units::parse_edit_length_lang(rest, u, &sc, mark).map_err(|e| match e {
            crate::units::InputError::CommaDecimal => TypedErr::CommaDecimal,
            _ => TypedErr::Length,
        })?
    };
    Ok((count, Some(v)))
}

pub fn linear_geometry(
    base: Vec3,
    cur: Vec3,
    count: usize,
    fit: bool,
    typed: Option<f64>,
    fallback: Vec3,
) -> Option<Geo> {
    if !(2..=MAX_COUNT).contains(&count) {
        return None;
    }
    let (dx, dy) = ((cur.x - base.x) as f64, (cur.y - base.y) as f64);
    let len = dx.hypot(dy);
    let (ux, uy) = if len > 1e-9 {
        (dx / len, dy / len)
    } else {
        let (fx, fy) = (fallback.x as f64, fallback.y as f64);
        let l = fx.hypot(fy);
        if l < 1e-9 {
            return None;
        }
        (fx / l, fy / l)
    };
    let along = typed.unwrap_or(len);
    if along.abs() < 1e-9 {
        return None;
    }
    let v = (ux * along, uy * along);
    let k = if fit { 1.0 / (count - 1) as f64 } else { 1.0 };
    let step = Vec3::new((v.0 * k) as f32, (v.1 * k) as f32, 0.0);
    Some(Geo {
        req: Request::Linear { count, v, fit },
        ghosts: (1..count).map(|i| Xform::Move(step * i as f32)).collect(),
        value: along.abs(),
    })
}

/// Sweep from the reference direction to the cursor, counter-clockwise, in (0, 2π]; pointing
/// at the reference gives the full circle, as in the plan.
fn sweep(c: Vec3, r: (f64, f64), cur: Vec3, snap_deg: f64) -> Option<f64> {
    let (dx, dy) = ((cur.x - c.x) as f64, (cur.y - c.y) as f64);
    if dx.hypot(dy) < 1e-9 {
        return None;
    }
    let mut a = dy.atan2(dx) - r.1.atan2(r.0);
    if snap_deg > 0.0 {
        let s = snap_deg.to_radians();
        a = (a / s).round() * s;
    }
    while a <= 1e-9 {
        a += TAU;
    }
    while a > TAU + 1e-9 {
        a -= TAU;
    }
    if (a - TAU).abs() < 1e-6 {
        a = TAU;
    }
    Some(a)
}

pub fn radial_geometry(
    centre: Vec3,
    reference: (f64, f64),
    cur: Vec3,
    count: usize,
    typed_deg: Option<f64>,
    snap_deg: f64,
) -> Option<Geo> {
    if !(2..=MAX_COUNT).contains(&count) {
        return None;
    }
    let total = match typed_deg {
        Some(d) => d.to_radians(),
        None => sweep(centre, reference, cur, snap_deg)?,
    };
    // beyond a full turn the copies would overlap
    if total.abs() < 1e-6 || total.abs() > TAU + 1e-9 {
        return None;
    }
    let full = (total.abs() - TAU).abs() < 1e-9;
    let parts = if full { count } else { count - 1 };
    let step = total / parts as f64;
    Some(Geo {
        req: Request::Radial {
            count,
            c: (centre.x as f64, centre.y as f64),
            total,
        },
        ghosts: (1..count)
            .map(|i| Xform::Rotate {
                pivot: centre,
                axis: Vec3::Z,
                angle: (step * i as f64) as f32,
            })
            .collect(),
        value: total,
    })
}

fn seg_dist_px(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    let t = if l2 < 1e-9 {
        0.0
    } else {
        ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    };
    (a + ab * t - p).length()
}

// ---- ghost ----

fn draw_ghost(
    painter: &egui::Painter,
    cam: &Cam,
    bodies: &mut Bodies,
    scene: &Scene,
    idx: &[usize],
    xfs: &[Xform],
    col: Color32,
) {
    if xfs.is_empty() {
        return;
    }
    let total: usize = idx
        .iter()
        .map(|&i| bodies.feature(i, scene).edges.len())
        .sum();
    let allowed = (GHOST_EDGES / total.max(1)).clamp(1, GHOST_MAX);
    let n = xfs.len();
    let pick: Vec<usize> = if n <= allowed {
        (0..n).collect()
    } else {
        (0..allowed - 1).chain([n - 1]).collect()
    };
    for &i in idx {
        let f = &bodies.feature(i, scene).edges;
        for &k in &pick {
            let x = xfs[k];
            paint::edges(painter, cam, f, Stroke::new(1.0, col), |p| x.point(p));
        }
    }
}

/// Everything one frame of the tool needs from the 3D view.
pub(super) struct Fx<'a> {
    pub ui: &'a mut egui::Ui,
    pub painter: &'a egui::Painter,
    pub cam: &'a Cam,
    pub scene: &'a Scene,
    pub bodies: &'a mut Bodies,
    pub ed: &'a mut Editor,
    pub rect: Rect,
    pub pointer: Option<Pos2>,
    /// Snapped point under the cursor.
    pub pt: Option<Vec3>,
    pub hover: Option<Target>,
    pub click: bool,
    pub enter: bool,
    pub alt: bool,
    pub shift: bool,
    pub ov: &'a crate::overlay::Palette,
}

impl Edit3d {
    fn ma_op_name(&self) -> String {
        tr(
            self.ru,
            if self.tool == Tool::Mirror {
                "x3d.op.mirror"
            } else {
                "x3d.op.array"
            },
        )
    }

    /// Grid line near the cursor, or the straight wall under it: the axis of a one-click mirror.
    fn axis_candidate(
        &self,
        ed: &Editor,
        cam: &Cam,
        pointer: Option<Pos2>,
        hover: Option<Target>,
    ) -> Option<(Vec3, Vec3, &'static str)> {
        let z = self.plane.origin.z;
        if let Some(p) = pointer {
            let mut best: Option<(f32, Vec3, Vec3)> = None;
            for &((ax, ay), (bx, by)) in &self.grids {
                let a = Vec3::new(ax as f32, ay as f32, z);
                let b = Vec3::new(bx as f32, by as f32, z);
                if let (Some(sa), Some(sb)) = (cam.to_screen(a), cam.to_screen(b)) {
                    let d = seg_dist_px(p, sa, sb);
                    if d <= PICK_PX && best.is_none_or(|(bd, ..)| d < bd) {
                        best = Some((d, a, b));
                    }
                }
            }
            if let Some((_, a, b)) = best {
                return Some((a, b, "x3d.axis.grid"));
            }
        }
        let h = hover.filter(|h| h.cat == Cat::Wall)?;
        // a curved wall has no straight axis
        ed.model.wall_bulge(h.id).ok().filter(|b| b.abs() < 1e-9)?;
        let s = wall_spec(&ed.model, h.id).ok()?;
        Some((
            Vec3::new(s.a.0 as f32, s.a.1 as f32, z),
            Vec3::new(s.b.0 as f32, s.b.1 as f32, z),
            "x3d.axis.wall",
        ))
    }

    /// Typed count and value: the canvas input wins over the option bar field.
    fn ma_typed(&self, ed: &Editor) -> Result<(Option<usize>, Option<f64>), TypedErr> {
        let src = self
            .typing
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(self.ma.val.as_str());
        let radial = self.ma.kind == ArrayKind::Radial;
        read_typed(src, radial, ed.tool.units, DecimalMark::for_ru(self.ru))
    }

    /// A selection made outside 3D (plan, browser, plan grip focus) becomes the 3D selection
    /// now: the command may come before any 3D frame has synced it.
    fn take_outside_sel(&mut self, ed: &Editor) {
        if ed.selection == self.mirror && !self.sel.is_empty() {
            return;
        }
        let mut ts: Vec<Target> = ed
            .selection
            .iter()
            .filter_map(|s| Target::from_sel(*s))
            .collect();
        for &(k, id) in &ed.grip.focus {
            if let Some(t) = Target::from_grip_kind(k, id) {
                if !ts.contains(&t) {
                    ts.push(t);
                }
            }
        }
        self.sel = ts;
        self.mirror = ed.selection.clone();
        self.grip_cache = None;
    }

    fn ma_commit(&mut self, ed: &mut Editor, plan: &Plan, req: Request) {
        let ts: Vec<ddc_model::Target> = plan.ok.iter().map(|p| p.1).collect();
        let n = match req {
            Request::Mirror { copy, .. } => usize::from(copy) * ts.len(),
            Request::Linear { count, .. } | Request::Radial { count, .. } => count - 1,
        };
        let r = ed.run_op(OpFn(Arc::new(move |m: &mut Model| run(m, &ts, &req))));
        self.grip_cache = None;
        let op = self.ma_op_name();
        match r {
            Ok(()) => {
                ed.status = Some(if plan.skipped.is_empty() {
                    let key = if matches!(req, Request::Mirror { .. }) {
                        "x3d.done.mirror"
                    } else {
                        "x3d.done.array"
                    };
                    tr(self.ru, key).replace("{n}", &n.to_string())
                } else {
                    tr(self.ru, "x3d.partial")
                        .replace("{op}", &op)
                        .replace("{why}", &why(self.ru, &plan.skipped))
                });
                self.set_tool(Tool::Select);
            }
            Err(e) => ed.status = Some(self.err(&e)),
        }
    }

    /// Option bar: returns (apply, cancel).
    fn ma_bar(&mut self, ui: &mut egui::Ui, rect: Rect, ed: &Editor) -> (bool, bool) {
        let th = ddc_ui::theme::of(ui.ctx());
        let (ru, units) = (self.ru, ed.tool.units);
        let mirror = self.tool == Tool::Mirror;
        let parse = |s: &str, angle: bool| parse_value(s, angle, ed);
        let ma = &mut self.ma;
        let out = egui::Area::new(egui::Id::new("ddc.x3d.bar"))
            .order(ddc_ui::z::Z::Canvas.order())
            .fixed_pos(bar_origin(rect))
            .show(ui.ctx(), |ui| bar_ui(ui, &th, ru, units, mirror, ma, &parse));
        let b = out.inner;
        self.ma.bar = Some(out.response.rect);
        (b.apply, b.cancel)
    }

    /// One frame of the Mirror or Array tool: preview, clicks, typed values, commit.
    pub(super) fn ma_frame(&mut self, fx: Fx<'_>) {
        let Fx {
            ui,
            painter,
            cam,
            scene,
            bodies,
            ed,
            rect,
            pointer,
            pt,
            hover,
            click,
            enter,
            alt,
            shift,
            ov,
        } = fx;
        let mirror = self.tool == Tool::Mirror;
        let (fg, bg) = (ov.ink, ov.plate);
        let plan = self.ma.refresh(&ed.model, &self.sel, mirror, ed.revision);
        if plan.ok.is_empty() {
            let msg = tr(self.ru, "x3d.refused")
                .replace("{op}", &self.ma_op_name())
                .replace("{why}", &why(self.ru, &plan.skipped));
            ed.status = Some(msg);
            self.set_tool(Tool::Select);
            return;
        }
        let (apply, cancel) = self.ma_bar(ui, rect, ed);
        if cancel {
            self.set_tool(Tool::Select);
            return;
        }
        // a click or hover on the option bar is not a canvas action
        let over_bar = pointer.is_some_and(|p| self.ma.bar.is_some_and(|r| r.contains(p)));
        let (pointer, pt, click) = if over_bar {
            (None, None, false)
        } else {
            (pointer, pt, click)
        };
        let cur = pt.map(|p| self.plane.project(p));
        if cur.is_some() {
            self.ma.last = cur;
        }
        let cur = cur.or(self.ma.last);
        let idx: Vec<usize> = plan.ok.iter().filter_map(|(t, _)| bodies.of(*t)).collect();
        let ghost_col = ov.preview;

        if mirror {
            // the wall being mirrored cannot be its own axis
            let own_wall = axis_wall_selected(hover, &plan);
            let cand = if self.ma.pts.is_empty() && !alt && !own_wall {
                self.axis_candidate(ed, cam, pointer, hover)
            } else {
                None
            };
            if own_wall && self.ma.pts.is_empty() && !alt && click {
                ed.status = Some(tr(self.ru, "x3d.axis.wall_selected"));
                return;
            }
            let axis = match (self.ma.pts.first().copied(), cand, cur) {
                (Some(a), _, Some(c)) => Some((a, c)),
                (None, Some((a, b, _)), _) => Some((a, b)),
                _ => None,
            };
            let valid = axis.filter(|(a, b)| (*b - *a).truncate().length_squared() > 1e-8);
            if let Some((a, b)) = valid {
                let d = (b - a).normalize_or_zero();
                let e = cam.m_per_px(a) * 40.0;
                paint::dashed(painter, cam, a - d * e, b + d * e, Stroke::new(1.5, ov.snap));
                draw_ghost(painter, cam, bodies, scene, &idx, &[Xform::Mirror { a, b }], ghost_col);
            }
            if let (Some((_, _, key)), Some(p)) = (cand, pointer) {
                let txt = tr(self.ru, key);
                paint::label(painter, p + Vec2::new(16.0, 16.0), &txt, fg, bg, Some(ov.snap));
            }
            if let Some(a) = self.ma.pts.first().copied() {
                if let Some(at) = cam.to_screen(a) {
                    paint::grip(painter, at, paint::Glyph::Square, Vec2::ZERO, true, ov.snap, ov);
                }
            }
            let go = apply || click || enter;
            if go {
                match valid {
                    Some((a, b)) if cand.is_some() || !self.ma.pts.is_empty() => {
                        let req = Request::Mirror {
                            a: (a.x as f64, a.y as f64),
                            b: (b.x as f64, b.y as f64),
                            copy: self.ma.keep,
                        };
                        self.ma_commit(ed, &plan, req);
                        return;
                    }
                    _ if click && self.ma.pts.is_empty() => {
                        if let Some(c) = cur {
                            self.ma.pts.push(c);
                        }
                    }
                    _ => ed.status = Some(tr(self.ru, "x3d.need_axis")),
                }
            }
            return;
        }

        // ---- array ----
        let typed = self.ma_typed(ed);
        if let Err(e) = typed {
            // refused text is never replaced by the cursor or the count field: nothing applies
            let msg = tr(self.ru, e.key());
            if let Some(p) = pointer {
                paint::label(painter, p + Vec2::new(16.0, 16.0), &msg, fg, bg, Some(ov.snap));
            }
            if ed.status.as_deref() != Some(msg.as_str()) {
                ed.status = Some(msg);
            }
            if !self.ma.pts.is_empty() || !click {
                return;
            }
        }
        let (typed_n, typed_v) = typed.unwrap_or((None, None));
        let count = typed_n.unwrap_or(self.ma.count);
        let Some(base) = self.ma.pts.first().copied() else {
            if click {
                if let Some(c) = cur {
                    self.ma.pts.push(c);
                }
            } else if apply || enter {
                ed.status = Some(tr(self.ru, "x3d.need_base"));
            }
            return;
        };
        // radial: the sweep starts at the direction from the centre to the selection
        let o = self.gizmo_origin(bodies).unwrap_or(base);
        let refdir = ((o.x - base.x) as f64, (o.y - base.y) as f64);
        let refdir = if refdir.0.hypot(refdir.1) < 1e-6 {
            (1.0, 0.0)
        } else {
            refdir
        };
        let geo = match (self.ma.kind, cur) {
            (ArrayKind::Linear, Some(c)) => {
                linear_geometry(base, c, count, self.ma.fit, typed_v, self.plane.u)
            }
            (ArrayKind::Radial, Some(c)) => {
                let exact = self.snap.is_some_and(|s| !matches!(s.kind, SnapKind::Plane));
                let step = if exact {
                    0.0
                } else if shift {
                    15.0
                } else {
                    5.0
                };
                radial_geometry(base, refdir, c, count, typed_v, step)
            }
            _ => None,
        };
        if let Some(at) = cam.to_screen(base) {
            paint::grip(painter, at, paint::Glyph::Square, Vec2::ZERO, true, ov.snap, ov);
        }
        let Some(g) = geo else {
            if apply || enter {
                ed.status = Some(tr(self.ru, "x3d.need_input"));
            }
            return;
        };
        draw_ghost(painter, cam, bodies, scene, &idx, &g.ghosts, ghost_col);
        match (g.req, cur) {
            (Request::Linear { count, v, fit }, _) => {
                let last = base + Vec3::new(v.0 as f32, v.1 as f32, 0.0);
                let txt = format!(
                    "{}{}  \u{d7}{count}",
                    if fit { "\u{3a3} " } else { "" },
                    fmt_len(g.value as f32, ed)
                );
                paint::dim(painter, cam, base, last, &txt, fg, bg);
            }
            (Request::Radial { count, total, .. }, Some(c)) => {
                let r = (c.x - base.x).hypot(c.y - base.y).max(0.5) as f64;
                let a0 = refdir.1.atan2(refdir.0);
                let arc: Vec<Vec3> = (0..=32)
                    .map(|i| {
                        let a = a0 + total * i as f64 / 32.0;
                        base + Vec3::new((a.cos() * r) as f32, (a.sin() * r) as f32, 0.0)
                    })
                    .collect();
                paint::polyline(painter, cam, &arc, Stroke::new(1.5, ov.snap));
                paint::dashed(painter, cam, base, c, Stroke::new(1.0, ov.snap));
                if let Some(mid) = cam.to_screen(arc[16]) {
                    let txt = format!("{:.1}\u{b0}  \u{d7}{count}", total.to_degrees());
                    paint::label(painter, mid + Vec2::new(10.0, -10.0), &txt, fg, bg, Some(ov.snap));
                }
            }
            _ => {}
        }
        if click || enter || apply {
            self.ma_commit(ed, &plan, g.req);
        }
    }
}

/// Mirror and Array in 3D are the same commands as in the plan (ribbon, mnemonics, chords).
/// True when the 3D view took the command.
pub fn route(a: &mut EditorApp, op: crate::modify_tools::Op) -> bool {
    use crate::modify_tools::Op;
    match op {
        Op::Mirror | Op::MirrorDraw => start(a, Tool::Mirror),
        Op::Array => start(a, Tool::Array),
        _ => return false,
    }
    true
}

/// Mirror or array in 3D on a selection with 3D-only items (beams off the level, MEP, panels
/// the plan selection cannot hold): the 3D tool takes it, or refuses with its reason. Anything
/// the plan selection holds goes to the plan modify session instead. True when taken here.
pub fn route_3d_only(a: &mut EditorApp, op: crate::modify_tools::Op) -> bool {
    use crate::modify_tools::Op;
    if !matches!(op, Op::Mirror | Op::MirrorDraw | Op::Array) {
        return false;
    }
    a.view.e3.take_outside_sel(&a.ed);
    let only3d = a.view.e3.sel.iter().any(|t| t.sel().is_none() || t.cat == Cat::Panel);
    only3d && route(a, op)
}

/// Starts the tool, or refuses with the reason when nothing selected can take it.
pub fn start(a: &mut EditorApp, tool: Tool) {
    a.view3d = true;
    a.view.e3.ru = a.sh.cat.lang == ddc_ui::i18n::Lang::Ru;
    a.view.e3.take_outside_sel(&a.ed);
    let e = &mut a.view.e3;
    if e.tool == tool {
        e.set_tool(Tool::Select);
        return;
    }
    let mirror = tool == Tool::Mirror;
    let op = tr(
        e.ru,
        if mirror {
            "x3d.op.mirror"
        } else {
            "x3d.op.array"
        },
    );
    if e.sel.is_empty() {
        a.ed.status = Some(tr(e.ru, "x3d.need_sel").replace("{op}", &op));
        return;
    }
    let plan = classify(&a.ed.model, &e.sel, mirror);
    if plan.ok.is_empty() {
        a.ed.status = Some(
            tr(e.ru, "x3d.refused")
                .replace("{op}", &op)
                .replace("{why}", &why(e.ru, &plan.skipped)),
        );
        return;
    }
    e.set_tool(tool);
}

#[cfg(test)]
mod tests {
    use super::super::xform::tests::{base, column, wall};
    use super::*;

    fn count(m: &Model, table: &str) -> i64 {
        m.sql()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn wall_starts(m: &Model) -> Vec<(f64, f64)> {
        let mut st = m.sql().prepare("SELECT id FROM wall").unwrap();
        let ids: Vec<i64> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        ids.into_iter()
            .map(|id| wall_spec(m, id).unwrap().a)
            .collect()
    }

    fn tgt(cat: Cat, id: i64) -> Target {
        Target::new(cat, id)
    }

    #[test]
    fn a_selected_wall_is_not_offered_as_its_own_mirror_axis() {
        let w = Target::new(Cat::Wall, 7);
        let plan = Plan {
            ok: vec![(w, ddc_model::Target::Wall(7))],
            skipped: vec![],
        };
        assert!(axis_wall_selected(Some(w), &plan));
        assert!(!axis_wall_selected(Some(Target::new(Cat::Wall, 8)), &plan));
        assert!(!axis_wall_selected(None, &plan));
    }

    #[test]
    fn typed_grammar() {
        assert_eq!(split_typed("2.5"), (None, "2.5"));
        assert_eq!(split_typed(" 6 2.5 "), (Some("6"), "2.5"));
        assert_eq!(split_typed("6"), (None, "6"));
        // imperial input is one value, not a count
        assert_eq!(split_typed("2' 6\""), (None, "2' 6\""));
        assert_eq!(split_typed("6 1/2\""), (None, "6 1/2\""));
        assert_eq!(split_typed("4 2' 6\""), (Some("4"), "2' 6\""));
        assert_eq!(split_typed("12 ft 6 in"), (None, "12 ft 6 in"));
        let (_, v) = split_typed("2' 6\"");
        let m = crate::units::parse_length(v, crate::units::Units::Imperial).unwrap();
        assert!((m - 0.762).abs() < 1e-9, "{m}");
        // a number-shaped head is a count attempt, so a bad one is refused, not swallowed
        assert_eq!(split_typed("-1 3"), (Some("-1"), "3"));
        assert_eq!(split_typed("1.5 3000"), (Some("1.5"), "3000"));
        // thousands groups are one value, never a count
        assert_eq!(split_typed("3 000"), (None, "3 000"));
        assert_eq!(split_typed("3\u{a0}000 mm"), (None, "3\u{a0}000 mm"));
        assert_eq!(split_typed("3\u{202f}000"), (None, "3\u{202f}000"));
        assert_eq!(split_typed("1 200 mm"), (None, "1 200 mm"));
        assert_eq!(split_typed("4 3 000"), (Some("4"), "3 000"));
        assert_eq!(split_typed("0 300"), (Some("0"), "300"));
        // `;` types a count before a three-digit step
        assert_eq!(split_typed("4;300"), (Some("4"), "300"));
        assert_eq!(split_typed("4 ; 300"), (Some("4"), "300"));
    }

    type TypedRead = Result<(Option<usize>, Option<f64>), TypedErr>;

    fn val(r: TypedRead, n: Option<usize>, want: f64) {
        let (got_n, got_v) = r.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(got_n, n);
        let v = got_v.expect("a value");
        assert!((v - want).abs() < 1e-9, "{v} != {want}");
    }

    #[test]
    fn typed_array_values_use_the_explicit_decimal_mark() {
        use crate::units::Units::{Imperial, Metric};
        let (en, ru) = (DecimalMark::Point, DecimalMark::Comma);
        for mark in [en, ru] {
            // group spaces of both locales: 3 m, never 3 copies at 0
            val(read_typed("3 000", false, Metric, mark), None, 3.0);
            val(read_typed("3\u{a0}000", false, Metric, mark), None, 3.0);
            val(read_typed("3\u{202f}000", false, Metric, mark), None, 3.0);
            val(read_typed("3\u{a0}000 mm", false, Metric, mark), None, 3.0);
            val(read_typed("1 200 mm", false, Metric, mark), None, 1.2);
            val(read_typed("4 3 000", false, Metric, mark), Some(4), 3.0);
            val(read_typed("4;300", false, Metric, mark), Some(4), 0.3);
            // units in the spacing
            val(read_typed("5 2 m", false, Metric, mark), Some(5), 2.0);
            val(read_typed("5 1 200 mm", false, Metric, mark), Some(5), 1.2);
            val(read_typed("4 2' 6\"", false, Imperial, mark), Some(4), 0.762);
            assert_eq!(read_typed("  ", false, Metric, mark), Ok((None, None)));
            assert_eq!(read_typed("6;", false, Metric, mark), Ok((Some(6), None)));
        }
        // English comma groups thousands, Russian comma is the decimal mark
        val(read_typed("3,000", false, Metric, en), None, 3.0);
        val(read_typed("3,000", false, Metric, ru), None, 0.003);
        val(read_typed("3,5", false, Metric, ru), None, 0.0035);
        val(read_typed("4 3,5", false, Metric, ru), Some(4), 0.0035);
        // ambiguous in English: refused with the decimal-point message, not read as 35
        assert_eq!(read_typed("3,5", false, Metric, en), Err(TypedErr::CommaDecimal));
        assert_eq!(read_typed("4 3,5", false, Metric, en), Err(TypedErr::CommaDecimal));
        assert_eq!(TypedErr::CommaDecimal.key(), "decimal_input.comma_decimal");
    }

    #[test]
    fn typed_array_counts_are_whole_numbers_in_range() {
        use crate::units::Units::Metric;
        for mark in [DecimalMark::Point, DecimalMark::Comma] {
            for s in [
                "1.5 3000", "1,5 3000", "0 3000", "0 300", "1 3000", "-2 3000", "-1 3",
                "201 3000", "99999999999999999999999 3000", ";3000", "x;3000", "2.0;300",
            ] {
                assert_eq!(read_typed(s, false, Metric, mark), Err(TypedErr::Count), "{s:?} {mark:?}");
            }
            val(read_typed("2 3000", false, Metric, mark), Some(2), 3.0);
            val(read_typed("200 3000", false, Metric, mark), Some(200), 3.0);
            // the step itself must be a positive length
            for s in ["0", "-3000", "abc", "4 abc", "4;-300"] {
                assert_eq!(read_typed(s, false, Metric, mark), Err(TypedErr::Length), "{s:?}");
            }
        }
        assert_eq!(TypedErr::Count.key(), "mirror_array.err.count");
    }

    #[test]
    fn typed_radial_sweep_is_an_angle_in_degrees() {
        use crate::units::Units::Metric;
        let (en, ru) = (DecimalMark::Point, DecimalMark::Comma);
        for mark in [en, ru] {
            val(read_typed("4 90", true, Metric, mark), Some(4), 90.0);
            val(read_typed("90\u{b0}", true, Metric, mark), None, 90.0);
            val(read_typed("90 \u{b0}", true, Metric, mark), None, 90.0);
            val(read_typed("-180", true, Metric, mark), None, -180.0);
            val(read_typed("360", true, Metric, mark), None, 360.0);
            for s in ["0", "400", "90 mm", "abc"] {
                assert_eq!(read_typed(s, true, Metric, mark), Err(TypedErr::Angle), "{s:?}");
            }
            assert_eq!(read_typed("1.5 90", true, Metric, mark), Err(TypedErr::Count));
        }
        val(read_typed("22,5", true, Metric, ru), None, 22.5);
        val(read_typed("22.5", true, Metric, en), None, 22.5);
        assert_eq!(read_typed("22,5", true, Metric, en), Err(TypedErr::CommaDecimal));
    }

    #[test]
    fn array_tool_reads_its_field_in_the_tool_language_and_refuses_bad_text() {
        let mut a = plan_selected_walls();
        start(&mut a, Tool::Array);
        a.ed.tool.units = crate::units::Units::Metric;
        let e3 = &mut a.view.e3;
        e3.typing = None;
        e3.ma.kind = ArrayKind::Linear;
        // the tool language picks the mark; the thread UI language is not switched
        e3.ma.val = "3,000".into();
        e3.ru = false;
        assert_eq!(e3.ma_typed(&a.ed), Ok((None, Some(3.0))));
        e3.ru = true;
        assert_eq!(e3.ma_typed(&a.ed), Ok((None, Some(0.003))));
        e3.ma.val = "3 000".into();
        for ru in [false, true] {
            e3.ru = ru;
            assert_eq!(e3.ma_typed(&a.ed), Ok((None, Some(3.0))));
        }
        e3.ma.val = "1.5 3000".into();
        assert_eq!(e3.ma_typed(&a.ed), Err(TypedErr::Count));
        // the canvas text wins over the option bar field, with the same rules
        e3.ma.val = "4 3000".into();
        e3.typing = Some("0 3000".into());
        assert_eq!(e3.ma_typed(&a.ed), Err(TypedErr::Count));
        for k in [TypedErr::Count, TypedErr::Length, TypedErr::Angle] {
            for ru in [false, true] {
                assert_ne!(tr(ru, k.key()), k.key(), "{k:?} has a message in both languages");
            }
        }
    }

    #[test]
    fn linear_geometry_step_and_total() {
        let b = Vec3::ZERO;
        let c = Vec3::new(4.0, 0.0, 7.0);
        let g = linear_geometry(b, c, 3, false, None, Vec3::X).unwrap();
        assert_eq!(g.ghosts.len(), 2);
        assert_eq!(g.ghosts[1], Xform::Move(Vec3::new(8.0, 0.0, 0.0)));
        let Request::Linear { v, fit, .. } = g.req else {
            panic!()
        };
        assert!(!fit && (v.0 - 4.0).abs() < 1e-9 && v.1.abs() < 1e-9, "height is ignored");
        // total length: the last placement sits at the cursor
        let g = linear_geometry(b, c, 5, true, None, Vec3::X).unwrap();
        assert_eq!(g.ghosts[3], Xform::Move(Vec3::new(4.0, 0.0, 0.0)));
        // typed value runs along the cursor direction; without a direction, along the plane
        let g = linear_geometry(b, Vec3::new(0.0, 2.0, 0.0), 2, false, Some(1.5), Vec3::X).unwrap();
        assert_eq!(g.ghosts[0], Xform::Move(Vec3::new(0.0, 1.5, 0.0)));
        let g = linear_geometry(b, b, 2, false, Some(1.5), Vec3::X).unwrap();
        assert_eq!(g.ghosts[0], Xform::Move(Vec3::new(1.5, 0.0, 0.0)));
        assert!(linear_geometry(b, b, 2, false, None, Vec3::X).is_none());
        assert!(linear_geometry(b, c, 1, false, None, Vec3::X).is_none());
        assert!(linear_geometry(b, c, MAX_COUNT + 1, false, None, Vec3::X).is_none());
    }

    #[test]
    fn radial_sweep_matches_plan_conventions() {
        let o = Vec3::ZERO;
        let r = (1.0, 0.0);
        // a quarter turn counter-clockwise
        let g = radial_geometry(o, r, Vec3::new(0.0, 3.0, 0.0), 4, None, 5.0).unwrap();
        let Request::Radial { total, .. } = g.req else {
            panic!()
        };
        assert!((total - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert_eq!(g.ghosts.len(), 3);
        // pointing at the reference is the full circle
        let g = radial_geometry(o, r, Vec3::new(2.0, 0.0, 0.0), 4, None, 5.0).unwrap();
        let Request::Radial { total, .. } = g.req else {
            panic!()
        };
        assert!((total - TAU).abs() < 1e-9);
        // a full circle spaces by 360/count, an open sweep by total/(count-1)
        let Xform::Rotate { angle, .. } = g.ghosts[0] else {
            panic!()
        };
        assert!((angle as f64 - TAU / 4.0).abs() < 1e-5);
        let g = radial_geometry(o, r, o, 4, Some(90.0), 5.0).unwrap();
        let Xform::Rotate { angle, .. } = g.ghosts[0] else {
            panic!()
        };
        assert!((angle as f64 - std::f64::consts::FRAC_PI_2 / 3.0).abs() < 1e-5);
        assert!(radial_geometry(o, r, o, 4, Some(0.0), 5.0).is_none());
        assert!(radial_geometry(o, r, o, 4, Some(400.0), 5.0).is_none());
        assert!(radial_geometry(o, r, o, 4, None, 5.0).is_none());
    }

    #[test]
    fn mirror_ghost_matches_the_model_result() {
        let mut t = base();
        let w = wall(&mut t, (1.0, 1.0), (4.0, 1.0));
        let plan = classify(&t.m, &[tgt(Cat::Wall, w)], true);
        assert_eq!(plan.ok.len(), 1);
        let (a, b) = ((0.0, 0.0), (0.0, 5.0));
        let req = Request::Mirror { a, b, copy: true };
        let ts: Vec<_> = plan.ok.iter().map(|p| p.1).collect();
        run(&mut t.m, &ts, &req).unwrap();
        assert_eq!(count(&t.m, "wall"), 2);
        let ghost = Xform::Mirror {
            a: Vec3::new(0.0, 0.0, 0.0),
            b: Vec3::new(0.0, 5.0, 0.0),
        }
        // a mirrored wall is reversed, so its start is the mirror of the original end
        .point(Vec3::new(4.0, 1.0, 0.0));
        let copies = wall_starts(&t.m);
        assert!(
            copies
                .iter()
                .any(|p| (p.0 as f32 - ghost.x).abs() < 1e-4 && (p.1 as f32 - ghost.y).abs() < 1e-4),
            "{copies:?} vs {ghost:?}"
        );
        // one undo removes the copy
        t.m.undo().unwrap();
        assert_eq!(count(&t.m, "wall"), 1);
    }

    #[test]
    fn linear_array_makes_count_minus_one_copies_in_one_step() {
        let mut t = base();
        let w = wall(&mut t, (0.0, 0.0), (2.0, 0.0));
        let plan = classify(&t.m, &[tgt(Cat::Wall, w)], false);
        let ts: Vec<_> = plan.ok.iter().map(|p| p.1).collect();
        let req = Request::Linear {
            count: 4,
            v: (0.0, 3.0),
            fit: false,
        };
        run(&mut t.m, &ts, &req).unwrap();
        assert_eq!(count(&t.m, "wall"), 4);
        t.m.undo().unwrap();
        assert_eq!(count(&t.m, "wall"), 1);
        // total length divides by count - 1
        let req = Request::Linear {
            count: 4,
            v: (0.0, 9.0),
            fit: true,
        };
        run(&mut t.m, &ts, &req).unwrap();
        let ys: Vec<f64> = wall_starts(&t.m).iter().map(|p| p.1).collect();
        for want in [0.0, 3.0, 6.0, 9.0] {
            assert!(ys.iter().any(|y| (y - want).abs() < 1e-9), "{ys:?}");
        }
    }

    #[test]
    fn radial_array_and_count_limits() {
        let mut t = base();
        let w = wall(&mut t, (1.0, 0.0), (2.0, 0.0));
        let ts: Vec<_> = classify(&t.m, &[tgt(Cat::Wall, w)], false)
            .ok
            .iter()
            .map(|p| p.1)
            .collect();
        let req = Request::Radial {
            count: 4,
            c: (0.0, 0.0),
            total: TAU,
        };
        run(&mut t.m, &ts, &req).unwrap();
        assert_eq!(count(&t.m, "wall"), 4);
        let bad = Request::Linear {
            count: 1,
            v: (1.0, 0.0),
            fit: false,
        };
        assert!(matches!(run(&mut t.m, &ts, &bad), Err(Error::Invalid(k)) if k == "e3d.err.array_count"));
        let big = Request::Linear {
            count: MAX_COUNT + 1,
            v: (1.0, 0.0),
            fit: false,
        };
        assert!(matches!(run(&mut t.m, &ts, &big), Err(Error::Invalid(k)) if k == "e3d.err.array_max"));
        assert_eq!(count(&t.m, "wall"), 4, "a refused request leaves the model alone");
    }

    #[test]
    fn framing_members_take_part_like_in_the_plan() {
        let mut t = base();
        let c = column(&mut t, (1.0, 1.0));
        let plan = classify(&t.m, &[tgt(Cat::Column, c)], true);
        assert_eq!(plan.ok.len(), 1, "{:?}", plan.skipped);
        let ts: Vec<_> = plan.ok.iter().map(|p| p.1).collect();
        let req = Request::Mirror {
            a: (0.0, 0.0),
            b: (0.0, 1.0),
            copy: true,
        };
        run(&mut t.m, &ts, &req).unwrap();
        assert_eq!(count(&t.m, "col"), 2);
    }

    #[test]
    fn hosted_opening_goes_with_its_wall_and_refuses_alone() {
        let mut t = base();
        let w = wall(&mut t, (0.0, 0.0), (5.0, 0.0));
        let o =
            t.m.add_opening(
                w,
                &ddc_model::OpeningSpec {
                    along: 1.0,
                    width: 1.0,
                    sill: 0.9,
                    height: 1.2,
                    kind: "window".into(),
                },
            )
            .unwrap();
        let alone = classify(&t.m, &[tgt(Cat::Opening, o)], true);
        assert!(alone.ok.is_empty());
        assert_eq!(alone.skipped, vec![(Cat::Opening, elem::HOSTED)]);
        let both = classify(&t.m, &[tgt(Cat::Wall, w), tgt(Cat::Opening, o)], true);
        assert_eq!(both.ok.len(), 2);
        let ts: Vec<_> = both.ok.iter().map(|p| p.1).collect();
        let req = Request::Mirror {
            a: (0.0, 2.0),
            b: (1.0, 2.0),
            copy: true,
        };
        run(&mut t.m, &ts, &req).unwrap();
        assert_eq!(count(&t.m, "wall"), 2);
        assert_eq!(count(&t.m, "opening"), 2, "the copy of the wall keeps its opening");
    }

    #[test]
    fn panels_and_unsupported_families_are_refused_with_a_reason() {
        let t = base();
        let sel = [
            Target {
                cat: Cat::Panel,
                id: 1,
                sub: 3,
            },
            // a319e363 gave the stair every transform: a rebar set has no model target alone
            tgt(Cat::Ext(crate::scene_ids::EXT_REBAR), 1),
        ];
        let p = classify(&t.m, &sel, false);
        assert!(p.ok.is_empty());
        assert!(p.skipped.contains(&(Cat::Panel, "x3d.no.panel")));
        assert!(p.skipped.contains(&(Cat::Ext(crate::scene_ids::EXT_REBAR), elem::NOT_YET)));
        let txt = why(false, &p.skipped);
        assert!(!txt.is_empty() && !txt.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)));
    }

    #[test]
    fn mixed_selection_applies_to_the_supported_part_only() {
        let mut t = base();
        let w = wall(&mut t, (0.0, 0.0), (2.0, 0.0));
        let p = classify(&t.m, &[tgt(Cat::Wall, w), tgt(Cat::Ext(crate::scene_ids::EXT_REBAR), 9)], true);
        assert_eq!(p.ok.len(), 1);
        assert_eq!(p.skipped.len(), 1);
    }

    #[test]
    fn grid_line_distance_in_pixels() {
        let d = seg_dist_px(Pos2::new(5.0, 3.0), Pos2::new(0.0, 0.0), Pos2::new(10.0, 0.0));
        assert!((d - 3.0).abs() < 1e-6);
        let d = seg_dist_px(Pos2::new(14.0, 3.0), Pos2::new(0.0, 0.0), Pos2::new(10.0, 0.0));
        assert!((d - 5.0).abs() < 1e-6, "beyond the end the distance is to the end");
    }

    /// Two walls selected in the plan only; the 3D view has never synced them.
    fn plan_selected_walls() -> EditorApp {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))]);
        let mut a = EditorApp::new(ed, None, "test".into());
        a.ed.selection = a.ed.walls.iter().map(|w| crate::editor::Sel::Wall(w.id)).collect();
        assert_eq!(a.ed.selection.len(), 2);
        a.view3d = false;
        a.view.e3.sel.clear();
        a
    }

    fn commit_on_plan_selection(tool: Tool, req: Request, walls_after: i64) {
        let mut a = plan_selected_walls();
        start(&mut a, tool);
        assert_eq!(a.view.e3.tool, tool, "{:?}", a.ed.status);
        assert_eq!(a.view.e3.sel.len(), 2, "the plan selection became the 3D selection");
        let plan = classify(&a.ed.model, &a.view.e3.sel, tool == Tool::Mirror);
        assert_eq!(plan.ok.len(), 2);
        let (n0, h0) = (a.ed.undo_len(), a.ed.model.content_hash().unwrap());
        a.view.e3.ma_commit(&mut a.ed, &plan, req);
        assert_eq!(count(&a.ed.model, "wall"), walls_after, "{:?}", a.ed.status);
        assert_eq!(a.ed.undo_len(), n0 + 1, "one gesture, one undo step");
        a.ed.undo();
        assert_eq!(count(&a.ed.model, "wall"), 2);
        assert_eq!(a.ed.model.content_hash().unwrap(), h0, "one undo takes it all back");
    }

    #[test]
    fn plan_selected_walls_mirror_in_3d_in_one_undo_step() {
        let req = Request::Mirror {
            a: (0.0, 5.0),
            b: (1.0, 5.0),
            copy: true,
        };
        commit_on_plan_selection(Tool::Mirror, req, 4);
    }

    #[test]
    fn plan_selected_walls_array_in_3d_in_one_undo_step() {
        let req = Request::Linear {
            count: 3,
            v: (0.0, 5.0),
            fit: false,
        };
        commit_on_plan_selection(Tool::Array, req, 6);
    }

    #[test]
    fn a_3d_only_selection_keeps_the_3d_tool_and_its_refusal() {
        use crate::modify_tools::Op;
        let mut a = plan_selected_walls();
        assert!(!route_3d_only(&mut a, Op::Mirror), "plan-held walls go to the plan session");
        a.ed.selection.clear();
        // picked in 3D: the plan selection mirrored into the view holds none of it
        a.view.e3.mirror.clear();
        a.view.e3.sel = vec![Target { cat: Cat::Panel, id: 1, sub: 3 }];
        assert!(route_3d_only(&mut a, Op::Array), "a panel is not dropped on the way to the plan");
        assert_eq!(a.view.e3.tool, Tool::Select);
        let st = a.ed.status.clone().unwrap_or_default();
        assert!(!st.is_empty() && st != "x3d.refused", "the 3D refusal says why: {st}");
        assert!(!route_3d_only(&mut a, Op::Move), "only mirror and array have a 3D tool");
    }

    #[test]
    fn plan_grip_focus_counts_as_a_selection() {
        let mut a = plan_selected_walls();
        let ids: Vec<i64> = a.ed.walls.iter().map(|w| w.id).collect();
        a.ed.selection.clear();
        a.ed.grip.focus = vec![(crate::grips::Kind::Wall, ids[0])];
        start(&mut a, Tool::Mirror);
        assert_eq!(a.view.e3.tool, Tool::Mirror, "{:?}", a.ed.status);
        assert_eq!(a.view.e3.sel, vec![Target::new(Cat::Wall, ids[0])]);
    }

    // ---- option bar: kit widgets, same look as the plan option bars ----

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn parse_plain(s: &str, angle: bool) -> Option<f32> {
        if angle {
            s.trim().parse::<f32>().ok()
        } else {
            crate::units::parse_edit_length(s, Units::Metric).map(|v| v as f32)
        }
    }

    fn screen() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))),
            ..Default::default()
        }
    }

    /// Bar rect and report after three frames at 1280 px.
    fn run_bar(ru: bool, mirror: bool, kind: ArrayKind, val: &str) -> (Rect, BarOut) {
        let ctx = font_ctx();
        let mut ma = State {
            kind,
            val: val.to_owned(),
            ..State::default()
        };
        let mut res = None;
        for _ in 0..3 {
            let _ = ctx.run(screen(), |c| {
                let th = ddc_ui::theme::of(c);
                let at = Pos2::new(space::SM, 60.0);
                let a = egui::Area::new(egui::Id::new("t.bar")).fixed_pos(at).show(c, |ui| {
                    bar_ui(ui, &th, ru, Units::Metric, mirror, &mut ma, &parse_plain)
                });
                res = Some((a.response.rect, a.inner));
            });
        }
        res.expect("frame ran")
    }

    /// A plan-style option bar: checkbox, count field, primary button.
    fn plan_bar_rect(ru: bool) -> Rect {
        let ctx = font_ctx();
        let mut rect = Rect::NOTHING;
        for _ in 0..3 {
            let _ = ctx.run(screen(), |c| {
                let th = ddc_ui::theme::of(c);
                let at = Pos2::new(space::SM, 60.0);
                let a = egui::Area::new(egui::Id::new("t.plan")).fixed_pos(at).show(c, |ui| {
                    mo::bar_frame(&th).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            mo::row_style(ui, &th);
                            let mut on = true;
                            widgets::checkbox(ui, &th, &mut on, "x");
                            let mut n = 4usize;
                            mo::count_field(ui, &th, ru, &mut n);
                            mo::done_button(ui, &th, ru, "OK");
                        });
                    });
                });
                rect = a.response.rect;
            });
        }
        rect
    }

    const CASES: [(bool, ArrayKind); 3] = [
        (true, ArrayKind::Linear),
        (false, ArrayKind::Linear),
        (false, ArrayKind::Radial),
    ];

    #[test]
    fn option_bar_fits_1280_px_in_both_languages() {
        for ru in [false, true] {
            for (mirror, kind) in CASES {
                let (r, out) = run_bar(ru, mirror, kind, "");
                assert!(r.left() >= 0.0 && r.right() <= 1280.0 - space::SM, "{ru} {mirror} {kind:?}: {r:?}");
                assert!(out.apply_ok && out.reason.is_none());
            }
        }
    }

    #[test]
    fn option_bar_has_the_plan_bar_height_in_both_languages() {
        for ru in [false, true] {
            let plan = plan_bar_rect(ru).height();
            for (mirror, kind) in CASES {
                let h = run_bar(ru, mirror, kind, "").0.height();
                assert!((h - plan).abs() <= 2.0, "{ru} {mirror} {kind:?}: {h} vs plan {plan}");
            }
        }
    }

    #[test]
    fn unreadable_value_disables_apply_with_a_reason() {
        for ru in [false, true] {
            let lin = run_bar(ru, false, ArrayKind::Linear, "abc").1;
            assert!(!lin.apply_ok && !lin.apply);
            assert_eq!(lin.reason, Some(crate::tr_lang(ru, "length_input.opt.bad_length", &[])));
            let rad = run_bar(ru, false, ArrayKind::Radial, "abc").1;
            assert!(!rad.apply_ok);
            assert_eq!(rad.reason, Some(crate::tr_lang(ru, "x3d.bad_angle", &[])));
            // a count below two is refused; `1 500` is one grouped value (split_typed), not a count
            let cnt = run_bar(ru, false, ArrayKind::Linear, "1;500").1;
            assert!(!cnt.apply_ok && cnt.reason.is_some());
            let cnt = run_bar(ru, false, ArrayKind::Linear, "1 5000").1;
            assert!(!cnt.apply_ok && cnt.reason.is_some());
            for ok in ["", "500", "1 500", "6 2500", "200 1000"] {
                let o = run_bar(ru, false, ArrayKind::Linear, ok).1;
                assert!(o.apply_ok && o.reason.is_none(), "{ru} {ok:?}");
            }
            let o = run_bar(ru, false, ArrayKind::Radial, "4 90").1;
            assert!(o.apply_ok, "{ru}");
        }
        assert_ne!(
            crate::tr_lang(false, "x3d.bad_angle", &[]),
            crate::tr_lang(true, "x3d.bad_angle", &[])
        );
    }

    #[test]
    fn the_reason_line_adds_height_but_stays_inside_the_screen() {
        for ru in [false, true] {
            let ok = run_bar(ru, false, ArrayKind::Linear, "").0;
            let bad = run_bar(ru, false, ArrayKind::Linear, "abc").0;
            assert!(bad.height() > ok.height());
            assert!(bad.right() <= 1280.0 - space::SM, "{ru}: {bad:?}");
        }
    }

    #[test]
    fn bar_origin_comes_from_tokens_and_clears_the_view_bar() {
        let ctx = font_ctx();
        let th = ddc_ui::theme::of(&ctx);
        let p = bar_origin(Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0)));
        assert_eq!(p, crate::ui_kit::canvas_bar_pos(Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::ZERO), 1));
        // the view bar: one control row in its frame at the SM inset
        let view_bottom = 50.0 + space::SM + th.d().control + 2.0 * space::XS;
        assert!(p.y >= view_bottom + space::SM, "{p:?}");
    }

    #[test]
    fn option_bar_uses_kit_widgets_only() {
        let src = include_str!("mirror_array.rs");
        let code = &src[..src.find("#[cfg(test)]").expect("tests")];
        for bad in [
            "DragValue",
            "TextEdit",
            "egui::Button",
            ".button(",
            ".small_button(",
            ".checkbox(",
            ".selectable_value(",
            ".selectable_label(",
            ".radio",
            "ui.separator",
            "Frame::popup",
            "Vec2::new(12.0, 44.0)",
        ] {
            assert!(!code.contains(bad), "raw widget or literal offset left: {bad}");
        }
        let from = code.find("fn bar_ui(").expect("bar_ui");
        let bar = &code[from..code.find("fn axis_wall_selected(").expect("next fn")];
        for want in ["mo::bar_frame", "mo::count_field", "TextField::new", "Button::primary", "Button::quiet", "widgets::segmented", "widgets::checkbox"] {
            assert!(bar.contains(want), "missing {want}");
        }
    }
}
