// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Surface values of a material row that override its library class: roughness, metallic,
//! opacity and refraction index. Columns of `material`, added in place (see `material_texture`),
//! so renames, undo snapshots and old files need no schema step. `None` keeps the class value.

use rusqlite::{params, Connection, OptionalExtension};

use crate::materials::{insert_spec_ignore, row_id};
use crate::ops::Inv;
use crate::{material_defaults, Error, Model};

/// Refraction index limits.
pub const IOR_MIN: f64 = 1.0;
pub const IOR_MAX: f64 = 3.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MaterialSurface {
    /// 0 mirror smooth, 1 fully rough.
    pub roughness: Option<f64>,
    /// 0 dielectric, 1 bare metal.
    pub metallic: Option<f64>,
    /// 1 opaque, 0 clear.
    pub opacity: Option<f64>,
    pub ior: Option<f64>,
}

impl MaterialSurface {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn check(&self) -> Result<(), Error> {
        let unit = |v: Option<f64>, what: &str| match v {
            Some(x) if !(x.is_finite() && (0.0..=1.0).contains(&x)) => {
                Err(Error::Invalid(format!("{what} must be 0..1")))
            }
            _ => Ok(()),
        };
        unit(self.roughness, "roughness")?;
        unit(self.metallic, "metallic")?;
        unit(self.opacity, "opacity")?;
        match self.ior {
            Some(x) if !(x.is_finite() && (IOR_MIN..=IOR_MAX).contains(&x)) => {
                Err(Error::Invalid("refraction index must be 1..3".into()))
            }
            _ => Ok(()),
        }
    }
}

const COLUMNS: [&str; 4] = ["surf_rough", "surf_metal", "surf_opacity", "surf_ior"];

/// Additive and idempotent; runs on every open from `materials::migrate` (DDL only).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    for col in COLUMNS {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('material') WHERE name=?1",
            [col],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE material ADD COLUMN {col} REAL"))?;
        }
    }
    Ok(())
}

impl Model {
    /// Surface values of a material; all `None` for a text without a row.
    pub fn material_surface(&self, name: &str) -> Result<MaterialSurface, Error> {
        let row = self
            .db
            .query_row(
                "SELECT surf_rough,surf_metal,surf_opacity,surf_ior FROM material WHERE name=?1",
                [name],
                |r| {
                    let f = |i| r.get::<_, Option<f64>>(i).map(|v| v.filter(|x| x.is_finite()));
                    Ok(MaterialSurface { roughness: f(0)?, metallic: f(1)?, opacity: f(2)?, ior: f(3)? })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    /// Sets the surface values of a material (`None` fields go back to the class value); the row
    /// is made when the material so far exists only as text. One undo step.
    pub fn set_material_surface(&mut self, name: &str, s: &MaterialSurface) -> Result<(), Error> {
        s.check()?;
        let (name, s) = (name.to_string(), *s);
        self.op("material_surface", move |db| {
            let (id, inv) = match row_id(db, &name)? {
                Some(id) => (id, Inv::default().restore(db, "material", "id", id)?),
                None => {
                    insert_spec_ignore(db, &material_defaults(&name))?;
                    let id = db.last_insert_rowid();
                    (id, Inv::default().delete("material", "id", id))
                }
            };
            db.execute(
                "UPDATE material SET surf_rough=?2,surf_metal=?3,surf_opacity=?4,surf_ior=?5 WHERE id=?1",
                params![id, s.roughness, s.metallic, s.opacity, s.ior],
            )?;
            Ok(((), id, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_undo_and_look_override() {
        let mut m = Model::open(":memory:").unwrap();
        let base = m.material_look("steel S355").unwrap().unwrap();
        let s = MaterialSurface { roughness: Some(0.9), metallic: Some(0.0), opacity: Some(0.5), ior: Some(1.33) };
        m.set_material_surface("steel S355", &s).unwrap();
        assert_eq!(m.material_surface("steel S355").unwrap(), s);
        let l = m.material_look("steel S355").unwrap().unwrap();
        assert!((l.roughness - 0.9).abs() < 1e-6 && l.metallic == 0.0, "{l:?} vs {base:?}");
        assert_eq!((l.opacity, l.ior), (Some(0.5), Some(1.33)));
        m.undo().unwrap();
        assert!(m.material_surface("steel S355").unwrap().is_default());
        assert_eq!(m.material_look("steel S355").unwrap().unwrap(), base);
    }

    #[test]
    fn out_of_range_values_are_refused() {
        let mut m = Model::open(":memory:").unwrap();
        for s in [
            MaterialSurface { roughness: Some(1.5), ..Default::default() },
            MaterialSurface { metallic: Some(-0.1), ..Default::default() },
            MaterialSurface { opacity: Some(f64::NAN), ..Default::default() },
            MaterialSurface { ior: Some(0.5), ..Default::default() },
        ] {
            assert!(m.set_material_surface("Zz", &s).is_err(), "{s:?}");
        }
        assert!(m.material_surface("Zz").unwrap().is_default());
    }
}
