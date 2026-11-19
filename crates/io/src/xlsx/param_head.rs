// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Header of a parameter column. `units::norm` drops brackets and case, so two parameters, or a
//! parameter and a format column, can share one normalised header. The later one is exported as
//! `Name (#id)`, optionally followed by the unit, and the id binds it on read. Every other header
//! is matched on the exact name first; a normalised match counts only when it names one parameter.

use ddc_model::ParamDef;

use super::units;

/// What a sheet header says about the parameters of its sheet.
pub(crate) enum Bound<'a> {
    One(&'a ParamDef),
    /// Several parameters fit and nothing tells them apart: their names.
    Many(Vec<String>),
    /// Carries a `(#id)` that no parameter of the sheet answers to.
    StaleTag,
    None,
}

/// `Name (#id)`: the header of a parameter whose plain header is taken.
pub(crate) fn tagged_name(name: &str, id: i64) -> String {
    format!("{} (#{id})", name.trim())
}

/// Name and id of a `Name (#id)` header; a unit in brackets may follow the tag.
pub(crate) fn split_tag(h: &str) -> Option<(&str, i64)> {
    let t = h.trim();
    let at = t.rfind(" (#")?;
    let rest = &t[at + 3..];
    let end = rest.find(')')?;
    let digits = &rest[..end];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let tail = rest[end + 1..].trim();
    if !tail.is_empty() && !tail.starts_with('[') {
        return None;
    }
    let name = t[..at].trim();
    if name.is_empty() {
        return None;
    }
    Some((name, digits.parse().ok()?))
}

/// The parameter a header's tag names, when the name in the header is still that parameter's.
pub(crate) fn tag_of<'a>(h: &str, defs: &'a [ParamDef]) -> Option<&'a ParamDef> {
    let (name, id) = split_tag(h)?;
    defs.iter().find(|d| d.id == id && d.spec.name.trim() == name)
}

/// True when the rest of a header after the name is at most one unit in brackets.
fn only_unit(rest: &str) -> bool {
    let r = rest.trim();
    if r.is_empty() {
        return true;
    }
    r.starts_with('[') && r.ends_with(']') && r.matches('[').count() == 1
}

/// Finds the parameter a header binds to: tag, exact name, exact name with a unit, then the
/// normalised name when only one parameter has it.
pub(crate) fn bind<'a>(h: &str, own: &'a [ParamDef]) -> Bound<'a> {
    if let Some(d) = tag_of(h, own) {
        return Bound::One(d);
    }
    let t = h.trim();
    if let Some(d) = own.iter().find(|d| d.spec.name.trim() == t) {
        return Bound::One(d);
    }
    if split_tag(t).is_some() {
        return Bound::StaleTag;
    }
    let with_unit: Vec<&ParamDef> = own
        .iter()
        .filter(|d| t.strip_prefix(d.spec.name.trim()).is_some_and(only_unit))
        .collect();
    let pick = |v: Vec<&'a ParamDef>| match v.as_slice() {
        [] => Bound::None,
        [d] => Bound::One(*d),
        many => Bound::Many(many.iter().map(|d| d.spec.name.trim().to_string()).collect()),
    };
    if !with_unit.is_empty() {
        return pick(with_unit);
    }
    let n = units::norm(t);
    pick(own.iter().filter(|d| units::norm(&d.spec.name) == n).collect())
}

/// The part of a header that can hold the unit: what follows the name, so brackets in the name
/// (`Cost [EUR] [m]`) are not read as the unit.
pub(crate) fn unit_part<'a>(h: &'a str, d: &ParamDef) -> &'a str {
    let t = h.trim();
    if let Some((name, id)) = split_tag(t) {
        if id == d.id && name == d.spec.name.trim() {
            return t.rfind(')').map_or(t, |at| &t[at + 1..]);
        }
    }
    t.strip_prefix(d.spec.name.trim()).unwrap_or(t)
}
