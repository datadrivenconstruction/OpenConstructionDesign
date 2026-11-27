// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model totals for the inspector: per level and for the whole model.

use rusqlite::params;

use crate::{Error, Model};

/// SQL condition: the element `(kind, id)` is a member of a design option that the view does not
/// show. `?2` is the chosen option (NULL: every set shows its primary option); the other sets
/// always show their primary option. Same rule as `Model::option_hidden`.
pub(crate) fn hidden_sql(kind: &str, id: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM option_member hm JOIN design_option ho ON ho.id=hm.option_id          WHERE hm.kind={kind} AND hm.ref={id} AND hm.option_id IS NOT CASE          WHEN ho.set_id=(SELECT hd.set_id FROM design_option hd WHERE hd.id=?2) THEN ?2          ELSE (SELECT hp.id FROM design_option hp WHERE hp.set_id=ho.set_id AND hp.is_primary=1          LIMIT 1) END)"
    )
}

/// Quantities and sums. Areas in m², volumes in m³.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub walls: usize,
    pub doors: usize,
    pub windows: usize,
    /// Sum of the layer footprint areas of all walls.
    pub wall_plan_area: f64,
    pub wall_volume: f64,
    pub slabs: usize,
    /// Plan area (by the top layer), without double counting across layers.
    pub slab_area: f64,
    pub slab_volume: f64,
    pub rooms: usize,
    pub room_area: f64,
    pub room_volume: f64,
    pub curtains: usize,
    /// Area of the non-empty curtain-wall panels.
    pub curtain_panel_area: f64,
}

impl Totals {
    pub(crate) fn add(&mut self, o: &Totals) {
        self.walls += o.walls;
        self.doors += o.doors;
        self.windows += o.windows;
        self.wall_plan_area += o.wall_plan_area;
        self.wall_volume += o.wall_volume;
        self.slabs += o.slabs;
        self.slab_area += o.slab_area;
        self.slab_volume += o.slab_volume;
        self.rooms += o.rooms;
        self.room_area += o.room_area;
        self.room_volume += o.room_volume;
        self.curtains += o.curtains;
        self.curtain_panel_area += o.curtain_panel_area;
    }
}

/// Totals for one level.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelTotals {
    pub level: i64,
    pub name: String,
    pub elevation: f64,
    pub totals: Totals,
    /// What the level holds, by category of [`SUMMARY_CATEGORIES`]; only categories with elements.
    pub categories: Vec<CategoryTotal>,
}

/// Elements of one category and their sums. Lengths in m, areas in m², volumes in m³, mass in kg
/// (volume × the density of a known material; 0 where none is known).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CategoryTotal {
    pub key: &'static str,
    pub count: usize,
    pub length: f64,
    pub area: f64,
    pub volume: f64,
    pub mass: f64,
}

/// Categories of the model summary in display order: key and the element table counted for it.
/// Parametric solids join the category of their IFC class ([`SOLID_CATEGORIES`]).
pub const SUMMARY_CATEGORIES: &[(&str, &str)] = &[
    ("wall", "wall"),
    ("curtain_wall", "curtain_wall"),
    ("door", "opening"),
    ("window", "opening"),
    ("opening", "opening"),
    ("slab", "slab"),
    ("roof", "roof"),
    ("ceiling", "ceiling"),
    ("room", "room"),
    ("column", "col"),
    ("beam", "beam"),
    ("brace", "brace"),
    ("truss", "truss"),
    ("beam_system", "beam_system"),
    ("gridshell", "gridshell"),
    ("member", "solid"),
    ("plate", "solid"),
    ("footing", "iso_footing"),
    ("strip_footing", "strip_footing"),
    ("raft", "raft"),
    ("stair", "stair"),
    ("ramp", "ramp"),
    ("railing", "railing"),
    ("component", "comp"),
    ("mep_run", "mep_run"),
    ("mass", "mass"),
    ("mesh", "mesh"),
    ("solid", "solid"),
];

/// IFC class of a parametric solid -> summary category; any other class counts as "solid".
pub const SOLID_CATEGORIES: &[(&str, &str)] = &[
    ("IfcWall", "wall"),
    ("IfcSlab", "slab"),
    ("IfcRoof", "roof"),
    ("IfcColumn", "column"),
    ("IfcBeam", "beam"),
    ("IfcMember", "member"),
    ("IfcPlate", "plate"),
    ("IfcFooting", "footing"),
    ("IfcStair", "stair"),
    ("IfcRamp", "ramp"),
    ("IfcRailing", "railing"),
];

fn solid_category(class: Option<&str>) -> &'static str {
    class.and_then(|c| SOLID_CATEGORIES.iter().find(|r| r.0 == c)).map_or("solid", |r| r.1)
}

/// Length of a solid along its own axis: an extrusion's depth, 0 for other forms.
fn solid_length(def: &str) -> f64 {
    let v: serde_json::Value = serde_json::from_str(def).unwrap_or_default();
    let n = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
    match (v.get("kind").and_then(|k| k.as_str()), n("start"), n("end")) {
        (Some("extrusion"), Some(a), Some(b)) => (b - a).abs(),
        _ => 0.0,
    }
}

impl Model {
    /// Totals per level, ordered by elevation. Elements of non-primary design options are excluded.
    pub fn level_totals(&self) -> Result<Vec<LevelTotals>, Error> {
        self.level_totals_in(None)
    }

    /// Totals as seen by option `option`: shared elements, members of this option and members
    /// of the primary options of the other sets. `None` means every set shows its primary option.
    pub fn level_totals_in(&self, option: Option<i64>) -> Result<Vec<LevelTotals>, Error> {
        let db = &self.db;
        // every query below binds ?1 = level and ?2 = option, used by the hidden_sql conditions
        let one = |sql: &str, lv: i64| -> Result<(i64, f64, f64), Error> {
            Ok(db.query_row(sql, params![lv, option], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        };
        let (hw, hs, hr) = (
            hidden_sql("'wall'", "w.id"),
            hidden_sql("'slab'", "s.id"),
            hidden_sql("'room'", "r.id"),
        );
        let ho = hidden_sql("'opening'", "o.id");
        let levels: Vec<(i64, String, f64)> = {
            let mut st =
                db.prepare("SELECT id, name, elevation FROM level ORDER BY elevation, id")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for (level, name, elevation) in levels {
            let mut t = Totals::default();
            let (n, a, v) = one(
                &format!(
                    "SELECT count(DISTINCT w.id), coalesce(sum(q.plan_area),0), \
                     coalesce(sum(q.volume),0) FROM wall w LEFT JOIN wall_qty q ON q.wall=w.id \
                     WHERE w.level=?1 AND NOT {hw}"
                ),
                level,
            )?;
            (t.walls, t.wall_plan_area, t.wall_volume) = (n as usize, a, v);
            let (n, a, v) = one(
                &format!("SELECT count(DISTINCT s.id), coalesce(sum(CASE WHEN q.pos=0 THEN q.area END),0), \
                 coalesce(sum(q.volume),0) FROM slab s LEFT JOIN slab_qty q ON q.slab=s.id \
                 WHERE s.level=?1 AND NOT {hs}"),
                level,
            )?;
            (t.slabs, t.slab_area, t.slab_volume) = (n as usize, a, v);
            let (n, a, v) = one(
                &format!("SELECT count(DISTINCT r.id), coalesce(sum(q.area),0), coalesce(sum(q.volume),0) \
                 FROM room r LEFT JOIN room_qty q ON q.room=r.id WHERE r.level=?1 AND NOT {hr}"),
                level,
            )?;
            (t.rooms, t.room_area, t.room_volume) = (n as usize, a, v);
            // an opening is hidden with its own option or with its host wall
            let (d, w, _) = one(
                &format!("SELECT coalesce(sum(o.kind='door'),0), coalesce(sum(o.kind='window'),0), 0 \
                 FROM opening o JOIN wall w ON w.id=o.wall \
                 WHERE w.level=?1 AND NOT {hw} AND NOT {ho}"),
                level,
            )?;
            (t.doors, t.windows) = (d as usize, w as usize);
            let cws: Vec<i64> = {
                let mut st = db.prepare(&format!(
                    "SELECT c.id FROM curtain_wall c WHERE c.level=?1 AND NOT {} ORDER BY c.id",
                    hidden_sql("'curtain_wall'", "c.id")
                ))?;
                let v = st
                    .query_map(params![level, option], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                v
            };
            t.curtains = cws.len();
            for id in cws {
                t.curtain_panel_area += self.curtain_layout(id)?.quantities().panel_area;
            }
            let categories = self.category_totals(level, option, &t)?;
            out.push(LevelTotals {
                level,
                name,
                elevation,
                totals: t,
                categories,
            });
        }
        Ok(out)
    }

    /// Categories of level `level` that have elements, in [`SUMMARY_CATEGORIES`] order. Walls,
    /// slabs, rooms, openings and curtain walls take the counts and sums of `t`.
    fn category_totals(
        &self,
        level: i64,
        option: Option<i64>,
        t: &Totals,
    ) -> Result<Vec<CategoryTotal>, Error> {
        let db = &self.db;
        let mut by: Vec<CategoryTotal> = SUMMARY_CATEGORIES
            .iter()
            .map(|(k, _)| CategoryTotal { key: k, ..Default::default() })
            .collect();
        let mut add = |key: &str, n: usize, len: f64, area: f64, vol: f64, material: &str| {
            if let Some(c) = by.iter_mut().find(|c| c.key == key) {
                c.count += n;
                c.length += len;
                c.area += area;
                c.volume += vol;
                c.mass += vol * crate::struct_ext::density(material).unwrap_or(0.0);
            }
        };
        let openings: i64 = db.query_row(
            &format!(
                "SELECT count(*) FROM opening o JOIN wall w ON w.id=o.wall WHERE w.level=?1 \
                 AND NOT {} AND NOT {}",
                hidden_sql("'wall'", "w.id"),
                hidden_sql("'opening'", "o.id")
            ),
            params![level, option],
            |r| r.get(0),
        )?;
        add("wall", t.walls, 0.0, t.wall_plan_area, t.wall_volume, "");
        add("curtain_wall", t.curtains, 0.0, t.curtain_panel_area, 0.0, "");
        add("door", t.doors, 0.0, 0.0, 0.0, "");
        add("window", t.windows, 0.0, 0.0, 0.0, "");
        add("opening", (openings as usize).saturating_sub(t.doors + t.windows), 0.0, 0.0, 0.0, "");
        add("slab", t.slabs, 0.0, t.slab_area, t.slab_volume, "");
        add("room", t.rooms, 0.0, t.room_area, t.room_volume, "");
        // columns and beams: quantities from the structural rebuild
        for (key, table, kind) in [("column", "col", "column"), ("beam", "beam", "beam")] {
            let mut st = db.prepare(&format!(
                "SELECT coalesce(q.length,0), coalesce(q.volume,0), coalesce(t.material,'') \
                 FROM \"{table}\" e LEFT JOIN member_type t ON t.id=e.mtype \
                 LEFT JOIN member_qty q ON q.kind='{kind}' AND q.ref=e.id WHERE e.level=?1"
            ))?;
            let rows = st.query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            for r in rows {
                let (l, v, m): (f64, f64, String) = r?;
                add(key, 1, l, 0.0, v, &m);
            }
        }
        // assemblies (braces, trusses, joists, footings) and the rest: elements from their
        // table, sums from their structural parts where the rebuild made any
        for &(key, table) in SUMMARY_CATEGORIES {
            if matches!(table, "wall" | "curtain_wall" | "opening" | "slab" | "room" | "col" | "beam" | "solid") {
                continue;
            }
            let n: i64 = db
                .query_row(&format!("SELECT count(*) FROM \"{table}\" WHERE level=?1"), [level], |r| r.get(0))
                .unwrap_or(0);
            if n == 0 {
                continue;
            }
            add(key, n as usize, 0.0, 0.0, 0.0, "");
            let mut st = db.prepare(&format!(
                "SELECT q.length, q.volume, q.material FROM struct_qty q JOIN \"{table}\" e \
                 ON e.id=q.ref WHERE q.kind=?1 AND e.level=?2"
            ))?;
            let rows = st.query_map(params![table, level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            for r in rows {
                let (l, v, m): (f64, f64, String) = r?;
                add(key, 0, l, 0.0, v, &m);
            }
        }
        // parametric solids (script members, plates, free forms) by their IFC class
        let mut st = db.prepare(
            "SELECT s.ifc_class, s.def, s.material, coalesce(q.volume,0) FROM solid s \
             LEFT JOIN solid_qty q ON q.solid=s.id WHERE s.void=0 AND s.level=?1",
        )?;
        let rows = st.query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        for r in rows {
            let (class, def, m, v): (Option<String>, String, String, f64) = r?;
            add(solid_category(class.as_deref()), 1, solid_length(&def), 0.0, v, &m);
        }
        by.retain(|c| c.count > 0);
        Ok(by)
    }

    /// Totals for the whole model: the sum over levels.
    pub fn model_totals(&self) -> Result<Totals, Error> {
        let mut t = Totals::default();
        for l in self.level_totals()? {
            t.add(&l.totals);
        }
        Ok(t)
    }
}

/// Category key and the model table behind it, as reports, the command line and headless runs
/// count them. A missing table counts as 0.
pub const COUNTED_CATEGORIES: &[(&str, &str)] = &[
    ("level", "level"),
    ("wall_type", "wall_type"),
    ("wall", "wall"),
    ("opening", "opening"),
    ("slab", "slab"),
    ("curtain_wall", "curtain_wall"),
    ("room", "room"),
    ("grid", "grid"),
    ("column", "col"),
    ("footing", "iso_footing"),
    ("strip_footing", "strip_footing"),
    ("raft", "raft"),
    ("beam", "beam"),
    ("brace", "brace"),
    ("roof", "roof"),
    ("ceiling", "ceiling"),
    ("stair", "stair"),
    ("ramp", "ramp"),
    ("railing", "railing"),
    ("mass", "mass"),
    ("mesh", "mesh"),
    ("solid", "solid"),
    ("component", "comp"),
    ("mep_run", "mep_run"),
    ("toposolid", "toposolid"),
    ("sheet", "sheet"),
];

/// Rows per category of [`COUNTED_CATEGORIES`], in that order.
pub fn category_counts(m: &Model) -> Vec<(&'static str, i64)> {
    COUNTED_CATEGORIES
        .iter()
        .map(|(k, t)| {
            let n = m
                .sql()
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(0);
            (*k, n)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::LevelTotals;
    use crate::sections::tests::house;

    #[test]
    fn totals_sum_levels_and_match_house() {
        let (m, _) = house();
        let lv = m.level_totals().unwrap();
        assert_eq!(lv.len(), 2);
        assert_eq!((lv[0].name.as_str(), lv[1].name.as_str()), ("L1", "L2"));
        let all = m.model_totals().unwrap();
        let (a, b) = (&lv[0].totals, &lv[1].totals);
        assert_eq!(a.walls, 4);
        assert_eq!((a.doors, a.windows), (1, 1));
        // 6×4 Brick 200 walls, inward: 4 walls of 3 m, footprint area = 6·4 − 5.6·3.6 = 3.84
        assert!(
            (a.wall_plan_area - 3.84).abs() < 1e-9,
            "{}",
            a.wall_plan_area
        );
        assert!((a.wall_volume - (3.84 * 3.0 - 0.9 * 2.1 * 0.2 - 1.5 * 1.5 * 0.2)).abs() < 1e-6);
        assert_eq!(a.slabs, 1);
        assert_eq!(b.slabs, 1);
        assert!((a.slab_area - 24.0).abs() < 1e-9);
        assert_eq!(a.rooms + b.rooms, all.rooms);
        assert_eq!(a.curtains + b.curtains, 1);
        assert_eq!(all.walls, a.walls + b.walls);
        assert!((all.slab_volume - (a.slab_volume + b.slab_volume)).abs() < 1e-12);
        assert!((all.wall_volume - (a.wall_volume + b.wall_volume)).abs() < 1e-12);
        assert!(all.curtain_panel_area > 0.0);
    }

    /// The summary lists what the model holds: an empty level lists nothing, a model of script
    /// members lists members with their length and mass and no walls.
    #[test]
    fn categories_list_only_what_the_level_holds() {
        use crate::{Model, SolidForm, SolidFrame, SolidIfc, SolidSection, SolidSpec};
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        assert_eq!(m.level_totals().unwrap()[0].categories, vec![]);
        let sq = vec![
            ddc_geom::v2(0.0, 0.0),
            ddc_geom::v2(0.1, 0.0),
            ddc_geom::v2(0.1, 0.1),
            ddc_geom::v2(0.0, 0.1),
        ];
        for (class, len) in [(Some("IfcMember"), 4.0), (Some("IfcMember"), 2.0), (None, 1.0)] {
            m.add_solid(&SolidSpec {
                level: lv,
                name: "bar".into(),
                void: false,
                material: "Steel S355".into(),
                ifc: class.map(|c| SolidIfc { class: c.into(), predefined: "MEMBER".into() }),
                frame: SolidFrame::default(),
                form: SolidForm::Extrusion { profile: SolidSection::polygon(sq.clone()), start: 0.0, end: len },
            })
            .unwrap();
        }
        let c = &m.level_totals().unwrap()[0].categories;
        let keys: Vec<&str> = c.iter().map(|c| c.key).collect();
        assert_eq!(keys, ["member", "solid"]);
        let mem = &c[0];
        assert_eq!(mem.count, 2);
        assert!((mem.length - 6.0).abs() < 1e-9, "{}", mem.length);
        assert!((mem.volume - 0.06).abs() < 1e-6, "{}", mem.volume);
        assert!((mem.mass - 0.06 * 7850.0).abs() < 1e-2, "{}", mem.mass);
    }

    #[test]
    fn option_filter_hides_walls_slabs_and_hosted_openings() {
        let (mut m, ids) = house();
        let (slab, slab_lv): (i64, i64) = m
            .sql()
            .query_row("SELECT id, level FROM slab ORDER BY id LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let before = m.level_totals().unwrap();
        let (set, _a) = m.add_option_set("S", "A").unwrap();
        let b = m.add_option(set, "B").unwrap();
        let items = [("wall".to_string(), ids[0]), ("slab".to_string(), slab)];
        m.add_to_option(b, &items).unwrap();
        let at = |v: &[LevelTotals], l: i64| v.iter().find(|t| t.level == l).unwrap().totals.clone();
        let primary = m.level_totals().unwrap();
        let shown = m.level_totals_in(Some(b)).unwrap();
        assert_eq!(at(&primary, slab_lv).slabs + 1, at(&before, slab_lv).slabs);
        assert_eq!(at(&shown, slab_lv).slabs, at(&before, slab_lv).slabs);
        let l1 = before[0].level;
        let (p, s) = (at(&primary, l1), at(&shown, l1));
        assert_eq!((p.walls, p.doors, p.windows), (3, 0, 1), "door goes with its hidden wall");
        assert_eq!((s.walls, s.doors, s.windows), (4, 1, 1));
        let sum: usize = before.iter().map(|t| t.totals.walls).sum();
        assert_eq!(m.model_totals().unwrap().walls + 1, sum);
    }
}
