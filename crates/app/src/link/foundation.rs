//! The foundation a spec asks for (`ddc gen`): a raft under the footprint or strips under the
//! bearing walls, written into the model after the project, each with its element key.

use ddc_gen::project::{plan_foundation, FoundationSpec, GProject, ProjectSpec};
use ddc_model::{RaftSpec, StripFootingSpec, KEY_PROP};
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};

use crate::editor::Editor;

/// Thickness of the ground slab at `level`, so the raft's top meets its underside.
fn ground_slab(ed: &Editor, level: i64) -> f64 {
    ed.model
        .sql()
        .query_row(
            "SELECT COALESCE(SUM(l.thickness), 0) FROM slab_type_layer l WHERE l.stype = \
             (SELECT stype FROM slab WHERE level=?1 ORDER BY id LIMIT 1)",
            [level],
            |r| r.get::<_, f64>(0),
        )
        .unwrap_or(0.0)
}

fn wall_by_key(ed: &Editor, key: &str) -> Option<i64> {
    ed.model
        .sql()
        .query_row("SELECT ref FROM prop WHERE kind='wall' AND key=?1 AND CAST(value AS TEXT)=?2", rusqlite::params![KEY_PROP, key], |r| r.get(0))
        .ok()
}

/// Builds the foundation of the spec in `ed` (ground level `ed.level`); returns what was built
/// as `{kind, count, keys, ...}`, or `None` when the spec has none.
pub fn build(ed: &mut Editor, spec: &ProjectSpec, p: &GProject) -> Result<Option<Value>, String> {
    let Some(plan) = plan_foundation(spec, p, 1) else { return Ok(None) };
    let level = ed.level;
    let key = |ed: &mut Editor, table: &str, id: i64, k: &str| {
        ed.model.set_prop(table, id, KEY_PROP, Some((Sql::Text(k.to_string()), ""))).map_err(|e| format!("{k}: {e}"))
    };
    match plan.spec {
        FoundationSpec::Raft { thickness } => {
            let under = ground_slab(ed, level);
            let id = ed
                .model
                .add_raft(&RaftSpec { level, ring: plan.ring.clone(), thickness, edge_width: 0.0, edge_depth: 0.0, top_offset: -under, material: "Concrete".into() })
                .map_err(|e| format!("raft: {e}"))?;
            key(ed, "raft", id, &plan.key)?;
            Ok(Some(json!({"kind": "raft", "count": 1, "keys": [plan.key], "thickness_m": thickness, "area_m2": plan.raft_area(), "top_offset_m": -under})))
        }
        FoundationSpec::Strip { width, depth } => {
            let mut keys = vec![];
            for s in &plan.strips {
                let wall = wall_by_key(ed, &s.wall_key);
                let id = ed
                    .model
                    .add_strip_footings(&[StripFootingSpec { level, wall, a: s.a, b: s.b, bulge: s.bulge, width, depth, ecc: 0.0, top_offset: 0.0, material: "Concrete".into() }])
                    .map_err(|e| format!("strip {}: {e}", s.key))?[0];
                key(ed, "strip_footing", id, &s.key)?;
                keys.push(s.key.clone());
            }
            Ok(Some(json!({"kind": "strip", "count": keys.len(), "keys": keys, "width_m": width, "depth_m": depth})))
        }
    }
}
