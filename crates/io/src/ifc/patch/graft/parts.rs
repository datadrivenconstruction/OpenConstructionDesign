//! An edited whole with aggregated parts (stairs and ramps). The whole takes the ordinary edit;
//! parts are matched to the export by GlobalId. A part whose lines say what the export says keeps
//! them byte for byte; a changed one gets its line replaced under its id; a new one is appended;
//! one the model no longer has is removed with the lines only it used. The one aggregation is
//! rewritten only when its list changes. Anything else naming a part is a doubt.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use super::super::lines::{
    for_each_ref, guid_of, join_args, list_items, owned, ref_list, single_ref, split_top,
    string_arg, upper,
};
use super::super::refs::{probe, Index, Probe};
use super::super::{
    doubt, GeomEdit, Out, PatchError, DOUBT_EDIT, DOUBT_NESTED, DOUBT_SHARED, DOUBT_STRUCTURE,
    REASON_SCHEMA,
};
use super::{
    add_rels, edit_one, first_end_is, is_context, items_at, parts_of, related, set_named, Tx,
    AGGREGATES, DEFINES, FILLS, NESTS, VOIDS,
};
use crate::job::Progress;

/// Passes over the file for one tree; deeper than this is not a representation.
const MAX_DEPTH: usize = 48;

/// Kinds written as a whole with aggregated parts.
pub(super) const WHOLE_KINDS: [&str; 2] = ["stair", "ramp"];

/// Property set holding the exact row of a kind, which the importer rebuilds the element from.
pub(super) fn data_set(kind: &str) -> Option<&'static str> {
    match kind {
        "stair" => Some("DDC_Stair"),
        "ramp" => Some("DDC_Ramp"),
        "railing" => Some("DDC_Railing"),
        _ => None,
    }
}

/// Lines of the file under `roots`, with who points at each. Contexts and rooted entities other
/// than the roots are read but not followed.
fn read_trees(
    src: &Path,
    roots: &HashSet<u32>,
    ctl: Option<&Arc<Progress>>,
) -> Result<HashMap<u32, Probe>, PatchError> {
    let mut all: HashMap<u32, Probe> = HashMap::new();
    let mut next: HashSet<u32> = roots.clone();
    let mut passes = 0;
    while !next.is_empty() {
        passes += 1;
        if passes > MAX_DEPTH {
            return Err(doubt(DOUBT_STRUCTURE, "part representation deeper than expected"));
        }
        let got = probe(src, &next, ctl)?;
        let mut more = HashSet::new();
        for (id, p) in got {
            if p.ty.is_empty() {
                return Err(doubt(DOUBT_STRUCTURE, format!("#{id} is not in the file")));
            }
            let leaf = is_context(&p.ty) || (!roots.contains(&id) && guid_of(&p.args).is_some());
            if !leaf {
                for_each_ref(&p.args, &mut |r| {
                    if !all.contains_key(&r) && !next.contains(&r) {
                        more.insert(r);
                    }
                });
            }
            all.insert(id, p);
        }
        next = more;
    }
    Ok(all)
}

enum Tok<'a> {
    Ref(u32),
    Num(f64),
    Raw(&'a [u8]),
}

/// Tokens of an argument text: references, numbers, and everything else verbatim.
fn toks(b: &[u8]) -> Vec<Tok<'_>> {
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c == b'\'' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(Tok::Raw(&b[start..i]));
        } else if c == b'#' {
            let n = b[i + 1..].iter().take_while(|x| x.is_ascii_digit()).count();
            i += 1 + n;
            match std::str::from_utf8(&b[start + 1..i])
                .ok()
                .and_then(|t| t.parse().ok())
            {
                Some(r) => out.push(Tok::Ref(r)),
                None => out.push(Tok::Raw(&b[start..i])),
            }
        } else if c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            while i < b.len() && b[i] != b'.' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(Tok::Raw(&b[start..i]));
        } else if c.is_ascii_alphabetic() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Raw(&b[start..i]));
        } else if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') {
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_digit() || matches!(b[i], b'.' | b'-' | b'+' | b'e' | b'E'))
            {
                i += 1;
            }
            match std::str::from_utf8(&b[start..i])
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
            {
                Some(v) => out.push(Tok::Num(v)),
                None => out.push(Tok::Raw(&b[start..i])),
            }
        } else {
            i += 1;
            out.push(Tok::Raw(&b[start..i]));
        }
    }
    out
}

fn num_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

type Memo = HashMap<(u32, u32), bool>;

/// File arguments `fa` say what export arguments `ea` (already in the file's units) say.
fn same_toks(
    tx: &Tx<'_>,
    tree: &HashMap<u32, Probe>,
    fa: &[u8],
    ea: &[u8],
    memo: &mut Memo,
) -> bool {
    let (x, y) = (toks(fa), toks(ea));
    if x.len() != y.len() {
        return false;
    }
    for (a, b) in x.iter().zip(&y) {
        let ok = match (a, b) {
            (Tok::Ref(f), Tok::Ref(e)) => same(tx, tree, *f, *e, memo),
            (Tok::Num(f), Tok::Num(e)) => num_eq(*f, *e),
            (Tok::Raw(f), Tok::Raw(e)) => f == e,
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// File entity `f` holds what export entity `e` holds, all the way down.
fn same(tx: &Tx<'_>, tree: &HashMap<u32, Probe>, f: u32, e: u32, memo: &mut Memo) -> bool {
    if let Some(v) = memo.get(&(f, e)) {
        return *v;
    }
    memo.insert((f, e), true);
    let v = same_ent(tx, tree, f, e, memo);
    memo.insert((f, e), v);
    v
}

fn same_ent(tx: &Tx<'_>, tree: &HashMap<u32, Probe>, f: u32, e: u32, memo: &mut Memo) -> bool {
    let (Some(fp), Some(ee)) = (tree.get(&f), tx.ex.ent(e)) else {
        return false;
    };
    let ty = upper(ee.ty);
    if fp.ty != ty {
        return false;
    }
    if is_context(&ty) {
        return tx.context(&ty, ee.args).ok() == Some(f);
    }
    if let Some(g) = guid_of(ee.args) {
        return guid_of(&fp.args) == Some(g);
    }
    let ea = tx.scaled(&ty, ee.args);
    same_toks(tx, tree, &fp.args, &ea, memo)
}

/// Part `f` of the file says what part `k` of the export says, apart from GlobalId and owner.
fn same_part(tx: &Tx<'_>, tree: &HashMap<u32, Probe>, f: u32, k: u32, memo: &mut Memo) -> bool {
    let (Some(fp), Some(ee)) = (tree.get(&f), tx.ex.ent(k)) else {
        return false;
    };
    let ty = upper(ee.ty);
    if fp.ty != ty {
        return false;
    }
    let ea = tx.scaled(&ty, ee.args);
    let (fa, xa) = (split_top(&fp.args), split_top(&ea));
    if fa.len() != xa.len() || fa.len() < 2 {
        return false;
    }
    let fa = join_args(&owned(&fa[2..]));
    let xa = join_args(&owned(&xa[2..]));
    same_toks(tx, tree, &fa, &xa, memo)
}

/// Writes edited whole `e` and its parts. `Ok(false)`: the file's schema cannot hold it.
#[allow(clippy::too_many_arguments)]
pub(super) fn edit_whole(
    tx: &mut Tx<'_>,
    src: &Path,
    e: &GeomEdit,
    file_id: u32,
    ex_id: u32,
    file_ty: &str,
    file_args: &[u8],
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<bool, PatchError> {
    let ix = tx.ix;
    let ex = tx.ex;
    let g = e.guid.as_str();

    // the file: at most one aggregation of parts, the whole not itself a part, nothing nested
    let mut agg: Option<(u32, Vec<u8>)> = None;
    for &ri in ix.by_ref.get(&file_id).into_iter().flatten() {
        let r = &ix.rels[ri];
        if o.remove.contains(&r.id) || (r.ty != AGGREGATES && r.ty != NESTS) {
            continue;
        }
        let now = o.current_args(r.id, &r.args);
        if !first_end_is(&now, file_id) {
            return Err(doubt(DOUBT_SHARED, format!("{} {g} is a part of another", e.kind)));
        }
        if r.ty == NESTS {
            return Err(doubt(DOUBT_NESTED, format!("{} {g} has nested parts", e.kind)));
        }
        if split_top(&now).len() < 6 {
            return Err(doubt(DOUBT_STRUCTURE, format!("relation #{}", r.id)));
        }
        if agg.replace((r.id, now)).is_some() {
            return Err(doubt(
                DOUBT_EDIT,
                format!("{} {g} has more than one aggregation of parts", e.kind),
            ));
        }
    }
    let agg_id = agg.as_ref().map(|a| a.0);
    let file_parts: Vec<u32> = agg
        .as_ref()
        .map(|(_, a)| items_at(&split_top(a), 5))
        .unwrap_or_default();
    let uniq: HashSet<u32> = file_parts.iter().copied().collect();
    if uniq.len() != file_parts.len() {
        return Err(doubt(DOUBT_STRUCTURE, format!("parts of {g} repeated")));
    }
    // a part in another aggregation or nesting belongs to more than this whole
    for &fp in &file_parts {
        let shared = ix
            .by_ref
            .get(&fp)
            .into_iter()
            .flatten()
            .map(|ri| &ix.rels[*ri])
            .any(|r| {
                (r.ty == AGGREGATES || r.ty == NESTS)
                    && Some(r.id) != agg_id
                    && !o.remove.contains(&r.id)
            });
        if shared {
            return Err(doubt(DOUBT_SHARED, format!("part #{fp} of {g} is shared")));
        }
    }

    // the export: its parts must fit the file before anything is written
    let kids = parts_of(ex, ex_id, g)?;
    let mut seen = HashSet::new();
    if kids.iter().any(|k| tx.check(*k, true, &mut seen).is_err()) {
        o.deny(&e.kind, e.id, "", REASON_SCHEMA);
        return Ok(false);
    }

    let tree = if uniq.is_empty() {
        HashMap::new()
    } else {
        read_trees(src, &uniq, ctl)?
    };
    let mut by_guid: HashMap<String, u32> = HashMap::new();
    for &fp in &file_parts {
        let Some(pg) = tree.get(&fp).and_then(|p| guid_of(&p.args)) else {
            return Err(doubt(DOUBT_STRUCTURE, format!("part #{fp} of {g}")));
        };
        if by_guid.insert(pg.to_string(), fp).is_some() {
            return Err(doubt(DOUBT_STRUCTURE, format!("part {pg} of {g} repeated")));
        }
    }
    // (export id, GlobalId, file id when the file has it)
    let mut plan: Vec<(u32, String, Option<u32>)> = Vec::with_capacity(kids.len());
    for &k in &kids {
        let Some(pg) = ex.guid_at(k) else {
            return Err(doubt(DOUBT_STRUCTURE, format!("export part #{k}")));
        };
        if ex.dup.contains(pg) {
            return Err(doubt(
                DOUBT_EDIT,
                format!("part {pg} of {g} is on more than one exported line"),
            ));
        }
        let have = by_guid.get(pg).copied();
        if have.is_none() && (ix.rooted.contains_key(pg) || o.new_guids.contains(pg)) {
            return Err(doubt(
                DOUBT_SHARED,
                format!("part {pg} of {g} is already in the file elsewhere"),
            ));
        }
        if let Some(f) = have {
            let ty = upper(ex.ent(k).map_or(&b""[..], |x| x.ty));
            if tree.get(&f).map(|p| p.ty.as_str()) != Some(ty.as_str()) {
                return Err(doubt(DOUBT_EDIT, format!("part {pg} of {g} changes class")));
            }
        }
        plan.push((k, pg.to_string(), have));
    }
    let kept: HashSet<u32> = plan.iter().filter_map(|p| p.2).collect();
    let gone: Vec<u32> = file_parts
        .iter()
        .copied()
        .filter(|p| !kept.contains(p))
        .collect();

    if !edit_one(tx, e, file_id, ex_id, file_ty, file_args, o)? {
        return Ok(false);
    }

    // kept parts: compared first, so the unchanged ones stay byte for byte
    let mut memo = Memo::new();
    let mut fresh: Vec<(u32, String, u32)> = vec![];
    for (k, pg, have) in &plan {
        let Some(f) = *have else {
            let n = o.alloc();
            tx.map.insert(*k, n);
            o.new_guids.insert(pg.clone());
            fresh.push((*k, pg.clone(), n));
            continue;
        };
        tx.map.insert(*k, f);
        if same_part(tx, &tree, f, *k, &mut memo) {
            continue;
        }
        let ent = ex
            .ent(*k)
            .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("export has no #{k}")))?;
        let ty = upper(ent.ty);
        let args = tx.root_args(&ty, ent.args, o)?;
        let fa = split_top(&tree[&f].args);
        let mut a = owned(&split_top(&args));
        if a.len() < 2 || fa.len() < 2 {
            return Err(doubt(DOUBT_STRUCTURE, format!("part line #{f}")));
        }
        a[0] = fa[0].to_vec();
        a[1] = fa[1].to_vec();
        o.swap(f, &ty, &join_args(&a));
        o.stale.insert(f);
        o.edited += 1;
        o.touched.push(pg.clone());
    }
    for (k, pg, n) in &fresh {
        let ent = ex
            .ent(*k)
            .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("export has no #{k}")))?;
        let ty = upper(ent.ty);
        let args = tx.root_args(&ty, ent.args, o)?;
        o.put(*n, &ty, &args);
        o.added += 1;
        o.touched.push(pg.clone());
    }
    for (k, pg, n) in &fresh {
        add_rels(tx, *k, *n, pg, (e.kind.as_str(), e.id), None, Some(ex_id), o)?;
    }
    // a link of a new part that failed is kept on the whole: the next save tries it again
    if o.pending_links.iter().any(|(x, _)| x == g) && !o.retried.iter().any(|x| x == g) {
        o.retried.push(g.to_string());
    }

    drop_parts(ix, &tree, agg_id, &gone, g, o)?;

    // the one aggregation: kept parts in the file's order, then the new ones
    let mut items: Vec<u32> = file_parts
        .iter()
        .copied()
        .filter(|p| kept.contains(p))
        .collect();
    items.extend(fresh.iter().map(|f| f.2));
    match &agg {
        Some((id, _)) if items.is_empty() => {
            o.remove.insert(*id);
            o.replace.remove(id);
        }
        Some((id, now)) => {
            if items != file_parts {
                let mut a = owned(&split_top(now));
                a[5] = ref_list(&items);
                o.swap(*id, AGGREGATES, &join_args(&a));
            }
        }
        None if !items.is_empty() => {
            tx.new_rel(
                AGGREGATES,
                &format!("{g}|parts"),
                [format!("#{file_id}").into_bytes(), ref_list(&items)],
                o,
            )?;
        }
        None => {}
    }
    Ok(true)
}

/// Removes parts the model no longer has: each leaves the list relations naming it, and the
/// lines only removed parts used go with them. Any other reference to a part is a doubt.
fn drop_parts(
    ix: &Index,
    tree: &HashMap<u32, Probe>,
    agg_id: Option<u32>,
    gone: &[u32],
    whole: &str,
    o: &mut Out,
) -> Result<(), PatchError> {
    if gone.is_empty() {
        return Ok(());
    }
    let gone_set: HashSet<u32> = gone.iter().copied().collect();
    for &r in gone {
        let mut rels: HashSet<u32> = HashSet::new();
        for &ri in ix.by_ref.get(&r).into_iter().flatten() {
            let rel = &ix.rels[ri];
            rels.insert(rel.id);
            if Some(rel.id) == agg_id || o.remove.contains(&rel.id) {
                continue;
            }
            let now = o.current_args(rel.id, &rel.args);
            let a = split_top(&now);
            let listed = a.get(4).and_then(|v| list_items(v));
            let ids: Vec<u32> = listed
                .as_ref()
                .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
                .unwrap_or_default();
            let mixed = listed.as_ref().is_some_and(|l| l.len() != ids.len());
            let elsewhere = a.iter().enumerate().any(|(i, v)| {
                let mut hit = false;
                if i != 4 {
                    for_each_ref(v, &mut |x| hit |= x == r);
                }
                hit
            });
            let listing = !matches!(rel.ty.as_str(), AGGREGATES | NESTS | VOIDS | FILLS);
            if !listing || elsewhere || mixed || !ids.contains(&r) {
                return Err(doubt(
                    DOUBT_SHARED,
                    format!("part #{r} of {whole} is named by #{}", rel.id),
                ));
            }
            let left: Vec<u32> = ids.into_iter().filter(|x| *x != r).collect();
            if left.is_empty() {
                o.remove.insert(rel.id);
                o.replace.remove(&rel.id);
            } else {
                let mut x = owned(&a);
                x[4] = ref_list(&left);
                o.swap(rel.id, &rel.ty, &join_args(&x));
            }
        }
        let Some(p) = tree.get(&r) else {
            return Err(doubt(DOUBT_STRUCTURE, format!("part #{r} of {whole} not read")));
        };
        if let Some(x) = p
            .referrers
            .iter()
            .find(|x| Some(**x) != agg_id && !rels.contains(*x))
        {
            return Err(doubt(
                DOUBT_SHARED,
                format!("part #{r} of {whole} is named by #{x}"),
            ));
        }
        o.remove.insert(r);
        o.replace.remove(&r);
        o.gone += 1;
    }

    // lines under the removed parts that nothing else names
    let mut own: HashSet<u32> = HashSet::new();
    let mut seen: HashSet<u32> = gone_set.clone();
    let mut stack: Vec<u32> = gone.to_vec();
    while let Some(x) = stack.pop() {
        let Some(p) = tree.get(&x) else { continue };
        let root = gone_set.contains(&x);
        if !root && (is_context(&p.ty) || guid_of(&p.args).is_some()) {
            continue;
        }
        if !root {
            own.insert(x);
        }
        for_each_ref(&p.args, &mut |r| {
            if seen.insert(r) {
                stack.push(r);
            }
        });
    }
    loop {
        let used: Vec<u32> = own
            .iter()
            .copied()
            .filter(|x| {
                tree.get(x).map_or(true, |p| {
                    p.referrers
                        .iter()
                        .any(|r| !own.contains(r) && !gone_set.contains(r))
                })
            })
            .collect();
        if used.is_empty() {
            break;
        }
        for x in used {
            own.remove(&x);
        }
    }
    for x in own {
        o.remove.insert(x);
        o.replace.remove(&x);
    }
    Ok(())
}

/// Keeps the data set of an edited element (`set`, the exact row the importer reads) in step
/// with the export: each value that differs is replaced in its own line. The set and its values
/// must be the element's own and name the same properties as the export.
pub(super) fn sync_set(
    tx: &Tx<'_>,
    src: &Path,
    file_id: u32,
    ex_id: u32,
    set: &str,
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let (ix, ex) = (tx.ix, tx.ex);
    let mut want: Vec<(String, Vec<u8>)> = vec![];
    let mut found = false;
    for &i in ex.by_ref.get(&ex_id).into_iter().flatten() {
        let (ty, args) = &ex.rels[i];
        let a = split_top(args);
        if ty != DEFINES || !related(&a).contains(&ex_id) {
            continue;
        }
        let Some(s) = a.get(5).and_then(|v| single_ref(v)).and_then(|s| ex.ent(s)) else {
            continue;
        };
        let sa = split_top(s.args);
        if upper(s.ty) != "IFCPROPERTYSET" || !set_named(&sa, set) {
            continue;
        }
        found = true;
        for it in sa.get(4).and_then(|v| list_items(v)).unwrap_or_default() {
            let Some(p) = single_ref(it).and_then(|r| ex.ent(r)) else {
                continue;
            };
            if upper(p.ty) != "IFCPROPERTYSINGLEVALUE" {
                continue;
            }
            let scaled = tx.scaled("IFCPROPERTYSINGLEVALUE", p.args);
            let pa = split_top(&scaled);
            let (Some(name), Some(v)) = (pa.first().and_then(|v| string_arg(v)), pa.get(2)) else {
                continue;
            };
            want.push((name, v.to_vec()));
        }
    }
    if !found {
        return Ok(());
    }

    let mut rels: Vec<(u32, u32)> = vec![];
    for r in ix.rels_of(file_id, DEFINES) {
        if o.remove.contains(&r.id) {
            continue;
        }
        let now = o.current_args(r.id, &r.args);
        let a = split_top(&now);
        if a.len() < 6 {
            continue;
        }
        if let Some(s) = single_ref(a[5]) {
            if related(&a).contains(&file_id) {
                rels.push((r.id, s));
            }
        }
    }
    let ids: HashSet<u32> = rels.iter().map(|r| r.1).collect();
    let sets = if ids.is_empty() {
        HashMap::new()
    } else {
        probe(src, &ids, ctl)?
    };
    let mut mine: Option<u32> = None;
    for (rid, s) in &rels {
        let Some(p) = sets.get(s) else { continue };
        if p.ty != "IFCPROPERTYSET" || !set_named(&split_top(&p.args), set) {
            continue;
        }
        let rel = ix.rels_of(file_id, DEFINES).find(|r| r.id == *rid);
        let only = rel.is_some_and(|r| {
            related(&split_top(&o.current_args(r.id, &r.args))) == [file_id]
        }) && p.referrers.as_slice() == [*rid];
        if !only || mine.replace(*s).is_some() {
            return Err(doubt(
                DOUBT_EDIT,
                format!("#{file_id}: {set} set is shared or repeated"),
            ));
        }
    }
    let Some(s) = mine else {
        return Err(doubt(
            DOUBT_EDIT,
            format!("#{file_id}: no {set} set to keep in step"),
        ));
    };
    let items: HashSet<u32> = items_at(&split_top(&sets[&s].args), 4)
        .into_iter()
        .collect();
    let lines = if items.is_empty() {
        HashMap::new()
    } else {
        probe(src, &items, ctl)?
    };
    let mut by_name: HashMap<String, (u32, &Probe)> = HashMap::new();
    for (id, p) in &lines {
        if p.ty != "IFCPROPERTYSINGLEVALUE" {
            continue;
        }
        let pa = split_top(&p.args);
        let Some(name) = pa.first().and_then(|v| string_arg(v)) else {
            continue;
        };
        if p.referrers.as_slice() != [s] || pa.len() < 3 || by_name.contains_key(&name) {
            return Err(doubt(
                DOUBT_EDIT,
                format!("#{file_id}: {set} value {name} is shared or repeated"),
            ));
        }
        by_name.insert(name, (*id, p));
    }
    let names: HashSet<&str> = want.iter().map(|w| w.0.as_str()).collect();
    if names.len() != want.len()
        || names.len() != by_name.len()
        || !by_name.keys().all(|k| names.contains(k.as_str()))
    {
        return Err(doubt(
            DOUBT_EDIT,
            format!("#{file_id}: {set} names other properties than the model"),
        ));
    }
    for (name, v) in &want {
        let (id, p) = by_name[name];
        let pa = split_top(&p.args);
        if pa[2] == v.as_slice() {
            continue;
        }
        let mut a = owned(&pa);
        a[2] = v.clone();
        o.swap(id, &p.ty, &join_args(&a));
    }
    Ok(())
}
