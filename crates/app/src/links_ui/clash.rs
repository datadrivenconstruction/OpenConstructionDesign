// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Clashes of host elements with link elements (a standalone simple check; the general
//! clash tool is separate). Host bodies are the 3D meshes of all levels (like `scene_all`),
//! built from a model copy in a background thread together with the `ddc_model::links::clash` check;
//! the editor (level, selection, revision) is not touched.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use ddc_model::links::{clash, Body, Transform};
use ddc_model::Model;

use super::derive::{append, body_in_host};
use super::load::Elem;
use crate::editor::{Editor, Sel};

/// Touch tolerance, m: a smaller overlap is not a clash.
pub const TOL: f64 = 0.001;
/// No more than this many pairs in the report.
pub const LIMIT: usize = 5000;

#[derive(Clone, Debug, PartialEq)]
pub struct ClashRow {
    /// A host element and its level.
    pub sel: Sel,
    pub level: i64,
    /// A link element: kind, id in the link model, source guid.
    pub kind: &'static str,
    pub id: i64,
    pub guid: String,
    /// Shared AABB (host world), m.
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// Model element bodies across all levels in world coordinates (its own editor, in a thread).
pub fn host_bodies(model: Model) -> Result<(Vec<(Sel, i64)>, Vec<Body>), ddc_model::Error> {
    let mut ed = Editor::new_in_memory()?;
    ed.model = model;
    ed.underlay_on = false;
    ed.reload()?;
    let mut items: Vec<(Sel, i64)> = vec![];
    let mut bodies: Vec<Body> = vec![];
    let mut at: HashMap<Sel, usize> = HashMap::new();
    for (lv, _, _) in ed.levels() {
        ed.level = lv;
        if ed.reload().is_err() {
            continue;
        }
        let z = ed.elevation;
        at.clear();
        for m in ed.scene().meshes {
            let Some(s) = ed.sel_of_element(m.element) else {
                continue;
            };
            let i = *at.entry(s).or_insert_with(|| {
                items.push((s, lv));
                bodies.push(Body::default());
                bodies.len() - 1
            });
            append(&mut bodies[i], &m, z);
        }
    }
    Ok((items, bodies))
}

/// A copy of the host model for the thread (a database connection is not shared between threads).
fn snapshot(ed: &Editor, link: i64) -> Result<std::path::PathBuf, String> {
    let path = std::env::temp_dir().join(format!("ddc-clash-{}-{link}.db", std::process::id()));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    let p = path.to_string_lossy().into_owned();
    ed.model
        .sql()
        .execute("VACUUM INTO ?1", [&p])
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// Host x link pairs (link elements are moved into the host world by placement `xf`).
pub fn run(items: &[(Sel, i64)], host: &[Body], elems: &[Elem], xf: &Transform) -> Vec<ClashRow> {
    let idx: Vec<usize> = (0..elems.len())
        .filter(|&i| elems[i].kind != "join")
        .collect();
    let lb: Vec<Body> = idx
        .iter()
        .map(|&i| body_in_host(&elems[i].body, xf))
        .collect();
    clash(host, &lb, TOL, LIMIT)
        .into_iter()
        .map(|c| {
            let (sel, level) = items[c.host];
            let e = &elems[idx[c.link]];
            ClashRow {
                sel,
                level,
                kind: e.kind,
                id: e.id,
                guid: e.guid.clone(),
                min: c.min,
                max: c.max,
            }
        })
        .collect()
}

/// Open the copy, then host bodies, then the check; any error is returned, not hidden in an empty list.
fn compute(
    opened: Result<Model, ddc_model::Error>,
    elems: &[Elem],
    xf: &Transform,
) -> Result<Vec<ClashRow>, String> {
    let (items, host) = opened.and_then(host_bodies).map_err(|e| e.to_string())?;
    Ok(run(&items, &host, elems, xf))
}

pub struct ClashJob {
    pub link: i64,
    rx: Receiver<Result<Vec<ClashRow>, String>>,
}

impl ClashJob {
    /// On the UI thread only the database copy; host bodies and the check run in their own thread.
    pub fn start(
        ed: &Editor,
        link: i64,
        elems: Arc<Vec<Elem>>,
        xf: Transform,
    ) -> Result<Self, String> {
        let path = snapshot(ed, link)?;
        let (tx, rx) = channel();
        let tx_panic = tx.clone();
        let tmp = path.clone();
        // works on a snapshot copy of the host database: the model is not touched
        let _ = crate::jobs::spawn_guarded(
            "link-clash",
            move || {
                let rows = compute(Model::open(&path.to_string_lossy()), &elems, &xf);
                let _ = std::fs::remove_file(&path);
                let _ = tx.send(rows);
            },
            move |msg| {
                let _ = std::fs::remove_file(&tmp);
                let _ = tx_panic.send(Err(msg));
            },
        );
        Ok(Self { link, rx })
    }

    /// `Err` is a failure to open the copy, build bodies or run the thread: it is not "no clashes".
    pub fn poll(&mut self) -> Option<Result<Vec<ClashRow>, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(crate::jobs::internal_error())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident() -> Transform {
        Transform::default()
    }

    #[test]
    fn open_failure_is_error_not_empty() {
        let r = compute(Err(ddc_model::Error::NothingToUndo), &[], &ident());
        assert!(r.is_err());
    }

    #[test]
    fn host_bodies_failure_is_error_not_empty() {
        let dir = std::env::temp_dir().join(format!("ddc-clash-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.db");
        std::fs::write(&bad, b"not a database, just garbage bytes").unwrap();
        let r = compute(Model::open(&bad.to_string_lossy()), &[], &ident());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err());
    }

    #[test]
    fn healthy_empty_model_is_ok_empty() {
        let m = Model::open(":memory:").unwrap();
        let r = compute(Ok(m), &[], &ident());
        assert_eq!(r, Ok(vec![]));
    }

    #[test]
    fn dropped_sender_is_error() {
        let (tx, rx) = channel();
        drop(tx);
        let mut j = ClashJob { link: 1, rx };
        assert!(matches!(j.poll(), Some(Err(_))));
    }
}
