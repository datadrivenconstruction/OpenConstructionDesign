//! Drawing set as data: sheets with stable keys (sheet field `dset`), section and elevation
//! views to ensure (matched by kind and name), viewports, schedule parts and levels to tag.
//! Applying a plan again updates the sheets of the earlier run in place (same ids): one undo step.

use std::collections::BTreeSet;

use rusqlite::types::Value;

use super::*;
use crate::{SectionSpec, TagCat, TagSpec};

/// Sheet field holding the stable key of a generated sheet.
pub const DSET_FIELD: &str = "dset";
/// Sheet field of a typical floor plan: the ids of the levels it stands for, comma separated;
/// their floor levels are marked on the plan.
pub const DSET_LEVELS: &str = "dset_levels";
/// Token in a schedule key, replaced by the id of the drawing set's sheet set.
pub const DSET_SET: &str = "{set}";

/// What a generated viewport shows: a level plan or the n-th view of `DsetPlan::views`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DsetRef {
    Plan { level: i64, cut: f64 },
    Def(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DsetViewport {
    pub view: DsetRef,
    pub scale: f64,
    /// Centre on the sheet, mm from the lower left corner.
    pub center: (f64, f64),
    pub title: String,
}

/// A schedule part: `key` as on the sheet (`doors@0-24` for rows 0..24), `pos` its top left,
/// `max_h` the column height, mm (0: up to the frame).
#[derive(Clone, Debug, PartialEq)]
pub struct DsetSchedule {
    pub key: String,
    pub pos: (f64, f64),
    pub max_h: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DsetSheet {
    /// Stable across runs: `cover`, `plan:<level>`, `sections:1`, `schedules:2`, ...
    pub key: String,
    pub def: SheetDef,
    pub viewports: Vec<DsetViewport>,
    pub schedules: Vec<DsetSchedule>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DsetPlan {
    pub set_name: String,
    pub views: Vec<SectionSpec>,
    pub sheets: Vec<DsetSheet>,
    /// Tags the set moves clear of the sheet's texts and marks, by host.
    pub tags: Vec<TagSpec>,
}

/// Mark prefixes of doors and windows in the drawings' language (`D1`, `W1`; GOST 21.501
/// uses its own letters); an opening without a mark gets the prefix and its type's number.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkPrefix {
    pub door: String,
    pub window: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DsetApplied {
    pub sheets: Vec<i64>,
    pub set: i64,
    pub views: Vec<i64>,
    /// Sheets of an earlier run the plan no longer has.
    pub removed: usize,
}

impl Model {
    /// Sheets made by an earlier drawing set: (id, stable key), in sheet order.
    pub fn drawing_set_sheets(&self) -> Result<Vec<(i64, String)>, Error> {
        Ok(self
            .sheets()?
            .into_iter()
            .filter_map(|(id, s)| s.fields.get(DSET_FIELD).map(|k| (id, k.clone())))
            .collect())
    }

    /// Writes a drawing set plan as one undo step; see the module note. A step, not a
    /// transaction: it runs inside an AI plan's or a command's own `atomic`.
    pub fn apply_drawing_set(&mut self, p: &DsetPlan) -> Result<DsetApplied, Error> {
        self.step(|m| apply(m, p)).map(|(v, _)| v)
    }

    /// Tags for drawings on `levels`: rooms first, so door and window tags keep clear of them;
    /// tags already there stay. Doors and windows without a mark are marked by type first
    /// (see [`Model::mark_by_type`]), so no tag goes out blank. One undo step (none when
    /// nothing is new), nestable as above; the count of new tags.
    pub fn tag_for_drawings(
        &mut self,
        levels: &[i64],
        prefix: &MarkPrefix,
    ) -> Result<usize, Error> {
        self.step(|m| {
            m.mark_by_type(levels, prefix)?;
            let mut n = 0;
            for &lv in levels {
                for cat in [TagCat::Room, TagCat::Door, TagCat::Window] {
                    n += m.tag_all(lv, &[cat], false)?.len();
                }
            }
            Ok(n)
        })
        .map(|(n, _)| n)
    }
}

fn apply(m: &mut Model, p: &DsetPlan) -> Result<DsetApplied, Error> {
    let mut out = DsetApplied::default();
    let have = m.view_defs()?;
    for spec in &p.views {
        let found = have
            .iter()
            .find(|(_, s)| s.kind == spec.kind && s.name == spec.name);
        let id = match found {
            Some((id, s)) => {
                if s != spec {
                    m.update_view_def(*id, spec)?;
                }
                *id
            }
            None => m.add_view_def(spec)?,
        };
        out.views.push(id);
    }
    let old: Vec<(i64, SheetDef)> = m
        .sheets()?
        .into_iter()
        .filter(|(_, s)| s.fields.contains_key(DSET_FIELD))
        .collect();
    // numbers may move between sheets of the set: park them first
    for (id, s) in &old {
        let mut t = s.clone();
        t.number = format!("~dset~{id}");
        m.update_sheet(*id, &t)?;
    }
    for sh in &p.sheets {
        let mut def = sh.def.clone();
        def.fields.insert(DSET_FIELD.into(), sh.key.clone());
        let prev = old
            .iter()
            .find(|(id, s)| s.fields.get(DSET_FIELD) == Some(&sh.key) && !out.sheets.contains(id));
        let id = match prev {
            Some((id, s)) => {
                // a stamp value typed by hand on the sheet stays where the plan has none
                for (k, v) in s.fields.iter().filter(|(k, _)| *k != DSET_LEVELS) {
                    let e = def.fields.entry(k.clone()).or_default();
                    if e.is_empty() {
                        e.clone_from(v);
                    }
                }
                m.update_sheet(*id, &def)?;
                *id
            }
            None => m.add_sheet(&def)?,
        };
        out.sheets.push(id);
    }
    for (id, _) in &old {
        if !out.sheets.contains(id) {
            m.delete_sheet(*id)?;
            out.removed += 1;
        }
    }
    let sets = m.sheet_sets()?;
    out.set = match sets.into_iter().find(|s| s.1 == p.set_name) {
        Some((id, ..)) => {
            m.update_sheet_set(id, &p.set_name, &out.sheets)?;
            id
        }
        None => m.add_sheet_set(&p.set_name, &out.sheets)?,
    };
    for (sh, &id) in p.sheets.iter().zip(&out.sheets) {
        place_views(m, id, sh, &out.views)?;
        for (sid, _) in m.schedule_places(id)? {
            m.delete_schedule_place(sid)?;
        }
        for s in &sh.schedules {
            let key = s.key.replace(DSET_SET, &out.set.to_string());
            let pid = m.add_schedule_place(&SchedulePlace {
                sheet: id,
                schedule: key,
                pos: s.pos,
            })?;
            if s.max_h > 0.0 {
                let split = ScheduleSplit {
                    max_h: s.max_h,
                    gap: DEFAULT_COLUMN_GAP,
                };
                m.set_schedule_split(pid, split)?;
            }
        }
    }
    // the set reads in plan order after any sheet drawn by hand
    let mut order: Vec<i64> = m
        .sheets()?
        .into_iter()
        .map(|s| s.0)
        .filter(|i| !out.sheets.contains(i))
        .collect();
    order.extend(&out.sheets);
    m.reorder_sheets(&order)?;
    m.place_tags(&p.tags)?;
    Ok(out)
}

/// An opening's type for marking: kind and size to the millimetre.
type OpeningType = (String, i64, i64, i64);

/// Openings on `levels`: id, type, mark ("" when none).
fn openings_on(m: &Model, levels: &[i64]) -> Result<Vec<(i64, OpeningType, String)>, Error> {
    let mut st = m.db.prepare(
        "SELECT o.id, o.kind, o.width, o.height, o.sill, \
         (SELECT CAST(value AS TEXT) FROM prop WHERE kind='opening' AND ref=o.id AND key='mark') \
         FROM opening o JOIN wall w ON w.id=o.wall WHERE w.level=?1 ORDER BY o.id",
    )?;
    let mm = |v: f64| (v * 1000.0).round() as i64;
    let mut out = vec![];
    for &lv in levels {
        let rows = st.query_map([lv], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, kind, w, h, sill, mark) = row?;
            let mark = mark.unwrap_or_default().trim().to_string();
            out.push((id, (kind, mm(w), mm(h), mm(sill)), mark));
        }
    }
    Ok(out)
}

impl Model {
    /// Doors and windows on `levels` without a mark: what [`Model::mark_by_type`] would mark.
    pub fn unmarked_openings(&self, levels: &[i64]) -> Result<usize, Error> {
        Ok(openings_on(self, levels)?
            .iter()
            .filter(|o| o.2.is_empty())
            .count())
    }

    /// Marks doors and windows on `levels` that have none, by type (kind and size): a type
    /// some opening already marks with one mark keeps it, others get the prefix and the next
    /// number no opening of the model uses ("D3"). One operation; the count marked.
    pub fn mark_by_type(&mut self, levels: &[i64], prefix: &MarkPrefix) -> Result<usize, Error> {
        let ops = openings_on(self, levels)?;
        if ops.iter().all(|o| !o.2.is_empty()) {
            return Ok(0);
        }
        let used: BTreeSet<String> = {
            let mut st = self.db.prepare(
                "SELECT CAST(value AS TEXT) FROM prop WHERE kind='opening' AND key='mark'",
            )?;
            let v = st
                .query_map([], |r| r.get::<_, Option<String>>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            v.into_iter()
                .flatten()
                .map(|s| s.trim().to_string())
                .collect()
        };
        let mut by_type: BTreeMap<&OpeningType, BTreeSet<&str>> = BTreeMap::new();
        for (_, t, mark) in &ops {
            let e = by_type.entry(t).or_default();
            if !mark.is_empty() {
                e.insert(mark.as_str());
            }
        }
        let mut taken = used;
        let mut mark_of: BTreeMap<&OpeningType, String> = BTreeMap::new();
        // types in a fixed order (kind, then size), so the same model numbers alike
        for (t, marks) in &by_type {
            if !ops.iter().any(|o| &o.1 == *t && o.2.is_empty()) {
                continue;
            }
            let mark = match marks.iter().next() {
                Some(m) if marks.len() == 1 => m.to_string(),
                _ => {
                    let p = if t.0 == "door" {
                        &prefix.door
                    } else {
                        &prefix.window
                    };
                    let n = (1..)
                        .find(|n| !taken.contains(&format!("{p}{n}")))
                        .unwrap_or(1);
                    format!("{p}{n}")
                }
            };
            taken.insert(mark.clone());
            mark_of.insert(*t, mark);
        }
        let marks: Vec<(i64, String)> = ops
            .iter()
            .filter(|o| o.2.is_empty())
            .filter_map(|o| mark_of.get(&o.1).map(|m| (o.0, m.clone())))
            .collect();
        self.op("tag_mark", |db| {
            let mut inv = Inv::default();
            for (id, mark) in &marks {
                inv = inv.sql(
                    "DELETE FROM prop WHERE kind='opening' AND ref=?1 AND key='mark'",
                    vec![Value::Integer(*id)],
                );
                db.execute(
                    "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) \
                     VALUES ('opening',?1,'mark',?2,'')",
                    params![id, mark],
                )?;
            }
            Ok((marks.len(), marks[0].0, inv))
        })
    }
}

/// Viewports of an earlier run are updated in order (their ids stay); extras go, new ones add.
fn place_views(m: &mut Model, id: i64, sh: &DsetSheet, views: &[i64]) -> Result<(), Error> {
    let old = m.viewports(id)?;
    for (k, v) in sh.viewports.iter().enumerate() {
        let view = match v.view {
            DsetRef::Plan { level, cut } => ViewRef::Plan { level, cut },
            DsetRef::Def(i) => ViewRef::View(
                *views
                    .get(i)
                    .ok_or_else(|| Error::Invalid(format!("drawing set: no view {i}")))?,
            ),
        };
        let def = ViewportDef {
            sheet: id,
            view,
            scale: v.scale,
            center: v.center,
            crop: None,
            title: v.title.clone(),
        };
        match old.get(k) {
            Some((_, d)) if *d == def => {}
            Some((vid, _)) => m.update_viewport(*vid, &def)?,
            None => {
                m.add_viewport(&def)?;
            }
        }
    }
    for (vid, _) in old.iter().skip(sh.viewports.len()) {
        m.delete_viewport(*vid)?;
    }
    Ok(())
}
