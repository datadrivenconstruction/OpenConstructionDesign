// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Architectural (CL) and structural (SC) column: click inserts, Space rotates 90°,
//! "At intersections" places columns at all grid intersections in one journal entry.

use super::{Active, Kind, Rec};
use crate::editor::Editor;
use ddc_geom::V2;
use ddc_model::{ColumnSpec, Error, MemberShape, Model};

/// Architectural column types: name and section.
pub const ARCH: [(&str, MemberShape); 4] = [
    ("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} 400×400", MemberShape::Rect { b: 0.4, h: 0.4 }),
    ("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} 300×300", MemberShape::Rect { b: 0.3, h: 0.3 }),
    ("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} 600×400", MemberShape::Rect { b: 0.6, h: 0.4 }),
    ("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} Ø400", MemberShape::Circle { d: 0.4 }),
];

/// Section part of a type name: `400×400`, `Ø400`.
fn size_of(name: &str) -> &str {
    name.rsplit(' ').next().unwrap_or("")
}

/// Names of the architectural column types in the catalog language (new documents only: a type
/// already stored in a document keeps its name).
pub fn arch_names(cat: &ddc_ui::Catalog) -> Vec<String> {
    let word = cat.t("agent_draft.type.column");
    ARCH.iter().map(|t| format!("{word} {}", size_of(t.0))).collect()
}

/// Section of an architectural column type by its name in either language.
pub fn arch_shape(name: &str) -> Option<MemberShape> {
    let named = name.starts_with("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} ")
        || name.starts_with("Column ");
    if !named {
        return None;
    }
    ARCH.iter().find(|t| size_of(t.0) == size_of(name)).map(|t| t.1.clone())
}

/// Catalog sizes for structural columns and beams.
pub fn catalog_names(kind: Kind) -> Vec<String> {
    let all = ddc_geom::member::catalog();
    let pick = |pre: &str| -> Vec<String> {
        all.iter()
            .filter(|c| c.0.starts_with(pre))
            .take(6)
            .map(|c| c.0.to_string())
            .collect()
    };
    let mut v = match kind {
        Kind::Beam => pick("IPE"),
        _ => pick("HEB"),
    };
    if v.is_empty() {
        v = all.iter().take(8).map(|c| c.0.to_string()).collect();
    }
    v
}

/// Section of the selected type (for preview), m.
pub fn section(a: &Active) -> MemberShape {
    let name = a.type_name();
    if let Some(s) = arch_shape(name) {
        return s;
    }
    ddc_geom::member::catalog()
        .into_iter()
        .find(|c| c.0 == name)
        .map_or(MemberShape::Rect { b: 0.3, h: 0.3 }, |c| c.1)
}

/// Section extents (b along local x, h along y).
pub fn extent(s: &MemberShape) -> (f64, f64) {
    match s {
        MemberShape::Rect { b, h } => (*b, *h),
        MemberShape::Circle { d } => (*d, *d),
        MemberShape::I { h, b, .. } => (*b, *h),
        _ => (0.3, 0.3),
    }
}

/// Member type by name; created if missing (architectural — concrete, catalog — steel).
pub fn ensure_member(m: &mut Model, name: &str) -> Result<(i64, usize), Error> {
    if let Some(t) = m.member_types()?.into_iter().find(|t| t.1 == name) {
        return Ok((t.0, 0));
    }
    let id = match arch_shape(name) {
        Some(s) => m.add_member_type(name, &s, "Concrete")?,
        None => m.add_catalog_member_type(name)?,
    };
    Ok((id, 1))
}

pub fn specs(a: &Active, at: &[V2]) -> Vec<ColumnSpec> {
    // level above exists — attach the top to it, otherwise use an unconnected height
    let (top_level, top_offset) = match a.above {
        Some((id, _)) => (Some(id), 0.0),
        None => (None, a.opts.top_off),
    };
    at.iter()
        .map(|p| ColumnSpec {
            level: a.level,
            top_level,
            member_type: 0,
            structural: a.kind == Kind::StructColumn,
            at: (p.x, p.y),
            base_offset: a.opts.base_off,
            top_offset,
            rotation: a.rotation(),
            top_at: None,
        })
        .collect()
}

pub fn rec(a: &Active, at: &[V2]) -> Result<Rec, String> {
    let name = a.type_name().to_string();
    let specs = specs(a, at);
    if specs.is_empty() {
        return Err(a.t("tool.err.no_grids"));
    }
    Ok(Rec::new("column", move |m| {
        let (t, n) = ensure_member(m, &name)?;
        let ss: Vec<ColumnSpec> = specs
            .iter()
            .cloned()
            .map(|mut s| {
                s.member_type = t;
                s
            })
            .collect();
        m.add_columns(&ss)?;
        Ok(n + 1)
    }))
}

/// Columns at all grid intersections (one journal entry).
pub fn at_grids(ed: &mut Editor, a: &mut Active) {
    let pts: Vec<V2> = ed
        .model
        .grid_intersections()
        .unwrap_or_default()
        .into_iter()
        .map(|((x, y), ..)| ddc_geom::v2(x, y))
        .collect();
    let r = rec(a, &pts);
    super::commit(ed, a, r);
}
