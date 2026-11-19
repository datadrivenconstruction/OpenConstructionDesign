// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Move/rotate of the 3D selection using existing model operations. Order: move/rotate in
//! plan via the shared edit operation (`move/copy/rotate_elements`), then vertical and structural
//! elements via one batch edit (`apply_edits`), then the rest via their own `update_*`.
//! Returns the number of operations (one editor undo step reverts them); on error
//! the operations already done are rolled back.

use ddc_model::{Edit, Error, Model, RoofKind};
use ddc_render::glam::Vec3;

use super::grips::{ceiling_spec, elev, opening_row, wall_spec};
use super::math::{f3, rotate3};
use super::pick::{Cat, Target};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Xform {
    Move(Vec3),
    /// Rotation by `angle` (rad, counterclockwise when looking from the axis end) about an axis through `pivot`.
    Rotate {
        pivot: Vec3,
        axis: Vec3,
        angle: f32,
    },
    /// Mirror in the vertical plane through the plan line `a`→`b` (heights are ignored).
    Mirror { a: Vec3, b: Vec3 },
    /// Linear array: `count` placements in all, original included; copy `k` sits at `k * step`.
    Array { step: Vec3, count: u32 },
}

impl Xform {
    pub fn is_identity(&self) -> bool {
        match *self {
            Xform::Move(d) => d.length_squared() < 1e-12,
            Xform::Rotate { angle, .. } => angle.abs() < 1e-7,
            Xform::Mirror { a, b } => (b - a).truncate().length_squared() < 1e-12,
            Xform::Array { step, .. } => step.length_squared() < 1e-12,
        }
    }

    /// Image of a scene point (for preview).
    pub fn point(&self, p: Vec3) -> Vec3 {
        match *self {
            Xform::Move(d) => p + d,
            Xform::Rotate { pivot, axis, angle } => {
                super::math::v3(rotate3(f3(p), f3(pivot), f3(axis), angle as f64))
            }
            Xform::Mirror { a, b } => {
                let d = (b - a).truncate().normalize_or_zero();
                let r = (p - a).truncate();
                let along = d * r.dot(d);
                let m = a.truncate() + along + (along - r);
                Vec3::new(m.x, m.y, p.z)
            }
            // the preview of an array is its first copy
            Xform::Array { step, .. } => p + step,
        }
    }

    fn vertical(&self) -> Option<f64> {
        match *self {
            Xform::Rotate { axis, angle, .. } if axis.normalize_or_zero().z.abs() > 0.999 => {
                Some(angle as f64 * axis.z.signum() as f64)
            }
            _ => None,
        }
    }
}

fn bad(s: &str) -> Error {
    Error::Invalid(s.into())
}

fn rot2(p: (f64, f64), c: (f64, f64), a: f64) -> (f64, f64) {
    let (s, k) = a.sin_cos();
    let (x, y) = (p.0 - c.0, p.1 - c.1);
    (c.0 + x * k - y * s, c.1 + x * s + y * k)
}

/// Our target from a model edit target (for copies).
pub fn from_modify(t: ddc_model::Target) -> Option<Target> {
    use ddc_model::Target as T;
    Some(match t {
        T::Wall(i) => Target::new(Cat::Wall, i),
        T::Opening(i) => Target::new(Cat::Opening, i),
        T::Curtain(i) => Target::new(Cat::Curtain, i),
        T::Slab(i) => Target::new(Cat::Slab, i),
        T::Room(i) => Target::new(Cat::Room, i),
        T::Component(i) => Target::new(Cat::Comp, i),
        T::Solid(i) => Target::new(Cat::Ext(crate::scene_ids::EXT_SOLID), i),
        T::Mass(i) => Target::new(Cat::Ext(crate::scene_ids::EXT_MASS), i),
        T::Topo(i) => Target::new(Cat::Ext(crate::scene_ids::EXT_TOPO), i),
        T::Roof(i) => Target::new(Cat::Roof, i),
        _ => return None,
    })
}

/// Transform targets; `copy` copies (supported for plan edits, columns, beams,
/// roofs, ceilings). Ok(n) means n model operations were done.
/// Nothing done is an error (`e3d.err.noop` or `e3d.err.unsupported`): a tool record
/// with no changes does not become an undo step.
pub fn apply(m: &mut Model, z0: f64, ts: &[Target], x: &Xform, copy: bool) -> Result<usize, Error> {
    // rotating a member would diverge from the group definition; forbidden as in plan rotations
    if matches!(x, Xform::Rotate { .. }) && !copy {
        let items: Vec<(String, i64)> = ts
            .iter()
            .filter_map(|t| crate::clip_tools::pick_item(*t))
            .collect();
        let refs: Vec<(&str, i64)> = items.iter().map(|i| (i.0.as_str(), i.1)).collect();
        m.reject_group_turn(&refs)?;
    }
    // mass history does not store rotation: refuse with an explanation, the model is untouched
    if matches!(x, Xform::Rotate { .. })
        && ts.iter().any(|t| t.cat == Cat::Ext(crate::scene_ids::EXT_MASS))
    {
        return Err(bad("elem.no.mass_turn"));
    }
    // rollback goes to this journal mark, not by a count of ops: a miscount must never undo
    // an edit made before this one
    let mark = m.op_top()?;
    let mut n = 0;
    match run(m, z0, ts, x, copy, &mut n) {
        Ok(()) if n == 0 => {
            let none = ts.iter().all(|t| !supported(t.cat));
            Err(bad(if none && !x.is_identity() {
                "e3d.err.unsupported"
            } else {
                "e3d.err.noop"
            }))
        }
        Ok(()) => Ok(n),
        Err(e) => match m.rollback_to(mark) {
            Ok(_) => Err(e),
            Err(u) => Err(Error::Invalid(format!("{e}; rollback failed: {u}"))),
        },
    }
}

/// Categories the gizmo moves (a panel with its curtain wall, external ones via their own provider).
pub fn supported(c: Cat) -> bool {
    match c {
        Cat::Panel => false,
        Cat::Ext(code) => {
            super::pick::native_ext(code) || super::pick::ext_plan_target(code, 0).is_some()
        }
        _ => true,
    }
}

/// Foundation and framing families (and ramps) live in the plan: they take part in horizontal
/// moves, turns, mirrors and arrays through the shared model operations, but have no height of
/// their own to shift. Solids, masses and terrains lift, so they are not plan-only.
fn plan_only(t: &Target) -> bool {
    matches!(t.cat, Cat::Ext(c) if !super::pick::native_ext(c)) && t.modify().is_some()
}

/// Mirror and array reuse the plan operations, so every target must have one.
fn all_plan(ts: &[Target]) -> Result<Vec<ddc_model::Target>, Error> {
    let plan: Vec<ddc_model::Target> = ts.iter().filter_map(|t| t.modify()).collect();
    if plan.len() != ts.len() {
        return Err(bad("e3d.err.unsupported"));
    }
    Ok(plan)
}

/// Columns and beams move here through edits, not the plan operation, so the model cannot see
/// them as hosts: a footing or brace whose hosts are all selected is left to follow them.
fn rides_member(m: &Model, ts: &[Target], t: ddc_model::Target) -> Result<bool, Error> {
    use ddc_model::{MemberHost, Target as T};
    let has = |c: Cat, id: i64| ts.contains(&Target::new(c, id));
    Ok(match t {
        T::Footing(id) => m.footing(id)?.column.is_some_and(|c| has(Cat::Column, c)),
        T::Brace(id) => {
            let b = m.brace(id)?;
            let ends: Vec<_> = [b.start, b.end].into_iter().flatten().collect();
            !ends.is_empty()
                && ends.iter().all(|a| match a.host {
                    MemberHost::Column => has(Cat::Column, a.id),
                    MemberHost::Beam => has(Cat::Beam, a.id),
                })
        }
        _ => false,
    })
}

/// Applies member edits. Copied columns bring their footings: each footing is re-hosted on the
/// new column (the plan copy cannot, because columns are not plan targets here).
fn apply_member_edits(m: &mut Model, ts: &[Target], edits: &[Edit], copy: bool) -> Result<(), Error> {
    let cols: Vec<i64> = ts.iter().filter(|t| t.cat == Cat::Column).map(|t| t.id).collect();
    if !copy || cols.is_empty() {
        return Ok(m.apply_edits(edits)?);
    }
    let top = |m: &Model| -> Result<i64, Error> {
        Ok(m.sql().query_row("SELECT COALESCE(MAX(id),0) FROM col", [], |r| r.get(0))?)
    };
    let before = top(m)?;
    m.apply_edits(edits)?;
    let new: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM col WHERE id>?1 ORDER BY id")?;
        let v = st.query_map([before], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
        v
    };
    if new.len() != cols.len() {
        return Err(bad("e3d.err.copy"));
    }
    let mut specs = vec![];
    for (old, fresh) in cols.iter().zip(new) {
        for f in m.footings_of_column(*old)? {
            let mut sp = m.footing(f)?;
            sp.column = Some(fresh);
            specs.push(sp);
        }
    }
    if !specs.is_empty() {
        m.add_footings(&specs)?;
    }
    Ok(())
}

/// Kinds of `ts` the transform leaves in place, without repeats.
pub fn skipped(ts: &[Target]) -> Vec<Cat> {
    let mut out: Vec<Cat> = vec![];
    for t in ts.iter().filter(|t| !supported(t.cat)) {
        if !out.contains(&t.cat) {
            out.push(t.cat);
        }
    }
    out
}

/// Status text for a transform that touched only part of the selection, as the plan tools say it.
pub fn partial_note(ts: &[Target], x: &Xform, copy: bool) -> Option<String> {
    let cats = skipped(ts);
    if cats.is_empty() {
        return None;
    }
    let mut names: Vec<String> = vec![];
    for c in cats {
        let n = crate::tr(&format!("e3d.cat.{}", c.key()), &[]);
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let op = match (x, copy) {
        (Xform::Rotate { .. }, _) => "rotate",
        (Xform::Move(_), true) => "copy",
        (Xform::Move(_), false) => "move",
        (Xform::Mirror { .. }, _) => "mirror",
        (Xform::Array { .. }, _) => "array",
    };
    let kinds = names.join(", ");
    Some(crate::tr("mt.partial", &[("op", op), ("kinds", kinds.as_str())]))
}

/// A lone pipe moves through the network follow-up: an end on a device connector refuses
/// with a reason instead of coming loose. Several targets move together, so they keep the
/// plain write (their joins move with them).
fn write_run(m: &mut Model, ts: &[Target], id: i64, r: &ddc_model::RunSpec) -> Result<(), Error> {
    if ts.len() == 1 {
        m.mep_edit_run(id, r)
    } else {
        m.update_mep_run(id, r)
    }
}

fn run(
    m: &mut Model,
    z0: f64,
    ts: &[Target],
    x: &Xform,
    copy: bool,
    n: &mut usize,
) -> Result<(), Error> {
    if x.is_identity() || ts.is_empty() {
        return Ok(());
    }
    let mut plan_ts: Vec<ddc_model::Target> = vec![];
    for t in ts.iter().filter_map(|t| t.modify()) {
        if !rides_member(m, ts, t)? {
            plan_ts.push(t);
        }
    }
    let mut edits: Vec<Edit> = vec![];
    match *x {
        Xform::Move(d) => {
            let (dx, dy, dz) = (d.x as f64, d.y as f64, d.z as f64);
            let flat = dx.abs() + dy.abs() > 1e-9;
            let lift = dz.abs() > 1e-9;
            if lift && ts.iter().any(plan_only) {
                return Err(bad("e3d.err.plan_only"));
            }
            // targets the vertical applies to: the originals or their copies
            let mut zs: Vec<Target> = ts.to_vec();
            // runs move first: a device moved afterwards finds no run at its old connector
            if !copy {
                for t in ts.iter().filter(|t| t.cat == Cat::Mep) {
                    let mut r = m.mep_run(t.id)?;
                    r.points
                        .iter_mut()
                        .for_each(|q| *q = (q.0 + dx, q.1 + dy, q.2 + dz));
                    write_run(m, ts, t.id, &r)?;
                    *n += 1;
                }
            }
            // solids, masses and terrains lift together in one model operation
            let mut lifted: Vec<ddc_model::Target> = vec![];
            // a roof goes through the plan path, so skylights and soffit walls follow it
            let mut mv = plan_ts.clone();
            mv.extend(
                ts.iter()
                    .filter(|t| t.cat == Cat::Roof)
                    .map(|t| ddc_model::Target::Roof(t.id)),
            );
            if !mv.is_empty() && (flat || copy) {
                if copy {
                    let new = m.copy_elements(&mv, &[(dx, dy)])?;
                    *n += 1;
                    zs.retain(|t| t.modify().is_none() && t.cat != Cat::Roof);
                    zs.extend(new.into_iter().filter_map(from_modify));
                } else {
                    m.move_elements(&mv, (dx, dy))?;
                    *n += 1;
                }
            }
            for t in &zs {
                let id = t.id;
                match t.cat {
                    Cat::Ext(e) if lift && super::pick::native_ext(e) => {
                        lifted.extend(t.modify());
                    }
                    Cat::Wall if lift => {
                        let mut spec = wall_spec(m, id)?;
                        spec.base_offset += dz;
                        edits.push(Edit::Wall {
                            id,
                            spec,
                            bulge: m.wall_bulge(id)?,
                        });
                    }
                    Cat::Opening if lift => {
                        let (wall, mut spec) = opening_row(m, id)?;
                        if !zs.contains(&Target::new(Cat::Wall, wall)) {
                            spec.sill += dz;
                            edits.push(Edit::Opening { id, wall, spec });
                        }
                    }
                    Cat::Slab if lift => {
                        let (level, slab_type, offset): (i64, i64, f64) = m.sql().query_row(
                            "SELECT level, stype, offset FROM slab WHERE id=?1",
                            [id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )?;
                        edits.push(Edit::Slab {
                            id,
                            level,
                            slab_type,
                            offset: offset + dz,
                        });
                    }
                    Cat::Curtain if lift => {
                        let (level, name, base): (i64, String, f64) = m.sql().query_row(
                            "SELECT level, name, base_offset FROM curtain_wall WHERE id=?1",
                            [id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )?;
                        edits.push(Edit::CurtainProps {
                            id,
                            level,
                            name,
                            base_offset: base + dz,
                        });
                    }
                    Cat::Column => {
                        let mut c = m.column(id)?;
                        let sh = |p: (f64, f64)| (p.0 + dx, p.1 + dy);
                        c.at = sh(c.at);
                        c.top_at = c.top_at.map(sh);
                        c.base_offset += dz;
                        c.top_offset += dz;
                        edits.push(if copy {
                            Edit::NewColumn(c)
                        } else {
                            Edit::Column { id, spec: c }
                        });
                    }
                    Cat::Beam => {
                        let mut b = m.beam(id)?;
                        let sh = |p: (f64, f64, f64)| (p.0 + dx, p.1 + dy, p.2 + dz);
                        b.a = sh(b.a);
                        b.b = sh(b.b);
                        edits.push(if copy {
                            Edit::NewBeam(b)
                        } else {
                            Edit::Beam { id, spec: b }
                        });
                    }
                    _ => {}
                }
            }
            if !edits.is_empty() {
                apply_member_edits(m, ts, &edits, copy)?;
                *n += 1;
            }
            if !lifted.is_empty() {
                m.lift_elements(&lifted, dz)?;
                *n += 1;
            }
            let sh = |p: (f64, f64)| (p.0 + dx, p.1 + dy);
            for t in &zs {
                let id = t.id;
                match t.cat {
                    Cat::Roof if lift => {
                        let mut r = m.roof(id)?;
                        r.offset += dz;
                        m.update_roof(id, &r)?;
                        *n += 1;
                    }
                    Cat::Ceiling => {
                        let mut c = ceiling_spec(m, id)?;
                        c.outer.iter_mut().for_each(|p| *p = sh(*p));
                        c.holes.iter_mut().flatten().for_each(|p| *p = sh(*p));
                        c.height += dz;
                        if copy {
                            m.add_ceiling_copy(id, &c)?;
                        } else {
                            m.update_ceiling(id, &c)?;
                        }
                        *n += 1;
                    }
                    Cat::Stair if !copy => {
                        let mut s = m.stair(id)?;
                        s.origin = sh(s.origin);
                        s.base_offset += dz;
                        s.top_offset += dz;
                        m.update_stair(id, &s)?;
                        *n += 1;
                    }
                    Cat::Railing if !copy => {
                        let p = m.railing_path(id)?;
                        // a stair's railing follows it
                        if !p.is_empty() {
                            let p: Vec<_> =
                                p.iter().map(|q| (q.0 + dx, q.1 + dy, q.2 + dz)).collect();
                            m.set_railing_path(id, &p)?;
                            *n += 1;
                        }
                    }
                    Cat::Comp if lift => {
                        let mut s = m.component(id)?;
                        s.at.2 += dz;
                        m.mep_update_device(id, &s)?;
                        *n += 1;
                    }
                    Cat::Mep if copy => {
                        let mut r = m.mep_run(id)?;
                        r.points
                            .iter_mut()
                            .for_each(|q| *q = (q.0 + dx, q.1 + dy, q.2 + dz));
                        if copy {
                            m.add_mep_run(&r)?;
                        } else {
                            m.update_mep_run(id, &r)?;
                        }
                        *n += 1;
                    }
                    Cat::Mesh if !copy => {
                        m.move_mesh(id, (dx, dy, dz))?;
                        *n += 1;
                    }
                    Cat::Stair | Cat::Railing | Cat::Mesh => return Err(bad("e3d.err.copy")),
                    _ => {}
                }
            }
        }
        Xform::Mirror { a, b } => {
            let plan = all_plan(ts)?;
            m.mirror_elements(&plan, (a.x as f64, a.y as f64), (b.x as f64, b.y as f64), copy)?;
            *n += 1;
        }
        Xform::Array { step, count } => {
            if count < 2 {
                return Err(bad("e3d.err.array_count"));
            }
            if step.z.abs() > 1e-6 && ts.iter().any(plan_only) {
                return Err(bad("e3d.err.plan_only"));
            }
            let plan = all_plan(ts)?;
            let vs: Vec<(f64, f64)> = (1..count)
                .map(|k| (step.x as f64 * k as f64, step.y as f64 * k as f64))
                .collect();
            m.copy_elements(&plan, &vs)?;
            *n += 1;
        }
        Xform::Rotate { pivot, axis, angle } => {
            let Some(a) = x.vertical() else {
                return tilt(m, z0, ts, pivot, axis, angle as f64, copy, n);
            };
            let c = (pivot.x as f64, pivot.y as f64);
            let r2 = |p: (f64, f64)| rot2(p, c, a);
            if !copy {
                for t in ts.iter().filter(|t| t.cat == Cat::Mep) {
                    let mut r = m.mep_run(t.id)?;
                    r.points.iter_mut().for_each(|q| {
                        let p = r2((q.0, q.1));
                        *q = (p.0, p.1, q.2);
                    });
                    write_run(m, ts, t.id, &r)?;
                    *n += 1;
                }
            }
            if !plan_ts.is_empty() {
                m.rotate_elements(&plan_ts, c, a, copy)?;
                *n += 1;
            }
            for t in ts {
                let id = t.id;
                match t.cat {
                    Cat::Column => {
                        let mut s = m.column(id)?;
                        s.at = r2(s.at);
                        s.top_at = s.top_at.map(r2);
                        s.rotation += a;
                        edits.push(if copy {
                            Edit::NewColumn(s)
                        } else {
                            Edit::Column { id, spec: s }
                        });
                    }
                    Cat::Beam => {
                        let mut s = m.beam(id)?;
                        let (pa, pb) = (r2((s.a.0, s.a.1)), r2((s.b.0, s.b.1)));
                        s.a = (pa.0, pa.1, s.a.2);
                        s.b = (pb.0, pb.1, s.b.2);
                        edits.push(if copy {
                            Edit::NewBeam(s)
                        } else {
                            Edit::Beam { id, spec: s }
                        });
                    }
                    _ => {}
                }
            }
            if !edits.is_empty() {
                apply_member_edits(m, ts, &edits, copy)?;
                *n += 1;
            }
            for t in ts {
                let id = t.id;
                match t.cat {
                    Cat::Roof => {
                        let mut r = m.roof(id)?;
                        match &mut r.kind {
                            RoofKind::Footprint { ring, .. } => {
                                ring.iter_mut().for_each(|p| *p = r2(*p))
                            }
                            RoofKind::Extrusion { a: pa, b: pb, .. } => {
                                *pa = r2(*pa);
                                *pb = r2(*pb);
                            }
                            RoofKind::Arrow {
                                ring, tail, head, ..
                            } => {
                                ring.iter_mut().for_each(|p| *p = r2(*p));
                                *tail = r2(*tail);
                                *head = r2(*head);
                            }
                        }
                        if copy {
                            m.add_roof(&r)?;
                        } else {
                            m.update_roof(id, &r)?;
                        }
                        *n += 1;
                    }
                    Cat::Ceiling => {
                        let mut s = ceiling_spec(m, id)?;
                        s.outer.iter_mut().for_each(|p| *p = r2(*p));
                        s.holes.iter_mut().flatten().for_each(|p| *p = r2(*p));
                        if copy {
                            m.add_ceiling_copy(id, &s)?;
                        } else {
                            m.update_ceiling(id, &s)?;
                        }
                        *n += 1;
                    }
                    Cat::Stair if !copy => {
                        let mut s = m.stair(id)?;
                        s.origin = r2(s.origin);
                        s.angle += a;
                        m.update_stair(id, &s)?;
                        *n += 1;
                    }
                    Cat::Railing if !copy => {
                        let p = m.railing_path(id)?;
                        if !p.is_empty() {
                            let p: Vec<_> = p
                                .iter()
                                .map(|q| {
                                    let r = r2((q.0, q.1));
                                    (r.0, r.1, q.2)
                                })
                                .collect();
                            m.set_railing_path(id, &p)?;
                            *n += 1;
                        }
                    }
                    Cat::Mep if copy => {
                        let mut r = m.mep_run(id)?;
                        r.points.iter_mut().for_each(|q| {
                            let p = r2((q.0, q.1));
                            *q = (p.0, p.1, q.2);
                        });
                        if copy {
                            m.add_mep_run(&r)?;
                        } else {
                            m.update_mep_run(id, &r)?;
                        }
                        *n += 1;
                    }
                    Cat::Stair | Cat::Railing => return Err(bad("e3d.err.copy")),
                    Cat::Mesh => return Err(bad("e3d.err.unsupported")),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Rotation about a non-vertical axis: only beams (ends in 3D) and columns (they become tilted).
#[allow(clippy::too_many_arguments)]
fn tilt(
    m: &mut Model,
    z0: f64,
    ts: &[Target],
    pivot: Vec3,
    axis: Vec3,
    angle: f64,
    copy: bool,
    n: &mut usize,
) -> Result<(), Error> {
    if ts.iter().any(|t| !matches!(t.cat, Cat::Beam | Cat::Column)) {
        return Err(bad("e3d.err.tilt"));
    }
    let (pv, ax) = (f3(pivot), f3(axis));
    let rot = |p: [f64; 3]| rotate3(p, pv, ax, angle);
    let mut edits = vec![];
    for t in ts {
        let id = t.id;
        if t.cat == Cat::Beam {
            let mut b = m.beam(id)?;
            let e = elev(m, b.level)? - z0;
            let go = |p: (f64, f64, f64)| {
                let q = rot([p.0, p.1, p.2 + e]);
                (q[0], q[1], q[2] - e)
            };
            b.a = go(b.a);
            b.b = go(b.b);
            edits.push(if copy {
                Edit::NewBeam(b)
            } else {
                Edit::Beam { id, spec: b }
            });
        } else {
            let mut c = m.column(id)?;
            let eb = elev(m, c.level)? - z0;
            let et = elev(m, c.top_level.unwrap_or(c.level))? - z0;
            let top_xy = c.top_at.unwrap_or(c.at);
            let base = rot([c.at.0, c.at.1, eb + c.base_offset]);
            let top = rot([top_xy.0, top_xy.1, et + c.top_offset]);
            if top[2] - base[2] < 0.01 {
                return Err(bad("e3d.err.upside_down"));
            }
            c.at = (base[0], base[1]);
            c.base_offset = base[2] - eb;
            c.top_at = Some((top[0], top[1]));
            c.top_offset = top[2] - et;
            edits.push(if copy {
                Edit::NewColumn(c)
            } else {
                Edit::Column { id, spec: c }
            });
        }
    }
    m.apply_edits(&edits)?;
    *n += 1;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ddc_model::{BeamJoin, BeamSpec, ColumnSpec, Just, LayerSpec, WallSpec};

    pub(crate) struct T {
        pub m: Model,
        pub lv: i64,
        pub top: i64,
        pub wt: i64,
        pub mt: i64,
    }

    pub(crate) fn base() -> T {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let top = m.add_level("L2", 3.0).unwrap();
        let layer = [LayerSpec {
            material: "c".into(),
            thickness: 0.2,
        }];
        let wt = m.add_wall_type("W", &layer).unwrap();
        let mt = m.add_catalog_member_type("R 400x400").unwrap();
        T { m, lv, top, wt, mt }
    }

    pub(crate) fn wall(t: &mut T, a: (f64, f64), b: (f64, f64)) -> i64 {
        t.m.add_wall(&WallSpec {
            a,
            b,
            level: t.lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t.wt,
            offset0: None,
        })
        .unwrap()
    }

    pub(crate) fn column(t: &mut T, at: (f64, f64)) -> i64 {
        t.m.add_column(&ColumnSpec {
            level: t.lv,
            top_level: Some(t.top),
            member_type: t.mt,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap()
    }

    pub(crate) fn beam(t: &mut T, a: (f64, f64, f64), b: (f64, f64, f64)) -> i64 {
        t.m.add_beam(&BeamSpec {
            level: t.lv,
            member_type: t.mt,
            a,
            b,
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::default(),
            join_end: BeamJoin::default(),
        })
        .unwrap()
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn move_3d_mixed_then_rollback_by_count() {
        let mut t = base();
        let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
        let c = column(&mut t, (1.0, 1.0));
        let b = beam(&mut t, (0.0, 2.0, 2.5), (4.0, 2.0, 2.5));
        let ts = [
            Target::new(Cat::Wall, w),
            Target::new(Cat::Column, c),
            Target::new(Cat::Beam, b),
        ];
        let n = apply(
            &mut t.m,
            0.0,
            &ts,
            &Xform::Move(Vec3::new(1.0, 0.5, 0.25)),
            false,
        )
        .unwrap();
        assert_eq!(n, 2, "plan move + one batch edit");
        let ws = wall_spec(&t.m, w).unwrap();
        assert!(near(ws.a.0, 1.0) && near(ws.a.1, 0.5) && near(ws.base_offset, 0.25));
        let cs = t.m.column(c).unwrap();
        assert!(near(cs.at.0, 2.0) && near(cs.base_offset, 0.25) && near(cs.top_offset, 0.25));
        let bs = t.m.beam(b).unwrap();
        assert!(near(bs.a.2, 2.75) && near(bs.b.0, 5.0));
        for _ in 0..n {
            t.m.undo().unwrap();
        }
        assert!(near(wall_spec(&t.m, w).unwrap().a.0, 0.0));
        assert!(near(t.m.column(c).unwrap().at.0, 1.0));
        assert!(near(t.m.beam(b).unwrap().a.2, 2.5));
    }

    #[test]
    fn rotate_about_z_turns_column_and_beam() {
        let mut t = base();
        let c = column(&mut t, (2.0, 0.0));
        let b = beam(&mut t, (1.0, 0.0, 3.0), (3.0, 0.0, 3.0));
        let x = Xform::Rotate {
            pivot: Vec3::ZERO,
            axis: Vec3::Z,
            angle: std::f32::consts::FRAC_PI_2,
        };
        let n = apply(
            &mut t.m,
            0.0,
            &[Target::new(Cat::Column, c), Target::new(Cat::Beam, b)],
            &x,
            false,
        )
        .unwrap();
        assert_eq!(n, 1);
        let cs = t.m.column(c).unwrap();
        assert!(cs.at.0.abs() < 1e-6 && near(cs.at.1, 2.0));
        assert!(near(cs.rotation, std::f64::consts::FRAC_PI_2));
        let bs = t.m.beam(b).unwrap();
        assert!(near(bs.b.1, 3.0) && near(bs.b.2, 3.0));
        // axis pointing down: the rotation goes the other way
        let back = Xform::Rotate {
            pivot: Vec3::ZERO,
            axis: Vec3::NEG_Z,
            angle: std::f32::consts::FRAC_PI_2,
        };
        apply(&mut t.m, 0.0, &[Target::new(Cat::Column, c)], &back, false).unwrap();
        assert!(near(t.m.column(c).unwrap().at.0, 2.0));
    }

    #[test]
    fn tilt_only_members_and_copy() {
        let mut t = base();
        let w = wall(&mut t, (0.0, 0.0), (4.0, 0.0));
        let b = beam(&mut t, (0.0, 0.0, 3.0), (4.0, 0.0, 3.0));
        let tilt = Xform::Rotate {
            pivot: Vec3::new(0.0, 0.0, 3.0),
            axis: Vec3::NEG_Y,
            angle: std::f32::consts::FRAC_PI_8,
        };
        // a wall cannot be rotated this way, and nothing is written
        let before =
            t.m.sql()
                .query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0))
                .unwrap();
        assert!(apply(
            &mut t.m,
            0.0,
            &[Target::new(Cat::Wall, w), Target::new(Cat::Beam, b)],
            &tilt,
            false
        )
        .is_err());
        let after =
            t.m.sql()
                .query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0))
                .unwrap();
        assert_eq!(before, after);
        apply(&mut t.m, 0.0, &[Target::new(Cat::Beam, b)], &tilt, false).unwrap();
        let bs = t.m.beam(b).unwrap();
        assert!(bs.b.2 > 3.5, "end lifted: {:?}", bs.b);
        // copy by move: the original stays, and there are now two beams
        apply(
            &mut t.m,
            0.0,
            &[Target::new(Cat::Beam, b)],
            &Xform::Move(Vec3::Y),
            true,
        )
        .unwrap();
        let cnt: i64 =
            t.m.sql()
                .query_row("SELECT COUNT(*) FROM beam", [], |r| r.get(0))
                .unwrap();
        assert_eq!(cnt, 2);
        assert!(near(t.m.beam(b).unwrap().a.1, 0.0));
    }

    #[test]
    fn vertical_move_lifts_mass_and_terrain_in_one_step() {
        use crate::scene_ids::{EXT_MASS, EXT_TOPO};
        let mut t = base();
        let topo = t
            .m
            .add_toposolid(&ddc_model::TopoSpec {
                name: "t".into(),
                thickness: 1.0,
                material: "earth".into(),
                points: vec![(0.0, 0.0, 1.0), (5.0, 0.0, 1.0), (5.0, 5.0, 1.0), (0.0, 5.0, 1.0)],
                ..Default::default()
            })
            .unwrap();
        let mass = t
            .m
            .add_mass(&ddc_model::MassSpec {
                level: t.lv,
                name: "m".into(),
                void: false,
                inplace: false,
                form: ddc_model::Form::Cuboid {
                    lo: (0.0, 0.0, 0.0),
                    hi: (2.0, 2.0, 2.0),
                },
            })
            .unwrap();
        let ts = [
            Target::new(Cat::Ext(EXT_MASS), mass),
            Target::new(Cat::Ext(EXT_TOPO), topo),
        ];
        let n = apply(&mut t.m, 0.0, &ts, &Xform::Move(Vec3::Z * 0.5), false).unwrap();
        assert_eq!(n, 1, "no plan move, one lift");
        assert!(t.m.topo_points(topo).unwrap().iter().all(|p| near(p.1 .2, 1.5)));
        let top = |m: &Model| m.mass_body(mass).unwrap().bbox().unwrap().1.z;
        assert!(near(top(&t.m), 2.5));
        t.m.undo().unwrap();
        assert!(t.m.topo_points(topo).unwrap().iter().all(|p| near(p.1 .2, 1.0)));
        assert!(near(top(&t.m), 2.0));
    }

    #[test]
    fn opening_follows_lifted_host_once() {
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
        let ts = [Target::new(Cat::Wall, w), Target::new(Cat::Opening, o)];
        apply(&mut t.m, 0.0, &ts, &Xform::Move(Vec3::Z * 0.5), false).unwrap();
        assert!(
            near(opening_row(&t.m, o).unwrap().1.sill, 0.9),
            "sill is relative to the wall"
        );
        apply(&mut t.m, 0.0, &ts[1..], &Xform::Move(Vec3::Z * 0.1), false).unwrap();
        assert!(near(opening_row(&t.m, o).unwrap().1.sill, 1.0));
        let p = Xform::Move(Vec3::X).point(Vec3::ZERO);
        assert_eq!(p, Vec3::X);
    }

    fn ext(kind: &str, id: i64) -> Target {
        Target::new(Cat::Ext(crate::editor::structx::code_of(kind).unwrap()), id)
    }

    fn footing(t: &mut T, column: Option<i64>, at: (f64, f64)) -> i64 {
        t.m.add_footing(&ddc_model::FootingSpec {
            level: t.lv,
            column,
            at,
            rotation: 0.0,
            round: false,
            steps: vec![ddc_model::FootingStep {
                w: 1.0,
                l: 1.0,
                h: 0.5,
            }],
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap()
    }

    fn raft(t: &mut T) -> i64 {
        t.m.add_raft(&ddc_model::RaftSpec {
            level: t.lv,
            ring: vec![(1.0, 0.0), (5.0, 0.0), (5.0, 4.0), (1.0, 4.0)],
            thickness: 0.4,
            edge_width: 0.0,
            edge_depth: 0.0,
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap()
    }

    fn rafts(t: &T) -> i64 {
        t.m.sql()
            .query_row("SELECT count(*) FROM raft", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn rotate_and_move_footing_in_3d() {
        let mut t = base();
        let f = footing(&mut t, None, (1.0, 2.0));
        let ts = [ext("footing", f)];
        let rot = Xform::Rotate {
            pivot: Vec3::ZERO,
            axis: Vec3::Z,
            angle: 0.5,
        };
        apply(&mut t.m, 0.0, &ts, &rot, false).unwrap();
        assert!(near(t.m.footing(f).unwrap().rotation, 0.5));
        apply(&mut t.m, 0.0, &ts, &Xform::Move(Vec3::new(1.0, 0.0, 0.0)), false).unwrap();
        let moved = t.m.footing(f).unwrap().at;
        let want = rot2((1.0, 2.0), (0.0, 0.0), 0.5);
        assert!(near(moved.0, want.0 + 1.0) && near(moved.1, want.1));
    }

    #[test]
    fn footing_has_no_height_to_lift() {
        let mut t = base();
        let f = footing(&mut t, None, (1.0, 2.0));
        let r = apply(&mut t.m, 0.0, &[ext("footing", f)], &Xform::Move(Vec3::Z), false);
        assert!(matches!(r, Err(Error::Invalid(k)) if k == "e3d.err.plan_only"));
        assert_eq!(t.m.footing(f).unwrap().at, (1.0, 2.0));
    }

    #[test]
    fn mirror_raft_across_a_vertical_plane() {
        let mut t = base();
        let r = raft(&mut t);
        let x = Xform::Mirror {
            a: Vec3::ZERO,
            b: Vec3::Y,
        };
        apply(&mut t.m, 0.0, &[ext("raft", r)], &x, false).unwrap();
        let ring = t.m.raft(r).unwrap().ring;
        assert!(ring.iter().all(|p| p.0 < 0.0), "{ring:?}");
        let xs: Vec<f64> = ring.iter().map(|p| p.0).collect();
        assert!(xs.iter().any(|v| near(*v, -5.0)) && xs.iter().any(|v| near(*v, -1.0)));
        // a mirror copy keeps the original
        apply(&mut t.m, 0.0, &[ext("raft", r)], &x, true).unwrap();
        assert_eq!(rafts(&t), 2);
    }

    #[test]
    fn array_raft_makes_count_minus_one_copies_in_one_op() {
        let mut t = base();
        let r = raft(&mut t);
        let x = Xform::Array {
            step: Vec3::new(0.0, 10.0, 0.0),
            count: 3,
        };
        let n = apply(&mut t.m, 0.0, &[ext("raft", r)], &x, false).unwrap();
        assert_eq!(n, 1);
        assert_eq!(rafts(&t), 3);
        t.m.undo().unwrap();
        assert_eq!(rafts(&t), 1, "one undo removes every copy");
        let one = Xform::Array {
            step: Vec3::Y,
            count: 1,
        };
        assert!(apply(&mut t.m, 0.0, &[ext("raft", r)], &one, false).is_err());
    }

    #[test]
    fn mirror_and_array_refuse_targets_without_plan_operations() {
        let mut t = base();
        let c = column(&mut t, (0.0, 0.0));
        let x = Xform::Mirror {
            a: Vec3::ZERO,
            b: Vec3::Y,
        };
        let r = apply(&mut t.m, 0.0, &[Target::new(Cat::Column, c)], &x, false);
        assert!(matches!(r, Err(Error::Invalid(k)) if k == "e3d.err.unsupported"));
    }

    #[test]
    fn copying_a_column_in_3d_brings_its_footing() {
        let mut t = base();
        let c = column(&mut t, (0.0, 0.0));
        let f = footing(&mut t, Some(c), (0.0, 0.0));
        let mv = Xform::Move(Vec3::new(2.0, 0.0, 0.0));
        apply(&mut t.m, 0.0, &[Target::new(Cat::Column, c)], &mv, true).unwrap();
        let cols: Vec<i64> = (1..=9).filter(|&i| i != c && t.m.column(i).is_ok()).collect();
        assert_eq!(cols.len(), 1);
        assert!(t.m.footings_of_column(cols[0]).unwrap().len() == 1, "copy keeps a footing");
        assert_eq!(t.m.footings_of_column(c).unwrap(), vec![f], "the original keeps its own");
    }

    #[test]
    fn hosted_footing_refuses_alone_and_rides_with_its_column() {
        let mut t = base();
        let c = column(&mut t, (0.0, 0.0));
        let f = footing(&mut t, Some(c), (0.0, 0.0));
        let mv = Xform::Move(Vec3::new(2.0, 0.0, 0.0));
        let alone = apply(&mut t.m, 0.0, &[ext("footing", f)], &mv, false);
        assert!(matches!(alone, Err(Error::Modify(_))), "{alone:?}");
        assert!(apply(&mut t.m, 0.0, &[ext("footing", f), Target::new(Cat::Column, c)], &mv, false).is_ok());
    }

    #[test]
    fn builtin_provider_owns_and_lists_grips_of_the_new_families() {
        use super::super::grips::{Builtin, Provider};
        let mut t = base();
        let f = footing(&mut t, None, (1.0, 2.0));
        let r = raft(&mut t);
        for tg in [ext("footing", f), ext("raft", r)] {
            assert!(Builtin.owns(tg));
            assert!(!Builtin.grips(&t.m, 0.0, tg).unwrap().is_empty());
        }
    }

    #[test]
    fn a_lone_pipe_on_a_device_is_not_torn_off_by_a_3d_move() {
        use ddc_model::{DeviceSpec, SystemKind};
        let mut t = base();
        let sa = t.m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
        t.m.add_mep_run(&ddc_model::RunSpec::new(
            sa,
            t.lv,
            ddc_model::MepSection::new(
                ddc_model::MepDomain::Duct,
                ddc_model::MepShape::Round { d: 0.4 },
            ),
            vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)],
        ))
        .unwrap();
        let mut place = ddc_model::CompSpec::at(t.lv, 0, 3.0, 2.0);
        place.at.2 = 2.7;
        let p = t
            .m
            .mep_place(&DeviceSpec {
                family: "Supply Diffuser".into(),
                ty: "600x600".into(),
                place,
                system: Some(sa),
                flow: None,
            })
            .unwrap();
        let stub = p.runs[0];
        let before = t.m.mep_run(stub).unwrap();
        let ts = [Target::new(Cat::Mep, stub)];
        let r = apply(&mut t.m, 0.0, &ts, &Xform::Move(Vec3::new(0.0, 0.0, 0.5)), false);
        assert!(r.is_err(), "the end on the connector must not come loose");
        assert_eq!(t.m.mep_run(stub).unwrap(), before);
    }

    #[test]
    fn roof_move_plan_eq_3d_skylight() {
        use crate::geom_props::tests::fixture;
        use ddc_model::{SkylightSpec, Target as MT};
        let sky = |m: &mut Model, roof: i64| {
            m.add_skylight(&SkylightSpec {
                roof,
                center: (33.0, 3.0),
                width: 0.8,
                length: 1.0,
                curb: 0.15,
            })
            .unwrap()
        };
        let (mut a, mut b) = (fixture(), fixture());
        let (ra, rb) = (a.ids["roof"], b.ids["roof"]);
        let (ka, kb) = (sky(&mut a.ed.model, ra), sky(&mut b.ed.model, rb));
        a.ed.model.move_elements(&[MT::Roof(ra)], (1.5, -0.5)).unwrap();
        let ts = [Target::new(Cat::Roof, rb)];
        apply(&mut b.ed.model, 0.0, &ts, &Xform::Move(Vec3::new(1.5, -0.5, 0.0)), false).unwrap();
        let (sa, sb) = (a.ed.model.skylight(ka).unwrap(), b.ed.model.skylight(kb).unwrap());
        assert_eq!(sa.center, sb.center);
        assert!((sb.center.0 - 34.5).abs() < 1e-9, "the skylight follows in 3D");
        // a pure lift keeps the plan position and shifts the roof offset
        let o = b.ed.model.roof(rb).unwrap().offset;
        apply(&mut b.ed.model, 0.0, &ts, &Xform::Move(Vec3::new(0.0, 0.0, 0.4)), false).unwrap();
        // the gizmo delta is f32: 0.4 arrives as 0.4000000059…
        assert!((b.ed.model.roof(rb).unwrap().offset - o - 0.4).abs() < 1e-6);
        assert_eq!(b.ed.model.skylight(kb).unwrap().center, sb.center);
    }
}

#[cfg(test)]
mod partial_tests {
    use super::*;

    #[test]
    fn a_wall_with_a_panel_names_the_panel_as_skipped() {
        let ts = [Target::new(Cat::Wall, 1), Target::new(Cat::Panel, 2), Target::new(Cat::Panel, 3)];
        assert_eq!(skipped(&ts), vec![Cat::Panel]);
        let note = partial_note(&ts, &Xform::Move(Vec3::X), false).unwrap();
        assert!(note.contains(&crate::tr("e3d.cat.curtain", &[])), "{note}");
        assert!(note.contains("move"), "{note}");
    }

    #[test]
    fn a_fully_moved_selection_says_nothing() {
        let ts = [Target::new(Cat::Wall, 1), Target::new(Cat::Slab, 2)];
        assert!(skipped(&ts).is_empty());
        assert!(partial_note(&ts, &Xform::Move(Vec3::X), true).is_none());
    }
}
