//! Saved 3D views with no new schema step: a `view_def` row with kind = 'view3d'
//! (ax, ay, bx, by, depth are the plan extents of the clipping box and its height; far_clip means the box
//! is on) and the view description as JSON in `prop` (kind = 'view3d', ref = id, key = 'spec'):
//! camera, box, section fill, style, hidden elements, category rules.
//! Sections and elevations (`view_defs`) do not see such rows. One edit is one journal operation.

use rusqlite::{params, types::Value, Connection, OptionalExtension};
use serde_json::{json, Value as J};

use crate::ops::Inv;
use crate::{Error, Model};

pub const VIEW3D_KIND: &str = "view3d";
const WORK_STYLE: &str = "view3d.style";

#[derive(Clone, Debug, PartialEq)]
pub struct View3dCamera {
    pub target: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub fov_y: f64,
    pub ortho: bool,
}

impl Default for View3dCamera {
    fn default() -> Self {
        Self {
            target: [0.0; 3],
            yaw: -2.2,
            pitch: 0.6,
            distance: 10.0,
            fov_y: 45f64.to_radians(),
            ortho: false,
        }
    }
}

/// A category rule in 3D: visibility and transparency, %.
#[derive(Clone, Debug, PartialEq)]
pub struct CategoryRule {
    pub category: String,
    pub visible: bool,
    pub transparency: u8,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct View3dSpec {
    pub name: String,
    pub camera: View3dCamera,
    /// Clipping box (min, max), m, in model coordinates.
    pub section_box: Option<([f64; 3], [f64; 3])>,
    /// Section fill by the box.
    pub caps: bool,
    /// Visual style key (`shaded`, `consistent`, `hidden_line`, `wireframe`, `xray`).
    pub style: String,
    /// Elements hidden in the view: (table, id).
    pub hidden: Vec<(String, i64)>,
    pub categories: Vec<CategoryRule>,
    /// A per-level view: the level and the section height above it.
    pub level: Option<(i64, f64)>,
}

impl View3dSpec {
    pub fn to_json(&self) -> String {
        let c = &self.camera;
        let mut o = json!({
            "camera": {
                "target": c.target,
                "yaw": c.yaw,
                "pitch": c.pitch,
                "distance": c.distance,
                "fov_y": c.fov_y,
                "ortho": c.ortho,
            },
            "caps": self.caps,
            "style": self.style,
            "hidden": self.hidden.iter().map(|(k, id)| json!([k, id])).collect::<Vec<_>>(),
            "categories": self.categories.iter().map(|r| json!({
                "category": r.category,
                "visible": r.visible,
                "transparency": r.transparency,
            })).collect::<Vec<_>>(),
        });
        if let Some((a, b)) = self.section_box {
            o["box"] = json!({ "min": a, "max": b });
        }
        if let Some((l, off)) = self.level {
            o["level"] = json!({ "id": l, "offset": off });
        }
        o.to_string()
    }

    /// Parsing with defaults: unknown fields are skipped, missing ones take defaults.
    pub fn from_json(name: &str, s: &str) -> Result<Self, Error> {
        let j: J = serde_json::from_str(s).map_err(|e| Error::Invalid(format!("view3d: {e}")))?;
        let f = |v: &J, d: f64| v.as_f64().unwrap_or(d);
        let v3 = |v: &J| -> Option<[f64; 3]> {
            let a = v.as_array()?;
            (a.len() == 3).then(|| [f(&a[0], 0.0), f(&a[1], 0.0), f(&a[2], 0.0)])
        };
        let d = View3dCamera::default();
        let c = &j["camera"];
        let camera = View3dCamera {
            target: v3(&c["target"]).unwrap_or(d.target),
            yaw: f(&c["yaw"], d.yaw),
            pitch: f(&c["pitch"], d.pitch),
            distance: f(&c["distance"], d.distance),
            fov_y: f(&c["fov_y"], d.fov_y),
            ortho: c["ortho"].as_bool().unwrap_or(false),
        };
        let section_box = match (v3(&j["box"]["min"]), v3(&j["box"]["max"])) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        };
        let hidden = j["hidden"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_i64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let categories = j["categories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(CategoryRule {
                            category: r["category"].as_str()?.to_string(),
                            visible: r["visible"].as_bool().unwrap_or(true),
                            transparency: r["transparency"].as_u64().unwrap_or(0).min(100) as u8,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let level = j["level"]["id"]
            .as_i64()
            .map(|l| (l, f(&j["level"]["offset"], 1.2)));
        Ok(Self {
            name: name.to_string(),
            camera,
            section_box,
            caps: j["caps"].as_bool().unwrap_or(true),
            style: j["style"].as_str().unwrap_or("shaded").to_string(),
            hidden,
            categories,
            level,
        })
    }
}

fn check(s: &View3dSpec) -> Result<(), Error> {
    let c = &s.camera;
    let finite = c
        .target
        .iter()
        .chain([&c.yaw, &c.pitch, &c.distance, &c.fov_y])
        .all(|v| v.is_finite());
    if s.name.trim().is_empty() || !finite || c.distance <= 0.0 || !(0.01..3.1).contains(&c.fov_y) {
        return Err(Error::Invalid(
            "3D view: name required, camera finite, distance > 0, fov in (0, π)".into(),
        ));
    }
    if let Some((a, b)) = s.section_box {
        if !(0..3).all(|k| a[k].is_finite() && b[k].is_finite() && a[k] < b[k]) {
            return Err(Error::Invalid("3D view: section box min < max".into()));
        }
    }
    Ok(())
}

/// Box plan extents and height for the `view_def` columns.
fn row_geom(s: &View3dSpec) -> ([f64; 4], f64) {
    match s.section_box {
        Some((a, b)) => ([a[0], a[1], b[0], b[1]], b[2] - a[2]),
        None => {
            let t = s.camera.target;
            ([t[0], t[1], t[0], t[1]], 0.0)
        }
    }
}

fn write_spec(db: &Connection, id: i64, s: &View3dSpec) -> Result<(), Error> {
    db.execute(
        "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,'spec',?3,'')",
        params![VIEW3D_KIND, id, s.to_json()],
    )?;
    Ok(())
}

fn read_spec(db: &Connection, id: i64) -> Result<Option<View3dSpec>, Error> {
    let name: Option<String> = db
        .query_row(
            "SELECT name FROM view_def WHERE id=?1 AND kind=?2",
            params![id, VIEW3D_KIND],
            |r| r.get(0),
        )
        .optional()?;
    let Some(name) = name else {
        return Ok(None);
    };
    let body: Option<String> = db
        .query_row(
            "SELECT value FROM prop WHERE kind=?1 AND ref=?2 AND key='spec'",
            params![VIEW3D_KIND, id],
            |r| r.get(0),
        )
        .optional()?;
    View3dSpec::from_json(&name, body.as_deref().unwrap_or("{}")).map(Some)
}

/// Inverse command: restore the view description (or delete it if there was none).
fn spec_inv(db: &Connection, id: i64) -> Result<Inv, Error> {
    let old: Option<String> = db
        .query_row(
            "SELECT value FROM prop WHERE kind=?1 AND ref=?2 AND key='spec'",
            params![VIEW3D_KIND, id],
            |r| r.get(0),
        )
        .optional()?;
    let k = Value::Text(VIEW3D_KIND.into());
    Ok(match old {
        Some(v) => Inv::default().sql(
            "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,'spec',?3,'')",
            vec![k, Value::Integer(id), Value::Text(v)],
        ),
        None => Inv::default().sql(
            "DELETE FROM prop WHERE kind=?1 AND ref=?2",
            vec![k, Value::Integer(id)],
        ),
    })
}

impl Model {
    pub fn add_view3d(&mut self, s: &View3dSpec) -> Result<i64, Error> {
        check(s)?;
        self.op("view3d_add", |db| {
            let (g, h) = row_geom(s);
            db.execute(
                "INSERT INTO view_def(kind,name,ax,ay,bx,by,depth,far_clip) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![VIEW3D_KIND, s.name, g[0], g[1], g[2], g[3], h, i64::from(s.section_box.is_some())],
            )?;
            let id = db.last_insert_rowid();
            write_spec(db, id, s)?;
            let inv = Inv::default().delete("view_def", "id", id).sql(
                "DELETE FROM prop WHERE kind=?1 AND ref=?2",
                vec![Value::Text(VIEW3D_KIND.into()), Value::Integer(id)],
            );
            Ok((id, id, inv))
        })
    }

    pub fn update_view3d(&mut self, id: i64, s: &View3dSpec) -> Result<(), Error> {
        check(s)?;
        self.op("view3d_update", |db| {
            if read_spec(db, id)?.is_none() {
                return Err(Error::Invalid(format!("no such 3D view {id}")));
            }
            let inv = Inv::default()
                .restore(db, "view_def", "id", id)?
                .then(spec_inv(db, id)?);
            let (g, h) = row_geom(s);
            db.execute(
                "UPDATE view_def SET name=?2,ax=?3,ay=?4,bx=?5,by=?6,depth=?7,far_clip=?8 WHERE id=?1",
                params![id, s.name, g[0], g[1], g[2], g[3], h, i64::from(s.section_box.is_some())],
            )?;
            write_spec(db, id, s)?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_view3d(&mut self, id: i64) -> Result<(), Error> {
        self.op("view3d_delete", |db| {
            if read_spec(db, id)?.is_none() {
                return Err(Error::Invalid(format!("no such 3D view {id}")));
            }
            let inv = Inv::default()
                .restore(db, "view_def", "id", id)?
                .then(spec_inv(db, id)?);
            db.execute("DELETE FROM view_def WHERE id=?1", [id])?;
            db.execute(
                "DELETE FROM prop WHERE kind=?1 AND ref=?2",
                params![VIEW3D_KIND, id],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Style of the working 3D view; `None` in projects that never saved one. Display state:
    /// stored in `meta`, outside undo, with no schema step.
    pub fn view3d_style_code(&self) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .prepare_cached("SELECT value FROM meta WHERE key=?1")?
            .query_row([WORK_STYLE], |r| r.get(0))
            .optional()?)
    }

    pub fn set_view3d_style_code(&self, code: i64) -> Result<(), Error> {
        self.db.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![WORK_STYLE, code],
        )?;
        Ok(())
    }

    pub fn view3d(&self, id: i64) -> Result<View3dSpec, Error> {
        read_spec(&self.db, id)?.ok_or_else(|| Error::Invalid(format!("no such 3D view {id}")))
    }

    /// All saved 3D views by ascending id.
    pub fn view3ds(&self) -> Result<Vec<(i64, View3dSpec)>, Error> {
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM view_def WHERE kind=?1 ORDER BY id")?;
            let v = st
                .query_map([VIEW3D_KIND], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        ids.into_iter()
            .map(|id| Ok((id, self.view3d(id)?)))
            .collect()
    }

    /// Wall type layer materials (in layer order), the section fills in 3D.
    pub fn wall_type_materials(&self, wtype: i64) -> Result<Vec<String>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT material FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")?;
        let v = st
            .query_map([wtype], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Slab layer materials (by slab type).
    pub fn slab_materials(&self, slab: i64) -> Result<Vec<String>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT l.material FROM slab s JOIN slab_type_layer l ON l.stype = s.stype \
             WHERE s.id=?1 ORDER BY l.pos",
        )?;
        let v = st
            .query_map([slab], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SectionSpec, ViewKind};

    fn spec() -> View3dSpec {
        View3dSpec {
            name: "3D - facade".into(),
            camera: View3dCamera {
                target: [1.25, -3.5, 0.1 + 0.2],
                yaw: -1.0471975511965976,
                pitch: 0.31,
                distance: 42.0,
                fov_y: 0.8,
                ortho: true,
            },
            section_box: Some(([-1.0, -2.0, -0.3], [10.0, 8.5, 1.2])),
            caps: true,
            style: "hidden_line".into(),
            hidden: vec![("wall".into(), 7), ("slab".into(), 3)],
            categories: vec![
                CategoryRule {
                    category: "floor".into(),
                    visible: false,
                    transparency: 0,
                },
                CategoryRule {
                    category: "wall".into(),
                    visible: true,
                    transparency: 60,
                },
            ],
            level: Some((2, 1.2)),
        }
    }

    #[test]
    fn json_roundtrip_exact() {
        let s = spec();
        assert_eq!(View3dSpec::from_json(&s.name, &s.to_json()).unwrap(), s);
        let mut t = spec();
        t.section_box = None;
        t.level = None;
        t.hidden.clear();
        assert_eq!(View3dSpec::from_json(&t.name, &t.to_json()).unwrap(), t);
        // an empty description gives defaults
        let d = View3dSpec::from_json("x", "{}").unwrap();
        assert_eq!(d.camera, View3dCamera::default());
        assert!(d.caps && d.style == "shaded" && d.section_box.is_none());
        assert!(View3dSpec::from_json("x", "not json").is_err());
    }

    #[test]
    fn view3d_persist_update_delete_undo() {
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let sec = m
            .add_view_def(&SectionSpec {
                kind: ViewKind::Section,
                name: "1-1".into(),
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                depth: 3.0,
                far_clip: false,
            })
            .unwrap();
        let id = m.add_view3d(&spec()).unwrap();
        assert_eq!(m.view3d(id).unwrap(), spec());
        // sections do not see 3D views, and vice versa
        let defs = m.view_defs().unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].0, sec);
        assert_eq!(m.view3ds().unwrap().len(), 1);
        assert!(m.view3d(sec).is_err());
        // the sections API neither reads, changes nor deletes a 3D view row
        assert!(m.view_def(id).is_err());
        let s1 = m.view_def(sec).unwrap();
        assert!(m.update_view_def(id, &s1).is_err());
        assert!(m.delete_view_def(id).is_err());
        assert_eq!(m.view3d(id).unwrap(), spec());

        let mut s2 = spec();
        s2.name = "3D - level 2".into();
        s2.camera.ortho = false;
        s2.section_box = None;
        m.update_view3d(id, &s2).unwrap();
        assert_eq!(m.view3d(id).unwrap(), s2);
        m.undo().unwrap();
        assert_eq!(m.view3d(id).unwrap(), spec());

        m.delete_view3d(id).unwrap();
        assert!(m.view3d(id).is_err());
        assert!(m.view3ds().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.view3d(id).unwrap(), spec());

        m.undo().unwrap(); // addition
        assert!(m.view3d(id).is_err());
        assert_eq!(m.view_defs().unwrap().len(), 1);

        let mut bad = spec();
        bad.section_box = Some(([0.0; 3], [0.0, 1.0, 1.0]));
        assert!(m.add_view3d(&bad).is_err());
        bad = spec();
        bad.name = " ".into();
        assert!(m.add_view3d(&bad).is_err());
        assert!(m.update_view3d(sec, &spec()).is_err());
        assert!(m.delete_view3d(9999).is_err());
    }
}

#[cfg(test)]
mod work_style_tests {
    use crate::Model;

    #[test]
    fn working_style_is_absent_until_saved_and_round_trips() {
        let m = Model::open(":memory:").unwrap();
        assert_eq!(m.view3d_style_code().unwrap(), None);
        m.set_view3d_style_code(4).unwrap();
        m.set_view3d_style_code(3).unwrap();
        assert_eq!(m.view3d_style_code().unwrap(), Some(3));
    }
}
