//! The hangar outfit of a truss hall: the roof cladding over the top chords, a sliding door
//! across the near gable (y = 0) hung from a track beam under the gable truss, crane runway
//! beams on corbels along both column lines, and an office and workshop annex of rooms on two
//! storeys behind the far gable, with its stair. Every dimension is a `steel.hangar.*` rule
//! (the stair riser `core.max_riser`). Overhead cranes of the library family run on the runway,
//! as many and as strong as `steel.hangar.crane.*` say.

use crate::project::rules::RuleSet;
use crate::{GLayout, GOpening, GRoom, GStair, GWall, GenError, WallRole};
use ddc_geom::stair::Shape;
use ddc_geom::member::Shape as Section;
use ddc_geom::steel::lattice::{Frame, MemRole};
use ddc_geom::steel::sections;
use ddc_geom::v3;

const TRACK: &str = "HEB 300";
const HANGER: &str = "CHS 168.3x8";
const CORBEL: &str = "HEB 300";
const RUNWAY: &str = "HEB 500";
/// The library family the cranes are placed from.
pub const CRANE_FAMILY: &str = "Overhead Crane";

/// What a truss hall gains as a hangar besides its frame: plan positions in the hall's axes,
/// heights above its level.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HallOutfit {
    /// Roof cladding ring over the outer truss lines, at `roof_z`, projecting `overhang`.
    pub roof: Vec<(f64, f64)>,
    pub roof_z: f64,
    pub overhang: f64,
    /// The near gable wall with the hangar door opening, `height` = the eaves.
    pub gable: GLayout,
    /// Annex storeys bottom up, each `height` above the one below; the ground one holds the
    /// stair up to the next.
    pub annex: Vec<GLayout>,
    /// Overhead cranes on the runway pair.
    pub cranes: Vec<HallCrane>,
}

/// One overhead crane: plan centre of its bridge and the rail top, its span rail to rail and
/// its capacity in tonnes; the bridge runs along x, across the hall.
#[derive(Clone, Debug, PartialEq)]
pub struct HallCrane {
    pub at: (f64, f64, f64),
    pub span: f64,
    pub capacity: f64,
}

/// The hall `span` wide, `len` long, bottom chords at `eaves`.
pub(crate) struct Hangar<'a> {
    pub rs: &'a RuleSet,
    pub span: f64,
    pub len: f64,
    pub eaves: f64,
    /// Frames along the hall, `spacing` apart.
    pub bays: usize,
    pub spacing: f64,
}

impl Hangar<'_> {
    fn get(&self, id: &str) -> f64 {
        self.rs.get(id)
    }

    /// Height of the door track: the door's head.
    fn track_z(&self) -> f64 {
        self.eaves - self.get("steel.hangar.track_drop")
    }

    /// Height of the crane runway: the crane girder and its hoist fit under the bottom chords.
    fn crane_z(&self) -> f64 {
        self.eaves - self.get("steel.hangar.crane_headroom")
    }

    /// Refuses a hall too small for its door or too low for its crane.
    pub fn check(&self) -> Result<(), GenError> {
        let bad = |w: String| Err(GenError::Infeasible(w));
        let door_w = self.span - 2.0 * self.get("steel.hangar.door_jamb");
        let least = self.get("steel.hangar.door_min");
        if self.track_z() < least || door_w < least {
            return bad(format!("hangar: a door {door_w:.1} m wide and {:.1} m high is no door; lower the jambs or the track drop", self.track_z()));
        }
        if self.crane_z() <= 0.0 {
            return bad(format!("hangar: a crane headroom of {:.1} m leaves no runway under {:.1} m eaves", self.get("steel.hangar.crane_headroom"), self.eaves));
        }
        if self.get("steel.hangar.corbel_reach") >= self.get("steel.hangar.door_jamb") {
            return bad("hangar: the crane runway would stand in the door; the corbel reach must be less than the door jamb".into());
        }
        Ok(())
    }

    /// Heights the columns of frame `b` are split at, so the beams framing in meet a node.
    pub fn splits(&self, b: usize) -> Vec<f64> {
        if b == 0 {
            vec![self.crane_z(), self.track_z()]
        } else {
            vec![self.crane_z()]
        }
    }

    /// Adds the outfit's members to the frame and returns what the model gets besides.
    pub fn outfit(&self, f: &mut Frame) -> Result<(HallOutfit, Vec<(&'static str, f64)>), GenError> {
        let (s, l) = (self.span, self.len);
        let top = f.nodes.iter().map(|p| p.z).fold(f64::MIN, f64::max);
        let overhang = self.get("steel.hangar.roof_overhang");
        let tracks = self.door_track(f);
        let (corbels, runway) = self.crane_runway(f);
        let jamb = self.get("steel.hangar.door_jamb");
        let (door_w, door_h) = (s - 2.0 * jamb, self.track_z());
        let gable = GLayout {
            walls: vec![GWall::line((0.0, 0.0), (s, 0.0), WallRole::Exterior)],
            openings: vec![GOpening { wall: 0, along: jamb, door: true, width: door_w, height: door_h, sill: 0.0, ty: None }],
            height: self.eaves,
            ..GLayout::default()
        };
        let annex = self.annex()?;
        let rooms: usize = annex.iter().map(|a| a.rooms.len()).sum();
        let annex_area = s * self.get("steel.hangar.annex_depth") * annex.len() as f64;
        let storeys = annex.len();
        let cranes = self.cranes();
        let capacity = cranes.first().map_or(0.0, |c| c.capacity);
        let crane_count = cranes.len();
        let h = HallOutfit { roof: vec![(0.0, 0.0), (s, 0.0), (s, l), (0.0, l)], roof_z: top, overhang, gable, annex, cranes };
        let area = (s + 2.0 * overhang) * (l + 2.0 * overhang);
        let r2 = |x: f64| (x * 100.0).round() / 100.0;
        let metrics = vec![
            ("roof_area_m2", area.round()),
            ("roof_z", r2(top)),
            ("door_width", r2(door_w)),
            ("door_height", r2(door_h)),
            ("door_track_beams", tracks as f64),
            ("crane_rail_z", r2(self.crane_z())),
            ("crane_corbels", corbels as f64),
            ("crane_runway_beams", runway as f64),
            ("cranes", crane_count as f64),
            ("crane_capacity_t", capacity),
            ("annex_storeys", storeys as f64),
            ("annex_rooms", rooms as f64),
            ("annex_area_m2", annex_area.round()),
        ];
        Ok((h, metrics))
    }

    /// Track beam across the gable at the door head, from column to column, hung from every
    /// bottom chord node of the gable truss between them; returns the track beams.
    fn door_track(&self, f: &mut Frame) -> usize {
        let z = self.track_z();
        let mut xs: Vec<f64> = f.nodes.iter().filter(|p| p.y.abs() < 1e-6 && (p.z - self.eaves).abs() < 1e-6).map(|p| p.x).collect();
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        if xs.len() < 2 {
            return 0;
        }
        let mut n = 0;
        for w in xs.windows(2) {
            n += f.add(v3(w[0], 0.0, z), v3(w[1], 0.0, z), TRACK, MemRole::Beam, v3(0.0, 1.0, 0.0)).is_some() as usize;
        }
        for &x in &xs[1..xs.len().saturating_sub(1)] {
            f.add(v3(x, 0.0, z), v3(x, 0.0, self.eaves), HANGER, MemRole::Strut, v3(0.0, 1.0, 0.0));
        }
        n
    }

    /// A corbel off each column at the crane height, reaching into the hall, and a runway beam
    /// from corbel tip to corbel tip along both column lines; returns (corbels, runway beams).
    fn crane_runway(&self, f: &mut Frame) -> (usize, usize) {
        let (z, reach) = (self.crane_z(), self.get("steel.hangar.corbel_reach"));
        let (mut corbels, mut runway) = (0, 0);
        for (cx, rx) in [(0.0, reach), (self.span, self.span - reach)] {
            for b in 0..=self.bays {
                let y = b as f64 * self.spacing;
                corbels += f.add(v3(cx, y, z), v3(rx, y, z), CORBEL, MemRole::Beam, v3(0.0, 0.0, 1.0)).is_some() as usize;
                if b > 0 {
                    let y0 = y - self.spacing;
                    runway += f.add(v3(rx, y0, z), v3(rx, y, z), RUNWAY, MemRole::Beam, v3(0.0, 0.0, 1.0)).is_some() as usize;
                }
            }
        }
        (corbels, runway)
    }

    /// The annex behind the far gable (y = len): `annex_depth` deep across the span, cut into
    /// rooms about `annex_room` wide. The first room on every storey is the stair hall; the
    /// ground storey's rooms are workshops with a door to the hall, the upper ones offices
    /// opening one into the next.
    fn annex(&self) -> Result<Vec<GLayout>, GenError> {
        let (s, y0) = (self.span, self.len);
        let d = self.get("steel.hangar.annex_depth");
        let h = self.get("steel.hangar.annex_storey");
        let storeys = self.get("steel.hangar.annex_storeys").round() as usize;
        let (dw, dh) = (self.get("steel.hangar.annex_door_width"), self.get("steel.hangar.annex_door_height"));
        let n = ((s / self.get("steel.hangar.annex_room")).floor() as usize).max(2);
        let w = s / n as f64;
        let x = |k: usize| k as f64 * w;
        let (stair, lo, hi) = self.stair((w / 2.0, y0 + d / 2.0), h)?;
        if hi.0 - lo.0 >= w || hi.1 - lo.1 >= d || dh > h || dw >= w.min(d) {
            return Err(GenError::Infeasible(format!("hangar: the annex rooms {w:.1} x {d:.1} m hold neither the stair nor the doors; widen annex_room or annex_depth")));
        }
        let ring = |a: (f64, f64), b: (f64, f64)| vec![a, (b.0, a.1), b, (a.0, b.1)];
        let mut out = Vec::with_capacity(storeys);
        for i in 0..storeys {
            let ground = i == 0;
            let mut walls = vec![
                GWall::line((0.0, y0), (s, y0), WallRole::Exterior),
                GWall::line((s, y0), (s, y0 + d), WallRole::Exterior),
                GWall::line((s, y0 + d), (0.0, y0 + d), WallRole::Exterior),
                GWall::line((0.0, y0 + d), (0.0, y0), WallRole::Exterior),
            ];
            walls.extend((1..n).map(|k| GWall::line((x(k), y0), (x(k), y0 + d), WallRole::Partition)));
            let door = |wall: usize, along: f64| GOpening { wall, along, door: true, width: dw, height: dh, sill: 0.0, ty: None };
            let openings = if ground {
                (0..n).map(|k| door(0, x(k) + (w - dw) / 2.0)).collect()
            } else {
                (1..n).map(|k| door(3 + k, (d - dw) / 2.0)).collect()
            };
            let rooms = (0..n)
                .map(|k| GRoom {
                    name: match (k, ground) {
                        (0, _) => "Stair hall",
                        (_, true) => "Workshop",
                        _ => "Office",
                    }
                    .to_string(),
                    apartment: None,
                    point: (x(k) + w / 2.0, y0 + d / 2.0),
                    polygon: ring((x(k), y0), (x(k + 1), y0 + d)),
                })
                .collect();
            let below_top = i + 1 < storeys;
            out.push(GLayout {
                walls,
                openings,
                rooms,
                slab: if ground { vec![] } else { ring((0.0, y0), (s, y0 + d)) },
                slab_holes: if ground { vec![] } else { vec![ring(lo, hi)] },
                stairs: if below_top { vec![stair.clone()] } else { vec![] },
                height: h,
                ..GLayout::default()
            });
        }
        Ok(out)
    }

    /// A U stair of one storey `h` centred on `c`, and its plan box.
    fn stair(&self, c: (f64, f64), h: f64) -> Result<(GStair, (f64, f64), (f64, f64)), GenError> {
        let width = self.get("steel.hangar.stair_width");
        let mut g = GStair {
            origin: (0.0, 0.0),
            angle: std::f64::consts::FRAC_PI_2,
            shape: Shape::U { left: true, first: None, gap: 0.1 },
            width,
            max_riser: self.get("core.max_riser"),
            landing: width,
            height: h,
        };
        let st = g.def().layout().map_err(|e| GenError::Infeasible(format!("hangar annex stair: {e:?}")))?;
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        let rings = st.flights.iter().map(|f| f.ring(st.tread)).chain(st.landings.iter().map(|l| l.ring.clone()));
        for p in rings.flatten() {
            lo = (lo.0.min(p.x), lo.1.min(p.y));
            hi = (hi.0.max(p.x), hi.1.max(p.y));
        }
        let shift = (c.0 - (lo.0 + hi.0) / 2.0, c.1 - (lo.1 + hi.1) / 2.0);
        g.origin = shift;
        Ok((g, (lo.0 + shift.0, lo.1 + shift.1), (hi.0 + shift.0, hi.1 + shift.1)))
    }

    /// `steel.hangar.crane.count` cranes spread evenly along the runway pair, rail to rail
    /// across the span, standing on the rail tops.
    fn cranes(&self) -> Vec<HallCrane> {
        let n = self.get("steel.hangar.crane.count").round() as usize;
        let capacity = self.get("steel.hangar.crane.capacity");
        let reach = self.get("steel.hangar.corbel_reach");
        let half = match sections::find(RUNWAY) {
            Some(Section::I { h, .. }) => h / 2.0,
            _ => 0.0,
        };
        (0..n)
            .map(|i| HallCrane {
                at: (self.span / 2.0, self.len * (i as f64 + 0.5) / n as f64, self.crane_z() + half),
                span: self.span - 2.0 * reach,
                capacity,
            })
            .collect()
    }
}
