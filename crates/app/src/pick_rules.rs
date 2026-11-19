// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One pick query for hover, click, measure and the context menu. The query reads the
//! editor and never changes the selection; hover highlights exactly what a click would take.
//! The status bar category filter and the "select pinned" toggle narrow it, and items hidden
//! in the view (temporary, permanent, phase, graphics, view range) never reach it.

use crate::edit3d::pick::{Hit3, Target};
use crate::editor::{Editor, Sel};
use crate::isolate::Vis;
use crate::selection::{self, CATEGORIES};
use ddc_geom::V2;
use egui::{self, RichText};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

/// What a pick may take beyond plain hit testing.
#[derive(Clone, Debug, Default)]
pub struct PickFilter {
    /// Category keys (`selection::CATEGORIES`) a pick skips.
    pub off: BTreeSet<&'static str>,
    /// Pinned items are skipped ("select pinned" off in the status bar).
    pub skip_pinned: bool,
    /// Visibility of the view sending the current click; set only for that click.
    pub vis: Option<Arc<Vis>>,
}

impl PickFilter {
    /// May a pick take `s`?
    pub fn allows(&self, ed: &Editor, s: Sel) -> bool {
        if self.off.contains(selection::category(ed, s)) {
            return false;
        }
        if self.vis.as_ref().is_some_and(|v| !v.visible(ed, s)) {
            return false;
        }
        !(self.skip_pinned
            && crate::clip_tools::sel_item(s).is_some_and(|(k, id)| pinned(ed, &k, id)))
    }

    /// The same rule for a 3D target (some have no plan selection).
    pub fn allows_target(&self, ed: &Editor, t: Target) -> bool {
        if let Some(s) = t.sel() {
            return self.allows(ed, s);
        }
        if target_category(t).is_some_and(|c| self.off.contains(c)) {
            return false;
        }
        !(self.skip_pinned
            && crate::clip_tools::pick_item(t).is_some_and(|(k, id)| pinned(ed, &k, id)))
    }
}

fn pinned(ed: &Editor, kind: &str, id: i64) -> bool {
    ed.model.is_pinned(kind, id).unwrap_or(false)
}

/// Filter category of a 3D target without a plan selection.
fn target_category(t: Target) -> Option<&'static str> {
    match t.cat.key() {
        "slab" => Some("floor"),
        "mesh" => Some("imported"),
        "other" => None,
        k => Some(k),
    }
}

/// What a click at `p` selects (plan). The click path itself calls this.
pub fn target_at(ed: &Editor, p: V2) -> Option<Sel> {
    ed.pick_at(p, &|s| ed.pick_filter.allows(ed, s))
}

/// What hover shows at `p` in a view with visibility `vis`: the same item a click takes.
pub fn hover_at(ed: &Editor, vis: &Vis, p: V2) -> Option<Sel> {
    ed.pick_at(p, &|s| vis.visible(ed, s) && ed.pick_filter.allows(ed, s))
}

type HoverKey = (
    [u64; 3],
    i64,
    [usize; 6],
    u64,
    bool,
    BTreeSet<&'static str>,
);

thread_local! {
    static HOVER: RefCell<Option<(HoverKey, Option<Sel>)>> = const { RefCell::new(None) };
}

/// [`hover_at`] for a frame loop: recomputed when the cursor, the model, the view's
/// visibility or the filter change, so a resting cursor costs nothing on big models.
pub fn hover_cached(ed: &Editor, vis: &Vis, p: V2) -> Option<Sel> {
    let temp = match &vis.temp {
        crate::isolate::Temp::Off => 0,
        crate::isolate::Temp::Hide(h) => 1 + 2 * h.len(),
        crate::isolate::Temp::Isolate(k) => 2 + 2 * k.len(),
    };
    let range = vis.range.as_ref().map_or(0, |r| Arc::as_ptr(r) as usize);
    let key: HoverKey = (
        [p.x.to_bits(), p.y.to_bits(), ed.tool.radius.to_bits()],
        ed.level,
        [
            vis.hidden.len(),
            temp,
            vis.phase.hide.len(),
            range,
            ed.walls.len(),
            ed.selection.len(),
        ],
        ed.revision ^ vis.gfx.stamp.rotate_left(32),
        ed.pick_filter.skip_pinned,
        ed.pick_filter.off.clone(),
    );
    if let Some(hit) = HOVER.with(|h| {
        h.borrow()
            .as_ref()
            .and_then(|(k, s)| (*k == key).then_some(*s))
    }) {
        return hit;
    }
    let s = hover_at(ed, vis, p);
    HOVER.with(|h| *h.borrow_mut() = Some((key, s)));
    s
}

/// A plan click that sees the view's visibility, as hover does.
pub fn click(ed: &mut Editor, vis: &Vis, p: V2) {
    ed.pick_filter.vis = vis.any().then(|| Arc::new(vis.clone()));
    ed.handle(crate::wall_tool::Event::Click(p));
    ed.pick_filter.vis = None;
}

/// 3D hits a pick may take, nearest first (hover, Tab cycle and click use this list).
pub fn filter3(ed: &Editor, hits: Vec<Hit3>) -> Vec<Hit3> {
    hits.into_iter()
        .filter(|h| ed.pick_filter.allows_target(ed, h.target))
        .collect()
}

/// The hit at the Tab cycle position: one choice for hover and click.
pub fn choose(hits: &[Hit3], cycle: usize) -> Option<Hit3> {
    (!hits.is_empty()).then(|| hits[cycle % hits.len()])
}

/// An item under the cursor, as the context menu lists it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Here {
    pub sel: Sel,
    pub cat: &'static str,
    /// Length, m, for linear items.
    pub len: Option<f64>,
}

pub fn here_of(ed: &Editor, s: Sel) -> Here {
    Here {
        sel: s,
        cat: selection::category(ed, s),
        len: length(ed, s),
    }
}

/// Every item at `p` (radius `r`), the click target first. Pure.
pub fn items_here(ed: &Editor, vis: &Vis, p: V2, r: f64) -> Vec<Here> {
    let ok = |s: Sel| vis.visible(ed, s) && ed.pick_filter.allows(ed, s);
    let mut v: Vec<Sel> = hover_at(ed, vis, p).into_iter().collect();
    for c in selection::candidates(ed, p, r, ok) {
        if let [s] = c.as_slice() {
            if !v.contains(s) {
                v.push(*s);
            }
        }
    }
    v.into_iter().map(|s| here_of(ed, s)).collect()
}

/// Length of a linear item, m; `None` for areas and points.
pub fn length(ed: &Editor, s: Sel) -> Option<f64> {
    match s {
        Sel::Wall(id) => ed
            .walls
            .iter()
            .find(|w| w.id == id)
            .map(|w| (w.b - w.a).len()),
        Sel::Opening(id) => ed.opening(id).map(|o| o.width),
        Sel::Curtain(id) | Sel::Panel(id, _) => {
            let l: f64 = ed
                .curtain_lines
                .iter()
                .filter(|l| l.0 == id)
                .map(|&(_, a, b)| (b - a).len())
                .sum();
            (l > 0.0).then_some(l)
        }
        _ => {
            // a thin four-sided outline (beam, run, railing): its long side
            let os = selection::outlines(ed, s);
            let [o] = os.as_slice() else { return None };
            if o.len() != 4 {
                return None;
            }
            let e: Vec<f64> = (0..4).map(|i| (o[(i + 1) % 4] - o[i]).len()).collect();
            let (long, short) = (e[0].max(e[1]), e[0].min(e[1]));
            (short > 0.0 && long >= 4.0 * short).then_some(long)
        }
    }
}

/// "Wall · 4.00 m": category and, for linear items, length.
pub fn describe(ed: &Editor, h: &Here) -> String {
    let cat = crate::tr(&format!("quick.cat.{}", h.cat), &[]);
    match h.len {
        Some(l) => crate::tr(
            "pick_rules.here.row",
            &[
                ("cat", &cat),
                ("len", &crate::units::format_length(l, ed.tool.units)),
            ],
        ),
        None => cat,
    }
}

thread_local! {
    static HERE: RefCell<Vec<Here>> = const { RefCell::new(Vec::new()) };
}

/// Items listed by the open context menu.
pub fn set_here(v: Vec<Here>) {
    HERE.with(|h| *h.borrow_mut() = v);
}

pub fn here() -> Vec<Here> {
    HERE.with(|h| h.borrow().clone())
}

/// Status bar chip of the category filter: text and whether some category is off.
pub fn filter_label(c: &ddc_ui::Catalog, f: &PickFilter) -> (String, bool) {
    let n = f.off.len();
    if n > 0 {
        (c.tf("pick_rules.filter.on", &[("n", &n.to_string())]), true)
    } else {
        (c.t("pick_rules.filter"), false)
    }
}

/// Body of the status bar pick menu; returns the new set of skipped categories when changed.
pub fn filter_menu(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    c: &ddc_ui::Catalog,
    f: &PickFilter,
) -> Option<BTreeSet<&'static str>> {
    let n = f.off.len();
    let mut out = None;
    ui.label(RichText::new(c.t("pick_rules.filter.title")).color(th.c("text.secondary")));
    let mut off = f.off.clone();
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            for &cat in CATEGORIES.iter() {
                let mut on = !off.contains(cat);
                if ui
                    .checkbox(&mut on, c.t(&format!("quick.cat.{cat}")))
                    .changed()
                {
                    if on {
                        off.remove(cat);
                    } else {
                        off.insert(cat);
                    }
                    out = Some(off.clone());
                }
            }
        });
    ui.separator();
    if ui
        .add_enabled(n > 0, egui::Button::new(c.t("pick_rules.filter.all")))
        .clicked()
    {
        out = Some(BTreeSet::new());
        ui.close();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit3d::pick::Cat;
    use crate::modify_tools::tests::walls;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;
    use ddc_model::OpeningSpec;

    fn scene() -> (Editor, i64, i64, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 3.0), (4.0, 3.0))],
        );
        let (a, b) = (ed.walls[0].id, ed.walls[1].id);
        let spec = OpeningSpec {
            along: 1.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        let d = ed.model.add_opening(a, &spec).unwrap();
        ed.reload().unwrap();
        ed.set_mode(Mode::Select);
        (ed, a, b, d)
    }

    fn click_at(ed: &mut Editor, p: V2) -> Option<Sel> {
        ed.selection.clear();
        click(ed, &Vis::default(), p);
        ed.selection.first().copied()
    }

    #[test]
    fn hover_matches_click_target() {
        let (mut ed, a, b, d) = scene();
        let pts = [
            (v2(1.45, 0.0), Some(Sel::Opening(d))),
            (v2(3.0, 0.02), Some(Sel::Wall(a))),
            (v2(2.0, 3.0), Some(Sel::Wall(b))),
            (v2(2.0, 1.5), None),
            (v2(9.0, 9.0), None),
        ];
        for (p, want) in pts {
            let hover = hover_at(&ed, &Vis::default(), p);
            assert_eq!(hover, want, "hover at {p:?}");
            assert_eq!(click_at(&mut ed, p), hover, "click at {p:?}");
        }
        // hidden in the view: neither hovered nor clicked
        let mut vis = Vis::default();
        vis.hidden.insert(Sel::Wall(b));
        let p = v2(2.0, 3.0);
        assert_eq!(hover_at(&ed, &vis, p), None);
        ed.selection.clear();
        click(&mut ed, &vis, p);
        assert!(ed.selection.is_empty());
        assert!(ed.pick_filter.vis.is_none(), "view visibility lasts one click");
        // pinned with "select pinned" off: skipped by hover and click alike
        ed.model.pin(&[("wall", a)]).unwrap();
        ed.reload().unwrap();
        ed.pick_filter.skip_pinned = true;
        let p = v2(3.0, 0.02);
        assert_eq!(hover_at(&ed, &Vis::default(), p), None);
        assert_eq!(click_at(&mut ed, p), None);
        ed.pick_filter.skip_pinned = false;
        assert_eq!(click_at(&mut ed, p), Some(Sel::Wall(a)));
    }

    #[test]
    fn category_filter_excludes_doors() {
        let (mut ed, a, _, d) = scene();
        let p = v2(1.45, 0.0);
        assert_eq!(click_at(&mut ed, p), Some(Sel::Opening(d)));
        ed.pick_filter.off.insert("door");
        // the door is skipped: its host wall is taken, by hover and click alike
        assert_eq!(hover_at(&ed, &Vis::default(), p), Some(Sel::Wall(a)));
        assert_eq!(click_at(&mut ed, p), Some(Sel::Wall(a)));
        let here = items_here(&ed, &Vis::default(), p, 0.1);
        assert!(here.iter().all(|h| h.cat != "door"), "{here:?}");
        ed.pick_filter.off.insert("wall");
        assert_eq!(click_at(&mut ed, p), None);
        // 3D: the same filter drops door targets
        let hit = |t: Target| Hit3 {
            body: 0,
            target: t,
            t: 1.0,
            point: Default::default(),
            normal: Default::default(),
        };
        let kept = filter3(
            &ed,
            vec![hit(Target::new(Cat::Opening, d)), hit(Target::new(Cat::Slab, 7))],
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, Target::new(Cat::Slab, 7));
        assert_eq!(choose(&kept, 5).map(|h| h.target), Some(kept[0].target));
        assert!(choose(&[], 0).is_none());
    }

    #[test]
    fn measure_keeps_selection() {
        let (mut ed, a, b, d) = scene();
        ed.selection = vec![Sel::Wall(b)];
        let before = ed.selection.clone();
        let rev = ed.revision;
        let here = items_here(&ed, &Vis::default(), v2(1.45, 0.0), 0.1);
        assert_eq!(here[0].sel, Sel::Opening(d), "click target first");
        assert!(here.iter().any(|h| h.sel == Sel::Wall(a)), "{here:?}");
        let door = here[0];
        assert_eq!(door.cat, "door");
        assert!((door.len.unwrap() - 0.9).abs() < 1e-9);
        let wall = here_of(&ed, Sel::Wall(a));
        assert!((wall.len.unwrap() - 4.0).abs() < 1e-9);
        assert!(!describe(&ed, &wall).is_empty());
        assert_eq!(target_at(&ed, v2(3.0, 0.02)), Some(Sel::Wall(a)));
        assert_eq!(ed.selection, before, "queries never select");
        assert_eq!(ed.revision, rev, "queries never edit");
        // a move event over another item does not select it either
        ed.handle(Event::Move(v2(3.0, 0.02)));
        assert_eq!(ed.selection, before);
    }
}
