// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc-app --ui-shot script.txt`: the whole editor UI rendered offscreen, driven by a script of
//! app commands and egui input fed straight into the context. No window, no OS input, no focus.
//!
//! One step per line, `#` starts a comment. Setup lines come before the first action:
//!   size 1600x1000 · scale 1 · lang en|ru · theme light|dark · open project.ddc
//! Actions:
//!   level NAME        plan of that level          fit             fit the plan to the model
//!   view X,Y,PX       plan centre (m) and px/m    quiet           dismiss the toasts on screen
//!   run CMD           a command id (arch.wall)    mode select|wall|rect
//!   at X,Y            click the plan at model metres, through the canvas
//!   hover X,Y         move the pointer there      tap X,Y         click at screen points
//!   type TEXT         text into the focused field key enter|escape|tab|backspace
//!   fold SPEC         as `--fold`                 settings PAGE   as `--settings-page`
//!   ai-mode chat|build|review                     select-room NUMBER
//!   ai-demo chat|build  a scripted conversation in the AI panel (no network)
//!   frames N          passes before each shot     shot out.png    write the frame
use crate::shell::EditorApp;
use crate::RenderState;
use eframe::egui;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub line: usize,
    pub verb: String,
    pub arg: String,
}

const SETUP: [&str; 5] = ["size", "scale", "lang", "theme", "open"];
const ACTIONS: [&str; 19] = [
    "level",
    "fit",
    "view",
    "quiet",
    "run",
    "mode",
    "at",
    "hover",
    "tap",
    "type",
    "key",
    "fold",
    "settings",
    "ai-mode",
    "ai-demo",
    "select-room",
    "frames",
    "shot",
    "wait",
];

/// Lines into steps; an unknown verb or a setup line after an action is an error with its line.
pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut out: Vec<Step> = vec![];
    for (i, raw) in text.lines().enumerate() {
        let l = raw.split_once(" #").map_or(raw, |(a, _)| a).trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (verb, arg) = l.split_once(char::is_whitespace).unwrap_or((l, ""));
        let s = Step {
            line: i + 1,
            verb: verb.to_string(),
            arg: arg.trim().to_string(),
        };
        if SETUP.contains(&verb) {
            if out.iter().any(|p| ACTIONS.contains(&p.verb.as_str())) {
                return Err(format!(
                    "line {}: {verb} must come before the first action",
                    s.line
                ));
            }
        } else if !ACTIONS.contains(&verb) {
            return Err(format!("line {}: unknown step {verb}", s.line));
        }
        out.push(s);
    }
    if !out.iter().any(|s| s.verb == "shot") {
        return Err("the script writes no shot".into());
    }
    Ok(out)
}

fn pair(s: &Step) -> Result<(f64, f64), String> {
    let bad = || format!("line {}: {} wants X,Y", s.line, s.verb);
    let (a, b) = s.arg.split_once(',').ok_or_else(bad)?;
    // num-lint: script coordinates, never typed into the UI
    let x = a.trim().parse::<f64>().map_err(|_| bad())?;
    // num-lint: script coordinates, never typed into the UI
    let y = b.trim().parse::<f64>().map_err(|_| bad())?;
    Ok((x, y))
}

fn btn(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn key(k: egui::Key) -> Vec<egui::Event> {
    let e = |pressed| egui::Event::Key {
        key: k,
        physical_key: Some(k),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    vec![e(true), e(false)]
}

struct Run<'a> {
    rs: &'a RenderState,
    ctx: egui::Context,
    size: (u32, u32),
    scale: f32,
    frames: usize,
    clock: std::cell::Cell<f64>,
}

impl Run<'_> {
    /// Passes with `evs[i]` fed on pass `i`, then two settling passes; returns the last frame.
    fn pass(&self, app: &mut EditorApp, evs: Vec<Vec<egui::Event>>, extra: usize) -> Vec<u8> {
        let n = evs.len() + extra.max(2);
        let rs = self.rs;
        let guard = crate::plan_view::tab_guard_on(app, &self.ctx);
        let t0 = self.clock.get();
        self.clock.set(t0 + n as f64 * 0.016 + 1.0);
        crate::shot::frame_ev_at(
            rs,
            &self.ctx,
            self.size,
            self.scale,
            n,
            t0,
            |c, i| {
                let mut e = evs.get(i).cloned().unwrap_or_default();
                crate::plan_view::tab_strip(c, guard, &mut e);
                e
            },
            |c, _| app.ui(c, Some(rs)),
        )
    }
}

/// Runs the script; returns the PNGs written. Relative paths are taken from `base`.
pub fn run(
    rs: &RenderState,
    backend: String,
    text: &str,
    base: &Path,
) -> Result<Vec<PathBuf>, String> {
    let steps = parse(text)?;
    let path = |a: &str| {
        let p = PathBuf::from(a);
        if p.is_absolute() {
            p
        } else {
            base.join(p)
        }
    };
    let mut r = Run {
        rs,
        ctx: egui::Context::default(),
        size: (1600, 1000),
        scale: 1.0,
        frames: 10,
        clock: std::cell::Cell::new(0.0),
    };
    let (mut theme, mut lang, mut open) = ("light".to_string(), None, None);
    let mut i = 0;
    while let Some(s) = steps.get(i).filter(|s| SETUP.contains(&s.verb.as_str())) {
        match s.verb.as_str() {
            "size" => {
                let (w, h) = s
                    .arg
                    .split_once('x')
                    .ok_or(format!("line {}: size WxH", s.line))?;
                r.size = (
                    w.parse().map_err(|_| format!("line {}: size", s.line))?,
                    h.parse().map_err(|_| format!("line {}: size", s.line))?,
                );
            }
            "scale" => {
                r.scale = s
                    .arg
                    .parse()
                    .map_err(|_| format!("line {}: scale", s.line))?
            }
            "lang" => lang = Some(ddc_ui::i18n::Lang::from_code(&s.arg)),
            "theme" => theme = s.arg.clone(),
            _ => open = Some(path(&s.arg)),
        }
        i += 1;
    }
    if let Some(l) = lang {
        crate::set_ui_lang(l);
    }
    let ed = match &open {
        Some(p) => {
            let mut ed = crate::editor::Editor::start_project().map_err(|e| e.to_string())?;
            ed.open_project(p)
                .map_err(|e| format!("{}: {e:?}", p.display()))?;
            ed
        }
        None => crate::editor::Editor::new_in_memory().map_err(|e| e.to_string())?,
    };
    let mut app = EditorApp::new(ed, Some(rs), backend);
    app.ed.status = None;
    app.prefs.theme = theme;
    app.chrome.onboard.quiet = true;
    if let Some(l) = lang {
        app.set_lang(l);
    }
    app.plan.fit(&app.ed);
    r.pass(&mut app, vec![], 2);
    let mut out = vec![];
    for s in &steps[i..] {
        let err = |m: &str| format!("line {}: {m}", s.line);
        match s.verb.as_str() {
            "level" => {
                let id = app
                    .ed
                    .levels()
                    .into_iter()
                    .find(|l| l.1 == s.arg)
                    .ok_or_else(|| err("no such level"))?
                    .0;
                app.ed.set_level(id);
                app.plan.fit(&app.ed);
            }
            "fit" => app.plan.fit(&app.ed),
            "view" => {
                let v: Vec<f64> = s
                    .arg
                    .split(',')
                    .filter_map(|t| t.trim().parse().ok())
                    .collect();
                let [x, y, px] = v[..] else {
                    return Err(err("view X,Y,PX"));
                };
                (app.plan.center, app.plan.scale) = (ddc_geom::v2(x, y), px);
            }
            "quiet" => {
                r.pass(&mut app, vec![], 2);
                while let Some(&t) = app.sh.toasts.visible_idx().first() {
                    app.sh.toasts.dismiss(t);
                }
            }
            "run" => {
                if !app.run(&s.arg) {
                    return Err(err("not a command"));
                }
            }
            "mode" => app.ed.set_mode(match s.arg.as_str() {
                "wall" => crate::wall_tool::Mode::Polyline,
                "rect" => crate::wall_tool::Mode::Rect,
                "select" => crate::wall_tool::Mode::Select,
                _ => return Err(err("mode select|wall|rect")),
            }),
            "at" | "hover" => {
                let (x, y) = pair(s)?;
                let p = app
                    .plan
                    .screen_of(ddc_geom::v2(x, y))
                    .ok_or_else(|| err("the plan is not on screen"))?;
                let mut evs = vec![vec![egui::Event::PointerMoved(p)]];
                if s.verb == "at" {
                    evs.push(vec![btn(p, true)]);
                    evs.push(vec![btn(p, false)]);
                }
                r.pass(&mut app, evs, 2);
            }
            "tap" => {
                let (x, y) = pair(s)?;
                let p = egui::pos2(x as f32, y as f32);
                r.pass(
                    &mut app,
                    vec![
                        vec![egui::Event::PointerMoved(p)],
                        vec![btn(p, true)],
                        vec![btn(p, false)],
                    ],
                    2,
                );
            }
            "type" => {
                r.pass(&mut app, vec![vec![egui::Event::Text(s.arg.clone())]], 2);
            }
            "key" => {
                let k = match s.arg.as_str() {
                    "enter" => egui::Key::Enter,
                    "escape" => egui::Key::Escape,
                    "tab" => egui::Key::Tab,
                    "backspace" => egui::Key::Backspace,
                    _ => return Err(err("key enter|escape|tab|backspace")),
                };
                r.pass(&mut app, vec![key(k)], 2);
            }
            "fold" => crate::shot::apply_fold(&mut app, &["--fold".to_string(), s.arg.clone()])
                .map_err(|e| err(&e))?,
            "settings" => {
                let n = crate::shell::settings_win::page_from_args(&[
                    "--settings-page".to_string(),
                    s.arg.clone(),
                ])
                .ok_or_else(|| err("no such settings page"))?;
                app.open_settings(n);
            }
            "ai-mode" => {
                let m = crate::shot::ai_mode(&["--ai-mode".to_string(), s.arg.clone()])
                    .ok_or_else(|| err("ai-mode chat|build|review"))?;
                crate::ai::ui::modes::enter(&mut app, m);
            }
            "ai-demo" => crate::ai::ui::demo::seed(&mut app, &s.arg).map_err(|e| err(&e))?,
            "select-room" => {
                let id = app
                    .ed
                    .rooms
                    .iter()
                    .find(|x| x.number == s.arg)
                    .ok_or_else(|| err("no such room on this level"))?
                    .id;
                app.ed.selection = vec![crate::editor::Sel::Room(id)];
            }
            "frames" => r.frames = s.arg.parse().map_err(|_| err("frames N"))?,
            "wait" => {
                r.pass(&mut app, vec![], s.arg.parse().map_err(|_| err("wait N"))?);
            }
            _ => {
                let px = r.pass(&mut app, vec![], r.frames);
                let p = path(&s.arg);
                crate::shot::save_png(&p, r.size, &px)?;
                out.push(p);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keeps_setup_first_and_wants_a_shot() {
        let s = parse("size 800x500\n# note\nrun arch.wall  # arm\nshot a.png\n").unwrap();
        assert_eq!(
            s.iter().map(|x| x.verb.as_str()).collect::<Vec<_>>(),
            ["size", "run", "shot"]
        );
        assert_eq!(s[1].arg, "arch.wall");
        assert!(parse("run arch.wall\nsize 800x500\nshot a.png")
            .unwrap_err()
            .contains("line 2"));
        assert!(parse("jump 1\nshot a.png")
            .unwrap_err()
            .contains("unknown step jump"));
        assert!(parse("size 800x500").is_err());
    }

    #[test]
    #[ignore = "needs a graphics adapter"]
    fn two_step_script_writes_two_pngs() {
        let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir = std::env::temp_dir().join(format!("ddc_ui_shot_{}", std::process::id()));
        let text = "size 640x400\nmode wall\nat 0,0\nat 6,0\nshot one.png\nkey escape\nrun view.3d\nshot two.png\n";
        let out = run(&rs, desc, text, &dir).unwrap();
        assert_eq!(out.len(), 2);
        for p in &out {
            let len = std::fs::metadata(p).unwrap().len();
            assert!(len > 1000, "{} is {len} bytes", p.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
