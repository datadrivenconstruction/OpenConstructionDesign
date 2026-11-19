//! Cut and projection line styles per category of the plan, with halftone. The live plan and the
//! sheets read the same overrides (`range_layer`, `ViewStyle`). Apply is one undo step.

use ddc_model::drawq::{LineStyle, PatternLib, PenTable, StyleMap, StyleOverride};
use ddc_model::ViewKey;
use ddc_ui::theme::space;
use ddc_ui::widgets;
use ddc_ui::Theme;

use crate::editor::Editor;
use crate::ui_kit as kit;
use crate::view_gfx_ui::run;

/// Catalog text in the window's own language.
fn t(ru: bool, key: &str) -> String {
    crate::tr_lang(ru, key, &[])
}

const PICK_W: f32 = 104.0;

/// Categories that have a line in a plan, in the order of the list.
pub const CATS: [&str; 14] = [
    "wall",
    "door",
    "window",
    "curtain_wall",
    "slab",
    "col",
    "beam",
    "roof",
    "ceiling",
    "stair",
    "railing",
    "mep_run",
    "comp",
    "shaft",
];

#[derive(Clone, Debug, Default)]
pub struct Styles {
    pub saved: StyleMap,
    pub draft: StyleMap,
    /// Level the draft was loaded from; an unsaved draft stays bound to it when the level changes.
    pub level: Option<i64>,
}

impl Styles {
    pub fn load(ed: &Editor, level: i64) -> Self {
        let m = ed.model.view_styles(ViewKey::Plan(level)).unwrap_or_default();
        Self {
            saved: m.clone(),
            draft: m,
            level: Some(level),
        }
    }

    /// Level the draft belongs to (`cur` when it was never loaded).
    pub fn level_or(&self, cur: i64) -> i64 {
        self.level.unwrap_or(cur)
    }

    /// On a level change: a clean draft follows the new level, an unsaved one is kept as is.
    pub fn follow_level(&mut self, ed: &Editor, level: i64) {
        if self.level != Some(level) && !self.changed() {
            *self = Self::load(ed, level);
        }
    }

    /// The draft without empty overrides, as it will be stored.
    pub fn cleaned(&self) -> StyleMap {
        self.draft
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect()
    }

    pub fn changed(&self) -> bool {
        self.cleaned() != self.saved
    }

    /// Stores the draft as one model operation; false (draft kept) when it was refused.
    pub fn apply(&mut self, ed: &mut Editor, level: i64) -> bool {
        let map = self.cleaned();
        let m2 = map.clone();
        if run(ed, move |m| m.set_view_styles(ViewKey::Plan(level), &m2)) {
            self.saved = map;
            true
        } else {
            false
        }
    }

    /// Back to what is stored.
    pub fn revert(&mut self) {
        self.draft = self.saved.clone();
    }
}

/// Pen change: `None` goes back to the default style, a name keeps the pattern already chosen.
pub fn with_pen(cur: Option<LineStyle>, pen: Option<&str>) -> Option<LineStyle> {
    match (cur, pen) {
        (_, None) => None,
        (None, Some(p)) => Some(LineStyle::new(p, "solid")),
        (Some(mut s), Some(p)) => {
            s.pen = p.to_string();
            Some(s)
        }
    }
}

/// Pattern change; without a pen of its own the style stays at the default.
pub fn with_pattern(cur: Option<LineStyle>, pattern: &str) -> Option<LineStyle> {
    cur.map(|mut s| {
        s.pattern = pattern.to_string();
        s
    })
}

fn cat_label(ru: bool, key: &str) -> String {
    let k = format!("draw_quality.cat.{key}");
    let s = t(ru, &k);
    if s == k {
        key.to_string()
    } else {
        s
    }
}

fn pen_label(ru: bool, name: &str) -> String {
    let k = format!("draw_quality.pen.{name}");
    let s = t(ru, &k);
    if s == k {
        name.to_string()
    } else {
        s
    }
}

fn pat_label(ru: bool, name: &str) -> String {
    let k = format!("draw_quality.pat.{name}");
    let s = t(ru, &k);
    if s == k {
        name.to_string()
    } else {
        s
    }
}

/// Pen and pattern pickers of one style.
fn style_cells(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    id: (&str, &str),
    cur: &mut Option<LineStyle>,
    pens: &PenTable,
    pats: &PatternLib,
) {
    // item 0 is the default style, the rest follow the pen table
    let mut pen_items = vec![t(ru, "draw_quality.style.default")];
    pen_items.extend(pens.pens.iter().map(|p| pen_label(ru, &p.name)));
    let pen_refs: Vec<&str> = pen_items.iter().map(String::as_str).collect();
    let mut sel = cur
        .as_ref()
        .and_then(|s| pens.pens.iter().position(|p| p.name == s.pen))
        .map_or(0, |i| i + 1);
    if widgets::dropdown(ui, th, (id.0, id.1, "pen"), &mut sel, &pen_refs, Some(PICK_W)).changed() {
        let pen = sel.checked_sub(1).and_then(|i| pens.pens.get(i)).map(|p| p.name.as_str());
        *cur = with_pen(cur.take(), pen);
    }
    let names = pats.names();
    let pat_items: Vec<String> = if cur.is_some() {
        names.iter().map(|n| pat_label(ru, n)).collect()
    } else {
        vec!["-".to_string()]
    };
    let pat_refs: Vec<&str> = pat_items.iter().map(String::as_str).collect();
    let mut sel = cur
        .as_ref()
        .and_then(|s| names.iter().position(|n| *n == s.pattern))
        .unwrap_or(0);
    let r = ui
        .add_enabled_ui(cur.is_some(), |ui| {
            widgets::dropdown(ui, th, (id.0, id.1, "pattern"), &mut sel, &pat_refs, Some(PICK_W))
        })
        .inner;
    if r.changed() {
        if let Some(name) = names.get(sel) {
            *cur = with_pattern(cur.take(), name);
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    st: &mut Styles,
    pens: &PenTable,
    pats: &PatternLib,
    level: i64,
    ru: bool,
) {
    let th = ddc_ui::theme::of(ui.ctx());
    ui.label(t(ru, "draw_quality.styles.hint"));
    egui::Grid::new("draw_quality.styles").striped(true).show(ui, |ui| {
        ui.label(t(ru, "draw_quality.styles.category"));
        ui.label(t(ru, "draw_quality.styles.cut"));
        ui.label("");
        ui.label(t(ru, "draw_quality.styles.projection"));
        ui.label("");
        ui.label(t(ru, "draw_quality.styles.halftone"));
        ui.end_row();
        for key in CATS {
            ui.label(cat_label(ru, key));
            let mut ov = st.draft.get(key).cloned().unwrap_or_default();
            style_cells(ui, &th, ru, (key, "cut"), &mut ov.cut, pens, pats);
            style_cells(ui, &th, ru, (key, "proj"), &mut ov.projection, pens, pats);
            halftone_box(ui, &th, ru, key, &mut ov.halftone);
            ui.end_row();
            if ov.is_empty() {
                st.draft.remove(key);
            } else {
                st.draft.insert(key.to_string(), ov);
            }
        }
    });
    let same = !st.changed();
    let why = t(ru, "draw_quality.no_change");
    let blocked = same.then_some(why.as_str());
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        if kit::primary_action(ui, t(ru, "draw_quality.apply"), blocked) {
            st.apply(ed, level);
        }
        let revert = ui
            .add_enabled_ui(!same, |ui| {
                widgets::Button::quiet(&t(ru, "draw_quality.revert")).show(ui, &th).clicked()
            })
            .inner;
        if revert {
            st.revert();
        }
        if kit::danger_button(ui, t(ru, "draw_quality.styles.clear"), !st.draft.is_empty()).clicked() {
            st.draft.clear();
        }
    });
}

/// Halftone switch of one category. The row shows no text; the accessible name and the tooltip
/// say which category it belongs to.
fn halftone_box(ui: &mut egui::Ui, th: &Theme, ru: bool, key: &str, on: &mut bool) {
    let name = crate::tr_lang(ru, "draw_quality.halftone", &[("category", cat_label(ru, key).as_str())]);
    let enabled = ui.is_enabled();
    let r = widgets::checkbox(ui, th, on, "");
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *on, &name));
    r.on_hover_text(name.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_and_pattern_edits_compose() {
        let s = with_pen(None, Some("bold"));
        assert_eq!(s, Some(LineStyle::new("bold", "solid")));
        let s = with_pattern(s, "dash");
        assert_eq!(s, Some(LineStyle::new("bold", "dash")));
        let s = with_pen(s, Some("hair"));
        assert_eq!(s, Some(LineStyle::new("hair", "dash")), "pattern kept");
        assert_eq!(with_pattern(None, "dash"), None, "no pen, nothing to pattern");
        assert_eq!(with_pen(s, None), None);
    }

    #[test]
    fn all_categories_exist_in_the_model() {
        for c in CATS {
            assert!(
                ddc_model::GFX_CATEGORIES.iter().any(|(k, _)| *k == c),
                "{c}"
            );
        }
    }

    #[test]
    fn empty_overrides_do_not_count_as_a_change() {
        let mut s = Styles::default();
        s.draft.insert("wall".into(), StyleOverride::default());
        assert!(!s.changed());
        s.draft.insert(
            "wall".into(),
            StyleOverride {
                halftone: true,
                ..Default::default()
            },
        );
        assert!(s.changed());
        assert_eq!(s.cleaned().len(), 1);
    }

    #[test]
    fn draft_apply_is_one_undo_step_and_revert_drops_edits() {
        let mut ed = Editor::new_in_memory().unwrap();
        let v = ViewKey::Plan(1);
        let mut st = Styles::load(&ed, 1);
        assert!(!st.changed());
        let half = StyleOverride {
            halftone: true,
            ..Default::default()
        };
        st.draft.insert("wall".into(), half.clone());
        assert!(st.changed());
        st.revert();
        assert!(!st.changed(), "revert drops the edit");
        st.draft.insert("wall".into(), half.clone());
        st.draft.insert("slab".into(), half);
        assert!(st.apply(&mut ed, 1));
        assert!(!st.changed());
        assert_eq!(ed.model.view_styles(v).unwrap().len(), 2);
        ed.undo();
        assert!(ed.model.view_styles(v).unwrap().is_empty());
        assert!(!ed.can_undo(), "two categories, one step");
    }

    #[test]
    fn level_change_keeps_an_unsaved_draft_on_its_own_level() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = Styles::load(&ed, 1);
        st.follow_level(&ed, 2);
        assert_eq!(st.level, Some(2), "a clean draft follows the level");
        let half = StyleOverride {
            halftone: true,
            ..Default::default()
        };
        st.draft.insert("wall".into(), half);
        st.follow_level(&ed, 1);
        assert!(st.changed(), "the edit survives the level change");
        assert_eq!(st.level_or(1), 2, "and stays bound to its level");
        assert!(st.apply(&mut ed, st.level_or(1)));
        assert_eq!(ed.model.view_styles(ViewKey::Plan(2)).unwrap().len(), 1);
        assert!(ed.model.view_styles(ViewKey::Plan(1)).unwrap().is_empty());
        st.follow_level(&ed, 1);
        assert_eq!(st.level, Some(1), "once clean it follows again");
    }

    #[test]
    fn every_category_has_a_halftone_name_in_both_languages() {
        for c in CATS {
            let cat = |ru| crate::tr_lang(ru, &format!("draw_quality.cat.{c}"), &[]);
            for ru in [false, true] {
                let name = crate::tr_lang(ru, "draw_quality.halftone", &[("category", cat(ru).as_str())]);
                assert!(name.contains(&cat(ru)), "{c}: {name}");
            }
        }
    }

    #[test]
    fn applying_is_one_undo_step() {
        let mut m = ddc_model::Model::open(":memory:").unwrap();
        let v = ViewKey::Plan(1);
        let mut map = StyleMap::new();
        for c in ["wall", "slab", "col"] {
            map.insert(
                c.into(),
                StyleOverride {
                    halftone: true,
                    ..Default::default()
                },
            );
        }
        m.set_view_styles(v, &map).unwrap();
        assert_eq!(m.view_styles(v).unwrap().len(), 3);
        m.undo().unwrap();
        assert!(m.view_styles(v).unwrap().is_empty(), "three categories, one step");
        map.insert("nonsense".into(), StyleOverride::default());
        assert!(m.set_view_styles(v, &map).is_err());
    }
}
