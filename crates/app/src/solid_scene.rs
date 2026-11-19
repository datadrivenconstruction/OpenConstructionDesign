// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed solids for 3D, independent of the scene engine: parts (a solid or a translucent void
//! ghost), depth/height/angle handles, a work plane on a face. Glue with the 3D scene and
//! handle providers lives in `solid_scene_edit3d.rs` (module `solid_edit3d`, installed in `edit3d::register`).

use ddc_geom::frame3::{self, GripKey, GripMove};
use ddc_geom::gsolid::{Form, Frame};
use ddc_geom::{v2, v3, V2, V3};
use ddc_model::{Model, SolidSpec};

pub const TAG_SOLID: u32 = ddc_render::TAG_PANEL;
/// Voids are drawn as ghosts: the glass is rendered transparent after the opaque faces.
pub const TAG_VOID: u32 = ddc_render::TAG_GLASS;

/// Solid in the scene frame (z is relative to `z0`).
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub id: i64,
    pub void: bool,
    pub tag: u32,
    pub pos: Vec<[f32; 3]>,
    pub tris: Vec<[u32; 3]>,
}

/// Solids of a level (`None` — all levels) and solids whose mesh could not be built.
pub fn parts(m: &Model, level: Option<i64>, z0: f64) -> (Vec<Part>, Vec<(i64, String)>) {
    let Ok(all) = m.solids(level) else {
        return (vec![], vec![]);
    };
    let mut out = vec![];
    let mut bad = vec![];
    for (id, s) in all {
        match m.solid_mesh(id) {
            Ok(mesh) => out.push(Part {
                id,
                void: s.void,
                tag: if s.void { TAG_VOID } else { TAG_SOLID },
                pos: mesh
                    .positions
                    .iter()
                    .map(|p| [p.x as f32, p.y as f32, (p.z - z0) as f32])
                    .collect(),
                tris: mesh.triangles.clone(),
            }),
            Err(e) => bad.push((id, e.to_string())),
        }
    }
    (out, bad)
}

/// Prism with a vertical axis — such voids cut the walls of the 3D scene.
pub fn vertical_prism(s: &SolidSpec) -> bool {
    matches!(s.form, Form::Extrusion { .. }) && s.frame.z.z.abs() > 1.0 - 1e-9
}

/// Voids that cut walls in quantities but not in 3D (non-vertical prisms).
pub fn uncut_wall_voids(m: &Model) -> Vec<i64> {
    let Ok(all) = m.void_solids() else {
        return vec![];
    };
    all.into_iter()
        .filter(|(id, s)| {
            s.void
                && !vertical_prism(s)
                && m.solid_cuts(*id)
                    .is_ok_and(|c| c.iter().any(|k| k.kind == "wall"))
        })
        .map(|(id, _)| id)
        .collect()
}

// ---------- work plane ----------

/// Work plane on a face: origin, normal, x direction (projected onto the face).
pub fn frame_from_face(o: V3, n: V3, u: V3) -> Option<Frame> {
    let nl = n.len();
    if !(nl > 1e-9) {
        return None;
    }
    let z = n * (1.0 / nl);
    let mut x = u - z * u.dot(z);
    if x.len() < 1e-9 {
        // x is along the normal: pick any perpendicular
        let a = if z.x.abs() < 0.9 {
            v3(1.0, 0.0, 0.0)
        } else {
            v3(0.0, 1.0, 0.0)
        };
        x = a - z * a.dot(z);
    }
    let x = x * (1.0 / x.len());
    let y = z.cross(x);
    Some(Frame { o, x, y, z })
}

/// Plan point carried vertically onto the plane: local (x, y); None — the plane
/// is nearly vertical (a sketch on it cannot be defined from the plan).
pub fn onto(f: &Frame, p: V2) -> Option<V2> {
    if f.z.z.abs() < 0.2 {
        return None;
    }
    let z = f.o.z - (f.z.x * (p.x - f.o.x) + f.z.y * (p.y - f.o.y)) / f.z.z;
    let l = f.local(v3(p.x, p.y, z));
    Some(v2(l.x, l.y))
}

// ---------- 3D handles ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Start,
    End,
    Height,
    Angle,
    /// Grip of a smooth sweep path (`frame3::GripKey` code).
    Path(u32),
}

/// Path grip codes start here (`GripKey` codes stay below 4 << 20).
const PATH: u32 = 1 << 24;

impl Handle {
    pub fn code(self) -> u32 {
        match self {
            Handle::Start => 0,
            Handle::End => 1,
            Handle::Height => 2,
            Handle::Angle => 3,
            Handle::Path(k) => PATH + k,
        }
    }
    pub fn from_code(c: u32) -> Option<Handle> {
        if c >= PATH {
            return GripKey::from_code(c - PATH).map(|k| Handle::Path(k.code()));
        }
        [Handle::Start, Handle::End, Handle::Height, Handle::Angle]
            .into_iter()
            .find(|h| h.code() == c)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis {
    /// Travel along a unit direction from `anchor`; value in m.
    Line(V3),
    /// Rotation about an axis through `center`; value is the angle (rad) from `zero`.
    Ring { center: V3, axis: V3, zero: V3 },
    /// Free point: dragged in the horizontal plane, lifted with Ctrl; applied by `apply_at`.
    Point,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grip {
    pub handle: Handle,
    pub pos: V3,
    pub anchor: V3,
    pub axis: Axis,
}

fn centroid(pts: &[V2]) -> V2 {
    if pts.is_empty() {
        return v2(0.0, 0.0);
    }
    let s = pts.iter().fold(v2(0.0, 0.0), |a, p| a + *p);
    s * (1.0 / pts.len() as f64)
}

fn unit(v: V3) -> V3 {
    let l = v.len();
    if l > 0.0 {
        v * (1.0 / l)
    } else {
        v
    }
}

fn rot(p: V3, a: V3, phi: f64) -> V3 {
    let (c, s) = (phi.cos(), phi.sin());
    p * c + a.cross(p) * s + a * (a.dot(p) * (1.0 - c))
}

/// Solid handles in the frame, where `dz` is the solid's level elevation minus the frame's z0.
pub fn grips3(s: &SolidSpec, dz: f64) -> Vec<Grip> {
    let f = s.frame;
    let at = |q: V2, z: f64| f.at(v3(q.x, q.y, z)) + v3(0.0, 0.0, dz);
    let zd = unit(f.dir(v3(0.0, 0.0, 1.0)));
    match &s.form {
        Form::Extrusion {
            profile,
            start,
            end,
        } => {
            let c = centroid(&profile.outer.pts);
            let anchor = at(c, 0.0);
            vec![
                Grip {
                    handle: Handle::Start,
                    pos: at(c, *start),
                    anchor,
                    axis: Axis::Line(zd),
                },
                Grip {
                    handle: Handle::End,
                    pos: at(c, *end),
                    anchor,
                    axis: Axis::Line(zd),
                },
            ]
        }
        Form::Blend { top, height, .. } => {
            let c = centroid(&top.pts);
            vec![Grip {
                handle: Handle::Height,
                pos: at(c, *height),
                anchor: at(c, 0.0),
                axis: Axis::Line(zd),
            }]
        }
        Form::Revolve {
            profile,
            axis_o,
            axis_d,
            start,
            end,
        } => {
            let l = axis_d.len();
            if l < 1e-12 {
                return vec![];
            }
            let d = *axis_d * (1.0 / l);
            let c = centroid(&profile.outer.pts);
            let foot = *axis_o + d * (c - *axis_o).dot(d);
            let ad = v3(d.x, d.y, 0.0);
            let arm = v3(c.x - foot.x, c.y - foot.y, 0.0);
            if arm.len() < 1e-9 {
                return vec![];
            }
            let fo = v3(foot.x, foot.y, 0.0);
            let w = |p: V3| f.at(p) + v3(0.0, 0.0, dz);
            let center = w(fo);
            let zero = w(fo + rot(arm, ad, *start)) - center;
            vec![Grip {
                handle: Handle::Angle,
                pos: w(fo + rot(arm, ad, *end)),
                anchor: center,
                axis: Axis::Ring {
                    center,
                    axis: unit(f.dir(ad)),
                    zero,
                },
            }]
        }
        Form::Sweep { path, .. } => {
            let Some(sm) = &path.smooth else {
                return vec![];
            };
            let w = |p: V3| f.at(p) + v3(0.0, 0.0, dz);
            frame3::grips(sm)
                .into_iter()
                .map(|g| Grip {
                    handle: Handle::Path(g.key.code()),
                    pos: w(g.pos),
                    anchor: w(g.anchor),
                    axis: match g.mv {
                        GripMove::Point => Axis::Point,
                        GripMove::Line(d) => Axis::Line(unit(f.dir(d))),
                        GripMove::Ring { center, axis, zero } => Axis::Ring {
                            center: w(center),
                            axis: unit(f.dir(axis)),
                            zero: f.dir(zero),
                        },
                    },
                })
                .collect()
        }
        _ => vec![],
    }
}

/// Solid after dragging a handle to point `to` (world, frame as in `grips3`): path handles of a smooth
/// sweep take the point, the others take the value along their own axis.
pub fn apply_at(s: &SolidSpec, g: &Grip, to: V3, dz: f64) -> Option<SolidSpec> {
    let Handle::Path(code) = g.handle else {
        return apply(s, g.handle, value(g, to));
    };
    let key = GripKey::from_code(code)?;
    let mut s = s.clone();
    let local = s.frame.local(to - v3(0.0, 0.0, dz));
    let Form::Sweep { path, .. } = &mut s.form else {
        return None;
    };
    let sm = frame3::grip_apply(path.smooth.as_ref()?, key, local)?;
    *path = ddc_geom::gsolid::Path3::smooth(sm);
    ddc_geom::gsolid::validate(&s.form).ok()?;
    Some(s)
}

/// Handle value at point `to`: m along the axis or angle (rad, 0..2π) from zero.
pub fn value(g: &Grip, to: V3) -> f64 {
    match g.axis {
        Axis::Line(d) => (to - g.anchor).dot(d),
        Axis::Point => (to - g.anchor).len(),
        Axis::Ring { center, axis, zero } => {
            let a = unit(axis);
            let p = to - center;
            let p = p - a * p.dot(a);
            let z = unit(zero);
            let y = a.cross(z);
            let t = p.dot(y).atan2(p.dot(z));
            if t < 0.0 {
                t + std::f64::consts::TAU
            } else {
                t
            }
        }
    }
}

/// Solid after setting a handle value; None — the value is invalid.
pub fn apply(s: &SolidSpec, h: Handle, v: f64) -> Option<SolidSpec> {
    if !v.is_finite() {
        return None;
    }
    let mut s = s.clone();
    match (h, &mut s.form) {
        (Handle::Start, Form::Extrusion { start, end, .. }) => {
            if (*end - v).abs() < 1e-3 {
                return None;
            }
            *start = v;
        }
        (Handle::End, Form::Extrusion { start, end, .. }) => {
            if (v - *start).abs() < 1e-3 {
                return None;
            }
            *end = v;
        }
        (Handle::Height, Form::Blend { height, .. }) => {
            if v < 1e-3 {
                return None;
            }
            *height = v;
        }
        (Handle::Angle, Form::Revolve { start, end, .. }) => {
            // 0 after a full turn means a full turn
            let sweep = if v < 1e-3 { std::f64::consts::TAU } else { v };
            *end = *start + sweep.min(std::f64::consts::TAU);
        }
        _ => return None,
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::SolidSection;

    fn spec(form: Form, frame: Frame) -> SolidSpec {
        SolidSpec {
            level: 1,
            name: String::new(),
            void: false,
            material: String::new(),
            ifc: None,
            frame,
            form,
        }
    }

    fn sq() -> Vec<V2> {
        vec![v2(0.0, 0.0), v2(2.0, 0.0), v2(2.0, 2.0), v2(0.0, 2.0)]
    }

    #[test]
    fn extrusion_end_grip_moves_along_axis() {
        let s = spec(
            Form::Extrusion {
                profile: SolidSection::polygon(sq()),
                start: 0.0,
                end: 3.0,
            },
            Frame::level(1.0),
        );
        let g = grips3(&s, 0.5);
        let end = g.iter().find(|g| g.handle == Handle::End).unwrap();
        assert!((end.pos - v3(1.0, 1.0, 4.5)).len() < 1e-12);
        let v = value(end, v3(5.0, 5.0, 6.5));
        assert!((v - 5.0).abs() < 1e-12);
        let n = apply(&s, Handle::End, v).unwrap();
        assert!(matches!(n.form, Form::Extrusion { end, .. } if (end - 5.0).abs() < 1e-12));
        assert!(apply(&s, Handle::End, 0.0).is_none());
        assert!(apply(&s, Handle::Height, 1.0).is_none());
    }

    #[test]
    fn revolve_angle_grip_round_trips() {
        let s = spec(
            Form::Revolve {
                profile: SolidSection::polygon(vec![
                    v2(1.0, 0.0),
                    v2(2.0, 0.0),
                    v2(2.0, 1.0),
                    v2(1.0, 1.0),
                ]),
                axis_o: v2(0.0, 0.0),
                axis_d: v2(0.0, 1.0),
                start: 0.0,
                end: std::f64::consts::FRAC_PI_2,
            },
            Frame::default(),
        );
        let g = grips3(&s, 0.0);
        assert_eq!(g.len(), 1);
        let v = value(&g[0], g[0].pos);
        assert!((v - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "{v}");
        let n = apply(&s, Handle::Angle, 1.0).unwrap();
        assert!(matches!(n.form, Form::Revolve { end, .. } if (end - 1.0).abs() < 1e-12));
        let full = apply(&s, Handle::Angle, 0.0).unwrap();
        assert!(
            matches!(full.form, Form::Revolve { end, .. } if (end - std::f64::consts::TAU).abs() < 1e-12)
        );
    }

    #[test]
    fn smooth_path_grips_move_in_3d() {
        use ddc_geom::frame3::{Seg, Smooth};
        let sq = vec![v2(-0.1, -0.1), v2(0.1, -0.1), v2(0.1, 0.1), v2(-0.1, 0.1)];
        let sm = Smooth::new(
            v3(0.0, 0.0, 0.0),
            vec![Seg::Arc {
                via: v3(2.0, 1.0, 0.0),
                to: v3(4.0, 0.0, 0.0),
            }],
        );
        let s = spec(
            Form::Sweep {
                profile: SolidSection::polygon(sq),
                path: ddc_geom::gsolid::Path3::smooth(sm),
            },
            Frame::level(1.0),
        );
        let g = grips3(&s, 0.5);
        assert_eq!(g.len(), 3, "start, via, end");
        let via = g
            .iter()
            .find(|k| matches!(k.handle, Handle::Path(c) if c == GripKey::Via(0, 0).code()))
            .unwrap();
        assert!((via.pos - v3(2.0, 1.0, 1.5)).len() < 1e-12, "{:?}", via.pos);
        assert_eq!(Handle::from_code(via.handle.code()), Some(via.handle));
        let n = apply_at(&s, via, v3(2.0, 1.5, 2.0), 0.5).unwrap();
        let Form::Sweep { path, .. } = &n.form else { panic!() };
        let sm = path.smooth.as_ref().unwrap();
        assert!(matches!(sm.segs[0], Seg::Arc { via, .. } if (via - v3(2.0, 1.5, 0.5)).len() < 1e-12));
        assert!(apply_at(&s, via, v3(f64::NAN, 0.0, 0.0), 0.5).is_none());
    }

    #[test]
    fn face_frame_and_projection() {
        let f = frame_from_face(v3(0.0, 0.0, 3.0), v3(0.0, 0.0, 2.0), v3(1.0, 0.0, 0.5)).unwrap();
        assert!((f.det() - 1.0).abs() < 1e-12);
        assert!((f.x - v3(1.0, 0.0, 0.0)).len() < 1e-12);
        assert_eq!(onto(&f, v2(2.0, 1.0)), Some(v2(2.0, 1.0)));
        // 45° tilt about x: plan point (0, 1) lies on the face at local y = √2
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let t = frame_from_face(v3(0.0, 0.0, 0.0), v3(0.0, -s, s), v3(1.0, 0.0, 0.0)).unwrap();
        let q = onto(&t, v2(0.0, 1.0)).unwrap();
        assert!((q - v2(0.0, 2f64.sqrt())).len() < 1e-12, "{q:?}");
        let wall =
            frame_from_face(v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 0.0, 1.0)).unwrap();
        assert!(onto(&wall, v2(1.0, 1.0)).is_none());
        assert!(frame_from_face(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0)).is_none());
    }
}
