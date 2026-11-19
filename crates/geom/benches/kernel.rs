//! Benchmarks of the KERNEL.md §7 budgets. Prints a "measured / budget" table.
//! `DDC_BENCH_QUICK=1` prints only the table, without criterion. Full run if the 1/10 scale takes longer than 0.5 s: `DDC_BENCH_FULL=1`.

use criterion::{black_box, Criterion};
use ddc_geom::layered::{
    join, layer_footprints, layer_quantities, layer_solids, threads, Layer, Opening, Strip,
};
use ddc_geom::recognize::{recognize_walls, Options, Seg};
use ddc_geom::csg::BoolOp;
use ddc_geom::csg2::mesh_boolean;
use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, v3, Profile, Solid, Tol};
use std::time::{Duration, Instant};

fn wall3() -> (Strip, [Opening; 2]) {
    let s = Strip {
        a: v2(0.0, 0.0),
        b: v2(6.0, 0.0),
        layers: vec![
            Layer::with_priority(0.1, 3),
            Layer::with_priority(0.2, 1),
            Layer::with_priority(0.05, 5),
        ],
        offset0: -0.175,
        base: 0.0,
        height: 3.0,
    };
    let o = [
        Opening {
            along: 1.0,
            width: 1.0,
            sill: 0.9,
            height: 1.2,
        },
        Opening {
            along: 3.5,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
        },
    ];
    (s, o)
}

fn regenerate_wall(s: &Strip, o: &[Opening; 2], tol: Tol) -> usize {
    let j = join(std::slice::from_ref(s), tol).unwrap();
    let f = layer_footprints(s, &j.pieces[0], 0, tol).unwrap();
    let q = layer_quantities(s, &f, o, 0).unwrap();
    let solids = layer_solids(s, &f, tol);
    black_box(q.len() + solids.len())
}

/// nx×ny node lattice, 4 m pitch: (nx-1)·ny + nx·(ny-1) walls.
fn grid_walls(nx: usize, ny: usize) -> Vec<Strip> {
    let mut v = vec![];
    for j in 0..ny {
        for i in 0..nx {
            let p = v2(i as f64 * 4.0, j as f64 * 4.0);
            let mk = |b| Strip {
                a: p,
                b,
                layers: vec![Layer::new(0.1), Layer::new(0.2), Layer::new(0.1)],
                offset0: -0.2,
                base: 0.0,
                height: 3.0,
            };
            if i + 1 < nx {
                v.push(mk(v2(p.x + 4.0, p.y)));
            }
            if j + 1 < ny {
                v.push(mk(v2(p.x, p.y + 4.0)));
            }
        }
    }
    v
}

fn join_footprints(strips: &[Strip], tol: Tol) -> usize {
    let j = join(strips, tol).unwrap();
    let mut n = 0;
    for (i, s) in strips.iter().enumerate() {
        n += layer_footprints(s, &j.pieces[i], i, tol).unwrap().len();
    }
    black_box(n)
}

/// A set of boxes: 6 faces each, about `faces` in total.
fn boxes(faces: usize) -> Vec<Solid> {
    let p = Profile::rectangle(1.0, 2.0).expect("rectangle");
    let s = Solid::extrude(&p, 3.0, Tol::default());
    vec![s; faces / 6]
}

fn triangulate_all(solids: &[Solid]) -> usize {
    solids
        .iter()
        .map(|s| s.triangulate().unwrap().triangles.len())
        .sum()
}

/// UV sphere, `2 nu (nv - 1)` triangles.
fn sphere_mesh(r: f64, nu: usize, nv: usize) -> Mesh {
    use std::f64::consts::{PI, TAU};
    let mut m = Mesh::default();
    m.positions.push(v3(0., 0., -r));
    for j in 1..nv {
        let th = PI * j as f64 / nv as f64 - PI / 2.0;
        for i in 0..nu {
            let ph = TAU * i as f64 / nu as f64;
            m.positions
                .push(v3(th.cos() * ph.cos(), th.cos() * ph.sin(), th.sin()) * r);
        }
    }
    m.positions.push(v3(0., 0., r));
    let top = (m.positions.len() - 1) as u32;
    let ring = |j: usize, i: usize| (1 + (j - 1) * nu + i % nu) as u32;
    for i in 0..nu {
        m.triangles.push([0, ring(1, i + 1), ring(1, i)]);
        m.triangles.push([top, ring(nv - 1, i), ring(nv - 1, i + 1)]);
    }
    for j in 1..nv - 1 {
        for i in 0..nu {
            let (a, b, c, d) = (ring(j, i), ring(j, i + 1), ring(j + 1, i + 1), ring(j + 1, i));
            m.triangles.push([a, b, c]);
            m.triangles.push([a, c, d]);
        }
    }
    m.face_of = vec![0; m.triangles.len()];
    m
}

/// Vertical n-gon prism through the origin, slightly off-centre, caps fanned.
fn prism_mesh(r: f64, h: f64, n: usize) -> Mesh {
    let mut m = Mesh::default();
    for z in [-h, h] {
        for i in 0..n {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            m.positions.push(v3(0.05 + r * a.cos(), -0.03 + r * a.sin(), z));
        }
    }
    let n = n as u32;
    m.positions.push(v3(0.05, -0.03, -h));
    m.positions.push(v3(0.05, -0.03, h));
    for i in 0..n {
        let j = (i + 1) % n;
        m.triangles.push([i, j, j + n]);
        m.triangles.push([i, j + n, i + n]);
        m.triangles.push([2 * n, j, i]);
        m.triangles.push([2 * n + 1, i + n, j + n]);
    }
    m.face_of = vec![1; m.triangles.len()];
    m
}

/// n×n cells: walls along grid lines, each wall with two opposing segments.
fn plan_segments(n: usize, t: f64) -> Vec<Seg> {
    let mut v = vec![];
    for j in 0..=n {
        for i in 0..n {
            let (x, y) = (i as f64 * 4.0, j as f64 * 4.0);
            for s in [-t / 2.0, t / 2.0] {
                v.push(Seg {
                    a: v2(x, y + s),
                    b: v2(x + 4.0, y + s),
                });
                v.push(Seg {
                    a: v2(y + s, x),
                    b: v2(y + s, x + 4.0),
                });
            }
        }
    }
    v
}

fn median<F: FnMut()>(mut f: F, runs: usize) -> Duration {
    f();
    let mut t: Vec<Duration> = (0..runs)
        .map(|_| {
            let s = Instant::now();
            f();
            s.elapsed()
        })
        .collect();
    t.sort();
    t[t.len() / 2]
}

fn main() {
    let tol = Tol::default();
    let quick = std::env::var("DDC_BENCH_QUICK").is_ok();
    let (w, o) = wall3();
    let all_walls = grid_walls(101, 51); // 100*51 + 101*50 = 10 150
    let opt = Options::default();
    let tenth = |n: usize| (n / 10).max(1);

    // Run at 1/10 scale first; if it exceeds SLOW, the full run is skipped.
    const SLOW: Duration = Duration::from_millis(500);
    let force_full = std::env::var("DDC_BENCH_FULL").is_ok();
    let staged = |name: &str, small: &mut dyn FnMut(), full: &mut dyn FnMut(), runs: usize| {
        let ts = median(small, 1);
        eprintln!("{name}: 1/10 scale {ts:?}");
        if ts > SLOW && !force_full {
            return (ts, false);
        }
        (median(full, if ts > SLOW { 1 } else { runs }), true)
    };

    let walls = &all_walls[..10_000];
    let solids = boxes(1_000_002);
    let segs = plan_segments(50, 0.2); // 4 * 51 * 50 = 10 200 segments
    let small_segs = plan_segments(15, 0.2);
    let big_segs = plan_segments(224, 0.2); // 4 * 225 * 224 = 201 600 segments

    if !quick {
        let mut c = Criterion::default().configure_from_args();
        c.bench_function("wall3_2openings_regen", |b| {
            b.iter(|| regenerate_wall(&w, &o, tol))
        });
        c.final_summary();
    }

    let t_wall = median(
        || {
            regenerate_wall(&w, &o, tol);
        },
        2001,
    );
    let (t_join, j_full) = staged(
        "join",
        &mut || {
            join_footprints(&walls[..tenth(walls.len())], tol);
        },
        &mut || {
            join_footprints(walls, tol);
        },
        3,
    );
    let (t_tri, t_full) = staged(
        "triangulate",
        &mut || {
            triangulate_all(&solids[..tenth(solids.len())]);
        },
        &mut || {
            triangulate_all(&solids);
        },
        3,
    );
    let (t_rec, r_full) = staged(
        "recognize",
        &mut || {
            black_box(recognize_walls(&small_segs, &opt).strips.len());
        },
        &mut || {
            black_box(recognize_walls(&segs, &opt).strips.len());
        },
        3,
    );
    let (t_big, b_full) = staged(
        "recognize 200k",
        &mut || {
            black_box(recognize_walls(&segs, &opt).strips.len());
        },
        &mut || {
            black_box(recognize_walls(&big_segs, &opt).strips.len());
        },
        3,
    );
    let (ball, prism) = (sphere_mesh(1.0, 250, 101), prism_mesh(0.4, 1.5, 128));
    let t_bool = median(
        || {
            let r = mesh_boolean(&ball, &prism, BoolOp::Subtract, tol).expect("mesh boolean");
            black_box(r.mesh.triangles.len());
        },
        5,
    );
    let tag = |full: bool, n: &str| {
        if full {
            n.to_string()
        } else {
            format!("{n} [1/10 scale only]")
        }
    };

    let rows: [(String, Duration, Option<Duration>); 6] = [
        (
            "sphere 50k facets - prism 512 facets, mesh boolean".into(),
            t_bool,
            Some(Duration::from_millis(250)),
        ),
        (
            "wall 3 layers + 2 openings, regen (join+plan+qty+solids)".into(),
            t_wall,
            Some(Duration::from_micros(50)),
        ),
        (
            tag(
                j_full,
                &format!("10 000 walls join + footprints, {} thread(s)", threads()),
            ),
            t_join,
            Some(Duration::from_millis(300)),
        ),
        (
            tag(t_full, "triangulate 1M faces (166 667 boxes)"),
            t_tri,
            Some(Duration::from_secs(1)),
        ),
        (
            tag(r_full, "recognize 10k segments (no budget yet)"),
            t_rec,
            None,
        ),
        (
            tag(b_full, "recognize 200k segments"),
            t_big,
            Some(Duration::from_secs(1)),
        ),
    ];
    println!(
        "\n{:<66} {:>12} {:>10}  verdict",
        "operation", "measured", "budget"
    );
    for (name, t, b) in rows {
        let (bs, v) = match b {
            Some(b) => (format!("{b:?}"), if t <= b { "OK" } else { "OVER" }),
            None => ("-".into(), "info"),
        };
        println!("{name:<66} {:>12} {bs:>10}  {v}", format!("{t:?}"));
    }
}
