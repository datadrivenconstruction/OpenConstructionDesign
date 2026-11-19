//! The plan as rectangles: the spine between the end rooms, each run laid along x from its
//! start (a host's full-depth part, its back part behind the front strip, the small rooms in
//! the strip at the spine), the leftover as a grown room or a spare one. Fixed parts are the
//! caller's. South lies between y = 0 and the spine, north between the spine and the full depth.
//! A side without depth has no runs.

use super::{grower, hosts, stores_left, Cap, Plan, Problem, Run, Side, DOOR_RUN, EPS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Item(usize),
    /// A spare room: its index in [`Shape::spares`].
    Spare(usize),
    Corridor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub part: Part,
}

impl Rect {
    pub(crate) fn holds(&self, p: (f64, f64)) -> bool {
        p.0 > self.x0 && p.0 < self.x1 && p.1 > self.y0 && p.1 < self.y1
    }

    pub(crate) fn ring(&self) -> Vec<(f64, f64)> {
        vec![(self.x0, self.y0), (self.x1, self.y0), (self.x1, self.y1), (self.x0, self.y1)]
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Shape {
    pub rects: Vec<Rect>,
    /// Items that took leftover length.
    pub grown: Vec<usize>,
    /// Spare rooms: `true` for a store too short for a window.
    pub spares: Vec<bool>,
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64, part: Part) -> Rect {
    Rect { x0, y0, x1, y1, part }
}

/// `y` from and to of the strip `a..b` metres back from the spine on `side`.
fn depth_span(p: &Problem, side: Side, a: f64, b: f64) -> (f64, f64) {
    match side {
        Side::South => (p.depth[0] - b, p.depth[0] - a),
        Side::North => {
            let front = p.depth[0] + p.corridor;
            (front + a, front + b)
        }
    }
}

pub(crate) fn lay(p: &Problem, plan: &Plan) -> Shape {
    let mut s = Shape::default();
    let (xa, xb) = plan.spine;
    let full = p.full();
    if let Cap::Item(i) = plan.caps[0] {
        s.rects.push(rect(0.0, 0.0, xa, full, Part::Item(i)));
    }
    if let Cap::Item(i) = plan.caps[1] {
        s.rects.push(rect(xb, 0.0, p.w, full, Part::Item(i)));
    }
    s.rects.push(rect(xa, p.depth[0], xb, p.depth[0] + p.corridor, Part::Corridor));
    for run in &plan.runs {
        lay_run(p, run, &mut s);
    }
    s
}

fn lay_run(p: &Problem, run: &Run, s: &mut Shape) {
    let side = run.side;
    let depth = p.depth[side.at()];
    let (y0, y1) = depth_span(p, side, 0.0, depth);
    let groups = hosts(p, run).unwrap_or_default();
    let used: f64 = run.items.iter().map(|&i| p.len(i, side)).sum();
    let left = (run.len() - used).max(0.0);
    let grow = if left > EPS && left < p.min_w - EPS && !stores_left(p, run, left) { grower(p, run) } else { None };
    let first = s.rects.len();
    let mut x = run.x0;
    for (k, &i) in run.items.iter().enumerate() {
        if p.tucked(i, side) {
            continue;
        }
        let mut l = p.len(i, side);
        if grow == Some(i) {
            l += left;
            s.grown.push(i);
        }
        match groups.iter().find(|g| g.0 == k) {
            Some((_, ts, dt)) => {
                let dt = *dt;
                let tucked: f64 = ts.iter().map(|&t| p.items[t].area).sum();
                let slot = l + tucked / depth;
                let front = slot - tucked / dt;
                s.rects.push(rect(x, y0, x + front, y1, Part::Item(i)));
                let (b0, b1) = depth_span(p, side, dt, depth);
                s.rects.push(rect(x + front, b0, x + slot, b1, Part::Item(i)));
                let (f0, f1) = depth_span(p, side, 0.0, dt);
                let mut xt = x + front;
                for &t in ts {
                    let wt = p.items[t].area / dt;
                    s.rects.push(rect(xt, f0, xt + wt, f1, Part::Item(t)));
                    xt += wt;
                }
                x += slot;
            }
            None => {
                s.rects.push(rect(x, y0, x + l, y1, Part::Item(i)));
                x += l;
            }
        }
    }
    if left > EPS && grow.is_none() && left >= DOOR_RUN - EPS {
        s.spares.push(left < p.min_w - EPS || p.blind[side.at()]);
        s.rects.push(rect(x, y0, run.x1, y1, Part::Spare(s.spares.len() - 1)));
        x = run.x1;
    }
    // the run ends exactly where the next part starts
    for r in &mut s.rects[first..] {
        if (r.x1 - x).abs() < 1e-6 {
            r.x1 = run.x1;
        }
    }
}
