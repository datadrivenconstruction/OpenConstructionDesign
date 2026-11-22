// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural validator for IFC files (our own code, no external schema engine): references,
//! required attributes of the families the exporter writes, spatial structure, openings,
//! types, materials, property sets, units, placement chains and GlobalIds. It checks what a
//! BIM package needs to open a file cleanly; it is not a full EXPRESS schema check.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::reader::{refs_of as refs, StepFile, Value};
use super::schema_spec::{self, IfcSchema};
use super::step::is_global_id;

/// One finding: the rule, the entity id (0 for the file) and what is wrong.
#[derive(Clone, Debug, PartialEq)]
pub struct IfcIssue {
    pub rule: &'static str,
    pub entity: u32,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct IfcReport {
    pub errors: Vec<IfcIssue>,
    pub entities: usize,
    pub schema: String,
}

impl IfcReport {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }

    /// Up to `n` findings, one per line.
    pub fn brief(&self, n: usize) -> String {
        self.errors
            .iter()
            .take(n)
            .map(|e| format!("[{}] #{}: {}", e.rule, e.entity, e.message))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The rules, with a one-line meaning each.
pub const RULES: &[(&str, &str)] = &[
    ("step-syntax", "the file parses as STEP, ids are unique"),
    ("ref-missing", "every referenced entity exists"),
    ("schema-known", "FILE_SCHEMA is IFC2X3, IFC4 or IFC4X3_ADD2"),
    (
        "arity",
        "known classes have the attribute count of the file schema",
    ),
    (
        "entity-known",
        "every entity is defined in the file schema and listed in the attribute table",
    ),
    (
        "attr-type",
        "attribute values have the declared kind; $ only in OPTIONAL attributes",
    ),
    (
        "guid-format",
        "rooted entities have a 22-character GlobalId",
    ),
    ("guid-unique", "GlobalIds are unique in the file"),
    ("project-single", "exactly one IfcProject"),
    (
        "project-context",
        "project has a representation context and a unit assignment",
    ),
    (
        "unit-length",
        "length unit is an SI metre (optionally prefixed)",
    ),
    (
        "spatial-chain",
        "project > site > building > storeys, one parent each",
    ),
    ("storey-elevation", "every storey has a numeric elevation"),
    (
        "storey-present",
        "a file with elements has at least one storey",
    ),
    (
        "element-contained",
        "every element is in exactly one container or is a part",
    ),
    (
        "container-class",
        "elements are contained in a storey (pads and terrain: site)",
    ),
    (
        "space-placed",
        "a space has one parent, a name and a long name or number",
    ),
    (
        "space-qto",
        "a space with geometry has Qto_SpaceBaseQuantities with a floor area",
    ),
    ("opening-voids-one", "an opening voids exactly one element"),
    ("opening-host", "the voided element is a building element"),
    ("opening-filled-once", "an opening is filled at most once"),
    ("filler-once", "an element fills at most one opening"),
    (
        "filler-class",
        "a filling element is an element, not an opening or a space",
    ),
    ("type-single", "an element has at most one type"),
    ("type-class", "the relating type is a type or style class"),
    (
        "placement",
        "products are placed and the placement chain ends",
    ),
    (
        "representation",
        "walls, slabs, doors, windows and openings have a shape",
    ),
    (
        "material-single",
        "an element has at most one material association",
    ),
    (
        "layer-set",
        "layer set usages refer to layer sets with positive layers",
    ),
    ("wall-layers", "walls carry a material layer set usage"),
    (
        "pset-nonempty",
        "property sets have properties, one set per name and element",
    ),
    ("quantity-valid", "quantities are finite and not negative"),
    (
        "rel-nonempty",
        "relationships have a relating object and related objects",
    ),
    ("aggregate-acyclic", "aggregation has no cycles"),
    (
        "mapconv",
        "map conversion refers to a context and a named projected CRS",
    ),
    ("storey-unique-name", "storey names are unique"),
    (
        "taper-profiles",
        "a tapered extrusion ends in a profile derived from its start profile (or one of \
         the same parameterized class) and has a positive depth",
    ),
    (
        "face-index",
        "indexed polygonal faces have at least three points, all in the point list",
    ),
];

/// Classes that are IfcElement (or part of the building) and need a container.
const ELEMENTS: &[&str] = &[
    "IFCWALL",
    "IFCWALLSTANDARDCASE",
    "IFCSLAB",
    "IFCROOF",
    "IFCSTAIR",
    "IFCRAMP",
    "IFCCOLUMN",
    "IFCBEAM",
    "IFCMEMBER",
    "IFCPLATE",
    "IFCDOOR",
    "IFCWINDOW",
    "IFCCURTAINWALL",
    "IFCCOVERING",
    "IFCRAILING",
    "IFCFOOTING",
    "IFCSHADINGDEVICE",
    "IFCMECHANICALFASTENER",
    "IFCBUILDINGELEMENTPROXY",
    "IFCVIRTUALELEMENT",
    "IFCGEOGRAPHICELEMENT",
    "IFCREINFORCINGBAR",
    "IFCELEMENTASSEMBLY",
    "IFCPIPESEGMENT",
    "IFCPIPEFITTING",
    "IFCDUCTSEGMENT",
    "IFCDUCTFITTING",
    "IFCCABLECARRIERSEGMENT",
    "IFCCABLECARRIERFITTING",
    "IFCCABLESEGMENT",
];

/// Classes that may sit directly on the site.
const SITE_OK: &[&str] = &["IFCGEOGRAPHICELEMENT", "IFCSLAB"];

const SPATIAL: &[&str] = &["IFCSITE", "IFCBUILDING", "IFCBUILDINGSTOREY"];

/// Classes whose shape is required.
const NEED_SHAPE: &[&str] = &[
    "IFCWALL",
    "IFCSLAB",
    "IFCDOOR",
    "IFCWINDOW",
    "IFCOPENINGELEMENT",
];

fn is_element(c: &str) -> bool {
    ELEMENTS.contains(&c)
}

fn is_type_class(c: &str) -> bool {
    c.ends_with("TYPE") || c == "IFCDOORSTYLE" || c == "IFCWINDOWSTYLE"
}

struct Chk<'a> {
    f: &'a StepFile,
    by: BTreeMap<&'a str, Vec<u32>>,
    out: Vec<IfcIssue>,
    /// The version named by FILE_SCHEMA, when it is one the product knows.
    schema: Option<IfcSchema>,
}

impl<'a> Chk<'a> {
    fn err(&mut self, rule: &'static str, id: u32, message: String) {
        self.out.push(IfcIssue {
            rule,
            entity: id,
            message,
        });
    }

    fn args(&self, id: u32) -> &'a [Value] {
        let f: &'a StepFile = self.f;
        f.args(id)
    }

    fn ty(&self, id: u32) -> &'a str {
        let f: &'a StepFile = self.f;
        f.type_of(id).unwrap_or("")
    }

    fn ids(&self, class: &str) -> Vec<u32> {
        self.by.get(class).cloned().unwrap_or_default()
    }

    fn real_at(&self, id: u32, i: usize) -> Option<f64> {
        self.args(id).get(i).and_then(Value::as_f64)
    }

    fn str_at(&self, id: u32, i: usize) -> Option<&'a str> {
        let f: &'a StepFile = self.f;
        f.str_at(id, i)
    }

    /// Relations of a class as (id, relating, related list).
    fn rels(
        &self,
        class: &str,
        relating: usize,
        related: usize,
    ) -> Vec<(u32, Option<u32>, Vec<u32>)> {
        self.ids(class)
            .into_iter()
            .map(|id| {
                let a = self.args(id);
                (
                    id,
                    a.get(relating).and_then(Value::as_ref),
                    refs(a.get(related)),
                )
            })
            .collect()
    }
}

/// Validates the bytes of an IFC (STEP) file.
pub fn validate_ifc(bytes: &[u8]) -> IfcReport {
    let mut rep = IfcReport::default();
    let f = match StepFile::parse_tolerant(bytes) {
        Ok(f) => f,
        Err(e) => {
            rep.errors.push(IfcIssue {
                rule: "step-syntax",
                entity: 0,
                message: e.to_string(),
            });
            return rep;
        }
    };
    rep.entities = f.len();
    rep.schema = f.schema.first().cloned().unwrap_or_default();
    let mut c = Chk {
        f: &f,
        by: f.by_class(),
        out: vec![],
        schema: IfcSchema::parse(&rep.schema),
    };
    for w in &f.warnings {
        let rule = if w.contains("duplicate") {
            "step-syntax"
        } else {
            "ref-missing"
        };
        c.err(rule, 0, w.clone());
    }
    if c.schema.is_none() {
        c.err("schema-known", 0, format!("schema '{}'", rep.schema));
    }
    arity(&mut c);
    guids(&mut c);
    project(&mut c);
    spatial(&mut c);
    openings(&mut c);
    relations(&mut c);
    placement(&mut c);
    materials(&mut c);
    props(&mut c);
    spaces(&mut c);
    mapconv(&mut c);
    advanced(&mut c);
    rep.errors.append(&mut c.out);
    rep
}

/// Rules of the advanced geometry the exporter writes (tapered extrusions, polygonal sets).
fn advanced(c: &mut Chk) {
    for id in c.ids("IFCEXTRUDEDAREASOLIDTAPERED") {
        let a = c.args(id);
        let (start, end) = (
            a.first().and_then(Value::as_ref),
            a.get(4).and_then(Value::as_ref),
        );
        let ok = match (start, end) {
            (Some(s), Some(e)) => {
                let (ts, te) = (c.ty(s), c.ty(e));
                let derived = te == "IFCDERIVEDPROFILEDEF"
                    && c.args(e).get(2).and_then(Value::as_ref) == Some(s);
                // IfcTaperedSweptAreaProfiles: same parameterized class also qualifies
                let param = ts == te
                    && !ts.starts_with("IFCARBITRARY")
                    && !matches!(ts, "IFCDERIVEDPROFILEDEF" | "IFCCOMPOSITEPROFILEDEF");
                derived || param
            }
            _ => false,
        };
        if !ok {
            c.err("taper-profiles", id, "end profile is not derived from the start".into());
        }
        if !c.real_at(id, 3).is_some_and(|d| d > 0.0) {
            c.err("taper-profiles", id, "depth is not positive".into());
        }
    }
    for id in c.ids("IFCPOLYGONALFACESET") {
        let a = c.args(id);
        let n = a
            .first()
            .and_then(Value::as_ref)
            .and_then(|p| c.args(p).first())
            .and_then(Value::as_list)
            .map_or(0, |l| l.len());
        // PnIndex re-maps the face indices: they then index it
        let pn = a.get(3).and_then(Value::as_list).map(|l| l.len());
        let top = pn.unwrap_or(n) as i64;
        let ring_ok = |v: &Value| {
            let r = v.as_list().unwrap_or_default();
            r.len() >= 3 && r.iter().all(|x| matches!(x, Value::Int(i) if *i >= 1 && *i <= top))
        };
        for fid in a.get(2).map(Value::refs).unwrap_or_default() {
            let fa = c.args(fid);
            let outer = fa.first().is_some_and(ring_ok);
            let inner = fa
                .get(1)
                .and_then(Value::as_list)
                .unwrap_or_default()
                .iter()
                .all(ring_ok);
            if !(outer && inner) {
                c.err("face-index", fid, "face has fewer than 3 points or a bad index".into());
            }
        }
    }
}

fn arity(c: &mut Chk) {
    let Some(schema) = c.schema else {
        return;
    };
    let (f, name) = (c.f, schema.header_name());
    let mut cache: HashMap<&str, Option<Option<Vec<schema_spec::Attr>>>> = HashMap::new();
    for (id, class) in f.iter() {
        let spec = cache
            .entry(class)
            .or_insert_with(|| schema_spec::attrs(class, schema.column()));
        let attrs = match spec {
            None => {
                let m = format!("{class} is not in the attribute table of {name}");
                c.err("entity-known", id, m);
                continue;
            }
            Some(None) => {
                let m = format!("{class} does not exist in {name}");
                c.err("entity-known", id, m);
                continue;
            }
            Some(Some(a)) => a,
        };
        let vals = c.args(id);
        if vals.len() != attrs.len() {
            let m = format!(
                "{class} has {} attributes, expected {}",
                vals.len(),
                attrs.len()
            );
            c.err("arity", id, m);
            continue;
        }
        for (i, (a, v)) in attrs.iter().zip(vals).enumerate() {
            if let Err(why) = schema_spec::check(a, v) {
                c.err("attr-type", id, format!("{class} attribute {}: {why}", i + 1));
            }
        }
    }
}

fn guids(c: &mut Chk) {
    let f = c.f;
    let mut seen: HashMap<&str, u32> = HashMap::new();
    let ids: Vec<(u32, &str)> = f.iter().collect();
    for (id, class) in ids {
        let must = class.starts_with("IFCREL")
            || is_element(class)
            || SPATIAL.contains(&class)
            || matches!(
                class,
                "IFCPROJECT"
                    | "IFCSPACE"
                    | "IFCOPENINGELEMENT"
                    | "IFCPROPERTYSET"
                    | "IFCELEMENTQUANTITY"
                    | "IFCZONE"
                    | "IFCDISTRIBUTIONSYSTEM"
            )
            || is_type_class(class);
        if !must {
            continue;
        }
        match c.str_at(id, 0) {
            None => c.err("guid-format", id, format!("{class} has no GlobalId")),
            Some(g) if !is_global_id(g) => {
                c.err(
                    "guid-format",
                    id,
                    format!("{class} GlobalId '{g}' is not valid"),
                );
            }
            Some(g) => {
                if let Some(o) = seen.insert(g, id) {
                    c.err("guid-unique", id, format!("GlobalId '{g}' also on #{o}"));
                }
            }
        }
    }
}

fn project(c: &mut Chk) {
    let ps = c.ids("IFCPROJECT");
    if ps.len() != 1 {
        c.err(
            "project-single",
            0,
            format!("{} IfcProject entities", ps.len()),
        );
    }
    for p in ps {
        let a = c.args(p);
        if refs(a.get(7)).is_empty() {
            c.err("project-context", p, "no representation context".into());
        }
        let Some(u) = a.get(8).and_then(Value::as_ref) else {
            c.err("project-context", p, "no unit assignment".into());
            continue;
        };
        let units = refs(c.args(u).first());
        let len = units.iter().find(|&&x| {
            c.ty(x) == "IFCSIUNIT"
                && c.args(x).get(1).and_then(Value::as_enum) == Some("LENGTHUNIT")
        });
        match len {
            None => c.err("unit-length", u, "no SI length unit".into()),
            Some(&x) => {
                let name = c.args(x).get(3).and_then(Value::as_enum);
                if name != Some("METRE") {
                    c.err(
                        "unit-length",
                        x,
                        format!("length unit {name:?} is not METRE"),
                    );
                }
            }
        }
    }
}

fn spatial(c: &mut Chk) {
    let agg = c.rels("IFCRELAGGREGATES", 4, 5);
    let mut parents: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, p, kids) in &agg {
        let Some(p) = p else { continue };
        for &k in kids {
            parents.entry(k).or_default().push(*p);
            if k == *p {
                c.err("rel-nonempty", *id, "object aggregates itself".into());
            }
        }
    }
    // cycles
    for &start in parents.keys() {
        let (mut cur, mut steps) = (start, 0);
        while let Some(&p) = parents.get(&cur).and_then(|v| v.first()) {
            steps += 1;
            if p == start || steps > 1000 {
                c.err("aggregate-acyclic", start, "aggregation cycle".into());
                break;
            }
            cur = p;
        }
    }
    let one = |c: &mut Chk, class: &str, parent: &str, parents: &HashMap<u32, Vec<u32>>| {
        for id in c.ids(class) {
            let ps = parents.get(&id).cloned().unwrap_or_default();
            if ps.len() != 1 || c.ty(ps[0]) != parent {
                c.err(
                    "spatial-chain",
                    id,
                    format!(
                        "{class} needs exactly one {parent} parent, has {}",
                        ps.len()
                    ),
                );
            }
        }
    };
    one(c, "IFCSITE", "IFCPROJECT", &parents);
    one(c, "IFCBUILDING", "IFCSITE", &parents);
    one(c, "IFCBUILDINGSTOREY", "IFCBUILDING", &parents);

    let storeys = c.ids("IFCBUILDINGSTOREY");
    let mut names: HashSet<String> = HashSet::new();
    for &s in &storeys {
        if c.real_at(s, 9).is_none_or(|v| !v.is_finite()) {
            c.err(
                "storey-elevation",
                s,
                "storey without numeric elevation".into(),
            );
        }
        let n = c.str_at(s, 2).unwrap_or_default().to_string();
        if !names.insert(n.clone()) {
            c.err(
                "storey-unique-name",
                s,
                format!("storey name '{n}' repeated"),
            );
        }
    }

    // containment
    let mut contained: HashMap<u32, Vec<u32>> = HashMap::new();
    for (_, container, elems) in c.rels("IFCRELCONTAINEDINSPATIALSTRUCTURE", 5, 4) {
        let Some(k) = container else { continue };
        for e in elems {
            contained.entry(e).or_default().push(k);
        }
    }
    let nested: HashSet<u32> = c
        .rels("IFCRELNESTS", 4, 5)
        .into_iter()
        .flat_map(|(_, _, kids)| kids)
        .collect();
    let mut any = false;
    let f = c.f;
    let ids: Vec<(u32, &str)> = f.iter().filter(|(_, t)| is_element(t)).collect();
    for (id, class) in ids {
        any = true;
        let part = nested.contains(&id)
            || parents
                .get(&id)
                .is_some_and(|ps| ps.iter().any(|&p| !SPATIAL.contains(&c.ty(p))));
        if part {
            continue;
        }
        let ks = contained.get(&id).cloned().unwrap_or_default();
        if ks.len() != 1 {
            c.err(
                "element-contained",
                id,
                format!(
                    "{class} is in {} containers, expected exactly one",
                    ks.len()
                ),
            );
            continue;
        }
        let k = c.ty(ks[0]);
        let ok = k == "IFCBUILDINGSTOREY" || (k == "IFCSITE" && SITE_OK.contains(&class));
        if !ok {
            c.err(
                "container-class",
                id,
                format!("{class} is contained in {k}"),
            );
        }
    }
    if any && storeys.is_empty() {
        c.err("storey-present", 0, "elements but no storey".into());
    }
    // spaces: one parent, aggregated or contained
    for s in c.ids("IFCSPACE") {
        let n = parents.get(&s).map_or(0, Vec::len) + contained.get(&s).map_or(0, Vec::len);
        if n != 1 {
            c.err("space-placed", s, format!("space has {n} parents"));
        }
    }
}

fn openings(c: &mut Chk) {
    let mut host: HashMap<u32, usize> = HashMap::new();
    for (id, h, ops) in c.rels("IFCRELVOIDSELEMENT", 4, 5) {
        for o in ops {
            *host.entry(o).or_default() += 1;
            if let Some(h) = h {
                if !is_element(c.ty(h)) {
                    c.err("opening-host", id, format!("voided entity is {}", c.ty(h)));
                }
            }
        }
    }
    let mut filled: HashMap<u32, usize> = HashMap::new();
    let mut fills: HashMap<u32, usize> = HashMap::new();
    for (id, op, fillers) in c.rels("IFCRELFILLSELEMENT", 4, 5) {
        if let Some(o) = op {
            *filled.entry(o).or_default() += 1;
        }
        for x in fillers {
            *fills.entry(x).or_default() += 1;
            let t = c.ty(x);
            if t == "IFCOPENINGELEMENT" || t == "IFCSPACE" || !is_element(t) {
                c.err("filler-class", id, format!("filler is {t}"));
            }
        }
    }
    for o in c.ids("IFCOPENINGELEMENT") {
        let n = host.get(&o).copied().unwrap_or(0);
        if n != 1 {
            c.err(
                "opening-voids-one",
                o,
                format!("opening voids {n} elements"),
            );
        }
        let m = filled.get(&o).copied().unwrap_or(0);
        if m > 1 {
            c.err(
                "opening-filled-once",
                o,
                format!("opening filled {m} times"),
            );
        }
    }
    let mut many: Vec<(u32, usize)> = fills.into_iter().filter(|x| x.1 > 1).collect();
    many.sort_unstable();
    for (x, n) in many {
        c.err("filler-once", x, format!("element fills {n} openings"));
    }
}

fn relations(c: &mut Chk) {
    let classes = [
        ("IFCRELAGGREGATES", 4, 5),
        ("IFCRELCONTAINEDINSPATIALSTRUCTURE", 5, 4),
        ("IFCRELDEFINESBYTYPE", 5, 4),
        ("IFCRELDEFINESBYPROPERTIES", 5, 4),
        ("IFCRELASSOCIATESMATERIAL", 5, 4),
        ("IFCRELVOIDSELEMENT", 4, 5),
        ("IFCRELFILLSELEMENT", 4, 5),
    ];
    for (class, relating, related) in classes {
        for (id, a, b) in c.rels(class, relating, related) {
            if a.is_none() || b.is_empty() {
                c.err(
                    "rel-nonempty",
                    id,
                    format!("{class} without relating or related objects"),
                );
            }
        }
    }
    let mut types: HashMap<u32, u32> = HashMap::new();
    for (id, ty, objs) in c.rels("IFCRELDEFINESBYTYPE", 5, 4) {
        if let Some(t) = ty {
            if !is_type_class(c.ty(t)) {
                c.err("type-class", id, format!("relating type is {}", c.ty(t)));
            }
        }
        for o in objs {
            *types.entry(o).or_default() += 1;
        }
    }
    for (o, n) in types {
        if n > 1 {
            c.err("type-single", o, format!("element has {n} types"));
        }
    }
}

fn placement(c: &mut Chk) {
    let mut classes: Vec<&str> = ELEMENTS.to_vec();
    classes.extend(SPATIAL);
    classes.extend(["IFCSPACE", "IFCOPENINGELEMENT"]);
    for class in classes {
        for id in c.ids(class) {
            let a = c.args(id);
            let Some(mut p) = a.get(5).and_then(Value::as_ref) else {
                c.err("placement", id, format!("{class} without ObjectPlacement"));
                continue;
            };
            let mut steps = 0;
            while let Some(next) = c.args(p).first().and_then(Value::as_ref) {
                steps += 1;
                if steps > 256 {
                    c.err("placement", id, "placement chain does not end".into());
                    break;
                }
                p = next;
            }
            if NEED_SHAPE.contains(&class) && a.get(6).and_then(Value::as_ref).is_none() {
                c.err("representation", id, format!("{class} without shape"));
            }
        }
    }
}

fn materials(c: &mut Chk) {
    let mut n: HashMap<u32, u32> = HashMap::new();
    for (_, _, objs) in c.rels("IFCRELASSOCIATESMATERIAL", 5, 4) {
        for o in objs {
            *n.entry(o).or_default() += 1;
        }
    }
    for (o, k) in &n {
        if *k > 1 {
            c.err("material-single", *o, format!("{k} material associations"));
        }
    }
    for u in c.ids("IFCMATERIALLAYERSETUSAGE") {
        let Some(set) = c.args(u).first().and_then(Value::as_ref) else {
            c.err("layer-set", u, "usage without layer set".into());
            continue;
        };
        let layers = refs(c.args(set).first());
        if c.ty(set) != "IFCMATERIALLAYERSET" || layers.is_empty() {
            c.err("layer-set", u, "layer set missing or empty".into());
        }
        for l in layers {
            if c.real_at(l, 1).is_none_or(|t| !(t > 0.0)) {
                c.err("layer-set", l, "layer thickness is not positive".into());
            }
        }
    }
    for w in c.ids("IFCWALL") {
        if !n.contains_key(&w) {
            c.err("wall-layers", w, "wall without material association".into());
        }
    }
}

fn props(c: &mut Chk) {
    let mut names: HashMap<u32, HashSet<String>> = HashMap::new();
    for (id, def, objs) in c.rels("IFCRELDEFINESBYPROPERTIES", 5, 4) {
        let Some(def) = def else { continue };
        let name = c.str_at(def, 2).unwrap_or_default().to_string();
        match c.ty(def) {
            "IFCPROPERTYSET" | "IFCELEMENTQUANTITY" => {}
            t => c.err("pset-nonempty", id, format!("relating definition is {t}")),
        }
        for o in objs {
            if !names.entry(o).or_default().insert(name.clone()) {
                c.err("pset-nonempty", o, format!("two sets named '{name}'"));
            }
        }
    }
    for p in c.ids("IFCPROPERTYSET") {
        if refs(c.args(p).get(4)).is_empty() {
            c.err("pset-nonempty", p, "property set without properties".into());
        }
    }
    for q in c.ids("IFCELEMENTQUANTITY") {
        let items = refs(c.args(q).get(5));
        if items.is_empty() {
            c.err("pset-nonempty", q, "quantity set without quantities".into());
        }
        for i in items {
            let v = c.real_at(i, 3);
            if v.is_none_or(|v| !v.is_finite() || v < 0.0) {
                c.err("quantity-valid", i, format!("quantity value {v:?}"));
            }
        }
    }
}

fn spaces(c: &mut Chk) {
    let mut qto: HashMap<u32, u32> = HashMap::new();
    for (_, def, objs) in c.rels("IFCRELDEFINESBYPROPERTIES", 5, 4) {
        let Some(def) = def else { continue };
        if c.str_at(def, 2) == Some("Qto_SpaceBaseQuantities") {
            for o in objs {
                qto.insert(o, def);
            }
        }
    }
    for s in c.ids("IFCSPACE") {
        let a = c.args(s);
        let name = a.get(2).and_then(Value::as_str).unwrap_or_default();
        let long = a.get(7).and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() && long.is_empty() {
            c.err("space-placed", s, "space without name".into());
        }
        if a.get(6).and_then(Value::as_ref).is_none() {
            continue;
        }
        let area = qto.get(&s).and_then(|&q| {
            refs(c.args(q).get(5)).into_iter().find_map(|i| {
                (c.str_at(i, 0) == Some("NetFloorArea"))
                    .then(|| c.real_at(i, 3))
                    .flatten()
            })
        });
        if area.is_none_or(|v| !(v > 0.0)) {
            c.err(
                "space-qto",
                s,
                "no positive NetFloorArea in Qto_SpaceBaseQuantities".into(),
            );
        }
    }
}

fn mapconv(c: &mut Chk) {
    let ms = c.ids("IFCMAPCONVERSION");
    if c.schema == Some(IfcSchema::Ifc2x3) && !ms.is_empty() {
        c.err(
            "mapconv",
            ms[0],
            "IfcMapConversion in an IFC2X3 file".into(),
        );
    }
    if ms.len() > 1 {
        c.err("mapconv", ms[1], "more than one map conversion".into());
    }
    for m in ms {
        let a = c.args(m);
        let ctx_ok = a
            .first()
            .and_then(Value::as_ref)
            .is_some_and(|x| c.ty(x) == "IFCGEOMETRICREPRESENTATIONCONTEXT");
        let crs_ok = a.get(1).and_then(Value::as_ref).is_some_and(|x| {
            c.ty(x) == "IFCPROJECTEDCRS" && c.str_at(x, 0).is_some_and(|n| !n.is_empty())
        });
        let nums = (2..5).all(|i| a.get(i).and_then(Value::as_f64).is_some_and(f64::is_finite));
        let axis = (
            a.get(5).and_then(Value::as_f64),
            a.get(6).and_then(Value::as_f64),
        );
        let axis_ok = match axis {
            (Some(x), Some(y)) => x.is_finite() && y.is_finite() && (x != 0.0 || y != 0.0),
            _ => false,
        };
        if !(ctx_ok && crs_ok && nums && axis_ok) {
            c.err(
                "mapconv",
                m,
                "context, named CRS, coordinates and axis are required".into(),
            );
        }
    }
}
