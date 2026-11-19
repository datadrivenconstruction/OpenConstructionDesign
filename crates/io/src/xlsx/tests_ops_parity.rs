// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! XLSX batch through the wire envelope (`Model::apply_envelope`) against the legacy
//! `apply_edits` path: same final state, one undo step, and the edit kinds the envelope
//! executor still refuses, listed by name.

use calamine::Data;
use ddc_model::{
    Edit, LayerSpec, MemberShape, Model, OpeningSpec, PVal, ParamSpec, ParamType, RoomSpec,
    SlabSpec, Swing,
};
use ddc_ops::Cat;
use ddc_ops::Code;
use ddc_ops::Ref;

use super::tests::{edit, set, tmp};
use super::tests_ops::{build, count, dump, edited_book, preview};
use super::*;
use ddc_geom::member::Just;
use ddc_model::{BeamJoin, BeamSpec, ColumnSpec};

/// Ids of the rows the cases edit.
struct Fx {
    l1: i64,
    ext: i64,
    int: i64,
    col: i64,
    beam: i64,
    opening: i64,
    room: i64,
    slab: i64,
    slab_type: i64,
}

fn id_of(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn param(name: &str, ptype: ParamType, per_type: bool, cats: &[&str]) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype,
        per_type,
        unit: String::new(),
        group: "Data".into(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

/// `build()` plus a second wall type, a column, a beam, an opening, a room, a slab and the
/// project parameters the book carries (Mark, Comments, a length) and one per-type parameter.
fn fixture() -> (Model, Fx) {
    let mut m = build();
    let l1 = id_of(&m, "SELECT id FROM level WHERE name='L1'");
    let ext = id_of(&m, "SELECT id FROM wall_type WHERE name='ext'");
    let l2 = m.add_level("L2", 3.0).unwrap();
    let int = m
        .add_wall_type("int", &[LayerSpec { material: "gypsum".into(), thickness: 0.1 }])
        .unwrap();
    let mt = m
        .add_member_type("C300", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete")
        .unwrap();
    let col = m
        .add_column(&ColumnSpec {
            level: l1,
            top_level: Some(l2),
            member_type: mt,
            structural: true,
            at: (1.0, 1.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    let beam = m
        .add_beam(&BeamSpec {
            level: l2,
            member_type: mt,
            a: (0.0, 2.0, 0.0),
            b: (5.0, 2.0, 0.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    let opening = m
        .add_opening(
            1,
            &OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() },
        )
        .unwrap();
    let room = m
        .add_room(&RoomSpec {
            level: l1,
            name: "Hall".into(),
            number: "101".into(),
            point: (2.5, 2.0),
            height: 3.0,
        })
        .unwrap();
    let slab_type = m
        .add_slab_type("floor", &[LayerSpec { material: "concrete".into(), thickness: 0.2 }])
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: l1,
            slab_type,
            offset: 0.0,
            outer: vec![(0., 0.), (5., 0.), (5., 4.), (0., 4.)],
            holes: vec![],
        })
        .unwrap();
    m.add_param(&param("Mark", ParamType::Text, false, &["wall", "col", "beam"])).unwrap();
    m.add_param(&param("Comments", ParamType::Text, false, &["wall", "col", "beam"])).unwrap();
    m.add_param(&param("Fire offset", ParamType::Length, false, &["wall", "col", "beam"]))
        .unwrap();
    m.add_param(&param("Cost", ParamType::Number, true, &["wall"])).unwrap();
    (m, Fx { l1, ext, int, col, beam, opening, room, slab, slab_type })
}

/// Bookkeeping that is not model data: the journal, counters and caches.
fn bookkeeping(t: &str) -> bool {
    t == "op"
        || t == "meta"
        || t.starts_with("op_")
        || t.ends_with("_cache")
        || t.starts_with("change")
        || t.starts_with("journal")
}

/// A `dump()` row holds `s` as a whole text cell (cells print as `Text([bytes])`).
fn holds_text(row: &str, s: &str) -> bool {
    row.contains(&format!("Text({:?})", s.as_bytes()))
}

/// Recomputed tables: one batch and one op at a time may number their rows differently.
fn derived(t: &str) -> bool {
    t.ends_with("_qty")
        || ["geom_join", "struct_issue", "shaft_cut", "solid_cut", "solid_base", "dim_geo"]
            .contains(&t)
}

/// `dump()` without bookkeeping; derived rows compared without their row id.
fn state(m: &Model) -> Vec<String> {
    let mut v: Vec<String> = dump(m)
        .into_iter()
        .filter_map(|row| {
            let (t, rest) = row.split_once(':')?;
            if bookkeeping(t) {
                return None;
            }
            if derived(t) {
                let tail = rest.split_once('|').map_or("", |x| x.1);
                return Some(format!("{t}:{tail}"));
            }
            Some(row)
        })
        .collect();
    v.sort();
    v
}

/// The book edits (wall height and type, Mark, Comments, a length parameter) read by the
/// preview, plus the column and beam edits the book has no sheet for.
fn parity_edits(m: &mut Model, fx: &Fx, name: &str) -> Vec<Edit> {
    let src = tmp(&format!("{name}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("{name}_dst"));
    edit(&src, &dst, |sheet, g| {
        if sheet == WALLS {
            for r in 1..g.len() {
                set(g, r, "height [m]", Data::Float(3.5));
                set(g, r, "Mark", Data::String(format!("W-{r}")));
            }
            set(g, 2, "type", Data::String("int".into()));
            set(g, 1, "Comments", Data::String("checked".into()));
            set(g, 3, "Fire offset [m]", Data::Float(0.05));
        }
    });
    let rep = preview(m, &dst);
    assert!(rep.rejected.is_empty(), "book rejected {} cells", rep.rejected.len());
    let mut v = rep.batch();
    assert!(v.iter().filter(|e| matches!(e, Edit::Wall { .. })).count() >= 4, "{v:?}");
    assert!(v.iter().filter(|e| matches!(e, Edit::ParamValue { .. })).count() >= 6, "{v:?}");

    let mut c = m.column(fx.col).unwrap();
    c.base_offset = 0.2;
    c.top_offset = -0.1;
    let mut b = m.beam(fx.beam).unwrap();
    b.level = fx.l1;
    v.push(Edit::Column { id: fx.col, spec: c });
    v.push(Edit::Beam { id: fx.beam, spec: b });
    v.push(Edit::ParamValue {
        param: "Mark".into(),
        kind: "col".into(),
        id: fx.col,
        value: Some(PVal::Text("C-1".into())),
    });
    v.push(Edit::ParamValue {
        param: "Comments".into(),
        kind: "beam".into(),
        id: fx.beam,
        value: Some(PVal::Text("moved to L1".into())),
    });
    v.push(Edit::ParamValue {
        param: "Fire offset".into(),
        kind: "beam".into(),
        id: fx.beam,
        value: Some(PVal::Num(0.15)),
    });
    v
}

#[test]
fn xlsx_envelope_same_state_as_legacy() {
    let (mut a, fx) = fixture();
    let (mut b, _) = fixture();
    let before = state(&a);
    assert_eq!(state(&b), before, "fixtures differ");
    let edits = parity_edits(&mut a, &fx, "parity_same");
    assert_eq!(state(&a), before, "preview changed the model");

    a.apply_edits(&edits).unwrap();
    let legacy = state(&a);
    assert_ne!(legacy, before);

    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    let receipt = b.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    for (cat, id) in [(Cat::Wall, 1), (Cat::Wall, 2), (Cat::Column, fx.col), (Cat::Beam, fx.beam)] {
        assert!(receipt.changed.contains(&Ref::id(cat, id)), "{cat:?} {id}: {receipt:?}");
    }
    assert!(receipt.created.is_empty() && receipt.deleted.is_empty(), "{receipt:?}");
    let via_envelope = state(&b);
    if via_envelope != legacy {
        let only_a: Vec<&String> = legacy.iter().filter(|r| !via_envelope.contains(r)).collect();
        let only_b: Vec<&String> = via_envelope.iter().filter(|r| !legacy.contains(r)).collect();
        panic!("legacy only: {only_a:#?}\nenvelope only: {only_b:#?}");
    }
    let wtype = id_of(&b, "SELECT wtype FROM wall WHERE id=2");
    assert_eq!(wtype, fx.int);
}

#[test]
fn xlsx_envelope_one_undo_step() {
    let (mut m, fx) = fixture();
    let edits = parity_edits(&mut m, &fx, "parity_undo");
    let before = state(&m);
    let ops = count(&m, "op");
    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    assert!(adapted.envelope.op_count() > 1);
    let receipt = m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    assert_ne!(state(&m), before);
    // the whole XLSX envelope is one journal row
    assert_eq!(count(&m, "op"), ops + 1);
    assert!(receipt.journal_seq > 0);
    m.undo().unwrap();
    assert_eq!(state(&m), before);
    assert_eq!(count(&m, "op"), ops);
}

/// One XLSX row edit through the envelope gives the same state as the legacy path.
fn same_as_legacy(name: &str, cat: Cat, id: i64, edits: &[Edit]) {
    let (mut legacy, _) = fixture();
    let before = state(&legacy);
    legacy.apply_edits(edits).unwrap_or_else(|e| panic!("{name}: legacy refused: {e}"));
    let want = state(&legacy);
    assert_ne!(want, before, "{name}: the edit changes nothing");

    let adapted = edits_to_envelope(edits, "book.xlsx");
    assert!(adapted.complete(), "{name}: {:?}", adapted.refused);
    let (mut m, _) = fixture();
    let receipt = m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{name}: {r}"));
    assert!(receipt.changed.contains(&Ref::id(cat, id)), "{name}: {receipt:?}");
    let got = state(&m);
    if got != want {
        let only_a: Vec<&String> = want.iter().filter(|r| !got.contains(r)).collect();
        let only_b: Vec<&String> = got.iter().filter(|r| !want.contains(r)).collect();
        panic!("{name}: legacy only: {only_a:#?}\nenvelope only: {only_b:#?}");
    }
    m.undo().unwrap();
    assert_eq!(state(&m), before, "{name}: undo");
}

#[test]
fn set_slab_offset_matches_legacy() {
    let (_, fx) = fixture();
    same_as_legacy(
        "slab",
        Cat::Slab,
        fx.slab,
        &[Edit::Slab { id: fx.slab, level: fx.l1, slab_type: fx.slab_type, offset: 0.1 }],
    );
}

#[test]
fn set_opening_width_matches_legacy() {
    let (_, fx) = fixture();
    let spec = OpeningSpec { along: 1.5, width: 1.2, sill: 0.0, height: 2.1, kind: "door".into() };
    // the row writes kind=door: the receipt names the category it now has (5def7a71)
    same_as_legacy("opening", Cat::Door, fx.opening, &[Edit::Opening { id: fx.opening, wall: 1, spec }]);
}

#[test]
fn set_opening_swing_matches_legacy() {
    let (_, fx) = fixture();
    same_as_legacy(
        "opening swing",
        Cat::Opening,
        fx.opening,
        &[Edit::OpeningSwing { id: fx.opening, swing: Swing { side: true, hinge: true } }],
    );
}

#[test]
fn set_room_name_number_matches_legacy() {
    let (_, fx) = fixture();
    let spec = RoomSpec {
        level: fx.l1,
        name: "Lobby".into(),
        number: "102".into(),
        point: (2.5, 2.0),
        height: 3.0,
    };
    same_as_legacy("room", Cat::Room, fx.room, &[Edit::Room { id: fx.room, spec }]);
}

/// Edit kinds the XLSX import produces that `apply_envelope` does not lower yet. Shrink this
/// list as the executor learns them; the test fails when a kind starts to apply.
const ENVELOPE_GAPS: &[&str] = &["new_param"];

/// A wall type rename and a layer edit go through `type.rename` / `type.set` like the legacy batch.
#[test]
fn xlsx_wall_type_and_layer_same_state_as_legacy() {
    let (_, fx) = fixture();
    let layer = LayerSpec { material: "stone".into(), thickness: 0.25 };
    let edits =
        vec![Edit::WallType { id: fx.ext, name: "ext2".into() }, Edit::Layer { wtype: fx.ext, pos: 0, layer }];
    for one in [&edits[..1], &edits[1..], &edits[..]] {
        let (mut a, _) = fixture();
        let (mut b, _) = fixture();
        a.apply_edits(one).unwrap();
        let adapted = edits_to_envelope(one, "book.xlsx");
        assert!(adapted.complete(), "{:?}", adapted.refused);
        let receipt = b.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
        assert!(receipt.changed.contains(&Ref::id(Cat::WallType, fx.ext)), "{receipt:?}");
        assert_eq!(state(&b), state(&a));
    }
}

/// The stock edited book (wall heights plus a layer thickness) applies like the legacy batch.
#[test]
fn xlsx_edited_book_with_layers_same_state_as_legacy() {
    let (mut a, _) = fixture();
    let (mut b, _) = fixture();
    let book = edited_book(&a, "parity_book_layers");
    let rep = preview(&mut a, &book);
    a.apply_edits(&rep.batch()).unwrap();
    let adapted = preview(&mut b, &book).to_envelope("book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    b.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    assert_eq!(state(&b), state(&a));
}

/// A type parameter set through a typed `wtype:` ref lands like the legacy batch.
#[test]
fn xlsx_type_param_same_state_as_legacy() {
    let (mut a, fx) = fixture();
    let (mut b, _) = fixture();
    let edits = vec![Edit::ParamValue {
        param: "Cost".into(),
        kind: "wall_type".into(),
        id: fx.ext,
        value: Some(PVal::Num(12.0)),
    }];
    a.apply_edits(&edits).unwrap();
    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    let receipt = b.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    assert!(receipt.changed.contains(&Ref::id(Cat::WallType, fx.ext)), "{receipt:?}");
    assert_eq!(state(&b), state(&a));
}

#[test]
fn xlsx_envelope_unsupported_kinds_listed() {
    let cases: Vec<(&str, Vec<Edit>)> = vec![
        ("new_param", vec![Edit::NewParam(param("Phase note", ParamType::Text, false, &["wall"]))]),
    ];

    let mut gaps: Vec<String> = Vec::new();
    for (name, edits) in &cases {
        let adapted = edits_to_envelope(edits, "book.xlsx");
        assert!(adapted.complete(), "{name}: the adapter must map it: {:?}", adapted.refused);

        // the legacy path takes the edit, so the refusal is an envelope gap, not a bad edit
        let (mut legacy, _) = fixture();
        legacy.apply_edits(edits).unwrap_or_else(|e| panic!("{name}: legacy refused: {e}"));

        let (mut m, _) = fixture();
        let before = dump(&m);
        match m.apply_envelope(&adapted.envelope) {
            Ok(r) => panic!("{name}: now applies ({r:?}); remove it from ENVELOPE_GAPS"),
            Err(r) => {
                assert_eq!(r.code, Code::Unsupported, "{name}: {r}");
                assert!(r.got.as_deref().is_some_and(|g| !g.is_empty()), "{name}: {r}");
                assert_eq!(dump(&m), before, "{name}: a refused envelope changed the model");
                gaps.push((*name).to_owned());
            }
        }
    }

    assert_eq!(gaps, ENVELOPE_GAPS);
}

#[test]
fn adapter_type_refs_typed() {
    let layer = LayerSpec { material: "stone".into(), thickness: 0.25 };
    let pv = |kind: &str| Edit::ParamValue {
        param: "Cost".into(),
        kind: kind.into(),
        id: 7,
        value: Some(PVal::Num(1.0)),
    };
    let edits = vec![
        Edit::WallType { id: 7, name: "ext2".into() },
        Edit::Layer { wtype: 7, pos: 0, layer },
        pv("wall_type"),
        pv("slab_type"),
        pv("member_type"),
    ];
    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    let j = adapted.envelope.to_json().unwrap().replace(' ', "");
    for cat in ["\"wtype\"", "\"stype\"", "\"mtype\""] {
        assert!(j.contains(&format!("\"cat\":{cat}")), "{cat} missing: {j}");
    }
    assert!(!j.contains("\"cat\":\"type\""), "bare v1 type ref under v2: {j}");
}

fn new_wall_with_param(fx: &Fx, row: u32, text: &str) -> Vec<Edit> {
    vec![
        Edit::NewWall {
            spec: ddc_model::WallSpec {
                a: (0., 6.),
                b: (4., 6.),
                level: fx.l1,
                base_offset: 0.0,
                height: 3.0,
                wall_type: fx.ext,
                offset0: None,
            },
            bulge: 0.0,
        },
        Edit::Mark { kind: "wall".into(), row },
        Edit::ParamValueNew {
            param: "Comments".into(),
            kind: "wall".into(),
            row,
            value: Some(PVal::Text(text.into())),
        },
    ]
}

#[test]
fn new_row_param_set_on_created_slot() {
    let (_, fx) = fixture();
    let edits = new_wall_with_param(&fx, 7, "W-note-xl13c");
    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    // Mark itself produces no op
    let ops: Vec<(usize, &ddc_ops::Op)> = adapted
        .envelope
        .groups
        .iter()
        .enumerate()
        .flat_map(|(gi, g)| g.ops.iter().map(move |o| (gi, o)))
        .collect();
    let create = ops.iter().position(|(_, o)| o.slot().is_some()).expect("no Create");
    let slot = ops[create].1.slot().unwrap();
    let set = ops
        .iter()
        .position(|(_, o)| matches!(o, ddc_ops::Op::Set { .. }))
        .expect("no Set");
    assert!(set > create, "Set before Create");
    let tag = format!("{:?}", Ref::slot(slot));
    assert!(format!("{:?}", ops[set].1).contains(&tag), "Set not on slot {slot}: {:?}", ops[set].1);
    // the Set group comes after the Create group and depends on the group before it
    let g = &adapted.envelope.groups;
    assert!(ops[set].0 > ops[create].0);
    assert!(g[ops[set].0].depends_on.contains(&(ops[set].0 as u32)));

    let (mut m, _) = fixture();
    let receipt = m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    assert_eq!(receipt.created.len(), 1, "{receipt:?}");
    assert!(state(&m).iter().any(|r| holds_text(r, "W-note-xl13c")), "value not stored");
}

#[test]
fn new_row_param_matches_legacy() {
    let (mut legacy, fx) = fixture();
    let before = state(&legacy);
    let edits = new_wall_with_param(&fx, 7, "W-note-xl13c");
    legacy.apply_edits(&edits).unwrap();
    let want = state(&legacy);
    assert!(want.iter().any(|r| holds_text(r, "W-note-xl13c")), "{want:#?}");

    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    let (mut m, _) = fixture();
    m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    let got = state(&m);
    if got != want {
        let only_a: Vec<&String> = want.iter().filter(|r| !got.contains(r)).collect();
        let only_b: Vec<&String> = got.iter().filter(|r| !want.contains(r)).collect();
        panic!("legacy only: {only_a:#?}\nenvelope only: {only_b:#?}");
    }
    m.undo().unwrap();
    assert_eq!(state(&m), before, "undo");
}

#[test]
fn new_row_param_without_mark_refused() {
    let (_, fx) = fixture();
    let mut edits = new_wall_with_param(&fx, 7, "x");
    edits.remove(1); // drop the Mark
    let adapted = edits_to_envelope(&edits, "book.xlsx");
    assert!(!adapted.complete());
    assert_eq!(adapted.refused.len(), 1, "{:?}", adapted.refused);
    assert_eq!(adapted.refused[0].edit, "param_value_new");
    assert!(adapted.refused[0].reason.contains("row 7"), "{:?}", adapted.refused[0]);

    // a mark with no new row before it is refused too, and the later value follows
    let lone = vec![
        Edit::Mark { kind: "wall".into(), row: 3 },
        Edit::ParamValueNew {
            param: "Comments".into(),
            kind: "wall".into(),
            row: 3,
            value: Some(PVal::Text("y".into())),
        },
    ];
    let a = edits_to_envelope(&lone, "book.xlsx");
    assert_eq!(a.refused.len(), 2, "{:?}", a.refused);
    assert_eq!(a.refused[0].edit, "mark");
}

/// One new XLSX row through the envelope: the same rows as the legacy path, the receipt
/// names the created row with `cat`, one undo removes it.
fn new_same_as_legacy(name: &str, cat: Cat, edits: &[Edit]) {
    let (mut legacy, _) = fixture();
    let before = state(&legacy);
    legacy.apply_edits(edits).unwrap_or_else(|e| panic!("{name}: legacy refused: {e}"));
    let want = state(&legacy);
    assert_ne!(want, before, "{name}: the edit changes nothing");

    let adapted = edits_to_envelope(edits, "book.xlsx");
    assert!(adapted.complete(), "{name}: {:?}", adapted.refused);
    let (mut m, _) = fixture();
    let receipt = m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{name}: {r}"));
    assert_eq!(receipt.created.len(), 1, "{name}: {receipt:?}");
    assert!(matches!(receipt.created[0].1, Ref::Id { cat: c, .. } if c == cat), "{name}: {receipt:?}");
    let got = state(&m);
    if got != want {
        let only_a: Vec<&String> = want.iter().filter(|r| !got.contains(r)).collect();
        let only_b: Vec<&String> = got.iter().filter(|r| !want.contains(r)).collect();
        panic!("{name}: legacy only: {only_a:#?}\nenvelope only: {only_b:#?}");
    }
    m.undo().unwrap();
    assert_eq!(state(&m), before, "{name}: undo");
}

/// A new XLSX row the envelope refuses: the stated code, the model as it was.
fn new_refused(name: &str, edits: &[Edit], code: Code) {
    let adapted = edits_to_envelope(edits, "book.xlsx");
    assert!(adapted.complete(), "{name}: {:?}", adapted.refused);
    let (mut m, _) = fixture();
    let (before, ops) = (dump(&m), count(&m, "op"));
    let r = m.apply_envelope(&adapted.envelope).unwrap_err();
    assert_eq!(r.code, code, "{name}: {r}");
    assert_eq!(dump(&m), before, "{name}: a refused envelope changed the model");
    assert_eq!(count(&m, "op"), ops, "{name}: journal");
}

fn new_opening(wall: i64, along: f64, width: f64, sill: f64, height: f64, kind: &str, swing: Swing) -> Edit {
    Edit::NewOpening { wall, spec: OpeningSpec { along, width, sill, height, kind: kind.into() }, swing }
}

#[test]
fn xlsx_new_door_row_same_state_as_legacy() {
    let door = new_opening(1, 2.5, 1.0, 0.0, 2.0, "door", Swing::default());
    new_same_as_legacy("new door", Cat::Door, &[door]);
}

#[test]
fn xlsx_new_door_sill_kept_same_state_as_legacy() {
    let door = new_opening(1, 2.5, 1.0, 0.3, 1.8, "door", Swing::default());
    new_same_as_legacy("new door sill", Cat::Door, &[door.clone()]);
    let (mut m, _) = fixture();
    let adapted = edits_to_envelope(&[door], "book.xlsx");
    m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    let sill: f64 = m.sql().query_row("SELECT sill FROM opening ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    assert!((sill - 0.3).abs() < 1e-9, "sill {sill}");
}

#[test]
fn xlsx_new_door_zero_sill_stays_zero() {
    let door = new_opening(1, 2.5, 1.0, 0.0, 2.0, "door", Swing::default());
    let adapted = edits_to_envelope(&[door], "book.xlsx");
    let json = serde_json::to_string(&adapted.envelope).unwrap();
    assert!(!json.contains("\"sill\""), "{json}");
    let (mut m, _) = fixture();
    m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    let sill: f64 = m.sql().query_row("SELECT sill FROM opening ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(sill, 0.0);
}

#[test]
fn xlsx_new_window_sill_kept() {
    let window = new_opening(2, 1.0, 1.5, 0.9, 1.0, "window", Swing::default());
    let (mut m, _) = fixture();
    let adapted = edits_to_envelope(&[window], "book.xlsx");
    m.apply_envelope(&adapted.envelope).unwrap_or_else(|r| panic!("{r}"));
    let sill: f64 = m.sql().query_row("SELECT sill FROM opening ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    assert!((sill - 0.9).abs() < 1e-9, "sill {sill}");
}

#[test]
fn xlsx_new_door_negative_sill_refused_model_unchanged() {
    let door = new_opening(1, 2.5, 1.0, -0.2, 2.0, "door", Swing::default());
    new_refused("door negative sill", &[door], Code::Invalid);
}

#[test]
fn xlsx_new_window_row_same_state_as_legacy() {
    let window = new_opening(2, 1.0, 1.5, 0.75, 1.25, "window", Swing::default());
    new_same_as_legacy("new window", Cat::Window, &[window]);
}

#[test]
fn envelope_new_door_swing_applied() {
    let door = new_opening(1, 2.5, 1.0, 0.0, 2.0, "door", Swing { side: true, hinge: true });
    new_same_as_legacy("new door swing", Cat::Door, &[door.clone()]);
    let (mut m, _) = fixture();
    let r = m.apply_envelope(&edits_to_envelope(&[door], "book.xlsx").envelope).unwrap_or_else(|r| panic!("{r}"));
    let Ref::Id { id, .. } = r.created[0].1 else { panic!("{r:?}") };
    let (side, hinge): (bool, bool) = m
        .sql()
        .query_row("SELECT swing_side, swing_hinge FROM opening WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert!(side && hinge);
}

#[test]
fn envelope_new_door_off_wall_refused_model_unchanged() {
    // wall 1 is 5 m long: a 1 m door from 4.5 m runs past its end
    let door = new_opening(1, 4.5, 1.0, 0.0, 2.0, "door", Swing::default());
    new_refused("door off wall", &[door], Code::OutsideHost);
    // a door taller than the 3 m wall
    let tall = new_opening(1, 2.5, 1.0, 0.0, 3.5, "door", Swing::default());
    new_refused("door above wall", &[tall], Code::OutsideHost);
}

#[test]
fn xlsx_new_room_row_same_state_as_legacy() {
    let (_, fx) = fixture();
    let spec = RoomSpec { level: fx.l1, name: "Store".into(), number: "102".into(), point: (1.5, 1.0), height: 2.75 };
    new_same_as_legacy("new room", Cat::Room, &[Edit::NewRoom(spec.clone())]);
    // a blank number stays blank
    new_same_as_legacy("new room no number", Cat::Room, &[Edit::NewRoom(RoomSpec { number: String::new(), ..spec })]);
}

#[test]
fn xlsx_new_slab_row_same_state_as_legacy() {
    let (_, fx) = fixture();
    let spec = SlabSpec {
        level: fx.l1,
        slab_type: fx.slab_type,
        offset: -0.25,
        outer: vec![(6., 0.), (8., 0.), (8., 3.), (6., 3.)],
        holes: vec![vec![(6.5, 0.5), (7.0, 0.5), (7.0, 1.0), (6.5, 1.0)]],
    };
    new_same_as_legacy("new slab", Cat::Slab, &[Edit::NewSlab(spec)]);
}

#[test]
fn envelope_new_slab_open_boundary_refused_model_unchanged() {
    let (_, fx) = fixture();
    let spec = SlabSpec {
        level: fx.l1,
        slab_type: fx.slab_type,
        offset: 0.0,
        outer: vec![(6., 0.), (8., 0.)],
        holes: vec![],
    };
    new_refused("open slab", &[Edit::NewSlab(spec)], Code::Invalid);
}

#[test]
fn xlsx_new_column_row_same_state_as_legacy() {
    let (m, fx) = fixture();
    let l2 = id_of(&m, "SELECT id FROM level WHERE name='L2'");
    let mt = id_of(&m, "SELECT id FROM member_type WHERE name='C300'");
    let to_l2 = ColumnSpec {
        level: fx.l1,
        top_level: Some(l2),
        member_type: mt,
        structural: true,
        at: (4.0, 3.0),
        base_offset: 0.25,
        top_offset: -0.5,
        rotation: 0.5,
        top_at: None,
    };
    new_same_as_legacy("new column to L2", Cat::Column, &[Edit::NewColumn(to_l2.clone())]);
    // no top level: the top is the base level plus `top_offset`; a slanted top
    let own = ColumnSpec { top_level: None, top_offset: 2.5, structural: false, top_at: Some((4.5, 3.0)), ..to_l2 };
    new_same_as_legacy("new column on L1", Cat::Column, &[Edit::NewColumn(own)]);
}
