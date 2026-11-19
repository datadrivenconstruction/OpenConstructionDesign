// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A book without a whole baseline: the hidden comparison sheet was dropped by a tool that
//! re-saved the book, damaged, or never written (a hand-made book). No three-way compare is
//! possible then: every differing cell counts as a book edit and a missing row cannot be told
//! from one the book never had, so the import deletes nothing and flags `Report::no_base`.

use calamine::{Data, Range};

use super::baseline::Base;
use super::{CURTAINS, LAYERS, LEVELS, OPENINGS, ROOMS, SLABS, SLAB_LAYERS, TYPES, WALLS};

/// Element sheets every export records in the baseline whenever it writes them.
const CORE: [&str; 5] = [WALLS, OPENINGS, ROOMS, SLABS, CURTAINS];

/// Info row naming the data sheets the export wrote, so a later import knows the book's own set.
pub(crate) const SHEETS_ROW: &str = "sheets";

/// Data sheets every export writes, listed in the Info `sheets` row.
pub(crate) const WRITTEN: [&str; 9] =
    [LEVELS, TYPES, LAYERS, SLAB_LAYERS, WALLS, OPENINGS, ROOMS, SLABS, CURTAINS];

/// The export that wrote this book (its Info sheet) wrote `sheet`. A book older than the
/// `sheets` row tells by the rows it has: Curtains came before `revision`, SlabLayers after it.
pub(crate) fn wrote(info: &Range<Data>, sheet: &str) -> bool {
    let row = |key: &str| {
        info.rows()
            .find(|r| r.first().is_some_and(|d| d.to_string() == key))
            .map(|r| r.get(1).map(|d| d.to_string()).unwrap_or_default())
    };
    if let Some(list) = row(SHEETS_ROW) {
        return list.split(',').any(|s| s.trim() == sheet);
    }
    match sheet {
        CURTAINS => row("revision").is_some(),
        SLAB_LAYERS => false,
        _ => true,
    }
}

fn blank(d: &Data) -> bool {
    match d {
        Data::Empty => true,
        Data::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// The baseline sheet has the layout the export writes: blocks of `# | sheet | id | key ...`
/// each followed by rows with a whole-number id. Anything else means it was edited or cut.
pub(crate) fn intact(range: &Range<Data>) -> bool {
    let mut blocks = 0usize;
    for row in range.rows() {
        let first = row.first().unwrap_or(&Data::Empty);
        match first {
            Data::String(h) if h == "#" => {
                let name = row.get(1).map(|d| d.to_string()).unwrap_or_default();
                let key = row.get(2).map(|d| d.to_string()).unwrap_or_default();
                if name.trim().is_empty() || key != "id" {
                    return false;
                }
                blocks += 1;
            }
            Data::Float(f) if blocks > 0 && f.is_finite() && f.fract() == 0.0 => {}
            Data::Int(_) if blocks > 0 => {}
            d if blank(d) => {
                if !row.iter().all(blank) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    blocks > 0
}

/// Every element sheet of the book has its block: a baseline cut short counts as missing.
pub(crate) fn covers(base: &Base, sheets: &[String]) -> bool {
    CORE.iter().all(|&s| !sheets.iter().any(|n| n == s) || base.has_sheet(s))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use calamine::{open_workbook, Data, Reader, Xlsx};
    use ddc_model::{LayerSpec, Model, WallSpec};
    use rust_xlsxwriter::Workbook;

    use super::super::tests::{col, tmp};
    use super::super::{export_xlsx, import_xlsx_with, ImportOptions, Report, BASELINE, WALLS};

    type Grid = Vec<Vec<Data>>;

    fn model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.25 }])
            .unwrap();
        let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.)];
        for i in 0..4 {
            m.add_wall(&WallSpec {
                a: p[i],
                b: p[(i + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        }
        m
    }

    fn ids(m: &Model) -> Vec<i64> {
        let mut st = m.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    fn height(m: &Model, id: i64) -> f64 {
        m.sql().query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0)).unwrap()
    }

    /// Values-only copy of `src` (as another tool re-saves it): sheets `keep` refuses are dropped.
    fn copy(src: &Path, dst: &Path, keep: impl Fn(&str) -> bool, f: impl Fn(&str, &mut Grid)) {
        let mut wb: Xlsx<_> = open_workbook(src).unwrap();
        let mut out = Workbook::new();
        for name in wb.sheet_names() {
            if !keep(&name) {
                continue;
            }
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

    fn run(m: &mut Model, p: &Path, apply: bool, delete_missing: bool) -> Report {
        import_xlsx_with(m, p, ImportOptions { apply, delete_missing, new_params: false }).unwrap()
    }

    /// Grid row of the wall `id` on the Walls sheet.
    fn row_of(g: &Grid, id: i64) -> usize {
        let c = col(g, "id");
        g.iter()
            .position(|r| matches!(r.get(c), Some(Data::Float(x)) if *x as i64 == id))
            .unwrap()
    }

    fn set_height(g: &mut Grid, id: i64, v: f64) {
        let (r, c) = (row_of(g, id), col(g, "height [m]"));
        g[r][c] = Data::Float(v);
    }

    fn exported(m: &Model, tag: &str) -> PathBuf {
        let p = tmp(&format!("xs10_{tag}"));
        export_xlsx(m, &p).unwrap();
        p
    }

    #[test]
    fn book_with_its_baseline_has_no_flag() {
        let mut m = model();
        let p = exported(&m, "plain");
        let r = run(&mut m, &p, false, true);
        assert!(!r.no_base);
        assert!(r.changes.is_empty() && r.deletions.is_empty(), "{:?}", r.changes);
        // a value-only copy that keeps the sheet whole is still a book with a baseline
        let q = tmp("xs10_plain_copy");
        copy(&p, &q, |_| true, |_, _| {});
        assert!(!run(&mut m, &q, false, true).no_base);
    }

    #[test]
    fn dropped_baseline_is_flagged_and_changes_still_listed() {
        let mut m = model();
        let w = ids(&m)[0];
        let p = exported(&m, "drop_src");
        let q = tmp("xs10_drop_dst");
        copy(&p, &q, |n| n != BASELINE, |n, g| {
            if n == WALLS {
                set_height(g, w, 3.5);
            }
        });
        let r = run(&mut m, &q, false, false);
        assert!(r.no_base);
        assert_eq!(r.counts().changed, 1, "{:?}", r.changes);
        assert_eq!(height(&m, w), 3.0, "a preview changes nothing");
    }

    #[test]
    fn without_baseline_a_model_edit_since_the_export_is_not_detected() {
        let mut m = model();
        let w = ids(&m)[1];
        let p = exported(&m, "newer_src");
        let q = tmp("xs10_newer_dst");
        copy(&p, &q, |n| n != BASELINE, |_, _| {});
        m.sql().execute("UPDATE wall SET height=2.0 WHERE id=?1", [w]).unwrap();
        // with the baseline the model edit wins and nothing is offered
        let with = run(&mut m, &p, false, false);
        assert!(!with.no_base && with.changes.is_empty() && with.model_newer > 0, "{:?}", with.changes);
        // without it the stale book value looks like a user edit: hence the warning and the confirm
        let without = run(&mut m, &q, false, false);
        assert!(without.no_base && without.conflicts.is_empty());
        assert_eq!(without.counts().changed, 1, "{:?}", without.changes);
    }

    #[test]
    fn missing_rows_never_delete_without_baseline() {
        let mut m = model();
        let last = *ids(&m).last().unwrap();
        let p = exported(&m, "del_src");
        let cut = |n: &str, g: &mut Grid| {
            if n == WALLS {
                let r = row_of(g, last);
                g.remove(r);
            }
        };
        let with = tmp("xs10_del_with");
        copy(&p, &with, |_| true, &cut);
        assert_eq!(run(&mut m, &with, false, true).counts().deleted, 1, "with a baseline the row is a deletion");
        let q = tmp("xs10_del_dst");
        copy(&p, &q, |n| n != BASELINE, &cut);
        let r = run(&mut m, &q, true, true);
        assert!(r.no_base);
        assert_eq!(r.counts().deleted, 0, "{:?}", r.changes);
        assert!(r.deletions.is_empty());
        assert_eq!(ids(&m).len(), 4, "no element is deleted");
    }

    #[test]
    fn damaged_or_partial_baseline_counts_as_missing() {
        let mut m = model();
        let p = exported(&m, "dmg_src");
        type Cut = fn(&mut Grid);
        let cases: [(&str, Cut); 5] = [
            ("cleared", |g| g.clear()),
            ("no_headers", |g| g.retain(|r| r.first().map(|d| d.to_string()) != Some("#".into()))),
            ("bad_key", |g| {
                if let Some(r) = g.iter_mut().find(|r| r.first().map(|d| d.to_string()) == Some("#".into())) {
                    r[2] = Data::String("x".into());
                }
            }),
            ("junk_row", |g| g.insert(1, vec![Data::String("junk".into())])),
            ("walls_block_cut", |g| {
                let mut drop = false;
                g.retain(|r| {
                    if r.first().map(|d| d.to_string()) == Some("#".into()) {
                        drop = r.get(1).map(|d| d.to_string()) == Some(WALLS.into());
                    }
                    !drop
                });
            }),
        ];
        for (tag, f) in cases {
            let q = tmp(&format!("xs10_dmg_{tag}"));
            copy(&p, &q, |_| true, |n, g| {
                if n == BASELINE {
                    f(g);
                }
            });
            let r = run(&mut m, &q, false, true);
            assert!(r.no_base, "{tag}");
            assert!(r.deletions.is_empty() && r.counts().deleted == 0, "{tag}");
        }
    }
}
