//! Whitespace and line-ending noise in text cells (outer spaces, NBSP, CRLF vs LF) is never an
//! edit nor "model is newer"; a book without a baseline never renames on such noise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{CeilingSpec, Error, LayerSpec, Model, WallSpec};
use rust_xlsxwriter::Workbook;

use super::tests::{col, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

/// Caption of the `name` column in a Russian-language book (fixture).
const RU_NAME: &str = "\u{418}\u{43c}\u{44f}";
const NAME: &str = "name";

/// One level, wall type `type_name` (stored byte-exact) and two walls of it.
fn model(type_name: &str) -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = [LayerSpec { material: "brick".into(), thickness: 0.25 }];
    let t = m.add_wall_type("tmp", &layer).unwrap();
    rename_type(&m, t, type_name);
    for i in 0..2 {
        m.add_wall(&WallSpec {
            a: (0., i as f64 * 3.),
            b: (5., i as f64 * 3.),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    }
    (m, t)
}

fn rename_type(m: &Model, t: i64, name: &str) {
    m.sql()
        .execute("UPDATE wall_type SET name=?1 WHERE id=?2", name_id(name, t))
        .unwrap();
}

fn name_id(name: &str, id: i64) -> (String, i64) {
    (name.to_string(), id)
}

fn type_name(m: &Model, t: i64) -> String {
    m.sql()
        .query_row("SELECT name FROM wall_type WHERE id=?1", [t], |r| r.get(0))
        .unwrap()
}

fn walls(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
}

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    En,
    Ru,
}

impl Lang {
    fn name_head(self) -> &'static str {
        match self {
            Lang::En => NAME,
            Lang::Ru => RU_NAME,
        }
    }
}

/// Exports `m` (plain English book, or a professional one with a Russian `name` caption).
fn exported(m: &Model, tag: &str, lang: Lang) -> PathBuf {
    let p = tmp(&format!("xs15_{tag}_src"));
    match lang {
        Lang::En => export_xlsx(m, &p).unwrap(),
        Lang::Ru => {
            let caps: HashMap<String, String> = [("name".to_string(), RU_NAME.to_string())].into();
            export_xlsx_styled(m, &p, &p, Units::Metric, &[], &Style::professional(caps)).unwrap()
        }
    }
    p
}

/// Values-only copy of `src`; the Baseline sheet is dropped unless `base`.
fn copy(src: &Path, dst: &Path, base: bool, f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        if !base && name == BASELINE {
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

/// Grid row whose id cell (`id` or a caption `Id`) is `id`.
fn row_of(g: &Grid, id: i64) -> usize {
    let c = g[0].iter().position(|d| d.to_string().eq_ignore_ascii_case("id")).unwrap();
    (1..g.len())
        .find(|r| match g[*r].get(c) {
            Some(Data::Float(x)) => *x as i64 == id,
            Some(Data::Int(x)) => *x == id,
            _ => false,
        })
        .unwrap()
}

/// Book of `m` with the WallTypes name cell of `t` set to `cell` (None = left as exported).
fn book(m: &Model, tag: &str, lang: Lang, base: bool, t: i64, cell: Option<Data>) -> PathBuf {
    let p = exported(m, tag, lang);
    let q = tmp(&format!("xs15_{tag}_dst"));
    copy(&p, &q, base, |s, g| {
        if s == TYPES {
            let r = row_of(g, t);
            let c = col(g, lang.name_head());
            if let Some(v) = &cell {
                while g[r].len() <= c {
                    g[r].push(Data::Empty);
                }
                g[r][c] = v.clone();
            }
        }
    });
    q
}

fn run(m: &mut Model, p: &Path, apply: bool) -> Report {
    import_xlsx_with(m, p, ImportOptions { apply, delete_missing: false, new_params: false }).unwrap()
}

fn name_changes(rep: &Report) -> Vec<&Change> {
    rep.changes.iter().filter(|c| c.sheet == TYPES).collect()
}

/// The exported cell holds the model text as it is; noise around it is never an edit.
fn assert_noise_is_no_edit(model_name: &str, cell: Option<&str>, lang: Lang, base: bool, tag: &str) {
    let (mut m, t) = model(model_name);
    let q = book(&m, tag, lang, base, t, cell.map(|c| Data::String(c.into())));
    let pre = run(&mut m, &q, false);
    assert!(name_changes(&pre).is_empty(), "{tag}: {:?}", pre.changes);
    assert_eq!(pre.model_newer, 0, "{tag}");
    assert!(pre.conflicts.is_empty() && pre.rejected.is_empty(), "{tag}: {pre:?}");
    assert_eq!(pre.no_base, !base, "{tag}");
    let rep = run(&mut m, &q, true);
    assert!(name_changes(&rep).is_empty(), "{tag}: {:?}", rep.changes);
    assert_eq!(rep.model_newer, 0, "{tag}");
    assert_eq!(type_name(&m, t).as_bytes(), model_name.as_bytes(), "{tag}: name bytes kept");
    assert_eq!(walls(&m), 2, "{tag}: walls keep their type");
}

#[test]
fn same_text_rules() {
    use super::units::same_text;
    assert!(same_text("Wall 1 ", "Wall 1"));
    assert!(same_text("  Wall 1", "Wall 1\t"));
    assert!(same_text("a\r\nb", "a\nb"));
    assert!(same_text("a\rb", "a\nb"));
    assert!(same_text("Wall\u{a0}1", "Wall 1"));
    assert!(same_text("Wall\u{2009}1\u{202f}", "Wall 1"));
    assert!(same_text("", "   "));
    assert!(!same_text("Wall 1", "Wall  1"), "inner spacing is an edit");
    assert!(!same_text("Wall 1", "Wall 2"));
    assert!(!same_text("a\n\nb", "a\nb"));
    assert!(!same_text("", "x"));
}

#[test]
fn trailing_space_in_model_untouched_cell_is_no_edit() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        assert_noise_is_no_edit("Wall 1 ", None, lang, true, &format!("trail_{l}"));
        assert_noise_is_no_edit("Wall 1 ", Some("Wall 1"), lang, true, &format!("trim_{l}"));
        assert_noise_is_no_edit("  Wall  1 ", Some("Wall  1"), lang, true, &format!("lead_{l}"));
    }
}

#[test]
fn without_baseline_noise_never_renames() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        assert_noise_is_no_edit("Wall 1 ", None, lang, false, &format!("nb_trail_{l}"));
        assert_noise_is_no_edit("Wall 1 ", Some("Wall 1"), lang, false, &format!("nb_trim_{l}"));
        assert_noise_is_no_edit("Wall\u{a0}1", Some("Wall 1"), lang, false, &format!("nb_nbsp_{l}"));
    }
}

#[test]
fn crlf_vs_lf_is_no_edit() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        for base in [true, false] {
            let tag = format!("crlf_{l}_{base}");
            assert_noise_is_no_edit("a\r\nb", Some("a\nb"), lang, base, &tag);
            let tag = format!("lf_{l}_{base}");
            assert_noise_is_no_edit("a\nb", Some("a\r\nb "), lang, base, &tag);
        }
    }
}

#[test]
fn nbsp_and_thin_space_are_no_edit() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        assert_noise_is_no_edit("Wall 1", Some("Wall\u{a0}1\u{a0}"), lang, true, &format!("nbsp_{l}"));
        assert_noise_is_no_edit("Wall 1", Some("\u{2009}Wall 1"), lang, true, &format!("thin_{l}"));
    }
}

#[test]
fn real_edit_applies_once() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        for base in [true, false] {
            let (mut m, t) = model("Wall 1 ");
            let tag = format!("real_{l}_{base}");
            let q = book(&m, &tag, lang, base, t, Some(Data::String("Wall 2 ".into())));
            let rep = run(&mut m, &q, true);
            let ch = name_changes(&rep);
            assert_eq!(ch.len(), 1, "{tag}: {:?}", rep.changes);
            assert_eq!(ch[0].new, "Wall 2", "{tag}");
            assert_eq!(type_name(&m, t), "Wall 2", "{tag}: the book text as typed, trimmed");
            assert_eq!(walls(&m), 2, "{tag}");
            // the next round-trip sees nothing new
            let again = book(&m, &format!("{tag}_again"), lang, base, t, None);
            let rep = run(&mut m, &again, true);
            assert!(name_changes(&rep).is_empty(), "{tag}: {:?}", rep.changes);
            assert_eq!(rep.model_newer, 0, "{tag}");
        }
    }
}

#[test]
fn inner_whitespace_change_is_an_edit() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        let (mut m, t) = model("Wall 1");
        let tag = format!("inner_{l}");
        let q = book(&m, &tag, lang, true, t, Some(Data::String("Wall  1".into())));
        let rep = run(&mut m, &q, true);
        assert_eq!(name_changes(&rep).len(), 1, "{tag}: {:?}", rep.changes);
        assert_eq!(type_name(&m, t), "Wall  1", "{tag}");
    }
}

#[test]
fn whitespace_only_cell_is_blank_like_an_empty_one() {
    for (lang, l) in [(Lang::En, "en"), (Lang::Ru, "ru")] {
        for (cell, what) in [(Data::Empty, "empty"), (Data::String("  \u{a0} ".into()), "spaces")] {
            let (mut m, t) = model("Wall 1 ");
            let tag = format!("blank_{what}_{l}");
            let q = book(&m, &tag, lang, true, t, Some(cell));
            let rep = run(&mut m, &q, true);
            assert!(name_changes(&rep).is_empty(), "{tag}: {:?}", rep.changes);
            assert!(rep.rejected.iter().any(|r| r.sheet == TYPES), "{tag}: {:?}", rep.rejected);
            assert_eq!(type_name(&m, t), "Wall 1 ", "{tag}: never renamed to empty");
        }
    }
}

#[test]
fn later_model_rename_still_counts_as_model_newer() {
    let (mut m, t) = model("Wall 1 ");
    let q = book(&m, "newer", Lang::En, true, t, None);
    rename_type(&m, t, "Wall X");
    let rep = run(&mut m, &q, true);
    assert!(name_changes(&rep).is_empty(), "{:?}", rep.changes);
    // the type row and the walls naming it each see the newer model name
    assert!(rep.model_newer >= 1, "{rep:?}");
    assert_eq!(type_name(&m, t), "Wall X");
}

// ---- family sheet: type name and read-only text columns ----

const CEIL: &str = "Ceilings";
const TYPE_HEAD: &str = "Slab type (type id)";
const LEVEL_HEAD: &str = "level (read-only)";
const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

fn get_t(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.sql()
        .query_row("SELECT stype FROM ceiling WHERE id=?1", [id], |r| r.get::<_, i64>(0))? as f64)
}

fn set_t(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let (level, height): (i64, f64) = m.sql().query_row(
        "SELECT level,height FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let s = CeilingSpec { level, slab_type: v as i64, height, outer: RING.to_vec(), holes: vec![] };
    m.update_ceiling(id, &s)
}

fn fams() -> Vec<FamilySheet> {
    vec![FamilySheet {
        sheet: CEIL,
        table: "ceiling",
        cols: vec![FamilyCol {
            key: "stype",
            label: "Slab type".into(),
            dim: FamDim::Type("slab_type"),
            labels: vec![],
            lo: 0.0,
            hi: 0.0,
            get: get_t,
            set: set_t,
        }],
        delete: RowDelete::Allowed,
    }]
}

/// Level `level` and slab type `stype` (both byte-exact), one ceiling of that type.
fn fam_model(level: &str, stype: &str) -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let st = m
        .add_slab_type("tmp", &[LayerSpec { material: "plaster".into(), thickness: 0.02 }])
        .unwrap();
    m.sql().execute("UPDATE level SET name=?1 WHERE id=?2", name_id(level, lv)).unwrap();
    m.sql().execute("UPDATE slab_type SET name=?1 WHERE id=?2", name_id(stype, st)).unwrap();
    let id = m
        .add_ceiling(&CeilingSpec { level: lv, slab_type: st, height: 2.7, outer: RING.to_vec(), holes: vec![] })
        .unwrap();
    (m, id, st)
}

fn fam_run(m: &mut Model, tag: &str, base: bool, cells: &[(&str, &str)]) -> Report {
    let p = tmp(&format!("xs15_{tag}_src"));
    export_xlsx_families(m, &p, Units::Metric, &fams()).unwrap();
    let q = tmp(&format!("xs15_{tag}_dst"));
    copy(&p, &q, base, |s, g| {
        if s == CEIL {
            for (head, v) in cells {
                let c = col(g, head);
                while g[1].len() <= c {
                    g[1].push(Data::Empty);
                }
                g[1][c] = Data::String((*v).into());
            }
        }
    });
    import_xlsx_families(m, &q, ImportOptions { apply: true, delete_missing: false, new_params: false }, &fams())
        .unwrap()
}

#[test]
fn family_type_name_with_trailing_space_is_no_change() {
    for base in [true, false] {
        for cell in ["ceiling", "ceiling\u{a0}", " ceiling  "] {
            let (mut m, id, st) = fam_model("L1", "ceiling ");
            let tag = format!("fam_type_{base}_{}", cell.len());
            let rep = fam_run(&mut m, &tag, base, &[(TYPE_HEAD, cell)]);
            assert!(rep.changes.is_empty() && rep.props.is_empty(), "{tag}: {rep:?}");
            assert!(rep.rejected.is_empty(), "{tag}: {:?}", rep.rejected);
            assert_eq!(get_t(&m, id).unwrap(), st as f64, "{tag}");
        }
    }
}

#[test]
fn family_read_only_text_ignores_line_endings_and_spaces() {
    for (model_level, cell) in [("Floor\r\n1", "Floor\n1"), ("L1 ", "L1"), ("L\u{a0}1", "L 1")] {
        let (mut m, _, _) = fam_model(model_level, "ceiling");
        let tag = format!("fam_ro_{}", cell.len());
        let rep = fam_run(&mut m, &tag, true, &[(LEVEL_HEAD, cell)]);
        assert!(!rep.ignored.iter().any(|i| i.column.contains("read-only")), "{tag}: {:?}", rep.ignored);
        assert!(rep.changes.is_empty() && rep.props.is_empty(), "{tag}: {rep:?}");
    }
    // a real difference in a read-only column is still reported
    let (mut m, _, _) = fam_model("L1", "ceiling");
    let rep = fam_run(&mut m, "fam_ro_real", true, &[(LEVEL_HEAD, "L 2")]);
    assert!(rep.ignored.iter().any(|i| i.column.contains("read-only")), "{:?}", rep.ignored);
}
