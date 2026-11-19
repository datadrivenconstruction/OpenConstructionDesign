// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::HashSet;
use std::f64::consts::{PI, TAU};

use super::gridshell::*;
use crate::formfind::{force_density, relax, EdgeLaw, Relax};
use crate::host::{Axes, Host, HostSurface};
use crate::math::{v3, V3};

fn dome() -> Host {
    Host::Sphere {
        ax: Axes::world(),
        r: 20.0,
        lon0: 0.0,
        lon1: TAU,
        lat0: 0.0,
        lat1: 1.1,
    }
}

fn vault() -> Host {
    // barrel vault over x, crown up: axis along x, angle from +y over +z to -y
    Host::Cylinder {
        ax: Axes::new(v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)).unwrap(),
        r: 8.0,
        a0: 0.0,
        a1: PI,
        len: 24.0,
    }
}

fn hypar() -> Host {
    Host::Hypar {
        ax: Axes::world(),
        lx: 18.0,
        ly: 18.0,
        rise: 3.0,
    }
}

/// Euler characteristic V - E + F of the net (bars as edges, cells as faces).
fn euler(g: &Grid) -> i64 {
    g.nodes.len() as i64 - g.bars.len() as i64 + g.cells.len() as i64
}

fn no_duplicate_bars(g: &Grid) {
    let mut seen = HashSet::new();
    for b in &g.bars {
        assert!(seen.insert((b.a.min(b.b), b.a.max(b.b))), "duplicate bar {b:?}");
        assert!((g.nodes[b.a as usize].p - g.nodes[b.b as usize].p).len() > 1e-6);
    }
}

fn on_host(g: &Grid, h: &dyn HostSurface) {
    for n in &g.nodes {
        assert!((h.point(n.uv.0, n.uv.1) - n.p).len() < 1e-12);
        assert!((h.normal(n.uv.0, n.uv.1) - n.n).len() < 1e-12);
        assert!((n.n.len() - 1.0).abs() < 1e-12);
    }
}

#[test]
fn quad_counts_topology_and_supports() {
    let h = hypar();
    let (nu, nv) = (6, 4);
    let g = generate(&h, Pattern::Quad, nu, nv, Supports::Edges).unwrap();
    assert_eq!(g.nodes.len(), (nu + 1) * (nv + 1));
    assert_eq!(g.bars.len(), nu * (nv + 1) + (nu + 1) * nv);
    assert_eq!(g.cells.len(), nu * nv);
    assert_eq!(euler(&g), 1, "an open patch is a disc");
    no_duplicate_bars(&g);
    on_host(&g, &h);
    let fixed = g.nodes.iter().filter(|n| n.fixed).count();
    assert_eq!(fixed, 2 * (nu + nv));
    let edges = g.bars.iter().filter(|b| b.role == BarRole::Edge).count();
    assert_eq!(edges, 2 * (nu + nv));
    let corners = generate(&h, Pattern::Quad, nu, nv, Supports::Corners).unwrap();
    assert_eq!(corners.nodes.iter().filter(|n| n.fixed).count(), 4);
    assert_eq!(corners.to_lattice("CHS 168.3x8", "CHS 219.1x10").components(), 1);
}

#[test]
fn braced_and_triangular_grids_are_triangulated() {
    let h = vault();
    for pat in [Pattern::QuadBraced, Pattern::Triangular] {
        let (nu, nv) = (8, 5);
        let g = generate(&h, pat, nu, nv, Supports::Base).unwrap();
        assert_eq!(g.bars.len(), nu * (nv + 1) + (nu + 1) * nv + nu * nv);
        assert_eq!(g.cells.len(), 2 * nu * nv);
        assert!(g.cells.iter().all(|c| c.len() == 3));
        assert_eq!(euler(&g), 1);
        no_duplicate_bars(&g);
        on_host(&g, &h);
        // every cell is closed by three bars
        let set: HashSet<(u32, u32)> = g.bars.iter().map(|b| (b.a.min(b.b), b.a.max(b.b))).collect();
        for c in &g.cells {
            for k in 0..3 {
                let (a, b) = (c[k], c[(k + 1) % 3]);
                assert!(set.contains(&(a.min(b), a.max(b))));
            }
        }
    }
}

#[test]
fn closed_diagrid_dome() {
    let h = dome();
    let (nu, nv) = (24, 8);
    let g = generate(&h, Pattern::Diagrid, nu, nv, Supports::Base).unwrap();
    // half the lattice points, two diagonals per node and row, edge rings top and bottom
    assert_eq!(g.nodes.len(), nu / 2 * (nv + 1));
    let grid = g.bars.iter().filter(|b| b.role == BarRole::Grid).count();
    assert_eq!(grid, nu * nv);
    let edges = g.bars.iter().filter(|b| b.role == BarRole::Edge).count();
    assert_eq!(edges, nu);
    assert_eq!(euler(&g), 0, "a closed band is an annulus");
    no_duplicate_bars(&g);
    on_host(&g, &h);
    assert_eq!(g.nodes.iter().filter(|n| n.fixed).count(), nu / 2);
    // helical lines: every interior node lies on two lines with two neighbours each
    let nb = g.line_nbrs();
    let adj = g.adjacency();
    for (i, n) in g.nodes.iter().enumerate() {
        let j = ((n.uv.1 / 1.1) * nv as f64).round() as usize;
        if j > 0 && j < nv {
            assert_eq!(adj[i].len(), 4, "node {i}");
            let lines: HashSet<u32> = adj[i].iter().map(|&k| g.bars[k as usize].line).collect();
            assert_eq!(lines.len(), 2);
            for l in lines {
                assert_eq!(nb[&(i as u32, l)].len(), 2);
            }
        }
    }
    assert!(generate(&h, Pattern::Diagrid, 23, nv, Supports::Base).unwrap_err().contains("even"));
}

#[test]
fn open_diagrid_on_a_vault_is_one_piece() {
    let h = vault();
    let g = generate(&h, Pattern::Diagrid, 12, 6, Supports::Edges).unwrap();
    no_duplicate_bars(&g);
    on_host(&g, &h);
    assert_eq!(euler(&g), 1);
    assert_eq!(g.to_lattice("CHS 168.3x8", "CHS 219.1x10").components(), 1);
    // boundary nodes are the supported ones
    for n in &g.nodes {
        let (u, v) = n.uv;
        let edge = u.abs() < 1e-12 || (u - PI).abs() < 1e-12 || v.abs() < 1e-12 || (v - 24.0).abs() < 1e-12;
        assert_eq!(n.fixed, edge);
    }
}

#[test]
fn node_frames_and_member_roll() {
    let h = dome();
    let g = generate(&h, Pattern::Diagrid, 16, 6, Supports::Base).unwrap();
    let adj = g.adjacency();
    for i in 0..g.nodes.len() {
        let [x, y, z] = g.node_frame(i, &adj);
        for a in [x, y, z] {
            assert!((a.len() - 1.0).abs() < 1e-12);
        }
        assert!(x.dot(y).abs() < 1e-12 && x.dot(z).abs() < 1e-12 && y.dot(z).abs() < 1e-12);
        assert!((x.cross(y) - z).len() < 1e-12);
        assert!((z - g.nodes[i].n).len() < 1e-15);
    }
    // interior diagrid node on a sphere: the two helices are mirror images about the meridian,
    // so their acute bisector runs along the meridian or along the parallel
    let i = g
        .nodes
        .iter()
        .position(|n| (n.uv.1 - 1.1 * 2.0 / 6.0).abs() < 1e-9)
        .unwrap();
    let [x, ..] = g.node_frame(i, &adj);
    let (u, v) = g.nodes[i].uv;
    let (du, dv) = h.derivs(u, v);
    let (a, b) = (x.dot(du.normalized()).abs(), x.dot(dv.normalized()).abs());
    assert!(a.min(b) < 1e-9 && (a.max(b) - 1.0).abs() < 1e-9, "bisector {a} {b}");
    for k in 0..g.bars.len() {
        let b = g.bars[k];
        let e = (g.nodes[b.b as usize].p - g.nodes[b.a as usize].p).normalized();
        let w = g.bar_up(k);
        assert!((w.len() - 1.0).abs() < 1e-12 && w.dot(e).abs() < 1e-12);
        // on a sphere both end normals lie in the plane of the member and the centre: no twist
        assert!(g.bar_twist(k) < 1e-9, "bar {k} twist {}", g.bar_twist(k));
        assert!(w.dot(g.nodes[b.a as usize].n) > 0.0);
    }
    // on a hypar the end normals differ in twist; the web takes the mean, so the two ends
    // see equal and opposite angles
    let hp = hypar();
    let gh = generate(&hp, Pattern::Quad, 6, 6, Supports::Edges).unwrap();
    let mut twisted = 0;
    for k in 0..gh.bars.len() {
        let b = gh.bars[k];
        let (na, nb) = (gh.nodes[b.a as usize].n, gh.nodes[b.b as usize].n);
        let e = (gh.nodes[b.b as usize].p - gh.nodes[b.a as usize].p).normalized();
        let w = gh.bar_up(k);
        let ang = |n: V3| {
            let t = (n - e * n.dot(e)).normalized();
            w.cross(t).dot(e).atan2(w.dot(t))
        };
        assert!((ang(na) + ang(nb)).abs() < 1e-9, "bar {k}");
        if gh.bar_twist(k) > 1e-3 {
            twisted += 1;
        }
    }
    assert!(twisted > 0);
}

#[test]
fn lath_curves_follow_the_lines() {
    // plane: laths are straight, interior points on the chord at the quarter points
    let p = Host::Plane {
        ax: Axes::world(),
        w: 10.0,
        h: 6.0,
    };
    let g = generate(&p, Pattern::Quad, 5, 3, Supports::Edges).unwrap();
    let nb = g.line_nbrs();
    for k in 0..g.bars.len() {
        let b = g.bars[k];
        let (a, c) = (g.nodes[b.a as usize].p, g.nodes[b.b as usize].p);
        for (t, q) in [0.25, 0.5, 0.75].iter().zip(g.lath_via(k, &nb)) {
            assert!((q - (a + (c - a) * *t)).len() < 1e-12);
        }
    }
    // sphere: the C1 curve through the meridian nodes stays near the sphere
    let h = dome();
    let g = generate(&h, Pattern::Quad, 16, 8, Supports::Base).unwrap();
    let nb = g.line_nbrs();
    let mut worst: f64 = 0.0;
    for k in 0..g.bars.len() {
        for q in g.lath_via(k, &nb) {
            worst = worst.max((q.len() - 20.0).abs());
        }
    }
    // chord sag of a 20 m sphere at about 7.9 m spacing is 0.39 m; the Hermite lath is far closer
    assert!(worst < 0.05, "lath leaves the sphere by {worst}");
    // tangent continuity at an interior node of a line
    let b0 = g.bars.iter().position(|b| b.role == BarRole::Grid).unwrap();
    let line = g.bars[b0].line;
    let mid = g.bars[b0].b;
    let b1 = g
        .bars
        .iter()
        .position(|b| b.line == line && b.a == mid)
        .unwrap();
    let v0 = g.lath_via(b0, &nb);
    let v1 = g.lath_via(b1, &nb);
    let pm = g.nodes[mid as usize].p;
    let ang = |a: V3, b: V3| a.cross(b).len().atan2(a.dot(b));
    let lath = ang(pm - v0[2], v1[0] - pm);
    // the polyline kink at the node; a C1 curve turns about a quarter of it between the
    // quarter points on either side
    let (pa, pb) = (g.nodes[g.bars[b0].a as usize].p, g.nodes[g.bars[b1].b as usize].p);
    let kink = ang(pm - pa, pb - pm);
    assert!(kink > 0.1 && lath < 0.4 * kink, "lath {lath} kink {kink}");
}

#[test]
fn node_clash_check() {
    let h = dome();
    let g = generate(&h, Pattern::Diagrid, 24, 8, Supports::Base).unwrap();
    let r = vec![0.03; g.bars.len()];
    assert!(g.clashes(0.4, &r, 0.01).is_empty(), "{:?}", g.clashes(0.4, &r, 0.01).first());
    // a node larger than the spacing near the crown
    let c = g.clashes(5.0, &r, 0.01);
    assert!(c.iter().any(|c| c.text.contains("closer than a node")));
    assert!(c.iter().any(|c| c.text.contains("shorter than a node")));
    // members far wider than the node overlap at every crossing
    let fat = vec![0.3; g.bars.len()];
    let c = g.clashes(0.4, &fat, 0.01);
    assert!(c.iter().any(|c| c.text.contains("the node needs")));
    // no node size: nothing to check
    assert!(g.clashes(0.0, &r, 0.0).is_empty());
}

#[test]
fn bends_cover_every_interior_line_node() {
    let h = hypar();
    let g = generate(&h, Pattern::Quad, 4, 3, Supports::Edges).unwrap();
    let b = g.bends(1.0);
    // u-lines: 4 rows x 3 interior nodes; v-lines: 5 columns x 2 interior nodes
    assert_eq!(b.len(), 4 * 3 + 5 * 2);
    for x in &b {
        assert!(x.a != x.c && x.a != x.b);
    }
}

#[test]
fn cable_net_form_found_on_its_boundary() {
    // a hypar boundary with uniform force density: the free net settles on the minimal net,
    // which for uniform q is the discrete harmonic interpolation of the boundary; a hypar
    // z = c u v is harmonic, and on a uniform grid the discrete Laplacian of u v is zero too
    let h = hypar();
    let g = generate(&h, Pattern::Quad, 8, 8, Supports::Edges).unwrap();
    // flatten the interior so the solve has work to do
    let mut start = g.positions();
    for (p, n) in start.iter_mut().zip(&g.nodes) {
        if !n.fixed {
            p.z = 0.0;
        }
    }
    let e = g.edges();
    let q = vec![1.0; e.len()];
    let load = vec![v3(0.0, 0.0, 0.0); start.len()];
    let r = force_density(&start, &e, &q, &g.fixed(), &load).unwrap();
    for (a, n) in r.pos.iter().zip(&g.nodes) {
        assert!((*a - n.p).len() < 1e-10, "{a:?} vs {:?}", n.p);
    }
    // the same with dynamic relaxation and the new normals face up
    let law = vec![EdgeLaw::Density(1.0); e.len()];
    let fixed = g.fixed();
    let d = relax(
        &start,
        &Relax {
            edges: &e,
            law: &law,
            bends: &[],
            fixed: &fixed,
            load: &load,
            tol: 1e-10,
            max_steps: 200_000,
        },
    )
    .unwrap();
    let mut g2 = g.clone();
    for (n, p) in g2.nodes.iter_mut().zip(&d.pos) {
        assert!((n.p - *p).len() < 1e-8);
        n.p = *p;
    }
    g2.normals_from_cells();
    for (a, b) in g2.nodes.iter().zip(&g.nodes) {
        assert!((a.n.len() - 1.0).abs() < 1e-12);
        assert!(a.n.dot(b.n) > 0.99, "normal turned");
    }
}
