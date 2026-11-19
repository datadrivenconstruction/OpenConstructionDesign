// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lowering of `geometry.member` and `geometry.plate` onto real structural rows: a member is
//! a beam or brace of a catalogue section (found among the member types or added from the
//! catalogue, as for gridshells); a plate is a slab with holes when flat, or a wall with one
//! void opening row per opening when upright. Walls are centred on the outline, slabs hang
//! from it (top on the outline).

use ddc_ops::{plate_kind, Cat, GeoOp, MemberRole, PlateKind, Ref, P3};
use rusqlite::OptionalExtension;

use super::{bad_value, level_row, resolve_as, unsupported, Fail, Run};
use crate::{
    BeamJoin, BeamSpec, BraceSpec, Just, LayerSpec, Model, OpeningSpec, SlabSpec, Swing,
    WallSpec,
};

/// Shortest member built, m (as script parts).
const MIN_MEMBER: f64 = 0.005;
/// Distance of an opening point from the plate plane still taken as on it, m.
const ON_PLANE: f64 = ddc_ops::geo::PLATE_FLAT;
/// Smallest opening side, m.
const MIN_OPENING: f64 = 0.001;
const DEFAULT_MATERIAL: &str = "Steel";

pub(super) fn lower(m: &mut Model, run: &mut Run, g: &GeoOp) -> Result<(), Fail> {
    match g {
        GeoOp::Member { slot, level, start, end, section, role, rotation } => {
            let level = need_level(m, run, level.as_ref(), "geometry.member")?;
            let elev = level_row(m, level)?.1;
            let (a, b) = (start.to_m(), end.to_m());
            if !a.iter().chain(&b).all(|v| v.is_finite()) {
                return Err(bad_value("member ends must be finite").at("/start"));
            }
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
            if len < MIN_MEMBER {
                return Err(bad_value(format!("member is {:.1} mm long; 5 mm at least", len * 1000.0)).at("/end"));
            }
            let rotation = rotation.map_or(0.0, |r| r.to_rad());
            if !rotation.is_finite() {
                return Err(bad_value("rotation must be finite").at("/rotation"));
            }
            if *role == MemberRole::GridshellBar {
                return Err(unsupported(
                    "a gridshell bar belongs to a gridshell: use geometry.gridshell, or role brace for a free bar",
                )
                .at("/role"));
            }
            let member_type =
                m.member_type_for_section(section.trim()).map_err(|e| Fail::from(e).at("/section"))?;
            let (pa, pb) = ((a[0], a[1], a[2] - elev), (b[0], b[1], b[2] - elev));
            if *role == MemberRole::Beam {
                let id = m.add_beam(&BeamSpec {
                    level,
                    member_type,
                    a: pa,
                    b: pb,
                    bulge: 0.0,
                    yjust: Just::Center,
                    zjust: Just::Center,
                    rotation,
                    start_ext: 0.0,
                    end_ext: 0.0,
                    // the line is the member as drawn: no trimming at other members
                    join_start: BeamJoin::None,
                    join_end: BeamJoin::None,
                })?;
                run.made(*slot, Cat::Beam, id);
            } else {
                let id = m.add_brace(&BraceSpec {
                    level,
                    member_type,
                    a: pa,
                    b: pb,
                    start: None,
                    end: None,
                    rotation,
                })?;
                run.made(*slot, Cat::Brace, id);
            }
            Ok(())
        }
        GeoOp::Plate { slot, level, outline, thickness, openings, material } => {
            let level = need_level(m, run, level.as_ref(), "geometry.plate")?;
            let elev = level_row(m, level)?.1;
            let t = thickness.to_m();
            if !(t.is_finite() && t > 0.0) {
                return Err(bad_value("plate thickness must be positive").at("/thickness"));
            }
            let kind = plate_kind(outline).map_err(|e| bad_value(e).at("/outline"))?;
            let mat = material.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_MATERIAL);
            let tname = format!("Plate {} mm {mat}", fmt_mm(t));
            for (k, o) in openings.iter().enumerate() {
                if o.len() < 3 || o.iter().any(|p| !p.to_m().iter().all(|v| v.is_finite())) {
                    return Err(bad_value("an opening needs 3 or more finite points").at(format!("/openings/{k}")));
                }
            }
            match kind {
                PlateKind::Slab => slab(m, run, *slot, level, elev, outline, openings, t, mat, &tname),
                PlateKind::Wall => wall(m, run, *slot, level, elev, outline, openings, t, mat, &tname),
            }
        }
        _ => Err(unsupported("not a member or plate op")),
    }
}

fn need_level(m: &Model, run: &Run, r: Option<&Ref>, what: &str) -> Result<i64, Fail> {
    match r {
        Some(r) => resolve_as(m, run, r, Cat::Level).map_err(|e| e.at("/level")),
        None => Err(bad_value(format!("{what} needs `level`"))),
    }
}

fn fmt_mm(t: f64) -> String {
    let mm = (t * 1000.0 * 10.0).round() / 10.0;
    if mm.fract() == 0.0 {
        format!("{}", mm as i64)
    } else {
        format!("{mm}")
    }
}

/// The layered type of that name, else a new one-layer type.
fn type_named(m: &mut Model, table: &str, name: &str, mat: &str, t: f64) -> Result<i64, Fail> {
    let found: Option<i64> = m
        .db
        .query_row(&format!("SELECT id FROM {table} WHERE name=?1"), [name], |r| r.get(0))
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    let layers = [LayerSpec { material: mat.to_string(), thickness: t }];
    Ok(if table == "wall_type" { m.add_wall_type(name, &layers)? } else { m.add_slab_type(name, &layers)? })
}

#[allow(clippy::too_many_arguments)]
fn slab(
    m: &mut Model,
    run: &mut Run,
    slot: u32,
    level: i64,
    elev: f64,
    outline: &[P3],
    openings: &[Vec<P3>],
    t: f64,
    mat: &str,
    tname: &str,
) -> Result<(), Fail> {
    let plan = |r: &[P3]| -> Vec<(f64, f64)> {
        r.iter()
            .map(|p| {
                let [x, y, _] = p.to_m();
                (x, y)
            })
            .collect()
    };
    let top = outline[0].to_m()[2];
    for (k, o) in openings.iter().enumerate() {
        if o.iter().any(|p| (p.to_m()[2] - top).abs() > ON_PLANE) {
            return Err(bad_value("opening is not in the plate plane").at(format!("/openings/{k}")));
        }
    }
    let slab_type = type_named(m, "slab_type", tname, mat, t)?;
    let id = m.add_slab(&SlabSpec {
        level,
        slab_type,
        offset: top - elev,
        outer: plan(outline),
        holes: openings.iter().map(|o| plan(o)).collect(),
    })?;
    run.made(slot, Cat::Slab, id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn wall(
    m: &mut Model,
    run: &mut Run,
    slot: u32,
    level: i64,
    elev: f64,
    outline: &[P3],
    openings: &[Vec<P3>],
    t: f64,
    mat: &str,
    tname: &str,
) -> Result<(), Fail> {
    let p0 = outline[0].to_m();
    // horizontal direction in the plate: the first edge with a run in plan
    let x = outline
        .iter()
        .map(|p| p.to_m())
        .map(|q| (q[0] - p0[0], q[1] - p0[1]))
        .map(|(dx, dy)| (dx, dy, dx.hypot(dy)))
        .find(|d| d.2 > 1e-6)
        .map(|(dx, dy, l)| (dx / l, dy / l))
        .ok_or_else(|| bad_value("upright plate has no width").at("/outline"))?;
    let n = (-x.1, x.0);
    let local = |p: &P3| {
        let q = p.to_m();
        let (dx, dy) = (q[0] - p0[0], q[1] - p0[1]);
        (dx * x.0 + dy * x.1, q[2], dx * n.0 + dy * n.1)
    };
    let pts: Vec<(f64, f64, f64)> = outline.iter().map(local).collect();
    let (s0, s1, z0, z1) = bounds(&pts);
    // a wall is a rectangle in its plane: the outline must fill its bounding box
    let area = shoelace(&pts).abs();
    let rect = (s1 - s0) * (z1 - z0);
    if !(rect > 0.0) || (rect - area).abs() > 0.01 * rect {
        return Err(bad_value("an upright plate must be a rectangle (it becomes a wall)").at("/outline"));
    }
    let mut specs = Vec::with_capacity(openings.len());
    for (k, o) in openings.iter().enumerate() {
        let at = || format!("/openings/{k}");
        let q: Vec<(f64, f64, f64)> = o.iter().map(local).collect();
        if q.iter().any(|p| p.2.abs() > ON_PLANE) {
            return Err(bad_value("opening is not in the plate plane").at(at()));
        }
        let (a0, a1, b0, b1) = bounds(&q);
        const EPS: f64 = 1e-6;
        if a1 - a0 < MIN_OPENING || b1 - b0 < MIN_OPENING {
            return Err(bad_value("opening has no width or height").at(at()));
        }
        if a0 < s0 - EPS || a1 > s1 + EPS || b0 < z0 - EPS || b1 > z1 + EPS {
            return Err(bad_value("opening reaches outside the plate").at(at()));
        }
        specs.push((
            OpeningSpec {
                along: (a0 - s0).max(0.0),
                width: a1 - a0,
                sill: (b0 - z0).max(0.0),
                height: b1 - b0,
                kind: "void".into(),
            },
            Swing::default(),
        ));
    }
    let wall_type = type_named(m, "wall_type", tname, mat, t)?;
    let id = m.add_wall(&WallSpec {
        a: (p0[0] + x.0 * s0, p0[1] + x.1 * s0),
        b: (p0[0] + x.0 * s1, p0[1] + x.1 * s1),
        level,
        base_offset: z0 - elev,
        height: z1 - z0,
        wall_type,
        offset0: None,
    })?;
    run.made(slot, Cat::Wall, id);
    let rows: Vec<_> = specs.into_iter().map(|(o, sw)| (id, o, sw)).collect();
    m.add_openings_with(&rows)?;
    Ok(())
}

fn bounds(p: &[(f64, f64, f64)]) -> (f64, f64, f64, f64) {
    p.iter().fold((f64::MAX, f64::MIN, f64::MAX, f64::MIN), |b, q| {
        (b.0.min(q.0), b.1.max(q.0), b.2.min(q.1), b.3.max(q.1))
    })
}

fn shoelace(p: &[(f64, f64, f64)]) -> f64 {
    (0..p.len())
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ops::{Actor, Envelope, Group, Len, Op};

    fn env(ops: Vec<Op>) -> Envelope {
        let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "rows");
        e.groups.push(Group { id: 1, title: "g".into(), ops, depends_on: vec![], why: String::new() });
        e
    }

    fn lv() -> Option<Ref> {
        Some(Ref::name(Cat::Level, "L1"))
    }

    fn member(slot: u32, a: P3, b: P3, role: MemberRole) -> Op {
        Op::Geometry(GeoOp::Member {
            slot,
            level: lv(),
            start: a,
            end: b,
            section: "CHS 88.9x5".into(),
            role,
            rotation: None,
        })
    }

    fn count(m: &Model, table: &str) -> i64 {
        m.db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn members_become_beam_and_brace_rows_from_the_level() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 2.0).unwrap();
        let ops = vec![
            member(0, P3::m(0.0, 0.0, 5.0), P3::m(6.0, 0.0, 5.0), MemberRole::Beam),
            member(1, P3::m(0.0, 0.0, 2.0), P3::m(6.0, 0.0, 5.0), MemberRole::Brace),
            member(2, P3::m(6.0, 0.0, 2.0), P3::m(0.0, 0.0, 5.0), MemberRole::Brace),
        ];
        let r = m.apply_envelope(&env(ops)).unwrap();
        let cats: Vec<Cat> = r.created.iter().map(|(_, r)| match r {
            Ref::Id { cat, .. } => *cat,
            other => panic!("{other:?}"),
        }).collect();
        assert_eq!(cats, vec![Cat::Beam, Cat::Brace, Cat::Brace]);
        assert_eq!((count(&m, "beam"), count(&m, "brace")), (1, 2));
        let az: f64 = m.db.query_row("SELECT az FROM beam", [], |r| r.get(0)).unwrap();
        assert!((az - 3.0).abs() < 1e-9, "z is from the level: {az}");
        // one section type, reused by all three
        assert_eq!(m.member_types().unwrap().len(), 1);
        m.undo().unwrap();
        assert_eq!((count(&m, "beam"), count(&m, "brace")), (0, 0));
    }

    #[test]
    fn free_gridshell_bars_unknown_sections_and_short_members_are_refused() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let bar = member(0, P3::m(0.0, 0.0, 0.0), P3::m(1.0, 0.0, 0.0), MemberRole::GridshellBar);
        assert!(format!("{:?}", m.apply_envelope(&env(vec![bar])).unwrap_err()).contains("gridshell"));
        let short = member(0, P3::m(0.0, 0.0, 0.0), P3::m(0.001, 0.0, 0.0), MemberRole::Brace);
        assert!(m.apply_envelope(&env(vec![short])).is_err());
        let mut bad = member(0, P3::m(0.0, 0.0, 0.0), P3::m(1.0, 0.0, 0.0), MemberRole::Beam);
        if let Op::Geometry(GeoOp::Member { section, .. }) = &mut bad {
            *section = "no such section".into();
        }
        assert!(format!("{:?}", m.apply_envelope(&env(vec![bad])).unwrap_err()).contains("section"));
        assert_eq!((count(&m, "beam"), count(&m, "brace")), (0, 0));
    }

    fn square(x: f64, z: f64, d: f64) -> Vec<P3> {
        vec![P3::m(x, 1.0, z), P3::m(x + d, 1.0, z), P3::m(x + d / 2.0, 1.0, z + d)]
    }

    #[test]
    fn an_upright_plate_is_a_wall_with_one_opening_row_per_opening() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let outline = vec![P3::m(0.0, 1.0, 0.5), P3::m(12.0, 1.0, 0.5), P3::m(12.0, 1.0, 4.5), P3::m(0.0, 1.0, 4.5)];
        let mut openings = vec![];
        for i in 0..30 {
            for j in 0..5 {
                openings.push(square(0.2 + i as f64 * 0.39, 0.7 + j as f64 * 0.75, 0.3));
            }
        }
        let plate = Op::Geometry(GeoOp::Plate {
            slot: 0,
            level: lv(),
            outline,
            thickness: Len::mm(8.0),
            openings,
            material: Some("S355".into()),
        });
        let r = m.apply_envelope(&env(vec![plate.clone()])).unwrap();
        assert!(matches!(r.created[0].1, Ref::Id { cat: Cat::Wall, .. }));
        assert_eq!(count(&m, "opening"), 150);
        let (bo, h): (f64, f64) = m.db.query_row("SELECT base_offset, height FROM wall", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!((bo - 0.5).abs() < 1e-9 && (h - 4.0).abs() < 1e-9);
        let (along, sill, w): (f64, f64, f64) = m.db.query_row("SELECT along, sill, width FROM opening ORDER BY id LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert!((along - 0.2).abs() < 1e-9 && (sill - 0.2).abs() < 1e-9 && (w - 0.3).abs() < 1e-9);
        // a second plate reuses the plate type
        m.apply_envelope(&env(vec![plate])).unwrap();
        assert_eq!(count(&m, "wall_type"), 1);
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!((count(&m, "wall"), count(&m, "opening")), (0, 0));
    }

    #[test]
    fn a_flat_plate_is_a_slab_with_holes_and_bad_plates_are_refused() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 3.0).unwrap();
        let flat = vec![P3::m(0.0, 0.0, 4.0), P3::m(5.0, 0.0, 4.0), P3::m(5.0, 4.0, 4.0), P3::m(0.0, 4.0, 4.0)];
        let hole = vec![P3::m(1.0, 1.0, 4.0), P3::m(2.0, 1.0, 4.0), P3::m(2.0, 2.0, 4.0)];
        let plate = |outline: Vec<P3>, openings: Vec<Vec<P3>>| {
            Op::Geometry(GeoOp::Plate { slot: 0, level: lv(), outline, thickness: Len::mm(20.0), openings, material: None })
        };
        let r = m.apply_envelope(&env(vec![plate(flat.clone(), vec![hole])])).unwrap();
        let Ref::Id { cat: Cat::Slab, id } = r.created[0].1 else { panic!("{:?}", r.created) };
        let rings: i64 = m.db.query_row("SELECT count(DISTINCT ring) FROM slab_vertex WHERE slab=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(rings, 2);
        let off: f64 = m.db.query_row("SELECT offset FROM slab WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert!((off - 1.0).abs() < 1e-9);
        // tilted, an L-shaped upright plate, an opening off the plate
        let tilted = vec![P3::m(0.0, 0.0, 0.0), P3::m(4.0, 0.0, 0.0), P3::m(4.0, 2.0, 1.0)];
        assert!(m.apply_envelope(&env(vec![plate(tilted, vec![])])).is_err());
        let ell = vec![
            P3::m(0.0, 0.0, 0.0), P3::m(4.0, 0.0, 0.0), P3::m(4.0, 0.0, 1.0),
            P3::m(1.0, 0.0, 1.0), P3::m(1.0, 0.0, 3.0), P3::m(0.0, 0.0, 3.0),
        ];
        assert!(format!("{:?}", m.apply_envelope(&env(vec![plate(ell, vec![])])).unwrap_err()).contains("rectangle"));
        let up = vec![P3::m(0.0, 0.0, 0.0), P3::m(4.0, 0.0, 0.0), P3::m(4.0, 0.0, 3.0), P3::m(0.0, 0.0, 3.0)];
        let out = vec![P3::m(3.5, 0.0, 1.0), P3::m(4.5, 0.0, 1.0), P3::m(4.0, 0.0, 2.0)];
        assert!(m.apply_envelope(&env(vec![plate(up, vec![out])])).is_err());
        assert_eq!((count(&m, "slab"), count(&m, "wall"), count(&m, "opening")), (1, 0, 0));
    }
}
