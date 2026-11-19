//! Offscreen shots of the status bar: every control hovered, pressed and clicked (its menu
//! open), en and ru, at 1x, 1.25x and 1.5x. `DDC_SHOT_DIR` names the folder, `DDC_SHOT_TAG` the
//! file prefix. `cargo test -p ddc-app shots_status_bar -- --ignored`
use crate::shell::EditorApp;
use ddc_ui::{Catalog, Lang};
use eframe::egui;

const SALTS: [&str; 11] = [
    "recent", "grid", "snap", "ortho", "level", "style", "perf", "pick", "warn", "msgs", "save",
];
/// Physical size and display scale.
const SCREENS: [(u32, u32, f32); 4] = [
    (1280, 720, 1.0),
    (1920, 1080, 1.0),
    (1600, 900, 1.25),
    (1920, 1080, 1.5),
];
/// Points of the bottom of the window kept in a shot: the bar and an open menu above it.
const CROP_PT: f32 = 460.0;

fn btn(salt: &str) -> egui::Id {
    egui::Id::new(("ddc.sb", salt))
}

fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// What the pointer does: hover only, hold the button down, or click it.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    Hover,
    Down,
    Click,
}

fn bottom(rgba: &[u8], (w, h): (u32, u32), scale: f32) -> (Vec<u8>, (u32, u32)) {
    let ch = ((CROP_PT * scale) as u32).min(h);
    let start = ((h - ch) * w * 4) as usize;
    (rgba[start..].to_vec(), (w, ch))
}

#[test]
#[ignore = "needs a graphics adapter; writes screenshots to DDC_SHOT_DIR"]
fn shots_status_bar() {
    let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
        .expect("graphics adapter");
    let rs = crate::shot::render_state(pick).unwrap();
    let dir = std::env::var("DDC_SHOT_DIR").unwrap_or_else(|_| {
        std::env::temp_dir().join("ddc_sb_shots").display().to_string()
    });
    let dir = std::path::PathBuf::from(dir);
    let tag = std::env::var("DDC_SHOT_TAG").unwrap_or_else(|_| "now".into());
    for lang in [Lang::En, Lang::Ru] {
        for (w, h, scale) in SCREENS {
            for salt in SALTS {
                let steps: &[Step] = match salt {
                    "snap" | "level" => &[Step::Hover, Step::Down, Step::Click],
                    _ => &[Step::Click],
                };
                for &step in steps {
                    let mut ed = crate::query::demo_scene();
                    ed.status = None;
                    ed.set_mode(crate::wall_tool::Mode::Select);
                    let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
                    app.sh.cat = Catalog::new(lang);
                    app.plan.fit(&app.ed);
                    let ctx = egui::Context::default();
                    let mut at = None;
                    let events = |ctx: &egui::Context, i: usize| {
                        if i == 4 {
                            at = ctx.read_response(btn(salt)).map(|r| r.rect.center());
                        }
                        let Some(p) = at else { return vec![] };
                        match (i, step) {
                            (4, _) => vec![egui::Event::PointerMoved(p)],
                            (5, Step::Down | Step::Click) => vec![press(p, true)],
                            (6, Step::Click) => vec![press(p, false)],
                            _ => vec![],
                        }
                    };
                    let frames = if step == Step::Down { 7 } else { 10 };
                    let ui = |c: &egui::Context, _| app.ui(c, Some(&rs));
                    let size = (w, h);
                    let px = crate::shot::frame_ev(&rs, &ctx, size, scale, frames, events, ui);
                    if ctx.read_response(btn(salt)).is_none() {
                        continue;
                    }
                    let (crop, size) = bottom(&px, (w, h), scale);
                    let st = match step {
                        Step::Hover => "hover",
                        Step::Down => "down",
                        Step::Click => "open",
                    };
                    let name = format!("{tag}_{lang:?}_{w}x{h}@{scale}_{salt}_{st}.png");
                    crate::shot::save_png(&dir.join(name.to_lowercase()), size, &crop).unwrap();
                }
            }
        }
    }
}
