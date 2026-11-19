//! Frozen targets: filter text and frozen sets become concrete refs before the executor runs.
//!
//! The model executor resolves refs only (model/src/ops_apply.rs refuses `Filter` and `Set`).
//! An envelope is frozen once, against the model it is previewed or applied on, and the frozen
//! copy is what runs, is journaled and is replayed by redo: Apply touches exactly the ids the
//! preview showed. Every filter is read before any op runs, so elements the same envelope
//! creates are never matched by it.

use super::plan::filter_keys;
use super::sets::{parse_set, Key, Sets};
use crate::editor::Editor;
use ddc_ops::{Cat, Code, Envelope, Op, OptionOp, Ref, Rejection, Targets};
use std::collections::BTreeSet;

/// The targets of an op and their field name on the wire.
fn target_of(op: &Op) -> Option<(&Targets, &'static str)> {
    Some(match op {
        Op::Set { target, .. }
        | Op::SetType { target, .. }
        | Op::Delete { target }
        | Op::Transform { target, .. }
        | Op::Option(OptionOp::Assign { target, .. }) => (target, "/target"),
        Op::Pattern { source, .. } => (source, "/source"),
        _ => return None,
    })
}

fn target_mut(op: &mut Op) -> Option<&mut Targets> {
    Some(match op {
        Op::Set { target, .. }
        | Op::SetType { target, .. }
        | Op::Delete { target }
        | Op::Transform { target, .. }
        | Op::Option(OptionOp::Assign { target, .. }) => target,
        Op::Pattern { source, .. } => source,
        _ => return None,
    })
}

/// True when some op targets a filter or a set by name.
pub fn needs_freeze(env: &Envelope) -> bool {
    env.groups
        .iter()
        .flat_map(|g| g.ops.iter())
        .filter_map(target_of)
        .any(|(t, _)| !matches!(t, Targets::Refs { .. }))
}

/// The envelope with every `Filter` and `Set` target replaced by the refs it matches on `ed`.
/// Refused when a filter does not parse, matches nothing, or names a set that is unknown or
/// empty; `sets` is `None` where no frozen sets exist (the live editor).
pub fn freeze(ed: &Editor, sets: Option<&Sets>, env: &Envelope) -> Result<Envelope, Rejection> {
    let mut out = env.clone();
    if !needs_freeze(env) {
        return Ok(out);
    }
    for (gi, g) in out.groups.iter_mut().enumerate() {
        for (oi, op) in g.ops.iter_mut().enumerate() {
            let path = match target_of(op) {
                Some((Targets::Refs { .. }, _)) | None => continue,
                Some((_, p)) => p,
            };
            let Some(t) = target_mut(op) else { continue };
            let refs = match t {
                Targets::Refs { .. } => continue,
                Targets::Filter { cat, query } => filter_refs(ed, *cat, query),
                Targets::Set { name } => set_refs(ed, sets, name),
            }
            .map_err(|r| r.field(path).in_op(env, gi, oi))?;
            *t = Targets::Refs { refs };
        }
    }
    Ok(out)
}

fn filter_refs(ed: &Editor, cat: Option<Cat>, query: &str) -> Result<Vec<Ref>, Rejection> {
    let keys = filter_keys(ed, query)
        .map_err(|e| Rejection::new(Code::Invalid).got(format!("filter \"{query}\": {e}")))?;
    let refs = key_refs(&keys, cat).map_err(|e| Rejection::new(Code::Unsupported).got(e))?;
    if refs.is_empty() {
        return Err(Rejection::new(Code::NotFound)
            .got(format!("no element matches the filter \"{query}\"")));
    }
    Ok(refs)
}

/// A set member deleted since the set was frozen refuses the whole envelope: running the rest
/// would apply to fewer elements than the person picked.
fn set_refs(ed: &Editor, sets: Option<&Sets>, name: &str) -> Result<Vec<Ref>, Rejection> {
    // sets live in the assistant worker; the live editor's Apply never sees them
    let Some(sets) = sets else {
        return Err(Rejection::new(Code::Unsupported).got(format!(
            "set \"{name}\" is not available at Apply: only the assistant session holds its \
             elements, so the plan must name them as refs"
        )));
    };
    let Some(s) = parse_set(name).and_then(|n| sets.get(n)) else {
        return Err(Rejection::new(Code::NotFound).got(format!("no frozen set named \"{name}\"")));
    };
    let gone: Vec<String> = s.keys.iter().filter(|k| !key_exists(ed, k)).map(|k| k.text()).collect();
    if !gone.is_empty() {
        return Err(Rejection::new(Code::NotFound).got(format!(
            "set \"{name}\": {} no longer exist{} in the model",
            gone.join(", "),
            if gone.len() == 1 { "s" } else { "" }
        )));
    }
    let refs = key_refs(&s.keys, None).map_err(|e| Rejection::new(Code::Unsupported).got(e))?;
    if refs.is_empty() {
        return Err(Rejection::new(Code::NotFound).got(format!("set \"{name}\" is empty")));
    }
    Ok(refs)
}

/// Categories without a table of their own here (types) are left to the executor's own check.
fn key_exists(ed: &Editor, k: &Key) -> bool {
    let t = k.table();
    t.is_empty()
        || ed
            .model
            .sql()
            .prepare_cached(&format!("SELECT 1 FROM {t} WHERE id=?1"))
            .and_then(|mut st| st.exists([k.id]))
            .unwrap_or(false)
}

/// `door` targets accept the door rows of the opening table, `opening` accepts doors and windows.
fn cat_ok(want: Cat, got: Cat) -> bool {
    want == got || (want == Cat::Opening && matches!(got, Cat::Door | Cat::Window))
}

/// Keys as id refs, restricted to `cat`, sorted and without repeats so the result is the same
/// whatever order the rows came in.
pub(crate) fn key_refs(keys: &[Key], cat: Option<Cat>) -> Result<Vec<Ref>, String> {
    let mut out = BTreeSet::new();
    for k in keys {
        let c = super::lower::cat_of(k.cat)?;
        if cat.is_none_or(|w| cat_ok(w, c)) {
            out.insert((c, k.id));
        }
    }
    Ok(out.into_iter().map(|(cat, id)| Ref::Id { cat, id }).collect())
}

#[cfg(test)]
#[path = "freeze_tests.rs"]
mod tests;
