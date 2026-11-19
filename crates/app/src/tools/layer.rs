//! Plan layer for model-tool elements: grids, reference planes, columns, beams,
//! roofs, stairs, networks, shafts, ceilings, components. Cached by (revision, level); phase
//! of the level's plan view: future and earlier-demolished elements are left out, the rest is styled.

use ddc_geom::{v2, V2};
use ddc_model::{MepDetail, MepPrim, Model, PhaseShow, ViewKey};

/// Outline class for the drawing style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cls {
    /// Imported mesh section: fill (consecutive triangles), outline, hole, edges.
    MeshFill,
    MeshCut,
    MeshHole,
    MeshEdge,
    /// Outline of an imported mesh above the plan cut (dashed).
    MeshOverhead,
    Column,
    Beam,
    Roof,
    Stair,
    Mep([u8; 3]),
    /// Network axis (dashed).
    MepAxis([u8; 3]),
    /// Line of the duct itself under the insulation.
    MepIns([u8; 3]),
    Shaft,
    Ceiling,
    Rail,
    /// Shape: section at plan height and edges below/above.
    Mass,
    /// Hidden foundation lines (dashed).
    Footing,
    /// Room separator (dashed).
    RoomSep,
    /// Zone rooms (accent fill).
    Zone,
    /// Component symbol (furniture, plumbing fixture) in plan.
    Comp,
}

#[derive(Default)]
pub struct Layer {
    key: Option<(u64, i64)>,
    /// (name, start, end) — plan-view extents (2D, if set), otherwise model extents.
    pub grids: Vec<(String, V2, V2)>,
    /// Grid line bubbles (start, end), parallel to `grids`.
    pub grid_bubbles: Vec<(bool, bool)>,
    pub refs: Vec<(V2, V2)>,
    /// (class, points, closed, phase graphics).
    pub polys: Vec<(Cls, Vec<V2>, bool, PhaseShow)>,
    /// Level stair outlines: (id, outer outline).
    pub stairs: Vec<(i64, Vec<V2>)>,
    /// Component insertion points and phase graphics.
    pub comps: Vec<(V2, PhaseShow)>,
    /// Level room separators (snapping).
    pub seps: Vec<(V2, V2)>,
    /// Plan labels: ramp slope, zone name.
    pub labels: Vec<(V2, String)>,
}

fn p(t: (f64, f64)) -> V2 {
    v2(t.0, t.1)
}

fn ids(m: &Model, sql: &str, level: i64) -> Vec<i64> {
    let Ok(mut st) = m.sql().prepare(sql) else {
        return vec![];
    };
    st.query_map([level], |r| r.get(0))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

impl Layer {
    /// Re-read if the model or level changed.
    pub fn sync(&mut self, m: &Model, level: i64, rev: u64) {
        if self.key == Some((rev, level)) {
            return;
        }
        *self = Layer::load(m, level);
        self.key = Some((rev, level));
    }

    fn load(m: &Model, level: i64) -> Layer {
        let gfx = m.view_paint(ViewKey::Plan(level)).ok();
        let no_grid = gfx
            .as_ref()
            .is_some_and(|g| g.hidden_cats.iter().any(|c| c == "grid"));
        let axes: Vec<(String, V2, V2, (bool, bool))> = m
            .plan_grid_axes(level)
            .unwrap_or_default()
            .into_iter()
            .filter(|_| !no_grid)
            .map(|(g, bub)| (g.label, p(g.a), p(g.b), bub))
            .collect();
        let ix = m.phase_index().ok();
        let vp = m.view_phase(ViewKey::Plan(level)).ok();
        // outside the view phase — None: not drawn and not picked by tools
        let show = |kind: &str, id: i64| -> Option<PhaseShow> {
            if gfx.as_ref().is_some_and(|g| g.hidden(kind, id)) {
                return None;
            }
            match (&ix, vp) {
                (Some(ix), Some(vp)) => {
                    Some(ix.show(kind, id, vp)).filter(|s| *s != PhaseShow::Hidden)
                }
                _ => Some(PhaseShow::Normal),
            }
        };
        let mut l = Layer {
            grid_bubbles: axes.iter().map(|g| g.3).collect(),
            grids: axes.into_iter().map(|g| (g.0, g.1, g.2)).collect(),
            refs: m
                .ref_planes()
                .unwrap_or_default()
                .into_iter()
                .map(|r| (p(r.1), p(r.2)))
                .collect(),
            ..Default::default()
        };
        for s in m.struct_plan(level, 1.2).unwrap_or_default() {
            // phase key — the t-band table; structure g6 and roof edges g7 carry no
            // phase (shown as is): bracing/trusses as beams, roof edges as roof
            let (c, kind) = match s.kind {
                _ if s.class == ddc_model::StructClass::Hidden => (Cls::Footing, s.kind),
                "column" => (Cls::Column, "col"),
                "beam" => (Cls::Beam, "beam"),
                "mass" => (Cls::Mass, "mass"),
                "roof" => (Cls::Roof, "roof"),
                k if ddc_model::STRUCT_KINDS.contains(&k) => (Cls::Beam, k),
                k => (Cls::Roof, k),
            };
            let Some(sh) = show(kind, s.id) else { continue };
            l.polys
                .push((c, s.pts.iter().map(|q| p(*q)).collect(), s.closed, sh));
        }
        for id in ids(m, "SELECT id FROM roof WHERE level=?1", level) {
            let Some(sh) = show("roof", id) else { continue };
            // roof openings and dormers
            for o in m
                .host_openings(ddc_model::OpeningHost::Roof(id))
                .unwrap_or_default()
            {
                l.polys
                    .push((Cls::Roof, o.ring.iter().map(|q| p(*q)).collect(), true, sh));
            }
            if let Ok(ddc_model::RoofSpec {
                kind: ddc_model::RoofKind::Footprint { ring, .. },
                ..
            }) = m.roof(id)
            {
                l.polys
                    .push((Cls::Roof, ring.iter().map(|q| p(*q)).collect(), true, sh));
            }
        }
        let st = "SELECT id FROM stair WHERE level=?1 OR top_level=?1";
        for id in ids(m, st, level) {
            let Some(sh) = show("stair", id) else {
                continue;
            };
            let Ok(sp) = m.stair_plan(id, level, 1.2) else {
                continue;
            };
            if let Some(o) = sp.outline.first() {
                l.stairs.push((id, o.clone()));
            }
            for o in sp.outline {
                l.polys.push((Cls::Stair, o, true, sh));
            }
            for (a, b) in sp.below.into_iter().chain(sp.above) {
                l.polys.push((Cls::Stair, vec![a, b], false, sh));
            }
            if sp.arrow.len() > 1 {
                l.polys.push((Cls::Stair, sp.arrow, false, sh));
            }
        }
        for r in m.mep_plan(level, MepDetail::Double).unwrap_or_default() {
            let Some(sh) = show("mep_run", r.run) else {
                continue;
            };
            let (cls, pts, closed) = match r.prim {
                MepPrim::Centerline(v) => (Cls::MepAxis(r.color), v, false),
                MepPrim::Edge(v) => (Cls::Mep(r.color), v, false),
                MepPrim::Rise { outline, .. } | MepPrim::Drop { outline, .. } => {
                    (Cls::Mep(r.color), outline, true)
                }
            };
            l.polys.push((cls, pts, closed, sh));
        }
        for r in m
            .mep_plan_insulation(level, MepDetail::Double)
            .unwrap_or_default()
        {
            let Some(sh) = show("mep_run", r.run) else {
                continue;
            };
            if let MepPrim::Edge(v) = r.prim {
                l.polys.push((Cls::MepIns(r.color), v, false, sh));
            }
        }
        // a shaft is visible on every level it passes through
        let sq = "SELECT s.id FROM shaft s JOIN level b ON b.id=s.level JOIN level t ON t.id=s.top_level \
                  JOIN level l ON l.id=?1 WHERE b.elevation + s.base_offset < l.elevation + 1.2 \
                  AND t.elevation + s.top_offset > l.elevation";
        for id in ids(m, sq, level) {
            let Some(sh) = show("shaft", id) else {
                continue;
            };
            if let Ok(r) = m.shaft_ring(id) {
                l.polys
                    .push((Cls::Shaft, r.iter().map(|q| p(*q)).collect(), true, sh));
            }
        }
        for id in ids(m, "SELECT id FROM ceiling WHERE level=?1", level) {
            let Some(sh) = show("ceiling", id) else {
                continue;
            };
            for r in m.ceiling_view_rings(id).unwrap_or_default() {
                l.polys
                    .push((Cls::Ceiling, r.iter().map(|q| p(*q)).collect(), true, sh));
            }
        }
        for id in ids(m, "SELECT id FROM railing WHERE level=?1", level) {
            let Some(sh) = show("railing", id) else {
                continue;
            };
            if let Ok(r) = m.railing_layout(id) {
                let pts = r.rail.iter().map(|q| v2(q.x, q.y)).collect();
                l.polys.push((Cls::Rail, pts, false, sh));
            }
        }
        for (k, pts, closed) in crate::mesh_tools::plan_polys(m, level) {
            use crate::mesh_tools::PlanCls as P;
            let c = match k {
                P::Fill => Cls::MeshFill,
                P::Cut => Cls::MeshCut,
                P::Hole => Cls::MeshHole,
                P::Edge | P::Beyond => Cls::MeshEdge,
                P::Overhead => Cls::MeshOverhead,
            };
            l.polys.push((c, pts, closed, PhaseShow::Normal));
        }
        // the family's plan symbol; the insertion marker only for one that draws nothing
        let lines = m.components_plan(level, ddc_model::Lod::Medium).unwrap_or_default();
        let mut drawn = std::collections::HashSet::new();
        for p in lines {
            if let ddc_model::ViewPrim::Edge { element, a, b, .. } = p {
                let Some(sh) = show("comp", element) else { continue };
                drawn.insert(element);
                l.polys.push((Cls::Comp, vec![v2(a.0, a.1), v2(b.0, b.1)], false, sh));
            }
        }
        for id in m.components(Some(level)).unwrap_or_default() {
            if drawn.contains(&id) {
                continue;
            }
            let Some(sh) = show("comp", id) else { continue };
            let Ok(c) = m.component(id) else { continue };
            if c.host == ddc_model::CompHost::None {
                l.comps.push((v2(c.at.0, c.at.1), sh));
            }
        }
        for (axis, c, pts, closed) in crate::rebar_scene::plan_polys(m, level) {
            let cls = if axis { Cls::MepAxis(c) } else { Cls::Mep(c) };
            l.polys.push((cls, pts, closed, PhaseShow::Normal));
        }
        super::core_items::load_layer(m, level, &mut l, &show);
        l
    }

    /// Stair whose outline contains the point.
    pub fn stair_at(&self, q: V2) -> Option<i64> {
        self.stairs.iter().find(|(_, o)| inside(q, o)).map(|s| s.0)
    }
}

/// Point inside a polygon (crossing parity).
pub fn inside(q: V2, ring: &[V2]) -> bool {
    let mut c = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + n - 1) % n]);
        if (a.y > q.y) != (b.y > q.y) && q.x < (b.x - a.x) * (q.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{PhaseFilter, ViewPhase};

    fn cols(l: &Layer) -> Vec<PhaseShow> {
        l.polys
            .iter()
            .filter(|p| p.0 == Cls::Column)
            .map(|p| p.3)
            .collect()
    }

    #[test]
    fn column_follows_plan_phase_filter() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let mt = m
            .add_member_type(
                "K1",
                &ddc_model::MemberShape::Rect { b: 0.4, h: 0.4 },
                "Concrete",
            )
            .unwrap();
        let c = m
            .add_column(&ddc_model::ColumnSpec {
                level: lv,
                top_level: None,
                member_type: mt,
                structural: true,
                at: (1.0, 1.0),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        let ps = m.phases().unwrap();
        assert!(cols(&Layer::load(&m, lv))
            .iter()
            .all(|s| *s == PhaseShow::Normal));
        assert!(!cols(&Layer::load(&m, lv)).is_empty());
        // view in the first phase: the column is created later — absent from the layer
        let v = ViewKey::Plan(lv);
        m.set_view_phase(
            v,
            ViewPhase {
                phase: ps[0].id,
                filter: PhaseFilter::Complete,
            },
        )
        .unwrap();
        assert!(cols(&Layer::load(&m, lv)).is_empty());
        // demolished in the view phase, "Demolition" filter — dashed
        m.demolish(&[("col", c)], ps[2].id).unwrap();
        m.set_view_phase(
            v,
            ViewPhase {
                phase: ps[2].id,
                filter: PhaseFilter::Demolition,
            },
        )
        .unwrap();
        let l = Layer::load(&m, lv);
        assert!(!cols(&l).is_empty() && cols(&l).iter().all(|s| *s == PhaseShow::Dashed));
    }
}
