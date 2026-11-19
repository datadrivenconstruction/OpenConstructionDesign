// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saved measurements in the editor: the feature under the cursor (wall face, mass face by its
//! history tag, slab side, room) as a model `Anchor`, the tool's picks, and the quick-measure
//! lens. The lens only evaluates (`Model::eval_measure_spec`); it never writes the journal.
//! Plan, section and 3D build the same `MeasureSpec` from the same anchors (`build_spec`).

pub use ddc_model::MeasureAnchor as Anchor;
use ddc_model::{DimRef, MeasureKind, MeasureSpec, MeasureValue, Model, ViewKey};
use ddc_render::glam::Vec3;

use super::pick::{Bodies, Cat, Hit3, Query, Target};
use crate::scene_ids::EXT_MASS;

/// A feature under the cursor: what it is, the anchor to save, where, and its normal.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureHit {
    pub target: Target,
    pub anchor: Anchor,
    pub p: Vec3,
    pub n: Vec3,
}

/// A snap with the measurable anchor under it. `Snap` stays `Copy`, so the anchor travels beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchoredSnap {
    pub snap: super::snap::Snap,
    pub anchor: Option<Anchor>,
}

/// The anchor of a face snap: the feature of the hit the snap sits on; other snaps carry none.
pub fn anchored(m: &Model, snap: super::snap::Snap, hits: &[Hit3]) -> AnchoredSnap {
    let anchor = (snap.kind == super::snap::SnapKind::Face)
        .then(|| hits.iter().find(|h| h.point == snap.p))
        .flatten()
        .and_then(|h| feature_of_hit(m, h))
        .map(|f| f.anchor);
    AnchoredSnap { snap, anchor }
}

/// Face (0 or 1) of a straight wall nearest to a plan point: (face, gap to it, position along 0..1).
fn wall_face_near(m: &Model, wall: i64, p: (f64, f64)) -> Option<(u8, f64, f64)> {
    #[allow(clippy::type_complexity)]
    let (a, b, off, total, bulge): ((f64, f64), (f64, f64), Option<f64>, f64, f64) = m
        .sql()
        .query_row(
            "SELECT ax,ay,bx,by,offset0,bulge,(SELECT COALESCE(SUM(thickness),0) \
             FROM wall_type_layer WHERE wtype=wall.wtype) FROM wall WHERE id=?1",
            [wall],
            |r| {
                Ok((
                    (r.get(0)?, r.get(1)?),
                    (r.get(2)?, r.get(3)?),
                    r.get(4)?,
                    r.get(6)?,
                    r.get(5)?,
                ))
            },
        )
        .ok()?;
    if bulge != 0.0 {
        return None;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return None;
    }
    let (n, d) = ((-dy / len, dx / len), (dx / len, dy / len));
    let off0 = off.unwrap_or(-total / 2.0);
    let dist = (p.0 - a.0) * n.0 + (p.1 - a.1) * n.1;
    let along = (p.0 - a.0) * d.0 + (p.1 - a.1) * d.1;
    let (f, gap) = [0u8, 1]
        .into_iter()
        .map(|f| (f, (dist - off0 - f as f64 * total).abs()))
        .min_by(|x, y| x.1.total_cmp(&y.1))?;
    Some((f, gap, along / len))
}

/// Wall-face anchor from a point on the face (the same for a plan click and a 3D hit).
pub fn wall_face_anchor(m: &Model, wall: i64, p: (f64, f64)) -> Option<Anchor> {
    let (face, _, _) = wall_face_near(m, wall, p)?;
    Some(Anchor::Dim(DimRef::WallEnd { wall, face, end: 0 }))
}

/// Plan or section click at a point of a level: the wall face within `tol` metres.
pub fn plan_anchor(m: &Model, level: i64, p: (f64, f64), tol: f64) -> Option<Anchor> {
    // a box prefilter keeps the scan to the walls near the cursor (wall thickness under 1 m)
    let pad = tol + 1.0;
    let mut st = m
        .sql()
        .prepare_cached(
            "SELECT id FROM wall WHERE level=?1 AND MAX(ax,bx)>=?2 AND MIN(ax,bx)<=?3              AND MAX(ay,by)>=?4 AND MIN(ay,by)<=?5 ORDER BY id",
        )
        .ok()?;
    let ids: Vec<i64> = st
        .query_map(
            rusqlite::params![level, p.0 - pad, p.0 + pad, p.1 - pad, p.1 + pad],
            |r| r.get(0),
        )
        .ok()?
        .flatten()
        .collect();
    let best = ids
        .into_iter()
        .filter_map(|w| wall_face_near(m, w, p).map(|f| (w, f)))
        .filter(|(_, (_, gap, t))| *gap <= tol && (-0.02..=1.02).contains(t))
        .min_by(|a, b| a.1 .1.total_cmp(&b.1 .1))?;
    Some(Anchor::Dim(DimRef::WallEnd {
        wall: best.0,
        face: best.1 .0,
        end: 0,
    }))
}

/// The feature a hit lies on: wall face, mass face by tag, slab top/bottom, room.
pub fn feature_of_hit(m: &Model, h: &Hit3) -> Option<FeatureHit> {
    let (p, n) = (h.point, h.normal);
    let anchor = match h.target.cat {
        Cat::Wall => wall_face_anchor(m, h.target.id, (p.x as f64, p.y as f64))?,
        Cat::Slab => Anchor::SlabFace {
            kind: "slab".into(),
            id: h.target.id,
            top: n.z > 0.0,
        },
        Cat::Room => Anchor::Room { id: h.target.id },
        Cat::Ext(c) if c == EXT_MASS => {
            let q = [p.x as f64, p.y as f64, p.z as f64];
            let face = m.mass_faces(h.target.id).ok()?.into_iter().find(|f| {
                let Some(o) = f.outline.first().and_then(|o| o.first()).copied() else {
                    return false;
                };
                let k = [f.normal.0, f.normal.1, f.normal.2];
                let dot = k[0] * n.x as f64 + k[1] * n.y as f64 + k[2] * n.z as f64;
                let d = (q[0] - o.0) * k[0] + (q[1] - o.1) * k[1] + (q[2] - o.2) * k[2];
                dot > 0.99 && d.abs() < 5e-3
            })?;
            m.mass_face_anchor(h.target.id, face.tag).ok()?
        }
        _ => return None,
    };
    Some(FeatureHit {
        target: h.target,
        anchor,
        p,
        n,
    })
}

impl Bodies {
    /// Nearest feature along a ray (the first hit that is a measurable feature).
    pub fn feature_at(
        &self,
        m: &Model,
        scene: &ddc_render::Scene,
        ray: &ddc_render::Ray,
        q: &Query,
    ) -> Option<FeatureHit> {
        self.hits_in(scene, ray, q).iter().find_map(|h| feature_of_hit(m, h))
    }
}

/// What the tool measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Distance,
    Clear,
    Angle,
    Area,
    Perimeter,
    Volume,
    Slope,
    /// The point chain (`measure::Measure`); writes nothing.
    Chain,
}

impl Mode {
    pub fn kind(self) -> Option<MeasureKind> {
        Some(match self {
            Mode::Distance => MeasureKind::Distance,
            Mode::Clear => MeasureKind::Clear,
            Mode::Angle => MeasureKind::Angle,
            Mode::Area => MeasureKind::Area,
            Mode::Perimeter => MeasureKind::Perimeter,
            Mode::Volume => MeasureKind::Volume,
            Mode::Slope => MeasureKind::Slope,
            Mode::Chain => return None,
        })
    }

    /// Picks that complete a measurement.
    pub fn arity(self) -> usize {
        match self {
            Mode::Area | Mode::Perimeter | Mode::Volume => 1,
            Mode::Chain => 0,
            _ => 2,
        }
    }
}

/// The record every view saves: one spec from the same anchors, whichever view picked them.
pub fn build_spec(m: &Model, mode: Mode, view: ViewKey, anchors: &[Anchor]) -> Option<MeasureSpec> {
    Some(MeasureSpec {
        kind: mode.kind()?,
        view,
        pins: m.measure_pins(anchors).ok()?,
        label: String::new(),
    })
}

/// Picked features of the running measurement; Esc clears.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picks {
    pub anchors: Vec<Anchor>,
}

impl Picks {
    /// A click on a feature; the spec of the measurement this click completes, if any.
    pub fn click(&mut self, m: &Model, mode: Mode, view: ViewKey, a: Anchor) -> Option<MeasureSpec> {
        self.anchors.push(a);
        if self.anchors.len() < mode.arity() {
            return None;
        }
        let spec = build_spec(m, mode, view, &self.anchors);
        self.anchors.clear();
        spec
    }

    pub fn clear(&mut self) {
        self.anchors.clear();
    }
}

/// Quick measurement under the cursor: computed, never written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lens {
    Off,
    Hover(MeasureValue),
}

impl Lens {
    /// The picks so far plus the hovered feature, when they complete a measurement; otherwise
    /// the hovered feature's own area.
    pub fn hover(m: &Model, mode: Mode, view: ViewKey, picks: &Picks, hit: Option<&FeatureHit>) -> Lens {
        hit.map_or(Lens::Off, |h| Lens::over(m, mode, view, picks, &h.anchor))
    }

    /// The same for an anchor found some other way (a plan click point).
    pub fn over(m: &Model, mode: Mode, view: ViewKey, picks: &Picks, anchor: &Anchor) -> Lens {
        let mut a = picks.anchors.clone();
        a.push(anchor.clone());
        let spec = if a.len() == mode.arity() {
            build_spec(m, mode, view, &a)
        } else {
            build_spec(m, Mode::Area, view, std::slice::from_ref(anchor))
        };
        spec.map_or(Lens::Off, |s| Lens::Hover(m.eval_measure_spec(&s)))
    }
}

/// Tool state: what is measured and the picks so far. `Chain` keeps the point chain.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub mode: Mode,
    pub picks: Picks,
}

impl Default for State {
    fn default() -> Self {
        State {
            mode: Mode::Chain,
            picks: Picks::default(),
        }
    }
}

/// Saved measurements of the 3D view and of the current level's plan, over the scene.
pub fn draw3d(
    painter: &eframe::egui::Painter,
    cam: &super::math::Cam,
    ed: &crate::editor::Editor,
    fg: eframe::egui::Color32,
    bg: eframe::egui::Color32,
    warn: eframe::egui::Color32,
) {
    for view in [ViewKey::ThreeD, ViewKey::Plan(ed.level)] {
        let Ok(list) = ed.model.measures_in_view(view) else {
            continue;
        };
        for i in &list {
            let pts: Vec<Vec3> = i
                .points
                .iter()
                .map(|q| Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32))
                .collect();
            let text = crate::tools::measure::info_text(ed, i);
            let col = if i.state == ddc_model::MeasureState::Live { fg } else { warn };
            if let [a, b, ..] = pts[..] {
                super::paint::dim(painter, cam, a, b, &text, col, bg);
            } else if let Some(a) = pts.first().and_then(|a| cam.to_screen(*a)) {
                super::paint::label(painter, a, &text, col, bg, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ddc_model::{Form, LayerSpec, MassSpec, WallSpec};

    use super::*;

    fn room() -> (Model, i64, Vec<i64>) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "B".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        let specs: Vec<WallSpec> = (0..4)
            .map(|k| WallSpec {
                a: c[k],
                b: c[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .collect();
        let ids = m.add_walls(&specs).unwrap();
        (m, lv, ids)
    }

    fn hit(cat: Cat, id: i64, p: Vec3, n: Vec3) -> Hit3 {
        Hit3 {
            body: 0,
            target: Target::new(cat, id),
            t: 1.0,
            point: p,
            normal: n,
        }
    }

    #[test]
    fn feature_at_wall_face_gives_dimref() {
        let (m, _, w) = room();
        // inner face of the south wall (y = 0.2), seen from inside
        let f = feature_of_hit(&m, &hit(Cat::Wall, w[0], Vec3::new(3.0, 0.2, 1.5), Vec3::Y)).unwrap();
        assert_eq!(
            f.anchor,
            Anchor::Dim(DimRef::WallEnd {
                wall: w[0],
                face: 1,
                end: 0
            })
        );
        let f = feature_of_hit(&m, &hit(Cat::Wall, w[0], Vec3::new(3.0, 0.0, 1.5), -Vec3::Y)).unwrap();
        assert!(matches!(f.anchor, Anchor::Dim(DimRef::WallEnd { face: 0, .. })));
    }

    #[test]
    fn face_snap_carries_the_anchor() {
        use super::super::snap::{Snap, SnapKind};
        let (m, _, w) = room();
        let h = hit(Cat::Wall, w[0], Vec3::new(3.0, 0.2, 1.5), Vec3::Y);
        let face = Snap { kind: SnapKind::Face, p: h.point, px: 0.0, refs: [None, None] };
        let a = anchored(&m, face, &[h]);
        assert!(matches!(a.anchor, Some(Anchor::Dim(DimRef::WallEnd { face: 1, .. }))), "{:?}", a.anchor);
        let vtx = Snap { kind: SnapKind::Vertex, p: h.point, px: 0.0, refs: [None, None] };
        assert_eq!(anchored(&m, vtx, &[h]).anchor, None);
    }

    #[test]
    fn feature_at_mass_face_gives_ftag() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let id = m
            .add_mass(&MassSpec {
                level: lv,
                name: "b".into(),
                void: false,
                inplace: false,
                form: Form::Extrude {
                    ring: vec![(0., 0.), (10., 0.), (10., 6.), (0., 6.)],
                    z0: 0.0,
                    h: 9.0,
                    ids: vec![],
                },
            })
            .unwrap();
        let h = hit(Cat::Ext(EXT_MASS), id, Vec3::new(10.0, 3.0, 4.0), Vec3::X);
        let f = feature_of_hit(&m, &h).unwrap();
        let want = m.mass_face_anchor(id, ddc_geom::csg::ftag(1, 3)).unwrap();
        assert_eq!(f.anchor, want);
        assert!(matches!(f.anchor, Anchor::MassFace { ftag, .. } if ftag == ddc_geom::csg::ftag(1, 3)));
    }

    #[test]
    fn lens_writes_no_journal() {
        let (m, lv, w) = room();
        let view = ViewKey::Plan(lv);
        let before = m.undo_kept().len();
        let h1 = feature_of_hit(&m, &hit(Cat::Wall, w[0], Vec3::new(3.0, 0.2, 1.5), Vec3::Y)).unwrap();
        // one face alone: its own reading, some value or none, but a lens
        assert!(matches!(
            Lens::hover(&m, Mode::Clear, view, &Picks::default(), Some(&h1)),
            Lens::Hover(_)
        ));
        let mut p = Picks::default();
        p.anchors.push(h1.anchor.clone());
        let h2 = feature_of_hit(&m, &hit(Cat::Wall, w[2], Vec3::new(3.0, 3.8, 1.5), -Vec3::Y)).unwrap();
        match Lens::hover(&m, Mode::Clear, view, &p, Some(&h2)) {
            Lens::Hover(MeasureValue::Ok(v)) => assert!((v - 3.6).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        assert_eq!(m.undo_kept().len(), before);
        assert!(m.measures_in_view(view).unwrap().is_empty(), "nothing saved");
        assert_eq!(Lens::hover(&m, Mode::Clear, view, &p, None), Lens::Off);
    }

    #[test]
    fn measure_tool_plan_and_3d_same_record() {
        let (m, lv, w) = room();
        let view = ViewKey::Plan(lv);
        let mut p3 = Picks::default();
        let a = feature_of_hit(&m, &hit(Cat::Wall, w[0], Vec3::new(3.0, 0.2, 1.5), Vec3::Y)).unwrap();
        let b = feature_of_hit(&m, &hit(Cat::Wall, w[2], Vec3::new(2.0, 3.8, 0.5), -Vec3::Y)).unwrap();
        assert!(p3.click(&m, Mode::Clear, view, a.anchor).is_none());
        let s3 = p3.click(&m, Mode::Clear, view, b.anchor).unwrap();
        let mut pp = Picks::default();
        let a = plan_anchor(&m, lv, (1.0, 0.21), 0.05).unwrap();
        let b = plan_anchor(&m, lv, (5.0, 3.79), 0.05).unwrap();
        assert!(pp.click(&m, Mode::Clear, view, a).is_none());
        let sp = pp.click(&m, Mode::Clear, view, b).unwrap();
        // the same record: kind, view, anchors, value
        assert_eq!(s3.kind, sp.kind);
        assert_eq!(s3.view, sp.view);
        let anchors = |s: &MeasureSpec| s.pins.iter().map(|p| p.anchor.clone()).collect::<Vec<_>>();
        assert_eq!(anchors(&s3), anchors(&sp));
        assert_eq!(m.eval_measure_spec(&s3), m.eval_measure_spec(&sp));
        assert!(pp.anchors.is_empty(), "picks reset after the record");
        assert!(plan_anchor(&m, lv, (3.0, 2.0), 0.05).is_none());
    }

    /// The plan tool itself: two clicks save the record the 3D picks would, one journal step.
    #[test]
    fn plan_tool_clicks_save_the_3d_record() {
        use crate::tools::measure::{MeasureOut, MeasureTool};
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let wt = ed
            .model
            .add_wall_type("PT", &[LayerSpec { material: "B".into(), thickness: 0.2 }])
            .unwrap();
        let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        let specs: Vec<WallSpec> = (0..4)
            .map(|k| WallSpec {
                a: c[k],
                b: c[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .collect();
        ed.model.add_walls(&specs).unwrap();
        let steps = ed.model.journal_len().unwrap();
        let mut t = MeasureTool::new(Mode::Clear);
        assert!(matches!(t.click(&mut ed, ddc_geom::v2(1.0, 0.21), 0.05), MeasureOut::Nothing));
        assert!(matches!(t.click(&mut ed, ddc_geom::v2(5.0, 3.79), 0.05), MeasureOut::Done));
        assert!(matches!(t.click(&mut ed, ddc_geom::v2(3.0, 2.0), 0.05), MeasureOut::Refused(_)));
        let list = ed.model.measures_in_view(ViewKey::Plan(lv)).unwrap();
        assert_eq!(list.len(), 1);
        assert!((match list[0].value { MeasureValue::Ok(v) => v, o => panic!("{o:?}") } - 3.6).abs() < 1e-9);
        assert_eq!(ed.model.journal_len().unwrap(), steps + 1);
        assert!(t.picks.anchors.is_empty());
    }

    /// The quick lens at a cursor over 10 000 walls stays within a millisecond and writes nothing.
    #[test]
    fn lens_budget_10k_walls() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type("W", &[LayerSpec { material: "B".into(), thickness: 0.2 }])
            .unwrap();
        let specs: Vec<WallSpec> = (0..10_000)
            .map(|k| WallSpec {
                a: ((k % 100) as f64 * 6.0, (k / 100) as f64 * 6.0),
                b: ((k % 100) as f64 * 6.0 + 5.0, (k / 100) as f64 * 6.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .collect();
        let w = m.add_walls(&specs).unwrap();
        let first = plan_anchor(&m, lv, (2.0, 0.21), 0.05).unwrap();
        let mut picks = Picks::default();
        picks.anchors.push(first);
        let steps = m.journal_len().unwrap();
        let k: u128 = if cfg!(debug_assertions) { 20 } else { 1 };
        let mut worst = 0u128;
        for i in 0..20 {
            let t = std::time::Instant::now();
            let a = plan_anchor(&m, lv, (2.0, 6.0 * (50 + i) as f64 + 0.21), 0.05).unwrap();
            let l = Lens::over(&m, Mode::Distance, ViewKey::Plan(lv), &picks, &a);
            worst = worst.max(t.elapsed().as_micros());
            assert!(matches!(l, Lens::Hover(MeasureValue::Ok(_))), "{l:?}");
        }
        assert!(worst < 1000 * k, "lens took {worst} us");
        assert_eq!(m.journal_len().unwrap(), steps, "the lens writes nothing");
        let _ = w;
    }
}
