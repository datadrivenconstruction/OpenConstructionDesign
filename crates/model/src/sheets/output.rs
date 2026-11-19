//! Sheet issue (schema 36): splitting a schedule into columns, view numbers on a sheet and "view -> sheet"
//! references, a revision on several sheets at once, reading project fields for the stamp, legend data.

use rusqlite::types::Value;

use super::*;
use crate::LayerSpec;

/// Default gap between columns of a split schedule, mm.
pub const DEFAULT_COLUMN_GAP: f64 = 4.0;

const COLUMNS: [(&str, &str, &str); 3] = [
    ("sheet_schedule", "max_h", "REAL"),
    ("sheet_schedule", "col_gap", "REAL"),
    ("viewport", "detail", "TEXT"),
];

/// Additive, by presence: columns are added to a new file too, migration order does not matter.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    for (t, c, ty) in COLUMNS {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
            [t, c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN {c} {ty}"))?;
        }
    }
    Ok(())
}

/// Schedule splitting on a sheet: `max_h` is the maximum column height, mm (0 means by the sheet frame);
/// `gap` is the gap between columns, mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduleSplit {
    pub max_h: f64,
    pub gap: f64,
}

impl Default for ScheduleSplit {
    fn default() -> Self {
        Self {
            max_h: 0.0,
            gap: DEFAULT_COLUMN_GAP,
        }
    }
}

/// Where a view is placed: viewport, sheet, sheet number and the view number on that sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewPlacement {
    pub viewport: i64,
    pub view: ViewRef,
    pub sheet: i64,
    pub sheet_number: String,
    pub detail: String,
}

/// A wall-type legend row.
#[derive(Clone, Debug, PartialEq)]
pub struct WallTypeLegend {
    pub name: String,
    pub layers: Vec<LayerSpec>,
}

/// A sheet's view numbers in viewport order: a manually set one stays as is, the others get the first
/// free number (a manual "2" does not yield a second "2").
fn number_details(manual: &[Option<String>]) -> Vec<String> {
    let taken: Vec<&str> = manual
        .iter()
        .flatten()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut next = 1usize;
    manual
        .iter()
        .map(|m| match m.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                while taken.contains(&next.to_string().as_str()) {
                    next += 1;
                }
                next += 1;
                (next - 1).to_string()
            }
        })
        .collect()
}

/// A view number is fixed on placement: deleting a neighbour does not change references on issued sheets.
/// The view's own number stays if it is free on the sheet, otherwise the first free one is chosen.
pub(super) fn settle_detail(db: &Connection, id: i64, sheet: i64) -> Result<(), Error> {
    let mut st = db.prepare_cached("SELECT id, detail FROM viewport WHERE sheet=?1 ORDER BY id")?;
    let rows: Vec<(i64, Option<String>)> = st
        .query_map([sheet], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let Some(me) = rows.iter().position(|r| r.0 == id) else {
        return Ok(());
    };
    let norm = |d: &Option<String>| d.as_deref().map_or("", str::trim).to_string();
    let own = norm(&rows[me].1);
    let taken = rows.iter().any(|r| r.0 != id && norm(&r.1) == own);
    if !own.is_empty() && !taken {
        return Ok(());
    }
    let mut manual: Vec<Option<String>> = rows.into_iter().map(|r| r.1).collect();
    manual[me] = None;
    let number = number_details(&manual).swap_remove(me);
    db.execute(
        "UPDATE viewport SET detail=?2 WHERE id=?1",
        params![id, number],
    )?;
    Ok(())
}

impl Model {
    /// Schedule splitting on a sheet (empty columns mean defaults).
    pub fn schedule_split(&self, place: i64) -> Result<ScheduleSplit, Error> {
        let r: Option<(Option<f64>, Option<f64>)> = self
            .db
            .query_row(
                "SELECT max_h, col_gap FROM sheet_schedule WHERE id=?1",
                [place],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((max_h, gap)) = r else {
            return no_such("schedule placement", place);
        };
        let d = ScheduleSplit::default();
        Ok(ScheduleSplit {
            max_h: max_h.unwrap_or(d.max_h),
            gap: gap.unwrap_or(d.gap),
        })
    }

    pub fn set_schedule_split(&mut self, place: i64, s: ScheduleSplit) -> Result<(), Error> {
        self.op("sheet_schedule_split", |db| {
            if !(s.max_h.is_finite() && s.max_h >= 0.0 && s.gap.is_finite() && s.gap >= 0.0) {
                return Err(Error::Invalid("bad schedule split".into()));
            }
            if !exists(db, "sheet_schedule", place)? {
                return no_such("schedule placement", place);
            }
            let inv = Inv::default().restore(db, "sheet_schedule", "id", place)?;
            db.execute(
                "UPDATE sheet_schedule SET max_h=?2, col_gap=?3 WHERE id=?1",
                params![place, s.max_h, s.gap],
            )?;
            Ok(((), place, inv))
        })
    }

    /// The view number on a sheet; an empty string means automatic. A duplicate on the same sheet is an error.
    pub fn set_viewport_detail(&mut self, viewport: i64, detail: &str) -> Result<(), Error> {
        let detail = detail.trim();
        self.op("sheet_viewport_detail", |db| {
            let sheet: Option<i64> = db
                .query_row("SELECT sheet FROM viewport WHERE id=?1", [viewport], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(sheet) = sheet else {
                return no_such("viewport", viewport);
            };
            let n: i64 = db.query_row(
                "SELECT count(*) FROM viewport WHERE sheet=?1 AND id<>?2 AND detail=?3",
                params![sheet, viewport, detail],
                |r| r.get(0),
            )?;
            if n > 0 && !detail.is_empty() {
                return Err(Error::Invalid(format!("view number '{detail}' is taken")));
            }
            let inv = Inv::default().restore(db, "viewport", "id", viewport)?;
            let value = (!detail.is_empty()).then_some(detail);
            db.execute(
                "UPDATE viewport SET detail=?2 WHERE id=?1",
                params![viewport, value],
            )?;
            Ok(((), viewport, inv))
        })
    }

    /// A sheet's view numbers: (viewport, number) in viewport order.
    pub fn viewport_details(&self, sheet: i64) -> Result<Vec<(i64, String)>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT id, detail FROM viewport WHERE sheet=?1 ORDER BY id")?;
        let rows: Vec<(i64, Option<String>)> = st
            .query_map([sheet], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let manual: Vec<Option<String>> = rows.iter().map(|r| r.1.clone()).collect();
        Ok(rows
            .iter()
            .map(|r| r.0)
            .zip(number_details(&manual))
            .collect())
    }

    /// All view placements in sheet order: the basis of cross-references. Computed from the current
    /// numbers, so a reference follows renumbering and moving a view to another sheet.
    pub fn view_placements(&self) -> Result<Vec<ViewPlacement>, Error> {
        let mut out = vec![];
        for (sheet, def) in self.sheets()? {
            let details = self.viewport_details(sheet)?;
            for (viewport, v) in self.viewports(sheet)? {
                let detail = details
                    .iter()
                    .find(|d| d.0 == viewport)
                    .map(|d| d.1.clone())
                    .unwrap_or_default();
                out.push(ViewPlacement {
                    viewport,
                    view: v.view,
                    sheet,
                    sheet_number: def.number.clone(),
                    detail,
                });
            }
        }
        Ok(out)
    }

    /// A revision on all given sheets: one undo step. An empty `mark` means the one after the last
    /// on each sheet.
    pub fn issue_revision(&mut self, sheets: &[i64], rev: &Revision) -> Result<(), Error> {
        self.op("sheet_issue_revision", |db| {
            let mut inv = Inv::default();
            for s in sheets {
                if !exists(db, "sheet", *s)? {
                    return no_such("sheet", *s);
                }
                let idx: i64 = db.query_row(
                    "SELECT COALESCE(MAX(idx),-1)+1 FROM sheet_rev WHERE sheet=?1",
                    [s],
                    |r| r.get(0),
                )?;
                let last: Option<String> = db
                    .query_row(
                        "SELECT mark FROM sheet_rev WHERE sheet=?1 ORDER BY idx DESC LIMIT 1",
                        [s],
                        |r| r.get(0),
                    )
                    .optional()?;
                let mark = if rev.mark.trim().is_empty() {
                    last.map_or_else(|| "1".to_string(), |m| increment_number(&m))
                } else {
                    rev.mark.clone()
                };
                db.execute(
                    "INSERT INTO sheet_rev(sheet,idx,mark,date,description,author) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![s, idx, mark, rev.date, rev.description, rev.author],
                )?;
                inv = Inv::default()
                    .sql(
                        "DELETE FROM sheet_rev WHERE sheet=?1 AND idx=?2",
                        vec![Value::Integer(*s), Value::Integer(idx)],
                    )
                    .then(inv);
            }
            Ok(((), 0, inv))
        })
    }

    /// Auto-set in one action: plan sheets (`add_plan_sheets`) and a set of them; a taken set name
    /// gets a number. One undo step. Returns (set, sheets).
    pub fn add_plan_sheet_set(
        &mut self,
        set_name: &str,
        tpl: &SheetDef,
        levels: &[i64],
        scale: f64,
        center: (f64, f64),
    ) -> Result<(i64, Vec<i64>), Error> {
        let base = set_name.trim().to_string();
        self.op("sheet_add_plan_set", |db| {
            if base.is_empty() {
                return Err(Error::Invalid("sheet set name is empty".into()));
            }
            let (sheets, inv) = plan_sheets(db, tpl, levels, scale, center)?;
            let (mut name, mut k) = (base.clone(), 1);
            while db.query_row(
                "SELECT count(*) FROM sheet_set WHERE name=?1",
                [&name],
                |r| r.get::<_, i64>(0),
            )? > 0
            {
                k += 1;
                name = format!("{base} {k}");
            }
            db.execute("INSERT INTO sheet_set(name) VALUES (?1)", [&name])?;
            let set = db.last_insert_rowid();
            Self::write_set_items(db, set, &sheets)?;
            let inv = Inv::default()
                .delete("sheet_set_item", "sheet_set", set)
                .delete("sheet_set", "id", set)
                .then(inv);
            Ok(((set, sheets), set, inv))
        })
    }

    /// Project fields for the stamp: the same `prop` records the project-information dialog writes; they have
    /// no other storage or editor.
    pub fn project_fields(&self) -> Result<BTreeMap<String, String>, Error> {
        Ok(self
            .props("project_info", 0)?
            .into_iter()
            .filter_map(|(k, v, _)| match v {
                Value::Text(s) if !s.trim().is_empty() => Some((k, s)),
                _ => None,
            })
            .collect())
    }

    /// Materials of wall and slab layers, alphabetical, without repeats.
    pub fn material_names(&self) -> Result<Vec<String>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT material FROM wall_type_layer WHERE material<>''
             UNION SELECT material FROM slab_type_layer WHERE material<>'' ORDER BY 1",
        )?;
        let v = st
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(v)
    }

    /// Wall types with layers, by name.
    pub fn wall_type_legend(&self) -> Result<Vec<WallTypeLegend>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT id, name FROM wall_type ORDER BY name, id")?;
        let types: Vec<(i64, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut layer = self.db.prepare_cached(
            "SELECT material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos",
        )?;
        let mut out = vec![];
        for (id, name) in types {
            let layers = layer
                .query_map([id], |r| {
                    Ok(LayerSpec {
                        material: r.get(0)?,
                        thickness: r.get(1)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            out.push(WallTypeLegend { name, layers });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SectionSpec, ViewKind};

    fn level_sheet(m: &mut Model) -> (i64, i64) {
        let lv = m.add_level("L1", 0.0).unwrap();
        let s = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
        (lv, s)
    }

    fn plan_vp(sheet: i64, level: i64) -> ViewportDef {
        ViewportDef {
            sheet,
            view: ViewRef::Plan {
                level,
                cut: DEFAULT_CUT,
            },
            scale: 100.0,
            center: (150.0, 150.0),
            crop: None,
            title: "Plan".into(),
        }
    }

    #[test]
    fn schema_step_adds_columns_once() {
        let m = Model::open(":memory:").unwrap();
        // a repeated run changes nothing and does not fail on existing columns
        migrate(m.sql()).unwrap();
        for (t, c, _) in COLUMNS {
            let n: i64 = m
                .sql()
                .query_row(
                    "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
                    [t, c],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{t}.{c}");
        }
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    }

    #[test]
    fn split_is_one_undo_step_and_survives_move() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, s) = level_sheet(&mut m);
        let p = m
            .add_schedule_place(&SchedulePlace {
                sheet: s,
                schedule: "doors".into(),
                pos: (40.0, 250.0),
            })
            .unwrap();
        assert_eq!(m.schedule_split(p).unwrap(), ScheduleSplit::default());
        let sp = ScheduleSplit {
            max_h: 80.0,
            gap: 6.0,
        };
        m.set_schedule_split(p, sp).unwrap();
        m.move_schedule_place(p, (60.0, 240.0)).unwrap();
        assert_eq!(m.schedule_split(p).unwrap(), sp);
        m.undo().unwrap();
        assert_eq!(m.schedule_split(p).unwrap(), sp);
        m.undo().unwrap();
        assert_eq!(m.schedule_split(p).unwrap(), ScheduleSplit::default());
        assert!(m
            .set_schedule_split(
                p,
                ScheduleSplit {
                    max_h: -1.0,
                    gap: 0.0
                }
            )
            .is_err());
        assert!(m.schedule_split(p + 100).is_err());
    }

    #[test]
    fn details_number_automatically_and_skip_manual() {
        assert_eq!(
            number_details(&[None, Some("2".into()), None, Some(" ".into())]),
            ["1", "2", "3", "4"]
        );
        assert_eq!(
            number_details(&[None, None, Some("1".into())]),
            ["2", "3", "1"]
        );
        let mut m = Model::open(":memory:").unwrap();
        let (lv, s) = level_sheet(&mut m);
        let a = m.add_viewport(&plan_vp(s, lv)).unwrap();
        let b = m.add_viewport(&plan_vp(s, lv)).unwrap();
        assert_eq!(
            m.viewport_details(s).unwrap(),
            [(a, "1".to_string()), (b, "2".to_string())]
        );
        m.set_viewport_detail(b, "7").unwrap();
        assert!(m.set_viewport_detail(a, "7").is_err());
        // editing a viewport does not lose the number
        let mut d = m.viewport(b).unwrap();
        d.scale = 50.0;
        m.update_viewport(b, &d).unwrap();
        assert_eq!(m.viewport_details(s).unwrap()[1].1, "7");
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.viewport_details(s).unwrap()[1].1, "2");
    }

    #[test]
    fn placements_follow_renumber_and_move_between_sheets() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, s1) = level_sheet(&mut m);
        let s2 = m.add_sheet(&SheetDef::new("A-201", "Sections")).unwrap();
        let sec = m
            .add_view_def(&SectionSpec {
                kind: ViewKind::Section,
                name: "1-1".into(),
                a: (0.0, 0.0),
                b: (0.0, 5.0),
                depth: 5.0,
                far_clip: false,
            })
            .unwrap();
        let vp = m
            .add_viewport(&ViewportDef {
                sheet: s2,
                view: ViewRef::View(sec),
                scale: 50.0,
                center: (100.0, 100.0),
                crop: None,
                title: "1-1".into(),
            })
            .unwrap();
        let of = |m: &Model| {
            m.view_placements()
                .unwrap()
                .into_iter()
                .find(|p| p.view == ViewRef::View(sec))
                .unwrap()
        };
        assert_eq!(of(&m).sheet_number, "A-201");
        assert_eq!(of(&m).detail, "1");
        m.renumber_sheets(&[s2], "B-1").unwrap();
        assert_eq!(of(&m).sheet_number, "B-1");
        let mut d = m.viewport(vp).unwrap();
        d.sheet = s1;
        m.update_viewport(vp, &d).unwrap();
        assert_eq!(of(&m).sheet_number, "A-101");
    }

    #[test]
    fn revision_on_many_sheets_is_one_undo_step() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, a) = level_sheet(&mut m);
        let b = m.add_sheet(&SheetDef::new("A-102", "b")).unwrap();
        m.set_revisions(
            a,
            &[Revision {
                mark: "3".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        let rev = Revision {
            date: "05.11.26".into(),
            description: "Issue".into(),
            ..Default::default()
        };
        m.issue_revision(&[a, b], &rev).unwrap();
        assert_eq!(m.revisions(a).unwrap().last().unwrap().mark, "4");
        assert_eq!(m.revisions(b).unwrap()[0].mark, "1");
        m.undo().unwrap();
        assert_eq!(m.revisions(a).unwrap().len(), 1);
        assert!(m.revisions(b).unwrap().is_empty());
    }

    #[test]
    fn plan_sheet_set_is_one_undo_step_with_free_name() {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 3.0).unwrap();
        m.add_sheet_set("Plans", &[]).unwrap();
        let tpl = SheetDef::new("A-101", "Plan");
        let (set, sheets) = m
            .add_plan_sheet_set("Plans", &tpl, &[l1, l2], 100.0, (170.0, 170.0))
            .unwrap();
        assert_eq!(sheets.len(), 2);
        let sets = m.sheet_sets().unwrap();
        let got = sets.iter().find(|s| s.0 == set).unwrap();
        assert_eq!(got.1, "Plans 2");
        assert_eq!(got.2, sheets);
        assert_eq!(m.sheet(sheets[1]).unwrap().number, "A-102");
        m.undo().unwrap();
        assert!(m.sheets().unwrap().is_empty());
        assert_eq!(m.sheet_sets().unwrap().len(), 1);
    }

    #[test]
    fn project_fields_read_the_project_info_props() {
        let m = Model::open(":memory:").unwrap();
        assert!(m.project_fields().unwrap().is_empty());
        for (k, v) in [
            ("client", "Client LLC"),
            ("address", "  "),
            ("name", "School"),
        ] {
            m.sql()
                .execute(
                    "INSERT INTO prop(kind, ref, key, value, unit)
                     VALUES ('project_info', 0, ?1, ?2, '')",
                    [k, v],
                )
                .unwrap();
        }
        m.sql()
            .execute(
                "INSERT INTO prop(kind, ref, key, value, unit)
                 VALUES ('sheet', 0, 'client', 'foreign', '')",
                [],
            )
            .unwrap();
        let f = m.project_fields().unwrap();
        assert_eq!(f.len(), 2, "{f:?}");
        assert_eq!(f["client"], "Client LLC");
        assert_eq!(f["name"], "School");
    }

    fn numbers(m: &Model, sheet: i64) -> Vec<String> {
        m.viewport_details(sheet)
            .unwrap()
            .into_iter()
            .map(|d| d.1)
            .collect()
    }

    #[test]
    fn deleting_a_viewport_keeps_the_numbers_of_the_others() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let s = m.add_sheet(&SheetDef::new("A-101", "P")).unwrap();
        let a = m.add_viewport(&plan_vp(s, lv)).unwrap();
        m.add_viewport(&plan_vp(s, lv)).unwrap();
        m.add_viewport(&plan_vp(s, lv)).unwrap();
        assert_eq!(numbers(&m, s), ["1", "2", "3"]);
        m.delete_viewport(a).unwrap();
        assert_eq!(numbers(&m, s), ["2", "3"], "issued references stay");
        // the new one takes a free number, undo restores the previous state
        m.add_viewport(&plan_vp(s, lv)).unwrap();
        assert_eq!(numbers(&m, s), ["2", "3", "1"]);
        m.undo().unwrap();
        assert_eq!(numbers(&m, s), ["2", "3"]);
    }

    #[test]
    fn moving_a_viewport_onto_a_taken_number_picks_a_free_one() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let a = m.add_sheet(&SheetDef::new("A-101", "A")).unwrap();
        let b = m.add_sheet(&SheetDef::new("A-102", "B")).unwrap();
        let on_a = m.add_viewport(&plan_vp(a, lv)).unwrap();
        m.set_viewport_detail(on_a, "7").unwrap();
        let on_b = m.add_viewport(&plan_vp(b, lv)).unwrap();
        m.set_viewport_detail(on_b, "7").unwrap();
        m.update_viewport(on_a, &plan_vp(b, lv)).unwrap();
        assert_eq!(numbers(&m, b), ["1", "7"]);
        m.undo().unwrap();
        assert_eq!(numbers(&m, a), ["7"]);
        assert_eq!(numbers(&m, b), ["7"]);
        // a view moves to a free number keeping its own
        m.set_viewport_detail(on_b, "8").unwrap();
        m.update_viewport(on_a, &plan_vp(b, lv)).unwrap();
        assert_eq!(numbers(&m, b), ["7", "8"]);
    }

    #[test]
    fn legends_read_layers_and_materials() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_wall_type(
            "W",
            &[
                LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                },
                LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.1,
                },
            ],
        )
        .unwrap();
        let t = m.wall_type_legend().unwrap();
        let w = t.iter().find(|x| x.name == "W").unwrap();
        assert_eq!(w.layers.len(), 2);
        let names = m.material_names().unwrap();
        assert!(names.iter().any(|n| n == "Brick"));
        assert!(names.iter().any(|n| n == "Concrete"));
    }
}
