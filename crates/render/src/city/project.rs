//! A project in the city: any model source with an f64 origin and spatial units that load on
//! demand. Content comes in project-local coordinates and element ids; `place` moves it next to
//! the federation anchor and onto the global element range of the project.

use crate::chunks::{Inst, Instanced};
use crate::scene::{mesh_box, Aabb, SceneMesh};
use glam::{Mat4, Vec3};
use std::sync::Arc;

/// A spatial unit of a project (a building, a block of a large model).
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// Project-local bounds.
    pub bounds: Aabb,
    /// Project-local element ids of the unit, `first..end`.
    pub elements: std::ops::Range<u32>,
    /// Expected `Content::bytes` once loaded (0 — unknown); the memory budget asks before loading.
    pub bytes: u64,
}

/// What a project hands over: meshes and instanced groups (prototype meshes may be shared by
/// every project, they upload once).
#[derive(Clone, Debug, Default)]
pub struct Content {
    pub meshes: Vec<Arc<SceneMesh>>,
    pub groups: Vec<Instanced>,
}

pub trait Project: Send + Sync {
    /// World position of the project's local zero (metres, f64: city coordinates are large).
    fn origin(&self) -> [f64; 3];
    fn units(&self) -> Vec<Unit>;
    /// Loads one unit (`units()[unit]`); may take long, runs on a worker thread.
    fn load(&self, unit: usize) -> Content;
}

/// Bytes of loaded content on the CPU: own meshes and placements. Prototypes are shared by many
/// units and projects, they are not charged to one unit.
pub fn content_bytes(meshes: &[(Arc<SceneMesh>, Aabb)], groups: &[Instanced]) -> u64 {
    let m: u64 = meshes.iter().map(|(m, _)| crate::city::hlod::mesh_bytes(m)).sum();
    let i: usize = groups.iter().map(|g| g.inst.len()).sum();
    m + (i * std::mem::size_of::<Inst>()) as u64
}

/// Content placed in anchor coordinates with global element ids, boxes computed once.
#[derive(Clone, Debug, Default)]
pub struct Placed {
    pub meshes: Vec<(Arc<SceneMesh>, Aabb)>,
    pub groups: Vec<Instanced>,
    pub bytes: u64,
    pub triangles: usize,
}

/// Moves `c` by `off` and its element ids by `base`; group keys become `salt`-unique.
pub fn place(c: Content, off: Vec3, base: u32, salt: u64) -> Placed {
    let meshes: Vec<(Arc<SceneMesh>, Aabb)> = c
        .meshes
        .into_iter()
        .map(|m| {
            let mut n = Arc::unwrap_or_clone(m);
            n.element = n.element.wrapping_add(base);
            if off != Vec3::ZERO {
                for p in &mut n.positions {
                    *p = (Vec3::from(*p) + off).into();
                }
            }
            let b = mesh_box(&n);
            (Arc::new(n), b)
        })
        .collect();
    let t = Mat4::from_translation(off);
    let groups: Vec<Instanced> = c
        .groups
        .into_iter()
        .enumerate()
        .map(|(gi, g)| {
            let inst: Vec<Inst> = g
                .inst
                .iter()
                .map(|i| Inst {
                    m: t * i.m,
                    element: i.element.wrapping_add(base),
                })
                .collect();
            let key = mix(salt ^ mix(g.key.wrapping_add(gi as u64)));
            Instanced::new(key, g.proto.clone(), inst)
                .with_lods(g.lods.clone())
                .with_approx(g.approx)
        })
        .collect();
    let triangles = meshes.iter().map(|(m, _)| m.triangles.len()).sum::<usize>()
        + groups
            .iter()
            .map(|g| g.proto.triangles.len() * g.inst.len())
            .sum::<usize>();
    let bytes = content_bytes(&meshes, &groups);
    Placed {
        meshes,
        groups,
        bytes,
        triangles,
    }
}

/// 64-bit finaliser (splitmix): keys and seeds.
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Offset of a project from the anchor, rounded once to f32 (the subtraction is in f64).
pub fn offset(origin: [f64; 3], anchor: [f64; 3]) -> Vec3 {
    Vec3::new(
        (origin[0] - anchor[0]) as f32,
        (origin[1] - anchor[1]) as f32,
        (origin[2] - anchor[2]) as f32,
    )
}
