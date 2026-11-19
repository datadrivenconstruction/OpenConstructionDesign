//! Section and elevation canvas: the bottom HUD (shared with the plan view) and the designed
//! empty cards. Decisions are pure; painting reuses `plan_hud`.
use crate::plan_hud::{self, Hud, HudIn};
use crate::ui_kit::Tokens;
use crate::units::{display_length, Units};
use ddc_model::{PlanView as Drawing, ViewKind, ViewPrim};
use egui::{Painter, Rect};

/// Which designed empty card the section canvas shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionEmpty {
    /// The view is gone from the project.
    NotFound,
    /// A section whose cut line meets nothing.
    CutsNothing,
    /// An elevation with nothing inside its depth.
    ShowsNothing,
}

impl SectionEmpty {
    pub const ALL: [SectionEmpty; 3] =
        [SectionEmpty::NotFound, SectionEmpty::CutsNothing, SectionEmpty::ShowsNothing];

    /// Key stem of the strings: `empty.<what>.{title,hint,action}`.
    pub fn what(self) -> &'static str {
        match self {
            SectionEmpty::NotFound => "sv_notfound",
            SectionEmpty::CutsNothing => "sv_cuts_nothing",
            SectionEmpty::ShowsNothing => "sv_shows_nothing",
        }
    }

    /// What the card's one button does.
    pub fn action(self) -> SvAct {
        match self {
            SectionEmpty::NotFound => SvAct::Close,
            _ => SvAct::ShowOnPlan,
        }
    }
}

/// Request of the canvas to the shell (the canvas does not own the tab list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvAct {
    ShowOnPlan,
    Close,
}

/// The card for a view: `None` drawing is a lost view; a drawing without any cut or visible
/// edge (levels and grids alone do not count) is a blank sheet.
pub fn section_empty_kind(drawing: Option<&Drawing>, kind: ViewKind) -> Option<SectionEmpty> {
    let Some(d) = drawing else {
        return Some(SectionEmpty::NotFound);
    };
    let solid = |p: &ViewPrim| matches!(p, ViewPrim::SectionCut { .. } | ViewPrim::Edge { .. });
    if d.prims.iter().any(solid) {
        return None;
    }
    Some(match kind {
        ViewKind::Section => SectionEmpty::CutsNothing,
        ViewKind::Elevation => SectionEmpty::ShowsNothing,
    })
}

/// Texts and boxes of the HUD for one frame.
pub struct Parts {
    pub hud: Hud,
    pub name: String,
    pub scale: String,
    pub fit: String,
    pub bar_px: f32,
    pub bar_text: String,
}

pub fn parts(
    painter: &Painter,
    rect: Rect,
    name: &str,
    px_per_m: f64,
    u: Units,
    t: &Tokens,
) -> Parts {
    let bar_len = crate::plan_view::scale_bar_len(px_per_m, u);
    let bar_px = (bar_len * px_per_m) as f32;
    let bar_text = display_length(bar_len, u);
    let scale = plan_hud::scale_text(px_per_m);
    let fit = crate::tr("plan.hud.fit", &[]);
    let hud = plan_hud::layout(
        rect,
        HudIn {
            name_w: plan_hud::text_w(painter, name, t.text_dim),
            scale_w: plan_hud::text_w(painter, &scale, t.text_dim),
            fit_w: plan_hud::text_w(painter, &fit, t.text_dim),
            bar_px,
            bar_label_w: plan_hud::text_w(painter, &bar_text, t.text),
        },
    );
    Parts { hud, name: name.to_string(), scale, fit, bar_px, bar_text }
}

pub fn paint(painter: &Painter, p: &Parts, fit_hot: bool, t: &Tokens) {
    plan_hud::paint_chips(painter, &p.hud, &p.name, &p.scale, &p.fit, fit_hot, t);
    plan_hud::paint_bar(painter, p.hud.bar, p.bar_px.min(p.hud.bar.width()), &p.bar_text, t);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::LineClass;

    fn drawing(prims: Vec<ViewPrim>) -> Drawing {
        Drawing { prims, bbox: [0.0; 4] }
    }

    fn level() -> ViewPrim {
        ViewPrim::Level { elevation: 0.0, a: (0.0, 0.0), b: (1.0, 0.0), name: String::new() }
    }

    fn edge() -> ViewPrim {
        let class = LineClass::Projection;
        ViewPrim::Edge { table: "wall", element: 1, class, a: (0.0, 0.0), b: (1.0, 0.0) }
    }

    #[test]
    fn a_lost_view_is_not_found_whatever_its_kind() {
        for k in [ViewKind::Section, ViewKind::Elevation] {
            assert_eq!(section_empty_kind(None, k), Some(SectionEmpty::NotFound));
        }
    }

    #[test]
    fn levels_alone_leave_the_sheet_blank() {
        let d = drawing(vec![level()]);
        assert_eq!(
            section_empty_kind(Some(&d), ViewKind::Section),
            Some(SectionEmpty::CutsNothing)
        );
        assert_eq!(
            section_empty_kind(Some(&d), ViewKind::Elevation),
            Some(SectionEmpty::ShowsNothing)
        );
        let empty = drawing(vec![]);
        assert!(section_empty_kind(Some(&empty), ViewKind::Section).is_some());
    }

    #[test]
    fn any_cut_or_edge_means_content() {
        let d = drawing(vec![level(), edge()]);
        for k in [ViewKind::Section, ViewKind::Elevation] {
            assert_eq!(section_empty_kind(Some(&d), k), None);
        }
        let cut = ViewPrim::SectionCut {
            table: "wall",
            element: 1,
            material: String::new(),
            rings: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
            holes: vec![],
        };
        assert_eq!(section_empty_kind(Some(&drawing(vec![cut])), ViewKind::Section), None);
    }

    #[test]
    fn every_card_has_strings_in_both_languages() {
        use ddc_ui::i18n::Lang;
        for k in SectionEmpty::ALL {
            for part in ["title", "hint", "action"] {
                let key = format!("empty.{}.{part}", k.what());
                let en = crate::tr_in(Lang::En, &key, &[]);
                let ru = crate::tr_in(Lang::Ru, &key, &[]);
                assert!(!en.is_empty() && en != key, "{key} en");
                assert!(!ru.is_empty() && ru != key && ru != en, "{key} ru");
            }
        }
    }

    #[test]
    fn the_card_action_matches_the_state() {
        assert_eq!(SectionEmpty::NotFound.action(), SvAct::Close);
        assert_eq!(SectionEmpty::CutsNothing.action(), SvAct::ShowOnPlan);
        assert_eq!(SectionEmpty::ShowsNothing.action(), SvAct::ShowOnPlan);
    }
}
