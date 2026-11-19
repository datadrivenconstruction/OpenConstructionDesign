//! Small vector and frame algebra for the reader.

pub type V = [f64; 3];

pub fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn mul(a: V, s: f64) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn len(a: V) -> f64 {
    dot(a, a).sqrt()
}

pub fn dist(a: V, b: V) -> f64 {
    len(sub(a, b))
}

pub fn unit(a: V) -> Option<V> {
    let l = len(a);
    (l > 1e-300 && l.is_finite()).then(|| mul(a, 1.0 / l))
}

pub fn lerp(a: V, b: V, t: f64) -> V {
    add(a, mul(sub(b, a), t))
}

/// Distance from `p` to the segment `a`-`b`.
pub fn seg_dist(p: V, a: V, b: V) -> f64 {
    let d = sub(b, a);
    let l2 = dot(d, d);
    if l2 <= 0.0 {
        return dist(p, a);
    }
    let t = (dot(sub(p, a), d) / l2).clamp(0.0, 1.0);
    dist(p, lerp(a, b, t))
}

/// Right-handed orthonormal frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ax {
    pub o: V,
    pub x: V,
    pub y: V,
    pub z: V,
}

impl Ax {
    /// Z from `z` (default +Z), X from `x` made perpendicular (default +X, else +Y).
    pub fn new(o: V, z: Option<V>, x: Option<V>) -> Ax {
        let z = z.and_then(unit).unwrap_or([0.0, 0.0, 1.0]);
        let perp = move |c: V| unit(sub(c, mul(z, dot(c, z))));
        let x = x
            .and_then(perp)
            .or_else(|| perp([1.0, 0.0, 0.0]))
            .or_else(|| perp([0.0, 1.0, 0.0]))
            .unwrap_or([1.0, 0.0, 0.0]);
        Ax {
            o,
            x,
            y: cross(z, x),
            z,
        }
    }

    pub fn dir(&self, a: f64, b: f64, c: f64) -> V {
        add(add(mul(self.x, a), mul(self.y, b)), mul(self.z, c))
    }

    pub fn at(&self, a: f64, b: f64, c: f64) -> V {
        add(self.o, self.dir(a, b, c))
    }

    pub fn local(&self, p: V) -> V {
        let d = sub(p, self.o);
        [dot(d, self.x), dot(d, self.y), dot(d, self.z)]
    }

    pub fn xf(&self) -> Xf {
        let (x, y, z) = (self.x, self.y, self.z);
        Xf {
            m: [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]],
            t: self.o,
        }
    }
}

/// Affine map `p -> m p + t` (rows of `m`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xf {
    pub m: [[f64; 3]; 3],
    pub t: V,
}

impl Xf {
    pub const ID: Xf = Xf {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0; 3],
    };

    pub fn apply(&self, p: V) -> V {
        let m = &self.m;
        [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + self.t[r])
    }

    fn lin(&self, p: V) -> V {
        let m = &self.m;
        [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2])
    }

    /// `self` applied after `b`.
    pub fn after(&self, b: &Xf) -> Xf {
        let mut m = [[0.0; 3]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.m[r][k] * b.m[k][c]).sum();
            }
        }
        Xf {
            m,
            t: add(self.lin(b.t), self.t),
        }
    }

    pub fn det(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    pub fn inverse(&self) -> Option<Xf> {
        let d = self.det();
        if d.abs() < 1e-300 || !d.is_finite() {
            return None;
        }
        let m = &self.m;
        let c = |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
        let inv = [
            [c(1, 1, 2, 2) / d, -c(0, 1, 2, 2) / d, c(0, 1, 1, 2) / d],
            [-c(1, 0, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 1, 2) / d],
            [c(1, 0, 2, 1) / d, -c(0, 0, 2, 1) / d, c(0, 0, 1, 1) / d],
        ];
        let x = Xf { m: inv, t: [0.0; 3] };
        let t = x.lin(self.t);
        Some(Xf {
            m: inv,
            t: [-t[0], -t[1], -t[2]],
        })
    }

    /// Same map with the translation multiplied by `s` (file units to metres).
    pub fn scaled(&self, s: f64) -> Xf {
        Xf {
            m: self.m,
            t: mul(self.t, s),
        }
    }
}
