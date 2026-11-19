//! The chat tools `run_script` and `rerun_recipe`: a geometry script runs in the sandbox of
//! `ddc-script` and its ops envelope becomes one plan item, checked, previewed and diffed like
//! any other. Apply writes the elements and the recipe (script, params, hash) in one step; a
//! rerun with new params touches only the elements the recipe made.

use super::exec::ModelStep;
use super::fmt::{self, Out};
use super::plan::{push, CompileCx, PlanError, PlanOp};
use super::session::{Core, Done};
use ddc_ai::CallCx;
use ddc_ops::{Cat, Envelope, Op, Ref};
use ddc_script::{GeoScript, Link, RunOpts, Sandbox, ScriptError, ScriptRun};
use serde_json::{json, Map, Value as J};
use std::collections::HashMap;
use std::sync::Arc;

/// Part keys listed in a reply; the counts carry the rest.
const KEYS_SHOWN: usize = 12;

/// One script item of the plan. Slots in `env` are local to the item (0..`slots`).
#[derive(Debug)]
pub struct ScriptItem {
    pub env: Envelope,
    /// Local slot of each op that creates a row, with its part key.
    pub keys: Vec<(u32, String)>,
    /// The recipe a rerun updates; `None` stores a new one.
    pub recipe: Option<i64>,
    /// Keys whose elements the rerun deletes for good.
    pub gone: Vec<String>,
    pub name: String,
    pub script: String,
    pub params_json: String,
    pub hash: String,
    pub slots: u32,
}

fn level_of(ed: &crate::editor::Editor) -> Result<Ref, String> {
    ed.model
        .lowest_level()
        .map_err(|e| e.to_string())?
        .map(|l| Ref::id(Cat::Level, l.0))
        .ok_or_else(|| "the model has no level: add one before running a script".to_string())
}

fn script_err(e: ScriptError) -> String {
    format!("script error at {e}")
}

/// Runs `script` against the model of `ed` with `params`.
pub fn run(
    ed: &crate::editor::Editor,
    script: &str,
    params: &J,
    allow_many: bool,
) -> Result<ScriptRun, String> {
    run_at(ed, level_of(ed)?, script, params, allow_many)
}

/// The level a recipe was built on: that of its first element still in the model.
fn recipe_level(ed: &crate::editor::Editor, id: i64) -> Result<Ref, String> {
    let parts = ed.model.recipe_parts(id).map_err(|e| e.to_string())?;
    for p in parts {
        let level: Option<i64> = ed
            .model
            .sql()
            .query_row(&format!("SELECT level FROM {} WHERE id=?1", p.tbl), [p.el], |r| {
                r.get(0)
            })
            .ok()
            .flatten();
        if let Some(l) = level {
            return Ok(Ref::id(Cat::Level, l));
        }
    }
    level_of(ed)
}

fn run_at(
    ed: &crate::editor::Editor,
    level: Ref,
    script: &str,
    params: &J,
    allow_many: bool,
) -> Result<ScriptRun, String> {
    let mut opts = RunOpts::new(level, params.clone());
    opts.limits.allow_many = allow_many;
    let r = Sandbox.run(script, &opts).map_err(script_err)?;
    if let Some(i) = r.env.check().first() {
        return Err(format!("script output failed the envelope check: {i:?}"));
    }
    Ok(r)
}

fn slots_of(env: &Envelope) -> Vec<u32> {
    env.groups
        .iter()
        .flat_map(|g| g.ops.iter())
        .filter_map(|o| match o {
            // the rows the step lists come in this order (`exec::geometry_cats`)
            Op::Geometry(g) if g.made_cat().is_some() => o.slot(),
            _ => None,
        })
        .collect()
}

fn shift(v: &mut J, by: u32) {
    match v {
        J::Object(o) => {
            let new_ref = o.get("by").and_then(|b| b.as_str()) == Some("new");
            let maker = matches!(
                o.get("action").and_then(|a| a.as_str()),
                Some("solid" | "mass" | "gridshell" | "member" | "plate")
            );
            if new_ref || maker {
                if let Some(s) = o.get("slot").and_then(|s| s.as_u64()) {
                    o.insert("slot".into(), json!(s + by as u64));
                }
            }
            o.values_mut().for_each(|x| shift(x, by));
        }
        J::Array(a) => a.iter_mut().for_each(|x| shift(x, by)),
        _ => {}
    }
}

/// The item's envelope with its slots moved to `slot0..`.
pub fn env_at(env: &Envelope, slot0: u32) -> Result<Envelope, String> {
    if slot0 == 0 {
        return Ok(env.clone());
    }
    let mut v = serde_json::to_value(env).map_err(|e| e.to_string())?;
    shift(&mut v, slot0);
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// The ops of the item at `slot0`, as the v1 lowering lists them.
pub fn ops_at(slot0: u32, item: &ScriptItem) -> Result<Vec<Op>, String> {
    Ok(env_at(&item.env, slot0)?
        .groups
        .into_iter()
        .flat_map(|g| g.ops)
        .collect())
}

pub(super) fn compile(
    cx: &mut CompileCx<'_>,
    slot0: u32,
    item: &Arc<ScriptItem>,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let env = env_at(&item.env, slot0).map_err(PlanError)?;
    let makers = slots_of(&env);
    let step = ModelStep::Script {
        env: Box::new(env),
        made: vec![],
        slot0,
        item: item.clone(),
    };
    let ids = push(cx, out, step)?;
    for (slot, id) in makers.into_iter().zip(ids) {
        cx.slots.insert(slot, id);
    }
    if let Some(ModelStep::Script { made, .. }) = out.last() {
        cx.declared.extend(made.iter().cloned());
    }
    Ok(())
}

fn params_out(r: &ScriptRun) -> J {
    let mut o = Map::new();
    for p in &r.params {
        let mut e = Map::new();
        e.insert("value".into(), p.value.clone());
        e.insert("default".into(), p.default.clone());
        if let Some(u) = p.unit {
            e.insert("unit".into(), json!(u));
        }
        o.insert(p.name.clone(), J::Object(e));
    }
    J::Object(o)
}

fn measures_out(r: &ScriptRun) -> J {
    r.measures
        .iter()
        .map(|m| json!({"line": m.line, "value": m.value, "unit": m.unit}))
        .collect()
}

fn keys_out(keys: &[String]) -> J {
    let mut v: Vec<J> = keys
        .iter()
        .take(KEYS_SHOWN)
        .map(|k| json!(fmt::text(k)))
        .collect();
    if keys.len() > KEYS_SHOWN {
        v.push(json!(format!("... {} of {}", KEYS_SHOWN, keys.len())));
    }
    J::Array(v)
}

fn add(c: &mut Core, a: &J, cx: &CallCx<'_>, item: ScriptItem) -> Result<(u32, J), String> {
    let id = c.next_item_id();
    let slot0 = c.plan.reserve_slots(item.slots.max(1), id);
    let title = super::tools_write::title_of(a);
    let why = super::tools_write::why_of(a);
    let (n, counts) = c.add_item(
        title,
        why,
        PlanOp::Script {
            slot0,
            item: Arc::new(item),
        },
        cx.actor,
    )?;
    let changes = json!({"created": counts.0, "changed": counts.1, "deleted": counts.2});
    Ok((n, changes))
}

fn stats_out(r: &ScriptRun) -> J {
    json!({
        "elements": r.stats.elements,
        "ops": r.stats.ops,
        "cpu_ms": (r.stats.cpu_ms * 10.0).round() / 10.0,
        "memory_mb": (r.stats.memory_peak as f64 / 1048576.0 * 10.0).round() / 10.0,
    })
}

/// Most characters of a script built from parts.
const MAX_JOINED: usize = 8 * ddc_ai::catalog::MAX_SCRIPT_CHARS;

/// A call's script text with the parts kept before it. `None`: a part was kept (`more`) and
/// nothing runs yet. A model whose reply is cut by its output limit cannot send a long
/// script in one call, so it sends it in parts at line boundaries.
pub fn join_parts(parts: &mut String, text: &str, more: bool) -> Result<Option<String>, String> {
    {
        let mut all = std::mem::take(parts);
        if !all.is_empty() && !all.ends_with('\n') {
            all.push('\n');
        }
        all.push_str(text);
        if all.len() > MAX_JOINED {
            return Err(format!("the parts add up to more than {MAX_JOINED} characters"));
        }
        if more {
            *parts = all;
            Ok(None)
        } else {
            Ok(Some(all))
        }
    }
}

/// `run_script{script, params, allow_many}`: a new recipe as a draft item.
pub fn run_script(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let text = a["script"].as_str().ok_or("script must be text")?;
    let more = a["more"].as_bool().unwrap_or(false);
    let Some(script) = join_parts(&mut c.script_parts, text, more)? else {
        let mut o = Out::new(c.max_chars);
        o.put("kept", &json!(true));
        o.put(
            "next",
            &json!("part stored, nothing ran; send the next part with run_script, more=true until the last part, which has more=false"),
        );
        return Ok(o.finish().into());
    };
    let params = match &a["params"] {
        J::Null => json!({}),
        p => p.clone(),
    };
    let r = run(
        &c.ed,
        &script,
        &params,
        a["allow_many"].as_bool().unwrap_or(false),
    )?;
    let keys: Vec<(u32, String)> = r.parts.iter().map(|p| (p.slot, p.key.clone())).collect();
    let slots = r.env.groups.iter().map(|g| g.ops.len() as u32).sum();
    let names: Vec<String> = keys.iter().map(|k| k.1.clone()).collect();
    let item = ScriptItem {
        env: r.env.clone(),
        keys,
        recipe: None,
        gone: vec![],
        name: super::tools_write::title_of(a),
        script,
        params_json: params.to_string(),
        hash: r.hash.clone(),
        slots,
    };
    let (n, mut changes) = add(c, a, cx, item)?;
    // solids and masses are not in the element tables the plan counts: count the parts
    changes["created"] = json!(names.len());
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(n));
    o.put("changes", &changes);
    o.put("parts", &keys_out(&names));
    o.put("params", &params_out(&r));
    o.put("measures", &measures_out(&r));
    o.put("constraints_checked", &json!(r.constraints));
    o.put("hash", &json!(r.hash));
    o.put("stats", &stats_out(&r));
    if !r.log.is_empty() {
        o.put(
            "log",
            &J::from(r.log.iter().take(20).cloned().collect::<Vec<_>>()),
        );
    }
    Ok(o.finish().into())
}

fn merged(old: &J, new: &J) -> Result<J, String> {
    let mut o = old.as_object().cloned().unwrap_or_default();
    match new {
        J::Null => {}
        J::Object(n) => o.extend(n.iter().map(|(k, v)| (k.clone(), v.clone()))),
        _ => return Err("params must be an object {name: value}".into()),
    }
    Ok(J::Object(o))
}

fn cat_of(tbl: &str) -> Result<Cat, String> {
    match tbl {
        "solid" => Ok(Cat::Solid),
        "mass" => Ok(Cat::Mass),
        "gridshell" => Ok(Cat::Gridshell),
        t => Err(format!(
            "recipe links a {t} row; only solids, masses and grid structures rerun"
        )),
    }
}

/// What a rerun of recipe `id` with `params` changes, as a plan item and its summary.
pub fn rerun_item(
    ed: &crate::editor::Editor,
    id: i64,
    params: &J,
) -> Result<(ScriptItem, ddc_script::RerunPlan, ScriptRun), String> {
    let rec = ed
        .model
        .recipe(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no recipe {id}"))?;
    let old_params: J = serde_json::from_str(&rec.params_json).map_err(|e| {
        format!(
            "recipe {id}: the stored params are not valid JSON ({e}); \
             run the script again as a new recipe"
        )
    })?;
    let new_params = merged(&old_params, params)?;
    let level = recipe_level(ed, id)?;
    let old = run_at(ed, level.clone(), &rec.script, &old_params, true)?;
    if old.hash != rec.hash {
        return Err(format!(
            "recipe {id} no longer builds what it built (stored hash {}, now {}): the script, \
             the library or the level it ran on changed. Run the script again as a new recipe",
            &rec.hash[..rec.hash.len().min(12)],
            &old.hash[..old.hash.len().min(12)]
        ));
    }
    let new = run_at(ed, level, &rec.script, &new_params, true)?;
    let links = ed
        .model
        .recipe_parts(id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| {
            Ok(Link {
                key: p.key,
                cat: cat_of(&p.tbl)?,
                id: p.el,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let plan = ddc_script::rerun_envelope(&old, &new, &links, old.env.actor.clone(), &rec.name)?;
    let env = plan.env.clone().unwrap_or_else(|| new.env.clone());
    let slots = plan.created.iter().map(|p| p.slot + 1).max().unwrap_or(0);
    let item = ScriptItem {
        env,
        keys: plan
            .created
            .iter()
            .map(|p| (p.slot, p.key.clone()))
            .collect(),
        recipe: Some(id),
        gone: plan.deleted.clone(),
        name: rec.name,
        script: rec.script,
        params_json: new_params.to_string(),
        hash: new.hash.clone(),
        slots,
    };
    Ok((item, plan, new))
}

/// `rerun_recipe{recipe, params}`: new params for a stored recipe, as a draft item.
pub fn rerun_recipe(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let id = a["recipe"].as_i64().ok_or("recipe must be a recipe id")?;
    let (item, plan, new) = rerun_item(&c.ed, id, &a["params"])?;
    let mut o = Out::new(c.max_chars);
    if plan.env.is_none() {
        o.put("recipe", &json!(id));
        o.put("unchanged", &json!(plan.kept));
        o.put(
            "note",
            &json!("the new params build the same parts: nothing to change"),
        );
        return Ok(o.finish().into());
    }
    let (n, _) = add(c, a, cx, item)?;
    let created: Vec<String> = plan
        .created
        .iter()
        .map(|p| p.key.clone())
        .filter(|k| !plan.replaced.contains(k))
        .collect();
    // solids are not in the element tables the plan counts: count from the recipe diff
    let changes = json!({
        "created": created.len(),
        "changed": plan.reshaped.len() + plan.replaced.len(),
        "deleted": plan.deleted.len(),
    });
    o.put("item", &json!(n));
    o.put("recipe", &json!(id));
    o.put("changes", &changes);
    o.put(
        "diff",
        &json!({
            "kept": plan.kept,
            "reshaped": keys_out(&plan.reshaped),
            "replaced": keys_out(&plan.replaced),
            "created": keys_out(&created),
            "deleted": keys_out(&plan.deleted),
        }),
    );
    o.put("params", &params_out(&new));
    o.put("measures", &measures_out(&new));
    o.put("hash", &json!(new.hash));
    o.put("stats", &stats_out(&new));
    Ok(o.finish().into())
}

/// Recipe rows for what a step created: `created` pairs a shifted slot with the new row.
pub fn parts_of(
    item: &ScriptItem,
    slot0: u32,
    created: &[(u32, Ref)],
) -> Vec<ddc_model::RecipePart> {
    let keys: HashMap<u32, &str> = item
        .keys
        .iter()
        .map(|k| (k.0 + slot0, k.1.as_str()))
        .collect();
    created
        .iter()
        .filter_map(|(slot, r)| match r {
            // beam and brace rows are not recipe-linked tables: a rerun cannot rebuild them
            Ref::Id { cat: Cat::Beam | Cat::Brace, .. } => None,
            Ref::Id { cat, id } => Some(ddc_model::RecipePart {
                key: keys.get(slot)?.to_string(),
                tbl: match cat {
                    Cat::Mass => "mass",
                    Cat::Gridshell => "gridshell",
                    _ => "solid",
                }
                .to_string(),
                el: *id,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_script_arrives_in_parts_and_runs_whole() {
        let mut p = String::new();
        assert_eq!(join_parts(&mut p, "local a = 1", true), Ok(None));
        assert_eq!(join_parts(&mut p, "local b = a + 1\n", true), Ok(None));
        let whole = join_parts(&mut p, "part('p', nil)", false).unwrap().unwrap();
        assert_eq!(whole, "local a = 1\nlocal b = a + 1\npart('p', nil)");
        assert_eq!(join_parts(&mut p, "x", false), Ok(Some("x".to_string())));
        assert!(join_parts(&mut p, &"y".repeat(MAX_JOINED + 1), true).is_err());
        assert_eq!(join_parts(&mut p, "z", false), Ok(Some("z".to_string())), "an error clears the buffer");
    }
}

