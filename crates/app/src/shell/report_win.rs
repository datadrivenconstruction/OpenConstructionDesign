// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Problem report: a form (what happened / expected / steps) and a local bundle with the text,
//! build and system facts, a window screenshot and the last crash record. The model is attached
//! only on an explicit tick, user paths are masked unless ticked; nothing is ever sent.
//! A crash of the previous run is announced once at the next start.

use crate::shell::EditorApp;
use crate::ui_kit::{self as kit, DialogSize};
use crate::ai::ui::kit::Button;
use ddc_ui::notify::Level;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::checkbox;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use web_time::{SystemTime, UNIX_EPOCH};

const SEEN: &str = "crash_announced.txt";
const CRASH_CAP: usize = 64 * 1024;
const SHOT_MAX_PX: usize = 1600;
const SHOT_WAIT_FRAMES: u32 = 30;
const MASK: &str = "<user>";
pub const COMMAND: &str = "app.report";

#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub what: String,
    pub expected: String,
    pub steps: String,
    pub with_shot: bool,
    pub with_model: bool,
    pub with_paths: bool,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            what: String::new(),
            expected: String::new(),
            steps: String::new(),
            with_shot: true,
            with_model: false,
            with_paths: false,
        }
    }
}

impl Form {
    /// Buttons stay off until the first field has text.
    pub fn ready(&self) -> bool {
        !self.what.trim().is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub version: String,
    pub os: String,
    pub gpu: String,
    pub graphics: String,
    pub lang: String,
}

#[derive(Debug, Default)]
pub struct Bundle {
    pub files: Vec<(String, Vec<u8>)>,
}

impl Bundle {
    pub fn has(&self, name: &str) -> bool {
        self.files.iter().any(|(n, _)| n == name)
    }
}

/// Masks the user-name segment of home-style paths: `\Users\<name>\`, `/Users/<name>/`, `/home/<name>/`.
pub fn scrub_paths(text: &str) -> String {
    let mut out = text.to_string();
    for marker in ["\\Users\\", "\\users\\", "/Users/", "/users/", "/home/"] {
        let mut from = 0;
        while let Some(i) = out[from..].find(marker) {
            let start = from + i + marker.len();
            let end = out[start..]
                .find(|c: char| matches!(c, '\\' | '/' | '"' | '\'' | '\r' | '\n' | ' ' | '<'))
                .map_or(out.len(), |e| start + e);
            if end > start && out[start..end] != *MASK {
                out.replace_range(start..end, MASK);
                from = start + MASK.len();
            } else {
                from = end;
            }
        }
    }
    out
}

const KEY_MASK: &str = "<key>";
const SECRET_NAMES: [&str; 6] = ["api_key", "apikey", "token", "secret", "password", "passwd"];
const SECRET_PREFIXES: [&str; 11] = [
    "sk-", "sk_", "pk-", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "AIza", "AKIA", "hf_",
];

fn trim_punct(w: &str) -> &str {
    w.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']'))
}

fn looks_secret(word: &str) -> bool {
    let w = trim_punct(word);
    if SECRET_PREFIXES
        .iter()
        .any(|p| w.starts_with(p) && w.len() >= p.len() + 8)
    {
        return true;
    }
    w.len() >= 32
        && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

fn scrub_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut mask_next = 0;
    for (i, part) in line.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let lower = part.to_ascii_lowercase();
        if mask_next > 0 && !part.is_empty() {
            mask_next -= 1;
            out.push_str(KEY_MASK);
            continue;
        }
        if lower == "bearer" {
            mask_next = 1;
        }
        if lower.trim_end_matches(':') == "authorization" {
            mask_next = 2;
        }
        if let Some(cut) = part.find(['=', ':']) {
            let name = lower[..cut].to_string();
            // the AI settings' credential-header rule too: one notion of a secret name
            let named = SECRET_NAMES.iter().any(|n| name.contains(n))
                || (!name.is_empty() && ddc_ai::profile::is_secret_header(&name));
            if named {
                out.push_str(&part[..=cut]);
                if part.len() > cut + 1 {
                    out.push_str(KEY_MASK);
                    // `Authorization:Bearer <token>`: the scheme was masked, the token is next
                    if matches!(&lower[cut + 1..], "bearer" | "basic") {
                        mask_next = mask_next.max(1);
                    }
                } else {
                    // keep the two words `Authorization:` asked for (scheme and token)
                    mask_next = mask_next.max(1);
                }
                continue;
            }
        }
        if looks_secret(part) {
            out.push_str(KEY_MASK);
        } else {
            out.push_str(part);
        }
    }
    out
}

/// Wipes every key this session made (the AI settings' scrub list), then masks API keys,
/// bearer tokens and `name=value` secrets.
pub fn scrub_secrets(text: &str) -> String {
    let known = ddc_ai::secret::scrub_known(text);
    known.split('\n').map(scrub_line).collect::<Vec<_>>().join("\n")
}

/// Keys and tokens never leave in a report (no option keeps them); paths only on request.
fn mask(form: &Form, s: &str) -> String {
    let s = scrub_secrets(s);
    if form.with_paths {
        s
    } else {
        scrub_paths(&s)
    }
}

/// Names of the files the bundle will hold, in order; mirrors `build`.
pub fn attach_files(form: &Form, shot_ready: bool, crash: bool) -> Vec<&'static str> {
    let mut v = vec!["report.txt"];
    if crash {
        v.push("crash.txt");
    }
    if form.with_shot && shot_ready {
        v.push("window.png");
    }
    if form.with_model {
        v.push("model.ddc");
    }
    v
}

/// Label of the model option; the size is shown when the project file is known.
pub fn attach_label(c: &Catalog, size: Option<u64>) -> String {
    match size {
        Some(b) => c.tf("update_check.rep.model.sized", &[("size", size_text(c, b).as_str())]),
        None => c.t("update_check.rep.model"),
    }
}

/// File size in the catalog's units and decimal mark: "12.4 MB" (English) or the Russian form with a decimal comma.
pub fn size_text(c: &Catalog, bytes: u64) -> String {
    const U: [&str; 4] = ["update_check.unit.b", "update_check.unit.kb", "update_check.unit.mb", "update_check.unit.gb"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    let n = if i == 0 {
        bytes.to_string()
    } else {
        let s = format!("{v:.1}");
        if c.lang == ddc_ui::Lang::Ru {
            s.replace('.', ",")
        } else {
            s
        }
    };
    format!("{n}\u{a0}{}", c.t(U[i]))
}

/// The report text; same masking rule as the bundle.
pub fn report_text(form: &Form, facts: &Facts, crash: Option<&str>) -> String {
    let mut t = String::new();
    let mut put = |h: &str, v: &str| {
        t.push_str(h);
        t.push_str(":\n");
        t.push_str(v.trim());
        t.push_str("\n\n");
    };
    put("What happened", &mask(form, &form.what));
    put("Expected", &mask(form, &form.expected));
    put("Steps", &mask(form, &form.steps));
    let sys = format!(
        "build: {}\nsystem: {}\ngpu: {}\ngraphics: {}\nlanguage: {}",
        facts.version, facts.os, facts.gpu, facts.graphics, facts.lang
    );
    put("Build and system", &sys);
    put(
        "Attached",
        &format!(
            "screenshot: {}\nmodel: {}\nfile paths: {}\ncrash record: {}",
            yn(form.with_shot),
            yn(form.with_model),
            yn(form.with_paths),
            yn(crash.is_some())
        ),
    );
    t
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn clip(s: &str) -> &str {
    if s.len() <= CRASH_CAP {
        return s;
    }
    let mut e = CRASH_CAP;
    while !s.is_char_boundary(e) {
        e -= 1;
    }
    &s[..e]
}

/// Files of the bundle. `shot` and `model` are used only when the form asks for them.
pub fn build(
    form: &Form,
    facts: &Facts,
    crash: Option<&str>,
    shot: Option<&[u8]>,
    model: Option<Vec<u8>>,
) -> Bundle {
    let mut b = Bundle::default();
    let crash = crash.map(|c| mask(form, clip(c)));
    b.files.push((
        "report.txt".into(),
        report_text(form, facts, crash.as_deref()).into_bytes(),
    ));
    if let Some(c) = crash {
        b.files.push(("crash.txt".into(), c.into_bytes()));
    }
    if let (true, Some(s)) = (form.with_shot, shot) {
        b.files.push(("window.png".into(), s.to_vec()));
    }
    if let (true, Some(m)) = (form.with_model, model) {
        b.files.push(("model.ddc".into(), m));
    }
    b
}

/// Writes the bundle into a new folder `<root>/report-<ms>`; nothing is left half written.
pub fn write_bundle(root: &Path, b: &Bundle) -> Result<PathBuf, String> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut dir = root.join(format!("report-{ms}"));
    // a name never reused: a failed write must remove only its own folder
    let mut n = 1;
    while dir.exists() {
        dir = root.join(format!("report-{ms}-{n}"));
        n += 1;
    }
    let res = (|| {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for (name, bytes) in &b.files {
            std::fs::write(dir.join(name), bytes).map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    })();
    match res {
        Ok(()) => Ok(dir),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// The newest crash record not yet announced. Marks it announced, so the next call is `None`.
pub fn crash_notice(folder: &Path) -> Option<PathBuf> {
    let note = super::recover_win::last_crash_note(folder)?;
    let name = note.file_name()?.to_string_lossy().to_string();
    let seen = folder.join(SEEN);
    if std::fs::read_to_string(&seen).is_ok_and(|s| s.trim() == name) {
        return None;
    }
    // a failed mark repeats the notice at the next start; hiding a crash would be worse
    let _ = std::fs::write(&seen, &name);
    Some(note)
}

/// Downscaled copy of an RGBA image whose longer side is at most `max` pixels.
pub(crate) fn fit(rgba: Vec<u8>, w: usize, h: usize, max: usize) -> (Vec<u8>, usize, usize) {
    let long = w.max(h);
    if long <= max || w == 0 || h == 0 {
        return (rgba, w, h);
    }
    let (nw, nh) = ((w * max / long).max(1), (h * max / long).max(1));
    let mut out = Vec::with_capacity(nw * nh * 4);
    for y in 0..nh {
        for x in 0..nw {
            let i = ((y * h / nh) * w + x * w / nw) * 4;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    (out, nw, nh)
}

pub(crate) fn encode_png(rgba: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(rgba).ok()?;
    }
    Some(out)
}

struct ReportShot;

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    /// Screenshot requested; frames waited so far.
    Capture(u32),
    Open,
}

#[derive(Default)]
struct State {
    stage: Stage,
    checked: bool,
    form: Form,
    shot: Option<Vec<u8>>,
    crash: Option<String>,
    error: Option<String>,
    /// Save was pressed with the required field empty: show the inline error.
    tried: bool,
    /// Size of the project file on disk, when it has one.
    model_size: Option<u64>,
}

const PREVIEW_MAX_H: f32 = 140.0;

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

pub fn register(app: &mut EditorApp) {
    app.register_command(COMMAND, |_| {
        ST.with(|s| {
            let mut s = s.borrow_mut();
            if matches!(s.stage, Stage::Idle) {
                s.stage = Stage::Capture(0);
            }
        })
    });
}

pub(super) fn facts(app: &EditorApp) -> Facts {
    let g = &app.view.gfx;
    Facts {
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        gpu: g.adapter.clone(),
        graphics: format!(
            "level {} tier {:?}{}",
            app.prefs.gfx_level,
            g.tier,
            if g.safe { " safe" } else { "" }
        ),
        lang: format!("{:?}", app.sh.cat.lang),
    }
}

fn reports_root(app: &EditorApp) -> PathBuf {
    app.prefs
        .autosave_folder()
        .map_or_else(std::env::temp_dir, |d| d.join("reports"))
}

fn take_shot(ctx: &egui::Context) -> Option<Vec<u8>> {
    let img = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot {
                user_data, image, ..
            } if user_data
                .data
                .as_ref()
                .is_some_and(|d| d.downcast_ref::<ReportShot>().is_some()) =>
            {
                Some(image.clone())
            }
            _ => None,
        })
    })?;
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    let (rgba, w, h) = fit(rgba, w, h, SHOT_MAX_PX);
    encode_png(&rgba, w, h)
}

enum Act {
    Save,
    Copy,
    Close,
}

pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if !st.checked {
        st.checked = true;
        if let Some(dir) = app.prefs.autosave_folder() {
            if crash_notice(&dir).is_some() {
                let c = &app.sh.cat;
                let (text, act) = (c.t("report.crash.notice"), c.t("report.crash.action"));
                app.sh.toasts.push_cmd(Level::Warning, text, act, COMMAND);
            }
        }
    }
    match st.stage {
        Stage::Idle => {}
        Stage::Capture(n) => {
            // the window opens only after the capture, so it is not in the picture
            if n == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                    ReportShot,
                )));
            }
            let got = take_shot(ctx);
            if got.is_some() || n >= SHOT_WAIT_FRAMES {
                st.shot = got;
                st.form = Form {
                    with_shot: st.shot.is_some(),
                    ..Form::default()
                };
                st.error = None;
                st.tried = false;
                st.model_size = app
                    .ed
                    .model
                    .db_path()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len());
                st.crash = app
                    .prefs
                    .autosave_folder()
                    .and_then(|d| super::recover_win::last_crash_note(&d))
                    .and_then(|p| std::fs::read_to_string(p).ok());
                st.stage = Stage::Open;
            } else {
                st.stage = Stage::Capture(n + 1);
            }
            ctx.request_repaint();
        }
        Stage::Open => match window(app, ctx, &mut st) {
            Some(Act::Close) => st.stage = Stage::Idle,
            Some(Act::Copy) => {
                let text = report_text(&st.form, &facts(app), st.crash.as_deref());
                ctx.copy_text(text);
                let msg = app.sh.cat.t("report.copied");
                app.sh.toasts.push(Level::Info, msg, None);
            }
            Some(Act::Save) => save(app, &mut st),
            None => {}
        },
    }
    ST.with(|s| *s.borrow_mut() = st);
}

fn save(app: &mut EditorApp, st: &mut State) {
    let root = reports_root(app);
    let model = if st.form.with_model {
        match model_copy(app) {
            Ok(b) => Some(b),
            Err(e) => {
                // asked for, not made: nothing is saved and the user is told
                st.error = Some(app.sh.cat.tf("report.model.fail", &[("err", e.as_str())]));
                return;
            }
        }
    } else {
        None
    };
    let b = build(
        &st.form,
        &facts(app),
        st.crash.as_deref(),
        st.shot.as_deref(),
        model,
    );
    match write_bundle(&root, &b) {
        Ok(dir) => {
            let p = dir.display().to_string();
            let msg = app.sh.cat.tf("report.saved", &[("path", p.as_str())]);
            app.sh.toasts.push(Level::Info, msg, None);
            st.stage = Stage::Idle;
        }
        Err(e) => st.error = Some(app.sh.cat.tf("report.failed", &[("err", e.as_str())])),
    }
}

/// A copy of the project without AI consent or journal, read back as bytes.
fn model_copy(app: &mut EditorApp) -> Result<Vec<u8>, String> {
    let tmp = std::env::temp_dir().join(format!("ddc-report-{}.ddc", std::process::id()));
    let r = app
        .ed
        .model
        .save_shared_copy(&tmp, false, &mut |_, _| Ok(()))
        .map_err(|e| e.to_string())
        .and_then(|()| std::fs::read(&tmp).map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(&tmp);
    r
}

/// The open window with every optional part (shot, crash, model, error), for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &EditorApp, ctx: &egui::Context) {
    let mut st = State {
        tried: true,
        shot: Some(vec![0]),
        crash: Some("crash".into()),
        model_size: Some(3 << 20),
        error: Some("error".into()),
        ..State::default()
    };
    let _ = window(app, ctx, &mut st);
}

fn window(app: &EditorApp, ctx: &egui::Context, st: &mut State) -> Option<Act> {
    let c = &app.sh.cat;
    let mut act = None;
    let mut open = true;
    let th: &Theme = &app.sh.theme;
    let preview = report_text(&st.form, &facts(app), st.crash.as_deref());
    let State {
        form,
        shot,
        error,
        tried,
        model_size,
        crash,
        ..
    } = st;
    kit::Dialog::new("report", c.t("report.title"), DialogSize::Form)
        .open(&mut open)
        .show(ctx, |ui| {
            let missing = *tried && !form.ready();
            let err = c.t("update_check.rep.what.err");
            field(ui, th, &c.t("report.what"), true, missing.then_some(err.as_str()), &mut form.what, 4);
            field(ui, th, &c.t("report.expected"), false, None, &mut form.expected, 2);
            field(ui, th, &c.t("report.steps"), false, None, &mut form.steps, 3);

            section(ui, th, &c.t("update_check.rep.attached"));
            for name in attach_files(form, shot.is_some(), crash.is_some()) {
                file_row(ui, th, name, &file_note(c, name));
            }
            ui.add_space(space::SM);
            section(ui, th, &c.t("update_check.rep.options"));
            ui.add_enabled_ui(shot.is_some(), |ui| {
                option(ui, th, &mut form.with_shot, &c.t("report.opt.shot"), &c.t("update_check.rep.shot.note"));
            });
            option(ui, th, &mut form.with_model, &attach_label(c, *model_size), &c.t("report.opt.model.note"));
            option(ui, th, &mut form.with_paths, &c.t("report.opt.paths"), &c.t("update_check.rep.paths.note"));
            caption(ui, th, &c.t("update_check.rep.keys.note"));

            ui.add_space(space::SM);
            egui::CollapsingHeader::new(c.t("update_check.rep.preview"))
                .id_salt("update_check.rep.preview")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(PREVIEW_MAX_H)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&preview)
                                        .font(th.font(Role::Code))
                                        .color(th.c("text.secondary")),
                                )
                                .wrap(),
                            );
                        });
                });
            caption(ui, th, &c.t("report.local"));
            if let Some(e) = error.as_deref() {
                kit::error_note(ui, e);
            }
            kit::dialog_footer(ui, |ui| {
                if Button::primary(&c.t("report.save")).show(ui, th).clicked() {
                    if form.ready() {
                        act = Some(Act::Save);
                    } else {
                        *tried = true;
                    }
                }
                let copy = Button::secondary(&c.t("report.copy"))
                    .enabled(form.ready())
                    .tooltip(&c.t("report.blocked"), "")
                    .show(ui, th);
                if copy.clicked() {
                    act = Some(Act::Copy);
                }
                if kit::cancel_button(ui, c.t("report.cancel")).clicked() {
                    act = Some(Act::Close);
                }
            });
        });
    if !open {
        act = Some(Act::Close);
    }
    act
}

fn file_note(c: &Catalog, name: &str) -> String {
    let key = match name {
        "crash.txt" => "update_check.rep.file.crash",
        "window.png" => "update_check.rep.file.shot",
        "model.ddc" => "update_check.rep.file.model",
        _ => "update_check.rep.file.report",
    };
    c.t(key)
}

fn section(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.add_space(space::XS);
    ui.label(
        RichText::new(text)
            .font(th.font(Role::BodyStrong))
            .color(th.c("text.primary")),
    );
    ui.add_space(space::XS);
}

fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        )
        .wrap(),
    );
}

/// One file of the bundle: monospace name, muted description.
fn file_row(ui: &mut egui::Ui, th: &Theme, name: &str, note: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(name)
                .font(th.font(Role::Code))
                .color(th.c("text.primary")),
        );
        ui.label(
            RichText::new(note)
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        );
    });
}

/// Checkbox with its description as a muted second line.
fn option(ui: &mut egui::Ui, th: &Theme, value: &mut bool, label: &str, note: &str) {
    checkbox(ui, th, value, label);
    ui.indent(("ux50e.rep.note", label), |ui| caption(ui, th, note));
    ui.add_space(space::XS);
}

/// Label with the required marker, the multiline input and an inline error under it.
fn field(
    ui: &mut egui::Ui,
    th: &Theme,
    label: &str,
    required: bool,
    error: Option<&str>,
    text: &mut String,
    rows: usize,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        ui.label(
            RichText::new(label)
                .font(th.font(Role::BodyStrong))
                .color(th.c("text.primary")),
        );
        if required {
            ui.label(
                RichText::new("*")
                    .font(th.font(Role::BodyStrong))
                    .color(th.c("danger")),
            );
        }
    });
    ui.add(
        egui::TextEdit::multiline(text)
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    );
    if let Some(e) = error {
        ui.add(
            egui::Label::new(
                RichText::new(e)
                    .font(th.font(Role::Caption))
                    .color(th.c("danger")),
            )
            .wrap(),
        );
    }
    ui.add_space(space::SM);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> Form {
        Form {
            what: "crash on open".into(),
            ..Form::default()
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-ri13-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bundle_excludes_model_by_default() {
        let f = filled();
        let b = build(&f, &Facts::default(), None, Some(b"png"), Some(vec![1, 2, 3]));
        assert!(b.has("report.txt") && b.has("window.png"));
        assert!(!b.has("model.ddc"));
        let on = Form {
            with_model: true,
            ..filled()
        };
        let b = build(&on, &Facts::default(), None, None, Some(vec![1]));
        assert!(b.has("model.ddc"));
    }

    #[test]
    fn buttons_wait_for_first_field() {
        assert!(!Form::default().ready());
        let blank = Form {
            what: "  \n".into(),
            ..Form::default()
        };
        assert!(!blank.ready());
        assert!(filled().ready());
    }

    #[test]
    fn paths_masked_unless_ticked() {
        let crash = "at C:\\Users\\Ivan\\work\\a.rs and /home/ivan/x";
        let b = build(&filled(), &Facts::default(), Some(crash), None, None);
        let c = String::from_utf8(b.files[1].1.clone()).unwrap();
        assert!(!c.contains("Ivan") && !c.contains("ivan"));
        assert!(c.contains("C:\\Users\\<user>\\work"));
        let on = Form {
            with_paths: true,
            ..filled()
        };
        let b = build(&on, &Facts::default(), Some(crash), None, None);
        assert!(String::from_utf8_lossy(&b.files[1].1).contains("Ivan"));
        assert_eq!(scrub_paths("/home/<user>/x"), "/home/<user>/x");
    }

    #[test]
    fn crash_notice_shown_once() {
        let d = tmp("notice");
        assert!(crash_notice(&d).is_none());
        std::fs::write(d.join("crash-1.txt"), "a").unwrap();
        assert!(crash_notice(&d).is_some());
        assert!(crash_notice(&d).is_none());
        std::fs::write(d.join("crash-2.txt"), "b").unwrap();
        assert!(crash_notice(&d).is_some());
        assert!(crash_notice(&d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn bundle_written_whole() {
        let d = tmp("write");
        let b = build(&filled(), &Facts::default(), Some("boom"), None, None);
        let dir = write_bundle(&d, &b).unwrap();
        assert!(dir.join("report.txt").is_file() && dir.join("crash.txt").is_file());
        let bad = Bundle {
            files: vec![("a\0b".into(), vec![1])],
        };
        assert!(write_bundle(&d, &bad).is_err());
        // the failed bundle left no folder behind
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn cats() -> [Catalog; 2] {
        [Catalog::new(ddc_ui::Lang::En), Catalog::new(ddc_ui::Lang::Ru)]
    }

    #[test]
    fn attachment_defaults_are_private() {
        let f = Form::default();
        assert!(f.with_shot && !f.with_model && !f.with_paths);
        assert_eq!(attach_files(&f, true, false), vec!["report.txt", "window.png"]);
        assert_eq!(attach_files(&f, false, true), vec!["report.txt", "crash.txt"]);
    }

    #[test]
    fn attachment_toggles_change_the_list() {
        let mut f = Form::default();
        f.with_model = true;
        assert_eq!(attach_files(&f, true, false).last(), Some(&"model.ddc"));
        f.with_model = false;
        f.with_shot = false;
        assert_eq!(attach_files(&f, true, true), vec!["report.txt", "crash.txt"]);
    }

    #[test]
    fn attachment_list_matches_the_bundle() {
        for (model, shot, crash) in [(false, true, false), (true, true, true), (true, false, true)] {
            let f = Form {
                with_model: model,
                with_shot: shot,
                ..filled()
            };
            let c = crash.then_some("boom");
            let b = build(&f, &Facts::default(), c, Some(b"png"), Some(vec![1]));
            let got: Vec<&str> = b.files.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(got, attach_files(&f, true, crash));
        }
    }

    #[test]
    fn model_option_shows_its_size() {
        for c in cats() {
            let plain = attach_label(&c, None);
            let sized = attach_label(&c, Some(13_000_000));
            assert!(!plain.starts_with("update_check.") && !sized.starts_with("update_check."));
            assert!(!plain.contains(char::is_numeric));
        }
        assert!(attach_label(&cats()[0], Some(13_000_000)).contains("12.4\u{a0}MB"));
        assert!(attach_label(&cats()[1], Some(13_000_000)).contains("12,4\u{a0}"));
        assert_eq!(size_text(&cats()[0], 512), "512\u{a0}B");
        assert!(attach_label(&cats()[0], Some(13_000_000)).contains("Attach model"));
    }

    #[test]
    fn keys_and_paths_are_redacted_in_the_preview() {
        let f = Form {
            what: "failed with api_key=sk-abcdef123456 at C:\\Users\\Ivan\\a.rs\nAuthorization: Bearer abcdefgh1234".into(),
            expected: "token: ghp_0123456789abcdef and 0123456789abcdef0123456789abcdef0123".into(),
            ..Form::default()
        };
        let t = report_text(&f, &Facts::default(), None);
        for leak in ["sk-abcdef", "Ivan", "abcdefgh1234", "ghp_0123", "0123456789abcdef0123"] {
            assert!(!t.contains(leak), "leaked {leak}");
        }
        assert!(t.contains("<key>") && t.contains("<user>"));
        // paths on request; keys have no switch and stay masked
        let on = Form {
            with_paths: true,
            ..f
        };
        let t = report_text(&on, &Facts::default(), None);
        assert!(t.contains("Ivan") && !t.contains("sk-abcdef") && !t.contains("abcdefgh1234"));
    }

    #[test]
    fn a_key_made_this_session_is_wiped_even_without_a_key_like_shape() {
        let _k = ddc_ai::Secret::new("plainword-session-q7");
        let out = scrub_secrets("the server said plainword-session-q7 is wrong");
        assert!(!out.contains("plainword-session-q7"), "{out}");
        assert!(scrub_secrets("x-api-key: abc123").ends_with("<key>"));
    }

    #[test]
    fn a_dotted_token_after_a_glued_scheme_is_masked() {
        for line in ["Authorization:Bearer aa.bb.cc", "Authorization: Bearer aa.bb.cc"] {
            let out = scrub_secrets(line);
            assert!(!out.contains("aa.bb") && !out.contains("Bearer"), "{out}");
        }
    }

    #[test]
    fn plain_text_is_not_mangled() {
        let s = "Wall join fails: the corner at level 2 is wrong";
        assert_eq!(scrub_secrets(s), s);
    }

    #[test]
    fn required_error_and_option_text_exist_in_both_languages() {
        for c in cats() {
            for k in [
                "update_check.rep.what.err",
                "update_check.rep.attached",
                "update_check.rep.options",
                "update_check.rep.preview",
                "update_check.rep.keys.note",
                "update_check.unit.mb",
                "update_check.rep.paths.note",
                "update_check.rep.shot.note",
                "update_check.rep.file.report",
                "update_check.rep.file.crash",
                "update_check.rep.file.shot",
                "update_check.rep.file.model",
            ] {
                assert_ne!(c.t(k), k, "missing {k}");
            }
        }
    }

    #[test]
    fn dialog_uses_kit_only() {
        let src = include_str!("report_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for bad in [
            "ui.checkbox(",
            "egui::Checkbox",
            "egui::Button::new",
            "add_space(1",
            "add_space(2",
            "add_space(4",
            "add_space(6",
            "add_space(8",
            "set_width(420",
        ] {
            assert!(!code.contains(bad), "raw widget left: {bad}");
        }
    }

    #[test]
    fn shot_is_downscaled() {
        let (px, w, h) = fit(vec![0; 4000 * 2000 * 4], 4000, 2000, 1600);
        assert_eq!((w, h), (1600, 800));
        assert_eq!(px.len(), w * h * 4);
    }
}
