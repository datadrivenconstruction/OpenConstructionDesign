// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Read tools. They see only the scratch copy, and every reply goes through `fmt::Out`.

use super::card;
use ddc_ai::refusal::{self, RefusalCode};
use ddc_ai::registry::{CmdError, ErrorCode};
use super::fmt::{self, Out, Reply};
use super::plan::{self, ElRef, IdOrNew};
use super::rows::{self, element_materials};
use super::session::{Core, Done, FromWorker, Handler};
use super::sets::{norm_cat, parse_cursor, parse_set, table_of, Key};
use super::tools_write::target_of;
use crate::query::{self, Row};
use ddc_ai::catalog;
use ddc_ai::CallCx;
use ddc_gen::ParamKind;
use ddc_model::checks::{CheckScope, Finding, Fix, Severity};
use ddc_model::{ClashParams, ClashSide, ElKey, CLASH_CATS};
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

const PAGE: usize = 50;
const MAX_FINDINGS: usize = ddc_ai::limits::FINDINGS.us();
const MAX_COLS: usize = ddc_ai::limits::SCHED_COLS.us();
/// How many rows a filter tests between looks at the cancel token.
const CANCEL_EVERY: usize = 4096;

// A slice: the count can never drift from the entries.
pub const HANDLERS: &[(&str, Handler)] = &[
    ("query_spatial", query_spatial),
    ("mark_checkpoint", mark_checkpoint),
    ("measure", super::measure_tool::measure),
    ("measure_list", super::measure_tool::measure_list),
    ("measure_eval", super::measure_tool::measure_eval),
    ("get_reference", super::reference::get_reference),
    (catalog::ATTACHMENTS_LIST, super::attach::attachments_list),
    (catalog::ATTACHMENTS_READ, super::attach::attachments_read),
    ("create_schedule", super::sched_tool::create_schedule),
    ("query_relations", query_relations),
    ("query_topology", query_topology),
    ("get_card", get_card),
    ("model_summary", model_summary),
    ("get_context", get_context),
    ("list_levels", list_levels),
    ("list_materials", list_materials),
    ("list_types", list_types),
    ("list_params", list_params),
    ("describe_category", describe_category),
    ("query_elements", query_elements),
    ("page", page),
    (catalog::VIEW_SNAPSHOT, view_snapshot),
    (catalog::RENDER_PLAN, render_plan),
    (catalog::RENDER_3D, render_3d),
    ("get_element", get_element),
    ("quantities", quantities),
    ("schedule", schedule),
    ("export_schedule", export_schedule),
    ("run_checks", run_checks),
    ("clash", clash),
    ("get_plan", get_plan),
    ("list_generators", list_generators),
    ("describe_generator", describe_generator),
    ("script_recipes", script_recipes),
    ("list_options", list_options),
    ("list_issues", list_issues),
    ("find_missing", find_missing),
    ("check_requirements", check_requirements),
    ("requirements_report", requirements_report),
];

/// "N of M lack X": the gaps as refs and a frozen set, which the fill targets as `{"set": id}`.
fn find_missing(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let w = str_arg(a, "category")?;
    let param = str_arg(a, "param")?;
    let cat = norm_cat(w).ok_or_else(|| format!("unknown category: {}", fmt::text(w)))?;
    let pcat = super::tools_write::param_cat_word(cat);
    let m = c.ed.model.find_missing(&pcat, param).map_err(|e| e.to_string())?;
    let keys: Vec<Key> = m.ids.iter().map(|id| Key { cat, id: *id }).collect();
    let refs: Vec<J> = keys.iter().take(PAGE).map(|k| json!(k.text())).collect();
    let n = keys.len();
    let marks = c.marks_of(&keys);
    let set = c.sets.add_marked(keys, vec![], marks);
    let mut o = out(c);
    let what = format!("{n} of {} {cat} lack {}", m.total, fmt::text(param));
    o.put("summary", &json!(what));
    o.put("missing", &json!(n));
    o.put("total", &json!(m.total));
    o.put("set", &json!(format!("s{set}")));
    if n > 0 {
        let next = format!(
            "update_elements with target {{\"set\": \"s{set}\"}} drafts the fill; \
             the person reviews and verifies the values"
        );
        o.put("next", &json!(next));
    }
    o.list("refs", &refs);
    if n > PAGE {
        o.put("more", &json!(n - PAGE));
    }
    Ok(o.finish().into())
}

fn str_arg<'a>(a: &'a J, k: &str) -> Result<&'a str, String> {
    a[k].as_str().ok_or_else(|| format!("{k} must be text"))
}

fn str_list(a: &J) -> Vec<String> {
    a.as_array()
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn out(c: &Core) -> Out {
    Out::new(c.max_chars)
}

fn level_name(c: &Core, id: i64) -> String {
    c.ed.levels()
        .into_iter()
        .find(|l| l.0 == id)
        .map_or_else(|| id.to_string(), |l| l.1)
}

/// Selected ids listed by `get_context`; past this the reply carries counts and a set.
const SELECTION_SHOWN: usize = 50;
const BIG_SELECTION_HINT: &str = "The selection is large: only the first ids are listed. \
Call page with selection_set as set and selection_next as cursor to read the rest; \
selection_set is frozen at this call and works as the target of a write; \
use query_elements to narrow it down.";

/// Counts per category and how many ids were left out, only for a selection over the cap.
fn selection_summary(sel: &[Key]) -> Option<(J, usize)> {
    if sel.len() <= SELECTION_SHOWN {
        return None;
    }
    let mut by_cat: HashMap<String, usize> = HashMap::new();
    for k in sel {
        *by_cat.entry(k.cat.to_string()).or_default() += 1;
    }
    Some((fmt::facet(by_cat), sel.len() - SELECTION_SHOWN))
}

/// The `page` cursor right after the listed ids of a big selection.
fn selection_next(set: u32) -> String {
    format!("s{set}@{SELECTION_SHOWN}")
}

fn get_context(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    // units and phase: a change of them touches whatever is built on this answer
    c.reads.meta = true;
    let t = c.ed.model.model_totals().map_err(|e| e.to_string())?;
    let sel = c.selection_keys();
    let mut o = out(c);
    o.put("view", &json!(c.view.view));
    o.put(
        "level",
        &json!({"id": c.view.level, "name": level_name(c, c.view.level)}),
    );
    o.put(
        "units",
        &json!(if c.view.units == crate::units::Units::Metric {
            "metric"
        } else {
            "imperial"
        }),
    );
    o.put("selected", &json!(sel.len()));
    // what the user can do at this step (keys and current values), data only
    o.put("now", &c.ed.ix.describe(&c.ed));
    let depth = c.ed.model.depth_settings().map_err(|e| e.to_string())?;
    o.put("depth", &crate::depth_ui::settings_json(&depth));
    let big = selection_summary(&sel);
    if let Some((by_cat, more)) = &big {
        // a big selection goes as counts and a frozen set: the ids stay out of the context
        let marks = c.marks_of(&sel);
        let set = c.sets.add_marked(sel.clone(), vec![], marks);
        o.put("selection_by_cat", by_cat);
        o.put("selection_set", &json!(format!("s{set}")));
        o.put("selection_next", &json!(selection_next(set)));
        o.put("selection_more", &json!(more));
        o.put("hint", &json!(BIG_SELECTION_HINT));
    }
    o.put(
        "selection",
        &J::from(sel.iter().take(SELECTION_SHOWN).map(Key::text).collect::<Vec<_>>()),
    );
    if let Some(b) = c.view.bounds {
        o.put("bounds", &json!({
            "min": [fmt::num(f64::from(b.min.x)), fmt::num(f64::from(b.min.y)), fmt::num(f64::from(b.min.z))],
            "max": [fmt::num(f64::from(b.max.x)), fmt::num(f64::from(b.max.y)), fmt::num(f64::from(b.max.z))],
        }));
    }
    o.put("totals", &json!({
        "walls": t.walls, "doors": t.doors, "windows": t.windows, "slabs": t.slabs, "rooms": t.rooms, "curtains": t.curtains,
        "wall_volume": fmt::num(t.wall_volume), "slab_area": fmt::num(t.slab_area), "room_area": fmt::num(t.room_area),
    }));
    // applied generated projects with the spec revise_project patches
    let projects: Vec<J> = super::project_plan::stored(&c.ed.model)
        .unwrap_or_default()
        .into_iter()
        .map(|(pid, head)| json!({"id": pid, "spec": head["input"]}))
        .collect();
    if !projects.is_empty() {
        o.list("projects", &projects);
    }
    Ok(o.finish().into())
}

fn list_levels(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let items: Vec<J> =
        c.ed.levels()
            .into_iter()
            .map(|l| json!({"id": l.0, "name": l.1, "elevation": fmt::num(l.2)}))
            .collect();
    let mut o = out(c);
    o.list("levels", &items);
    Ok(o.finish().into())
}

fn list_materials(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let items = super::materials::rows(&c.ed.model);
    let gaps = c.ed.model.material_gaps().map(|g| g.len()).unwrap_or(0);
    let mut o = out(c);
    o.put("total", &json!(items.len()));
    o.put("elements_without_look", &json!(gaps));
    // a long project list must not starve the library the assistant picks looks from
    let half = o.left() / 2;
    o.list_capped("materials", &items, half);
    o.list("library", &super::materials::library());
    Ok(o.finish().into())
}

fn types_of(c: &Core, table: &str) -> Vec<J> {
    if table == "comp_type" {
        return rows::sql_rows(
            &c.ed.model,
            "SELECT t.id, f.name, t.name FROM comp_type t JOIN comp_family f ON f.id=t.family ORDER BY f.name, t.name",
            |r| {
                let id: i64 = r.get(0)?;
                Ok(json!({"ref": format!("comp_type:{id}"), "family": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?}))
            },
        );
    }
    rows::sql_rows(
        &c.ed.model,
        &format!("SELECT id, name FROM {table} ORDER BY name"),
        |r| {
            let id: i64 = r.get(0)?;
            Ok(json!({"ref": format!("{table}:{id}"), "name": r.get::<_, String>(1)?}))
        },
    )
}

fn list_types(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let tables: Vec<&str> = match a["cat"].as_str() {
        None => vec!["wall_type", "slab_type", "member_type", "comp_type"],
        Some(w) => {
            match norm_cat(w).ok_or_else(|| format!("unknown category: {}", fmt::text(w)))? {
                "wall" => vec!["wall_type"],
                "floor" | "roof" | "ceiling" => vec!["slab_type"],
                "column" | "beam" => vec!["member_type"],
                "comp" | "door" | "window" | "opening" => vec!["comp_type"],
                _ => vec![],
            }
        }
    };
    let items: Vec<J> = tables.iter().flat_map(|t| types_of(c, t)).collect();
    let mut o = out(c);
    o.put("total", &json!(items.len()));
    o.list("types", &items);
    Ok(o.finish().into())
}

fn list_params(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let defs = match a["cat"].as_str() {
        None => c.ed.model.params(),
        Some(w) => {
            let cat = norm_cat(w).ok_or_else(|| format!("unknown category: {}", fmt::text(w)))?;
            c.ed.model
                .params_for(&super::tools_write::param_cat_word(cat))
        }
    }
    .map_err(|e| e.to_string())?;
    let items: Vec<J> = defs
        .iter()
        .map(|d| json!({"name": d.spec.name, "ptype": d.spec.ptype.key(), "per_type": d.spec.per_type, "unit": d.spec.unit, "group": d.spec.group, "cats": d.spec.categories}))
        .collect();
    let mut o = out(c);
    o.put("total", &json!(items.len()));
    o.list("params", &items);
    Ok(o.finish().into())
}

/// Lazy schema of a category: the fields `create_elements` and `update_elements` understand.
pub(super) fn category_fields(cat: &str) -> Option<J> {
    let level = "level: level id, default the current level";
    let geometry = "numbers in metres, z above the level";
    Some(match cat {
        "wall" => json!({"create": ["a [x,y]", "b [x,y]", level, "height (3.0)", "base_offset (0)", "type wall_type ref or name"], "edit": ["type", "level", "height", "offset", "name", "lean (degrees toward the wall's left, straight flat-topped walls)", "top_scale (top/base thickness ratio)"]}),
        "door" | "window" => json!({"create": ["wall ref, may be wall:nK", "along: from the wall start", "width", "height", "sill"], "edit": ["width", "height", "sill"], "note": "doors and windows in walls have no types: change sizes or a parameter"}),
        "floor" => json!({"create": ["outer [[x,y],...]", "holes", level, "offset", "type slab_type ref or name"], "edit": ["type", "level", "offset"]}),
        "room" => json!({"create": ["point [x,y] inside the room", "name", "number", "height", level], "edit": ["name", "mark", "height"]}),
        "column" => json!({"create": ["at [x,y]", "type member_type ref or name", level, "top_level", "base_offset", "top_offset", "rotation", "structural", "top_at [x,y] top point of a slanted (V, raked) column"], "edit": ["parameters and replace_type only"]}),
        "beam" => json!({"create": ["a [x,y,z]", "b [x,y,z]", "type member_type ref or name", level], "edit": ["parameters and replace_type only"]}),
        "comp" => json!({"create": ["type comp_type ref", "at [x,y,z]", "rotation", "host wall/floor/ceiling ref", level], "edit": ["parameters and replace_type only"]}),
        "ceiling" => json!({"create": ["outer", "holes", "height above the level", "type slab_type ref or name", level], "edit": ["parameters and replace_type only"]}),
        "roof" => json!({"create": ["ring [[x,y],...]", "slope degrees for all edges, none for flat", "overhang", "offset", "type slab_type ref or name", level], "edit": ["parameters and replace_type only"]}),
        "stair" => json!({"create": ["origin [x,y]", "angle", "shape straight|l|u|spiral (l, u: left, first; u: gap; spiral: radius and core in m, turn in rad)", level, "top_level"], "edit": ["parameters only"]}),
        "mep" => json!({"create": ["system mep_system ref", "points [[x,y,z],...] absolute", "domain pipe|duct|tray|conduit (default pipe)", "pipe: dn, material", "duct/tray: width and height, or diameter for a round duct", "conduit: diameter", level], "edit": ["parameters only"], "prefer": "route_mep, supply_fixtures and power_devices route whole networks: state the intent, never hand-place points"}),
        "curtain" => json!({"create": ["a [x,y]", "b [x,y]", "height", "name", "spacing [u,v]", "mullion [w,d]", level], "edit": ["level", "name", "offset"]}),
        "railing" | "imported" => json!({"create": [], "edit": ["parameters only"]}),
        "shaft" => json!({"create": ["ring [[x,y],...] outline of the opening", level, "top_level", "top_offset (0)"], "edit": ["delete and create again"], "note": "cuts every slab, ceiling and roof between its levels"}),
        "grid" => json!({"create": ["name", "a [x,y]", "b [x,y]"], "edit": ["name", "move with the grid tools"], "prefer": "create_grids makes a whole set from count, spacing, angle and labels"}),
        "steel_conn" => json!({"create": ["type connection key or auto", "members column/beam refs", "params key=value;...", level], "edit": ["parameters only"], "prefer": "connect_members: omit members for every free node of the type"}),
        _ => return None,
    })
    .map(|mut v: J| {
        v["units"] = json!(geometry);
        v
    })
}

/// Lazy schema of a type table (`wall_type`, ...): what the type tools accept.
fn type_fields(table: &str) -> J {
    let how = "create_type adds one (cat, name, fields); replace_type retypes a target; list_types lists them";
    match table {
        "wall_type" => json!({"create": ["name", "fields: layers"], "edit": ["edit_wall_type: add, remove or resize a layer", "copy_types_from another project"], "how": how, "read": "get_element wall_type:N lists the layers"}),
        "slab_type" => json!({"create": ["name", "fields: thickness and material (one layer), or layers"], "edit": ["parameters"], "how": how, "used_by": "floor, roof, ceiling"}),
        "member_type" => json!({"create": ["name", "fields: shape such as rect 0.3 0.3 (or section), material"], "edit": ["parameters"], "how": how, "used_by": "column, beam"}),
        _ => json!({"create": ["name", "family"], "edit": ["parameters"], "how": how, "used_by": "comp, door, window"}),
    }
}

fn describe_category(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let w = str_arg(a, "cat")?;
    if let Ok(table) = plan::type_table(w) {
        let mut o = out(c);
        o.put("cat", &json!(table));
        o.put_own("fields", &type_fields(table));
        return Ok(o.finish().into());
    }
    let cat = norm_cat(w).ok_or_else(|| format!("unknown category: {}", fmt::text(w)))?;
    let fields = category_fields(cat).ok_or_else(|| format!("{cat} cannot be created"))?;
    let mut o = out(c);
    o.put("cat", &json!(cat));
    o.put_own("fields", &fields);
    Ok(o.finish().into())
}

pub(crate) fn row_value(r: &Row, field: &str, level: &dyn Fn(i64) -> String) -> J {
    match field {
        "ref" => json!(rows::key_of(r).text()),
        "cat" | "category" => json!(r.cat),
        "type" => json!(r.ty),
        "lvl" | "level" => json!(level(r.level)),
        "name" => json!(r.name),
        "mark" => json!(r.mark),
        "id" => json!(r.id()),
        _ => {
            let k = query::canon(field);
            if let Some(n) = r.nums.iter().find(|n| n.0 == k) {
                return fmt::num(n.1);
            }
            let low = field.to_lowercase();
            r.props
                .iter()
                .find(|p| p.0 == low || p.0 == k)
                .map_or(J::Null, |p| json!(p.1))
        }
    }
}

fn columns(fields: &[String]) -> Vec<String> {
    let mut cols: Vec<String> = ["ref", "type", "lvl"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for f in fields {
        if !cols.contains(f) && cols.len() < MAX_COLS {
            cols.push(f.clone());
        }
    }
    cols
}

fn level_lookup(c: &Core) -> HashMap<i64, String> {
    c.ed.levels().into_iter().map(|l| (l.0, l.1)).collect()
}

/// The rows a test accepts; Cancel stops the walk over a big model.
fn matching<'a>(
    all: &'a [Row],
    cx: &CallCx<'_>,
    test: impl Fn(&Row) -> bool,
) -> Result<Vec<&'a Row>, String> {
    let mut hits = vec![];
    for (i, r) in all.iter().enumerate() {
        if i % CANCEL_EVERY == 0 && cx.cancel.is_cancelled() {
            return Err("cancelled".to_string());
        }
        if test(r) {
            hits.push(r);
        }
    }
    Ok(hits)
}

fn query_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let filter = str_arg(a, "filter")?;
    query::check_tool_numbers(filter)?;
    let q = query::parse(filter)?;
    if !q.set.is_empty() {
        return Err("a filter cannot assign values".to_string());
    }
    let limit = a["limit"].as_u64().map_or(PAGE, |n| (n as usize).min(200));
    let fields = str_list(&a["fields"]);
    let levels = level_lookup(c);
    let level = |id: i64| levels.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let all = rows::rows(&c.ed);
    let env = query::Env::tool(&c.ed);
    let f = query::Filter::new(&q.expr, &env);
    let hits: Vec<&Row> = matching(&all, cx, |r| f.test(r))?;
    let (mut by_level, mut by_type, mut by_cat): (
        HashMap<String, usize>,
        HashMap<String, usize>,
        HashMap<String, usize>,
    ) = Default::default();
    for r in &hits {
        *by_level.entry(level(r.level)).or_default() += 1;
        *by_type.entry(r.ty.clone()).or_default() += 1;
        *by_cat.entry(r.cat.to_string()).or_default() += 1;
    }
    let facets = json!({"cat": fmt::facet(by_cat), "level": fmt::facet(by_level), "type": fmt::facet(by_type)});
    let keys: Vec<Key> = hits.iter().map(|r| rows::key_of(r)).collect();
    let total = keys.len();
    c.reads.filter(c.ed.model.sql(), &q.expr, &keys);
    super::view_tools::query_card(c, &all, filter, &q.expr, &keys);
    let marks = c.marks_of(&keys);
    let set = c.sets.add_marked(keys, fields.clone(), marks);
    let cols = columns(&fields);
    let rows_iter = hits.iter().map(|r| {
        cols.iter()
            .map(|k| row_value(r, k, &level))
            .collect::<Vec<J>>()
    });
    Ok(fmt::page(
        c.max_chars,
        set,
        total,
        Some(&facets),
        &cols,
        rows_iter,
        0,
        limit,
    )
    .into())
}

fn page(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let (id, at) = parse_cursor(str_arg(a, "cursor")?).ok_or("bad cursor")?;
    if parse_set(str_arg(a, "set")?) != Some(id) {
        return Err("cursor belongs to another set".to_string());
    }
    let set = c
        .sets
        .get(id)
        .ok_or_else(|| CmdError::wire(ErrorCode::NotFound, format!("no set s{id}")))?
        .clone();
    let levels = level_lookup(c);
    let level = |id: i64| levels.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let all: Rc<Vec<Row>> = rows::rows(&c.ed);
    let by_key: HashMap<(&'static str, i64), &Row> = all
        .iter()
        .map(|r| ((rows::key_of(r).table(), r.id()), r))
        .collect();
    let cols = columns(&set.fields);
    let total = set.keys.len();
    let rows_iter = set
        .keys
        .iter()
        .skip(at)
        .map(|k| match by_key.get(&(k.table(), k.id)) {
            Some(r) => cols
                .iter()
                .map(|f| row_value(r, f, &level))
                .collect::<Vec<J>>(),
            None => vec![json!(k.text()), json!("gone")],
        });
    Ok(fmt::page(c.max_chars, id, total, None, &cols, rows_iter, at, PAGE).into())
}

/// Images are read-only, but they leave the editor: the project's consent decides. The refusal
/// names the policy and where the person turns it on, so an agent can tell them.
pub(super) const SNAPSHOTS_OFF: &str = "not_allowed: images of the model are off by the \
    project's AI policy egress.snapshots; the person enables 'Snapshots of the current plan or \
    3D view' in the AI assistant connection settings";

fn snapshots_gate(c: &Core) -> Result<(), String> {
    if c.allow_snapshots.load(std::sync::atomic::Ordering::Relaxed) {
        Ok(())
    } else {
        let r = refusal::of(RefusalCode::SnapshotsOff)
            .fix(refusal::open_settings())
            .detail(SNAPSHOTS_OFF.trim_start_matches("not_allowed: "));
        Err(refusal::wire(&r))
    }
}

fn view_snapshot(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    snapshots_gate(c)?;
    super::snapshot::request(c, a)
}

fn audit_render(c: &Core, what: &str, img: &ddc_ai::ImageData, withheld: bool) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    img.bytes.hash(&mut h);
    c.audit(
        what,
        None,
        Some(format!(
            "{}x{} {} bytes {:016x}",
            img.width,
            img.height,
            img.bytes.len(),
            h.finish()
        )),
        if withheld { "metrics_only" } else { "ok" },
    );
}

fn render_plan(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    use super::{render_off, render_req as rq, verify_view as vv};
    let acc = vv::access(c)?;
    let req = rq::parse_plan(a, &c.ed.levels())?;
    let r = render_off::plan(&c.ed, c.view.units, &req)?;
    let (images, facts) = vv::deliver(acc, r.out.image.clone())?;
    audit_render(c, "render_plan", images.first().unwrap_or(&r.out.image), images.is_empty());
    let mut o = out(c);
    o.put("view", &facts);
    o.put("model", &vv::model_facts(c));
    o.put("level", &json!(level_name(c, req.level)));
    o.put("width", &json!(r.out.image.width));
    o.put("height", &json!(r.out.image.height));
    o.put("scale", &rq::scale_json(&r.size));
    o.put("region_m", &json!(r.region.map(|x| (x * 100.0).round() / 100.0)));
    o.put("legend_scope", &json!("walls crossing the region"));
    o.put("legend", &rq::legend_json(&r.out.legend, r.out.cut));
    Ok(Done {
        reply: o.finish(),
        images,
    })
}

fn render_3d(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    use super::{render_off, render_req as rq, verify_view as vv};
    let acc = vv::access(c)?;
    let req = rq::parse_3d(a)?;
    let r = render_off::view3d(&c.ed, c.view.units, &req)?;
    let (images, facts) = vv::deliver(acc, r.out.image.clone())?;
    audit_render(c, "render_3d", images.first().unwrap_or(&r.out.image), images.is_empty());
    let mut o = out(c);
    o.put("view", &facts);
    o.put("model", &vv::model_facts(c));
    o.put("width", &json!(r.out.image.width));
    o.put("height", &json!(r.out.image.height));
    o.put("max_px", &json!(req.max_px));
    o.put("legend_scope", &json!(r.legend_scope));
    o.put("legend", &rq::legend_json(&r.out.legend, r.out.cut));
    Ok(Done {
        reply: o.finish(),
        images,
    })
}

/// Card of an element without a query row (shaft, grid, steel connection, a type): the columns
/// of its table, so the model reads what it created. `table` is always a fixed model table.
fn raw_card(c: &mut Core, text: &str, cat: &str, table: &str, id: i64) -> Result<Done, String> {
    let found = (!table.is_empty())
        .then(|| {
            rows::sql_rows(
                &c.ed.model,
                &format!("SELECT * FROM {table} WHERE id={id}"),
                |r| {
                    let names: Vec<String> =
                        r.as_ref().column_names().map(|n| n.to_string()).collect();
                    let mut m = serde_json::Map::new();
                    for (i, n) in names.into_iter().enumerate() {
                        use rusqlite::types::ValueRef as V;
                        let v = match r.get_ref(i)? {
                            V::Null => J::Null,
                            V::Integer(x) => json!(x),
                            V::Real(x) => fmt::num(x),
                            V::Text(t) => json!(String::from_utf8_lossy(t)),
                            V::Blob(b) => json!(format!("{} bytes", b.len())),
                        };
                        m.insert(n, v);
                    }
                    Ok(m)
                },
            )
            .into_iter()
            .next()
        })
        .flatten();
    let Some(cols) = found else {
        return Err(CmdError::wire(ErrorCode::NotFound, format!("no element {text}")));
    };
    let mut o = out(c);
    o.put("ref", &json!(format!("{cat}:{id}")));
    o.put("cat", &json!(cat));
    o.put("table", &json!(table));
    o.put_own("columns", &J::Object(cols));
    if table == "wall_type" {
        if let Ok(layers) = c.ed.model.wall_type_layers_full(id) {
            let l: Vec<J> = layers
                .iter()
                .map(|x| json!({"material": x.material, "thickness": fmt::num(x.thickness)}))
                .collect();
            o.list("layers", &l);
        }
    }
    Ok(o.finish().into())
}

fn get_element(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let text = str_arg(a, "ref")?;
    if let Some((word, n)) = text.split_once(':') {
        if let (Ok(table), Ok(id)) = (plan::type_table(word), n.trim().parse::<i64>()) {
            return raw_card(c, text.trim(), table, table, id);
        }
    }
    let r = ElRef::parse(text).map_err(|e| e.0)?;
    let cat = norm_cat(&r.cat).ok_or_else(|| format!("unknown category: {}", fmt::text(&r.cat)))?;
    let id = match r.id {
        IdOrNew::Id(i) => i,
        IdOrNew::New(n) => *c
            .plan
            .slots
            .get(&n)
            .ok_or_else(|| format!("no new element n{n}"))?,
    };
    let table = table_of(cat);
    let all = rows::rows(&c.ed);
    let Some(row) = all
        .iter()
        .find(|x| x.id() == id && rows::key_of(x).table() == table)
    else {
        return raw_card(c, &r.text(), cat, table, id);
    };
    c.reads.keys(c.ed.model.sql(), &[rows::key_of(row)]);
    let mut o = out(c);
    o.put("ref", &json!(rows::key_of(row).text()));
    o.put("cat", &json!(row.cat));
    o.put(
        "level",
        &json!({"id": row.level, "name": level_name(c, row.level)}),
    );
    o.put("type", &json!(row.ty));
    o.put("name", &json!(row.name));
    o.put("mark", &json!(row.mark));
    let nums: serde_json::Map<String, J> = row
        .nums
        .iter()
        .map(|n| (n.0.to_string(), fmt::num(n.1)))
        .collect();
    o.put("nums", &J::Object(nums));
    let props: Vec<J> = row.props.iter().map(|p| json!([p.0, p.1])).collect();
    o.list("params", &props);
    Ok(o.finish().into())
}

/// A wall or door counts as in a room when it stands this close to the room outline, in metres.
const ROOM_TOL: f64 = 0.3;

/// The row of a reference; an unknown one gets a reason the model can act on.
fn locate(c: &Core, text: &str) -> Result<(Rc<Vec<Row>>, usize), String> {
    let r = ElRef::parse(text).map_err(|e| e.0)?;
    let cat = norm_cat(&r.cat).ok_or_else(|| format!("unknown category: {}", fmt::text(&r.cat)))?;
    let id = match r.id {
        IdOrNew::Id(i) => i,
        IdOrNew::New(n) => *c
            .plan
            .slots
            .get(&n)
            .ok_or_else(|| format!("no new element n{n}"))?,
    };
    let table = table_of(cat);
    let all = rows::rows(&c.ed);
    match all
        .iter()
        .position(|x| x.id() == id && rows::key_of(x).table() == table)
    {
        Some(i) => Ok((all, i)),
        None => {
            let same = all
                .iter()
                .filter(|x| rows::key_of(x).table() == table)
                .count();
            let why = format!(
                "no element {} ({same} of category {cat} exist; find references with \
                 query_elements)",
                fmt::text(&r.text())
            );
            Err(CmdError::wire(ErrorCode::NotFound, why))
        }
    }
}

fn host_of(conn: &rusqlite::Connection, key: &Key) -> Option<card::Host> {
    match key.cat {
        "door" | "window" | "opening" => conn
            .query_row(
                "SELECT wall, along FROM opening WHERE id=?1",
                rusqlite::params![key.id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
            )
            .ok()
            .map(|(w, a)| card::Host {
                key: format!("wall:{w}"),
                along: Some(a),
            }),
        "comp" => conn
            .query_row(
                "SELECT host_wall, host_slab, host_ceiling FROM comp WHERE id=?1",
                rusqlite::params![key.id],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .ok()
            .and_then(|(w, s, k)| {
                w.map(|i| ("wall", i))
                    .or(s.map(|i| ("floor", i)))
                    .or(k.map(|i| ("ceiling", i)))
            })
            .map(|(cat, i)| card::Host {
                key: format!("{cat}:{i}"),
                along: None,
            }),
        _ => None,
    }
}

/// Plan position and level of an element that can stand in a room.
fn plan_point(conn: &rusqlite::Connection, key: &Key) -> Option<(f64, f64, i64)> {
    match key.cat {
        "wall" => conn
            .query_row(
                "SELECT (ax+bx)/2.0, (ay+by)/2.0, level FROM wall WHERE id=?1",
                rusqlite::params![key.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok(),
        "door" | "window" | "opening" => {
            let (ax, ay, bx, by, along, level): (f64, f64, f64, f64, f64, i64) = conn
                .query_row(
                    "SELECT w.ax, w.ay, w.bx, w.by, o.along, w.level FROM opening o \
                     JOIN wall w ON w.id=o.wall WHERE o.id=?1",
                    rusqlite::params![key.id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .ok()?;
            let len = (bx - ax).hypot(by - ay);
            let t = if len > 0.0 {
                along.clamp(0.0, len) / len
            } else {
                0.0
            };
            Some((ax + (bx - ax) * t, ay + (by - ay) * t, level))
        }
        "comp" => conn
            .query_row(
                "SELECT x, y, level FROM comp WHERE id=?1",
                rusqlite::params![key.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok(),
        _ => None,
    }
}

/// `number name` of the rooms around the element, in room id order.
fn rooms_of(conn: &rusqlite::Connection, key: &Key) -> Vec<String> {
    let Some((x, y, level)) = plan_point(conn, key) else {
        return vec![];
    };
    let Ok(mut st) = conn.prepare(
        "SELECT r.number, r.name, q.rings FROM room r JOIN room_qty q ON q.room=r.id \
         WHERE r.level=?1 ORDER BY r.id",
    ) else {
        return vec![];
    };
    let Ok(it) = st.query_map(rusqlite::params![level], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    }) else {
        return vec![];
    };
    let found: Vec<String> = it
        .flatten()
        .filter(|(_, _, rings)| {
            serde_json::from_str::<Vec<Vec<(f64, f64)>>>(rings)
                .is_ok_and(|g| card::near_region(&g, (x, y), ROOM_TOL))
        })
        .map(|(n, name, _)| format!("{n} {name}").trim().to_string())
        .collect();
    found
}

fn card_input(c: &Core, row: &Row) -> card::CardIn {
    let key = rows::key_of(row);
    let el = key.el();
    let conn = c.ed.model.sql();
    card::CardIn {
        key: key.text(),
        ty: row.ty.clone(),
        name: row.name.clone(),
        mark: row.mark.clone(),
        level: level_name(c, row.level),
        nums: row.nums.iter().map(|n| (n.0.to_string(), n.1)).collect(),
        props: row.props.clone(),
        host: host_of(conn, &key),
        rooms: rooms_of(conn, &key),
        ai_items: c.plan.declared.values().filter(|s| s.contains(&el)).count(),
    }
}

/// One line about one element, at most `card::CARD_MAX` characters.
fn get_card(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let (all, i) = locate(c, str_arg(a, "ref")?)?;
    Ok(card::card_reply(&card_input(c, &all[i])).into())
}

fn model_summary(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let tokens = a["budget"]
        .as_u64()
        .map_or(card::SUMMARY_TOKENS, |n| n.min(1_000_000) as usize);
    let max = card::budget_chars(tokens, c.max_chars);
    let all = rows::rows(&c.ed);
    let mut per_level: HashMap<i64, usize> = HashMap::new();
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    let mut types: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut rooms = vec![];
    for r in all.iter() {
        *per_level.entry(r.level).or_default() += 1;
        *cats.entry(r.cat.to_string()).or_default() += 1;
        if r.cat == "room" {
            let area = r.nums.iter().find(|n| n.0 == "area").map_or(0.0, |n| n.1);
            let label = format!("{} {}", r.mark, r.name).trim().to_string();
            rooms.push((rows::key_of(r).text(), label, area));
        } else if !r.ty.is_empty() {
            *types
                .entry((r.cat.to_string(), r.ty.clone()))
                .or_default() += 1;
        }
    }
    let inp = card::SummaryIn {
        levels: c
            .ed
            .levels()
            .into_iter()
            .map(|l| (l.0, l.1, l.2, per_level.get(&l.0).copied().unwrap_or(0)))
            .collect(),
        cats: cats.into_iter().collect(),
        types: types.into_iter().map(|((k, t), n)| (k, t, n)).collect(),
        rooms,
        bounds: c.view.bounds.map(|b| {
            [
                f64::from(b.min.x),
                f64::from(b.min.y),
                f64::from(b.min.z),
                f64::from(b.max.x),
                f64::from(b.max.y),
                f64::from(b.max.z),
            ]
        }),
    };
    Ok(card::summary(&inp, max).into())
}

#[derive(Default)]
struct Sum {
    count: usize,
    volume: f64,
    area: f64,
    /// Walls, beams, MEP runs, railings: the row's `length` field.
    length: f64,
}

fn quantities(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let target = target_of(c, &a["target"])?;
    let keys = c.resolve_target(&target).map_err(|e| e.0)?;
    let group_by = str_arg(a, "group_by")?;
    let wanted: std::collections::HashSet<(&'static str, i64)> =
        keys.iter().map(|k| (k.table(), k.id)).collect();
    let mats: Vec<rows::ElemMaterial> = element_materials(&c.ed.model)
        .into_iter()
        .filter(|q| wanted.contains(&(q.table, q.id)))
        .collect();
    let mut groups: BTreeMap<String, Sum> = BTreeMap::new();
    let mut measured: std::collections::HashSet<(&'static str, i64)> =
        std::collections::HashSet::new();
    if group_by == "material" {
        for q in &mats {
            let g = groups.entry(q.material.clone()).or_default();
            g.volume += q.volume;
            g.area += q.area;
            if measured.insert((q.table, q.id)) {
                g.count += 1;
            }
        }
    } else {
        let levels = level_lookup(c);
        let mut by_el: HashMap<(&'static str, i64), (f64, f64)> = HashMap::new();
        for q in &mats {
            let e = by_el.entry((q.table, q.id)).or_default();
            e.0 += q.volume;
            e.1 += q.area;
        }
        let all = rows::rows(&c.ed);
        for r in all
            .iter()
            .filter(|r| wanted.contains(&(rows::key_of(r).table(), r.id())))
        {
            let name = match group_by {
                "level" => levels
                    .get(&r.level)
                    .cloned()
                    .unwrap_or_else(|| r.level.to_string()),
                "type" => r.ty.clone(),
                _ => r.cat.to_string(),
            };
            let g = groups.entry(name).or_default();
            g.count += 1;
            g.length += r.nums.iter().find(|n| n.0 == "length").map_or(0.0, |n| n.1);
            if let Some(q) = by_el.get(&(rows::key_of(r).table(), r.id())) {
                g.volume += q.0;
                g.area += q.1;
                measured.insert((rows::key_of(r).table(), r.id()));
            }
        }
    }
    let mut items: Vec<(String, Sum)> = groups.into_iter().collect();
    items.sort_by(|x, y| {
        y.1.volume
            .total_cmp(&x.1.volume)
            .then_with(|| x.0.cmp(&y.0))
    });
    let list: Vec<J> = items
        .iter()
        .map(|(k, s)| {
            let mut j = json!({"key": k, "count": s.count, "volume": fmt::num(s.volume), "area": fmt::num(s.area)});
            // only where something has a length: keeps door and room groups as short as before
            if s.length > 0.0 {
                j["length"] = fmt::num(s.length);
            }
            j
        })
        .collect();
    let total = keys.len();
    let marks = c.marks_of(&keys);
    let set = c.sets.add_marked(keys, vec![], marks);
    let mut o = out(c);
    o.put("set", &json!(format!("s{set}")));
    o.put("total", &json!(total));
    o.put("group_by", &json!(group_by));
    o.put("not_measured", &json!(total - measured.len()));
    o.list("groups", &list);
    Ok(o.finish().into())
}

fn schedule_rows<'a>(
    c: &Core,
    all: &'a [Row],
    a: &J,
    cx: &CallCx<'_>,
) -> Result<Vec<&'a Row>, String> {
    let w = str_arg(a, "cat")?;
    let cat = norm_cat(w).ok_or_else(|| format!("unknown category: {}", fmt::text(w)))?;
    let env = query::Env::tool(&c.ed);
    let filter = match a["filter"].as_str() {
        Some(t) => {
            query::check_tool_numbers(t)?;
            Some(query::parse(t)?)
        }
        None => None,
    };
    let f = filter.as_ref().map(|q| query::Filter::new(&q.expr, &env));
    let wanted = |r: &Row| {
        let same = r.cat == cat || (cat == "opening" && (r.cat == "door" || r.cat == "window"));
        same && f.as_ref().is_none_or(|f| f.test(r))
    };
    matching(all, cx, wanted)
}

fn schedule(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let all = rows::rows(&c.ed);
    let hits = schedule_rows(c, &all, a, cx)?;
    let levels = level_lookup(c);
    let level = |id: i64| levels.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let mut fields = str_list(&a["columns"]);
    if fields.is_empty() {
        fields = vec!["name".to_string(), "mark".to_string()];
        for r in hits.iter().take(200) {
            for n in &r.nums {
                if !fields.iter().any(|f| f == n.0) {
                    fields.push(n.0.to_string());
                }
            }
        }
    }
    if let Some(g) = a["group"].as_str() {
        let mut groups: BTreeMap<String, (usize, BTreeMap<String, f64>)> = BTreeMap::new();
        for r in &hits {
            let name = match row_value(r, g, &level) {
                J::String(s) => s,
                other => other.to_string(),
            };
            let e = groups.entry(name).or_default();
            e.0 += 1;
            for f in &fields {
                if let Some(x) = row_value(r, f, &level).as_f64() {
                    *e.1.entry(f.clone()).or_default() += x;
                }
            }
        }
        let list: Vec<J> = groups
            .iter()
            .map(|(k, (n, sums))| {
                let s: serde_json::Map<String, J> = sums
                    .iter()
                    .map(|(f, x)| (f.clone(), fmt::num(*x)))
                    .collect();
                json!({"key": k, "count": n, "sum": s})
            })
            .collect();
        let mut o = out(c);
        o.put("total", &json!(hits.len()));
        o.put("group", &json!(g));
        o.list("groups", &list);
        return Ok(o.finish().into());
    }
    let keys: Vec<Key> = hits.iter().map(|r| rows::key_of(r)).collect();
    let total = keys.len();
    let marks = c.marks_of(&keys);
    let set = c.sets.add_marked(keys, fields.clone(), marks);
    let cols = columns(&fields);
    let rows_iter = hits.iter().map(|r| {
        cols.iter()
            .map(|k| row_value(r, k, &level))
            .collect::<Vec<J>>()
    });
    Ok(fmt::page(c.max_chars, set, total, None, &cols, rows_iter, 0, PAGE).into())
}

/// The person picks the file in a dialog: no path comes from the model, no file is written here.
fn export_schedule(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let all = rows::rows(&c.ed);
    let total = schedule_rows(c, &all, a, cx)?.len();
    let columns = str_list(&a["columns"]);
    let group = a["group"].as_str().map(str::to_string);
    let cat = a["cat"].as_str().unwrap_or_default().to_string();
    super::ui::consent::check_export(&c.ed, &cat, &columns, group.as_deref())?;
    c.send(FromWorker::ExportOffer {
        cat,
        filter: a["filter"].as_str().map(str::to_string),
        columns,
        group,
    });
    let mut o = out(c);
    o.put("status", &json!("offered"));
    o.put("total", &json!(total));
    Ok(o.finish().into())
}

fn el_text(k: &ElKey) -> String {
    format!("{}:{}", k.0, k.1)
}

pub(super) fn scope_of(c: &Core, a: &J) -> Result<CheckScope, String> {
    if let Some(l) = a["level"].as_i64() {
        return Ok(CheckScope::Level(l));
    }
    if a["refs"].is_array() {
        let mut els = vec![];
        for s in str_list(&a["refs"]) {
            let r = ElRef::parse(&s).map_err(|e| e.0)?;
            let cat = norm_cat(&r.cat)
                .ok_or_else(|| format!("unknown category: {}", fmt::text(&r.cat)))?;
            let id = match r.id {
                IdOrNew::Id(i) => i,
                IdOrNew::New(n) => *c
                    .plan
                    .slots
                    .get(&n)
                    .ok_or_else(|| format!("no new element n{n}"))?,
            };
            els.push((table_of(cat).to_string(), id));
        }
        return Ok(CheckScope::Elements(els));
    }
    Ok(CheckScope::All)
}

fn severity(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Every rule over `scope`; one failing rule does not hide the others.
fn rules_over(
    c: &Core,
    ids: &[String],
    scope: &CheckScope,
    cx: &CallCx<'_>,
) -> Result<(Vec<Finding>, Vec<&'static str>, Vec<J>, Vec<&'static str>), String> {
    let all = plan::all_rules(&c.rules);
    if let Some(bad) = ids
        .iter()
        .find(|i| !all.iter().any(|r| r.id() == i.as_str()))
    {
        return Err(format!("unknown rule {}", fmt::text(bad)));
    }
    let mut found: Vec<Finding> = vec![];
    let (mut ran, mut failed, mut not_run) = (vec![], vec![], vec![]);
    for r in &all {
        if cx.cancel.is_cancelled() {
            return Err("cancelled".to_string());
        }
        if !(ids.is_empty() || ids.iter().any(|i| i == r.id())) {
            not_run.push(r.id());
            continue;
        }
        match r.run(&c.ed.model, scope, &mut found) {
            Ok(()) => ran.push(r.id()),
            Err(e) => failed.push(json!({"rule": r.id(), "error": fmt::text(&e.to_string())})),
        }
    }
    Ok((found, ran, failed, not_run))
}

/// Notes the findings that stand now under a name, for `run_checks` with `since`.
fn mark_checkpoint(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let name = fmt::text(a["name"].as_str().unwrap_or_default());
    if name.trim().is_empty() || name.starts_with("c:") {
        return Err(CmdError::wire(ErrorCode::InvalidArgs, "args.name is a name, not empty and not c:<number>"));
    }
    let (found, _, failed, _) = rules_over(c, &[], &CheckScope::All, cx)?;
    let head = c.ed.model.change_head();
    let print = super::checkpoints::print_of(&found);
    let n = print.len();
    c.checkpoints.mark(&name, head, print);
    let mut o = out(c);
    o.put("token", &json!(super::checkpoints::token_text(head)));
    o.put("name", &json!(name));
    o.put("findings", &json!(n));
    o.put("rules_failed", &json!(failed));
    Ok(o.finish().into())
}

fn run_checks(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let started = web_time::Instant::now();
    let ids = str_list(&a["rules"]);
    let since = match a["since"].as_str() {
        Some(s) => Some(c.checkpoints.since(&c.ed.model, s)?),
        None => None,
    };
    let scope = match &since {
        Some(s) => CheckScope::Elements(s.changed.clone()),
        None => scope_of(c, &a["scope"])?,
    };
    let (mut found, ran, failed, not_run) = rules_over(c, &ids, &scope, cx)?;
    let head = c.ed.model.change_head();
    let mut resolved: Vec<String> = vec![];
    let mut baseline = None;
    if let Some(sn) = &since {
        let now = super::checkpoints::print_of(&found);
        if let Some(base) = &sn.baseline {
            // a finding of the mark is cleared when the changes touched its elements and it is gone
            let touched: std::collections::HashSet<&ElKey> = sn.changed.iter().collect();
            resolved = base
                .iter()
                .filter(|(k, els)| !now.contains_key(*k) && els.iter().any(|e| touched.contains(e)))
                .map(|(k, _)| fmt::text(k))
                .collect();
            resolved.sort();
            found.retain(|f| !base.contains_key(&super::checkpoints::fingerprint(f)));
        }
        baseline = Some(if sn.baseline.is_some() { "ledger" } else { "scope_only" });
    } else if ids.is_empty() && scope == CheckScope::All {
        c.checkpoints.record(head, super::checkpoints::print_of(&found));
    }
    let company: Vec<String> =
        c.ed.model
            .project_rules()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.id)
            .collect();
    found.sort_by_key(|f| f.severity);
    let total = found.len();
    found.truncate(MAX_FINDINGS);
    let mut list = Vec::with_capacity(found.len());
    for f in found {
        let id = c.remember_finding(&f);
        let args: serde_json::Map<String, J> = f
            .args
            .iter()
            .map(|x| (x.0.to_string(), json!(x.1)))
            .collect();
        let els: Vec<String> = f.elements.iter().take(20).map(el_text).collect();
        list.push(json!({
            "ref": id,
            "rule": f.rule,
            "severity": severity(f.severity),
            "msg": f.msg_key,
            "args": args,
            "elements": els,
            "fixable": !matches!(f.fix, Fix::None),
        }));
    }
    let mut o = out(c);
    o.put("total", &json!(total));
    o.put("rules_ran", &json!(ran));
    o.put("rules_not_run", &json!(not_run));
    o.put("rules_failed", &json!(failed));
    o.put("company_rules", &json!(company));
    o.put("basis", &json!("Model checks only: passing means these rules found nothing. It is not proof of code compliance."));
    o.put("token", &json!(super::checkpoints::token_text(head)));
    if let (Some(sn), Some(b)) = (&since, baseline) {
        o.put("since", &json!(super::checkpoints::token_text(sn.head)));
        o.put("baseline", &json!(b));
        o.put("scope_size", &json!(sn.changed.len()));
        o.put("elapsed_ms", &json!(started.elapsed().as_secs_f64() * 1e3));
        resolved.truncate(MAX_FINDINGS);
        o.put("resolved", &json!(resolved));
    }
    o.list("findings", &list);
    Ok(o.finish().into())
}

const REQ_BASIS: &str = "Information requirements only: passing means the listed values are \
    present and well-formed. It is not proof of code compliance.";
const REQ_LIMIT: u64 = ddc_ai::limits::REQ_LIST.n();

/// The stored requirements on the tool's scope, narrowed to `cat` when given.
fn req_report(c: &Core, a: &J) -> Result<ddc_model::checks::ReqReport, String> {
    let scope = scope_of(c, &a["scope"])?;
    let mut rep = ddc_model::checks::requirements_report(&c.ed.model, &scope)
        .map_err(|e| e.to_string())?;
    if let Some(w) = a["cat"].as_str() {
        let want = ddc_model::checks::canon_cat(w)
            .ok_or_else(|| format!("unknown category: {}", fmt::text(w)))?;
        rep.cats.retain(|x| x.cat == want);
    }
    Ok(rep)
}

fn req_line(l: &ddc_model::checks::ReqLine) -> J {
    json!({
        "rule": l.rule_id,
        "source": l.origin,
        "param": l.param,
        "required": l.required,
        "kind": l.rule.kind(),
        "allowed": l.rule.text(),
        "phase": l.phase,
        "type": l.scope.type_name,
        "ran": l.ran,
        "checked": l.checked,
        "missing": l.missing(),
        "invalid": l.invalid(),
    })
}

fn no_requirements(c: &Core) -> Result<Option<Done>, String> {
    if !c.ed.model.requirements().map_err(|e| e.to_string())?.is_empty() {
        return Ok(None);
    }
    let mut o = out(c);
    o.put("status", &json!("no_requirements"));
    o.put("hint", &json!("Load a requirements workbook first (load_requirements)."));
    Ok(Some(o.finish().into()))
}

fn check_requirements(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    if let Some(d) = no_requirements(c)? {
        return Ok(d);
    }
    let rep = req_report(c, a)?;
    let scope = scope_of(c, &a["scope"])?;
    let ids: Vec<&str> = rep
        .cats
        .iter()
        .flat_map(|x| x.lines.iter().map(|l| l.rule_id.as_str()))
        .collect();
    let arg = |f: &Finding, k: &str| {
        f.args.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap_or_default()
    };
    let mut found: Vec<Finding> =
        ddc_model::checks::requirement_findings(&c.ed.model, &scope)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|f| ids.contains(&arg(f, "rule").as_str()))
            .collect();
    let total = found.len();
    found.truncate(MAX_FINDINGS);
    let mut list = Vec::with_capacity(found.len());
    for f in found {
        let id = c.remember_finding(&f);
        let els: Vec<String> = f.elements.iter().take(20).map(el_text).collect();
        list.push(json!({
            "ref": id,
            "rule": arg(&f, "rule"),
            "param": arg(&f, "param"),
            "value": arg(&f, "value"),
            "problem": if f.msg_key.ends_with("missing") { "missing" } else { "invalid" },
            "elements": els,
            "fixable": !matches!(f.fix, Fix::None),
        }));
    }
    let cats: Vec<J> = rep
        .cats
        .iter()
        .map(|x| {
            json!({
                "cat": x.cat,
                "compliant": format!("{} of {}", x.compliant, x.total),
                "failing": x.total - x.compliant,
                "lines": x.lines.iter().map(req_line).collect::<Vec<_>>(),
            })
        })
        .collect();
    let (fixable, manual) = rep.fixable_split();
    let mut o = out(c);
    o.put("compliant", &json!(format!("{} of {}", rep.compliant(), rep.total())));
    o.put("categories", &json!(cats));
    o.put("fixable", &json!(fixable));
    o.put("manual", &json!(manual));
    o.put("findings_total", &json!(total));
    o.put("basis", &json!(REQ_BASIS));
    o.list("findings", &list);
    Ok(o.finish().into())
}

fn requirements_report(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    if let Some(d) = no_requirements(c)? {
        return Ok(d);
    }
    let rep = req_report(c, a)?;
    let limit = a["limit"].as_u64().unwrap_or(REQ_LIMIT).min(200) as usize;
    let cats: Vec<J> = rep
        .cats
        .iter()
        .map(|x| {
            let lines: Vec<J> = x
                .lines
                .iter()
                .map(|l| {
                    let mut j = req_line(l);
                    let failing: Vec<J> = l
                        .hits
                        .iter()
                        .take(limit)
                        .map(|h| {
                            json!({
                                "element": el_text(&h.el),
                                "value": h.value,
                                "problem": if h.missing { "missing" } else { "invalid" },
                                "fixable": h.fixable,
                            })
                        })
                        .collect();
                    j["failing"] = json!(failing);
                    j["more"] = json!(l.hits.len().saturating_sub(limit));
                    j
                })
                .collect();
            json!({
                "cat": x.cat,
                "compliant": format!("{} of {}", x.compliant, x.total),
                "lines": lines,
            })
        })
        .collect();
    let export = a["export"].as_bool().unwrap_or(false) && !rep.cats.is_empty();
    if export {
        c.send(FromWorker::Req(super::session::ReqOffer::Export));
    }
    let mut o = out(c);
    o.put("requirements", &json!(rep.requirements()));
    o.put("compliant", &json!(format!("{} of {}", rep.compliant(), rep.total())));
    o.put("categories", &json!(cats));
    if export {
        o.put("export", &json!("offered: the person chooses the file; the saved model is used"));
    }
    o.put("basis", &json!(REQ_BASIS));
    Ok(o.finish().into())
}

fn clash_side(a: &J) -> Result<ClashSide, String> {
    // the model category of floors is slab; stairs have no solid body for the clash search yet
    let cats: Vec<String> = str_list(&a["cats"])
        .into_iter()
        .map(|c| if c == "floor" { "slab".to_string() } else { c })
        .collect();
    if cats.iter().any(|x| x == "stair") {
        return Err(format!(
            "clash does not test stairs (no solid body yet); use {}, floor",
            CLASH_CATS.join(", ")
        ));
    }
    if let Some(bad) = cats.iter().find(|x| !CLASH_CATS.contains(&x.as_str())) {
        return Err(format!(
            "unknown clash category {}; use {}, floor",
            fmt::text(bad),
            CLASH_CATS.join(", ")
        ));
    }
    let only = if a["refs"].is_array() {
        let mut v = vec![];
        for s in str_list(&a["refs"]) {
            let (cat, id) = s
                .split_once(':')
                .ok_or_else(|| format!("bad reference: {}", fmt::text(&s)))?;
            v.push((
                cat.to_string(),
                id.parse::<i64>()
                    .map_err(|_| format!("bad reference: {}", fmt::text(&s)))?,
            ));
        }
        Some(v)
    } else {
        None
    };
    Ok(ClashSide {
        cats,
        level: a["level"].as_i64(),
        only,
    })
}

fn clash(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let params = ClashParams {
        a: clash_side(&a["a"])?,
        b: clash_side(&a["b"])?,
        tolerance: a["tolerance"].as_f64().unwrap_or(0.0),
        ..ClashParams::default()
    };
    // the search itself takes no token, so Cancel lands right after it
    let mut hits = c.ed.model.clash_find(&params).map_err(|e| e.to_string())?;
    if cx.cancel.is_cancelled() {
        return Err("cancelled".to_string());
    }
    hits.sort_by(|x, y| y.volume.total_cmp(&x.volume));
    let list: Vec<J> = hits
        .iter()
        .take(MAX_FINDINGS)
        .map(|h| json!({"a": el_text(&h.a), "b": el_text(&h.b), "volume": fmt::num(h.volume), "level": h.level, "at": [fmt::num((h.lo[0] + h.hi[0]) / 2.0), fmt::num((h.lo[1] + h.hi[1]) / 2.0), fmt::num((h.lo[2] + h.hi[2]) / 2.0)]}))
        .collect();
    let mut o = out(c);
    o.put("total", &json!(hits.len()));
    o.list("hits", &list);
    Ok(o.finish().into())
}

fn query_spatial(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let space = super::qspatial::space_of(&c.ed)?;
    Ok(super::qspatial::answer(&space, a, c.max_chars)?.into())
}

fn query_relations(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let rel = super::qrel::rel_of(&c.ed)?;
    Ok(super::qrel::answer_relations(&rel, a, c.max_chars)?.into())
}

fn query_topology(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let rel = super::qrel::rel_of(&c.ed)?;
    Ok(super::qrel::answer_topology(&rel, a, c.max_chars)?.into())
}

fn get_plan(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let v = c.build_view(false).map_err(|e| e.to_string())?;
    let items: Vec<J> = v
        .items
        .iter()
        .map(|i| json!({"item": i.id, "title": i.title, "why": i.why, "checked": i.checked, "depends_on": i.depends_on, "conflict": i.conflict, "reads": i.sets.reads.summary(), "writes": i.sets.writes.summary()}))
        .collect();
    let (add, chg, del) = plan::counts(&v.diff);
    let issues: Vec<J> = v
        .issues
        .iter()
        .map(|i| json!({"block": i.level == super::validate::IssueLevel::Block, "item": i.item, "key": i.text_key, "args": i.args}))
        .collect();
    let diff: Vec<J> = v
        .diff
        .iter()
        .map(|e| json!({"el": e.label, "kind": format!("{:?}", e.kind), "item": e.item}))
        .collect();
    let mut o = out(c);
    o.put(
        "counts",
        &json!({"created": add, "changed": chg, "deleted": del, "side_effects": v.secondary}),
    );
    o.put("items", &J::from(items));
    o.list("issues", &issues);
    o.list("diff", &diff);
    Ok(o.finish().into())
}

fn list_generators(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let items: Vec<J> = c.gens.iter().map(|g| json!({"id": g.id(), "summary": g.summary(), "variants": g.variants(), "params": g.params().len()})).collect();
    let mut o = out(c);
    o.put_own("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    o.list("generators", &items);
    Ok(o.finish().into())
}

/// Block reference and recipe list, or one recipe's script (for run_script).
fn script_recipes(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    use ddc_script::recipes;
    let mut o = out(c);
    match a.get("name").and_then(J::as_str) {
        // a tool recipe (the lattice tower is a generator, not a script) answers under its name
        Some(name) if recipes::recipe(name).is_none() && ddc_ai::toolset::recipe(name).is_some() => {
            o.put("name", &json!(name));
            o.put_own("recipe", &json!(ddc_ai::toolset::recipe(name)));
            o.put("next", &json!("not a script: follow this recipe"));
        }
        Some(name) => {
            let script = recipes::recipe(name).ok_or_else(|| {
                let all: Vec<&str> = recipes::RECIPES.iter().map(|(n, _)| *n).collect();
                CmdError::wire(
                    ErrorCode::NotFound,
                    format!("no recipe {}; recipes: {}", fmt::text(name), all.join(", ")),
                )
            })?;
            o.put("name", &json!(name));
            o.put_own("summary", &json!(recipes::summary(script)));
            o.put_own("script", &json!(script));
            o.put("next", &json!("run_script with this script (change params or options as asked), then read the realism findings and render_3d"));
        }
        None => {
            // names only: the block reference fills most of the result; the summary comes with a name
            let items: Vec<J> = recipes::RECIPES.iter().map(|(n, _)| json!(n)).collect();
            o.put_own("blocks", &json!(recipes::BLOCKS));
            o.list("recipes", &items);
            o.put_own("not_scripts", &json!({"lattice_tower": "a steel lattice tower on legs (the 300 m kind, curved legs, platforms, joints) is one run_generator call: script_recipes name lattice_tower"}));
        }
    }
    Ok(o.finish().into())
}

fn describe_generator(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let id = str_arg(a, "id")?;
    let g = *c
        .gens
        .iter()
        .find(|g| g.id() == id)
        .ok_or_else(|| {
            CmdError::wire(ErrorCode::NotFound, format!("no generator {}", fmt::text(id)))
        })?;
    let params: Vec<J> = g
        .params()
        .iter()
        .map(|p| {
            let kind = match p.kind {
                ParamKind::Length => json!("length"),
                ParamKind::Area => json!("area"),
                ParamKind::Count => json!("count"),
                ParamKind::Ratio => json!("ratio"),
                ParamKind::RoomList => json!("room_list"),
                ParamKind::Points => json!("points"),
                ParamKind::Rules => json!("rules"),
                ParamKind::Choice(c) => json!(c),
            };
            json!({"name": p.name, "kind": kind, "min": p.min, "max": p.max, "default": p.default, "help": p.help})
        })
        .collect();
    let mut o = out(c);
    o.put("id", &json!(g.id()));
    o.put("summary", &json!(g.summary()));
    o.put("variants", &json!(g.variants()));
    o.put_own("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    o.put_own("params", &J::from(params));
    Ok(o.finish().into())
}

impl From<Reply> for Done {
    fn from(reply: Reply) -> Done {
        Done {
            reply,
            images: vec![],
        }
    }
}

fn list_options(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let set = a["set"].as_str();
    let (base, session_dir) = c.option_paths();
    let (_, db) = super::options_keep::locate(&session_dir);
    // no recipe file yet means no options were ever drafted
    let rows = if db.is_file() {
        let m = ddc_model::Model::open_copy(&base.to_string_lossy()).map_err(|e| e.to_string())?;
        let store = ddc_model::OptionStore::open(&db).map_err(|e| e.to_string())?;
        super::options_keep::list_rows(&m, &store, set)?
    } else {
        vec![]
    };
    let mut o = out(c);
    o.put("options", &json!(rows.len()));
    o.put(
        "note",
        &json!("accepted options are in the model; drafts have not reached it; stale options must be generated again"),
    );
    o.list("items", &rows);
    Ok(o.finish().into())
}

fn list_issues(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let status = a["status"].as_str();
    let limit = a["limit"].as_u64().map_or(20, |n| n.clamp(1, 100) as usize);
    let total = c.ed.model.topic_list(None).map_err(|e| e.to_string())?.len();
    let rows = c.ed.model.topic_list(status).map_err(|e| e.to_string())?;
    let shown: Vec<J> = rows
        .iter()
        .take(limit)
        .map(|r| {
            json!({
                "id": r.id,
                "title": r.title,
                "status": r.status,
                "priority": r.priority,
                "assignee": r.assignee,
                "elements": r.elements,
                "comments": r.comments,
            })
        })
        .collect();
    let mut o = out(c);
    o.put("count", &json!(format!("{} of {}", shown.len(), total)));
    if rows.len() > shown.len() {
        o.put("more", &json!(rows.len() - shown.len()));
    }
    o.list("items", &shown);
    Ok(o.finish().into())
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn keys(cat: &'static str, n: usize) -> Vec<Key> {
        (0..n).map(|i| Key { cat, id: i as i64 }).collect()
    }

    #[test]
    fn a_small_selection_is_listed_as_it_is() {
        assert!(selection_summary(&keys("wall", SELECTION_SHOWN)).is_none());
        assert!(selection_summary(&[]).is_none());
    }

    #[test]
    fn a_big_selection_becomes_counts_per_category() {
        let mut sel = keys("wall", 70);
        sel.extend(keys("door", 30));
        let (by_cat, more) = selection_summary(&sel).expect("over the cap");
        assert_eq!(more, 100 - SELECTION_SHOWN);
        assert_eq!(by_cat["wall"], json!(70));
        assert_eq!(by_cat["door"], json!(30));
    }

    #[test]
    fn the_selection_cursor_continues_right_after_the_listed_ids() {
        assert_eq!(parse_cursor(&selection_next(7)), Some((7, SELECTION_SHOWN)));
        assert_eq!(parse_set("s7"), Some(7));
        assert!(BIG_SELECTION_HINT.contains("selection_next"));
    }

    #[test]
    fn the_big_selection_hint_names_real_tools() {
        for name in ["page", "query_elements"] {
            assert!(BIG_SELECTION_HINT.contains(name), "{name}");
            assert!(catalog::find(name).is_some(), "{name}");
        }
    }
}
