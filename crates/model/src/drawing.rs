//! 2D drawing: layers, lines, polylines with bulge, arcs, texts, blocks and inserts with attributes.
//! The `d_*` tables are readable with plain SQL; each add/remove is one undoable operation.

use rusqlite::{params, types::Value, Connection};

use crate::ops::Inv;
use crate::{Error, Model};

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub x: f64,
    pub y: f64,
    pub bulge: f64,
}

/// Per-entity style; `None` = "by layer". Lineweight is in hundredths of a mm (DXF 370).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub color: Option<i64>,
    pub true_color: Option<i64>,
    pub linetype: Option<String>,
    pub lineweight: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Entity {
    Line,
    Polyline,
    Arc,
    Text,
    Insert,
}

type Child = Option<(&'static str, &'static str)>;
type Pt = (f64, f64);

impl Entity {
    /// (table, child table and link column)
    fn tables(self) -> (&'static str, Child) {
        match self {
            Entity::Line => ("d_line", None),
            Entity::Polyline => ("d_polyline", Some(("d_vertex", "polyline"))),
            Entity::Arc => ("d_arc", None),
            Entity::Text => ("d_text", None),
            Entity::Insert => ("d_insert", Some(("d_attr", "insert_id"))),
        }
    }
}

fn invalid<T>(msg: &str) -> Result<T, Error> {
    Err(Error::Invalid(msg.into()))
}

fn need(db: &Connection, table: &str, id: i64) -> Result<(), Error> {
    let n: i64 = db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return invalid(&format!("no such row in {table}"));
    }
    Ok(())
}

fn need_target(db: &Connection, layer: i64, block: Option<i64>) -> Result<(), Error> {
    need(db, "d_layer", layer)?;
    block.map_or(Ok(()), |b| need(db, "d_block", b))
}

impl Model {
    pub fn add_layer(
        &mut self,
        name: &str,
        color: i64,
        linetype: &str,
        weight: f64,
    ) -> Result<i64, Error> {
        self.op("draw_layer", |db| {
            db.execute(
                "INSERT INTO d_layer(name,color,linetype,weight) VALUES (?1,?2,?3,?4)",
                params![name, color, linetype, weight],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("d_layer", "id", id)))
        })
    }

    pub fn set_layer_color(&mut self, layer: i64, color: i64) -> Result<(), Error> {
        self.op("draw_layer_color", |db| {
            need(db, "d_layer", layer)?;
            let inv = Inv::default().restore(db, "d_layer", "id", layer)?;
            db.execute(
                "UPDATE d_layer SET color=?2 WHERE id=?1",
                params![layer, color],
            )?;
            Ok(((), layer, inv))
        })
    }

    pub fn add_line(
        &mut self,
        layer: i64,
        block: Option<i64>,
        p1: Pt,
        p2: Pt,
    ) -> Result<i64, Error> {
        Ok(self.add_lines(layer, block, &[(p1, p2)])?[0])
    }

    /// A batch of lines is one operation; undo removes the whole batch. Returns ids in order.
    pub fn add_lines(
        &mut self,
        layer: i64,
        block: Option<i64>,
        lines: &[(Pt, Pt)],
    ) -> Result<Vec<i64>, Error> {
        if lines.is_empty() {
            return invalid("no lines");
        }
        self.op("draw_lines", |db| {
            need_target(db, layer, block)?;
            let mut st = db.prepare(
                "INSERT INTO d_line(layer,block,x1,y1,x2,y2) VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            let mut ids = vec![];
            for (p1, p2) in lines {
                st.execute(params![layer, block, p1.0, p1.1, p2.0, p2.1])?;
                ids.push(db.last_insert_rowid());
            }
            let range = vec![Value::Integer(ids[0]), Value::Integer(ids[ids.len() - 1])];
            let inv = Inv::default().sql("DELETE FROM d_line WHERE id BETWEEN ?1 AND ?2", range);
            Ok((ids.clone(), ids[0], inv))
        })
    }

    pub fn add_polyline(
        &mut self,
        layer: i64,
        block: Option<i64>,
        closed: bool,
        vertices: &[Vertex],
    ) -> Result<i64, Error> {
        if vertices.len() < 2 {
            return invalid("polyline needs 2+ vertices");
        }
        self.op("draw_polyline", |db| {
            need_target(db, layer, block)?;
            db.execute(
                "INSERT INTO d_polyline(layer,block,closed) VALUES (?1,?2,?3)",
                params![layer, block, closed],
            )?;
            let id = db.last_insert_rowid();
            for (i, v) in vertices.iter().enumerate() {
                db.execute(
                    "INSERT INTO d_vertex VALUES (?1,?2,?3,?4,?5)",
                    params![id, i as i64, v.x, v.y, v.bulge],
                )?;
            }
            let inv =
                Inv::default()
                    .delete("d_vertex", "polyline", id)
                    .delete("d_polyline", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Arc: center, radius, angles a0..a1 (rad, counter-clockwise).
    pub fn add_arc(
        &mut self,
        layer: i64,
        block: Option<i64>,
        c: Pt,
        r: f64,
        a0: f64,
        a1: f64,
    ) -> Result<i64, Error> {
        if r <= 0.0 {
            return invalid("radius must be positive");
        }
        self.op("draw_arc", |db| {
            need_target(db, layer, block)?;
            db.execute(
                "INSERT INTO d_arc(layer,block,cx,cy,r,a0,a1) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![layer, block, c.0, c.1, r, a0, a1],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("d_arc", "id", id)))
        })
    }

    pub fn add_text(
        &mut self,
        layer: i64,
        block: Option<i64>,
        pos: Pt,
        height: f64,
        rotation: f64,
        content: &str,
    ) -> Result<i64, Error> {
        self.op("draw_text", |db| {
            need_target(db, layer, block)?;
            db.execute(
                "INSERT INTO d_text(layer,block,x,y,height,rotation,content) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![layer, block, pos.0, pos.1, height, rotation, content],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("d_text", "id", id)))
        })
    }

    /// Entity style (color, linetype and lineweight); `None` in a field = "by layer".
    pub fn set_style(&mut self, kind: Entity, id: i64, st: &Style) -> Result<(), Error> {
        let (table, _) = kind.tables();
        self.op("draw_style", |db| {
            need(db, table, id)?;
            let inv = Inv::default().restore(db, table, "id", id)?;
            db.execute(
                &format!(
                    "UPDATE {table} SET color=?2,true_color=?3,linetype=?4,lineweight=?5 WHERE id=?1"
                ),
                params![id, st.color, st.true_color, st.linetype, st.lineweight],
            )?;
            Ok(((), id, inv))
        })
    }

    pub fn style(&self, kind: Entity, id: i64) -> Result<Style, Error> {
        let (table, _) = kind.tables();
        need(&self.db, table, id)?;
        Ok(self.db.query_row(
            &format!("SELECT color,true_color,linetype,lineweight FROM {table} WHERE id=?1"),
            [id],
            |r| {
                Ok(Style {
                    color: r.get(0)?,
                    true_color: r.get(1)?,
                    linetype: r.get(2)?,
                    lineweight: r.get(3)?,
                })
            },
        )?)
    }

    /// Makes the text multi-line (MTEXT): frame width and attachment point 1..9.
    pub fn set_mtext(&mut self, text: i64, width: f64, attach: i64) -> Result<(), Error> {
        if !(1..=9).contains(&attach) || width < 0.0 {
            return invalid("mtext attach must be 1..9 and width >= 0");
        }
        self.op("draw_mtext", |db| {
            need(db, "d_text", text)?;
            let inv = Inv::default().restore(db, "d_text", "id", text)?;
            db.execute(
                "UPDATE d_text SET mtext_width=?2,attach=?3 WHERE id=?1",
                params![text, width, attach],
            )?;
            Ok(((), text, inv))
        })
    }

    /// Block definition: name, base point, attribute descriptions (tag, default value, position, height).
    pub fn add_block(
        &mut self,
        name: &str,
        base: Pt,
        attdefs: &[(&str, &str, Pt, f64)],
    ) -> Result<i64, Error> {
        self.op("draw_block", |db| {
            db.execute(
                "INSERT INTO d_block(name,bx,by) VALUES (?1,?2,?3)",
                params![name, base.0, base.1],
            )?;
            let id = db.last_insert_rowid();
            for (tag, def, p, h) in attdefs {
                db.execute(
                    "INSERT INTO d_attdef VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id, tag, def, p.0, p.1, h],
                )?;
            }
            let inv = Inv::default()
                .delete("d_attdef", "block", id)
                .delete("d_block", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Block insert; unspecified attributes get their default value, an unknown tag is an error.
    pub fn add_insert(
        &mut self,
        layer: i64,
        def: i64,
        pos: Pt,
        scale: Pt,
        rotation: f64,
        attrs: &[(&str, &str)],
    ) -> Result<i64, Error> {
        self.op("draw_insert", |db| {
            need_target(db, layer, Some(def))?;
            let mut values: Vec<(String, String)> = {
                let mut st = db.prepare(
                    "SELECT tag, default_value FROM d_attdef WHERE block=?1 ORDER BY tag",
                )?;
                let rows = st.query_map([def], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            for (tag, val) in attrs {
                match values.iter_mut().find(|(t, _)| t == tag) {
                    Some(slot) => slot.1 = val.to_string(),
                    None => return invalid(&format!("unknown attribute tag {tag}")),
                }
            }
            db.execute(
                "INSERT INTO d_insert(layer,def,x,y,sx,sy,rotation) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![layer, def, pos.0, pos.1, scale.0, scale.1, rotation],
            )?;
            let id = db.last_insert_rowid();
            for (tag, val) in &values {
                db.execute(
                    "INSERT INTO d_attr VALUES (?1,?2,?3)",
                    params![id, tag, val],
                )?;
            }
            let inv = Inv::default()
                .delete("d_attr", "insert_id", id)
                .delete("d_insert", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn delete_entity(&mut self, kind: Entity, id: i64) -> Result<(), Error> {
        let (table, child) = kind.tables();
        self.op("draw_delete", |db| {
            need(db, table, id)?;
            let mut inv = Inv::default().restore(db, table, "id", id)?;
            if let Some((ct, col)) = child {
                inv = inv.restore(db, ct, col, id)?;
                db.execute(&format!("DELETE FROM {ct} WHERE {col}=?1"), [id])?;
            }
            db.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])?;
            Ok(((), id, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(m: &Model, table: &str) -> i64 {
        m.assert_integrity();
        m.sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn thousand_lines_and_block_attribute_via_sql_with_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let layer = m.add_layer("walls", 7, "CONTINUOUS", 0.25).unwrap();
        let lines: Vec<_> = (0..1000)
            .map(|i| ((i as f64, 0.0), (i as f64, 10.0)))
            .collect();
        m.add_lines(layer, None, &lines).unwrap();
        let defs = [
            ("MARK", "D?", (0.1, 0.1), 0.2),
            ("FIRE", "EI30", (0.1, 0.4), 0.2),
        ];
        let blk = m.add_block("DOOR", (0., 0.), &defs).unwrap();
        m.add_line(layer, Some(blk), (0., 0.), (0.9, 0.)).unwrap();
        let ins = m
            .add_insert(layer, blk, (3., 4.), (1., 1.), 0.0, &[("MARK", "D-12")])
            .unwrap();

        let (cnt, len): (i64, f64) = m
            .sql()
            .query_row(
                "SELECT count(*), SUM(y2-y1) FROM d_line l JOIN d_layer a ON a.id=l.layer \
                 WHERE a.name='walls' AND l.block IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((cnt, len), (1000, 10000.0));
        let (mark, fire): (String, String) = m
            .sql()
            .query_row(
                "SELECT (SELECT value FROM d_attr WHERE insert_id=i.id AND tag='MARK'), \
                 (SELECT value FROM d_attr WHERE insert_id=i.id AND tag='FIRE') \
                 FROM d_insert i JOIN d_block b ON b.id=i.def WHERE b.name='DOOR'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((mark.as_str(), fire.as_str()), ("D-12", "EI30"));

        m.delete_entity(Entity::Insert, ins).unwrap();
        assert_eq!((n(&m, "d_insert"), n(&m, "d_attr")), (0, 0));
        m.undo().unwrap();
        assert_eq!((n(&m, "d_insert"), n(&m, "d_attr")), (1, 2));
        m.undo().unwrap(); // insert
        assert_eq!((n(&m, "d_insert"), n(&m, "d_attr")), (0, 0));
        m.undo().unwrap(); // line in the block
        m.undo().unwrap(); // block
        assert_eq!(
            (n(&m, "d_block"), n(&m, "d_attdef"), n(&m, "d_line")),
            (0, 0, 1000)
        );
        m.undo().unwrap(); // batch of 1000 lines
        assert_eq!(n(&m, "d_line"), 0);
        m.undo().unwrap();
        assert_eq!(n(&m, "d_layer"), 0);
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
    }

    #[test]
    fn polyline_arc_text_layer_edit_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_layer("a", 1, "DASHED", 0.5).unwrap();
        let v = |x, y, bulge| Vertex { x, y, bulge };
        let vs = [v(0., 0., 0.), v(2., 0., 0.5), v(2., 2., 0.)];
        let pl = m.add_polyline(l, None, true, &vs).unwrap();
        m.add_arc(l, None, (1., 1.), 2.0, 0.0, 1.5).unwrap();
        m.add_text(l, None, (0., 0.), 0.25, 0.0, "hello").unwrap();
        m.set_layer_color(l, 5).unwrap();
        let bulge: f64 = m
            .sql()
            .query_row(
                "SELECT bulge FROM d_vertex WHERE polyline=?1 AND idx=1",
                [pl],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bulge, 0.5);
        m.undo().unwrap();
        let color: i64 = m
            .sql()
            .query_row("SELECT color FROM d_layer", [], |r| r.get(0))
            .unwrap();
        assert_eq!(color, 1);
        m.delete_entity(Entity::Polyline, pl).unwrap();
        assert_eq!(n(&m, "d_vertex"), 0);
        m.undo().unwrap();
        assert_eq!(n(&m, "d_vertex"), 3);
        assert!(m.add_line(99, None, (0., 0.), (1., 1.)).is_err());
        assert!(m.add_insert(l, 99, (0., 0.), (1., 1.), 0.0, &[]).is_err());
    }
}
