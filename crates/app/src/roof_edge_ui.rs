// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roof-edge windows: command parameters (before create/arm, kept per session),
//! properties of an existing profile/lantern/soffit/wedge, the level's element list and the
//! profile section editor. Values are in display units (`units`); the model stores metres.

use crate::ui_kit as kit;
use ddc_ui::theme::{self, control, space};
use ddc_ui::widgets::{self, Button, TextField};
use ddc_ui::Theme;
use std::cell::RefCell;

use ddc_geom::sweep::{SectionProfile, SweepKind};
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, SweepHost};

use crate::editor::Editor;
use crate::roof_edge_tools::{self as tools, Cmd, Elem, Params};
use crate::shell::EditorApp;
use crate::units::{edit_length, parse_edit_length, unit_label, Units};

/// Edit text of a plain number: no groups (they would not read back), the UI decimal mark.
fn edit_num(v: f64, d: usize) -> String {
    crate::units::fmt_fixed_mark(v, d, crate::units::ui_decimal_mark())
}

fn t(key: &str) -> String {
    crate::tr(key, &[])
}

const F_W: &str = "roof_edge.f.width";
const F_L: &str = "roof_edge.f.length";
const F_CURB: &str = "roof_edge.f.curb";
const F_OFF: &str = "roof_edge.f.offset";
const F_RISE: &str = "roof_edge.f.rise";
const F_SLOPE: &str = "roof_edge.f.slope";
const F_TMIN: &str = "roof_edge.f.tmin";
const F_DROP: &str = "roof_edge.f.drop";
const F_TH: &str = "roof_edge.f.thickness";
const F_Z: &str = "roof_edge.f.z";
const F_BELOW: &str = "roof_edge.f.below_top";
const F_DX: &str = "roof_edge.f.dx";
const F_DY: &str = "roof_edge.f.dy";

// ---------------------------------------------------------------- field numbers

/// Unitless number: space separators, comma decimal, "%".
pub fn parse_plain(s: &str) -> Option<f64> {
    let t: String = s
        .trim()
        .trim_end_matches('%')
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}'))
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Signed length (zero allowed) in display units -> metres.
pub fn parse_signed(s: &str, u: Units) -> Option<f64> {
    let t = s.trim();
    if parse_plain(t) == Some(0.0) {
        return Some(0.0);
    }
    match t.strip_prefix('-') {
        Some(r) => parse_edit_length(r, u).map(|v| -v),
        None => parse_edit_length(t, u),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumKind {
    /// Length > 0.
    Len,
    /// Length of any sign.
    Signed,
    /// Length >= 0.
    NonNeg,
    /// Fraction, entered as a percentage, > 0.
    Pct,
}

/// Why a numeric field is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Number,
    Positive,
    NonNeg,
    Max,
}

impl Reject {
    pub fn key(self) -> &'static str {
        match self {
            Reject::Number => "roof_edge.err.number",
            Reject::Positive => "roof_edge.err.positive",
            Reject::NonNeg => "roof_edge.err.nonneg",
            Reject::Max => "roof_edge.err.max",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Num {
    pub key: &'static str,
    pub kind: NumKind,
    pub text: String,
    pub max: Option<f64>,
}

fn show(kind: NumKind, v: f64, u: Units) -> String {
    match kind {
        NumKind::Pct => edit_num(v * 100.0, 1),
        _ => edit_length(v, u),
    }
}

impl Num {
    pub fn new(key: &'static str, kind: NumKind, v: f64, u: Units) -> Num {
        Num {
            key,
            kind,
            text: show(kind, v, u),
            max: None,
        }
    }

    fn at_most(mut self, m: f64) -> Num {
        self.max = Some(m);
        self
    }

    pub fn parse(&self, u: Units) -> Option<f64> {
        self.check(u).ok()
    }

    /// Value in metres (or a fraction), or the reason the text is refused.
    pub fn check(&self, u: Units) -> Result<f64, Reject> {
        let v = match self.kind {
            NumKind::Len => match parse_edit_length(&self.text, u) {
                Some(v) => v,
                None if parse_signed(&self.text, u).is_some() => return Err(Reject::Positive),
                None => return Err(Reject::Number),
            },
            NumKind::Signed => parse_signed(&self.text, u).ok_or(Reject::Number)?,
            NumKind::NonNeg => match parse_signed(&self.text, u) {
                Some(v) if v >= 0.0 => v,
                Some(_) => return Err(Reject::NonNeg),
                None => return Err(Reject::Number),
            },
            NumKind::Pct => match parse_plain(&self.text) {
                Some(p) if p > 0.0 => p / 100.0,
                Some(_) => return Err(Reject::Positive),
                None => return Err(Reject::Number),
            },
        };
        if self.max.is_none_or(|m| v <= m + 1e-12) {
            Ok(v)
        } else {
            Err(Reject::Max)
        }
    }

    /// Plain-words reason for a refused text, in the given language.
    pub fn reason_lang(&self, ru: bool, r: Reject, u: Units) -> String {
        let max = match (r, self.max) {
            (Reject::Max, Some(m)) if self.kind == NumKind::Pct => {
                format!("{} %", crate::units::fmt_num_lang(m * 100.0, 1, ru))
            }
            (Reject::Max, Some(m)) => format!("{} {}", edit_length(m, u), unit_label(u)),
            _ => String::new(),
        };
        crate::tr_lang(ru, r.key(), &[("max", &max)])
    }

    fn reason(&self, r: Reject, u: Units) -> String {
        self.reason_lang(crate::ui_is_ru(), r, u)
    }

    fn unit(&self, u: Units) -> &'static str {
        match self.kind {
            NumKind::Pct => "%",
            _ => unit_label(u),
        }
    }
}

// ---------------------------------------------------------------- parameters/properties form

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Create(Cmd),
    Edit(Elem),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub goal: Goal,
    pub nums: Vec<Num>,
    pub profile: Option<String>,
    /// Covering type (vault): Some(choice) means the field is shown.
    pub stype: Option<Option<i64>>,
    pub side_left: Option<bool>,
    pub rakes: Option<bool>,
}

impl Form {
    fn bare(goal: Goal, nums: Vec<Num>) -> Form {
        Form {
            goal,
            nums,
            profile: None,
            stype: None,
            side_left: None,
            rakes: None,
        }
    }

    /// All fields parsed (in `nums` order); None if any is invalid.
    pub fn values(&self, u: Units) -> Option<Vec<f64>> {
        self.nums.iter().map(|n| n.parse(u)).collect()
    }

    fn val(&self, v: &[f64], key: &str) -> f64 {
        self.nums
            .iter()
            .position(|n| n.key == key)
            .and_then(|i| v.get(i))
            .copied()
            .unwrap_or(0.0)
    }

    /// Command parameter window built from the session parameters.
    pub fn create(cmd: Cmd, p: &Params, u: Units) -> Form {
        use NumKind::*;
        let g = Goal::Create(cmd);
        match cmd {
            Cmd::Skylight => Form::bare(
                g,
                vec![
                    Num::new(F_W, Len, p.sky.0, u),
                    Num::new(F_L, Len, p.sky.1, u),
                    Num::new(F_CURB, Len, p.sky.2, u),
                ],
            ),
            Cmd::Barrel => Form {
                stype: Some(p.slab_type),
                ..Form::bare(
                    g,
                    vec![
                        Num::new(F_OFF, Signed, p.barrel_offset, u),
                        Num::new(F_RISE, Pct, p.barrel_rise, u).at_most(0.4),
                    ],
                )
            },
            Cmd::Taper => Form::bare(
                g,
                vec![
                    Num::new(F_SLOPE, Pct, p.taper.0, u).at_most(1.0),
                    Num::new(F_TMIN, Len, p.taper.1, u),
                ],
            ),
            Cmd::Soffit => Form::bare(
                g,
                vec![
                    Num::new(F_DROP, NonNeg, p.soffit.0, u),
                    Num::new(F_TH, Len, p.soffit.1, u),
                ],
            ),
            _ => {
                let s = &p.sweeps[cmd.sweep_slot().unwrap_or(0)];
                let zk = if cmd == Cmd::Cornice { F_BELOW } else { F_Z };
                Form {
                    profile: Some(s.profile.clone()),
                    side_left: cmd.on_walls().then_some(s.side > 0),
                    rakes: matches!(cmd, Cmd::Fascia | Cmd::Gutter).then_some(s.rakes),
                    ..Form::bare(
                        g,
                        vec![
                            Num::new(zk, Signed, s.z, u),
                            Num::new(F_DX, Signed, s.dx, u),
                            Num::new(F_DY, Signed, s.dy, u),
                        ],
                    )
                }
            }
        }
    }

    /// Write the parsed values into the session parameters.
    pub fn store(&self, cmd: Cmd, v: &[f64], p: &mut Params) {
        let g = |k: &str| self.val(v, k);
        match cmd {
            Cmd::Skylight => p.sky = (g(F_W), g(F_L), g(F_CURB)),
            Cmd::Barrel => {
                p.barrel_offset = g(F_OFF);
                p.barrel_rise = g(F_RISE);
                if let Some(s) = self.stype {
                    p.slab_type = s;
                }
            }
            Cmd::Taper => p.taper = (g(F_SLOPE), g(F_TMIN)),
            Cmd::Soffit => p.soffit = (g(F_DROP), g(F_TH)),
            _ => {
                let Some(i) = cmd.sweep_slot() else {
                    return;
                };
                let s = &mut p.sweeps[i];
                if let Some(pr) = &self.profile {
                    s.profile = pr.clone();
                }
                s.z = g(if cmd == Cmd::Cornice { F_BELOW } else { F_Z });
                s.dx = g(F_DX);
                s.dy = g(F_DY);
                if let Some(l) = self.side_left {
                    s.side = if l { 1 } else { -1 };
                }
                if let Some(r) = self.rakes {
                    s.rakes = r;
                }
            }
        }
    }

    /// Properties window of an existing element.
    pub fn edit(m: &Model, e: Elem, u: Units) -> Result<Form, Error> {
        use NumKind::*;
        let g = Goal::Edit(e);
        Ok(match e {
            Elem::Sweep(id) => {
                let s = m.sweep(id)?;
                Form {
                    profile: Some(s.profile.clone()),
                    side_left: matches!(s.host, SweepHost::Walls(_)).then_some(s.side > 0),
                    rakes: match s.host {
                        SweepHost::Roof { rakes, .. } => Some(rakes),
                        _ => None,
                    },
                    ..Form::bare(
                        g,
                        vec![
                            Num::new(F_Z, Signed, s.z, u),
                            Num::new(F_DX, Signed, s.dx, u),
                            Num::new(F_DY, Signed, s.dy, u),
                        ],
                    )
                }
            }
            Elem::Skylight(id) => {
                let k = m.skylight(id)?;
                Form::bare(
                    g,
                    vec![
                        Num::new(F_W, Len, k.width, u),
                        Num::new(F_L, Len, k.length, u),
                        Num::new(F_CURB, Len, k.curb, u),
                    ],
                )
            }
            Elem::Soffit(id) => {
                let (_, dh, th, _) = m.soffit(id)?;
                Form::bare(
                    g,
                    vec![Num::new(F_DROP, NonNeg, dh, u), Num::new(F_TH, Len, th, u)],
                )
            }
            Elem::Taper(id) => {
                let (s, _) = m.taper(id)?;
                Form::bare(
                    g,
                    vec![
                        Num::new(F_SLOPE, Pct, s.slope, u).at_most(1.0),
                        Num::new(F_TMIN, Len, s.t_min, u),
                        Num::new(F_OFF, Signed, s.offset, u),
                    ],
                )
            }
        })
    }

    fn title(&self) -> String {
        match self.goal {
            Goal::Create(c) => {
                format!("{} — {}", t(&format!("cmd.{}", c.id())), t("roof_edge.dlg.params"))
            }
            Goal::Edit(e) => format!("{} — {}", elem_label(e, ""), t("roof_edge.dlg.props")),
        }
    }
}

fn elem_label(e: Elem, extra: &str) -> String {
    let base = format!("{} #{}", t(&format!("roof_edge.k.{}", e.kind())), e.id());
    if extra.is_empty() {
        base
    } else {
        format!("{base} · {extra}")
    }
}

fn gone(ed: &mut Editor) -> bool {
    ed.status = Some(t("roof_edge.gone"));
    false
}

/// Write the element's properties as a single model operation.
pub fn commit(ed: &mut Editor, e: Elem, f: &Form, v: &[f64]) -> bool {
    let g = |k: &str| f.val(v, k);
    match e {
        Elem::Sweep(id) => {
            let Ok(mut s) = ed.model.sweep(id) else {
                return gone(ed);
            };
            if let Some(p) = &f.profile {
                s.profile = p.clone();
            }
            s.z = g(F_Z);
            s.dx = g(F_DX);
            s.dy = g(F_DY);
            if let Some(l) = f.side_left {
                s.side = if l { 1 } else { -1 };
            }
            if let (Some(r), SweepHost::Roof { rakes, .. }) = (f.rakes, &mut s.host) {
                *rakes = r;
            }
            tools::run(ed, "roof_edge.done", move |m| m.update_sweep(id, &s))
        }
        Elem::Skylight(id) => {
            let Ok(mut k) = ed.model.skylight(id) else {
                return gone(ed);
            };
            k.width = g(F_W);
            k.length = g(F_L);
            k.curb = g(F_CURB);
            tools::run(ed, "roof_edge.done", move |m| m.update_skylight(id, &k))
        }
        Elem::Soffit(id) => {
            let Ok((_, _, _, mat)) = ed.model.soffit(id) else {
                return gone(ed);
            };
            let (d, th) = (g(F_DROP), g(F_TH));
            tools::run(ed, "roof_edge.done", move |m| {
                m.update_soffit(id, d, th, mat.as_deref())
            })
        }
        Elem::Taper(id) => {
            let Ok((mut s, _)) = ed.model.taper(id) else {
                return gone(ed);
            };
            s.slope = g(F_SLOPE);
            s.t_min = g(F_TMIN);
            s.offset = g(F_OFF);
            tools::run(ed, "roof_edge.done", move |m| m.update_taper(id, &s))
        }
    }
}

/// Form OK: session parameters + command, or an element edit.
pub fn submit(ed: &mut Editor, f: &Form, v: &[f64]) -> bool {
    match f.goal {
        Goal::Create(cmd) => {
            let mut p = tools::params();
            f.store(cmd, v, &mut p);
            tools::set_params(p.clone());
            tools::exec(ed, cmd, &p)
        }
        Goal::Edit(e) => commit(ed, e, f, v),
    }
}

// ---------------------------------------------------------------- section editor

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowOp {
    /// Point after i (edge midpoint; an arc is bisected).
    Insert(usize),
    Remove(usize),
    Up(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileEd {
    pub name: String,
    pub kind: SweepKind,
    /// x, y (display units), bulge of the edge to the next point.
    pub rows: Vec<[String; 3]>,
    /// Delete was asked for and waits for the confirming click.
    pub armed: bool,
}

/// (can remove, can move up) for point `i` of `n`: a section keeps at least 3 points.
pub fn row_actions(n: usize, i: usize) -> (bool, bool) {
    (n > 3, i > 0)
}

/// Whether the text of column `c` (0 x, 1 y, 2 bulge) is a usable number.
pub fn cell_ok(c: usize, text: &str, u: Units) -> bool {
    if c == 2 {
        parse_plain(text).is_some()
    } else {
        parse_signed(text, u).is_some()
    }
}

/// What deleting a library profile does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileDel {
    /// Not saved in the file: nothing to delete.
    NotSaved,
    /// Refused: this many edge elements still use it.
    InUse(i64),
    Ok,
}

pub fn profile_del_state(in_file: bool, used: i64) -> ProfileDel {
    if !in_file {
        ProfileDel::NotSaved
    } else if used > 0 {
        ProfileDel::InUse(used)
    } else {
        ProfileDel::Ok
    }
}

/// Number of sweeps that use the profile.
pub fn profile_usage(m: &Model, name: &str) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM sweep WHERE profile=?1", [name], |r| {
            r.get(0)
        })
        .unwrap_or(0)
}

pub fn profile_state(m: &Model, name: &str) -> ProfileDel {
    let in_file: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM sweep_profile WHERE name=?1",
            [name],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // a file copy of a built-in profile can always go: its users fall back to the built-in
    let builtin = ddc_geom::sweep::library().iter().any(|l| l.0 == name);
    let used = if builtin { 0 } else { profile_usage(m, name) };
    profile_del_state(in_file > 0, used)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelAct {
    Ask,
    Confirm,
    Cancel,
}

/// Delete of a library profile in two steps; only the confirming click deletes, and a profile
/// in use is refused with its count. True when something was deleted.
pub fn profile_step(ed: &mut Editor, name: &str, armed: &mut bool, act: DelAct) -> bool {
    match act {
        DelAct::Cancel => {
            *armed = false;
            false
        }
        DelAct::Ask => {
            *armed = profile_state(&ed.model, name) == ProfileDel::Ok;
            false
        }
        DelAct::Confirm => {
            let go = std::mem::take(armed) && profile_state(&ed.model, name) == ProfileDel::Ok;
            if !go {
                return false;
            }
            let n = name.to_string();
            let ok = tools::run(ed, "roof_edge.profile_deleted", move |m| {
                m.delete_sweep_profile(&n)
            });
            if ok {
                ed.offer_undo();
            }
            ok
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListAct {
    Ask(Elem),
    Confirm(Elem),
    Cancel,
}

/// Delete of a list element in two steps (one undo step); Confirm without a prior Ask does
/// nothing. True when something was deleted.
pub fn list_step(ed: &mut Editor, pending: &mut Option<Elem>, act: ListAct) -> bool {
    match act {
        ListAct::Cancel => {
            *pending = None;
            false
        }
        ListAct::Ask(e) => {
            *pending = Some(e);
            false
        }
        ListAct::Confirm(e) => {
            if pending.take() != Some(e) {
                return false;
            }
            tools::delete_elems(ed, &[e])
        }
    }
}

fn row(x: f64, y: f64, b: f64, u: Units) -> [String; 3] {
    [edit_length(x, u), edit_length(y, u), edit_num(b, 4)]
}

fn point(r: &[String; 3], u: Units) -> Option<(f64, f64, f64)> {
    Some((
        parse_signed(&r[0], u)?,
        parse_signed(&r[1], u)?,
        parse_plain(&r[2])?,
    ))
}

impl ProfileEd {
    pub fn from_profile(name: &str, kind: SweepKind, p: &SectionProfile, u: Units) -> Self {
        ProfileEd {
            name: name.to_string(),
            kind,
            rows: p.pts.iter().map(|q| row(q.0, q.1, q.2, u)).collect(),
            armed: false,
        }
    }

    /// First section of the file's library (or a 100x20 rectangle).
    pub fn first(m: &Model, u: Units) -> Self {
        match m.sweep_profiles().unwrap_or_default().into_iter().next() {
            Some((n, k, p, _)) => Self::from_profile(&n, k, &p, u),
            None => Self::from_profile(
                "profile",
                SweepKind::WallSweep,
                &SectionProfile::rect(0.0, 0.0, 0.1, 0.02),
                u,
            ),
        }
    }

    pub fn profile(&self, u: Units) -> Option<SectionProfile> {
        let pts: Option<Vec<(f64, f64, f64)>> = self.rows.iter().map(|r| point(r, u)).collect();
        pts.map(|pts| SectionProfile { pts })
    }

    /// (message key, can be saved).
    pub fn check(&self, u: Units) -> (&'static str, bool) {
        match self.profile(u) {
            None => ("roof_edge.pe.bad_number", false),
            Some(p) if p.validate().is_err() => ("roof_edge.pe.invalid", false),
            Some(_) if self.name.trim().is_empty() => ("roof_edge.pe.no_name", false),
            Some(_) => ("roof_edge.pe.valid", true),
        }
    }

    pub fn apply(&mut self, op: RowOp, u: Units) {
        let n = self.rows.len();
        match op {
            RowOp::Insert(i) if i < n => {
                let j = (i + 1) % n;
                let r = match (point(&self.rows[i], u), point(&self.rows[j], u)) {
                    (Some(a), Some(b)) => {
                        let (m, h) = tools::split_arc(v2(a.0, a.1), v2(b.0, b.1), a.2);
                        self.rows[i][2] = edit_num(h, 4);
                        row(m.x, m.y, h, u)
                    }
                    _ => self.rows[i].clone(),
                };
                self.rows.insert(i + 1, r);
            }
            RowOp::Remove(i) if i < n && row_actions(n, i).0 => {
                self.rows.remove(i);
            }
            RowOp::Up(i) if row_actions(n, i).1 && i < n => self.rows.swap(i - 1, i),
            _ => {}
        }
    }
}

fn kind_label(k: SweepKind) -> String {
    t(&format!("roof_edge.sk.{}", k.name()))
}

/// Section preview: dropdown-wide plus a hit target, 16:10 so a profile reads at a glance.
const PREVIEW: egui::Vec2 = egui::vec2(control::DROPDOWN_W * 1.625, control::DROPDOWN_W);

fn preview(ui: &mut egui::Ui, p: Option<&SectionProfile>) {
    let (rect, _) = ui.allocate_exact_size(PREVIEW, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();
    painter.rect_stroke(
        rect,
        2.0,
        vis.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let Some(p) = p else {
        return;
    };
    let o = p.outline();
    if o.len() < 2 {
        return;
    }
    // the frame includes the host face x = 0
    let (mut lo, mut hi) = (v2(0.0, 0.0), v2(0.0, 0.0));
    for q in &o {
        lo = v2(lo.x.min(q.x), lo.y.min(q.y));
        hi = v2(hi.x.max(q.x), hi.y.max(q.y));
    }
    let size = (hi.x - lo.x).max(hi.y - lo.y).max(1e-6);
    let s = f64::from(rect.width().min(rect.height()) - 16.0) / size;
    let (c, mid) = (rect.center(), (lo + hi) * 0.5);
    let to = |q: V2| {
        egui::pos2(
            c.x + ((q.x - mid.x) * s) as f32,
            c.y - ((q.y - mid.y) * s) as f32,
        )
    };
    painter.line_segment(
        [to(v2(0.0, lo.y)), to(v2(0.0, hi.y))],
        egui::Stroke::new(1.0, vis.weak_text_color()),
    );
    painter.add(egui::Shape::closed_line(
        o.iter().map(|&q| to(q)).collect(),
        egui::Stroke::new(1.5, vis.strong_text_color()),
    ));
}

// ---------------------------------------------------------------- windows

#[derive(Default)]
struct UiState {
    form: Option<Form>,
    list: bool,
    /// List element waiting for the confirming click.
    confirm: Option<Elem>,
    profiles: Option<ProfileEd>,
}

/// Width of every field and dropdown in these dialogs.
const FIELD_W: f32 = control::DROPDOWN_W;
/// Width of a point-table number cell and of the row-number column.
const CELL_W: f32 = control::FIELD_MIN_W * 1.5;
const NO_W: f32 = control::HIT_MIN;

/// Square icon button; false clicks while disabled.
fn icon_btn(ui: &mut egui::Ui, th: &Theme, icon: &str, enabled: bool, tip: &str) -> bool {
    let ctx = ui.ctx().clone();
    ui.add_enabled_ui(enabled, |ui| {
        widgets::with_icons(&ctx, |icons| {
            widgets::icon_button(ui, icons, th, icon, false, tip)
        })
    })
    .inner
    .clicked()
}

/// Quiet destructive delete icon (danger glyph on hover); false clicks while disabled.
fn icon_del(ui: &mut egui::Ui, th: &Theme, enabled: bool, tip: &str) -> bool {
    let ctx = ui.ctx().clone();
    ui.add_enabled_ui(enabled, |ui| {
        widgets::with_icons(&ctx, |icons| {
            widgets::icon_button_danger(ui, icons, th, "delete", tip)
        })
    })
    .inner
    .clicked()
}

/// Dropdown row over owned strings: the index picked, if it moved.
fn pick_row(
    ui: &mut egui::Ui,
    th: &Theme,
    label: &str,
    salt: &str,
    items: &[String],
    sel: usize,
) -> Option<usize> {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let mut i = sel;
    let moved = kit::labeled_row(ui, label, |ui| {
        widgets::dropdown(ui, th, salt, &mut i, &refs, Some(FIELD_W)).changed()
    });
    moved.then_some(i)
}

/// "Delete X?" strip: the answer is Some(true) on the confirming click, Some(false) on cancel.
/// Delete is never the default: Enter does nothing here.
fn confirm_strip(ui: &mut egui::Ui, text: &str) -> Option<bool> {
    let mut ans = None;
    kit::warn_note(ui, text);
    ui.horizontal(|ui| {
        if kit::confirm_danger_button(ui, t("roof_edge.delete"), true).clicked() {
            ans = Some(true);
        }
        if kit::cancel_button(ui, t("roof_edge.cancel")).clicked() {
            ans = Some(false);
        }
    });
    ans
}

thread_local! {
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
}

/// Command with parameters: preconditions, then the parameter window (OK creates/arms).
pub fn open_cmd(app: &mut EditorApp, cmd: Cmd) {
    tools::reset();
    if !tools::precheck(&mut app.ed, cmd) {
        return;
    }
    let f = Form::create(cmd, &tools::params(), app.ed.tool.units);
    UI.with(|s| s.borrow_mut().form = Some(f));
}

/// Element properties window (from the list or from a 3D selection).
pub fn open_props(ed: &mut Editor, e: Elem) {
    match Form::edit(&ed.model, e, ed.tool.units) {
        Ok(f) => UI.with(|s| s.borrow_mut().form = Some(f)),
        Err(err) => ed.status = Some(format!("{}: {err}", t("roof_edge.failed"))),
    }
}

pub fn open_list(_app: &mut EditorApp) {
    UI.with(|s| s.borrow_mut().list = true);
}

pub fn open_profiles(app: &mut EditorApp) {
    let pe = ProfileEd::first(&app.ed.model, app.ed.tool.units);
    UI.with(|s| s.borrow_mut().profiles = Some(pe));
}

/// Per-frame roof-edge windows (called from `EditorApp::ui`).
pub fn ui(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = UI.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if st.form.is_none() && !st.list && st.profiles.is_none() {
        return;
    }
    if let Some(f) = st.form.take() {
        st.form = form_window(app, ctx, f);
    }
    if st.list {
        st.list = list_window(app, ctx, &mut st.form, &mut st.confirm);
        if !st.list {
            st.confirm = None;
        }
    }
    if let Some(p) = st.profiles.take() {
        st.profiles = profile_window(app, ctx, p);
    }
    UI.with(|s| {
        let b = &mut *s.borrow_mut();
        // a window opened externally this frame takes precedence
        if b.form.is_none() {
            b.form = st.form;
        }
        b.list |= st.list;
        if b.confirm.is_none() {
            b.confirm = st.confirm;
        }
        if b.profiles.is_none() {
            b.profiles = st.profiles;
        }
    });
}

fn form_window(app: &mut EditorApp, ctx: &egui::Context, mut f: Form) -> Option<Form> {
    let u = app.ed.tool.units;
    let profiles: Vec<String> = if f.profile.is_some() {
        let lib = app.ed.model.sweep_profiles().unwrap_or_default();
        lib.into_iter().map(|p| p.0).collect()
    } else {
        vec![]
    };
    let types = if f.stype.is_some() {
        tools::slab_types(&app.ed.model)
    } else {
        vec![]
    };
    let th = theme::of(ctx);
    let first_bad: Option<String> = f.nums.iter().find_map(|n| {
        n.check(u)
            .err()
            .map(|r| format!("{}: {}", t(n.key), n.reason(r, u)))
    });
    let (mut open, mut ok, mut cancel) = (true, false, false);
    let mut drains: Option<i64> = None;
    let title = f.title();
    kit::Dialog::new("roof_edge_form", title, kit::DialogSize::Confirm)
        .open(&mut open)
        .show(ctx, |ui| {
            if let Some(p) = &mut f.profile {
                let mut names = profiles.clone();
                if !names.contains(p) {
                    names.insert(0, p.clone());
                }
                let cur = names.iter().position(|x| x == p).unwrap_or(0);
                if let Some(i) = pick_row(ui, &th, &t("roof_edge.f.profile"), "g7.form.profile", &names, cur)
                {
                    *p = names[i].clone();
                }
            }
            if let Some(sel) = &mut f.stype {
                if !types.is_empty() {
                    let names: Vec<String> = types.iter().map(|x| x.1.clone()).collect();
                    let cur = types.iter().position(|x| Some(x.0) == *sel).unwrap_or(0);
                    if let Some(i) =
                        pick_row(ui, &th, &t("roof_edge.f.type"), "g7.form.type", &names, cur)
                    {
                        *sel = Some(types[i].0);
                    }
                }
            }
            for n in &mut f.nums {
                let label = t(n.key);
                let res = n.check(u);
                let reason = res.err().map(|r| n.reason(r, u)).unwrap_or_default();
                let unit = n.unit(u);
                let key = n.key;
                kit::labeled_row_id(ui, &label, |ui, lid| {
                    let out = TextField::new(&mut n.text)
                        .id_salt(key)
                        .labelled_by(lid, &label)
                        .error(res.is_err())
                        .error_text(&reason)
                        .unit(unit)
                        .right_aligned()
                        .width(FIELD_W)
                        .show(ui, &th);
                    if res.is_err() {
                        out.response.on_hover_text(reason.as_str());
                    }
                });
            }
            if let Some(b) = &mut f.side_left {
                kit::labeled_row(ui, "", |ui| {
                    widgets::checkbox(ui, &th, b, &t("roof_edge.f.side_left"));
                });
            }
            if let Some(b) = &mut f.rakes {
                kit::labeled_row(ui, "", |ui| {
                    widgets::checkbox(ui, &th, b, &t("roof_edge.f.rakes"));
                });
            }
            if let Some(m) = &first_bad {
                ui.add_space(space::XS);
                kit::error_note(ui, m);
            }
            if let Goal::Edit(Elem::Taper(id)) = f.goal {
                ui.add_space(space::SM);
                if Button::secondary(&t("roof_edge.add_drains")).show(ui, &th).clicked() {
                    drains = Some(id);
                }
            }
            ui.add_space(space::SM);
            kit::dialog_footer(ui, |ui| {
                if kit::primary_action(ui, t("roof_edge.ok"), first_bad.as_deref()) {
                    ok = true;
                }
                if kit::cancel_button(ui, t("roof_edge.cancel")).clicked() {
                    cancel = true;
                }
            });
        });
    if let Some(id) = drains {
        tools::arm_drains(&mut app.ed, id);
        return None;
    }
    if ok {
        if let Some(v) = f.values(u) {
            submit(&mut app.ed, &f, &v);
            return None;
        }
    }
    (open && !cancel).then_some(f)
}

/// Edge elements of the level: (element, description).
pub fn level_elems(m: &Model, level: i64) -> Vec<(Elem, String)> {
    let mut out = vec![];
    for id in m.sweeps(Some(level)).unwrap_or_default() {
        let extra = m.sweep(id).map(|s| s.profile).unwrap_or_default();
        out.push((Elem::Sweep(id), extra));
    }
    for id in m.level_skylights(level).unwrap_or_default() {
        out.push((Elem::Skylight(id), String::new()));
    }
    for id in m.level_soffits(level).unwrap_or_default() {
        out.push((Elem::Soffit(id), String::new()));
    }
    for id in m.tapers(level).unwrap_or_default() {
        out.push((Elem::Taper(id), String::new()));
    }
    out
}

fn list_window(
    app: &mut EditorApp,
    ctx: &egui::Context,
    form: &mut Option<Form>,
    confirm: &mut Option<Elem>,
) -> bool {
    let items = level_elems(&app.ed.model, app.ed.level);
    if confirm.is_some_and(|c| !items.iter().any(|i| i.0 == c)) {
        *confirm = None;
    }
    let th = theme::of(ctx);
    let mut open = true;
    let mut edit: Option<Elem> = None;
    let mut del: Option<ListAct> = None;
    let asking = *confirm;
    kit::Dialog::new(
        "roof_edges",
        t("cmd.arch.roof_edges"),
        kit::DialogSize::Tool,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        if let Some(c) = asking {
            let text = crate::tr("roof_edge.confirm_edge", &[("name", &elem_label(c, ""))]);
            match confirm_strip(ui, &text) {
                Some(true) => del = Some(ListAct::Confirm(c)),
                Some(false) => del = Some(ListAct::Cancel),
                None => {}
            }
            ui.add_space(space::SM);
        }
        if items.is_empty() {
            kit::empty_note(ui, t("roof_edge.list.empty"), Some(&t("roof_edge.list.empty_hint")));
        }
        let btn_w = th.d().control + 4.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (e, extra) in &items {
                    let label = elem_label(*e, extra);
                    kit::fixed_row(ui, &format!("g7.list.{}.{}", e.kind(), e.id()), |ui| {
                        let w = ui.available_width() - 2.0 * (btn_w + ui.spacing().item_spacing.x);
                        kit::fixed_cell(ui, w, |ui| {
                            kit::name_with_tip(ui, &label, &label, false);
                        });
                        if icon_btn(ui, &th, "properties", true, &t("roof_edge.edit_tip")) {
                            edit = Some(*e);
                        }
                        if icon_del(ui, &th, true, &t("roof_edge.delete_tip")) {
                            del = Some(ListAct::Ask(*e));
                        }
                    });
                }
            });
    });
    if let Some(e) = edit {
        match Form::edit(&app.ed.model, e, app.ed.tool.units) {
            Ok(f) => *form = Some(f),
            Err(err) => app.ed.status = Some(format!("{}: {err}", t("roof_edge.failed"))),
        }
    }
    if let Some(a) = del {
        list_step(&mut app.ed, confirm, a);
    }
    open
}

fn profile_window(
    app: &mut EditorApp,
    ctx: &egui::Context,
    mut pe: ProfileEd,
) -> Option<ProfileEd> {
    let u = app.ed.tool.units;
    let th = theme::of(ctx);
    let lib = app.ed.model.sweep_profiles().unwrap_or_default();
    let parsed = pe.profile(u);
    let (msg, valid) = pe.check(u);
    let state = profile_state(&app.ed.model, pe.name.trim());
    if state != ProfileDel::Ok {
        pe.armed = false;
    }
    let lib_names: Vec<String> = lib.iter().map(|l| l.0.clone()).collect();
    let kind_names: Vec<String> = SweepKind::ALL.iter().map(|k| kind_label(*k)).collect();
    let mut load: Option<usize> = None;
    let mut op: Option<RowOp> = None;
    let mut del: Option<DelAct> = None;
    let (mut open, mut save) = (true, false);
    let n = pe.rows.len();
    let unit = unit_label(u);
    let (x_head, y_head) = (
        format!("{} {unit}", t("roof_edge.pe.x")),
        format!("{} {unit}", t("roof_edge.pe.y")),
    );
    let bulge_head = t("roof_edge.pe.bulge");
    let bad_cell = t("roof_edge.pe.bad_number");
    let name_before = pe.name.clone();
    let del_reason = match state {
        ProfileDel::NotSaved => t("dlg.roof.not_saved"),
        ProfileDel::InUse(k) => crate::tr_n("roof_edge.profile_in_use", k.max(0) as u64),
        ProfileDel::Ok => String::new(),
    };
    kit::Dialog::new(
        "sweep_profiles",
        t("cmd.arch.sweep_profiles"),
        kit::DialogSize::Form,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        let cur = lib_names
            .iter()
            .position(|x| x == pe.name.trim())
            .unwrap_or(usize::MAX);
        if let Some(i) = pick_row(ui, &th, &t("roof_edge.pe.load"), "roof_edge.pe.load", &lib_names, cur) {
            load = Some(i);
        }
        let name_label = t("roof_edge.pe.name");
        let no_name = pe.name.trim().is_empty();
        kit::labeled_row_id(ui, &name_label, |ui, lid| {
            TextField::new(&mut pe.name)
                .id_salt("roof_edge.pe.name")
                .labelled_by(lid, &name_label)
                .error(no_name)
                .error_text(&t("roof_edge.pe.no_name"))
                .width(FIELD_W)
                .show(ui, &th);
        });
        let kind_cur = SweepKind::ALL
            .iter()
            .position(|k| *k == pe.kind)
            .unwrap_or(0);
        if let Some(i) = pick_row(ui, &th, &t("roof_edge.pe.kind"), "roof_edge.pe.kind", &kind_names, kind_cur) {
            pe.kind = SweepKind::ALL[i];
        }
        ui.add_space(space::SM);
        ui.separator();
        // the column headers name the cells under them
        let mut head_ids = [egui::Id::NULL; 3];
        kit::fixed_row(ui, "g7.pe.head", |ui| {
            kit::fixed_cell(ui, NO_W, |ui| {
                kit::elided_weak(ui, &t("roof_edge.pe.n"));
            });
            let hs = [x_head.as_str(), y_head.as_str(), bulge_head.as_str()];
            for (h, hid) in hs.into_iter().zip(&mut head_ids) {
                kit::fixed_cell(ui, CELL_W, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        *hid = kit::elided_weak(ui, h).id;
                    });
                });
            }
        });
        let heads = [x_head.as_str(), y_head.as_str(), bulge_head.as_str()];
        egui::ScrollArea::vertical()
            .id_salt("g7.pe.pts")
            .max_height(th.d().row * 8.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, r) in pe.rows.iter_mut().enumerate() {
                    kit::fixed_row(ui, &format!("g7.pe.row.{i}"), |ui| {
                        kit::fixed_cell(ui, NO_W, |ui| {
                            kit::elided_weak(ui, &(i + 1).to_string());
                        });
                        for (c, cell) in r.iter_mut().enumerate() {
                            let bad = !cell_ok(c, cell, u);
                            let out = TextField::new(cell)
                                .labelled_by(head_ids[c], heads[c])
                                .id_salt(("g7.pe.cell", i, c))
                                .error(bad)
                                .error_text(&bad_cell)
                                .right_aligned()
                                .width(CELL_W)
                                .show(ui, &th);
                            if bad {
                                out.response.on_hover_text(bad_cell.as_str());
                            }
                        }
                        let (can_del, can_up) = row_actions(n, i);
                        if icon_btn(ui, &th, "plus", true, &t("roof_edge.pe.ins_tip")) {
                            op = Some(RowOp::Insert(i));
                        }
                        if icon_del(ui, &th, can_del, &t("roof_edge.pe.del_tip")) {
                            op = Some(RowOp::Remove(i));
                        }
                        if icon_btn(ui, &th, "move-up", can_up, &t("roof_edge.pe.up_tip")) {
                            op = Some(RowOp::Up(i));
                        }
                    });
                }
            });
        ui.separator();
        preview(ui, parsed.as_ref());
        ui.add_space(space::XS);
        if valid {
            kit::elided_weak(ui, &t(msg));
        } else {
            kit::error_note(ui, &t(msg));
        }
        if let ProfileDel::InUse(_) = state {
            kit::warn_note(ui, &del_reason);
        }
        if pe.armed {
            let text = crate::tr("roof_edge.confirm_profile", &[("name", pe.name.trim())]);
            match confirm_strip(ui, &text) {
                Some(true) => del = Some(DelAct::Confirm),
                Some(false) => del = Some(DelAct::Cancel),
                None => {}
            }
        }
        kit::dialog_footer(ui, |ui| {
            let why = t(msg);
            if kit::primary_action_table(ui, t("roof_edge.pe.save"), (!valid).then_some(why.as_str())) {
                save = true;
            }
            if kit::danger_button(ui, t("roof_edge.delete"), state == ProfileDel::Ok && !pe.armed)
                .on_disabled_hover_text(del_reason.as_str())
                .clicked()
            {
                del = Some(DelAct::Ask);
            }
        });
    });
    if let Some(i) = load {
        let l = &lib[i];
        pe = ProfileEd::from_profile(&l.0, l.1, &l.2, u);
    }
    if let Some(op) = op {
        pe.apply(op, u);
    }
    if save {
        if let Some(p) = pe.profile(u) {
            let (name, kind) = (pe.name.trim().to_string(), pe.kind);
            tools::run(&mut app.ed, "roof_edge.pe.saved", move |m| {
                m.set_sweep_profile(&name, kind, &p)
            });
        }
    }
    if pe.name != name_before {
        pe.armed = false;
    }
    if let Some(a) = del {
        let name = pe.name.trim().to_string();
        profile_step(&mut app.ed, &name, &mut pe.armed, a);
    }
    open.then_some(pe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roof_edge_tools::tests::with_roof;
    use ddc_ui::i18n::{Catalog, Lang};

    const M: Units = Units::Metric;

    fn near(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-12)
    }

    fn same(a: &SectionProfile, b: &SectionProfile) -> bool {
        a.pts.len() == b.pts.len()
            && a.pts.iter().zip(&b.pts).all(|(p, q)| {
                (p.0 - q.0).abs() < 1e-12 && (p.1 - q.1).abs() < 1e-12 && (p.2 - q.2).abs() < 1e-12
            })
    }

    #[test]
    fn numbers_in_display_units() {
        assert!(near(parse_signed("-150", M), -0.15));
        assert_eq!(parse_signed("0", M), Some(0.0));
        assert!(near(parse_signed("1 200", M), 1.2));
        assert_eq!(parse_signed("abc", M), None);
        assert_eq!(parse_plain("2,5 %"), Some(2.5));
        let n = Num::new(F_RISE, NumKind::Pct, 0.25, M).at_most(0.4);
        assert!(near(n.parse(M), 0.25));
        let big = Num {
            text: "45".into(),
            ..n.clone()
        };
        assert_eq!(big.parse(M), None, "rise above 40 % of span");
        let neg = Num::new(F_DROP, NumKind::NonNeg, -0.1, M);
        assert_eq!(neg.parse(M), None);
        assert_eq!(Num::new(F_TH, NumKind::Len, 0.02, M).text, "20");
    }

    #[test]
    fn create_form_round_trips_session_params() {
        let p = Params::default();
        for cmd in Cmd::ALL {
            let f = Form::create(cmd, &p, M);
            let v = f
                .values(M)
                .unwrap_or_else(|| panic!("{cmd:?} defaults parse"));
            let mut q = p.clone();
            f.store(cmd, &v, &mut q);
            assert_eq!(
                Form::create(cmd, &q, M),
                f,
                "{cmd:?}: defaults survive the form"
            );
        }
        let mut f = Form::create(Cmd::Skylight, &p, M);
        f.nums[0].text = "900".into();
        let v = f.values(M).unwrap();
        let mut q = p.clone();
        f.store(Cmd::Skylight, &v, &mut q);
        assert!(near(Some(q.sky.0), 0.9) && near(Some(q.sky.1), 1.18) && near(Some(q.sky.2), 0.15));
        f.nums[1].text = "x".into();
        assert!(f.values(M).is_none());
        let mut w = Form::create(Cmd::Reveal, &p, M);
        w.side_left = Some(true);
        w.profile = Some("reveal_v_30".into());
        let v = w.values(M).unwrap();
        w.store(Cmd::Reveal, &v, &mut q);
        let s = &q.sweeps[Cmd::Reveal.sweep_slot().unwrap()];
        assert_eq!((s.side, s.profile.as_str()), (1, "reveal_v_30"));
        assert!(near(Some(s.z), 1.0));
    }

    #[test]
    fn properties_edit_soffit_and_skylight_in_one_undo_step() {
        let (mut ed, _, roof) = with_roof();
        assert!(tools::exec(&mut ed, Cmd::Soffit, &Params::default()));
        let so = ed.model.soffits(roof).unwrap()[0];
        let mut f = Form::edit(&ed.model, Elem::Soffit(so), M).unwrap();
        f.nums[1].text = "40".into();
        let v = f.values(M).unwrap();
        assert!(submit(&mut ed, &f, &v));
        assert!(near(Some(ed.model.soffit(so).unwrap().2), 0.04));
        ed.undo();
        assert_eq!(ed.model.soffit(so).unwrap().2, 0.02);
        assert_eq!(
            level_elems(&ed.model, ed.level),
            vec![(Elem::Soffit(so), String::new())]
        );
        assert!(
            !commit(&mut ed, Elem::Skylight(999), &f, &v),
            "missing element"
        );
    }

    #[test]
    fn profile_editor_rows_validate_and_split_arcs() {
        let r = SectionProfile::rect(0.0, 0.0, 0.1, 0.02);
        let mut pe = ProfileEd::from_profile("p", SweepKind::WallSweep, &r, M);
        assert_eq!(pe.check(M), ("roof_edge.pe.valid", true));
        assert!(same(&pe.profile(M).unwrap(), &r));
        pe.apply(RowOp::Insert(0), M);
        assert_eq!(pe.rows.len(), 5);
        assert_eq!(pe.rows[1][0], "50");
        assert!(pe.profile(M).unwrap().validate().is_ok());
        pe.apply(RowOp::Remove(1), M);
        assert!(same(&pe.profile(M).unwrap(), &r));
        pe.apply(RowOp::Up(1), M);
        assert_eq!(pe.check(M), ("roof_edge.pe.invalid", false), "bow-tie after swap");
        pe.apply(RowOp::Up(1), M);
        assert!(same(&pe.profile(M).unwrap(), &r));
        pe.rows[0][2] = "1".into();
        pe.apply(RowOp::Insert(0), M);
        let p = pe.profile(M).unwrap();
        assert!((p.pts[0].2 - 0.4142).abs() < 1e-9, "half-angle bulge");
        assert!((p.pts[1].0 - 0.05).abs() < 1e-9 && (p.pts[1].1 + 0.05).abs() < 1e-9);
        pe.rows.truncate(2);
        pe.apply(RowOp::Remove(0), M);
        assert_eq!(pe.rows.len(), 2, "never below the minimum by removal");
        pe.rows[0][0] = "?".into();
        assert_eq!(pe.check(M).0, "roof_edge.pe.bad_number");
    }

    #[test]
    fn profile_saved_and_deleted_through_model() {
        let (mut ed, ..) = with_roof();
        let mut pe = ProfileEd::first(&ed.model, M);
        pe.name = "my_band".into();
        let p = pe.profile(M).unwrap();
        let k = pe.kind;
        assert!(tools::run(&mut ed, "roof_edge.pe.saved", move |m| {
            m.set_sweep_profile("my_band", k, &p)
        }));
        assert!(ed
            .model
            .sweep_profiles()
            .unwrap()
            .iter()
            .any(|l| l.0 == "my_band" && !l.3));
        assert!(tools::run(&mut ed, "roof_edge.deleted", |m| m.delete_sweep_profile("my_band")));
        assert!(!ed
            .model
            .sweep_profiles()
            .unwrap()
            .iter()
            .any(|l| l.0 == "my_band"));
    }

    #[test]
    fn dialog_strings_in_both_languages() {
        let mut keys: Vec<String> = [
            F_W,
            F_L,
            F_CURB,
            F_OFF,
            F_RISE,
            F_SLOPE,
            F_TMIN,
            F_DROP,
            F_TH,
            F_Z,
            F_BELOW,
            F_DX,
            F_DY,
            "roof_edge.f.profile",
            "roof_edge.f.type",
            "roof_edge.f.side_left",
            "roof_edge.f.rakes",
            "roof_edge.dlg.params",
            "roof_edge.dlg.props",
            "roof_edge.ok",
            "roof_edge.cancel",
            "roof_edge.edit",
            "roof_edge.delete",
            "roof_edge.gone",
            "roof_edge.add_drains",
            "roof_edge.bad_value",
            "roof_edge.list.empty",
            "roof_edge.pe.load",
            "roof_edge.pe.name",
            "roof_edge.pe.kind",
            "roof_edge.pe.n",
            "roof_edge.pe.x",
            "roof_edge.pe.y",
            "roof_edge.pe.bulge",
            "roof_edge.pe.ins",
            "roof_edge.pe.ins_tip",
            "roof_edge.pe.del",
            "roof_edge.pe.up",
            "roof_edge.pe.save",
            "roof_edge.pe.saved",
            "roof_edge.pe.valid",
            "roof_edge.pe.invalid",
            "roof_edge.pe.bad_number",
            "roof_edge.pe.no_name",
        ]
        .map(String::from)
        .to_vec();
        keys.extend(["sweep", "skylight", "soffit", "taper"].map(|k| format!("roof_edge.k.{k}")));
        keys.extend(SweepKind::ALL.map(|k| format!("roof_edge.sk.{}", k.name())));
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in &keys {
                assert_ne!(c.t_in(lang, k), *k, "{lang:?} {k}");
            }
        }
    }

    #[test]
    fn redesign_strings_in_both_languages() {
        let keys = [
            "roof_edge.err.number",
            "roof_edge.err.positive",
            "roof_edge.err.nonneg",
            "roof_edge.err.max",
            "roof_edge.list.empty_hint",
            "roof_edge.edit_tip",
            "roof_edge.delete_tip",
            "roof_edge.pe.del_tip",
            "roof_edge.pe.up_tip",
            "roof_edge.confirm_edge",
            "roof_edge.confirm_profile",
            "roof_edge.deleted_one",
            "roof_edge.profile_deleted",
        ];
        let plural = ["roof_edge.profile_in_use", "roof_edge.deleted_n"];
        let mut seen = vec![];
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in keys {
                assert_ne!(c.t_in(lang, k), k, "{lang:?} {k}");
            }
            for k in plural {
                for n in [1u64, 2, 5, 21] {
                    let s = c.plural(k, n);
                    assert_ne!(s, k, "{lang:?} {k} {n}");
                    assert!(s.contains(&n.to_string()), "{lang:?} {k} {n}: {s}");
                }
            }
            seen.push(c.t_in(lang, "roof_edge.err.number"));
        }
        assert_ne!(seen[0], seen[1], "ru differs from en");
    }

    #[test]
    fn field_reasons_name_the_problem_in_both_languages() {
        let len = Num::new(F_W, NumKind::Len, 0.5, M);
        let at = |n: &Num, s: &str| Num {
            text: s.into(),
            ..n.clone()
        };
        assert_eq!(len.check(M), Ok(0.5));
        assert_eq!(at(&len, "abc").check(M), Err(Reject::Number));
        assert_eq!(at(&len, "").check(M), Err(Reject::Number));
        assert_eq!(at(&len, "0").check(M), Err(Reject::Positive));
        assert_eq!(at(&len, "-5").check(M), Err(Reject::Positive));
        assert!(near(at(&len, "1 200").parse(M), 1.2));
        let nn = Num::new(F_DROP, NumKind::NonNeg, 0.0, M);
        assert_eq!(at(&nn, "0").check(M), Ok(0.0));
        assert_eq!(at(&nn, "-1").check(M), Err(Reject::NonNeg));
        assert_eq!(at(&nn, "x").check(M), Err(Reject::Number));
        let sg = Num::new(F_Z, NumKind::Signed, 0.0, M);
        assert!(near(at(&sg, "-150").parse(M), -0.15));
        assert_eq!(at(&sg, "--").check(M), Err(Reject::Number));
        let pct = Num::new(F_RISE, NumKind::Pct, 0.25, M).at_most(0.4);
        assert!(near(at(&pct, "2,5 %").parse(M), 0.025), "decimal comma");
        assert_eq!(at(&pct, "45").check(M), Err(Reject::Max));
        assert_eq!(at(&pct, "0").check(M), Err(Reject::Positive));
        let imp = Num::new(F_W, NumKind::Len, 0.3048, Units::Imperial);
        assert!(imp.parse(Units::Imperial).is_some_and(|v| (v - 0.3048).abs() < 1e-6));
        for r in [Reject::Number, Reject::Positive, Reject::NonNeg, Reject::Max] {
            let en = pct.reason_lang(false, r, M);
            let ru = pct.reason_lang(true, r, M);
            assert!(!en.is_empty() && !ru.is_empty() && en != ru, "{r:?}");
            assert_ne!(en, r.key(), "{r:?} has an English text");
            assert!(!en.contains('{') && !ru.contains('{'), "{r:?} placeholders filled");
        }
        assert!(pct.reason_lang(false, Reject::Max, M).contains("40"));
        assert!(pct.reason_lang(true, Reject::Max, M).contains("40"));
    }

    #[test]
    fn point_row_actions_and_cells() {
        assert_eq!(row_actions(3, 0), (false, false));
        assert_eq!(row_actions(3, 2), (false, true));
        assert_eq!(row_actions(4, 0), (true, false));
        assert_eq!(row_actions(8, 5), (true, true));
        assert!(cell_ok(0, "-150", M) && cell_ok(1, "0", M) && cell_ok(2, "0,5", M));
        assert!(!cell_ok(0, "x", M) && !cell_ok(2, "", M));
    }

    #[test]
    fn profile_delete_state_table() {
        assert_eq!(profile_del_state(false, 0), ProfileDel::NotSaved);
        assert_eq!(profile_del_state(false, 3), ProfileDel::NotSaved);
        assert_eq!(profile_del_state(true, 2), ProfileDel::InUse(2));
        assert_eq!(profile_del_state(true, 0), ProfileDel::Ok);
    }

    #[test]
    fn list_delete_needs_confirm_and_is_one_undo_step() {
        let (mut ed, _, roof) = with_roof();
        assert!(tools::exec(&mut ed, Cmd::Soffit, &Params::default()));
        let so = ed.model.soffits(roof).unwrap()[0];
        let (e, other) = (Elem::Soffit(so), Elem::Soffit(so + 1000));
        let mut pending = None;
        assert!(!list_step(&mut ed, &mut pending, ListAct::Confirm(e)), "no ask first");
        assert_eq!(ed.model.soffits(roof).unwrap(), vec![so]);
        assert!(!list_step(&mut ed, &mut pending, ListAct::Ask(e)));
        assert_eq!(pending, Some(e));
        assert_eq!(ed.model.soffits(roof).unwrap(), vec![so], "asking deletes nothing");
        assert!(!list_step(&mut ed, &mut pending, ListAct::Cancel));
        assert_eq!(pending, None);
        assert!(!list_step(&mut ed, &mut pending, ListAct::Confirm(e)));
        list_step(&mut ed, &mut pending, ListAct::Ask(e));
        assert!(!list_step(&mut ed, &mut pending, ListAct::Confirm(other)), "another element");
        assert_eq!(ed.model.soffits(roof).unwrap(), vec![so]);
        list_step(&mut ed, &mut pending, ListAct::Ask(e));
        ed.undo_offer = None;
        assert!(list_step(&mut ed, &mut pending, ListAct::Confirm(e)));
        assert!(ed.model.soffits(roof).unwrap().is_empty());
        assert_eq!(pending, None);
        assert!(ed.undo_offer.is_some(), "the toast carries Undo");
        assert_eq!(ed.undo_offer, ed.top_entry(), "for the delete just written");
        ed.undo();
        assert_eq!(ed.model.soffits(roof).unwrap(), vec![so], "one undo step brings it back");
    }

    #[test]
    fn empty_list_has_a_state() {
        let (ed, ..) = with_roof();
        assert!(level_elems(&ed.model, ed.level).is_empty());
    }

    #[test]
    fn profile_delete_is_refused_with_count_while_in_use() {
        let (mut ed, ..) = with_roof();
        let (kind, prof) = ed
            .model
            .sweep_profiles()
            .unwrap()
            .into_iter()
            .find(|l| l.0 == "fascia_200")
            .map(|l| (l.1, l.2))
            .unwrap();
        assert!(tools::run(&mut ed, "roof_edge.pe.saved", move |m| {
            m.set_sweep_profile("my_fascia", kind, &prof)
        }));
        assert_eq!(profile_state(&ed.model, "fascia_200"), ProfileDel::NotSaved);
        assert_eq!(profile_state(&ed.model, "my_fascia"), ProfileDel::Ok);
        let mut p = Params::default();
        p.sweeps[Cmd::Fascia.sweep_slot().unwrap()].profile = "my_fascia".into();
        assert!(tools::exec(&mut ed, Cmd::Fascia, &p));
        assert_eq!(profile_usage(&ed.model, "my_fascia"), 1);
        assert_eq!(profile_state(&ed.model, "my_fascia"), ProfileDel::InUse(1));
        let has = |ed: &Editor| {
            ed.model
                .sweep_profiles()
                .unwrap()
                .iter()
                .any(|l| l.0 == "my_fascia")
        };
        let mut armed = false;
        assert!(!profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Ask));
        assert!(!armed, "refused: nothing to confirm");
        assert!(!profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Confirm));
        assert!(has(&ed));
        // the dialog text says how many
        let c = Catalog::new(Lang::En);
        assert!(c.plural("roof_edge.profile_in_use", 1).contains('1'));
        // free it, then the two-step delete works and is one undo step
        let sw = ed.model.sweeps(Some(ed.level)).unwrap()[0];
        assert!(tools::delete_elems(&mut ed, &[Elem::Sweep(sw)]));
        assert_eq!(profile_state(&ed.model, "my_fascia"), ProfileDel::Ok);
        assert!(!profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Confirm), "no ask first");
        assert!(has(&ed));
        profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Ask);
        assert!(armed);
        profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Cancel);
        assert!(!armed && has(&ed));
        profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Ask);
        ed.undo_offer = None;
        assert!(profile_step(&mut ed, "my_fascia", &mut armed, DelAct::Confirm));
        assert!(!has(&ed) && !armed);
        assert!(ed.undo_offer.is_some() && ed.undo_offer == ed.top_entry());
        ed.undo();
        assert!(has(&ed), "one undo step brings the profile back");
    }

    #[test]
    fn file_copy_of_a_builtin_profile_stays_deletable_while_in_use() {
        let (mut ed, ..) = with_roof();
        let (kind, prof) = ed
            .model
            .sweep_profiles()
            .unwrap()
            .into_iter()
            .find(|l| l.0 == "fascia_200")
            .map(|l| (l.1, l.2))
            .unwrap();
        assert!(tools::run(&mut ed, "roof_edge.pe.saved", move |m| {
            m.set_sweep_profile("fascia_200", kind, &prof)
        }));
        let mut p = Params::default();
        p.sweeps[Cmd::Fascia.sweep_slot().unwrap()].profile = "fascia_200".into();
        assert!(tools::exec(&mut ed, Cmd::Fascia, &p));
        assert_eq!(profile_usage(&ed.model, "fascia_200"), 1);
        assert_eq!(profile_state(&ed.model, "fascia_200"), ProfileDel::Ok);
        let mut armed = false;
        profile_step(&mut ed, "fascia_200", &mut armed, DelAct::Ask);
        assert!(armed);
        assert!(profile_step(&mut ed, "fascia_200", &mut armed, DelAct::Confirm));
        assert_eq!(profile_state(&ed.model, "fascia_200"), ProfileDel::NotSaved);
        assert_eq!(profile_usage(&ed.model, "fascia_200"), 1, "the sweep keeps its profile name");
    }

    #[test]
    fn deletes_use_the_quiet_danger_icon() {
        let src = include_str!("roof_edge_ui.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(body.contains("icon_button_danger"));
        assert!(!body.contains("icon_btn(ui, &th, \"delete\""), "delete must be danger-tinted");
    }

    #[test]
    fn dialogs_use_kit_widgets_only() {
        let src = include_str!("roof_edge_ui.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for raw in [
            "egui::ComboBox",
            "ui.checkbox",
            "colored_label",
            ".spacing([",
            "egui::Grid",
            "egui::TextEdit",
            "small_button",
            "ui.button(",
            "egui::Button",
            ".size([",
        ] {
            assert!(!body.contains(raw), "raw widget or literal size: {raw}");
        }
        for (i, _) in body.match_indices("add_space(") {
            let next = body[i + "add_space(".len()..].chars().next().unwrap_or(' ');
            assert!(!next.is_ascii_digit(), "literal spacing at byte {i}");
        }
    }
}
