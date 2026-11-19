// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Relative values in tool text ("+0.5 m", "-10%", "x1.2") for numeric parameters and sizes,
//! and the elements an edit skipped with their reasons (shown on the plan card).

use super::plan::PlanState;
use super::validate::{IssueLevel, PlanIssue};
use crate::query::Val;
use ddc_model::{ParamType, PVal};

/// An element a plan item left out, with the reason; the rest of the item still applies.
#[derive(Clone, Debug, PartialEq)]
pub struct Skip {
    pub cat: String,
    pub id: i64,
    pub why: String,
}

/// Skips listed one by one on the card; the rest are counted.
const SHOWN: usize = 20;

fn starts_number(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.')
}

fn plain(s: &str) -> bool {
    // num-lint: tool arguments are machine text with a dot, never locale input
    starts_number(s) && s.parse::<f64>().is_ok_and(f64::is_finite)
}

/// `+0.5 m`, `-0.5 m`, `+10%`, `-10%`, `x1.2`. A bare `-3` stays a literal (a negative offset);
/// a minus is relative only when it has a unit or a percent sign. `None`: not relative.
pub fn parse(raw: &str) -> Option<Val> {
    let t = raw.trim();
    let mut ch = t.chars();
    let first = ch.next()?;
    let rest = ch.as_str().trim_start();
    match first {
        '\u{d7}' | '*' | 'x' | 'X' => plain(rest).then(|| Val::Rel('*', rest.to_string())),
        '+' | '-' | '\u{2212}' => {
            if !starts_number(rest) {
                return None;
            }
            let minus = first != '+';
            if let Some(p) = rest.strip_suffix('%') {
                let p = p.trim_end();
                return plain(p).then(|| {
                    Val::Rel('%', if minus { format!("-{p}") } else { p.to_string() })
                });
            }
            let unit = rest.trim_start_matches(|c: char| c.is_ascii_digit() || ".,".contains(c));
            if minus && unit.trim().is_empty() {
                return None;
            }
            Some(Val::Rel(if minus { '-' } else { '+' }, rest.to_string()))
        }
        _ => None,
    }
}

/// Parameter types a relative value may change.
pub fn numeric(t: ParamType) -> bool {
    matches!(
        t,
        ParamType::Int | ParamType::Number | ParamType::Length | ParamType::Area | ParamType::Volume
    )
}

/// The new value of a numeric parameter from its current one. `Err` is the reason to skip.
pub fn param_value(
    t: ParamType,
    cur: Option<PVal>,
    op: char,
    by: &str,
    units: crate::units::Units,
) -> Result<PVal, String> {
    let cur = cur
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "no value to change".to_string())?;
    // a length reads its unit; any other number is plain
    let key = if t == ParamType::Length { "width" } else { "count" };
    let a = crate::query::Assign {
        key: key.into(),
        val: Val::Rel(op, by.to_string()),
    };
    let x = crate::query::new_num(&a, cur, units, crate::query::NumIn::Tool)
        .map_err(|e| e.to_string())?;
    if !x.is_finite() {
        return Err("result is not a number".to_string());
    }
    if t != ParamType::Number && x < 0.0 {
        return Err("result would be negative".to_string());
    }
    if t == ParamType::Int {
        if (x - x.round()).abs() > 1e-9 {
            return Err("result is not a whole number".to_string());
        }
        return Ok(PVal::Int(x.round() as i64));
    }
    Ok(PVal::Num(x))
}

/// Card warnings for the elements the plan's items skipped.
pub fn skip_issues(st: &PlanState) -> Vec<PlanIssue> {
    let mut out = vec![];
    let mut more = 0usize;
    for it in &st.items {
        let Some(list) = st.skips.get(&it.id) else { continue };
        if !it.checked {
            continue;
        }
        for s in list {
            if out.len() >= SHOWN {
                more += 1;
                continue;
            }
            let table = if s.cat == "door" || s.cat == "window" { "opening" } else { &s.cat };
            out.push(PlanIssue {
                level: IssueLevel::Warn,
                item: Some(it.id),
                el: Some((table.to_string(), s.id)),
                text_key: "ai.issue.skipped",
                args: vec![s.cat.clone(), s.id.to_string(), s.why.clone()],
            });
        }
    }
    if more > 0 {
        out.push(PlanIssue {
            level: IssueLevel::Warn,
            item: None,
            el: None,
            text_key: "ai.issue.skipped_more",
            args: vec![more.to_string()],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> Option<(char, String)> {
        match parse(s)? {
            Val::Rel(c, b) => Some((c, b)),
            Val::Lit(_) => None,
        }
    }

    #[test]
    fn signed_and_unit_values_are_relative() {
        assert_eq!(rel("+0.5 m"), Some(('+', "0.5 m".into())));
        assert_eq!(rel(" + 50 mm "), Some(('+', "50 mm".into())));
        assert_eq!(rel("-0.5 m"), Some(('-', "0.5 m".into())));
        assert_eq!(rel("\u{2212}20mm"), Some(('-', "20mm".into())));
        assert_eq!(rel("+3"), Some(('+', "3".into())));
    }

    #[test]
    fn percent_keeps_its_sign() {
        assert_eq!(rel("-10%"), Some(('%', "-10".into())));
        assert_eq!(rel("+10 %"), Some(('%', "10".into())));
        assert_eq!(rel("+2.5%"), Some(('%', "2.5".into())));
    }

    #[test]
    fn factors() {
        assert_eq!(rel("\u{d7}1.2"), Some(('*', "1.2".into())));
        assert_eq!(rel("x 2"), Some(('*', "2".into())));
        assert_eq!(rel("*0.5"), Some(('*', "0.5".into())));
    }

    #[test]
    fn plain_values_are_not_relative() {
        for s in ["", "1.5", "-3", "-0.25", "800 mm", "abc", "+", "x", "x1a", "+a", "-%", "10%", "xx2"] {
            assert_eq!(rel(s), None, "{s:?}");
        }
    }

    #[test]
    fn parameter_values_follow_the_type() {
        let u = crate::units::Units::Metric;
        let go = |t, cur: f64, op, by| param_value(t, Some(PVal::Num(cur)), op, by, u);
        assert_eq!(go(ParamType::Number, 10.0, '%', "-10"), Ok(PVal::Num(9.0)));
        assert_eq!(go(ParamType::Number, 4.0, '*', "1.5"), Ok(PVal::Num(6.0)));
        assert_eq!(go(ParamType::Number, 1.0, '+', "0.5"), Ok(PVal::Num(1.5)));
        match go(ParamType::Length, 2.0, '+', "0.5 m") {
            Ok(PVal::Num(x)) => assert!((x - 2.5).abs() < 1e-9, "{x}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            param_value(ParamType::Int, Some(PVal::Int(4)), '*', "2", u),
            Ok(PVal::Int(8))
        );
        assert!(param_value(ParamType::Int, Some(PVal::Int(3)), '*', "1.5", u).is_err());
        assert!(go(ParamType::Area, 1.0, '-', "2").is_err());
        assert!(param_value(ParamType::Number, None, '+', "1", u).is_err());
    }

    #[test]
    fn skips_become_warnings_with_a_cap() {
        let mut st = PlanState::default();
        st.items.push(super::super::plan::PlanItem {
            id: 1,
            title: String::new(),
            why: String::new(),
            op: super::super::plan::PlanOp::Delete {
                target: super::super::plan::Targets::Refs(vec![]),
            },
            checked: true,
            depends_on: vec![],
            actor: String::new(),
            conflict: None,
            sets: Default::default(),
        });
        let list: Vec<Skip> = (0..SHOWN as i64 + 3)
            .map(|i| Skip { cat: "door".into(), id: i, why: "too wide".into() })
            .collect();
        st.skips.insert(1, list);
        let v = skip_issues(&st);
        assert_eq!(v.len(), SHOWN + 1);
        assert_eq!(v[0].text_key, "ai.issue.skipped");
        assert_eq!(v[0].el, Some(("opening".to_string(), 0)));
        assert_eq!(v[SHOWN].text_key, "ai.issue.skipped_more");
        assert_eq!(v[SHOWN].args, vec!["3".to_string()]);
        st.items[0].checked = false;
        assert!(skip_issues(&st).is_empty());
    }
}
