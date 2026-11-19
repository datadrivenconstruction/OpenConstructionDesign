// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lowering of `geometry.gridshell` onto `gridshell.rs`: the host is drawn in absolute
//! coordinates and stored from its level; member sections are catalogue names, found among
//! the member types or added from the catalogue in the same envelope.

use ddc_geom::{v3, V3};
use ddc_ops::{Cat, GeoOp, LatticeHost, LatticeKind, LatticePattern, LatticeSupports, P3};

use super::{bad_value, level_row, resolve_as, unsupported, Fail, Run};
use crate::{GridForm, GridKind, GridPattern, GridShellSpec, GridSupports, Model};

fn pt(p: &P3, elev: f64) -> V3 {
    let [x, y, z] = p.to_m();
    v3(x, y, z - elev)
}

/// The host as level coordinates.
pub(crate) fn form_of(h: &LatticeHost, elev: f64) -> GridForm {
    match h {
        LatticeHost::Dome { center, radius, rise, oculus } => GridForm::Dome {
            center: pt(center, elev),
            radius: radius.to_m(),
            rise: rise.to_m(),
            oculus: oculus.map_or(0.0, |o| o.to_m()),
        },
        LatticeHost::Vault { a, b, span, rise } => GridForm::Vault {
            a: pt(a, elev),
            b: pt(b, elev),
            span: span.to_m(),
            rise: rise.to_m(),
        },
        LatticeHost::Hypar { center, lx, ly, rise, angle } => GridForm::Hypar {
            center: pt(center, elev),
            lx: lx.to_m(),
            ly: ly.to_m(),
            rise: rise.to_m(),
            angle: angle.map_or(0.0, |a| a.to_rad()),
        },
        LatticeHost::Plane { corner, w, h, angle } => GridForm::Plane {
            corner: pt(corner, elev),
            w: w.to_m(),
            h: h.to_m(),
            angle: angle.map_or(0.0, |a| a.to_rad()),
        },
        LatticeHost::Paraboloid { vertex, radius, depth, hub, tilt, azimuth } => GridForm::Paraboloid {
            vertex: pt(vertex, elev),
            radius: radius.to_m(),
            depth: depth.to_m(),
            hub: hub.map_or(0.0, |o| o.to_m()),
            tilt: tilt.map_or(0.0, |a| a.to_rad()),
            azimuth: azimuth.map_or(0.0, |a| a.to_rad()),
        },
    }
}

pub(crate) fn kind_of(k: LatticeKind) -> GridKind {
    match k {
        LatticeKind::Diagrid => GridKind::Diagrid,
        LatticeKind::Gridshell => GridKind::Gridshell,
        LatticeKind::CableNet => GridKind::CableNet,
    }
}

/// Pattern a kind takes when none is asked: diagonals for a diagrid, two lath or cable
/// families otherwise.
pub(crate) fn pattern_of(p: Option<LatticePattern>, k: GridKind) -> GridPattern {
    match p {
        Some(LatticePattern::Diagrid) => GridPattern::Diagrid,
        Some(LatticePattern::Triangular) => GridPattern::Triangular,
        Some(LatticePattern::QuadBraced) => GridPattern::QuadBraced,
        Some(LatticePattern::Quad) => GridPattern::Quad,
        None if k == GridKind::Diagrid => GridPattern::Diagrid,
        None => GridPattern::Quad,
    }
}

fn supports_of(s: LatticeSupports) -> GridSupports {
    match s {
        LatticeSupports::Base => GridSupports::Base,
        LatticeSupports::Edges => GridSupports::Edges,
        LatticeSupports::Corners => GridSupports::Corners,
    }
}

pub(super) fn lower(m: &mut Model, run: &mut Run, g: &GeoOp) -> Result<(), Fail> {
    let GeoOp::Gridshell {
        slot,
        level,
        host,
        kind,
        pattern,
        count_u,
        count_v,
        supports,
        member,
        edge,
        node,
    } = g
    else {
        return Err(unsupported("not a gridshell op"));
    };
    let level = match level {
        Some(r) => resolve_as(m, run, r, Cat::Level).map_err(|e| e.at("/level"))?,
        None => return Err(bad_value("geometry.gridshell needs `level`")),
    };
    let form = form_of(host, level_row(m, level)?.1);
    let host = form.host().map_err(|e| bad_value(e.to_string()).at("/host"))?;
    let kind = kind_of(kind.unwrap_or(LatticeKind::Diagrid));
    let node_d = node.map_or(0.0, |d| d.to_m());
    if !(node_d.is_finite() && node_d >= 0.0) {
        return Err(bad_value("node diameter must be zero or positive").at("/node"));
    }
    let member_type = m.member_type_for_section(member.trim()).map_err(|e| Fail::from(e).at("/member"))?;
    let edge_type = match edge {
        Some(e) => Some(m.member_type_for_section(e.trim()).map_err(|x| Fail::from(x).at("/edge"))?),
        None => None,
    };
    let spec = GridShellSpec {
        level,
        kind,
        host,
        pattern: pattern_of(*pattern, kind),
        nu: *count_u as usize,
        nv: *count_v as usize,
        supports: supports.map_or(form.default_supports(), supports_of),
        member_type,
        edge_type,
        node_d,
    };
    let id = m.add_gridshell(&spec)?;
    run.made(*slot, Cat::Gridshell, id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ops::{Actor, Envelope, Group, Len, Op, Ref, Targets};

    fn env(ops: Vec<Op>) -> Envelope {
        let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "grid");
        e.groups.push(Group { id: 1, title: "g".into(), ops, depends_on: vec![], why: String::new() });
        e
    }

    fn dome(member: &str) -> Op {
        Op::Geometry(GeoOp::Gridshell {
            slot: 0,
            level: Some(Ref::name(Cat::Level, "L2")),
            host: LatticeHost::Dome {
                center: P3::m(10.0, 5.0, 4.0),
                radius: Len::m(12.0),
                rise: Len::m(5.0),
                oculus: Some(Len::m(1.5)),
            },
            kind: Some(LatticeKind::Diagrid),
            pattern: None,
            count_u: 16,
            count_v: 4,
            supports: None,
            member: member.into(),
            edge: Some("CHS 168.3x8".into()),
            node: None,
        })
    }

    #[test]
    fn op_builds_a_grid_on_the_level_and_undoes_in_one_step() {
        let mut m = Model::open(":memory:").unwrap();
        let l2 = m.add_level("L2", 4.0).unwrap();
        let r = m.apply_envelope(&env(vec![dome("CHS 88.9x5")])).unwrap();
        let Ref::Id { cat: Cat::Gridshell, id } = r.created[0].1 else { panic!("{:?}", r.created) };
        let s = m.gridshell(id).unwrap();
        assert_eq!((s.level, s.kind, s.pattern, s.supports), (l2, GridKind::Diagrid, GridPattern::Diagrid, GridSupports::Base));
        assert!(s.edge_type.is_some());
        // drawn at z 4 on a level at 4 m: the springing ring sits on the level
        let nodes = m.gridshell_nodes(id).unwrap();
        let low = nodes.iter().map(|n| n.p.z).fold(f64::MAX, f64::min);
        let high = nodes.iter().map(|n| n.p.z).fold(f64::MIN, f64::max);
        assert!(low.abs() < 1e-6 && high > 4.0 && high < 5.0 + 1e-6, "{low} {high}");
        assert!(nodes.iter().filter(|n| n.fixed).count() >= 8);
        // the member types came from the catalogue once; a second grid reuses them
        let types = m.member_types().unwrap().len();
        m.apply_envelope(&env(vec![dome("CHS 88.9x5")])).unwrap();
        assert_eq!(m.member_types().unwrap().len(), types);
        assert_eq!(m.gridshells(None).unwrap().len(), 2);
        m.undo().unwrap();
        assert_eq!(m.gridshells(None).unwrap(), vec![id]);
        m.apply_envelope(&env(vec![Op::Delete { target: Targets::one(Ref::id(Cat::Gridshell, id)) }])).unwrap();
        assert!(m.gridshells(None).unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.gridshells(None).unwrap(), vec![id]);
    }

    /// G39: an 18 m dish 4.5 m deep, its axis tilted 30 degrees, as a quad gridshell: every
    /// node lies on the paraboloid, the grid stands on its hub ring and reaches the rim.
    #[test]
    fn a_dish_op_builds_a_tilted_paraboloid_grid_on_its_hub_ring() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L2", 4.0).unwrap();
        let op = Op::Geometry(GeoOp::Gridshell {
            slot: 0,
            level: Some(Ref::name(Cat::Level, "L2")),
            host: LatticeHost::Paraboloid {
                vertex: P3::m(0.0, 0.0, 6.0),
                radius: Len::m(9.0),
                depth: Len::m(4.5),
                hub: Some(Len::m(1.5)),
                tilt: Some(ddc_ops::Angle::deg(30.0)),
                azimuth: None,
            },
            kind: Some(LatticeKind::Gridshell),
            pattern: None,
            count_u: 24,
            count_v: 6,
            supports: None,
            member: "CHS 88.9x5".into(),
            edge: None,
            node: None,
        });
        let r = m.apply_envelope(&env(vec![op])).unwrap();
        let Ref::Id { cat: Cat::Gridshell, id } = r.created[0].1 else { panic!("{:?}", r.created) };
        let s = m.gridshell(id).unwrap();
        assert_eq!((s.pattern, s.supports), (GridPattern::Quad, GridSupports::Base));
        let nodes = m.gridshell_nodes(id).unwrap();
        assert_eq!(nodes.len(), 24 * 7);
        // vertex at 6 m on a level at 4 m; focal length r^2 / (4 depth) = 4.5 m
        let (o, z, f) = (v3(0.0, 0.0, 2.0), v3(0.5, 0.0, 0.75f64.sqrt()), 4.5);
        let rho = |p: V3| {
            let q = p - o;
            let h = q.dot(z);
            ((q.dot(q) - h * h).max(0.0).sqrt(), h)
        };
        for n in &nodes {
            let (r, h) = rho(n.p);
            assert!((r * r / (4.0 * f) - h).abs() < 1e-6, "off the paraboloid: {:?}", n.p);
            assert!(r > 1.5 - 1e-6 && r < 9.0 + 1e-6, "{r}");
        }
        let fixed: Vec<f64> = nodes.iter().filter(|n| n.fixed).map(|n| rho(n.p).0).collect();
        assert_eq!(fixed.len(), 24);
        assert!(fixed.iter().all(|r| (r - 1.5).abs() < 1e-6), "{fixed:?}");
        assert_eq!(nodes.iter().filter(|n| (rho(n.p).0 - 9.0).abs() < 1e-6).count(), 24);
        // rings and radials of a closed quad grid
        assert!(m.gridshell_bars(id).unwrap().len() >= 24 * 6 + 24 * 6, "too few members");
    }

    #[test]
    fn refuses_unknown_sections_and_impossible_hosts() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L2", 4.0).unwrap();
        let e = m.apply_envelope(&env(vec![dome("no such section")])).unwrap_err();
        assert!(format!("{e:?}").contains("member"), "{e:?}");
        let mut op = dome("CHS 88.9x5");
        if let Op::Geometry(GeoOp::Gridshell { host: LatticeHost::Dome { rise, .. }, .. }) = &mut op {
            *rise = Len::m(13.0);
        }
        let e = m.apply_envelope(&env(vec![op])).unwrap_err();
        assert!(format!("{e:?}").contains("rise"), "{e:?}");
        assert!(m.gridshells(None).unwrap().is_empty());
        assert!(m.member_types().unwrap().is_empty());
    }
}
