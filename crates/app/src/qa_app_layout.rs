// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every command, run on the shell drawn headless at 1280x720 and 1920x1080 in both languages:
//! whatever it opens, no control leaves the screen sideways (below it only inside a scroll
//! area) and no label is elided that the idle shell did not already elide. Explicit catalogs and fonts; the global language is never touched.
//! `DDC_QAL_ONLY=mod.,edit.` limits the sweep to id prefixes (local debugging).

use crate::layout_contract::{app_in, driven_ctx, pass_on, viewport_clip, Probe};
use crate::shell::EditorApp;
use ddc_ui::{Lang, Registry};
use eframe::egui;
use std::collections::BTreeSet;

const EPS: f32 = 0.5;
/// One command's frames take seconds; past this it waits on something that never comes.
const CMD_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);
/// Commands that cannot run headless (network, process-wide language swap).
const SKIP: &[&str] = &["help.check_update", "set.lang"];

fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ddc_qal_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("APPDATA", &dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        crate::shot_press::set_screenshot_mode(true);
    });
    crate::update::set_freq(crate::update::Freq::Never);
}

/// Wheel events over `at`, as a mouse sends them.
fn wheel(at: egui::Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -240.0),
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn step(
    app: &mut EditorApp,
    ctx: &egui::Context,
    th: &mut ddc_ui::Theme,
    screen: (f32, f32),
    events: Vec<egui::Event>,
) -> Probe {
    pass_on(ctx, th, egui::Id::new("qa_app_layout"), screen, events, &mut |c| {
        app.ui(c, None);
    })
}

/// Problems of one drawn shell: elided texts, controls outside the screen, and controls in a
/// scroll area that scrolling does not bring into view.
fn problems(lang: Lang, screen: (f32, f32), id: Option<&str>) -> BTreeSet<String> {
    let mut app: EditorApp = app_in(lang);
    let (ctx, mut th) = driven_ctx();
    let mut p = step(&mut app, &ctx, &mut th, screen, vec![]);
    if let Some(id) = id {
        app.run(id);
    }
    for _ in 0..3 {
        p = step(&mut app, &ctx, &mut th, screen, vec![]);
    }
    let mut out = BTreeSet::new();
    for t in &p.elided {
        let boxed = p.spans.iter().find(|(s, _)| s == t).map(|(_, r)| r.width());
        match boxed {
            Some(w) => out.insert(format!("elided {t:?} (box {w:.0} px)")),
            None => out.insert(format!("elided {t:?}")),
        };
    }
    // the bottom-most control of each scroll viewport: if scrolling reaches it, all above do
    let mut deepest: Vec<(egui::Rect, u64, String, egui::Rect)> = vec![];
    for c in &p.controls {
        let r = c.rect;
        if r.left() < -EPS || r.right() > screen.0 + EPS {
            out.insert(format!("{:?} {:?} leaves the screen sideways", c.label, r));
        }
        if r.bottom() <= screen.1 + EPS {
            continue;
        }
        // a row its scroll culls paints nothing: it belongs to the scroll viewport above it
        let culled = || {
            p.scrolls
                .iter()
                .filter(|v| v.x_range().contains(r.center().x) && v.bottom() <= r.top() + EPS)
                .max_by(|a, b| a.bottom().total_cmp(&b.bottom()))
                .copied()
        };
        let clip = viewport_clip(&p, r).or_else(|| c.clipped_out.then(culled).flatten());
        match clip {
            None => {
                out.insert(format!(
                    "{:?} {:?} is below the screen outside a scroll area",
                    c.label, r
                ));
            }
            Some(clip) => match deepest.iter_mut().find(|d| d.0 == clip) {
                Some(d) if d.3.bottom() >= r.bottom() => {}
                Some(d) => *d = (clip, c.id, c.label.clone(), r),
                None => deepest.push((clip, c.id, c.label.clone(), r)),
            },
        }
    }
    for (clip, cid, label, r0) in deepest {
        let on_screen = |p: &Probe| {
            p.controls.iter().find(|c| c.id == cid).is_some_and(|c| {
                let v = c.rect.intersect(clip);
                v.is_positive() && c.rect.bottom() <= screen.1 + EPS && v.height() >= c.rect.height() - 1.0
            })
        };
        let mut seen = false;
        for _ in 0..200 {
            p = step(&mut app, &ctx, &mut th, screen, wheel(clip.center()));
            if on_screen(&p) {
                seen = true;
                break;
            }
        }
        if !seen {
            out.insert(format!(
                "{label:?} {r0:?} cannot be scrolled into view inside {clip:?}"
            ));
        }
    }
    out
}

fn sweep(lang: Lang, screen: (f32, f32)) {
    isolate();
    let only: Vec<String> = std::env::var("DDC_QAL_ONLY")
        .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let idle = problems(lang, screen, None);
    let ids: Vec<String> = Registry::builtin()
        .all()
        .iter()
        .map(|c| c.id.clone())
        .filter(|id| !SKIP.contains(&id.as_str()))
        .filter(|id| only.is_empty() || only.iter().any(|p| id.starts_with(p.as_str())))
        .collect();
    let mut bad: Vec<String> = idle.iter().map(|s| format!("[idle] {s}")).collect();
    for id in &ids {
        let (id2, l) = (id.clone(), lang);
        let (tx, rx) = std::sync::mpsc::channel();
        crate::jobs::spawn_guarded_sized(
            &format!("qal:{id}"),
            Some(16 << 20),
            move || {
                let _ = tx.send(problems(l, screen, Some(&id2)));
            },
            |_| {},
        )
        .unwrap();
        // a command that never returns is named and the sweep goes on (its thread is left)
        match rx.recv_timeout(CMD_LIMIT) {
            Ok(set) => {
                for s in set.difference(&idle) {
                    bad.push(format!("[{id}] {s}"));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("qa layout {lang:?} {screen:?}: [{id}] hung");
                bad.push(format!("[{id}] hung over {} s", CMD_LIMIT.as_secs()));
            }
            Err(_) => bad.push(format!("[{id}] panicked")),
        }
    }
    eprintln!("qa layout {lang:?} {screen:?}: {} commands, idle {} notes", ids.len(), idle.len());
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn qa_layout_en_1280() {
    sweep(Lang::En, (1280.0, 720.0));
}

#[test]
fn qa_layout_ru_1280() {
    sweep(Lang::Ru, (1280.0, 720.0));
}

#[test]
fn qa_layout_en_1920() {
    sweep(Lang::En, (1920.0, 1080.0));
}

#[test]
fn qa_layout_ru_1920() {
    sweep(Lang::Ru, (1920.0, 1080.0));
}
