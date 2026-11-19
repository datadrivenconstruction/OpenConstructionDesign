//! Glue between typed solids and 3D edits (`edit3d`: scene and handle providers, work plane).
//! Wired in as `solid_edit3d` (`lib.rs`), `install()` runs from `edit3d::register`. The logic
//! and its tests live in `solid_scene`.

use std::sync::Arc;

use crate::edit3d::grips::{self, Axis3, Grip3, Provider, Role3};
use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::plane::WorkPlane;
use crate::edit3d::scene3d::{self, InstPart, Part, SceneProvider};
use crate::editor::Editor;
use crate::solid_scene::{self as ss, Axis, Handle};
use ddc_geom::{v3, V3};
use ddc_model::{Error, Model, SolidFrame};
use ddc_render::glam::{Mat4, Vec3, Vec4};
use ddc_render::SceneMesh;

/// Solid selection category: `Cat::Ext(CAT)`, target id is the solid id.
pub const CAT: u16 = crate::scene_ids::EXT_SOLID;

fn g(v: V3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

fn d(v: Vec3) -> V3 {
    v3(v.x as f64, v.y as f64, v.z as f64)
}

/// A solid's frame (level elevation in) as a scene placement, z from the scene's `z0`.
fn placement(fr: &SolidFrame, z0: f64) -> Mat4 {
    let c = |v: V3, w: f32| Vec4::new(v.x as f32, v.y as f32, v.z as f32, w);
    let o = v3(fr.o.x, fr.o.y, fr.o.z - z0);
    Mat4::from_cols(c(fr.x, 0.0), c(fr.y, 0.0), c(fr.z, 0.0), c(o, 1.0))
}

struct Solids;

impl SceneProvider for Solids {
    fn key(&self) -> &'static str {
        "solid"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let (parts, _) = ss::parts(&ed.model, Some(level), ed.elevation);
        parts
            .into_iter()
            .map(|p| {
                let mut q = Part::new(Target::new(Cat::Ext(CAT), p.id), p.tag);
                q.pos = p.pos.iter().map(|a| Vec3::from_array(*a)).collect();
                q.tris = p.tris;
                q
            })
            .collect()
    }

    /// Solids of one form (the bolts and plates of many connections) share one mesh.
    fn parts_inst(&self, ed: &Editor, level: i64) -> (Vec<Part>, Vec<InstPart>) {
        let Ok(groups) = ed.model.solid_groups(Some(level)) else {
            return (vec![], vec![]);
        };
        let z0 = ed.elevation;
        let (mut plain, mut inst) = (vec![], vec![]);
        for grp in groups {
            let Ok(mesh) = &grp.mesh else { continue };
            let repeated = grp.items.iter().filter(|i| !i.2).count() >= 2;
            let proto = repeated.then(|| {
                Arc::new(SceneMesh {
                    element: 0,
                    tag: ss::TAG_SOLID,
                    positions: mesh.positions.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
                    triangles: mesh.triangles.clone(),
                })
            });
            for &(id, fr, void) in &grp.items {
                let target = Target::new(Cat::Ext(CAT), id);
                match &proto {
                    Some(pr) if !void => inst.push(InstPart {
                        target,
                        tag: ss::TAG_SOLID,
                        key: grp.key,
                        proto: pr.clone(),
                        m: placement(&fr, z0),
                    }),
                    _ => {
                        let mut q = Part::new(target, if void { ss::TAG_VOID } else { ss::TAG_SOLID });
                        q.pos = mesh
                            .positions
                            .iter()
                            .map(|&p| {
                                let w = fr.at(p);
                                Vec3::new(w.x as f32, w.y as f32, (w.z - z0) as f32)
                            })
                            .collect();
                        q.tris = mesh.triangles.clone();
                        plain.push(q);
                    }
                }
            }
        }
        (plain, inst)
    }

    fn tables(&self) -> &'static [&'static str] {
        &["solid", "solid_host", "solid_cut", "level"]
    }

    fn main_table(&self) -> Option<&'static str> {
        Some("solid")
    }
}

fn level_dz(m: &Model, level: i64, z0: f64) -> Result<f64, Error> {
    let lv = m.level_list()?;
    let e = lv.iter().find(|l| l.0 == level).map_or(0.0, |l| l.2);
    Ok(e - z0)
}

struct SolidGrips;

impl Provider for SolidGrips {
    fn owns(&self, t: Target) -> bool {
        t.cat == Cat::Ext(CAT)
    }

    fn grips(&self, m: &Model, z0: f64, t: Target) -> Result<Vec<Grip3>, Error> {
        let s = m.solid(t.id)?;
        let dz = level_dz(m, s.level, z0)?;
        Ok(ss::grips3(&s, dz)
            .into_iter()
            .map(|k| Grip3 {
                target: t,
                role: Role3::Ext(k.handle.code()),
                pos: g(k.pos),
                axis: match k.axis {
                    Axis::Line(v) => Axis3::Line(g(v)),
                    Axis::Point => Axis3::Plane(Vec3::Z),
                    Axis::Ring { center, axis, zero } => Axis3::Ring {
                        center: g(center),
                        axis: g(axis),
                        zero: g(zero),
                    },
                },
                anchor: g(k.anchor),
            })
            .collect())
    }

    fn apply(&self, m: &mut Model, z0: f64, gr: &Grip3, to: Vec3) -> Result<(), Error> {
        let Role3::Ext(code) = gr.role else {
            return Err(Error::Invalid("foreign grip".into()));
        };
        let h = Handle::from_code(code).ok_or_else(|| Error::Invalid("foreign grip".into()))?;
        let s = m.solid(gr.target.id)?;
        let dz = level_dz(m, s.level, z0)?;
        let k = ss::grips3(&s, dz)
            .into_iter()
            .find(|k| k.handle == h)
            .ok_or_else(|| Error::Invalid("stale grip".into()))?;
        let next = ss::apply_at(&s, &k, d(to), dz)
            .ok_or_else(|| Error::Invalid("solid: invalid size or path".into()))?;
        m.update_solid(gr.target.id, &next)
    }
}

/// Sketch work plane for solids from a 3D plane (face, reference plane, level).
pub fn work_plane(p: &WorkPlane) -> Option<SolidFrame> {
    ss::frame_from_face(d(p.origin), d(p.n), d(p.u))
}

/// Hook solids into the 3D scene and handles.
pub fn install() {
    scene3d::register(Arc::new(Solids));
    grips::register(Arc::new(SolidGrips));
}
