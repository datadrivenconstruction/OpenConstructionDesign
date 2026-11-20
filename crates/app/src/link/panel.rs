// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The ERP project panel and the one command "Publish to ERP". Nothing shows until a link
//! exists: a `.ddclink` was opened (after the user turned the link on), or the open model
//! lives in an ERP work folder. Network work runs on a worker thread; the window polls it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

use ddc_link::{LinkError, OsKeyring, RevisionView};
use ddc_ui::notify::Level;

use super::erp::{self, Fail, LockView, Opened, Session};
use crate::shell::EditorApp;

/// The link file's extension (contract §7).
pub const EXT: &str = "ddclink";

enum Done {
    Opened(Result<Opened, Fail>),
    Resumed(Result<Session, Fail>),
    Published(Result<RevisionView, Fail>),
}

struct Job {
    rx: Receiver<Done>,
    step: Arc<Mutex<String>>,
}

#[derive(Default)]
pub struct State {
    session: Option<Session>,
    job: Option<Job>,
    /// Link files waiting for the user to turn the link on.
    consent: Vec<PathBuf>,
    message: String,
    status: Option<(Level, String)>,
    /// A link under the status line: (label, address) of the ERP page that fixes it.
    action: Option<(String, String)>,
    /// Address of the ERP the last link file or work folder named; known before a session exists.
    erp_base: Option<String>,
    /// Work folder of the open model when it came from an ERP project and is not connected.
    offer: Option<PathBuf>,
    seen: Option<PathBuf>,
    /// `.ddclink` registration was requested for the current on-state of the link.
    assoc_done: bool,
    /// When a throttled request may be repeated; the status line counts down to it.
    retry_at: Option<web_time::Instant>,
}

impl State {
    pub fn linked(&self) -> bool {
        self.session.is_some()
    }
}

fn base_dir() -> Option<PathBuf> {
    crate::gfx::data_dir()
}

/// The ERP page that issues links for the active project and offers the editor download.
fn editor_page(base: &str) -> String {
    format!("{}/design/editor", base.trim_end_matches('/'))
}

/// The ERP address a link file names, read without using its code.
fn link_erp(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    ddc_link::LinkFile::parse(&text).ok().map(|l| l.erp.base)
}

/// The ERP address in the `link.json` of a work folder.
fn work_erp(work: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(work.join("link.json")).ok()?).ok()?;
    v["erp_url"].as_str().map(String::from)
}

fn say(a: &mut EditorApp, level: Level, text: String) {
    a.erp.status = Some((level, text));
    a.erp.action = None;
    a.erp.retry_at = None;
}

fn clear_status(a: &mut EditorApp) {
    a.erp.status = None;
    a.erp.action = None;
    a.erp.retry_at = None;
}

/// Shows a failure with the ERP page that resolves it, when there is one.
fn fail(a: &mut EditorApp, f: &Fail) {
    let text = fail_text(&a.sh.cat, a.erp.erp_base.as_deref(), f);
    let action = fail_action(&a.sh.cat, a.erp.erp_base.as_deref(), f);
    a.erp.status = Some((Level::Error, text));
    a.erp.action = action;
    a.erp.retry_at = match f {
        Fail::Link(LinkError::Throttled { retry_after: Some(s) }) => Some(web_time::Instant::now() + std::time::Duration::from_secs(*s)),
        _ => None,
    };
}

fn fail_action(cat: &ddc_ui::i18n::Catalog, base: Option<&str>, f: &Fail) -> Option<(String, String)> {
    let base = base?;
    let key = match f {
        Fail::Link(LinkError::Unsupported { .. }) => "erp.action.download",
        Fail::Link(LinkError::CodeGone) | Fail::Link(LinkError::Unauthorized) | Fail::Link(LinkError::NoToken) => {
            "erp.action.new_link"
        }
        _ => return None,
    };
    Some((cat.t(key), editor_page(base)))
}

fn is_link(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(EXT))
}

/// Takes the `.ddclink` files out of what the open door received; the rest is returned.
pub fn take_links(a: &mut EditorApp, inputs: Vec<PathBuf>) -> Vec<PathBuf> {
    let (links, rest): (Vec<PathBuf>, Vec<PathBuf>) = inputs.into_iter().partition(|p| is_link(p));
    if let Some(first) = links.into_iter().next() {
        if a.prefs.erp_link {
            start_open(a, first);
        } else {
            a.erp.erp_base = link_erp(&first);
            a.erp.consent = vec![first];
        }
    }
    rest
}

fn spawn(a: &mut EditorApp, work: impl FnOnce(&mut dyn FnMut(&str)) -> Done + Send + 'static) {
    let (tx, rx) = channel();
    let step = Arc::new(Mutex::new(String::new()));
    let s = step.clone();
    let _ = crate::jobs::spawn_guarded(
        "ddc-erp-link",
        move || {
            let mut progress = |p: &str| *s.lock().unwrap_or_else(|e| e.into_inner()) = p.to_string();
            let _ = tx.send(work(&mut progress));
        },
        |_| {},
    );
    a.erp.job = Some(Job { rx, step });
    clear_status(a);
}

fn start_open(a: &mut EditorApp, file: PathBuf) {
    if a.erp.job.is_some() {
        a.sh.toasts.push(Level::Info, a.sh.cat.t("erp.busy"), None);
        return;
    }
    let Some(base) = base_dir() else {
        let t = a.sh.cat.t("erp.err.no_data_dir");
        say(a, Level::Error, t);
        return;
    };
    a.erp.erp_base = link_erp(&file);
    spawn(a, move |p| {
        p("connect");
        Done::Opened(erp::open_link(&file, &OsKeyring, &base))
    });
}

fn start_resume(a: &mut EditorApp, work: PathBuf) {
    let Some(base) = base_dir() else { return };
    a.erp.erp_base = work_erp(&work);
    spawn(a, move |p| {
        p("connect");
        Done::Resumed(erp::resume(&work, &OsKeyring, &base))
    });
}

/// "Publish to ERP": saves the model (into the work folder when it has no file yet), then
/// builds and uploads the revision on the worker.
fn start_publish(a: &mut EditorApp) {
    let Some(s) = &a.erp.session else { return };
    let file = match a.ed.project.path() {
        Some(p) if !a.ed.project.read_only() => p.to_path_buf(),
        _ => s.work.join("model.ddc"),
    };
    let saved = if a.ed.project.path() != Some(file.as_path()) {
        a.ed.save_project_as(&file)
    } else if a.ed.is_dirty() {
        a.ed.save_project()
    } else {
        Ok(())
    };
    if let Err(e) = saved {
        let t = a.sh.cat.tf("erp.err.save", &[("detail", &e.to_string())]);
        say(a, Level::Error, t);
        return;
    }
    let msg = Some(a.erp.message.trim().to_string()).filter(|m| !m.is_empty());
    let job = match s.publish_job(&file, msg) {
        Ok(j) => j,
        Err(e) => {
            fail(a, &e);
            return;
        }
    };
    spawn(a, move |p| Done::Published(job.run(p)));
}

/// What the user reads for a failure: the link errors that ask for an action have their own text.
fn fail_text(c: &ddc_ui::i18n::Catalog, base: Option<&str>, f: &Fail) -> String {
    match f {
        Fail::Link(LinkError::Unsupported { supported }) => {
            let list = supported.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
            let t = c.tf("erp.err.unsupported", &[("list", if list.is_empty() { "?" } else { list.as_str() })]);
            match base {
                Some(b) => format!("{t}. {}", c.tf("erp.err.unsupported.page", &[("url", &editor_page(b))])),
                None => format!("{t}. {}", c.t("erp.err.unsupported.admin")),
            }
        }
        Fail::Link(LinkError::CodeGone) => c.t("erp.err.code_gone"),
        Fail::Link(LinkError::Unauthorized) | Fail::Link(LinkError::NoToken) => c.t("erp.err.unauthorized"),
        Fail::Link(LinkError::LockLost) => c.t("erp.err.lock_lost"),
        Fail::Link(LinkError::LockHeld { held_by, .. }) => c.tf("erp.lock.held", &[("name", held_by)]),
        Fail::Link(LinkError::ParentMoved { .. }) => c.t("erp.err.parent_moved"),
        Fail::Link(LinkError::Throttled { retry_after: Some(s) }) => c.tf("erp.err.throttled_in", &[("s", &s.to_string())]),
        Fail::Link(LinkError::Throttled { retry_after: None }) => c.t("erp.err.throttled"),
        Fail::Imported(n) => c.tf("erp.err.imported", &[("n", &n.to_string())]),
        Fail::Link(LinkError::Network(d)) => c.tf("erp.err.network", &[("detail", d)]),
        other => c.tf("erp.err.other", &[("detail", &other.to_string())]),
    }
}

/// The throttle line for `left` seconds still to wait.
fn throttle_line(c: &ddc_ui::i18n::Catalog, left: u64) -> String {
    if left == 0 {
        c.t("erp.err.throttled_ready")
    } else {
        c.tf("erp.err.throttled_in", &[("s", &left.to_string())])
    }
}

/// Whole seconds from `now` to `at`, rounded up.
fn secs_left(at: web_time::Instant, now: web_time::Instant) -> u64 {
    let d = at.saturating_duration_since(now);
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// Ticks the throttle countdown; the status turns to "try again" when it ends.
fn tick_retry(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(at) = a.erp.retry_at else { return };
    let left = secs_left(at, web_time::Instant::now());
    let line = throttle_line(&a.sh.cat, left);
    if left == 0 {
        a.erp.retry_at = None;
        a.erp.status = Some((Level::Info, line));
    } else {
        a.erp.status = Some((Level::Error, line));
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn poll(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(job) = &a.erp.job else { return };
    let done = match job.rx.try_recv() {
        Ok(d) => d,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            a.erp.job = None;
            let t = a.sh.cat.tf("erp.err.other", &[("detail", "worker stopped")]);
            say(a, Level::Error, t);
            return;
        }
    };
    a.erp.job = None;
    match done {
        Done::Opened(Ok(o)) => {
            let name = o.session.ctx.project.name.clone();
            a.erp.erp_base = Some(o.session.linked.erp.base.clone());
            a.erp.session = Some(o.session);
            a.erp.offer = None;
            match o.model {
                Some(p) => {
                    a.erp.seen = Some(p.clone());
                    crate::shell::project_seam::open_project(a, p);
                }
                None => {
                    crate::shell::project_seam::new_project(a, None);
                }
            }
            a.sh.toasts.push(Level::Info, a.sh.cat.tf("erp.connected", &[("name", &name)]), None);
        }
        Done::Resumed(Ok(s)) => {
            a.erp.erp_base = Some(s.linked.erp.base.clone());
            a.erp.session = Some(s);
            a.erp.offer = None;
        }
        Done::Opened(Err(e)) | Done::Resumed(Err(e)) => fail(a, &e),
        Done::Published(r) => {
            if let Some(s) = a.erp.session.as_mut() {
                s.published(&r);
            }
            match r {
                Ok(v) => {
                    a.erp.message.clear();
                    let n = v.number.to_string();
                    let t = a.sh.cat.tf("erp.published", &[("n", &n)]);
                    a.sh.toasts.push(Level::Success, t.clone(), None);
                    say(a, Level::Success, t);
                }
                Err(e) => fail(a, &e),
            }
        }
    }
}

/// The open model changed: offer to reconnect when it belongs to an ERP project.
fn watch_model(a: &mut EditorApp) {
    let now = a.ed.project.path().map(Path::to_path_buf);
    if now == a.erp.seen {
        return;
    }
    a.erp.seen = now.clone();
    if a.erp.session.is_some() {
        return;
    }
    a.erp.offer = now.as_deref().and_then(erp::work_of).filter(|_| a.prefs.erp_link);
}

fn consent_window(a: &mut EditorApp, ctx: &egui::Context) {
    if a.erp.consent.is_empty() {
        return;
    }
    let c = &a.sh.cat;
    let th = ddc_ui::theme::of(ctx);
    let (mut yes, mut no) = (false, false);
    let r = crate::ui_kit::modal_dialog("erp.consent", ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(c.t("erp.consent.title"));
        ui.add_space(ddc_ui::sem::space::SM);
        ui.label(egui::RichText::new(c.t("erp.consent.body")).color(th.c("text.secondary")));
        ui.add_space(ddc_ui::sem::space::MD);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            yes = crate::ui_kit::primary_click(ui, c.t("erp.consent.yes"), None);
            no = crate::ui_kit::cancel_button(ui, c.t("erp.consent.no")).clicked();
        });
    });
    if yes {
        a.prefs.erp_link = true;
        a.prefs.save();
        let files = std::mem::take(&mut a.erp.consent);
        if let Some(f) = files.into_iter().next() {
            start_open(a, f);
        }
    } else if no || r.should_close() {
        // the link stays unused: say how to connect later
        a.erp.consent.clear();
        let t = a.sh.cat.tf("erp.consent.later", &[("setting", &a.sh.cat.t("erp.settings"))]);
        say(a, Level::Info, t);
        a.erp.action = a.erp.erp_base.as_deref().map(|b| (a.sh.cat.t("erp.action.new_link"), editor_page(b)));
    }
}

fn lock_line(a: &EditorApp, s: &Session) -> (Level, String) {
    let c = &a.sh.cat;
    match s.lock() {
        LockView::Mine => (Level::Success, c.t("erp.lock.mine")),
        LockView::HeldBy(n) => (Level::Warning, c.tf("erp.lock.held", &[("name", &n)])),
        LockView::Lost => (Level::Error, c.t("erp.err.lock_lost")),
        LockView::ReadOnly => (Level::Info, c.t("erp.lock.read_only")),
        LockView::Stopped(v) => (Level::Error, fail_text(c, a.erp.erp_base.as_deref(), &Fail::Link(LinkError::Unsupported { supported: v }))),
        LockView::Unauthorized => (Level::Error, c.t("erp.err.unauthorized")),
        LockView::Released => (Level::Info, c.t("erp.lock.none")),
    }
}

/// The package builder reports its own steps; they all read as "building".
fn step_key(step: &str) -> &'static str {
    match step {
        "" | "connect" => "erp.step.connect",
        "tip" => "erp.step.tip",
        "upload" => "erp.step.upload",
        _ => "erp.step.build",
    }
}

fn level_color(th: &ddc_ui::Theme, l: Level) -> egui::Color32 {
    th.c(match l {
        Level::Error => "error",
        Level::Warning => "warning",
        Level::Success => "success",
        _ => "text.secondary",
    })
}

#[derive(Default)]
struct Clicks {
    publish: bool,
    relock: bool,
    close: bool,
    connect: bool,
}

fn panel(a: &mut EditorApp, ctx: &egui::Context) {
    let busy = a.erp.job.as_ref().map(|j| j.step.lock().unwrap_or_else(|e| e.into_inner()).clone());
    if a.erp.session.is_none() && busy.is_none() && a.erp.offer.is_none() && a.erp.status.is_none() {
        return;
    }
    let th = ddc_ui::theme::of(ctx);
    let mut k = Clicks::default();
    let title = a.sh.cat.t("erp.title");
    let mut open = true;
    // what the linked part shows, read before the window borrows the message field
    let linked = a.erp.session.as_ref().map(|s| {
        let c = &a.sh.cat;
        let latest = s
            .revision
            .as_ref()
            .map(|r| (r.number, r.status.clone()))
            .or_else(|| s.ctx.latest_revision.as_ref().map(|r| (r.number, r.status.clone())));
        let rev = match latest {
            Some((n, st)) => {
                let st = c.t(&format!("erp.status.{st}"));
                c.tf("erp.revision", &[("n", &n.to_string()), ("status", &st)])
            }
            None => c.t("erp.revision.none"),
        };
        let blocked = if busy.is_some() {
            Some(c.t("erp.busy"))
        } else if !s.ctx.permissions.can_publish {
            Some(c.t("erp.lock.read_only"))
        } else if !s.can_publish() {
            Some(c.t("erp.publish.needs_lock"))
        } else {
            None
        };
        (s.ctx.project.name.clone(), rev, lock_line(a, s), blocked, s.lock() == LockView::Lost)
    });
    let mut message = std::mem::take(&mut a.erp.message);
    crate::ui_kit::Dialog::hud("erp.panel", title, egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 96.0))
        .id(egui::Id::new("erp.panel"))
        .collapsible(true)
        .open(&mut open)
        .show(ctx, |ui| {
            let c = &a.sh.cat;
            if let Some((name, rev, (lv, lt), blocked, lost)) = &linked {
                ui.strong(name);
                ui.label(rev);
                ui.label(egui::RichText::new(lt).color(level_color(&th, *lv)));
                ui.add_space(ddc_ui::sem::space::SM);
                ui.add(egui::TextEdit::singleline(&mut message).hint_text(c.t("erp.message.hint")).desired_width(f32::INFINITY));
                ui.add_space(ddc_ui::sem::space::SM);
                ui.horizontal(|ui| {
                    k.publish = crate::ui_kit::primary_click(ui, c.t("erp.publish"), blocked.as_deref());
                    if *lost && busy.is_none() {
                        k.relock = crate::ui_kit::secondary_button(ui, c.t("erp.lock.retake")).clicked();
                    }
                    k.close = crate::ui_kit::cancel_button(ui, c.t("erp.disconnect")).clicked();
                });
            } else if let Some(w) = &a.erp.offer {
                let name = w.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(c.tf("erp.offer", &[("name", &name)]));
                k.connect = busy.is_none() && crate::ui_kit::primary_click(ui, c.t("erp.connect"), None);
            }
            if let Some(step) = &busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(c.t(step_key(step)));
                });
            }
            if let Some((l, t)) = &a.erp.status {
                ui.label(egui::RichText::new(t).color(level_color(&th, *l)));
            }
            if let Some((label, url)) = &a.erp.action {
                ui.hyperlink_to(label, url);
            }
        });
    a.erp.message = message;
    if k.publish {
        start_publish(a);
    }
    if k.relock {
        if let Some(s) = a.erp.session.as_mut() {
            let r = s.take_lock();
            if let Err(e) = r {
                fail(a, &e);
            } else {
                clear_status(a);
            }
        }
    }
    if k.connect {
        if let Some(w) = a.erp.offer.clone() {
            start_resume(a, w);
        }
    }
    if k.close || !open {
        if let Some(mut s) = a.erp.session.take() {
            let _ = s.close();
        }
        a.erp.offer = None;
        clear_status(a);
    }
}

/// True once per switch-on of the link (and once per launch while it is on, which also
/// refreshes the registered exe path); never while the link is off.
fn assoc_due(done: &mut bool, link_on: bool) -> bool {
    let go = link_on && !*done;
    *done = link_on;
    go
}

pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    // only with a settings file: screenshots and tests never touch the OS registration
    if a.prefs.path.is_some() && !crate::open::assoc_busy(a) && assoc_due(&mut a.erp.assoc_done, a.prefs.erp_link) {
        let req = crate::os_assoc::Req { register: true, default_models: false };
        a.open.assoc_quiet = true;
        crate::open::assoc_start(a, req);
    }
    poll(a, ctx);
    watch_model(a);
    tick_retry(a, ctx);
    consent_window(a, ctx);
    panel(a, ctx);
}

#[cfg(test)]
mod assoc_tests {
    use super::assoc_due;

    #[test]
    fn registers_once_when_the_link_turns_on_never_while_off() {
        let mut done = false;
        assert!(!assoc_due(&mut done, false));
        assert!(assoc_due(&mut done, true));
        assert!(!assoc_due(&mut done, true));
        assert!(!assoc_due(&mut done, false));
        assert!(assoc_due(&mut done, true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::{Catalog, Lang};
    use web_time::{Duration, Instant};

    const BASE: &str = "https://erp.example/";

    fn each(f: impl Fn(&Catalog, Lang)) {
        for l in [Lang::En, Lang::Ru] {
            f(&Catalog::new(l), l);
        }
    }

    #[test]
    fn unsupported_text_shows_update_address() {
        each(|c, _| {
            let f = Fail::Link(LinkError::Unsupported { supported: vec![1, 2] });
            let t = fail_text(c, Some(BASE), &f);
            assert!(t.contains("https://erp.example/design/editor"), "{t}");
            assert!(t.contains("1, 2"), "{t}");
            let (label, url) = fail_action(c, Some(BASE), &f).expect("download button");
            assert_eq!(url, "https://erp.example/design/editor");
            assert_eq!(label, c.t("erp.action.download"));
            // no address known: the text sends to the admin, no button
            assert_eq!(fail_text(c, None, &f).contains("http"), false);
            assert!(fail_action(c, None, &f).is_none());
        });
    }

    #[test]
    fn code_gone_has_reopen_button() {
        each(|c, _| {
            let f = Fail::Link(LinkError::CodeGone);
            let (label, url) = fail_action(c, Some(BASE), &f).expect("button");
            assert_eq!(url, "https://erp.example/design/editor");
            assert_eq!(label, c.t("erp.action.new_link"));
            assert_ne!(label, "erp.action.new_link");
            assert!(fail_action(c, None, &f).is_none());
        });
    }

    #[test]
    fn throttle_counts_down_and_ends() {
        each(|c, _| {
            let f = Fail::Link(LinkError::Throttled { retry_after: Some(30) });
            assert!(fail_text(c, None, &f).contains("30"));
            assert!(throttle_line(c, 7).contains('7'));
            assert!(!throttle_line(c, 7).contains("{s}"));
            let ready = throttle_line(c, 0);
            assert_ne!(ready, "erp.err.throttled_ready");
            assert_ne!(ready, throttle_line(c, 1));
        });
        let now = Instant::now();
        let at = now + Duration::from_millis(2300);
        assert_eq!(secs_left(at, now), 3);
        assert_eq!(secs_left(at, now + Duration::from_millis(1400)), 1);
        assert_eq!(secs_left(at, at), 0);
        assert_eq!(secs_left(at, at + Duration::from_secs(5)), 0);
    }

    #[test]
    fn consent_later_names_the_setting() {
        each(|c, _| {
            let setting = c.t("erp.settings");
            let t = c.tf("erp.consent.later", &[("setting", &setting)]);
            assert!(t.contains(&setting), "{t}");
            assert!(!t.contains('{'), "{t}");
        });
    }
}
