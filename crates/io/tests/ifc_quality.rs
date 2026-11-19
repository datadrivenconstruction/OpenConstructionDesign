// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Quality of the exchange IFC of authored models: the structural validator finds nothing in
//! IFC4, IFC4X3 and IFC2X3 files of a house and a nine-storey section, the validator catches
//! deliberate damage, the export is byte-identical twice, GlobalIds survive an edit, and the
//! file comes back through our import with the same counts and areas.

use std::collections::BTreeSet;

use ddc_io::ifc::{export_ifc_with, import_ifc, validate_ifc, IfcOptions, IfcSchema, RULES};
use ddc_model::{
    BeamJoin, BeamSpec, ColumnSpec, Just, LayerSpec, MemberShape, Model, OpeningSpec, RoofKind,
    RoofSpec, RoomSpec, SiteProps, SlabSpec, StairSpec, WallSpec,
};

const STAMP: &str = "2026-01-01T00:00:00";

fn opts(schema: IfcSchema) -> IfcOptions {
    IfcOptions {
        schema,
        timestamp: Some(STAMP.into()),
        ..Default::default()
    }
}

fn lay(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

fn opening(kind: &str) -> OpeningSpec {
    OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: if kind == "door" { 0.0 } else { 0.9 },
        height: if kind == "door" { 2.1 } else { 1.2 },
        kind: kind.into(),
    }
}

/// A closed box of walls `w` x `d` on `level`; returns the wall ids.
fn walls(m: &mut Model, level: i64, wt: i64, w: f64, d: f64, h: f64) -> Vec<i64> {
    let p = [(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)];
    (0..4)
        .map(|k| {
            m.add_wall(&WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level,
                base_offset: 0.0,
                height: h,
                wall_type: wt,
                offset0: None,
            })
            .unwrap()
        })
        .collect()
}

fn slab(m: &mut Model, level: i64, st: i64, w: f64, d: f64) {
    m.add_slab(&SlabSpec {
        level,
        slab_type: st,
        offset: 0.0,
        outer: vec![(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)],
        holes: vec![],
    })
    .unwrap();
}

fn site() -> SiteProps {
    SiteProps {
        name: "Plot 7".into(),
        north_deg: 20.0,
        base: (691_000.0, 5_335_000.0, 520.0),
        latitude: 48.1,
        longitude: 11.6,
        crs_code: "EPSG:32632".into(),
        crs_name: "WGS 84 / UTM zone 32N".into(),
        crs_vertical: String::new(),
    }
}

fn house() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("Ground", 0.0).unwrap();
    let l2 = m.add_level("First", 3.2).unwrap();
    let wt = m
        .add_wall_type("Brick 300", &[lay("brick", 0.2), lay("insulation", 0.1)])
        .unwrap();
    let st = m
        .add_slab_type("Slab 250", &[lay("concrete", 0.25)])
        .unwrap();
    let w1 = walls(&mut m, l1, wt, 8.0, 6.0, 3.0);
    m.add_opening(w1[0], &opening("door")).unwrap();
    m.add_opening(w1[1], &opening("window")).unwrap();
    m.add_opening(w1[2], &opening("window")).unwrap();
    let w2 = walls(&mut m, l2, wt, 8.0, 6.0, 3.0);
    m.add_opening(w2[0], &opening("window")).unwrap();
    m.add_opening(w2[1], &opening("window")).unwrap();
    slab(&mut m, l1, st, 8.0, 6.0);
    slab(&mut m, l2, st, 8.0, 6.0);
    for (lv, name, no) in [(l1, "Living", "001"), (l2, "Bedroom", "101")] {
        m.add_room(&RoomSpec {
            level: lv,
            name: name.into(),
            number: no.into(),
            point: (4.0, 3.0),
            height: 3.0,
        })
        .unwrap();
    }
    let t = m
        .add_member_type("C30", &MemberShape::Rect { b: 0.3, h: 0.3 }, "concrete")
        .unwrap();
    m.add_column(&ColumnSpec {
        level: l1,
        top_level: Some(l2),
        member_type: t,
        structural: true,
        at: (2.0, 2.0),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    })
    .unwrap();
    let bt = m
        .add_member_type("B30", &MemberShape::Rect { b: 0.2, h: 0.4 }, "concrete")
        .unwrap();
    m.add_beam(&BeamSpec {
        level: l2,
        member_type: bt,
        a: (0.0, 3.0, 3.0),
        b: (8.0, 3.0, 3.0),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Pos,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Butt,
    })
    .unwrap();
    m.add_stair(&StairSpec::straight(l1, l2, (5.0, 1.0), 0.0))
        .unwrap();
    m.add_roof(&RoofSpec {
        level: l2,
        slab_type: st,
        offset: 3.0,
        overhang: 0.4,
        fascia: 0.0,
        kind: RoofKind::Footprint {
            ring: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)],
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    })
    .unwrap();
    m.set_site(&site()).unwrap();
    m
}

/// Nine storeys of 3 m, a box of walls, a door and a window, a slab, a room and a column each.
fn section() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let wt = m
        .add_wall_type("Concrete 250", &[lay("concrete", 0.25)])
        .unwrap();
    let st = m
        .add_slab_type("Slab 220", &[lay("concrete", 0.22)])
        .unwrap();
    let ct = m
        .add_member_type("C40", &MemberShape::Rect { b: 0.4, h: 0.4 }, "concrete")
        .unwrap();
    for k in 0..9 {
        let lv = m.add_level(&format!("L{}", k + 1), 3.0 * k as f64).unwrap();
        let w = walls(&mut m, lv, wt, 12.0, 8.0, 2.9);
        m.add_opening(w[0], &opening("door")).unwrap();
        m.add_opening(w[2], &opening("window")).unwrap();
        slab(&mut m, lv, st, 12.0, 8.0);
        m.add_room(&RoomSpec {
            level: lv,
            name: "Office".into(),
            number: format!("{}01", k + 1),
            point: (6.0, 4.0),
            height: 2.9,
        })
        .unwrap();
        m.add_column(&ColumnSpec {
            level: lv,
            top_level: None,
            member_type: ct,
            structural: true,
            at: (3.0, 3.0),
            base_offset: 0.0,
            top_offset: 2.9,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    }
    m
}

fn count(t: &str, class: &str) -> usize {
    t.matches(&format!("={class}(")).count()
}

fn assert_clean(text: &str, what: &str) {
    let rep = validate_ifc(text.as_bytes());
    assert!(
        rep.is_clean(),
        "{what}: {} findings\n{}",
        rep.errors.len(),
        rep.brief(15)
    );
}

#[test]
fn the_house_validates_in_every_schema() {
    let m = house();
    for (schema, name) in [
        (IfcSchema::Ifc4, "IFC4"),
        (IfcSchema::Ifc4x3, "IFC4X3_ADD2"),
        (IfcSchema::Ifc2x3, "IFC2X3"),
    ] {
        let (t, _) = export_ifc_with(&m, &opts(schema)).unwrap();
        assert!(t.contains(&format!("FILE_SCHEMA(('{name}'))")), "{name}");
        assert!(t.contains(STAMP));
        assert_clean(&t, name);
    }
}

#[test]
fn the_nine_storey_section_validates() {
    let m = section();
    let (t, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap();
    assert_eq!(count(&t, "IFCBUILDINGSTOREY"), 9);
    assert_eq!(count(&t, "IFCWALL"), 36);
    assert_clean(&t, "section");
    let (t3, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap();
    assert_clean(&t3, "section IFC2X3");
}

#[test]
fn types_psets_quantities_and_georeference_are_written() {
    let (t, _) = export_ifc_with(&house(), &opts(IfcSchema::Ifc4)).unwrap();
    for class in [
        "IFCWALLTYPE",
        "IFCSLABTYPE",
        "IFCDOORTYPE",
        "IFCWINDOWTYPE",
        "IFCCOLUMNTYPE",
        "IFCBEAMTYPE",
        "IFCRELDEFINESBYTYPE",
        "IFCMAPCONVERSION",
        "IFCPROJECTEDCRS",
        "IFCMATERIALLAYERSETUSAGE",
        "IFCRELVOIDSELEMENT",
        "IFCRELFILLSELEMENT",
    ] {
        assert!(count(&t, class) > 0, "{class}");
    }
    for name in [
        "Pset_WallCommon",
        "Pset_SlabCommon",
        "Pset_RoofCommon",
        "Pset_DoorCommon",
        "Pset_WindowCommon",
        "Pset_ColumnCommon",
        "Pset_BeamCommon",
        "Pset_SpaceCommon",
        "Qto_SpaceBaseQuantities",
        "Qto_ColumnBaseQuantities",
        "Qto_WallBaseQuantities",
    ] {
        assert!(t.contains(&format!("'{name}'")), "{name}");
    }
    assert!(t.contains("'EPSG:32632'"), "UTM zone 32 for longitude 11.6");
    assert!(t.contains("'LoadBearing'"));
    assert_eq!(
        count(&t, "IFCRELDEFINESBYTYPE"),
        count(&t, "IFCWALLTYPE")
            + count(&t, "IFCSLABTYPE")
            + count(&t, "IFCROOFTYPE")
            + count(&t, "IFCDOORTYPE")
            + count(&t, "IFCWINDOWTYPE")
            + count(&t, "IFCCOLUMNTYPE")
            + count(&t, "IFCBEAMTYPE")
    );
}

#[test]
fn output_is_byte_identical_with_a_fixed_stamp() {
    let m = house();
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc2x3, IfcSchema::Ifc4x3] {
        let a = export_ifc_with(&m, &opts(schema)).unwrap().0;
        let b = export_ifc_with(&m, &opts(schema)).unwrap().0;
        assert_eq!(a, b, "{schema:?}");
    }
}

fn rooted(t: &str, class: &str) -> BTreeSet<String> {
    t.lines()
        .filter(|l| l.contains(&format!("={class}(")))
        .filter_map(|l| l.split('(').nth(1)?.split(',').next().map(str::to_string))
        .collect()
}

#[test]
fn global_ids_survive_a_re_export_after_an_edit() {
    let mut m = house();
    let before = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    let wall: i64 = m
        .sql()
        .query_row("SELECT min(id) FROM wall", [], |r| r.get(0))
        .unwrap();
    m.add_opening(
        wall,
        &OpeningSpec {
            along: 3.0,
            ..opening("window")
        },
    )
    .unwrap();
    let after = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    for class in [
        "IFCWALL",
        "IFCSLAB",
        "IFCDOOR",
        "IFCWINDOW",
        "IFCSPACE",
        "IFCBUILDINGSTOREY",
        "IFCCOLUMN",
        "IFCWALLTYPE",
        "IFCDOORTYPE",
    ] {
        let (a, b) = (rooted(&before, class), rooted(&after, class));
        assert!(!a.is_empty(), "{class}");
        assert!(a.is_subset(&b), "{class}: ids changed by an unrelated edit");
    }
    assert_eq!(count(&after, "IFCWINDOW"), count(&before, "IFCWINDOW") + 1);
}

fn table_count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn room_area(m: &Model) -> f64 {
    let ids: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM room ORDER BY id").unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    };
    ids.into_iter()
        .map(|id| m.room_quantities(id).unwrap().map_or(0.0, |q| q.area))
        .sum()
}

fn round_trip(src: &Model, schema: IfcSchema) {
    let (t, _) = export_ifc_with(src, &opts(schema)).unwrap();
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, t.as_bytes()).unwrap();
    for table in [
        "level", "wall", "opening", "slab", "room", "col", "beam", "stair", "roof",
    ] {
        assert_eq!(
            table_count(src, table),
            table_count(&back, table),
            "{schema:?}: {table}"
        );
    }
    let (a, b) = (room_area(src), room_area(&back));
    assert!(
        a > 0.0 && (a - b).abs() / a < 0.01,
        "{schema:?}: room area {a} vs {b}"
    );
    // the imported model exports the same classes
    let (t2, _) = export_ifc_with(&back, &opts(schema)).unwrap();
    for class in [
        "IFCBUILDINGSTOREY",
        "IFCWALL",
        "IFCSLAB",
        "IFCDOOR",
        "IFCWINDOW",
        "IFCSPACE",
        "IFCOPENINGELEMENT",
        "IFCCOLUMN",
        "IFCBEAM",
        "IFCROOF",
    ] {
        assert_eq!(count(&t, class), count(&t2, class), "{schema:?}: {class}");
    }
    assert_clean(&t2, "re-exported");
}

#[test]
fn the_house_round_trips_with_the_same_counts_and_areas() {
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3, IfcSchema::Ifc2x3] {
        round_trip(&house(), schema);
    }
}

#[test]
fn the_section_round_trips() {
    round_trip(&section(), IfcSchema::Ifc4);
}

/// Inserts `line` before the end of the DATA section.
fn add_line(t: &str, line: &str) -> String {
    t.replacen("ENDSEC;\nEND-ISO", &format!("{line}\nENDSEC;\nEND-ISO"), 1)
}

fn first_line(t: &str, class: &str) -> String {
    t.lines()
        .find(|l| l.contains(&format!("={class}(")))
        .unwrap()
        .to_string()
}

fn rules_of(t: &str) -> BTreeSet<&'static str> {
    validate_ifc(t.as_bytes())
        .errors
        .iter()
        .map(|e| e.rule)
        .collect()
}

#[test]
fn the_validator_catches_damage() {
    let (t, _) = export_ifc_with(&house(), &opts(IfcSchema::Ifc4)).unwrap();
    assert_clean(&t, "base");
    let next = 900_000;

    // a second fill of the same opening
    let fill = first_line(&t, "IFCRELFILLSELEMENT");
    let body = fill.split_once('=').unwrap().1;
    let dup = add_line(&t, &format!("#{next}={body}"));
    assert!(
        rules_of(&dup).contains("opening-filled-once") || rules_of(&dup).contains("guid-unique")
    );

    // same GlobalId on two walls
    let wall = first_line(&t, "IFCWALL");
    let body = wall.split_once('=').unwrap().1;
    assert!(rules_of(&add_line(&t, &format!("#{next}={body}"))).contains("guid-unique"));

    // a GlobalId of the wrong length
    let g = rooted(&t, "IFCWALL").into_iter().next().unwrap();
    let bad = t.replacen(&g, "'tooshort'", 1);
    assert!(rules_of(&bad).contains("guid-format"));

    // a reference to nothing
    let dangling = add_line(&t, &format!("#{next}=IFCDIRECTION((0.,0.,#7777777));"));
    assert!(rules_of(&dangling).contains("ref-missing"));

    // a wall nobody contains
    let contain = first_line(&t, "IFCRELCONTAINEDINSPATIALSTRUCTURE");
    let cut = t.replacen(&format!("{contain}\n"), "", 1);
    assert!(rules_of(&cut).contains("element-contained"));

    // an opening that voids nothing
    let void = first_line(&t, "IFCRELVOIDSELEMENT");
    let cut = t.replacen(&format!("{void}\n"), "", 1);
    assert!(rules_of(&cut).contains("opening-voids-one"));

    // a unit that is not the metre
    let unit = t.replacen(".METRE.", ".FOOT.", 1);
    assert!(rules_of(&unit).contains("unit-length"));

    // a storey without elevation
    let sto = first_line(&t, "IFCBUILDINGSTOREY");
    let elev = sto
        .rsplit_once(',')
        .unwrap()
        .1
        .trim_end_matches(");")
        .to_string();
    let no_elev = t.replacen(&sto, &sto.replacen(&format!(",{elev});"), ",$);", 1), 1);
    assert!(rules_of(&no_elev).contains("storey-elevation"));
}

#[test]
fn rule_list_is_complete_and_unique() {
    let names: BTreeSet<_> = RULES.iter().map(|r| r.0).collect();
    assert_eq!(names.len(), RULES.len());
    assert!(RULES.len() >= 30);
}

#[test]
fn the_streamed_file_matches_the_in_memory_export() {
    let dir = std::env::temp_dir().join(format!("ifcq1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let m = house();
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3, IfcSchema::Ifc2x3] {
        let path = dir.join(format!("{schema:?}.ifc"));
        let stats =
            ddc_io::ifc::export_ifc_file_with(&m, &path, "model.ifc", None, &opts(schema)).unwrap();
        let file = std::fs::read_to_string(&path).unwrap();
        assert_eq!(stats.bytes, file.len() as u64);
        assert!(!dir.join(format!("{schema:?}.ifc.part")).exists());
        let (mem, _) = export_ifc_with(&m, &opts(schema)).unwrap();
        assert_clean(&file, "streamed");
        assert_eq!(file.lines().count(), mem.lines().count(), "{schema:?}");
        for class in [
            "IFCWALL",
            "IFCSLAB",
            "IFCDOOR",
            "IFCWINDOW",
            "IFCSPACE",
            "IFCRELDEFINESBYTYPE",
        ] {
            assert_eq!(
                count(&file, class),
                count(&mem, class),
                "{schema:?}: {class}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_bearing_integers_become_booleans_in_both_export_paths() {
    use rusqlite::types::Value;
    let mut m = house();
    let first = |m: &Model, t: &str| -> i64 {
        m.sql()
            .query_row(&format!("SELECT min(id) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    };
    let (w, s) = (first(&m, "wall"), first(&m, "slab"));
    let props = [
        ("wall", w, "Pset_WallCommon.LoadBearing", Value::Integer(1)),
        (
            "wall",
            w,
            "Pset_WallCommon.FireRating",
            Value::Text("EI60".into()),
        ),
        ("slab", s, "Pset_SlabCommon.LoadBearing", Value::Integer(0)),
        (
            "slab",
            s,
            "Pset_SlabCommon.FireRating",
            Value::Text("REI90".into()),
        ),
    ];
    for (k, id, key, v) in props {
        m.set_prop(k, id, key, Some((v, ""))).unwrap();
    }
    let (mem, _) = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap();
    let dir = std::env::temp_dir().join(format!("ifcq1-lb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("lb.ifc");
    ddc_io::ifc::export_ifc_file_with(&m, &p, "model.ifc", None, &opts(IfcSchema::Ifc4)).unwrap();
    let file = std::fs::read_to_string(&p).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    for t in [&mem, &file] {
        assert!(t.contains("'LoadBearing',$,IFCBOOLEAN(.T.)"), "wall");
        assert!(t.contains("'LoadBearing',$,IFCBOOLEAN(.F.)"), "slab");
        assert!(t.contains("'FireRating',$,IFCLABEL('EI60')"));
        assert!(t.contains("'FireRating',$,IFCLABEL('REI90')"));
        assert_clean(t, "load bearing");
    }
}

#[test]
fn map_conversion_is_written_only_when_the_crs_is_known() {
    let has = |m: &Model| {
        let t = export_ifc_with(m, &opts(IfcSchema::Ifc4)).unwrap().0;
        assert_clean(&t, "georeference");
        (count(&t, "IFCMAPCONVERSION"), count(&t, "IFCPROJECTEDCRS"))
    };
    let mut m = house();
    assert_eq!(has(&m), (1, 1));
    // latitude, longitude and a base point without a coordinate system: nothing is guessed
    m.set_site(&SiteProps {
        crs_code: String::new(),
        crs_name: String::new(),
        ..site()
    })
    .unwrap();
    assert_eq!(has(&m), (0, 0));
    // a stored coordinate system is written as it is, with the vertical datum
    m.set_site(&SiteProps {
        crs_code: "EPSG:25832".into(),
        crs_name: "ETRS89 / UTM zone 32N".into(),
        crs_vertical: "EPSG:5783".into(),
        latitude: 0.0,
        longitude: 0.0,
        ..site()
    })
    .unwrap();
    let t = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    assert!(t.contains("IFCPROJECTEDCRS('EPSG:25832','ETRS89 / UTM zone 32N',$,'EPSG:5783',"));
    assert_eq!(has(&m), (1, 1));
    // no site at all
    let plain = Model::open(":memory:").unwrap();
    assert_eq!(has(&plain), (0, 0));
}

fn exported(m: &Model) -> String {
    export_ifc_with(m, &opts(IfcSchema::Ifc4)).unwrap().0
}

#[test]
fn coordinate_system_survives_export_and_import() {
    let m = house();
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, exported(&m).as_bytes()).unwrap();
    let s = back.site().unwrap();
    assert_eq!(s.crs_code, "EPSG:32632");
    assert_eq!(s.crs_name, "WGS 84 / UTM zone 32N");
    assert!((s.base.0 - 691_000.0).abs() < 1e-3 && (s.base.1 - 5_335_000.0).abs() < 1e-3);
    assert!((s.north_deg - 20.0).abs() < 1e-6, "{}", s.north_deg);
    // a file without a map conversion leaves the site without a coordinate system
    let mut plain = house();
    let no_crs = SiteProps {
        crs_code: String::new(),
        ..site()
    };
    plain.set_site(&no_crs).unwrap();
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, exported(&plain).as_bytes()).unwrap();
    assert_eq!(back.site().unwrap().crs_code, "");
    // an existing coordinate system is not overwritten by the file
    let mut keep = Model::open(":memory:").unwrap();
    let own = SiteProps {
        crs_code: "EPSG:2056".into(),
        ..Default::default()
    };
    keep.set_site(&own).unwrap();
    import_ifc(&mut keep, exported(&m).as_bytes()).unwrap();
    assert_eq!(keep.site().unwrap().crs_code, "EPSG:2056");
}

/// Top-level attributes of the STEP line that holds `class`.
fn attrs(t: &str, class: &str) -> Vec<String> {
    let l = t
        .lines()
        .find(|l| l.contains(&format!("={class}(")))
        .unwrap_or_else(|| panic!("no {class}"));
    let body = &l[l.find('(').unwrap() + 1..l.rfind(')').unwrap()];
    let (mut out, mut depth, mut cur, mut quoted) = (vec![], 0, String::new(), false);
    for ch in body.chars() {
        match ch {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    out.push(cur);
    out
}

fn house_with_grid_and_colour() -> Model {
    let mut m = house();
    m.add_grid(&ddc_model::GridSpec {
        name: None,
        a: (0.0, -1.0),
        b: (0.0, 7.0),
        bulge: 0.0,
        base_level: None,
        top_level: None,
        bubble_start: true,
        bubble_end: true,
    })
    .unwrap();
    m.set_material("brick", &ddc_model::MaterialSpec {
        name: "brick".into(),
        color: Some(0xAA5533),
        hatch: "none".into(),
        density: None,
        conductivity: None,
        specific_heat: None,
        note: String::new(),
    })
    .unwrap();
    m
}

#[test]
fn ifc2x3_project_and_grid_have_the_schema_attribute_counts() {
    let m = house_with_grid_and_colour();
    let t = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap().0;
    let p = attrs(&t, "IFCPROJECT");
    assert_eq!(p.len(), 9, "{p:?}");
    assert!(p[7].starts_with("(#"), "contexts: {}", p[7]);
    let units = p[8].trim_start_matches('#');
    assert!(t.contains(&format!("#{units}=IFCUNITASSIGNMENT(")), "units: {}", p[8]);
    assert_eq!(attrs(&t, "IFCGRID").len(), 10);
    let t4 = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    assert_eq!(attrs(&t4, "IFCPROJECT").len(), 9);
    assert_eq!(attrs(&t4, "IFCGRID").len(), 11);
    assert_clean(&t, "ifc2x3 grid");
}

#[test]
fn ifc2x3_styles_go_through_presentation_style_assignments() {
    let m = house_with_grid_and_colour();
    let t = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap().0;
    assert!(count(&t, "IFCSTYLEDITEM") >= 1);
    for l in t.lines().filter(|l| l.contains("=IFCSTYLEDITEM(")) {
        let id = l.split("(($").next().and_then(|_| l.split("(#").nth(1));
        let id = id.and_then(|x| x.split(')').next()).unwrap();
        assert!(
            t.contains(&format!("#{id}=IFCPRESENTATIONSTYLEASSIGNMENT((#")),
            "{l}"
        );
    }
    let t4 = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    assert_eq!(count(&t4, "IFCPRESENTATIONSTYLEASSIGNMENT"), 0);
}

#[test]
fn every_written_entity_is_in_the_schema_table() {
    let m = house_with_grid_and_colour();
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3, IfcSchema::Ifc2x3] {
        let t = export_ifc_with(&m, &opts(schema)).unwrap().0;
        let rep = validate_ifc(t.as_bytes());
        let bad: Vec<_> = rep
            .errors
            .iter()
            .filter(|e| matches!(e.rule, "entity-known" | "arity" | "attr-type"))
            .collect();
        assert!(bad.is_empty(), "{schema:?}: {bad:?}");
    }
    // a class outside the table, and a class the schema lacks, are findings
    let t = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    let at = t.rfind("ENDSEC;").unwrap();
    let odd = format!("{}#99999=IFCSURELYUNKNOWN(1);\n{}", &t[..at], &t[at..]);
    let rep = validate_ifc(odd.as_bytes());
    assert!(rep.errors.iter().any(|e| e.rule == "entity-known" && e.entity == 99999));
    let t2 = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap().0;
    let at = t2.rfind("ENDSEC;").unwrap();
    let door = format!("{}#99998=IFCDOORTYPE($);\n{}", &t2[..at], &t2[at..]);
    let rep = validate_ifc(door.as_bytes());
    assert!(rep.errors.iter().any(|e| e.rule == "entity-known" && e.entity == 99998));
}

#[test]
fn type_ids_differ_between_projects() {
    let guids = |m: &Model| -> BTreeSet<String> {
        let t = export_ifc_with(m, &opts(IfcSchema::Ifc4)).unwrap().0;
        t.lines()
            .filter(|l| l.contains("=IFCWALLTYPE(") || l.contains("=IFCSLABTYPE("))
            .map(|l| l.split('\'').nth(1).unwrap().to_string())
            .collect()
    };
    let (a, b) = (house(), house());
    let ga = guids(&a);
    assert!(!ga.is_empty());
    assert_eq!(ga, guids(&a), "same project, same ids");
    assert!(ga.is_disjoint(&guids(&b)), "merged projects must not share type ids");
}

#[test]
fn standard_common_properties_use_their_defined_types() {
    use rusqlite::types::Value;
    let mut m = house();
    let w: i64 = m
        .sql()
        .query_row("SELECT min(id) FROM wall", [], |r| r.get(0))
        .unwrap();
    for (k, v) in [
        ("Pset_WallCommon.FireRating", Value::Text("EI60".into())),
        ("Pset_WallCommon.Reference", Value::Text("W-1".into())),
        ("Pset_WallCommon.Note", Value::Text("free text".into())),
    ] {
        m.set_prop("wall", w, k, Some((v, ""))).unwrap();
    }
    let t = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    assert!(t.contains("IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('EI60'),$)"));
    assert!(t.contains("IFCPROPERTYSINGLEVALUE('Reference',$,IFCIDENTIFIER('W-1'),$)"));
    assert!(t.contains("IFCPROPERTYSINGLEVALUE('Note',$,IFCTEXT('free text'),$)"));
}

#[test]
fn the_header_time_is_pinned_only_by_the_option() {
    let m = house();
    let a = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    let b = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    assert_eq!(a, b, "fixed time stamp: identical bytes");
    assert!(a.contains(&format!("'{STAMP}'")));
}

#[test]
fn attribute_kinds_are_checked_against_the_schema() {
    let m = house();
    let t = exported(&m);
    let at = t.rfind("ENDSEC;").unwrap();
    let with = |line: &str| format!("{}{line}\n{}", &t[..at], &t[at..]);
    let bad = |line: &str| {
        let rep = validate_ifc(with(line).as_bytes());
        rep.errors.iter().any(|e| e.rule == "attr-type" && e.entity == 99990)
    };
    assert!(!bad("#99990=IFCDIRECTION((0.,0.,1.));"));
    assert!(bad("#99990=IFCDIRECTION($);"), "$ in a required attribute");
    assert!(bad("#99990=IFCDIRECTION('x');"), "literal where a list is declared");
    assert!(bad("#99990=IFCCARTESIANPOINT((0,0,0));"), "integers where reals are declared");
    assert!(bad("#99990=IFCCARTESIANPOINT(#1);"), "reference where a list is declared");
    assert!(bad("#99990=IFCSIUNIT(.LENGTHUNIT.,.LENGTHUNIT.,$,.METRE.);"), "derived must be *");
    assert!(bad("#99990=IFCSIUNIT(*,.LENGTHUNIT.,$,.NOSUCHNAME.);"), "enumeration value");
    assert!(!bad("#99990=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);"));
}

fn house_with_zone_and_separator() -> Model {
    let mut m = house();
    let (level, room): (i64, i64) = {
        let sql = m.sql();
        (
            sql.query_row("SELECT min(id) FROM level", [], |r| r.get(0)).unwrap(),
            sql.query_row("SELECT min(id) FROM room", [], |r| r.get(0)).unwrap(),
        )
    };
    m.add_zone(&ddc_model::ZoneSpec {
        name: "Zone A".into(),
        rooms: vec![room],
    })
    .unwrap();
    m.add_room_sep(&ddc_model::SepSpec {
        level,
        a: (1.0, 1.0),
        b: (3.0, 1.0),
    })
    .unwrap();
    m
}

#[test]
fn group_assignments_leave_related_objects_type_empty_in_every_schema() {
    let m = house_with_zone_and_separator();
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3, IfcSchema::Ifc2x3] {
        let t = export_ifc_with(&m, &opts(schema)).unwrap().0;
        let a = attrs(&t, "IFCRELASSIGNSTOGROUP");
        assert_eq!(a.len(), 7, "{schema:?}");
        assert_eq!(a[5], "$", "{schema:?}: RelatedObjectsType");
        assert_clean(&t, "group assignment");
    }
}

#[test]
fn doors_use_operation_types_that_exist_in_ifc4x3() {
    let m = house();
    let t = export_ifc_with(&m, &opts(IfcSchema::Ifc4x3)).unwrap().0;
    assert!(!t.contains("DOUBLE_PANEL"), "RC names are not in the released schema");
    let door = attrs(&t, "IFCDOOR");
    assert_eq!(door.len(), 13);
    assert!(door[11].starts_with(".SINGLE_SWING_"), "{}", door[11]);
    let ty = attrs(&t, "IFCDOORTYPE");
    assert_eq!(ty.len(), 13);
    assert_clean(&t, "ifc4x3 doors");
}

#[test]
fn virtual_elements_have_a_predefined_type_only_in_ifc4x3() {
    let m = house_with_zone_and_separator();
    let n = |schema| {
        let t = export_ifc_with(&m, &opts(schema)).unwrap().0;
        assert_clean(&t, "separator");
        attrs(&t, "IFCVIRTUALELEMENT").len()
    };
    assert_eq!(n(IfcSchema::Ifc4x3), 9);
    assert_eq!(n(IfcSchema::Ifc4), 8);
    assert_eq!(n(IfcSchema::Ifc2x3), 8);
}

/// Two columns and a beam between them with a stirrup set: bars and the analytical model.
fn frame_with_bars() -> Model {
    use ddc_model::{RebarHook, RebarLayout, RebarPath, RebarSetSpec};
    let mut m = Model::open(":memory:").unwrap();
    let l0 = m.add_level("L0", 0.0).unwrap();
    let l1 = m.add_level("L1", 3.0).unwrap();
    let ct = m
        .add_member_type("RC 400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
        .unwrap();
    let bt = m
        .add_member_type("RC 300x500", &MemberShape::Rect { b: 0.3, h: 0.5 }, "Concrete")
        .unwrap();
    for x in [0.0, 6.0] {
        m.add_column(&ColumnSpec {
            level: l0,
            top_level: Some(l1),
            member_type: ct,
            structural: true,
            at: (x, 0.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    }
    let b = m
        .add_beam(&BeamSpec {
            level: l0,
            member_type: bt,
            a: (0.0, 0.0, 3.0),
            b: (6.0, 0.0, 3.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    let t8 = m
        .rebar_types()
        .unwrap()
        .into_iter()
        .find(|t| (t.d - 0.008).abs() < 1e-9)
        .unwrap();
    m.add_rebar_sets(&[RebarSetSpec {
        host_kind: "beam".into(),
        host: b,
        rtype: t8.id,
        layout: RebarLayout::Stirrup {
            rule: RebarPath::Fixed(4),
            hook: RebarHook::H135,
        },
        mark: Some("S1".into()),
    }])
    .unwrap();
    m
}

#[test]
fn reinforcement_and_analytical_model_are_translated_for_ifc2x3() {
    let m = frame_with_bars();
    let t4 = export_ifc_with(&m, &opts(IfcSchema::Ifc4)).unwrap().0;
    let (t, warnings) = export_ifc_with(&m, &opts(IfcSchema::Ifc2x3)).unwrap();
    assert_clean(&t4, "frame ifc4");
    assert_clean(&t, "frame ifc2x3");
    for class in [
        "IFCREINFORCINGBAR",
        "IFCSTRUCTURALANALYSISMODEL",
        "IFCSTRUCTURALCURVEMEMBER",
        "IFCSTRUCTURALPOINTCONNECTION",
        "IFCRELCONNECTSSTRUCTURALMEMBER",
        "IFCBOUNDARYNODECONDITION",
        "IFCRELASSIGNSTOGROUP",
    ] {
        let n = count(&t4, class);
        assert!(n > 0, "{class} missing in IFC4");
        assert_eq!(count(&t, class), n, "{class}");
    }
    // the axis of each curve member is kept in a property set, not lost
    let members = count(&t, "IFCSTRUCTURALCURVEMEMBER");
    assert_eq!(t.matches("'DDC_StructuralMember'").count(), members);
    assert!(t.contains("'LocalAxisZ'"));
    // IfcBoundaryNodeCondition of IFC2X3: -1 is a fixed degree of freedom, 0 a free one
    let cond: Vec<&str> = t.lines().filter(|l| l.contains("=IFCBOUNDARYNODECONDITION(")).collect();
    assert!(cond.iter().any(|l| l.contains(",-1.,") || l.contains(",-1.)")), "{cond:?}");
    assert!(cond.iter().all(|l| !l.contains("IFCBOOLEAN")), "{cond:?}");
    assert!(warnings.iter().any(|w| w.contains("no axis")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("stiffness")), "{warnings:?}");
    // the shared placement of the analysis model goes to a property set, with a warning
    assert!(warnings.iter().any(|w| w.contains("shared placement")), "{warnings:?}");
    let model_sets = pset_props(&t, "DDC_AnalysisModel");
    assert_eq!(model_sets.len(), count(&t, "IFCSTRUCTURALANALYSISMODEL"));
    for n in ["LocationX", "LocationY", "LocationZ", "AxisZ", "RefDirectionX"] {
        assert!(model_sets[0].iter().any(|p| p == n), "{n} in {:?}", model_sets[0]);
    }
    // sweep of the bar: the representation type IFC2X3 knows
    assert!(t.contains("'SweptSolid'") && !t.contains("AdvancedSweptSolid"));
    assert_eq!(count(&t, "IFCSWEPTDISKSOLID"), count(&t4, "IFCSWEPTDISKSOLID"));
    assert_eq!(count(&t, "IFCINDEXEDPOLYCURVE"), 0);
}

/// Names of the properties of every property set called `set`.
fn pset_props(t: &str, set: &str) -> Vec<Vec<String>> {
    let by_id: std::collections::HashMap<&str, &str> =
        t.lines().filter_map(|l| l.split_once('=')).collect();
    t.lines()
        .filter(|l| l.contains("=IFCPROPERTYSET(") && l.contains(&format!(",'{set}',")))
        .map(|l| {
            let list = l.rsplit_once(",(").unwrap().1.trim_end_matches("));");
            list.split(',')
                .filter(|id| !id.is_empty())
                .map(|id| by_id[id].split('\'').nth(1).unwrap().to_string())
                .collect()
        })
        .collect()
}

#[test]
fn a_stored_set_takes_the_missing_common_properties_instead_of_blocking_them() {
    let mut m = house();
    let slab: i64 = m
        .sql()
        .query_row("SELECT min(id) FROM slab", [], |r| r.get(0))
        .unwrap();
    let fire = rusqlite::types::Value::Text("REI90".into());
    m.set_prop("slab", slab, "Pset_SlabCommon.FireRating", Some((fire, ""))).unwrap();
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc2x3] {
        let (t, _) = export_ifc_with(&m, &opts(schema)).unwrap();
        assert_clean(&t, "merged sets");
        let sets = pset_props(&t, "Pset_SlabCommon");
        let stored: Vec<_> = sets.iter().filter(|p| p.iter().any(|n| n == "FireRating")).collect();
        assert_eq!(stored.len(), 1, "{schema:?}: {sets:?}");
        assert!(stored[0].iter().any(|n| n == "IsExternal"), "{schema:?}: {stored:?}");
        for set in &sets {
            let names: BTreeSet<_> = set.iter().collect();
            assert_eq!(names.len(), set.len(), "{schema:?}: duplicate property in {set:?}");
        }
    }
}
