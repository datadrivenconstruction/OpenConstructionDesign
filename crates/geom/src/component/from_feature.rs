//! A component definition from a feature timeline: the timeline's parameters become the
//! family's parameters (plain values as instance parameters, formulas as formulas), the
//! timeline is the family's body. Instances with other values build other bodies.

use std::fmt::Write;

use super::expr::{self, ANGLE, AREA, LENGTH, NUMBER, VOLUME};
use super::from_mesh::ident;
use super::{Category, Family, Hosting};
use crate::feature::{json, Timeline};

/// Family text; the result parses and builds (checked before it is returned).
pub fn family_text(name: &str, category: Category, tl: &Timeline) -> Result<String, String> {
    let vals = tl.values().map_err(|e| e.to_string())?;
    let mut s = String::new();
    let _ = writeln!(s, "family {}", name.replace(['\n', '#'], " ").trim());
    let _ = writeln!(s, "category {}", category.name());
    let _ = writeln!(s, "host {}", Hosting::None.name());
    for p in &tl.params {
        if ident(&p.name) != p.name {
            return Err(format!("parameter name {} cannot name a family parameter", p.name));
        }
        let q = vals[&p.name];
        let kind = match q.d {
            d if d == LENGTH => "length",
            d if d == ANGLE => "angle",
            d if d == AREA => "area",
            d if d == VOLUME => "volume",
            d if d == NUMBER => "number",
            d => return Err(format!("parameter {} has unit {d}", p.name)),
        };
        // a plain value can differ per instance; a formula keeps following its inputs
        let plain = expr::parse(p.expr.trim())
            .ok()
            .and_then(|e| e.eval(&|_| None).ok())
            .is_some();
        if plain {
            let _ = writeln!(s, "param {} {kind} instance {}", p.name, expr::literal(q));
        } else {
            let _ = writeln!(s, "param {} {kind} instance = {}", p.name, p.expr.trim());
        }
    }
    let _ = writeln!(s, "type \"Default\"");
    let _ = writeln!(s, "feature Body data({})", json::to_data(tl));
    let f = Family::parse(&s).map_err(|e| format!("generated family does not parse: {e}"))?;
    let ty = f.types.first().map(|t| t.values.clone()).unwrap_or_default();
    let r = f.resolve(&ty, &[]).map_err(|e| e.to_string())?;
    f.build(&r, super::Lod::Fine).map_err(|e| e.to_string())?;
    Ok(s)
}
