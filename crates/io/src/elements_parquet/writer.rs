// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parquet writer and reader for [`ElementRow`]. Native types: strings, float64, int64.
//! zstd, row groups of at most [`ROW_GROUP_MAX`] rows, fixed `created_by`: the same rows give
//! the same bytes.

use std::io::Write;
use std::sync::Arc;

use parquet::basic::{Compression, ZstdLevel};
use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::file::writer::SerializedFileWriter;
use parquet::record::Field;
use parquet::schema::parser::parse_message_type;

use super::{key_ok, ElementRow, Note};

pub const ROW_GROUP_MAX: usize = 50_000;

/// Column names in file order (contract `element-row.schema.json`).
pub const COLUMNS: [&str; 27] = [
    "id", "stable_id", "element_key", "element_type", "ifc_class", "type_key", "name", "storey", "space",
    "discipline", "material", "phase", "space_usage", "geometry_hash", "length_m", "area_m2", "area_gross_m2", "volume_m3",
    "volume_gross_m3", "weight_kg", "count", "bbox_min_x", "bbox_min_y", "bbox_min_z", "bbox_max_x",
    "bbox_max_y", "bbox_max_z",
];
// `props` is the 28th and last column.

const MESSAGE: &str = "message ddc_elements {
  required binary id (STRING);
  required binary stable_id (STRING);
  required binary element_key (STRING);
  required binary element_type (STRING);
  required binary ifc_class (STRING);
  required binary type_key (STRING);
  required binary name (STRING);
  optional binary storey (STRING);
  optional binary space (STRING);
  required binary discipline (STRING);
  optional binary material (STRING);
  optional binary phase (STRING);
  optional binary space_usage (STRING);
  required binary geometry_hash (STRING);
  optional double length_m;
  optional double area_m2;
  optional double area_gross_m2;
  optional double volume_m3;
  optional double volume_gross_m3;
  optional double weight_kg;
  optional int64 count;
  required double bbox_min_x;
  required double bbox_min_y;
  required double bbox_min_z;
  required double bbox_max_x;
  required double bbox_max_y;
  required double bbox_max_z;
  optional binary props (STRING);
}";

const DISCIPLINES: [&str; 5] = ["architecture", "structure", "mep", "site", "other"];
const USAGES: [&str; 9] =
    ["living", "office", "production", "storage", "education", "care", "other_use", "circulation", "technical"];
const PHASES: [&str; 4] = ["new", "existing", "demolished", "temporary"];

fn err(e: impl std::fmt::Display) -> String {
    format!("parquet: {e}")
}

/// A quantity the contract can hold: finite and not negative. Anything else becomes null.
fn quantity(v: Option<f64>, key: &str, field: &'static str, notes: &mut Vec<Note>) -> Option<f64> {
    let x = v?;
    if x.is_finite() && x >= 0.0 {
        // -0.0 would compare unequal to 0.0 bit for bit
        return Some(if x == 0.0 { 0.0 } else { x });
    }
    notes.push(Note {
        key: key.into(),
        field,
        what: format!("{x} is not a finite non-negative quantity; written as null"),
    });
    None
}

fn checked(rows: &[ElementRow]) -> Result<(Vec<ElementRow>, Vec<Note>), String> {
    let mut notes = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if !key_ok(&r.key) {
            return Err(format!("element key '{}' is not in the contract grammar", r.key));
        }
        if !seen.insert(r.key.as_str()) {
            return Err(format!("element key '{}' occurs twice", r.key));
        }
        if !DISCIPLINES.contains(&r.discipline.as_str()) {
            return Err(format!("{}: discipline '{}' is not one of {DISCIPLINES:?}", r.key, r.discipline));
        }
        if let Some(p) = &r.phase {
            if !PHASES.contains(&p.as_str()) {
                return Err(format!("{}: phase '{p}' is not one of {PHASES:?}", r.key));
            }
        }
        if let Some(u) = &r.space_usage {
            if r.ifc_class != "IfcSpace" || !USAGES.contains(&u.as_str()) {
                return Err(format!("{}: space_usage '{u}' is only for IfcSpace, one of {USAGES:?}", r.key));
            }
        }
        if r.element_type.is_empty() || r.type_key.is_empty() {
            return Err(format!("{}: element_type and type_key must not be empty", r.key));
        }
        if !(r.ifc_class.starts_with("Ifc") && r.ifc_class.len() > 3 && r.ifc_class.chars().all(|c| c.is_ascii_alphabetic())) {
            return Err(format!("{}: '{}' is not an IFC class name", r.key, r.ifc_class));
        }
        if r.geometry_hash.len() < 16
            || r.geometry_hash.len() > 64
            || !r.geometry_hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        {
            return Err(format!("{}: geometry_hash must be 16 to 64 lower-case hex digits", r.key));
        }
        if let Some(p) = &r.props {
            if !serde_json::from_str::<serde_json::Value>(p).is_ok_and(|v| v.is_object()) {
                return Err(format!("{}: props is not a JSON object", r.key));
            }
        }
        let mut c = r.clone();
        let k = r.key.as_str();
        c.length_m = quantity(r.length_m, k, "length_m", &mut notes);
        c.area_m2 = quantity(r.area_m2, k, "area_m2", &mut notes);
        c.area_gross_m2 = quantity(r.area_gross_m2, k, "area_gross_m2", &mut notes);
        c.volume_m3 = quantity(r.volume_m3, k, "volume_m3", &mut notes);
        c.volume_gross_m3 = quantity(r.volume_gross_m3, k, "volume_gross_m3", &mut notes);
        c.weight_kg = quantity(r.weight_kg, k, "weight_kg", &mut notes);
        if c.count.is_some_and(|n| n < 0) {
            notes.push(Note { key: r.key.clone(), field: "count", what: "negative count; written as null".into() });
            c.count = None;
        }
        if c.bbox.iter().any(|x| !x.is_finite()) {
            notes.push(Note {
                key: r.key.clone(),
                field: "bbox",
                what: "bounding box is not finite; written as zeros".into(),
            });
            c.bbox = [0.0; 6];
        }
        for x in &mut c.bbox {
            if *x == 0.0 {
                *x = 0.0;
            }
        }
        out.push(c);
    }
    Ok((out, notes))
}

fn strings<'a>(it: impl Iterator<Item = Option<&'a str>>, required: bool) -> (Vec<ByteArray>, Option<Vec<i16>>) {
    let mut vals = vec![];
    let mut def = vec![];
    for v in it {
        match v {
            Some(s) => {
                vals.push(ByteArray::from(s));
                def.push(1i16);
            }
            None => def.push(0),
        }
    }
    (vals, if required { None } else { Some(def) })
}

fn doubles(it: impl Iterator<Item = Option<f64>>, required: bool) -> (Vec<f64>, Option<Vec<i16>>) {
    let mut vals = vec![];
    let mut def = vec![];
    for v in it {
        match v {
            Some(x) => {
                vals.push(x);
                def.push(1i16);
            }
            None => def.push(0),
        }
    }
    (vals, if required { None } else { Some(def) })
}

/// Writes `rows` as a Parquet file to `out`. Returns what was repaired on the way (non-finite or
/// negative quantities become null, a non-finite box becomes zeros); malformed rows (key, class,
/// hash, discipline) are an error and nothing is written.
pub fn write_parquet<W: Write + Send>(rows: &[ElementRow], out: W) -> Result<Vec<Note>, String> {
    let (rows, notes) = checked(rows)?;
    let schema = Arc::new(parse_message_type(MESSAGE).map_err(err)?);
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::try_new(3).map_err(err)?))
            .set_created_by("ddc-elements/1".into())
            .set_max_row_group_size(ROW_GROUP_MAX)
            .build(),
    );
    let mut w = SerializedFileWriter::new(out, schema, props).map_err(err)?;
    // an empty model is a file with a schema and no row group
    for chunk in rows.chunks(ROW_GROUP_MAX) {
        let mut rg = w.next_row_group().map_err(err)?;
        let mut col = 0usize;
        macro_rules! text {
            ($required:expr, $f:expr) => {{
                let mut c = rg.next_column().map_err(err)?.ok_or("parquet: schema has fewer columns")?;
                let (v, d) = strings(chunk.iter().map($f), $required);
                c.typed::<ByteArrayType>().write_batch(&v, d.as_deref(), None).map_err(err)?;
                c.close().map_err(err)?;
                col += 1;
            }};
        }
        macro_rules! real {
            ($required:expr, $f:expr) => {{
                let mut c = rg.next_column().map_err(err)?.ok_or("parquet: schema has fewer columns")?;
                let (v, d) = doubles(chunk.iter().map($f), $required);
                c.typed::<DoubleType>().write_batch(&v, d.as_deref(), None).map_err(err)?;
                c.close().map_err(err)?;
                col += 1;
            }};
        }
        text!(true, |r| Some(r.key.as_str()));
        text!(true, |r| Some(r.key.as_str()));
        text!(true, |r| Some(r.key.as_str()));
        text!(true, |r| Some(r.element_type.as_str()));
        text!(true, |r| Some(r.ifc_class.as_str()));
        text!(true, |r| Some(r.type_key.as_str()));
        text!(true, |r| Some(r.name.as_str()));
        text!(false, |r| r.storey.as_deref());
        text!(false, |r| r.space.as_deref());
        text!(true, |r| Some(r.discipline.as_str()));
        text!(false, |r| r.material.as_deref());
        text!(false, |r| r.phase.as_deref());
        text!(false, |r| r.space_usage.as_deref());
        text!(true, |r| Some(r.geometry_hash.as_str()));
        real!(false, |r| r.length_m);
        real!(false, |r| r.area_m2);
        real!(false, |r| r.area_gross_m2);
        real!(false, |r| r.volume_m3);
        real!(false, |r| r.volume_gross_m3);
        real!(false, |r| r.weight_kg);
        {
            let mut c = rg.next_column().map_err(err)?.ok_or("parquet: schema has fewer columns")?;
            let (mut v, mut d) = (vec![], vec![]);
            for r in chunk {
                match r.count {
                    Some(n) => {
                        v.push(n);
                        d.push(1i16);
                    }
                    None => d.push(0),
                }
            }
            c.typed::<Int64Type>().write_batch(&v, Some(&d), None).map_err(err)?;
            c.close().map_err(err)?;
            col += 1;
        }
        for i in 0..6 {
            real!(true, move |r| Some(r.bbox[i]));
        }
        text!(false, |r| r.props.as_deref());
        debug_assert_eq!(col, COLUMNS.len() + 1);
        rg.close().map_err(err)?;
    }
    w.close().map_err(err)?;
    Ok(notes)
}

/// Reads a file written by [`write_parquet`] (or any file with the same columns).
pub fn read_parquet(bytes: Vec<u8>) -> Result<Vec<ElementRow>, String> {
    let r = SerializedFileReader::new(bytes::Bytes::from(bytes)).map_err(err)?;
    let mut rows = vec![];
    for row in r.get_row_iter(None).map_err(err)? {
        let row = row.map_err(err)?;
        let mut o = ElementRow {
            key: String::new(),
            element_type: String::new(),
            ifc_class: String::new(),
            type_key: String::new(),
            name: String::new(),
            storey: None,
            space: None,
            discipline: String::new(),
            material: None,
            phase: None,
            space_usage: None,
            geometry_hash: String::new(),
            length_m: None,
            area_m2: None,
            area_gross_m2: None,
            volume_m3: None,
            volume_gross_m3: None,
            weight_kg: None,
            count: None,
            bbox: [0.0; 6],
            props: None,
        };
        let mut ids: [Option<String>; 3] = [None, None, None];
        for (name, f) in row.get_column_iter() {
            let s = || match f {
                Field::Str(s) => Ok(Some(s.clone())),
                Field::Null => Ok(None),
                other => Err(format!("parquet: column {name} holds {other:?}, expected text")),
            };
            let d = || match f {
                Field::Double(x) => Ok(Some(*x)),
                Field::Null => Ok(None),
                other => Err(format!("parquet: column {name} holds {other:?}, expected float64")),
            };
            match name.as_str() {
                "id" => ids[0] = s()?,
                "stable_id" => ids[1] = s()?,
                "element_key" => ids[2] = s()?,
                "element_type" => o.element_type = s()?.unwrap_or_default(),
                "ifc_class" => o.ifc_class = s()?.unwrap_or_default(),
                "type_key" => o.type_key = s()?.unwrap_or_default(),
                "name" => o.name = s()?.unwrap_or_default(),
                "storey" => o.storey = s()?,
                "space" => o.space = s()?,
                "discipline" => o.discipline = s()?.unwrap_or_default(),
                "material" => o.material = s()?,
                "phase" => o.phase = s()?,
                "space_usage" => o.space_usage = s()?,
                "geometry_hash" => o.geometry_hash = s()?.unwrap_or_default(),
                "length_m" => o.length_m = d()?,
                "area_m2" => o.area_m2 = d()?,
                "area_gross_m2" => o.area_gross_m2 = d()?,
                "volume_m3" => o.volume_m3 = d()?,
                "volume_gross_m3" => o.volume_gross_m3 = d()?,
                "weight_kg" => o.weight_kg = d()?,
                "count" => {
                    o.count = match f {
                        Field::Long(n) => Some(*n),
                        Field::Null => None,
                        other => return Err(format!("parquet: column count holds {other:?}, expected int64")),
                    }
                }
                "bbox_min_x" => o.bbox[0] = d()?.unwrap_or(0.0),
                "bbox_min_y" => o.bbox[1] = d()?.unwrap_or(0.0),
                "bbox_min_z" => o.bbox[2] = d()?.unwrap_or(0.0),
                "bbox_max_x" => o.bbox[3] = d()?.unwrap_or(0.0),
                "bbox_max_y" => o.bbox[4] = d()?.unwrap_or(0.0),
                "bbox_max_z" => o.bbox[5] = d()?.unwrap_or(0.0),
                "props" => o.props = s()?,
                other => return Err(format!("parquet: unexpected column '{other}'")),
            }
        }
        if ids[0] != ids[1] || ids[0] != ids[2] {
            return Err("parquet: id, stable_id and element_key differ".into());
        }
        o.key = ids[0].take().unwrap_or_default();
        rows.push(o);
    }
    Ok(rows)
}
