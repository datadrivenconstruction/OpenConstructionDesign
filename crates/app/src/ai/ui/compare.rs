// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The options view: a thin frame over `compare_logic`. It draws the filtered, ranked table and
//! pushes `Action::ApplyOption`; the model changes only through the Apply path.

use super::compare_logic::{self as logic, Dir, Filters, TileSync, Weight};
use super::kit::{self, space, Button, Tone};
use super::{Action, Cx};
use ddc_ui::theme::Role;
use crate::ai::options_keep;
use crate::units::Units;
use ddc_model::{ElKey, Model, OptionRec, OptionStatus, OptionStore};
use egui::{self, Align, Layout, RichText, Ui};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use web_time::UNIX_EPOCH;

/// Columns offered by default: (metric key, direction that counts as better).
const DEFAULT_METRICS: [(&str, Dir); 3] = [
    ("room_area", Dir::Max),
    ("cost", Dir::Min),
    ("findings_error", Dir::Min),
];

pub struct Compare {
    pub filters: Filters,
    pub weights: Vec<Weight>,
    pub tiles: TileSync,
    /// Differences only: options with the same numbers as an earlier one are hidden.
    pub diff_only: bool,
    pub cache: OptCache,
}

impl Default for Compare {
    fn default() -> Self {
        Self {
            filters: Filters {
                hide_blocked: true,
                rules: Vec::new(),
            },
            weights: DEFAULT_METRICS
                .iter()
                .map(|(k, d)| Weight {
                    key: (*k).to_string(),
                    weight: 1.0,
                    dir: *d,
                })
                .collect(),
            tiles: TileSync::default(),
            diff_only: false,
            cache: OptCache::default(),
        }
    }
}

/// Journal head, then either the model table's `(rows, rejected)` or the store file's
/// `(path, modified, length)` of the database and its WAL.
type Key = (i64, i64, (i64, i64), Option<(PathBuf, [(u128, u64); 2])>);

fn file_sig(p: &Path) -> (u128, u64) {
    std::fs::metadata(p).map_or((0, 0), |md| {
        let t = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
        (t.map_or(0, |d| d.as_nanos()), md.len())
    })
}

/// Journal head and option table stamp of the model: the two reads behind a [`Key`].
type Heads = ((i64, i64), (i64, i64));

fn heads_of(m: &Model) -> Heads {
    (
        m.ai_option_head().unwrap_or((0, 0)),
        m.ai_option_rev().unwrap_or((0, 0)),
    )
}

fn key_from((head, rev): Heads, store: Option<&Path>) -> Key {
    let (seq, stamp) = head;
    match store {
        Some(db) => {
            let wal = PathBuf::from(format!("{}-wal", db.display()));
            (seq, stamp, (0, 0), Some((db.to_path_buf(), [file_sig(db), file_sig(&wal)])))
        }
        None => (seq, stamp, rev, None),
    }
}

fn key_of(m: &Model, store: Option<&Path>) -> Key {
    key_from(heads_of(m), store)
}

/// Rows of the stored options, reloaded only when the journal head or the stored rows move.
/// An unsaved project's recipes are rows of the session store (`store`), as the tools see them.
#[derive(Default)]
pub struct OptCache {
    key: Option<Key>,
    store: Option<PathBuf>,
    pub rows: Vec<logic::Row>,
    pub rejected: HashSet<i64>,
    pub accepted: HashSet<i64>,
    /// Drafts whose base is no longer the model's journal head: they must be generated again.
    pub stale: HashSet<i64>,
    /// Existing elements each recipe names: what Show selects and frames.
    pub els: BTreeMap<i64, Vec<ElKey>>,
    pub loads: u32,
    /// Model reads of the key, taken once per editor cache key.
    heads: Option<((u64, u64), Heads)>,
}

impl OptCache {
    /// [`Self::sync`] for the live editor: an idle frame runs no SQL; a session store is still
    /// checked by its file stamps, which its own connection moves.
    pub fn sync_ed(&mut self, ed: &crate::editor::Editor) -> bool {
        let rk = ed.cache_key();
        let heads = match self.heads {
            Some((k, h)) if k == rk => h,
            _ => {
                let h = heads_of(&ed.model);
                self.heads = Some((rk, h));
                h
            }
        };
        let session = options_keep::live_session_dir();
        let src = options_keep::ui_store(&ed.model, session.as_deref());
        let store = src.as_ref().map(|s| s.1.as_path()).filter(|p| p.is_file());
        self.load(&ed.model, store, key_from(heads, store))
    }

    /// Rows where the tools keep them for this model and the live session. True when reloaded.
    pub fn sync(&mut self, m: &Model) -> bool {
        let session = options_keep::live_session_dir();
        let src = options_keep::ui_store(m, session.as_deref());
        self.sync_from(m, src.as_ref().map(|s| s.1.as_path()))
    }

    /// `store`: the session recipe database of an unsaved project (`None`: the model's table).
    pub fn sync_from(&mut self, m: &Model, store: Option<&Path>) -> bool {
        let store = store.filter(|p| p.is_file());
        self.load(m, store, key_of(m, store))
    }

    fn load(&mut self, m: &Model, store: Option<&Path>, key: Key) -> bool {
        let head = (key.0, key.1);
        let key = Some(key);
        if self.key == key {
            return false;
        }
        let list: Vec<OptionRec> = match store {
            Some(db) => OptionStore::open(db).and_then(|s| s.list(None)).unwrap_or_default(),
            None => m.ai_option_list(None).unwrap_or_default(),
        };
        // a store recipe Apply copied into the model reads as the model row does
        let status = |r: &OptionRec| match store {
            Some(_) => options_keep::model_twin(m, r)
                .filter(|t| options_keep::applied(t.status))
                .map_or(r.status, |t| t.status),
            None => r.status,
        };
        let marks: Vec<(i64, OptionStatus)> = list.iter().map(|r| (r.id, status(r))).collect();
        self.rows = list.iter().map(logic::row_from_rec).collect();
        self.rejected = marks.iter().filter(|x| x.1 == OptionStatus::Rejected).map(|x| x.0).collect();
        self.accepted = marks.iter().filter(|x| options_keep::applied(x.1)).map(|x| x.0).collect();
        self.stale = list.iter().filter(|r| is_stale(r, head)).map(|r| r.id).collect();
        self.els = list.iter().map(|r| (r.id, logic::option_elements(&r.envelope))).collect();
        self.store = store.map(Path::to_path_buf);
        self.key = key;
        self.loads += 1;
        true
    }

    /// Discards a draft through the same function as `discard_option`: the row stays, marked
    /// rejected, and its copy in `dir` goes to `_archive`. Returns the refusal text.
    pub fn discard(&mut self, m: &Model, id: i64, dir: Option<&Path>) -> Result<(), String> {
        match &self.store {
            Some(db) => {
                let s = OptionStore::open(db).map_err(|e| e.to_string())?;
                options_keep::discard(&s, dir, id)?;
            }
            None => {
                options_keep::discard(m, dir, id)?;
            }
        }
        self.rejected.insert(id);
        self.key = Some(key_of(m, self.store.as_deref()));
        Ok(())
    }
}

/// A recipe is stale when its base is not the model's current journal head.
pub fn is_stale(r: &OptionRec, head: (i64, i64)) -> bool {
    (r.base_head, r.base_stamp) != head
}

/// Apply is offered only to a fresh draft that is neither discarded nor already applied.
pub fn apply_offered(stale: bool, rejected: bool, accepted: bool) -> bool {
    !stale && !rejected && !accepted
}

pub fn tab(ui: &mut Ui, st: &mut Compare, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    st.cache.sync_ed(cx.ed);
    let cache = std::mem::take(&mut st.cache);
    table(ui, st, cx, &cache, (crate::units::active_units(), crate::ui_is_ru()), acts);
    st.cache = cache;
}

/// `fmt`: the project units and whether numbers read in Russian; explicit so a frame never
/// depends on global state it did not read itself.
fn table(
    ui: &mut Ui,
    st: &mut Compare,
    cx: &Cx<'_>,
    cache: &OptCache,
    fmt: (Units, bool),
    acts: &mut Vec<Action>,
) {
    let (units, ru) = fmt;
    let (rows, rejected, accepted) = (&cache.rows, &cache.rejected, &cache.accepted);
    let stale = &cache.stale;
    let (th, cat) = (cx.th, cx.cat);
    if rows.is_empty() {
        let key = logic::KEY_EMPTY;
        kit::empty_state(ui, th, &cat.t(key), &cat.t(&format!("{key}_hint")), None);
        return;
    }
    kit::checkbox(ui, th, &mut st.filters.hide_blocked, &cat.t("ai.cmp.hide_blocked"));
    kit::checkbox(ui, th, &mut st.diff_only, &cat.t("ai.cmp.diff_only"));
    ui.add_space(space::XS);
    let mut shown = logic::visible(rows, &st.filters);
    if let Some(key) = logic::empty_key(rows.len(), shown.len()) {
        kit::empty_state(ui, th, &cat.t(key), &cat.t(&format!("{key}_hint")), None);
        return;
    }
    if st.diff_only {
        let keys: Vec<String> = st.weights.iter().map(|w| w.key.clone()).collect();
        shown = logic::diff_rows(&shown, &keys);
    }
    let ranked = logic::ranking(&shown, &st.weights);
    let metrics: Vec<(String, Dir)> = st.weights.iter().map(|w| (w.key.clone(), w.dir)).collect();
    let front = logic::pareto(&shown, &metrics);
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("ai_cmp_table")
            .spacing([space::SM, space::XS])
            .show(ui, |ui| {
                kit::caption(ui, th, &cat.t("ai.cmp.col.option"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    kit::caption(ui, th, &cat.t("ai.cmp.col.score"));
                });
                for w in &st.weights {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        kit::caption(ui, th, &cat.t(&format!("ai.cmp.metric.{}", w.key)));
                    });
                }
                ui.end_row();
                for (id, score) in &ranked {
                    let Some(r) = shown.iter().find(|r| r.id == *id) else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&r.name)
                                .font(th.font(Role::Label))
                                .color(th.c("text.primary")),
                        );
                        if front.contains(id) {
                            kit::badge(ui, th, &cat.t("ai.cmp.pareto"), Tone::Success);
                        }
                        if rejected.contains(id) {
                            kit::badge(ui, th, &cat.t("ai.cmp.rejected"), Tone::Neutral);
                        }
                        if accepted.contains(id) {
                            kit::badge(ui, th, &cat.t("ai.cmp.accepted"), Tone::Success);
                        }
                        if stale.contains(id) && !rejected.contains(id) && !accepted.contains(id) {
                            kit::badge(ui, th, &cat.t("ai.cmp.stale"), Tone::Warning)
                                .on_hover_text(cat.t("ai.cmp.stale_tip"));
                        }
                    });
                    let numeric = |ui: &mut Ui, text: String| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(text).font(th.font(Role::Numeric)));
                        });
                    };
                    numeric(ui, logic::num_text(*score, 2, ru));
                    for w in &st.weights {
                        let text = r
                            .cells
                            .get(&w.key)
                            .map_or_else(|| "-".to_string(), |v| logic::metric_text(&w.key, *v, units, ru));
                        numeric(ui, text);
                    }
                    ui.horizontal(|ui| {
                        let els = cache.els.get(id).map_or(&[][..], Vec::as_slice);
                        let show_text = cat.t("ai.cmp.show");
                        let show = Button::quiet(&show_text).small().enabled(!els.is_empty());
                        let resp = show.show(ui, th);
                        let resp = if els.is_empty() {
                            resp.on_disabled_hover_text(cat.t("ai.cmp.show_none"))
                        } else {
                            resp.on_hover_text(cat.t("ai.cmp.show_tip"))
                        };
                        if resp.clicked() {
                            acts.push(Action::ShowApplied(els.to_vec()));
                        }
                        if !rejected.contains(id) && !accepted.contains(id) {
                            if apply_offered(stale.contains(id), false, false)
                                && Button::secondary(&cat.t("ai.cmp.apply"))
                                    .small()
                                    .show(ui, th)
                                    .clicked()
                            {
                                acts.push(Action::ApplyOption(*id));
                            }
                            // irreversible: kept apart from Apply by a gap and a divider
                            ui.add_space(space::MD);
                            ui.separator();
                            ui.add_space(space::MD);
                            if Button::danger(&cat.t("ai.cmp.discard"))
                                .small()
                                .show(ui, th)
                                .clicked()
                            {
                                acts.push(Action::DiscardOption(*id));
                            }
                        }
                    });
                    ui.end_row();
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(h: i64, st: i64) -> OptionRec {
        OptionRec {
            id: 1,
            set: "s".into(),
            name: "A".into(),
            base_head: h,
            base_stamp: st,
            envelope: String::new(),
            seed: 0,
            params: "{}".into(),
            generator: None,
            metrics: None,
            note: None,
            why: None,
            status: OptionStatus::Draft,
        }
    }

    #[test]
    fn stale_draft_has_badge_no_apply() {
        assert!(is_stale(&rec(3, 9), (4, 9)));
        assert!(is_stale(&rec(4, 8), (4, 9)));
        assert!(!apply_offered(true, false, false));
    }

    #[test]
    fn fresh_draft_has_apply() {
        assert!(!is_stale(&rec(4, 9), (4, 9)));
        assert!(apply_offered(false, false, false));
        assert!(!apply_offered(false, true, false));
        assert!(!apply_offered(false, false, true));
    }

    const METRICS: &str = r#"{"cells":[{"key":"room_area","unit":"m2","value":12.5},
        {"key":"cost","unit":"","value":1234.5},{"key":"findings_error","unit":"","value":3.0}],
        "findings":{"error":0}}"#;

    fn env_with(target: &str) -> String {
        format!(
            r#"{{"v":1,"actor":{{"kind":"user"}},"title":"opt","why":null,"groups":[{{"id":1,
            "title":"g","depends_on":[],"ops":[{{"op":"wall","action":"set","targets":{{"refs":["{target}"]}}}}]}}]}}"#
        )
    }

    fn add(ed: &crate::editor::Editor, name: &str, env: String) -> i64 {
        let (base_head, base_stamp) = ed.model.ai_option_head().unwrap();
        let mut r = rec(base_head, base_stamp);
        r.id = 0;
        r.name = name.into();
        r.envelope = env;
        r.metrics = Some(METRICS.into());
        ed.model.ai_option_add(&r).unwrap()
    }

    fn texts(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn walk(s: &egui::Shape, out: &mut String) {
            match s {
                egui::Shape::Text(t) => {
                    out.push_str(t.galley.text());
                    out.push('\n');
                }
                egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut out = String::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    /// One headless frame of the tab in language `lang`; the global UI language is not touched.
    fn render(st: &mut Compare, ed: &crate::editor::Editor, lang: ddc_ui::Lang, u: Units) -> String {
        let th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = ddc_ui::Catalog::new(lang);
        let ctx = ddc_ui::theme::headless_ctx();
        let cx = Cx { th: &th, cat: &cat, ed, view3d: false, palette_open: false };
        let mut acts = Vec::new();
        let mut shown = String::new();
        st.cache.sync_ed(ed);
        let cache = std::mem::take(&mut st.cache);
        for _ in 0..2 {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    table(ui, st, &cx, &cache, (u, lang == ddc_ui::Lang::Ru), &mut acts);
                });
            });
            shown = texts(&out.shapes);
        }
        st.cache = cache;
        shown
    }

    fn app() -> crate::shell::EditorApp {
        crate::shell::EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "test".into())
    }

    #[test]
    fn empty_tab_has_no_filters() {
        let a = app();
        let t = render(&mut Compare::default(), &a.ed, ddc_ui::Lang::En, Units::Metric);
        assert!(t.contains("No options yet"), "{t}");
        assert!(!t.contains("Differences only") && !t.contains("Hide options"), "{t}");
    }

    #[test]
    fn numbers_render_in_each_language() {
        let a = app();
        add(&a.ed, "Alpha", env_with("wall:5"));
        let en = render(&mut Compare::default(), &a.ed, ddc_ui::Lang::En, Units::Metric);
        assert!(en.contains("1,234.50") && en.contains("12.50"), "{en}");
        assert!(!en.contains("3.00"), "counts are whole: {en}");
        let ru = render(&mut Compare::default(), &a.ed, ddc_ui::Lang::Ru, Units::Metric);
        assert!(ru.contains("1\u{a0}234,50") && ru.contains("12,50"), "{ru}");
        let ft = render(&mut Compare::default(), &a.ed, ddc_ui::Lang::En, Units::Imperial);
        assert!(ft.contains("ft\u{b2}"), "{ft}");
    }

    #[test]
    fn differences_only_filters_rendered_rows() {
        let a = app();
        add(&a.ed, "Alpha", env_with("wall:5"));
        add(&a.ed, "Beta", env_with("wall:6"));
        let mut st = Compare::default();
        let all = render(&mut st, &a.ed, ddc_ui::Lang::En, Units::Metric);
        assert!(all.contains("Alpha") && all.contains("Beta"), "{all}");
        st.diff_only = true;
        let diff = render(&mut st, &a.ed, ddc_ui::Lang::En, Units::Metric);
        assert!(diff.contains("Alpha") && !diff.contains("Beta"), "{diff}");
    }

    #[test]
    fn show_selects_the_option_elements_and_leaves_the_model() {
        let mut a = app();
        let id = add(&a.ed, "Alpha", env_with("wall:5"));
        let mut st = super::super::PanelUi::default();
        st.cmp.cache.sync(&a.ed.model);
        let keys = st.cmp.cache.els[&id].clone();
        assert_eq!(keys, vec![("wall".to_string(), 5)]);
        let sig = |a: &crate::shell::EditorApp| {
            (
                a.ed.model.change_head(),
                a.ed.model.last_op_seq().unwrap(),
                a.ed.model.ai_option_head().unwrap(),
                a.ed.model.ai_option_get(id).unwrap().unwrap().status,
            )
        };
        let before = sig(&a);
        st.run(&mut a, Action::ShowApplied(keys));
        assert_eq!(a.ed.selection, vec![crate::editor::Sel::Wall(5)]);
        assert_eq!(sig(&a), before, "Show changes nothing in the model");
        assert!(a.ed.model.ai_option_get(id).unwrap().unwrap().status == OptionStatus::Draft);
    }

    #[test]
    fn discard_is_not_next_to_apply_in_source() {
        let src = include_str!("compare.rs");
        let table_src = &src[src.find("fn table(").unwrap()..src.find("#[cfg(test)]").unwrap()];
        let apply = table_src.find("ai.cmp.apply").unwrap();
        let discard = table_src.find("ai.cmp.discard").unwrap();
        assert!(apply < discard && table_src[apply..discard].contains("ui.separator()"));
        assert!(table_src.contains("Button::danger(&cat.t(\"ai.cmp.discard\"))"));
    }

    #[test]
    fn option_keys_both_languages() {
        for lang in ["en", "ru"] {
            let p = format!("{}/../ui/data/i18n/{lang}/ai_cmp.json", env!("CARGO_MANIFEST_DIR"));
            let t = std::fs::read_to_string(p).unwrap();
            for k in ["ai.cmp.show_tip\"", "ai.cmp.show_none\""] {
                assert!(t.contains(k), "{lang} lacks {k}");
            }
        }
    }

    #[test]
    fn stale_keys_both_languages() {
        for lang in ["en", "ru"] {
            let p = format!("{}/../ui/data/i18n/{lang}/ai_logic.json", env!("CARGO_MANIFEST_DIR"));
            let t = std::fs::read_to_string(p).unwrap();
            for k in ["ai.cmp.stale\"", "ai.cmp.stale_tip\""] {
                assert!(t.contains(k), "{lang} lacks {k}");
            }
        }
    }
}
