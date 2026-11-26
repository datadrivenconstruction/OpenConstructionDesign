// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Watching link files: a modification time newer than the load gives a calm notice once and
//! a line in the links window; only the user reloads. The disk is read in a thread: the file may
//! sit on a slow network folder.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use web_time::{Duration, Instant, UNIX_EPOCH};

use ddc_model::links::Link;
use ddc_ui::notify::Level;

use crate::shell::EditorApp;

const POLL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Watch {
    next: Option<Instant>,
    rx: Option<Receiver<Vec<(i64, i64)>>>,
    /// The new file time that was reported.
    changed: HashMap<i64, i64>,
    /// The file time already seen by the user ("Later", reload).
    seen: HashMap<i64, i64>,
}

/// Call every frame.
pub fn tick(w: &mut Watch, list: &[Link], a: &mut EditorApp, ctx: &egui::Context) {
    if let Some(rx) = &w.rx {
        if let Ok(times) = rx.try_recv() {
            w.rx = None;
            absorb(w, list, a, &times);
        }
    }
    let now = Instant::now();
    if list.is_empty() || w.rx.is_some() || w.next.is_some_and(|t| now < t) {
        return;
    }
    w.next = Some(now + POLL);
    let paths: Vec<(i64, String)> = list.iter().map(|l| (l.id, l.path.clone())).collect();
    let (tx, rx) = mpsc::channel();
    // file times only; a crash leaves the dropped sender, which the poll treats as "no news"
    let _ = crate::jobs::spawn_guarded(
        "link-watch",
        move || {
            let _ = tx.send(modified(&paths));
        },
        |_| {},
    );
    w.rx = Some(rx);
    ctx.request_repaint_after(POLL);
}

fn modified(paths: &[(i64, String)]) -> Vec<(i64, i64)> {
    paths
        .iter()
        .filter_map(|(id, p)| {
            let t = ddc_model::wtime::modified(&std::fs::metadata(p).ok()?).ok()?;
            let s = t.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some((*id, s as i64))
        })
        .collect()
}

fn absorb(w: &mut Watch, list: &[Link], a: &mut EditorApp, times: &[(i64, i64)]) {
    for (id, t) in times {
        let Some(l) = list.iter().find(|l| l.id == *id) else {
            continue;
        };
        let known = w.seen.get(id).copied().unwrap_or(0).max(l.loaded_at);
        if *t <= known {
            w.changed.remove(id);
            continue;
        }
        if w.changed.insert(*id, *t) != Some(*t) {
            let msg = a.sh.cat.tf("links.changed_toast", &[("name", &l.name)]);
            a.sh.toasts.push(Level::Info, msg, None);
        }
    }
}

/// The "file changed" line in the links window; true means the user asks to reload.
pub fn row(ui: &mut egui::Ui, w: &mut Watch, id: i64, t: &dyn Fn(&str) -> String) -> bool {
    let Some(&time) = w.changed.get(&id) else {
        return false;
    };
    let mut reload = false;
    let mut later = false;
    ui.horizontal(|ui| {
        ui.label(t("links.changed"));
        reload = ui.button(t("links.reload")).clicked();
        later = ui.button(t("links.later")).clicked();
    });
    if reload || later {
        w.seen.insert(id, time);
        w.changed.remove(&id);
    }
    reload
}
