//! Guard on load: a stored number that is not finite (infinity; NaN is stored as NULL by the
//! database) refuses the whole file and names the table and row.

use rusqlite::Connection;

use crate::Error;

/// First `(table, rowid)` that holds an infinite value in a REAL column; `None` when clean.
/// Tables that cannot be scanned (views, no rowid) are skipped.
pub(crate) fn first_non_finite(db: &Connection) -> Result<Option<(String, i64)>, Error> {
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    for t in tables {
        let cols: Vec<String> = db
            .prepare(&format!(
                "SELECT name FROM pragma_table_info('{}') WHERE upper(type) IN ('REAL','DOUBLE','FLOAT')",
                t.replace('\'', "''")
            ))?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        if cols.is_empty() {
            continue;
        }
        let cond: Vec<String> = cols
            .iter()
            .map(|c| {
                let c = c.replace('"', "\"\"");
                format!("\"{c}\" = 9e999 OR \"{c}\" = -9e999")
            })
            .collect();
        let sql = format!(
            "SELECT rowid FROM \"{}\" WHERE {} LIMIT 1",
            t.replace('"', "\"\""),
            cond.join(" OR ")
        );
        match db.query_row(&sql, [], |r| r.get::<_, i64>(0)) {
            Ok(row) => return Ok(Some((t, row))),
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(_) => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use crate::{Error, LayerSpec, Model, WallSpec};

    #[test]
    fn open_rejects_non_finite_coordinate() {
        let p = std::env::temp_dir().join(format!("ddc-ri2-fin-{}.ddc", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let s = p.to_str().unwrap();
        {
            let mut m = Model::open(s).unwrap();
            let lv = m.add_level("L1", 0.0).unwrap();
            let t = m
                .add_wall_type(
                    "w",
                    &[LayerSpec {
                        material: "brick".into(),
                        thickness: 0.2,
                    }],
                )
                .unwrap();
            m.add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
            m.sql().execute("UPDATE wall SET bx = 9e999", []).unwrap();
        }
        let e = Model::open(s).err().expect("an infinite coordinate must refuse the file");
        match e {
            Error::Corrupt(t) => assert!(t.contains("wall") && t.contains("row"), "{t}"),
            e => panic!("wrong error: {e}"),
        }
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{s}{ext}"));
        }
    }

    #[test]
    fn a_clean_file_opens() {
        let m = Model::open(":memory:").unwrap();
        assert!(super::first_non_finite(m.sql()).unwrap().is_none());
    }
}
