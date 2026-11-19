// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 12-DOF frame element: local axes, stiffness (Euler-Bernoulli or Timoshenko), end releases
//! by static condensation, rigid offsets, and exact equivalent loads via exact shape functions.

use crate::Orient;

pub(crate) type V3 = [f64; 3];
pub(crate) type V12 = [f64; 12];
pub(crate) type M12 = [[f64; 12]; 12];

/// 4-point Gauss-Legendre on [0, 1]: (abscissa, weight); exact to degree 7.
pub(crate) const G4: [(f64, f64); 4] = [
    (0.5 - 0.5 * 0.861_136_311_594_052_6, 0.5 * 0.347_854_845_137_453_8),
    (0.5 - 0.5 * 0.339_981_043_584_856_3, 0.5 * 0.652_145_154_862_546_1),
    (0.5 + 0.5 * 0.339_981_043_584_856_3, 0.5 * 0.652_145_154_862_546_1),
    (0.5 + 0.5 * 0.861_136_311_594_052_6, 0.5 * 0.347_854_845_137_453_8),
];

pub(crate) fn add3(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn sub3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn scale3(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub(crate) fn dot3(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross3(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn norm3(a: V3) -> f64 {
    dot3(a, a).sqrt()
}
pub(crate) fn lerp3(a: V3, b: V3, t: f64) -> V3 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Local axes [x, y, z] (rows, global components) for unit axis `x`.
pub(crate) fn axes(x: V3, orient: Orient) -> Result<[V3; 3], &'static str> {
    let auto = if x[2].abs() > 1.0 - 1e-9 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
    let (v, roll) = match orient {
        Orient::Auto => (auto, 0.0),
        Orient::Vector(v) => (v, 0.0),
        Orient::Roll(r) => (auto, r),
    };
    let y = cross3(v, x);
    let ny = norm3(y);
    // partial_cmp: a NaN norm is refused too
    if ny.partial_cmp(&(1e-9 * norm3(v))) != Some(std::cmp::Ordering::Greater) || !ny.is_finite() {
        return Err("orientation vector is zero or parallel to the member");
    }
    let y = scale3(y, 1.0 / ny);
    let z = cross3(x, y);
    if roll == 0.0 {
        return Ok([x, y, z]);
    }
    let (s, c) = roll.sin_cos();
    let y2 = add3(scale3(y, c), scale3(z, s));
    let z2 = add3(scale3(y, -s), scale3(z, c));
    Ok([x, y2, z2])
}

/// Rigidities of one member; `phy`/`phz` = 12EI/(G Av L^2) for bending in local x-y / x-z.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Props {
    pub ea: f64,
    pub gj: f64,
    pub eiy: f64,
    pub eiz: f64,
    pub phy: f64,
    pub phz: f64,
}

fn put4(k: &mut M12, d: [usize; 4], m: [[f64; 4]; 4]) {
    for a in 0..4 {
        for b in 0..4 {
            k[d[a]][d[b]] += m[a][b];
        }
    }
}

/// Local stiffness, DOFs per end: ux, uy, uz, rx, ry, rz (rz = dv/dx, ry = -dw/dx).
pub(crate) fn k_local(l: f64, p: &Props) -> M12 {
    let mut k = [[0.0; 12]; 12];
    let ka = p.ea / l;
    let kt = p.gj / l;
    put4(&mut k, [0, 6, 3, 9], [[ka, -ka, 0.0, 0.0], [-ka, ka, 0.0, 0.0], [0.0; 4], [0.0; 4]]);
    put4(&mut k, [3, 9, 0, 6], [[kt, -kt, 0.0, 0.0], [-kt, kt, 0.0, 0.0], [0.0; 4], [0.0; 4]]);
    let l2 = l * l;
    let f = p.phy;
    let c = p.eiz / ((1.0 + f) * l2 * l);
    let (a, b, d, e) = (12.0 * c, 6.0 * l * c, (4.0 + f) * l2 * c, (2.0 - f) * l2 * c);
    put4(&mut k, [1, 5, 7, 11], [[a, b, -a, b], [b, d, -b, e], [-a, -b, a, -b], [b, e, -b, d]]);
    let f = p.phz;
    let c = p.eiy / ((1.0 + f) * l2 * l);
    let (a, b, d, e) = (12.0 * c, 6.0 * l * c, (4.0 + f) * l2 * c, (2.0 - f) * l2 * c);
    put4(&mut k, [2, 4, 8, 10], [[a, -b, -a, -b], [-b, d, b, e], [-a, b, a, b], [-b, e, b, d]]);
    k
}

/// Exact (interdependent) shapes in the x-y plane at xi: deflections N and section rotations.
fn shapes(xi: f64, l: f64, ph: f64) -> ([f64; 4], [f64; 4]) {
    let mu = 1.0 / (1.0 + ph);
    let x2 = xi * xi;
    let x3 = x2 * xi;
    let n = [
        mu * (1.0 - 3.0 * x2 + 2.0 * x3 + ph * (1.0 - xi)),
        mu * l * (xi - 2.0 * x2 + x3 + 0.5 * ph * (xi - x2)),
        mu * (3.0 * x2 - 2.0 * x3 + ph * xi),
        mu * l * (x3 - x2 + 0.5 * ph * (x2 - xi)),
    ];
    let r = [
        mu * 6.0 / l * (x2 - xi),
        mu * (1.0 - 4.0 * xi + 3.0 * x2 + ph * (1.0 - xi)),
        mu * 6.0 / l * (xi - x2),
        mu * (3.0 * x2 - 2.0 * xi + ph * xi),
    ];
    (n, r)
}

/// Member load in local axes on the flexible part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MLoad {
    Point { a: f64, f: V3, m: V3 },
    Dist { a: f64, b: f64, w1: V3, w2: V3 },
}

/// Work-equivalent nodal load at `a` (Betti: reaction = load * exact shape at the load point).
fn add_point(out: &mut V12, l: f64, p: &Props, a: f64, f: V3, m: V3) {
    let xi = a / l;
    out[0] += f[0] * (1.0 - xi);
    out[6] += f[0] * xi;
    out[3] += m[0] * (1.0 - xi);
    out[9] += m[0] * xi;
    let (n, r) = shapes(xi, l, p.phy);
    for (k, &d) in [1usize, 5, 7, 11].iter().enumerate() {
        out[d] += f[1] * n[k] + m[2] * r[k];
    }
    let (n, r) = shapes(xi, l, p.phz);
    let sg = [1.0, -1.0, 1.0, -1.0];
    for (k, &d) in [2usize, 4, 8, 10].iter().enumerate() {
        out[d] += sg[k] * (f[2] * n[k] - m[1] * r[k]);
    }
}

/// Equivalent nodal loads (local, at the flexible ends) of the member loads; exact.
pub(crate) fn equiv(l: f64, p: &Props, loads: &[MLoad]) -> V12 {
    let mut out = [0.0; 12];
    for ld in loads {
        match *ld {
            MLoad::Point { a, f, m } => add_point(&mut out, l, p, a, f, m),
            MLoad::Dist { a, b, w1, w2 } => {
                for (g, wt) in G4 {
                    let q = scale3(lerp3(w1, w2, g), (b - a) * wt);
                    add_point(&mut out, l, p, a + (b - a) * g, q, [0.0; 3]);
                }
            }
        }
    }
    out
}

/// Condenses released DOFs out of `k` and `f`; Err(dof) if a released DOF has load, no stiffness.
pub(crate) fn condense(k: &mut M12, f: &mut V12, rel: &[bool; 12]) -> Result<(), usize> {
    let d0: Vec<f64> = (0..12).map(|i| k[i][i]).collect();
    for r in 0..12 {
        if !rel[r] {
            continue;
        }
        let krr = k[r][r];
        if krr > 1e-9 * d0[r] && d0[r] > 0.0 {
            for i in 0..12 {
                if i == r {
                    continue;
                }
                let c = k[i][r] / krr;
                if c == 0.0 {
                    continue;
                }
                for j in 0..12 {
                    k[i][j] -= c * k[r][j];
                }
                f[i] -= c * f[r];
            }
        } else {
            let fmax = f.iter().fold(0.0f64, |m, v| m.max(v.abs()));
            if f[r].abs() > 1e-12 * fmax && f[r] != 0.0 {
                return Err(r);
            }
        }
        for j in 0..12 {
            k[r][j] = 0.0;
            k[j][r] = 0.0;
        }
        f[r] = 0.0;
    }
    Ok(())
}

/// u_local = T u_node: rotation to local axes after the rigid offset `u + theta x r`.
pub(crate) fn transform(rot: &[V3; 3], off: &[V3; 2]) -> M12 {
    let mut t = [[0.0; 12]; 12];
    for (e, r) in off.iter().enumerate() {
        let s = [[0.0, -r[2], r[1]], [r[2], 0.0, -r[0]], [-r[1], r[0], 0.0]];
        for a in 0..3 {
            for b in 0..3 {
                let rs: f64 = (0..3).map(|c| rot[a][c] * s[c][b]).sum();
                t[6 * e + a][6 * e + b] = rot[a][b];
                t[6 * e + a][6 * e + 3 + b] = -rs;
                t[6 * e + 3 + a][6 * e + 3 + b] = rot[a][b];
            }
        }
    }
    t
}

/// T^T k T.
pub(crate) fn tkt(t: &M12, k: &M12) -> M12 {
    let mut kt = [[0.0; 12]; 12];
    for i in 0..12 {
        for m in 0..12 {
            let v = k[i][m];
            if v == 0.0 {
                continue;
            }
            for j in 0..12 {
                kt[i][j] += v * t[m][j];
            }
        }
    }
    let mut out = [[0.0; 12]; 12];
    for m in 0..12 {
        for i in 0..12 {
            let v = t[m][i];
            if v == 0.0 {
                continue;
            }
            for j in 0..12 {
                out[i][j] += v * kt[m][j];
            }
        }
    }
    out
}

pub(crate) fn mul(t: &M12, v: &V12) -> V12 {
    let mut o = [0.0; 12];
    for i in 0..12 {
        o[i] = (0..12).map(|j| t[i][j] * v[j]).sum();
    }
    o
}

pub(crate) fn mul_t(t: &M12, v: &V12) -> V12 {
    let mut o = [0.0; 12];
    for i in 0..12 {
        o[i] = (0..12).map(|j| t[j][i] * v[j]).sum();
    }
    o
}
