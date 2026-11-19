//! Generated steel frames (lattice towers, trusses, portal and braced frames) into the model:
//! columns as columns, beams and girders as beams, lattice bars as braces (IfcMember).
//! Member types come from the section table and are reused by name. A frame's decks become a
//! level and a slab each, its flights stairs; a tower's rising chords become columns. Every column
//! standing on the level gets an isolated pad footing sized by its height, unless the frame has piers.

use ddc_geom::stair::Shape;
use ddc_geom::steel::lattice::{Frame, MemRole, FLIGHT};
use ddc_geom::steel::sections;
use rusqlite::OptionalExtension;

use crate::framing::BraceSpec;
use crate::{FootingSpec, FootingStep};
use crate::structure::{BeamJoin, BeamSpec, ColumnSpec};
use crate::{CompHost, CompSpec, Error, Just, LayerSpec, Model, RailingHost, RailingSpec, RampSpec, SlabSpec, StairSpec};
use ddc_geom::component::Value;

/// Slab type of the decks: a steel plate floor on the frame beams.
const DECK_TYPE: (&str, &str, f64) = ("Steel deck 150", "Steel", 0.15);
/// Library family and type of a lift run: car and mast fit the lift plan box of the tower rule.
const LIFT: (&str, &str) = ("Mast Lift", "1500 kg");
/// Library family and type of an insulator string on a cross-arm.
const INSULATOR: (&str, &str) = ("Insulator String", "String");

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SteelFrameIds {
    pub columns: Vec<i64>,
    pub beams: Vec<i64>,
    pub braces: Vec<i64>,
    pub types: Vec<i64>,
    pub footings: Vec<i64>,
    /// Levels made for decks (an existing level at the height is reused), the deck slab type
    /// when new, deck slabs, stairs and lifts (components of the library mast lift).
    pub levels: Vec<i64>,
    pub slab_types: Vec<i64>,
    pub slabs: Vec<i64>,
    pub stairs: Vec<i64>,
    pub lifts: Vec<i64>,
    pub railings: Vec<i64>,
    pub ramps: Vec<i64>,
    /// Insulator string components of a power-line tower.
    pub insulators: Vec<i64>,
}

/// Pad side for a ground column of `height`: 1.5 m plus 4 % of the height, at most 5 m.
pub fn pad_side(height: f64) -> f64 {
    (1.5 + 0.04 * height).min(5.0)
}

impl Model {
    /// Adds every member of `f` on `level` (z of the frame is from the level elevation).
    pub fn add_steel_frame(&mut self, level: i64, f: &Frame) -> Result<SteelFrameIds, Error> {
        let mut out = SteelFrameIds::default();
        let have = self.member_types()?;
        let mut tid = vec![];
        for name in &f.sections {
            let Some(shape) = sections::find(name) else {
                return Err(Error::Invalid(format!("no section '{name}'")));
            };
            let id = match have.iter().find(|t| &t.1 == name) {
                Some(t) => t.0,
                None => {
                    let id = self.add_member_type(name, &shape, "Steel")?;
                    out.types.push(id);
                    id
                }
            };
            tid.push(id);
        }
        let (mut cols, mut beams, mut braces) = (vec![], vec![], vec![]);
        for m in &f.members {
            let (a, b) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
            let (lo, hi) = if a.z <= b.z { (a, b) } else { (b, a) };
            let t = tid[m.sec as usize];
            let column = match m.role {
                MemRole::Column => true,
                MemRole::Chord => f.chords_are_columns,
                _ => false,
            };
            match m.role {
                _ if column && hi.z - lo.z > 1e-3 => cols.push(ColumnSpec {
                    level,
                    top_level: None,
                    member_type: t,
                    structural: true,
                    at: (lo.x, lo.y),
                    base_offset: lo.z,
                    top_offset: hi.z,
                    rotation: 0.0,
                    top_at: ((hi.x - lo.x).abs() + (hi.y - lo.y).abs() > 1e-6).then_some((hi.x, hi.y)),
                }),
                MemRole::Beam | MemRole::Girder => beams.push(BeamSpec {
                    level,
                    member_type: t,
                    a: (a.x, a.y, a.z),
                    b: (b.x, b.y, b.z),
                    bulge: 0.0,
                    yjust: Just::Center,
                    zjust: Just::Center,
                    rotation: 0.0,
                    start_ext: 0.0,
                    end_ext: 0.0,
                    join_start: BeamJoin::None,
                    join_end: BeamJoin::None,
                }),
                _ => braces.push(BraceSpec {
                    level,
                    member_type: t,
                    a: (a.x, a.y, a.z),
                    b: (b.x, b.y, b.z),
                    start: None,
                    end: None,
                    rotation: 0.0,
                }),
            }
        }
        if !cols.is_empty() {
            out.columns = self.add_columns(&cols)?;
            // the highest top of each plan position: a stack is sized for its whole height
            let key = |at: (f64, f64)| ((at.0 * 1e6).round() as i64, (at.1 * 1e6).round() as i64);
            let mut stack: std::collections::HashMap<(i64, i64), f64> = std::collections::HashMap::new();
            for o in &cols {
                let t = stack.entry(key(o.at)).or_insert(o.top_offset);
                *t = t.max(o.top_offset);
            }
            let pads: Vec<FootingSpec> = out
                .columns
                .iter()
                .zip(&cols)
                // a frame with its own piers (the tower) stands on those instead
                .filter(|(_, s)| f.piers.is_empty() && s.base_offset.abs() < 1e-6)
                .map(|(&c, s)| {
                    let top = stack.get(&key(s.at)).copied().unwrap_or(s.top_offset).max(s.top_offset);
                    let w = pad_side(top);
                    FootingSpec {
                        level,
                        column: Some(c),
                        at: (0.0, 0.0),
                        rotation: 0.0,
                        round: false,
                        steps: vec![FootingStep { w, l: w, h: 0.6 + 0.1 * w }],
                        top_offset: 0.0,
                        material: crate::footings::CONCRETE.into(),
                    }
                })
                .collect();
            if !pads.is_empty() {
                out.footings.extend(self.add_footings(&pads)?);
            }
        }
        if !beams.is_empty() {
            out.beams = self.add_beams(&beams)?;
        }
        if !braces.is_empty() {
            out.braces = self.add_braces(&braces)?;
        }
        for g in &f.guards {
            let mut r = RailingSpec::new(level, RailingHost::Path(g.path.clone()));
            r.height = g.height;
            r.post_spacing = g.post_spacing;
            r.baluster_spacing = g.infill_spacing;
            out.railings.push(self.add_railing(&r)?);
        }
        for r in &f.ramps {
            out.ramps.push(self.add_ramp(&RampSpec {
                level,
                top_level: None,
                base_offset: r.base_z,
                top_offset: r.rise,
                width: r.width,
                slope: r.slope,
                thickness: r.thickness,
                landing: r.landing,
                max_run: r.max_run,
                path: r.path.iter().map(|p| (p.x, p.y)).collect(),
                material: "Concrete".into(),
            })?);
        }
        for w in &f.stairs {
            let d = &w.def;
            let id = self.add_stair(&StairSpec {
                level,
                top_level: level,
                base_offset: d.base_z,
                top_offset: d.base_z + d.height,
                shape: d.shape,
                origin: (d.origin.x, d.origin.y),
                angle: d.angle,
                width: d.width,
                max_riser: d.max_riser,
                tread: d.tread,
                landing: d.landing,
                thickness: d.thickness,
                stringer: d.stringer,
                material: "Steel".into(),
            })?;
            out.stairs.push(id);
            let mut r = RailingSpec::new(level, RailingHost::Stair { stair: id, side: 0 });
            r.height = w.height;
            r.post_spacing = w.post_spacing;
            r.baluster_spacing = w.infill_spacing;
            out.railings.extend(self.add_stair_railings(id, &r)?);
        }
        // Masonry piers: three steps, widest at the foot, tops at the leg feet.
        for p in &f.piers {
            let h = (p.z_top - p.z_foot) / 3.0;
            let steps = (0..3)
                .map(|i| {
                    let half = p.half_foot + (p.half_top - p.half_foot) * i as f64 / 2.0;
                    FootingStep { w: 2.0 * half, l: 2.0 * half, h }
                })
                .collect();
            out.footings.push(self.add_footing(&FootingSpec {
                level,
                column: None,
                at: (p.x, p.y),
                rotation: 0.0,
                round: false,
                steps,
                top_offset: p.z_top,
                material: "Masonry".into(),
            })?);
        }
        self.add_frame_floors(level, f, &mut out)?;
        Ok(out)
    }

    /// A connection (plates and bolts by its type's rules) at every free node whose members are
    /// all of the frame `ids`; nodes shared with other members, and nodes a connection cannot be
    /// built on, are left to the person.
    pub fn connect_steel_frame(&mut self, ids: &SteelFrameIds) -> Result<Vec<i64>, Error> {
        let ours: std::collections::HashSet<(&str, i64)> = (ids.columns.iter().map(|&i| ("column", i)))
            .chain(ids.beams.iter().map(|&i| ("beam", i)))
            .chain(ids.braces.iter().map(|&i| ("brace", i)))
            .collect();
        self.connect_members_of(&ours)
    }

    /// Decks as a level and a slab each, flights as U stairs from the frame's level.
    fn add_frame_floors(&mut self, level: i64, f: &Frame, out: &mut SteelFrameIds) -> Result<(), Error> {
        if f.decks.is_empty() && f.flights.is_empty() && f.lifts.is_empty() && f.insulators.is_empty() {
            return Ok(());
        }
        let base: f64 = self.db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| r.get(0))?;
        let mut slabs = vec![];
        if !f.decks.is_empty() {
            let (name, material, t) = DECK_TYPE;
            let have: Option<i64> = self.db.query_row("SELECT id FROM slab_type WHERE name=?1", [name], |r| r.get(0)).optional()?;
            let stype = match have {
                Some(id) => id,
                None => {
                    let id = self.add_slab_type(name, &[LayerSpec { material: material.into(), thickness: t }])?;
                    out.slab_types.push(id);
                    id
                }
            };
            for (i, d) in f.decks.iter().enumerate() {
                let z = base + d.z;
                let found: Option<i64> = self
                    .db
                    .query_row("SELECT id FROM level WHERE abs(elevation - ?1) < 0.01 ORDER BY id LIMIT 1", [z], |r| r.get(0))
                    .optional()?;
                let lv = match found {
                    Some(id) => id,
                    None => {
                        let id = self.add_level(&format!("Platform {}", i + 1), z)?;
                        out.levels.push(id);
                        id
                    }
                };
                let sq = |h: f64| vec![(-h, -h), (h, -h), (h, h), (-h, h)];
                let mut holes: Vec<Vec<(f64, f64)>> = d.holes.iter().map(|b| vec![(b[0], b[1]), (b[2], b[1]), (b[2], b[3]), (b[0], b[3])]).collect();
                if d.open > 0.0 {
                    holes.push(sq(d.open));
                }
                slabs.push(SlabSpec { level: lv, slab_type: stype, offset: 0.0, outer: sq(d.half), holes });
            }
            out.slabs = self.add_slabs(&slabs)?;
        }
        let (tread, riser, width, gap) = FLIGHT;
        for fl in &f.flights {
            // first riser on the +x or -x end of the plan box; the return flight to its left
            let (x, y) = if fl.up_x > 0.0 { (fl.plan[0], fl.plan[1] + width / 2.0) } else { (fl.plan[2], fl.plan[3] - width / 2.0) };
            let s = StairSpec {
                base_offset: fl.z0,
                top_offset: fl.z1,
                shape: Shape::U { left: true, first: None, gap },
                width,
                max_riser: riser,
                tread: Some(tread),
                landing: width,
                material: "Steel".into(),
                ..StairSpec::straight(level, level, (x, y), if fl.up_x > 0.0 { 0.0 } else { std::f64::consts::PI })
            };
            out.stairs.push(self.add_stair(&s)?);
        }
        if !f.lifts.is_empty() {
            let fam = self.load_library_family(LIFT.0)?;
            let ctype = self.comp_type_id(fam, LIFT.1)?.ok_or_else(|| Error::Invalid(format!("no type {} of {}", LIFT.1, LIFT.0)))?;
            let specs: Vec<CompSpec> = f
                .lifts
                .iter()
                .map(|l| CompSpec {
                    level,
                    ctype,
                    at: (l.at.0, l.at.1, l.z0),
                    rotation: 0.0,
                    mirror: false,
                    tilt: None,
                    host: CompHost::None,
                    values: vec![("MastH".into(), Value::Num(l.z1 - l.z0)), ("CarZ".into(), Value::Num(0.0))],
                })
                .collect();
            out.lifts = self.add_components(&specs)?;
        }
        if !f.insulators.is_empty() {
            let fam = self.load_library_family(INSULATOR.0)?;
            let ctype = self.comp_type_id(fam, INSULATOR.1)?.ok_or_else(|| Error::Invalid(format!("no type {} of {}", INSULATOR.1, INSULATOR.0)))?;
            let specs: Vec<CompSpec> = f
                .insulators
                .iter()
                .map(|i| CompSpec {
                    level,
                    ctype,
                    at: (i.at.0, i.at.1, i.z),
                    rotation: 0.0,
                    mirror: false,
                    tilt: None,
                    host: CompHost::None,
                    values: vec![("Length".into(), Value::Num(i.length))],
                })
                .collect();
            out.insulators = self.add_components(&specs)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::steel::tower;

    #[test]
    fn tower_piers_become_masonry_footings() {
        use ddc_geom::steel::{outfit, tower::TowerParams};
        let p = TowerParams::default();
        let mut f = tower::lattice_tower_300(&p).unwrap();
        outfit::outfit(&mut f, &p, &outfit::OutfitRules::default()).unwrap();
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let ids = m.add_steel_frame(lv, &f).unwrap();
        assert_eq!(ids.footings.len(), 4);
        let ft = m.footing(ids.footings[0]).unwrap();
        assert_eq!(ft.material, "Masonry");
        assert_eq!(ft.steps.len(), 3);
        assert!(ft.steps[0].w > ft.steps[2].w);
    }

    #[test]
    fn tower_300_has_stairs_lifts_and_platform_levels() {
        // the H01 counts by construction, on the detail-1 path run_generator takes (no outfit)
        // TowerParams::default() has no lifts; run_generator sets them from rule steel.out.lifts (2)
        let p = tower::TowerParams { lifts: 2, ..tower::TowerParams::default() };
        let f = tower::lattice_tower_300(&p).unwrap();
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let ids = m.add_steel_frame(lv, &f).unwrap();
        assert!(ids.stairs.len() >= 2, "stairs {}", ids.stairs.len());
        assert!(ids.lifts.len() >= 2, "lifts {}", ids.lifts.len());
        assert!(f.decks.len() >= 3 && ids.slabs.len() == f.decks.len(), "decks {}", f.decks.len());
        assert!(ids.levels.len() >= 3, "platforms are levels: {}", ids.levels.len());
        assert!(ids.columns.len() + ids.beams.len() + ids.braces.len() >= 600);
        assert!(ids.braces.len() >= 150, "braces {}", ids.braces.len());
        // every IFC class the case asks for has members behind it
        assert!(!ids.columns.is_empty() && !ids.beams.is_empty(), "columns {} beams {}", ids.columns.len(), ids.beams.len());
        for &s in &ids.slabs {
            let on: i64 = m.sql().query_row("SELECT level FROM slab WHERE id=?1", [s], |r| r.get(0)).unwrap();
            assert!(on == lv || ids.levels.contains(&on), "slab {s} on foreign level {on}");
        }
    }

    #[test]
    fn braced_frame_lands_in_the_model() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let f = tower::braced_frame(2, 1, 6.0, 6.0, 2, 3.5).unwrap();
        let ids = m.add_steel_frame(lv, &f).unwrap();
        assert_eq!(ids.columns.len(), 3 * 2 * 2);
        assert_eq!(ids.columns.len() + ids.beams.len() + ids.braces.len(), f.members.len());
        assert_eq!(ids.types.len(), f.sections.len());
        // only the ground storey's columns stand on footings
        assert_eq!(ids.footings.len(), 3 * 2);
        // Types are reused on a second frame.
        let again = m.add_steel_frame(lv, &f).unwrap();
        assert!(again.types.is_empty());
        let meshes = m.member_meshes().unwrap();
        assert!(meshes.len() >= ids.columns.len() + ids.beams.len());
    }

    #[test]
    fn insulator_strings_become_components_of_the_library_family() {
        use ddc_geom::steel::lattice::InsulatorString;
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let mut f = tower::braced_frame(2, 1, 6.0, 6.0, 2, 3.5).unwrap();
        f.insulators = (0..4).map(|i| InsulatorString { at: (i as f64, 0.0), z: 7.0, length: 2.8 }).collect();
        let ids = m.add_steel_frame(lv, &f).unwrap();
        assert_eq!(ids.insulators.len(), 4);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM comp c JOIN comp_type t ON t.id=c.ctype JOIN comp_family f ON f.id=t.family WHERE f.name='Insulator String'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn decks_become_levels_and_slabs_flights_stairs_rising_chords_columns() {
        use ddc_geom::steel::lattice::{Deck, Flight, LiftRun, Pattern, Secs};
        use ddc_geom::v3;
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 2.0).unwrap();
        let top = m.add_level("Existing", 9.0).unwrap();
        let mut f = Frame::new();
        let s = Secs { chord: "L 120x120x12", diag: "L 70x70x7", strut: "L 60x60x6" };
        let ring = |z: f64, h: f64| [v3(h, h, z), v3(-h, h, z), v3(-h, -h, z), v3(h, -h, z)];
        f.box_lattice(&[ring(0.0, 6.0), ring(3.5, 5.5), ring(7.0, 5.0)], Pattern::X, s, 0, "L 60x60x6");
        let fl = Flight::at((0.0, 0.0), 0.0, 3.5, 1.0);
        f.flights = vec![fl, Flight::at((0.0, 0.0), 3.5, 7.0, -1.0)];
        f.decks = vec![
            Deck { z: 3.5, half: 5.5, open: 0.0, holes: vec![fl.plan] },
            Deck { z: 7.0, half: 5.0, open: 4.0, holes: vec![] },
        ];
        let plain = m.add_steel_frame(lv, &f).unwrap();
        assert!(plain.lifts.is_empty());
        f.lifts = vec![LiftRun { at: (3.0, -3.0), z0: 0.0, z1: 7.0, plan: [2.2, -5.0, 3.8, -1.0] }];
        assert!(plain.columns.is_empty(), "chords stay bracing unless the frame says so");
        f.chords_are_columns = true;
        let ids = m.add_steel_frame(lv, &f).unwrap();
        assert_eq!(ids.columns.len(), 4 * 2);
        assert_eq!(ids.columns.len() + ids.braces.len(), f.members.len());
        // the deck at 7 m lands on the existing level at 9 m elevation; one new level at 5.5 m,
        // made by the first pass and reused by the second
        assert_eq!(plain.levels.len(), 1);
        assert!(ids.levels.is_empty());
        assert!(ids.slab_types.is_empty() && plain.slab_types.len() == 1);
        assert_eq!((ids.slabs.len(), ids.stairs.len()), (2, 2));
        let on: Vec<i64> = ids.slabs.iter().map(|&s| m.sql().query_row("SELECT level FROM slab WHERE id=?1", [s], |r| r.get(0)).unwrap()).collect();
        assert_eq!(on[1], top);
        let el: f64 = m.sql().query_row("SELECT elevation FROM level WHERE id=?1", [on[0]], |r| r.get(0)).unwrap();
        assert!((el - 5.5).abs() < 1e-9);
        let rings = |id: i64| -> i64 { m.sql().query_row("SELECT count(DISTINCT ring) FROM slab_vertex WHERE slab=?1", [id], |r| r.get(0)).unwrap() };
        assert_eq!((rings(ids.slabs[0]), rings(ids.slabs[1])), (2, 2), "stair opening and open centre cut");
        // a lift run is the library mast lift on the frame's level, its mast as tall as the run
        assert_eq!(ids.lifts.len(), 1);
        let c = m.component(ids.lifts[0]).unwrap();
        assert_eq!((c.level, c.at), (lv, (3.0, -3.0, 0.0)));
        assert!(c.values.iter().any(|(k, v)| k == "MastH" && *v == Value::Num(7.0)), "{:?}", c.values);
        let fam: String = m.sql().query_row("SELECT f.name FROM comp_type t JOIN comp_family f ON f.id = t.family WHERE t.id=?1", [c.ctype], |r| r.get(0)).unwrap();
        assert_eq!(fam, LIFT.0);
    }

    #[test]
    fn a_long_span_hall_stands_every_column_on_a_footing() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let f = ddc_geom::steel::longspan::truss_hall(80.0, 20.0, 4, 10.0, 4.0, ddc_geom::steel::lattice::Pattern::Warren).unwrap();
        let ids = m.add_steel_frame(lv, &f).unwrap();
        assert_eq!(ids.columns.len(), 10);
        assert_eq!(ids.footings.len(), 10);
        let held: i64 = m.db.query_row("SELECT count(DISTINCT col) FROM iso_footing WHERE col IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(held, 10);
        assert!((pad_side(20.0) - 2.3).abs() < 1e-9);
    }
}
