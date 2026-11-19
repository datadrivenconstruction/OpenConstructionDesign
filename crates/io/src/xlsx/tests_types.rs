// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cell kinds a spreadsheet produces on its own (errors, dates, booleans): one human reason each.

use calamine::{CellErrorType, ExcelDateTime, ExcelDateTimeType};

use super::*;

fn tab(cells: usize) -> Tab {
    let keys = ["a", "b", "c"];
    Tab {
        name: "Walls",
        r0: 1,
        c0: 0,
        head: vec![],
        idx: keys.iter().take(cells).enumerate().map(|(i, k)| (*k, i)).collect(),
        headers: HashMap::new(),
        factors: HashMap::new(),
        dims: HashMap::new(),
        rows: vec![],
        base: None,
        also_known: HashSet::new(),
        merged_id: None,
        covered: HashSet::new(),
        formulas: HashSet::new(),
    }
}

fn date() -> Data {
    Data::DateTime(ExcelDateTime::new(45000.0, ExcelDateTimeType::DateTime, false))
}

fn reason_of(cell: Data, number: bool) -> String {
    let t = tab(1);
    let cells = [cell];
    let mut row = Row::new(&t, 2, &cells);
    let mut rep = Report::default();
    let r = if number {
        row.parse_num(&mut rep, "a").map(|_| ())
    } else {
        row.parse_text(&mut rep, "a").map(|_| ())
    };
    assert!(r.is_err() && !row.ok);
    assert_eq!(rep.rejected.len(), 1);
    rep.rejected.remove(0).reason
}

fn locale(src: &str) -> std::collections::BTreeMap<String, String> {
    serde_json::from_str(src).expect("locale file is valid JSON")
}

/// Locale key whose English template matches `reason`.
fn key_of(en: &std::collections::BTreeMap<String, String>, reason: &str) -> String {
    en.iter()
        .filter(|(k, _)| k.starts_with("xl.msg."))
        .find(|(_, t)| match_template(t, reason).is_some())
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| panic!("no locale template for {reason:?}"))
}

#[test]
fn import_error_cell_reason() {
    let en = locale(include_str!("../../../ui/data/i18n/en/xl.json"));
    let ru = locale(include_str!("../../../ui/data/i18n/ru/xl.json"));
    for number in [true, false] {
        let why = reason_of(Data::Error(CellErrorType::Ref), number);
        assert_eq!(why, "formula error #REF! \u{2014} fix or clear the cell");
        assert!(!why.contains("Error("), "{why}");
        let k = key_of(&en, &why);
        assert!(ru[&k].contains("#REF!") == false && ru[&k].contains("{}"));
    }
}

#[test]
fn import_date_in_mark_reason() {
    let en = locale(include_str!("../../../ui/data/i18n/en/xl.json"));
    let ru = locale(include_str!("../../../ui/data/i18n/ru/xl.json"));
    let text = reason_of(date(), false);
    assert_eq!(
        text,
        "the spreadsheet turned this value into a date \u{2014} format the column as Text and type it again"
    );
    let num = reason_of(date(), true);
    assert!(num.contains("as Number"), "{num}");
    for why in [&text, &num] {
        assert!(!why.contains("DateTime"), "{why}");
        let k = key_of(&en, why);
        assert!(ru[&k].chars().any(|c| (0x400..=0x4FF).contains(&(c as u32))));
    }
}

#[test]
fn import_bool_in_text_kept() {
    let t = tab(2);
    let cells = [Data::Bool(true), Data::Bool(false)];
    let mut row = Row::new(&t, 2, &cells);
    let mut rep = Report::default();
    assert_eq!(row.parse_text(&mut rep, "a"), Ok(Some("TRUE".to_string())));
    assert_eq!(row.parse_text(&mut rep, "b"), Ok(Some("FALSE".to_string())));
    assert!(row.ok && rep.rejected.is_empty());
}
