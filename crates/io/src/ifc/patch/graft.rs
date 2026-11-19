// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! New geometry for the patch: the model goes through the exporter into a temp file, and the
//! entities of changed elements are copied from it under fresh ids. The whole placement chain comes
//! along, so an element's place in the world does not depend on the source's spatial placements.
//! Exporter output is in metres and radians: lengths are converted to the file's units here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_model::Model;

use super::lines::{
    guid_of, join_args, list_items, map_refs, owned, parse_ent, ref_list, scale_numbers,
    single_ref, split_top, string_arg, upper, Ent, Stmts,
};
use super::refs::{probe, Index};
use super::{
    doubt, failed, GeomEdit, Out, PatchError, Units, Work, DOUBT_BUILDING, DOUBT_EDIT,
    DOUBT_NESTED, DOUBT_STRUCTURE, REASON_EXPORT, REASON_SCHEMA,
};
use crate::ifc::export::sink::export_rows;
use crate::ifc::schema_spec::IfcSchema;
use crate::job::Progress;

mod parts;

const CONTAINED: &str = "IFCRELCONTAINEDINSPATIALSTRUCTURE";
const VOIDS: &str = "IFCRELVOIDSELEMENT";
const FILLS: &str = "IFCRELFILLSELEMENT";
const AGGREGATES: &str = "IFCRELAGGREGATES";
const NESTS: &str = "IFCRELNESTS";
const DEFINES: &str = "IFCRELDEFINESBYPROPERTIES";
const MATERIAL: &str = "IFCRELASSOCIATESMATERIAL";
/// Property set in which the exporter keeps a component's frame; the importer places it from there.
const FRAME_SET: &str = "DDC_Component";
const FRAME_PROPS: [&str; 2] = ["Local", "World"];

/// Joins a filler's relation and the filler in the pending entry of its opening:
/// `IFCRELFILLSELEMENT@<GlobalId>`, `IFCRELASSOCIATESMATERIAL@<GlobalId>`.
const FILL_AT: &str = "@";
/// Separates a relation type from the GlobalId of the part it belongs to, in a pending link.
const PART_AT: &str = "^";

/// Tables a patch does not add to: they change the file's structure, not one element.
const NO_ADD: [&str; 2] = ["grid", "curtain_wall"];
const STOREY: &str = "IFCBUILDINGSTOREY";

/// Geometry and data entities valid in IFC2X3 with the exporter's attribute layout.
const X3_ENTITIES: [&str; 29] = [
    "IFCCARTESIANPOINT",
    "IFCDIRECTION",
    "IFCAXIS1PLACEMENT",
    "IFCAXIS2PLACEMENT2D",
    "IFCAXIS2PLACEMENT3D",
    "IFCLOCALPLACEMENT",
    "IFCPOLYLINE",
    "IFCTRIMMEDCURVE",
    "IFCCIRCLE",
    "IFCPLANE",
    "IFCPOLYLOOP",
    "IFCFACEOUTERBOUND",
    "IFCFACESURFACE",
    "IFCGEOMETRICCURVESET",
    "IFCRECTANGLEPROFILEDEF",
    "IFCCIRCLEPROFILEDEF",
    "IFCARBITRARYCLOSEDPROFILEDEF",
    "IFCARBITRARYPROFILEDEFWITHVOIDS",
    "IFCEXTRUDEDAREASOLID",
    "IFCREVOLVEDAREASOLID",
    "IFCBOOLEANRESULT",
    "IFCSHAPEREPRESENTATION",
    "IFCPRODUCTDEFINITIONSHAPE",
    "IFCPROPERTYSET",
    "IFCPROPERTYSINGLEVALUE",
    "IFCELEMENTQUANTITY",
    "IFCQUANTITYLENGTH",
    "IFCQUANTITYAREA",
    "IFCQUANTITYVOLUME",
];

/// Products valid in IFC2X3, with their attribute count there.
const X3_PRODUCTS: [(&str, usize); 11] = [
    ("IFCWALL", 8),
    ("IFCCOLUMN", 8),
    ("IFCBEAM", 8),
    ("IFCMEMBER", 8),
    ("IFCPLATE", 8),
    ("IFCOPENINGELEMENT", 8),
    ("IFCDOOR", 10),
    ("IFCWINDOW", 10),
    ("IFCSLAB", 9),
    ("IFCCOVERING", 9),
    ("IFCBUILDINGELEMENTPROXY", 9),
];

/// Attributes holding lengths (in metres in the export). A type not listed here and not in
/// [`NO_LENGTHS`] cannot be converted to another unit.
const LENGTHS: [(&str, &[usize]); 10] = [
    ("IFCEXTRUDEDAREASOLIDTAPERED", &[3]),
    ("IFCCARTESIANPOINT", &[0]),
    ("IFCCARTESIANPOINTLIST2D", &[0]),
    ("IFCCARTESIANPOINTLIST3D", &[0]),
    ("IFCCIRCLE", &[1]),
    ("IFCCIRCLEPROFILEDEF", &[3]),
    ("IFCRECTANGLEPROFILEDEF", &[3, 4]),
    ("IFCEXTRUDEDAREASOLID", &[3]),
    ("IFCSWEPTDISKSOLID", &[1, 2]),
    ("IFCMATERIALLAYER", &[1]),
];

const NO_LENGTHS: [&str; 39] = [
    "IFCPOLYGONALFACESET",
    "IFCINDEXEDPOLYGONALFACE",
    "IFCINDEXEDPOLYGONALFACEWITHVOIDS",
    "IFCDERIVEDPROFILEDEF",
    "IFCCARTESIANTRANSFORMATIONOPERATOR2D",
    "IFCDIRECTION",
    "IFCAXIS1PLACEMENT",
    "IFCAXIS2PLACEMENT2D",
    "IFCAXIS2PLACEMENT3D",
    "IFCLOCALPLACEMENT",
    "IFCPOLYLINE",
    "IFCINDEXEDPOLYCURVE",
    "IFCTRIANGULATEDFACESET",
    "IFCTRIMMEDCURVE",
    "IFCPLANE",
    "IFCPOLYLOOP",
    "IFCFACEOUTERBOUND",
    "IFCFACESURFACE",
    "IFCGEOMETRICCURVESET",
    "IFCARBITRARYCLOSEDPROFILEDEF",
    "IFCARBITRARYPROFILEDEFWITHVOIDS",
    "IFCREVOLVEDAREASOLID",
    "IFCBOOLEANRESULT",
    "IFCSHAPEREPRESENTATION",
    "IFCPRODUCTDEFINITIONSHAPE",
    "IFCMAPPEDITEM",
    "IFCREPRESENTATIONMAP",
    "IFCCARTESIANTRANSFORMATIONOPERATOR3D",
    "IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM",
    "IFCPROPERTYSET",
    "IFCPROPERTYSINGLEVALUE",
    "IFCELEMENTQUANTITY",
    "IFCQUANTITYLENGTH",
    "IFCQUANTITYAREA",
    "IFCQUANTITYVOLUME",
    "IFCQUANTITYWEIGHT",
    "IFCSIUNIT",
    "IFCMATERIAL",
    "IFCMATERIALLAYERSET",
];

fn is_context(ty: &str) -> bool {
    matches!(
        ty,
        "IFCGEOMETRICREPRESENTATIONCONTEXT" | "IFCGEOMETRICREPRESENTATIONSUBCONTEXT"
    )
}

fn x3_arity(ty: &str) -> Option<usize> {
    X3_PRODUCTS.iter().find(|p| p.0 == ty).map(|p| p.1)
}

/// The export of the model, read for random access.
struct Export {
    bytes: Vec<u8>,
    at: HashMap<u32, (usize, usize)>,
    rooted: HashMap<String, u32>,
    /// GlobalIds on more than one line of the export.
    dup: HashSet<String>,
    rels: Vec<(String, Vec<u8>)>,
    by_ref: HashMap<u32, Vec<usize>>,
    rows: HashMap<(String, i64), String>,
}

impl Export {
    fn read(path: &Path, rows: HashMap<(String, i64), String>) -> Result<Self, PatchError> {
        let bytes = std::fs::read(path).map_err(failed)?;
        let mut at = HashMap::new();
        let mut rooted = HashMap::new();
        let mut dup = HashSet::new();
        let mut rels = vec![];
        let mut by_ref: HashMap<u32, Vec<usize>> = HashMap::new();
        {
            let mut st = Stmts::new(&bytes[..]);
            let mut pos = 0usize;
            while let Some(p) = st.next().map_err(failed)? {
                let from = pos + p.lead.len();
                pos = from + p.body.len();
                let Some(e) = parse_ent(p.body) else { continue };
                at.insert(e.id, (from, pos));
                if let Some(g) = guid_of(e.args) {
                    if rooted.insert(g.to_string(), e.id).is_some() {
                        dup.insert(g.to_string());
                    }
                    let ty = upper(e.ty);
                    if ty.starts_with("IFCREL") {
                        let i = rels.len();
                        for x in split_top(e.args).iter().skip(4) {
                            super::lines::for_each_ref(x, &mut |t| {
                                let v = by_ref.entry(t).or_default();
                                if v.last() != Some(&i) {
                                    v.push(i);
                                }
                            });
                        }
                        rels.push((ty, e.args.to_vec()));
                    }
                }
            }
        }
        Ok(Self {
            bytes,
            at,
            rooted,
            dup,
            rels,
            by_ref,
            rows,
        })
    }

    fn ent(&self, id: u32) -> Option<Ent<'_>> {
        let (a, b) = *self.at.get(&id)?;
        parse_ent(&self.bytes[a..b])
    }

    fn guid_at(&self, id: u32) -> Option<&str> {
        guid_of(self.ent(id)?.args)
    }

    /// Values (argument 3) of the frame properties of export element `id`, by property name.
    /// `Ok(None)`: no complete frame. `Err`: two or more frame sets relate to the element, the
    /// same rule as on the file side, so no arbitrary one is taken.
    /// `who` names the element in the reason (its kind and stable id).
    fn frame(&self, id: u32, who: &str) -> Result<Option<HashMap<String, Vec<u8>>>, String> {
        let mut out = HashMap::new();
        let mut sets = 0usize;
        let Some(rels) = self.by_ref.get(&id) else {
            return Ok(None);
        };
        for i in rels {
            let (ty, args) = &self.rels[*i];
            let a = split_top(args);
            if ty != DEFINES || !related(&a).contains(&id) {
                continue;
            }
            let Some(set) = a.get(5).and_then(|v| single_ref(v)).and_then(|s| self.ent(s)) else {
                continue;
            };
            let sa = split_top(set.args);
            if upper(set.ty) != "IFCPROPERTYSET" || !set_named(&sa, FRAME_SET) {
                continue;
            }
            sets += 1;
            if sets > 1 {
                return Err(repeated_set(who));
            }
            for it in sa.get(4).and_then(|v| list_items(v)).unwrap_or_default() {
                let Some(p) = single_ref(it).and_then(|r| self.ent(r)) else {
                    continue;
                };
                let pa = split_top(p.args);
                if upper(p.ty) != "IFCPROPERTYSINGLEVALUE" {
                    continue;
                }
                if let Some(name) = FRAME_PROPS.iter().find(|n| named(&pa, n)) {
                    let Some(v) = pa.get(2).filter(|v| !unset(v)) else {
                        return Err(bad_line(who, name));
                    };
                    if out.insert((*name).to_string(), v.to_vec()).is_some() {
                        return Err(bad_line(who, name));
                    }
                }
            }
        }
        Ok((out.len() == FRAME_PROPS.len()).then_some(out))
    }
}

/// Reason shared by both sides when an element has a shared or repeated frame set.
fn repeated_set(who: &str) -> String {
    format!("{who}: {FRAME_SET} set is shared or repeated")
}

/// Reason shared by both sides when a frame property is repeated or has no value.
fn bad_line(who: &str, name: &str) -> String {
    format!("{who}: {name} line is repeated or has no value")
}

/// A `$` (unset) or empty value is no value; both sides share this rule.
fn unset(v: &[u8]) -> bool {
    let t = v.trim_ascii();
    t.is_empty() || t == b"$"
}

/// Items of a relation's first end (argument 5).
fn related(a: &[&[u8]]) -> Vec<u32> {
    a.get(4)
        .and_then(|v| list_items(v))
        .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
        .unwrap_or_default()
}

/// Argument 1 (Name) is the string `name`.
fn named(a: &[&[u8]], name: &str) -> bool {
    a.first().and_then(|v| string_arg(v)).as_deref() == Some(name)
}

/// Argument 3 (Name, after GlobalId and OwnerHistory) of a property set is the string `name`.
fn set_named(a: &[&[u8]], name: &str) -> bool {
    a.get(2).and_then(|v| string_arg(v)).as_deref() == Some(name)
}

/// One frame property line of the file.
#[derive(Debug)]
struct FrameLine {
    name: String,
    id: u32,
    ty: String,
    args: Vec<u8>,
}

/// The frame property lines of each place-only element of the file. Each must be the element's
/// own: a set or value shared with another element, or a missing one, is a doubt, since a new
/// placement written without its frame would put the element back on the next read.
fn frame_lines(
    ix: &Index,
    src: &Path,
    owners: &[u32],
    o: &Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<HashMap<u32, Vec<FrameLine>>, PatchError> {
    if owners.is_empty() {
        return Ok(HashMap::new());
    }
    // (owner, relation, set)
    let mut cand: Vec<(u32, u32, u32)> = vec![];
    for &own in owners {
        for r in ix.rels_of(own, DEFINES) {
            if o.remove.contains(&r.id) {
                continue;
            }
            let now = o.current_args(r.id, &r.args);
            let a = split_top(&now);
            if !related(&a).contains(&own) {
                continue;
            }
            if let Some(set) = a.get(5).and_then(|v| single_ref(v)) {
                cand.push((own, r.id, set));
            }
        }
    }
    let sets = if cand.is_empty() {
        HashMap::new()
    } else {
        let ids: HashSet<u32> = cand.iter().map(|c| c.2).collect();
        probe(src, &ids, ctl)?
    };
    let mut set_of: HashMap<u32, u32> = HashMap::new();
    for (own, rel, set) in &cand {
        let Some(p) = sets.get(set) else { continue };
        let sa = split_top(&p.args);
        if p.ty != "IFCPROPERTYSET" || !set_named(&sa, FRAME_SET) {
            continue;
        }
        let rel_args = ix
            .rels_of(*own, DEFINES)
            .find(|r| r.id == *rel)
            .map(|r| o.current_args(r.id, &r.args))
            .unwrap_or_default();
        let only = related(&split_top(&rel_args)) == [*own] && p.referrers.as_slice() == [*rel];
        if !only || set_of.insert(*own, *set).is_some() {
            return Err(doubt(DOUBT_EDIT, repeated_set(&format!("#{own}"))));
        }
    }
    let mut items_of: HashMap<u32, Vec<u32>> = HashMap::new();
    for (own, set) in &set_of {
        let items = split_top(&sets[set].args)
            .get(4)
            .and_then(|v| list_items(v))
            .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
            .unwrap_or_default();
        items_of.insert(*own, items);
    }
    let wanted: HashSet<u32> = items_of.values().flatten().copied().collect();
    let items = if wanted.is_empty() {
        HashMap::new()
    } else {
        probe(src, &wanted, ctl)?
    };
    let mut out = HashMap::new();
    for &own in owners {
        let Some(&set) = set_of.get(&own) else {
            return Err(doubt(
                DOUBT_EDIT,
                format!("#{own}: no {FRAME_SET} frame to move with the placement"),
            ));
        };
        let mut lines: Vec<FrameLine> = vec![];
        for it in items_of.get(&own).into_iter().flatten() {
            let Some(p) = items.get(it) else { continue };
            let pa = split_top(&p.args);
            if p.ty != "IFCPROPERTYSINGLEVALUE" {
                continue;
            }
            let Some(name) = FRAME_PROPS.iter().find(|n| named(&pa, n)) else {
                continue;
            };
            if p.referrers.as_slice() != [set] {
                return Err(doubt(DOUBT_EDIT, format!("#{own}: {name} value is shared")));
            }
            if pa.get(2).is_none_or(|v| unset(v)) || lines.iter().any(|l| l.name == *name) {
                return Err(doubt(DOUBT_EDIT, bad_line(&format!("#{own}"), name)));
            }
            lines.push(FrameLine {
                name: (*name).to_string(),
                id: *it,
                ty: p.ty.clone(),
                args: p.args.clone(),
            });
        }
        if lines.len() != FRAME_PROPS.len() {
            return Err(doubt(
                DOUBT_EDIT,
                format!("#{own}: no {FRAME_SET} frame to move with the placement"),
            ));
        }
        out.insert(own, lines);
    }
    Ok(out)
}

/// Removes the temp export when dropped.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Copying from the export into the patch.
struct Tx<'a> {
    ix: &'a Index,
    ex: &'a Export,
    schema: IfcSchema,
    u: &'a Units,
    map: HashMap<u32, u32>,
    /// Frame property lines of the place-only elements, by file id.
    frames: HashMap<u32, Vec<FrameLine>>,
}

impl<'a> Tx<'a> {
    /// The entity tree under `id` fits the file; `Err` names the first type that does not.
    fn check(&self, id: u32, root: bool, seen: &mut HashSet<u32>) -> Result<(), String> {
        if !seen.insert(id) {
            return Ok(());
        }
        let Some(e) = self.ex.ent(id) else {
            return Err(format!("#{id}"));
        };
        let ty = upper(e.ty);
        if is_context(&ty) || (!root && guid_of(e.args).is_some()) {
            return Ok(());
        }
        let rooted = guid_of(e.args).is_some();
        if self.schema == IfcSchema::Ifc2x3
            && !X3_ENTITIES.contains(&ty.as_str())
            && x3_arity(&ty).is_none()
        {
            return Err(ty);
        }
        if self.u.length != 1.0
            && !rooted
            && !LENGTHS.iter().any(|l| l.0 == ty)
            && !NO_LENGTHS.contains(&ty.as_str())
            && ty != "IFCMATERIALLAYERSETUSAGE"
        {
            return Err(ty);
        }
        if self.u.angle != 1.0
            && ty == "IFCTRIMMEDCURVE"
            && e.args.windows(9).any(|w| w == b"IFCPARAME")
        {
            return Err(ty);
        }
        let mut refs = vec![];
        super::lines::for_each_ref(e.args, &mut |r| refs.push(r));
        for r in refs {
            self.check(r, false, seen)?;
        }
        Ok(())
    }

    fn context(&self, ty: &str, args: &[u8]) -> Result<u32, PatchError> {
        if ty == "IFCGEOMETRICREPRESENTATIONSUBCONTEXT" {
            let name = split_top(args)
                .first()
                .and_then(|a| string_arg(a))
                .unwrap_or_default();
            if let Some(i) = self.ix.subcontexts.get(&name.to_ascii_uppercase()) {
                return Ok(*i);
            }
        }
        self.ix
            .context
            .ok_or_else(|| doubt(DOUBT_STRUCTURE, "the file has no representation context"))
    }

    /// `#id` of the file for entity `id` of the export, copying it first when needed.
    fn emit(&mut self, id: u32, o: &mut Out) -> Result<u32, PatchError> {
        if let Some(n) = self.map.get(&id) {
            return Ok(*n);
        }
        let ex = self.ex;
        let e = ex
            .ent(id)
            .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("export has no #{id}")))?;
        let ty = upper(e.ty);
        if is_context(&ty) {
            let n = self.context(&ty, e.args)?;
            self.map.insert(id, n);
            return Ok(n);
        }
        if let Some(g) = guid_of(e.args) {
            if let Some(n) = self.ix.rooted.get(g).filter(|n| !o.remove.contains(*n)) {
                self.map.insert(id, *n);
                return Ok(*n);
            }
            if !matches!(ty.as_str(), "IFCPROPERTYSET" | "IFCELEMENTQUANTITY") {
                return Err(doubt(DOUBT_EDIT, format!("{ty} {g} is not in the source")));
            }
            let n = o.alloc();
            self.map.insert(id, n);
            o.new_guids.insert(g.to_string());
            let args = self.root_args(&ty, e.args, o)?;
            o.put(n, &ty, &args);
            return Ok(n);
        }
        let n = o.alloc();
        self.map.insert(id, n);
        let args = self.render(&ty, e.args, o)?;
        o.put(n, &ty, &args);
        Ok(n)
    }

    fn render(&mut self, ty: &str, args: &[u8], o: &mut Out) -> Result<Vec<u8>, PatchError> {
        let mapped = map_refs(args, &mut |r| self.emit(r, o))?;
        Ok(self.scaled(ty, &mapped))
    }

    /// Metres and radians of the export in the file's units.
    fn scaled(&self, ty: &str, args: &[u8]) -> Vec<u8> {
        let u = self.u;
        let (kl, ka) = (1.0 / u.length, 1.0 / u.angle);
        let a = split_top(args);
        let mut out = owned(&a);
        if let Some((_, idx)) = LENGTHS.iter().find(|l| l.0 == ty) {
            for i in idx.iter().filter(|i| **i < a.len()) {
                out[*i] = scale_numbers(a[*i], kl);
            }
        }
        match ty {
            "IFCDOOR" | "IFCWINDOW" => {
                for i in [8usize, 9] {
                    if let Some(v) = a.get(i) {
                        out[i] = scale_numbers(v, kl);
                    }
                }
            }
            // RiserHeight, TreadLength
            "IFCSTAIRFLIGHT" => {
                for i in [10usize, 11] {
                    if let Some(v) = a.get(i) {
                        out[i] = scale_numbers(v, kl);
                    }
                }
            }
            "IFCREVOLVEDAREASOLID" => {
                if let Some(v) = a.get(3) {
                    out[3] = scale_numbers(v, ka);
                }
            }
            "IFCMATERIALLAYERSETUSAGE" => {
                if let Some(v) = a.get(3) {
                    out[3] = scale_numbers(v, kl);
                }
            }
            "IFCPROPERTYSINGLEVALUE" => {
                if let Some(v) = a.get(2) {
                    let k = measure_factor(v, u);
                    if k != 1.0 {
                        out[2] = scale_numbers(v, k);
                    }
                }
            }
            _ => {}
        }
        join_args(&out)
    }

    /// A rooted line for the file's schema: attribute count, mandatory owner history.
    fn root_args(&mut self, ty: &str, args: &[u8], o: &mut Out) -> Result<Vec<u8>, PatchError> {
        let rendered = self.render(ty, args, o)?;
        let a = split_top(&rendered);
        let mut out = owned(&a);
        if self.schema == IfcSchema::Ifc2x3 {
            if let Some(n) = x3_arity(ty) {
                out.truncate(n);
            }
            if ty == "IFCBUILDINGELEMENTPROXY" && out.len() > 8 {
                out[8] = b"$".to_vec();
            }
            self.fill_owner(&mut out)?;
        }
        Ok(join_args(&out))
    }

    fn fill_owner(&self, a: &mut [Vec<u8>]) -> Result<(), PatchError> {
        if a.get(1).is_some_and(|v| v == b"$") {
            let h = self
                .ix
                .owner
                .ok_or_else(|| doubt(DOUBT_STRUCTURE, "IFC2X3 file without an owner history"))?;
            a[1] = format!("#{h}").into_bytes();
        }
        Ok(())
    }

    /// A new relation: fresh id and GlobalId, stable across saves.
    fn new_rel(
        &mut self,
        ty: &str,
        seed: &str,
        ends: [Vec<u8>; 2],
        o: &mut Out,
    ) -> Result<u32, PatchError> {
        let guid = o.new_guid(&format!("{ty}|{seed}"), self.ix);
        let mut a = vec![
            format!("'{guid}'").into_bytes(),
            b"$".to_vec(),
            b"$".to_vec(),
            b"$".to_vec(),
            ends[0].clone(),
            ends[1].clone(),
        ];
        if self.schema == IfcSchema::Ifc2x3 {
            self.fill_owner(&mut a)?;
        }
        Ok(o.add(ty, &join_args(&a)))
    }
}

/// How many units of the file make one SI unit of a typed measure value.
fn measure_factor(v: &[u8], u: &Units) -> f64 {
    let name = v
        .iter()
        .position(|c| *c == b'(')
        .map_or_else(String::new, |i| upper(&v[..i]));
    match name.as_str() {
        "IFCLENGTHMEASURE" | "IFCPOSITIVELENGTHMEASURE" | "IFCNONNEGATIVELENGTHMEASURE" => {
            1.0 / u.length
        }
        "IFCAREAMEASURE" => 1.0 / u.area,
        "IFCVOLUMEMEASURE" => 1.0 / u.volume,
        "IFCPLANEANGLEMEASURE" | "IFCPOSITIVEPLANEANGLEMEASURE" => 1.0 / u.angle,
        _ => 1.0,
    }
}

/// Storey GlobalId of an element in the export.
fn storey_in_export(ex: &Export, id: u32) -> Option<String> {
    for i in ex.by_ref.get(&id)? {
        let (ty, args) = &ex.rels[*i];
        if ty != CONTAINED {
            continue;
        }
        let a = split_top(args);
        let s = single_ref(a.get(5)?)?;
        if s != id {
            return ex.guid_at(s).map(str::to_string);
        }
    }
    None
}

/// Moves an edited element to the storey the model says, when that differs from the file.
fn follow_storey(
    tx: &mut Tx<'_>,
    file_id: u32,
    ex_id: u32,
    guid: &str,
    o: &mut Out,
) -> Result<(), PatchError> {
    let Some(want) = storey_in_export(tx.ex, ex_id) else {
        return Ok(());
    };
    let ix = tx.ix;
    let Some(rel) = ix.rels_of(file_id, CONTAINED).next() else {
        return Ok(());
    };
    let now = o.current_args(rel.id, &rel.args);
    let a = split_top(&now);
    let Some(cur) = a.get(5).and_then(|v| single_ref(v)) else {
        return Err(doubt(DOUBT_STRUCTURE, format!("relation #{}", rel.id)));
    };
    // a storey this patch appends is in the map under its export id
    let target = ix.rooted.get(&want).copied().or_else(|| {
        let e = tx.ex.rooted.get(&want)?;
        tx.map.get(e).copied()
    });
    let Some(target) = target else {
        return Err(doubt(
            DOUBT_EDIT,
            format!("storey {want} is not in the source"),
        ));
    };
    if cur == target {
        return Ok(());
    }
    let items: Vec<u32> = a
        .get(4)
        .and_then(|v| list_items(v))
        .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
        .unwrap_or_default();
    if !items.contains(&file_id) {
        return Err(doubt(DOUBT_STRUCTURE, format!("relation #{}", rel.id)));
    }
    let left: Vec<u32> = items.into_iter().filter(|t| *t != file_id).collect();
    if left.is_empty() {
        o.remove.insert(rel.id);
        o.replace.remove(&rel.id);
    } else {
        let mut args = owned(&a);
        args[4] = ref_list(&left);
        o.swap(rel.id, CONTAINED, &join_args(&args));
    }
    tx.new_rel(
        CONTAINED,
        guid,
        [ref_list(&[file_id]), format!("#{target}").into_bytes()],
        o,
    )?;
    Ok(())
}

/// Openings of edited hosts and fillers of edited openings are placed from their host in the file,
/// so they are rewritten with it even when their own rows did not change.
fn with_dependents(ix: &Index, edits: &mut Vec<GeomEdit>) {
    let guids = ix.guid_by_id();
    let mut known: HashSet<String> = edits.iter().map(|e| e.guid.clone()).collect();
    let mut i = 0;
    while i < edits.len() {
        if let Some(&id) = ix.rooted.get(&edits[i].guid) {
            let mut next = ix.second_of(id, VOIDS);
            next.extend(ix.second_of(id, FILLS));
            for n in next {
                if let Some(g) = guids.get(&n) {
                    if known.insert((*g).to_string()) {
                        edits.push(GeomEdit {
                            guid: (*g).to_string(),
                            kind: "opening".into(),
                            id: -1,
                            renamed: false,
                            place_only: false,
                        });
                    }
                }
            }
        }
        i += 1;
    }
}

/// Railings along an edited stair follow its edge, so they are rewritten with it.
fn with_railings(
    model: &Model,
    ix: &Index,
    ex: &Export,
    edits: &mut Vec<GeomEdit>,
) -> Result<(), PatchError> {
    let stairs: HashSet<&str> = edits
        .iter()
        .filter(|e| e.kind == "stair")
        .map(|e| e.guid.as_str())
        .collect();
    if stairs.is_empty() {
        return Ok(());
    }
    let mut ids: Vec<i64> = ex
        .rows
        .iter()
        .filter(|((t, _), g)| t == "stair" && stairs.contains(g.as_str()))
        .map(|((_, id), _)| *id)
        .collect();
    ids.sort_unstable();
    let mut known: HashSet<String> = edits.iter().map(|e| e.guid.clone()).collect();
    let mut add = vec![];
    let mut st = model
        .sql()
        .prepare("SELECT id FROM railing WHERE stair=?1 ORDER BY id")
        .map_err(failed)?;
    for id in ids {
        let rails: Vec<i64> = st
            .query_map([id], |r| r.get(0))
            .map_err(failed)?
            .collect::<Result<_, _>>()
            .map_err(failed)?;
        for r in rails {
            let Some(g) = ex.rows.get(&("railing".to_string(), r)) else {
                continue;
            };
            if ix.rooted.contains_key(g) && known.insert(g.clone()) {
                add.push(GeomEdit {
                    guid: g.clone(),
                    kind: "railing".into(),
                    id: r,
                    renamed: false,
                    place_only: false,
                });
            }
        }
    }
    edits.extend(add);
    Ok(())
}

/// One product line of the file with new placement and shape from the export.
fn edit_one(
    tx: &mut Tx<'_>,
    e: &GeomEdit,
    file_id: u32,
    ex_id: u32,
    file_ty: &str,
    file_args: &[u8],
    o: &mut Out,
) -> Result<bool, PatchError> {
    let ex = tx.ex;
    let Some(ent) = ex.ent(ex_id) else {
        return Err(doubt(DOUBT_STRUCTURE, format!("export has no #{ex_id}")));
    };
    let a = split_top(ent.args);
    if a.len() < 7 {
        return Err(doubt(DOUBT_STRUCTURE, format!("export line #{ex_id}")));
    }
    // a place-only edit takes argument 6 (ObjectPlacement) alone; argument 7 stays the file's
    let take: &[usize] = if e.place_only { &[5] } else { &[5, 6] };
    if e.place_only && tx.ex.dup.contains(&e.guid) {
        return Err(doubt(
            DOUBT_EDIT,
            format!("{} {} is on more than one exported line", e.kind, e.guid),
        ));
    }
    let mut seen = HashSet::new();
    for v in take.iter().map(|i| a[*i]) {
        if let Some(r) = single_ref(v) {
            if tx.check(r, false, &mut seen).is_err() {
                o.deny(&e.kind, e.id, "", REASON_SCHEMA);
                return Ok(false);
            }
        }
    }
    let fa = split_top(file_args);
    if fa.len() < 7 {
        return Err(doubt(DOUBT_STRUCTURE, format!("product line #{file_id}")));
    }
    let frame = if e.place_only {
        let who = format!("{} {}", e.kind, e.guid);
        let Some(want) = ex.frame(ex_id, &who).map_err(|r| doubt(DOUBT_EDIT, r))? else {
            return Err(doubt(
                DOUBT_EDIT,
                format!("{} {}: the export has no frame", e.kind, e.guid),
            ));
        };
        let Some(lines) = tx.frames.get(&file_id) else {
            return Err(doubt(
                DOUBT_EDIT,
                format!("{} {}: the file has no frame", e.kind, e.guid),
            ));
        };
        let mut swaps = vec![];
        for l in lines {
            let mut pa = owned(&split_top(&l.args));
            pa[2] = want[&l.name].clone();
            swaps.push((l.id, l.ty.clone(), join_args(&pa)));
        }
        swaps
    } else {
        vec![]
    };
    let mut args = owned(&fa);
    for &i in take {
        args[i] = match single_ref(a[i]) {
            Some(r) => format!("#{}", tx.emit(r, o)?).into_bytes(),
            None => a[i].to_vec(),
        };
    }
    if e.place_only {
        o.swap(file_id, file_ty, &join_args(&args));
        for (id, ty, line) in frame {
            o.swap(id, &ty, &line);
        }
        follow_storey(tx, file_id, ex_id, &e.guid, o)?;
        // a move changes no length, area or volume: the element's quantities still hold
        o.edited += 1;
        o.count(&e.kind, &e.guid);
        return Ok(true);
    }
    if e.renamed {
        args[2] = a[2].to_vec();
    }
    // Tag and PredefinedType follow the shape of a stair or ramp
    if matches!(file_ty, "IFCSTAIR" | "IFCRAMP") {
        for i in 7..fa.len().min(a.len()) {
            args[i] = a[i].to_vec();
        }
    }
    if matches!(file_ty, "IFCDOOR" | "IFCWINDOW") && fa.len() > 9 && a.len() > 9 {
        let k = 1.0 / tx.u.length;
        args[8] = scale_numbers(a[8], k);
        args[9] = scale_numbers(a[9], k);
    }
    o.swap(file_id, file_ty, &join_args(&args));
    follow_storey(tx, file_id, ex_id, &e.guid, o)?;
    o.stale.insert(file_id);
    o.edited += 1;
    o.count(&e.kind, &e.guid);
    Ok(true)
}

/// Reference items of argument `k` of a relation.
fn items_at(a: &[&[u8]], k: usize) -> Vec<u32> {
    a.get(k)
        .and_then(|v| list_items(v))
        .map(|l| l.iter().filter_map(|t| single_ref(t)).collect())
        .unwrap_or_default()
}

/// Parts a new element brings: the products of its own aggregations. Nesting, or the element
/// being itself a part, is refused.
fn parts_of(ex: &Export, ex_id: u32, guid: &str) -> Result<Vec<u32>, PatchError> {
    let mut out = vec![];
    for &i in ex.by_ref.get(&ex_id).into_iter().flatten() {
        let (ty, args) = &ex.rels[i];
        if ty == NESTS {
            return Err(doubt(
                DOUBT_NESTED,
                format!("new element {guid} has nested parts"),
            ));
        }
        if ty != AGGREGATES {
            continue;
        }
        if !first_end_is(args, ex_id) {
            return Err(doubt(
                DOUBT_EDIT,
                format!("new element {guid} is a part of another"),
            ));
        }
        out.extend(items_at(&split_top(args), 5));
    }
    Ok(out)
}

/// The relations an appended element needs, each holding only that element. A part (`whole` is
/// the export id of its whole) is not contained on its own: the whole's aggregation holds it.
fn add_rels(
    tx: &mut Tx<'_>,
    ex_id: u32,
    new_id: u32,
    guid: &str,
    (table, row): (&str, i64),
    only: Option<&[String]>,
    whole: Option<u32>,
    o: &mut Out,
) -> Result<(), PatchError> {
    let ex = tx.ex;
    let Some(list) = ex.by_ref.get(&ex_id) else {
        return Ok(());
    };
    // a filler (door, window) has no base of its own: its pending links are kept on its opening
    let host = (table == "opening"
        && ex.rows.get(&("opening_fill".to_string(), row)).map(String::as_str) == Some(guid))
    .then(|| ex.rows.get(&("opening".to_string(), row)).cloned())
    .flatten();
    // a part's link is kept on its whole, which is what a later save tries again
    let whole_guid = whole.and_then(|w| ex.guid_at(w)).map(str::to_string);
    let deny = |o: &mut Out, ty: &str, why: &str| match (&host, &whole_guid) {
        (Some(h), _) => o.deny_link("opening", row, h, &format!("{ty}{FILL_AT}{guid}"), why),
        (None, Some(w)) => o.deny_link(table, row, w, &format!("{ty}{PART_AT}{guid}"), why),
        (None, None) => o.deny_link(table, row, guid, ty, why),
    };
    for &i in list {
        let (ty, args) = &ex.rels[i];
        let a = split_top(args);
        let one = |k: usize| a.get(k).and_then(|v| single_ref(v));
        if only.is_some_and(|l| !l.iter().any(|x| x == ty)) {
            continue;
        }
        match ty.as_str() {
            CONTAINED if whole.is_some() => {}
            CONTAINED | DEFINES | MATERIAL => {
                let Some(end) = one(5) else { continue };
                if ty == MATERIAL || ty == DEFINES {
                    let mut seen = HashSet::new();
                    let bad = if super::links_blocked() {
                        Some("a blocked link".to_string())
                    } else {
                        tx.check(end, true, &mut seen).err()
                    };
                    if let Some(t) = bad {
                        deny(o, ty, &t);
                        continue;
                    }
                }
                let n = tx.emit(end, o)?;
                let seed = format!("{guid}|{}", ex.guid_at(end).unwrap_or_default());
                tx.new_rel(
                    ty,
                    &seed,
                    [ref_list(&[new_id]), format!("#{n}").into_bytes()],
                    o,
                )?;
            }
            VOIDS | FILLS => {
                if one(5) != Some(ex_id) {
                    continue;
                }
                let Some(end) = one(4) else { continue };
                if ty == FILLS && super::links_blocked() {
                    // kept on the opening's base, which a filler has none of
                    let host = ex.guid_at(end).unwrap_or_default();
                    let rel = format!("{FILLS}{FILL_AT}{guid}");
                    o.deny_link("opening", row, host, &rel, "a blocked link");
                    continue;
                }
                let n = tx.emit(end, o)?;
                if ty == VOIDS {
                    o.stale.insert(n);
                }
                tx.new_rel(
                    ty,
                    guid,
                    [
                        format!("#{n}").into_bytes(),
                        format!("#{new_id}").into_bytes(),
                    ],
                    o,
                )?;
            }
            NESTS => {
                return Err(doubt(
                    DOUBT_NESTED,
                    format!("new element {guid} has nested parts"),
                ));
            }
            AGGREGATES => {
                // the whole writes the one aggregation of its parts
                if whole.is_some() && one(4) == whole {
                    continue;
                }
                if one(4) != Some(ex_id) {
                    return Err(doubt(
                        DOUBT_EDIT,
                        format!("new element {guid} is a part of another"),
                    ));
                }
                let mut items = vec![];
                for p in items_at(&a, 5) {
                    let Some(&n) = tx.map.get(&p) else {
                        return Err(doubt(
                            DOUBT_EDIT,
                            format!("part #{p} of new element {guid} not written"),
                        ));
                    };
                    items.push(n);
                }
                tx.new_rel(
                    ty,
                    &format!("{guid}|parts"),
                    [format!("#{new_id}").into_bytes(), ref_list(&items)],
                    o,
                )?;
            }
            other => o.warnings.push(format!("{guid}: {other} not written")),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    model: &Model,
    ix: &Index,
    src: &Path,
    out: &Path,
    u: &Units,
    work: &mut Work,
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let schema = ix
        .schema
        .ok_or_else(|| doubt(super::DOUBT_SCHEMA, "unknown schema"))?;
    let temp = TempFile({
        let mut name = out.as_os_str().to_os_string();
        name.push(".export");
        PathBuf::from(name)
    });
    let rows = export_rows(model, &temp.0, "patch", ctl.cloned()).map_err(failed)?;
    let ex = Export::read(&temp.0, rows)?;
    drop(temp);

    // rows new to the model: elements not yet in the file get appended; one that our own earlier
    // write over the source put there is an ordinary edit
    let mut fresh: Vec<(String, String, i64, String)> = vec![];
    let mut levels: Vec<(i64, String)> = vec![];
    for (table, id) in std::mem::take(&mut work.added) {
        if table == "level" {
            match ex.rows.get(&(table.clone(), id)) {
                None => o.deny(&table, id, "", REASON_EXPORT),
                // our own earlier write put it there: only the link is missing
                Some(g) if ix.rooted.contains_key(g) => {
                    o.linked.push((table, id, g.clone()))
                }
                Some(_) if levels.iter().any(|l| l.0 == id) => {}
                Some(g) => levels.push((id, g.clone())),
            }
            continue;
        }
        if NO_ADD.contains(&table.as_str()) {
            return Err(doubt(
                DOUBT_EDIT,
                format!("new {table}: structure of the file changes"),
            ));
        }
        let mut names = vec![table.clone()];
        if table == "opening" {
            names.push("opening_fill".to_string());
        }
        let mut any = false;
        for n in names {
            let Some(g) = ex.rows.get(&(n.clone(), id)) else {
                continue;
            };
            any = true;
            if ix.rooted.contains_key(g) {
                work.geom.push(GeomEdit {
                    guid: g.clone(),
                    kind: table.clone(),
                    id,
                    renamed: true,
                    place_only: false,
                });
                o.linked.push((n, id, g.clone()));
            } else {
                fresh.push((table.clone(), n, id, g.clone()));
            }
        }
        if !any {
            o.deny(&table, id, "", REASON_EXPORT);
        }
    }

    let mut edits = std::mem::take(&mut work.geom);
    with_dependents(ix, &mut edits);
    with_railings(model, ix, &ex, &mut edits)?;
    edits.retain(|e| {
        ix.rooted
            .get(&e.guid)
            .map_or(true, |i| !o.remove.contains(i))
    });
    let wanted: HashSet<u32> = edits
        .iter()
        .filter_map(|e| ix.rooted.get(&e.guid).copied())
        .collect();
    let lines = if wanted.is_empty() {
        HashMap::new()
    } else {
        probe(src, &wanted, ctl)?
    };

    let place: Vec<u32> = edits
        .iter()
        .filter(|e| e.place_only)
        .filter_map(|e| ix.rooted.get(&e.guid).copied())
        .collect();
    let frames = frame_lines(ix, src, &place, o, ctl)?;
    let mut tx = Tx {
        ix,
        ex: &ex,
        schema,
        u,
        map: HashMap::new(),
        frames,
    };
    if !levels.is_empty() {
        add_storeys(&mut tx, src, &levels, o, ctl)?;
    }
    for e in &edits {
        let Some(&file_id) = ix.rooted.get(&e.guid) else {
            return Err(doubt(
                DOUBT_EDIT,
                format!("{} is linked but not in the file", e.guid),
            ));
        };
        let Some(&ex_id) = ex.rooted.get(&e.guid) else {
            return Err(doubt(
                DOUBT_EDIT,
                format!("{} {} is not exported", e.kind, e.guid),
            ));
        };
        let Some(p) = lines.get(&file_id) else {
            return Err(doubt(DOUBT_STRUCTURE, format!("#{file_id} not read")));
        };
        let whole = parts::WHOLE_KINDS.contains(&e.kind.as_str());
        if !whole
            && ix
                .rels_of(file_id, AGGREGATES)
                .any(|r| first_end_is(&r.args, file_id))
        {
            return Err(doubt(DOUBT_EDIT, format!("{} has parts", e.guid)));
        }
        let done = if whole {
            parts::edit_whole(&mut tx, src, e, file_id, ex_id, &p.ty, &p.args, o, ctl)?
        } else {
            edit_one(&mut tx, e, file_id, ex_id, &p.ty, &p.args, o)?
        };
        if let Some(set) = parts::data_set(&e.kind).filter(|_| done && !e.place_only) {
            parts::sync_set(&tx, src, file_id, ex_id, set, o, ctl)?;
        }
    }

    // ids first, so elements that point at each other (opening and filler) resolve in any order
    let mut planned: Vec<(String, String, u32, u32, i64)> = vec![];
    // parts of new elements: GlobalId, export id, file id, export id of the whole, its table and row
    let mut parts: Vec<(String, u32, u32, u32, String, i64)> = vec![];
    for (table, link, row, g) in fresh {
        let Some(&ex_id) = ex.rooted.get(&g) else {
            o.deny(&table, row, "", REASON_EXPORT);
            continue;
        };
        let kids = parts_of(&ex, ex_id, &g)?;
        let mut seen = HashSet::new();
        let fits = tx.check(ex_id, true, &mut seen).is_ok()
            && kids.iter().all(|k| tx.check(*k, true, &mut seen).is_ok());
        if !fits {
            o.deny(&table, row, "", REASON_SCHEMA);
            continue;
        }
        let mut named = Vec::with_capacity(kids.len());
        for k in kids {
            let Some(pg) = ex.guid_at(k) else {
                return Err(doubt(DOUBT_STRUCTURE, format!("export part #{k}")));
            };
            if ix.rooted.contains_key(pg) || ex.dup.contains(pg) || o.new_guids.contains(pg) {
                return Err(doubt(
                    DOUBT_EDIT,
                    format!("part {pg} of new element {g} is already in the file"),
                ));
            }
            named.push((pg.to_string(), k));
        }
        let n = o.alloc();
        tx.map.insert(ex_id, n);
        o.new_guids.insert(g.clone());
        o.linked.push((link, row, g.clone()));
        for (pg, k) in named {
            let pn = o.alloc();
            tx.map.insert(k, pn);
            o.new_guids.insert(pg.clone());
            parts.push((pg, k, pn, ex_id, table.clone(), row));
        }
        planned.push((table, g, ex_id, n, row));
    }
    for (table, g, ex_id, n, _) in &planned {
        let ent = ex
            .ent(*ex_id)
            .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("export has no #{ex_id}")))?;
        let ty = upper(ent.ty);
        let args = tx.root_args(&ty, ent.args, o)?;
        o.put(*n, &ty, &args);
        o.added += 1;
        o.count(table, g);
    }
    for (g, ex_id, n, ..) in &parts {
        let ent = ex
            .ent(*ex_id)
            .ok_or_else(|| doubt(DOUBT_STRUCTURE, format!("export has no #{ex_id}")))?;
        let ty = upper(ent.ty);
        let args = tx.root_args(&ty, ent.args, o)?;
        o.put(*n, &ty, &args);
        o.added += 1;
        o.touched.push(g.clone());
    }
    for (table, g, ex_id, n, row) in &planned {
        add_rels(&mut tx, *ex_id, *n, g, (table, *row), None, None, o)?;
    }
    for (g, ex_id, n, whole, table, row) in &parts {
        add_rels(&mut tx, *ex_id, *n, g, (table, *row), None, Some(*whole), o)?;
    }

    // links an earlier save could not write: the element is in the file already and stays as it is
    for r in std::mem::take(&mut work.relink) {
        let Some(&file_id) = ix.rooted.get(&r.guid) else {
            return Err(doubt(
                DOUBT_EDIT,
                format!("{} has a pending link but is not in the file", r.guid),
            ));
        };
        if o.remove.contains(&file_id) {
            continue;
        }
        o.retried.push(r.guid.clone());
        let (fills, rest): (Vec<String>, Vec<String>) = r
            .rels
            .iter()
            .cloned()
            .partition(|x| x.contains(FILL_AT));
        let (part_rels, plain): (Vec<String>, Vec<String>) =
            rest.into_iter().partition(|x| x.contains(PART_AT));
        for rel in &part_rels {
            let Some((ty, part)) = rel.split_once(PART_AT) else { continue };
            match (ix.rooted.get(part), ex.rooted.get(part), ex.rooted.get(&r.guid)) {
                (Some(p), ..) if o.remove.contains(p) => {}
                (Some(&pid), Some(&xid), Some(&whole)) => add_rels(
                    &mut tx,
                    xid,
                    pid,
                    part,
                    (&r.table, r.id),
                    Some(&[ty.to_string()][..]),
                    Some(whole),
                    o,
                )?,
                _ => o.deny_link(&r.table, r.id, &r.guid, rel, "the part is not in the file"),
            }
        }
        for rel in &fills {
            let Some((ty, door)) = rel.split_once(FILL_AT) else { continue };
            if ty != FILLS {
                // a material or property link of the filler, tried again on the filler itself
                match (ix.rooted.get(door), ex.rooted.get(door)) {
                    (Some(d), _) if o.remove.contains(d) => {}
                    (Some(&door_id), Some(&ex_door)) => add_rels(
                        &mut tx,
                        ex_door,
                        door_id,
                        door,
                        (&r.table, r.id),
                        Some(&[ty.to_string()][..]),
                        None,
                        o,
                    )?,
                    _ => o.deny_link(&r.table, r.id, &r.guid, rel, "the filler is not in the file"),
                }
                continue;
            }
            let Some(&door_id) = ix.rooted.get(door) else {
                o.deny_link(&r.table, r.id, &r.guid, rel, "the filler is not in the file");
                continue;
            };
            if o.remove.contains(&door_id) {
                continue;
            }
            if ix.rels_of(door_id, FILLS).next().is_some() {
                continue;
            }
            if super::links_blocked() {
                o.deny_link(&r.table, r.id, &r.guid, rel, "a blocked link");
                continue;
            }
            tx.new_rel(
                FILLS,
                door,
                [
                    format!("#{file_id}").into_bytes(),
                    format!("#{door_id}").into_bytes(),
                ],
                o,
            )?;
        }
        if plain.is_empty() {
            o.count(&r.table, &r.guid);
            continue;
        }
        match ex.rooted.get(&r.guid) {
            Some(&ex_id) => add_rels(
                &mut tx,
                ex_id,
                file_id,
                &r.guid,
                (&r.table, r.id),
                Some(&plain),
                None,
                o,
            )?,
            None => {
                for rel in &plain {
                    o.deny_link(&r.table, r.id, &r.guid, rel, "the element is not exported");
                }
            }
        }
        o.count(&r.table, &r.guid);
    }
    Ok(())
}

/// New levels become storeys of the file's one building: placed relative to the building, at the
/// level's elevation, and aggregated under it. Elements on them then take the ordinary containment.
fn add_storeys(
    tx: &mut Tx<'_>,
    src: &Path,
    levels: &[(i64, String)],
    o: &mut Out,
    ctl: Option<&Arc<Progress>>,
) -> Result<(), PatchError> {
    let ix = tx.ix;
    let mut buildings: Vec<u32> = ix
        .rooted
        .values()
        .copied()
        .filter(|id| ix.type_name(*id) == Some("IFCBUILDING") && !o.remove.contains(id))
        .collect();
    buildings.sort_unstable();
    let &[building] = &buildings[..] else {
        return Err(doubt(
            DOUBT_BUILDING,
            format!("new level: {} buildings in the file", buildings.len()),
        ));
    };
    let read = probe(src, &HashSet::from([building]), ctl)?;
    let Some(b) = read.get(&building).filter(|b| b.ty == "IFCBUILDING") else {
        return Err(doubt(DOUBT_STRUCTURE, format!("#{building} not read")));
    };
    let place = match split_top(&b.args).get(5) {
        Some(v) if super::lines::trim(v) == b"$" => b"$".to_vec(),
        Some(v) => match single_ref(v) {
            Some(r) => format!("#{r}").into_bytes(),
            None => return Err(doubt(DOUBT_STRUCTURE, format!("building #{building}"))),
        },
        None => return Err(doubt(DOUBT_STRUCTURE, format!("building #{building}"))),
    };

    // the one aggregation of the building that holds storeys and nothing else is extended;
    // any other layout gets a relation of its own
    let mut own: Vec<(u32, Vec<u8>)> = vec![];
    for r in ix.rels_of(building, AGGREGATES) {
        if o.remove.contains(&r.id) {
            continue;
        }
        let now = o.current_args(r.id, &r.args);
        if first_end_is(&now, building) {
            own.push((r.id, now));
        }
    }
    let items_of = |args: &[u8]| -> Option<Vec<u32>> {
        let l = split_top(args).get(5).and_then(|v| list_items(v))?;
        l.iter().map(|t| single_ref(t)).collect()
    };
    let extend = match &own[..] {
        [(id, args)] => items_of(args)
            .filter(|l| !l.is_empty() && l.iter().all(|t| ix.type_name(*t) == Some(STOREY)))
            .map(|l| (*id, args.clone(), l)),
        _ => None,
    };

    let ex = tx.ex;
    let kl = 1.0 / tx.u.length;
    let mut added: Vec<u32> = vec![];
    for (row, g) in levels {
        let Some(&ex_id) = ex.rooted.get(g) else {
            o.deny("level", *row, "", REASON_EXPORT);
            continue;
        };
        let Some(e) = ex.ent(ex_id) else {
            return Err(doubt(DOUBT_STRUCTURE, format!("export has no #{ex_id}")));
        };
        let a = split_top(e.args);
        if upper(e.ty) != STOREY || a.len() < 10 {
            return Err(doubt(DOUBT_STRUCTURE, format!("export line #{ex_id}")));
        }
        let elev = scale_numbers(a[9], kl);
        let mut pt = b"((0.,0.,".to_vec();
        pt.extend_from_slice(&elev);
        pt.extend_from_slice(b"))");
        let p = o.add("IFCCARTESIANPOINT", &pt);
        let ax = o.add("IFCAXIS2PLACEMENT3D", format!("#{p},$,$").as_bytes());
        let mut lp = place.clone();
        lp.extend_from_slice(format!(",#{ax}").as_bytes());
        let pl = o.add("IFCLOCALPLACEMENT", &lp);
        let n = o.alloc();
        tx.map.insert(ex_id, n);
        o.new_guids.insert(g.clone());
        let mut args = vec![
            a[0].to_vec(),
            b"$".to_vec(),
            a[2].to_vec(),
            b"$".to_vec(),
            b"$".to_vec(),
            format!("#{pl}").into_bytes(),
            b"$".to_vec(),
            b"$".to_vec(),
            b".ELEMENT.".to_vec(),
            elev,
        ];
        if tx.schema == IfcSchema::Ifc2x3 {
            tx.fill_owner(&mut args)?;
        }
        o.put(n, STOREY, &join_args(&args));
        o.added += 1;
        o.count("level", g);
        o.linked.push(("level".into(), *row, g.clone()));
        added.push(n);
    }
    if added.is_empty() {
        return Ok(());
    }
    match extend {
        Some((id, args, mut items)) => {
            items.extend_from_slice(&added);
            let mut a = owned(&split_top(&args));
            a[5] = ref_list(&items);
            o.swap(id, AGGREGATES, &join_args(&a));
        }
        None => {
            let seed = format!("{building}|{}", levels[0].1);
            tx.new_rel(
                AGGREGATES,
                &seed,
                [format!("#{building}").into_bytes(), ref_list(&added)],
                o,
            )?;
        }
    }
    Ok(())
}

fn first_end_is(args: &[u8], id: u32) -> bool {
    split_top(args).get(4).and_then(|v| single_ref(v)) == Some(id)
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    const G: [&str; 6] = [
        "0aaaaaaaaaaaaaaaaaaaa1",
        "0aaaaaaaaaaaaaaaaaaaa2",
        "0aaaaaaaaaaaaaaaaaaaa3",
        "0aaaaaaaaaaaaaaaaaaaa4",
        "0aaaaaaaaaaaaaaaaaaaa5",
        "0aaaaaaaaaaaaaaaaaaaa6",
    ];

    fn export_with(sets: usize, tag: &str) -> Export {
        export_props(sets, tag, false)
    }

    fn export_props(sets: usize, tag: &str, dup: bool) -> Export {
        let mut d = String::from("ISO-10303-21;
HEADER;
ENDSEC;
DATA;
");
        let mut item = 30;
        for k in 0..sets {
            let (rel, set) = (20 + 2 * k, 21 + 2 * k);
            d += &format!(
                "#{rel}=IFCRELDEFINESBYPROPERTIES('{}',$,$,$,(#10),#{set});
",
                G[k]
            );
            d += &format!(
                "#{set}=IFCPROPERTYSET('{}',$,'{FRAME_SET}',$,(#{item},#{}{}));
",
                G[2 + k],
                item + 1,
                if dup { ",#90" } else { "" }
            );
            d += &format!("#{item}=IFCPROPERTYSINGLEVALUE('Local',$,IFCTEXT('a'),$);
");
            d += &format!("#{}=IFCPROPERTYSINGLEVALUE('World',$,IFCTEXT('b'),$);
", item + 1);
            item += 2;
        }
        if dup {
            d += "#90=IFCPROPERTYSINGLEVALUE('Local',$,IFCTEXT('c'),$);
";
        }
        d += "ENDSEC;
END-ISO-10303-21;
";
        let p = std::env::temp_dir().join(format!("ddc-frame-{tag}-{}.ifc", std::process::id()));
        std::fs::write(&p, d).unwrap();
        let ex = Export::read(&p, HashMap::new());
        let _ = std::fs::remove_file(&p);
        ex.unwrap_or_else(|e| panic!("{e:?}"))
    }

    #[test]
    fn export_two_frame_sets_is_doubt() {
        let ex = export_with(2, "two");
        let r = ex.frame(10, "IfcWall g").unwrap_err();
        assert!(r.contains("shared or repeated"), "{r}");
    }

    #[test]
    fn export_dup_prop_is_doubt() {
        let ex = export_props(1, "dup", true);
        let r = ex.frame(10, "IfcWall g").unwrap_err();
        assert_eq!(r, bad_line("IfcWall g", "Local"));
    }

    #[test]
    fn export_dollar_value_is_doubt() {
        let ex = export_props(1, "dollar", false);
        // rebuild with an unset Local value
        let mut d = String::new();
        d += "ISO-10303-21;
HEADER;
ENDSEC;
DATA;
";
        d += &format!("#20=IFCRELDEFINESBYPROPERTIES('{}',$,$,$,(#10),#21);
", G[0]);
        d += &format!("#21=IFCPROPERTYSET('{}',$,'{FRAME_SET}',$,(#30,#31));
", G[2]);
        d += "#30=IFCPROPERTYSINGLEVALUE('Local',$,$,$);
";
        d += "#31=IFCPROPERTYSINGLEVALUE('World',$,IFCTEXT('b'),$);
";
        d += "ENDSEC;
END-ISO-10303-21;
";
        let p = std::env::temp_dir().join(format!("ddc-frame-dollar-{}.ifc", std::process::id()));
        std::fs::write(&p, d).unwrap();
        let ex2 = Export::read(&p, HashMap::new());
        let _ = std::fs::remove_file(&p);
        let ex2 = ex2.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            ex2.frame(10, "IfcWall g").unwrap_err(),
            bad_line("IfcWall g", "Local")
        );
        assert!(ex.frame(10, "IfcWall g").unwrap().is_some());
    }

    #[test]
    fn file_dollar_value_is_doubt() {
        // the file side uses the same predicate on argument 3
        assert!(unset(b"$") && unset(b" $ ") && unset(b""));
        assert!(!unset(b"IFCTEXT('a')"));
    }

    /// File side: the frame lines of a file whose Local value is `$` are a doubt.
    fn file_frame(local: &str, tag: &str) -> Result<HashMap<u32, Vec<FrameLine>>, PatchError> {
        let d = format!(
            "ISO-10303-21;
HEADER;
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
             #10=IFCWALL('{}',$,$,$,$,$,$,$,$);
             #20=IFCRELDEFINESBYPROPERTIES('{}',$,$,$,(#10),#21);
             #21=IFCPROPERTYSET('{}',$,'{FRAME_SET}',$,(#30,#31));
             #30=IFCPROPERTYSINGLEVALUE('Local',$,{local},$);
             #31=IFCPROPERTYSINGLEVALUE('World',$,IFCTEXT('b'),$);
             ENDSEC;
END-ISO-10303-21;
",
            G[0], G[1], G[2]
        );
        let p = std::env::temp_dir().join(format!("ddc-frame-file-{tag}-{}.ifc", std::process::id()));
        std::fs::write(&p, d).unwrap();
        let ix = Index::scan(&p, None).unwrap();
        let o = Out::new(&ix);
        let r = frame_lines(&ix, &p, &[10], &o, None);
        let _ = std::fs::remove_file(&p);
        r
    }

    #[test]
    fn file_frame_lines_dollar_value_is_doubt() {
        let ok = file_frame("IFCTEXT('a')", "ok").unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(ok[&10].len(), FRAME_PROPS.len());
        match file_frame("$", "dollar").unwrap_err() {
            PatchError::Doubt { key, detail } => {
                assert_eq!(key, DOUBT_EDIT);
                assert_eq!(detail, bad_line("#10", "Local"));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn export_one_frame_set_patches() {
        let ex = export_with(1, "one");
        let f = ex.frame(10, "IfcWall g").unwrap().expect("frame");
        assert_eq!(f.len(), FRAME_PROPS.len());
        assert!(f.contains_key("Local") && f.contains_key("World"));
    }

    #[test]
    fn file_and_export_side_same_rule() {
        // both sides build the doubt text from one function
        let ex = export_with(2, "same");
        assert_eq!(ex.frame(10, "IfcWall g").unwrap_err(), repeated_set("IfcWall g"));
    }
}
