// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Random Apply / undo / redo / reopen / re-export against a linked book that holds new rows
//! (no id) on Walls, Rooms and Levels. The record of created rows must keep a second Apply of
//! the same book from creating anything, whatever happened to the model in between. Rows are
//! only ever appended to the book (no insertion in the middle). A failure names its seed and
//! the operations so far; it is a product finding, not a flaky test.
use std::path::PathBuf;

use ddc_io::xlsx::{self, TableCell as C};
use ddc_model::{Model, RoomSpec, WallSpec};

use super::super::xlsx_job::{self, ApplyTail, Outcome};
use super::super::xlsx_win::{apply_begin, apply_finish, XlsxPreview};
use crate::editor::Editor;
use crate::xl_text::Lang;

/// (sheet of the book, table of the elements)
const SHEETS: [(&str, &str); 3] = [("Walls", "wall"), ("Rooms", "room"), ("Levels", "level")];

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

struct World {
    ed: Editor,
    dir: PathBuf,
    book: PathBuf,
    /// elements per table before any Apply
    base: [i64; 3],
    /// rows appended to each sheet of the book
    rows: [i64; 3],
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

fn counts(m: &Model) -> [i64; 3] {
    [count(m, "wall"), count(m, "room"), count(m, "level")]
}

fn first_id(m: &Model, table: &str) -> i64 {
    m.sql().query_row(&format!("SELECT min(id) FROM {table}"), [], |r| r.get(0)).unwrap()
}

/// A project with 4 walls, 2 rooms and 2 levels, exported to a book that then gets
/// `extra` more new rows per sheet on top of one each.
fn world(tag: &str, seed: u64, extra: [usize; 3]) -> World {
    let dir = std::env::temp_dir().join(format!("ddc_tst30_{tag}_{seed}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let m = Model::open(dir.join("m.ddc").to_str().unwrap()).unwrap();
    let mut ed = Editor::seeded(m).unwrap();
    let (lv, ty) = (ed.level, ed.types[1].id);
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    for i in 0..4 {
        let spec = WallSpec { a: p[i], b: p[(i + 1) % 4], level: lv, base_offset: 0.0, height: 3.0, wall_type: ty, offset0: None };
        ed.model.add_wall(&spec).unwrap();
    }
    for (name, number, point) in [("Hall", "101", (1.0, 1.0)), ("Office", "102", (5.0, 3.0))] {
        let spec = RoomSpec { level: lv, name: name.into(), number: number.into(), point, height: 2.7 };
        ed.model.add_room(&spec).unwrap();
    }
    ed.model.add_level("L2", 3.0).unwrap();
    ed.reload().unwrap();
    let book = dir.join("book.xlsx");
    assert!(matches!(xlsx_job::export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
    let rows = [1 + extra[0] as i64, 1 + extra[1] as i64, 1 + extra[2] as i64];
    let w = World { base: counts(&ed.model), ed, dir, book, rows };
    w.append_rows();
    w
}

impl World {
    /// New rows at the end of the sheets, written one file generation at a time.
    fn append_rows(&self) {
        let mut cur = self.book.clone();
        let mut n = 0;
        // the export writes captions, and columns are read by the caption; the unit is optional
        let caps = super::super::xlsx_style::style("en").captions;
        let cap = |key: &str| caps[key].as_str();
        for (si, (sheet, table)) in SHEETS.iter().enumerate() {
            let from = first_id(&self.ed.model, table);
            for k in 0..self.rows[si] {
                let f = k as f64;
                let tweaks: Vec<(&str, C)> = match *sheet {
                    "Walls" => vec![
                        (cap("ax"), C::Num(20.0 + 3.0 * f)),
                        (cap("ay"), C::Num(10.0)),
                        (cap("bx"), C::Num(22.0 + 3.0 * f)),
                        (cap("by"), C::Num(10.0)),
                    ],
                    "Rooms" => vec![
                        (cap("name"), C::Text(format!("New{k}"))),
                        (cap("number"), C::Text(format!("9{k}"))),
                        // outside the walled region: a point inside Hall's region is refused
                        (cap("px"), C::Num(40.0 + 3.0 * f)),
                        (cap("py"), C::Num(40.0)),
                    ],
                    _ => vec![
                        (cap("name"), C::Text(format!("N{k}"))),
                        (cap("elevation"), C::Num(6.0 + 3.0 * f)),
                    ],
                };
                n += 1;
                let next = self.dir.join(format!("gen{n}.xlsx"));
                xlsx::clone_row_as_new(&cur, &next, sheet, from, &tweaks).unwrap();
                cur = next;
            }
        }
        std::fs::copy(&cur, &self.book).unwrap();
    }

    /// One Apply of the book as the window does it; the error text when it was refused.
    fn apply(&mut self) -> Result<(), String> {
        let mut pv = XlsxPreview::load(&mut self.ed.model, &self.book, false)?;
        pv.rev = self.ed.content_snap();
        apply_begin(&mut self.ed, &mut pv, Lang::En).map(|_| ())
    }

    fn preview_added(&mut self) -> usize {
        XlsxPreview::load(&mut self.ed.model, &self.book, false).unwrap().report.counts().added
    }

    /// The re-export that follows an Apply: the created rows get their ids.
    fn reexport(&mut self) {
        let o = xlsx_job::export_sync(&self.ed.model, &self.book, xlsx::Units::Metric, "en");
        assert!(matches!(o, Outcome::Done), "re-export failed");
        let mut pv = XlsxPreview::linked(&self.book, false);
        let tail = ApplyTail { created: 0, ..Default::default() };
        apply_finish(self.ed.content_snap(), &mut self.ed.model, &mut pv, &tail, &Outcome::Done);
    }

    /// Save and open the project file again (the connection is closed first).
    fn reopen(&mut self) {
        let path = self.ed.model.db_path().expect("file model");
        drop(std::mem::replace(&mut self.ed.model, Model::open(":memory:").unwrap()));
        self.ed.model = Model::open(&path).unwrap();
        self.ed.reload().unwrap();
    }

    fn done(self) {
        let World { ed, dir, .. } = self;
        drop(ed);
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn limit(w: &World) -> [i64; 3] {
    [w.base[0] + w.rows[0], w.base[1] + w.rows[1], w.base[2] + w.rows[2]]
}

fn ctx(seed: u64, log: &[&str]) -> String {
    format!("seed {seed}, operations so far: {log:?}")
}

/// One seeded walk; every invariant names the seed.
fn walk(seed: u64, steps: usize) {
    let mut rng = Rng::new(seed);
    let extra = [rng.below(3), rng.below(3), rng.below(3)];
    let mut w = world("walk", seed, extra);
    let lim = limit(&w);
    let rows = w.preview_added();
    assert_eq!(rows as i64, w.rows.iter().sum::<i64>(), "setup: every appended row is offered as new, seed {seed}");
    let mut log: Vec<&str> = vec![];
    // the model is exactly as the last Apply left it: another Apply has nothing to create
    let mut settled = false;
    for _ in 0..steps {
        let before = counts(&w.ed.model);
        match rng.below(7) {
            0 | 1 => {
                log.push("apply");
                let refused = w.apply().is_err();
                let after = counts(&w.ed.model);
                if refused {
                    assert_eq!(after, before, "a refused Apply changed the model, {}", ctx(seed, &log));
                } else if settled {
                    assert_eq!(after, before, "second Apply of an unchanged book created elements, {}", ctx(seed, &log));
                }
                settled = !refused;
            }
            2 => {
                log.push("apply twice");
                let _ = w.apply();
                let mid = counts(&w.ed.model);
                let second = w.apply();
                assert_eq!(counts(&w.ed.model), mid, "second Apply created elements, {}", ctx(seed, &log));
                settled = second.is_ok();
            }
            3 => {
                log.push("undo");
                w.ed.undo();
                settled = false;
            }
            4 => {
                log.push("redo");
                w.ed.redo();
                settled = false;
            }
            5 => {
                log.push("reopen");
                w.reopen();
            }
            _ => {
                log.push("re-export");
                w.reexport();
            }
        }
        let now = counts(&w.ed.model);
        for i in 0..3 {
            assert!(now[i] <= lim[i], "{} has {} elements, at most {}, {}", SHEETS[i].1, now[i], lim[i], ctx(seed, &log));
        }
        let iss = w.ed.model.check_integrity().unwrap();
        assert!(iss.is_empty(), "integrity {iss:?}, {}", ctx(seed, &log));
    }
    w.done();
}

#[test]
fn links_fuzz_second_apply_creates_nothing() {
    for seed in 1..=24 {
        walk(seed, 20);
    }
}

#[test]
#[ignore = "300 seeds, run on demand"]
fn links_fuzz_second_apply_creates_nothing_300() {
    for seed in 1..=300 {
        walk(seed, 30);
    }
}

#[test]
fn links_fuzz_undo_then_apply_once() {
    for seed in 1..=12u64 {
        let mut rng = Rng::new(seed);
        let mut w = world("undo", seed, [rng.below(3), rng.below(3), rng.below(3)]);
        let lim = limit(&w);
        w.apply().unwrap_or_else(|e| panic!("seed {seed}: first Apply refused: {e}"));
        let created = counts(&w.ed.model);
        assert_eq!(created, lim, "seed {seed}: the first Apply creates every new row once");
        // the creating Apply is undone: its rows are new again and the next Apply makes them once
        w.ed.undo();
        assert_eq!(counts(&w.ed.model), w.base, "seed {seed}: one undo step removes everything the Apply made");
        assert_eq!(w.preview_added() as i64, w.rows.iter().sum::<i64>(), "seed {seed}: undone rows are offered again");
        w.apply().unwrap_or_else(|e| panic!("seed {seed}: Apply after undo refused: {e}"));
        assert_eq!(counts(&w.ed.model), lim, "seed {seed}: Apply after undo creates the rows once");
        w.apply().ok();
        assert_eq!(counts(&w.ed.model), lim, "seed {seed}: and not twice");
        w.done();
    }
}

#[test]
fn links_fuzz_reopen_keeps_record() {
    for seed in 1..=12u64 {
        let mut rng = Rng::new(seed);
        let mut w = world("reopen", seed, [rng.below(3), rng.below(3), rng.below(3)]);
        let lim = limit(&w);
        w.apply().unwrap_or_else(|e| panic!("seed {seed}: Apply refused: {e}"));
        assert_eq!(counts(&w.ed.model), lim);
        w.reopen();
        assert_eq!(w.preview_added(), 0, "seed {seed}: reopened project offers the created rows again");
        w.apply().ok();
        assert_eq!(counts(&w.ed.model), lim, "seed {seed}: Apply after reopen created elements");
        // the re-export gives the rows their ids; reopening again and applying still creates nothing
        w.reexport();
        w.reopen();
        assert_eq!(w.preview_added(), 0, "seed {seed}: re-exported book offers new rows");
        w.apply().ok();
        assert_eq!(counts(&w.ed.model), lim, "seed {seed}: Apply after re-export and reopen created elements");
        w.done();
    }
}
