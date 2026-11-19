//! Drawing quality: plan view range (top, cut, bottom, depth), pens in mm per scale with separate
//! cut and projection styles and halftone, line patterns and hatch families (own libraries).
//! Settings live in the `drawq_style` key/value table, created on first use (additive, no schema
//! step); every write is one `op`, so one user action is one undo step.

mod bands;
mod hatch;
mod pattern;
mod pens;
mod range;
mod unify;

use std::collections::HashMap;

use ddc_geom::arc::CHORD_SAG;
use ddc_geom::Tol;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value as J;

use crate::ops::Inv;
use crate::phases::ViewKey;
use crate::views::{GridAxis, LineClass, PlanView, PlanViewSpec, ViewPrim};
use crate::{Error, Model};

pub use bands::{ElemBand, Extent};
pub use hatch::{HatchError, HatchFamily, HatchLib, HatchPattern};
pub use pattern::{LinePattern, PatternLib, Seg};
pub use pens::{
    default_style, halftone, resolve, screen_px, LineStyle, Pen, PenTable, ResolvedLine,
    StyleMap, StyleOverride, MAX_MM, MIN_MM,
};
pub use range::{Band, Plane, ViewRange};
pub(crate) use unify::migrate_legacy_range;

/// Share of the way to the paper colour that halftone lines are blended.
pub const HALFTONE_K: f32 = 0.55;

/// Pens, libraries and category styles of one view, loaded once for a renderer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawLook {
    pub pens: PenTable,
    pub patterns: PatternLib,
    pub hatches: HatchLib,
    pub styles: StyleMap,
}

impl DrawLook {
    /// Nothing customised: renderers keep their own weights and dashes.
    pub fn is_default(&self) -> bool {
        self.pens == PenTable::default()
            && self.patterns.user.is_empty()
            && self.hatches.user.is_empty()
            && self.styles.is_empty()
    }

    /// Weight, pattern name, colour and halftone of a line of `category` in `band` at `1:denom`.
    pub fn line(&self, category: &str, band: Band, denom: u32) -> ResolvedLine {
        resolve(&self.pens, self.styles.get(category), band, denom)
    }
}

const DDL: &str = "CREATE TABLE IF NOT EXISTS drawq_style(key TEXT PRIMARY KEY, spec TEXT NOT NULL)";
const PENS: &str = "pens";
const PATTERNS: &str = "patterns";
const HATCHES: &str = "hatches";

fn get(db: &Connection, key: &str) -> Result<Option<J>, Error> {
    db.execute_batch(DDL)?;
    let s: Option<String> = db
        .query_row("SELECT spec FROM drawq_style WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Writes (or with `None` removes) a key; the returned inverse puts the old row back.
fn put(db: &Connection, key: &str, val: Option<&J>) -> Result<Inv, Error> {
    db.execute_batch(DDL)?;
    let old: Option<String> = db
        .query_row("SELECT spec FROM drawq_style WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?;
    let k = Value::Text(key.into());
    let inv = match old {
        Some(o) => Inv::default().sql(
            "INSERT OR REPLACE INTO drawq_style(key,spec) VALUES (?1,?2)",
            vec![k, Value::Text(o)],
        ),
        None => Inv::default().sql("DELETE FROM drawq_style WHERE key=?1", vec![k]),
    };
    match val {
        Some(v) => db.execute(
            "INSERT OR REPLACE INTO drawq_style(key,spec) VALUES (?1,?2)",
            rusqlite::params![key, v.to_string()],
        )?,
        None => db.execute("DELETE FROM drawq_style WHERE key=?1", [key])?,
    };
    Ok(inv)
}

fn style_key(v: ViewKey) -> String {
    let (k, r) = v.parts();
    format!("ov:{k}:{r}")
}

impl Model {
    /// View range of a plan as four planes, read through the plan's stored view range
    /// ([`Model::plan_view_range`]); an open top reads as an infinite `top`.
    pub fn plan_range(&self, level: i64) -> Result<ViewRange, Error> {
        Ok(ViewRange::from_plan(self.plan_view_range(level)))
    }

    /// The range of a level when it differs from the default one (set here, in the view
    /// graphics window or by a view template).
    pub fn plan_range_saved(&self, level: i64) -> Result<Option<ViewRange>, Error> {
        let r = self.plan_view_range(level);
        Ok((r != crate::ViewRange::default()).then(|| ViewRange::from_plan(r)))
    }

    /// One undo step, written to the plan's view range. Fails (nothing written) when the planes
    /// are out of order, the level is unknown or a view template controls the range.
    pub fn set_plan_range(&mut self, level: i64, r: ViewRange) -> Result<(), Error> {
        r.check()?;
        let n: i64 = self
            .db
            .query_row("SELECT count(*) FROM level WHERE id=?1", [level], |x| x.get(0))?;
        if n == 0 {
            return Err(Error::Invalid(format!("level {level} not found")));
        }
        self.set_view_range(ViewKey::Plan(level), r.to_plan())
    }

    /// Back to the default range (one undo step).
    pub fn reset_plan_range(&mut self, level: i64) -> Result<(), Error> {
        self.set_view_range(ViewKey::Plan(level), crate::ViewRange::default())
    }

    /// Plan of a level drawn with its view range: the cut plane is `range.cut`; elements above it up
    /// to the top are overhead (dashed), those below it down to the bottom are seen, down to the
    /// depth are beyond, and the rest is left out. Walls, openings, curtain walls and stairs are
    /// sorted; every other kind is sorted by [`Model::plan_bands`] for the live plan.
    pub fn plan_view_ranged(&self, level: i64, grids: Vec<GridAxis>) -> Result<PlanView, Error> {
        let r = self.plan_range(level)?;
        let mut v = self.plan_view(&PlanViewSpec {
            level,
            cut_height: r.cut,
            grids,
        })?;
        let db = &self.db;
        let bands: HashMap<(&'static str, i64), Band> = self
            .plan_bands(level, &r)?
            .into_iter()
            .map(|b| ((b.table, b.id), b.band))
            .collect();
        v.prims.retain_mut(|p| match p {
            ViewPrim::Edge {
                table,
                element,
                class,
                ..
            } => match bands.get(&(*table, *element)) {
                None => true,
                Some(b) if *table == "wall" => match b.line_class() {
                    Some(c) => {
                        *class = c;
                        true
                    }
                    None => false,
                },
                Some(Band::Hidden) => false,
                Some(Band::Overhead) => {
                    *class = LineClass::Overhead;
                    true
                }
                Some(Band::Beyond) => {
                    *class = LineClass::Beyond;
                    true
                }
                Some(_) => true,
            },
            ViewPrim::DoorLeaf { opening, .. }
            | ViewPrim::DoorSwing { opening, .. }
            | ViewPrim::WindowLine { opening, .. } => {
                bands.get(&("opening", *opening)) != Some(&Band::Hidden)
            }
            ViewPrim::CurtainLine { curtain, .. } => {
                bands.get(&("curtain_wall", *curtain)) != Some(&Band::Hidden)
            }
            _ => true,
        });
        let over: Vec<i64> = bands
            .iter()
            .filter(|((t, _), b)| *t == "wall" && **b == Band::Overhead)
            .map(|((_, id), _)| *id)
            .collect();
        if !over.is_empty() {
            let elev: f64 = db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |x| {
                x.get(0)
            })?;
            let (walls, joined) = crate::views::load_walls(db, level, elev, Tol::default())?;
            for (i, w) in walls.iter().enumerate() {
                if !over.contains(&w.id) {
                    continue;
                }
                for pr in joined.rings[i].iter().flatten() {
                    let ring = pr.tessellate(CHORD_SAG);
                    for k in 0..ring.len() {
                        let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                        v.prims.push(ViewPrim::Edge {
                            table: "wall",
                            element: w.id,
                            class: LineClass::Overhead,
                            a: (a.x, a.y),
                            b: (b.x, b.y),
                        });
                    }
                }
            }
        }
        Ok(v)
    }

    /// The project pen table (the built-in one until edited).
    pub fn drawq_pens(&self) -> Result<PenTable, Error> {
        Ok(get(&self.db, PENS)?.map_or_else(PenTable::default, |j| PenTable::from_json(&j)))
    }

    pub fn set_drawq_pens(&mut self, t: &PenTable) -> Result<(), Error> {
        if !t.check() {
            return Err(Error::Invalid("bad pen table".into()));
        }
        let j = t.to_json();
        self.op("drawq_pens", move |db| {
            Ok(((), 0, put(db, PENS, Some(&j))?))
        })
    }

    pub fn drawq_patterns(&self) -> Result<PatternLib, Error> {
        Ok(get(&self.db, PATTERNS)?.map_or_else(PatternLib::default, |j| PatternLib::from_json(&j)))
    }

    pub fn set_drawq_patterns(&mut self, l: &PatternLib) -> Result<(), Error> {
        if !l.user.iter().all(LinePattern::check) {
            return Err(Error::Invalid("bad line pattern".into()));
        }
        let j = l.to_json();
        self.op("drawq_patterns", move |db| {
            Ok(((), 0, put(db, PATTERNS, Some(&j))?))
        })
    }

    pub fn drawq_hatches(&self) -> Result<HatchLib, Error> {
        Ok(get(&self.db, HATCHES)?.map_or_else(HatchLib::default, |j| HatchLib::from_json(&j)))
    }

    pub fn set_drawq_hatches(&mut self, l: &HatchLib) -> Result<(), Error> {
        if !l.user.iter().all(HatchPattern::check) {
            return Err(Error::Invalid("bad hatch pattern".into()));
        }
        let j = l.to_json();
        self.op("drawq_hatches", move |db| {
            Ok(((), 0, put(db, HATCHES, Some(&j))?))
        })
    }

    /// Pens, libraries and category styles of a view in one read.
    pub fn drawq_look(&self, v: ViewKey) -> Result<DrawLook, Error> {
        Ok(DrawLook {
            pens: self.drawq_pens()?,
            patterns: self.drawq_patterns()?,
            hatches: self.drawq_hatches()?,
            styles: self.view_styles(v)?,
        })
    }

    /// Cut and projection overrides of a view by category.
    pub fn view_styles(&self, v: ViewKey) -> Result<StyleMap, Error> {
        let Some(j) = get(&self.db, &style_key(v))? else {
            return Ok(StyleMap::new());
        };
        Ok(j.as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, s)| (k.clone(), StyleOverride::from_json(s)))
                    .filter(|(_, s)| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Sets (or with `None` or an empty override clears) the override of one category in a view.
    pub fn set_view_style(
        &mut self,
        v: ViewKey,
        category: &str,
        ov: Option<StyleOverride>,
    ) -> Result<(), Error> {
        let mut map = self.view_styles(v)?;
        match ov.filter(|o| !o.is_empty()) {
            Some(o) => map.insert(category.to_string(), o),
            None => map.remove(category),
        };
        self.set_view_styles(v, &map)
    }

    /// Replaces all category overrides of a view in one undo step. Empty overrides are dropped; an
    /// unknown category writes nothing.
    pub fn set_view_styles(&mut self, v: ViewKey, styles: &StyleMap) -> Result<(), Error> {
        for k in styles.keys() {
            if !crate::view_gfx::GFX_CATEGORIES.iter().any(|(c, _)| c == k) {
                return Err(Error::Invalid(format!("unknown category {k}")));
            }
        }
        let map: StyleMap = styles
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect();
        let j: J = J::Object(map.iter().map(|(k, s)| (k.clone(), s.to_json())).collect());
        let key = style_key(v);
        let val = if map.is_empty() { None } else { Some(j) };
        self.op("drawq_style", move |db| {
            Ok(((), 0, put(db, &key, val.as_ref())?))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};

    /// Five parallel walls 5 m long, 2 m apart, with the given (base_offset, height).
    fn walls(zs: &[(f64, f64)]) -> (Model, i64, Vec<i64>) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let specs: Vec<WallSpec> = zs
            .iter()
            .enumerate()
            .map(|(i, (b, h))| WallSpec {
                a: (0.0, 2.0 * i as f64),
                b: (5.0, 2.0 * i as f64),
                level: lv,
                base_offset: *b,
                height: *h,
                wall_type: wt,
                offset0: None,
            })
            .collect();
        let ids = m.add_walls(&specs).unwrap();
        (m, lv, ids)
    }

    fn classes(v: &PlanView, id: i64) -> Vec<LineClass> {
        let mut c: Vec<LineClass> = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::Edge {
                    table: "wall",
                    element,
                    class,
                    ..
                } if *element == id => Some(*class),
                _ => None,
            })
            .collect();
        c.dedup();
        c
    }

    #[test]
    fn range_default_set_undo_and_reset() {
        let (mut m, lv, _) = walls(&[(0.0, 3.0)]);
        // an untouched plan reads the stored default: open top, no depth below the bottom
        let unset = ViewRange::from_plan(crate::ViewRange::default());
        assert_eq!(m.plan_range(lv).unwrap(), unset);
        assert_eq!(m.plan_range_saved(lv).unwrap(), None);
        let r = ViewRange {
            top: 4.0,
            cut: 1.5,
            bottom: 0.25,
            depth: -1.0,
        };
        m.set_plan_range(lv, r).unwrap();
        assert_eq!(m.plan_range(lv).unwrap(), r);
        assert_eq!(m.plan_view_range(lv), r.to_plan(), "one stored range");
        m.undo().unwrap();
        assert_eq!(m.plan_range(lv).unwrap(), unset, "one undo step");
        m.set_plan_range(lv, r).unwrap();
        m.reset_plan_range(lv).unwrap();
        assert_eq!(m.plan_range(lv).unwrap(), unset);
        m.undo().unwrap();
        assert_eq!(m.plan_range(lv).unwrap(), r);
    }

    #[test]
    fn bad_range_or_level_writes_nothing() {
        let (mut m, lv, _) = walls(&[(0.0, 3.0)]);
        let bad = ViewRange {
            top: 0.0,
            cut: 1.0,
            bottom: 0.0,
            depth: 0.0,
        };
        assert!(m.set_plan_range(lv, bad).is_err());
        assert!(m.set_plan_range(9999, ViewRange::default()).is_err());
        assert_eq!(m.plan_range_saved(lv).unwrap(), None);
    }

    #[test]
    fn plan_bands_by_range() {
        // cut 1.2; walls: through the cut, overhead, seen, beyond, hidden
        let (mut m, lv, ids) = walls(&[
            (0.0, 3.0),
            (2.0, 0.5),
            (0.0, 1.0),
            (0.0, 0.4),
            (0.0, 0.1),
        ]);
        m.set_plan_range(
            lv,
            ViewRange {
                top: 3.0,
                cut: 1.2,
                bottom: 0.5,
                depth: 0.2,
            },
        )
        .unwrap();
        let v = m.plan_view_ranged(lv, vec![]).unwrap();
        assert_eq!(classes(&v, ids[0]), vec![], "cut walls come as sections, not edges");
        assert_eq!(classes(&v, ids[1]), vec![LineClass::Overhead]);
        assert_eq!(classes(&v, ids[2]), vec![LineClass::Projection]);
        assert_eq!(classes(&v, ids[3]), vec![LineClass::Beyond]);
        assert_eq!(classes(&v, ids[4]), vec![], "below the depth is not drawn");
        assert!(v.count(|p| matches!(p, ViewPrim::Cut { wall, .. } if *wall == ids[0])) > 0);
    }

    #[test]
    fn narrow_top_hides_overhead() {
        let (mut m, lv, ids) = walls(&[(2.0, 0.5)]);
        m.set_plan_range(
            lv,
            ViewRange {
                top: 1.8,
                cut: 1.2,
                bottom: 0.0,
                depth: -0.5,
            },
        )
        .unwrap();
        let v = m.plan_view_ranged(lv, vec![]).unwrap();
        assert_eq!(classes(&v, ids[0]), vec![]);
    }

    #[test]
    fn pens_patterns_hatches_persist_with_one_undo_each() {
        let (mut m, _, _) = walls(&[(0.0, 3.0)]);
        assert_eq!(m.drawq_pens().unwrap(), PenTable::default());
        let mut t = PenTable::default();
        t.set(Pen {
            name: "mine".into(),
            columns: vec![(50, 0.33)],
        });
        m.set_drawq_pens(&t).unwrap();
        assert_eq!(m.drawq_pens().unwrap(), t);
        m.undo().unwrap();
        assert_eq!(m.drawq_pens().unwrap(), PenTable::default());
        assert!(m.set_drawq_pens(&PenTable { pens: vec![] }).is_err());

        let mut pl = PatternLib::default();
        pl.set(LinePattern {
            name: "mine".into(),
            segs: vec![Seg::Dash(5.0), Seg::Gap(2.0)],
        });
        m.set_drawq_patterns(&pl).unwrap();
        assert_eq!(m.drawq_patterns().unwrap(), pl);
        let bad = PatternLib {
            user: vec![LinePattern {
                name: "".into(),
                segs: vec![],
            }],
        };
        assert!(m.set_drawq_patterns(&bad).is_err());

        let mut hl = HatchLib::default();
        let mut h = hl.get("cross").unwrap();
        h.name = "mine".into();
        hl.set(h);
        m.set_drawq_hatches(&hl).unwrap();
        assert_eq!(m.drawq_hatches().unwrap(), hl);
        m.undo().unwrap();
        assert_eq!(m.drawq_hatches().unwrap(), HatchLib::default());
        assert_eq!(m.drawq_patterns().unwrap(), pl, "undo touched only the last step");
    }

    #[test]
    fn view_style_overrides_per_category() {
        let (mut m, lv, _) = walls(&[(0.0, 3.0)]);
        let v = ViewKey::Plan(lv);
        assert!(m.view_styles(v).unwrap().is_empty());
        let ov = StyleOverride {
            cut: Some(LineStyle::new("heavy", "solid")),
            projection: Some(LineStyle::new("hair", "dash")),
            halftone: true,
        };
        m.set_view_style(v, "wall", Some(ov.clone())).unwrap();
        assert_eq!(m.view_styles(v).unwrap().get("wall"), Some(&ov));
        assert!(m.view_styles(ViewKey::ThreeD).unwrap().is_empty());
        assert!(m.set_view_style(v, "nonsense", Some(ov)).is_err());
        m.set_view_style(v, "wall", Some(StyleOverride::default()))
            .unwrap();
        assert!(m.view_styles(v).unwrap().is_empty(), "empty override clears");
        m.undo().unwrap();
        assert!(m.view_styles(v).unwrap().contains_key("wall"));
    }

    #[test]
    fn resolved_weight_follows_scale() {
        let t = PenTable::default();
        let a = resolve(&t, None, Band::Cut, 20);
        let b = resolve(&t, None, Band::Cut, 500);
        assert!(a.mm > b.mm);
    }

    #[test]
    fn look_is_default_until_something_is_set() {
        let (mut m, lv, _) = walls(&[(0.0, 3.0)]);
        let v = ViewKey::Plan(lv);
        assert!(m.drawq_look(v).unwrap().is_default());
        m.set_view_style(
            v,
            "wall",
            Some(StyleOverride {
                halftone: true,
                ..Default::default()
            }),
        )
        .unwrap();
        let look = m.drawq_look(v).unwrap();
        assert!(!look.is_default());
        assert!(look.line("wall", Band::Cut, 100).halftone);
        assert!(!look.line("slab", Band::Cut, 100).halftone, "other categories keep their look");
        m.undo().unwrap();
        assert!(m.drawq_look(v).unwrap().is_default());
    }

    #[test]
    fn ranged_plan_keeps_openings_of_hidden_walls_out() {
        // a wall wholly above the top plane is outside the range: no edges, no overhead dashes
        let (mut m, lv, ids) = walls(&[(0.0, 3.0), (5.0, 3.0)]);
        m.set_plan_range(lv, ViewRange::default()).unwrap();
        let v = m.plan_view_ranged(lv, vec![]).unwrap();
        assert!(classes(&v, ids[1]).is_empty());
        assert!(!classes(&v, ids[0]).is_empty() || v.count(|p| matches!(p, ViewPrim::Cut { .. })) > 0);
    }
}
