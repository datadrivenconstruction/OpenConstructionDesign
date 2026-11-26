// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Phases: an ordered list `phase` (seq); on an element, the creation and demolition phases
//! (`elem_phase`; no row means created in the first phase, not demolished); on a view, the phase
//! and filter (`view_phase`). A new element gets the current phase (`meta.phase_current`) in the
//! same operation; a deleted element takes its row with it (`sweep`); both steps are undoable.
//! "Demolish" keeps the element in the model: only the demolition phase changes (with host inserts).

use std::collections::HashMap;

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Issue, Model};

/// Element tables that have phases.
pub const PHASED: [&str; 28] = [
    "ramp",
    "wall",
    "opening",
    "curtain_wall",
    "slab",
    "room",
    "col",
    "beam",
    "roof",
    "ceiling",
    "stair",
    "railing",
    "mep_run",
    "comp",
    "shaft",
    // terrain and site (g5)
    "toposolid",
    "topo_pad",
    // structure (g6)
    "iso_footing",
    "strip_footing",
    "raft",
    "brace",
    "truss",
    "beam_system",
    // roof edges and elements (g7)
    "sweep",
    "skylight",
    "soffit",
    "roof_taper",
    // reinforcement (r1): sets attached to a host
    "rebar_set",
];

/// Standard phases (key, English default name); the UI shows a standard phase whose name is
/// unchanged in the display language.
pub const DEFAULT_PHASES: [(&str, &str); 3] = [
    ("existing", "Existing"),
    ("demolition", "Demolition"),
    ("new", "New construction"),
];

/// Default names earlier versions stored (key, name); read as the standard phase of that key.
const LEGACY_PHASE_NAMES: [(&str, &str); 3] = [
    ("existing", "\u{421}\u{443}\u{449}\u{435}\u{441}\u{442}\u{432}\u{443}\u{44e}\u{449}\u{435}\u{435}"),
    ("demolition", "\u{414}\u{435}\u{43c}\u{43e}\u{43d}\u{442}\u{430}\u{436}"),
    ("new", "\u{41d}\u{43e}\u{432}\u{43e}\u{435}"),
];

/// A stored row as the standard phase it is: an old default name (with or without its key)
/// becomes the key and its English default, so existing files show their phases translated.
fn normalize(p: &mut Phase) {
    let Some((k, _)) = LEGACY_PHASE_NAMES
        .iter()
        .find(|(k, n)| *n == p.name && p.key.as_deref().map_or(true, |pk| pk == *k))
    else {
        return;
    };
    p.key = Some(k.to_string());
    p.name = std_phase_en(k).to_string();
}

const CURRENT: &str = "phase_current";

/// English name of a standard phase by key.
pub fn std_phase_en(key: &str) -> &'static str {
    match key {
        "existing" => "Existing",
        "demolition" => "Demolition",
        _ => "New construction",
    }
}

/// A phase from field or cell text: the name (ignoring case and surrounding spaces), the key of
/// a standard phase, or its English name.
pub fn find_phase(phases: &[Phase], text: &str) -> Option<i64> {
    let t = text.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    phases
        .iter()
        .find(|p| p.name.trim().to_lowercase() == t)
        .or_else(|| {
            phases.iter().find(|p| {
                p.key
                    .as_deref()
                    .is_some_and(|k| {
                        k == t
                            || std_phase_en(k).to_lowercase() == t
                            || LEGACY_PHASE_NAMES.iter().any(|(lk, n)| *lk == k && n.to_lowercase() == t)
                    })
            })
        })
        .map(|p| p.id)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub id: i64,
    pub name: String,
    pub seq: i64,
    /// Key of a standard phase (`DEFAULT_PHASES`), None if added by the user.
    pub key: Option<String>,
}

/// Element phases: created / demolished (None means not demolished).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElemPhase {
    pub created: i64,
    pub demolished: Option<i64>,
}

/// Element state relative to the view's phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhaseStatus {
    /// Created earlier, still standing.
    Existing,
    /// Created in this phase.
    New,
    /// Created earlier, demolished in this phase.
    Demolished,
    /// Created and demolished in this phase.
    Temporary,
    /// Created later.
    Future,
    /// Demolished earlier.
    Gone,
}

/// View phase filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PhaseFilter {
    /// No filter: everything standing in the view's phase uses ordinary graphics.
    #[default]
    Off,
    /// Phase result: existing and new, without the demolished.
    Complete,
    /// Survey: existing (including to-be-demolished) in ordinary graphics, no new.
    Existing,
    /// Demolition: existing in halftone, to-be-demolished dashed, no new.
    Demolition,
    /// Design: existing in halftone, new in ordinary graphics, no demolished.
    Project,
    /// New only.
    NewOnly,
    /// All: existing in halftone, demolished dashed, new in ordinary graphics.
    All,
}

impl PhaseFilter {
    pub const ALL: [PhaseFilter; 7] = [
        PhaseFilter::Off,
        PhaseFilter::Complete,
        PhaseFilter::Existing,
        PhaseFilter::Demolition,
        PhaseFilter::Project,
        PhaseFilter::NewOnly,
        PhaseFilter::All,
    ];
    pub fn key(self) -> &'static str {
        match self {
            PhaseFilter::Off => "off",
            PhaseFilter::Complete => "complete",
            PhaseFilter::Existing => "existing",
            PhaseFilter::Demolition => "demolition",
            PhaseFilter::Project => "project",
            PhaseFilter::NewOnly => "new",
            PhaseFilter::All => "all",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == s)
    }

    /// Element graphics in a view.
    pub fn show(self, st: PhaseStatus) -> PhaseShow {
        use PhaseShow::*;
        use PhaseStatus as S;
        if matches!(st, S::Future | S::Gone) {
            return Hidden;
        }
        match (self, st) {
            (PhaseFilter::Off, _) => Normal,
            (PhaseFilter::Complete, S::Existing | S::New) => Normal,
            (PhaseFilter::Existing, S::Existing | S::Demolished) => Normal,
            (PhaseFilter::Demolition, S::Existing) => Halftone,
            (PhaseFilter::Demolition, S::Demolished | S::Temporary) => Dashed,
            (PhaseFilter::Project, S::Existing) => Halftone,
            (PhaseFilter::Project, S::New) => Normal,
            (PhaseFilter::NewOnly, S::New) => Normal,
            (PhaseFilter::All, S::Existing) => Halftone,
            (PhaseFilter::All, S::New) => Normal,
            (PhaseFilter::All, S::Demolished | S::Temporary) => Dashed,
            _ => Hidden,
        }
    }
}

/// How to draw an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhaseShow {
    Hidden,
    Normal,
    Halftone,
    /// Demolished: dashed outline, no fill.
    Dashed,
}

/// View phase and filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewPhase {
    pub phase: i64,
    pub filter: PhaseFilter,
}

/// A view for `view_phase`: level plan, section/elevation, 3D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewKey {
    Plan(i64),
    Section(i64),
    ThreeD,
}

impl ViewKey {
    pub(crate) fn parts(self) -> (&'static str, i64) {
        match self {
            ViewKey::Plan(l) => ("level", l),
            ViewKey::Section(v) => ("view_def", v),
            ViewKey::ThreeD => ("3d", 0),
        }
    }
}

/// Phase demolition volumes: per table, the element count and volume, m³.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Demolition {
    pub by_kind: Vec<(&'static str, usize, f64)>,
    pub count: usize,
    pub volume: f64,
}

/// A phase snapshot for a frame: displaying hundreds of elements without a query for each.
#[derive(Clone, Debug, Default)]
pub struct PhaseIndex {
    pub phases: Vec<Phase>,
    rows: HashMap<&'static str, HashMap<i64, ElemPhase>>,
}

impl PhaseIndex {
    pub fn load(db: &Connection) -> Result<Self, Error> {
        let phases = load_phases(db)?;
        let mut rows: HashMap<&'static str, HashMap<i64, ElemPhase>> = HashMap::new();
        let mut st = db.prepare_cached("SELECT kind, ref, created, demolished FROM elem_phase")?;
        let r = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        for x in r {
            let (k, id, created, demolished) = x?;
            if let Some(k) = static_kind(&k) {
                rows.entry(k).or_default().insert(
                    id,
                    ElemPhase {
                        created,
                        demolished,
                    },
                );
            }
        }
        Ok(Self { phases, rows })
    }

    pub(crate) fn rows(&self) -> impl Iterator<Item = (&'static str, &HashMap<i64, ElemPhase>)> {
        self.rows.iter().map(|(k, m)| (*k, m))
    }

    pub fn first(&self) -> Option<i64> {
        self.phases.first().map(|p| p.id)
    }

    /// Ordinal of a phase (0 is the first).
    pub fn pos(&self, phase: i64) -> Option<usize> {
        self.phases.iter().position(|p| p.id == phase)
    }

    pub fn name(&self, phase: i64) -> &str {
        self.phases
            .iter()
            .find(|p| p.id == phase)
            .map_or("", |p| p.name.as_str())
    }

    pub fn get(&self, kind: &str, id: i64) -> ElemPhase {
        self.rows
            .get(kind)
            .and_then(|m| m.get(&id))
            .copied()
            .unwrap_or(ElemPhase {
                created: self.first().unwrap_or(0),
                demolished: None,
            })
    }

    pub fn status(&self, kind: &str, id: i64, view_phase: i64) -> PhaseStatus {
        let e = self.get(kind, id);
        let (Some(p), Some(c)) = (self.pos(view_phase), self.pos(e.created)) else {
            return PhaseStatus::Existing;
        };
        let d = e.demolished.and_then(|d| self.pos(d));
        status_of(c, d, p)
    }

    pub fn show(&self, kind: &str, id: i64, v: ViewPhase) -> PhaseShow {
        v.filter.show(self.status(kind, id, v.phase))
    }
}

/// State by phase numbers: created `c`, demolished `d`, view `p`.
pub fn status_of(c: usize, d: Option<usize>, p: usize) -> PhaseStatus {
    match d {
        _ if c > p => PhaseStatus::Future,
        Some(d) if d < p => PhaseStatus::Gone,
        Some(d) if d == p && c == p => PhaseStatus::Temporary,
        Some(d) if d == p => PhaseStatus::Demolished,
        _ if c == p => PhaseStatus::New,
        _ => PhaseStatus::Existing,
    }
}

fn static_kind(k: &str) -> Option<&'static str> {
    PHASED.iter().copied().find(|p| *p == k)
}

fn bad(msg: String) -> Error {
    Error::Invalid(msg)
}

pub(crate) fn load_phases(db: &Connection) -> Result<Vec<Phase>, Error> {
    let mut st = db.prepare_cached("SELECT id, name, seq, key FROM phase ORDER BY seq, id")?;
    let r = st.query_map([], |r| {
        Ok(Phase {
            id: r.get(0)?,
            name: r.get(1)?,
            seq: r.get(2)?,
            key: r.get(3)?,
        })
    })?;
    let mut ps: Vec<Phase> = r.collect::<Result<_, _>>()?;
    ps.iter_mut().for_each(normalize);
    Ok(ps)
}

/// Standard phases into an empty table; the current one is the last ("New").
pub(crate) fn seed(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row("SELECT count(*) FROM phase", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    for (i, (k, name)) in DEFAULT_PHASES.iter().enumerate() {
        db.execute(
            "INSERT INTO phase(name, seq, key) VALUES (?1,?2,?3)",
            params![name, i as i64 + 1, k],
        )?;
    }
    db.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        params![CURRENT, db.last_insert_rowid()],
    )?;
    Ok(())
}

fn current(db: &Connection) -> Result<Option<i64>, Error> {
    Ok(db
        .prepare_cached("SELECT value FROM meta WHERE key=?1 AND value IN (SELECT id FROM phase)")?
        .query_row([CURRENT], |r| r.get(0))
        .optional()?)
}

fn first(db: &Connection) -> Result<Option<i64>, Error> {
    Ok(db
        .prepare_cached("SELECT id FROM phase ORDER BY seq, id LIMIT 1")?
        .query_row([], |r| r.get(0))
        .optional()?)
}

/// Tables whose rows are tracked in an operation: elements and types (phases, parameter
/// values), levels and views (view phase), project parameters (their values).
const TRACKED: [&str; 35] = [
    "rebar_set",
    "ramp",
    "toposolid",
    "topo_pad",
    "iso_footing",
    "strip_footing",
    "raft",
    "brace",
    "truss",
    "beam_system",
    "sweep",
    "skylight",
    "soffit",
    "roof_taper",
    "wall",
    "opening",
    "curtain_wall",
    "slab",
    "room",
    "col",
    "beam",
    "roof",
    "ceiling",
    "stair",
    "railing",
    "mep_run",
    "comp",
    "shaft",
    "wall_type",
    "slab_type",
    "member_type",
    "comp_type",
    "level",
    "view_def",
    "pparam",
];

/// Temporary connection triggers write the rows touched by an operation into `ddc_dirty`:
/// 'r' is an overwrite of an existing row (INSERT OR REPLACE), 'i' an insert, 'd' a delete,
/// 'p' / 'v' an explicit write of a phase / parameter value. The cascade and the phase of new
/// elements do not scan whole tables, and a reused rowid is distinguishable from an edit.
pub(crate) fn track(db: &Connection) -> Result<(), Error> {
    let mut sql = String::from(
        "CREATE TEMP TABLE IF NOT EXISTS ddc_dirty(seq INTEGER PRIMARY KEY, tbl TEXT NOT NULL, \
         id INTEGER NOT NULL, ev TEXT NOT NULL);",
    );
    for t in TRACKED {
        sql += &format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ddc_r_{t} BEFORE INSERT ON {t} \
             WHEN NEW.id IS NOT NULL AND EXISTS(SELECT 1 FROM {t} WHERE id=NEW.id) \
             BEGIN INSERT INTO ddc_dirty(tbl, id, ev) VALUES ('{t}', NEW.id, 'r'); END;
             CREATE TEMP TRIGGER IF NOT EXISTS ddc_i_{t} AFTER INSERT ON {t} \
             BEGIN INSERT INTO ddc_dirty(tbl, id, ev) VALUES ('{t}', NEW.id, 'i'); END;
             CREATE TEMP TRIGGER IF NOT EXISTS ddc_d_{t} AFTER DELETE ON {t} \
             BEGIN INSERT INTO ddc_dirty(tbl, id, ev) VALUES ('{t}', OLD.id, 'd'); END;"
        );
    }
    for (t, ev) in [("elem_phase", "p"), ("pparam_val", "v")] {
        for when in ["INSERT", "UPDATE"] {
            sql += &format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS ddc_{ev}_{when} AFTER {when} ON {t} \
                 BEGIN INSERT INTO ddc_dirty(tbl, id, ev) VALUES (NEW.kind, NEW.ref, '{ev}'); END;"
            );
        }
    }
    db.execute_batch(&sql)?;
    Ok(())
}

/// Before an operation: the row journal is empty (undo writes to it too, which does not matter).
pub(crate) fn begin(db: &Connection) -> Result<(), Error> {
    db.prepare_cached("DELETE FROM ddc_dirty")?.execute([])?;
    Ok(())
}

/// What happened to a row during the operation.
#[derive(Default)]
struct Fate {
    first: Option<char>,
    deleted: bool,
    /// Event number of the creation of the latest instance (an insert not after 'r').
    born: Option<i64>,
    /// The last explicit phase / value write.
    phase_w: Option<i64>,
    value_w: Option<i64>,
    pending_r: bool,
}

fn exists(db: &Connection, t: &str, id: i64) -> Result<bool, Error> {
    Ok(db
        .prepare_cached(&format!("SELECT 1 FROM {t} WHERE id=?1"))?
        .query_row([id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Rows of `table` selected by rowid query are deleted with a snapshot in `inv`.
fn take_rows(
    db: &Connection,
    table: &str,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
    mut inv: Inv,
) -> Result<Inv, Error> {
    let rows: Vec<i64> = db
        .prepare_cached(sql)?
        .query_map(args, |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for rowid in rows {
        inv = inv.restore(db, table, "rowid", rowid)?;
        db.prepare_cached(&format!("DELETE FROM {table} WHERE rowid=?1"))?
            .execute([rowid])?;
    }
    Ok(inv)
}

const EP_ROWS: &str = "SELECT rowid FROM elem_phase WHERE kind=?1 AND ref=?2";
const PV_ROWS: &str = "SELECT rowid FROM pparam_val WHERE kind=?1 AND ref=?2";

/// After an operation, by the row journal: deleted elements, levels, views and parameters take
/// their phases and values with them; new elements (including on a reused id) get the current
/// phase unless one was written explicitly. All with snapshots in the inverse commands of the same entry.
pub(crate) fn settle(db: &Connection, mut inv: Inv) -> Result<Inv, Error> {
    let mut fates: HashMap<(String, i64), Fate> = HashMap::new();
    {
        let mut st = db.prepare_cached("SELECT seq, tbl, id, ev FROM ddc_dirty ORDER BY seq")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for x in rows {
            let (seq, t, id, ev) = x?;
            let f = fates.entry((t, id)).or_default();
            match ev.as_str() {
                "r" => {
                    f.first.get_or_insert('r');
                    f.pending_r = true;
                }
                "i" => {
                    f.first.get_or_insert('i');
                    if !f.pending_r {
                        f.born = Some(seq);
                    }
                    f.pending_r = false;
                }
                "d" => {
                    f.first.get_or_insert('d');
                    f.deleted = true;
                    f.born = None;
                    f.pending_r = false;
                }
                "p" => f.phase_w = Some(seq),
                "v" => f.value_w = Some(seq),
                _ => {}
            }
        }
    }
    if fates.is_empty() {
        return Ok(inv);
    }
    let mut cur_first: Option<(Option<i64>, Option<i64>)> = None;
    let mut keys: Vec<&(String, i64)> = fates.keys().collect();
    keys.sort();
    for key in keys {
        let f = &fates[key];
        let id = key.1;
        // only phase/value writes, with no element row event: skip
        let Some(tk) = TRACKED.iter().copied().find(|x| *x == key.0) else {
            continue;
        };
        let phased = PHASED.contains(&tk);
        let valued = crate::params::VALUE_KINDS.contains(&tk);
        if !exists(db, tk, id)? {
            if !f.deleted {
                continue;
            }
            if phased {
                inv = take_rows(db, "elem_phase", EP_ROWS, &[&tk, &id], inv)?;
            }
            if valued {
                inv = take_rows(db, "pparam_val", PV_ROWS, &[&tk, &id], inv)?;
            }
            inv = take_rows(
                db,
                "option_member",
                "SELECT rowid FROM option_member WHERE kind=?1 AND ref=?2",
                &[&tk, &id],
                inv,
            )?;
            inv = match tk {
                "level" | "view_def" => {
                    let inv = take_rows(
                        db,
                        "view_gfx",
                        "SELECT rowid FROM view_gfx WHERE vkind=?1 AND ref=?2",
                        &[&tk, &id],
                        inv,
                    )?;
                    take_rows(
                        db,
                        "view_phase",
                        "SELECT rowid FROM view_phase WHERE vkind=?1 AND ref=?2",
                        &[&tk, &id],
                        inv,
                    )?
                }
                "pparam" => take_rows(
                    db,
                    "pparam_val",
                    "SELECT rowid FROM pparam_val WHERE param=?1",
                    &[&id],
                    inv,
                )?,
                _ => inv,
            };
            continue;
        }
        // alive; new if created in the operation: the first event is an insert, or there was a deletion
        let Some(born) = f.born.filter(|_| f.first == Some('i') || f.deleted) else {
            continue;
        };
        // values of a deleted predecessor with the same id are not its own
        if valued && f.deleted && f.value_w.is_none_or(|w| w < born) {
            inv = take_rows(db, "pparam_val", PV_ROWS, &[&tk, &id], inv)?;
        }
        if !phased || f.phase_w.is_some_and(|w| w > born) {
            continue; // the new element's phase was written explicitly
        }
        let (cur, fst) = match cur_first {
            Some(x) => x,
            None => *cur_first.insert((current(db)?, first(db)?)),
        };
        if f.deleted || f.phase_w.is_some() {
            inv = take_rows(db, "elem_phase", EP_ROWS, &[&tk, &id], inv)?;
        }
        // no row = the first phase
        if let Some(c) = cur.filter(|c| Some(*c) != fst) {
            let n = db
                .prepare_cached(
                    "INSERT OR IGNORE INTO elem_phase(kind, ref, created, demolished) \
                     VALUES (?1,?2,?3,NULL)",
                )?
                .execute(params![tk, id, c])?;
            if n > 0 {
                inv = Inv::default()
                    .sql(
                        "DELETE FROM elem_phase WHERE kind=?1 AND ref=?2",
                        vec![Value::Text(tk.into()), Value::Integer(id)],
                    )
                    .then(inv);
            }
        }
    }
    begin(db)?;
    Ok(inv)
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    let mut push = |table: &str, sql: &str, problem: String| -> Result<(), Error> {
        let mut st = db.prepare(sql)?;
        let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        out.extend(ids.into_iter().map(|id| Issue {
            table: table.into(),
            id,
            problem: problem.clone(),
        }));
        Ok(())
    };
    for t in PHASED {
        push(
            "elem_phase",
            &format!(
                "SELECT rowid FROM elem_phase WHERE kind='{t}' AND ref NOT IN (SELECT id FROM {t})"
            ),
            format!("phases of a missing {t}"),
        )?;
    }
    let known = PHASED
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    push(
        "elem_phase",
        &format!("SELECT rowid FROM elem_phase WHERE kind NOT IN ({known})"),
        "phases of an unknown element kind".into(),
    )?;
    push(
        "elem_phase",
        "SELECT rowid FROM elem_phase WHERE created NOT IN (SELECT id FROM phase) \
         OR (demolished IS NOT NULL AND demolished NOT IN (SELECT id FROM phase))",
        "missing phase".into(),
    )?;
    push(
        "elem_phase",
        "SELECT e.rowid FROM elem_phase e JOIN phase c ON c.id=e.created \
         JOIN phase d ON d.id=e.demolished WHERE d.seq<c.seq",
        "demolished before it was created".into(),
    )?;
    push(
        "view_phase",
        "SELECT rowid FROM view_phase WHERE phase NOT IN (SELECT id FROM phase)",
        "view phase is missing".into(),
    )?;
    push(
        "phase",
        "SELECT a.id FROM phase a JOIN phase b ON a.seq=b.seq AND a.id>b.id",
        "two phases share one position".into(),
    )?;
    Ok(out)
}

/// Whether the element exists.
fn element_exists(db: &Connection, kind: &str, id: i64) -> Result<bool, Error> {
    let Some(k) = static_kind(kind) else {
        return Err(bad(format!("element kind '{kind}' has no phases")));
    };
    Ok(db
        .prepare_cached(&format!("SELECT 1 FROM {k} WHERE id=?1"))?
        .query_row([id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Writes an element's phases; the previous state (a row or its absence) goes to `inv`.
pub(crate) fn put(
    db: &Connection,
    kind: &str,
    id: i64,
    e: ElemPhase,
    inv: Inv,
) -> Result<Inv, Error> {
    if !element_exists(db, kind, id)? {
        return Err(bad(format!("no such element {kind} {id}")));
    }
    let seq = |p: i64| -> Result<i64, Error> {
        db.prepare_cached("SELECT seq FROM phase WHERE id=?1")?
            .query_row([p], |r| r.get(0))
            .optional()?
            .ok_or_else(|| bad(format!("no such phase {p}")))
    };
    let c = seq(e.created)?;
    if let Some(d) = e.demolished {
        if seq(d)? < c {
            return Err(Error::Conflict(format!(
                "{kind} {id}: demolition phase precedes the creation phase"
            )));
        }
    }
    let old: Option<i64> = db
        .prepare_cached("SELECT rowid FROM elem_phase WHERE kind=?1 AND ref=?2")?
        .query_row(params![kind, id], |r| r.get(0))
        .optional()?;
    let inv = match old {
        Some(rowid) => inv.restore(db, "elem_phase", "rowid", rowid)?,
        None => inv.sql(
            "DELETE FROM elem_phase WHERE kind=?1 AND ref=?2",
            vec![Value::Text(kind.into()), Value::Integer(id)],
        ),
    };
    match old {
        Some(rowid) => db
            .prepare_cached("UPDATE elem_phase SET created=?2, demolished=?3 WHERE rowid=?1")?
            .execute(params![rowid, e.created, e.demolished])?,
        None => db
            .prepare_cached(
                "INSERT INTO elem_phase(kind, ref, created, demolished) VALUES (?1,?2,?3,?4)",
            )?
            .execute(params![kind, id, e.created, e.demolished])?,
    };
    Ok(inv)
}

fn get(db: &Connection, kind: &str, id: i64) -> Result<ElemPhase, Error> {
    let row: Option<(i64, Option<i64>)> = db
        .prepare_cached("SELECT created, demolished FROM elem_phase WHERE kind=?1 AND ref=?2")?
        .query_row(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(match row {
        Some((created, demolished)) => ElemPhase {
            created,
            demolished,
        },
        None => ElemPhase {
            created: first(db)?.ok_or_else(|| bad("model has no phases".into()))?,
            demolished: None,
        },
    })
}

/// Host inserts demolished together with it.
fn hosted(db: &Connection, kind: &str, id: i64) -> Result<Vec<(&'static str, i64)>, Error> {
    let q: &[(&'static str, &str)] = match kind {
        "wall" => &[
            ("opening", "SELECT id FROM opening WHERE wall=?1"),
            ("comp", "SELECT id FROM comp WHERE host_wall=?1"),
        ],
        "slab" => &[("comp", "SELECT id FROM comp WHERE host_slab=?1")],
        "ceiling" => &[("comp", "SELECT id FROM comp WHERE host_ceiling=?1")],
        "stair" => &[("railing", "SELECT id FROM railing WHERE stair=?1")],
        _ => &[],
    };
    let mut out = vec![];
    for (k, sql) in q {
        let mut st = db.prepare_cached(sql)?;
        for x in st.query_map([id], |r| r.get::<_, i64>(0))? {
            out.push((*k, x?));
        }
    }
    Ok(out)
}

fn volume_of(db: &Connection, kind: &str, id: i64) -> Result<f64, Error> {
    let sql = match kind {
        "wall" => "SELECT COALESCE(SUM(volume),0) FROM wall_qty WHERE wall=?1",
        "slab" => "SELECT COALESCE(SUM(volume),0) FROM slab_qty WHERE slab=?1",
        "roof" => "SELECT COALESCE(SUM(volume),0) FROM roof_qty WHERE roof=?1",
        "ceiling" => "SELECT COALESCE(SUM(volume),0) FROM ceiling_qty WHERE ceiling=?1",
        "col" => "SELECT COALESCE(SUM(volume),0) FROM member_qty WHERE kind='col' AND ref=?1",
        "beam" => "SELECT COALESCE(SUM(volume),0) FROM member_qty WHERE kind='beam' AND ref=?1",
        "stair" => {
            "SELECT COALESCE(SUM(volume+landing_volume+stringer_volume),0) FROM stair_qty WHERE stair=?1"
        }
        "railing" => "SELECT COALESCE(SUM(volume),0) FROM railing_qty WHERE railing=?1",
        "comp" => "SELECT COALESCE(SUM(volume),0) FROM comp_qty WHERE comp=?1",
        "ramp" => return crate::ramps::volume(db, id),
        _ => return Ok(0.0),
    };
    Ok(db.prepare_cached(sql)?.query_row([id], |r| r.get(0))?)
}

impl Model {
    /// Phases in order.
    pub fn phases(&self) -> Result<Vec<Phase>, Error> {
        load_phases(&self.db)
    }

    pub fn phase_index(&self) -> Result<PhaseIndex, Error> {
        PhaseIndex::load(&self.db)
    }

    /// A new phase after `after` (None appends at the end).
    pub fn add_phase(&mut self, name: &str, after: Option<i64>) -> Result<i64, Error> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(bad("phase name is empty".into()));
        }
        self.op("phase_add", |db| {
            let ps = load_phases(db)?;
            if ps.iter().any(|p| p.name == name) {
                return Err(bad(format!("phase '{name}' already exists")));
            }
            let at = match after {
                Some(a) => {
                    ps.iter()
                        .position(|p| p.id == a)
                        .ok_or_else(|| bad(format!("no such phase {a}")))?
                        + 1
                }
                None => ps.len(),
            };
            let mut inv = Inv::default();
            // shift the tail: seq of the following phases +1
            for p in &ps[at..] {
                inv = inv.restore(db, "phase", "id", p.id)?;
                db.execute("UPDATE phase SET seq=seq+1 WHERE id=?1", [p.id])?;
            }
            let seq = ps
                .get(at)
                .map_or(ps.last().map_or(1, |p| p.seq + 1), |p| p.seq);
            db.execute(
                "INSERT INTO phase(name, seq, key) VALUES (?1,?2,NULL)",
                params![name, seq],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("phase", "id", id).then(inv)))
        })
    }

    pub fn rename_phase(&mut self, id: i64, name: &str) -> Result<(), Error> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(bad("phase name is empty".into()));
        }
        self.op("phase_rename", |db| {
            if load_phases(db)?.iter().any(|p| p.name == name && p.id != id) {
                return Err(bad(format!("phase '{name}' already exists")));
            }
            let inv = Inv::default().restore(db, "phase", "id", id)?;
            if db.execute("UPDATE phase SET name=?2 WHERE id=?1", params![id, name])? == 0 {
                return Err(bad(format!("no such phase {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// Moves a phase to position `to` (0 is first). If any element would end up demolished
    /// before being created, it is rejected with that element's name and nothing changes.
    pub fn move_phase(&mut self, id: i64, to: usize) -> Result<(), Error> {
        self.op("phase_move", |db| {
            let mut ps = load_phases(db)?;
            let from = ps
                .iter()
                .position(|p| p.id == id)
                .ok_or_else(|| bad(format!("no such phase {id}")))?;
            let old_first = ps[0].id;
            let p = ps.remove(from);
            ps.insert(to.min(ps.len()), p);
            let mut inv = Inv::default();
            // the first phase changed: elements without a row stay in the previous first one
            if ps[0].id != old_first {
                inv = materialize(db, old_first)?;
            }
            for (i, p) in ps.iter().enumerate() {
                inv = inv.restore(db, "phase", "id", p.id)?;
                db.execute("UPDATE phase SET seq=?2 WHERE id=?1", params![p.id, i as i64 + 1])?;
            }
            let bad_row: Option<(String, i64)> = db
                .query_row(
                    "SELECT e.kind, e.ref FROM elem_phase e JOIN phase c ON c.id=e.created                      JOIN phase d ON d.id=e.demolished WHERE d.seq<c.seq LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((k, r)) = bad_row {
                return Err(Error::Conflict(format!(
                    "{k} {r}: demolition would precede creation"
                )));
            }
            Ok(((), id, inv))
        })
    }

    /// Deletes a phase. References (elements, views, the current phase) are moved to `into`;
    /// without `into`, if there are references, `Referenced`. The last phase cannot be deleted.
    pub fn delete_phase(&mut self, id: i64, into: Option<i64>) -> Result<(), Error> {
        self.op("phase_delete", |db| {
            let ps = load_phases(db)?;
            let pos = ps
                .iter()
                .position(|p| p.id == id)
                .ok_or_else(|| bad(format!("no such phase {id}")))?;
            if ps.len() == 1 {
                return Err(bad("the only phase cannot be deleted".into()));
            }
            if into == Some(id) {
                return Err(bad("phase cannot be merged into itself".into()));
            }
            if let Some(t) = into {
                if !ps.iter().any(|p| p.id == t) {
                    return Err(bad(format!("no such phase {t}")));
                }
            }
            let mut inv = Inv::default().restore(db, "phase", "id", id)?;
            // elements without a row live in the first phase; their explicit rows are removed last
            let mut tail = Inv::default();
            if pos == 0 {
                let mut unrowed: Vec<(&'static str, i64)> = vec![];
                for t in PHASED {
                    let mut st = db.prepare(&format!(
                        "SELECT id FROM {t} WHERE id NOT IN                          (SELECT ref FROM elem_phase WHERE kind='{t}') ORDER BY id LIMIT 50"
                    ))?;
                    for x in st.query_map([], |r| r.get::<_, i64>(0))? {
                        unrowed.push((t, x?));
                    }
                }
                if !unrowed.is_empty() {
                    if into.is_none() {
                        return Err(Error::Referenced {
                            kind: "phase",
                            id,
                            by: unrowed,
                        });
                    }
                    tail = materialize(db, id)?;
                }
            }
            let mut by: Vec<(&'static str, i64)> = vec![];
            {
                let mut st = db.prepare(
                    "SELECT kind, ref FROM elem_phase WHERE created=?1 OR demolished=?1 ORDER BY kind, ref",
                )?;
                for x in st.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                    let (k, r) = x?;
                    by.push((static_kind(&k).unwrap_or("element"), r));
                }
                let mut st = db.prepare("SELECT ref FROM view_phase WHERE phase=?1 ORDER BY ref")?;
                for x in st.query_map([id], |r| r.get::<_, i64>(0))? {
                    by.push(("view_phase", x?));
                }
            }
            let Some(t) = into else {
                if !by.is_empty() {
                    return Err(Error::Referenced { kind: "phase", id, by });
                }
                return finish_delete(db, id, ps, inv.then(tail));
            };
            // move: snapshots of the affected rows, then replacement
            let rows: Vec<i64> = {
                let mut st =
                    db.prepare("SELECT rowid FROM elem_phase WHERE created=?1 OR demolished=?1")?;
                let r = st.query_map([id], |r| r.get(0))?;
                r.collect::<Result<_, _>>()?
            };
            for r in rows {
                inv = inv.restore(db, "elem_phase", "rowid", r)?;
            }
            let views: Vec<i64> = {
                let mut st = db.prepare("SELECT rowid FROM view_phase WHERE phase=?1")?;
                let r = st.query_map([id], |r| r.get(0))?;
                r.collect::<Result<_, _>>()?
            };
            for r in views {
                inv = inv.restore(db, "view_phase", "rowid", r)?;
            }
            db.execute("UPDATE elem_phase SET created=?2 WHERE created=?1", params![id, t])?;
            db.execute("UPDATE elem_phase SET demolished=?2 WHERE demolished=?1", params![id, t])?;
            db.execute("UPDATE view_phase SET phase=?2 WHERE phase=?1", params![id, t])?;
            // demolition before creation after the merge is rejected
            let bad_row: Option<(String, i64)> = db
                .query_row(
                    "SELECT e.kind, e.ref FROM elem_phase e JOIN phase c ON c.id=e.created \
                     JOIN phase d ON d.id=e.demolished WHERE d.seq<c.seq LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((k, r)) = bad_row {
                return Err(Error::Conflict(format!(
                    "{k} {r}: demolition would precede creation"
                )));
            }
            finish_delete(db, id, ps, inv.then(tail))
        })
    }

    /// The current phase: new elements get it (usually the active view's phase).
    pub fn current_phase(&self) -> Result<i64, Error> {
        match current(&self.db)? {
            Some(c) => Ok(c),
            None => load_phases(&self.db)?
                .last()
                .map(|p| p.id)
                .ok_or_else(|| bad("model has no phases".into())),
        }
    }

    /// A setting, not a model edit: it is not journaled.
    pub fn set_current_phase(&mut self, id: i64) -> Result<(), Error> {
        if !load_phases(&self.db)?.iter().any(|p| p.id == id) {
            return Err(bad(format!("no such phase {id}")));
        }
        self.db.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![CURRENT, id],
        )?;
        Ok(())
    }

    pub fn element_phase(&self, kind: &str, id: i64) -> Result<ElemPhase, Error> {
        get(&self.db, kind, id)
    }

    /// Creation/demolition phases of many elements in one operation. `created` None leaves it
    /// unchanged; `demolished` None leaves it unchanged, Some(None) means "not demolished".
    pub fn set_element_phases(
        &mut self,
        items: &[(&str, i64)],
        created: Option<i64>,
        demolished: Option<Option<i64>>,
    ) -> Result<(), Error> {
        if items.is_empty() {
            return Ok(());
        }
        self.op("phase_set", |db| {
            let mut inv = Inv::default();
            for &(k, id) in items {
                let mut e = get(db, k, id)?;
                if let Some(c) = created {
                    e.created = c;
                }
                if let Some(d) = demolished {
                    e.demolished = d;
                }
                inv = put(db, k, id, e, Inv::default())?.then(inv);
            }
            Ok(((), items[0].1, inv))
        })
    }

    /// "Demolish" in phase `phase`: the element stays, and its inserts (openings, components, stair
    /// railings) are demolished with it. Returns the demolished elements.
    pub fn demolish(
        &mut self,
        items: &[(&str, i64)],
        phase: i64,
    ) -> Result<Vec<(&'static str, i64)>, Error> {
        if items.is_empty() {
            return Ok(vec![]);
        }
        self.op("phase_demolish", |db| {
            let ix = PhaseIndex::load(db)?;
            let at = ix
                .pos(phase)
                .ok_or_else(|| bad(format!("no such phase {phase}")))?;
            // the selected ones are demolished (or rejected); inserts only those standing at this phase:
            // one demolished earlier keeps its phase, one created later is not touched
            let mut all: Vec<((&'static str, i64), bool)> = vec![];
            for &(k, id) in items {
                let k = static_kind(k)
                    .ok_or_else(|| bad(format!("element kind '{k}' has no phases")))?;
                match all.iter_mut().find(|x| x.0 == (k, id)) {
                    Some(x) => x.1 = true,
                    None => all.push(((k, id), true)),
                }
                for h in hosted(db, k, id)? {
                    if !all.iter().any(|x| x.0 == h) {
                        all.push((h, false));
                    }
                }
            }
            let mut done: Vec<(&'static str, i64)> = vec![];
            let mut inv = Inv::default();
            for &((k, id), direct) in &all {
                let mut e = get(db, k, id)?;
                if !direct {
                    let born_later = ix.pos(e.created).is_some_and(|c| c > at);
                    let gone_earlier = e
                        .demolished
                        .and_then(|d| ix.pos(d))
                        .is_some_and(|d| d <= at);
                    if born_later || gone_earlier {
                        continue;
                    }
                }
                e.demolished = Some(phase);
                inv = put(db, k, id, e, Inv::default())?.then(inv);
                done.push((k, id));
            }
            Ok((done, items[0].1, inv))
        })
    }

    /// View phase and filter; with no row, the last phase without a filter.
    pub fn view_phase(&self, v: ViewKey) -> Result<ViewPhase, Error> {
        let (k, r) = v.parts();
        let row: Option<(i64, String)> = self
            .db
            .prepare_cached(
                "SELECT phase, filter FROM view_phase WHERE vkind=?1 AND ref=?2 \
                 AND phase IN (SELECT id FROM phase)",
            )?
            .query_row(params![k, r], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(match row {
            Some((phase, f)) => ViewPhase {
                phase,
                filter: PhaseFilter::from_key(&f).unwrap_or_default(),
            },
            None => ViewPhase {
                phase: load_phases(&self.db)?
                    .last()
                    .map(|p| p.id)
                    .ok_or_else(|| bad("model has no phases".into()))?,
                filter: PhaseFilter::Off,
            },
        })
    }

    pub fn set_view_phase(&mut self, v: ViewKey, vp: ViewPhase) -> Result<(), Error> {
        let (k, r) = v.parts();
        self.op("phase_view", |db| {
            if !load_phases(db)?.iter().any(|p| p.id == vp.phase) {
                return Err(bad(format!("no such phase {}", vp.phase)));
            }
            let ok = match k {
                "level" | "view_def" => {
                    db.query_row(&format!("SELECT count(*) FROM {k} WHERE id=?1"), [r], |r| {
                        r.get::<_, i64>(0)
                    })? > 0
                }
                _ => true,
            };
            if !ok {
                return Err(bad(format!("no such view {k} {r}")));
            }
            let old: Option<i64> = db
                .query_row(
                    "SELECT rowid FROM view_phase WHERE vkind=?1 AND ref=?2",
                    params![k, r],
                    |r| r.get(0),
                )
                .optional()?;
            let inv = match old {
                Some(rowid) => Inv::default().restore(db, "view_phase", "rowid", rowid)?,
                None => Inv::default().sql(
                    "DELETE FROM view_phase WHERE vkind=?1 AND ref=?2",
                    vec![Value::Text(k.into()), Value::Integer(r)],
                ),
            };
            db.execute(
                "INSERT OR REPLACE INTO view_phase(vkind, ref, phase, filter) VALUES (?1,?2,?3,?4)",
                params![k, r, vp.phase, vp.filter.key()],
            )?;
            Ok(((), r, inv))
        })
    }

    /// Phase demolition schedule: the elements demolished in it and their volume.
    pub fn demolition(&self, phase: i64) -> Result<Demolition, Error> {
        let mut by: Vec<(&'static str, usize, f64)> = vec![];
        let mut st = self.db.prepare_cached(
            "SELECT kind, ref FROM elem_phase WHERE demolished=?1 ORDER BY kind, ref",
        )?;
        let rows: Vec<(String, i64)> = st
            .query_map([phase], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (k, id) in rows {
            let Some(k) = static_kind(&k) else { continue };
            let v = volume_of(&self.db, k, id)?;
            match by.iter_mut().find(|b| b.0 == k) {
                Some(b) => {
                    b.1 += 1;
                    b.2 += v;
                }
                None => by.push((k, 1, v)),
            }
        }
        Ok(Demolition {
            count: by.iter().map(|b| b.1).sum(),
            volume: by.iter().map(|b| b.2).sum(),
            by_kind: by,
        })
    }
}

/// Gives elements without a row an explicit "created in `phase`" row (before the first phase changes).
/// Returns the inverse command, which belongs at the end of the operation's inverse commands.
fn materialize(db: &Connection, phase: i64) -> Result<Inv, Error> {
    let max: i64 = db.query_row("SELECT COALESCE(MAX(rowid),0) FROM elem_phase", [], |r| {
        r.get(0)
    })?;
    for t in PHASED {
        db.execute(
            &format!(
                "INSERT INTO elem_phase(kind, ref, created, demolished) SELECT '{t}', id, ?1, NULL                  FROM {t} WHERE id NOT IN (SELECT ref FROM elem_phase WHERE kind='{t}')"
            ),
            [phase],
        )?;
    }
    Ok(Inv::default().sql(
        "DELETE FROM elem_phase WHERE rowid>?1",
        vec![Value::Integer(max)],
    ))
}

fn finish_delete(
    db: &Connection,
    id: i64,
    ps: Vec<Phase>,
    mut inv: Inv,
) -> Result<((), i64, Inv), Error> {
    let cur = current(db)?;
    if cur == Some(id) {
        let next = ps
            .iter()
            .rev()
            .find(|p| p.id != id)
            .map(|p| p.id)
            .expect("two phases at least");
        inv = inv.sql(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            vec![Value::Text(CURRENT.into()), Value::Integer(id)],
        );
        db.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![CURRENT, next],
        )?;
    }
    db.execute("DELETE FROM phase WHERE id=?1", [id])?;
    Ok(((), id, inv))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, OpeningSpec, WallSpec};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (m, lv, t)
    }

    fn wall(m: &mut Model, lv: i64, t: i64, y: f64) -> i64 {
        m.add_wall(&WallSpec {
            a: (0.0, y),
            b: (5.0, y),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
    }

    fn ids(m: &Model) -> (i64, i64, i64) {
        let p = m.phases().unwrap();
        (p[0].id, p[1].id, p[2].id)
    }

    #[test]
    fn phase_lookup_ignores_case_and_accepts_keys() {
        let (m, _, _) = model();
        let ps = m.phases().unwrap();
        assert_eq!(find_phase(&ps, "  new construction "), Some(ps[2].id));
        assert_eq!(find_phase(&ps, "existing"), Some(ps[0].id));
        assert_eq!(find_phase(&ps, "New construction"), Some(ps[2].id));
        assert_eq!(find_phase(&ps, "no such one"), None);
        assert_eq!(find_phase(&ps, " "), None);
    }

    #[test]
    fn status_matrix() {
        use PhaseStatus::*;
        assert_eq!(status_of(0, None, 0), New);
        assert_eq!(status_of(0, None, 2), Existing);
        assert_eq!(status_of(2, None, 1), Future);
        assert_eq!(status_of(0, Some(1), 1), Demolished);
        assert_eq!(status_of(1, Some(1), 1), Temporary);
        assert_eq!(status_of(0, Some(1), 2), Gone);
        assert_eq!(PhaseFilter::Demolition.show(Demolished), PhaseShow::Dashed);
        assert_eq!(PhaseFilter::Project.show(Existing), PhaseShow::Halftone);
        assert_eq!(PhaseFilter::Project.show(Demolished), PhaseShow::Hidden);
        assert_eq!(PhaseFilter::Complete.show(New), PhaseShow::Normal);
        assert_eq!(PhaseFilter::Off.show(Gone), PhaseShow::Hidden);
        for f in PhaseFilter::ALL {
            assert_eq!(PhaseFilter::from_key(f.key()), Some(f));
        }
    }

    #[test]
    fn new_elements_take_current_phase_and_undo_clears_it() {
        let (mut m, lv, t) = model();
        let (ex, _, new) = ids(&m);
        assert_eq!(m.current_phase().unwrap(), new);
        let a = wall(&mut m, lv, t, 0.0);
        assert_eq!(m.element_phase("wall", a).unwrap().created, new);
        m.set_current_phase(ex).unwrap();
        let b = wall(&mut m, lv, t, 2.0);
        // the first phase has no row
        assert_eq!(m.element_phase("wall", b).unwrap().created, ex);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM elem_phase", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        m.undo().unwrap();
        m.undo().unwrap();
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM elem_phase", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        m.assert_integrity();
    }

    #[test]
    fn demolish_keeps_element_takes_hosted_and_undoes() {
        let (mut m, lv, t) = model();
        let (ex, demo, new) = ids(&m);
        m.set_current_phase(ex).unwrap();
        let w = wall(&mut m, lv, t, 0.0);
        let d = m
            .add_opening(
                w,
                &OpeningSpec {
                    along: 1.0,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        let gone = m.demolish(&[("wall", w)], demo).unwrap();
        assert_eq!(gone, vec![("wall", w), ("opening", d)]);
        let ix = m.phase_index().unwrap();
        assert_eq!(ix.status("wall", w, demo), PhaseStatus::Demolished);
        assert_eq!(ix.status("opening", d, new), PhaseStatus::Gone);
        let vp = ViewPhase {
            phase: demo,
            filter: PhaseFilter::Demolition,
        };
        assert_eq!(ix.show("wall", w, vp), PhaseShow::Dashed);
        let dm = m.demolition(demo).unwrap();
        assert_eq!(dm.count, 2);
        // wall volume net of the opening
        assert!(dm.volume > 2.0 && dm.volume < 3.0 + 1e-9, "{}", dm.volume);
        // the wall is in place
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        m.undo().unwrap();
        assert_eq!(m.element_phase("wall", w).unwrap().demolished, None);
        // demolition before creation is rejected
        m.set_element_phases(&[("wall", w)], Some(new), None)
            .unwrap();
        assert!(m.demolish(&[("wall", w)], ex).is_err());
        m.assert_integrity();
    }

    #[test]
    fn delete_element_cascades_phase_rows_with_undo() {
        let (mut m, lv, t) = model();
        let w = wall(&mut m, lv, t, 0.0);
        let (_, demo, new) = ids(&m);
        // created in "New": demolition in the same phase makes a temporary element
        m.demolish(&[("wall", w)], new).unwrap();
        assert_eq!(
            m.phase_index().unwrap().status("wall", w, new),
            PhaseStatus::Temporary
        );
        m.delete_wall(w).unwrap();
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM elem_phase", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.element_phase("wall", w).unwrap().demolished, Some(new));
        m.set_view_phase(
            ViewKey::Plan(lv),
            ViewPhase {
                phase: demo,
                filter: PhaseFilter::Demolition,
            },
        )
        .unwrap();
        assert_eq!(
            m.view_phase(ViewKey::Plan(lv)).unwrap().filter,
            PhaseFilter::Demolition
        );
        m.undo().unwrap();
        assert_eq!(
            m.view_phase(ViewKey::Plan(lv)).unwrap().filter,
            PhaseFilter::Off
        );
        // a deleted level takes its plan's filter with it
        let l2 = m.add_level("L2", 3.0).unwrap();
        m.set_view_phase(
            ViewKey::Plan(l2),
            ViewPhase {
                phase: demo,
                filter: PhaseFilter::All,
            },
        )
        .unwrap();
        m.delete_level(l2).unwrap();
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM view_phase", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(
            m.view_phase(ViewKey::Plan(l2)).unwrap().filter,
            PhaseFilter::All
        );
    }

    fn door(m: &mut Model, w: i64, along: f64) -> i64 {
        m.add_opening(
            w,
            &OpeningSpec {
                along,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn reused_rowid_is_a_new_element_and_rewrite_keeps_phase() {
        let (mut m, lv, t) = model();
        let (ex, demo, new) = ids(&m);
        m.set_current_phase(ex).unwrap();
        let w = wall(&mut m, lv, t, 0.0);
        let d = door(&mut m, w, 1.0);
        m.demolish(&[("opening", d)], demo).unwrap();
        m.set_current_phase(new).unwrap();
        // one operation: delete the last opening and insert a new one; SQLite hands out the same id
        let d2 = m
            .op("opening_swap", |db| {
                db.execute("DELETE FROM opening WHERE id=?1", [d])?;
                db.execute(
                    "INSERT INTO opening(wall,along,width,sill,height,kind) VALUES (?1,2,0.9,0,2.1,'door')",
                    [w],
                )?;
                Ok((db.last_insert_rowid(), w, Inv::default()))
            })
            .unwrap();
        assert_eq!(d2, d);
        assert_eq!(
            m.element_phase("opening", d2).unwrap(),
            ElemPhase {
                created: new,
                demolished: None
            }
        );
        // overwriting a row (INSERT OR REPLACE) is the same element: the phase is kept
        m.set_element_phases(&[("opening", d2)], Some(demo), None)
            .unwrap();
        m.op("opening_rewrite", |db| {
            db.execute(
                "INSERT OR REPLACE INTO opening(id,wall,along,width,sill,height,kind) \
                 VALUES (?1,?2,2.5,0.9,0,2.1,'door')",
                [d2, w],
            )?;
            Ok(((), w, Inv::default()))
        })
        .unwrap();
        assert_eq!(m.element_phase("opening", d2).unwrap().created, demo);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM ddc_dirty", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the row journal is cleared after the operation");
    }

    #[test]
    fn demolish_host_keeps_earlier_demolished_and_skips_later_inserts() {
        let (mut m, lv, t) = model();
        let (ex, demo, new) = ids(&m);
        m.set_current_phase(ex).unwrap();
        let w = wall(&mut m, lv, t, 0.0);
        let d1 = door(&mut m, w, 0.5);
        m.demolish(&[("opening", d1)], ex).unwrap();
        m.set_current_phase(new).unwrap();
        let d2 = door(&mut m, w, 3.0);
        let gone = m.demolish(&[("wall", w)], demo).unwrap();
        assert_eq!(gone, vec![("wall", w)]);
        assert_eq!(m.element_phase("opening", d1).unwrap().demolished, Some(ex));
        assert_eq!(m.element_phase("opening", d2).unwrap().demolished, None);
        m.undo().unwrap();
        assert_eq!(m.element_phase("wall", w).unwrap().demolished, None);
    }

    #[test]
    fn migration_22_is_additive_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("ddc_mig22_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.ddc");
        let p = p.to_str().unwrap();
        let w = {
            let mut m = Model::open(p).unwrap();
            let lv = m.add_level("L1", 0.0).unwrap();
            let t = m
                .add_wall_type(
                    "w",
                    &[LayerSpec {
                        material: "b".into(),
                        thickness: 0.2,
                    }],
                )
                .unwrap();
            wall(&mut m, lv, t, 0.0)
        };
        {
            // a pre-22 file: without the phase and parameter tables
            let c = rusqlite::Connection::open(p).unwrap();
            c.execute_batch(
                "DROP TABLE phase; DROP TABLE elem_phase; DROP TABLE view_phase; DROP TABLE pparam;
                 DROP TABLE pparam_cat; DROP TABLE pparam_val; DELETE FROM meta WHERE key='phase_current';
                 UPDATE meta SET value=17 WHERE key='schema_version';",
            )
            .unwrap();
        }
        for _ in 0..2 {
            let m = Model::open(p).unwrap();
            assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
            let ps = m.phases().unwrap();
            assert_eq!(ps.len(), 3, "seeded once");
            // old elements are in the first phase
            assert_eq!(m.element_phase("wall", w).unwrap().created, ps[0].id);
            m.assert_integrity();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn phase_list_add_move_delete() {
        let (mut m, lv, t) = model();
        let (ex, demo, new) = ids(&m);
        let mid = m.add_phase("Stage 2", Some(demo)).unwrap();
        let names: Vec<String> = m.phases().unwrap().into_iter().map(|p| p.name).collect();
        let [p_ex, p_demo, p_new] = DEFAULT_PHASES.map(|d| d.1);
        assert_eq!(names, [p_ex, p_demo, "Stage 2", p_new]);
        assert!(m.add_phase(p_new, None).is_err());
        m.undo().unwrap();
        assert_eq!(m.phases().unwrap().len(), 3);
        let _ = mid;
        // elements without a row are in the first phase; deleting the first one without a move is rejected
        m.set_current_phase(ex).unwrap();
        let w = wall(&mut m, lv, t, 0.0);
        assert!(matches!(
            m.delete_phase(ex, None),
            Err(Error::Referenced { .. })
        ));
        m.delete_phase(ex, Some(new)).unwrap();
        assert_eq!(m.element_phase("wall", w).unwrap().created, new);
        assert_eq!(m.current_phase().unwrap(), new);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.element_phase("wall", w).unwrap().created, ex);
        assert_eq!(m.current_phase().unwrap(), ex);
        // a reorder that breaks the "created <= demolished" order is rejected
        m.demolish(&[("wall", w)], demo).unwrap();
        assert!(m.move_phase(demo, 0).is_err());
        m.move_phase(new, 1).unwrap();
        let names: Vec<String> = m.phases().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            [DEFAULT_PHASES[0].1, DEFAULT_PHASES[2].1, DEFAULT_PHASES[1].1]
        );
        m.assert_integrity();
        m.undo().unwrap();
        m.rename_phase(new, "Project").unwrap();
        assert_eq!(m.phase_index().unwrap().name(new), "Project");
    }

    fn seed_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_mig2_ph_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn upgrade_writes(p: &str) -> u64 {
        let mut c = Connection::open(p).unwrap();
        crate::schema::upgrade(&mut c, Some(p)).unwrap();
        c.total_changes()
    }

    /// The editor never removes the last phase; a file whose phase list was emptied outside it
    /// keeps that state on open: no reseed, no write, nothing to undo.
    #[test]
    fn an_emptied_phase_list_is_not_reseeded_on_open() {
        let d = seed_dir("empty");
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        {
            let mut m = Model::open(ps).unwrap();
            let ps_ = m.phases().unwrap();
            for ph in &ps_[1..] {
                m.delete_phase(ph.id, None).unwrap();
            }
            assert!(m.delete_phase(ps_[0].id, None).is_err(), "last phase is refused");
            assert_eq!(m.phases().unwrap().len(), 1);
            // emptied by an outside tool, its journal with it
            m.db.execute_batch("DELETE FROM phase; DELETE FROM op;")
                .unwrap();
        }
        assert_eq!(upgrade_writes(ps), 0, "reopen writes nothing");
        let mut m = Model::open(ps).unwrap();
        assert!(m.phases().unwrap().is_empty(), "no reseed");
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
        drop(m);
        let _ = std::fs::remove_dir_all(d);
    }

    /// Default names stored by earlier versions (also without a key) read as the standard
    /// phases; the file is not rewritten and a renamed phase keeps its name.
    #[test]
    fn stored_old_default_names_read_as_standard_phases() {
        let (mut m, _, _) = model();
        let (ex, demo, new) = ids(&m);
        for (id, (_, old)) in [ex, demo, new].into_iter().zip(LEGACY_PHASE_NAMES) {
            m.db.execute("UPDATE phase SET name=?2 WHERE id=?1", params![id, old]).unwrap();
        }
        m.db.execute("UPDATE phase SET key=NULL WHERE id=?1", [demo]).unwrap();
        let ps = m.phases().unwrap();
        let got: Vec<(&str, Option<&str>)> =
            ps.iter().map(|p| (p.name.as_str(), p.key.as_deref())).collect();
        let want: Vec<(&str, Option<&str>)> =
            DEFAULT_PHASES.iter().map(|(k, n)| (*n, Some(*k))).collect();
        assert_eq!(got, want);
        assert_eq!(find_phase(&ps, LEGACY_PHASE_NAMES[2].1), Some(new));
        assert!(m.rename_phase(ex, DEFAULT_PHASES[2].1).is_err(), "taken by the standard phase");
        m.rename_phase(ex, "Survey").unwrap();
        assert_eq!(m.phases().unwrap()[0].name, "Survey");
    }

    /// An older file without the phase table gets the default phases once, by the upgrade.
    #[test]
    fn an_old_file_without_phases_is_seeded_once() {
        let d = seed_dir("old");
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        drop(Model::open(ps).unwrap());
        {
            let c = Connection::open(ps).unwrap();
            c.execute_batch("DROP TABLE phase; DELETE FROM op;").unwrap();
            c.execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [crate::schema::VERSION - 1],
            )
            .unwrap();
        }
        let mut m = Model::open(ps).unwrap();
        let seeded = m.phases().unwrap();
        assert_eq!(seeded.len(), DEFAULT_PHASES.len());
        assert_eq!(m.current_phase().unwrap(), seeded.last().unwrap().id);
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
        drop(m);
        assert_eq!(upgrade_writes(ps), 0, "seeded once");
        let m = Model::open(ps).unwrap();
        assert_eq!(m.phases().unwrap().len(), seeded.len());
        drop(m);
        let _ = std::fs::remove_dir_all(d);
    }
}
