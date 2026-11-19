//! Seeded XLSX round trip of the Rooms, Slabs and Curtain walls sheets: random edits of the
//! editable columns are listed by the preview, applied to the model and survive a re-export;
//! read-only edits are refused; an unchanged book gives zero edits.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{
    CurtainSpec, GridLines, LayerSpec, Model, MullionSection, OpeningSpec, RoomSpec, SlabSpec,
    WallSpec,
};

use super::tests::{edit, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;
type Book = Vec<(String, Grid)>;
type Cells = BTreeSet<(String, usize, usize)>;
type Key = Vec<(i64, i64)>;

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
    /// Millimetres around the current value.
    Near(i64),
    /// New text with this prefix.
    Text(&'static str),
    /// A different value out of the list.
    Pick(&'static [&'static str]),
    /// A different creation phase of the model.
    Phase,
    /// New rectangular outline.
    Ring,
}

struct T {
    sheet: &'static str,
    header: &'static str,
    /// Model table; for a phase, the element kind.
    table: &'static str,
    /// SQL expression of the value (may use the table's columns).
    col: &'static str,
    k: K,
}

const fn t(
    sheet: &'static str,
    header: &'static str,
    table: &'static str,
    col: &'static str,
    k: K,
) -> T {
    T { sheet, header, table, col, k }
}

const LEVEL_NAMES: &[&str] = &["L1", "L2"];
const SLAB_TYPES: &[&str] = &["floor", "deck"];

const ROOM_T: &[T] = &[
    t(ROOMS, "name", "room", "name", K::Text("rm")),
    t(ROOMS, "number", "room", "number", K::Text("N")),
    t(ROOMS, "point x [m]", "room", "px", K::Near(300)),
    t(ROOMS, "point y [m]", "room", "py", K::Near(300)),
    t(ROOMS, "height [m]", "room", "height", K::Num(2500, 4000)),
];
const SLAB_T: &[T] = &[
    t(SLABS, "level", "slab", "(SELECT name FROM level WHERE level.id=slab.level)", K::Pick(LEVEL_NAMES)),
    t(SLABS, "type", "slab", "(SELECT name FROM slab_type WHERE slab_type.id=slab.stype)", K::Pick(SLAB_TYPES)),
    t(SLABS, "offset [m]", "slab", "\"offset\"", K::Num(-300, 300)),
    t(SLABS, "outline [m] (x y; x y; ...)", "slab", "", K::Ring),
];
const CURTAIN_T: &[T] = &[
    t(CURTAINS, "level", "curtain_wall", "(SELECT name FROM level WHERE level.id=curtain_wall.level)", K::Pick(LEVEL_NAMES)),
    t(CURTAINS, "name", "curtain_wall", "name", K::Text("cw")),
    t(CURTAINS, "base_offset [m]", "curtain_wall", "base_offset", K::Num(-500, 500)),
    t(CURTAINS, PHASE_CREATED, "curtain_wall", "", K::Phase),
];

struct Case {
    sheet: &'static str,
    targets: &'static [T],
    /// Read-only column whose edits are refused.
    ro: &'static str,
}

const ROOMS_CASE: Case = Case { sheet: ROOMS, targets: ROOM_T, ro: "area [m2] (read-only)" };
const SLABS_CASE: Case = Case { sheet: SLABS, targets: SLAB_T, ro: "plan area [m2] (read-only)" };
const CURTAIN_CASE: Case = Case { sheet: CURTAINS, targets: CURTAIN_T, ro: "area [m2] (read-only)" };

#[derive(Clone, Debug)]
enum Want {
    N(f64),
    T(String),
    P(Key),
}

struct Plan {
    sheet: &'static str,
    i: usize,
    j: usize,
    data: Data,
    want: Want,
    /// Index into the case targets; `None` for a read-only cell.
    target: Option<usize>,
}

fn fixture() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.2).unwrap();
    let layer = |n: &str, t: f64| LayerSpec { material: n.into(), thickness: t };
    let ext = m.add_wall_type("ext", &[layer("brick", 0.2)]).unwrap();
    let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.), (0., 0.)];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[i + 1],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ext,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    let door = OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
    m.add_opening(1, &door).unwrap();
    // one room inside the closed wall loop, one in the open
    let room = |name: &str, number: &str, point: (f64, f64), height: f64| RoomSpec {
        level: l1,
        name: name.into(),
        number: number.into(),
        point,
        height,
    };
    m.add_room(&room("Hall", "101", (2.5, 2.0), 3.0)).unwrap();
    m.add_room(&room("Yard", "102", (10.0, 10.0), 2.8)).unwrap();
    let floor = m.add_slab_type("floor", &[layer("concrete", 0.2)]).unwrap();
    let deck = m.add_slab_type("deck", &[layer("timber", 0.1)]).unwrap();
    m.add_slab(&SlabSpec {
        level: l1,
        slab_type: floor,
        offset: 0.0,
        outer: vec![(0., 0.), (5., 0.), (5., 4.), (0., 4.)],
        holes: vec![],
    })
    .unwrap();
    m.add_slab(&SlabSpec {
        level: l2,
        slab_type: deck,
        offset: 0.1,
        outer: vec![(6., 0.), (9., 0.), (9., 3.), (6., 3.)],
        holes: vec![],
    })
    .unwrap();
    let sec = MullionSection::Rect { w: 0.05, d: 0.1 };
    m.add_curtain_wall(&CurtainSpec {
        level: l1,
        name: "CW1".into(),
        a: (0.0, 6.0),
        b: (4.0, 6.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: GridLines::Spacing(1.0),
        v: GridLines::At(vec![1.0, 2.0]),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    })
    .unwrap();
    m
}

fn captions() -> HashMap<String, String> {
    [("height", "Vysota"), ("level", "Uroven"), ("type", "Tip"), ("name", "Imya")]
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
            let g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
            (n, g)
        })
        .collect()
}

fn sheet<'a>(b: &'a Book, name: &str) -> &'a Grid {
    &b.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("sheet {name}")).1
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

/// Corner set of a ring in micrometres: independent of start point and winding.
fn key(pts: impl Iterator<Item = (f64, f64)>) -> Key {
    let mut k: Key = pts.map(|(x, y)| ((x * 1e6).round() as i64, (y * 1e6).round() as i64)).collect();
    k.sort_unstable();
    k.dedup();
    k
}

fn ring_key(text: &str) -> Key {
    key(text.split(';').map(str::trim).filter(|p| !p.is_empty()).map(|p| {
        let v: Vec<f64> = p.split_whitespace().map(|x| x.parse().unwrap()).collect();
        (v[0], v[1])
    }))
}

fn make_plans(
    rng: &mut Rng,
    ru: bool,
    case: &Case,
    book: &Book,
    plain: &Book,
    phases: &[String],
) -> (Vec<Plan>, Vec<Plan>) {
    let dec = |x: f64| {
        let s = x.to_string();
        if ru {
            s.replace('.', ",")
        } else {
            s
        }
    };
    let mut plans = vec![];
    let g = sheet(book, case.sheet);
    for (ti, t) in case.targets.iter().enumerate() {
        let j = pos(plain, t.sheet, t.header);
        for i in 1..g.len() {
            if i != 1 && !rng.chance(1, 2) {
                continue;
            }
            let old = g[i].get(j).cloned().unwrap_or(Data::Empty);
            let otext = old.to_string();
            let otext = otext.trim();
            let range = match t.k {
                K::Num(lo, hi) => Some((lo, hi)),
                K::Near(s) => {
                    let o = (num_of(&old).unwrap() * 1000.0).round() as i64;
                    Some((o - s, o + s))
                }
                _ => None,
            };
            let (data, want) = if let Some((lo, hi)) = range {
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
            } else {
                match t.k {
                    K::Text(prefix) => {
                        let mut s = format!("{prefix}{}", rng.below(1000));
                        if otext == s {
                            s.push('x');
                        }
                        (Data::String(s.clone()), Want::T(s))
                    }
                    K::Pick(opts) => {
                        let rest: Vec<&str> = opts.iter().copied().filter(|o| *o != otext).collect();
                        let s = rest[rng.below(rest.len() as u64) as usize].to_string();
                        (Data::String(s.clone()), Want::T(s))
                    }
                    K::Phase => {
                        let rest: Vec<&String> = phases.iter().filter(|p| p.as_str() != otext).collect();
                        let s = rest[rng.below(rest.len() as u64) as usize].clone();
                        (Data::String(s.clone()), Want::T(s))
                    }
                    K::Ring => {
                        let w = 3000 + rng.below(4000) as i64;
                        let h = 2000 + rng.below(3000) as i64;
                        let x0 = 10.0 * i as f64;
                        let f = |mm: i64| mm as f64 / 1000.0;
                        let pts = [(x0, 0.0), (x0 + f(w), 0.0), (x0 + f(w), f(h)), (x0, f(h))];
                        let text = pts
                            .iter()
                            .map(|p| format!("{} {}", p.0, p.1))
                            .collect::<Vec<_>>()
                            .join("; ");
                        (Data::String(text), Want::P(key(pts.iter().copied())))
                    }
                    K::Num(..) | K::Near(_) => unreachable!(),
                }
            };
            plans.push(Plan { sheet: t.sheet, i, j, data, want, target: Some(ti) });
        }
    }
    let mut ro = vec![];
    let j = pos(plain, case.sheet, case.ro);
    for i in 1..g.len() {
        // a blank computed cell (room outside the walls) has no value to protect
        if let Some(o) = g[i].get(j).and_then(num_of) {
            if rng.chance(1, 2) || ro.is_empty() {
                let v = o + 1.0 + rng.below(100) as f64 / 10.0;
                ro.push(Plan {
                    sheet: case.sheet,
                    i,
                    j,
                    data: Data::Float(v),
                    want: Want::N(v),
                    target: None,
                });
            }
        }
    }
    (plans, ro)
}

fn cells(plans: &[Plan]) -> Cells {
    plans.iter().map(|p| (p.sheet.to_string(), p.i + 1, p.j + 1)).collect()
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
    let id = num_of(&g0[i][pos(plain, t.sheet, "id")]).unwrap() as i64;
    match t.k {
        K::Phase => {
            let idx = m.phase_index().unwrap();
            Want::T(idx.name(m.element_phase(t.table, id).unwrap().created).to_string())
        }
        K::Ring => {
            let mut st = m
                .sql()
                .prepare("SELECT x,y FROM slab_vertex WHERE slab=?1 AND ring=0")
                .unwrap();
            let pts: Vec<(f64, f64)> = st
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|x| x.unwrap())
                .collect();
            Want::P(key(pts.into_iter()))
        }
        K::Text(_) | K::Pick(_) => {
            let sql = format!("SELECT {} FROM {} WHERE id={id}", t.col, t.table);
            Want::T(m.sql().query_row(&sql, [], |r| r.get::<_, String>(0)).unwrap())
        }
        K::Num(..) | K::Near(_) => {
            let sql = format!("SELECT {} FROM {} WHERE id={id}", t.col, t.table);
            Want::N(m.sql().query_row(&sql, [], |r| r.get::<_, f64>(0)).unwrap())
        }
    }
}

fn want_eq(a: &Want, b: &Want) -> bool {
    match (a, b) {
        (Want::N(a), Want::N(b)) => (a - b).abs() <= 1e-9,
        (Want::T(a), Want::T(b)) => a == b,
        (Want::P(a), Want::P(b)) => a == b,
        _ => false,
    }
}

fn op_count(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap()
}

fn preview() -> ImportOptions {
    ImportOptions { apply: false, delete_missing: false, new_params: false }
}

fn change_cells(rep: &Report) -> Cells {
    rep.changes.iter().map(|c| (c.sheet.clone(), c.row as usize, c.col as usize)).collect()
}

fn assert_noop(ctx: &str, rep: &Report) {
    if !(rep.changes.is_empty()
        && rep.rejected.is_empty()
        && rep.ignored.is_empty()
        && rep.conflicts.is_empty()
        && rep.edits.is_empty())
    {
        bad(ctx, "-", 0, 0, &format!("unchanged book is not a no-op: {rep:?}"));
    }
}

fn run(case: &Case, seed: u64, ru: bool) {
    let loc = if ru { "ru" } else { "en" };
    let ctx = format!("{} seed={seed} loc={loc}", case.sheet);
    let mut rng = Rng::new(seed);
    let mut m = fixture();
    let name = format!("sheets_{}_{seed}_{loc}", case.sheet.replace(' ', "_"));
    let p = tmp(&format!("{name}_a"));
    let q = tmp(&format!("{name}_b"));
    let r = tmp(&format!("{name}_c"));
    let plain_p = tmp(&format!("{name}_plain"));
    export_xlsx(&m, &plain_p).unwrap();
    let plain = read_book(&plain_p);
    export_loc(&m, &p, &p, ru);
    let book = read_book(&p);
    let n = case.sheet;
    if sheet(&book, n)[0].len() != sheet(&plain, n)[0].len() {
        bad(&ctx, n, 1, 0, "column count differs between the plain and the localized book");
    }
    let phases: Vec<String> =
        m.phase_index().unwrap().phases.iter().map(|p| p.name.clone()).collect();

    let ops = op_count(&m);
    let rep = import_xlsx_with(&mut m, &p, preview()).unwrap();
    assert_noop(&ctx, &rep);

    let (plans, ro) = make_plans(&mut rng, ru, case, &book, &plain, &phases);
    assert!(!plans.is_empty() && !ro.is_empty(), "{ctx}: no edits planned");
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
        let tg = &case.targets[pl.target.unwrap()];
        let have = model_value(&m, tg, sheet(&book, tg.sheet), &plain, pl.i);
        if !want_eq(&have, &pl.want) {
            bad(&ctx, pl.sheet, pl.i + 1, pl.j + 1, &format!("model {have:?}, book {:?}", pl.want));
        }
    }

    // re-export equals the edited book (read-only cells keep their computed value, unless the
    // outline of their row changed)
    export_loc(&m, &p, &r, ru);
    let again = read_book(&r);
    let (g0, ga) = (sheet(&book, n), sheet(&again, n));
    if g0.len() != ga.len() {
        bad(&ctx, n, 0, 0, &format!("row count {} -> {}", g0.len(), ga.len()));
    }
    for (j, h) in sheet(&plain, n)[0].iter().enumerate() {
        let h = h.to_string();
        let is_ro = h.contains("(read-only)");
        if is_ro && h != case.ro {
            continue;
        }
        for i in 1..g0.len() {
            let over = plans.iter().find(|pl| pl.sheet == n && pl.i == i && pl.j == j);
            let moved = plans
                .iter()
                .any(|pl| pl.sheet == n && pl.i == i && matches!(pl.want, Want::P(_)));
            if is_ro && moved {
                continue;
            }
            let have = ga[i].get(j).cloned().unwrap_or(Data::Empty);
            if let Some(Want::P(k)) = over.map(|pl| &pl.want) {
                if ring_key(&have.to_string()) != *k {
                    bad(&ctx, n, i + 1, j + 1, &format!("re-export outline '{have}'"));
                }
                continue;
            }
            let want = match over.map(|pl| &pl.want) {
                Some(Want::N(v)) => Data::Float(*v),
                Some(Want::T(s)) => Data::String(s.clone()),
                _ => g0[i].get(j).cloned().unwrap_or(Data::Empty),
            };
            if !cell_eq(&want, &have) {
                bad(&ctx, n, i + 1, j + 1, &format!("re-export '{have}', expected '{want}'"));
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

const SEEDS: [u64; 5] = [1, 2, 3, 0xBEEF, 0x5EED];

#[test]
fn rooms_sheet_roundtrip_seeded() {
    for seed in SEEDS {
        run(&ROOMS_CASE, seed, false);
        run(&ROOMS_CASE, seed, true);
    }
}

#[test]
fn slabs_sheet_roundtrip_seeded() {
    for seed in SEEDS {
        run(&SLABS_CASE, seed, false);
        run(&SLABS_CASE, seed, true);
    }
}

#[test]
fn curtain_sheet_roundtrip_seeded() {
    for seed in SEEDS {
        run(&CURTAIN_CASE, seed, false);
        run(&CURTAIN_CASE, seed, true);
    }
}

#[test]
fn three_sheets_unchanged_zero_edits() {
    for ru in [false, true] {
        let mut m = fixture();
        let p = tmp(&format!("sheets_same_{}", u8::from(ru)));
        export_loc(&m, &p, &p, ru);
        let book = read_book(&p);
        for n in [ROOMS, SLABS, CURTAINS] {
            assert!(sheet(&book, n).len() > 1, "{n} has no rows (ru={ru})");
        }
        let ops = op_count(&m);
        let rep = import_xlsx_with(&mut m, &p, preview()).unwrap();
        assert_noop(&format!("preview ru={ru}"), &rep);
        let rep = import_xlsx(&mut m, &p).unwrap();
        assert!(rep.applied, "ru={ru}: {rep:?}");
        assert_noop(&format!("apply ru={ru}"), &rep);
        assert_eq!(ops, op_count(&m), "ru={ru}: unchanged book added operations");
    }
}
