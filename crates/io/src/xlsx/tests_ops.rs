//! XLSX batch -> wire envelope adapter (actor Xlsx, one envelope, one undo).

use std::collections::BTreeSet;
use std::path::PathBuf;

use calamine::Data;
use ddc_geom::member::Just;
use ddc_model::{
    BeamJoin, BeamSpec, ColumnSpec, CurtainSpec, Edit, GridLines, LayerSpec, Model,
    MullionSection, OpeningSpec, PVal, ParamSpec, ParamType, RoomSpec, SlabSpec, Swing, WallSpec,
};
use ddc_ops::{Actor, Envelope, Op};

use super::tests::{edit, set, tmp};
use super::*;

pub(super) fn build() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layers = [("brick", 0.2), ("insulation", 0.1)].map(|(n, t)| LayerSpec {
        material: n.into(),
        thickness: t,
    });
    let t = m.add_wall_type("ext", &layers).unwrap();
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
    m
}

/// Every row of every table, sorted: a byte-level picture of the database.
pub(super) fn dump(m: &Model) -> Vec<String> {
    let mut names: Vec<String> = {
        let mut st = m
            .sql()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(|x| x.unwrap()).collect()
    };
    names.sort();
    let mut out = vec![];
    for t in names {
        let mut st = m.sql().prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = st.column_count();
        let rows = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        let mut v: Vec<String> = rows.map(|x| format!("{t}:{}", x.unwrap())).collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// `dump()` without what differs between two models built alike: the random project uid and
/// the op stamps (2c637a30, 98024f20).
fn content(m: &Model) -> Vec<String> {
    let uid = format!("meta:Text({:?})|", b"project_uid".as_slice());
    let stamp = m
        .sql()
        .prepare("SELECT cid FROM pragma_table_info('op') WHERE name='stamp'")
        .and_then(|mut st| st.query_row([], |r| r.get::<_, i64>(0)))
        .ok()
        .map(|c| c as usize);
    dump(m)
        .into_iter()
        .filter(|r| !r.starts_with(&uid))
        .map(|r| {
            let Some(c) = stamp.filter(|_| r.starts_with("op:")) else {
                return r;
            };
            let mut f: Vec<&str> = r["op:".len()..].split('|').collect();
            if c < f.len() {
                f.remove(c);
            }
            format!("op:{}", f.join("|"))
        })
        .collect()
}

pub(super) fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

pub(super) fn edited_book(m: &Model, name: &str) -> PathBuf {
    let src = tmp(&format!("{name}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("{name}_dst"));
    edit(&src, &dst, |sheet, g| {
        if sheet == WALLS {
            for r in 1..g.len() {
                set(g, r, "height [m]", Data::Float(3.5));
            }
        }
        if sheet == LAYERS {
            set(g, 1, "thickness [m]", Data::Float(0.25));
        }
    });
    dst
}

pub(super) fn preview(m: &mut Model, book: &PathBuf) -> Report {
    let o = ImportOptions { apply: false, delete_missing: false, new_params: false };
    import_xlsx_with(m, book, o).unwrap()
}

#[test]
fn xlsx_import_db_identical_before_after() {
    let mut a = build();
    let book = edited_book(&a, "ops_identical");
    let before = dump(&a);
    let before_content = content(&a);

    // Old path alone.
    assert!(import_xlsx(&mut a, &book).unwrap().applied);
    let via_import = dump(&a);
    assert_ne!(via_import, before);

    // Preview + adapter must not touch the database; applying the same batch afterwards
    // ends in the identical state as the plain import.
    let mut b = build();
    let rep = preview(&mut b, &book);
    let adapted = rep.to_envelope("book.xlsx");
    assert!(adapted.complete(), "{:?}", adapted.refused);
    assert_eq!(content(&b), before_content);
    b.apply_edits(&rep.batch()).unwrap();
    assert_eq!(content(&b), content(&a));
}

#[test]
fn xlsx_envelope_actor_xlsx() {
    let mut m = build();
    let book = edited_book(&m, "ops_actor");
    let rep = preview(&mut m, &book);
    let adapted = rep.to_envelope("book.xlsx");
    assert!(adapted.complete());
    let env = &adapted.envelope;
    assert_eq!(env.actor, Actor::Xlsx);
    assert_eq!(env.v, ddc_ops::OP_VERSION);
    assert_eq!(env.why.as_deref(), Some("xlsx: book.xlsx"));
    assert!(!env.groups.is_empty());
    // one op per edit for these simple batches (no creates), same order
    assert_eq!(env.op_count(), rep.batch().len());
    assert!(env.ops().all(|o| matches!(o, Op::Set { .. } | Op::Type(_))), "{env:?}");
    for (i, g) in env.groups.iter().enumerate() {
        assert_eq!(g.id as usize, i + 1);
        assert_eq!(g.depends_on, if i == 0 { vec![] } else { vec![i as u32] });
    }
    let back = Envelope::from_json(&env.to_json().unwrap()).unwrap();
    assert_eq!(&back, env);
}

#[test]
fn xlsx_undo_one_step() {
    let mut m = build();
    let book = edited_book(&m, "ops_undo");
    let before = dump(&m);
    let ops = count(&m, "op");
    let rep = import_xlsx(&mut m, &book).unwrap();
    assert!(rep.applied);
    // the whole batch (the envelope's worth of edits) is one journal row
    assert!(rep.to_envelope("book.xlsx").envelope.op_count() > 1);
    assert_eq!(count(&m, "op"), ops + 1);
    m.undo().unwrap();
    assert_eq!(dump(&m), before);
    assert_eq!(count(&m, "op"), ops);
}

#[test]
fn envelope_from_sample_sheet_is_deterministic() {
    let mut m = build();
    let book = edited_book(&m, "ops_det");
    let one = preview(&mut m, &book).to_envelope("book.xlsx");
    let two = preview(&mut m, &book).to_envelope("book.xlsx");
    assert_eq!(one, two);
    assert_eq!(one.envelope.to_json().unwrap(), two.envelope.to_json().unwrap());
    let all = sample_edits();
    let a = edits_to_envelope(&all, "s.xlsx");
    let b = edits_to_envelope(&all, "s.xlsx");
    assert_eq!(a.envelope.to_json().unwrap(), b.envelope.to_json().unwrap());
}

fn wall_spec() -> WallSpec {
    WallSpec { a: (0., 0.), b: (4., 0.), level: 1, base_offset: 0.1, height: 3.0, wall_type: 1, offset0: Some(0.05) }
}

fn opening(kind: &str) -> OpeningSpec {
    OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: kind.into() }
}

fn room() -> RoomSpec {
    RoomSpec { level: 1, name: "Hall".into(), number: "101".into(), point: (1., 1.), height: 2.7 }
}

fn slab() -> SlabSpec {
    SlabSpec {
        level: 1,
        slab_type: 1,
        offset: 0.0,
        outer: vec![(0., 0.), (4., 0.), (4., 4.), (0., 4.)],
        holes: vec![vec![(1., 1.), (2., 1.), (2., 2.)]],
    }
}

fn column() -> ColumnSpec {
    ColumnSpec {
        level: 1,
        top_level: Some(2),
        member_type: 1,
        structural: true,
        at: (1., 1.),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.5,
        top_at: None,
    }
}

fn beam() -> BeamSpec {
    BeamSpec {
        level: 1,
        member_type: 1,
        a: (0., 0., 3.),
        b: (4., 0., 3.),
        bulge: 0.0,
        yjust: Just::default(),
        zjust: Just::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    }
}

fn curtain() -> CurtainSpec {
    let sec = MullionSection::Rect { w: 0.05, d: 0.1 };
    CurtainSpec {
        level: 1,
        name: "CW".into(),
        a: (0., 6.),
        b: (4., 6.),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: GridLines::Spacing(1.0),
        v: GridLines::Spacing(1.5),
        interior: sec.clone(),
        border: sec,
        lines: Default::default(),
        top: vec![],
    }
}

fn param(ptype: ParamType) -> ParamSpec {
    ParamSpec {
        name: "Fire".into(),
        ptype,
        per_type: false,
        unit: String::new(),
        group: "Data".into(),
        guid: None,
        categories: vec!["wall".into(), "door".into()],
    }
}

/// One of every `Edit` variant.
fn sample_edits() -> Vec<Edit> {
    vec![
        Edit::Level { id: 1, name: "L1".into(), elevation: 0.0 },
        Edit::NewLevel { name: "L2".into(), elevation: 3.0 },
        Edit::WallType { id: 1, name: "ext".into() },
        Edit::Layer { wtype: 1, pos: 0, layer: LayerSpec { material: "brick".into(), thickness: 0.2 } },
        Edit::Wall { id: 1, spec: wall_spec(), bulge: 0.0 },
        Edit::NewWall { spec: wall_spec(), bulge: 0.0 },
        Edit::Delete { table: "wall", id: 2 },
        Edit::Opening { id: 1, wall: 1, spec: opening("door") },
        Edit::NewOpening { wall: 1, spec: opening("door"), swing: Swing::default() },
        Edit::Delete { table: "opening", id: 1 },
        Edit::OpeningSwing { id: 1, swing: Swing { side: true, hinge: false } },
        Edit::Room { id: 1, spec: room() },
        Edit::NewRoom(room()),
        Edit::DeleteRoom(1),
        Edit::Slab { id: 1, level: 1, slab_type: 1, offset: 0.0 },
        Edit::Column { id: 1, spec: column() },
        Edit::NewColumn(column()),
        Edit::DeleteColumn(1),
        Edit::Beam { id: 1, spec: beam() },
        Edit::NewBeam(beam()),
        Edit::DeleteBeam(1),
        Edit::Roof { id: 1, offset: 0.0, overhang: 0.4, fascia: 0.1 },
        Edit::SlabBoundary { id: 1, outer: slab().outer, holes: slab().holes },
        Edit::NewSlab(slab()),
        Edit::Delete { table: "slab", id: 1 },
        Edit::CurtainProps { id: 1, level: 1, name: "CW".into(), base_offset: 0.0 },
        Edit::NewCurtain(curtain()),
        Edit::DeleteCurtain(1),
        Edit::NewParam(param(ParamType::Text)),
        Edit::ParamValue { param: "Fire".into(), kind: "wall".into(), id: 1, value: Some(PVal::Text("EI60".into())) },
        Edit::Phase { kind: "wall".into(), id: 1, created: 1, demolished: None },
    ]
}

#[test]
fn every_bulk_edit_variant_maps_or_is_refused_with_reason() {
    let all = sample_edits();
    let names: BTreeSet<&str> = all.iter().map(edit_name).collect();
    assert_eq!(names.len(), all.len(), "one sample per variant, unique names");
    assert_eq!(all.len(), 31);

    let out = edits_to_envelope(&all, "s.xlsx");
    // only the curtain creation is outside the wire for now
    let refused: Vec<(&str, &str)> =
        out.refused.iter().map(|r| (r.edit, r.reason.as_str())).collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "new_curtain");
    assert!(!refused[0].1.is_empty());
    assert_eq!(out.refused[0].index, 26);

    // every other edit produced at least one op; creates take consecutive slots
    let mapped: usize = all.len() - out.refused.len();
    assert!(out.envelope.op_count() >= mapped);
    let slots: Vec<u32> = out.envelope.ops().filter_map(|o| o.slot()).collect();
    assert_eq!(slots, (1..=slots.len() as u32).collect::<Vec<_>>());
    let back = Envelope::from_json(&out.envelope.to_json().unwrap()).unwrap();
    assert_eq!(back, out.envelope);
}

#[test]
fn unrepresentable_edits_are_refused_not_dropped_silently() {
    let mut guid = param(ParamType::Text);
    guid.guid = Some("g".into());
    let mut label = param(ParamType::Number);
    label.unit = "pcs".into();
    let mut cat = param(ParamType::Text);
    cat.categories = vec!["nonsense".into()];
    let edits = vec![
        Edit::NewParam(guid),
        Edit::NewParam(label),
        Edit::NewParam(cat),
        Edit::NewOpening { wall: 1, spec: opening("hatch"), swing: Swing::default() },
        Edit::ParamValue { param: "p".into(), kind: "mystery".into(), id: 1, value: None },
        Edit::Phase { kind: "mystery".into(), id: 1, created: 1, demolished: None },
        Edit::Delete { table: "wall", id: 1 },
    ];
    let out = edits_to_envelope(&edits, "s.xlsx");
    assert_eq!(out.refused.len(), 6);
    assert!(out.refused.iter().all(|r| !r.reason.is_empty()));
    assert!(!out.complete());
    assert_eq!(out.envelope.op_count(), 1);
}
