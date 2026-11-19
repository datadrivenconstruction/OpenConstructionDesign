// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! IFC patch back end: writes a changed copy of an imported file. Every statement that no edit
//! touches is copied byte for byte; changed elements get new geometry entities, their product
//! lines are rewritten by GlobalId, deleted ones leave their relations, new ones are appended above
//! the highest id. Anything uncertain returns [`PatchError::Doubt`] with no output kept: the caller
//! rewrites to a copy instead and says so. A doubt never replaces the source itself.
//! Contract: docs/design/OPEN_SAVE.md, section 6.3.

mod graft;
mod lines;
mod props;
mod refs;
mod types;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use ddc_model::source::file::hash_file;
use ddc_model::source::{ChangeKind, PendingLink, SourceRebase};
use ddc_model::Model;
use rusqlite::OptionalExtension;
use serde_json::Value as J;

use self::lines::{parse_ent, quick_id, single_ref, split_top, Stmts};
use self::refs::Index;
use super::export::sink::{ExportStats, NotExported};
use super::import::{file_units, Units};
use super::reader::{StepFile, StreamOptions};
use super::step::{compress_guid, hash128};
use crate::job::{Progress, CANCELLED};

pub const DOUBT_SOURCE: &str = "ifc_patch.fallback.source_changed";
pub const DOUBT_ZIP: &str = "ifc_patch.fallback.zipped";
pub const DOUBT_SOURCES: &str = "ifc_patch.fallback.sources";
pub const DOUBT_SCHEMA: &str = "ifc_patch.fallback.schema";
pub const DOUBT_STRUCTURE: &str = "ifc_patch.fallback.structure";
pub const DOUBT_EDIT: &str = "ifc_patch.fallback.edit";
pub const DOUBT_TYPES: &str = "ifc_patch.fallback.types";
/// A new level needs the one building of the file, and there is none or more than one.
pub const DOUBT_BUILDING: &str = "ifc_patch.fallback.building";
/// A new element has nested parts (ports and the like), which the patch does not write.
pub const DOUBT_NESTED: &str = "ifc_patch.fallback.nested";
/// A part of an edited stair or ramp is also used by something the patch cannot account for.
pub const DOUBT_SHARED: &str = "ifc_patch.fallback.shared_part";

pub const REASON_SCHEMA: &str = "ifc_patch.reason.schema";
pub const REASON_PROP: &str = "ifc_patch.reason.prop";
pub const REASON_EXPORT: &str = "ifc_patch.reason.no_export";
pub const REASON_LINK: &str = "ifc_patch.reason.link";

/// Kinds whose geometry the exporter and importer carry through one product line, or through a
/// whole and its aggregated parts (stairs and ramps).
const GEOM_KINDS: [&str; 12] = [
    "wall", "opening", "slab", "col", "beam", "mesh", "roof", "ceiling", "room", "stair", "ramp",
    "railing",
];
/// Kinds whose exported shape is local to the product placement, with the columns that move only
/// that placement. Stairs, railings and MEP runs are exported with world coordinates in the shape,
/// so a move of theirs changes the representation and stays a full edit.
const PLACE_FIELDS: [(&str, &[&str]); 1] = [("comp", &["x", "y", "z", "rotation", "level"])];
/// Model tables that can gain rows but are not in the journal of local additions.
const OTHER_TABLES: [&str; 8] = [
    "comp", "roof", "mep_run", "room_sep", "level", "stair", "railing", "ramp",
];
const SPATIAL: [&str; 4] = ["IFCPROJECT", "IFCSITE", "IFCBUILDING", "IFCBUILDINGSTOREY"];

#[derive(Clone, Debug, PartialEq)]
pub enum PatchError {
    /// The patch cannot be trusted; `key` is a catalog key for the report.
    Doubt { key: &'static str, detail: String },
    /// I/O, database or cancel: not a reason to change the writer.
    Failed(String),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::Doubt { detail, .. } => f.write_str(detail),
            PatchError::Failed(e) => f.write_str(e),
        }
    }
}

fn doubt(key: &'static str, detail: impl Into<String>) -> PatchError {
    PatchError::Doubt {
        key,
        detail: detail.into(),
    }
}

fn failed(e: impl ToString) -> PatchError {
    PatchError::Failed(e.to_string())
}

/// What the check of a finished patch expects to read back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expect {
    pub schema_name: String,
    pub guids: HashSet<String>,
    pub dangling: u64,
    pub dup_guids: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchOutput {
    pub stats: ExportStats,
    /// (kind, catalog key of the reason): edits the file's schema cannot hold.
    pub will_not: Vec<(String, String)>,
    /// (kind, row id or -1 for every row, field or "" for every field): the edits that are not in
    /// the output, one by one.
    pub unwritten: HashSet<(String, i64, String)>,
    pub edited: u64,
    pub added: u64,
    pub removed: u64,
    /// (link kind, row, guid) of elements the output holds and the model has no link for.
    pub linked: Vec<(String, i64, String)>,
    /// (guid, relation) of links the output still lacks, new elements and retried ones alike.
    pub pending_links: Vec<(String, String)>,
    /// Elements of the file whose pending links were tried again in this write.
    pub retried: Vec<String>,
    pub no_changes: bool,
    /// None when the output is a plain copy of the source.
    pub expect: Option<Expect>,
}

/// One property change of an element.
#[derive(Clone)]
struct PropEdit {
    pub guid: String,
    pub kind: String,
    pub id: i64,
    pub key: String,
    pub value: J,
}

/// One element whose type column changed; `name` is the new type's name in the model.
struct TypeEdit {
    pub guid: String,
    pub kind: String,
    pub id: i64,
    pub name: String,
}

/// One element with a geometry or attribute change.
struct GeomEdit {
    pub guid: String,
    pub kind: String,
    /// The model row, -1 when the edit is not one of its own.
    pub id: i64,
    pub renamed: bool,
    /// Only the placement changed: the file keeps its representation byte for byte.
    pub place_only: bool,
}

#[derive(Default)]
struct Work {
    pub deleted: Vec<String>,
    pub geom: Vec<GeomEdit>,
    pub props: Vec<PropEdit>,
    pub types: Vec<TypeEdit>,
    pub added: Vec<(String, i64)>,
    /// Elements already in the file that lack a link an earlier save could not write.
    pub relink: Vec<PendingLink>,
}

impl Work {
    fn is_empty(&self) -> bool {
        self.deleted.is_empty()
            && self.geom.is_empty()
            && self.props.is_empty()
            && self.types.is_empty()
            && self.added.is_empty()
            && self.relink.is_empty()
    }
}

/// Everything the patch will change in the output.
struct Out {
    next: u32,
    eol: &'static str,
    pub lines: Vec<u8>,
    pub replace: HashMap<u32, Vec<u8>>,
    pub remove: HashSet<u32>,
    pub will_not: Vec<(String, String)>,
    pub unwritten: HashSet<(String, i64, String)>,
    pub warnings: Vec<String>,
    pub new_guids: HashSet<String>,
    pub touched: Vec<String>,
    pub linked: Vec<(String, i64, String)>,
    pub pending_links: Vec<(String, String)>,
    pub retried: Vec<String>,
    /// Elements of the file whose shape changed, so their measured quantities no longer hold.
    pub stale: HashSet<u32>,
    pub by_kind: BTreeMap<String, u64>,
    pub edited: u64,
    pub added: u64,
    /// Parts of edited wholes that the model no longer has, removed with their lines.
    pub gone: u64,
}

impl Out {
    /// An edit the output does not hold: a row in the report, and the edit stays pending.
    /// `id` -1 and `field` "" stand for every row and every field.
    pub fn deny(&mut self, kind: &str, id: i64, field: &str, reason: &str) {
        self.will_not.push((kind.to_string(), reason.to_string()));
        self.unwritten
            .insert((kind.to_string(), id, field.to_string()));
    }

    /// A link of an appended element (material, type) the file cannot hold. The element itself is in
    /// the file and stays recorded as written (it is linked, so the next save edits it and never
    /// appends it again); only the link is listed as not written and kept pending under `~link`.
    pub fn deny_link(&mut self, table: &str, row: i64, guid: &str, rel: &str, why: &str) {
        self.warnings
            .push(format!("{guid}: {rel} not written, {why} does not fit the file"));
        self.deny(table, row, "~link", REASON_LINK);
        let key = (guid.to_string(), rel.to_string());
        if !self.pending_links.contains(&key) {
            self.pending_links.push(key);
        }
    }

    fn new(ix: &Index) -> Self {
        Self {
            next: ix.max_id,
            eol: if ix.crlf { "\r\n" } else { "\n" },
            lines: vec![],
            replace: HashMap::new(),
            remove: HashSet::new(),
            will_not: vec![],
            unwritten: HashSet::new(),
            warnings: vec![],
            new_guids: HashSet::new(),
            touched: vec![],
            linked: vec![],
            pending_links: vec![],
            retried: vec![],
            stale: HashSet::new(),
            by_kind: BTreeMap::new(),
            edited: 0,
            added: 0,
            gone: 0,
        }
    }

    pub fn alloc(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    pub fn put(&mut self, id: u32, ty: &str, args: &[u8]) {
        self.lines
            .extend_from_slice(format!("#{id}={ty}(").as_bytes());
        self.lines.extend_from_slice(args);
        self.lines.extend_from_slice(b");");
        self.lines.extend_from_slice(self.eol.as_bytes());
    }

    pub fn add(&mut self, ty: &str, args: &[u8]) -> u32 {
        let id = self.alloc();
        self.put(id, ty, args);
        id
    }

    /// Statement `id` becomes `TY(args)`.
    pub fn swap(&mut self, id: u32, ty: &str, args: &[u8]) {
        let mut b = format!("#{id}={ty}(").into_bytes();
        b.extend_from_slice(args);
        b.push(b')');
        self.replace.insert(id, b);
    }

    /// Arguments of a relation as this patch left them so far.
    pub fn current_args(&self, id: u32, orig: &[u8]) -> Vec<u8> {
        self.replace
            .get(&id)
            .and_then(|b| parse_ent(b).map(|e| e.args.to_vec()))
            .unwrap_or_else(|| orig.to_vec())
    }

    /// A GlobalId stable across saves, from `seed`.
    pub fn new_guid(&mut self, seed: &str, ix: &Index) -> String {
        let mut salt = 0u32;
        loop {
            let g = compress_guid(hash128(&format!("ddc-patch|{seed}|{salt}")));
            if !ix.rooted.contains_key(&g) && self.new_guids.insert(g.clone()) {
                return g;
            }
            salt += 1;
        }
    }

    pub fn count(&mut self, kind: &str, guid: &str) {
        *self.by_kind.entry(kind.to_string()).or_default() += 1;
        self.touched.push(guid.to_string());
    }
}

/// A row exists for this GlobalId in the model.
fn alive(model: &Model, source: &str, guid: &str) -> Result<bool, PatchError> {
    let sql = model.sql();
    let mut st = sql
        .prepare_cached("SELECT kind, ref FROM ext_ref WHERE source=?1 AND guid=?2")
        .map_err(failed)?;
    let rows: Vec<(String, i64)> = st
        .query_map([source, guid], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(failed)?
        .collect::<Result<_, _>>()
        .map_err(failed)?;
    for (kind, id) in rows {
        if matches!(kind.as_str(), "project" | "site" | "building") {
            return Ok(true);
        }
        let table = if kind == "opening_fill" {
            "opening"
        } else {
            kind.as_str()
        };
        if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(doubt(DOUBT_STRUCTURE, format!("unknown link kind {kind}")));
        }
        let q = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)");
        let n: i64 = sql.query_row(&q, [id], |r| r.get(0)).map_err(failed)?;
        if n != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(model: &Model, t: &str) -> Result<bool, PatchError> {
    let n: i64 = model
        .sql()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [t],
            |r| r.get(0),
        )
        .map_err(failed)?;
    Ok(n > 0)
}

/// A column that changes which other rows an element belongs to: the patch does not rebuild those.
/// The `kind` of a stair is its shape, which its parts follow.
fn is_link_field(table: &str, f: &str) -> bool {
    matches!(f, "wall" | "system") || (f == "kind" && table != "stair") || f.ends_with("type")
}

/// Every changed column of a `kind` row moves only its placement.
fn placement_only(kind: &str, fields: &[&str]) -> bool {
    PLACE_FIELDS
        .iter()
        .find(|(k, _)| *k == kind)
        .is_some_and(|(_, cols)| !fields.is_empty() && fields.iter().all(|f| cols.contains(f)))
}

/// Name of the type row a changed column now points at.
fn type_name(model: &Model, table: &str, v: &J) -> Result<Option<String>, PatchError> {
    let Some(id) = v.as_i64().or_else(|| v.as_f64().map(|x| x as i64)) else {
        return Ok(None);
    };
    model
        .sql()
        .query_row(&format!("SELECT name FROM {table} WHERE id=?1"), [id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(failed)
}

fn gather(model: &Model, source: &str) -> Result<Work, PatchError> {
    let mut w = Work::default();
    for c in model.local_changes(source, true).map_err(failed)? {
        let guid = c.ifc_guid.clone().unwrap_or_else(|| c.guid.clone());
        match c.change {
            ChangeKind::AddedLocally => w.added.push((c.kind, c.id)),
            ChangeKind::DeletedLocally => w.deleted.push(guid),
            ChangeKind::Edited => {
                let (mut geom, mut renamed) = (false, false);
                let mut fields: Vec<&str> = vec![];
                for f in &c.fields {
                    if let Some(key) = f.field.strip_prefix("p:") {
                        w.props.push(PropEdit {
                            guid: guid.clone(),
                            kind: c.kind.clone(),
                            id: c.id,
                            key: key.to_string(),
                            value: f.mine.clone(),
                        });
                    } else if !f.field.starts_with('~') {
                        let table = types::type_table(&f.field)
                            .filter(|_| GEOM_KINDS.contains(&c.kind.as_str()));
                        if let Some(table) = table {
                            let name = type_name(model, table, &f.mine)?.ok_or_else(|| {
                                doubt(
                                    DOUBT_EDIT,
                                    format!("{} {}: type not in the file", c.kind, c.id),
                                )
                            })?;
                            w.types.push(TypeEdit {
                                guid: guid.clone(),
                                kind: c.kind.clone(),
                                id: c.id,
                                name,
                            });
                            // another type has other layers or another profile
                            geom = true;
                            continue;
                        }
                        if is_link_field(&c.kind, &f.field) {
                            return Err(doubt(
                                DOUBT_EDIT,
                                format!("{} {}: field {}", c.kind, c.id, f.field),
                            ));
                        }
                        renamed |= f.field == "name";
                        geom = true;
                        fields.push(f.field.as_str());
                    }
                }
                if geom {
                    let place_only = !GEOM_KINDS.contains(&c.kind.as_str());
                    if place_only && !placement_only(&c.kind, &fields) {
                        return Err(doubt(
                            DOUBT_EDIT,
                            format!("{} {}: geometry edit", c.kind, c.id),
                        ));
                    }
                    w.geom.push(GeomEdit {
                        guid,
                        kind: c.kind,
                        id: c.id,
                        renamed,
                        place_only,
                    });
                }
            }
        }
    }
    w.relink = model.pending_links(source).map_err(failed)?;
    for t in OTHER_TABLES {
        // added rows of deletable kinds already came from `local_changes`: listing them twice
        // would append the element twice
        if ddc_model::source::DELETABLE.contains(&t) || !table_exists(model, t)? {
            continue;
        }
        let q = format!("SELECT id FROM {t} WHERE id NOT IN (SELECT ref FROM ext_ref WHERE kind='{t}') ORDER BY id");
        let mut st = model.sql().prepare(&q).map_err(failed)?;
        let ids: Vec<i64> = st
            .query_map([], |r| r.get(0))
            .map_err(failed)?
            .collect::<Result<_, _>>()
            .map_err(failed)?;
        w.added
            .extend(ids.into_iter().map(|id| (t.to_string(), id)));
    }
    Ok(w)
}

/// The file on disk is the one the model was imported from, or our own last write over it.
fn check_source(
    model: &Model,
    source: &str,
    src: &Path,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let Some(row) = model.source_file(source).map_err(failed)? else {
        return Err(doubt(DOUBT_SOURCE, format!("no record of source {source}")));
    };
    let stop = || ctl.is_some_and(|c| c.is_cancelled());
    let hash = hash_file(src, &stop)
        .map_err(|e| doubt(DOUBT_SOURCE, format!("{}: {e}", src.display())))?;
    if stop() {
        return Err(failed(CANCELLED));
    }
    if hash.is_empty() || (hash != row.content_hash && hash != row.written_hash) {
        return Err(doubt(
            DOUBT_SOURCE,
            format!("{} changed on disk", src.display()),
        ));
    }
    let mut head = [0u8; 4];
    let n = {
        use std::io::Read;
        File::open(src)
            .and_then(|mut f| f.read(&mut head))
            .map_err(failed)?
    };
    if super::zip::is_zip(&head[..n]) {
        return Err(doubt(DOUBT_ZIP, "a zipped source is not patched"));
    }
    let list = model.sources().map_err(failed)?;
    if list.len() != 1 {
        return Err(doubt(
            DOUBT_SOURCES,
            format!("{} sources in the project", list.len()),
        ));
    }
    Ok(())
}

/// Hash of `src`, when it is the file the model was read from or our own last write over it.
pub fn current_hash(
    model: &Model,
    source: &str,
    src: &Path,
    ctl: Option<&Arc<Progress>>,
) -> Option<String> {
    let row = model.source_file(source).ok().flatten()?;
    let stop = || ctl.is_some_and(|c| c.is_cancelled());
    let hash = hash_file(src, &stop).ok()?;
    (!hash.is_empty() && (hash == row.content_hash || hash == row.written_hash)).then_some(hash)
}

/// Levels, types and materials have no base rows, so `gather` cannot see their edits and the
/// patch cannot write them.
fn check_shared(model: &Model, source: &str) -> Result<(), PatchError> {
    match model.shared_rows_changed(source).map_err(failed)? {
        Some(false) => Ok(()),
        Some(true) => Err(doubt(
            DOUBT_TYPES,
            "levels, types or materials changed since the import",
        )),
        None => Err(doubt(
            DOUBT_TYPES,
            "the import left no record of levels, types and materials",
        )),
    }
}

/// Elements whose quantities a change makes wrong: the changed and removed ones, the hosts of
/// their openings and the wholes they are parts of.
fn quantity_owners(ix: &Index, o: &Out) -> HashSet<u32> {
    let mut work: Vec<u32> = o.stale.iter().chain(o.remove.iter()).copied().collect();
    let mut seen: HashSet<u32> = work.iter().copied().collect();
    while let Some(x) = work.pop() {
        let mut next = vec![];
        for ty in ["IFCRELVOIDSELEMENT", "IFCRELFILLSELEMENT"] {
            for r in ix.rels_of(x, ty) {
                let a = split_top(&r.args);
                if a.get(5).and_then(|v| single_ref(v)) == Some(x) {
                    next.extend(a.get(4).and_then(|v| single_ref(v)));
                }
            }
        }
        for r in ix.rels_of(x, "IFCRELAGGREGATES") {
            let a = split_top(&r.args);
            let whole = a.get(4).and_then(|v| single_ref(v)).filter(|w| *w != x);
            next.extend(whole.filter(|w| !ix.type_name(*w).is_some_and(|t| SPATIAL.contains(&t))));
        }
        for n in next {
            if seen.insert(n) {
                work.push(n);
            }
        }
    }
    seen.retain(|i| !o.remove.contains(i));
    seen
}

/// True when the model holds nothing the source lacks. Refuses as [`patch_file`] does.
pub fn has_changes(model: &Model, source: &str, src: &Path) -> Result<bool, PatchError> {
    has_changes_with(model, source, src, None)
}

/// [`has_changes`] that a cancel can stop.
pub fn has_changes_with(
    model: &Model,
    source: &str,
    src: &Path,
    ctl: Option<&Arc<Progress>>,
) -> Result<bool, PatchError> {
    check_source(model, source, src, ctl)?;
    check_shared(model, source)?;
    Ok(!gather(model, source)?.is_empty())
}

/// How the base moves once `p` is in the source's place, read from the model it was written from.
/// An edit the file could not hold stays as it is, and only that one: it is still to be written.
pub fn base_rebase(
    model: &Model,
    source: &str,
    p: &PatchOutput,
) -> Result<SourceRebase, PatchError> {
    let mut r = model
        .plan_source_rebase(source, &p.unwritten, &p.linked)
        .map_err(failed)?;
    let rels_of = |guid: &str| -> Vec<String> {
        p.pending_links
            .iter()
            .filter(|(g, _)| g == guid)
            .map(|(_, rel)| rel.clone())
            .collect()
    };
    for l in &mut r.linked {
        l.pending = rels_of(&l.guid);
    }
    for guid in &p.retried {
        r.relinked.push((guid.clone(), rels_of(guid)));
    }
    Ok(r)
}

/// Makes every link of every appended element unwritable on this thread (tests only).
#[cfg(feature = "test-hooks")]
pub fn set_links_blocked(on: bool) {
    LINKS_BLOCKED.with(|c| c.set(on));
}

#[cfg(feature = "test-hooks")]
thread_local! {
    static LINKS_BLOCKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn links_blocked() -> bool {
    #[cfg(feature = "test-hooks")]
    {
        LINKS_BLOCKED.with(|c| c.get())
    }
    #[cfg(not(feature = "test-hooks"))]
    {
        false
    }
}

fn units_of(src: &Path, ctl: Option<&Arc<Progress>>) -> Result<Units, PatchError> {
    let opt = StreamOptions {
        tolerant: true,
        ..Default::default()
    };
    let f = StepFile::open_with(src, &opt, ctl.cloned()).map_err(|e| match e {
        // a cancel or a read error is a failure, not a reason to doubt the file
        crate::ifc::OpenError::Cancelled => failed(CANCELLED),
        crate::ifc::OpenError::Io(e) => failed(e),
        e => doubt(DOUBT_STRUCTURE, format!("{e:?}")),
    })?;
    file_units(&f).map_err(|e| doubt(DOUBT_STRUCTURE, format!("units: {e:?}")))
}

/// Streams `src` to `out`, applying `o`. Unnamed bytes pass through untouched.
/// `created`: set once `out` is opened for writing (truncated), so only then may an error remove it.
fn copy(
    src: &Path,
    out: &Path,
    o: &Out,
    ctl: Option<&Arc<Progress>>,
    created: &mut bool,
) -> Result<u64, PatchError> {
    let f = File::open(src).map_err(failed)?;
    if let Some(c) = ctl {
        c.set_total(f.metadata().map_err(failed)?.len());
    }
    let mut st = Stmts::new(f);
    let mut w = BufWriter::with_capacity(1 << 20, File::create(out).map_err(failed)?);
    *created = true;
    let (mut in_data, mut appended, mut bytes, mut n) = (false, false, 0u64, 0u64);
    let mut hits = 0usize;
    let mut put = |w: &mut BufWriter<File>, b: &[u8]| -> Result<(), PatchError> {
        bytes += b.len() as u64;
        w.write_all(b).map_err(failed)
    };
    while let Some(p) = st.next().map_err(failed)? {
        n += 1;
        if n % 4096 == 0 {
            if let Some(c) = ctl {
                if c.is_cancelled() {
                    return Err(failed(CANCELLED));
                }
                c.add(4096 * 64);
            }
        }
        if !in_data {
            put(&mut w, p.lead)?;
            put(&mut w, p.body)?;
            in_data = !appended && p.body.starts_with(b"DATA");
            continue;
        }
        if p.body.starts_with(b"ENDSEC") {
            put(&mut w, p.lead)?;
            put(&mut w, &o.lines)?;
            put(&mut w, p.body)?;
            in_data = false;
            appended = true;
            continue;
        }
        let id = quick_id(p.body);
        if id.is_some_and(|i| o.remove.contains(&i)) {
            // a comment before a removed statement stays; bare white space goes with it
            if !p.lead.iter().all(u8::is_ascii_whitespace) {
                put(&mut w, p.lead)?;
            }
            hits += 1;
            continue;
        }
        put(&mut w, p.lead)?;
        match id.and_then(|i| o.replace.get(&i)) {
            Some(b) => {
                put(&mut w, b)?;
                put(&mut w, b";")?;
                hits += 1;
            }
            None => put(&mut w, p.body)?,
        }
    }
    if !appended {
        return Err(doubt(DOUBT_STRUCTURE, "no end of the DATA section"));
    }
    // a statement the copy never met is an edit that would silently not be in the file
    let want = o.remove.len() + o.replace.keys().filter(|i| !o.remove.contains(*i)).count();
    if hits != want {
        return Err(doubt(
            DOUBT_STRUCTURE,
            format!("{want} statements to change, {hits} found"),
        ));
    }
    w.flush().map_err(failed)?;
    drop(put);
    Ok(bytes)
}

/// True when `out` already exists and is the same file as `src` (same canonical path; on
/// Windows also case-insensitively, which covers short 8.3 names the canonical form expands;
/// same volume and file index, which covers hard links), or when that cannot be told.
pub fn is_same_file(src: &Path, out: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::canonicalize(src), std::fs::canonicalize(out)) else {
        return false;
    };
    if a == b {
        return true;
    }
    #[cfg(windows)]
    if a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase() {
        return true;
    }
    // a hard link is another path to the same bytes: writing it would overwrite the source.
    // Both files exist here, so an identity that cannot be read is treated as the same file.
    match (file_identity(&a), file_identity(&b)) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// (volume, file index) of an existing file; equal values mean the same file on disk.
#[cfg(windows)]
fn file_identity(p: &Path) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        attrs: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(h: *mut std::ffi::c_void, info: *mut Info) -> i32;
    }
    let f = std::fs::File::open(p).ok()?;
    let mut info = Info::default();
    // SAFETY: the handle is open for the call and `info` matches BY_HANDLE_FILE_INFORMATION.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle().cast(), &mut info) };
    (ok != 0).then(|| {
        (
            u64::from(info.volume),
            (u64::from(info.index_high) << 32) | u64::from(info.index_low),
        )
    })
}

#[cfg(unix)]
fn file_identity(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    Some((m.dev(), m.ino()))
}

#[cfg(not(any(windows, unix)))]
fn file_identity(_: &Path) -> Option<(u64, u64)> {
    None
}

/// Writes the patched copy of `src` to `out` (the caller syncs, checks and places it). An
/// output that is the source is refused before anything is read or written.
pub fn patch_file(
    model: &Model,
    source: &str,
    src: &Path,
    out: &Path,
    ctl: Option<&Arc<Progress>>,
) -> Result<PatchOutput, PatchError> {
    if is_same_file(src, out) {
        return Err(failed("the output file is the source file; choose another name"));
    }
    check_source(model, source, src, ctl)?;
    check_shared(model, source)?;
    let mut work = gather(model, source)?;
    if work.is_empty() {
        let bytes = std::fs::copy(src, out).map_err(failed)?;
        return Ok(PatchOutput {
            stats: ExportStats {
                bytes,
                ..Default::default()
            },
            no_changes: true,
            ..Default::default()
        });
    }
    let ix = Index::scan(src, ctl)?;
    if ix.schema.is_none() {
        return Err(doubt(DOUBT_SCHEMA, format!("schema {}", ix.schema_name)));
    }
    if !ix.ended || ix.dup_ids > 0 || ix.max_id >= i32::MAX as u32 {
        return Err(doubt(
            DOUBT_STRUCTURE,
            "broken DATA section, repeated ids or ids near the limit",
        ));
    }
    let mut o = Out::new(&ix);

    let mut seeds = vec![];
    for g in &work.deleted {
        // gone already: our own earlier write over the source removed it
        let Some(id) = ix.rooted.get(g).copied() else {
            continue;
        };
        if ix.type_name(id).is_some_and(|t| SPATIAL.contains(&t)) {
            return Err(doubt(DOUBT_EDIT, format!("spatial element {g} deleted")));
        }
        seeds.push(id);
    }
    let removed_products;
    {
        let guids = ix.guid_by_id();
        let mut live = |id: u32| match guids.get(&id) {
            Some(g) => alive(model, source, g),
            None => Ok(false),
        };
        let c = refs::cut(&ix, &seeds, &mut live)?;
        if !c.removed.is_empty() {
            for (id, p) in refs::probe(src, &c.removed, ctl)? {
                if let Some(r) = p
                    .referrers
                    .iter()
                    .find(|r| !c.removed.contains(*r) && !c.rewrites.contains_key(*r))
                {
                    return Err(doubt(
                        DOUBT_STRUCTURE,
                        format!("#{r} still needs deleted #{id}"),
                    ));
                }
            }
        }
        for (id, args) in &c.rewrites {
            if let Some(t) = ix.type_name(*id) {
                let t = t.to_string();
                o.swap(*id, &t, args);
            }
        }
        removed_products = c
            .removed
            .iter()
            .filter(|i| ix.type_name(**i).is_some_and(|t| !t.starts_with("IFCREL")))
            .count() as u64;
        o.remove = c.removed;
    }

    let u = if work.geom.is_empty()
        && work.added.is_empty()
        && work.props.is_empty()
        && work.relink.is_empty()
    {
        None
    } else {
        Some(units_of(src, ctl)?)
    };
    if let Some(u) = &u {
        if !work.geom.is_empty() || !work.added.is_empty() || !work.relink.is_empty() {
            graft::run(model, &ix, src, out, u, &mut work, &mut o, ctl)?;
        }
    }
    if !work.types.is_empty() {
        types::apply(&ix, src, &work.types, &mut o, ctl)?;
    }
    let owners = quantity_owners(&ix, &o);
    let dropped = props::drop_quantities(&ix, src, &owners, &mut o, ctl)?;
    if dropped > 0 {
        o.warnings.push(format!(
            "{dropped} quantity sets of changed elements removed: they no longer held"
        ));
    }
    if let Some(u) = &u {
        if !work.props.is_empty() {
            props::apply(model, source, &ix, src, u, &work.props, &mut o, ctl)?;
        }
    }

    let mut created = false;
    let bytes = match copy(src, out, &o, ctl, &mut created) {
        Ok(b) => b,
        Err(e) => {
            // a cancel or a write error must not leave half a file under the output name;
            // a file this call never opened for writing is not touched
            if created {
                let _ = std::fs::remove_file(out);
            }
            return Err(e);
        }
    };

    let mut guids: HashSet<String> = ix
        .rooted
        .iter()
        .filter(|(_, id)| !o.remove.contains(*id))
        .map(|(g, _)| g.clone())
        .collect();
    guids.extend(o.new_guids.iter().cloned());
    let mut touched = o.touched.clone();
    touched.sort();
    touched.dedup();
    let elements = (ix.products + o.added).saturating_sub(removed_products + o.gone);
    let stats = ExportStats {
        bytes,
        written: o.by_kind.clone(),
        elements,
        ids_kept: o.edited,
        ids_new: o.added,
        warnings: o.warnings.clone(),
        element_guids: touched,
        all_guids: guids.iter().cloned().collect(),
        not_written: Vec::<NotExported>::new(),
        ..Default::default()
    };
    Ok(PatchOutput {
        stats,
        will_not: o.will_not.clone(),
        unwritten: o.unwritten.clone(),
        edited: o.edited,
        added: o.added,
        removed: o.remove.len() as u64,
        linked: o.linked.clone(),
        pending_links: o.pending_links.clone(),
        retried: o.retried.clone(),
        no_changes: false,
        expect: Some(Expect {
            schema_name: ix.schema_name.clone(),
            guids,
            dangling: ix.dangling,
            dup_guids: ix.dup_guids,
        }),
    })
}

/// Reads a finished patch back: same schema, unique ids, no new dangling reference, and exactly
/// the rooted entities expected.
pub fn check_patched(out: &Path, e: &Expect, ctl: Option<&Arc<Progress>>) -> Result<(), String> {
    let ix = Index::scan(out, ctl).map_err(|e| e.to_string())?;
    if ix.schema_name != e.schema_name {
        return Err(format!(
            "schema {} instead of {}",
            ix.schema_name, e.schema_name
        ));
    }
    if !ix.ended || ix.dup_ids > 0 {
        return Err("broken DATA section or repeated ids".into());
    }
    if ix.dangling > e.dangling {
        return Err(format!(
            "{} references to nothing, the source had {}",
            ix.dangling, e.dangling
        ));
    }
    if ix.dup_guids > e.dup_guids {
        return Err("repeated GlobalId".into());
    }
    if let Some(g) = e.guids.iter().find(|g| !ix.rooted.contains_key(*g)) {
        return Err(format!("{g} is missing from the file"));
    }
    if let Some(g) = ix.rooted.keys().find(|g| !e.guids.contains(*g)) {
        return Err(format!("{g} should not be in the file"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n#2=IFCUNITASSIGNMENT((#1));\nENDSEC;\nEND-ISO-10303-21;\n";

    fn paths(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("ddc-sv1-copy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("a.ifc");
        std::fs::write(&src, FILE).unwrap();
        (src, d.join("out.ifc"))
    }

    #[test]
    fn an_unwritten_link_of_a_new_element_stays_pending_and_is_listed() {
        let (src, _) = paths("link");
        let ix = Index::scan(&src, None).unwrap();
        let mut o = Out::new(&ix);
        o.deny_link("wall", 7, "g1", "IFCRELASSOCIATESMATERIAL", "IFCFOO");
        assert!(o
            .unwritten
            .contains(&("wall".to_string(), 7, "~link".to_string())));
        // the element itself is not held back: it is recorded as written, so no second append
        assert!(!o
            .unwritten
            .contains(&("wall".to_string(), 7, String::new())));
        assert_eq!(o.will_not, [("wall".to_string(), REASON_LINK.to_string())]);
        assert_eq!(o.warnings.len(), 1);
    }

    #[test]
    fn a_replacement_the_copy_never_met_is_a_doubt() {
        let (src, out) = paths("miss");
        let ix = Index::scan(&src, None).unwrap();
        let mut o = Out::new(&ix);
        o.replace.insert(99, b"#99=IFCSIUNIT()".to_vec());
        let e = copy(&src, &out, &o, None, &mut false).unwrap_err();
        assert!(matches!(
            e,
            PatchError::Doubt {
                key: DOUBT_STRUCTURE,
                ..
            }
        ));
    }

    #[test]
    fn only_placement_columns_of_a_listed_kind_are_placement_only() {
        assert!(placement_only("comp", &["x", "y"]));
        assert!(placement_only("comp", &["rotation", "z", "level"]));
        for f in ["mirror", "ctype", "host_wall", "host_slab", "vals", "name"] {
            assert!(!placement_only("comp", &["x", f]), "{f}");
        }
        assert!(!placement_only("comp", &[]));
        for k in ["stair", "railing", "mep_run", "wall"] {
            assert!(!placement_only(k, &["x"]), "{k}");
        }
    }

    #[test]
    fn a_removal_the_copy_never_met_is_a_doubt() {
        let (src, out) = paths("gone");
        let ix = Index::scan(&src, None).unwrap();
        let mut o = Out::new(&ix);
        o.remove.insert(99);
        assert!(copy(&src, &out, &o, None, &mut false).is_err());
    }

    #[test]
    fn edits_that_all_land_pass_even_when_one_id_is_named_twice() {
        let (src, out) = paths("hit");
        let ix = Index::scan(&src, None).unwrap();
        let mut o = Out::new(&ix);
        o.replace.insert(2, b"#2=IFCUNITASSIGNMENT(())".to_vec());
        o.replace.insert(1, b"#1=IFCSIUNIT()".to_vec());
        o.remove.insert(1);
        copy(&src, &out, &o, None, &mut false).unwrap();
        let t = std::fs::read_to_string(&out).unwrap();
        assert!(t.contains("#2=IFCUNITASSIGNMENT(());") && !t.contains("#1="));
    }
}
