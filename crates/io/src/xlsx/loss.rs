//! What a re-export into an existing workbook would not carry over. The new book is written
//! first; it is then compared with the old one, so only what really goes missing is listed:
//! user cells (by column, rows without an id, user sheets) and package parts the writer does
//! not keep (charts, pictures, comments, validation, merges, formats of user sheets, ...).
//! The caller refuses with the list until the user confirms exactly those items.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;

use super::extra::Kept;
use super::INFO;

/// Start of the refusal text; the items follow, joined by [`LOSS_SEP`].
pub const LOSS_PREFIX: &str = "the workbook was not overwritten, re-export would drop: ";
pub const LOSS_SEP: &str = "; ";

pub(crate) const NOT_OURS: &str =
    "the whole workbook: it was not made by this app, its sheets would be replaced";
const NAMES: &str = "named ranges of the workbook";
const MACROS: &str = "macros of the workbook";
const CELLS: &str = "{} cells of column '{}' on sheet '{}'";
const BARE_CELLS: &str = "{} cells without a column header on sheet '{}'";
const IDLESS: &str = "{} rows without an id on sheet '{}'";
const DUP_ROWS: &str = "{} rows with a repeated id on sheet '{}'";
const SHEET_CELLS: &str = "{} cells of sheet '{}'";
const TITLE_CELLS: &str = "{} cells above the header on sheet '{}'";
const FORMULAS: &str = super::msg::FORMULAS_DROPPED.1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum What {
    Charts,
    Pictures,
    Comments,
    Validation,
    Merged,
    Conditional,
    Format,
    Sizes,
    Links,
    Tables,
    Pivots,
    Filter,
    Sort,
    HiddenRows,
    HiddenCols,
}

impl What {
    fn tpl(self) -> &'static str {
        match self {
            What::Charts => "charts on sheet '{}'",
            What::Pictures => "pictures on sheet '{}'",
            What::Comments => "comments on sheet '{}'",
            What::Validation => "data validation on sheet '{}'",
            What::Merged => "merged cells on sheet '{}'",
            What::Conditional => "conditional formatting on sheet '{}'",
            What::Format => "cell formatting on sheet '{}'",
            What::Sizes => "column widths and row heights on sheet '{}'",
            What::Links => "links on sheet '{}'",
            What::Tables => "tables on sheet '{}'",
            What::Pivots => "pivot tables on sheet '{}'",
            What::Filter => "filter on sheet '{}'",
            What::Sort => "sort settings on sheet '{}'",
            What::HiddenRows => "hidden rows on sheet '{}'",
            What::HiddenCols => "hidden columns on sheet '{}'",
        }
    }
}

/// Refusal text for `lost`, or None when every item was accepted by the user.
pub fn refusal(lost: &[String], accepted: &[String]) -> Option<String> {
    let open: Vec<&str> = lost
        .iter()
        .filter(|l| !accepted.contains(l))
        .map(String::as_str)
        .collect();
    (!open.is_empty()).then(|| format!("{LOSS_PREFIX}{}", open.join(LOSS_SEP)))
}

/// Items of a refusal made by [`refusal`]; None for any other text.
pub fn loss_items(err: &str) -> Option<Vec<String>> {
    let rest = err.strip_prefix(LOSS_PREFIX)?;
    Some(rest.split(LOSS_SEP).map(str::to_string).collect())
}

/// What replacing the book `old` by the freshly written `new` would drop; empty when `old`
/// does not exist. `id_given`: rows without an id that the new book numbers (rows the Apply
/// before this re-export created or matched), not counted as lost.
pub fn reexport_losses(
    old: &Path,
    new: &Path,
    fams: &[super::family::FamilySheet],
    id_given: usize,
) -> Result<Vec<String>, String> {
    if !old.exists() {
        return Ok(vec![]);
    }
    let a = Pkg::scan(old)?;
    if !a.ours {
        return Ok(vec![NOT_OURS.to_string()]);
    }
    let mut ka = Kept::read(old, fams)?;
    let kb = Kept::read(new, fams)?;
    let b = Pkg::scan(new)?;
    // layer rows are keyed by position: the old counts move to the new keys of their own layer
    let mut out = super::layer_match::rekey_losses(old, new, &mut ka.counts);
    // user cells first: they are what the user typed
    // a row whose element is gone from the new book takes its cells along on purpose;
    // a sheet missing from the new book loses them all
    let rows_b: HashMap<&str, BTreeSet<i64>> =
        kb.counts.ids.iter().map(|(s, ids)| (s.as_str(), ids.iter().copied().collect())).collect();
    for (sheet, norm, shown, ids) in &ka.counts.cols {
        let live = rows_b.get(sheet.as_str());
        let kept: BTreeSet<i64> = kb
            .counts
            .cols
            .iter()
            .filter(|c| &c.0 == sheet && &c.1 == norm)
            .flat_map(|c| c.3.iter().copied())
            .collect();
        let n = ids.iter().filter(|&i| live.is_none_or(|l| l.contains(i)) && !kept.contains(i)).count();
        if n > 0 {
            let k = n.to_string();
            out.push(if norm.is_empty() {
                fill(BARE_CELLS, &[&k, sheet])
            } else {
                fill(CELLS, &[&k, shown, sheet])
            });
        }
    }
    // a second row with the same id (a copied row the import ignores) is not written again:
    // count rows, not ids, or it would go with all its cells unasked
    let repeats = |ids: &[i64], live: Option<&BTreeSet<i64>>| {
        let mut seen = BTreeSet::new();
        ids.iter().filter(|i| !seen.insert(**i) && live.is_none_or(|l| l.contains(*i))).count()
    };
    for (sheet, ids) in &ka.counts.ids {
        let live = rows_b.get(sheet.as_str());
        let n = repeats(ids, live);
        let m = kb.counts.ids.iter().find(|c| &c.0 == sheet).map_or(0, |c| repeats(&c.1, None));
        if n > m {
            out.push(fill(DUP_ROWS, &[&(n - m).to_string(), sheet]));
        }
    }
    let mut spare = id_given;
    for (sheet, n) in &ka.counts.idless {
        let m = kb.counts.idless.iter().find(|c| &c.0 == sheet).map_or(0, |c| c.1);
        let mut lost = n.saturating_sub(m);
        let take = lost.min(spare);
        lost -= take;
        spare -= take;
        if lost > 0 {
            out.push(fill(IDLESS, &[&lost.to_string(), sheet]));
        }
    }
    // title and note rows above the header of an app sheet are the user's too
    for (sheet, n) in &ka.counts.titles {
        let m = kb.counts.titles.iter().find(|c| &c.0 == sheet).map_or(0, |c| c.1);
        if m < *n {
            out.push(fill(TITLE_CELLS, &[&(n - m).to_string(), sheet]));
        }
    }
    for (sheet, n) in &ka.counts.sheets {
        let m = kb.counts.sheets.iter().find(|c| &c.0 == sheet).map_or(0, |c| c.1);
        if m < *n {
            out.push(fill(SHEET_CELLS, &[&(n - m).to_string(), sheet]));
        }
    }
    // formulas of the app's own columns: kept while their result is the model value
    // both books come from the cache: the export already parsed `old` (and a repeat check `new`)
    let (va, vb) = (super::sheet_view::cached(old), super::sheet_view::cached(new));
    for (sheet, shown, n) in super::sheet_view::formulas_dropped(&va, &vb) {
        out.push(fill(FORMULAS, &[&n.to_string(), &shown, &sheet]));
    }
    let user: BTreeSet<&str> = ka.counts.sheets.iter().map(|s| s.0.as_str()).collect();
    let rows_kept = super::sheet_view::rows_kept(&va, &vb, fams);
    for ((sheet, what), n) in &a.marks {
        // formats and sizes of the app's own sheets are its own and written anew
        if matches!(what, What::Format | What::Sizes) && !user.contains(sheet.as_str()) {
            continue;
        }
        // hidden rows follow their element (sheet_view.rs); a deleted element takes its row
        if *what == What::HiddenRows && rows_kept.contains(sheet) {
            continue;
        }
        let m = b.marks.get(&(sheet.clone(), *what)).copied().unwrap_or(0);
        if m < *n {
            out.push(what.tpl().replace("{}", sheet));
        }
    }
    if a.names.iter().any(|n| !b.names.contains(n)) {
        out.push(NAMES.to_string());
    }
    if a.macros {
        out.push(MACROS.to_string());
    }
    Ok(out)
}

fn fill(tpl: &str, args: &[&str]) -> String {
    super::fill_template(tpl, args)
}

/// Marks of one package.
struct Scan {
    /// The book has the app's Info sheet.
    ours: bool,
    /// (sheet, kind) -> count; non-worksheet sheets are left to the caller's own check.
    marks: BTreeMap<(String, What), usize>,
    /// Defined names, without the built-in `_xlnm.` ones.
    names: BTreeSet<String>,
    macros: bool,
}

struct Pkg {
    zip: zip::ZipArchive<std::fs::File>,
}

impl Pkg {
    fn scan(path: &Path) -> Result<Scan, String> {
        let unread = |e: &dyn std::fmt::Display| format!("{}: {e}", super::extra::UNREADABLE);
        let f = std::fs::File::open(path).map_err(|e| unread(&e))?;
        let zip = zip::ZipArchive::new(f).map_err(|e| unread(&e))?;
        let mut pkg = Pkg { zip };
        let book = pkg.text("xl/workbook.xml").ok_or_else(|| unread(&"no xl/workbook.xml"))?;
        let rels = pkg.rels("xl/workbook.xml");
        let sheets: Vec<(String, String)> = tags(&book, "sheet")
            .filter_map(|t| {
                let name = unescape(attr(t, "name")?);
                let rid = attr(t, "r:id").or_else(|| attr_suffix(t, ":id"))?;
                let target = rels.get(rid)?.1.clone();
                Some((name, target))
            })
            .collect();
        let ours = sheets.iter().any(|(n, _)| n == INFO);
        let mut marks: BTreeMap<(String, What), usize> = BTreeMap::new();
        for (name, part) in &sheets {
            if !part.contains("worksheets/") {
                continue;
            }
            let mut found: HashMap<What, usize> = HashMap::new();
            if let Some(xml) = pkg.text(part) {
                sheet_xml(&xml, &mut found);
                view_xml(&xml, &mut found);
            }
            for (ty, target) in pkg.rels(part).values() {
                match rel_kind(ty) {
                    "drawing" => {
                        for (dty, _) in pkg.rels(target).values() {
                            match rel_kind(dty) {
                                "chart" | "chartEx" => *found.entry(What::Charts).or_default() += 1,
                                "image" => *found.entry(What::Pictures).or_default() += 1,
                                _ => {}
                            }
                        }
                    }
                    "image" => *found.entry(What::Pictures).or_default() += 1,
                    "comments" => {
                        let xml = pkg.text(target).unwrap_or_default();
                        // the app's own notes sit on the header row (row 1) of its sheets
                        let n = tags(&xml, "comment")
                            .filter_map(|t| attr(t, "ref"))
                            .filter(|r| row_of(r) != Some(1))
                            .count();
                        *found.entry(What::Comments).or_default() += n;
                    }
                    "table" => *found.entry(What::Tables).or_default() += 1,
                    "pivotTable" => *found.entry(What::Pivots).or_default() += 1,
                    _ => {}
                }
            }
            for (w, n) in found {
                if n > 0 {
                    marks.insert((name.clone(), w), n);
                }
            }
        }
        let names = tags(&book, "definedName")
            .filter_map(|t| attr(t, "name"))
            .filter(|n| !n.starts_with("_xlnm."))
            .map(unescape)
            .collect();
        let macros = pkg.zip.by_name("xl/vbaProject.bin").is_ok();
        Ok(Scan { ours, marks, names, macros })
    }

    fn text(&mut self, part: &str) -> Option<String> {
        let mut f = self.zip.by_name(part.trim_start_matches('/')).ok()?;
        let mut s = String::new();
        f.read_to_string(&mut s).ok()?;
        Some(s)
    }

    /// Relationships of `part`: id -> (type, resolved part path).
    fn rels(&mut self, part: &str) -> HashMap<String, (String, String)> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_path = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let Some(xml) = self.text(&rels_path) else {
            return HashMap::new();
        };
        tags(&xml, "Relationship")
            .filter(|t| attr(t, "TargetMode") != Some("External"))
            .filter_map(|t| {
                let id = attr(t, "Id")?.to_string();
                let ty = attr(t, "Type")?.to_string();
                let target = resolve(dir, &unescape(attr(t, "Target")?));
                Some((id, (ty, target)))
            })
            .collect()
    }
}

/// Counts of one sheet's XML. Formats and sizes are counted on every sheet; the caller keeps
/// them only for user sheets.
fn sheet_xml(xml: &str, found: &mut HashMap<What, usize>) {
    for (name, t) in all_tags(xml) {
        let w = match name {
            "mergeCell" => Some(What::Merged),
            "dataValidation" => Some(What::Validation),
            "conditionalFormatting" => Some(What::Conditional),
            "hyperlink" => Some(What::Links),
            "c" if attr(t, "s").is_some_and(|s| s != "0") => Some(What::Format),
            "row" if attr(t, "customFormat") == Some("1") => Some(What::Format),
            "row" if attr(t, "customHeight") == Some("1") => Some(What::Sizes),
            "col" if attr(t, "customWidth") == Some("1") => Some(What::Sizes),
            "col" if attr(t, "style").is_some_and(|s| s != "0") => Some(What::Format),
            _ => None,
        };
        if let Some(w) = w {
            *found.entry(w).or_default() += 1;
        }
    }
}

/// Last column of a sheet (1-based, `XFD`).
pub(super) const LAST_COL: u32 = 16_384;

/// `min` and `max` of one `<col>`, 1-based.
type ColRange = (Option<usize>, Option<usize>);

/// View marks of one sheet's XML: filter criteria, sort state, hidden rows and hidden columns
/// (by column count). The writer keeps no filter or sort state; hidden rows and columns of the
/// app sheets it carries itself (sheet_view.rs), so only what it could not place is missing.
fn view_xml(xml: &str, found: &mut HashMap<What, usize>) {
    let mut hidden_cols: Vec<ColRange> = vec![];
    let mut used: usize = 0;
    for (name, t) in all_tags(xml) {
        let hidden = || matches!(attr(t, "hidden"), Some("1") | Some("true"));
        let (w, n) = match name {
            "filterColumn" => (What::Filter, 1),
            "sortState" => (What::Sort, 1),
            "row" if hidden() => (What::HiddenRows, 1),
            "col" if hidden() => {
                let at = |k: &str| attr(t, k).and_then(|v| v.parse::<usize>().ok());
                hidden_cols.push((at("min"), at("max")));
                continue;
            }
            // cells only: `<dimension>` also counts the column formats of spare columns
            "c" => {
                let r = attr(t, "r").unwrap_or("");
                used = used.max(col_num(r).unwrap_or(0));
                continue;
            }
            _ => continue,
        };
        *found.entry(w).or_default() += n;
    }
    let n: usize = merge_hidden(hidden_cols).into_iter().map(|r| hidden_col_count(r, used)).sum();
    if n > 0 {
        *found.entry(What::HiddenCols).or_default() += n;
    }
}

/// Joins adjoining `<col>` ranges: the writer splits one hidden run where column styles change.
fn merge_hidden(mut ranges: Vec<ColRange>) -> Vec<ColRange> {
    ranges.sort_by_key(|r| r.0);
    let mut out: Vec<ColRange> = vec![];
    for r in ranges {
        if let (Some((Some(_), Some(b))), (Some(a2), Some(b2))) = (out.last_mut(), r) {
            if a2 == *b + 1 {
                *b = b2;
                continue;
            }
        }
        out.push(r);
    }
    out
}

/// Hidden columns of one `<col min max>` as the loss check counts them: a range to the sheet
/// end ("hide everything right of the table") counts its columns inside the used range plus one
/// mark for the rest, which the writer re-applies; otherwise every column counts.
fn hidden_col_count(range: ColRange, used: usize) -> usize {
    match range {
        (Some(a), Some(b)) if b >= a && b >= LAST_COL as usize && used < b => (a..=used).count() + 1,
        (Some(a), Some(b)) if b >= a => b - a + 1,
        _ => 1,
    }
}

/// Column number (1-based) of a cell reference such as `XFD12` or `$C$4`.
fn col_num(r: &str) -> Option<usize> {
    let letters: Vec<u8> = r.bytes().filter(|b| *b != b'$').take_while(u8::is_ascii_alphabetic).collect();
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    Some(letters.iter().fold(0, |n, b| n * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize))
}

/// Worksheet XML of the sheets of `path` that `want` names, by sheet name; empty when the
/// package cannot be read (the loss check itself reports an unreadable book).
pub(super) fn sheet_xmls(path: &Path, want: impl Fn(&str) -> bool) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(zip) = std::fs::File::open(path).ok().and_then(|f| zip::ZipArchive::new(f).ok()) else {
        return out;
    };
    let mut pkg = Pkg { zip };
    let Some(book) = pkg.text("xl/workbook.xml") else {
        return out;
    };
    let rels = pkg.rels("xl/workbook.xml");
    for t in tags(&book, "sheet") {
        let Some(name) = attr(t, "name").map(unescape) else {
            continue;
        };
        let rid = attr(t, "r:id").or_else(|| attr_suffix(t, ":id"));
        let Some((_, part)) = rid.and_then(|r| rels.get(r)) else {
            continue;
        };
        if want(&name) {
            if let Some(xml) = pkg.text(part) {
                out.insert(name, xml);
            }
        }
    }
    out
}

/// Row number of a cell reference such as `B12`.
fn row_of(r: &str) -> Option<u32> {
    r.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '$').parse().ok()
}

/// Last segment of a relationship type URI.
fn rel_kind(ty: &str) -> &str {
    ty.rsplit('/').next().unwrap_or(ty)
}

/// Package path of `target` relative to the folder `dir`.
fn resolve(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Every start tag as (local name, tag text without `<` and `>`).
pub(super) fn all_tags(xml: &str) -> impl Iterator<Item = (&str, &str)> {
    xml.split('<').skip(1).filter_map(|chunk| {
        let t = chunk.split('>').next()?;
        if t.starts_with(['/', '?', '!']) {
            return None;
        }
        let qname = t.split(|c: char| c.is_whitespace() || c == '/').next()?;
        let local = qname.rsplit(':').next()?;
        Some((local, t))
    })
}

fn tags<'a>(xml: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    all_tags(xml).filter(move |(n, _)| *n == name).map(|(_, t)| t)
}

/// Value of attribute `name` in a start tag (either quote style).
pub(super) fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before = rest[..i].chars().last();
        let after = &rest[i + name.len()..];
        rest = after;
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let Some(v) = after.trim_start().strip_prefix('=') else {
            continue;
        };
        let v = v.trim_start();
        let q = v.chars().next()?;
        if q != '"' && q != '\'' {
            continue;
        }
        return v[1..].split(q).next();
    }
    None
}

/// Attribute whose qualified name ends with `suffix` (a relationship id under any prefix).
fn attr_suffix<'a>(tag: &'a str, suffix: &str) -> Option<&'a str> {
    let i = tag.find(&format!("{suffix}="))?;
    let start = tag[..i].rfind(char::is_whitespace)? + 1;
    attr(tag, &tag[start..i + suffix.len()])
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod hidden_tail_tests {
    use super::*;

    /// Hidden column count of a sheet with cells A1..`last`1 and the given `<col>` elements.
    fn hidden(last: &str, cols: &str) -> usize {
        let xml = format!(
            "<worksheet><dimension ref=\"A1:{last}1\"/><cols>{cols}</cols><sheetData><row r=\"1\"><c r=\"A1\"/><c r=\"{last}1\"/></row></sheetData></worksheet>"
        );
        let mut found = HashMap::new();
        view_xml(&xml, &mut found);
        found.get(&What::HiddenCols).copied().unwrap_or(0)
    }

    #[test]
    fn a_range_to_the_sheet_end_is_one_mark_past_the_used_range() {
        let tail = "<col min=\"12\" max=\"16384\" width=\"0\" hidden=\"1\"/>";
        assert_eq!(hidden("K", tail), 1);
        // the same tail behind a wider table: still one mark, so old and new books agree
        assert_eq!(hidden("M", "<col min=\"14\" max=\"16384\" hidden=\"1\"/>"), 1);
        // partly inside: J..L count one by one, the rest is one mark
        assert_eq!(hidden("L", "<col min=\"10\" max=\"16384\" hidden=\"1\"/>"), 4);
        // data in the last sheet column: every column counts as before
        assert_eq!(hidden("XFD", "<col min=\"16383\" max=\"16384\" hidden=\"1\"/>"), 2);
    }

    #[test]
    fn a_dropped_hidden_column_still_counts_less() {
        let old = hidden("E", "<col min=\"3\" max=\"3\" hidden=\"1\"/><col min=\"6\" max=\"16384\" hidden=\"1\"/>");
        let new = hidden("E", "<col min=\"6\" max=\"16384\" hidden=\"1\"/>");
        assert_eq!((old, new), (2, 1), "a user column hidden inside the table is lost");
        // a range past the table that stops short of the sheet end counts column by column
        assert_eq!(hidden("E", "<col min=\"8\" max=\"9\" hidden=\"1\"/>"), 2);
        assert_eq!(hidden("E", "<col min=\"3\" max=\"4\" width=\"9\" customWidth=\"1\"/>"), 0);
    }

    #[test]
    fn column_numbers_of_references() {
        assert_eq!(col_num("A1"), Some(1));
        assert_eq!(col_num("$C$4"), Some(3));
        assert_eq!(col_num("XFD1048576"), Some(16_384));
        assert_eq!(col_num("12"), None);
    }
}
