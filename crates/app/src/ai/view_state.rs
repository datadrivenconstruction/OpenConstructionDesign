// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The screen state the AI view tools change, on the UI thread. Nothing here touches the model:
//! selection, temporary isolation (`isolate::Vis`), plan zoom, 3D camera and a color override
//! layered on the view graphics. The view from before the first change is kept, and Reset view
//! (the tool or the button) brings exactly that back.

use super::sets::Key;
use super::view_tools::{Legend, ViewCmd};
use crate::editor::Sel;
use crate::isolate::{Part, Temp};
use crate::shell::EditorApp;
use crate::view_gfx_ui::GfxLayer;
use ddc_geom::{v2, V2};
use ddc_model::ElKey;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Tables drawn in plan by lines only: their color goes on the line too.
const LINED: [&str; 3] = ["opening", "mep_run", "railing"];
/// Colored layers kept: plan, 3D and a few sections.
const TINT_CACHE: usize = 8;

/// The view before the assistant changed it.
struct Saved {
    temp: Temp,
    parts: HashSet<Part>,
    selection: Vec<Sel>,
    level: i64,
    center: V2,
    scale: f64,
    camera: ddc_render::OrbitCamera,
}

struct Tint {
    rev: u64,
    fills: HashMap<(&'static str, i64), u32>,
}

/// What the person asked of the view panel this frame; run with the app at the next `tick`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Row(usize),
    Reset,
}

/// What the view panel says about the assistant's view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub shown: usize,
    pub isolated: usize,
    pub hidden: usize,
}

#[derive(Default)]
struct State {
    saved: Option<Saved>,
    tint: Option<Arc<Tint>>,
    legend: Option<Arc<Legend>>,
    status: Status,
    next_rev: u64,
    cache: Vec<(Arc<GfxLayer>, Arc<GfxLayer>)>,
    ask: Option<Ask>,
    hover: Option<usize>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// The assistant changed the view and Reset view would bring something back.
pub fn active() -> bool {
    with(|s| s.saved.is_some())
}

pub fn legend() -> Option<Arc<Legend>> {
    with(|s| s.legend.clone())
}

pub fn status() -> Status {
    with(|s| s.status)
}

fn els(keys: &[Key]) -> Vec<ElKey> {
    keys.iter().map(Key::el).collect()
}

/// Selections and 3D parts of live keys; roofs, stairs and railings have only a part.
fn split(keys: &[Key]) -> (Vec<Sel>, Vec<Part>) {
    let (mut sels, mut parts) = (vec![], vec![]);
    for k in keys {
        let (s, p) = super::preview::hide_of(&k.el());
        sels.extend(s);
        parts.extend(p);
    }
    (sels, parts)
}

fn remember(app: &EditorApp) {
    with(|s| {
        if s.saved.is_none() {
            let vis = &app.plan.b.vis;
            s.saved = Some(Saved {
                temp: vis.temp.clone(),
                parts: vis.parts.clone(),
                selection: app.ed.selection.clone(),
                level: app.ed.level,
                center: app.plan.center,
                scale: app.plan.scale,
                camera: app.view.camera.clone(),
            });
        }
    });
}

/// Plan points of a selected element on the current level.
fn points(app: &EditorApp, s: Sel) -> Vec<V2> {
    let ed = &app.ed;
    match s {
        Sel::Wall(id) => ed.walls.iter().filter(|w| w.id == id).flat_map(|w| [w.a, w.b]).collect(),
        Sel::Opening(id) => {
            let Some(o) = ed.opening(id) else {
                return vec![];
            };
            let Some(w) = ed.walls.iter().find(|w| w.id == o.wall) else {
                return vec![];
            };
            let (dx, dy) = (w.b.x - w.a.x, w.b.y - w.a.y);
            let len = (dx * dx + dy * dy).sqrt().max(1e-9);
            let at = |t: f64| v2(w.a.x + dx / len * t, w.a.y + dy / len * t);
            vec![at(o.along), at(o.along + o.width)]
        }
        Sel::Body(k, id) => ed
            .bodies
            .iter()
            .filter(|b| b.kind == k && b.id == id)
            .flat_map(|b| b.hull.iter().copied())
            .collect(),
        _ => vec![],
    }
}

/// Plan and 3D go to the selection; a selection without plan points keeps the plan as it is.
fn frame(app: &mut EditorApp) {
    if app.view3d {
        app.run("view.zoom_sel_3d");
        return;
    }
    let pts: Vec<V2> = app.ed.selection.iter().flat_map(|s| points(app, *s)).collect();
    let Some(first) = pts.first() else {
        crate::open::summary::frame_selection(app);
        return;
    };
    let (mut lo, mut hi) = (*first, *first);
    for p in &pts {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    let span = ((hi.x - lo.x).max(hi.y - lo.y) * 1.2).max(1.0);
    app.plan.center = v2((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
    app.plan.scale = (320.0 / span).clamp(20.0, 2000.0);
}

fn select(app: &mut EditorApp, keys: &[Key]) {
    app.ed.selection = split(keys).0;
}

/// Runs a view command; returns the keys to keep lit (shown and selected elements).
pub fn apply(app: &mut EditorApp, cmd: &ViewCmd) -> Vec<ElKey> {
    match cmd {
        ViewCmd::Reset => {
            reset(app);
            vec![]
        }
        ViewCmd::Show { keys, level } => {
            remember(app);
            if let Some(l) = level {
                if *l != app.ed.level {
                    app.ed.set_level(*l);
                }
            }
            select(app, keys);
            frame(app);
            with(|s| s.status.shown = keys.len());
            els(keys)
        }
        ViewCmd::Select(keys) => {
            remember(app);
            select(app, keys);
            with(|s| s.status.shown = keys.len());
            els(keys)
        }
        ViewCmd::Isolate { keys, hide } => {
            remember(app);
            let (sels, parts) = split(keys);
            let vis = &mut app.plan.b.vis;
            if *hide {
                vis.hide_parts(&sels, &parts);
            } else {
                vis.isolate_parts(&app.ed, &sels, &parts);
            }
            with(|s| {
                if *hide {
                    s.status.hidden += keys.len();
                    s.status.isolated = 0;
                } else {
                    s.status.isolated = keys.len();
                    s.status.hidden = 0;
                }
            });
            vec![]
        }
        ViewCmd::Color(lg) => {
            remember(app);
            color(lg.clone());
            vec![]
        }
    }
}

fn color(lg: Arc<Legend>) {
    with(|s| {
        s.next_rev += 1;
        let mut fills = HashMap::new();
        for r in &lg.rows {
            for k in r.keys.iter() {
                fills.insert((k.table(), k.id), r.rgb);
            }
        }
        s.tint = Some(Arc::new(Tint {
            rev: s.next_rev,
            fills,
        }));
        s.cache.clear();
        s.legend = Some(lg);
        s.hover = None;
    });
}

/// Reset view: the view from before the first change, the colors gone.
pub fn reset(app: &mut EditorApp) {
    let saved = with(|s| {
        let saved = s.saved.take();
        *s = State {
            next_rev: s.next_rev,
            ..State::default()
        };
        saved
    });
    let Some(v) = saved else {
        return;
    };
    app.plan.b.vis.temp = v.temp;
    app.plan.b.vis.parts = v.parts;
    if app.ed.level != v.level {
        app.ed.set_level(v.level);
    }
    app.ed.selection = v.selection;
    app.plan.center = v.center;
    app.plan.scale = v.scale;
    app.view.camera = v.camera;
}

/// Another project: the kept view belongs to the old one and is dropped, not restored.
pub fn forget() {
    with(|s| {
        *s = State {
            next_rev: s.next_rev,
            ..State::default()
        }
    });
}

/// The view graphics with the color override on top; `base` itself without one. Called by
/// `view_gfx_ui::layer` for every view, so plan, 3D and sections color alike.
pub fn tinted(base: Arc<GfxLayer>) -> Arc<GfxLayer> {
    with(|s| {
        let Some(t) = s.tint.clone() else {
            return base;
        };
        if let Some((_, out)) = s.cache.iter().find(|(b, _)| Arc::ptr_eq(b, &base)) {
            return out.clone();
        }
        let mut g = (*base).clone();
        let paint = Arc::make_mut(&mut g.paint);
        for (&key, &rgb) in &t.fills {
            let e = paint.elems.entry(key).or_default();
            e.over.fill = Some(rgb);
            if LINED.contains(&key.0) {
                e.over.line = Some(rgb);
            }
        }
        // a stamp no plain layer has: the 3D scene and the plan edges rebuild once per change
        g.stamp = base.stamp ^ t.rev.rotate_left(40) ^ (1u64 << 63);
        let out = Arc::new(g);
        if s.cache.len() >= TINT_CACHE {
            s.cache.remove(0);
        }
        s.cache.push((base, out.clone()));
        out
    })
}

/// From the view panel (drawn without the app): a legend row or Reset view was clicked.
pub fn ask(a: Ask) {
    with(|s| s.ask = Some(a));
}

/// The legend row under the pointer this frame.
pub fn hover(row: Option<usize>) {
    with(|s| s.hover = row);
}

/// Elements of the hovered legend row, lit like a card row.
pub fn hover_keys() -> Option<Vec<ElKey>> {
    with(|s| {
        let lg = s.legend.as_ref()?;
        Some(els(&lg.rows.get(s.hover?)?.keys))
    })
}

/// Runs what the panel asked last frame. A legend row selects its elements.
pub fn tick(app: &mut EditorApp) -> Option<Vec<ElKey>> {
    let (a, lg) = with(|s| (s.ask.take(), s.legend.clone()));
    match a? {
        Ask::Reset => {
            reset(app);
            Some(vec![])
        }
        Ask::Row(i) => {
            let keys = lg?.rows.get(i)?.keys.clone();
            Some(apply(app, &ViewCmd::Select(keys)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::view_tools::{self, Bucket, Found, FoundBy};
    use crate::editor::Editor;

    fn app_of(ed: Editor) -> EditorApp {
        forget();
        EditorApp::new(ed, None, "test".into())
    }

    fn snapshot(app: &EditorApp) -> String {
        format!(
            "{:?} {:?} {:?} {} {:?} {} {:?}",
            app.plan.b.vis.temp,
            app.plan.b.vis.parts,
            app.ed.selection,
            app.ed.level,
            app.plan.center,
            app.plan.scale,
            app.view.camera,
        )
    }

    fn base_layer() -> Arc<GfxLayer> {
        Arc::new(GfxLayer::default())
    }

    #[test]
    fn reset_restores_the_previous_view() {
        let (ed, ids) = view_tools::tests::windows(8, &[1]);
        let mut app = app_of(ed);
        let first = Sel::Opening(ids[5]);
        app.ed.selection = vec![first];
        app.plan.center = v2(-3.0, 4.0);
        app.plan.scale = 77.0;
        let before = snapshot(&app);
        let keys: Arc<Vec<Key>> =
            Arc::new(ids[..3].iter().map(|id| Key { cat: "window", id: *id }).collect());
        let lit = apply(&mut app, &ViewCmd::Show { keys: keys.clone(), level: None });
        assert_eq!(lit.len(), 3);
        assert_eq!(app.ed.selection.len(), 3);
        assert_ne!(snapshot(&app), before, "Show selects and zooms");
        apply(&mut app, &ViewCmd::Isolate { keys: keys.clone(), hide: false });
        assert!(matches!(app.plan.b.vis.temp, Temp::Isolate(_)));
        let g = view_tools::color_groups(&app.ed, "fire_rating", Some(&keys[..])).unwrap();
        apply(&mut app, &ViewCmd::Color(Arc::new(view_tools::legend("fire_rating", g))));
        let base = base_layer();
        assert!(!Arc::ptr_eq(&tinted(base.clone()), &base), "the views are colored");
        assert!(active());
        apply(&mut app, &ViewCmd::Reset);
        assert_eq!(snapshot(&app), before, "everything the tools changed is back");
        assert!(!active() && legend().is_none());
        assert!(Arc::ptr_eq(&tinted(base.clone()), &base), "the colors are gone");
        assert_eq!(status(), Status::default());
        // a second reset has nothing to bring back and changes nothing
        apply(&mut app, &ViewCmd::Reset);
        assert_eq!(snapshot(&app), before);
    }

    #[test]
    fn the_view_kept_is_the_one_before_the_first_change() {
        let (ed, ids) = view_tools::tests::windows(4, &[]);
        let mut app = app_of(ed);
        let before = snapshot(&app);
        let one = Arc::new(vec![Key { cat: "window", id: ids[0] }]);
        let two = Arc::new(vec![Key { cat: "window", id: ids[1] }]);
        apply(&mut app, &ViewCmd::Show { keys: one, level: None });
        apply(&mut app, &ViewCmd::Isolate { keys: two.clone(), hide: true });
        assert_eq!(status().hidden, 1);
        apply(&mut app, &ViewCmd::Select(two));
        reset(&mut app);
        assert_eq!(snapshot(&app), before);
    }

    #[test]
    fn a_legend_row_click_selects_its_elements_and_hover_lights_them() {
        let (ed, ids) = view_tools::tests::windows(6, &[0, 4]);
        let mut app = app_of(ed);
        let windows: Vec<Key> = ids.iter().map(|id| Key { cat: "window", id: *id }).collect();
        let g = view_tools::color_groups(&app.ed, "fire_rating", Some(&windows[..])).unwrap();
        apply(&mut app, &ViewCmd::Color(Arc::new(view_tools::legend("fire_rating", g))));
        let lg = legend().unwrap();
        let empty = lg.rows.iter().position(|r| r.bucket == Bucket::Empty).unwrap();
        hover(Some(empty));
        assert_eq!(hover_keys().map(|k| k.len()), Some(2));
        ask(Ask::Row(empty));
        let lit = tick(&mut app).unwrap();
        assert_eq!(lit.len(), 2);
        let mut sel = app.ed.selection.clone();
        sel.sort_by_key(|s| format!("{s:?}"));
        let mut want = vec![Sel::Opening(ids[0]), Sel::Opening(ids[4])];
        want.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(sel, want);
        assert!(tick(&mut app).is_none(), "one click, one run");
        ask(Ask::Reset);
        tick(&mut app);
        assert!(!active());
    }

    #[test]
    fn a_tinted_layer_paints_fill_and_opening_lines_and_is_cached() {
        let (ed, ids) = view_tools::tests::windows(4, &[]);
        let mut app = app_of(ed);
        let keys: Vec<Key> = ids.iter().map(|id| Key { cat: "window", id: *id }).collect();
        let g = view_tools::color_groups(&app.ed, "fire_rating", Some(&keys[..])).unwrap();
        apply(&mut app, &ViewCmd::Color(Arc::new(view_tools::legend("fire_rating", g))));
        let base = base_layer();
        let a = tinted(base.clone());
        assert!(Arc::ptr_eq(&a, &tinted(base.clone())), "one layer per base and change");
        assert_ne!(a.stamp, base.stamp);
        let e = a.paint.elem("opening", ids[0]).unwrap();
        assert!(e.over.fill.is_some() && e.over.line == e.over.fill);
        reset(&mut app);
    }

    /// The showcase: find windows without a fire rating, show 4 of 202, color by fire rating.
    #[test]
    fn showcase_windows_without_fire_rating() {
        let unrated = [7, 58, 120, 199];
        let (ed, ids) = view_tools::tests::windows(202, &unrated);
        let mut app = app_of(ed);
        let before = snapshot(&app);
        // 1. find: the same set query_elements returns, with its scope
        let h = view_tools::resolve_filter(&app.ed, "window fire_rating=empty").unwrap();
        assert_eq!((h.keys.len(), h.of), (4, 202));
        let found = Found {
            by: FoundBy::Query,
            what: h.what.clone(),
            keys: Arc::new(h.keys.clone()),
            of: h.of,
            level: h.level,
        };
        let en = ddc_ui::i18n::Catalog::new(ddc_ui::Lang::En);
        assert_eq!(crate::ai::ui::found::count_line(&en, &found), "4 of 202");
        // 2. show: selected, lit and framed
        let lit = apply(&mut app, &ViewCmd::Show { keys: found.keys.clone(), level: h.level });
        assert_eq!(lit.len(), 4);
        let mut sel: Vec<i64> = app
            .ed
            .selection
            .iter()
            .filter_map(|s| match s {
                Sel::Opening(id) => Some(*id),
                _ => None,
            })
            .collect();
        sel.sort_unstable();
        let mut want: Vec<i64> = unrated.iter().map(|i| ids[*i]).collect();
        want.sort_unstable();
        assert_eq!(sel, want);
        // 3. color by fire rating: every window in the legend, the 4 as "no value"
        let windows = view_tools::resolve_filter(&app.ed, "window").unwrap();
        let g =
            view_tools::color_groups(&app.ed, "fire_rating", Some(&windows.keys[..])).unwrap();
        let lg = view_tools::legend("fire_rating", g);
        assert_eq!(lg.count(), 202);
        let shown: Vec<(Bucket, usize)> =
            lg.rows.iter().map(|r| (r.bucket, r.keys.len())).collect();
        let rated: usize = (0..202).filter(|i| !unrated.contains(i)).count();
        let sixty = (0..202).filter(|i| !unrated.contains(i) && i % 3 == 0).count();
        assert_eq!(
            shown,
            vec![(Bucket::Value, rated - sixty), (Bucket::Value, sixty), (Bucket::Empty, 4)]
        );
        apply(&mut app, &ViewCmd::Color(Arc::new(lg)));
        let colored = tinted(base_layer());
        let painted = ids
            .iter()
            .filter(|id| colored.paint.elem("opening", **id).is_some_and(|e| e.over.fill.is_some()))
            .count();
        assert_eq!(painted, 202);
        // and back
        reset(&mut app);
        assert_eq!(snapshot(&app), before);
    }
}
