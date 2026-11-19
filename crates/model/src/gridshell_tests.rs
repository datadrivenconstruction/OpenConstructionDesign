// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::TAU;

use ddc_geom::host::{Axes, Host};
use ddc_geom::member::Shape;
use ddc_geom::v3;

use super::*;
use crate::Model;

fn setup() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l0 = m.add_level("L0", 0.0).unwrap();
    let t = m
        .add_member_type("CHS 88.9x5", &Shape::Pipe { d: 0.0889, t: 0.005 }, "S355")
        .unwrap();
    (m, l0, t)
}

fn dome() -> Host {
    Host::Sphere {
        ax: Axes::world(),
        r: 15.0,
        lon0: 0.0,
        lon1: TAU,
        lat0: 0.1,
        lat1: 1.1,
    }
}

fn spec(level: i64, t: i64, kind: GridKind, host: Host, pattern: GridPattern) -> GridShellSpec {
    GridShellSpec {
        level,
        kind,
        host,
        pattern,
        nu: 16,
        nv: 4,
        supports: GridSupports::Base,
        member_type: t,
        edge_type: None,
        node_d: 0.0,
    }
}

fn bodies(m: &Model, id: i64) -> Vec<crate::StructBody> {
    let mut b: Vec<_> = m
        .struct_part_bodies(None)
        .unwrap()
        .into_iter()
        .filter(|b| b.kind == "gridshell" && b.id == id)
        .collect();
    b.sort_by_key(|b| b.part);
    b
}

fn chord(m: &Model, id: i64, a: u32, b: u32) -> f64 {
    let n = m.gridshell_nodes(id).unwrap();
    (n[b as usize].p - n[a as usize].p).len()
}

#[test]
fn diagrid_dome_has_rolled_members_between_ball_nodes() {
    let (mut m, l0, t) = setup();
    let id = m
        .add_gridshell(&spec(l0, t, GridKind::Diagrid, dome(), GridPattern::Diagrid))
        .unwrap();
    let nodes = m.gridshell_nodes(id).unwrap();
    let bars = m.gridshell_bars(id).unwrap();
    // closed diagrid: half the columns per row, grid bars nu per band, a ring at both edges
    assert_eq!(nodes.len(), 8 * 5);
    assert_eq!(bars.len(), 16 * 4 + 2 * 8);
    for n in &nodes {
        assert!(((n.p.len()) - 15.0).abs() < 1e-9, "node off the sphere");
        assert!((n.n - n.p * (1.0 / 15.0)).len() < 1e-12, "normal not radial");
    }
    assert_eq!(nodes.iter().filter(|n| n.fixed).count(), 8);
    let parts = m.struct_parts("gridshell", id).unwrap();
    assert_eq!(parts.len(), bars.len() + nodes.len());
    let mut members = 0;
    for (k, (role, len, vol)) in parts.iter().enumerate() {
        if k < bars.len() {
            let (a, b, _, _) = bars[k];
            let c = chord(&m, id, a, b);
            // the ball nodes set both ends back, never by more than the member is long
            assert!(*len < c - 1e-3 && *len > 0.8 * c, "member {k}: {len} of {c}");
            assert!(role == "member" || role == "edge", "{role}");
            members += 1;
        } else {
            assert_eq!(role, "node");
            assert!(*vol > 0.0, "node {k} has no solid");
        }
    }
    assert_eq!(members, bars.len());
    let rep = m.gridshell_report(id).unwrap();
    assert_eq!((rep.nodes, rep.bars, rep.supports), (40, 80, 8));
    assert!(rep.clashes.is_empty(), "{:?}", rep.clashes);
    // web of a straight member bisects its end normals: equal opposite turn at both ends
    assert!(rep.max_twist < 0.2, "twist {}", rep.max_twist);
    let sum: f64 = bars.iter().map(|&(a, b, _, _)| chord(&m, id, a, b)).sum();
    assert!((rep.length - sum).abs() < 1e-9);
}

#[test]
fn moving_one_node_moves_only_its_members_in_one_undo() {
    let (mut m, l0, t) = setup();
    let host = Host::Plane {
        ax: Axes::world(),
        w: 12.0,
        h: 8.0,
    };
    let mut s = spec(l0, t, GridKind::Diagrid, host, GridPattern::Quad);
    s.nu = 6;
    s.nv = 4;
    s.supports = GridSupports::Edges;
    s.node_d = 0.3;
    let id = m.add_gridshell(&s).unwrap();
    let nodes = m.gridshell_nodes(id).unwrap();
    let bars = m.gridshell_bars(id).unwrap();
    let before = bodies(&m, id);
    assert_eq!(before.len(), bars.len() + nodes.len());
    let i = nodes
        .iter()
        .position(|n| (n.p - v3(6.0, 4.0, 0.0)).len() < 1e-9)
        .unwrap();
    let incident: Vec<usize> = bars
        .iter()
        .enumerate()
        .filter(|(_, b)| b.0 as usize == i || b.1 as usize == i)
        .map(|(k, _)| k)
        .collect();
    assert_eq!(incident.len(), 4);
    m.move_gridshell_node(id, i, v3(6.0, 4.0, 0.6), false).unwrap();
    assert_eq!(m.gridshell_nodes(id).unwrap()[i].p, v3(6.0, 4.0, 0.6));
    let after = bodies(&m, id);
    for k in 0..bars.len() {
        let same = before[k].mesh.positions == after[k].mesh.positions;
        assert_eq!(same, !incident.contains(&k), "member {k}");
    }
    let nb = bars.len();
    assert_ne!(before[nb + i].mesh.positions, after[nb + i].mesh.positions);
    for j in 0..nodes.len() {
        let near = bars.iter().any(|b| {
            (b.0 as usize == i && b.1 as usize == j) || (b.1 as usize == i && b.0 as usize == j)
        });
        if j != i && !near {
            assert_eq!(before[nb + j].mesh.positions, after[nb + j].mesh.positions, "node {j}");
        }
    }
    // the incident members still run to the moved node, set back at both balls
    for &k in &incident {
        let o = if bars[k].0 as usize == i { bars[k].1 } else { bars[k].0 };
        let c = chord(&m, id, i as u32, o);
        assert!(after[k].length < c && after[k].length > 0.5 * c, "{} of {c}", after[k].length);
    }
    // snapping puts the node back on the host
    m.move_gridshell_node(id, i, v3(6.0, 4.0, 0.6), true).unwrap();
    assert!(m.gridshell_nodes(id).unwrap()[i].p.z.abs() < 1e-12);
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(m.gridshell_nodes(id).unwrap(), nodes);
    let back = bodies(&m, id);
    for (a, b) in before.iter().zip(&back) {
        assert_eq!(a.mesh.positions, b.mesh.positions);
    }
    assert!(m.move_gridshell_node(id, nodes.len(), v3(0.0, 0.0, 0.0), false).is_err());
}

#[test]
fn cable_net_hangs_by_force_density_and_undoes() {
    let (mut m, l0, t) = setup();
    let host = Host::Plane {
        ax: Axes::world(),
        w: 8.0,
        h: 8.0,
    };
    let mut s = spec(l0, t, GridKind::CableNet, host, GridPattern::Quad);
    s.nu = 8;
    s.nv = 8;
    s.supports = GridSupports::Edges;
    let id = m.add_gridshell(&s).unwrap();
    let nodes = m.gridshell_nodes(id).unwrap();
    let r = m
        .form_find_gridshell(
            id,
            &FormFind::Density {
                q: 2.0,
                edge_q: 2.0,
                load: -1.0,
            },
        )
        .unwrap();
    assert!(r.residual < 1e-9, "{}", r.residual);
    assert!(r.min_force > 0.0, "a cable in compression");
    let hung = m.gridshell_nodes(id).unwrap();
    for (a, b) in nodes.iter().zip(&hung) {
        if a.fixed {
            assert_eq!(a.p, b.p);
        } else {
            assert!(b.p.z < 0.0);
            assert!((b.p.x - a.p.x).abs() < 1e-9 && (b.p.y - a.p.y).abs() < 1e-9);
        }
        assert!((b.n.len() - 1.0).abs() < 1e-9 && b.n.z > 0.5);
    }
    // centre is the lowest point, the net is symmetric
    let low = hung.iter().map(|n| n.p.z).fold(f64::INFINITY, f64::min);
    let c = hung
        .iter()
        .find(|n| (n.p.x - 4.0).abs() < 1e-9 && (n.p.y - 4.0).abs() < 1e-9)
        .unwrap();
    assert_eq!(c.p.z, low);
    assert!((r.moved + low).abs() < 1e-12);
    for a in &hung {
        let mirror = hung
            .iter()
            .find(|b| (b.p.x - (8.0 - a.p.x)).abs() < 1e-9 && (b.p.y - a.p.y).abs() < 1e-9)
            .unwrap();
        assert!((a.p.z - mirror.p.z).abs() < 1e-9);
    }
    let roles = m.struct_parts("gridshell", id).unwrap();
    assert!(roles.iter().any(|p| p.0 == "cable") && roles.iter().any(|p| p.0 == "node"));
    m.undo().unwrap();
    assert_eq!(m.gridshell_nodes(id).unwrap(), nodes);
}

#[test]
fn gridshell_laths_follow_their_curves() {
    let (mut m, l0, t) = setup();
    let id = m
        .add_gridshell(&spec(l0, t, GridKind::Gridshell, dome(), GridPattern::Diagrid))
        .unwrap();
    let bars = m.gridshell_bars(id).unwrap();
    let parts = m.struct_parts("gridshell", id).unwrap();
    let mut laths = 0;
    for (k, &(a, b, role, _)) in bars.iter().enumerate() {
        let (r, len, _) = &parts[k];
        let c = chord(&m, id, a, b);
        if role == "grid" {
            assert_eq!(r, "lath");
            // a curve through the node line is longer than its chord, by little
            assert!(*len > c && *len < 1.02 * c, "lath {k}: {len} of {c}");
            laths += 1;
        }
    }
    assert_eq!(laths, 16 * 4);
    let rep = m.gridshell_report(id).unwrap();
    assert!(rep.clashes.is_empty(), "{:?}", rep.clashes);
}

#[test]
fn bending_form_finding_keeps_supports_and_balances() {
    let (mut m, l0, t) = setup();
    let host = Host::Plane {
        ax: Axes::world(),
        w: 6.0,
        h: 6.0,
    };
    let mut s = spec(l0, t, GridKind::Gridshell, host, GridPattern::Quad);
    s.nu = 6;
    s.nv = 6;
    s.supports = GridSupports::Edges;
    let id = m.add_gridshell(&s).unwrap();
    let nodes = m.gridshell_nodes(id).unwrap();
    let r = m
        .form_find_gridshell(
            id,
            &FormFind::Bending {
                ea: 1e5,
                ei: 50.0,
                load: 10.0,
            },
        )
        .unwrap();
    assert!(r.steps > 0 && r.residual <= 10.0 * 1e-6, "{r:?}");
    let up = m.gridshell_nodes(id).unwrap();
    for (a, b) in nodes.iter().zip(&up) {
        if a.fixed {
            assert_eq!(a.p, b.p);
        } else {
            assert!(b.p.z > 0.0, "an upward load lifts every free node");
        }
    }
}

#[test]
fn bad_grids_are_refused_and_delete_takes_the_tables() {
    let (mut m, l0, t) = setup();
    let mut s = spec(l0, t, GridKind::Diagrid, dome(), GridPattern::Diagrid);
    s.nu = 15;
    assert!(m.add_gridshell(&s).unwrap_err().to_string().contains("even"));
    s.nu = 16;
    s.member_type = 999;
    assert!(m.add_gridshell(&s).is_err());
    s.member_type = t;
    s.node_d = -1.0;
    assert!(m.add_gridshell(&s).is_err());
    s.node_d = 0.0;
    let id = m.add_gridshell(&s).unwrap();
    assert_eq!(m.gridshell(id).unwrap(), s);
    m.delete_struct("gridshell", id).unwrap();
    assert!(m.gridshell_nodes(id).unwrap().is_empty());
    assert!(m.gridshell_bars(id).unwrap().is_empty());
    assert!(m.struct_parts("gridshell", id).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.gridshell_nodes(id).unwrap().len(), 40);
    assert_eq!(m.gridshell_bars(id).unwrap().len(), 80);
    assert_eq!(m.struct_parts("gridshell", id).unwrap().len(), 120);
}
