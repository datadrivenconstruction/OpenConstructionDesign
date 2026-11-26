// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Notify-only update check. One GET to our own endpoint carrying three facts (version, OS and
//! architecture, install kind). It never downloads or installs anything and has no telemetry:
//! the answer is a version, a date, a summary and a release page the user opens on request.
//! Off by default; the Help command works whatever the frequency is.
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::notify::{Level, ToastAction};
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
    /// Newest version already announced (an automatic check or the dialog).
    pub seen: String,
    /// Version the user chose to skip: no toast and no pill for it.
    pub skip: String,
    /// Version of the last run; empty on a fresh install.
    pub ran: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            freq: Freq::Never,
            last: 0,
            seen: String::new(),
            skip: String::new(),
            ran: String::new(),
        }
    }
}

fn ver_field(v: &str) -> String {
    v.trim().chars().take(40).collect()
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
                "seen" => p.seen = ver_field(v),
                "skip" => p.skip = ver_field(v),
                "ran" => p.ran = ver_field(v),
                _ => {}
            }
        }
        p
    }

    pub fn text(&self) -> String {
        format!(
            "freq={}\nlast={}\nseen={}\nskip={}\nran={}\n",
            self.freq.key(),
            self.last,
            self.seen.replace(['\n', '\r'], " "),
            self.skip.replace(['\n', '\r'], " "),
            self.ran.replace(['\n', '\r'], " ")
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

/// The last newer release seen, kept so the pill and the after-update "What's new" can show
/// its notes without asking the server again.
fn release_path() -> Option<std::path::PathBuf> {
    crate::gfx::data_dir().map(|d| d.join("update_notes.json"))
}

pub fn release_text(r: &Release) -> String {
    serde_json::json!({
        "version": r.version,
        "date": r.date,
        "summary": r.summary,
        "url": r.url,
    })
    .to_string()
}

/// Reads a cached release with the same limits as a server reply.
pub fn release_parse(s: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let version = clean(text("version"));
    parse_ver(&version)?;
    Some(Release {
        version,
        date: clean(text("date")).chars().take(32).collect(),
        summary: clean(text("summary")),
        url: safe_release_url(text("url"), ENDPOINT).unwrap_or_default(),
    })
}

fn load_release() -> Option<Release> {
    release_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| release_parse(&s))
}

fn save_release(r: &Release) {
    if let Some(p) = release_path() {
        let _ = std::fs::write(p, release_text(r));
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

#[cfg(target_arch = "wasm32")]
fn http_get(_url: &str) -> Result<String, String> {
    // the browser build is updated with the ERP, never by itself
    Err("no update check in the browser".into())
}

#[cfg(not(target_arch = "wasm32"))]
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

/// What an automatic check says out loud: nothing unless there is news the user did not skip.
pub fn notes(o: &Outcome, seen: &str, unfit_told: bool, current: &str, skip: &str) -> Vec<Note> {
    let mut n = vec![];
    if let Status::Newer(r) = &o.status {
        if r.version != seen && r.version != skip {
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

/// Version the pill offers: a newer one already announced and not skipped.
pub fn pending(seen: &str, skip: &str, current: &str) -> Option<String> {
    (!seen.is_empty() && seen != skip && cmp_ver(seen, current) == Some(Ordering::Greater))
        .then(|| seen.to_string())
}

/// Whether this run shows "What's new": the build is newer than the last run and the notes of
/// exactly this version were cached. A fresh install (no last run) never does.
pub fn whats_new_due(ran: &str, current: &str, cached: Option<&str>) -> bool {
    !ran.is_empty()
        && cmp_ver(current, ran) == Some(Ordering::Greater)
        && cached.is_some_and(|c| cmp_ver(c, current) == Some(Ordering::Equal))
}

const HIGHLIGHT_MAX: usize = 90;

/// `[text](url)` keeps the text; emphasis and code marks go.
fn plain_md(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let link = tail
            .find("](")
            .and_then(|j| tail[j..].find(')').map(|k| (j, j + k)));
        match link {
            Some((j, k)) => {
                out.push_str(&tail[..j]);
                rest = &tail[k + 1..];
            }
            None => {
                out.push('[');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace(['`', '*'], "").trim().to_string()
}

/// One plain line for the toast: the first bullet of the notes, else their first text line;
/// a long line ends in an ellipsis.
pub fn highlight(summary: &str) -> String {
    let lines: Vec<&str> = summary.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let bullet = |l: &str| -> Option<String> {
        ["- ", "* ", "+ ", "\u{2022} "]
            .iter()
            .find_map(|m| l.strip_prefix(m))
            .map(str::to_string)
    };
    let line = lines
        .iter()
        .find_map(|l| bullet(l))
        .or_else(|| lines.iter().find(|l| !l.starts_with('#')).map(|l| l.to_string()))
        .unwrap_or_default();
    let plain = plain_md(&line);
    if plain.chars().count() > HIGHLIGHT_MAX {
        let cut: String = plain.chars().take(HIGHLIGHT_MAX - 1).collect();
        format!("{}\u{2026}", cut.trim_end())
    } else {
        plain
    }
}

/// Toast commands; the shell hands any id with this prefix to [`fire`].
pub const CMD_PREFIX: &str = "update.";
pub const CMD_NEWS: &str = "update.whats_new";
pub const CMD_LATER: &str = "update.later";
pub const CMD_SKIP: &str = "update.skip";

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ToastAct {
    News,
    Later,
    Skip,
}

pub fn toast_act(cmd: &str) -> Option<ToastAct> {
    match cmd {
        CMD_NEWS => Some(ToastAct::News),
        CMD_LATER => Some(ToastAct::Later),
        CMD_SKIP => Some(ToastAct::Skip),
        _ => None,
    }
}

/// The three choices of the "new version" toast, primary first.
pub fn toast_actions(c: &Catalog) -> Vec<ToastAction> {
    vec![
        ToastAction::new(c.t("update_check.toast.news"), CMD_NEWS, true),
        ToastAction::new(c.t("update_check.later"), CMD_LATER, false),
        ToastAction::new(c.t("update_check.skip"), CMD_SKIP, false),
    ]
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
    /// Newest release known (this run's check or the cache): what the pill opens.
    release: Option<Release>,
    /// Minimum version the last answer named, when this build is below it.
    floor: Option<String>,
    /// The after-update "What's new" dialog of this run.
    news: Option<Release>,
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
    release: None,
    floor: None,
    news: None,
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

/// Opens the dialog on the newest known release; without one it checks.
fn open_release(st: &mut State) {
    let Some(r) = st.release.clone() else {
        st.want_manual = true;
        return;
    };
    st.dialog = true;
    st.result = Some(Outcome {
        status: Status::Newer(r),
        unfit_below: st.floor.clone(),
    });
}

/// "Skip this version": no toast, no pill for `v` until a newer one comes.
fn skip_version(st: &mut State, v: &str) {
    let s = settings(st);
    s.skip = v.to_string();
    save_settings(s);
    st.dialog = false;
}

/// The pill's version, if any.
pub fn pending_version() -> Option<String> {
    with(|st| {
        let s = settings(st);
        pending(&s.seen, &s.skip, env!("CARGO_PKG_VERSION"))
    })
}

/// Runs a toast command (`CMD_PREFIX`); false for an id that is not ours.
pub fn fire(app: &mut EditorApp, cmd: &str) -> bool {
    let Some(a) = toast_act(cmd) else {
        return false;
    };
    match a {
        ToastAct::News => with(open_release),
        ToastAct::Later => {}
        ToastAct::Skip => with(|st| {
            let v = st.release.as_ref().map(|r| r.version.clone());
            let v = v.or_else(|| Some(settings(st).seen.clone()).filter(|s| !s.is_empty()));
            if let Some(v) = v {
                skip_version(st, &v);
            }
        }),
    }
    app.sh.toasts.dismiss_command(cmd);
    true
}

/// Pill geometry, every value a token on the 4-pt grid: a small control's height centred in the
/// tab strip, the dot one grid step from the label, the label starting on the grid.
pub mod pill {
    use ddc_ui::theme::{control, ribbon, space};
    pub const H: f32 = control::HIT_MIN;
    pub const INSET_Y: f32 = (ribbon::TAB_H - H) / 2.0;
    pub const PAD_START: f32 = space::SM;
    pub const DOT: f32 = space::SM;
    pub const GAP: f32 = space::XS;
    pub const TEXT_X: f32 = PAD_START + DOT + GAP;
    pub const PAD_END: f32 = space::MD;
    pub const RADIUS: f32 = H / 2.0;

    /// Width for a label `text_w` points wide, whole points so the edges land on pixels.
    pub fn width(text_w: f32) -> f32 {
        TEXT_X + text_w.ceil() + PAD_END
    }
}

/// One physical pixel in points: hairlines stay crisp at 1x, 1.25x, 1.5x and 2x.
fn hairline(ctx: &egui::Context) -> f32 {
    hairline_at(ctx.pixels_per_point())
}

pub fn hairline_at(ppp: f32) -> f32 {
    1.0 / ppp.max(0.5)
}

/// Fill and text colour of the pill: accent on its subtle tint, the theme's hover and press
/// overlays on top.
pub fn pill_colors(th: &Theme, hover: f32, pressed: bool) -> (egui::Color32, egui::Color32, egui::Color32) {
    let over = if pressed {
        th.pressed_fill()
    } else {
        th.hover_fill().gamma_multiply(hover.clamp(0.0, 1.0))
    };
    // the press overlay is the strongest: its text steps to the pressed accent to keep 4.5:1
    let fg = if pressed {
        th.c("accent.pressed")
    } else {
        th.c("accent").lerp_to_gamma(th.c("accent.hover"), hover.clamp(0.0, 1.0))
    };
    (th.c("accent.subtle"), over, fg)
}

/// Quiet pill "Update {v}" for the end of the tab row (or a rail): drawn only while a newer,
/// not skipped version is known. A click (or Enter/Space when focused) opens the release
/// notes; true when clicked. Place it in a row `ribbon::TAB_H` tall: it centres itself.
pub fn badge(ui: &mut egui::Ui, th: &Theme, cat: &Catalog) -> bool {
    let Some(v) = pending_version() else {
        return false;
    };
    let text = cat.tf("update_check.badge", &[("v", v.as_str())]);
    let g = ui
        .painter()
        .layout_no_wrap(text.clone(), th.font(Role::Label), egui::Color32::PLACEHOLDER);
    let (r, resp) = ui.allocate_exact_size(egui::vec2(pill::width(g.size().x), pill::H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
    // eases over the theme's 120 ms, which is zero with reduced motion
    let t = ui.ctx().animate_bool_with_time(
        resp.id.with("ri12.badge.hover"),
        resp.hovered() || resp.has_focus(),
        ui.style().animation_time,
    );
    let (bg, over, fg) = pill_colors(th, t, resp.is_pointer_button_down_on());
    let p = ui.painter();
    let r = p.round_rect_to_pixels(r);
    p.rect_filled(r, pill::RADIUS, bg);
    p.rect_filled(r, pill::RADIUS, over);
    // the dot leads the reading direction
    let (dot_c, text_x) = if ddc_ui::i18n::rtl() {
        (r.right() - pill::PAD_START - pill::DOT / 2.0, r.right() - pill::TEXT_X - g.size().x)
    } else {
        (r.left() + pill::PAD_START + pill::DOT / 2.0, r.left() + pill::TEXT_X)
    };
    p.circle_filled(p.round_pos_to_pixels(egui::pos2(dot_c, r.center().y)), pill::DOT / 2.0, fg);
    let at = p.round_pos_to_pixels(egui::pos2(text_x, r.center().y - g.size().y / 2.0));
    p.galley_with_override_text_color(at, g, fg);
    let clicked = resp
        .on_hover_text(cat.tf("update_check.badge.tip", &[("v", v.as_str())]))
        .clicked();
    if clicked {
        with(open_release);
    }
    clicked
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
    Skip(String),
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
                c.tf("update_check.date", &[("d", r.date.as_str())])
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

/// First pass of a run: picks the after-update notes and remembers this version.
fn boot(st: &mut State) {
    let cur = env!("CARGO_PKG_VERSION");
    let cached = load_release();
    let ran = settings(st).ran.clone();
    if whats_new_due(&ran, cur, cached.as_ref().map(|r| r.version.as_str())) {
        st.news = cached.clone();
    }
    st.release = cached.filter(|r| cmp_ver(&r.version, cur) == Some(Ordering::Greater));
    if ran != cur {
        let s = settings(st);
        s.ran = cur.to_string();
        save_settings(s);
    }
}

/// Release notes in a hairline box that scrolls past `NOTES_MAX_H`.
fn notes_box(ui: &mut egui::Ui, th: &Theme, cx: &crate::ai::ui::Cx, salt: &str, text: &str) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(hairline(ui.ctx()), th.c("border.subtle")))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt(salt)
                .max_height(NOTES_MAX_H)
                .show(ui, |ui| {
                    crate::ai::ui::md::answer(ui, cx, text, NOTES_SLOT);
                });
        });
}

/// Per frame: starts due checks, collects the answer, draws the update and "What's new" windows.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let cur = env!("CARGO_PKG_VERSION");
    let mut said: Vec<Note> = vec![];
    let mut known: Option<Release> = None;
    let (view, news) = with(|st| {
        if !st.started {
            boot(st);
        }
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
                st.floor = o.unfit_below.clone();
            }
            if let Status::Newer(r) = &o.status {
                save_release(r);
                st.release = Some(r.clone());
            }
            if st.job_manual {
                // the dialog announced it: the pill stays, no toast repeats it
                if let Status::Newer(r) = &o.status {
                    settings(st).seen = r.version.clone();
                }
                st.result = Some(o);
            } else {
                let (seen, skip) = {
                    let s = settings(st);
                    (s.seen.clone(), s.skip.clone())
                };
                said = notes(&o, &seen, st.unfit_told, cur, &skip);
                for n in &said {
                    match n {
                        Note::Newer(v) => settings(st).seen = v.clone(),
                        Note::Unfit { .. } => st.unfit_told = true,
                    }
                }
            }
            save_settings(settings(st));
        }
        known = st.release.clone();
        (
            st.dialog.then(|| (st.job.is_some(), st.result.clone())),
            st.news.clone(),
        )
    });
    for n in said {
        match n {
            Note::Newer(v) => {
                let c = &app.sh.cat;
                let title = c.tf("update_check.toast.title", &[("v", v.as_str())]);
                let line = known
                    .as_ref()
                    .filter(|r| r.version == v)
                    .map(|r| highlight(&r.summary))
                    .filter(|l| !l.is_empty());
                let acts = toast_actions(c);
                app.sh.toasts.push_choice(Level::Info, title, line, acts);
            }
            Note::Unfit { version, floor } => {
                let msg = app.sh.cat.tf(
                    "update_check.toast.unfit",
                    &[("v", version.as_str()), ("f", floor.as_str())],
                );
                app.sh.toasts.push(Level::Warning, msg, None);
            }
        }
    }
    if let Some(r) = news {
        news_dialog(app, ctx, &r);
    }
    let Some((busy, result)) = view else {
        return;
    };
    let mut act: Option<Act> = None;
    let mut skip_hit = false;
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
                    notes_box(ui, th, &cx, "ux50e.notes", &r.summary);
                }
                if let Some(f) = result.as_ref().and_then(|o| o.unfit_below.as_ref()) {
                    ui.add_space(space::MD);
                    let text = c.tf("update_check.unfit", &[("v", cur), ("f", f.as_str())]);
                    kit::banner::banner(ui, th, Tone::Warning, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .font(th.font(Role::Body))
                                    .color(th.c("text.primary")),
                            )
                            .wrap(),
                        );
                    });
                }
                ui.add_space(space::SM);
                match newer {
                    Some(r) if ph == Phase::Newer => {
                        kit::dialog_footer_split(
                            ui,
                            |ui| {
                                // tertiary: a quiet text button away from the primary
                                skip_hit = Button::quiet(&c.t("update_check.skip")).show(ui, th).clicked();
                            },
                            |ui| {
                                let dl = c.tf("update_check.up.download", &[("v", r.version.as_str())]);
                                if Button::primary(&dl).show(ui, th).clicked() {
                                    act = Some(Act::Open(r.url.clone()));
                                }
                                if Button::secondary(&c.t("update_check.later")).show(ui, th).clicked() {
                                    act = Some(Act::Close);
                                }
                            },
                        );
                    }
                    _ => {
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
                    }
                }
            });
    }
    if skip_hit {
        if let Some(Status::Newer(r)) = result.as_ref().map(|o| &o.status) {
            act = Some(Act::Skip(r.version.clone()));
        }
    }
    if !open {
        act = Some(Act::Close);
    }
    match act {
        Some(Act::Check) => with(|st| start(st, ctx, true)),
        Some(Act::Close) => with(|st| st.dialog = false),
        Some(Act::Skip(v)) => {
            with(|st| skip_version(st, &v));
            app.sh.toasts.dismiss_command(CMD_SKIP);
        }
        Some(Act::Open(u)) => {
            if open_url(&u).is_err() {
                let msg = app.sh.cat.t("update_check.open_failed");
                app.sh.toasts.push(Level::Warning, msg, None);
            }
        }
        None => {}
    }
}

/// Once after an update: this version's notes, calm, one button.
fn news_dialog(app: &mut EditorApp, ctx: &egui::Context, r: &Release) {
    let mut open = true;
    let mut done = false;
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
        let v = r.version.as_str();
        let title = c.tf("update_check.news.title", &[("v", v)]);
        kit::Dialog::new("ri12.news", title, kit::DialogSize::Form)
            .open(&mut open)
            .show(ctx, |ui| {
                let sub = if r.date.is_empty() {
                    c.tf("update_check.news.body", &[("v", v)])
                } else {
                    format!(
                        "{} {}",
                        c.tf("update_check.news.body", &[("v", v)]),
                        c.tf("update_check.date", &[("d", r.date.as_str())])
                    )
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(sub)
                            .font(th.font(Role::Body))
                            .color(th.c("text.secondary")),
                    )
                    .wrap(),
                );
                ui.add_space(space::MD);
                notes_box(ui, th, &cx, "ri12.news.notes", &r.summary);
                ui.add_space(space::SM);
                kit::dialog_footer(ui, |ui| {
                    if Button::primary(&c.t("update_check.news.done")).show(ui, th).clicked() {
                        done = true;
                    }
                });
            });
    }
    if !open || done {
        with(|st| st.news = None);
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
        let n = notes(&evaluate("1.2.3", BASE, &body("1.2.3", "1.3.0")), "", false, "1.2.3", "");
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
        assert!(notes(&cur, "", false, "1.2.3", "").is_empty());
        let down = unknown("offline");
        assert!(notes(&down, "", false, "1.2.3", "").is_empty());
        let new = evaluate("1.2.3", BASE, r#"{"version":"1.3.0"}"#);
        assert_eq!(notes(&new, "", false, "1.2.3", ""), vec![Note::Newer("1.3.0".into())]);
        assert!(notes(&new, "1.3.0", false, "1.2.3", "").is_empty());
    }

    #[test]
    fn settings_round_trip_and_default_off() {
        assert_eq!(Settings::default().freq, Freq::Never);
        let s = Settings {
            freq: Freq::Weekly,
            last: 77,
            seen: "1.3.0".into(),
            skip: "1.3.0".into(),
            ran: "1.2.3".into(),
        };
        assert_eq!(Settings::parse(&s.text()), s);
        assert_eq!(Settings::parse("freq=hourly\nlast=x\njunk").freq, Freq::Never);
        // a file written before skip/ran existed still loads
        let old = Settings::parse("freq=weekly\nlast=77\nseen=1.3.0\n");
        assert_eq!((old.freq, old.last, old.seen.as_str()), (Freq::Weekly, 77, "1.3.0"));
        assert!(old.skip.is_empty() && old.ran.is_empty());
    }

    #[test]
    fn skipped_version_is_not_announced_and_has_no_pill() {
        let new = evaluate("1.2.3", BASE, r#"{"version":"1.3.0"}"#);
        assert!(notes(&new, "", false, "1.2.3", "1.3.0").is_empty());
        let newer = evaluate("1.2.3", BASE, r#"{"version":"1.4.0"}"#);
        assert_eq!(notes(&newer, "1.3.0", false, "1.2.3", "1.3.0"), vec![Note::Newer("1.4.0".into())]);
        assert_eq!(pending("1.3.0", "", "1.2.3").as_deref(), Some("1.3.0"));
        assert_eq!(pending("1.3.0", "1.3.0", "1.2.3"), None);
        assert_eq!(pending("1.3.0", "", "1.3.0"), None, "gone once updated");
        assert_eq!(pending("", "", "1.2.3"), None);
    }

    #[test]
    fn whats_new_only_after_an_upgrade_with_cached_notes() {
        assert!(!whats_new_due("", "1.3.0", Some("1.3.0")), "fresh install");
        assert!(whats_new_due("1.2.3", "1.3.0", Some("1.3.0")), "upgrade");
        assert!(!whats_new_due("1.3.0", "1.3.0", Some("1.3.0")), "same version");
        assert!(!whats_new_due("1.2.3", "1.3.0", None), "no notes cached");
        assert!(!whats_new_due("1.2.3", "1.3.0", Some("1.4.0")), "notes of another version");
        assert!(!whats_new_due("1.4.0", "1.3.0", Some("1.3.0")), "downgrade");
    }

    #[test]
    fn cached_release_round_trips_and_stays_on_our_host() {
        let r = Release {
            version: "1.3.0".into(),
            date: "2026-11-01".into(),
            summary: "- Faster\n- Fixes".into(),
            url: format!("{}/r/1.3.0", ENDPOINT.trim_end_matches("/latest")),
        };
        assert_eq!(release_parse(&release_text(&r)), Some(r.clone()));
        let bad = Release { url: "https://evil.test/x".into(), ..r };
        assert_eq!(release_parse(&release_text(&bad)).unwrap().url, "");
        assert_eq!(release_parse("{}"), None);
        assert_eq!(release_parse("junk"), None);
    }

    #[test]
    fn toast_line_is_the_first_bullet_in_plain_text() {
        assert_eq!(highlight("## 1.3\n- **AI** in [45 languages](https://x)\n- b"), "AI in 45 languages");
        assert_eq!(highlight("Faster drawings.\n\n- later"), "later");
        assert_eq!(highlight("# Title\nFaster `drawings`."), "Faster drawings.");
        assert_eq!(highlight(""), "");
        let long = highlight(&format!("- {}", "word ".repeat(40)));
        assert!(long.chars().count() <= HIGHLIGHT_MAX && long.ends_with('\u{2026}'));
    }

    #[test]
    fn toast_actions_dispatch_to_their_choices() {
        for c in cats() {
            let a = toast_actions(&c);
            assert_eq!(a.iter().filter(|a| a.primary).count(), 1);
            assert!(a[0].primary, "What's new leads");
            let acts: Vec<_> = a.iter().map(|a| toast_act(&a.command)).collect();
            assert_eq!(acts, [Some(ToastAct::News), Some(ToastAct::Later), Some(ToastAct::Skip)]);
            for x in &a {
                assert!(x.command.starts_with(CMD_PREFIX));
                assert!(!x.label.is_empty() && !x.label.starts_with("update_check"), "{}", x.label);
            }
        }
        assert_eq!(toast_act("update.other"), None);
        assert_eq!(toast_act("edit.undo"), None);
    }

    fn on_grid(v: f32) -> bool {
        v > 0.0 && (v / ddc_ui::theme::space::XS).fract() == 0.0
    }

    /// `over` (premultiplied, as egui keeps it) composited on opaque `under`.
    fn over(under: egui::Color32, top: egui::Color32) -> egui::Color32 {
        let k = 1.0 - top.a() as f32 / 255.0;
        let ch = |u: u8, t: u8| (t as f32 + u as f32 * k).round().min(255.0) as u8;
        egui::Color32::from_rgb(ch(under.r(), top.r()), ch(under.g(), top.g()), ch(under.b(), top.b()))
    }

    #[test]
    fn pill_metrics_are_tokens_on_the_4pt_grid() {
        use ddc_ui::theme::{control, ribbon, space};
        for v in [pill::H, pill::INSET_Y, pill::PAD_START, pill::DOT, pill::GAP, pill::TEXT_X, pill::PAD_END] {
            assert!(on_grid(v), "{v} off the 4-pt grid");
        }
        assert_eq!(pill::H, control::HIT_MIN, "a full pointer target");
        assert_eq!(pill::INSET_Y * 2.0 + pill::H, ribbon::TAB_H, "centred in the tab strip");
        assert_eq!(pill::RADIUS * 2.0, pill::H, "fully round ends");
        assert_eq!(pill::TEXT_X, pill::PAD_START + pill::DOT + pill::GAP);
        assert_eq!(pill::width(41.3), pill::TEXT_X + 42.0 + pill::PAD_END, "whole points");
        assert!(pill::PAD_END > pill::PAD_START - space::XS, "text side not tighter than the dot side");
        assert_eq!(NOTES_MAX_H % space::XS, 0.0);
    }

    #[test]
    fn pill_reads_in_both_themes_at_rest_hover_and_press() {
        for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
            let th = Theme::new(mode, ddc_ui::Density::Compact);
            for (hover, pressed) in [(0.0, false), (1.0, false), (1.0, true)] {
                let (bg, top, fg) = pill_colors(&th, hover, pressed);
                let fill = over(bg, top);
                let r = ddc_ui::theme::contrast(fg, fill);
                assert!(r >= 4.5, "{mode:?} hover={hover} pressed={pressed}: {r:.2}");
                // the pill stands apart from the panel it sits on
                assert_ne!(fill, th.c("bg.panel"));
            }
        }
    }

    #[test]
    fn hairline_is_one_device_pixel() {
        for ppp in [1.0f32, 1.25, 1.5, 2.0] {
            assert!((hairline_at(ppp) * ppp - 1.0).abs() < 1e-6, "{ppp}");
        }
    }

    /// Light/dark at 1x and 2x into C:/tmp/update_shots (run with --ignored).
    #[cfg(feature = "gui")]
    #[test]
    #[ignore]
    fn shots_update() {
        let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
            .expect("graphics adapter");
        let rs = crate::shot::render_state(pick).unwrap();
        let dir = std::path::Path::new("C:/tmp/update_shots");
        std::fs::create_dir_all(dir).unwrap();
        let rel = Release {
            version: "0.12.0".into(),
            date: "2026-11-20".into(),
            summary: "- **AI** in 45 languages\n- Drawings in your language\n- Faster section views on large models\n- Fixes in IFC export".into(),
            url: String::new(),
        };
        for theme in ["light", "dark"] {
            for scale in [1.0f32, 2.0] {
                for scene in ["toast", "dialog", "news"] {
                    with(|st| {
                        st.started = true;
                        st.settings = Some(Settings { seen: "0.12.0".into(), ..Settings::default() });
                        st.release = Some(rel.clone());
                        st.dialog = scene == "dialog";
                        st.result = Some(Outcome { status: Status::Newer(rel.clone()), unfit_below: Some("0.11.0".into()) });
                        st.news = (scene == "news").then(|| rel.clone());
                    });
                    let ed = crate::editor::Editor::new_in_memory().unwrap();
                    let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
                    app.prefs.theme = theme.into();
                    if scene == "toast" {
                        let c = &app.sh.cat;
                        let title = c.tf("update_check.toast.title", &[("v", "0.12.0")]);
                        let acts = toast_actions(c);
                        app.sh.toasts.push_choice(Level::Info, title, Some(highlight(&rel.summary)), acts);
                    }
                    let ctx = egui::Context::default();
                    let size = ((1200.0 * scale) as u32, (720.0 * scale) as u32);
                    let px = crate::shot::frame(&rs, &ctx, size, scale, 8, None, |c, _| {
                        app.ui(c, Some(&rs));
                        // the shell owner places the pill at the tab row's end; shown here
                        egui::Area::new(egui::Id::new("shot.pill"))
                            .fixed_pos(egui::pos2(1200.0 - 260.0, pill::INSET_Y))
                            .order(egui::Order::Foreground)
                            .show(c, |ui| badge(ui, &app.sh.theme, &app.sh.cat));
                    });
                    let name = format!("{scene}_{theme}_{scale}x.png");
                    crate::shot::save_png(&dir.join(name), size, &px).unwrap();
                }
            }
        }
        with(|st| *st = State { settings: None, started: false, want_manual: false, job: None, job_manual: false, dialog: false, result: None, unfit_told: false, release: None, floor: None, news: None });
    }

    #[test]
    fn new_strings_exist_in_both_languages() {
        for c in cats() {
            for k in [
                "update_check.toast.title",
                "update_check.badge",
                "update_check.badge.tip",
                "update_check.up.download",
                "update_check.news.title",
                "update_check.news.body",
            ] {
                let t = c.tf(k, &[("v", "9.9.9")]);
                assert!(t.contains("9.9.9") && !t.contains('{'), "{k}: {t}");
            }
            for k in ["update_check.later", "update_check.skip", "update_check.news.done"] {
                assert_ne!(c.t(k), k);
            }
        }
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
            assert!(!c.t(primary_key(Primary::Retry)).contains("update_check"));
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
