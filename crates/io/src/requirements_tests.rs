// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use ddc_model::checks::ValueRule;

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_req_{tag}_{}.xlsx", std::process::id()))
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

const EXAMPLE: [[&str; 9]; 4] = [
    ["Windows", "", "", "", "FireRating", "yes", "one_of", "EI30; EI60", ""],
    ["Doors", "L1", "", "", "width", "yes", "range", "800..1200", "mm"],
    ["Walls", "", "W200", "", "Code", "no", "regex", "W-\\d{3}", ""],
    ["Rooms", "", "", "New construction", "Name", "yes", "", "", ""],
];

fn english() -> (TemplateText, Vocab) {
    let headers = s(&[
        "Category", "Level", "Type", "Phase", "Parameter", "Required", "Rule", "Value", "Unit",
    ]);
    let t = TemplateText {
        sheet_req: "Requirements".into(),
        sheet_example: "Example".into(),
        sheet_help: "Instructions".into(),
        headers: headers.clone(),
        example: EXAMPLE.iter().map(|r| s(r)).collect(),
        help: s(&["How to fill in", "Category\tdoor, window, wall", "Passing is not compliance"]),
        kinds: s(&["non_empty", "one_of", "range", "regex", "unit"]),
        yes_no: s(&["yes", "no"]),
        cats: s(&["door", "window", "wall", "room"]),
    };
    let v = Vocab {
        headers: ReqField::ALL.into_iter().zip(headers).collect(),
        words: vec![],
        sheets: vec!["Requirements".into()],
        skip: vec!["Example".into(), "Instructions".into()],
    };
    (t, v)
}

fn parsed(r: &[&str; 9]) -> Requirement {
    let t = ReqText {
        cat: r[0].into(),
        level: r[1].into(),
        type_name: r[2].into(),
        phase: r[3].into(),
        param: r[4].into(),
        required: r[5].into(),
        kind: r[6].into(),
        value: r[7].into(),
        unit: r[8].into(),
    };
    parse_requirement(&t).unwrap()
}

#[test]
fn the_template_round_trips_its_example() {
    let (t, v) = english();
    let p = tmp("tpl_en");
    write_template(&p, &t).unwrap();
    assert_eq!(sheet_names(&p).unwrap(), s(&["Requirements", "Example", "Instructions"]));
    let empty = read_requirements(&p, &v, None).unwrap();
    let shape = (empty.sheet.as_str(), empty.rows.len(), empty.errors.len());
    assert_eq!(shape, ("Requirements", 0, 0));
    let ex = read_requirements(&p, &v, Some("Example")).unwrap();
    assert!(ex.errors.is_empty(), "{:?}", ex.errors);
    let got: Vec<Requirement> = ex.rows.iter().map(|r| r.req.clone()).collect();
    let want: Vec<Requirement> = EXAMPLE.iter().map(parsed).collect();
    assert_eq!(got, want);
    assert_eq!(ex.rows[1].origin(), "Example!3");
    let ValueRule::Range { min, max } = &got[1].rule else {
        panic!("{:?}", got[1].rule)
    };
    assert!((min.unwrap() - 0.8).abs() < 1e-12 && (max.unwrap() - 1.2).abs() < 1e-12);
    // the first sheet with the right columns is read when none is named so
    let other = Vocab { sheets: vec![], skip: vec![], ..v.clone() };
    assert_eq!(read_requirements(&p, &other, None).unwrap().sheet, "Requirements");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn a_russian_book_reads_through_its_words() {
    // Russian headers (Category ... Unit); values (Windows, yes); sheet name (Requirements)
    let ru = |x: &str| x.to_string();
    let heads = [
        ru("\u{41a}\u{430}\u{442}\u{435}\u{433}\u{43e}\u{440}\u{438}\u{44f}"),
        ru("\u{423}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}"),
        ru("\u{422}\u{438}\u{43f}"),
        ru("\u{421}\u{442}\u{430}\u{434}\u{438}\u{44f}"),
        ru("\u{41f}\u{430}\u{440}\u{430}\u{43c}\u{435}\u{442}\u{440}"),
        ru("\u{41e}\u{431}\u{44f}\u{437}\u{430}\u{442}\u{435}\u{43b}\u{44c}\u{43d}\u{43e}"),
        ru("\u{41f}\u{440}\u{430}\u{432}\u{438}\u{43b}\u{43e}"),
        ru("\u{417}\u{43d}\u{430}\u{447}\u{435}\u{43d}\u{438}\u{435}"),
        ru("\u{415}\u{434}\u{438}\u{43d}\u{438}\u{446}\u{430}"),
    ];
    let (windows, yes, list) = (
        "\u{41e}\u{43a}\u{43d}\u{430}",
        "\u{434}\u{430}",
        "\u{441}\u{43f}\u{438}\u{441}\u{43e}\u{43a}",
    );
    let sheet = "\u{422}\u{440}\u{435}\u{431}\u{43e}\u{432}\u{430}\u{43d}\u{438}\u{44f}";
    let row = s(&[windows, "", "", "", "FireRating", yes, list, "EI30; EI60", ""]);
    let t = TemplateText {
        sheet_req: sheet.into(),
        sheet_example: "Ex".into(),
        sheet_help: "Help".into(),
        headers: heads.to_vec(),
        example: vec![row.clone()],
        help: vec![],
        kinds: vec![],
        yes_no: vec![],
        cats: vec![],
    };
    let v = Vocab {
        headers: ReqField::ALL.into_iter().zip(heads.iter().cloned()).collect(),
        words: vec![
            ("\u{43e}\u{43a}\u{43d}\u{430}".into(), "window".into()),
            (yes.into(), "yes".into()),
            (list.into(), "one_of".into()),
        ],
        sheets: vec![sheet.into()],
        skip: vec!["Ex".into()],
    };
    let p = tmp("tpl_ru");
    write_template(&p, &t).unwrap();
    let got = read_requirements(&p, &v, Some("Ex")).unwrap();
    assert!(got.errors.is_empty(), "{:?}", got.errors);
    let r = &got.rows[0].req;
    assert_eq!((r.cat, r.required), ("window", true));
    assert_eq!(r.rule, ValueRule::OneOf(s(&["EI30", "EI60"])));
    assert_eq!(read_requirements(&p, &v, None).unwrap().sheet, sheet);
    let _ = std::fs::remove_file(&p);
}

/// A title above the header, columns in another order, typed numbers and a ticked box.
fn book_with_errors(p: &Path) {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Info").unwrap();
    ws.write_string(0, 0, "notes only").unwrap();
    let ws = wb.add_worksheet();
    ws.set_name("Req").unwrap();
    ws.write_string(0, 0, "Project information requirements").unwrap();
    for (j, h) in ["Parameter", "Category", "Rule", "Value", "Unit", "Required (yes/no)", "Level"]
        .iter()
        .enumerate()
    {
        ws.write_string(1, j as u16, *h).unwrap();
    }
    let rows: [[&str; 7]; 6] = [
        ["FireRating", "window", "", "", "", "yes", ""],
        ["FireRating", "spaceship", "", "", "", "yes", ""],
        ["", "", "", "", "", "", ""],
        ["Width", "door", "range", "9..1", "", "", ""],
        ["Code", "wall", "regex", "W-(", "", "no", ""],
        ["", "door", "", "", "", "yes", ""],
    ];
    for (i, r) in rows.iter().enumerate() {
        for (j, c) in r.iter().enumerate() {
            if !c.is_empty() {
                ws.write_string(2 + i as u32, j as u16, *c).unwrap();
            }
        }
    }
    ws.write_number(8, 0, 120.0).unwrap();
    ws.write_string(8, 1, "wall").unwrap();
    ws.write_boolean(8, 5, false).unwrap();
    ws.write_string(9, 0, "Mark").unwrap();
    ws.write_string(9, 1, "wall").unwrap();
    ws.write_string(9, 5, "maybe").unwrap();
    wb.save(p).unwrap();
}

#[test]
fn every_refused_cell_names_sheet_row_and_column() {
    let (_, v) = english();
    let p = tmp("errors");
    book_with_errors(&p);
    let got = read_requirements(&p, &v, None).unwrap();
    assert_eq!(got.sheet, "Req");
    let at: Vec<(usize, &str, ReqField, &str)> = got
        .errors
        .iter()
        .map(|e| (e.row, e.col.as_str(), e.field, e.code))
        .collect();
    assert_eq!(
        at,
        vec![
            (4, "B", ReqField::Category, "req.err.category"),
            (6, "D", ReqField::Value, "req.err.range_order"),
            (7, "D", ReqField::Value, "req.err.pattern"),
            (8, "A", ReqField::Parameter, "req.err.param"),
            (10, "F", ReqField::Required, "req.err.required"),
        ]
    );
    assert!(got.errors.iter().all(|e| e.sheet == "Req"));
    assert_eq!(got.errors[0].arg, "spaceship");
    assert_eq!(got.errors[4].arg, "maybe");
    let ok: Vec<(usize, &str, bool)> =
        got.rows.iter().map(|r| (r.row, r.req.param.as_str(), r.req.required)).collect();
    assert_eq!(ok, vec![(3, "FireRating", true), (9, "120", false)]);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn a_book_without_requirement_columns_is_no_sheet() {
    let p = tmp("nosheet");
    let mut wb = Workbook::new();
    wb.add_worksheet().write_string(0, 0, "Category only").unwrap();
    wb.save(&p).unwrap();
    let (_, v) = english();
    assert_eq!(read_requirements(&p, &v, None), Err(BookError::NoSheet));
    assert_eq!(read_requirements(&p, &v, Some("Nope")), Err(BookError::NoSheet));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn sheet_names_are_cleaned_cut_and_unique() {
    let taken = s(&["Windows"]);
    assert_eq!(sheet_name("Windows", &taken), "Windows 2");
    assert_eq!(sheet_name("a/b:c*?[x]", &[]), "a b c   x");
    assert_eq!(sheet_name("  ", &[]), "Sheet");
    let long = "x".repeat(40);
    assert_eq!(sheet_name(&long, &[]).chars().count(), 31);
    let n = sheet_name(&long, &[sheet_name(&long, &[])]);
    assert!(n.ends_with(" 2") && n.chars().count() == 31, "{n}");
}

#[test]
fn the_report_opens_with_a_summary_and_a_sheet_per_category() {
    let p = tmp("report");
    let summary = ReportSheet {
        name: "Summary".into(),
        note: Some("Passing is not proof of code compliance.".into()),
        headers: s(&["Category", "Elements", "Compliant"]),
        rows: vec![
            vec![RCell::Text("Windows".into()), RCell::Num(202.0), RCell::Num(198.0)],
            vec![RCell::Text("Doors".into()), RCell::Num(10.0), RCell::Num(10.0)],
        ],
    };
    let windows = ReportSheet {
        name: "Windows".into(),
        note: None,
        headers: s(&["Element", "Requirement", "Problem"]),
        rows: vec![vec![
            RCell::Text("opening:12".into()),
            RCell::Text("FireRating".into()),
            RCell::Text("=cmd|' /C calc'!A0".into()),
        ]],
    };
    let doors = ReportSheet { name: "Windows".into(), ..ReportSheet::default() };
    write_report(&p, &[summary, windows, doors]).unwrap();
    assert_eq!(sheet_names(&p).unwrap(), s(&["Summary", "Windows", "Windows 2"]));
    let groups = vec![vec!["Summary".to_string()], vec!["Windows".to_string()]];
    let got = read_first_of(&p, &groups, &|| false).unwrap();
    let sum = got[0].as_ref().unwrap();
    assert_eq!(sum.grid[0][0], SheetCell::Text("Passing is not proof of code compliance.".into()));
    assert_eq!(sum.grid[2][1], SheetCell::Text("Elements".into()));
    assert_eq!(sum.grid[3][1], SheetCell::Num(202.0));
    assert_eq!(sum.grid[3][2], SheetCell::Num(198.0));
    let win = got[1].as_ref().unwrap();
    assert_eq!(win.grid[1][0], SheetCell::Text("opening:12".into()));
    assert!(win.formulas.is_empty(), "a value that looks like a formula stays text");
    assert!(write_report(&p, &[]).is_err());
    let _ = std::fs::remove_file(&p);
}
