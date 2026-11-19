//! Repeated meshes: a mesh in world coordinates is split into a local shape (centred, turned
//! about Z so that its first off-axis vertex lies on +X) and a placement (turn + shift).
//! Copies of one type placed by translation and rotation about Z get the same local shape
//! and the same hash; a hash match is always confirmed by comparing the shapes.

use crate::math::{v3, V3};

/// Grid of the hashed local coordinates, m (a hash miss only costs a duplicate prototype).
pub const QUANTUM: f64 = 1e-5;

/// world = Rz(θ)·local + t, with (cos θ, sin θ).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub cos: f64,
    pub sin: f64,
    pub t: [f64; 3],
}

impl Place {
    pub const IDENTITY: Place = Place {
        cos: 1.0,
        sin: 0.0,
        t: [0.0; 3],
    };

    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        [
            self.cos * p[0] - self.sin * p[1] + self.t[0],
            self.sin * p[0] + self.cos * p[1] + self.t[1],
            p[2] + self.t[2],
        ]
    }

    /// Column-major 4×4 (the layout of `glam::Mat4::from_cols_array`).
    pub fn cols(&self) -> [f32; 16] {
        let (c, s) = (self.cos as f32, self.sin as f32);
        [
            c,
            s,
            0.0,
            0.0, //
            -s,
            c,
            0.0,
            0.0, //
            0.0,
            0.0,
            1.0,
            0.0, //
            self.t[0] as f32,
            self.t[1] as f32,
            self.t[2] as f32,
            1.0,
        ]
    }
}

/// Local shape of a mesh and where it is placed.
#[derive(Clone, Debug)]
pub struct Canon {
    pub hash: u64,
    pub local: Vec<[f64; 3]>,
    pub place: Place,
    /// Largest distance of a local vertex from the local origin.
    pub radius: f64,
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= *b as u64;
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// Canonical local shape; None for an empty or non-finite mesh.
pub fn canonical(pos: &[V3], tris: &[[u32; 3]]) -> Option<Canon> {
    if pos.is_empty()
        || pos
            .iter()
            .any(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()))
    {
        return None;
    }
    let n = pos.len() as f64;
    let mut c = v3(0.0, 0.0, 0.0);
    for p in pos {
        c = c + *p;
    }
    c = c * (1.0 / n);
    let rel: Vec<V3> = pos.iter().map(|p| *p - c).collect();
    let rmax = rel.iter().map(|p| p.x.hypot(p.y)).fold(0.0, f64::max);
    // first vertex clearly off the Z axis fixes the turn; vertex order is part of the shape
    let (cos, sin) = rel
        .iter()
        .find(|p| p.x.hypot(p.y) > 0.1 * rmax && rmax > 1e-9)
        .map(|p| {
            let r = p.x.hypot(p.y);
            (p.x / r, p.y / r)
        })
        .unwrap_or((1.0, 0.0));
    let local: Vec<[f64; 3]> = rel
        .iter()
        .map(|p| [cos * p.x + sin * p.y, -sin * p.x + cos * p.y, p.z])
        .collect();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fnv(&mut h, &(local.len() as u64).to_le_bytes());
    fnv(&mut h, &(tris.len() as u64).to_le_bytes());
    for t in tris {
        for i in t {
            fnv(&mut h, &i.to_le_bytes());
        }
    }
    for p in &local {
        for x in p {
            let q = (x / QUANTUM).round() as i64;
            fnv(&mut h, &q.to_le_bytes());
        }
    }
    let radius = local
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .fold(0.0, f64::max);
    Some(Canon {
        hash: h,
        local,
        place: Place {
            cos,
            sin,
            t: [c.x, c.y, c.z],
        },
        radius,
    })
}

/// Same shape within `tol` (per coordinate), same triangles.
pub fn same_shape(
    a: &[[f64; 3]],
    at: &[[u32; 3]],
    b: &[[f32; 3]],
    bt: &[[u32; 3]],
    tol: f64,
) -> bool {
    a.len() == b.len()
        && at == bt
        && a.iter()
            .zip(b)
            .all(|(p, q)| (0..3).all(|k| (p[k] - q[k] as f64).abs() <= tol))
}

/// Tolerance of [`same_shape`] for a shape of `radius` stored as f32.
pub fn shape_tol(radius: f64) -> f64 {
    QUANTUM + radius * 4.0 * f32::EPSILON as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chair() -> (Vec<V3>, Vec<[u32; 3]>) {
        let p = vec![
            v3(0.0, 0.0, 0.0),
            v3(0.5, 0.0, 0.0),
            v3(0.5, 0.4, 0.0),
            v3(0.0, 0.4, 0.0),
            v3(0.0, 0.0, 0.9),
            v3(0.5, 0.0, 0.45),
        ];
        (p, vec![[0, 1, 2], [0, 2, 3], [0, 1, 5], [0, 5, 4]])
    }

    fn placed(p: &[V3], th: f64, t: V3) -> Vec<V3> {
        let (c, s) = (th.cos(), th.sin());
        p.iter()
            .map(|q| v3(c * q.x - s * q.y + t.x, s * q.x + c * q.y + t.y, q.z + t.z))
            .collect()
    }

    #[test]
    fn turned_and_shifted_copies_share_shape_and_rebuild_exactly() {
        let (p, t) = chair();
        let a = canonical(&p, &t).unwrap();
        for (th, sh) in [
            (0.0, v3(10., 0., 0.)),
            (1.234, v3(-3., 7., 2.5)),
            (3.0, v3(1e4, -2e4, 30.)),
        ] {
            let w = placed(&p, th, sh);
            let b = canonical(&w, &t).unwrap();
            assert_eq!(a.hash, b.hash, "θ={th}");
            let stored: Vec<[f32; 3]> = a.local.iter().map(|q| q.map(|x| x as f32)).collect();
            assert!(same_shape(&b.local, &t, &stored, &t, shape_tol(a.radius)));
            for (k, q) in a.local.iter().enumerate() {
                let r = b.place.apply(*q);
                let e = w[k];
                assert!(
                    (r[0] - e.x).abs() < 1e-7
                        && (r[1] - e.y).abs() < 1e-7
                        && (r[2] - e.z).abs() < 1e-9
                );
            }
        }
    }

    #[test]
    fn different_or_mirrored_shapes_do_not_match() {
        let (p, t) = chair();
        let a = canonical(&p, &t).unwrap();
        let mut q = p.clone();
        q[4].z = 1.0;
        assert_ne!(a.hash, canonical(&q, &t).unwrap().hash);
        let m: Vec<V3> = p.iter().map(|v| v3(-v.x, v.y, v.z)).collect();
        let b = canonical(&m, &t).unwrap();
        let stored: Vec<[f32; 3]> = a.local.iter().map(|q| q.map(|x| x as f32)).collect();
        assert!(!same_shape(&b.local, &t, &stored, &t, shape_tol(a.radius)));
        assert!(canonical(&[], &[]).is_none());
        assert!(canonical(&[v3(f64::NAN, 0., 0.)], &[]).is_none());
    }

    #[test]
    fn placement_matrix_matches_apply() {
        let pl = Place {
            cos: 0.6,
            sin: 0.8,
            t: [1.0, 2.0, 3.0],
        };
        let m = pl.cols();
        let p = [2.0, -1.0, 0.5];
        let r = pl.apply(p);
        let x = m[0] * 2.0 + m[4] * -1.0 + m[8] * 0.5 + m[12];
        let y = m[1] * 2.0 + m[5] * -1.0 + m[9] * 0.5 + m[13];
        let z = m[2] * 2.0 + m[6] * -1.0 + m[10] * 0.5 + m[14];
        assert!(
            (x as f64 - r[0]).abs() < 1e-6
                && (y as f64 - r[1]).abs() < 1e-6
                && (z as f64 - r[2]).abs() < 1e-6
        );
    }
}
