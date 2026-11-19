// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Window size and position: the first start fits 90 % of the work area, later starts restore
//! the last geometry clamped to the visible desktop. Pure rules here; the file is `window.txt`.

use std::path::PathBuf;
use std::sync::Mutex;

pub const DEFAULT: (f32, f32) = (1400.0, 900.0);
pub const MIN: (f32, f32) = (800.0, 520.0);
/// Share of the work area a fresh window may take.
pub const SHARE: f32 = 0.9;

/// Inner size and outer position, logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub pos: (f32, f32),
    pub size: (f32, f32),
}

/// First-run size on a monitor of this size: the default, but at most 90 % of the screen.
pub fn initial_size(monitor: (f32, f32)) -> (f32, f32) {
    (
        DEFAULT.0.min((monitor.0 * SHARE).floor()),
        DEFAULT.1.min((monitor.1 * SHARE).floor()),
    )
}

/// Saved geometry made visible: the size fits the work area, the window lies inside the
/// desktop (`[left, top, right, bottom]`), so its title bar can always be reached.
pub fn restore(saved: Geom, work: [f32; 4], desktop: [f32; 4]) -> Geom {
    let (ww, wh) = (work[2] - work[0], work[3] - work[1]);
    let w = saved.size.0.min(ww).max(MIN.0.min(ww));
    let h = saved.size.1.min(wh).max(MIN.1.min(wh));
    let x = saved.pos.0.min(desktop[2] - w).max(desktop[0]);
    let y = saved.pos.1.min(desktop[3] - h).max(desktop[1]);
    Geom {
        pos: (x, y),
        size: (w, h),
    }
}

/// What to open with: the saved geometry restored, else the first-run size (no position).
pub fn start(
    saved: Option<Geom>,
    screens: Option<([f32; 4], [f32; 4])>,
) -> ((f32, f32), Option<(f32, f32)>) {
    match (saved, screens) {
        (Some(g), Some((work, desk))) => {
            let g = restore(g, work, desk);
            (g.size, Some(g.pos))
        }
        (None, Some((work, _))) => (initial_size((work[2] - work[0], work[3] - work[1])), None),
        (Some(g), None) => (g.size, Some(g.pos)),
        (None, None) => (DEFAULT, None),
    }
}

pub fn parse(text: &str) -> Option<Geom> {
    let v: Vec<f32> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let ok = v.len() == 4 && v.iter().all(|n| n.is_finite()) && v[2] >= 100.0 && v[3] >= 100.0;
    ok.then(|| Geom {
        pos: (v[0], v[1]),
        size: (v[2], v[3]),
    })
}

pub fn format(g: &Geom) -> String {
    format!("{} {} {} {}
", g.pos.0, g.pos.1, g.size.0, g.size.1)
}

fn file() -> Option<PathBuf> {
    Some(crate::gfx::data_dir()?.join("window.txt"))
}

pub fn load() -> Option<Geom> {
    parse(&std::fs::read_to_string(file()?).ok()?)
}

static LAST: Mutex<Option<Geom>> = Mutex::new(None);

/// Remembers the geometry of the normal (not maximized, not minimized) window; writes it when
/// it changed. Failures to write are ignored.
pub fn track(g: Geom) {
    let Ok(mut last) = LAST.lock() else { return };
    if *last == Some(g) {
        return;
    }
    *last = Some(g);
    if let Some(f) = file() {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, format(&g));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_720: [f32; 4] = [0.0, 0.0, 1280.0, 680.0];
    const DESK_720: [f32; 4] = [0.0, 0.0, 1280.0, 720.0];

    #[test]
    fn first_run_fits_small_and_keeps_default_on_large() {
        let (w, h) = initial_size((1280.0, 720.0));
        assert!(w <= 1152.0 && h <= 648.0, "{w}x{h}");
        assert_eq!(initial_size((3840.0, 2160.0)), (1400.0, 900.0));
    }

    #[test]
    fn first_run_on_1280x720_uses_work_area_and_no_position() {
        let (size, pos) = start(None, Some((WORK_720, DESK_720)));
        assert_eq!(pos, None);
        assert!(size.0 <= 1152.0 && size.1 <= 612.0, "{size:?}");
    }

    #[test]
    fn off_screen_position_is_pulled_back() {
        let g = Geom {
            pos: (3000.0, -400.0),
            size: (1000.0, 600.0),
        };
        let r = restore(g, WORK_720, DESK_720);
        assert_eq!(r.pos, (280.0, 0.0));
        let g = Geom {
            pos: (-900.0, 900.0),
            size: (1000.0, 600.0),
        };
        let r = restore(g, WORK_720, DESK_720);
        assert_eq!(r.pos, (0.0, 120.0));
    }

    #[test]
    fn oversized_saved_window_shrinks_to_the_work_area() {
        let g = Geom {
            pos: (0.0, 0.0),
            size: (2500.0, 1400.0),
        };
        assert_eq!(restore(g, WORK_720, DESK_720).size, (1280.0, 680.0));
    }

    #[test]
    fn a_second_monitor_position_stays_when_visible() {
        let desk = [-1920.0, 0.0, 1280.0, 1080.0];
        let g = Geom {
            pos: (-1500.0, 100.0),
            size: (1000.0, 600.0),
        };
        assert_eq!(restore(g, WORK_720, desk).pos, (-1500.0, 100.0));
    }

    #[test]
    fn file_text_round_trips_and_rejects_junk() {
        let g = Geom {
            pos: (-20.0, 30.0),
            size: (1100.0, 700.0),
        };
        assert_eq!(parse(&format(&g)), Some(g));
        assert_eq!(parse("1 2 3"), None);
        assert_eq!(parse("a b c d"), None);
        assert_eq!(parse("0 0 10 10"), None);
    }
}
