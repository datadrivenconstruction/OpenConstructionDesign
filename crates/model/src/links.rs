//! Linked models (read-only): an .rvt/.ifc file, placement (offset, rotation), visibility,
//! load time. The link content is a separate model in the cache; the host stores only this row.
//! Adding, placement/visibility changes and removal are journal operations (undo); the load
//! stamp is cache state, not an edit, so it is not journaled. Link geometry lives in `geo`.

use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

mod geo;
#[cfg(test)]
mod tests;

pub use geo::{clash, section, Body, Clash};

pub(crate) const DDL: &str = "
-- \u{421}\u{432}\u{44f}\u{437}\u{430}\u{43d}\u{43d}\u{430}\u{44f} \u{43c}\u{43e}\u{434}\u{435}\u{43b}\u{44c}: \u{43f}\u{443}\u{442}\u{44c}, \u{444}\u{43e}\u{440}\u{43c}\u{430}\u{442}, \u{441}\u{434}\u{432}\u{438}\u{433} dx/dy/dz (\u{43c}), \u{43f}\u{43e}\u{432}\u{43e}\u{440}\u{43e}\u{442} rot (\u{440}\u{430}\u{434}, \u{432}\u{43e}\u{43a}\u{440}\u{443}\u{433} z), \u{432}\u{438}\u{434}\u{438}\u{43c}\u{43e}\u{441}\u{442}\u{44c},
-- \u{432}\u{440}\u{435}\u{43c}\u{44f} \u{43f}\u{43e}\u{441}\u{43b}\u{435}\u{434}\u{43d}\u{435}\u{439} \u{437}\u{430}\u{433}\u{440}\u{443}\u{437}\u{43a}\u{438} (unix, \u{441}), \u{43f}\u{443}\u{442}\u{44c} \u{43a}\u{44d}\u{448}\u{430} \u{437}\u{430}\u{433}\u{440}\u{443}\u{436}\u{435}\u{43d}\u{43d}\u{43e}\u{439} \u{43c}\u{43e}\u{434}\u{435}\u{43b}\u{438} ('' — \u{43d}\u{435} \u{437}\u{430}\u{433}\u{440}\u{443}\u{436}\u{435}\u{43d}\u{430}).
CREATE TABLE IF NOT EXISTS model_link(id INTEGER PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL,
    format TEXT NOT NULL, dx REAL NOT NULL DEFAULT 0, dy REAL NOT NULL DEFAULT 0,
    dz REAL NOT NULL DEFAULT 0, rot REAL NOT NULL DEFAULT 0, visible INTEGER NOT NULL DEFAULT 1,
    loaded_at INTEGER NOT NULL DEFAULT 0, cache TEXT NOT NULL DEFAULT '');
";

/// Schema 28: the links table (additive, idempotent).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// Link placement in host coordinates: rotation `rot` (rad) about the z axis through the link
/// origin, then offset (dx, dy, dz).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transform {
    pub dx: f64,
    pub dy: f64,
    pub dz: f64,
    pub rot: f64,
}

impl Transform {
    pub fn is_finite(&self) -> bool {
        [self.dx, self.dy, self.dz, self.rot]
            .iter()
            .all(|v| v.is_finite())
    }

    /// Link point -> host point.
    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        let (x, y) = self.apply2(p[0], p[1]);
        [x, y, p[2] + self.dz]
    }

    pub fn apply2(&self, x: f64, y: f64) -> (f64, f64) {
        let (s, c) = self.rot.sin_cos();
        (c * x - s * y + self.dx, s * x + c * y + self.dy)
    }

    /// Host point -> link point.
    pub fn inverse(&self, q: [f64; 3]) -> [f64; 3] {
        let (s, c) = self.rot.sin_cos();
        let (x, y) = (q[0] - self.dx, q[1] - self.dy);
        [c * x + s * y, -s * x + c * y, q[2] - self.dz]
    }
}

/// A `model_link` row.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub id: i64,
    pub path: String,
    pub name: String,
    /// "rvt" | "ifc".
    pub format: String,
    pub xf: Transform,
    pub visible: bool,
    /// Unix time of the last load, seconds; 0 = never loaded.
    pub loaded_at: i64,
    pub cache: String,
}

/// Link format by file extension; None = unsupported.
pub fn link_format(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path.trim())
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        "rvt" => Some("rvt"),
        "ifc" => Some("ifc"),
        _ => None,
    }
}

fn stem(path: &str) -> String {
    std::path::Path::new(path.trim())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.trim().to_string())
}

const COLS: &str = "id, path, name, format, dx, dy, dz, rot, visible, loaded_at, cache";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Link> {
    Ok(Link {
        id: r.get(0)?,
        path: r.get(1)?,
        name: r.get(2)?,
        format: r.get(3)?,
        xf: Transform {
            dx: r.get(4)?,
            dy: r.get(5)?,
            dz: r.get(6)?,
            rot: r.get(7)?,
        },
        visible: r.get::<_, i64>(8)? != 0,
        loaded_at: r.get(9)?,
        cache: r.get(10)?,
    })
}

impl Model {
    /// Adds a link (path to .rvt/.ifc; the name defaults to the file name). One undo operation.
    pub fn add_link(&mut self, path: &str, name: &str, xf: Transform) -> Result<i64, Error> {
        let Some(format) = link_format(path) else {
            return Err(Error::Invalid(format!("link: unsupported file '{path}'")));
        };
        if !xf.is_finite() {
            return Err(Error::Invalid("link: bad transform".into()));
        }
        let name = match name.trim() {
            "" => stem(path),
            n => n.to_string(),
        };
        let path = path.trim().to_string();
        self.op("link_add", |db| {
            db.execute(
                "INSERT INTO model_link(path, name, format, dx, dy, dz, rot) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![path, name, format, xf.dx, xf.dy, xf.dz, xf.rot],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("model_link", "id", id)))
        })
    }

    /// Link placement and visibility. One undo operation.
    pub fn update_link(&mut self, id: i64, xf: Transform, visible: bool) -> Result<(), Error> {
        if !xf.is_finite() {
            return Err(Error::Invalid("link: bad transform".into()));
        }
        self.op("link_edit", |db| {
            // only the edited fields: undo must not bring back a stale reload (loaded_at/cache)
            let old: Option<(f64, f64, f64, f64, i64)> = db
                .query_row(
                    "SELECT dx, dy, dz, rot, visible FROM model_link WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((dx, dy, dz, rot, vis)) = old else {
                return Err(Error::Invalid("no such link".into()));
            };
            let inv = Inv::default().sql(
                "UPDATE model_link SET dx=?2, dy=?3, dz=?4, rot=?5, visible=?6 WHERE id=?1",
                vec![
                    id.into(),
                    dx.into(),
                    dy.into(),
                    dz.into(),
                    rot.into(),
                    vis.into(),
                ],
            );
            let n = db.execute(
                "UPDATE model_link SET dx=?2, dy=?3, dz=?4, rot=?5, visible=?6 WHERE id=?1",
                params![id, xf.dx, xf.dy, xf.dz, xf.rot, visible as i64],
            )?;
            if n == 0 {
                return Err(Error::Invalid("no such link".into()));
            }
            Ok(((), id, inv))
        })
    }

    /// Removes a link (the linked file is left untouched). One undo operation.
    pub fn remove_link(&mut self, id: i64) -> Result<(), Error> {
        self.op("link_delete", |db| {
            let inv = Inv::default().restore(db, "model_link", "id", id)?;
            if db.execute("DELETE FROM model_link WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid("no such link".into()));
            }
            Ok(((), id, inv))
        })
    }

    pub fn links(&self) -> Result<Vec<Link>, Error> {
        let mut st = self
            .db
            .prepare(&format!("SELECT {COLS} FROM model_link ORDER BY id"))?;
        let rows = st.query_map([], row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn link(&self, id: i64) -> Result<Option<Link>, Error> {
        Ok(self
            .db
            .query_row(
                &format!("SELECT {COLS} FROM model_link WHERE id=?1"),
                [id],
                row,
            )
            .optional()?)
    }

    /// Load stamp (time, cache path): state, not an edit, so it is not journaled;
    /// otherwise Ctrl+Z would undo the load instead of the user's last edit.
    pub fn mark_link_loaded(&mut self, id: i64, at: i64, cache: &str) -> Result<(), Error> {
        let n = self.db.execute(
            "UPDATE model_link SET loaded_at=?2, cache=?3 WHERE id=?1",
            params![id, at, cache],
        )?;
        if n == 0 {
            return Err(Error::Invalid("no such link".into()));
        }
        Ok(())
    }
}
