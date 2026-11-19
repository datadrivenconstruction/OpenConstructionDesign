//! Closed-shell primitives of detailed parts: prisms of any profile along any axis, profiles
//! swept with mitred corners along a polyline in the opening plane, round bars and tubes along
//! 3D polylines. Every shell is closed and turned outward by its own signed volume.

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::profile::signed_area;

/// Rings of points along a path, each ring the same profile; `cap` triangulates the profile.
struct Rows {
    rows: Vec<Vec<V3>>,
    closed: bool,
}

fn ccw(p: &[V2]) -> Vec<V2> {
    let mut v = p.to_vec();
    if signed_area(&v) < 0.0 {
        v.reverse();
    }
    v
}

/// Appends a closed shell, outward by the sign of its volume; one face id per shell.
fn push_shell(m: &mut Mesh, pos: Vec<V3>, mut tris: Vec<[u32; 3]>) {
    if pos.is_empty() || tris.is_empty() {
        return;
    }
    let c = pos.iter().fold(V3::default(), |a, p| a + *p) * (1.0 / pos.len() as f64);
    let vol: f64 = tris
        .iter()
        .map(|t| {
            let [a, b, d] = t.map(|i| pos[i as usize] - c);
            a.dot(b.cross(d))
        })
        .sum();
    if vol < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    let base = m.positions.len() as u32;
    let face = m.face_of.last().map_or(0, |f| f + 1);
    m.positions.extend(pos);
    for t in tris {
        m.triangles.push([t[0] + base, t[1] + base, t[2] + base]);
        m.face_of.push(face);
    }
}

impl Rows {
    /// Sides between consecutive rings; an open run gets both end caps from `prof` (CCW).
    fn emit(self, m: &mut Mesh, prof: &[V2]) {
        let nr = self.rows.len();
        let k = prof.len();
        if nr < 2 || k < 3 {
            return;
        }
        let mut pos = Vec::with_capacity(nr * k);
        for r in &self.rows {
            pos.extend_from_slice(r);
        }
        let at = |r: usize, i: usize| (r * k + i) as u32;
        let spans = if self.closed { nr } else { nr - 1 };
        let mut tris = Vec::with_capacity(spans * k * 2 + 2 * k);
        for r in 0..spans {
            let s = (r + 1) % nr;
            for i in 0..k {
                let j = (i + 1) % k;
                let (a, b, c, d) = (at(r, i), at(r, j), at(s, i), at(s, j));
                tris.push([a, c, b]);
                tris.push([b, c, d]);
            }
        }
        if !self.closed {
            for t in crate::poly::triangulate(prof) {
                tris.push([at(0, t[0]), at(0, t[1]), at(0, t[2])]);
                tris.push([at(nr - 1, t[2]), at(nr - 1, t[1]), at(nr - 1, t[0])]);
            }
        }
        push_shell(m, pos, tris);
    }
}

/// Prism: `ring` in the plane `o + eu·u + ev·v`, extruded by `ew` (any length, any direction).
pub fn prism(m: &mut Mesh, ring: &[V2], o: V3, eu: V3, ev: V3, ew: V3) {
    let p = ccw(ring);
    if p.len() < 3 || ew.len() < 1e-9 {
        return;
    }
    let row = |w: V3| p.iter().map(|q| o + eu * q.x + ev * q.y + w).collect::<Vec<_>>();
    Rows {
        rows: vec![row(V3::default()), row(ew)],
        closed: false,
    }
    .emit(m, &p);
}

/// Axis-aligned block; parts are usually more than one, so a single block is never a whole
/// element.
pub fn block(m: &mut Mesh, lo: V3, hi: V3) {
    if hi.x - lo.x < 1e-6 || hi.y - lo.y < 1e-6 || hi.z - lo.z < 1e-6 {
        return;
    }
    let r = [v2(lo.x, lo.y), v2(hi.x, lo.y), v2(hi.x, hi.y), v2(lo.x, hi.y)];
    prism(m, &r, v3(0.0, 0.0, lo.z), X, Y, v3(0.0, 0.0, hi.z - lo.z));
}

pub const X: V3 = v3(1.0, 0.0, 0.0);
pub const Y: V3 = v3(0.0, 1.0, 0.0);
pub const Z: V3 = v3(0.0, 0.0, 1.0);

/// Unit vectors across a direction (any pair completing a right-handed frame).
pub fn across(d: V3) -> (V3, V3) {
    let d = d.normalized();
    let h = if d.z.abs() < 0.9 { Z } else { X };
    let u = h.cross(d).normalized();
    (u, d.cross(u))
}

/// Regular n-gon of radius `r` (vertices on the circle), CCW.
pub fn ngon(r: f64, n: usize) -> Vec<V2> {
    let n = n.max(3);
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * (i as f64 + 0.5) / n as f64;
            v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

/// Round bar from `a` to `b`, `n` sides.
pub fn rod(m: &mut Mesh, a: V3, b: V3, r: f64, n: usize) {
    let d = b - a;
    if d.len() < 1e-9 || r <= 0.0 {
        return;
    }
    let (u, v) = across(d);
    prism(m, &ngon(r, n), a, u, v, d);
}

/// Bar of any section from `a` to `b`; section x along `up × d` (horizontal), y along up.
pub fn bar(m: &mut Mesh, a: V3, b: V3, sec: &[V2]) {
    let d = b - a;
    if d.len() < 1e-9 {
        return;
    }
    let dn = d.normalized();
    let up = if dn.z.abs() > 0.99 { Y } else { Z };
    let u = up.cross(dn).normalized();
    let v = dn.cross(u);
    prism(m, sec, a, u, v, d);
}

/// Profile swept along a polyline in the opening plane (x along, z up) with mitred corners.
/// Profile x: offset to the right of the path in that plane; profile y: across (local y).
/// A clockwise path keeps a profile with x ≥ 0 inside the outline.
pub fn elev_sweep(m: &mut Mesh, path: &[V2], closed: bool, prof: &[V2]) {
    let p = ccw(prof);
    let n = path.len();
    if p.len() < 3 || n < 2 {
        return;
    }
    let seg = |i: usize| {
        let d = path[(i + 1) % n] - path[i];
        let l = d.len();
        (l > 1e-9).then(|| v2(d.y / l, -d.x / l))
    };
    let segs = if closed { n } else { n - 1 };
    let rights: Option<Vec<V2>> = (0..segs).map(seg).collect();
    let Some(rights) = rights else { return };
    let rows: Vec<Vec<V3>> = (0..n)
        .map(|k| {
            let mv = if closed {
                miter(rights[(k + segs - 1) % segs], rights[k % segs])
            } else if k == 0 {
                rights[0]
            } else if k == n - 1 {
                rights[segs - 1]
            } else {
                miter(rights[k - 1], rights[k])
            };
            p.iter()
                .map(|q| {
                    let xy = path[k] + mv * q.x;
                    v3(xy.x, q.y, xy.y)
                })
                .collect()
        })
        .collect();
    Rows { rows, closed }.emit(m, &p);
}

/// Mitre vector between two unit right-normals: `m·a = m·b = 1`.
fn miter(a: V2, b: V2) -> V2 {
    let s = a + b;
    let d = 1.0 + a.dot(b);
    if d < 0.05 {
        // a hairpin: square corner across the first side
        return a;
    }
    s * (1.0 / d)
}

/// Round tube of radius `r` along a 3D polyline, mitred at the vertices, ends closed.
pub fn tube(m: &mut Mesh, path: &[V3], r: f64, sides: usize) {
    let path: Vec<V3> = {
        let mut v: Vec<V3> = Vec::with_capacity(path.len());
        for p in path {
            if v.last().is_none_or(|q: &V3| (*p - *q).len() > 1e-6) {
                v.push(*p);
            }
        }
        v
    };
    let n = path.len();
    if n < 2 || r <= 0.0 {
        return;
    }
    let ring = ngon(r, sides);
    let dirs: Vec<V3> = path.windows(2).map(|w| (w[1] - w[0]).normalized()).collect();
    // frames carried along the segments (no twist from one to the next)
    let mut us = Vec::with_capacity(dirs.len());
    let (mut u, _) = across(dirs[0]);
    for d in &dirs {
        let w = u - *d * u.dot(*d);
        u = if w.len() < 1e-6 { across(*d).0 } else { w.normalized() };
        us.push(u);
    }
    let rows: Vec<Vec<V3>> = (0..n)
        .map(|k| {
            let s = if k == 0 { 0 } else { k - 1 };
            let (d, u) = (dirs[s], us[s]);
            let v = d.cross(u);
            let t = if k == 0 || k == n - 1 {
                d
            } else {
                (dirs[k - 1] + dirs[k]).normalized()
            };
            ring.iter()
                .map(|q| {
                    let off = u * q.x + v * q.y;
                    let p = path[k] + off;
                    // slide along the incoming segment onto the mitre plane through the vertex
                    let den = d.dot(t);
                    if den.abs() < 1e-6 {
                        p
                    } else {
                        p + d * ((path[k] - p).dot(t) / den)
                    }
                })
                .collect()
        })
        .collect();
    Rows {
        rows,
        closed: false,
    }
    .emit(m, &ring);
}

/// Rectangle with its corners cut by `c` (a chamfered bar or a bead), CCW.
pub fn chamfered(x0: f64, y0: f64, x1: f64, y1: f64, c: f64) -> Vec<V2> {
    let c = c.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    if c < 1e-6 {
        return vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)];
    }
    vec![
        v2(x0 + c, y0),
        v2(x1 - c, y0),
        v2(x1, y0 + c),
        v2(x1, y1 - c),
        v2(x1 - c, y1),
        v2(x0 + c, y1),
        v2(x0, y1 - c),
        v2(x0, y0 + c),
    ]
}

/// Rectangle with rounded corners of radius `r`, `seg` segments per quarter, CCW.
pub fn rounded(x0: f64, y0: f64, x1: f64, y1: f64, r: f64, seg: usize) -> Vec<V2> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    if r < 1e-6 || seg == 0 {
        return chamfered(x0, y0, x1, y1, 0.0);
    }
    let mut out = vec![];
    let corners = [
        (x1 - r, y0 + r, -0.25),
        (x1 - r, y1 - r, 0.0),
        (x0 + r, y1 - r, 0.25),
        (x0 + r, y0 + r, 0.5),
    ];
    for (cx, cy, a0) in corners {
        for i in 0..=seg {
            let a = std::f64::consts::TAU * (a0 + 0.25 * i as f64 / seg as f64);
            out.push(v2(cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    // a radius of half a side makes neighbouring corners meet: one point there
    out.dedup_by(|a, b| (*a - *b).len() < 1e-9);
    while out.len() > 3 && (out[0] - out[out.len() - 1]).len() < 1e-9 {
        out.pop();
    }
    out
}

/// Plain 8-vertex block: what a placeholder looks like.
pub fn is_plain_box(m: &Mesh) -> bool {
    m.positions.len() <= 8 && m.triangles.len() <= 12
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_out(m: &Mesh) -> bool {
        m.is_watertight() && m.volume() > 0.0
    }

    #[test]
    fn prism_and_block_are_closed_and_outward() {
        let mut m = Mesh::default();
        block(&mut m, v3(0.0, 0.0, 0.0), v3(1.0, 2.0, 3.0));
        assert!(closed_out(&m));
        assert!((m.volume() - 6.0).abs() < 1e-9);
        let mut m = Mesh::default();
        prism(&mut m, &ngon(0.5, 24), v3(0.0, 0.0, 0.0), X, Z, v3(0.0, -2.0, 0.0));
        assert!(closed_out(&m), "a prism along -y still turns outward");
    }

    #[test]
    fn mitred_frame_has_the_area_of_its_outline_band() {
        // clockwise 1 x 2 outline, 0.1 wide band inside, 0.05 deep
        let path = [v2(0.0, 0.0), v2(0.0, 2.0), v2(1.0, 2.0), v2(1.0, 0.0)];
        let prof = [v2(0.0, 0.0), v2(0.1, 0.0), v2(0.1, 0.05), v2(0.0, 0.05)];
        let mut m = Mesh::default();
        elev_sweep(&mut m, &path, true, &prof);
        assert!(closed_out(&m));
        let band = 1.0 * 2.0 - 0.8 * 1.8;
        assert!((m.volume() - band * 0.05).abs() < 1e-9, "{}", m.volume());
        let lo = m.positions.iter().fold(f64::MAX, |a, p| a.min(p.x));
        assert!(lo > -1e-9, "the band stays inside the outline");
    }

    #[test]
    fn tube_is_closed_round_and_mitred() {
        let path = [v3(0.0, 0.0, 0.0), v3(2.0, 0.0, 1.0), v3(3.0, 0.0, 1.0), v3(3.0, 2.0, 1.0)];
        let mut m = Mesh::default();
        tube(&mut m, &path, 0.02, 16);
        assert!(closed_out(&m));
        let len: f64 = path.windows(2).map(|w| (w[1] - w[0]).len()).sum();
        let area = signed_area(&ngon(0.02, 16));
        assert!((m.volume() / (area * len) - 1.0).abs() < 0.02, "{}", m.volume() / (area * len));
    }
}
