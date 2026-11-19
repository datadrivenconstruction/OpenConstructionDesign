//! Reinforcement and the analytical model in the views: `edit3d::scene3d` 3D providers (bar bodies
//! per set, analytical axes as tubes) and the plan layer (bars below the cut plane are lines,
//! bars crossing it are dots; analytical axes are dashed). Providers declare tables and, using
//! fingerprints of sets and elements, rebuild only the levels whose bodies changed; set bodies
//! are cached until their geometry changes.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, V2, V3};
use ddc_model::Model;

use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d::{self, Part, SceneProvider};
use crate::editor::Editor;
use crate::scene_ids::{EXT_ANALYTICAL, EXT_REBAR};

/// Bar color (tag `TAG_RGB`) and analytical axis color.
pub const REBAR_RGB: [u8; 3] = [0xb0, 0x3a, 0x2e];
pub const AXIS_RGB: [u8; 3] = [0x2e, 0x7d, 0xd0];
/// Plan cut height above the level, m (same as the structural `struct_plan`).
pub const PLAN_CUT: f64 = 1.2;
/// Radius of the analytical axis tube in 3D, m.
const AXIS_R: f64 = 0.02;

static SHOW_REBAR: AtomicBool = AtomicBool::new(true);
static SHOW_ANALYTICAL: AtomicBool = AtomicBool::new(false);

pub fn rebar_shown() -> bool {
    SHOW_REBAR.load(Ordering::Relaxed)
}

pub fn set_rebar_shown(on: bool) {
    SHOW_REBAR.store(on, Ordering::Relaxed);
}

pub fn analytical_shown() -> bool {
    SHOW_ANALYTICAL.load(Ordering::Relaxed)
}

pub fn set_analytical_shown(on: bool) {
    SHOW_ANALYTICAL.store(on, Ordering::Relaxed);
}

fn tag(c: [u8; 3]) -> u32 {
    ddc_render::TAG_RGB | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

/// Tables the bars depend on: sets, layers, types, and hosts with cutouts and profiles.
const REBAR_TABLES: &[&str] = &[
    "rebar_set",
    "rebar_cover",
    "rebar_type",
    "rebar_mark",
    "col",
    "beam",
    "member_type",
    "wall",
    "wall_type_layer",
    "wall_profile",
    "wall_attach",
    "wall_attach_level",
    "opening",
    "slab",
    "slab_vertex",
    "slab_shape",
    "slab_type_layer",
    "iso_footing",
    "shaft",
    "shaft_vertex",
    "shaft_cut",
    "host_opening",
    "sketch",
    "level",
];

/// Tables the analytical model depends on.
const AN_TABLES: &[&str] = &[
    "col",
    "beam",
    "brace",
    "member_type",
    "wall",
    "wall_type_layer",
    "slab",
    "slab_vertex",
    "slab_type_layer",
    "iso_footing",
    "strip_footing",
    "raft",
    "raft_vertex",
    "analytical_adj",
    "level",
];

/// Fingerprints: key (set, target) -> (level, geometry fingerprint).
type Sigs = BTreeMap<i64, (i64, u64)>;

/// Levels where a key's fingerprint appeared, disappeared or changed; a change in tube face count affects all.
fn changed_levels(old: &Sigs, old_k: usize, new: &Sigs, k: usize) -> Vec<i64> {
    let mut out = BTreeSet::new();
    for (id, v) in old {
        if old_k != k || new.get(id) != Some(v) {
            out.insert(v.0);
        }
    }
    for (id, v) in new {
        if old_k != k || old.get(id) != Some(v) {
            out.insert(v.0);
        }
    }
    out.into_iter().collect()
}

/// Level bars: one part per set (a 3D pick selects the whole set).
#[derive(Default)]
struct RebarProvider {
    /// Fingerprints and face count at the previous check.
    seen: Mutex<(Sigs, usize)>,
    /// Set bodies: set -> (fingerprint, face count, body).
    memo: Mutex<HashMap<i64, (u64, usize, Arc<Mesh>)>>,
}

impl SceneProvider for RebarProvider {
    fn key(&self) -> &'static str {
        "rebar"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        if !rebar_shown() {
            return vec![];
        }
        let Ok(b) = ed.model.rebar_built() else {
            return vec![];
        };
        let k = ddc_geom::rebar::lod_sides(b.bars.len());
        let mut by: BTreeMap<i64, Vec<&ddc_geom::rebar::Bar>> = BTreeMap::new();
        for x in b.bars.iter().filter(|x| x.level == level) {
            by.entry(x.set).or_default().push(&x.bar);
        }
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        memo.retain(|s, _| b.sigs.contains_key(s));
        let t = tag(REBAR_RGB);
        let mut out = vec![];
        for (set, bars) in by {
            let sig = b.sigs.get(&set).map_or(0, |s| s.1);
            let mesh = match memo.get(&set) {
                Some((s, kk, m)) if *s == sig && *kk == k => m.clone(),
                _ => {
                    let ms: Vec<Mesh> = bars.iter().map(|x| x.mesh(k)).collect();
                    let m = Arc::new(ddc_geom::mep::merge(&ms));
                    memo.insert(set, (sig, k, m.clone()));
                    m
                }
            };
            if mesh.triangles.is_empty() {
                continue;
            }
            let mut p = Part::new(Target::new(Cat::Ext(EXT_REBAR), set), t);
            p.push(&mesh, ed.elevation);
            out.push(p);
        }
        out
    }

    fn tables(&self) -> &'static [&'static str] {
        REBAR_TABLES
    }

    fn dirty_levels(&self, ed: &Editor) -> Option<Vec<i64>> {
        // hidden bars are not built: toggling visibility resets the scene cache
        if !rebar_shown() {
            return Some(vec![]);
        }
        let b = ed.model.rebar_built().ok()?;
        let k = ddc_geom::rebar::lod_sides(b.bars.len());
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let out = changed_levels(&seen.0, seen.1, &b.sigs, k);
        *seen = (b.sigs.clone(), k);
        Some(out)
    }
}

/// Stable analytical target id: kind in the high bits, element id in the low bits.
pub(crate) fn an_id(kind: &str, id: i64) -> i64 {
    let code: i64 = match kind {
        "column" => 1,
        "beam" => 2,
        "brace" => 3,
        "wall" => 4,
        "slab" => 5,
        _ => 15,
    };
    code << 40 | (id & ((1i64 << 40) - 1))
}

/// Inverse of [`an_id`]: the adjustable host kind and its id; None for kinds without adjustments.
pub(crate) fn an_split(id: i64) -> Option<(&'static str, i64)> {
    let kind = match id >> 40 {
        1 => "column",
        2 => "beam",
        3 => "brace",
        4 => "wall",
        5 => "slab",
        _ => return None,
    };
    Some((kind, id & ((1i64 << 40) - 1)))
}

/// Analytical element fingerprints: target id -> (level, node coordinates).
fn an_sigs(a: &ddc_model::Analytical) -> Sigs {
    let mut out = Sigs::new();
    let mut put = |kind: &str, id: i64, level: i64, nodes: &[usize]| {
        let mut h = DefaultHasher::new();
        for n in nodes {
            let p = a.nodes[*n];
            [p.x, p.y, p.z].map(f64::to_bits).hash(&mut h);
        }
        out.insert(an_id(kind, id), (level, h.finish()));
    };
    for m in &a.members {
        put(m.kind, m.id, m.level, &m.nodes);
    }
    for p in &a.panels {
        put(p.kind, p.id, p.level, &p.nodes);
    }
    out
}

/// Bar axes and analytical plate outlines of a level (one part per element).
#[derive(Default)]
struct AnalyticalProvider {
    seen: Mutex<Sigs>,
}

impl SceneProvider for AnalyticalProvider {
    fn key(&self) -> &'static str {
        "analytical"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        if !analytical_shown() {
            return vec![];
        }
        let Ok(a) = ed.model.analytical() else {
            return vec![];
        };
        let t = tag(AXIS_RGB);
        let mut out = vec![];
        let part = |id: i64, pts: Vec<V3>, out: &mut Vec<Part>| {
            if pts.len() < 2 {
                return;
            }
            let m = ddc_geom::rebar::tube(&pts, AXIS_R, 6);
            let mut p = Part::new(Target::new(Cat::Ext(EXT_ANALYTICAL), id), t);
            p.push(&m, ed.elevation);
            out.push(p);
        };
        for m in a.members.iter().filter(|m| m.level == level) {
            let pts: Vec<V3> = m.nodes.iter().map(|n| a.nodes[*n]).collect();
            part(an_id(m.kind, m.id), pts, &mut out);
        }
        for pn in a.panels.iter().filter(|p| p.level == level) {
            let mut pts: Vec<V3> = pn.nodes.iter().map(|n| a.nodes[*n]).collect();
            if let Some(f) = pts.first().copied() {
                pts.push(f);
            }
            part(an_id(pn.kind, pn.id), pts, &mut out);
        }
        out
    }

    fn tables(&self) -> &'static [&'static str] {
        AN_TABLES
    }

    fn dirty_levels(&self, ed: &Editor) -> Option<Vec<i64>> {
        // hidden analytics is not computed: toggling visibility resets the scene cache
        if !analytical_shown() {
            return Some(vec![]);
        }
        let a = ed.model.analytical().ok()?;
        let now = an_sigs(&a);
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let out = changed_levels(&seen, 0, &now, 0);
        *seen = now;
        Some(out)
    }
}

/// Register the providers (a repeated call replaces the same ones).
pub fn register() {
    scene3d::register(Arc::new(RebarProvider::default()));
    scene3d::register(Arc::new(AnalyticalProvider::default()));
}

fn p2(v: V3) -> V2 {
    v2(v.x, v.y)
}

/// Level plan outlines for `tools::layer::Layer`: (axis is dashed, color, points, closed).
pub fn plan_polys(m: &Model, level: i64) -> Vec<(bool, [u8; 3], Vec<V2>, bool)> {
    let mut out = vec![];
    if rebar_shown() {
        for p in m.rebar_plan(level, PLAN_CUT).unwrap_or_default() {
            for l in p.lines {
                out.push((false, REBAR_RGB, l, false));
            }
            // bars above the cut plane: dashed hidden lines
            for l in p.hidden {
                out.push((true, REBAR_RGB, l, false));
            }
            // `dots` already holds the bar radius
            for (c, r) in p.dots {
                out.push((false, REBAR_RGB, dot(c, dot_r(r)), true));
            }
        }
    }
    if analytical_shown() {
        if let Ok(a) = m.analytical() {
            for mb in a.members.iter().filter(|x| x.level == level) {
                let pts: Vec<V2> = mb.nodes.iter().map(|n| p2(a.nodes[*n])).collect();
                if pts.len() >= 2 && (pts[0] - pts[pts.len() - 1]).len() > 1e-6 {
                    out.push((true, AXIS_RGB, pts, false));
                } else if let Some(c) = pts.first() {
                    // vertical bar: shown as a node in plan
                    out.push((true, AXIS_RGB, dot(*c, 0.05), true));
                }
            }
            for pn in a.panels.iter().filter(|x| x.level == level) {
                let pts: Vec<V2> = pn.nodes.iter().map(|n| p2(a.nodes[*n])).collect();
                if pts.len() >= 3 {
                    out.push((true, AXIS_RGB, pts, true));
                }
            }
        }
    }
    out
}

/// Repeats the first point: picking treats hosted outlines as open polylines.
fn closed(mut v: Vec<V2>) -> Vec<V2> {
    if let Some(f) = v.first().copied() {
        v.push(f);
    }
    v
}

type PickCache = Option<((i64, u64), Vec<(i64, Vec<Vec<V2>>)>)>;

thread_local! {
    static PICK_SETS: std::cell::RefCell<PickCache> = const { std::cell::RefCell::new(None) };
}

/// Plan outlines of the rebar sets drawn on a level, for picking: bars below the cut plane as
/// lines, bars above it as hidden lines, bars crossing it as closed small polygons. Empty while
/// the rebar is hidden. Reused while the geometry signatures of the level's sets stay equal, so
/// a reload after an unrelated edit does not walk every bar again.
pub(crate) fn pick_sets(m: &Model, level: i64) -> Vec<(i64, Vec<Vec<V2>>)> {
    if !rebar_shown() {
        return vec![];
    }
    let Ok(b) = m.rebar_built() else {
        return vec![];
    };
    let mut h = DefaultHasher::new();
    for (set, (lv, sig)) in &b.sigs {
        if *lv == level {
            (set, sig).hash(&mut h);
        }
    }
    let key = (level, h.finish());
    if let Some(v) = PICK_SETS.with_borrow(|c| c.as_ref().filter(|x| x.0 == key).map(|x| x.1.clone()))
    {
        return v;
    }
    let out: Vec<_> = m
        .rebar_plan(level, PLAN_CUT)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            let mut rings: Vec<Vec<V2>> = p
                .lines
                .into_iter()
                .chain(p.hidden)
                .filter(|l| l.len() >= 2)
                .collect();
            rings.extend(p.dots.into_iter().map(|(c, r)| closed(dot(c, dot_r(r)))));
            (!rings.is_empty()).then_some((p.set, rings))
        })
        .collect();
    PICK_SETS.with_borrow_mut(|c| *c = Some((key, out.clone())));
    out
}

/// Plan outlines of the analytical members of a level, keyed by [`an_id`]. Empty while the
/// analytical model is hidden.
pub(crate) fn pick_members(m: &Model, level: i64) -> Vec<(i64, Vec<Vec<V2>>)> {
    if !analytical_shown() {
        return vec![];
    }
    let Ok(a) = m.analytical() else {
        return vec![];
    };
    let mut out = vec![];
    for mb in a.members.iter().filter(|x| x.level == level) {
        let pts: Vec<V2> = mb.nodes.iter().map(|n| p2(a.nodes[*n])).collect();
        let ring = if pts.len() >= 2 && (pts[0] - pts[pts.len() - 1]).len() > 1e-6 {
            pts
        } else if let Some(c) = pts.first() {
            closed(dot(*c, 0.05))
        } else {
            continue;
        };
        out.push((an_id(mb.kind, mb.id), vec![ring]));
    }
    for pn in a.panels.iter().filter(|x| x.level == level) {
        let pts: Vec<V2> = pn.nodes.iter().map(|n| p2(a.nodes[*n])).collect();
        if pts.len() >= 3 {
            out.push((an_id(pn.kind, pn.id), vec![closed(pts)]));
        }
    }
    out
}

/// Bar dot radius in plan: the bar radius, at least 6 mm for visibility.
fn dot_r(r: f64) -> f64 {
    r.max(0.006)
}

/// Octagon of a bar dot in section.
fn dot(c: V2, r: f64) -> Vec<V2> {
    (0..8)
        .map(|k| {
            let a = k as f64 * std::f64::consts::FRAC_PI_4;
            v2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_is_closed_octagon() {
        let d = dot(v2(1.0, 2.0), 0.01);
        assert_eq!(d.len(), 8);
        assert!(d
            .iter()
            .all(|p| ((*p - v2(1.0, 2.0)).len() - 0.01).abs() < 1e-12));
    }

    #[test]
    fn plan_dot_keeps_the_bar_radius() {
        // 20 mm bar: radius 10 mm, not 5; thin bars stay visible
        assert_eq!(dot_r(0.01), 0.01);
        assert_eq!(dot_r(0.003), 0.006);
    }

    #[test]
    fn only_levels_with_changed_sets_rebuild() {
        let old: Sigs = [(1, (10, 7)), (2, (20, 8)), (3, (30, 9))].into();
        let mut new = old.clone();
        assert!(changed_levels(&old, 8, &new, 8).is_empty());
        new.insert(2, (20, 99));
        new.remove(&3);
        new.insert(4, (40, 1));
        assert_eq!(changed_levels(&old, 8, &new, 8), vec![20, 30, 40]);
        // a set moved between levels: both rebuild
        let moved: Sigs = [(1, (11, 7)), (2, (20, 8)), (3, (30, 9))].into();
        assert_eq!(changed_levels(&old, 8, &moved, 8), vec![10, 11]);
        // fewer tube sides: everything rebuilds
        assert_eq!(changed_levels(&old, 8, &old, 6), vec![10, 20, 30]);
    }

    #[test]
    fn analytical_ids_are_stable_per_element() {
        assert_eq!(an_id("beam", 5), an_id("beam", 5));
        assert_ne!(an_id("beam", 5), an_id("column", 5));
        assert_ne!(an_id("wall", 5), an_id("slab", 5));
        assert_eq!(an_id("brace", 7) & 0xff_ffff_ffff, 7);
    }

    #[test]
    fn providers_declare_their_tables() {
        assert!(REBAR_TABLES.contains(&"rebar_set") && REBAR_TABLES.contains(&"shaft"));
        assert!(!REBAR_TABLES.contains(&"door") && !AN_TABLES.contains(&"rebar_set"));
        let r = RebarProvider::default();
        assert_eq!(r.tables(), REBAR_TABLES);
        assert_eq!(AnalyticalProvider::default().tables(), AN_TABLES);
    }

    #[test]
    fn colour_tag_carries_rgb() {
        assert_eq!(tag([1, 2, 3]) & 0xff_ffff, 0x01_0203);
        assert_ne!(tag(REBAR_RGB) & ddc_render::TAG_RGB, 0);
    }
}
