// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed refusals of materials, view templates and view filters: each names the field the user
//! has to change, so the UI can localize it without showing internal text or ids.

/// Why a material, template or filter edit was refused (always bad input).
#[derive(Debug, Clone, PartialEq)]
pub enum FieldErr {
    MaterialName,
    MaterialColor,
    MaterialHatch(String),
    MaterialValue,
    TemplateName,
    FilterName,
    FilterCategory(String),
    FilterRuleParam,
}

impl FieldErr {
    /// Catalog key of the user message (`view_gfx.err.*`).
    pub fn key(&self) -> &'static str {
        match self {
            FieldErr::MaterialName => "view_gfx.err.material_name",
            FieldErr::MaterialColor => "view_gfx.err.material_color",
            FieldErr::MaterialHatch(_) => "view_gfx.err.material_hatch",
            FieldErr::MaterialValue => "view_gfx.err.material_value",
            FieldErr::TemplateName => "view_gfx.err.template_name",
            FieldErr::FilterName => "view_gfx.err.filter_name",
            FieldErr::FilterCategory(_) => "view_gfx.err.filter_category",
            FieldErr::FilterRuleParam => "view_gfx.err.filter_rule",
        }
    }

    /// Message arguments by placeholder name.
    pub fn args(&self) -> Vec<(&'static str, String)> {
        match self {
            FieldErr::MaterialHatch(h) => vec![("hatch", h.clone())],
            FieldErr::FilterCategory(c) => vec![("category", c.clone())],
            _ => vec![],
        }
    }
}

impl std::fmt::Display for FieldErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldErr::MaterialName => f.write_str("material name must be 1..120 characters"),
            FieldErr::MaterialColor => f.write_str("material color is out of range"),
            FieldErr::MaterialHatch(h) => write!(f, "unknown hatch '{h}'"),
            FieldErr::MaterialValue => f.write_str("material property must be a number >= 0"),
            FieldErr::TemplateName => f.write_str("template name is empty"),
            FieldErr::FilterName => f.write_str("filter name must be 1..120 characters"),
            FieldErr::FilterCategory(c) => write!(f, "unknown category '{c}'"),
            FieldErr::FilterRuleParam => f.write_str("filter rule without a parameter"),
        }
    }
}

impl From<FieldErr> for crate::Error {
    fn from(e: FieldErr) -> Self {
        crate::Error::Field(e)
    }
}
