//! Placed drawings (underlays) kept in the project file. The app owns their live state and
//! writes a snapshot here before every save; the geometry is an opaque compressed record.
//! Outside the undo journal: underlay undo lives in the app session.

use rusqlite::{params, Connection, OptionalExtension};

use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS underlay(
    id INTEGER PRIMARY KEY,
    pos INTEGER NOT NULL,
    name TEXT NOT NULL,
    source TEXT,
    source_hash TEXT,
    level INTEGER NOT NULL,
    unit REAL NOT NULL,
    off_x REAL NOT NULL,
    off_y REAL NOT NULL,
    rot REAL NOT NULL,
    visible INTEGER NOT NULL,
    halftone INTEGER NOT NULL,
    layers TEXT NOT NULL,
    entities INTEGER NOT NULL,
    geom BLOB NOT NULL
);
";

/// Additive, by table existence; called from `schema::upgrade` on every open.
pub(crate) fn ensure(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// One placed drawing as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct UnderlayRow {
    pub name: String,
    pub source: Option<String>,
    pub source_hash: Option<String>,
    pub level: i64,
    /// Meters per file unit.
    pub unit: f64,
    pub offset: (f64, f64),
    pub rot: f64,
    pub visible: bool,
    pub halftone: bool,
    /// Layer states, JSON written and read by the app.
    pub layers: String,
    /// Primitive count of `geom`: checked after the write.
    pub entities: i64,
    pub geom: Vec<u8>,
}

fn has_table(db: &Connection) -> Result<bool, Error> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='underlay'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// (drawings, primitives) stored in the project file at `path`, read without a lock or upgrade:
/// a save checks with it that the file it wrote holds every drawing.
pub fn saved_underlays(path: &std::path::Path) -> Result<(i64, i64), Error> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if !has_table(&db)? {
        return Ok((0, 0));
    }
    Ok(db.query_row(
        "SELECT count(*), coalesce(sum(entities), 0) FROM underlay",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

impl Model {
    /// Total (drawings, primitives) held now.
    pub fn underlay_totals(&self) -> Result<(i64, i64), Error> {
        if !has_table(&self.db)? {
            return Ok((0, 0));
        }
        Ok(self.db.query_row(
            "SELECT count(*), coalesce(sum(entities), 0) FROM underlay",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Stored drawings in placement order; an older file without the table has none.
    pub fn underlays(&self) -> Result<Vec<UnderlayRow>, Error> {
        if !has_table(&self.db)? {
            return Ok(vec![]);
        }
        let mut st = self.db.prepare(
            "SELECT name, source, source_hash, level, unit, off_x, off_y, rot, visible, halftone, \
             layers, entities, geom FROM underlay ORDER BY pos, id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(UnderlayRow {
                name: r.get(0)?,
                source: r.get(1)?,
                source_hash: r.get(2)?,
                level: r.get(3)?,
                unit: r.get(4)?,
                offset: (r.get(5)?, r.get(6)?),
                rot: r.get(7)?,
                visible: r.get::<_, i64>(8)? != 0,
                halftone: r.get::<_, i64>(9)? != 0,
                layers: r.get(10)?,
                entities: r.get(11)?,
                geom: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replaces every stored drawing with `rows`, in one transaction, then reads back counts
    /// and sizes: any mismatch is an error and nothing is changed.
    pub fn set_underlays(&mut self, rows: &[UnderlayRow]) -> Result<(), Error> {
        for r in rows {
            let nums = [r.unit, r.offset.0, r.offset.1, r.rot];
            if nums.iter().any(|v| !v.is_finite()) || r.geom.is_empty() {
                return Err(Error::Invalid(format!("drawing '{}' cannot be stored", r.name)));
            }
        }
        // a savepoint inside `atomic`: a plain transaction would refuse to nest
        let tx = crate::ops::scope(&mut self.db)?;
        tx.execute_batch(DDL)?;
        tx.execute("DELETE FROM underlay", [])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO underlay(pos, name, source, source_hash, level, unit, off_x, off_y, \
                 rot, visible, halftone, layers, entities, geom) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for (k, r) in rows.iter().enumerate() {
                ins.execute(params![
                    k as i64,
                    r.name,
                    r.source,
                    r.source_hash,
                    r.level,
                    r.unit,
                    r.offset.0,
                    r.offset.1,
                    r.rot,
                    r.visible as i64,
                    r.halftone as i64,
                    r.layers,
                    r.entities,
                    r.geom,
                ])?;
            }
        }
        let (n, ents, bytes): (i64, i64, i64) = tx.query_row(
            "SELECT count(*), coalesce(sum(entities), 0), coalesce(sum(length(geom)), 0) \
             FROM underlay",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let want_ents: i64 = rows.iter().map(|r| r.entities).sum();
        let want_bytes: i64 = rows.iter().map(|r| r.geom.len() as i64).sum();
        if (n, ents, bytes) != (rows.len() as i64, want_ents, want_bytes) {
            return Err(Error::Invalid("drawings were not written completely".into()));
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> UnderlayRow {
        UnderlayRow {
            name: name.into(),
            source: Some("C:/x/plan.dxf".into()),
            source_hash: Some("00ff".into()),
            level: 1,
            unit: 0.001,
            offset: (1.5, -2.0),
            rot: 0.25,
            visible: true,
            halftone: true,
            layers: "[]".into(),
            entities: 3,
            geom: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn rows_round_trip_in_order_and_replace() {
        let mut m = Model::open(":memory:").unwrap();
        assert!(m.underlays().unwrap().is_empty());
        let rows = vec![row("b"), row("a")];
        m.set_underlays(&rows).unwrap();
        assert_eq!(m.underlays().unwrap(), rows);
        m.set_underlays(&rows[1..]).unwrap();
        assert_eq!(m.underlays().unwrap(), rows[1..].to_vec());
        m.set_underlays(&[]).unwrap();
        assert!(m.underlays().unwrap().is_empty());
    }

    #[test]
    fn a_file_without_the_table_reads_none_and_takes_rows() {
        let mut m = Model::open(":memory:").unwrap();
        m.db.execute_batch("DROP TABLE underlay").unwrap();
        assert!(m.underlays().unwrap().is_empty());
        m.set_underlays(&[row("x")]).unwrap();
        assert_eq!(m.underlays().unwrap().len(), 1);
        ensure(&m.db).unwrap();
        assert_eq!(m.underlays().unwrap().len(), 1, "ensure keeps rows");
    }

    #[test]
    fn bad_rows_change_nothing() {
        let mut m = Model::open(":memory:").unwrap();
        m.set_underlays(&[row("keep")]).unwrap();
        let mut bad = row("bad");
        bad.offset.0 = f64::INFINITY;
        assert!(m.set_underlays(&[row("n"), bad]).is_err());
        let mut empty = row("empty");
        empty.geom.clear();
        assert!(m.set_underlays(&[empty]).is_err());
        assert_eq!(m.underlays().unwrap()[0].name, "keep");
    }
}
