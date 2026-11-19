// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Multi-category schedules: definition storage, formulas, totals, drafts and the XLSX diff.

use super::*;
use ddc_model::OpeningSpec;

const U: Units = Units::Metric;

/// The test room (four walls) with two doors on its first wall.
fn room_with_doors() -> Editor {
    let mut e = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut e);
    let wall = e.walls[0].id;
    for along in [1.0, 2.2] {
        let spec = OpeningSpec {
            along,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
            kind: "door".into(),
        };
        e.model.add_opening(wall, &spec).unwrap();
    }
    e.reload().unwrap();
    e
}

fn param(k: &str) -> ColSpec {
    ColSpec::Param(k.to_string())
}

fn formula(name: &str, expr: &str) -> ColSpec {
    ColSpec::Formula {
        name: name.into(),
        expr: expr.into(),
    }
}

fn def(columns: Vec<ColSpec>) -> MultiDef {
    MultiDef {
        name: "Heights".into(),
        cats: vec![Cat::Walls, Cat::Doors],
        columns,
        ..Default::default()
    }
}

fn at(m: &Multi, key: &str) -> usize {
    m.table
        .cols
        .iter()
        .position(|c| c.key == key)
        .unwrap_or_else(|| panic!("no column {key}"))
}

fn num(m: &Multi, row: usize, key: &str) -> f64 {
    match &m.table.rows[row].vals[at(m, key)] {
        Val::N(x) => *x,
        v => panic!("{key}: {v:?}"),
    }
}

#[test]
fn definition_round_trips_and_lives_in_the_project() {
    let mut d = def(vec![param("h"), formula("Twice", "h * 2")]);
    d.filters = vec![("h".into(), ">1000".into())];
    d.sort = Some(("h".into(), false));
    d.group = vec!["cat".into()];
    d.totals = true;
    let t = defs_to_text(std::slice::from_ref(&d));
    assert_eq!(defs_from_text(&t).unwrap(), vec![d.clone()]);
    assert!(defs_from_text("{\"schedules\":[{\"cats\":[\"nope\"],\"columns\":[]}]}").is_err());

    let mut e = room_with_doors();
    assert!(load(&e).is_empty());
    let u0 = e.undo_len();
    assert!(save(&mut e, std::slice::from_ref(&d)).unwrap());
    assert_eq!(e.undo_len(), u0 + 1, "one undo step");
    assert_eq!(load(&e), vec![d.clone()]);
    assert!(
        !save(&mut e, std::slice::from_ref(&d)).unwrap(),
        "same text: no write"
    );
    e.undo();
    assert!(load(&e).is_empty());
}

#[test]
fn categories_formulas_groups_and_totals() {
    let e = room_with_doors();
    let mut d = def(vec![
        param("h"),
        formula("Twice", "h * 2"),
        formula("Group sum", "sum([h])"),
        formula("All", "total(h)"),
        formula("Rows", "count(h)"),
        formula("Share", "round(h / total(h), 3)"),
        formula("Big", "if(h > 2.5, 1, 0)"),
    ]);
    d.group = vec!["cat".into()];
    d.totals = true;
    let m = build(&e, &d, false);
    assert!(m.notes.is_empty(), "{:?}", m.notes);
    assert_eq!(m.table.rows.len(), 4 + 2, "walls and doors in one table");
    let (walls, doors): (Vec<_>, Vec<_>) = (0..6).partition(|&r| m.origin[r].0 == 0);
    assert_eq!((walls.len(), doors.len()), (4, 2));
    let h: Vec<f64> = (0..6).map(|r| num(&m, r, "h")).collect();
    let all: f64 = h.iter().sum();
    for r in 0..6 {
        let grp = if m.origin[r].0 == 0 { &walls } else { &doors };
        assert_eq!(num(&m, r, "f:0"), h[r] * 2.0);
        assert!((num(&m, r, "f:1") - grp.iter().map(|&i| h[i]).sum::<f64>()).abs() < 1e-9);
        assert!((num(&m, r, "f:2") - all).abs() < 1e-9);
        assert_eq!(num(&m, r, "f:3"), grp.len() as f64);
    }
    assert_eq!(
        m.table.cols[at(&m, "f:0")].kind,
        Kind::Len,
        "units follow the formula"
    );
    assert_eq!(
        m.table.cols[at(&m, "f:4")].kind,
        Kind::Num,
        "a ratio has no unit"
    );
    let laid = lay(&m, U);
    assert_eq!(laid.ord.len(), 6);
    let tot = totals(&m, &laid);
    assert!(
        (tot[at(&m, "h")].unwrap() - all).abs() < 1e-9,
        "totals run over every category"
    );
    assert!(laid
        .lines
        .iter()
        .any(|l| matches!(l, crate::schedule::group::Line::Sub { .. })));
    // a filter and sort act on the combined rows
    d.filters = vec![("cat".into(), "Doors".into())];
    d.sort = Some(("h".into(), true));
    let m2 = build(&e, &d, false);
    assert_eq!(lay(&m2, U).ord.len(), 2);
}

#[test]
fn bad_formulas_are_clear_errors_not_panics() {
    let e = room_with_doors();
    let bad = [
        "h +",
        "nothing * 2",
        "w + area",
        "h + area",
        "sqrt(h)",
        "sum(1)",
        "sum([h]",
        "avg(h) $ 2",
    ];
    for expr in bad {
        let cols = vec![param("h"), param("area"), param("w"), formula("X", expr)];
        let m = build(&e, &def(cols), false);
        let col = at(&m, "f:0");
        let all_err = m
            .table
            .rows
            .iter()
            .all(|r| matches!(&r.vals[col], Val::T(t) if t.starts_with('#')));
        assert!(all_err, "{expr}: {:?}", m.table.rows[0].vals[col]);
        assert_eq!(m.notes.len(), 1, "{expr}: {:?}", m.notes);
    }
    // division by zero is a cell error, the column stays
    let m = build(&e, &def(vec![param("h"), formula("X", "h / 0")]), false);
    assert!(m.notes.is_empty());
    assert!(matches!(&m.table.rows[0].vals[at(&m, "f:0")], Val::T(t) if t.starts_with('#')));
    // unit mismatch names both units; sqrt of an area is a length
    let m = build(
        &e,
        &def(vec![param("h"), param("area"), formula("X", "h + area")]),
        false,
    );
    assert!(
        m.notes[0].contains("m^1") && m.notes[0].contains("m^2"),
        "{:?}",
        m.notes
    );
    let m = build(
        &e,
        &def(vec![param("area"), formula("X", "sqrt(area)")]),
        false,
    );
    assert!(m.notes.is_empty(), "{:?}", m.notes);
    assert_eq!(m.table.cols[at(&m, "f:0")].kind, Kind::Len);
    // an unknown column is a note, the rest still builds
    let m = build(&e, &def(vec![param("h"), param("zzz")]), false);
    assert_eq!(m.notes.len(), 1);
    assert_eq!(m.table.rows.len(), 6);
}

fn hs(e: &Editor) -> (f64, f64) {
    (e.walls[0].height, e.openings[0].height)
}

#[test]
fn cell_edit_is_a_draft_then_one_undo_step() {
    let mut e = room_with_doors();
    let d = def(vec![param("h"), formula("Twice", "h * 2")]);
    let m = build(&e, &d, false);
    let h = at(&m, "h");
    let wall = m.origin.iter().position(|o| *o == (0, 0)).unwrap();
    let door = m.origin.iter().position(|o| *o == (1, 0)).unwrap();
    let cells = vec![
        (wall, h, "3500".to_string()),
        (door, h, "2400".to_string()),
        (wall, at(&m, "f:0"), "1".to_string()),
        (wall, 0, "x".to_string()),
    ];
    let draft = draft_cells(&m, &cells);
    assert_eq!(draft.skipped.len(), 2, "computed cells are read-only");
    let before = hs(&e);
    let p = preview(&e, &m, &draft, U);
    assert_eq!(hs(&e), before, "a preview writes nothing");
    assert_eq!(p.changes.len(), 2, "{p:?}");
    assert_eq!(p.refused.len(), 2);
    let c = p.changes.iter().find(|c| c.cat == Cat::Doors).unwrap();
    assert_eq!((c.id, c.after.as_str()), (m.parts[1].rows[0].id, "2400"));
    let u0 = e.undo_len();
    let o = apply(&mut e, &m, &draft);
    assert_eq!(o.done, 2, "{o:?}");
    assert_eq!(e.undo_len(), u0 + 1, "both categories in one undo step");
    assert!((e.walls[0].height - 3.5).abs() < 1e-9 && (e.openings[0].height - 2.4).abs() < 1e-9);
    e.undo();
    assert_eq!(hs(&e), before);
    // the same value again changes nothing
    let m = build(&e, &d, false);
    let same = draft_cells(&m, &[(wall, h, format!("{:.0}", before.0 * 1000.0))]);
    let p = preview(&e, &m, &same, U);
    assert_eq!((p.changes.len(), p.unchanged), (0, 1));
}

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_sch1m_{tag}_{}.xlsx", std::process::id()))
}

#[test]
fn xlsx_round_trip_gives_the_exact_diff() {
    let mut e = room_with_doors();
    let mut d = def(vec![param("h"), param("w"), formula("Twice", "h * 2")]);
    d.group = vec!["cat".into()];
    d.totals = true;
    let m = build(&e, &d, false);
    let laid = lay(&m, U);
    let p = tmp("rt");
    export_xlsx(&e.model, &m, &laid, U, &p).unwrap();
    let (name, mut grid) = ddc_io::xlsx::read_sheet_cells(&p, Some(&sheet_name(&d))).unwrap();
    // untouched book: no difference
    let none = import_grid(&e, &m, U, &name, &grid).unwrap();
    assert!(
        none.draft.is_empty() && none.preview.changes.is_empty(),
        "{:?}",
        none.preview
    );
    assert_eq!(none.rows, 6);
    // edit a wall height and a door height; a computed cell is not an edit
    let head = grid.iter().position(|r| r.iter().any(is_id_head)).unwrap();
    let col = |t: &str| {
        grid[head]
            .iter()
            .position(|c| matches!(c, SheetCell::Text(s) if s.starts_with(t)))
            .unwrap()
    };
    let (ch, ctwice) = (col("Height"), col("Twice"));
    let row_of = |g: &[Vec<SheetCell>], id: String| {
        g.iter()
            .position(|r| matches!(r.first(), Some(SheetCell::Text(s)) if *s == id))
            .unwrap()
    };
    let wid = format!("walls:{}", m.parts[0].rows[0].id);
    let did = format!("doors:{}", m.parts[1].rows[1].id);
    let (rw, rd) = (row_of(&grid, wid), row_of(&grid, did));
    grid[rw][ch] = SheetCell::Num(3500.0);
    grid[rd][ch] = SheetCell::Num(2300.0);
    grid[rw][ctwice] = SheetCell::Num(1.0);
    let imp = import_grid(&e, &m, U, &name, &grid).unwrap();
    assert_eq!(
        imp.preview.changes.len(),
        2,
        "{:?} {:?}",
        imp.preview.changes,
        imp.preview.refused
    );
    let w = imp
        .preview
        .changes
        .iter()
        .find(|c| c.cat == Cat::Walls)
        .unwrap();
    assert_eq!((w.id, w.after.as_str()), (m.parts[0].rows[0].id, "3500"));
    assert_eq!(
        w.at,
        Some((rw as u32 + 1, ch as u32 + 1)),
        "book coordinates"
    );
    let dr = imp
        .preview
        .changes
        .iter()
        .find(|c| c.cat == Cat::Doors)
        .unwrap();
    assert_eq!((dr.id, dr.after.as_str()), (m.parts[1].rows[1].id, "2300"));
    // nothing is written before Apply; Apply is one undo step
    assert!((e.walls[0].height - 3.5).abs() > 1e-6);
    let u0 = e.undo_len();
    let o = apply(&mut e, &m, &imp.draft);
    assert_eq!(o.done, 2, "{o:?}");
    assert_eq!(e.undo_len(), u0 + 1);
    assert!((e.walls[0].height - 3.5).abs() < 1e-9 && (e.openings[1].height - 2.3).abs() < 1e-9);
    // a category the schedule does not hold is counted, never created
    grid[rw][0] = SheetCell::Text("ghosts:1".into());
    let imp = import_grid(&e, &m, U, &name, &grid).unwrap();
    assert_eq!(imp.unknown, 1);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn strings_exist_in_both_languages() {
    let args = [
        ("k", "K"),
        ("h", "H"),
        ("n", "N"),
        ("e", "E"),
        ("row", "1"),
        ("col", "C"),
        ("cat", "T"),
        ("id", "7"),
        ("why", "W"),
        ("f", "F"),
        ("a", "A"),
        ("b", "B"),
    ];
    for key in [
        "schm.col.cat",
        "schm.unknown_col",
        "schm.bad_formula",
        "schm.not_editable",
        "schm.gone",
        "schm.filter_after_agg",
        "schm.dup_header",
        "schm.ambiguous_header",
        "schm.ui.columns_hint",
        "schm.ui.title",
        "schm.ui.filters",
        "schm.ui.filters_hint",
        "schm.refused",
        "schm.f.agg_arg",
        "schm.f.agg_here",
        "schm.f.agg_empty",
        "schm.f.unit_mismatch",
        "schm.f.unit_root",
        "schm.f.unit_unknown",
        "schm.f.round_digits",
        "schm.f.condition",
    ] {
        let (en, ru) = (
            crate::tr_lang(false, key, &args),
            crate::tr_lang(true, key, &args),
        );
        assert!(en != key && ru != key && en != ru, "{key}: {en} / {ru}");
        assert!(
            !en.contains('{') && !ru.contains('{'),
            "{key}: unfilled argument"
        );
    }
}

#[test]
fn form_text_round_trips() {
    let cols = parse_columns(
        "h; w ;Twice = h * 2;\n Share = round(h / total(h), 3); p:12; Big = if(h = 2, 1, 0)",
    );
    assert_eq!(cols.len(), 6);
    assert_eq!(cols[2], formula("Twice", "h * 2"));
    assert_eq!(
        cols[4],
        param("p:12"),
        "a project parameter key is a column, not a formula"
    );
    assert_eq!(cols[5], formula("Big", "if(h = 2, 1, 0)"));
    assert_eq!(parse_columns(&columns_text(&cols)), cols);
    assert!(parse_columns("  ; ").is_empty());
    assert_eq!(
        parse_keys("cat, h,,"),
        vec!["cat".to_string(), "h".to_string()]
    );
    assert_eq!(parse_sort("h desc"), Some(("h".into(), false)));
    assert_eq!(
        parse_sort(&sort_text(&Some(("w".into(), true)))),
        Some(("w".into(), true))
    );
    assert_eq!(parse_sort(" "), None);
    let f = parse_filters("h: >2500; cat:Doors;bad;: x; w:");
    assert_eq!(
        f,
        vec![
            ("h".to_string(), ">2500".to_string()),
            ("cat".to_string(), "Doors".to_string())
        ]
    );
    assert_eq!(parse_filters(&filters_text(&f)), f);
}

#[test]
fn drafts_merge_newest_text_wins() {
    let cell = |id: i64, col: &str, t: &str| DraftCell {
        cat: Cat::Walls,
        id,
        sub: 0,
        col: col.into(),
        text: t.into(),
    };
    let mut a = Draft::default();
    a.set(cell(1, "h", "a"));
    let mut b = Draft::default();
    b.set(cell(1, "h", "b"));
    b.set(cell(2, "h", "c"));
    b.skipped.push((Cat::Walls, 9, "Area".into()));
    a.merge(b);
    assert_eq!(a.len(), 2);
    assert_eq!(a.text(Cat::Walls, 1, 0, "h"), Some("b"));
    assert_eq!(a.text(Cat::Walls, 2, 0, "h"), Some("c"));
    assert_eq!(a.text(Cat::Doors, 1, 0, "h"), None);
    assert_eq!(a.skipped.len(), 1);
}

#[test]
fn filters_sort_group_keys_must_exist() {
    let e = room_with_doors();
    let mut d = def(vec![param("h")]);
    d.filters = vec![("h".into(), ">2500".into()), ("nope".into(), "x".into())];
    d.group = vec!["gone".into()];
    d.sort = Some(("missing".into(), true));
    let m = build(&e, &d, false);
    assert_eq!(m.notes.len(), 3, "{:?}", m.notes);
    // the real filter still acts: walls are 3 m, doors 2 m
    d.filters = vec![("h".into(), ">2500".into())];
    d.group.clear();
    d.sort = None;
    let m = build(&e, &d, false);
    assert!(m.notes.is_empty());
    let n = lay(&m, U).ord.len();
    assert_eq!(n, 4, "only the walls pass the filter");
}

fn wall_cell(m: &Multi, id: i64, key: &str, text: &str) -> (usize, usize, String) {
    let row = m
        .origin
        .iter()
        .position(|&(p, i)| m.parts[p].cat == Cat::Walls && m.parts[p].rows[i].id == id);
    (row.unwrap(), at(m, key), text.into())
}

fn delete_wall(e: &mut Editor, id: i64) {
    let op = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.delete_wall(id)
    }));
    e.run_op(op).unwrap();
}

/// A draft names elements, so an undo or a deleted element cannot redirect it.
#[test]
fn draft_survives_row_shifts_without_hitting_a_neighbour() {
    let mut e = room_with_doors();
    let d = def(vec![param("h")]);
    let m = build(&e, &d, false);
    // the doors sit on wall 0: delete a wall before the staged one
    let (first, second) = (e.walls[1].id, e.walls[2].id);
    let draft = draft_cells(&m, &[wall_cell(&m, second, "h", "3700")]);
    delete_wall(&mut e, first);
    let m2 = build(&e, &d, false);
    assert!(m2.table.rows.len() < m.table.rows.len());
    let p = preview(&e, &m2, &draft, U);
    assert_eq!(p.changes.len(), 1, "{p:?}");
    assert_eq!(p.changes[0].id, second);
    let before: HashMap<i64, f64> = e.walls.iter().map(|w| (w.id, w.height)).collect();
    assert_eq!(apply(&mut e, &m2, &draft).done, 1);
    for w in &e.walls {
        let want = if w.id == second { 3.7 } else { before[&w.id] };
        assert!((w.height - want).abs() < 1e-9, "wall {} changed", w.id);
    }
    // the element the draft names is gone: reported, nothing written, no panic
    let gone = draft_cells(&m, &[wall_cell(&m, first, "h", "3100")]);
    let m3 = build(&e, &d, false);
    let p = preview(&e, &m3, &gone, U);
    assert!(p.changes.is_empty() && p.refused.len() == 1, "{p:?}");
    let u0 = e.undo_len();
    let o = apply(&mut e, &m3, &gone);
    assert_eq!((o.done, o.bad), (0, 1));
    assert_eq!(e.undo_len(), u0, "nothing written");
    // an undo that brings the element back: the same draft acts on it again
    e.undo();
    e.undo();
    let m4 = build(&e, &d, false);
    assert_eq!(preview(&e, &m4, &gone, U).changes.len(), 1);
}

/// Project parameter keys carry a colon.
#[test]
fn project_parameter_keys_with_a_colon_are_columns_and_filters() {
    assert_eq!(parse_columns("p:12"), vec![param("p:12")]);
    assert_eq!(
        parse_columns("p:12; Net = h - 1"),
        vec![param("p:12"), formula("Net", "h - 1")]
    );
    let f = parse_filters("p:12: >5; name:a:b; h:>2500");
    assert_eq!(
        f,
        vec![
            ("p:12".to_string(), ">5".to_string()),
            ("name".to_string(), "a:b".to_string()),
            ("h".to_string(), ">2500".to_string()),
        ]
    );
    assert_eq!(
        parse_filters("p:12:>5"),
        vec![("p:12".to_string(), ">5".to_string())]
    );
    assert_eq!(parse_filters(&filters_text(&f)), f);
    // comparison operators in a bare item do not make it a formula
    assert_eq!(parse_columns("a >= 2"), vec![param("a >= 2")]);
}

/// A draft of many cells stays linear: a lookup per cell, no scan.
#[test]
fn large_drafts_stage_and_merge_fast() {
    let cell = |id: i64, t: &str| DraftCell {
        cat: Cat::Walls,
        id,
        sub: 0,
        col: "h".into(),
        text: t.into(),
    };
    let t = std::time::Instant::now();
    let mut d = Draft::default();
    let mut again = Draft::default();
    for i in 0..60_000 {
        d.set(cell(i, "1"));
        again.set(cell(i, "2"));
    }
    d.merge(again);
    assert_eq!(d.len(), 60_000);
    assert_eq!(d.text(Cat::Walls, 59_999, 0, "h"), Some("2"));
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
}

/// Group functions count the rows the filters let through.
#[test]
fn group_functions_follow_the_filters() {
    let e = room_with_doors();
    let mut d = def(vec![
        param("h"),
        formula("All", "total(h)"),
        formula("Rows", "count(h)"),
    ]);
    let m = build(&e, &d, false);
    let all: f64 = (0..6).map(|r| num(&m, r, "h")).sum();
    assert!((num(&m, 0, "f:0") - all).abs() < 1e-9);
    d.filters = vec![("cat".into(), "Doors".into())];
    let m = build(&e, &d, false);
    let doors: Vec<usize> = (0..6).filter(|&r| m.origin[r].0 == 1).collect();
    let want: f64 = doors.iter().map(|&r| num(&m, r, "h")).sum();
    for r in 0..6 {
        assert!((num(&m, r, "f:0") - want).abs() < 1e-9, "row {r}");
        assert_eq!(num(&m, r, "f:1"), 2.0);
    }
    let laid = lay(&m, U);
    let shown: f64 = laid.ord.iter().map(|&r| num(&m, r as usize, "h")).sum();
    assert!(
        (shown - want).abs() < 1e-9,
        "the total matches what is visible"
    );
    // a numeric filter on a parameter column
    d.filters = vec![("h".into(), ">2500".into())];
    let m = build(&e, &d, false);
    assert_eq!(num(&m, 0, "f:1"), 4.0);
    // a filter on a computed column is said to act after the group functions
    d.filters = vec![("f:1".into(), ">1".into())];
    assert!(build(&e, &d, false)
        .notes
        .iter()
        .any(|n| n.contains("group functions") || n.contains("\u{433}\u{440}\u{443}\u{43f}\u{43f}\u{43e}\u{432}\u{44b}\u{445}")));
}

fn head_text(g: &[Vec<SheetCell>], r: usize, c: usize) -> String {
    match &g[r][c] {
        SheetCell::Text(t) => t.clone(),
        _ => String::new(),
    }
}

/// A header two columns share is never guessed.
#[test]
fn shared_headers_are_not_read_from_a_book() {
    let e = room_with_doors();
    let d = def(vec![param("h"), formula("Height", "h * 2")]);
    let m = build(&e, &d, false);
    assert!(
        m.notes
            .iter()
            .any(|n| n.contains("Height") && n.contains("share")),
        "{:?}",
        m.notes
    );
    let laid = lay(&m, U);
    let p = tmp("dup");
    export_xlsx(&e.model, &m, &laid, U, &p).unwrap();
    let (name, mut grid) = ddc_io::xlsx::read_sheet_cells(&p, Some(&sheet_name(&d))).unwrap();
    let head = grid.iter().position(|r| r.iter().any(is_id_head)).unwrap();
    // the book has two columns under one header: the real one and the formula one
    let hs: Vec<usize> = (0..grid[head].len())
        .filter(|&j| matches!(&grid[head][j], SheetCell::Text(s) if s.starts_with("Height")))
        .collect();
    assert_eq!(hs.len(), 2);
    let same = head_text(&grid, head, hs[0]);
    grid[head][hs[1]] = SheetCell::Text(same);
    let wid = format!("walls:{}", m.parts[0].rows[0].id);
    let r = grid
        .iter()
        .position(|r| matches!(r.first(), Some(SheetCell::Text(s)) if *s == wid))
        .unwrap();
    for &j in &hs {
        grid[r][j] = SheetCell::Num(4100.0);
    }
    let imp = import_grid(&e, &m, U, &name, &grid).unwrap();
    assert!(imp.draft.is_empty(), "{:?}", imp.preview);
    assert_eq!(imp.ambiguous.len(), 1);
    assert!(imp
        .preview
        .refused
        .iter()
        .any(|l| l.contains("several") || l.contains("\u{43d}\u{435}\u{441}\u{43a}\u{43e}\u{43b}\u{44c}\u{43a}\u{438}\u{445}")));
    let _ = std::fs::remove_file(&p);
}

/// Sheet names a spreadsheet accepts.
#[test]
fn sheet_names_are_sanitised() {
    let n = |s: &str| {
        sheet_name(&MultiDef {
            name: s.into(),
            ..Default::default()
        })
    };
    assert_eq!(n("'Walls'"), "Walls");
    assert_eq!(n("''x''"), "x");
    assert_eq!(n("History"), "Schedule history");
    assert_eq!(n(" history "), "Schedule history");
    assert_eq!(n("a/b:c[d]*?"), "abcd");
    assert_eq!(n("''"), "Schedule");
    assert_eq!(n(&"x".repeat(50)).chars().count(), 31);
}

/// Group headings and total lines are not element rows.
#[test]
fn group_lines_with_colons_are_not_unknown_elements() {
    let e = room_with_doors();
    let d = def(vec![param("h")]);
    let m = build(&e, &d, false);
    let laid = lay(&m, U);
    let p = tmp("heads");
    export_xlsx(&e.model, &m, &laid, U, &p).unwrap();
    let (name, mut grid) = ddc_io::xlsx::read_sheet_cells(&p, Some(&sheet_name(&d))).unwrap();
    let head = grid.iter().position(|r| r.iter().any(is_id_head)).unwrap();
    let blank = vec![SheetCell::Empty; grid[head].len()];
    for text in ["Walls: 4", "Level 1: 12", "Room 5: 7 \u{b7} 3"] {
        let mut row = blank.clone();
        row[0] = SheetCell::Text(text.into());
        grid.push(row);
    }
    // a heading with other cells too is still no element when its tail is no id
    let mut row = blank.clone();
    row[0] = SheetCell::Text("Zone: north".into());
    row[1] = SheetCell::Num(1.0);
    grid.push(row);
    assert_eq!(import_grid(&e, &m, U, &name, &grid).unwrap().unknown, 0);
    // a real ghost still counts
    let mut row = blank;
    row[0] = SheetCell::Text("ghosts:7".into());
    row[1] = SheetCell::Num(1.0);
    grid.push(row);
    assert_eq!(import_grid(&e, &m, U, &name, &grid).unwrap().unknown, 1);
    let _ = std::fs::remove_file(&p);
}
