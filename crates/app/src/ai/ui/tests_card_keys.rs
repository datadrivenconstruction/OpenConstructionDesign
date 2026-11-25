// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The plan card's keys and inline values, drawn headless beside a stand-in canvas: Ctrl+Enter
//! and Esc follow the person into the view, a text field keeps them, the canvas keeps its own
//! Esc, a corrected value becomes a draft edit (never a model write), refused text stays with
//! its reason, and a rebased card marks its rows and says "Apply updated plan".
//! Fonts and the catalog are explicit; the global UI language is never touched.

use super::kit::probe;
use super::plan_delta::PlanDelta;
use super::{chat, Action, Cx, Facts, PanelUi};
use crate::ai::plan::{CompiledPlan, NewElem, PlanItem, PlanOp, PlanView};
use crate::ai::validate::Target;
use crate::ai::value_edit::{self, Field, Num};
use crate::editor::{Editor, Sel};
use crate::units::DecimalMark;
use crate::wall_tool::Mode;
use std::sync::Arc;

const LANGS: [ddc_ui::Lang; 2] = [ddc_ui::Lang::En, ddc_ui::Lang::Ru];
const SCREEN: egui::Vec2 = egui::vec2(1100.0, 900.0);
/// An empty spot of the stand-in canvas, left of the panel and below its text field.
const CANVAS: egui::Pos2 = egui::pos2(200.0, 600.0);

fn wall_item(id: u32) -> PlanItem {
    PlanItem {
        id,
        title: format!("New wall {id}"),
        why: "Close the room".to_string(),
        op: PlanOp::Create {
            slot0: id,
            items: vec![NewElem::Wall {
                level: 1,
                a: (0.0, 5.0),
                b: (6.0, 5.0),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                wtype: None,
            }],
        },
        checked: true,
        depends_on: vec![],
        actor: "person".to_string(),
        conflict: None,
        sets: Default::default(),
    }
}

fn plan(ed: &Editor) -> Arc<PlanView> {
    plan_of(ed, vec![wall_item(1), wall_item(2)])
}

fn plan_of(ed: &Editor, items: Vec<PlanItem>) -> Arc<PlanView> {
    Arc::new(PlanView {
        items,
        diff: vec![],
        secondary: 0,
        qty: vec![],
        issues: vec![],
        main_issues: vec![],
        preview: Arc::new(crate::ai::preview::PreviewSet::default()),
        compiled: CompiledPlan {
            base_head: ed.model.change_head(),
            steps: Arc::new(vec![]),
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

fn facts(p: &Arc<PlanView>) -> Facts {
    Facts {
        plan: Some(p.clone()),
        has_session: true,
        can_apply: true,
        plan_rev: 1,
        ..Facts::default()
    }
}

/// The whole panel with a rebased two-wall draft: value fields under the rows, the rebase line
/// and "Apply updated plan", for the layout contract.
pub(crate) fn layout_probe(lang: ddc_ui::Lang) -> crate::layout_contract::Probe {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let p = plan(&ed);
    let f = Facts {
        rebased: Some(PlanDelta {
            changed: vec![(2, p.items[1].title.clone())],
            ..PlanDelta::default()
        }),
        ..facts(&p)
    };
    let mut st = PanelUi::default();
    st.expanded.insert(1);
    let mut icons = ddc_ui::icons::IconCache::default();
    let mut hot = None;
    crate::layout_contract::probe("ai.card", |ctx| {
        let cx = Cx {
            th: &th,
            cat: &cat,
            ed: &ed,
            view3d: false,
            palette_open: false,
        };
        let mut acts: Vec<Action> = vec![];
        let size = egui::vec2(380.0, 700.0);
        crate::ai::ui::tests_states::host(ctx, "ai.card", size, |ui| {
            chat::panel(ui, &mut st, &cx, &mut icons, &f, &mut acts, &mut hot);
        });
    })
}

/// The card once a request ended with the agent's length beside the person's held one: both
/// values, the two picks and Apply held off, for the layout contract.
pub(crate) fn layout_probe_choice(lang: ddc_ui::Lang) -> crate::layout_contract::Probe {
    let ed = Editor::new_in_memory().unwrap();
    let cat = ddc_ui::Catalog::new(lang);
    let th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let (len, base) = length_of(&ed, &plan(&ed), lang);
    let p = lengthened(&ed);
    let f = facts(&p);
    let mut st = PanelUi::default();
    let h = super::value_row::Held {
        value: value_edit::Value::Metres(7.0),
        mine: "7".to_string(),
        base,
        title: p.items[0].title.clone(),
        label: "len".to_string(),
    };
    st.values.hold(1, len.field, h);
    let mut icons = ddc_ui::icons::IconCache::default();
    let mut hot = None;
    crate::layout_contract::probe("ai.card", |ctx| {
        let cx = Cx {
            th: &th,
            cat: &cat,
            ed: &ed,
            view3d: false,
            palette_open: false,
        };
        let mut acts: Vec<Action> = vec![];
        let size = egui::vec2(380.0, 700.0);
        crate::ai::ui::tests_states::host(ctx, "ai.card", size, |ui| {
            chat::panel(ui, &mut st, &cx, &mut icons, &f, &mut acts, &mut hot);
        });
    })
}

struct Shot {
    texts: Vec<(String, egui::Rect)>,
    acts: Vec<Action>,
    btns: Vec<probe::Seen>,
    /// Filled circles drawn: the rows' accent marks among them.
    dots: usize,
    /// The text field on the canvas (an annotation being typed, outside the panel).
    note: egui::Rect,
}

fn collect(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>, dots: &mut usize) {
    match s {
        egui::Shape::Text(t) => {
            let text = t.galley.text().to_string();
            if !text.trim().is_empty() {
                out.push((text, t.galley.rect.translate(t.pos.to_vec2())));
            }
        }
        egui::Shape::Circle(c) if c.fill != egui::Color32::TRANSPARENT => *dots += 1,
        egui::Shape::Vec(v) => v.iter().for_each(|x| collect(x, out, dots)),
        _ => {}
    }
}

/// The whole panel docked right of a stand-in canvas: explicit fonts and catalog.
struct Drv {
    ctx: egui::Context,
    st: PanelUi,
    lang: ddc_ui::Lang,
    note: String,
}

impl Drv {
    fn new(lang: ddc_ui::Lang) -> Drv {
        // named font weights bind from the pass after install: a bare first frame would panic
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        Drv {
            ctx,
            st: PanelUi::default(),
            lang,
            note: String::new(),
        }
    }

    fn cat(&self) -> ddc_ui::Catalog {
        ddc_ui::Catalog::new(self.lang)
    }

    fn run(
        &mut self,
        ed: &Editor,
        f: &Facts,
        events: Vec<egui::Event>,
        m: egui::Modifiers,
    ) -> Shot {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = ddc_ui::Catalog::new(self.lang);
        let mut acts: Vec<Action> = Vec::new();
        let mut hot = None;
        let _ = probe::take();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            modifiers: m,
            ..egui::RawInput::default()
        };
        let mut icons = ddc_ui::icons::IconCache::default();
        let (st, note) = (&mut self.st, &mut self.note);
        let mut note_rect = egui::Rect::NOTHING;
        let out = self.ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            let cx = Cx {
                th: &th,
                cat: &cat,
                ed,
                view3d: false,
                palette_open: false,
            };
            egui::SidePanel::right("ai.test.panel")
                .exact_width(420.0)
                .resizable(false)
                .show(ctx, |ui| chat::panel(ui, st, &cx, &mut icons, f, &mut acts, &mut hot));
            egui::CentralPanel::default().show(ctx, |ui| {
                let field = egui::TextEdit::singleline(&mut *note).id_salt("canvas.note");
                note_rect = ui.add(field).rect;
                let rest = ui.available_rect_before_wrap();
                let _ = ui.allocate_rect(rest, egui::Sense::click_and_drag());
            });
        });
        let (mut texts, mut dots) = (Vec::new(), 0);
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts, &mut dots));
        Shot {
            texts,
            acts,
            btns: probe::take(),
            dots,
            note: note_rect,
        }
    }

    fn frame(&mut self, ed: &Editor, f: &Facts) -> Shot {
        self.run(ed, f, vec![], egui::Modifiers::NONE)
    }

    fn settle(&mut self, ed: &Editor, f: &Facts) -> Shot {
        let _ = self.frame(ed, f);
        let _ = self.frame(ed, f);
        self.frame(ed, f)
    }

    fn click(&mut self, ed: &Editor, f: &Facts, pos: egui::Pos2) -> Shot {
        let btn = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let no = egui::Modifiers::NONE;
        let _ = self.run(ed, f, vec![egui::Event::PointerMoved(pos)], no);
        let _ = self.run(ed, f, vec![btn(true)], no);
        let _ = self.run(ed, f, vec![btn(false)], no);
        self.frame(ed, f)
    }

    /// A click whose release frame is returned: what a button pushed on its click is in it.
    fn tap(&mut self, ed: &Editor, f: &Facts, pos: egui::Pos2) -> Shot {
        let btn = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let no = egui::Modifiers::NONE;
        let _ = self.run(ed, f, vec![egui::Event::PointerMoved(pos)], no);
        let _ = self.run(ed, f, vec![btn(true)], no);
        self.run(ed, f, vec![btn(false)], no)
    }

    /// One key press with its modifiers held for the frame; the frame that saw it.
    fn key(&mut self, ed: &Editor, f: &Facts, key: egui::Key, m: egui::Modifiers) -> Shot {
        let ev = egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        self.run(ed, f, vec![ev], m)
    }

    fn type_text(&mut self, ed: &Editor, f: &Facts, text: &str) {
        let _ = self.key(ed, f, egui::Key::A, egui::Modifiers::COMMAND);
        let ev = egui::Event::Text(text.to_string());
        let _ = self.run(ed, f, vec![ev], egui::Modifiers::NONE);
    }
}

fn applies(s: &Shot) -> usize {
    s.acts.iter().filter(|a| matches!(a, Action::Apply)).count()
}

fn cancels(s: &Shot) -> usize {
    s.acts.iter().filter(|a| matches!(a, Action::Cancel)).count()
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

fn apply_label(s: &Shot) -> String {
    let b = s.btns.iter().find(|b| b.primary);
    b.unwrap_or_else(|| panic!("no primary button: {:?}", s.btns)).label.clone()
}

#[test]
fn ctrl_enter_applies_after_a_click_into_the_view() {
    for lang in LANGS {
        let ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let f = facts(&p);
        let head = ed.model.change_head();
        let _ = drv.settle(&ed, &f);
        let _ = drv.click(&ed, &f, CANVAS);
        let s = drv.key(&ed, &f, egui::Key::Enter, egui::Modifiers::COMMAND);
        assert_eq!(applies(&s), 1, "{lang:?}: Ctrl+Enter from the view applies once");
        // a bare Enter or Shift+Ctrl+Enter is not the chord
        let s = drv.key(&ed, &f, egui::Key::Enter, egui::Modifiers::NONE);
        assert_eq!(applies(&s), 0, "{lang:?}: bare Enter");
        let m = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        let s = drv.key(&ed, &f, egui::Key::Enter, m);
        assert_eq!(applies(&s), 0, "{lang:?}: Ctrl+Shift+Enter");
        assert_eq!(ed.model.change_head(), head, "{lang:?}: the panel never writes the model");
    }
}

#[test]
fn esc_stops_a_running_request_from_the_view_unless_the_canvas_needs_it() {
    for lang in LANGS {
        let mut ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let f = Facts {
            busy: true,
            ..facts(&p)
        };
        let _ = drv.settle(&ed, &f);
        let _ = drv.click(&ed, &f, CANVAS);
        // a drawing tool is active: Esc leaves the tool first
        assert_ne!(ed.wall_mode(), Mode::Select, "the editor opens with a drawing tool");
        let s = drv.key(&ed, &f, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(cancels(&s), 0, "{lang:?}: the drawing tool keeps its Esc");
        ed.set_mode(Mode::Select);
        let s = drv.key(&ed, &f, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(cancels(&s), 1, "{lang:?}: Esc from the view stops the request");
        // a selection on the canvas: Esc clears it there first
        ed.selection = vec![Sel::Wall(1)];
        let s = drv.key(&ed, &f, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(cancels(&s), 0, "{lang:?}: the canvas keeps its Esc");
        ed.selection.clear();
        // any other canvas tool (dimension, tag, section, 3D edit) keeps its Esc too
        drv.st.canvas_tool = true;
        let s = drv.key(&ed, &f, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(cancels(&s), 0, "{lang:?}: an active canvas tool keeps its Esc");
        drv.st.canvas_tool = false;
        // nothing running: Esc is not taken from the view
        let idle = facts(&p);
        let s = drv.key(&ed, &idle, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(cancels(&s), 0, "{lang:?}: no request to stop");
    }
}

#[test]
fn a_text_field_outside_the_panel_keeps_the_keys() {
    for lang in LANGS {
        let ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let f = Facts {
            busy: true,
            ..facts(&p)
        };
        let s = drv.settle(&ed, &f);
        let _ = drv.click(&ed, &f, s.note.center());
        assert!(drv.ctx.wants_keyboard_input(), "{lang:?}: the note field has the keyboard");
        let s = drv.key(&ed, &f, egui::Key::Enter, egui::Modifiers::COMMAND);
        assert_eq!(applies(&s), 0, "{lang:?}: Ctrl+Enter stays with the text field");
        // a one-line field lets go on Enter; back into it before Esc
        let _ = drv.click(&ed, &f, s.note.center());
        assert!(drv.ctx.wants_keyboard_input(), "{lang:?}: typing again");
        let s = drv.key(&ed, &f, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(cancels(&s), 0, "{lang:?}: Esc stays with the text field");
    }
}

#[test]
fn the_stop_and_apply_tooltips_name_the_keys_that_work() {
    for lang in LANGS {
        let cat = ddc_ui::Catalog::new(lang);
        let stop = super::logic::STOP_KEYS;
        assert!(stop.contains("Esc") && stop.contains("Ctrl+Alt+."), "{stop}");
        let anywhere = cat.t("ai.plan.keys_anywhere");
        assert!(anywhere.contains("Ctrl+Enter") && anywhere.contains("Esc"), "{anywhere}");
        let ru = anywhere.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        assert_eq!(ru, lang == ddc_ui::Lang::Ru, "{lang:?}: {anywhere}");
    }
}

#[test]
fn a_value_on_the_card_is_corrected_in_the_draft_or_refused_with_its_reason() {
    for lang in LANGS {
        let ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let f = facts(&p);
        let cat = drv.cat();
        let head = ed.model.change_head();
        let all = value_edit::editables(&ed, &p.items, &p.items[0]);
        let len = all
            .iter()
            .find(|e| matches!(e.field, Field::Create { index: 0, num: Num::Length }))
            .expect("a wall's length is editable");
        let units = ed.tool.units;
        let shown = value_edit::shown(len, units, DecimalMark::for_lang(lang));
        let s = drv.settle(&ed, &f);
        let _ = drv.click(&ed, &f, text_pos(&s, &shown));
        // valid: one draft edit, the model untouched
        drv.type_text(&ed, &f, "7");
        let s = drv.key(&ed, &f, egui::Key::Enter, egui::Modifiers::NONE);
        let want = value_edit::check(len, "7", units, &cat).expect("7 is a length");
        let sent: Vec<_> = s
            .acts
            .iter()
            .filter_map(|a| match a {
                Action::SetValue { item, field, value } => Some((*item, field.clone(), value)),
                _ => None,
            })
            .collect();
        assert_eq!(sent.len(), 1, "{lang:?}: one draft edit");
        assert_eq!(sent[0].0, 1, "{lang:?}: the first item");
        assert_eq!(sent[0].1, len.field, "{lang:?}: the length");
        assert_eq!(*sent[0].2, want, "{lang:?}: read as the properties panel reads it");
        assert_eq!(applies(&s), 0, "{lang:?}: Enter in the field is not Apply");
        // invalid: nothing sent, the reason drawn in the person's language
        let bad = "abc";
        let why = value_edit::check(len, bad, units, &cat).expect_err("not a length");
        assert!(!why.contains("ai.value."), "{lang:?}: {why}");
        let s = drv.settle(&ed, &f);
        let _ = drv.click(&ed, &f, text_pos(&s, &shown));
        drv.type_text(&ed, &f, bad);
        let s = drv.key(&ed, &f, egui::Key::Enter, egui::Modifiers::NONE);
        assert!(
            !s.acts.iter().any(|a| matches!(a, Action::SetValue { .. })),
            "{lang:?}: refused text is not sent"
        );
        let s = drv.frame(&ed, &f);
        assert!(drawn(&s, &why), "{lang:?}: {why} drawn: {:?}", s.texts);
        assert_eq!(ed.model.change_head(), head, "{lang:?}: the model keeps its revision");
    }
}

#[test]
fn a_rebased_card_marks_its_rows_and_says_apply_updated_plan() {
    for lang in LANGS {
        let ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let cat = drv.cat();
        let plain = drv.settle(&ed, &facts(&p));
        let d = PlanDelta {
            changed: vec![(2, p.items[1].title.clone())],
            ..PlanDelta::default()
        };
        let f = Facts {
            rebased: Some(d.clone()),
            ..facts(&p)
        };
        let s = drv.settle(&ed, &f);
        assert_eq!(apply_label(&s), cat.t("ai.plan.apply_updated"), "{lang:?}");
        assert_ne!(apply_label(&plain), cat.t("ai.plan.apply_updated"), "{lang:?}");
        let what = d.note().counts(&cat);
        let line = cat.tf("ai.plan.rebased", &[("what", &what)]);
        assert!(drawn(&s, &line), "{lang:?}: {line} drawn: {:?}", s.texts);
        assert_eq!(s.dots, plain.dots + 1, "{lang:?}: exactly the changed row is marked");
    }
}

fn sent_values(s: &Shot) -> Vec<(u32, value_edit::Value)> {
    s.acts
        .iter()
        .filter_map(|a| match a {
            Action::SetValue { item, value, .. } => Some((*item, value.clone())),
            _ => None,
        })
        .collect()
}

/// The first wall's length as the card shows it.
fn length_of(ed: &Editor, p: &PlanView, lang: ddc_ui::Lang) -> (value_edit::Editable, String) {
    let all = value_edit::editables(ed, &p.items, &p.items[0]);
    let len = all
        .into_iter()
        .find(|e| matches!(e.field, Field::Create { index: 0, num: Num::Length }))
        .expect("a wall's length is editable");
    let shown = value_edit::shown(&len, ed.tool.units, DecimalMark::for_lang(lang));
    (len, shown)
}

/// The same draft after the agent lengthened the first wall.
fn lengthened(ed: &Editor) -> Arc<PlanView> {
    let mut first = wall_item(1);
    if let PlanOp::Create { items, .. } = &mut first.op {
        if let NewElem::Wall { b, .. } = &mut items[0] {
            b.0 = 8.0;
        }
    }
    plan_of(ed, vec![first, wall_item(2)])
}

/// "7" typed into the first wall's length while a request runs: held, said, not sent.
fn hold_seven(drv: &mut Drv, ed: &Editor, busy: &Facts, shown: &str) {
    let lang = drv.lang;
    let cat = drv.cat();
    let s = drv.settle(ed, busy);
    let _ = drv.click(ed, busy, text_pos(&s, shown));
    drv.type_text(ed, busy, "7");
    let s = drv.key(ed, busy, egui::Key::Enter, egui::Modifiers::NONE);
    assert!(sent_values(&s).is_empty(), "{lang:?}: nothing reaches the draft mid-request");
    let s = drv.frame(ed, busy);
    let held = cat.tf("ai.value.held", &[("mine", "7")]);
    assert!(drawn(&s, &held), "{lang:?}: {held} drawn: {:?}", s.texts);
}

#[test]
fn value_edit_while_busy_is_held_and_goes_in_when_the_agent_left_the_value() {
    for lang in LANGS {
        let ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let cat = drv.cat();
        let head = ed.model.change_head();
        let (len, shown) = length_of(&ed, &p, lang);
        let busy = Facts {
            busy: true,
            ..facts(&p)
        };
        hold_seven(&mut drv, &ed, &busy, &shown);
        // the request ends with the same length on the card: the person's value goes in once
        let idle = facts(&p);
        let s = drv.frame(&ed, &idle);
        let want = value_edit::check(&len, "7", ed.tool.units, &cat).expect("7 is a length");
        assert_eq!(sent_values(&s), vec![(1, want)], "{lang:?}");
        let s = drv.frame(&ed, &idle);
        assert!(sent_values(&s).is_empty(), "{lang:?}: sent once");
        assert_eq!(ed.model.change_head(), head, "{lang:?}: the model keeps its revision");
    }
}

#[test]
fn value_edit_while_busy_shows_both_and_waits_for_a_pick_when_the_agent_changed_it() {
    for lang in LANGS {
        for keep_mine in [true, false] {
            let ed = Editor::new_in_memory().unwrap();
            let (p, mut drv) = (plan(&ed), Drv::new(lang));
            let cat = drv.cat();
            let head = ed.model.change_head();
            let (len, shown) = length_of(&ed, &p, lang);
            let busy = Facts {
                busy: true,
                ..facts(&p)
            };
            hold_seven(&mut drv, &ed, &busy, &shown);
            // the agent's card lengthened the same wall: both values shown, nothing sent
            let p2 = lengthened(&ed);
            let (_, agent) = length_of(&ed, &p2, lang);
            assert_ne!(agent, shown);
            let idle = facts(&p2);
            let s = drv.settle(&ed, &idle);
            assert!(sent_values(&s).is_empty(), "{lang:?}: no silent overwrite");
            let args = [("agent", agent.as_str()), ("mine", "7")];
            let both = cat.tf("ai.value.both", &args);
            assert!(drawn(&s, &both), "{lang:?}: {both} drawn: {:?}", s.texts);
            let apply = s.btns.iter().find(|b| b.primary).expect("Apply is drawn");
            assert!(!apply.enabled, "{lang:?}: Apply waits for the pick");
            let why = cat.t("ai.value.choose_first");
            assert!(drawn(&s, &why), "{lang:?}: {why} drawn");
            let key = if keep_mine { "ai.value.use_mine" } else { "ai.value.keep_agent" };
            let label = cat.tf(key, &args);
            let s = drv.tap(&ed, &idle, text_pos(&s, &label));
            let s2 = drv.settle(&ed, &idle);
            let sent: Vec<_> = sent_values(&s).into_iter().chain(sent_values(&s2)).collect();
            if keep_mine {
                let want = value_edit::check(&len, "7", ed.tool.units, &cat).expect("a length");
                assert_eq!(sent, vec![(1, want)], "{lang:?}: the person's value, once");
            } else {
                assert!(sent.is_empty(), "{lang:?}: the agent's value stays");
            }
            assert!(!drawn(&s2, &both), "{lang:?}: the pick closes the question");
            assert_eq!(ed.model.change_head(), head, "{lang:?}: no model change before Apply");
        }
    }
}

#[test]
fn a_held_value_whose_item_left_the_card_is_said_not_dropped() {
    for lang in LANGS {
        let ed = Editor::new_in_memory().unwrap();
        let (p, mut drv) = (plan(&ed), Drv::new(lang));
        let cat = drv.cat();
        let (len, shown) = length_of(&ed, &p, lang);
        let busy = Facts {
            busy: true,
            ..facts(&p)
        };
        hold_seven(&mut drv, &ed, &busy, &shown);
        let idle = facts(&plan_of(&ed, vec![wall_item(2)]));
        let s = drv.settle(&ed, &idle);
        assert!(sent_values(&s).is_empty(), "{lang:?}: nothing to send it to");
        let what = match &len.label {
            value_edit::Label::Key(k) => cat.t(k),
            value_edit::Label::Name(n) => n.clone(),
        };
        let args = [("mine", "7"), ("what", what.as_str()), ("title", "New wall 1")];
        let line = cat.tf("ai.value.lost", &args);
        assert!(drawn(&s, &line), "{lang:?}: {line} drawn: {:?}", s.texts);
        // it does not hold Apply: there is nothing left to pick
        let apply = s.btns.iter().find(|b| b.primary).expect("Apply is drawn");
        assert!(apply.enabled, "{lang:?}: Apply stays armed");
    }
}

#[test]
fn a_held_value_waits_goes_in_drops_or_asks() {
    use super::value_row::{held_step, Held, HeldStep};
    use value_edit::Value;
    let h = Held {
        value: Value::Metres(7.0),
        mine: "7".to_string(),
        base: "6".to_string(),
        title: "New wall 1".to_string(),
        label: "Length".to_string(),
    };
    assert_eq!(held_step(true, &h, "8", &Value::Metres(8.0)), HeldStep::Wait);
    assert_eq!(held_step(false, &h, "7", &Value::Metres(7.0)), HeldStep::Drop);
    assert_eq!(held_step(false, &h, "6", &Value::Metres(6.0)), HeldStep::Send);
    assert_eq!(held_step(false, &h, "8", &Value::Metres(8.0)), HeldStep::Choose);
}

/// A new request while a held value waits for its pick keeps it for the next card; only
/// Apply and Reject (explicit) clear it.
#[test]
fn a_new_request_keeps_a_held_value() {
    use super::value_row::{Edited, Held};
    use value_edit::{Field, Value};
    let h = Held {
        value: Value::Metres(7.0),
        mine: "7".to_string(),
        base: "6".to_string(),
        title: "New wall 1".to_string(),
        label: "Length".to_string(),
    };
    let f = Field::Assign("Mark".to_string());
    let mut e = Edited::default();
    e.hold(3, f.clone(), h);
    assert!(e.choosing(false));
    e.next_request();
    assert!(e.held(3, &f).is_some() && e.choosing(false), "held value dropped by a request");
    e.clear();
    assert!(!e.choosing(false));
}
