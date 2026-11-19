// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Type catalog: a "type × type parameters" table, CSV exchange. Header: `Type`, then
//! `Name (unit)` such as `Width (mm)`, `Angle (deg)`, `Area (m2)`; a length without a unit uses the
//! project unit (with a warning), anything else without a unit is SI or text. Delimiter `,`, `;` or
//! tab (`;` and tab allow a decimal comma), quoting per RFC 4180, a UTF-8 byte order mark at the
//! start is skipped. A cell with a unit (`1.2m`) is read as is.
//! Columns are type parameters without formulas only. Import is a plan (`plan_import`): an empty cell
//! or a missing column keeps the current type value, a bad cell is not applied and
//! goes into the report; only changed and new types are written.

use super::expr::{Dim, ANGLE, AREA, LENGTH, NUMBER, VOLUME};
use super::{Family, FamilyType, Kind, Value};
use crate::csv::{self, Style};

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogError {
    /// CSV line, 1-based; 0 means the whole file.
    pub line: usize,
    pub msg: String,
}

/// Import result: types and warnings (header line, text).
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub types: Vec<FamilyType>,
    pub warnings: Vec<CatalogError>,
}

/// Reads the number part of a cell under the file's rule; Err is the reason, shown as is.
pub type NumRead<'a> = &'a dyn Fn(&str) -> Result<f64, String>;

/// One value a catalog row changes; `old` is the type's current value or the family default.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamChange {
    pub param: String,
    pub old: Value,
    pub new: Value,
}

/// A type the import adds or changes.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeChange {
    pub name: String,
    pub added: bool,
    pub params: Vec<ParamChange>,
}

/// Import plan: nothing is written until the caller applies `types`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    /// Full merged values of the added and changed types only, in file order.
    pub types: Vec<FamilyType>,
    pub changes: Vec<TypeChange>,
    /// Existing types in the file whose values stay as they are.
    pub unchanged: usize,
    /// Cells not applied (bad number, unit, yes/no): the old value stays.
    pub skipped: Vec<CatalogError>,
    pub warnings: Vec<CatalogError>,
}

impl Plan {
    /// Nothing to write.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// Changed values over all types.
    pub fn value_count(&self) -> usize {
        self.changes.iter().map(|c| c.params.len()).sum()
    }
}

/// Catalog CSV split into cells: separator, header and body rows (line numbers from 1).
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    sep: char,
    head: (usize, Vec<String>),
    body: Vec<(usize, Vec<String>)>,
}

impl Sheet {
    /// Comma-separated columns: the decimal mark can only be the point.
    pub fn comma_separated(&self) -> bool {
        self.sep == ','
    }

    /// Number parts (unit cut off) of the numeric columns: input of the file's mark vote.
    pub fn number_cells(&self, f: &Family) -> Vec<String> {
        let numeric: Vec<bool> = self
            .head
            .1
            .iter()
            .skip(1)
            .map(|h| {
                f.param(&header(h).0)
                    .is_some_and(|p| !p.kind.is_text() && p.kind != Kind::Bool)
            })
            .collect();
        self.body
            .iter()
            .flat_map(|(_, r)| r.iter().skip(1).zip(&numeric))
            .filter(|(_, n)| **n)
            .map(|(c, _)| split_unit(c).0.to_string())
            .collect()
    }
}

/// Splits the catalog text into cells (separator from the header line).
pub fn read_sheet(text: &str) -> Result<Sheet, CatalogError> {
    let text = strip_bom(text);
    let first = text.lines().next().unwrap_or("");
    // separators inside quoted header cells (`"Width, outer (mm)"`) do not count
    let mut inside = false;
    let first: String = first
        .chars()
        .filter(|&c| {
            if c == '"' {
                inside = !inside;
            }
            !inside && c != '"'
        })
        .collect();
    let sep = if first.contains('\t') {
        '\t'
    } else if first.contains(';') && !first.contains(',') {
        ';'
    } else {
        ','
    };
    let mut all = rows(text, sep)?;
    if all.is_empty() {
        return Err(CatalogError {
            line: 0,
            msg: "empty catalog".into(),
        });
    }
    let head = all.remove(0);
    Ok(Sheet {
        sep,
        head,
        body: all,
    })
}

/// Plain reader without a file mark: the point, and in `;` or tab files the comma as well.
pub fn plain_num(sheet: &Sheet) -> impl Fn(&str) -> Result<f64, String> {
    let comma = !sheet.comma_separated();
    move |s: &str| {
        let t = s.trim();
        let t = if comma {
            t.replace(',', ".")
        } else {
            t.to_string()
        };
        t.parse::<f64>()
            .map_err(|_| format!("'{}' is not a number", s.trim()))
    }
}

/// `450mm` -> (`450`, `mm`); without a unit the second part is empty.
fn split_unit(s: &str) -> (&str, &str) {
    let t = s.trim();
    let end = t
        .rfind(|c: char| c.is_ascii_digit() || c == '.' || c == ',')
        .map_or(0, |i| i + 1);
    (t[..end].trim_end(), t[end..].trim())
}

/// Digits with marks, spaces, sign or exponent only: text meant as a plain number.
fn is_number_text(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(
                    c,
                    '.' | ',' | '+' | '-' | 'e' | 'E' | ' ' | '\u{a0}' | '\u{202f}' | '\u{2009}'
                )
        })
}

/// Equal within the precision the catalog text carries.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
        _ => a == b,
    }
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

fn unit_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Length => "mm",
        Kind::Angle => "deg",
        Kind::Area => "m2",
        Kind::Volume => "m3",
        _ => "",
    }
}

fn unit_scale(u: &str) -> Option<(f64, Dim)> {
    Some(match u.trim().to_ascii_lowercase().as_str() {
        "mm" | "millimeters" => (1e-3, LENGTH),
        "cm" | "centimeters" => (1e-2, LENGTH),
        "m" | "meters" => (1.0, LENGTH),
        "in" | "inches" => (0.0254, LENGTH),
        "ft" | "feet" => (0.3048, LENGTH),
        "deg" | "degrees" | "°" => (std::f64::consts::PI / 180.0, ANGLE),
        "rad" | "radians" => (1.0, ANGLE),
        "m2" | "m²" => (1.0, AREA),
        "mm2" => (1e-6, AREA),
        "m3" | "m³" => (1.0, VOLUME),
        "" => (1.0, NUMBER),
        _ => return None,
    })
}

fn num_text(v: f64) -> String {
    let s = format!("{:.9}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

/// Type parameters without formulas are the catalog columns.
pub fn columns(f: &Family) -> Vec<&super::Param> {
    f.params
        .iter()
        .filter(|p| !p.instance && p.formula.is_none() && !f.ext.has_text_formula(&p.name))
        .collect()
}

/// Catalog table: header row, then one row per type (lengths in mm, angles in degrees).
fn table(f: &Family, types: &[FamilyType], st: Style) -> Vec<Vec<String>> {
    let cols = columns(f);
    let mut head = vec!["Type".to_string()];
    for p in &cols {
        let u = unit_of(p.kind);
        head.push(if u.is_empty() {
            p.name.clone()
        } else {
            format!("{} ({u})", p.name)
        });
    }
    let mut out = vec![head];
    for t in types {
        let mut row = vec![t.name.clone()];
        for p in &cols {
            let v = t.values.iter().find(|(k, _)| *k == p.name).map(|x| &x.1);
            row.push(match v {
                None => String::new(),
                Some(Value::Text(s)) => s.clone(),
                Some(Value::Num(x)) => {
                    let (k, _) = unit_scale(unit_of(p.kind)).unwrap_or((1.0, NUMBER));
                    st.mark(num_text(x / k))
                }
            });
        }
        out.push(row);
    }
    out
}

/// Catalog CSV as text for the editor box and the clipboard: `,` separated, LF, no byte order mark.
pub fn export_csv(f: &Family, types: &[FamilyType]) -> String {
    csv::join(table(f, types, Style::EN), ',', "\n")
}

/// Catalog as a .csv file in the language's style (byte order mark, CRLF; `;` and decimal comma
/// for Russian); [`import_csv`] reads it back.
pub fn export_csv_file(f: &Family, types: &[FamilyType], ru: bool) -> String {
    let st = Style::for_lang(ru);
    csv::file(table(f, types, st), st)
}

/// CSV lines into cells (quotes, doubled quotes, line breaks inside quotes).
fn rows(text: &str, sep: char) -> Result<Vec<(usize, Vec<String>)>, CatalogError> {
    let (mut out, mut row, mut cell) = (vec![], vec![], String::new());
    let (mut quoted, mut line, mut start) = (false, 1usize, 1usize);
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted => {
                if it.peek() == Some(&'"') {
                    it.next();
                    cell.push('"');
                } else {
                    quoted = false;
                }
            }
            '"' if cell.trim().is_empty() => {
                cell.clear();
                quoted = true;
            }
            '\n' if quoted => {
                line += 1;
                cell.push('\n');
            }
            c if c == sep && !quoted => row.push(std::mem::take(&mut cell)),
            '\r' if !quoted => {}
            '\n' => {
                row.push(std::mem::take(&mut cell));
                if row.iter().any(|c| !c.trim().is_empty()) {
                    out.push((start, std::mem::take(&mut row)));
                } else {
                    row.clear();
                }
                line += 1;
                start = line;
            }
            c => cell.push(c),
        }
    }
    if quoted {
        return Err(CatalogError {
            line: start,
            msg: "unterminated quote".into(),
        });
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        if row.iter().any(|c| !c.trim().is_empty()) {
            out.push((start, row));
        }
    }
    Ok(out)
}

/// Header `Name (unit)` / `Name [unit]` / `Name##kind##unit` → (name, unit).
fn header(h: &str) -> (String, Option<String>) {
    let h = h.trim();
    if let Some((n, rest)) = h.split_once("##") {
        let u = rest.rsplit("##").next().unwrap_or("").to_string();
        return (n.trim().to_string(), Some(u));
    }
    for (o, c) in [('(', ')'), ('[', ']')] {
        if let (Some(a), true) = (h.rfind(o), h.ends_with(c)) {
            return (
                h[..a].trim().to_string(),
                Some(h[a + 1..h.len() - 1].trim().to_string()),
            );
        }
    }
    (h.to_string(), None)
}

fn cell_value(
    kind: Kind,
    raw: &str,
    unit: Option<&str>,
    num: NumRead,
) -> Result<Value, String> {
    let s = raw.trim();
    if kind.is_text() {
        return Ok(Value::Text(csv::unguard(s).to_string()));
    }
    if kind == Kind::Bool {
        return match s.to_lowercase().as_str() {
            "1" | "yes" | "true" | "\u{434}\u{430}" | "y" => Ok(Value::Num(1.0)),
            "0" | "no" | "false" | "\u{43d}\u{435}\u{442}" | "n" => Ok(Value::Num(0.0)),
            o => Err(format!("expected yes/no, got '{o}'")),
        };
    }
    let want = kind.dim();
    let (n, cell_unit) = split_unit(s);
    // a unit in the cell overrides the header unit; an empty header unit means a dimensionless number
    let scale = if !is_number_text(n) {
        None
    } else if cell_unit.is_empty() {
        Some(match unit {
            Some(u) => (
                unit_scale(u).ok_or_else(|| format!("unknown unit '{u}'"))?,
                u.trim().is_empty(),
            ),
            None => ((1.0, want), false),
        })
    } else {
        unit_scale(cell_unit).map(|s| (s, false))
    };
    let Some(((k, d), bare)) = scale else {
        return super::constant(kind, s);
    };
    if d != want && !(d == NUMBER && bare) {
        return Err(format!("unit does not fit {}", kind.name()));
    }
    let v = num(n)? * k;
    if !v.is_finite() {
        return Err("not finite".into());
    }
    Ok(Value::Num(if kind == Kind::Int { v.round() } else { v }))
}

/// Strips the UTF-8 byte order mark (as spreadsheet editors write "CSV UTF-8").
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Types from CSV by family parameters, without current types (strict variant: a bad cell is an
/// error). `project_length` is the project length unit for length columns without a unit.
pub fn import_csv(f: &Family, text: &str, project_length: &str) -> Result<Import, CatalogError> {
    let sheet = read_sheet(text)?;
    let num = plain_num(&sheet);
    let plan = plan_import(f, &[], &sheet, project_length, &num)?;
    if let Some(e) = plan.skipped.into_iter().next() {
        return Err(e);
    }
    Ok(Import {
        types: plan.types,
        warnings: plan.warnings,
    })
}

/// Import plan into types `current`: a same-named type receives only the non-empty cells of the file
/// (other values stay), a new type is added, an unchanged one is not written. A bad cell
/// is not applied and goes into `skipped`. An unknown column, an instance parameter or a
/// formula, a duplicated type, or a type that fails to evaluate is an error for the whole file.
pub fn plan_import(
    f: &Family,
    current: &[FamilyType],
    sheet: &Sheet,
    project_length: &str,
    num: NumRead,
) -> Result<Plan, CatalogError> {
    if !unit_scale(project_length).is_some_and(|u| u.1 == LENGTH) {
        return Err(CatalogError {
            line: 0,
            msg: format!("unknown project length unit '{project_length}'"),
        });
    }
    let (hl, head) = (&sheet.head.0, &sheet.head.1);
    if head.is_empty() || !head[0].trim().eq_ignore_ascii_case("type") && !head[0].trim().is_empty()
    {
        return Err(CatalogError {
            line: *hl,
            msg: "first column must be 'Type'".into(),
        });
    }
    let mut cols = vec![];
    let mut warnings = vec![];
    for h in &head[1..] {
        let (name, mut unit) = header(h);
        let p = f.param(&name).ok_or_else(|| CatalogError {
            line: *hl,
            msg: format!("unknown parameter {name}"),
        })?;
        if p.instance {
            return Err(CatalogError {
                line: *hl,
                msg: format!("{name} is an instance parameter; the catalog holds type values"),
            });
        }
        if p.kind == Kind::Length && unit.is_none() {
            warnings.push(CatalogError {
                line: *hl,
                msg: format!("{name}: no unit in the header, read in {project_length}"),
            });
            unit = Some(project_length.to_string());
        }
        if p.formula.is_some() || f.ext.has_text_formula(&name) {
            return Err(CatalogError {
                line: *hl,
                msg: format!("{name} is a formula and cannot be set"),
            });
        }
        if cols
            .iter()
            .any(|c: &(&super::Param, Option<String>)| c.0.name == name)
        {
            return Err(CatalogError {
                line: *hl,
                msg: format!("column {name} twice"),
            });
        }
        cols.push((p, unit));
    }
    let mut plan = Plan {
        warnings,
        ..Plan::default()
    };
    let mut seen: Vec<String> = vec![];
    for (ln, row) in &sheet.body {
        let name = csv::unguard(row[0].trim()).to_string();
        let e = |msg: String| CatalogError { line: *ln, msg };
        if name.is_empty() {
            return Err(e("empty type name".into()));
        }
        if row.len() > cols.len() + 1 {
            return Err(e(format!(
                "{} cells, header has {}",
                row.len(),
                cols.len() + 1
            )));
        }
        // set_types writes a quote in a name as an apostrophe: this way the type is still found after import
        let key = name.replace('"', "'");
        let cur = current
            .iter()
            .find(|t| t.name.trim() == name)
            .or_else(|| current.iter().find(|t| t.name.trim() == key));
        // a matched type keeps its stored name, so the write updates it instead of adding a twin
        let name = cur.map_or(name, |t| t.name.clone());
        if seen.contains(&name) {
            return Err(e(format!("type {name} twice")));
        }
        seen.push(name.clone());
        let mut values = cur.map_or_else(Vec::new, |t| t.values.clone());
        let mut params = vec![];
        for (k, (p, unit)) in cols.iter().enumerate() {
            // no cell or an empty one: the current value stays
            let Some(raw) = row.get(k + 1) else { continue };
            if raw.trim().is_empty() {
                continue;
            }
            let new = match cell_value(p.kind, raw, unit.as_deref(), num) {
                Ok(v) => v,
                Err(m) => {
                    plan.skipped.push(e(format!("{name}, {}: {m}", p.name)));
                    continue;
                }
            };
            let slot = values.iter().position(|(n, _)| *n == p.name);
            let old = slot.map_or(&p.default, |i| &values[i].1).clone();
            if same(&old, &new) {
                // a new catalog row keeps what the file says, even when it equals the default
                if cur.is_none() && slot.is_none() {
                    values.push((p.name.clone(), new));
                }
                continue;
            }
            match slot {
                Some(i) => values[i].1 = new.clone(),
                None => values.push((p.name.clone(), new.clone())),
            }
            params.push(ParamChange {
                param: p.name.clone(),
                old,
                new,
            });
        }
        if cur.is_some() && params.is_empty() {
            plan.unchanged += 1;
            continue;
        }
        // every type to be written is evaluated: a formula or sketch error is reported with the line number
        f.resolve(&values, &[])
            .map_err(|err| e(format!("{name}: {err}")))?;
        plan.changes.push(TypeChange {
            name: name.clone(),
            added: cur.is_none(),
            params,
        });
        plan.types.push(FamilyType { name, values });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: &str = "family Box\nparam Width length type 600mm\nparam Depth length type 400mm\n\
param Mat material type \"Oak\"\nparam Area2 area type = Width * Depth\n\
param Shelf bool type 1\n\
extrude Body rect(0, 0, Width, Depth) z(0, 500mm)\n\
type \"Small\" Width=300mm, Mat=\"Ash\"\ntype \"Big\" Width=1200mm, Depth=600mm\n";

    #[test]
    fn csv_round_trip_and_units() {
        let f = Family::parse(F).unwrap();
        let csv = export_csv(&f, &f.types);
        assert!(
            csv.starts_with("Type,Width (mm),Depth (mm),Mat,Shelf\n"),
            "{csv}"
        );
        let back = import_csv(&f, &csv, "mm").unwrap();
        assert!(back.warnings.is_empty());
        let back = back.types;
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "Small");
        let w = back[1].values.iter().find(|v| v.0 == "Width").unwrap();
        assert!(matches!(w.1, Value::Num(x) if (x - 1.2).abs() < 1e-12));
        // semicolon, decimal comma, meters in the header, unit in the cell
        let t = import_csv(
            &f,
            "Type;Width [m];Depth (mm);Mat;Shelf\n\"A; B\";0,75;450mm;\"Steel \"\"X\"\"\";no\n",
            "mm",
        )
        .unwrap()
        .types;
        assert_eq!(t[0].name, "A; B");
        let get = |k: &str| t[0].values.iter().find(|v| v.0 == k).unwrap().1.clone();
        assert_eq!(get("Width"), Value::Num(0.75));
        assert!(matches!(get("Depth"), Value::Num(x) if (x - 0.45).abs() < 1e-12));
        assert_eq!(get("Mat"), Value::Text("Steel \"X\"".into()));
        assert_eq!(get("Shelf"), Value::Num(0.0));
    }

    #[test]
    fn csv_bom_tabs_project_unit_and_instance_columns() {
        let with_gap = F.replace(
            "type \"Small\"",
            "param Gap length instance 0mm\ntype \"Small\"",
        );
        let f = Family::parse(&with_gap).unwrap();
        // "CSV UTF-8": byte order mark before the header
        let t = import_csv(&f, "\u{feff}Type,Width (mm)\nA,700\n", "mm").unwrap();
        assert!(matches!(t.types[0].values[0].1, Value::Num(x) if (x - 0.7).abs() < 1e-12));
        // tab and decimal comma; a length without a unit uses the project unit
        let t = import_csv(&f, "Type\tWidth\tDepth (m)\nA\t750,5\t0,4\n", "mm").unwrap();
        assert_eq!(t.warnings.len(), 1);
        assert!(t.warnings[0].msg.contains("Width"), "{:?}", t.warnings);
        let get = |k: &str| {
            t.types[0]
                .values
                .iter()
                .find(|v| v.0 == k)
                .unwrap()
                .1
                .clone()
        };
        assert!(matches!(get("Width"), Value::Num(x) if (x - 0.7505).abs() < 1e-12));
        assert!(matches!(get("Depth"), Value::Num(x) if (x - 0.4).abs() < 1e-12));
        let t = import_csv(&f, "Type,Width\nA,30\n", "in").unwrap();
        assert!(matches!(t.types[0].values[0].1, Value::Num(x) if (x - 0.762).abs() < 1e-12));
        // an instance parameter is not a catalog column
        let e = import_csv(&f, "Type,Gap (mm)\nA,10\n", "mm").unwrap_err();
        assert!(e.msg.contains("instance"), "{e}");
        assert!(import_csv(&f, "Type,Width (mm)\nA,1\n", "parsec").is_err());
    }

    fn plan_of(f: &Family, cur: &[FamilyType], csv: &str) -> Plan {
        let sheet = read_sheet(csv).unwrap();
        let num = plain_num(&sheet);
        plan_import(f, cur, &sheet, "mm", &num).unwrap()
    }

    fn num_of(t: &FamilyType, k: &str) -> f64 {
        match t.values.iter().find(|v| v.0 == k).map(|v| &v.1) {
            Some(Value::Num(x)) => *x,
            _ => f64::NAN,
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn plan_keeps_missing_columns_and_empty_cells() {
        let f = Family::parse(F).unwrap();
        // Big: Width=1200mm, Depth=600mm; the file has Width only
        let p = plan_of(&f, &f.types, "Type,Width (mm)\nBig,1000\n");
        assert_eq!(p.types.len(), 1);
        assert!(near(num_of(&p.types[0], "Width"), 1.0));
        assert!(near(num_of(&p.types[0], "Depth"), 0.6), "{:?}", p.types);
        let c = &p.changes[0];
        assert!(!c.added && c.name == "Big" && c.params.len() == 1);
        assert_eq!(c.params[0].param, "Width");
        assert!(matches!(c.params[0].old, Value::Num(x) if near(x, 1.2)));
        assert!(matches!(c.params[0].new, Value::Num(x) if near(x, 1.0)));
        // empty cell and a short row keep the current value
        for csv in [
            "Type,Width (mm),Depth (mm)\nBig,1000,\n",
            "Type,Width (mm),Depth (mm)\nBig,1000\n",
            "Type,Width (mm),Depth (mm)\nBig,1000,\"  \"\n",
        ] {
            let p = plan_of(&f, &f.types, csv);
            assert!(near(num_of(&p.types[0], "Depth"), 0.6), "{csv}");
            assert_eq!(p.value_count(), 1, "{csv}");
        }
        // Small keeps Mat="Ash" when the Mat column is absent
        let p = plan_of(&f, &f.types, "Type,Width (mm)\nSmall,350\n");
        let mat = p.types[0].values.iter().find(|v| v.0 == "Mat").unwrap();
        assert_eq!(mat.1, Value::Text("Ash".into()));
    }

    #[test]
    fn plan_untouched_reimport_writes_nothing() {
        let f = Family::parse(F).unwrap();
        let p = plan_of(&f, &f.types, &export_csv(&f, &f.types));
        assert!(p.is_empty(), "{:?}", p.changes);
        assert_eq!((p.unchanged, p.value_count()), (2, 0));
        assert!(p.skipped.is_empty());
        // the family default typed into a type that inherits it is not a change
        let p = plan_of(&f, &f.types, "Type,Depth (mm)\nSmall,400\n");
        assert!(p.is_empty() && p.unchanged == 1);
    }

    #[test]
    fn plan_reports_bad_cells_and_adds_new_types() {
        let f = Family::parse(F).unwrap();
        let p = plan_of(
            &f,
            &f.types,
            "Type,Width (mm),Depth (mm),Shelf\nBig,12..5,700,maybe\nNew,500,,\n",
        );
        assert_eq!(p.skipped.len(), 2, "{:?}", p.skipped);
        assert_eq!(p.skipped[0].line, 2);
        assert!(p.skipped[0].msg.contains("Width"), "{}", p.skipped[0]);
        assert!(p.skipped[1].msg.contains("Shelf"), "{}", p.skipped[1]);
        // the bad cell is not applied: Width stays, the good Depth cell is applied
        assert!(near(num_of(&p.types[0], "Width"), 1.2));
        assert!(near(num_of(&p.types[0], "Depth"), 0.7));
        let new = &p.changes[1];
        assert!(new.added && new.name == "New");
        assert_eq!(p.types[1].values.len(), 1);
        // the strict import refuses the same file
        let e = import_csv(&f, "Type,Width (mm)\nBig,12..5\n", "mm").unwrap_err();
        assert!(e.msg.contains("Width"), "{e}");
    }

    #[test]
    fn plan_matches_quoted_names_and_keeps_the_stored_name() {
        let f = Family::parse(F).unwrap();
        let cur = vec![
            FamilyType {
                name: "Pipe 12' long".into(),
                values: vec![],
            },
            FamilyType {
                name: "Rod \"A\"".into(),
                values: vec![],
            },
        ];
        // the writer quotes `"` as `""`; the reader unquotes it back to the stored name
        let csv = export_csv(&f, &cur);
        assert!(csv.contains("\"Rod \"\"A\"\"\""), "{csv}");
        let p = plan_of(&f, &cur, &csv);
        assert!(p.is_empty() && p.unchanged == 2, "{:?}", p.changes);
        // `"` in the file finds the type stored with `'` and writes under that name
        let p = plan_of(&f, &cur, "Type,Width (mm)\n\"Pipe 12\"\" long\",700\n");
        assert_eq!(p.types.len(), 1);
        assert_eq!(p.types[0].name, "Pipe 12' long");
        assert!(!p.changes[0].added);
        // an exact name wins over the `'` key; both spellings of one type are refused
        let p = plan_of(&f, &cur, "Type,Width (mm)\n\"Rod \"\"A\"\"\",700\n");
        assert_eq!(p.types[0].name, "Rod \"A\"");
        let sheet =
            read_sheet("Type,Width (mm)\n\"Pipe 12\"\" long\",700\nPipe 12' long,800\n").unwrap();
        let e = plan_import(&f, &cur, &sheet, "mm", &plain_num(&sheet)).unwrap_err();
        assert!(e.msg.contains("twice") && e.line == 3, "{e}");
    }

    #[test]
    fn plan_reads_numbers_through_the_given_reader() {
        let f = Family::parse(F).unwrap();
        let sheet = read_sheet("Type;Width (mm);Mat\nBig;1,250mm;1,5\n").unwrap();
        assert!(!sheet.comma_separated());
        // text columns do not vote for the decimal mark; the unit is cut off
        assert_eq!(sheet.number_cells(&f), ["1,250"]);
        let comma = |s: &str| {
            s.trim()
                .replace(',', ".")
                .parse::<f64>()
                .map_err(|e| e.to_string())
        };
        let p = plan_import(&f, &f.types, &sheet, "mm", &comma).unwrap();
        assert!(near(num_of(&p.types[0], "Width"), 0.00125));
        let refuse = |s: &str| Err::<f64, String>(format!("'{s}' reads two ways"));
        let p = plan_import(&f, &f.types, &sheet, "mm", &refuse).unwrap();
        assert!(p.skipped[0].msg.contains("two ways"), "{:?}", p.skipped);
        assert!(near(num_of(&p.types[0], "Width"), 1.2));
    }

    #[test]
    fn russian_file_round_trips_with_semicolons_and_decimal_commas() {
        let f = Family::parse(F).unwrap();
        let mut types = f.types.clone();
        types[0].values = vec![
            ("Width".into(), Value::Num(0.7505)),
            ("Mat".into(), Value::Text("=1+2".into())),
        ];
        types[1].name = "Big; \"wide\"".into();
        let file = export_csv_file(&f, &types, true);
        assert_eq!(&file.as_bytes()[..3], &[0xEF, 0xBB, 0xBF]);
        let body = file.trim_start_matches(crate::csv::BOM);
        assert!(body.starts_with("Type;Width (mm);Depth (mm);Mat;Shelf\r\n"), "{body}");
        assert!(body.contains("Small;750,5;"), "{body}");
        assert!(body.contains(";'=1+2;"), "{body}");
        assert!(body.contains("\"Big; \"\"wide\"\"\";1200;"), "{body}");
        let back = import_csv(&f, &file, "mm").unwrap().types;
        assert_eq!(back.len(), 2);
        let get = |t: &FamilyType, k: &str| t.values.iter().find(|v| v.0 == k).unwrap().1.clone();
        assert!(matches!(get(&back[0], "Width"), Value::Num(x) if (x - 0.7505).abs() < 1e-12));
        assert_eq!(get(&back[0], "Mat"), Value::Text("=1+2".into()));
        assert_eq!(back[1].name, "Big; \"wide\"");
        // the English file keeps ',' and '.'
        let en = export_csv_file(&f, &types, false);
        assert!(en.contains("Small,750.5,"), "{en}");
        let back = import_csv(&f, &en, "mm").unwrap().types;
        assert!(matches!(get(&back[0], "Width"), Value::Num(x) if (x - 0.7505).abs() < 1e-12));
    }

    #[test]
    fn export_uses_the_shared_writer() {
        let src = include_str!("catalog.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("csv::file(") && code.contains("csv::join("));
        assert!(!code.contains("fn quote("), "own quoting is back");
    }

    #[test]
    fn csv_errors_name_the_line() {
        let f = Family::parse(F).unwrap();
        let e = import_csv(&f, "Type,Area2 (m2)\nA,1\n", "mm").unwrap_err();
        assert!(e.msg.contains("formula"), "{e}");
        let e = import_csv(&f, "Type,Width (mm)\nA,100\nA,200\n", "mm").unwrap_err();
        assert_eq!(e.line, 3);
        let e = import_csv(&f, "Type,Width (deg)\nA,100\n", "mm").unwrap_err();
        assert!(e.msg.contains("unit"), "{e}");
        let e = import_csv(&f, "Type,Nope\nA,1\n", "mm").unwrap_err();
        assert!(e.msg.contains("Nope"));
    }
}
