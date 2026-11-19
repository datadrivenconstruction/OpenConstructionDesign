//! Frame templates: the `lattice_tower_300` benchmark (four curved lattice legs merging into
//! one shaft, three platforms, base arches), a guyed triangular mast, a portal frame shed and a
//! multi-storey braced frame.

use super::lattice::{Deck, Flight, Frame, Guard, LiftRun, MemRole, Pattern, Secs};
use super::tower_size::{size_at, Loads};
use super::profile::Profile;
use crate::math::{v3, V3};

/// Parameters of the 300 m lattice tower; lengths in m.
#[derive(Clone, Debug, PartialEq)]
pub struct TowerParams {
    pub height: f64,
    /// Half-width of the leg footprint profile W(z) = a + b·exp(-z/k).
    pub w_a: f64,
    pub w_b: f64,
    pub w_k: f64,
    /// Leg box side at the ground.
    pub leg0: f64,
    /// Platform levels; the second one is where the legs merge into the shaft.
    pub platforms: [f64; 3],
    /// Ring width of the first platform floor (its centre stays open).
    pub ring1: f64,
    pub truss_depth: [f64; 3],
    /// Lattice panel height in the legs and in the shaft.
    pub panel_leg: f64,
    pub panel_shaft: f64,
    /// Floor beam spacing.
    pub floor_grid: f64,
    pub arch_rise: f64,
    pub arch_depth: f64,
    pub arch_panels: usize,
    pub pattern: Pattern,
    /// User outline `(z, half-width)`; replaces the exponential rule when set.
    pub profile: Option<Vec<(f64, f64)>>,
    /// Numbers of the member sizing rule; the tower is refused without them.
    pub loads: Option<Loads>,
    /// Lift runs up the core (0..2: ground to the second platform, then on to the top), their
    /// distance from the axis along the diagonal no plan brace takes, and the plan box (across,
    /// along) each needs with its mast.
    pub lifts: usize,
    pub lift_offset: f64,
    pub lift_box: (f64, f64),
}

impl Default for TowerParams {
    fn default() -> Self {
        TowerParams {
            height: 300.0,
            w_a: 3.0,
            w_b: 59.5,
            w_k: 98.0,
            leg0: 15.0,
            platforms: [57.6, 115.7, 276.1],
            ring1: 15.0,
            truss_depth: [7.0, 4.0, 2.0],
            panel_leg: 1.6,
            panel_shaft: 1.25,
            floor_grid: 2.0,
            arch_rise: 39.0,
            arch_depth: 2.5,
            arch_panels: 48,
            pattern: Pattern::X,
            profile: None,
            loads: Some(Loads::default()),
            lifts: 0,
            lift_offset: 3.0,
            lift_box: (1.6, 4.0),
        }
    }
}

impl TowerParams {
    pub fn outline(&self) -> Profile {
        match &self.profile {
            Some(p) => Profile::Spline(p.clone()),
            None => Profile::Exp { a: self.w_a, b: self.w_b, k: self.w_k },
        }
    }
    pub fn w(&self, z: f64) -> f64 {
        match &self.profile {
            Some(p) => super::profile::hermite(p, z),
            None => self.w_a + self.w_b * (-z / self.w_k).exp(),
        }
    }
    /// What a tower of legs and a shaft alone needs: no platforms, arches or lifts.
    pub fn check_shaft(&self) -> Result<(), String> {
        self.shaft_fault().map_or(Ok(()), |(_, e)| Err(e))
    }

    /// The first thing a bare lattice (legs and shaft) cannot be built with, and which part of
    /// the brief it is: `outline`, `merge` (legs into shaft, `platforms[1]`), `leg_box`,
    /// `panel` or `loads`. Loads are checked only when given: a bare tower sizes by the defaults.
    pub fn shaft_fault(&self) -> Option<(&'static str, String)> {
        if let Err(e) = self.outline().check() {
            return Some(("outline", e));
        }
        let merge = self.platforms[1];
        if !(0.0 < merge && merge < self.height) {
            return Some(("merge", "the legs must merge into the shaft below the top".into()));
        }
        if self.leg0 <= 0.0 || self.leg0 > self.w(0.0) || self.w(merge) < self.leg0 * 0.5 {
            return Some(("leg_box", "leg box must fit inside the footprint".into()));
        }
        if self.panel_leg < 0.3 || self.panel_shaft < 0.3 {
            return Some(("panel", "panel sizes below 0.3 m".into()));
        }
        match self.loads.map(|l| l.check()) {
            Some(Err(e)) => Some(("loads", e)),
            _ => None,
        }
    }

    pub fn check(&self) -> Result<(), String> {
        self.outline().check()?;
        let [p1, p2, p3] = self.platforms;
        if !(0.0 < p1 && p1 < p2 && p2 < p3 && p3 < self.height) {
            return Err("platforms must rise strictly below the top".into());
        }
        if self.leg0 <= 0.0 || self.leg0 > self.w(0.0) || self.w(p2) < self.leg0 * 0.5 {
            return Err("leg box must fit inside the footprint".into());
        }
        if self.panel_leg < 0.3 || self.panel_shaft < 0.3 || self.floor_grid < 0.5 {
            return Err("panel and grid sizes below 0.3 m / 0.5 m".into());
        }
        if self.arch_rise >= p1 - self.truss_depth[0] || self.arch_panels < 4 {
            return Err("arches must stay under the first platform truss and have 4+ panels".into());
        }
        let (bw, bd) = self.lift_box;
        if self.lifts > 2 || (self.lifts > 0 && (bw <= 0.0 || bd <= 0.0 || self.lift_offset < bw.hypot(bd) / 2.0)) {
            return Err("lifts: at most two, clear of the plan brace".into());
        }
        self.loads.ok_or("the tower needs the loads of its sizing rule")?.check()?;
        Ok(())
    }
}

/// Heights from z0 to z1 in equal steps not over `step`, through every key height between.
pub(super) fn levels(keys: &[f64], step: f64) -> Vec<f64> {
    let mut out = vec![keys[0]];
    for w in keys.windows(2) {
        let n = ((w[1] - w[0]) / step).ceil().max(1.0) as usize;
        for i in 1..=n {
            out.push(w[0] + (w[1] - w[0]) * i as f64 / n as f64);
        }
    }
    out
}

/// Sections by height (chord, diagonal, strut) from the wind and dead-load rule.
fn band(z: f64, p: &TowerParams) -> Secs<'static> {
    let s = size_at(p, &p.loads.unwrap_or_default(), z);
    Secs { chord: s.chord, diag: s.diag, strut: s.strut }
}

fn same(a: Secs<'_>, b: Secs<'_>) -> bool {
    (a.chord, a.diag, a.strut) == (b.chord, b.diag, b.strut)
}

/// Builds the tower frame.
pub fn lattice_tower_300(p: &TowerParams) -> Result<Frame, String> {
    p.check()?;
    let mut f = Frame::new();
    let [p1, p2, p3] = p.platforms;
    let [d1, d2, d3] = p.truss_depth;
    let wm = p.w(p2);
    let s_at = |z: f64| p.leg0 + (wm - p.leg0) * z / p2;
    let plan = "L 70x70x7";

    // Legs, ground to the second platform, in height bands so sections change by band.
    let zl = levels(&[0.0, p1 - d1, p1, p2 - d2, p2], p.panel_leg);
    for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
        let lv: Vec<[V3; 4]> = zl
            .iter()
            .map(|&z| {
                let (w, s) = (p.w(z), s_at(z));
                let c = |x: f64, y: f64| v3(sx * x, sy * y, z);
                [c(w, w), c(w - s, w), c(w - s, w - s), c(w, w - s)]
            })
            .collect();
        lattice_bands(&mut f, &lv, p, plan);
    }
    // Shaft from the merge to the top.
    let zs = levels(&[p2, p3 - d3, p3, p.height], p.panel_shaft);
    let lv: Vec<[V3; 4]> = zs
        .iter()
        .map(|&z| {
            let w = p.w(z);
            [v3(w, w, z), v3(-w, w, z), v3(-w, -w, z), v3(w, -w, z)]
        })
        .collect();
    lattice_bands(&mut f, &lv, p, plan);
    let top = lv.last().unwrap();
    f.add(top[0], top[2], plan, MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
    f.add(top[1], top[3], plan, MemRole::PlanBrace, v3(0.0, 0.0, 1.0));

    // Platforms: perimeter trusses on the four faces plus a floor grid.
    platform(&mut f, p, p1, d1, Some(p.ring1), "IPE 300");
    platform(&mut f, p, p2, d2, None, "IPE 300");
    platform(&mut f, p, p3, d3, None, "IPE 200");

    // Arches on the four faces between the legs, two planes tied by struts.
    for face in 0..4 {
        arch(&mut f, p, face);
    }

    // Stairs inside two opposite legs, ground to the merge, beside the plan brace that crosses
    // each leg box corner to corner; the floors they arrive at open over them.
    let mut holes: [Vec<[f64; 4]>; 3] = Default::default();
    for (sx, sy) in [(1.0, 1.0), (-1.0, -1.0)] {
        for w in levels(&[0.0, p1, p2], 3.6).windows(2) {
            let zm = (w[0] + w[1]) / 2.0;
            let s = s_at(zm);
            let c = p.w(zm) - s / 2.0;
            let fl = Flight::at((sx * (c + 0.18 * s), sy * (c - 0.18 * s)), w[0], w[1], -sx);
            for (i, pz) in [p1, p2].into_iter().enumerate() {
                if (w[1] - pz).abs() < 1e-6 {
                    holes[i].push(fl.plan);
                }
            }
            f.flights.push(fl);
        }
    }
    // lifts on the diagonal the plan braces leave free: the first through the open centre of
    // the first platform to the second, the next from there on the other side to the third
    let (bw, bd) = p.lift_box;
    let r = p.lift_offset / std::f64::consts::SQRT_2;
    // a run whose box does not fit inside the shaft at its top is left out (a slim outline)
    let reach = p.lift_offset * std::f64::consts::SQRT_2 / 2.0 + bw.hypot(bd) / 2.0;
    for (i, (at, z0, z1)) in [((r, -r), 0.0, p2), ((-r, r), p2, p3)].into_iter().take(p.lifts).enumerate() {
        if reach >= p.w(z1) - 0.3 {
            continue;
        }
        let plan = [at.0 - bw / 2.0, at.1 - bd / 2.0, at.0 + bw / 2.0, at.1 + bd / 2.0];
        holes[i + 1].push(plan);
        f.lifts.push(LiftRun { at, z0, z1, plan });
    }
    let [h1, h2, h3] = holes;
    f.decks = vec![
        Deck { z: p1, half: p.w(p1), open: p.w(p1) - p.ring1, holes: h1 },
        Deck { z: p2, half: wm, open: 0.0, holes: h2 },
        Deck { z: p3, half: p.w(p3), open: 0.0, holes: h3 },
    ];
    f.chords_are_columns = true;
    Ok(f)
}

/// One more platform on the tower `p` at height `z`, alone: perimeter trusses `depth` deep on
/// the four faces and the floor grid, corners on the tower's corner chords, and its deck. It is
/// added to a tower already built from the same parameters.
pub fn tower_platform(p: &TowerParams, z: f64, depth: f64) -> Result<Frame, String> {
    p.check()?;
    if !(depth > 0.2 && z - depth > 1.0 && z < p.height - 1.0) {
        return Err(format!("tower_platform: {z:.1} m must stand between the ground and the top ({:.1} m) with its truss", p.height));
    }
    if let Some(q) = p.platforms.iter().find(|q| (*q - z).abs() < depth + 1.0) {
        return Err(format!("tower_platform: the tower already has a platform at {q:.1} m"));
    }
    let mut f = Frame::new();
    platform(&mut f, p, z, depth, None, "IPE 200");
    let w = p.w(z);
    f.decks = vec![Deck { z, half: w, open: 0.0, holes: vec![] }];
    // edge railing round the deck
    let path = [(w, w), (-w, w), (-w, -w), (w, -w), (w, w)].iter().map(|&(x, y)| (x, y, z)).collect();
    f.guards.push(Guard { path, height: 1.1, post_spacing: 2.0, infill_spacing: 0.12 });
    Ok(f)
}

/// A bare lattice tower for a transmission line: four legs to the merge height
/// (`platforms[1]`), one shaft above it to the top, plan braces at the top. No platforms,
/// arches, stairs, lifts or decks.
pub fn pylon_tower(p: &TowerParams) -> Result<Frame, String> {
    p.check_shaft()?;
    let mut f = Frame::new();
    let p2 = p.platforms[1];
    let wm = p.w(p2);
    let s_at = |z: f64| p.leg0 + (wm - p.leg0) * z / p2;
    let plan = "L 70x70x7";
    let zl = levels(&[0.0, p2], p.panel_leg);
    for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
        let lv: Vec<[V3; 4]> = zl
            .iter()
            .map(|&z| {
                let (w, s) = (p.w(z), s_at(z));
                let c = |x: f64, y: f64| v3(sx * x, sy * y, z);
                [c(w, w), c(w - s, w), c(w - s, w - s), c(w, w - s)]
            })
            .collect();
        lattice_bands(&mut f, &lv, p, plan);
    }
    let zs = levels(&[p2, p.height], p.panel_shaft);
    let lv: Vec<[V3; 4]> = zs
        .iter()
        .map(|&z| {
            let w = p.w(z);
            [v3(w, w, z), v3(-w, w, z), v3(-w, -w, z), v3(w, -w, z)]
        })
        .collect();
    lattice_bands(&mut f, &lv, p, plan);
    let top = lv.last().unwrap();
    f.add(top[0], top[2], plan, MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
    f.add(top[1], top[3], plan, MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
    f.chords_are_columns = true;
    Ok(f)
}

fn lattice_bands(f: &mut Frame, lv: &[[V3; 4]], p: &TowerParams, plan: &str) {
    let mut i0 = 0;
    for i in 1..lv.len() {
        let z_mid = (lv[i - 1][0].z + lv[i][0].z) / 2.0;
        let here = band(z_mid, p);
        if i + 1 == lv.len() || !same(band((lv[i][0].z + lv[i + 1][0].z) / 2.0, p), here) {
            f.box_lattice(&lv[i0..=i], p.pattern, here, 3, plan);
            i0 = i;
        }
    }
}

fn platform(f: &mut Frame, p: &TowerParams, z: f64, depth: f64, ring: Option<f64>, beam: &str) {
    let w = p.w(z);
    let n = ((2.0 * w) / p.floor_grid).round().max(2.0) as usize;
    let xs: Vec<f64> = (0..=n).map(|i| -w + 2.0 * w * i as f64 / n as f64).collect();
    let secs = Secs { chord: "L 150x150x15", diag: "L 100x100x10", strut: "L 80x80x8" };
    // Four faces: top chord on the grid points, bottom chord `depth` lower.
    for face in 0..4 {
        let pt = |t: f64, zz: f64| match face {
            0 => v3(t, -w, zz),
            1 => v3(w, t, zz),
            2 => v3(-t, w, zz),
            _ => v3(-w, -t, zz),
        };
        let top: Vec<V3> = xs.iter().map(|&t| pt(t, z)).collect();
        let bot: Vec<V3> = xs.iter().map(|&t| pt(t, z - depth)).collect();
        f.truss(&top, &bot, Pattern::Warren, secs, MemRole::Girder);
    }
    let inside = |x: f64, y: f64| match ring {
        Some(r) => x.abs().max(y.abs()) >= w - r - 1e-6,
        None => true,
    };
    for (i, &x) in xs.iter().enumerate() {
        for j in 0..n {
            let (y0, y1) = (xs[j], xs[j + 1]);
            if i > 0 && i < n && inside(x, y0) && inside(x, y1) {
                f.add(v3(x, y0, z), v3(x, y1, z), beam, MemRole::Beam, v3(1.0, 0.0, 0.0));
            }
            if i > 0 && i < n && inside(y0, x) && inside(y1, x) {
                f.add(v3(y0, x, z), v3(y1, x, z), beam, MemRole::Beam, v3(0.0, 1.0, 0.0));
            }
        }
    }
}

fn arch(f: &mut Frame, p: &TowerParams, face: usize) {
    let (w0, s0) = (p.w(0.0), p.leg0);
    let half = w0 - s0;
    let n = p.arch_panels;
    let secs = Secs { chord: "L 120x120x12", diag: "L 80x80x8", strut: "L 70x70x7" };
    // In-face point: t across the face, z up, `inset` towards the tower axis.
    let at = |t: f64, z: f64, inset: f64| {
        let y = -(p.w(z) - inset);
        match face {
            0 => v3(t, y, z),
            1 => v3(-y, t, z),
            2 => v3(-t, -y, z),
            _ => v3(y, -t, z),
        }
    };
    let planes = [0.0, 1.5];
    let mut chords: Vec<(Vec<V3>, Vec<V3>)> = vec![];
    for &inset in &planes {
        let mut intr = vec![];
        let mut extr = vec![];
        for i in 0..=n {
            let th = std::f64::consts::PI * (1.0 - i as f64 / n as f64);
            let (x, z) = (half * th.cos(), p.arch_rise * th.sin());
            let dz = 0.3 + (p.arch_depth - 0.3) * th.sin();
            intr.push(at(x, z, inset));
            extr.push(at(x, z + dz, inset));
        }
        f.truss(&extr, &intr, Pattern::Warren, secs, MemRole::Arch);
        chords.push((intr, extr));
    }
    for i in 0..=n {
        f.add(chords[0].0[i], chords[1].0[i], secs.strut, MemRole::Strut, v3(0.0, 0.0, 1.0));
        if i % 2 == 0 {
            f.add(chords[0].1[i], chords[1].1[i], secs.strut, MemRole::Strut, v3(0.0, 0.0, 1.0));
        }
    }
}

/// Portal-frame shed: `bays` frames at `spacing`, `span`, eaves `height`, roof `pitch_deg`,
/// purlins every `purlin` m and X bracing in both end bays (roof and walls).
pub fn portal_frame(span: f64, height: f64, pitch_deg: f64, bays: usize, spacing: f64, purlin: f64) -> Result<Frame, String> {
    if !(span > 1.0 && height > 1.0 && (0.0..60.0).contains(&pitch_deg) && bays >= 1 && spacing > 1.0 && purlin > 0.3) {
        return Err("portal_frame: span, height, spacing > 1 m, pitch 0..60°, bays >= 1, purlin > 0.3 m".into());
    }
    let mut f = Frame::new();
    let ridge = height + span / 2.0 * pitch_deg.to_radians().tan();
    let np = ((span / 2.0) / purlin).ceil().max(1.0) as usize;
    let roof: Vec<(f64, f64)> = (0..=2 * np)
        .map(|i| {
            let x = span * i as f64 / (2 * np) as f64;
            (x, height + (ridge - height) * (1.0 - ((x - span / 2.0).abs() / (span / 2.0))))
        })
        .collect();
    for b in 0..=bays {
        let y = b as f64 * spacing;
        let n = v3(0.0, 1.0, 0.0);
        f.add(v3(0.0, y, 0.0), v3(0.0, y, height), "HEA 300", MemRole::Column, n);
        f.add(v3(span, y, 0.0), v3(span, y, height), "HEA 300", MemRole::Column, n);
        let pts: Vec<V3> = roof.iter().map(|&(x, z)| v3(x, y, z)).collect();
        f.polyline(&pts, "IPE 400", MemRole::Beam, n);
    }
    for b in 0..bays {
        let (y0, y1) = (b as f64 * spacing, (b + 1) as f64 * spacing);
        for &(x, z) in &roof {
            f.add(v3(x, y0, z), v3(x, y1, z), "UPN 160", MemRole::Beam, v3(0.0, 0.0, 1.0));
        }
        f.add(v3(0.0, y0, 0.0), v3(0.0, y1, 0.0), "L 80x80x8", MemRole::Strut, v3(0.0, 0.0, 1.0));
        f.add(v3(span, y0, 0.0), v3(span, y1, 0.0), "L 80x80x8", MemRole::Strut, v3(0.0, 0.0, 1.0));
        if b == 0 || b + 1 == bays {
            for &x in &[0.0, span] {
                f.add(v3(x, y0, 0.0), v3(x, y1, height), "CHS 88.9x5", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
                f.add(v3(x, y1, 0.0), v3(x, y0, height), "CHS 88.9x5", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
            }
            for w in roof.windows(2).step_by(2) {
                let (a, c) = (w[0], w[1]);
                f.add(v3(a.0, y0, a.1), v3(c.0, y1, c.1), "CHS 48.3x4", MemRole::Diagonal, v3(0.0, 0.0, 1.0));
            }
        }
    }
    Ok(f)
}

/// Multi-storey braced frame: `nx` x `ny` bays, `storeys` of `storey_h`, X bracing in the
/// middle bay of each outer frame line.
pub fn braced_frame(nx: usize, ny: usize, bay_x: f64, bay_y: f64, storeys: usize, storey_h: f64) -> Result<Frame, String> {
    if nx == 0 || ny == 0 || storeys == 0 || bay_x < 1.0 || bay_y < 1.0 || storey_h < 2.0 {
        return Err("braced_frame: bays and storeys >= 1, bays >= 1 m, storey >= 2 m".into());
    }
    let mut f = Frame::new();
    let p = |i: usize, j: usize, k: usize| v3(i as f64 * bay_x, j as f64 * bay_y, k as f64 * storey_h);
    for k in 0..storeys {
        let col = if storeys - k > 4 { "HEB 300" } else { "HEB 200" };
        for i in 0..=nx {
            for j in 0..=ny {
                f.add(p(i, j, k), p(i, j, k + 1), col, MemRole::Column, v3(1.0, 0.0, 0.0));
                if i < nx {
                    f.add(p(i, j, k + 1), p(i + 1, j, k + 1), "IPE 400", MemRole::Beam, v3(0.0, 1.0, 0.0));
                }
                if j < ny {
                    f.add(p(i, j, k + 1), p(i, j + 1, k + 1), "IPE 300", MemRole::Beam, v3(1.0, 0.0, 0.0));
                }
            }
        }
        let (mi, mj) = (nx / 2, ny / 2);
        for j in [0, ny] {
            f.add(p(mi, j, k), p(mi + 1, j, k + 1), "CHS 114.3x6", MemRole::Diagonal, v3(0.0, 1.0, 0.0));
            f.add(p(mi + 1, j, k), p(mi, j, k + 1), "CHS 114.3x6", MemRole::Diagonal, v3(0.0, 1.0, 0.0));
        }
        for i in [0, nx] {
            f.add(p(i, mj, k), p(i, mj + 1, k + 1), "CHS 114.3x6", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
            f.add(p(i, mj + 1, k), p(i, mj, k + 1), "CHS 114.3x6", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
        }
    }
    Ok(f)
}

/// A guyed lattice mast: three legs on a triangle of side `side`, panels about `panel` high
/// braced in `pat`, and `guys` levels of guy cables (the top one just under the head), each from
/// its leg straight out to a ground anchor `anchor` x the height from the axis.
pub fn guyed_mast(height: f64, side: f64, panel: f64, guys: usize, anchor: f64, pat: Pattern) -> Result<Frame, String> {
    if !(height >= 5.0 && (0.3..=6.0).contains(&side) && panel >= 0.3 && (1..=8).contains(&guys) && anchor >= 0.2) {
        return Err("guyed_mast: height >= 5 m, side 0.3..6 m, panel >= 0.3 m, 1..8 guy levels, anchor >= 0.2 x height".into());
    }
    let n = (height / panel).round().max(1.0) as usize;
    let r = side / 3f64.sqrt();
    let dir = |i: usize| {
        let a = std::f64::consts::FRAC_PI_2 + 2.0 * std::f64::consts::PI / 3.0 * i as f64;
        (a.cos(), a.sin())
    };
    let leg = |i: usize, k: usize| {
        let (c, s) = dir(i);
        v3(r * c, r * s, height * k as f64 / n as f64)
    };
    let s = Secs { chord: if height > 60.0 { "CHS 168.3x8" } else { "CHS 114.3x6" }, diag: "L 70x70x7", strut: "L 60x60x6" };
    let mut f = Frame::new();
    for i in 0..3 {
        let (c, sn) = dir(i);
        let line: Vec<V3> = (0..=n).map(|k| leg(i, k)).collect();
        f.polyline(&line, s.chord, MemRole::Chord, v3(c, sn, 0.0));
    }
    for k in 0..n {
        for i in 0..3 {
            let j = (i + 1) % 3;
            f.panel(leg(i, k), leg(i, k + 1), leg(j, k), leg(j, k + 1), pat, (k + i) % 2 == 1, s, true);
        }
    }
    for i in 0..3 {
        f.add(leg(i, 0), leg((i + 1) % 3, 0), s.strut, MemRole::Strut, v3(0.0, 0.0, 1.0));
    }
    for g in 1..=guys {
        // 19/20 of the way up at the top level, in exact integer steps
        let k = (((n * g * 19) as f64 / (20 * guys) as f64).round() as usize).clamp(1, n);
        for i in 0..3 {
            let (c, sn) = dir(i);
            let anchor_at = v3(anchor * height * c, anchor * height * sn, 0.0);
            f.add(leg(i, k), anchor_at, "RB 30", MemRole::Cable, v3(-sn, c, 0.0));
        }
    }
    Ok(f)
}
