use calamine::Data;
use ddc_model::{Model, RoomSpec};

use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

fn room(m: &mut Model, lv: i64, name: &str, number: &str) -> i64 {
    m.add_room(&RoomSpec {
        level: lv,
        name: name.into(),
        number: number.into(),
        point: (1.0, 1.0),
        height: 3.0,
    })
    .unwrap()
}

fn model(numbers: &[(&str, &str)]) -> (Model, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let ids = numbers.iter().map(|(n, k)| room(&mut m, lv, n, k)).collect();
    (m, ids)
}

fn opts(apply: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing: false, new_params: false }
}

/// Book where the first room row lost its id and got a new name.
fn lost_id_book(m: &Model, tag: &str) -> std::path::PathBuf {
    let src = tmp(&format!("{tag}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("{tag}_dst"));
    edit(&src, &dst, |s, g: &mut Grid| {
        if s == "Rooms" {
            set(g, 1, "id", Data::Empty);
            set(g, 1, "name", Data::String("Lobby".into()));
        }
    });
    dst
}

fn room_count(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM room", [], |r| r.get(0)).unwrap()
}

fn room_name(m: &Model, id: i64) -> String {
    m.sql().query_row("SELECT name FROM room WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

#[test]
fn xlsx_row_without_id_unique_mark_is_proposed() {
    let (mut m, ids) = model(&[("Hall", "101"), ("Office", "102")]);
    let p = lost_id_book(&m, "ri1_prop");
    let rep = import_xlsx_keyed(&mut m, &p, opts(false), &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.matches.len(), 1, "{:?}", rep.matches);
    let km = &rep.matches[0];
    assert_eq!((km.id, km.state, km.kind, km.key.as_str()), (ids[0], MatchState::Pending, "room", "101"));
    assert_eq!(km.changes.len(), 1, "the rename waits inside the match");
    // nothing is applied or created while the decision is open
    assert!(rep.changes.is_empty() && rep.edits.is_empty(), "{:?}", rep.changes);
    assert_eq!(rep.counts().added, 0);
    assert_eq!(rep.pending_matches(), 1);

    let mut dec = KeyDecisions::default();
    dec.accept.insert(("Rooms".into(), km.row));
    let rep = import_xlsx_keyed(&mut m, &p, opts(false), &dec).unwrap();
    assert_eq!(rep.matches[0].state, MatchState::Accepted);
    assert_eq!((rep.counts().added, rep.counts().changed), (0, 1));
    assert_eq!(rep.edits.len(), 1);
    assert_eq!(rep.pending_matches(), 0);
}

#[test]
fn xlsx_pending_match_applies_nothing_in_the_model() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "ri1_pend");
    let ops = m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0)).unwrap();
    let rep = import_xlsx_keyed(&mut m, &p, opts(true), &KeyDecisions::default()).unwrap();
    assert_eq!(rep.pending_matches(), 1);
    assert_eq!((room_count(&m), room_name(&m, ids[0]).as_str()), (1, "Hall"));
    let ops2 = m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(ops, ops2, "no operation was recorded");
}

#[test]
fn xlsx_ambiguous_mark_never_matches() {
    let (mut m, _) = model(&[("Hall", "101"), ("Annex", "101")]);
    let p = lost_id_book(&m, "ri1_amb");
    let rep = import_xlsx_keyed(&mut m, &p, opts(false), &KeyDecisions::default()).unwrap();
    assert!(rep.matches.is_empty(), "{:?}", rep.matches);
    assert_eq!(rep.counts().added, 1, "the row creates as before");
    let note = rep.ignored.iter().find(|i| i.reason.starts_with("key matches 2 elements"));
    assert!(note.is_some(), "{:?}", rep.ignored);
}

#[test]
fn xlsx_key_owned_by_a_row_with_an_id_is_not_matched() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let src = tmp("ri1_taken_src");
    export_xlsx(&m, &src).unwrap();
    let dst = tmp("ri1_taken_dst");
    edit(&src, &dst, |s, g: &mut Grid| {
        if s == "Rooms" {
            let mut copy = g[1].clone();
            copy[0] = Data::Empty;
            g.push(copy);
        }
    });
    let rep = import_xlsx_keyed(&mut m, &dst, opts(false), &KeyDecisions::default()).unwrap();
    assert!(rep.matches.is_empty(), "{:?}", rep.matches);
    assert_eq!(rep.counts().added, 1);
    assert!(rep.ignored.iter().any(|i| i.reason.contains("used by another row")));
    assert_eq!(ids.len(), 1);
}

#[test]
fn xlsx_rejected_match_creates_a_new_room() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "ri1_rej");
    let mut dec = KeyDecisions::default();
    dec.reject.insert(("Rooms".into(), 2));
    let rep = import_xlsx_keyed(&mut m, &p, opts(true), &dec).unwrap();
    assert!(rep.applied && rep.accepted_matches() == 0 && rep.pending_matches() == 0, "{rep:?}");
    assert_eq!(rep.matches.len(), 1);
    assert_eq!(rep.matches[0].state, MatchState::Rejected);
    assert_eq!(room_count(&m), 2);
    assert_eq!(room_name(&m, ids[0]), "Hall");
}

#[test]
fn xlsx_accepted_match_updates_in_place_and_undoes_as_one_step() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "ri1_acc");
    let mut dec = KeyDecisions::default();
    dec.accept.insert(("Rooms".into(), 2));
    let rep = import_xlsx_keyed(&mut m, &p, opts(true), &dec).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!((room_count(&m), room_name(&m, ids[0]).as_str()), (1, "Lobby"));
    m.undo().unwrap();
    assert_eq!(room_name(&m, ids[0]), "Hall");
}

// --- a rejected key match keeps its element ---

fn opts_del(apply: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing: true, new_params: false }
}

fn rejected(sheet: &str) -> KeyDecisions {
    let mut dec = KeyDecisions::default();
    dec.reject.insert((sheet.into(), 2));
    dec
}

fn accepted(sheet: &str) -> KeyDecisions {
    let mut dec = KeyDecisions::default();
    dec.accept.insert((sheet.into(), 2));
    dec
}

/// Model with one curtain wall named "CW"; returns its id.
fn curtain_model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let sec = ddc_model::MullionSection::Rect { w: 0.05, d: 0.1 };
    let id = m
        .add_curtain_wall(&ddc_model::CurtainSpec {
            level: lv,
            name: "CW".into(),
            a: (0.0, 6.0),
            b: (4.0, 6.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: ddc_model::GridLines::Spacing(1.0),
            v: ddc_model::GridLines::Spacing(1.5),
            interior: sec,
            border: sec,
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    (m, id)
}

/// Book where the curtain wall row lost its id (the name still matches) and got a new base
/// offset; its height is locked on an existing curtain wall (6e97c283), the offset is not.
fn curtain_lost_id_book(m: &Model, tag: &str) -> std::path::PathBuf {
    let src = tmp(&format!("{tag}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("{tag}_dst"));
    edit(&src, &dst, |s, g: &mut Grid| {
        if s == CURTAINS {
            set(g, 1, "id", Data::Empty);
            set(g, 1, "base_offset [m]", Data::Float(0.5));
        }
    });
    dst
}

fn curtain_count(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM curtain_wall", [], |r| r.get(0)).unwrap()
}

fn curtain_base(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT base_offset FROM curtain_wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn deletes(rep: &Report, id: i64) -> bool {
    rep.changes.iter().any(|c| c.id == Some(id) && c.new == MISSING)
}

fn kept_note<'a>(rep: &'a Report, sheet: &str) -> Option<&'a Ignored> {
    rep.ignored
        .iter()
        .find(|i| i.sheet == sheet && i.row == 2 && i.reason == super::msg::RI1_KEPT.1)
}

#[test]
fn rejected_match_not_deleted_with_delete_missing() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "xl28_rdel");
    let pre = import_xlsx_keyed(&mut m, &p, opts_del(false), &rejected("Rooms")).unwrap();
    assert!(!deletes(&pre, ids[0]), "preview: {:?}", pre.changes);
    let rep = import_xlsx_keyed(&mut m, &p, opts_del(true), &rejected("Rooms")).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!(!deletes(&rep, ids[0]), "{:?}", rep.changes);
    assert_eq!(room_count(&m), 2, "the old room stays, the row adds a new one");
    assert_eq!(room_name(&m, ids[0]), "Hall", "the kept room is not changed");

    let (mut m, cw) = curtain_model();
    let p = curtain_lost_id_book(&m, "xl28_cdel");
    let pre = import_xlsx_keyed(&mut m, &p, opts_del(false), &rejected(CURTAINS)).unwrap();
    assert!(!deletes(&pre, cw), "preview: {:?}", pre.changes);
    let rep = import_xlsx_keyed(&mut m, &p, opts_del(true), &rejected(CURTAINS)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!(!deletes(&rep, cw), "{:?}", rep.changes);
    assert_eq!(curtain_count(&m), 2, "the old curtain wall stays, the row adds a new one");
    assert_eq!(curtain_base(&m, cw), 0.0, "the kept curtain wall is not changed");
}

#[test]
fn rejected_match_listed_as_kept() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "xl28_rkept");
    let rep = import_xlsx_keyed(&mut m, &p, opts_del(false), &rejected("Rooms")).unwrap();
    assert_eq!(rep.matches.len(), 1, "{:?}", rep.matches);
    assert_eq!(rep.matches[0].state, MatchState::Rejected);
    let k = kept_note(&rep, "Rooms").unwrap_or_else(|| panic!("{:?}", rep.ignored));
    assert_eq!((k.id, k.new.as_str()), (Some(ids[0]), "101"));
    assert_eq!(rep.counts().added, 1);
    let n = rep.ignored.iter().filter(|i| i.reason == super::msg::RI1_KEPT.1).count();
    assert_eq!(n, 1, "one note per rejected row");

    let (mut m, cw) = curtain_model();
    let p = curtain_lost_id_book(&m, "xl28_ckept");
    let rep = import_xlsx_keyed(&mut m, &p, opts_del(false), &rejected(CURTAINS)).unwrap();
    assert_eq!(rep.matches.len(), 1, "{:?}", rep.matches);
    assert_eq!(rep.matches[0].state, MatchState::Rejected);
    let k = kept_note(&rep, CURTAINS).unwrap_or_else(|| panic!("{:?}", rep.ignored));
    assert_eq!((k.id, k.new.as_str()), (Some(cw), "CW"));
    assert_eq!(rep.counts().added, 1);
}

#[test]
fn accepted_match_still_updates_in_place() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "xl28_racc");
    let rep = import_xlsx_keyed(&mut m, &p, opts_del(true), &accepted("Rooms")).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!(!deletes(&rep, ids[0]), "{:?}", rep.changes);
    assert!(kept_note(&rep, "Rooms").is_none(), "{:?}", rep.ignored);
    assert_eq!((room_count(&m), room_name(&m, ids[0]).as_str()), (1, "Lobby"));
    m.undo().unwrap();
    assert_eq!(room_name(&m, ids[0]), "Hall");

    let (mut m, cw) = curtain_model();
    let p = curtain_lost_id_book(&m, "xl28_cacc");
    let rep = import_xlsx_keyed(&mut m, &p, opts_del(true), &accepted(CURTAINS)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!(!deletes(&rep, cw), "{:?}", rep.changes);
    assert!(kept_note(&rep, CURTAINS).is_none(), "{:?}", rep.ignored);
    assert_eq!(curtain_count(&m), 1);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(curtain_base(&m, cw), 0.5);
}

#[test]
fn ri1_reasons_localized_ru() {
    let cat = |src: &str| -> std::collections::BTreeMap<String, String> {
        serde_json::from_str(src).expect("locale file is valid JSON")
    };
    let en = cat(include_str!("../../../ui/data/i18n/en/xlsx_confirm.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xlsx_confirm.json"));
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c));
    for (key, tpl) in super::msg::RI1_REASONS {
        assert_eq!(en.get(key).map(String::as_str), Some(tpl), "{key}: en text is the template");
        let r = ru.get(key).unwrap_or_else(|| panic!("{key}: no ru entry"));
        assert!(cyr(r) && r != tpl, "{key}: ru text is not Russian: {r}");
        assert_eq!(tpl.matches("{}").count(), r.matches("{}").count(), "{key}");
    }
    // reasons the import really writes render in Russian through their template
    let render = |text: &str| -> String {
        let (key, tpl) = super::msg::RI1_REASONS
            .into_iter()
            .find(|(_, t)| match_template(t, text).is_some())
            .unwrap_or_else(|| panic!("no ri1 template for {text:?}"));
        fill_template(&ru[key], &match_template(tpl, text).unwrap())
    };
    let (mut m, _) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "xl28_ru_kept");
    let rep = import_xlsx_keyed(&mut m, &p, opts(false), &rejected("Rooms")).unwrap();
    let kept = &kept_note(&rep, "Rooms").expect("kept note").reason;
    assert_eq!(render(kept), ru[super::msg::RI1_KEPT.0]);
    let (mut m, _) = model(&[("Hall", "101"), ("Annex", "101")]);
    let p = lost_id_book(&m, "xl28_ru_many");
    let rep = import_xlsx_keyed(&mut m, &p, opts(false), &KeyDecisions::default()).unwrap();
    let many = rep
        .ignored
        .iter()
        .find(|i| match_template(super::msg::RI1_MANY.1, &i.reason).is_some())
        .unwrap_or_else(|| panic!("{:?}", rep.ignored));
    let ru_many = render(&many.reason);
    assert!(cyr(&ru_many) && ru_many.contains('2') && !ru_many.contains("{}"), "{ru_many}");
}

#[test]
fn rejected_match_keeps_the_row_listed_and_import_is_unchanged() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "ux9_keep");
    let rep = import_xlsx_keyed(&mut m, &p, opts(false), &rejected("Rooms")).unwrap();
    assert_eq!(rep.matches.len(), 1);
    let km = &rep.matches[0];
    assert_eq!((km.id, km.state, km.row), (ids[0], MatchState::Rejected, 2));
    assert!(km.changes.is_empty());
    assert_eq!((rep.pending_matches(), rep.accepted_matches()), (0, 0));
    assert_eq!(rep.counts().added, 1);
    // applying: same outcome as the former skip (a new room, the old one untouched)
    let rep = import_xlsx_keyed(&mut m, &p, opts(true), &rejected("Rooms")).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!((room_count(&m), room_name(&m, ids[0]).as_str()), (2, "Hall"));
}

#[test]
fn reject_then_accept_and_accept_then_reject() {
    let (mut m, ids) = model(&[("Hall", "101")]);
    let p = lost_id_book(&m, "ux9_flip");
    let mut dec = rejected("Rooms");
    let r1 = import_xlsx_keyed(&mut m, &p, opts(false), &dec).unwrap();
    assert_eq!(r1.matches[0].state, MatchState::Rejected);
    // accept: the reject entry is dropped, the row moves back to the match
    dec.reject.clear();
    dec.accept.insert(("Rooms".into(), 2));
    let r2 = import_xlsx_keyed(&mut m, &p, opts(false), &dec).unwrap();
    assert_eq!((r2.matches.len(), r2.matches[0].state), (1, MatchState::Accepted));
    assert_eq!((r2.counts().added, r2.accepted_matches()), (0, 1));
    // reject again
    dec.accept.clear();
    dec.reject.insert(("Rooms".into(), 2));
    let r3 = import_xlsx_keyed(&mut m, &p, opts(false), &dec).unwrap();
    assert_eq!((r3.matches.len(), r3.matches[0].state), (1, MatchState::Rejected));
    assert_eq!(r3.counts().added, 1);
    assert_eq!(room_name(&m, ids[0]), "Hall");
}
