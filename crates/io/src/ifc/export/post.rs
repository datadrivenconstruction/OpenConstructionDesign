// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Schema choice and the additive post-pass of an authored export: element types with
//! IfcRelDefinesByType, `Pset_*Common` for the families the builders leave without one, and the
//! file header (schema, time stamp). Everything added is derived from the written entities and
//! gets a GlobalId hashed from stable keys (type ids also from the project id, so merged
//! models do not collide). A re-export of the same model is byte-identical when
//! `IfcOptions::timestamp` is set; without it the header carries the current time.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use web_time::{SystemTime, UNIX_EPOCH};

use super::ifc2x3;
use crate::ifc::reader::{refs_of, StepFile, Value};
use crate::ifc::step::{compress_guid, hash128, header, list, r, text, TRAILER};

pub use crate::ifc::schema_spec::IfcSchema;

const NONE: &str = "$";

/// Options of [`super::export_ifc_with`].
#[derive(Clone, Debug, Default)]
pub struct IfcOptions {
    pub schema: IfcSchema,
    /// FILE_NAME time stamp (`YYYY-MM-DDThh:mm:ss`); `None` — the current UTC time. Tests pin it.
    pub timestamp: Option<String>,
    /// Triangles instead of exact advanced bodies (smooth sweeps, tapered extrusions), for
    /// receivers without advanced geometry; the export notes how many bodies it changed.
    pub tessellate: bool,
    /// Mass forms as IfcBuildingElementProxy with the form kind as ObjectType.
    pub mass_forms: bool,
}

/// Lines appended after the existing entities.
pub(super) struct Emit {
    next: u32,
    pub lines: Vec<String>,
    /// Properties appended to the property set of the file with that id.
    pub extend: HashMap<u32, Vec<u32>>,
    /// Entities of the file that are left out (property sets merged into another).
    pub dropped: HashSet<u32>,
}

impl Emit {
    pub fn new(next: u32) -> Self {
        Self {
            next,
            lines: vec![],
            extend: HashMap::new(),
            dropped: HashSet::new(),
        }
    }

    /// `data` with the dropped entities removed and the extended property sets written with
    /// their added properties (a set's property list is its last attribute).
    pub fn patch<'a>(&self, data: &'a str) -> Cow<'a, str> {
        if self.extend.is_empty() && self.dropped.is_empty() {
            return Cow::Borrowed(data);
        }
        let mut out = String::with_capacity(data.len());
        for l in data.split_inclusive('\n') {
            let id = l
                .strip_prefix('#')
                .and_then(|r| r.split_once('='))
                .and_then(|(n, _)| n.parse::<u32>().ok());
            let Some(id) = id else {
                out.push_str(l);
                continue;
            };
            if self.dropped.contains(&id) {
                continue;
            }
            match (self.extend.get(&id), l.trim_end().strip_suffix("));")) {
                (Some(add), Some(head)) => {
                    out.push_str(head);
                    for (k, p) in add.iter().enumerate() {
                        if k > 0 || !head.ends_with('(') {
                            out.push(',');
                        }
                        out.push_str(&format!("#{p}"));
                    }
                    out.push_str("));
");
                }
                _ => out.push_str(l),
            }
        }
        Cow::Owned(out)
    }

    pub fn add(&mut self, name: &str, args: &[String]) -> u32 {
        let n = self.next;
        self.next += 1;
        self.lines.push(format!("#{n}={name}({});", args.join(",")));
        n
    }
}

use ddc_geom::to_mm as mm;

fn guid(key: &str) -> String {
    format!("'{}'", compress_guid(hash128(key)))
}

/// Occurrence class, type class, predefined type of the type, label used in generated names.
const TYPE_MAP: &[(&str, &str, &str)] = &[
    ("IFCWALL", "IFCWALLTYPE", ".STANDARD."),
    ("IFCSLAB", "IFCSLABTYPE", ".NOTDEFINED."),
    ("IFCROOF", "IFCROOFTYPE", ".NOTDEFINED."),
    ("IFCCOLUMN", "IFCCOLUMNTYPE", ".NOTDEFINED."),
    ("IFCBEAM", "IFCBEAMTYPE", ".NOTDEFINED."),
    ("IFCDOOR", "IFCDOORTYPE", ".NOTDEFINED."),
    ("IFCWINDOW", "IFCWINDOWTYPE", ".NOTDEFINED."),
];

/// Occurrence class, `Pset_*Common` name, IsExternal (fixed, or None: that of the host wall).
const COMMON: &[(&str, &str, Option<bool>)] = &[
    ("IFCSLAB", "Pset_SlabCommon", Some(false)),
    ("IFCROOF", "Pset_RoofCommon", Some(true)),
    ("IFCSTAIR", "Pset_StairCommon", Some(false)),
    ("IFCRAMP", "Pset_RampCommon", Some(false)),
    ("IFCCURTAINWALL", "Pset_CurtainWallCommon", Some(true)),
    ("IFCCOVERING", "Pset_CoveringCommon", Some(false)),
    ("IFCRAILING", "Pset_RailingCommon", Some(false)),
    ("IFCSPACE", "Pset_SpaceCommon", Some(false)),
    ("IFCDOOR", "Pset_DoorCommon", None),
    ("IFCWINDOW", "Pset_WindowCommon", None),
];

struct Group {
    name: String,
    members: Vec<u32>,
    set: Option<u32>,
}

fn flag(v: bool) -> String {
    format!("IFCBOOLEAN({})", if v { ".T." } else { ".F." })
}

fn nones(n: usize) -> impl Iterator<Item = String> {
    (0..n).map(|_| NONE.to_string())
}

/// What the additions of one export share: the parsed file, its class index and the
/// OwnerHistory attribute of every rooted entity added.
struct Pass<'a> {
    f: &'a StepFile,
    by: BTreeMap<&'a str, Vec<u32>>,
    legacy: bool,
    /// `$`, or the owner history IFC2X3 requires on rooted entities.
    own: String,
    /// The project's own id salts the type ids: two projects with a type of the same name
    /// stay apart when their files are merged.
    salt: &'a str,
}

/// One use of a property set by an element: the set, its relation, and whether the element
/// owns it alone.
struct SetUse {
    set: u32,
    rel: u32,
    exclusive: bool,
}

type SetTable = BTreeMap<(u32, String), Vec<SetUse>>;

/// A property set of single values on `target`: `props` are (name, typed value), `own` the
/// owner history attribute.
pub(super) fn add_pset(
    e: &mut Emit,
    own: &str,
    key: &str,
    name: &str,
    props: &[(&str, String)],
    target: u32,
) {
    let ids: Vec<u32> = props
        .iter()
        .map(|(n, v)| {
            let a = [text(n), NONE.into(), v.clone(), NONE.into()];
            e.add("IFCPROPERTYSINGLEVALUE", &a)
        })
        .collect();
    let set_args = [guid(key), own.into(), text(name), NONE.into(), list(&ids)];
    let set = e.add("IFCPROPERTYSET", &set_args);
    let rel = [guid(&format!("{key}:rel")), own.into(), NONE.into(), NONE.into()];
    let mut rel = rel.to_vec();
    rel.extend([list(&[target]), r(set)]);
    e.add("IFCRELDEFINESBYPROPERTIES", &rel);
}

impl Pass<'_> {
    fn all(&self, class: &str) -> &[u32] {
        self.by.get(class).map_or(&[][..], Vec::as_slice)
    }

    /// `a[key_at] -> a[val_at]` over the relations of `class`.
    fn link(&self, class: &str, key_at: usize, val_at: usize) -> HashMap<u32, u32> {
        let mut m = HashMap::new();
        for &rel in self.all(class) {
            let a = self.f.args(rel);
            if let (Some(k), Some(v)) = (
                a.get(key_at).and_then(Value::as_ref),
                a.get(val_at).and_then(Value::as_ref),
            ) {
                m.insert(k, v);
            }
        }
        m
    }

    /// A rooted entity: GlobalId hashed from `key`, owner history, then `rest`.
    fn rooted(&self, e: &mut Emit, class: &str, key: &str, rest: Vec<String>) -> u32 {
        let mut a = vec![guid(key), self.own.clone()];
        a.extend(rest);
        e.add(class, &a)
    }

    /// A relationship without name and description.
    fn rel(&self, e: &mut Emit, class: &str, key: &str, ends: [String; 2]) -> u32 {
        let mut rest: Vec<String> = nones(2).collect();
        rest.extend(ends);
        self.rooted(e, class, key, rest)
    }

    /// Layer set of each element, through IfcRelAssociatesMaterial.
    fn layer_sets(&self) -> HashMap<u32, u32> {
        let mut set_of = HashMap::new();
        for &rel in self.all("IFCRELASSOCIATESMATERIAL") {
            let a = self.f.args(rel);
            let Some(mat) = a.get(5).and_then(Value::as_ref) else {
                continue;
            };
            let set = match self.f.type_of(mat) {
                Some("IFCMATERIALLAYERSETUSAGE") => {
                    self.f.args(mat).first().and_then(Value::as_ref)
                }
                Some("IFCMATERIALLAYERSET") => Some(mat),
                _ => None,
            };
            if let Some(set) = set {
                for el in refs_of(a.get(4)) {
                    set_of.insert(el, set);
                }
            }
        }
        set_of
    }

    /// Elements of `class` without a type, grouped by what makes them one type.
    fn groups(
        &self,
        class: &str,
        set_of: &HashMap<u32, u32>,
        typed: &HashSet<u32>,
    ) -> BTreeMap<String, Group> {
        let f = self.f;
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for &el in self.all(class) {
            if typed.contains(&el) {
                continue;
            }
            let a = f.args(el);
            let (key, name, set) = if let Some(&set) = set_of.get(&el) {
                let n = f.str_at(set, 1).filter(|n| !n.is_empty());
                (
                    format!("ls{set}"),
                    n.map_or_else(|| "Layered".into(), str::to_string),
                    Some(set),
                )
            } else if class == "IFCDOOR" || class == "IFCWINDOW" {
                let (h, w) = (
                    a.get(8).and_then(Value::as_f64),
                    a.get(9).and_then(Value::as_f64),
                );
                let (Some(h), Some(w)) = (h, w) else { continue };
                let label = if class == "IFCDOOR" { "Door" } else { "Window" };
                (
                    format!("d{}x{}", mm(w), mm(h)),
                    format!("{label} {}x{}", mm(w), mm(h)),
                    None,
                )
            } else if let Some(o) = a.get(4).and_then(Value::as_str).filter(|o| !o.is_empty()) {
                (format!("o{o}"), o.to_string(), None)
            } else {
                continue;
            };
            let g = Group {
                name,
                members: vec![],
                set,
            };
            groups.entry(key).or_insert(g).members.push(el);
        }
        groups
    }

    /// The type entity of one group, its material association and IfcRelDefinesByType.
    fn add_type(&self, e: &mut Emit, kind: (&str, &str, &str), key: &str, g: &Group) {
        let (class, type_class, predefined) = kind;
        let opening = class == "IFCDOOR" || class == "IFCWINDOW";
        let mut rest: Vec<String> = vec![text(&g.name)];
        let type_class = match (opening, self.legacy) {
            // IFC2X3 holds doors and windows as styles
            (true, true) => {
                rest.extend(nones(5));
                rest.extend([".NOTDEFINED.", ".NOTDEFINED.", ".F.", ".F."].map(String::from));
                if class == "IFCDOOR" {
                    "IFCDOORSTYLE"
                } else {
                    "IFCWINDOWSTYLE"
                }
            }
            (true, false) => {
                rest.extend(nones(6));
                rest.extend([predefined, ".NOTDEFINED.", ".F."].map(String::from));
                rest.push(NONE.into());
                type_class
            }
            (false, _) => {
                rest.extend(nones(6));
                rest.push(predefined.into());
                type_class
            }
        };
        let ty = self.rooted(e, type_class, key, rest);
        if let Some(set) = g.set {
            let ends = [list(&[ty]), r(set)];
            self.rel(e, "IFCRELASSOCIATESMATERIAL", &format!("{key}:mat"), ends);
        }
        let ends = [list(&g.members), r(ty)];
        self.rel(e, "IFCRELDEFINESBYTYPE", &format!("{key}:rel"), ends);
    }

    /// Element types (with material layer set and IfcRelDefinesByType); the type name of each
    /// element that got one.
    fn element_types(&self, e: &mut Emit) -> HashMap<u32, String> {
        let set_of = self.layer_sets();
        let mut typed: HashSet<u32> = HashSet::new();
        for &rel in self.all("IFCRELDEFINESBYTYPE") {
            typed.extend(refs_of(self.f.args(rel).get(4)));
        }
        let mut used: BTreeSet<String> = BTreeSet::new();
        let mut type_name: HashMap<u32, String> = HashMap::new();
        for &(class, type_class, predefined) in TYPE_MAP {
            // IFC2X3 has no roof type
            if self.legacy && class == "IFCROOF" {
                continue;
            }
            for g in self.groups(class, &set_of, &typed).into_values() {
                let mut key = format!("type:{}:{class}:{}", self.salt, g.name);
                let mut n = 1;
                while !used.insert(key.clone()) {
                    n += 1;
                    key = format!("type:{}:{class}:{}:{n}", self.salt, g.name);
                }
                self.add_type(e, (class, type_class, predefined), &key, &g);
                for m in g.members {
                    type_name.insert(m, g.name.clone());
                }
            }
        }
        type_name
    }

    /// IsExternal of walls, from their stored `Pset_WallCommon`.
    fn wall_externals(&self) -> HashMap<u32, bool> {
        let f = self.f;
        let mut wall_ext: HashMap<u32, bool> = HashMap::new();
        for &rel in self.all("IFCRELDEFINESBYPROPERTIES") {
            let a = f.args(rel);
            let Some(def) = a.get(5).and_then(Value::as_ref) else {
                continue;
            };
            let wall_set = f.type_of(def) == Some("IFCPROPERTYSET")
                && f.str_at(def, 2) == Some("Pset_WallCommon");
            if !wall_set {
                continue;
            }
            for p in refs_of(f.args(def).get(4)) {
                let pa = f.args(p);
                if pa.first().and_then(Value::as_str) != Some("IsExternal") {
                    continue;
                }
                if let Some(Value::Typed(_, v)) = pa.get(2) {
                    if let Some(Value::Enum(t)) = v.first() {
                        for o in refs_of(a.get(4)) {
                            wall_ext.insert(o, t == "T");
                        }
                    }
                }
            }
        }
        wall_ext
    }

    /// The property sets of the file by (owner, name), in set id order. `exclusive`: the set
    /// has one owner and one relation, so it may be extended or merged for that owner alone.
    fn set_table(&self) -> SetTable {
        let f = self.f;
        let mut uses: HashMap<u32, usize> = HashMap::new();
        for &rel in self.all("IFCRELDEFINESBYPROPERTIES") {
            if let Some(set) = f.args(rel).get(5).and_then(Value::as_ref) {
                *uses.entry(set).or_default() += 1;
            }
        }
        let mut table: SetTable = BTreeMap::new();
        for &rel in self.all("IFCRELDEFINESBYPROPERTIES") {
            let a = f.args(rel);
            let Some(set) = a.get(5).and_then(Value::as_ref) else {
                continue;
            };
            if f.type_of(set) != Some("IFCPROPERTYSET") {
                continue;
            }
            let Some(name) = f.str_at(set, 2) else {
                continue;
            };
            let owners = refs_of(a.get(4));
            let exclusive = owners.len() == 1 && uses[&set] == 1;
            for o in owners {
                let u = SetUse {
                    set,
                    rel,
                    exclusive,
                };
                table.entry((o, name.to_string())).or_default().push(u);
            }
        }
        for v in table.values_mut() {
            v.sort_by_key(|u| u.set);
        }
        table
    }

    /// The missing `Pset_*Common` of every element of the families in [`COMMON`]. A set of
    /// that name the element owns alone gets the properties it lacks; a set shared with other
    /// elements is not changed (the value belongs to this element only), so the element gets
    /// its own set with what the shared ones lack.
    fn common_sets(&self, e: &mut Emit, type_name: &HashMap<u32, String>, table: &SetTable) {
        let f = self.f;
        let wall_ext = self.wall_externals();
        let host_of_open = self.link("IFCRELVOIDSELEMENT", 5, 4);
        let open_of_fill = self.link("IFCRELFILLSELEMENT", 5, 4);
        for &(class, pset, ext) in COMMON {
            for &el in self.all(class) {
                let Some(owner_guid) = f.str_at(el, 0) else {
                    continue;
                };
                let label = type_name
                    .get(&el)
                    .map(String::as_str)
                    .or_else(|| f.str_at(el, 2))
                    .unwrap_or_default();
                let external = ext.unwrap_or_else(|| {
                    open_of_fill
                        .get(&el)
                        .and_then(|o| host_of_open.get(o))
                        .and_then(|h| wall_ext.get(h))
                        .copied()
                        .unwrap_or(false)
                });
                let mut props = vec![];
                if !label.is_empty() {
                    props.push(("Reference", format!("IFCIDENTIFIER({})", text(label))));
                }
                props.push(("IsExternal", flag(external)));
                let uses = table.get(&(el, pset.to_string()));
                let keeper = uses.and_then(|u| u.iter().find(|x| x.exclusive)).map(|x| x.set);
                let mut have: HashSet<&str> = HashSet::new();
                for u in uses.into_iter().flatten() {
                    have.extend(self.prop_names(u.set));
                }
                if let Some(add) = keeper.and_then(|k| e.extend.get(&k)) {
                    have.extend(add.iter().filter_map(|&p| f.str_at(p, 0)));
                }
                let lacking: Vec<(&str, String)> =
                    props.into_iter().filter(|(n, _)| !have.contains(n)).collect();
                match (keeper, uses.is_some()) {
                    (_, false) => {
                        let key = format!("pset:{pset}:{owner_guid}");
                        add_pset(e, &self.own, &key, pset, &lacking, el);
                    }
                    (Some(set), _) => {
                        for (n, v) in lacking {
                            let a = [text(n), NONE.into(), v, NONE.into()];
                            let p = e.add("IFCPROPERTYSINGLEVALUE", &a);
                            e.extend.entry(set).or_default().push(p);
                        }
                    }
                    (None, true) if !lacking.is_empty() => {
                        let key = format!("pset:{pset}:{owner_guid}:own");
                        add_pset(e, &self.own, &key, pset, &lacking, el);
                    }
                    (None, true) => {}
                }
            }
        }
    }

    /// Names of the properties of property set `set`.
    fn prop_names(&self, set: u32) -> HashSet<&str> {
        refs_of(self.f.args(set).get(4))
            .into_iter()
            .filter_map(|p| self.f.str_at(p, 0))
            .collect()
    }

    /// Several sets of one name that one element owns alone become one: the lowest id keeps
    /// its properties and takes those of the others it lacks (by name); the others are left
    /// out. [`Pass::set_table`] is the one source for which set of an element is the kept one.
    fn merge_duplicates(&self, e: &mut Emit, table: &SetTable) {
        let f = self.f;
        for uses in table.values() {
            let mut own = uses.iter().filter(|u| u.exclusive);
            let Some(keep) = own.next().map(|u| u.set) else {
                continue;
            };
            let mut names = self.prop_names(keep);
            for u in own {
                for p in refs_of(f.args(u.set).get(4)) {
                    match f.str_at(p, 0) {
                        Some(n) if names.insert(n) => e.extend.entry(keep).or_default().push(p),
                        _ => {
                            e.dropped.insert(p);
                        }
                    }
                }
                e.dropped.extend([u.set, u.rel]);
            }
        }
    }
}

/// Types (with material layer set and IfcRelDefinesByType) and the missing common sets.
/// `owner` is the owner history of the entities added (IFC2X3 only).
pub(super) fn enrich(f: &StepFile, e: &mut Emit, owner: Option<u32>, salt: &str) {
    let pass = Pass {
        f,
        by: f.by_class(),
        legacy: owner.is_some(),
        own: owner.map_or_else(|| NONE.into(), |o| format!("#{o}")),
        salt,
    };
    let type_name = pass.element_types(e);
    let table = pass.set_table();
    pass.merge_duplicates(e, &table);
    pass.common_sets(e, &type_name, &table);
}

/// Stable id of the model's project (`meta.project_uid`, set when the file is created); empty
/// when the model has none.
pub(super) fn project_salt(model: &ddc_model::Model) -> String {
    model
        .sql()
        .query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| {
            r.get::<_, rusqlite::types::Value>(0)
        })
        .map(|v| match v {
            rusqlite::types::Value::Integer(i) => i.to_string(),
            rusqlite::types::Value::Text(t) => t,
            _ => String::new(),
        })
        .unwrap_or_default()
}

/// UTC time as `YYYY-MM-DDThh:mm:ss`.
fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Classes whose last attribute IFC4 does not have (IFC4X3 added it).
const IFC4X3_LAST: [&str; 3] = [
    "IFCVIRTUALELEMENT",
    "IFCCARTESIANPOINTLIST2D",
    "IFCCARTESIANPOINTLIST3D",
];

/// Geographic element classes and the index of the attribute that names a user-defined kind
/// (ObjectType of the occurrence, ElementType of the type).
const IFC4X3_ENUM: [(&str, usize); 2] = [("IFCGEOGRAPHICELEMENT", 4), ("IFCGEOGRAPHICELEMENTTYPE", 8)];

/// The builders write the IFC4X3 shape. The entities of [`IFC4X3_LAST`] are written again
/// without their last attribute; IFC4 has no VEGETATION geographic element, so a plant is
/// USERDEFINED and names itself `Vegetation`.
fn ifc4_data(f: &StepFile, data: &str, out: &mut String) {
    let mut by = f.by_class();
    let cut: HashSet<u32> = IFC4X3_LAST
        .iter()
        .flat_map(|c| by.remove(c).unwrap_or_default())
        .collect();
    let veg = |v: Option<&Value>| matches!(v, Some(Value::Enum(e)) if e == "VEGETATION");
    let named: HashMap<u32, usize> = IFC4X3_ENUM
        .iter()
        .flat_map(|&(c, k)| by.remove(c).unwrap_or_default().into_iter().map(move |id| (id, k)))
        .filter(|(id, _)| veg(f.args(*id).last()))
        .collect();
    if cut.is_empty() && named.is_empty() {
        out.push_str(data);
        return;
    }
    for l in data.split_inclusive('\n') {
        let id = l
            .strip_prefix('#')
            .and_then(|r| r.split_once('='))
            .and_then(|(n, _)| n.parse::<u32>().ok())
            .filter(|id| cut.contains(id) || named.contains_key(id));
        match id {
            Some(id) => {
                let mut a = f.args(id).to_vec();
                match named.get(&id) {
                    Some(&k) => {
                        if let Some(x) = a.get_mut(k) {
                            *x = Value::Str("Vegetation".into());
                        }
                        if let Some(x) = a.last_mut() {
                            *x = Value::Enum("USERDEFINED".into());
                        }
                    }
                    None => {
                        a.pop();
                    }
                }
                let class = f.type_of(id).unwrap_or_default();
                out.push_str(&ifc2x3::line(id, class, &a));
                out.push('\n');
            }
            None => out.push_str(l),
        }
    }
}

/// The exported `text_in` with types and common sets added, the header set for `opts`, and
/// the entities converted for IFC2X3 when asked. `file_name` goes into the header.
pub(super) fn apply(
    text_in: &str,
    opts: &IfcOptions,
    file_name: &str,
    salt: &str,
) -> Result<(String, Vec<String>), String> {
    let f = StepFile::parse(text_in.as_bytes()).map_err(|e| format!("post-pass: {e}"))?;
    let body = text_in
        .strip_suffix(TRAILER)
        .ok_or("post-pass: file has no trailer")?;
    let (_, data) = body
        .split_once("DATA;\n")
        .ok_or("post-pass: no DATA section")?;
    let mut e = Emit::new(f.iter().map(|(id, _)| id).max().unwrap_or(0) + 1);
    let owner = (opts.schema == IfcSchema::Ifc2x3).then(|| ifc2x3::owner_history(&mut e));
    enrich(&f, &mut e, owner, salt);
    let data = e.patch(data);
    let stamp = opts.timestamp.clone().unwrap_or_else(now_iso);
    let mut out = header(file_name, opts.schema, &stamp);
    out.reserve(text_in.len() + e.lines.len() * 120);
    let mut notes = vec![];
    match owner {
        Some(oh) => ifc2x3::downgrade(&f, &mut e, oh, &mut out, &mut notes),
        None if opts.schema == IfcSchema::Ifc4 => ifc4_data(&f, &data, &mut out),
        None => out.push_str(&data),
    }
    for l in &e.lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(TRAILER);
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(data: &str) -> String {
        let head = header("m.ifc", IfcSchema::Ifc4x3, "2026-01-01T00:00:00");
        // the exporter writes one entity per line starting at column 0; indentation of a
        // multi-line literal is not part of the file
        let data: String = data.lines().map(|l| format!("{}\n", l.trim_start())).collect();
        format!("{head}{data}{TRAILER}")
    }

    fn sets_of_one_name() -> String {
        file(
            "#1=IFCFURNISHINGELEMENT('g1',$,'F',$,$,$,$,$,$);\n\
             #2=IFCPROPERTYSINGLEVALUE('A',$,IFCLABEL('a'),$);\n\
             #3=IFCPROPERTYSINGLEVALUE('B',$,IFCLABEL('b'),$);\n\
             #4=IFCPROPERTYSINGLEVALUE('A',$,IFCLABEL('x'),$);\n\
             #5=IFCPROPERTYSET('s1',$,'Pset_X',$,(#2));\n\
             #6=IFCRELDEFINESBYPROPERTIES('r1',$,$,$,(#1),#5);\n\
             #7=IFCPROPERTYSET('s2',$,'Pset_X',$,(#3,#4));\n\
             #8=IFCRELDEFINESBYPROPERTIES('r2',$,$,$,(#1),#7);\n",
        )
    }

    #[test]
    fn sets_of_one_name_on_one_element_are_merged_without_duplicate_properties() {
        let opts = |schema| IfcOptions {
            schema,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        };
        for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3, IfcSchema::Ifc2x3] {
            let (out, _) = apply(&sets_of_one_name(), &opts(schema), "m.ifc", "").unwrap();
            assert_eq!(out.matches("=IFCPROPERTYSET(").count(), 1, "{schema:?}");
            assert_eq!(out.matches("=IFCRELDEFINESBYPROPERTIES(").count(), 1, "{schema:?}");
            let set = out.lines().find(|l| l.contains("=IFCPROPERTYSET(")).unwrap();
            assert!(set.ends_with("(#2,#3));"), "{set}");
            // the second 'A' lost to the first, and the dropped entities are gone
            assert!(!out.contains("#4=") && !out.contains("#7="), "{schema:?}");
            assert!(StepFile::parse(out.as_bytes()).is_ok(), "{schema:?}");
        }
    }
    fn opts(schema: IfcSchema) -> IfcOptions {
        IfcOptions {
            schema,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        }
    }

    /// Property names of every set called `name` in `out`.
    fn sets_named(out: &str, name: &str) -> Vec<Vec<String>> {
        let f = StepFile::parse(out.as_bytes()).unwrap();
        let by = f.by_class();
        let sets = by.get("IFCPROPERTYSET").cloned().unwrap_or_default();
        sets.into_iter()
            .filter(|&s| f.str_at(s, 2) == Some(name))
            .map(|s| {
                refs_of(f.args(s).get(4))
                    .into_iter()
                    .map(|p| f.str_at(p, 0).unwrap_or_default().to_string())
                    .collect()
            })
            .collect()
    }

    /// Every `#n` used on the right of a line is defined in the file.
    fn assert_no_dangling(out: &str) {
        let defined: HashSet<&str> =
            out.lines().filter_map(|l| l.split_once('=')).map(|x| x.0).collect();
        for l in out.lines().filter(|l| l.starts_with('#')) {
            let rhs = l.split_once('=').unwrap().1;
            for part in rhs.split('#').skip(1) {
                let n: String = part.chars().take_while(char::is_ascii_digit).collect();
                assert!(defined.contains(format!("#{n}").as_str()), "dangling #{n} in {l}");
            }
        }
    }

    #[test]
    fn the_kept_set_is_the_lowest_id_whatever_the_relation_order() {
        let text = file(
            "#1=IFCSTAIR('g1',$,'S',$,$,$,$,$,$);
             #2=IFCPROPERTYSINGLEVALUE('A',$,IFCLABEL('a'),$);
             #3=IFCPROPERTYSINGLEVALUE('B',$,IFCLABEL('b'),$);
             #4=IFCPROPERTYSINGLEVALUE('A',$,IFCLABEL('x'),$);
             #5=IFCPROPERTYSET('s1',$,'Pset_StairCommon',$,(#3));
             #6=IFCPROPERTYSET('s2',$,'Pset_StairCommon',$,(#2,#4));
             #7=IFCRELDEFINESBYPROPERTIES('r1',$,$,$,(#1),#6);
             #8=IFCRELDEFINESBYPROPERTIES('r2',$,$,$,(#1),#5);
",
        );
        for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3] {
            let (out, _) = apply(&text, &opts(schema), "m.ifc", "").unwrap();
            let sets = sets_named(&out, "Pset_StairCommon");
            assert_eq!(sets.len(), 1, "{schema:?}: {sets:?}");
            let mut names = sets[0].clone();
            names.sort();
            assert_eq!(names, ["A", "B", "IsExternal", "Reference"], "{schema:?}");
            assert!(!out.contains("#4=") && !out.contains("#6="), "{schema:?}");
            assert_no_dangling(&out);
        }
    }

    #[test]
    fn a_set_shared_by_two_elements_is_not_given_the_value_of_one() {
        let text = file(
            "#1=IFCSTAIR('g1',$,'S1',$,$,$,$,$,$);
             #2=IFCSTAIR('g2',$,'S2',$,$,$,$,$,$);
             #3=IFCPROPERTYSET('s1',$,'Pset_StairCommon',$,(#4));
             #4=IFCPROPERTYSINGLEVALUE('FireRating',$,IFCLABEL('EI30'),$);
             #5=IFCRELDEFINESBYPROPERTIES('r1',$,$,$,(#1,#2),#3);
",
        );
        for schema in [IfcSchema::Ifc4, IfcSchema::Ifc4x3] {
            let (out, _) = apply(&text, &opts(schema), "m.ifc", "").unwrap();
            let sets = sets_named(&out, "Pset_StairCommon");
            // the shared set as it was, and one set of its own for each stair
            assert_eq!(sets.len(), 3, "{schema:?}: {sets:?}");
            assert!(sets.contains(&vec!["FireRating".to_string()]), "{sets:?}");
            let own = sets.iter().filter(|s| s.iter().any(|n| n == "IsExternal")).count();
            assert_eq!(own, 2, "{sets:?}");
            for set in &sets {
                let names: HashSet<_> = set.iter().collect();
                assert_eq!(names.len(), set.len(), "duplicate property in {set:?}");
            }
            assert_no_dangling(&out);
        }
    }
}
