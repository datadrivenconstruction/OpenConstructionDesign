// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `DDC_Provenance` per element: the recipe and part that built it, whether a person edited it
//! since, the source file of an import and how many values the assistant set, verified or not.
//! An element with none of these gets no set.
use super::super::step::{list, r, text};
use super::{s, Ctx, NONE};

fn label(v: &str) -> String {
    format!("IFCLABEL({})", text(v))
}

pub(super) fn write(c: &mut Ctx, model: &ddc_model::Model) {
    let by = crate::provenance::read(model, |k, id| c.prod.contains_key(&(k.to_string(), id)));
    for ((k, id), p) in by {
        let fields: Vec<(&str, String)> = crate::provenance::fields(&p)
            .into_iter()
            .map(|(n, v)| {
                let v = match v {
                    serde_json::Value::Bool(b) => format!("IFCBOOLEAN({})", if b { ".T." } else { ".F." }),
                    serde_json::Value::Number(n) => format!("IFCINTEGER({n})"),
                    v => label(v.as_str().unwrap_or_default()),
                };
                (n, v)
            })
            .collect();
        if fields.is_empty() {
            continue;
        }
        let ids: Vec<u32> = fields
            .iter()
            .map(|(n, v)| c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v.clone(), s(NONE)]))
            .collect();
        let ent = super::userprops::target(c, &k, id);
        let gk = format!("prov-{k}");
        let set = c.rooted("IFCPROPERTYSET", &gk, id, "DDC_Provenance", &[list(&ids)]);
        c.rel("IFCRELDEFINESBYPROPERTIES", &format!("{gk}-rel"), id, &[list(&[ent]), r(set)]);
    }
}

#[cfg(test)]
mod tests {
    use crate::ifc::export_ifc;
    use ddc_model::{Edit, LayerSpec, Model, PVal, ParamSpec, ParamType, SrcKind, ValueSrc, WallSpec};

    fn wall(m: &mut Model, x: f64) -> i64 {
        let lv = m.add_level("L1", 0.0).unwrap();
        let layers = [LayerSpec { material: "brick".into(), thickness: 0.2 }];
        let t = m.add_wall_type("ext", &layers).unwrap();
        let spec = WallSpec {
            a: (x, 0.0),
            b: (x + 6.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&spec).unwrap()
    }

    #[test]
    fn assistant_values_go_out_counted_and_a_plain_wall_gets_no_set() {
        let mut m = Model::open(":memory:").unwrap();
        let w = wall(&mut m, 0.0);
        for name in ["Fire", "U"] {
            let spec = ParamSpec {
                name: name.into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            };
            m.add_param(&spec).unwrap();
            let edit = Edit::ParamValue { param: name.into(), kind: "wall".into(), id: w, value: Some(PVal::Text("x".into())) };
            m.apply_edits(&[edit]).unwrap();
        }
        let src = ValueSrc::ai(SrcKind::Ai, 100, "s", "r1", "fill fire rating");
        m.record_value_src(&[("wall", w, "Fire"), ("wall", w, "U")], &src).unwrap();
        m.verify_values(&[("wall", w, "U")], 200).unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(t.matches("'DDC_Provenance'").count(), 1, "{t}");
        assert!(t.contains("'AiValues',$,IFCINTEGER(2)"), "{t}");
        assert!(t.contains("'AiValuesUnverified',$,IFCINTEGER(1)"), "{t}");

        let mut plain = Model::open(":memory:").unwrap();
        wall(&mut plain, 0.0);
        assert!(!export_ifc(&plain).unwrap().contains("DDC_Provenance"));
    }
}
