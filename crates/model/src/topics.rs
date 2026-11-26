// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Issues (BCF topics) kept in the project file: title, status, priority, assignee, text,
//! comments, linked elements, a camera and an optional snapshot (schema 44, additive).
//! Outside the undo journal like `clash_item`: the rows are a review log, not model content, so
//! writing one neither moves the model head nor makes an open plan stale.
//! Elements are kept as (table, id) plus the external guid when the model has one (`ext_ref`);
//! a guid that matched no element stays as a row with an empty table, so a re-export loses nothing.

use web_time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::topic_clip::ClipPlane;
use crate::{Error, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS issue(id INTEGER PRIMARY KEY, guid TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL, status TEXT NOT NULL, priority TEXT NOT NULL,
    assignee TEXT NOT NULL DEFAULT '', description TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL, modified INTEGER NOT NULL,
    source TEXT NOT NULL DEFAULT '', source_key TEXT NOT NULL DEFAULT '',
    viewpoint TEXT, snapshot BLOB);
CREATE TABLE IF NOT EXISTS issue_elem(issue INTEGER NOT NULL, pos INTEGER NOT NULL,
    tbl TEXT NOT NULL DEFAULT '', ref INTEGER NOT NULL DEFAULT 0, guid TEXT,
    PRIMARY KEY(issue, pos));
CREATE TABLE IF NOT EXISTS issue_comment(id INTEGER PRIMARY KEY, issue INTEGER NOT NULL,
    guid TEXT NOT NULL, ts INTEGER NOT NULL, author TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS issue_comment_issue ON issue_comment(issue);
CREATE TABLE IF NOT EXISTS issue_doc(issue INTEGER NOT NULL, guid TEXT NOT NULL,
    name TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY(issue, guid));
";

/// Step 45: additive; every statement is `IF NOT EXISTS`, so merge order is free.
/// Later additive: `issue.bcf` (the exchange fields as JSON) and the embedded `issue_doc`s.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    let has_bcf = db
        .prepare("SELECT 1 FROM pragma_table_info('issue') WHERE name='bcf'")?
        .exists([])?;
    if !has_bcf {
        db.execute_batch("ALTER TABLE issue ADD COLUMN bcf TEXT")?;
    }
    Ok(())
}

/// Exchange fields of a topic the editor shows little of but keeps, so a file read and
/// written again says the same: type, labels, due date and stage as written, reference links,
/// document references, and the status and priority words of the file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicBcf {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// `xs:dateTime` as the file wrote it; empty when none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub due: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<TopicDoc>,
    /// The file's word for the status and the priority; written back while the stored
    /// status and priority still mean the same.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_word: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub priority_word: String,
}

impl TopicBcf {
    pub fn is_empty(&self) -> bool {
        *self == TopicBcf::default()
    }
}

/// A document reference: an outside address or a document carried in the file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicDoc {
    pub guid: String,
    /// Outside address (empty for a carried document).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<TopicDocFile>,
}

/// A carried document; its bytes live in `issue_doc`, not in the JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicDocFile {
    pub guid: String,
    pub name: String,
    #[serde(skip)]
    pub data: Vec<u8>,
}

/// Cap on one carried document, bytes.
pub const DOC_MAX: usize = 32 << 20;
const LIST_MAX: usize = 1000;

pub const STATUSES: [&str; 4] = ["open", "in_progress", "resolved", "closed"];
pub const PRIORITIES: [&str; 4] = ["low", "normal", "high", "critical"];
/// Cap on a snapshot kept in the file, bytes.
pub const SNAPSHOT_MAX: usize = 8 << 20;
const TEXT_MAX: usize = 20_000;
/// Height given to an element whose place is a plan outline, when framing it.
const FRAME_HEIGHT: f64 = 3.0;
const FRAME_FOV_DEG: f64 = 45.0;

/// Camera of an issue, in BCF terms: metres, Z up. `fov` is the vertical field of view in
/// degrees; `scale` is the visible height in world units (orthographic only).
/// `clip` are the section planes, `aspect` the width/height of the view (0 when unknown) and
/// `orbit` the editor camera it was taken from: BCF has no pivot, so the orbit keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Viewpoint {
    pub eye: [f64; 3],
    pub dir: [f64; 3],
    pub up: [f64; 3],
    pub ortho: bool,
    pub fov: f64,
    pub scale: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clip: Vec<ClipPlane>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub aspect: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orbit: Option<Orbit>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// Orbit camera parameters, the editor's own form of a camera.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Orbit {
    pub target: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub fov_y: f64,
    pub ortho: bool,
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    (n.is_finite() && n > 1e-9).then(|| [a[0] / n, a[1] / n, a[2] / n])
}

impl Viewpoint {
    pub fn from_orbit(o: &Orbit) -> Viewpoint {
        let (sp, cp) = o.pitch.sin_cos();
        let (sy, cy) = o.yaw.sin_cos();
        let back = [cp * cy, cp * sy, sp];
        let dir = [-back[0], -back[1], -back[2]];
        let right = [-sy, cy, 0.0];
        let t = o.target;
        let d = o.distance;
        Viewpoint {
            eye: [t[0] + back[0] * d, t[1] + back[1] * d, t[2] + back[2] * d],
            dir,
            up: cross(right, dir),
            ortho: o.ortho,
            fov: o.fov_y.to_degrees(),
            scale: if o.ortho { 2.0 * d * (o.fov_y * 0.5).tan() } else { 0.0 },
            clip: vec![],
            aspect: 0.0,
            orbit: Some(*o),
        }
    }

    /// The camera must be finite with a usable direction and up vector; section planes need a
    /// finite place and a direction.
    pub fn is_valid(&self) -> bool {
        let finite = self
            .eye
            .iter()
            .chain(&self.dir)
            .chain(&self.up)
            .chain([&self.fov, &self.scale, &self.aspect])
            .all(|v| v.is_finite());
        finite
            && unit(self.dir).is_some()
            && unit(self.up).is_some()
            && self.fov > 0.0
            && self.fov < 180.0
            && self.aspect >= 0.0
            && self.clip.iter().all(ClipPlane::is_valid)
    }

    /// Same picture: eye, view direction, up vector, projection and size within `tol`
    /// (directions compared as unit vectors). Section planes and the orbit are not compared.
    pub fn same_camera(&self, o: &Viewpoint, tol: f64) -> bool {
        let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|i| (a[i] - b[i]).abs() <= tol);
        let dirs = |a: [f64; 3], b: [f64; 3]| match (unit(a), unit(b)) {
            (Some(x), Some(y)) => near(x, y),
            _ => false,
        };
        self.ortho == o.ortho
            && near(self.eye, o.eye)
            && dirs(self.dir, o.dir)
            && dirs(self.up, o.up)
            && if self.ortho {
                (self.scale - o.scale).abs() <= tol
            } else {
                (self.fov - o.fov).abs() <= tol
            }
    }

    /// Orbit form: `hint` is the distance to the target for a perspective camera (an orthographic
    /// one takes it from `scale`). The stored orbit comes back as it was when it still gives this
    /// camera. Returns `None` for an invalid camera.
    pub fn to_orbit(&self, hint: f64) -> Option<Orbit> {
        if !self.is_valid() {
            return None;
        }
        if let Some(o) = self.orbit {
            if Viewpoint::from_orbit(&o).same_camera(self, 1e-9) {
                return Some(o);
            }
        }
        let dir = unit(self.dir)?;
        let up = unit(self.up)?;
        let back = [-dir[0], -dir[1], -dir[2]];
        let pitch = back[2].clamp(-1.0, 1.0).asin();
        let yaw = if pitch.cos().abs() > 1e-6 {
            back[1].atan2(back[0])
        } else if back[2] > 0.0 {
            (-up[1]).atan2(-up[0])
        } else {
            up[1].atan2(up[0])
        };
        let fov_y = self.fov.to_radians();
        let distance = if self.ortho && self.scale > 0.0 {
            self.scale * 0.5 / (fov_y * 0.5).tan()
        } else {
            hint.max(0.1)
        };
        let target = [
            self.eye[0] + dir[0] * distance,
            self.eye[1] + dir[1] * distance,
            self.eye[2] + dir[2] * distance,
        ];
        Some(Orbit {
            target,
            yaw,
            pitch,
            distance,
            fov_y,
            ortho: self.ortho,
        })
    }
}

/// A linked element. `table` is empty when the guid matched nothing in this model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicElem {
    pub table: String,
    pub id: i64,
    pub guid: Option<String>,
}

impl TopicElem {
    pub fn local(table: &str, id: i64) -> Self {
        TopicElem {
            table: table.to_string(),
            id,
            guid: None,
        }
    }
    pub fn external(guid: &str) -> Self {
        TopicElem {
            table: String::new(),
            id: 0,
            guid: Some(guid.to_string()),
        }
    }
    pub fn matched(&self) -> bool {
        !self.table.is_empty() && self.id > 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicComment {
    pub guid: String,
    /// Unix seconds.
    pub ts: i64,
    pub author: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Topic {
    pub id: i64,
    pub guid: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub assignee: String,
    pub description: String,
    pub author: String,
    pub created: i64,
    pub modified: i64,
    /// Where it came from: `check:<rule>`, `clash`, `selection`, `ai`, `bcf` or empty.
    pub source: String,
    /// Identity inside the source (a finding key, a clash pair) so one cause makes one issue.
    pub source_key: String,
    pub viewpoint: Option<Viewpoint>,
    pub snapshot: Option<Vec<u8>>,
    pub elements: Vec<TopicElem>,
    pub comments: Vec<TopicComment>,
    pub bcf: TopicBcf,
}

/// The exchange fields cleaned for storage: one-line words, no empty or repeated list items.
fn clean_bcf(b: &TopicBcf) -> Result<TopicBcf, Error> {
    let words = |v: &[String]| -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for w in v.iter().map(|s| one_line(s)).filter(|s| !s.is_empty()) {
            if !out.contains(&w) && out.len() < LIST_MAX {
                out.push(w);
            }
        }
        out
    };
    let mut docs = vec![];
    for d in b.docs.iter().take(LIST_MAX) {
        if let Some(f) = &d.file {
            if f.data.len() > DOC_MAX {
                return Err(Error::Invalid("issue document is too large".into()));
            }
            if one_line(&f.guid).is_empty() || one_line(&f.name).is_empty() {
                return Err(Error::Invalid("issue document without a guid or name".into()));
            }
        } else if one_line(&d.url).is_empty() {
            continue;
        }
        docs.push(TopicDoc {
            guid: one_line(&d.guid),
            url: one_line(&d.url),
            description: clip(&d.description),
            file: d.file.as_ref().map(|f| TopicDocFile {
                guid: one_line(&f.guid),
                name: one_line(&f.name),
                data: f.data.clone(),
            }),
        });
    }
    Ok(TopicBcf {
        topic_type: one_line(&b.topic_type),
        labels: words(&b.labels),
        due: one_line(&b.due),
        stage: one_line(&b.stage),
        links: words(&b.links),
        docs,
        status_word: one_line(&b.status_word),
        priority_word: one_line(&b.priority_word),
    })
}

/// Stores the exchange fields of issue `id` (replacing its documents).
fn put_bcf(tx: &Connection, id: i64, b: &TopicBcf) -> Result<(), Error> {
    let json = (!b.is_empty()).then(|| serde_json::to_string(b)).transpose()?;
    tx.execute("UPDATE issue SET bcf=?2 WHERE id=?1", params![id, json])?;
    tx.execute("DELETE FROM issue_doc WHERE issue=?1", [id])?;
    for f in b.docs.iter().filter_map(|d| d.file.as_ref()) {
        tx.execute(
            "INSERT OR REPLACE INTO issue_doc(issue, guid, name, data) VALUES (?1,?2,?3,?4)",
            params![id, f.guid, f.name, f.data],
        )?;
    }
    Ok(())
}

fn get_bcf(db: &Connection, id: i64, json: Option<String>) -> Result<TopicBcf, Error> {
    let mut b: TopicBcf = json.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut st = db.prepare_cached("SELECT data FROM issue_doc WHERE issue=?1 AND guid=?2")?;
    for f in b.docs.iter_mut().filter_map(|d| d.file.as_mut()) {
        f.data = st
            .query_row(params![id, f.guid], |r| r.get(0))
            .optional()?
            .unwrap_or_default();
    }
    Ok(b)
}

impl Topic {
    pub fn new(title: &str) -> Topic {
        Topic {
            title: title.to_string(),
            status: STATUSES[0].to_string(),
            priority: PRIORITIES[1].to_string(),
            ..Topic::default()
        }
    }
}

/// One list row: no snapshot or comment text, cheap for a panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicRow {
    pub id: i64,
    pub guid: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub assignee: String,
    pub modified: i64,
    pub source: String,
    pub comments: usize,
    pub elements: usize,
    pub has_view: bool,
    pub has_snapshot: bool,
}

/// Fields the panel edits; `None` keeps the stored value.
#[derive(Clone, Debug, Default)]
pub struct TopicEdit {
    pub title: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopicImport {
    Created(i64),
    /// The guid was already in the file: its fields were replaced, new comments appended.
    Updated(i64),
}

impl TopicImport {
    pub fn id(self) -> i64 {
        match self {
            TopicImport::Created(i) | TopicImport::Updated(i) => i,
        }
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn clip(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
    s.chars().take(TEXT_MAX).collect()
}

fn one_line(s: &str) -> String {
    clip(s).replace(['\n', '\t'], " ").trim().to_string()
}

fn fresh_guid(seed: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    crate::new_guid(&format!("issue|{seed}|{t}|{n}|{}", crate::process_id()))
}

fn is_element_table(t: &str) -> bool {
    crate::phases::PHASED.iter().any(|p| *p == t)
}

impl Model {
    /// Fills the guid of local elements and the element of known guids. Returns how many
    /// entries stay unmatched (no element here for their guid).
    pub fn topic_resolve(&self, elems: &mut [TopicElem]) -> Result<usize, Error> {
        let db = self.sql();
        let mut by_ref = db.prepare("SELECT guid FROM ext_ref WHERE kind=?1 AND ref=?2 LIMIT 1")?;
        let mut by_guid = db.prepare("SELECT kind, ref FROM ext_ref WHERE guid=?1 LIMIT 1")?;
        let mut unmatched = 0;
        for e in elems.iter_mut() {
            if e.matched() {
                if e.guid.is_none() {
                    e.guid = by_ref
                        .query_row(params![e.table, e.id], |r| r.get(0))
                        .optional()?;
                }
                continue;
            }
            let Some(g) = e.guid.clone() else {
                unmatched += 1;
                continue;
            };
            let hit: Option<(String, i64)> = by_guid
                .query_row([&g], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match hit.filter(|(k, _)| is_element_table(k)) {
                Some((k, id)) if self.topic_elem_alive(&k, id) => {
                    e.table = k;
                    e.id = id;
                }
                _ => unmatched += 1,
            }
        }
        Ok(unmatched)
    }

    /// Whether the element row still exists (a linked element may be deleted later).
    pub fn topic_elem_alive(&self, table: &str, id: i64) -> bool {
        is_element_table(table)
            && self
                .sql()
                .query_row(&format!("SELECT 1 FROM \"{table}\" WHERE id=?1"), [id], |_| Ok(()))
                .optional()
                .map(|r| r.is_some())
                .unwrap_or(false)
    }

    /// Adds an issue (a new guid when empty). Returns its id.
    pub fn topic_add(&self, t: &Topic) -> Result<i64, Error> {
        self.topic_write(t, None).map(|(id, _)| id)
    }

    fn topic_write(&self, t: &Topic, replace: Option<i64>) -> Result<(i64, bool), Error> {
        let title = one_line(&t.title);
        if title.is_empty() {
            return Err(Error::Invalid("issue title is empty".into()));
        }
        if let Some(v) = &t.viewpoint {
            if !v.is_valid() {
                return Err(Error::Invalid("issue camera is not valid".into()));
            }
        }
        if t.snapshot.as_ref().is_some_and(|s| s.len() > SNAPSHOT_MAX) {
            return Err(Error::Invalid("issue snapshot is too large".into()));
        }
        let mut elems = t.elements.clone();
        self.topic_resolve(&mut elems)?;
        let now = now_secs();
        let guid = if t.guid.is_empty() {
            fresh_guid(&title)
        } else {
            t.guid.clone()
        };
        let status = if t.status.trim().is_empty() { STATUSES[0] } else { t.status.trim() };
        let priority = if t.priority.trim().is_empty() { PRIORITIES[1] } else { t.priority.trim() };
        let created = if t.created > 0 { t.created } else { now };
        let modified = if t.modified > 0 { t.modified } else { now };
        let vp = t.viewpoint.as_ref().map(serde_json::to_string).transpose()?;
        let bcf = clean_bcf(&t.bcf)?;
        let db = self.sql();
        let tx = db.unchecked_transaction()?;
        let (id, updated) = match replace {
            Some(id) => {
                tx.execute(
                    "UPDATE issue SET title=?2, status=?3, priority=?4, assignee=?5, \
                     description=?6, author=?7, created=?8, modified=?9, viewpoint=?10, \
                     snapshot=?11 WHERE id=?1",
                    params![
                        id,
                        title,
                        one_line(status),
                        one_line(priority),
                        one_line(&t.assignee),
                        clip(&t.description),
                        one_line(&t.author),
                        created,
                        modified,
                        vp,
                        t.snapshot
                    ],
                )?;
                tx.execute("DELETE FROM issue_elem WHERE issue=?1", [id])?;
                (id, true)
            }
            None => {
                tx.execute(
                    "INSERT INTO issue(guid,title,status,priority,assignee,description,author,\
                     created,modified,source,source_key,viewpoint,snapshot) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    params![
                        guid,
                        title,
                        one_line(status),
                        one_line(priority),
                        one_line(&t.assignee),
                        clip(&t.description),
                        one_line(&t.author),
                        created,
                        modified,
                        one_line(&t.source),
                        one_line(&t.source_key),
                        vp,
                        t.snapshot
                    ],
                )?;
                (tx.last_insert_rowid(), false)
            }
        };
        for (pos, e) in elems.iter().enumerate() {
            tx.execute(
                "INSERT INTO issue_elem(issue,pos,tbl,ref,guid) VALUES (?1,?2,?3,?4,?5)",
                params![id, pos as i64, e.table, e.id, e.guid],
            )?;
        }
        for c in &t.comments {
            insert_comment(&tx, id, c)?;
        }
        put_bcf(&tx, id, &bcf)?;
        tx.commit()?;
        Ok((id, updated))
    }

    /// Import: an issue whose guid is already here is replaced field by field (comments are
    /// merged by guid), any other is added.
    /// A camera equal to the stored one keeps the stored orbit and aspect, which BCF does not
    /// carry, so exporting and importing again loses nothing.
    pub fn topic_import(&self, t: &Topic) -> Result<TopicImport, Error> {
        match self.topic_by_guid(&t.guid)? {
            Some(id) => {
                let old = self.topic_get(id)?.viewpoint;
                let mut t = t.clone();
                if let (Some(v), Some(o)) = (t.viewpoint.as_mut(), old) {
                    if v.same_camera(&o, 1e-9) {
                        // the stored numbers, not the text round trip of them
                        (v.eye, v.dir, v.up, v.fov, v.scale) = (o.eye, o.dir, o.up, o.fov, o.scale);
                        if v.orbit.is_none() {
                            v.orbit = o.orbit;
                        }
                        if v.aspect == 0.0 {
                            v.aspect = o.aspect;
                        }
                    }
                }
                self.topic_write(&t, Some(id))?;
                Ok(TopicImport::Updated(id))
            }
            None => Ok(TopicImport::Created(self.topic_write(t, None)?.0)),
        }
    }

    pub fn topic_by_guid(&self, guid: &str) -> Result<Option<i64>, Error> {
        if guid.is_empty() {
            return Ok(None);
        }
        Ok(self
            .sql()
            // BCF 3.0 writes guids in lower case: a topic from an upper-case file still matches
            .query_row(
                "SELECT id FROM issue WHERE lower(guid)=lower(?1) ORDER BY guid<>?1, id LIMIT 1",
                [guid],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The issue made earlier from the same cause, if any.
    pub fn topic_by_source(&self, source: &str, key: &str) -> Result<Option<i64>, Error> {
        if key.is_empty() {
            return Ok(None);
        }
        Ok(self
            .sql()
            .query_row(
                "SELECT id FROM issue WHERE source=?1 AND source_key=?2 ORDER BY id LIMIT 1",
                [source, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn topic_get(&self, id: i64) -> Result<Topic, Error> {
        let db = self.sql();
        let mut bcf_json: Option<String> = None;
        let mut t = db
            .query_row(
                "SELECT id,guid,title,status,priority,assignee,description,author,created,\
                 modified,source,source_key,viewpoint,snapshot,bcf FROM issue WHERE id=?1",
                [id],
                |r| {
                    let vp: Option<String> = r.get(12)?;
                    bcf_json = r.get(14)?;
                    Ok(Topic {
                        id: r.get(0)?,
                        guid: r.get(1)?,
                        title: r.get(2)?,
                        status: r.get(3)?,
                        priority: r.get(4)?,
                        assignee: r.get(5)?,
                        description: r.get(6)?,
                        author: r.get(7)?,
                        created: r.get(8)?,
                        modified: r.get(9)?,
                        source: r.get(10)?,
                        source_key: r.get(11)?,
                        viewpoint: vp.and_then(|s| serde_json::from_str(&s).ok()),
                        snapshot: r.get(13)?,
                        ..Topic::default()
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such issue {id}")))?;
        let mut st =
            db.prepare("SELECT tbl, ref, guid FROM issue_elem WHERE issue=?1 ORDER BY pos")?;
        t.elements = st
            .query_map([id], |r| {
                Ok(TopicElem {
                    table: r.get(0)?,
                    id: r.get(1)?,
                    guid: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut st = db.prepare(
            "SELECT guid, ts, author, text FROM issue_comment WHERE issue=?1 ORDER BY ts, id",
        )?;
        t.comments = st
            .query_map([id], |r| {
                Ok(TopicComment {
                    guid: r.get(0)?,
                    ts: r.get(1)?,
                    author: r.get(2)?,
                    text: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        t.bcf = get_bcf(db, id, bcf_json)?;
        Ok(t)
    }

    /// Every issue, full, oldest first (export).
    pub fn topic_all(&self) -> Result<Vec<Topic>, Error> {
        let ids: Vec<i64> = {
            let mut st = self.sql().prepare("SELECT id FROM issue ORDER BY id")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        ids.into_iter().map(|i| self.topic_get(i)).collect()
    }

    /// List rows, newest change first; `status` keeps one status (exact match).
    pub fn topic_list(&self, status: Option<&str>) -> Result<Vec<TopicRow>, Error> {
        let mut st = self.sql().prepare(
            "SELECT id,guid,title,status,priority,assignee,modified,source,\
             (SELECT count(*) FROM issue_comment c WHERE c.issue=issue.id),\
             (SELECT count(*) FROM issue_elem e WHERE e.issue=issue.id),\
             viewpoint IS NOT NULL, snapshot IS NOT NULL \
             FROM issue WHERE ?1 IS NULL OR status=?1 ORDER BY modified DESC, id DESC",
        )?;
        let rows = st
            .query_map([status], |r| {
                Ok(TopicRow {
                    id: r.get(0)?,
                    guid: r.get(1)?,
                    title: r.get(2)?,
                    status: r.get(3)?,
                    priority: r.get(4)?,
                    assignee: r.get(5)?,
                    modified: r.get(6)?,
                    source: r.get(7)?,
                    comments: r.get::<_, i64>(8)? as usize,
                    elements: r.get::<_, i64>(9)? as usize,
                    has_view: r.get(10)?,
                    has_snapshot: r.get(11)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn topic_edit(&self, id: i64, e: &TopicEdit) -> Result<(), Error> {
        let mut cur = self.topic_get(id)?;
        if let Some(v) = &e.title {
            if one_line(v).is_empty() {
                return Err(Error::Invalid("issue title is empty".into()));
            }
            cur.title = one_line(v);
        }
        if let Some(v) = &e.status {
            cur.status = one_line(v);
        }
        if let Some(v) = &e.priority {
            cur.priority = one_line(v);
        }
        if let Some(v) = &e.assignee {
            cur.assignee = one_line(v);
        }
        if let Some(v) = &e.description {
            cur.description = clip(v);
        }
        self.sql().execute(
            "UPDATE issue SET title=?2,status=?3,priority=?4,assignee=?5,description=?6,\
             modified=?7 WHERE id=?1",
            params![
                id,
                cur.title,
                cur.status,
                cur.priority,
                cur.assignee,
                cur.description,
                now_secs()
            ],
        )?;
        Ok(())
    }

    /// A camera that frames the linked elements from above the south-west, for a format that
    /// needs a camera in every viewpoint. `None` when no element has a place.
    pub fn topic_frame_view(&self, elems: &[TopicElem]) -> Result<Option<Viewpoint>, Error> {
        let keys: Vec<(&str, i64)> = elems
            .iter()
            .filter(|e| e.matched())
            .map(|e| (e.table.as_str(), e.id))
            .collect();
        let spots = crate::compare::spots_here(self.sql(), &keys)?;
        let Some(first) = spots.first() else {
            return Ok(None);
        };
        let mut lo = [first.min[0], first.min[1], first.z];
        let mut hi = [first.max[0], first.max[1], first.z + FRAME_HEIGHT];
        for s in &spots {
            for i in 0..2 {
                lo[i] = lo[i].min(s.min[i]);
                hi[i] = hi[i].max(s.max[i]);
            }
            lo[2] = lo[2].min(s.z);
            hi[2] = hi[2].max(s.z + FRAME_HEIGHT);
        }
        let half = ((0..3).map(|i| (hi[i] - lo[i]).powi(2)).sum::<f64>().sqrt() * 0.5).max(1.0);
        let fov_y = FRAME_FOV_DEG.to_radians();
        let o = Orbit {
            target: [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5],
            yaw: -0.75 * std::f64::consts::PI,
            pitch: 0.6,
            distance: half / (fov_y * 0.5).sin(),
            fov_y,
            ortho: false,
        };
        Ok(Some(Viewpoint::from_orbit(&o)).filter(Viewpoint::is_valid))
    }

    /// Replaces the camera and snapshot (both optional); the elements stay.
    pub fn topic_set_view(
        &self,
        id: i64,
        view: Option<&Viewpoint>,
        snapshot: Option<&[u8]>,
    ) -> Result<(), Error> {
        if view.is_some_and(|v| !v.is_valid()) {
            return Err(Error::Invalid("issue camera is not valid".into()));
        }
        if snapshot.is_some_and(|s| s.len() > SNAPSHOT_MAX) {
            return Err(Error::Invalid("issue snapshot is too large".into()));
        }
        let vp = view.map(serde_json::to_string).transpose()?;
        let n = self.sql().execute(
            "UPDATE issue SET viewpoint=?2, snapshot=?3, modified=?4 WHERE id=?1",
            params![id, vp, snapshot, now_secs()],
        )?;
        if n == 0 {
            return Err(Error::Invalid(format!("no such issue {id}")));
        }
        Ok(())
    }

    pub fn topic_comment_add(&self, id: i64, author: &str, text: &str) -> Result<i64, Error> {
        let text = clip(text);
        if text.trim().is_empty() {
            return Err(Error::Invalid("comment is empty".into()));
        }
        if self.topic_by_id_exists(id)? {
            let c = TopicComment {
                guid: fresh_guid("comment"),
                ts: now_secs(),
                author: one_line(author),
                text,
            };
            let db = self.sql();
            let n = insert_comment(db, id, &c)?;
            db.execute("UPDATE issue SET modified=?2 WHERE id=?1", params![id, c.ts])?;
            Ok(n)
        } else {
            Err(Error::Invalid(format!("no such issue {id}")))
        }
    }

    fn topic_by_id_exists(&self, id: i64) -> Result<bool, Error> {
        Ok(self
            .sql()
            .query_row("SELECT 1 FROM issue WHERE id=?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn topic_delete(&self, id: i64) -> Result<(), Error> {
        let tx = self.sql().unchecked_transaction()?;
        tx.execute("DELETE FROM issue_comment WHERE issue=?1", [id])?;
        tx.execute("DELETE FROM issue_elem WHERE issue=?1", [id])?;
        tx.execute("DELETE FROM issue_doc WHERE issue=?1", [id])?;
        tx.execute("DELETE FROM issue WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Label of this project written into exported components, so a reader of the same file can
    /// match elements that have no external guid.
    pub fn topic_project_tag(&self) -> String {
        self.sql()
            .query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| {
                r.get::<_, rusqlite::types::Value>(0)
            })
            .ok()
            .map(|v| match v {
                rusqlite::types::Value::Integer(i) => format!("{i:x}"),
                rusqlite::types::Value::Text(s) => s,
                _ => String::new(),
            })
            .unwrap_or_default()
    }
}

fn insert_comment(db: &Connection, issue: i64, c: &TopicComment) -> Result<i64, Error> {
    let guid = if c.guid.is_empty() {
        fresh_guid("comment")
    } else {
        c.guid.clone()
    };
    // a comment already stored under this guid is not added twice (re-import)
    let have: Option<i64> = db
        .query_row(
            "SELECT id FROM issue_comment WHERE issue=?1 AND guid=?2",
            params![issue, guid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = have {
        return Ok(id);
    }
    db.execute(
        "INSERT INTO issue_comment(issue,guid,ts,author,text) VALUES (?1,?2,?3,?4,?5)",
        params![
            issue,
            guid,
            if c.ts > 0 { c.ts } else { now_secs() },
            one_line(&c.author),
            clip(&c.text)
        ],
    )?;
    Ok(db.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const GUID: &str = "1abcDEF2ghiJKL3mnoPQR4";

    pub(crate) fn model_with_wall() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[crate::LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&crate::WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('s',?1,'wall',?2)",
                params![GUID, w],
            )
            .unwrap();
        (m, w)
    }

    #[test]
    fn orbit_viewpoint_round_trips() {
        // 1.5707963 is a pitch just short of straight down, not FRAC_PI_2
        #[allow(clippy::approx_constant)]
        let cases = [(-2.2, 0.6, false), (0.4, -0.3, true), (1.0, 1.5707963, false)];
        for (yaw, pitch, ortho) in cases {
            let o = Orbit {
                target: [1.0, 2.0, 3.0],
                yaw,
                pitch,
                distance: 12.0,
                fov_y: 45f64.to_radians(),
                ortho,
            };
            let v = Viewpoint::from_orbit(&o);
            assert!(v.is_valid());
            let b = v.to_orbit(12.0).unwrap();
            assert!((b.yaw - o.yaw).abs() < 1e-6 && (b.pitch - o.pitch).abs() < 1e-6);
            assert!((b.distance - 12.0).abs() < 1e-6);
            for i in 0..3 {
                assert!((b.target[i] - o.target[i]).abs() < 1e-6);
            }
        }
    }

    fn orbits() -> Vec<Orbit> {
        let mut v = vec![];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for i in 0..200 {
            v.push(Orbit {
                target: [next() * 400.0 - 200.0, next() * 400.0 - 200.0, next() * 60.0],
                yaw: next() * 6.2 - 3.1,
                pitch: next() * 3.0 - 1.5,
                distance: 0.5 + next() * 300.0,
                fov_y: (20.0 + next() * 70.0f64).to_radians(),
                ortho: i % 3 == 0,
            });
        }
        v
    }

    /// A stored issue view gives back the very orbit it was taken from, bit for bit, whatever
    /// distance the current view would suggest.
    #[test]
    fn a_stored_view_restores_the_camera_exactly() {
        let m = Model::open(":memory:").unwrap();
        for o in orbits() {
            let mut t = Topic::new("cam");
            let mut v = Viewpoint::from_orbit(&o);
            v.aspect = 1.6;
            v.clip = ClipPlane::of_box([-1.0, -2.0, 0.0], [5.0, 6.0, 3.0]);
            t.viewpoint = Some(v.clone());
            let id = m.topic_add(&t).unwrap();
            let back = m.topic_get(id).unwrap().viewpoint.unwrap();
            assert_eq!(back, v, "the stored view is the saved one");
            assert_eq!(back.to_orbit(123.0), Some(o), "exact orbit for {o:?}");
        }
    }

    /// Without the stored orbit (a file from elsewhere) the picture is still the same: eye,
    /// direction and up within float rounding.
    #[test]
    fn a_bare_camera_gives_the_same_picture() {
        for o in orbits() {
            let mut v = Viewpoint::from_orbit(&o);
            v.orbit = None;
            let b = v.to_orbit(o.distance * 0.37).unwrap();
            let again = Viewpoint::from_orbit(&b);
            let tol = 1e-9 * (1.0 + o.distance + o.target.iter().map(|x| x.abs()).sum::<f64>());
            assert!(again.same_camera(&v, tol), "{o:?}\n{v:?}\n{again:?}");
        }
    }

    #[test]
    fn an_edited_camera_does_not_take_the_stale_orbit() {
        let o = orbits()[1];
        let mut v = Viewpoint::from_orbit(&o);
        v.eye[0] += 1.0;
        let b = v.to_orbit(o.distance).unwrap();
        assert_ne!(b, o);
        assert!(Viewpoint::from_orbit(&b).same_camera(&v, 1e-6));
    }

    #[test]
    fn importing_the_same_camera_keeps_orbit_and_aspect() {
        let m = Model::open(":memory:").unwrap();
        let o = orbits()[5];
        let mut t = Topic::new("cam");
        t.guid = "0F8FAD5B-D9CB-469F-A165-70867728950E".into();
        let mut v = Viewpoint::from_orbit(&o);
        v.aspect = 1.25;
        t.viewpoint = Some(v.clone());
        let id = m.topic_add(&t).unwrap();
        // the same topic as a BCF file brings it back: lower-case guid, no orbit, no aspect
        let mut again = t.clone();
        again.guid = again.guid.to_lowercase();
        let mut bare = v.clone();
        bare.orbit = None;
        bare.aspect = 0.0;
        again.viewpoint = Some(bare.clone());
        assert_eq!(m.topic_import(&again).unwrap(), TopicImport::Updated(id));
        let got = m.topic_get(id).unwrap().viewpoint.unwrap();
        assert_eq!(got.orbit, Some(o));
        assert_eq!(got.aspect, 1.25);
        // a moved camera is a new camera
        bare.eye[2] += 2.0;
        again.viewpoint = Some(bare);
        m.topic_import(&again).unwrap();
        assert_eq!(m.topic_get(id).unwrap().viewpoint.unwrap().orbit, None);
    }

    fn exchange_fields() -> TopicBcf {
        TopicBcf {
            topic_type: "Clash".into(),
            labels: vec!["MEP".into(), "Level 2".into()],
            due: "2026-12-01T17:00:00+03:00".into(),
            stage: "Design".into(),
            links: vec!["https://example.org/issue/7".into()],
            docs: vec![
                TopicDoc {
                    guid: "d0000000-0000-4000-8000-000000000001".into(),
                    url: "https://example.org/spec.pdf".into(),
                    description: "Spec".into(),
                    file: None,
                },
                TopicDoc {
                    guid: "d0000000-0000-4000-8000-000000000002".into(),
                    url: String::new(),
                    description: "Markup".into(),
                    file: Some(TopicDocFile {
                        guid: "f0000000-0000-4000-8000-000000000003".into(),
                        name: "note.txt".into(),
                        data: b"bytes\0and more".to_vec(),
                    }),
                },
            ],
            status_word: "Active".into(),
            priority_word: "Major".into(),
        }
    }

    #[test]
    fn exchange_fields_and_documents_are_kept() {
        let m = Model::open(":memory:").unwrap();
        let mut t = Topic::new("fields");
        t.bcf = exchange_fields();
        let id = m.topic_add(&t).unwrap();
        assert_eq!(m.topic_get(id).unwrap().bcf, t.bcf);
        // an import of the same guid replaces them, documents included
        let mut again = m.topic_get(id).unwrap();
        again.bcf.labels = vec!["MEP".into(), " MEP ".into(), String::new()];
        again.bcf.docs.truncate(1);
        m.topic_import(&again).unwrap();
        let got = m.topic_get(id).unwrap().bcf;
        assert_eq!(got.labels, vec!["MEP".to_string()], "trimmed, no repeats or blanks");
        assert_eq!(got.docs.len(), 1);
        let left: i64 = m.sql().query_row("SELECT count(*) FROM issue_doc", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "the dropped document's bytes go too");
        // an issue without them stores no JSON
        let plain = m.topic_add(&Topic::new("plain")).unwrap();
        let json: Option<String> =
            m.sql().query_row("SELECT bcf FROM issue WHERE id=?1", [plain], |r| r.get(0)).unwrap();
        assert_eq!(json, None);
    }

    #[test]
    fn a_document_over_the_cap_is_refused() {
        let m = Model::open(":memory:").unwrap();
        let mut t = Topic::new("big");
        t.bcf.docs = vec![TopicDoc {
            guid: "g".into(),
            file: Some(TopicDocFile {
                guid: "f".into(),
                name: "big.bin".into(),
                data: vec![0; DOC_MAX + 1],
            }),
            ..TopicDoc::default()
        }];
        assert!(m.topic_add(&t).is_err());
    }

    #[test]
    fn a_camera_frames_the_linked_elements() {
        let (m, wall) = model_with_wall();
        let v = m
            .topic_frame_view(&[TopicElem::local("wall", wall), TopicElem::external("x")])
            .unwrap()
            .unwrap();
        assert!(v.is_valid() && !v.ortho);
        let o = v.orbit.unwrap();
        // the wall runs along the plan; the camera looks at the middle of its box
        let spot = crate::compare::spots_here(m.sql(), &[("wall", wall)]).unwrap()[0];
        assert!((o.target[0] - (spot.min[0] + spot.max[0]) * 0.5).abs() < 1e-9);
        assert!(v.dir[2] < 0.0, "looks down");
        assert!(m.topic_frame_view(&[TopicElem::external("x")]).unwrap().is_none());
    }

    #[test]
    fn straight_down_keeps_the_azimuth() {
        let o = Orbit {
            target: [0.0; 3],
            yaw: 0.7,
            pitch: std::f64::consts::FRAC_PI_2,
            distance: 5.0,
            fov_y: 0.8,
            ortho: false,
        };
        let b = Viewpoint::from_orbit(&o).to_orbit(5.0).unwrap();
        assert!((b.yaw - 0.7).abs() < 1e-6, "{}", b.yaw);
    }

    #[test]
    fn a_degenerate_camera_is_refused() {
        let mut v = Viewpoint::from_orbit(&Orbit {
            target: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            distance: 1.0,
            fov_y: 0.8,
            ortho: false,
        });
        v.dir = [0.0; 3];
        assert!(v.to_orbit(1.0).is_none());
        let m = Model::open(":memory:").unwrap();
        let mut t = Topic::new("x");
        t.viewpoint = Some(v);
        assert!(m.topic_add(&t).is_err());
    }

    #[test]
    fn add_get_edit_comment_delete() {
        let (m, wall) = model_with_wall();
        let mut t = Topic::new("  Door clash\n");
        t.assignee = "Anna".into();
        t.elements = vec![TopicElem::local("wall", wall)];
        let id = m.topic_add(&t).unwrap();
        let got = m.topic_get(id).unwrap();
        assert_eq!(got.title, "Door clash");
        assert_eq!(got.status, "open");
        assert_eq!(got.elements[0].guid.as_deref(), Some("1abcDEF2ghiJKL3mnoPQR4"));
        assert!(!got.guid.is_empty());
        m.topic_edit(
            id,
            &TopicEdit {
                status: Some("resolved".into()),
                ..TopicEdit::default()
            },
        )
        .unwrap();
        m.topic_comment_add(id, "Bob", "fixed it").unwrap();
        assert!(m.topic_comment_add(id, "Bob", "  ").is_err());
        assert!(m.topic_comment_add(999, "Bob", "x").is_err());
        let rows = m.topic_list(Some("resolved")).unwrap();
        assert_eq!((rows.len(), rows[0].comments, rows[0].elements), (1, 1, 1));
        assert!(m.topic_list(Some("open")).unwrap().is_empty());
        assert_eq!(m.topic_list(None).unwrap().len(), 1);
        m.topic_delete(id).unwrap();
        assert!(m.topic_get(id).is_err());
        let left: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM issue_comment", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn an_empty_title_is_refused() {
        let m = Model::open(":memory:").unwrap();
        assert!(m.topic_add(&Topic::new(" \n ")).is_err());
    }

    #[test]
    fn guids_resolve_to_elements_and_unknown_stay() {
        let (m, wall) = model_with_wall();
        let mut v = vec![
            TopicElem::external(GUID),
            TopicElem::external("0000000000000000000000"),
            TopicElem::local("wall", wall),
        ];
        assert_eq!(m.topic_resolve(&mut v).unwrap(), 1);
        assert_eq!((v[0].table.as_str(), v[0].id), ("wall", wall));
        assert!(!v[1].matched() && v[1].guid.is_some());
        assert_eq!(v[2].guid.as_deref(), Some(GUID));
        // a guid whose element row is gone does not match
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('s','stale','wall',99999)",
                [],
            )
            .unwrap();
        let mut gone = vec![TopicElem::external("stale")];
        assert_eq!(m.topic_resolve(&mut gone).unwrap(), 1);
    }

    #[test]
    fn same_cause_finds_the_same_issue() {
        let m = Model::open(":memory:").unwrap();
        let mut t = Topic::new("a");
        t.source = "clash".into();
        t.source_key = "k1".into();
        let id = m.topic_add(&t).unwrap();
        assert_eq!(m.topic_by_source("clash", "k1").unwrap(), Some(id));
        assert_eq!(m.topic_by_source("clash", "k2").unwrap(), None);
        assert_eq!(m.topic_by_source("clash", "").unwrap(), None);
    }

    #[test]
    fn import_replaces_by_guid_and_merges_comments() {
        let m = Model::open(":memory:").unwrap();
        let mut t = Topic::new("first");
        t.guid = "11111111-1111-1111-1111-111111111111".into();
        t.comments = vec![TopicComment {
            guid: "c1".into(),
            ts: 100,
            author: "A".into(),
            text: "one".into(),
        }];
        assert!(matches!(m.topic_import(&t).unwrap(), TopicImport::Created(_)));
        t.title = "second".into();
        t.comments.push(TopicComment {
            guid: "c2".into(),
            ts: 200,
            author: "B".into(),
            text: "two".into(),
        });
        let r = m.topic_import(&t).unwrap();
        assert!(matches!(r, TopicImport::Updated(_)));
        let got = m.topic_get(r.id()).unwrap();
        assert_eq!((got.title.as_str(), got.comments.len()), ("second", 2));
        assert_eq!(m.topic_list(None).unwrap().len(), 1);
    }

    #[test]
    fn tables_survive_reopen_and_migrate_twice() {
        let m = Model::open(":memory:").unwrap();
        migrate(m.sql()).unwrap();
        migrate(m.sql()).unwrap();
    }
}
