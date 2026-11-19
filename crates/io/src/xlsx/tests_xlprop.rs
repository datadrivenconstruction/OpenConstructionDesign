//! Property tests of the XLSX round trip on random models: levels, wall types and layers, walls,
//! doors and windows, parameters of every type (a unit label too), Cyrillic and long names, empty
//! values. The exported book gets random cell edits (change, clear, junk text, a number in the
//! other locale) and shape edits (deleted row, copied or renamed column, sheet order, an extra
//! sheet); then preview and import run.
//!
//! Invariants: an untouched book is a no-op; every listed change is one edited cell; preview and
//! apply list the same cells; a refused cell never changes the model; nothing unlisted changes;
//! import then export is a fixed point; no input panics. A failure prints the seed
//! (`XLPROP_SEED=<n>` reruns it) and the shrunk case; paste that case into `fixed_cases`.
//!
//! Open decisions kept out of the inputs:
//! - "3,5" text in a dot-mark book: no other-locale number goes into an en book.
//! - empty catalog cells: family type sheets are never generated or edited.
//! - skipping a bad cell versus stopping the book: no check depends on which rule holds.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, PVal, ParamSet, ParamSpec, ParamType, WallSpec};
use proptest::arbitrary::any;
use proptest::strategy::{Just, Strategy};
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestError, TestRunner};
use rust_xlsxwriter::Workbook;

use super::*;

type Grid = Vec<Vec<Data>>;
type Book = Vec<(String, Grid)>;
/// (sheet, row key, header of the plain export): one cell, wherever the edited book moved it.
type Cell = (String, String, String);

const SHEETS: [&str; 5] = [LEVELS, TYPES, LAYERS, WALLS, OPENINGS];
const CASES: u32 = 12;
const SEED: u64 = 0x5EED_0F_E5C1;

const ABC: &[char] = &[
    'a', 'b', 'k', 'x', 'Z', ' ', '-', '1', '7', '\u{0410}', '\u{0416}', '\u{0438}', '\u{044F}',
    '\u{0451}', '\u{0419}',
];
const JUNK: &[&str] = &[
    "abc",
    "\u{0416}\u{0416}\u{0416}",
    "12abc",
    "#REF!",
    "--",
    "?",
    "1..2",
    "x\u{a0}y",
];

#[derive(Clone, Debug)]
struct WallGen {
    level: usize,
    wtype: usize,
    len_dm: u32,
    height_cm: u32,
    offset0_mm: Option<u32>,
    /// Some(true) a door, Some(false) a window.
    opening: Option<bool>,
}

#[derive(Clone, Debug)]
struct ParamGen {
    name: String,
    ptype: usize,
    openings: bool,
    unit: usize,
    /// Value seeds by element index (cycled); None leaves the value empty.
    values: Vec<Option<u32>>,
}

#[derive(Clone, Debug)]
struct ModelGen {
    levels: Vec<(String, i32)>,
    types: Vec<(String, Vec<(String, u32)>)>,
    walls: Vec<WallGen>,
    params: Vec<ParamGen>,
}

#[derive(Clone, Debug)]
enum CellOp {
    Change(u32),
    Clear,
    Junk(usize),
    /// A number written in the other locale's mark; ru books only (see the module note).
    Other(u32),
}

#[derive(Clone, Debug)]
struct CellEdit {
    sheet: usize,
    row: usize,
    col: usize,
    op: CellOp,
}

#[derive(Clone, Debug)]
enum Shape {
    DelRow(usize, usize),
    DupCol(usize, usize),
    RenameCol(usize, usize),
    Reorder(u64),
    /// true: an edited copy of Walls under a look-alike name; false: an unrelated notes sheet.
    Extra(bool),
}

#[derive(Clone, Debug)]
struct Case {
    ru: bool,
    model: ModelGen,
    cells: Vec<CellEdit>,
    shapes: Vec<Shape>,
}

// --- strategies ---

fn name_st() -> impl Strategy<Value = String> {
    let ch = || proptest::sample::select(ABC);
    proptest::prop_oneof![
        3 => proptest::collection::vec(ch(), 1..12),
        1 => proptest::collection::vec(ch(), 80..200),
    ]
    .prop_map(|v| v.into_iter().collect::<String>())
}

fn wall_st() -> impl Strategy<Value = WallGen> {
    (
        0usize..8,
        0usize..8,
        40u32..80,
        250u32..450,
        proptest::option::of(0u32..300),
        proptest::option::of(any::<bool>()),
    )
        .prop_map(|(level, wtype, len_dm, height_cm, offset0_mm, opening)| WallGen {
            level,
            wtype,
            len_dm,
            height_cm,
            offset0_mm,
            opening,
        })
}

fn param_st() -> impl Strategy<Value = ParamGen> {
    (
        name_st(),
        0usize..7,
        any::<bool>(),
        0usize..3,
        proptest::collection::vec(proptest::option::of(any::<u32>()), 1..6),
    )
        .prop_map(|(name, ptype, openings, unit, values)| ParamGen {
            name,
            ptype,
            openings,
            unit,
            values,
        })
}

fn model_st() -> impl Strategy<Value = ModelGen> {
    let layer = (name_st(), 50u32..400);
    (
        proptest::collection::vec((name_st(), -20i32..60), 1..4),
        proptest::collection::vec((name_st(), proptest::collection::vec(layer, 1..4)), 1..4),
        proptest::collection::vec(wall_st(), 1..6),
        proptest::collection::vec(param_st(), 0..5),
    )
        .prop_map(|(levels, types, walls, params)| ModelGen {
            levels,
            types,
            walls,
            params,
        })
}

fn op_st() -> impl Strategy<Value = CellOp> {
    proptest::prop_oneof![
        4 => any::<u32>().prop_map(CellOp::Change),
        1 => Just(CellOp::Clear),
        1 => (0usize..JUNK.len()).prop_map(CellOp::Junk),
        1 => any::<u32>().prop_map(CellOp::Other),
    ]
}

fn cell_st() -> impl Strategy<Value = CellEdit> {
    (0usize..SHEETS.len(), any::<usize>(), any::<usize>(), op_st())
        .prop_map(|(sheet, row, col, op)| CellEdit { sheet, row, col, op })
}

fn shape_st() -> impl Strategy<Value = Shape> {
    let at = || (0usize..SHEETS.len(), any::<usize>());
    proptest::prop_oneof![
        at().prop_map(|(s, r)| Shape::DelRow(s, r)),
        at().prop_map(|(s, c)| Shape::DupCol(s, c)),
        at().prop_map(|(s, c)| Shape::RenameCol(s, c)),
        any::<u64>().prop_map(Shape::Reorder),
        any::<bool>().prop_map(Shape::Extra),
    ]
}

fn case_st() -> impl Strategy<Value = Case> {
    (
        any::<bool>(),
        model_st(),
        proptest::collection::vec(cell_st(), 0..8),
        proptest::collection::vec(shape_st(), 0..3),
    )
        .prop_map(|(ru, model, cells, shapes)| Case { ru, model, cells, shapes })
}

// --- model ---

fn xorshift(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

/// Text from a seed: Cyrillic and Latin, every seventh one 150 characters long.
fn text_of(seed: u32) -> String {
    let mut x = u64::from(seed) | 1;
    let n = if seed % 7 == 0 { 150 } else { 3 + (seed % 17) as usize };
    let s: String = (0..n)
        .map(|_| ABC[(xorshift(&mut x) % ABC.len() as u64) as usize])
        .collect();
    let t = s.trim();
    if t.is_empty() {
        format!("t{seed}")
    } else {
        t.to_string()
    }
}

fn label(n: &str, i: usize) -> String {
    format!("{}{i}", n.trim())
}

fn ptype_of(p: &ParamGen) -> ParamType {
    ParamType::ALL[p.ptype % ParamType::ALL.len()]
}

fn pval(t: ParamType, v: u32) -> PVal {
    match t {
        ParamType::Text => PVal::Text(text_of(v)),
        ParamType::Int => PVal::Int(i64::from(v % 1000)),
        ParamType::Number => PVal::Num(f64::from(v % 10_000) / 10.0),
        ParamType::Length => PVal::Num(f64::from(v % 5000) / 1000.0),
        ParamType::Area | ParamType::Volume => PVal::Num(f64::from(v % 500) / 10.0),
        ParamType::YesNo => PVal::Bool(v % 2 == 0),
    }
}

fn param_name(k: usize, p: &ParamGen) -> String {
    let base: String = p.name.trim().chars().take(40).collect();
    format!("P{k}_{}", base.trim())
}

fn build(g: &ModelGen) -> Result<Model, String> {
    let e = |what: &str, err: ddc_model::Error| format!("fixture {what}: {err}");
    let mut m = Model::open(":memory:").map_err(|x| e("open", x))?;
    let mut lv = vec![];
    for (i, (n, d)) in g.levels.iter().enumerate() {
        let z = i as f64 * 3.5 + f64::from(*d) / 100.0;
        let id = m
            .add_level(&label(n, i), z)
            .or_else(|_| m.add_level(&format!("L{i}"), z))
            .map_err(|x| e("level", x))?;
        lv.push(id);
    }
    let mut ty = vec![];
    for (i, (n, layers)) in g.types.iter().enumerate() {
        let specs: Vec<LayerSpec> = layers
            .iter()
            .enumerate()
            .map(|(j, (mat, t))| LayerSpec {
                material: label(mat, j),
                thickness: f64::from(*t) / 1000.0,
            })
            .collect();
        let id = m
            .add_wall_type(&label(n, i), &specs)
            .or_else(|_| m.add_wall_type(&format!("T{i}"), &specs))
            .map_err(|x| e("wall type", x))?;
        ty.push(id);
    }
    let (mut walls, mut ops) = (vec![], vec![]);
    for (i, w) in g.walls.iter().enumerate() {
        let x0 = i as f64 * 12.0;
        let spec = WallSpec {
            a: (x0, 0.0),
            b: (x0 + f64::from(w.len_dm) / 10.0, 0.0),
            level: lv[w.level % lv.len()],
            base_offset: 0.0,
            height: f64::from(w.height_cm) / 100.0,
            wall_type: ty[w.wtype % ty.len()],
            offset0: w.offset0_mm.map(|x| f64::from(x) / 1000.0),
        };
        let id = m.add_wall(&spec).map_err(|x| e("wall", x))?;
        walls.push(id);
        if let Some(door) = w.opening {
            let o = OpeningSpec {
                along: 1.0,
                width: 0.9,
                sill: if door { 0.0 } else { 0.9 },
                height: if door { 2.1 } else { 1.2 },
                kind: if door { "door" } else { "window" }.into(),
            };
            ops.push(m.add_opening(id, &o).map_err(|x| e("opening", x))?);
        }
    }
    for (k, p) in g.params.iter().enumerate() {
        let ptype = ptype_of(p);
        let unit = match ptype {
            ParamType::Number => ["", "pcs", "\u{0440}\u{0443}\u{0431}"][p.unit % 3],
            _ => "",
        };
        let cats: Vec<String> = if p.openings {
            vec!["door".into(), "window".into()]
        } else {
            vec!["wall".into()]
        };
        let spec = ParamSpec {
            name: param_name(k, p),
            ptype,
            per_type: false,
            unit: unit.into(),
            group: String::new(),
            guid: None,
            categories: cats,
        };
        let param = m.add_param(&spec).map_err(|x| e("parameter", x))?;
        let (kind, ids) = if p.openings { ("opening", &ops) } else { ("wall", &walls) };
        let sets: Vec<ParamSet> = ids
            .iter()
            .enumerate()
            .filter_map(|(n, id)| {
                let v = p.values[n % p.values.len()]?;
                Some(ParamSet {
                    param,
                    kind: kind.into(),
                    id: *id,
                    value: Some(pval(ptype, v)),
                })
            })
            .collect();
        m.set_param_values(&sets).map_err(|x| e("parameter value", x))?;
    }
    Ok(m)
}

// --- books ---

static N: AtomicUsize = AtomicUsize::new(0);

/// Temporary books of one case, removed when the case ends.
#[derive(Default)]
struct Files(RefCell<Vec<PathBuf>>);

impl Files {
    fn path(&self, tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join("ddc_xlsx_tests");
        let _ = std::fs::create_dir_all(&d);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = d.join(format!("{}_xlprop_{n}_{tag}.xlsx", std::process::id()));
        self.0.borrow_mut().push(p.clone());
        p
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        for p in self.0.borrow().iter() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn captions() -> HashMap<String, String> {
    [("height", "Vysota"), ("level", "Uroven"), ("type", "Tip"), ("thickness", "Tolshchina")]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into()
}

/// A ru book records its comma mark on Info, as the app's export does.
fn export(m: &Model, from: &Path, to: &Path, ru: bool) -> Result<(), String> {
    if ru {
        let st = Style::professional(captions()).with_number_format(true, "ru");
        export_xlsx_styled(m, from, to, Units::Metric, &[], &st)
    } else {
        export_xlsx(m, to)
    }
}

fn read_book(path: &Path) -> Result<Book, String> {
    let mut wb: Xlsx<_> = open_workbook(path).map_err(|e| format!("open {path:?}: {e}"))?;
    let mut out: Book = vec![];
    for n in wb.sheet_names() {
        let r = wb.worksheet_range(&n).map_err(|e| format!("sheet {n}: {e}"))?;
        out.push((n, r.rows().map(|r| r.to_vec()).collect()));
    }
    Ok(out)
}

fn grid<'a>(b: &'a Book, name: &str) -> Option<&'a Grid> {
    b.iter().find(|(n, _)| n == name).map(|(_, g)| g)
}

fn head_of(g: &Grid) -> Vec<String> {
    g.first().map_or(vec![], |r| r.iter().map(|d| d.to_string()).collect())
}

fn num_of(d: &Data) -> Option<f64> {
    match d {
        Data::Float(f) => Some(*f),
        Data::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn blank(d: &Data) -> bool {
    matches!(d, Data::Empty) || matches!(d, Data::String(s) if s.trim().is_empty())
}

fn same(a: &Data, b: &Data) -> bool {
    if blank(a) || blank(b) {
        return blank(a) && blank(b);
    }
    match (num_of(a), num_of(b)) {
        (Some(x), Some(y)) => (x - y).abs() <= 1e-9,
        (None, None) => a.to_string().trim() == b.to_string().trim(),
        _ => false,
    }
}

fn norm(d: &Data) -> String {
    if blank(d) {
        return String::new();
    }
    match num_of(d) {
        Some(x) => format!("{x:.6}"),
        None => d.to_string().trim().to_string(),
    }
}

/// Columns the model state is read from: not read-only, not the row key, not the delete flag.
fn compared(h: &str) -> bool {
    !h.contains("(read-only)") && !matches!(h, "id" | "type_id" | "pos" | "delete")
}

fn key_of(head: &[String], row: &[Data]) -> String {
    let at = |h: &str| {
        let j = head.iter().position(|x| x == h)?;
        row.get(j).and_then(num_of)
    };
    match (at("id"), at("type_id"), at("pos")) {
        (Some(id), _, _) => format!("{id}"),
        (None, Some(t), Some(p)) => format!("{t}/{p}"),
        _ => "?".into(),
    }
}

/// Name -> `#id` of a sheet's rows, so a renamed level or type does not count as a wall edit.
fn names(book: &Book, plain: &Book, sh: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let (Some(g), Some(p)) = (grid(book, sh), grid(plain, sh)) else {
        return out;
    };
    let head = head_of(p);
    let (Some(i), Some(n)) = (
        head.iter().position(|h| h == "id"),
        head.iter().position(|h| h == "name"),
    ) else {
        return out;
    };
    for r in g.iter().skip(1) {
        if let (Some(id), Some(name)) = (r.get(i).and_then(num_of), r.get(n)) {
            out.insert(norm(name), format!("#{id}"));
        }
    }
    out
}

/// What the book says about the model, one entry per compared cell.
fn state(book: &Book, plain: &Book) -> Result<BTreeMap<Cell, String>, String> {
    let (lv, ty) = (names(book, plain, LEVELS), names(book, plain, TYPES));
    let mut out = BTreeMap::new();
    for sh in SHEETS {
        let g = grid(book, sh).ok_or(format!("sheet {sh} missing from an export"))?;
        let head = head_of(grid(plain, sh).ok_or(format!("plain export has no {sh}"))?);
        for r in g.iter().skip(1) {
            let k = key_of(&head, r);
            for (j, h) in head.iter().enumerate().filter(|(_, h)| compared(h)) {
                let mut v = norm(r.get(j).unwrap_or(&Data::Empty));
                let refs = match (sh == WALLS, h.as_str()) {
                    (true, "level") => Some(&lv),
                    (true, "type") => Some(&ty),
                    _ => None,
                };
                if let Some(id) = refs.and_then(|m| m.get(&v)) {
                    v = id.clone();
                }
                out.insert((sh.to_string(), k.clone(), h.clone()), v);
            }
        }
    }
    Ok(out)
}

fn diff(a: &BTreeMap<Cell, String>, b: &BTreeMap<Cell, String>) -> BTreeSet<Cell> {
    let keys: BTreeSet<&Cell> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

// --- edits ---

/// One sheet of the edited book and where its rows and columns came from.
struct Out {
    name: String,
    grid: Grid,
    /// The exported sheet it is; None for an added sheet.
    src: Option<&'static str>,
    rows: Vec<usize>,
    cols: Vec<usize>,
}

fn outs_of(book: &Book) -> Vec<Out> {
    book.iter()
        .map(|(name, g)| {
            let w = g.iter().map(|r| r.len()).max().unwrap_or(0);
            Out {
                name: name.clone(),
                grid: g.clone(),
                src: SHEETS.iter().find(|s| **s == name.as_str()).copied(),
                rows: (0..g.len()).collect(),
                cols: (0..w).collect(),
            }
        })
        .collect()
}

fn write_book(sheets: &[Out], path: &Path) -> Result<(), String> {
    let mut wb = Workbook::new();
    for s in sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&s.name).map_err(|e| e.to_string())?;
        for (i, row) in s.grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(t) => ws.write_string(i, j, t).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    Data::DateTime(t) => ws
                        .write_number_with_format(
                            i,
                            j,
                            t.as_f64(),
                            &rust_xlsxwriter::Format::new().set_num_format("yyyy-mm-dd"),
                        )
                        .map(|_| ()),
                    _ => Ok(()),
                }
                .map_err(|e| e.to_string())?;
            }
        }
    }
    wb.save(path).map_err(|e| e.to_string())
}

enum Class {
    Len,
    Num,
    Int,
    Text,
    Flag,
    Pick(Vec<String>),
    Ids(Vec<f64>),
}

fn column_names(book: &Book, plain: &Book, sh: &str) -> Vec<String> {
    let mut v: Vec<String> = names(book, plain, sh).into_keys().collect();
    v.sort();
    v
}

fn class(c: &Case, sh: &str, h: &str, book: &Book, plain: &Book) -> Class {
    let param = c
        .model
        .params
        .iter()
        .enumerate()
        .find(|(k, _)| h.starts_with(&format!("P{k}_")));
    if let Some((_, p)) = param {
        return match ptype_of(p) {
            ParamType::Text => Class::Text,
            ParamType::Int => Class::Int,
            ParamType::YesNo => Class::Flag,
            ParamType::Length => Class::Len,
            ParamType::Number | ParamType::Area | ParamType::Volume => Class::Num,
        };
    }
    fn pick(v: &[&str]) -> Class {
        Class::Pick(v.iter().map(|s| s.to_string()).collect())
    }
    if sh == WALLS && h == "level" {
        Class::Pick(column_names(book, plain, LEVELS))
    } else if sh == WALLS && h == "type" {
        Class::Pick(column_names(book, plain, TYPES))
    } else if sh == OPENINGS && h == "wall_id" {
        let ids = grid(book, WALLS).map_or(vec![], |g| {
            g.iter().skip(1).filter_map(|r| r.first().and_then(num_of)).collect()
        });
        Class::Ids(ids)
    } else if sh == OPENINGS && h == "kind" {
        pick(&["door", "window"])
    } else if sh == OPENINGS && h.starts_with("swing") {
        pick(&["left", "right"])
    } else if sh == OPENINGS && h.starts_with("hinge") {
        pick(&["start", "end"])
    } else if h.contains("[m]") {
        Class::Len
    } else if h.starts_with("bulge") {
        Class::Num
    } else {
        Class::Text
    }
}

fn dec(x: f64, ru: bool) -> String {
    let s = format!("{x}");
    if ru {
        s.replace('.', ",")
    } else {
        s
    }
}

fn new_value(c: &Case, cls: &Class, old: &Data, op: &CellOp) -> Option<Data> {
    let old_n = num_of(old).unwrap_or(0.0);
    match op {
        CellOp::Clear => Some(Data::Empty),
        CellOp::Junk(k) => Some(Data::String(JUNK[k % JUNK.len()].into())),
        // open decision: "3,5" in a dot-mark book is not generated
        CellOp::Other(_) if !c.ru => None,
        CellOp::Other(v) => match cls {
            Class::Len | Class::Num => {
                Some(Data::String(format!("{:.2}", old_n + 0.25 + f64::from(v % 40) / 10.0)))
            }
            _ => None,
        },
        CellOp::Change(v) => match cls {
            Class::Len => {
                let mm = (old_n * 1000.0).round() as i64 + 1 + i64::from(v % 200);
                let x = mm as f64 / 1000.0;
                Some(match v % 3 {
                    0 => Data::Float(x),
                    1 => Data::String(dec(x, c.ru)),
                    _ => Data::String(format!("{mm} mm")),
                })
            }
            Class::Num => Some(Data::Float(old_n + f64::from(1 + v % 50) / 100.0)),
            Class::Int => Some(Data::Float(old_n.round() + f64::from(1 + v % 100))),
            Class::Text => Some(Data::String(text_of(*v))),
            Class::Flag => Some(Data::String(if v % 2 == 0 { "yes" } else { "no" }.into())),
            Class::Pick(list) if !list.is_empty() => {
                Some(Data::String(list[*v as usize % list.len()].clone()))
            }
            Class::Ids(list) if !list.is_empty() => {
                Some(Data::Float(list[*v as usize % list.len()]))
            }
            Class::Pick(_) | Class::Ids(_) => None,
        },
    }
}

type Planned = BTreeMap<(&'static str, usize, usize), Data>;

fn plan_cells(c: &Case, book: &Book, plain: &Book) -> Planned {
    let mut out = Planned::new();
    for e in &c.cells {
        let sh = SHEETS[e.sheet % SHEETS.len()];
        let (Some(g), Some(p)) = (grid(book, sh), grid(plain, sh)) else {
            continue;
        };
        let head = head_of(p);
        let cols: Vec<usize> = (0..head.len()).filter(|j| compared(&head[*j])).collect();
        if g.len() < 2 || cols.is_empty() {
            continue;
        }
        let i = 1 + e.row % (g.len() - 1);
        let j = cols[e.col % cols.len()];
        let old = g[i].get(j).cloned().unwrap_or(Data::Empty);
        let cls = class(c, sh, &head[j], book, plain);
        if let Some(d) = new_value(c, &cls, &old, &e.op) {
            if !same(&d, &old) {
                out.insert((sh, i, j), d);
            }
        }
    }
    out
}

fn shuffle(outs: &mut [Out], seed: u64) {
    let mut x = seed | 1;
    for i in (1..outs.len()).rev() {
        let j = (xorshift(&mut x) % (i as u64 + 1)) as usize;
        outs.swap(i, j);
    }
}

fn find_src(outs: &[Out], k: usize) -> Option<usize> {
    outs.iter().position(|o| o.src == Some(SHEETS[k % SHEETS.len()]))
}

fn apply_shapes(outs: &mut Vec<Out>, shapes: &[Shape]) {
    for s in shapes {
        match *s {
            Shape::DelRow(k, r) => {
                let Some(i) = find_src(outs, k) else { continue };
                let o = &mut outs[i];
                if o.grid.len() >= 2 {
                    let i = 1 + r % (o.grid.len() - 1);
                    o.grid.remove(i);
                    o.rows.remove(i);
                }
            }
            Shape::DupCol(k, c) => {
                let Some(i) = find_src(outs, k) else { continue };
                let o = &mut outs[i];
                let w = o.cols.len();
                if w == 0 {
                    continue;
                }
                let j = c % w;
                for row in o.grid.iter_mut() {
                    row.resize(w, Data::Empty);
                    let d = row[j].clone();
                    row.push(d);
                }
                let src = o.cols[j];
                o.cols.push(src);
            }
            Shape::RenameCol(k, c) => {
                let Some(i) = find_src(outs, k) else { continue };
                let o = &mut outs[i];
                let w = o.cols.len();
                if let (true, Some(head)) = (w > 0, o.grid.first_mut()) {
                    let j = c % w;
                    head.resize(w.max(head.len()), Data::Empty);
                    head[j] = Data::String(format!("{} \u{0425}", head[j]));
                }
            }
            Shape::Reorder(seed) => shuffle(outs, seed),
            Shape::Extra(copy) => {
                let (name, g) = if copy {
                    let Some(w) = outs.iter().find(|o| o.src == Some(WALLS)) else { continue };
                    let mut g = w.grid.clone();
                    for row in g.iter_mut().skip(1) {
                        for d in row.iter_mut() {
                            if let Data::Float(x) = d {
                                *x += 1.0;
                            }
                        }
                    }
                    (format!("{WALLS} (2)"), g)
                } else {
                    let note = "\u{0417}\u{0430}\u{043C}\u{0435}\u{0442}\u{043A}\u{0438}";
                    let g = vec![
                        vec![Data::String("note".into())],
                        vec![Data::String(text_of(91))],
                    ];
                    (note.to_string(), g)
                };
                if outs.iter().any(|o| o.name == name) {
                    continue;
                }
                outs.push(Out { name, grid: g, src: None, rows: vec![], cols: vec![] });
            }
        }
    }
}

/// A report cell (1-based) of the edited book -> the exported cell it stands for.
fn map_cell(
    outs: &[Out],
    book0: &Book,
    plain: &Book,
    sheet: &str,
    row: u32,
    col: u32,
) -> Option<Cell> {
    let o = outs.iter().find(|o| o.name == sheet)?;
    let src = o.src?;
    let r = (row as usize).checked_sub(1)?;
    let c = (col as usize).checked_sub(1)?;
    let (orow, ocol) = (*o.rows.get(r)?, *o.cols.get(c)?);
    if orow == 0 {
        return None;
    }
    let head = head_of(grid(plain, src)?);
    let key = key_of(&head, grid(book0, src)?.get(orow)?);
    Some((src.to_string(), key, head.get(ocol)?.clone()))
}

fn at(sheet: &str, row: u32, col: u32) -> String {
    format!("{sheet}!R{row}C{col}")
}

/// Changes of a report as exported cells: each one an edited cell, no cell twice.
fn change_cells(
    rep: &Report,
    outs: &[Out],
    book0: &Book,
    plain: &Book,
    edited: &BTreeSet<Cell>,
) -> Result<BTreeSet<Cell>, String> {
    let mut out = BTreeSet::new();
    for ch in &rep.changes {
        let w = format!("{} '{}' -> '{}'", at(&ch.sheet, ch.row, ch.col), ch.old, ch.new);
        if ch.row == 0 || ch.col == 0 {
            return Err(format!("whole-row change without a row edit: {w}"));
        }
        let cell = map_cell(outs, book0, plain, &ch.sheet, ch.row, ch.col)
            .ok_or(format!("change outside the exported sheets: {w}"))?;
        if !edited.contains(&cell) {
            return Err(format!("change of a cell nobody edited: {w} = {cell:?}"));
        }
        if !out.insert(cell.clone()) {
            return Err(format!("two changes for one edited cell: {w} = {cell:?}"));
        }
    }
    Ok(out)
}

fn quiet(rep: &Report) -> Result<(), String> {
    if let Some(c) = rep.changes.first() {
        return Err(format!("change {} '{}' -> '{}'", at(&c.sheet, c.row, c.col), c.old, c.new));
    }
    if let Some(x) = rep.rejected.first() {
        return Err(format!("refusal {}: {}", at(&x.sheet, x.row, x.col), x.reason));
    }
    if let Some(x) = rep.ignored.first() {
        return Err(format!("ignored {}: {}", at(&x.sheet, x.row, x.col), x.reason));
    }
    if let Some(x) = rep.conflicts.first() {
        return Err(format!("conflict {}", at(&x.sheet, x.row, x.col)));
    }
    Ok(())
}

fn preview() -> ImportOptions {
    ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    }
}

fn op_count(m: &Model) -> Result<i64, String> {
    m.sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

fn same_sheets(a: &Book, b: &Book) -> Result<(), String> {
    for sh in SHEETS {
        let (Some(ga), Some(gb)) = (grid(a, sh), grid(b, sh)) else {
            return Err(format!("sheet {sh} missing from a re-export"));
        };
        if ga.len() != gb.len() {
            return Err(format!("{sh}: {} rows, then {}", ga.len(), gb.len()));
        }
        for (i, (ra, rb)) in ga.iter().zip(gb).enumerate() {
            for j in 0..ra.len().max(rb.len()) {
                let (x, y) = (ra.get(j).unwrap_or(&Data::Empty), rb.get(j).unwrap_or(&Data::Empty));
                if !same(x, y) {
                    let w = at(sh, i as u32 + 1, j as u32 + 1);
                    return Err(format!("re-export not stable at {w}: '{x}' then '{y}'"));
                }
            }
        }
    }
    Ok(())
}

fn check(c: &Case) -> Result<(), String> {
    let files = Files::default();
    let mut m = build(&c.model)?;
    let p_plain = files.path("plain");
    export_xlsx(&m, &p_plain)?;
    let plain = read_book(&p_plain)?;
    let p0 = files.path("book");
    export(&m, &p0, &p0, c.ru)?;
    let book0 = read_book(&p0)?;
    for sh in SHEETS {
        let n = |b: &Book| grid(b, sh).map(|g| head_of(g).len());
        if n(&book0) != n(&plain) {
            return Err(format!("{sh}: column count differs between the plain and the ru book"));
        }
    }

    // an untouched book, as exported and as rewritten by another tool, is a no-op
    let ops0 = op_count(&m)?;
    quiet(&import_xlsx_with(&mut m, &p0, preview())?).map_err(|e| format!("untouched: {e}"))?;
    let p1 = files.path("same");
    write_book(&outs_of(&book0), &p1)?;
    quiet(&import_xlsx_with(&mut m, &p1, preview())?).map_err(|e| format!("rewritten: {e}"))?;

    let planned = plan_cells(c, &book0, &plain);
    let mut outs = outs_of(&book0);
    let mut edited = BTreeSet::new();
    for ((sh, i, j), d) in &planned {
        let Some(o) = outs.iter_mut().find(|o| o.src == Some(*sh)) else { continue };
        let row = &mut o.grid[*i];
        if row.len() <= *j {
            row.resize(*j + 1, Data::Empty);
        }
        row[*j] = d.clone();
        let head = head_of(grid(&plain, sh).ok_or("plain sheet")?);
        let key = key_of(&head, &grid(&book0, sh).ok_or("book sheet")?[*i]);
        edited.insert((sh.to_string(), key, head[*j].clone()));
    }
    apply_shapes(&mut outs, &c.shapes);
    let q = files.path("edited");
    write_book(&outs, &q)?;

    let pre = import_xlsx_with(&mut m, &q, preview())?;
    if op_count(&m)? != ops0 {
        return Err("preview changed the model".into());
    }
    let listed = change_cells(&pre, &outs, &book0, &plain, &edited)
        .map_err(|e| format!("preview: {e}"))?;
    let rep = import_xlsx(&mut m, &q)?;
    let got = change_cells(&rep, &outs, &book0, &plain, &edited)
        .map_err(|e| format!("apply: {e}"))?;
    if got != listed {
        return Err(format!("preview lists {listed:?}, apply lists {got:?}"));
    }
    if !rep.applied && op_count(&m)? != ops0 {
        return Err("an import that was not applied changed the model".into());
    }

    let r1 = files.path("after");
    export(&m, &p0, &r1, c.ru)?;
    let book1 = read_book(&r1)?;
    let changed = diff(&state(&book0, &plain)?, &state(&book1, &plain)?);
    if !rep.applied && !changed.is_empty() {
        return Err(format!("not applied, yet the model changed: {changed:?}"));
    }
    if let Some(x) = changed.iter().find(|x| !got.contains(*x)) {
        return Err(format!("model changed where the report lists nothing: {x:?}"));
    }
    let refused = rep
        .rejected
        .iter()
        .map(|x| (&x.sheet, x.row, x.col, x.reason.as_str()))
        .chain(rep.ignored.iter().map(|x| (&x.sheet, x.row, x.col, x.reason.as_str())))
        .chain(rep.conflicts.iter().map(|x| (&x.sheet, x.row, x.col, "conflict")));
    for (sheet, row, col, why) in refused {
        let Some(cell) = map_cell(&outs, &book0, &plain, sheet, row, col) else { continue };
        if changed.contains(&cell) {
            return Err(format!("refused cell {} changed the model ({why})", at(sheet, row, col)));
        }
    }

    // import then export is a fixed point
    quiet(&import_xlsx_with(&mut m, &r1, preview())?).map_err(|e| format!("re-export: {e}"))?;
    let r2 = files.path("again");
    export(&m, &p0, &r2, c.ru)?;
    same_sheets(&book1, &read_book(&r2)?)
}

fn run_prop(cases: u32) {
    let seed = std::env::var("XLPROP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEED);
    let cfg = Config {
        cases,
        max_shrink_iters: 256,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(seed),
        ..Config::default()
    };
    let mut runner = TestRunner::new(cfg);
    match runner.run(&case_st(), |c| check(&c).map_err(|e| TestCaseError::fail(e))) {
        Ok(()) => {}
        Err(TestError::Fail(why, c)) => {
            panic!("xlprop seed={seed} (rerun: XLPROP_SEED={seed}): {why}\nshrunk case: {c:#?}")
        }
        Err(e) => panic!("xlprop seed={seed}: {e}"),
    }
}

#[test]
fn xlprop_round_trip_invariants() {
    run_prop(CASES);
}

#[test]
#[ignore = "long: 300 cases"]
fn xlprop_round_trip_invariants_long() {
    run_prop(300);
}

// --- plain regressions: shrunk counterexamples go here as fixed cases ---

fn wall(level: usize, wtype: usize, opening: Option<bool>) -> WallGen {
    WallGen {
        level,
        wtype,
        len_dm: 50,
        height_cm: 300,
        offset0_mm: (level % 2 == 0).then_some(100),
        opening,
    }
}

fn fixed_model() -> ModelGen {
    let cyr = "\u{0421}\u{0442}\u{0435}\u{043D}\u{0430}".to_string();
    ModelGen {
        levels: vec![(cyr.clone(), 0), ("x".repeat(150), 10)],
        types: vec![
            (cyr.clone(), vec![("brick".into(), 200), (cyr.clone(), 100)]),
            ("int".into(), vec![("gypsum".into(), 100)]),
        ],
        walls: vec![wall(0, 0, Some(true)), wall(1, 1, Some(false)), wall(0, 1, None)],
        params: (0..7)
            .map(|t| ParamGen {
                name: if t % 2 == 0 { cyr.clone() } else { "k".repeat(60) },
                ptype: t,
                openings: t == 3,
                unit: 2,
                values: vec![Some(t as u32 * 7 + 3), None],
            })
            .collect(),
    }
}

fn fixed_cases() -> Vec<Case> {
    let every_op = |sheet: usize| {
        let ops = [CellOp::Change(5), CellOp::Clear, CellOp::Junk(2), CellOp::Other(3)];
        ops.into_iter()
            .enumerate()
            .map(move |(k, op)| CellEdit { sheet, row: k, col: k * 3 + 1, op })
    };
    let cells: Vec<CellEdit> = (0..SHEETS.len()).flat_map(every_op).collect();
    let shapes = vec![
        Shape::DelRow(3, 1),
        Shape::DupCol(4, 2),
        Shape::RenameCol(0, 1),
        Shape::Reorder(7),
        Shape::Extra(true),
        Shape::Extra(false),
    ];
    let mut out = vec![];
    for ru in [false, true] {
        out.push(Case { ru, model: fixed_model(), cells: vec![], shapes: vec![] });
        out.push(Case { ru, model: fixed_model(), cells: cells.clone(), shapes: vec![] });
        out.push(Case { ru, model: fixed_model(), cells: cells.clone(), shapes: shapes.clone() });
    }
    out
}

#[test]
fn xlprop_fixed_cases() {
    for (i, c) in fixed_cases().iter().enumerate() {
        if let Err(e) = check(c) {
            panic!("fixed case {i} (ru={}): {e}", c.ru);
        }
    }
}
