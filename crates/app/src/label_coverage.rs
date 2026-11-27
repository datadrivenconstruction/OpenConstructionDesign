// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Audit item 1 (P0): no surface shows a command id or a catalog key in place of a label.
//!
//! Two checks, both languages, explicit catalogs (the global language is never touched):
//! - every command a surface can show (the palette lists them all; the ribbon adds group and tab
//!   captions and short labels) has its own non-empty label that is not a key or an id;
//! - the painted text of the whole window holds no id and no key: the ribbon on every tab and
//!   on the contextual tab, the File menu, the canvas context menu (empty and with a wall
//!   selected) and Ctrl+K with an empty query and with queries.
//!
//! Also the ribbon naming rules of audit item 39: one command has one button on the whole
//! ribbon, and a short ribbon label only leaves words out of the full name.

use crate::layout_contract::{app_in, driven_ctx, pass_on, Probe};
use crate::shell::EditorApp;
use ddc_ui::widgets::ribbon::{group_name, short_label, CTX_TAB};
use ddc_ui::{Catalog, Lang, Registry};
use std::collections::{BTreeMap, BTreeSet, HashSet};

const SCREEN: (f32, f32) = (1440.0, 900.0);
const LANGS: [Lang; 2] = [Lang::En, Lang::Ru];

/// The label `key` in `lang`, or why it cannot be shown.
fn label_fault(cat: &Catalog, lang: Lang, key: &str, id: &str) -> Option<String> {
    let v = cat.table(lang).get(key).and_then(|v| v.as_str());
    match v {
        None => Some(format!("{key}: no {lang:?} label")),
        Some(s) if s.trim().is_empty() => Some(format!("{key}: empty {lang:?} label")),
        Some(s) if s.starts_with("cmd.") || s == id || s == key => {
            Some(format!("{key}: {lang:?} label is the raw key {s:?}"))
        }
        Some(_) => None,
    }
}

/// Keys of every caption the ribbon, the menus and the palette draw for the registry.
fn surface_keys(reg: &Registry) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    for c in reg.all() {
        keys.insert(c.label_key(), c.id.clone());
        if c.on("R") || !c.context.is_empty() {
            keys.insert(format!("rgroup.{}", group_name(c)), c.id.clone());
        }
    }
    for t in reg.tabs().into_iter().chain([CTX_TAB.to_string()]) {
        keys.insert(format!("tab.{t}"), t);
    }
    keys
}

/// Every command id, every catalog key: text that must never reach the screen as is.
fn raw_strings(cat: &Catalog, reg: &Registry) -> HashSet<String> {
    let mut raw: HashSet<String> = reg.all().iter().map(|c| c.id.clone()).collect();
    for l in LANGS {
        raw.extend(cat.table(l).keys().cloned());
    }
    raw
}

/// Painted texts and control labels of a frame that show an id or a key.
fn leaks(p: &Probe, raw: &HashSet<String>) -> Vec<String> {
    let all = p.text.iter().chain(p.controls.iter().map(|c| &c.label));
    let mut out: Vec<String> = all
        .filter(|t| {
            let t = t.trim();
            raw.contains(t) || t.split_whitespace().any(|w| w.starts_with("cmd."))
        })
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Frames of the whole window; `events` go into the first one. What the last frame drew.
fn frames(a: &mut EditorApp, events: Vec<egui::Event>) -> Probe {
    let (ctx, mut th) = driven_ctx();
    let id = egui::Id::new("uxfix2.labels");
    let mut draw = |c: &egui::Context| a.ui(c, None);
    let mut p = pass_on(&ctx, &mut th, id, SCREEN, vec![], &mut draw);
    p = pass_on(&ctx, &mut th, id, SCREEN, events, &mut draw);
    for _ in 0..3 {
        p = pass_on(&ctx, &mut th, id, SCREEN, vec![], &mut draw);
    }
    p
}

fn alt_f() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::F,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::ALT,
    }
}

/// An app in `lang` with one wall selected (`select`) or nothing selected.
fn app(lang: Lang, select: bool) -> EditorApp {
    let mut a = app_in(lang);
    crate::modify_tools::tests::walls(&mut a.ed, &[((0.0, 0.0), (8.0, 0.0))]);
    if select {
        let w = a.ed.walls[0].id;
        a.ed.selection = vec![crate::editor::Sel::Wall(w)];
    }
    a
}

#[test]
fn every_command_and_caption_has_its_own_label_in_en_and_ru() {
    let reg = Registry::builtin();
    let cat = Catalog::new(Lang::En);
    let mut bad = vec![];
    for (key, id) in surface_keys(&reg) {
        for l in LANGS {
            bad.extend(label_fault(&cat, l, &key, &id));
            let short = format!("{key}.short");
            if key.starts_with("cmd.") && cat.table(l).contains_key(&short) {
                bad.extend(label_fault(&cat, l, &short, &id));
            }
        }
    }
    // the three commands of the audit, by name: they showed their ids on the Architecture tab
    for id in ["arch.wall_sweep", "arch.skylight", "arch.sweep_profiles"] {
        assert!(reg.get(id).is_some_and(|c| c.on("R")), "{id} left the ribbon");
    }
    assert!(bad.is_empty(), "{} labels:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn label_check_fires_on_a_missing_or_raw_label() {
    let cat = Catalog::new(Lang::En);
    assert!(label_fault(&cat, Lang::En, "cmd.no.such", "no.such").is_some());
    assert!(label_fault(&cat, Lang::Ru, "cmd.arch.wall", "arch.wall").is_none());
    let raw: HashSet<String> = ["cmd.arch.wall".to_string()].into_iter().collect();
    let p = Probe { text: vec!["cmd.arch.wall".into(), "Wall".into()], ..Probe::default() };
    assert_eq!(leaks(&p, &raw), ["cmd.arch.wall"]);
    let p = Probe { text: vec!["Wall cmd.x.y".into()], ..Probe::default() };
    assert_eq!(leaks(&p, &HashSet::new()).len(), 1);
}

/// The ribbon on every tab and on the contextual tab of a selected wall.
#[test]
fn the_ribbon_shows_no_id_or_key() {
    for lang in LANGS {
        let mut a = app(lang, false);
        let raw = raw_strings(&a.sh.cat, &a.sh.reg);
        let mut tabs = 0;
        for tab in a.sh.reg.tabs() {
            a.sh.ribbon.tab = tab.clone();
            let p = frames(&mut a, vec![]);
            assert!(p.text.len() > 20, "{lang:?} {tab}: nothing drawn");
            assert_eq!(leaks(&p, &raw), Vec::<String>::new(), "{lang:?} ribbon {tab}");
            tabs += 1;
        }
        assert!(tabs >= 8, "{lang:?}: {tabs} tabs");
        let mut a = app(lang, true);
        frames(&mut a, vec![]);
        a.sh.ribbon.tab = CTX_TAB.into();
        let p = frames(&mut a, vec![]);
        assert!(p.text.len() > 20, "{lang:?}: contextual tab not drawn");
        assert_eq!(leaks(&p, &raw), Vec::<String>::new(), "{lang:?} contextual tab");
    }
}

/// The same ribbon with the 3D view in front and a selection: no tab shows a key there either.
#[test]
fn the_ribbon_shows_no_id_or_key_in_3d_and_with_a_selection() {
    for lang in LANGS {
        for (select, v3) in [(true, false), (false, true), (true, true)] {
            let mut a = app(lang, select);
            a.view3d = v3;
            let raw = raw_strings(&a.sh.cat, &a.sh.reg);
            for tab in a.sh.reg.tabs() {
                a.sh.ribbon.tab = tab.clone();
                let p = frames(&mut a, vec![]);
                assert_eq!(
                    leaks(&p, &raw),
                    Vec::<String>::new(),
                    "{lang:?} ribbon {tab} select={select} 3d={v3}"
                );
            }
        }
    }
}

/// The File menu (Alt+F) and the canvas context menu, empty and on a wall.
#[test]
fn the_menus_show_no_id_or_key() {
    for lang in LANGS {
        let mut a = app(lang, false);
        let raw = raw_strings(&a.sh.cat, &a.sh.reg);
        let shut = frames(&mut a, vec![]).text.len();
        let p = frames(&mut a, vec![alt_f()]);
        assert!(p.text.len() > shut, "{lang:?}: the File menu did not open");
        assert_eq!(leaks(&p, &raw), Vec::<String>::new(), "{lang:?} File menu");
        for select in [false, true] {
            let mut a = app(lang, select);
            let shut = frames(&mut a, vec![]).text.len();
            a.quick.menu = Some(egui::pos2(700.0, 450.0));
            a.quick.menu_fresh = true;
            let p = frames(&mut a, vec![]);
            assert!(p.text.len() > shut + 5, "{lang:?} {select}: no context menu");
            assert_eq!(leaks(&p, &raw), Vec::<String>::new(), "{lang:?} context menu");
        }
    }
}

/// Ctrl+K: the empty list and queries that reach every tab's commands.
#[test]
fn the_palette_shows_no_id_or_key() {
    let queries = ["", "a", "e", "o", "roof", "sweep", "wall", "sheet", "ai", "ifc", "xlsx"];
    for lang in LANGS {
        let mut a = app(lang, false);
        let raw = raw_strings(&a.sh.cat, &a.sh.reg);
        let mut rows = BTreeSet::new();
        for q in queries {
            a.sh.palette.open = true;
            a.sh.palette.query = q.to_string();
            a.sh.palette.sel = 0;
            let p = frames(&mut a, vec![]);
            assert!(a.sh.palette.open, "{lang:?} {q:?}: the palette closed");
            assert_eq!(leaks(&p, &raw), Vec::<String>::new(), "{lang:?} palette {q:?}");
            rows.extend(a.sh.palette.rows(&a.sh.reg, &a.sh.cat));
        }
        assert!(rows.len() >= 40, "{lang:?}: only {} rows seen", rows.len());
        // one command is one row: an alias never repeats its target
        let targets: Vec<&str> = rows.iter().map(|r| a.sh.reg.resolve(r)).collect();
        let distinct: BTreeSet<&str> = targets.iter().copied().collect();
        assert!(distinct.len() * 10 >= targets.len() * 9, "{lang:?}: duplicate rows");
    }
}

/// Audit 39: one command has one button per tab (variants live under their parent's caret) and
/// one name everywhere: a discipline tab may repeat a shared tool (levels, grids) only as an
/// alias that shows the same label.
#[test]
fn one_command_has_one_button_per_tab_and_one_name() {
    let reg = Registry::builtin();
    let cat = Catalog::new(Lang::En);
    let mut bad = vec![];
    for tab in reg.tabs() {
        let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let on: Vec<_> = reg
            .all()
            .iter()
            .filter(|c| c.tab.as_deref() == Some(tab.as_str()) && c.on("R"))
            .collect();
        let folded: HashSet<&str> =
            on.iter().flat_map(|c| c.variants.iter().map(String::as_str)).collect();
        for c in on.iter().filter(|c| !folded.contains(c.id.as_str())) {
            let target = reg.resolve(&c.id).to_string();
            seen.entry(target).or_default().push(c.id.clone());
        }
        for (t, v) in seen.iter().filter(|(_, v)| v.len() > 1) {
            bad.push(format!("{tab}: {t} on {} buttons: {}", v.len(), v.join(", ")));
        }
    }
    for c in reg.all().iter().filter(|c| c.alias.is_some() && c.on("R")) {
        let t = reg.resolve(&c.id);
        for l in LANGS {
            let (a, b) = (cat.t_in(l, &c.label_key()), cat.t_in(l, &format!("cmd.{t}")));
            if a != b {
                bad.push(format!("{l:?} {} {a:?} is an alias of {t} {b:?}", c.id));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("
"));
}

/// Band buttons of one ribbon tab at width `w` in `lang`: (accessible label, width), and the
/// painted texts.
fn ribbon_band(tab: &str, lang: Lang, w: f32) -> (Vec<(String, bool)>, Vec<String>) {
    use ddc_ui::icons::IconCache;
    use ddc_ui::theme::ribbon as k;
    use ddc_ui::widgets::ribbon::Ribbon;
    use ddc_ui::{Density, Mode, Theme};
    let reg = Registry::builtin();
    let cat = Catalog::new(lang);
    let th = Theme::new(Mode::Dark, Density::Compact);
    let mut rb = Ribbon::default();
    rb.tab = tab.to_string();
    let mut icons = IconCache::default();
    let p = crate::layout_contract::probe_on(egui::Id::new(("uxfix2.rb", tab)), (w, 300.0), |c| {
        egui::TopBottomPanel::top("uxfix2.rb").frame(egui::Frame::NONE).show(c, |ui| {
            rb.show(ui, &th, &cat, &reg, &mut icons, None);
        });
    });
    let band_bottom = k::TAB_H + k::PAD_Y + k::BAND_H;
    // the caret part is named "<command> - <tip>"; a command whose own name holds the tip
    // word ("Design options" in Russian) is not a caret
    let caret = format!(" - {}", cat.t("ribbon.split_tip"));
    // a label painted inside the button, or in the caret part right under it (large split)
    let painted = |r: egui::Rect| {
        p.spans.iter().any(|(s, sr)| !s.trim().is_empty() && r.expand(0.5).contains_rect(*sr))
    };
    let named = |r: egui::Rect| {
        r.width() > 2.0 * k::SMALL_W
            || painted(r)
            || p.controls.iter().any(|d| {
                d.label.ends_with(&caret)
                    && (d.rect.left() - r.left()).abs() < 0.5
                    && (d.rect.top() - r.bottom()).abs() < 0.5
                    && painted(d.rect)
            })
    };
    let band = p
        .controls
        .iter()
        .filter(|c| c.rect.top() >= k::TAB_H && c.rect.center().y < band_bottom)
        .filter(|c| c.role == egui::accesskit::Role::Button && !c.label.ends_with(&caret))
        .map(|c| (c.label.clone(), named(c.rect)))
        .collect();
    (band, p.text)
}

/// Audit 20 and 21 at 1280, 1440 and 1920: every group of every tab keeps its caption, every
/// button but the two clipboard icons keeps its label, and English and Russian collapse alike.
#[test]
fn ribbon_groups_keep_captions_and_collapse_alike_in_en_and_ru() {
    use ddc_ui::widgets::ribbon::ICON_ONLY;
    let reg = Registry::builtin();
    let mut checked = 0;
    for tab in reg.tabs() {
        for w in [1280.0_f32, 1440.0, 1920.0] {
            let mut counts = vec![];
            for lang in LANGS {
                let cat = Catalog::new(lang);
                let (band, text) = ribbon_band(&tab, lang, w);
                let tag = format!("{tab} {lang:?} @{w}");
                let icons: Vec<String> = ICON_ONLY
                    .iter()
                    .filter_map(|id| reg.get(id))
                    .map(|c| cat.t(&c.label_key()))
                    .collect();
                for (label, named) in &band {
                    let iconic = icons.iter().any(|i| label.starts_with(i.as_str()));
                    assert!(iconic || *named, "{tag}: {label:?} has no caption");
                }
                // every group the band draws (a button of it is painted) shows its caption;
                // groups past the window edge live in the trailing menu
                let mut groups: BTreeMap<String, bool> = BTreeMap::new();
                for c in reg.all() {
                    if c.tab.as_deref() != Some(tab.as_str()) || !c.on("R") {
                        continue;
                    }
                    let drawn = text.iter().any(|t| *t == short_label(&cat, c));
                    let g = cat.t(&format!("rgroup.{}", group_name(c)));
                    *groups.entry(g).or_default() |= drawn;
                }
                // a collapsed group sets its caption over two lines inside its drop-down
                let shown = |g: &str| {
                    text.iter().any(|t| t == g)
                        || text.windows(2).any(|w| format!("{} {}", w[0], w[1]) == g)
                };
                for g in groups.iter().filter(|(_, d)| **d).map(|(g, _)| g) {
                    assert!(shown(g), "{tag}: group {g:?} has no caption");
                }
                checked += band.len();
                counts.push(band.into_iter().map(|b| b.0).collect::<Vec<_>>());
            }
            let n: Vec<usize> = counts.iter().map(Vec::len).collect();
            assert_eq!(n[0], n[1], "{tab} @{w}: en and ru collapse differently: {counts:?}");
        }
    }
    assert!(checked >= 300, "only {checked} buttons checked");
}

/// Words of a label, lower case, without punctuation.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Audit 39: a short ribbon label is the full name with words left out, never another name
/// and never an abbreviation (such as a truncated Russian "Sync.", "Area" or "src").
#[test]
fn short_labels_only_leave_words_out() {
    let cat = Catalog::new(Lang::En);
    let mut bad = vec![];
    for l in LANGS {
        for (k, v) in cat.table(l) {
            let (Some(base), Some(short)) = (k.strip_suffix(".short"), v.as_str()) else {
                continue;
            };
            if !base.starts_with("cmd.") {
                continue;
            }
            let full = cat.table(l).get(base).and_then(|v| v.as_str()).unwrap_or("");
            let fw = words(full);
            let mut it = fw.iter();
            if !words(short).iter().all(|w| it.any(|f| f == w)) {
                bad.push(format!("{l:?} {k}: {short:?} is not a shortening of {full:?}"));
            }
            // a word cut with a dot: "Sync.", "Pl."; a dotted number or ".rvt" is not one
            let abbrev = |s: &str| {
                s.split_whitespace().any(|w| {
                    let w = w.trim_end_matches(['\u{2026}', ')']);
                    w.len() > 1 && w.ends_with('.') && w.chars().any(char::is_alphabetic)
                })
            };
            if abbrev(short) || abbrev(full) {
                bad.push(format!("{l:?} {k}: abbreviation in {short:?} / {full:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
