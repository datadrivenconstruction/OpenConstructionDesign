// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drafting views: named 2D canvases with their own scale that hold annotation only (detail
//! lines, regions, clouds, text, spots, detail components). Model elements are never drawn there.
//! Create, rename, rescale, duplicate and delete are one journal operation each.

use rusqlite::{params, types::Value as Sql, Connection, OptionalExtension};

use super::AnnoView;
use crate::ops::Inv;
use crate::{Error, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct DraftingDef {
    pub id: i64,
    pub name: String,
    /// Scale denominator (1:N): text heights and arrows are sheet millimetres times N.
    pub scale: f64,
}

fn drafting_of(db: &Connection, id: i64) -> Result<DraftingDef, Error> {
    db.query_row(
        "SELECT name, scale FROM anno_drafting WHERE id=?1",
        [id],
        |r| {
            Ok(DraftingDef {
                id,
                name: r.get(0)?,
                scale: r.get(1)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such drafting view {id}")))
}

fn clean_name(name: &str) -> Result<&str, Error> {
    let n = name.trim();
    if n.is_empty() {
        return Err(Error::Invalid("drafting view needs a name".into()));
    }
    Ok(n)
}

fn check_scale(scale: f64) -> Result<(), Error> {
    if scale.is_finite() && scale > 0.0 {
        Ok(())
    } else {
        Err(Error::Invalid("scale must be positive".into()))
    }
}

/// Refuses a name another drafting view already has (`except` is the view being renamed).
fn name_free(db: &Connection, name: &str, except: Option<i64>) -> Result<(), Error> {
    let other: Option<i64> = db
        .query_row(
            "SELECT id FROM anno_drafting WHERE name=?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    match other {
        Some(o) if Some(o) != except => Err(Error::Invalid(format!(
            "a drafting view named \"{name}\" already exists"
        ))),
        _ => Ok(()),
    }
}

fn free_name_in(db: &Connection, base: &str) -> Result<String, Error> {
    let base = base.trim();
    let taken = |n: &str| -> Result<bool, Error> {
        let c: i64 = db.query_row(
            "SELECT count(*) FROM anno_drafting WHERE name=?1",
            [n],
            |r| r.get(0),
        )?;
        Ok(c > 0)
    };
    if !base.is_empty() && !taken(base)? {
        return Ok(base.to_string());
    }
    let mut k = 2;
    loop {
        let n = if base.is_empty() {
            k.to_string()
        } else {
            format!("{base} {k}")
        };
        if !taken(&n)? {
            return Ok(n);
        }
        k += 1;
    }
}

impl Model {
    /// New empty drafting view.
    pub fn add_drafting(&mut self, name: &str, scale: f64) -> Result<i64, Error> {
        let name = clean_name(name)?.to_string();
        check_scale(scale)?;
        self.op("anno_drafting_add", |db| {
            name_free(db, &name, None)?;
            db.execute(
                "INSERT INTO anno_drafting(name,scale) VALUES (?1,?2)",
                params![name, scale],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno_drafting", "id", id)))
        })
    }

    pub fn rename_drafting(&mut self, id: i64, name: &str) -> Result<(), Error> {
        let name = clean_name(name)?.to_string();
        self.op("anno_drafting_rename", |db| {
            drafting_of(db, id)?;
            name_free(db, &name, Some(id))?;
            let inv = Inv::default().restore(db, "anno_drafting", "id", id)?;
            db.execute(
                "UPDATE anno_drafting SET name=?2 WHERE id=?1",
                params![id, name],
            )?;
            Ok(((), id, inv))
        })
    }

    /// The view's own scale; a per-view override from `set_anno_scale` is dropped in the same
    /// step so the new scale is the one shown.
    pub fn set_drafting_scale(&mut self, id: i64, scale: f64) -> Result<(), Error> {
        check_scale(scale)?;
        self.op("anno_drafting_scale", |db| {
            drafting_of(db, id)?;
            let mut inv = Inv::default().restore(db, "anno_drafting", "id", id)?;
            let over: Option<f64> = db
                .query_row(
                    "SELECT scale FROM anno_view_scale WHERE view_kind='drafting' AND view_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(s) = over {
                inv = inv.sql(
                    "INSERT OR REPLACE INTO anno_view_scale(view_kind,view_id,scale)
                     VALUES ('drafting',?1,?2)",
                    vec![Sql::Integer(id), Sql::Real(s)],
                );
                db.execute(
                    "DELETE FROM anno_view_scale WHERE view_kind='drafting' AND view_id=?1",
                    [id],
                )?;
            }
            db.execute(
                "UPDATE anno_drafting SET scale=?2 WHERE id=?1",
                params![id, scale],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Copy of the view with all its annotations under a new unique name.
    pub fn duplicate_drafting(&mut self, id: i64, name: &str) -> Result<i64, Error> {
        let name = clean_name(name)?.to_string();
        self.op("anno_drafting_duplicate", |db| {
            let src = drafting_of(db, id)?;
            name_free(db, &name, None)?;
            db.execute(
                "INSERT INTO anno_drafting(name,scale) VALUES (?1,?2)",
                params![name, src.scale],
            )?;
            let new = db.last_insert_rowid();
            db.execute(
                "INSERT INTO anno(view_kind,view_id,kind,text_style,line_style,data)
                 SELECT view_kind,?2,kind,text_style,line_style,data FROM anno
                 WHERE view_kind='drafting' AND view_id=?1 ORDER BY id",
                params![id, new],
            )?;
            db.execute(
                "INSERT INTO anno_view_scale(view_kind,view_id,scale)
                 SELECT view_kind,?2,scale FROM anno_view_scale
                 WHERE view_kind='drafting' AND view_id=?1",
                params![id, new],
            )?;
            let keys = vec![Sql::Integer(new)];
            let inv = Inv::default()
                .sql(
                    "DELETE FROM anno WHERE view_kind='drafting' AND view_id=?1",
                    keys.clone(),
                )
                .sql(
                    "DELETE FROM anno_view_scale WHERE view_kind='drafting' AND view_id=?1",
                    keys,
                )
                .delete("anno_drafting", "id", new);
            Ok((new, new, inv))
        })
    }

    /// The view with its annotations and scale override; annotations of other views stay.
    pub fn delete_drafting(&mut self, id: i64) -> Result<(), Error> {
        self.op("anno_drafting_delete", |db| {
            drafting_of(db, id)?;
            let inv = Inv::default().restore(db, "anno_drafting", "id", id)?;
            let inv = super::drop_view(db, inv, AnnoView::Drafting(id))?;
            db.execute("DELETE FROM anno_drafting WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn draftings(&self) -> Result<Vec<DraftingDef>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT id,name,scale FROM anno_drafting ORDER BY name, id")?;
        let v = st
            .query_map([], |r| {
                Ok(DraftingDef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    scale: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn drafting(&self, id: i64) -> Result<DraftingDef, Error> {
        drafting_of(&self.db, id)
    }

    /// `base` when no drafting view has it, else `base 2`, `base 3`, ...
    pub fn drafting_free_name(&self, base: &str) -> Result<String, Error> {
        free_name_in(&self.db, base)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AnnoShape, TextNote};
    use super::*;
    use crate::views::tests::room_6x4;

    fn line(a: (f64, f64), b: (f64, f64)) -> AnnoShape {
        AnnoShape::Line {
            pts: vec![a, b],
            closed: false,
        }
    }

    fn count(m: &Model, view: AnnoView) -> usize {
        m.annos(view).unwrap().len()
    }

    #[test]
    fn drafting_view_create_rename_delete_undo() {
        let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
        let id = m.add_drafting("  Eave detail ", 10.0).unwrap();
        let d = m.drafting(id).unwrap();
        assert_eq!((d.name.as_str(), d.scale), ("Eave detail", 10.0));
        assert_eq!(m.anno_scale(AnnoView::Drafting(id)).unwrap(), 10.0);
        m.rename_drafting(id, "Eave detail A").unwrap();
        assert_eq!(m.drafting(id).unwrap().name, "Eave detail A");
        m.undo().unwrap();
        assert_eq!(m.drafting(id).unwrap().name, "Eave detail");
        m.set_anno_scale(AnnoView::Drafting(id), 20.0).unwrap();
        m.set_drafting_scale(id, 5.0).unwrap();
        assert_eq!(m.anno_scale(AnnoView::Drafting(id)).unwrap(), 5.0);
        m.undo().unwrap();
        assert_eq!(m.anno_scale(AnnoView::Drafting(id)).unwrap(), 20.0);
        assert_eq!(m.drafting(id).unwrap().scale, 10.0);
        m.add_anno(AnnoView::Drafting(id), &line((0.0, 0.0), (1.0, 0.0)), None, None)
            .unwrap();
        m.delete_drafting(id).unwrap();
        assert!(m.drafting(id).is_err());
        assert!(m.draftings().unwrap().is_empty());
        let left: i64 = m
            .db
            .query_row(
                "SELECT count(*) FROM anno WHERE view_kind='drafting' AND view_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
        // one undo brings back the view, its annotation and its scale override together
        m.undo().unwrap();
        assert_eq!(m.drafting(id).unwrap().name, "Eave detail");
        assert_eq!(count(&m, AnnoView::Drafting(id)), 1);
        assert_eq!(m.anno_scale(AnnoView::Drafting(id)).unwrap(), 20.0);
        m.assert_integrity();
        // undo of the anno, the scale override, then of the create itself
        m.undo().unwrap();
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(m.drafting(id).is_err());
        assert!(m.add_drafting("   ", 10.0).is_err());
        assert!(m.add_drafting("X", 0.0).is_err());
        assert!(m.rename_drafting(id, "Gone").is_err());
        assert!(m.delete_drafting(id).is_err());
    }

    #[test]
    fn detail_line_in_drafting_not_in_plan() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let plan_before = count(&m, AnnoView::Plan(lv));
        let spec = crate::PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        };
        let prims_before = m.plan_view(&spec).unwrap().prims.len();
        let id = m.add_drafting("Sill", 5.0).unwrap();
        let a = m
            .add_anno(AnnoView::Drafting(id), &line((0.0, 0.0), (0.3, 0.0)), None, None)
            .unwrap();
        assert_eq!(m.anno_view(a).unwrap(), AnnoView::Drafting(id));
        assert_eq!(m.annos(AnnoView::Drafting(id)).unwrap()[0].id, a);
        assert_eq!(count(&m, AnnoView::Plan(lv)), plan_before);
        assert_eq!(m.plan_view(&spec).unwrap().prims.len(), prims_before);
        // no sheet holds the view, its own scale drives text and arrow sizes
        assert_eq!(m.anno_view_standard(AnnoView::Drafting(id)).unwrap(), None);
        assert_eq!(m.anno_scale(AnnoView::Drafting(id)).unwrap(), 5.0);
        // a spot in a drafting view reads the drawn height, not a model surface
        let s = m
            .add_anno(
                AnnoView::Drafting(id),
                &AnnoShape::Spot {
                    kind: super::super::SpotKind::Elevation,
                    at: (1.0, 0.25),
                    head: (1.5, 0.75),
                },
                None,
                None,
            )
            .unwrap();
        let spot = m.anno(s).unwrap().spot.unwrap();
        assert!((spot.z - 0.25).abs() < 1e-9 && !spot.free);
        assert!(m
            .add_anno(AnnoView::Drafting(id + 100), &line((0.0, 0.0), (1.0, 0.0)), None, None)
            .is_err());
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(count(&m, AnnoView::Drafting(id)), 0);
        m.assert_integrity();
    }

    #[test]
    fn duplicate_copies_annotations() {
        let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
        let ls = m.line_styles().unwrap()[1].id;
        let id = m.add_drafting("Parapet", 10.0).unwrap();
        let v = AnnoView::Drafting(id);
        m.add_anno(v, &line((0.0, 0.0), (1.0, 0.0)), None, Some(ls))
            .unwrap();
        m.add_anno(v, &AnnoShape::Text(TextNote::new((0.0, 0.5), "Flashing")), None, None)
            .unwrap();
        m.set_anno_scale(v, 20.0).unwrap();
        let name = m.drafting_free_name("Parapet").unwrap();
        assert_eq!(name, "Parapet 2");
        let copy = m.duplicate_drafting(id, &name).unwrap();
        assert_ne!(copy, id);
        let d = m.drafting(copy).unwrap();
        assert_eq!((d.name.as_str(), d.scale), ("Parapet 2", 10.0));
        let (a, b) = (m.annos(v).unwrap(), m.annos(AnnoView::Drafting(copy)).unwrap());
        assert_eq!(b.len(), 2);
        for (x, y) in a.iter().zip(&b) {
            assert_ne!(x.id, y.id);
            assert_eq!(x.shape, y.shape);
            assert_eq!(x.line_style, y.line_style);
        }
        assert_eq!(m.anno_scale(AnnoView::Drafting(copy)).unwrap(), 20.0);
        // the copy is independent of the source
        m.delete_annos(&[b[0].id]).unwrap();
        assert_eq!(count(&m, v), 2);
        m.undo().unwrap();
        // one undo removes the copy with all its annotations and its scale
        m.undo().unwrap();
        assert!(m.drafting(copy).is_err());
        assert_eq!(count(&m, AnnoView::Drafting(copy)), 0);
        assert_eq!(count(&m, v), 2);
        let n: i64 = m
            .db
            .query_row(
                "SELECT count(*) FROM anno_view_scale WHERE view_kind='drafting' AND view_id=?1",
                [copy],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        assert!(m.duplicate_drafting(id, "Parapet").is_err());
        assert!(m.duplicate_drafting(id + 100, "Other").is_err());
        m.assert_integrity();
    }

    #[test]
    fn delete_removes_its_annos_only() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let a = m.add_drafting("A", 10.0).unwrap();
        let b = m.add_drafting("B", 10.0).unwrap();
        m.add_anno(AnnoView::Drafting(a), &line((0.0, 0.0), (1.0, 0.0)), None, None)
            .unwrap();
        m.add_anno(AnnoView::Drafting(b), &line((0.0, 0.0), (2.0, 0.0)), None, None)
            .unwrap();
        m.add_anno(AnnoView::Plan(lv), &line((0.0, 0.0), (3.0, 0.0)), None, None)
            .unwrap();
        let plan = count(&m, AnnoView::Plan(lv));
        m.delete_drafting(a).unwrap();
        assert_eq!(count(&m, AnnoView::Drafting(a)), 0);
        assert_eq!(count(&m, AnnoView::Drafting(b)), 1);
        assert_eq!(count(&m, AnnoView::Plan(lv)), plan);
        assert_eq!(
            m.draftings().unwrap().iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![b]
        );
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(count(&m, AnnoView::Drafting(a)), 1);
        assert_eq!(m.draftings().unwrap().len(), 2);
        m.assert_integrity();
    }

    #[test]
    fn name_unique_refused() {
        let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
        let a = m.add_drafting("Detail", 10.0).unwrap();
        let b = m.add_drafting("Detail 2", 10.0).unwrap();
        let err = m.add_drafting("Detail", 20.0).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert!(m.add_drafting(" Detail ", 20.0).is_err());
        assert!(m.rename_drafting(b, "Detail").is_err());
        assert_eq!(m.drafting(b).unwrap().name, "Detail 2");
        // renaming to its own name is not a clash
        m.rename_drafting(a, "Detail").unwrap();
        assert!(m.duplicate_drafting(a, "Detail 2").is_err());
        assert_eq!(m.draftings().unwrap().len(), 2);
        assert_eq!(m.drafting_free_name("Detail").unwrap(), "Detail 3");
        assert_eq!(m.drafting_free_name("Fresh").unwrap(), "Fresh");
        // refused edits leave no undo step: one undo removes the rename above
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(m.drafting(b).is_err());
        assert_eq!(m.drafting(a).unwrap().name, "Detail");
    }
}
