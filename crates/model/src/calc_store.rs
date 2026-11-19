//! Structural analysis storage, schema step 44 (docs/design/STRUCTURAL_ANALYSIS.md, 1.4).
//! Intent is content: analytical overrides, load cases, loads, combinations and settings; every
//! edit is one undo step and compare, merge and the content hash see it. Runs (`calc_run`,
//! `calc_blob`) are project data outside the undo journal, like `ai_audit`: compare, merge, the
//! plan diff and the content hash skip them. Older unpinned runs move to an archive file next to
//! the project and are never deleted. Units are SI; values are JSON. A row with `locked = 1` is
//! changed only by `src = "manual"`: generation, import and AI are refused.

use std::path::{Path, PathBuf};
use web_time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use serde_json::Value as J;

use crate::ops::Inv;
use crate::{Error, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS an_override(id INTEGER PRIMARY KEY, target TEXT NOT NULL,
    field TEXT NOT NULL, value TEXT NOT NULL, anchor TEXT, locked INTEGER NOT NULL DEFAULT 0,
    src TEXT NOT NULL DEFAULT 'manual', why TEXT, UNIQUE(target, field));
CREATE TABLE IF NOT EXISTS load_case(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    category TEXT NOT NULL, self_weight INTEGER NOT NULL DEFAULT 0,
    note TEXT NOT NULL DEFAULT '', locked INTEGER NOT NULL DEFAULT 0,
    src TEXT NOT NULL DEFAULT 'manual');
CREATE TABLE IF NOT EXISTS load(id INTEGER PRIMARY KEY, lcase INTEGER NOT NULL,
    kind TEXT NOT NULL, target TEXT NOT NULL, frame TEXT NOT NULL DEFAULT 'global',
    dir TEXT NOT NULL, value TEXT NOT NULL, grp TEXT, locked INTEGER NOT NULL DEFAULT 0,
    src TEXT NOT NULL DEFAULT 'manual', why TEXT);
CREATE INDEX IF NOT EXISTS load_lcase ON load(lcase);
CREATE TABLE IF NOT EXISTS load_combo(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL, generated INTEGER NOT NULL DEFAULT 0, rule TEXT,
    locked INTEGER NOT NULL DEFAULT 0, src TEXT NOT NULL DEFAULT 'manual');
CREATE TABLE IF NOT EXISTS load_combo_term(combo INTEGER NOT NULL, lcase INTEGER NOT NULL,
    factor REAL NOT NULL, PRIMARY KEY(combo, lcase));
CREATE TABLE IF NOT EXISTS calc_settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Run tables; also the schema of each archive file.
const RUN_DDL: &str = "
CREATE TABLE IF NOT EXISTS calc_run(id INTEGER PRIMARY KEY, created INTEGER NOT NULL,
    input_hash TEXT NOT NULL, fem_version TEXT NOT NULL, status TEXT NOT NULL,
    stats TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0, src TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS calc_blob(run INTEGER NOT NULL, part TEXT NOT NULL,
    data BLOB NOT NULL, PRIMARY KEY(run, part));
";

/// Mechanical material columns, SI: E and G in Pa, Poisson ratio, fy in Pa, alpha in 1/K.
const MECH_COLS: [&str; 5] = ["e_mod", "g_mod", "poisson", "fy", "alpha_t"];

/// Every table of this step (registries of compare, merge, clipboard and regeneration).
pub const CALC_TABLES: [&str; 8] = [
    "an_override",
    "load_case",
    "load",
    "load_combo",
    "load_combo_term",
    "calc_settings",
    "calc_run",
    "calc_blob",
];

/// References for `integrity` and merge: (child table, column, parent table).
pub(crate) const REFS: [(&str, &str, &str); 4] = [
    ("load", "lcase", "load_case"),
    ("load_combo_term", "lcase", "load_case"),
    ("load_combo_term", "combo", "load_combo"),
    ("calc_blob", "run", "calc_run"),
];

/// Unpinned runs kept in the file; older ones move to the archive.
pub const CALC_KEEP_UNPINNED: usize = 5;

pub const LOAD_CATEGORIES: [&str; 10] = [
    "permanent",
    "imposed_residential",
    "imposed_office",
    "imposed_assembly",
    "imposed_storage",
    "imposed_roof",
    "snow",
    "wind",
    "temperature",
    "accidental",
];
pub const LOAD_KINDS: [&str; 4] = ["point", "line", "area", "temp"];
pub const LOAD_FRAMES: [&str; 3] = ["global", "local", "projected"];
pub const COMBO_STATES: [&str; 5] = ["uls", "sls_char", "sls_freq", "sls_qp", "accidental"];
pub const RUN_STATUSES: [&str; 4] = ["ok", "suspect", "failed", "stale"];

/// Tables written outside the undo journal.
pub(crate) fn is_run_table(t: &str) -> bool {
    matches!(t, "calc_run" | "calc_blob")
}

/// Additive and idempotent: a current file opens with no schema change.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    db.execute_batch(RUN_DDL)?;
    for c in MECH_COLS {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('material') WHERE name=?1",
            [c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE material ADD COLUMN {c} REAL"))?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnOverride {
    /// 0 for a new row.
    pub id: i64,
    /// Stable key (`m:beam:45`, `n:...`, `s:...`, `p:slab:3`) or `global`.
    pub target: String,
    pub field: String,
    pub value: J,
    /// Node anchor `{at:[x,y,z], ends:[...]}` for re-anchoring after regeneration.
    pub anchor: Option<J>,
    pub locked: bool,
    pub src: String,
    pub why: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadCase {
    pub id: i64,
    pub name: String,
    /// One of [`LOAD_CATEGORIES`].
    pub category: String,
    pub self_weight: bool,
    pub note: String,
    pub locked: bool,
    pub src: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Load {
    pub id: i64,
    pub lcase: i64,
    /// One of [`LOAD_KINDS`].
    pub kind: String,
    /// Stable key, JSON list of keys, or `filter:<query>`.
    pub target: String,
    pub frame: String,
    /// `x y z -x -y -z`, local `lx ly lz -lx -ly -lz`, or a JSON vector.
    pub dir: String,
    /// `{"f":N}` | `{"q1":N/m,"q2":N/m,"x1":m,"x2":m}` | `{"p":Pa}` | `{"dt":K}`.
    pub value: J,
    pub grp: Option<String>,
    pub locked: bool,
    pub src: String,
    pub why: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadCombo {
    pub id: i64,
    pub name: String,
    /// One of [`COMBO_STATES`].
    pub state: String,
    pub generated: bool,
    pub rule: Option<String>,
    pub locked: bool,
    pub src: String,
    /// (load case id, factor), ordered by case id.
    pub terms: Vec<(i64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CalcRun {
    /// 0 for a new run.
    pub id: i64,
    /// Unix seconds; set on insert.
    pub created: i64,
    /// Hash of the full solver input: a different current hash means "out of date".
    pub input_hash: String,
    pub fem_version: String,
    /// One of [`RUN_STATUSES`].
    pub status: String,
    pub stats: J,
    pub pinned: bool,
    pub src: String,
    pub note: String,
}

/// Mechanical properties of a material, SI; None = not set.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MatMech {
    /// Young's modulus, Pa.
    pub e: Option<f64>,
    /// Shear modulus, Pa.
    pub g: Option<f64>,
    pub nu: Option<f64>,
    /// Yield or characteristic strength, Pa.
    pub fy: Option<f64>,
    /// Thermal expansion, 1/K.
    pub alpha: Option<f64>,
}

impl MatMech {
    /// G as set, else E / (2 (1 + nu)).
    pub fn shear(&self) -> Option<f64> {
        self.g.or_else(|| Some(self.e? / (2.0 * (1.0 + self.nu?))))
    }
}

fn bad<T>(field: &str, why: &str) -> Result<T, Error> {
    Err(Error::Invalid(format!("calc {field}: {why}")))
}

fn js(s: &str) -> J {
    serde_json::from_str(s).unwrap_or_else(|_| J::String(s.to_string()))
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn finite(v: &J) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

fn num(o: &J, k: &str, field: &str) -> Result<f64, Error> {
    match o.get(k).and_then(finite) {
        Some(x) => Ok(x),
        None => bad(field, &format!("'{k}' must be a finite number")),
    }
}

fn opt_num(o: &J, k: &str, field: &str) -> Result<Option<f64>, Error> {
    match o.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(v) => match finite(v) {
            Some(x) => Ok(Some(x)),
            None => bad(field, &format!("'{k}' must be a finite number")),
        },
    }
}

fn vec3(v: Option<&J>, field: &str) -> Result<[f64; 3], Error> {
    let a = v.and_then(J::as_array).filter(|a| a.len() == 3);
    let Some(a) = a else {
        return bad(field, "expected a vector [x, y, z]");
    };
    let mut out = [0.0; 3];
    for (o, x) in out.iter_mut().zip(a) {
        *o = match finite(x) {
            Some(f) => f,
            None => return bad(field, "vector values must be finite"),
        };
    }
    Ok(out)
}

fn len3(d: [f64; 3]) -> f64 {
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn one_of(v: &str, set: &[&str], field: &str) -> Result<(), Error> {
    if set.contains(&v) {
        Ok(())
    } else {
        bad(field, &format!("'{v}' is not one of {}", set.join(", ")))
    }
}

/// `manual | generated | import | ai:<session> | mcp:<client>`.
pub fn check_src(src: &str) -> Result<(), Error> {
    let tagged = |p: &str| src.strip_prefix(p).is_some_and(|r| !r.trim().is_empty());
    if matches!(src, "manual" | "generated" | "import") || tagged("ai:") || tagged("mcp:") {
        Ok(())
    } else {
        bad("src", "expected manual, generated, import, ai:<session> or mcp:<client>")
    }
}

fn check_name(name: &str) -> Result<(), Error> {
    let n = name.trim();
    if n.is_empty() || n != name || n.chars().count() > 120 || n.chars().any(char::is_control) {
        return bad("name", "1 to 120 characters, no spaces at the ends");
    }
    Ok(())
}

/// Kind of a stable key: `m` member, `n` node, `s` support, `p` panel, `g` global.
pub fn key_kind(key: &str) -> Option<char> {
    if key == "global" {
        return Some('g');
    }
    if key.len() > 200 || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (k, rest) = key.split_once(':')?;
    match k {
        "m" if typed_id(rest, &["column", "beam", "brace"]) => Some('m'),
        "p" if typed_id(rest, &["wall", "slab"]) => Some('p'),
        "n" if !rest.is_empty() => Some('n'),
        "s" if !rest.is_empty() => Some('s'),
        _ => None,
    }
}

/// `<kind>:<id>` with a kind of `kinds` and a positive id.
fn typed_id(rest: &str, kinds: &[&str]) -> bool {
    rest.split_once(':')
        .is_some_and(|(t, id)| kinds.contains(&t) && id.parse::<i64>().is_ok_and(|i| i > 0))
}

fn mask(v: &J, ok: &str, field: &str) -> Result<String, Error> {
    match v.as_str() {
        Some(s) if s.len() == 6 && s.chars().all(|c| ok.contains(c)) => Ok(s.to_string()),
        _ => bad(field, &format!("expected 6 letters of {ok} (ux uy uz rx ry rz)")),
    }
}

fn springs(v: Option<&J>, field: &str) -> Result<(), Error> {
    let a = v.and_then(J::as_array).filter(|a| a.len() == 6);
    let Some(a) = a else {
        return bad(field, "expected 6 spring values");
    };
    if a.iter().all(|x| finite(x).is_some_and(|f| f >= 0.0)) {
        Ok(())
    } else {
        bad(field, "springs must be finite and not negative")
    }
}

fn positive(o: &J, k: &str, field: &str) -> Result<(), Error> {
    if num(o, k, field)? > 0.0 {
        Ok(())
    } else {
        bad(field, &format!("'{k}' must be above zero"))
    }
}

/// Validation of one override (design 2.1): the same function for UI, XLSX import, AI and MCP.
pub fn check_override(db: &Connection, o: &AnOverride) -> Result<(), Error> {
    let Some(kind) = key_kind(&o.target) else {
        return bad("target", "not a stable key");
    };
    check_src(&o.src)?;
    let f = o.field.as_str();
    let v = &o.value;
    match (kind, f) {
        ('m', "ecc_start" | "ecc_end") => {
            one_of(v.get("frame").and_then(J::as_str).unwrap_or(""), &["global", "local"], f)?;
            if len3(vec3(v.get("d"), f)?) > 10.0 {
                return bad(f, "offset longer than 10 m");
            }
        }
        ('m', "release_start" | "release_end") => {
            let m = mask(v.get("mask").unwrap_or(&J::Null), "FPS", f)?;
            if m.contains('S') || v.get("k").is_some() {
                springs(v.get("k"), f)?;
            }
        }
        ('m', "section") => {
            if let Some(t) = v.get("type") {
                let Some(id) = t.as_i64().filter(|i| *i > 0) else {
                    return bad(f, "'type' must be a member type id");
                };
                let n: i64 =
                    db.query_row("SELECT count(*) FROM member_type WHERE id=?1", [id], |r| {
                        r.get(0)
                    })?;
                if n == 0 {
                    return bad(f, "no such member type");
                }
            } else {
                for k in ["A", "Iy", "Iz", "J"] {
                    positive(v, k, f)?;
                }
            }
        }
        ('m', "material") => {
            let name = v.get("name").and_then(J::as_str).unwrap_or("");
            if crate::materials::row_id(db, name)?.is_none() {
                return bad(f, "no such material");
            }
        }
        ('m', "role") => one_of(v.as_str().unwrap_or(""), &["frame", "truss", "excluded"], f)?,
        ('m', "roll") => {
            num(v, "deg", f)?;
        }
        ('m', "buckling") => {
            let mut any = false;
            for k in ["ky", "kz", "Ly", "Lz"] {
                if let Some(x) = opt_num(v, k, f)? {
                    if x <= 0.0 {
                        return bad(f, &format!("'{k}' must be above zero"));
                    }
                    any = true;
                }
            }
            if !any {
                return bad(f, "set at least one of ky, kz, Ly, Lz");
            }
        }
        ('n', "move") => {
            if len3(vec3(v.get("d"), f)?) > 10.0 {
                return bad(f, "move longer than 10 m");
            }
        }
        ('n', "merge_with") => {
            if !v.as_str().is_some_and(|k| key_kind(k) == Some('n') && k != o.target) {
                return bad(f, "expected another node key");
            }
        }
        ('n', "split") => {
            if v != &J::Bool(true) {
                return bad(f, "expected true");
            }
        }
        ('s', "dofs") => {
            mask(v, "FP", f)?;
        }
        ('s', "springs") => springs(Some(v), f)?,
        ('s', "subgrade") => positive(v, "ks", f)?,
        ('s', "enabled") => {
            if v != &J::Bool(false) {
                return bad(f, "expected false");
            }
        }
        ('p', "role") => one_of(
            v.as_str().unwrap_or(""),
            &["load_only", "line_support", "shell", "plate", "membrane", "excluded"],
            f,
        )?,
        ('p', "mesh") => {
            let h = num(v, "h", f)?;
            if !(h > 0.0 && h <= 100.0) {
                return bad(f, "'h' must be above 0 and at most 100 m");
            }
        }
        ('p', "span") => {
            if !matches!(v.get("way").and_then(J::as_i64), Some(1 | 2)) {
                return bad(f, "'way' must be 1 or 2");
            }
            opt_num(v, "deg", f)?;
        }
        ('g', "merge" | "snap") => {
            if !finite(v).is_some_and(|x| x > 0.0 && x <= 1.0) {
                return bad(f, "a tolerance above 0 and at most 1 m");
            }
        }
        _ => return bad("field", &format!("'{f}' does not apply to {}", o.target)),
    }
    if o.anchor.as_ref().is_some_and(|a| !a.is_object()) {
        return bad("anchor", "expected an object");
    }
    Ok(())
}

fn check_dir(dir: &str) -> Result<(), Error> {
    const AXES: [&str; 12] =
        ["x", "y", "z", "-x", "-y", "-z", "lx", "ly", "lz", "-lx", "-ly", "-lz"];
    if AXES.contains(&dir) {
        return Ok(());
    }
    let Ok(v) = serde_json::from_str::<J>(dir) else {
        return bad("dir", "an axis or a non-zero vector [x, y, z]");
    };
    if len3(vec3(Some(&v), "dir")?) > 1e-12 {
        Ok(())
    } else {
        bad("dir", "an axis or a non-zero vector [x, y, z]")
    }
}

fn check_load_target(t: &str) -> Result<(), Error> {
    if let Some(q) = t.strip_prefix("filter:") {
        return if q.trim().is_empty() {
            bad("target", "empty filter")
        } else {
            Ok(())
        };
    }
    if key_kind(t).is_some_and(|k| k != 'g') {
        return Ok(());
    }
    match serde_json::from_str::<J>(t) {
        Ok(J::Array(a))
            if !a.is_empty()
                && a.iter().all(|k| k.as_str().and_then(key_kind).is_some_and(|c| c != 'g')) =>
        {
            Ok(())
        }
        _ => bad("target", "a stable key, a list of keys or filter:<query>"),
    }
}

/// Validation of one load: case, kind, target, frame, direction and value by kind.
pub fn check_load(db: &Connection, l: &Load) -> Result<(), Error> {
    check_src(&l.src)?;
    if !exists(db, "load_case", l.lcase)? {
        return bad("lcase", "no such load case");
    }
    one_of(&l.kind, &LOAD_KINDS, "kind")?;
    one_of(&l.frame, &LOAD_FRAMES, "frame")?;
    check_load_target(&l.target)?;
    if l.kind != "temp" {
        check_dir(&l.dir)?;
    }
    let v = &l.value;
    match l.kind.as_str() {
        "point" => {
            num(v, "f", "value")?;
            if opt_num(v, "x", "value")?.is_some_and(|x| x < 0.0) {
                return bad("value", "'x' must not be negative");
            }
        }
        "line" => {
            num(v, "q1", "value")?;
            opt_num(v, "q2", "value")?;
            let x1 = opt_num(v, "x1", "value")?;
            let x2 = opt_num(v, "x2", "value")?;
            if x1.is_some_and(|x| x < 0.0) || x2.is_some_and(|x| x < 0.0) {
                return bad("value", "'x1' and 'x2' must not be negative");
            }
            if let (Some(a), Some(b)) = (x1, x2) {
                if b < a {
                    return bad("value", "'x2' before 'x1'");
                }
            }
        }
        "area" => {
            num(v, "p", "value")?;
        }
        _ => {
            num(v, "dt", "value")?;
        }
    }
    Ok(())
}

fn check_case(db: &Connection, c: &LoadCase, id: i64) -> Result<(), Error> {
    check_name(&c.name)?;
    check_src(&c.src)?;
    one_of(&c.category, &LOAD_CATEGORIES, "category")?;
    if name_taken(db, "load_case", &c.name, id)? {
        return bad("name", "a load case with this name exists");
    }
    Ok(())
}

fn check_combo(db: &Connection, c: &LoadCombo, id: i64) -> Result<(), Error> {
    check_name(&c.name)?;
    check_src(&c.src)?;
    one_of(&c.state, &COMBO_STATES, "state")?;
    if name_taken(db, "load_combo", &c.name, id)? {
        return bad("name", "a combination with this name exists");
    }
    if c.terms.is_empty() {
        return bad("terms", "at least one load case");
    }
    for (i, (lc, f)) in c.terms.iter().enumerate() {
        if !f.is_finite() {
            return bad("terms", "factors must be finite");
        }
        if c.terms[..i].iter().any(|t| t.0 == *lc) {
            return bad("terms", "a load case is listed twice");
        }
        if !exists(db, "load_case", *lc)? {
            return bad("terms", "no such load case");
        }
    }
    Ok(())
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    let n: i64 =
        db.query_row(&format!("SELECT count(*) FROM {table} WHERE id=?1"), [id], |r| r.get(0))?;
    Ok(n > 0)
}

fn name_taken(db: &Connection, table: &str, name: &str, id: i64) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE name=?1 AND id<>?2"),
        params![name, id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// A locked row is changed only by a manual edit; a missing row is an error.
fn guard(db: &Connection, table: &str, id: i64, by: &str) -> Result<(), Error> {
    let locked: Option<i64> = db
        .query_row(&format!("SELECT locked FROM {table} WHERE id=?1"), [id], |r| r.get(0))
        .optional()?;
    match locked {
        None => bad("id", &format!("no {table} {id}")),
        Some(1) if by != "manual" => bad("locked", &format!("{table} {id} is locked")),
        _ => Ok(()),
    }
}

fn opt_json(v: &Option<J>) -> Option<String> {
    v.as_ref().map(J::to_string)
}

fn read_override(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnOverride> {
    let anchor: Option<String> = r.get(4)?;
    Ok(AnOverride {
        id: r.get(0)?,
        target: r.get(1)?,
        field: r.get(2)?,
        value: js(&r.get::<_, String>(3)?),
        anchor: anchor.as_deref().map(js),
        locked: r.get::<_, i64>(5)? != 0,
        src: r.get(6)?,
        why: r.get(7)?,
    })
}

fn read_case(r: &rusqlite::Row<'_>) -> rusqlite::Result<LoadCase> {
    Ok(LoadCase {
        id: r.get(0)?,
        name: r.get(1)?,
        category: r.get(2)?,
        self_weight: r.get::<_, i64>(3)? != 0,
        note: r.get(4)?,
        locked: r.get::<_, i64>(5)? != 0,
        src: r.get(6)?,
    })
}

fn read_load(r: &rusqlite::Row<'_>) -> rusqlite::Result<Load> {
    Ok(Load {
        id: r.get(0)?,
        lcase: r.get(1)?,
        kind: r.get(2)?,
        target: r.get(3)?,
        frame: r.get(4)?,
        dir: r.get(5)?,
        value: js(&r.get::<_, String>(6)?),
        grp: r.get(7)?,
        locked: r.get::<_, i64>(8)? != 0,
        src: r.get(9)?,
        why: r.get(10)?,
    })
}

fn read_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<CalcRun> {
    Ok(CalcRun {
        id: r.get(0)?,
        created: r.get(1)?,
        input_hash: r.get(2)?,
        fem_version: r.get(3)?,
        status: r.get(4)?,
        stats: js(&r.get::<_, String>(5)?),
        pinned: r.get::<_, i64>(6)? != 0,
        src: r.get(7)?,
        note: r.get(8)?,
    })
}

const OVERRIDE_COLS: &str = "id,target,field,value,anchor,locked,src,why";
const CASE_COLS: &str = "id,name,category,self_weight,note,locked,src";
const LOAD_COLS: &str = "id,lcase,kind,target,frame,dir,value,grp,locked,src,why";
const RUN_COLS: &str = "id,created,input_hash,fem_version,status,stats,pinned,src,note";

fn terms_of(db: &Connection, combo: i64) -> Result<Vec<(i64, f64)>, Error> {
    let mut st = db.prepare_cached(
        "SELECT lcase, factor FROM load_combo_term WHERE combo=?1 ORDER BY lcase",
    )?;
    let v = st.query_map([combo], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(v.collect::<Result<_, _>>()?)
}

fn write_terms(db: &Connection, combo: i64, terms: &[(i64, f64)]) -> Result<(), Error> {
    for (lc, f) in terms {
        db.execute(
            "INSERT INTO load_combo_term(combo, lcase, factor) VALUES (?1, ?2, ?3)",
            params![combo, lc, f],
        )?;
    }
    Ok(())
}

fn check_part(p: &str) -> Result<(), Error> {
    let ok = !p.is_empty()
        && p.len() <= 32
        && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        bad("part", "1 to 32 of a-z, 0-9, _")
    }
}

/// Copies run `id` with its blobs into its own archive file, checks the copy, then removes it
/// from the project. Any failure leaves the run in the project.
fn archive_run(db: &Connection, dir: &Path, id: i64, created: i64) -> Result<PathBuf, Error> {
    let file = dir.join(format!("run-{id}-{created}.sqlite"));
    let row: Vec<Value> = db.query_row(
        &format!("SELECT {RUN_COLS} FROM calc_run WHERE id=?1"),
        [id],
        |r| (0..9).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>(),
    )?;
    let blobs: Vec<(String, Vec<u8>)> = {
        let mut st = db.prepare("SELECT part, data FROM calc_blob WHERE run=?1 ORDER BY part")?;
        let v = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        v.collect::<Result<_, _>>()?
    };
    let mut arch = Connection::open(&file)?;
    let tx = arch.transaction()?;
    tx.execute_batch(RUN_DDL)?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO calc_run({RUN_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"),
        params_from_iter(row.iter()),
    )?;
    for (p, d) in &blobs {
        tx.execute(
            "INSERT OR REPLACE INTO calc_blob(run, part, data) VALUES (?1, ?2, ?3)",
            params![id, p, d],
        )?;
    }
    tx.commit()?;
    let n: i64 =
        arch.query_row("SELECT count(*) FROM calc_blob WHERE run=?1", [id], |r| r.get(0))?;
    let runs: i64 =
        arch.query_row("SELECT count(*) FROM calc_run WHERE id=?1", [id], |r| r.get(0))?;
    drop(arch);
    if n != blobs.len() as i64 || runs != 1 {
        return Err(Error::Invalid(format!("calc archive: run {id} copy is incomplete")));
    }
    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM calc_blob WHERE run=?1", [id])?;
    tx.execute("DELETE FROM calc_run WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(file)
}

impl Model {
    // ---- analytical overrides ----

    pub fn an_overrides(&self) -> Result<Vec<AnOverride>, Error> {
        let mut st = self
            .db
            .prepare(&format!("SELECT {OVERRIDE_COLS} FROM an_override ORDER BY id"))?;
        let v = st.query_map([], read_override)?;
        Ok(v.collect::<Result<_, _>>()?)
    }

    /// Sets the override of (`target`, `field`), creating or replacing it; one undo step.
    pub fn set_an_override(&mut self, o: &AnOverride) -> Result<i64, Error> {
        check_override(&self.db, o)?;
        let o = o.clone();
        self.op("an_override", move |db| {
            let had: Option<i64> = db
                .query_row(
                    "SELECT id FROM an_override WHERE target=?1 AND field=?2",
                    params![o.target, o.field],
                    |r| r.get(0),
                )
                .optional()?;
            match had {
                Some(id) => {
                    guard(db, "an_override", id, &o.src)?;
                    let inv = Inv::default().restore(db, "an_override", "id", id)?;
                    db.execute(
                        "UPDATE an_override SET target=?1, field=?2, value=?3, anchor=?4, \
                         locked=?5, src=?6, why=?7 WHERE id=?8",
                        params![
                            o.target,
                            o.field,
                            o.value.to_string(),
                            opt_json(&o.anchor),
                            i64::from(o.locked),
                            o.src,
                            o.why,
                            id
                        ],
                    )?;
                    Ok((id, 0, inv))
                }
                None => {
                    db.execute(
                        "INSERT INTO an_override(target, field, value, anchor, locked, src, why) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            o.target,
                            o.field,
                            o.value.to_string(),
                            opt_json(&o.anchor),
                            i64::from(o.locked),
                            o.src,
                            o.why
                        ],
                    )?;
                    let id = db.last_insert_rowid();
                    Ok((id, 0, Inv::default().delete("an_override", "id", id)))
                }
            }
        })
    }

    /// Removes an override; `by` is the actor's `src` (a locked row needs `manual`).
    pub fn delete_an_override(&mut self, id: i64, by: &str) -> Result<(), Error> {
        let by = by.to_string();
        self.op("an_override_delete", move |db| {
            guard(db, "an_override", id, &by)?;
            let inv = Inv::default().restore(db, "an_override", "id", id)?;
            db.execute("DELETE FROM an_override WHERE id=?1", [id])?;
            Ok(((), 0, inv))
        })
    }

    /// Locks or unlocks a row of `an_override`, `load_case`, `load` or `load_combo`; only a
    /// manual gesture unlocks.
    pub fn set_calc_locked(
        &mut self,
        table: &str,
        id: i64,
        locked: bool,
        by: &str,
    ) -> Result<(), Error> {
        if !matches!(table, "an_override" | "load_case" | "load" | "load_combo") {
            return bad("table", "has no lock");
        }
        if !locked && by != "manual" {
            return bad("locked", "only a manual edit unlocks");
        }
        check_src(by)?;
        let (table, by) = (table.to_string(), by.to_string());
        self.op("calc_lock", move |db| {
            guard(db, &table, id, if locked { "manual" } else { by.as_str() })?;
            let inv = Inv::default().restore(db, &table, "id", id)?;
            db.execute(
                &format!("UPDATE {table} SET locked=?1 WHERE id=?2"),
                params![i64::from(locked), id],
            )?;
            Ok(((), 0, inv))
        })
    }

    // ---- load cases ----

    pub fn load_cases(&self) -> Result<Vec<LoadCase>, Error> {
        let mut st = self.db.prepare(&format!("SELECT {CASE_COLS} FROM load_case ORDER BY id"))?;
        let v = st.query_map([], read_case)?;
        Ok(v.collect::<Result<_, _>>()?)
    }

    pub fn add_load_case(&mut self, c: &LoadCase) -> Result<i64, Error> {
        check_case(&self.db, c, 0)?;
        let c = c.clone();
        self.op("load_case_add", move |db| {
            db.execute(
                "INSERT INTO load_case(name, category, self_weight, note, locked, src) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    c.name,
                    c.category,
                    i64::from(c.self_weight),
                    c.note,
                    i64::from(c.locked),
                    c.src
                ],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, 0, Inv::default().delete("load_case", "id", id)))
        })
    }

    pub fn set_load_case(&mut self, id: i64, c: &LoadCase) -> Result<(), Error> {
        check_case(&self.db, c, id)?;
        let c = c.clone();
        self.op("load_case_edit", move |db| {
            guard(db, "load_case", id, &c.src)?;
            let inv = Inv::default().restore(db, "load_case", "id", id)?;
            db.execute(
                "UPDATE load_case SET name=?1, category=?2, self_weight=?3, note=?4, locked=?5, \
                 src=?6 WHERE id=?7",
                params![
                    c.name,
                    c.category,
                    i64::from(c.self_weight),
                    c.note,
                    i64::from(c.locked),
                    c.src,
                    id
                ],
            )?;
            Ok(((), 0, inv))
        })
    }

    /// Deletes a case with its loads and combination terms in one undo step; returns the
    /// counts of (loads, terms) removed with it. Automation is refused when the case, one of
    /// its loads or a combination using it is locked.
    pub fn delete_load_case(&mut self, id: i64, by: &str) -> Result<(usize, usize), Error> {
        let by = by.to_string();
        self.op("load_case_delete", move |db| {
            guard(db, "load_case", id, &by)?;
            if by != "manual" {
                let held: i64 = db.query_row(
                    "SELECT (SELECT count(*) FROM load WHERE lcase=?1 AND locked=1) + \
                     (SELECT count(*) FROM load_combo c JOIN load_combo_term t ON t.combo=c.id \
                      WHERE t.lcase=?1 AND c.locked=1)",
                    [id],
                    |r| r.get(0),
                )?;
                if held > 0 {
                    return bad("locked", "locked loads or combinations use this case");
                }
            }
            let inv = Inv::default()
                .restore(db, "load", "lcase", id)?
                .restore(db, "load_combo_term", "lcase", id)?
                .restore(db, "load_case", "id", id)?;
            let loads = db.execute("DELETE FROM load WHERE lcase=?1", [id])?;
            let terms = db.execute("DELETE FROM load_combo_term WHERE lcase=?1", [id])?;
            db.execute("DELETE FROM load_case WHERE id=?1", [id])?;
            Ok(((loads, terms), 0, inv))
        })
    }

    // ---- loads ----

    /// Loads of one case, or all with `None`.
    pub fn loads(&self, lcase: Option<i64>) -> Result<Vec<Load>, Error> {
        let mut st = self.db.prepare(&format!(
            "SELECT {LOAD_COLS} FROM load WHERE ?1 IS NULL OR lcase=?1 ORDER BY id"
        ))?;
        let v = st.query_map([lcase], read_load)?;
        Ok(v.collect::<Result<_, _>>()?)
    }

    pub fn add_load(&mut self, l: &Load) -> Result<i64, Error> {
        check_load(&self.db, l)?;
        let l = l.clone();
        self.op("load_add", move |db| {
            db.execute(
                "INSERT INTO load(lcase, kind, target, frame, dir, value, grp, locked, src, why) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    l.lcase,
                    l.kind,
                    l.target,
                    l.frame,
                    l.dir,
                    l.value.to_string(),
                    l.grp,
                    i64::from(l.locked),
                    l.src,
                    l.why
                ],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, 0, Inv::default().delete("load", "id", id)))
        })
    }

    pub fn set_load(&mut self, id: i64, l: &Load) -> Result<(), Error> {
        check_load(&self.db, l)?;
        let l = l.clone();
        self.op("load_edit", move |db| {
            guard(db, "load", id, &l.src)?;
            let inv = Inv::default().restore(db, "load", "id", id)?;
            db.execute(
                "UPDATE load SET lcase=?1, kind=?2, target=?3, frame=?4, dir=?5, value=?6, \
                 grp=?7, locked=?8, src=?9, why=?10 WHERE id=?11",
                params![
                    l.lcase,
                    l.kind,
                    l.target,
                    l.frame,
                    l.dir,
                    l.value.to_string(),
                    l.grp,
                    i64::from(l.locked),
                    l.src,
                    l.why,
                    id
                ],
            )?;
            Ok(((), 0, inv))
        })
    }

    pub fn delete_load(&mut self, id: i64, by: &str) -> Result<(), Error> {
        let by = by.to_string();
        self.op("load_delete", move |db| {
            guard(db, "load", id, &by)?;
            let inv = Inv::default().restore(db, "load", "id", id)?;
            db.execute("DELETE FROM load WHERE id=?1", [id])?;
            Ok(((), 0, inv))
        })
    }

    // ---- combinations ----

    pub fn load_combos(&self) -> Result<Vec<LoadCombo>, Error> {
        let mut st = self.db.prepare(
            "SELECT id, name, state, generated, rule, locked, src FROM load_combo ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(LoadCombo {
                id: r.get(0)?,
                name: r.get(1)?,
                state: r.get(2)?,
                generated: r.get::<_, i64>(3)? != 0,
                rule: r.get(4)?,
                locked: r.get::<_, i64>(5)? != 0,
                src: r.get(6)?,
                terms: vec![],
            })
        })?;
        let mut out: Vec<LoadCombo> = rows.collect::<Result<_, _>>()?;
        for c in &mut out {
            c.terms = terms_of(&self.db, c.id)?;
        }
        Ok(out)
    }

    pub fn add_load_combo(&mut self, c: &LoadCombo) -> Result<i64, Error> {
        check_combo(&self.db, c, 0)?;
        let c = c.clone();
        self.op("load_combo_add", move |db| {
            db.execute(
                "INSERT INTO load_combo(name, state, generated, rule, locked, src) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    c.name,
                    c.state,
                    i64::from(c.generated),
                    c.rule,
                    i64::from(c.locked),
                    c.src
                ],
            )?;
            let id = db.last_insert_rowid();
            write_terms(db, id, &c.terms)?;
            let inv = Inv::default()
                .delete("load_combo_term", "combo", id)
                .delete("load_combo", "id", id);
            Ok((id, 0, inv))
        })
    }

    /// Replaces a combination with its terms; one undo step.
    pub fn set_load_combo(&mut self, id: i64, c: &LoadCombo) -> Result<(), Error> {
        check_combo(&self.db, c, id)?;
        let c = c.clone();
        self.op("load_combo_edit", move |db| {
            guard(db, "load_combo", id, &c.src)?;
            // undo runs in order: drop the new terms, then put the old rows back
            let inv = Inv::default()
                .delete("load_combo_term", "combo", id)
                .restore(db, "load_combo_term", "combo", id)?
                .restore(db, "load_combo", "id", id)?;
            db.execute(
                "UPDATE load_combo SET name=?1, state=?2, generated=?3, rule=?4, locked=?5, \
                 src=?6 WHERE id=?7",
                params![
                    c.name,
                    c.state,
                    i64::from(c.generated),
                    c.rule,
                    i64::from(c.locked),
                    c.src,
                    id
                ],
            )?;
            db.execute("DELETE FROM load_combo_term WHERE combo=?1", [id])?;
            write_terms(db, id, &c.terms)?;
            Ok(((), 0, inv))
        })
    }

    pub fn delete_load_combo(&mut self, id: i64, by: &str) -> Result<(), Error> {
        let by = by.to_string();
        self.op("load_combo_delete", move |db| {
            guard(db, "load_combo", id, &by)?;
            let inv = Inv::default()
                .restore(db, "load_combo_term", "combo", id)?
                .restore(db, "load_combo", "id", id)?;
            db.execute("DELETE FROM load_combo_term WHERE combo=?1", [id])?;
            db.execute("DELETE FROM load_combo WHERE id=?1", [id])?;
            Ok(((), 0, inv))
        })
    }

    // ---- settings ----

    pub fn calc_setting(&self, key: &str) -> Result<Option<J>, Error> {
        let v: Option<String> = self
            .db
            .query_row("SELECT value FROM calc_settings WHERE key=?1", [key], |r| r.get(0))
            .optional()?;
        Ok(v.as_deref().map(js))
    }

    /// Sets (`Some`) or clears (`None`) one setting; one undo step.
    pub fn set_calc_setting(&mut self, key: &str, value: Option<&J>) -> Result<(), Error> {
        let ok = !key.is_empty()
            && key.len() <= 64
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if !ok {
            return bad("key", "1 to 64 of A-Z, a-z, 0-9, _ and .");
        }
        let (key, value) = (key.to_string(), value.map(J::to_string));
        self.op("calc_settings", move |db| {
            let k = [Value::Text(key.clone())];
            let inv = Inv::default()
                .sql("DELETE FROM calc_settings WHERE key=?1", k.to_vec())
                .restore_where(db, "calc_settings", "key=?1", &k)?;
            match &value {
                Some(v) => db.execute(
                    "INSERT INTO calc_settings(key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, v],
                )?,
                None => db.execute("DELETE FROM calc_settings WHERE key=?1", [&key])?,
            };
            Ok(((), 0, inv))
        })
    }

    // ---- material mechanics ----

    /// Mechanical properties by material name; a name without a row has none set.
    pub fn material_mech(&self, name: &str) -> Result<MatMech, Error> {
        let row = self
            .db
            .query_row(
                "SELECT e_mod, g_mod, poisson, fy, alpha_t FROM material WHERE name=?1",
                [name],
                |r| {
                    Ok(MatMech {
                        e: r.get(0)?,
                        g: r.get(1)?,
                        nu: r.get(2)?,
                        fy: r.get(3)?,
                        alpha: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    /// Sets the mechanical properties of a material (a text-only material gets its row);
    /// one undo step.
    pub fn set_material_mech(&mut self, name: &str, m: &MatMech) -> Result<(), Error> {
        let pos = |v: Option<f64>| v.is_none_or(|x| x.is_finite() && x > 0.0);
        if !pos(m.e) || !pos(m.g) || !pos(m.fy) {
            return bad("material", "E, G and fy must be finite and above zero");
        }
        if !m.nu.is_none_or(|x| (0.0..0.5).contains(&x)) {
            return bad("material", "Poisson ratio from 0 to below 0.5");
        }
        if !m.alpha.is_none_or(|x| x.is_finite() && x.abs() < 1e-3) {
            return bad("material", "thermal expansion must be finite and below 0.001 1/K");
        }
        let (name, m) = (name.to_string(), *m);
        self.op("material_mech", move |db| {
            let (id, inv) = match crate::materials::row_id(db, &name)? {
                Some(id) => (id, Inv::default().restore(db, "material", "id", id)?),
                None => {
                    if crate::materials::usage(db, &name)?.is_empty() {
                        return bad("material", &format!("no material '{name}'"));
                    }
                    let spec = crate::materials::material_defaults(&name);
                    crate::materials::insert_spec_ignore(db, &spec)?;
                    let id = db.last_insert_rowid();
                    (id, Inv::default().delete("material", "id", id))
                }
            };
            db.execute(
                "UPDATE material SET e_mod=?1, g_mod=?2, poisson=?3, fy=?4, alpha_t=?5 \
                 WHERE id=?6",
                params![m.e, m.g, m.nu, m.fy, m.alpha, id],
            )?;
            Ok(((), id, inv))
        })
    }

    // ---- runs: outside the undo journal ----

    /// Stores a run with its blobs (`input`, `nodes`, `reactions`, `members`, `design`) in
    /// one transaction, outside undo. `id` and `created` of `r` are ignored. Call
    /// [`Model::retain_calc_runs`] afterwards to move older runs to the archive.
    pub fn add_calc_run(&self, r: &CalcRun, parts: &[(&str, &[u8])]) -> Result<i64, Error> {
        one_of(&r.status, &RUN_STATUSES, "status")?;
        check_src(&r.src)?;
        if r.input_hash.trim().is_empty() || r.fem_version.trim().is_empty() {
            return bad("run", "input hash and solver version are required");
        }
        for (i, (p, _)) in parts.iter().enumerate() {
            check_part(p)?;
            if parts[..i].iter().any(|q| q.0 == *p) {
                return bad("part", "listed twice");
            }
        }
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO calc_run(created, input_hash, fem_version, status, stats, pinned, src, \
             note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now_s(),
                r.input_hash,
                r.fem_version,
                r.status,
                r.stats.to_string(),
                i64::from(r.pinned),
                r.src,
                r.note
            ],
        )?;
        let id = tx.last_insert_rowid();
        for (p, d) in parts {
            tx.execute(
                "INSERT INTO calc_blob(run, part, data) VALUES (?1, ?2, ?3)",
                params![id, p, d],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Runs in the project, newest first.
    pub fn calc_runs(&self) -> Result<Vec<CalcRun>, Error> {
        let mut st = self
            .db
            .prepare(&format!("SELECT {RUN_COLS} FROM calc_run ORDER BY id DESC"))?;
        let v = st.query_map([], read_run)?;
        Ok(v.collect::<Result<_, _>>()?)
    }

    pub fn calc_run(&self, id: i64) -> Result<Option<CalcRun>, Error> {
        Ok(self
            .db
            .query_row(&format!("SELECT {RUN_COLS} FROM calc_run WHERE id=?1"), [id], read_run)
            .optional()?)
    }

    /// Newest run whose input hash is `hash`: the results that are current for that input.
    pub fn calc_run_for(&self, hash: &str) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT id FROM calc_run WHERE input_hash=?1 ORDER BY id DESC LIMIT 1",
                [hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn calc_blob(&self, run: i64, part: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT data FROM calc_blob WHERE run=?1 AND part=?2",
                params![run, part],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Pins a run (used by a report: never archived) or releases it; outside undo.
    pub fn set_calc_run_pinned(&self, id: i64, pinned: bool) -> Result<(), Error> {
        let n = self.db.execute(
            "UPDATE calc_run SET pinned=?1 WHERE id=?2",
            params![i64::from(pinned), id],
        )?;
        if n == 0 {
            return bad("id", &format!("no calc_run {id}"));
        }
        Ok(())
    }

    pub fn set_calc_run_status(&self, id: i64, status: &str) -> Result<(), Error> {
        one_of(status, &RUN_STATUSES, "status")?;
        let n = self
            .db
            .execute("UPDATE calc_run SET status=?1 WHERE id=?2", params![status, id])?;
        if n == 0 {
            return bad("id", &format!("no calc_run {id}"));
        }
        Ok(())
    }

    /// `<project>_archive/calc/` beside the project file; None for a model in memory.
    pub fn calc_archive_dir(&self) -> Option<PathBuf> {
        let p = PathBuf::from(self.db_path()?);
        let stem = p.file_stem()?.to_string_lossy().into_owned();
        Some(p.with_file_name(format!("{stem}_archive")).join("calc"))
    }

    /// Keeps the newest [`CALC_KEEP_UNPINNED`] unpinned runs; each older unpinned run moves
    /// to its own archive file. Pinned runs stay. A model in memory keeps every run. Returns
    /// the archive files written, oldest run first.
    pub fn retain_calc_runs(&self) -> Result<Vec<PathBuf>, Error> {
        let mut old: Vec<(i64, i64)> = {
            let mut st = self.db.prepare(
                "SELECT id, created FROM calc_run WHERE pinned=0 ORDER BY id DESC \
                 LIMIT -1 OFFSET ?1",
            )?;
            let v = st.query_map([CALC_KEEP_UNPINNED as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
            v.collect::<Result<_, _>>()?
        };
        if old.is_empty() {
            return Ok(vec![]);
        }
        let Some(dir) = self.calc_archive_dir() else {
            return Ok(vec![]);
        };
        std::fs::create_dir_all(&dir)?;
        old.reverse();
        let mut out = vec![];
        for (id, created) in old {
            out.push(archive_run(&self.db, &dir, id, created)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "calc_store_tests.rs"]
mod tests;
