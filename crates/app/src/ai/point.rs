// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Point and ask": what the person pointed at, as data the assistant can use. The context is
//! collected on the UI thread from the live editor (the worker only sees the rendered block),
//! writes nothing to the model, and leaves no trace once the mode is left.

use super::plan::{ElRef, IdOrNew};
use super::rows;
use crate::editor::{Editor, Sel};
use crate::query::Row;
use ddc_geom::math::v2;
use ddc_geom::predicates::point_in_ring;
use serde_json::{json, Value as J};
use std::collections::HashMap;

/// Most targets one question carries.
pub const MAX_TARGETS: usize = 50;
/// Most properties one target carries.
pub const MAX_PROPS: usize = 30;
/// A hit this close to a face plane (model units) belongs to that face.
const FACE_TOL: f64 = 1e-3;

/// Stable face tag of a form (operation << 12 | local index); elements without one have `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceLabel(pub u32);

/// What the model looked like when the context was taken: a changed token means the answer was
/// about an older state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeToken(pub u64);

impl ChangeToken {
    pub fn of(ed: &Editor) -> Self {
        ChangeToken((ed.model.sql().total_changes() << 20) ^ ed.revision)
    }
}

/// A frame cut from a snapshot; `None` while snapshots are off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Plan,
    View3d,
    Section,
    Sheet,
}

impl ViewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewKind::Plan => "plan",
            ViewKind::View3d => "3d",
            ViewKind::Section => "section",
            ViewKind::Sheet => "sheet",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewRef {
    pub kind: ViewKind,
    pub name: String,
    pub level: Option<i64>,
}

/// The camera as the person saw it (orbit form); plan, section and sheet have none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PoseResolved {
    pub target: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
}

/// A rectangle dragged in a view, in model units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub min: [f64; 2],
    pub max: [f64; 2],
    pub level: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointTarget {
    pub el: ElRef,
    pub family: String,
    pub type_name: Option<String>,
    pub level: Option<i64>,
    pub room: Option<i64>,
    pub hit: [f64; 3],
    pub face: Option<FaceLabel>,
    pub props: Vec<(String, String)>,
    pub qty: Vec<(String, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointCtx {
    pub targets: Vec<PointTarget>,
    /// Targets over the cap that were left out.
    pub dropped: usize,
    pub region: Option<Region>,
    pub view: ViewRef,
    pub pose: Option<PoseResolved>,
    pub crop: Option<ImageRef>,
    pub token: ChangeToken,
}

/// What the view's own pick returned: the selection and where the ray or cursor met it.
#[derive(Clone, Debug)]
pub struct PointInput {
    pub view: ViewRef,
    pub hits: Vec<(Sel, [f64; 3])>,
    pub region: Option<Region>,
    pub pose: Option<PoseResolved>,
    pub crop: Option<ImageRef>,
}

type Rings = Vec<(i64, Vec<ddc_geom::math::V2>)>;

/// Rooms of a level with the ring each covers: a sketched room its own outline, any other the
/// wall-bounded region that holds its point.
fn level_rooms(ed: &Editor, level: i64) -> Rings {
    let list = rows::sql_rows(
        &ed.model,
        &format!("SELECT id,px,py,outline FROM room WHERE level={level}"),
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        },
    );
    // the wall arrangement is the costly part: a level without rooms never builds it
    let regions = if list.is_empty() { vec![] } else { ed.model.room_regions(level).unwrap_or_default() };
    list.into_iter()
        .filter_map(|(id, px, py, outline)| {
            let own: Option<Vec<(f64, f64)>> = outline.and_then(|s| serde_json::from_str(&s).ok());
            let ring: Vec<_> = match own {
                Some(o) => o.into_iter().map(|(x, y)| v2(x, y)).collect(),
                None => regions.iter().find(|g| g.contains(v2(px, py)))?.outer.clone(),
            };
            Some((id, ring))
        })
        .collect()
}

/// Distance from `p` to the closest edge of a closed ring.
fn ring_dist(p: ddc_geom::math::V2, ring: &[ddc_geom::math::V2]) -> f64 {
    (0..ring.len())
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
        })
        .fold(f64::INFINITY, f64::min)
}

/// A mass has no query row: its level and name from its own table.
fn mass_row(ed: &Editor, id: i64) -> Option<Row> {
    let (level, name) = rows::sql_rows(&ed.model, &format!("SELECT level, name FROM mass WHERE id={id}"), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })
    .into_iter()
    .next()?;
    Some(Row {
        sel: Sel::Mass(id),
        cat: "mass",
        level,
        ty: String::new(),
        name,
        mark: String::new(),
        nums: vec![],
        props: vec![],
    })
}

fn face_at(ed: &Editor, mass: i64, hit: [f64; 3]) -> Option<FaceLabel> {
    let faces = ed.model.mass_faces(mass).ok()?;
    faces
        .iter()
        .filter(|f| f.class != ddc_model::FaceClass::Curved)
        .filter_map(|f| {
            let p0 = f.outline.first()?.first()?;
            let d = f.normal.0 * (hit[0] - p0.0) + f.normal.1 * (hit[1] - p0.1) + f.normal.2 * (hit[2] - p0.2);
            (d.abs() <= FACE_TOL).then_some((d.abs(), f.tag))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, tag)| FaceLabel(tag))
}

fn el_ref(r: &Row) -> ElRef {
    ElRef {
        cat: r.cat.to_string(),
        id: IdOrNew::Id(r.id()),
    }
}

/// The context of a pointing action. Reads the model only.
pub fn collect(ed: &Editor, input: &PointInput) -> PointCtx {
    let all = rows::rows(ed);
    let by_sel: HashMap<Sel, &Row> = all.iter().map(|r| (r.sel, r)).collect();
    let mut rooms: HashMap<i64, Rings> = HashMap::new();
    let mut seen: Vec<Sel> = Vec::new();
    let mut targets = Vec::new();
    let mut dropped = 0;
    for &(sel, hit) in &input.hits {
        if seen.contains(&sel) {
            continue;
        }
        seen.push(sel);
        if targets.len() >= MAX_TARGETS {
            dropped += 1;
            continue;
        }
        let owned;
        let r: &Row = match (by_sel.get(&sel), sel) {
            (Some(r), _) => *r,
            (None, Sel::Mass(id)) => match mass_row(ed, id) {
                Some(m) => {
                    owned = m;
                    &owned
                }
                None => continue,
            },
            (None, _) => continue,
        };
        let room = match sel {
            Sel::Room(id) => Some(id),
            _ => {
                let rings = rooms.entry(r.level).or_insert_with(|| level_rooms(ed, r.level));
                let p = v2(hit[0], hit[1]);
                // a room ring runs along the wall faces: a hit inside a wall's body (its axis,
                // say) belongs to the nearest room within half the wall's thickness
                let reach = match sel {
                    Sel::Wall(_) => r.nums.iter().find(|(k, _)| *k == "thickness").map_or(0.0, |(_, t)| t / 2.0) + FACE_TOL,
                    _ => 0.0,
                };
                rings.iter().find(|(_, ring)| point_in_ring(p, ring) > 0).map(|(id, _)| *id).or_else(|| {
                    rings
                        .iter()
                        .map(|(id, ring)| (*id, ring_dist(p, ring)))
                        .filter(|(_, d)| *d <= reach)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(id, _)| id)
                })
            }
        };
        let face = match sel {
            Sel::Mass(id) => face_at(ed, id, hit),
            _ => None,
        };
        targets.push(PointTarget {
            el: el_ref(r),
            family: r.cat.to_string(),
            type_name: (!r.ty.is_empty()).then(|| r.ty.clone()),
            level: Some(r.level),
            room,
            hit,
            face,
            props: r.props.iter().take(MAX_PROPS).cloned().collect(),
            qty: r.nums.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        });
    }
    PointCtx {
        targets,
        dropped,
        region: input.region,
        view: input.view.clone(),
        pose: input.pose,
        crop: input.crop.clone(),
        token: ChangeToken::of(ed),
    }
}

fn el_text(e: &ElRef) -> String {
    match e.id {
        IdOrNew::Id(i) => format!("{}:{i}", e.cat),
        IdOrNew::New(n) => format!("{}:n{n}", e.cat),
    }
}

pub fn to_json(c: &PointCtx) -> J {
    let targets: Vec<J> = c
        .targets
        .iter()
        .map(|t| {
            json!({
                "el": el_text(&t.el),
                "family": t.family,
                "type": t.type_name,
                "level": t.level,
                "room": t.room,
                "hit": t.hit,
                "face": t.face.map(|f| f.0),
                "props": t.props.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                "qty": t.qty.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "view": { "kind": c.view.kind.as_str(), "name": c.view.name, "level": c.view.level },
        "pose": c.pose.map(|p| json!({
            "target": p.target, "yaw": p.yaw, "pitch": p.pitch, "distance": p.distance
        })),
        "region": c.region.map(|r| json!({ "min": r.min, "max": r.max, "level": r.level })),
        "crop": c.crop.as_ref().map(|i| i.0.clone()),
        "token": c.token.0,
        "targets": targets,
        "targets_dropped": c.dropped,
    })
}

/// The message the model gets: the `point_ctx` block, then what the person wrote.
pub fn message(c: &PointCtx, text: &str) -> String {
    format!("{}\n{text}", block(c))
}

/// The `point_ctx` block alone.
pub fn block(c: &PointCtx) -> String {
    format!("```point_ctx\n{}\n```", to_json(c))
}

/// Pointing mode of the assistant panel. Leaving it drops everything gathered.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PointMode {
    #[default]
    Off,
    Pointing {
        prompt_draft: String,
        ctx: Option<PointCtx>,
    },
}

impl PointMode {
    pub fn active(&self) -> bool {
        matches!(self, PointMode::Pointing { .. })
    }

    /// The "?" key or the panel button.
    pub fn toggle(&mut self) {
        *self = match self {
            PointMode::Off => PointMode::Pointing {
                prompt_draft: String::new(),
                ctx: None,
            },
            PointMode::Pointing { .. } => PointMode::Off,
        };
    }

    /// A click or a box in a view; ignored outside the mode.
    pub fn point(&mut self, c: PointCtx) {
        if let PointMode::Pointing { ctx, .. } = self {
            *ctx = Some(c);
        }
    }

    /// Esc: out of the mode, no context, no draft.
    pub fn esc(&mut self) {
        *self = PointMode::Off;
    }

    /// The block for the next turn, if something was pointed at. The mode ends.
    pub fn take_block(&mut self) -> Option<String> {
        match std::mem::take(self) {
            PointMode::Pointing { ctx: Some(c), .. } => Some(block(&c)),
            _ => None,
        }
    }

    /// The text to send, with the context block when something was pointed at. The mode ends.
    pub fn send(&mut self, text: &str) -> Option<String> {
        let PointMode::Pointing { ctx, .. } = std::mem::take(self) else {
            return None;
        };
        Some(match ctx {
            Some(c) => message(&c, text),
            None => text.to_string(),
        })
    }
}

#[cfg(test)]
#[path = "point_tests.rs"]
mod tests;
