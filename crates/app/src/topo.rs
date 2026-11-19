//! Terrain and site: terrain points by click with an elevation, survey import (CSV/XYZ/DXF/IFC),
//! boundary, sub-regions with a material, building pad with slopes, smoothing and flattening of
//! a region, point editing in plan (drag, typed Z, delete) and in 3D (drag along Z).
//! Labelled contour lines and the north arrow are `paint`, 3D solids `view3d`, windows `window`.
//! Every action is one model operation through `Editor::run_op` (one undo step).

mod paint;
#[cfg(test)]
mod tests;
mod units_ui;
#[cfg(test)]
mod units_ui_tests;
mod view3d;
pub(crate) mod window;
#[cfg(test)]
mod window_tests;

pub use paint::{paint, refresh, Cache, Item};
pub use view3d::{append_scene, overlay3d, tag_of, TOPO_ELEMENT};
pub use window::{demo_import, window, ImportDlg, LenUnit, PropsBuf, SiteBuf};

use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::V2;
use ddc_io::topo::TopoSource;
use ddc_model::Model;
use ddc_ui::i18n::{Catalog, Lang};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Click adds a terrain point at the option elevation; Enter creates (or appends to the active terrain).
    Points,
    Boundary,
    Region,
    Pad,
    Smooth,
    Flatten,
    /// Editing points of the active terrain.
    Edit,
}

impl Tool {
    /// Hint key `hint.topo.<key>.<step>`.
    pub fn key(self) -> &'static str {
        match self {
            Tool::Points => "points",
            Tool::Boundary => "boundary",
            Tool::Region => "region",
            Tool::Pad => "pad",
            Tool::Smooth => "smooth",
            Tool::Flatten => "flatten",
            Tool::Edit => "edit",
        }
    }

    /// Ribbon command (active tool highlight).
    pub fn command(self) -> &'static str {
        match self {
            Tool::Points => "site.toposolid",
            Tool::Boundary => "site.boundary",
            Tool::Region => "site.subregion",
            Tool::Pad => "site.pad",
            Tool::Smooth => "site.smooth",
            Tool::Flatten => "site.flatten",
            Tool::Edit => "site.edit_points",
        }
    }

    /// Tool name in the status-bar hint key (`hint.<this>.<step>`).
    pub fn hint_tool(self) -> &'static str {
        match self {
            Tool::Points => "topo.points",
            Tool::Boundary => "topo.boundary",
            Tool::Region => "topo.region",
            Tool::Pad => "topo.pad",
            Tool::Smooth => "topo.smooth",
            Tool::Flatten => "topo.flatten",
            Tool::Edit => "topo.edit",
        }
    }

    /// The tool collects a closed outline.
    pub fn is_loop(self) -> bool {
        !matches!(self, Tool::Points | Tool::Edit)
    }
}

/// Subregion materials (keys `topo.mat.<m>`).
pub const MATERIALS: [&str; 5] = ["grass", "asphalt", "gravel", "paving", "water"];
/// Default material of the terrain body and the building pad.
pub const EARTH: &str = "earth";
pub const PAD_MATERIAL: &str = "concrete";

/// Terrain tool and view options.
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    /// Elevation of the next point, m.
    pub elevation: f64,
    /// Points are appended to the active terrain (otherwise a new terrain is created).
    pub add_to_active: bool,
    /// Index into `MATERIALS`.
    pub material: usize,
    pub pad_elevation: f64,
    /// Pad slope ratio: horizontal run per 1 of vertical (0 — vertical sides).
    pub slope: f64,
    pub pad_thickness: f64,
    pub passes: u32,
    pub flat_z: f64,
    /// Contour interval, m; every `major`-th contour is thickened and labelled.
    pub interval: f64,
    pub major: u32,
    /// Thickness of a new terrain, m.
    pub thickness: f64,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            elevation: 0.0,
            add_to_active: true,
            material: 0,
            pad_elevation: 0.0,
            slope: 1.5,
            pad_thickness: 0.3,
            passes: 3,
            flat_z: 0.0,
            interval: 0.5,
            major: 5,
            thickness: 1.0,
        }
    }
}

/// Point drag in plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PtDrag {
    pub topo: i64,
    pub idx: i64,
    pub from: (f64, f64, f64),
    pub start: V2,
    pub to: V2,
}

/// Point drag along Z in 3D: (terrain, index, original point, current elevation).
pub type Drag3d = (i64, i64, (f64, f64, f64), f64);

#[derive(Default)]
pub struct State {
    pub tool: Option<Tool>,
    /// Active terrain (None — the first in the model).
    pub active: Option<i64>,
    pub opts: Opts,
    /// Typed value (elevation, point Z, pass count).
    pub input: String,
    /// Collected points (plan, elevation; elevation is unused for outlines).
    pub pts: Vec<(V2, f64)>,
    pub cur: Option<V2>,
    pub hide_contours: bool,
    /// Selected points (indices) in edit mode.
    pub picked: Vec<i64>,
    pub hover: Option<i64>,
    pub drag: Option<PtDrag>,
    pub drag3d: Option<Drag3d>,
    /// "Building pad" window.
    pub window_open: bool,
    pub cache: Option<Cache>,
    pub lang: Option<Lang>,
    pub site_buf: Option<SiteBuf>,
    pub props_buf: Option<PropsBuf>,
    /// Survey import window (column preview, layer selection).
    pub import: Option<ImportDlg>,
    cat: Option<Catalog>,
}

impl State {
    /// UI string by key (language comes from the command that activated the tool).
    pub fn t(&mut self, key: &str) -> String {
        let lang = self.lang.unwrap_or(Lang::Ru);
        if self.cat.as_ref().map(|c| c.lang) != Some(lang) {
            self.cat = Some(Catalog::new(lang));
        }
        self.cat
            .as_ref()
            .map_or_else(|| key.to_string(), |c| c.t(key))
    }

    /// Step for the hint.
    pub fn step(&self) -> &'static str {
        match (self.tool, self.pts.is_empty()) {
            (Some(Tool::Edit), _) => "pick",
            (_, true) => "first",
            _ => "next",
        }
    }

    pub fn hint(&mut self) -> Option<String> {
        let tool = self.tool?;
        let k = format!("hint.topo.{}.{}", tool.key(), self.step());
        Some(self.t(&k))
    }
}

/// Model operation for `Editor::run_op`.
pub(crate) fn op(
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> OpFn {
    OpFn(Arc::new(f))
}

/// Active terrain: the selected one, otherwise the first in the model.
pub fn active(ed: &Editor) -> Option<i64> {
    let list = ed.model.toposolids().ok()?;
    match ed.topo.active {
        Some(id) if list.iter().any(|t| t.0 == id) => Some(id),
        _ => list.first().map(|t| t.0),
    }
}

fn newest(ed: &Editor) -> Option<i64> {
    let list = ed.model.toposolids().ok()?;
    list.iter().map(|t| t.0).max()
}

/// Terrain points: from the plan cache if fresh, otherwise from the model.
pub fn points_of(ed: &Editor, id: i64) -> Vec<(i64, (f64, f64, f64))> {
    let rev = ed.cache_key();
    if let Some(c) = ed.topo.cache.as_ref().filter(|c| c.rev == rev) {
        if let Some(it) = c.items.iter().find(|i| i.id == id) {
            return it.points.clone();
        }
    }
    ed.model.topo_points(id).unwrap_or_default()
}

fn say(ed: &mut Editor, key: &str) {
    let m = ed.topo.t(key);
    ed.status = Some(m);
}

fn say_err(ed: &mut Editor, e: &ddc_model::Error) {
    let m = format!("{}: {e}", ed.topo.t("topo.err.op"));
    ed.status = Some(m);
}

/// Reset the tool without changing mode (from `Editor::set_mode`: another tool releases the terrain tool).
pub fn reset(s: &mut State) {
    s.tool = None;
    s.pts.clear();
    s.input.clear();
    s.picked.clear();
    s.drag = None;
    s.drag3d = None;
    s.hover = None;
}

/// Ctrl+Z while collecting an outline or points removes the last point; false — nothing is being collected.
pub fn undo_point(ed: &mut Editor) -> bool {
    let s = &mut ed.topo;
    if s.tool.is_none() || s.pts.is_empty() {
        return false;
    }
    s.pts.pop();
    true
}

/// Command of the active terrain tool (ribbon highlight).
pub fn command(ed: &Editor) -> Option<&'static str> {
    ed.topo.tool.map(Tool::command)
}

/// Step hint for the shell status bar.
pub fn hint_pair(ed: &Editor) -> Option<(&'static str, &'static str)> {
    ed.topo.tool.map(|t| (t.hint_tool(), ed.topo.step()))
}

/// Activate a tool (from a ribbon command or a test). Outline tools need wall
/// mode so that a click does not select elements; ribbon highlight and hint follow `command`.
pub fn arm(ed: &mut Editor, tool: Tool, lang: Lang) {
    crate::interact::sessions::model_tool::stop(ed);
    // a contour tool draws through its session; the wall stays unarmed
    ed.set_mode(Mode::Select);
    if tool != Tool::Edit {
        ed.selection.clear();
    }
    let s = &mut ed.topo;
    s.lang = Some(lang);
    s.tool = Some(tool);
    ed.status = None;
    if tool != Tool::Points && active(ed).is_none() {
        say(ed, "topo.err.none");
        disarm(ed);
    }
    crate::interact::Interaction::refresh(ed);
}

/// Point editing of one terrain (from its properties panel): the plan picks, drags and types
/// elevations for this terrain; the selection stays, so the panel stays.
pub fn edit_points(ed: &mut Editor, id: i64) {
    let lang = if crate::ui_lang_now() {
        Lang::Ru
    } else {
        Lang::En
    };
    arm(ed, Tool::Edit, lang);
    ed.topo.active = Some(id);
}

pub fn disarm(ed: &mut Editor) {
    reset(&mut ed.topo);
    ed.set_mode(Mode::Select);
}

fn parse_num(s: &str) -> Option<f64> {
    crate::units::parse_locale_number(s.trim(), crate::units::ui_decimal_mark()).ok()
}

/// Plan event for the active terrain tool; false — no tool is active.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    let Some(tool) = ed.topo.tool else {
        return false;
    };
    match e {
        Event::Move(p) => {
            ed.topo.cur = Some(p);
            if let Some(d) = &mut ed.topo.drag {
                d.to = p;
            } else if tool == Tool::Edit {
                ed.topo.hover = hit_point(ed, p);
            }
        }
        Event::Click(p) => click(ed, tool, p),
        Event::Press(p) if tool == Tool::Edit => press(ed, p),
        Event::Release(p) if tool == Tool::Edit => release(ed, p),
        Event::Press(_) | Event::Release(_) => {}
        Event::Key(Key::Escape) => {
            let s = &mut ed.topo;
            if !s.input.is_empty() {
                s.input.clear();
            } else if s.drag.take().is_some() {
            } else if !s.pts.is_empty() {
                s.pts.clear();
            } else if !s.picked.is_empty() {
                s.picked.clear();
            } else {
                disarm(ed);
            }
        }
        Event::Key(Key::Enter) => enter(ed, tool),
        Event::Key(Key::Backspace) => {
            if ed.topo.input.pop().is_none() {
                if tool == Tool::Edit {
                    delete_picked(ed);
                } else {
                    ed.topo.pts.pop();
                }
            }
        }
        Event::Key(Key::Close) if tool.is_loop() => commit_loop(ed, tool),
        Event::Key(_) => {}
        Event::Char(c) => {
            if c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+') {
                ed.topo.input.push(c);
            }
        }
    }
    true
}

/// Delete the selected points (Delete key in edit mode); false — the selection is not ours.
pub fn delete(ed: &mut Editor) -> bool {
    if ed.topo.tool != Some(Tool::Edit) || ed.topo.picked.is_empty() {
        return false;
    }
    delete_picked(ed);
    true
}

/// Point under the cursor: via the plan cache without copying (runs on every mouse move).
fn hit_point(ed: &Editor, p: V2) -> Option<i64> {
    let id = active(ed)?;
    let r2 = ed.tool.radius.max(1e-6).powi(2);
    let near = |pts: &[(i64, (f64, f64, f64))]| {
        pts.iter()
            .map(|&(i, q)| (i, (q.0 - p.x).powi(2) + (q.1 - p.y).powi(2)))
            .filter(|x| x.1 <= r2)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|x| x.0)
    };
    let rev = ed.cache_key();
    let cached = ed
        .topo
        .cache
        .as_ref()
        .filter(|c| c.rev == rev)
        .and_then(|c| c.items.iter().find(|i| i.id == id));
    match cached {
        Some(it) => near(&it.points),
        None => near(&ed.model.topo_points(id).unwrap_or_default()),
    }
}

fn click(ed: &mut Editor, tool: Tool, p: V2) {
    match tool {
        Tool::Points => {
            let z = ed.topo.opts.elevation;
            ed.topo.pts.push((p, z));
        }
        Tool::Edit => match hit_point(ed, p) {
            Some(i) => {
                let s = &mut ed.topo;
                match s.picked.iter().position(|&k| k == i) {
                    Some(k) => {
                        s.picked.remove(k);
                    }
                    None => s.picked.push(i),
                }
            }
            None => ed.topo.picked.clear(),
        },
        _ => {
            let r = ed.tool.radius.max(1e-6);
            let s = &ed.topo;
            if s.pts.len() >= 3 && (s.pts[0].0 - p).len() <= r {
                commit_loop(ed, tool);
            } else {
                ed.topo.pts.push((p, 0.0));
            }
        }
    }
}

fn press(ed: &mut Editor, p: V2) {
    let (Some(id), Some(i)) = (active(ed), hit_point(ed, p)) else {
        return;
    };
    let Some(from) = points_of(ed, id)
        .into_iter()
        .find(|q| q.0 == i)
        .map(|q| q.1)
    else {
        return;
    };
    ed.topo.drag = Some(PtDrag {
        topo: id,
        idx: i,
        from,
        start: p,
        to: p,
    });
}

fn release(ed: &mut Editor, p: V2) {
    let Some(d) = ed.topo.drag.take() else {
        return;
    };
    let dv = p - d.start;
    if dv.len() <= 1e-9 {
        return;
    }
    let to = (d.from.0 + dv.x, d.from.1 + dv.y, d.from.2);
    let (t, i) = (d.topo, d.idx);
    match ed.run_op(op(move |m| m.move_topo_point(t, i, to))) {
        Ok(()) => say(ed, "topo.done.moved"),
        Err(e) => say_err(ed, &e),
    }
}

fn enter(ed: &mut Editor, tool: Tool) {
    let s = std::mem::take(&mut ed.topo.input);
    if !s.trim().is_empty() {
        match parse_num(&s) {
            Some(v) => apply_value(ed, tool, v),
            None => say(ed, "topo.err.number"),
        }
        return;
    }
    match tool {
        Tool::Points => commit_points(ed),
        Tool::Edit => {}
        _ => commit_loop(ed, tool),
    }
}

fn apply_value(ed: &mut Editor, tool: Tool, v: f64) {
    let o = &mut ed.topo.opts;
    match tool {
        Tool::Points => o.elevation = v,
        Tool::Pad => o.pad_elevation = v,
        Tool::Flatten => o.flat_z = v,
        Tool::Smooth => o.passes = v.round().clamp(1.0, 50.0) as u32,
        Tool::Edit => set_picked_z(ed, v),
        Tool::Boundary | Tool::Region => {}
    }
}

/// Z of the selected points (or the point under the cursor): one point — one operation, several — one write.
pub(crate) fn set_picked_z(ed: &mut Editor, z: f64) {
    let Some(id) = active(ed) else {
        return;
    };
    let mut idx = ed.topo.picked.clone();
    if idx.is_empty() {
        idx.extend(ed.topo.hover);
    }
    if idx.is_empty() {
        say(ed, "topo.err.pick");
        return;
    }
    let pts = points_of(ed, id);
    let moves: Vec<(i64, (f64, f64, f64))> = pts
        .into_iter()
        .filter(|q| idx.contains(&q.0))
        .map(|(i, q)| (i, (q.0, q.1, z)))
        .collect();
    if moves.is_empty() {
        say(ed, "topo.err.pick");
        return;
    }
    let r = ed.run_op(op(move |m| m.move_topo_points(id, &moves)));
    match r {
        Ok(()) => say(ed, "topo.done.z"),
        Err(e) => say_err(ed, &e),
    }
}

fn delete_picked(ed: &mut Editor) {
    let Some(id) = active(ed) else {
        return;
    };
    let idx = std::mem::take(&mut ed.topo.picked);
    if idx.is_empty() {
        return;
    }
    match ed.run_op(op(move |m| m.delete_topo_points(id, &idx))) {
        Ok(()) => say(ed, "topo.done.deleted"),
        Err(e) => say_err(ed, &e),
    }
}

fn commit_points(ed: &mut Editor) {
    let pts: Vec<(f64, f64, f64)> = ed.topo.pts.iter().map(|(p, z)| (p.x, p.y, *z)).collect();
    if pts.is_empty() {
        return;
    }
    let target = if ed.topo.opts.add_to_active {
        active(ed)
    } else {
        None
    };
    let r = match target {
        Some(id) => ed.run_op(op(move |m| m.add_topo_points(id, &pts))),
        None => {
            if pts.len() < 3 {
                say(ed, "topo.err.few");
                return;
            }
            let n = ed.model.toposolids().map(|l| l.len()).unwrap_or(0) + 1;
            let name = format!("{} {n}", ed.topo.t("topo.default_name"));
            let th = ed.topo.opts.thickness;
            ed.run_op(op(move |m| {
                m.add_toposolid(&ddc_model::TopoSpec {
                    name: name.clone(),
                    thickness: th,
                    material: EARTH.to_string(),
                    points: pts.clone(),
                    breaklines: vec![],
                    boundary: vec![],
                })
                .map(|_| ())
            }))
        }
    };
    match r {
        Ok(()) => {
            ed.topo.pts.clear();
            if target.is_none() {
                ed.topo.active = newest(ed);
                say(ed, "topo.done.created");
            } else {
                say(ed, "topo.done.points");
            }
        }
        Err(e) => say_err(ed, &e),
    }
}

fn commit_loop(ed: &mut Editor, tool: Tool) {
    if ed.topo.pts.len() < 3 {
        say(ed, "topo.err.loop");
        return;
    }
    let Some(id) = active(ed) else {
        say(ed, "topo.err.none");
        return;
    };
    let ring: Vec<(f64, f64)> = ed.topo.pts.iter().map(|(p, _)| (p.x, p.y)).collect();
    let o = ed.topo.opts.clone();
    let r = match tool {
        Tool::Boundary => ed.run_op(op(move |m| m.set_topo_boundary(id, &ring))),
        Tool::Region => {
            let mat = MATERIALS[o.material.min(MATERIALS.len() - 1)].to_string();
            let name = ed.topo.t(&format!("topo.mat.{mat}"));
            ed.run_op(op(move |m| {
                m.add_topo_region(id, &name, &mat, &ring).map(|_| ())
            }))
        }
        Tool::Pad => {
            let name = ed.topo.t("topo.pad_name");
            ed.run_op(op(move |m| {
                m.add_pad(&ddc_model::PadSpec {
                    topo: id,
                    name: name.clone(),
                    elevation: o.pad_elevation,
                    slope: o.slope,
                    thickness: o.pad_thickness,
                    material: PAD_MATERIAL.to_string(),
                    outline: ring.clone(),
                })
                .map(|_| ())
            }))
        }
        Tool::Smooth | Tool::Flatten => {
            let (n, z, smooth) = (o.passes, o.flat_z, tool == Tool::Smooth);
            // nothing changed — no model operation and no undo step
            let changed = Arc::new(AtomicUsize::new(0));
            let seen = changed.clone();
            let label = if smooth {
                "topo_smooth"
            } else {
                "topo_flatten"
            };
            let rec = crate::tools::Rec::new(label, move |m| {
                let k = if smooth {
                    m.smooth_topo(id, &ring, n)?
                } else {
                    m.flatten_topo(id, &ring, z)?
                };
                seen.store(k, Ordering::Relaxed);
                Ok(usize::from(k > 0))
            });
            let r = ed.apply_ext(rec);
            if r.is_ok() && changed.load(Ordering::Relaxed) == 0 {
                ed.topo.pts.clear();
                say(ed, "topo.done.unchanged");
                return;
            }
            r
        }
        Tool::Points | Tool::Edit => return,
    };
    match r {
        Ok(()) => {
            ed.topo.pts.clear();
            if tool == Tool::Pad {
                pad_report(ed, id);
            } else {
                say(ed, &format!("topo.done.{}", tool.key()));
            }
        }
        Err(e) => say_err(ed, &e),
    }
}

/// Status after a pad: cut/fill of the terrain's last pad.
fn pad_report(ed: &mut Editor, id: i64) {
    let rep = ed
        .model
        .topo_built_shared(id)
        .ok()
        .and_then(|b| b.pads.iter().max_by_key(|r| r.pad).cloned());
    let Some(r) = rep else {
        say(ed, "topo.done.pad");
        return;
    };
    let m = ed
        .topo
        .t("topo.done.pad_report")
        .replace("{cut}", &crate::unit_text::vol_ui(r.cut))
        .replace("{fill}", &crate::unit_text::vol_ui(r.fill));
    ed.status = Some(m);
}

/// A survey farther than this from the origin (m) is treated as being in a geodetic system.
pub const FAR_SURVEY: f64 = 10_000.0;

/// Shift of a survey to the project origin: project = rotation(north) · (survey − origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shift {
    pub origin: (f64, f64, f64),
    /// True-north angle, rad (counter-clockwise from the project's +Y).
    pub angle: f64,
    /// The origin was taken from the survey and is recorded as the site base point.
    pub new_base: bool,
}

/// Shift for a geodetic survey (coordinates > `FAR_SURVEY`): the site base point if
/// set, otherwise the extent minimum rounded to 100 m; a local survey is not shifted.
pub fn survey_shift(src: &TopoSource, site: &ddc_model::SiteProps) -> Option<Shift> {
    let xy = src
        .points
        .iter()
        .map(|p| (p.0, p.1))
        .chain(src.breaklines.iter().flatten().map(|p| (p.0, p.1)))
        .chain(src.boundary.iter().copied());
    let (mut lo, mut far, mut any) = ((f64::INFINITY, f64::INFINITY), false, false);
    for (x, y) in xy {
        any = true;
        lo = (lo.0.min(x), lo.1.min(y));
        far |= x.abs().max(y.abs()) > FAR_SURVEY;
    }
    if !any || !far {
        return None;
    }
    let angle = site.north_deg.to_radians();
    if site.base.0 != 0.0 || site.base.1 != 0.0 {
        return Some(Shift {
            origin: site.base,
            angle,
            new_base: false,
        });
    }
    let r = |v: f64| (v / 100.0).floor() * 100.0;
    Some(Shift {
        origin: (r(lo.0), r(lo.1), site.base.2),
        angle,
        new_base: true,
    })
}

/// Shift in f64 before writing to the model (scene and plan work in f32 relative to the project origin).
pub fn apply_shift(src: &mut TopoSource, s: &Shift) {
    let (sn, cs) = s.angle.sin_cos();
    let (ox, oy, oz) = s.origin;
    let xy = |x: f64, y: f64| {
        let (dx, dy) = (x - ox, y - oy);
        (dx * cs - dy * sn, dx * sn + dy * cs)
    };
    let p3 = |p: &mut (f64, f64, f64)| {
        let (x, y) = xy(p.0, p.1);
        *p = (x, y, p.2 - oz);
    };
    src.points.iter_mut().for_each(p3);
    src.breaklines.iter_mut().flatten().for_each(p3);
    for b in &mut src.boundary {
        *b = xy(b.0, b.1);
    }
}

/// Survey import from a file with default choices (no window): one terrain, one undo step.
pub fn import_file(ed: &mut Editor, path: &std::path::Path) {
    match ddc_io::topo::read_file(path) {
        Ok(src) => {
            let name = stem(ed, path);
            import_source(ed, &name, src);
        }
        Err(e) => import_error(ed, &e),
    }
}

pub(crate) fn stem(ed: &mut Editor, path: &std::path::Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| ed.topo.t("topo.default_name"))
}

pub(crate) fn import_error(ed: &mut Editor, e: &str) {
    let m = format!("{}: {e}", ed.topo.t("topo.err.import"));
    ed.status = Some(m);
}

/// Parsed survey → terrain (and the site base point on auto-shift) in one undo step.
pub fn import_source(ed: &mut Editor, name: &str, mut src: TopoSource) {
    let site = ed.model.site().unwrap_or_default();
    let shift = survey_shift(&src, &site);
    if let Some(s) = &shift {
        apply_shift(&mut src, s);
    }
    let th = ed.topo.opts.thickness;
    let (np, nb) = (src.points.len(), src.breaklines.len());
    let skipped = src.skipped + src.filtered;
    let (ambiguous, grouped) = (src.ambiguous, src.grouped);
    let spec = ddc_model::TopoSpec {
        name: name.to_string(),
        thickness: th,
        material: EARTH.to_string(),
        points: src.points,
        breaklines: src.breaklines,
        boundary: src.boundary,
    };
    let new_site = shift.filter(|s| s.new_base).map(|s| ddc_model::SiteProps {
        base: s.origin,
        ..site.clone()
    });
    let rec = crate::tools::Rec::new("topo_import", move |m| {
        let mut n = 1;
        if let Some(st) = &new_site {
            m.set_site(st)?;
            n += 1;
        }
        m.add_toposolid(&spec)?;
        Ok(n)
    });
    match ed.apply_ext(rec) {
        Ok(()) => {
            ed.topo.active = newest(ed);
            let mut m = ed
                .topo
                .t("topo.done.imported")
                .replace("{n}", &np.to_string())
                .replace("{b}", &nb.to_string())
                .replace("{s}", &skipped.to_string());
            let notes = [
                ("topo_import.topo.ambiguous", ambiguous),
                ("topo_import.topo.grouped", grouped),
            ];
            for (key, n) in notes {
                if n > 0 {
                    let k = ed.topo.t(key).replace("{n}", &n.to_string());
                    m = format!("{m} · {k}");
                }
            }
            if let Some(s) = shift {
                let k = ed
                    .topo
                    .t("topo.done.shifted")
                    .replace("{e}", &crate::units::fmt_num(s.origin.0, 2))
                    .replace("{n}", &crate::units::fmt_num(s.origin.1, 2));
                m = format!("{m} · {k}");
            }
            ed.status = Some(m);
        }
        Err(e) => say_err(ed, &e),
    }
}

fn lang(app: &EditorApp) -> Lang {
    app.sh.cat.lang
}

/// Activate a tool from the ribbon; plan-editing tools are released so events reach the terrain tool.
fn arm_from(app: &mut EditorApp, tool: Tool) {
    crate::interact::sessions::modify::stop(&mut app.ed);
    app.plan.b.dim = None;
    let l = lang(app);
    arm(&mut app.ed, tool, l);
}

fn h_points(app: &mut EditorApp) {
    arm_from(app, Tool::Points);
}
fn h_boundary(app: &mut EditorApp) {
    arm_from(app, Tool::Boundary);
}
fn h_region(app: &mut EditorApp) {
    arm_from(app, Tool::Region);
}
fn h_pad(app: &mut EditorApp) {
    arm_from(app, Tool::Pad);
}
fn h_smooth(app: &mut EditorApp) {
    arm_from(app, Tool::Smooth);
}
fn h_flatten(app: &mut EditorApp) {
    arm_from(app, Tool::Flatten);
}
fn h_edit(app: &mut EditorApp) {
    arm_from(app, Tool::Edit);
}
fn h_contours(app: &mut EditorApp) {
    toggle_contours(&mut app.ed);
}

/// Contours on/off, said in the status line (with no toposolid nothing changes on screen).
pub fn toggle_contours(ed: &mut Editor) {
    let none = ed
        .model
        .sql()
        .query_row("SELECT COUNT(*) FROM toposolid", [], |r| r.get::<_, i64>(0))
        .map_or(true, |n| n == 0);
    let s = &mut ed.topo;
    s.hide_contours = !s.hide_contours;
    let key = match (s.hide_contours, none) {
        (_, true) => "sketch_hints.contours_none",
        (true, false) => "sketch_hints.contours_off",
        (false, false) => "sketch_hints.contours_on",
    };
    ed.status = Some(crate::tr(key, &[]));
}
fn h_properties(app: &mut EditorApp) {
    let l = lang(app);
    let s = &mut app.ed.topo;
    s.lang = Some(l);
    s.window_open = true;
    s.site_buf = None;
    s.props_buf = None;
}
fn h_import(app: &mut EditorApp) {
    let l = lang(app);
    app.ed.topo.lang = Some(l);
    let title = app.sh.cat.t("topo.import.title");
    let filter = app.sh.cat.t("topo.import.filter");
    let Some(path) = crate::file_pick::Dlg::new()
        .set_title(title)
        .add_filter(filter, &ddc_io::topo::EXTENSIONS)
        .pick_file()
    else {
        return;
    };
    window::open_import(&mut app.ed, &path);
}

/// Registration of site commands in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    let list: &[(&str, crate::shell::Handler)] = &[
        ("site.toposolid", h_points),
        ("site.toposolid_import", h_import),
        ("site.boundary", h_boundary),
        ("site.subregion", h_region),
        ("site.pad", h_pad),
        ("site.smooth", h_smooth),
        ("site.flatten", h_flatten),
        ("site.edit_points", h_edit),
        ("site.contours", h_contours),
        ("site.properties", h_properties),
    ];
    for (id, h) in list {
        app.register_command(id, *h);
    }
}
