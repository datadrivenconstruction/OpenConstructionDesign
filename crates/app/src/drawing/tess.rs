//! Tessellation cache of plan underlays. Fills, lines and arcs of a drawing are tessellated once
//! per (drawing id, `Drawings::version`, zoom bucket, style) in a local frame at the bucket's
//! reference scale, cut into grid pieces. A frame only scales and moves the visible pieces; a
//! frame with the same view reuses the shapes of the last one. Saving or exporting the project
//! does not touch the drawings, so it never rebuilds them.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ddc_geom::{v2, V2};
use egui::{
    self,
    emath::TSTransform,
    epaint::{Mesh, TessellationOptions, Tessellator},
    Color32, Painter, Pos2, Rect, Shape,
};

use super::Drawing;

/// Zoom buckets per doubling of the scale: line widths stray at most ~12 % inside one.
const PER_DOUBLING: f64 = 3.0;
/// Grid of pieces per side: off-screen pieces are skipped on a frame.
const GRID: usize = 8;
/// Above this many vertices a build gives up (fine dashes at deep zoom): drawn directly.
const MAX_VERTS: usize = 6_000_000;
/// Local coordinates stay well inside f32 precision.
const MAX_EXTENT_PX: f64 = 4.0e6;
/// Entries kept: the current and the last buckets of a few drawings.
const CAP: usize = 8;

pub fn bucket(scale: f64) -> i32 {
    (scale.max(1e-9).log2() * PER_DOUBLING).round() as i32
}

/// Reference scale of a bucket (px per m).
pub fn bucket_scale(b: i32) -> f64 {
    2f64.powf(b as f64 / PER_DOUBLING)
}

/// What a tessellation depends on besides the geometry.
#[derive(Clone, Copy)]
pub struct Style {
    pub fg: Color32,
    pub paper: Color32,
    pub ppp: f32,
    pub opts: TessellationOptions,
}

impl Style {
    pub fn plain(fg: Color32, paper: Color32) -> Self {
        Style {
            fg,
            paper,
            ppp: 1.0,
            opts: TessellationOptions::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub id: u64,
    pub version: u64,
    pub bucket: i32,
    /// Layers' visibility and colors, theme colors, pixels per point.
    pub style: u64,
}

pub fn style_hash(d: &Drawing, st: &Style) -> u64 {
    let mut h = DefaultHasher::new();
    d.halftone.hash(&mut h);
    // belt and braces next to the version: an edit generation or a size change never hits
    (d.gen, d.u.lines.len(), d.u.arcs.len(), d.u.fills.len()).hash(&mut h);
    for l in &d.layers {
        (&l.name, l.visible, l.frozen, l.color, l.file_color, l.halftone).hash(&mut h);
    }
    st.fg.to_array().hash(&mut h);
    st.paper.to_array().hash(&mut h);
    st.ppp.to_bits().hash(&mut h);
    (st.opts.feathering, st.opts.feathering_size_in_pixels.to_bits()).hash(&mut h);
    h.finish()
}

pub struct Piece {
    pub mesh: Arc<Mesh>,
    /// Bounds in the local frame.
    pub bounds: Rect,
}

pub enum Tess {
    /// Local frame: `((x - lo.x) * s0, -(y - lo.y) * s0)`; fill pieces come first.
    Pieces(Vec<Piece>),
    /// Too heavy to keep: the frame draws it directly, clipped to the view.
    Direct,
}

/// Exact view of the last frame: scale, origin on screen, clip.
type View = [u32; 7];

struct Entry {
    key: Key,
    tess: Arc<Tess>,
    last: Option<(View, Vec<Shape>)>,
    used: u64,
}

#[derive(Default)]
pub struct Cache {
    entries: Vec<Entry>,
    tick: u64,
    /// Bounds per (drawing id, version): a frame does not walk the geometry for them.
    bounds: Vec<(u64, u64, Option<(V2, V2)>)>,
    /// Tessellations built so far.
    pub builds: u64,
}

impl Cache {
    pub fn contains(&self, key: &Key) -> bool {
        self.entries.iter().any(|e| e.key == *key)
    }

    /// The entry of `key`, built by `build` on a miss. A new version of a drawing drops its
    /// older entries at once; past `CAP` the least recently used goes.
    fn entry(&mut self, key: Key, build: impl FnOnce() -> Tess) -> &mut Entry {
        self.tick += 1;
        let tick = self.tick;
        if let Some(k) = self.entries.iter().position(|e| e.key == key) {
            let e = &mut self.entries[k];
            e.used = tick;
            return e;
        }
        self.entries
            .retain(|e| e.key.id != key.id || e.key.version == key.version);
        if self.entries.len() >= CAP {
            if let Some(k) = (0..self.entries.len()).min_by_key(|&k| self.entries[k].used) {
                self.entries.swap_remove(k);
            }
        }
        self.builds += 1;
        self.entries.push(Entry {
            key,
            tess: Arc::new(build()),
            last: None,
            used: tick,
        });
        self.entries.last_mut().expect("just pushed")
    }

    /// As `entry`, for tests and warm-up: `true` on a hit.
    pub fn touch(&mut self, key: Key, build: impl FnOnce() -> Tess) -> bool {
        let hit = self.contains(&key);
        self.entry(key, build);
        hit
    }
}

impl Cache {
    fn bbox(&mut self, version: u64, d: &Drawing) -> Option<(V2, V2)> {
        if let Some(b) = self.bounds.iter().find(|b| b.0 == d.id && b.1 == version) {
            return b.2;
        }
        let b = d.bbox();
        self.bounds.retain(|b| b.0 != d.id);
        self.bounds.push((d.id, version, b));
        b
    }
}

/// Key, reference scale and bounds of `d` at `scale`, if the cache can take this view.
fn key_of(
    cache: &mut Cache,
    version: u64,
    d: &Drawing,
    scale: f64,
    st: &Style,
) -> Option<(Key, f64, (V2, V2))> {
    let (lo, hi) = cache.bbox(version, d)?;
    let b = bucket(scale);
    let s0 = bucket_scale(b);
    if (hi.x - lo.x).max(hi.y - lo.y) * s0 > MAX_EXTENT_PX || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let key = Key {
        id: d.id,
        version,
        bucket: b,
        style: style_hash(d, st),
    };
    Some((key, s0, (lo, hi)))
}

/// Tessellate `d` at reference scale `s0` in its local frame.
pub fn build(d: &Drawing, (lo, hi): (V2, V2), s0: f64, st: &Style) -> Tess {
    let mut tz = Tessellator::new(st.ppp, st.opts, [1, 1], vec![]);
    let to_l = |w: V2| Pos2::new(((w.x - lo.x) * s0) as f32, (-(w.y - lo.y) * s0) as f32);
    let (span_x, span_y) = ((hi.x - lo.x).max(1e-9), (hi.y - lo.y).max(1e-9));
    let (wx, wy) = ((span_x * s0) as f32, (span_y * s0) as f32);
    let cell = |p: Pos2| {
        let cx = ((p.x / wx) * GRID as f32).floor().clamp(0.0, GRID as f32 - 1.0) as usize;
        let cy = ((-p.y / wy) * GRID as f32).floor().clamp(0.0, GRID as f32 - 1.0) as usize;
        cy * GRID + cx
    };
    let mut fills: Vec<Mesh> = (0..GRID * GRID).map(|_| Mesh::default()).collect();
    let mut lines: Vec<Mesh> = (0..GRID * GRID).map(|_| Mesh::default()).collect();
    let mut verts = 0usize;
    let mut over = false;
    super::emit_geometry(d, &to_l, s0, None, st.fg, st.paper, &mut |sh, fill, at| {
        if over {
            return;
        }
        let set = if fill { &mut fills } else { &mut lines };
        let m = &mut set[cell(at)];
        let before = m.vertices.len();
        tz.tessellate_shape(sh, m);
        verts += m.vertices.len() - before;
        over = verts > MAX_VERTS;
    });
    if over {
        return Tess::Direct;
    }
    let pieces = fills
        .into_iter()
        .chain(lines)
        .filter(|m| !m.is_empty())
        .map(|m| Piece {
            bounds: m.calc_bounds(),
            mesh: Arc::new(m),
        })
        .collect();
    Tess::Pieces(pieces)
}

/// Make sure the entry of `d` at `scale` exists; `true` when it already did.
pub fn warm(cache: &mut Cache, version: u64, d: &Drawing, scale: f64, st: &Style) -> bool {
    match key_of(cache, version, d, scale, st) {
        Some((key, s0, b)) => cache.touch(key, || build(d, b, s0, st)),
        None => true,
    }
}

/// Paint fills, lines and arcs of `d` from the cache; `false` when the caller must draw them
/// directly (view not axis-aligned, extent too big, build too heavy).
#[allow(clippy::too_many_arguments)]
pub fn paint_cached(
    cache: &mut Cache,
    version: u64,
    d: &Drawing,
    p: &Painter,
    to_s: &dyn Fn(V2) -> Pos2,
    clip: Rect,
    scale: f64,
    st: &Style,
) -> bool {
    let Some((key, s0, b)) = key_of(cache, version, d, scale, st) else {
        return false;
    };
    let lo = b.0;
    // the plan maps world to screen by one uniform scale with y down: check, else draw directly
    let p0 = to_s(lo);
    let px = to_s(v2(lo.x + 1.0, lo.y + 1.0)) - p0;
    let tol = 0.01 * scale as f32 + 0.05;
    if (px.x - scale as f32).abs() > tol || (px.y + scale as f32).abs() > tol {
        return false;
    }
    let e = cache.entry(key, || build(d, b, s0, st));
    let Tess::Pieces(pieces) = &*e.tess else {
        return false;
    };
    let t = TSTransform::new(p0.to_vec2(), (scale / s0) as f32);
    let view: View = [
        t.scaling.to_bits(),
        p0.x.to_bits(),
        p0.y.to_bits(),
        clip.min.x.to_bits(),
        clip.min.y.to_bits(),
        clip.max.x.to_bits(),
        clip.max.y.to_bits(),
    ];
    if let Some((v, shapes)) = &e.last {
        if *v == view {
            p.extend(shapes.iter().cloned());
            return true;
        }
    }
    let mut out = Vec::new();
    for pc in pieces {
        let r = Rect::from_min_max(t * pc.bounds.min, t * pc.bounds.max);
        if !r.intersects(clip) {
            continue;
        }
        let mut m = (*pc.mesh).clone();
        m.transform(t);
        out.push(egui::Shape::mesh(m));
    }
    p.extend(out.iter().cloned());
    e.last = Some((view, out));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::{Drawings, Origin};
    use ddc_io::drawing_model::{self as dm, Underlay};

    fn one() -> Drawings {
        let doc = dm::fixture::sample_plan(0);
        let mut ds = Drawings::default();
        ds.add_underlay(
            Underlay::from_dxf(&doc, 0.001),
            0.001,
            "p",
            None,
            1,
            Origin::AsIs,
            v2(0.0, 0.0),
        );
        ds
    }

    fn st() -> Style {
        Style::plain(Color32::WHITE, Color32::from_gray(20))
    }

    #[test]
    fn same_version_and_bucket_is_a_hit() {
        let ds = one();
        assert!(!ds.tess_warm(1, 40.0, &st()), "first frame builds");
        let n = ds.tess_builds();
        assert_eq!(n, 1);
        // a pan, an idle frame and a zoom inside the bucket all reuse it
        assert!(ds.tess_warm(1, 40.0, &st()));
        assert_eq!(bucket(40.0), bucket(41.0));
        assert!(ds.tess_warm(1, 41.0, &st()));
        assert_eq!(ds.tess_builds(), n);
        let d = &ds.list[0];
        match build(d, d.bbox().unwrap(), bucket_scale(bucket(40.0)), &st()) {
            Tess::Pieces(pieces) => assert!(!pieces.is_empty()),
            Tess::Direct => panic!("a small plan is cached"),
        }
    }

    #[test]
    fn a_new_version_misses_and_drops_the_old_entry() {
        let mut ds = one();
        ds.tess_warm(1, 40.0, &st());
        let old = key_of(&mut ds.tess.borrow_mut(), ds.version, &ds.list[0], 40.0, &st())
            .unwrap()
            .0;
        ds.touch();
        assert!(!ds.tess_warm(1, 40.0, &st()));
        assert_eq!(ds.tess_builds(), 2);
        assert!(!ds.tess.borrow().contains(&old), "stale version is freed");
        // a layer switched off changes the style: also a miss
        ds.list[0].layers[0].visible = !ds.list[0].layers[0].visible;
        assert!(!ds.tess_warm(1, 40.0, &st()));
    }

    #[test]
    fn an_edit_and_its_undo_miss_and_a_reload_misses() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        let lv = ed.level;
        let doc = dm::fixture::sample_plan(0);
        ds.add(&doc, "p", None, lv, Some(0.001), Origin::AsIs, v2(0.0, 0.0));
        assert!(!ds.tess_warm(lv, 40.0, &st()));
        assert!(ds.tess_warm(lv, 40.0, &st()), "unchanged: hit");
        let n = ds.tess_builds();
        ds.xf = (3.0, 0.0, 0.0);
        ds.apply_xf(true, false);
        ds.sync_undo(&mut ed);
        assert!(!ds.tess_warm(lv, 40.0, &st()), "edit: miss");
        assert_eq!(ds.tess_builds(), n + 1);
        ed.undo();
        ds.sync_undo(&mut ed);
        assert!(!ds.tess_warm(lv, 40.0, &st()), "undo: miss");
        assert_eq!(ds.tess_builds(), n + 2);
        // a reload adds a drawing with a new id: its own entry is built
        ds.add(&doc, "p2", None, lv, Some(0.001), Origin::AsIs, v2(0.0, 0.0));
        assert!(!ds.tess_warm(lv, 40.0, &st()), "reload: miss");
    }

    #[test]
    fn a_new_zoom_bucket_misses_and_the_old_one_stays() {
        let ds = one();
        ds.tess_warm(1, 40.0, &st());
        assert_ne!(bucket(40.0), bucket(80.0));
        assert!(!ds.tess_warm(1, 80.0, &st()));
        assert_eq!(ds.tess_builds(), 2);
        assert!(ds.tess_warm(1, 40.0, &st()), "zooming back reuses the first bucket");
        assert_eq!(ds.tess_builds(), 2);
    }

    #[test]
    fn buckets_are_stable_and_reference_scales_close() {
        for s in [2.0, 7.5, 40.0, 333.0, 2000.0] {
            let r = s / bucket_scale(bucket(s));
            assert!((0.88..1.13).contains(&r), "{s}: {r}");
        }
    }
}
