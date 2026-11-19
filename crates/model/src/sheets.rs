//! Sheets (WORKFLOWS W3): a sheet has a number, name, format, orientation, title-block profile, its fields,
//! revisions; a viewport is a level plan or section/elevation (`view_def`) at scale 1:N, centred on the
//! sheet (mm), with a crop in view coordinates (m); schedules on a sheet; sheet sets; ordering and
//! numbering. All are journal operations; deleting a view or level removes its viewports (undo restores them).

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::integrity::Issue;
use crate::ops::Inv;
use crate::{Error, Model};

mod dset;
pub(crate) mod output;
#[cfg(test)]
mod tests;

pub use dset::{
    DsetApplied, DsetPlan, DsetRef, DsetSchedule, DsetSheet, DsetViewport, DSET_FIELD,
    DSET_LEVELS, DSET_SET, MarkPrefix,
};
pub use output::{ScheduleSplit, ViewPlacement, WallTypeLegend, DEFAULT_COLUMN_GAP};

/// Sheet tables; created on open if missing (independent of the migration order of other migrations).
pub(crate) const DDL: [(&str, &str); 6] = [
    (
        "sheet",
        "CREATE TABLE IF NOT EXISTS sheet(id INTEGER PRIMARY KEY, number TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL, paper TEXT NOT NULL, landscape INTEGER NOT NULL, standard TEXT NOT NULL,
            fields TEXT NOT NULL, seq INTEGER NOT NULL);",
    ),
    (
        "sheet_rev",
        "CREATE TABLE IF NOT EXISTS sheet_rev(sheet INTEGER NOT NULL, idx INTEGER NOT NULL,
            mark TEXT NOT NULL, date TEXT NOT NULL, description TEXT NOT NULL, author TEXT NOT NULL,
            PRIMARY KEY(sheet, idx));",
    ),
    (
        "viewport",
        "CREATE TABLE IF NOT EXISTS viewport(id INTEGER PRIMARY KEY, sheet INTEGER NOT NULL,
            level INTEGER, view_def INTEGER, cut REAL, scale REAL NOT NULL, x REAL NOT NULL,
            y REAL NOT NULL, cx0 REAL, cy0 REAL, cx1 REAL, cy1 REAL, title TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS viewport_sheet ON viewport(sheet);
         CREATE INDEX IF NOT EXISTS viewport_level ON viewport(level);
         CREATE INDEX IF NOT EXISTS viewport_view_def ON viewport(view_def);",
    ),
    (
        "sheet_schedule",
        "CREATE TABLE IF NOT EXISTS sheet_schedule(id INTEGER PRIMARY KEY, sheet INTEGER NOT NULL,
            schedule TEXT NOT NULL, x REAL NOT NULL, y REAL NOT NULL);",
    ),
    (
        "sheet_set",
        "CREATE TABLE IF NOT EXISTS sheet_set(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);",
    ),
    (
        "sheet_set_item",
        "CREATE TABLE IF NOT EXISTS sheet_set_item(sheet_set INTEGER NOT NULL, idx INTEGER NOT NULL,
            sheet INTEGER NOT NULL, PRIMARY KEY(sheet_set, idx));",
    ),
];

/// Sheet table references (child, column, parent); they extend `integrity::REFS`.
pub(crate) const REFS: [(&str, &str, &str); 10] = [
    ("viewport", "sheet", "sheet"),
    ("viewport", "level", "level"),
    ("viewport", "view_def", "view_def"),
    ("sheet_rev", "sheet", "sheet"),
    ("sheet_schedule", "sheet", "sheet"),
    ("sheet_set_item", "sheet", "sheet"),
    ("sheet_set_item", "sheet_set", "sheet_set"),
    ("anno_legend_place", "sheet", "sheet"),
    ("anno_legend_place", "legend", "anno_legend"),
    ("anno_legend_row", "legend", "anno_legend"),
];

/// Sheet parts and view viewports: deleted with the parent, they do not block deletion.
pub(crate) const OWNED: [&str; 5] = [
    "viewport",
    "sheet_rev",
    "sheet_schedule",
    "sheet_set_item",
    "anno_legend_place",
];

/// Paper formats (sizes are in the ddc-io sheet data).
pub const PAPERS: [&str; 10] = [
    "A0", "A1", "A2", "A3", "A4", "ANSI A", "ANSI B", "ANSI C", "ANSI D", "ANSI E",
];
/// Title-block profiles.
pub const STANDARDS: [&str; 2] = ["iso", "gost"];
/// Selectable scales (denominators of 1:N).
pub const SCALES: [f64; 14] = [
    1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 250.0, 500.0, 1000.0,
];
/// Default plan cut height on a sheet, m.
pub const DEFAULT_CUT: f64 = 1.2;

/// A sheet: `fields` are the title-block fields (profile keys: title, designation, developer, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct SheetDef {
    pub number: String,
    pub name: String,
    pub paper: String,
    pub landscape: bool,
    pub standard: String,
    pub fields: BTreeMap<String, String>,
}

impl SheetDef {
    /// A3 landscape, ISO profile, empty fields.
    pub fn new(number: &str, name: &str) -> Self {
        Self {
            number: number.into(),
            name: name.into(),
            paper: "A3".into(),
            landscape: true,
            standard: "iso".into(),
            fields: BTreeMap::new(),
        }
    }
}

/// A sheet revision (a row of the revision table).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Revision {
    pub mark: String,
    pub date: String,
    pub description: String,
    pub author: String,
}

/// What a viewport shows: a level plan at a cut height or a section/elevation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewRef {
    Plan { level: i64, cut: f64 },
    View(i64),
}

/// A viewport: `center` is the centre on the sheet, mm from the lower-left corner; `crop` is the
/// crop [x0, y0, x1, y1] in view coordinates, m (None means the whole view).
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportDef {
    pub sheet: i64,
    pub view: ViewRef,
    pub scale: f64,
    pub center: (f64, f64),
    pub crop: Option<[f64; 4]>,
    pub title: String,
}

/// A schedule on a sheet: `schedule` is the schedule key (walls, doors, ...), `pos` is the top-left, mm.
#[derive(Clone, Debug, PartialEq)]
pub struct SchedulePlace {
    pub sheet: i64,
    pub schedule: String,
    pub pos: (f64, f64),
}

/// Next number: the last digit group +1, keeping the width ("A-109" -> "A-110").
pub fn increment_number(s: &str) -> String {
    let b = s.as_bytes();
    let end = b.len() - b.iter().rev().take_while(|c| !c.is_ascii_digit()).count();
    let start = b[..end]
        .iter()
        .rposition(|c| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    if start == end {
        return format!("{s}1");
    }
    // Decimal carry on the string: the digit length is unbounded (no u64 overflow).
    let mut d = s[start..end].as_bytes().to_vec();
    let mut i = d.len();
    loop {
        if i == 0 {
            d.insert(0, b'1');
            break;
        }
        i -= 1;
        if d[i] == b'9' {
            d[i] = b'0';
        } else {
            d[i] += 1;
            break;
        }
    }
    format!(
        "{}{}{}",
        &s[..start],
        String::from_utf8(d).unwrap_or_default(),
        &s[end..]
    )
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn no_such<T>(what: &'static str, _id: i64) -> Result<T, Error> {
    Err(SheetErr::NoSuch(what).into())
}

/// Why a sheet edit was refused, with the data a localized message needs. No internal row ids:
/// the text names what the user sees (number, name), never database keys.
#[derive(Debug, Clone, PartialEq)]
pub enum SheetErr {
    NumberEmpty,
    /// Another sheet already has this number.
    NumberTaken { number: String, name: String },
    UnknownPaper(String),
    UnknownStandard(String),
    /// The row is gone; `what` is one of sheet, sheet set, viewport, schedule placement, level, view.
    NoSuch(&'static str),
    BadScale(f64),
    BadCut,
    BadPosition,
    BadCrop,
    NoSingleView,
}

/// Kind of refusal: bad input, or a clash with existing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetKind {
    Invalid,
    Conflict,
}

impl SheetErr {
    pub fn kind(&self) -> SheetKind {
        match self {
            SheetErr::NumberTaken { .. } => SheetKind::Conflict,
            _ => SheetKind::Invalid,
        }
    }

    /// Catalog key of the user message (`sheets.err.*`).
    pub fn key(&self) -> &'static str {
        match self {
            SheetErr::NumberEmpty => "sheets.err.no_number",
            SheetErr::NumberTaken { .. } => "sheets.err.number_taken",
            SheetErr::UnknownPaper(_) => "sheets.err.paper",
            SheetErr::UnknownStandard(_) => "sheets.err.standard",
            SheetErr::NoSuch(_) => "sheets.err.no_such",
            SheetErr::BadScale(_) => "sheets.err.scale",
            SheetErr::BadCut => "sheets.err.cut",
            SheetErr::BadPosition => "sheets.err.position",
            SheetErr::BadCrop => "sheets.err.crop",
            SheetErr::NoSingleView => "sheets.err.single_view",
        }
    }

    /// Message arguments by placeholder name; `what` of `NoSuch` is a catalog key
    /// (`sheets.err.what.*`), see [`SheetErr::what_key`].
    pub fn args(&self) -> Vec<(&'static str, String)> {
        match self {
            SheetErr::NumberTaken { number, name } => {
                vec![("number", number.clone()), ("name", name.clone())]
            }
            SheetErr::UnknownPaper(p) => vec![("paper", p.clone())],
            SheetErr::UnknownStandard(p) => vec![("standard", p.clone())],
            SheetErr::BadScale(v) => vec![("scale", format!("{v}"))],
            _ => vec![],
        }
    }

    /// Catalog key naming the missing thing, for `NoSuch`.
    pub fn what_key(&self) -> Option<&'static str> {
        match self {
            SheetErr::NoSuch("sheet") => Some("sheets.err.what.sheet"),
            SheetErr::NoSuch("sheet set") => Some("sheets.err.what.sheet_set"),
            SheetErr::NoSuch("viewport") => Some("sheets.err.what.viewport"),
            SheetErr::NoSuch("schedule placement") => Some("sheets.err.what.schedule"),
            SheetErr::NoSuch("level") => Some("sheets.err.what.level"),
            SheetErr::NoSuch(_) => Some("sheets.err.what.view"),
            _ => None,
        }
    }
}

impl std::fmt::Display for SheetErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SheetErr::NumberEmpty => f.write_str("sheet number is empty"),
            SheetErr::NumberTaken { number, name } => {
                write!(f, "sheet number '{number}' is used by '{name}'")
            }
            SheetErr::UnknownPaper(p) => write!(f, "unknown paper '{p}'"),
            SheetErr::UnknownStandard(p) => write!(f, "unknown standard '{p}'"),
            SheetErr::NoSuch(w) => write!(f, "no such {w}"),
            SheetErr::BadScale(v) => write!(f, "bad scale 1:{v}"),
            SheetErr::BadCut => f.write_str("cut height must be finite"),
            SheetErr::BadPosition => f.write_str("viewport position must be finite"),
            SheetErr::BadCrop => f.write_str("crop must be a non-empty box"),
            SheetErr::NoSingleView => f.write_str("viewport has no single view"),
        }
    }
}

impl From<SheetErr> for Error {
    fn from(e: SheetErr) -> Error {
        Error::Sheet(e)
    }
}

fn check_sheet(db: &Connection, s: &SheetDef, id: Option<i64>) -> Result<(), Error> {
    if s.number.trim().is_empty() {
        return Err(SheetErr::NumberEmpty.into());
    }
    if !PAPERS.contains(&s.paper.as_str()) {
        return Err(SheetErr::UnknownPaper(s.paper.clone()).into());
    }
    if !STANDARDS.contains(&s.standard.as_str()) {
        return Err(SheetErr::UnknownStandard(s.standard.clone()).into());
    }
    let taken: Option<String> = db
        .query_row(
            "SELECT name FROM sheet WHERE number=?1 AND id IS NOT ?2",
            params![s.number, id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(name) = taken {
        return Err(SheetErr::NumberTaken {
            number: s.number.clone(),
            name,
        }
        .into());
    }
    Ok(())
}

fn check_viewport(db: &Connection, v: &ViewportDef) -> Result<(), Error> {
    if !exists(db, "sheet", v.sheet)? {
        return no_such("sheet", v.sheet);
    }
    match v.view {
        ViewRef::Plan { level, cut } => {
            if !exists(db, "level", level)? {
                return no_such("level", level);
            }
            if !cut.is_finite() {
                return Err(SheetErr::BadCut.into());
            }
        }
        ViewRef::View(id) => {
            if !exists(db, "view_def", id)? {
                return no_such("view", id);
            }
        }
    }
    if !(v.scale.is_finite() && v.scale > 0.0 && v.scale <= 1e5) {
        return Err(SheetErr::BadScale(v.scale).into());
    }
    if !(v.center.0.is_finite() && v.center.1.is_finite()) {
        return Err(SheetErr::BadPosition.into());
    }
    if let Some(c) = v.crop {
        if !(c.iter().all(|x| x.is_finite()) && c[2] - c[0] > 1e-6 && c[3] - c[1] > 1e-6) {
            return Err(SheetErr::BadCrop.into());
        }
    }
    Ok(())
}

fn fields_text(f: &BTreeMap<String, String>) -> String {
    serde_json::to_string(f).unwrap_or_else(|_| "{}".into())
}

fn read_sheet(db: &Connection, id: i64) -> Result<Option<SheetDef>, Error> {
    let r = db
        .query_row(
            "SELECT number,name,paper,landscape,standard,fields FROM sheet WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((number, name, paper, landscape, standard, f)) = r else {
        return Ok(None);
    };
    Ok(Some(SheetDef {
        number,
        name,
        paper,
        landscape,
        standard,
        fields: serde_json::from_str(&f)?,
    }))
}

fn read_viewport(db: &Connection, id: i64) -> Result<Option<ViewportDef>, Error> {
    type Row = (
        i64,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        f64,
        (f64, f64),
        [Option<f64>; 4],
        String,
    );
    let r: Option<Row> = db
        .query_row(
            "SELECT sheet,level,view_def,cut,scale,x,y,cx0,cy0,cx1,cy1,title FROM viewport WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    (r.get(5)?, r.get(6)?),
                    [r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?],
                    r.get(11)?,
                ))
            },
        )
        .optional()?;
    let Some((sheet, level, view_def, cut, scale, center, c, title)) = r else {
        return Ok(None);
    };
    let view = match (level, view_def) {
        (Some(level), None) => ViewRef::Plan {
            level,
            cut: cut.unwrap_or(DEFAULT_CUT),
        },
        (None, Some(v)) => ViewRef::View(v),
        _ => return Err(SheetErr::NoSingleView.into()),
    };
    let crop = match c {
        [Some(a), Some(b), Some(c), Some(d)] => Some([a, b, c, d]),
        _ => None,
    };
    Ok(Some(ViewportDef {
        sheet,
        view,
        scale,
        center,
        crop,
        title,
    }))
}

fn write_viewport(db: &Connection, id: Option<i64>, v: &ViewportDef) -> Result<i64, Error> {
    let (level, view_def, cut) = match v.view {
        ViewRef::Plan { level, cut } => (Some(level), None, Some(cut)),
        ViewRef::View(id) => (None, Some(id), None),
    };
    let c = v.crop.map_or([None; 4], |c| c.map(Some));
    db.execute(
        // upsert, not REPLACE: columns outside the definition (the view number) survive a viewport edit
        "INSERT INTO viewport(id,sheet,level,view_def,cut,scale,x,y,cx0,cy0,cx1,cy1,title)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
         ON CONFLICT(id) DO UPDATE SET sheet=excluded.sheet, level=excluded.level,
            view_def=excluded.view_def, cut=excluded.cut, scale=excluded.scale, x=excluded.x,
            y=excluded.y, cx0=excluded.cx0, cy0=excluded.cy0, cx1=excluded.cx1, cy1=excluded.cy1,
            title=excluded.title",
        params![
            id, v.sheet, level, view_def, cut, v.scale, v.center.0, v.center.1, c[0], c[1], c[2],
            c[3], v.title
        ],
    )?;
    let id = id.unwrap_or_else(|| db.last_insert_rowid());
    output::settle_detail(db, id, v.sheet)?;
    Ok(id)
}

fn insert_sheet(db: &Connection, s: &SheetDef) -> Result<i64, Error> {
    let seq: i64 = db.query_row("SELECT COALESCE(MAX(seq),-1)+1 FROM sheet", [], |r| {
        r.get(0)
    })?;
    db.execute(
        "INSERT INTO sheet(number,name,paper,landscape,standard,fields,seq) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            s.number,
            s.name,
            s.paper,
            i64::from(s.landscape),
            s.standard,
            fields_text(&s.fields),
            seq
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn ids(db: &Connection, sql: &str, arg: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare_cached(sql)?;
    let v = st
        .query_map([arg], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Snapshots rows `table WHERE col=id` (undo restores them) and deletes them.
fn update_sheet_row(db: &Connection, id: i64, s: &SheetDef) -> Result<(), Error> {
    db.execute(
        "UPDATE sheet SET number=?2,name=?3,paper=?4,landscape=?5,standard=?6,fields=?7 WHERE id=?1",
        params![
            id,
            s.number,
            s.name,
            s.paper,
            i64::from(s.landscape),
            s.standard,
            fields_text(&s.fields)
        ],
    )?;
    Ok(())
}

/// Replaces a sheet's revisions; the inverse command deletes the new ones and restores the old ones.
fn write_revisions(db: &Connection, sheet: i64, revs: &[Revision]) -> Result<Inv, Error> {
    let inv = take(db, Inv::default(), "sheet_rev", "sheet", sheet)?;
    for (k, r) in revs.iter().enumerate() {
        db.execute(
            "INSERT INTO sheet_rev(sheet,idx,mark,date,description,author) VALUES (?1,?2,?3,?4,?5,?6)",
            params![sheet, k as i64, r.mark, r.date, r.description, r.author],
        )?;
    }
    Ok(Inv::default().delete("sheet_rev", "sheet", sheet).then(inv))
}

/// Rows of `table` of sheet `from` are copied to sheet `to` with all columns except id.
fn clone_rows(db: &Connection, table: &str, from: i64, to: i64) -> Result<(), Error> {
    let cols: Vec<String> = {
        let mut st = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let v = st
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    let keep: Vec<&str> = cols
        .iter()
        .map(String::as_str)
        .filter(|c| *c != "id")
        .collect();
    let list = keep.join(",");
    let sel = keep
        .iter()
        .map(|c| if *c == "sheet" { "?2".to_string() } else { (*c).to_string() })
        .collect::<Vec<_>>()
        .join(",");
    db.execute(
        &format!("INSERT INTO {table}({list}) SELECT {sel} FROM {table} WHERE sheet=?1 ORDER BY id"),
        params![from, to],
    )?;
    Ok(())
}

fn take(db: &Connection, inv: Inv, table: &str, col: &str, id: i64) -> Result<Inv, Error> {
    let inv = inv.restore(db, table, col, id)?;
    db.execute(&format!("DELETE FROM {table} WHERE {col}=?1"), [id])?;
    Ok(inv)
}

/// A view's viewports go with the view (`col` is level or view_def); inverse commands restore them.
pub(crate) fn drop_viewports(db: &Connection, col: &str, id: i64) -> Result<Inv, Error> {
    take(db, Inv::default(), "viewport", col, id)
}

/// Violations not expressible as references: a viewport with no view or with two views, a foreign format.
pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    let mut st = db.prepare(
        "SELECT id FROM viewport WHERE (level IS NULL) = (view_def IS NULL) ORDER BY id",
    )?;
    for id in st.query_map([], |r| r.get::<_, i64>(0))? {
        out.push(Issue {
            table: "viewport".into(),
            id: id?,
            problem: "viewport must show exactly one view".into(),
        });
    }
    let mut st = db.prepare("SELECT id, paper FROM sheet ORDER BY id")?;
    for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, paper) = r?;
        if !PAPERS.contains(&paper.as_str()) {
            out.push(Issue {
                table: "sheet".into(),
                id,
                problem: format!("unknown paper '{paper}'"),
            });
        }
    }
    Ok(out)
}

impl Model {
    pub fn add_sheet(&mut self, s: &SheetDef) -> Result<i64, Error> {
        self.op("sheet_add", |db| {
            check_sheet(db, s, None)?;
            let id = insert_sheet(db, s)?;
            Ok((id, id, Inv::default().delete("sheet", "id", id)))
        })
    }

    pub fn update_sheet(&mut self, id: i64, s: &SheetDef) -> Result<(), Error> {
        self.op("sheet_update", |db| {
            if !exists(db, "sheet", id)? {
                return no_such("sheet", id);
            }
            check_sheet(db, s, Some(id))?;
            let inv = Inv::default().restore(db, "sheet", "id", id)?;
            update_sheet_row(db, id, s)?;
            Ok(((), id, inv))
        })
    }

    /// A sheet with its viewports, revisions, schedules and set memberships: one undo step.
    pub fn delete_sheet(&mut self, id: i64) -> Result<(), Error> {
        self.op("sheet_delete", |db| {
            if !exists(db, "sheet", id)? {
                return no_such("sheet", id);
            }
            let mut inv = take(db, Inv::default(), "sheet", "id", id)?;
            for t in OWNED {
                inv = take(db, inv, t, "sheet", id)?;
            }
            Ok(((), id, inv))
        })
    }

    /// A sheet copy in one undo step: title-block fields, format, schedules and legends; with
    /// `with_views` also all viewports (same view, scale, crop, position). Revisions are not
    /// copied: the new sheet is unissued. The number is the next free one, the name is "<name> copy".
    /// `copy` is the UI-language word appended to the name.
    pub fn duplicate_sheet(&mut self, src: i64, with_views: bool, copy: &str) -> Result<i64, Error> {
        let mut def = self.sheet(src)?;
        def.number = self.next_sheet_number()?;
        let names: Vec<String> = self.sheets()?.into_iter().map(|(_, s)| s.name).collect();
        let base = format!("{} {copy}", def.name);
        let mut name = base.clone();
        let mut k = 2;
        while names.contains(&name) {
            name = format!("{base} {k}");
            k += 1;
        }
        def.name = name;
        self.op("sheet_duplicate", |db| {
            check_sheet(db, &def, None)?;
            let id = insert_sheet(db, &def)?;
            let mut inv = Inv::default().delete("sheet", "id", id);
            let mut tables = vec!["sheet_schedule", "anno_legend_place"];
            if with_views {
                tables.push("viewport");
            }
            for t in tables {
                clone_rows(db, t, src, id)?;
                inv = Inv::default().delete(t, "sheet", id).then(inv);
            }
            Ok((id, id, inv))
        })
    }

    pub fn sheet(&self, id: i64) -> Result<SheetDef, Error> {
        read_sheet(&self.db, id)?.map_or_else(|| no_such("sheet", id), Ok)
    }

    /// Sheets in list order (`seq`), then by id.
    pub fn sheets(&self) -> Result<Vec<(i64, SheetDef)>, Error> {
        ids(
            &self.db,
            "SELECT id FROM sheet WHERE ?1=?1 ORDER BY seq, id",
            0,
        )?
        .into_iter()
        .map(|id| Ok((id, self.sheet(id)?)))
        .collect()
    }

    /// Number for a new sheet: the one after the last in the list, and free.
    pub fn next_sheet_number(&self) -> Result<String, Error> {
        let all = self.sheets()?;
        let mut n = all
            .last()
            .map_or_else(|| "A-101".to_string(), |(_, s)| increment_number(&s.number));
        while all.iter().any(|(_, s)| s.number == n) {
            n = increment_number(&n);
        }
        Ok(n)
    }

    /// A new list order: `order` contains every sheet exactly once.
    pub fn reorder_sheets(&mut self, order: &[i64]) -> Result<(), Error> {
        self.op("sheet_reorder", |db| {
            let mut all = ids(db, "SELECT id FROM sheet WHERE ?1=?1 ORDER BY id", 0)?;
            let mut want = order.to_vec();
            want.sort_unstable();
            all.sort_unstable();
            if want != all {
                return Err(Error::Invalid("order must list every sheet once".into()));
            }
            let mut inv = Inv::default();
            for (k, id) in order.iter().enumerate() {
                inv = inv.restore(db, "sheet", "id", *id)?;
                db.execute("UPDATE sheet SET seq=?2 WHERE id=?1", params![id, k as i64])?;
            }
            Ok(((), 0, inv))
        })
    }

    /// Shift a sheet in the list by `delta` positions (clamped at the ends).
    pub fn move_sheet(&mut self, id: i64, delta: i64) -> Result<(), Error> {
        let mut order: Vec<i64> = self.sheets()?.into_iter().map(|s| s.0).collect();
        let Some(k) = order.iter().position(|x| *x == id) else {
            return no_such("sheet", id);
        };
        let to = (k as i64 + delta).clamp(0, order.len() as i64 - 1) as usize;
        let v = order.remove(k);
        order.insert(to, v);
        self.reorder_sheets(&order)
    }

    /// Numbering: `ids` in order get `first`, the next, ... A clash with another sheet's
    /// number is rejected without changes.
    pub fn renumber_sheets(&mut self, sheet_ids: &[i64], first: &str) -> Result<(), Error> {
        self.op("sheet_renumber", |db| {
            let mut nums = vec![];
            let mut n = first.trim().to_string();
            for _ in sheet_ids {
                nums.push(n.clone());
                n = increment_number(&n);
            }
            let mut inv = Inv::default();
            for id in sheet_ids {
                if !exists(db, "sheet", *id)? {
                    return no_such("sheet", *id);
                }
                inv = inv.restore(db, "sheet", "id", *id)?;
                // temporary number: swapping numbers within a set does not hit UNIQUE
                db.execute("UPDATE sheet SET number=char(1)||id WHERE id=?1", [*id])?;
            }
            for (id, num) in sheet_ids.iter().zip(&nums) {
                let s = read_sheet(db, *id)?.expect("checked above");
                check_sheet(
                    db,
                    &SheetDef {
                        number: num.clone(),
                        ..s
                    },
                    Some(*id),
                )?;
                db.execute("UPDATE sheet SET number=?2 WHERE id=?1", params![id, num])?;
            }
            Ok(((), 0, inv))
        })
    }

    /// All revisions of a sheet (the revision table), in order.
    pub fn set_revisions(&mut self, sheet: i64, revs: &[Revision]) -> Result<(), Error> {
        self.op("sheet_revisions", |db| {
            if !exists(db, "sheet", sheet)? {
                return no_such("sheet", sheet);
            }
            Ok(((), sheet, write_revisions(db, sheet, revs)?))
        })
    }

    /// Sheet properties and its revisions: one undo step. `id = None` creates a sheet; returns the sheet id.
    pub fn save_sheet_with_revisions(
        &mut self,
        id: Option<i64>,
        s: &SheetDef,
        revs: &[Revision],
    ) -> Result<i64, Error> {
        self.op("sheet_save", |db| {
            let (id, inv) = match id {
                None => {
                    check_sheet(db, s, None)?;
                    let id = insert_sheet(db, s)?;
                    (id, Inv::default().delete("sheet", "id", id))
                }
                Some(id) => {
                    if !exists(db, "sheet", id)? {
                        return no_such("sheet", id);
                    }
                    check_sheet(db, s, Some(id))?;
                    let inv = Inv::default().restore(db, "sheet", "id", id)?;
                    update_sheet_row(db, id, s)?;
                    (id, inv)
                }
            };
            // Revisions roll back before the sheet: on creation, the sheet row is deleted last.
            let inv = write_revisions(db, id, revs)?.then(inv);
            Ok((id, id, inv))
        })
    }

    /// Moving a sheet's viewports and schedules: one undo step (an error rolls back everything).
    pub fn move_sheet_items(
        &mut self,
        viewports: &[(i64, (f64, f64))],
        schedules: &[(i64, (f64, f64))],
    ) -> Result<(), Error> {
        self.op("sheet_items_move", |db| {
            let mut inv = Inv::default();
            for (table, what, list) in [
                ("viewport", "viewport", viewports),
                ("sheet_schedule", "schedule placement", schedules),
            ] {
                for (id, (x, y)) in list {
                    if !(x.is_finite() && y.is_finite()) {
                        return Err(SheetErr::BadPosition.into());
                    }
                    inv = inv.restore(db, table, "id", *id)?;
                    if db.execute(
                        &format!("UPDATE {table} SET x=?2, y=?3 WHERE id=?1"),
                        params![id, x, y],
                    )? == 0
                    {
                        return no_such(what, *id);
                    }
                }
            }
            Ok(((), 0, inv))
        })
    }

    /// Deleting selected viewports and schedules: one undo step.
    pub fn delete_sheet_items(
        &mut self,
        viewports: &[i64],
        schedules: &[i64],
    ) -> Result<(), Error> {
        self.op("sheet_items_delete", |db| {
            let mut inv = Inv::default();
            for (table, what, list) in [
                ("viewport", "viewport", viewports),
                ("sheet_schedule", "schedule placement", schedules),
            ] {
                for id in list {
                    if !exists(db, table, *id)? {
                        return no_such(what, *id);
                    }
                    inv = take(db, inv, table, "id", *id)?;
                }
            }
            Ok(((), 0, inv))
        })
    }

    pub fn revisions(&self, sheet: i64) -> Result<Vec<Revision>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT mark,date,description,author FROM sheet_rev WHERE sheet=?1 ORDER BY idx",
        )?;
        let v = st
            .query_map([sheet], |r| {
                Ok(Revision {
                    mark: r.get(0)?,
                    date: r.get(1)?,
                    description: r.get(2)?,
                    author: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn add_viewport(&mut self, v: &ViewportDef) -> Result<i64, Error> {
        self.op("viewport_add", |db| {
            check_viewport(db, v)?;
            let id = write_viewport(db, None, v)?;
            Ok((id, id, Inv::default().delete("viewport", "id", id)))
        })
    }

    pub fn update_viewport(&mut self, id: i64, v: &ViewportDef) -> Result<(), Error> {
        self.op("viewport_update", |db| {
            if !exists(db, "viewport", id)? {
                return no_such("viewport", id);
            }
            check_viewport(db, v)?;
            let inv = Inv::default().restore(db, "viewport", "id", id)?;
            write_viewport(db, Some(id), v)?;
            Ok(((), id, inv))
        })
    }

    /// Moving several viewports (alignment): one undo step.
    pub fn move_viewports(&mut self, moves: &[(i64, (f64, f64))]) -> Result<(), Error> {
        self.op("viewport_move", |db| {
            let mut inv = Inv::default();
            for (id, (x, y)) in moves {
                if !(x.is_finite() && y.is_finite()) {
                    return Err(SheetErr::BadPosition.into());
                }
                inv = inv.restore(db, "viewport", "id", *id)?;
                if db.execute(
                    "UPDATE viewport SET x=?2, y=?3 WHERE id=?1",
                    params![id, x, y],
                )? == 0
                {
                    return no_such("viewport", *id);
                }
            }
            Ok(((), 0, inv))
        })
    }

    pub fn delete_viewport(&mut self, id: i64) -> Result<(), Error> {
        self.op("viewport_delete", |db| {
            if !exists(db, "viewport", id)? {
                return no_such("viewport", id);
            }
            Ok(((), id, take(db, Inv::default(), "viewport", "id", id)?))
        })
    }

    pub fn viewport(&self, id: i64) -> Result<ViewportDef, Error> {
        read_viewport(&self.db, id)?.map_or_else(|| no_such("viewport", id), Ok)
    }

    /// A sheet's viewports by id (placement order).
    pub fn viewports(&self, sheet: i64) -> Result<Vec<(i64, ViewportDef)>, Error> {
        ids(
            &self.db,
            "SELECT id FROM viewport WHERE sheet=?1 ORDER BY id",
            sheet,
        )?
        .into_iter()
        .map(|id| Ok((id, self.viewport(id)?)))
        .collect()
    }

    /// Where a view is placed: (viewport, sheet). A plan is compared by level.
    pub fn placements(&self, view: ViewRef) -> Result<Vec<(i64, i64)>, Error> {
        let (sql, arg) = match view {
            ViewRef::Plan { level, .. } => (
                "SELECT id, sheet FROM viewport WHERE level=?1 ORDER BY id",
                level,
            ),
            ViewRef::View(v) => (
                "SELECT id, sheet FROM viewport WHERE view_def=?1 ORDER BY id",
                v,
            ),
        };
        let mut st = self.db.prepare_cached(sql)?;
        let v = st
            .query_map([arg], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn add_schedule_place(&mut self, p: &SchedulePlace) -> Result<i64, Error> {
        self.op("sheet_schedule_add", |db| {
            if !exists(db, "sheet", p.sheet)? {
                return no_such("sheet", p.sheet);
            }
            if p.schedule.trim().is_empty() || !(p.pos.0.is_finite() && p.pos.1.is_finite()) {
                return Err(Error::Invalid("schedule key and position required".into()));
            }
            db.execute(
                "INSERT INTO sheet_schedule(sheet,schedule,x,y) VALUES (?1,?2,?3,?4)",
                params![p.sheet, p.schedule, p.pos.0, p.pos.1],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("sheet_schedule", "id", id)))
        })
    }

    pub fn move_schedule_place(&mut self, id: i64, pos: (f64, f64)) -> Result<(), Error> {
        self.op("sheet_schedule_move", |db| {
            if !(pos.0.is_finite() && pos.1.is_finite()) {
                return Err(SheetErr::BadPosition.into());
            }
            let inv = Inv::default().restore(db, "sheet_schedule", "id", id)?;
            if db.execute(
                "UPDATE sheet_schedule SET x=?2, y=?3 WHERE id=?1",
                params![id, pos.0, pos.1],
            )? == 0
            {
                return no_such("schedule placement", id);
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_schedule_place(&mut self, id: i64) -> Result<(), Error> {
        self.op("sheet_schedule_delete", |db| {
            if !exists(db, "sheet_schedule", id)? {
                return no_such("schedule placement", id);
            }
            Ok((
                (),
                id,
                take(db, Inv::default(), "sheet_schedule", "id", id)?,
            ))
        })
    }

    pub fn schedule_places(&self, sheet: i64) -> Result<Vec<(i64, SchedulePlace)>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT id,schedule,x,y FROM sheet_schedule WHERE sheet=?1 ORDER BY id",
        )?;
        let v = st
            .query_map([sheet], |r| {
                Ok((
                    r.get(0)?,
                    SchedulePlace {
                        sheet,
                        schedule: r.get(1)?,
                        pos: (r.get(2)?, r.get(3)?),
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    fn write_set_items(db: &Connection, set: i64, sheets: &[i64]) -> Result<(), Error> {
        for (k, s) in sheets.iter().enumerate() {
            if !exists(db, "sheet", *s)? {
                return no_such("sheet", *s);
            }
            db.execute(
                "INSERT INTO sheet_set_item(sheet_set,idx,sheet) VALUES (?1,?2,?3)",
                params![set, k as i64, s],
            )?;
        }
        Ok(())
    }

    fn check_set_name(db: &Connection, name: &str, id: Option<i64>) -> Result<(), Error> {
        if name.trim().is_empty() {
            return Err(Error::Invalid("sheet set name is empty".into()));
        }
        let n: i64 = db.query_row(
            "SELECT count(*) FROM sheet_set WHERE name=?1 AND id IS NOT ?2",
            params![name, id],
            |r| r.get(0),
        )?;
        if n > 0 {
            return Err(Error::Invalid(format!("sheet set '{name}' exists")));
        }
        Ok(())
    }

    /// A sheet set for printing: name and sheets in order.
    pub fn add_sheet_set(&mut self, name: &str, sheets: &[i64]) -> Result<i64, Error> {
        self.op("sheet_set_add", |db| {
            Self::check_set_name(db, name, None)?;
            db.execute("INSERT INTO sheet_set(name) VALUES (?1)", [name])?;
            let id = db.last_insert_rowid();
            Self::write_set_items(db, id, sheets)?;
            let inv = Inv::default()
                .delete("sheet_set_item", "sheet_set", id)
                .delete("sheet_set", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn update_sheet_set(&mut self, id: i64, name: &str, sheets: &[i64]) -> Result<(), Error> {
        self.op("sheet_set_update", |db| {
            if !exists(db, "sheet_set", id)? {
                return no_such("sheet set", id);
            }
            Self::check_set_name(db, name, Some(id))?;
            let inv = Inv::default().restore(db, "sheet_set", "id", id)?;
            let inv = take(db, inv, "sheet_set_item", "sheet_set", id)?;
            db.execute(
                "UPDATE sheet_set SET name=?2 WHERE id=?1",
                params![id, name],
            )?;
            Self::write_set_items(db, id, sheets)?;
            let inv = Inv::default()
                .delete("sheet_set_item", "sheet_set", id)
                .then(inv);
            Ok(((), id, inv))
        })
    }

    pub fn delete_sheet_set(&mut self, id: i64) -> Result<(), Error> {
        self.op("sheet_set_delete", |db| {
            if !exists(db, "sheet_set", id)? {
                return no_such("sheet set", id);
            }
            let inv = take(db, Inv::default(), "sheet_set", "id", id)?;
            Ok(((), id, take(db, inv, "sheet_set_item", "sheet_set", id)?))
        })
    }

    /// Sets: (id, name, sheets in order).
    pub fn sheet_sets(&self) -> Result<Vec<(i64, String, Vec<i64>)>, Error> {
        let sets: Vec<(i64, String)> = {
            let mut st = self
                .db
                .prepare("SELECT id, name FROM sheet_set ORDER BY name, id")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        sets.into_iter()
            .map(|(id, name)| {
                let s = ids(
                    &self.db,
                    "SELECT sheet FROM sheet_set_item WHERE sheet_set=?1 ORDER BY idx",
                    id,
                )?;
                Ok((id, name, s))
            })
            .collect()
    }

    /// Auto-set: one sheet per level in `levels` (from template `tpl`: format, stamp, first
    /// number), the level plan at scale 1:`scale` at point `center` of the sheet, the same on all sheets.
    /// The sheet name is "`tpl.name` · level". One undo step.
    pub fn add_plan_sheets(
        &mut self,
        tpl: &SheetDef,
        levels: &[i64],
        scale: f64,
        center: (f64, f64),
    ) -> Result<Vec<i64>, Error> {
        self.op("sheet_add_plans", |db| {
            let (out, inv) = plan_sheets(db, tpl, levels, scale, center)?;
            Ok((out, 0, inv))
        })
    }
}

/// Level plan sheets: new sheets and the inverse command (the body of the auto-set).
fn plan_sheets(
    db: &Connection,
    tpl: &SheetDef,
    levels: &[i64],
    scale: f64,
    center: (f64, f64),
) -> Result<(Vec<i64>, Inv), Error> {
    let mut out = vec![];
    let mut inv = Inv::default();
    let mut number = tpl.number.clone();
    for lv in levels {
        let name: String = db
            .query_row("SELECT name FROM level WHERE id=?1", [lv], |r| r.get(0))
            .optional()?
            .map_or_else(|| no_such("level", *lv), Ok)?;
        loop {
            let n: i64 = db.query_row(
                "SELECT count(*) FROM sheet WHERE number=?1",
                [&number],
                |r| r.get(0),
            )?;
            if n == 0 {
                break;
            }
            number = increment_number(&number);
        }
        let s = SheetDef {
            number: number.clone(),
            name: format!("{} · {name}", tpl.name)
                .trim_start_matches(" · ")
                .into(),
            ..tpl.clone()
        };
        check_sheet(db, &s, None)?;
        let sid = insert_sheet(db, &s)?;
        let v = ViewportDef {
            sheet: sid,
            view: ViewRef::Plan {
                level: *lv,
                cut: DEFAULT_CUT,
            },
            scale,
            center,
            crop: None,
            title: name,
        };
        check_viewport(db, &v)?;
        let vid = write_viewport(db, None, &v)?;
        inv = Inv::default()
            .delete("viewport", "id", vid)
            .delete("sheet", "id", sid)
            .then(inv);
        out.push(sid);
        number = increment_number(&number);
    }
    Ok((out, inv))
}
