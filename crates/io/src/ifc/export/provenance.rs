//! `DDC_Provenance` per element: the recipe and part that built it, whether a person edited it
//! since, the source file of an import and how many values the assistant set, verified or not.
//! An element with none of these gets no set.
use std::collections::BTreeMap;

use super::super::step::{list, r, text};
use super::{s, Ctx, NONE};

#[derive(Default)]
struct Prov {
    recipe: Option<(String, String, bool)>,
    import: Option<String>,
    ai: (i64, i64),
}

fn label(v: &str) -> String {
    format!("IFCLABEL({})", text(v))
}

fn read(c: &Ctx, model: &ddc_model::Model) -> BTreeMap<(String, i64), Prov> {
    let mut by: BTreeMap<(String, i64), Prov> = BTreeMap::new();
    let live = |k: &str, id: i64| c.prod.contains_key(&(k.to_string(), id));
    for rec in model.recipes().unwrap_or_default() {
        let edited = model.recipe_held(rec.id).map(|h| h.edited).unwrap_or_default();
        for p in model.recipe_parts(rec.id).unwrap_or_default() {
            if live(&p.tbl, p.el) {
                let e = edited.contains(&p.key);
                by.entry((p.tbl, p.el)).or_default().recipe = Some((rec.name.clone(), p.key, e));
            }
        }
    }
    let db = model.sql();
    // tables are made on first use: an older or empty model may not have them
    if let Ok(mut st) = db.prepare("SELECT kind, ref, source FROM ext_ref WHERE source<>'' ORDER BY guid") {
        let rows = st.query_map([], |x| Ok((x.get::<_, String>(0)?, x.get::<_, i64>(1)?, x.get::<_, String>(2)?)));
        for (k, id, src) in rows.into_iter().flatten().flatten() {
            if live(&k, id) {
                by.entry((k, id)).or_default().import.get_or_insert(src);
            }
        }
    }
    // only values that still hold something, as the editor counts them
    for ((k, id, _), src) in model.value_srcs().unwrap_or_default() {
        if src.kind.by_ai() && live(&k, id) {
            let p = by.entry((k, id)).or_default();
            p.ai.0 += 1;
            p.ai.1 += i64::from(!src.verified());
        }
    }
    by
}

pub(super) fn write(c: &mut Ctx, model: &ddc_model::Model) {
    for ((k, id), p) in read(c, model) {
        let mut fields: Vec<(&str, String)> = vec![];
        if let Some((name, part, edited)) = &p.recipe {
            fields.push(("Recipe", label(name)));
            fields.push(("RecipePart", label(part)));
            let b = if *edited { ".T." } else { ".F." };
            fields.push(("EditedByHand", format!("IFCBOOLEAN({b})")));
        }
        if let Some(src) = &p.import {
            fields.push(("ImportSource", label(src)));
        }
        if p.ai.0 > 0 {
            fields.push(("AiValues", format!("IFCINTEGER({})", p.ai.0)));
            fields.push(("AiValuesUnverified", format!("IFCINTEGER({})", p.ai.1)));
        }
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
