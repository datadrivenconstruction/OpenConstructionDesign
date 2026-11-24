// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Where each element came from, for the exports: the recipe and part that built it and whether
//! a person edited it since, the source file of an import, and how many values the assistant set.
use std::collections::BTreeMap;

use ddc_model::Model;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prov {
    /// recipe name, part key, edited by hand since
    pub recipe: Option<(String, String, bool)>,
    pub import: Option<String>,
    /// values the assistant set, of them not verified
    pub ai: (i64, i64),
}

/// Provenance of the elements `live` accepts, by (table, id); elements with none are absent.
pub fn read(model: &Model, live: impl Fn(&str, i64) -> bool) -> BTreeMap<(String, i64), Prov> {
    let mut by: BTreeMap<(String, i64), Prov> = BTreeMap::new();
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

/// The `DDC_Provenance` properties of one element as (name, JSON value), in the IFC set's order.
pub fn fields(p: &Prov) -> Vec<(&'static str, serde_json::Value)> {
    let mut f = vec![];
    if let Some((name, part, edited)) = &p.recipe {
        f.push(("Recipe", name.as_str().into()));
        f.push(("RecipePart", part.as_str().into()));
        f.push(("EditedByHand", (*edited).into()));
    }
    if let Some(src) = &p.import {
        f.push(("ImportSource", src.as_str().into()));
    }
    if p.ai.0 > 0 {
        f.push(("AiValues", p.ai.0.into()));
        f.push(("AiValuesUnverified", p.ai.1.into()));
    }
    f
}
