// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Panel frame: header, context strip, the dialog and the composer pinned under it.

use super::budget;
use super::kit::{self, space, Button};
use super::logic::{self, ErrKind, PanelState, StateIn};
use super::stream_status;
use super::{
    checks, compare, connect, consent, plan_card, proposal_card, retry, Action, Cx, Entry, Facts, PanelUi,
};
use ddc_ai::Mode;
use ddc_model::ElKey;
use ddc_ui::icons::IconCache;
use ddc_ui::theme::{icon_size, Role};
use ddc_ui::widgets;
use egui::{self, Frame, Key, KeyboardShortcut, Margin, Modifiers, RichText, TextEdit, Ui};

const COMPOSER_ROWS: f32 = 6.0;
const BUBBLE_SHARE: f32 = 0.85;
const COMPOSER_MIN_ROWS: usize = 2;
/// Widest answer-mode menu, in panel pixels.
const MODE_MENU_W: f32 = 280.0;
/// Most width Send may take beside the attach and reference icons, in control rows.
#[cfg(test)]
const SEND_RESERVE_ROWS: f32 = 4.0;

fn composer_margin() -> Margin {
    Margin::symmetric(space::SM as i8, space::SM as i8)
}

/// Two body lines plus the field padding.
fn input_min_height(th: &ddc_ui::Theme) -> f32 {
    let line = th.font(Role::Body).size * 1.4;
    line * COMPOSER_MIN_ROWS as f32 + 2.0 * space::SM
}

/// The field grows with the text up to this height, then scrolls.
pub(super) fn input_max_height(th: &ddc_ui::Theme) -> f32 {
    th.d().row * COMPOSER_ROWS
}

fn mode_of(st: &PanelUi) -> Mode {
    if st.propose {
        Mode::Propose
    } else {
        Mode::ReadOnly
    }
}

pub fn state_of(st: &PanelUi, f: &Facts) -> PanelState {
    let steps = f
        .plan
        .as_ref()
        .is_some_and(|p| !p.compiled.steps.is_empty());
    let has_profile = st.active_ready();
    logic::panel_state(StateIn {
        has_profile,
        needs_consent: st.consent.is_some(),
        local_blocked: st.local_wait,
        preparing: st.starting || st.prepared.is_some(),
        busy: f.busy,
        tool_running: st.tool_running,
        has_steps: steps,
        can_apply: f.can_apply,
        stale: f.stale,
        blocked: f.plan.as_ref().is_some_and(|p| p.blocked()),
        error: st.error.is_some() || st.limit.is_some(),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn panel(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    header(ui, st, cx, icons, acts, &cx.cat.t("ai.panel.title"), true);
    if !header_parts(!st.profiles.is_empty(), st.active_ready()).strip {
        // not connected: no context to show and no other tab to go to, only the connect card
        st.tab = 0;
        ui.add_space(space::XS);
        dialog(ui, st, cx, icons, f, acts, hot, true, None);
        return;
    }
    super::ctx_chips::strip(ui, st, cx, hot, acts);
    let labels = [
        cx.cat.t("ai.tab.dialog"),
        cx.cat.t("ai.tab.checks"),
        cx.cat.t("ai.tab.options"),
    ];
    let names = [labels[0].as_str(), labels[1].as_str(), labels[2].as_str()];
    kit::tab_bar(ui, cx.th, &names, &mut st.tab);
    ui.add_space(space::XS);
    if st.tab == 2 {
        compare::tab(ui, &mut st.cmp, cx, acts);
    } else if st.tab == 1 {
        checks::tab(ui, st, cx, acts, hot);
    } else {
        dialog(ui, st, cx, icons, f, acts, hot, true, None);
    }
}

/// Title row, connection picker and answer mode. The quick panel has a close button; an AI tab
/// is left through the view bar instead.
pub(super) fn header(
    ui: &mut Ui,
    st: &PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    acts: &mut Vec<Action>,
    title: &str,
    closable: bool,
) {
    let (th, cat) = (cx.th, cx.cat);
    let parts = header_parts(!st.profiles.is_empty(), st.active_ready());
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(title)
                    .font(th.font(Role::Title))
                    .color(th.c("text.primary")),
            )
            .selectable(false)
            .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(ui, icons, th, "settings", false, &cat.t("cmd.ai.settings"))
                .clicked()
            {
                acts.push(Action::OpenSettings);
            }
            if closable {
                let close_tip = format!("{}  ·  Ctrl+J", cat.t("ai.panel.close"));
                if widgets::icon_button(ui, icons, th, "close", false, &close_tip).clicked() {
                    acts.push(Action::ClosePanel);
                }
            }
        });
    });
    if parts.picker {
        // the name the person gave the connection; vendors are never named by the product
        ui.horizontal(|ui| {
            let names: Vec<&str> = st.profiles.iter().map(|p| p.name.as_str()).collect();
            let mut i = st.profile;
            let width = (ui.available_width() * 0.7).max(th.d().control * 3.0);
            if kit::dropdown(ui, th, "ddc.ai.profile", &mut i, &names, width) {
                acts.push(Action::SetProfile(i));
            }
            if let Some(p) = st.profiles.get(st.profile) {
                let (key, tone) = if connect::is_local(p) {
                    ("ai.conn.local", kit::Tone::Success)
                } else {
                    ("ai.conn.cloud", kit::Tone::Accent)
                };
                kit::badge(ui, th, &cat.t(key), tone);
            }
        });
    }
    // the answer mode is chosen where the request is typed: see `mode_chip`
    let _ = parts.mode;
}

/// The answer mode beside the attach icons: the current mode, and a menu that says what each does.
fn mode_chip(ui: &mut Ui, st: &PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let now = mode_of(st);
    let label = format!("{} \u{25be}", cat.t(logic::mode_key(now)));
    let resp = Button::quiet(&label).small().tooltip(&cat.t("ai.mode.pick_tip"), "").show(ui, th);
    egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_max_width(MODE_MENU_W.min(ui.ctx().screen_rect().width() - 2.0 * space::MD));
            for (m, desc) in [(Mode::ReadOnly, "ai.mode.read_only.desc"), (Mode::Propose, "ai.mode.propose.desc")] {
                let on = m == now;
                let r = Frame::new()
                    .fill(if on { th.c("bg.selected") } else { egui::Color32::TRANSPARENT })
                    .corner_radius(th.radius("md"))
                    .inner_margin(Margin::symmetric(space::SM as i8, space::XS as i8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(cat.t(logic::mode_key(m)))
                                .font(th.font(Role::BodyStrong))
                                .color(th.c("text.primary")),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(cat.t(desc))
                                    .font(th.font(Role::Caption))
                                    .color(th.c("text.secondary")),
                            )
                            .wrap(),
                        );
                    })
                    .response
                    .interact(egui::Sense::click());
                if r.hovered() && !on {
                    kit::row_highlight(ui, th, r.rect, false);
                }
                if r.clicked() && !on {
                    acts.push(Action::SetPropose(m == Mode::Propose));
                }
            }
        });
}

/// The conversation: log and cards scrolling, the plan's actions and (with `with_composer`) the
/// composer pinned under them. The build tab leaves the composer out while its brief asks.
#[allow(clippy::too_many_arguments)]
pub(super) fn dialog(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
    with_composer: bool,
    mut above: Option<&mut dyn FnMut(&mut Ui, &mut Vec<Action>)>,
) {
    let state = state_of(st, f);
    shortcuts(ui, st, cx, f, acts);
    st.attach.rect = Some(ui.max_rect());
    let host = st
        .active()
        .map(|p| ddc_ai::profile::url_origin(&p.base_url))
        .unwrap_or_else(|| cx.cat.t("ai.att.host_none"));
    super::attach_ui::preview(ui.ctx(), st, cx, &host, acts);
    // not connected and nothing said yet: no dead composer or status, but a plan keeps its actions
    let connect = shows_connect_cta(state, st.log.is_empty()) || !with_composer;
    let pending = f.plan.as_deref().filter(|p| !p.items.is_empty());
    if !connect || pending.is_some() || above.is_some() {
        egui::TopBottomPanel::bottom("ddc.ai.composer_panel")
            .frame(Frame::new().inner_margin(Margin {
                top: space::XS as i8,
                bottom: space::XS as i8,
                ..Margin::ZERO
            }))
            .show_separator_line(false)
            .resizable(false)
            .show_inside(ui, |ui| {
                if let Some(p) = pending {
                    plan_card::actions(ui, st, cx, p, f, acts);
                    ui.add_space(space::XS);
                }
                // an AI tab's own row (quick requests) sits between the card and the composer
                if let Some(add) = above.as_mut() {
                    add(ui, acts);
                    ui.add_space(space::XS);
                }
                if !connect {
                    if !f.busy && !st.starting {
                        super::attach_ui::chips(ui, st, cx, acts);
                        super::attach_ui::req_card(ui, st, cx, acts);
                        ui.add_space(space::XS);
                    }
                    composer(ui, st, cx, icons, f, acts);
                    status_line(ui, st, cx, state, f, acts);
                }
            });
    }
    egui::CentralPanel::default()
        .frame(Frame::new())
        .show_inside(ui, |ui| {
            // a draft is read from its headline down: only the conversation follows its end
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(pending.is_none())
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    body(ui, st, cx, icons, state, f, acts, hot);
                });
        });
}

fn shortcuts(ui: &Ui, st: &mut PanelUi, cx: &Cx<'_>, f: &Facts, acts: &mut Vec<Action>) {
    if cx.palette_open || st.settings_open {
        return;
    }
    // the keys follow the person into the plan and 3D view, where the preview is looked at; a
    // text field keeps them (Ctrl+Enter in an annotation commits that text, not the plan)
    let at = key_at(ui, st, cx);
    let (apply, stop) = logic::card_keys(at);
    // Esc leaves the pointing mode without a trace; "?" outside a text field enters it
    if st.point.active() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        st.point.esc();
        return;
    }
    if !ui.memory(|m| m.focused().is_some())
        && ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Slash))
    {
        st.point.toggle();
        return;
    }
    let exact = |m: Modifiers| ui.input(|i| i.modifiers.matches_exact(m));
    // the ghost draft on the canvas: Enter takes it as one undo step, Esc drops it
    if f.can_apply && !f.busy && !st.starting && logic::draft_keys_on_canvas(at) && exact(Modifiers::NONE) {
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            acts.push(Action::Apply);
            return;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            acts.push(Action::Reject);
            return;
        }
    }
    if apply
        && f.can_apply
        && exact(Modifiers::COMMAND)
        && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter))
    {
        acts.push(Action::Apply);
    }
    if stop
        && (f.busy || st.starting)
        && exact(Modifiers::NONE)
        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
    {
        acts.push(Action::Cancel);
    }
}

/// What the frame knows of the keyboard for `logic::card_keys`.
fn key_at(ui: &Ui, st: &PanelUi, cx: &Cx<'_>) -> logic::KeyAt {
    let ctx = ui.ctx();
    let ed = cx.ed;
    let tool =
        st.canvas_tool || ed.tool_armed() || !ed.wall_idle();
    logic::KeyAt {
        focus_in: kit::focus_inside(ui, ui.max_rect()),
        typing: ctx.wants_keyboard_input(),
        composer: st.composer_focused,
        canvas_free: crate::ui_kit::canvas_owns_keys(ctx),
        canvas_esc: tool || ed.drag.is_some() || !ed.selection.is_empty(),
    }
}

fn body(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    state: PanelState,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let connect = shows_connect_cta(state, st.log.is_empty());
    let pending = f.plan.as_deref().filter(|p| !p.items.is_empty());
    // only the connect card when there is no draft to decide on: a plan, or an agent's proposal
    // or draft (an agent over MCP needs no model: its draft is the person's to Apply)
    if connect && pending.is_none() && f.proposals.is_empty() && f.agents.is_empty() {
        // a refusal or a turn limit stays readable above the card
        errors(ui, st, cx, acts);
        if connect_card(ui, cx, false, acts) {
            acts.push(Action::OpenSettings);
        }
        return;
    }
    // the build and review tabs bring their own examples: the chat's greeting would crowd them
    let own_examples = matches!(st.modes.cur, Some(super::modes::AiMode::Build | super::modes::AiMode::Review));
    if st.log.is_empty() && f.plan.is_none() && state == PanelState::Idle && !own_examples {
        suggestions(ui, st, cx, icons, acts);
    }
    log(ui, st, cx, icons, f.busy || st.starting, acts, hot);
    if state == PanelState::Idle && f.plan.is_none() {
        follow_ups(ui, st, cx, acts);
    }
    let mut card_top = None;
    match state {
        PanelState::Streaming => kit::loading_state(ui, th, &cat.t("ai.state.streaming")),
        PanelState::Preparing => match st.prepared {
            Some((done, total)) if total > 0 => kit::progress(
                ui,
                th,
                done as f32 / total as f32,
                &cat.t("ai.state.preparing"),
            ),
            _ => kit::loading_state(ui, th, &cat.t("ai.state.preparing")),
        },
        PanelState::NeedConsent => consent::card(ui, st, cx, acts),
        PanelState::LocalOnlyBlocked => local_blocked(ui, cx, acts),
        _ => {}
    }
    if let Some(p) = pending {
        ui.add_space(space::SM);
        // a new draft opens at its counts line, also under a long conversation
        let seen = egui::Id::new("ddc.ai.plan_top_rev");
        if ui.data(|d| d.get_temp::<u64>(seen)) != Some(f.plan_rev) {
            ui.data_mut(|d| d.insert_temp(seen, f.plan_rev));
            card_top = Some(egui::Rect::from_min_size(ui.cursor().min, egui::vec2(1.0, 1.0)));
        }
        plan_card::card(ui, st, cx, p, f, acts, hot);
    }
    proposal_card::cards(ui, cx, f, acts, hot);
    super::agent_draft_card::cards(ui, cx, f, acts, hot);
    family_offers(ui, st, cx);
    errors(ui, st, cx, acts);
    if connect && connect_card(ui, cx, true, acts) {
        acts.push(Action::OpenSettings);
    }
    // asked last: a scroll area that ends first (rows, proposals) would take the request
    if let Some(r) = card_top {
        let now = egui::style::ScrollAnimation::none();
        ui.scroll_to_rect_animation(r, Some(egui::Align::TOP), now);
    }
}

/// What the last entry invites next: up to three prompts, each with the mode it runs in.
/// After an applied draft, checking it; after an answer, its sources or a change; else nothing.
pub(super) fn next_keys(last: Option<&Entry>, with_selection: bool) -> Vec<(&'static str, Mode)> {
    match last {
        Some(Entry::Applied(_)) => vec![("ai.next.check_change", Mode::ReadOnly), ("ai.prompt.fix", Mode::Propose)],
        Some(Entry::Assistant(_)) => {
            let mut v = vec![("ai.next.sources", Mode::ReadOnly)];
            if with_selection {
                v.push(("ai.prompt.explain", Mode::ReadOnly));
            }
            v.push(("ai.next.propose", Mode::Propose));
            v
        }
        _ => vec![],
    }
}

/// Follow-up chips under the last reply: one tap asks the next natural question.
fn follow_ups(ui: &mut Ui, st: &PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let with_selection = !cx.ed.selection.is_empty() && st.use_selection;
    let keys = next_keys(st.log.last(), with_selection);
    if keys.is_empty() {
        return;
    }
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(space::XS, space::XS);
        for (key, mode) in keys {
            let text = cat.t(key);
            let r = Frame::new()
                .fill(th.c("bg.sunken"))
                .stroke(egui::Stroke::new(1.0, th.c("border.subtle")))
                .corner_radius(th.radius("lg"))
                .inner_margin(Margin::symmetric(space::SM as i8, space::XXS as i8))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&text).font(th.font(Role::Label)).color(th.c("text.primary")),
                        )
                        .selectable(false)
                        .wrap(),
                    );
                })
                .response
                .interact(egui::Sense::click());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
            if r.hovered() {
                ui.painter().rect_stroke(
                    r.rect,
                    th.radius("lg"),
                    egui::Stroke::new(1.0, th.c("accent")),
                    egui::StrokeKind::Inside,
                );
            }
            if r.clicked() {
                acts.push(Action::Send(text, mode));
            }
        }
    });
}

/// Keys of the muted examples shown under the connect card.
pub(super) const CONNECT_EXAMPLES: [&str; 3] = ["ai.suggest.count", "ai.suggest.check", "ai.prompt.fix"];
/// What each example can already do without a connection: (button label key, command id).
/// Never a dead end: the same question gets a manual answer now, the assistant's after connect.
pub(super) const OFFLINE_ANSWERS: [(&str, &str); 3] = [
    ("sketch_hints.ai.offline.schedule", "view.schedule"),
    ("sketch_hints.ai.offline.check", "data.check"),
    ("sketch_hints.ai.offline.warnings", "view.warnings"),
];
/// Most room above the not-connected card, whatever the panel height.
pub(super) const CONNECT_TOP_MAX: f32 = space::XXL;

/// The not-connected group: the card with the one primary button, then quiet examples of what
/// to ask, each with a quiet button that answers it by hand right now. Under a pending draft it
/// is the card alone, in flow, with a quiet button: Apply owns the primary. True on connect.
fn connect_card(ui: &mut Ui, cx: &Cx<'_>, under_draft: bool, acts: &mut Vec<Action>) -> bool {
    let (th, cat) = (cx.th, cx.cat);
    let mut go = false;
    // the group reads from the top: a short panel spreads it a little, a tall one never opens a
    // void above the card; its height comes from the previous frame
    let hid = egui::Id::new("ddc.ai.connect_h");
    if under_draft {
        ui.add_space(space::SM);
    } else {
        let known: f32 = ui.data(|d| d.get_temp(hid)).unwrap_or(0.0);
        // a scroll area gives infinite height: the visible clip is the room there is
        let room = ui.clip_rect().bottom() - ui.cursor().top();
        let free = if room.is_finite() { (room - known).max(0.0) } else { 0.0 };
        ui.add_space((free * 0.4).clamp(space::MD, CONNECT_TOP_MAX));
    }
    let y0 = ui.cursor().top();
    kit::card(ui, th, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(cat.t("ai.state.no_profile"))
                    .font(th.font(Role::BodyStrong))
                    .color(th.c("text.primary")),
            );
            ui.add_space(space::XS);
            ui.label(
                RichText::new(cat.t("ai.state.no_profile.hint"))
                    .font(th.font(Role::Body))
                    .color(th.c("text.secondary")),
            );
            ui.add_space(space::MD);
            let label = cat.t("ai.state.no_profile.action");
            let button = if under_draft {
                Button::secondary(&label)
            } else {
                Button::primary(&label)
            };
            go = button.show(ui, th).clicked();
        });
    });
    if under_draft {
        return go;
    }
    ui.add_space(space::LG);
    kit::caption(ui, th, &cat.t("ai.state.no_profile.examples"));
    ui.add_space(space::XS);
    for (key, (label, cmd)) in CONNECT_EXAMPLES.into_iter().zip(OFFLINE_ANSWERS) {
        Frame::new()
            .fill(th.c("bg.sunken"))
            .stroke(egui::Stroke::new(1.0, th.c("border.subtle")))
            .corner_radius(th.radius("md"))
            .inner_margin(Margin::symmetric(space::SM as i8, space::XS as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(
                    egui::Label::new(
                        RichText::new(cat.t(key))
                            .font(th.font(Role::Label))
                            .color(th.c("text.secondary")),
                    )
                    .selectable(false)
                    .wrap(),
                );
                if Button::quiet(&cat.t(label)).small().show(ui, th).clicked() {
                    acts.push(Action::Offline(cmd));
                }
            });
        ui.add_space(space::XS);
    }
    let h = ui.cursor().top() - y0;
    ui.data_mut(|d| d.insert_temp(hid, h));
    go
}

fn prompt_icon(key: &str) -> &'static str {
    match key {
        "ai.suggest.check" => "check",
        "ai.suggest.replace" => "spark-fix",
        "ai.prompt.explain" => "spark-why",
        _ => "query",
    }
}

/// A quiet starter row: icon, then the prompt wrapped whole. True when clicked.
fn prompt_row(
    ui: &mut Ui,
    th: &ddc_ui::Theme,
    icons: &mut IconCache,
    key: &str,
    text: &str,
) -> bool {
    let icon = icon_size::SM;
    let pad = space::XS;
    let wrap = (ui.available_width() - icon - space::SM - 2.0 * pad).max(1.0);
    let (font, ink) = (th.font(Role::Label), th.c("text.secondary"));
    let galley = ui.painter().layout(text.to_string(), font, ink, wrap);
    let h = (galley.size().y + 2.0 * pad).max(th.d().row);
    let size = egui::vec2(ui.available_width(), h);
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    // a quiet card that lifts on hover: the starters read as things to tap, not as text
    let fill = th.c(if r.hovered() { "bg.hover" } else { "bg.sunken" });
    let edge = egui::Stroke::new(1.0, th.c("border.subtle"));
    ui.painter().rect(rect, th.radius("md"), fill, edge, egui::StrokeKind::Inside);
    let at = egui::pos2(rect.left() + pad, rect.center().y - icon / 2.0);
    let color = th.c(if r.hovered() { "text.primary" } else { "text.secondary" });
    let spot = egui::Rect::from_min_size(at, egui::Vec2::splat(icon));
    icons.paint(ui, prompt_icon(key), icon as u32, spot, color);
    let text_y = rect.center().y - galley.size().y / 2.0;
    ui.painter().galley(egui::pos2(at.x + icon + space::SM, text_y), galley, color);
    r.clicked()
}

fn suggestions(
    ui: &mut Ui,
    st: &PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    widgets::empty_state(
        ui,
        th,
        Some((&mut *icons, "spark")),
        &cat.t("ai.state.idle"),
        &cat.t("ai.state.idle.hint"),
        None,
    );
    // the rows sit right above the composer: the room above them is last frame's leftover
    let hid = egui::Id::new("ddc.ai.prompts_h");
    let known: f32 = ui.data(|d| d.get_temp(hid)).unwrap_or(0.0);
    let room = ui.clip_rect().bottom() - ui.cursor().top();
    if room.is_finite() {
        ui.add_space((room - known - 1.0).max(space::MD));
    }
    let y0 = ui.cursor().top();
    let with_selection = !cx.ed.selection.is_empty() && st.use_selection;
    for (key, mode) in suggestion_keys(with_selection) {
        let text = cat.t(key);
        if prompt_row(ui, th, icons, key, &text) {
            acts.push(Action::Send(text, mode));
        }
        ui.add_space(space::XS);
    }
    let h = ui.cursor().top() - y0;
    ui.data_mut(|d| d.insert_temp(hid, h));
}

fn local_blocked(ui: &mut Ui, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    kit::card(ui, th, |ui| {
        ui.label(
            RichText::new(cat.t("ai.state.local_only"))
                .font(th.font(Role::BodyStrong))
                .color(th.c("text.primary")),
        );
        ui.label(
            RichText::new(cat.t("ai.state.local_only.hint"))
                .font(th.font(Role::Label))
                .color(th.c("text.secondary")),
        );
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            if Button::secondary(&cat.t("cmd.ai.settings"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::OpenSettings);
            }
            if Button::quiet(&cat.t("ai.common.cancel"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ConsentNo);
            }
        });
    });
}

/// Family drafts of external agents: they wait here until the person opens or dismisses them.
/// The answer goes through egui's memory; `family_editor::windows` applies it.
fn family_offers(ui: &mut Ui, st: &PanelUi, cx: &Cx<'_>) {
    for (name, _, _, _) in &st.family_offers {
        ui.horizontal_wrapped(|ui| {
            ui.label(cx.cat.tf("ai.fam_offer", &[("name", name)]));
            if widgets::Button::secondary(&cx.cat.t("ai.fam_offer.open")).show(ui, cx.th).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("ddc.fam_offer"), (name.clone(), true)));
            }
            if widgets::Button::quiet(&cx.cat.t("ai.fam_offer.dismiss")).show(ui, cx.th).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("ddc.fam_offer"), (name.clone(), false)));
            }
        });
    }
}

fn errors(ui: &mut Ui, st: &PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    if retry::card(ui, cx, &st.retry, acts) {
        return;
    }
    let (kind, detail) = match (&st.error, st.limit) {
        (Some(e), _) => {
            let k = logic::classify(e);
            let mut hint = cat.t(&format!("{}.hint", logic::err_key(k)));
            if let Some(d) = logic::err_detail(e) {
                hint = format!("{hint}\n{}", cat.tf("ai.err.detail", &[("text", &d)]));
            }
            (k, hint)
        }
        (None, Some(what)) => (ErrKind::TurnLimit, cat.t(logic::turn_limit_key(what))),
        (None, None) => return,
    };
    if st.retry.exhausted() {
        retry::exhausted_note(ui, cx);
    }
    let retry = cat.t("ai.common.retry");
    let settings = cat.t("cmd.ai.settings");
    let shorten = cat.t(retry::fix_label_key(retry::Fix::Shorten));
    let shorten_fix = retry::fix_for(kind) == Some(retry::Fix::Shorten);
    let mut labels: Vec<&str> = vec![];
    if shorten_fix {
        labels.push(shorten.as_str());
    } else {
        if logic::can_retry(kind) {
            labels.push(retry.as_str());
        }
        if logic::goes_to_settings(kind) || logic::can_retry(kind) {
            labels.push(settings.as_str());
        }
    }
    ui.add_space(space::SM);
    let hit = kit::error_state(ui, th, &cat.t(logic::err_key(kind)), &detail, &labels);
    let tech = st.error.as_ref().map(logic::error_detail).unwrap_or_default();
    if !tech.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(cat.t("ai.err.details"))
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        )
        .id_salt("ddc.ai.err_details")
        .show(ui, |ui| {
            if let Some(code) = tech.status {
                kit::caption(ui, th, &cat.tf("ai.err.http", &[("code", &code.to_string())]));
            }
            if let Some(n) = tech.retry_after_s {
                kit::caption(ui, th, &cat.tf("ai.err.retry_after", &[("n", &n.to_string())]));
            }
            if !tech.text.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(&tech.text)
                            .font(th.font(Role::Numeric))
                            .color(th.c("text.secondary")),
                    )
                    .selectable(true)
                    .wrap(),
                );
            }
        });
    }
    if let Some(i) = hit {
        if shorten_fix {
            acts.push(Action::Shorten);
        } else if labels[i] == retry {
            acts.push(Action::Retry);
        } else {
            acts.push(Action::OpenSettings);
        }
    }
}

fn log(
    ui: &mut Ui,
    st: &PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    busy: bool,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let undo_top = super::receipt::undo_head(cx.ed);
    let items = &st.log;
    let mut i = 0;
    while i < items.len() {
        match &items[i] {
            Entry::Tool { .. } => {
                let end = (i..items.len())
                    .find(|j| !matches!(items[*j], Entry::Tool { .. }))
                    .unwrap_or(items.len());
                tools(ui, cx, &items[i..end], i);
                i = end;
                continue;
            }
            Entry::User(text) => {
                let shown = ui.scope(|ui| bubble(ui, cx, text)).response.rect;
                super::recall::actions(ui, cx, text, i, busy, shown);
            }
            Entry::Assistant(text) => {
                // a reply after the person's words is signed once; follow-ups in a run are not
                if i == 0 || matches!(items[i - 1], Entry::User(_)) {
                    author(ui, cx, icons);
                }
                super::md::answer(ui, cx, text, i);
                if let Some(r) = super::md::take_pill_click(ui.ctx()) {
                    acts.extend(super::md::pill_action(&r));
                }
            }
            Entry::Note(key) => {
                kit::caption(ui, th, &cat.t(key));
            }
            Entry::Applied(r) => super::receipt::card(ui, cx, r, undo_top, acts, hot),
            Entry::Redraft(n) => {
                kit::caption(ui, th, &n.history(cat));
            }
            Entry::Export {
                cat: what,
                columns,
                group,
                saved,
                ..
            } => {
                kit::card(ui, th, |ui| {
                    ui.label(
                        RichText::new(cat.tf("ai.export.offer", &[("cat", what)]))
                            .font(th.font(Role::Label))
                            .color(th.c("text.primary")),
                    );
                    if !columns.is_empty() {
                        kit::caption(ui, th, &columns.join(", "));
                    }
                    if let Some(g) = group {
                        kit::caption(ui, th, &cat.tf("ai.export.group", &[("col", g)]));
                    }
                    let label = cat.t(if *saved {
                        "ai.export.saved"
                    } else {
                        "ai.export.save"
                    });
                    if Button::secondary(&label)
                        .small()
                        .enabled(!*saved)
                        .show(ui, th)
                        .clicked()
                    {
                        acts.push(Action::Export(i));
                    }
                });
            }
            Entry::Schedule { made, saved } => {
                kit::card(ui, th, |ui| {
                    ui.label(
                        RichText::new(&made.title)
                            .font(th.font(Role::BodyStrong))
                            .color(th.c("text.primary")),
                    );
                    kit::caption(
                        ui,
                        th,
                        &cat.tf(
                            "ai.schedule.rows",
                            &[("cat", &made.cat), ("n", &made.rows.to_string())],
                        ),
                    );
                    if !made.preset.group.is_empty() {
                        kit::caption(
                            ui,
                            th,
                            &cat.tf("ai.export.group", &[("col", &made.preset.group.join(", "))]),
                        );
                    }
                    ui.horizontal(|ui| {
                        if Button::primary(&cat.t("ai.schedule.open"))
                            .small()
                            .show(ui, th)
                            .clicked()
                        {
                            acts.push(Action::OpenSchedule(i));
                        }
                        let label = cat.t(if *saved {
                            "ai.export.saved"
                        } else {
                            "ai.export.save"
                        });
                        if Button::secondary(&label)
                            .small()
                            .enabled(!*saved)
                            .show(ui, th)
                            .clicked()
                        {
                            acts.push(Action::ExportSchedule(i));
                        }
                    });
                });
            }
            Entry::Rule { def, text, saved } => {
                kit::card(ui, th, |ui| {
                    ui.label(
                        RichText::new(&def.title)
                            .font(th.font(Role::BodyStrong))
                            .color(th.c("text.primary")),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .font(th.font(Role::Label))
                                .color(th.c("text.secondary")),
                        )
                        .wrap(),
                    );
                    let label = cat.t(if *saved {
                        "ai.rule.saved"
                    } else {
                        "ai.rule.save"
                    });
                    if Button::secondary(&label)
                        .small()
                        .enabled(!*saved)
                        .show(ui, th)
                        .clicked()
                    {
                        acts.push(Action::SaveRule(i));
                    }
                });
            }
            Entry::Found(f) => super::found::card(ui, cx, f, i, acts, hot),
            Entry::Issue { topic, saved } => {
                kit::card(ui, th, |ui| {
                    ui.label(
                        RichText::new(&topic.title)
                            .font(th.font(Role::BodyStrong))
                            .color(th.c("text.primary")),
                    );
                    if !topic.description.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&topic.description)
                                    .font(th.font(Role::Label))
                                    .color(th.c("text.secondary")),
                            )
                            .wrap(),
                        );
                    }
                    let n = topic.elements.len().to_string();
                    ui.label(
                        RichText::new(cat.tf("ai.issue.elements", &[("n", n.as_str())]))
                            .font(th.font(Role::Label))
                            .color(th.c("text.secondary")),
                    );
                    ui.horizontal_wrapped(|ui| {
                        let keys: Vec<_> = topic.elements.iter().map(|e| (e.table.clone(), e.id)).collect();
                        if !keys.is_empty()
                            && Button::secondary(&cat.t("ai.issue.show")).small().show(ui, th).clicked()
                        {
                            acts.push(Action::ShowDraft(keys));
                        }
                        let label = cat.t(if saved.is_some() {
                            "ai.issue.saved"
                        } else {
                            "ai.issue.save"
                        });
                        if Button::secondary(&label)
                            .small()
                            .enabled(saved.is_none())
                            .show(ui, th)
                            .clicked()
                        {
                            acts.push(Action::SaveIssue(i));
                        }
                    });
                });
            }
            Entry::Req { offer, done } => super::requirements::card(ui, cx, offer, *done, i, acts),
        }
        // a turn ends with a wider gap than the parts inside it
        // the person's copy/edit row already spaces their bubble from the reply
        let gap = match (&items[i], items.get(i + 1)) {
            (Entry::User(_), _) => space::XS,
            (_, Some(Entry::User(_))) => space::LG,
            _ => space::SM,
        };
        ui.add_space(gap);
        i += 1;
    }
}

/// The assistant's name over its reply: the spark in the accent, then the name in a caption.
fn author(ui: &mut Ui, cx: &Cx<'_>, icons: &mut IconCache) {
    let th = cx.th;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        let s = icon_size::SM;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(s, s), egui::Sense::hover());
        icons.paint(ui, "spark", s as u32, rect, th.c("accent"));
        ui.add(
            egui::Label::new(
                RichText::new(cx.cat.t("ai.panel.title"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .selectable(false),
        );
    });
    ui.add_space(space::XXS);
}

/// Own message: 12 px corners, the one next to the author (bottom right) 4.
pub(super) fn bubble_radius(th: &ddc_ui::Theme) -> egui::CornerRadius {
    let r = th.radius("lg").round().clamp(0.0, 255.0) as u8;
    let corner = th.radius("sm").round().clamp(0.0, 255.0) as u8;
    egui::CornerRadius { nw: r, ne: r, sw: r, se: corner }
}

fn bubble(ui: &mut Ui, cx: &Cx<'_>, text: &str) {
    let th = cx.th;
    // right-to-left languages put the own bubble on the left and align its text to the right
    let rtl = ddc_ui::i18n::rtl();
    let side = if rtl { egui::Align::Min } else { egui::Align::Max };
    ui.with_layout(egui::Layout::top_down(side), |ui| {
        let max = ui.available_width() * BUBBLE_SHARE;
        Frame::new()
            .fill(th.c("bg.selected"))
            .corner_radius(bubble_radius(th))
            .inner_margin(Margin::symmetric(space::MD as i8, space::SM as i8))
            .show(ui, |ui| {
                ui.set_max_width(max);
                ui.with_layout(egui::Layout::top_down(ddc_ui::i18n::text_align()), |ui| {
                    let font = th.font(Role::Body);
                    let w = ui.available_width();
                    // right-to-left text is wrapped logically first, then shaped and ordered per line
                    let shaped = ddc_ui::rtl::contains_rtl(text).then(|| {
                        ddc_ui::rtl::display_wrapped(text, rtl, w, |t| {
                            ui.fonts(|f| {
                                f.layout_no_wrap(t.to_owned(), font.clone(), th.c("text.primary")).size().x
                            })
                        })
                    });
                    let label = egui::Label::new(
                        RichText::new(shaped.as_deref().unwrap_or(text))
                            .font(font)
                            .color(th.c("text.primary")),
                    )
                    .selectable(true);
                    ui.add(if shaped.is_some() { label.extend() } else { label.wrap() });
                });
            });
    });
}

/// A run of tool calls is one quiet card; see `steps`.
fn tools(ui: &mut Ui, cx: &Cx<'_>, run: &[Entry], at: usize) {
    let run: Vec<&super::steps::Step> = run
        .iter()
        .filter_map(|e| match e {
            Entry::Tool(s) => Some(s),
            _ => None,
        })
        .collect();
    super::steps::card(ui, cx, &run, at);
}

fn composer(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    f: &Facts,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let id = egui::Id::new("ddc.ai.composer");
    // nothing to answer with: the field stays in place but cannot take input or send
    let off = !st.active_ready();
    if st.focus_input && !off {
        ui.memory_mut(|m| m.request_focus(id));
        st.focus_input = false;
    }
    let focused = ui.memory(|m| m.has_focus(id));
    st.composer_focused = focused;
    if !off {
        // the depth the next run works to, right above where the request is typed
        super::depth_chip::show(ui, st, cx, acts);
    }
    let idle = st.consent.is_none() && !f.busy && !st.starting;
    super::recall::keys(ui, id, &mut st.input, &mut st.recall, &st.log, focused && idle);
    let focused = ui.memory(|m| m.has_focus(id));
    // Enter sends and Shift+Enter breaks the line; `consume_key` alone would also take Shift+Enter
    let enter = focused
        && st.consent.is_none()
        && ui.input_mut(|i| {
            !i.modifiers.shift && !i.modifiers.command && i.consume_key(Modifiers::NONE, Key::Enter)
        });
    let hint = cat.t(if off {
        "ai.composer.hint_off"
    } else if st.propose {
        "ai.composer.hint_propose"
    } else {
        "ai.composer.hint_read"
    });
    // one surface holds the field and its actions; the ring takes the accent while typing
    let ring = if focused && !off { th.c("accent") } else { th.c("border.control") };
    let surface = Frame::new()
        .fill(th.c("bg.input"))
        .stroke(egui::Stroke::new(if focused && !off { 1.5 } else { 1.0 }, ring))
        .corner_radius(th.radius("lg"))
        // a thin top edge keeps the depth chip close to the text it sets
        .inner_margin(Margin { top: space::XXS as i8, ..Margin::same(space::XS as i8) });
    let mut send = false;
    surface.show(ui, |ui| {
    let field = egui::ScrollArea::vertical()
        .id_salt("ddc.ai.composer_scroll")
        .max_height(input_max_height(th))
        .min_scrolled_height(input_min_height(th))
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut st.input)
                    .id(id)
                    .interactive(!off)
                    .desired_rows(COMPOSER_MIN_ROWS)
                    .desired_width(f32::INFINITY)
                    .margin(composer_margin())
                    .frame(false)
                    .hint_text(RichText::new(hint).color(th.c("text.secondary")))
                    .font(th.font(Role::Body))
                    .return_key(Some(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter))),
            );
        });
    // `inner_rect` is the room the area was offered; the field shrinks to its text inside it
    #[cfg(test)]
    super::kit::probe::set_field(egui::Rect::from_min_size(
        field.inner_rect.min,
        field.inner_rect.size().min(field.content_size),
    ));
    #[cfg(not(test))]
    let _ = field;
    if off {
        return;
    }
    if focused && idle {
        paste_attachments(ui, st, acts);
    }
    let context = budget::context_of(st);
    let chips = budget::live_chips(st, cx);
    let bud = budget::measure(&st.input, &chips, context);
    let can_send = budget::can_send(&st.input, f.busy, bud.level);
    let send_tip = if bud.level == budget::Level::Block {
        budget::reason(cat, &bud)
    } else {
        cat.t("ai.composer.send_tip")
    };
    send = enter && can_send;
    // the toolbar inside the surface: attach and reference on the left, Send (or Stop) on the right
    ui.horizontal(|ui| {
        if !f.busy && !st.starting {
            if widgets::icon_button(ui, icons, th, "plus", false, &cat.t("ai.att.attach_tip")).clicked() {
                acts.push(Action::AttachFiles);
            }
            if widgets::icon_button(ui, icons, th, "reference", false, &cat.t("ai.ref.attach_tip")).clicked() {
                acts.push(Action::AttachReference);
            }
            mode_chip(ui, st, cx, acts);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if f.busy || st.starting {
            let label = cat.t("ai.common.stop");
            if Button::secondary(&label)
                .small()
                .tooltip(&cat.t("cmd.ai.cancel"), logic::STOP_KEYS)
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::Cancel);
            }
            return;
        }
        // Apply, or the consent choice, is the primary while it is shown: only one per surface
        let label = cat.t("ai.common.send");
        let b = if f.plan.is_some() || st.consent.is_some() {
            Button::secondary(&label)
        } else {
            Button::primary(&label)
        };
        if b.small()
            .enabled(can_send)
            .tooltip(&send_tip, "Enter")
            .show(ui, th)
            .clicked()
        {
            send = true;
        }
        });
    });
    });
    if off {
        return;
    }
    if !f.busy && !st.starting {
        // the keys in one quiet line under the surface, where the eye leaves the field
        ui.add_space(space::XXS);
        ui.add(
            egui::Label::new(
                RichText::new(cat.t("ai.composer.keys_hint"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .selectable(false)
            .wrap(),
        );
        let context = budget::context_of(st);
        let chips = budget::live_chips(st, cx);
        let bud = budget::measure(&st.input, &chips, context);
        let way = budget::way_out(&st.input, &chips, context);
        if let Some(w) = budget::row(ui, th, cat, &bud, way) {
            budget::apply(w, st, &chips, context);
        }
    }
    if send {
        st.recall.reset();
        acts.push(Action::Send(std::mem::take(&mut st.input), mode_of(st)));
        st.focus_input = true;
    }
}

/// Ctrl+V in the composer: pasted file paths attach those files instead of landing as text,
/// and a paste that brought no text may be an image on the clipboard.
fn paste_attachments(ui: &Ui, st: &mut PanelUi, acts: &mut Vec<Action>) {
    let (text, key) = ui.input(|i| {
        let text = i.events.iter().find_map(|e| match e {
            egui::Event::Paste(s) => Some(s.clone()),
            _ => None,
        });
        (text, i.modifiers.command && i.key_pressed(Key::V))
    });
    match text {
        Some(s) if !s.trim().is_empty() => {
            let paths = super::attach_ui::pasted_paths(&s);
            if !paths.is_empty() {
                st.input = st.input.replacen(&s, "", 1);
                acts.push(Action::AttachPaths(paths));
            }
        }
        _ if key => acts.push(Action::AttachClipboardImage),
        _ => {}
    }
}

fn status_line(
    ui: &mut Ui,
    st: &PanelUi,
    cx: &Cx<'_>,
    state: PanelState,
    f: &Facts,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let row = stream_status::stream_status(&st.stream, web_time::Instant::now());
    // frames only while a turn streams: an idle panel asks for none
    if let Some(after) = stream_status::repaint_after(&row).filter(|_| f.busy) {
        ui.ctx().request_repaint_after(after);
    }
    ui.horizontal(|ui| {
        if f.busy {
            kit::spinner(ui, th);
        }
        if f.busy && row.live() {
            let line = stream_status::row_text(&row, &|k, a| cat.tf(k, a));
            let line = if row.caret {
                let calm = th.reduced_motion || ui.ctx().style().animation_time <= 0.0;
                let t = ui.ctx().input(|i| i.time);
                if !calm {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                }
                // the caret keeps its width when off so the line does not jitter
                let c = if crate::ai::ghost::caret_visible(t, calm) { "\u{258c}" } else { "\u{2002}" };
                format!("{line} {c}")
            } else {
                line
            };
            let r = kit::caption(ui, th, &line);
            if let Some(name) = &row.tool {
                r.on_hover_text(cat.t(&logic::tool_key(name)));
            }
            if row.stalled
                && Button::quiet(&cat.t("ai.stream.stop"))
                    .small()
                    .tooltip(&cat.t("cmd.ai.cancel"), logic::STOP_KEYS)
                    .show(ui, th)
                    .clicked()
            {
                acts.push(Action::Cancel);
            }
        } else {
            // the greeting is the empty panel's own title; idle, the line stays quiet
            if state != PanelState::Idle {
                kit::caption(ui, th, &cat.t(logic::state_key(state)));
            }
            if row.incomplete {
                kit::caption(ui, th, &cat.t("ai.stream.incomplete"));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !st.sent.is_empty()
                && Button::quiet(&cat.t("cmd.ai.what_sent"))
                    .small()
                    .show(ui, th)
                    .clicked()
            {
                acts.push(Action::OpenSent);
            }
            let steps = st.journal.steps.len();
            if steps > 0 {
                let label = cat.tf("ai.journal.open", &[("n", &steps.to_string())]);
                if Button::quiet(&label).small().show(ui, th).clicked() {
                    acts.push(Action::ToggleJournal);
                }
            }
            let used = st.usage.session.total();
            if used > 0 {
                let line = usage_line(cat, used, st.usage.cost);
                let r = kit::caption(ui, th, &line);
                if st.usage.cost.is_some() {
                    r.on_hover_text(cat.t("ai.usage.cost_tip"));
                }
            }
        });
    });
    if st.journal_open && !st.journal.steps.is_empty() {
        step_feed(ui, &st.journal, cx, acts);
    }
}

/// The step feed of the last request: a line per model step, the totals, and the export.
fn step_feed(ui: &mut Ui, j: &ddc_ai::TaskJournal, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    ui.vertical(|ui| {
        for s in &j.steps {
            kit::caption(ui, th, &logic::step_line(cat, s));
        }
        ui.horizontal(|ui| {
            kit::caption(ui, th, &logic::journal_total_line(cat, j));
            if Button::quiet(&cat.t("ai.journal.export"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ExportJournal);
            }
        });
    });
}

/// Which connection controls the header carries. With no connection there is nothing to pick
/// and nothing to answer with: the body's one button is the way in.
#[derive(Debug, PartialEq, Eq)]
struct HeaderParts {
    picker: bool,
    mode: bool,
    /// Context chips and the tab bar: only with a usable connection.
    strip: bool,
}

fn header_parts(has_profiles: bool, ready: bool) -> HeaderParts {
    HeaderParts {
        picker: has_profiles,
        mode: has_profiles,
        strip: ready,
    }
}

fn shows_connect_cta(state: PanelState, log_empty: bool) -> bool {
    state == PanelState::NoProfile && log_empty
}

/// Starter prompts; those about "the selected elements" only when something is selected.
fn suggestion_keys(with_selection: bool) -> Vec<(&'static str, Mode)> {
    let mut list = vec![];
    if with_selection {
        list.push(("ai.prompt.explain", Mode::ReadOnly));
    }
    list.push(("ai.suggest.count", Mode::ReadOnly));
    list.push(("ai.suggest.check", Mode::ReadOnly));
    if with_selection {
        list.push(("ai.suggest.replace", Mode::Propose));
    }
    list
}

/// Tokens of the session, then the estimated cost with what it is based on.
pub(super) fn usage_line(cat: &ddc_ui::Catalog, used: u64, cost: Option<f64>) -> String {
    let mut line = cat.tf("ai.usage.tokens", &[("n", &logic::fmt_tokens(used))]);
    if let Some(c) = cost {
        line.push_str(" · ");
        let ru = cat.lang == ddc_ui::Lang::Ru;
        let cost = crate::units::fmt_num_lang(c, 2, ru);
        line.push_str(&cat.tf("ai.usage.cost", &[("cost", &cost)]));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_placeholder_fits_320_px_panel() {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let ctx = egui::Context::default();
        let avail = 320.0 - 2.0 * space::LG - composer_margin().sum().x;
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let mut widths = Vec::new();
            for _ in 0..2 {
                widths.clear();
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    th.apply(ctx);
                    for k in ["ai.composer.hint_propose", "ai.composer.hint_read"] {
                        let t = cat.t(k);
                        assert_ne!(t, k, "{k} in {lang:?}");
                        let w = ctx.fonts(|f| {
                            f.layout_no_wrap(t, th.font(Role::Body), egui::Color32::WHITE)
                                .size()
                                .x
                        });
                        widths.push((k, w));
                    }
                });
            }
            for (k, w) in widths {
                assert!(w > 0.0 && w <= avail, "{k} {lang:?}: {w} > {avail}");
            }
            assert_ne!(cat.t("ai.composer.keys_hint"), "ai.composer.keys_hint", "{lang:?}");
        }
    }

    #[test]
    fn send_and_reference_buttons_fit_the_reserve() {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let ctx = egui::Context::default();
        let reserve = th.d().row * SEND_RESERVE_ROWS;
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let mut total = 0.0;
            for _ in 0..2 {
                total = 0.0;
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    th.apply(ctx);
                    for k in ["ai.common.send", "ai.common.stop", "ai.ref.attach"] {
                        let t = cat.t(k);
                        assert_ne!(t, k, "{k} in {lang:?}");
                        let w = ctx.fonts(|f| {
                            f.layout_no_wrap(t, th.font(Role::Body), egui::Color32::WHITE).size().x
                        });
                        // Reference is an icon beside it; Stop never sits beside Send
                        if k == "ai.common.send" {
                            total += w + 2.0 * space::MD;
                        }
                    }
                });
            }
            assert!(total > 0.0 && total <= reserve, "{lang:?}: {total} > {reserve}");
        }
    }

    #[test]
    fn composer_min_height_is_two_rows() {
        let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let body = th.font(Role::Body).size;
        assert!(COMPOSER_MIN_ROWS >= 2);
        assert!(input_min_height(&th) >= 2.0 * body + composer_margin().sum().y);
        assert!(input_min_height(&th) <= th.d().row * COMPOSER_ROWS);
    }

    #[test]
    fn no_profile_shows_one_connect_action() {
        assert_eq!(
            header_parts(false, false),
            HeaderParts {
                picker: false,
                mode: false,
                strip: false
            },
            "the header has no second connect button and no live mode switch"
        );
        assert!(shows_connect_cta(PanelState::NoProfile, true));
        assert!(!shows_connect_cta(PanelState::Idle, true));
        assert_eq!(
            header_parts(true, true),
            HeaderParts {
                picker: true,
                mode: true,
                strip: true
            }
        );
    }

    #[test]
    fn follow_ups_fit_what_came_last() {
        assert!(next_keys(None, false).is_empty());
        assert!(next_keys(Some(&Entry::User("q".into())), true).is_empty());
        let a = Entry::Assistant("a".into());
        let plain: Vec<_> = next_keys(Some(&a), false).into_iter().map(|k| k.0).collect();
        assert_eq!(plain, ["ai.next.sources", "ai.next.propose"]);
        assert_eq!(next_keys(Some(&a), true).len(), 3, "a selection adds the explain prompt");
        assert!(next_keys(Some(&a), true).iter().any(|(_, m)| *m == Mode::Propose));
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in ["ai.next.sources", "ai.next.propose", "ai.next.check_change"] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
        }
    }

    #[test]
    fn replace_suggestion_needs_selection() {
        let keys = |sel| -> Vec<&'static str> {
            suggestion_keys(sel).into_iter().map(|(k, _)| k).collect()
        };
        assert!(!keys(false).contains(&"ai.suggest.replace"));
        assert!(!keys(false).contains(&"ai.prompt.explain"));
        assert!(keys(true).contains(&"ai.suggest.replace"));
        assert!(keys(true).contains(&"ai.prompt.explain"));
        assert!(suggestion_keys(true)
            .iter()
            .any(|(k, m)| *k == "ai.suggest.replace" && *m == Mode::Propose));
    }

    #[test]
    fn cost_caption_is_labelled() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in ["ai.usage.cost", "ai.usage.cost_tip", "ai.panel.close"] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
            let bare = usage_line(&cat, 1200, None);
            let line = usage_line(&cat, 1200, Some(0.125));
            let num = crate::units::fmt_num_lang(0.125, 2, lang == ddc_ui::Lang::Ru);
            assert!(line.starts_with(&bare), "{line}");
            let tail = &line[bare.len()..];
            assert!(tail.contains(&num), "{line}");
            assert!(tail.contains('≈'), "approximate sign: {line}");
            assert!(
                tail.trim_start_matches(" · ").chars().count() > num.chars().count() + 2,
                "more than a bare number: {line}"
            );
        }
    }
}
