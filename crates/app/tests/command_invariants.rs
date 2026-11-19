// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every-command invariants: each registry command (`Registry::builtin()`) and each handler the
//! app registers (`register_command`) runs headless on a realistic project with no selection,
//! one wall, a mixed selection, and on an empty project. Per run, on a fresh app and thread:
//! - no panic, in the command or in the two window frames drawn after it;
//! - the model passes `check_integrity` (references resolve, no orphans) after it and after undo;
//! - a model change is exactly one undo step, and undo restores the content byte for byte
//!   (`content_hash`: every content row with its rowid); a project swap (new, open, sample) is
//!   not an edit and is checked for integrity only;
//! - a command that opens a window or modal changes nothing.
//! A disabled ribbon command explains why in en and ru (catalog keys of both languages).
//! Commands that cannot run headless are in `EXEMPT` with a reason; the list only shrinks.
//! The process gets its own user-data folder, no update check and no native dialogs.
//! `DDC_CMDINV_ONLY=mod.,edit.copy` limits the sweep to id prefixes (local debugging).

mod e2e_common;

use std::collections::BTreeSet;
use std::thread;

use ddc_app::editor::{Editor, Sel};
use ddc_app::modify_tools::{gate_for, Op};
use ddc_app::shell::EditorApp;
use ddc_model::{
    BeamJoin, BeamSpec, ColumnSpec, DimKind, DimRef, DimSpec, GridSpec, LayerSpec, Model,
    OpeningSpec, RoofKind, RoofSpec, RoomSpec, SchedulePlace, SheetDef, SlabSpec, TagCat, ViewRef,
    ViewportDef, WallSpec, DEFAULT_CUT,
};
use ddc_ui::commands::Registry;
use ddc_ui::i18n::Lang;
use e2e_common as ec;
use egui::{self, accesskit};

/// Commands never run headless: (id, one-line reason). Remove a line when the command can run
/// and lower `EXEMPT_MAX`: the list may only shrink.
const EXEMPT: &[(&str, &str)] = &[
    ("help.check_update", "asks the update server over the network"),
    (
        "set.lang",
        "swaps the catalog; the next frame sets the language process-wide (quick tests cover it)",
    ),
];

/// Ceiling of the exempt list (baseline): lower it with every command made runnable.
const EXEMPT_MAX: usize = 2;

/// Hide, Isolate, Selection filter: with nothing selected they must say why.
const UX68D: [&str; 3] = ["mod.hide", "mod.isolate", "mod.select_filter"];

/// Transform tools whose ribbon buttons the selection can disable (`sync_caps_block`).
const CAPS_OPS: [Op; 6] = [Op::Move, Op::Copy, Op::Rotate, Op::Mirror, Op::MirrorDraw, Op::Array];

/// Runs in flight per sweep; each run owns a thread, an app and an egui context.
const PAR: usize = 2;
const STACK: usize = 16 << 20;
const SCREEN: (f32, f32) = (1600.0, 1000.0);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scenario {
    Empty,
    Project,
    Wall,
    Mixed,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Scenario::Empty => "empty",
            Scenario::Project => "project",
            Scenario::Wall => "project+wall",
            Scenario::Mixed => "project+mixed",
        }
    }
}

// ---------------------------------------------------------------- process

/// Own user-data folder, no native dialogs, no update check: nothing of the real profile.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ddc_cmdinv_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("APPDATA", &dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        ddc_app::shot_press::set_screenshot_mode(true);
    });
    ddc_app::update::set_freq(ddc_app::update::Freq::Never);
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-text panic".into())
}

/// `f` on its own thread: thread-local tool and view state of one run never reaches the next.
fn on_thread<T: Send + 'static>(
    name: String,
    f: impl FnOnce() -> T + Send + 'static,
) -> thread::JoinHandle<T> {
    thread::Builder::new().name(name).stack_size(STACK).spawn(f).unwrap()
}

// ---------------------------------------------------------------- fixtures

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

fn opening(along: f64, width: f64, sill: f64, height: f64, kind: &str) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    }
}

/// Compact local project: two levels, two wall types, a 10x6 ring with a partition, doors and
/// windows, slab, roof, two rooms, grids, a column and a beam, a dimension, tags, a sheet with
/// a plan and a schedule.
fn build_compact(m: &mut Model) {
    let lv = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let outer = m
        .add_wall_type(
            "Outer",
            &[
                layer("plaster", 0.02),
                layer("masonry", 0.25),
                layer("insulation", 0.1),
            ],
        )
        .unwrap();
    let inner = m.add_wall_type("Inner", &[layer("gypsum", 0.1)]).unwrap();
    for (name, a, b) in [
        ("A", (0.0, -1.0), (0.0, 7.0)),
        ("1", (-1.0, 0.0), (11.0, 0.0)),
    ] {
        m.add_grid(&GridSpec::line(name, a, b)).unwrap();
    }
    let c = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let wall = |a, b, wall_type| WallSpec {
        a,
        b,
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    };
    let ring: Vec<WallSpec> = (0..4).map(|k| wall(c[k], c[(k + 1) % 4], outer)).collect();
    let w = m.add_walls(&ring).unwrap();
    let w4 = m.add_wall(&wall((5.0, 0.0), (5.0, 6.0), inner)).unwrap();
    m.add_opening(w[0], &opening(1.0, 0.9, 0.0, 2.1, "door")).unwrap();
    m.add_opening(w4, &opening(2.5, 0.9, 0.0, 2.1, "door")).unwrap();
    m.add_opening(w[2], &opening(3.0, 1.5, 0.9, 1.5, "window")).unwrap();
    m.add_opening(w[1], &opening(2.0, 1.2, 0.9, 1.5, "window")).unwrap();
    let slab_t = m.add_slab_type("Slab 200", &[layer("concrete", 0.2)]).unwrap();
    // a layer text names a material; opening a file adds the row, so the fixture has it already
    m.add_material(&ddc_model::material_defaults("tile")).unwrap();
    let roof_t = m.add_slab_type("Roof 200", &[layer("tile", 0.2)]).unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: slab_t,
        offset: 0.0,
        outer: c.to_vec(),
        holes: vec![],
    })
    .unwrap();
    m.add_roof(&RoofSpec {
        level: lv,
        slab_type: roof_t,
        offset: 3.0,
        overhang: 0.4,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: c.to_vec(),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    })
    .unwrap();
    for (name, number, point) in [("Office", "101", (2.5, 3.0)), ("Store", "102", (7.5, 3.0))] {
        m.add_room(&RoomSpec {
            level: lv,
            name: name.into(),
            number: number.into(),
            point,
            height: 3.0,
        })
        .unwrap();
    }
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let bt = m.add_catalog_member_type("IPE 300").unwrap();
    m.add_column(&ColumnSpec {
        level: lv,
        top_level: Some(l2),
        member_type: ct,
        structural: true,
        at: (2.5, 1.5),
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    })
    .unwrap();
    m.add_beam(&BeamSpec {
        level: l2,
        member_type: bt,
        a: (0.0, 3.0, 0.0),
        b: (10.0, 3.0, 0.0),
        bulge: 0.0,
        yjust: Default::default(),
        zjust: Default::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    })
    .unwrap();
    m.add_dim(&DimSpec {
        level: lv,
        kind: DimKind::Linear,
        refs: vec![
            DimRef::AxisEnd { wall: w[0], end: 0 },
            DimRef::AxisEnd { wall: w[0], end: 1 },
        ],
        dir: (0.0, 0.0),
        side: -1.0,
        offset: 1.0,
        tier: -1,
        auto: false,
    })
    .unwrap();
    m.tag_all(lv, &[TagCat::Door, TagCat::Window, TagCat::Room], false).unwrap();
    let sheet = m.add_sheet(&SheetDef::new("A-101", "Plans")).unwrap();
    m.add_viewport(&ViewportDef {
        sheet,
        view: ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        },
        scale: 100.0,
        center: (150.0, 150.0),
        crop: None,
        title: "Plan".into(),
    })
    .unwrap();
    m.add_schedule_place(&SchedulePlace {
        sheet,
        schedule: "walls".into(),
        pos: (300.0, 280.0),
    })
    .unwrap();
}

/// HOOK: showcase projects are built elsewhere. When their fixtures land, return them from
/// here (one sweep per showcase) and keep `build_compact` as the small case; nothing else in
/// this file needs to change.
fn project_model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    build_compact(&mut m);
    m
}

fn app_for(sc: Scenario) -> EditorApp {
    let mut ed = Editor::new_in_memory().unwrap();
    if sc != Scenario::Empty {
        ed.replace_model(project_model()).unwrap();
    }
    let mut a = EditorApp::new(ed, None, "test".into());
    let m = &a.ed.model;
    let first = |t: &str| ec::ids(m, t).first().copied();
    a.ed.selection = match sc {
        Scenario::Empty | Scenario::Project => vec![],
        Scenario::Wall => first("wall").map(Sel::Wall).into_iter().collect(),
        Scenario::Mixed => [
            first("wall").map(Sel::Wall),
            first("opening").map(Sel::Opening),
            first("slab").map(Sel::Slab),
            first("room").map(Sel::Room),
        ]
        .into_iter()
        .flatten()
        .collect(),
    };
    a
}

// ---------------------------------------------------------------- frames

fn new_ctx() -> egui::Context {
    let ctx = ddc_ui::theme::headless_ctx();
    ctx.enable_accesskit();
    ctx
}

/// One window frame; returns the titles of the windows it drew (`<modal>` for a modal).
fn frame(a: &mut EditorApp, ctx: &egui::Context, t: &mut f64) -> BTreeSet<String> {
    *t += 0.05;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(SCREEN.0, SCREEN.1),
        )),
        time: Some(*t),
        ..Default::default()
    };
    let out = ctx.run(input, |c| a.ui(c, None));
    let mut wins = BTreeSet::new();
    if let Some(u) = out.platform_output.accesskit_update {
        let root = u.tree.as_ref().map(|tr| tr.root);
        for (id, node) in u.nodes {
            if node.role() == accesskit::Role::Window && Some(id) != root {
                wins.insert(node.label().unwrap_or("").to_string());
            }
        }
    }
    if ctx.memory(|m| m.top_modal_layer()).is_some() {
        wins.insert("<modal>".into());
    }
    wins
}

// ---------------------------------------------------------------- one run

fn project_uid(m: &Model) -> Option<String> {
    m.sql()
        .query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| {
            r.get::<_, rusqlite::types::Value>(0)
        })
        .ok()
        .map(|v| format!("{v:?}"))
}

fn integrity(m: &Model, base: &[ddc_model::Issue], tag: &str, bad: &mut Vec<String>) {
    match m.integrity_regressions(base) {
        Ok(iss) if iss.is_empty() => {}
        Ok(iss) => bad.push(format!("{tag}: integrity {iss:?}")),
        Err(e) => bad.push(format!("{tag}: integrity check failed: {e}")),
    }
}

/// Runs `id` once in scenario `sc` on a fresh app; returns the broken invariants.
fn exercise(id: &str, sc: Scenario) -> Vec<String> {
    let tag = format!("{}[{id}]", sc.name());
    let mut bad = vec![];
    let mut a = app_for(sc);
    let ctx = new_ctx();
    let mut t = 0.0;
    frame(&mut a, &ctx, &mut t);
    let wins0 = frame(&mut a, &ctx, &mut t);
    let base = a.ed.model.check_integrity().unwrap_or_default();
    let uid0 = project_uid(&a.ed.model);
    let h0 = a.ed.model.content_hash().unwrap();
    let dump0 = ec::dump_data(&a.ed.model);
    let steps0 = a.ed.undo_len();
    let said0 = (a.sh.toasts.history.len(), a.ed.status.clone());

    a.run(id);
    frame(&mut a, &ctx, &mut t);
    let wins1 = frame(&mut a, &ctx, &mut t);

    let m = &a.ed.model;
    let swapped = project_uid(m) != uid0;
    let base = if swapped { vec![] } else { base };
    integrity(m, &base, &tag, &mut bad);
    let changed = !swapped && m.content_hash().unwrap() != h0;
    let steps = a.ed.undo_len();
    let opened: Vec<&String> = wins1.difference(&wins0).collect();
    if UX68D.contains(&id) && a.ed.selection.is_empty() && sc != Scenario::Empty {
        let said = (a.sh.toasts.history.len(), a.ed.status.clone());
        if said.0 == said0.0 && said.1 == said0.1 {
            bad.push(format!("{tag}: nothing selected, refused without a word (ux68d)"));
        }
    }
    if !changed {
        return bad;
    }
    if !opened.is_empty() {
        bad.push(format!("{tag}: opened {opened:?} and changed the model"));
    }
    match steps.checked_sub(steps0) {
        Some(1) => {}
        Some(0) => bad.push(format!("{tag}: changed the model without an undo step")),
        Some(n) => bad.push(format!("{tag}: one command, {n} undo steps")),
        None => bad.push(format!("{tag}: changed the model and dropped undo steps")),
    }
    if steps <= steps0 {
        return bad;
    }
    a.run("edit.undo");
    frame(&mut a, &ctx, &mut t);
    let m = &a.ed.model;
    if m.content_hash().unwrap() != h0 {
        let diff = ec::diff(&ec::dump_data(m), &dump0);
        bad.push(format!("{tag}: undo does not restore the content{diff}"));
    }
    if a.ed.undo_len() != steps0 {
        bad.push(format!("{tag}: undo left {} steps, want {steps0}", a.ed.undo_len()));
    }
    integrity(m, &base, &format!("{tag} after undo"), &mut bad);
    bad
}

// ---------------------------------------------------------------- sweep

fn exempt() -> BTreeSet<&'static str> {
    EXEMPT.iter().map(|(k, _)| *k).collect()
}

/// Registry commands and registered handlers, sorted.
fn all_commands() -> (BTreeSet<String>, EditorApp) {
    let a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    let mut ids: BTreeSet<String> =
        Registry::builtin().all().iter().map(|c| c.id.clone()).collect();
    ids.extend(a.handlers.keys().cloned());
    (ids, a)
}

fn swept() -> Vec<String> {
    let only: Vec<String> = std::env::var("DDC_CMDINV_ONLY")
        .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let ex = exempt();
    let (ids, _) = all_commands();
    ids.into_iter()
        .filter(|id| !ex.contains(id.as_str()))
        .filter(|id| only.is_empty() || only.iter().any(|p| id.starts_with(p.as_str())))
        .collect()
}

fn sweep(sc: Scenario) {
    isolate();
    let ids = swept();
    let mut bad = vec![];
    for chunk in ids.chunks(PAR) {
        let runs: Vec<_> = chunk
            .iter()
            .map(|id| {
                let id2 = id.clone();
                let h = on_thread(format!("{}:{id}", sc.name()), move || exercise(&id2, sc));
                (id.clone(), h)
            })
            .collect();
        for (id, h) in runs {
            match h.join() {
                Ok(v) => bad.extend(v),
                Err(p) => bad.push(format!("{}[{id}]: panicked: {}", sc.name(), panic_text(&*p))),
            }
        }
    }
    eprintln!("command invariants {}: {} commands run", sc.name(), ids.len());
    assert!(bad.is_empty(), "{} broken invariants:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn every_command_on_an_empty_project() {
    sweep(Scenario::Empty);
}

#[test]
fn every_command_on_the_project_without_selection() {
    sweep(Scenario::Project);
}

#[test]
fn every_command_on_the_project_with_a_wall_selected() {
    sweep(Scenario::Wall);
}

#[test]
fn every_command_on_the_project_with_a_mixed_selection() {
    sweep(Scenario::Mixed);
}

// ---------------------------------------------------------------- coverage and fixture

#[test]
fn the_sweep_covers_every_command_and_the_exempt_list_only_shrinks() {
    isolate();
    let (ids, a) = all_commands();
    let shell: BTreeSet<String> = a.sh.reg.all().iter().map(|c| c.id.clone()).collect();
    let builtin: BTreeSet<String> =
        Registry::builtin().all().iter().map(|c| c.id.clone()).collect();
    assert_eq!(shell, builtin, "the shell registry is Registry::builtin()");
    assert!(ids.len() > 400, "only {} commands found", ids.len());
    let stray: Vec<&String> = a.handlers.keys().filter(|k| !builtin.contains(*k)).collect();
    assert!(stray.is_empty(), "handlers outside the registry: {stray:?}");
    assert!(EXEMPT.len() <= EXEMPT_MAX, "EXEMPT has {}, ceiling {EXEMPT_MAX}", EXEMPT.len());
    assert!(EXEMPT.len() == EXEMPT_MAX, "EXEMPT shrank to {}: lower EXEMPT_MAX", EXEMPT.len());
    let mut seen = BTreeSet::new();
    for (id, why) in EXEMPT {
        assert!(seen.insert(*id), "duplicate EXEMPT entry {id}");
        assert!(ids.contains(*id), "EXEMPT names no command: {id}");
        assert!(!why.trim().is_empty() && !why.contains('\n'), "{id}: one-line reason");
    }
}

#[test]
fn the_compact_project_is_clean_and_rich() {
    let m = project_model();
    ec::assert_clean(&m, "compact project");
    for (t, n) in [
        ("level", 2),
        ("wall", 5),
        ("opening", 4),
        ("slab", 1),
        ("roof", 1),
        ("room", 2),
        ("grid", 2),
        ("col", 1),
        ("beam", 1),
        ("dim", 1),
        ("sheet", 1),
    ] {
        assert!(ec::count(&m, t) >= n, "{t}: {} rows, want at least {n}", ec::count(&m, t));
    }
}

#[test]
fn a_run_finds_opened_windows_and_project_swaps() {
    isolate();
    let h = on_thread("cmdinv.self".into(), || {
        let mut a = app_for(Scenario::Project);
        let ctx = new_ctx();
        let mut t = 0.0;
        frame(&mut a, &ctx, &mut t);
        let w0 = frame(&mut a, &ctx, &mut t);
        a.run("mgr.settings");
        frame(&mut a, &ctx, &mut t);
        let w1 = frame(&mut a, &ctx, &mut t);
        let uid = project_uid(&a.ed.model);
        let other = project_uid(&project_model());
        (w1.difference(&w0).count(), uid.is_some() && uid != other)
    });
    let (opened, distinct) = h.join().unwrap();
    assert!(opened >= 1, "the settings window was not seen");
    assert!(distinct, "two projects share a project_uid: swaps would go unseen");
}

// ---------------------------------------------------------------- disabled commands

fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
}

/// Catalog text of `key` in both languages: present, non-empty, ru in Cyrillic.
fn both_langs(a: &EditorApp, key: &str, tag: &str, bad: &mut Vec<String>) {
    for lang in [Lang::En, Lang::Ru] {
        let v = a.sh.cat.table(lang).get(key).and_then(|v| v.as_str()).unwrap_or("");
        if v.trim().is_empty() {
            bad.push(format!("{tag}: no {lang:?} text for {key}"));
        } else if lang == Lang::Ru && !has_cyrillic(v) {
            bad.push(format!("{tag}: ru text of {key} is not Russian: {v:?}"));
        }
    }
}

fn disabled_reasons(sc: Scenario) -> Vec<String> {
    let mut bad = vec![];
    let mut a = app_for(sc);
    let ctx = new_ctx();
    let mut t = 0.0;
    frame(&mut a, &ctx, &mut t);
    frame(&mut a, &ctx, &mut t);
    let reg = Registry::builtin();
    for c in reg.all() {
        let tag = format!("{}[{}] disabled", sc.name(), c.id);
        if a.sh.ribbon.enabled(&reg, &c.id) {
            continue;
        }
        let Some(text) = a.sh.ribbon.blocked_reason(&reg, &c.id).map(str::to_string) else {
            both_langs(&a, "ribbon.soon_tip", &tag, &mut bad);
            continue;
        };
        if text.trim().is_empty() {
            bad.push(format!("{tag}: empty reason"));
        }
        let id = reg.resolve(&c.id);
        let Some(op) = CAPS_OPS.into_iter().find(|o| o.command() == id) else {
            // The tip follows the app language, so check the catalog key in both
            if ddc_app::modify_tools::NEEDS_SELECTION.iter().any(|n| *n == id) {
                both_langs(&a, "modify_ops.op.select_first", &tag, &mut bad);
                continue;
            }
            // a blocker outside the transform gate: the shown (ru) text is all there is
            if !has_cyrillic(&text) {
                bad.push(format!("{tag}: reason {text:?} has no ru text"));
            }
            continue;
        };
        match gate_for(op, &a.ed.selection) {
            ddc_app::elem::caps::Gate::Refused { why, .. } => {
                both_langs(&a, why, &tag, &mut bad);
                both_langs(&a, "caps.refused", &tag, &mut bad);
            }
            g => bad.push(format!("{tag}: disabled, but the gate says {g:?}")),
        }
    }
    if sc == Scenario::Project {
        for id in UX68D {
            let tag = format!("{}[{id}] (ux68d)", sc.name());
            if a.sh.ribbon.enabled(&reg, id) {
                bad.push(format!("{tag}: enabled with nothing selected"));
            } else if a.sh.ribbon.blocked_reason(&reg, id).is_none_or(|r| r.trim().is_empty()) {
                bad.push(format!("{tag}: greyed without a reason"));
            }
        }
    }
    bad
}

#[test]
fn disabled_commands_explain_why_in_en_and_ru() {
    isolate();
    let scs = [Scenario::Empty, Scenario::Project, Scenario::Wall, Scenario::Mixed];
    let mut bad = vec![];
    for sc in scs {
        match on_thread(format!("cmdinv.off.{}", sc.name()), move || disabled_reasons(sc)).join() {
            Ok(v) => bad.extend(v),
            Err(p) => bad.push(format!("{}: panicked: {}", sc.name(), panic_text(&*p))),
        }
    }
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}
