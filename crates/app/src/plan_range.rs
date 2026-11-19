//! Plan view range as the plan draws it: one cached read per model revision and a
//! classification of an element's height span against the range.

use std::rc::Rc;

use ddc_geom::V2;
use ddc_model::ViewRange;

use crate::editor::cw::SlabItem;
use crate::editor::{Editor, Wall};

/// How a plan draws an element, by where its height span lies relative to the view range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeClass {
    /// Crosses the cut plane: filled section.
    Cut,
    /// Inside the primary range below the cut: thin projection.
    Projection,
    /// Above the cut, or below the bottom within the view depth: halftone dashed.
    Beyond,
    /// Below the view depth: not drawn and not pickable.
    Hidden,
}

/// Class of a span `[z_lo, z_hi]` (absolute metres) on a level at `elevation`.
/// The default range hides nothing, so untouched views draw as before view ranges existed.
pub fn classify(r: &ViewRange, elevation: f64, z_lo: f64, z_hi: f64) -> RangeClass {
    let cut_z = elevation + r.cut;
    let bottom_z = elevation + r.bottom;
    if z_lo <= cut_z && z_hi >= cut_z {
        RangeClass::Cut
    } else if z_lo > cut_z {
        match r.top {
            Some(t) if z_lo > elevation + t => RangeClass::Hidden,
            _ => RangeClass::Beyond,
        }
    } else if z_hi >= bottom_z || *r == ViewRange::default() {
        RangeClass::Projection
    } else if z_hi >= bottom_z - r.depth {
        RangeClass::Beyond
    } else {
        RangeClass::Hidden
    }
}

type Key = (usize, u64, u64, i64);

/// Last range read, valid while the key (connection, change count, revision, level) holds.
#[derive(Default)]
pub struct RangeCache(Option<(Key, ViewRange)>);

impl RangeCache {
    /// The cached range for `key`; `load` runs only when the key changed.
    pub fn get(&mut self, key: Key, load: impl FnOnce() -> ViewRange) -> ViewRange {
        match self.0 {
            Some((k, r)) if k == key => r,
            _ => {
                let r = load();
                self.0 = Some((key, r));
                r
            }
        }
    }
}

/// Absolute height span of a wall, including its top and base profiles.
pub fn wall_span(w: &Wall, elevation: f64) -> (f64, f64) {
    let bottom = elevation + w.base_offset;
    let low = w.base.iter().flatten().map(|p| p.1).fold(0.0, f64::min);
    let top = w
        .top
        .as_ref()
        .filter(|t| !t.is_empty())
        .map_or(w.height, |t| t.iter().map(|p| p.1).fold(f64::MIN, f64::max));
    (bottom + low, (bottom + top).max(bottom + low))
}

/// Absolute height span of a slab on a level at `elevation`, following its slope.
pub fn slab_span(s: &SlabItem, elevation: f64) -> (f64, f64) {
    let (mut lo, mut hi) = (s.top, s.top);
    for p in s.rings.first().into_iter().flatten() {
        let z = s.top_at(*p);
        lo = lo.min(z);
        hi = hi.max(z);
    }
    (elevation + lo - s.thickness, elevation + hi)
}

/// Class of every wall and slab of the current plan, plus the slabs of the level below that
/// the range reaches. Built once per model revision, never per frame.
#[derive(Debug, Default, PartialEq)]
pub struct LevelClasses {
    /// By index in `Editor::walls`.
    pub walls: Vec<RangeClass>,
    /// By index in `Editor::slabs`.
    pub slabs: Vec<RangeClass>,
    /// Slabs of the level below inside the primary range or the view depth: (rings, class).
    pub below: Vec<(Vec<Vec<V2>>, RangeClass)>,
}

impl LevelClasses {
    pub fn wall(&self, i: usize) -> RangeClass {
        self.walls.get(i).copied().unwrap_or(RangeClass::Cut)
    }
    pub fn slab(&self, i: usize) -> RangeClass {
        self.slabs.get(i).copied().unwrap_or(RangeClass::Projection)
    }
}

/// Classes for `ed` under `r`. The default range shows no slab of the level below.
pub fn level_classes(ed: &Editor, r: &ViewRange) -> LevelClasses {
    let el = ed.elevation;
    let mut c = LevelClasses {
        walls: ed
            .walls
            .iter()
            .map(|w| {
                let (lo, hi) = wall_span(w, el);
                classify(r, el, lo, hi)
            })
            .collect(),
        slabs: ed
            .slabs
            .iter()
            .map(|s| {
                let (lo, hi) = slab_span(s, el);
                classify(r, el, lo, hi)
            })
            .collect(),
        below: vec![],
    };
    if *r != ViewRange::default() {
        if let Some((lv, ..)) = ed.level_below() {
            let (slabs, below_el) = crate::editor::cw::slabs_of(&ed.model, lv);
            for s in slabs {
                let (lo, hi) = slab_span(&s, below_el);
                let k = classify(r, el, lo, hi);
                if matches!(k, RangeClass::Projection | RangeClass::Beyond) {
                    c.below.push((s.rings, k));
                }
            }
        }
    }
    c
}

/// Classes of the editor's current plan; recomputed only when the model changed.
pub fn classes(ed: &Editor) -> Rc<LevelClasses> {
    thread_local! {
        static CACHE: std::cell::RefCell<Option<(Key, Rc<LevelClasses>)>> = const { std::cell::RefCell::new(None) };
    }
    let c = ed.model.sql();
    let key: Key = (
        c as *const rusqlite::Connection as usize,
        c.total_changes(),
        ed.revision,
        ed.level,
    );
    if let Some((k, v)) = CACHE.with(|x| x.borrow().clone()) {
        if k == key {
            return v;
        }
    }
    let v = Rc::new(level_classes(ed, &range(ed)));
    CACHE.with(|x| *x.borrow_mut() = Some((key, v.clone())));
    v
}

/// View range of the editor's current plan; reads the database once per revision.
pub fn range(ed: &Editor) -> ViewRange {
    thread_local! {
        static CACHE: std::cell::RefCell<RangeCache> = std::cell::RefCell::new(RangeCache::default());
    }
    let c = ed.model.sql();
    let key: Key = (
        c as *const rusqlite::Connection as usize,
        c.total_changes(),
        ed.revision,
        ed.level,
    );
    CACHE.with(|x| x.borrow_mut().get(key, || ed.model.plan_view_range(ed.level)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{TemplateField, ViewKey};

    fn rg(cut: f64, bottom: f64, depth: f64) -> ViewRange {
        ViewRange {
            cut,
            bottom,
            depth,
            top: None,
        }
    }

    #[test]
    fn above_top_hidden() {
        let r = ViewRange {
            top: Some(2.0),
            ..rg(1.2, 0.0, 0.0)
        };
        assert_eq!(classify(&r, 0.0, 1.5, 1.9), RangeClass::Beyond);
        assert_eq!(classify(&r, 0.0, 2.5, 3.0), RangeClass::Hidden);
        assert_eq!(classify(&r, 3.0, 4.5, 4.9), RangeClass::Beyond, "relative to the level");
        assert_eq!(classify(&r, 0.0, 0.0, 3.0), RangeClass::Cut, "a cut wall is never hidden");
    }

    /// One range for every consumer: a range set in the drawing quality window (four planes)
    /// changes the plan classes, picking and the reload cut; undo restores all of them.
    #[test]
    fn drawing_quality_range_reaches_every_consumer() {
        use ddc_model::drawq::ViewRange as Planes;
        let mut e = Editor::new_in_memory().unwrap();
        // wall 0: top at 0.9 m; wall 1: 2.5..3.0 m
        add_wall(&mut e, 0.0, 0.9);
        add_wall(&mut e, 2.5, 0.5);
        assert_eq!(classes(&e).wall(0), RangeClass::Projection);
        assert_eq!(classes(&e).wall(1), RangeClass::Beyond);
        let lv = e.level;
        let steps = e.undo_len();
        let p = Planes {
            top: 2.0,
            cut: 0.5,
            bottom: 0.0,
            depth: 0.0,
        };
        e.run_op(crate::editor::OpFn(std::sync::Arc::new(move |m| {
            m.set_plan_range(lv, p)
        })))
        .unwrap();
        assert_eq!(e.undo_len(), steps + 1);
        let r = range(&e);
        assert_eq!((r.cut, r.top), (0.5, Some(2.0)), "plan reads the planes");
        assert_eq!(e.model.plan_view_range(lv), r, "view graphics window reads the same");
        assert_eq!(classes(&e).wall(0), RangeClass::Cut);
        assert_eq!(classes(&e).wall(1), RangeClass::Hidden, "above the top");
        // a hidden wall is not pickable; back in range after undo it is
        let w1 = crate::editor::Sel::Wall(e.walls[1].id);
        let only_w1 = move |s: crate::editor::Sel| s == w1;
        let at = ddc_geom::v2(2.0, 0.0);
        assert_eq!(e.pick_at(at, &only_w1), None);
        e.undo();
        assert_eq!(e.pick_at(at, &only_w1), Some(w1));
        assert_eq!(range(&e), ViewRange::default());
        assert_eq!(classes(&e).wall(0), RangeClass::Projection);
        assert_eq!(classes(&e).wall(1), RangeClass::Beyond);
    }

    #[test]
    fn default_range_draws_as_before() {
        let d = ViewRange::default();
        // same split the plan used with the fixed cut: crossing, below, above; nothing hidden
        assert_eq!(classify(&d, 0.0, 0.0, 3.0), RangeClass::Cut);
        assert_eq!(classify(&d, 0.0, 0.0, 0.9), RangeClass::Projection);
        assert_eq!(classify(&d, 0.0, -5.0, -4.0), RangeClass::Projection);
        assert_eq!(classify(&d, 3.0, 2.0, 2.9), RangeClass::Projection);
        assert_eq!(classify(&d, 0.0, 2.5, 3.0), RangeClass::Beyond);
        assert_eq!(classify(&d, 3.0, 1.0, 5.0), RangeClass::Cut);
        // walls and slabs under the default range: tall wall cut, slab a projection, no
        // slab of the level below
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        add_wall(&mut e, 0.0, 3.0);
        add_slab(&mut e, l1, 0.0);
        let l2 = e.model.add_level("L2", 3.0).unwrap();
        e.set_level(l1);
        let c = level_classes(&e, &ViewRange::default());
        assert_eq!((c.wall(0), c.slab(0)), (RangeClass::Cut, RangeClass::Projection));
        e.set_level(l2);
        assert!(level_classes(&e, &ViewRange::default()).below.is_empty());
    }

    fn add_wall(e: &mut Editor, base_offset: f64, height: f64) {
        let t = e.types[1].id;
        e.model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: e.level,
                base_offset,
                height,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
    }

    fn add_slab(e: &mut Editor, level: i64, offset: f64) {
        let st = e
            .model
            .add_slab_type(
                "S",
                &[ddc_model::LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        e.model
            .add_slab(&ddc_model::SlabSpec {
                level,
                slab_type: st,
                offset,
                outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                holes: vec![],
            })
            .unwrap();
        e.reload().unwrap();
    }

    #[test]
    fn low_wall_drawn_projection() {
        let mut e = Editor::new_in_memory().unwrap();
        // top at 0.9 m, below the 1.2 m cut: a projection, not a cut section
        add_wall(&mut e, 0.0, 0.9);
        assert_eq!(level_classes(&e, &ViewRange::default()).wall(0), RangeClass::Projection);
        // reaching the cut plane stays a cut
        assert_eq!(wall_span(&e.walls[0], 0.0), (0.0, 0.9));
        e.walls[0].height = 1.5;
        assert_eq!(level_classes(&e, &ViewRange::default()).wall(0), RangeClass::Cut);
        // a sloped top counts at its highest point
        e.walls[0].height = 0.5;
        e.walls[0].top = Some(vec![(0.0, 0.5), (4.0, 1.4)]);
        assert_eq!(wall_span(&e.walls[0], 0.0).1, 1.4);
        assert_eq!(level_classes(&e, &ViewRange::default()).wall(0), RangeClass::Cut);
        // raised above the cut: beyond; sunk below the depth: hidden, never silently cut
        e.walls[0].top = None;
        e.walls[0].base_offset = 2.0;
        e.walls[0].height = 1.0;
        assert_eq!(level_classes(&e, &ViewRange::default()).wall(0), RangeClass::Beyond);
        e.walls[0].base_offset = -5.0;
        e.walls[0].height = 1.0;
        assert_eq!(level_classes(&e, &rg(1.2, 0.0, 0.5)).wall(0), RangeClass::Hidden);
    }

    #[test]
    fn slab_below_in_depth_beyond() {
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        add_slab(&mut e, l1, 0.0);
        let l2 = e.model.add_level("L2", 3.0).unwrap();
        e.set_level(l2);
        let set = |e: &mut Editor, r: ViewRange| {
            e.model.set_view_range(ViewKey::Plan(l2), r).unwrap();
            e.reload().unwrap();
            classes(e)
        };
        // the slab top is 3 m under the plan: inside a 3.5 m depth, outside a 1 m one
        let c = set(&mut e, rg(1.2, 0.0, 3.5));
        assert_eq!(c.below.len(), 1);
        assert_eq!(c.below[0].1, RangeClass::Beyond);
        assert_eq!(c.below[0].0[0].len(), 4);
        assert!(set(&mut e, rg(1.2, 0.0, 1.0)).below.is_empty());
        // classified once per revision
        let a = classes(&e);
        assert!(Rc::ptr_eq(&a, &classes(&e)));
    }

    #[test]
    fn element_below_bottom_hidden() {
        let r = rg(1.2, 0.0, 0.5);
        assert_eq!(classify(&r, 0.0, -2.0, -1.0), RangeClass::Hidden);
        // no depth: everything below the bottom is out of the view (the all-default range is
        // exempt: it hides nothing, so a bottom a little lower stands for "no depth" here)
        assert_eq!(
            classify(&rg(1.2, -0.05, 0.0), 0.0, -0.3, -0.1),
            RangeClass::Hidden
        );
        assert_eq!(
            classify(&rg(1.2, 0.0, 0.0), 0.0, -0.3, -0.1),
            RangeClass::Projection
        );
        // span reaching into the primary range stays visible
        assert_eq!(classify(&r, 0.0, -2.0, 0.2), RangeClass::Projection);
        // levels are relative to the plan's elevation
        assert_eq!(classify(&r, 3.0, 0.0, 1.0), RangeClass::Hidden);
    }

    #[test]
    fn slab_below_within_depth_beyond() {
        let r = rg(1.2, 0.0, 1.0);
        assert_eq!(classify(&r, 0.0, -0.4, -0.1), RangeClass::Beyond);
        assert_eq!(
            classify(&r, 0.0, -1.5, -1.0),
            RangeClass::Beyond,
            "depth edge"
        );
        assert_eq!(classify(&r, 0.0, -1.5, -1.01), RangeClass::Hidden);
        // a raised bottom puts the lower part of the primary range into the depth band
        assert_eq!(
            classify(&rg(1.2, 0.5, 0.5), 0.0, 0.0, 0.3),
            RangeClass::Beyond
        );
    }

    #[test]
    fn range_read_once_per_revision() {
        let mut c = RangeCache::default();
        let mut reads = 0;
        let a = rg(0.9, -0.2, 0.4);
        for _ in 0..100 {
            let got = c.get((1, 5, 2, 7), || {
                reads += 1;
                a
            });
            assert_eq!(got, a);
        }
        assert_eq!(reads, 1, "a frame must not query the database");
        // a change count, revision or level step reads again
        for k in [(1, 6, 2, 7), (1, 6, 3, 7), (1, 6, 3, 8), (2, 6, 3, 8)] {
            c.get(k, || {
                reads += 1;
                a
            });
            c.get(k, || panic!("second read for the same key"));
        }
        assert_eq!(reads, 5);
    }

    fn set_range_op(ed: &mut Editor, r: ViewRange) -> Result<(), ddc_model::Error> {
        let v = ViewKey::Plan(ed.level);
        ed.run_op(crate::editor::OpFn(std::sync::Arc::new(move |m| {
            m.set_view_range(v, r)
        })))
    }

    #[test]
    fn cut_height_change_is_one_undo_and_refreshes_caches() {
        let mut e = Editor::new_in_memory().unwrap();
        // wall top at 0.9 m: a projection under the default cut at 1.2 m
        add_wall(&mut e, 0.0, 0.9);
        assert_eq!(classes(&e).wall(0), RangeClass::Projection);
        let steps = e.undo_len();
        set_range_op(&mut e, rg(0.5, 0.0, 0.0)).unwrap();
        assert_eq!(e.undo_len(), steps + 1, "one undo step per change");
        assert_eq!(range(&e), rg(0.5, 0.0, 0.0));
        assert_eq!(classes(&e).wall(0), RangeClass::Cut, "class cache follows the range");
        e.undo();
        assert_eq!(e.undo_len(), steps);
        assert_eq!(range(&e), ViewRange::default());
        assert_eq!(classes(&e).wall(0), RangeClass::Projection);
    }

    #[test]
    fn bottom_above_cut_refused_with_reason() {
        let mut e = Editor::new_in_memory().unwrap();
        let steps = e.undo_len();
        let err = set_range_op(&mut e, rg(0.4, 0.8, 0.0)).unwrap_err().to_string();
        // the reason is a stable key, the UI maps it to en/ru text
        assert!(err.contains("cut_not_above_bottom"), "{err}");
        assert_eq!(e.undo_len(), steps, "refused change leaves no undo step");
        assert_eq!(range(&e), ViewRange::default());
    }

    #[test]
    fn template_locked_range_used() {
        let mut ed = Editor::new_in_memory().unwrap();
        let l1 = ed.level;
        let l2 = ed.model.add_level("L2", 3.0).unwrap();
        let (p1, p2) = (ViewKey::Plan(l1), ViewKey::Plan(l2));
        let r = rg(0.8, -0.4, 0.6);
        ed.model.set_view_range(p1, r).unwrap();
        let t = ed
            .model
            .template_from_view("Plan", p1, &[TemplateField::ViewRange])
            .unwrap();
        ed.model.link_view_template(Some(t), &[p2]).unwrap();
        ed.level = l2;
        assert_eq!(range(&ed), r, "linked plan reads the template range");
        // editing the template reaches the cached value
        let mut spec = ed.model.view_templates().unwrap()[0].spec.clone();
        spec.range = rg(1.6, 0.1, 0.0);
        ed.model
            .edit_view_template(t, "Plan", &[TemplateField::ViewRange], &spec)
            .unwrap();
        assert_eq!(range(&ed), rg(1.6, 0.1, 0.0));
        ed.level = l1;
        assert_eq!(range(&ed), r);
    }
}
