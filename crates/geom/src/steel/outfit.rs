//! Outfit of the lattice tower: platform railings, stairs and lift guides inside the legs, the
//! top cabin with its mast, and the masonry piers under the leg feet. Every size comes from the
//! rule set (`steel.out.*`); every added member ties into the tower's nodes, so the frame stays
//! one connected structure.

use super::hier::Decision;
use super::lattice::{Frame, MemRole, Pier};
use super::tower::{levels, TowerParams};
use crate::math::{v3, V3};

#[derive(Clone, Debug, PartialEq)]
pub struct OutfitRules {
    pub rail_height: f64,
    pub stair_width: f64,
    pub stair_riser: f64,
    /// Most risers in one flight; a landing follows.
    pub stair_flight: f64,
    pub lift_inset: f64,
    pub cabin_size: f64,
    pub cabin_height: f64,
    pub roof_rise: f64,
    pub mast_height: f64,
    pub pier_margin: f64,
    pub pier_depth: f64,
    pub pier_batter: f64,
}

impl Default for OutfitRules {
    fn default() -> Self {
        Self { rail_height: 1.1, stair_width: 0.9, stair_riser: 0.19, stair_flight: 16.0, lift_inset: 0.45, cabin_size: 4.0, cabin_height: 3.0, roof_rise: 1.5, mast_height: 24.0, pier_margin: 1.5, pier_depth: 6.0, pier_batter: 0.1 }
    }
}

impl OutfitRules {
    pub fn check(&self) -> Result<(), String> {
        let pos = [self.rail_height, self.stair_width, self.stair_riser, self.stair_flight, self.lift_inset, self.cabin_size, self.cabin_height, self.roof_rise, self.mast_height, self.pier_margin, self.pier_depth];
        if pos.iter().any(|v| !(*v > 0.0 && v.is_finite())) || !(0.0..1.0).contains(&self.pier_batter) {
            return Err("outfit rules must be positive numbers".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outfit {
    pub piers: Vec<Pier>,
    pub rail_posts: usize,
    pub stair_treads: usize,
    pub stair_landings: usize,
    pub lift_rail_members: usize,
    pub cabin_members: usize,
    pub mast_members: usize,
    pub decisions: Vec<Decision>,
}

fn fmt(x: f64) -> String {
    format!("{x:.2}")
}

fn dec(rule: &'static str, value: f64, key: &'static str, args: Vec<(&'static str, String)>, count: usize) -> Decision {
    Decision { rule, value, key, args, count }
}

/// Adds railings, stairs, lift guides, cabin and mast to the tower frame built from `p`.
pub fn outfit(f: &mut Frame, p: &TowerParams, r: &OutfitRules) -> Result<Outfit, String> {
    r.check()?;
    let mut o = Outfit::default();
    let n0 = f.members.len();
    let up = v3(0.0, 0.0, 1.0);

    // Railings on the three platforms, posts on the perimeter nodes of the top chord.
    for &z in &p.platforms {
        let w = p.w(z);
        let n = ((2.0 * w) / p.floor_grid).round().max(2.0) as usize;
        let mut ring: Vec<V3> = vec![];
        for face in 0..4 {
            for i in 0..n {
                let t = -w + 2.0 * w * i as f64 / n as f64;
                ring.push(match face {
                    0 => v3(t, -w, z),
                    1 => v3(w, t, z),
                    2 => v3(-t, w, z),
                    _ => v3(-w, -t, z),
                });
            }
        }
        for a in &ring {
            // the post is split at the mid rail so that the rail ties into a node
            let mid = f.add(*a, v3(a.x, a.y, z + 0.5 * r.rail_height), "L 60x60x6", MemRole::Column, v3(1.0, 0.0, 0.0));
            f.add(v3(a.x, a.y, z + 0.5 * r.rail_height), v3(a.x, a.y, z + r.rail_height), "L 60x60x6", MemRole::Column, v3(1.0, 0.0, 0.0));
            if mid.is_some() {
                o.rail_posts += 1;
            }
        }
        for k in 0..ring.len() {
            let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
            let d = (b - a).len();
            if d < 1e-3 {
                continue;
            }
            for h in [r.rail_height, 0.5 * r.rail_height] {
                f.add(v3(a.x, a.y, z + h), v3(b.x, b.y, z + h), "L 50x50x5", MemRole::Beam, up);
            }
        }
    }
    o.decisions.push(dec("steel.out.rail_height", r.rail_height, "steel_rails", vec![("h", fmt(r.rail_height)), ("posts", o.rail_posts.to_string())], o.rail_posts));

    // Stairs and a lift guide inside each leg, up to where the legs merge; a leg that already
    // carries the frame's stair flights (stair elements in the model) gets the guide only.
    let [p1, p2, _] = p.platforms;
    let [d1, d2, _] = p.truss_depth;
    let zl = levels(&[0.0, p1 - d1, p1, p2 - d2, p2], p.panel_leg);
    let wm = p.w(p2);
    let mut guide_nodes = 0;
    for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
        let leg: Vec<(V3, [V3; 4])> = zl
            .iter()
            .map(|&z| {
                let (w, s) = (p.w(z), p.leg0 + (wm - p.leg0) * z / p2);
                let c = |x: f64, y: f64| v3(sx * x, sy * y, z);
                (c(w - s / 2.0, w - s / 2.0), [c(w, w), c(w - s, w), c(w - s, w - s), c(w, w - s)])
            })
            .collect();
        // u across the leg's diagonal, d towards the tower axis.
        let u = v3(-sy, sx, 0.0) * (1.0 / 2f64.sqrt());
        let d = v3(-sx, -sy, 0.0) * (1.0 / 2f64.sqrt());
        let mut prev: Option<(V3, V3, V3)> = None;
        let mut risers = 0usize;
        let flights = f.flights.iter().any(|fl| (fl.plan[0] + fl.plan[2]) * sx > 0.0 && (fl.plan[1] + fl.plan[3]) * sy > 0.0);
        for (c, corners) in &leg {
            let s = (corners[0] - corners[2]).len() / 2f64.sqrt();
            let half = (r.stair_width / 2.0).min(s * 0.3);
            let (pa, pb, pr) = (*c + u * half, *c - u * half, *c + d * (r.lift_inset.min(s * 0.3)));
            let near = |q: V3| *corners.iter().max_by(|a, b| (**a - *c).dot(q - *c).total_cmp(&(**b - *c).dot(q - *c))).unwrap();
            for (q, tie) in [(pa, near(pa)), (pb, near(pb)), (pr, corners[2])] {
                if !flights || q == pr {
                    f.add(q, tie, "L 50x50x5", MemRole::Strut, up);
                }
            }
            if !flights && f.add(pa, pb, "L 80x80x8", MemRole::Beam, d).is_some() {
                o.stair_treads += 1;
            }
            if let Some((qa, qb, qr)) = prev {
                // one riser per `stair_riser` of height: stringer nodes and a tread at each
                let n = if flights { 0 } else { (((pa.z - qa.z) / r.stair_riser).ceil() as usize).max(1) };
                let at = |x: V3, y: V3, i: usize| x + (y - x) * (i as f64 / n as f64);
                for i in 0..n {
                    let (a0, a1, b0, b1) = (at(qa, pa, i), at(qa, pa, i + 1), at(qb, pb, i), at(qb, pb, i + 1));
                    f.add(a0, a1, "UPN 160", MemRole::Beam, d);
                    f.add(b0, b1, "UPN 160", MemRole::Beam, d);
                    if i > 0 && f.add(a0, b0, "L 80x80x8", MemRole::Beam, d).is_some() {
                        o.stair_treads += 1;
                    }
                    if i > 0 {
                        risers += 1;
                        if risers as f64 >= r.stair_flight {
                            // landing: a square platform on the stringers, then the next flight
                            risers = 0;
                            let ln = r.stair_width.min(s * 0.3);
                            let (la, lb) = (a0 + d * ln, b0 + d * ln);
                            f.add(a0, la, "L 80x80x8", MemRole::Beam, d);
                            f.add(b0, lb, "L 80x80x8", MemRole::Beam, d);
                            if f.add(la, lb, "L 80x80x8", MemRole::Beam, d).is_some() {
                                o.stair_landings += 1;
                            }
                        }
                    }
                }
                if f.add(qr, pr, "CHS 114.3x6", MemRole::Column, up).is_some() {
                    o.lift_rail_members += 1;
                }
            }
            guide_nodes += 1;
            prev = Some((pa, pb, pr));
        }
    }
    o.decisions.push(dec("steel.out.stair_width", r.stair_width, "steel_stairs", vec![("w", fmt(r.stair_width)), ("treads", o.stair_treads.to_string()), ("flight", format!("{}", r.stair_flight)), ("landings", o.stair_landings.to_string())], o.stair_treads));
    o.decisions.push(dec("steel.out.lift_inset", r.lift_inset, "steel_lift", vec![("inset", fmt(r.lift_inset)), ("n", guide_nodes.to_string())], o.lift_rail_members));

    // Cabin on the shaft top, hip roof and mast.
    let h = p.height;
    let wt = p.w(h);
    let c = (r.cabin_size / 2.0).min(wt * 0.8);
    let top = |sx: f64, sy: f64, z: f64, k: f64| v3(sx * k, sy * k, z);
    let signs = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)];
    let m0 = f.members.len();
    for &(sx, sy) in &signs {
        f.add(top(sx, sy, h, wt), top(sx, sy, h, c), "L 80x80x8", MemRole::Beam, up);
    }
    let zt = h + r.cabin_height;
    for i in 0..4 {
        let (a, b) = (signs[i], signs[(i + 1) % 4]);
        f.add(top(a.0, a.1, h, c), top(a.0, a.1, zt, c), "L 80x80x8", MemRole::Column, v3(1.0, 0.0, 0.0));
        f.add(top(a.0, a.1, h, c), top(b.0, b.1, h, c), "L 80x80x8", MemRole::Beam, up);
        f.add(top(a.0, a.1, zt, c), top(b.0, b.1, zt, c), "L 80x80x8", MemRole::Beam, up);
        f.add(top(a.0, a.1, h, c), top(b.0, b.1, zt, c), "L 60x60x6", MemRole::Diagonal, up);
        f.add(top(b.0, b.1, h, c), top(a.0, a.1, zt, c), "L 60x60x6", MemRole::Diagonal, up);
    }
    let apex_z = zt + r.roof_rise;
    for &(sx, sy) in &signs {
        f.add(top(sx, sy, zt, c), v3(0.0, 0.0, apex_z), "L 80x80x8", MemRole::Strut, up);
    }
    o.cabin_members = f.members.len() - m0;
    let m1 = f.members.len();
    let segs = ((r.mast_height / 8.0).ceil() as usize).max(1);
    for i in 0..segs {
        let (z0, z1) = (apex_z + r.mast_height * i as f64 / segs as f64, apex_z + r.mast_height * (i + 1) as f64 / segs as f64);
        let sec = if i * 3 < segs { "CHS 168.3x8" } else { "CHS 114.3x6" };
        f.add(v3(0.0, 0.0, z0), v3(0.0, 0.0, z1), sec, MemRole::Column, v3(1.0, 0.0, 0.0));
    }
    o.mast_members = f.members.len() - m1;
    o.decisions.push(dec("steel.out.cabin_size", r.cabin_size, "steel_cabin", vec![("size", fmt(2.0 * c)), ("h", fmt(r.cabin_height)), ("mast", fmt(r.mast_height))], o.cabin_members + o.mast_members));

    // Masonry piers under the leg feet, widening downwards.
    let s0 = p.leg0;
    for &(sx, sy) in &signs {
        let w = p.w(0.0);
        let half_top = s0 / 2.0 + r.pier_margin;
        o.piers.push(Pier { x: sx * (w - s0 / 2.0), y: sy * (w - s0 / 2.0), z_top: 0.0, z_foot: -r.pier_depth, half_top, half_foot: half_top + r.pier_batter * r.pier_depth });
    }
    o.decisions.push(dec("steel.out.pier_margin", r.pier_margin, "steel_piers", vec![("margin", fmt(r.pier_margin)), ("depth", fmt(r.pier_depth)), ("batter", fmt(r.pier_batter))], o.piers.len()));
    f.piers.clone_from(&o.piers);
    debug_assert!(f.members.len() >= n0);
    Ok(o)
}
