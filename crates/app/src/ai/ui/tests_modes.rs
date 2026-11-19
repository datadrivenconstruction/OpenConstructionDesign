// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI tabs on the real engine and the whole app: switching, one conversation for the tabs and
//! the quick panel, Apply and Undo from each tab, and the layout of every tab and state.

use super::modes::{self, AiMode};
use super::{with, Action, PANEL};
use crate::ai::{with_session, SessionCfg, UiSession, ViewCtx};
use crate::layout_contract::{app_in, font_ctx, pass_on, violations, Probe};
use crate::shell::EditorApp;
use ddc_ai::fake::{self, Script, ScriptedProvider};
use ddc_ui::{Catalog, Density, Lang, Theme};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(60);
/// The scripted model's closing words: they show wherever the conversation is drawn.
const REPLY: &str = "Drafted the house as one item; check it in the preview.";
const FRAMES: usize = 4;
/// Window sizes of the layout probes, in points.
pub(crate) const SIZES: [(f32, f32); 3] = [(1280.0, 800.0), (1440.0, 900.0), (1920.0, 1080.0)];

/// The whole app in a headless context of one size, with fonts and accessibility on.
pub(crate) struct Shell {
    pub app: EditorApp,
    ctx: egui::Context,
    th: Theme,
    size: (f32, f32),
    /// Probe the topmost modal window instead of the panel.
    pub(crate) on_modal: bool,
}

impl Shell {
    /// The first frame loads the panel (profiles, project chat); the fixtures go in after it,
    /// so the tests never depend on connections saved on this machine.
    pub(crate) fn new(lang: Lang, size: (f32, f32)) -> Self {
        // one app per panel state: what an earlier app of this thread left goes with it
        end_session();
        with(|st| *st = super::PanelUi::default());
        let ctx = font_ctx();
        ctx.enable_accesskit();
        let th = Theme::new(ddc_ui::Mode::Dark, Density::Compact);
        let mut s = Self { app: app_in(lang), ctx, th, size, on_modal: false };
        s.pass(vec![]);
        with(|st| {
            st.profiles.clear();
            st.profile = 0;
            st.open = false;
        });
        s
    }

    pub(crate) fn resize(&mut self, size: (f32, f32)) {
        self.size = size;
    }

    pub(crate) fn pass(&mut self, events: Vec<egui::Event>) -> Probe {
        if self.on_modal {
            // one theme for both appliers: two faces would reinstall the fonts every frame
            self.th = self.app.sh.theme.clone();
        }
        let app = &mut self.app;
        let mut id = egui::Id::new("aiux1.shell");
        if self.on_modal {
            // a modal when one is up, else the topmost dialog window (most dialogs float)
            if let Some(l) = self.ctx.memory(|m| m.top_modal_layer()) {
                id = l.id;
            } else if let Some(w) = ddc_ui::z::top_window(&self.ctx) {
                id = w;
            }
        }
        pass_on(&self.ctx, &mut self.th, id, self.size, events, &mut |x| app.ui(x, None))
    }

    pub(crate) fn settle(&mut self) -> Probe {
        for _ in 0..FRAMES - 1 {
            self.pass(vec![]);
        }
        self.pass(vec![])
    }

    /// A primary click at `at` over three frames, then the frames settle.
    pub(crate) fn click(&mut self, at: egui::Pos2) -> Probe {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.pass(vec![egui::Event::PointerMoved(at)]);
        self.pass(vec![button(true)]);
        self.pass(vec![button(false)]);
        self.settle()
    }
}

/// The session ends with its app, as the panel ends it, and not in the thread-local destructor
/// at thread exit, where the worker join does not return.
impl Drop for Shell {
    fn drop(&mut self) {
        end_session();
    }
}

fn end_session() {
    let ended = with_session(|x| x.take());
    drop(ended);
}

/// A connection the panel counts as ready; no request leaves the test.
pub(crate) fn connect() {
    with(|st| st.profiles = vec![fake::profile()]);
}

/// A real `generate_project` draft from the scripted model, waiting in the session.
pub(crate) fn draft(app: &mut EditorApp) {
    let spec = serde_json::json!({"spec": {"typology": "house", "floors": 2}});
    let turns = vec![
        vec![Script::Tool { name: "generate_project".to_string(), args: spec }],
        vec![Script::Text(REPLY.to_string())],
    ];
    let cfg = SessionCfg {
        provider: ScriptedProvider::new(turns),
        profile: fake::profile(),
        limits: ddc_ai::Limits::default(),
        session: "aiux1-modes".to_string(),
        allow_snapshots: Arc::new(AtomicBool::new(false)),
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: false,
        confirm_ttl: crate::ai::session::CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: false,
    };
    let view = ViewCtx {
        view: "plan".to_string(),
        level: app.ed.level,
        units: app.ed.tool.units,
        ..ViewCtx::default()
    };
    let mut s = UiSession::start(&app.ed, cfg, view.clone()).unwrap();
    assert!(s.wait_idle(&mut app.ed, WAIT), "the session did not open");
    s.send_turn(&app.ed, "A two-storey house".to_string(), view, ddc_ai::Mode::Propose)
        .unwrap();
    assert!(s.wait_idle(&mut app.ed, WAIT), "the turn did not finish");
    if !s.can_apply() {
        // say why: the tool's refusal, or the checks that hold Apply
        let tools: Vec<String> = s
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                crate::ai::UiEvent::Tool { name, ok, err, code } => Some(format!("{name} ok={ok:?} {code:?} {err:?}")),
                crate::ai::UiEvent::Failed(e) => Some(format!("failed {e:?}")),
                _ => None,
            })
            .collect();
        let blocks: Option<Vec<String>> = s.plan().map(|p| {
            p.issues
                .iter()
                .chain(&p.main_issues)
                .filter(|i| i.level == crate::ai::IssueLevel::Block)
                .map(|i| format!("{} {:?}", i.text_key, i.args))
                .collect()
        });
        panic!("the scripted turn left no draft: stale={} tools={tools:?} blocks={blocks:?}", s.is_stale());
    }
    with_session(|x| *x = Some(s));
}

/// Runs `a` on the one conversation, as its own frame would.
fn act(app: &mut EditorApp, a: Action) {
    let mut st = PANEL.with(|p| std::mem::take(&mut *p.borrow_mut()));
    st.run(app, a);
    PANEL.with(|p| *p.borrow_mut() = st);
}

fn walls(app: &EditorApp) -> i64 {
    app.ed
        .model
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap()
}

fn has_text(p: &Probe, text: &str) -> bool {
    p.text.iter().any(|t| t.contains(text))
}

fn control<'a>(p: &'a Probe, label: &str) -> Option<&'a crate::layout_contract::Control> {
    p.controls.iter().find(|c| c.label == label)
}

/// The label of the card's Apply with the draft's one item checked.
fn apply_label(cat: &Catalog) -> String {
    cat.plural("ai.items.apply", 1)
}

#[test]
fn tabs_open_from_the_bar_commands_and_leave_for_a_view() {
    let mut sh = Shell::new(Lang::En, SIZES[1]);
    let cat = Catalog::new(Lang::En);
    let p = sh.settle();
    for m in AiMode::ALL {
        assert!(has_text(&p, &cat.t(m.label_key())), "{m:?}: no tab in the bar");
    }
    // a click on the tab in the bar: the view-tab bar is the lowest bar of the window top, under
    // the ribbon, whose tabs may carry the same word
    let at = p
        .spans
        .iter()
        .filter(|(t, r)| *t == cat.t(AiMode::Build.label_key()) && r.top() < SIZES[1].1 / 3.0)
        .max_by(|a, b| a.1.top().total_cmp(&b.1.top()))
        .map(|(_, r)| r.center())
        .expect("build tab");
    sh.click(at);
    assert_eq!(modes::current(), Some(AiMode::Build));
    // the commands, as the palette and the keys run them
    for m in AiMode::ALL {
        assert!(sh.app.run(m.command()), "{m:?}: command not registered");
        sh.settle();
        assert_eq!(modes::current(), Some(m));
        assert!(!with(|st| st.docked()), "{m:?}: the quick panel doubles the tab");
    }
    // Ctrl+J in a tab stays in it; the quick panel command goes back to the model
    sh.app.run("ai.ask");
    assert_eq!(modes::current(), Some(AiMode::Review));
    sh.app.run("ai.panel");
    sh.settle();
    assert_eq!(modes::current(), None);
    assert!(with(|st| st.docked()), "ai.panel from a tab opens the quick panel");
    // another view opening under the tab leaves it
    sh.app.run(AiMode::Chat.command());
    sh.settle();
    sh.app.view3d = true;
    sh.settle();
    assert_eq!(modes::current(), None, "3D opened: the tab gave way");
}

/// The AI modes are one segmented switch after the view tabs: a single framed track
/// holds the three modes and nothing else, so they do not read as three more documents.
#[test]
fn ai_modes_are_one_segment_apart_from_the_view_tabs() {
    for lang in [Lang::En, Lang::Ru] {
        let cat = Catalog::new(lang);
        let mut sh = Shell::new(lang, SIZES[1]);
        let p = sh.settle();
        let names: Vec<String> = AiMode::ALL.iter().map(|m| cat.t(m.label_key())).collect();
        let modes: Vec<egui::Rect> = names
            .iter()
            .map(|n| {
                p.controls
                    .iter()
                    .filter(|c| c.label == *n && c.rect.top() < SIZES[1].1 / 3.0)
                    .max_by(|a, b| a.rect.top().total_cmp(&b.rect.top()))
                    .unwrap_or_else(|| panic!("{lang:?}: no {n:?} mode"))
                    .rect
            })
            .collect();
        let all = modes.iter().fold(modes[0], |a, r| a.union(*r));
        let track = p.shapes.iter().find_map(|s| match s {
            egui::Shape::Rect(r)
                if r.stroke.width > 0.0
                    && r.rect.width() > 0.9 * all.width()
                    && r.rect.width() <= all.width() + 0.5
                    && all.expand(0.5).contains_rect(r.rect) =>
            {
                Some(r.rect)
            }
            _ => None,
        });
        let track = track.unwrap_or_else(|| panic!("{lang:?}: no framed track around the modes"));
        // the track holds only the modes: no other control of the bar starts inside it
        let others = p.controls.iter().filter(|c| {
            !names.contains(&c.label) && track.contains(c.rect.center()) && c.rect.width() > 4.0
        });
        assert_eq!(others.count(), 0, "{lang:?}: another control inside the mode track");
        for w in modes.windows(2) {
            assert!(w[1].left() - w[0].right() <= 2.0, "{lang:?}: modes are not one segment");
        }
    }
}

/// Without a model every mode shows one and the same setup card under the panel
/// name, not three cards that each ask to connect.
#[test]
fn one_setup_card_for_the_whole_panel() {
    for lang in [Lang::En, Lang::Ru] {
        let cat = Catalog::new(lang);
        let mut cards = vec![];
        for m in AiMode::ALL {
            let mut sh = tab_in(lang, m, State::Setup);
            let p = sh.settle();
            let connect = cat.t("ai_modes.setup.connect");
            let n = p.controls.iter().filter(|c| c.label == connect).count();
            assert_eq!(n, 1, "{lang:?} {m:?}: {n} connect buttons");
            assert!(has_text(&p, &cat.t("cmd.ai.panel")), "{lang:?} {m:?}: no panel name");
            assert!(!has_text(&p, &cat.t(m.title_key())) || m == AiMode::Chat);
            let keys = ["title", "lead", "1", "2", "3", "privacy"];
            let card: Vec<String> = keys.map(|k| cat.t(&format!("ai_modes.setup.{k}"))).into();
            for line in &card {
                assert!(has_text(&p, line), "{lang:?} {m:?}: the card lacks {line:?}");
            }
            cards.push(p.text.iter().filter(|t| card.contains(t)).cloned().collect::<Vec<_>>());
        }
        assert!(cards.windows(2).all(|w| w[0] == w[1]), "{lang:?}: the card differs by mode");
    }
}

#[test]
fn one_conversation_for_every_tab_and_the_quick_panel() {
    let mut sh = Shell::new(Lang::En, SIZES[2]);
    connect();
    draft(&mut sh.app);
    let log = |p: &Probe| has_text(p, REPLY);
    for m in AiMode::ALL {
        sh.app.run(m.command());
        let p = sh.settle();
        assert!(log(&p), "{m:?}: the conversation is not shown");
    }
    let n = with(|st| st.log.len());
    sh.app.run("ai.panel");
    let p = sh.settle();
    assert!(log(&p), "quick panel: not the same conversation");
    assert_eq!(with(|st| st.log.len()), n, "switching added or lost entries");
}

#[test]
fn apply_and_undo_from_each_tab_is_one_step() {
    for m in AiMode::ALL {
        let mut sh = Shell::new(Lang::Ru, SIZES[2]);
        let cat = Catalog::new(Lang::Ru);
        connect();
        let before = walls(&sh.app);
        draft(&mut sh.app);
        sh.app.run(m.command());
        let p = sh.settle();
        assert_eq!(walls(&sh.app), before, "{m:?}: the draft changed the model");
        let apply = control(&p, &apply_label(&cat))
            .unwrap_or_else(|| panic!("{m:?}: no Apply in {:?}", p.text))
            .rect
            .center();
        let p = sh.click(apply);
        assert!(walls(&sh.app) > before, "{m:?}: Apply did not write the draft");
        assert_eq!(modes::current(), Some(m), "{m:?}: Apply left the tab");
        if m == AiMode::Build {
            let next = cat.t("ai_modes.build.drawings.title");
            assert!(has_text(&p, &next), "build: no drawings step after Apply");
        }
        sh.app.ed.undo();
        assert_eq!(walls(&sh.app), before, "{m:?}: one undo did not take it back");
    }
}

#[test]
fn the_build_form_reaches_the_generator_within_its_limits() {
    let max = ddc_gen::project::slots::MAX_STOREYS;
    let mut f = modes::BuildForm { kind: 1, floors: max + 99, floor_h: 1.0, outputs: [true; 2] };
    let line = f.spec_line();
    assert!(line.contains("typology=apartment_section"), "{line}");
    assert!(line.contains(&format!("floors={max}")), "{line}");
    let lo = ddc_gen::project::limits::FLOOR_HEIGHT.min;
    assert!(line.contains(&format!("floor_height={lo:.2}")), "{line}");
    assert_eq!(f.output_cmds().count(), 2);
    f.outputs = [false, true];
    let ids: Vec<_> = f.output_cmds().collect();
    assert_eq!(ids, ["data.export_ifc"]);
    // every output names a command the app registers
    let app = app_in(Lang::En);
    for id in ids.iter().chain(&["data.export_pdf"]) {
        assert!(app.handlers.contains_key(*id), "{id} is not registered");
    }
}

#[test]
fn build_steps_follow_the_conversation() {
    use super::modes::{build_step, BuildStep as S};
    use super::Entry;
    let user = Entry::User("x".into());
    assert_eq!(build_step(&[], false, false, false, false), S::Brief);
    assert_eq!(build_step(&[], true, false, false, false), S::Review);
    assert_eq!(build_step(&[], false, true, false, false), S::Brief);
    // the receipt of a real Apply of a real draft
    let mut sh = Shell::new(Lang::En, SIZES[0]);
    connect();
    draft(&mut sh.app);
    sh.settle();
    act(&mut sh.app, Action::Apply);
    sh.settle();
    let step = |fresh, sheets| with(|st| build_step(&st.log, false, false, fresh, sheets));
    assert_eq!(step(false, false), S::Drawings);
    assert_eq!(step(false, true), S::Files);
    assert_eq!(step(true, true), S::Brief, "new brief");
    with(|st| st.log.push(user));
    assert_eq!(step(false, true), S::Brief, "a newer request");
}

/// What one tab shows in one state, for the layout probes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum State {
    /// No connection, nothing to decide.
    Setup,
    Idle,
    /// A real draft waiting.
    Draft,
    /// The draft applied.
    Applied,
    /// The review tab after its checks ran.
    Checked,
}

/// The app on tab `m` in `state`, settled at the first size; `SIZES` resize it.
pub(crate) fn tab_in(lang: Lang, m: AiMode, state: State) -> Shell {
    let mut sh = Shell::new(lang, SIZES[0]);
    if state != State::Setup {
        connect();
    }
    if matches!(state, State::Draft | State::Applied) {
        draft(&mut sh.app);
    }
    sh.app.run(m.command());
    sh.settle();
    match state {
        State::Applied => act(&mut sh.app, Action::Apply),
        State::Checked => act(&mut sh.app, Action::CheckInReview),
        _ => {}
    }
    sh
}

/// Every tab in every state at every size: no control leaves the app window or the screen, no
/// control's text is cut, and the state's own controls are there.
pub(crate) fn contract(lang: Lang) {
    let cat = Catalog::new(lang);
    let states = [State::Setup, State::Idle, State::Draft, State::Applied, State::Checked];
    let mut fails: Vec<String> = vec![];
    for m in AiMode::ALL {
        for state in states {
            if state == State::Checked && m != AiMode::Review {
                continue;
            }
            let mut sh = tab_in(lang, m, state);
            for size in SIZES {
                sh.resize(size);
                let mut p = sh.settle();
                // the whole app is drawn: its window is the screen
                p.inner = p.screen;
                let tag = format!("{lang:?} {m:?} {state:?} {}x{}", size.0, size.1);
                let bad = violations(&p, false);
                if !bad.is_empty() {
                    eprintln!("{}", crate::layout_contract::dump(&tag, &p, &bad));
                }
                fails.extend(bad.into_iter().map(|b| format!("{tag}: {b}")));
                let mut want = |ok: bool, what: &str| {
                    if !ok {
                        fails.push(format!("{tag}: no {what}"));
                    }
                };
                // before a connection the panel, not the mode, asks for a model
                let title = if state == State::Setup { "cmd.ai.panel" } else { m.title_key() };
                want(has_text(&p, &cat.t(title)), "title");
                match state {
                    State::Setup => {
                        want(control(&p, &cat.t("ai_modes.setup.connect")).is_some(), "connect");
                    }
                    State::Draft => want(control(&p, &apply_label(&cat)).is_some(), "Apply"),
                    State::Applied if m == AiMode::Build => {
                        let t = cat.t("ai_modes.build.drawings.title");
                        want(has_text(&p, &t), "drawings step");
                    }
                    State::Checked => {
                        want(has_text(&p, &cat.t("ai_modes.review.errors")), "counters");
                    }
                    State::Idle if m == AiMode::Build => {
                        let b = cat.t("ai_modes.build.generate");
                        want(control(&p, &b).is_some(), "generate");
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(fails.is_empty(), "{} faults:\n{}", fails.len(), fails.join("\n"));
}
