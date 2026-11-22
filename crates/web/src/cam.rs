// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Orbit camera around a target, Z up; column-major matrices for GL.

pub type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 0.0 {
        mul(a, 1.0 / l)
    } else {
        a
    }
}

#[derive(Clone, Debug)]
pub struct Orbit {
    pub target: V3,
    /// Radians around Z from +X.
    pub yaw: f32,
    /// Radians above the horizon.
    pub pitch: f32,
    pub dist: f32,
    pub fov_y: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            target: [0.0; 3],
            yaw: -0.9,
            pitch: 0.55,
            dist: 50.0,
            fov_y: 45f32.to_radians(),
        }
    }
}

impl Orbit {
    /// Looks at the box from the current direction with all of it in view.
    pub fn fit(&mut self, lo: V3, hi: V3) {
        self.target = mul(add(lo, hi), 0.5);
        let r = (dot(sub(hi, lo), sub(hi, lo)).sqrt() * 0.5).max(0.5);
        self.dist = r / (self.fov_y * 0.5).sin() * 1.05;
    }

    pub fn eye(&self) -> V3 {
        let (cp, sp) = (self.pitch.cos(), self.pitch.sin());
        add(
            self.target,
            [self.dist * cp * self.yaw.cos(), self.dist * cp * self.yaw.sin(), self.dist * sp],
        )
    }

    /// Forward, right and up unit vectors.
    pub fn basis(&self) -> (V3, V3, V3) {
        let f = norm(sub(self.target, self.eye()));
        let r = norm(cross(f, [0.0, 0.0, 1.0]));
        let u = cross(r, f);
        (f, r, u)
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-1.55, 1.55);
    }

    /// Moves the target by screen pixels of a view `h` pixels high.
    pub fn pan(&mut self, dx: f32, dy: f32, h: f32) {
        let (_, r, u) = self.basis();
        let k = 2.0 * self.dist * (self.fov_y * 0.5).tan() / h.max(1.0);
        self.target = add(self.target, add(mul(r, -dx * k), mul(u, dy * k)));
    }

    pub fn zoom(&mut self, steps: f32) {
        self.dist = (self.dist * (-steps * 0.0015).exp()).clamp(0.2, 1e6);
    }

    /// World ray through a point of the view, `x` and `y` in -1..1 (y up).
    pub fn ray(&self, x: f32, y: f32, aspect: f32) -> (V3, V3) {
        let (f, r, u) = self.basis();
        let t = (self.fov_y * 0.5).tan();
        (self.eye(), norm(add(f, add(mul(r, x * t * aspect), mul(u, y * t)))))
    }

    /// Projection times view, column-major.
    pub fn matrix(&self, aspect: f32) -> [f32; 16] {
        let (f, r, u) = self.basis();
        let e = self.eye();
        let near = (self.dist * 0.002).max(0.01);
        let far = self.dist * 20.0 + 1000.0;
        let view = [
            r[0], u[0], -f[0], 0.0,
            r[1], u[1], -f[1], 0.0,
            r[2], u[2], -f[2], 0.0,
            -dot(r, e), -dot(u, e), dot(f, e), 1.0,
        ];
        let t = 1.0 / (self.fov_y * 0.5).tan();
        let proj = [
            t / aspect, 0.0, 0.0, 0.0,
            0.0, t, 0.0, 0.0,
            0.0, 0.0, (far + near) / (near - far), -1.0,
            0.0, 0.0, 2.0 * far * near / (near - far), 0.0,
        ];
        mat_mul(&proj, &view)
    }
}

fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut o = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_ray_hits_target_and_projects_to_centre() {
        let mut o = Orbit::default();
        o.fit([0.0; 3], [10.0, 10.0, 10.0]);
        let (e, d) = o.ray(0.0, 0.0, 1.5);
        let to = norm(sub(o.target, e));
        assert!(dot(d, to) > 0.9999);
        let m = o.matrix(1.5);
        let p = o.target;
        let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
        let x = (m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12]) / w;
        let y = (m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13]) / w;
        assert!(x.abs() < 1e-4 && y.abs() < 1e-4);
    }
}
