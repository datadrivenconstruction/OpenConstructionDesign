//! Survey point files: CSV, TSV, semicolon (decimal comma) or whitespace columns (decimal point or
//! decimal comma). A header row names the columns (x/y/z, e/n/z, easting/northing/elevation,
//! p/id, d/code); without one the columns are x y z, or P N E Z [D] when the first column is text.
//! Numeric point ids (`1 5000 3000 12.5`) are ambiguous with `x y z code` and need an explicit
//! column order. Numbers follow the per-file decimal rule of [`crate::num_file`]: a row with an
//! ambiguous number (`1,250` in a file with no decimal mark elsewhere) is skipped and counted.
//! A header cell may carry the unit (`x(mm)`, `Z [ft]`); [`xyz_unit`] reports it for the dialog.

use super::units::{comment_unit, split_unit, SurveyUnit};
use super::TopoSource;
use crate::num_file::{detect_mark, parse_num, FileNum, Mark, NumErr};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sep {
    Tab,
    Semi,
    Comma,
    Space,
}

impl Sep {
    /// Character shown in the import preview.
    pub fn label(self) -> &'static str {
        match self {
            Sep::Tab => "\\t",
            Sep::Semi => ";",
            Sep::Comma => ",",
            Sep::Space => "␣",
        }
    }
}

/// Column order of header-less rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColumnOrder {
    /// Header names the columns; else `x y z …`, or `P N E Z …` when the first column is text.
    #[default]
    Auto,
    /// x (easting), y (northing), z.
    Xyz,
    /// Northing first: n, e, z.
    Nez,
    /// Point id, northing, easting, elevation [, description].
    Pnez,
    /// Point id, easting, northing, elevation [, description].
    Penz,
}

impl ColumnOrder {
    pub const ALL: [ColumnOrder; 5] = [
        ColumnOrder::Auto,
        ColumnOrder::Xyz,
        ColumnOrder::Nez,
        ColumnOrder::Pnez,
        ColumnOrder::Penz,
    ];

    /// Stable key (`topo.import.order.<key>` in the UI).
    pub fn key(self) -> &'static str {
        match self {
            ColumnOrder::Auto => "auto",
            ColumnOrder::Xyz => "xyz",
            ColumnOrder::Nez => "nez",
            ColumnOrder::Pnez => "pnez",
            ColumnOrder::Penz => "penz",
        }
    }

    /// Column of x, y, z for an explicit order.
    fn cols(self) -> Option<[usize; 3]> {
        match self {
            ColumnOrder::Auto => None,
            ColumnOrder::Xyz => Some([0, 1, 2]),
            ColumnOrder::Nez => Some([1, 0, 2]),
            ColumnOrder::Pnez => Some([2, 1, 3]),
            ColumnOrder::Penz => Some([1, 2, 3]),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct XyzOptions {
    pub order: ColumnOrder,
    /// Unit of the coordinates; None - the file's own unit ([`xyz_unit`]), else metres.
    pub unit: Option<SurveyUnit>,
}

/// First rows of a survey file as the import dialog shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct XyzPreview {
    pub sep: Sep,
    /// The file's decimal mark is a comma.
    pub decimal_comma: bool,
    /// Header cells, when the first row is a header.
    pub header: Option<Vec<String>>,
    /// First data rows split into cells.
    pub rows: Vec<Vec<String>>,
    /// Points of those rows with the given order, in file units (None — row unreadable).
    pub points: Vec<Option<(f64, f64, f64)>>,
    /// Unit the file names (header cell or `# units:` comment).
    pub unit: Option<SurveyUnit>,
}

fn is_comment(l: &str) -> bool {
    l.starts_with('#') || l.starts_with("//") || l.starts_with(';') || l.starts_with('!')
}

fn split(l: &str, sep: Sep) -> Vec<String> {
    let clean = |t: &str| t.trim().trim_matches('"').trim().to_string();
    match sep {
        Sep::Tab => l.split('\t').map(clean).collect(),
        Sep::Semi => l.split(';').map(clean).collect(),
        Sep::Comma => l.split(',').map(clean).collect(),
        Sep::Space => l.split_whitespace().map(clean).collect(),
    }
}

fn num(t: &str, mark: Option<Mark>) -> Result<FileNum, NumErr> {
    parse_num(t, mark)
}

/// The file's decimal mark: `.` for comma-separated columns, else from all cells once.
fn file_mark(lines: &[&str], sep: Sep) -> Option<Mark> {
    if sep == Sep::Comma {
        return Some(Mark::Point);
    }
    detect_mark(lines.iter().flat_map(|l| split(l, sep)))
}

/// Whitespace-separated row whose every comma sits between digits (`512345,12 6123456,78 101,5`).
fn decimal_comma_row(l: &str) -> bool {
    if l.split_whitespace().count() < 3 {
        return false;
    }
    let b = l.as_bytes();
    b.iter()
        .enumerate()
        .filter(|&(_, &c)| c == b',')
        .all(|(i, _)| {
            i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit()
        })
}

/// Separator from a sample of rows (header and data); a comma inside whitespace columns is a decimal.
fn sniff(lines: &[&str]) -> (Sep, bool) {
    let sample: Vec<&str> = lines.iter().take(50).copied().collect();
    let n = sample.len().max(1);
    let share = |c: char| sample.iter().filter(|l| l.contains(c)).count() * 2 >= n;
    if share('\t') {
        return (Sep::Tab, false);
    }
    if share(';') {
        return (Sep::Semi, false);
    }
    let with_comma: Vec<&&str> = sample
        .iter()
        .filter(|l| l.contains(',') && l.bytes().any(|c| c.is_ascii_digit()))
        .collect();
    if with_comma.is_empty() {
        return (Sep::Space, false);
    }
    if with_comma.iter().all(|l| decimal_comma_row(l)) {
        return (Sep::Space, true);
    }
    (Sep::Comma, false)
}

type Roles = [Option<usize>; 3];

/// Column roles from a header row (None when the row is not a header) and the unit its
/// coordinate cells name (None when none or they disagree).
fn header_units(cols: &[String]) -> Option<(Roles, Option<SurveyUnit>)> {
    let mut role = [None; 3];
    let mut units = [None; 3];
    let mut named = 0;
    for (i, cell) in cols.iter().enumerate() {
        let (name, unit) = split_unit(cell);
        let c = name.to_ascii_lowercase();
        let r = match c.as_str() {
            "x" | "e" | "east" | "easting" => Some(0),
            "y" | "n" | "north" | "northing" => Some(1),
            "z" | "h" | "elev" | "elevation" | "height" | "alt" | "altitude" => Some(2),
            "p" | "id" | "pt" | "point" | "name" | "no" | "#" | "d" | "desc" | "code"
            | "description" => {
                named += 1;
                None
            }
            _ => None,
        };
        if let Some(r) = r {
            named += 1;
            if role[r].is_none() {
                role[r] = Some(i);
                units[r] = unit;
            }
        }
    }
    if !(role.iter().all(Option::is_some) && named >= 3) {
        return None;
    }
    let mut named_units = units.iter().flatten();
    let unit = named_units
        .next()
        .copied()
        .filter(|u| named_units.all(|v| v == u));
    Some((role, unit))
}

fn data_lines(text: &str) -> Vec<&str> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_comment(l))
        .collect()
}

/// Separator, the file's decimal mark, header roles (None — no header row).
fn layout(lines: &[&str]) -> (Sep, Option<Mark>, Option<Roles>) {
    let (sep, mark, head) = layout_units(lines);
    (sep, mark, head.map(|h| h.0))
}

/// As [`layout`], with the unit the header names.
fn layout_units(lines: &[&str]) -> (Sep, Option<Mark>, Option<(Roles, Option<SurveyUnit>)>) {
    let (sep, _) = sniff(lines);
    let mark = file_mark(lines, sep);
    let head = lines.first().and_then(|first| {
        let head = split(first, sep);
        if head.iter().any(|t| num(t, mark).is_err()) {
            header_units(&head)
        } else {
            None
        }
    });
    (sep, mark, head)
}

/// Unit the survey file names: a unit in the header's x/y/z cells, else a `# units: mm` comment.
pub fn xyz_unit(text: &str) -> Option<SurveyUnit> {
    let lines = data_lines(text);
    if let Some((_, Some(u))) = layout_units(&lines).2 {
        return Some(u);
    }
    text.lines()
        .map(str::trim)
        .filter(|l| is_comment(l))
        .find_map(comment_unit)
}

/// A point and whether one of its numbers carried digit groups.
type RowPoint = Result<((f64, f64, f64), bool), NumErr>;

fn at(t: &[String], c: [usize; 3], mark: Option<Mark>) -> RowPoint {
    let mut v = [0.0; 3];
    let mut grouped = false;
    for (k, &col) in c.iter().enumerate() {
        let n = num(t.get(col).ok_or(NumErr::Invalid)?, mark)?;
        v[k] = n.value;
        grouped |= n.grouped;
    }
    Ok(((v[0], v[1], v[2]), grouped))
}

fn row_point(
    t: &[String],
    mark: Option<Mark>,
    role: Option<[Option<usize>; 3]>,
    order: ColumnOrder,
) -> RowPoint {
    if let Some(c) = order.cols() {
        return at(t, c, mark);
    }
    match role {
        Some([Some(x), Some(y), Some(z)]) => at(t, [x, y, z], mark),
        _ => positional(t, mark),
    }
}

/// Parses survey points (column order from the header or auto). Rows that do not give three
/// finite numbers count in `skipped`, those with an ambiguous number also in `ambiguous`.
pub fn parse_xyz(text: &str) -> Result<TopoSource, String> {
    parse_xyz_with(text, &XyzOptions::default())
}

/// Parses survey points with an explicit column order (Auto — as [`parse_xyz`]).
pub fn parse_xyz_with(text: &str, o: &XyzOptions) -> Result<TopoSource, String> {
    let lines = data_lines(text);
    if lines.is_empty() {
        return Err("no points".into());
    }
    let (sep, mark, role) = layout(&lines);
    let mut out = TopoSource::default();
    let skip = usize::from(role.is_some());
    for l in &lines[skip..] {
        match row_point(&split(l, sep), mark, role, o.order) {
            Ok((p, grouped)) => {
                out.points.push(p);
                out.grouped += usize::from(grouped);
            }
            Err(e) => {
                out.skipped += 1;
                out.ambiguous += usize::from(e == NumErr::Ambiguous);
            }
        }
    }
    if out.points.is_empty() {
        return Err(format!(
            "no points ({} rows unreadable, {} with ambiguous numbers)",
            out.skipped, out.ambiguous
        ));
    }
    let unit = o.unit.or_else(|| xyz_unit(text)).unwrap_or_default();
    out.scale(unit.meters());
    Ok(out)
}

/// First `n` data rows with their cells and the points the given order reads from them.
pub fn preview_xyz(text: &str, n: usize, o: &XyzOptions) -> XyzPreview {
    let lines = data_lines(text);
    let (sep, mark, role) = layout(&lines);
    let decimal_comma = mark == Some(Mark::Comma);
    let skip = usize::from(role.is_some());
    let header = role.and_then(|_| lines.first().map(|l| split(l, sep)));
    let rows: Vec<Vec<String>> = lines
        .iter()
        .skip(skip)
        .take(n)
        .map(|l| split(l, sep))
        .collect();
    let points = rows
        .iter()
        .map(|t| row_point(t, mark, role, o.order).ok().map(|(p, _)| p))
        .collect();
    XyzPreview {
        sep,
        decimal_comma,
        header,
        rows,
        points,
        unit: xyz_unit(text),
    }
}

/// No header, auto order: `x y z …`, or `P N E Z [D]` when the first column is text (an id).
/// An ambiguous first number is reported, never taken for a text id.
fn positional(t: &[String], mark: Option<Mark>) -> RowPoint {
    match num(t.first().ok_or(NumErr::Invalid)?, mark) {
        Err(NumErr::Invalid) => at(t, [2, 1, 3], mark),
        Err(e) => Err(e),
        Ok(_) => at(t, [0, 1, 2], mark),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(text: &str, order: ColumnOrder) -> TopoSource {
        parse_xyz_with(
            text,
            &XyzOptions {
                order,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn csv_with_header() {
        let s = parse_xyz("Name,Easting,Northing,Elevation,Code\nP1,100.5,200,10.25,TOP\nP2,101,201,10.5,TOP\nbad,row\n").unwrap();
        assert_eq!(s.points, vec![(100.5, 200.0, 10.25), (101.0, 201.0, 10.5)]);
        assert_eq!(s.skipped, 1);
    }

    #[test]
    fn xyz_space_and_comments() {
        let s = parse_xyz("# survey\n1.0 2.0 3.0\n\n4 5 6 0.7\n// end\n").unwrap();
        assert_eq!(s.points, vec![(1.0, 2.0, 3.0), (4.0, 5.0, 6.0)]);
        assert_eq!(s.skipped, 0);
    }

    #[test]
    fn integer_rows_with_code_are_xyz() {
        let s = parse_xyz("100 200 10 3\n101 201 11 3\n").unwrap();
        assert_eq!(s.points, vec![(100.0, 200.0, 10.0), (101.0, 201.0, 11.0)]);
    }

    #[test]
    fn semicolon_decimal_comma() {
        let s = parse_xyz("x;y;z\n1,5;2,25;100,125\n3;4;5\n").unwrap();
        assert_eq!(s.points, vec![(1.5, 2.25, 100.125), (3.0, 4.0, 5.0)]);
    }

    #[test]
    fn space_separated_decimal_comma() {
        let s = parse_xyz("512345,12 6123456,78 101,5\n512346 6123457,5 102\n").unwrap();
        assert_eq!(
            s.points,
            vec![(512345.12, 6123456.78, 101.5), (512346.0, 6123457.5, 102.0)]
        );
        assert_eq!(s.skipped, 0);
        let p = preview_xyz("512345,12 6123456,78 101,5\n", 5, &XyzOptions::default());
        assert_eq!((p.sep, p.decimal_comma), (Sep::Space, true));
        // a comma-separated file stays comma-separated
        let c = parse_xyz("1.5,2.5,3.5\n4,5,6\n").unwrap();
        assert_eq!(c.points, vec![(1.5, 2.5, 3.5), (4.0, 5.0, 6.0)]);
    }

    #[test]
    fn separator_sniffed_over_several_rows() {
        // first row has no comma, later rows decide; tabs win when most rows have them
        let s = parse_xyz("1 2 3\n4,5 5,5 6,5\n").unwrap();
        assert_eq!(s.points, vec![(1.0, 2.0, 3.0), (4.5, 5.5, 6.5)]);
        let t = parse_xyz("1\t2\t3\n4\t5\t6\n").unwrap();
        assert_eq!(t.points.len(), 2);
    }

    #[test]
    fn pnezd_needs_text_id_or_explicit_order() {
        let t = parse_xyz("A1\t10\t20\t30\n").unwrap();
        assert_eq!(t.points[0], (20.0, 10.0, 30.0));
        let text = "1,5000.0,3000.0,12.5,GND\n2,5001.0,3002.0,12.75,GND\n";
        let auto = parse_xyz(text).unwrap();
        assert_eq!(
            auto.points[0],
            (1.0, 5000.0, 3000.0),
            "numeric id is not guessed"
        );
        assert_eq!(
            with(text, ColumnOrder::Pnez).points[0],
            (3000.0, 5000.0, 12.5)
        );
        assert_eq!(
            with(text, ColumnOrder::Penz).points[0],
            (5000.0, 3000.0, 12.5)
        );
        assert_eq!(
            with("5000 3000 12\n", ColumnOrder::Nez).points[0],
            (3000.0, 5000.0, 12.0)
        );
    }

    #[test]
    fn preview_rows_and_mapping() {
        let text = "P N E Z\n1 5000 3000 12.5\n2 5001 3002 12.75\n";
        let p = preview_xyz(text, 1, &XyzOptions::default());
        assert_eq!(p.header.as_ref().map(Vec::len), Some(4));
        assert_eq!(p.rows.len(), 1);
        assert_eq!(
            p.points,
            vec![Some((3000.0, 5000.0, 12.5))],
            "header maps E/N"
        );
        let q = preview_xyz(
            text,
            5,
            &XyzOptions {
                order: ColumnOrder::Xyz,
                ..Default::default()
            },
        );
        assert_eq!(q.points[1], Some((2.0, 5001.0, 3002.0)));
    }

    #[test]
    fn dot_decimal_file() {
        let s = parse_xyz("x y z\n1.250 2.5 100.125\n1234.56 5 6\n").unwrap();
        assert_eq!(s.points, vec![(1.25, 2.5, 100.125), (1234.56, 5.0, 6.0)]);
        assert_eq!((s.skipped, s.ambiguous, s.grouped), (0, 0, 0));
    }

    #[test]
    fn comma_decimal_file_semicolon_and_tab() {
        // `2,5` proves the comma: `1,250` is then 1.25, and `1.234,5` / `1 250,5` are grouped
        let s = parse_xyz("x;y;z\n1,250;2,5;3\n1.234,5;1 250,5;7\n").unwrap();
        assert_eq!(s.points, vec![(1.25, 2.5, 3.0), (1234.5, 1250.5, 7.0)]);
        assert_eq!((s.skipped, s.grouped), (0, 1));
        let t = parse_xyz("1,250\t2,5\t3\n4\t5,75\t6\n").unwrap();
        assert_eq!(t.points, vec![(1.25, 2.5, 3.0), (4.0, 5.75, 6.0)]);
        let p = preview_xyz("1,250\t2,5\t3\n", 5, &XyzOptions::default());
        assert_eq!((p.sep, p.decimal_comma), (Sep::Tab, true));
    }

    #[test]
    fn comma_columns_with_dot_decimals() {
        let s = parse_xyz("1.5,2.250,3\n1250.75,2,3.5\n").unwrap();
        assert_eq!(s.points, vec![(1.5, 2.25, 3.0), (1250.75, 2.0, 3.5)]);
        assert_eq!((s.skipped, s.ambiguous), (0, 0));
    }

    #[test]
    fn space_file_with_point_mark_reads_comma_groups() {
        // `12.75` proves the point, so `1,234.56` and `1,250` are digit groups, never 1.234
        let s = parse_xyz("1,234.56 2000 12.75\n1,250 2001 13\n").unwrap();
        assert_eq!(
            s.points,
            vec![(1234.56, 2000.0, 12.75), (1250.0, 2001.0, 13.0)]
        );
        assert_eq!((s.skipped, s.grouped), (0, 2));
    }

    #[test]
    fn ambiguous_rows_are_reported_not_imported() {
        // no cell proves a mark: `1,250` may be 1.25 or 1250
        let s = parse_xyz("1,250 2,500 3\n10 20 30\n").unwrap();
        assert_eq!(s.points, vec![(10.0, 20.0, 30.0)]);
        assert_eq!((s.skipped, s.ambiguous), (1, 1));
        let e = parse_xyz("1,250 2,500 3,750\n").unwrap_err();
        assert!(e.contains("1 with ambiguous"), "{e}");
        // an ambiguous first column is not taken for a point id
        let p = parse_xyz("1,250 5000 3000 12\n").unwrap_err();
        assert!(p.contains("1 with ambiguous"), "{p}");
        let q = preview_xyz("1,250 2,500 3\n", 5, &XyzOptions::default());
        assert_eq!((q.decimal_comma, q.points.clone()), (false, vec![None]));
    }

    #[test]
    fn convention_is_per_file_not_global() {
        // the same text reads the same in any order: no state between files, no UI language
        let comma = "1,250;2,5;3\n";
        let dot = "1,250 12.5 3\n";
        let a = parse_xyz(comma).unwrap();
        let b = parse_xyz(dot).unwrap();
        assert_eq!(parse_xyz(comma).unwrap(), a);
        assert_eq!(a.points, vec![(1.25, 2.5, 3.0)]);
        assert_eq!(b.points, vec![(1250.0, 12.5, 3.0)]);
    }

    #[test]
    fn header_unit_is_respected_and_choice_overrides_it() {
        let mm = "x(mm);y(mm);z(mm)\n1000;2000;101500\n3500,5;2000;101000\n";
        assert_eq!(xyz_unit(mm), Some(SurveyUnit::Mm));
        let p = preview_xyz(mm, 5, &XyzOptions::default());
        assert_eq!(p.unit, Some(SurveyUnit::Mm));
        assert_eq!(p.points[0], Some((1000.0, 2000.0, 101500.0)), "preview in file units");
        let s = parse_xyz(mm).unwrap();
        assert!((s.points[0].0 - 1.0).abs() < 1e-12 && (s.points[0].2 - 101.5).abs() < 1e-12);
        assert!((s.points[1].0 - 3.5005).abs() < 1e-12, "decimal comma per file, then scaled");
        let as_m = parse_xyz_with(
            mm,
            &XyzOptions {
                unit: Some(SurveyUnit::M),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(as_m.points[0], (1000.0, 2000.0, 101500.0));
        // `# units:` comment, and ft in a TSV header
        assert_eq!(xyz_unit("# units: ft\n1 2 3\n"), Some(SurveyUnit::Ft));
        assert_eq!(xyz_unit("E [ft]\tN [ft]\tZ [ft]\n1\t2\t3\n"), Some(SurveyUnit::Ft));
        // no unit, or coordinate cells that disagree: the dialog asks
        assert_eq!(xyz_unit("x y z\n1 2 3\n"), None);
        assert_eq!(xyz_unit("x(m);y(m);z(mm)\n1;2;3\n"), None);
        assert_eq!(xyz_unit("1 2 3\n"), None);
    }

    #[test]
    fn same_points_in_m_and_mm_give_equal_terrains() {
        let m = parse_xyz("x y z\n10.5 20 101.25\n80 60 104\n").unwrap();
        let mm = parse_xyz_with(
            "x y z\n10500 20000 101250\n80000 60000 104000\n",
            &XyzOptions {
                unit: Some(SurveyUnit::Mm),
                ..Default::default()
            },
        )
        .unwrap();
        for (a, b) in m.points.iter().zip(&mm.points) {
            let d = (a.0 - b.0).abs().max((a.1 - b.1).abs()).max((a.2 - b.2).abs());
            assert!(d < 1e-9, "{a:?} vs {b:?}");
        }
        assert_eq!(m.points.len(), mm.points.len());
    }

    #[test]
    fn tsv_header_order() {
        let s = parse_xyz("z\tx\ty\n7\t1\t2\n").unwrap();
        assert_eq!(s.points, vec![(1.0, 2.0, 7.0)]);
        assert!(parse_xyz("# nothing\n").is_err());
    }
}
