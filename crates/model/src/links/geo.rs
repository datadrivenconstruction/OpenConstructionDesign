// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! UI-free link geometry: slicing a mesh with a horizontal plane (plan lines) and intersections
//! of host solids with link solids. Intersection: AABB with tolerance (broad phase, sweep along x), then
//! triangle intersection (an edge of one x a triangle of the other) or containment (the center of the
//! smaller solid inside the other's closed mesh, ray parity). Face contact is not an intersection:
//! the AABB overlap must exceed the tolerance on every axis (except thin solids, thickness <= tolerance).
//! Limitations: containment is tested at the vertex centroid (approximate for concave solids);
//! intersections of coplanar triangles are ignored.

type P3 = [f64; 3];

/// Solid: vertices (m) and triangles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub pos: Vec<P3>,
    pub tri: Vec<[u32; 3]>,
}

/// Intersection: solid indices (host, link) and the shared AABB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clash {
    pub host: usize,
    pub link: usize,
    pub min: P3,
    pub max: P3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Box3 {
    min: P3,
    max: P3,
}

impl Body {
    fn aabb(&self) -> Option<Box3> {
        let mut it = self
            .tri
            .iter()
            .flatten()
            .filter_map(|&i| self.pos.get(i as usize));
        let first = *it.next()?;
        Some(it.fold(
            Box3 {
                min: first,
                max: first,
            },
            |b, p| Box3 {
                min: lo(b.min, *p),
                max: hi(b.max, *p),
            },
        ))
    }

    fn tris(&self) -> impl Iterator<Item = [P3; 3]> + '_ {
        self.tri.iter().filter_map(|t| {
            Some([
                *self.pos.get(t[0] as usize)?,
                *self.pos.get(t[1] as usize)?,
                *self.pos.get(t[2] as usize)?,
            ])
        })
    }
}

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn lo(a: P3, b: P3) -> P3 {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])]
}

fn hi(a: P3, b: P3) -> P3 {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])]
}

fn len(a: P3) -> f64 {
    dot(a, a).sqrt()
}

fn tri_box(t: &[P3; 3]) -> Box3 {
    Box3 {
        min: lo(lo(t[0], t[1]), t[2]),
        max: hi(hi(t[0], t[1]), t[2]),
    }
}

fn boxes_meet(a: &Box3, b: &Box3, tol: f64) -> bool {
    (0..3).all(|k| a.min[k] <= b.max[k] + tol && b.min[k] <= a.max[k] + tol)
}

/// Shared AABB if the solids overlap deeper than the tolerance on every axis
/// (a thin solid at least by touching).
fn overlap(a: &Box3, b: &Box3, tol: f64) -> Option<Box3> {
    let o = Box3 {
        min: hi(a.min, b.min),
        max: lo(a.max, b.max),
    };
    let deep = (0..3).all(|k| {
        let depth = o.max[k] - o.min[k];
        let thin = (a.max[k] - a.min[k]).min(b.max[k] - b.min[k]) <= tol;
        depth > tol || (thin && depth >= -tol)
    });
    deep.then_some(o)
}

/// Segment pq intersects the triangle (boundary included); a parallel/coplanar one does not.
fn seg_tri(p: P3, q: P3, t: &[P3; 3]) -> bool {
    const EPS: f64 = 1e-9;
    let d = sub(q, p);
    let e1 = sub(t[1], t[0]);
    let e2 = sub(t[2], t[0]);
    let h = cross(d, e2);
    let det = dot(e1, h);
    let scale = len(d) * len(e1) * len(e2);
    if scale == 0.0 || det.abs() <= 1e-12 * scale {
        return false;
    }
    let f = 1.0 / det;
    let s = sub(p, t[0]);
    let u = f * dot(s, h);
    if !(-EPS..=1.0 + EPS).contains(&u) {
        return false;
    }
    let qv = cross(s, e1);
    let v = f * dot(d, qv);
    if v < -EPS || u + v > 1.0 + EPS {
        return false;
    }
    let tt = f * dot(e2, qv);
    (-EPS..=1.0 + EPS).contains(&tt)
}

fn tri_tri(a: &[P3; 3], b: &[P3; 3]) -> bool {
    (0..3).any(|k| seg_tri(a[k], a[(k + 1) % 3], b))
        || (0..3).any(|k| seg_tri(b[k], b[(k + 1) % 3], a))
}

/// Number of intersections of a ray from `p` (skewed direction, to avoid hitting edges) with the mesh.
fn inside(p: P3, body: &Body) -> bool {
    let dir = [0.546_3, 0.617_2, 0.565_8];
    let far = 1e6;
    let q = [
        p[0] + dir[0] * far,
        p[1] + dir[1] * far,
        p[2] + dir[2] * far,
    ];
    body.tris().filter(|t| seg_tri(p, q, t)).count() % 2 == 1
}

fn center(body: &Body) -> Option<P3> {
    let n = body.pos.len();
    if n == 0 {
        return None;
    }
    let c = body
        .pos
        .iter()
        .fold([0.0; 3], |c, p| [c[0] + p[0], c[1] + p[1], c[2] + p[2]]);
    Some(c.map(|v| v / n as f64))
}

fn bodies_clash(a: &Body, b: &Body, o: &Box3, tol: f64) -> bool {
    let near = |body: &Body| -> Vec<([P3; 3], Box3)> {
        body.tris()
            .map(|t| (t, tri_box(&t)))
            .filter(|(_, bx)| boxes_meet(bx, o, tol))
            .collect()
    };
    let (ta, tb) = (near(a), near(b));
    for (x, bx) in &ta {
        for (y, by) in &tb {
            if boxes_meet(bx, by, 0.0) && tri_tri(x, y) {
                return true;
            }
        }
    }
    // a solid entirely inside another: the surfaces do not intersect
    let (small, big) = if a.pos.len() <= b.pos.len() {
        (a, b)
    } else {
        (b, a)
    };
    center(small).is_some_and(|c| inside(c, big)) || center(big).is_some_and(|c| inside(c, small))
}

/// Pairs (host solid, link solid) that intersect; at most `limit`. `tol` is the contact
/// tolerance, m (e.g. 0.001).
pub fn clash(host: &[Body], link: &[Body], tol: f64, limit: usize) -> Vec<Clash> {
    let hb: Vec<Option<Box3>> = host.iter().map(Body::aabb).collect();
    let lb: Vec<Option<Box3>> = link.iter().map(Body::aabb).collect();
    let boxes = [&hb, &lb];
    // sweep along x over both sets: (left edge, 0 = host / 1 = link, index); active ones are
    // solids whose right edge is not yet left of the current left edge, so pairs are searched only among them
    let mut ev: Vec<(f64, u8, usize)> = (0..2u8)
        .flat_map(|s| {
            boxes[s as usize]
                .iter()
                .enumerate()
                .filter_map(move |(i, b)| b.map(|b| (b.min[0], s, i)))
        })
        .collect();
    ev.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut active: [Vec<usize>; 2] = [vec![], vec![]];
    let mut pairs: Vec<(usize, usize, Box3)> = vec![];
    for (x, side, k) in ev {
        let (s, other) = (side as usize, 1 - side as usize);
        let Some(me) = boxes[s][k] else { continue };
        active[other].retain(|&o| boxes[other][o].is_some_and(|b| b.max[0] + tol >= x));
        for &o in &active[other] {
            let Some(ob) = boxes[other][o] else { continue };
            let (h, l, a, b) = if s == 0 {
                (k, o, me, ob)
            } else {
                (o, k, ob, me)
            };
            if let Some(ov) = overlap(&a, &b, tol) {
                pairs.push((h, l, ov));
            }
        }
        active[s].push(k);
    }
    pairs.sort_by_key(|p| (p.0, p.1));
    let mut out = vec![];
    for (i, j, o) in pairs {
        if bodies_clash(&host[i], &link[j], &o, tol) {
            out.push(Clash {
                host: i,
                link: j,
                min: o.min,
                max: o.max,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Slice of triangles by plane z: segments [(x, y); 2]. A vertex on the plane counts as
/// above it, so a triangle lying in the plane yields no lines and a mixed one yields exactly one.
pub fn section(pos: &[P3], tri: &[[u32; 3]], z: f64) -> Vec<[[f64; 2]; 2]> {
    let mut out = vec![];
    for t in tri {
        let (Some(a), Some(b), Some(c)) = (
            pos.get(t[0] as usize),
            pos.get(t[1] as usize),
            pos.get(t[2] as usize),
        ) else {
            continue;
        };
        let p = [*a, *b, *c];
        let d = p.map(|q| q[2] - z);
        let up = d.map(|x| x >= 0.0);
        if up[0] == up[1] && up[1] == up[2] {
            continue;
        }
        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(2);
        for k in 0..3 {
            let (i, j) = (k, (k + 1) % 3);
            if up[i] != up[j] {
                let s = d[i] / (d[i] - d[j]);
                pts.push([
                    p[i][0] + (p[j][0] - p[i][0]) * s,
                    p[i][1] + (p[j][1] - p[i][1]) * s,
                ]);
            }
        }
        if let [u, v] = pts[..] {
            if (u[0] - v[0]).hypot(u[1] - v[1]) > 1e-9 {
                out.push([u, v]);
            }
        }
    }
    out
}
