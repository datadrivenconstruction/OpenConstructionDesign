//! What a file holds and how its entities point at each other: a one-pass index, targeted probes,
//! and the cut: which statements go or change when elements are deleted.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use super::lines::{
    for_each_ref, guid_of, join_args, list_items, owned, parse_ent, ref_list, single_ref,
    split_top, string_arg, upper, Bits, Stmts,
};
use super::{doubt, PatchError, DOUBT_EDIT, DOUBT_STRUCTURE};
use crate::ifc::schema_spec::IfcSchema;
use crate::job::{Progress, CANCELLED};

/// A relationship statement of the file.
pub(super) struct Rel {
    pub id: u32,
    pub ty: String,
    pub args: Vec<u8>,
}

#[derive(Default)]
pub(super) struct Index {
    pub schema_name: String,
    pub schema: Option<IfcSchema>,
    pub max_id: u32,
    pub crlf: bool,
    /// GlobalId to entity; rooted entities only, relations included.
    pub rooted: HashMap<String, u32>,
    pub ty_of: HashMap<u32, u16>,
    pub types: Vec<String>,
    pub rels: Vec<Rel>,
    /// Entity to the relations naming it after the four root attributes.
    pub by_ref: HashMap<u32, Vec<usize>>,
    pub context: Option<u32>,
    /// Upper-case ContextIdentifier to subcontext.
    pub subcontexts: HashMap<String, u32>,
    pub owner: Option<u32>,
    pub products: u64,
    pub dup_ids: u64,
    pub dup_guids: u64,
    pub dangling: u64,
    pub ended: bool,
}

fn cancelled(c: Option<&Arc<Progress>>) -> bool {
    c.is_some_and(|c| c.is_cancelled())
}

fn io_err(e: std::io::Error) -> PatchError {
    PatchError::Failed(e.to_string())
}

impl Index {
    pub fn scan(path: &Path, ctl: Option<&Arc<Progress>>) -> Result<Self, PatchError> {
        let f = File::open(path).map_err(io_err)?;
        let mut st = Stmts::new(f);
        let mut ix = Index::default();
        let mut ty_ix: HashMap<Vec<u8>, u16> = HashMap::new();
        let (mut ids, mut refs) = (Bits::default(), Bits::default());
        let (mut in_data, mut eol_known, mut n) = (false, false, 0u64);
        while let Some(p) = st.next().map_err(io_err)? {
            n += 1;
            if n % 4096 == 0 && cancelled(ctl) {
                return Err(PatchError::Failed(CANCELLED.into()));
            }
            if !eol_known && p.lead.contains(&b'\n') {
                ix.crlf = p.lead.windows(2).any(|w| w == b"\r\n");
                eol_known = true;
            }
            let body = p.body;
            if !in_data {
                if body.starts_with(b"FILE_SCHEMA") {
                    let name = body
                        .iter()
                        .position(|c| *c == b'\'')
                        .map(|a| &body[a + 1..])
                        .and_then(|r| r.iter().position(|c| *c == b'\'').map(|e| upper(&r[..e])))
                        .unwrap_or_default();
                    ix.schema = IfcSchema::family(&name);
                    ix.schema_name = name;
                } else if body.starts_with(b"DATA") {
                    in_data = true;
                }
                continue;
            }
            if body.starts_with(b"ENDSEC") {
                ix.ended = true;
                in_data = false;
                continue;
            }
            if body.is_empty() {
                continue;
            }
            let Some(e) = parse_ent(body) else {
                return Err(doubt(
                    DOUBT_STRUCTURE,
                    "a statement in DATA is not an entity",
                ));
            };
            if !ids.set(e.id) {
                ix.dup_ids += 1;
            }
            ix.max_id = ix.max_id.max(e.id);
            for_each_ref(e.args, &mut |r| {
                refs.set(r);
            });
            if let Some(g) = guid_of(e.args) {
                if ix.rooted.insert(g.to_string(), e.id).is_some() {
                    ix.dup_guids += 1;
                }
                let t = match ty_ix.get(e.ty) {
                    Some(t) => *t,
                    None => {
                        let t = ix.types.len() as u16;
                        ix.types.push(upper(e.ty));
                        ty_ix.insert(e.ty.to_vec(), t);
                        t
                    }
                };
                ix.ty_of.insert(e.id, t);
                let name = ix.types[t as usize].as_str();
                if name.starts_with("IFCREL") {
                    let rel = Rel {
                        id: e.id,
                        ty: name.to_string(),
                        args: e.args.to_vec(),
                    };
                    ix.rels.push(rel);
                } else if !matches!(name, "IFCPROPERTYSET" | "IFCELEMENTQUANTITY") {
                    ix.products += 1;
                }
            } else if e
                .ty
                .eq_ignore_ascii_case(b"IFCGEOMETRICREPRESENTATIONSUBCONTEXT")
            {
                if let Some(name) = split_top(e.args).first().and_then(|a| string_arg(a)) {
                    ix.subcontexts
                        .entry(name.to_ascii_uppercase())
                        .or_insert(e.id);
                }
            } else if e
                .ty
                .eq_ignore_ascii_case(b"IFCGEOMETRICREPRESENTATIONCONTEXT")
            {
                if ix.context.is_none() {
                    ix.context = Some(e.id);
                }
            } else if e.ty.eq_ignore_ascii_case(b"IFCOWNERHISTORY") && ix.owner.is_none() {
                ix.owner = Some(e.id);
            }
        }
        ix.dangling = refs.count_without(&ids);
        ix.link();
        Ok(ix)
    }

    fn link(&mut self) {
        let mut by_ref: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, r) in self.rels.iter().enumerate() {
            for x in split_top(&r.args).iter().skip(4) {
                for_each_ref(x, &mut |t| {
                    let v = by_ref.entry(t).or_default();
                    if v.last() != Some(&i) {
                        v.push(i);
                    }
                });
            }
        }
        self.by_ref = by_ref;
    }

    pub fn type_name(&self, id: u32) -> Option<&str> {
        self.ty_of
            .get(&id)
            .map(|t| self.types[*t as usize].as_str())
    }

    /// Relations of `ty` naming `id`.
    pub fn rels_of<'a>(&'a self, id: u32, ty: &'a str) -> impl Iterator<Item = &'a Rel> + 'a {
        self.by_ref
            .get(&id)
            .into_iter()
            .flatten()
            .map(move |i| &self.rels[*i])
            .filter(move |r| r.ty == ty)
    }

    /// Second single end (argument 5) of each `ty` relation whose first end (argument 4) is `id`.
    pub fn second_of(&self, id: u32, ty: &str) -> Vec<u32> {
        self.rels_of(id, ty)
            .filter_map(|r| {
                let a = split_top(&r.args);
                if single_ref(a.get(4)?)? == id {
                    single_ref(a.get(5)?)
                } else {
                    None
                }
            })
            .collect()
    }

    /// GlobalId of each rooted entity, for reverse look-ups.
    pub fn guid_by_id(&self) -> HashMap<u32, &str> {
        self.rooted.iter().map(|(g, i)| (*i, g.as_str())).collect()
    }
}

/// One entity's text and who points at it.
#[derive(Clone, Debug, Default)]
pub(super) struct Probe {
    pub ty: String,
    pub args: Vec<u8>,
    pub referrers: Vec<u32>,
}

/// One pass over the file for the given entities.
pub(super) fn probe(
    path: &Path,
    wanted: &HashSet<u32>,
    ctl: Option<&Arc<Progress>>,
) -> Result<HashMap<u32, Probe>, PatchError> {
    let mut out: HashMap<u32, Probe> = wanted.iter().map(|i| (*i, Probe::default())).collect();
    let f = File::open(path).map_err(io_err)?;
    let mut st = Stmts::new(f);
    let mut n = 0u64;
    while let Some(p) = st.next().map_err(io_err)? {
        n += 1;
        if n % 4096 == 0 && cancelled(ctl) {
            return Err(PatchError::Failed(CANCELLED.into()));
        }
        let Some(e) = parse_ent(p.body) else { continue };
        for_each_ref(e.args, &mut |r| {
            if let Some(x) = out.get_mut(&r) {
                if x.referrers.last() != Some(&e.id) {
                    x.referrers.push(e.id);
                }
            }
        });
        if let Some(x) = out.get_mut(&e.id) {
            x.ty = upper(e.ty);
            x.args = e.args.to_vec();
        }
    }
    Ok(out)
}

/// Result of deleting elements: statements gone, relation arguments rewritten.
#[derive(Default, Debug)]
pub(super) struct Cut {
    pub removed: HashSet<u32>,
    pub rewrites: HashMap<u32, Vec<u8>>,
}

/// Entities that exist only for `gone` once a relation of it falls.
fn dependents(ty: &str, a: &[&[u8]], gone: u32) -> Result<Vec<u32>, PatchError> {
    let one = |i: usize| a.get(i).and_then(|x| single_ref(x));
    Ok(match ty {
        "IFCRELVOIDSELEMENT" if one(4) == Some(gone) => one(5).into_iter().collect(),
        "IFCRELFILLSELEMENT" if one(4) == Some(gone) => one(5).into_iter().collect(),
        "IFCRELFILLSELEMENT" if one(5) == Some(gone) => one(4).into_iter().collect(),
        "IFCRELAGGREGATES" | "IFCRELNESTS" if one(4) == Some(gone) => a
            .get(5)
            .and_then(|x| list_items(x))
            .map(|l| l.iter().filter_map(|x| single_ref(x)).collect())
            .unwrap_or_default(),
        "IFCRELCONTAINEDINSPATIALSTRUCTURE" | "IFCRELREFERENCEDINSPATIALSTRUCTURE"
            if one(5) == Some(gone) =>
        {
            return Err(doubt(
                DOUBT_EDIT,
                "a spatial element holding elements was deleted",
            ));
        }
        _ => vec![],
    })
}

/// Deletes `seeds` and whatever exists only for them. Relations lose the entity from their lists;
/// a relation left with an empty list or a missing single end goes too. A dependent still linked
/// to a live model row (`alive`) means the file and the model disagree: that is a doubt.
pub(super) fn cut(
    ix: &Index,
    seeds: &[u32],
    alive: &mut dyn FnMut(u32) -> Result<bool, PatchError>,
) -> Result<Cut, PatchError> {
    let mut c = Cut::default();
    let mut work: Vec<u32> = seeds.to_vec();
    while let Some(x) = work.pop() {
        if !c.removed.insert(x) {
            continue;
        }
        let Some(list) = ix.by_ref.get(&x) else {
            continue;
        };
        for &ri in list {
            let rel = &ix.rels[ri];
            if c.removed.contains(&rel.id) {
                continue;
            }
            let orig = split_top(&rel.args);
            for d in dependents(&rel.ty, &orig, x)? {
                if !c.removed.contains(&d) {
                    if alive(d)? {
                        return Err(doubt(
                            DOUBT_EDIT,
                            format!("#{d} depends on deleted #{x} but its model row is alive"),
                        ));
                    }
                    work.push(d);
                }
            }
            let now = c
                .rewrites
                .get(&rel.id)
                .cloned()
                .unwrap_or_else(|| rel.args.clone());
            let a = split_top(&now);
            let mut args = owned(&a);
            let mut gone = false;
            for (i, v) in a.iter().enumerate().skip(4) {
                if single_ref(v) == Some(x) {
                    gone = true;
                } else if let Some(items) = list_items(v) {
                    let ids: Vec<u32> = items.iter().filter_map(|t| single_ref(t)).collect();
                    if ids.contains(&x) {
                        if ids.len() != items.len() {
                            return Err(doubt(
                                DOUBT_STRUCTURE,
                                format!("mixed list in relation #{}", rel.id),
                            ));
                        }
                        let left: Vec<u32> = ids.into_iter().filter(|t| *t != x).collect();
                        if left.is_empty() {
                            gone = true;
                        } else {
                            args[i] = ref_list(&left);
                        }
                    }
                }
            }
            if gone {
                c.rewrites.remove(&rel.id);
                work.push(rel.id);
            } else {
                c.rewrites.insert(rel.id, join_args(&args));
            }
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(stmts: &str) -> (std::path::PathBuf, Index) {
        let nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p =
            std::env::temp_dir().join(format!("ddc-o6-refs-{}-{nanos}.ifc", std::process::id()));
        let s = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{stmts}ENDSEC;\nEND-ISO-10303-21;\n"
        );
        std::fs::write(&p, s).unwrap();
        let ix = Index::scan(&p, None).unwrap();
        (p, ix)
    }

    const G: [&str; 8] = [
        "0000000000000000000001",
        "0000000000000000000002",
        "0000000000000000000003",
        "0000000000000000000004",
        "0000000000000000000005",
        "0000000000000000000006",
        "0000000000000000000007",
        "0000000000000000000008",
    ];

    fn fixture() -> String {
        format!(
            "#1=IFCWALL('{}',$,$,$,$,$,$,$);\n#2=IFCWALL('{}',$,$,$,$,$,$,$);\n\
             #3=IFCOPENINGELEMENT('{}',$,$,$,$,$,$,$);\n#4=IFCDOOR('{}',$,$,$,$,$,$,$);\n\
             #5=IFCRELVOIDSELEMENT('{}',$,$,$,#1,#3);\n#6=IFCRELFILLSELEMENT('{}',$,$,$,#3,#4);\n\
             #7=IFCRELCONTAINEDINSPATIALSTRUCTURE('{}',$,$,$,(#1,#2,#4),#9);\n\
             #9=IFCBUILDINGSTOREY('{}',$,$,$,$,$,$,$,$);\n",
            G[0], G[1], G[2], G[3], G[4], G[5], G[6], G[7]
        )
    }

    #[test]
    fn the_index_sees_the_schema_ids_and_relations() {
        let (p, ix) = index(&fixture());
        assert_eq!(ix.schema, Some(IfcSchema::Ifc4));
        assert_eq!(ix.max_id, 9);
        assert_eq!(ix.rels.len(), 3);
        assert_eq!(ix.dangling, 0);
        assert!(ix.ended);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn deleting_a_host_takes_its_opening_and_filler_and_shrinks_the_list() {
        let (p, ix) = index(&fixture());
        let c = cut(&ix, &[1], &mut |_| Ok(false)).unwrap();
        for id in [1, 3, 4, 5, 6] {
            assert!(c.removed.contains(&id), "#{id}");
        }
        assert!(!c.removed.contains(&7));
        let list = String::from_utf8_lossy(c.rewrites.get(&7).unwrap()).to_string();
        assert!(list.contains("(#2)"), "{list}");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_live_dependent_is_a_doubt() {
        let (p, ix) = index(&fixture());
        let e = cut(&ix, &[1], &mut |d| Ok(d == 3)).unwrap_err();
        assert!(matches!(e, PatchError::Doubt { .. }));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_relation_left_empty_goes_too() {
        let (p, ix) = index(&format!(
            "#1=IFCWALL('{}',$,$,$,$,$,$,$);\n#2=IFCRELDEFINESBYPROPERTIES('{}',$,$,$,(#1),#3);\n#3=IFCPROPERTYSET('{}',$,'S',$,());\n",
            G[0], G[1], G[2]
        ));
        let c = cut(&ix, &[1], &mut |_| Ok(false)).unwrap();
        assert!(c.removed.contains(&2));
        assert!(!c.removed.contains(&3));
        let _ = std::fs::remove_file(p);
    }
}
