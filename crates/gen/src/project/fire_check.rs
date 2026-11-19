//! Hazard classes of the sprinkler design, the head grid kept off the columns, and the check of
//! a finished fire protection design: every protected room covered by heads of its class, no
//! head further from its neighbour than the class spacing nor serving more than the class area,
//! heads clear of the columns and under the ceiling, pipes clear of the columns, and a
//! standpipe with a landing valve on every storey within hose reach. Every number is a rule.

use super::rules::{self, rule};
use super::sprinkler::{bbox, coverage_by, grid_for, inside, P2, SAMPLES};
use super::{svc, GProject, RoomKind};
use crate::GColumn;

/// The hazard classes, lightest first: the options of the `fire.hazard.<room kind>` rules.
pub const CLASSES: [&str; 3] = ["light", "ordinary", "high"];
/// The class the base rules `fire.head_spacing`, `fire.head_area`, `fire.head_wall_max` and
/// `fire.density` describe.
pub(super) const ORDINARY: usize = 1;

const HEAD: &str = "Sprinkler Head";

/// What burns in a room sets how closely its heads stand and how much water they give.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hazard {
    /// Index into [`CLASSES`].
    pub class: usize,
    /// Greatest distance between two heads, m.
    pub spacing: f64,
    /// Greatest floor area one head serves, m2.
    pub area: f64,
    /// Greatest distance from a head to a wall, m.
    pub wall: f64,
    /// Design density, mm/min: litres per minute on each square metre.
    pub density: f64,
}

impl Hazard {
    /// The class at index `i` of [`CLASSES`], read from its rules.
    pub fn class(i: usize) -> Hazard {
        let i = i.min(CLASSES.len() - 1);
        let id = |own: &str, base: &str| {
            if i == ORDINARY {
                base.to_string()
            } else {
                format!("fire.{}.{own}", CLASSES[i])
            }
        };
        Hazard {
            class: i,
            spacing: rule(&id("spacing", "fire.head_spacing")),
            area: rule(&id("head_area", "fire.head_area")),
            wall: rule(&id("wall_max", "fire.head_wall_max")),
            density: rule(&id("density", "fire.density")),
        }
    }

    /// Flow of one head, l/min: the density over the area it serves.
    pub fn flow(&self) -> f64 {
        self.density * self.area
    }

    pub fn name(&self) -> &'static str {
        CLASSES[self.class]
    }
}

/// The hazard class the rule of a room kind names; ordinary for a kind without a rule.
pub fn hazard_of(kind: RoomKind) -> Hazard {
    let id = format!("fire.hazard.{}", svc::kind_name(kind));
    Hazard::class(rules::def(&id).map_or(ORDINARY, |_| rule(&id) as usize))
}

/// The share of a room's floor a set of heads protects, and the first point left out: a head
/// protects the points within the class wall distance along both plan axes.
pub(super) fn coverage_sq(poly: &[P2], heads: &[P2], hz: &Hazard) -> (f64, Option<P2>) {
    let reach = hz.wall + 1e-6;
    let step = hz.spacing / SAMPLES as f64;
    coverage_by(poly, step, |q| heads.iter().any(|h| (h.0 - q.0).abs() <= reach && (h.1 - q.1).abs() <= reach))
}

/// Plan box of a column grown by `pad`.
fn column_box(c: &GColumn, pad: f64) -> [f64; 4] {
    let (cs, sn) = (c.rotation.cos().abs(), c.rotation.sin().abs());
    let hx = (cs * c.section.0 + sn * c.section.1) / 2.0 + pad;
    let hy = (sn * c.section.0 + cs * c.section.1) / 2.0 + pad;
    [c.at.0 - hx, c.at.1 - hy, c.at.0 + hx, c.at.1 + hy]
}

fn in_box(b: &[f64; 4], q: P2) -> bool {
    q.0 > b[0] && q.0 < b[2] && q.1 > b[1] && q.1 < b[3]
}

/// The columns as boxes grown by the clearance rule: the one rule the layout keeps and the
/// check measures.
fn column_boxes(cols: &[GColumn]) -> Vec<[f64; 4]> {
    let pad = rule("fire.column_clear");
    cols.iter().map(|c| column_box(c, pad)).collect()
}

/// The column boxes that reach into a room's extent.
fn room_columns(poly: &[P2], cols: &[GColumn]) -> Vec<[f64; 4]> {
    let b = bbox(poly);
    column_boxes(cols).into_iter().filter(|c| c[0] < b[2] && c[2] > b[0] && c[1] < b[3] && c[3] > b[1]).collect()
}

/// Lines of one axis of the head grid, `n` at least, kept off the column boxes: a line that
/// meets a box (`hit` gives the box's extent across the line) moves past it as far as the
/// class keeps coverage and spacing, else the lines grow denser. With `first`, only the first
/// line is kept off: the line of the room trunk.
fn shift_lines(lo: f64, len: f64, n: usize, hz: &Hazard, first: bool, hit: impl Fn(f64) -> Option<(f64, f64)>) -> Vec<f64> {
    let centres = |n: usize| (0..n).map(|i| lo + len * (i as f64 + 0.5) / n as f64).collect::<Vec<f64>>();
    for m in n..=n * 2 {
        let pitch = len / m as f64;
        let slack = (hz.wall - pitch / 2.0).min((hz.spacing - pitch) / 2.0).max(0.0);
        let mut lines = centres(m);
        let upto = if first { 1 } else { m };
        let ok = lines.iter_mut().take(upto).all(|l| match hit(*l) {
            None => true,
            Some((a, b)) => {
                let best = [a, b]
                    .into_iter()
                    .filter(|x| (x - *l).abs() <= slack && hit(*x).is_none())
                    .min_by(|x, y| (x - *l).abs().total_cmp(&(y - *l).abs()));
                best.map(|x| *l = x).is_some()
            }
        });
        if ok {
            return lines;
        }
    }
    centres(n)
}

/// The head grid of a room in its hazard class with the branch rows and the trunk kept off the
/// columns standing in the room.
pub(super) fn grid_clear(poly: &[P2], hz: &Hazard, cols: &[GColumn]) -> (Vec<f64>, Vec<f64>) {
    let b = bbox(poly);
    let (xs, ys) = grid_for(&b, hz);
    let boxes = room_columns(poly, cols);
    if boxes.is_empty() {
        return (xs, ys);
    }
    let row_hit = |y: f64| boxes.iter().find(|c| y > c[1] && y < c[3]).map(|c| (c[1], c[3]));
    let ys = shift_lines(b[1], b[3] - b[1], ys.len(), hz, false, row_hit);
    let trunk_hit = |x: f64| boxes.iter().find(|c| x > c[0] && x < c[2]).map(|c| (c[0], c[2]));
    let xs = shift_lines(b[0], b[2] - b[0], xs.len(), hz, true, trunk_hit);
    (xs, ys)
}

/// A head at `q` when it stands clear of every column by the clearance rule.
pub(super) fn clear_of_columns(cols: &[GColumn], q: P2) -> Option<P2> {
    column_boxes(cols).iter().all(|b| !in_box(b, q)).then_some(q)
}

/// Every fault of the fire protection of a project, empty when it is sound.
pub fn fire_check(p: &GProject) -> Vec<String> {
    let f = &p.fire;
    let mut out: Vec<String> = f.issues.iter().map(|i| format!("sprinklers: {i}")).collect();
    if f.heads > 0 {
        for (k, st) in p.storeys.iter().enumerate() {
            heads_of_storey(p, k, &mut out);
            let boxes: Vec<[f64; 4]> = st.layout.columns.iter().map(|c| column_box(c, svc::gap())).collect();
            for r in f.runs.iter().filter(|r| r.run.storey == k) {
                for w in r.run.points.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let crosses = |c: &[f64; 4]| {
                        a.0.min(b.0) < c[2] && a.0.max(b.0) > c[0] && a.1.min(b.1) < c[3] && a.1.max(b.1) > c[1]
                    };
                    if let Some(c) = boxes.iter().find(|c| crosses(c)) {
                        out.push(format!("storey {k}: a sprinkler pipe at {:?} crosses the column at {:?}", (a.0, a.1), (c[0], c[1])));
                    }
                }
            }
        }
    }
    let top = p.storeys.last().map_or(0.0, |t| t.elevation);
    let stairs = p.storeys.iter().any(|s| s.rooms.iter().any(|r| r.kind == RoomKind::Stair));
    if stairs && top >= rule("fire.standpipe_height") && f.standpipes.is_empty() {
        out.push(format!("no standpipe in a building {top:.1} m high"));
    }
    for sp in &f.standpipes {
        for k in 0..p.storeys.len() {
            if !sp.valves.iter().any(|v| v.0 == k) {
                out.push(format!("{}: no landing valve on storey {k}", sp.name));
            }
        }
        let refs: Vec<&super::GRun> = sp.runs.iter().map(|r| &r.run).collect();
        let ports: Vec<ddc_geom::math::V3> = sp
            .links
            .iter()
            .filter_map(|l| svc::port_world(p, l.storey, l.fixture, l.connector))
            .map(|c| ddc_geom::math::v3(c.at.0, c.at.1, c.at.2))
            .collect();
        out.extend(svc::net_faults(&refs, &ports).into_iter().map(|x| format!("{}: {x}", sp.name)));
    }
    for g in &f.reach_gaps {
        out.push(format!("storey {} room {}: {:?} is beyond the hose reach of every landing valve", g.storey, g.room, g.at));
    }
    out
}

/// Coverage, spacing, area per head, column clearance and ceiling of the heads of storey `k`.
fn heads_of_storey(p: &GProject, k: usize, out: &mut Vec<String>) {
    let st = &p.storeys[k];
    let ceiling = svc::ceiling(p, k);
    let heads: Vec<&crate::GFixture> = st.layout.fixtures.iter().filter(|x| x.family == HEAD).collect();
    for h in &heads {
        if h.z > ceiling + 1e-6 {
            out.push(format!("storey {k}: the head at {:?} hangs above the ceiling", h.at));
        }
        if clear_of_columns(&st.layout.columns, h.at).is_none() {
            out.push(format!("storey {k}: the head at {:?} stands against a column", h.at));
        }
    }
    for (i, (pr, gr)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
        if matches!(pr.kind, RoomKind::Stair | RoomKind::Lift) || gr.polygon.len() < 3 {
            continue;
        }
        let hz = hazard_of(pr.kind);
        let hs: Vec<P2> = heads.iter().map(|h| h.at).filter(|q| inside(&gr.polygon, *q)).collect();
        let what = format!("storey {k} room {i} ({}, {} hazard)", svc::kind_name(pr.kind), hz.name());
        if hs.is_empty() {
            out.push(format!("{what}: no sprinkler head"));
            continue;
        }
        let (share, gap) = coverage_sq(&gr.polygon, &hs, &hz);
        if let Some(at) = gap {
            out.push(format!("{what}: {:.1} % covered, {at:?} out of reach", share * 100.0));
        }
        let per = super::massing::ring_area(&gr.polygon) / hs.len() as f64;
        if per > hz.area + 1e-6 {
            out.push(format!("{what}: {per:.2} m2 per head over the {} m2 of the class", hz.area));
        }
        for h in &hs {
            let near = hs.iter().filter(|o| *o != h).map(|o| (o.0 - h.0).hypot(o.1 - h.1)).fold(f64::MAX, f64::min);
            if hs.len() > 1 && near > hz.spacing + 1e-6 {
                out.push(format!("{what}: the head at {h:?} is {near:.2} m from the next, over {}", hz.spacing));
            }
        }
    }
}
