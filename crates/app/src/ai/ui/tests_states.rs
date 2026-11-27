// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every state of the assistant panel drawn headless at the narrowest panel width, in both
//! languages and both themes, with an explicit catalog (the global language is never touched).
//! A frame may not write the model, may not emit an action on its own, shows at most one primary
//! button, and draws no text beyond the panel edge or its clip.

use super::kit::probe;
use super::logic::{self, ErrKind, PanelState};
use super::{chat, receipt, steps, Action, Cx, Entry, Facts, PanelUi, PANEL_W};
use crate::ai::exec::ModelStep;
use crate::ai::plan::{CompiledPlan, ElemDiff, PlanItem, PlanOp, PlanView, Targets};
use crate::ai::session::{ProposalCard, ProposalState};
use crate::ai::validate::{IssueLevel, PlanIssue, Target};
use crate::ai::AiError;
use crate::editor::Editor;
use ddc_ai::{AuthKind, Profile, Protocol, ToolCalling};
use ddc_ui::icons::IconCache;
use ddc_ui::theme::space;
use std::sync::Arc;

/// The test host of the AI panel and its cards: a fixed-size window under the dialog id (the
/// panel is a dock in the product). Every AI test draws through this one host.
pub(crate) fn host(ctx: &egui::Context, id: &str, size: egui::Vec2, add: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(id.to_owned())
        .id(egui::Id::new(("ddc.dialog", id)))
        .title_bar(false)
        .fixed_size(size)
        .show(ctx, add);
}

const EPS: f32 = 0.5;
/// Longer than the panel without a break: names and paths look like this.
const LONG_WORD: &str = "Level_03_Architecture_Walls_Exterior_Concrete_Insulated_Type_B_300";

struct Case {
    name: &'static str,
    st: PanelUi,
    f: Facts,
    want: PanelState,
}

struct Out {
    /// Text, its rectangle, the clip it was drawn under.
    texts: Vec<(String, egui::Rect, egui::Rect)>,
    buttons: Vec<probe::Seen>,
    field: Option<egui::Rect>,
    acts: Vec<Action>,
    head_before: u64,
    head_after: u64,
}

fn profile() -> Profile {
    Profile {
        id: "p1".to_string(),
        name: "Local model with a long profile name 70B instruct q4".to_string(),
        protocol: Protocol::ChatCompletions,
        base_url: "http://127.0.0.1:1/v1".to_string(),
        model: "test-model".to_string(),
        auth: AuthKind::None,
        extra_headers: vec![],
        tool_calling: ToolCalling::Native,
        images: false,
        context_tokens: 8000,
        max_output_tokens: 1000,
        price_in_per_mtok: None,
        price_out_per_mtok: None,
        executor: None,
    }
}

fn base(connected: bool) -> PanelUi {
    let mut st = PanelUi::default();
    st.propose = true;
    st.preview_on = true;
    st.use_selection = true;
    if connected {
        st.profiles = vec![profile()];
    }
    st
}

fn item(id: u32, title: &str) -> PlanItem {
    PlanItem {
        id,
        title: title.to_string(),
        why: "Because the walls are listed with the wrong type".to_string(),
        op: PlanOp::Delete {
            target: Targets::Selection,
        },
        checked: true,
        depends_on: vec![],
        actor: "person".to_string(),
        conflict: None,
        sets: Default::default(),
    }
}

fn plan(ed: &Editor, n: usize, stale: bool, blocked: bool) -> Arc<PlanView> {
    let items: Vec<PlanItem> = (0..n)
        .map(|i| item(i as u32 + 1, &format!("Change {i} {LONG_WORD}")))
        .collect();
    let diff = (0..n)
        .map(|i| ElemDiff {
            key: ("walls".to_string(), i as i64 + 1),
            kind: ddc_model::RowKind::Changed,
            item: Some(i as u32 + 1),
            level: None,
            label: format!("Wall {LONG_WORD}"),
            cells: vec![],
        })
        .collect();
    let issues = if blocked {
        vec![PlanIssue {
            level: IssueLevel::Block,
            item: Some(1),
            el: None,
            text_key: "ai.issue.integrity",
            args: vec![],
        }]
    } else {
        vec![]
    };
    Arc::new(PlanView {
        items,
        diff,
        secondary: 0,
        qty: vec![],
        issues,
        main_issues: vec![],
        preview: Arc::new(crate::ai::preview::PreviewSet::default()),
        compiled: CompiledPlan {
            base_head: ed.model.change_head(),
            steps: Arc::new(vec![ModelStep::Edits(vec![])]),
        },
        stale,
        variants: vec![],
        variant: 0,
        truncated: false,
        flagged: 0,
        ack: 0,
        gated: Target::Draft,
        allow_large: false,
        unreviewed: vec![],
        expires_in: None,
        deliver: None,
        self_check: None,
    })
}

fn facts(plan: Arc<PlanView>, can_apply: bool, stale: bool) -> Facts {
    Facts {
        plan: Some(plan),
        has_session: true,
        can_apply,
        stale,
        plan_rev: 1,
        ..Facts::default()
    }
}

/// One waiting agent proposal, as the session reports it even with no chat running.
fn proposal_facts(ed: &Editor) -> Facts {
    Facts {
        proposals: vec![ProposalCard {
            id: 7,
            actor: "mcp:agent".into(),
            title: "Tidy walls".into(),
            ops: 3,
            diff: serde_json::json!({"changed": ["wall:1", "wall:2", "wall:3"]}),
            state: ProposalState::Waiting,
            base_head: ed.model.change_head(),
        }],
        ..Facts::default()
    }
}

fn busy() -> Facts {
    Facts {
        has_session: true,
        busy: true,
        ..Facts::default()
    }
}

fn talk(st: &mut PanelUi) {
    st.log.push(Entry::User(format!("Move {LONG_WORD} to level 2")));
    st.log.push(Entry::Assistant(format!(
        "Found 12 walls.\n\n- {LONG_WORD}\n- second item that is long enough to wrap on a narrow panel\n\n`{LONG_WORD}`"
    )));
    let mut ok = steps::Step::start("t", 10);
    ok.finish(true, None, 20);
    st.log.push(Entry::Tool(ok));
    let mut bad = steps::Step::start("t", 30);
    bad.finish(false, Some(LONG_WORD), 40);
    st.log.push(Entry::Tool(bad));
    st.log.push(Entry::Note("ai.plan.applied"));
    st.log.push(Entry::Applied(receipt::Receipt {
        op_seq: 12,
        stamp: 77,
        n: 5,
        cats: vec![("wall".to_string(), 4), ("opening".to_string(), 1)],
        ids: vec![("wall".to_string(), 1), ("opening".to_string(), 2)],
        undone: false,
        drafts: vec![],
    }));
}

/// The chat after an apply that wrote a draft family: the receipt offers "Make working".
pub(crate) fn layout_probe_draft_receipt(lang: ddc_ui::Lang) -> crate::layout_contract::Probe {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let mut st = base(true);
    st.profiles[0].name = MY_MODEL.to_string();
    st.log.push(Entry::Applied(receipt::Receipt {
        op_seq: 12,
        stamp: 77,
        n: 3,
        cats: vec![("comp".to_string(), 3)],
        ids: vec![("comp".to_string(), 1)],
        undone: false,
        drafts: vec!["RuralToilet".to_string()],
    }));
    let mut icons = IconCache::default();
    let mut hot = None;
    let f = Facts::default();
    crate::layout_contract::probe("ai.panel", |ctx| {
        let cx = Cx { th: &th, cat: &cat, ed: &ed, view3d: false, palette_open: false };
        let mut acts: Vec<Action> = vec![];
        host(ctx, "ai.panel", egui::vec2(PANEL_W.0, 600.0), |ui| {
            chat::panel(ui, &mut st, &cx, &mut icons, &f, &mut acts, &mut hot);
        });
    })
}

fn cases(ed: &Editor) -> Vec<Case> {
    let mut v: Vec<Case> = vec![];
    let mut add = |name, st, f, want| v.push(Case { name, st, f, want });

    add("no_profile", base(false), Facts::default(), PanelState::NoProfile);
    let mut st = base(false);
    talk(&mut st);
    add("no_profile_with_log", st, Facts::default(), PanelState::NoProfile);

    add("idle", base(true), Facts::default(), PanelState::Idle);
    let mut st = base(true);
    talk(&mut st);
    add("idle_with_log", st, Facts::default(), PanelState::Idle);

    let mut st = base(true);
    st.local_wait = true;
    add("local_only_blocked", st, Facts::default(), PanelState::LocalOnlyBlocked);

    let mut st = base(true);
    st.consent = Some(format!("{LONG_WORD}.example.test"));
    add("need_consent", st, Facts::default(), PanelState::NeedConsent);

    let mut st = base(true);
    st.starting = true;
    add("preparing", st, Facts::default(), PanelState::Preparing);
    let mut st = base(true);
    st.starting = true;
    st.prepared = Some((3, 10));
    add("preparing_progress", st, Facts::default(), PanelState::Preparing);

    let mut st = base(true);
    talk(&mut st);
    add("streaming", st, busy(), PanelState::Streaming);
    let mut st = base(true);
    st.tool_running = true;
    add("tool", st, busy(), PanelState::Tool);

    let errors = [
        ("err_auth", AiError::Provider("authentication failed".to_string())),
        ("err_rate", AiError::Provider("rate limited".to_string())),
        (
            "err_context",
            AiError::Provider("request exceeds the model context".to_string()),
        ),
        (
            "err_no_tools",
            AiError::Provider("model does not support tool calling".to_string()),
        ),
        (
            "err_network",
            AiError::Provider(format!("network error: {LONG_WORD} refused")),
        ),
        ("err_model", AiError::Model(format!("wall 5 is locked {LONG_WORD}"))),
        ("err_internal", AiError::Internal(LONG_WORD.to_string())),
        ("err_stale", AiError::Stale),
        ("err_blocked", AiError::Blocked),
        ("err_diverged", AiError::Diverged(format!("wall 5 {LONG_WORD}"))),
    ];
    for (name, e) in errors {
        let mut st = base(true);
        st.error = Some(e);
        add(name, st, Facts::default(), PanelState::Error);
    }
    let mut st = base(true);
    st.error = Some(AiError::Provider("rate limited".to_string()));
    st.retry.step(Some(ErrKind::RateLimit), Some(30), false, 0.0);
    add("err_rate_countdown", st, Facts::default(), PanelState::Error);
    for what in ["tokens", "cost", "wall", "output", "filtered", "steps"] {
        let mut st = base(true);
        st.limit = Some(what);
        add("turn_limit", st, Facts::default(), PanelState::Error);
    }

    let mut st = base(true);
    talk(&mut st);
    add("plan_ready", st, facts(plan(ed, 3, false, false), true, false), PanelState::PlanReady);
    add(
        "plan_many",
        base(true),
        facts(plan(ed, 30, false, false), true, false),
        PanelState::PlanReady,
    );
    add(
        "plan_blocked",
        base(true),
        facts(plan(ed, 2, false, true), false, false),
        PanelState::PlanReady,
    );
    add(
        "plan_recomputing",
        base(true),
        facts(plan(ed, 2, false, false), false, false),
        PanelState::Recomputing,
    );
    add(
        "plan_stale",
        base(true),
        facts(plan(ed, 2, true, false), false, true),
        PanelState::Stale,
    );
    let mut st = base(true);
    st.error = Some(AiError::Stale);
    add(
        "plan_with_error",
        st,
        facts(plan(ed, 2, false, false), true, false),
        PanelState::Error,
    );

    for (name, tab) in [("tab_checks", 1), ("tab_options", 2)] {
        let mut st = base(true);
        st.tab = tab;
        add(name, st, Facts::default(), PanelState::Idle);
    }
    let mut st = base(false);
    st.tab = 1;
    add("tab_checks_no_profile", st, Facts::default(), PanelState::NoProfile);

    add(
        "no_profile_plan",
        base(false),
        facts(plan(ed, 3, false, false), true, false),
        PanelState::NoProfile,
    );
    add(
        "no_profile_plan_many",
        base(false),
        facts(plan(ed, 30, false, false), true, false),
        PanelState::NoProfile,
    );
    add(
        "no_profile_plan_stale",
        base(false),
        facts(plan(ed, 2, true, false), false, true),
        PanelState::NoProfile,
    );
    let mut st = base(false);
    talk(&mut st);
    add(
        "no_profile_plan_with_log",
        st,
        facts(plan(ed, 3, false, false), true, false),
        PanelState::NoProfile,
    );
    add(
        "no_profile_proposal",
        base(false),
        proposal_facts(ed),
        PanelState::NoProfile,
    );
    v
}

fn collect(s: &egui::Shape, clip: egui::Rect, out: &mut Vec<(String, egui::Rect, egui::Rect)>) {
    match s {
        egui::Shape::Text(t) => {
            let text = t.galley.text().to_string();
            if !text.trim().is_empty() {
                out.push((text, t.galley.rect.translate(t.pos.to_vec2()), clip));
            }
        }
        egui::Shape::Vec(v) => v.iter().for_each(|x| collect(x, clip, out)),
        _ => {}
    }
}

/// Three frames so the layout settles; the last one is measured.
fn render(
    ed: &Editor,
    st: &mut PanelUi,
    f: &Facts,
    lang: ddc_ui::Lang,
    mode: ddc_ui::Mode,
) -> Out {
    render_at(ed, st, f, lang, mode, egui::vec2(PANEL_W.0, 900.0))
}

fn render_at(
    ed: &Editor,
    st: &mut PanelUi,
    f: &Facts,
    lang: ddc_ui::Lang,
    mode: ddc_ui::Mode,
    size: egui::Vec2,
) -> Out {
    let mut th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
    let cat = ddc_ui::Catalog::new(lang);
    // named font weights bind from the pass after install: a bare first frame would panic
    let ctx = egui::Context::default();
    ddc_ui::theme::install_fonts(&ctx);
    let _ = ctx.run(Default::default(), |_| {});
    let mut icons = IconCache::default();
    let mut hot = None;
    let mut acts: Vec<Action> = vec![];
    let head_before = ed.model.change_head();
    let mut texts = vec![];
    let mut buttons = vec![];
    let mut field = None;
    for _ in 0..3 {
        acts.clear();
        let _ = probe::take();
        let _ = probe::take_field();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..egui::RawInput::default()
        };
        let out = ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            let cx = Cx {
                th: &th,
                cat: &cat,
                ed,
                view3d: false,
                palette_open: false,
            };
            egui::CentralPanel::default()
                .frame(egui::Frame::new())
                .show(ctx, |ui| {
                    chat::panel(ui, st, &cx, &mut icons, f, &mut acts, &mut hot);
                });
        });
        texts.clear();
        for c in &out.shapes {
            collect(&c.shape, c.clip_rect, &mut texts);
        }
        buttons = probe::take();
        field = probe::take_field();
    }
    Out {
        texts,
        buttons,
        field,
        acts,
        head_before,
        head_after: ed.model.change_head(),
    }
}

fn looks_like_key(t: &str) -> bool {
    (t.starts_with("ai.") || t.starts_with("cmd."))
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
}

fn check(tag: &str, o: &Out) {
    assert!(!o.texts.is_empty(), "{tag}: nothing drawn");
    assert_eq!(o.head_before, o.head_after, "{tag}: the model changed without Apply");
    assert!(o.acts.is_empty(), "{tag}: a frame without input emitted an action");
    let primaries: Vec<_> = o.buttons.iter().filter(|b| b.primary).collect();
    assert!(primaries.len() <= 1, "{tag}: primaries {primaries:?}");
    assert!(
        primaries.iter().filter(|b| b.enabled).count() <= 1,
        "{tag}: enabled primaries {primaries:?}"
    );
    for b in &o.buttons {
        assert!(b.right <= PANEL_W.0 + EPS, "{tag}: button {b:?} past the panel edge");
    }
    for (text, r, clip) in &o.texts {
        assert!(!looks_like_key(text), "{tag}: missing translation {text:?}");
        assert!(
            r.left() >= -EPS && r.right() <= PANEL_W.0 + EPS,
            "{tag}: text {text:?} spans {}..{} of a {} px panel",
            r.left(),
            r.right(),
            PANEL_W.0
        );
        if r.intersects(*clip) {
            assert!(
                r.left() >= clip.left() - EPS && r.right() <= clip.right() + EPS,
                "{tag}: text {text:?} is cut by its clip"
            );
        }
    }
}

fn count_text(o: &Out, text: &str) -> usize {
    o.texts.iter().filter(|(t, _, _)| t == text).count()
}

fn run_lang(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        for mut c in cases(&ed) {
            let tag = format!("{} {lang:?} {mode:?}", c.name);
            let got = chat::state_of(&c.st, &c.f);
            assert!(
                got == c.want,
                "{tag}: fixture reached {} instead of {}",
                logic::state_key(got),
                logic::state_key(c.want)
            );
            let o = render(&ed, &mut c.st, &c.f, lang, mode);
            check(&tag, &o);
            let cat = ddc_ui::Catalog::new(lang);
            let send = cat.t("ai.common.send");
            let stop = cat.t("ai.common.stop");
            let on_dialog = c.st.tab == 0;
            match c.want {
                PanelState::Streaming | PanelState::Tool if on_dialog => {
                    assert!(o.buttons.iter().all(|b| !b.primary), "{tag}: primary while busy");
                    assert!(o.buttons.iter().any(|b| b.label == stop), "{tag}: no Stop");
                }
                PanelState::PlanReady | PanelState::Recomputing | PanelState::Stale
                    if on_dialog && c.f.plan.is_some() =>
                {
                    // the plan owns the primary: Send steps back
                    assert!(
                        o.buttons.iter().filter(|b| b.primary).count() == 1,
                        "{tag}: the plan card has one primary"
                    );
                    assert!(
                        o.buttons.iter().any(|b| b.label == send && !b.primary),
                        "{tag}: Send stays quiet next to a plan"
                    );
                    let armed = o.buttons.iter().any(|b| b.primary && b.enabled);
                    assert_eq!(armed, c.f.can_apply, "{tag}: Apply enabled only when it can run");
                }
                PanelState::Idle if on_dialog && c.f.plan.is_none() => {
                    assert!(
                        o.buttons.iter().any(|b| b.label == send && b.primary),
                        "{tag}: Send is the primary of an idle panel"
                    );
                }
                _ => {}
            }
            if c.name == "no_profile" {
                let title = cat.t("ai.state.no_profile");
                let action = cat.t("ai.state.no_profile.action");
                assert_eq!(count_text(&o, &title), 1, "{tag}: the missing connection is named once");
                assert_eq!(count_text(&o, &action), 1, "{tag}: one connect action");
                assert_eq!(
                    o.buttons.iter().filter(|b| b.label == action).count(),
                    1,
                    "{tag}: no second connect button"
                );
            }
        }
    }
}

/// Not connected: the card, quiet examples and a disabled composer form one group; no void.
fn not_connected_group(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let action = cat.t("ai.state.no_profile.action");
    let off_hint = cat.t("ai.composer.hint_off");
    let send = cat.t("ai.common.send");
    let keys_hint = cat.t("ai.composer.keys_hint");
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        for w in [PANEL_W.0, PANEL_W.1] {
            let tag = format!("not_connected {lang:?} {mode:?} {w}px");
            let mut st = base(false);
            let o = render_at(
                &ed,
                &mut st,
                &Facts::default(),
                lang,
                mode,
                egui::vec2(w, 640.0),
            );
            let th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
            assert!(o.acts.is_empty(), "{tag}: action without input");
            let primaries: Vec<_> = o.buttons.iter().filter(|b| b.primary).collect();
            assert_eq!(primaries.len(), 1, "{tag}: one primary {primaries:?}");
            assert_eq!(primaries[0].label, action, "{tag}: the primary connects");
            assert!(primaries[0].enabled, "{tag}: connect is enabled");
            assert!(o.buttons.iter().all(|b| b.label != send), "{tag}: no Send while disconnected");
            assert_eq!(count_text(&o, &off_hint), 0, "{tag}: no dead composer under the card");
            assert!(o.field.is_none(), "{tag}: an input is drawn while disconnected");
            assert_eq!(count_text(&o, &keys_hint), 0, "{tag}: no typing hint on a dead field");
            let title = cat.t("ai.state.no_profile");
            assert_eq!(count_text(&o, &title), 1, "{tag}: the missing connection is named once");
            assert_eq!(count_text(&o, &action), 1, "{tag}: one connect action");
            for (text, r, clip) in &o.texts {
                assert!(!looks_like_key(text), "{tag}: missing translation {text:?}");
                assert!(
                    r.left() >= -EPS && r.right() <= w + EPS,
                    "{tag}: text {text:?} spans {}..{} of {w}",
                    r.left(),
                    r.right()
                );
                if r.intersects(*clip) {
                    assert!(
                        r.left() >= clip.left() - EPS && r.right() <= clip.right() + EPS,
                        "{tag}: text {text:?} is cut by its clip"
                    );
                }
            }
            let mut last_example = f32::MIN;
            for key in chat::CONNECT_EXAMPLES {
                let t = cat.t(key);
                let hit = o.texts.iter().find(|(x, _, _)| *x == t);
                let (_, r, _) = hit.unwrap_or_else(|| panic!("{tag}: example {key} not drawn"));
                last_example = last_example.max(r.bottom());
            }
            let first = o
                .texts
                .iter()
                .find(|(x, _, _)| *x == cat.t("ai.state.no_profile"))
                .map(|(_, r, _)| r.top())
                .unwrap();
            // the group reads from under the panel title: some air above the card, never a void
            let head = o.texts.iter().filter(|(_, r, _)| r.bottom() < first).map(|(_, r, _)| r.bottom());
            let above = first - head.fold(0.0, f32::max);
            assert!(above >= space::MD - EPS, "{tag}: group touches the top ({above})");
            assert!(
                above <= chat::CONNECT_TOP_MAX + 2.0 * th.d().row,
                "{tag}: {above} px of void above the card"
            );
            assert!(last_example <= 640.0, "{tag}: examples below the panel");
        }
    }
}

/// A tall panel (the 1000 px window) keeps the card near the top in both widths: the room
/// above it does not grow with the panel height.
fn not_connected_tall_panel(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let title = cat.t("ai.state.no_profile");
    for w in [PANEL_W.0, 360.0] {
        let mut tops = vec![];
        for h in [640.0, 900.0] {
            let mut st = base(false);
            let o = render_at(
                &ed,
                &mut st,
                &Facts::default(),
                lang,
                ddc_ui::Mode::Light,
                egui::vec2(w, h),
            );
            let top = o.texts.iter().find(|(x, _, _)| *x == title).map(|(_, r, _)| r.top());
            tops.push(top.unwrap_or_else(|| panic!("{lang:?} {w}x{h}: no card")));
        }
        assert!((tops[0] - tops[1]).abs() <= 1.0, "{lang:?} {w}: card moves down {tops:?}");
    }
}

#[test]
fn not_connected_tall_panel_en() {
    not_connected_tall_panel(ddc_ui::Lang::En);
}

#[test]
fn not_connected_tall_panel_ru() {
    not_connected_tall_panel(ddc_ui::Lang::Ru);
}

/// Connected and idle: Send sits right under the input, the key hint under both, and
/// the input grows with the text up to the token maximum, then scrolls.
fn connected_composer(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let send = cat.t("ai.common.send");
    let keys_hint = cat.t("ai.composer.keys_hint");
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        let th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
        for w in [PANEL_W.0, PANEL_W.1] {
            let mut heights = vec![];
            for lines in [0usize, 4, 40] {
                let tag = format!("connected {lang:?} {mode:?} {w}px {lines} lines");
                let mut st = base(true);
                st.input = vec!["line"; lines].join("\n");
                let before = st.input.clone();
                let o = render_at(&ed, &mut st, &Facts::default(), lang, mode, egui::vec2(w, 900.0));
                assert!(o.acts.is_empty(), "{tag}: an idle frame sent something");
                assert_eq!(o.head_before, o.head_after, "{tag}: the model changed");
                assert_eq!(st.input, before, "{tag}: the idle frame rewrote the input");
                let field = o.field.unwrap_or_else(|| panic!("{tag}: no input drawn"));
                let btn = o
                    .buttons
                    .iter()
                    .find(|b| b.label == send)
                    .unwrap_or_else(|| panic!("{tag}: no Send"));
                let gap = btn.top - field.bottom();
                assert!(gap >= -EPS, "{tag}: Send overlaps the input ({gap})");
                assert!(gap <= space::MD, "{tag}: Send floats {gap} px under the input");
                assert!(btn.right <= w + EPS, "{tag}: Send past the panel edge");
                let (_, hint, _) = o
                    .texts
                    .iter()
                    .find(|(t, _, _)| *t == keys_hint)
                    .unwrap_or_else(|| panic!("{tag}: no key hint"));
                assert!(hint.top() >= btn.bottom - EPS, "{tag}: hint not under Send");
                assert!(hint.top() - btn.bottom <= space::LG, "{tag}: hint floats under the composer");
                assert!(hint.right() <= w + EPS, "{tag}: hint past the panel edge");
                heights.push(field.height());
                assert!(
                    field.height() <= chat::input_max_height(&th) + EPS,
                    "{tag}: input {} px over the maximum",
                    field.height()
                );
            }
            assert!(heights[1] > heights[0] + EPS, "{lang:?} {mode:?} {w}px: the input does not grow");
            assert!(
                (heights[2] - chat::input_max_height(&th)).abs() <= EPS,
                "{lang:?} {mode:?} {w}px: long text is not capped at the maximum ({})",
                heights[2]
            );
        }
    }
}

const MY_MODEL: &str = "My model";

/// The panel at its narrowest width in a fixed window, for the layout contract.
pub(crate) fn layout_probe(lang: ddc_ui::Lang, connected: bool) -> crate::layout_contract::Probe {
    let shape = Shape { items: 0, log: false, size: egui::vec2(PANEL_W.0, 600.0) };
    layout_probe_shape(lang, connected, shape)
}

/// What the probed panel holds: a draft of `items` changes, a long conversation, a window size.
#[derive(Clone, Copy)]
pub(crate) struct Shape {
    pub items: usize,
    pub log: bool,
    pub size: egui::Vec2,
}

/// The panel connected with `shape`; with a draft the composer and Apply bar are pinned.
pub(crate) fn layout_probe_shape(
    lang: ddc_ui::Lang,
    connected: bool,
    shape: Shape,
) -> crate::layout_contract::Probe {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let mut st = base(connected);
    if connected {
        st.profiles[0].name = MY_MODEL.to_string();
    }
    if shape.log {
        for _ in 0..40 {
            talk(&mut st);
        }
    }
    let mut icons = IconCache::default();
    let mut hot = None;
    let f = if shape.items > 0 {
        facts(plan(&ed, shape.items, false, false), true, false)
    } else {
        Facts::default()
    };
    crate::layout_contract::probe("ai.panel", |ctx| {
        let cx = Cx {
            th: &th,
            cat: &cat,
            ed: &ed,
            view3d: false,
            palette_open: false,
        };
        let mut acts: Vec<Action> = vec![];
        host(ctx, "ai.panel", shape.size, |ui| {
            chat::panel(ui, &mut st, &cx, &mut icons, &f, &mut acts, &mut hot);
        });
    })
}

fn has_text(o: &Out, text: &str) -> bool {
    o.texts.iter().any(|(t, _, _)| t.contains(text))
}

/// The title is always there; chips and tabs only with a connection; the connection name is the
/// one the person set.
fn title_strip_and_name(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let title = cat.t("ai.panel.title");
    let tabs = [cat.t("ai.tab.dialog"), cat.t("ai.tab.checks"), cat.t("ai.tab.options")];
    let view = cat.t("ai.ctx.view_plan");
    let connect = cat.t("ai.state.no_profile.action");
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        // not connected, also when the person last stood on another tab
        for tab in [0usize, 1, 2] {
            let tag = format!("no_profile tab {tab} {lang:?} {mode:?}");
            let mut st = base(false);
            st.tab = tab;
            let o = render(&ed, &mut st, &Facts::default(), lang, mode);
            check(&tag, &o);
            assert_eq!(count_text(&o, &title), 1, "{tag}: title");
            for t in &tabs {
                assert!(!has_text(&o, t), "{tag}: tab {t} while disconnected");
            }
            assert!(!has_text(&o, &view), "{tag}: context chip while disconnected");
            assert_eq!(o.buttons.iter().filter(|b| b.primary).count(), 1, "{tag}");
            assert!(o.buttons.iter().any(|b| b.primary && b.enabled && b.label == connect), "{tag}");
            assert_eq!(count_text(&o, &cat.t("ai.composer.hint_off")), 0, "{tag}: no composer");
            assert_eq!(count_text(&o, &cat.t("ai.state.no_profile")), 1, "{tag}: said once");
            assert!(o.buttons.iter().all(|b| b.label != cat.t("ai.common.send")), "{tag}");
            assert_eq!(st.tab, 0, "{tag}: the dialog is the only tab");
        }
        // connecting, error, idle: title, tabs and chips; the name the person set
        let mut idle = base(true);
        idle.profiles[0].name = MY_MODEL.to_string();
        let mut preparing = base(true);
        preparing.profiles[0].name = MY_MODEL.to_string();
        preparing.starting = true;
        let mut failed = base(true);
        failed.profiles[0].name = MY_MODEL.to_string();
        failed.error = Some(AiError::Provider("network error: refused".to_string()));
        for (name, mut st) in [("idle", idle), ("preparing", preparing), ("error", failed)] {
            let tag = format!("{name} {lang:?} {mode:?}");
            let o = render(&ed, &mut st, &Facts::default(), lang, mode);
            check(&tag, &o);
            assert_eq!(count_text(&o, &title), 1, "{tag}: title");
            for t in &tabs {
                assert!(has_text(&o, t), "{tag}: tab {t}");
            }
            assert!(has_text(&o, &view), "{tag}: context chip");
            assert!(has_text(&o, MY_MODEL), "{tag}: the connection name");
        }
    }
}

fn profile_without_model() -> PanelUi {
    let mut st = base(true);
    st.profiles[0].model = String::new();
    st
}

/// A plan keeps Apply and Reject in every connection state, with or without a log; only the
/// composer and the status line follow the connection.
fn pending_plan_keeps_actions(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let apply = cat.plural("ai.items.apply", 3);
    let reject = cat.t("ai.plan.reject");
    let connect = cat.t("ai.state.no_profile.action");
    let send = cat.t("ai.common.send");
    let off_hint = cat.t("ai.composer.hint_off");
    let not_connected = cat.t("ai.state.no_profile");
    let states: [(&str, bool, fn() -> PanelUi); 3] = [
        ("no profile", false, || base(false)),
        ("profile without a model", false, profile_without_model),
        ("connected", true, || base(true)),
    ];
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        for (name, ready, make) in states {
            for with_log in [false, true] {
                let tag = format!("{name} log={with_log} {lang:?} {mode:?}");
                let mut st = make();
                if with_log {
                    talk(&mut st);
                }
                let f = facts(plan(&ed, 3, false, false), true, false);
                let o = render(&ed, &mut st, &f, lang, mode);
                check(&tag, &o);
                let primaries: Vec<_> = o.buttons.iter().filter(|b| b.primary).collect();
                assert_eq!(primaries.len(), 1, "{tag}: one primary {primaries:?}");
                assert_eq!(primaries[0].label, apply, "{tag}: Apply is the primary");
                assert!(primaries[0].enabled, "{tag}: Apply is armed");
                assert!(
                    o.buttons.iter().any(|b| b.label == reject && b.enabled),
                    "{tag}: Reject {:?}",
                    o.buttons
                );
                // Send follows the connection; the plan's actions do not
                assert_eq!(o.buttons.iter().any(|b| b.label == send), ready, "{tag}: Send");
                match (ready, with_log) {
                    (true, _) => assert!(o.field.is_some(), "{tag}: the input"),
                    (false, false) => {
                        assert!(o.field.is_none(), "{tag}: no input");
                        assert_eq!(count_text(&o, &off_hint), 0, "{tag}: no dead composer");
                        // the status line would say the same words a second time
                        assert_eq!(count_text(&o, &not_connected), 1, "{tag}: no status line");
                        assert_eq!(
                            o.buttons.iter().filter(|b| b.label == connect).count(),
                            1,
                            "{tag}: the way to connect stays"
                        );
                    }
                    (false, true) => assert!(o.field.is_some(), "{tag}: the disabled input"),
                }
            }
        }
    }
}

/// Not connected with no plan to act on: the connect card and nothing of a plan.
fn no_plan_shows_only_the_connect_card(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let reject = cat.t("ai.plan.reject");
    let connect = cat.t("ai.state.no_profile.action");
    let empty = facts(plan(&ed, 0, false, false), true, false);
    for (name, f) in [("no plan", Facts::default()), ("plan without items", empty)] {
        let tag = format!("{name} {lang:?}");
        let mut st = base(false);
        let o = render(&ed, &mut st, &f, lang, ddc_ui::Mode::Light);
        check(&tag, &o);
        assert!(o.buttons.iter().all(|b| b.label != reject), "{tag}: {:?}", o.buttons);
        let primaries: Vec<_> = o.buttons.iter().filter(|b| b.primary).collect();
        assert_eq!(primaries.len(), 1, "{tag}: {primaries:?}");
        assert_eq!(primaries[0].label, connect, "{tag}: the primary connects");
        assert!(o.field.is_none(), "{tag}: no input");
        assert!(!has_text(&o, &cat.t("ai.plan.title")), "{tag}: no plan card");
        assert!(!has_text(&o, &cat.t("ai.proposal.title")), "{tag}: no proposal card");
        assert!(has_text(&o, &cat.t(chat::CONNECT_EXAMPLES[0])), "{tag}: the examples");
    }
}

/// Not connected, empty log: a plan Apply refuses says why, Reject stays, a refused Apply is seen.
fn not_connected_plan_says_why(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let reject = cat.t("ai.plan.reject");
    let apply = cat.plural("ai.items.apply", 2);
    for (name, f, why) in [
        ("stale", facts(plan(&ed, 2, true, false), false, true), "ai.plan.stale"),
        ("blocked", facts(plan(&ed, 2, false, true), false, false), "ai.err.plan_blocked"),
    ] {
        let tag = format!("{name} {lang:?}");
        let mut st = base(false);
        let o = render(&ed, &mut st, &f, lang, ddc_ui::Mode::Light);
        check(&tag, &o);
        assert!(
            o.buttons.iter().any(|b| b.label == apply && b.primary && !b.enabled),
            "{tag}: Apply is drawn and off {:?}",
            o.buttons
        );
        assert!(o.buttons.iter().any(|b| b.label == reject && b.enabled), "{tag}: Reject");
        assert!(has_text(&o, &cat.t(why)), "{tag}: the reason");
    }
    let mut st = base(false);
    st.error = Some(AiError::Stale);
    let f = facts(plan(&ed, 2, false, false), true, false);
    let o = render(&ed, &mut st, &f, lang, ddc_ui::Mode::Light);
    check("refused", &o);
    assert!(has_text(&o, &cat.t(logic::err_key(ErrKind::Stale))), "{lang:?}: the refusal is seen");
}

/// The not-connected panel holding an error or a turn limit, for the layout contract.
pub(crate) fn layout_probe_error(lang: ddc_ui::Lang) -> crate::layout_contract::Probe {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let mut st = base(false);
    st.error = Some(AiError::Stale);
    let mut icons = IconCache::default();
    let mut hot = None;
    let f = Facts::default();
    crate::layout_contract::probe("ai.panel", |ctx| {
        let cx = Cx { th: &th, cat: &cat, ed: &ed, view3d: false, palette_open: false };
        let mut acts: Vec<Action> = vec![];
        host(ctx, "ai.panel", egui::vec2(PANEL_W.0, 600.0), |ui| {
            chat::panel(ui, &mut st, &cx, &mut icons, &f, &mut acts, &mut hot);
        });
    })
}

fn text_top(o: &Out, text: &str) -> Option<f32> {
    o.texts.iter().find(|(t, _, _)| t == text).map(|(_, r, _)| r.top())
}

/// Not connected, empty log, no draft: an error or a turn limit is drawn above the connect card
/// and the card keeps its one primary; with a draft of no items too.
fn not_connected_errors_stay_seen(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let connect = cat.t("ai.state.no_profile.action");
    let no_profile = cat.t("ai.state.no_profile");
    let mut cases: Vec<(Option<AiError>, Option<&'static str>, ErrKind)> = vec![];
    for e in [
        AiError::Stale,
        AiError::Blocked,
        AiError::Provider("authentication failed".to_string()),
        AiError::Provider("key spending limit reached: Key limit exceeded".to_string()),
        AiError::Provider("account balance used up: Insufficient credits".to_string()),
        AiError::Internal(LONG_WORD.to_string()),
    ] {
        let k = logic::classify(&e);
        cases.push((Some(e), None, k));
    }
    for what in ["tokens", "cost", "steps"] {
        cases.push((None, Some(what), ErrKind::TurnLimit));
    }
    for (error, limit, kind) in cases {
        let empty = facts(plan(&ed, 0, false, false), true, false);
        for (draft, f) in [("no draft", Facts::default()), ("empty draft", empty)] {
            let tag = format!("{kind:?} {limit:?} {draft} {lang:?}");
            let mut st = base(false);
            st.error = error.clone();
            st.limit = limit;
            assert_eq!(chat::state_of(&st, &f), PanelState::NoProfile, "{tag}");
            let o = render(&ed, &mut st, &f, lang, ddc_ui::Mode::Light);
            check(&tag, &o);
            let title = cat.t(logic::err_key(kind));
            let at = text_top(&o, &title);
            assert!(at.is_some(), "{tag}: the error {title:?} is hidden");
            assert_eq!(count_text(&o, &title), 1, "{tag}: said once");
            let primaries: Vec<_> = o.buttons.iter().filter(|b| b.primary).collect();
            assert_eq!(primaries.len(), 1, "{tag}: {primaries:?}");
            assert_eq!(primaries[0].label, connect, "{tag}: the primary still connects");
            let card = text_top(&o, &no_profile).expect("the connect card");
            assert!(at.unwrap() < card, "{tag}: the error sits above the connect card");
        }
    }
}

/// An external agent's draft of `n` walls, as the session reports it with no chat running.
fn agent_facts(ed: &Editor, n: usize) -> Facts {
    use crate::ai::session::{AgentDraftView, AgentOp};
    let ops = (0..n as u32)
        .map(|item| AgentOp {
            item,
            title: format!("Wall {item}"),
            checked: true,
            conflict: None,
            created: 1,
            changed: 0,
            deleted: 0,
        })
        .collect();
    let d = AgentDraftView {
        actor: "mcp:agent".into(),
        name: "agent".into(),
        number: 1,
        rev: 1,
        ops,
        plan: plan(ed, n, false, false),
    };
    Facts { agents: vec![Arc::new(d)], agent_ready: true, ..Facts::default() }
}

/// ui_no_model_agent_draft: with no model configured an agent's draft still shows its card with
/// the counts and Apply, Preview and Discard, and Apply is the one primary button.
fn no_model_agent_draft(lang: ddc_ui::Lang) {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let f = agent_facts(&ed, 4);
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        let mut st = base(false);
        assert_eq!(chat::state_of(&st, &f), PanelState::NoProfile);
        let o = render(&ed, &mut st, &f, lang, mode);
        let tag = format!("{lang:?} {mode:?}");
        check(&tag, &o);
        let labels: Vec<&str> = o.buttons.iter().map(|b| b.label.as_str()).collect();
        for k in ["agent_draft.card.apply", "agent_draft.ag.preview", "agent_draft.card.discard"] {
            assert!(labels.contains(&cat.t(k).as_str()), "{tag}: no {k} button in {labels:?}");
        }
        let ops = cat.tf("agent_draft.card.ops", &[("n", "4")]);
        assert!(text_top(&o, &ops).is_some(), "{tag}: the count {ops:?} is missing");
        let primaries: Vec<_> = o.buttons.iter().filter(|b| b.primary).collect();
        assert_eq!(primaries.len(), 1, "{tag}: {primaries:?}");
        assert_eq!(primaries[0].label, cat.t("agent_draft.card.apply"), "{tag}");
    }
}

#[test]
fn no_model_agent_draft_is_appliable_in_english() {
    no_model_agent_draft(ddc_ui::Lang::En);
}

#[test]
fn no_model_agent_draft_is_appliable_in_russian() {
    no_model_agent_draft(ddc_ui::Lang::Ru);
}

#[test]
fn not_connected_errors_stay_seen_in_english() {
    not_connected_errors_stay_seen(ddc_ui::Lang::En);
}

#[test]
fn not_connected_errors_stay_seen_in_russian() {
    not_connected_errors_stay_seen(ddc_ui::Lang::Ru);
}

#[test]
fn pending_plan_keeps_actions_in_english() {
    pending_plan_keeps_actions(ddc_ui::Lang::En);
}

#[test]
fn pending_plan_keeps_actions_in_russian() {
    pending_plan_keeps_actions(ddc_ui::Lang::Ru);
}

#[test]
fn no_plan_shows_only_the_connect_card_in_english() {
    no_plan_shows_only_the_connect_card(ddc_ui::Lang::En);
}

#[test]
fn no_plan_shows_only_the_connect_card_in_russian() {
    no_plan_shows_only_the_connect_card(ddc_ui::Lang::Ru);
}

#[test]
fn not_connected_plan_says_why_in_english() {
    not_connected_plan_says_why(ddc_ui::Lang::En);
}

#[test]
fn not_connected_plan_says_why_in_russian() {
    not_connected_plan_says_why(ddc_ui::Lang::Ru);
}

#[test]
fn title_strip_and_name_follow_the_connection_in_english() {
    title_strip_and_name(ddc_ui::Lang::En);
}

#[test]
fn title_strip_and_name_follow_the_connection_in_russian() {
    title_strip_and_name(ddc_ui::Lang::Ru);
}

#[test]
fn connected_composer_keeps_send_with_the_input_in_english() {
    connected_composer(ddc_ui::Lang::En);
}

#[test]
fn connected_composer_keeps_send_with_the_input_in_russian() {
    connected_composer(ddc_ui::Lang::Ru);
}

#[test]
fn not_connected_is_composed_in_english() {
    not_connected_group(ddc_ui::Lang::En);
}

#[test]
fn not_connected_is_composed_in_russian() {
    not_connected_group(ddc_ui::Lang::Ru);
}

#[test]
fn every_panel_state_fits_in_english() {
    run_lang(ddc_ui::Lang::En);
}

#[test]
fn every_panel_state_fits_in_russian() {
    run_lang(ddc_ui::Lang::Ru);
}

#[test]
fn drawing_a_ready_plan_leaves_the_model_alone() {
    let ed = Editor::new_in_memory().unwrap();
    let mut st = base(true);
    let f = facts(plan(&ed, 3, false, false), true, false);
    let o = render(&ed, &mut st, &f, ddc_ui::Lang::En, ddc_ui::Mode::Light);
    assert_eq!(o.head_before, o.head_after);
    assert!(!o.acts.iter().any(|a| matches!(a, Action::Apply | Action::ApplyProposal(_))));
    assert!(o.buttons.iter().any(|b| b.primary && b.enabled), "Apply is armed but not fired");
}

/// Not connected is never a dead end: each example has a quiet button that answers it by hand
/// now, through an app command that runs without a connection; connect stays the one primary.
#[test]
fn not_connected_examples_each_answer_by_hand() {
    let ed = Editor::new_in_memory().unwrap();
    for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
        let cat = ddc_ui::Catalog::new(lang);
        let mut st = base(false);
        let o = render(&ed, &mut st, &Facts::default(), lang, ddc_ui::Mode::Light);
        let tag = format!("{lang:?}");
        check(&tag, &o);
        assert_eq!(o.buttons.iter().filter(|b| b.primary).count(), 1, "{tag}");
        for (label, _) in chat::OFFLINE_ANSWERS {
            let l = cat.t(label);
            assert!(!looks_like_key(&l), "{tag}: {label} untranslated");
            let b = o.buttons.iter().find(|b| b.label == l);
            assert!(b.is_some_and(|b| !b.primary && b.enabled), "{tag}: {l:?} in {:?}", o.buttons);
        }
    }
    let mut app = crate::shell::EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    for (_, cmd) in chat::OFFLINE_ANSWERS {
        assert!(app.run(cmd), "{cmd} does not run without a connection");
    }
}

/// The depth chip sits right above the input, inside the panel, at both widths; the full
/// label at the wide panel, the project depth at least at the narrow one. Not connected: none.
fn depth_chip_above_the_input(lang: ddc_ui::Lang) {
    let mut ed = Editor::new_in_memory().unwrap();
    let s = ddc_model::DepthSettings { project: 3, ..Default::default() };
    crate::depth_ui::apply(&mut ed, s).unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let full = format!("{} \u{25be}", crate::depth_ui::chip_text(&cat, &s));
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        for w in [PANEL_W.0, PANEL_W.1] {
            let tag = format!("depth chip {lang:?} {mode:?} {w}px");
            let mut st = base(true);
            let o = render_at(&ed, &mut st, &Facts::default(), lang, mode, egui::vec2(w, 900.0));
            assert!(o.acts.is_empty(), "{tag}: an idle frame emitted an action");
            assert_eq!(o.head_before, o.head_after, "{tag}: the model changed");
            let field = o.field.unwrap_or_else(|| panic!("{tag}: no input drawn"));
            let chip = o
                .buttons
                .iter()
                .find(|b| b.label.ends_with('\u{25be}'))
                .unwrap_or_else(|| panic!("{tag}: no depth chip"));
            assert!(chip.label.contains("D3"), "{tag}: {chip:?}");
            assert!(!chip.primary, "{tag}: the chip is not a primary");
            assert!(chip.right <= w + EPS, "{tag}: chip past the panel edge");
            let gap = field.top() - chip.bottom;
            assert!(gap >= -EPS, "{tag}: chip overlaps the input ({gap})");
            assert!(gap <= space::MD, "{tag}: chip floats {gap} px over the input");
            if w >= PANEL_W.1 {
                assert_eq!(chip.label, full, "{tag}: the wide panel shows the whole chip");
            }
        }
    }
    let o = render(&ed, &mut base(false), &Facts::default(), lang, ddc_ui::Mode::Light);
    assert!(
        !o.buttons.iter().any(|b| b.label.ends_with('\u{25be}')),
        "{lang:?}: no chip without a connection"
    );
}

#[test]
fn depth_chip_above_the_input_in_english() {
    depth_chip_above_the_input(ddc_ui::Lang::En);
}

#[test]
fn depth_chip_above_the_input_in_russian() {
    depth_chip_above_the_input(ddc_ui::Lang::Ru);
}

/// The chip's popup body alone in the narrowest panel: in place, no action without input.
#[test]
fn depth_popup_body_fits_and_stays_idle() {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (6.0, 0.0))]);
    for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
        let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = ddc_ui::Catalog::new(lang);
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let mut acts = vec![];
        let mut texts = vec![];
        for _ in 0..3 {
            acts.clear();
            let out = ctx.run(Default::default(), |ctx| {
                let cx = Cx { th: &th, cat: &cat, ed: &ed, view3d: false, palette_open: false };
                egui::SidePanel::left("depth.popup.test")
                    .exact_width(PANEL_W.0)
                    .resizable(false)
                    .show(ctx, |ui| {
                        super::depth_chip::popup(ui, &cx, ed.model.depth_settings().unwrap(), &mut acts)
                    });
            });
            texts.clear();
            for c in &out.shapes {
                collect(&c.shape, c.clip_rect, &mut texts);
            }
        }
        assert!(acts.is_empty(), "{lang:?}: idle popup emitted {}", acts.len());
        let est = cat.t("depth.estimate");
        let head = est.split('{').next().unwrap_or("").trim().to_string();
        assert!(
            texts.iter().any(|(t, _, _)| t.starts_with(&head)),
            "{lang:?}: no estimate line"
        );
        for (t, r, _) in &texts {
            assert!(!looks_like_key(t), "{lang:?}: missing translation {t:?}");
            assert!(r.right() <= PANEL_W.0 + EPS, "{lang:?}: {t:?} past the panel");
        }
    }
}
