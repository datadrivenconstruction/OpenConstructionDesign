//! Planar faces filled again from their boundary loops. A face sliced and then moved inside its
//! plane (a wall side under a sloped top) keeps interior slice points at their old place; its old
//! triangles then fold over the new outline and show as spikes with missing faces. Refilling from
//! the boundary drops those points: ear clipping with hole bridges in the plane of the face,
//! projected by the dominant normal axis; collinear and duplicate points give zero-area ears.

use crate::math::{V2, V3};
use crate::mesh::{projector, Mesh};
use crate::mesh_ops::triangulate_region;
use crate::profile::signed_area;
use std::collections::{BTreeMap, HashMap, HashSet};

fn newell(pos: &[V3], l: &[u32]) -> V3 {
    let mut n = V3::default();
    for k in 0..l.len() {
        let (p, q) = (pos[l[k] as usize], pos[l[(k + 1) % l.len()] as usize]);
        n.x += (p.y - q.y) * (p.z + q.z);
        n.y += (p.z - q.z) * (p.x + q.x);
        n.z += (p.x - q.x) * (p.y + q.y);
    }
    n
}

/// Triangles of the planar region bounded by `loops` (vertex ids into `pos`). Loops wound like
/// the region's normal are outers, the others holes. Triangles keep that winding and use only
/// loop vertices. None: loops not planar, region not fillable, or the fill does not cover the
/// region exactly (area check).
pub fn fill_loops(pos: &[V3], loops: &[Vec<u32>]) -> Option<Vec<[u32; 3]>> {
    if loops.is_empty()
        || loops.iter().any(|l| l.len() < 3)
        || loops.iter().flatten().any(|&i| i as usize >= pos.len())
    {
        return None;
    }
    let n = loops
        .iter()
        .fold(V3::default(), |a, l| a + newell(pos, l));
    let len = n.len();
    if !(len > 0.0 && len.is_finite()) {
        return None;
    }
    let nh = n * (1.0 / len);
    let p0 = pos[loops[0][0] as usize];
    let ext = loops
        .iter()
        .flatten()
        .map(|&i| (pos[i as usize] - p0).len())
        .fold(0.0, f64::max)
        .max(1.0);
    if loops
        .iter()
        .flatten()
        .any(|&i| (pos[i as usize] - p0).dot(nh).abs() > 1e-7 * ext)
    {
        return None;
    }
    let proj = projector(nh);
    let rings: Vec<Vec<V2>> = loops
        .iter()
        .map(|l| l.iter().map(|&i| proj(pos[i as usize])).collect())
        .collect();
    let areas: Vec<f64> = rings.iter().map(|r| signed_area(r)).collect();
    let (outers, holes): (Vec<usize>, Vec<usize>) =
        (0..loops.len()).partition(|&k| areas[k] > 0.0);
    // each hole goes to the smallest outer around one of its points off the outer's boundary
    let mut held: Vec<Vec<usize>> = vec![vec![]; outers.len()];
    for &h in &holes {
        let at = crate::poly::centroid(&rings[h]);
        let probe = std::iter::once(at).chain(rings[h].iter().copied());
        let k = probe
            .filter_map(|p| {
                outers
                    .iter()
                    .enumerate()
                    .filter(|&(_, &o)| crate::predicates::point_in_ring(p, &rings[o]) > 0)
                    .min_by(|a, b| areas[*a.1].total_cmp(&areas[*b.1]))
                    .map(|(k, _)| k)
            })
            .next()?;
        held[k].push(h);
    }
    let mut out = vec![];
    let mut want = 0.0;
    for (k, &o) in outers.iter().enumerate() {
        let hs: Vec<Vec<V2>> = held[k].iter().map(|&h| rings[h].clone()).collect();
        let ids: Vec<u32> = loops[o]
            .iter()
            .chain(held[k].iter().flat_map(|&h| loops[h].iter()))
            .copied()
            .collect();
        let tris = triangulate_region(&rings[o], &hs)?;
        want += areas[o] + held[k].iter().map(|&h| areas[h]).sum::<f64>();
        out.extend(tris.into_iter().map(|t| t.map(|i| ids[i])));
    }
    // exact cover: no folded triangle and the signed sum equals the region
    let eps = 1e-9 * ext * ext;
    let mut got = 0.0;
    for t in &out {
        let [a, b, c] = t.map(|i| proj(pos[i as usize]));
        let s = (b - a).cross(c - a) * 0.5;
        if s < -eps {
            return None;
        }
        got += s;
    }
    ((got - want).abs() <= eps.max(1e-9 * want.abs())).then_some(out)
}

/// Boundary loops of a triangle set: directed edges without a matching reverse edge, chained.
/// None for a non-manifold boundary (a vertex with two outgoing boundary edges).
fn boundary_loops(tris: &[[u32; 3]]) -> Option<Vec<Vec<u32>>> {
    let mut net = HashMap::<(u32, u32), i32>::new();
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if a != b {
                *net.entry((a.min(b), a.max(b))).or_default() += if a < b { 1 } else { -1 };
            }
        }
    }
    let mut next = BTreeMap::<u32, u32>::new();
    for (&(a, b), &v) in &net {
        let (p, q) = match v {
            0 => continue,
            1 => (a, b),
            -1 => (b, a),
            _ => return None,
        };
        if next.insert(p, q).is_some() {
            return None;
        }
    }
    let mut loops = vec![];
    while let Some((&s, _)) = next.iter().next() {
        let mut l = vec![s];
        let mut cur = next.remove(&s)?;
        while cur != s {
            l.push(cur);
            cur = next.remove(&cur)?;
        }
        loops.push(l);
    }
    Some(loops)
}

/// Fills again every planar face (by `face_of`) that has a vertex flagged in `touched`; drops
/// positions from index `from` on that no triangle uses any more (earlier ones keep their index).
/// Faces that are not planar or do not fill stay as they are. Returns the faces filled again.
pub fn refill_faces(mesh: &mut Mesh, touched: &[bool], from: usize) -> usize {
    if mesh.face_of.len() != mesh.triangles.len() {
        return 0;
    }
    let mut by_face = BTreeMap::<u32, Vec<[u32; 3]>>::new();
    for (t, &f) in mesh.triangles.iter().zip(&mesh.face_of) {
        by_face.entry(f).or_default().push(*t);
    }
    let hit = |i: &u32| touched.get(*i as usize).copied().unwrap_or(false);
    let mut fresh = HashMap::<u32, Vec<[u32; 3]>>::new();
    for (f, ts) in &by_face {
        if !ts.iter().flatten().any(hit) {
            continue;
        }
        let Some(loops) = boundary_loops(ts) else {
            continue;
        };
        if let Some(t) = fill_loops(&mesh.positions, &loops) {
            fresh.insert(*f, t);
        }
    }
    if fresh.is_empty() {
        return 0;
    }
    let old_t = std::mem::take(&mut mesh.triangles);
    let old_f = std::mem::take(&mut mesh.face_of);
    let mut done = HashSet::<u32>::new();
    for (t, f) in old_t.into_iter().zip(old_f) {
        match fresh.get(&f) {
            None => {
                mesh.triangles.push(t);
                mesh.face_of.push(f);
            }
            Some(n) => {
                if done.insert(f) {
                    mesh.triangles.extend_from_slice(n);
                    mesh.face_of.extend(std::iter::repeat(f).take(n.len()));
                }
            }
        }
    }
    compact(mesh, from);
    fresh.len()
}

/// Removes positions from `from` on that no triangle refers to, keeping the order of the rest.
fn compact(mesh: &mut Mesh, from: usize) {
    let mut used: Vec<bool> = (0..mesh.positions.len()).map(|i| i < from).collect();
    for &i in mesh.triangles.iter().flatten() {
        used[i as usize] = true;
    }
    if used.iter().all(|&u| u) {
        return;
    }
    let mut map = vec![u32::MAX; used.len()];
    let mut kept = Vec::with_capacity(used.len());
    for (i, p) in mesh.positions.iter().enumerate() {
        if used[i] {
            map[i] = kept.len() as u32;
            kept.push(*p);
        }
    }
    mesh.positions = kept;
    for t in &mut mesh.triangles {
        *t = t.map(|i| map[i as usize]);
    }
}

#[cfg(test)]
#[path = "face_fill_tests.rs"]
mod tests;
