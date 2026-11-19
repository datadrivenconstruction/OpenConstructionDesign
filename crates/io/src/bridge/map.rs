//! Frames → native model rows inside the caller's batch transaction (one undo).
//! Stable guids go to `ext_ref`, namespaced by source (bridge name + input file identity): a guid
//! is unique per (source, guid); the same guid from another file is another element. A guid seen before is compared and updated in place when its
//! data changed (undoable in the same operation); guids absent from the source are reported,
//! never deleted. Re-import is a 3-way merge against the last import's base (`merge`): local
//! edits are kept, non-conflicting source changes applied, conflicts listed. Arc walls become
//! native arc walls (bulge); grids, columns, beams and MEP runs become native rows too.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_model::Snapshots;
use rusqlite::types::Value as Sql;

use ddc_bridge_proto::{
    CurtainWall, Curve, Element, Frame, Member, MemberKind, MepRun, MullionSection, OpeningKind,
    PanelKind, Progress, Top, Type, Value, P2,
};
use rusqlite::{params, Connection, OptionalExtension};

use super::BridgeError;

mod merge;

/// What one import did. `skipped` maps a neutral category to a count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub bridge: String,
    pub bridge_version: String,
    pub levels: usize,
    pub materials: usize,
    pub wall_types: usize,
    pub slab_types: usize,
    pub family_types: usize,
    pub walls: usize,
    pub arc_walls: usize,
    /// Native wall rows created.
    pub wall_segments: usize,
    pub doors: usize,
    pub windows: usize,
    pub voids: usize,
    pub slabs: usize,
    /// Slabs with a sloped top.
    pub sloped_slabs: usize,
    pub curtain_walls: usize,
    /// Panels of created curtain walls, doors and windows among them.
    pub curtain_panels: usize,
    pub curtain_doors: usize,
    pub curtain_windows: usize,
    pub rooms: usize,
    pub grids: usize,
    pub columns: usize,
    pub beams: usize,
    pub mep_runs: usize,
    pub ceilings: usize,
    /// Bodies stored as native extrusions.
    pub solids: usize,
    pub meshes: usize,
    pub properties: usize,
    /// Elements whose guid was imported before (re-import): kept, not duplicated.
    pub unchanged: usize,
    /// Elements imported before whose data changed: updated in place.
    pub updated: usize,
    /// Elements imported before as another body kind (mesh ↔ native): replaced, properties moved.
    pub migrated: usize,
    /// Guids imported before but absent from this source: kept in the model, only reported.
    pub missing: Vec<String>,
    /// 3-way merge with the last import: taken, kept, conflicts, deleted upstream, …
    pub merge: ddc_model::source::MergeStats,
    /// Guids removed from the source whose unedited elements this import deleted.
    pub deleted: Vec<String>,
    pub skipped: BTreeMap<String, usize>,
    pub warnings: Vec<String>,
    pub warning_count: usize,
}

const MAX_WARNINGS: usize = 1000;
/// Largest accepted `Units.length_to_m` (1 source unit = 1 km).
pub const MAX_LENGTH_TO_M: f64 = 1000.0;

/// (category, name, parameters).
type FamilyType = (String, String, Vec<(String, Value)>);

/// Result of matching one element by guid.
#[derive(Clone, Copy, PartialEq)]
enum Up {
    New(i64),
    Same(i64),
    Updated(i64),
    /// Deleted locally and kept deleted (merge): nothing to attach.
    Gone,
}

impl Up {
    fn id(self) -> i64 {
        match self {
            Up::New(i) | Up::Same(i) | Up::Updated(i) => i,
            Up::Gone => 0,
        }
    }
}

const ELEMENT_KINDS: [&str; 12] = [
    "solid",
    "ceiling",
    "wall",
    "opening",
    "slab",
    "room",
    "mesh",
    "curtain_wall",
    "grid",
    "col",
    "beam",
    "mep_run",
];
/// Body kinds one source element may move between across bridge versions (mesh ↔ native).
const MIGRATES: [&str; 6] = ["mesh", "col", "beam", "mep_run", "ceiling", "solid"];
/// Every table an `ext_ref.kind` may name (nothing else is interpolated into SQL).
const ALL_KINDS: [&str; 15] = [
    "solid",
    "ceiling",
    "grid",
    "col",
    "beam",
    "mep_run",
    "curtain_wall",
    "wall",
    "opening",
    "slab",
    "room",
    "mesh",
    "level",
    "wall_type",
    "slab_type",
];
const WALL_COLS: [&str; 12] = [
    "level",
    "wtype",
    "ax",
    "ay",
    "bx",
    "by",
    "base_offset",
    "height",
    "offset0",
    "axis_kind",
    "bulge",
    "top_profile",
];
const CURTAIN_COLS: [&str; 15] = [
    "level",
    "name",
    "ax",
    "ay",
    "bx",
    "by",
    "bulge",
    "base_offset",
    "height",
    "ugrid",
    "vgrid",
    "mullion_in",
    "mullion_border",
    "mullion_lines",
    "top_profile",
];
const OPENING_COLS: [&str; 8] = [
    "wall",
    "along",
    "width",
    "sill",
    "height",
    "kind",
    "swing_side",
    "swing_hinge",
];

fn same(a: &Sql, b: &Sql) -> bool {
    match (a, b) {
        (Sql::Real(x), Sql::Real(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
        (Sql::Integer(x), Sql::Real(y)) | (Sql::Real(y), Sql::Integer(x)) => {
            (*x as f64 - y).abs() <= 1e-9
        }
        _ => a == b,
    }
}

pub(crate) struct Mapper<'a> {
    db: &'a Connection,
    snaps: &'a mut Snapshots,
    /// Input file identity; the guid namespace is chosen at Hello (see `namespace`), never
    /// from the reader's name, so a reader update merges into the same elements.
    file: String,
    /// Namespace named by the caller (re-import of a known source).
    ns: Option<String>,
    /// Parameter key → source data type from ParamDefs (unitless lengths are converted).
    specs: HashMap<String, String>,
    source: Option<String>,
    seen: HashSet<String>,
    scale: f64,
    /// A scaled length or point left the finite range; checked after every frame.
    overflow: Cell<bool>,
    units_seen: bool,
    levels: HashMap<String, (i64, f64)>,
    materials: HashMap<String, String>,
    wall_types: HashMap<String, i64>,
    slab_types: HashMap<String, i64>,
    family: HashMap<String, FamilyType>,
    received: u64,
    ended: bool,
    /// Wall guid → top edge (metres), from WallTops frames ahead of the wall.
    tops: HashMap<String, Vec<P2>>,
    /// Slab guid → (point, gradient) of its top plane (metres), from SlabSlopes frames.
    slopes: HashMap<String, (P2, P2)>,
    /// Curtain wall guid → grid lines with their own mullion, from CurtainLines frames.
    lines: HashMap<String, (ddc_model::LineSections, Vec<P2>)>,
    /// Member types by section text: (id, name), filled on first use (cleared on item rollback).
    mtypes: HashMap<String, Vec<(i64, String)>>,
    /// 3-way merge with the last import (base), set up at Hello.
    merge: ddc_model::source::Merge,
    policy: ddc_model::source::Policy,
    /// Input path as the user named it; document facts from DocInfo.
    path: String,
    /// Hash and stat of the input: identity by content, recorded in `src_file` at the end.
    stamp: Option<ddc_model::source::file::FileStamp>,
    info: BTreeMap<String, String>,
    /// (table, row) → source guid, for property merge.
    guids: HashMap<(String, i64), String>,
    pub report: Report,
}

fn data<T>(m: impl Into<String>) -> Result<T, BridgeError> {
    Err(BridgeError::Data(m.into()))
}

fn db_err(e: rusqlite::Error) -> BridgeError {
    BridgeError::Model(e.to_string())
}

fn model_err(e: ddc_model::Error) -> BridgeError {
    BridgeError::Model(e.to_string())
}

impl<'a> Mapper<'a> {
    pub(crate) fn new(db: &'a Connection, snaps: &'a mut Snapshots, file: &str) -> Self {
        Self {
            db,
            snaps,
            file: file.to_string(),
            source: None,
            seen: HashSet::new(),
            scale: 1.0,
            overflow: Cell::new(false),
            units_seen: false,
            levels: HashMap::new(),
            materials: HashMap::new(),
            wall_types: HashMap::new(),
            slab_types: HashMap::new(),
            family: HashMap::new(),
            received: 0,
            ended: false,
            tops: HashMap::new(),
            slopes: HashMap::new(),
            lines: HashMap::new(),
            mtypes: HashMap::new(),
            ns: None,
            specs: HashMap::new(),
            merge: ddc_model::source::Merge::new("", Default::default()),
            policy: Default::default(),
            path: String::new(),
            stamp: None,
            info: BTreeMap::new(),
            guids: HashMap::new(),
            report: Report::default(),
        }
    }

    /// Displayed input path and merge policy of this import.
    pub(crate) fn with(mut self, path: &str, policy: ddc_model::source::Policy) -> Self {
        self.path = path.to_string();
        self.policy = policy;
        self
    }

    /// Content hash and stat of the input (identity survives a rename or move).
    pub(crate) fn with_stamp(mut self, s: Option<ddc_model::source::file::FileStamp>) -> Self {
        self.stamp = s;
        self
    }

    /// Merge into this existing source namespace (`SourceInfo::source`).
    pub(crate) fn into_source(mut self, source: Option<&str>) -> Self {
        self.ns = source.filter(|s| !s.is_empty()).map(str::to_string);
        self
    }

    /// Guid namespace: the caller's, else the one an earlier import of this file used (any
    /// reader; links from before provenance are `<reader>|<file>`), else `src|<file>`.
    fn namespace(&self) -> Result<String, BridgeError> {
        if let Some(s) = &self.ns {
            return Ok(s.clone());
        }
        let known: Option<String> = self
            .db
            .query_row(
                "SELECT source FROM src_file WHERE file_key=?1 ORDER BY imported_at DESC, id DESC LIMIT 1",
                [&self.file],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if let Some(s) = known {
            return Ok(s);
        }
        if let Some(st) = &self.stamp {
            let moved = ddc_model::source::file::find_source(self.db, &self.file, &st.hash)
                .map_err(model_err)?;
            if let Some(s) = moved {
                return Ok(s);
            }
        }
        let legacy: Option<String> = self
            .db
            .query_row(
                "SELECT source FROM ext_ref WHERE length(source) > length(?1) + 1 \
                 AND substr(source, length(source) - length(?1)) = '|' || ?1 LIMIT 1",
                [&self.file],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(legacy.unwrap_or_else(|| format!("src|{}", self.file)))
    }

    /// DocInfo ahead of any element: a first import of a project that was imported before from
    /// another path (same `project_guid`) merges into that source.
    fn adopt_project(&mut self) -> Result<(), BridgeError> {
        let Some(pg) = self
            .info
            .get("project_guid")
            .filter(|g| !g.is_empty())
            .cloned()
        else {
            return Ok(());
        };
        if self.ns.is_some() || !self.seen.is_empty() || !self.levels.is_empty() {
            return Ok(());
        }
        let here: i64 = self
            .db
            .query_row(
                "SELECT count(*) FROM src_file WHERE source=?1",
                [self.src()],
                |r| r.get(0),
            )
            .map_err(db_err)?;
        if here > 0 {
            return Ok(());
        }
        let rows: Vec<(String, String)> = self
            .db
            .prepare("SELECT source, info FROM src_file ORDER BY imported_at DESC, id DESC")
            .map_err(db_err)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        let other = rows.into_iter().find(|(_, info)| {
            serde_json::from_str::<serde_json::Value>(info)
                .ok()
                .and_then(|j| {
                    j.get("project_guid")
                        .and_then(|g| g.as_str())
                        .map(|g| g == pg)
                })
                .unwrap_or(false)
        });
        if let Some((s, _)) = other {
            self.source = Some(s);
            self.merge = ddc_model::source::Merge::new(self.src(), self.policy);
            self.merge
                .begin(self.db, &mut *self.snaps)
                .map_err(model_err)?;
        }
        Ok(())
    }

    /// Value and unit of a parameter: a unitless number whose source data type is a length,
    /// area or volume is in source units and is converted to SI.
    fn measure(&self, key: &str, v: f64, unit: &str) -> (f64, String) {
        if !unit.is_empty() {
            return (v, unit.to_string());
        }
        let bare = key.strip_prefix("type.").unwrap_or(key);
        let k = self.scale;
        match self.specs.get(bare).map(|s| spec_dim(s)) {
            Some(1) => (self.len(v), "m".into()),
            Some(2) => (self.len(v * k), "m²".into()),
            Some(3) => (self.len(v * k * k), "m³".into()),
            _ => (v, String::new()),
        }
    }

    fn warn(&mut self, m: String) {
        self.report.warning_count += 1;
        if self.report.warnings.len() < MAX_WARNINGS {
            self.report.warnings.push(m);
        }
    }

    /// One item of a frame: its bad content or a model rejection rolls back that item only and
    /// becomes a warning; caps, protocol and I/O errors still stop the import.
    fn guarded(
        &mut self,
        what: &str,
        f: impl FnOnce(&mut Self) -> Result<(), BridgeError>,
    ) -> Result<(), BridgeError> {
        let run = |db: &Connection, sql: &str| -> Result<(), BridgeError> {
            db.prepare_cached(sql)
                .and_then(|mut st| st.execute([]))
                .map(|_| ())
                .map_err(db_err)
        };
        run(self.db, "SAVEPOINT item")?;
        match f(self) {
            Ok(()) => run(self.db, "RELEASE item"),
            Err(BridgeError::Data(m) | BridgeError::Model(m)) if !self.overflow.get() => {
                run(self.db, "ROLLBACK TO item")?;
                run(self.db, "RELEASE item")?;
                // rows created inside the item are gone: cached ids may point at them
                self.mtypes.clear();
                self.skip(what);
                self.warn(format!("{what}: {m}, skipped"));
                Ok(())
            }
            Err(e) => {
                let _ = self.db.execute_batch("ROLLBACK TO item; RELEASE item");
                self.mtypes.clear();
                Err(e)
            }
        }
    }

    fn skip(&mut self, what: &str) {
        *self.report.skipped.entry(what.into()).or_default() += 1;
    }

    fn len(&self, v: f64) -> f64 {
        let x = v * self.scale;
        if !x.is_finite() {
            self.overflow.set(true);
        }
        x
    }

    fn pt(&self, p: P2) -> P2 {
        (self.len(p.0), self.len(p.1))
    }

    fn src(&self) -> &str {
        self.source.as_deref().unwrap_or_default()
    }

    /// Native row for `guid` if it was imported before and still exists in `table`.
    fn existing(&self, guid: &str, table: &str) -> Result<Option<i64>, BridgeError> {
        let r: Option<i64> = self
            .db
            .prepare_cached("SELECT ref FROM ext_ref WHERE source=?1 AND guid=?2 AND kind=?3")
            .map_err(db_err)?
            .query_row(params![self.src(), guid, table], |r| r.get(0))
            .optional()
            .map_err(db_err)?;
        let Some(id) = r else { return Ok(None) };
        let n: i64 = self
            .db
            .prepare_cached(&format!("SELECT count(*) FROM {table} WHERE id=?1"))
            .map_err(db_err)?
            .query_row([id], |r| r.get(0))
            .map_err(db_err)?;
        Ok((n > 0).then_some(id))
    }

    fn alive(&self, table: &str, id: i64) -> Result<bool, BridgeError> {
        if !ALL_KINDS.contains(&table) {
            return Ok(false);
        }
        let n: i64 = self
            .db
            .prepare_cached(&format!("SELECT count(*) FROM {table} WHERE id=?1"))
            .map_err(db_err)?
            .query_row([id], |r| r.get(0))
            .map_err(db_err)?;
        Ok(n > 0)
    }

    fn link(&mut self, guid: &str, table: &str, id: i64) -> Result<(), BridgeError> {
        if guid.is_empty() || guid.len() > 256 {
            return data(format!("guid of {} bytes", guid.len()));
        }
        let cur: Option<(String, i64)> = self
            .db
            .prepare_cached("SELECT kind, ref FROM ext_ref WHERE source=?1 AND guid=?2")
            .map_err(db_err)?
            .query_row(params![self.src(), guid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(db_err)?;
        match cur {
            // Unchanged links stay: a replaced row would get a new rowid and be dropped by undo.
            Some((k, r)) if k == table && r == id => Ok(()),
            // A live object of another kind owns this guid: report, never overwrite.
            Some((k, r)) if k != table && self.alive(&k, r)? => {
                self.warn(format!(
                    "guid '{guid}' is a {k} and a {table}: second link not stored"
                ));
                self.skip("guid collision");
                Ok(())
            }
            // A stale link (its row deleted) is replaced; undo then simply forgets it.
            _ => {
                self.db
                    .prepare_cached(
                        "INSERT OR REPLACE INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,?4)",
                    )
                    .map_err(db_err)?
                    .execute(params![self.src(), guid, table, id])
                    .map_err(db_err)?;
                Ok(())
            }
        }
    }

    /// Unique name in `table`: `name`, `name (2)`, …
    fn free_name(&self, table: &str, name: &str) -> Result<String, BridgeError> {
        let base = if name.trim().is_empty() {
            "imported"
        } else {
            name
        };
        let mut cand = base.to_string();
        for k in 2.. {
            let n: i64 = self
                .db
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE name=?1"),
                    [&cand],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            if n == 0 {
                break;
            }
            cand = format!("{base} ({k})");
        }
        Ok(cand)
    }

    fn level(&self, guid: &str) -> Result<(i64, f64), BridgeError> {
        match self.levels.get(guid) {
            Some(l) => Ok(*l),
            None => data(format!("unknown level '{guid}'")),
        }
    }

    /// Guid → existing row: merged with the base (non-conflicting source changes applied in
    /// place, snapshot first), kept deleted, or inserted and linked.
    fn upsert(
        &mut self,
        guid: &str,
        table: &str,
        cols: &[&str],
        vals: &[Sql],
    ) -> Result<Up, BridgeError> {
        if !self.seen.insert(guid.to_string()) {
            return data(format!("duplicate guid '{guid}'"));
        }
        // An element that changed between body and native row (a file imported as a mesh before
        // native members existed): the old row is replaced, its properties move to the new one.
        if self.retire_blocked(guid, table)? {
            return Ok(Up::Gone);
        }
        let retired = self.retire(guid, table)?;
        // A guid already owned by a live row of another kind links under `guid#table`, so the
        // second row is found again on re-import instead of being inserted twice.
        let keyed;
        let guid = match self.owner(guid)? {
            // a deletable owner is an element that changed kind: `replace_kind` retires it below
            Some((k, r))
                if k != table
                    && !ddc_model::source::DELETABLE.contains(&k.as_str())
                    && self.alive(&k, r)? =>
            {
                keyed = format!("{guid}#{table}");
                keyed.as_str()
            }
            _ => guid,
        };
        if let Some(id) = self.existing(guid, table)? {
            self.guids.insert((table.to_string(), id), guid.to_string());
            return self.merge_existing(guid, table, id, cols, vals);
        }
        if self.replace_kind(guid, table)? {
            return Ok(Up::Gone);
        }
        if self.merge_dead(guid, table, cols, vals)? {
            return Ok(Up::Gone);
        }
        let ph: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
        self.db
            .prepare_cached(&format!(
                "INSERT INTO {table}({}) VALUES ({})",
                cols.join(","),
                ph.join(",")
            ))
            .map_err(db_err)?
            .execute(rusqlite::params_from_iter(vals))
            .map_err(db_err)?;
        let id = self.db.last_insert_rowid();
        self.link(guid, table, id)?;
        if let Some((k, r)) = retired {
            self.db
                .prepare_cached("UPDATE prop SET kind=?3, ref=?4 WHERE kind=?1 AND ref=?2")
                .map_err(db_err)?
                .execute(params![k, r, table, id])
                .map_err(db_err)?;
            self.report.migrated += 1;
        }
        self.merge_added(guid, table, id, cols, vals);
        Ok(Up::New(id))
    }

    /// A pinned row of another body kind stays: the whole import does not fail on it.
    fn retire_blocked(&mut self, guid: &str, table: &str) -> Result<bool, BridgeError> {
        if !MIGRATES.contains(&table) {
            return Ok(false);
        }
        let Some((k, r)) = self.owner(guid)? else {
            return Ok(false);
        };
        if k == table || !MIGRATES.contains(&k.as_str()) || !self.alive(&k, r)? {
            return Ok(false);
        }
        if !self
            .merge
            .blocked_by_pin(self.db, &k, r)
            .map_err(model_err)?
        {
            return Ok(false);
        }
        self.warn(format!("'{guid}' stays as it is: the element is pinned"));
        Ok(true)
    }

    /// When `guid` is owned by a live row of another body kind (`MIGRATES`) from an earlier
    /// import, deletes that row (snapshots first, so undo restores it with its link and
    /// properties) and returns it; the caller inserts the new row and moves the properties.
    fn retire(&mut self, guid: &str, table: &str) -> Result<Option<(String, i64)>, BridgeError> {
        if !MIGRATES.contains(&table) {
            return Ok(None);
        }
        let Some((k, r)) = self.owner(guid)? else {
            return Ok(None);
        };
        if k == table || !MIGRATES.contains(&k.as_str()) || !self.alive(&k, r)? {
            return Ok(None);
        }
        let snap = |s: &mut Self, t: &str, c: &str| s.snaps.save(s.db, t, c, r).map_err(model_err);
        snap(self, &k, "id")?;
        snap(self, "prop", "ref")?;
        snap(self, "ext_ref", "ref")?;
        if k == "ceiling" {
            snap(self, "ceiling_vertex", "ceiling")?;
            self.db
                .execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [r])
                .map_err(db_err)?;
        }
        if k == "solid" {
            snap(self, "solid_host", "solid")?;
            self.db
                .execute("DELETE FROM solid_host WHERE solid=?1", [r])
                .map_err(db_err)?;
        }
        // `k` is one of MIGRATES: never user text in the SQL
        self.db
            .execute(&format!("DELETE FROM {k} WHERE id=?1"), [r])
            .map_err(db_err)?;
        Ok(Some((k, r)))
    }

    /// Counts a matched element; true when it was created.
    fn tally(&mut self, u: Up) -> bool {
        match u {
            Up::New(_) => true,
            Up::Same(_) => {
                self.report.unchanged += 1;
                false
            }
            Up::Updated(_) => {
                self.report.updated += 1;
                false
            }
            Up::Gone => false,
        }
    }

    /// Elements imported before that this source no longer has (still present in the model).
    fn missing(&self) -> Result<Vec<String>, BridgeError> {
        let mut out = vec![];
        let mut st = self
            .db
            .prepare("SELECT guid, kind, ref FROM ext_ref WHERE source=?1 ORDER BY guid")
            .map_err(db_err)?;
        let rows = st
            .query_map([self.src()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(db_err)?;
        for r in rows {
            let (g, kind, id) = r.map_err(db_err)?;
            // a `guid#table` link stands for the source guid
            let g = match g
                .strip_suffix(kind.as_str())
                .and_then(|b| b.strip_suffix('#'))
            {
                Some(base) => base.to_string(),
                None => g,
            };
            if !ELEMENT_KINDS.contains(&kind.as_str()) || self.seen.contains(&g) {
                continue;
            }
            if self.alive(&kind, id)? {
                out.push(g);
            }
        }
        out.dedup();
        Ok(out)
    }

    pub(crate) fn frame(
        &mut self,
        f: Frame,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<(), BridgeError> {
        if self.ended {
            return data("frame after End");
        }
        let hello = matches!(f, Frame::Hello(_));
        if hello == self.source.is_some() {
            return Err(BridgeError::Protocol(if hello {
                "second Hello".into()
            } else {
                "first frame is not Hello".into()
            }));
        }
        self.frame_inner(f, progress)?;
        if self.overflow.get() {
            return data("length out of range after unit scaling");
        }
        Ok(())
    }

    fn frame_inner(
        &mut self,
        f: Frame,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<(), BridgeError> {
        match f {
            Frame::Hello(h) => {
                if h.protocol != ddc_bridge_proto::PROTOCOL {
                    return Err(BridgeError::Protocol(format!("protocol {}", h.protocol)));
                }
                self.source = Some(self.namespace()?);
                self.merge = ddc_model::source::Merge::new(self.src(), self.policy);
                self.merge
                    .begin(self.db, &mut *self.snaps)
                    .map_err(model_err)?;
                self.report.bridge = h.bridge;
                self.report.bridge_version = h.version;
            }
            Frame::Progress(p) => progress(&p),
            Frame::Units(u) => {
                let k = u.length_to_m;
                if !(k.is_finite() && k > 0.0 && k <= MAX_LENGTH_TO_M) {
                    return data(format!("length unit {k} m outside (0, {MAX_LENGTH_TO_M}]"));
                }
                if self.report.levels > 0 || self.received > 0 {
                    return data("Units after geometry");
                }
                self.scale = u.length_to_m;
                self.units_seen = true;
            }
            Frame::Levels(v) => {
                for l in v {
                    self.add_level(&l.guid, &l.name, self.len(l.elevation))?;
                }
            }
            Frame::Grids(v) => {
                for g in v {
                    self.guarded("grid", |s| s.add_grid(g))?;
                }
            }
            Frame::Members(v) => {
                for m in v {
                    self.guarded("member", |s| s.add_member(m))?;
                }
            }
            Frame::MepRuns(v) => {
                for r in v {
                    self.guarded("mep_run", |s| s.add_mep_run(r))?;
                }
            }
            Frame::Ceilings(v) => {
                for c in v {
                    self.guarded("ceiling", |s| s.add_ceiling(c))?;
                }
            }
            Frame::Prisms(v) => {
                for p in v {
                    self.guarded("solid", |s| s.add_prism(p))?;
                }
            }
            Frame::Materials(v) => {
                for m in v {
                    self.materials.insert(m.guid, m.name);
                    self.report.materials += 1;
                }
            }
            Frame::Types(v) => {
                for t in v {
                    self.guarded("type", |s| s.add_type(t))?;
                }
            }
            Frame::Elements(v) => {
                for e in v {
                    self.received += 1;
                    self.guarded("element", |s| s.add_element(e))?;
                }
            }
            Frame::Properties(v) => {
                for p in v {
                    self.guarded("property", |s| {
                        s.add_prop(&p.owner, &p.key, &p.value, &p.unit)
                    })?;
                }
            }
            Frame::Warnings(v) => {
                for w in v {
                    let at = w.element.map(|g| format!(" [{g}]")).unwrap_or_default();
                    self.warn(format!("{}: {}{at}", w.code, w.message));
                }
            }
            Frame::End(s) => {
                if s.elements != self.received {
                    return Err(BridgeError::Protocol(format!(
                        "End reports {} elements, received {}",
                        s.elements, self.received
                    )));
                }
                if !self.units_seen {
                    self.warn("no Units frame: lengths taken as metres".into());
                }
                self.merge_finish()?;
                self.report.missing = self.missing()?;
                self.ended = true;
            }
            Frame::WallTops(v) => {
                for t in v {
                    let pts: Vec<P2> = t.points.iter().map(|&p| self.pt(p)).collect();
                    let ordered = pts.windows(2).all(|w| w[1].0 >= w[0].0);
                    if pts.len() < 2 || !ordered || pts.iter().any(|p| p.1 <= 0.0) {
                        self.warn(format!("wall '{}': bad top profile, flat top kept", t.wall));
                        continue;
                    }
                    self.tops.insert(t.wall, pts);
                }
            }
            Frame::SlabSlopes(v) => {
                for t in v {
                    let g = t.gradient;
                    if !(g.0.hypot(g.1) <= 10.0) {
                        self.warn(format!("slab '{}': slope too steep, flat top kept", t.slab));
                        continue;
                    }
                    self.slopes.insert(t.slab, (self.pt(t.point), g));
                }
            }
            Frame::CurtainLines(v) => {
                for c in v {
                    type Line = (usize, Option<usize>, ddc_model::MullionSection);
                    let conv = |l: &[(u32, i32, MullionSection)]| -> Vec<Line> {
                        l.iter()
                            .map(|&(k, g, s)| {
                                (k as usize, usize::try_from(g).ok(), self.section(s))
                            })
                            .collect()
                    };
                    let ls = ddc_model::LineSections {
                        u: conv(&c.u),
                        v: conv(&c.v),
                    };
                    let top: Vec<P2> = c.top.iter().map(|&p| self.pt(p)).collect();
                    self.lines.insert(c.wall, (ls, top));
                }
            }
            Frame::CurtainWalls(v) => {
                for c in v {
                    self.add_curtain(c)?;
                }
            }
            Frame::DocInfo(v) => {
                self.info.extend(v);
                self.adopt_project()?;
            }
            Frame::ParamDefs(v) => self.param_defs(v)?,
            Frame::SourceIds(v) => v
                .iter()
                .for_each(|i| self.merge.ifc_guid(&i.guid, &i.ifc_guid)),
            Frame::Views(v) => self.views(v)?,
            Frame::Unknown(t) => self.skip(&format!("frame {t}")),
        }
        Ok(())
    }

    fn add_grid(&mut self, g: ddc_bridge_proto::Grid) -> Result<(), BridgeError> {
        let (a, b) = (self.pt(g.a), self.pt(g.b));
        if dist(a, b) < 1e-6 {
            self.skip("grid: zero length");
            return Ok(());
        }
        // grid names are unique in the model: a re-import keeps the name it got first
        let name = match self.existing(&g.guid, "grid")? {
            Some(id) => self
                .db
                .query_row("SELECT name FROM grid WHERE id=?1", [id], |r| r.get(0))
                .map_err(db_err)?,
            None => self.free_name("grid", &g.name)?,
        };
        let vals = [
            Sql::Text(name),
            Sql::Real(a.0),
            Sql::Real(a.1),
            Sql::Real(b.0),
            Sql::Real(b.1),
            Sql::Real(0.0),
            Sql::Null,
            Sql::Null,
            Sql::Integer(3),
        ];
        let cols = [
            "name",
            "ax",
            "ay",
            "bx",
            "by",
            "bulge",
            "base_level",
            "top_level",
            "bubbles",
        ];
        let u = self.upsert(&g.guid, "grid", &cols, &vals)?;
        if self.tally(u) {
            self.report.grids += 1;
        }
        Ok(())
    }

    /// Section of an imported member or run; None for an unusable one.
    fn shape(&self, s: MullionSection) -> Option<ddc_model::MemberShape> {
        let ok = |v: f64| v.is_finite() && v > 1e-4;
        match s {
            MullionSection::Rect { width, depth } if ok(width) && ok(depth) => {
                Some(ddc_model::MemberShape::Rect {
                    b: self.len(width),
                    h: self.len(depth),
                })
            }
            MullionSection::Circle { diameter } if ok(diameter) => {
                Some(ddc_model::MemberShape::Circle {
                    d: self.len(diameter),
                })
            }
            _ => None,
        }
    }

    /// Member type with this name and section: found (also from an earlier import) or created.
    fn member_type(
        &mut self,
        name: &str,
        shape: &ddc_model::MemberShape,
    ) -> Result<i64, BridgeError> {
        let text = shape.text();
        let base = if name.trim().is_empty() {
            text.clone()
        } else {
            name.to_string()
        };
        if !self.mtypes.contains_key(&text) {
            let mut st = self
                .db
                .prepare_cached("SELECT id, name FROM member_type WHERE shape=?1 ORDER BY id")
                .map_err(db_err)?;
            let v: Vec<(i64, String)> = st
                .query_map([&text], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db_err)?
                .collect::<Result<_, _>>()
                .map_err(db_err)?;
            self.mtypes.insert(text.clone(), v);
        }
        let same_name = |n: &str| {
            n == base
                || n.strip_prefix(base.as_str())
                    .is_some_and(|r| r.starts_with(" (") && r.ends_with(')'))
        };
        if let Some((id, _)) = self.mtypes[&text].iter().find(|(_, n)| same_name(n)) {
            return Ok(*id);
        }
        let name = self.free_name("member_type", &base)?;
        self.db
            .prepare_cached("INSERT INTO member_type(name, shape, material) VALUES (?1,?2,?3)")
            .map_err(db_err)?
            .execute(params![name, text, "Imported"])
            .map_err(db_err)?;
        let id = self.db.last_insert_rowid();
        if let Some(v) = self.mtypes.get_mut(&text) {
            v.push((id, name));
        }
        Ok(id)
    }

    fn add_member(&mut self, m: Member) -> Result<(), BridgeError> {
        let (lv, elev) = self.level(&m.level)?;
        let Some(shape) = self.shape(m.section) else {
            self.skip("member: no section");
            return Ok(());
        };
        let p = |v: [f64; 3]| [self.len(v[0]), self.len(v[1]), self.len(v[2])];
        let (a, b) = (p(m.a), p(m.b));
        if (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt() < 1e-4 {
            self.skip("member: zero length");
            return Ok(());
        }
        // a measured outline replaces the box section; coordinates rounded to 0.1 mm so equal
        // bodies share one type
        let shape = if m.profile.len() >= 3 {
            let r = |v: f64| (self.len(v) * 1e4).round() / 1e4;
            let p =
                ddc_model::MemberShape::Poly(m.profile.iter().map(|q| (r(q.0), r(q.1))).collect());
            if p.check().is_ok() {
                p
            } else {
                shape
            }
        } else {
            shape
        };
        let mtype = self.member_type(&m.name, &shape)?;
        match m.kind {
            MemberKind::Column => {
                let slanted = dist((a[0], a[1]), (b[0], b[1])) > 1e-6;
                let vals = [
                    Sql::Integer(lv),
                    Sql::Null,
                    Sql::Integer(mtype),
                    Sql::Integer(m.structural as i64),
                    Sql::Real(a[0]),
                    Sql::Real(a[1]),
                    Sql::Real(a[2] - elev),
                    Sql::Real(b[2] - elev),
                    Sql::Real(m.rotation),
                    if slanted { Sql::Real(b[0]) } else { Sql::Null },
                    if slanted { Sql::Real(b[1]) } else { Sql::Null },
                ];
                let cols = [
                    "level",
                    "top_level",
                    "mtype",
                    "structural",
                    "x",
                    "y",
                    "base_offset",
                    "top_offset",
                    "rotation",
                    "tx",
                    "ty",
                ];
                let u = self.upsert(&m.guid, "col", &cols, &vals)?;
                if self.tally(u) {
                    self.report.columns += 1;
                }
            }
            MemberKind::Beam => {
                // the source body is already trimmed at its supports: joins would cut it again
                let none = ddc_model::BeamJoin::None.text();
                let c = ddc_model::Just::Center.to_i();
                let vals = [
                    Sql::Integer(lv),
                    Sql::Integer(mtype),
                    Sql::Real(a[0]),
                    Sql::Real(a[1]),
                    Sql::Real(a[2] - elev),
                    Sql::Real(b[0]),
                    Sql::Real(b[1]),
                    Sql::Real(b[2] - elev),
                    Sql::Real(0.0),
                    Sql::Integer(c),
                    Sql::Integer(c),
                    Sql::Real(m.rotation),
                    Sql::Real(0.0),
                    Sql::Real(0.0),
                    Sql::Text(none.into()),
                    Sql::Text(none.into()),
                ];
                let cols = [
                    "level",
                    "mtype",
                    "ax",
                    "ay",
                    "az",
                    "bx",
                    "by",
                    "bz",
                    "bulge",
                    "yjust",
                    "zjust",
                    "rotation",
                    "start_ext",
                    "end_ext",
                    "join_start",
                    "join_end",
                ];
                let u = self.upsert(&m.guid, "beam", &cols, &vals)?;
                if self.tally(u) {
                    self.report.beams += 1;
                }
            }
        }
        Ok(())
    }

    fn add_ceiling(&mut self, c: ddc_bridge_proto::Ceiling) -> Result<(), BridgeError> {
        let (lv, _) = self.level(&c.level)?;
        let Some(&st) = self.slab_types.get(&c.slab_type) else {
            return data(format!(
                "ceiling '{}': unknown type '{}'",
                c.guid, c.slab_type
            ));
        };
        let bad = |r: &Vec<P2>| r.len() < 3 || area(r).abs() < 1e-9;
        let outer = orient(self.ring(&c.outer), true);
        if bad(&outer) {
            self.seen.insert(c.guid.clone());
            self.skip("ceiling: invalid");
            return Ok(());
        }
        let mut rings = vec![outer];
        for h in &c.holes {
            let h = orient(self.ring(h), false);
            if bad(&h) {
                // a sliver hole must not cost the whole ceiling
                self.skip("ceiling hole: degenerate");
                self.warn(format!("ceiling '{}': degenerate hole dropped", c.guid));
            } else {
                rings.push(h);
            }
        }
        let vals = [
            Sql::Integer(lv),
            Sql::Integer(st),
            Sql::Real(self.len(c.height)),
        ];
        let mut u = self.upsert(&c.guid, "ceiling", &["level", "stype", "height"], &vals)?;
        if u == Up::Gone {
            return Ok(());
        }
        let id = u.id();
        let new = matches!(u, Up::New(_));
        if self.merge_rings("ceiling", &c.guid, id, &rings, new)?
            && self.sync_rings("ceiling", id, &rings, new)?
            && u == Up::Same(id)
        {
            u = Up::Updated(id);
        }
        if self.tally(u) {
            self.report.ceilings += 1;
        }
        Ok(())
    }

    /// A body measured as one extrusion → native solid, its working plane measured from the level.
    fn add_prism(&mut self, p: ddc_bridge_proto::Prism) -> Result<(), BridgeError> {
        use ddc_geom::{gsolid, v2, v3};
        use ddc_model::{form_json, frame_json, SolidForm, SolidFrame, SolidSection};
        // nine decimals: the same body reads back as the same text on re-import
        let r9 = |x: f64| (x * 1e9).round() / 1e9 + 0.0;
        let (lv, elev) = self.level(&p.level)?;
        let o = p.o.map(|c| self.len(c));
        let (x, y) = (p.x, p.y);
        let z = [
            x[1] * y[2] - x[2] * y[1],
            x[2] * y[0] - x[0] * y[2],
            x[0] * y[1] - x[1] * y[0],
        ];
        let frame = SolidFrame {
            o: v3(r9(o[0]), r9(o[1]), r9(o[2] - elev)),
            x: v3(r9(x[0]), r9(x[1]), r9(x[2])),
            y: v3(r9(y[0]), r9(y[1]), r9(y[2])),
            z: v3(r9(z[0]), r9(z[1]), r9(z[2])),
        };
        let ring: Vec<_> = orient(self.ring(&p.outline), true)
            .into_iter()
            .map(|q| v2(r9(q.0), r9(q.1)))
            .collect();
        let form = SolidForm::Extrusion {
            profile: SolidSection::polygon(ring),
            start: 0.0,
            end: r9(self.len(p.depth)),
        };
        if !frame.is_valid() || gsolid::validate(&form).is_err() {
            self.seen.insert(p.guid.clone());
            self.skip("prism: invalid");
            self.warn(format!("prism '{}': invalid body, not imported", p.guid));
            return Ok(());
        }
        let text = |t: &str| Sql::Text(t.chars().take(200).collect());
        let vals = [
            Sql::Integer(lv),
            text(&p.name),
            Sql::Text("extrusion".into()),
            Sql::Integer(0),
            text(&p.material),
            Sql::Text(frame_json(&frame).to_string()),
            Sql::Text(form_json(&form).to_string()),
        ];
        let u = self.upsert(
            &p.guid,
            "solid",
            &["level", "name", "kind", "void", "material", "plane", "def"],
            &vals,
        )?;
        if u == Up::Gone {
            return Ok(());
        }
        if self.tally(u) {
            self.report.solids += 1;
        }
        // the table has no category column, as a mesh has
        if !p.category.is_empty() {
            self.add_prop(&p.guid, "Category", &Value::Text(p.category), "")?;
        }
        Ok(())
    }

    /// One system per domain for imported runs; the user reassigns runs to real systems.
    fn mep_system(&mut self, domain: ddc_model::MepDomain) -> Result<i64, BridgeError> {
        use ddc_model::{MepDomain as D, SystemKind as K};
        let kind = match domain {
            D::Duct => K::SupplyAir,
            D::Pipe => K::ColdWater,
            D::Conduit | D::CableTray => K::Power,
        };
        let name = format!("Imported {}", domain.name().replace('_', " "));
        let id: Option<i64> = self
            .db
            .query_row("SELECT id FROM mep_system WHERE name=?1", [&name], |r| {
                r.get(0)
            })
            .optional()
            .map_err(db_err)?;
        if let Some(id) = id {
            return Ok(id);
        }
        let [r, g, b] = kind.color();
        self.db
            .execute(
                "INSERT INTO mep_system(name, kind, color) VALUES (?1,?2,?3)",
                params![
                    name,
                    kind.name(),
                    (r as i64) << 16 | (g as i64) << 8 | b as i64
                ],
            )
            .map_err(db_err)?;
        Ok(self.db.last_insert_rowid())
    }

    fn add_mep_run(&mut self, r: MepRun) -> Result<(), BridgeError> {
        let (lv, _) = self.level(&r.level)?;
        let Some(domain) = ddc_model::MepDomain::parse(&r.domain) else {
            self.skip("mep run: unknown domain");
            return Ok(());
        };
        let (shape, w, h) = match self.shape(r.section) {
            Some(ddc_model::MemberShape::Rect { b, h }) => ("rect", b, h),
            Some(ddc_model::MemberShape::Circle { d }) => ("round", d, d),
            _ => {
                self.skip("mep run: no section");
                return Ok(());
            }
        };
        let pts: Vec<(f64, f64, f64)> = r
            .points
            .iter()
            .map(|p| (self.len(p[0]), self.len(p[1]), self.len(p[2])))
            .collect();
        if pts.len() < 2 {
            self.skip("mep run: fewer than 2 points");
            return Ok(());
        }
        let system = self.mep_system(domain)?;
        let vals = [
            Sql::Integer(system),
            Sql::Integer(lv),
            Sql::Text(domain.name().into()),
            Sql::Text(shape.into()),
            Sql::Real(w),
            Sql::Real(h),
            Sql::Real(0.0),
            Sql::Real(0.0),
            Sql::Real(0.0),
            Sql::Null,
            Sql::Null,
            Sql::Null,
            Sql::Real(0.0),
            Sql::Text(ddc_model::points_text(&pts)),
            Sql::Integer(0),
            Sql::Integer(0),
        ];
        let cols = [
            "system",
            "level",
            "domain",
            "shape",
            "w",
            "h",
            "wall",
            "insulation",
            "lining",
            "material",
            "dn",
            "bend",
            "slope",
            "points",
            "cap0",
            "cap1",
        ];
        let u = self.upsert(&r.guid, "mep_run", &cols, &vals)?;
        if self.tally(u) {
            self.report.mep_runs += 1;
        }
        Ok(())
    }

    fn section(&self, s: MullionSection) -> ddc_model::MullionSection {
        match s {
            MullionSection::None => ddc_model::MullionSection::None,
            MullionSection::Rect { width, depth } => ddc_model::MullionSection::Rect {
                w: self.len(width),
                d: self.len(depth),
            },
            MullionSection::Circle { diameter } => ddc_model::MullionSection::Circle {
                d: self.len(diameter),
            },
        }
    }

    /// Curtain wall row plus its panel rows (replaced, snapshot first, when they differ).
    fn add_curtain(&mut self, c: CurtainWall) -> Result<(), BridgeError> {
        let guid = c.guid.clone();
        let (lv, elev) = self.level(&c.level)?;
        let (a, b, bulge) = match c.axis {
            Curve::Line { a, b } => (self.pt(a), self.pt(b), 0.0),
            Curve::Arc { a, mid, b } => {
                let (a, m, b) = (self.pt(a), self.pt(mid), self.pt(b));
                (a, b, arc_bulge(a, m, b).unwrap_or(0.0))
            }
        };
        let height = self.len(c.height);
        let sections = [self.section(c.interior), self.section(c.border)];
        let (lines, top) = self.lines.remove(&guid).unwrap_or_default();
        let bad_section = sections
            .iter()
            .chain(lines.u.iter().chain(&lines.v).map(|l| &l.2))
            .any(|s| {
                matches!(*s, ddc_model::MullionSection::Rect { w, d } if !(w > 0.0 && d > 0.0))
                    || matches!(*s, ddc_model::MullionSection::Circle { d } if !(d > 0.0))
            });
        if dist(a, b) < 1e-6 || height <= 1e-6 || bulge.abs() > 1.0 || bad_section {
            self.seen.insert(guid.clone());
            self.warn(format!(
                "curtain wall '{guid}': bad axis, height or mullions, skipped"
            ));
            self.skip("curtain wall: invalid");
            return Ok(());
        }
        let u: Vec<f64> = c.u.iter().map(|x| self.len(*x)).collect();
        let v: Vec<f64> = c.v.iter().map(|x| self.len(*x)).collect();
        let len = ddc_geom::arc::axis_length(ddc_geom::v2(a.0, a.1), ddc_geom::v2(b.0, b.1), bulge);
        let (nu, nv) = (
            ddc_geom::curtain::explicit_lines(len, &u).len() - 1,
            ddc_geom::curtain::explicit_lines(height, &v).len() - 1,
        );
        // overrides pointing outside the grid are dropped, one warning each
        let mut lines = lines;
        for (name, over, n_line, n_seg) in
            [("u", &mut lines.u, nu, nv), ("v", &mut lines.v, nv, nu)]
        {
            over.retain(|&(k, seg, _)| {
                let ok = k <= n_line && seg.is_none_or(|g| g < n_seg);
                if !ok {
                    let at = seg.map_or(k.to_string(), |g| format!("{k}:{g}"));
                    self.warn(format!(
                        "curtain wall '{guid}': mullion override {name} {at} is outside the grid, dropped"
                    ));
                }
                ok
            });
        }
        let spec = ddc_model::CurtainSpec {
            level: lv,
            name: c.name.clone(),
            a,
            b,
            bulge,
            base_offset: self.len(c.base_offset),
            height,
            u: ddc_model::GridLines::At(u.clone()),
            v: ddc_model::GridLines::At(v.clone()),
            interior: sections[0],
            border: sections[1],
            lines: lines.clone(),
            top: top.clone(),
        };
        if let Err(e) = ddc_model::check_curtain(self.db, &spec) {
            self.warn(format!(
                "curtain wall '{guid}': {e}; stored as a mesh without its grid"
            ));
            return self.curtain_as_mesh(&guid, &spec, elev);
        }
        let mut rows: Vec<(i64, i64, String, String, i64, i64)> = vec![];
        for p in &c.panels {
            if p.i as usize >= nu || p.j as usize >= nv {
                self.warn(format!(
                    "curtain wall '{guid}': panel {},{} outside the grid",
                    p.i, p.j
                ));
                continue;
            }
            let kind = match p.kind {
                PanelKind::Glazed => "glazed",
                PanelKind::Solid => "solid",
                PanelKind::Door => "door",
                PanelKind::Window => "window",
                PanelKind::Empty => "empty",
            };
            rows.push((
                p.i as i64,
                p.j as i64,
                kind.into(),
                p.name.clone(),
                p.swing_side as i64,
                p.swing_hinge as i64,
            ));
        }
        rows.sort_by_key(|r| (r.0, r.1));
        rows.dedup_by_key(|r| (r.0, r.1));
        let vals = [
            Sql::Integer(lv),
            Sql::Text(c.name.clone()),
            Sql::Real(a.0),
            Sql::Real(a.1),
            Sql::Real(b.0),
            Sql::Real(b.1),
            Sql::Real(bulge),
            Sql::Real(self.len(c.base_offset)),
            Sql::Real(height),
            Sql::Text(ddc_model::GridLines::At(u).text()),
            Sql::Text(ddc_model::GridLines::At(v).text()),
            Sql::Text(ddc_model::section_text(&sections[0])),
            Sql::Text(ddc_model::section_text(&sections[1])),
            lines.text().map_or(Sql::Null, Sql::Text),
            if top.is_empty() {
                Sql::Null
            } else {
                Sql::Text(ddc_model::top_profile_text(&top))
            },
        ];
        let mut up = self.upsert(&guid, "curtain_wall", &CURTAIN_COLS, &vals)?;
        if up == Up::Gone {
            return Ok(());
        }
        let id = up.id();
        let have: Vec<(i64, i64, String, String, i64, i64)> = self
            .db
            .prepare_cached(
                "SELECT i,j,kind,name,swing_side,swing_hinge FROM curtain_panel WHERE cw=?1 \
                 ORDER BY i,j",
            )
            .map_err(db_err)?
            .query_map([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        if have != rows {
            if !matches!(up, Up::New(_)) {
                self.snaps
                    .save(self.db, "curtain_panel", "cw", id)
                    .map_err(model_err)?;
                if up == Up::Same(id) {
                    up = Up::Updated(id);
                }
            }
            self.db
                .execute("DELETE FROM curtain_panel WHERE cw=?1", [id])
                .map_err(db_err)?;
            let mut ins = self
                .db
                .prepare_cached(
                    "INSERT INTO curtain_panel(cw,i,j,kind,name,swing_side,swing_hinge) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                )
                .map_err(db_err)?;
            for r in &rows {
                ins.execute(params![id, r.0, r.1, r.2, r.3, r.4, r.5])
                    .map_err(db_err)?;
            }
        }
        if self.tally(up) {
            self.report.curtain_walls += 1;
            let empty = rows.iter().filter(|r| r.2 == "empty").count();
            self.report.curtain_panels += nu * nv - empty;
            self.report.curtain_doors += rows.iter().filter(|r| r.2 == "door").count();
            self.report.curtain_windows += rows.iter().filter(|r| r.2 == "window").count();
        }
        Ok(())
    }

    /// Fallback for a curtain wall the model rejects: one panel over the whole wall (arc: 16
    /// chords) with border mullions, stored as a generic mesh.
    fn curtain_as_mesh(
        &mut self,
        guid: &str,
        s: &ddc_model::CurtainSpec,
        elev: f64,
    ) -> Result<(), BridgeError> {
        use ddc_geom::curtain::{layout, mesh, Axis};
        let ax = match Axis::new(
            ddc_geom::v2(s.a.0, s.a.1),
            ddc_geom::v2(s.b.0, s.b.1),
            s.bulge,
        ) {
            Ok(ax) => ax,
            Err(e) => {
                self.seen.insert(guid.into());
                self.warn(format!("curtain wall '{guid}': {e}, skipped"));
                self.skip("curtain wall: invalid");
                return Ok(());
            }
        };
        let n = if ax.arc.is_some() { 16 } else { 1 };
        let len = ax.length();
        let u: Vec<f64> = (0..=n).map(|k| len * k as f64 / n as f64).collect();
        let none = ddc_geom::curtain::Section::None;
        let l = layout(
            &ax,
            elev + s.base_offset,
            &u,
            &[0.0, s.height],
            none,
            s.border,
        );
        let m = mesh(&l, &vec![Some(0.025); l.cells.len()]);
        let pb: Vec<u8> = m
            .positions
            .iter()
            .flat_map(|p| [p.x, p.y, p.z])
            .flat_map(f64::to_le_bytes)
            .collect();
        let ib: Vec<u8> = m
            .triangles
            .iter()
            .flatten()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        let vals = [
            Sql::Integer(s.level),
            Sql::Text("Curtain Wall".into()),
            Sql::Text(s.name.clone()),
            Sql::Blob(pb),
            Sql::Blob(ib),
        ];
        let cols = ["level", "category", "name", "positions", "indices"];
        let up = self.upsert(guid, "mesh", &cols, &vals)?;
        if self.tally(up) {
            self.report.meshes += 1;
        }
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<(), BridgeError> {
        if self.ended {
            Ok(())
        } else {
            Err(BridgeError::Protocol("stream ended without End".into()))
        }
    }

    fn add_level(&mut self, guid: &str, name: &str, elev: f64) -> Result<(), BridgeError> {
        if let Some(id) = self.existing(guid, "level")? {
            self.guids.insert(("level".into(), id), guid.into());
            self.merge_named(guid, "level", id, name, &[("elevation", Sql::Real(elev))])?;
            let e: f64 = self
                .db
                .query_row("SELECT elevation FROM level WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .map_err(db_err)?;
            self.levels.insert(guid.into(), (id, e));
            return self.link(guid, "level", id);
        }
        let found: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM level WHERE name=?1 AND abs(elevation-?2)<1e-9",
                params![name, elev],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let id = match found {
            Some(id) => {
                let e: f64 = self
                    .db
                    .query_row("SELECT elevation FROM level WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .map_err(db_err)?;
                self.levels.insert(guid.into(), (id, e));
                id
            }
            None => {
                let n = self.free_name("level", name)?;
                self.db
                    .execute(
                        "INSERT INTO level(name,elevation) VALUES (?1,?2)",
                        params![n, elev],
                    )
                    .map_err(db_err)?;
                self.report.levels += 1;
                let id = self.db.last_insert_rowid();
                self.levels.insert(guid.into(), (id, elev));
                id
            }
        };
        self.link(guid, "level", id)?;
        self.record_named(guid, "level", id, name, &["elevation"], found.is_none())
    }

    fn add_type(&mut self, t: Type) -> Result<(), BridgeError> {
        let wall = matches!(t, Type::Wall { .. });
        match t {
            Type::Wall { guid, name, layers } | Type::Slab { guid, name, layers } => {
                let (table, ltable, col) = if wall {
                    ("wall_type", "wall_type_layer", "wtype")
                } else {
                    ("slab_type", "slab_type_layer", "stype")
                };
                if layers.is_empty() || layers.iter().any(|l| l.thickness <= 0.0) {
                    return data(format!(
                        "type '{name}': layers must have positive thickness"
                    ));
                }
                let id = match self.existing(&guid, table)? {
                    Some(id) => {
                        self.guids.insert((table.to_string(), id), guid.clone());
                        self.merge_named(&guid, table, id, &name, &[])?;
                        id
                    }
                    None => {
                        let n = self.free_name(table, &name)?;
                        self.db
                            .execute(&format!("INSERT INTO {table}(name) VALUES (?1)"), [n])
                            .map_err(db_err)?;
                        let id = self.db.last_insert_rowid();
                        for (pos, l) in layers.iter().enumerate() {
                            let mat = self.materials.get(&l.material).unwrap_or(&l.material);
                            self.db
                                .execute(
                                    &format!(
                                        "INSERT INTO {ltable}({col},pos,material,thickness) VALUES (?1,?2,?3,?4)"
                                    ),
                                    params![id, pos as i64, mat, self.len(l.thickness)],
                                )
                                .map_err(db_err)?;
                        }
                        if wall {
                            self.report.wall_types += 1;
                        } else {
                            self.report.slab_types += 1;
                        }
                        self.link(&guid, table, id)?;
                        self.record_named(&guid, table, id, &name, &[], true)?;
                        id
                    }
                };
                self.link(&guid, table, id)?;
                if wall {
                    self.wall_types.insert(guid, id);
                } else {
                    self.slab_types.insert(guid, id);
                }
            }
            Type::Family {
                guid,
                category,
                name,
                params,
            } => {
                self.report.family_types += 1;
                self.family.insert(guid, (category, name, params));
            }
        }
        Ok(())
    }

    fn add_element(&mut self, e: Element) -> Result<(), BridgeError> {
        match e {
            Element::Wall {
                guid,
                level,
                wall_type,
                axis,
                base_offset,
                top,
                offset0,
            } => {
                let (lv, elev) = self.level(&level)?;
                let Some(&wt) = self.wall_types.get(&wall_type) else {
                    return data(format!("wall '{guid}': unknown type '{wall_type}'"));
                };
                let base = self.len(base_offset);
                let height = match top {
                    Top::Unconnected { height } => self.len(height),
                    Top::Level { level, offset } => {
                        self.level(&level)?.1 + self.len(offset) - elev - base
                    }
                };
                if height <= 1e-6 {
                    self.seen.insert(guid.clone());
                    self.warn(format!("wall '{guid}': non-positive height, skipped"));
                    self.skip("wall: invalid");
                    return Ok(());
                }
                let off = offset0.map(|o| self.len(o));
                let (a, b, bulge) = match axis {
                    Curve::Line { a, b } => (self.pt(a), self.pt(b), 0.0),
                    Curve::Arc { a, mid, b } => {
                        let (a, m, b) = (self.pt(a), self.pt(mid), self.pt(b));
                        (a, b, arc_bulge(a, m, b).unwrap_or(0.0))
                    }
                };
                if dist(a, b) < 1e-6 {
                    self.seen.insert(guid.clone());
                    self.warn(format!("wall '{guid}': zero length, skipped"));
                    self.skip("wall: invalid");
                    return Ok(());
                }
                let vals = [
                    Sql::Integer(lv),
                    Sql::Integer(wt),
                    Sql::Real(a.0),
                    Sql::Real(a.1),
                    Sql::Real(b.0),
                    Sql::Real(b.1),
                    Sql::Real(base),
                    Sql::Real(height),
                    off.map_or(Sql::Null, Sql::Real),
                    Sql::Integer((bulge != 0.0) as i64),
                    Sql::Real(bulge),
                    match self.tops.remove(&guid) {
                        Some(t) if bulge == 0.0 => Sql::Text(ddc_model::top_profile_text(&t)),
                        _ => Sql::Null,
                    },
                ];
                let u = self.upsert(&guid, "wall", &WALL_COLS, &vals)?;
                if self.tally(u) {
                    self.report.walls += 1;
                    self.report.wall_segments += 1;
                    if bulge != 0.0 {
                        self.report.arc_walls += 1;
                    }
                }
            }
            Element::Opening {
                guid,
                host,
                kind,
                family_type,
                along,
                width,
                height,
                sill,
                swing_side,
                swing_hinge,
            } => {
                let (along, width, height, sill) = (
                    self.len(along),
                    self.len(width),
                    self.len(height),
                    self.len(sill),
                );
                if width <= 0.0 || height <= 0.0 {
                    return data(format!("opening '{guid}': size must be positive"));
                }
                let Some(wall) = self.existing(&host, "wall")? else {
                    if self.host_deleted_here(&guid, &host)? {
                        return Ok(());
                    }
                    return data(format!("opening '{guid}': unknown host '{host}'"));
                };
                let (ax, ay, bx, by, bulge): (f64, f64, f64, f64, f64) = self
                    .db
                    .prepare_cached("SELECT ax,ay,bx,by,bulge FROM wall WHERE id=?1")
                    .map_err(db_err)?
                    .query_row([wall], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })
                    .map_err(db_err)?;
                let host_len =
                    ddc_geom::arc::axis_length(ddc_geom::v2(ax, ay), ddc_geom::v2(bx, by), bulge);
                if along < -1e-6 || along + width > host_len + 1e-6 {
                    self.seen.insert(guid.clone());
                    self.warn(format!("opening '{guid}': outside its host wall, skipped"));
                    self.skip("opening: outside host");
                    return Ok(());
                }
                let k = match kind {
                    OpeningKind::Door => "door",
                    OpeningKind::Window => "window",
                    OpeningKind::Void => "void",
                };
                let vals = [
                    Sql::Integer(wall),
                    Sql::Real(along.max(0.0)),
                    Sql::Real(width),
                    Sql::Real(sill),
                    Sql::Real(height),
                    Sql::Text(k.into()),
                    Sql::Integer(swing_side as i64),
                    Sql::Integer(swing_hinge as i64),
                ];
                let u = self.upsert(&guid, "opening", &OPENING_COLS, &vals)?;
                if self.tally(u) {
                    match kind {
                        OpeningKind::Door => self.report.doors += 1,
                        OpeningKind::Window => self.report.windows += 1,
                        OpeningKind::Void => self.report.voids += 1,
                    }
                }
                if u == Up::Gone {
                    return Ok(());
                }
                let id = u.id();
                if let Some((_, name, ps)) = family_type.and_then(|t| self.family.get(&t)).cloned()
                {
                    self.put_prop("opening", id, "type.name", &Value::Text(name), "")?;
                    for (k, v) in ps {
                        self.put_prop("opening", id, &format!("type.{k}"), &v, "")?;
                    }
                }
            }
            Element::Slab {
                guid,
                level,
                slab_type,
                offset,
                outer,
                holes,
            } => {
                let (lv, _) = self.level(&level)?;
                let Some(&st) = self.slab_types.get(&slab_type) else {
                    return data(format!("slab '{guid}': unknown type '{slab_type}'"));
                };
                let mut rings = vec![orient(self.ring(&outer), true)];
                rings.extend(holes.iter().map(|h| orient(self.ring(h), false)));
                if rings.iter().any(|r| r.len() < 3 || area(r).abs() < 1e-9) {
                    self.seen.insert(guid.clone());
                    self.warn(format!("slab '{guid}': degenerate boundary, skipped"));
                    self.skip("slab: invalid");
                    return Ok(());
                }
                let slope = self
                    .slopes
                    .remove(&guid)
                    .map(|(p, g)| ddc_model::SlabSlope {
                        p0: ddc_geom::v2(p.0, p.1),
                        gx: g.0,
                        gy: g.1,
                    });
                let sloped = slope.is_some_and(|p| !p.is_flat());
                let vals = [
                    Sql::Integer(lv),
                    Sql::Integer(st),
                    Sql::Real(self.len(offset)),
                    match slope.filter(|p| !p.is_flat()) {
                        Some(p) => Sql::Text(ddc_model::slope_text(&p)),
                        None => Sql::Null,
                    },
                ];
                let cols = ["level", "stype", "offset", "slope"];
                let mut u = self.upsert(&guid, "slab", &cols, &vals)?;
                if u == Up::Gone {
                    return Ok(());
                }
                let id = u.id();
                if self.merge_rings("slab", &guid, id, &rings, matches!(u, Up::New(_)))?
                    && self.sync_vertices(id, &rings, matches!(u, Up::New(_)))?
                    && u == Up::Same(id)
                {
                    u = Up::Updated(id);
                }
                if self.tally(u) {
                    self.report.slabs += 1;
                    if sloped {
                        self.report.sloped_slabs += 1;
                    }
                }
            }
            Element::Room {
                guid,
                level,
                name,
                number,
                height,
                boundary,
                point,
            } => {
                let (lv, _) = self.level(&level)?;
                let ring = self.ring(&boundary);
                let p = match point.map(|p| self.pt(p)).or_else(|| interior_point(&ring)) {
                    Some(p) => p,
                    None => {
                        self.seen.insert(guid.clone());
                        self.warn(format!("room '{guid}': no interior point, skipped"));
                        self.skip("room: invalid");
                        return Ok(());
                    }
                };
                let outline = if ring.len() >= 3 {
                    let r: Vec<(f64, f64)> =
                        ring.iter().map(|q| (round6(q.0), round6(q.1))).collect();
                    Sql::Text(
                        serde_json::to_string(&r).map_err(|e| BridgeError::Data(e.to_string()))?,
                    )
                } else {
                    Sql::Null
                };
                let vals = [
                    Sql::Integer(lv),
                    Sql::Text(name),
                    Sql::Text(number),
                    Sql::Real(p.0),
                    Sql::Real(p.1),
                    Sql::Real(self.len(height)),
                    outline,
                ];
                let cols = ["level", "name", "number", "px", "py", "height", "outline"];
                let u = self.upsert(&guid, "room", &cols, &vals)?;
                if self.tally(u) {
                    self.report.rooms += 1;
                }
            }
            Element::Generic {
                guid,
                level,
                category,
                name,
                positions,
                indices,
            } => {
                let lv = match level {
                    Some(l) => Some(self.level(&l)?.0),
                    None => None,
                };
                if indices.len() % 3 != 0 || indices.iter().any(|&i| i as usize >= positions.len())
                {
                    return data(format!("mesh '{guid}': bad triangle indices"));
                }
                let mut pb = Vec::with_capacity(positions.len() * 24);
                for p in &positions {
                    p.iter()
                        .for_each(|c| pb.extend_from_slice(&self.len(*c).to_le_bytes()));
                }
                let ib: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
                let vals = [
                    lv.map_or(Sql::Null, Sql::Integer),
                    Sql::Text(category),
                    Sql::Text(name),
                    Sql::Blob(pb),
                    Sql::Blob(ib),
                ];
                let cols = ["level", "category", "name", "positions", "indices"];
                let u = self.upsert(&guid, "mesh", &cols, &vals)?;
                if self.tally(u) {
                    self.report.meshes += 1;
                }
            }
        }
        Ok(())
    }

    /// Slab rings: written for a new slab, replaced (snapshot first) when they differ.
    /// True when an existing slab changed.
    fn sync_vertices(
        &mut self,
        id: i64,
        rings: &[Vec<P2>],
        new: bool,
    ) -> Result<bool, BridgeError> {
        self.sync_rings("slab", id, rings, new)
    }

    /// Rings of a slab or ceiling (`owner`_vertex rows); see [`Self::sync_vertices`].
    fn sync_rings(
        &mut self,
        owner: &str,
        id: i64,
        rings: &[Vec<P2>],
        new: bool,
    ) -> Result<bool, BridgeError> {
        let vt = format!("{owner}_vertex");
        let want: Vec<(i64, i64, f64, f64)> = rings
            .iter()
            .enumerate()
            .flat_map(|(ri, r)| {
                r.iter()
                    .enumerate()
                    .map(move |(i, p)| (ri as i64, i as i64, p.0, p.1))
            })
            .collect();
        if !new {
            let have: Vec<(i64, i64, f64, f64)> = self
                .db
                .prepare_cached(&format!(
                    "SELECT ring,idx,x,y FROM {vt} WHERE {owner}=?1 ORDER BY ring,idx"
                ))
                .map_err(db_err)?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(db_err)?
                .collect::<Result<_, _>>()
                .map_err(db_err)?;
            let eq = have.len() == want.len()
                && have.iter().zip(&want).all(|(h, w)| {
                    h.0 == w.0 && h.1 == w.1 && (h.2 - w.2).abs() < 1e-9 && (h.3 - w.3).abs() < 1e-9
                });
            if eq {
                return Ok(false);
            }
            self.snaps
                .save(self.db, &vt, owner, id)
                .map_err(model_err)?;
            self.db
                .execute(&format!("DELETE FROM {vt} WHERE {owner}=?1"), [id])
                .map_err(db_err)?;
        }
        let mut ins = self
            .db
            .prepare_cached(&format!(
                "INSERT INTO {vt}({owner},ring,idx,x,y) VALUES (?1,?2,?3,?4,?5)"
            ))
            .map_err(db_err)?;
        for (ri, i, x, y) in want {
            ins.execute(params![id, ri, i, x, y]).map_err(db_err)?;
        }
        Ok(!new)
    }

    fn ring(&self, r: &[P2]) -> Vec<P2> {
        let mut v: Vec<P2> = r.iter().map(|p| self.pt(*p)).collect();
        if v.len() > 1 && dist(v[0], *v.last().unwrap()) < 1e-9 {
            v.pop();
        }
        v
    }

    fn owner(&self, guid: &str) -> Result<Option<(String, i64)>, BridgeError> {
        self.db
            .prepare_cached("SELECT kind, ref FROM ext_ref WHERE source=?1 AND guid=?2")
            .map_err(db_err)?
            .query_row(params![self.src(), guid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(db_err)
    }

    fn add_prop(
        &mut self,
        owner: &str,
        key: &str,
        v: &Value,
        unit: &str,
    ) -> Result<(), BridgeError> {
        match self.owner(owner)? {
            Some((kind, id)) => {
                self.guids.insert((kind.clone(), id), owner.to_string());
                self.put_prop(&kind, id, key, v, unit)
            }
            None => {
                self.skip("property: unknown owner");
                Ok(())
            }
        }
    }

    fn put_prop(
        &mut self,
        kind: &str,
        id: i64,
        key: &str,
        v: &Value,
        unit: &str,
    ) -> Result<(), BridgeError> {
        let mut unit = unit.to_string();
        let val = match v {
            Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
            Value::Int(i) => rusqlite::types::Value::Integer(*i),
            Value::Real(r) => {
                let (x, u) = self.measure(key, *r, &unit);
                unit = u;
                rusqlite::types::Value::Real(x)
            }
            Value::Text(t) => rusqlite::types::Value::Text(t.clone()),
        };
        let unit = unit.as_str();
        let old: Option<(Sql, String)> = self
            .db
            .prepare_cached("SELECT value, unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3")
            .map_err(db_err)?
            .query_row(params![kind, id, key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(db_err)?;
        if !self.prop_merge(kind, id, key, &val, old.as_ref().map(|o| &o.0))? {
            return Ok(());
        }
        match old {
            Some((v, u)) if same(&v, &val) && u == unit => {}
            Some(_) => {
                self.snaps
                    .save(self.db, "prop", "ref", id)
                    .map_err(model_err)?;
                self.db
                    .execute(
                        "UPDATE prop SET value=?4, unit=?5 WHERE kind=?1 AND ref=?2 AND key=?3",
                        params![kind, id, key, val, unit],
                    )
                    .map_err(db_err)?;
            }
            None => {
                self.db
                    .prepare_cached(
                        "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                    )
                    .map_err(db_err)?
                    .execute(params![kind, id, key, val, unit])
                    .map_err(db_err)?;
                self.report.properties += 1;
            }
        }
        Ok(())
    }
}

/// Dimension of a source data type: 1 length, 2 area, 3 volume, 0 other. Takes plain names
/// and qualified ids (`<vendor>.spec.aec:length-2.0.0`).
fn spec_dim(spec: &str) -> u8 {
    let s = spec.to_ascii_lowercase();
    let s = s.rsplit(':').next().unwrap_or("");
    let s = s.split('-').next().unwrap_or("");
    match s.trim() {
        "length" | "distance" => 1,
        "area" => 2,
        "volume" => 3,
        _ => 0,
    }
}

fn dist(a: P2, b: P2) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn area(r: &[P2]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

fn orient(mut r: Vec<P2>, ccw: bool) -> Vec<P2> {
    if (area(&r) > 0.0) != ccw {
        r.reverse();
    }
    r
}

fn inside(p: P2, r: &[P2]) -> bool {
    let mut c = false;
    let n = r.len();
    for i in 0..n {
        let (a, b) = (r[i], r[(i + n - 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

/// Centroid if inside, else the middle of the first span of a horizontal line through it.
fn interior_point(r: &[P2]) -> Option<P2> {
    if r.len() < 3 || area(r).abs() < 1e-12 {
        return None;
    }
    let n = r.len() as f64;
    let c = (
        r.iter().map(|p| p.0).sum::<f64>() / n,
        r.iter().map(|p| p.1).sum::<f64>() / n,
    );
    if inside(c, r) {
        return Some(c);
    }
    let mut xs: Vec<f64> = (0..r.len())
        .filter_map(|i| {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            ((a.1 > c.1) != (b.1 > c.1)).then(|| a.0 + (c.1 - a.1) * (b.0 - a.0) / (b.1 - a.1))
        })
        .collect();
    xs.sort_by(f64::total_cmp);
    (xs.len() >= 2).then(|| ((xs[0] + xs[1]) / 2.0, c.1))
}

/// bulge (tan(sweep/4), > 0 counter-clockwise) of the arc a→mid→b; None if collinear.
pub(crate) fn arc_bulge(a: P2, m: P2, b: P2) -> Option<f64> {
    use ddc_geom::{arc::ArcAxis, v2, Tol};
    ArcAxis::through(v2(a.0, a.1), v2(m.0, m.1), v2(b.0, b.1), Tol::default())
        .ok()
        .map(|x| x.bulge)
}

/// Stored outline coordinates: 1 µm, so a re-import compares equal.
fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}
