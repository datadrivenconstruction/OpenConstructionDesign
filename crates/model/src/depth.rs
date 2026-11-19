// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Design depth (schema 51): how far each element is worked out, D1 concept .. D5 fabrication
//! (LOD 100/200/300/350/400), and the discipline that owns it. One row per element in
//! `elem_depth`, keyed like a parameter value: (table of the row, row id). No row: the element
//! was made before depth existed or by hand; it reads as "not set", never as a guess.
//! The project setting lives in `depth_cfg`: the overall depth plus one entry per discipline
//! (absent: on and following the overall depth; 1..5: on with its own depth; -d or 0: off).
//! The pre-run estimate turns element counts (the generator's task graph or the model's own
//! rows) into elements, stages, model calls, time and tokens; no model is called for it.

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

pub const TABLE: &str = "elem_depth";
pub const CFG_TABLE: &str = "depth_cfg";
const DDL: &str = "CREATE TABLE IF NOT EXISTS elem_depth(tbl TEXT NOT NULL, id INTEGER NOT NULL, \
     depth INTEGER NOT NULL, discipline TEXT NOT NULL, PRIMARY KEY(tbl, id)); \
     CREATE TABLE IF NOT EXISTS depth_cfg(key TEXT PRIMARY KEY, value INTEGER NOT NULL);";
const PUT: &str =
    "INSERT OR REPLACE INTO elem_depth(tbl, id, depth, discipline) VALUES (?1,?2,?3,?4)";
const DROP: &str = "DELETE FROM elem_depth WHERE tbl=?1 AND id=?2";
const CFG_PUT: &str = "INSERT OR REPLACE INTO depth_cfg(key, value) VALUES (?1,?2)";
const CFG_DROP: &str = "DELETE FROM depth_cfg WHERE key=?1";
const PROJECT_KEY: &str = "project";

/// IFC property set and its properties written by the export and read back by the import.
pub const PSET: &str = "DDC_Depth";
pub const PROP_DEPTH: &str = "Depth";
pub const PROP_DISCIPLINE: &str = "Discipline";

pub const MIN: u8 = 1;
pub const MAX: u8 = 5;
/// Depth of a new project: what the project generator writes (walls, rooms, openings, no layers).
pub const DEFAULT: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Discipline {
    Arch,
    Struct,
    Mep,
    Doc,
}

impl Discipline {
    pub const ALL: [Discipline; 4] = [
        Discipline::Arch,
        Discipline::Struct,
        Discipline::Mep,
        Discipline::Doc,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Discipline::Arch => "arch",
            Discipline::Struct => "struct",
            Discipline::Mep => "mep",
            Discipline::Doc => "doc",
        }
    }

    /// A code this build does not know (a newer file) reads as `None`, never fails.
    pub fn parse(s: &str) -> Option<Discipline> {
        Discipline::ALL.into_iter().find(|d| d.as_str() == s.trim())
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Owner of the rows of `table`; `None`: a table that carries no depth.
    pub fn of_table(table: &str) -> Option<Discipline> {
        TABLES.iter().find(|t| t.0 == table).map(|t| t.1)
    }
}

/// Element tables that carry a depth, with the discipline that owns them by default.
pub const TABLES: &[(&str, Discipline)] = &[
    ("wall", Discipline::Arch),
    ("opening", Discipline::Arch),
    ("curtain_wall", Discipline::Arch),
    ("room", Discipline::Arch),
    ("roof", Discipline::Arch),
    ("ceiling", Discipline::Arch),
    ("stair", Discipline::Arch),
    ("railing", Discipline::Arch),
    ("ramp", Discipline::Arch),
    ("comp", Discipline::Arch),
    ("slab", Discipline::Struct),
    ("col", Discipline::Struct),
    ("beam", Discipline::Struct),
    ("iso_footing", Discipline::Struct),
    ("strip_footing", Discipline::Struct),
    ("raft", Discipline::Struct),
    ("brace", Discipline::Struct),
    ("truss", Discipline::Struct),
    ("grid", Discipline::Struct),
    ("rebar_set", Discipline::Struct),
    ("shaft", Discipline::Mep),
    ("mep_run", Discipline::Mep),
    ("mep_system", Discipline::Mep),
    ("sheet", Discipline::Doc),
    ("view_def", Discipline::Doc),
    ("dim", Discipline::Doc),
    ("anno", Discipline::Doc),
];

/// Depth and owner of one element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElemDepth {
    pub depth: u8,
    pub discipline: Discipline,
}

/// One discipline of the project setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscSetting {
    pub on: bool,
    /// Its own depth; `None` follows the project depth.
    pub own: Option<u8>,
}

impl Default for DiscSetting {
    fn default() -> Self {
        DiscSetting { on: true, own: None }
    }
}

/// The project depth and its four disciplines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthSettings {
    pub project: u8,
    pub disc: [DiscSetting; 4],
}

impl Default for DepthSettings {
    fn default() -> Self {
        DepthSettings {
            project: DEFAULT,
            disc: [DiscSetting::default(); 4],
        }
    }
}

impl DepthSettings {
    /// Depth a run gives discipline `d`; `None`: switched off.
    pub fn of(&self, d: Discipline) -> Option<u8> {
        let s = self.disc[d.index()];
        s.on.then(|| s.own.unwrap_or(self.project))
    }

    /// Deepest enabled discipline; `None` when every one is off.
    pub fn deepest(&self) -> Option<u8> {
        Discipline::ALL.into_iter().filter_map(|d| self.of(d)).max()
    }

    /// The same setting with every depth clamped to D1..D5.
    pub fn clamped(mut self) -> Self {
        self.project = clamp(self.project);
        for s in &mut self.disc {
            s.own = s.own.map(clamp);
        }
        self
    }

    /// Stored value of discipline `d`; `None`: no row (on, follows the project).
    fn stored(&self, d: Discipline) -> Option<i64> {
        let s = self.disc[d.index()];
        match (s.on, s.own) {
            (true, None) => None,
            (true, Some(x)) => Some(i64::from(x)),
            (false, None) => Some(0),
            (false, Some(x)) => Some(-i64::from(x)),
        }
    }

    fn read_stored(&mut self, d: Discipline, v: i64) {
        let own = u8::try_from(v.unsigned_abs())
            .ok()
            .filter(|x| (MIN..=MAX).contains(x));
        self.disc[d.index()] = DiscSetting { on: v > 0, own };
    }
}

pub fn clamp(d: u8) -> u8 {
    d.clamp(MIN, MAX)
}

/// The LOD figure of a depth, for labels and the IFC export.
pub fn lod(d: u8) -> u16 {
    match clamp(d) {
        1 => 100,
        2 => 200,
        3 => 300,
        4 => 350,
        _ => 400,
    }
}

/// Words of a request that name a depth, in every language the parser reads.
#[derive(serde::Deserialize)]
struct DepthWords {
    d_letters: Vec<char>,
    stages: Vec<(String, u8)>,
}

fn depth_words() -> &'static DepthWords {
    static W: std::sync::OnceLock<DepthWords> = std::sync::OnceLock::new();
    W.get_or_init(|| serde_json::from_str(include_str!("../data/depth_words.json")).expect("depth_words.json"))
}

/// The depth a request asks for: "D4", "LOD 300", "concept", a Russian stage word, ... The
/// highest one named wins ("from D2 to D4" asks for D4). `None`: the text names no depth.
pub fn parse_words(text: &str) -> Option<u8> {
    let t = text.to_lowercase();
    let chars: Vec<char> = t.chars().collect();
    let mut best: Option<u8> = None;
    let mut take = |d: u8| best = Some(best.map_or(d, |b| b.max(d)));
    let alnum = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    for i in 0..chars.len() {
        let c = chars[i];
        if depth_words().d_letters.contains(&c) && !alnum(i.checked_sub(1).and_then(|j| chars.get(j))) {
            if let Some(n) = chars.get(i + 1).and_then(|c| c.to_digit(10)) {
                if (1..=5).contains(&n) && !alnum(chars.get(i + 2)) {
                    take(n as u8);
                }
            }
        }
    }
    let flat: String = t.split_whitespace().collect::<Vec<_>>().join("");
    for (lod_s, d) in [("lod100", 1), ("lod200", 2), ("lod300", 3), ("lod350", 4), ("lod400", 5)] {
        if flat.contains(lod_s) {
            take(d);
        }
    }
    // The Russian word for "workplaces" in a brief is no depth: only the documentation stage counts
    for (w, d) in &depth_words().stages {
        if t.contains(w.as_str()) {
            take(*d);
        }
    }
    best
}

/// Step 51: additive; opening a file that has the tables writes nothing.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

fn table_exists(db: &Connection, name: &str) -> Result<bool, Error> {
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row([name], |r| r.get(0))?;
    Ok(n > 0)
}

fn old_row(db: &Connection, tbl: &str, id: i64) -> Result<Option<(i64, String)>, Error> {
    Ok(db
        .prepare_cached("SELECT depth, discipline FROM elem_depth WHERE tbl=?1 AND id=?2")?
        .query_row(params![tbl, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

/// Writes one element's depth and returns the inverse that puts the old state back.
fn put(db: &Connection, tbl: &str, id: i64, e: ElemDepth, inv: Inv) -> Result<Inv, Error> {
    let inv = match old_row(db, tbl, id)? {
        Some((d, disc)) => inv.sql(
            PUT,
            vec![
                Value::Text(tbl.into()),
                Value::Integer(id),
                Value::Integer(d),
                Value::Text(disc),
            ],
        ),
        None => inv.sql(DROP, vec![Value::Text(tbl.into()), Value::Integer(id)]),
    };
    db.prepare_cached(PUT)?.execute(params![
        tbl,
        id,
        i64::from(e.depth),
        e.discipline.as_str()
    ])?;
    Ok(inv)
}

/// Writes depth `d` (and discipline) found in an imported file onto a row the import just made.
/// The import batch's watermark undoes it with the rest of the import.
pub fn import_prop(
    db: &Connection,
    snaps: &mut crate::Snapshots,
    tbl: &str,
    id: i64,
    depth: Option<i64>,
    discipline: Option<&str>,
) -> Result<bool, Error> {
    let Some(default_disc) = Discipline::of_table(tbl) else {
        return Ok(false);
    };
    migrate(db)?;
    snaps.watermark(db, TABLE)?;
    // a re-import replaces the row: undo brings the earlier one back
    snaps.save(db, TABLE, "id", id)?;
    let old = old_row(db, tbl, id)?;
    let depth = match depth {
        Some(d) => u8::try_from(d).ok().filter(|d| (MIN..=MAX).contains(d)),
        None => old.as_ref().and_then(|o| u8::try_from(o.0).ok()),
    };
    let Some(depth) = depth else {
        return Ok(false);
    };
    let discipline = discipline
        .and_then(Discipline::parse)
        .or_else(|| old.as_ref().and_then(|o| Discipline::parse(&o.1)))
        .unwrap_or(default_disc);
    db.prepare_cached(PUT)?.execute(params![
        tbl,
        id,
        i64::from(depth),
        discipline.as_str()
    ])?;
    Ok(true)
}

/// Ids of `tbl` that still exist, of the ones given.
fn live_ids(db: &Connection, tbl: &str) -> Result<std::collections::HashSet<i64>, Error> {
    if !table_exists(db, tbl)? {
        return Ok(Default::default());
    }
    let mut st = db.prepare(&format!("SELECT id FROM \"{tbl}\""))?;
    let ids = st
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

/// Elements counted by the estimate's keys (the generator's gate counts use the same names).
pub(crate) fn count_rows(db: &Connection) -> Result<Vec<(&'static str, usize)>, Error> {
    const Q: [(&str, &str, &str); 14] = [
        ("levels", "level", ""),
        ("walls", "wall", ""),
        ("doors", "opening", " WHERE kind='door'"),
        ("windows", "opening", " WHERE kind='window'"),
        ("rooms", "room", ""),
        ("slabs", "slab", ""),
        ("stairs", "stair", ""),
        ("shafts", "shaft", ""),
        ("roofs", "roof", ""),
        ("grids", "grid", ""),
        ("cols", "col", ""),
        ("beams", "beam", ""),
        ("mep_runs", "mep_run", ""),
        ("sheets", "sheet", ""),
    ];
    let mut out = vec![];
    for (key, table, filter) in Q {
        let n: i64 = if table_exists(db, table)? {
            db.query_row(&format!("SELECT count(*) FROM \"{table}\"{filter}"), [], |r| {
                r.get(0)
            })?
        } else {
            0
        };
        out.push((key, n.max(0) as usize));
    }
    Ok(out)
}

impl Model {
    /// The project depth setting; a file without one reads as the default (D2, all on).
    pub fn depth_settings(&self) -> Result<DepthSettings, Error> {
        let mut s = DepthSettings::default();
        if !table_exists(&self.db, CFG_TABLE)? {
            return Ok(s);
        }
        let mut st = self.db.prepare("SELECT key, value FROM depth_cfg")?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        for (k, v) in rows {
            if k == PROJECT_KEY {
                if let Ok(d) = u8::try_from(v) {
                    s.project = clamp(d);
                }
            } else if let Some(d) = Discipline::parse(&k) {
                s.read_stored(d, v);
            }
        }
        Ok(s)
    }

    /// Stores the project setting as one undo step; an unchanged setting writes nothing.
    pub fn set_depth_settings(&mut self, s: &DepthSettings) -> Result<bool, Error> {
        let s = s.clamped();
        let old = self.depth_settings()?;
        if old == s {
            return Ok(false);
        }
        self.op("depth_settings", |db| {
            migrate(db)?;
            let mut inv = Inv::default();
            let mut write = |key: &str, new: Option<i64>| -> Result<(), Error> {
                let was: Option<i64> = db
                    .prepare_cached("SELECT value FROM depth_cfg WHERE key=?1")?
                    .query_row([key], |r| r.get(0))
                    .optional()?;
                if was == new {
                    return Ok(());
                }
                let back = match was {
                    Some(v) => Inv::default()
                        .sql(CFG_PUT, vec![Value::Text(key.into()), Value::Integer(v)]),
                    None => Inv::default().sql(CFG_DROP, vec![Value::Text(key.into())]),
                };
                inv = std::mem::take(&mut inv).then(back);
                match new {
                    Some(v) => db.prepare_cached(CFG_PUT)?.execute(params![key, v])?,
                    None => db.prepare_cached(CFG_DROP)?.execute([key])?,
                };
                Ok(())
            };
            write(PROJECT_KEY, Some(i64::from(s.project)))?;
            for d in Discipline::ALL {
                write(d.as_str(), s.stored(d))?;
            }
            Ok((true, 0, inv))
        })
    }

    /// Depth of one element; `None`: not set.
    pub fn elem_depth(&self, tbl: &str, id: i64) -> Result<Option<ElemDepth>, Error> {
        if !table_exists(&self.db, TABLE)? {
            return Ok(None);
        }
        Ok(old_row(&self.db, tbl, id)?.and_then(|(d, disc)| {
            Some(ElemDepth {
                depth: u8::try_from(d).ok()?,
                discipline: Discipline::parse(&disc)?,
            })
        }))
    }

    /// Sets depth `depth` on each (table, id); the discipline is `discipline` or the table's
    /// owner. One undo step. Refused for a table without depth or a row that does not exist.
    pub fn set_elem_depth(
        &mut self,
        cells: &[(&str, i64)],
        depth: u8,
        discipline: Option<Discipline>,
    ) -> Result<usize, Error> {
        if !(MIN..=MAX).contains(&depth) {
            return Err(Error::Invalid(format!("depth {depth} is outside D1..D5")));
        }
        let mut todo: Vec<(&str, i64, ElemDepth)> = vec![];
        let mut seen = std::collections::HashSet::new();
        for (t, id) in cells {
            let Some(owner) = Discipline::of_table(t) else {
                return Err(Error::Invalid(format!("'{t}' elements carry no depth")));
            };
            if !seen.insert((*t, *id)) {
                continue;
            }
            let there: Option<i64> = if table_exists(&self.db, t)? {
                self.db
                    .query_row(&format!("SELECT 1 FROM \"{t}\" WHERE id=?1"), [id], |r| {
                        r.get(0)
                    })
                    .optional()?
            } else {
                None
            };
            if there.is_none() {
                return Err(Error::Invalid(format!("no element {t} {id}")));
            }
            let e = ElemDepth {
                depth,
                discipline: discipline.unwrap_or(owner),
            };
            todo.push((t, *id, e));
        }
        if todo.is_empty() {
            return Ok(0);
        }
        self.op("elem_depth", |db| {
            migrate(db)?;
            let mut inv = Inv::default();
            for (t, id, e) in &todo {
                inv = put(db, t, *id, *e, Inv::default())?.then(inv);
            }
            Ok((todo.len(), 0, inv))
        })
    }

    /// Every live element with a depth, in (table, id) order; rows of deleted elements are skipped.
    pub fn elem_depths(&self) -> Result<Vec<((String, i64), ElemDepth)>, Error> {
        if !table_exists(&self.db, TABLE)? {
            return Ok(vec![]);
        }
        let mut st = self
            .db
            .prepare("SELECT tbl, id, depth, discipline FROM elem_depth ORDER BY tbl, id")?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut live: std::collections::HashMap<String, std::collections::HashSet<i64>> =
            Default::default();
        let mut out = vec![];
        for (t, id, d, disc) in rows {
            if Discipline::of_table(&t).is_none() {
                continue;
            }
            if !live.contains_key(&t) {
                let ids = live_ids(&self.db, &t)?;
                live.insert(t.clone(), ids);
            }
            if !live[&t].contains(&id) {
                continue;
            }
            let (Ok(depth), Some(discipline)) = (u8::try_from(d), Discipline::parse(&disc)) else {
                continue;
            };
            out.push(((t, id), ElemDepth { depth, discipline }));
        }
        Ok(out)
    }

    /// Elements at depth `min` or deeper, optionally of one discipline: the view filter
    /// "depth >= N" and the heat map read this.
    pub fn elems_at_depth(
        &self,
        min: u8,
        discipline: Option<Discipline>,
    ) -> Result<Vec<(String, i64)>, Error> {
        Ok(self
            .elem_depths()?
            .into_iter()
            .filter(|(_, e)| e.depth >= min && discipline.is_none_or(|d| e.discipline == d))
            .map(|(k, _)| k)
            .collect())
    }

    /// Counts the estimate starts from when there is no generator spec: the model's own rows.
    pub fn depth_base_counts(&self) -> Result<Vec<(&'static str, usize)>, Error> {
        count_rows(&self.db)
    }
}

// ---- pre-run estimate ----

/// Elements of a discipline at D1..D5 against D2 (what the generator writes = 1.0). Initial
/// calibration from demo runs; measured ratios from the eval ledger should replace it.
const K: [[f64; 5]; 4] = [
    // arch: D3 layered types, fixtures, ceilings, finishes; D4 hardware, sills, nosing, furniture
    [0.3, 1.0, 1.9, 3.2, 4.5],
    // struct: D3 sized members and footings; D4 reinforcement and embeds
    [0.4, 1.0, 1.6, 4.0, 6.0],
    // mep: D2 zones and shafts only; D3 mains and terminals; D4 branches, hangers, valves
    [0.3, 1.0, 6.0, 14.0, 18.0],
    // doc: sheets, views, schedules, tags; D4 callouts and notes
    [0.3, 1.0, 2.5, 5.0, 6.0],
];
/// Sheets against the D2 set (plans per storey, a section, elevations, schedule): D1 one
/// sheet, D3 adds sections and schedules by discipline, D4 detail sheets.
const SHEETS: [f64; 5] = [0.2, 1.0, 1.6, 2.4, 2.6];
/// Operations per staged changeset.
pub const BATCH_OPS: u64 = 500;
/// Generator time per element and a draft read per stage.
pub const MS_PER_ELEMENT: f64 = 4.0;
pub const READ_S_PER_STAGE: f64 = 1.0;
/// One model call: answer latency, and prompt + tools + context + answer tokens.
pub const CALL_S: f64 = 6.0;
pub const TOKENS_PER_CALL: u64 = 6_800;
/// Share of stages that need a second call to fix what the stage check found.
pub const FIX_SHARE: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    /// Elements at the target depth, per discipline (`Discipline::index`) and in total.
    pub by_disc: [u64; 4],
    pub elements: u64,
    /// Elements beyond the D2 draft the counts describe.
    pub added: u64,
    pub stages: u32,
    pub calls: u32,
    pub seconds: f64,
    pub tokens: u64,
    pub sheets: u32,
}

fn get(counts: &[(&str, usize)], k: &str) -> f64 {
    counts.iter().find(|c| c.0 == k).map_or(0.0, |c| c.1 as f64)
}

/// D2-equivalent element count of each discipline from task graph sizes.
pub fn base_counts(counts: &[(&str, usize)]) -> [f64; 4] {
    let storeys = (get(counts, "levels") - 1.0).max(get(counts, "slabs")).max(1.0);
    let arch = ["walls", "doors", "windows", "rooms", "stairs", "roofs"]
        .iter()
        .map(|k| get(counts, k))
        .sum::<f64>();
    let grids = get(counts, "grids");
    // no columns yet: one per grid crossing on every storey, the lines split between x and y
    let cols = match get(counts, "cols") {
        c if c > 0.0 => c,
        _ => (grids / 2.0).floor() * (grids - (grids / 2.0).floor()) * storeys,
    };
    let structure = get(counts, "slabs") + grids + cols + get(counts, "beams");
    // three systems (heating/vent, water, power) as zones per storey, plus shafts and runs
    let mep = get(counts, "shafts") + get(counts, "mep_runs") + 3.0 * storeys;
    // plans per storey, a section, elevations and the room schedule
    let doc = get(counts, "sheets").max(storeys + 3.0);
    [arch, structure, mep, doc]
}

/// Elements, stages, calls, time and tokens of a run to setting `s` from the counts of a D2
/// draft (`ddc_gen` gate counts or [`Model::depth_base_counts`]).
pub fn estimate(counts: &[(&str, usize)], s: &DepthSettings) -> Estimate {
    let base = base_counts(counts);
    let s = s.clamped();
    let mut by_disc = [0u64; 4];
    let (mut stages, mut batches, mut added) = (0u32, 0u64, 0u64);
    for d in Discipline::ALL {
        let Some(depth) = s.of(d) else { continue };
        let k = &K[d.index()];
        let at = |lvl: u8| (base[d.index()] * k[usize::from(lvl) - 1] / k[1]).round() as u64;
        by_disc[d.index()] = at(depth);
        added += at(depth).saturating_sub(at(2));
        for lvl in 1..=depth {
            stages += 1;
            let made = if lvl == 1 { at(1) } else { at(lvl).saturating_sub(at(lvl - 1)) };
            batches += made.div_ceil(BATCH_OPS).max(1);
        }
    }
    let elements: u64 = by_disc.iter().sum();
    let fixes = (f64::from(stages) * FIX_SHARE).ceil() as u32;
    // the planner, one call per stage batch the model looks at, and the fix rounds
    let calls = if stages == 0 { 0 } else { 1 + batches as u32 + fixes };
    let seconds = elements as f64 * MS_PER_ELEMENT / 1000.0
        + f64::from(stages) * READ_S_PER_STAGE
        + f64::from(calls) * CALL_S;
    let sheets = s.of(Discipline::Doc).map_or(0, |d| {
        ((base[Discipline::Doc.index()] * SHEETS[usize::from(d) - 1]).round() as u32).max(1)
    });
    Estimate {
        by_disc,
        elements,
        added,
        stages,
        calls,
        seconds,
        tokens: u64::from(calls) * TOKENS_PER_CALL,
        sheets,
    }
}

#[cfg(test)]
#[path = "depth_tests.rs"]
mod tests;
