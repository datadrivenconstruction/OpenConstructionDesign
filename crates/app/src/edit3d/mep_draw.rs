// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing duct, pipe and cable tray runs in 3D. Points chain on a horizontal plane at the run
//! elevation (offset from the active level); a typed `+v`/`-v` or a Ctrl click above/below the last
//! point makes a riser. A connector, run end or run axis under the cursor snaps the point: a start
//! there takes that run's section, system and elevation, a later point there ends the run (tee or
//! connection). The run is written by the plan tool's model op (`mep_draw`): one undo step, elbows
//! and tees come from the network build.

use std::sync::Arc;

use ddc_model::{MepDomain, MepHit, MepSnapSet};
use ddc_render::glam::Vec3;
use ddc_render::Ray;
use ddc_ui::i18n::Lang;
use egui::{Stroke, Vec2};

use super::mirror_array::Fx;
use super::snap::SnapKind;
use super::{fmt_len, math, paint, Edit3d, Tool};
use crate::editor::{Editor, OpFn};
use crate::tools::{self, Active};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Duct,
    Pipe,
    Tray,
}

impl Kind {
    pub fn command(self) -> &'static str {
        match self {
            Kind::Duct => "e3d.duct",
            Kind::Pipe => "e3d.pipe",
            Kind::Tray => "e3d.tray",
        }
    }

    fn plan(self) -> tools::Kind {
        match self {
            Kind::Duct => tools::Kind::Duct,
            Kind::Pipe => tools::Kind::Pipe,
            Kind::Tray => tools::Kind::Tray,
        }
    }

    fn fits(self, d: MepDomain) -> bool {
        match self {
            Kind::Duct => d == MepDomain::Duct,
            Kind::Pipe => d == MepDomain::Pipe,
            Kind::Tray => matches!(d, MepDomain::CableTray | MepDomain::Conduit),
        }
    }
}

/// Chain being drawn.
#[derive(Default)]
pub struct State {
    kind: Option<Kind>,
    /// Axis points, model coordinates (absolute z).
    pub pts: Vec<(f64, f64, f64)>,
    /// Run elevation above the active level ("middle elevation"); kept between runs.
    pub z: Option<f64>,
    first: Option<MepHit>,
    snap: Option<(u64, Arc<MepSnapSet>)>,
    pub lang: Option<Lang>,
}

impl State {
    /// Tool switch: the chain is dropped; another kind takes its own default elevation.
    pub fn set_tool(&mut self, t: Tool) {
        let k = match t {
            Tool::Mep(k) => Some(k),
            _ => None,
        };
        if k.is_some() && k != self.kind {
            self.z = None;
        }
        self.kind = k.or(self.kind);
        self.clear();
    }

    pub fn clear(&mut self) {
        self.pts.clear();
        self.first = None;
    }

    fn active(&self, ed: &Editor) -> Option<Active> {
        Some(Active::new(self.kind?.plan(), ed, self.lang.unwrap_or(Lang::En)))
    }

    /// Absolute elevation of the next point.
    pub fn elev(&mut self, ed: &Editor) -> f64 {
        if let Some(&(.., z)) = self.pts.last() {
            return z;
        }
        if self.z.is_none() {
            self.z = self.active(ed).map(|a| a.opts.z);
        }
        ed.elevation + self.z.unwrap_or(0.0)
    }

    /// Network point under `p` (model coordinates): a connector, then a run end, then a run axis
    /// of the tool's kind (and of the start's system), within `r` in plan and `zr` in height.
    pub fn hit(&mut self, ed: &Editor, p: (f64, f64, f64), r: f64, zr: f64) -> Option<MepHit> {
        let k = self.kind?;
        let set = match &self.snap {
            Some((rev, s)) if *rev == ed.revision => s.clone(),
            _ => {
                let s = Arc::new(ed.model.mep_snap_set().ok()?);
                self.snap = Some((ed.revision, s.clone()));
                s
            }
        };
        let want = self.first.as_ref().and_then(MepHit::system);
        let keep = move |d: MepDomain, sys: Option<i64>| {
            k.fits(d) && (sys.is_none() || want.is_none() || sys == want)
        };
        set.pick((p.0, p.1), r, (p.2 - zr, p.2 + zr), &keep)
    }

    /// A click at `p` (its z is ignored for a free point). The first point on a network starts
    /// there at its elevation; a later one ends the run: `Some` is the run to write.
    pub fn click(&mut self, ed: &Editor, p: (f64, f64, f64), hit: Option<MepHit>) -> Option<Option<MepHit>> {
        let z = self.elev(ed);
        if self.pts.is_empty() {
            let at = hit.as_ref().map_or((p.0, p.1, z), MepHit::at);
            self.z = Some(at.2 - ed.elevation);
            self.first = hit;
            self.pts.push(at);
            return None;
        }
        let at = hit.as_ref().map_or(p, MepHit::at);
        self.push((at.0, at.1, z));
        hit.map(Some)
    }

    /// Riser at the last point by `dz`; before the first point it shifts the start elevation.
    pub fn rise(&mut self, ed: &Editor, dz: f64) {
        let z = self.elev(ed) + dz;
        self.z = Some(z - ed.elevation);
        if let Some(&(x, y, _)) = self.pts.last() {
            self.push((x, y, z));
        }
    }

    fn push(&mut self, p: (f64, f64, f64)) {
        let same = self.pts.last().is_some_and(|q| {
            (q.0 - p.0).abs() < 1e-6 && (q.1 - p.1).abs() < 1e-6 && (q.2 - p.2).abs() < 1e-6
        });
        if !same {
            self.pts.push(p);
        }
    }

    /// Writes the chain as one run (one undo step) and clears it; `last` is the network it ends on.
    pub fn commit(&mut self, ed: &mut Editor, last: Option<MepHit>) -> Result<i64, ddc_model::Error> {
        if self.pts.len() < 2 {
            return Err(ddc_model::Error::Invalid("e3d.err.mep_points".into()));
        }
        let a = self
            .active(ed)
            .ok_or_else(|| ddc_model::Error::Invalid("e3d.err.unsupported".into()))?;
        let (name, kind, spec) =
            tools::mep::spec_3d(ed, &a, self.first.as_ref(), last.as_ref(), &self.pts)?;
        let id = Arc::new(std::sync::Mutex::new(0));
        let out = id.clone();
        ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| {
            *out.lock().unwrap() = m.mep_draw(&name, kind, &spec)?;
            Ok(())
        })))?;
        ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
        self.clear();
        let id = *id.lock().unwrap();
        Ok(id)
    }
}

/// Typed entry: a signed `+v`/`-v` is a rise, a plain value a length (before the first point: the
/// elevation above the level).
enum Typed {
    Rise(f64),
    Value(f64),
}

fn typed(s: &str, ed: &Editor) -> Option<Typed> {
    let s = s.trim();
    let (sign, body) = match s.chars().next()? {
        '+' => (Some(1.0), &s[1..]),
        '-' => (Some(-1.0), &s[1..]),
        _ => (None, s),
    };
    let v = crate::units::parse_edit_length(body, ed.tool.units)?;
    Some(match sign {
        Some(k) => Typed::Rise(k * v),
        None => Typed::Value(v),
    })
}

fn scene(p: (f64, f64, f64), z0: f64) -> Vec3 {
    Vec3::new(p.0 as f32, p.1 as f32, (p.2 - z0) as f32)
}

impl Edit3d {
    /// One frame of the MEP run tool: cursor on the run plane, network snap, typed values, commit.
    pub(super) fn mep_frame(&mut self, fx: Fx<'_>) {
        let Fx {
            ui,
            painter,
            cam,
            ed,
            pointer,
            click,
            enter,
            shift,
            ov,
            ..
        } = fx;
        let ctrl = ui.input(|i| i.modifiers.command);
        let (fg, bg) = (ov.ink, ov.plate);
        let z0 = ed.elevation;
        let z = self.mep.elev(ed);
        let last = self.mep.pts.last().copied();
        let ray: Option<Ray> = pointer.map(|p| cam.ray(p));
        let typed = self.typing.as_deref().and_then(|s| typed(s, ed));

        // cursor: vertical from the last point with Ctrl, else on the run plane (a geometry snap
        // gives its plan position)
        let mut cur: Option<(f64, f64, f64)> = None;
        let mut probes = vec![];
        if let (Some(r), Some(l), true) = (&ray, last, ctrl) {
            let o = scene(l, z0);
            cur = math::ray_line(r, o, Vec3::Z).map(|s| (l.0, l.1, l.2 + s as f64));
        } else if let Some(r) = &ray {
            use SnapKind as K;
            // a face or edge under the cursor is not a plan position at the run's height
            let body = self.snap.filter(|s| s.kind != K::Plane).map(|s| s.p);
            let point = self
                .snap
                .filter(|s| !matches!(s.kind, K::Plane | K::Face | K::Surface | K::Edge | K::Curve | K::Iso))
                .map(|s| s.p);
            let on = math::ray_plane(r, Vec3::new(0.0, 0.0, (z - z0) as f32), Vec3::Z).map(|h| h.1);
            probes.extend(body);
            probes.extend(on);
            cur = point.or(on).map(|p| (p.x as f64, p.y as f64, z));
        }
        if let (Some(c), Some(l), true, false) = (&mut cur, last, shift, ctrl) {
            if (c.0 - l.0).abs() > (c.1 - l.1).abs() {
                c.1 = l.1;
            } else {
                c.0 = l.0;
            }
        }
        if let (Some(c), Some(l), Some(Typed::Value(v)), false) = (&mut cur, last, &typed, ctrl) {
            let d = Vec2::new((c.0 - l.0) as f32, (c.1 - l.1) as f32).normalized();
            let d = if d.x.is_finite() { d } else { Vec2::X };
            *c = (l.0 + d.x as f64 * v, l.1 + d.y as f64 * v, z);
        }
        // network under the cursor: by the body hit (true 3D point), else on the run plane
        let mut hit = None;
        if typed.is_none() {
            for p in probes {
                let r = (cam.m_per_px(p) * 14.0).max(0.05) as f64;
                let pm = (p.x as f64, p.y as f64, p.z as f64 + z0);
                hit = self.mep.hit(ed, pm, r, r.max(0.3));
                if hit.is_some() {
                    break;
                }
            }
        }
        if let Some(h) = &hit {
            cur = Some(h.at());
        }

        // input
        let mut write: Option<Option<MepHit>> = None;
        if enter {
            match typed {
                Some(Typed::Rise(dz)) => self.mep.rise(ed, dz),
                Some(Typed::Value(v)) if self.mep.pts.is_empty() => self.mep.z = Some(v),
                Some(Typed::Value(_)) => {
                    if let Some(c) = cur {
                        self.mep.click(ed, c, None);
                    }
                }
                None => write = Some(None),
            }
            self.typing = None;
        } else if let (true, Some(c)) = (click, cur) {
            match (ctrl, last) {
                (true, Some(l)) => self.mep.rise(ed, c.2 - l.2),
                _ => write = self.mep.click(ed, c, hit.clone()),
            }
            self.typing = None;
        }
        if let Some(end) = write {
            match self.mep.commit(ed, end) {
                Ok(_) => {
                    let what = super::tr(self.ru, &format!("cmd.{}", self.tool.command().unwrap_or("e3d")));
                    ed.status = Some(what);
                }
                Err(e) => {
                    ed.status = Some(self.err(&e));
                    // a refused chain stays for correction, an empty one is just nothing
                    if self.mep.pts.len() < 2 {
                        self.mep.clear();
                    }
                }
            }
        }

        // preview
        let mut line: Vec<Vec3> = self.mep.pts.iter().map(|&p| scene(p, z0)).collect();
        if let (Some(c), Some(l)) = (cur, self.mep.pts.last().copied()) {
            let (a, b) = (scene(l, z0), scene((c.0, c.1, if ctrl { c.2 } else { l.2 }), z0));
            line.push(b);
            let txt = self.typing.clone().map_or_else(|| fmt_len((b - a).length(), ed), |t| format!("{t}▏"));
            paint::dim(painter, cam, a, b, &txt, fg, bg);
            if !ctrl && (c.2 - l.2).abs() > 1e-6 {
                line.push(scene(c, z0));
            }
        }
        paint::polyline(painter, cam, &line, Stroke::new(3.0, ov.preview));
        if let Some(c) = cur {
            if let Some(at) = cam.to_screen(scene(c, z0)) {
                let elev = self.mep.z.unwrap_or(0.0);
                let shown = if ctrl { c.2 - z0 } else { elev };
                let mut txt = format!("{} {}", self.t("e3d.mep.elev"), fmt_len(shown as f32, ed));
                if let Some(h) = &hit {
                    let k = match h {
                        MepHit::Port(_) => "e3d.mep.port",
                        MepHit::RunEnd { .. } => "e3d.mep.end",
                        MepHit::RunMid { .. } => "e3d.mep.on_run",
                    };
                    paint::snap(painter, at, SnapKind::Vertex, &self.t(k), ov);
                    txt = format!("{} · {txt}", self.t(k));
                }
                paint::label(painter, at + Vec2::new(18.0, -30.0), &txt, fg, bg, hit.as_ref().map(|_| ov.snap));
            }
        }
    }
}

/// Ribbon commands `e3d.duct`, `e3d.pipe`, `e3d.tray`.
pub fn register(a: &mut crate::shell::EditorApp) {
    fn arm(a: &mut crate::shell::EditorApp, k: Kind) {
        a.view.e3.mep.lang = Some(a.sh.cat.lang);
        super::open(a, Tool::Mep(k));
    }
    a.register_command("e3d.duct", |a| arm(a, Kind::Duct));
    a.register_command("e3d.pipe", |a| arm(a, Kind::Pipe));
    a.register_command("e3d.tray", |a| arm(a, Kind::Tray));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn tool(ed: &Editor, k: Kind) -> State {
        let mut s = State::default();
        s.set_tool(Tool::Mep(k));
        let _ = s.elev(ed);
        s
    }

    #[test]
    fn a_free_chain_with_a_riser_is_one_run_and_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut s = tool(&ed, Kind::Duct);
        let z = ed.elevation + s.z.unwrap();
        assert!((s.z.unwrap() - 2.9).abs() < 1e-9, "the plan tool's default elevation");
        assert!(s.click(&ed, (0.0, 0.0, 0.0), None).is_none());
        assert!(s.click(&ed, (4.0, 0.0, 99.0), None).is_none());
        s.rise(&ed, 1.5);
        assert!(s.click(&ed, (4.0, 3.0, 0.0), None).is_none());
        assert_eq!(
            s.pts,
            vec![(0.0, 0.0, z), (4.0, 0.0, z), (4.0, 0.0, z + 1.5), (4.0, 3.0, z + 1.5)]
        );
        let before = ed.model.mep_runs(None).unwrap().len();
        let id = s.commit(&mut ed, None).unwrap();
        assert!(s.pts.is_empty());
        let run = ed.model.mep_run(id).unwrap();
        assert_eq!(run.points.len(), 4);
        assert_eq!(run.section.domain, MepDomain::Duct);
        assert!(ed.model.mep_network(Some(run.system)).is_ok());
        assert_eq!(ed.model.mep_runs(None).unwrap().len(), before + 1);
        ed.undo();
        assert_eq!(ed.model.mep_runs(None).unwrap().len(), before, "one undo step");
    }

    #[test]
    fn a_branch_from_a_run_takes_its_system_and_section_and_ends_on_a_network() {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::mep3d::demo(&mut ed).unwrap();
        let ducts: Vec<i64> = ed
            .model
            .mep_runs(None)
            .unwrap()
            .into_iter()
            .filter(|&i| ed.model.mep_run(i).unwrap().section.domain == MepDomain::Duct)
            .collect();
        // the longest straight duct leg: a tee needs room for its trims
        let (main_id, k) = ducts
            .iter()
            .flat_map(|&i| (0..ed.model.mep_run(i).unwrap().points.len() - 1).map(move |k| (i, k)))
            .max_by(|x, y| {
                let len = |(i, k): (i64, usize)| {
                    let p = ed.model.mep_run(i).unwrap().points;
                    (p[k + 1].0 - p[k].0).hypot(p[k + 1].1 - p[k].1)
                };
                len(*x).total_cmp(&len(*y))
            })
            .unwrap();
        let main = ed.model.mep_run(main_id).unwrap();
        let (a, b) = (main.points[k], main.points[k + 1]);
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, (a.2 + b.2) / 2.0);
        let mut s = tool(&ed, Kind::Duct);
        let hit = s.hit(&ed, mid, 0.1, 0.3);
        assert!(matches!(hit, Some(MepHit::RunMid { run, .. }) if run == main_id), "{hit:?}");
        s.click(&ed, mid, hit);
        assert!((s.z.unwrap() + ed.elevation - mid.2).abs() < 1e-9, "the run's elevation");
        // across the main axis, away from it
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let n = dx.hypot(dy);
        let side = (mid.0 - dy / n * 3.0, mid.1 + dx / n * 3.0, 0.0);
        s.click(&ed, side, None);
        s.rise(&ed, -1.5);
        let before = ed.model.mep_runs(None).unwrap().len();
        let id = s.commit(&mut ed, None).unwrap();
        let run = ed.model.mep_run(id).unwrap();
        assert_eq!(run.system, main.system, "the start run's system");
        assert_eq!(run.section, main.section, "the start run's section");
        assert!((run.points.last().unwrap().2 - (mid.2 - 1.5)).abs() < 1e-6, "riser down");
        assert_eq!(ed.model.mep_runs(None).unwrap().len(), before + 1);
        assert!(ed.model.mep_network(Some(main.system)).is_ok());
        ed.undo();
        assert_eq!(ed.model.mep_runs(None).unwrap().len(), before);
    }

    #[test]
    fn an_end_on_a_lower_network_gets_a_vertical_joint() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut s = tool(&ed, Kind::Pipe);
        s.click(&ed, (0.0, 0.0, 0.0), None);
        s.click(&ed, (5.0, 0.0, 0.0), None);
        let first = s.commit(&mut ed, None).unwrap();
        let z = ed.model.mep_run(first).unwrap().points[0].2;
        // a second pipe one metre higher ends on the first one's end
        s.rise(&ed, 1.0);
        s.click(&ed, (5.0, 4.0, 0.0), None);
        let end = s.hit(&ed, (5.0, 0.0, z), 0.1, 0.3);
        assert!(matches!(end, Some(MepHit::RunEnd { .. })), "{end:?}");
        let w = s.click(&ed, (5.0, 0.0, 0.0), end.clone());
        assert_eq!(w, Some(end.clone()), "a point on a network ends the run");
        let id = s.commit(&mut ed, end).unwrap();
        let p = ed.model.mep_run(id).unwrap().points;
        let near = |a: (f64, f64, f64), b: (f64, f64, f64)| {
            (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs() < 1e-6
        };
        assert!(near(p[0], (5.0, 4.0, z + 1.0)), "{p:?}");
        assert!(near(p[p.len() - 1], (5.0, 0.0, z)), "joined at the network's elevation: {p:?}");
        assert!(near(p[p.len() - 2], (5.0, 0.0, z + 1.0)), "vertical joint: {p:?}");
    }

    #[test]
    fn a_short_chain_is_refused_with_a_translated_reason() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut s = tool(&ed, Kind::Tray);
        s.click(&ed, (0.0, 0.0, 0.0), None);
        let e = s.commit(&mut ed, None).unwrap_err();
        let txt = super::super::err_text(false, &e);
        assert!(!txt.starts_with("e3d."), "{txt}");
    }
}
