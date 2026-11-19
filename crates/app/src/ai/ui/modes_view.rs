//! The work column of an AI tab and the bar over its preview. Drawn after the view area with
//! the whole app in hand, from the one `PanelUi`; the actions run after the draw.

use super::kit::{self, space, Button, Tone};
use super::modes::{
    build_step, AiMode, BuildForm, BuildStep, Layout, Preview, OUTPUTS,
};
use super::{chat, checks, logic, preview, Action, Cx, Facts, PanelUi, PANEL};
use super::modes::AiFold;
use crate::shell::{fold, EditorApp};
use ddc_ai::Mode;
use ddc_model::ElKey;
use ddc_ui::icons::IconCache;
use ddc_ui::sem::stroke;
use ddc_ui::theme::Role;
use ddc_ui::widgets::{self, NumberField, NumberFormat};
use egui::{self, Key, Modifiers, RichText, TextEdit, Ui, UiBuilder};

/// The one delivery window (drawings and files) when it is part of the build.
const DELIVER: &str = "file.deliver";
/// Without it, a sheet per level, then the ticked export commands.
const DRAWINGS: &str = "view.sheets_for_levels";
/// The assistant's drawing-set tool, asked for from the chat when the catalog has it.
const SET_TOOL: &str = "create_drawing_set";
/// Example briefs of the build tab.
const EXAMPLES: [&str; 3] = [
    "ai_modes.build.ex.house",
    "ai_modes.build.ex.section",
    "ai_modes.build.ex.school",
];
/// Labels of `ddc_gen::project::Typology::NAMES`, in that order.
const KINDS: [&str; 4] = [
    "ai_modes.build.kind.house",
    "ai_modes.build.kind.section",
    "ai_modes.build.kind.office",
    "ai_modes.build.kind.school",
];
/// Widest the setup and brief cards grow in a wide column.
const CARD_MAX: f32 = 640.0;
const BRIEF_ID: &str = "ddc.ai.modes.brief";
/// Smallest part of the review column (checks or answers).
const PART_MIN: f32 = 160.0;
/// Width of the number fields of the build form.
const NUM_W: f32 = 88.0;

/// What the app offers an AI tab this frame: registered commands with their labels.
pub(super) struct Offer {
    /// The build tab's drawings step: the delivery window, else the per-level sheets.
    pub drawings: Option<(&'static str, String)>,
    /// The files step: the delivery window, else the exports ticked in the build form.
    pub files: Vec<(&'static str, String)>,
    /// The chat's file request: the delivery window, else the IFC copy.
    pub ifc: Option<(&'static str, String)>,
    pub first_sheet: Option<i64>,
}

pub(super) fn offer(app: &EditorApp, form: &BuildForm) -> Offer {
    let has = |id: &str| app.handlers.contains_key(id);
    let label = |id: &'static str| {
        let key = app.sh.reg.get(id).map_or(format!("cmd.{id}"), |c| c.label_key());
        (id, app.sh.cat.t(&key))
    };
    let deliver = has(DELIVER);
    let pick = |id: &'static str| {
        if deliver {
            Some(label(DELIVER))
        } else {
            has(id).then(|| label(id))
        }
    };
    let files = if deliver {
        vec![label(DELIVER)]
    } else {
        form.output_cmds().filter(|&id| has(id)).map(|id| label(id)).collect()
    };
    Offer {
        drawings: pick(DRAWINGS),
        files,
        ifc: pick("data.export_ifc"),
        first_sheet: app.ed.model.sheets().ok().and_then(|v| v.first().map(|s| s.0)),
    }
}

/// Draws the AI tab in `lay` and runs what it asked for. The conversation is the panel's.
pub fn workspace(app: &mut EditorApp, ctx: &egui::Context, lay: &Layout) {
    let mut st = PANEL.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let mut cmds = vec![];
    if let Some(mode) = st.modes.cur {
        cmds = frame(app, ctx, &mut st, mode, lay);
    }
    PANEL.with(|p| *p.borrow_mut() = st);
    // commands run with the conversation back in place: their handlers may open the panel
    for id in cmds {
        app.run(id);
    }
}

fn frame(
    app: &mut EditorApp,
    ctx: &egui::Context,
    st: &mut PanelUi,
    mode: AiMode,
    lay: &Layout,
) -> Vec<&'static str> {
    st.init(app);
    let facts = Facts::read();
    let offer = offer(app, &st.modes.form);
    let palette_open = app.sh.palette.open;
    let view3d = app.view3d;
    let mut acts: Vec<Action> = vec![];
    let mut hot: Option<Vec<ElKey>> = None;
    let mut cmds = vec![];
    if let Some(c) = side_rail(app, ctx, lay) {
        cmds.push(c);
    }
    let keys = (app.fold_keys(fold::CMD_AI_WORK), app.fold_keys(fold::CMD_AI_PREVIEW));
    {
        let sh = &mut app.sh;
        let (th, cat, icons) = (&sh.theme, &sh.cat, &mut sh.icons);
        let cx = Cx {
            th,
            cat,
            ed: &app.ed,
            view3d,
            palette_open,
        };
        if lay.work.is_positive() {
            let mut ui = layer_ui(ctx, "ddc.ai.modes.work", lay.work);
            work(&mut ui, st, &cx, icons, &facts, mode, &offer, &mut acts, &mut hot);
        }
        if lay.grip.is_positive() && grip(ctx, st, lay, th) {
            cmds.push(fold::CMD_AI_WORK);
        }
        if lay.view.is_positive() {
            let mut bar = layer_ui(ctx, "ddc.ai.modes.bar", lay.bar);
            preview_bar(&mut bar, st, &cx, &facts, offer.first_sheet, &mut acts);
            if let Some(c) = fold_buttons(&mut bar, &cx, lay, &keys) {
                cmds.push(c);
            }
        }
    }
    if let Some(keys) = hot {
        preview::set_hot(logic::hot_set(keys));
    }
    for a in acts {
        match a {
            Action::Command(id) => cmds.push(id),
            a => st.run(app, a),
        }
    }
    st.save_chat(app);
    cmds
}

/// A top-level `Ui` over `r` on the view's layer, drawn after it.
fn layer_ui(ctx: &egui::Context, id: &str, r: egui::Rect) -> Ui {
    let mut ui = Ui::new(
        ctx.clone(),
        egui::Id::new(id),
        UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(r),
    );
    ui.set_clip_rect(r);
    ui
}

#[allow(clippy::too_many_arguments)]
fn work(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    f: &Facts,
    mode: AiMode,
    offer: &Offer,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let th = cx.th;
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, th.c("bg.panel"));
    let inner = full.shrink2(egui::vec2(space::LG, space::MD));
    let mut ui = ui.new_child(UiBuilder::new().max_rect(inner));
    ui.set_clip_rect(full);
    let ui = &mut ui;
    let connected = st.active_ready();
    let pending = f.plan.as_deref().is_some_and(|p| !p.items.is_empty())
        || !f.proposals.is_empty()
        || !f.agents.is_empty();
    // without a connection and nothing to decide the panel, not the tab, asks for a model:
    // one card under the panel name, the same in every mode
    if !connected && !pending {
        chat::header(ui, st, cx, icons, acts, &cx.cat.t("cmd.ai.panel"), false);
        setup(ui, cx, acts);
        // the norm and requirement checks are judged by code: they run without a model
        if mode == AiMode::Review {
            ui.add_space(space::MD);
            review(ui, st, cx, icons, f, acts, hot);
        }
        return;
    }
    let title = cx.cat.t(mode.title_key());
    chat::header(ui, st, cx, icons, acts, &title, false);
    status(ui, st, cx, connected);
    ui.add_space(space::XS);
    match mode {
        AiMode::Chat => {
            if connected {
                super::ctx_chips::strip(ui, st, cx, hot, acts);
            }
            let ready = connected && !f.busy && !st.starting;
            let has_plan = f.plan.as_deref().is_some_and(|p| !p.items.is_empty());
            let mut row = |ui: &mut Ui, acts: &mut Vec<Action>| {
                quick_requests(ui, cx, offer, has_plan, ready, acts)
            };
            chat::dialog(ui, st, cx, icons, f, acts, hot, true, Some(&mut row));
        }
        AiMode::Build => build(ui, st, cx, icons, f, offer, pending, acts, hot),
        AiMode::Review => review(ui, st, cx, icons, f, acts, hot),
    }
}

/// The connection and what this session cost so far.
fn status(ui: &mut Ui, st: &PanelUi, cx: &Cx<'_>, connected: bool) {
    let (th, cat) = (cx.th, cx.cat);
    ui.horizontal_wrapped(|ui| {
        let (key, tone) = if !connected {
            ("ai_modes.status.off", Tone::Warning)
        } else if st.profiles.get(st.profile).is_some_and(super::connect::is_local) {
            ("ai_modes.status.local", Tone::Success)
        } else {
            ("ai_modes.status.cloud", Tone::Accent)
        };
        kit::badge(ui, th, &cat.t(key), tone);
        let used = st.usage.session.total();
        if used > 0 {
            kit::caption(ui, th, &chat::usage_line(cat, used, st.usage.cost));
        }
    });
}

/// The chat's quick requests under the cards: change the request of a waiting draft, the
/// drawing set, the files, the code checks.
fn quick_requests(
    ui: &mut Ui,
    cx: &Cx<'_>,
    offer: &Offer,
    has_plan: bool,
    ready: bool,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    ui.horizontal_wrapped(|ui| {
        if has_plan && chip(ui, th, Button::quiet(&cat.t("ai_modes.chat.edit_request")).tooltip(
            &cat.t("ai_modes.chat.edit_request_tip"),
            "",
        )) {
            acts.push(Action::EditRequest);
        }
        if !ready {
            return;
        }
        if ddc_ai::catalog::find(SET_TOOL).is_some() {
            if chip(ui, th, Button::quiet(&cat.t("ai_modes.chat.drawings"))) {
                acts.push(Action::Send(cat.t("ai_modes.chat.drawings_prompt"), Mode::Propose));
            }
        } else if let Some((id, label)) = &offer.drawings {
            if chip(ui, th, Button::quiet(label)) {
                acts.push(Action::Command(*id));
            }
        }
        for (id, label) in offer.ifc.iter() {
            if chip(ui, th, Button::quiet(label)) {
                acts.push(Action::Command(*id));
            }
        }
        let check_tip = cat.t("ai_modes.chat.check_tip");
        if chip(ui, th, Button::quiet(&cat.t("ai_modes.chat.check")).tooltip(&check_tip, "")) {
            acts.push(Action::CheckInReview);
        }
    });
}

/// One small button of a wrapping row: it starts a new line rather than cut its label.
fn chip(ui: &mut Ui, th: &ddc_ui::Theme, b: Button<'_>) -> bool {
    let b = b.small();
    // what is left of this row, not the whole width a wrapping layout reports
    let left = ui.available_size_before_wrap().x;
    if left < b.width(ui, th) && ui.cursor().min.x > ui.min_rect().left() {
        ui.end_row();
    }
    b.show(ui, th).clicked()
}

/// No connection and nothing to decide: what the assistant does and the two ways in.
fn setup(ui: &mut Ui, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let key = "ai_modes.setup";
    ui.add_space(space::LG);
    centered(ui, |ui| {
        kit::card(ui, th, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(cat.t("ai_modes.setup.title"))
                        .font(th.font(Role::Title))
                        .color(th.c("text.primary")),
                )
                .wrap(),
            );
            ui.add_space(space::XS);
            body_text(ui, th, &cat.t(&format!("{key}.lead")));
            ui.add_space(space::SM);
            for i in 1..=3 {
                ui.horizontal_top(|ui| {
                    let dot = RichText::new("\u{2022}").color(th.c("accent"));
                    ui.label(dot.font(th.font(Role::Body)));
                    body_text(ui, th, &cat.t(&format!("{key}.{i}")));
                });
            }
            ui.add_space(space::MD);
            ui.horizontal_wrapped(|ui| {
                if Button::primary(&cat.t("ai_modes.setup.connect")).show(ui, th).clicked() {
                    acts.push(Action::OpenSettings);
                }
                let mcp_label = cat.t("ai_modes.setup.mcp");
                let mcp = Button::secondary(&mcp_label);
                // a new line rather than a cut label beside a long connect button
                if ui.available_size_before_wrap().x < mcp.width(ui, th) {
                    ui.end_row();
                }
                if mcp.show(ui, th).clicked() {
                    acts.push(Action::McpServer);
                }
            });
            ui.add_space(space::SM);
            kit::caption(ui, th, &cat.t("ai_modes.setup.privacy"));
        });
    });
}

fn body_text(ui: &mut Ui, th: &ddc_ui::Theme, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(th.font(Role::Body))
                .color(th.c("text.secondary")),
        )
        .wrap(),
    );
}

/// Content at most `CARD_MAX` wide, centred in a wide column.
fn centered<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = ui.available_width().min(CARD_MAX);
    let pad = ((ui.available_width() - w) / 2.0).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(w);
            add(ui)
        })
        .inner
    })
    .inner
}

#[allow(clippy::too_many_arguments)]
fn build(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    f: &Facts,
    offer: &Offer,
    pending: bool,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let busy = f.busy || st.starting;
    let step = build_step(&st.log, pending, busy, st.modes.fresh, offer.first_sheet.is_some());
    stepper(ui, cx, step);
    ui.add_space(space::SM);
    match step {
        BuildStep::Brief if busy => chat::dialog(ui, st, cx, icons, f, acts, hot, true, None),
        BuildStep::Brief => {
            egui::ScrollArea::vertical()
                .id_salt("aiux1.build.form")
                .max_height(ui.available_height() * 0.6)
                .auto_shrink([false, true])
                .show(ui, |ui| brief_card(ui, st, cx, f, acts));
            ui.add_space(space::SM);
            chat::dialog(ui, st, cx, icons, f, acts, hot, false, None);
        }
        BuildStep::Review => {
            let remarks = f.plan.as_deref().is_some_and(|p| !p.issues.is_empty());
            let (th, cat) = (cx.th, cx.cat);
            let mut row = |ui: &mut Ui, acts: &mut Vec<Action>| {
                if remarks
                    && Button::secondary(&cat.t("ai_modes.build.fix_remarks"))
                        .small()
                        .show(ui, th)
                        .clicked()
                {
                    acts.push(Action::Send(cat.t("ai_modes.build.fix_prompt"), Mode::Propose));
                }
            };
            chat::dialog(ui, st, cx, icons, f, acts, hot, true, Some(&mut row));
        }
        BuildStep::Drawings => {
            let next: Vec<_> = offer.drawings.iter().cloned().collect();
            let keys = ["ai_modes.build.drawings.title", "ai_modes.build.drawings.hint"];
            next_card(ui, st, cx, keys, &next, acts);
            ui.add_space(space::SM);
            chat::dialog(ui, st, cx, icons, f, acts, hot, false, None);
        }
        BuildStep::Files => {
            let keys = ["ai_modes.build.files.title", "ai_modes.build.files.hint"];
            next_card(ui, st, cx, keys, &offer.files, acts);
            ui.add_space(space::SM);
            chat::dialog(ui, st, cx, icons, f, acts, hot, false, None);
        }
    }
}

/// Draft, review and Apply, drawings, files: done steps ticked, the current one in the accent.
fn stepper(ui: &mut Ui, cx: &Cx<'_>, step: BuildStep) {
    let (th, cat) = (cx.th, cx.cat);
    let at = BuildStep::ALL.iter().position(|s| *s == step).unwrap_or(0);
    ui.horizontal_wrapped(|ui| {
        for (i, s) in BuildStep::ALL.iter().enumerate() {
            let (mark, color) = if i < at {
                ("\u{2713}".to_string(), th.c("success"))
            } else if i == at {
                (format!("{}", i + 1), th.c("accent"))
            } else {
                (format!("{}", i + 1), th.c("text.disabled"))
            };
            let text_color = if i == at { th.c("text.primary") } else { th.c("text.secondary") };
            ui.label(RichText::new(mark).font(th.font(Role::BodyStrong)).color(color));
            ui.label(RichText::new(cat.t(s.key())).font(th.font(Role::Label)).color(text_color));
            if i + 1 < BuildStep::ALL.len() {
                ui.label(RichText::new("\u{2192}").color(th.c("text.disabled")));
            }
        }
    });
}

/// The brief: description, type, floors, floor height, the files to make (when the delivery
/// tool is there), examples, and the one primary that sends it.
fn brief_card(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, f: &Facts, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let id = egui::Id::new(BRIEF_ID);
    if st.modes.focus_brief {
        ui.memory_mut(|m| m.request_focus(id));
        st.modes.focus_brief = false;
    }
    let ready = st.active_ready() && !f.busy && !st.starting && st.consent.is_none();
    let mut send = false;
    let hint = RichText::new(cat.t("ai_modes.build.brief_hint")).color(th.c("text.secondary"));
    centered(ui, |ui| {
        kit::card(ui, th, |ui| {
            ui.set_width(ui.available_width());
            label(ui, th, &cat.t("ai_modes.build.brief_title"));
            ui.add_space(space::XS);
            ui.add(
                TextEdit::multiline(&mut st.modes.brief)
                    .id(id)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint)
                    .font(th.font(Role::Body)),
            );
            if ui.memory(|m| m.has_focus(id))
                && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter))
            {
                send = true;
            }
            ui.add_space(space::SM);
            form_fields(ui, st, cx);
            ui.add_space(space::SM);
            label(ui, th, &cat.t("ai_modes.build.outputs"));
            ui.horizontal_wrapped(|ui| {
                for (i, (key, _)) in OUTPUTS.iter().enumerate() {
                    widgets::checkbox(ui, th, &mut st.modes.form.outputs[i], &cat.t(key));
                }
            });
            ui.add_space(space::SM);
            kit::caption(ui, th, &cat.t("ai_modes.build.examples"));
            for key in EXAMPLES {
                let text = cat.t(key);
                if example(ui, th, &text).clicked() {
                    st.modes.brief = text;
                    ui.memory_mut(|m| m.request_focus(id));
                }
            }
            ui.add_space(space::SM);
            kit::caption(ui, th, &cat.t("ai_modes.build.safe"));
            ui.add_space(space::XS);
            let has = !st.modes.brief.trim().is_empty();
            if Button::primary(&cat.t("ai_modes.build.generate"))
                .enabled(ready && has)
                .tooltip(&cat.t("ai_modes.build.generate_tip"), "Ctrl+Enter")
                .show(ui, th)
                .clicked()
            {
                send = true;
            }
            send &= ready && has;
        });
    });
    if send {
        let spec = st.modes.form.spec_line();
        let text = cat.tf(
            "ai_modes.build.prompt",
            &[("brief", st.modes.brief.trim()), ("spec", &spec)],
        );
        st.modes.fresh = false;
        acts.push(Action::Send(text, Mode::Propose));
    }
}

fn label(ui: &mut Ui, th: &ddc_ui::Theme, text: &str) -> egui::Response {
    ui.label(
        RichText::new(text)
            .font(th.font(Role::BodyStrong))
            .color(th.c("text.primary")),
    )
}

/// Type, floors and floor height, within the generator's own limits.
fn form_fields(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>) {
    use ddc_gen::project::{limits::FLOOR_HEIGHT, slots::MAX_STOREYS};
    let (th, cat) = (cx.th, cx.cat);
    let fmt = NumberFormat::for_lang(cat.lang);
    let form = &mut st.modes.form;
    ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            kit::caption(ui, th, &cat.t("ai_modes.build.kind"));
            let names: Vec<String> = KINDS.iter().map(|k| cat.t(k)).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            kit::dropdown(ui, th, "ai_modes.build.kind", &mut form.kind, &names, NUM_W * 2.0);
        });
        ui.vertical(|ui| {
            let text = cat.t("ai_modes.build.floors");
            let lr = kit::caption(ui, th, &text);
            let mut v = f64::from(form.floors);
            NumberField::new("ai_modes.build.floors", &mut v, fmt)
                .labelled_by(lr.id, &text)
                .decimals(0)
                .step(1.0)
                .range(1.0, f64::from(MAX_STOREYS))
                .width(NUM_W)
                .show(ui, th);
            form.floors = v.round().clamp(1.0, f64::from(MAX_STOREYS)) as u32;
        });
        ui.vertical(|ui| {
            let text = cat.t("ai_modes.build.floor_h");
            let lr = kit::caption(ui, th, &text);
            let unit = cat.t("ai_modes.unit.m");
            NumberField::new("ai_modes.build.floor_h", &mut form.floor_h, fmt)
                .labelled_by(lr.id, &text)
                .decimals(2)
                .step(0.1)
                .range(FLOOR_HEIGHT.min, FLOOR_HEIGHT.max)
                .unit(&unit)
                .width(NUM_W)
                .show(ui, th);
            form.floor_h = form.floor_h.clamp(FLOOR_HEIGHT.min, FLOOR_HEIGHT.max);
        });
    });
}

/// An example brief: wrapped text on a sunken row; a click puts it into the field.
fn example(ui: &mut Ui, th: &ddc_ui::Theme, text: &str) -> egui::Response {
    let out = egui::Frame::new()
        .fill(th.c("bg.sunken"))
        .stroke(egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle")))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::symmetric(space::SM as i8, space::XS as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .font(th.font(Role::Label))
                        .color(th.c("text.primary")),
                )
                .selectable(false)
                .wrap(),
            );
        });
    let id = ui.id().with(("aiux1.example", text));
    let r = ui.interact(out.response.rect, id, egui::Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    if r.hovered() {
        ui.painter().rect_stroke(
            out.response.rect,
            th.radius("md"),
            egui::Stroke::new(stroke::HAIRLINE, th.c("accent")),
            egui::StrokeKind::Inside,
        );
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// After an Apply: the next commands under a title and a hint (`keys`) or a new brief.
fn next_card(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    keys: [&str; 2],
    cmds: &[(&'static str, String)],
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    centered(ui, |ui| {
        kit::card(ui, th, |ui| {
            ui.set_width(ui.available_width());
            label(ui, th, &cat.t(keys[0]));
            ui.add_space(space::XS);
            body_text(ui, th, &cat.t(keys[1]));
            ui.add_space(space::SM);
            ui.horizontal_wrapped(|ui| {
                for (i, (id, name)) in cmds.iter().enumerate() {
                    let b = if i == 0 { Button::primary(name) } else { Button::secondary(name) };
                    if b.show(ui, th).clicked() {
                        acts.push(Action::Command(*id));
                    }
                }
                if Button::quiet(&cat.t("ai_modes.build.new_brief")).show(ui, th).clicked() {
                    st.modes.fresh = true;
                    st.modes.focus_brief = true;
                }
            });
        });
    });
}

/// Counters, then the checks (rule sets, findings with Explain and Fix), then the answers and
/// the fix drafts; the line between them drags.
#[allow(clippy::too_many_arguments)]
fn review(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    icons: &mut IconCache,
    f: &Facts,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let th = cx.th;
    if let Some(n) = st.checks.counts() {
        counters(ui, cx, n);
        ui.add_space(space::XS);
    }
    let r = ui.available_rect_before_wrap();
    let grip = space::SM;
    let room = (r.height() - grip).max(0.0);
    let top_h = if room >= 2.0 * PART_MIN {
        (room * st.modes.review_split).clamp(PART_MIN, room - PART_MIN)
    } else {
        room / 2.0
    };
    let top = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), top_h));
    let line = egui::Rect::from_min_size(
        egui::pos2(r.left(), top.bottom()),
        egui::vec2(r.width(), grip),
    );
    let bottom = egui::Rect::from_min_max(egui::pos2(r.left(), line.bottom()), r.max);
    let mut up = ui.new_child(UiBuilder::new().max_rect(top));
    up.set_clip_rect(top.intersect(ui.clip_rect()));
    checks::tab(&mut up, st, cx, acts, hot);
    let resp = ui
        .interact(line, ui.id().with("aiux1.review.grip"), egui::Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if resp.dragged() && room > 0.0 {
        let y = resp.interact_pointer_pos().map_or(line.center().y, |p| p.y);
        st.modes.review_split = ((y - r.top()) / room).clamp(0.1, 0.9);
    }
    let c = if resp.hovered() || resp.dragged() { th.c("accent") } else { th.c("border.subtle") };
    ui.painter().hline(line.x_range(), line.center().y, egui::Stroke::new(stroke::HAIRLINE, c));
    let mut down = ui.new_child(UiBuilder::new().max_rect(bottom));
    down.set_clip_rect(bottom.intersect(ui.clip_rect()));
    kit::caption(&mut down, th, &cx.cat.t("ai_modes.review.ask"));
    chat::dialog(&mut down, st, cx, icons, f, acts, hot, true, None);
    ui.advance_cursor_after_rect(r);
}

/// Errors, warnings and notes of the last model check as three figures.
fn counters(ui: &mut Ui, cx: &Cx<'_>, n: [usize; 3]) {
    let (th, cat) = (cx.th, cx.cat);
    let tiles = [
        (n[0], "ai_modes.review.errors", "error"),
        (n[1], "ai_modes.review.warnings", "warning"),
        (n[2], "ai_modes.review.notes", "text.secondary"),
    ];
    ui.horizontal_wrapped(|ui| {
        for (count, key, token) in tiles {
            ui.label(
                RichText::new(count.to_string())
                    .font(th.font(Role::Title))
                    .color(th.c(token)),
            );
            ui.label(
                RichText::new(cat.t(key))
                    .font(th.font(Role::Label))
                    .color(th.c("text.secondary")),
            );
            ui.add_space(space::MD);
        }
    });
}

/// The line between the work column and the preview; dragging it sets the split, a
/// double-click folds the work column (true).
fn grip(ctx: &egui::Context, st: &mut PanelUi, lay: &Layout, th: &ddc_ui::Theme) -> bool {
    let ui = layer_ui(ctx, "ddc.ai.modes.grip", lay.grip);
    let resp = ui
        .interact(lay.grip, ui.id().with("grip"), egui::Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    let total = lay.view.right() - lay.work.left();
    if resp.dragged() && total > 0.0 {
        let x = resp.interact_pointer_pos().map_or(lay.grip.center().x, |p| p.x);
        st.modes.split = ((x - lay.work.left()) / total).clamp(0.2, 0.8);
    }
    let on = resp.hovered() || resp.dragged();
    let c = if on { th.c("accent") } else { th.c("border.subtle") };
    let w = if on { stroke::INDICATOR } else { stroke::HAIRLINE };
    ui.painter().rect_filled(lay.grip, 0.0, th.c("bg.panel"));
    ui.painter().vline(lay.grip.center().x, lay.grip.y_range(), egui::Stroke::new(w, c));
    resp.double_clicked()
}

/// The rail of the folded side, if any; a click or a rest on it asks for the fold command
/// that brings the side back.
fn side_rail(app: &mut EditorApp, ctx: &egui::Context, lay: &Layout) -> Option<&'static str> {
    if !lay.rail.is_positive() {
        return None;
    }
    let work = lay.folded == AiFold::Work;
    let (icon, title, tip, cmd) = if work {
        (ddc_ui::icons::ctl::AI, "layout_fold.ai.work", "layout_fold.ai.work.show", fold::CMD_AI_WORK)
    } else {
        ("plan-view", "layout_fold.ai.preview", "layout_fold.ai.preview.show", fold::CMD_AI_PREVIEW)
    };
    // too narrow for both: the preview comes back in place of the work column
    let narrow = !work && app.prefs.fold.ai != AiFold::Preview;
    let cmd = if narrow { fold::CMD_AI_WORK } else { cmd };
    let keys = app.fold_keys(cmd);
    let (title, tip) = (app.sh.cat.t(title), app.sh.cat.t(tip));
    let edge = if work { fold::Edge::Left } else { fold::Edge::Right };
    let mut ui = layer_ui(ctx, "ddc.ai.modes.rail", lay.rail);
    let text = fold::RailText {
        icon,
        title: &title,
        tip: &tip,
        keys: &keys,
    };
    let sh = &mut app.sh;
    let hold = &mut app.chrome.fold.hold_ai;
    let id = egui::Id::new("ddc.ai.modes.rail");
    let out = fold::rail(&mut ui, lay.rail, id, &sh.theme, &mut sh.icons, &text, edge, hold);
    (out.clicked || out.held).then_some(cmd)
}

/// Two chevrons at the right of the preview bar: fold the work column, fold the preview.
fn fold_buttons(
    ui: &mut Ui,
    cx: &Cx<'_>,
    lay: &Layout,
    (work_keys, preview_keys): &(String, String),
) -> Option<&'static str> {
    let (th, cat) = (cx.th, cx.cat);
    let r = lay.bar.shrink2(egui::vec2(space::SM, 0.0));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(r)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let tip = cat.t("layout_fold.ai.preview.hide");
    if fold::fold_button(&mut ui, th, false, &tip, preview_keys).clicked() {
        return Some(fold::CMD_AI_PREVIEW);
    }
    if lay.work.is_positive() {
        let tip = cat.t("layout_fold.ai.work.hide");
        if fold::fold_button(&mut ui, th, true, &tip, work_keys).clicked() {
            return Some(fold::CMD_AI_WORK);
        }
    }
    None
}

/// "Draft preview" or "Model", the Plan / 3D / Sheet switch, and whether the model is untouched.
fn preview_bar(
    ui: &mut Ui,
    st: &PanelUi,
    cx: &Cx<'_>,
    f: &Facts,
    first_sheet: Option<i64>,
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let r = ui.max_rect();
    ui.painter().rect_filled(r, 0.0, th.c("bg.panel"));
    ui.painter().hline(
        r.x_range(),
        r.bottom() - stroke::HAIRLINE / 2.0,
        egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
    );
    let inner = r.shrink2(egui::vec2(space::MD, space::XS));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let draft = preview::rev() != 0 && f.plan.is_some();
    let title = if draft { "ai_modes.preview.draft" } else { "ai_modes.preview.model" };
    kit::caption(&mut ui, th, &cat.t(title));
    ui.add_space(space::SM);
    let plan = cat.t("ai_modes.preview.plan");
    let d3 = cat.t("ai_modes.preview.3d");
    let sheet = cat.t("ai_modes.preview.sheet");
    let mut items = vec![plan.as_str(), d3.as_str()];
    let mut kinds = vec![Preview::Plan, Preview::D3];
    if first_sheet.is_some() {
        items.push(sheet.as_str());
        kinds.push(Preview::Sheet);
    }
    let sel = kinds.iter().position(|k| *k == st.modes.preview).unwrap_or(0);
    if let Some(i) = widgets::segmented(&mut ui, th, &items, sel) {
        if kinds[i] != st.modes.preview {
            acts.push(Action::Preview(kinds[i], first_sheet));
        }
    }
    if draft {
        ui.add_space(space::SM);
        kit::badge(&mut ui, th, &cat.t("ai_modes.preview.untouched"), Tone::Accent);
    }
}
