//! Exchange safety, XLSX: one edited cell per column type changes the model exactly there and
//! nowhere else; every kind of bad workbook gives its documented result; one undo restores the
//! whole model. The model is compared table by table (every row, every column), not by a few
//! chosen values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use calamine::{open_workbook, Data, DataType, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, RoomSpec, WallSpec};
use rust_xlsxwriter::{Formula, Workbook};

use super::*;

type Grid = Vec<Vec<Data>>;

// ---------------------------------------------------------------------------------------------
// fixture and whole-model comparison

fn lay(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// Two levels, two wall types, a closed box of four walls, a door in wall 1, one room.
fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    m.add_level("L2", 3.5).unwrap();
    let ext = m
        .add_wall_type("ext", &[lay("brick", 0.2), lay("insulation", 0.1)])
        .unwrap();
    m.add_wall_type("int", &[lay("gypsum", 0.1)]).unwrap();
    let p = [(0.0, 0.0), (5.0, 0.0), (5.0, 4.0), (0.0, 4.0)];
    for k in 0..4 {
        m.add_wall(&WallSpec {
            a: p[k],
            b: p[(k + 1) % 4],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ext,
            offset0: None,
        })
        .unwrap();
    }
    m.add_opening(
        1,
        &OpeningSpec {
            along: 1.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    m.add_room(&RoomSpec {
        level: l1,
        name: "Hall".into(),
        number: "101".into(),
        point: (2.5, 2.0),
        height: 3.0,
    })
    .unwrap();
    m
}

/// (table, rowid) -> (column, value) for every table that holds model data.
type Dump = BTreeMap<(String, i64), Vec<(String, String)>>;

/// Bookkeeping that is not model data: the operation log, counters and caches.
fn bookkeeping(table: &str) -> bool {
    table == "op"
        || table == "meta"
        || table.starts_with("op_")
        || table.starts_with("sqlite_")
        || table.ends_with("_cache")
        || table.starts_with("change")
        || table.starts_with("journal")
}

fn derived(table: &str) -> bool {
    table.ends_with("_qty")
        || ["struct_issue", "shaft_cut", "solid_cut", "solid_base", "dim_geo", "join_cache"]
            .contains(&table)
}

/// Derived tables are recomputed after an undo, so their row ids carry no meaning: compare
/// them by content. Everything the user can edit keeps its row ids.
fn norm(d: Dump) -> Dump {
    let mut out = Dump::new();
    let mut by: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for ((t, rid), cells) in d {
        if derived(&t) {
            by.entry(t).or_default().push(format!("{cells:?}"));
        } else {
            out.insert((t, rid), cells);
        }
    }
    for (t, mut rows) in by {
        rows.sort();
        for (i, r) in rows.into_iter().enumerate() {
            out.insert((t.clone(), i as i64), vec![("rows".into(), r)]);
        }
    }
    out
}

fn dump(m: &Model) -> Dump {
    let db = m.sql();
    let tables: Vec<String> = {
        let mut st = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let v: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        v
    };
    let mut out = Dump::new();
    for t in tables {
        if bookkeeping(&t) {
            continue;
        }
        let (mut st, has_rowid) = match db.prepare(&format!("SELECT rowid, * FROM \"{t}\"")) {
            Ok(s) => (s, true),
            Err(_) => (db.prepare(&format!("SELECT 0, * FROM \"{t}\"")).unwrap(), false),
        };
        let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
        let mut rows = st.query([]).unwrap();
        let mut k = 0i64;
        while let Some(r) = rows.next().unwrap() {
            let rid: i64 = if has_rowid { r.get(0).unwrap() } else { k };
            k += 1;
            let cells = (1..names.len())
                .map(|i| (names[i].clone(), format!("{:?}", r.get_ref(i).unwrap())))
                .collect();
            out.insert((t.clone(), rid), cells);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
struct Diff {
    table: String,
    rowid: i64,
    /// "changed", "added" or "removed"
    kind: &'static str,
    /// Changed columns (all columns for an added or removed row).
    cols: Vec<String>,
}

fn diff(a: &Dump, b: &Dump) -> Vec<Diff> {
    let mut out = vec![];
    for (k, va) in a {
        match b.get(k) {
            None => out.push(Diff {
                table: k.0.clone(),
                rowid: k.1,
                kind: "removed",
                cols: va.iter().map(|c| c.0.clone()).collect(),
            }),
            Some(vb) if vb != va => out.push(Diff {
                table: k.0.clone(),
                rowid: k.1,
                kind: "changed",
                cols: va
                    .iter()
                    .zip(vb)
                    .filter(|(x, y)| x != y)
                    .map(|(x, _)| x.0.clone())
                    .collect(),
            }),
            Some(_) => {}
        }
    }
    for (k, vb) in b {
        if !a.contains_key(k) {
            out.push(Diff {
                table: k.0.clone(),
                rowid: k.1,
                kind: "added",
                cols: vb.iter().map(|c| c.0.clone()).collect(),
            });
        }
    }
    out
}

fn show(d: &[Diff]) -> String {
    d.iter()
        .map(|x| format!("  {} #{} {} {:?}", x.table, x.rowid, x.kind, x.cols))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ops(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// workbook plumbing

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join("ddc_xlsx_exchange");
    std::fs::create_dir_all(&d).unwrap();
    let n = N.fetch_add(1, Ordering::Relaxed);
    let tag: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    d.join(format!("{}_{n}_{tag}.xlsx", std::process::id()))
}

fn export(m: &Model, tag: &str) -> PathBuf {
    let p = tmp(tag);
    export_xlsx(m, &p).unwrap();
    p
}

/// A cell value to write.
#[derive(Clone, Copy, Debug)]
enum V {
    N(f64),
    S(&'static str),
    /// Formula and the value the spreadsheet cached for it.
    F(&'static str, &'static str),
    Empty,
}

fn data(v: V) -> Data {
    match v {
        V::N(x) => Data::Float(x),
        V::S(t) => Data::String(t.into()),
        V::F(f, r) => Data::String(format!("\u{1}{f}\u{1}{r}")),
        V::Empty => Data::Empty,
    }
}

/// Reads `src`, lets `f` edit each sheet, writes `dst`. Sheets named in `hidden` are hidden.
fn rewrite(src: &Path, dst: &Path, hidden: &[&str], f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut grid: Grid = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        f(&name, &mut grid);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(t) if t.starts_with('\u{1}') => {
                        let mut p = t[1..].split('\u{1}');
                        let (fm, res) = (p.next().unwrap(), p.next().unwrap());
                        ws.write_formula(i, j, Formula::new(fm).set_result(res))
                            .map(|_| ())
                    }
                    Data::String(t) => ws.write_string(i, j, t).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
        if hidden.contains(&name.as_str()) {
            ws.set_hidden(true);
        }
    }
    out.save(dst).unwrap();
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column '{header}' in {:?}", g[0]))
}

fn set(g: &mut Grid, row: usize, header: &str, v: Data) {
    let c = col(g, header);
    while g[row].len() <= c {
        g[row].push(Data::Empty);
    }
    g[row][c] = v;
}

/// Grid row of the element with `id` (the sheet has an `id` column).
fn row_of(g: &Grid, id: i64) -> usize {
    let c = col(g, "id");
    (1..g.len())
        .find(|&r| g[r].get(c).and_then(|d| d.as_f64()) == Some(id as f64))
        .unwrap_or_else(|| panic!("no row with id {id}"))
}

fn add_col(g: &mut Grid, header: &str, vals: &[&str]) {
    let w = g.iter().map(Vec::len).max().unwrap_or(0);
    for r in g.iter_mut() {
        r.resize(w, Data::Empty);
    }
    g[0].push(Data::String(header.into()));
    for (i, r) in g.iter_mut().enumerate().skip(1) {
        r.push(
            vals.get(i - 1)
                .map_or(Data::Empty, |t| Data::String((*t).into())),
        );
    }
}

fn preview() -> ImportOptions {
    ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    }
}

fn apply() -> ImportOptions {
    ImportOptions {
        apply: true,
        ..preview()
    }
}

// ---------------------------------------------------------------------------------------------
// one edited cell per column type

#[derive(Clone, Copy)]
enum Row {
    /// The row whose `id` column has this value.
    Id(i64),
    /// Grid row (1 = first data row), for sheets without an `id` column.
    Index(usize),
}

struct Case {
    /// Column type and variant, for the failure message.
    family: &'static str,
    sheet: &'static str,
    row: Row,
    header: &'static str,
    value: V,
    /// Model table and column that must change, and no other column of no other row.
    table: &'static str,
    column: &'static str,
    /// Derived tables that may be recomputed from the edit (listed, not guessed).
    derived: &'static [&'static str],
    /// SQL returning 1 when the model holds the new value.
    verify: &'static str,
    /// Part of the one-workbook test that edits one cell of every type at once.
    together: bool,
}

const WALL_DERIVED: &[&str] = &["wall_qty", "geom_join"];

const CASES: &[Case] = &[
    // number with units
    Case {
        family: "number [m], plain number",
        sheet: WALLS,
        row: Row::Id(2),
        header: "height [m]",
        value: V::N(3.5),
        table: "wall",
        column: "height",
        derived: WALL_DERIVED,
        verify: "SELECT abs(height-3.5)<1e-9 FROM wall WHERE id=2",
        together: true,
    },
    Case {
        family: "number [m], text cell '3.5'",
        sheet: WALLS,
        row: Row::Id(2),
        header: "height [m]",
        value: V::S("3.5"),
        table: "wall",
        column: "height",
        derived: WALL_DERIVED,
        verify: "SELECT abs(height-3.5)<1e-9 FROM wall WHERE id=2",
        together: false,
    },
    Case {
        family: "number [m], decimal comma '3,5'",
        sheet: WALLS,
        row: Row::Id(2),
        header: "height [m]",
        value: V::S("3,5"),
        table: "wall",
        column: "height",
        derived: WALL_DERIVED,
        verify: "SELECT abs(height-3.5)<1e-9 FROM wall WHERE id=2",
        together: false,
    },
    Case {
        family: "number [m], text with unit '3500 mm'",
        sheet: WALLS,
        row: Row::Id(2),
        header: "height [m]",
        value: V::S("3500 mm"),
        table: "wall",
        column: "height",
        derived: WALL_DERIVED,
        verify: "SELECT abs(height-3.5)<1e-9 FROM wall WHERE id=2",
        together: false,
    },
    Case {
        family: "number [m], padded text ' 3.5 '",
        sheet: WALLS,
        row: Row::Id(2),
        header: "height [m]",
        value: V::S(" 3.5 "),
        table: "wall",
        column: "height",
        derived: WALL_DERIVED,
        verify: "SELECT abs(height-3.5)<1e-9 FROM wall WHERE id=2",
        together: false,
    },
    Case {
        family: "number [m], base offset",
        sheet: WALLS,
        row: Row::Id(3),
        header: "base_offset [m]",
        value: V::N(0.25),
        table: "wall",
        column: "base_offset",
        derived: WALL_DERIVED,
        verify: "SELECT abs(base_offset-0.25)<1e-9 FROM wall WHERE id=3",
        together: false,
    },
    Case {
        family: "number [m], layer thickness",
        sheet: LAYERS,
        row: Row::Index(2),
        header: "thickness [m]",
        value: V::N(0.15),
        table: "wall_type_layer",
        column: "thickness",
        derived: &["wall_qty", "geom_join", "room_qty"],
        verify: "SELECT abs(y.thickness-0.15)<1e-9 FROM wall_type_layer y \
                 JOIN wall_type t ON t.id=y.wtype WHERE t.name='ext' AND y.pos=1",
        together: false,
    },
    Case {
        family: "number [m], level elevation",
        sheet: LEVELS,
        row: Row::Id(2),
        header: "elevation [m]",
        value: V::N(4.0),
        table: "level",
        column: "elevation",
        derived: &[],
        verify: "SELECT abs(elevation-4.0)<1e-9 FROM level WHERE name='L2'",
        together: true,
    },
    // choice (a name from a list)
    Case {
        family: "choice, wall type",
        sheet: WALLS,
        row: Row::Id(3),
        header: "type",
        value: V::S("int"),
        table: "wall",
        column: "wtype",
        derived: WALL_DERIVED,
        verify: "SELECT t.name='int' FROM wall w JOIN wall_type t ON t.id=w.wtype WHERE w.id=3",
        together: true,
    },
    // text
    Case {
        family: "text, room name",
        sheet: ROOMS,
        row: Row::Id(1),
        header: "name",
        value: V::S("\u{41a}\u{443}\u{445}\u{43d}\u{44f}"), // Russian "kitchen"
        table: "room",
        column: "name",
        derived: &["room_qty"],
        verify: "SELECT name='\u{41a}\u{443}\u{445}\u{43d}\u{44f}' FROM room WHERE id=1",
        together: true,
    },
    Case {
        family: "text, room number given as a number",
        sheet: ROOMS,
        row: Row::Id(1),
        header: "number",
        value: V::N(102.0),
        table: "room",
        column: "number",
        derived: &["room_qty"],
        verify: "SELECT number IN ('102','102.0') FROM room WHERE id=1",
        together: false,
    },
    // level (a name from the level list)
    Case {
        family: "level, wall level",
        sheet: WALLS,
        row: Row::Id(4),
        header: "level",
        value: V::S("L2"),
        table: "wall",
        column: "level",
        derived: &["wall_qty", "geom_join", "room_qty"],
        verify: "SELECT l.name='L2' FROM wall w JOIN level l ON l.id=w.level WHERE w.id=4",
        together: true,
    },
    // material
    Case {
        family: "material, layer material",
        sheet: LAYERS,
        row: Row::Index(1),
        header: "material",
        value: V::S("stone"),
        table: "wall_type_layer",
        column: "material",
        derived: WALL_DERIVED,
        verify: "SELECT y.material='stone' FROM wall_type_layer y \
                 JOIN wall_type t ON t.id=y.wtype WHERE t.name='ext' AND y.pos=0",
        together: true,
    },
];

fn resolve(g: &Grid, r: Row) -> usize {
    match r {
        Row::Id(id) => row_of(g, id),
        Row::Index(i) => i,
    }
}

fn describe(c: &Case) -> String {
    format!("[{}] {}!{} = {:?}", c.family, c.sheet, c.header, c.value)
}

fn edit_cases<'a>(cases: &'a [&'a Case]) -> impl Fn(&str, &mut Grid) + 'a {
    move |sheet, g| {
        // export writes Info decimal=point (fb92e87a), where '3,5' is not a number (41b8b085):
        // a decimal-comma value is typed in a comma book
        if sheet == INFO && cases.iter().any(|c| matches!(c.value, V::S(t) if t.contains(','))) {
            for r in g.iter_mut() {
                if r.len() > 1 && r[0].to_string() == "decimal" {
                    r[1] = Data::String("comma".into());
                }
            }
        }
        for c in cases.iter().filter(|c| c.sheet == sheet) {
            let r = resolve(g, c.row);
            set(g, r, c.header, data(c.value));
        }
    }
}

/// Preview changes nothing, apply changes exactly the listed cells, one undo restores all.
fn run_cases(label: &str, cases: &[&Case]) {
    let mut m = model();
    let before = dump(&m);
    let op0 = ops(&m);
    let src = export(&m, label);
    let dst = tmp(label);
    rewrite(&src, &dst, &[], edit_cases(cases));
    let ctx = cases.iter().map(|c| describe(c)).collect::<Vec<_>>().join("; ");

    let pre = import_xlsx_with(&mut m, &dst, preview())
        .unwrap_or_else(|e| panic!("{ctx}: preview failed: {e}"));
    assert!(
        pre.rejected.is_empty(),
        "{ctx}: a valid edit was rejected: {:?}",
        pre.rejected
    );
    assert!(!pre.applied, "{ctx}: preview must not apply");
    assert_eq!(
        pre.changes.len(),
        cases.len(),
        "{ctx}: preview must list one change per edited cell, got {:?}",
        pre.changes
    );
    for c in cases {
        assert!(
            pre.changes.iter().any(|x| x.sheet == c.sheet),
            "{ctx}: no change listed on sheet {}",
            c.sheet
        );
    }
    assert_eq!(norm(dump(&m)), norm(before.clone()), "{ctx}: the preview changed the model");
    assert_eq!(ops(&m), op0, "{ctx}: the preview logged an operation");

    let rep = import_xlsx_with(&mut m, &dst, apply())
        .unwrap_or_else(|e| panic!("{ctx}: apply failed: {e}"));
    assert!(rep.applied, "{ctx}: not applied, rejected {:?}", rep.rejected);
    for c in cases {
        let ok: i64 = m
            .sql()
            .query_row(c.verify, [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("{}: verify query failed: {e}", describe(c)));
        assert_eq!(ok, 1, "{}: the model does not hold the new value", describe(c));
    }

    let d = diff(&before, &dump(&m));
    for c in cases {
        let own: Vec<&Diff> = d.iter().filter(|x| x.table == c.table).collect();
        let cols: Vec<&str> = own
            .iter()
            .filter(|x| x.kind == "changed")
            .flat_map(|x| x.cols.iter().map(String::as_str))
            .collect();
        assert!(
            cols.contains(&c.column),
            "{}: column {}.{} did not change; changes:\n{}",
            describe(c),
            c.table,
            c.column,
            show(&d)
        );
    }
    let per_table = |t: &str| cases.iter().filter(|c| c.table == t).count();
    for x in &d {
        // recomputed tables follow any edit; their content is still compared after undo
        let allowed = cases.iter().any(|c| c.table == x.table && x.cols == [c.column])
            || derived(&x.table)
            || cases
                .iter()
                .any(|c| c.derived.contains(&x.table.as_str()));
        assert!(
            allowed,
            "{ctx}: nothing else may change, but found {} #{} {} {:?}\nall changes:\n{}",
            x.table,
            x.rowid,
            x.kind,
            x.cols,
            show(&d)
        );
    }
    for t in cases.iter().map(|c| c.table) {
        let rows = d
            .iter()
            .filter(|x| x.table == t && x.kind == "changed")
            .count();
        assert!(
            rows <= per_table(t),
            "{ctx}: {rows} rows of {t} changed for {} edited cells\n{}",
            per_table(t),
            show(&d)
        );
    }

    assert_eq!(ops(&m), op0 + 1, "{ctx}: the import must be one operation");
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()), "{ctx}: one undo did not restore the model");
    assert_eq!(ops(&m), op0, "{ctx}: undo left an operation behind");
}

/// A formula with a cached value in a model column is listed, never applied (tests_formula_keep.rs).
#[test]
fn formula_with_cached_value_is_listed_not_applied() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "xs19_formula");
    let dst = tmp("xs19_formula_b");
    let c = Case {
        family: "number [m], formula with cached value",
        sheet: WALLS,
        row: Row::Id(2),
        header: "height [m]",
        value: V::F("=7/2", "3.5"),
        table: "wall",
        column: "height",
        derived: WALL_DERIVED,
        verify: "SELECT abs(height-3.5)<1e-9 FROM wall WHERE id=2",
        together: false,
    };
    rewrite(&src, &dst, &[], edit_cases(&[&c]));
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    let left = rep.ignored.iter().filter(|i| i.reason == super::msg::FORMULA_LEFT.1).count();
    assert_eq!(left, 1, "{:?}", rep.ignored);
    assert_eq!(norm(dump(&m)), norm(before));
}

#[test]
fn each_column_type_changes_exactly_its_cell_and_one_undo_restores() {
    for (i, c) in CASES.iter().enumerate() {
        run_cases(&format!("case{i}"), &[c]);
    }
}

#[test]
fn one_cell_of_every_type_in_one_workbook_is_one_undo_step() {
    let cases: Vec<&Case> = CASES.iter().filter(|c| c.together).collect();
    assert!(cases.len() >= 5, "the together set lost a column type");
    run_cases("together", &cases);
}

#[test]
fn an_unedited_workbook_is_a_no_op() {
    let mut m = model();
    let before = dump(&m);
    let op0 = ops(&m);
    let p = export(&m, "noop");
    let rep = import_xlsx_with(&mut m, &p, apply()).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(),
        "an unedited export must import as no change: {:?} {:?}",
        rep.changes,
        rep.rejected
    );
    assert_eq!(norm(dump(&m)), norm(before.clone()));
    assert_eq!(ops(&m), op0);
}

// ---------------------------------------------------------------------------------------------
// bad workbooks: each gives its documented result and changes nothing

struct Bad {
    label: &'static str,
    edit: fn(&str, &mut Grid),
    sheet: &'static str,
    /// Part of the header named by the rejection.
    column: &'static str,
    /// Part of the reason.
    reason: &'static str,
}

const BAD: &[Bad] = &[
    Bad {
        label: "text in a number column",
        edit: |s, g| {
            if s == WALLS {
                set(g, 1, "height [m]", data(V::S("abc")));
            }
        },
        sheet: WALLS,
        column: "height [m]",
        reason: "not a number",
    },
    Bad {
        label: "empty required number",
        edit: |s, g| {
            if s == WALLS {
                set(g, 1, "height [m]", data(V::Empty));
            }
        },
        sheet: WALLS,
        column: "height [m]",
        reason: "required",
    },
    Bad {
        label: "negative layer thickness",
        edit: |s, g| {
            if s == LAYERS {
                set(g, 1, "thickness [m]", data(V::N(-0.1)));
            }
        },
        sheet: LAYERS,
        column: "thickness [m]",
        reason: "> 0",
    },
    Bad {
        label: "unknown level name",
        edit: |s, g| {
            if s == WALLS {
                set(g, 1, "level", data(V::S("Nowhere")));
            }
        },
        sheet: WALLS,
        column: "level",
        reason: "unknown level 'Nowhere'",
    },
    Bad {
        label: "unknown wall type name",
        edit: |s, g| {
            if s == WALLS {
                set(g, 2, "type", data(V::S("nope")));
            }
        },
        sheet: WALLS,
        column: "type",
        reason: "nope",
    },
    Bad {
        label: "unknown id",
        edit: |s, g| {
            if s == WALLS {
                let r = g[1].clone();
                g.push(r);
                let n = g.len() - 1;
                set(g, n, "id", data(V::N(999.0)));
            }
        },
        sheet: WALLS,
        column: "id",
        // a row of an unknown id is ignored with this reason (22b83543)
        reason: "wall 999 no longer exists",
    },
    // an edited copy of a row is the edit, not a bad book: see tests_dup.rs
    Bad {
        label: "opening on a wall that does not exist",
        edit: |s, g| {
            if s == OPENINGS {
                set(g, 1, "wall_id", data(V::N(999.0)));
            }
        },
        sheet: OPENINGS,
        column: "wall_id",
        reason: "unknown or deleted wall",
    },
    Bad {
        label: "edit of a read-only column",
        edit: |s, g| {
            if s == WALLS {
                set(g, 1, "length [m] (read-only)", data(V::N(99.0)));
            }
        },
        sheet: WALLS,
        column: "length",
        reason: "read-only",
    },
];

#[test]
fn bad_workbooks_are_rejected_with_the_cell_and_change_nothing() {
    for b in BAD {
        let mut m = model();
        let before = dump(&m);
        let op0 = ops(&m);
        let src = export(&m, b.label);
        let dst = tmp(b.label);
        rewrite(&src, &dst, &[], b.edit);
        for (mode, o) in [("preview", preview()), ("apply", apply())] {
            let rep = import_xlsx_with(&mut m, &dst, o)
                .unwrap_or_else(|e| panic!("{} ({mode}): import failed: {e}", b.label));
            // a bad cell or row skips alone (36c71265): nothing else to apply, nothing blocks
            assert_eq!(rep.blocking(), 0, "{} ({mode}): a bad cell blocked the book", b.label);
            assert_eq!(rep.applied, mode == "apply", "{} ({mode}): applied flag", b.label);
            // read-only, stale and duplicate-id rows are ignored with a reason, not rejected
            // (22b83543): either list names the cell
            let named = |sheet: &str, column: &str, reason: &str| {
                sheet == b.sheet && column.contains(b.column) && reason.contains(b.reason)
            };
            let rejected = rep
                .rejected
                .iter()
                .find(|r| named(r.sheet.as_str(), r.column.as_str(), r.reason.as_str()));
            let ignored = rep
                .ignored
                .iter()
                .find(|r| named(r.sheet.as_str(), r.column.as_str(), r.reason.as_str()));
            let row = rejected.map(|r| r.row).or(ignored.map(|r| r.row)).unwrap_or_else(|| {
                panic!(
                    "{} ({mode}): expected a rejection on {}/{} containing '{}', got {:?} {:?}",
                    b.label, b.sheet, b.column, b.reason, rep.rejected, rep.ignored
                )
            });
            assert!(row > 0, "{} ({mode}): the rejection names no row", b.label);
            assert_eq!(norm(dump(&m)), norm(before.clone()), "{} ({mode}): the model changed", b.label);
            assert_eq!(ops(&m), op0, "{} ({mode}): an operation was logged", b.label);
        }
    }
}

#[test]
fn one_bad_cell_is_skipped_alone_and_valid_edits_apply() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "mixed");
    let dst = tmp("mixed");
    rewrite(&src, &dst, &[], |s, g| {
        if s == WALLS {
            set(g, 1, "height [m]", data(V::N(3.5)));
            set(g, 2, "height [m]", data(V::S("tall")));
        }
        if s == ROOMS {
            set(g, 1, "name", data(V::S("Office")));
        }
    });
    // a bad cell skips alone (36c71265): the valid edits beside it apply, the bad one does not
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{:?}", rep.rejected);
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    let r = &rep.rejected[0];
    assert_eq!((r.sheet.as_str(), r.row, r.column.as_str()), (WALLS, 3, "height [m]"));
    assert_ne!(norm(dump(&m)), norm(before.clone()), "the valid edits were not applied");
    let q = |sql: &str| -> i64 { m.sql().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(q("SELECT count(*) FROM room WHERE name='Office'"), 1);
    assert_eq!(q("SELECT count(*) FROM wall WHERE abs(height-3.5)<1e-9"), 1);
    assert_eq!(q("SELECT count(*) FROM wall WHERE abs(height-3.0)<1e-9"), 3, "the bad cell kept its value");
}

// ---------------------------------------------------------------------------------------------
// rows and columns the user moved, hid or added

#[test]
fn reordered_columns_import_by_header() {
    let reverse = |s: &str, g: &mut Grid| {
        if [WALLS, OPENINGS, ROOMS, LAYERS, LEVELS, TYPES].contains(&s) {
            for r in g.iter_mut() {
                r.reverse();
            }
        }
    };
    // no edit: nothing changes
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "rev");
    let dst = tmp("rev_a");
    rewrite(&src, &dst, &[], reverse);
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(),
        "reversed columns, no edit: {:?} {:?}",
        rep.changes,
        rep.rejected
    );
    assert_eq!(norm(dump(&m)), norm(before.clone()));
    // one edit: exactly that cell
    let dst = tmp("rev_b");
    rewrite(&src, &dst, &[], |s, g| {
        reverse(s, g);
        if s == WALLS {
            let r = row_of(g, 2);
            set(g, r, "height [m]", data(V::N(3.5)));
        }
    });
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.applied, "reversed columns with one edit: {:?}", rep.rejected);
    let d = diff(&before, &dump(&m));
    assert!(
        d.iter()
            .any(|x| x.table == "wall" && x.rowid == 2 && x.cols == ["height"]),
        "wall 2 height did not change:\n{}",
        show(&d)
    );
    assert!(
        d.iter()
            .all(|x| (x.table == "wall" && x.rowid == 2) || WALL_DERIVED.contains(&x.table.as_str())),
        "something else changed:\n{}",
        show(&d)
    );
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()), "one undo did not restore the model");
}

#[test]
fn a_hidden_sheet_is_read_like_a_visible_one() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "hid");
    let dst = tmp("hid");
    rewrite(&src, &dst, &[WALLS], |s, g| {
        if s == WALLS {
            let r = row_of(g, 2);
            set(g, r, "height [m]", data(V::N(3.5)));
        }
    });
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(
        rep.applied && rep.changes.len() == 1,
        "an edit on a hidden Walls sheet: changes {:?} rejected {:?}",
        rep.changes,
        rep.rejected
    );
    let d = diff(&before, &dump(&m));
    assert!(
        d.iter()
            .any(|x| x.table == "wall" && x.rowid == 2 && x.cols == ["height"]),
        "the hidden sheet's edit did not reach wall 2:\n{}",
        show(&d)
    );
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()));
}

#[test]
fn a_hidden_sheet_without_edits_changes_nothing() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "hid0");
    let dst = tmp("hid0");
    rewrite(&src, &dst, &[ROOMS, OPENINGS], |_, _| {});
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.applied && rep.changes.is_empty(), "{:?}", rep.changes);
    assert_eq!(norm(dump(&m)), norm(before.clone()));
}

#[test]
fn an_extra_user_column_is_offered_as_a_parameter_never_applied_silently() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "extra");
    let dst = tmp("extra");
    rewrite(&src, &dst, &[], |s, g| {
        if s == WALLS {
            add_col(g, "Note", &["a", "b", "c", "d"]);
        }
    });
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert!(rep.changes.is_empty(), "the extra column became a change: {:?}", rep.changes);
    assert_eq!(rep.proposals.len(), 1, "{:?}", rep.proposals);
    assert_eq!(rep.proposals[0].header, "Note");
    assert!(!rep.proposals[0].accept, "an unknown column was accepted by default");
    assert_eq!(norm(dump(&m)), norm(before.clone()), "an unaccepted column changed the model");

    // accepted: the parameter and its values arrive in one step, one undo removes them
    let op0 = ops(&m);
    let o = ImportOptions {
        new_params: true,
        ..apply()
    };
    let rep = import_xlsx_with(&mut m, &dst, o).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert_eq!(ops(&m), op0 + 1, "the new parameter must be one operation");
    assert_ne!(dump(&m), before, "the accepted column left no trace");
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()), "one undo did not remove the parameter");
}

#[test]
fn a_reexport_keeps_the_user_column_beside_unchanged_data() {
    let m = model();
    let src = export(&m, "keep");
    let dst = tmp("keep");
    rewrite(&src, &dst, &[], |s, g| {
        if s == WALLS {
            add_col(g, "Note", &["a", "b", "c", "d"]);
        }
    });
    export_xlsx(&m, &dst).unwrap();
    let mut wb: Xlsx<_> = open_workbook(&dst).unwrap();
    let g: Grid = wb
        .worksheet_range(WALLS)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect();
    let heads: Vec<String> = g[0].iter().map(|d| d.to_string()).collect();
    assert!(
        heads.iter().any(|h| h == "Note"),
        "re-export dropped the user column; headers: {heads:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// deleted rows

#[test]
fn a_removed_row_deletes_only_when_asked_and_one_undo_restores() {
    let drop_wall4 = |s: &str, g: &mut Grid| {
        if s == WALLS {
            let r = row_of(g, 4);
            g.remove(r);
        }
    };
    let mut m = model();
    let before = dump(&m);
    let op0 = ops(&m);
    let src = export(&m, "del");
    let dst = tmp("del");
    rewrite(&src, &dst, &[], drop_wall4);

    // default: the model keeps the wall
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty(),
        "a removed row must not delete by default: {:?}",
        rep.changes
    );
    assert_eq!(norm(dump(&m)), norm(before.clone()));

    // asked: the wall goes, in one step
    let o = ImportOptions {
        delete_missing: true,
        ..apply()
    };
    let pre = import_xlsx_with(&mut m, &dst, ImportOptions { apply: false, ..o }).unwrap();
    assert_eq!(pre.counts().deleted, 1, "preview: {:?}", pre.changes);
    assert_eq!(norm(dump(&m)), norm(before.clone()), "the preview deleted a wall");
    let rep = import_xlsx_with(&mut m, &dst, o).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert_eq!(count(&m, "wall"), 3, "wall 4 should be gone");
    assert_eq!(count(&m, "wall WHERE id=4"), 0);
    assert_eq!(ops(&m), op0 + 1);
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()), "one undo did not bring the wall back");
}

#[test]
fn the_delete_column_deletes_the_row_and_one_undo_restores() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "delflag");
    let dst = tmp("delflag");
    rewrite(&src, &dst, &[], |s, g| {
        if s == WALLS {
            let r = row_of(g, 4);
            set(g, r, "delete", data(V::S("delete")));
        }
    });
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert_eq!(count(&m, "wall WHERE id=4"), 0, "the flagged wall is still there");
    assert_eq!(count(&m, "wall"), 3, "more than the flagged wall was deleted");
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()));
}

#[test]
fn deleting_a_wall_with_a_door_takes_the_door_and_undo_brings_both_back() {
    let mut m = model();
    let before = dump(&m);
    let src = export(&m, "deldoor");
    let dst = tmp("deldoor");
    rewrite(&src, &dst, &[], |s, g| {
        if s == WALLS {
            let r = row_of(g, 1);
            set(g, r, "delete", data(V::S("delete")));
        }
    });
    let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
    assert!(rep.applied, "{:?}", rep.rejected);
    assert_eq!(count(&m, "wall WHERE id=1"), 0);
    assert_eq!(
        count(&m, "opening WHERE wall=1"),
        0,
        "the door outlived its wall"
    );
    m.undo().unwrap();
    assert_eq!(norm(dump(&m)), norm(before.clone()), "one undo did not restore the wall and its door");
}

#[test]
fn xlsx_nan_cell_is_refused_with_address() {
    for (label, v) in [
        ("nan-num", V::N(f64::NAN)),
        ("inf-num", V::N(f64::INFINITY)),
        ("nan-text", V::S("NaN")),
        ("inf-text", V::S("inf")),
    ] {
        let mut m = model();
        let before = dump(&m);
        let src = export(&m, label);
        let dst = tmp(label);
        rewrite(&src, &dst, &[], |s, g| {
            if s == WALLS {
                set(g, 1, "height [m]", data(v));
            }
        });
        let rep = import_xlsx_with(&mut m, &dst, apply()).unwrap();
        // bad cells skip alone (tests_sync): the cell is left out, it never reaches the model
        assert!(
            !rep.changes.iter().any(|c| c.sheet == WALLS && c.column.contains("height")),
            "{label}: a non-finite cell was applied"
        );
        let hit = rep
            .rejected
            .iter()
            .find(|r| r.sheet == WALLS && r.column.contains("height"))
            .unwrap_or_else(|| panic!("{label}: no rejection on the cell: {:?}", rep.rejected));
        assert!(hit.row > 0 && hit.col > 0, "{label}: no cell named: {hit:?}");
        let a = hit.address();
        assert!(a.contains(&hit.row.to_string()), "{label}: address {a}");
        assert_eq!(dump(&m), before, "{label}: the model changed");
    }
}
