//! Render source of one linked project, separate from the host scene: shapes in their own f32
//! frame around f64 anchors in the link's world, repeated bodies instanced, unique bodies batched
//! by cell. Built once per load off the UI thread; placing it (link position, host level) only
//! writes instance matrices, so host edits and link moves never rebuild link vertices.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ddc_model::links::{Body, Transform};
use ddc_render::glam::{Mat4, Vec3};
use ddc_render::{Inst, Instanced, SceneMesh};

use super::derive::{LINK_ELEMENT, LINK_TAG};
use super::load::Elem;

/// Side of the cell that batches unique bodies, m (link world).
const BATCH_CELL: f64 = 32.0;
/// Vertices in one batch at most.
const BATCH_VERTS: usize = 1 << 16;
/// Shapes equal to this step are one shape, m.
const QUANT: f64 = 1e-4;

// per thread: a test or the bench counts what its own (UI) thread did, whatever runs beside it
thread_local! {
    static BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PLACES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Render sources built on this thread (each costs a pass over all link vertices).
pub fn builds() -> u64 {
    BUILDS.with(|c| c.get())
}

/// Placements written on this thread (instance matrices only).
pub fn placements() -> u64 {
    PLACES.with(|c| c.get())
}

/// One shape and its anchors in the link world (f64): vertex = anchor + local.
pub struct Shape {
    pub mesh: Arc<SceneMesh>,
    pub places: Vec<[f64; 3]>,
}

pub struct Source {
    pub shapes: Vec<Shape>,
    /// Identity of this build: group keys differ between two loads of the same link.
    pub serial: u64,
    /// Bodies drawn by instancing (shapes with two or more anchors).
    pub instanced: usize,
    /// Triangles of all link bodies (each placement counted).
    pub tris: usize,
}

fn next_serial() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn lo(b: &Body) -> [f64; 3] {
    let mut m = [f64::INFINITY; 3];
    for p in &b.pos {
        for (mk, pk) in m.iter_mut().zip(p) {
            *mk = mk.min(*pk);
        }
    }
    m
}

fn shape_key(b: &Body, a: [f64; 3]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.pos.len().hash(&mut h);
    for p in &b.pos {
        for (v, o) in p.iter().zip(&a) {
            (((v - o) / QUANT).round() as i64).hash(&mut h);
        }
    }
    b.tri.hash(&mut h);
    h.finish()
}

fn mesh() -> SceneMesh {
    SceneMesh {
        element: LINK_ELEMENT,
        tag: LINK_TAG,
        ..Default::default()
    }
}

fn push_local(m: &mut SceneMesh, b: &Body, a: [f64; 3]) {
    let base = m.positions.len() as u32;
    m.positions.extend(b.pos.iter().map(|p| {
        [
            (p[0] - a[0]) as f32,
            (p[1] - a[1]) as f32,
            (p[2] - a[2]) as f32,
        ]
    }));
    m.triangles.extend(
        b.tri
            .iter()
            .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
    );
}

/// Builds the source of a link's bodies (link world). Deterministic for the same input.
pub fn build(elems: &[Elem]) -> Source {
    BUILDS.with(|c| c.set(c.get() + 1));
    // shape → (first body, anchors), in order of first sight
    let mut at: HashMap<u64, usize> = HashMap::new();
    let mut seen: Vec<(usize, Vec<[f64; 3]>)> = vec![];
    for (i, e) in elems.iter().enumerate() {
        if e.body.pos.is_empty() || e.body.tri.is_empty() {
            continue;
        }
        let a = lo(&e.body);
        let k = shape_key(&e.body, a);
        match at.get(&k) {
            Some(&s) => seen[s].1.push(a),
            None => {
                at.insert(k, seen.len());
                seen.push((i, vec![a]));
            }
        }
    }
    let mut shapes = vec![];
    let mut instanced = 0;
    let mut cells: BTreeMap<(i64, i64, i64), Vec<(usize, [f64; 3])>> = BTreeMap::new();
    for (i, places) in seen {
        if places.len() >= 2 {
            let mut m = mesh();
            push_local(&mut m, &elems[i].body, places[0]);
            instanced += places.len();
            shapes.push(Shape {
                mesh: Arc::new(m),
                places,
            });
        } else {
            let a = places[0];
            let c = a.map(|v| (v / BATCH_CELL).floor() as i64);
            cells.entry((c[0], c[1], c[2])).or_default().push((i, a));
        }
    }
    for (c, items) in cells {
        let a = [c.0, c.1, c.2].map(|v| v as f64 * BATCH_CELL);
        let mut m = mesh();
        for (i, _) in items {
            let b = &elems[i].body;
            if !m.positions.is_empty() && m.positions.len() + b.pos.len() > BATCH_VERTS {
                let full = std::mem::replace(&mut m, mesh());
                shapes.push(Shape {
                    mesh: Arc::new(full),
                    places: vec![a],
                });
            }
            push_local(&mut m, b, a);
        }
        if !m.positions.is_empty() {
            shapes.push(Shape {
                mesh: Arc::new(m),
                places: vec![a],
            });
        }
    }
    let tris = elems.iter().map(|e| e.body.tri.len()).sum();
    Source {
        shapes,
        serial: next_serial(),
        instanced,
        tris,
    }
}

/// Instance matrix of an anchor: link world → host (rotation about z, then shift), z from the
/// host level elevation `z0`. The sum is taken in f64; only the host-relative result is f32.
pub fn matrix(xf: &Transform, z0: f64, a: [f64; 3]) -> Mat4 {
    let t = xf.apply(a);
    Mat4::from_translation(Vec3::new(t[0] as f32, t[1] as f32, (t[2] - z0) as f32))
        * Mat4::from_rotation_z(xf.rot as f32)
}

fn group_key(link: i64, serial: u64, i: usize) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("link-source", link, serial, i).hash(&mut h);
    h.finish()
}

/// Groups of the source at a link position: shapes shared with the source (same `Arc`), new
/// instance lists only.
pub fn place(src: &Source, link: i64, xf: &Transform, z0: f64) -> Vec<Instanced> {
    PLACES.with(|c| c.set(c.get() + 1));
    src.shapes
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let inst: Vec<Inst> = s
                .places
                .iter()
                .map(|&a| Inst {
                    m: matrix(xf, z0, a),
                    element: LINK_ELEMENT,
                })
                .collect();
            Instanced::shared(
                group_key(link, src.serial, i),
                s.mesh.clone(),
                Arc::new(inst),
            )
            .with_own()
        })
        .collect()
}
