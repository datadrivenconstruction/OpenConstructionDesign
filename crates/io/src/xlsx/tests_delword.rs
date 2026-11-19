// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The delete column is read by one parser (`units::delete_word`) at every call site:
//! main sheets (walls, rooms), family sheets and the extra-parameter pass.

use calamine::Data;
use ddc_model::{CeilingSpec, LayerSpec, Model, RoomSpec, WallSpec};

use super::tests::{edit, set, tmp};
use super::units::{delete_word, DelWord};
use super::*;

const FAM: &str = "Ceilings";
const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

/// Every word that deletes, per language (escapes: Cyrillic x, "da", "udalit", check marks).
const YES: [&str; 11] = [
    "delete", "x", "\u{0445}", "1", "true", "yes", "\u{0434}\u{0430}",
    "\u{0443}\u{0434}\u{0430}\u{043b}\u{0438}\u{0442}\u{044c}", "+", "\u{2713}", "\u{2714}",
];
const NO: [&str; 6] = ["", "0", "no", "false", "\u{043d}\u{0435}\u{0442}", "-"];
const UNKNOWN: [&str; 3] = ["maybe", "\u{0434}\u{0430}\u{0436}\u{0435}", "xx"];

fn s(t: &str) -> Data {
    Data::String(t.into())
}

#[test]
fn parser_reads_every_word_case_and_space_insensitively() {
    for w in YES {
        for shown in [
            w.to_string(),
            w.to_uppercase(),
            format!("  {w}\t"),
            format!("\u{00a0}{}\u{00a0}", w.to_uppercase()),
        ] {
            assert_eq!(delete_word(&s(&shown)), DelWord::Yes, "{shown:?}");
        }
    }
    for w in NO {
        for shown in [w.to_string(), w.to_uppercase(), format!(" {w} "), format!("\u{00a0}{w}")] {
            assert_eq!(delete_word(&s(&shown)), DelWord::No, "{shown:?}");
        }
    }
    for w in UNKNOWN {
        assert_eq!(delete_word(&s(&format!(" {w} "))), DelWord::Unknown(w.into()));
    }
}

#[test]
fn parser_cyrillic_x_equals_latin_x_and_non_text_cells() {
    assert_eq!(delete_word(&s("\u{0445}")), delete_word(&s("x")));
    assert_eq!(delete_word(&s("\u{0425}")), DelWord::Yes);
    assert_eq!(delete_word(&Data::Empty), DelWord::No);
    assert_eq!(delete_word(&Data::Bool(true)), DelWord::Yes);
    assert_eq!(delete_word(&Data::Bool(false)), DelWord::No);
    assert_eq!(delete_word(&Data::Float(1.0)), DelWord::Yes);
    assert_eq!(delete_word(&Data::Int(1)), DelWord::Yes);
    assert_eq!(delete_word(&Data::Float(0.0)), DelWord::No);
    assert!(matches!(delete_word(&Data::Float(2.0)), DelWord::Unknown(_)));
}

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
        .unwrap();
    for x0 in [0.0, 10.0] {
        let p = [(x0, 0.0), (x0 + 5.0, 0.0), (x0 + 5.0, 4.0), (x0, 4.0)];
        for k in 0..4 {
            let w = WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            };
            m.add_wall(&w).unwrap();
        }
    }
    let r = RoomSpec { level: lv, name: "Hall".into(), number: "101".into(), point: (2.5, 2.0), height: 3.0 };
    m.add_room(&r).unwrap();
    let st = m
        .add_slab_type("ceiling", &[LayerSpec { material: "plaster".into(), thickness: 0.02 }])
        .unwrap();
    for h in [2.75, 2.6] {
        let c = CeilingSpec { level: lv, slab_type: st, height: h, outer: RING.to_vec(), holes: vec![] };
        m.add_ceiling(&c).unwrap();
    }
    m
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

fn ops(m: &Model) -> i64 {
    count(m, "op")
}

fn get_h(m: &Model, id: i64) -> Result<f64, ddc_model::Error> {
    Ok(m.sql().query_row("SELECT height FROM ceiling WHERE id=?1", [id], |r| r.get(0))?)
}

fn set_h(m: &mut Model, id: i64, v: f64) -> Result<(), ddc_model::Error> {
    let (level, slab_type): (i64, i64) = m.sql().query_row(
        "SELECT level,stype FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let c = CeilingSpec { level, slab_type, height: v, outer: RING.to_vec(), holes: vec![] };
    m.update_ceiling(id, &c)
}

fn fams() -> Vec<FamilySheet> {
    vec![FamilySheet {
        sheet: FAM,
        table: "ceiling",
        cols: vec![FamilyCol {
            key: "height",
            label: "Height".into(),
            dim: FamDim::Len,
            labels: vec![],
            lo: 0.0,
            hi: 100.0,
            get: get_h,
            set: set_h,
        }],
        delete: RowDelete::Allowed,
    }]
}

#[derive(Clone, Copy, Debug)]
enum Site {
    Walls,
    Rooms,
    Family,
}

#[derive(Debug, PartialEq, Eq)]
enum Out {
    Deleted,
    Kept,
    /// Refused with this reason (the element is kept).
    Refused(String),
}

fn opts(apply: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing: false, new_params: false }
}

/// Puts `cell` into the delete column of the first row of `site`, previews, applies and undoes;
/// checks preview == apply and undo, returns the outcome.
fn outcome(site: Site, cell: &Data, tag: &str) -> Out {
    let mut m = model();
    let (sheet, table) = match site {
        Site::Walls => (WALLS, "wall"),
        Site::Rooms => (ROOMS, "room"),
        Site::Family => (FAM, "ceiling"),
    };
    let p = tmp(&format!("dw_{tag}_src"));
    match site {
        Site::Family => export_xlsx_families(&m, &p, Units::Metric, &fams()).unwrap(),
        _ => export_xlsx(&m, &p).unwrap(),
    }
    let q = tmp(&format!("dw_{tag}_dst"));
    edit(&p, &q, |sh, g| {
        if sh == sheet {
            set(g, 1, "delete", cell.clone());
        }
    });
    let (n0, ops0) = (count(&m, table), ops(&m));
    let run = |m: &mut Model, apply: bool| match site {
        Site::Family => import_xlsx_families(m, &q, opts(apply), &fams()).unwrap(),
        _ => import_xlsx_with(m, &q, opts(apply)).unwrap(),
    };
    let prev = run(&mut m, false);
    assert_eq!((count(&m, table), ops(&m)), (n0, ops0), "preview must not write: {prev:?}");
    let rejected: Vec<_> = prev.rejected.iter().filter(|r| r.sheet == sheet).collect();
    if let Some(r) = rejected.first() {
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert!(r.row > 0 && r.sheet == sheet, "{r:?}");
    }
    let reason = rejected.first().map(|r| r.reason.clone());
    let app = run(&mut m, true);
    let gone = n0 - count(&m, table);
    assert_eq!(prev.counts().deleted, app.counts().deleted, "preview == apply");
    assert_eq!(
        app.rejected.iter().filter(|r| r.sheet == sheet).count(),
        rejected.len(),
        "preview == apply"
    );
    if gone > 0 {
        assert_eq!((gone, app.counts().deleted), (1, 1), "{app:?}");
        assert_eq!(ops(&m) - ops0, 1, "one undo step");
        m.undo().unwrap();
    }
    assert_eq!((count(&m, table), ops(&m)), (n0, ops0), "undo restores");
    match (gone, reason) {
        (1, None) => Out::Deleted,
        (0, None) => Out::Kept,
        (0, Some(why)) => Out::Refused(why),
        other => panic!("{other:?} {app:?}"),
    }
}

#[test]
fn every_site_reads_every_word_the_same_way() {
    let sites = [Site::Walls, Site::Rooms, Site::Family];
    let mut k = 0;
    for (words, want) in [(&YES[..], 'y'), (&NO[..], 'n')] {
        for w in words {
            for shown in [w.to_string(), w.to_uppercase(), format!(" {w} ")] {
                let cell = if shown.trim().is_empty() { Data::Empty } else { s(&shown) };
                for site in sites {
                    k += 1;
                    let got = outcome(site, &cell, &format!("t{k}"));
                    let exp = if want == 'y' { Out::Deleted } else { Out::Kept };
                    assert_eq!(got, exp, "{site:?} {shown:?}");
                }
            }
        }
    }
}

#[test]
fn unknown_word_is_refused_with_the_word_at_every_site_and_nothing_is_deleted() {
    let mut k = 0;
    for w in UNKNOWN {
        let mut reasons = vec![];
        for site in [Site::Walls, Site::Rooms, Site::Family] {
            k += 1;
            match outcome(site, &s(w), &format!("u{k}")) {
                Out::Refused(why) => {
                    assert!(why.starts_with("delete flag must be one of"), "{why}");
                    assert!(why.contains(&format!("'{w}'")), "{why}");
                    reasons.push(why);
                }
                other => panic!("{site:?} {w:?}: {other:?}"),
            }
        }
        assert!(reasons.windows(2).all(|p| p[0] == p[1]), "{reasons:?}");
    }
}

/// A sheet with an extra column runs the extra-parameter pass over its rows too: an unknown word
/// is refused once (main and extra passes agree on sheet, row and text) and nothing is deleted.
#[test]
fn extra_parameter_pass_refuses_an_unknown_word_once() {
    let mut m = model();
    let p = tmp("dw_extra_src");
    export_xlsx(&m, &p).unwrap();
    let q = tmp("dw_extra_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let w = g.iter().map(Vec::len).max().unwrap_or(0);
            for r in g.iter_mut() {
                r.resize(w, Data::Empty);
            }
            g[0].push(s("Work code"));
            for r in g.iter_mut().skip(1) {
                r.push(s("A-1"));
            }
            set(g, 1, "delete", s("maybe"));
        }
    });
    let walls = count(&m, "wall");
    let why = super::msg::fill_template(super::msg::DELETE_UNKNOWN.1, &["maybe"]);
    for apply in [false, true] {
        let rep = import_xlsx_with(&mut m, &q, opts(apply)).unwrap();
        let hits: Vec<_> = rep.rejected.iter().filter(|r| r.sheet == WALLS && r.reason == why).collect();
        assert_eq!(hits.len(), 1, "apply={apply}: {:?}", rep.rejected);
        assert_eq!(rep.counts().deleted, 0);
        assert_eq!(count(&m, "wall"), walls, "the row is kept");
    }
}

#[test]
fn refusal_text_has_en_and_ru_templates_listing_the_russian_words() {
    let en = include_str!("../../../ui/data/i18n/en/xl.json");
    let ru = include_str!("../../../ui/data/i18n/ru/xl.json");
    let (key, tpl) = super::msg::DELETE_UNKNOWN;
    for (name, json) in [("en", en), ("ru", ru)] {
        let line = json.lines().find(|l| l.contains(&format!("\"{key}\""))).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(line.matches("{}").count(), tpl.matches("{}").count(), "{name}");
    }
    let line = ru.lines().find(|l| l.contains(key)).unwrap();
    for w in ["\u{0434}\u{0430}", "\u{0443}\u{0434}\u{0430}\u{043b}\u{0438}\u{0442}\u{044c}", "\u{0445}"] {
        assert!(line.contains(w), "{line}");
    }
}
