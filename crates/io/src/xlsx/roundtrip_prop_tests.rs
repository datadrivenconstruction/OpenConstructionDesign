//! Seeded property test of the XLSX round trip: random edits of editable cells are listed
//! by the preview, applied to the model and survive a re-export; read-only edits are refused.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, WallSpec};

use super::tests::{edit, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;
type Book = Vec<(String, Grid)>;
type Cells = BTreeSet<(String, usize, usize)>;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        (self.next() >> 11) % n
    }
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

#[derive(Clone, Copy, PartialEq)]
enum K {
    /// Millimetre range.
    Num(i64, i64),
    /// Like `Num`, and a filled cell may be cleared.
    Clearable(i64, i64),
    Text,
}

struct T {
    sheet: &'static str,
    header: &'static str,
    table: &'static str,
    col: &'static str,
    /// (grid header, sql column) of the row key.
    keys: &'static [(&'static str, &'static str)],
    k: K,
}

const ID: &[(&str, &str)] = &[("id", "id")];
const LAYER_KEY: &[(&str, &str)] = &[("type_id", "wtype"), ("pos", "pos")];

const fn t(
    sheet: &'static str,
    header: &'static str,
    table: &'static str,
    col: &'static str,
    keys: &'static [(&'static str, &'static str)],
    k: K,
) -> T {
    T { sheet, header, table, col, keys, k }
}

const TARGETS: &[T] = &[
    t(LEVELS, "elevation [m]", "level", "elevation", ID, K::Num(-1000, 9000)),
    t(LAYERS, "thickness [m]", "wall_type_layer", "thickness", LAYER_KEY, K::Num(50, 400)),
    t(LAYERS, "material", "wall_type_layer", "material", LAYER_KEY, K::Text),
    t(WALLS, "base_offset [m]", "wall", "base_offset", ID, K::Num(-500, 500)),
    t(WALLS, "height [m]", "wall", "height", ID, K::Num(3000, 5000)),
    t(WALLS, "offset0 [m]", "wall", "offset0", ID, K::Clearable(50, 300)),
    t(OPENINGS, "along [m]", "opening", "along", ID, K::Num(500, 2000)),
    t(OPENINGS, "width [m]", "opening", "width", ID, K::Num(500, 1200)),
    t(OPENINGS, "sill [m]", "opening", "sill", ID, K::Num(0, 400)),
    t(OPENINGS, "height [m]", "opening", "height", ID, K::Num(1500, 2400)),
];

const RO_SHEET: &str = WALLS;
const RO_HEADER: &str = "length [m] (read-only)";
const COMPARED: [&str; 5] = [LEVELS, TYPES, LAYERS, WALLS, OPENINGS];

#[derive(Clone, Debug)]
enum Want {
    N(f64),
    T(String),
    Blank,
}

struct Plan {
    sheet: &'static str,
    i: usize,
    j: usize,
    data: Data,
    want: Want,
    /// Index into `TARGETS`; `None` for a read-only cell.
    target: Option<usize>,
}

fn fixture() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    m.add_level("L2", 3.2).unwrap();
    let layer = |n: &str, t: f64| LayerSpec {
        material: n.into(),
        thickness: t,
    };
    let ext = m
        .add_wall_type("ext", &[layer("brick", 0.2), layer("insulation", 0.1)])
        .unwrap();
    let int = m.add_wall_type("int", &[layer("gypsum", 0.1)]).unwrap();
    let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.), (0., 0.)];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[i + 1],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: if i % 2 == 0 { ext } else { int },
            offset0: (i % 2 == 0).then_some(0.1),
        };
        m.add_wall(&w).unwrap();
    }
    let door = OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    m.add_opening(1, &door).unwrap();
    let window = OpeningSpec {
        along: 1.5,
        width: 1.2,
        sill: 0.9,
        height: 1.2,
        kind: "window".into(),
    };
    m.add_opening(2, &window).unwrap();
    m
}

fn captions() -> HashMap<String, String> {
    [("height", "Vysota"), ("level", "Uroven"), ("type", "Tip"), ("thickness", "Tolshchina")]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into()
}

fn export_loc(m: &Model, from: &Path, to: &Path, ru: bool) {
    if ru {
        // a Russian book records its comma mark on Info, as the app's export does
        let st = Style::professional(captions()).with_number_format(true, "ru");
        export_xlsx_styled(m, from, to, Units::Metric, &[], &st).unwrap();
    } else {
        export_xlsx(m, to).unwrap();
    }
}

fn read_book(path: &Path) -> Book {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    wb.sheet_names()
        .into_iter()
        .map(|n| {
            let g: Grid = wb
                .worksheet_range(&n)
                .unwrap()
                .rows()
                .map(|r| r.to_vec())
                .collect();
            (n, g)
        })
        .collect()
}

fn sheet<'a>(b: &'a Book, name: &str) -> &'a Grid {
    &b.iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("sheet {name}"))
        .1
}

fn pos(plain: &Book, sh: &str, header: &str) -> usize {
    sheet(plain, sh)[0]
        .iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("column '{header}' on {sh}"))
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

fn cell_eq(a: &Data, b: &Data) -> bool {
    if blank(a) || blank(b) {
        return blank(a) && blank(b);
    }
    match (num_of(a), num_of(b)) {
        (Some(x), Some(y)) => (x - y).abs() <= 1e-9,
        (None, None) => a.to_string().trim() == b.to_string().trim(),
        _ => false,
    }
}

fn bad(ctx: &str, sh: &str, row: usize, col: usize, msg: &str) -> ! {
    panic!("{ctx} sheet={sh} row={row} col={col}: {msg}")
}

fn make_plans(rng: &mut Rng, ru: bool, book: &Book, plain: &Book) -> (Vec<Plan>, Vec<Plan>) {
    let dec = |x: f64| {
        let s = x.to_string();
        if ru {
            s.replace('.', ",")
        } else {
            s
        }
    };
    let mut plans = vec![];
    for (ti, t) in TARGETS.iter().enumerate() {
        let j = pos(plain, t.sheet, t.header);
        let g = sheet(book, t.sheet);
        for i in 1..g.len() {
            if !rng.chance(1, 3) {
                continue;
            }
            let old = g[i].get(j).cloned().unwrap_or(Data::Empty);
            let (data, want) = match t.k {
                K::Text => {
                    let mut s = format!("mat{}", rng.below(1000));
                    if old.to_string() == s {
                        s.push('x');
                    }
                    (Data::String(s.clone()), Want::T(s))
                }
                K::Clearable(..) if !blank(&old) && rng.chance(1, 3) => (Data::Empty, Want::Blank),
                K::Num(lo, hi) | K::Clearable(lo, hi) => {
                    let mut mm = lo + rng.below((hi - lo + 1) as u64) as i64;
                    if num_of(&old).is_some_and(|o| (o - mm as f64 / 1000.0).abs() < 1e-6) {
                        mm += 1;
                    }
                    let v = mm as f64 / 1000.0;
                    let data = match rng.below(4) {
                        0 => Data::Float(v),
                        1 => Data::String(dec(v)),
                        2 => Data::String(format!("{mm} mm")),
                        _ => Data::String(format!("{} m", dec(v))),
                    };
                    (data, Want::N(v))
                }
            };
            plans.push(Plan { sheet: t.sheet, i, j, data, want, target: Some(ti) });
        }
    }
    let mut ro = vec![];
    let j = pos(plain, RO_SHEET, RO_HEADER);
    let g = sheet(book, RO_SHEET);
    for i in 1..g.len() {
        if rng.chance(1, 2) {
            let v = num_of(&g[i][j]).unwrap_or(0.0) + 1.0 + rng.below(100) as f64 / 10.0;
            ro.push(Plan {
                sheet: RO_SHEET,
                i,
                j,
                data: Data::Float(v),
                want: Want::N(v),
                target: None,
            });
        }
    }
    (plans, ro)
}

fn cells(plans: &[Plan]) -> Cells {
    plans
        .iter()
        .map(|p| (p.sheet.to_string(), p.i + 1, p.j + 1))
        .collect()
}

fn same_cells(ctx: &str, what: &str, want: &Cells, got: &Cells, n_got: usize) {
    if let Some((s, r, c)) = want.difference(got).next() {
        bad(ctx, s, *r, *c, &format!("{what}: edited cell missing"));
    }
    if let Some((s, r, c)) = got.difference(want).next() {
        bad(ctx, s, *r, *c, &format!("{what}: cell listed but not edited"));
    }
    if n_got != want.len() {
        bad(ctx, "-", 0, 0, &format!("{what}: {n_got} entries for {} cells", want.len()));
    }
}

fn model_value(m: &Model, t: &T, g0: &Grid, plain: &Book, i: usize) -> Want {
    let w: Vec<String> = t
        .keys
        .iter()
        .map(|(h, c)| {
            let v = num_of(&g0[i][pos(plain, t.sheet, h)]).unwrap() as i64;
            format!("{c}={v}")
        })
        .collect();
    let sql = format!("SELECT {} FROM {} WHERE {}", t.col, t.table, w.join(" AND "));
    if t.k == K::Text {
        Want::T(m.sql().query_row(&sql, [], |r| r.get::<_, String>(0)).unwrap())
    } else {
        match m
            .sql()
            .query_row(&sql, [], |r| r.get::<_, Option<f64>>(0))
            .unwrap()
        {
            Some(x) => Want::N(x),
            None => Want::Blank,
        }
    }
}

fn op_count(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

fn preview() -> ImportOptions {
    ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    }
}

fn change_cells(rep: &Report) -> Cells {
    rep.changes
        .iter()
        .map(|c| (c.sheet.clone(), c.row as usize, c.col as usize))
        .collect()
}

fn run(seed: u64, ru: bool) {
    let loc = if ru { "ru" } else { "en" };
    let ctx = format!("seed={seed} loc={loc}");
    let mut rng = Rng::new(seed);
    let mut m = fixture();
    let name = format!("prop_{seed}_{loc}");
    let p = tmp(&format!("{name}_a"));
    let q = tmp(&format!("{name}_b"));
    let r = tmp(&format!("{name}_c"));
    let plain_p = tmp(&format!("{name}_plain"));
    export_xlsx(&m, &plain_p).unwrap();
    let plain = read_book(&plain_p);
    export_loc(&m, &p, &p, ru);
    let book = read_book(&p);
    for n in COMPARED {
        if sheet(&book, n)[0].len() != sheet(&plain, n)[0].len() {
            bad(&ctx, n, 1, 0, "column count differs between the plain and the localized book");
        }
    }

    // an unchanged book gives zero edits
    let ops = op_count(&m);
    let rep = import_xlsx_with(&mut m, &p, preview()).unwrap();
    if !(rep.changes.is_empty()
        && rep.rejected.is_empty()
        && rep.ignored.is_empty()
        && rep.conflicts.is_empty()
        && rep.edits.is_empty())
    {
        bad(&ctx, "-", 0, 0, &format!("unchanged book is not a no-op: {rep:?}"));
    }

    let (plans, ro) = make_plans(&mut rng, ru, &book, &plain);
    assert!(!plans.is_empty(), "{ctx}: no edits planned");
    edit(&p, &q, |sh, g: &mut Grid| {
        for pl in plans.iter().chain(&ro).filter(|pl| pl.sheet == sh) {
            g[pl.i][pl.j] = pl.data.clone();
        }
    });

    // preview: exactly the edited cells, nothing applied
    let rep = import_xlsx_with(&mut m, &q, preview()).unwrap();
    if let Some(x) = rep.rejected.first() {
        bad(&ctx, &x.sheet, x.row as usize, x.col as usize, &format!("rejected: {}", x.reason));
    }
    if let Some(x) = rep.conflicts.first() {
        bad(&ctx, &x.sheet, x.row as usize, x.col as usize, "unexpected conflict");
    }
    same_cells(&ctx, "preview", &cells(&plans), &change_cells(&rep), rep.changes.len());
    let refused: Cells = rep
        .ignored
        .iter()
        .map(|c| (c.sheet.clone(), c.row as usize, c.col as usize))
        .collect();
    same_cells(&ctx, "read-only refusals", &cells(&ro), &refused, rep.ignored.len());
    if let Some(x) = rep.ignored.iter().find(|x| !x.reason.contains("read-only")) {
        bad(&ctx, &x.sheet, x.row as usize, x.col as usize, &format!("reason: {}", x.reason));
    }
    assert_eq!(ops, op_count(&m), "{ctx}: preview changed the model");

    // apply: model equals the book
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{ctx}: {rep:?}");
    same_cells(&ctx, "apply", &cells(&plans), &change_cells(&rep), rep.changes.len());
    for pl in &plans {
        let tg = &TARGETS[pl.target.unwrap()];
        let have = model_value(&m, tg, sheet(&book, tg.sheet), &plain, pl.i);
        let ok = match (&have, &pl.want) {
            (Want::N(a), Want::N(b)) => (a - b).abs() <= 1e-9,
            (Want::T(a), Want::T(b)) => a == b,
            (Want::Blank, Want::Blank) => true,
            _ => false,
        };
        if !ok {
            bad(&ctx, pl.sheet, pl.i + 1, pl.j + 1, &format!("model {have:?}, book {:?}", pl.want));
        }
    }

    // re-export equals the edited book (read-only cells keep their computed value)
    export_loc(&m, &p, &r, ru);
    let again = read_book(&r);
    for n in COMPARED {
        let (g0, ga) = (sheet(&book, n), sheet(&again, n));
        if g0.len() != ga.len() {
            bad(&ctx, n, 0, 0, &format!("row count {} -> {}", g0.len(), ga.len()));
        }
        for (j, h) in sheet(&plain, n)[0].iter().enumerate() {
            let h = h.to_string();
            if h.contains("(read-only)") && !(n == RO_SHEET && h == RO_HEADER) {
                continue;
            }
            for i in 1..g0.len() {
                let over = plans.iter().find(|pl| pl.sheet == n && pl.i == i && pl.j == j);
                let want = match over.map(|pl| &pl.want) {
                    Some(Want::N(v)) => Data::Float(*v),
                    Some(Want::T(s)) => Data::String(s.clone()),
                    Some(Want::Blank) => Data::Empty,
                    None => g0[i].get(j).cloned().unwrap_or(Data::Empty),
                };
                let have = ga[i].get(j).cloned().unwrap_or(Data::Empty);
                if !cell_eq(&want, &have) {
                    bad(&ctx, n, i + 1, j + 1, &format!("re-export '{have}', expected '{want}'"));
                }
            }
        }
    }

    // the re-exported book is a fixed point
    let rep = import_xlsx_with(&mut m, &r, preview()).unwrap();
    if let Some(x) = rep.changes.first() {
        bad(&ctx, &x.sheet, x.row as usize, x.col as usize, "re-exported book lists a change");
    }
    assert!(
        rep.rejected.is_empty() && rep.ignored.is_empty() && rep.conflicts.is_empty(),
        "{ctx}: {rep:?}"
    );
}

#[test]
fn random_edits_roundtrip_fixed_seeds() {
    for seed in [1, 2, 3, 0xBEEF, 0x5EED] {
        run(seed, false);
        run(seed, true);
    }
}

#[test]
#[ignore = "long: 50 seeds"]
fn random_edits_roundtrip_50_seeds() {
    for seed in 100..150 {
        run(seed, false);
        run(seed, true);
    }
}
