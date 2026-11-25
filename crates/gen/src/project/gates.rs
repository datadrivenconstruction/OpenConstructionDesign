//! Quality gates of a generated project, run on the builder output before anything reaches a
//! draft: the layout check of every storey plus program, door, daylight and stair rules.

use super::{GProject, PRole, ProjectSpec, RoomKind};
use crate::{GLayout, GOpening, GRoom};

const AREA_TOL: f64 = 0.02;
const ON_EDGE: f64 = 1e-6;
const MIN_TREAD: f64 = 0.25;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gates {
    /// A failure means the project must not be offered.
    pub failures: Vec<String>,
    /// Advice the person sees beside the draft (daylight short, stair comfort).
    pub warnings: Vec<String>,
    pub counts: Vec<(&'static str, usize)>,
}

impl Gates {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Opening area (doors when `door`, else windows) on the outline of room `r`.
fn open_area(l: &GLayout, centers: &[((f64, f64), &GOpening)], r: &GRoom, door: bool) -> f64 {
    centers
        .iter()
        .filter(|(_, o)| o.door == door)
        .filter(|(c, o)| on_outline(&r.polygon, *c) || on_arc_chord(&r.polygon, l, o))
        .map(|(_, o)| o.width * o.height)
        .sum()
}

fn center(l: &GLayout, o: &GOpening) -> (f64, f64) {
    l.walls[o.wall].at(o.along + o.width / 2.0)
}

/// The middle of opening `o` on an arc wall lies between the ends of an outline chord that
/// follows that arc (the outline runs in chords, the opening on the arc itself).
fn on_arc_chord(poly: &[(f64, f64)], l: &GLayout, o: &GOpening) -> bool {
    let w = &l.walls[o.wall];
    if w.bulge == 0.0 {
        return false;
    }
    let t = o.along + o.width / 2.0;
    let n = poly.len();
    (0..n).any(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if w.dist(a) > ON_EDGE || w.dist(b) > ON_EDGE {
            return false;
        }
        let (ta, tb) = (w.along(a), w.along(b));
        ta.min(tb) - ON_EDGE <= t && t <= ta.max(tb) + ON_EDGE
    })
}

fn on_outline(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = poly.len();
    (0..n).any(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
        let (qx, qy) = (a.0 + dx * t - p.0, a.1 + dy * t - p.1);
        (qx * qx + qy * qy).sqrt() < ON_EDGE
    })
}

fn area(poly: &[(f64, f64)]) -> f64 {
    let n = poly.len();
    ((0..n)
        .map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1)
        .sum::<f64>()
        / 2.0)
        .abs()
}

fn bbox(poly: &[(f64, f64)]) -> ((f64, f64), (f64, f64)) {
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    for p in poly {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    (lo, hi)
}

/// Rooms the spec asks on floor `k`, apartments counted room by room.
fn asked_rooms(spec: &ProjectSpec, k: usize) -> u32 {
    // a periodic storey (a refuge floor) lays its own rooms instead of the flats
    if spec.traits.flats && spec.periodic_at(k).is_none() {
        return spec
            .apartments
            .iter()
            .map(|a| {
                let own: u32 = a
                    .rooms
                    .iter()
                    .filter(|r| r.kind != super::RoomKind::Hall)
                    .map(|r| r.count)
                    .sum();
                own * a.count
            })
            .sum();
    }
    spec.storey_rooms(k).iter().map(|r| r.count).sum()
}

fn check_stairs(p: &GProject, spec: &ProjectSpec, g: &mut Gates) {
    for (k, s) in p.storeys.iter().enumerate() {
        for st in &s.layout.stairs {
            let lay = match st.def().layout() {
                Ok(x) => x,
                Err(e) => {
                    g.failures.push(format!("floor {}: stair {e:?}", k + 1));
                    continue;
                }
            };
            let rise = lay.risers as f64 * lay.riser;
            if (rise - s.height).abs() > 1e-3 {
                g.failures.push(format!(
                    "floor {}: stair rises {rise:.3} m, floor height {}",
                    k + 1,
                    s.height
                ));
            }
            if lay.riser > spec.max_riser + 1e-9 || lay.tread < MIN_TREAD {
                g.failures.push(format!(
                    "floor {}: riser {:.3} / tread {:.3} out of range",
                    k + 1,
                    lay.riser,
                    lay.tread
                ));
            }
            let step = 2.0 * lay.riser + lay.tread;
            if !(0.58..=0.66).contains(&step) {
                g.warnings
                    .push(format!("floor {}: 2R+T = {step:.3} m", k + 1));
            }
            let pts: Vec<(f64, f64)> = lay
                .flights
                .iter()
                .flat_map(|f| f.ring(lay.tread))
                .chain(lay.landings.iter().flat_map(|l| l.ring.clone()))
                .map(|v| (v.x, v.y))
                .collect();
            let (lo, hi) = bbox(&pts);
            // a block of wings has a stair room in each; the flight stays within one of them
            let mut halls = s
                .stair_room
                .into_iter()
                .chain((0..s.rooms.len()).filter(|&i| s.rooms[i].kind == RoomKind::Stair))
                .peekable();
            if halls.peek().is_none() {
                g.failures
                    .push(format!("floor {}: a stair without a stair room", k + 1));
                continue;
            }
            let within = halls.any(|i| {
                let (rlo, rhi) = bbox(&s.layout.rooms[i].polygon);
                lo.0 >= rlo.0 - 1e-6 && lo.1 >= rlo.1 - 1e-6 && hi.0 <= rhi.0 + 1e-6 && hi.1 <= rhi.1 + 1e-6
            });
            if !within {
                g.failures
                    .push(format!("floor {}: the stair leaves its room", k + 1));
            }
        }
    }
}

/// How to make the asked rooms fit, in numbers, so one revise_project lands: fewer rooms,
/// a wider outline at the same room depth, or the rooms spread over more storeys.
fn room_fix(spec: &ProjectSpec, l: &GLayout, met: u32, want: u32) -> String {
    if met == 0 || met > want {
        return String::new();
    }
    let (lo, hi) = bbox(&l.slab);
    let width = hi.0 - lo.0;
    let ratio = f64::from(want) / f64::from(met);
    let need = (width * ratio).ceil();
    let unit = if spec.traits.flats {
        "apartments[].count or apartment rooms"
    } else {
        "rooms[].count"
    };
    let mut fix = format!("; fix with revise_project: lower {unit} so {met} rooms remain");
    if need <= super::limits::WIDTH.max {
        fix.push_str(&format!(", or set footprint.width {need} m (built {width:.1} m)"));
    }
    let floors = (f64::from(spec.floors) * ratio).ceil() as u32;
    if floors <= spec.typology.max_floors() && !spec.traits.flats {
        fix.push_str(&format!(", or floors {floors} with fewer rooms per floor"));
    }
    fix
}

/// Runs every gate; the counts are what the project holds, for the reply and the log.
pub fn check_project(spec: &ProjectSpec, p: &GProject) -> Gates {
    let mut g = Gates::default();
    let (mut walls, mut doors, mut windows, mut rooms, mut stairs, mut shafts) = (0, 0, 0, 0, 0, 0);
    if p.storeys.len() != spec.floors as usize {
        g.failures.push(format!(
            "{} floors built, {} asked",
            p.storeys.len(),
            spec.floors
        ));
    }
    if p.basements.len() != spec.basements as usize {
        g.failures.push(format!(
            "{} basements built, {} asked",
            p.basements.len(),
            spec.basements
        ));
    }
    if let Err(e) = super::lift::check(spec, p) {
        g.failures.push(format!("lift: {e}"));
    }
    g.warnings.extend(p.lifts.issues.iter().map(|i| format!("lift: {i}")));
    g.warnings.extend(p.fire.head_issues.iter().map(|i| format!("fire: {i}")));
    let metric = |id: &str| p.metrics.iter().filter(|m| m.0 == id).map(|m| m.1).sum::<f64>();
    if metric("hvac_unserved") > 0.0 && metric("hvac_refused_stair_head") > 0.0 {
        g.warnings.push(format!(
            "hvac: no roof position for the air handling unit outside the stair heads and fire.stair_head.plant_clear ({} m); {} shaft position(s) refused for it",
            spec.rules.get("fire.stair_head.plant_clear"),
            metric("hvac_refused_stair_head")
        ));
    }
    let share = spec.rules.get("light.glazing_floor");
    let glazing_hard = spec.rules.hard("light.glazing_floor");
    let niche_max = spec.rules.get("light.kitchen_niche_max");
    for (k, s) in p.storeys.iter().enumerate() {
        let l = &s.layout;
        let fl = k + 1;
        if let Err(e) = crate::check::validate_layout(l) {
            g.failures.push(format!("floor {fl}: {e}"));
        }
        if l.slab.len() < 3 {
            g.failures.push(format!("floor {fl}: no floor slab"));
        }
        walls += l.walls.len();
        stairs += l.stairs.len();
        shafts += l.shafts.len();
        rooms += l.rooms.len();
        let centers: Vec<((f64, f64), &GOpening)> =
            l.openings.iter().map(|o| (center(l, o), o)).collect();
        doors += l.openings.iter().filter(|o| o.door).count();
        windows += l.openings.iter().filter(|o| !o.door).count();
        let mut met = 0;
        for (r, meta) in l.rooms.iter().zip(&s.rooms) {
            let a = area(&r.polygon);
            if let Some(want) = meta.asked {
                met += 1;
                // a room grown into spare band length may only be larger than asked
                let off = if meta.grown {
                    want - a
                } else {
                    (a - want).abs()
                };
                if off > AREA_TOL * want {
                    g.failures.push(format!(
                        "floor {fl}: room {} is {a:.2} m2, asked {want:.2} m2",
                        meta.number
                    ));
                }
            }
            let has = |door: bool| open_area(l, &centers, r, door);
            if has(true) == 0.0 {
                g.failures
                    .push(format!("floor {fl}: room {} has no door", meta.number));
            }
            if meta.kind.habitable() {
                let glazed = has(false);
                if glazed == 0.0 {
                    // a small kitchen may borrow the light of the living room it opens to
                    let borrowed = meta.kind == RoomKind::Kitchen
                        && a <= niche_max + 1e-9
                        && l.rooms.iter().zip(&s.rooms).any(|(r2, m2)| {
                            m2.kind == RoomKind::Living
                                && m2.owner == meta.owner
                                && open_area(l, &centers, r2, false) > 0.0
                                && centers.iter().any(|(c, o)| {
                                    o.door && on_outline(&r.polygon, *c) && on_outline(&r2.polygon, *c)
                                })
                        });
                    if borrowed {
                        g.warnings.push(format!(
                            "floor {fl}: light.kitchen_niche_max: kitchen {} ({a:.1} m2) has no window and is lit through its living room; it needs a mechanical extract",
                            meta.number
                        ));
                    } else if meta.kind == RoomKind::Kitchen {
                        g.failures.push(format!(
                            "floor {fl}: room {} is a kitchen without a facade window: a window on its own wall, or at most {niche_max} m2 opening to a lit living room (light.kitchen_niche_max)",
                            meta.number
                        ));
                    } else {
                        g.failures.push(format!("floor {fl}: room {} has no window", meta.number));
                    }
                } else if glazed < a * share - 1e-9 {
                    let mut line = format!(
                        "floor {fl}: light.glazing_floor: room {} glazing {glazed:.2} m2 < {share} of {a:.1} m2",
                        meta.number
                    );
                    if let Some(c) = spec.rules.cite("light.glazing_floor") {
                        line.push_str(&format!(" [norm {c}]"));
                    }
                    if glazing_hard {
                        g.failures.push(line);
                    } else {
                        g.warnings.push(line);
                    }
                }
            }
            if let PRole::Apartment(_) = meta.owner {
                if r.apartment.is_none() {
                    g.failures.push(format!(
                        "floor {fl}: room {} lost its apartment",
                        meta.number
                    ));
                }
            }
        }
        // a programme shared out between the wings asks each wing for its share
        let shared = p.metrics.iter().find(|m| m.0 == "program_shared").filter(|_| !spec.traits.flats);
        let want = match shared {
            Some(&(_, n)) => spec.storey_rooms(k).iter().map(|r| r.count.div_ceil(n as u32)).sum::<u32>() * p.copies,
            None => asked_rooms(spec, k) * p.copies,
        };
        if met != want {
            let fix = room_fix(spec, l, met, want);
            g.failures.push(format!(
                "floor {fl}: {met} program rooms, the spec asks {want}{fix}"
            ));
        }
    }
    for (b, s) in p.basements.iter().enumerate() {
        if let Err(e) = crate::check::validate_layout(&s.layout) {
            g.failures.push(format!("basement {}: {e}", b + 1));
        }
    }
    check_stairs(p, spec, &mut g);
    // plan checks: a hard rule fails the project, a soft one is advice naming its rule
    for f in super::nav::check(spec, p) {
        if f.hard {
            g.failures.push(f.line());
        } else {
            g.warnings.push(f.line());
        }
    }
    if p.roof.ring.len() < 3 {
        g.failures.push("no roof".to_string());
    }
    let (site_fail, site_warn) = super::site::audit(spec, p);
    g.failures.extend(site_fail);
    g.warnings.extend(site_warn);
    let (typ_fail, typ_warn) = super::typology::check::run(spec, p);
    g.failures.extend(typ_fail);
    g.warnings.extend(typ_warn);
    g.counts = vec![
        ("levels", p.storeys.len() + 1),
        ("walls", walls),
        ("doors", doors),
        ("windows", windows),
        ("rooms", rooms),
        ("slabs", p.storeys.len()),
        ("stairs", stairs),
        ("shafts", shafts),
        ("roofs", 1 + p.extra_roofs.len()),
        (
            "grids",
            p.storeys.first().map_or(0, |s| s.layout.grids.len()),
        ),
    ];
    g
}
