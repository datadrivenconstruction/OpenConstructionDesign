// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lowering of `geometry.*` ops (docs/design/AI_NATIVE.md section 4) onto the typed geometry
//! requests of the model: extrusions (`solids`, `mass`), sweeps and lofts (`geo_req`), linked
//! booleans and voids (`mass_bool`), kind conversion (`geo_convert`). A form, profile or path
//! without an exact lowering is rejected with the reason, never approximated.
//!
//! Coordinates in ops are absolute (project z); model forms store z above the level, so every
//! point and frame origin is shifted by the level elevation here.

use ddc_geom::csg::BoolOp;
use ddc_geom::gsolid::{self, Frame as GFrame, Section as GSection};
use ddc_geom::profile_cat::{self, PointList, ProfileSpec, Quantity};
use ddc_geom::v3;
use ddc_ops::{
    BoolKind, Cat, ConvertTo as OpConvertTo, Form, Frame, GeoOp, Len, Path, Profile, Ref, Tool, P2,
};

use super::{bad_ref, bad_value, level_row, resolve, resolve_as, row_exists, table, unsupported};
use super::{Fail, Run, EPS};
use crate::{
    BoolRequest, ConvertRequest, ConvertTo, GeoBuilt, GeoMeta, GeoReq, LoftReq, LoftSection,
    MassSpec, Model, PathReq, ProfileReq, SolidSpec, SweepReq, VoidRequest,
};

/// Chord sag for arcs of a profile turned into a mass ring, metres.
const SAG: f64 = 1e-3;
/// Tables a void may cut (`mass_bool::apply_void` hosts).
const VOID_HOSTS: [&str; 6] = ["wall", "slab", "roof", "col", "beam", "mass"];

pub(super) fn lower(m: &mut Model, run: &mut Run, g: &GeoOp) -> Result<(), Fail> {
    match g {
        GeoOp::Solid { slot, level, form, material, name, ifc, role } => {
            let ifc = ifc
                .as_ref()
                .map(|c| crate::SolidIfc::new(&c.class, c.predefined.as_deref()))
                .transpose()
                .map_err(|e| bad_value(e.to_string()))?;
            let meta = GeoMeta {
                level: opt_level(m, run, level.as_ref())?,
                name: name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Solid".into()),
                // a part without a material takes the one its name hints at, else the default
                material: material.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| {
                    crate::material_look::default_material("solid", name.as_deref().unwrap_or("")).into()
                }),
                void: false,
                ifc,
            };
            let built = build(m, run, &meta, form, false)?;
            let solid = match &built {
                GeoBuilt::Solid(id) => Some(*id),
                _ => None,
            };
            made(run, *slot, built);
            if let (Some(r), Some(solid)) = (role, solid) {
                let level = need_level(meta.level, "a part with a role")?;
                let comp = add_role(m, level, r)?;
                m.link_role_comp(solid, comp)?;
            }
            Ok(())
        }
        GeoOp::Mass { slot, level, form, name } => {
            let meta = GeoMeta {
                level: opt_level(m, run, level.as_ref())?,
                name: name.clone().unwrap_or_else(|| "Mass".into()),
                material: String::new(),
                void: false,
                ifc: None,
            };
            let built = build(m, run, &meta, form, true)?;
            made(run, *slot, built);
            Ok(())
        }
        GeoOp::Boolean { kind, a, b, keep_tool } => boolean(m, run, *kind, a, b, *keep_tool),
        GeoOp::Void { host, form } => void(m, run, host, form),
        GeoOp::Reshape { target, form } => reshape(m, run, target, form),
        GeoOp::Convert { target, to } => convert(m, run, target, *to),
        GeoOp::Freeform { .. }
        | GeoOp::EditPoint { .. }
        | GeoOp::Thicken { .. }
        | GeoOp::Trim { .. }
        | GeoOp::Rebuild { .. }
        | GeoOp::Panelize { .. }
        | GeoOp::SurfaceShell { .. } => super::ops_freeform::lower(m, run, g),
        GeoOp::Gridshell { .. } => super::ops_lattice::lower(m, run, g),
        GeoOp::Member { .. } | GeoOp::Plate { .. } => super::ops_member::lower(m, run, g),
    }
}

/// The family a role names, loaded on first use: a marker body only, the part's own solid is
/// the visible geometry.
fn role_family(m: &mut Model, role: &str) -> Result<i64, Fail> {
    let mut c = role.chars();
    let word = c.next().is_some_and(|f| f.is_ascii_alphabetic())
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !word {
        return Err(bad_value(format!("role `{role}` must start with a letter and use letters, digits or _")));
    }
    let name = role[..1].to_ascii_uppercase() + &role[1..];
    if let Some(id) = m.family_id(&name)? {
        return Ok(id);
    }
    let def = format!(
        "family {name}
category generic
host none
param Size length type 10mm
type \"Default\" Size=10mm
extrude Marker rect(-Size, -Size, Size, Size) z(0, Size)
"
    );
    Ok(m.load_family(&def)?)
}

/// One component row of the role's family at `r.at` (project z, as for the solid).
fn add_role(m: &mut Model, level: i64, r: &ddc_ops::RoleTag) -> Result<i64, Fail> {
    let family = role_family(m, &r.name)?;
    let ctype = m
        .comp_types(family)?
        .first()
        .map(|t| t.0)
        .ok_or_else(|| bad_value(format!("role `{}`: the family has no type", r.name)))?;
    let [x, y, z] = r.at.to_m();
    let mut spec = crate::CompSpec::at(level, ctype, x, y);
    spec.at.2 = z - elevation(m, level)?;
    Ok(m.add_component(&spec)?)
}

fn made(run: &mut Run, slot: u32, b: GeoBuilt) {
    match b {
        GeoBuilt::Solid(id) => run.made(slot, Cat::Solid, id),
        GeoBuilt::Mass(id) => run.made(slot, Cat::Mass, id),
    }
}

fn opt_level(m: &Model, run: &Run, r: Option<&Ref>) -> Result<Option<i64>, Fail> {
    r.map(|r| resolve_as(m, run, r, Cat::Level)).transpose()
}

fn need_level(level: Option<i64>, what: &str) -> Result<i64, Fail> {
    level.ok_or_else(|| bad_value(format!("{what} needs `level`")))
}

fn elevation(m: &Model, level: i64) -> Result<f64, Fail> {
    Ok(level_row(m, level)?.1)
}

/// One element from a form: a solid, or a mass for `geometry.mass` extrusions. Sweeps and lofts
/// go through `geo_req`, which picks solid or mass by the form; the receipt carries the real cat.
fn build(m: &mut Model, run: &Run, meta: &GeoMeta, form: &Form, mass: bool) -> Result<GeoBuilt, Fail> {
    match form {
        Form::Extrude { profile, frame, start, end } => {
            let level = need_level(meta.level, "an extrusion")?;
            if mass {
                let mut spec = mass_spec(m, level, form, &meta.name, false)?;
                spec.inplace = false;
                return Ok(GeoBuilt::Mass(m.add_mass(&spec)?));
            }
            let elev = elevation(m, level)?;
            let spec = SolidSpec {
                level,
                name: meta.name.clone(),
                void: meta.void,
                material: meta.material.clone(),
                frame: frame_of(frame, elev)?,
                form: gsolid::Form::Extrusion {
                    profile: section(profile)?,
                    start: start.to_m(),
                    end: end.to_m(),
                },
                ifc: meta.ifc.clone(),
            };
            gsolid::validate(&spec.form).map_err(|e| bad_value(format!("extrude: {e}")))?;
            Ok(GeoBuilt::Solid(m.add_solid(&spec)?))
        }
        Form::Sweep { profile, path, twist, scale } => {
            sweep(m, run, meta, profile, path, twist.map_or(0.0, |a| a.to_rad()), scale.unwrap_or(1.0))
        }
        Form::SweptBlend { a, b, path } if a == b => sweep(m, run, meta, a, path, 0.0, 1.0),
        Form::SweptBlend { .. } => Err(unsupported(
            "swept_blend between two different profiles is not lowered yet",
        )),
        Form::Loft { sections, path } => {
            let elev = match (meta.level, path) {
                (Some(l), _) => elevation(m, l)?,
                (None, Some(_)) => 0.0,
                (None, None) => return Err(bad_value("a loft without a path needs `level`")),
            };
            let mut secs = Vec::with_capacity(sections.len());
            for s in sections {
                let (x, z) = (unit3(&s.frame.x)?, unit3(&s.frame.z)?);
                if !(near(x, [1.0, 0.0, 0.0]) && near(z, [0.0, 0.0, 1.0])) {
                    return Err(unsupported(
                        "loft sections must lie in horizontal planes with x along +x",
                    ));
                }
                let o = s.frame.origin.to_m();
                secs.push(LoftSection { profile: profile_req(&s.profile)?, at: [o[0], o[1], o[2] - elev] });
            }
            let path = match path {
                Some(p) => Some(path_req(m, run, p, meta.level)?),
                None => None,
            };
            let req = GeoReq::Loft(LoftReq { meta: meta.clone(), sections: secs, path });
            Ok(m.apply_geo(&req)?)
        }
        Form::Revolve { .. } => Err(unsupported(
            "revolve about a 3D axis is not lowered yet; use extrude, sweep or loft",
        )),
    }
}

/// New form of an extruded solid, in place: the id, level, name, material and void flag stay,
/// so references and recipe links keep pointing at it. Other kinds are refused by name.
fn reshape(m: &mut Model, run: &mut Run, target: &Ref, form: &Form) -> Result<(), Fail> {
    let (cat, id) = resolve(m, run, target)?;
    let (Cat::Solid, Form::Extrude { profile, frame, start, end }) = (cat, form) else {
        return Err(unsupported(format!(
            "geometry.reshape of {} to a {} is not lowered yet; delete it and create it again",
            target.label(),
            form_kind(form)
        )));
    };
    let old = m.solid(id)?;
    if !matches!(old.form, gsolid::Form::Extrusion { .. }) {
        return Err(unsupported(format!("{} is not an extrusion", target.label())));
    }
    let elev = elevation(m, old.level)?;
    let spec = SolidSpec {
        frame: frame_of(frame, elev)?,
        form: gsolid::Form::Extrusion { profile: section(profile)?, start: start.to_m(), end: end.to_m() },
        ..old
    };
    gsolid::validate(&spec.form).map_err(|e| bad_value(format!("reshape: {e}")))?;
    m.update_solid(id, &spec)?;
    run.receipt.changed.push(Ref::id(Cat::Solid, id));
    Ok(())
}

fn form_kind(f: &Form) -> &'static str {
    match f {
        Form::Extrude { .. } => "extrude",
        Form::Revolve { .. } => "revolve",
        Form::Sweep { .. } => "sweep",
        Form::Loft { .. } => "loft",
        Form::SweptBlend { .. } => "swept_blend",
    }
}

fn sweep(
    m: &mut Model,
    run: &Run,
    meta: &GeoMeta,
    profile: &Profile,
    path: &Path,
    twist: f64,
    scale: f64,
) -> Result<GeoBuilt, Fail> {
    let req = GeoReq::Sweep(SweepReq {
        meta: meta.clone(),
        profile: profile_req(profile)?,
        path: path_req(m, run, path, meta.level)?,
        twist,
        scale,
        style: Default::default(),
    });
    Ok(m.apply_geo(&req)?)
}

// ---- profiles ----

fn q(v: f64) -> Quantity {
    Quantity::new(v, "m")
}

fn named(kind: &str, ps: &[(&str, f64)]) -> ProfileSpec {
    ProfileSpec::Named {
        kind: kind.into(),
        params: ps.iter().map(|(n, v)| (n.to_string(), q(*v))).collect(),
    }
}

fn flanged(kind: &str, h: &Len, b: &Len, tw: &Len, tf: &Len) -> ProfileSpec {
    named(kind, &[("height", h.to_m()), ("width", b.to_m()), ("web", tw.to_m()), ("flange", tf.to_m())])
}

fn points(pts: &[P2], bulges: Option<Vec<f64>>) -> PointList {
    PointList { unit: "m".into(), pts: pts.iter().map(|p| p.to_m()).collect(), bulges }
}

/// Wire profile to a catalogue request; lengths are passed in metres.
fn profile_spec(p: &Profile) -> Result<ProfileSpec, Fail> {
    Ok(match p {
        Profile::Rect { w, h } => named("rect", &[("width", w.to_m()), ("height", h.to_m())]),
        Profile::Circle { r } => named("circle", &[("diameter", 2.0 * r.to_m())]),
        Profile::Ring { r_out, r_in } => named(
            "ring",
            &[("outer_diameter", 2.0 * r_out.to_m()), ("inner_diameter", 2.0 * r_in.to_m())],
        ),
        Profile::I { h, b, tw, tf } => flanged("i", h, b, tw, tf),
        Profile::H { h, b, tw, tf } => flanged("h", h, b, tw, tf),
        Profile::C { h, b, tw, tf } => flanged("c", h, b, tw, tf),
        Profile::T { h, b, tw, tf } => flanged("t", h, b, tw, tf),
        Profile::L { h, b, t } => {
            named("l", &[("height", h.to_m()), ("width", b.to_m()), ("thickness", t.to_m())])
        }
        Profile::Z { h, b, t } => flanged("z", h, b, t, t),
        Profile::Tube { w, h, t } => {
            named("rect_tube", &[("width", w.to_m()), ("height", h.to_m()), ("wall", t.to_m())])
        }
        Profile::Polygon { pts } => ProfileSpec::Polygon(points(pts, None)),
        Profile::Polyline { pts, bulges, closed } => {
            if !closed {
                return Err(bad_value("profile polyline must be closed"));
            }
            let b = (!bulges.is_empty()).then(|| bulges.clone());
            ProfileSpec::Polygon(points(pts, b))
        }
        Profile::Rings { outer, holes } => ProfileSpec::Rings {
            outer: points(outer, None),
            holes: holes.iter().map(|h| points(h, None)).collect(),
        },
        Profile::Stored { of } => {
            return Err(unsupported(format!("stored profile {} is not resolved yet", of.label())))
        }
    })
}

/// Built section; a refused parameter becomes `bad_value` naming the field.
fn section(p: &Profile) -> Result<GSection, Fail> {
    let spec = profile_spec(p)?;
    profile_cat::build(&spec).map_err(|e| bad_value(format!("profile {}: {}", e.field, e.reason)))
}

fn profile_req(p: &Profile) -> Result<ProfileReq, Fail> {
    section(p)?;
    Ok(ProfileReq::Catalogue(profile_spec(p)?))
}

// ---- frames and paths ----

fn unit3(d: &ddc_ops::Dir3) -> Result<[f64; 3], Fail> {
    let n = (d.x * d.x + d.y * d.y + d.z * d.z).sqrt();
    if !(n.is_finite() && n > EPS) {
        return Err(bad_value("frame direction is zero or not finite"));
    }
    Ok([d.x / n, d.y / n, d.z / n])
}

fn near(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= 1e-6)
}

fn frame_of(f: &Frame, elev: f64) -> Result<GFrame, Fail> {
    let (x, z) = (unit3(&f.x)?, unit3(&f.z)?);
    if (x[0] * z[0] + x[1] * z[1] + x[2] * z[2]).abs() > 1e-6 {
        return Err(bad_value("frame x and z are not perpendicular"));
    }
    let y = [z[1] * x[2] - z[2] * x[1], z[2] * x[0] - z[0] * x[2], z[0] * x[1] - z[1] * x[0]];
    let o = f.origin.to_m();
    Ok(GFrame {
        o: v3(o[0], o[1], o[2] - elev),
        x: v3(x[0], x[1], x[2]),
        y: v3(y[0], y[1], y[2]),
        z: v3(z[0], z[1], z[2]),
    })
}

fn path_req(m: &Model, run: &Run, p: &Path, level: Option<i64>) -> Result<PathReq, Fail> {
    match p {
        Path::Polyline { pts, bulges, closed } => {
            let elev = elevation(m, need_level(level, "a point path")?)?;
            Ok(PathReq::Points {
                pts: pts.iter().map(|p| p.to_m()).map(|a| [a[0], a[1], a[2] - elev]).collect(),
                bulges: bulges.clone(),
                closed: *closed,
            })
        }
        Path::Ref { of } => {
            let (host, part) = match of {
                Ref::Part { of: h, part } => (h.as_ref(), Some(part.as_str())),
                r => (r, None),
            };
            let (cat, id) = resolve(m, run, host)?;
            match (cat, part) {
                (Cat::Wall, None | Some("axis")) => Ok(PathReq::WallAxis { wall: id, z: 0.0 }),
                (Cat::Slab, None | Some("boundary")) => {
                    Ok(PathReq::SlabEdge { slab: id, ring: 0, z: 0.0 })
                }
                (Cat::Slab, Some(h)) if h.starts_with("hole:") => {
                    let k: usize = h[5..]
                        .parse()
                        .map_err(|_| bad_ref(format!("{}: hole index is not a number", of.label())))?;
                    Ok(PathReq::SlabEdge { slab: id, ring: k + 1, z: 0.0 })
                }
                _ => Err(unsupported(format!(
                    "path {} is not lowered (wall axis, slab boundary or slab hole:k)",
                    of.label()
                ))),
            }
        }
    }
}

// ---- masses, booleans, voids ----

/// Mass extrusion of a form; only vertical extrusions of a profile without holes map exactly.
fn mass_spec(m: &Model, level: i64, form: &Form, name: &str, void: bool) -> Result<MassSpec, Fail> {
    let Form::Extrude { profile, frame, start, end } = form else {
        return Err(unsupported("tool and void forms other than extrude are not lowered yet"));
    };
    let sec = section(profile)?;
    if !sec.holes.is_empty() {
        return Err(unsupported("a mass extrusion needs a profile without holes"));
    }
    let f = frame_of(frame, elevation(m, level)?)?;
    if !near([f.z.x, f.z.y, f.z.z], [0.0, 0.0, 1.0]) {
        return Err(unsupported("a mass extrusion needs frame z pointing up"));
    }
    let h = end.to_m() - start.to_m();
    if !(h.is_finite() && h > EPS) {
        return Err(bad_value("extrude end must be above start"));
    }
    let ring = gsolid::tess(&sec.outer, SAG)
        .iter()
        .map(|p| (f.o.x + p.x * f.x.x + p.y * f.y.x, f.o.y + p.x * f.x.y + p.y * f.y.y))
        .collect();
    Ok(MassSpec {
        level,
        name: name.into(),
        void,
        inplace: true,
        form: crate::Form::Extrude { ring, z0: f.o.z + start.to_m(), h, ids: vec![] },
    })
}

fn host_level(m: &Model, table: &str, id: i64) -> Result<i64, Fail> {
    Ok(m.db.query_row(&format!("SELECT level FROM {table} WHERE id=?1"), [id], |r| r.get(0))?)
}

/// Linked result `a op b`: a new mass; both operands stay and drive it. New rows without a slot
/// (result, tool form) are listed in `changed`.
fn boolean(m: &mut Model, run: &mut Run, kind: BoolKind, a: &Ref, b: &Tool, keep_tool: bool) -> Result<(), Fail> {
    if !keep_tool {
        return Err(unsupported(
            "geometry.boolean: a linked boolean keeps both operands; set keep_tool to true",
        ));
    }
    let a_id = resolve_as(m, run, a, Cat::Mass)?;
    let b_id = match b {
        Tool::Ref { of } => resolve_as(m, run, of, Cat::Mass)?,
        Tool::Form { form } => {
            let level = host_level(m, "mass", a_id)?;
            let spec = mass_spec(m, level, form, "Tool", false)?;
            let id = m.add_mass(&spec)?;
            run.receipt.changed.push(Ref::id(Cat::Mass, id));
            run.receipt.copied.push(Ref::id(Cat::Mass, id));
            id
        }
    };
    let op = match kind {
        BoolKind::Cut => BoolOp::Subtract,
        BoolKind::Union => BoolOp::Union,
        BoolKind::Intersect => BoolOp::Intersect,
    };
    let id = m.apply_boolean(&BoolRequest { op, a: a_id, b: b_id, name: None })?;
    run.receipt.changed.push(Ref::id(Cat::Mass, id));
    run.receipt.copied.push(Ref::id(Cat::Mass, id));
    Ok(())
}

fn void(m: &mut Model, run: &mut Run, host: &Ref, form: &Form) -> Result<(), Fail> {
    let (cat, id) = resolve(m, run, host)?;
    let t = table(cat).filter(|t| VOID_HOSTS.contains(t)).ok_or_else(|| {
        bad_ref(format!("{} cannot host a void (wall, slab, roof, column, beam, mass)", host.label()))
    })?;
    let level = host_level(m, t, id)?;
    let spec = mass_spec(m, level, form, "Void", true)?;
    let req = VoidRequest { host: (t.into(), id), level, form: spec.form, name: "Void".into() };
    let v = m.apply_void(&req)?;
    run.receipt.changed.push(Ref::id(cat, id));
    run.receipt.changed.push(Ref::id(Cat::Mass, v));
    run.receipt.copied.push(Ref::id(Cat::Mass, v));
    Ok(())
}

// ---- conversion ----

fn cat_of_table(t: &str) -> Option<Cat> {
    Some(match t {
        "mesh" => Cat::Mesh,
        "solid" => Cat::Solid,
        "wall" => Cat::Wall,
        "slab" => Cat::Slab,
        "roof" => Cat::Roof,
        "col" => Cat::Column,
        "beam" => Cat::Beam,
        "mass" => Cat::Mass,
        "freeform" => Cat::Freeform,
        _ => return None,
    })
}

/// One conversion; its loss report goes to `Receipt.loss` as `what: measure` lines.
fn convert(m: &mut Model, run: &mut Run, target: &Ref, to: OpConvertTo) -> Result<(), Fail> {
    let (cat, id) = resolve(m, run, target)?;
    if cat == Cat::Freeform {
        return super::ops_freeform::to_mesh(m, run, id, to);
    }
    let t = match cat {
        Cat::Mesh => "mesh",
        Cat::Solid => "solid",
        _ => return Err(bad_ref(format!("{} is not a mesh or a solid", target.label()))),
    };
    let to = match to {
        OpConvertTo::Solid => ConvertTo::Solid,
        OpConvertTo::Mesh => ConvertTo::Mesh,
        OpConvertTo::Parametric => ConvertTo::Parametric,
        OpConvertTo::Copies => ConvertTo::Copies,
        OpConvertTo::Path => ConvertTo::Path,
    };
    let (rep, new_id) = m.convert_kind(&ConvertRequest::new(t, id, to))?;
    if let Some(c) = cat_of_table(&rep.result_table) {
        run.receipt.changed.push(Ref::id(c, new_id));
    }
    if !row_exists(&m.db, t, id)? {
        run.receipt.deleted.push(Ref::id(cat, id));
    }
    run.receipt.loss.extend(rep.loss.iter().map(|l| format!("{}: {}", l.what, l.measure)));
    Ok(())
}
