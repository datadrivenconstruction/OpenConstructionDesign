//! Cooking a project: every unit is loaded once, gets its proxy chain, and a quadtree of group
//! proxies is built over the units (never across projects). Loaded content is dropped after the
//! cook; only proxies stay resident.

use super::hlod::{from_proxies, opaque_tag, unit_chain, Label, Proxy, Source};
use super::project::{place, Project};
use crate::scene::Aabb;
use glam::Vec3;
use web_time::Instant;

/// Units under one leaf node at most.
pub const LEAF_UNITS: usize = 4;
/// Group proxies keep per-unit labels (pick selects the building) up to this many units below.
pub const ID_KEEP: usize = 64;
/// Voxels along the larger horizontal side of a group node.
pub const NODE_DIV: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kid {
    Node(u32),
    Unit(u32),
}

#[derive(Debug)]
pub struct Node {
    pub bounds: Aabb,
    pub proxy: Proxy,
    pub kids: Vec<Kid>,
    /// Units below (for labels and stats).
    pub units: usize,
    /// Refined into its kids last frame (hysteresis state).
    pub open: bool,
}

#[derive(Debug)]
pub struct UnitLod {
    /// Anchor-coordinate bounds of the content.
    pub bounds: Aabb,
    /// Proxies fine to coarse; never empty for a unit with geometry.
    pub levels: Vec<Proxy>,
    /// Shown level last frame: index into `levels`, -1 = full content.
    pub cur: i32,
    /// First global element (labels of its proxies).
    pub root: u32,
    pub bytes_hint: u64,
    pub triangles: usize,
}

impl UnitLod {
    pub fn coarsest(&self) -> Option<&Proxy> {
        self.levels.last()
    }
}

#[derive(Debug)]
pub struct Cooked {
    pub units: Vec<UnitLod>,
    pub nodes: Vec<Node>,
    pub root: u32,
    pub proxy_bytes: u64,
    pub proxy_triangles: usize,
    /// Triangles of all full content (instances expanded).
    pub source_triangles: usize,
    pub cook_ms: f64,
}

impl Cooked {
    pub fn root_node(&self) -> &Node {
        &self.nodes[self.root as usize]
    }
}

/// Builds the proxies of one project placed at `off` with element base `base`.
pub fn cook(project: &dyn Project, off: Vec3, base: u32, salt: u64) -> Cooked {
    let t = Instant::now();
    let infos = project.units();
    let mut units = Vec::with_capacity(infos.len());
    let mut source_triangles = 0;
    for (ui, info) in infos.iter().enumerate() {
        let placed = place(project.load(ui), off, base, salt ^ ui as u64);
        let root = info.elements.start.wrapping_add(base);
        let src = Source {
            meshes: &placed.meshes,
            groups: &placed.groups,
        };
        let levels = unit_chain(&src, &|_, tag| Label {
            element: root,
            tag: opaque_tag(tag),
        });
        let mut bounds = src.bounds();
        if bounds.is_empty() {
            bounds = Aabb {
                min: info.bounds.min + off,
                max: info.bounds.max + off,
            };
        }
        source_triangles += placed.triangles;
        units.push(UnitLod {
            bounds,
            cur: levels.len() as i32 - 1,
            levels,
            root,
            bytes_hint: if info.bytes > 0 { info.bytes } else { placed.bytes },
            triangles: placed.triangles,
        });
    }
    let mut nodes = vec![];
    let all: Vec<u32> = (0..units.len() as u32).collect();
    let root = build(&units, all, &mut nodes);
    let proxy_bytes = nodes.iter().map(|n| n.proxy.bytes).sum::<u64>()
        + units
            .iter()
            .flat_map(|u| u.levels.iter())
            .map(|p| p.bytes)
            .sum::<u64>();
    let proxy_triangles = nodes.iter().map(|n| n.proxy.triangles).sum::<usize>()
        + units
            .iter()
            .flat_map(|u| u.levels.iter())
            .map(|p| p.triangles)
            .sum::<usize>();
    Cooked {
        units,
        nodes,
        root,
        proxy_bytes,
        proxy_triangles,
        source_triangles,
        cook_ms: t.elapsed().as_secs_f64() * 1000.0,
    }
}

fn center(b: &Aabb) -> Vec3 {
    (b.min + b.max) * 0.5
}

/// Quadtree over unit centres; returns the node index (children are built first).
fn build(units: &[UnitLod], ids: Vec<u32>, nodes: &mut Vec<Node>) -> u32 {
    let mut bounds = Aabb::EMPTY;
    ids.iter()
        .for_each(|&i| bounds.union(&units[i as usize].bounds));
    let kids: Vec<Kid> = if ids.len() <= LEAF_UNITS {
        ids.iter().map(|&i| Kid::Unit(i)).collect()
    } else {
        let mid = center(&bounds);
        let mut q: [Vec<u32>; 4] = Default::default();
        for &i in &ids {
            let c = center(&units[i as usize].bounds);
            q[(c.x >= mid.x) as usize + 2 * (c.y >= mid.y) as usize].push(i);
        }
        if q.iter().filter(|v| !v.is_empty()).count() < 2 {
            // all centres in one quadrant (stacked units): split the list itself
            let n = ids.len().div_ceil(4);
            q = Default::default();
            for (k, c) in ids.chunks(n).enumerate() {
                q[k] = c.to_vec();
            }
        }
        q.into_iter()
            .filter(|v| !v.is_empty())
            .map(|v| Kid::Node(build(units, v, nodes)))
            .collect()
    };
    let count = ids.len();
    let first_root = ids
        .iter()
        .map(|&i| units[i as usize].root)
        .min()
        .unwrap_or(0);
    let parts: Vec<&Proxy> = kids
        .iter()
        .filter_map(|k| match *k {
            Kid::Node(n) => Some(&nodes[n as usize].proxy),
            Kid::Unit(u) => units[u as usize].coarsest(),
        })
        .collect();
    let ext = (bounds.max - bounds.min).truncate().max_element();
    let finest = parts.iter().map(|p| p.step).fold(0.0f32, f32::max);
    let h = (ext / NODE_DIV).max(finest * 2.0).max(0.25);
    let keep = count <= ID_KEEP;
    let proxy = if parts.is_empty() || bounds.is_empty() {
        Proxy::default()
    } else {
        from_proxies(&parts, &bounds, h, &|l: Label| {
            if keep {
                l
            } else {
                Label {
                    element: first_root,
                    tag: l.tag,
                }
            }
        })
    };
    nodes.push(Node {
        bounds,
        proxy,
        kids,
        units: count,
        open: false,
    });
    nodes.len() as u32 - 1
}
