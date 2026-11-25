// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Draft preview contract of the two AI cards: the plan card (the assistant's own draft) and the
//! proposal card (an agent's `ops_propose`). Both show the same draft of the same change and
//! must keep the same promises, so every test runs a table of card kind x language:
//! the model stays at its revision until Apply, the chip counts are the draft's ops, a hovered
//! chip lights exactly the affected live elements, Reject leaves model and outline clean, Apply
//! is one undo step, and a draft the model has moved past can no longer be applied.
//! Fonts and the catalog are explicit; the global UI language is never touched.

use super::kit::probe;
use super::{chat, plan_card, proposal_card, proposal_draft, Action, Cx, Facts, PanelUi};
use crate::ai::exec::{self, ModelStep};
use crate::ai::plan::{CompiledPlan, ElemDiff, PlanItem, PlanOp, PlanView, Targets};
use crate::ai::session::{EnvelopeRunner, Proposals};
use crate::ai::validate::Target;
use crate::ai::AiError;
use crate::editor::ops_run::set_thread_exec;
use crate::editor::Editor;
use crate::ops_exec::{apply_proposal, ModelExec};
use ddc_model::{Edit, ElKey, Model, RowKind, WallSpec};
use ddc_ops::{Actor, Cat, Op, Ref};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const KINDS: [Kind; 2] = [Kind::Plan, Kind::Proposal];
const LANGS: [ddc_ui::Lang; 2] = [ddc_ui::Lang::En, ddc_ui::Lang::Ru];
/// Walls the draft deletes and creates; distinct numbers so a chip is told from the other.
const DELETED: usize = 3;
const CREATED: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Plan,
    Proposal,
}

fn wall_ids(ed: &Editor) -> Vec<i64> {
    let mut st = ed.model.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
    let ids = st.query_map([], |r| r.get(0)).unwrap();
    ids.map(Result::unwrap).collect()
}

struct Snap {
    head: u64,
    undo: usize,
    walls: Vec<i64>,
}

fn snap(ed: &Editor) -> Snap {
    Snap {
        head: ed.model.change_head(),
        undo: ed.undo_len(),
        walls: wall_ids(ed),
    }
}

fn same(ed: &Editor, s: &Snap, why: &str) {
    assert_eq!(ed.model.change_head(), s.head, "{why}: model revision");
    assert_eq!(ed.undo_len(), s.undo, "{why}: undo steps");
    assert_eq!(wall_ids(ed), s.walls, "{why}: walls");
}

/// One live model with four walls and one pending draft in the shape of `kind`: delete the
/// first three walls, create two new ones. Both kinds hold the same change.
struct Fx {
    kind: Kind,
    ed: Editor,
    all: Vec<i64>,
    gone: Vec<i64>,
    next: i64,
    plan: Option<Arc<PlanView>>,
    cards: Proposals,
    pid: u32,
    scratch: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        set_thread_exec(None);
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

static SERIAL: AtomicUsize = AtomicUsize::new(0);

fn new_segs() -> Vec<(ddc_geom::V2, ddc_geom::V2)> {
    vec![
        (ddc_geom::v2(20.0, 0.0), ddc_geom::v2(20.0, 5.0)),
        (ddc_geom::v2(30.0, 0.0), ddc_geom::v2(30.0, 5.0)),
    ]
}

fn plan_item(id: u32, title: &str) -> PlanItem {
    PlanItem {
        id,
        title: title.to_string(),
        why: "Tidy the walls".to_string(),
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

impl Fx {
    fn new(kind: Kind) -> Fx {
        set_thread_exec(Some(Arc::new(ModelExec)));
        let mut ed = Editor::new_in_memory().unwrap();
        let rows: Vec<_> = (0..4)
            .map(|i| {
                let y = 6.0 * f64::from(i);
                (ddc_geom::v2(0.0, y), ddc_geom::v2(4.0, y))
            })
            .collect();
        let env = ed.wall_envelope(&rows).expect("a wall envelope");
        ed.run_ops(env).expect("four walls");
        let all = wall_ids(&ed);
        assert_eq!(all.len(), 4, "the fixture walls");
        let gone = all[..DELETED].to_vec();
        let next = all.iter().max().copied().unwrap() + 1;
        let scratch = std::env::temp_dir().join(format!(
            "ddc-qa38a-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let mut fx = Fx {
            kind,
            ed,
            all,
            gone,
            next,
            plan: None,
            cards: Proposals::default(),
            pid: 0,
            scratch,
        };
        match kind {
            Kind::Plan => fx.plan = Some(fx.make_plan()),
            Kind::Proposal => fx.pid = fx.make_proposal(),
        }
        fx
    }

    /// Five items (two creates, three deletes): as many as the plan draft's Reject asks at.
    fn make_plan(&self) -> Arc<PlanView> {
        let specs: Vec<WallSpec> = new_segs()
            .iter()
            .map(|(a, b)| WallSpec {
                a: (a.x, a.y),
                b: (b.x, b.y),
                level: self.ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: self.ed.types[0].id,
                offset0: None,
            })
            .collect();
        let expect: Vec<i64> = (0..CREATED as i64).map(|i| self.next + i).collect();
        let mut steps = vec![ModelStep::Walls { specs, expect }];
        // as the compiler writes it: walls delete through edits, not through a late step
        let gone = self.gone.iter().map(|&id| Edit::Delete { table: "wall", id }).collect();
        steps.push(ModelStep::Edits(gone));
        let diff_row = |id: i64, kind: RowKind, item: u32| ElemDiff {
            key: ("wall".to_string(), id),
            kind,
            item: Some(item),
            level: None,
            label: format!("Wall {id}"),
            cells: vec![],
        };
        let mut diff = Vec::new();
        let mut items = Vec::new();
        for i in 0..CREATED {
            let id = self.next + i as i64;
            diff.push(diff_row(id, RowKind::Created, i as u32 + 1));
            items.push(plan_item(i as u32 + 1, "New wall"));
        }
        for (i, id) in self.gone.iter().enumerate() {
            let item = (CREATED + i) as u32 + 1;
            diff.push(diff_row(*id, RowKind::Deleted, item));
            items.push(plan_item(item, "Remove wall"));
        }
        Arc::new(PlanView {
            items,
            diff,
            secondary: 0,
            qty: vec![],
            issues: vec![],
            main_issues: vec![],
            preview: Arc::new(crate::ai::preview::PreviewSet::default()),
            compiled: CompiledPlan {
                base_head: self.ed.model.change_head(),
                steps: Arc::new(steps),
            },
            stale: false,
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

    /// The real executor on a copy of the live file, as `ops_propose` runs it.
    fn make_proposal(&mut self) -> u32 {
        let path = self.scratch.join("scratch.db");
        self.ed.model.snapshot_to(&path).unwrap();
        let mut scratch = Model::open(&path.to_string_lossy()).unwrap();
        let mut env = self.ed.wall_envelope(&new_segs()).expect("a wall envelope");
        let group = env.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1;
        let refs = self.gone.iter().map(|&id| Ref::id(Cat::Wall, id)).collect();
        env.groups.push(ddc_ops::Group {
            id: group,
            title: "drop".into(),
            ops: vec![Op::Delete {
                target: ddc_ops::Targets::Refs { refs },
            }],
            depends_on: vec![],
            why: String::new(),
        });
        env.actor = Actor::Agent {
            name: "mcp:agent".into(),
        };
        let dry = ModelExec.dry_run(&mut scratch, &env).unwrap();
        assert!(dry.rejections.is_empty(), "{:?}", dry.rejections);
        self.cards
            .add_for_test("mcp:agent", env, dry, self.ed.model.change_head())
    }

    /// What the panel reads this frame; staleness comes from the live model, as in the session.
    fn facts(&self) -> Facts {
        match self.kind {
            Kind::Plan => {
                // after Reject the session holds no draft and has nothing to apply
                let Some(p) = self.plan.clone() else {
                    return Facts {
                        has_session: true,
                        ..Facts::default()
                    };
                };
                let stale = self.ed.model.plan_stale(p.compiled.base_head);
                Facts {
                    plan: Some(p),
                    has_session: true,
                    // the session's can_apply is false once the model moved
                    can_apply: !stale,
                    stale,
                    plan_rev: 1,
                    ..Facts::default()
                }
            }
            Kind::Proposal => Facts {
                proposals: self.cards.cards(),
                ..Facts::default()
            },
        }
    }

    fn gone_keys(&self) -> BTreeSet<ElKey> {
        self.gone.iter().map(|&id| ("wall".to_string(), id)).collect()
    }

    fn is_apply(&self, a: &Action) -> bool {
        match (self.kind, a) {
            (Kind::Plan, Action::Apply) => true,
            (Kind::Proposal, Action::ApplyProposal(i)) => *i == self.pid,
            _ => false,
        }
    }

    fn is_reject(&self, a: &Action) -> bool {
        match (self.kind, a) {
            (Kind::Plan, Action::Reject) => true,
            (Kind::Proposal, Action::RejectProposal(i)) => *i == self.pid,
            _ => false,
        }
    }

    /// The person's Apply as the session runs it: the card's own executor path.
    fn run_apply(&mut self) -> Result<(), String> {
        match self.kind {
            Kind::Plan => {
                let p = self.plan.clone().unwrap();
                exec::apply(&mut self.ed, &p.compiled).map_err(|e| format!("{e:?}"))
            }
            Kind::Proposal => {
                apply_proposal(&mut self.ed, &mut self.cards, self.pid).map(|_| ())
            }
        }
    }

    /// What the session does with a Reject: the draft is gone.
    fn run_reject(&mut self) {
        match self.kind {
            Kind::Plan => self.plan = None,
            Kind::Proposal => {
                assert!(self.cards.reject(self.pid), "a waiting card can be rejected");
            }
        }
    }

    /// The views' outline of the open drafts, where the card kind has one beyond the chips.
    fn outline(&self) -> Vec<ElKey> {
        match self.kind {
            Kind::Plan => vec![],
            Kind::Proposal => proposal_draft::open_keys(&self.cards.cards()),
        }
    }
}

struct Shot {
    texts: Vec<(String, egui::Rect)>,
    acts: Vec<Action>,
    hot: Option<Vec<ElKey>>,
    btns: Vec<probe::Seen>,
}

fn collect(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
    match s {
        egui::Shape::Text(t) => {
            let text = t.galley.text().to_string();
            if !text.trim().is_empty() {
                out.push((text, t.galley.rect.translate(t.pos.to_vec2())));
            }
        }
        egui::Shape::Vec(v) => v.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// One headless panel: explicit fonts, explicit catalog, its own card state.
struct Drv {
    ctx: egui::Context,
    st: PanelUi,
    lang: ddc_ui::Lang,
    /// Draws the whole panel instead of the two cards alone.
    whole: bool,
}

impl Drv {
    /// The whole panel with no connection and an empty log (`PanelUi::default`).
    fn not_connected(lang: ddc_ui::Lang) -> Drv {
        let mut drv = Drv::new(lang);
        drv.whole = true;
        drv
    }

    fn new(lang: ddc_ui::Lang) -> Drv {
        // named font weights bind from the pass after install: a bare first frame would panic
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        Drv {
            ctx,
            st: PanelUi::default(),
            lang,
            whole: false,
        }
    }

    fn cat(&self) -> ddc_ui::Catalog {
        ddc_ui::Catalog::new(self.lang)
    }

    fn frame(&mut self, ed: &Editor, f: &Facts, events: Vec<egui::Event>) -> Shot {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = ddc_ui::Catalog::new(self.lang);
        let mut acts: Vec<Action> = Vec::new();
        let mut hot: Option<Vec<ElKey>> = None;
        let _ = probe::take();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 900.0),
            )),
            events,
            ..egui::RawInput::default()
        };
        let whole = self.whole;
        let mut icons = ddc_ui::icons::IconCache::default();
        let st = &mut self.st;
        let out = self.ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            let cx = Cx {
                th: &th,
                cat: &cat,
                ed,
                view3d: false,
                palette_open: false,
            };
            egui::CentralPanel::default().show(ctx, |ui| {
                if whole {
                    chat::panel(ui, st, &cx, &mut icons, f, &mut acts, &mut hot);
                    return;
                }
                if let Some(p) = &f.plan {
                    plan_card::card(ui, st, &cx, p, f, &mut acts, &mut hot);
                    plan_card::actions(ui, st, &cx, p, f, &mut acts);
                }
                proposal_card::cards(ui, &cx, f, &mut acts, &mut hot);
            });
        });
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        Shot {
            texts,
            acts,
            hot,
            btns: probe::take(),
        }
    }

    /// Three frames so the layout settles; the last one is returned.
    fn settle(&mut self, ed: &Editor, f: &Facts) -> Shot {
        let _ = self.frame(ed, f, vec![]);
        let _ = self.frame(ed, f, vec![]);
        self.frame(ed, f, vec![])
    }

    /// Moves the pointer onto `pos`; the frame after it sees the hover.
    fn hover(&mut self, ed: &Editor, f: &Facts, pos: egui::Pos2) -> Shot {
        let _ = self.frame(ed, f, vec![egui::Event::PointerMoved(pos)]);
        self.frame(ed, f, vec![])
    }

    /// Hover, press, release at `pos`; the release frame.
    fn click(&mut self, ed: &Editor, f: &Facts, pos: egui::Pos2) -> Shot {
        let btn = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = self.frame(ed, f, vec![egui::Event::PointerMoved(pos)]);
        let _ = self.frame(ed, f, vec![btn(true)]);
        self.frame(ed, f, vec![btn(false)])
    }
}

fn centre(b: &probe::Seen) -> egui::Pos2 {
    egui::pos2(b.right - 8.0, (b.top + b.bottom) / 2.0)
}

fn apply_btn(s: &Shot) -> &probe::Seen {
    s.btns
        .iter()
        .find(|b| b.primary)
        .unwrap_or_else(|| panic!("no primary button drawn: {:?}", s.btns))
}

fn reject_btn<'a>(s: &'a Shot, cat: &ddc_ui::Catalog) -> &'a probe::Seen {
    let label = cat.t("ai.plan.reject");
    s.btns
        .iter()
        .find(|b| b.label == label)
        .unwrap_or_else(|| panic!("no {label} button: {:?}", s.btns))
}

fn text_pos(s: &Shot, text: &str) -> egui::Pos2 {
    s.texts
        .iter()
        .find(|(t, _)| t == text)
        .unwrap_or_else(|| panic!("{text} not drawn: {:?}", s.texts))
        .1
        .center()
}

fn drawn(s: &Shot, text: &str) -> bool {
    s.texts.iter().any(|(t, _)| t == text)
}

/// The count chips in the title row, left to right: number and the spot to hover.
fn chips(s: &Shot) -> Vec<(String, egui::Pos2)> {
    let nums: Vec<&(String, egui::Rect)> = s
        .texts
        .iter()
        .filter(|(t, _)| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let top = nums.iter().map(|(_, r)| r.top()).fold(f32::MAX, f32::min);
    let mut row: Vec<(String, egui::Pos2)> = nums
        .into_iter()
        .filter(|(_, r)| r.top() < top + 4.0)
        .map(|(t, r)| (t.clone(), r.center()))
        .collect();
    row.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
    row
}

/// Runs `body` for every card kind and language.
fn each(body: impl Fn(&mut Fx, &mut Drv, Kind, ddc_ui::Lang)) {
    for kind in KINDS {
        for lang in LANGS {
            let mut fx = Fx::new(kind);
            let mut drv = Drv::new(lang);
            body(&mut fx, &mut drv, kind, lang);
        }
    }
}

#[test]
fn apply_is_the_one_primary_and_comes_before_reject() {
    each(|fx, drv, kind, lang| {
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        let cat = drv.cat();
        let at = format!("{kind:?} {lang:?}");
        assert_eq!(s.btns.iter().filter(|b| b.primary).count(), 1, "{at}: one primary");
        let (a, r) = (apply_btn(&s), reject_btn(&s, &cat));
        assert!(a.enabled, "{at}: a fresh draft can be applied");
        assert!(!r.primary && r.enabled, "{at}: Reject is the quiet, armed one");
        let pos = |b: &probe::Seen| s.btns.iter().position(|x| x.label == b.label).unwrap();
        assert!(pos(a) < pos(r), "{at}: Apply is drawn first");
        assert!(a.right < r.right, "{at}: Apply stands left of Reject");
        assert!((a.top - r.top).abs() < 1.0, "{at}: one row");
    });
}

#[test]
fn chip_counts_are_the_drafts_ops() {
    each(|fx, drv, kind, lang| {
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        let at = format!("{kind:?} {lang:?}");
        let got: Vec<String> = chips(&s).into_iter().map(|c| c.0).collect();
        // created, then deleted; the zero Changed chip is not drawn
        assert_eq!(got, [CREATED.to_string(), DELETED.to_string()], "{at}: {:?}", s.texts);
        if kind == Kind::Proposal {
            let rows = proposal_draft::rows(&f.proposals[0].diff);
            let n = proposal_draft::counts(&rows);
            assert_eq!((n.created, n.changed, n.deleted), (CREATED, 0, DELETED), "{at}");
        }
    });
}

#[test]
fn a_hovered_chip_lights_exactly_the_affected_live_elements() {
    each(|fx, drv, kind, lang| {
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        let at = format!("{kind:?} {lang:?}");
        let c = chips(&s);
        assert_eq!(c.len(), 2, "{at}");
        let live = |hot: &Option<Vec<ElKey>>| -> BTreeSet<ElKey> {
            hot.iter()
                .flatten()
                .filter(|k| k.0 == "wall" && fx.all.contains(&k.1))
                .cloned()
                .collect()
        };
        // Created: nothing exists yet, so no live element lights
        let o = drv.hover(&fx.ed, &f, c[0].1);
        assert!(live(&o.hot).is_empty(), "{at}: created lights no live element: {:?}", o.hot);
        // Deleted: exactly the ids the draft deletes
        let o = drv.hover(&fx.ed, &f, c[1].1);
        assert_eq!(live(&o.hot), fx.gone_keys(), "{at}: deleted chip");
        let all: BTreeSet<ElKey> = o.hot.iter().flatten().cloned().collect();
        assert_eq!(all, fx.gone_keys(), "{at}: and nothing else");
        // off the chips the card lights nothing by itself
        let o = drv.hover(&fx.ed, &f, egui::pos2(410.0, 890.0));
        assert!(o.hot.is_none(), "{at}: no hover, no highlight");
        if kind == Kind::Proposal {
            let out: BTreeSet<ElKey> = fx.outline().into_iter().collect();
            assert_eq!(out, fx.gone_keys(), "{at}: the views outline the affected ids");
        }
        // clicking a chip only asks to show; it writes nothing and decides nothing
        let before = snap(&fx.ed);
        let o = drv.click(&fx.ed, &f, c[1].1);
        assert!(!o.acts.iter().any(|a| fx.is_apply(a) || fx.is_reject(a)), "{at}");
        same(&fx.ed, &before, &at);
    });
}

#[test]
fn the_model_keeps_its_revision_until_apply() {
    each(|fx, drv, kind, lang| {
        let before = snap(&fx.ed);
        let f = fx.facts();
        let at = format!("{kind:?} {lang:?}");
        let s = drv.settle(&fx.ed, &f);
        same(&fx.ed, &before, &format!("{at}: drawing"));
        let o = drv.hover(&fx.ed, &f, chips(&s)[1].1);
        same(&fx.ed, &before, &format!("{at}: hovering"));
        assert!(o.acts.is_empty(), "{at}: a frame decides nothing");
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, centre(apply_btn(&s)));
        assert!(o.acts.iter().any(|a| fx.is_apply(a)), "{at}: one click on the primary asks to apply");
        assert!(!o.acts.iter().any(|a| fx.is_reject(a)), "{at}");
        assert_eq!(o.acts.iter().filter(|a| fx.is_apply(a)).count(), 1, "{at}: once");
        same(&fx.ed, &before, &format!("{at}: the click only asks"));
    });
}

#[test]
fn reject_asks_then_leaves_model_and_outline_clean() {
    each(|fx, drv, kind, lang| {
        let before = snap(&fx.ed);
        let f = fx.facts();
        let at = format!("{kind:?} {lang:?}");
        let cat = drv.cat();
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, centre(reject_btn(&s, &cat)));
        assert!(!o.acts.iter().any(|a| fx.is_reject(a)), "{at}: the first click only asks");
        same(&fx.ed, &before, &format!("{at}: asking"));
        let s = drv.frame(&fx.ed, &f, vec![]);
        let ask = cat.plural("ai.plan.reject_ask", (DELETED + CREATED) as u64);
        assert!(drawn(&s, &ask), "{at}: the question {ask}: {:?}", s.texts);
        assert!(drawn(&s, &cat.t("ai.plan.reject_keep")), "{at}: Keep is offered");
        let o = drv.click(&fx.ed, &f, text_pos(&s, &cat.t("ai.plan.reject_confirm")));
        assert_eq!(o.acts.iter().filter(|a| fx.is_reject(a)).count(), 1, "{at}: confirmed once");
        assert!(!o.acts.iter().any(|a| fx.is_apply(a)), "{at}");
        same(&fx.ed, &before, &format!("{at}: the confirmed Reject"));
        // the session drops the draft; the next frame has no card, no chip, no outline
        fx.run_reject();
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        assert!(s.btns.is_empty(), "{at}: no buttons: {:?}", s.btns);
        assert!(chips(&s).is_empty(), "{at}: no chips");
        assert!(s.hot.is_none(), "{at}: no highlight");
        assert!(fx.outline().is_empty(), "{at}: the views outline nothing");
        same(&fx.ed, &before, &format!("{at}: after Reject"));
    });
}

#[test]
fn keep_leaves_the_draft_and_the_model_alone() {
    each(|fx, drv, kind, lang| {
        let before = snap(&fx.ed);
        let f = fx.facts();
        let at = format!("{kind:?} {lang:?}");
        let cat = drv.cat();
        let s = drv.settle(&fx.ed, &f);
        let _ = drv.click(&fx.ed, &f, centre(reject_btn(&s, &cat)));
        let s = drv.frame(&fx.ed, &f, vec![]);
        let o = drv.click(&fx.ed, &f, text_pos(&s, &cat.t("ai.plan.reject_keep")));
        assert!(!o.acts.iter().any(|a| fx.is_reject(a)), "{at}: Keep keeps");
        let s = drv.settle(&fx.ed, &f);
        assert!(apply_btn(&s).enabled, "{at}: the draft is back and armed");
        assert_eq!(chips(&s).len(), 2, "{at}");
        same(&fx.ed, &before, &at);
    });
}

/// A plan that outlived a cleared log with no connection: the whole panel, clicked for real.
#[test]
fn not_connected_apply_works_and_only_it_writes() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Plan);
        let mut drv = Drv::not_connected(lang);
        let before = snap(&fx.ed);
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        assert!(s.acts.is_empty(), "{lang:?}: a frame without input acted");
        same(&fx.ed, &before, "drawing the plan");
        let b = apply_btn(&s);
        assert!(b.enabled, "{lang:?}: Apply is armed: {:?}", s.btns);
        let o = drv.click(&fx.ed, &f, centre(b));
        let n = o.acts.iter().filter(|a| matches!(a, Action::Apply)).count();
        assert_eq!(n, 1, "{lang:?}: Apply once");
        assert!(!o.acts.iter().any(|a| matches!(a, Action::Reject)), "{lang:?}");
        same(&fx.ed, &before, "the click only asks");
        fx.run_apply().unwrap_or_else(|e| panic!("{lang:?}: {e}"));
        assert_eq!(fx.ed.undo_len(), before.undo + 1, "{lang:?}: one undo step");
        assert!(wall_ids(&fx.ed).iter().all(|w| !fx.gone.contains(w)), "{lang:?}: applied");
    }
}

#[test]
fn not_connected_reject_works_and_leaves_the_model_alone() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Plan);
        let mut drv = Drv::not_connected(lang);
        let cat = drv.cat();
        let before = snap(&fx.ed);
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, centre(reject_btn(&s, &cat)));
        assert!(!o.acts.iter().any(|a| matches!(a, Action::Reject)), "{lang:?}: it asks first");
        // the bar under the dialog grows with the question: let it settle before aiming
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, text_pos(&s, &cat.t("ai.plan.reject_confirm")));
        let n = o.acts.iter().filter(|a| matches!(a, Action::Reject)).count();
        assert_eq!(n, 1, "{lang:?}: confirmed once");
        assert!(!o.acts.iter().any(|a| matches!(a, Action::Apply)), "{lang:?}");
        same(&fx.ed, &before, "the confirmed Reject");
        // the session dropped the draft: back to the connect card alone
        fx.run_reject();
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        assert!(s.btns.iter().all(|b| b.label != cat.t("ai.plan.reject")), "{lang:?}");
        assert_eq!(apply_btn(&s).label, cat.t("ai.state.no_profile.action"), "{lang:?}");
        same(&fx.ed, &before, "after Reject");
    }
}

/// What a panel with no connection and no draft draws: the primary connects, no card of a draft.
fn only_the_connect_card(s: &Shot, cat: &ddc_ui::Catalog, at: &str) {
    let connect = cat.t("ai.state.no_profile.action");
    let primaries: Vec<_> = s.btns.iter().filter(|b| b.primary).collect();
    assert_eq!(primaries.len(), 1, "{at}: {primaries:?}");
    assert_eq!(primaries[0].label, connect, "{at}: the primary connects");
    assert!(primaries[0].enabled, "{at}: connect is enabled");
    assert!(!drawn(s, &cat.t("ai.proposal.title")), "{at}: no proposal card: {:?}", s.texts);
    assert!(s.btns.iter().all(|b| b.label != cat.t("ai.plan.reject")), "{at}: {:?}", s.btns);
    assert!(s.btns.iter().all(|b| b.label != cat.t("ai.common.send")), "{at}: no Send");
    assert!(chips(s).is_empty(), "{at}: no chips");
    assert!(drawn(s, &cat.t(chat::CONNECT_EXAMPLES[0])), "{at}: the examples: {:?}", s.texts);
}

/// An agent's proposal with no connection and an empty log, the whole panel clicked for real:
/// the card reads as usual, the way to connect stays quiet, and only Apply writes.
#[test]
fn not_connected_proposal_shows_its_card_and_only_apply_writes() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Proposal);
        let mut drv = Drv::not_connected(lang);
        let cat = drv.cat();
        let before = snap(&fx.ed);
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        assert!(s.acts.is_empty(), "{lang:?}: a frame without input acted");
        same(&fx.ed, &before, "drawing the proposal");
        assert!(drawn(&s, &cat.t("ai.proposal.title")), "{lang:?}: the card: {:?}", s.texts);
        let c = chips(&s);
        let nums: Vec<&str> = c.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(nums, [CREATED.to_string(), DELETED.to_string()], "{lang:?}: the counts");
        let primaries: Vec<_> = s.btns.iter().filter(|b| b.primary).collect();
        assert_eq!(primaries.len(), 1, "{lang:?}: one primary {primaries:?}");
        assert_eq!(primaries[0].label, cat.t("ai.items.apply_none"), "{lang:?}: Apply owns it");
        assert!(primaries[0].enabled, "{lang:?}: Apply is armed");
        assert!(reject_btn(&s, &cat).enabled, "{lang:?}: Reject is armed");
        assert!(s.btns.iter().all(|b| b.label != cat.t("ai.common.send")), "{lang:?}: no Send");
        assert!(!drawn(&s, &cat.t("ai.composer.hint_off")), "{lang:?}: no dead composer");
        // the chips still light the affected elements
        let o = drv.hover(&fx.ed, &f, c[1].1);
        let lit: BTreeSet<ElKey> = o.hot.expect("the chip is hovered").into_iter().collect();
        assert_eq!(lit, fx.gone_keys(), "{lang:?}: the deleted walls");
        same(&fx.ed, &before, "hovering");
        // the way to connect still works and decides nothing
        let label = cat.t("ai.state.no_profile.action");
        let s = drv.settle(&fx.ed, &f);
        let b = s.btns.iter().find(|b| b.label == label);
        let b = b.unwrap_or_else(|| panic!("{lang:?}: no connect button: {:?}", s.btns));
        assert!(b.enabled && !b.primary, "{lang:?}: connect is quiet: {b:?}");
        let o = drv.click(&fx.ed, &f, centre(b));
        let n = o.acts.iter().filter(|a| matches!(a, Action::OpenSettings)).count();
        assert_eq!(n, 1, "{lang:?}: connect once");
        assert!(!o.acts.iter().any(|a| fx.is_apply(a) || fx.is_reject(a)), "{lang:?}");
        same(&fx.ed, &before, "connecting");
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, centre(apply_btn(&s)));
        assert_eq!(o.acts.iter().filter(|a| fx.is_apply(a)).count(), 1, "{lang:?}: Apply once");
        assert!(!o.acts.iter().any(|a| fx.is_reject(a)), "{lang:?}");
        same(&fx.ed, &before, "the click only asks");
        fx.run_apply().unwrap_or_else(|e| panic!("{lang:?}: {e}"));
        assert_eq!(fx.ed.undo_len(), before.undo + 1, "{lang:?}: one undo step");
        assert!(wall_ids(&fx.ed).iter().all(|w| !fx.gone.contains(w)), "{lang:?}: applied");
    }
}

#[test]
fn not_connected_proposal_reject_asks_and_leaves_the_connect_card() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Proposal);
        let mut drv = Drv::not_connected(lang);
        let cat = drv.cat();
        let before = snap(&fx.ed);
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, centre(reject_btn(&s, &cat)));
        assert!(!o.acts.iter().any(|a| fx.is_reject(a)), "{lang:?}: it asks first");
        let s = drv.settle(&fx.ed, &f);
        let ask = cat.plural("ai.plan.reject_ask", (DELETED + CREATED) as u64);
        assert!(drawn(&s, &ask), "{lang:?}: the question {ask}: {:?}", s.texts);
        let o = drv.click(&fx.ed, &f, text_pos(&s, &cat.t("ai.plan.reject_confirm")));
        assert_eq!(o.acts.iter().filter(|a| fx.is_reject(a)).count(), 1, "{lang:?}: once");
        assert!(!o.acts.iter().any(|a| fx.is_apply(a)), "{lang:?}");
        same(&fx.ed, &before, "the confirmed Reject");
        fx.run_reject();
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        only_the_connect_card(&s, &cat, &format!("{lang:?}: after Reject"));
        assert!(fx.outline().is_empty(), "{lang:?}: the views outline nothing");
        same(&fx.ed, &before, "after Reject");
    }
}

/// A draft the model moved past stays on screen with its reason and can still be dropped.
#[test]
fn not_connected_stale_proposal_says_why_and_keeps_reject() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Proposal);
        let mut drv = Drv::not_connected(lang);
        let cat = drv.cat();
        fx.ed.model.add_level("moved", 3.0).unwrap();
        let moved = snap(&fx.ed);
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        assert!(drawn(&s, &cat.t("ai.proposal.stale")), "{lang:?}: the reason: {:?}", s.texts);
        let a = apply_btn(&s);
        assert!(!a.enabled, "{lang:?}: Apply is greyed");
        assert!(reject_btn(&s, &cat).enabled, "{lang:?}: Reject stays armed");
        let o = drv.click(&fx.ed, &f, centre(a));
        assert!(!o.acts.iter().any(|x| fx.is_apply(x)), "{lang:?}: a greyed Apply asks nothing");
        same(&fx.ed, &moved, "the greyed Apply");
    }
}

/// A plan and a proposal at once with no connection: both keep their own armed Apply.
#[test]
fn not_connected_plan_and_proposal_both_keep_their_actions() {
    for lang in LANGS {
        let fx = Fx::new(Kind::Proposal);
        let mut drv = Drv::not_connected(lang);
        let cat = drv.cat();
        let before = snap(&fx.ed);
        let f = Facts {
            plan: Some(fx.make_plan()),
            has_session: true,
            can_apply: true,
            plan_rev: 1,
            proposals: fx.cards.cards(),
            ..Facts::default()
        };
        let s = drv.settle(&fx.ed, &f);
        assert!(s.acts.is_empty(), "{lang:?}: a frame without input acted");
        assert!(drawn(&s, &cat.t("ai.proposal.title")), "{lang:?}: the card: {:?}", s.texts);
        let armed = s.btns.iter().filter(|b| b.primary && b.enabled).count();
        assert_eq!(armed, 2, "{lang:?}: both Apply: {:?}", s.btns);
        let rejects = s.btns.iter().filter(|b| b.label == cat.t("ai.plan.reject")).count();
        assert_eq!(rejects, 2, "{lang:?}: both Reject: {:?}", s.btns);
        same(&fx.ed, &before, "drawing both");
    }
}

#[test]
fn not_connected_without_a_proposal_shows_only_the_connect_card() {
    for lang in LANGS {
        let fx = Fx::new(Kind::Plan);
        let mut drv = Drv::not_connected(lang);
        let cat = drv.cat();
        let before = snap(&fx.ed);
        let s = drv.settle(&fx.ed, &Facts::default());
        assert!(s.acts.is_empty(), "{lang:?}: a frame without input acted");
        only_the_connect_card(&s, &cat, &format!("{lang:?}"));
        same(&fx.ed, &before, "drawing the connect card");
    }
}

#[test]
fn apply_is_exactly_one_undo_step() {
    each(|fx, _drv, kind, lang| {
        let before = snap(&fx.ed);
        let at = format!("{kind:?} {lang:?}");
        fx.run_apply().unwrap_or_else(|e| panic!("{at}: {e}"));
        assert_eq!(fx.ed.undo_len(), before.undo + 1, "{at}: one undo step");
        assert_ne!(fx.ed.model.change_head(), before.head, "{at}: the revision moved");
        let now = wall_ids(&fx.ed);
        assert!(fx.gone.iter().all(|g| !now.contains(g)), "{at}: the walls are gone");
        assert!(now.contains(&fx.all[DELETED]), "{at}: the untouched wall stays");
        assert_eq!(now.len(), fx.all.len() - DELETED + CREATED, "{at}: the new walls");
        assert!(fx.run_apply().is_err(), "{at}: a draft applies once");
        assert_eq!(fx.ed.undo_len(), before.undo + 1, "{at}: the second Apply wrote nothing");
        fx.ed.undo();
        assert_eq!(fx.ed.undo_len(), before.undo, "{at}: one Ctrl+Z takes it all back");
        assert_eq!(wall_ids(&fx.ed), before.walls, "{at}: the walls are back");
    });
}

/// Both cards read the live head through `plan_card::draft_stale`, so one inconsistent Facts
/// never arms a stale Apply.
#[test]
fn a_stale_plan_greys_apply_even_if_the_facts_say_armed() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Plan);
        let mut drv = Drv::new(lang);
        fx.ed.model.add_level("moved", 3.0).unwrap();
        let mut f = fx.facts();
        assert!(f.stale);
        f.can_apply = true;
        let s = drv.settle(&fx.ed, &f);
        assert!(!apply_btn(&s).enabled, "{lang:?}: Apply is greyed while stale");
    }
}

/// The session has not noticed yet (stale false, can_apply true), but the model moved past the
/// plan's base head: the card checks the head itself, like the proposal card.
#[test]
fn a_moved_model_greys_plan_apply_with_the_stale_reason_before_the_session_notices() {
    for lang in LANGS {
        let mut fx = Fx::new(Kind::Plan);
        let mut drv = Drv::new(lang);
        let cat = drv.cat();
        fx.ed.model.add_level("moved", 3.0).unwrap();
        let moved = snap(&fx.ed);
        let mut f = fx.facts();
        f.stale = false;
        f.can_apply = true;
        let s = drv.settle(&fx.ed, &f);
        assert!(drawn(&s, &cat.t("ai.plan.stale")), "{lang:?}: the reason is shown: {:?}", s.texts);
        let a = apply_btn(&s);
        assert!(!a.enabled, "{lang:?}: Apply is greyed");
        let o = drv.click(&fx.ed, &f, centre(a));
        assert!(!o.acts.iter().any(|x| fx.is_apply(x)), "{lang:?}: a greyed Apply asks for nothing");
        // the executor stays the second guard
        assert_eq!(fx.run_apply(), Err(format!("{:?}", AiError::Stale)), "{lang:?}");
        same(&fx.ed, &moved, &format!("{lang:?}: refused Apply"));
        // Reject is untouched by the stale state
        let s = drv.settle(&fx.ed, &f);
        let r = reject_btn(&s, &cat);
        assert!(r.enabled, "{lang:?}: Reject stays armed");
        let o = drv.click(&fx.ed, &f, centre(r));
        let o = if o.acts.iter().any(|x| fx.is_reject(x)) {
            o
        } else {
            let s = drv.frame(&fx.ed, &f, vec![]);
            drv.click(&fx.ed, &f, text_pos(&s, &cat.t("ai.plan.reject_confirm")))
        };
        assert!(o.acts.iter().any(|x| fx.is_reject(x)), "{lang:?}: a stale plan can be rejected");
        same(&fx.ed, &moved, &format!("{lang:?}: Reject while stale"));
    }
}

/// A fresh draft of either kind is armed and shows no stale reason.
#[test]
fn a_fresh_draft_is_armed_without_a_stale_reason() {
    each(|fx, drv, kind, lang| {
        let at = format!("{kind:?} {lang:?}");
        let cat = drv.cat();
        let f = fx.facts();
        assert!(!f.stale, "{at}");
        let s = drv.settle(&fx.ed, &f);
        assert!(apply_btn(&s).enabled, "{at}: Apply is armed");
        for key in ["ai.plan.stale", "ai.proposal.stale"] {
            assert!(!drawn(&s, &cat.t(key)), "{at}: no {key} note");
        }
        let o = drv.click(&fx.ed, &f, centre(apply_btn(&s)));
        assert!(o.acts.iter().any(|x| fx.is_apply(x)), "{at}: an armed Apply asks for the run");
    });
}

#[test]
fn a_stale_draft_cannot_apply() {
    each(|fx, drv, kind, lang| {
        let at = format!("{kind:?} {lang:?}");
        let cat = drv.cat();
        fx.ed.model.add_level("moved", 3.0).unwrap();
        let moved = snap(&fx.ed);
        let f = fx.facts();
        let s = drv.settle(&fx.ed, &f);
        let why = match kind {
            Kind::Plan => cat.t("ai.plan.stale"),
            Kind::Proposal => cat.t("ai.proposal.stale"),
        };
        assert!(drawn(&s, &why), "{at}: the reason is shown: {:?}", s.texts);
        let a = apply_btn(&s);
        assert!(!a.enabled, "{at}: Apply is greyed");
        let o = drv.click(&fx.ed, &f, centre(a));
        assert!(!o.acts.iter().any(|x| fx.is_apply(x)), "{at}: a greyed Apply asks for nothing");
        // the executor refuses on its own too, and writes nothing
        let err = fx.run_apply().expect_err("a stale draft is refused");
        if kind == Kind::Plan {
            assert_eq!(err, format!("{:?}", AiError::Stale), "{at}");
        }
        same(&fx.ed, &moved, &format!("{at}: refused Apply"));
        // it can still be dropped
        let s = drv.settle(&fx.ed, &f);
        let o = drv.click(&fx.ed, &f, centre(reject_btn(&s, &cat)));
        let o = if o.acts.iter().any(|x| fx.is_reject(x)) {
            o
        } else {
            let s = drv.frame(&fx.ed, &f, vec![]);
            drv.click(&fx.ed, &f, text_pos(&s, &cat.t("ai.plan.reject_confirm")))
        };
        assert!(o.acts.iter().any(|x| fx.is_reject(x)), "{at}: a stale draft can be rejected");
        same(&fx.ed, &moved, &format!("{at}: Reject while stale"));
    });
}

fn lab(c: egui::Color32) -> [f32; 3] {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s > 0.04045 { ((s + 0.055) / 1.055).powf(2.4) } else { s / 12.92 }
    };
    let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let t = |v: f32| if v > 0.008856 { v.cbrt() } else { 7.787 * v + 16.0 / 116.0 };
    [116.0 * t(y) - 16.0, 500.0 * (t(x) - t(y)), 200.0 * (t(y) - t(z))]
}

#[test]
fn ai_accent_is_told_from_preview_in_both_modes() {
    for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
        let th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
        let (a, p) = (lab(th.c("ai.accent")), lab(th.c("preview")));
        let de = ((a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2) + (a[2] - p[2]).powi(2)).sqrt();
        assert!(de >= 10.0, "{mode:?}: ai.accent vs preview dE {de:.1} < 10");
    }
}

#[test]
fn own_bubble_is_12_with_a_4_author_corner() {
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
    let r = chat::bubble_radius(&th);
    assert_eq!((r.nw, r.ne, r.sw, r.se), (12, 12, 12, 4));
}
