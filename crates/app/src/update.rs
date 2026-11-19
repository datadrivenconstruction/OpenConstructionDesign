//! Notify-only update check. One GET to our own endpoint carrying three facts (version, OS and
//! architecture, install kind). It never downloads or installs anything and has no telemetry:
//! the answer is a version, a date, a summary and a release page the user opens on request.
//! Off by default; the Help command works whatever the frequency is.
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::notify::Level;
use ddc_ui::theme::{icon_size, space, Role};
use ddc_ui::widgets::surface::spinner;
use ddc_ui::widgets::{with_icons, Button, Tone};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};
use std::cmp::Ordering;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const ENDPOINT: &str = "https://updates.datadrivenconstruction.io/ddc-editor/v1/latest";
/// Query keys, in order: the only things a request says about the user.
pub const PARAMS: [&str; 3] = ["v", "os", "kind"];
const DAY: u64 = 86_400;
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY: u64 = 64 * 1024;
const MAX_TEXT: usize = 600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Freq {
    Startup,
    Daily,
    Weekly,
    Never,
}

impl Freq {
    pub const ALL: [Freq; 4] = [Freq::Startup, Freq::Daily, Freq::Weekly, Freq::Never];

    pub fn key(self) -> &'static str {
        match self {
            Freq::Startup => "startup",
            Freq::Daily => "daily",
            Freq::Weekly => "weekly",
            Freq::Never => "never",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Freq::Startup => "update_check.freq.startup",
            Freq::Daily => "update_check.freq.daily",
            Freq::Weekly => "update_check.freq.weekly",
            Freq::Never => "update_check.freq.never",
        }
    }

    pub fn parse(s: &str) -> Option<Freq> {
        Freq::ALL.into_iter().find(|f| f.key() == s)
    }
}

/// Stored in `update.txt` next to the other user files, not in a project.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub freq: Freq,
    /// Unix seconds of the last check that got an answer.
    pub last: u64,
    /// Newest version already announced by an automatic check.
    pub seen: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            freq: Freq::Never,
            last: 0,
            seen: String::new(),
        }
    }
}

impl Settings {
    pub fn parse(s: &str) -> Self {
        let mut p = Settings::default();
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "freq" => p.freq = Freq::parse(v.trim()).unwrap_or(p.freq),
                "last" => p.last = v.trim().parse().unwrap_or(0),
                "seen" => p.seen = v.trim().chars().take(40).collect(),
                _ => {}
            }
        }
        p
    }

    pub fn text(&self) -> String {
        format!(
            "freq={}\nlast={}\nseen={}\n",
            self.freq.key(),
            self.last,
            self.seen.replace(['\n', '\r'], " ")
        )
    }
}

fn settings_path() -> Option<std::path::PathBuf> {
    crate::gfx::data_dir().map(|d| d.join("update.txt"))
}

fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| Settings::parse(&s))
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    if let Some(p) = settings_path() {
        let _ = std::fs::write(p, s.text());
    }
}

/// The three facts a request carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub version: String,
    pub os_arch: String,
    pub kind: String,
}

impl Facts {
    pub fn current() -> Self {
        let exe = std::env::current_exe().unwrap_or_default();
        let roots: Vec<String> = ["ProgramFiles", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .collect();
        Facts {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os_arch: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            kind: install_kind(&exe, &roots).to_string(),
        }
    }

    pub fn pairs(&self) -> [(&'static str, &str); 3] {
        [
            (PARAMS[0], &self.version),
            (PARAMS[1], &self.os_arch),
            (PARAMS[2], &self.kind),
        ]
    }

    pub fn url(&self, base: &str) -> String {
        let q: Vec<String> = self
            .pairs()
            .iter()
            .map(|(k, v)| format!("{k}={}", pct(v)))
            .collect();
        format!("{base}?{}", q.join("&"))
    }
}

/// "installed" under a program-files root, otherwise "portable".
pub fn install_kind(exe: &std::path::Path, roots: &[String]) -> &'static str {
    let e = exe.to_string_lossy().to_lowercase().replace('\\', "/");
    let under = roots.iter().any(|r| {
        let r = r.to_lowercase().replace('\\', "/");
        !r.is_empty() && e.starts_with(r.trim_end_matches('/'))
    });
    if under {
        "installed"
    } else {
        "portable"
    }
}

fn pct(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub summary: String,
    /// Empty when the reply gave no page on our own host.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// No usable answer. Never shown as "up to date".
    Unknown(String),
    Current,
    Newer(Release),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub status: Status,
    /// Minimum recommended version when this build is below it.
    pub unfit_below: Option<String>,
}

fn unknown(why: &str) -> Outcome {
    Outcome {
        status: Status::Unknown(why.to_string()),
        unfit_below: None,
    }
}

/// Numeric dotted version; a `-suffix` is ignored.
fn parse_ver(v: &str) -> Option<Vec<u64>> {
    let core = v.trim().trim_start_matches('v').split('-').next()?;
    let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

pub fn cmp_ver(a: &str, b: &str) -> Option<Ordering> {
    let (mut x, mut y) = (parse_ver(a)?, parse_ver(b)?);
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    Some(x.cmp(&y))
}

fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_TEXT)
        .collect()
}

fn host(u: &str) -> Option<&str> {
    let r = u.strip_prefix("https://")?;
    let h = r.split(['/', '?', '#']).next()?;
    (!h.is_empty() && !h.contains('@')).then_some(h)
}

/// A release page is opened only when it is https on the endpoint's own host.
pub fn safe_release_url(u: &str, base: &str) -> Option<String> {
    let ok = !u.chars().any(|c| c.is_whitespace() || c.is_control())
        && host(u)?.eq_ignore_ascii_case(host(base)?);
    ok.then(|| u.to_string())
}

/// Reads the reply: `{version, date, summary, url, unfit_below}`.
pub fn evaluate(current: &str, base: &str, body: &str) -> Outcome {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return unknown("malformed reply");
    };
    let text = |k: &str| v.get(k).and_then(|x| x.as_str());
    let Some(latest) = text("version") else {
        return unknown("malformed reply");
    };
    let Some(ord) = cmp_ver(latest, current) else {
        return unknown("unreadable version");
    };
    let unfit_below = text("unfit_below")
        .filter(|f| cmp_ver(current, f) == Some(Ordering::Less))
        .map(clean);
    let status = if ord == Ordering::Greater {
        Status::Newer(Release {
            version: clean(latest),
            date: clean(text("date").unwrap_or("")).chars().take(32).collect(),
            summary: clean(text("summary").unwrap_or("")),
            url: text("url")
                .and_then(|u| safe_release_url(u, base))
                .unwrap_or_default(),
        })
    } else {
        Status::Current
    };
    Outcome {
        status,
        unfit_below,
    }
}

pub type Fetch<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// One request; any failure is `Unknown`, never `Current`.
pub fn check(fetch: Fetch, base: &str, facts: &Facts) -> Outcome {
    match fetch(&facts.url(base)) {
        Ok(body) => evaluate(&facts.version, base, &body),
        Err(e) => unknown(&clean(&e)),
    }
}

fn http_get(url: &str) -> Result<String, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| format!("tls: {e}"))?;
    let agent = ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .redirects(0)
        .timeout_connect(TIMEOUT)
        .timeout_read(TIMEOUT)
        .timeout_write(TIMEOUT)
        .https_only(true)
        .user_agent("ddc-editor")
        .build();
    match agent.get(url).call() {
        Ok(r) => {
            let mut s = String::new();
            r.into_reader()
                .take(MAX_BODY)
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
            Ok(s)
        }
        Err(ureq::Error::Status(c, _)) => Err(format!("HTTP {c}")),
        Err(e) => Err(e.to_string()),
    }
}

fn span(freq: Freq) -> Option<u64> {
    match freq {
        Freq::Startup => Some(0),
        Freq::Daily => Some(DAY),
        Freq::Weekly => Some(7 * DAY),
        Freq::Never => None,
    }
}

/// (run an automatic check, run a manual check). The manual one ignores the frequency.
pub fn decide(freq: Freq, last: u64, now: u64, started: bool, manual: bool) -> (bool, bool) {
    let auto = !started
        && span(freq).is_some_and(|s| last == 0 || now < last || now - last >= s);
    (auto && !manual, manual)
}

#[derive(Debug, PartialEq)]
pub enum Note {
    Newer(String),
    Unfit { version: String, floor: String },
}

/// What an automatic check says out loud: nothing unless there is news.
pub fn notes(o: &Outcome, seen: &str, unfit_told: bool, current: &str) -> Vec<Note> {
    let mut n = vec![];
    if let Status::Newer(r) = &o.status {
        if r.version != seen {
            n.push(Note::Newer(r.version.clone()));
        }
    }
    if let (Some(f), false) = (&o.unfit_below, unfit_told) {
        n.push(Note::Unfit {
            version: current.to_string(),
            floor: f.clone(),
        });
    }
    n
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    let tool = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(tool)
        .arg(url)
        .spawn()
        .map(|_| ())
}

type Slot = Arc<Mutex<Option<Outcome>>>;

struct State {
    settings: Option<Settings>,
    started: bool,
    want_manual: bool,
    job: Option<Slot>,
    job_manual: bool,
    dialog: bool,
    result: Option<Outcome>,
    unfit_told: bool,
}

static ST: Mutex<State> = Mutex::new(State {
    settings: None,
    started: false,
    want_manual: false,
    job: None,
    job_manual: false,
    dialog: false,
    result: None,
    unfit_told: false,
});

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = ST.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

fn settings(st: &mut State) -> &mut Settings {
    st.settings.get_or_insert_with(load_settings)
}

pub fn freq() -> Freq {
    with(|s| settings(s).freq)
}

pub fn set_freq(f: Freq) {
    with(|s| {
        let c = settings(s);
        if c.freq != f {
            c.freq = f;
            save_settings(c);
        }
    });
}

/// The Help command: works on every frequency, including "never".
pub fn request_manual() {
    with(|s| s.want_manual = true);
}

pub fn register(app: &mut EditorApp) {
    app.register_command("help.check_update", |_| request_manual());
}

fn start(st: &mut State, ctx: &egui::Context, manual: bool) {
    if st.job.is_some() {
        st.dialog |= manual;
        return;
    }
    if manual {
        st.dialog = true;
        st.result = None;
    }
    st.job_manual = manual;
    let slot: Slot = Arc::new(Mutex::new(None));
    st.job = Some(slot.clone());
    let ctx = ctx.clone();
    let (slot_w, slot_p, ctx_p) = (slot.clone(), slot.clone(), ctx.clone());
    let spawned = crate::jobs::spawn_guarded(
        "ri12-update",
        move || {
            let o = check(&http_get, ENDPOINT, &Facts::current());
            *slot_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(o);
            ctx.request_repaint();
        },
        move |msg| {
            *slot_p.lock().unwrap_or_else(|e| e.into_inner()) = Some(unknown(&msg));
            ctx_p.request_repaint();
        },
    );
    if spawned.is_err() {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(unknown("no worker thread"));
    }
}

enum Act {
    Close,
    Check,
    Open(String),
}

const NOTES_MAX_H: f32 = 168.0;
const NOTES_SLOT: usize = 5_050;

/// What the dialog shows: one designed state per phase.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Checking,
    Current,
    Newer,
    Error,
}

pub fn phase(busy: bool, result: Option<&Outcome>) -> Phase {
    if busy {
        return Phase::Checking;
    }
    match result.map(|o| &o.status) {
        None => Phase::Idle,
        Some(Status::Current) => Phase::Current,
        Some(Status::Newer(_)) => Phase::Newer,
        Some(Status::Unknown(_)) => Phase::Error,
    }
}

/// Icon name, tone, title key, body key of a phase.
pub fn update_view(p: Phase) -> (&'static str, Tone, &'static str, &'static str) {
    match p {
        Phase::Idle => ("level-info", Tone::Neutral, "update_check.up.idle.title", "update_check.up.idle.body"),
        Phase::Checking => ("level-info", Tone::Accent, "update_check.up.checking.title", "update_check.up.checking.body"),
        Phase::Current => ("level-success", Tone::Success, "update_check.up.current.title", "update_check.up.current.body"),
        Phase::Newer => ("level-info", Tone::Accent, "update_check.up.newer.title", "update_check.up.newer.body"),
        Phase::Error => ("level-error", Tone::Danger, "update_check.up.error.title", "update_check.up.error.body"),
    }
}

/// Title and muted body of a phase, with the version, date and reason filled in.
pub fn phase_text(c: &Catalog, p: Phase, o: Option<&Outcome>) -> (String, String) {
    let (_, _, tk, bk) = update_view(p);
    let ver = env!("CARGO_PKG_VERSION");
    match (p, o.map(|o| &o.status)) {
        (Phase::Current, _) => (c.tf(tk, &[("v", ver)]), c.t(bk)),
        (Phase::Newer, Some(Status::Newer(r))) => {
            let body = if r.date.is_empty() {
                c.t("update_check.up.newer.nodate")
            } else {
                c.tf(bk, &[("d", r.date.as_str())])
            };
            (c.tf(tk, &[("v", r.version.as_str())]), body)
        }
        (Phase::Error, Some(Status::Unknown(e))) => (c.t(tk), c.tf(bk, &[("e", e.as_str())])),
        _ => (c.t(tk), c.t(bk)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Primary {
    Check,
    Retry,
    Open,
    Close,
}

/// The one primary action of a phase; none while a check runs.
pub fn primary_for(p: Phase) -> Option<Primary> {
    match p {
        Phase::Idle => Some(Primary::Check),
        Phase::Checking => None,
        Phase::Current => Some(Primary::Close),
        Phase::Newer => Some(Primary::Open),
        Phase::Error => Some(Primary::Retry),
    }
}

pub fn primary_key(p: Primary) -> &'static str {
    match p {
        Primary::Check => "update_check.up.check",
        Primary::Retry => "update_check.up.retry",
        Primary::Open => "update_check.up.open",
        Primary::Close => "update_check.close",
    }
}

/// Status mark (icon or spinner), a title and a muted body.
fn state_row(ui: &mut egui::Ui, th: &Theme, p: Phase, title: &str, body: &str) {
    let (icon, tone, _, _) = update_view(p);
    ui.horizontal_top(|ui| {
        if p == Phase::Checking {
            spinner(ui, th);
        } else {
            let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(icon_size::LG), egui::Sense::hover());
            let col = tone.foreground(th.sem());
            let ctx = ui.ctx().clone();
            with_icons(&ctx, |ic| ic.paint(ui, icon, icon_size::LG as u32, r, col));
        }
        ui.add_space(space::SM);
        ui.vertical(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(title)
                        .font(th.font(Role::BodyStrong))
                        .color(th.c("text.primary")),
                )
                .wrap(),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(body)
                        .font(th.font(Role::Body))
                        .color(th.c("text.secondary")),
                )
                .wrap(),
            );
        });
    });
}

/// Per frame: starts due checks, collects the answer, draws the manual result window.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut said: Vec<Note> = vec![];
    let view = with(|st| {
        let (freq, last) = {
            let s = settings(st);
            (s.freq, s.last)
        };
        let (auto, manual) = decide(freq, last, now, st.started, st.want_manual);
        st.started = true;
        st.want_manual = false;
        if auto || manual {
            start(st, ctx, manual);
        }
        let done = st
            .job
            .as_ref()
            .and_then(|j| j.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(o) = done {
            st.job = None;
            if !matches!(o.status, Status::Unknown(_)) {
                settings(st).last = now;
            }
            if st.job_manual {
                st.result = Some(o);
            } else {
                let seen = settings(st).seen.clone();
                said = notes(&o, &seen, st.unfit_told, env!("CARGO_PKG_VERSION"));
                for n in &said {
                    match n {
                        Note::Newer(v) => settings(st).seen = v.clone(),
                        Note::Unfit { .. } => st.unfit_told = true,
                    }
                }
            }
            save_settings(settings(st));
        }
        st.dialog.then(|| (st.job.is_some(), st.result.clone()))
    });
    for n in said {
        let (level, msg) = match n {
            Note::Newer(v) => (
                Level::Info,
                app.sh.cat.tf("update_check.toast.newer", &[("v", v.as_str())]),
            ),
            Note::Unfit { version, floor } => (
                Level::Warning,
                app.sh.cat.tf(
                    "update_check.toast.unfit",
                    &[("v", version.as_str()), ("f", floor.as_str())],
                ),
            ),
        };
        app.sh.toasts.push(level, msg, None);
    }
    let Some((busy, result)) = view else {
        return;
    };
    let mut act: Option<Act> = None;
    let mut open = true;
    let ph = phase(busy, result.as_ref());
    {
        let th = &app.sh.theme;
        let c = &app.sh.cat;
        let cx = crate::ai::ui::Cx {
            th,
            cat: c,
            ed: &app.ed,
            view3d: false,
            palette_open: false,
        };
        let title = c.t("update_check.dialog.title");
        kit::Dialog::new("ri12.update", title, kit::DialogSize::Form)
            .open(&mut open)
            .show(ctx, |ui| {
                let (head, sub) = phase_text(c, ph, result.as_ref());
                state_row(ui, th, ph, &head, &sub);
                let newer = match result.as_ref().map(|o| &o.status) {
                    Some(Status::Newer(r)) => Some(r),
                    _ => None,
                };
                if let Some(r) = newer.filter(|r| !r.summary.is_empty()) {
                    ui.add_space(space::MD);
                    egui::ScrollArea::vertical()
                        .id_salt("ux50e.notes")
                        .max_height(NOTES_MAX_H)
                        .show(ui, |ui| {
                            crate::ai::ui::md::answer(ui, &cx, &r.summary, NOTES_SLOT);
                        });
                }
                if let Some(f) = result.as_ref().and_then(|o| o.unfit_below.as_ref()) {
                    ui.add_space(space::MD);
                    let v = env!("CARGO_PKG_VERSION");
                    ui.add(
                        egui::Label::new(
                            RichText::new(c.tf("update_check.unfit", &[("v", v), ("f", f.as_str())]))
                                .font(th.font(Role::Body))
                                .color(th.c("warning")),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(space::SM);
                let prim = primary_for(ph);
                kit::dialog_footer(ui, |ui| {
                    if let Some(p) = prim {
                        let label = c.t(primary_key(p));
                        if Button::primary(&label).show(ui, th).clicked() {
                            act = Some(match (p, newer) {
                                (Primary::Open, Some(r)) => Act::Open(r.url.clone()),
                                (Primary::Close, _) => Act::Close,
                                _ => Act::Check,
                            });
                        }
                    }
                    if prim != Some(Primary::Close)
                        && Button::secondary(&c.t("update_check.close")).show(ui, th).clicked()
                    {
                        act = Some(Act::Close);
                    }
                });
            });
    }
    if !open {
        act = Some(Act::Close);
    }
    match act {
        Some(Act::Check) => with(|st| start(st, ctx, true)),
        Some(Act::Close) => with(|st| st.dialog = false),
        Some(Act::Open(u)) => {
            if open_url(&u).is_err() {
                let msg = app.sh.cat.t("update_check.open_failed");
                app.sh.toasts.push(Level::Warning, msg, None);
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const BASE: &str = "https://updates.example.test/v1/latest";

    fn facts() -> Facts {
        Facts {
            version: "1.2.3".into(),
            os_arch: "windows-x86_64".into(),
            kind: "portable".into(),
        }
    }

    #[test]
    fn unreachable_is_unknown_not_current() {
        let down = |_: &str| Err::<String, String>("connection refused".into());
        let o = check(&down, BASE, &facts());
        assert!(matches!(o.status, Status::Unknown(_)));
        assert_ne!(o.status, Status::Current);
        for body in ["", "not json", "{}", r#"{"version":"x.y"}"#] {
            let o = evaluate("1.2.3", BASE, body);
            assert!(matches!(o.status, Status::Unknown(_)), "{body}");
        }
    }

    #[test]
    fn unfit_floor_warns() {
        let body = |v: &str, f: &str| format!(r#"{{"version":"{v}","unfit_below":"{f}"}}"#);
        let o = evaluate("1.2.3", BASE, &body("1.2.3", "1.3.0"));
        assert_eq!(o.status, Status::Current);
        assert_eq!(o.unfit_below.as_deref(), Some("1.3.0"));
        assert!(evaluate("1.2.3", BASE, &body("2.0.0", "1.3.0")).unfit_below.is_some());
        assert!(evaluate("1.3.0", BASE, &body("2.0.0", "1.3.0")).unfit_below.is_none());
        assert!(evaluate("1.2.3", BASE, r#"{"version":"1.2.3"}"#).unfit_below.is_none());
        let n = notes(&evaluate("1.2.3", BASE, &body("1.2.3", "1.3.0")), "", false, "1.2.3");
        assert!(matches!(n.as_slice(), [Note::Unfit { .. }]));
    }

    #[test]
    fn request_carries_three_facts_only() {
        let seen = RefCell::new(String::new());
        let f = |u: &str| {
            *seen.borrow_mut() = u.to_string();
            Err::<String, String>("x".into())
        };
        check(&f, BASE, &facts());
        let url = seen.borrow().clone();
        let (path, q) = url.split_once('?').unwrap();
        assert_eq!(path, BASE);
        let keys: Vec<&str> = q.split('&').map(|p| p.split('=').next().unwrap()).collect();
        assert_eq!(keys, PARAMS);
        assert_eq!(q, "v=1.2.3&os=windows-x86_64&kind=portable");
        assert_eq!(facts().pairs().len(), 3);
        assert_eq!(pct("a b&c"), "a%20b%26c");
    }

    #[test]
    fn newer_reply_has_release_details() {
        let b = r#"{"version":"1.3.0","date":"2026-11-01","summary":"Fixes","url":"https://updates.example.test/r/1.3.0"}"#;
        let Status::Newer(r) = evaluate("1.2.3", BASE, b).status else {
            panic!("expected newer");
        };
        assert_eq!((r.version.as_str(), r.date.as_str()), ("1.3.0", "2026-11-01"));
        assert_eq!(r.url, "https://updates.example.test/r/1.3.0");
        assert_eq!(evaluate("1.3.0", BASE, b).status, Status::Current);
        assert_eq!(evaluate("1.10.0", BASE, b).status, Status::Current);
    }

    #[test]
    fn release_page_stays_on_our_host() {
        for bad in [
            "http://updates.example.test/r",
            "https://evil.test/r",
            "https://updates.example.test@evil.test/r",
            "https://updates.example.test/a b",
            "javascript:alert(1)",
        ] {
            assert_eq!(safe_release_url(bad, BASE), None, "{bad}");
        }
        assert!(safe_release_url("https://UPDATES.example.test/r?x=1", BASE).is_some());
    }

    #[test]
    fn frequency_and_manual_rules() {
        let day = DAY;
        assert_eq!(decide(Freq::Never, 0, 100, false, false), (false, false));
        assert_eq!(decide(Freq::Never, 0, 100, false, true), (false, true));
        assert_eq!(decide(Freq::Startup, 50, 60, false, false), (true, false));
        assert_eq!(decide(Freq::Startup, 50, 60, true, false), (false, false));
        assert_eq!(decide(Freq::Daily, 1000, 1000 + day - 1, false, false).0, false);
        assert_eq!(decide(Freq::Daily, 1000, 1000 + day, false, false).0, true);
        assert_eq!(decide(Freq::Weekly, 1000, 1000 + 6 * day, false, false).0, false);
        assert_eq!(decide(Freq::Weekly, 0, 5, false, false).0, true);
        assert_eq!(decide(Freq::Daily, 9000, 10, false, false).0, true);
    }

    #[test]
    fn silent_when_nothing_new() {
        let cur = evaluate("1.2.3", BASE, r#"{"version":"1.2.3"}"#);
        assert!(notes(&cur, "", false, "1.2.3").is_empty());
        let down = unknown("offline");
        assert!(notes(&down, "", false, "1.2.3").is_empty());
        let new = evaluate("1.2.3", BASE, r#"{"version":"1.3.0"}"#);
        assert_eq!(notes(&new, "", false, "1.2.3"), vec![Note::Newer("1.3.0".into())]);
        assert!(notes(&new, "1.3.0", false, "1.2.3").is_empty());
    }

    #[test]
    fn settings_round_trip_and_default_off() {
        assert_eq!(Settings::default().freq, Freq::Never);
        let s = Settings {
            freq: Freq::Weekly,
            last: 77,
            seen: "1.3.0".into(),
        };
        assert_eq!(Settings::parse(&s.text()), s);
        assert_eq!(Settings::parse("freq=hourly\nlast=x\njunk").freq, Freq::Never);
    }

    fn newer_outcome() -> Outcome {
        evaluate(
            "1.2.3",
            BASE,
            r#"{"version":"1.3.0","date":"2026-11-01","summary":"- Fixes"}"#,
        )
    }

    fn cats() -> [Catalog; 2] {
        [Catalog::new(ddc_ui::Lang::En), Catalog::new(ddc_ui::Lang::Ru)]
    }

    #[test]
    fn phase_follows_job_and_result() {
        assert_eq!(phase(false, None), Phase::Idle);
        assert_eq!(phase(true, None), Phase::Checking);
        assert_eq!(phase(true, Some(&newer_outcome())), Phase::Checking);
        assert_eq!(phase(false, Some(&newer_outcome())), Phase::Newer);
        assert_eq!(phase(false, Some(&unknown("x"))), Phase::Error);
        let cur = evaluate("1.2.3", BASE, r#"{"version":"1.2.3"}"#);
        assert_eq!(phase(false, Some(&cur)), Phase::Current);
    }

    #[test]
    fn every_phase_renders_text_in_both_languages() {
        let feed = format!(r#"{{"version":"{}"}}"#, env!("CARGO_PKG_VERSION"));
        let cur = evaluate(env!("CARGO_PKG_VERSION"), BASE, &feed);
        let cases = [
            (Phase::Idle, None),
            (Phase::Checking, None),
            (Phase::Current, Some(cur)),
            (Phase::Newer, Some(newer_outcome())),
            (Phase::Error, Some(unknown("connection refused"))),
        ];
        for c in cats() {
            for (p, o) in &cases {
                let (_, _, tk, bk) = update_view(*p);
                let (title, body) = phase_text(&c, *p, o.as_ref());
                assert!(!title.is_empty() && !body.is_empty(), "{p:?}");
                assert_ne!(title, tk, "missing title {tk}");
                assert_ne!(body, bk, "missing body {bk}");
                assert!(!title.contains('{') && !body.contains('{'), "{p:?}");
            }
        }
        let [en, ru] = cats();
        let o = newer_outcome();
        let (t_en, b_en) = phase_text(&en, Phase::Newer, Some(&o));
        let (t_ru, _) = phase_text(&ru, Phase::Newer, Some(&o));
        assert!(t_en.contains("1.3.0") && t_ru.contains("1.3.0"));
        assert!(b_en.contains("2026-11-01"));
        assert_ne!(t_en, t_ru);
    }

    #[test]
    fn error_shows_reason_and_retry() {
        let o = unknown("connection refused");
        for c in cats() {
            let (_, body) = phase_text(&c, Phase::Error, Some(&o));
            assert!(body.contains("connection refused"));
            assert!(!c.t(primary_key(Primary::Retry)).starts_with("update_check."));
        }
        assert_eq!(primary_for(Phase::Error), Some(Primary::Retry));
    }

    #[test]
    fn one_primary_per_state() {
        assert_eq!(primary_for(Phase::Checking), None);
        assert_eq!(primary_for(Phase::Newer), Some(Primary::Open));
        assert_eq!(primary_for(Phase::Current), Some(Primary::Close));
        assert_eq!(primary_for(Phase::Idle), Some(Primary::Check));
        for p in [Primary::Check, Primary::Retry, Primary::Open, Primary::Close] {
            for c in cats() {
                let t = c.t(primary_key(p));
                assert!(!t.is_empty() && t != primary_key(p), "{p:?}");
            }
        }
    }

    #[test]
    fn dialog_closes_on_escape() {
        let src = include_str!("update.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        assert!(code.contains("DialogSize::Form") && code.contains(".open(&mut open)"));
    }

    #[test]
    fn dialog_uses_kit_only() {
        let src = include_str!("update.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for bad in [
            "ui.checkbox(",
            "egui::Button::new",
            "add_space(1",
            "add_space(2",
            "add_space(4",
            "add_space(6",
            "add_space(8",
            "set_width(420",
            "ui.spinner(",
            "shell_kit::",
        ] {
            assert!(!code.contains(bad), "raw widget left: {bad}");
        }
    }

    #[test]
    fn install_kind_by_root() {
        let roots = vec!["C:\\Program Files".to_string()];
        let p = std::path::Path::new("C:\\Program Files\\DDC\\ddc-app.exe");
        assert_eq!(install_kind(p, &roots), "installed");
        let q = std::path::Path::new("D:\\tools\\ddc-app.exe");
        assert_eq!(install_kind(q, &roots), "portable");
        assert_eq!(install_kind(q, &[]), "portable");
    }
}
