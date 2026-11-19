//! The context line of the AI panel as chips: view, level and the selection with its categories.
//! Every chip can be left out of what the next turn tells the model. Hovering the selection chip
//! lights those elements up in the views, a click frames them. Nothing here writes the model.

use super::{Action, Cx, PanelUi};
use crate::ai::preview;
use crate::ai::session::ViewCtx;
use crate::editor::Sel;
use ddc_model::ElKey;
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::{icon_size, space, Role};
use ddc_ui::widgets;
use egui::{Color32, CursorIcon, Frame, Margin, Sense, Stroke, Ui, Vec2};
use std::collections::BTreeMap;

/// How many categories the selection chip names before it says "+N more".
const NAMED: usize = 2;

/// Chips taken out of the context; the selection's switch is `PanelUi::use_selection`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Off {
    pub view: bool,
    pub level: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    View,
    Level,
    Selection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    View3d(bool),
    Level(String),
    Selection {
        top: Vec<(String, usize)>,
        more: usize,
        total: usize,
    },
}

/// `on`: the chip is part of the context; off, it is the quiet "add it back" chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub kind: Kind,
    pub on: bool,
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ev {
    Remove(Kind),
    Restore(Kind),
    Frame,
}

/// Selected elements by category (the preview's table names), most numerous first.
pub fn counts(sel: &[Sel]) -> Vec<(String, usize)> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for s in sel {
        let name = preview::key_of(*s).map_or_else(|| "other".to_string(), |k| k.0);
        *m.entry(name).or_default() += 1;
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// The chips in order. No selection: no selection chip at all; selection left out: its add chip.
pub fn chips(
    view3d: bool,
    level: &str,
    by_cat: &[(String, usize)],
    off: Off,
    use_sel: bool,
) -> Vec<Chip> {
    let mut out = vec![Chip {
        kind: Kind::View,
        on: !off.view,
        body: Body::View3d(view3d),
    }];
    if !level.is_empty() {
        out.push(Chip {
            kind: Kind::Level,
            on: !off.level,
            body: Body::Level(level.to_string()),
        });
    }
    let total: usize = by_cat.iter().map(|c| c.1).sum();
    if total > 0 {
        out.push(Chip {
            kind: Kind::Selection,
            on: use_sel,
            body: Body::Selection {
                top: by_cat.iter().take(NAMED).cloned().collect(),
                more: by_cat.len().saturating_sub(NAMED),
                total,
            },
        });
    }
    out
}

/// The elements the selection chip lights up.
pub fn hover_keys(sel: &[Sel]) -> Vec<ElKey> {
    sel.iter().filter_map(|s| preview::key_of(*s)).collect()
}

/// What a click on the chip's body does.
pub fn on_click(c: &Chip) -> Option<Ev> {
    match (c.on, c.kind) {
        (false, k) => Some(Ev::Restore(k)),
        (true, Kind::Selection) => Some(Ev::Frame),
        (true, _) => None,
    }
}

/// Applies a remove or restore; `Frame` changes nothing here.
pub fn run(ev: Ev, off: &mut Off, use_sel: &mut bool) {
    let (kind, on) = match ev {
        Ev::Remove(k) => (k, false),
        Ev::Restore(k) => (k, true),
        Ev::Frame => return,
    };
    match kind {
        Kind::View => off.view = !on,
        Kind::Level => off.level = !on,
        Kind::Selection => *use_sel = on,
    }
}

pub fn act_of(ev: Ev) -> Option<Action> {
    matches!(ev, Ev::Frame).then_some(Action::FrameSelection)
}

/// What the model is told: left-out chips stay out of the note of the turn.
pub fn hide(v: &mut ViewCtx, off: Off) {
    v.hide_view = off.view;
    v.hide_level = off.level;
}

pub(super) fn cat_name(cat: &Catalog, table: &str) -> String {
    let key = format!("ai.ctx.cat.{table}");
    let t = cat.t(&key);
    if t == key {
        cat.t("ai.ctx.cat.other")
    } else {
        t
    }
}

fn base_label(cat: &Catalog, body: &Body) -> String {
    match body {
        Body::View3d(d3) => cat.t(if *d3 {
            "ai.ctx.view_3d"
        } else {
            "ai.ctx.view_plan"
        }),
        Body::Level(n) => n.clone(),
        Body::Selection { top, more, total } => {
            let mut parts: Vec<String> = top
                .iter()
                .map(|(t, n)| format!("{}: {n}", cat_name(cat, t)))
                .collect();
            if parts.is_empty() {
                return cat.tf("ai.ctx.selected", &[("n", &total.to_string())]);
            }
            if *more > 0 {
                parts.push(cat.tf("ai.ctx.more", &[("n", &more.to_string())]));
            }
            parts.join(", ")
        }
    }
}

pub fn label(cat: &Catalog, c: &Chip) -> String {
    if c.on {
        return base_label(cat, &c.body);
    }
    match &c.body {
        Body::Selection { total, .. } => {
            cat.tf("ai.ctx.add_selection", &[("n", &total.to_string())])
        }
        b => cat.tf("ai.ctx.add", &[("what", &base_label(cat, b))]),
    }
}

fn tip(cat: &Catalog, c: &Chip) -> String {
    match (c.on, c.kind) {
        (false, _) => cat.t("ai.ctx.add_tip"),
        (true, Kind::Selection) => cat.t("ai.ctx.frame_tip"),
        (true, _) => String::new(),
    }
}

fn remove_tip(cat: &Catalog, k: Kind) -> String {
    cat.t(match k {
        Kind::View => "ai.ctx.drop_view",
        Kind::Level => "ai.ctx.drop_level",
        Kind::Selection => "ai.ctx.drop_selection",
    })
}

fn icon_of(c: &Chip) -> &'static str {
    match &c.body {
        Body::View3d(true) => "3d-view",
        Body::View3d(false) => "plan-view",
        Body::Level(_) => "level",
        Body::Selection { .. } => "select",
    }
}

/// The context line. A hovered selection chip fills `hot`; clicks come back as actions.
pub fn strip(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    hot: &mut Option<Vec<ElKey>>,
    acts: &mut Vec<Action>,
) {
    let sel = &cx.ed.selection;
    let levels = cx.ed.levels();
    let level = levels
        .iter()
        .find(|l| l.0 == cx.ed.level)
        .map_or("", |l| l.1.as_str());
    let list = chips(cx.view3d, level, &counts(sel), st.ctx_off, st.use_selection);
    let cap = ui.available_width();
    let mut ev = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::XS);
        for c in &list {
            ev = chip(ui, cx, c, cap, sel, hot).or(ev);
        }
    });
    if let Some(e) = ev {
        run(e, &mut st.ctx_off, &mut st.use_selection);
        acts.extend(act_of(e));
    }
}

fn chip(
    ui: &mut Ui,
    cx: &Cx<'_>,
    c: &Chip,
    cap: f32,
    sel: &[Sel],
    hot: &mut Option<Vec<ElKey>>,
) -> Option<Ev> {
    let (th, cat) = (cx.th, cx.cat);
    let (fill, ink) = if c.on {
        (th.c("bg.sunken"), th.c("text.primary"))
    } else {
        (Color32::TRANSPARENT, th.c("text.secondary"))
    };
    let text = label(cat, c);
    let close_w = th.d().control_sm + space::XS;
    let reserve = icon_size::SM + space::XS + space::SM * 2.0 + if c.on { close_w } else { 0.0 };
    let max_w = (cap - reserve).max(space::XXL);
    let mut ev = None;
    let out = Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, th.c("border.subtle")))
        .corner_radius(th.radius("md"))
        .inner_margin(Margin::symmetric(space::SM as i8, space::XXS as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(icon_size::SM), Sense::hover());
                let ctx = ui.ctx().clone();
                widgets::with_icons(&ctx, |icons| {
                    icons.paint(ui, icon_of(c), icon_size::SM as u32, r, ink);
                });
                let g = widgets::elide(ui, &text, th.font(Role::Caption), ink, max_w);
                let cut = g.elided;
                let acts = on_click(c).is_some();
                let sense = if acts { Sense::click() } else { Sense::hover() };
                let (r, resp) = ui.allocate_exact_size(g.size(), sense);
                ui.painter().galley(r.min, g, ink);
                let hint = tip(cat, c);
                let (title, desc) = if cut {
                    (text.as_str(), hint.as_str())
                } else {
                    (hint.as_str(), "")
                };
                if acts {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let resp = if title.is_empty() {
                    resp
                } else {
                    widgets::rich_tooltip(resp, th, title, desc, "")
                };
                if resp.clicked() {
                    ev = on_click(c);
                }
                if c.on && widgets::close_button(ui, &remove_tip(cat, c.kind)).clicked() {
                    ev = Some(Ev::Remove(c.kind));
                }
            });
        });
    if c.on && c.kind == Kind::Selection && ui.rect_contains_pointer(out.response.rect) {
        let keys = hover_keys(sel);
        if !keys.is_empty() {
            *hot = Some(keys);
        }
    }
    ev
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ui::connect;
    use crate::ai::ui::logic;
    use crate::editor::Editor;
    use crate::shell::EditorApp;
    use ddc_ui::Lang;

    fn cats(v: &[(&str, usize)]) -> Vec<(String, usize)> {
        v.iter().map(|(a, n)| (a.to_string(), *n)).collect()
    }

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    fn kinds(v: &[Chip]) -> Vec<(Kind, bool)> {
        v.iter().map(|c| (c.kind, c.on)).collect()
    }

    #[test]
    fn view_level_and_selection_become_chips() {
        let v = chips(true, "L1", &cats(&[("wall", 3), ("opening", 1)]), Off::default(), true);
        assert_eq!(
            kinds(&v),
            vec![(Kind::View, true), (Kind::Level, true), (Kind::Selection, true)]
        );
        assert_eq!(v[0].body, Body::View3d(true));
        assert_eq!(v[1].body, Body::Level("L1".into()));
        let Body::Selection { top, more, total } = &v[2].body else {
            panic!("selection body");
        };
        assert_eq!((top.len(), *more, *total), (2, 0, 4));
    }

    #[test]
    fn empty_selection_has_no_chip_even_when_switched_off() {
        for use_sel in [true, false] {
            let v = chips(false, "L1", &[], Off::default(), use_sel);
            assert_eq!(kinds(&v), vec![(Kind::View, true), (Kind::Level, true)]);
        }
    }

    #[test]
    fn selection_left_out_leaves_the_add_chip() {
        let v = chips(false, "L1", &cats(&[("wall", 2)]), Off::default(), false);
        assert_eq!(v.last().map(|c| (c.kind, c.on)), Some((Kind::Selection, false)));
        assert_eq!(on_click(v.last().unwrap()), Some(Ev::Restore(Kind::Selection)));
    }

    #[test]
    fn more_than_two_categories_say_more() {
        let by = cats(&[("wall", 5), ("slab", 3), ("room", 2), ("col", 1)]);
        let v = chips(false, "L1", &by, Off::default(), true);
        let Body::Selection { top, more, total } = &v[2].body else {
            panic!("selection body");
        };
        assert_eq!(top, &cats(&[("wall", 5), ("slab", 3)]));
        assert_eq!((*more, *total), (2, 11));
    }

    #[test]
    fn counts_group_by_category_most_numerous_first() {
        let sel = [Sel::Wall(1), Sel::Wall(2), Sel::Slab(3), Sel::Wall(4), Sel::Room(5)];
        assert_eq!(counts(&sel), cats(&[("wall", 3), ("room", 1), ("slab", 1)]));
        assert!(counts(&[]).is_empty());
    }

    #[test]
    fn removing_a_chip_removes_it_from_the_context_sent() {
        let mut a = app();
        a.ed.selection = vec![Sel::Wall(1), Sel::Wall(2)];
        let (mut off, mut use_sel) = (Off::default(), true);
        let sent = |off: Off, use_sel: bool| {
            let mut v = connect::view_ctx(&a, use_sel);
            hide(&mut v, off);
            v
        };
        let v = sent(off, use_sel);
        assert_eq!(v.selection.len(), 2);
        assert!(!v.hide_view && !v.hide_level);

        run(Ev::Remove(Kind::Selection), &mut off, &mut use_sel);
        assert!(sent(off, use_sel).selection.is_empty());
        run(Ev::Remove(Kind::View), &mut off, &mut use_sel);
        run(Ev::Remove(Kind::Level), &mut off, &mut use_sel);
        let v = sent(off, use_sel);
        assert!(v.hide_view && v.hide_level);

        let list = chips(false, "L1", &cats(&[("wall", 2)]), off, use_sel);
        assert!(list.iter().all(|c| !c.on), "{list:?}");

        run(Ev::Restore(Kind::Selection), &mut off, &mut use_sel);
        run(Ev::Restore(Kind::View), &mut off, &mut use_sel);
        run(Ev::Restore(Kind::Level), &mut off, &mut use_sel);
        assert_eq!(off, Off::default());
        assert_eq!(sent(off, use_sel).selection.len(), 2);
    }

    #[test]
    fn hover_lights_the_selected_elements() {
        let sel = [Sel::Wall(7), Sel::Slab(9), Sel::Anno(3)];
        let want = logic::hot_set([("wall".to_string(), 7), ("slab".to_string(), 9)]);
        assert_eq!(logic::hot_set(hover_keys(&sel)), want);
        assert!(hover_keys(&[]).is_empty());
    }

    #[test]
    fn click_on_the_selection_chip_frames_and_others_do_nothing() {
        let v = chips(false, "L1", &cats(&[("wall", 1)]), Off::default(), true);
        assert_eq!(on_click(&v[0]), None);
        assert_eq!(on_click(&v[1]), None);
        let ev = on_click(&v[2]).unwrap();
        assert_eq!(ev, Ev::Frame);
        assert!(matches!(act_of(ev), Some(Action::FrameSelection)));
        assert!(act_of(Ev::Remove(Kind::View)).is_none());
        let (mut off, mut use_sel) = (Off::default(), true);
        run(Ev::Frame, &mut off, &mut use_sel);
        assert!(use_sel && off == Off::default());
    }

    #[test]
    fn chips_never_touch_the_model() {
        let mut a = app();
        a.ed.selection = vec![Sel::Wall(1)];
        let h0 = a.ed.model.content_hash().unwrap();
        let j0 = a.ed.model.journal_len().unwrap();
        let (mut off, mut use_sel) = (Off::default(), true);
        for ev in [
            Ev::Remove(Kind::Selection),
            Ev::Restore(Kind::Selection),
            Ev::Remove(Kind::View),
            Ev::Frame,
        ] {
            run(ev, &mut off, &mut use_sel);
            let mut v = connect::view_ctx(&a, use_sel);
            hide(&mut v, off);
            let _ = chips(false, "L1", &counts(&a.ed.selection), off, use_sel);
            let _ = hover_keys(&a.ed.selection);
        }
        assert_eq!(a.ed.model.content_hash().unwrap(), h0);
        assert_eq!(a.ed.model.journal_len().unwrap(), j0);
    }

    #[test]
    fn labels_are_translated_in_both_languages() {
        let by = cats(&[("wall", 3), ("opening", 1), ("slab", 1)]);
        let on = chips(true, "L1", &by, Off::default(), true);
        let off = chips(true, "L1", &by, Off { view: true, level: true }, false);
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "ai.ctx.drop_view",
                "ai.ctx.drop_level",
                "ai.ctx.drop_selection",
                "ai.ctx.add_tip",
                "ai.ctx.frame_tip",
                "ai.ctx.cat.other",
            ] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
            let s = label(&cat, &on[2]);
            assert!(s.contains(": 3"), "{s}");
            assert!(!s.contains("ai.ctx"), "{s}");
            for c in off.iter() {
                let l = label(&cat, c);
                assert!(!l.contains("ai.ctx") && l.contains('+'), "{l}");
            }
            for t in ["wall", "opening", "curtain_wall", "slab", "room", "mep_run", "comp", "col", "beam", "ceiling", "mesh", "railing", "roof", "stair"] {
                let key = format!("ai.ctx.cat.{t}");
                assert_ne!(cat.t(&key), key, "{key} in {lang:?}");
            }
        }
    }
}
