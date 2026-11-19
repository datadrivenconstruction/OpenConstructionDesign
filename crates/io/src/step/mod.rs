// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! STEP AP203/AP214 reader (docs/design/IO_FORMATS.md): own Part 21 parser; B-rep faces
//! tessellated in parameter space, faceted B-reps and shell models; assemblies through
//! representation relationships with item transforms and mapped items; length units;
//! surface colours. Returns a [`MeshSet`]: one part per placed solid, metres, Z up.
//! The exact path (docs/IO4.md): [`read_exact`] into `crate::exact` bodies, [`write`] back out.

mod curve;
mod exact_read;
#[cfg(test)]
mod exact_tests;
mod math;
mod p21;
mod surf;
mod topo;
#[cfg(test)]
mod tests;
pub mod write;

pub use exact_read::read_exact;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::job::{Progress, CANCELLED};
use crate::meshx::{srgb8_to_linear, MeshSet, Part, GENERIC};
use math::Xf;
use p21::{Store, P};
use topo::{Mesh, Tess, SOLIDS};

/// Reads a `.stp` / `.step` file; returns the parts and warnings.
pub fn read_file(path: &Path, progress: Option<&Progress>) -> Result<(MeshSet, Vec<String>), String> {
    let name = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    if let Some(p) = progress {
        p.phase("job.stage.read", 0.0, 0.1);
        p.set_bytes(true);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    if let Some(p) = progress {
        let n = bytes.len() as u64;
        p.set_total(n);
        p.set(n);
    }
    named(&bytes, &name, progress)
}

/// Reads STEP text already in memory; `name` prefixes the errors.
pub fn read_bytes(bytes: &[u8], name: &str, progress: Option<&Progress>) -> Result<(MeshSet, Vec<String>), String> {
    if let Some(p) = progress {
        p.phase("job.stage.read", 0.0, 0.1);
        p.set_bytes(true);
        p.set_total(bytes.len() as u64);
        p.set(bytes.len() as u64);
    }
    named(bytes, name, progress)
}

fn named(bytes: &[u8], name: &str, progress: Option<&Progress>) -> Result<(MeshSet, Vec<String>), String> {
    if progress.is_some_and(|p| p.is_cancelled()) {
        return Err(CANCELLED.into());
    }
    read(bytes, progress).map_err(|e| if e == CANCELLED { e } else { format!("{name}: {e}") })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Units {
    /// Metres per file length unit.
    len: f64,
    /// Radians per file plane angle unit.
    ang: f64,
}

/// One placement of a solid.
struct Inst {
    solid: u64,
    units: Units,
    /// File coordinates scaled to metres, then this map.
    xf: Xf,
    path: Vec<u64>,
    name: String,
}

type Rgba = ([u8; 3], f32);

fn read(bytes: &[u8], progress: Option<&Progress>) -> Result<(MeshSet, Vec<String>), String> {
    if let Some(p) = progress {
        p.phase("job.stage.parse", 0.1, 0.5);
    }
    let st = p21::parse(bytes, progress)?;
    if progress.is_some_and(|p| p.is_cancelled()) {
        return Err(CANCELLED.into());
    }
    let mut warns = vec![];
    let (insts, def_len) = structure(&st, &mut warns);
    let tol_m = tolerance(&st, def_len);
    let cols = colours(&st);

    let mut uniq: Vec<(u64, Units)> = vec![];
    let mut seen = HashSet::new();
    for i in &insts {
        if seen.insert(mesh_key(i.solid, i.units)) {
            uniq.push((i.solid, i.units));
        }
    }
    if let Some(p) = progress {
        p.phase("job.stage.plan", 0.5, 0.8);
        let total: u64 = uniq.iter().map(|u| topo::face_count(&st, u.0) as u64).sum();
        p.set_total(total.max(1));
        p.set(0);
    }
    let mut tess = Tess::new(&st, progress);
    let mut meshes: HashMap<(u64, u64, u64), Mesh> = HashMap::new();
    for &(id, u) in &uniq {
        tess.set_units(tol_m / u.len, u.ang);
        let m = tess.solid(id);
        if tess.cancelled || progress.is_some_and(|p| p.is_cancelled()) {
            return Err(CANCELLED.into());
        }
        meshes.insert(mesh_key(id, u), m);
    }

    let mut count: HashMap<u64, usize> = HashMap::new();
    for i in &insts {
        *count.entry(i.solid).or_default() += 1;
    }
    let mut ms = MeshSet::default();
    let mut empty = 0usize;
    for (k, inst) in insts.iter().enumerate() {
        let Some(m) = meshes.get(&mesh_key(inst.solid, inst.units)) else { continue };
        if m.tri.is_empty() {
            empty += 1;
            continue;
        }
        let material = solid_colour(&st, &cols, inst.solid, m).map(|(c, a)| {
            let mut name = format!("step #{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
            if a < 1.0 {
                name += &format!(" a{a:.2}");
            }
            ms.material(&name, [srgb8_to_linear(c[0]), srgb8_to_linear(c[1]), srgb8_to_linear(c[2]), a])
        });
        let s = inst.units.len;
        let flip = inst.xf.det() < 0.0;
        let mut key = format!("step:#{}", inst.solid);
        if count[&inst.solid] > 1 {
            if inst.path.is_empty() {
                key += &format!("@{k}");
            } else {
                let path: Vec<String> = inst.path.iter().map(|p| format!("#{p}")).collect();
                key += &format!("@{}", path.join("/"));
            }
        }
        let part = Part {
            key,
            name: inst.name.clone(),
            category: GENERIC.into(),
            level: None,
            material,
            positions: m.pos.iter().map(|&v| inst.xf.apply(math::mul(v, s))).collect(),
            triangles: m
                .tri
                .iter()
                .map(|t| if flip { [t[0], t[2], t[1]] } else { *t })
                .collect(),
        };
        if !ms.push(part) {
            empty += 1;
        }
    }
    if let Some(p) = progress {
        let (_, total) = p.counts();
        p.set(total);
    }
    let skipped: usize = tess.skipped.values().sum();
    if skipped > 0 {
        let why: Vec<String> = tess.skipped.iter().map(|(r, n)| format!("{r} ({n})")).collect();
        warns.push(format!("{skipped} face(s) skipped: {}", why.join("; ")));
    }
    if empty > 0 {
        warns.push(format!("{empty} solid placement(s) gave no triangles"));
    }
    if ms.parts.is_empty() {
        let mut e = "no solid or surface model with faces found".to_string();
        if let Some(w) = warns.last() {
            e += &format!(" ({w})");
        }
        return Err(e);
    }
    Ok((ms, warns))
}

fn mesh_key(solid: u64, u: Units) -> (u64, u64, u64) {
    (solid, u.len.to_bits(), u.ang.to_bits())
}

/// Chord tolerance in metres: max(0.5 mm, 0.2 % of the size of the vertex cloud).
fn tolerance(st: &Store, len: f64) -> f64 {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    let mut add = |p: [f64; 3]| {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    };
    for &v in st.all("VERTEX_POINT") {
        if let Some(Ok(p)) = st
            .rec(v, "VERTEX_POINT")
            .and_then(|a| a.get(1))
            .and_then(P::id)
            .map(|i| curve::point(st, i))
        {
            add(p);
        }
    }
    for &l in st.all("POLY_LOOP") {
        for i in st.rec(l, "POLY_LOOP").and_then(|a| a.get(1)).map(P::ids).unwrap_or_default() {
            if let Ok(p) = curve::point(st, i) {
                add(p);
            }
        }
    }
    let diag = if lo[0] <= hi[0] { math::dist(lo, hi) * len } else { 0.0 };
    (0.002 * diag).max(0.0005)
}

fn prefix(e: &str) -> Option<f64> {
    Some(match e {
        "EXA" => 1e18,
        "PETA" => 1e15,
        "TERA" => 1e12,
        "GIGA" => 1e9,
        "MEGA" => 1e6,
        "KILO" => 1e3,
        "HECTO" => 1e2,
        "DECA" => 1e1,
        "DECI" => 1e-1,
        "CENTI" => 1e-2,
        "MILLI" => 1e-3,
        "MICRO" => 1e-6,
        "NANO" => 1e-9,
        "PICO" => 1e-12,
        "FEMTO" => 1e-15,
        "ATTO" => 1e-18,
        _ => return None,
    })
}

/// (1 length / 2 plane angle / 0 other, metres or radians per unit).
fn unit(st: &Store, id: u64, depth: u32) -> Option<(u8, f64)> {
    if depth > 8 {
        return None;
    }
    let hint = if st.is(id, "LENGTH_UNIT") {
        1
    } else if st.is(id, "PLANE_ANGLE_UNIT") {
        2
    } else {
        0
    };
    if let Some(a) = st.rec(id, "SI_UNIT") {
        let (pre, name) = match a.len() {
            2 => (&a[0], &a[1]),
            3 => (&a[1], &a[2]),
            _ => return None,
        };
        let f = match pre.en() {
            Some(e) => prefix(e)?,
            None => 1.0,
        };
        let k = match name.en()? {
            "METRE" => 1,
            "RADIAN" => 2,
            _ => hint,
        };
        return Some((k, f));
    }
    if let Some(a) = st.rec(id, "CONVERSION_BASED_UNIT") {
        let m = a.get(1)?.id()?;
        let mw = ["LENGTH_MEASURE_WITH_UNIT", "PLANE_ANGLE_MEASURE_WITH_UNIT", "MEASURE_WITH_UNIT"]
            .iter()
            .find_map(|n| st.rec(m, n).filter(|x| x.len() >= 2))?;
        let v = mw[0].num()?;
        let (k, f) = unit(st, mw[1].id()?, depth + 1)?;
        return Some((if hint != 0 { hint } else { k }, v * f));
    }
    None
}

/// Arguments of a representation (`name, items, context`), simple or complex.
fn rep_args(st: &Store, id: u64) -> Option<&[P]> {
    if let Some(a) = st.rec(id, "REPRESENTATION") {
        return Some(a);
    }
    let k = st.kind(id)?;
    let a = st.rec(id, k)?;
    (k.ends_with("_REPRESENTATION") && a.len() >= 3 && a[1].list().is_some() && a[2].id().is_some())
        .then_some(a)
}

struct Rep {
    items: Vec<u64>,
    units: Units,
}

/// Union-find over representations joined without a transform; the root is the lowest id.
#[derive(Default)]
struct Groups(HashMap<u64, u64>);

impl Groups {
    fn find(&self, x: u64) -> u64 {
        let mut r = x;
        while let Some(&p) = self.0.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        r
    }

    fn union(&mut self, a: u64, b: u64) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0.insert(ra.max(rb), ra.min(rb));
        }
    }
}

struct Child {
    child: u64,
    xf: Xf,
    via: u64,
}

struct Graph {
    solids: BTreeMap<u64, Vec<(u64, Units)>>,
    kids: BTreeMap<u64, Vec<Child>>,
    names: HashMap<u64, String>,
}

impl Graph {
    fn walk(&self, st: &Store, g: u64, xf: Xf, path: &mut Vec<u64>, stack: &mut Vec<u64>, out: &mut Vec<Inst>) {
        if stack.contains(&g) || stack.len() > 32 {
            return;
        }
        stack.push(g);
        let list = self.solids.get(&g).map_or(&[][..], |v| &v[..]);
        for (k, &(solid, units)) in list.iter().enumerate() {
            let own = solid_name(st, solid);
            let name = match (self.names.get(&g), own) {
                (Some(p), Some(o)) if list.len() > 1 => format!("{p} / {o}"),
                (Some(p), None) if list.len() > 1 => format!("{p} ({})", k + 1),
                (Some(p), _) => p.clone(),
                (None, Some(o)) => o,
                (None, None) => format!("Solid #{solid}"),
            };
            out.push(Inst {
                solid,
                units,
                xf,
                path: path.clone(),
                name,
            });
        }
        for c in self.kids.get(&g).map_or(&[][..], |v| &v[..]) {
            path.push(c.via);
            self.walk(st, c.child, xf.after(&c.xf), path, stack, out);
            path.pop();
        }
        stack.pop();
    }
}

fn is_solid(st: &Store, id: u64) -> bool {
    st.kind(id).is_some_and(|k| SOLIDS.contains(&k))
}

fn solid_name(st: &Store, id: u64) -> Option<String> {
    let k = st.kind(id)?;
    let n = st.rec(id, k)?.first()?.text()?.trim();
    (!n.is_empty()).then(|| n.to_string())
}

fn product_name(st: &Store, pd: u64) -> Option<String> {
    let f = st.rec(pd, st.kind(pd)?)?.get(2)?.id()?;
    let prod = st.rec(f, st.kind(f)?)?.get(2)?.id()?;
    let a = st.rec(prod, "PRODUCT")?;
    let n = a
        .get(1)
        .and_then(P::text)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| a.first().and_then(P::text))?
        .trim();
    (!n.is_empty()).then(|| n.to_string())
}

/// Relating and related product definitions of an assembly usage occurrence.
fn usage(st: &Store, id: u64) -> Option<(u64, u64)> {
    let a = st.rec(id, st.kind(id)?)?;
    Some((a.get(3)?.id()?, a.get(4)?.id()?))
}

/// Every placed solid: product tree from the roots down, then solids no tree reaches.
fn structure(st: &Store, warns: &mut Vec<String>) -> (Vec<Inst>, f64) {
    let mut ids: Vec<u64> = st.ents.keys().copied().collect();
    ids.sort_unstable();
    let mut ctx_units: BTreeMap<u64, (Option<f64>, Option<f64>)> = BTreeMap::new();
    let mut raw: Vec<(u64, Vec<u64>, u64)> = vec![];
    for &id in &ids {
        let Some(a) = rep_args(st, id) else { continue };
        let ctx = a[2].id().unwrap_or(0);
        raw.push((id, a[1].ids(), ctx));
        ctx_units.entry(ctx).or_insert_with(|| {
            let mut u = (None, None);
            if let Some(g) = st.rec(ctx, "GLOBAL_UNIT_ASSIGNED_CONTEXT") {
                for id in g.first().map(P::ids).unwrap_or_default() {
                    match unit(st, id, 0) {
                        Some((1, f)) => u.0 = Some(f),
                        Some((2, f)) => u.1 = Some(f),
                        _ => {}
                    }
                }
            }
            u
        });
    }
    let def_len = ctx_units.values().find_map(|u| u.0).unwrap_or_else(|| {
        warns.push("no length unit in the file: millimetres assumed".into());
        1e-3
    });
    let units_of = |ctx: u64| {
        let u = ctx_units.get(&ctx).copied().unwrap_or((None, None));
        Units {
            len: u.0.unwrap_or(def_len),
            ang: u.1.unwrap_or(1.0),
        }
    };
    let def = Units { len: def_len, ang: 1.0 };
    let mut reps: BTreeMap<u64, Rep> = BTreeMap::new();
    let mut solid_rep: HashMap<u64, u64> = HashMap::new();
    for (id, items, ctx) in raw {
        for &i in &items {
            if is_solid(st, i) {
                solid_rep.entry(i).or_insert(id);
            }
        }
        reps.insert(id, Rep { items, units: units_of(ctx) });
    }

    let rel_names = [
        "REPRESENTATION_RELATIONSHIP",
        "SHAPE_REPRESENTATION_RELATIONSHIP",
        "REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION",
    ];
    let mut rels: Vec<u64> = rel_names.iter().flat_map(|n| st.all(n).iter().copied()).collect();
    rels.sort_unstable();
    rels.dedup();
    let mut groups = Groups::default();
    let mut moved: Vec<(u64, u64, u64, u64)> = vec![];
    for id in rels {
        let Some(a) = rel_names.iter().find_map(|n| st.rec(id, n).filter(|a| a.len() >= 4)) else { continue };
        let (Some(r1), Some(r2)) = (a[2].id(), a[3].id()) else { continue };
        if !reps.contains_key(&r1) || !reps.contains_key(&r2) {
            continue;
        }
        let op = st
            .rec(id, "REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
            .and_then(|x| if x.len() >= 5 { x[4].id() } else { x.first().and_then(P::id) });
        match op {
            None => groups.union(r1, r2),
            Some(op) => moved.push((id, r1, r2, op)),
        }
    }

    let mut pd_of_group: BTreeMap<u64, u64> = BTreeMap::new();
    let mut groups_of_pd: HashMap<u64, HashSet<u64>> = HashMap::new();
    for &id in st.all("SHAPE_DEFINITION_REPRESENTATION") {
        let Some(a) = st.rec(id, "SHAPE_DEFINITION_REPRESENTATION") else { continue };
        let (Some(pds), Some(rep)) = (a.first().and_then(P::id), a.get(1).and_then(P::id)) else { continue };
        let Some(def) = st.rec(pds, "PRODUCT_DEFINITION_SHAPE").and_then(|x| x.get(2)).and_then(P::id) else {
            continue;
        };
        if !reps.contains_key(&rep) || st.is(def, "NEXT_ASSEMBLY_USAGE_OCCURRENCE") {
            continue;
        }
        let g = groups.find(rep);
        pd_of_group.entry(g).or_insert(def);
        groups_of_pd.entry(def).or_default().insert(g);
    }
    let mut rr_usage: HashMap<u64, u64> = HashMap::new();
    for &id in st.all("CONTEXT_DEPENDENT_SHAPE_REPRESENTATION") {
        let Some(a) = st.rec(id, "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION") else { continue };
        let (Some(rr), Some(pds)) = (a.first().and_then(P::id), a.get(1).and_then(P::id)) else { continue };
        if let Some(u) = st.rec(pds, "PRODUCT_DEFINITION_SHAPE").and_then(|x| x.get(2)).and_then(P::id) {
            rr_usage.insert(rr, u);
        }
    }

    let mut members: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for &r in reps.keys() {
        members.entry(groups.find(r)).or_default().push(r);
    }
    let in_group = |g: u64, item: u64| {
        members
            .get(&g)
            .is_some_and(|rs| rs.iter().any(|r| reps[r].items.contains(&item)))
    };
    let place = |item: u64, len: f64| curve::transform(st, item).map(|x| x.scaled(len));

    let mut kids: BTreeMap<u64, Vec<Child>> = BTreeMap::new();
    let mut bad = 0usize;
    for (rid, r1, r2, op) in moved {
        let (mut child, mut parent) = (r1, r2);
        let via = rr_usage.get(&rid).copied();
        if let Some((_, cpd)) = via.and_then(|u| usage(st, u)) {
            let gc = groups_of_pd.get(&cpd);
            let has = |r: u64| gc.is_some_and(|s| s.contains(&groups.find(r)));
            if has(r2) && !has(r1) {
                std::mem::swap(&mut child, &mut parent);
            }
        }
        let (uc, up) = (reps[&child].units.len, reps[&parent].units.len);
        let xf = if let Some(a) = st.rec(op, "ITEM_DEFINED_TRANSFORMATION") {
            let (Some(i1), Some(i2)) = (a.get(2).and_then(P::id), a.get(3).and_then(P::id)) else {
                bad += 1;
                continue;
            };
            let (mut ci, mut pi) = if child == r1 { (i1, i2) } else { (i2, i1) };
            let gc = groups.find(child);
            if !in_group(gc, ci) && in_group(gc, pi) {
                std::mem::swap(&mut ci, &mut pi);
            }
            match (place(ci, uc).ok().and_then(|x| x.inverse()), place(pi, up)) {
                (Some(inv), Ok(p)) => p.after(&inv),
                _ => {
                    bad += 1;
                    continue;
                }
            }
        } else {
            match place(op, up) {
                Ok(x) => x,
                Err(_) => {
                    bad += 1;
                    continue;
                }
            }
        };
        kids.entry(groups.find(parent)).or_default().push(Child {
            child: groups.find(child),
            xf,
            via: via.unwrap_or(rid),
        });
    }
    let mut solids: BTreeMap<u64, Vec<(u64, Units)>> = BTreeMap::new();
    for (&rid, rep) in &reps {
        let g = groups.find(rid);
        for &i in &rep.items {
            if is_solid(st, i) {
                let v = solids.entry(g).or_default();
                if !v.iter().any(|s| s.0 == i) {
                    v.push((i, rep.units));
                }
            } else if let Some(a) = st.rec(i, "MAPPED_ITEM") {
                let src = a.get(1).and_then(P::id).and_then(|s| st.rec(s, "REPRESENTATION_MAP"));
                let (Some(src), Some(target)) = (src, a.get(2).and_then(P::id)) else { continue };
                let (Some(origin), Some(mapped)) = (src.first().and_then(P::id), src.get(1).and_then(P::id)) else {
                    continue;
                };
                let Some(mrep) = reps.get(&mapped) else { continue };
                match (place(origin, mrep.units.len).ok().and_then(|x| x.inverse()), place(target, rep.units.len)) {
                    (Some(inv), Ok(t)) => kids.entry(g).or_default().push(Child {
                        child: groups.find(mapped),
                        xf: t.after(&inv),
                        via: i,
                    }),
                    _ => bad += 1,
                }
            }
        }
    }
    if bad > 0 {
        warns.push(format!("{bad} assembly placement(s) could not be read and were left out"));
    }
    let names: HashMap<u64, String> = pd_of_group
        .iter()
        .filter_map(|(&g, &pd)| product_name(st, pd).map(|n| (g, n)))
        .collect();
    let child_set: HashSet<u64> = kids.values().flatten().map(|c| c.child).collect();
    let graph = Graph { solids, kids, names };
    let mut out = vec![];
    let roots: Vec<u64> = members.keys().copied().filter(|g| !child_set.contains(g)).collect();
    for g in roots {
        graph.walk(st, g, Xf::ID, &mut vec![], &mut vec![], &mut out);
    }
    let reached: HashSet<u64> = out.iter().map(|i| i.solid).collect();
    for k in SOLIDS {
        for &id in st.all(k) {
            if reached.contains(&id) || st.kind(id) != Some(k) {
                continue;
            }
            let units = solid_rep.get(&id).map_or(def, |r| reps[r].units);
            out.push(Inst {
                solid: id,
                units,
                xf: Xf::ID,
                path: vec![],
                name: solid_name(st, id).unwrap_or_else(|| format!("Solid #{id}")),
            });
        }
    }
    (out, def_len)
}

fn colour(st: &Store, id: u64) -> Option<[u8; 3]> {
    if let Some(a) = st.rec(id, "COLOUR_RGB") {
        let c = [1, 2, 3].map(|i| a.get(i).and_then(P::num).unwrap_or(0.0));
        return Some(c.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8));
    }
    let a = st.rec(id, "DRAUGHTING_PRE_DEFINED_COLOUR")?;
    Some(match a.first()?.text()?.trim().to_ascii_lowercase().as_str() {
        "red" => [255, 0, 0],
        "green" => [0, 255, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "magenta" => [255, 0, 255],
        "cyan" => [0, 255, 255],
        "black" => [0, 0, 0],
        "white" => [255, 255, 255],
        _ => return None,
    })
}

/// Surface colour of presentation style assignments: fill area first, then rendering.
fn style_colour(st: &Store, styles: &[u64]) -> Option<Rgba> {
    let (mut fill, mut render, mut alpha) = (None, None, 1.0f32);
    for &psa in styles {
        let list = ["PRESENTATION_STYLE_ASSIGNMENT", "PRESENTATION_STYLE_BY_CONTEXT"]
            .iter()
            .find_map(|n| st.rec(psa, n))
            .and_then(|a| a.first())
            .map(P::ids)
            .unwrap_or_default();
        for su in list {
            let Some(side) = st.rec(su, "SURFACE_STYLE_USAGE").and_then(|a| a.get(1)).and_then(P::id) else {
                continue;
            };
            let elems = st.rec(side, "SURFACE_SIDE_STYLE").and_then(|a| a.get(1)).map(P::ids).unwrap_or_default();
            for el in elems {
                if let Some(fas) = st.rec(el, "SURFACE_STYLE_FILL_AREA").and_then(|a| a.first()).and_then(P::id) {
                    for fs in st.rec(fas, "FILL_AREA_STYLE").and_then(|a| a.get(1)).map(P::ids).unwrap_or_default() {
                        if let Some(c) = st
                            .rec(fs, "FILL_AREA_STYLE_COLOUR")
                            .and_then(|a| a.get(1))
                            .and_then(P::id)
                            .and_then(|c| colour(st, c))
                        {
                            fill.get_or_insert(c);
                        }
                    }
                }
                for rn in ["SURFACE_STYLE_RENDERING_WITH_PROPERTIES", "SURFACE_STYLE_RENDERING"] {
                    let Some(r) = st.rec(el, rn) else { continue };
                    if let Some(c) = r.get(1).and_then(P::id).and_then(|c| colour(st, c)) {
                        render.get_or_insert(c);
                    }
                    for pr in r.get(2).map(P::ids).unwrap_or_default() {
                        if let Some(t) = st.rec(pr, "SURFACE_STYLE_TRANSPARENT").and_then(|a| a.first()).and_then(P::num) {
                            alpha = (1.0 - t as f32).clamp(0.0, 1.0);
                        }
                    }
                }
            }
        }
    }
    fill.or(render).map(|c| (c, alpha))
}

/// Styled item to its surface colour; overriding styles win.
fn colours(st: &Store) -> HashMap<u64, Rgba> {
    let mut m = HashMap::new();
    let passes: [&[&str]; 2] = [
        &["STYLED_ITEM"],
        &["OVER_RIDING_STYLED_ITEM", "CONTEXT_DEPENDENT_OVER_RIDING_STYLED_ITEM"],
    ];
    for names in passes {
        for n in names {
            for &id in st.all(n) {
                let Some(a) = st.rec(id, n) else { continue };
                let (Some(styles), Some(item)) = (a.get(1), a.get(2).and_then(P::id)) else { continue };
                if let Some(c) = style_colour(st, &styles.ids()) {
                    m.insert(item, c);
                }
            }
        }
    }
    m
}

/// Colour of the solid, else of its shells, else the colour covering most of its triangles.
fn solid_colour(st: &Store, cols: &HashMap<u64, Rgba>, solid: u64, m: &Mesh) -> Option<Rgba> {
    if let Some(c) = cols.get(&solid) {
        return Some(*c);
    }
    for (s, _) in topo::shells(st, solid) {
        if let Some(c) = cols.get(&s) {
            return Some(*c);
        }
    }
    let mut w: BTreeMap<([u8; 3], u32), usize> = BTreeMap::new();
    for (f, n) in &m.faces {
        if let Some(c) = cols.get(f) {
            *w.entry((c.0, c.1.to_bits())).or_default() += n;
        }
    }
    w.into_iter()
        .max_by_key(|(k, n)| (*n, *k))
        .map(|((c, a), _)| (c, f32::from_bits(a)))
}
