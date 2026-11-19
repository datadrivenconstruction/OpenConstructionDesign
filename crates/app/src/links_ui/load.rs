// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Background link load: .ifc by import into a separate model, .rvt via a bridge (like "Import"),
//! then element bodies by level (the same meshes the host 3D builds) and wall axes for snaps.
//! All in its own thread: the UI thread receives ready data in [`LoadJob::poll`]. With the cache (`cache`)
//! a reopen reads bodies from the cache file with no import; the render source (`source`)
//! is built here too, not on the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use ddc_io::bridge::{self, TrustStore};
use ddc_model::links::Body;
use ddc_model::Model;

use crate::editor::{Editor, Sel};

/// A link element: kind (`wall`, `opening`, `slab`, `curtain_wall`, `mesh`, `join` is a wall
/// joint filler), id in the link model, source guid ('' if none), body in the link world.
pub struct Elem {
    pub kind: &'static str,
    pub id: i64,
    pub guid: String,
    pub body: Body,
}

/// A link wall axis (link world): ends, bottom and top elevations, half thickness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkWall {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub z0: f64,
    pub z1: f64,
    pub half: f64,
}

pub struct LinkData {
    /// Levels in the link model (the model itself is not kept in memory).
    pub levels: usize,
    pub elems: Arc<Vec<Elem>>,
    pub walls: Vec<LinkWall>,
    /// Its own render source: shapes and placements, separate from the host scene.
    pub source: Arc<super::source::Source>,
    /// Cache file name in the `<project>.links` folder (None means no cache).
    pub cache: Option<String>,
    /// Read from the cache (no import).
    pub hit: bool,
}

/// Where and how to cache: the cache folder and whether to recompute the source hash.
#[derive(Clone, Debug)]
pub struct CacheUse {
    pub dir: PathBuf,
    pub rehash: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoadError {
    /// The .rvt module is not installed or not signed.
    NoModule,
    Failed(String),
}

enum Msg {
    Progress(u64, u64),
    Done(Result<LinkData, LoadError>),
}

pub struct LoadJob {
    pub cancel: Arc<AtomicBool>,
    /// (done, total) from the bridge; for .ifc it is (0, 0).
    pub progress: (u64, u64),
    rx: Receiver<Msg>,
}

impl LoadJob {
    pub fn start(
        path: PathBuf,
        format: String,
        module: Option<(PathBuf, TrustStore)>,
        cache: Option<CacheUse>,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let c = cancel.clone();
        let tx_panic = tx.clone();
        // reads the linked file into memory; the host model is not touched
        let _ = crate::jobs::spawn_guarded(
            "link-load",
            move || {
                let r = load(&path, &format, module, cache.as_ref(), &c, &mut |d, t| {
                    let _ = tx.send(Msg::Progress(d, t));
                });
                let _ = tx.send(Msg::Done(r));
            },
            move |msg| {
                let _ = tx_panic.send(Msg::Done(Err(LoadError::Failed(msg))));
            },
        );
        Self {
            cancel,
            progress: (0, 0),
            rx,
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Some means the thread finished.
    pub fn poll(&mut self) -> Option<Result<LinkData, LoadError>> {
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Progress(d, t)) => self.progress = (d, t),
                Ok(Msg::Done(r)) => return Some(r),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    return Some(Err(LoadError::Failed(crate::jobs::internal_error())))
                }
            }
        }
    }
}

fn failed(e: impl std::fmt::Display) -> LoadError {
    LoadError::Failed(e.to_string())
}

/// Reads a link (called in a background thread): from the cache if it exists and matches the
/// source content and import version, otherwise by import with a cache write.
pub fn load(
    path: &Path,
    format: &str,
    module: Option<(PathBuf, TrustStore)>,
    cache: Option<&CacheUse>,
    cancel: &Arc<AtomicBool>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<LinkData, LoadError> {
    let mdir = module.as_ref().map(|m| m.0.clone());
    let key = cache.and_then(|c| {
        let hash = super::cache::content_hash(&c.dir, path, c.rehash).ok()?;
        let importer = super::cache::importer(format, mdir.as_deref());
        Some(super::cache::Key { hash, importer })
    });
    if let (Some(c), Some(k)) = (cache, &key) {
        if let Some(hit) = super::cache::read(&c.dir, k) {
            let mut d = finish(hit.elems, hit.walls, hit.levels);
            d.cache = Some(k.file_name());
            d.hit = true;
            return Ok(d);
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(failed(crate::jobs::CANCELLED));
    }
    let m = import(path, format, module, cancel, progress)?;
    let (m, elems, walls, levels) = derive_all(m).map_err(failed)?;
    let mut name = None;
    if let (Some(c), Some(k)) = (cache, &key) {
        name = super::cache::write(&c.dir, path, k, &m, &elems, &walls, levels).ok();
        if let Some(n) = &name {
            super::cache::evict(&c.dir, std::slice::from_ref(n), super::cache::limit());
        }
    }
    drop(m);
    let mut d = finish(elems, walls, levels);
    d.cache = name;
    Ok(d)
}

fn import(
    path: &Path,
    format: &str,
    module: Option<(PathBuf, TrustStore)>,
    cancel: &Arc<AtomicBool>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Model, LoadError> {
    let mut m = Model::open(":memory:").map_err(failed)?;
    match format {
        "ifc" => {
            ddc_io::ifc::import_ifc_file(&mut m, path).map_err(failed)?;
        }
        "rvt" => {
            let Some((dir, trust)) = module else {
                return Err(LoadError::NoModule);
            };
            let o = bridge::RunOptions {
                cancel: Some(cancel.clone()),
                ..Default::default()
            };
            let frames =
                bridge::collect_trusted(&dir, &trust, path, &o, &mut |p| progress(p.done, p.total))
                    .map_err(failed)?;
            bridge::import_frames(&mut m, path, frames).map_err(failed)?;
        }
        f => return Err(failed(format!("unsupported link format '{f}'"))),
    }
    Ok(m)
}

fn key(s: Sel) -> (&'static str, i64) {
    match s {
        Sel::Wall(id) => ("wall", id),
        Sel::Opening(id) => ("opening", id),
        Sel::Curtain(id) | Sel::Panel(id, _) => ("curtain_wall", id),
        Sel::Slab(id) => ("slab", id),
        Sel::Room(id) => ("room", id),
        Sel::Mass(id) => ("mass", id),
        Sel::Mep(id) => ("mep_run", id),
        Sel::Comp(id) => ("comp", id),
        Sel::Body(k, id) => (k.table(), id),
        Sel::Ext(_, id) => ("ext", id),
        Sel::Anno(id) => ("anno", id),
    }
}

/// Kind and id of a host element for the clash list.
pub fn host_key(s: Sel) -> (&'static str, i64) {
    key(s)
}

fn link_walls(m: &Model) -> Result<Vec<LinkWall>, ddc_model::Error> {
    let mut st = m.sql().prepare(
        "SELECT w.ax, w.ay, w.bx, w.by, lv.elevation + w.base_offset, w.height, \
         COALESCE((SELECT SUM(thickness) FROM wall_type_layer WHERE wtype=w.wtype), 0) \
         FROM wall w JOIN level lv ON lv.id=w.level ORDER BY w.id",
    )?;
    let rows = st.query_map([], |r| {
        let z0: f64 = r.get(4)?;
        let h: f64 = r.get(5)?;
        let t: f64 = r.get(6)?;
        Ok(LinkWall {
            a: (r.get(0)?, r.get(1)?),
            b: (r.get(2)?, r.get(3)?),
            z0,
            z1: z0 + h,
            half: t / 2.0,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Meshes of the `mesh` table (bridge import): f64 xyz LE in world coordinates, u32 LE indices.
fn table_meshes(m: &Model) -> Result<Vec<(i64, Body)>, ddc_model::Error> {
    let mut st = m
        .sql()
        .prepare("SELECT id, positions, indices FROM mesh ORDER BY id")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (id, pb, ib) = row?;
        let f = |c: &[u8]| f64::from_le_bytes(c.try_into().unwrap_or([0; 8]));
        let pos: Vec<[f64; 3]> = pb
            .chunks_exact(24)
            .map(|c| [f(&c[0..8]), f(&c[8..16]), f(&c[16..24])])
            .collect();
        let idx: Vec<u32> = ib
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
            .collect();
        let n = pos.len() as u32;
        let tri: Vec<[u32; 3]> = idx
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .filter(|t| t.iter().all(|&i| i < n))
            .collect();
        if !tri.is_empty() {
            out.push((id, Body { pos, tri }));
        }
    }
    Ok(out)
}

fn guids(m: &Model) -> HashMap<(String, i64), String> {
    m.sql()
        .prepare("SELECT kind, ref, guid FROM ext_ref")
        .and_then(|mut st| {
            st.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
                .collect()
        })
        .unwrap_or_default()
}

/// Link data from a model without the cache (bodies, wall axes, render source).
pub fn build(model: Model) -> Result<LinkData, ddc_model::Error> {
    let (_, elems, walls, levels) = derive_all(model)?;
    Ok(finish(elems, walls, levels))
}

/// The render source is built here, in the background load thread.
pub(super) fn finish(elems: Vec<Elem>, walls: Vec<LinkWall>, levels: usize) -> LinkData {
    let source = Arc::new(super::source::build(&elems));
    LinkData {
        levels,
        elems: Arc::new(elems),
        walls,
        source,
        cache: None,
        hit: false,
    }
}

type Derived = (Model, Vec<Elem>, Vec<LinkWall>, usize);

/// Element bodies across all levels of the link model (link world), wall axes, level count;
/// the model is returned for the cache write.
fn derive_all(model: Model) -> Result<Derived, ddc_model::Error> {
    let walls = link_walls(&model)?;
    let meshes = table_meshes(&model)?;
    let ids = guids(&model);
    let mut le = Editor::new_in_memory()?;
    le.model = model;
    le.underlay_on = false;
    le.reload()?;
    let mut elems: Vec<Elem> = vec![];
    let mut at: HashMap<(&'static str, i64), usize> = HashMap::new();
    let levels = le.levels();
    let n_levels = levels.len();
    for (lv, _, _) in levels {
        le.level = lv;
        le.reload()?;
        let z = le.elevation;
        for m in le.scene().meshes {
            let k = le.sel_of_element(m.element).map_or(("join", lv), key);
            let i = *at.entry(k).or_insert_with(|| {
                elems.push(Elem {
                    kind: k.0,
                    id: k.1,
                    guid: String::new(),
                    body: Body::default(),
                });
                elems.len() - 1
            });
            super::derive::append(&mut elems[i].body, &m, z);
        }
    }
    elems.extend(meshes.into_iter().map(|(id, body)| Elem {
        kind: "mesh",
        id,
        guid: String::new(),
        body,
    }));
    for e in &mut elems {
        if let Some(g) = ids.get(&(e.kind.to_string(), e.id)) {
            e.guid = g.clone();
        }
    }
    let model = std::mem::replace(&mut le.model, Model::open(":memory:")?);
    Ok((model, elems, walls, n_levels))
}
