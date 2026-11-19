//! Propose tools: each adds a plan item, compiled and run on the scratch copy at once, so the
//! model hears the exact outcome (`+3 ~0 −0`) and can reference what it just created.

use super::fmt::{self, Out};
use super::materials;
use super::rel_value;
use super::plan::{self, ElRef, FindingRef, NewElem, NewType, PlanOp, Targets, VariantInfo};
use super::rows;
use super::session::{Core, Done, FromWorker, Handler};
use super::sets::{norm_cat, parse_set};
use crate::query;
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use ddc_gen::GenOut;
use ddc_model::{LayerSpec, ParamSpec, ParamValue};
use serde_json::{json, Value as J};

/// Length fields take metres as JSON numbers; a text value stays in project units.
const LENGTH_FIELDS: [&str; 6] = ["width", "height", "sill", "length", "thickness", "offset"];
const DEFAULT_VARIANTS: u64 = 3;
const STOREY: f64 = 3.0;

pub const HANDLERS: [(&str, Handler); 48] = [
    ("define_family", define_family),
    ("create_geometry", create_geometry),
    ("run_script", super::script_tools::run_script),
    ("rerun_recipe", super::script_tools::rerun_recipe),
    ("create_grids", create_grids),
    ("connect_members", connect_members),
    ("move_elements", super::modify_cmds::move_elements),
    ("copy_elements", super::modify_cmds::copy_elements),
    ("rotate_elements", super::modify_cmds::rotate_elements),
    ("mirror_elements", super::modify_cmds::mirror_elements),
    ("scale_elements", super::modify_cmds::scale_elements),
    ("array_elements", super::modify_cmds::array_elements),
    ("offset_wall", super::modify_cmds::offset_wall),
    ("split_wall", super::modify_cmds::split_wall),
    ("pin_elements", super::modify_cmds::pin_elements),
    ("flip_openings", super::modify_cmds::flip_openings),
    ("join_elements", super::modify_cmds::join_elements),
    ("create_level", super::modify_cmds::create_level),
    ("edit_level", super::modify_cmds::edit_level),
    ("delete_level", super::modify_cmds::delete_level),
    ("place_rooms", super::modify_cmds::place_rooms),
    ("place_footings", super::modify_cmds::place_footings),
    ("create_terrain", super::modify_cmds::create_terrain),
    (ddc_ai::catalog::MEASURE_SAVE, super::form_cmds::measure_save),
    (ddc_ai::catalog::EDIT_MASS, super::form_cmds::edit_mass),
    (ddc_ai::catalog::MASS_TO_FAMILY, super::form_cmds::mass_to_family),
    ("edit_wall_type", super::type_lib_tools::edit_wall_type),
    ("copy_types_from", super::type_lib_tools::copy_types_from),
    ("update_elements", update_elements),
    ("replace_type", replace_type),
    ("create_elements", create_elements),
    (ddc_ai::catalog::UNDERLAY_TRACE, super::attach::underlay_trace),
    (ddc_ai::catalog::PLAN_FROM_IMAGE, super::plan_read::plan_from_image),
    ("delete_elements", delete_elements),
    ("create_type", create_type),
    ("add_param", add_param),
    ("load_family", load_family),
    ("run_generator", run_generator),
    ("generate_options", generate_options),
    ("discard_option", discard_option),
    ("apply_fix", apply_fix),
    ("define_rule", define_rule),
    ("create_issue", create_issue),
    ("drop_item", drop_item),
    ("replan_items", super::replan::replan_items),
    ("load_requirements", load_requirements),
    ("create_drawing_set", super::dset_tool::create_drawing_set),
    ("deliver_project", super::deliver_tool::deliver_project),
];

/// The reason is cut to its cap and loses control characters, newlines included.
pub fn why_of(a: &J) -> String {
    fmt::clip(a["why"].as_str().unwrap_or_default().trim(), ddc_ai::catalog::MAX_WHY_CHARS)
}

pub fn title_of(a: &J) -> String {
    fmt::text(a["title"].as_str().unwrap_or_default())
}

/// A target argument. The selection is frozen now: the person may click elsewhere before a replay.
pub fn target_of(c: &Core, a: &J) -> Result<Targets, String> {
    if let Some(f) = a["filter"].as_str() {
        return Ok(Targets::Filter(f.to_string()));
    }
    if let Some(s) = a["set"].as_str() {
        return parse_set(s)
            .map(Targets::Set)
            .ok_or_else(|| format!("bad set: {}", fmt::text(s)));
    }
    if let Some(list) = a["refs"].as_array() {
        let mut out = Vec::with_capacity(list.len());
        for r in list {
            let s = r.as_str().ok_or("refs must be text")?;
            out.extend(ElRef::parse_many(s).map_err(|e| e.0)?);
        }
        return Ok(Targets::Refs(out));
    }
    if a["selection"].as_bool() == Some(true) {
        let refs: Vec<ElRef> = c
            .selection_keys()
            .iter()
            .map(|k| ElRef {
                cat: k.cat.to_string(),
                id: plan::IdOrNew::Id(k.id),
            })
            .collect();
        if refs.is_empty() {
            return Err("nothing is selected".to_string());
        }
        return Ok(Targets::Refs(refs));
    }
    Err("target needs one of filter, set, refs, selection".to_string())
}

/// Category words of the project parameter tables.
pub fn param_cat_word(cat: &str) -> String {
    match cat {
        "floor" => "slab",
        "column" => "col",
        "curtain" => "curtain_wall",
        "mep" => "mep_run",
        other => other,
    }
    .to_string()
}

fn lit(v: &J) -> String {
    match v {
        J::String(s) => fmt::visible(s),
        J::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        J::Null => String::new(),
        other => other.to_string(),
    }
}

fn assign_of(
    c: &Core,
    defs: &[ddc_model::ParamDef],
    key: &str,
    v: &J,
) -> Result<query::Assign, String> {
    if let Some(d) = plan::find_param(defs, key) {
        let relative = match v {
            J::String(s) if rel_value::numeric(d.spec.ptype) => rel_value::parse(&fmt::visible(s)),
            _ => None,
        };
        return Ok(query::Assign {
            key: d.spec.name.clone(),
            val: relative.unwrap_or_else(|| query::Val::Lit(lit(v))),
        });
    }
    let k = query::canon(key);
    let val = match v {
        J::Number(n) if LENGTH_FIELDS.contains(&k.as_str()) => {
            let x = n.as_f64().ok_or("bad number")?;
            query::Val::Lit(plan::length_text(x, c.ed.tool.units))
        }
        J::String(s) => {
            let s = fmt::visible(s);
            let shorthand = || LENGTH_FIELDS.contains(&k.as_str()).then(|| rel_value::parse(&s));
            query::rel(&k, &s)
                .or_else(|| shorthand().flatten())
                .unwrap_or(query::Val::Lit(s))
        }
        other => query::Val::Lit(lit(other)),
    };
    Ok(query::Assign { key: k, val })
}

/// The tool result of a plan item.
pub(super) struct Added {
    pub(super) id: u32,
    pub(super) created: Vec<String>,
    pub(super) counts: (usize, usize, usize),
    /// Elements the item left out, "door:7: reason".
    skipped: Vec<String>,
}

/// `cat:n1`, `cat:n2`, `cat:n3` become `cat:n1..n3`: three or more neighbours of one
/// category cost one short entry instead of one each. Every other entry is kept as it is.
pub(super) fn abbreviate_refs(refs: &[String]) -> Vec<String> {
    let split = |r: &str| -> Option<(String, u32)> {
        let (cat, n) = r.split_once(":n")?;
        Some((cat.to_string(), n.parse().ok()?))
    };
    let mut out = Vec::with_capacity(refs.len());
    let mut i = 0;
    while i < refs.len() {
        let mut j = i;
        if let Some((cat, first)) = split(&refs[i]) {
            while j + 1 < refs.len()
                && split(&refs[j + 1]) == Some((cat.clone(), first + (j + 1 - i) as u32))
            {
                j += 1;
            }
            if j - i >= 2 {
                out.push(format!("{cat}:n{first}..n{}", first + (j - i) as u32));
                i = j + 1;
                continue;
            }
        }
        out.push(refs[i].clone());
        i += 1;
    }
    out
}

pub(super) fn reply(c: &Core, a: &Added) -> Done {
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(a.id));
    o.put(
        "changes",
        &json!({"created": a.counts.0, "changed": a.counts.1, "deleted": a.counts.2}),
    );
    o.put("created", &J::from(abbreviate_refs(&a.created)));
    if !a.skipped.is_empty() {
        o.put("skipped", &J::from(a.skipped.clone()));
    }
    o.finish().into()
}

fn update_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let target = target_of(c, &a["target"])?;
    let set_arg = a["set"].as_object().ok_or("set must be an object")?;
    if set_arg.is_empty() {
        return Err("set is empty".to_string());
    }
    let defs = c.ed.model.params().map_err(|e| e.to_string())?;
    let mut set = Vec::with_capacity(set_arg.len());
    for (k, v) in set_arg {
        set.push(assign_of(c, &defs, k, v)?);
    }
    let added = c.add_item(title_of(a), why_of(a), PlanOp::Assign { target, set }, cx.actor)?;
    let mut out = added_of(added, vec![]);
    out.skipped = c
        .plan
        .skips
        .get(&out.id)
        .map(|l| l.iter().map(|s| format!("{}:{}: {}", s.cat, s.id, fmt::text(&s.why))).collect())
        .unwrap_or_default();
    Ok(reply(c, &out))
}

fn replace_type(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let target = target_of(c, &a["target"])?;
    let to =
        ElRef::parse(a["to"].as_str().ok_or("to must be a type reference")?).map_err(|e| e.0)?;
    plan::type_table(&to.cat).map_err(|e| e.0)?;
    let added = c.add_item(title_of(a), why_of(a), PlanOp::ReplaceType { target, to }, cx.actor)?;
    Ok(reply(c, &added_of(added, vec![])))
}

pub(super) fn added_of(r: (u32, (usize, usize, usize)), created: Vec<String>) -> Added {
    Added {
        id: r.0,
        created,
        counts: r.1,
        skipped: vec![],
    }
}

pub(super) fn f(o: &J, k: &str, default: f64) -> Result<f64, String> {
    match &o[k] {
        J::Null => Ok(default),
        v => v
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| format!("{k} must be a number")),
    }
}

/// A size of zero or less would reach the model as a broken element.
pub(super) fn pos(o: &J, k: &str, default: f64) -> Result<f64, String> {
    let x = f(o, k, default)?;
    if x > 0.0 {
        Ok(x)
    } else {
        Err(format!("{k} must be above zero"))
    }
}

fn pos2(o: &J, k: &str, default: (f64, f64)) -> Result<(f64, f64), String> {
    if o[k].is_null() {
        return Ok(default);
    }
    let p = p2(&o[k], k)?;
    if p.0 > 0.0 && p.1 > 0.0 {
        Ok(p)
    } else {
        Err(format!("{k} must be above zero"))
    }
}

fn need(o: &J, k: &str) -> Result<f64, String> {
    o[k].as_f64()
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("{k} must be a number"))
}

pub(super) fn p2(v: &J, what: &str) -> Result<(f64, f64), String> {
    let a = v
        .as_array()
        .filter(|a| a.len() >= 2)
        .ok_or_else(|| format!("{what} must be [x,y]"))?;
    match (a[0].as_f64(), a[1].as_f64()) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Ok((x, y)),
        _ => Err(format!("{what} must be numbers")),
    }
}

fn p3(v: &J, what: &str) -> Result<(f64, f64, f64), String> {
    let (x, y) = p2(v, what)?;
    let z = match v.get(2) {
        None => 0.0,
        Some(z) => z
            .as_f64()
            .filter(|z| z.is_finite())
            .ok_or_else(|| format!("{what} must be numbers"))?,
    };
    Ok((x, y, z))
}

pub(super) fn ring(v: &J, what: &str) -> Result<Vec<(f64, f64)>, String> {
    let a = v
        .as_array()
        .ok_or_else(|| format!("{what} must be a list of points"))?;
    a.iter().map(|x| p2(x, what)).collect()
}

fn rings(v: &J) -> Result<Vec<Vec<(f64, f64)>>, String> {
    match v {
        J::Null => Ok(vec![]),
        J::Array(a) => a.iter().map(|r| ring(r, "holes")).collect(),
        _ => Err("holes must be a list of rings".to_string()),
    }
}

pub(super) fn level_of(c: &Core, o: &J, k: &str, default: i64) -> Result<i64, String> {
    let id = match &o[k] {
        J::Null => return Ok(default),
        J::String(s) => return super::create_more::level_named(c, s),
        v => v
            .as_i64()
            .ok_or_else(|| format!("{k} must be a level id or name"))?,
    };
    if super::create_more::level_known(c, id) {
        Ok(id)
    } else {
        Err(CmdError::wire(ErrorCode::NotFound, format!("no level {id}: see list_levels")))
    }
}

/// A type by reference, or by name in the expected table; `None` takes the first type.
pub(super) fn type_ref(c: &Core, o: &J, table: &str) -> Result<Option<ElRef>, String> {
    let Some(s) = o["type"].as_str() else {
        return Ok(None);
    };
    if s.contains(':') {
        return ElRef::parse(s).map(Some).map_err(|e| e.0);
    }
    let want = s.trim().to_lowercase();
    if let Some(r) = super::create_more::planned_type(c, table, &want) {
        return Ok(Some(r));
    }
    let all = rows::sql_rows(&c.ed.model, &format!("SELECT id, name FROM {table}"), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    });
    let hit = all
        .into_iter()
        .find(|t| t.1.to_lowercase() == want)
        .ok_or_else(|| format!("no {table} named {}", fmt::text(s)))?;
    Ok(Some(ElRef {
        cat: table.to_string(),
        id: plan::IdOrNew::Id(hit.0),
    }))
}

pub(super) fn text_field(o: &J, k: &str) -> String {
    fmt::text(o[k].as_str().unwrap_or_default())
}

/// The stair shape an item asks for: `shape` names the kind, the numbers sit beside it.
fn stair_shape(o: &J) -> Result<ddc_model::StairShape, String> {
    let left = o["left"].as_bool().unwrap_or(true);
    let first = if o["first"].is_null() {
        None
    } else {
        Some(o["first"].as_u64().ok_or("first is a count of risers")? as usize)
    };
    Ok(match o["shape"].as_str().ok_or("shape must be text")? {
        "straight" => ddc_model::StairShape::Straight,
        "l" => ddc_model::StairShape::L { left, first },
        "u" => ddc_model::StairShape::U { left, first, gap: f(o, "gap", 0.0)? },
        "spiral" => {
            let num = |k: &str| o[k].as_f64().ok_or_else(|| format!("a spiral stair needs {k}"));
            ddc_model::StairShape::Spiral { radius: num("radius")?, core: num("core")?, turn: num("turn")? }
        }
        other => return Err(format!("shape {other} is straight, l, u or spiral")),
    })
}

fn new_elem(c: &Core, cat: &str, o: &J) -> Result<NewElem, String> {
    let dl = c.view.level;
    let level = level_of(c, o, "level", dl)?;
    Ok(match cat {
        "wall" => NewElem::Wall {
            level,
            a: p2(&o["a"], "a")?,
            b: p2(&o["b"], "b")?,
            base_offset: f(o, "base_offset", 0.0)?,
            height: pos(o, "height", STOREY)?,
            wtype: type_ref(c, o, "wall_type")?,
            bulge: f(o, "bulge", 0.0)?,
        },
        "door" | "window" => NewElem::Opening {
            wall: ElRef::parse(o["wall"].as_str().ok_or("wall must be a reference")?)
                .map_err(|e| e.0)?,
            along: need(o, "along")?,
            width: pos(o, "width", if cat == "door" { 0.9 } else { 1.2 })?,
            height: pos(o, "height", if cat == "door" { 2.1 } else { 1.4 })?,
            sill: f(o, "sill", if cat == "door" { 0.0 } else { 0.9 })?,
            kind: cat.to_string(),
        },
        "floor" => NewElem::Slab {
            level,
            stype: type_ref(c, o, "slab_type")?,
            offset: f(o, "offset", 0.0)?,
            outer: ring(&o["outer"], "outer")?,
            holes: rings(&o["holes"])?,
        },
        "room" => NewElem::Room {
            level,
            name: text_field(o, "name"),
            number: text_field(o, "number"),
            point: p2(&o["point"], "point")?,
            height: pos(o, "height", STOREY)?,
        },
        "column" => NewElem::Column {
            level,
            top_level: if o["top_level"].is_null() {
                None
            } else {
                Some(level_of(c, o, "top_level", dl)?)
            },
            mtype: type_ref(c, o, "member_type")?,
            at: p2(&o["at"], "at")?,
            base_offset: f(o, "base_offset", 0.0)?,
            // a top level is the top itself; a storey height only stands in when none is named
            top_offset: f(
                o,
                "top_offset",
                if o["top_level"].is_null() { STOREY } else { 0.0 },
            )?,
            rotation: f(o, "rotation", 0.0)?,
            structural: o["structural"].as_bool().unwrap_or(true),
            top_at: if o["top_at"].is_null() { None } else { Some(p2(&o["top_at"], "top_at")?) },
        },
        "beam" => NewElem::Beam {
            level,
            mtype: type_ref(c, o, "member_type")?,
            a: p3(&o["a"], "a")?,
            b: p3(&o["b"], "b")?,
        },
        "comp" => NewElem::Comp {
            level,
            ctype: type_ref(c, o, "comp_type")?.ok_or("type must name a family type")?,
            at: p3(&o["at"], "at")?,
            rotation: f(o, "rotation", 0.0)?,
            host: match o["host"].as_str() {
                Some(s) => Some(ElRef::parse(s).map_err(|e| e.0)?),
                None => None,
            },
            scale: f(o, "scale", 1.0)?,
            min_clear: f(o, "min_clear", 0.0)?,
        },
        "ceiling" => NewElem::Ceiling {
            level,
            stype: type_ref(c, o, "slab_type")?,
            height: pos(o, "height", STOREY - 0.3)?,
            outer: ring(&o["outer"], "outer")?,
            holes: rings(&o["holes"])?,
        },
        "roof" => NewElem::Roof {
            level,
            stype: type_ref(c, o, "slab_type")?,
            offset: f(o, "offset", 0.0)?,
            overhang: f(o, "overhang", 0.0)?,
            ring: ring(&o["ring"], "ring")?,
            slope: match &o["slope"] {
                J::Null => None,
                v => Some(
                    v.as_f64()
                        .filter(|x| x.is_finite())
                        .ok_or("slope must be a number")?,
                ),
            },
            edges: vec![],
            vault: match &o["vault"] {
                J::Null => None,
                v => Some(super::lower::lift_vault(v)?),
            },
        },
        "stair" => {
            let top_level = level_of(c, o, "top_level", dl)?;
            let origin = p2(&o["origin"], "origin")?;
            let angle = f(o, "angle", 0.0)?;
            if o["shape"].is_null() {
                NewElem::Stair { level, top_level, origin, angle }
            } else {
                NewElem::GenStair {
                    level,
                    top_level,
                    top_offset: f(o, "top_offset", 0.0)?,
                    origin,
                    angle,
                    shape: stair_shape(o)?,
                    width: f(o, "width", 1.2)?,
                    max_riser: f(o, "max_riser", 0.18)?,
                    landing: f(o, "landing", 1.2)?,
                }
            }
        }
        "mep" => {
            let pts = o["points"].as_array().ok_or("points must be a list")?;
            let domain = o["domain"].as_str().unwrap_or("pipe");
            let section = match domain {
                "pipe" => None,
                "duct" | "tray" | "cable_tray" | "conduit" => {
                    let d = match domain {
                        "duct" => ddc_model::MepDomain::Duct,
                        "conduit" => ddc_model::MepDomain::Conduit,
                        _ => ddc_model::MepDomain::CableTray,
                    };
                    let round = d == ddc_model::MepDomain::Conduit || !o["diameter"].is_null();
                    let (w, h) = if round {
                        (f(o, "diameter", 0.0)?, 0.0)
                    } else {
                        (f(o, "width", 0.0)?, f(o, "height", 0.0)?)
                    };
                    if !(w > 0.0) || (!round && !(h > 0.0)) {
                        return Err(format!("{domain} needs diameter, or width and height"));
                    }
                    Some(plan::RunSection { domain: d, w, h })
                }
                other => return Err(format!("unknown domain {}", fmt::text(other))),
            };
            if section.is_some() {
                return Ok(NewElem::Pipe {
                    system: ElRef::parse(o["system"].as_str().ok_or("system must be a reference")?)
                        .map_err(|e| e.0)?,
                    level,
                    material: String::new(),
                    dn: 0,
                    points: pts
                        .iter()
                        .map(|x| p3(x, "points"))
                        .collect::<Result<_, _>>()?,
                    section,
                });
            }
            NewElem::Pipe {
                system: ElRef::parse(
                    o["system"]
                        .as_str()
                        .ok_or("system must be a reference such as mep_system:1")?,
                )
                .map_err(|e| e.0)?,
                level,
                material: o["material"].as_str().unwrap_or("Steel").to_string(),
                dn: o["dn"]
                    .as_u64()
                    .and_then(|d| u32::try_from(d).ok())
                    .filter(|d| *d > 0)
                    .ok_or("dn must be a whole number above zero")?,
                points: pts
                    .iter()
                    .map(|x| p3(x, "points"))
                    .collect::<Result<_, _>>()?,
                section: None,
            }
        }
        "curtain" => NewElem::Curtain {
            level,
            name: text_field(o, "name"),
            a: p2(&o["a"], "a")?,
            b: p2(&o["b"], "b")?,
            base_offset: f(o, "base_offset", 0.0)?,
            height: pos(o, "height", STOREY)?,
            spacing: pos2(o, "spacing", (1.5, 1.5))?,
            mullion: pos2(o, "mullion", (0.05, 0.15))?,
            spandrel: f(o, "spandrel", 0.0)?,
        },
        other => return super::create_more::new_elem(c, other, o, level),
    })
}

pub(super) fn create_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let word = a["cat"].as_str().ok_or("cat must be text")?;
    if let Ok(table) = plan::type_table(word) {
        return super::create_more::create_types(c, a, table, cx);
    }
    let (_, added) = create_raw(c, a, cx)?;
    Ok(reply(c, &added))
}

/// One create item of elements of `cat`: its first new slot (`cat:n<slot0>` is the first item)
/// and what it added.
pub(super) fn create_raw(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<(u32, Added), String> {
    let word = a["cat"].as_str().ok_or("cat must be text")?;
    let cat = norm_cat(word).ok_or_else(|| format!("unknown category: {}", fmt::text(word)))?;
    let list = a["items"].as_array().ok_or("items must be a list")?;
    if list.is_empty() {
        return Err("items is empty".to_string());
    }
    let items = list
        .iter()
        .map(|o| new_elem(c, cat, &super::create_more::type_alias(o)))
        .collect::<Result<Vec<_>, _>>()?;
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(items.len() as u32, item);
    let created: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, e)| format!("{}:n{}", e.cat(), slot0 + i as u32))
        .collect();
    let added = c.add_item(title_of(a), why_of(a), PlanOp::Create { slot0, items }, cx.actor)?;
    Ok((slot0, added_of(added, created)))
}

/// Grid lines from a count, spacing, angle, origin and labelling: one plan item that creates
/// them all, so the person applies or drops the whole set.
fn create_grids(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let set = super::grid_tool::set_of(a)?;
    let lines = crate::grid_set::lines(&set)?;
    let taken: Vec<String> = c
        .ed
        .model
        .grids()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|g| g.name)
        .collect();
    if let Some(l) = lines.iter().find(|l| taken.contains(&l.name)) {
        return Err(format!(
            "grid {} already exists: give the family another start label",
            fmt::text(&l.name)
        ));
    }
    let items: Vec<NewElem> = lines
        .into_iter()
        .map(|l| NewElem::Grid { name: l.name, a: l.a, b: l.b })
        .collect();
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(items.len() as u32, item);
    let created: Vec<String> =
        (0..items.len() as u32).map(|i| format!("grid:n{}", slot0 + i)).collect();
    let added = c.add_item(title_of(a), why_of(a), PlanOp::Create { slot0, items }, cx.actor)?;
    Ok(reply(c, &added_of(added, created)))
}

/// `{k: v}` as catalogue parameter text; numbers are millimetres.
pub(super) fn steel_params(v: &J) -> Result<String, String> {
    let Some(o) = v.as_object() else {
        return match v {
            J::Null => Ok(String::new()),
            J::String(s) => Ok(s.clone()),
            _ => Err("params must be an object".into()),
        };
    };
    let mut p = ddc_geom::steel::Params::default();
    for (k, v) in o {
        let s = match v {
            J::String(s) => s.clone(),
            J::Number(_) | J::Bool(_) => v.to_string(),
            _ => return Err(format!("bad value for {}", fmt::text(k))),
        };
        p.set(k, &s)?;
    }
    Ok(p.text())
}

/// Level of an existing column or beam, for a connection named by its members.
pub(super) fn member_level(c: &Core, r: &ElRef) -> Option<i64> {
    let plan::IdOrNew::Id(id) = r.id else {
        return None;
    };
    let table = match r.cat.as_str() {
        "column" => "col",
        "beam" => "beam",
        _ => return None,
    };
    let q = format!("SELECT level FROM {table} WHERE id={id}");
    rows::sql_rows(&c.ed.model, &q, |r| r.get::<_, i64>(0)).into_iter().next()
}

/// Steel connections as one plan item: the named members with a type, or every free node of
/// a type (`auto`: all types) at a level. Parts are not checked by analysis yet.
fn connect_members(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    use ddc_geom::steel::ConnKind;
    let word = a["type"].as_str().unwrap_or("auto").trim();
    let kind = match word {
        "auto" | "" => None,
        w => Some(ConnKind::parse(w).ok_or_else(|| format!("unknown type {}", fmt::text(w)))?),
    };
    let params = steel_params(&a["params"])?;
    let refs = match &a["members"] {
        J::Null => vec![],
        J::Array(l) => l
            .iter()
            .map(|v| {
                let s = v.as_str().ok_or("members must be references like column:3")?;
                let r = ElRef::parse(s).map_err(|e| e.0)?;
                let cat = norm_cat(&r.cat).or((r.cat.trim() == "brace").then_some("brace")).unwrap_or("");
                if !ddc_model::STEEL_MEMBER_KINDS.contains(&cat) {
                    return Err(format!("{}: only columns, beams and braces connect", fmt::text(s)));
                }
                Ok(ElRef { cat: cat.to_string(), id: r.id })
            })
            .collect::<Result<Vec<_>, String>>()?,
        _ => return Err("members must be a list".into()),
    };
    let mut sheet = None;
    let items: Vec<NewElem> = if refs.is_empty() {
        let level = match &a["level"] {
            J::Null => None,
            _ => Some(level_of(c, a, "level", 0)?),
        };
        let found =
            c.ed.model.steel_conn_candidates(kind, level).map_err(|e| e.to_string())?;
        if found.is_empty() {
            return Err("no free node of that type: every one already has a connection (run_generator places them itself for a steel_frame with joints, e.g. a lattice tower at detail 2 or 3; list steel_conn to see them)".into());
        }
        found
            .into_iter()
            .map(|s| NewElem::SteelConn {
                level: s.level,
                kind: s.kind.key().to_string(),
                members: s
                    .members
                    .iter()
                    .map(|(cat, id)| ElRef { cat: cat.clone(), id: plan::IdOrNew::Id(*id) })
                    .collect(),
                params: params.clone(),
            })
            .collect()
    } else {
        let kind = kind.ok_or("type is needed when members are named")?;
        let (lo, hi) = kind.arity();
        if refs.len() < lo || refs.len() > hi {
            return Err(format!(
                "{} takes {lo}..{hi} members: {}",
                kind.key(),
                kind.members_hint()
            ));
        }
        let first = c.ed.levels().first().map(|l| l.0).unwrap_or(0);
        let default = member_level(c, &refs[0]).unwrap_or(first);
        let level = level_of(c, a, "level", default)?;
        sheet = detailing(c, level, kind, &refs, &params);
        vec![NewElem::SteelConn {
            level,
            kind: kind.key().to_string(),
            members: refs,
            params,
        }]
    };
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(items.len() as u32, item);
    let created: Vec<String> =
        (0..items.len() as u32).map(|i| format!("steel_conn:n{}", slot0 + i)).collect();
    let added = c.add_item(title_of(a), why_of(a), PlanOp::Create { slot0, items }, cx.actor)?;
    let mut o = Out::new(c.max_chars);
    let r = added_of(added, created);
    o.put("item", &json!(r.id));
    o.put("created", &J::from(r.created));
    o.put("status", &json!("not_checked"));
    if let Some(s) = sheet {
        o.put("detailing", &s);
    }
    o.put("note", &json!("connections are not checked by analysis until member forces exist"));
    Ok(o.finish().into())
}

/// The detailing sheet of a connection on existing members, built without storing it:
/// bolt groups, plates, stiffeners, welds and the rule findings, with the note that it is no
/// design calculation. `None` when a member is new in the plan.
fn detailing(c: &Core, level: i64, kind: ddc_geom::steel::ConnKind, refs: &[ElRef], params: &str) -> Option<J> {
    use ddc_geom::steel::detail;
    let members = refs
        .iter()
        .map(|r| match r.id {
            plan::IdOrNew::Id(id) => Some((r.cat.clone(), id)),
            plan::IdOrNew::New(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let spec = ddc_model::SteelConnSpec {
        level,
        kind,
        members,
        params: ddc_geom::steel::Params::parse(params).ok()?,
    };
    let r = c.ed.model.steel_conn_preview(spec).ok()?;
    let Some(conn) = &r.conn else {
        return Some(json!({"error": r.error}));
    };
    let s = detail::sheet(conn, &r.findings);
    let sch = &s.schedule;
    Some(json!({
        "bolt_groups": s.lines(),
        "stiffeners": s.stiffeners,
        "plates": sch.plates.iter().map(|p| format!("{} x{}", p.mark, p.count)).collect::<Vec<_>>(),
        "weld_m": (sch.weld_length() * 100.0).round() / 100.0,
        "rules_kept": s.rules_kept(),
        "findings": s.findings.iter().map(|f| format!("[{}] {}", f.0, f.1)).collect::<Vec<_>>(),
        "not_design": s.note,
    }))
}

fn delete_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let target = target_of(c, &a["target"])?;
    let added = c.add_item(title_of(a), why_of(a), PlanOp::Delete { target }, cx.actor)?;
    Ok(reply(c, &added_of(added, vec![])))
}

fn layers_of(fields: &J, known: &[String]) -> Result<Vec<LayerSpec>, String> {
    let Some(list) = fields["layers"].as_array() else {
        // one layer of `thickness` and `material`, as describe_category offers slab types
        let thickness = match &fields["thickness"] {
            J::Null => 0.2,
            v => v.as_f64().filter(|t| *t > 0.0).ok_or("thickness must be metres above zero")?,
        };
        let material = fmt::visible(fields["material"].as_str().unwrap_or("concrete"));
        return Ok(vec![LayerSpec { material: materials::canon(known, &material), thickness }]);
    };
    list.iter()
        .map(|l| {
            Ok(LayerSpec {
                material: materials::canon(
                    known,
                    &fmt::visible(l["material"].as_str().ok_or("layer needs a material")?),
                ),
                thickness: l["thickness"].as_f64().ok_or("layer needs a thickness")?,
            })
        })
        .collect()
}

fn create_type(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let (added, created) = plan_type(c, a, cx)?;
    Ok(reply(c, &added_of(added, created)))
}

/// One new type as its own plan item: the item and the type's reference.
pub(super) fn plan_type(
    c: &mut Core,
    a: &J,
    cx: &CallCx<'_>,
) -> Result<((u32, (usize, usize, usize)), Vec<String>), String> {
    let word = a["cat"].as_str().ok_or("cat must be text")?;
    let cat = norm_cat(word).ok_or_else(|| format!("unknown category: {}", fmt::text(word)))?;
    let table = match cat {
        "wall" => "wall_type",
        "floor" | "roof" | "ceiling" => "slab_type",
        "column" | "beam" => "member_type",
        "comp" => "comp_type",
        other => return Err(format!("{other} has no types")),
    };
    let fields = &a["fields"];
    let values: Vec<(String, ParamValue)> = fields["values"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| match v {
                    J::Number(n) => n.as_f64().map(|x| (k.clone(), ParamValue::Num(x))),
                    J::String(s) => Some((k.clone(), ParamValue::Text(fmt::visible(s)))),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let known = materials::names(&c.ed.model);
    let item = c.next_item_id();
    let slot = c.plan.reserve_slots(1, item);
    let t = NewType {
        slot,
        cat: cat.to_string(),
        name: fmt::text(a["name"].as_str().ok_or("name must be text")?),
        layers: layers_of(fields, &known)?,
        shape: fields["shape"].as_str().or(fields["section"].as_str()).map(str::to_string),
        material: materials::canon(
            &known,
            &fmt::visible(fields["material"].as_str().unwrap_or("concrete")),
        ),
        family: fields["family"].as_str().map(str::to_string),
        values,
    };
    let added = c.add_item(title_of(a), why_of(a), PlanOp::NewType(t), cx.actor)?;
    Ok((added, vec![format!("{table}:n{slot}")]))
}

fn add_param(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let ptype = a["ptype"]
        .as_str()
        .and_then(plan::param_type)
        .ok_or("unknown ptype")?;
    let mut categories = vec![];
    for w in a["cats"].as_array().ok_or("cats must be a list")? {
        let w = w.as_str().ok_or("cats must be text")?;
        let cat = norm_cat(w).ok_or_else(|| format!("unknown category: {}", fmt::text(w)))?;
        categories.push(param_cat_word(cat));
    }
    let name = fmt::text(a["name"].as_str().ok_or("name must be text")?)
        .trim()
        .to_string();
    // made here, once: a guid made at each run would differ between the copy and the live model
    let guid = Some(ddc_model::new_guid(&name));
    let spec = ParamSpec {
        name,
        ptype,
        per_type: a["per_type"].as_bool().unwrap_or(false),
        unit: text_field(a, "unit"),
        group: text_field(a, "group"),
        guid,
        categories,
    };
    let added = c.add_item(title_of(a), why_of(a), PlanOp::NewParam(spec), cx.actor)?;
    Ok(reply(c, &added_of(added, vec![])))
}

fn load_family(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let def = a["def"].as_str().ok_or("def must be text")?.to_string();
    let added = c.add_item(title_of(a), why_of(a), PlanOp::LoadFamily { def }, cx.actor)?;
    Ok(reply(c, &added_of(added, vec![])))
}

/// A family from structured JSON. It lands as a draft: the person promotes it.
fn define_family(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let def = a["family"].clone();
    super::geo_tools::check_family(&def)?;
    let name = fmt::text(def["name"].as_str().unwrap_or_default());
    let added = c.add_item(title_of(a), why_of(a), PlanOp::DefineFamily { def }, cx.actor)?;
    Ok(reply(c, &added_of(added, vec![format!("family:{name}")])))
}

/// The call's `level` goes to each solid, mass, member or plate operation without `level`, so a
/// model names the level once instead of on every operation.
fn default_level(ops: &mut [J], level: &J) {
    if level.is_null() {
        return;
    }
    for op in ops {
        let solid = matches!(op["action"].as_str(), Some("solid" | "mass" | "member" | "plate"));
        if let (true, Some(m)) = (solid, op.as_object_mut()) {
            m.entry("level").or_insert_with(|| level.clone());
        }
    }
}

fn create_geometry(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let mut ops = a["ops"].as_array().ok_or("ops must be a list")?.clone();
    default_level(&mut ops, &a["level"]);
    super::geo_tools::check_ops(1, &ops)?;
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(ops.len() as u32, item);
    let created: Vec<String> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o["action"].as_str() {
            Some(k @ ("solid" | "mass")) => Some(format!("{k}:n{}", slot0 + i as u32)),
            _ => None,
        })
        .collect();
    let added =
        c.add_item(title_of(a), why_of(a), PlanOp::Geometry { slot0, ops }, cx.actor)?;
    Ok(reply(c, &added_of(added, created)))
}

fn run_generator(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let id = a["id"].as_str().ok_or("id must be text")?.to_string();
    let g = *c
        .gens
        .iter()
        .find(|g| g.id() == id)
        .ok_or_else(|| {
            CmdError::wire(ErrorCode::NotFound, format!("no generator {}", fmt::text(&id)))
        })?;
    let n = a["variants"]
        .as_u64()
        .unwrap_or(DEFAULT_VARIANTS)
        .clamp(1, 3);
    let params = a["params"].clone();
    let mut variants: Vec<VariantInfo> = vec![];
    let mut slots_needed = 0;
    let mut last_err = None;
    let mut built: Vec<GenOut> = vec![];
    for seed in 1..=n {
        match g.build(&params, seed) {
            Ok(result) => {
                // the same result under another criterion is no choice at all
                if built.contains(&result) {
                    continue;
                }
                let metrics: Vec<(String, f64)> = match &result {
                    GenOut::Layout(l) => {
                        slots_needed = slots_needed.max(super::gen_map::slot_count(l));
                        l.metrics.iter().map(|m| (m.0.to_string(), m.1)).collect()
                    }
                    GenOut::Family { def } => ddc_gen::family_metrics(def)
                        .into_iter()
                        .map(|m| (m.0.to_string(), m.1))
                        .collect(),
                    GenOut::Steel { metrics, hall, .. } => {
                        if let Some(h) = hall {
                            slots_needed = slots_needed.max(super::gen_map::hall_slots(h));
                        }
                        metrics.iter().map(|m| (m.0.to_string(), m.1)).collect()
                    }
                };
                let criterion = g
                    .variants()
                    .get(seed as usize - 1)
                    .copied()
                    .unwrap_or_default()
                    .to_string();
                variants.push(VariantInfo {
                    id: seed as u32 - 1,
                    seed,
                    criterion,
                    metrics,
                });
                built.push(result);
            }
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    let Some(first) = variants.first().cloned() else {
        // a parameter or a layout the generator cannot build is the call's arguments, not an
        // editor fault: uncoded it read as "the tool failed inside the editor" (seen live)
        let why = last_err.unwrap_or_else(|| "no variant".to_string());
        return Err(CmdError::wire(ErrorCode::InvalidArgs, format!("params: {why}")));
    };
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(slots_needed, item);
    let op = PlanOp::Generate {
        id: id.clone(),
        params,
        seed: first.seed,
        level: c.view.level,
        slot0,
    };
    let added = c.add_item(fmt::text(&id), why_of(a), op, cx.actor)?;
    let count = variants.len();
    let metrics: Vec<J> = variants
        .iter()
        .map(|v| json!({"variant": v.id, "criterion": v.criterion, "metrics": v.metrics}))
        .collect();
    c.plan.variants = variants;
    c.plan.variant = first.id;
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(added.0));
    o.put(
        "changes",
        &json!({"created": added.1 .0, "changed": added.1 .1, "deleted": added.1 .2}),
    );
    o.put("variants", &json!(count));
    o.put("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    o.list("metrics", &metrics);
    if let Some(GenOut::Steel { why, .. }) = built.first() {
        let why: Vec<J> = why.iter().map(|w| w.to_json()).collect();
        if !why.is_empty() {
            o.list("why", &why);
        }
    }
    Ok(o.finish().into())
}

fn generate_options(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    super::options_gen::handle(c, a)
}

/// Recipes are outside undo and the copy goes to `_archive`, so nothing here needs a changeset.
/// The row stays, marked rejected: the same function as the Options tab Discard.
fn discard_option(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let id = a["id"].as_i64().ok_or("id must be a whole number")?;
    let (_, session_dir) = c.option_paths();
    let (dir, db) = super::options_keep::locate(&session_dir);
    if !db.is_file() {
        let why = format!("no option {id}: call list_options");
        return Err(CmdError::wire(ErrorCode::NotFound, why));
    }
    let store = ddc_model::OptionStore::open(&db).map_err(|e| e.to_string())?;
    let moved = super::options_keep::discard(&store, Some(&dir), id)?;
    c.audit_as(
        cx.actor,
        "tool_write",
        Some(format!("discard option {id}")),
        None,
        "ok",
    );
    let mut o = Out::new(c.max_chars);
    o.put("option", &json!(id));
    o.put("status", &json!("rejected"));
    o.put("archived_copy", &json!(moved.is_some()));
    Ok(o.finish().into())
}

fn apply_fix(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let ids = a["findings"].as_array().ok_or("findings must be a list")?;
    let mut done = vec![];
    let mut failed: Vec<(&str, CmdError)> = vec![];
    for v in ids {
        let id = v.as_str().ok_or("findings must be text")?;
        if cx.cancel.is_cancelled() {
            failed.push((id, CmdError::new(ErrorCode::Cancelled, "cancelled")));
            continue;
        }
        let found: Option<FindingRef> = c.findings.get(id).cloned();
        let Some(f) = found else {
            let unknown = "unknown finding: call run_checks";
            failed.push((id, CmdError::new(ErrorCode::NotFound, unknown)));
            continue;
        };
        let title = fmt::text(&format!("{}: {}", f.rule, id));
        match c.add_item(title, why_of(a), PlanOp::Fix(f), cx.actor) {
            Ok((item, counts)) => done.push(json!({"finding": id, "item": item, "created": counts.0, "changed": counts.1, "deleted": counts.2})),
            Err(why) => failed.push((id, CmdError::from_text(&why))),
        }
    }
    // nothing became an item: the first failure's code and words, with the finding named
    if done.is_empty() {
        let Some((id, e)) = failed.first() else {
            return Err(CmdError::wire(ErrorCode::InvalidArgs, "findings is empty"));
        };
        let why = format!("finding {}: {}", fmt::text(id), e.message);
        return Err(CmdError::wire(e.code, why));
    }
    let failed: Vec<J> = failed
        .iter()
        .map(|(id, e)| json!({"finding": id, "why": e.message, "code": e.code.as_str()}))
        .collect();
    let mut o = Out::new(c.max_chars);
    o.list("items", &done);
    o.list("failed", &failed);
    Ok(o.finish().into())
}

/// Rule storage is not part of the undo journal, so saving waits for the person's confirmation;
/// the scratch copy gets it now so `run_checks` can try the rule in this conversation.
fn define_rule(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let agent = cx.actor.starts_with(crate::ai::session::ACTOR_PREFIX);
    if agent && c.group_open(cx.actor) {
        return Err("a rule is not an undo step, so it cannot be part of a changeset".to_string());
    }
    let text = a["text"].as_str().ok_or("text must be text")?.to_string();
    let mut raw = a["rule"].clone();
    if !raw.is_object() {
        return Err("rule must be an object".to_string());
    }
    raw["source_text"] = J::String(fmt::clip(&text, 400));
    let pack = J::Array(vec![raw]).to_string();
    let rule = ddc_model::checks::load_pack(&pack)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or("no rule")?;
    c.ed.model.save_rule(&rule).map_err(|e| e.to_string())?;
    let id = rule.id.clone();
    c.audit_as(
        cx.actor,
        "tool_write",
        Some(format!("rule {}", fmt::text(&id))),
        None,
        "ok",
    );
    // whoever asked, the person saves it: an agent's rule waits like its other writes
    c.send(FromWorker::RuleDraft { rule, text });
    let saved = false;
    let mut o = Out::new(c.max_chars);
    o.put("rule", &json!(id));
    o.put(
        "status",
        &json!(if saved {
            "saved"
        } else {
            "awaiting_confirmation"
        }),
    );
    if !saved {
        o.put("note", &json!(crate::ai::session::AWAITING_APPLY));
    }
    Ok(o.finish().into())
}

/// Issues live outside the undo journal, so the person confirms a card before anything is saved,
/// whoever asked: the draft carries the linked elements and no camera.
fn create_issue(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let title = title_of(a);
    if title.trim().is_empty() {
        return Err("title must not be empty".to_string());
    }
    let mut t = ddc_model::Topic::new(&title);
    t.description = a["description"]
        .as_str()
        .map(|d| fmt::clip(d.trim(), 4000))
        .unwrap_or_default();
    if let Some(p) = a["priority"].as_str() {
        if !ddc_model::TOPIC_PRIORITIES.contains(&p) {
            return Err(format!("unknown priority: {}", fmt::text(p)));
        }
        t.priority = p.to_string();
    }
    t.assignee = fmt::text(a["assignee"].as_str().unwrap_or_default());
    t.source = "ai".to_string();
    t.author = cx.actor.to_string();
    let mut refs: Vec<ElRef> = vec![];
    for r in a["refs"].as_array().into_iter().flatten() {
        refs.extend(ElRef::parse_many(r.as_str().ok_or("refs must be text")?).map_err(|e| e.0)?);
    }
    if a["selection"].as_bool() == Some(true) {
        refs.extend(c.selection_keys().iter().map(|k| ElRef {
            cat: k.cat.to_string(),
            id: plan::IdOrNew::Id(k.id),
        }));
    }
    for r in refs {
        let plan::IdOrNew::Id(id) = r.id else {
            return Err("an issue can only link elements that already exist".to_string());
        };
        let table = super::sets::table_of(&r.cat).to_string();
        if table.is_empty() || !c.ed.model.topic_elem_alive(&table, id) {
            let gone = format!("no element {}", fmt::text(&format!("{}:{id}", r.cat)));
            return Err(CmdError::wire(ErrorCode::NotFound, gone));
        }
        if !t.elements.iter().any(|e| e.table == table && e.id == id) {
            t.elements.push(ddc_model::TopicElem::local(&table, id));
        }
    }
    let n = t.elements.len();
    c.audit_as(
        cx.actor,
        "tool_write",
        Some(format!("issue {}", fmt::clip(&t.title, 60))),
        None,
        "ok",
    );
    c.send(FromWorker::IssueDraft(Box::new(t)));
    let mut o = Out::new(c.max_chars);
    o.put("elements", &json!(n));
    o.put("status", &json!("awaiting_confirmation"));
    o.put("note", &json!(crate::ai::session::AWAITING_APPLY));
    Ok(o.finish().into())
}

/// Rows of `load_requirements` as the cells of a book row.
fn req_texts(rows: &[J]) -> Vec<ddc_model::checks::ReqText> {
    let s = |r: &J, k: &str| r[k].as_str().unwrap_or_default().to_string();
    rows.iter()
        .map(|r| ddc_model::checks::ReqText {
            cat: s(r, "category"),
            level: s(r, "level"),
            type_name: s(r, "type"),
            phase: s(r, "phase"),
            param: s(r, "parameter"),
            required: s(r, "required"),
            kind: s(r, "rule"),
            value: s(r, "value"),
            unit: s(r, "unit"),
        })
        .collect()
}

#[cfg(windows)]
const NOT_LOCAL: &str = "path must be a file on this computer: a drive path such as \
    C:\\work\\book.xlsx on a local disk, or a name inside the project folder; not a network, \
    device or stream path";
#[cfg(not(windows))]
const NOT_LOCAL: &str = "path must be a file inside the project folder or the home folder, \
    with no '..' steps";
const UNSAVED: &str = "a relative path is read from the project's folder, and this project \
    is not saved yet: save it first or give the full path";

/// A file path an agent names, as an allowlist; relative names are read from the folder of the
/// saved project file `project`, never from the process's working folder. On Windows: `X:\...`
/// on a local fixed drive, or relative inside the project folder. Anything else may make the
/// system connect out and send the person's credentials (`\\host\share`, `\??\UNC\...`, a mapped
/// network drive, a link that leads to a share) or reach a device (`\\.\`, `NUL`) or a stream.
/// Elsewhere: a path that resolves inside the project folder or the home folder.
pub(super) fn local_file_arg(
    p: &str,
    project: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    let base = project.and_then(std::path::Path::parent).filter(|d| !d.as_os_str().is_empty());
    resolve_arg(p, base)
}

#[cfg(windows)]
fn resolve_arg(p: &str, base: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
    path_shape(p).and_then(|shape| resolve_local(p, shape, base))
}

#[cfg(not(windows))]
fn resolve_arg(p: &str, base: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    resolve_posix(p, base, home.as_deref())
}

/// The rules off Windows: no control characters, no `..` step; a relative path joins the
/// project folder; the resolved path (links followed) lies in the project folder or `home`.
#[cfg(not(windows))]
fn resolve_posix(
    p: &str,
    base: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Path};
    let bad = || NOT_LOCAL.to_string();
    if p.is_empty() || p.chars().any(char::is_control) || p.contains("://") {
        return Err(bad());
    }
    let path = Path::new(p);
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(bad());
    }
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.ok_or_else(|| UNSAVED.to_string())?.join(path)
    };
    let canon = std::fs::canonicalize(&full).map_err(|e| format!("cannot open {}: {e}", fmt::text(p)))?;
    let inside = |root: Option<&Path>| {
        root.and_then(|r| std::fs::canonicalize(r).ok()).is_some_and(|r| canon.starts_with(r))
    };
    let ok = if path.is_absolute() { inside(base) || inside(home) } else { inside(base) };
    if ok {
        Ok(canon)
    } else {
        Err(bad())
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    Drive,
    Relative,
}

/// The lexical half of [`local_file_arg`] on Windows: a path
/// that starts with a separator (`\\?\`, `\\.\`, `\??\`, UNC, rooted) is refused, so is a colon
/// past `X:` (drive-relative `C:foo`, alternate data stream), `..`, and a reserved device name.
#[cfg(windows)]
fn path_shape(p: &str) -> Result<Shape, String> {
    let bad = || Err(NOT_LOCAL.to_string());
    if p.is_empty() || p.chars().any(char::is_control) {
        return bad();
    }
    let sep = |x: char| x == '\\' || x == '/';
    let b = p.as_bytes();
    let (shape, rest) = if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && sep(b[2] as char) {
        (Shape::Drive, &p[3..])
    } else if p.starts_with(sep) {
        return bad();
    } else {
        (Shape::Relative, p)
    };
    if rest.contains(':') {
        return bad();
    }
    for part in rest.split(sep) {
        // the system drops trailing dots and spaces: `.. ` and `...` walk up like `..`
        let dots = !part.is_empty() && part.trim_end_matches(['.', ' ']).is_empty();
        if (dots && part != ".") || reserved_device(part) {
            return bad();
        }
    }
    Ok(shape)
}

/// `CON`, `NUL`, `COM1`, `lpt2.txt`, `aux .xlsx`: names that open a device in any folder.
#[cfg(windows)]
fn reserved_device(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |pre: &str| {
        stem.strip_prefix(pre).is_some_and(|d| {
            let mut c = d.chars();
            matches!((c.next(), c.next()), (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None))
        })
    };
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$")
        || numbered("COM")
        || numbered("LPT")
}

/// `X:\` of a drive-absolute path, also behind a `\\?\` prefix; None for anything else.
#[cfg(windows)]
fn drive_root(s: &str) -> Option<String> {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
        .then(|| format!("{}:\\", b[0] as char))
}

/// Only `DRIVE_FIXED` (3) passes: `DRIVE_REMOTE` (4, a mapped share), removable, optical, RAM
/// and missing drives are refused. Asks the drive letter only, nothing on the drive is opened.
#[cfg(windows)]
fn fixed_drive(root: &str) -> Result<(), String> {
    const DRIVE_FIXED: u32 = 3;
    let w: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `w` is a NUL-terminated UTF-16 string that outlives the call.
    let t = unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(w.as_ptr()) };
    if t == DRIVE_FIXED {
        Ok(())
    } else {
        Err(NOT_LOCAL.to_string())
    }
}

/// Walks `full` part by part without following links: each link's target is read, not opened,
/// and has to be a fixed drive path or relative, so no step reaches a share before it is known.
#[cfg(windows)]
fn follow_links(full: &str) -> Result<std::path::PathBuf, String> {
    let bad = || NOT_LOCAL.to_string();
    let parts = |s: &str| -> Vec<String> {
        s.split(['\\', '/']).filter(|x| !x.is_empty()).rev().map(str::to_string).collect()
    };
    let root = drive_root(full).ok_or_else(bad)?;
    fixed_drive(&root)?;
    let mut at = std::path::PathBuf::from(&root);
    let mut todo = parts(&full.strip_prefix(r"\\?\").unwrap_or(full)[3..]);
    let mut hops = 0;
    while let Some(part) = todo.pop() {
        match part.as_str() {
            "." => continue,
            ".." => {
                at.pop();
                continue;
            }
            _ => {}
        }
        let next = at.join(&part);
        let link = std::fs::symlink_metadata(&next).is_ok_and(|m| m.file_type().is_symlink());
        if !link {
            at = next;
            continue;
        }
        hops += 1;
        if hops > 32 {
            return Err(bad());
        }
        let target = std::fs::read_link(&next).map_err(|_| bad())?;
        let t = target.to_string_lossy().into_owned();
        if let Some(r) = drive_root(&t) {
            fixed_drive(&r)?;
            at = std::path::PathBuf::from(&r);
            let rest = t.strip_prefix(r"\\?\").unwrap_or(&t);
            todo.extend(parts(&rest[3..]));
        } else if !t.starts_with(['\\', '/']) && !t.contains(':') {
            todo.extend(parts(&t));
        } else {
            return Err(bad());
        }
    }
    Ok(at)
}

/// `base`: the project folder (None: not saved yet, so a relative path is refused).
#[cfg(windows)]
fn resolve_local(
    p: &str,
    shape: Shape,
    base: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    let base_s = match (shape, base) {
        (Shape::Relative, None) => return Err(UNSAVED.into()),
        (_, b) => b.map(|b| b.to_string_lossy().into_owned()),
    };
    let full = match (shape, &base_s) {
        (Shape::Relative, Some(b)) => format!("{}\\{p}", b.trim_end_matches(['\\', '/'])),
        _ => p.to_string(),
    };
    let real = follow_links(&full)?;
    let canon = std::fs::canonicalize(&real).map_err(|e| format!("cannot open {}: {e}", fmt::text(p)))?;
    // the final path is checked again: it must still be a fixed drive path, not `\\?\UNC\...`
    let root = drive_root(&canon.to_string_lossy()).ok_or_else(|| NOT_LOCAL.to_string())?;
    fixed_drive(&root)?;
    if let (Shape::Relative, Some(b)) = (shape, &base_s) {
        // a project on a share has no local folder to read relative names from
        let home = std::fs::canonicalize(follow_links(b)?).map_err(|e| format!("no project folder: {e}"))?;
        if !canon.starts_with(&home) {
            return Err("a relative path must stay inside the project folder".into());
        }
    }
    Ok(canon)
}

fn load_requirements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    use super::reqbook::{self, Refusal};
    use ddc_io::requirements::{read_requirements, read_texts, BookError};
    let agent = cx.actor.starts_with(crate::ai::session::ACTOR_PREFIX);
    if agent && c.group_open(cx.actor) {
        return Err("requirements are not an undo step, so they cannot join a changeset".into());
    }
    let vocab = reqbook::vocab(&ddc_ui::Catalog::new(ddc_ui::Lang::En));
    let (book, source) = match (a["path"].as_str(), a["rows"].as_array()) {
        (Some(p), None) => {
            if !p.to_ascii_lowercase().ends_with(".xlsx") {
                return Err("path must name a .xlsx workbook".into());
            }
            let path = local_file_arg(p, c.project_file().as_deref())?;
            let b = read_requirements(&path, &vocab, a["sheet"].as_str()).map_err(|e| match e {
                BookError::NoSheet => "no sheet has the requirement columns".to_string(),
                BookError::Read(s) => format!("cannot read the workbook: {}", fmt::text(&s)),
            })?;
            let name: String =
                path.file_name().map_or_else(|| p.into(), |n| n.to_string_lossy().into());
            (b, name)
        }
        (None, Some(rows)) => (read_texts("rows", &req_texts(rows), &vocab), "rows".to_string()),
        _ => return Err("give either path or rows".into()),
    };
    let rules = match reqbook::rules_from_book(&c.ed.model, &book) {
        Ok(r) => r,
        Err(Refusal::Rows(errs)) => {
            let list: Vec<J> = errs
                .iter()
                .map(|e| {
                    json!({
                        "sheet": e.sheet, "row": e.row, "col": e.col,
                        "field": e.field.key(), "code": e.code, "value": fmt::text(&e.arg),
                    })
                })
                .collect();
            let mut o = Out::new(c.max_chars);
            o.put("status", &json!("refused"));
            let all = book.rows.len() + book.errors.len();
            o.put("refused", &json!(format!("{} of {all} rows", errs.len())));
            o.put("note", &json!("Nothing was loaded: fix every listed cell, then load again."));
            o.list("errors", &list);
            return Ok(o.finish().into());
        }
        Err(Refusal::Empty) => return Err("no requirement rows".into()),
        Err(Refusal::NoSheet) => return Err("no sheet has the requirement columns".into()),
        Err(Refusal::Read(s)) => return Err(fmt::text(&s)),
    };
    c.ed.model.save_requirements(&rules).map_err(|e| e.to_string())?;
    c.audit_as(
        cx.actor,
        "tool_write",
        Some(format!("requirements {} from {}", rules.len(), fmt::text(&source))),
        None,
        "ok",
    );
    let mut per: Vec<(String, usize)> = vec![];
    for r in &rules {
        let k = r.scope.cats.first().cloned().unwrap_or_default();
        match per.iter_mut().find(|x| x.0 == k) {
            Some(x) => x.1 += 1,
            None => per.push((k, 1)),
        }
    }
    let n = rules.len();
    c.send(FromWorker::Req(super::session::ReqOffer::Draft { rules, source }));
    let mut o = Out::new(c.max_chars);
    o.put("requirements", &json!(n));
    let cats: Vec<J> = per.iter().map(|x| json!({"cat": x.0, "n": x.1})).collect();
    o.put("categories", &json!(cats));
    o.put("status", &json!("awaiting_confirmation"));
    o.put("note", &json!(crate::ai::session::AWAITING_APPLY));
    Ok(o.finish().into())
}

fn drop_item(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let id = a["item"]
        .as_i64()
        .and_then(|x| u32::try_from(x).ok())
        .ok_or("item must be a number")?;
    let before = c.plan.items.len();
    c.plan.items.retain(|i| i.id != id);
    if c.plan.items.len() == before {
        return Err(CmdError::wire(ErrorCode::NotFound, format!("no item {id}")));
    }
    c.audit_as(
        cx.actor,
        "tool_write",
        Some(format!("drop {id}")),
        None,
        "ok",
    );
    c.replan().map_err(|e| e.to_string())?;
    let unticked: Vec<u32> = c
        .plan
        .items
        .iter()
        .filter(|i| !i.checked)
        .map(|i| i.id)
        .collect();
    let mut o = Out::new(c.max_chars);
    o.put("dropped", &json!(id));
    o.put("items", &json!(c.plan.items.len()));
    o.put("unticked", &json!(unticked));
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn sec_agent_paths_refuse_network_and_device_paths() {
        for bad in [
            r"\\evil.host\share\book.xlsx",
            "//evil.host/share/book.xlsx",
            r"\/evil.host\share\a.ddc",
            r"\\?\UNC\evil.host\s\a.ddc",
            r"\\?\C:\work\a.ddc",
            r"\\.\PhysicalDrive0",
            r"\??\UNC\evil.host\share\a.xlsx",
            r"\??\GLOBALROOT\Device\Mup\evil.host\s\a.ddc",
            r"\??\C:\work\a.xlsx",
            r"\work\a.xlsx",
            "/home/me/a.ddc",
            "C:foo.xlsx",
            r"C:..\a.xlsx",
            r"C:\work\a.xlsx:evil",
            r"a.xlsx:Zone.Identifier",
            r"C:\work\..\..\a.ddc",
            r"..\a.ddc",
            r"sub\... \a.ddc",
            r"C:\work\NUL",
            r"C:\work\nul.xlsx",
            r"con .ddc",
            r"C:\work\COM1.xlsx",
            "LPT\u{b9}.ddc",
            "CONOUT$",
            "file://evil.host/a.xlsx",
            "a\u{0}.xlsx",
            "",
        ] {
            assert!(path_shape(bad).is_err(), "{bad}");
            let project = std::env::temp_dir().join("p.ddc");
            assert!(local_file_arg(bad, Some(project.as_path())).is_err(), "{bad}");
        }
        for (ok, shape) in [
            (r"C:\Users\me\req.xlsx", Shape::Drive),
            ("D:/work/House.ddc", Shape::Drive),
            ("req.xlsx", Shape::Relative),
            (r".\sub\console.xlsx", Shape::Relative),
            (r"C:\work\COM10.xlsx", Shape::Drive),
        ] {
            assert_eq!(path_shape(ok), Ok(shape), "{ok}");
        }
        assert_eq!(drive_root(r"\\?\c:\a").as_deref(), Some(r"c:\"));
        assert_eq!(drive_root(r"\\?\UNC\h\s"), None);
    }

    /// The resolved path: a real local file passes, as drive path and as relative name; a drive
    /// that is not a local fixed disk and a link that leads to a share do not.
    #[cfg(windows)]
    #[test]
    fn sec_agent_paths_resolve_to_local_fixed_drives_only() {
        let dir = std::env::temp_dir().join(format!("ddc-sec-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let home = dir.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        let f = dir.join("req.xlsx");
        std::fs::write(&f, b"x").unwrap();
        let abs = format!(r"{home}\req.xlsx");
        if fixed_drive(&abs[..3]).is_ok() {
            let base = Some(std::path::Path::new(&home));
            assert_eq!(resolve_local(&abs, Shape::Drive, base).unwrap(), f);
            assert_eq!(resolve_local("req.xlsx", Shape::Relative, base).unwrap(), f);
            // relative names come from the project file's folder, not the process's
            let project = dir.join("House.ddc");
            assert_eq!(local_file_arg("req.xlsx", Some(project.as_path())).unwrap(), f);
            assert_eq!(local_file_arg(&abs, None).unwrap(), f, "a full path needs no project");
            let e = local_file_arg("req.xlsx", None).unwrap_err();
            assert!(e.contains("not saved"), "{e}");
            // a link to a share is refused from its target text, never opened
            let link = dir.join("share");
            let _ = std::fs::remove_dir(&link);
            if std::os::windows::fs::symlink_dir(r"\\evil.invalid\s", &link).is_ok() {
                let via = format!(r"{home}\share\a.xlsx");
                assert!(resolve_local(&via, Shape::Drive, base).is_err());
                assert!(resolve_local(r"share\a.xlsx", Shape::Relative, base).is_err());
                let _ = std::fs::remove_dir(&link);
            }
        }
        // a letter with no fixed disk behind it (unused, remote or removable)
        let none = ('D'..='Z').map(|c| format!(r"{c}:\")).find(|r| fixed_drive(r).is_err());
        if let Some(r) = none {
            let p = format!(r"{r}work\a.xlsx");
            assert!(resolve_local(&p, Shape::Drive, Some(std::path::Path::new(&home))).is_err(), "{p}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Off Windows: inside the project folder or home only, no `..`, relative needs a project.
    #[cfg(not(windows))]
    #[test]
    fn sec_agent_paths_stay_in_project_or_home() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("ddc-sec-posix-{}", std::process::id()));
        let (proj, home, other) = (root.join("proj"), root.join("home"), root.join("other"));
        for d in [&proj, &home, &other] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("a.xlsx"), b"x").unwrap();
        }
        let (pb, hb) = (Some(proj.as_path()), Some(home.as_path()));
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        assert_eq!(resolve_posix("a.xlsx", pb, hb).unwrap(), proj.join("a.xlsx"));
        assert_eq!(resolve_posix(&s(&home.join("a.xlsx")), pb, hb).unwrap(), home.join("a.xlsx"));
        assert!(resolve_posix(&s(&proj.join("a.xlsx")), None, hb).is_err(), "outside home");
        assert!(resolve_posix("a.xlsx", None, hb).unwrap_err().contains("not saved"));
        for bad in [
            s(&other.join("a.xlsx")),
            "../other/a.xlsx".to_string(),
            format!("{}/../other/a.xlsx", s(&proj)),
            "/etc/passwd".to_string(),
            "a\u{0}.xlsx".to_string(),
            String::new(),
        ] {
            assert!(resolve_posix(&bad, pb, hb).is_err(), "{bad}");
        }
        // a link out of the project folder is judged by where it leads
        let link = proj.join("out");
        if std::os::unix::fs::symlink(&other, &link).is_ok() {
            assert!(resolve_posix("out/a.xlsx", pb, hb).is_err());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_call_level_fills_only_solids_without_one() {
        let lvl = json!({"by": "id", "cat": "level", "id": 3});
        let own = json!({"by": "id", "cat": "level", "id": 9});
        let mut ops = vec![
            json!({"action": "solid"}),
            json!({"action": "mass", "level": own}),
            json!({"action": "solid", "level": null}),
            json!({"op": "pattern", "of": 0}),
        ];
        default_level(&mut ops, &lvl);
        assert_eq!(ops[0]["level"], lvl);
        assert_eq!(ops[1]["level"], own, "an operation's own level wins");
        assert!(ops[2]["level"].is_null(), "an explicit null stays");
        assert!(ops[3].get("level").is_none(), "other operations are not given one");
        let mut same = ops.clone();
        default_level(&mut same, &J::Null);
        assert_eq!(same, ops);
    }

    #[test]
    fn runs_of_new_references_are_abbreviated() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let a = abbreviate_refs(&v(&["column:n1", "column:n2", "column:n3", "column:n4"]));
        assert_eq!(a, v(&["column:n1..n4"]));
        // two neighbours, other categories, gaps and plain ids stay as they are
        let b = v(&["wall:n1", "wall:n2", "door:n3", "wall:n5", "wall:7", "wall:n8", "wall:n9"]);
        assert_eq!(abbreviate_refs(&b), b);
        let c = abbreviate_refs(&v(&["a:n1", "b:n2", "b:n3", "b:n4", "b:n6"]));
        assert_eq!(c, v(&["a:n1", "b:n2..n4", "b:n6"]));
        assert!(abbreviate_refs(&[]).is_empty());
    }

    #[test]
    fn parameter_categories_use_table_words() {
        assert_eq!(param_cat_word("floor"), "slab");
        assert_eq!(param_cat_word("door"), "door");
        assert_eq!(param_cat_word("column"), "col");
    }

    #[test]
    fn points_accept_two_or_three_numbers() {
        assert_eq!(p2(&json!([1, 2]), "a").unwrap(), (1.0, 2.0));
        assert_eq!(p3(&json!([1, 2]), "a").unwrap(), (1.0, 2.0, 0.0));
        assert_eq!(p3(&json!([1, 2, 3]), "a").unwrap(), (1.0, 2.0, 3.0));
        assert!(p2(&json!([1]), "a").is_err());
        assert!(p2(&json!("x"), "a").is_err());
    }

    #[test]
    fn a_third_coordinate_that_is_no_number_is_refused() {
        assert!(p3(&json!([1, 2, "x"]), "a").is_err());
        assert!(p3(&json!([1, 2, null]), "a").is_err());
    }

    #[test]
    fn sizes_must_be_above_zero() {
        assert_eq!(pos(&json!({}), "width", 0.9).unwrap(), 0.9);
        assert_eq!(pos(&json!({"width": 1.2}), "width", 0.9).unwrap(), 1.2);
        assert!(pos(&json!({"width": -1}), "width", 0.9).is_err());
        assert!(pos(&json!({"width": 0}), "width", 0.9).is_err());
        assert!(pos2(&json!({"spacing": [1, 0]}), "spacing", (1.5, 1.5)).is_err());
    }

    #[test]
    fn literals_lose_hidden_characters() {
        assert_eq!(lit(&json!("EI\u{202e}30")), "EI30");
    }

    #[test]
    fn literals_are_plain_text() {
        assert_eq!(lit(&json!("EI30")), "EI30");
        assert_eq!(lit(&json!(true)), "1");
        assert_eq!(lit(&json!(2.5)), "2.5");
        assert_eq!(lit(&J::Null), "");
    }
}
