// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Read-back proof of an exported book: a clean book replaces the old one; a book that reads
//! back otherwise (test hook on the written file) keeps the old book byte for byte and names
//! the first differing cell. Seeded random models must always pass; a failure names its seed.
use std::path::{Path, PathBuf};

use calamine::{Data, Reader};
use ddc_io::xlsx;
use ddc_model::{LayerSpec, WallSpec};

use super::super::xlsx_job::{export_sync, Outcome};
use super::{differences, Fault, BEFORE_READ};
use crate::editor::Editor;
use crate::xl_text::Lang;

type Grid = Vec<Vec<Data>>;

fn file_editor(tag: &str) -> (Editor, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ddc_xs6_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let m = ddc_model::Model::open(dir.join("m.ddc").to_str().unwrap()).unwrap();
    (Editor::seeded(m).unwrap(), dir)
}

fn part_of(book: &Path) -> PathBuf {
    let stem = book.file_stem().unwrap().to_string_lossy().into_owned();
    book.with_file_name(format!("{stem}.export-part.xlsx"))
}

/// Copy of `src` where `f` may change each sheet grid (values only).
fn rewrite(src: &Path, dst: &Path, f: &dyn Fn(&str, &mut Grid)) {
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(src).unwrap();
    let mut out = rust_xlsxwriter::Workbook::new();
    for name in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
        f(&name, &mut g);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

/// Column whose header starts with `prefix` (any case).
fn head(g: &Grid, prefix: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string().to_lowercase().starts_with(prefix))
        .unwrap_or_else(|| panic!("no column {prefix}: {:?}", g[0]))
}

/// Grid row of element `id`.
fn row_of(g: &Grid, id: i64) -> usize {
    let c = head(g, "id");
    (1..g.len())
        .find(|&r| g[r].get(c).and_then(|d| d.to_string().parse::<f64>().ok()) == Some(id as f64))
        .unwrap_or_else(|| panic!("no row for {id}"))
}

fn put(g: &mut Grid, r: usize, c: usize, d: Data) {
    while g[r].len() <= c {
        g[r].push(Data::Empty);
    }
    g[r][c] = d;
}

/// The next read-back sees the written file changed by `f` on sheet `sheet`.
fn tamper(sheet: &'static str, f: impl Fn(&mut Grid) + 'static) {
    let hook = move |part: &Path| {
        let tmp = part.with_file_name("tampered.xlsx");
        rewrite(part, &tmp, &|name: &str, g: &mut Grid| {
            if name == sheet {
                f(g)
            }
        });
        std::fs::rename(&tmp, part).unwrap();
    };
    BEFORE_READ.with(|b| *b.borrow_mut() = Some(Box::new(hook)));
}

fn export(ed: &Editor, book: &Path) -> Outcome {
    export_sync(&ed.model, book, xlsx::Units::Metric, "en")
}

/// Exports `ed` once cleanly, then again with `f` applied to the written file: the second
/// export must be refused, the first book stay byte for byte, no part file remain.
fn refused(tag: &str, ed: &Editor, dir: &Path, sheet: &'static str, f: impl Fn(&mut Grid) + 'static) -> String {
    let book = dir.join("b.xlsx");
    assert!(matches!(export(ed, &book), Outcome::Done), "{tag}: clean export");
    let before = std::fs::read(&book).unwrap();
    tamper(sheet, f);
    let Outcome::Failed(e) = export(ed, &book) else {
        panic!("{tag}: a book that reads back otherwise must not replace the old one");
    };
    assert_eq!(std::fs::read(&book).unwrap(), before, "{tag}: old book byte for byte");
    assert!(!part_of(&book).exists(), "{tag}: no part file left");
    assert!(!dir.join("b.xlsx.bak").exists(), "{tag}: nothing moved aside");
    assert!(e.starts_with("read-back check failed"), "{tag}: {e}");
    // the UI language is not touched: both texts are asked for explicitly
    let ru = crate::xl_text::tr(&e, Lang::Ru);
    assert_ne!(ru, e, "{tag}: translated by template");
    e
}

fn level_ed(tag: &str, name: &str) -> (Editor, PathBuf, i64) {
    let (mut ed, dir) = file_editor(tag);
    let id = ed.model.add_level(name, 3.0).unwrap();
    (ed, dir, id)
}

#[test]
fn clean_export_reads_back_and_replaces() {
    let (ed, dir, _) = level_ed("clean", "L2");
    let book = dir.join("b.xlsx");
    assert!(matches!(export(&ed, &book), Outcome::Done));
    assert!(differences(&ed.model, &book, &book, &[], &[]).unwrap().is_empty());
    // the hook sees the file, changes nothing: the rewrite alone is no difference
    tamper("Levels", |_| {});
    assert!(matches!(export(&ed, &book), Outcome::Done));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changed_value_named_with_cell_expected_and_read() {
    let (ed, dir, id) = level_ed("value", "L2");
    let e = refused("value", &ed, &dir, "Levels", move |g| {
        let (r, c) = (row_of(g, id), head(g, "name"));
        put(g, r, c, Data::String("L2 wrong".into()));
    });
    assert!(e.contains("Levels!") && e.contains("'L2'") && e.contains("'L2 wrong'"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_row_named() {
    let (mut ed, dir, id) = wall_ed("missing");
    // a second wall: a sheet left with no row at all is reported as emptied, not row by row
    let sql = "SELECT wtype FROM wall WHERE id=?1";
    let t: i64 = ed.model.sql().query_row(sql, [id], |r| r.get(0)).unwrap();
    let (a, b, level) = ((0.0, 5.0), (4.0, 5.0), ed.level);
    let w = WallSpec { a, b, level, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
    ed.model.add_wall(&w).unwrap();
    let e = refused("missing", &ed, &dir, "Walls", move |g| {
        let r = row_of(g, id);
        g.remove(r);
    });
    assert!(e.contains("Walls") && e.contains(&format!("id {id}")), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn extra_row_named() {
    let (ed, dir, id) = level_ed("extra", "L2");
    let e = refused("extra", &ed, &dir, "Levels", move |g| {
        // a copy of a row under an id the model does not have
        let mut row = g[row_of(g, id)].clone();
        row[head(g, "id")] = Data::Float(9999.0);
        row[head(g, "name")] = Data::String("Ghost".into());
        g.push(row);
    });
    assert!(e.contains("Levels!") && e.contains("not in the model"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wrong_type_and_number_format_named() {
    let (ed, dir, id) = level_ed("type", "L2");
    let e = refused("type", &ed, &dir, "Levels", move |g| {
        let (r, c) = (row_of(g, id), head(g, "elevation"));
        put(g, r, c, Data::String("three".into()));
    });
    assert!(e.contains("Levels!"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
    let (ed, dir, id) = level_ed("number", "L2");
    let e = refused("number", &ed, &dir, "Levels", move |g| {
        let (r, c) = (row_of(g, id), head(g, "elevation"));
        // the shown number nudged in its own unit, as a formatting slip would do
        let v = g[r].get(c).and_then(|d| d.to_string().parse::<f64>().ok()).unwrap();
        put(g, r, c, Data::Float(v * 1.0002 + 0.001));
    });
    assert!(e.contains("Levels!") && e.contains("expected '3"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn long_text_difference_named_and_shown_short() {
    let long: String = "abc".repeat(3000) + "MIDDLE" + &"xyz".repeat(3000);
    let (ed, dir, id) = level_ed("long", &long);
    let bad = long.replace("MIDDLE", "MIDDLF");
    let e = refused("long", &ed, &dir, "Levels", move |g| {
        let (r, c) = (row_of(g, id), head(g, "name"));
        put(g, r, c, Data::String(bad.clone()));
    });
    assert!(e.contains("MIDDLE") && e.contains("MIDDLF") && e.contains("..."), "{e}");
    assert!(e.chars().count() < 400, "{} chars", e.chars().count());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unicode_difference_named_exactly() {
    let name = "\u{00c5}rhus \u{4e2d}\u{6587} \u{1f3d7}";
    let (ed, dir, id) = level_ed("unicode", name);
    let e = refused("unicode", &ed, &dir, "Levels", move |g| {
        let (r, c) = (row_of(g, id), head(g, "name"));
        put(g, r, c, Data::String("\u{00c5}rhus \u{4e2d}\u{6587} \u{1f3e0}".into()));
    });
    assert!(e.contains(name) && e.contains('\u{1f3e0}'), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn wall_ed(tag: &str) -> (Editor, PathBuf, i64) {
    let (mut ed, dir) = file_editor(tag);
    let t = ed.model.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.25 }]).unwrap();
    let w = WallSpec { a: (0.0, 0.0), b: (4.0, 0.0), level: ed.level, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
    let id = ed.model.add_wall(&w).unwrap();
    (ed, dir, id)
}

#[test]
fn computed_read_only_column_is_not_a_difference() {
    let (ed, dir, id) = wall_ed("readonly");
    let book = dir.join("b.xlsx");
    assert!(matches!(export(&ed, &book), Outcome::Done));
    tamper("Walls", move |g| {
        let (r, c) = (row_of(g, id), head(g, "length"));
        put(g, r, c, Data::Float(99.0));
    });
    assert!(matches!(export(&ed, &book), Outcome::Done), "a computed column may read back otherwise");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unreadable_written_book_keeps_old_one() {
    let (ed, dir, _) = level_ed("unreadable", "L2");
    let book = dir.join("b.xlsx");
    assert!(matches!(export(&ed, &book), Outcome::Done));
    let before = std::fs::read(&book).unwrap();
    BEFORE_READ.with(|b| *b.borrow_mut() = Some(Box::new(|p: &Path| std::fs::write(p, b"not a workbook").unwrap())));
    let Outcome::Failed(e) = export(&ed, &book) else {
        panic!("an unreadable new book must not replace the old one");
    };
    assert!(e.contains("cannot be read"), "{e}");
    assert_eq!(std::fs::read(&book).unwrap(), before);
    assert!(!part_of(&book).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_export_refused_leaves_no_book() {
    let (ed, dir, id) = level_ed("first", "L2");
    let book = dir.join("b.xlsx");
    tamper("Levels", move |g| {
        let (r, c) = (row_of(g, id), head(g, "name"));
        put(g, r, c, Data::String("L9".into()));
    });
    assert!(matches!(export(&ed, &book), Outcome::Failed(_)));
    assert!(!book.exists() && !part_of(&book).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fault_texts_follow_templates_in_both_languages() {
    let f = Fault::Unreadable("zip error".into());
    let en = f.text();
    assert!(en.contains("zip error"), "{en}");
    let ru = crate::xl_text::tr(&en, Lang::Ru);
    assert!(ru.contains("zip error") && ru != en, "{ru}");
    let en_cat = |k: &str| crate::tr_lang(false, k, &[]);
    for (key, tpl) in xlsx::VERIFY_TEMPLATES {
        assert_eq!(en_cat(key), tpl, "en locale entry {key} equals the template");
        let ru = crate::tr_lang(true, key, &[]);
        assert_eq!(ru.matches("{}").count(), tpl.matches("{}").count(), "{key}: same slots in ru");
    }
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) % n.max(1)
    }
}

fn text(r: &mut Rng, i: usize) -> String {
    match r.below(6) {
        0 => format!("Level {i}"),
        1 => format!("\u{00c5}rhus \u{4e2d}\u{6587} \u{1f3d7} {i}"),
        // over the cell limit: written cut, read-only on the way back
        2 => format!("{i}") + &"x".repeat(40_000),
        3 => format!("{i} ") + &"ab\u{00e9}".repeat(3000),
        4 => format!("Q{i} \"quoted\" & <tag> 'apos'"),
        _ => format!("{i}.{}", r.below(1000)),
    }
}

/// Random model: levels with odd names and elevations (one may be infinite), wall and floor
/// types with odd layer names, walls (some pinned).
fn random_editor(seed: u64) -> (Editor, PathBuf) {
    let (mut ed, dir) = file_editor(&format!("seed{seed}"));
    let mut r = Rng::new(seed);
    let mut levels = vec![ed.level];
    for i in 0..1 + r.below(4) as usize {
        let e = r.below(20_000) as f64 / 997.0;
        levels.push(ed.model.add_level(&text(&mut r, i), e).unwrap());
    }
    let mut types = vec![];
    for i in 0..1 + r.below(3) as usize {
        let layers: Vec<LayerSpec> = (0..1 + r.below(3))
            .map(|k| LayerSpec { material: text(&mut r, k as usize).chars().take(80).collect(), thickness: 0.01 + r.below(400) as f64 / 1000.0 })
            .collect();
        types.push(ed.model.add_wall_type(&format!("WT{i} {}", text(&mut r, i).chars().take(40).collect::<String>()), &layers).unwrap());
        if r.below(2) == 0 {
            ed.model.add_slab_type(&format!("ST{i}"), &layers).unwrap();
        }
    }
    let mut walls = vec![];
    for i in 0..r.below(7) {
        let x = i as f64 * 5.0;
        let w = WallSpec {
            a: (x, 0.0),
            b: (x + 1.0 + r.below(5000) as f64 / 1000.0, r.below(3000) as f64 / 1000.0),
            level: levels[r.below(levels.len() as u64) as usize],
            base_offset: r.below(500) as f64 / 1000.0,
            height: 2.0 + r.below(3000) as f64 / 1001.0,
            wall_type: types[r.below(types.len() as u64) as usize],
            offset0: (r.below(2) == 0).then(|| r.below(100) as f64 / 1000.0),
        };
        walls.push(ed.model.add_wall(&w).unwrap());
    }
    let pinned: Vec<(&str, i64)> = walls.iter().filter(|_| r.below(3) == 0).map(|w| ("wall", *w)).collect();
    if !pinned.is_empty() {
        ed.model.pin(&pinned).unwrap();
    }
    // a non-finite number is written blank and read-only (after the walls: none stands on it)
    if r.below(3) == 0 {
        let id = ed.model.add_level(&format!("Inf {seed}"), 0.0).unwrap();
        ed.model.sql().execute("UPDATE level SET elevation=9e999 WHERE id=?1", [id]).unwrap();
    }
    (ed, dir)
}

fn seeded_case(seed: u64) {
    let (ed, dir) = random_editor(seed);
    let units = if seed % 2 == 0 { xlsx::Units::Metric } else { xlsx::Units::Imperial };
    let locale = if seed % 3 == 0 { "ru" } else { "en" };
    let book = dir.join("b.xlsx");
    let out = export_sync(&ed.model, &book, units, locale);
    assert!(matches!(out, Outcome::Done), "seed {seed}: first export {:?}", failed(&out));
    let notes = xlsx::take_export_notes();
    let left = differences(&ed.model, &book, &book, &[], &notes).unwrap();
    assert!(left.is_empty(), "seed {seed}: {:?}", left.first().map(|m| m.message()));
    let out = export_sync(&ed.model, &book, units, locale);
    assert!(matches!(out, Outcome::Done), "seed {seed}: re-export {:?}", failed(&out));
    assert!(!part_of(&book).exists(), "seed {seed}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn failed(o: &Outcome) -> Option<&str> {
    match o {
        Outcome::Failed(e) => Some(e.as_str()),
        _ => None,
    }
}

#[test]
fn seeded_random_models_read_back_clean() {
    for seed in [1, 2, 3, 5, 8, 13, 21, 42] {
        seeded_case(seed);
    }
}
