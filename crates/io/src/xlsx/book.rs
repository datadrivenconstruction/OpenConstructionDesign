//! Professional book: captions in the project language with a unit suffix, raw column keys in a
//! hidden `Keys` sheet (per sheet a row of keys, key of column j in cell j+1, and a row of the
//! captions as exported) and dropdown lists from a hidden `Lists` sheet. Import finds a column's
//! key by its caption, so inserted, deleted or moved columns keep their keys. Books without
//! `Keys` are read by their headers as before.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use calamine::{Reader, Xlsx};
use ddc_model::Model;
use rust_xlsxwriter::utility::column_number_to_name;
use rust_xlsxwriter::{DataValidation, Formula, Workbook, Worksheet};

use super::*;

/// How the book is dressed. The default is the plain book (canonical headers, no lists).
#[derive(Clone, Debug, Default)]
pub struct Style {
    /// Captions, dropdowns and key row on.
    pub professional: bool,
    /// Column key (`base_offset`) -> caption without unit, in the project language.
    pub captions: HashMap<String, String>,
    /// What the book carries; None = the whole model.
    pub scope: Option<super::scope::Scope>,
    /// Number format of the user: true = comma decimal. Written to Info `decimal`; default point.
    pub decimal_comma: bool,
    /// Locale tag (`de-DE`) written to Info `locale`; empty = no row.
    pub locale: String,
    /// Adds the `ValueSources` sheet (who set each value, verified or not). Off by default:
    /// the book is then byte for byte the book without it, and import never reads it.
    pub sources: bool,
}

impl Style {
    pub fn professional(captions: HashMap<String, String>) -> Style {
        Style {
            professional: true,
            captions,
            scope: None,
            decimal_comma: false,
            locale: String::new(),
            sources: false,
        }
    }

    pub fn with_sources(mut self, on: bool) -> Style {
        self.sources = on;
        self
    }

    pub fn with_number_format(mut self, decimal_comma: bool, locale: &str) -> Style {
        self.decimal_comma = decimal_comma;
        self.locale = locale.to_string();
        self
    }

    pub fn with_scope(mut self, scope: super::scope::Scope) -> Style {
        self.scope = Some(scope);
        self
    }
}

/// Spare rows below the data that already carry the dropdown.
const SPARE_ROWS: u32 = 500;
const LIST_NAMES: [&str; 7] = [
    "levels",
    "wall_types",
    "slab_types",
    "materials",
    "kinds",
    "phases",
    "hatches",
];

struct Ctx {
    style: Style,
    lists: Vec<Vec<String>>,
    /// Names of the lists past `LIST_NAMES`, added by columns that carry their own options.
    more: Vec<String>,
    /// Sheet, keys of the format columns, captions of all columns written.
    keys: Vec<(String, Vec<String>, Vec<String>)>,
}

/// Suffix of the `Keys` row with the captions as exported; `:` never occurs in a sheet name.
const CAPTIONS: &str = ":captions";

/// One sheet's entry of the `Keys` sheet.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeyRow {
    /// Key of the format column exported at position j.
    pub keys: Vec<String>,
    /// Header text of every column exported at position j; None in books of older builds.
    pub captions: Option<Vec<String>>,
}

pub(crate) type KeyRows = HashMap<String, KeyRow>;

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn strings(model: &Model, sql: &str) -> Vec<String> {
    let Ok(mut st) = model.sql().prepare(sql) else {
        return vec![];
    };
    let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) else {
        return vec![];
    };
    rows.flatten().filter(|s| !s.is_empty()).collect()
}

/// Starts one export; a plain style leaves the book as it was.
pub(crate) fn begin(model: &Model, style: &Style) {
    let ctx = style.professional.then(|| {
        let mut materials = strings(
            model,
            "SELECT material FROM wall_type_layer UNION SELECT material FROM slab_type_layer",
        );
        materials.extend(
            model
                .materials()
                .map(|v| v.into_iter().map(|m| m.spec.name).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        materials.sort();
        materials.dedup();
        // library hatches in use are exported as they are, so the dropdown must accept them
        let mut hatches = ddc_model::MATERIAL_HATCHES.map(String::from).to_vec();
        hatches.extend(
            strings(model, "SELECT DISTINCT hatch FROM material ORDER BY hatch")
                .into_iter()
                .filter(|h| ddc_model::is_lib_hatch(h)),
        );
        Ctx {
            style: style.clone(),
            lists: vec![
                strings(model, "SELECT name FROM level ORDER BY id"),
                strings(model, "SELECT name FROM wall_type ORDER BY id"),
                strings(model, "SELECT name FROM slab_type ORDER BY id"),
                materials,
                ["door", "window", "void"].map(String::from).to_vec(),
                model
                    .phases()
                    .map(|p| p.into_iter().map(|p| p.name).collect())
                    .unwrap_or_default(),
                hatches,
            ],
            more: vec![],
            keys: vec![],
        }
    });
    CTX.with(|c| *c.borrow_mut() = ctx);
}

pub(crate) fn active() -> bool {
    CTX.with(|c| c.borrow().is_some())
}

fn prettify(key: &str) -> String {
    let s = key.replace('_', " ");
    let mut ch = s.chars();
    ch.next().map_or(String::new(), |f| {
        f.to_uppercase().collect::<String>() + ch.as_str()
    })
}

/// `Base offset [m]`: caption in the project language, unit of the book in brackets.
pub(crate) fn header(key: &str, canonical: &str, u: Units) -> String {
    let cap = CTX.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|x| x.style.captions.get(key).cloned())
    });
    let cap = cap.unwrap_or_else(|| prettify(key));
    let shown = u.header(canonical);
    match (shown.find('['), shown.find(']')) {
        (Some(a), Some(b)) if b > a => format!("{cap} {}", &shown[a..=b]),
        _ => cap,
    }
}

/// Keys of the format columns and the captions of all columns as written on the sheet.
pub(crate) fn record_keys(sheet: &str, keys: Vec<String>, captions: Vec<String>) {
    CTX.with(|c| {
        if let Some(x) = c.borrow_mut().as_mut() {
            x.keys.push((sheet.to_string(), keys, captions));
        }
    });
}

fn list_of(sheet: &str, key: &str) -> Option<usize> {
    Some(match (sheet, key) {
        (WALLS | ROOMS | SLABS | CURTAINS, "level") => 0,
        (WALLS, "type") => 1,
        (SLABS, "type") => 2,
        (LAYERS | SLAB_LAYERS, "material") => 3,
        (OPENINGS, "kind") => 4,
        (MATERIALS, "hatch") => 6,
        (_, "phase_created" | "phase_demolished") => 5,
        _ => return None,
    })
}

/// Dropdown on a column from its list (rows 1.. plus spare rows); no-op without a list.
pub(crate) fn validation(
    ws: &mut Worksheet,
    sheet: &str,
    key: &str,
    col: u16,
    rows: usize,
) -> Result<(), String> {
    let Some(li) = list_of(sheet, key) else {
        return Ok(());
    };
    let n = CTX.with(|c| c.borrow().as_ref().map_or(0, |x| x.lists[li].len()));
    if n == 0 {
        return Ok(());
    }
    let f = format!(
        "=Lists!${0}$2:${0}${1}",
        column_number_to_name(li as u16),
        n + 1
    );
    let dv = DataValidation::new().allow_list_formula(Formula::new(f));
    // the data rows start below the header, which rows of the user may push down
    let top = super::extra::titles_of(sheet).0;
    ws.add_data_validation(top + 1, col, top + rows as u32 + SPARE_ROWS, col, &dv)
        .map_err(err)?;
    Ok(())
}

/// Dropdown of `items` on a column (rows 1.. plus spare rows). The options go to the `Lists`
/// sheet of a styled export, else inline when they fit the 255-character limit.
/// Other typing stays possible: the import checks every cell and reports the refused ones.
pub(crate) fn list_validation(
    ws: &mut Worksheet,
    name: &str,
    items: &[String],
    col: u16,
    rows: usize,
    top: u32,
) -> Result<(), String> {
    if items.is_empty() {
        return Ok(());
    }
    let last = top + rows as u32 + SPARE_ROWS;
    let slot = CTX.with(|c| {
        c.borrow_mut().as_mut().map(|x| {
            x.lists.push(items.to_vec());
            x.more.push(name.to_string());
            x.lists.len() - 1
        })
    });
    let dv = match slot {
        Some(li) => DataValidation::new().allow_list_formula(Formula::new(format!(
            "=Lists!${0}$2:${0}${1}",
            column_number_to_name(li as u16),
            items.len() + 1
        ))),
        None => {
            let total: usize = items.iter().map(|s| s.chars().count() + 1).sum();
            if total > 255 || items.iter().any(|s| s.contains(',')) {
                return Ok(());
            }
            DataValidation::new().allow_list_strings(items).map_err(err)?
        }
    };
    let dv = dv.show_error_message(false);
    ws.add_data_validation(top + 1, col, last, col, &dv).map_err(err)?;
    Ok(())
}

/// Hidden `Keys` and `Lists` sheets; ends the export.
pub(crate) fn finish(wb: &mut Workbook) -> Result<(), String> {
    let Some(x) = CTX.with(|c| c.borrow_mut().take()) else {
        return Ok(());
    };
    let ws = wb.add_worksheet();
    ws.set_name(KEYS).map_err(err)?;
    for (r, (sheet, keys, caps)) in x.keys.iter().enumerate() {
        let (rk, rc) = (2 * r as u32, 2 * r as u32 + 1);
        ws.write_string(rk, 0, sheet).map_err(err)?;
        for (j, k) in keys.iter().enumerate() {
            ws.write_string(rk, 1 + j as u16, k).map_err(err)?;
        }
        ws.write_string(rc, 0, format!("{sheet}{CAPTIONS}")).map_err(err)?;
        for (j, h) in caps.iter().enumerate() {
            ws.write_string(rc, 1 + j as u16, h).map_err(err)?;
        }
    }
    ws.set_hidden(true);
    let ws = wb.add_worksheet();
    ws.set_name(LISTS).map_err(err)?;
    for (i, list) in x.lists.iter().enumerate() {
        let head = LIST_NAMES
            .get(i)
            .copied()
            .or_else(|| x.more.get(i - LIST_NAMES.len()).map(String::as_str))
            .unwrap_or("");
        ws.write_string(0, i as u16, head).map_err(err)?;
        for (r, v) in list.iter().enumerate() {
            let v = super::cellsafe::clipped(v).unwrap_or_else(|| v.to_string());
            ws.write_string(r as u32 + 1, i as u16, &v).map_err(err)?;
        }
    }
    ws.set_hidden(true);
    Ok(())
}

/// Workbook-level name of the totals row of a table sheet written by `export_table`.
pub(crate) const TOTAL_ROW: &str = "ddc_total_row";

/// Name prefix of the group header and subtotal rows of a grouped table sheet.
pub(crate) const GROUP_ROW: &str = "ddc_group_row_";

/// 1-based sheet rows of the group header and subtotal rows of `sheet`.
pub(crate) fn group_rows<RS: std::io::Read + std::io::Seek>(
    wb: &Xlsx<RS>,
    sheet: &str,
) -> Vec<u32> {
    wb.defined_names()
        .iter()
        .filter(|(n, _)| n.starts_with(GROUP_ROW))
        .filter_map(|(_, v)| {
            let (sh, cells) = v.trim_start_matches('=').rsplit_once('!')?;
            if sh.trim_matches('\'').replace("''", "'") != sheet {
                return None;
            }
            let first = cells.split(':').next()?;
            first
                .trim_start_matches(|c: char| c == '$' || c.is_ascii_alphabetic())
                .parse()
                .ok()
        })
        .collect()
}

/// 1-based sheet row of the totals row of `sheet`, if the book marks one.
pub(crate) fn total_row<RS: std::io::Read + std::io::Seek>(
    wb: &Xlsx<RS>,
    sheet: &str,
) -> Option<u32> {
    let (_, v) = wb.defined_names().iter().find(|(n, _)| n == TOTAL_ROW)?;
    let (sh, cells) = v.trim_start_matches('=').rsplit_once('!')?;
    let sh = sh.trim_matches('\'').replace("''", "'");
    if sh != sheet {
        return None;
    }
    let first = cells.split(':').next()?;
    first
        .trim_start_matches(|c: char| c == '$' || c.is_ascii_alphabetic())
        .parse()
        .ok()
}

/// Values and formulas of sheet `name` in one read of its XML: the same ranges as
/// `worksheet_range` and `worksheet_formula`, which each inflate and parse the whole sheet.
pub(crate) fn values_and_formulas<RS: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<RS>,
    name: &str,
) -> Result<(calamine::Range<calamine::Data>, calamine::Range<String>), calamine::XlsxError> {
    use calamine::{Cell, Data, DataRef, Range, XlsxError};
    let mut cells = match wb.worksheet_cells_reader(name) {
        Ok(c) => c,
        // a chart sheet has no cells, as both calls report it
        Err(XlsxError::NotAWorksheet(_)) => return Ok((Range::default(), Range::default())),
        Err(e) => return Err(e),
    };
    let (mut vals, mut forms) = (Vec::new(), Vec::new());
    while let Some(c) = cells.next_cell_with_formula()? {
        if let Some(f) = c.formula.filter(|f| !f.is_empty()) {
            forms.push(Cell::new(c.pos, f));
        }
        if !matches!(c.value, DataRef::Empty) {
            vals.push(Cell::new(c.pos, Data::from(c.value)));
        }
    }
    Ok((Range::from_sparse(vals), Range::from_sparse(forms)))
}

/// Key rows of a book by sheet; empty for a book without `Keys`.
pub(crate) fn read_keys<RS: std::io::Read + std::io::Seek>(wb: &mut Xlsx<RS>) -> KeyRows {
    let mut out = KeyRows::new();
    if !wb.sheet_names().iter().any(|n| n == KEYS) {
        return out;
    }
    let Ok(range) = wb.worksheet_range(KEYS) else {
        return out;
    };
    let mut raw: HashMap<String, Vec<String>> = HashMap::new();
    for row in range.rows() {
        let name = row.first().map(|d| d.to_string()).unwrap_or_default();
        if !name.is_empty() {
            let mut cells: Vec<String> = row.iter().skip(1).map(|d| d.to_string()).collect();
            // the range is as wide as its longest row: shorter rows end in blanks
            while cells.last().is_some_and(|c| c.is_empty()) {
                cells.pop();
            }
            raw.insert(name, cells);
        }
    }
    for (name, keys) in &raw {
        if name.ends_with(CAPTIONS) {
            continue;
        }
        let captions = raw.get(&format!("{name}{CAPTIONS}")).cloned();
        out.insert(
            name.clone(),
            KeyRow {
                keys: keys.clone(),
                captions,
            },
        );
    }
    out
}

/// Offset (0-based, within the first 10 rows) of the header row: the row with most known
/// headers; it must hold `id`. Falls back to 0 so a bad book is rejected as before.
/// `heads` maps the trimmed texts of a row to the normalized headers the importer would see.
pub(crate) fn find_header(
    range: &calamine::Range<calamine::Data>,
    known: &[String],
    heads: impl Fn(&[String]) -> Vec<String>,
) -> usize {
    let id = super::units::norm("id");
    let mut best = (0usize, 0usize);
    for (n, row) in range.rows().take(10).enumerate() {
        let texts: Vec<String> = row.iter().map(|d| d.to_string().trim().to_string()).collect();
        let normed = heads(&texts);
        if !normed.contains(&id) {
            continue;
        }
        let score = normed.iter().filter(|h| known.contains(h)).count();
        if score > best.1 {
            best = (n, score);
        }
    }
    best.0
}

/// Header texts the importer should see for one sheet, column by column.
#[derive(Clone, Debug, Default)]
pub(crate) struct Heads {
    /// Canonical header of the key a column carries, else the text as it is (a user column).
    pub texts: Vec<String>,
    /// Why the key row of the sheet cannot be trusted; when set, nothing may be applied.
    pub refused: Option<String>,
    /// Keys exported on the sheet that no column carries now: (key, caption as exported).
    pub missing: Vec<(String, String)>,
    /// Columns under the caption of a key another column already carries: (index, its header).
    pub copies: Vec<(usize, String)>,
}

/// Resolves the header row `head` of `sheet` (first column at absolute position `c0`) against
/// the key row of the book. A sheet without a key row keeps its texts.
pub(crate) fn resolve(
    keys: &KeyRows,
    sheet: &str,
    cols: &[Col],
    c0: usize,
    head: &[String],
) -> Heads {
    match keys.get(sheet) {
        None => Heads {
            texts: head.to_vec(),
            ..Heads::default()
        },
        Some(KeyRow {
            keys,
            captions: Some(caps),
        }) => by_caption(keys, caps, cols, c0, head),
        Some(KeyRow {
            keys,
            captions: None,
        }) => by_position(keys, cols, c0, head),
    }
}

fn col_of<'a>(cols: &'a [Col], key: &str) -> Option<&'a Col> {
    cols.iter().find(|c| c.key == key)
}

/// Current book: a column is found by its exported caption, wherever it stands now.
fn by_caption(
    keys: &[String],
    caps: &[String],
    cols: &[Col],
    c0: usize,
    head: &[String],
) -> Heads {
    use super::units::norm;
    let n = head.len();
    let cap = |e: usize| caps.get(e).map_or(String::new(), |c| norm(c));
    let normed: Vec<String> = head.iter().map(|h| norm(h)).collect();
    let mut of: Vec<Option<usize>> = vec![None; n];
    let mut taken = vec![false; keys.len()];
    // 1: still at its place under its caption
    for i in 0..n {
        let a = c0 + i;
        if a < keys.len() && !keys[a].is_empty() && !normed[i].is_empty() && cap(a) == normed[i] {
            of[i] = Some(a);
            taken[a] = true;
        }
    }
    // 2: moved, found by its caption
    for i in 0..n {
        if of[i].is_some() || normed[i].is_empty() {
            continue;
        }
        let e = (0..keys.len()).find(|&e| !taken[e] && !keys[e].is_empty() && cap(e) == normed[i]);
        if let Some(e) = e {
            of[i] = Some(e);
            taken[e] = true;
        }
    }
    // 3: a renamed caption keeps the key of its place only between neighbours that did not move
    let known: HashSet<String> = cols
        .iter()
        .map(|c| norm(c.header))
        .chain((0..caps.len()).map(cap))
        .collect();
    let fixed = of.clone();
    let stays = |j: usize, e: usize| -> bool {
        if e >= caps.len() {
            true
        } else if j >= n {
            cap(e).is_empty()
        } else if e < keys.len() {
            fixed[j] == Some(e)
        } else {
            normed[j] == cap(e)
        }
    };
    for i in 0..n {
        let a = c0 + i;
        if fixed[i].is_some() || normed[i].is_empty() || a >= keys.len() || taken[a] {
            continue;
        }
        if keys[a].is_empty() || known.contains(&normed[i]) {
            continue;
        }
        let left = if a == 0 { i == 0 } else { i > 0 && stays(i - 1, a - 1) };
        if left && stays(i + 1, a + 1) {
            of[i] = Some(a);
            taken[a] = true;
        }
    }
    let texts = head
        .iter()
        .zip(&of)
        .map(|(h, e)| {
            e.and_then(|e| col_of(cols, &keys[e]))
                .map_or_else(|| h.clone(), |c| c.header.to_string())
        })
        .collect();
    let missing = (0..keys.len())
        .filter(|&e| !taken[e] && !keys[e].is_empty())
        .map(|e| (keys[e].clone(), caps.get(e).cloned().unwrap_or_default()))
        .collect();
    // a caption the book itself wrote twice (a parameter of an older build) is no copy
    let once = |c: &str| (0..caps.len()).filter(|&e| cap(e) == c).count() == 1;
    let copies = (0..n)
        .filter(|&i| of[i].is_none() && !normed[i].is_empty() && once(&normed[i]))
        .filter_map(|i| {
            let e = (0..keys.len()).find(|&e| !keys[e].is_empty() && cap(e) == normed[i])?;
            Some((i, col_of(cols, &keys[e])?.header.to_string()))
        })
        .collect();
    Heads {
        texts,
        refused: None,
        missing,
        copies,
    }
}

/// Book of an older build (keys without captions): positions are trusted only while every keyed
/// column still looks like the column exported there; otherwise the sheet is refused.
fn by_position(keys: &[String], cols: &[Col], c0: usize, head: &[String]) -> Heads {
    use super::units::{dim, header_factor, norm, Dim};
    let normed: Vec<String> = head.iter().map(|h| norm(h)).collect();
    let names = |c: &Col| [norm(c.header), norm(&prettify(c.key))];
    let mut texts = head.to_vec();
    let mut bad: Vec<String> = vec![];
    for (e, k) in keys.iter().enumerate() {
        let Some(col) = col_of(cols, k) else {
            continue;
        };
        let Some(i) = e.checked_sub(c0).filter(|&i| i < head.len()) else {
            bad.push(format!("'{}' is missing", prettify(k)));
            continue;
        };
        let t = &normed[i];
        let fits = if t.is_empty() {
            false
        } else if names(col).contains(t) {
            true
        } else if cols.iter().any(|c| c.key != col.key && names(c).contains(t)) {
            false
        } else if (0..head.len()).any(|j| j != i && c0 + j < keys.len() && normed[j] == *t) {
            false
        } else {
            match dim(col.header) {
                Dim::None => [Dim::Len, Dim::Area, Dim::Vol]
                    .into_iter()
                    .all(|d| header_factor(&head[i], d).is_none()),
                d => header_factor(&head[i], d).is_some(),
            }
        };
        if fits {
            texts[i] = col.header.to_string();
        } else if t.is_empty() {
            bad.push(format!("'{}' is missing", prettify(k)));
        } else {
            bad.push(format!(
                "'{}' stands where '{}' was exported",
                head[i].trim(),
                prettify(k)
            ));
        }
    }
    let refused = (!bad.is_empty()).then(|| {
        format!(
            "columns were inserted, deleted or moved in a book of an older version ({}); \
             nothing applied, export the book again and repeat the edits",
            bad.join("; ")
        )
    });
    Heads {
        texts,
        refused,
        ..Heads::default()
    }
}

/// Names of all sheets of a book, hidden ones included.
pub fn sheet_names(path: &std::path::Path) -> Result<Vec<String>, String> {
    let wb: Xlsx<_> = calamine::open_workbook(path).map_err(err)?;
    Ok(wb.sheet_names())
}

/// Header row (first row) of one sheet as text.
pub fn sheet_headers(path: &std::path::Path, sheet: &str) -> Result<Vec<String>, String> {
    let mut wb: Xlsx<_> = calamine::open_workbook(path).map_err(err)?;
    let range = wb.worksheet_range(sheet).map_err(err)?;
    Ok(range
        .rows()
        .next()
        .map(|r| r.iter().map(|c| c.to_string()).collect())
        .unwrap_or_default())
}
