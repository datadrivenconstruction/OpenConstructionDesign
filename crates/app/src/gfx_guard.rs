// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Startup crash memory. A native driver crash cannot be caught in-process, so a marker file is
//! written before any GPU call and removed after the first frames. A marker left behind means the
//! previous start died: this one moves one step along the recovery ladder (`gfx::recovery_ladder`)
//! and stays there until the user asks for a normal start.

use crate::gfx::{Answer, Buttons};
use ddc_model::ProjectLock;
use ddc_ui::i18n::{Catalog, Lang};
use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = "gfx_start.marker";
const STICKY: &str = "gfx_safe.txt";
const LOCK: &str = "gfx_start";
/// Frames drawn before the start counts as survived.
const OK_FRAMES: u32 = 5;
/// Early deaths in a row on the same rung before the next start steps down. One death is often
/// not the driver (killed from the task list, a crash elsewhere at start).
pub const STEP_AFTER: u32 = 2;
/// Survived starts on a recovery rung before the graphics card is tried again; doubles after
/// each failed try (up to 16 times).
pub const RETRY_OK: u32 = 5;

/// The rung after a crash. `chosen` is the ladder position of the back end in use, `None` when the
/// crash came while still probing, which is then charged to the rung the start began on.
pub fn next_rung(started: u32, chosen: Option<u32>, last: u32) -> u32 {
    (chosen.unwrap_or(started) + 1).min(last)
}

fn parse_marker(s: &str) -> Option<(u32, Option<u32>)> {
    let mut it = s.split_whitespace();
    let started = it.next()?.parse().ok()?;
    Some((started, it.next().and_then(|c| c.parse().ok())))
}

fn marker_text(started: u32, chosen: Option<u32>) -> String {
    match chosen {
        Some(c) => format!("{started} {c}\n"),
        None => format!("{started}\n"),
    }
}

/// The remembered start rung and why (`gfx_safe.txt`, `key=value` lines; an older file holds
/// the rung alone).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sticky {
    pub rung: u32,
    /// Early deaths in a row not yet charged to a rung.
    pub early: u32,
    /// Survived starts on this recovery rung since it was set or last tried.
    pub ok: u32,
    /// Failed tries of the graphics card while on this rung.
    pub fails: u32,
    /// Build and display driver (`start_key`) of the last step or failed try: a new one tries
    /// the graphics card again.
    pub key: String,
    pub reason: String,
}

impl Sticky {
    pub fn parse(s: &str) -> Self {
        let mut t = Self::default();
        if let Ok(r) = s.trim().parse::<u32>() {
            t.rung = r;
            return t;
        }
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let n = || v.trim().parse::<u32>().unwrap_or(0);
            match k.trim() {
                "rung" => t.rung = n(),
                "early" => t.early = n(),
                "ok" => t.ok = n(),
                "fails" => t.fails = n(),
                "key" => t.key = v.trim().to_string(),
                "reason" => t.reason = v.trim().to_string(),
                _ => {}
            }
        }
        t
    }

    pub fn text(&self) -> String {
        let one = |s: &str| s.replace(['\n', '\r'], " ");
        format!(
            "rung={}\nearly={}\nok={}\nfails={}\nkey={}\nreason={}\n",
            self.rung,
            self.early,
            self.ok,
            self.fails,
            one(&self.key),
            one(&self.reason)
        )
    }

    pub fn load(dir: &Path) -> Self {
        fs::read_to_string(dir.join(STICKY))
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    /// Writes the state; the normal start with nothing pending leaves no file.
    pub fn save(&self, dir: &Path) {
        if self.rung == 0 && self.early == 0 {
            let _ = fs::remove_file(dir.join(STICKY));
            return;
        }
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(dir.join(STICKY), self.text());
    }

    /// Survived starts before the next try of the graphics card.
    pub fn retry_after(&self) -> u32 {
        RETRY_OK << self.fails.min(4)
    }
}

/// What the start decided, for the startup log and the status chip.
#[derive(Clone, Debug, PartialEq)]
pub enum Why {
    Normal,
    /// One early death: the same rung once more before stepping.
    Repeat(u32),
    /// `STEP_AFTER` early deaths in a row on `from`: now on `to`.
    Stepped { from: u32, to: u32 },
    /// Staying on the remembered rung.
    Kept(u32),
    /// The graphics card is tried again: new build or driver, or enough good starts.
    Retry { from: u32, new_key: bool },
    /// The try of the graphics card died: back on `to`.
    RetryFailed { to: u32 },
}

impl Why {
    pub fn log_text(&self) -> String {
        match self {
            Why::Normal => "normal start".into(),
            Why::Repeat(r) => format!("previous start ended early; rung {r} once more"),
            Why::Stepped { from, to } => {
                format!("{STEP_AFTER} early ends in a row on rung {from}; now rung {to}")
            }
            Why::Kept(r) => format!("remembered recovery rung {r}"),
            Why::Retry { from, new_key } => format!(
                "trying the graphics card again from rung {from} ({})",
                if *new_key { "new build or driver" } else { "enough good starts" }
            ),
            Why::RetryFailed { to } => format!("the graphics card try ended early; rung {to}"),
        }
    }
}

/// Build and display driver of this start: when it differs from the remembered one, a recovery
/// start tries the graphics card again.
pub fn start_key() -> String {
    let exe = std::env::current_exe()
        .and_then(fs::metadata)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(web_time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let commit = option_env!("DDC_BUILD_COMMIT").unwrap_or("");
    let ver = env!("CARGO_PKG_VERSION");
    format!("{ver}+{commit}+{exe}|{}", display_driver().unwrap_or_default())
}

/// Display adapters and driver versions from the registry: no graphics call is made.
#[cfg(windows)]
fn display_driver() -> Option<String> {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: isize,
            sub: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut u16,
            len: *mut u32,
        ) -> i32;
    }
    const HKLM: isize = 0x8000_0002u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x2;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let read = |sub: &[u16], name: &str| {
        let mut buf = [0u16; 256];
        let mut len = (buf.len() * 2) as u32;
        let name = wide(name);
        // SAFETY: valid NUL-terminated strings, a buffer of `len` bytes
        let r = unsafe {
            RegGetValueW(
                HKLM,
                sub.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        (r == 0).then(|| {
            let n = (len as usize / 2).min(buf.len());
            String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string()
        })
    };
    let class = "SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let mut out = Vec::new();
    for i in 0..4 {
        let sub = wide(&format!("{class}\\{i:04}"));
        if let (Some(d), Some(v)) = (read(&sub, "DriverDesc"), read(&sub, "DriverVersion")) {
            out.push(format!("{d} {v}"));
        }
    }
    (!out.is_empty()).then(|| out.join("; "))
}

#[cfg(not(windows))]
fn display_driver() -> Option<String> {
    None
}

/// One guarded start. Holds an OS lock while starting: the OS frees it when the process dies, so a
/// marker with a held lock is a second instance starting now, not a crash.
pub struct Guard {
    dir: PathBuf,
    rung: u32,
    crashed: bool,
    lock: Option<ProjectLock>,
    frames: u32,
    sticky: Sticky,
    why: Why,
    /// This start tries the graphics card while the remembered rung is a recovery one.
    trial: bool,
}

impl Guard {
    /// Call before any GPU work. `last` is the ladder's last position.
    pub fn begin(dir: &Path, last: u32) -> Self {
        Self::begin_with(dir, last, &start_key())
    }

    /// `begin` with the build and driver key given (tests).
    pub fn begin_with(dir: &Path, last: u32, key: &str) -> Self {
        let _ = fs::create_dir_all(dir);
        let mut s = Sticky::load(dir);
        s.rung = s.rung.min(last);
        let mut g = Self {
            dir: dir.to_path_buf(),
            rung: s.rung,
            crashed: false,
            lock: None,
            frames: 0,
            why: if s.rung > 0 { Why::Kept(s.rung) } else { Why::Normal },
            sticky: s,
            trial: false,
        };
        let Ok(lock) = ProjectLock::acquire(&dir.join(LOCK)) else {
            return g;
        };
        g.lock = Some(lock);
        let s = &mut g.sticky;
        if let Ok(text) = fs::read_to_string(dir.join(MARKER)) {
            // an unreadable marker still means the start did not finish
            let (started, chosen) = parse_marker(&text).unwrap_or((s.rung, None));
            g.crashed = true;
            if started < s.rung {
                s.fails += 1;
                s.ok = 0;
                s.early = 0;
                s.key = key.to_string();
                g.why = Why::RetryFailed { to: s.rung };
            } else if s.early + 1 >= STEP_AFTER {
                let to = next_rung(started, chosen, last);
                *s = Sticky {
                    rung: to,
                    key: key.to_string(),
                    ..Sticky::default()
                };
                g.why = Why::Stepped { from: started, to };
            } else {
                s.early += 1;
                g.crashed = false;
                g.why = Why::Repeat(started);
            }
            s.reason = g.why.log_text();
            s.save(dir);
            g.rung = s.rung;
        }
        if g.rung > 0 && !g.crashed {
            let new_key = s.key != key;
            if new_key || s.ok >= s.retry_after() {
                g.trial = true;
                g.why = Why::Retry {
                    from: g.rung,
                    new_key,
                };
                g.rung = 0;
            }
        }
        let _ = fs::write(dir.join(MARKER), marker_text(g.rung, None));
        g
    }

    /// Why this start runs on its rung (for the startup log).
    pub fn why(&self) -> &Why {
        &self.why
    }

    /// The remembered state after this start's decision.
    pub fn sticky(&self) -> &Sticky {
        &self.sticky
    }

    /// Ladder start: 0 is the normal order.
    pub fn rung(&self) -> u32 {
        self.rung
    }

    /// The previous start died in the driver.
    pub fn crashed(&self) -> bool {
        self.crashed
    }

    /// The back end was chosen: a crash from here on is charged to it.
    pub fn chosen(&self, position: u32) {
        if self.lock.is_some() {
            let _ = fs::write(
                self.dir.join(MARKER),
                marker_text(self.rung, Some(position)),
            );
        }
    }

    /// One frame drawn; true once the start counts as survived.
    pub fn frame(&mut self) -> bool {
        self.frames += 1;
        if self.frames >= OK_FRAMES {
            self.finish(true);
            return true;
        }
        false
    }

    /// `survived`: enough frames came. Only the owner of the start lock records anything.
    fn finish(&mut self, survived: bool) {
        if self.lock.take().is_none() {
            return;
        }
        let _ = fs::remove_file(self.dir.join(MARKER));
        if !survived {
            return;
        }
        let s = &mut self.sticky;
        if self.trial {
            // the graphics card works again: the normal order from now on
            *s = Sticky::default();
        } else if s.rung > 0 {
            s.ok += 1;
            s.early = 0;
        } else {
            s.early = 0;
        }
        s.save(&self.dir);
    }
}

impl Drop for Guard {
    /// A normal return (closed early, window refused) is no crash. A panic keeps the marker: at
    /// this stage it most likely came from the graphics device.
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.finish(false);
        }
    }
}

/// Writes `rung` as chosen on purpose now: no try of the graphics card until the build or the
/// driver changes or enough starts survive.
fn set_sticky(dir: &Path, rung: u32, reason: &str) {
    Sticky {
        rung,
        key: start_key(),
        reason: reason.to_string(),
        ..Sticky::default()
    }
    .save(dir);
}

/// One step safer at the next start, at the user's request.
pub fn step_down(dir: &Path, last: u32) {
    let now = Sticky::load(dir).rung;
    set_sticky(dir, (now + 1).min(last), "asked for a safer start");
}

/// Back to the normal order at the next start.
pub fn reset(dir: &Path) {
    let _ = fs::remove_file(dir.join(STICKY));
}

/// The next start begins on `rung` (after a device loss).
pub fn set_rung(dir: &Path, rung: u32) {
    set_sticky(dir, rung, "the graphics device was lost");
}

/// Catalog key of a remembered reason, for the status chip (the log keeps the full text).
pub fn cause_key(reason: &str) -> &'static str {
    if reason.contains("asked") {
        "gfx.why.asked"
    } else if reason.contains("lost") {
        "gfx.why.lost"
    } else if reason.contains("early") {
        "gfx.why.early"
    } else {
        "gfx.chip.why_unknown"
    }
}

fn log_text(c: &Catalog, log: Option<&Path>) -> String {
    log.map(|p| p.display().to_string())
        .unwrap_or_else(|| c.t("gfx.fatal.no_log"))
}

/// Native box text when no adapter works, before any window exists: (title, body).
/// `can_safe`: a software start was not tried yet, so the box offers it.
pub fn fatal_gfx_message(
    lang: Lang,
    log: Option<&Path>,
    reason: &str,
    can_safe: bool,
) -> (String, String) {
    let c = Catalog::new(lang);
    let log = log_text(&c, log);
    let body = c.tf("gfx.fatal.body", &[("reason", reason), ("log", log.as_str())]);
    let choose = if can_safe {
        "gfx.fatal.choose_safe"
    } else {
        "gfx.fatal.choose_log"
    };
    (c.t("gfx.fatal.title"), format!("{body}\n\n{}", c.t(choose)))
}

/// What the user picked in the start failure box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FatalNext {
    SafeStart,
    OpenLog,
    Close,
}

/// Shows the start failure box through `show` (the native box in the app, a fake in tests).
pub fn fatal_start(
    lang: Lang,
    log: Option<&Path>,
    reason: &str,
    can_safe: bool,
    show: &mut dyn FnMut(&str, &str, Buttons) -> Answer,
) -> FatalNext {
    let (title, body) = fatal_gfx_message(lang, log, reason, can_safe);
    let buttons = if can_safe {
        Buttons::YesNoCancel
    } else {
        Buttons::YesNo
    };
    match (show(&title, &body, buttons), can_safe) {
        (Answer::Yes, true) => FatalNext::SafeStart,
        (Answer::No, true) | (Answer::Yes, false) => FatalNext::OpenLog,
        _ => FatalNext::Close,
    }
}

/// Relaunches after device losses in a row before the app stops and says so.
pub const MAX_RESETS: u32 = 2;

/// The running window's graphics: ladder rung it started on, ladder position of its back end,
/// the ladder's last position, device losses so far, and whether the start was guarded.
#[derive(Clone, Copy, Debug, Default)]
pub struct LossCtx {
    pub rung: u32,
    pub chosen: u32,
    pub last: u32,
    pub resets: u32,
    pub guarded: bool,
}

/// Rung of the relaunch after a device loss, `None` when losses keep coming.
pub fn plan_after_loss(c: &LossCtx) -> Option<u32> {
    if c.resets >= MAX_RESETS {
        return None;
    }
    // an explicit or safe choice of back end is kept: the relaunch repeats it
    Some(if c.guarded {
        next_rung(c.rung, Some(c.chosen), c.last)
    } else {
        c.rung
    })
}

/// The snapshot before a device-loss restart was not written while there is unsaved work.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapFail {
    pub why: String,
    /// The working copy the edits live in, if any.
    pub work: Option<PathBuf>,
}

/// Side effects of a device loss, faked in tests.
pub trait LossActions {
    /// Autosave snapshot of unsaved work, before anything else. Ok also when nothing is unsaved.
    fn save(&mut self) -> Result<(), SnapFail>;
    /// The native Save As dialog and a save there: the saved file, None when cancelled.
    fn save_as(&mut self) -> Result<Option<PathBuf>, String>;
    fn persist_rung(&mut self, rung: u32);
    /// Frees the project and session locks so the new process does not find them held.
    fn release_locks(&mut self);
    /// Start a new process (`resets` losses so far) that opens `open` when given (the Save As
    /// target); Ok means this one may exit now.
    fn relaunch(&mut self, resets: u32, open: Option<&Path>) -> Result<(), String>;
    fn show(&mut self, title: &str, body: &str);
    fn ask(&mut self, title: &str, body: &str, buttons: Buttons) -> Answer;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LossEnd {
    Relaunched(u32),
    Stopped,
    /// The snapshot failed and the user chose to close without a saved copy.
    Unsaved,
}

/// The snapshot failed: no restart until the work is saved somewhere or the user gives up.
/// The process stays in the native box meanwhile; nothing has to be drawn. None: the user gave
/// up; Some(Some(file)): saved there by Save As; Some(None): the snapshot was written.
fn save_until_done(
    reason: &str,
    lang: Lang,
    mut f: SnapFail,
    a: &mut dyn LossActions,
) -> Option<Option<PathBuf>> {
    loop {
        let (title, body) = unsaved_message(lang, reason, &f);
        match a.ask(&title, &body, Buttons::YesNoCancel) {
            Answer::Yes => match a.save() {
                Ok(()) => return Some(None),
                Err(e) => f = e,
            },
            Answer::No => match a.save_as() {
                Ok(Some(p)) => return Some(Some(p)),
                Ok(None) => {}
                Err(e) => f.why = e,
            },
            Answer::Cancel => return None,
        }
    }
}

/// Arguments of the relaunched process: this one's (without the program), with the file
/// arguments replaced by `open` when the work was saved there.
pub fn relaunch_args(argv: &[String], open: Option<&Path>) -> Vec<String> {
    let Some(open) = open else {
        return argv.iter().skip(1).cloned().collect();
    };
    let files = crate::shell::project_seam::file_args(argv);
    let mut out: Vec<String> = argv
        .iter()
        .skip(1)
        .filter(|s| !files.iter().any(|f| f.as_os_str() == s.as_str()))
        .cloned()
        .collect();
    out.push(open.display().to_string());
    out
}

/// Native box text when the snapshot after a device loss failed: (title, body). It never says
/// the work was kept.
pub fn unsaved_message(lang: Lang, reason: &str, f: &SnapFail) -> (String, String) {
    let c = Catalog::new(lang);
    let copy = f
        .work
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| c.t("gfx.lost.no_copy"));
    let args = [("reason", reason), ("why", f.why.as_str()), ("copy", copy.as_str())];
    let body = c.tf("gfx.lost.unsaved_body", &args);
    (c.t("gfx.lost.unsaved_title"), body)
}

/// Device lost mid-session: save first, then relaunch one step safer, or say why not. A failed
/// snapshot never relaunches silently: the user saves another way or closes knowingly.
pub fn handle_loss(
    c: &LossCtx,
    reason: &str,
    lang: Lang,
    log: Option<&Path>,
    a: &mut dyn LossActions,
) -> LossEnd {
    let mut saved_as = None;
    if let Err(f) = a.save() {
        match save_until_done(reason, lang, f, a) {
            Some(p) => saved_as = p,
            None => return LossEnd::Unsaved,
        }
    }
    let mut why = reason.to_string();
    if let Some(rung) = plan_after_loss(c) {
        if c.guarded {
            a.persist_rung(rung);
        }
        // this process writes nothing more: the child must not find the files still held
        a.release_locks();
        match a.relaunch(c.resets + 1, saved_as.as_deref()) {
            Ok(()) => return LossEnd::Relaunched(rung),
            Err(e) => why = format!("{reason}; restart failed: {e}"),
        }
    }
    let (title, body) = loss_message(lang, log, &why);
    a.show(&title, &body);
    LossEnd::Stopped
}

/// Native box text when the app stops after a device loss: (title, body).
pub fn loss_message(lang: Lang, log: Option<&Path>, reason: &str) -> (String, String) {
    let c = Catalog::new(lang);
    let log = log_text(&c, log);
    let body = c.tf("gfx.lost.body", &[("reason", reason), ("log", log.as_str())]);
    (c.t("gfx.lost.title"), body)
}

/// The real `LossActions::save`: snapshot through the crash path, which also marks the session
/// so the next start offers the snapshot. Ok(Some) is the snapshot written, Ok(None) means
/// nothing unsaved needed one; Err: unsaved work and no snapshot.
pub fn save_for_loss(a: &mut crate::shell::EditorApp) -> Result<Option<PathBuf>, SnapFail> {
    a.autosave.last_path = None;
    a.crash_flush();
    if let Some(p) = a.autosave.last_path.clone() {
        return Ok(Some(p));
    }
    if a.nothing_to_snapshot() || !a.ed.is_dirty() {
        return Ok(None);
    }
    let why = a
        .autosave_reason()
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| a.sh.cat.t("gfx.lost.no_folder"));
    Err(SnapFail {
        why,
        work: a.ed.project.work_path().map(Path::to_path_buf),
    })
}

/// Save As after a failed snapshot, through the native dialog (no drawing needed): the saved
/// file. Drawings that cannot be written fail it before the dialog, so the box stays up.
pub fn save_as_for_loss(a: &mut crate::shell::EditorApp) -> Result<Option<PathBuf>, String> {
    crate::drawing::store::begin_save(a)?;
    let Some(target) = crate::project_ui::pick_save_path(a) else {
        return Ok(None);
    };
    a.ed.save_project_as(&target).map_err(|e| e.to_string())?;
    crate::drawing::store::saved(a);
    Ok(Some(target))
}

/// Before the relaunch: the project file and the session instance lock go free.
pub fn release_for_handoff(a: &mut crate::shell::EditorApp) {
    a.ed.project.release_lock();
    a.autosave.instance = None;
}

/// Until this moment a busy lock may still be the parent's after a device-loss relaunch.
static HANDOFF_UNTIL: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
const HANDOFF_STEP: std::time::Duration = std::time::Duration::from_millis(100);

/// Started by a device-loss relaunch: busy locks are waited for up to `wait` from now.
pub fn expect_parent_locks(wait: std::time::Duration) {
    let _ = HANDOFF_UNTIL.set(web_time::Instant::now() + wait);
}

/// `ProjectLock::acquire`, which during the handoff window retries a busy lock: the parent
/// releases it before spawning, but a slow driver teardown may delay its exit.
pub fn acquire_lock(path: &Path) -> Result<ProjectLock, ddc_model::LockError> {
    let tries = HANDOFF_UNTIL.get().map_or(0, |u| {
        let left = u.saturating_duration_since(web_time::Instant::now());
        (left.as_millis() / HANDOFF_STEP.as_millis()) as u32
    });
    retry_lock(
        || ProjectLock::acquire(path),
        tries,
        &mut |()| std::thread::sleep(HANDOFF_STEP),
    )
}

/// Tries once, then up to `tries` more times while the lock is busy, `wait` between tries.
pub(crate) fn retry_lock<T>(
    mut acquire: impl FnMut() -> Result<T, ddc_model::LockError>,
    tries: u32,
    wait: &mut dyn FnMut(()),
) -> Result<T, ddc_model::LockError> {
    let mut left = tries;
    loop {
        match acquire() {
            Err(ddc_model::LockError::Busy) if left > 0 => {
                left -= 1;
                wait(());
            }
            r => return r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_guard_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// What a dead process leaves behind: the lock free, the marker in place.
    fn crash(g: Guard, dir: &Path, text: &str) {
        drop(g);
        fs::write(dir.join(MARKER), text).unwrap();
    }

    #[test]
    fn clean_start_stays_normal_and_clears_the_marker() {
        let d = temp("clean");
        let mut g = Guard::begin(&d, 3);
        assert_eq!((g.rung(), g.crashed()), (0, false));
        assert!(d.join(MARKER).exists());
        let mut done = false;
        for _ in 0..OK_FRAMES {
            done = g.frame();
        }
        assert!(done && !d.join(MARKER).exists());
        drop(g);
        assert_eq!(Guard::begin(&d, 3).rung(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn two_early_deaths_in_a_row_step_once_and_stick_until_reset() {
        let d = temp("crash");
        let g = Guard::begin(&d, 3);
        g.chosen(0);
        crash(g, &d, "0 0\n");
        // one early death: the same rung once more, nothing reported
        let g = Guard::begin(&d, 3);
        assert_eq!((g.rung(), g.crashed(), g.why()), (0, false, &Why::Repeat(0)));
        g.chosen(0);
        crash(g, &d, "0 0\n");
        let mut g = Guard::begin(&d, 3);
        assert_eq!((g.rung(), g.crashed()), (1, true));
        assert!(g.sticky().reason.contains("early"), "{:?}", g.sticky());
        while !g.frame() {}
        drop(g);
        // a survived recovery start stays on its rung and is not reported as a new crash
        let g = Guard::begin(&d, 3);
        assert_eq!((g.rung(), g.crashed()), (1, false));
        drop(g);
        reset(&d);
        assert_eq!(Guard::begin(&d, 3).rung(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn crash_while_probing_and_on_the_last_rung() {
        assert_eq!(next_rung(0, None, 3), 1);
        assert_eq!(next_rung(1, None, 3), 2);
        assert_eq!(next_rung(0, Some(1), 3), 2);
        assert_eq!(next_rung(3, Some(3), 3), 3);
        let d = temp("garbled");
        let g = Guard::begin(&d, 3);
        crash(g, &d, "not a number");
        let g = Guard::begin(&d, 3);
        assert_eq!(g.rung(), 0);
        crash(g, &d, "not a number");
        assert_eq!(Guard::begin(&d, 3).rung(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_survived_start_between_two_early_deaths_keeps_the_rung() {
        let d = temp("between");
        let g = Guard::begin(&d, 3);
        crash(g, &d, "0 0\n");
        let mut g = Guard::begin(&d, 3);
        while !g.frame() {}
        drop(g);
        assert!(!d.join(STICKY).exists(), "nothing pending is left behind");
        let g = Guard::begin(&d, 3);
        crash(g, &d, "0 0\n");
        assert_eq!(Guard::begin(&d, 3).rung(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    fn sticky(d: &Path, rung: u32, key: &str) {
        Sticky {
            rung,
            key: key.into(),
            ..Sticky::default()
        }
        .save(d);
    }

    #[test]
    fn a_new_build_or_driver_tries_the_card_and_a_good_try_goes_back_to_normal() {
        let d = temp("newkey");
        sticky(&d, 2, "b16|drv1");
        let mut g = Guard::begin_with(&d, 3, "b17|drv1");
        let why = Why::Retry {
            from: 2,
            new_key: true,
        };
        assert_eq!((g.rung(), g.crashed(), g.why()), (0, false, &why));
        while !g.frame() {}
        drop(g);
        assert!(!d.join(STICKY).exists());
        assert_eq!(Guard::begin_with(&d, 3, "b17|drv1").rung(), 0);
        // an older file holds the rung alone: it is tried again too
        fs::write(d.join(STICKY), "2").unwrap();
        assert_eq!(Guard::begin_with(&d, 3, "b17|drv1").rung(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_try_goes_back_and_waits_longer_for_the_next() {
        let d = temp("failtry");
        sticky(&d, 2, "b16|drv1");
        let g = Guard::begin_with(&d, 3, "b17|drv2");
        g.chosen(0);
        crash(g, &d, "0 0\n");
        let g = Guard::begin_with(&d, 3, "b17|drv2");
        assert_eq!(g.why(), &Why::RetryFailed { to: 2 });
        assert_eq!((g.rung(), g.crashed(), g.sticky().fails), (2, true, 1));
        drop(g);
        let wait = RETRY_OK << 1;
        for i in 0..wait {
            let mut g = Guard::begin_with(&d, 3, "b17|drv2");
            assert_eq!(g.rung(), 2, "start {i} stays on the recovery rung");
            while !g.frame() {}
        }
        let g = Guard::begin_with(&d, 3, "b17|drv2");
        assert_eq!((g.rung(), g.why()), (0, &Why::Retry { from: 2, new_key: false }));
        drop(g);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sticky_text_round_trips() {
        let s = Sticky {
            rung: 3,
            early: 1,
            ok: 4,
            fails: 2,
            key: "1.0+abc+5|GPU 31.0".into(),
            reason: "two\nlines".into(),
        };
        let back = Sticky::parse(&s.text());
        assert_eq!(back.reason, "two lines");
        assert_eq!((back.rung, back.early, back.ok, back.fails), (3, 1, 4, 2));
        assert_eq!(back.key, s.key);
        assert_eq!(Sticky::parse("2\n").rung, 2);
    }

    #[test]
    fn second_instance_starting_is_not_a_crash() {
        let d = temp("second");
        let first = Guard::begin(&d, 3);
        let second = Guard::begin(&d, 3);
        assert_eq!((second.rung(), second.crashed()), (0, false));
        drop(second);
        // the second one owns nothing: the first start's marker is still there
        assert!(d.join(MARKER).exists());
        drop(first);
        assert!(!d.join(MARKER).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn asking_for_a_safer_start_steps_down_and_stops_at_the_end() {
        let d = temp("step");
        step_down(&d, 1);
        assert_eq!(Guard::begin(&d, 1).rung(), 1);
        step_down(&d, 1);
        let g = Guard::begin(&d, 1);
        assert_eq!((g.rung(), g.crashed()), (1, false));
        drop(g);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn fatal_message_names_reason_log_and_next_steps_in_en_and_ru() {
        let log = Path::new("C:/data/logs/startup.log");
        let (te, be) = fatal_gfx_message(Lang::En, Some(log), "no adapter: none", true);
        let (tr, br) = fatal_gfx_message(Lang::Ru, Some(log), "no adapter: none", true);
        for (t, b) in [(&te, &be), (&tr, &br)] {
            assert!(!t.is_empty() && t.as_str() != "gfx.fatal.title");
            assert!(b.contains("no adapter: none") && b.contains("startup.log"));
            assert!(!b.contains("gfx.fatal.choose_safe") && !b.contains('{'));
        }
        assert_ne!(be, br);
        let c = Catalog::new(Lang::Ru);
        assert!(br.ends_with(&c.t_in(Lang::Ru, "gfx.fatal.choose_safe")));
        let (_, no_log) = fatal_gfx_message(Lang::En, None, "x", false);
        let c = Catalog::new(Lang::En);
        assert!(no_log.contains(&c.t("gfx.fatal.no_log")));
        assert!(no_log.ends_with(&c.t("gfx.fatal.choose_log")));
    }

    #[test]
    fn adapter_failure_shows_the_box_and_maps_the_answer() {
        let mut shown = Vec::new();
        let mut show = |t: &str, b: &str, k: Buttons| {
            shown.push((t.to_string(), b.to_string(), k));
            Answer::Yes
        };
        let log = Path::new("startup.log");
        assert_eq!(
            fatal_start(Lang::En, Some(log), "boom", true, &mut show),
            FatalNext::SafeStart
        );
        assert_eq!(
            fatal_start(Lang::En, Some(log), "boom", false, &mut show),
            FatalNext::OpenLog
        );
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].2, Buttons::YesNoCancel);
        assert_eq!(shown[1].2, Buttons::YesNo);
        assert!(shown[0].1.contains("boom"));
        let mut cancel = |_: &str, _: &str, _: Buttons| Answer::Cancel;
        assert_eq!(
            fatal_start(Lang::Ru, None, "boom", true, &mut cancel),
            FatalNext::Close
        );
        let mut no = |_: &str, _: &str, _: Buttons| Answer::No;
        assert_eq!(
            fatal_start(Lang::En, None, "boom", true, &mut no),
            FatalNext::OpenLog
        );
    }

    #[derive(Default)]
    struct Fake {
        steps: Vec<String>,
        fail_relaunch: bool,
        /// Snapshot failures still to come, in order.
        snap_fails: Vec<&'static str>,
        /// Answers of the failed-snapshot box, in order.
        answers: Vec<Answer>,
        save_as_ok: bool,
        boxes: Vec<String>,
        /// The file the relaunch was told to open.
        opened: Option<PathBuf>,
    }

    const SAVED_AS: &str = "D:/elsewhere/rescued.ddc";

    impl LossActions for Fake {
        fn save(&mut self) -> Result<(), SnapFail> {
            self.steps.push("save".into());
            if self.snap_fails.is_empty() {
                return Ok(());
            }
            Err(SnapFail {
                why: self.snap_fails.remove(0).into(),
                work: Some(PathBuf::from("C:/work/live-1.ddc")),
            })
        }
        fn save_as(&mut self) -> Result<Option<PathBuf>, String> {
            self.steps.push("save as".into());
            Ok(self.save_as_ok.then(|| PathBuf::from(SAVED_AS)))
        }
        fn persist_rung(&mut self, rung: u32) {
            self.steps.push(format!("rung {rung}"));
        }
        fn release_locks(&mut self) {
            self.steps.push("release".into());
        }
        fn ask(&mut self, _title: &str, body: &str, buttons: Buttons) -> Answer {
            assert_eq!(buttons, Buttons::YesNoCancel);
            self.steps.push("ask".into());
            self.boxes.push(body.to_string());
            if self.answers.is_empty() {
                Answer::Cancel
            } else {
                self.answers.remove(0)
            }
        }
        fn relaunch(&mut self, resets: u32, open: Option<&Path>) -> Result<(), String> {
            self.steps.push(format!("relaunch {resets}"));
            self.opened = open.map(Path::to_path_buf);
            if self.fail_relaunch {
                Err("spawn failed".into())
            } else {
                Ok(())
            }
        }
        fn show(&mut self, _title: &str, body: &str) {
            self.steps.push(format!("show {body}"));
        }
    }

    fn ctx(rung: u32, chosen: u32, resets: u32, guarded: bool) -> LossCtx {
        LossCtx {
            rung,
            chosen,
            last: 3,
            resets,
            guarded,
        }
    }

    #[test]
    fn loss_on_step_n_saves_first_then_relaunches_on_n_plus_one() {
        let mut f = Fake::default();
        let end = handle_loss(&ctx(1, 1, 0, true), "device lost", Lang::En, None, &mut f);
        assert_eq!(end, LossEnd::Relaunched(2));
        assert_eq!(f.steps, ["save", "rung 2", "release", "relaunch 1"]);
        // on the last step it stays there; an unguarded start keeps its own choice
        assert_eq!(plan_after_loss(&ctx(3, 3, 0, true)), Some(3));
        let mut f = Fake::default();
        handle_loss(&ctx(0, 0, 1, false), "lost", Lang::En, None, &mut f);
        assert_eq!(f.steps, ["save", "release", "relaunch 2"]);
    }

    #[test]
    fn repeated_loss_or_failed_relaunch_saves_and_says_why() {
        let mut f = Fake::default();
        let c = ctx(2, 2, MAX_RESETS, true);
        let log = Path::new("startup.log");
        let end = handle_loss(&c, "device lost: reset", Lang::Ru, Some(log), &mut f);
        assert_eq!(end, LossEnd::Stopped);
        assert_eq!(f.steps[0], "save");
        assert_eq!(f.steps.len(), 2);
        assert!(f.steps[1].contains("device lost: reset") && f.steps[1].contains("startup.log"));
        let mut f = Fake {
            fail_relaunch: true,
            ..Default::default()
        };
        assert_eq!(
            handle_loss(&ctx(0, 0, 0, true), "lost", Lang::En, None, &mut f),
            LossEnd::Stopped
        );
        assert_eq!(f.steps[..4], ["save", "rung 1", "release", "relaunch 1"]);
        assert!(f.steps[4].contains("spawn failed"));
        let (te, be) = loss_message(Lang::En, None, "r");
        let (tr, br) = loss_message(Lang::Ru, None, "r");
        assert!(te != tr && be != br && !be.contains('{'));
    }

    #[test]
    fn device_loss_writes_the_unsaved_work_to_a_snapshot() {
        let d = temp("loss_snap");
        let ed = crate::editor::Editor::start_project().unwrap();
        let mut a = crate::shell::EditorApp::new(ed, None, "test".into());
        a.prefs.autosave_dir = d.join("auto").display().to_string();
        let snap = save_for_loss(&mut a).unwrap().expect("snapshot written");
        assert!(snap.exists() && snap.starts_with(d.join("auto")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn handle_loss_failed_snapshot_shows_box_en_ru() {
        for lang in [Lang::En, Lang::Ru] {
            let mut f = Fake {
                snap_fails: vec!["disk full"],
                ..Default::default()
            };
            let end = handle_loss(&ctx(0, 0, 0, true), "lost", lang, None, &mut f);
            // closed from the box: no restart, no lock handed over, no "kept" box
            assert_eq!(end, LossEnd::Unsaved);
            assert_eq!(f.steps, ["save", "ask"]);
            let b = &f.boxes[0];
            assert!(b.contains("disk full") && b.contains("live-1.ddc") && !b.contains('{'));
            let kept = loss_message(lang, None, "lost").1;
            assert_ne!(*b, kept);
        }
        let f = SnapFail {
            why: "w".into(),
            work: None,
        };
        let (te, be) = unsaved_message(Lang::En, "r", &f);
        let (tr, br) = unsaved_message(Lang::Ru, "r", &f);
        assert!(te != tr && be != br);
        // honest in both languages: says NOT saved, never "kept"
        assert!(be.contains("NOT saved") && !be.contains("is kept"));
        assert!(br.contains("\u{41d}\u{415} \u{441}\u{43e}\u{445}\u{440}\u{430}\u{43d}"));
        let c = Catalog::new(Lang::En);
        assert!(be.contains(&c.t("gfx.lost.no_copy")));
    }

    #[test]
    fn failed_snapshot_then_retry_or_save_as_restarts() {
        let mut f = Fake {
            snap_fails: vec!["disk full", "still full"],
            answers: vec![Answer::Yes, Answer::Yes],
            ..Default::default()
        };
        let end = handle_loss(&ctx(0, 0, 0, true), "lost", Lang::En, None, &mut f);
        assert_eq!(end, LossEnd::Relaunched(1));
        assert_eq!(
            f.steps,
            ["save", "ask", "save", "ask", "save", "rung 1", "release", "relaunch 1"]
        );
        assert!(f.boxes[1].contains("still full"));
        let mut f = Fake {
            snap_fails: vec!["disk full"],
            answers: vec![Answer::No, Answer::No],
            ..Default::default()
        };
        // a cancelled dialog asks again; the second box is closed: nothing restarts
        assert_eq!(
            handle_loss(&ctx(0, 0, 0, true), "lost", Lang::En, None, &mut f),
            LossEnd::Unsaved
        );
        assert_eq!(f.steps, ["save", "ask", "save as", "ask", "save as", "ask"]);
        let mut f = Fake {
            snap_fails: vec!["disk full"],
            answers: vec![Answer::No],
            save_as_ok: true,
            ..Default::default()
        };
        let end = handle_loss(&ctx(0, 0, 0, false), "lost", Lang::En, None, &mut f);
        assert_eq!(end, LossEnd::Relaunched(0));
        assert_eq!(f.steps, ["save", "ask", "save as", "release", "relaunch 1"]);
    }

    #[test]
    fn relaunch_after_save_as_opens_the_saved_file() {
        let mut f = Fake {
            snap_fails: vec!["disk full"],
            answers: vec![Answer::No],
            save_as_ok: true,
            ..Default::default()
        };
        let end = handle_loss(&ctx(0, 0, 0, true), "lost", Lang::En, None, &mut f);
        assert_eq!(end, LossEnd::Relaunched(1));
        assert_eq!(f.opened.as_deref(), Some(Path::new(SAVED_AS)));
        // a written snapshot restarts on the same arguments
        let mut f = Fake::default();
        handle_loss(&ctx(0, 0, 0, true), "lost", Lang::En, None, &mut f);
        assert_eq!(f.opened, None);
        // the old file argument is replaced, flags and their values stay
        let old = ["ddc.exe", "--layout", "plan", "C:/old/house.ddc", "--safe-graphics"];
        let argv: Vec<String> = old.map(String::from).to_vec();
        let args = relaunch_args(&argv, Some(Path::new(SAVED_AS)));
        assert_eq!(args, ["--layout", "plan", "--safe-graphics", SAVED_AS]);
        let mut child = vec!["ddc.exe".to_string()];
        child.extend(args);
        let files = crate::shell::project_seam::file_args(&child);
        assert_eq!(files, [PathBuf::from(SAVED_AS)], "the new process opens the saved file");
        assert_eq!(relaunch_args(&argv, None), &argv[1..]);
    }

    #[test]
    fn save_as_drawings_failure_is_not_success() {
        let ed = crate::editor::Editor::start_project().unwrap();
        let mut a = crate::shell::EditorApp::new(ed, None, "test".into());
        crate::drawing::store::FAIL_WRITE.with(|c| c.set(true));
        // fails before any dialog: the failed-snapshot box asks again
        let r = save_as_for_loss(&mut a);
        crate::drawing::store::FAIL_WRITE.with(|c| c.set(false));
        let e = r.expect_err("drawings not written must not count as saved");
        assert!(!e.is_empty());
    }

    #[test]
    fn relaunch_waits_for_parent_lock() {
        use ddc_model::LockError;
        let mut calls = 0;
        let mut waits = 0;
        // the parent lets go after two busy tries
        let r = retry_lock(
            || {
                calls += 1;
                if calls <= 2 {
                    Err(LockError::Busy)
                } else {
                    Ok(calls)
                }
            },
            50,
            &mut |()| waits += 1,
        );
        assert!(matches!(r, Ok(3)));
        assert_eq!(waits, 2);
        // bounded: a lock that stays busy is reported busy after the last try
        let mut n = 0;
        let r = retry_lock(|| Err::<(), _>(LockError::Busy), 3, &mut |()| n += 1);
        assert!(matches!(r, Err(LockError::Busy)) && n == 3);
        // outside the handoff window a busy lock answers at once
        let mut m = 0;
        let r = retry_lock(|| Err::<(), _>(LockError::Busy), 0, &mut |()| m += 1);
        assert!(matches!(r, Err(LockError::Busy)) && m == 0);
    }

    #[test]
    fn handoff_frees_the_project_and_session_locks() {
        let d = temp("handoff");
        fs::create_dir_all(&d).unwrap();
        let ed = crate::editor::Editor::start_project().unwrap();
        let mut a = crate::shell::EditorApp::new(ed, None, "test".into());
        let p = d.join("held.lock");
        a.autosave.instance = Some(ProjectLock::acquire(&p).unwrap());
        assert!(matches!(
            ProjectLock::acquire(&p),
            Err(ddc_model::LockError::Busy)
        ));
        release_for_handoff(&mut a);
        assert!(a.autosave.instance.is_none());
        drop(ProjectLock::acquire(&p).unwrap());
        drop(a);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn marker_text_round_trips() {
        assert_eq!(parse_marker(&marker_text(2, Some(3))), Some((2, Some(3))));
        assert_eq!(parse_marker(&marker_text(1, None)), Some((1, None)));
        assert_eq!(parse_marker(""), None);
    }
}
