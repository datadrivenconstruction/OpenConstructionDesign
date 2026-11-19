// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mesh cache (`mesh_geo`/`mesh_proto`) in the 3D scene: placements are drawn as instances, each with its own
//! pick number. Mapping rule: a row on the active level (or with no level) gets a slot
//! `Target(Cat::Mesh, id)` in the same range as provider parts, so selection, hiding and
//! categories work through `scene3d::target_of`; a row on another level is visible but not selectable.
//!
//! A mesh changed after the cache was built leaves the instances for the ordinary `mesh` provider
//! (`Editor::big.instanced_meshes` lists whom the provider skips) until the next cache build.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use ddc_model::{
    read_instance_set_file, read_proto_levels_file, Changes, InstProto, InstRow, InstanceSet, Model,
};
use ddc_render::glam::Mat4;
use ddc_render::{Inst, Instanced, SceneMesh};

use super::scene3d::TAG_MESH;
use crate::editor::levels::OTHER_LEVEL;
use crate::editor::Editor;
use crate::shell::EditorApp;

/// Prototype geometry held in process memory; beyond the limit only the coarse level stays.
const CPU_BUDGET: u64 = 1 << 30;
/// How many exact meshes are loaded at a time and how many are held in total beyond `CPU_BUDGET`.
const FINE_BATCH: usize = 32;
const FINE_BUDGET: u64 = 1 << 30;
/// With more rows, reading happens in the background; mesh numbers are read immediately, otherwise the provider would process them all.
const SYNC_ROWS: usize = 50_000;
const KEY_BIT: u64 = 1 << 63;

type GroupKey = (i64, bool);

/// What the placement numbers and z depend on.
#[derive(Clone, Debug, PartialEq)]
pub struct MapKey {
    pub active: i64,
    pub z0: f64,
    pub levels: Vec<i64>,
    /// Reset counter of the scene slot map.
    pub epoch: u64,
}

/// Rows to placements by group (prototype, active level). `slot` is the mesh pick number.
pub fn map_rows(
    rows: &[InstRow],
    removed: &HashSet<i64>,
    key: &MapKey,
    slot: &mut dyn FnMut(i64) -> u32,
) -> BTreeMap<GroupKey, Vec<Inst>> {
    let mut out: BTreeMap<GroupKey, Vec<Inst>> = BTreeMap::new();
    for r in rows {
        if removed.contains(&r.mesh) {
            continue;
        }
        let active = match r.level {
            None => true,
            Some(l) if l == key.active => true,
            Some(l) if key.levels.contains(&l) => false,
            // the provider does not draw such a row either
            Some(_) => continue,
        };
        let element = if active { slot(r.mesh) } else { OTHER_LEVEL };
        let mut cols = r.place.cols();
        // z from f64: the difference is more precise than a difference of f32-rounded values
        cols[14] = (r.place.t[2] - key.z0) as f32;
        out.entry((r.proto, active)).or_default().push(Inst {
            m: Mat4::from_cols_array(&cols),
            element,
        });
    }
    out
}

/// Rows of prototype `proto` in a set sorted by prototype.
fn rows_of(rows: &[InstRow], proto: i64) -> &[InstRow] {
    let a = rows.partition_point(|r| r.proto < proto);
    let b = rows.partition_point(|r| r.proto <= proto);
    &rows[a..b]
}

fn group_key((proto, active): GroupKey) -> u64 {
    KEY_BIT | (proto as u64) << 1 | u64::from(active)
}

/// Prototype of a cache group; groups of other providers have none.
fn proto_of_key(k: u64) -> Option<i64> {
    (k & KEY_BIT != 0).then_some(((k & !KEY_BIT) >> 1) as i64)
}

struct ProtoMeshes {
    full: Arc<SceneMesh>,
    lods: Vec<(Arc<SceneMesh>, f32)>,
    /// Deviation of `full` from the true shape, m; nonzero means the exact one did not fit in memory.
    approx: f32,
}

fn proto_meshes(p: InstProto) -> Option<ProtoMeshes> {
    let mut levels = p.levels.into_iter().map(|l| {
        let err = l.err;
        let m = SceneMesh {
            element: 0,
            tag: TAG_MESH,
            positions: l.positions,
            triangles: l.triangles,
        };
        (Arc::new(m), err)
    });
    let (full, approx) = levels.next()?;
    Some(ProtoMeshes {
        full,
        lods: levels.collect(),
        approx,
    })
}

/// Feed state; lives in `scene3d::Cache`. Does nothing without `enable` (one-shot builds).
#[derive(Default)]
pub struct Feed {
    on: bool,
    rx: Option<mpsc::Receiver<Result<InstanceSet, String>>>,
    rows: Vec<InstRow>,
    protos: HashMap<i64, ProtoMeshes>,
    /// Changed after the set was read.
    removed: HashSet<i64>,
    /// Prototypes whose groups must be rebuilt.
    touched: HashSet<i64>,
    groups: BTreeMap<GroupKey, Instanced>,
    mapped: Option<MapKey>,
    /// The cache was rebuilt: re-read on the next scene build.
    stale: bool,
    /// Background read failed: return the meshes to the provider.
    failed: bool,
    /// Changed during the background read: the set may have read them stale.
    since: HashSet<i64>,
    /// Loading of exact meshes; ones requested a second time are not re-read.
    fine_rx: Option<mpsc::Receiver<Result<Vec<InstProto>, String>>>,
    fine_asked: HashSet<i64>,
    fine_bytes: u64,
}

impl Feed {
    /// Enables the feed; `DDC_NO_MESH_INSTANCES` keeps the old path through the provider.
    pub fn enable(&mut self) {
        self.on = std::env::var_os("DDC_NO_MESH_INSTANCES").is_none();
    }

    pub fn invalidate(&mut self) {
        self.stale = self.on;
    }

    pub fn loading(&self) -> bool {
        self.rx.is_some()
    }

    fn clear(&mut self) {
        self.rx = None;
        self.rows = vec![];
        self.protos.clear();
        self.removed.clear();
        self.touched.clear();
        self.groups.clear();
        self.mapped = None;
        self.since.clear();
        self.fine_rx = None;
        self.fine_asked.clear();
        self.fine_bytes = 0;
    }

    /// The set is re-read: previous placements stay on screen until it is ready, except those
    /// that left the cache (the provider already draws them).
    fn retire(&mut self, keep: &HashSet<i64>) {
        for r in &self.rows {
            if !keep.contains(&r.mesh) && self.removed.insert(r.mesh) {
                self.touched.insert(r.proto);
            }
        }
        self.since.clear();
    }

    fn accept(&mut self, set: InstanceSet) {
        self.rows = set.rows;
        self.protos = set
            .protos
            .into_iter()
            .filter_map(|p| {
                let id = p.id;
                proto_meshes(p).map(|m| (id, m))
            })
            .collect();
        self.removed = std::mem::take(&mut self.since);
        self.touched.clear();
        self.mapped = None;
        self.fine_rx = None;
        self.fine_asked.clear();
        self.fine_bytes = 0;
    }

    /// Prototypes from `wanted` (frame group keys) whose exact shape can still be loaded.
    fn fine_ids(&self, wanted: &[u64]) -> Vec<i64> {
        if self.fine_bytes >= FINE_BUDGET {
            return vec![];
        }
        let mut v: Vec<i64> = wanted
            .iter()
            .filter_map(|&k| proto_of_key(k))
            .filter(|id| {
                !self.fine_asked.contains(id) && self.protos.get(id).is_some_and(|p| p.approx > 0.0)
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v.truncate(FINE_BATCH);
        v
    }

    fn ask_fine(&mut self, wanted: &[u64], model: &Model) {
        if self.fine_rx.is_some() {
            return;
        }
        let ids = self.fine_ids(wanted);
        if ids.is_empty() {
            return;
        }
        self.fine_asked.extend(ids.iter().copied());
        let path = model.sql().path().unwrap_or_default().to_string();
        let (tx, rx) = mpsc::channel();
        if path.is_empty() {
            let _ = tx.send(model.proto_levels(&ids).map_err(|e| e.to_string()));
            self.fine_rx = Some(rx);
            return;
        }
        let tx_panic = tx.clone();
        // reads the database file read-only; the model is not touched
        let job = crate::jobs::spawn_guarded(
            "ddc-mesh-fine",
            move || {
                let _ = tx.send(read_proto_levels_file(&path, &ids).map_err(|e| e.to_string()));
            },
            move |msg| {
                let _ = tx_panic.send(Err(msg));
            },
        );
        // it failed: the shape stays coarse, and the status line shows that
        self.fine_rx = job.is_ok().then_some(rx);
    }

    fn accept_fine(&mut self, v: Vec<InstProto>) -> bool {
        let mut any = false;
        for p in v {
            let id = p.id;
            let Some(m) = proto_meshes(p).filter(|m| m.approx <= 0.0) else {
                continue;
            };
            self.fine_bytes += (m.full.positions.len() as u64 + m.full.triangles.len() as u64) * 12;
            self.protos.insert(id, m);
            self.touched.insert(id);
            any = true;
        }
        any
    }

    /// Collect the loaded shapes; true if the scene should be rebuilt.
    fn poll_fine(&mut self) -> bool {
        let Some(rx) = &self.fine_rx else {
            return false;
        };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => false,
            Ok(Ok(v)) => {
                self.fine_rx = None;
                self.accept_fine(v)
            }
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                self.fine_rx = None;
                false
            }
        }
    }

    /// Collect the background read result; true if the scene should be rebuilt.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => false,
            Ok(Ok(set)) => {
                self.rx = None;
                self.accept(set);
                true
            }
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                self.rx = None;
                self.failed = true;
                true
            }
        }
    }

    fn reload(&mut self, ed: &mut Editor) -> bool {
        let ids = ed.model.instanced_mesh_ids().unwrap_or_default();
        let had = !ed.big.instanced_meshes.is_empty();
        if ids.is_empty() {
            self.clear();
            ed.big.instanced_meshes.clear();
            return had;
        }
        let n = ids.len();
        let ids: HashSet<i64> = ids.into_iter().collect();
        let path = ed.model.sql().path().unwrap_or_default().to_string();
        if n >= SYNC_ROWS && !path.is_empty() {
            let (tx, rx) = mpsc::channel();
            let tx_panic = tx.clone();
            // reads the database file read-only; the model is not touched
            let job = crate::jobs::spawn_guarded(
                "ddc-mesh-feed",
                move || {
                    let r = read_instance_set_file(&path, CPU_BUDGET).map_err(|e| e.to_string());
                    let _ = tx.send(r);
                },
                move |msg| {
                    let _ = tx_panic.send(Err(msg));
                },
            );
            if job.is_ok() {
                self.retire(&ids);
                ed.big.instanced_meshes = ids;
                self.rx = Some(rx);
                return true;
            }
        }
        self.clear();
        match ed.model.instance_set(CPU_BUDGET) {
            Ok(set) => {
                ed.big.instanced_meshes = ids;
                self.accept(set);
            }
            Err(_) => ed.big.instanced_meshes.clear(),
        }
        true
    }

    /// Scene build step from the edit journal; true if the `mesh` provider's mesh set changed entirely.
    pub fn sync(&mut self, ed: &mut Editor, ch: &Changes) -> bool {
        if !self.on {
            return false;
        }
        if std::mem::take(&mut self.failed) {
            self.clear();
            ed.big.instanced_meshes.clear();
            return true;
        }
        if std::mem::take(&mut self.stale) || ch.whole("mesh") {
            return self.reload(ed);
        }
        let changed: HashSet<i64> = ch
            .ids("mesh")
            .filter(|id| ed.big.instanced_meshes.contains(id))
            .collect();
        if changed.is_empty() {
            return false;
        }
        for id in &changed {
            ed.big.instanced_meshes.remove(id);
        }
        // the journal rebuilds the provider parts: the same `mesh` in the scene's `dirty`
        self.touched.extend(
            self.rows
                .iter()
                .filter(|r| changed.contains(&r.mesh))
                .map(|r| r.proto),
        );
        if self.rx.is_some() {
            self.since.extend(changed.iter().copied());
        }
        self.removed.extend(changed);
        false
    }

    fn group(&self, k: GroupKey, inst: Vec<Inst>) -> Option<Instanced> {
        let p = self.protos.get(&k.0)?;
        let g = Instanced::shared(group_key(k), p.full.clone(), Arc::new(inst));
        Some(g.with_lods(p.lods.clone()).with_approx(p.approx))
    }

    /// Scene groups. Unchanged groups return the same `Arc`s: the GPU and the index do not touch them.
    pub fn groups(&mut self, key: MapKey, slot: &mut dyn FnMut(i64) -> u32) -> Vec<Instanced> {
        if self.rows.is_empty() {
            return vec![];
        }
        if self.mapped.as_ref() != Some(&key) {
            let mapped = map_rows(&self.rows, &self.removed, &key, slot);
            self.groups = mapped
                .into_iter()
                .filter_map(|(k, v)| self.group(k, v).map(|g| (k, g)))
                .collect();
            self.touched.clear();
            self.mapped = Some(key);
        } else {
            for proto in std::mem::take(&mut self.touched) {
                self.groups.remove(&(proto, true));
                self.groups.remove(&(proto, false));
                let mapped = map_rows(rows_of(&self.rows, proto), &self.removed, &key, slot);
                for (k, v) in mapped {
                    if let Some(g) = self.group(k, v) {
                        self.groups.insert(k, g);
                    }
                }
            }
        }
        self.groups.values().cloned().collect()
    }
}

/// Shell frame: a ready set sends the scene for rebuilding.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let wanted = ddc_render::perf::coarse_wanted();
    let f = &mut a.view.e3.cache.inst;
    let set = f.poll();
    let fine = f.poll_fine();
    if set || fine {
        a.clash.dirty3d = true;
        ctx.request_repaint();
    }
    f.ask_fine(&wanted, &a.ed.model);
    if f.loading() || f.fine_rx.is_some() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::super::pick::{Cat, Target};
    use super::super::scene3d::{self, compose_with, Cache, EXT_BASE, EXT_END};
    use super::*;
    use ddc_geom::instance::Place;

    fn row(mesh: i64, proto: i64, level: Option<i64>, z: f64) -> InstRow {
        InstRow {
            mesh,
            proto,
            level,
            place: Place {
                t: [mesh as f64, 0.0, z],
                ..Place::IDENTITY
            },
        }
    }

    fn key(active: i64, z0: f64, levels: &[i64]) -> MapKey {
        MapKey {
            active,
            z0,
            levels: levels.to_vec(),
            epoch: 0,
        }
    }

    fn slots() -> impl FnMut(i64) -> u32 {
        |mesh| EXT_BASE + mesh as u32
    }

    #[test]
    fn active_rows_get_slots_and_others_are_not_selectable() {
        let rows = [
            row(1, 7, Some(10), 0.0),
            row(2, 7, Some(11), 3.0),
            row(3, 7, None, 0.0),
        ];
        let m = map_rows(
            &rows,
            &HashSet::new(),
            &key(10, 0.0, &[10, 11]),
            &mut slots(),
        );
        assert_eq!(m.len(), 2, "split by active level: a group is one category");
        let on: Vec<u32> = m[&(7, true)].iter().map(|i| i.element).collect();
        assert_eq!(on, vec![EXT_BASE + 1, EXT_BASE + 3]);
        let off = &m[&(7, false)];
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].element, OTHER_LEVEL);
    }

    #[test]
    fn z_is_relative_to_the_active_elevation() {
        let rows = [row(1, 7, Some(10), 7.5)];
        let m = map_rows(&rows, &HashSet::new(), &key(10, 3.0, &[10]), &mut slots());
        let p = m[&(7, true)][0]
            .m
            .transform_point3(ddc_render::glam::Vec3::ZERO);
        assert_eq!(p.z, 4.5);
        assert_eq!(p.x, 1.0);
    }

    #[test]
    fn removed_rows_and_rows_of_unknown_levels_are_skipped() {
        let rows = [
            row(1, 7, Some(10), 0.0),
            row(2, 7, Some(10), 0.0),
            row(3, 8, Some(99), 0.0),
        ];
        let removed: HashSet<i64> = [1].into_iter().collect();
        let m = map_rows(&rows, &removed, &key(10, 0.0, &[10]), &mut slots());
        assert_eq!(m.len(), 1);
        assert_eq!(m[&(7, true)].len(), 1);
        assert_eq!(m[&(7, true)][0].element, EXT_BASE + 2);
    }

    #[test]
    fn rows_of_finds_the_range_of_one_proto() {
        let rows = [
            row(1, 1, None, 0.0),
            row(2, 2, None, 0.0),
            row(3, 2, None, 0.0),
            row(4, 3, None, 0.0),
        ];
        assert_eq!(rows_of(&rows, 2).len(), 2);
        assert_eq!(rows_of(&rows, 5).len(), 0);
    }

    #[test]
    fn group_keys_do_not_collide() {
        let keys: HashSet<u64> = [(1, true), (1, false), (2, true), (2, false)]
            .into_iter()
            .map(group_key)
            .collect();
        assert_eq!(keys.len(), 4);
        assert!(keys.iter().all(|k| k & KEY_BIT != 0));
    }

    fn proto(id: i64, err: f32) -> InstProto {
        InstProto {
            id,
            levels: vec![ddc_model::InstLevel {
                positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                triangles: vec![[0, 1, 2]],
                err,
            }],
        }
    }

    fn set_of(rows: Vec<InstRow>, p: InstProto) -> InstanceSet {
        InstanceSet {
            rows,
            protos: vec![p],
            dropped_levels: 0,
        }
    }

    fn drawn(f: &mut Feed) -> Vec<i64> {
        let k = key(1, 0.0, &[]);
        let mut v: Vec<i64> = f
            .groups(k, &mut slots())
            .iter()
            .flat_map(|g| g.inst.iter())
            .map(|i| (i.element - EXT_BASE) as i64)
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn old_rows_stay_on_screen_until_the_new_set_arrives() {
        let mut f = Feed::default();
        f.accept(set_of(
            vec![row(1, 7, None, 0.0), row(2, 7, None, 0.0)],
            proto(7, 0.0),
        ));
        assert_eq!(drawn(&mut f), vec![1, 2]);
        // re-reading started: row 1 left the cache for the provider, row 2 is still visible
        f.retire(&[2, 3].into_iter().collect());
        assert_eq!(
            drawn(&mut f),
            vec![2],
            "row 1 is the provider's now, row 3 comes with the set"
        );
        // an edit during the read: the set may have read the mesh stale
        f.since.insert(3);
        f.accept(set_of(
            vec![row(2, 7, None, 0.0), row(3, 7, None, 0.0)],
            proto(7, 0.0),
        ));
        assert_eq!(
            drawn(&mut f),
            vec![2],
            "an edit during the read wins over the set"
        );
    }

    #[test]
    fn a_coarse_prototype_says_so_and_is_replaced_by_the_exact_one() {
        let mut f = Feed::default();
        f.accept(set_of(vec![row(1, 7, None, 0.0)], proto(7, 0.25)));
        let g = f.groups(key(1, 0.0, &[]), &mut slots());
        assert_eq!(g[0].approx, 0.25, "the deviation is not thrown away");
        let wanted = [g[0].key, 5];
        assert_eq!(proto_of_key(g[0].key), Some(7));
        assert_eq!(proto_of_key(5), None, "a group of another provider");
        assert_eq!(f.fine_ids(&wanted), vec![7]);
        f.fine_asked.insert(7);
        assert!(f.fine_ids(&wanted).is_empty(), "asked once");
        f.fine_asked.clear();
        assert!(f.accept_fine(vec![proto(7, 0.0)]));
        let g = f.groups(key(1, 0.0, &[]), &mut slots());
        assert_eq!(g[0].approx, 0.0);
        assert!(f.fine_ids(&wanted).is_empty(), "already exact");
        f.fine_bytes = FINE_BUDGET;
        f.accept(set_of(vec![row(1, 7, None, 0.0)], proto(7, 0.25)));
        assert_eq!(f.fine_bytes, 0, "a new set starts its own count");
    }

    fn mesh_row(ed: &mut Editor, level: i64, dx: f64) -> i64 {
        let mesh = ddc_geom::mesh::Mesh {
            positions: [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.5, 0.0],
                [0.0, 0.0, 2.0],
            ]
            .iter()
            .map(|p: &[f64; 3]| ddc_geom::v3(p[0] + dx, p[1], p[2]))
            .collect(),
            triangles: vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]],
            face_of: vec![0, 1, 2, 3],
        };
        let (pb, ib) = ddc_model::mesh_blobs(&mesh);
        ed.model
            .batch_walls("wall_mesh_inst_test", |db| {
                db.execute(
                    "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'c','n',?2,?3)",
                    (level, pb, ib),
                )?;
                Ok(db.last_insert_rowid())
            })
            .unwrap()
    }

    fn mesh_parts(s: &ddc_render::Scene, targets: &[Target]) -> usize {
        s.meshes
            .iter()
            .filter(|m| {
                scene3d::target_of(m.element, targets)
                    .flatten()
                    .is_some_and(|t| t.cat == Cat::Mesh)
            })
            .count()
    }

    #[test]
    fn cached_meshes_become_instances_that_select_their_rows() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let up = ed.model.add_level("L2", 3.0).unwrap();
        ed.reload().unwrap();
        let a = mesh_row(&mut ed, lv, 0.0);
        let b = mesh_row(&mut ed, lv, 5.0);
        let far = mesh_row(&mut ed, up, 10.0);
        ed.model.mesh_cache_build(&mut |_, _| true).unwrap();
        ed.reload().unwrap();

        let mut c = Cache::default();
        c.inst.enable();
        let (s, targets) = compose_with(&mut ed, &mut c);
        assert_eq!(
            mesh_parts(&s, &targets),
            0,
            "the provider leaves cached rows out"
        );
        let groups: Vec<_> = s
            .instanced
            .iter()
            .filter(|g| g.key & KEY_BIT != 0)
            .collect();
        assert_eq!(groups.len(), 2);
        let mut got: Vec<Target> = groups
            .iter()
            .flat_map(|g| g.inst.iter())
            .filter(|i| (EXT_BASE..=EXT_END).contains(&i.element))
            .map(|i| scene3d::target_of(i.element, &targets).flatten().unwrap())
            .collect();
        got.sort();
        let mut want = vec![Target::new(Cat::Mesh, a), Target::new(Cat::Mesh, b)];
        want.sort();
        assert_eq!(got, want);
        let other: Vec<u32> = groups
            .iter()
            .flat_map(|g| g.inst.iter())
            .map(|i| i.element)
            .filter(|e| !(EXT_BASE..=EXT_END).contains(e))
            .collect();
        assert_eq!(other, vec![OTHER_LEVEL], "row {far} of the other level");

        // without edits, the same Arcs
        let (s2, _) = compose_with(&mut ed, &mut c);
        let ptrs = |s: &ddc_render::Scene| -> Vec<*const Vec<Inst>> {
            s.instanced.iter().map(|g| Arc::as_ptr(&g.inst)).collect()
        };
        assert_eq!(ptrs(&s), ptrs(&s2));

        // the changed mesh returns to the provider; the rest stay instanced
        let (pb, _) = ddc_model::mesh_blobs(&ed.model.imported_mesh(a).unwrap().mesh);
        ed.model
            .batch_walls("wall_mesh_inst_test2", |db| {
                db.execute("UPDATE mesh SET positions=?2 WHERE id=?1", (a, pb))?;
                Ok(())
            })
            .unwrap();
        let (s3, t3) = compose_with(&mut ed, &mut c);
        assert_eq!(mesh_parts(&s3, &t3), 1);
        assert!(!ed.big.instanced_meshes.contains(&a));
        let rows: usize = s3
            .instanced
            .iter()
            .filter(|g| g.key & KEY_BIT != 0)
            .map(|g| g.inst.len())
            .sum();
        assert_eq!(rows, 2);
    }

    #[test]
    fn level_switch_remaps_slots() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let up = ed.model.add_level("L2", 3.0).unwrap();
        ed.reload().unwrap();
        let a = mesh_row(&mut ed, lv, 0.0);
        let far = mesh_row(&mut ed, up, 10.0);
        ed.model.mesh_cache_build(&mut |_, _| true).unwrap();
        ed.reload().unwrap();
        let mut c = Cache::default();
        c.inst.enable();
        let (_, t1) = compose_with(&mut ed, &mut c);
        assert!(t1.contains(&Target::new(Cat::Mesh, a)));
        assert!(!t1.contains(&Target::new(Cat::Mesh, far)));
        ed.set_level(up);
        let (s2, t2) = compose_with(&mut ed, &mut c);
        assert!(t2.contains(&Target::new(Cat::Mesh, far)));
        assert!(!t2.contains(&Target::new(Cat::Mesh, a)));
        let selectable = s2
            .instanced
            .iter()
            .flat_map(|g| g.inst.iter())
            .filter(|i| {
                scene3d::target_of(i.element, &t2).flatten() == Some(Target::new(Cat::Mesh, far))
            })
            .count();
        assert_eq!(selectable, 1);
    }

    #[test]
    fn a_one_off_compose_leaves_the_live_feed_alone() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let a = mesh_row(&mut ed, lv, 0.0);
        ed.model.mesh_cache_build(&mut |_, _| true).unwrap();
        ed.reload().unwrap();
        let mut c = Cache::default();
        c.inst.enable();
        compose_with(&mut ed, &mut c);
        assert!(ed.big.instanced_meshes.contains(&a));
        let (s, t) = scene3d::compose(&mut ed);
        assert_eq!(mesh_parts(&s, &t), 1, "a snapshot draws the mesh itself");
        assert!(ed.big.instanced_meshes.contains(&a));
    }

    #[test]
    fn a_disabled_feed_changes_nothing() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        mesh_row(&mut ed, lv, 0.0);
        ed.model.mesh_cache_build(&mut |_, _| true).unwrap();
        ed.reload().unwrap();
        let (s, t) = compose_with(&mut ed, &mut Cache::default());
        assert_eq!(mesh_parts(&s, &t), 1);
        assert!(s.instanced.iter().all(|g| g.key & KEY_BIT == 0));
        assert!(ed.big.instanced_meshes.is_empty());
    }
}
