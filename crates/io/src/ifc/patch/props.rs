// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Property edits: only the lines that hold the value change. A property or set shared with other
//! elements is not touched; the edited element gets its own copy instead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use ddc_model::Model;
use rusqlite::OptionalExtension;
use serde_json::Value as J;

use super::lines::{
    join_args, list_items, owned, ref_list, single_ref, split_top, string_arg, upper,
};
use super::refs::{probe, Index, Probe};
use super::{doubt, failed, Out, PatchError, PropEdit, Units, DOUBT_STRUCTURE, REASON_PROP};
use crate::ifc::step::{real, text};
use crate::ifc::schema_spec::IfcSchema;
use crate::job::Progress;

const DEFINES: &str = "IFCRELDEFINESBYPROPERTIES";
/// Key prefixes the importer derives from something other than a property set.
const DERIVED: [&str; 3] = ["IFC.", "Material.", "type."];

fn is_text_wrapper(w: &str) -> bool {
    matches!(
        w,
        "" | "IFCLABEL" | "IFCTEXT" | "IFCIDENTIFIER" | "IFCDESCRIPTIVEMEASURE"
    )
}

fn wrapper(v: &[u8]) -> String {
    v.iter()
        .position(|c| *c == b'(')
        .map_or_else(String::new, |i| upper(&v[..i]))
}

fn unit_wrapper(unit: &str) -> Option<&'static str> {
    Some(match unit {
        "m" => "IFCLENGTHMEASURE",
        "m2" => "IFCAREAMEASURE",
        "m3" => "IFCVOLUMEMEASURE",
        "deg" => "IFCPLANEANGLEMEASURE",
        _ => return None,
    })
}

fn integral(x: f64) -> Option<i64> {
    (x.fract() == 0.0 && x.abs() < 9e15).then_some(x as i64)
}

/// The value as STEP text, in the file's units, keeping the old wrapper where it fits.
pub(super) fn value_token(prev: Option<&[u8]>, v: &J, unit: &str, u: &Units) -> Option<Vec<u8>> {
    let w = prev.map(wrapper).unwrap_or_default();
    let number = |x: f64, w: &str| -> Option<String> {
        Some(match w {
            "IFCLENGTHMEASURE" | "IFCPOSITIVELENGTHMEASURE" | "IFCNONNEGATIVELENGTHMEASURE" => {
                format!("{w}({})", real(x / u.length))
            }
            "IFCAREAMEASURE" => format!("{w}({})", real(x / u.area)),
            "IFCVOLUMEMEASURE" => format!("{w}({})", real(x / u.volume)),
            "IFCPLANEANGLEMEASURE" | "IFCPOSITIVEPLANEANGLEMEASURE" => {
                format!("{w}({})", real(x.to_radians() / u.angle))
            }
            "IFCBOOLEAN" | "IFCLOGICAL" => match integral(x)? {
                0 => format!("{w}(.F.)"),
                1 => format!("{w}(.T.)"),
                _ => return None,
            },
            "IFCINTEGER" | "IFCCOUNTMEASURE" => format!("{w}({})", integral(x)?),
            _ => format!("{w}({})", real(x)),
        })
    };
    let s = match v {
        J::Bool(b) => match w.as_str() {
            "IFCLOGICAL" => format!("IFCLOGICAL(.{}.)", if *b { 'T' } else { 'F' }),
            _ => format!("IFCBOOLEAN(.{}.)", if *b { 'T' } else { 'F' }),
        },
        J::Number(n) => {
            let x = n.as_f64()?;
            if !x.is_finite() {
                return None;
            }
            if is_text_wrapper(&w) {
                match (unit_wrapper(unit), n.as_i64()) {
                    (Some(m), _) => number(x, m)?,
                    (None, Some(i)) => format!("IFCINTEGER({i})"),
                    (None, None) => format!("IFCREAL({})", real(x)),
                }
            } else {
                number(x, &w)?
            }
        }
        J::String(t) if t.starts_with('\u{0}') => return None,
        J::String(t) if is_text_wrapper(&w) => {
            let name = if w.is_empty() { "IFCLABEL" } else { w.as_str() };
            format!("{name}({})", text(t))
        }
        _ => return None,
    };
    Some(s.into_bytes())
}

/// New value of a quantity line, in the file's units; `None` when it does not fit.
fn quantity_value(ty: &str, v: &J, u: &Units) -> Option<Vec<u8>> {
    let x = v.as_f64().filter(|x| x.is_finite())?;
    let k = match ty {
        "IFCQUANTITYLENGTH" => u.length,
        "IFCQUANTITYAREA" => u.area,
        "IFCQUANTITYVOLUME" => u.volume,
        "IFCQUANTITYWEIGHT" | "IFCQUANTITYTIME" | "IFCQUANTITYCOUNT" => 1.0,
        _ => return None,
    };
    Some(real(x / k).into_bytes())
}

/// The name of a set or a property from its raw argument.
fn name_of(p: &Probe, at: usize) -> Option<String> {
    split_top(&p.args).get(at).and_then(|a| string_arg(a))
}

fn items_of(p: &Probe) -> Vec<u32> {
    let at = if p.ty == "IFCELEMENTQUANTITY" { 5 } else { 4 };
    split_top(&p.args)
        .get(at)
        .and_then(|a| list_items(a))
        .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
        .unwrap_or_default()
}

/// Row's filler, whose property sets the importer merged into the opening row.
fn fill_guid(model: &Model, source: &str, id: i64) -> Result<Option<String>, PatchError> {
    model
        .sql()
        .query_row(
            "SELECT guid FROM ext_ref WHERE source=?1 AND kind='opening_fill' AND ref=?2",
            rusqlite::params![source, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(failed)
}

fn unit_of(model: &Model, kind: &str, id: i64, key: &str) -> String {
    model
        .sql()
        .query_row(
            "SELECT unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            rusqlite::params![kind, id, key],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
}

/// One change inside a set an owner already has: `None` removes the property.
struct InSet {
    item: u32,
    value: Option<J>,
    unit: String,
    kind: String,
    /// The model row and the edited key.
    id: i64,
    key: String,
}

/// A property with no line in the file: a new set of this name on the owner.
struct NewProp {
    name: String,
    value: J,
    unit: String,
    kind: String,
    id: i64,
    key: String,
}

pub(super) fn owner_token(ix: &Index, old: Option<&[u8]>) -> Option<Vec<u8>> {
    match (ix.schema, old) {
        (_, Some(t)) => Some(t.to_vec()),
        (Some(IfcSchema::Ifc2x3), None) => ix.owner.map(|h| format!("#{h}").into_bytes()),
        _ => Some(b"$".to_vec()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    model: &Model,
    source: &str,
    ix: &Index,
    src: &Path,
    u: &Units,
    edits: &[PropEdit],
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let is_cls = |e: &PropEdit| {
        e.key
            .strip_prefix("Classification.")
            .is_some_and(|s| !s.is_empty())
    };
    if edits.iter().any(is_cls) {
        let cls: Vec<&PropEdit> = edits.iter().filter(|e| is_cls(e)).collect();
        classify(model, source, ix, src, &cls, o, ctl)?;
        let rest: Vec<PropEdit> = edits.iter().filter(|e| !is_cls(e)).cloned().collect();
        return apply(model, source, ix, src, u, &rest, o, ctl);
    }
    // who may hold each edited property: the element and, for an opening row, its filler
    let mut owners: Vec<Vec<u32>> = Vec::with_capacity(edits.len());
    let mut psets: HashSet<u32> = HashSet::new();
    for e in edits {
        let mut ids = vec![];
        if let Some(i) = ix.rooted.get(&e.guid) {
            ids.push(*i);
        }
        if e.kind == "opening" {
            if let Some(g) = fill_guid(model, source, e.id)? {
                if let Some(i) = ix.rooted.get(&g) {
                    ids.push(*i);
                }
            }
        }
        ids.retain(|i| !o.remove.contains(i));
        for i in &ids {
            for r in ix.rels_of(*i, DEFINES) {
                if let Some(p) = split_top(&r.args).get(5).and_then(|v| single_ref(v)) {
                    psets.insert(p);
                }
            }
        }
        owners.push(ids);
    }
    let sets = if psets.is_empty() {
        HashMap::new()
    } else {
        probe(src, &psets, ctl)?
    };
    let wanted: HashSet<u32> = sets.values().flat_map(items_of).collect();
    let items = if wanted.is_empty() {
        HashMap::new()
    } else {
        probe(src, &wanted, ctl)?
    };

    let mut groups: BTreeMap<(u32, u32), Vec<InSet>> = BTreeMap::new();
    let mut fresh: BTreeMap<(u32, String), Vec<NewProp>> = BTreeMap::new();
    for (e, ids) in edits.iter().zip(&owners) {
        let Some(&first) = ids.first() else {
            o.deny(&e.kind, e.id, &format!("p:{}", e.key), REASON_PROP);
            continue;
        };
        let unit = unit_of(model, &e.kind, e.id, &e.key);
        let mut found = false;
        'owners: for &owner in ids {
            for r in ix.rels_of(owner, DEFINES) {
                let Some(ps) = split_top(&r.args).get(5).and_then(|v| single_ref(v)) else {
                    continue;
                };
                let Some(set) = sets.get(&ps) else { continue };
                let Some(rest) = name_of(set, 2)
                    .and_then(|n| e.key.strip_prefix(&format!("{n}.")).map(str::to_string))
                else {
                    continue;
                };
                for it in items_of(set) {
                    if items.get(&it).and_then(|p| name_of(p, 0)).as_deref() == Some(rest.as_str())
                    {
                        let value = (!e.value.is_null()).then(|| e.value.clone());
                        groups.entry((owner, ps)).or_default().push(InSet {
                            item: it,
                            value,
                            unit: unit.clone(),
                            kind: e.kind.clone(),
                            id: e.id,
                            key: e.key.clone(),
                        });
                        found = true;
                        break 'owners;
                    }
                }
            }
        }
        if found || e.value.is_null() {
            continue;
        }
        let split = e
            .key
            .split_once('.')
            .filter(|(a, b)| !a.is_empty() && !b.is_empty());
        match split {
            Some((set, name)) if !DERIVED.iter().any(|d| e.key.starts_with(d)) => {
                fresh
                    .entry((first, set.to_string()))
                    .or_default()
                    .push(NewProp {
                        name: name.to_string(),
                        value: e.value.clone(),
                        unit,
                        kind: e.kind.clone(),
                        id: e.id,
                        key: e.key.clone(),
                    });
            }
            _ => o.deny(&e.kind, e.id, &format!("p:{}", e.key), REASON_PROP),
        }
    }

    let guids = ix.guid_by_id();
    for ((owner, ps), list) in groups {
        let Some(set) = sets.get(&ps) else { continue };
        let guid = guids.get(&owner).copied().unwrap_or_default().to_string();
        edit_set(ix, owner, ps, set, &items, list, &guid, u, o)?;
    }
    for ((owner, name), list) in fresh {
        let guid = guids.get(&owner).copied().unwrap_or_default().to_string();
        add_set(ix, owner, &name, list, &guid, u, o)?;
    }
    Ok(())
}

/// Takes quantity sets off `owners`. Quantities are measured from a shape the file no longer holds;
/// a set others still use stays for them. Returns how many times a set left an element.
pub(super) fn drop_quantities(
    ix: &Index,
    src: &Path,
    owners: &HashSet<u32>,
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<u64, PatchError> {
    let mut emptied: HashSet<u32> = HashSet::new();
    let mut dropped = 0;
    for &id in owners {
        for r in ix.rels_of(id, DEFINES) {
            if o.remove.contains(&r.id) {
                continue;
            }
            let now = o.current_args(r.id, &r.args);
            let a = split_top(&now);
            let Some(set) = a.get(5).and_then(|v| single_ref(v)) else {
                continue;
            };
            if ix.type_name(set) != Some("IFCELEMENTQUANTITY") {
                continue;
            }
            let items = a.get(4).and_then(|v| list_items(v)).unwrap_or_default();
            let ids: Vec<u32> = items.iter().filter_map(|t| single_ref(t)).collect();
            if ids.len() != items.len() || items.is_empty() {
                return Err(doubt(DOUBT_STRUCTURE, format!("relation #{}", r.id)));
            }
            if !ids.contains(&id) {
                continue;
            }
            let left: Vec<u32> = ids.into_iter().filter(|t| *t != id).collect();
            if left.is_empty() {
                o.remove.insert(r.id);
                o.replace.remove(&r.id);
                emptied.insert(set);
            } else {
                let mut args = owned(&a);
                args[4] = ref_list(&left);
                o.swap(r.id, DEFINES, &join_args(&args));
            }
            dropped += 1;
        }
    }
    if emptied.is_empty() {
        return Ok(dropped);
    }
    let mut lines: HashSet<u32> = HashSet::new();
    for (set, p) in probe(src, &emptied, ctl)? {
        if p.referrers.iter().all(|r| o.remove.contains(r)) {
            o.remove.insert(set);
            lines.extend(items_of(&p));
        }
    }
    if !lines.is_empty() {
        for (line, p) in probe(src, &lines, ctl)? {
            if p.referrers.iter().all(|r| o.remove.contains(r)) {
                o.remove.insert(line);
            }
        }
    }
    Ok(dropped)
}

/// A set whose only user is this owner is edited in place; otherwise the owner gets a copy.
#[allow(clippy::too_many_arguments)]
fn edit_set(
    ix: &Index,
    owner: u32,
    ps: u32,
    set: &Probe,
    items: &HashMap<u32, Probe>,
    list: Vec<InSet>,
    guid: &str,
    u: &Units,
    o: &mut Out,
) -> Result<(), PatchError> {
    let at = if set.ty == "IFCELEMENTQUANTITY" { 5 } else { 4 };
    let rel = ix
        .rels_of(owner, DEFINES)
        .find(|r| split_top(&r.args).get(5).and_then(|v| single_ref(v)) == Some(ps))
        .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("relation of set #{ps}")))?;
    let rel_now = o.current_args(rel.id, &rel.args);
    let ra = split_top(&rel_now);
    let related: Vec<u32> = ra
        .get(4)
        .and_then(|v| list_items(v))
        .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
        .unwrap_or_default();
    if o.remove.contains(&rel.id) || !related.contains(&owner) {
        // the owner's shape changed and the set left with it; a value in it has nowhere to go
        for c in list {
            o.deny(&c.kind, c.id, &format!("p:{}", c.key), REASON_PROP);
        }
        return Ok(());
    }
    let exclusive = set.referrers.len() == 1 && related == [owner];

    let mut members = items_of(set);
    let mut line_swaps: Vec<(u32, String, Vec<u8>)> = vec![];
    let mut changed_list = false;
    let mut done: Vec<(String, i64, String)> = vec![];
    let mut gone: Vec<u32> = vec![];
    for c in list {
        let Some(p) = items.get(&c.item) else {
            continue;
        };
        let Some(v) = c.value.as_ref() else {
            members.retain(|m| *m != c.item);
            gone.push(c.item);
            changed_list = true;
            done.push((c.kind, c.id, c.key));
            continue;
        };
        let pa = split_top(&p.args);
        let mut args = owned(&pa);
        let new_value = match p.ty.as_str() {
            "IFCPROPERTYSINGLEVALUE" => pa
                .get(2)
                .and_then(|old| value_token(Some(*old), v, &c.unit, u))
                .map(|t| (2usize, t)),
            t if t.starts_with("IFCQUANTITY") && pa.get(2).is_some_and(|x| *x == b"$") => {
                quantity_value(t, v, u).map(|t| (3usize, t))
            }
            _ => None,
        };
        let Some((i, token)) = new_value.filter(|(i, _)| *i < args.len()) else {
            o.deny(&c.kind, c.id, &format!("p:{}", c.key), REASON_PROP);
            continue;
        };
        args[i] = token;
        let joined = join_args(&args);
        if exclusive && p.referrers.as_slice() == [ps] {
            line_swaps.push((c.item, p.ty.clone(), joined));
        } else {
            let n = o.add(&p.ty, &joined);
            for m in members.iter_mut().filter(|m| **m == c.item) {
                *m = n;
            }
            changed_list = true;
        }
        done.push((c.kind, c.id, c.key));
    }
    if done.is_empty() {
        return Ok(());
    }
    if members.is_empty() {
        // every property of the set is gone: the element leaves the relation, and what nobody
        // else uses goes with it
        let left: Vec<u32> = related.iter().copied().filter(|t| *t != owner).collect();
        if left.is_empty() {
            o.remove.insert(rel.id);
            o.replace.remove(&rel.id);
        } else {
            let mut keep = owned(&ra);
            keep[4] = ref_list(&left);
            o.swap(rel.id, DEFINES, &join_args(&keep));
        }
        if set.referrers.iter().all(|r| o.remove.contains(r)) {
            o.remove.insert(ps);
            o.replace.remove(&ps);
            for it in gone {
                let free = items
                    .get(&it)
                    .is_some_and(|p| p.referrers.iter().all(|r| *r == ps));
                if free {
                    o.remove.insert(it);
                    o.replace.remove(&it);
                }
            }
        }
        for (k, _, _) in done {
            o.edited += 1;
            o.count(&k, guid);
        }
        return Ok(());
    }
    if exclusive {
        for it in &gone {
            if items.get(it).is_some_and(|p| p.referrers.as_slice() == [ps]) {
                o.remove.insert(*it);
                o.replace.remove(it);
            }
        }
    }
    for (id, ty, args) in line_swaps {
        o.swap(id, &ty, &args);
    }
    if changed_list || !exclusive {
        let sa = split_top(&set.args);
        let mut args = owned(&sa);
        if at >= args.len() {
            return Err(doubt(DOUBT_STRUCTURE, format!("set #{ps}")));
        }
        args[at] = ref_list(&members);
        if exclusive {
            o.swap(ps, &set.ty, &join_args(&args));
        } else {
            let g = o.new_guid(&format!("set|{guid}|{ps}"), ix);
            args[0] = format!("'{g}'").into_bytes();
            let new_set = o.add(&set.ty, &join_args(&args));
            let rel_guid = o.new_guid(&format!("rel|{guid}|{ps}"), ix);
            let hist = ra
                .get(1)
                .map(|t| t.to_vec())
                .unwrap_or_else(|| b"$".to_vec());
            let line = join_args(&[
                format!("'{rel_guid}'").into_bytes(),
                hist,
                b"$".to_vec(),
                b"$".to_vec(),
                ref_list(&[owner]),
                format!("#{new_set}").into_bytes(),
            ]);
            o.add(DEFINES, &line);
            let left: Vec<u32> = related.iter().copied().filter(|t| *t != owner).collect();
            if left.is_empty() {
                o.remove.insert(rel.id);
                o.replace.remove(&rel.id);
            } else {
                let mut keep = owned(&ra);
                keep[4] = ref_list(&left);
                o.swap(rel.id, DEFINES, &join_args(&keep));
            }
        }
    }
    for (k, _, _) in done {
        o.edited += 1;
        o.count(&k, guid);
    }
    Ok(())
}

fn add_set(
    ix: &Index,
    owner: u32,
    name: &str,
    list: Vec<NewProp>,
    guid: &str,
    u: &Units,
    o: &mut Out,
) -> Result<(), PatchError> {
    let Some(hist) = owner_token(ix, None) else {
        for p in list {
            o.deny(&p.kind, p.id, &format!("p:{}", p.key), REASON_PROP);
        }
        return Ok(());
    };
    let mut ids = vec![];
    let mut kinds = vec![];
    for p in list {
        match value_token(None, &p.value, &p.unit, u) {
            Some(tok) => {
                let line = join_args(&[
                    text(&p.name).into_bytes(),
                    b"$".to_vec(),
                    tok,
                    b"$".to_vec(),
                ]);
                ids.push(o.add("IFCPROPERTYSINGLEVALUE", &line));
                kinds.push(p.kind);
            }
            None => o.deny(&p.kind, p.id, &format!("p:{}", p.key), REASON_PROP),
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    let g = o.new_guid(&format!("newset|{guid}|{name}"), ix);
    let set = join_args(&[
        format!("'{g}'").into_bytes(),
        hist.clone(),
        text(name).into_bytes(),
        b"$".to_vec(),
        ref_list(&ids),
    ]);
    let set_id = o.add("IFCPROPERTYSET", &set);
    let rg = o.new_guid(&format!("newrel|{guid}|{name}"), ix);
    let rel = join_args(&[
        format!("'{rg}'").into_bytes(),
        hist,
        b"$".to_vec(),
        b"$".to_vec(),
        ref_list(&[owner]),
        format!("#{set_id}").into_bytes(),
    ]);
    o.add(DEFINES, &rel);
    for k in kinds {
        o.edited += 1;
        o.count(&k, guid);
    }
    Ok(())
}

const ASSOC: &str = "IFCRELASSOCIATESCLASSIFICATION";
const CREF: &str = "IFCCLASSIFICATIONREFERENCE";

/// A classification reference as the importer shows it: system name and code.
struct Cref {
    id: u32,
    system: String,
    code: String,
    source: Option<u32>,
    args: Vec<Vec<u8>>,
    referrers: Vec<u32>,
}

fn related_of(args: &[u8]) -> Vec<u32> {
    split_top(args)
        .get(4)
        .and_then(|v| list_items(v))
        .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
        .unwrap_or_default()
}

/// The element leaves a classification relation; the relation goes when nobody is left.
fn leave(o: &mut Out, rel: u32, orig: &[u8], owner: u32) {
    let now = o.current_args(rel, orig);
    let left: Vec<u32> = related_of(&now).into_iter().filter(|t| *t != owner).collect();
    if left.is_empty() {
        o.remove.insert(rel);
        o.replace.remove(&rel);
        return;
    }
    let mut args = owned(&split_top(&now));
    if args.len() > 5 {
        args[4] = ref_list(&left);
        o.swap(rel, ASSOC, &join_args(&args));
    }
}

/// The element joins the relation of `target`, or gets a relation of its own.
fn join_ref(ix: &Index, o: &mut Out, target: u32, owner: u32, guid: &str) -> bool {
    let existing = ix
        .rels_of(target, ASSOC)
        .find(|r| !o.remove.contains(&r.id))
        .map(|r| (r.id, r.args.clone()));
    if let Some((id, orig)) = existing {
        let now = o.current_args(id, &orig);
        let mut list = related_of(&now);
        if !list.contains(&owner) {
            list.push(owner);
        }
        let mut args = owned(&split_top(&now));
        if args.len() < 6 {
            return false;
        }
        args[4] = ref_list(&list);
        o.swap(id, ASSOC, &join_args(&args));
        return true;
    }
    let Some(hist) = owner_token(ix, None) else {
        return false;
    };
    let g = o.new_guid(&format!("clsrel|{guid}|{target}"), ix);
    let line = join_args(&[
        format!("'{g}'").into_bytes(),
        hist,
        b"$".to_vec(),
        b"$".to_vec(),
        ref_list(&[owner]),
        format!("#{target}").into_bytes(),
    ]);
    o.add(ASSOC, &line);
    true
}

/// Classification code edits: one reference per (element, system). A reference only this element
/// uses gets the new code in place; a shared one stays and the element moves to a reference with
/// the code. An empty code takes the element off the system's relation.
fn classify(
    model: &Model,
    source: &str,
    ix: &Index,
    src: &Path,
    edits: &[&PropEdit],
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let wanted: HashSet<u32> = ix
        .rels
        .iter()
        .filter(|r| r.ty == ASSOC)
        .filter_map(|r| split_top(&r.args).get(5).and_then(|v| single_ref(v)))
        .collect();
    let refs = if wanted.is_empty() {
        HashMap::new()
    } else {
        probe(src, &wanted, ctl)?
    };
    let sources: HashSet<u32> = refs
        .values()
        .filter(|p| p.ty == CREF)
        .filter_map(|p| split_top(&p.args).get(3).and_then(|v| single_ref(v)))
        .collect();
    let named = if sources.is_empty() {
        HashMap::new()
    } else {
        probe(src, &sources, ctl)?
    };
    let mut crefs: HashMap<u32, Cref> = HashMap::new();
    for (id, p) in refs.iter().filter(|(_, p)| p.ty == CREF) {
        let a = split_top(&p.args);
        let text_at = |i: usize| a.get(i).and_then(|x| string_arg(x)).unwrap_or_default();
        let source = a.get(3).and_then(|v| single_ref(v));
        let system = source
            .and_then(|s| named.get(&s))
            .and_then(|n| split_top(&n.args).get(3).and_then(|x| string_arg(x)))
            .unwrap_or_else(|| "Classification".into());
        let code = [text_at(1), text_at(2)]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        crefs.insert(
            *id,
            Cref {
                id: *id,
                system,
                code,
                source,
                args: owned(&a),
                referrers: p.referrers.clone(),
            },
        );
    }

    let mut swapped: HashSet<u32> = HashSet::new();
    let mut made: HashMap<(String, String), u32> = HashMap::new();
    for e in edits {
        let system = e.key.strip_prefix("Classification.").unwrap_or_default();
        // an opening row carries its door or window: the classification belongs to the filler
        let filler = if e.kind == "opening" {
            fill_guid(model, source, e.id)?.and_then(|g| ix.rooted.get(&g).copied())
        } else {
            None
        };
        let Some(owner) = filler
            .or_else(|| ix.rooted.get(&e.guid).copied())
            .filter(|i| !o.remove.contains(i))
        else {
            o.will_not.push((e.kind.clone(), REASON_PROP.to_string()));
            continue;
        };
        let code = match &e.value {
            J::String(t) => t.trim().to_string(),
            J::Number(n) => n.to_string(),
            _ => String::new(),
        };
        // relations of an element in this system, as the patch left them so far
        let rels_in = |o: &Out, who: u32| -> Vec<(u32, Vec<u8>, u32)> {
            ix.rels_of(who, ASSOC)
                .filter(|r| !o.remove.contains(&r.id))
                .filter(|r| related_of(&o.current_args(r.id, &r.args)).contains(&who))
                .filter_map(|r| {
                    let rf = split_top(&r.args).get(5).and_then(|v| single_ref(v))?;
                    (crefs.get(&rf)?.system == system).then(|| (r.id, r.args.clone(), rf))
                })
                .collect()
        };
        let cur = rels_in(o, owner);
        let mut done = false;
        if code.is_empty() {
            // the row shows the opening's own code when its filler has none: both leave
            let opening = filler.and(ix.rooted.get(&e.guid).copied()).filter(|i| *i != owner);
            for who in std::iter::once(owner).chain(opening) {
                for (rel, orig, _) in rels_in(o, who) {
                    leave(o, rel, &orig, who);
                    done = true;
                }
            }
        } else if cur.iter().any(|(_, _, rf)| crefs[rf].code == code) {
            continue;
        } else {
            if let Some((rel, orig, rf)) = cur.first() {
                let c = &crefs[rf];
                let alone = c.referrers == [*rel]
                    && related_of(&o.current_args(*rel, orig)) == [owner]
                    && c.args.len() >= 3
                    && !swapped.contains(rf);
                if alone {
                    let mut args = c.args.clone();
                    args[1] = text(&code).into_bytes();
                    args[2] = b"$".to_vec();
                    o.swap(*rf, CREF, &join_args(&args));
                    swapped.insert(*rf);
                    o.edited += 1;
                    o.count(&e.kind, &e.guid);
                    continue;
                }
            }
            let key = (system.to_string(), code.clone());
            let found = crefs
                .values()
                .filter(|c| c.system == system && c.code == code && !swapped.contains(&c.id))
                .map(|c| c.id)
                .min()
                .or_else(|| made.get(&key).copied());
            let target = match found {
                Some(t) => Some(t),
                None => {
                    let proto = cur
                        .first()
                        .and_then(|(_, _, rf)| crefs.get(rf))
                        .or_else(|| {
                            crefs
                                .values()
                                .filter(|c| c.system == system && c.source.is_some())
                                .min_by_key(|c| c.id)
                        });
                    proto.and_then(|p| {
                        let mut args = vec![b"$".to_vec(); p.args.len().max(4)];
                        args[0] = p.args.first().cloned().unwrap_or_else(|| b"$".to_vec());
                        args[1] = text(&code).into_bytes();
                        args[3] = format!("#{}", p.source?).into_bytes();
                        Some(o.add(CREF, &join_args(&args)))
                    })
                }
            };
            let Some(target) = target else {
                o.will_not.push((e.kind.clone(), REASON_PROP.to_string()));
                continue;
            };
            made.insert(key, target);
            // join first: a refused join must not take the element off its old reference
            if !join_ref(ix, o, target, owner, &e.guid) {
                o.will_not.push((e.kind.clone(), REASON_PROP.to_string()));
                continue;
            }
            for (rel, orig, _) in &cur {
                leave(o, *rel, orig, owner);
            }
            done = true;
        }
        if done {
            o.edited += 1;
            o.count(&e.kind, &e.guid);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: Units = Units {
        length: 1e-3,
        angle: 1.0,
        area: 1e-6,
        volume: 1e-9,
    };

    fn tok(prev: Option<&str>, v: J, unit: &str) -> Option<String> {
        value_token(prev.map(str::as_bytes), &v, unit, &MM).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn a_length_goes_back_into_file_units_under_its_wrapper() {
        assert_eq!(
            tok(Some("IFCLENGTHMEASURE(2500.)"), J::from(3.5), "m").as_deref(),
            Some("IFCLENGTHMEASURE(3500.)")
        );
    }

    #[test]
    fn text_keeps_its_wrapper_and_a_number_does_not_fit_a_text_slot_as_text() {
        assert_eq!(
            tok(Some("IFCTEXT('a')"), J::from("b"), "").as_deref(),
            Some("IFCTEXT('b')")
        );
        assert_eq!(
            tok(Some("IFCLABEL('a')"), J::from(5), "").as_deref(),
            Some("IFCINTEGER(5)")
        );
    }

    #[test]
    fn text_does_not_go_into_a_measure() {
        assert_eq!(tok(Some("IFCLENGTHMEASURE(1.)"), J::from("wide"), ""), None);
    }

    #[test]
    fn a_boolean_stays_a_boolean() {
        assert_eq!(
            tok(Some("IFCBOOLEAN(.F.)"), J::from(1), "").as_deref(),
            Some("IFCBOOLEAN(.T.)")
        );
        assert_eq!(tok(Some("IFCBOOLEAN(.F.)"), J::from(7), ""), None);
    }

    #[test]
    fn a_new_length_takes_its_measure_from_the_unit() {
        assert_eq!(
            tok(None, J::from(2.0), "m").as_deref(),
            Some("IFCLENGTHMEASURE(2000.)")
        );
    }
}
