// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every element as one CSV row: `category,id,level,global_id`, then the row's own columns and
//! its properties, one column each. Geometry blobs are left out; empty cell = no value.

use std::collections::{BTreeSet, HashMap};

use ddc_model::{Model, COUNTED_CATEGORIES};
use rusqlite::types::ValueRef;

/// First four columns of every export; the rest follow from the model.
pub const FIXED: [&str; 4] = ["category", "id", "level", "global_id"];

fn text(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Null | ValueRef::Blob(_) => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
    }
}

fn table_exists(m: &Model, t: &str) -> bool {
    m.sql()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [t],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0
}

struct Row {
    cat: &'static str,
    table: &'static str,
    id: i64,
    level: String,
    cells: HashMap<String, String>,
}

/// `ext_ref`/`prop` rows name an element by its table or its category key.
fn kinds(r: &Row) -> Vec<(String, i64)> {
    let mut k = vec![(r.table.to_string(), r.id)];
    if r.cat != r.table {
        k.push((r.cat.to_string(), r.id));
    }
    k
}

/// The CSV text of `m`. Columns: [`FIXED`], then table columns in first-seen order (one that
/// repeats a fixed name gets its table as prefix), then property columns sorted (`key [unit]`;
/// a key that is also a column name gets `prop:`).
pub fn csv_full(m: &Model) -> Result<String, String> {
    let e = |x: rusqlite::Error| x.to_string();
    let mut levels: HashMap<i64, String> = HashMap::new();
    if table_exists(m, "level") {
        let mut st = m.sql().prepare("SELECT id, name FROM level").map_err(e)?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
        for r in rows {
            let (id, n) = r.map_err(e)?;
            levels.insert(id, n);
        }
    }
    let mut guids: HashMap<(String, i64), String> = HashMap::new();
    if table_exists(m, "ext_ref") {
        let mut st = m
            .sql()
            .prepare("SELECT kind, ref, guid FROM ext_ref ORDER BY source, guid")
            .map_err(e)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get(2)?)))
            .map_err(e)?;
        for r in rows {
            let (k, id, g) = r.map_err(e)?;
            guids.entry((k, id)).or_insert(g);
        }
    }
    let mut props: HashMap<(String, i64), Vec<(String, String)>> = HashMap::new();
    if table_exists(m, "prop") {
        let mut st = m
            .sql()
            .prepare("SELECT kind, ref, key, value, unit FROM prop ORDER BY kind, ref, key")
            .map_err(e)?;
        let mut q = st.query([]).map_err(e)?;
        while let Some(r) = q.next().map_err(e)? {
            let (kind, id): (String, i64) = (r.get(0).map_err(e)?, r.get(1).map_err(e)?);
            let (key, unit): (String, String) = (r.get(2).map_err(e)?, r.get(4).map_err(e)?);
            let Some(v) = text(r.get_ref(3).map_err(e)?) else {
                continue;
            };
            let head = if unit.is_empty() { key } else { format!("{key} [{unit}]") };
            props.entry((kind, id)).or_default().push((head, v));
        }
    }
    // solids carry their measured quantities: the stored form alone says nothing about size
    let mut qty: HashMap<i64, [f64; 3]> = HashMap::new();
    if table_exists(m, "solid_qty") {
        let mut st = m
            .sql()
            .prepare("SELECT solid, volume, net_volume, area FROM solid_qty WHERE error IS NULL")
            .map_err(e)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, [r.get(1)?, r.get(2)?, r.get(3)?])))
            .map_err(e)?;
        for r in rows {
            let (id, q) = r.map_err(e)?;
            qty.insert(id, q);
        }
    }
    let mut cols: Vec<String> = vec![];
    let mut rows: Vec<Row> = vec![];
    for (cat, table) in COUNTED_CATEGORIES {
        if !table_exists(m, table) {
            continue;
        }
        let mut st = m
            .sql()
            .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
            .map_err(e)?;
        let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
        let mut q = st.query([]).map_err(e)?;
        while let Some(r) = q.next().map_err(e)? {
            let mut row = Row {
                cat,
                table,
                id: 0,
                level: String::new(),
                cells: HashMap::new(),
            };
            for (i, n) in names.iter().enumerate() {
                let v = r.get_ref(i).map_err(e)?;
                match (n.as_str(), v) {
                    ("id", ValueRef::Integer(x)) => row.id = x,
                    ("level", ValueRef::Integer(x)) if *table != "level" => {
                        row.level = levels.get(&x).cloned().unwrap_or_default();
                    }
                    ("level", _) if *table != "level" => {}
                    _ => {
                        let Some(s) = text(v) else { continue };
                        let name = if FIXED.contains(&n.as_str()) {
                            format!("{table}.{n}")
                        } else {
                            n.clone()
                        };
                        if !cols.contains(&name) {
                            cols.push(name.clone());
                        }
                        row.cells.insert(name, s);
                    }
                }
            }
            if *table == "level" {
                row.level = levels.get(&row.id).cloned().unwrap_or_default();
            }
            if *table == "solid" {
                if let Some(q) = qty.get(&row.id) {
                    for (n, v) in [("volume_m3", q[0]), ("net_volume_m3", q[1]), ("area_m2", q[2])] {
                        if !cols.iter().any(|c| c == n) {
                            cols.push(n.to_string());
                        }
                        row.cells.insert(n.to_string(), format!("{v}"));
                    }
                }
            }
            rows.push(row);
        }
    }
    let mut heads = BTreeSet::new();
    for r in &rows {
        for k in kinds(r) {
            heads.extend(props.get(&k).into_iter().flatten().map(|(h, _)| h.clone()));
        }
    }
    let prop_name = |h: &str| {
        if cols.iter().any(|c| c == h) || FIXED.contains(&h) {
            format!("prop:{h}")
        } else {
            h.to_string()
        }
    };
    let prop_cols: Vec<(String, String)> = heads.into_iter().map(|h| (prop_name(&h), h)).collect();
    let f = |s: &str| ddc_geom::csv::field(s, ',').into_owned();
    let mut out = FIXED.join(",");
    for c in cols.iter().chain(prop_cols.iter().map(|(n, _)| n)) {
        out.push(',');
        out += &f(c);
    }
    out.push('\n');
    for r in &rows {
        let mut guid = "";
        let mut pv: HashMap<&str, &str> = HashMap::new();
        for k in kinds(r) {
            if guid.is_empty() {
                guid = guids.get(&k).map_or("", String::as_str);
            }
            for (h, v) in props.get(&k).into_iter().flatten() {
                pv.entry(h.as_str()).or_insert(v.as_str());
            }
        }
        out += &format!("{},{},{},{}", r.cat, r.id, f(&r.level), f(guid));
        for c in &cols {
            out.push(',');
            out += &r.cells.get(c).map(|v| f(v)).unwrap_or_default();
        }
        for (_, h) in &prop_cols {
            out.push(',');
            out += &pv.get(h.as_str()).map(|v| f(v)).unwrap_or_default();
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits one CSV line written by `field` (quotes double, separators inside quotes kept).
    fn cells(line: &str) -> Vec<String> {
        let (mut out, mut cur, mut quoted) = (vec![], String::new(), false);
        let mut it = line.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '"' if quoted && it.peek() == Some(&'"') => {
                    cur.push('"');
                    it.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => out.push(std::mem::take(&mut cur)),
                c => cur.push(c),
            }
        }
        out.push(cur);
        out
    }

    #[test]
    fn csv_has_columns_properties_guid_and_level() {
        let m = Model::open(":memory:").unwrap();
        let db = m.sql();
        db.execute("INSERT INTO level(id, name, elevation) VALUES (7, 'L1', 0.0)", [])
            .unwrap();
        db.execute(
            "INSERT INTO mesh(id, level, category, name, positions, indices) \
             VALUES (3, 7, 'Footing', 'F, 1', x'00', x'00')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO ext_ref VALUES ('ifc','0abcdefghijklmnopqrstu','mesh',3)", [])
            .unwrap();
        for (k, v, u) in [("Pset.Load", "12.5", "kN"), ("name", "dup", "")] {
            db.execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('mesh',3,?1,?2,?3)",
                [k, v, u],
            )
            .unwrap();
        }
        let csv = csv_full(&m).unwrap();
        let head = cells(csv.lines().next().unwrap());
        assert_eq!(&head[..4], &FIXED);
        let at = |n: &str| head.iter().position(|h| h == n).unwrap_or_else(|| panic!("{n}: {head:?}"));
        let row = cells(csv.lines().find(|l| l.starts_with("mesh,3,")).unwrap());
        assert_eq!(row.len(), head.len(), "{row:?}");
        assert_eq!(&row[..4], &["mesh", "3", "L1", "0abcdefghijklmnopqrstu"]);
        assert_eq!(row[at("mesh.category")], "Footing", "a column named like a fixed one");
        assert_eq!(row[at("name")], "F, 1");
        assert_eq!(row[at("Pset.Load [kN]")], "12.5");
        assert_eq!(row[at("prop:name")], "dup", "a property named like a column");
        assert!(!head.iter().any(|h| h == "positions"), "blobs stay out: {head:?}");
        let lvl = cells(csv.lines().find(|l| l.starts_with("level,7,")).unwrap());
        assert_eq!(&lvl[..3], &["level", "7", "L1"]);
        assert_eq!(lvl.len(), head.len());
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn solids_carry_volume_and_area() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        use ddc_geom::gsolid::{Form, Frame, Section};
        use ddc_geom::v2;
        let spec = ddc_model::SolidSpec {
            level: lv,
            name: "block".into(),
            void: false,
            material: "Concrete".into(),
            frame: Frame::default(),
            form: Form::Extrusion {
                profile: Section::polygon(vec![v2(0.0, 0.0), v2(2.0, 0.0), v2(2.0, 3.0), v2(0.0, 3.0)]),
                start: 0.0,
                end: 4.0,
            },
            ifc: None,
        };
        m.add_solid(&spec).unwrap();
        let csv = csv_full(&m).unwrap();
        let head = cells(csv.lines().next().unwrap());
        let at = |n: &str| head.iter().position(|h| h == n).unwrap_or_else(|| panic!("{n}: {head:?}"));
        let row = cells(csv.lines().find(|l| l.starts_with("solid,")).unwrap());
        let vol: f64 = row[at("volume_m3")].parse().unwrap();
        assert!((vol - 24.0).abs() < 1e-6, "{vol}");
        let area: f64 = row[at("area_m2")].parse().unwrap();
        assert!((area - 2.0 * (6.0 + 8.0 + 12.0)).abs() < 1e-6, "{area}");
    }
}
