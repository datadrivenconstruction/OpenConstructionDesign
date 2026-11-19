//! Re-import of an updated source as a 3-way merge (base = last import, local edits, new
//! file): merge matrix (edit/edit, edit/delete, delete/edit, add/add), id stability, parameter
//! round-trip, provenance, undo, delta exports with the source ids.

use std::path::PathBuf;

use ddc_io::bridge::proto::{self, Curve, Element, Frame, OpeningKind, Top, Value};
use ddc_io::bridge::{self, ImportAs, Report};
use ddc_model::source::{ConflictCase, Pick, Policy, SqlValue, MASS_CHANGE};
use ddc_model::Model;

fn input() -> PathBuf {
    std::env::temp_dir()
        .join("ddc-merge-test")
        .join("project.rvt")
}

fn q(m: &Model, sql: &str) -> f64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn id_of(m: &Model, guid: &str) -> i64 {
    m.sql()
        .query_row("SELECT ref FROM ext_ref WHERE guid=?1", [guid], |r| {
            r.get(0)
        })
        .unwrap()
}

fn alive(m: &Model, table: &str, id: i64) -> bool {
    q(m, &format!("SELECT count(*) FROM {table} WHERE id={id}")) > 0.0
}

/// The source document: walls (guid, y, height), a door in W1, properties of W1.
#[derive(Clone)]
struct Doc {
    level_name: String,
    type_name: String,
    walls: Vec<(String, f64, f64)>,
    door: bool,
    props: Vec<(String, Value, String)>,
    /// Reader name in Hello (must not change the namespace).
    bridge: String,
    /// Source length unit in metres.
    scale: f64,
    /// More elements (meshes, an element changing kind).
    extra: Vec<Element>,
    /// Parameter definitions sent before Properties.
    defs: Vec<proto::ParamDef>,
}

impl Doc {
    fn new() -> Self {
        Doc {
            level_name: "Level 1".into(),
            type_name: "Basic 200".into(),
            walls: vec![("W1".into(), 0.0, 3.0), ("W2".into(), 5.0, 3.0)],
            door: true,
            props: vec![
                ("Fire rating".into(), Value::Text("EI 60".into()), "".into()),
                ("Area".into(), Value::Real(12.0), "m2".into()),
            ],
            bridge: "b".into(),
            scale: 1.0,
            extra: vec![],
            defs: vec![],
        }
    }

    fn wall(&mut self, g: &str) -> &mut (String, f64, f64) {
        self.walls.iter_mut().find(|w| w.0 == g).unwrap()
    }

    fn frames(&self) -> Vec<Frame> {
        let mut el: Vec<Element> = self
            .walls
            .iter()
            .map(|(g, y, h)| Element::Wall {
                guid: g.clone(),
                level: "L1".into(),
                wall_type: "T1".into(),
                axis: Curve::Line {
                    a: (0.0, *y),
                    b: (6.0, *y),
                },
                base_offset: 0.0,
                top: Top::Unconnected { height: *h },
                offset0: None,
            })
            .collect();
        if self.door && self.walls.iter().any(|w| w.0 == "W1") {
            el.push(Element::Opening {
                guid: "D1".into(),
                host: "W1".into(),
                kind: OpeningKind::Door,
                family_type: None,
                along: 1.0,
                width: 0.9,
                height: 2.1,
                sill: 0.0,
                swing_side: false,
                swing_hinge: false,
            });
        }
        el.extend(self.extra.iter().cloned());
        let n = el.len() as u64;
        let has_w1 = self.walls.iter().any(|w| w.0 == "W1");
        let props: Vec<proto::Property> = if has_w1 {
            self.props
                .iter()
                .map(|(k, v, u)| proto::Property {
                    owner: "W1".into(),
                    key: k.clone(),
                    value: v.clone(),
                    unit: u.clone(),
                })
                .collect()
        } else {
            vec![]
        };
        vec![
            Frame::Hello(proto::Hello {
                protocol: proto::PROTOCOL,
                bridge: self.bridge.clone(),
                version: "1".into(),
                formats: vec!["rvt".into()],
                capabilities: vec![],
            }),
            Frame::DocInfo(vec![
                ("version".into(), "2024".into()),
                ("project".into(), "P-1".into()),
            ]),
            Frame::Units(proto::Units {
                length_to_m: self.scale,
            }),
            Frame::Levels(vec![proto::Level {
                guid: "L1".into(),
                name: self.level_name.clone(),
                elevation: 0.0,
            }]),
            Frame::Views(vec![proto::View {
                guid: "V1".into(),
                name: "Level 1 plan".into(),
                kind: "plan".into(),
                level: Some("L1".into()),
            }]),
            Frame::Types(vec![proto::Type::Wall {
                guid: "T1".into(),
                name: self.type_name.clone(),
                layers: vec![proto::Layer {
                    material: "m".into(),
                    thickness: 0.2,
                }],
            }]),
            Frame::Elements(el),
            Frame::SourceIds(vec![proto::SourceId {
                guid: "W1".into(),
                ifc_guid: "0YvctVUKr0kugbFTf53O9L".into(),
            }]),
            Frame::ParamDefs(self.defs.clone()),
            Frame::Properties(props),
            Frame::ParamDefs(vec![
                proto::ParamDef {
                    key: "Fire rating".into(),
                    group: "Identity".into(),
                    type_param: false,
                    shared: Some(true),
                    shared_guid: "s-1".into(),
                    spec: "text".into(),
                },
                proto::ParamDef {
                    key: "Area".into(),
                    group: "Dimensions".into(),
                    type_param: false,
                    shared: Some(false),
                    shared_guid: String::new(),
                    spec: "area".into(),
                },
            ]),
            Frame::End(proto::Stats {
                elements: n,
                warnings: 0,
            }),
        ]
    }
}

fn first(m: &mut Model, d: &Doc) -> Report {
    bridge::import_frames(m, &input(), d.frames()).unwrap()
}

fn again(m: &mut Model, d: &Doc) -> Report {
    try_again(
        m,
        d,
        Policy {
            delete_removed: true,
            ..Default::default()
        },
    )
    .unwrap()
}

fn try_again(m: &mut Model, d: &Doc, policy: Policy) -> Result<Report, bridge::BridgeError> {
    let how = ImportAs {
        policy,
        ..Default::default()
    };
    bridge::import_frames_as(m, &input(), d.frames(), &how)
}

fn mesh(guid: &str, x: f64) -> Element {
    Element::Generic {
        guid: guid.into(),
        level: Some("L1".into()),
        category: "generic".into(),
        name: "box".into(),
        positions: vec![[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]],
        indices: vec![0, 1, 2],
    }
}

fn positions(m: &Model, id: i64) -> Vec<u8> {
    m.sql()
        .query_row("SELECT positions FROM mesh WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn source(m: &Model) -> String {
    m.sources().unwrap()[0].source.clone()
}

fn y_of(m: &Model, id: i64) -> f64 {
    q(m, &format!("SELECT ay FROM wall WHERE id={id}"))
}

fn h_of(m: &Model, id: i64) -> f64 {
    q(m, &format!("SELECT height FROM wall WHERE id={id}"))
}

fn prop(m: &Model, kind: &str, id: i64, key: &str) -> Option<String> {
    m.sql()
        .query_row(
            "SELECT CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            rusqlite::params![kind, id, key],
            |r| r.get(0),
        )
        .ok()
}

#[test]
fn ids_are_stable_and_unchanged_reimport_changes_nothing() {
    let mut m = Model::open(":memory:").unwrap();
    let d = Doc::new();
    first(&mut m, &d);
    let ids: Vec<i64> = ["W1", "W2", "D1", "L1", "T1"]
        .iter()
        .map(|g| id_of(&m, g))
        .collect();
    let r = again(&mut m, &d);
    assert_eq!(
        (r.walls, r.updated, r.merge.conflicts, r.merge.taken),
        (0, 0, 0, 0)
    );
    assert!(r.deleted.is_empty());
    let now: Vec<i64> = ["W1", "W2", "D1", "L1", "T1"]
        .iter()
        .map(|g| id_of(&m, g))
        .collect();
    assert_eq!(ids, now);
    // provenance
    let s = &m.sources().unwrap()[0];
    assert_eq!((s.format.as_str(), s.imports), ("rvt", 2));
    assert_eq!(s.info.get("version").map(String::as_str), Some("2024"));
    let p = m.provenance("wall", ids[0]).unwrap().unwrap();
    assert_eq!(p.guid, "W1");
    assert_eq!(p.ifc_guid.as_deref(), Some("0YvctVUKr0kugbFTf53O9L"));
    assert!(p.tracked && !p.edited);
    let views = m.source_views(&s.source).unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].name, "Level 1 plan");
}

#[test]
fn upstream_changes_apply_local_edits_stay_edit_edit_conflicts() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let (w1, w2) = (id_of(&m, "W1"), id_of(&m, "W2"));
    // local edit of W1 height; upstream moves W1 (other field) and W2, and changes W1 height
    m.sql()
        .execute(&format!("UPDATE wall SET height=3.3 WHERE id={w1}"), [])
        .unwrap();
    d.wall("W1").1 = 0.5;
    d.wall("W1").2 = 3.6;
    d.wall("W2").1 = 5.5;
    let r = again(&mut m, &d);
    assert_eq!(y_of(&m, w1), 0.5, "non-conflicting field taken");
    assert_eq!(y_of(&m, w2), 5.5);
    assert_eq!(h_of(&m, w1), 3.3, "conflict keeps the local value");
    assert_eq!(r.merge.conflicts, 1);
    assert_eq!(r.merge.merged, 2);
    let c = m.conflicts(None).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].case, c[0].field.as_str(), c[0].element),
        (ConflictCase::EditEdit, "height", w1)
    );
    // take theirs
    m.resolve_conflicts(&[(c[0].id, Pick::Theirs)]).unwrap();
    assert_eq!(h_of(&m, w1), 3.6);
    assert!(!m.is_edited("wall", w1).unwrap());
}

#[test]
fn reimport_is_one_undo_step() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let w2 = id_of(&m, "W2");
    let src_rows = |m: &Model| {
        (
            q(m, "SELECT count(*) FROM src_base"),
            q(m, "SELECT count(*) FROM src_conflict"),
            q(m, "SELECT imports FROM src_file"),
        )
    };
    let before = src_rows(&m);
    d.wall("W2").1 = 7.0;
    d.walls.push(("W3".into(), 9.0, 3.0));
    again(&mut m, &d);
    assert_eq!(y_of(&m, w2), 7.0);
    m.undo().unwrap();
    assert_eq!(y_of(&m, w2), 5.0);
    assert_eq!(src_rows(&m), before);
    assert_eq!(q(&m, "SELECT count(*) FROM ext_ref WHERE guid='W3'"), 0.0);
}

#[test]
fn deleted_upstream_removes_unedited_and_conflicts_edited() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let (w1, w2, d1) = (id_of(&m, "W1"), id_of(&m, "W2"), id_of(&m, "D1"));
    // W1 edited here, W2 not; both removed upstream
    m.sql()
        .execute(&format!("UPDATE wall SET height=3.3 WHERE id={w1}"), [])
        .unwrap();
    d.walls.clear();
    let r = again(&mut m, &d);
    assert!(
        !alive(&m, "wall", w2),
        "unedited element deleted with its source"
    );
    assert!(r.deleted.contains(&"W2".to_string()));
    assert!(alive(&m, "wall", w1), "edited element kept");
    // the door is gone upstream too and was not edited here
    assert!(!alive(&m, "opening", d1));
    assert_eq!(r.merge.deleted_upstream, 2);
    let c = m.conflicts(None).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].case, c[0].field.as_str(), c[0].element),
        (ConflictCase::EditDelete, "*", w1)
    );
    m.resolve_conflicts(&[(c[0].id, Pick::Theirs)]).unwrap();
    assert!(!alive(&m, "wall", w1));
    m.undo().unwrap();
    assert!(alive(&m, "wall", w1));
    m.undo().unwrap();
    assert!(
        alive(&m, "wall", w2) && alive(&m, "opening", d1),
        "re-import undone"
    );
}

#[test]
fn deleted_here_stays_deleted_and_upstream_edit_is_a_conflict() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let (w1, w2) = (id_of(&m, "W1"), id_of(&m, "W2"));
    m.delete_wall(w2).unwrap();
    m.delete_wall(w1).unwrap();
    // unchanged upstream: W2 stays deleted, no conflict; W1 changed upstream → delete/edit
    d.wall("W1").2 = 4.0;
    let r = again(&mut m, &d);
    assert_eq!(r.walls, 0, "nothing recreated");
    assert!(r.merge.kept_deleted >= 2);
    let c = m.conflicts(None).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].case, c[0].field.as_str()),
        (ConflictCase::DeleteEdit, "*")
    );
    // take theirs: recreated from the new source, linked to the same guid
    let st = m.resolve_conflicts(&[(c[0].id, Pick::Theirs)]).unwrap();
    assert_eq!(st.restored, 1);
    let w1b = id_of(&m, "W1");
    assert!(alive(&m, "wall", w1b));
    assert_eq!(h_of(&m, w1b), 4.0);
    // and the next import matches it again
    let r = again(&mut m, &d);
    assert_eq!((r.walls, r.updated, r.merge.conflicts), (0, 0, 0));
    assert_eq!(id_of(&m, "W1"), w1b);
}

#[test]
fn added_on_both_sides_are_kept_apart() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let lv = id_of(&m, "L1");
    let t = id_of(&m, "T1");
    let mine = m
        .add_wall(&ddc_model::WallSpec {
            a: (0.0, 20.0),
            b: (5.0, 20.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    d.walls.push(("W3".into(), 20.0, 3.0));
    let r = again(&mut m, &d);
    assert_eq!((r.walls, r.merge.added), (1, 1));
    let theirs = id_of(&m, "W3");
    assert_ne!(mine, theirs);
    assert!(alive(&m, "wall", mine), "local addition untouched");
    assert!(m.provenance("wall", mine).unwrap().is_none());
    let src = source(&m);
    let ch = m.local_changes(&src, true).unwrap();
    assert!(ch.iter().any(|c| c.id == mine && c.change.key() == "added"));
    assert!(!ch.iter().any(|c| c.id == theirs));
}

#[test]
fn parameters_round_trip_with_units_groups_and_merge() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let w1 = id_of(&m, "W1");
    let ps = m.source_params("wall", w1).unwrap();
    let fire = ps.iter().find(|p| p.key == "Fire rating").unwrap();
    assert_eq!(
        (fire.group.as_str(), fire.shared, fire.scope),
        ("Identity", Some(true), "instance")
    );
    let area = ps.iter().find(|p| p.key == "Area").unwrap();
    assert_eq!((area.unit.as_str(), area.shared), ("m2", Some(false)));
    assert!(!fire.edited);

    // local edit of one parameter; upstream changes the other and the same one differently
    m.set_source_prop(
        "wall",
        w1,
        "Fire rating",
        SqlValue::Text("EI 90".into()),
        "",
    )
    .unwrap();
    assert!(m.is_edited("wall", w1).unwrap());
    d.props[1].1 = Value::Real(13.5);
    let r = again(&mut m, &d);
    assert_eq!(
        prop(&m, "wall", w1, "Fire rating").as_deref(),
        Some("EI 90"),
        "local edit kept"
    );
    assert_eq!(
        prop(&m, "wall", w1, "Area").as_deref(),
        Some("13.5"),
        "upstream change taken"
    );
    assert_eq!(r.merge.conflicts, 0);
    d.props[0].1 = Value::Text("EI 120".into());
    let r = again(&mut m, &d);
    assert_eq!(r.merge.conflicts, 1);
    let c = m.conflicts(None).unwrap();
    assert_eq!(c[0].field, "p:Fire rating");
    m.resolve_conflicts(&[(c[0].id, Pick::Mine)]).unwrap();
    assert_eq!(
        prop(&m, "wall", w1, "Fire rating").as_deref(),
        Some("EI 90")
    );

    // a parameter removed upstream and unedited here is removed
    d.props.remove(1);
    let r = again(&mut m, &d);
    assert_eq!(r.merge.params_removed, 1);
    assert_eq!(prop(&m, "wall", w1, "Area"), None);
}

#[test]
fn level_and_type_names_follow_source_renames_and_keep_local_ones() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let (lv, t) = (id_of(&m, "L1"), id_of(&m, "T1"));
    let name = |m: &Model, table: &str, id: i64| -> String {
        m.sql()
            .query_row(
                &format!("SELECT name FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(name(&m, "level", lv), "Level 1");
    // renamed in the source → follows
    d.level_name = "Ground".into();
    again(&mut m, &d);
    assert_eq!(name(&m, "level", lv), "Ground");
    // renamed here → kept while the source keeps its name
    m.sql()
        .execute(
            &format!("UPDATE wall_type SET name='Mine 200' WHERE id={t}"),
            [],
        )
        .unwrap();
    let r = again(&mut m, &d);
    assert_eq!(name(&m, "wall_type", t), "Mine 200");
    assert_eq!(r.merge.conflicts, 0);
    // renamed on both sides → conflict
    d.type_name = "Basic 250".into();
    let r = again(&mut m, &d);
    assert_eq!(r.merge.conflicts, 1);
    assert_eq!(name(&m, "wall_type", t), "Mine 200");
}

#[test]
fn delta_exports_carry_source_ids() {
    let mut m = Model::open(":memory:").unwrap();
    let d = Doc::new();
    first(&mut m, &d);
    let w1 = id_of(&m, "W1");
    m.sql()
        .execute(&format!("UPDATE wall SET height=3.3 WHERE id={w1}"), [])
        .unwrap();
    let src = source(&m);
    let lv2 = m.add_level("Level 2", 3.0).unwrap();
    m.sql()
        .execute(&format!("UPDATE wall SET level={lv2} WHERE id={w1}"), [])
        .unwrap();
    let rows = ddc_io::xlsx::delta_rows_for(&m, &src, &m.local_changes(&src, true).unwrap());
    assert_eq!(rows.len(), 2);
    use ddc_io::xlsx::TableCell as C;
    assert_eq!(rows[0][0], C::Text("W1".into()));
    assert_eq!(rows[0][1], C::Text("0YvctVUKr0kugbFTf53O9L".into()));
    let h = rows
        .iter()
        .find(|r| r[5] == C::Text("height".into()))
        .unwrap();
    assert_eq!((&h[7], &h[8]), (&C::Num(3.0), &C::Num(3.3)));
    // a reference is written as the source id of the level, a local level by its name
    let l = rows
        .iter()
        .find(|r| r[5] == C::Text("level".into()))
        .unwrap();
    assert_eq!(
        (&l[7], &l[8]),
        (&C::Text("L1".into()), &C::Text("Level 2".into()))
    );
    let dir = std::env::temp_dir().join(format!("ddc-merge-delta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let n = ddc_io::xlsx::export_delta(&m, &src, true, &dir.join("delta.xlsx")).unwrap();
    assert_eq!(n, 1);
    let ifc = ddc_io::ifc::export_ifc_source_guids(&m).unwrap();
    assert!(ifc.contains("'0YvctVUKr0kugbFTf53O9L'"));
    assert_eq!(ifc.matches("'0YvctVUKr0kugbFTf53O9L'").count(), 1);
    let _ = std::fs::remove_file(dir.join("delta.xlsx"));
}

#[test]
fn namespace_does_not_depend_on_the_reader_name() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    first(&mut m, &d);
    let w1 = id_of(&m, "W1");
    d.bridge = "reader-2".into();
    let r = again(&mut m, &d);
    assert_eq!((r.walls, r.merge.added), (0, 0));
    assert_eq!(id_of(&m, "W1"), w1);
    assert_eq!(m.sources().unwrap().len(), 1);
    assert_eq!(q(&m, "SELECT count(*) FROM wall"), 2.0);
}

#[test]
fn element_changing_kind_replaces_the_old_row() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    d.extra = vec![mesh("G1", 10.0)];
    first(&mut m, &d);
    let g = id_of(&m, "G1");
    d.extra = vec![];
    d.walls.push(("G1".into(), 12.0, 3.0));
    again(&mut m, &d);
    assert!(
        !alive(&m, "mesh", g),
        "the mesh is replaced, not duplicated"
    );
    assert_eq!(q(&m, "SELECT count(*) FROM ext_ref WHERE guid='G1'"), 1.0);
    let w = id_of(&m, "G1");
    assert!(alive(&m, "wall", w));
    assert_eq!(m.provenance("wall", w).unwrap().unwrap().guid, "G1");
    // matched as a wall from now on
    let r = again(&mut m, &d);
    assert_eq!((r.walls, r.merge.conflicts), (0, 0));
    m.undo().unwrap();
    m.undo().unwrap();
    assert!(alive(&m, "mesh", g));
    assert_eq!(id_of(&m, "G1"), g);
}

#[test]
fn geometry_conflict_take_theirs_applies_the_new_body() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    d.extra = vec![mesh("G1", 0.0)];
    first(&mut m, &d);
    let g = id_of(&m, "G1");
    let local: Vec<u8> = (0..72).collect();
    m.sql()
        .execute(
            "UPDATE mesh SET positions=?1 WHERE id=?2",
            rusqlite::params![local, g],
        )
        .unwrap();
    d.extra = vec![mesh("G1", 5.0)];
    let r = again(&mut m, &d);
    assert_eq!(r.merge.conflicts, 1);
    assert_eq!(positions(&m, g), local, "mine stays until resolved");
    let c = m.conflicts(None).unwrap();
    assert_eq!(c[0].field, "positions");
    m.resolve_conflicts(&[(c[0].id, Pick::Theirs)]).unwrap();
    let want: Vec<u8> = [[5.0f64, 0.0, 0.0], [6.0, 0.0, 0.0], [5.0, 1.0, 0.0]]
        .iter()
        .flatten()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    assert_eq!(positions(&m, g), want);
    m.undo().unwrap();
    assert_eq!(positions(&m, g), local);
}

#[test]
fn unitless_length_parameters_are_converted_to_metres() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    d.scale = 0.3048;
    d.props = vec![
        ("Depth".into(), Value::Real(10.0), "".into()),
        ("Area".into(), Value::Real(100.0), "".into()),
        ("Count".into(), Value::Real(3.0), "".into()),
    ];
    let def = |key: &str, spec: &str| proto::ParamDef {
        key: key.into(),
        group: "Dimensions".into(),
        type_param: false,
        shared: Some(false),
        shared_guid: String::new(),
        spec: spec.into(),
    };
    d.defs = vec![
        def("Depth", "spec.aec:length-2.0.0"),
        def("Area", "area"),
        def("Count", "number"),
    ];
    first(&mut m, &d);
    let w1 = id_of(&m, "W1");
    let num = |key: &str| prop(&m, "wall", w1, key).unwrap().parse::<f64>().unwrap();
    assert!((num("Depth") - 3.048).abs() < 1e-9);
    assert!((num("Area") - 100.0 * 0.3048 * 0.3048).abs() < 1e-9);
    assert_eq!(num("Count"), 3.0);
    let unit: String = m
        .sql()
        .query_row(
            "SELECT unit FROM prop WHERE kind='wall' AND ref=?1 AND key='Depth'",
            [w1],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unit, "m");
    // unchanged re-import: no spurious change from the conversion
    let r = again(&mut m, &d);
    assert_eq!((r.merge.taken, r.merge.conflicts), (0, 0));
}

#[test]
fn reimport_matching_almost_nothing_is_refused() {
    let mut m = Model::open(":memory:").unwrap();
    let mut d = Doc::new();
    d.walls = (0..25)
        .map(|i| (format!("W{i}"), i as f64 * 2.0, 3.0))
        .collect();
    first(&mut m, &d);
    let n = q(&m, "SELECT count(*) FROM wall");
    // the same walls under other ids (another id format) must not delete the project
    d.walls = (0..25)
        .map(|i| (format!("X{i}"), i as f64 * 2.0, 3.0))
        .collect();
    let e = try_again(
        &mut m,
        &d,
        Policy {
            delete_removed: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains(MASS_CHANGE), "{e}");
    assert_eq!(q(&m, "SELECT count(*) FROM wall"), n, "nothing changed");
    let allowed = Policy {
        delete_removed: true,
        allow_mass_change: true,
    };
    try_again(&mut m, &d, allowed).unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM wall"), 25.0);
}
