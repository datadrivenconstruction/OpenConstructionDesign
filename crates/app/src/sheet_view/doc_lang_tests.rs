// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Document language: sheet words, title blocks, schedules on sheets and tags follow the
//! project setting (else the interface language), for any locale and either standard.
use super::dset::build_drawing_set;
use super::*;
use ddc_io::sheets::dset_fixture::{build_into, build_unmarked_into};

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    build_into(&mut ed.model, 1).unwrap();
    ed.reload().unwrap();
    ed
}

fn sheet(ed: &mut Editor, standard: &str) -> i64 {
    let mut d = SheetDef::new("X-1", "Plan");
    d.standard = standard.into();
    let id = ed.model.add_sheet(&d).unwrap();
    let r = Revision {
        mark: "1".into(),
        date: "01.01.2026".into(),
        description: "issue".into(),
        author: "AB".into(),
    };
    ed.model.set_revisions(id, &[r]).unwrap();
    id
}

/// Texts of the sheet as the canvas, the preview and the PDF/DXF output draw it.
fn words(ed: &Editor, id: i64) -> Vec<String> {
    let c = composed(&mut Sheets::default(), ed, id).unwrap();
    c.drawing
        .marks
        .iter()
        .filter_map(|(_, m)| match m {
            Mark::Text { s, .. } => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// `f` with the stored document language `code` and the interface in Russian or English.
fn in_lang<R>(ed: &mut Editor, code: Option<&str>, ui_ru: bool, f: impl FnOnce(&Editor) -> R) -> R {
    ed.model.set_document_lang(code).unwrap();
    crate::with_test_lang(ui_ru, || f(ed))
}

fn has(w: &[String], s: &str) -> bool {
    w.iter().any(|x| x == s)
}

#[test]
fn sheet_and_title_block_words_follow_the_document_language() {
    let mut ed = editor();
    let g = sheet(&mut ed, "gost");
    // no setting: the interface language; Russian GOST sheets keep their words
    let ru = in_lang(&mut ed, None, true, |ed| words(ed, g));
    for w in ["Изменения", "Изм.", "Кол.уч.", "№ док.", "Н.контр.", "Стадия", "Листов"] {
        assert!(has(&ru, w), "{w}: {ru:?}");
    }
    let en = in_lang(&mut ed, None, false, |ed| words(ed, g));
    for w in ["Revisions", "Rev.", "Qty", "Doc. No.", "Stage", "Sheets"] {
        assert!(has(&en, w), "{w}: {en:?}");
    }
    assert!(!has(&en, "Изм.") && !has(&en, "Листов"));
    // a stored language wins over the interface, whichever it is
    let de = in_lang(&mut ed, Some("de"), true, |ed| words(ed, g));
    assert!(has(&de, "Revisionen") && has(&de, "Beschreibung"), "{de:?}");
    assert!(!has(&de, "Изменения") && !has(&de, "Изм."));
    let ja = in_lang(&mut ed, Some("ja"), false, |ed| words(ed, g));
    assert!(has(&ja, "改訂") && has(&ja, "内容"), "{ja:?}");
    assert!(!has(&ja, "Revisions"));
}

#[test]
fn iso_title_block_keeps_its_words_for_russian_and_reads_any_language() {
    let mut ed = editor();
    let s = sheet(&mut ed, "iso");
    let ru = in_lang(&mut ed, None, true, |ed| words(ed, s));
    assert!(has(&ru, "Responsible dept.") && has(&ru, "Изменения"), "{ru:?}");
    let en = in_lang(&mut ed, None, false, |ed| words(ed, s));
    assert!(has(&en, "Responsible dept.") && has(&en, "Revisions"));
    let de = in_lang(&mut ed, Some("de"), false, |ed| words(ed, s));
    assert!(has(&de, "Revisionen"), "{de:?}");
}

#[test]
fn sheet_schedules_follow_the_document_language() {
    let mut ed = editor();
    for (code, ui_ru, title, width) in [
        (None, false, "Doors", "Width, mm"),
        (None, true, "Двери", "Ширина, мм"),
        (Some("de"), false, "Türen", "Breite, mm"),
        (Some("ja"), true, "ドア", "幅, mm"),
    ] {
        let t = in_lang(&mut ed, code, ui_ru, |ed| schedule_table(ed, "doors").unwrap());
        assert_eq!(t.title, title, "{code:?}");
        assert!(t.header.iter().any(|h| h == width), "{code:?}: {:?}", t.header);
    }
}

#[test]
fn drawing_set_tags_follow_the_document_language() {
    for (code, ui_ru, want) in [(None, false, "D1"), (None, true, "Д1"), (Some("de"), false, "T1")] {
        let mut ed = Editor::new_in_memory().unwrap();
        build_unmarked_into(&mut ed.model, 1).unwrap();
        ed.reload().unwrap();
        ed.model.set_document_lang(code).unwrap();
        crate::with_test_lang(ui_ru, || {
            build_drawing_set(&mut ed, DsetDialog::default().options(), ui_ru).unwrap();
        });
        let tb = crate::with_test_lang(ui_ru, || schedule::build_building(&ed, Cat::Doors));
        let tags: Vec<String> = tb
            .rows
            .iter()
            .map(|r| ed.model.mark(ddc_model::TagHost::Opening(r.id)).unwrap())
            .collect();
        assert!(tags.iter().any(|t| t == want), "{code:?}: {tags:?}");
    }
}

#[test]
fn north_and_east_letters_follow_the_document_language() {
    let fmt = |l: Lang, standard: &str| {
        let w = |k: &str| crate::tr_in(l, k, &[]);
        let f = io::anno_fmt(standard, Some(&w));
        (f.north, f.east)
    };
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(fmt(Lang::Ru, "gost"), pair("С", "В"));
    assert_eq!(fmt(Lang::En, "gost"), pair("N", "E"));
    assert_eq!(fmt(Lang::Ru, "iso"), pair("N", "E"));
    // no translation yet: English, never the key
    assert_eq!(fmt(Lang::Ja, "gost"), pair("N", "E"));
    assert_eq!(io::anno_fmt("gost", None).north, "С");
}
