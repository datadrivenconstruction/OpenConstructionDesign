// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The depth chip above the composer: "Depth D3 ▾ · AR ST MEP Doc". A click opens the project
//! setting (depth, discipline switches), the pre-run estimate and the heat map switch. The chip
//! only reads the model; a change goes out as an action, applied as one undo step.

use super::kit::{space, Button};
use super::{Action, Cx, PanelUi};
use crate::depth_ui;
use ddc_ai::limits::Reach;
use ddc_model::DepthSettings;
use ddc_ui::theme::Role;
use ddc_ui::widgets;
use egui::{self, RichText, Ui};

/// Widest popup body, in panel pixels.
const POPUP_W: f32 = 340.0;

/// The project setting at this model state, read once per `Editor::cache_key`.
pub(super) fn settings(st: &mut PanelUi, cx: &Cx<'_>) -> DepthSettings {
    let key = cx.ed.cache_key();
    if let Some((k, s)) = st.depth {
        if k == key {
            return s;
        }
    }
    let s = cx.ed.model.depth_settings().unwrap_or_default();
    st.depth = Some((key, s));
    s
}

/// The label that fits `avail`: the full one, else the depth alone.
pub fn label_for(ui: &Ui, cx: &Cx<'_>, s: &DepthSettings, avail: f32) -> String {
    let full = format!("{} \u{25be}", depth_ui::chip_text(cx.cat, s));
    if Button::quiet(&full).small().width(ui, cx.th) <= avail {
        return full;
    }
    format!("{} \u{25be}", depth_ui::level_label(s.project))
}

pub(super) fn show(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let s = settings(st, cx);
    // the row is as tall as the chips, not as a full-size control
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal(|ui| {
            let text = label_for(ui, cx, &s, ui.available_width() - REACH_W);
            let tip = cat.t("depth.chip_tip");
            let resp = Button::quiet(&text).small().tooltip(&tip, "").show(ui, th);
            egui::Popup::menu(&resp)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.set_max_width(POPUP_W.min(ui.ctx().screen_rect().width() - 2.0 * space::MD));
                    popup(ui, cx, s, acts);
                });
            reach_chip(ui, cx, acts);
        });
    });
    ui.add_space(space::XS);
}

/// Room kept for the mode chip beside the depth chip.
const REACH_W: f32 = 110.0;

/// The words of a mode, in the catalog.
pub fn reach_label(cat: &ddc_ui::i18n::Catalog, r: Reach) -> String {
    cat.t(match r {
        Reach::Normal => "ai.reach.normal",
        Reach::Large => "ai.reach.large",
        Reach::Unlimited => "ai.reach.unlimited",
    })
}

/// "Normal / Large / Unlimited": how far one turn may go, as the settings row shows it too.
/// A click on a mode sets it for this project; the money cap and mass-delete confirmation stay.
pub fn reach_picker(ui: &mut Ui, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let cur = super::connect::reach_of(&cx.ed.model);
    let labels: Vec<String> = Reach::ALL.iter().map(|r| reach_label(cx.cat, *r)).collect();
    let items: Vec<&str> = labels.iter().map(String::as_str).collect();
    let sel = Reach::ALL.iter().position(|r| *r == cur).unwrap_or(0);
    if let Some(i) = widgets::segmented(ui, cx.th, &items, sel) {
        acts.push(Action::SetReach(Reach::ALL[i]));
    }
    let hint = match cur {
        Reach::Normal => "ai.reach.normal_hint",
        Reach::Large => "ai.reach.large_hint",
        Reach::Unlimited => "ai.reach.unlimited_hint",
    };
    super::kit::caption(ui, cx.th, &cx.cat.t(hint));
}

fn reach_chip(ui: &mut Ui, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let cur = super::connect::reach_of(&cx.ed.model);
    let text = format!("{} \u{25be}", reach_label(cat, cur));
    let resp = Button::quiet(&text)
        .small()
        .tooltip(&cat.t("ai.reach.chip_tip"), "")
        .show(ui, th);
    egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(POPUP_W.min(ui.ctx().screen_rect().width() - 2.0 * space::MD));
            reach_picker(ui, cx, acts);
        });
}

/// The popup body; also drawn alone by the layout tests.
pub fn popup(ui: &mut Ui, cx: &Cx<'_>, s: DepthSettings, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let mut d = s;
    if depth_ui::editor(ui, th, cat, &mut d) {
        acts.push(Action::SetDepth(d));
    }
    ui.add_space(space::SM);
    let line = match depth_ui::model_estimate(&cx.ed.model, &d) {
        Some(e) => depth_ui::estimate_text(cat, &e),
        None => cat.t("depth.estimate_none"),
    };
    ui.add(
        egui::Label::new(
            RichText::new(line)
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        )
        .wrap(),
    );
    ui.add_space(space::SM);
    let mut heat = depth_ui::heat_on();
    if widgets::toggle(ui, th, &mut heat, &cat.t("depth.heat")).changed() {
        acts.push(Action::DepthHeat(heat));
    }
}
