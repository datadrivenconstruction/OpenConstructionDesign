//! Looks and the setup wizard drawn as the whole app, headless, in both languages at 1280, 1440
//! and 1920 points: every preset lands in the theme; nothing leaves the window, no control
//! text is cut, no two buttons overlap and every form row keeps its line (the uipol1 row
//! probe) — in the main window, each wizard step, each settings page and the dialogs; each
//! focus builds its workspace; settings, mode layouts and the wizard's state survive a restart.
use crate::ai::ui::tests_modes::{Shell, SIZES};
use crate::layout_contract::{row_faults, violations, Probe};
use crate::prefs::Prefs;
use crate::shell::mode_layout::{Layout, WorkMode};
use crate::shell::onboard::{self, Focus, STEPS};
use ddc_ui::looks;
use ddc_ui::theme::parse_hex;
use ddc_ui::Lang;
use egui::{self, accesskit::Role};

const EPS: f32 = 0.5;

fn shell(lang: Lang, size: (f32, f32), look: &str) -> Shell {
    let mut sh = Shell::new(lang, size);
    sh.on_modal = true;
    sh.app.use_look(look);
    sh.settle();
    sh
}

/// Buttons whose rects cross without one holding the other.
fn overlaps(p: &Probe) -> Vec<String> {
    let b: Vec<_> = p
        .controls
        .iter()
        .filter(|c| c.role == Role::Button && c.rect.is_positive())
        .collect();
    let mut bad = vec![];
    for (i, x) in b.iter().enumerate() {
        for y in &b[i + 1..] {
            let s = x.rect.intersect(y.rect);
            let nested = x.rect.expand(EPS).contains_rect(y.rect)
                || y.rect.expand(EPS).contains_rect(x.rect);
            if s.width() > 1.0 && s.height() > 1.0 && !nested {
                bad.push(format!("{:?} {:?} × {:?} {:?}", x.label, x.rect, y.label, y.rect));
            }
        }
    }
    bad
}

/// Every control inside the window, no control text cut, no buttons overlapping.
fn whole(p: &Probe, size: (f32, f32)) -> Vec<String> {
    let sr = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.0, size.1));
    let mut bad = vec![];
    for c in &p.controls {
        if !sr.expand(EPS).contains_rect(c.rect) {
            bad.push(format!("{:?} {:?} {:?} leaves the window", c.role, c.label, c.rect));
        }
    }
    for e in &p.elided {
        if p.controls.iter().any(|c| c.label.trim() == e.trim()) {
            bad.push(format!("control text {e:?} is cut"));
        }
    }
    bad.extend(overlaps(p));
    bad
}

/// What the modal window itself drew: everything the app drew without it removed.
fn modal_part(mut p: Probe, blank: &Probe) -> Probe {
    // the app behind may re-lay out while the window is up (a focus applied on a step): only
    // what touches the window counts, so a control leaking out of it is still caught
    let near = p.inner.map(|w| w.expand(24.0));
    let touches = |r: egui::Rect| near.is_none_or(|w| w.intersects(r));
    p.controls.retain(|c| touches(c.rect) && !blank.controls.iter().any(|b| b.id == c.id));
    p.spans.retain(|s| touches(s.1));
    p.clips.retain(|c| touches(c.0));
    p.lines.retain(|l| touches(l.rect) && !blank.lines.iter().any(|b| b.text == l.text && b.rect == l.rect));
    p.elided.retain(|e| !blank.elided.contains(e));
    p.spans.retain(|s| !blank.spans.contains(s));
    p.clips.retain(|c| !blank.clips.contains(c));
    p
}

/// The window or dialog contract plus the uipol1 form-row probe.
fn faults(p: &Probe, size: (f32, f32)) -> Vec<String> {
    let mut bad = whole(p, size);
    bad.extend(violations(p, false));
    bad.extend(row_faults(p, true));
    bad
}

/// Stack of a worker: the whole app is laid out in debug builds, deeper than a test thread's.
const STACK: usize = 32 << 20;

/// `f` for every look on a few threads, faults joined in look order. Each look builds its own
/// shells (every piece of state the shell reads is per thread), so the looks share nothing; a
/// panic in one is raised again here with its message.
fn par_looks(f: impl Fn(&'static looks::Look) -> Vec<String> + Sync) -> Vec<String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let all = looks::presets();
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let workers = cores.clamp(1, 4).min(all.len());
    let next = AtomicUsize::new(0);
    let out = std::sync::Mutex::new(vec![Vec::new(); all.len()]);
    std::thread::scope(|s| {
        let work = || loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(l) = all.get(i) else { break };
            let v = f(l);
            out.lock().unwrap()[i] = v;
        };
        let hs: Vec<_> = (0..workers)
            .map(|_| std::thread::Builder::new().stack_size(STACK).spawn_scoped(s, work).unwrap())
            .collect();
        for h in hs {
            if let Err(e) = h.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
    out.into_inner().unwrap().into_iter().flatten().collect()
}

fn report(tag: &str, bad: Vec<String>, all: &mut Vec<String>) {
    all.extend(bad.into_iter().map(|b| format!("{tag}: {b}")));
}

fn finish(all: Vec<String>) {
    assert!(all.is_empty(), "{} faults:\n{}", all.len(), all.join("\n"));
}

/// The main window in every look: drawn whole, and the look is what it drew with.
fn app_in_every_look(lang: Lang) {
    finish(par_looks(|l| {
        let mut all = vec![];
        for size in SIZES {
            let mut sh = shell(lang, size, &l.id);
            let p = sh.settle();
            let tag = format!("{} {lang:?} {size:?}", l.id);
            report(&tag, whole(&p, size), &mut all);
            let a = &sh.app;
            let t = &a.sh.theme;
            let (lp, dp) = &*looks::patches_cached(l, None);
            let patch = if t.mode == ddc_ui::Mode::Dark { dp } else { lp };
            for (k, v) in patch {
                assert_eq!(t.c(k), parse_hex(v).unwrap(), "{tag}: {k}");
            }
            assert_eq!(t.font, l.face(), "{tag}");
            assert_eq!(t.text_scale, l.text_size, "{tag}");
            assert_eq!(t.scale, l.ui_scale, "{tag}");
            let set = ddc_ui::icons::set(&l.icons).unwrap().style();
            assert_eq!(a.sh.icons.style(), set, "{tag}");
            let ribbon = crate::shell::looks_ui::ribbon_now(&a.prefs);
            assert_eq!(ribbon, l.ribbon_look(), "{tag}");
            if l.theme != "system" {
                let dark = l.theme == "dark";
                let want = if dark { ddc_ui::Mode::Dark } else { ddc_ui::Mode::Light };
                assert_eq!(t.mode, want, "{tag}");
            }
        }
        all
    }));
}

#[test]
fn app_in_every_look_en() {
    app_in_every_look(Lang::En);
}

#[test]
fn app_in_every_look_ru() {
    app_in_every_look(Lang::Ru);
}

/// Every wizard step in every look: whole, rows on their lines, the step's controls named.
fn wizard_steps(lang: Lang) {
    finish(par_looks(|l| {
        let mut all = vec![];
        for size in SIZES {
            let mut sh = shell(lang, size, &l.id);
            // two focuses chosen, so the step shows a main badge and a «make main» button
            sh.app.set_focus(None, &[Focus::Data, Focus::Ai]);
            let blank = sh.settle();
            sh.app.open_onboarding(0);
            for step in 0..STEPS {
                sh.app.chrome.onboard.step = step;
                let raw = sh.settle();
                let p = modal_part(raw, &blank);
                let tag = format!("wizard {step} {} {lang:?} {size:?}", l.id);
                report(&tag, faults(&p, size), &mut all);
                let cat = &sh.app.sh.cat;
                let named = |label: &str| p.controls.iter().any(|c| c.label == label);
                let next = if step + 1 == STEPS { "onb.start" } else { "onb.next" };
                for k in [next, "onb.skip"] {
                    if !named(&cat.t(k)) {
                        all.push(format!("{tag}: no {k}"));
                    }
                }
                let want: Vec<String> = match step {
                    0 => Focus::ALL.iter().map(|f| cat.t(&format!("onb.focus.{}", f.key()))).collect(),
                    1 => looks::presets().iter().map(|k| cat.t(&k.name_key())).collect(),
                    2 => ddc_ui::icons::sets()
                        .iter()
                        .map(|s| cat.t(&format!("theme.icons.{}", s.id)))
                        .collect(),
                    _ => vec![],
                };
                for w in want {
                    if !named(&w) {
                        all.push(format!("{tag}: no control {w:?}"));
                    }
                }
            }
        }
        all
    }));
}

#[test]
fn wizard_steps_en() {
    wizard_steps(Lang::En);
}

#[test]
fn wizard_steps_ru() {
    wizard_steps(Lang::Ru);
}

/// Every settings page in every look and both languages.
fn settings_pages(lang: Lang) {
    finish(par_looks(|l| {
        let mut all = vec![];
        for size in SIZES {
            let mut sh = shell(lang, size, &l.id);
            let blank = sh.settle();
            for page in 0..crate::shell::settings_win::PAGE_COUNT as u8 {
                sh.app.open_settings(page);
                sh.app.chrome.settings.query.clear();
                let p = modal_part(sh.settle(), &blank);
                let tag = format!("settings {page} {} {lang:?} {size:?}", l.id);
                report(&tag, faults(&p, size), &mut all);
                if page == crate::shell::settings_win::PAGE_APPEARANCE {
                    for k in looks::presets() {
                        let name = sh.app.sh.cat.t(&k.name_key());
                        if !p.controls.iter().any(|c| c.label == name) {
                            all.push(format!("{tag}: no card {name:?}"));
                        }
                    }
                }
            }
            sh.app.settings_open = false;
        }
        all
    }));
}

#[test]
fn settings_pages_en() {
    settings_pages(Lang::En);
}

#[test]
fn settings_pages_ru() {
    settings_pages(Lang::Ru);
}

/// Dialogs the palette opens inside the app, on the test room with a wall selected.
const DIALOGS: [&str; 7] = [
    "mgr.level",
    "mgr.level_rename",
    "mgr.purge",
    "data.export_pdf",
    // the glTF export asks the system's save dialog, which the app does not draw; the clash
    // window is the data tab's own dialog in its place
    "data.clash",
    "data.export_stl",
    "app.where_is",
];

fn dialogs(lang: Lang) {
    finish(par_looks(|l| {
        let mut all = vec![];
        for size in SIZES {
            let mut sh = shell(lang, size, &l.id);
            sh.app.ed.set_mode(crate::wall_tool::Mode::Polyline);
            crate::schedule::test_room(&mut sh.app.ed);
            sh.app.ed.set_mode(crate::wall_tool::Mode::Select);
            // the PDF export prints sheets: without one it only warns and opens nothing
            let sheet = ddc_model::SheetDef::new("A-101", "Plan");
            sh.app.ed.model.add_sheet(&sheet).unwrap();
            let blank = sh.settle();
            for id in DIALOGS {
                assert!(sh.app.run(id), "{id} runs");
                let full = sh.settle();
                let tag = format!("{id} {} {lang:?} {size:?}", l.id);
                // the dialog is up: something new is on screen
                if full.controls.len() <= blank.controls.len() && full.text == blank.text {
                    all.push(format!("{tag}: nothing opened"));
                }
                let p = modal_part(full, &blank);
                let bad = faults(&p, size);
                if !bad.is_empty() {
                    eprintln!("{}", crate::layout_contract::dump(&tag, &p, &bad));
                }
                report(&tag, bad, &mut all);
                let esc = |pressed| egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: Some(egui::Key::Escape),
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                };
                sh.pass(vec![esc(true), esc(false)]);
                sh.settle();
            }
        }
        all
    }));
}

#[test]
fn dialogs_en() {
    dialogs(Lang::En);
}

#[test]
fn dialogs_ru() {
    dialogs(Lang::Ru);
}

/// Each focus builds its workspace: panels of its mode, ribbon order and tab, view.
#[test]
fn each_focus_builds_its_workspace() {
    for f in Focus::ALL {
        let mut sh = Shell::new(Lang::En, SIZES[1]);
        let id = format!("ui.focus.{}", f.key());
        assert!(sh.app.run(&id), "{id}");
        let p = sh.settle();
        let a = &sh.app;
        let tag = f.key();
        assert_eq!(a.prefs.focus, f.key(), "{tag}");
        let base = Layout::capture(&Prefs::default());
        let want = f.layout(&base);
        assert_eq!(a.prefs.browser_open, want.browser_open, "{tag}: browser");
        assert_eq!(a.prefs.props_open, want.props_open, "{tag}: properties");
        assert_eq!(a.prefs.table_height, want.table_height, "{tag}: table");
        assert_eq!(a.prefs.ai_panel_open, want.ai_panel_open, "{tag}: assistant");
        assert_eq!(a.prefs.mode_layouts.modes.get(&f.mode()), Some(&want), "{tag}: mode layout");
        let order = onboard::tab_order(&[f]);
        assert_eq!(a.sh.ribbon.tab_order, order, "{tag}");
        // the ribbon strip draws the focus's tabs first
        let tabs: Vec<String> = a.sh.reg.tabs();
        let names: Vec<String> =
            ddc_ui::widgets::ribbon::ordered_tabs(tabs, &order)
                .iter()
                .map(|t| a.sh.cat.t(&format!("tab.{t}")))
                .collect();
        // the probe lists controls in no layout order: read the strip left to right
        let mut strip: Vec<_> = p.controls.iter().filter(|c| names.contains(&c.label)).collect();
        strip.sort_by(|a, b| a.rect.min.x.total_cmp(&b.rect.min.x));
        let drawn: Vec<&str> = strip.iter().map(|c| c.label.as_str()).collect();
        if !order.is_empty() {
            assert_eq!(drawn.first().copied(), Some(names[0].as_str()), "{tag}: {drawn:?}");
        }
        match f {
            Focus::Model => {
                assert!(a.view3d, "{tag}: 3D");
                assert_eq!(a.sh.ribbon.tab, "arch");
            }
            Focus::Drawings => {
                assert!(!a.view3d, "{tag}: plan");
                assert_eq!(a.sh.ribbon.tab, "annotate");
            }
            Focus::Data => {
                assert!(!a.view3d, "{tag}: plan");
                assert_eq!(a.sh.ribbon.tab, "data");
            }
            Focus::Ai => {
                assert!(crate::ai::ui::modes::current().is_some(), "{tag}: AI tab");
            }
        }
        assert_eq!(a.chrome.work_mode, Some(crate::shell::mode_layout::current(a)), "{tag}");
        crate::ai::ui::modes::leave();
    }
    // two focuses: both modes get their panels, the primary's tabs lead
    let mut sh = Shell::new(Lang::En, SIZES[1]);
    sh.app.set_focus(None, &[Focus::Drawings, Focus::Data]);
    let base = Layout::capture(&Prefs::default());
    let m = &sh.app.prefs.mode_layouts.modes;
    assert_eq!(m.get(&WorkMode::Data), Some(&Focus::Data.layout(&base)));
    assert_eq!(sh.app.sh.ribbon.tab_order[..2], ["annotate", "view"]);
    assert_eq!(sh.app.sh.ribbon.tab_order[2..], ["data", "manage"]);
}

fn temp_prefs(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ddc-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("prefs.txt")
}

fn key(k: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    let ev = |pressed| egui::Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers };
    vec![ev(true), ev(false)]
}

/// First start opens the wizard; Esc skips it and it stays away after a restart; the command
/// opens it again; Enter walks to the end and «Start» finishes with every choice saved.
#[test]
fn wizard_first_start_skip_rerun_and_finish() {
    let path = temp_prefs("onb");
    let _ = std::fs::remove_file(&path);
    let mut sh = Shell::new(Lang::Ru, SIZES[0]);
    sh.app.prefs.path = Some(path.clone());
    sh.app.prefs.onboarded = false;
    sh.app.prefs.first_run_done = false;
    sh.settle();
    assert!(sh.app.chrome.onboard.open, "first start opens the wizard");
    sh.pass(key(egui::Key::Escape, egui::Modifiers::NONE));
    sh.settle();
    assert!(!sh.app.chrome.onboard.open, "Esc skips");
    let back = Prefs::load_from(Some(path.clone()));
    assert!(back.onboarded, "skipped is remembered");
    // a restart: the saved prefs, the wizard stays closed
    let mut sh = Shell::new(Lang::Ru, SIZES[0]);
    sh.app.prefs = back;
    sh.settle();
    assert!(!sh.app.chrome.onboard.open, "not again after a skip");
    // on demand: the command, Settings and Help open it
    assert!(sh.app.run("ui.onboarding"));
    sh.settle();
    assert!(sh.app.chrome.onboard.open);
    assert_eq!(sh.app.chrome.onboard.step, 0);
    // focus step: Space on the first card (focused on entry) chooses AI
    sh.pass(key(egui::Key::Space, egui::Modifiers::NONE));
    sh.settle();
    assert_eq!(sh.app.prefs.focus, "ai");
    // Alt+→ walks on, Alt+← back
    sh.pass(key(egui::Key::ArrowRight, egui::Modifiers::ALT));
    sh.settle();
    assert_eq!(sh.app.chrome.onboard.step, 1);
    sh.pass(key(egui::Key::ArrowLeft, egui::Modifiers::ALT));
    sh.settle();
    assert_eq!(sh.app.chrome.onboard.step, 0);
    // choices of the later steps are live and saved
    sh.app.chrome.onboard.step = onboard::STEP_LOOK;
    sh.app.use_look("night");
    sh.app.run("ui.icons.filled");
    sh.app.run("ui.ribbon.tabs");
    sh.app.chrome.onboard.step = onboard::STEP_SUMMARY;
    sh.settle();
    sh.pass(key(egui::Key::Enter, egui::Modifiers::NONE));
    sh.settle();
    assert!(!sh.app.chrome.onboard.open, "Start closes");
    let back = Prefs::load_from(Some(path.clone()));
    assert!(back.onboarded);
    assert_eq!(back.focus, "ai");
    assert_eq!(back.look, "night");
    assert_eq!(back.icon_set, "filled");
    // the finished focus carries the ribbon chosen after it
    let ai = back.mode_layouts.modes.get(&WorkMode::Ai).expect("ai layout");
    assert_eq!(ai.ribbon, crate::shell::fold::RibbonMode::Tabs);
    assert!(ai.ai_panel_open);
    crate::ai::ui::modes::leave();
    let _ = std::fs::remove_file(&path);
}

/// Every look, accent, face, size, icon set, density, ribbon and mode layout chosen in one
/// session is there after a restart (the settings file read back).
#[test]
fn settings_persist_across_restart() {
    let path = temp_prefs("theme1");
    let mut sh = Shell::new(Lang::En, SIZES[0]);
    let a = &mut sh.app;
    a.prefs.path = Some(path.clone());
    a.use_look("engineer");
    for id in [
        "ui.accent.violet",
        "ui.font.system",
        "ui.text.larger",
        "ui.icons.duotone_all",
        "ui.density.comfortable",
        "ui.ribbon.tabs",
        "ui.theme.dark",
    ] {
        assert!(a.run(id), "{id}");
    }
    let lay = Layout {
        browser_open: false,
        table_height: 310.0,
        ..Layout::default()
    };
    a.prefs.mode_layouts.modes.insert(WorkMode::Data, lay.clone());
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../data/layout_looks_tests.json")).unwrap();
    let name = data["mode_layout_name_ru"].as_str().unwrap();
    assert_eq!(a.mode_layout_save_as(name).as_deref(), Some(name));
    let before = a.prefs.clone();
    let back = Prefs::load_from(Some(path.clone()));
    assert_eq!(back.look, "engineer");
    assert_eq!(back.accent, looks::hex(looks::accents()[2]));
    assert_eq!(back.font, "system");
    assert_eq!(back.text_size, crate::shell::looks_ui::TEXT_SIZES[2]);
    assert_eq!(back.icon_set, "duotone_all");
    assert_eq!(back.density, "comfortable");
    assert_eq!(back.theme, "dark");
    assert_eq!(crate::shell::looks_ui::ribbon_now(&back), looks::RibbonLook::Tabs);
    assert_eq!(back.mode_layouts.modes.get(&WorkMode::Data), Some(&lay));
    assert_eq!(back.mode_layouts.saved.len(), 1);
    assert_eq!(Prefs { path: None, ..back }, Prefs { path: None, ..before });
    let _ = std::fs::remove_file(&path);
}

/// Leaving a mode keeps its panels; coming back restores them; each mode has its own.
#[test]
fn layout_per_mode_saves_and_restores() {
    let mut sh = Shell::new(Lang::En, SIZES[1]);
    let model_tab = sh.app.sh.ribbon.tab.clone();
    assert_ne!(model_tab, "data");
    sh.settle();
    assert_eq!(sh.app.chrome.work_mode, Some(WorkMode::Model));
    sh.app.prefs.browser_open = false;
    sh.app.prefs.props_width = 420.0;
    sh.settle();
    sh.app.sh.ribbon.tab = "data".into();
    sh.settle();
    let a = &sh.app;
    assert_eq!(a.chrome.work_mode, Some(WorkMode::Data));
    let model = a.prefs.mode_layouts.modes.get(&WorkMode::Model).cloned().expect("model kept");
    assert!(!model.browser_open);
    assert_eq!(model.props_width, 420.0);
    sh.app.prefs.browser_open = true;
    sh.app.prefs.props_open = false;
    sh.app.prefs.table_height = 260.0;
    sh.settle();
    sh.app.sh.ribbon.tab = model_tab.clone();
    sh.settle();
    let a = &sh.app;
    assert_eq!(a.chrome.work_mode, Some(WorkMode::Model));
    assert!(!a.prefs.browser_open && a.prefs.props_open);
    assert_eq!(a.prefs.props_width, 420.0);
    let data = a.prefs.mode_layouts.modes.get(&WorkMode::Data).cloned().expect("data kept");
    assert!(data.browser_open && !data.props_open);
    assert_eq!(data.table_height, 260.0);
    sh.app.sh.ribbon.tab = "data".into();
    sh.settle();
    assert!(sh.app.prefs.browser_open && !sh.app.prefs.props_open);
    assert_eq!(sh.app.prefs.table_height, 260.0);
    // reset forgets the current mode's layout and puts the default panels back
    sh.app.mode_layout_reset();
    sh.settle();
    assert!(!sh.app.prefs.mode_layouts.modes.contains_key(&WorkMode::Data));
    assert!(sh.app.prefs.browser_open && sh.app.prefs.props_open);
    // a saved layout goes onto the current mode
    sh.app.prefs.browser_open = false;
    sh.app.mode_layout_save_as("narrow");
    sh.app.prefs.browser_open = true;
    sh.app.chrome.settings.use_layout = Some("narrow".into());
    sh.settle();
    assert!(!sh.app.prefs.browser_open);
    let m = &sh.app.prefs.mode_layouts.modes;
    assert_eq!(m.get(&WorkMode::Data).map(|l| l.browser_open), Some(false));
}

/// Every option of the appearance page and the wizard is a palette command in both languages.
#[test]
fn every_option_is_a_command_in_both_languages() {
    let sh = Shell::new(Lang::En, SIZES[0]);
    let a = &sh.app;
    let en = ddc_ui::Catalog::new(Lang::En);
    let ru = ddc_ui::Catalog::new(Lang::Ru);
    let mut ids: Vec<&str> = crate::shell::looks_ui::COMMANDS.to_vec();
    ids.extend(onboard::COMMANDS);
    ids.extend([
        "ui.layout.mode.reset",
        "ui.layout.mode.save_as",
        "ui.layout.mode.export",
        "ui.layout.mode.import",
    ]);
    for id in ids {
        assert!(a.sh.reg.get(id).is_some(), "{id}: not in the registry");
        assert!(a.handlers.contains_key(id), "{id}: no handler");
        for c in [&en, &ru] {
            for k in [format!("cmd.{id}"), format!("cmd.{id}.desc")] {
                let t = c.t(&k);
                assert!(!t.is_empty() && t != k, "{id}: {k} missing");
            }
        }
    }
    // Help (the ribbon's help group) holds the wizard
    let c = a.sh.reg.get("ui.onboarding").unwrap();
    assert_eq!(c.group.as_deref(), Some("help"));
}
