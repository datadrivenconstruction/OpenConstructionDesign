// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::HashMap;

use calamine::{open_workbook, Data, DataType, Reader, Xlsx};
use ddc_model::{ColumnSpec, LayerSpec, MemberShape, Model, OpeningSpec, RoomSpec, SlabSpec, StairSpec, WallSpec};

use super::*;

fn row(key: &str) -> ElementRow {
    ElementRow {
        key: key.into(),
        element_type: "Wall".into(),
        ifc_class: "IfcWall".into(),
        type_key: "wall/ext".into(),
        name: "Wall".into(),
        storey: Some("EG".into()),
        space: None,
        discipline: "architecture".into(),
        material: Some("brick".into()),
        phase: Some("new".into()),
        space_usage: None,
        geometry_hash: "0123456789abcdef".into(),
        length_m: Some(5.0),
        area_m2: Some(12.5),
        area_gross_m2: Some(15.0),
        volume_m3: Some(3.0),
        volume_gross_m3: Some(3.5),
        weight_kg: Some(7200.0),
        count: Some(1),
        bbox: [0.0, 0.0, -4.0, 5.0, 3.0, 0.0],
        props: Some("{\"fire\":\"REI 90\",\"u\":0.21}".into()),
    }
}

fn bytes(rows: &[ElementRow]) -> (Vec<u8>, Vec<Note>) {
    let mut v = vec![];
    let n = write_parquet(rows, &mut v).unwrap();
    (v, n)
}

#[test]
fn every_column_type_survives_a_round_trip_bit_for_bit() {
    let mut a = row("b1/s00/wall-N/0");
    let mut b = row("b1/s00/wall-S/0");
    b.storey = None;
    b.space = Some("b1/s00/u01/room-hall".into());
    b.material = None;
    b.phase = None;
    (b.length_m, b.area_m2, b.area_gross_m2) = (None, None, None);
    (b.volume_m3, b.volume_gross_m3, b.weight_kg, b.count, b.props) = (None, None, None, None, None);
    a.length_m = Some(0.1 + 0.2); // not exactly 0.3
    a.bbox = [f64::MIN_POSITIVE, -1e12, 1e-300, 1e300, 0.1, std::f64::consts::PI];
    let (data, notes) = bytes(&[a.clone(), b.clone()]);
    assert!(notes.is_empty(), "{notes:?}");
    let back = read_parquet(data).unwrap();
    assert_eq!(back, vec![a, b]);
}

#[test]
fn the_file_has_exactly_the_contract_columns_with_native_types() {
    let (data, _) = bytes(&[row("k1")]);
    let r = parquet::file::reader::SerializedFileReader::new(bytes::Bytes::from(data)).unwrap();
    use parquet::file::reader::FileReader;
    let schema = r.metadata().file_metadata().schema_descr();
    let cols: Vec<String> = schema.columns().iter().map(|c| c.name().to_string()).collect();
    let mut want: Vec<String> = COLUMNS.iter().map(|s| s.to_string()).collect();
    want.push("props".into());
    assert_eq!(cols, want);
    let schema_json: serde_json::Value =
        serde_json::from_str(include_str!("../../contract/schemas/element-row.schema.json")).unwrap();
    let p = schema_json["properties"].as_object().unwrap();
    let mut have: Vec<&str> = cols.iter().map(String::as_str).collect();
    let mut names: Vec<&str> = p.keys().map(String::as_str).collect();
    have.sort();
    names.sort();
    assert_eq!(have, names, "column set equals the contract schema");
    use parquet::basic::Type;
    for c in schema.columns() {
        let want = match c.name() {
            "count" => Type::INT64,
            n if n.starts_with("bbox_") || n.ends_with("_m") || n.ends_with("_m2") || n.ends_with("_m3") || n == "weight_kg" => Type::DOUBLE,
            _ => Type::BYTE_ARRAY,
        };
        assert_eq!(c.physical_type(), want, "{}", c.name());
    }
    let codec = r.metadata().row_group(0).column(0).compression();
    assert!(matches!(codec, parquet::basic::Compression::ZSTD(_)), "{codec:?}");
}

#[test]
fn an_empty_model_is_a_valid_file_with_no_rows() {
    let (data, _) = bytes(&[]);
    assert!(data.len() > 100);
    assert_eq!(read_parquet(data).unwrap(), vec![]);
    let mut m = Model::open(":memory:").unwrap();
    let c = collect(&mut m, &GeoMap::new()).unwrap();
    assert!(c.rows.is_empty());
}

#[test]
fn unicode_and_awkward_text_come_back_unchanged() {
    let mut r = row("b1/s00/wall-1");
    r.name = "Außenwand 36,5 — 外墙 \u{1F9F1} \"quoted\" 'x' \\ \n tab\t".into();
    r.storey = Some(format!("1.OG / Erdgeschoss {}", crate::ru_fixtures::ru("hedgehog")));
    r.material = Some("Mauerwerk 摩洛哥".into());
    r.props = Some(serde_json::json!({"Brandschutz": "REI 90", "メモ": "テスト", "n": 1.5}).to_string());
    let empty = ElementRow { name: String::new(), storey: Some(String::new()), ..row("b1/s00/wall-2") };
    let (data, _) = bytes(&[r.clone(), empty.clone()]);
    assert_eq!(read_parquet(data).unwrap(), vec![r, empty]);
}

#[test]
fn nan_infinite_and_negative_quantities_become_null_with_a_note() {
    let mut r = row("k1");
    r.length_m = Some(f64::NAN);
    r.area_m2 = Some(f64::INFINITY);
    r.area_gross_m2 = Some(f64::NEG_INFINITY);
    r.volume_m3 = Some(-0.5);
    r.volume_gross_m3 = Some(0.0);
    r.weight_kg = Some(-0.0);
    r.count = Some(-3);
    r.bbox = [0.0, f64::NAN, 0.0, 1.0, 1.0, f64::INFINITY];
    let (data, notes) = bytes(&[r]);
    let fields: Vec<&str> = notes.iter().map(|n| n.field).collect();
    assert_eq!(fields, ["length_m", "area_m2", "area_gross_m2", "volume_m3", "count", "bbox"], "{notes:?}");
    assert!(notes.iter().all(|n| n.key == "k1" && !n.what.is_empty()));
    let b = read_parquet(data).unwrap().remove(0);
    assert_eq!((b.length_m, b.area_m2, b.area_gross_m2, b.volume_m3, b.count), (None, None, None, None, None));
    assert_eq!(b.volume_gross_m3, Some(0.0), "zero stays zero");
    assert_eq!(b.weight_kg.map(f64::to_bits), Some(0.0f64.to_bits()), "negative zero is written as zero");
    assert_eq!(b.bbox, [0.0; 6]);
}

#[test]
fn zero_area_elements_keep_their_zero() {
    let mut r = row("k1");
    (r.area_m2, r.area_gross_m2, r.volume_m3, r.length_m, r.count) = (Some(0.0), Some(0.0), Some(0.0), Some(0.0), Some(0));
    r.bbox = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
    let (data, notes) = bytes(&[r.clone()]);
    assert!(notes.is_empty());
    assert_eq!(read_parquet(data).unwrap(), vec![r]);
}

#[test]
fn a_huge_props_cell_and_many_rows_round_trip() {
    let mut big = row("big");
    let blob: String = (0..60_000).map(|i| format!("\"p{i}\":{i}")).collect::<Vec<_>>().join(",");
    big.props = Some(format!("{{{blob}}}"));
    assert!(big.props.as_ref().unwrap().len() > 600_000);
    let mut rows = vec![big];
    for i in 0..(ROW_GROUP_MAX + 10) {
        let mut r = row(&format!("e/{i}"));
        r.count = Some(i as i64);
        rows.push(r);
    }
    let (data, _) = bytes(&rows);
    let r = parquet::file::reader::SerializedFileReader::new(bytes::Bytes::from(data.clone())).unwrap();
    use parquet::file::reader::FileReader;
    assert_eq!(r.metadata().num_row_groups(), 2, "50 000 rows per group at most");
    assert!(r.metadata().row_groups().iter().all(|g| g.num_rows() as usize <= ROW_GROUP_MAX));
    assert_eq!(read_parquet(data).unwrap(), rows);
}

#[test]
fn the_same_rows_give_the_same_bytes() {
    let rows = vec![row("a"), row("b"), row("c")];
    assert_eq!(bytes(&rows).0, bytes(&rows).0);
}

#[test]
fn malformed_rows_are_refused_and_nothing_is_written() {
    let bad = |f: &dyn Fn(&mut ElementRow)| {
        let mut r = row("ok");
        f(&mut r);
        let mut out = vec![];
        let e = write_parquet(&[r], &mut out).unwrap_err();
        assert!(out.is_empty(), "no partial file");
        e
    };
    assert!(bad(&|r| r.key = String::new()).contains("grammar"));
    assert!(bad(&|r| r.key = "a b".into()).contains("grammar"));
    assert!(bad(&|r| r.key = "x".repeat(256)).contains("grammar"));
    assert!(bad(&|r| r.key = "/lead".into()).contains("grammar"));
    assert!(bad(&|r| r.discipline = "plumbing".into()).contains("discipline"));
    assert!(bad(&|r| r.phase = Some("old".into())).contains("phase"));
    assert!(bad(&|r| r.space_usage = Some("living".into())).contains("space_usage"), "a wall has no use");
    assert!(bad(&|r| {
        r.ifc_class = "IfcSpace".into();
        r.space_usage = Some("hangar".into());
    })
    .contains("space_usage"));
    assert!(bad(&|r| r.ifc_class = "Wall".into()).contains("IFC class"));
    assert!(bad(&|r| r.geometry_hash = "XYZ".into()).contains("geometry_hash"));
    assert!(bad(&|r| r.props = Some("[1]".into())).contains("props"));
    assert!(bad(&|r| r.props = Some("{broken".into())).contains("props"));
    assert!(bad(&|r| r.type_key = String::new()).contains("type_key"));
    let mut out = vec![];
    let e = write_parquet(&[row("dup"), row("dup")], &mut out).unwrap_err();
    assert!(e.contains("twice"), "{e}");
}

fn footer_file(meta: &[u8], declared_len: u32) -> Vec<u8> {
    let mut f = b"PAR1".to_vec();
    f.extend_from_slice(meta);
    f.extend_from_slice(&declared_len.to_le_bytes());
    f.extend_from_slice(b"PAR1");
    f
}

#[test]
fn a_file_declaring_a_huge_length_is_refused_without_allocating_it() {
    // version field, then created_by (a string) declaring about 2^40 bytes with none present
    let meta = [0x15, 0x02, 0x58, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x1F, b'x'];
    let e = read_parquet(footer_file(&meta, meta.len() as u32)).unwrap_err();
    assert!(!e.is_empty());
    // a footer longer than the whole file
    assert!(read_parquet(footer_file(&meta, u32::MAX)).is_err());
    // a footer whose list claims billions of schema elements
    let list = [0x29, 0xFC, 0xFF, 0xFF, 0xFF, 0x0F];
    assert!(read_parquet(footer_file(&list, list.len() as u32)).is_err());
}

#[test]
fn global_id_characters_are_keys() {
    for k in ["2Gx1$_ab-CD0123456789", "b1/s03/u-2B/room-kitchen#1", "a.b:c"] {
        assert!(key_ok(k), "{k}");
        let (d, _) = bytes(&[row(k)]);
        assert_eq!(read_parquet(d).unwrap()[0].key, k);
    }
}

// ---- rows from a model, checked against the xlsx export of the same model ----

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l0 = m.add_level("EG", 0.0).unwrap();
    let l1 = m.add_level("1.OG", 3.0).unwrap();
    let layers = [("Mauerwerk", 0.30), ("Dämmung", 0.10)].map(|(n, t)| LayerSpec { material: n.into(), thickness: t });
    let wt = m.add_wall_type("ext-masonry-365", &layers).unwrap();
    let it = m.add_wall_type("int", &[LayerSpec { material: "Gips".into(), thickness: 0.1 }]).unwrap();
    let st = m.add_slab_type("concrete-200", &[LayerSpec { material: "Stahlbeton".into(), thickness: 0.2 }]).unwrap();
    let ct = m.add_member_type("rc-300", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Stahlbeton").unwrap();
    let mut walls = vec![];
    for lv in [l0, l1] {
        let w = |a, b, t| WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
        walls.extend(
            m.add_walls(&[
                w((0.0, 0.0), (8.0, 0.0), wt),
                w((8.0, 0.0), (8.0, 6.0), wt),
                w((8.0, 6.0), (0.0, 6.0), wt),
                w((0.0, 6.0), (0.0, 0.0), wt),
                w((4.0, 0.0), (4.0, 6.0), it),
            ])
            .unwrap(),
        );
        m.add_room(&RoomSpec { level: lv, name: "Küche".into(), number: "01".into(), point: (2.0, 3.0), height: 2.7 }).unwrap();
        m.add_room(&RoomSpec { level: lv, name: "Wohnen 居".into(), number: "02".into(), point: (6.0, 3.0), height: 2.7 }).unwrap();
        m.add_slab(&SlabSpec { level: lv, slab_type: st, offset: 0.0, outer: vec![(-0.2, -0.2), (8.2, -0.2), (8.2, 6.2), (-0.2, 6.2)], holes: vec![] }).unwrap();
        m.add_column(&ColumnSpec {
            level: lv,
            top_level: None,
            member_type: ct,
            structural: true,
            at: (4.0, 3.0),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    }
    m.add_opening(walls[0], &OpeningSpec { along: 2.0, width: 1.0, sill: 0.9, height: 1.2, kind: "window".into() }).unwrap();
    m.add_opening(walls[4], &OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() }).unwrap();
    m.add_stair(&StairSpec::straight(l0, l1, (1.0, 1.0), std::f64::consts::FRAC_PI_2)).unwrap();
    m
}

fn sheet(path: &std::path::Path, name: &str) -> Vec<HashMap<String, f64>> {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let r = wb.worksheet_range(name).unwrap();
    let rows: Vec<&[Data]> = r.rows().collect();
    let h = rows.iter().position(|x| matches!(x.first(), Some(Data::String(s)) if s == "id")).expect("header row");
    let head: Vec<String> = rows[h].iter().map(|c| c.to_string()).collect();
    rows[h + 1..]
        .iter()
        .filter(|x| !x.is_empty() && !matches!(x[0], Data::Empty))
        .map(|x| head.iter().cloned().zip(x.iter().map(|c| c.as_f64().unwrap_or(f64::NAN))).collect())
        .collect()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * (1.0 + b.abs())
}

#[test]
fn model_rows_equal_the_xlsx_export_value_by_value() {
    let mut m = model();
    let p = std::env::temp_dir().join(format!("ddc_pq_{}.xlsx", std::process::id()));
    crate::xlsx::export_xlsx(&m, &p).unwrap();
    let c = collect(&mut m, &GeoMap::new()).unwrap();
    // keys are unique and all of them minted (the model came from the API, not from a generator)
    let keys: std::collections::HashSet<&String> = c.rows.iter().map(|r| &r.key).collect();
    assert_eq!(keys.len(), c.rows.len());
    assert!(c.rows.iter().all(|r| r.key.starts_with("x/")), "{:?}", c.rows[0].key);

    let walls = sheet(&p, "Walls");
    let wrows: Vec<&ElementRow> = c.rows.iter().filter(|r| r.ifc_class == "IfcWall").collect();
    assert_eq!(wrows.len(), walls.len());
    assert_eq!(walls.len(), 10);
    for (r, x) in wrows.iter().zip(&walls) {
        assert!(near(r.length_m.unwrap(), x["length [m] (read-only)"]), "{} length", r.key);
        let h = x["height [m]"];
        assert!(near(r.area_gross_m2.unwrap(), x["length [m] (read-only)"] * h));
        assert!(r.area_m2.unwrap() <= r.area_gross_m2.unwrap() + 1e-9);
        assert!(r.volume_m3.unwrap() <= r.volume_gross_m3.unwrap() + 1e-9);
    }
    // the wall that holds the window loses its area
    let holed = wrows.iter().filter(|r| r.area_m2.unwrap() + 1e-9 < r.area_gross_m2.unwrap()).count();
    assert_eq!(holed, 2, "window wall and door wall");

    let rooms = sheet(&p, "Rooms");
    let rrows: Vec<&ElementRow> = c.rows.iter().filter(|r| r.ifc_class == "IfcSpace").collect();
    assert_eq!(rrows.len(), 4);
    assert_eq!(rooms.len(), 4);
    for (r, x) in rrows.iter().zip(&rooms) {
        assert!(near(r.area_m2.unwrap(), x["area [m2] (read-only)"]), "{} area", r.key);
        assert!(near(r.length_m.unwrap(), x["perimeter [m] (read-only)"]));
        assert!(near(r.volume_m3.unwrap(), x["volume [m3] (read-only)"]));
        assert_eq!(r.type_key.split('/').next(), Some("space"));
        assert_ne!(r.geometry_hash, GEOMETRY_NONE, "a room has a model-derived box");
        let b = r.bbox;
        assert!(b[0] < b[3] && b[1] < b[4] && b[2] < b[5], "{b:?}");
    }
    let slabs = sheet(&p, "Slabs");
    let srows: Vec<&ElementRow> = c.rows.iter().filter(|r| r.ifc_class == "IfcSlab").collect();
    assert_eq!(srows.len(), 2);
    for (r, x) in srows.iter().zip(&slabs) {
        assert!(near(r.area_m2.unwrap(), x["plan area [m2] (read-only)"]), "{} area", r.key);
        assert!(near(r.volume_m3.unwrap(), x["volume [m3] (read-only)"]));
        assert_eq!(r.material.as_deref(), Some("Stahlbeton"));
    }
    let ops = sheet(&p, "Openings");
    let orows: Vec<&ElementRow> = c.rows.iter().filter(|r| matches!(r.ifc_class.as_str(), "IfcDoor" | "IfcWindow")).collect();
    assert_eq!(orows.len(), ops.len());
    for (r, x) in orows.iter().zip(&ops) {
        assert!(near(r.area_m2.unwrap(), x["width [m]"] * x["height [m]"]));
    }
    assert_eq!(by_ifc_count(&c, "IfcColumn"), 2);
    assert_eq!(by_ifc_count(&c, "IfcStair"), 1);
    // each column stands inside the room that holds its point
    let col = c.rows.iter().find(|r| r.ifc_class == "IfcColumn").unwrap();
    assert!(col.length_m.unwrap() > 2.9 && col.volume_m3.unwrap() > 0.0);
    let _ = std::fs::remove_file(&p);

    // and the whole table survives the file
    let mut data = vec![];
    let notes = write_parquet(&c.rows, &mut data).unwrap();
    assert!(notes.iter().all(|n| n.field == "bbox"), "only the missing-geometry notes: {notes:?}");
    assert_eq!(read_parquet(data).unwrap(), c.rows);
}

/// The row as the ERP reads it: the same names and types as the Parquet columns.
fn row_json(r: &ElementRow) -> serde_json::Value {
    use serde_json::json;
    json!({
        "id": r.key, "stable_id": r.key, "element_key": r.key, "element_type": r.element_type,
        "ifc_class": r.ifc_class, "type_key": r.type_key, "name": r.name, "storey": r.storey,
        "space": r.space, "discipline": r.discipline, "material": r.material, "phase": r.phase,
        "space_usage": r.space_usage, "geometry_hash": r.geometry_hash, "length_m": r.length_m,
        "area_m2": r.area_m2, "area_gross_m2": r.area_gross_m2, "volume_m3": r.volume_m3,
        "volume_gross_m3": r.volume_gross_m3, "weight_kg": r.weight_kg, "count": r.count,
        "bbox_min_x": r.bbox[0], "bbox_min_y": r.bbox[1], "bbox_min_z": r.bbox[2],
        "bbox_max_x": r.bbox[3], "bbox_max_y": r.bbox[4], "bbox_max_z": r.bbox[5], "props": r.props,
    })
}

#[test]
fn every_row_of_a_model_validates_against_the_element_row_schema() {
    use crate::contract::{validate, Doc};
    let mut m = model();
    let c = collect(&mut m, &GeoMap::new()).unwrap();
    assert!(c.rows.len() > 20);
    for r in &c.rows {
        validate(Doc::ElementRow, &row_json(r)).unwrap_or_else(|e| panic!("{}: {e:?}", r.key));
    }
}

#[test]
fn the_widened_key_grammar_and_the_space_use_survive_the_file_and_the_schema() {
    use crate::contract::{validate, Doc};
    for k in ["_abc", "$x", "a$b", "2Ab_x", "b1/s00/u01#2:r.3-4"] {
        assert!(key_ok(k), "{k}");
    }
    for k in ["", "-a", ".a", "a b", "a|b", "ä"] {
        assert!(!key_ok(k), "{k}");
    }
    let mut s = row("_a$b/c:d#1");
    (s.ifc_class, s.element_type, s.type_key) = ("IfcSpace".into(), "Room".into(), "space".into());
    s.space_usage = Some("living".into());
    s.geometry_hash = "ab".repeat(32);
    let (data, _) = bytes(&[s.clone()]);
    assert_eq!(read_parquet(data).unwrap(), vec![s.clone()]);
    validate(Doc::ElementRow, &row_json(&s)).unwrap();
    s.space_usage = Some("hangar".into());
    assert!(validate(Doc::ElementRow, &row_json(&s)).is_err(), "the schema refuses an unknown use");
}

fn by_ifc_count(c: &Collected, class: &str) -> usize {
    c.rows.iter().filter(|r| r.ifc_class == class).count()
}

#[test]
fn keys_written_by_a_generator_win_and_are_not_minted_again() {
    use rusqlite::types::Value;
    let mut m = model();
    let first = collect(&mut m, &GeoMap::new()).unwrap();
    let wall = first.rows.iter().find(|r| r.ifc_class == "IfcWall").unwrap().key.clone();
    assert!(wall.starts_with("x/wall/"));
    // same model, collected twice: the minted keys are kept
    let again = collect(&mut m, &GeoMap::new()).unwrap();
    assert_eq!(first.rows.iter().map(|r| &r.key).collect::<Vec<_>>(), again.rows.iter().map(|r| &r.key).collect::<Vec<_>>());
    m.set_prop("wall", 1, ddc_model::KEY_PROP, Some((Value::Text("b1/s00/wall-N/0".into()), ""))).unwrap();
    let third = collect(&mut m, &GeoMap::new()).unwrap();
    assert!(third.rows.iter().any(|r| r.key == "b1/s00/wall-N/0"));
    assert_eq!(third.rows.len(), first.rows.len());
}

#[test]
fn geometry_from_the_caller_replaces_the_model_box() {
    let mut m = model();
    let mut geo = GeoMap::new();
    geo.insert(("wall".into(), 1), Geo { bbox: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0], hash: "ffffffffffffffff".into() });
    let c = collect(&mut m, &geo).unwrap();
    let w = c.rows.iter().find(|r| r.bbox == [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).expect("box taken as given");
    assert_eq!(w.geometry_hash, "ffffffffffffffff");
    // walls without a mesh say so
    assert!(c.notes.iter().any(|n| n.field == "bbox"));
}

#[test]
fn properties_go_to_props_with_their_units_and_skip_the_key() {
    use rusqlite::types::Value;
    let mut m = model();
    m.set_prop("wall", 1, "Pset_WallCommon.FireRating", Some((Value::Text("REI 90".into()), ""))).unwrap();
    m.set_prop("wall", 1, "U-Value", Some((Value::Real(0.21), "W/m2K"))).unwrap();
    m.set_prop("wall", 1, ddc_model::KEY_PROP, Some((Value::Text("w1".into()), ""))).unwrap();
    let c = collect(&mut m, &GeoMap::new()).unwrap();
    let w = c.rows.iter().find(|r| r.key == "w1").unwrap();
    let j: serde_json::Value = serde_json::from_str(w.props.as_ref().unwrap()).unwrap();
    assert_eq!(j["Pset_WallCommon.FireRating"], "REI 90");
    assert_eq!(j["U-Value [W/m2K]"], 0.21);
    assert!(j.as_object().unwrap().len() == 2, "{j}");
}

#[test]
fn an_imported_global_id_is_the_key_and_an_unkeyed_element_is_minted_once() {
    let mut m = model();
    m.sql().execute("INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('f','3vB$_r9QD1Bgm4p0aN7x1z','slab',1)", []).unwrap();
    let c = collect(&mut m, &GeoMap::new()).unwrap();
    let s = c.rows.iter().find(|r| r.key == "3vB$_r9QD1Bgm4p0aN7x1z").expect("GlobalId is the key");
    assert_eq!(s.ifc_class, "IfcSlab");
    assert!(c.rows.iter().filter(|r| r.key.starts_with("x/")).count() >= 10);
}

#[test]
fn old_files_without_keys_get_the_same_minted_keys_after_save_and_reopen() {
    let dir = std::env::temp_dir().join(format!("ddc_pq_reopen_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("m.ddc");
    let mut m = model();
    let a = collect(&mut m, &GeoMap::new()).unwrap();
    m.save_project_to(&f, &mut |_, _| Ok(())).unwrap();
    let work = dir.join("work.ddc");
    let mut back = ddc_model::open_project_copy(&f, &work, false, &mut |_, _| Ok(())).unwrap().model;
    let b = collect(&mut back, &GeoMap::new()).unwrap();
    assert_eq!(a.rows.iter().map(|r| &r.key).collect::<Vec<_>>(), b.rows.iter().map(|r| &r.key).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn assistant_values_travel_in_props_as_ddc_provenance() {
    use ddc_model::{Edit, PVal, ParamSpec, ParamType, SrcKind, ValueSrc};
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.2 }]).unwrap();
    let spec = WallSpec { a: (0.0, 0.0), b: (6.0, 0.0), level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
    let w = m.add_wall(&spec).unwrap();
    m.add_wall(&WallSpec { a: (0.0, 5.0), b: (6.0, 5.0), ..spec }).unwrap();
    let p = ParamSpec {
        name: "Fire".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    };
    m.add_param(&p).unwrap();
    let edit = Edit::ParamValue { param: "Fire".into(), kind: "wall".into(), id: w, value: Some(PVal::Text("REI 90".into())) };
    m.apply_edits(&[edit]).unwrap();
    m.record_value_src(&[("wall", w, "Fire")], &ValueSrc::ai(SrcKind::Ai, 100, "s", "r1", "fire rating")).unwrap();
    let c = collect(&mut m, &GeoMap::new()).unwrap();
    let with: Vec<serde_json::Value> = c
        .rows
        .iter()
        .filter_map(|r| r.props.as_deref())
        .map(|p| serde_json::from_str::<serde_json::Value>(p).unwrap())
        .filter(|j| j.get("DDC_Provenance.AiValues").is_some())
        .collect();
    assert_eq!(with.len(), 1, "only the wall with an assistant value: {with:?}");
    assert_eq!(with[0]["DDC_Provenance.AiValuesUnverified"], 1, "{}", with[0]);
    assert_eq!(c.rows.iter().filter(|r| r.element_type == "Wall").count(), 2);
}
