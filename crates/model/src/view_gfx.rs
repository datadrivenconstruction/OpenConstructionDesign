// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View graphics (schema 35): level of detail, thin lines, visibility and per-category overrides
//! (`view_gfx`, keyed by `ViewKey`), view templates (`view_template`), rule filters
//! (`view_filter`), project object styles (`object_style`).
//! A template is applied once (a copy of fields) or linked: the fields it controls are taken by the view
//! from the template and cannot be edited. The result for drawing is [`ViewPaint`]: the object style,
//! over it the view category, over that the filters (the upper one in the view's list wins).

use std::collections::{BTreeMap, HashMap};

use ddc_geom::component::Lod;
use rusqlite::{params, types::Value, Connection, OptionalExtension};
use serde_json::{json, Value as J};

use crate::ops::Inv;
use crate::phases::ViewKey;
use crate::{Error, FieldErr, Model, TagHost, ViewPrim};

mod eval;

pub(crate) use eval::PaintCache;
pub use eval::{rule_number, rule_quantity};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS view_gfx(vkind TEXT NOT NULL, ref INTEGER NOT NULL, template INTEGER,
    spec TEXT NOT NULL, PRIMARY KEY(vkind, ref));
CREATE TABLE IF NOT EXISTS view_template(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    fields TEXT NOT NULL, spec TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS view_filter(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, def TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS object_style(category TEXT PRIMARY KEY, spec TEXT NOT NULL);
";

/// Additive and idempotent.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// View categories: (key, table). A door and a window share one table, told apart by `opening.kind`.
pub const GFX_CATEGORIES: [(&str, &str); 26] = [
    ("wall", "wall"),
    ("door", "opening"),
    ("window", "opening"),
    ("curtain_wall", "curtain_wall"),
    ("slab", "slab"),
    ("room", "room"),
    ("col", "col"),
    ("beam", "beam"),
    ("roof", "roof"),
    ("ceiling", "ceiling"),
    ("stair", "stair"),
    ("railing", "railing"),
    ("mep_run", "mep_run"),
    ("comp", "comp"),
    ("shaft", "shaft"),
    ("mass", "mass"),
    ("solid", "solid"),
    ("iso_footing", "iso_footing"),
    ("strip_footing", "strip_footing"),
    ("raft", "raft"),
    ("brace", "brace"),
    ("truss", "truss"),
    ("toposolid", "toposolid"),
    ("grid", "grid"),
    ("elec_circuit", "elec_circuit"),
    ("elec_cable", "elec_cable"),
];

/// A view's level of detail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DetailLevel {
    Coarse,
    #[default]
    Medium,
    Fine,
}

impl DetailLevel {
    pub const ALL: [DetailLevel; 3] = [DetailLevel::Coarse, DetailLevel::Medium, DetailLevel::Fine];

    pub fn key(self) -> &'static str {
        match self {
            DetailLevel::Coarse => "coarse",
            DetailLevel::Medium => "medium",
            DetailLevel::Fine => "fine",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.key() == s)
    }
    pub fn lod(self) -> Lod {
        match self {
            DetailLevel::Coarse => Lod::Coarse,
            DetailLevel::Medium => Lod::Medium,
            DetailLevel::Fine => Lod::Fine,
        }
    }
    /// The next one cyclically (a ribbon command).
    pub fn next(self) -> Self {
        match self {
            DetailLevel::Coarse => DetailLevel::Medium,
            DetailLevel::Medium => DetailLevel::Fine,
            DetailLevel::Fine => DetailLevel::Coarse,
        }
    }
}

/// A graphics override; None means unset (the level below is used).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GfxOverride {
    /// Line colour, 0xRRGGBB.
    pub line: Option<u32>,
    /// Line weight, screen px.
    pub line_weight: Option<f32>,
    /// Fill colour, 0xRRGGBB.
    pub fill: Option<u32>,
    /// Fill transparency, %.
    pub transparency: Option<u8>,
}

impl GfxOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// The fields of `self`, with unset ones taken from `lower`.
    pub fn or(self, lower: GfxOverride) -> GfxOverride {
        GfxOverride {
            line: self.line.or(lower.line),
            line_weight: self.line_weight.or(lower.line_weight),
            fill: self.fill.or(lower.fill),
            transparency: self.transparency.or(lower.transparency),
        }
    }
    fn to_json(self) -> J {
        let mut m = serde_json::Map::new();
        if let Some(v) = self.line {
            m.insert("line".into(), json!(v));
        }
        if let Some(v) = self.line_weight {
            m.insert("weight".into(), json!(f64::from(v)));
        }
        if let Some(v) = self.fill {
            m.insert("fill".into(), json!(v));
        }
        if let Some(v) = self.transparency {
            m.insert("transparency".into(), json!(v));
        }
        J::Object(m)
    }
    fn from_json(j: &J) -> Self {
        GfxOverride {
            line: j["line"].as_u64().map(|v| v as u32),
            line_weight: j["weight"].as_f64().map(|v| v as f32),
            fill: j["fill"].as_u64().map(|v| v as u32),
            transparency: j["transparency"].as_u64().map(|v| v.min(100) as u8),
        }
    }
    fn check(&self) -> Result<(), Error> {
        let color = |c: Option<u32>| c.is_none_or(|c| c <= 0xFF_FFFF);
        let weight = self.line_weight.is_none_or(|w| w.is_finite() && w > 0.0);
        let tr = self.transparency.is_none_or(|t| t <= 100);
        if color(self.line) && color(self.fill) && weight && tr {
            Ok(())
        } else {
            Err(Error::Invalid("bad graphic override".into()))
        }
    }
}

/// A category setting in a view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatGfx {
    pub visible: bool,
    pub over: GfxOverride,
}

impl Default for CatGfx {
    fn default() -> Self {
        Self {
            visible: true,
            over: GfxOverride::default(),
        }
    }
}

/// A project filter attached to a view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterUse {
    pub filter: i64,
    pub enabled: bool,
    /// false means matching elements are hidden.
    pub visible: bool,
    pub over: GfxOverride,
}

/// Plan view range, metres relative to the view's level: the cut plane, the bottom of the
/// primary range, how far the view looks below the bottom and an optional top plane (elements
/// wholly above it are not drawn). The one stored range every plan consumer reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewRange {
    pub cut: f64,
    pub bottom: f64,
    pub depth: f64,
    /// `None`: no upper limit, everything above the cut shows as beyond.
    pub top: Option<f64>,
}

impl ViewRange {
    /// The cut height every plan used before view ranges were stored.
    pub const DEFAULT_CUT: f64 = 1.2;

    /// Why the range cannot be used, if it cannot.
    pub fn issue(&self) -> Option<RangeIssue> {
        let top_ok = self.top.is_none_or(f64::is_finite);
        if !(self.cut.is_finite() && self.bottom.is_finite() && self.depth.is_finite() && top_ok) {
            Some(RangeIssue::NotFinite)
        } else if self.cut <= self.bottom {
            Some(RangeIssue::CutNotAboveBottom)
        } else if self.depth < 0.0 {
            Some(RangeIssue::NegativeDepth)
        } else if self.top.is_some_and(|t| t < self.cut) {
            Some(RangeIssue::TopBelowCut)
        } else {
            None
        }
    }

    fn check(&self) -> Result<(), Error> {
        match self.issue() {
            Some(i) => Err(Error::Invalid(format!("view range: {}", i.key()))),
            None => Ok(()),
        }
    }

    fn to_json(self) -> J {
        let mut j = json!({"cut": self.cut, "bottom": self.bottom, "depth": self.depth});
        if let Some(t) = self.top {
            j["top"] = json!(t);
        }
        j
    }

    /// Missing keys take defaults; an unusable stored range reads as the default one.
    fn from_json(j: &J) -> Self {
        let d = Self::default();
        let r = ViewRange {
            cut: j["cut"].as_f64().unwrap_or(d.cut),
            bottom: j["bottom"].as_f64().unwrap_or(d.bottom),
            depth: j["depth"].as_f64().unwrap_or(d.depth),
            top: j["top"].as_f64(),
        };
        if r.issue().is_some() {
            d
        } else {
            r
        }
    }
}

impl Default for ViewRange {
    fn default() -> Self {
        Self {
            cut: Self::DEFAULT_CUT,
            bottom: 0.0,
            depth: 0.0,
            top: None,
        }
    }
}

/// Reason a view range is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeIssue {
    NotFinite,
    CutNotAboveBottom,
    NegativeDepth,
    TopBelowCut,
}

impl RangeIssue {
    pub fn key(self) -> &'static str {
        match self {
            RangeIssue::NotFinite => "not_finite",
            RangeIssue::CutNotAboveBottom => "cut_not_above_bottom",
            RangeIssue::NegativeDepth => "negative_depth",
            RangeIssue::TopBelowCut => "top_below_cut",
        }
    }
}

/// View (or template) settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewGfx {
    pub detail: DetailLevel,
    pub thin_lines: bool,
    /// Only those differing from the default; the key is from [`GFX_CATEGORIES`].
    pub categories: BTreeMap<String, CatGfx>,
    pub filters: Vec<FilterUse>,
    /// Plan view range; stored only when it differs from the default.
    pub range: ViewRange,
}

impl ViewGfx {
    pub fn to_json(&self) -> String {
        let cats: serde_json::Map<String, J> = self
            .categories
            .iter()
            .map(|(k, c)| {
                (
                    k.clone(),
                    json!({"visible": c.visible, "over": c.over.to_json()}),
                )
            })
            .collect();
        let filters: Vec<J> = self
            .filters
            .iter()
            .map(|f| {
                json!({
                    "filter": f.filter,
                    "enabled": f.enabled,
                    "visible": f.visible,
                    "over": f.over.to_json(),
                })
            })
            .collect();
        let mut j = json!({
            "detail": self.detail.key(),
            "thin_lines": self.thin_lines,
            "categories": J::Object(cats),
            "filters": filters,
        });
        if self.range != ViewRange::default() {
            j["range"] = self.range.to_json();
        }
        j.to_string()
    }

    /// Parsing with defaults: unknown fields are skipped, corrupted text gives default settings.
    pub fn from_json(s: &str) -> Self {
        let Ok(j) = serde_json::from_str::<J>(s) else {
            return Self::default();
        };
        let categories = j["categories"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, c)| {
                        (
                            k.clone(),
                            CatGfx {
                                visible: c["visible"].as_bool().unwrap_or(true),
                                over: GfxOverride::from_json(&c["over"]),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let filters = j["filters"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        Some(FilterUse {
                            filter: f["filter"].as_i64()?,
                            enabled: f["enabled"].as_bool().unwrap_or(true),
                            visible: f["visible"].as_bool().unwrap_or(true),
                            over: GfxOverride::from_json(&f["over"]),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ViewGfx {
            detail: j["detail"]
                .as_str()
                .and_then(DetailLevel::from_key)
                .unwrap_or_default(),
            thin_lines: j["thin_lines"].as_bool().unwrap_or(false),
            categories,
            filters,
            range: ViewRange::from_json(&j["range"]),
        }
    }

    fn normalized(mut self) -> Self {
        self.categories.retain(|_, c| *c != CatGfx::default());
        self
    }

    /// Copies fields from `from`.
    pub fn take_fields(&mut self, from: &ViewGfx, fields: &[TemplateField]) {
        for f in fields {
            match f {
                TemplateField::Detail => self.detail = from.detail,
                TemplateField::ThinLines => self.thin_lines = from.thin_lines,
                TemplateField::Categories => self.categories = from.categories.clone(),
                TemplateField::Filters => self.filters = from.filters.clone(),
                TemplateField::ViewRange => self.range = from.range,
            }
        }
    }

    fn check(&self, db: &Connection) -> Result<(), Error> {
        self.range.check()?;
        for (k, c) in &self.categories {
            if !GFX_CATEGORIES.iter().any(|g| g.0 == k) {
                return Err(Error::Invalid(format!("unknown category '{k}'")));
            }
            c.over.check()?;
        }
        for f in &self.filters {
            f.over.check()?;
            if load_filter(db, f.filter)?.is_none() {
                return Err(Error::Invalid(format!("no view filter {}", f.filter)));
            }
        }
        Ok(())
    }
}

/// A view settings field a template can control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TemplateField {
    Detail,
    ThinLines,
    Categories,
    Filters,
    ViewRange,
}

impl TemplateField {
    pub const ALL: [TemplateField; 5] = [
        TemplateField::Detail,
        TemplateField::ThinLines,
        TemplateField::Categories,
        TemplateField::Filters,
        TemplateField::ViewRange,
    ];
    pub fn key(self) -> &'static str {
        match self {
            TemplateField::Detail => "detail",
            TemplateField::ThinLines => "thin_lines",
            TemplateField::Categories => "categories",
            TemplateField::Filters => "filters",
            TemplateField::ViewRange => "view_range",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == s)
    }
    fn same(self, a: &ViewGfx, b: &ViewGfx) -> bool {
        match self {
            TemplateField::Detail => a.detail == b.detail,
            TemplateField::ThinLines => a.thin_lines == b.thin_lines,
            TemplateField::Categories => a.categories == b.categories,
            TemplateField::Filters => a.filters == b.filters,
            TemplateField::ViewRange => a.range == b.range,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewTemplate {
    pub id: i64,
    pub name: String,
    /// The fields the template controls.
    pub fields: Vec<TemplateField>,
    pub spec: ViewGfx,
}

/// View settings taking the linked template into account.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewGfxState {
    pub template: Option<i64>,
    /// Fields locked by the linked template.
    pub locked: Vec<TemplateField>,
    /// The view's own settings.
    pub own: ViewGfx,
    /// What is drawn: its own plus the locked fields from the template.
    pub effective: ViewGfx,
}

/// A rule comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleOp {
    Eq,
    Ne,
    Contains,
    NotContains,
    Lt,
    Le,
    Gt,
    Ge,
    Exists,
    Missing,
}

impl RuleOp {
    pub const ALL: [RuleOp; 10] = [
        RuleOp::Eq,
        RuleOp::Ne,
        RuleOp::Contains,
        RuleOp::NotContains,
        RuleOp::Lt,
        RuleOp::Le,
        RuleOp::Gt,
        RuleOp::Ge,
        RuleOp::Exists,
        RuleOp::Missing,
    ];
    pub fn key(self) -> &'static str {
        match self {
            RuleOp::Eq => "eq",
            RuleOp::Ne => "ne",
            RuleOp::Contains => "contains",
            RuleOp::NotContains => "not_contains",
            RuleOp::Lt => "lt",
            RuleOp::Le => "le",
            RuleOp::Gt => "gt",
            RuleOp::Ge => "ge",
            RuleOp::Exists => "exists",
            RuleOp::Missing => "missing",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.key() == s)
    }
    /// The comparison needs a value (otherwise the value field is hidden).
    pub fn takes_value(self) -> bool {
        !matches!(self, RuleOp::Exists | RuleOp::Missing)
    }
}

/// A rule: parameter, comparison, value ("Fire resistance limit = EI30").
#[derive(Clone, Debug, PartialEq)]
pub struct FilterRule {
    pub param: String,
    pub op: RuleOp,
    pub value: String,
}

impl FilterRule {
    /// Value check. Numbers compare as numbers (comma or point, units in SI),
    /// text case-insensitively; a number is not order-compared with text.
    /// An element without the parameter matches only `Missing`: a value rule does not concern it.
    pub fn test(&self, v: Option<&str>) -> bool {
        eval::Prepared::new(self, None).test_text(v)
    }
}

/// A view filter definition.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewFilterSpec {
    pub name: String,
    /// Categories from [`GFX_CATEGORIES`]; empty means all.
    pub categories: Vec<String>,
    /// One rule suffices (otherwise all are needed).
    pub any: bool,
    pub rules: Vec<FilterRule>,
}

impl ViewFilterSpec {
    pub fn applies_to(&self, category: &str) -> bool {
        self.categories.is_empty() || self.categories.iter().any(|c| c == category)
    }

    /// Whether a category element matches; `get` returns a parameter value by name. A filter with no rules
    /// matches all elements of its categories.
    pub fn matches(&self, category: &str, get: &dyn Fn(&str) -> Option<String>) -> bool {
        if !self.applies_to(category) {
            return false;
        }
        let one = |r: &FilterRule| r.test(get(&r.param).as_deref());
        if self.rules.is_empty() {
            true
        } else if self.any {
            self.rules.iter().any(one)
        } else {
            self.rules.iter().all(one)
        }
    }

    fn def_json(&self) -> String {
        let rules: Vec<J> = self
            .rules
            .iter()
            .map(|r| json!({"param": r.param, "op": r.op.key(), "value": r.value}))
            .collect();
        json!({"categories": self.categories, "any": self.any, "rules": rules}).to_string()
    }

    fn from_def(name: String, s: &str) -> Self {
        let j: J = serde_json::from_str(s).unwrap_or(J::Null);
        let categories = j["categories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| Some(c.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let rules = j["rules"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(FilterRule {
                            param: r["param"].as_str()?.to_string(),
                            op: RuleOp::from_key(r["op"].as_str()?)?,
                            value: r["value"].as_str().unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ViewFilterSpec {
            name,
            categories,
            any: j["any"].as_bool().unwrap_or(false),
            rules,
        }
    }

    fn check(&self) -> Result<(), Error> {
        let n = self.name.trim();
        if n.is_empty() || n.chars().count() > 120 {
            return Err(FieldErr::FilterName.into());
        }
        if let Some(c) = self
            .categories
            .iter()
            .find(|c| !GFX_CATEGORIES.iter().any(|g| g.0 == c.as_str()))
        {
            return Err(FieldErr::FilterCategory(c.clone()).into());
        }
        if self.rules.iter().any(|r| r.param.trim().is_empty()) {
            return Err(FieldErr::FilterRuleParam.into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewFilter {
    pub id: i64,
    pub spec: ViewFilterSpec,
}

/// An element's graphics in a view.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ElemPaint {
    pub hidden: bool,
    pub over: GfxOverride,
}

/// The result for drawing a view; the element key is (table, id). `elems` holds only elements
/// differing from the default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewPaint {
    pub detail: DetailLevel,
    pub thin_lines: bool,
    pub hidden_cats: Vec<String>,
    pub elems: HashMap<(&'static str, i64), ElemPaint>,
    /// Whether there are hidden elements: drawing without them does not check every primitive.
    pub any_hidden: bool,
}

/// A short-lived table lookup: the key is covariant, the string is not copied.
fn elem_at<'a>(
    m: &'a HashMap<(&'a str, i64), ElemPaint>,
    table: &'a str,
    id: i64,
) -> Option<ElemPaint> {
    m.get(&(table, id)).copied()
}

impl ViewPaint {
    /// An element's graphics; None if it does not differ from the default. Allocation-free: called per frame.
    pub fn elem(&self, table: &str, id: i64) -> Option<ElemPaint> {
        elem_at(&self.elems, table, id)
    }

    pub fn hidden(&self, table: &str, id: i64) -> bool {
        self.any_hidden && self.elem(table, id).is_some_and(|e| e.hidden)
    }

    /// Removes hidden elements from the drawing primitives; a tag goes with its host.
    pub fn retain_prims(&self, prims: &mut Vec<ViewPrim>) {
        let no_grid = self.hidden_cats.iter().any(|c| c == "grid");
        if !self.any_hidden && !no_grid {
            return;
        }
        prims.retain(|q| match hide_ref(q) {
            Some((t, id)) => !self.hidden(t, id),
            None => !(no_grid && matches!(q, ViewPrim::Grid { .. })),
        });
    }
}

fn load_filter(db: &Connection, id: i64) -> Result<Option<ViewFilter>, Error> {
    let row: Option<(String, String)> = db
        .prepare_cached("SELECT name, def FROM view_filter WHERE id=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(row.map(|(name, def)| ViewFilter {
        id,
        spec: ViewFilterSpec::from_def(name, &def),
    }))
}

fn parse_fields(s: &str) -> Vec<TemplateField> {
    s.split(',').filter_map(TemplateField::from_key).collect()
}

fn fields_text(f: &[TemplateField]) -> String {
    f.iter().map(|x| x.key()).collect::<Vec<_>>().join(",")
}

fn load_template(db: &Connection, id: i64) -> Result<Option<ViewTemplate>, Error> {
    let row: Option<(String, String, String)> = db
        .prepare_cached("SELECT name, fields, spec FROM view_template WHERE id=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    Ok(row.map(|(name, fields, spec)| ViewTemplate {
        id,
        name,
        fields: parse_fields(&fields),
        spec: ViewGfx::from_json(&spec),
    }))
}

fn need_template(db: &Connection, id: i64) -> Result<ViewTemplate, Error> {
    load_template(db, id)?.ok_or_else(|| Error::Invalid(format!("no view template {id}")))
}

pub(crate) fn state(db: &Connection, v: ViewKey) -> Result<ViewGfxState, Error> {
    let (k, r) = v.parts();
    let row: Option<(Option<i64>, String)> = db
        .prepare_cached("SELECT template, spec FROM view_gfx WHERE vkind=?1 AND ref=?2")?
        .query_row(params![k, r], |x| Ok((x.get(0)?, x.get(1)?)))
        .optional()?;
    let Some((template, spec)) = row else {
        return Ok(ViewGfxState::default());
    };
    let own = ViewGfx::from_json(&spec);
    let tpl = match template {
        Some(t) => load_template(db, t)?,
        None => None,
    };
    let mut effective = own.clone();
    let locked = match &tpl {
        Some(t) => {
            effective.take_fields(&t.spec, &t.fields);
            t.fields.clone()
        }
        None => vec![],
    };
    Ok(ViewGfxState {
        template: tpl.map(|t| t.id),
        locked,
        own,
        effective,
    })
}

/// Writes a view row; the inverse command restores the previous one.
pub(crate) fn write_view(
    db: &Connection,
    v: ViewKey,
    template: Option<i64>,
    spec: &ViewGfx,
) -> Result<Inv, Error> {
    let (k, r) = v.parts();
    let old: Option<i64> = db
        .query_row(
            "SELECT rowid FROM view_gfx WHERE vkind=?1 AND ref=?2",
            params![k, r],
            |x| x.get(0),
        )
        .optional()?;
    let inv = match old {
        Some(rowid) => Inv::default().restore(db, "view_gfx", "rowid", rowid)?,
        None => Inv::default().sql(
            "DELETE FROM view_gfx WHERE vkind=?1 AND ref=?2",
            vec![Value::Text(k.into()), Value::Integer(r)],
        ),
    };
    db.execute(
        "INSERT OR REPLACE INTO view_gfx(vkind,ref,template,spec) VALUES (?1,?2,?3,?4)",
        params![k, r, template, spec.to_json()],
    )?;
    Ok(inv)
}

fn table_exists(db: &Connection, t: &str) -> Result<bool, Error> {
    Ok(db
        .prepare_cached("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row([t], |_| Ok(()))
        .optional()?
        .is_some())
}

use crate::schema::has_col;

/// Ids of a category's elements (`only` restricts to this list).
fn category_ids(
    db: &Connection,
    cat: &str,
    table: &str,
    only: Option<&[i64]>,
) -> Result<Vec<i64>, Error> {
    if !table_exists(db, table)? {
        return Ok(vec![]);
    }
    let part = match only {
        Some(ids) => {
            let list: Vec<String> = ids.iter().map(i64::to_string).collect();
            format!(" AND id IN ({})", list.join(","))
        }
        None => String::new(),
    };
    let v = if table == "opening" {
        let mut st = db.prepare(&format!(
            "SELECT id FROM opening WHERE kind=?1{part} ORDER BY id"
        ))?;
        let r: Vec<i64> = st
            .query_map([cat], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        r
    } else {
        let mut st = db.prepare(&format!(
            "SELECT id FROM {table} WHERE 1=1{part} ORDER BY id"
        ))?;
        let r: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        r
    };
    Ok(v)
}

/// The element a primitive is hidden with: the element itself, for a tag its host.
fn hide_ref(p: &ViewPrim) -> Option<(&str, i64)> {
    if let ViewPrim::Tag { tag } = p {
        return Some(match tag.spec.host {
            TagHost::Wall(i) => ("wall", i),
            TagHost::Opening(i) => ("opening", i),
            TagHost::Room(i) => ("room", i),
            TagHost::Comp(i) => ("comp", i),
        });
    }
    prim_ref(p)
}

/// The name is taken by another table row: a clear error instead of a UNIQUE violation.
fn name_free(db: &Connection, table: &str, name: &str, except: i64) -> Result<(), Error> {
    let taken = db
        .prepare_cached(&format!("SELECT 1 FROM {table} WHERE name=?1 AND id<>?2"))?
        .query_row(params![name, except], |_| Ok(()))
        .optional()?
        .is_some();
    if taken {
        return Err(Error::Conflict(format!("{table} '{name}' already exists")));
    }
    Ok(())
}

/// The table and id of a drawing primitive's element.
pub fn prim_ref(p: &ViewPrim) -> Option<(&str, i64)> {
    Some(match p {
        ViewPrim::Cut { wall, .. } => ("wall", *wall),
        ViewPrim::DoorLeaf { opening, .. }
        | ViewPrim::DoorSwing { opening, .. }
        | ViewPrim::WindowLine { opening, .. } => ("opening", *opening),
        ViewPrim::CurtainLine { curtain, .. } => ("curtain_wall", *curtain),
        ViewPrim::RoomTag { room, .. } => ("room", *room),
        ViewPrim::SectionCut { table, element, .. } | ViewPrim::Edge { table, element, .. } => {
            (*table, *element)
        }
        _ => return None,
    })
}

/// Level of detail of component symbols on the level plan.
pub(crate) fn plan_lod(db: &Connection, level: i64) -> Result<Lod, Error> {
    Ok(state(db, ViewKey::Plan(level))?.effective.detail.lod())
}

impl Model {
    pub fn view_gfx(&self, v: ViewKey) -> Result<ViewGfxState, Error> {
        state(&self.db, v)
    }

    /// View range of a level's plan (template applied); the default when nothing is stored.
    pub fn plan_view_range(&self, level: i64) -> ViewRange {
        state(&self.db, ViewKey::Plan(level))
            .map_or_else(|_| ViewRange::default(), |s| s.effective.range)
    }

    /// New view range: one operation, refused with a reason when unusable or template-locked.
    pub fn set_view_range(&mut self, v: ViewKey, r: ViewRange) -> Result<(), Error> {
        r.check()?;
        self.edit_view_gfx(v, |g| g.range = r)
    }

    /// A view's component detail.
    pub fn view_lod(&self, v: ViewKey) -> Result<Lod, Error> {
        Ok(state(&self.db, v)?.effective.detail.lod())
    }

    /// New view settings; a field locked by the linked template cannot be changed.
    pub fn set_view_gfx(&mut self, v: ViewKey, g: &ViewGfx) -> Result<(), Error> {
        let g = g.clone().normalized();
        self.op("view_gfx_set", move |db| {
            g.check(db)?;
            let st = state(db, v)?;
            if let Some(f) = st.locked.iter().find(|f| !f.same(&st.effective, &g)) {
                return Err(Error::Conflict(format!(
                    "view template controls '{}'",
                    f.key()
                )));
            }
            let mut own = g.clone();
            own.take_fields(&st.own, &st.locked);
            let inv = write_view(db, v, st.template, &own)?;
            Ok(((), v.parts().1, inv))
        })
    }

    /// Edit the effective view settings in one operation.
    pub fn edit_view_gfx(&mut self, v: ViewKey, f: impl FnOnce(&mut ViewGfx)) -> Result<(), Error> {
        let mut g = state(&self.db, v)?.effective;
        f(&mut g);
        self.set_view_gfx(v, &g)
    }

    /// Hide or show a category in a view.
    pub fn set_view_category_visible(
        &mut self,
        v: ViewKey,
        category: &str,
        visible: bool,
    ) -> Result<(), Error> {
        self.edit_view_gfx(v, |g| {
            g.categories
                .entry(category.to_string())
                .or_default()
                .visible = visible;
        })
    }

    // ---- templates ----

    pub fn view_templates(&self) -> Result<Vec<ViewTemplate>, Error> {
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM view_template ORDER BY name")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        ids.into_iter()
            .map(|id| need_template(&self.db, id))
            .collect()
    }

    /// A template from the effective view settings.
    pub fn template_from_view(
        &mut self,
        name: &str,
        v: ViewKey,
        fields: &[TemplateField],
    ) -> Result<i64, Error> {
        let spec = state(&self.db, v)?.effective;
        self.add_view_template(name, fields, &spec)
    }

    pub fn add_view_template(
        &mut self,
        name: &str,
        fields: &[TemplateField],
        spec: &ViewGfx,
    ) -> Result<i64, Error> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(FieldErr::TemplateName.into());
        }
        let (fields, spec) = (fields_text(fields), spec.clone().normalized());
        self.op("view_template_add", move |db| {
            spec.check(db)?;
            name_free(db, "view_template", &name, 0)?;
            db.execute(
                "INSERT INTO view_template(name,fields,spec) VALUES (?1,?2,?3)",
                params![name, fields, spec.to_json()],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("view_template", "id", id)))
        })
    }

    /// Template edit: linked views change immediately.
    pub fn edit_view_template(
        &mut self,
        id: i64,
        name: &str,
        fields: &[TemplateField],
        spec: &ViewGfx,
    ) -> Result<(), Error> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(FieldErr::TemplateName.into());
        }
        let (fields, spec) = (fields_text(fields), spec.clone().normalized());
        self.op("view_template_edit", move |db| {
            need_template(db, id)?;
            spec.check(db)?;
            name_free(db, "view_template", &name, id)?;
            let inv = Inv::default().restore(db, "view_template", "id", id)?;
            db.execute(
                "UPDATE view_template SET name=?2, fields=?3, spec=?4 WHERE id=?1",
                params![id, name, fields, spec.to_json()],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Template deletion: linked views keep the same graphics but lose the link.
    pub fn delete_view_template(&mut self, id: i64) -> Result<(), Error> {
        self.op("view_template_delete", move |db| {
            let tpl = need_template(db, id)?;
            let rows: Vec<(i64, String)> = {
                let mut st = db.prepare("SELECT rowid, spec FROM view_gfx WHERE template=?1")?;
                let v = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                v
            };
            let mut inv = Inv::default();
            for (rowid, spec) in rows {
                inv = inv.restore(db, "view_gfx", "rowid", rowid)?;
                let mut own = ViewGfx::from_json(&spec);
                own.take_fields(&tpl.spec, &tpl.fields);
                db.execute(
                    "UPDATE view_gfx SET template=NULL, spec=?2 WHERE rowid=?1",
                    params![rowid, own.to_json()],
                )?;
            }
            inv = inv.restore(db, "view_template", "id", id)?;
            db.execute("DELETE FROM view_template WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    /// One-off application: template fields are copied into views, no link. One operation for all views.
    /// For a view with a linked template the locked fields do not change.
    pub fn apply_view_template(&mut self, id: i64, views: &[ViewKey]) -> Result<(), Error> {
        let views = views.to_vec();
        self.op("view_template_apply", move |db| {
            let tpl = need_template(db, id)?;
            let mut inv = Inv::default();
            for v in &views {
                let st = state(db, *v)?;
                let mut own = st.own.clone();
                own.take_fields(&tpl.spec, &tpl.fields);
                own.take_fields(&st.own, &st.locked);
                inv = write_view(db, *v, st.template, &own)?.then(inv);
            }
            Ok(((), id, inv))
        })
    }

    /// Links views to a template (locking its fields) or unlinks: the view keeps what it
    /// drew. One operation for all views.
    pub fn link_view_template(
        &mut self,
        template: Option<i64>,
        views: &[ViewKey],
    ) -> Result<(), Error> {
        let views = views.to_vec();
        self.op("view_template_link", move |db| {
            if let Some(t) = template {
                need_template(db, t)?;
            }
            let mut inv = Inv::default();
            for v in &views {
                let st = state(db, *v)?;
                let own = if template.is_none() {
                    st.effective
                } else {
                    st.own
                };
                inv = write_view(db, *v, template, &own)?.then(inv);
            }
            Ok(((), template.unwrap_or(0), inv))
        })
    }

    // ---- filters ----

    pub fn view_filters(&self) -> Result<Vec<ViewFilter>, Error> {
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM view_filter ORDER BY name")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for id in ids {
            out.extend(load_filter(&self.db, id)?);
        }
        Ok(out)
    }

    pub fn add_view_filter(&mut self, s: &ViewFilterSpec) -> Result<i64, Error> {
        s.check()?;
        let s = s.clone();
        self.op("view_filter_add", move |db| {
            name_free(db, "view_filter", s.name.trim(), 0)?;
            db.execute(
                "INSERT INTO view_filter(name,def) VALUES (?1,?2)",
                params![s.name.trim(), s.def_json()],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("view_filter", "id", id)))
        })
    }

    pub fn edit_view_filter(&mut self, id: i64, s: &ViewFilterSpec) -> Result<(), Error> {
        s.check()?;
        let s = s.clone();
        self.op("view_filter_edit", move |db| {
            if load_filter(db, id)?.is_none() {
                return Err(Error::Invalid(format!("no view filter {id}")));
            }
            name_free(db, "view_filter", s.name.trim(), id)?;
            let inv = Inv::default().restore(db, "view_filter", "id", id)?;
            db.execute(
                "UPDATE view_filter SET name=?2, def=?3 WHERE id=?1",
                params![id, s.name.trim(), s.def_json()],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Filter deletion: it leaves all views and templates in the same operation.
    pub fn delete_view_filter(&mut self, id: i64) -> Result<(), Error> {
        self.op("view_filter_delete", move |db| {
            let mut inv = Inv::default();
            for (table, key) in [("view_gfx", "rowid"), ("view_template", "id")] {
                let rows: Vec<(i64, String)> = {
                    let mut st = db.prepare(&format!("SELECT {key}, spec FROM {table}"))?;
                    let v = st
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<_, _>>()?;
                    v
                };
                for (k, spec) in rows {
                    let mut g = ViewGfx::from_json(&spec);
                    if g.filters.iter().any(|f| f.filter == id) {
                        inv = inv.restore(db, table, key, k)?;
                        g.filters.retain(|f| f.filter != id);
                        db.execute(
                            &format!("UPDATE {table} SET spec=?2 WHERE {key}=?1"),
                            params![k, g.to_json()],
                        )?;
                    }
                }
            }
            inv = inv.restore(db, "view_filter", "id", id)?;
            db.execute("DELETE FROM view_filter WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    // ---- object styles ----

    /// Project object styles: category -> default graphics.
    pub fn object_styles(&self) -> Result<BTreeMap<String, GfxOverride>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT category, spec FROM object_style")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = BTreeMap::new();
        for x in rows {
            let (c, s) = x?;
            let j: J = serde_json::from_str(&s).unwrap_or(J::Null);
            out.insert(c, GfxOverride::from_json(&j));
        }
        Ok(out)
    }

    /// A category's object style; an empty one is removed.
    pub fn set_object_style(&mut self, category: &str, o: GfxOverride) -> Result<(), Error> {
        self.set_object_styles(&[(category.to_string(), o)])
    }

    /// Several styles in one operation.
    pub fn set_object_styles(&mut self, styles: &[(String, GfxOverride)]) -> Result<(), Error> {
        for (category, o) in styles {
            if !GFX_CATEGORIES.iter().any(|g| g.0 == category) {
                return Err(Error::Invalid(format!("unknown category '{category}'")));
            }
            o.check()?;
        }
        let styles = styles.to_vec();
        self.op("object_style_set", move |db| {
            let mut inv = Inv::default();
            for (category, o) in &styles {
                let old: Option<i64> = db
                    .query_row(
                        "SELECT rowid FROM object_style WHERE category=?1",
                        [category],
                        |r| r.get(0),
                    )
                    .optional()?;
                let one = match old {
                    Some(rowid) => Inv::default().restore(db, "object_style", "rowid", rowid)?,
                    None => Inv::default().sql(
                        "DELETE FROM object_style WHERE category=?1",
                        vec![Value::Text(category.clone())],
                    ),
                };
                inv = one.then(inv);
                if o.is_empty() {
                    db.execute("DELETE FROM object_style WHERE category=?1", [category])?;
                } else {
                    db.execute(
                        "INSERT OR REPLACE INTO object_style(category,spec) VALUES (?1,?2)",
                        params![category, o.to_json().to_string()],
                    )?;
                }
            }
            Ok(((), 0, inv))
        })
    }

    // ---- result for drawing ----

    /// Removes view-hidden elements from the drawing primitives (plan, section, sheet).
    pub fn hide_in_drawing(&self, v: ViewKey, prims: &mut Vec<ViewPrim>) -> Result<(), Error> {
        self.view_paint(v)?.retain_prims(prims);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::tests::room_6x4;
    use crate::{PVal, ParamSet, ParamSpec, ParamType, PlanViewSpec};

    fn rule(param: &str, op: RuleOp, value: &str) -> FilterRule {
        FilterRule {
            param: param.into(),
            op,
            value: value.into(),
        }
    }

    fn ids(m: &Model, sql: &str) -> Vec<i64> {
        let mut st = m.db.prepare(sql).unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap();
        v.map(|x| x.unwrap()).collect()
    }

    fn red() -> GfxOverride {
        GfxOverride {
            fill: Some(0xFF0000),
            line: Some(0xCC0000),
            ..Default::default()
        }
    }

    #[test]
    fn rule_compares_numbers_and_text() {
        let eq = rule("x", RuleOp::Eq, "EI30");
        assert!(eq.test(Some("ei30 ")));
        assert!(!eq.test(Some("EI60")));
        assert!(!eq.test(None), "no parameter: the rule does not apply");
        assert!(!rule("x", RuleOp::Ne, "EI30").test(None));
        assert!(rule("x", RuleOp::Ne, "EI30").test(Some("EI60")));
        assert!(rule("x", RuleOp::Missing, "").test(None));
        assert!(rule("x", RuleOp::Missing, "").test(Some("  ")));
        assert!(rule("x", RuleOp::Exists, "").test(Some("0")));
        assert!(!rule("x", RuleOp::Exists, "").test(None));
        // numbers as numbers, a comma is decimal: 9 < 10, although "9" > "10" as text
        assert!(rule("x", RuleOp::Lt, "10").test(Some("9")));
        assert!(rule("x", RuleOp::Ge, "2.5").test(Some("2,5")));
        assert!(rule("x", RuleOp::Eq, "2.50").test(Some("2,5")));
        assert!(rule("x", RuleOp::Contains, "ei").test(Some("REI 60")));
        assert!(rule("x", RuleOp::NotContains, "ei").test(Some("R 60")));
    }

    #[test]
    fn rule_reads_locale_numbers_and_units() {
        assert_eq!(rule_number("2,5"), Some(2.5));
        assert_eq!(rule_number("1 234,5"), Some(1234.5));
        assert_eq!(rule_number("1\u{a0}234.5"), Some(1234.5));
        assert_eq!(rule_number("1\u{202f}234"), Some(1234.0));
        assert_eq!(rule_number("1,234.5"), Some(1234.5));
        assert_eq!(rule_number("1.234,5"), Some(1234.5));
        assert_eq!(rule_number("1,234,567"), Some(1234567.0));
        assert_eq!(rule_number("\u{2212}3"), Some(-3.0));
        assert_eq!(rule_number("1.2.3"), None, "a version, not a number");
        assert_eq!(rule_number("EI30"), None);
        assert_eq!(rule_number("inf"), None);
        assert_eq!(rule_quantity(crate::test_ru::ru("q_3000_mm")), Some(3.0));
        assert_eq!(rule_quantity(crate::test_ru::ru("q_2_5_m2")), Some(2.5));
        assert_eq!(rule_quantity("3m"), Some(3.0));
        assert_eq!(rule_quantity("3"), None, "the unit is required");
        assert_eq!(rule_quantity("Main"), None);
        // rule unit: the value is converted to SI
        assert!(rule("x", RuleOp::Eq, "3000 mm").test(Some("3 m")));
        assert!(rule("x", RuleOp::Gt, crate::test_ru::ru("q_2500_mm")).test(Some(crate::test_ru::ru("q_3_m"))));
        assert!(rule("x", RuleOp::Lt, "1 000,5").test(Some("999")));
        // a number is not compared with text as a string
        assert!(!rule("x", RuleOp::Gt, "30").test(Some("EI60")));
        assert!(!rule("x", RuleOp::Lt, "30").test(Some("EI60")));
        assert!(rule("x", RuleOp::Lt, "b").test(Some("a")));
    }

    /// A quantity value is stored in SI, the rule is written in units; yes/no is in words.
    #[test]
    fn typed_parameter_compares_with_units_and_yes_no() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let walls = ids(&m, "SELECT id FROM wall ORDER BY id");
        let spec = |name: &str, ptype| ParamSpec {
            name: name.into(),
            ptype,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        };
        let len = m.add_param(&spec("Offset",ParamType::Length)).unwrap();
        let yes = m.add_param(&spec("Bearing",ParamType::YesNo)).unwrap();
        let set = |param, id, value| ParamSet {
            param,
            kind: "wall".into(),
            id,
            value: Some(value),
        };
        m.set_param_values(&[
            set(len, walls[0], PVal::Num(2.1234567)),
            set(yes, walls[1], PVal::Bool(true)),
        ])
        .unwrap();
        let f = m
            .add_view_filter(&ViewFilterSpec {
                name: "f".into(),
                any: true,
                rules: vec![
                    rule("offset", RuleOp::Gt, crate::test_ru::ru("q_2123_mm")),
                    rule("bearing", RuleOp::Eq, crate::test_ru::ru("word_yes")),
                ],
                ..Default::default()
            })
            .unwrap();
        let v = ViewKey::Plan(lv);
        m.edit_view_gfx(v, |g| {
            g.filters = vec![FilterUse {
                filter: f,
                enabled: true,
                visible: true,
                over: red(),
            }];
        })
        .unwrap();
        let p = m.view_paint(v).unwrap();
        assert!(p.elem("wall", walls[0]).is_some(), "2123.4567 mm > 2123 mm");
        assert!(p.elem("wall", walls[1]).is_some(), "yes == 1");
        assert!(p.elem("wall", walls[2]).is_none());
    }

    fn hide_filter(m: &mut Model, v: ViewKey, rules: Vec<FilterRule>) {
        let f = m
            .add_view_filter(&ViewFilterSpec {
                name: "hide".into(),
                categories: vec!["wall".into()],
                any: false,
                rules,
            })
            .unwrap();
        m.edit_view_gfx(v, |g| {
            g.filters = vec![FilterUse {
                filter: f,
                enabled: true,
                visible: false,
                over: GfxOverride::default(),
            }];
        })
        .unwrap();
    }

    #[test]
    fn paint_is_cached_and_updated_by_the_change_journal() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let l2 = m.add_level("L2", 3.0).unwrap();
        let walls = ids(&m, "SELECT id FROM wall ORDER BY id");
        let v = ViewKey::Plan(lv);
        hide_filter(&mut m, v, vec![rule(crate::test_ru::ru("param_level"), RuleOp::Eq, "L2")]);
        let a = m.view_paint(v).unwrap();
        assert!(!a.any_hidden);
        assert!(
            std::sync::Arc::ptr_eq(&a, &m.view_paint(v).unwrap()),
            "no writes: same result"
        );
        // an edit that does not change the result: the same result with no copy
        m.db.execute("UPDATE wall SET height=height+0.5 WHERE id=?1", [walls[1]])
            .unwrap();
        let b = m.view_paint(v).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "the edit changes nothing in the view"
        );
        // moving one wall: it and its opening are recomputed (the door on the first wall)
        m.db.execute("UPDATE wall SET level=?1 WHERE id=?2", [l2, walls[0]])
            .unwrap();
        let c = m.view_paint(v).unwrap();
        assert!(c.any_hidden);
        assert!(c.hidden("wall", walls[0]));
        assert!(!c.hidden("wall", walls[1]));
        let door: i64 = ids(
            &m,
            &format!("SELECT id FROM opening WHERE wall={}", walls[0]),
        )[0];
        assert!(c.hidden("opening", door), "a door does not hang in the air");
        let window = ids(
            &m,
            &format!("SELECT id FROM opening WHERE wall={}", walls[2]),
        )[0];
        assert!(!c.hidden("opening", window));
        // and the same as a full recompute
        assert_eq!(*c, eval_full(&m, v));
        // moving back unhides the opening too; the previous result did not change
        m.db.execute("UPDATE wall SET level=?1 WHERE id=?2", [lv, walls[0]])
            .unwrap();
        let d = m.view_paint(v).unwrap();
        assert!(!d.any_hidden && !d.hidden("opening", door));
        assert_eq!(*d, eval_full(&m, v));
        assert!(c.hidden("wall", walls[0]));
        // a deleted wall drops out of the result
        m.db.execute("UPDATE wall SET level=?1 WHERE id=?2", [l2, walls[3]])
            .unwrap();
        assert!(m.view_paint(v).unwrap().hidden("wall", walls[3]));
        m.db.execute("DELETE FROM wall WHERE id=?1", [walls[3]])
            .unwrap();
        let e = m.view_paint(v).unwrap();
        assert!(!e.hidden("wall", walls[3]));
        assert_eq!(*e, eval_full(&m, v));
    }

    fn eval_full(m: &Model, v: ViewKey) -> ViewPaint {
        eval::full(m, v).unwrap()
    }

    /// A rolled-back transaction leaves no traces in the cache.
    #[test]
    fn paint_read_inside_a_transaction_is_not_cached() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let walls = ids(&m, "SELECT id FROM wall ORDER BY id");
        let v = ViewKey::Plan(lv);
        hide_filter(
            &mut m,
            v,
            vec![rule("id", RuleOp::Eq, &walls[0].to_string())],
        );
        assert!(m.view_paint(v).unwrap().hidden("wall", walls[0]));
        m.db.execute_batch("BEGIN").unwrap();
        m.db.execute("DELETE FROM view_gfx", []).unwrap();
        assert!(!m.view_paint(v).unwrap().hidden("wall", walls[0]));
        m.db.execute_batch("ROLLBACK").unwrap();
        assert!(m.view_paint(v).unwrap().hidden("wall", walls[0]));
    }

    #[test]
    fn tag_leaves_with_its_host_and_names_conflict_clearly() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let walls = ids(&m, "SELECT id FROM wall ORDER BY id");
        m.add_tags(&[crate::TagSpec {
            level: lv,
            host: TagHost::Wall(walls[0]),
            offset: (0.0, 0.5),
            leader: false,
        }])
        .unwrap();
        let spec = PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        };
        let tags = |m: &Model| {
            m.plan_view(&spec)
                .unwrap()
                .count(|p| matches!(p, ViewPrim::Tag { .. }))
        };
        assert_eq!(tags(&m), 1);
        m.set_view_category_visible(ViewKey::Plan(lv), "wall", false)
            .unwrap();
        assert_eq!(tags(&m), 0);
        m.undo().unwrap();
        assert_eq!(tags(&m), 1);
        // a taken name is a clear conflict, not a database error
        let s = ViewFilterSpec {
            name: "F".into(),
            ..Default::default()
        };
        let f = m.add_view_filter(&s).unwrap();
        assert!(matches!(m.add_view_filter(&s), Err(Error::Conflict(_))));
        m.edit_view_filter(f, &s).unwrap();
        let t = m.add_view_template("T", &[], &ViewGfx::default()).unwrap();
        assert!(matches!(
            m.add_view_template("T", &[], &ViewGfx::default()),
            Err(Error::Conflict(_))
        ));
        m.edit_view_template(t, "T", &[], &ViewGfx::default())
            .unwrap();
    }

    /// Deleting a level removes its view settings and restores them on undo.
    #[test]
    fn deleted_view_takes_its_graphics() {
        let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
        let l2 = m.add_level("L2", 3.0).unwrap();
        m.edit_view_gfx(ViewKey::Plan(l2), |g| g.thin_lines = true)
            .unwrap();
        let rows = |m: &Model| ids(m, &format!("SELECT ref FROM view_gfx WHERE ref={l2}")).len();
        assert_eq!(rows(&m), 1);
        m.delete_level(l2).unwrap();
        assert_eq!(rows(&m), 0);
        m.undo().unwrap();
        assert_eq!(rows(&m), 1);
    }

    #[test]
    fn filter_combines_rules_and_limits_categories() {
        let get = |p: &str| match p {
            "fire" => Some("EI30".to_string()),
            "h" => Some("3".to_string()),
            _ => None,
        };
        let mut f = ViewFilterSpec {
            name: "F".into(),
            categories: vec!["wall".into()],
            any: false,
            rules: vec![rule("fire", RuleOp::Eq, "EI30"), rule("h", RuleOp::Gt, "4")],
        };
        assert!(!f.matches("wall", &get), "all rules are required");
        f.any = true;
        assert!(f.matches("wall", &get));
        assert!(!f.matches("door", &get), "category is out of the filter");
        f.rules.clear();
        assert!(f.matches("wall", &get), "no rules: the whole category");
    }

    #[test]
    fn filter_paints_by_property_and_project_parameter() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let walls = ids(&m, "SELECT id FROM wall ORDER BY id");
        // an element property (import): the key is case-insensitive
        m.set_prop(
            "wall",
            walls[0],
            "Fire Rating",
            Some((Value::Text("EI30".into()), "")),
        )
        .unwrap();
        // a project parameter on another wall
        let p = m
            .add_param(&ParamSpec {
                // a name of its own: the property above is "Fire Rating"
                name: "Fire resistance".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        m.set_param_values(&[ParamSet {
            param: p,
            kind: "wall".into(),
            id: walls[1],
            value: Some(PVal::Text("EI30".into())),
        }])
        .unwrap();
        let by_prop = m
            .add_view_filter(&ViewFilterSpec {
                name: "EI30 prop".into(),
                categories: vec!["wall".into()],
                any: false,
                rules: vec![rule("fire rating", RuleOp::Eq, "ei30")],
            })
            .unwrap();
        let by_param = m
            .add_view_filter(&ViewFilterSpec {
                name: "EI30 param".into(),
                categories: vec![],
                any: false,
                rules: vec![rule("fire resistance", RuleOp::Eq, "EI30")],
            })
            .unwrap();
        let v = ViewKey::Plan(lv);
        let use_f = |filter, over, visible| FilterUse {
            filter,
            enabled: true,
            visible,
            over,
        };
        m.edit_view_gfx(v, |g| {
            g.filters = vec![
                use_f(by_prop, red(), true),
                use_f(by_param, GfxOverride::default(), false),
            ];
        })
        .unwrap();
        let p = m.view_paint(v).unwrap();
        let at = |id: i64| p.elem("wall", id);
        assert_eq!(at(walls[0]).unwrap().over, red());
        assert!(!at(walls[0]).unwrap().hidden);
        assert!(at(walls[1]).unwrap().hidden, "hiding filter");
        assert_eq!(at(walls[2]), None, "no such value: untouched");
        // the same filter works in a 3D view too (a different view key, the same data)
        m.edit_view_gfx(ViewKey::ThreeD, |g| {
            g.filters = vec![use_f(by_prop, red(), true)]
        })
        .unwrap();
        let p3 = m.view_paint(ViewKey::ThreeD).unwrap();
        assert!(p3.elems.contains_key(&("wall", walls[0])));
        // built-in parameters: an opening's level via the wall, type, material
        m.edit_view_gfx(v, |g| g.filters.clear()).unwrap();
        let door = m
            .add_view_filter(&ViewFilterSpec {
                name: "L1 doors".into(),
                categories: vec!["door".into()],
                any: false,
                rules: vec![rule(crate::test_ru::ru("param_level"), RuleOp::Eq, "L1")],
            })
            .unwrap();
        let mat = m
            .add_view_filter(&ViewFilterSpec {
                name: "brick".into(),
                categories: vec!["wall".into()],
                any: false,
                rules: vec![
                    rule("material", RuleOp::Eq, "brick"),
                    rule("type", RuleOp::Eq, "W"),
                ],
            })
            .unwrap();
        m.edit_view_gfx(v, |g| {
            g.filters = vec![use_f(door, red(), true), use_f(mat, red(), true)];
        })
        .unwrap();
        let p = m.view_paint(v).unwrap();
        let tables: Vec<&str> = p.elems.keys().map(|k| k.0).collect();
        assert_eq!(
            tables.iter().filter(|t| **t == "opening").count(),
            1,
            "the door, not the window"
        );
        assert_eq!(tables.iter().filter(|t| **t == "wall").count(), 4);
    }

    #[test]
    fn overrides_stack_object_style_category_filter() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let w = ids(&m, "SELECT id FROM wall ORDER BY id")[0];
        m.set_prop("wall", w, "F", Some((Value::Text("1".into()), "")))
            .unwrap();
        m.set_object_style(
            "wall",
            GfxOverride {
                line: Some(0x111111),
                fill: Some(0x222222),
                line_weight: Some(3.0),
                transparency: None,
            },
        )
        .unwrap();
        let v = ViewKey::Plan(lv);
        let f1 = m
            .add_view_filter(&ViewFilterSpec {
                name: "a".into(),
                rules: vec![rule("f", RuleOp::Eq, "1")],
                ..Default::default()
            })
            .unwrap();
        let f2 = m
            .add_view_filter(&ViewFilterSpec {
                name: "b".into(),
                rules: vec![rule("f", RuleOp::Exists, "")],
                ..Default::default()
            })
            .unwrap();
        m.edit_view_gfx(v, |g| {
            g.categories.insert(
                "wall".into(),
                CatGfx {
                    visible: true,
                    over: GfxOverride {
                        fill: Some(0x333333),
                        ..Default::default()
                    },
                },
            );
            g.filters = vec![
                FilterUse {
                    filter: f1,
                    enabled: true,
                    visible: true,
                    over: GfxOverride {
                        line: Some(0x444444),
                        ..Default::default()
                    },
                },
                FilterUse {
                    filter: f2,
                    enabled: true,
                    visible: true,
                    over: GfxOverride {
                        line: Some(0x555555),
                        transparency: Some(40),
                        ..Default::default()
                    },
                },
            ];
        })
        .unwrap();
        let p = m.view_paint(v).unwrap();
        let o = p.elems[&("wall", w)].over;
        assert_eq!(o.line, Some(0x444444), "top filter wins");
        assert_eq!(
            o.transparency,
            Some(40),
            "second filter fills what the first left"
        );
        assert_eq!(o.fill, Some(0x333333), "category over object style");
        assert_eq!(o.line_weight, Some(3.0), "object style at the bottom");
        // another wall, no filters: category and style
        let w2 = ids(&m, "SELECT id FROM wall ORDER BY id")[1];
        assert_eq!(p.elems[&("wall", w2)].over.line, Some(0x111111));
        // a disabled filter has no effect; removing a style is undoable
        m.edit_view_gfx(v, |g| g.filters.iter_mut().for_each(|f| f.enabled = false))
            .unwrap();
        let p = m.view_paint(v).unwrap();
        assert_eq!(p.elems[&("wall", w)].over.line, Some(0x111111));
        m.set_object_style("wall", GfxOverride::default()).unwrap();
        assert!(m.object_styles().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.object_styles().unwrap().len(), 1);
    }

    fn sample_gfx() -> ViewGfx {
        let mut g = ViewGfx {
            detail: DetailLevel::Fine,
            thin_lines: true,
            ..Default::default()
        };
        g.categories.insert(
            "door".into(),
            CatGfx {
                visible: false,
                over: GfxOverride::default(),
            },
        );
        g
    }

    /// A view configured once and applied to all floors in one operation.
    fn range(cut: f64, bottom: f64, depth: f64) -> ViewRange {
        ViewRange {
            cut,
            bottom,
            depth,
            top: None,
        }
    }

    #[test]
    fn default_range_keeps_old_cut() {
        let (m, l1) = room_6x4(&[("Brick", 0.2)]);
        assert_eq!(ViewRange::default(), range(1.2, 0.0, 0.0));
        assert_eq!(ViewGfx::default().range.cut, ViewRange::DEFAULT_CUT);
        assert_eq!(m.plan_view_range(l1), ViewRange::default());
        assert_eq!(m.plan_view_range(9999), ViewRange::default(), "unknown level");
        // a default range is not written, so stored specs of untouched views stay as they were
        let j: J = serde_json::from_str(&ViewGfx::default().to_json()).unwrap();
        assert!(j.get("range").is_none());
        let g = ViewGfx {
            range: range(0.9, -0.3, 0.5),
            ..Default::default()
        };
        assert_eq!(ViewGfx::from_json(&g.to_json()), g, "round trip");
    }

    #[test]
    fn old_view_json_reads_default() {
        let old = r#"{"detail":"fine","thin_lines":true,"categories":{},"filters":[]}"#;
        let g = ViewGfx::from_json(old);
        assert_eq!(g.detail, DetailLevel::Fine);
        assert_eq!(g.range, ViewRange::default());
        // each key is optional on its own
        let part = r#"{"range":{"cut":2.0}}"#;
        assert_eq!(ViewGfx::from_json(part).range, range(2.0, 0.0, 0.0));
        let part = r#"{"range":{"bottom":-0.5,"depth":1.0}}"#;
        assert_eq!(ViewGfx::from_json(part).range, range(1.2, -0.5, 1.0));
        // an unusable stored range draws like before instead of breaking the view
        let bad = r#"{"range":{"cut":0.5,"bottom":1.0}}"#;
        assert_eq!(ViewGfx::from_json(bad).range, ViewRange::default());
        let bad = r#"{"range":"x"}"#;
        assert_eq!(ViewGfx::from_json(bad).range, ViewRange::default());
    }

    #[test]
    fn top_plane_stored_only_when_set_and_checked() {
        let open = range(0.9, -0.2, 0.4);
        assert!(open.to_json().get("top").is_none(), "older specs read unchanged");
        let topped = ViewRange {
            top: Some(2.5),
            ..open
        };
        assert_eq!(ViewRange::from_json(&topped.to_json()), topped);
        let low = ViewRange {
            top: Some(0.5),
            ..open
        };
        assert_eq!(low.issue(), Some(RangeIssue::TopBelowCut));
        let inf = ViewRange {
            top: Some(f64::INFINITY),
            ..open
        };
        assert_eq!(inf.issue(), Some(RangeIssue::NotFinite));
    }

    #[test]
    fn view_range_is_saved_and_undone_in_one_step() {
        let (mut m, l1) = room_6x4(&[("Brick", 0.2)]);
        let v = ViewKey::Plan(l1);
        m.set_view_gfx(v, &sample_gfx()).unwrap();
        m.set_view_range(v, range(0.9, -0.2, 0.4)).unwrap();
        assert_eq!(m.plan_view_range(l1), range(0.9, -0.2, 0.4));
        let s = m.view_gfx(v).unwrap();
        assert_eq!(s.effective.detail, DetailLevel::Fine, "other settings kept");
        assert!(s.effective.thin_lines);
        m.undo().unwrap();
        assert_eq!(m.plan_view_range(l1), ViewRange::default());
        assert_eq!(m.view_gfx(v).unwrap().effective, sample_gfx());
    }

    #[test]
    fn view_range_survives_save_and_reopen() {
        let (mut m, l1) = room_6x4(&[("Brick", 0.2)]);
        let v = ViewKey::Plan(l1);
        m.set_view_range(v, range(0.9, -0.2, 0.4)).unwrap();
        let dir = std::env::temp_dir().join(format!("ddc_vr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vr.ddc");
        m.backup_to(&path).unwrap();
        let re = Model::open(path.to_str().unwrap()).unwrap();
        assert_eq!(re.plan_view_range(l1), range(0.9, -0.2, 0.4));
        drop(re);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_range_refused_nothing_written() {
        let (mut m, l1) = room_6x4(&[("Brick", 0.2)]);
        let v = ViewKey::Plan(l1);
        m.set_view_range(v, range(1.5, 0.0, 0.0)).unwrap();
        let before = m.view_gfx(v).unwrap();
        let bad = [
            (range(0.5, 0.5, 0.0), RangeIssue::CutNotAboveBottom),
            (range(0.5, 1.0, 0.0), RangeIssue::CutNotAboveBottom),
            (range(1.2, 0.0, -0.1), RangeIssue::NegativeDepth),
            (range(f64::NAN, 0.0, 0.0), RangeIssue::NotFinite),
            (range(1.2, f64::NEG_INFINITY, 0.0), RangeIssue::NotFinite),
        ];
        for (r, why) in bad {
            assert_eq!(r.issue(), Some(why));
            let e = m.set_view_range(v, r).unwrap_err();
            assert!(e.to_string().contains(why.key()), "{e}");
            let g = ViewGfx {
                range: r,
                ..Default::default()
            };
            assert!(m.set_view_gfx(v, &g).is_err());
            assert!(m.add_view_template("Bad", &[TemplateField::ViewRange], &g).is_err());
        }
        assert_eq!(m.view_gfx(v).unwrap(), before, "nothing written");
        assert!(m.view_templates().unwrap().is_empty());
        // no undo step was added: one undo takes back the last good change
        m.undo().unwrap();
        assert_eq!(m.plan_view_range(l1), ViewRange::default());
    }

    #[test]
    fn template_applies_view_range() {
        let (mut m, l1) = room_6x4(&[("Brick", 0.2)]);
        let l2 = m.add_level("L2", 3.0).unwrap();
        let (p1, p2) = (ViewKey::Plan(l1), ViewKey::Plan(l2));
        let r = range(0.8, -0.4, 0.6);
        m.set_view_range(p1, r).unwrap();
        let t = m
            .template_from_view("Plan", p1, &[TemplateField::ViewRange])
            .unwrap();
        assert_eq!(m.view_templates().unwrap()[0].spec.range, r);
        // one-time apply copies the range in one undo step
        m.apply_view_template(t, &[p2]).unwrap();
        assert_eq!(m.plan_view_range(l2), r);
        m.undo().unwrap();
        assert_eq!(m.plan_view_range(l2), ViewRange::default());
        // a linked template controls the range and refuses a local change
        m.link_view_template(Some(t), &[p2]).unwrap();
        assert_eq!(m.plan_view_range(l2), r);
        assert!(m.view_gfx(p2).unwrap().locked.contains(&TemplateField::ViewRange));
        assert!(matches!(
            m.set_view_range(p2, range(2.0, 0.0, 0.0)),
            Err(Error::Conflict(_))
        ));
        assert_eq!(m.plan_view_range(l2), r, "refused change writes nothing");
        // editing the template moves every linked plan
        let mut spec = m.view_templates().unwrap()[0].spec.clone();
        spec.range = range(1.6, 0.1, 0.0);
        m.edit_view_template(t, "Plan", &[TemplateField::ViewRange], &spec)
            .unwrap();
        assert_eq!(m.plan_view_range(l2), range(1.6, 0.1, 0.0));
        // unlinking keeps what the plan drew
        m.link_view_template(None, &[p2]).unwrap();
        assert_eq!(m.plan_view_range(l2), range(1.6, 0.1, 0.0));
        assert_eq!(TemplateField::from_key("view_range"), Some(TemplateField::ViewRange));
    }

    #[test]
    fn template_created_from_view_and_applied_to_many() {
        let (mut m, l1) = room_6x4(&[("Brick", 0.2)]);
        let l2 = m.add_level("L2", 3.0).unwrap();
        let l3 = m.add_level("L3", 6.0).unwrap();
        let src = ViewKey::Plan(l1);
        m.set_view_gfx(src, &sample_gfx()).unwrap();
        let t = m
            .template_from_view(
                "Floor",
                src,
                &[TemplateField::Detail, TemplateField::Categories],
            )
            .unwrap();
        assert_eq!(
            m.view_templates().unwrap()[0].spec.detail,
            DetailLevel::Fine
        );
        // a field the template does not own is untouched: thin lines are not part of the template
        let many = [ViewKey::Plan(l2), ViewKey::Plan(l3), ViewKey::ThreeD];
        m.edit_view_gfx(many[0], |g| g.thin_lines = false).unwrap();
        m.apply_view_template(t, &many).unwrap();
        for v in many {
            let s = m.view_gfx(v).unwrap();
            assert_eq!(s.effective.detail, DetailLevel::Fine, "{v:?}");
            assert!(!s.effective.categories["door"].visible);
            assert!(!s.effective.thin_lines);
            assert_eq!(s.template, None, "one-time apply leaves no link");
        }
        assert_eq!(m.view_lod(many[1]).unwrap(), Lod::Fine);
        // one operation: one undo restores all views
        m.undo().unwrap();
        assert_eq!(m.view_gfx(many[1]).unwrap().effective, ViewGfx::default());
        assert_eq!(m.view_gfx(many[2]).unwrap().effective, ViewGfx::default());
        assert!(m.apply_view_template(999, &many).is_err());
    }

    #[test]
    fn linked_template_locks_fields() {
        let (mut m, l1) = room_6x4(&[("Brick", 0.2)]);
        let l2 = m.add_level("L2", 3.0).unwrap();
        let t = m
            .add_view_template("T", &[TemplateField::Detail], &sample_gfx())
            .unwrap();
        let v = ViewKey::Plan(l2);
        m.edit_view_gfx(v, |g| g.detail = DetailLevel::Coarse)
            .unwrap();
        m.link_view_template(Some(t), &[v, ViewKey::Plan(l1)])
            .unwrap();
        let s = m.view_gfx(v).unwrap();
        assert_eq!(s.template, Some(t));
        assert_eq!(s.locked, [TemplateField::Detail]);
        assert_eq!(s.effective.detail, DetailLevel::Fine);
        assert_eq!(
            s.own.detail,
            DetailLevel::Coarse,
            "own value is kept under the lock"
        );
        // a locked field is not editable, an open one is
        let locked = m.edit_view_gfx(v, |g| g.detail = DetailLevel::Coarse);
        assert!(matches!(locked, Err(Error::Conflict(_))));
        m.edit_view_gfx(v, |g| g.thin_lines = true).unwrap();
        assert!(m.view_gfx(v).unwrap().effective.thin_lines);
        assert_eq!(m.view_gfx(v).unwrap().effective.detail, DetailLevel::Fine);
        // a template edit is immediately visible in the linked view
        let mut sp = sample_gfx();
        sp.detail = DetailLevel::Medium;
        m.edit_view_template(t, "T", &[TemplateField::Detail], &sp)
            .unwrap();
        assert_eq!(m.view_gfx(v).unwrap().effective.detail, DetailLevel::Medium);
        // unlinking keeps what the view drew and unlocks the field
        m.link_view_template(None, &[v]).unwrap();
        let s = m.view_gfx(v).unwrap();
        assert_eq!((s.template, s.own.detail), (None, DetailLevel::Medium));
        m.edit_view_gfx(v, |g| g.detail = DetailLevel::Coarse)
            .unwrap();
        // deleting a template unlinks the remaining views, same graphics; undo restores the link
        let other = ViewKey::Plan(l1);
        m.delete_view_template(t).unwrap();
        let s = m.view_gfx(other).unwrap();
        assert_eq!(
            (s.template, s.effective.detail),
            (None, DetailLevel::Medium)
        );
        m.undo().unwrap();
        assert_eq!(m.view_gfx(other).unwrap().template, Some(t));
    }

    #[test]
    fn deleting_a_filter_removes_it_from_views_and_templates() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let f = m
            .add_view_filter(&ViewFilterSpec {
                name: "f".into(),
                ..Default::default()
            })
            .unwrap();
        let u = FilterUse {
            filter: f,
            enabled: true,
            visible: false,
            over: GfxOverride::default(),
        };
        let v = ViewKey::Plan(lv);
        m.edit_view_gfx(v, |g| g.filters = vec![u]).unwrap();
        let spec = m.view_gfx(v).unwrap().effective;
        let t = m
            .add_view_template("T", &[TemplateField::Filters], &spec)
            .unwrap();
        m.delete_view_filter(f).unwrap();
        assert!(m.view_gfx(v).unwrap().effective.filters.is_empty());
        assert!(m.view_templates().unwrap()[0].spec.filters.is_empty());
        m.undo().unwrap();
        assert_eq!(m.view_gfx(v).unwrap().effective.filters, [u]);
        assert_eq!(m.view_templates().unwrap()[0].id, t);
        assert_eq!(m.view_templates().unwrap()[0].spec.filters, [u]);
        // a nonexistent filter cannot be added to a view
        let bad = FilterUse { filter: 999, ..u };
        assert!(m.edit_view_gfx(v, |g| g.filters = vec![bad]).is_err());
    }

    #[test]
    fn hidden_category_leaves_the_plan_drawing() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let spec = PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        };
        let before = m.plan_view(&spec).unwrap();
        assert!(before.count(|p| matches!(p, ViewPrim::DoorLeaf { .. })) > 0);
        let v = ViewKey::Plan(lv);
        m.set_view_category_visible(v, "door", false).unwrap();
        let p = m.plan_view(&spec).unwrap();
        assert_eq!(p.count(|p| matches!(p, ViewPrim::DoorLeaf { .. })), 0);
        assert!(
            p.count(|p| matches!(p, ViewPrim::WindowLine { .. })) > 0,
            "windows stay"
        );
        assert!(p.count(|p| matches!(p, ViewPrim::Cut { .. })) > 0);
        assert_eq!(m.view_paint(v).unwrap().hidden_cats, ["door"]);
        m.undo().unwrap();
        assert_eq!(m.plan_view(&spec).unwrap(), before);
    }

    #[test]
    fn json_round_trip_and_tolerant_read() {
        let mut g = sample_gfx();
        g.filters.push(FilterUse {
            filter: 7,
            enabled: false,
            visible: false,
            over: GfxOverride {
                line: Some(1),
                line_weight: Some(1.5),
                fill: Some(2),
                transparency: Some(30),
            },
        });
        assert_eq!(ViewGfx::from_json(&g.to_json()), g);
        assert_eq!(ViewGfx::from_json("not json"), ViewGfx::default());
        assert_eq!(ViewGfx::from_json("{\"future\":1}"), ViewGfx::default());
    }
}
