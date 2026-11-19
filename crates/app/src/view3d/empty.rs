//! Empty-state card of the 3D view: an empty model, everything hidden in this view, or a section
//! box that cuts the whole model away. One centred card with a single way out instead of a blank
//! grey canvas (`ui_kit::empty_state_action`, keys `empty.<what>.*`).

use super::{Nav, TempCats, CATS};
use crate::viewport::Viewport;
use ddc_render::{Aabb, Scene};
use ddc_ui::sem::{layout, space};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The model has no 3D geometry at all.
    Model,
    /// Geometry exists but temp hide / isolate / the view's hidden set leave nothing.
    Hidden,
    /// Only the view's category rules (view graphics) leave nothing; the card opens them.
    Graphics,
    /// The section box does not touch the model.
    Box,
    /// The graphics adapter is unusable; the card restarts with safer graphics.
    Gpu,
    /// The model is still opening or the scene is being built; no action, just progress.
    Loading,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Model,
        Kind::Hidden,
        Kind::Graphics,
        Kind::Box,
        Kind::Gpu,
        Kind::Loading,
    ];

    /// Whether the card offers a one-click way out.
    pub fn has_action(self) -> bool {
        self != Kind::Loading
    }

    /// Key stem: `empty.<what>.{title,hint,action}`.
    pub fn what(self) -> &'static str {
        match self {
            Kind::Model => "v3d_model",
            Kind::Hidden => "v3d_hidden",
            Kind::Graphics => "v3d_graphics",
            Kind::Box => "v3d_box",
            Kind::Gpu => "v3d_gpu",
            Kind::Loading => "v3d_loading",
        }
    }
}

/// Graphics trouble beats everything; a still-loading model never reads as an empty one.
/// `loading` is `Some(percent)` while a job runs (`Some(None)` without a known amount).
pub fn lead_kind(usable: bool, loading: Option<Option<u8>>, k: Option<Kind>) -> Option<Kind> {
    if !usable {
        return Some(Kind::Gpu);
    }
    match (k, loading) {
        (Some(Kind::Model), Some(_)) => Some(Kind::Loading),
        (k, _) => k,
    }
}

/// "Show all" may only drop temporary hiding; when nothing temporary is active the cause is
/// the view's own graphics, which the card opens instead of silently resetting.
pub fn hidden_cause(kind: Option<Kind>, temporary: bool) -> Option<Kind> {
    match kind {
        Some(Kind::Hidden) if !temporary => Some(Kind::Graphics),
        k => k,
    }
}

/// Which card the view shows. `bounds` of the final scene, `view_hidden` when the open view
/// hides elements, `visible` meshes left after category rules, `box_hits` meshes touched by
/// the section box (`None` without a box).
pub fn empty_kind(
    bounds: Option<&Aabb>,
    view_hidden: bool,
    visible: usize,
    box_hits: Option<usize>,
) -> Option<Kind> {
    if bounds.is_none() {
        return Some(if view_hidden { Kind::Hidden } else { Kind::Model });
    }
    if visible == 0 {
        return Some(Kind::Hidden);
    }
    (box_hits == Some(0)).then_some(Kind::Box)
}

/// Meshes (then instance groups) whose category is visible; `cats[i]` is the 1-based `CATS`
/// number, none or 0 means no rule (always visible).
pub fn count_visible(cats: &[u8], total: usize, vis: impl Fn(&str) -> bool) -> usize {
    (0..total)
        .filter(|i| match cats.get(*i).copied().unwrap_or(0) {
            0 => true,
            c => CATS.get(c as usize - 1).is_none_or(|n| vis(n)),
        })
        .count()
}

/// 1 when the section box touches the scene bounds, 0 when it is clear of them, `None` without
/// a box. Bounds are a cheap stand-in for a per-mesh test.
pub fn box_hits(bounds: Option<&Aabb>, section: Option<&Aabb>) -> Option<usize> {
    let b = section?;
    Some(bounds.is_some_and(|s| {
        s.min.x <= b.max.x
            && s.max.x >= b.min.x
            && s.min.y <= b.max.y
            && s.max.y >= b.min.y
            && s.min.z <= b.max.z
            && s.max.z >= b.min.z
    }) as usize)
}

fn filtered(nav: &Nav) -> bool {
    nav.temp.is_some()
        || !nav.gfx_hidden.is_empty()
        || nav.rules.values().any(|r| !r.visible)
}

fn kind_of(nav: &Nav, scene: &Scene, section: Option<&Aabb>) -> Option<Kind> {
    let total = scene.meshes.len() + scene.instanced.len();
    let visible = if scene.bounds.is_some() && total == 0 {
        1
    } else if filtered(nav) {
        count_visible(&scene.cats, total, |c| nav.cat_visible(c))
    } else {
        total
    };
    let k = empty_kind(
        scene.bounds.as_ref(),
        !nav.view_hidden.is_empty(),
        visible,
        box_hits(scene.bounds.as_ref(), section),
    );
    hidden_cause(k, nav.temp.is_some() || !nav.view_hidden.is_empty())
}

/// Pending click on the card, executed in `sync` where the app is available.
pub type Act = Kind;

/// Draws the card; returns its rect so clicks on it stay out of model selection.
pub fn show(v: &mut Viewport, ui: &mut egui::Ui, rect: egui::Rect, scene: &Scene) -> Option<egui::Rect> {
    let kind = lead_kind(
        v.gfx.health.usable,
        v.nav.loading,
        kind_of(&v.nav, scene, v.section_box.as_ref()),
    )?;
    let ctx = ui.ctx().clone();
    let ru = v.nav.labels_lang == Some(ddc_ui::i18n::Lang::Ru);
    let pct = v.nav.loading.flatten();
    let mut clicked = None;
    let r = egui::Area::new(ui.id().with("v3d_empty"))
        .order(ddc_ui::z::Z::Canvas.order())
        .pivot(egui::Align2::CENTER_CENTER)
        .fixed_pos(rect.center())
        .show(&ctx, |ui| {
            super::overlay::frame(v.dark)
                .inner_margin(egui::Margin::same(space::LG as i8))
                .show(ui, |ui| {
                    ui.set_width(layout::EMPTY_W);
                    clicked = card_ui(ui, kind, ru, pct);
                })
        });
    if clicked.is_some() {
        v.nav.empty_act = clicked;
    }
    Some(r.response.rect)
}

/// The card body in an explicit language. Returns the action that was clicked: the card's own,
/// or `Kind::Model` ("Open plan") from the second button of the graphics card.
pub fn card_ui(ui: &mut egui::Ui, kind: Kind, ru: bool, pct: Option<u8>) -> Option<Act> {
    let th = ddc_ui::theme::of(ui.ctx());
    if kind == Kind::Loading {
        let title = match pct {
            Some(p) => crate::tr_lang(ru, "empty.v3d_loading.pct", &[("pct", &p.to_string())]),
            None => crate::tr_lang(ru, "empty.v3d_loading.title", &[]),
        };
        ddc_ui::widgets::loading_state(ui, &th, &title);
        return None;
    }
    let mut out = crate::ui_kit::empty_state_lang(ui, ru, kind.what(), true).then_some(kind);
    if kind == Kind::Gpu {
        ui.vertical_centered(|ui| {
            let label = crate::tr_lang(ru, "empty.v3d_model.action", &[]);
            if ddc_ui::widgets::Button::secondary(&label).show(ui, &th).clicked() {
                out = Some(Kind::Model);
            }
        });
    }
    out
}

/// Drops only the temporary hide/isolate and the open view's in-memory hidden set; category
/// rules and view graphics stay as the user set them.
pub fn reveal_temporary(nav: &mut Nav) {
    nav.temp = None::<TempCats>;
    nav.set_view_hidden(Default::default());
}

/// The card's one action.
pub fn apply(app: &mut crate::shell::EditorApp, act: Act) {
    match act {
        Kind::Model => app.view3d = false,
        Kind::Hidden => reveal_temporary(&mut app.view.nav),
        Kind::Graphics => app.view.nav.vg_open = true,
        Kind::Box => super::h_box_fit(app),
        Kind::Gpu => crate::gfx_ui::note_clicked(app, crate::gfx_ui::NoteAct::SaferStart),
        Kind::Loading => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view3d::CatRule;
    use ddc_render::glam::Vec3;
    use ddc_ui::i18n::Lang;

    fn bb(min: f32, max: f32) -> Aabb {
        Aabb {
            min: Vec3::splat(min),
            max: Vec3::splat(max),
        }
    }

    #[test]
    fn empty_model_without_bounds() {
        assert_eq!(empty_kind(None, false, 0, None), Some(Kind::Model));
    }

    #[test]
    fn view_hidden_set_with_no_bounds_is_hidden_not_empty() {
        assert_eq!(empty_kind(None, true, 0, None), Some(Kind::Hidden));
    }

    #[test]
    fn no_visible_category_is_hidden() {
        let b = bb(0.0, 1.0);
        assert_eq!(empty_kind(Some(&b), false, 0, None), Some(Kind::Hidden));
        assert_eq!(empty_kind(Some(&b), false, 0, Some(0)), Some(Kind::Hidden));
    }

    #[test]
    fn box_outside_model() {
        let b = bb(0.0, 1.0);
        assert_eq!(empty_kind(Some(&b), false, 3, Some(0)), Some(Kind::Box));
    }

    #[test]
    fn normal_model_has_no_card() {
        let b = bb(0.0, 1.0);
        assert_eq!(empty_kind(Some(&b), false, 3, None), None);
        assert_eq!(empty_kind(Some(&b), false, 3, Some(1)), None);
        assert_eq!(empty_kind(Some(&b), true, 3, None), None);
    }

    #[test]
    fn box_hits_by_bounds() {
        let m = bb(0.0, 10.0);
        assert_eq!(box_hits(Some(&m), None), None);
        assert_eq!(box_hits(Some(&m), Some(&bb(5.0, 20.0))), Some(1));
        assert_eq!(box_hits(Some(&m), Some(&bb(11.0, 20.0))), Some(0));
        assert_eq!(box_hits(None, Some(&bb(0.0, 1.0))), Some(0));
    }

    #[test]
    fn count_visible_follows_category_rules() {
        // wall, door, no rule, wall
        let cats = [1u8, 2, 0, 1];
        let all = |_: &str| true;
        let no_walls = |c: &str| c != "wall";
        let none = |_: &str| false;
        assert_eq!(count_visible(&cats, 4, all), 4);
        assert_eq!(count_visible(&cats, 4, no_walls), 2);
        // a mesh without a rule stays visible even when every category is off
        assert_eq!(count_visible(&cats, 4, none), 1);
        // missing cats entries count as no rule
        assert_eq!(count_visible(&[], 3, none), 3);
        assert_eq!(count_visible(&cats, 0, all), 0);
    }

    #[test]
    fn temp_isolate_of_absent_category_hides_everything() {
        let mut nav = Nav::default();
        nav.temp = Some(TempCats::Isolate(["mep".to_string()].into()));
        assert!(filtered(&nav));
        let cats = [1u8, 2, 5];
        assert_eq!(count_visible(&cats, 3, |c| nav.cat_visible(c)), 0);
    }

    #[test]
    fn show_all_clears_only_temporary_hiding() {
        let mut nav = Nav::default();
        assert!(!filtered(&nav));
        nav.temp = Some(TempCats::Hide(["wall".to_string()].into()));
        nav.gfx_hidden = vec!["door".to_string()];
        nav.rules.insert(
            "floor".into(),
            CatRule {
                visible: false,
                transparency: 0,
            },
        );
        reveal_temporary(&mut nav);
        assert!(nav.temp.is_none() && nav.view_hidden.is_empty());
        assert_eq!(nav.gfx_hidden, vec!["door".to_string()]);
        assert!(!nav.rules["floor"].visible);
        assert!(nav.cat_visible("wall") && !nav.cat_visible("door"));
    }

    #[test]
    fn hidden_without_temporary_cause_points_to_graphics() {
        let h = Some(Kind::Hidden);
        assert_eq!(hidden_cause(h, true), h);
        assert_eq!(hidden_cause(h, false), Some(Kind::Graphics));
        assert_eq!(hidden_cause(Some(Kind::Box), false), Some(Kind::Box));
        assert_eq!(hidden_cause(None, false), None);
    }

    #[test]
    fn unusable_graphics_lead_over_everything() {
        for k in [None, Some(Kind::Model), Some(Kind::Hidden), Some(Kind::Box)] {
            assert_eq!(lead_kind(false, None, k), Some(Kind::Gpu));
            assert_eq!(lead_kind(false, Some(None), k), Some(Kind::Gpu));
        }
    }

    #[test]
    fn loading_model_is_never_an_empty_one() {
        let m = Some(Kind::Model);
        assert_eq!(lead_kind(true, Some(Some(40)), m), Some(Kind::Loading));
        assert_eq!(lead_kind(true, Some(None), m), Some(Kind::Loading));
        assert_eq!(lead_kind(true, None, m), m);
        // a finished scene or a filter card is not replaced by the spinner
        assert_eq!(lead_kind(true, Some(None), None), None);
        let h = Some(Kind::Hidden);
        assert_eq!(lead_kind(true, Some(None), h), h);
    }

    fn card_probe(kind: Kind, ru: bool) -> crate::layout_contract::Probe {
        crate::layout_contract::probe("v3d.empty", |ctx| {
            egui::Area::new(egui::Id::new(("ddc.dialog", "v3d.empty")))
                .show(ctx, |ui| {
                    ui.set_width(layout::EMPTY_W);
                    card_ui(ui, kind, ru, Some(42));
                });
        })
    }

    #[test]
    fn every_card_fits_the_card_width_in_both_languages() {
        for k in Kind::ALL {
            for ru in [false, true] {
                let p = card_probe(k, ru);
                let r = p.inner.expect("card drawn");
                assert!(r.width() <= layout::EMPTY_W + 1.0, "{k:?} ru={ru} {r:?}");
                if k.has_action() {
                    assert!(!p.controls.is_empty(), "{k:?} ru={ru} has an action");
                } else {
                    assert!(p.controls.is_empty(), "{k:?} ru={ru} has no action");
                }
                if k == Kind::Gpu {
                    assert_eq!(p.controls.len(), 2, "safe graphics and open plan");
                }
            }
        }
    }

    #[test]
    fn loading_card_shows_percent_in_both_languages() {
        for (lang, ru) in [(Lang::En, false), (Lang::Ru, true)] {
            let p = card_probe(Kind::Loading, ru);
            let want = crate::tr_in(lang, "empty.v3d_loading.pct", &[("pct", "42")]);
            assert!(p.text.iter().any(|t| *t == want), "{lang:?} {:?}", p.text);
        }
        assert_ne!(
            crate::tr_in(Lang::En, "empty.v3d_loading.title", &[]),
            crate::tr_in(Lang::Ru, "empty.v3d_loading.title", &[])
        );
    }

    #[test]
    fn every_card_has_strings_in_both_languages() {
        for k in Kind::ALL.into_iter().filter(|k| k.has_action()) {
            for part in ["title", "hint", "action"] {
                let key = format!("empty.{}.{part}", k.what());
                let en = crate::tr_in(Lang::En, &key, &[]);
                let ru = crate::tr_in(Lang::Ru, &key, &[]);
                assert!(!en.is_empty() && en != key, "{key} en");
                assert!(!ru.is_empty() && ru != key, "{key} ru");
                assert_ne!(en, ru, "{key}");
            }
        }
    }
}
