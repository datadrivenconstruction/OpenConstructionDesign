//! The foundation of a built project: what the spec asked for, placed under the ground floor.
//! Raft: one slab whose outline is the ground slab's. Strip: one strip under each exterior or
//! party wall of the ground floor, named by that wall. Keys follow the key scheme of the project
//! (`b1/found/raft`, `b1/found/strip/<wall key of the ground floor>`).

use super::keys::project_keys;
use super::{FoundationSpec, GProject, ProjectSpec};
use crate::WallRole;

#[derive(Clone, Debug, PartialEq)]
pub struct StripPlan {
    /// Axis of the host wall, metres.
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// Arc of the host wall (bulge, 0 for a straight one): the strip follows the curve, not its chord.
    pub bulge: f64,
    /// Key of the host wall (ground floor); the strip's own key is `key`.
    pub wall_key: String,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FoundationPlan {
    pub spec: FoundationSpec,
    /// Outline of the raft (empty for strips).
    pub ring: Vec<(f64, f64)>,
    /// Key of the raft (empty for strips).
    pub key: String,
    pub strips: Vec<StripPlan>,
}

impl FoundationPlan {
    /// Plan area of the raft, m2.
    pub fn raft_area(&self) -> f64 {
        let r = &self.ring;
        (0..r.len()).map(|i| r[i].0 * r[(i + 1) % r.len()].1 - r[(i + 1) % r.len()].0 * r[i].1).sum::<f64>().abs() / 2.0
    }
}

/// `None` when the spec asks for no foundation or the project has no ground floor.
pub fn plan_foundation(spec: &ProjectSpec, p: &GProject, building: u32) -> Option<FoundationPlan> {
    let f = spec.foundation?;
    let ground = p.storeys.first()?;
    let l = &ground.layout;
    match f {
        FoundationSpec::Raft { .. } => (l.slab.len() >= 3).then(|| FoundationPlan {
            spec: f,
            ring: l.slab.clone(),
            key: format!("b{building}/found/raft"),
            strips: vec![],
        }),
        FoundationSpec::Strip { .. } => {
            let keys = project_keys(p, building);
            let prefix = format!("b{building}/s01/");
            let strips: Vec<StripPlan> = l
                .walls
                .iter()
                .zip(&keys.storeys[0].walls)
                .filter(|(w, _)| matches!(w.role, WallRole::Exterior | WallRole::Party))
                .map(|(w, k)| StripPlan {
                    a: w.a,
                    b: w.b,
                    bulge: w.bulge,
                    wall_key: k.clone(),
                    key: format!("b{building}/found/strip/{}", k.strip_prefix(&prefix).unwrap_or(k)),
                })
                .collect();
            (!strips.is_empty()).then(|| FoundationPlan { spec: f, ring: vec![], key: String::new(), strips })
        }
    }
}
