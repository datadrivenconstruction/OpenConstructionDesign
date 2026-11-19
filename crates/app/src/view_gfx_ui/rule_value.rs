//! Filter rule value in the window: shown in project units on screen, stored in SI in the model.

use ddc_model::{
    rule_number, rule_quantity, FilterRule, ParamDef, ParamType, RuleOp, ViewFilterSpec,
};

use crate::schedule::Kind;
use crate::units::{area_in_units, edit_length, parse_edit_length, volume_in_units, Units};

/// (parameter, shown, stored): an unchanged rule does not lose precision to display rounding.
pub(super) type Known = Vec<(String, String, String)>;

/// Quantity kind, if the rule value is compared as a quantity of this parameter.
fn quantity(r: &FilterRule, defs: &[ParamDef]) -> Option<ParamType> {
    if !r.op.takes_value() || matches!(r.op, RuleOp::Contains | RuleOp::NotContains) {
        return None;
    }
    let t = defs
        .iter()
        .find(|p| p.spec.name == r.param.trim())?
        .spec
        .ptype;
    matches!(t, ParamType::Length | ParamType::Area | ParamType::Volume).then_some(t)
}

fn rounded(x: f64) -> String {
    ((x * 1e6).round() / 1e6).to_string()
}

fn shown(r: &FilterRule, defs: &[ParamDef], u: Units) -> String {
    let Some(t) = quantity(r, defs) else {
        return r.value.clone();
    };
    let Some(si) = rule_quantity(&r.value).or_else(|| rule_number(&r.value)) else {
        return r.value.clone();
    };
    match t {
        ParamType::Length => edit_length(si, u),
        ParamType::Area => rounded(area_in_units(si, u)),
        _ => rounded(volume_in_units(si, u)),
    }
}

/// Field text → SI; an explicitly written unit takes precedence over project units.
fn to_si(raw: &str, t: ParamType, u: Units) -> Option<String> {
    let raw = raw.trim();
    let si = rule_quantity(raw).or_else(|| match t {
        ParamType::Length => parse_edit_length(raw, u),
        ParamType::Area => rule_number(raw).map(|x| x / area_in_units(1.0, u)),
        _ => rule_number(raw).map(|x| x / volume_in_units(1.0, u)),
    })?;
    si.is_finite().then(|| si.to_string())
}

/// Filter for the window: quantity values in project units.
pub(super) fn open(
    mut spec: ViewFilterSpec,
    defs: &[ParamDef],
    u: Units,
) -> (ViewFilterSpec, Known) {
    let mut known = vec![];
    for r in &mut spec.rules {
        let text = shown(r, defs, u);
        if text != r.value {
            let saved = std::mem::replace(&mut r.value, text.clone());
            known.push((r.param.clone(), text, saved));
        }
    }
    (spec, known)
}

/// Filter for the model; None — the quantity value was not recognised.
pub(super) fn to_model(
    spec: &ViewFilterSpec,
    known: &Known,
    defs: &[ParamDef],
    u: Units,
) -> Option<ViewFilterSpec> {
    let mut out = spec.clone();
    for r in &mut out.rules {
        let Some(t) = quantity(r, defs) else { continue };
        r.value = match known.iter().find(|k| k.0 == r.param && k.1 == r.value) {
            Some(k) => k.2.clone(),
            None => to_si(&r.value, t, u)?,
        };
    }
    Some(out)
}

/// Value field hint: the quantity's unit.
pub(super) fn unit(r: &FilterRule, defs: &[ParamDef], u: Units) -> Option<&'static str> {
    quantity(r, defs).map(|t| {
        let kind = match t {
            ParamType::Length => Kind::Len,
            ParamType::Area => Kind::Area,
            _ => Kind::Vol,
        };
        kind.unit(u)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::ParamSpec;

    fn def(name: &str, ptype: ParamType) -> ParamDef {
        ParamDef {
            id: 1,
            spec: ParamSpec {
                name: name.into(),
                ptype,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec![],
            },
        }
    }

    fn filter(param: &str, op: RuleOp, value: &str) -> ViewFilterSpec {
        ViewFilterSpec {
            name: "F".into(),
            rules: vec![FilterRule {
                param: param.into(),
                op,
                value: value.into(),
            }],
            ..Default::default()
        }
    }

    fn value_of(s: Option<ViewFilterSpec>) -> Option<String> {
        s.map(|x| x.rules[0].value.clone())
    }

    #[test]
    fn typed_length_goes_to_the_model_in_si() {
        let defs = [def("H", ParamType::Length)];
        let save = |v: &str, u| value_of(to_model(&filter("H", RuleOp::Gt, v), &vec![], &defs, u));
        assert_eq!(save("3000", Units::Metric).as_deref(), Some("3"));
        assert_eq!(save(&crate::app2_fx::fx("rule_3_m"), Units::Metric).as_deref(), Some("3"));
        assert_eq!(save("2,5 m", Units::Metric).as_deref(), Some("2.5"));
        assert_eq!(save("abc", Units::Metric), None);
        assert_eq!(save("", Units::Metric), None);
    }

    #[test]
    fn area_uses_project_units_and_comma() {
        let defs = [def("A", ParamType::Area)];
        let save = |v: &str, u| value_of(to_model(&filter("A", RuleOp::Ge, v), &vec![], &defs, u));
        assert_eq!(save("1,5", Units::Metric).as_deref(), Some("1.5"));
        let si: f64 = save("10.7639104", Units::Imperial)
            .unwrap()
            .parse()
            .unwrap();
        assert!((si - 1.0).abs() < 1e-6, "{si}");
    }

    #[test]
    fn untouched_rule_keeps_its_saved_value() {
        let defs = [def("H", ParamType::Length)];
        let (shown, known) = open(filter("H", RuleOp::Eq, "1.2345"), &defs, Units::Metric);
        assert_ne!(shown.rules[0].value, "1.2345");
        let back = to_model(&shown, &known, &defs, Units::Metric);
        assert_eq!(value_of(back).as_deref(), Some("1.2345"));
    }

    #[test]
    fn text_rules_stay_as_written() {
        let defs = [def("H", ParamType::Length), def("T", ParamType::Text)];
        for f in [
            filter("H", RuleOp::Contains, "30"),
            filter("T", RuleOp::Eq, "EI60"),
            filter("X", RuleOp::Eq, "1,5"),
        ] {
            let (shown, known) = open(f.clone(), &defs, Units::Metric);
            assert_eq!(shown.rules[0].value, f.rules[0].value);
            let back = to_model(&shown, &known, &defs, Units::Metric);
            assert_eq!(value_of(back), Some(f.rules[0].value.clone()));
        }
        assert!(unit(&filter("T", RuleOp::Eq, "").rules[0], &defs, Units::Metric).is_none());
        assert!(unit(&filter("H", RuleOp::Eq, "").rules[0], &defs, Units::Metric).is_some());
    }
}
