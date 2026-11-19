//! Element properties (`prop` table) as property sets. A value whose stored unit label is not
//! the project unit of its measure is written in that unit with the unit entity of its own
//! (one per distinct label); project-unit values keep `$`.

use std::collections::BTreeMap;

use rusqlite::types::Value;

use super::super::step::{list, r, real, text};
use super::{db, s, Ctx, NONE};

const WALL_COMMON: &str = "Pset_WallCommon";
const IS_EXTERNAL: &str = "IsExternal";

/// Value type the standard property sets fix for a property.
#[derive(Clone, Copy)]
enum Std {
    Bool,
    Label,
    Ident,
    Measure(&'static str),
}

/// Type of a property of a standard `Pset_*Common` set, as the property set definitions give it.
fn standard(set: &str, name: &str) -> Option<Std> {
    if !(set.starts_with("Pset_") && set.ends_with("Common")) {
        return None;
    }
    Some(match name {
        "IsExternal" | "LoadBearing" | "Combustible" | "Compartmentation" | "SelfClosing"
        | "SmokeStop" | "FireExit" | "HandicapAccessible" => Std::Bool,
        "FireRating" | "AcousticRating" | "SurfaceSpreadOfFlame" | "SecurityRating"
        | "Status" => Std::Label,
        "Reference" => Std::Ident,
        "ThermalTransmittance" => Std::Measure("IFCTHERMALTRANSMITTANCEMEASURE"),
        "GlazingAreaFraction" => Std::Measure("IFCPOSITIVERATIOMEASURE"),
        _ => return None,
    })
}

/// Derived from the file itself on import; writing them back would duplicate its sets.
const SKIP: [&str; 4] = ["IFC.", "Material.", "DDC_", "Qto_"];

enum Unit {
    /// Project unit, or a unit the reader takes from the measure type: `$`.
    Plain,
    Si {
        ty: &'static str,
        prefix: &'static str,
        name: &'static str,
    },
    /// `factor` of the project unit `base` (index into the project length/area/volume/angle units).
    Conv {
        ty: &'static str,
        name: &'static str,
        measure: &'static str,
        factor: f64,
        base: usize,
        dims: [i32; 7],
    },
}

const LEN: &str = "IFCLENGTHMEASURE";
const AREA: &str = "IFCAREAMEASURE";
const VOL: &str = "IFCVOLUMEMEASURE";
const ANG: &str = "IFCPLANEANGLEMEASURE";

fn si(ty: &'static str, prefix: &'static str, name: &'static str) -> Unit {
    Unit::Si { ty, prefix, name }
}

fn spec(label: &str) -> Option<(&'static str, Unit)> {
    const L: &str = ".LENGTHUNIT.";
    const A: &str = ".AREAUNIT.";
    const V: &str = ".VOLUMEUNIT.";
    const M: &str = ".MASSUNIT.";
    Some(match label {
        "m" => (LEN, Unit::Plain),
        "m2" => (AREA, Unit::Plain),
        "m3" => (VOL, Unit::Plain),
        "mm" => (LEN, si(L, ".MILLI.", ".METRE.")),
        "cm" => (LEN, si(L, ".CENTI.", ".METRE.")),
        "km" => (LEN, si(L, ".KILO.", ".METRE.")),
        "mm2" => (AREA, si(A, ".MILLI.", ".SQUARE_METRE.")),
        "cm2" => (AREA, si(A, ".CENTI.", ".SQUARE_METRE.")),
        "mm3" => (VOL, si(V, ".MILLI.", ".CUBIC_METRE.")),
        "cm3" => (VOL, si(V, ".CENTI.", ".CUBIC_METRE.")),
        "kg" => ("IFCMASSMEASURE", si(M, ".KILO.", ".GRAM.")),
        "g" => ("IFCMASSMEASURE", si(M, NONE, ".GRAM.")),
        "ft" | "in" => {
            let (name, factor) = if label == "ft" {
                ("FOOT", 0.3048)
            } else {
                ("INCH", 0.0254)
            };
            let u = Unit::Conv {
                ty: L,
                name,
                measure: LEN,
                factor,
                base: 0,
                dims: [1, 0, 0, 0, 0, 0, 0],
            };
            (LEN, u)
        }
        "deg" => (
            ANG,
            Unit::Conv {
                ty: ".PLANEANGLEUNIT.",
                name: "DEGREE",
                measure: ANG,
                factor: std::f64::consts::PI / 180.0,
                base: 3,
                dims: [0; 7],
            },
        ),
        "K" => ("IFCTHERMODYNAMICTEMPERATUREMEASURE", Unit::Plain),
        "W/(m2K)" => ("IFCTHERMALTRANSMITTANCEMEASURE", Unit::Plain),
        _ => return None,
    })
}

type Cache = BTreeMap<String, u32>;

/// Unit entity for `label`, created once; None for `$`.
fn unit_ent(c: &mut Ctx, cache: &mut Cache, base: [u32; 4], label: &str, u: &Unit) -> Option<u32> {
    if let Some(id) = cache.get(label) {
        return Some(*id);
    }
    let id = match u {
        Unit::Plain => return None,
        Unit::Si { ty, prefix, name } => {
            c.s.add("IFCSIUNIT", &[s("*"), s(ty), s(prefix), s(name)])
        }
        Unit::Conv {
            ty,
            name,
            measure,
            factor,
            base: b,
            dims,
        } => {
            let d: Vec<String> = dims.iter().map(|x| x.to_string()).collect();
            let dim = c.s.add("IFCDIMENSIONALEXPONENTS", &d);
            let m = c.s.add(
                "IFCMEASUREWITHUNIT",
                &[format!("{measure}({})", real(*factor)), r(base[*b])],
            );
            c.s.add("IFCCONVERSIONBASEDUNIT", &[r(dim), s(ty), text(name), r(m)])
        }
    };
    cache.insert(label.to_string(), id);
    Some(id)
}

/// `"1; 2.5"` as numbers when writing them back gives exactly this text.
fn numbers(t: &str) -> Option<Vec<f64>> {
    let v: Vec<f64> = t
        .split("; ")
        .map(|x| x.parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()?;
    let back = v
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    (v.len() > 1 && back == t).then_some(v)
}

fn one(c: &mut Ctx, cache: &mut Cache, base: [u32; 4], n: &str, v: &Value, unit: &str) -> Option<u32> {
    let known = spec(unit);
    let typed = |x: f64| match &known {
        Some((m, _)) => format!("{m}({})", real(x)),
        None => format!("IFCREAL({})", real(x)),
    };
    let ent = match v {
        Value::Integer(i) if unit.is_empty() => {
            return Some(c.s.add(
                "IFCPROPERTYSINGLEVALUE",
                &[text(n), s(NONE), format!("IFCINTEGER({i})"), s(NONE)],
            ));
        }
        Value::Text(t) => {
            let nums = numbers(t).filter(|_| unit.is_empty() || known.is_some());
            let Some(xs) = nums else {
                return Some(c.s.add(
                    "IFCPROPERTYSINGLEVALUE",
                    &[text(n), s(NONE), format!("IFCTEXT({})", text(t)), s(NONE)],
                ));
            };
            let items: Vec<String> = xs.into_iter().map(typed).collect();
            let u = match &known {
                Some((_, k)) => unit_ent(c, cache, base, unit, k),
                None => None,
            };
            return Some(c.s.add(
                "IFCPROPERTYLISTVALUE",
                &[
                    text(n),
                    s(NONE),
                    format!("({})", items.join(",")),
                    u.map_or(NONE.to_string(), r),
                ],
            ));
        }
        Value::Integer(i) => (*i as f64, typed(*i as f64)),
        Value::Real(x) if x.is_finite() => (*x, typed(*x)),
        _ => return None,
    };
    let u = match &known {
        Some((_, k)) => unit_ent(c, cache, base, unit, k),
        None => None,
    };
    Some(c.s.add(
        "IFCPROPERTYSINGLEVALUE",
        &[text(n), s(NONE), ent.1, u.map_or(NONE.to_string(), r)],
    ))
}

type Sets = BTreeMap<String, Vec<(String, Value, String)>>;

/// One property set per (element, set name) from the model's `prop` rows.
pub(super) fn write(
    c: &mut Ctx,
    model: &ddc_model::Model,
    base: [u32; 4],
    wall_ext: &BTreeMap<i64, bool>,
) -> Result<(), String> {
    let mut st = model
        .sql()
        .prepare("SELECT kind, ref, key, value, unit FROM prop ORDER BY kind, ref, key")
        .or_else(db)?;
    let rows = st
        .query_map([], |x| {
            Ok((
                x.get::<_, String>(0)?,
                x.get::<_, i64>(1)?,
                x.get::<_, String>(2)?,
                x.get::<_, Value>(3)?,
                x.get::<_, String>(4)?,
            ))
        })
        .or_else(db)?
        .collect::<Result<Vec<_>, _>>()
        .or_else(db)?;
    let mut by: BTreeMap<(String, i64), Sets> = BTreeMap::new();
    for (k, id, key, v, u) in rows {
        if SKIP.iter().any(|p| key.starts_with(p)) || !c.prod.contains_key(&(k.clone(), id)) {
            continue;
        }
        let (set, name) = key.split_once('.').unwrap_or(("Properties", key.as_str()));
        by.entry((k, id))
            .or_default()
            .entry(set.to_string())
            .or_default()
            .push((name.to_string(), v, u));
    }
    // one Pset_WallCommon per wall: IsExternal is merged into it, replacing a stored value
    for (id, ext) in wall_ext {
        if !c.prod.contains_key(&("wall".to_string(), *id)) {
            continue;
        }
        let props = by
            .entry(("wall".to_string(), *id))
            .or_default()
            .entry(WALL_COMMON.to_string())
            .or_default();
        props.retain(|x| x.0 != IS_EXTERNAL);
        props.push((IS_EXTERNAL.to_string(), Value::Integer(i64::from(*ext)), String::new()));
        if super::ext_walls::explicit(model, *id).is_none() {
            by.entry(("wall".to_string(), *id))
                .or_default()
                .entry(super::ext_walls::COMPUTED_SET.to_string())
                .or_default()
                .push((
                    super::ext_walls::COMPUTED_NAME.to_string(),
                    Value::Integer(i64::from(*ext)),
                    String::new(),
                ));
        }
    }
    let mut cache = Cache::new();
    for ((k, id), sets) in by {
        let ent = target(c, &k, id);
        for (set, props) in sets {
            let ids: Vec<u32> = props
                .iter()
                .filter_map(|(n, v, u)| {
                    let computed = set == super::ext_walls::COMPUTED_SET
                        && n == super::ext_walls::COMPUTED_NAME;
                    let kind = if computed {
                        Some(Std::Bool)
                    } else {
                        standard(&set, n)
                    };
                    let typed = match (kind, v) {
                        (Some(Std::Bool), Value::Integer(i)) => {
                            Some(format!("IFCBOOLEAN({})", if *i != 0 { ".T." } else { ".F." }))
                        }
                        (Some(Std::Label), Value::Text(t)) => {
                            Some(format!("IFCLABEL({})", text(t)))
                        }
                        (Some(Std::Ident), Value::Text(t)) => {
                            Some(format!("IFCIDENTIFIER({})", text(t)))
                        }
                        (Some(Std::Measure(m)), Value::Real(x))
                            if u.is_empty() && x.is_finite() =>
                        {
                            Some(format!("{m}({})", real(*x)))
                        }
                        _ => None,
                    };
                    match typed {
                        Some(val) => Some(c.s.add(
                            "IFCPROPERTYSINGLEVALUE",
                            &[text(n), s(NONE), val, s(NONE)],
                        )),
                        None => one(c, &mut cache, base, n, v, u),
                    }
                })
                .collect();
            if ids.is_empty() {
                continue;
            }
            let gk = format!("uprop-{k}-{set}");
            let ps = c.rooted("IFCPROPERTYSET", &gk, id, &set, &[list(&ids)]);
            c.rel(
                "IFCRELDEFINESBYPROPERTIES",
                &format!("{gk}-rel"),
                id,
                &[list(&[ent]), r(ps)],
            );
        }
    }
    params(c, model, base, &mut cache)?;
    depth(c, model)
}

/// One `DDC_Depth` set per element with a design depth: `Depth` (1..5) and `Discipline`.
fn depth(c: &mut Ctx, model: &ddc_model::Model) -> Result<(), String> {
    use ddc_model::depth::{PROP_DEPTH, PROP_DISCIPLINE, PSET};
    let rows = model.elem_depths().map_err(|e| format!("depth: {e}"))?;
    for ((k, id), e) in rows {
        if !c.prod.contains_key(&(k.clone(), id)) {
            continue;
        }
        let ent = target(c, &k, id);
        let ids = [
            c.s.add(
                "IFCPROPERTYSINGLEVALUE",
                &[text(PROP_DEPTH), s(NONE), format!("IFCINTEGER({})", e.depth), s(NONE)],
            ),
            c.s.add(
                "IFCPROPERTYSINGLEVALUE",
                &[
                    text(PROP_DISCIPLINE),
                    s(NONE),
                    format!("IFCLABEL({})", text(e.discipline.as_str())),
                    s(NONE),
                ],
            ),
        ];
        let gk = format!("udepth-{k}");
        let ps = c.rooted("IFCPROPERTYSET", &gk, id, PSET, &[list(&ids)]);
        c.rel(
            "IFCRELDEFINESBYPROPERTIES",
            &format!("{gk}-rel"),
            id,
            &[list(&[ent]), r(ps)],
        );
    }
    Ok(())
}

/// Product that carries the data of `(kind, id)`: a door or window opening has both an
/// IfcOpeningElement and the IfcDoor/IfcWindow filling it; its data belongs to the filling.
fn target(c: &Ctx, kind: &str, id: i64) -> u32 {
    if kind == "opening" {
        if let Some(&e) = c.prod.get(&("opening_fill".to_string(), id)) {
            return e;
        }
    }
    c.prod[&(kind.to_string(), id)]
}

/// Project parameter value as a property; text is IFCLABEL, numbers are typed by the parameter.
fn pvalue(
    c: &mut Ctx,
    cache: &mut Cache,
    base: [u32; 4],
    d: &ddc_model::ParamDef,
    v: &ddc_model::PVal,
) -> Option<u32> {
    use ddc_model::{PVal, ParamType as T};
    let n = d.spec.name.as_str();
    let single = |c: &mut Ctx, val: String| {
        c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), val, s(NONE)])
    };
    match (d.spec.ptype, v) {
        (T::Text, PVal::Text(t)) if !t.is_empty() => Some(single(c, format!("IFCLABEL({})", text(t)))),
        (T::YesNo, PVal::Bool(b)) => Some(single(c, format!("IFCBOOLEAN({})", if *b { ".T." } else { ".F." }))),
        (T::Int, v) => {
            let i = match v {
                PVal::Int(i) => *i,
                PVal::Num(x) if x.is_finite() => x.round() as i64,
                _ => return None,
            };
            Some(single(c, format!("IFCINTEGER({i})")))
        }
        (t @ (T::Number | T::Length | T::Area | T::Volume), v) => {
            let x = v.as_f64().filter(|x| x.is_finite())?;
            let unit = match t {
                T::Length => "m",
                T::Area => "m2",
                T::Volume => "m3",
                _ => d.spec.unit.as_str(),
            };
            one(c, cache, base, n, &Value::Real(x), unit)
        }
        _ => None,
    }
}

/// One `DDC_Parameters` set per element from the project parameter values. Type parameters
/// take the value of the element's type; no type objects are exported, so it lands on every
/// instance of the type. Empty values are not written.
fn params(c: &mut Ctx, model: &ddc_model::Model, base: [u32; 4], cache: &mut Cache) -> Result<(), String> {
    let e = |e: ddc_model::Error| format!("params: {e}");
    let defs = model.params().map_err(e)?;
    let mut by: BTreeMap<(String, i64), Vec<u32>> = BTreeMap::new();
    for d in &defs {
        let vals = model.param_values(d.id).map_err(e)?;
        if vals.is_empty() {
            continue;
        }
        let mut got: Vec<((String, i64), ddc_model::PVal)> = vec![];
        if d.spec.per_type {
            let tables: Vec<&str> = ddc_model::PARAM_CATEGORIES
                .iter()
                .filter(|x| d.spec.categories.iter().any(|k| k == x.0))
                .map(|x| x.1)
                .collect();
            for (k, id) in c.prod.keys() {
                if !tables.contains(&k.as_str()) {
                    continue;
                }
                if let Some((tt, tid)) = model.type_of(k, *id).map_err(e)? {
                    if let Some(v) = vals.get(&(tt.to_string(), tid)) {
                        got.push(((k.clone(), *id), v.clone()));
                    }
                }
            }
        } else {
            for (key, v) in &vals {
                if c.prod.contains_key(key) {
                    got.push((key.clone(), v.clone()));
                }
            }
            // param_values is a HashMap: without this the entity numbering differs per export
            got.sort_by(|a, b| a.0.cmp(&b.0));
        }
        for (key, v) in got {
            if let Some(p) = pvalue(c, cache, base, d, &v) {
                by.entry(key).or_default().push(p);
            }
        }
    }
    for ((k, id), ids) in by {
        let ent = target(c, &k, id);
        let gk = format!("uparam-{k}");
        let ps = c.rooted("IFCPROPERTYSET", &gk, id, "DDC_Parameters", &[list(&ids)]);
        c.rel(
            "IFCRELDEFINESBYPROPERTIES",
            &format!("{gk}-rel"),
            id,
            &[list(&[ent]), r(ps)],
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ifc::{export_ifc, import_ifc};
    use ddc_model::{LayerSpec, Model, WallSpec};
    use rusqlite::types::Value;

    const KEY: &str = ".LENGTHUNIT.,.MILLI.,.METRE.";

    fn wall_model(props: &[(&str, Value, &str)]) -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "ext",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        for (k, v, u) in props {
            m.set_prop("wall", w, k, Some((v.clone(), *u))).unwrap();
        }
        m
    }

    fn back(text: &str) -> Vec<(String, Value, String)> {
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, text.as_bytes()).unwrap();
        let id: i64 = d
            .sql()
            .query_row("SELECT id FROM wall", [], |x| x.get(0))
            .unwrap();
        d.props("wall", id).unwrap()
    }

    #[test]
    fn door_props_attach_to_the_door_not_the_opening() {
        let mut m = wall_model(&[]);
        let w: i64 = m.sql().query_row("SELECT id FROM wall", [], |x| x.get(0)).unwrap();
        let d = m
            .add_opening(
                w,
                &ddc_model::OpeningSpec {
                    along: 1.5,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        m.set_prop("opening", d, "Pset_DoorCommon.FireRating", Some((Value::Text("EI30".into()), "")))
            .unwrap();
        let t = export_ifc(&m).unwrap();
        let id_of = |pat: &str| -> String {
            let l = t.lines().find(|l| l.contains(pat)).unwrap_or_else(|| panic!("{pat}"));
            l.split('=').next().unwrap().to_string()
        };
        let (door, val) = (id_of("=IFCDOOR("), id_of("'FireRating'"));
        let set = t
            .lines()
            .find(|l| l.contains("=IFCPROPERTYSET(") && l.contains(&format!("({val})")))
            .map(|l| l.split('=').next().unwrap().to_string())
            .unwrap();
        let rel = t
            .lines()
            .find(|l| l.contains("=IFCRELDEFINESBYPROPERTIES(") && l.contains(&format!(",{set})")))
            .unwrap();
        assert!(rel.contains(&format!("({door})")), "{rel}");
    }

    fn real_of(p: &[(String, Value, String)], k: &str) -> (f64, String) {
        match p.iter().find(|x| x.0 == k) {
            Some((_, Value::Real(x), u)) => (*x, u.clone()),
            o => panic!("{k}: {o:?} in {p:?}"),
        }
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-9 * b.abs().max(1e-12), "{a} != {b}");
    }

    #[test]
    fn pset_export_own_unit_roundtrip_mm() {
        let m = wall_model(&[
            ("Pset_T.Len", Value::Real(1500.0), "mm"),
            ("Pset_T.Ang", Value::Real(37.5), "deg"),
            ("Pset_T.Mass", Value::Real(2.25), "kg"),
        ]);
        let t = export_ifc(&m).unwrap();
        assert!(t.contains(KEY), "own length unit written");
        let p = back(&t);
        let (x, u) = real_of(&p, "Pset_T.Len");
        near(x, 1.5);
        assert_eq!(u, "m");
        let (x, u) = real_of(&p, "Pset_T.Ang");
        near(x, 37.5);
        assert_eq!(u, "deg");
        let (x, u) = real_of(&p, "Pset_T.Mass");
        near(x, 2.25);
        assert_eq!(u, "kg");
    }

    #[test]
    fn pset_export_project_unit_stays_dollar() {
        let m = wall_model(&[("Pset_T.Len", Value::Real(2.5), "m")]);
        let t = export_ifc(&m).unwrap();
        assert!(t.contains("IFCPROPERTYSINGLEVALUE('Len',$,IFCLENGTHMEASURE(2.5),$)"));
        assert!(!t.contains(KEY));
        near(real_of(&back(&t), "Pset_T.Len").0, 2.5);
    }

    #[test]
    fn pset_list_export_roundtrip() {
        let m = wall_model(&[("Pset_T.Cuts", Value::Text("100; 250.5; 3000".into()), "mm")]);
        let t = export_ifc(&m).unwrap();
        assert!(t.contains("IFCPROPERTYLISTVALUE('Cuts',$,(IFCLENGTHMEASURE(100.)"));
        let p = back(&t);
        let Some((_, Value::Text(s), u)) = p.iter().find(|x| x.0 == "Pset_T.Cuts") else {
            panic!("{p:?}");
        };
        assert_eq!(u, "m");
        let got: Vec<f64> = s.split("; ").map(|x| x.parse().unwrap()).collect();
        for (g, w) in got.iter().zip([0.1, 0.2505, 3.0]) {
            near(*g, w);
        }
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn unit_entities_are_not_duplicated() {
        let m = wall_model(&[
            ("Pset_T.A", Value::Real(1.0), "mm"),
            ("Pset_T.B", Value::Real(2.0), "mm"),
            ("Pset_U.C", Value::Text("1; 2".into()), "mm"),
            ("Pset_U.D", Value::Real(10.0), "deg"),
            ("Pset_U.E", Value::Real(20.0), "deg"),
        ]);
        let t = export_ifc(&m).unwrap();
        assert_eq!(t.matches(KEY).count(), 1);
        assert_eq!(t.matches("IFCCONVERSIONBASEDUNIT(").count(), 1);
    }

    use ddc_model::{PVal, ParamSet, ParamSpec, ParamType};

    fn spec(name: &str, ptype: ParamType, per_type: bool) -> ParamSpec {
        ParamSpec {
            name: name.into(),
            ptype,
            per_type,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        }
    }

    fn put(m: &mut Model, p: i64, kind: &str, id: i64, v: PVal) {
        m.set_param_values(&[ParamSet {
            param: p,
            kind: kind.into(),
            id,
            value: Some(v),
        }])
        .unwrap();
    }

    fn wall_ids(m: &Model) -> (i64, i64) {
        m.sql()
            .query_row("SELECT id, wtype FROM wall", [], |x| Ok((x.get(0)?, x.get(1)?)))
            .unwrap()
    }

    #[test]
    fn export_writes_imported_pset_values() {
        let m = wall_model(&[
            ("Pset_T.Note", Value::Text("a b".into()), ""),
            ("Pset_T.Count", Value::Integer(4), ""),
            ("Pset_T.Len", Value::Real(1500.0), "mm"),
        ]);
        let t = export_ifc(&m).unwrap();
        assert!(t.contains("IFCPROPERTYSINGLEVALUE('Note',$,IFCTEXT('a b'),$)"));
        assert!(t.contains("IFCPROPERTYSINGLEVALUE('Count',$,IFCINTEGER(4),$)"));
        let p = back(&t);
        assert!(p.iter().any(|x| x.0 == "Pset_T.Note" && x.1 == Value::Text("a b".into())));
        assert!(p.iter().any(|x| x.0 == "Pset_T.Count" && x.1 == Value::Integer(4)));
        near(real_of(&p, "Pset_T.Len").0, 1.5);
    }

    #[test]
    fn export_writes_user_params_pset() {
        let mut m = wall_model(&[]);
        let (w, _) = wall_ids(&m);
        let mark = m.add_param(&spec("Mark", ParamType::Text, false)).unwrap();
        let len = m.add_param(&spec("Reach", ParamType::Length, false)).unwrap();
        let area = m.add_param(&spec("Face", ParamType::Area, false)).unwrap();
        let vol = m.add_param(&spec("Fill", ParamType::Volume, false)).unwrap();
        let ok = m.add_param(&spec("Checked", ParamType::YesNo, false)).unwrap();
        let n = m.add_param(&spec("Qty", ParamType::Int, false)).unwrap();
        let x = m.add_param(&spec("Factor", ParamType::Number, false)).unwrap();
        let empty = m.add_param(&spec("Blank", ParamType::Text, false)).unwrap();
        put(&mut m, mark, "wall", w, PVal::Text("W-01".into()));
        put(&mut m, len, "wall", w, PVal::Num(2.5));
        put(&mut m, area, "wall", w, PVal::Num(18.0));
        put(&mut m, vol, "wall", w, PVal::Num(3.6));
        put(&mut m, ok, "wall", w, PVal::Bool(true));
        put(&mut m, n, "wall", w, PVal::Int(7));
        put(&mut m, x, "wall", w, PVal::Num(1.25));
        put(&mut m, empty, "wall", w, PVal::Text(String::new()));
        let t = export_ifc(&m).unwrap();
        assert_eq!(t.matches("'DDC_Parameters'").count(), 1);
        for want in [
            "IFCPROPERTYSINGLEVALUE('Mark',$,IFCLABEL('W-01'),$)",
            "IFCPROPERTYSINGLEVALUE('Reach',$,IFCLENGTHMEASURE(2.5),$)",
            "IFCPROPERTYSINGLEVALUE('Face',$,IFCAREAMEASURE(18.),$)",
            "IFCPROPERTYSINGLEVALUE('Fill',$,IFCVOLUMEMEASURE(3.6),$)",
            "IFCPROPERTYSINGLEVALUE('Checked',$,IFCBOOLEAN(.T.),$)",
            "IFCPROPERTYSINGLEVALUE('Qty',$,IFCINTEGER(7),$)",
            "IFCPROPERTYSINGLEVALUE('Factor',$,IFCREAL(1.25),$)",
        ] {
            assert!(t.contains(want), "missing {want}");
        }
        assert!(!t.contains("'Blank'"), "empty value is not written");
    }

    #[test]
    fn ifc_roundtrip_keeps_user_param_value() {
        let mut m = wall_model(&[]);
        let (w, _) = wall_ids(&m);
        let mark = m.add_param(&spec("Mark", ParamType::Text, false)).unwrap();
        let len = m.add_param(&spec("Reach", ParamType::Length, false)).unwrap();
        put(&mut m, mark, "wall", w, PVal::Text("W-01".into()));
        put(&mut m, len, "wall", w, PVal::Num(2.5));
        let t = export_ifc(&m).unwrap();
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let (dw, _) = wall_ids(&d);
        let dm = d.param_by_name("Mark").unwrap().expect("Mark parameter");
        assert_eq!(dm.spec.ptype, ParamType::Text);
        assert_eq!(
            d.param_value(dm.id, "wall", dw).unwrap(),
            Some(PVal::Text("W-01".into()))
        );
        let dl = d.param_by_name("Reach").unwrap().expect("Reach parameter");
        assert_eq!(dl.spec.ptype, ParamType::Length);
        assert_eq!(d.param_value(dl.id, "wall", dw).unwrap(), Some(PVal::Num(2.5)));
        let p = d.props("wall", dw).unwrap();
        assert!(p.iter().all(|x| !x.0.starts_with("DDC_Parameters.")), "{p:?}");
    }

    #[test]
    fn ifc_export_carries_depth_as_property_and_import_reads_it_back() {
        use ddc_model::{Discipline, ElemDepth};
        let mut m = wall_model(&[]);
        let (w, _) = wall_ids(&m);
        m.set_elem_depth(&[("wall", w)], 3, Some(Discipline::Struct)).unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(t.matches("'DDC_Depth'").count(), 1);
        assert!(t.contains("IFCPROPERTYSINGLEVALUE('Depth',$,IFCINTEGER(3),$)"), "depth");
        assert!(t.contains("IFCPROPERTYSINGLEVALUE('Discipline',$,IFCLABEL('struct'),$)"));
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let (dw, _) = wall_ids(&d);
        assert_eq!(
            d.elem_depth("wall", dw).unwrap(),
            Some(ElemDepth { depth: 3, discipline: Discipline::Struct })
        );
        // the set is the depth, not a generic property, and leaves with the import's undo
        let p = d.props("wall", dw).unwrap();
        assert!(p.iter().all(|x| !x.0.starts_with("DDC_Depth.")), "{p:?}");
        d.undo().unwrap();
        let rows: i64 = d
            .sql()
            .query_row("SELECT count(*) FROM elem_depth", [], |x| x.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        // an element without a depth writes no set
        let plain = export_ifc(&wall_model(&[])).unwrap();
        assert!(!plain.contains("'DDC_Depth'"));
    }

    #[test]
    fn per_type_value_written_once() {
        let mut m = wall_model(&[]);
        let (w1, ty) = wall_ids(&m);
        let lv: i64 = m.sql().query_row("SELECT level FROM wall", [], |x| x.get(0)).unwrap();
        let w2 = m
            .add_wall(&WallSpec {
                a: (0.0, 5.0),
                b: (6.0, 5.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ty,
                offset0: None,
            })
            .unwrap();
        let fire = m.add_param(&spec("Fire", ParamType::Text, true)).unwrap();
        put(&mut m, fire, "wall_type", ty, PVal::Text("EI60".into()));
        let t = export_ifc(&m).unwrap();
        let one = "IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI60'),$)";
        assert_eq!(t.matches(one).count(), 2, "one property per wall, none extra");
        assert_eq!(t.matches("'DDC_Parameters'").count(), 2);
        let _ = (w1, w2);
    }

    /// Rows of the parameter tables with their rowids: undo must give them back exactly.
    fn ptables(m: &Model) -> Vec<String> {
        let mut out = vec![];
        for t in ["pparam", "pparam_cat", "pparam_val"] {
            let mut st = m
                .sql()
                .prepare(&format!("SELECT rowid, * FROM {t} ORDER BY rowid"))
                .unwrap();
            let n = st.column_count();
            let rows = st
                .query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect::<Result<Vec<_>, _>>())
                .unwrap();
            for r in rows {
                out.push(format!("{t}:{:?}", r.unwrap()));
            }
        }
        out
    }

    fn ops(m: &Model) -> i64 {
        m.sql().query_row("SELECT count(*) FROM op", [], |x| x.get(0)).unwrap()
    }

    fn marked() -> String {
        let mut m = wall_model(&[]);
        let (w, _) = wall_ids(&m);
        let mark = m.add_param(&spec("Mark", ParamType::Text, false)).unwrap();
        let len = m.add_param(&spec("Reach", ParamType::Length, false)).unwrap();
        put(&mut m, mark, "wall", w, PVal::Text("W-01".into()));
        put(&mut m, len, "wall", w, PVal::Num(2.5));
        export_ifc(&m).unwrap()
    }

    #[test]
    fn ifc_import_undo_removes_param_values() {
        let t = marked();
        let mut d = Model::open(":memory:").unwrap();
        let mut s = spec("Mark", ParamType::Text, false);
        s.categories = vec!["door".into()];
        let mark = d.add_param(&s).unwrap();
        let before = ptables(&d);
        let n = ops(&d);
        import_ifc(&mut d, t.as_bytes()).unwrap();
        assert_eq!(ops(&d), n + 1, "one journal entry");
        let (dw, _) = wall_ids(&d);
        assert_eq!(d.param_value(mark, "wall", dw).unwrap(), Some(PVal::Text("W-01".into())));
        let cats = d.param_by_name("Mark").unwrap().unwrap().spec.categories;
        assert!(cats.iter().any(|c| c == "wall") && cats.iter().any(|c| c == "door"), "{cats:?}");
        d.undo().unwrap();
        assert_eq!(ops(&d), n);
        assert_eq!(ptables(&d), before, "values and added category undone");
        // redo runs the import again
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let (dw, _) = wall_ids(&d);
        assert_eq!(d.param_value(mark, "wall", dw).unwrap(), Some(PVal::Text("W-01".into())));
    }

    #[test]
    fn ifc_import_undo_removes_created_params() {
        let t = marked();
        let mut d = Model::open(":memory:").unwrap();
        let before = ptables(&d);
        import_ifc(&mut d, t.as_bytes()).unwrap();
        assert!(d.param_by_name("Mark").unwrap().is_some());
        assert!(d.param_by_name("Reach").unwrap().is_some());
        d.undo().unwrap();
        assert_eq!(ptables(&d), before);
        assert!(d.params().unwrap().is_empty());
        let walls: i64 = d.sql().query_row("SELECT count(*) FROM wall", [], |x| x.get(0)).unwrap();
        assert_eq!(walls, 0);
    }

    /// Source model with Mark/Reach set on its wall, and the model it is imported into.
    fn linked() -> (Model, Model, i64, i64, i64) {
        let mut m = wall_model(&[]);
        let (w, _) = wall_ids(&m);
        let mark = m.add_param(&spec("Mark", ParamType::Text, false)).unwrap();
        let len = m.add_param(&spec("Reach", ParamType::Length, false)).unwrap();
        put(&mut m, mark, "wall", w, PVal::Text("W-01".into()));
        put(&mut m, len, "wall", w, PVal::Num(2.5));
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        (m, d, w, mark, len)
    }

    fn val(d: &Model, name: &str) -> Option<PVal> {
        let (dw, _) = wall_ids(d);
        let id = d.param_by_name(name).unwrap().unwrap().id;
        d.param_value(id, "wall", dw).unwrap()
    }

    fn set_here(d: &mut Model, name: &str, v: PVal) {
        let (dw, _) = wall_ids(d);
        let id = d.param_by_name(name).unwrap().unwrap().id;
        put(d, id, "wall", dw, v);
    }

    const MARK_FIELD: &str = "p:DDC_Parameters.Mark";

    #[test]
    fn first_import_sets_base() {
        let (_, mut d, _, _, _) = linked();
        let (dw, _) = wall_ids(&d);
        let diff = d.element_diff("wall", dw).unwrap();
        assert!(diff.iter().all(|f| !f.field.starts_with(MARK_FIELD)), "{diff:?}");
        set_here(&mut d, "Mark", PVal::Text("LOCAL".into()));
        let diff = d.element_diff("wall", dw).unwrap();
        let f = diff.iter().find(|f| f.field == MARK_FIELD).expect("local edit seen against base");
        assert_eq!(f.mine, serde_json::json!("LOCAL"));
    }

    #[test]
    fn reimport_keeps_local_param_edit() {
        let (m, mut d, _, _, _) = linked();
        set_here(&mut d, "Mark", PVal::Text("LOCAL".into()));
        set_here(&mut d, "Reach", PVal::Num(9.0));
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        let walls: i64 = d.sql().query_row("SELECT count(*) FROM wall", [], |x| x.get(0)).unwrap();
        assert_eq!(walls, 1, "re-import links the same wall");
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("LOCAL".into())));
        assert_eq!(val(&d, "Reach"), Some(PVal::Num(9.0)));
        assert_eq!(r.conflicts, 0);
        assert!(r.kept_local >= 2, "{r:?}");
        let (dw, _) = wall_ids(&d);
        let p = d.props("wall", dw).unwrap();
        assert!(p.iter().all(|x| !x.0.starts_with("DDC_Parameters.")), "{p:?}");
    }

    #[test]
    fn reimport_takes_file_change() {
        let (mut m, mut d, w, mark, _) = linked();
        put(&mut m, mark, "wall", w, PVal::Text("W-02".into()));
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("W-02".into())));
        assert_eq!(val(&d, "Reach"), Some(PVal::Num(2.5)));
        assert_eq!(r.conflicts, 0);
        // the base moved to the file value: a later local edit is kept on the next re-import
        set_here(&mut d, "Mark", PVal::Text("LOCAL".into()));
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("LOCAL".into())));
    }

    #[test]
    fn reimport_both_changed_is_conflict_keeps_local() {
        let (mut m, mut d, w, mark, _) = linked();
        set_here(&mut d, "Mark", PVal::Text("LOCAL".into()));
        put(&mut m, mark, "wall", w, PVal::Text("W-02".into()));
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("LOCAL".into())));
        assert_eq!(r.conflicts, 1, "{r:?}");
        let (dw, _) = wall_ids(&d);
        let c = d.conflicts(None).unwrap();
        let c = c.iter().find(|c| c.field == MARK_FIELD).expect("conflict listed");
        assert_eq!((c.kind.as_str(), c.element), ("wall", dw));
        assert_eq!(c.mine, serde_json::json!("LOCAL"));
        assert_eq!(c.theirs, serde_json::json!("W-02"));
        // base is the file value now: the same file again adds no new decision
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("LOCAL".into())));
        assert_eq!(r.conflicts, 0, "{r:?}");
    }

    #[test]
    fn ifc_reimport_skips_locked_param_keeps_rest() {
        let (mut m, mut d, w, mark, len) = linked();
        let (dw, _) = wall_ids(&d);
        d.set_param_lock(&[("wall", dw, "Mark")]).unwrap();
        put(&mut m, mark, "wall", w, PVal::Text("W-02".into()));
        put(&mut m, len, "wall", w, PVal::Num(4.0));
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("W-01".into())), "locked value kept");
        assert_eq!(val(&d, "Reach"), Some(PVal::Num(4.0)), "the rest of the file is taken");
        assert_eq!(r.conflicts, 0, "{r:?}");
        assert_eq!(r.locked, vec![("wall".to_string(), dw, "Mark".to_string())]);
        assert!(d.param_locked("wall", dw, "Mark").unwrap());
        // the same file again: still kept and still reported, never a conflict
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("W-01".into())));
        assert_eq!((r.conflicts, r.locked.len()), (0, 1), "{r:?}");
    }

    #[test]
    fn undo_restores_all() {
        let (mut m, mut d, w, mark, len) = linked();
        set_here(&mut d, "Reach", PVal::Num(9.0));
        put(&mut m, mark, "wall", w, PVal::Text("W-02".into()));
        put(&mut m, len, "wall", w, PVal::Num(3.5));
        let before = ptables(&d);
        let conflicts = d.conflicts(None).unwrap().len();
        let n = ops(&d);
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(ops(&d), n + 1, "one journal entry");
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("W-02".into())), "taken");
        assert_eq!(val(&d, "Reach"), Some(PVal::Num(9.0)), "conflict keeps local");
        assert_eq!(r.conflicts, 1);
        d.undo().unwrap();
        assert_eq!(ops(&d), n);
        assert_eq!(ptables(&d), before, "values back with their rowids");
        assert_eq!(d.conflicts(None).unwrap().len(), conflicts, "conflict undone");
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("W-01".into())));
        // base undone too: the same re-import decides the same way again
        let r = import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        assert_eq!(val(&d, "Mark"), Some(PVal::Text("W-02".into())));
        assert_eq!(r.conflicts, 1);
    }

    #[test]
    fn number_param_with_unit_roundtrips() {
        let mut m = wall_model(&[]);
        let (w, _) = wall_ids(&m);
        let mut gap = spec("Gap", ParamType::Number, false);
        gap.unit = "mm".into();
        let mut ratio = spec("Ratio", ParamType::Number, false);
        ratio.unit = "m".into();
        let g = m.add_param(&gap).unwrap();
        let r = m.add_param(&ratio).unwrap();
        put(&mut m, g, "wall", w, PVal::Num(1500.0));
        put(&mut m, r, "wall", w, PVal::Num(2.0));
        let t = export_ifc(&m).unwrap();
        assert!(t.contains("IFCPROPERTYSINGLEVALUE('Ratio',$,IFCLENGTHMEASURE(2.),$)"), "measure written");
        let mut d = Model::open(":memory:").unwrap();
        let dg = d.add_param(&gap).unwrap();
        let dr = d.add_param(&ratio).unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let (dw, _) = wall_ids(&d);
        let defs = d.params().unwrap();
        assert_eq!(defs.len(), 2, "no Length twin created: {defs:?}");
        let num = |id: i64| match d.param_value(id, "wall", dw).unwrap() {
            Some(PVal::Num(x)) => x,
            o => panic!("{o:?}"),
        };
        near(num(dg), 1500.0);
        near(num(dr), 2.0);
        let p = d.props("wall", dw).unwrap();
        assert!(p.iter().all(|x| !x.0.starts_with("DDC_Parameters.")), "{p:?}");
    }
}
