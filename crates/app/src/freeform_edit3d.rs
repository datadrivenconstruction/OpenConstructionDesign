//! Freeform NURBS elements in 3D: the scene parts (triangles of the exact patch or shell, so
//! picking and snapping work as for any body) and one grip per control point. A drag moves
//! the point in its horizontal plane, Ctrl lifts it, Shift at the press moves it along the
//! surface normal, Alt drags its weight; each drop is one undoable model edit. The exact
//! patch and its boundary feed the curve and surface snaps.

use std::sync::Arc;

use crate::edit3d::exact::{self, ExactProvider, Shape3};
use crate::edit3d::grips::{self, Axis3, Grip3, Provider, Role3};
use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d::{self, Part, SceneProvider};
use crate::editor::Editor;
use ddc_geom::{v3, V3};
use ddc_model::{Error, Model};
use ddc_render::glam::Vec3;

/// Pick category: `Cat::Ext(CAT)`, the target id is the freeform id.
pub const CAT: u16 = crate::scene_ids::EXT_FREEFORM;

/// Grip numbers: control point (i, j) is `i * GRID + j`; `WEIGHT` marks a weight drag.
const GRID: u32 = 1 << 15;
const WEIGHT: u32 = 1 << 31;

fn decode(code: u32) -> (usize, usize, bool) {
    let c = code & !WEIGHT;
    ((c / GRID) as usize, (c % GRID) as usize, code & WEIGHT != 0)
}

fn g(v: V3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

struct Freeforms;

impl SceneProvider for Freeforms {
    fn key(&self) -> &'static str {
        "freeform"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let Ok(rows) = ed.model.freeforms(Some(level)) else {
            return vec![];
        };
        let z0 = ed.elevation as f32;
        rows.into_iter()
            .filter_map(|(id, _)| {
                // a patch that does not tessellate stays out of the scene, the model keeps it
                let fm = ed.model.freeform_mesh(id).ok()?;
                let mut q = Part::new(Target::new(Cat::Ext(CAT), id), ddc_render::TAG_PANEL);
                q.pos = fm.mesh.positions.iter().map(|p| g(*p) - Vec3::new(0.0, 0.0, z0)).collect();
                q.tris = fm.mesh.triangles.clone();
                Some(q)
            })
            .collect()
    }

    fn tables(&self) -> &'static [&'static str] {
        &["freeform", "level"]
    }

    fn main_table(&self) -> Option<&'static str> {
        Some("freeform")
    }
}

/// Elevation of `level` relative to the active level `z0`.
fn level_dz(m: &Model, level: i64, z0: f64) -> Result<f64, Error> {
    let lv = m.level_list()?;
    let e = lv.iter().find(|l| l.0 == level).map_or(0.0, |l| l.2);
    Ok(e - z0)
}

struct FreeformGrips;

impl Provider for FreeformGrips {
    fn key(&self) -> &'static str {
        "freeform"
    }

    fn owns(&self, t: Target) -> bool {
        t.cat == Cat::Ext(CAT)
    }

    fn grips(&self, m: &Model, z0: f64, t: Target) -> Result<Vec<Grip3>, Error> {
        let s = m.freeform(t.id)?;
        let dz = level_dz(m, s.level, z0)?;
        let nv = s.surface.nv();
        Ok(s.surface
            .ctrl()
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let pos = g(*p + v3(0.0, 0.0, dz));
                Grip3 {
                    target: t,
                    role: Role3::Ext((k / nv) as u32 * GRID + (k % nv) as u32),
                    pos,
                    axis: Axis3::Plane(Vec3::Z),
                    anchor: pos,
                }
            })
            .collect())
    }

    fn apply(&self, m: &mut Model, z0: f64, gr: &Grip3, to: Vec3) -> Result<(), Error> {
        let Role3::Ext(code) = gr.role else {
            return Err(Error::Invalid("foreign grip".into()));
        };
        let (i, j, weight) = decode(code);
        if weight {
            return m.set_freeform_weight(gr.target.id, i, j, gr.value(to) as f64);
        }
        let s = m.freeform(gr.target.id)?;
        let dz = level_dz(m, s.level, z0)?;
        let p = v3(to.x as f64, to.y as f64, to.z as f64 - dz);
        m.move_freeform_point(gr.target.id, i, j, p)
    }

    /// Shift: move along the surface normal at the point's Greville parameters.
    /// Alt: drag its weight along that normal (1 m = 1 unit; the typed value is the weight).
    fn variant(&self, m: &Model, _z0: f64, gr: &Grip3, alt: bool, shift: bool) -> Option<Grip3> {
        let Role3::Ext(code) = gr.role else {
            return None;
        };
        let (i, j, _) = decode(code);
        let s = m.freeform(gr.target.id).ok()?.surface;
        let (gu, gv) = (s.greville_u(), s.greville_v());
        let n = g(s.normal(*gu.get(i)?, *gv.get(j)?)?).normalize_or_zero();
        if n == Vec3::ZERO {
            return None;
        }
        if alt {
            let w = s.weights()[i * s.nv() + j] as f32;
            return Some(Grip3 {
                role: Role3::Ext(code | WEIGHT),
                axis: Axis3::Line(n),
                anchor: gr.pos - n * w,
                ..gr.clone()
            });
        }
        shift.then(|| Grip3 { axis: Axis3::Line(n), anchor: gr.pos, ..gr.clone() })
    }

    /// The control polygon row and column through the moved point; for a weight drag the
    /// two iso curves of the reweighted patch through the point's Greville parameters.
    fn preview(&self, m: &Model, z0: f64, gr: &Grip3, to: Vec3) -> Result<Vec<Vec<Vec3>>, Error> {
        let Role3::Ext(code) = gr.role else {
            return Ok(vec![vec![gr.pos, to]]);
        };
        let s = m.freeform(gr.target.id)?;
        let dz = level_dz(m, s.level, z0)?;
        let (i, j, weight) = decode(code);
        if weight {
            let w = gr.value(to) as f64;
            let sw = s.surface.with_weight(i, j, w).map_err(|_| Error::Invalid(format!("weight {w}")))?;
            let (gu, gv) = (sw.greville_u(), sw.greville_v());
            let iso = |c: Result<ddc_geom::nurbs::Curve, _>| -> Vec<Vec3> {
                let Ok(c) = c else { return vec![] };
                let (a, b) = c.domain();
                (0..=32).map(|k| g(c.point(a + (b - a) * k as f64 / 32.0) + v3(0.0, 0.0, dz))).collect()
            };
            return Ok(vec![iso(sw.iso_u(gu[i])), iso(sw.iso_v(gv[j])), vec![gr.pos, to]]);
        }
        let (nu, nv) = (s.surface.nu(), s.surface.nv());
        let at = |a: usize, b: usize| {
            if (a, b) == (i, j) {
                to
            } else {
                g(s.surface.ctrl()[a * nv + b] + v3(0.0, 0.0, dz))
            }
        };
        Ok(vec![(0..nv).map(|b| at(i, b)).collect(), (0..nu).map(|a| at(a, j)).collect()])
    }
}

/// Exact patches of the active level for curve and surface snaps: the surface and its four
/// boundary curves, in scene coordinates.
struct FreeformShapes;

impl ExactProvider for FreeformShapes {
    fn key(&self) -> &'static str {
        "freeform"
    }

    fn shapes(&self, ed: &Editor) -> Vec<Shape3> {
        let Ok(rows) = ed.model.freeforms(Some(ed.level)) else {
            return vec![];
        };
        rows.into_iter()
            .filter_map(|(id, s)| {
                let dz = level_dz(&ed.model, s.level, ed.elevation).ok()?;
                let sf = s.surface.translated(v3(0.0, 0.0, dz));
                let curves = sf.boundary().map(|b| b.to_vec()).unwrap_or_default();
                Some(Shape3::new(Target::new(Cat::Ext(CAT), id), curves, Some(sf)))
            })
            .collect()
    }
}

/// Wires freeform elements into the 3D scene, grips and exact snaps.
pub fn install() {
    scene3d::register(Arc::new(Freeforms));
    grips::register(Arc::new(FreeformGrips));
    exact::register(Arc::new(FreeformShapes));
}
