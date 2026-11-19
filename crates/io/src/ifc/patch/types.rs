//! Type changes: an element that now has another type moves between the `IFCRELDEFINESBYTYPE`
//! relations of the file. Only those relation lines change; the type objects stay as they are. A
//! type the file has no object for (made in the editor) is a doubt, as the file cannot name it.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use super::lines::{
    guid_of, join_args, list_items, owned, parse_ent, ref_list, single_ref, split_top, string_arg,
    upper, Stmts,
};
use super::props::owner_token;
use super::refs::{Index, Rel};
use super::{doubt, failed, Out, PatchError, TypeEdit, DOUBT_EDIT, DOUBT_STRUCTURE};
use crate::job::{Progress, CANCELLED};

const REL: &str = "IFCRELDEFINESBYTYPE";
const MREL: &str = "IFCRELASSOCIATESMATERIAL";

/// Model table of the types a column points at; `None` for every other column.
pub(super) fn type_table(field: &str) -> Option<&'static str> {
    match field {
        "wtype" => Some("wall_type"),
        "stype" => Some("slab_type"),
        "mtype" => Some("member_type"),
        _ => None,
    }
}

/// Type object classes an occurrence of `entity` can have.
fn classes_of(entity: &str) -> [String; 2] {
    let base = entity
        .strip_suffix("STANDARDCASE")
        .or_else(|| entity.strip_suffix("ELEMENTEDCASE"))
        .unwrap_or(entity);
    [format!("{base}TYPE"), format!("{base}STYLE")]
}

struct Found {
    id: u32,
    ty: String,
    name: String,
}

fn ends_with_ci(b: &[u8], s: &[u8]) -> bool {
    b.len() >= s.len() && b[b.len() - s.len()..].eq_ignore_ascii_case(s)
}

/// Layer-based material objects of the file: usages, layer sets and layer thicknesses.
#[derive(Default)]
struct Mats {
    usages: BTreeMap<u32, Vec<u8>>,
    sets: HashMap<u32, Vec<u32>>,
    thick: HashMap<u32, f64>,
}

impl Mats {
    fn total(&self, set: u32) -> f64 {
        self.sets
            .get(&set)
            .map_or(0.0, |l| l.iter().filter_map(|x| self.thick.get(x)).sum())
    }
}

/// Rooted type objects of the file whose name is in `names`, and its layer-based materials.
fn scan(
    src: &Path,
    names: &HashSet<&str>,
    ctl: Option<&Arc<Progress>>,
) -> Result<(Vec<Found>, Mats), PatchError> {
    let mut mats = Mats::default();
    let mut st = Stmts::new(File::open(src).map_err(failed)?);
    let (mut in_data, mut n) = (false, 0u64);
    let mut out = vec![];
    while let Some(p) = st.next().map_err(failed)? {
        n += 1;
        if n % 4096 == 0 && ctl.is_some_and(|c| c.is_cancelled()) {
            return Err(failed(CANCELLED));
        }
        if !in_data {
            in_data = p.body.starts_with(b"DATA");
            continue;
        }
        if p.body.starts_with(b"ENDSEC") {
            break;
        }
        let Some(e) = parse_ent(p.body) else { continue };
        if e.ty.eq_ignore_ascii_case(b"IFCMATERIALLAYERSETUSAGE") {
            mats.usages.insert(e.id, e.args.to_vec());
            continue;
        }
        if e.ty.eq_ignore_ascii_case(b"IFCMATERIALLAYERSET") {
            let l = split_top(e.args).first().and_then(|a| list_items(a));
            let l: Option<Vec<u32>> = l.map(|v| v.iter().filter_map(|t| single_ref(t)).collect());
            mats.sets.insert(e.id, l.unwrap_or_default());
            continue;
        }
        if e.ty.eq_ignore_ascii_case(b"IFCMATERIALLAYER") {
            let t = split_top(e.args)
                .get(1)
                .and_then(|a| std::str::from_utf8(a).ok()?.parse::<f64>().ok());
            if let Some(t) = t {
                mats.thick.insert(e.id, t);
            }
            continue;
        }
        if !(ends_with_ci(e.ty, b"TYPE") || ends_with_ci(e.ty, b"STYLE"))
            || guid_of(e.args).is_none()
        {
            continue;
        }
        let name = split_top(e.args).get(2).and_then(|a| string_arg(a));
        if let Some(name) = name.filter(|x| names.contains(x.as_str())) {
            out.push(Found {
                id: e.id,
                ty: upper(e.ty),
                name,
            });
        }
    }
    Ok((out, mats))
}

/// One relation as this patch holds it: its arguments, the members it had and has now.
struct St {
    args: Vec<u8>,
    was: Vec<u32>,
    now: Vec<u32>,
}

fn members(args: &[u8], id: u32) -> Result<Vec<u32>, PatchError> {
    let a = split_top(args);
    let items = a
        .get(4)
        .and_then(|v| list_items(v))
        .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("relation #{id} has no object list")))?;
    items
        .iter()
        .map(|t| single_ref(t))
        .collect::<Option<Vec<u32>>>()
        .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("mixed list in relation #{id}")))
}

pub(super) fn apply(
    ix: &Index,
    src: &Path,
    edits: &[TypeEdit],
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let names: HashSet<&str> = edits.iter().map(|e| e.name.as_str()).collect();
    let (found, mats) = scan(src, &names, ctl)?;
    let mut ms = MatState::default();
    let mut rels: BTreeMap<u32, St> = BTreeMap::new();
    let mut fresh: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for e in edits {
        let Some(&el) = ix.rooted.get(&e.guid) else {
            return Err(doubt(
                DOUBT_STRUCTURE,
                format!("{} {}: not in the file", e.kind, e.id),
            ));
        };
        if o.remove.contains(&el) {
            continue;
        }
        let classes = classes_of(ix.type_name(el).unwrap_or_default());
        let hits: Vec<&Found> = found
            .iter()
            .filter(|t| t.name == e.name && classes.contains(&t.ty))
            .collect();
        let target = match hits.as_slice() {
            [t] => t.id,
            [] => {
                return Err(doubt(
                    DOUBT_EDIT,
                    format!("{} {}: type not in the file", e.kind, e.id),
                ))
            }
            _ => {
                return Err(doubt(
                    DOUBT_EDIT,
                    format!("{} {}: type name is not unique in the file", e.kind, e.id),
                ))
            }
        };
        // every relation of this kind that holds the element, and the one that holds the target
        let mut old: Vec<u32> = vec![];
        let mut target_rel = None;
        for r in ix.rels_of(el, REL).chain(ix.rels_of(target, REL)) {
            if o.remove.contains(&r.id) {
                continue;
            }
            let st = match rels.entry(r.id) {
                Entry::Occupied(x) => x.into_mut(),
                Entry::Vacant(v) => {
                    let args = o.current_args(r.id, &r.args);
                    let was = members(&args, r.id)?;
                    v.insert(St {
                        now: was.clone(),
                        was,
                        args,
                    })
                }
            };
            let a = split_top(&r.args);
            if a.get(5).and_then(|v| single_ref(v)) == Some(target) && target_rel.is_none() {
                target_rel = Some(r.id);
            }
            if st.now.contains(&el) && !old.contains(&r.id) {
                old.push(r.id);
            }
        }
        if target_rel.is_some_and(|t| old.contains(&t)) {
            continue;
        }
        for r in old {
            if let Some(st) = rels.get_mut(&r) {
                st.now.retain(|x| *x != el);
            }
        }
        match target_rel.and_then(|t| rels.get_mut(&t)) {
            Some(st) => st.now.push(el),
            None => {
                let v = fresh.entry(target).or_default();
                if !v.contains(&el) {
                    v.push(el);
                }
            }
        }
        move_material(ix, &mats, &mut ms, e, el, target, o)?;
    }
    write_material(ix, ms, o)?;
    for (id, st) in rels {
        if st.now == st.was {
            continue;
        }
        if st.now.is_empty() {
            o.remove.insert(id);
            o.replace.remove(&id);
            continue;
        }
        let mut parts = owned(&split_top(&st.args));
        parts[4] = ref_list(&st.now);
        o.swap(id, REL, &join_args(&parts));
    }
    for (target, objects) in fresh {
        let Some(hist) = owner_token(ix, None) else {
            return Err(doubt(DOUBT_STRUCTURE, "no owner history for a new relation"));
        };
        let g = o.new_guid(&format!("typerel|#{target}"), ix);
        let args = join_args(&[
            format!("'{g}'").into_bytes(),
            hist,
            b"$".to_vec(),
            b"$".to_vec(),
            ref_list(&objects),
            format!("#{target}").into_bytes(),
        ]);
        o.add(REL, &args);
    }
    Ok(())
}

#[derive(Default)]
struct MatState {
    rels: BTreeMap<u32, St>,
    /// Material (usage or layer set) to the elements that need a new relation to it.
    fresh: BTreeMap<u32, Vec<u32>>,
    /// Usages made here: layer set, remaining arguments, id.
    made: Vec<(u32, Vec<Vec<u8>>, u32)>,
}

fn load<'a>(rels: &'a mut BTreeMap<u32, St>, o: &Out, r: &Rel) -> Result<&'a mut St, PatchError> {
    Ok(match rels.entry(r.id) {
        Entry::Occupied(x) => x.into_mut(),
        Entry::Vacant(v) => {
            let args = o.current_args(r.id, &r.args);
            let was = members(&args, r.id)?;
            v.insert(St {
                now: was.clone(),
                was,
                args,
            })
        }
    })
}

fn relating(args: &[u8]) -> Option<u32> {
    single_ref(split_top(args).get(5)?)
}

/// The element follows its new type into the type's layer set: a usage of that set replaces the
/// old one (made here when the file has none like it), so the layers match the new body.
fn move_material(
    ix: &Index,
    mats: &Mats,
    ms: &mut MatState,
    e: &TypeEdit,
    el: u32,
    target: u32,
    o: &mut Out,
) -> Result<(), PatchError> {
    let mut tset = None;
    for r in ix.rels_of(target, MREL) {
        if o.remove.contains(&r.id) {
            continue;
        }
        let args = o.current_args(r.id, &r.args);
        let Some(x) = relating(&args).filter(|x| mats.sets.contains_key(x)) else {
            continue;
        };
        if members(&args, r.id)?.contains(&target) {
            tset = Some(x);
            break;
        }
    }
    let mut olds: Vec<(u32, u32)> = vec![];
    for r in ix.rels_of(el, MREL) {
        if o.remove.contains(&r.id) {
            continue;
        }
        let st = load(&mut ms.rels, o, r)?;
        if let (true, Some(m)) = (st.now.contains(&el), relating(&st.args)) {
            olds.push((r.id, m));
        }
    }
    let Some(tset) = tset else {
        if olds.iter().any(|(_, m)| mats.usages.contains_key(m)) {
            o.warnings.push(format!(
                "{} {}: the new type has no layer set in the file, the old material stays",
                e.kind, e.id
            ));
        }
        return Ok(());
    };
    for (rid, m) in olds {
        let desired = if let Some(u) = mats.usages.get(&m) {
            let a = split_top(u);
            let from = a.first().and_then(|x| single_ref(x));
            if from == Some(tset) || a.len() < 4 {
                continue;
            }
            let mut tail = owned(&a[1..]);
            let (old_t, new_t) = (mats.total(from.unwrap_or(0)), mats.total(tset));
            let off = std::str::from_utf8(&tail[2])
                .ok()
                .and_then(|t| t.parse::<f64>().ok());
            // a centred layer set stays centred on the reference line
            if let Some(off) = off {
                if old_t > 0.0 && new_t > 0.0 && (off + old_t / 2.0).abs() <= 1e-6 * old_t.max(1.0) {
                    tail[2] = format!("{:.6}", -new_t / 2.0).into_bytes();
                }
            }
            let same = mats.usages.iter().find(|(_, b)| {
                let b = split_top(b);
                b.first().and_then(|x| single_ref(x)) == Some(tset) && owned(&b[1..]) == tail
            });
            if let Some((id, _)) = same {
                *id
            } else if let Some((_, _, id)) = ms.made.iter().find(|(s, t, _)| *s == tset && *t == tail) {
                *id
            } else {
                let mut all = vec![format!("#{tset}").into_bytes()];
                all.extend(tail.iter().cloned());
                let id = o.add("IFCMATERIALLAYERSETUSAGE", &join_args(&all));
                ms.made.push((tset, tail, id));
                id
            }
        } else if mats.sets.contains_key(&m) && m != tset {
            tset
        } else {
            continue;
        };
        if let Some(st) = ms.rels.get_mut(&rid) {
            st.now.retain(|x| *x != el);
        }
        let mut joined = false;
        for r in ix.rels_of(desired, MREL) {
            if o.remove.contains(&r.id) {
                continue;
            }
            let st = load(&mut ms.rels, o, r)?;
            if relating(&st.args) == Some(desired) {
                if !st.now.contains(&el) {
                    st.now.push(el);
                }
                joined = true;
                break;
            }
        }
        if !joined {
            let v = ms.fresh.entry(desired).or_default();
            if !v.contains(&el) {
                v.push(el);
            }
        }
    }
    Ok(())
}

fn write_material(ix: &Index, ms: MatState, o: &mut Out) -> Result<(), PatchError> {
    for (id, st) in ms.rels {
        if st.now == st.was {
            continue;
        }
        if st.now.is_empty() {
            o.remove.insert(id);
            o.replace.remove(&id);
            continue;
        }
        let mut parts = owned(&split_top(&st.args));
        parts[4] = ref_list(&st.now);
        o.swap(id, MREL, &join_args(&parts));
    }
    for (material, objects) in ms.fresh {
        let Some(hist) = owner_token(ix, None) else {
            return Err(doubt(DOUBT_STRUCTURE, "no owner history for a new relation"));
        };
        let g = o.new_guid(&format!("matrel|#{material}"), ix);
        let args = join_args(&[
            format!("'{g}'").into_bytes(),
            hist,
            b"$".to_vec(),
            b"$".to_vec(),
            ref_list(&objects),
            format!("#{material}").into_bytes(),
        ]);
        o.add(MREL, &args);
    }
    Ok(())
}
