// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan outlines from a project database: the thumbnail of every card (walls, slab outlines and
//! the world boxes of imported meshes, so a mesh-only model has a picture too) and the early
//! view of a build in progress, read on a helper thread through a second read-only connection
//! to the file the worker is writing. No editing, no 3D here: that stays with the renderer.
use crate::project::{render_thumb, THUMB_SIZE};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use web_time::{Duration, Instant};

/// Most outlines of one kind in a picture.
pub const MAX_ITEMS: i64 = 20_000;
/// The early view refreshes at most this often.
pub const EVERY: Duration = Duration::from_millis(1200);

fn rect_lines(b: [f64; 4]) -> [[f64; 4]; 4] {
    let [x0, y0, x1, y1] = b;
    [
        [x0, y0, x1, y0],
        [x1, y0, x1, y1],
        [x1, y1, x0, y1],
        [x0, y1, x0, y0],
    ]
}

fn exists(db: &Connection, table: &str) -> bool {
    db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get::<_, i64>(0),
    )
    .map_or(false, |n| n > 0)
}

/// Boxes (min x, min y, max x, max y) of the cached world boxes of imported meshes.
pub fn mesh_boxes(db: &Connection, limit: i64) -> Vec<[f64; 4]> {
    if !exists(db, "mesh_geo") {
        return vec![];
    }
    let Ok(mut st) =
        db.prepare("SELECT minx, miny, maxx, maxy FROM mesh_geo WHERE proto IS NOT NULL LIMIT ?1")
    else {
        return vec![];
    };
    let v = st
        .query_map([limit], |r| {
            Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?])
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    v
}

/// Boxes straight from the mesh blobs (little-endian f64 triples), for a build whose box cache
/// does not exist yet. Reads at most `limit` meshes.
pub fn blob_boxes(db: &Connection, limit: i64) -> Vec<[f64; 4]> {
    if !exists(db, "mesh") {
        return vec![];
    }
    let Ok(mut st) = db.prepare("SELECT positions FROM mesh LIMIT ?1") else {
        return vec![];
    };
    let rows = st.query_map([limit], |r| r.get::<_, Vec<u8>>(0));
    let Ok(rows) = rows else {
        return vec![];
    };
    let v = rows.flatten().filter_map(|b| blob_box(&b)).collect();
    v
}

/// Plan box of a positions blob; `None` for an empty or non-finite one.
pub fn blob_box(b: &[u8]) -> Option<[f64; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in b.chunks_exact(24) {
        let f = |i: usize| f64::from_le_bytes(p[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
        let (x, y) = (f(0), f(1));
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    (x0 <= x1).then_some([x0, y0, x1, y1])
}

/// Slab outer outlines.
pub fn slab_lines(db: &Connection, limit: i64) -> Vec<[f64; 4]> {
    if !exists(db, "slab_vertex") {
        return vec![];
    }
    let Ok(mut st) =
        db.prepare("SELECT slab, x, y FROM slab_vertex WHERE ring=0 ORDER BY slab, idx LIMIT ?1")
    else {
        return vec![];
    };
    let Ok(rows) = st.query_map([limit * 4], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    }) else {
        return vec![];
    };
    let mut out = vec![];
    let mut ring: Vec<(f64, f64)> = vec![];
    let mut cur = i64::MIN;
    let close = |ring: &mut Vec<(f64, f64)>, out: &mut Vec<[f64; 4]>| {
        let n = ring.len();
        if n > 1 {
            for i in 0..n {
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                out.push([p.0, p.1, q.0, q.1]);
            }
        }
        ring.clear();
    };
    for (s, x, y) in rows.flatten() {
        if s != cur {
            close(&mut ring, &mut out);
            cur = s;
        }
        ring.push((x, y));
    }
    close(&mut ring, &mut out);
    out
}

/// Everything a picture of the model is made of, from a connection to its database.
pub fn db_lines(db: &Connection, blobs: bool) -> Vec<[f64; 4]> {
    let mut v: Vec<[f64; 4]> = vec![];
    if exists(db, "wall") {
        if let Ok(mut st) = db.prepare("SELECT ax, ay, bx, by FROM wall LIMIT ?1") {
            if let Ok(rows) = st.query_map([MAX_ITEMS], |r| {
                Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?])
            }) {
                v.extend(rows.flatten());
            }
        }
    }
    v.extend(slab_lines(db, MAX_ITEMS / 4));
    let mut boxes = mesh_boxes(db, MAX_ITEMS);
    if boxes.is_empty() && blobs {
        boxes = blob_boxes(db, 3000);
    }
    v.extend(boxes.into_iter().flat_map(rect_lines));
    v
}

/// The early view of a build: lines of the half-written file, refreshed from a helper thread.
#[derive(Default)]
pub struct Early {
    /// The build file; set by the worker once it exists.
    pub path: Arc<Mutex<Option<PathBuf>>>,
    rx: Option<Receiver<Vec<[f64; 4]>>>,
    asked: Option<Instant>,
    tex: Option<egui::TextureHandle>,
    n: usize,
}

impl Early {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Per frame while a build runs; `reading` is false in stages that do not write yet.
    pub fn poll(&mut self, ctx: &egui::Context, writing: bool) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(lines) => {
                    self.rx = None;
                    self.n = lines.len();
                    if let Some(px) = render_thumb(&lines, THUMB_SIZE) {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [THUMB_SIZE.0 as usize, THUMB_SIZE.1 as usize],
                            &px,
                        );
                        self.tex = Some(ctx.load_texture("o3.early", img, Default::default()));
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }
        if !writing || self.rx.is_some() || self.asked.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        let Some(path) = self.path.lock().ok().and_then(|p| p.clone()) else {
            return;
        };
        self.asked = Some(Instant::now());
        let (tx, rx) = channel();
        self.rx = Some(rx);
        // read-only connection; a crash drops the sender and the next tick asks again
        let _ = crate::jobs::spawn_guarded(
            "open-preview",
            move || {
                let _ = tx.send(read_lines(&path));
            },
            |_| {},
        );
    }

    pub fn texture(&self) -> Option<&egui::TextureHandle> {
        self.tex.as_ref()
    }

    /// Lines drawn in the last picture.
    pub fn items(&self) -> usize {
        self.n
    }
}

/// Lines of the database at `path`, opened read-only; an unreadable file gives none.
pub fn read_lines(path: &Path) -> Vec<[f64; 4]> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(path, flags) {
        Ok(db) => {
            let _ = db.busy_timeout(Duration::from_millis(200));
            db_lines(&db, true)
        }
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(p: &[[f64; 3]]) -> Vec<u8> {
        p.iter().flatten().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn blob_box_is_the_plan_extent() {
        let b = blob(&[[1.0, 2.0, 0.0], [4.0, -1.0, 9.0], [2.0, 0.0, 1.0]]);
        assert_eq!(blob_box(&b), Some([1.0, -1.0, 4.0, 2.0]));
        assert_eq!(blob_box(&[]), None);
        assert_eq!(blob_box(&blob(&[[f64::NAN, 0.0, 0.0]])), None);
    }

    #[test]
    fn rect_has_four_closed_sides() {
        let l = rect_lines([0.0, 0.0, 2.0, 1.0]);
        assert_eq!(l[0], [0.0, 0.0, 2.0, 0.0]);
        assert_eq!(l[3][2..], [0.0, 0.0]);
    }

    #[test]
    fn mesh_only_model_has_a_picture() {
        let m = ddc_model::Model::open(":memory:").unwrap();
        let db = m.sql();
        assert!(db_lines(db, true).is_empty());
        let p = blob(&[[0.0, 0.0, 0.0], [3.0, 2.0, 1.0]]);
        db.execute(
            "INSERT INTO mesh(level, category, name, positions, indices) VALUES (NULL,'x','a',?1,?2)",
            (p, vec![0u8; 12]),
        )
        .unwrap();
        // no box cache yet: the blobs give the box, the cached boxes need none
        assert_eq!(db_lines(db, true).len(), 4);
        assert!(db_lines(db, false).is_empty());
    }
}
