// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Component records for the writers (build package, drawing blocks, glTF extras, schedules):
//! one row per instance with its family, type, category, IFC class, resolved parameters (type
//! and instance, in definition order) and classification codes. One read for all writers, so
//! every format names a component the same way.

use std::collections::HashMap;
use std::sync::Arc;

use ddc_geom::component::{Family, Kind, Value};
use rusqlite::OptionalExtension;

use crate::components::{CompHost, Placement};
use crate::params::PVal;
use crate::{Error, Model};

/// Parameter name prefix of a classification system (`Classification.<system>`).
pub const CLASSIFICATION_PREFIX: &str = "Classification.";

/// One resolved parameter of an instance.
#[derive(Clone, Debug, PartialEq)]
pub struct CompParam {
    pub name: String,
    pub kind: Kind,
    /// Instance parameter (else a type parameter).
    pub instance: bool,
    /// SI number or text.
    pub value: Value,
}

/// One component instance as the writers see it.
#[derive(Clone, Debug, PartialEq)]
pub struct CompRecord {
    pub id: i64,
    pub level: i64,
    pub family_id: i64,
    pub family: String,
    pub type_id: i64,
    pub type_name: String,
    pub category: &'static str,
    pub hosting: &'static str,
    pub ifc_class: String,
    pub ifc_predefined: String,
    pub host: CompHost,
    /// World placement (from the derived quantities).
    pub placement: Placement,
    /// Instance assignments as stored (`Key=value, ...`).
    pub vals: String,
    pub params: Vec<CompParam>,
    /// (system, code), systems sorted.
    pub classification: Vec<(String, String)>,
}

impl CompRecord {
    /// Value of a parameter by name.
    pub fn param(&self, name: &str) -> Option<&CompParam> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// SI unit label of a parameter kind (`""` for unitless and text).
pub fn unit_of(k: Kind) -> &'static str {
    match k {
        Kind::Length => "m",
        Kind::Angle => "rad",
        Kind::Area => "m2",
        Kind::Volume => "m3",
        _ => "",
    }
}

/// Plain text of a value for attribute and cell output: numbers with up to 6 decimals, no
/// trailing zeros; text as is.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Text(t) => t.clone(),
        Value::Num(x) => {
            let s = format!("{:.6}", if x.abs() < 5e-7 { 0.0 } else { *x });
            let s = s.trim_end_matches('0').trim_end_matches('.');
            s.to_string()
        }
    }
}

fn pval_text(v: &PVal) -> String {
    match v {
        PVal::Text(t) => t.clone(),
        PVal::Int(i) => i.to_string(),
        PVal::Num(x) => value_text(&Value::Num(*x)),
        PVal::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
    }
}

fn fam_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

/// Every parameter of a family resolved for type values `tv` and instance assignments `iv`.
pub fn resolved_params(
    f: &Family,
    tv: &[(String, Value)],
    iv: &[(String, Value)],
) -> Result<Vec<CompParam>, Error> {
    let r = f.resolve(tv, iv).map_err(fam_err)?;
    Ok(f.params
        .iter()
        .map(|p| CompParam {
            name: p.name.clone(),
            kind: p.kind,
            instance: p.instance,
            value: if p.kind.is_text() {
                Value::Text(r.texts.get(&p.name).cloned().unwrap_or_default())
            } else {
                Value::Num(r.num(&p.name).unwrap_or(0.0))
            },
        })
        .collect())
}

type RawRow = (i64, i64, i64, String, Option<i64>, Option<i64>, Option<i64>, Placement);

impl Model {
    /// Classification codes of every component: id -> (system, code), systems sorted. Codes are
    /// project parameters named `Classification.<system>` (on the instance or its type).
    pub fn component_classifications(&self) -> Result<HashMap<i64, Vec<(String, String)>>, Error> {
        let defs: Vec<_> = self
            .params()?
            .into_iter()
            .filter(|d| d.spec.name.starts_with(CLASSIFICATION_PREFIX))
            .collect();
        let mut out: HashMap<i64, Vec<(String, String)>> = HashMap::new();
        if defs.is_empty() {
            return Ok(out);
        }
        let ids: Vec<i64> = {
            let mut st = self.db.prepare("SELECT id FROM comp ORDER BY id")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        for d in &defs {
            let system = d.spec.name[CLASSIFICATION_PREFIX.len()..].to_string();
            for &id in &ids {
                if let Some(v) = self.param_value_of(d, "comp", id)? {
                    let code = pval_text(&v);
                    if !code.is_empty() {
                        out.entry(id).or_default().push((system.clone(), code));
                    }
                }
            }
        }
        for v in out.values_mut() {
            v.sort();
        }
        Ok(out)
    }

    /// Records of the components on `level` (None: all), by id. An instance whose values do not
    /// resolve is an error naming it: the writers never drop a component silently.
    pub fn component_records(&self, level: Option<i64>) -> Result<Vec<CompRecord>, Error> {
        let raw: Vec<RawRow> = {
            let mut st = self.db.prepare(
                "SELECT c.id, c.level, c.ctype, c.vals, c.host_wall, c.host_slab, c.host_ceiling, \
                 q.wx, q.wy, q.wz, q.wrot, q.wmirror, c.tilt FROM comp c JOIN comp_qty q ON q.comp=c.id \
                 WHERE ?1 IS NULL OR c.level=?1 ORDER BY c.id",
            )?;
            let v = st
                .query_map([level], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        Placement {
                            origin: (r.get(7)?, r.get(8)?, r.get(9)?),
                            rotation: r.get(10)?,
                            mirror: r.get::<_, i64>(11)? != 0,
                            tilt: crate::components::tilt_parse(r.get(12)?),
                        },
                    ))
                })?
                .collect::<Result<_, _>>()?;
            v
        };
        let classes = self.component_classifications()?;
        let mut fams: HashMap<i64, (String, Arc<Family>)> = HashMap::new();
        let mut types: HashMap<i64, (i64, String, Vec<(String, Value)>)> = HashMap::new();
        let mut out = Vec::with_capacity(raw.len());
        for (id, lvl, ctype, vals, hw, hs, hc, placement) in raw {
            if !types.contains_key(&ctype) {
                let (fid, name, tvals): (i64, String, String) = self
                    .db
                    .query_row(
                        "SELECT family, name, vals FROM comp_type WHERE id=?1",
                        [ctype],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?
                    .ok_or_else(|| Error::Invalid(format!("no component type {ctype}")))?;
                if !fams.contains_key(&fid) {
                    let (fname, def): (String, String) = self.db.query_row(
                        "SELECT name, def FROM comp_family WHERE id=?1",
                        [fid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    fams.insert(fid, (fname, Arc::new(Family::parse(&def).map_err(fam_err)?)));
                }
                let f = &fams[&fid].1;
                let tv = f.assignments(&tvals).map_err(fam_err)?;
                types.insert(ctype, (fid, name, tv));
            }
            let (fid, tname, tv) = &types[&ctype];
            let (fname, f) = &fams[fid];
            let iv = f
                .assignments(&vals)
                .map_err(|e| Error::Invalid(format!("component {id}: {e}")))?;
            let params = resolved_params(f, tv, &iv)
                .map_err(|e| Error::Invalid(format!("component {id}: {e}")))?;
            let host = match (hw, hs, hc) {
                (Some(w), _, _) => CompHost::Wall(w),
                (_, Some(s), _) => CompHost::Floor(s),
                (_, _, Some(c)) => CompHost::Ceiling(c),
                _ => CompHost::None,
            };
            out.push(CompRecord {
                id,
                level: lvl,
                family_id: *fid,
                family: fname.clone(),
                type_id: ctype,
                type_name: tname.clone(),
                category: f.category.name(),
                hosting: f.hosting.name(),
                ifc_class: f.ifc.0.clone(),
                ifc_predefined: f.ifc.1.clone(),
                host,
                placement,
                vals,
                params,
                classification: classes.get(&id).cloned().unwrap_or_default(),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompSpec, ParamSet, ParamSpec, ParamType};

    fn with_table() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L1", 0.0).unwrap();
        let fam = m.load_library_family("Table").unwrap();
        let ctype = m.comp_types(fam).unwrap()[0].0;
        let mut s = CompSpec::at(level, ctype, 2.0, 3.0);
        s.rotation = 0.5;
        let id = m.add_component(&s).unwrap();
        (m, fam, id)
    }

    #[test]
    fn records_carry_family_type_class_params_and_placement() {
        let (m, fam, id) = with_table();
        let f = m.family(fam).unwrap();
        let r = m.component_records(None).unwrap();
        assert_eq!(r.len(), 1);
        let r = &r[0];
        assert_eq!((r.id, r.family.as_str()), (id, "Table"));
        assert_eq!(r.category, "furniture");
        assert_eq!((r.ifc_class.as_str(), r.hosting), (f.ifc.0.as_str(), "none"));
        assert_eq!(r.params.len(), f.params.len());
        for (p, d) in r.params.iter().zip(&f.params) {
            assert_eq!((p.name.as_str(), p.instance, p.kind), (d.name.as_str(), d.instance, d.kind));
        }
        assert!((r.placement.origin.0 - 2.0).abs() < 1e-9);
        assert!((r.placement.rotation - 0.5).abs() < 1e-9);
        assert!(r.classification.is_empty());
    }

    #[test]
    fn classification_codes_come_from_classification_parameters() {
        let (mut m, _, id) = with_table();
        let pid = m
            .add_param(&ParamSpec {
                name: format!("{CLASSIFICATION_PREFIX}Uniclass"),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["comp".into()],
            })
            .unwrap();
        m.set_param_values(&[ParamSet {
            param: pid,
            kind: "comp".into(),
            id,
            value: Some(PVal::Text("Pr_40_50_12".into())),
        }])
        .unwrap();
        let r = m.component_records(None).unwrap();
        assert_eq!(r[0].classification, vec![("Uniclass".into(), "Pr_40_50_12".into())]);
    }

    #[test]
    fn value_text_is_short_and_stable() {
        assert_eq!(value_text(&Value::Num(0.9)), "0.9");
        assert_eq!(value_text(&Value::Num(2.0)), "2");
        assert_eq!(value_text(&Value::Num(-1e-9)), "0");
        assert_eq!(value_text(&Value::Text("Oak".into())), "Oak");
    }
}
