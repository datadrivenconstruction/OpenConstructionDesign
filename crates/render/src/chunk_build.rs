// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Off-frame chunk vertex building: worker threads turn shared `Arc<SceneMesh>` into flat
//! vertices, the frame thread only uploads the finished data to the GPU within the budget. The queue is short
//! (order is set by frame visibility), stale results are dropped by the receiver using the fingerprint.

use crate::chunks::{crease_vertices, set_cat, tri_vertices, Inst};
use crate::scene::{SceneMesh, Vertex};
use crate::vpack::{bounds, pack_lines, pack_tris, Deq, Stream};
use glam::Vec3;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Chunk mesh, category number and placement of a rare body (vertices are computed during the build,
/// there is no CPU copy of the mesh).
pub(crate) type MeshItem = (Arc<SceneMesh>, u8, Option<Inst>);
pub(crate) type MeshList = Arc<Vec<MeshItem>>;

/// Mesh edges keyed by content fingerprint: editing a neighbour rebuilds the chunk, but the edge dictionary
/// of unchanged bodies is not rebuilt. The oldest entries beyond the budget are evicted.
struct Creases {
    map: std::collections::HashMap<u64, Arc<Vec<Vertex>>>,
    order: VecDeque<u64>,
    bytes: usize,
}

const CREASE_BUDGET: usize = 96 << 20;
/// Below this size the edge dictionary is cheaper than a fingerprint and a cache entry.
const CREASE_CACHE_MIN_TRIS: usize = 24;

static CREASES: Mutex<Option<Creases>> = Mutex::new(None);

pub(crate) fn crease_cached(m: &SceneMesh, out: &mut Vec<Vertex>) {
    if m.triangles.len() < CREASE_CACHE_MIN_TRIS {
        crease_vertices(m, out);
        return;
    }
    let key = crate::chunks::mesh_fp(m);
    let hit = CREASES
        .lock()
        .ok()
        .and_then(|c| c.as_ref().and_then(|c| c.map.get(&key).cloned()));
    if let Some(v) = hit {
        out.extend_from_slice(&v);
        return;
    }
    let mut v = Vec::new();
    crease_vertices(m, &mut v);
    out.extend_from_slice(&v);
    let size = v.len() * std::mem::size_of::<Vertex>() + 64;
    if size > CREASE_BUDGET / 4 {
        return;
    }
    let Ok(mut g) = CREASES.lock() else { return };
    let c = g.get_or_insert_with(|| Creases {
        map: Default::default(),
        order: VecDeque::new(),
        bytes: 0,
    });
    if c.map.insert(key, Arc::new(v)).is_none() {
        c.order.push_back(key);
        c.bytes += size;
    }
    while c.bytes > CREASE_BUDGET {
        let Some(k) = c.order.pop_front() else { break };
        if let Some(old) = c.map.remove(&k) {
            c.bytes -= old.len() * std::mem::size_of::<Vertex>() + 64;
        }
    }
}

pub(crate) fn item_element(m: &MeshItem) -> u32 {
    m.2.map_or(m.0.element, |i| i.element)
}

fn place_vertices(v: &mut [Vertex], i: &Inst) {
    for x in v {
        x.pos = i.m.transform_point3(Vec3::from(x.pos)).into();
        x.normal =
            i.m.transform_vector3(Vec3::from(x.normal))
                .normalize_or_zero()
                .into();
        x.element = i.element;
    }
}

/// Compressed chunk vertices from origin `deq`; for glass — triangle centres for sorting.
pub(crate) struct Built {
    pub deq: Deq,
    pub tri: Stream,
    pub lines: Stream,
    pub centers: Vec<Vec3>,
}

impl Built {
    pub fn bytes(&self) -> u64 {
        self.tri.bytes() + self.lines.bytes()
    }
}

/// `crease_limit` — a mesh with many triangles goes without crease edges (the background
/// build does not spend minutes on the edge dictionary of a single body).
/// `last` — the provoking vertex of a triangle is the last (GL).
pub(crate) fn build_meshes(
    list: &[MeshItem],
    glass: bool,
    crease_limit: usize,
    last: bool,
) -> Built {
    let tris: usize = list.iter().map(|m| m.0.triangles.len()).sum();
    let mut v = Vec::with_capacity(tris * 3);
    let mut l = vec![];
    for (m, cat, place) in list {
        let (v0, l0) = (v.len(), l.len());
        tri_vertices(m, &mut v);
        if m.triangles.len() <= crease_limit {
            crease_cached(m, &mut l);
        }
        if let Some(i) = place {
            place_vertices(&mut v[v0..], i);
            place_vertices(&mut l[l0..], i);
        }
        set_cat(&mut v[v0..], *cat);
        set_cat(&mut l[l0..], *cat);
    }
    let centers = if glass {
        v.chunks_exact(3)
            .map(|t: &[Vertex]| {
                (Vec3::from(t[0].pos) + Vec3::from(t[1].pos) + Vec3::from(t[2].pos)) / 3.0
            })
            .collect()
    } else {
        vec![]
    };
    let mut b = bounds(&v);
    b.union(&bounds(&l));
    let deq = Deq::of(&b);
    Built {
        deq,
        tri: pack_tris(&v, deq, glass, last),
        lines: pack_lines(&l, deq),
        centers,
    }
}

/// Prototype mesh from the `deq` shared by its levels: edges — only on the exact level.
pub(crate) fn build_proto(
    m: &SceneMesh,
    cat: u8,
    lines: bool,
    deq: Deq,
    last: bool,
) -> (Stream, Stream) {
    let (mut v, mut l) = (vec![], vec![]);
    tri_vertices(m, &mut v);
    if lines {
        crease_cached(m, &mut l);
    }
    set_cat(&mut v, cat);
    set_cat(&mut l, cat);
    (pack_tris(&v, deq, false, last), pack_lines(&l, deq))
}

pub(crate) struct ProtoJob {
    /// Key of a prototype level in memory accounting.
    pub key: u64,
    pub mesh: Arc<SceneMesh>,
    pub cat: u8,
    pub lines: bool,
    pub deq: Deq,
    pub last: bool,
}

pub(crate) enum Job {
    Meshes {
        key: u64,
        stamp: u64,
        list: MeshList,
        glass: bool,
        crease_limit: usize,
        last: bool,
    },
    Protos(Vec<ProtoJob>),
}

pub(crate) enum Done {
    Meshes { key: u64, stamp: u64, built: Built },
    Protos(Vec<(u64, Stream, Stream)>),
}

fn run(job: Job) -> Done {
    match job {
        Job::Meshes {
            key,
            stamp,
            list,
            glass,
            crease_limit,
            last,
        } => Done::Meshes {
            key,
            stamp,
            built: build_meshes(&list, glass, crease_limit, last),
        },
        Job::Protos(v) => Done::Protos(
            v.into_iter()
                .map(|j| {
                    let (a, b) = build_proto(&j.mesh, j.cat, j.lines, j.deq, j.last);
                    (j.key, a, b)
                })
                .collect(),
        ),
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    done: Mutex<Vec<Done>>,
    stop: AtomicBool,
}

/// Worker threads; stopped on `Drop`. Without threads (failed to start) tasks
/// run in place.
pub(crate) struct Workers {
    shared: Arc<Shared>,
    alive: usize,
    /// Joined on drop: a thread still running at process exit is torn down mid-flight.
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn worker(s: Arc<Shared>) {
    loop {
        let job = {
            let Ok(mut q) = s.queue.lock() else {
                return;
            };
            loop {
                if s.stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(j) = q.pop_front() {
                    break j;
                }
                q = match s.wake.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        let d = run(job);
        if let Ok(mut out) = s.done.lock() {
            out.push(d);
        }
        crate::perf::wake();
    }
}

impl Workers {
    pub fn new() -> Self {
        let n = crate::chunks::worker_count();
        let shared = Arc::new(Shared::default());
        let mut threads = Vec::with_capacity(n);
        for i in 0..n {
            let s = shared.clone();
            let t = std::thread::Builder::new()
                .name(format!("ddc-chunks-{i}"))
                .spawn(move || worker(s));
            if let Ok(t) = t {
                threads.push(t);
            }
        }
        Self {
            shared,
            alive: threads.len(),
            threads,
        }
    }

    /// Tasks in flight at once: the queue is short, the next frame decides the order.
    pub fn max_jobs(&self) -> usize {
        self.alive * 2 + 2
    }

    pub fn push(&self, job: Job) {
        if self.alive == 0 {
            let d = run(job);
            if let Ok(mut out) = self.shared.done.lock() {
                out.push(d);
            }
            crate::perf::wake();
            return;
        }
        if let Ok(mut q) = self.shared.queue.lock() {
            q.push_back(job);
            self.shared.wake.notify_one();
        }
    }

    pub fn take_done(&self) -> Vec<Done> {
        self.shared
            .done
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        {
            // the queue is held during notification: the thread cannot fall asleep between the check and the wait
            let _guard = self.shared.queue.lock();
            self.shared.wake.notify_all();
        }
        // a worker ends after its current job; the queue is not drained
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use web_time::{Duration, Instant};

    fn tri() -> Arc<SceneMesh> {
        Arc::new(SceneMesh {
            element: 1,
            tag: 0,
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            triangles: vec![[0, 1, 2]],
        })
    }

    /// Terrain of n×n quads: many creases, the fingerprint depends on `seed`.
    fn relief(n: u32, seed: u32) -> SceneMesh {
        let h = |x: u32, y: u32| (((x * 7 + y * 13 + seed) % 5) as f32) * 0.3;
        let positions = (0..=n)
            .flat_map(|y| (0..=n).map(move |x| [x as f32, y as f32, h(x, y)]))
            .collect();
        let at = |x: u32, y: u32| y * (n + 1) + x;
        let triangles = (0..n)
            .flat_map(|y| {
                (0..n).flat_map(move |x| {
                    [
                        [at(x, y), at(x + 1, y), at(x + 1, y + 1)],
                        [at(x, y), at(x + 1, y + 1), at(x, y + 1)],
                    ]
                })
            })
            .collect();
        SceneMesh {
            element: 5 + seed,
            tag: 0,
            positions,
            triangles,
        }
    }

    fn sorted(mut v: Vec<Vertex>) -> Vec<[u32; 8]> {
        let mut rows: Vec<[u32; 8]> = v
            .drain(..)
            .map(|x| {
                let p = x.pos.map(f32::to_bits);
                let n = x.normal.map(f32::to_bits);
                [p[0], p[1], p[2], n[0], n[1], n[2], x.element, x.tag]
            })
            .collect();
        rows.sort_unstable();
        rows
    }

    #[test]
    fn crease_cache_returns_the_same_edges_and_skips_the_rebuild() {
        let m = relief(120, 1);
        let mut want = vec![];
        let t0 = web_time::Instant::now();
        crease_vertices(&m, &mut want);
        let cold = t0.elapsed();
        assert!(want.len() > 1000);
        let mut first = vec![Vertex {
            pos: [9.0; 3],
            normal: [0.0; 3],
            element: 0,
            tag: 0,
        }];
        crease_cached(&m, &mut first);
        assert_eq!(first.len(), want.len() + 1, "appends after what is there");
        let t1 = web_time::Instant::now();
        let mut again = vec![];
        crease_cached(&m, &mut again);
        let warm = t1.elapsed();
        assert_eq!(sorted(again.clone()), sorted(want.clone()));
        assert_eq!(sorted(first[1..].to_vec()), sorted(want));
        // different content — a different key: its own edges and its own element number
        let other = relief(120, 2);
        let mut o = vec![];
        crease_cached(&other, &mut o);
        let mut o_want = vec![];
        crease_vertices(&other, &mut o_want);
        assert_eq!(sorted(o), sorted(o_want));
        eprintln!("creases 28.8k tris: built {cold:?}, cached {warm:?}");
    }

    #[test]
    fn crease_limit_only_drops_lines() {
        let list = vec![(tri(), 2u8, None)];
        let a = build_meshes(&list, true, usize::MAX, false);
        assert_eq!((a.tri.count(), a.tri.v.len()), (3, 3));
        assert_eq!(a.lines.count(), 6, "open triangle: three boundary edges");
        assert_eq!(a.lines.v.len(), 3, "edge ends are shared");
        assert_eq!(a.centers.len(), 1);
        assert!(a.tri.v.iter().all(|x| (x.tag >> 24) & 15 == 2));
        let b = build_meshes(&list, false, 0, false);
        assert_eq!(b.tri.count(), 3);
        assert!(b.lines.count() == 0 && b.centers.is_empty());
    }

    #[test]
    fn placed_item_is_moved_and_carries_its_element() {
        let i = Inst {
            m: glam::Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))
                * glam::Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
            element: 77,
        };
        let b = build_meshes(&[(tri(), 0u8, Some(i))], false, usize::MAX, false);
        assert!(b.tri.v.iter().chain(&b.lines.v).all(|x| x.element == 77));
        let at = |k: u32| {
            let v = b.tri.v[b.tri.vertex_of(k) as usize];
            (b.deq.pos(crate::vpack::unpack_q(v.q)), v.n)
        };
        let (p, _) = at(1);
        assert!((p - Vec3::new(10.0, 1.0, 0.0)).length() < 1e-5, "{p}");
        let n = crate::vpack::unoct(at(0).1);
        assert!((n - Vec3::Z).length() < 1e-4, "{n}");
        assert_eq!(item_element(&(tri(), 0u8, Some(i))), 77);
        assert_eq!(item_element(&(tri(), 0u8, None)), 1);
    }

    #[test]
    fn dropped_workers_have_ended_their_threads() {
        let w = Workers::new();
        let shared = w.shared.clone();
        w.push(Job::Protos(vec![ProtoJob {
            key: 1,
            mesh: tri(),
            cat: 0,
            lines: true,
            deq: Deq::UNIT,
            last: false,
        }]));
        drop(w);
        // every thread held a clone: only this one is left once they are joined
        assert_eq!(Arc::strong_count(&shared), 1);
    }

    #[test]
    fn workers_return_every_job() {
        let w = Workers::new();
        assert!(w.max_jobs() >= 2);
        for k in 0..16u64 {
            w.push(Job::Protos(vec![ProtoJob {
                key: k,
                mesh: tri(),
                cat: 0,
                lines: k % 2 == 0,
                deq: Deq::UNIT,
                last: false,
            }]));
        }
        let t = Instant::now();
        let mut got = vec![];
        while got.len() < 16 && t.elapsed() < Duration::from_secs(20) {
            for d in w.take_done() {
                if let Done::Protos(v) = d {
                    got.extend(v.into_iter().map(|x| (x.0, x.2.count())));
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        got.sort_unstable();
        assert_eq!(got.len(), 16);
        assert_eq!(got[1], (1, 0), "no lines were asked for");
        assert_eq!(got[2], (2, 6));
    }
}
