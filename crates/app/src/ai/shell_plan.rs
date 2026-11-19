//! The free-form envelope of a project (`design.shell`) as geometry ops: a freeform surface per
//! patch, glazed in panels on the skin and layered as a roof shell on the roof, steel ribs along
//! the patch edges, and a diagrid dome over the central opening. Rows land in the project's
//! shell slots; the head records them with a hash of what was asked, so an unchanged shell is
//! kept and a changed or dropped one is deleted before the new one is made.

use super::exec::{self, ModelStep};
use super::plan::{push, CompileCx, PlanError};
use ddc_gen::project::slots;
use ddc_gen::project::{GShell, ShellPatch};
use ddc_ops::{Actor, Cat, Envelope, Group, Op, Ref, Targets};
use serde_json::{json, Value as J};
use std::f64::consts::TAU;

type Row = (String, i64);

/// Tables of shell rows that can be deleted, with their categories.
const TABLES: [(&str, Cat); 5] = [
    ("shell_elem", Cat::SurfaceShell),
    ("gridshell", Cat::Gridshell),
    ("panel_system", Cat::PanelSystem),
    ("solid", Cat::Solid),
    ("freeform", Cat::Freeform),
];

fn len(v: f64) -> J {
    json!({"v": v, "unit": "m"})
}

fn pt(base: f64, p: [f64; 3]) -> J {
    json!({"x": p[0], "y": p[1], "z": base + p[2], "unit": "m"})
}

fn loft(base: f64, level: &J, patch: &ShellPatch) -> J {
    let curves: Vec<J> = patch
        .sections
        .iter()
        .map(|s| json!({"pts": s.iter().map(|p| pt(base, *p)).collect::<Vec<_>>(), "fit": "through", "degree": 3}))
        .collect();
    // one parameter per section row, the same for every patch: neighbours share their edge
    let last = (patch.sections.len() - 1) as f64;
    let params: Vec<f64> = (0..patch.sections.len()).map(|i| i as f64 / last).collect();
    json!({"action": "freeform", "level": level, "surface": {"by": "loft", "curves": curves, "degree": 3, "params": params},
           "name": patch.name})
}

/// Moves every `{"by": "new", "slot": k}` of an operation `by` places on.
fn shift(v: &mut J, by: u64) {
    match v {
        J::Object(o) => {
            if o.get("by").and_then(J::as_str) == Some("new") {
                if let Some(k) = o.get("slot").and_then(J::as_u64) {
                    o.insert("slot".into(), json!(k + by));
                }
            }
            o.values_mut().for_each(|x| shift(x, by));
        }
        J::Array(a) => a.iter_mut().for_each(|x| shift(x, by)),
        _ => {}
    }
}

/// Runs of at most the call limit; a group joins a run whole, its refs moved to its place.
fn runs(groups: Vec<Vec<J>>) -> Vec<Vec<J>> {
    let mut out: Vec<Vec<J>> = vec![];
    for mut g in groups {
        match out.last_mut() {
            Some(r) if r.len() + g.len() <= super::geo_tools::MAX_GEO_OPS => {
                let at = r.len() as u64;
                g.iter_mut().for_each(|o| shift(o, at));
                r.extend(g);
            }
            _ => out.push(g),
        }
    }
    out
}

/// The operations of the whole shell, in groups that name only their own operations.
fn groups(sh: &GShell, base: f64, level: &J) -> Vec<Vec<J>> {
    let mut g: Vec<Vec<J>> = vec![];
    for patch in &sh.skin {
        g.push(vec![
            loft(base, level, patch),
            json!({"action": "panelize", "host": {"by": "new", "slot": 0},
                   "columns": {"by": "width", "width": len(sh.panel.0)},
                   "rows": {"by": "width", "width": len(sh.panel.1)},
                   "pattern": "quad", "planarize": sh.planarize, "glass": len(sh.glass),
                   "mullion": {"width": len(sh.mullion.0), "depth": len(sh.mullion.1)},
                   "flat": len(sh.flat), "cold": len(sh.cold), "name": format!("{} glazing", patch.name)}),
        ]);
    }
    for patch in &sh.roof {
        // the surface normal of a roof patch points down: its layers are listed outer first
        let layers: Vec<J> = ddc_gen::project::ROOF_LAYER_NAMES
            .iter()
            .zip(sh.layer_shares)
            .map(|(n, share)| json!({"thickness": len(sh.roof_thickness * share), "name": n}))
            .collect();
        g.push(vec![
            loft(base, level, patch),
            json!({"action": "surface_shell", "host": {"by": "new", "slot": 0}, "kind": "roof",
                   "layers": layers, "side": "normal", "name": patch.name}),
        ]);
    }
    for (i, rib) in sh.skin_ribs.iter().chain(&sh.roof_ribs).enumerate() {
        g.push(vec![json!({
            "action": "solid", "level": level, "name": format!("Rib {}", i + 1),
            "ifc": {"class": "IfcMember"},
            "form": {"kind": "sweep",
                     "profile": {"shape": "rect", "w": len(sh.rib * sh.rib_width), "h": len(sh.rib)},
                     "path": {"kind": "polyline", "pts": rib.iter().map(|p| pt(base, *p)).collect::<Vec<_>>(),
                              "bulges": [], "closed": false}}})]);
    }
    let r = sh.oculus;
    let cu = (((TAU * r / sh.panel.0).round() as u32).clamp(8, 48) / 2) * 2;
    let cv = ((1.5 * sh.dome_rise / sh.panel.1).round() as u32).clamp(3, 12);
    g.push(vec![json!({
        "action": "gridshell", "level": level, "kind": "diagrid", "count_u": cu, "count_v": cv,
        "host": {"shape": "dome", "center": pt(base, [sh.centre.0, sh.centre.1, sh.rim]),
                 "radius": len(r), "rise": len(sh.dome_rise)},
        "member": sh.member})]);
    g
}

fn exists(cx: &CompileCx<'_>, table: &str, id: i64) -> bool {
    TABLES.iter().any(|t| t.0 == table)
        && cx
            .ed
            .model
            .sql()
            .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE id=?1"), [id], |r| r.get::<_, i64>(0))
            .is_ok_and(|n| n > 0)
}

fn rows_of(head: &J) -> Vec<Row> {
    head["rows"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| Some((r[0].as_str()?.to_string(), r[1].as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// Rows of the stored shell still in the model, deleted in one envelope.
fn delete_old(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, old: &[Row]) -> Result<(), PlanError> {
    let refs: Vec<Ref> = old
        .iter()
        .filter(|r| exists(cx, &r.0, r.1))
        .filter_map(|r| TABLES.iter().find(|t| t.0 == r.0).map(|t| Ref::id(t.1, r.1)))
        .collect();
    if refs.is_empty() {
        return Ok(());
    }
    let mut env = Envelope::new(Actor::Ai { session: "chat".into(), model: "plan".into() }, "shell");
    env.groups.push(Group {
        id: 1,
        title: "shell".into(),
        ops: vec![Op::Delete { target: Targets::Refs { refs } }],
        depends_on: vec![],
        why: String::new(),
    });
    push(cx, out, ModelStep::Geometry { env: Box::new(env), made: vec![] })?;
    Ok(())
}

/// Brings the shell rows to `shell` (none drops them); the result is the head entry to store.
/// `at` is the project's first slot, `base` the absolute height of the project's base.
pub(super) fn compile(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    at: u32,
    level: i64,
    base: f64,
    shell: Option<&GShell>,
    prev: &J,
) -> Result<J, PlanError> {
    let old = rows_of(prev);
    let Some(sh) = shell else {
        delete_old(cx, out, &old)?;
        clear(cx, at);
        return Ok(J::Null);
    };
    let intent = ddc_model::fnv1a(format!("{sh:?}|{base}|{level}").as_bytes()).to_string();
    if prev["intent"] == intent && !old.is_empty() && old.iter().all(|r| exists(cx, &r.0, r.1)) {
        return Ok(prev.clone());
    }
    delete_old(cx, out, &old)?;
    clear(cx, at);
    let lvl = json!({"by": "id", "cat": "level", "id": level});
    let mut slot = at + slots::SHELL;
    let mut rows: Vec<Row> = vec![];
    for run in runs(groups(sh, base, &lvl)) {
        if slot + run.len() as u32 > at + slots::SHELL + slots::SHELL_MAX {
            return Err(PlanError("the free-form shell has more elements than its slots".into()));
        }
        let from = out.len();
        super::geo_tools::compile(cx, slot, &run, out)?;
        for s in &out[from..] {
            rows.extend(exec::created_keys(s));
        }
        slot += run.len() as u32;
    }
    Ok(json!({"intent": intent, "rows": rows}))
}

fn clear(cx: &mut CompileCx<'_>, at: u32) {
    let (a, b) = (at + slots::SHELL, at + slots::SHELL + slots::SHELL_MAX);
    cx.slots.retain(|k, _| *k < a || *k >= b);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The patches of the shell as the model stores them meet on one edge, as the generator's
    /// own surfaces do.
    #[test]
    fn neighbouring_skin_patches_meet_within_a_micrometre_in_the_model() {
        let ring: Vec<(f64, f64)> = (0..72)
            .map(|i| {
                let t = TAU * i as f64 / 72.0;
                (30.0 * t.cos(), 20.0 * t.sin())
            })
            .collect();
        let sh = ddc_gen::project::shell_envelope(&ring, 0.0, 12.0, &[]).unwrap();
        let mut model = ddc_model::Model::open(":memory:").unwrap();
        let l = model.add_level("L1", 0.0).unwrap();
        let level = json!({"by": "id", "cat": "level", "id": l});
        let g = groups(&sh, 0.0, &level);
        let run = runs(g[..3].to_vec()).remove(0);
        let env = super::super::geo_tools::envelope_of(super::super::geo_tools::geo_ops(0, &run).unwrap(), "t").unwrap();
        let r = model.apply_envelope(&env).unwrap();
        let ids: Vec<i64> = r
            .created
            .iter()
            .filter_map(|c| match &c.1 {
                Ref::Id { cat: Cat::Freeform, id } => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 3);
        let s: Vec<_> = ids.iter().map(|id| model.freeform(*id).unwrap().surface).collect();
        for w in s.windows(2) {
            let gap = (0..=40)
                .map(|i| {
                    let v = i as f64 / 40.0;
                    (w[0].point(1.0, v) - w[1].point(0.0, v)).len()
                })
                .fold(0.0, f64::max);
            assert!(gap <= 1e-6, "seam in the model {gap} m");
        }
    }

    /// Walls round the outline under the roof of the shell: the roof trim sets every wall top
    /// on the underside of the roof (up to it, never above it).
    #[test]
    fn walls_under_the_shell_roof_reach_its_underside() {
        let n = 72;
        let ring: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let t = TAU * i as f64 / n as f64;
                (30.0 * t.cos(), 20.0 * t.sin())
            })
            .collect();
        let sh = ddc_gen::project::shell_envelope(&ring, 0.0, 12.0, &[]).unwrap();
        let mut model = ddc_model::Model::open(":memory:").unwrap();
        let l = model.add_level("L1", 0.0).unwrap();
        let level = json!({"by": "id", "cat": "level", "id": l});
        let g = groups(&sh, 0.0, &level);
        let roofs = g[sh.skin.len()..sh.skin.len() + sh.roof.len()].to_vec();
        let mut surfaces = vec![];
        for run in runs(roofs) {
            let env = super::super::geo_tools::envelope_of(super::super::geo_tools::geo_ops(0, &run).unwrap(), "t").unwrap();
            let r = model.apply_envelope(&env).unwrap();
            for c in r.created {
                if let Ref::Id { cat: Cat::Freeform, id } = c.1 {
                    surfaces.push(model.freeform(id).unwrap().surface);
                }
            }
        }
        assert_eq!(surfaces.len(), sh.roof.len());
        // the roof as a cloud of points of its top surface
        let mut cloud: Vec<[f64; 3]> = vec![];
        for s in &surfaces {
            let (u0, u1) = s.domain_u();
            let (v0, v1) = s.domain_v();
            for i in 0..=60 {
                for j in 0..=60 {
                    let p = s.point(u0 + (u1 - u0) * i as f64 / 60.0, v0 + (v1 - v0) * j as f64 / 60.0);
                    cloud.push([p.x, p.y, p.z]);
                }
            }
        }
        let wt = model.add_wall_type("Wall", &[ddc_model::LayerSpec { material: "Concrete".into(), thickness: 0.3 }]).unwrap();
        let mut checked = 0;
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let w = model
                .add_wall(&ddc_model::WallSpec { a, b, level: l, base_offset: 0.0, height: 12.0, wall_type: wt, offset0: None })
                .unwrap();
            let top = model.wall_top_profile(w).unwrap().unwrap_or_else(|| panic!("wall {i} is not trimmed by the shell"));
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            for k in 0..=4 {
                let u = len * k as f64 / 4.0;
                let h = {
                    let j = top.iter().position(|p| p.0 >= u - 1e-9).unwrap_or(top.len() - 1);
                    if j == 0 || (top[j].0 - u).abs() < 1e-9 { top[j].1 } else {
                        let (p, q) = (top[j - 1], top[j]);
                        p.1 + (q.1 - p.1) * (u - p.0) / (q.0 - p.0)
                    }
                };
                let (x, y) = (a.0 + (b.0 - a.0) * k as f64 / 4.0, a.1 + (b.1 - a.1) * k as f64 / 4.0);
                let near = cloud.iter().min_by(|p, q| (p[0] - x).hypot(p[1] - y).total_cmp(&(q[0] - x).hypot(q[1] - y))).unwrap();
                // the underside lies the roof build-up below the top surface; the nearest sample
                // of the cloud sets the slope tolerance
                let slack = 0.25;
                assert!(h <= near[2] + slack, "wall {i} at {u:.1} m: top {h:.3} above the roof {:.3}", near[2]);
                assert!(h >= near[2] - sh.roof_thickness * 1.5 - slack, "wall {i} at {u:.1} m: top {h:.3} short of the roof {:.3}", near[2]);
                checked += 1;
            }
        }
        assert_eq!(checked, n * 5);
    }
}
