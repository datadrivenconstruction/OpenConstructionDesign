// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA net for the input surface of the app: every command has a label and a handler, every
//! shortcut and two-letter sequence reaches its command, real key events run commands, every
//! assistant tool is described and reachable, and a refused command or tool says why.
//! Headless, explicit catalogs (the global language is never touched), no network.

use std::collections::BTreeSet;

use ddc_ai::catalog;
use ddc_app::editor::Editor;
use ddc_app::shell::{EditorApp, WIRED};
use ddc_ui::commands::{Registry, Shortcut};
use ddc_ui::i18n::{Catalog, Lang};
use ddc_ui::shortcuts2::SeqEvent;
use eframe::egui;

fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ddc_qaapp_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("APPDATA", &dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        ddc_app::shot_press::set_screenshot_mode(true);
    });
    ddc_app::update::set_freq(ddc_app::update::Freq::Never);
}

fn app() -> EditorApp {
    isolate();
    EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
}

fn handled(a: &EditorApp, id: &str) -> bool {
    let r = a.sh.reg.resolve(id);
    a.handlers.contains_key(r) || WIRED.contains(&r)
}

#[test]
fn registry_has_no_conflicts() {
    let c = Registry::builtin().conflicts();
    assert!(c.is_empty(), "{c:?}");
}

#[test]
fn every_command_has_a_label_in_en_and_ru() {
    let cat = Catalog::new(Lang::En);
    let mut bad = vec![];
    for c in Registry::builtin().all() {
        let key = c.label_key();
        for lang in [Lang::En, Lang::De, Lang::Ja, Lang::Ar, Lang::Ru] {
            let t = cat.t_in(lang, &key);
            if t == key || t.trim().is_empty() {
                bad.push(format!("{} has no {lang:?} label", c.id));
            }
        }
    }
    assert!(bad.is_empty(), "{} commands:\n{}", bad.len(), bad.join("\n"));
}

/// Registered commands whose tool is not built yet (stage M): the shell keeps them off the
/// ribbon and the palette. The list only shrinks: wire a command, then delete it here.
const UNWIRED: &[&str] = &[];

#[test]
fn every_command_is_handled_or_listed_as_unwired_and_disabled() {
    let a = app();
    let mut bad = vec![];
    let mut unwired = BTreeSet::new();
    for c in a.sh.reg.all() {
        let id = c.id.as_str();
        if handled(&a, id) {
            if UNWIRED.contains(&id) {
                bad.push(format!("{id}: wired now, delete it from UNWIRED"));
            }
            continue;
        }
        unwired.insert(id.to_string());
        if !UNWIRED.contains(&id) {
            bad.push(format!("{id}: no handler and not listed in UNWIRED"));
        }
        if a.sh.ribbon.enabled(&a.sh.reg, id) {
            bad.push(format!("{id}: no handler but the ribbon enables it (dead button)"));
        }
    }
    for id in UNWIRED {
        if !unwired.contains(*id) {
            bad.push(format!("{id}: in UNWIRED but not an unhandled command"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn every_shortcut_reaches_its_command() {
    let mut a = app();
    let mut bad = vec![];
    let mut n = 0;
    for built in [false, true] {
        if built {
            ddc_app::quick::rebuild_keys(&mut a);
        }
        for c in a.sh.reg.all().to_vec() {
            if c.hold || !c.context_keys.is_empty() {
                continue;
            }
            for s in &c.shortcuts {
                let Some(sc) = Shortcut::parse(s) else {
                    bad.push(format!("{}: unparsable shortcut {s}", c.id));
                    continue;
                };
                let m = egui::Modifiers {
                    alt: sc.alt,
                    ctrl: sc.ctrl,
                    shift: sc.shift,
                    mac_cmd: false,
                    command: sc.ctrl,
                };
                n += 1;
                match ddc_app::quick::chord(&a, sc.key, m) {
                    Some(id) if a.sh.reg.resolve(&id) == a.sh.reg.resolve(&c.id) => {}
                    other => bad.push(format!("{} {s} (built={built}): got {other:?}", c.id)),
                }
            }
        }
    }
    assert!(n > 100, "only {n} shortcuts checked");
    assert!(bad.is_empty(), "{} shortcuts:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn every_two_letter_sequence_fires_its_command() {
    let a = app();
    let mut bad = vec![];
    let mut n = 0;
    for c in a.sh.reg.all() {
        let Some(mn) = &c.mnemonic else { continue };
        let keys: Vec<egui::Key> =
            mn.chars().filter_map(|ch| egui::Key::from_name(&ch.to_string())).collect();
        if keys.len() != 2 {
            bad.push(format!("{}: mnemonic {mn} has no keys", c.id));
            continue;
        }
        let mut seq = ddc_ui::shortcuts2::Seq::default();
        let first = seq.key(&a.keys2, keys[0], 1.0);
        let second = seq.key(&a.keys2, keys[1], 1.1);
        n += 1;
        match (first, second) {
            (Some(SeqEvent::Pending(_)), Some(SeqEvent::Fired(id)))
                if a.sh.reg.resolve(&id) == a.sh.reg.resolve(&c.id) => {}
            (f, s) => bad.push(format!("{} {mn}: {f:?} then {s:?}", c.id)),
        }
    }
    assert!(n > 100, "only {n} sequences checked");
    assert!(bad.is_empty(), "{} sequences:\n{}", bad.len(), bad.join("\n"));
}

fn frame(a: &mut EditorApp, ctx: &egui::Context, t: &mut f64, events: Vec<egui::Event>) {
    *t += 0.05;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
        time: Some(*t),
        events,
        ..Default::default()
    };
    let _ = ctx.run(input, |c| a.ui(c, None));
}

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }
}

fn ctrl() -> egui::Modifiers {
    egui::Modifiers { ctrl: true, command: true, ..Default::default() }
}

#[test]
fn ctrl_z_and_ctrl_y_in_a_frame_undo_and_redo_one_step() {
    let mut a = app();
    let ctx = ddc_ui::theme::headless_ctx();
    let mut t = 0.0;
    frame(&mut a, &ctx, &mut t, vec![]);
    let h0 = a.ed.model.content_hash().unwrap();
    let steps0 = a.ed.undo_len();
    a.ed.commit_walls(vec![(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(4.0, 0.0))]);
    assert_eq!(a.ed.undo_len(), steps0 + 1, "fixture edit is one step");
    let h1 = a.ed.model.content_hash().unwrap();
    assert_ne!(h0, h1);
    frame(&mut a, &ctx, &mut t, vec![key_event(egui::Key::Z, ctrl())]);
    assert_eq!(a.ed.model.content_hash().unwrap(), h0, "Ctrl+Z restores the model");
    assert_eq!(a.ed.undo_len(), steps0);
    frame(&mut a, &ctx, &mut t, vec![key_event(egui::Key::Y, ctrl())]);
    assert_eq!(a.ed.model.content_hash().unwrap(), h1, "Ctrl+Y redoes it");
}

#[test]
fn an_unknown_command_id_is_refused_without_a_panic() {
    let mut a = app();
    let h0 = a.ed.model.content_hash().unwrap();
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.run("no.such.command")));
    assert!(ran.is_ok(), "unknown id panicked");
    assert_eq!(a.ed.model.content_hash().unwrap(), h0);
}

#[test]
fn every_assistant_tool_is_described_unique_and_reachable() {
    let mut seen = BTreeSet::new();
    let mut bad = vec![];
    for s in catalog::catalog() {
        if !seen.insert(s.name) {
            bad.push(format!("{}: duplicate name", s.name));
        }
        if s.summary.trim().len() < 20 {
            bad.push(format!("{}: summary too short", s.name));
        }
        if !s.name.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()) {
            bad.push(format!("{}: name is not snake_case", s.name));
        }
        if catalog::find(s.name).is_none() {
            bad.push(format!("{}: find() misses it", s.name));
        }
        let top = catalog::Tier::T3Auto;
        if !catalog::allowed_at(s, top, true) {
            bad.push(format!("{}: not allowed at any tier", s.name));
        }
        if s.read_only() && s.destructive() {
            bad.push(format!("{}: read-only and destructive", s.name));
        }
    }
    assert!(seen.len() >= 40, "only {} tools", seen.len());
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
