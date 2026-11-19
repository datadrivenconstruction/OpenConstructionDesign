// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Text cells a spreadsheet coerced into numbers or dates (`01` -> 1, a 19-digit code ->
//! 1.2e18, `1-2` -> a date) never rename elements or change text parameters; a real numeric
//! edit still applies; the export writes such text as text.

use calamine::{Data, ExcelDateTime, ExcelDateTimeType};
use ddc_model::{
    CeilingSpec, Error, LayerSpec, Model, PVal, ParamSet, ParamSpec, ParamType, RoomSpec, WallSpec,
};

use super::tests::{edit, tmp};
use super::units::{num_cell_text, num_text, Dec, NumText};
use super::*;

type Grid = Vec<Vec<Data>>;

fn f(x: f64) -> Data {
    Data::Float(x)
}

#[test]
fn num_text_cases() {
    let same = |m: &str, d: Data, dec| assert_eq!(num_text(&d, m, dec), NumText::Same, "{m} {d:?}");
    same("01", f(1.0), None);
    same("007", Data::Int(7), None);
    same("00123", f(123.0), Some(Dec::Point));
    same("1.10", f(1.1), Some(Dec::Point));
    same("1,10", f(1.1), Some(Dec::Comma));
    same("1E3", f(1000.0), None);
    same("12", f(12.0), None);
    same("12345678901234", f(12345678901234.0), None);
    let edit_to = |m: &str, d: Data, dec, want: &str| {
        assert_eq!(num_text(&d, m, dec), NumText::Edit(want.into()), "{m} {d:?}")
    };
    edit_to("101", f(102.0), None, "102");
    edit_to("A-7", f(7.0), None, "7");
    edit_to("", f(5.5), None, "5.5");
    // in a comma book `1,5` is 1.5: the number 15 is a real edit
    edit_to("1,5", f(15.0), Some(Dec::Comma), "15");
    edit_to("1.10", f(1.2), Some(Dec::Point), "1.2");
    // a point book never drops a comma as a group unless the groups have three digits
    edit_to("1,5", f(15.0), Some(Dec::Point), "15");
    edit_to("1,5", f(15.0), None, "15");
    edit_to("1,50", f(150.0), None, "150");
    edit_to("1,5", f(1.5), Some(Dec::Point), "1.5");
    edit_to("1.5", f(15.0), Some(Dec::Comma), "15");
    edit_to("1,000", f(1.0), Some(Dec::Point), "1");
    same("1,000", f(1000.0), Some(Dec::Point));
    same("1,000.5", f(1000.5), Some(Dec::Point));
    same("1.000", f(1000.0), Some(Dec::Comma));
    same("1\u{a0}000", f(1000.0), None);
    same("1,5", f(1.5), None);
    same("+7", f(7.0), None);
    edit_to("1,0000", f(10000.0), Some(Dec::Point), "10000");
    let lossy = |m: &str, d: Data| assert_eq!(num_text(&d, m, None), NumText::Lossy, "{m} {d:?}");
    lossy("1234567890123456789", f(1.2345678901234568e18));
    lossy("1234567890123456", f(1234567890123456.0));
    lossy("", f(1.2345678901234568e18));
    lossy("ABC", f(1.2345678901234568e18));
}

#[test]
fn reasons_have_en_and_ru_texts() {
    type Loc = std::collections::BTreeMap<String, String>;
    let load = |a: &str, b: &str| -> Loc {
        let mut m: Loc = serde_json::from_str(a).unwrap();
        m.extend(serde_json::from_str::<Loc>(b).unwrap());
        m
    };
    let en = load(include_str!("../../../ui/data/i18n/en/xl.json"), include_str!("../../../ui/data/i18n/en/xl_convert.json"));
    let ru = load(include_str!("../../../ui/data/i18n/ru/xl.json"), include_str!("../../../ui/data/i18n/ru/xl_convert.json"));
    for (key, tpl) in [super::msg::NUM_SAME, super::msg::NUM_LOSSY] {
        assert_eq!(en.get(key).map(String::as_str), Some(tpl), "{key}");
        assert!(ru[key].chars().any(|c| (0x400..=0x4FF).contains(&(c as u32))), "{key}");
    }
}

#[test]
fn num_cell_text_is_plain() {
    assert_eq!(num_cell_text(102.0), "102");
    assert_eq!(num_cell_text(0.1 + 0.2), "0.3");
    assert_eq!(num_cell_text(-0.0), "0");
    assert_eq!(num_cell_text(1.5), "1.5");
    assert_eq!(num_cell_text(-7.0), "-7");
}

fn col_of(g: &Grid, key: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string().trim().eq_ignore_ascii_case(key))
        .unwrap_or_else(|| panic!("no column {key}: {:?}", g[0]))
}

fn row_of(g: &Grid, id: i64) -> usize {
    let c = col_of(g, "id");
    (1..g.len())
        .find(|r| match g[*r].get(c) {
            Some(Data::Float(x)) => *x == id as f64,
            Some(Data::Int(x)) => *x == id,
            _ => false,
        })
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn put(g: &mut Grid, r: usize, key: &str, v: Data) {
    let c = col_of(g, key);
    while g[r].len() <= c {
        g[r].push(Data::Empty);
    }
    g[r][c] = v;
}

fn ops(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0)).unwrap()
}

fn room_text(m: &Model, id: i64) -> (String, String) {
    m.sql()
        .query_row("SELECT name, number FROM room WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
}

fn rooms(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM room", [], |r| r.get(0)).unwrap()
}

/// Four walls on L1 and one room `name` / `number` inside.
fn room_model(name: &str, number: &str) -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.3 }])
        .unwrap();
    let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.)];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[(i + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    let spec = RoomSpec { level: lv, name: name.into(), number: number.into(), point: (2.5, 2.0), height: 3.0 };
    let id = m.add_room(&spec).unwrap();
    (m, id)
}

/// Exports `m` as an en (point) or ru (comma) book, sets room cells, previews, then applies;
/// the preview report must equal the applied one.
fn room_edit(m: &mut Model, id: i64, tag: &str, comma: bool, cells: &[(&str, Data)]) -> Report {
    let p = tmp(&format!("nt_{tag}_src"));
    let st = Style::default().with_number_format(comma, "");
    export_xlsx_styled(m, &p, &p, Units::Metric, &[], &st).unwrap();
    let q = tmp(&format!("nt_{tag}_dst"));
    edit(&p, &q, |s, g| {
        if s == ROOMS {
            let r = row_of(g, id);
            for (k, v) in cells {
                put(g, r, k, v.clone());
            }
        }
    });
    let n = ops(m);
    let o = ImportOptions { apply: false, delete_missing: false, new_params: false };
    let pre = import_xlsx_with(m, &q, o).unwrap();
    assert_eq!(ops(m), n, "{tag}: the preview changes nothing");
    let rep = import_xlsx(m, &q).unwrap();
    let key = |r: &Report| format!("{:?} {:?} {:?} {}", r.changes, r.ignored, r.rejected, r.model_newer);
    assert_eq!(key(&pre), key(&rep), "{tag}: preview == apply");
    rep
}

/// The cell is no edit: model text byte-exact, a note naming the cell, no op, no model_newer.
fn assert_kept(m: &Model, id: i64, rep: &Report, n: i64, want: (&str, &str), tag: &str) {
    assert_eq!(room_text(m, id), (want.0.to_string(), want.1.to_string()), "{tag}");
    assert!(rep.changes.is_empty() && rep.rejected.is_empty(), "{tag}: {rep:?}");
    assert_eq!(rep.model_newer, 0, "{tag}");
    assert_eq!(ops(m), n, "{tag}");
    assert_eq!(rep.ignored.len(), 1, "{tag}: {rep:?}");
    let i = &rep.ignored[0];
    assert!(i.sheet == ROOMS && i.row > 1 && i.col > 0, "{tag}: {i:?}");
    assert_eq!(i.reason, super::msg::NUM_SAME.1, "{tag}");
}

#[test]
fn leading_zero_room_number_is_not_renamed() {
    for comma in [false, true] {
        let (mut m, id) = room_model("Hall", "01");
        let n = ops(&m);
        let rep = room_edit(&mut m, id, &format!("lz{comma}"), comma, &[("number", f(1.0))]);
        assert_kept(&m, id, &rep, n, ("Hall", "01"), "01");
    }
}

#[test]
fn trailing_zero_mark_is_not_renamed_in_en_and_ru_books() {
    for (comma, number) in [(false, "1.10"), (true, "1,10")] {
        let (mut m, id) = room_model("Hall", number);
        let n = ops(&m);
        let rep = room_edit(&mut m, id, &format!("tz{comma}"), comma, &[("number", f(1.1))]);
        assert_kept(&m, id, &rep, n, ("Hall", number), number);
    }
}

#[test]
fn leading_zero_room_name_is_not_renamed() {
    let (mut m, id) = room_model("007", "101");
    let n = ops(&m);
    let rep = room_edit(&mut m, id, "nm007", false, &[("name", Data::Int(7))]);
    assert_kept(&m, id, &rep, n, ("007", "101"), "007");
}

#[test]
fn a_comma_code_retyped_as_a_number_in_a_point_book_is_a_real_edit() {
    // `1,5` in an en book is no thousands form of 15: the user's 15 must apply
    let (mut m, id) = room_model("Hall", "1,5");
    let n = ops(&m);
    let rep = room_edit(&mut m, id, "cp15", false, &[("number", f(15.0))]);
    assert!(rep.rejected.is_empty() && rep.ignored.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(room_text(&m, id).1, "15");
    assert_eq!(ops(&m), n + 1);
}

#[test]
fn real_numeric_edit_in_a_text_column_applies() {
    for comma in [false, true] {
        let (mut m, id) = room_model("Hall", "101");
        let n = ops(&m);
        let rep = room_edit(&mut m, id, &format!("re{comma}"), comma, &[("number", f(102.0))]);
        assert!(rep.applied && rep.rejected.is_empty() && rep.ignored.is_empty(), "{rep:?}");
        assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
        assert_eq!(rep.changes[0].new, "102");
        assert_eq!(room_text(&m, id).1, "102", "never 102.0");
        assert_eq!(ops(&m), n + 1, "one undo step");
    }
}

#[test]
fn long_code_turned_into_a_number_is_refused() {
    let code = "1234567890123456789";
    let (mut m, id) = room_model(code, "101");
    let n = ops(&m);
    let rep = room_edit(&mut m, id, "long", false, &[("name", f(1.2345678901234568e18))]);
    assert_eq!(room_text(&m, id), (code.to_string(), "101".to_string()));
    assert!(rep.changes.is_empty() && rep.ignored.is_empty(), "{rep:?}");
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    let rj = &rep.rejected[0];
    assert!(rj.sheet == ROOMS && rj.row > 1 && rj.col > 0, "{rj:?}");
    assert_eq!(rj.reason, super::msg::NUM_LOSSY.1);
    assert_eq!(ops(&m), n);
}

#[test]
fn long_number_in_a_text_cell_of_a_code_is_refused() {
    // the model has some other text: a 16+ digit number cannot be a reliable new code
    let (mut m, id) = room_model("Hall", "A-1");
    let n = ops(&m);
    let rep = room_edit(&mut m, id, "long2", false, &[("number", f(1.2345678901234568e18))]);
    assert_eq!(room_text(&m, id).1, "A-1");
    assert!(rep.changes.is_empty(), "{rep:?}");
    assert!(rep.rejected.iter().any(|r| r.reason == super::msg::NUM_LOSSY.1), "{rep:?}");
    assert_eq!(ops(&m), n);
}

#[test]
fn long_number_in_a_new_row_is_refused() {
    let (mut m, id) = room_model("Hall", "101");
    let p = tmp("nt_newrow_src");
    export_xlsx(&m, &p).unwrap();
    let q = tmp("nt_newrow_dst");
    edit(&p, &q, |s, g| {
        if s == ROOMS {
            let mut row = g[row_of(g, id)].clone();
            row[col_of(g, "id")] = Data::Empty;
            row[col_of(g, "number")] = f(1.2345678901234568e18);
            g.push(row);
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.rejected.iter().any(|r| r.reason == super::msg::NUM_LOSSY.1), "{rep:?}");
    assert_eq!(rooms(&m), 1, "no room with a broken number");
}

#[test]
fn date_in_a_text_column_never_renames_and_the_row_still_applies() {
    let (mut m, id) = room_model("Hall", "1-2");
    let n = ops(&m);
    let date = Data::DateTime(ExcelDateTime::new(46024.0, ExcelDateTimeType::DateTime, false));
    let rep = room_edit(&mut m, id, "date", false, &[("number", date), ("name", Data::String("Lobby".into()))]);
    assert_eq!(room_text(&m, id), ("Lobby".to_string(), "1-2".to_string()));
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    assert!(rep.rejected[0].reason.contains("into a date"), "{:?}", rep.rejected);
    assert_eq!(rep.changes.len(), 1, "only the name: {:?}", rep.changes);
    assert_eq!(ops(&m), n + 1);
}

#[test]
fn export_writes_numeric_looking_text_as_text() {
    let (m, id) = room_model("007", "01");
    let p = tmp("nt_export");
    export_xlsx(&m, &p).unwrap();
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&p).unwrap();
    let g: Grid = calamine::Reader::worksheet_range(&mut wb, ROOMS)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect();
    let r = row_of(&g, id);
    assert_eq!(g[r][col_of(&g, "number")], Data::String("01".into()));
    assert_eq!(g[r][col_of(&g, "name")], Data::String("007".into()));
}

fn spec(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type: true,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["slab".into()],
    }
}

#[test]
fn type_mark_text_param_coerced_is_kept_and_a_real_edit_applies() {
    let (mut m, _) = room_model("Hall", "101");
    let st = m.add_slab_type("deck", &[LayerSpec { material: "concrete".into(), thickness: 0.2 }]).unwrap();
    let mark = m.add_param(&spec("Mark")).unwrap();
    let set_mark = |m: &mut Model, t: &str| {
        let ps = ParamSet { param: mark, kind: "slab_type".into(), id: st, value: Some(PVal::Text(t.into())) };
        m.set_param_values(&[ps]).unwrap();
    };
    set_mark(&mut m, "007");
    let book = |m: &Model, tag: &str, v: Data| {
        let p = tmp(&format!("nt_mark_{tag}_src"));
        export_xlsx(m, &p).unwrap();
        let q = tmp(&format!("nt_mark_{tag}_dst"));
        edit(&p, &q, |s, g| {
            if s == SLAB_TYPES {
                let r = row_of(g, st);
                put(g, r, "Mark", v.clone());
            }
        });
        q
    };
    let mark_of = |m: &Model| m.param_value(mark, "slab_type", st).unwrap();
    let q = book(&m, "kept", f(7.0));
    let n = ops(&m);
    let o = ImportOptions { apply: false, delete_missing: false, new_params: false };
    let pre = import_xlsx_with(&mut m, &q, o).unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert_eq!(format!("{:?}", pre.ignored), format!("{:?}", rep.ignored), "preview == apply");
    assert!(rep.changes.is_empty() && rep.rejected.is_empty() && rep.model_newer == 0, "{rep:?}");
    assert!(rep.ignored.iter().any(|i| i.reason == super::msg::NUM_SAME.1), "{rep:?}");
    assert_eq!(mark_of(&m), Some(PVal::Text("007".into())));
    assert_eq!(ops(&m), n);

    set_mark(&mut m, "7");
    let q = book(&m, "real", f(8.0));
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.rejected.is_empty() && rep.changes.len() == 1, "{rep:?}");
    assert_eq!(mark_of(&m), Some(PVal::Text("8".into())), "never 8.0");
}

fn get_t(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.sql().query_row("SELECT stype FROM ceiling WHERE id=?1", [id], |r| r.get::<_, i64>(0))? as f64)
}

const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

fn set_t(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let (level, height): (i64, f64) =
        m.sql().query_row("SELECT level,height FROM ceiling WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let s = CeilingSpec { level, slab_type: v as i64, height, outer: RING.to_vec(), holes: vec![] };
    m.update_ceiling(id, &s)
}

const FAM_SHEET: &str = "Ceilings";

fn fams() -> Vec<FamilySheet> {
    vec![FamilySheet {
        sheet: FAM_SHEET,
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

#[test]
fn family_type_named_like_a_number_is_not_read_as_an_id() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = || [LayerSpec { material: "plaster".into(), thickness: 0.02 }];
    // types up to id 7 exist, so a bare 7 would name another type
    let mut i = 0;
    while m.add_slab_type(&format!("t{i}"), &layer()).unwrap() < 7 {
        i += 1;
    }
    let t007 = m.add_slab_type("007", &layer()).unwrap();
    let spec = CeilingSpec { level: lv, slab_type: t007, height: 2.7, outer: RING.to_vec(), holes: vec![] };
    let c = m.add_ceiling(&spec).unwrap();
    let p = tmp("nt_fam_src");
    export_xlsx_families(&m, &p, Units::Metric, &fams()).unwrap();
    let q = tmp("nt_fam_dst");
    let head = "Slab type (type id)";
    edit(&p, &q, |s, g| {
        if s == FAM_SHEET {
            let r = row_of(g, c);
            assert_eq!(g[r][col_of(g, head)], Data::String("007".into()), "exported as text");
            put(g, r, head, f(7.0));
        }
    });
    let n = ops(&m);
    let o = |apply| ImportOptions { apply, delete_missing: false, new_params: false };
    let pre = import_xlsx_families(&mut m, &q, o(false), &fams()).unwrap();
    let rep = import_xlsx_families(&mut m, &q, o(true), &fams()).unwrap();
    assert_eq!(format!("{:?}", pre.ignored), format!("{:?}", rep.ignored), "preview == apply");
    assert!(rep.rejected.is_empty() && rep.props.is_empty(), "{rep:?}");
    assert!(rep.ignored.iter().any(|i| i.reason == super::msg::NUM_SAME.1), "{rep:?}");
    assert_eq!(get_t(&m, c).unwrap(), t007 as f64);
    assert_eq!(ops(&m), n);
}
