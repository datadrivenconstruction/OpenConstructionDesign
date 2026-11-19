// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The wider create path of the assistant: shafts, grid lines and steel connections through
//! `create_elements`; a type table as its category (`wall_type`, `member_type`, ...) makes
//! types; an item's type named under its table word; levels named, or planned earlier in the
//! same plan by `create_level`, so a building of several new levels is drawn before Apply.

use super::exec::ModelStep;
use super::fmt::{self, Out};
use super::modify_plan::ToolStep;
use super::plan::{ElRef, IdOrNew, NewElem, PlanOp};
use super::session::{Core, Done};
use super::sets::norm_cat;
use super::tools_write::{f, member_level, p2, plan_type, ring, steel_params, text_field, title_of, why_of};
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use serde_json::{json, Value as J};

/// Type table words an item may name its type under, as `describe_category` lists them.
const TYPE_KEYS: [&str; 4] = ["wall_type", "slab_type", "member_type", "comp_type"];

/// Most types one `create_elements` call makes: each is its own plan item.
pub const MAX_TYPES_PER_CALL: usize = 24;

/// The item with `{"wall_type": "X"}` (or another table word) read as its `type`.
pub(super) fn type_alias(o: &J) -> J {
    let mut o = o.clone();
    if o.is_object() && o["type"].is_null() {
        if let Some(s) = TYPE_KEYS.iter().find_map(|k| o.get(*k).and_then(J::as_str)) {
            o["type"] = json!(s);
        }
    }
    o
}

/// A type of `table` the plan creates, by its name in lower case; the last one wins.
pub(super) fn planned_type(c: &Core, table: &str, want: &str) -> Option<ElRef> {
    c.plan.items.iter().rev().find_map(|i| match &i.op {
        PlanOp::NewType(t)
            if t.name.trim().to_lowercase() == want
                && super::plan::type_table(&format!("{}_type", type_word(&t.cat))).ok() == Some(table) =>
        {
            Some(ElRef { cat: table.to_string(), id: IdOrNew::New(t.slot) })
        }
        _ => None,
    })
}

/// The type word of a plan type's category (`floor` types are `slab_type`).
fn type_word(cat: &str) -> &str {
    match norm_cat(cat).unwrap_or(cat) {
        "wall" => "wall",
        "floor" | "roof" | "ceiling" => "slab",
        "column" | "beam" => "member",
        _ => "comp",
    }
}

/// Levels the plan creates before Apply: (id the replay gives it, name).
pub(super) fn planned_levels(c: &Core) -> Vec<(i64, String)> {
    c.plan
        .steps
        .iter()
        .flat_map(|s| s.1.iter())
        .filter_map(|s| match s {
            ModelStep::Tool {
                step: ToolStep::AddLevel { name, .. },
                expect,
            } => expect.iter().find(|r| r.0 == "level").map(|r| (r.1, name.clone())),
            _ => None,
        })
        .collect()
}

/// A level of the model or one the plan creates.
pub(super) fn level_known(c: &Core, id: i64) -> bool {
    c.ed.levels().iter().any(|l| l.0 == id) || planned_levels(c).iter().any(|l| l.0 == id)
}

/// A level given as `level:N`, a bare number in text, or its name (any case).
pub(super) fn level_named(c: &Core, s: &str) -> Result<i64, String> {
    let t = s.trim();
    let num = t.strip_prefix("level:").unwrap_or(t).trim().parse::<i64>().ok();
    if let Some(id) = num.filter(|id| level_known(c, *id)) {
        return Ok(id);
    }
    let want = t.to_lowercase();
    let real = c.ed.levels().into_iter().map(|l| (l.0, l.1));
    real.chain(planned_levels(c))
        .find(|l| l.1.trim().to_lowercase() == want)
        .map(|l| l.0)
        .ok_or_else(|| {
            CmdError::wire(
                ErrorCode::NotFound,
                format!("no level {}: see list_levels, or plan it with create_level", fmt::text(t)),
            )
        })
}

/// Shaft, grid line and steel connection items of `create_elements`.
pub(super) fn new_elem(c: &Core, cat: &str, o: &J, level: i64) -> Result<NewElem, String> {
    Ok(match cat {
        "shaft" => {
            let r = ring(if o["ring"].is_null() { &o["outer"] } else { &o["ring"] }, "ring")?;
            if r.len() < 3 {
                return Err("ring needs at least three points".into());
            }
            if o["top_level"].is_null() {
                return Err("top_level is needed: the level the shaft runs up to".into());
            }
            let top_level = super::tools_write::level_of(c, o, "top_level", level)?;
            NewElem::Shaft { level, top_level, top_offset: f(o, "top_offset", 0.0)?, ring: r }
        }
        "grid" => {
            let name = text_field(o, "name");
            if name.trim().is_empty() {
                return Err("a grid line needs a name".into());
            }
            let (a, b) = (p2(&o["a"], "a")?, p2(&o["b"], "b")?);
            if (a.0 - b.0).hypot(a.1 - b.1) < 1e-6 {
                return Err("a and b of a grid line must differ".into());
            }
            NewElem::Grid { name, a, b }
        }
        "steel_conn" => steel_conn(c, o)?,
        other => return Err(format!("{other} cannot be created here")),
    })
}

fn steel_conn(c: &Core, o: &J) -> Result<NewElem, String> {
    use ddc_geom::steel::ConnKind;
    let word = o["type"].as_str().or(o["kind"].as_str()).ok_or("type must name a connection")?;
    let kind = ConnKind::parse(word.trim()).ok_or_else(|| format!("unknown type {}", fmt::text(word)))?;
    let members = o["members"]
        .as_array()
        .ok_or("members must be a list of column, beam and brace references")?
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
        .collect::<Result<Vec<_>, String>>()?;
    let (lo, hi) = kind.arity();
    if members.len() < lo || members.len() > hi {
        return Err(format!("{} takes {lo}..{hi} members: {}", kind.key(), kind.members_hint()));
    }
    let first = c.ed.levels().first().map(|l| l.0).unwrap_or(0);
    let default = member_level(c, &members[0]).unwrap_or(first);
    Ok(NewElem::SteelConn {
        level: super::tools_write::level_of(c, o, "level", default)?,
        kind: kind.key().to_string(),
        members,
        params: steel_params(&o["params"])?,
    })
}

/// `create_elements` on a type table: every item a type, each its own plan item so a wrong one
/// is dropped alone.
pub(super) fn create_types(c: &mut Core, a: &J, table: &str, cx: &CallCx<'_>) -> Result<Done, String> {
    let list = a["items"].as_array().ok_or("items must be a list")?;
    if list.is_empty() {
        return Err("items is empty".to_string());
    }
    if list.len() > MAX_TYPES_PER_CALL {
        return Err(format!("at most {MAX_TYPES_PER_CALL} types per call"));
    }
    let cat = match table {
        "wall_type" => "wall",
        "slab_type" => "floor",
        "member_type" => "column",
        _ => "comp",
    };
    // every item read before any is planned, and a type the plan refuses takes the ones before
    // it back out: the call adds all its types or none
    let mut named = vec![];
    for (i, o) in list.iter().enumerate() {
        let name = o["name"].as_str().filter(|n| !n.trim().is_empty()).ok_or_else(|| format!("item {}: every type needs a name", i + 1))?;
        if named.iter().any(|n: &&str| n.trim().eq_ignore_ascii_case(name.trim())) {
            return Err(format!("item {}: the name {} comes twice", i + 1, fmt::text(name)));
        }
        named.push(name);
    }
    let (mut items, mut created) = (vec![], vec![]);
    for (o, name) in list.iter().zip(named) {
        let fields = if o["fields"].is_object() { o["fields"].clone() } else { o.clone() };
        let title = match title_of(a) {
            t if t.is_empty() => name.to_string(),
            t => format!("{t}: {name}"),
        };
        let one = json!({"title": title, "why": why_of(a), "cat": cat, "name": name, "fields": fields});
        match plan_type(c, &one, cx) {
            Ok((added, refs)) => {
                items.push(added.0);
                created.extend(refs);
            }
            Err(e) => {
                c.plan.items.retain(|i| !items.contains(&i.id));
                c.plan.slot_owner.retain(|_, owner| !items.contains(owner));
                if let Err(re) = c.replan() {
                    return Err(format!("{e}; taking the earlier types back failed: {re}"));
                }
                return Err(e);
            }
        }
    }
    let mut out = Out::new(c.max_chars);
    out.put("items", &json!(items));
    out.put("created", &json!(created));
    out.put("note", &json!("each type is its own plan item; use the refs as `type` of elements"));
    Ok(out.finish().into())
}

#[cfg(test)]
mod tests {
    use super::super::plan::type_table;
    use super::super::sets::norm_cat;
    use ddc_ai::catalog::CREATE_CATS;

    /// The categories `create_elements` names are exactly the ones it takes: every element
    /// category it creates and every type table.
    #[test]
    fn the_named_categories_are_the_ones_taken() {
        for w in CREATE_CATS {
            assert!(norm_cat(w).is_some() || type_table(w).is_ok(), "{w} is named but not taken");
        }
        for table in ["wall_type", "slab_type", "member_type", "comp_type"] {
            assert_eq!(type_table(table).ok(), Some(table));
            assert!(CREATE_CATS.contains(&table), "{table} is taken but not named");
        }
        // describe_category tells how to fill every element category named
        for w in CREATE_CATS.iter().filter(|w| type_table(w).is_err()) {
            let f = super::super::tools_read::category_fields;
            assert!(norm_cat(w).and_then(f).or_else(|| f(w)).is_some(), "{w} has no fields to describe");
        }
    }
}
