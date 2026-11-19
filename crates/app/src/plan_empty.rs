// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Empty-state card of the plan view: why a level shows nothing and the one step that fixes it.
//! The whole-model-empty case keeps the start hint; this card covers the rest (keys
//! `empty.plan_<what>.*`, same card widget as the 3D view).

use ddc_ui::sem::{layout, space};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Temporary hide or isolate leaves nothing.
    Hidden,
    /// The view's category rules or permanently hidden elements leave nothing.
    Graphics,
    /// The phase filter leaves nothing.
    Phase,
    /// This level has no elements while another level has.
    Level,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Hidden, Kind::Graphics, Kind::Phase, Kind::Level];

    /// Key stem: `empty.<what>.{title,hint,action}`.
    pub fn what(self) -> &'static str {
        match self {
            Kind::Hidden => "plan_hidden",
            Kind::Graphics => "plan_graphics",
            Kind::Phase => "plan_phase",
            Kind::Level => "plan_level",
        }
    }
}

/// What hides elements of the open plan view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hides {
    pub temporary: bool,
    pub phase: bool,
    pub graphics: bool,
}

/// Elements of the view level the plan draws: walls, bodies, slabs, rooms, curtain walls,
/// openings, forms, structure and the tool layer (MEP, stairs, roofs, components, room
/// separators). The card
/// must never sit over drawn content; datum (grids, reference planes) does not count.
pub fn drawn(ed: &crate::editor::Editor) -> usize {
    let l = &ed.tool.layer;
    ed.walls.len()
        + ed.bodies.len()
        + ed.slabs.len()
        + ed.rooms.len()
        + ed.curtains.len()
        + ed.curtain_lines.len()
        + ed.openings.len()
        + ed.mass_plan.len()
        + ed.struct_fp.items.len()
        + l.polys.len()
        + l.comps.len()
        + l.stairs.len()
        + l.seps.len()
}

/// `total` plan elements on the level before hiding, `visible` after it; `other_level` is a
/// level that holds elements. `None`: something is shown, or the whole model is empty (the
/// start hint covers that).
pub fn plan_empty_kind(
    total: usize,
    visible: usize,
    hides: Hides,
    other_level: Option<i64>,
) -> Option<Kind> {
    if visible > 0 {
        return None;
    }
    if total == 0 {
        return other_level.map(|_| Kind::Level);
    }
    Some(if hides.temporary {
        Kind::Hidden
    } else if hides.phase {
        Kind::Phase
    } else {
        Kind::Graphics
    })
}

/// The card in an explicit language; true when its action was clicked.
pub fn card_ui(ui: &mut egui::Ui, kind: Kind, ru: bool) -> bool {
    crate::ui_kit::empty_state_lang(ui, ru, kind.what(), true)
}

/// Draws the card centred in `rect`; returns whether its action was clicked.
pub fn show(ui: &egui::Ui, rect: egui::Rect, kind: Kind) -> bool {
    let ctx = ui.ctx().clone();
    let ru = crate::ui_lang_now();
    let mut clicked = false;
    egui::Area::new(ui.id().with("plan_empty"))
        .order(ddc_ui::z::Z::Canvas.order())
        .pivot(egui::Align2::CENTER_CENTER)
        .fixed_pos(rect.center())
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(space::LG as i8))
                .show(ui, |ui| {
                    ui.set_width(layout::EMPTY_W);
                    clicked = card_ui(ui, kind, ru);
                })
        });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::Lang;

    const OFF: Hides = Hides { temporary: false, phase: false, graphics: false };

    #[test]
    fn something_visible_has_no_card() {
        let all = Hides { temporary: true, phase: true, graphics: true };
        assert_eq!(plan_empty_kind(5, 1, all, Some(2)), None);
    }

    #[test]
    fn empty_level_points_to_another_level_only_when_one_has_elements() {
        assert_eq!(plan_empty_kind(0, 0, OFF, Some(2)), Some(Kind::Level));
        assert_eq!(plan_empty_kind(0, 0, OFF, None), None);
    }

    #[test]
    fn hidden_by_filters_names_the_cause() {
        let t = Hides { temporary: true, ..OFF };
        let p = Hides { phase: true, ..OFF };
        let g = Hides { graphics: true, ..OFF };
        assert_eq!(plan_empty_kind(4, 0, t, None), Some(Kind::Hidden));
        assert_eq!(plan_empty_kind(4, 0, p, None), Some(Kind::Phase));
        assert_eq!(plan_empty_kind(4, 0, g, None), Some(Kind::Graphics));
        // temporary hiding wins over a phase filter that is also on
        assert_eq!(plan_empty_kind(4, 0, Hides { phase: true, ..t }, None), Some(Kind::Hidden));
        // nothing known to hide it: the view's graphics are the place to look
        assert_eq!(plan_empty_kind(4, 0, OFF, None), Some(Kind::Graphics));
    }

    #[test]
    fn level_with_rooms_only_has_no_card() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        assert_eq!(drawn(&ed), 0);
        let ring = vec![
            ddc_geom::v2(0.0, 0.0),
            ddc_geom::v2(4.0, 0.0),
            ddc_geom::v2(4.0, 3.0),
            ddc_geom::v2(0.0, 3.0),
        ];
        ed.rooms.push(crate::editor::RoomItem {
            id: 1,
            name: "Room".into(),
            number: "1".into(),
            rings: vec![ring],
        });
        let n = drawn(&ed);
        assert!(n > 0);
        // another level with elements must not pull the user away from drawn rooms
        assert_eq!(plan_empty_kind(n, n, OFF, Some(2)), None);
    }

    #[test]
    fn level_with_mep_only_has_no_card() {
        use ddc_model::{PipeMaterial, RunSpec, SystemKind};
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let level = ed.level;
        let s = ed.model.add_mep_system("HW", SystemKind::Heating).unwrap();
        let pts = vec![(0.0, 0.0, 1.0), (3.0, 0.0, 1.0)];
        let spec = RunSpec::pipe(s, level, PipeMaterial::Steel, 100, pts).unwrap();
        ed.model.add_mep_run(&spec).unwrap();
        ed.reload().unwrap();
        assert!(ed.walls.is_empty() && ed.rooms.is_empty());
        ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
        let n = drawn(&ed);
        assert!(n > 0, "a pipe is drawn content");
        assert_eq!(plan_empty_kind(n, n, OFF, Some(2)), None);
    }

    #[test]
    fn every_card_fits_the_card_width_with_one_action_in_both_languages() {
        for k in Kind::ALL {
            for ru in [false, true] {
                let p = crate::layout_contract::probe("plan.empty", |ctx| {
                    egui::Area::new(egui::Id::new(("ddc.dialog", "plan.empty"))).show(ctx, |ui| {
                        ui.set_width(layout::EMPTY_W);
                        card_ui(ui, k, ru);
                    });
                });
                let r = p.inner.expect("card drawn");
                assert!(r.width() <= layout::EMPTY_W + 1.0, "{k:?} ru={ru} {r:?}");
                assert_eq!(p.controls.len(), 1, "{k:?} ru={ru}");
            }
        }
    }

    #[test]
    fn every_card_has_strings_in_both_languages() {
        for k in Kind::ALL {
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
