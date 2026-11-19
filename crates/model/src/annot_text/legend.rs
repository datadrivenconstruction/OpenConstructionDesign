//! Legends: a view without model geometry. Rows are a sample (line style, fill, detail component, wall
//! type) and a caption; any annotations can also be placed on the same view. The view goes on a sheet as a table.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::{Anno, AnnoShape, AnnoView, RegionFill, Styles, TextNote};
use crate::ops::Inv;
use crate::views::{PlanView, ViewPrim};
use crate::{Error, Model};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LegendSwatch {
    None,
    Line(i64),
    Region(RegionFill),
    Detail(i64),
    WallType(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LegendRow {
    pub swatch: LegendSwatch,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LegendDef {
    pub id: i64,
    pub name: String,
    pub scale: f64,
}

/// A legend on a sheet: `pos` is the top-left corner, sheet mm.
#[derive(Clone, Debug, PartialEq)]
pub struct LegendPlace {
    pub id: i64,
    pub sheet: i64,
    pub legend: i64,
    pub pos: (f64, f64),
}

const ROW_MM: f64 = 8.0;
const SWATCH_MM: f64 = 20.0;
const LABEL_X_MM: f64 = 24.0;
const TITLE_H_MM: f64 = 3.5;
const LABEL_H_MM: f64 = 2.5;

fn swatch_json(s: LegendSwatch) -> Value {
    match s {
        LegendSwatch::None => json!({"k": "none"}),
        LegendSwatch::Line(i) => json!({"k": "line", "id": i}),
        LegendSwatch::Region(f) => json!({"k": "region", "fill": super::fill_json(f)}),
        LegendSwatch::Detail(i) => json!({"k": "detail", "id": i}),
        LegendSwatch::WallType(i) => json!({"k": "wall", "id": i}),
    }
}

fn json_swatch(v: &Value) -> Option<LegendSwatch> {
    Some(match v["k"].as_str()? {
        "none" => LegendSwatch::None,
        "line" => LegendSwatch::Line(v["id"].as_i64()?),
        "region" => LegendSwatch::Region(super::json_fill(&v["fill"])?),
        "detail" => LegendSwatch::Detail(v["id"].as_i64()?),
        "wall" => LegendSwatch::WallType(v["id"].as_i64()?),
        _ => return None,
    })
}

fn rows_in(db: &Connection, id: i64) -> Result<Vec<LegendRow>, Error> {
    let rows: Vec<(String, String)> = {
        let mut st = db.prepare_cached(
            "SELECT swatch,label FROM anno_legend_row WHERE legend=?1 ORDER BY idx",
        )?;
        let v = st
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    rows.into_iter()
        .map(|(s, label)| -> Result<LegendRow, Error> {
            let v: Value = serde_json::from_str(&s)?;
            let swatch = json_swatch(&v)
                .ok_or_else(|| Error::Invalid(format!("bad legend swatch in legend {id}")))?;
            Ok(LegendRow { swatch, label })
        })
        .collect()
}

fn legend_of(db: &Connection, id: i64) -> Result<LegendDef, Error> {
    db.query_row(
        "SELECT name, scale FROM anno_legend WHERE id=?1",
        [id],
        |r| {
            Ok(LegendDef {
                id,
                name: r.get(0)?,
                scale: r.get(1)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such legend {id}")))
}

fn wall_thickness(db: &Connection, wtype: i64) -> Result<f64, Error> {
    Ok(db.query_row(
        "SELECT COALESCE(SUM(thickness),0) FROM wall_type_layer WHERE wtype=?1",
        [wtype],
        |r| r.get(0),
    )?)
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

impl Model {
    pub fn add_legend(&mut self, name: &str, scale: f64) -> Result<i64, Error> {
        if name.trim().is_empty() || !(scale.is_finite() && scale > 0.0) {
            return Err(Error::Invalid("legend needs a name and a scale".into()));
        }
        self.op("anno_legend_add", |db| {
            db.execute(
                "INSERT INTO anno_legend(name,scale) VALUES (?1,?2)",
                params![name, scale],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno_legend", "id", id)))
        })
    }

    pub fn update_legend(&mut self, id: i64, name: &str, scale: f64) -> Result<(), Error> {
        if name.trim().is_empty() || !(scale.is_finite() && scale > 0.0) {
            return Err(Error::Invalid("legend needs a name and a scale".into()));
        }
        self.op("anno_legend_edit", |db| {
            let inv = Inv::default().restore(db, "anno_legend", "id", id)?;
            let n = db.execute(
                "UPDATE anno_legend SET name=?2, scale=?3 WHERE id=?1",
                params![id, name, scale],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such legend {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// A legend together with its rows, sheet placements and own annotations is one undo step.
    pub fn delete_legend(&mut self, id: i64) -> Result<(), Error> {
        self.op("anno_legend_delete", |db| {
            legend_of(db, id)?;
            let inv = Inv::default()
                .restore(db, "anno_legend", "id", id)?
                .restore(db, "anno_legend_row", "legend", id)?
                .restore(db, "anno_legend_place", "legend", id)?;
            let inv = super::drop_view(db, inv, AnnoView::Legend(id))?;
            db.execute("DELETE FROM anno_legend_row WHERE legend=?1", [id])?;
            db.execute("DELETE FROM anno_legend_place WHERE legend=?1", [id])?;
            db.execute("DELETE FROM anno_legend WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn legends(&self) -> Result<Vec<LegendDef>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT id,name,scale FROM anno_legend ORDER BY id")?;
        let v = st
            .query_map([], |r| {
                Ok(LegendDef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    scale: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn legend_rows(&self, id: i64) -> Result<Vec<LegendRow>, Error> {
        rows_in(&self.db, id)
    }

    /// Replacing legend rows is one operation.
    pub fn set_legend_rows(&mut self, id: i64, rows: &[LegendRow]) -> Result<(), Error> {
        self.op("anno_legend_rows", |db| {
            legend_of(db, id)?;
            let inv = Inv::default()
                .delete("anno_legend_row", "legend", id)
                .restore(db, "anno_legend_row", "legend", id)?;
            db.execute("DELETE FROM anno_legend_row WHERE legend=?1", [id])?;
            for (i, r) in rows.iter().enumerate() {
                db.execute(
                    "INSERT INTO anno_legend_row(legend,idx,swatch,label) VALUES (?1,?2,?3,?4)",
                    params![id, i as i64, swatch_json(r.swatch).to_string(), r.label],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    /// Rows derived from what is already on the view: line styles, fills and detail components (no duplicates).
    pub fn legend_rows_from_view(&self, view: AnnoView) -> Result<Vec<LegendRow>, Error> {
        let mut out: Vec<LegendRow> = vec![];
        let mut put = |swatch: LegendSwatch, label: String| {
            if !out.iter().any(|r| r.swatch == swatch) {
                out.push(LegendRow { swatch, label });
            }
        };
        for a in self.annos(view)? {
            match &a.shape {
                AnnoShape::Line { .. } => {
                    if let Some(s) = a.line_style {
                        put(LegendSwatch::Line(s), a.line.name.clone());
                    }
                }
                AnnoShape::Region { fill, .. } => {
                    put(LegendSwatch::Region(*fill), String::new());
                }
                AnnoShape::Detail { def, .. } => {
                    let name = a.detail.as_ref().map_or(String::new(), |d| d.name.clone());
                    put(LegendSwatch::Detail(*def), name);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Legend drawing: the origin is the top-left corner, rows go downward.
    pub fn legend_view(&self, id: i64) -> Result<PlanView, Error> {
        let db = &self.db;
        let def = legend_of(db, id)?;
        let view = AnnoView::Legend(id);
        let k = self.anno_scale(view)? / 1000.0;
        let styles = Styles::load(db)?;
        let item = |shape: AnnoShape, ts: Option<i64>, ls: Option<i64>| -> Result<Anno, Error> {
            styles.resolve(self, 0, view, shape, ts, ls)
        };
        let mut prims = vec![];
        let note =
            |pos: (f64, f64), s: &str, h: f64, prims: &mut Vec<ViewPrim>| -> Result<(), Error> {
                if s.trim().is_empty() {
                    return Ok(());
                }
                let mut a = item(AnnoShape::Text(TextNote::new(pos, s)), None, None)?;
                a.text.height_mm = h;
                prims.push(ViewPrim::Anno { item: a });
                Ok(())
            };
        note((0.0, 0.0), &def.name, TITLE_H_MM, &mut prims)?;
        let rows = rows_in(db, id)?;
        let mut item_rows = vec![];
        for (i, r) in rows.iter().enumerate() {
            let y = -(i as f64 + 1.0) * ROW_MM * k - (ROW_MM - TITLE_H_MM) * k;
            let (x0, x1) = (0.0, SWATCH_MM * k);
            let half = ROW_MM * 0.3 * k;
            match r.swatch {
                LegendSwatch::None => {}
                LegendSwatch::Line(s) => item_rows.push((
                    AnnoShape::Line {
                        pts: vec![(x0, y), (x1, y)],
                        closed: false,
                    },
                    Some(s),
                )),
                LegendSwatch::Region(fill) => item_rows.push((
                    AnnoShape::Region {
                        rings: vec![rect(x0, y - half, x1, y + half)],
                        fill,
                        boundary: true,
                    },
                    None,
                )),
                LegendSwatch::Detail(d) => item_rows.push((
                    AnnoShape::Detail {
                        def: d,
                        at: (x0, y),
                        rot_deg: 0.0,
                        length: Some(x1),
                        flip: false,
                    },
                    None,
                )),
                LegendSwatch::WallType(t) => {
                    let th = wall_thickness(db, t)?.max(1e-3) / 2.0;
                    item_rows.push((
                        AnnoShape::Region {
                            rings: vec![rect(x0, y - th, x1, y + th)],
                            fill: RegionFill::Hatch {
                                deg: 45.0,
                                step_mm: 1.5,
                                cross: false,
                            },
                            boundary: true,
                        },
                        None,
                    ));
                }
            }
            note(
                (LABEL_X_MM * k, y + LABEL_H_MM * k / 2.0),
                &r.label,
                LABEL_H_MM,
                &mut prims,
            )?;
        }
        for (shape, ls) in item_rows {
            prims.push(ViewPrim::Anno {
                item: item(shape, None, ls)?,
            });
        }
        prims.extend(super::view_prims(self, view)?);
        let n = rows.len() as f64;
        let bbox = [0.0, -(n + 1.0) * ROW_MM * k, 90.0 * k, 0.0];
        Ok(PlanView { prims, bbox })
    }

    pub fn place_legend(&mut self, sheet: i64, legend: i64, pos: (f64, f64)) -> Result<i64, Error> {
        self.op("anno_legend_place", |db| {
            legend_of(db, legend)?;
            let n: i64 = db.query_row("SELECT count(*) FROM sheet WHERE id=?1", [sheet], |r| {
                r.get(0)
            })?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such sheet {sheet}")));
            }
            db.execute(
                "INSERT INTO anno_legend_place(sheet,legend,x,y) VALUES (?1,?2,?3,?4)",
                params![sheet, legend, pos.0, pos.1],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno_legend_place", "id", id)))
        })
    }

    pub fn move_legend_place(&mut self, id: i64, pos: (f64, f64)) -> Result<(), Error> {
        self.op("anno_legend_place_move", |db| {
            let inv = Inv::default().restore(db, "anno_legend_place", "id", id)?;
            let n = db.execute(
                "UPDATE anno_legend_place SET x=?2, y=?3 WHERE id=?1",
                params![id, pos.0, pos.1],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such legend place {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn remove_legend_place(&mut self, id: i64) -> Result<(), Error> {
        self.op("anno_legend_place_remove", |db| {
            let inv = Inv::default().restore(db, "anno_legend_place", "id", id)?;
            if db.execute("DELETE FROM anno_legend_place WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid(format!("no such legend place {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn legend_places(&self, sheet: i64) -> Result<Vec<LegendPlace>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT id,legend,x,y FROM anno_legend_place WHERE sheet=?1 ORDER BY id",
        )?;
        let v = st
            .query_map([sheet], |r| {
                Ok(LegendPlace {
                    id: r.get(0)?,
                    sheet,
                    legend: r.get(1)?,
                    pos: (r.get(2)?, r.get(3)?),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }
}
