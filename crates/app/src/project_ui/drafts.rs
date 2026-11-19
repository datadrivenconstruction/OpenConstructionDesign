// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Draft editors whose edits stay outside the model until applied: family tabs, the open line
//! pattern or hatch entry, the view range line styles, the view graphics window (view, filter,
//! object styles), the material card, the type dialog, the parameter editor and the project
//! information window. Close, New and Open ask about them through the one unsaved-changes
//! question.

use ddc_model::Model;
use ddc_ui::{Catalog, Lang};

use crate::editor::Editor;
use crate::shell::EditorApp;

/// One editor holding edits the project file does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Draft {
    Family(String),
    Line(String),
    Styles,
    /// View graphics of the named view.
    View(String),
    /// Filter rule editor; an empty name is a new filter not named yet.
    Filter(String),
    ObjectStyles,
    /// Material card, by the stored material name.
    Material(String),
    /// Type dialog, by the stored type name.
    Type(String),
    /// Parameter editor, by family name.
    Params(String),
    /// Project information window with typed values.
    ProjectInfo,
    /// Schedules window: staged cells and the unsaved definition form, counted.
    Schedules(usize),
}

/// Every draft: families in tab order, the library entry, the line styles, then the view
/// graphics, material, type, parameter and project information windows.
pub fn list(m: &Model, ru: bool) -> Vec<Draft> {
    let info = crate::data_ui::info_draft(m);
    collect(ru, crate::shell::panels::browser::drafts(m), info)
}

/// `list` with the model-reading checks given (type, parameters, project information); the
/// rest is memory.
fn collect(ru: bool, (ty, params): (Option<String>, Option<String>), info: bool) -> Vec<Draft> {
    let mut v: Vec<Draft> = crate::family_editor::draft_names()
        .into_iter()
        .map(Draft::Family)
        .collect();
    let (line, styles) = crate::range_ui::drafts(ru);
    v.extend(line.map(Draft::Line));
    if styles {
        v.push(Draft::Styles);
    }
    let (view, filter, objects) = crate::view_gfx_ui::vv_win::drafts(ru);
    v.extend(view.map(Draft::View));
    v.extend(filter.map(Draft::Filter));
    if objects {
        v.push(Draft::ObjectStyles);
    }
    v.extend(crate::view_gfx_ui::materials_win::draft_name().map(Draft::Material));
    v.extend(ty.map(Draft::Type));
    v.extend(params.map(Draft::Params));
    if info {
        v.push(Draft::ProjectInfo);
    }
    let sched = crate::schedule::multi_ui::draft_count();
    if sched > 0 {
        v.push(Draft::Schedules(sched));
    }
    v
}

/// How many drafts; asked every frame (title, close checks), so the model-reading part is
/// remembered per `Editor::cache_key` and draft value.
pub fn count(ed: &Editor) -> usize {
    let (ty, params) = crate::shell::panels::browser::drafts_at(ed);
    let info = crate::data_ui::info_draft_at(&ed.model, ed.cache_key());
    collect(false, (ty, params), info).len()
}

pub fn label(c: &Catalog, d: &Draft) -> String {
    let named = |k: &str, n: &str| c.tf(k, &[("name", n)]);
    match d {
        Draft::Family(n) => named("drafts.draft.family", n),
        Draft::Line(n) => named("drafts.draft.line", n),
        Draft::Styles => c.t("drafts.draft.styles"),
        Draft::View(n) => named("drafts.draft.view", n),
        Draft::Filter(n) if n.is_empty() => c.t("drafts.draft.filter_new"),
        Draft::Filter(n) => named("drafts.draft.filter", n),
        Draft::ObjectStyles => c.t("drafts.draft.object_styles"),
        Draft::Material(n) => named("drafts.draft.material", &material_name(c, n)),
        Draft::Type(n) => named("drafts.draft.type", n),
        Draft::Params(n) => named("drafts.draft.params", n),
        Draft::ProjectInfo => c.t("drafts.draft.info"),
        Draft::Schedules(n) => c.tf("schm.ui.draft_item", &[("n", &n.to_string())]),
    }
}

/// A built-in material shows its catalog name, an own one its name as typed.
fn material_name(c: &Catalog, n: &str) -> String {
    let key = format!("mat.{n}");
    let s = c.t(&key);
    if s == key {
        n.to_owned()
    } else {
        s
    }
}

/// The drafts as lines of the question, in the catalog's language.
pub fn labels(c: &Catalog, m: &Model) -> Vec<String> {
    list(m, c.lang == Lang::Ru).iter().map(|d| label(c, d)).collect()
}

/// Discard: every draft goes, as Discard in its own window would drop it.
pub fn discard_all() {
    crate::family_editor::discard_drafts();
    crate::range_ui::discard_drafts();
    crate::view_gfx_ui::vv_win::discard_drafts();
    crate::view_gfx_ui::materials_win::discard_drafts();
    crate::shell::panels::browser::discard_drafts();
    crate::data_ui::discard_drafts();
    crate::schedule::multi_ui::discard_drafts();
}

/// After Save all wrote the project (or had nothing to write): drops what cannot be stored.
pub fn saved() {
    crate::family_editor::drop_sketches();
}

/// Save: every draft goes into the model first, so the project save writes it. False when one
/// was refused; that editor is shown and the rest stay drafts. Sketches stay until `saved`.
pub fn save_all(a: &mut EditorApp) -> bool {
    let (ed, cat) = (&mut a.ed, &a.sh.cat);
    crate::family_editor::save_drafts(ed, cat)
        && crate::range_ui::save_drafts(ed)
        && crate::view_gfx_ui::vv_win::save_drafts(ed)
        && crate::view_gfx_ui::materials_win::save_drafts(ed)
        && crate::shell::panels::browser::save_drafts(ed, cat)
        && crate::data_ui::save_drafts(ed)
        && crate::schedule::multi_ui::save_drafts(ed)
}
