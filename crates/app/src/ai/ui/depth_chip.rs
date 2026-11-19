//! The depth chip above the composer: "Depth D3 ▾ · AR ST MEP Doc". A click opens the project
//! setting (depth, discipline switches), the pre-run estimate and the heat map switch. The chip
//! only reads the model; a change goes out as an action, applied as one undo step.

use super::kit::{space, Button};
use super::{Action, Cx, PanelUi};
use crate::depth_ui;
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
    let text = label_for(ui, cx, &s, ui.available_width());
    let tip = cat.t("depth.chip_tip");
    let resp = Button::quiet(&text).small().tooltip(&tip, "").show(ui, th);
    egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(POPUP_W.min(ui.ctx().screen_rect().width() - 2.0 * space::MD));
            popup(ui, cx, s, acts);
        });
    ui.add_space(space::XS);
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
