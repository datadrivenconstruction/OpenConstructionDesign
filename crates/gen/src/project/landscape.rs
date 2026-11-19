//! Landscape of a generated site: a plot round the building, a hedge along its boundary, beds
//! with shrubs outside the paving, lawn on the open ground, paths from every entrance to the
//! street, and tree rows along the street and the paths. Every size, count and type comes from
//! `site.*` rules and `data/landscape.json`; the library families are Hedge, Shrub and Lawn.
//!
//! Everything is laid in a frame in which the street is the south side (`site.street_side`), then
//! turned back. Trees keep a setback from the building and clear of the paths (`tree_ok`).

use super::rules::{def, rule};
use crate::check::{dist_to_seg, inside};
use crate::{GFixture, GLayout, GenError};
use serde_json::Value;
use std::sync::OnceLock;

type P = (f64, f64);

const DATA: &str = include_str!("../../data/landscape.json");
/// Gap between the paving and the first tree ring, m (`site()` kept it before the landscape).
const TREE_GAP: f64 = 3.0;
/// Free ground between the tree ring and the boundary, m: a row of trees fits in it.
const TREE_APRON: f64 = 6.0;

fn data() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(DATA).expect("data/landscape.json is not JSON"))
}

fn num(path: &[&str]) -> f64 {
    let mut v = data();
    for k in path {
        v = &v[*k];
    }
    v.as_f64().unwrap_or_else(|| panic!("data/landscape.json: {path:?} is not a number"))
}

fn text(path: &[&str]) -> &'static str {
    let mut v = data();
    for k in path {
        v = &v[*k];
    }
    v.as_str().unwrap_or_else(|| panic!("data/landscape.json: {path:?} is not a text"))
}

fn option(id: &str) -> String {
    def(id).and_then(|d| d.options.get(rule(id).round() as usize).cloned()).unwrap_or_default()
}

/// The rules of the landscape, read once per build.
pub struct Cfg {
    pub on: bool,
    /// Quarter turns that bring the street side to the south.
    side: u8,
    margin: f64,
    hedge: Option<&'static str>,
    /// The only side of the plot the hedge runs on, as an index of `sides` (0 street, 1 left of it, ...); `None`: all.
    hedge_edge: Option<usize>,
    path_rows: usize,
    pub setback: f64,
    pub path_clear: f64,
    rows: (bool, bool),
    row_gap: f64,
    bed: f64,
}

impl Cfg {
    pub fn read() -> Cfg {
        let side = ["south", "east", "north", "west"].iter().position(|s| *s == option("site.street_side")).unwrap_or(0);
        let hedge = data()["hedge"]["types"][option("site.hedge")].as_str();
        let rows = option("site.tree_rows");
        let hedge_edge = match option("site.hedge_edge").as_str() {
            "all" | "" => None,
            "street" => Some(0),
            w => ["south", "east", "north", "west"].iter().position(|s| *s == w).map(|i| (i + 4 - side) % 4),
        };
        Cfg {
            on: rule("site.landscape") > 0.5,
            side: side as u8,
            margin: rule("site.plot_margin"),
            hedge,
            hedge_edge,
            path_rows: ((rule("site.path_width") / num(&["path", "tile"])).round() as usize).max(1),
            setback: rule("site.tree_setback"),
            path_clear: rule("site.tree_path_clear"),
            rows: (matches!(rows.as_str(), "street" | "both"), matches!(rows.as_str(), "path" | "both")),
            row_gap: rule("site.row_spacing"),
            bed: rule("site.bed_width"),
        }
    }
}

fn rot(p: P, k: u8) -> P {
    match k {
        0 => p,
        1 => (p.1, -p.0),
        2 => (-p.0, -p.1),
        _ => (-p.1, p.0),
    }
}

fn unrot(q: P, k: u8) -> P {
    match k {
        0 => q,
        1 => (-q.1, q.0),
        2 => (-q.0, -q.1),
        _ => (q.1, -q.0),
    }
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn dist(a: P, b: P) -> f64 {
    let d = sub(a, b);
    d.0.hypot(d.1)
}

fn bbox(r: &[P]) -> (P, P) {
    r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| {
        ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
    })
}

/// Distance from `p` to the outline of the ground slab, its holes included.
pub fn to_outline(l: &GLayout, p: P) -> f64 {
    std::iter::once(&l.slab)
        .chain(l.slab_holes.iter())
        .flat_map(|r| (0..r.len()).map(move |i| dist_to_seg(p, r[i], r[(i + 1) % r.len()])))
        .fold(f64::MAX, f64::min)
}

pub fn built_on(l: &GLayout, p: P) -> bool {
    inside(&l.slab, p) && !l.slab_holes.iter().any(|h| inside(h, p))
}

/// What the landscape put on the ground, in world metres.
#[derive(Default)]
pub struct Land {
    /// Distance of the tree ring from the slab box.
    pub tree_off: f64,
    pub setback: f64,
    pub path_clear: f64,
    pub path_tile: f64,
    /// Centres of the path tiles.
    pub paths: Vec<P>,
    /// Hedge lengths: centre, turn, type.
    pub hedge: Vec<(P, f64, &'static str)>,
    pub beds: Vec<P>,
    pub shrubs: Vec<(P, &'static str)>,
    pub lawn: Vec<P>,
    /// Places for the trees of the rows along the street and the paths.
    pub rows: Vec<P>,
    /// The plot, as the corners of its box turned back to the world, counter-clockwise.
    pub plot: Vec<P>,
}

impl Land {
    /// A tree may stand at `p`: the setback from the building, clear of every path tile.
    pub fn tree_ok(&self, l: &GLayout, p: P) -> bool {
        to_outline(l, p) >= self.setback
            && !built_on(l, p)
            && self.paths.iter().all(|t| dist(*t, p) >= self.path_tile / 2.0 + self.path_clear)
    }

    pub fn fixtures(&self) -> Vec<GFixture> {
        let on = |family, ty, at, rotation| GFixture { scale: 1.0, min_clear: 0.0, family, ty, at, rotation, wall: None, z: 0.0 };
        let mut out = vec![];
        let (path, bed, lawn) = (text(&["path", "type"]), text(&["bed", "type"]), text(&["lawn", "type"]));
        out.extend(self.paths.iter().map(|p| on("Paving Slab", path, *p, 0.0)));
        // sizes are seeded by the place, so a site keeps its plants: lengths of a hedge differ a little
        // (clipped), shrubs as much as trees do
        let jitter = rule("site.tree_size_jitter");
        let sized = |f: GFixture, k: usize, j: f64| {
            let seed = [f.at.0, f.at.1].iter().fold(0x5EEDu64, |a, v| a.rotate_left(21) ^ (v * 1000.0).round() as i64 as u64);
            GFixture { scale: super::trees::scale(seed, k, j), ..f }
        };
        out.extend(self.hedge.iter().enumerate().map(|(k, (p, a, ty))| sized(on("Hedge", ty, *p, *a), k, jitter * 0.25)));
        out.extend(self.beds.iter().map(|p| on("Lawn", bed, *p, 0.0)));
        // a shrub takes any turn: the lumps of its crown are not round
        out.extend(self.shrubs.iter().enumerate().map(|(k, (p, ty))| {
            let turn = f64::from(super::trees::scale(0xB05C, k + (p.0 * 7.0 + p.1 * 13.0).abs() as usize, 1.0)) * std::f64::consts::PI;
            sized(on("Shrub", ty, *p, turn), k, jitter)
        }));
        out.extend(self.lawn.iter().map(|p| on("Lawn", lawn, *p, 0.0)));
        out
    }
}

/// The tree ring offset with and without the landscape.
fn tree_off(cfg: &Cfg, band: f64, clear: f64, pad: f64) -> f64 {
    let base = (band + TREE_GAP).max(cfg.setback);
    if !cfg.on {
        return base;
    }
    let ring = band + clear + cfg.bed + pad;
    base.max(ring + cfg.path_rows as f64 * pad / 2.0 + cfg.path_clear + 1.0)
}

/// Lay the landscape of the site round the ground slab of `l`. `pads` are the paving cells,
/// `doors` the exterior door points; `trees` is the number of trees the brief asks for.
pub fn lay(
    l: &GLayout,
    band: f64,
    clear: f64,
    pad: f64,
    doors: &[P],
    pads: usize,
    trees: u32,
    cfg: &Cfg,
) -> Result<Land, GenError> {
    let mut land = Land {
        tree_off: tree_off(cfg, band, clear, pad),
        setback: cfg.setback,
        path_clear: cfg.path_clear,
        path_tile: pad,
        ..Land::default()
    };
    if !cfg.on || (trees == 0 && band <= 0.0) {
        return Ok(land);
    }
    let k = cfg.side;
    let (lo, hi) = bbox(&l.slab);
    let c = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
    let fwd = |p: P| rot(sub(p, c), k);
    let back = |q: P| {
        let u = unrot(q, k);
        (u.0 + c.0, u.1 + c.1)
    };
    let (llo, lhi) = bbox(&l.slab.iter().map(|p| fwd(*p)).collect::<Vec<_>>());
    let angle = text_angle(k);
    let room = (super::slots::FIXTURES_MAX as usize)
        .saturating_sub(l.fixtures.len() + pads + num(&["reserve"]) as usize);
    let cap = |what: &str| num(&["caps", what]) as usize;

    // ring of the paths just outside the beds, boundary of the plot beyond the tree ring
    let w = band + clear + cfg.bed + pad;
    let margin = (band + cfg.margin).max(if trees > 0 { land.tree_off + TREE_APRON } else { w + 2.0 * pad });
    let (plo, phi) = ((llo.0 - margin, llo.1 - margin), (lhi.0 + margin, lhi.1 + margin));
    land.plot = [plo, (phi.0, plo.1), phi, (plo.0, phi.1)].iter().map(|q| back(*q)).collect();
    let (rlo, rhi) = ((llo.0 - w, llo.1 - w), (lhi.0 + w, lhi.1 + w));
    let (rw, rh) = (rhi.0 - rlo.0, rhi.1 - rlo.1);
    let per = 2.0 * (rw + rh);
    let edge = num(&["hedge", "inset"]);
    let street_y = plo.1 + edge;

    // paths: door, straight out to the ring, round it the shorter way, then to the street
    let ring_t = |e: P| {
        let eps = 1e-6;
        if (e.1 - rlo.1).abs() < eps {
            e.0 - rlo.0
        } else if (e.0 - rhi.0).abs() < eps {
            rw + (e.1 - rlo.1)
        } else if (e.1 - rhi.1).abs() < eps {
            rw + rh + (rhi.0 - e.0)
        } else {
            2.0 * rw + rh + (rhi.1 - e.1)
        }
    };
    let ring_pt = |t: f64| {
        let t = t.rem_euclid(per);
        if t <= rw {
            (rlo.0 + t, rlo.1)
        } else if t <= rw + rh {
            (rhi.0, rlo.1 + t - rw)
        } else if t <= 2.0 * rw + rh {
            (rhi.0 - (t - rw - rh), rhi.1)
        } else {
            (rlo.0, rhi.1 - (t - 2.0 * rw - rh))
        }
    };
    // (local centre, local axis the tile row runs along)
    let mut centres: Vec<(P, P)> = vec![];
    let mut legs: Vec<f64> = vec![];
    for d in doors {
        let probe = |dir: P| to_outline(l, (d.0 + dir.0 * 2.0 * pad, d.1 + dir.1 * 2.0 * pad));
        let Some(dir) = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
            .into_iter()
            .filter(|v| !built_on(l, (d.0 + v.0 * 2.0 * pad, d.1 + v.1 * 2.0 * pad)))
            .max_by(|a, b| probe(*a).total_cmp(&probe(*b)))
        else {
            continue;
        };
        let n = rot(dir, k);
        let mut p = fwd((d.0 + dir.0 * clear, d.1 + dir.1 * clear));
        let out = |p: P| p.0 <= rlo.0 || p.0 >= rhi.0 || p.1 <= rlo.1 || p.1 >= rhi.1;
        let mut guard = 0;
        while !out(p) && guard < 400 {
            centres.push((p, n));
            p = (p.0 + n.0 * pad, p.1 + n.1 * pad);
            guard += 1;
        }
        let e = (p.0.clamp(rlo.0, rhi.0), p.1.clamp(rlo.1, rhi.1));
        let e = if (e.0 - rlo.0).min(rhi.0 - e.0).min(e.1 - rlo.1).min(rhi.1 - e.1) < 1e-6 {
            e
        } else {
            continue;
        };
        centres.push((e, n));
        let s = (e.0, rlo.1);
        let (te, ts) = (ring_t(e), s.0 - rlo.0);
        let fwd_len = (ts - te).rem_euclid(per);
        let (len, sign) = if fwd_len <= per / 2.0 { (fwd_len, 1.0) } else { (per - fwd_len, -1.0) };
        let steps = (len / pad).ceil() as usize;
        for i in 1..=steps {
            let t = te + sign * len * i as f64 / steps as f64;
            let (a, b) = (ring_pt(t - sign * 0.01), ring_pt(t));
            let v = sub(b, a);
            let axis = if v.0.abs() >= v.1.abs() { (v.0.signum(), 0.0) } else { (0.0, v.1.signum()) };
            centres.push((ring_pt(t), axis));
        }
        let mut y = rlo.1 - pad;
        while y > street_y {
            centres.push(((s.0, y), (0.0, -1.0)));
            y -= pad;
        }
        centres.push(((s.0, street_y), (0.0, -1.0)));
        legs.push(s.0);
    }
    let rows = cfg.path_rows as f64;
    for (q, axis) in &centres {
        let perp = (-axis.1, axis.0);
        for j in 0..cfg.path_rows {
            let o = (j as f64 - (rows - 1.0) / 2.0) * pad;
            let w = back((q.0 + perp.0 * o, q.1 + perp.1 * o));
            if !built_on(l, w) && !land.paths.iter().any(|t| dist(*t, w) < pad * 0.55) {
                land.paths.push(w);
            }
        }
    }
    if land.paths.len() > cap("path").min(room) {
        return Err(GenError::Infeasible(format!(
            "the paths to the street need {} tiles; lower `site.path_width` or the number of entrances",
            land.paths.len()
        )));
    }
    let mut room = room - land.paths.len();
    let paths = land.paths.clone();
    let near_path = |p: P, reach: f64| paths.iter().any(|t| (t.0 - p.0).abs() < reach && (t.1 - p.1).abs() < reach);

    // hedge along the boundary, with a gate where a path leaves the plot
    if let Some(ty) = cfg.hedge {
        let len = num(&["hedge", "length"]);
        let (a, b) = ((plo.0 + edge, plo.1 + edge), (phi.0 - edge, phi.1 - edge));
        let sides = [(a, (b.0, a.1), 0.0), ((b.0, a.1), b, 90.0), (b, (a.0, b.1), 180.0), ((a.0, b.1), a, 270.0_f64)];
        let mut hedge = vec![];
        for (si, (s, e, deg)) in sides.into_iter().enumerate() {
            if cfg.hedge_edge.is_some_and(|only| only != si) {
                continue;
            }
            let l_side = dist(s, e);
            let n = (l_side / len).ceil().max(1.0);
            let step = l_side / n;
            let u = ((e.0 - s.0) / l_side, (e.1 - s.1) / l_side);
            for i in 0..n as usize {
                let q = (s.0 + u.0 * step * (i as f64 + 0.5), s.1 + u.1 * step * (i as f64 + 0.5));
                let at = back(q);
                if !land.paths.iter().any(|t| dist(*t, at) < step / 2.0 + pad / 2.0) {
                    hedge.push((at, (deg + angle).to_radians(), ty));
                }
            }
        }
        if hedge.len() <= cap("hedge").min(room) {
            room -= hedge.len();
            land.hedge = hedge;
        }
    }

    // beds in the cells just outside the paving, a shrub on every few of them
    if cfg.bed > 0.0 {
        let shrubs = data()["shrubs"].as_array().map_or(&[][..], Vec::as_slice);
        let stride = num(&["bed", "shrub_stride"]) as i64;
        let reach = band + pad + (cfg.bed + clear) .div_euclid(pad).mul_add(pad, 2.0 * pad);
        let (nx, ny) = (((hi.0 - lo.0 + 2.0 * reach) / pad) as i64, ((hi.1 - lo.1 + 2.0 * reach) / pad) as i64);
        let (mut beds, mut sh) = (vec![], vec![]);
        for i in 0..=nx {
            for j in 0..=ny {
                let p = (lo.0 - reach + pad * (i as f64 + 0.5), lo.1 - reach + pad * (j as f64 + 0.5));
                let d = to_outline(l, p);
                if built_on(l, p) || d <= band + clear + 1e-9 || d > band + clear + cfg.bed + 1e-9 || near_path(p, pad) {
                    continue;
                }
                beds.push(p);
                if !shrubs.is_empty() && (i + 2 * j).rem_euclid(stride.max(1)) == 0 {
                    let ty = shrubs[((i * 7 + j * 3).rem_euclid(shrubs.len() as i64)) as usize].as_str().unwrap_or_default();
                    sh.push((p, ty));
                }
            }
        }
        if beds.len() + sh.len() <= cap("bed").min(room) {
            room -= beds.len() + sh.len();
            land.beds = beds;
            land.shrubs = sh;
        }
    }

    // lawn tiles on the open ground inside the hedge, clear of the beds and the paths
    let tile = num(&["lawn", "tile"]);
    let half = tile / 2.0;
    let free = band + clear + cfg.bed + (half + pad / 2.0) * std::f64::consts::SQRT_2;
    let (nx, ny) = (((phi.0 - plo.0) / tile) as i64, ((phi.1 - plo.1) / tile) as i64);
    let mut lawn = vec![];
    for i in 0..nx {
        for j in 0..ny {
            let q = (plo.0 + tile * (i as f64 + 0.5), plo.1 + tile * (j as f64 + 0.5));
            let (inner_lo, inner_hi) = ((plo.0 + edge + 0.5, plo.1 + edge + 0.5), (phi.0 - edge - 0.5, phi.1 - edge - 0.5));
            if q.0 - half < inner_lo.0 || q.1 - half < inner_lo.1 || q.0 + half > inner_hi.0 || q.1 + half > inner_hi.1 {
                continue;
            }
            let w = back(q);
            if built_on(l, w) || to_outline(l, w) < free || near_path(w, half + pad / 2.0) {
                continue;
            }
            lawn.push(w);
        }
    }
    if lawn.len() <= cap("lawn").min(room) {
        room -= lawn.len();
        land.lawn = lawn;
    }

    // tree rows: along the street edge and along each street leg of a path
    let reach = pad / 2.0 + cfg.path_clear + 1.0;
    let mut rows: Vec<P> = vec![];
    if cfg.rows.0 {
        let y = plo.1 + num(&["row_inset"]);
        let n = (((phi.0 - plo.0) - 2.0 * num(&["row_inset"])) / cfg.row_gap).floor() as i64 + 1;
        let start = (plo.0 + phi.0) / 2.0 - (n - 1) as f64 * cfg.row_gap / 2.0;
        for i in 0..n {
            let x = start + i as f64 * cfg.row_gap;
            if legs.iter().all(|g| (g - x).abs() >= reach + cfg.path_rows as f64 * pad / 2.0) {
                rows.push(back((x, y)));
            }
        }
    }
    if cfg.rows.1 {
        let side = reach + (cfg.path_rows as f64 - 1.0) * pad / 2.0 + pad / 2.0;
        let mut seen: Vec<f64> = vec![];
        for g in &legs {
            if seen.iter().any(|s| (s - g).abs() < 1e-6) {
                continue;
            }
            seen.push(*g);
            let mut y = rlo.1 - cfg.row_gap / 2.0;
            while y > plo.1 + num(&["row_inset"]) {
                rows.push(back((g - side, y)));
                rows.push(back((g + side, y)));
                y -= cfg.row_gap;
            }
        }
    }
    rows.truncate(cap("row"));
    land.rows = rows.into_iter().filter(|p| land.tree_ok(l, *p) && inside_plot(&land.plot, *p)).collect();
    Ok(land)
}

fn text_angle(k: u8) -> f64 {
    let v = &data()["street_angles"];
    let name = ["south", "east", "north", "west"][k as usize];
    v[name].as_f64().unwrap_or_else(|| panic!("data/landscape.json: street_angles.{name}"))
}

fn inside_plot(plot: &[P], p: P) -> bool {
    inside(plot, p)
}
