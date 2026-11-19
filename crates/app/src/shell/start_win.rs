//! Start screen: recent projects, new from a start template, open, sample, drop import and
//! what is new. Also the window-wide file drop: overlay while hovering, routing on release.
use super::project_seam::{self, RecentProject};
use super::recover_win::fmt_time;
use crate::ui_kit::fmt_size;
use super::EditorApp;
use crate::recent_probe::{RecentProbe, State};
use crate::units::Units;
use ddc_ui::help::{Block, Inline};
use ddc_ui::notify::Level;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, shell_kit};
use ddc_ui::{Lang, Theme};
use egui::{self, RichText, Sense, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use web_time::{Duration, Instant};

/// Decoded thumbnail (or `None`: unreadable) from the worker, by file.
type ThumbMsg = (PathBuf, Option<egui::ColorImage>);

/// Poll interval while some recent file is still being looked at off the UI thread.
const PROBE_POLL: Duration = Duration::from_millis(100);

const CARD_W: f32 = 196.0;
const CARD_H: f32 = 164.0;
const THUMB_H: f32 = 108.0;
const NEWS_MAX: usize = 5;

/// Units chosen last on the start screen (`Prefs::start_units`); metric until chosen.
fn units_of_pref(s: &str) -> Units {
    if s == "imperial" {
        Units::Imperial
    } else {
        Units::Metric
    }
}

fn pref_of_units(u: Units) -> &'static str {
    match u {
        Units::Metric => "metric",
        Units::Imperial => "imperial",
    }
}

fn units_index(u: Units) -> usize {
    usize::from(u == Units::Imperial)
}

#[derive(Default)]
pub struct StartUi {
    pub open: bool,
    /// Model revision at opening: any change means a project is in, the screen closes.
    rev0: u64,
    /// Read once per opening: it touches the disk for every file.
    recent: Option<Vec<RecentProject>>,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// Thumbnails being decoded on workers.
    thumb_busy: HashSet<PathBuf>,
    thumb_chan: Option<(Sender<ThumbMsg>, Receiver<ThumbMsg>)>,
    /// What the disk says about each recent file; asked on workers, kept across openings.
    probe: RecentProbe,
    news: Option<(Lang, String, Vec<String>)>,
    /// Units of a new project still waiting for the answer about unsaved work.
    pub pending_units: Option<Units>,
    /// File from the command line, opened as soon as no dialog is up.
    pub arg_file: Option<PathBuf>,
    /// Last removed recent entry, kept for the Undo action of its toast.
    forgot: Option<Forgot>,
    /// Text of the "Describe the project" card, handed to the assistant.
    pub ask: String,
}

/// A recent entry taken off both lists, with the place it had in each.
#[derive(Debug, Clone, PartialEq)]
struct Forgot {
    key: String,
    in_recent: Option<usize>,
    in_projects: Option<usize>,
}

/// Command id the Undo action of the "removed" toast carries; handled here, not registered.
const FORGET_UNDO: &str = "start.forget_undo";

/// Removes `key` from `list`; returns where it was.
fn take_entry(list: &mut Vec<String>, key: &str) -> Option<usize> {
    let at = list.iter().position(|r| r == key)?;
    list.retain(|r| r != key);
    Some(at)
}

/// Puts `key` back at `at` (or the end when the list got shorter); no duplicates.
fn put_entry(list: &mut Vec<String>, key: &str, at: usize) {
    list.retain(|r| r != key);
    list.insert(at.min(list.len()), key.to_owned());
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Nav {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Keys the recent grid reacts to.
const GRID_KEYS: [egui::Key; 8] = [
    egui::Key::ArrowLeft,
    egui::Key::ArrowRight,
    egui::Key::ArrowUp,
    egui::Key::ArrowDown,
    egui::Key::Home,
    egui::Key::End,
    egui::Key::Delete,
    egui::Key::Backspace,
];

fn nav_of(k: egui::Key) -> Option<Nav> {
    Some(match k {
        egui::Key::ArrowLeft => Nav::Left,
        egui::Key::ArrowRight => Nav::Right,
        egui::Key::ArrowUp => Nav::Up,
        egui::Key::ArrowDown => Nav::Down,
        egui::Key::Home => Nav::Home,
        egui::Key::End => Nav::End,
        _ => return None,
    })
}

/// Card that a key moves to. Left/Right walk the reading order and wrap; Up/Down keep the column
/// and wrap top to bottom; under a short last row Down lands on the last card.
fn grid_nav(idx: usize, cols: usize, n: usize, nav: Nav) -> usize {
    if n == 0 {
        return 0;
    }
    let cols = cols.max(1);
    let i = idx.min(n - 1);
    match nav {
        Nav::Home => 0,
        Nav::End => n - 1,
        Nav::Right => (i + 1) % n,
        Nav::Left => (i + n - 1) % n,
        Nav::Down => {
            if i + cols < n {
                i + cols
            } else if i / cols < (n - 1) / cols {
                n - 1
            } else {
                i % cols
            }
        }
        Nav::Up => {
            if i >= cols {
                i - cols
            } else {
                let last_row = (n - 1) / cols;
                let col = i % cols;
                if last_row * cols + col < n {
                    last_row * cols + col
                } else {
                    // the last row is short: the nearest full row keeps the column
                    last_row.saturating_sub(1) * cols + col
                }
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum RecentKey {
    Focus(usize),
    Forget(usize),
}

/// What a key does on the recent grid. Nothing while a text field has focus, and nothing for a
/// card that is not there. Enter is left to the focused card itself (a click).
fn recent_key(
    key: egui::Key,
    focus: Option<usize>,
    cols: usize,
    n: usize,
    typing: bool,
) -> Option<RecentKey> {
    if typing || n == 0 {
        return None;
    }
    let focus = focus.filter(|i| *i < n);
    match key {
        egui::Key::Delete | egui::Key::Backspace => focus.map(RecentKey::Forget),
        k => {
            let nav = nav_of(k)?;
            match focus {
                Some(i) => Some(RecentKey::Focus(grid_nav(i, cols, n, nav))),
                // nothing focused yet: forward keys enter the grid
                None => match nav {
                    Nav::Down | Nav::Right | Nav::Home => Some(RecentKey::Focus(0)),
                    Nav::End => Some(RecentKey::Focus(n - 1)),
                    _ => None,
                },
            }
        }
    }
}

fn card_id(i: usize) -> egui::Id {
    egui::Id::new(("ddc.start.card", i))
}

fn focused_card(focused: Option<egui::Id>, n: usize) -> Option<usize> {
    let f = focused?;
    (0..n).find(|i| card_id(*i) == f)
}

/// Context-menu row: label left, shortcut right, kit hover fill.
fn menu_row(
    ui: &mut egui::Ui,
    th: &Theme,
    label: &str,
    keys: &str,
    enabled: bool,
) -> egui::Response {
    let w = ui.available_width().max(ddc_ui::sem::control::DROPDOWN_W);
    let h = th.control_h(ddc_ui::sem::ControlSize::Medium);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled && resp.hovered() {
        ui.painter()
            .rect_filled(rect, th.radius("sm"), th.hover_fill());
    }
    let (ink, dim) = if enabled {
        (th.c("text.primary"), th.c("text.secondary"))
    } else {
        (th.c("text.disabled"), th.c("text.disabled"))
    };
    let p = ui.painter();
    p.text(
        egui::pos2(rect.left() + space::SM, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        th.font(Role::Body),
        ink,
    );
    if !keys.is_empty() {
        p.text(
            egui::pos2(rect.right() - space::SM, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            keys,
            th.font(Role::Caption),
            dim,
        );
    }
    resp
}

enum Act {
    New(Option<Units>),
    Tutorial,
    OpenDialog,
    Sample,
    Open(PathBuf),
    Reveal(PathBuf),
    Forget(PathBuf),
    /// A recent project that is gone: pick where it is now.
    Locate(PathBuf),
    /// A recent project on a share or drive that did not answer: ask again.
    Retry(PathBuf),
    Skip,
    Run(&'static str),
    /// The assistant with this text (empty: just open it).
    Ask(String),
}

/// How a recent card is drawn and what a click does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// No answer from the disk yet: skeleton.
    Loading,
    Ready,
    /// The file is gone from a folder that answers: Locate / Forget.
    Missing,
    /// The place does not answer: "Not available now", Retry. Never removed on its own.
    Unavailable,
}

fn card_kind(st: Option<&State>) -> Kind {
    match st {
        None => Kind::Loading,
        Some(State::Present { .. }) => Kind::Ready,
        Some(State::Missing) => Kind::Missing,
        Some(State::Unavailable) => Kind::Unavailable,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Click {
    Nothing,
    Open,
    Locate,
    Retry,
}

fn click_of(k: Kind) -> Click {
    match k {
        Kind::Loading => Click::Nothing,
        Kind::Ready => Click::Open,
        Kind::Missing => Click::Locate,
        Kind::Unavailable => Click::Retry,
    }
}

/// Decodes `file` on a worker; the answer comes through `tx`. An unreadable file or a crashed
/// decoder gives `None`, so the card keeps its type icon.
fn decode_in_background(file: &Path, tx: &Sender<ThumbMsg>) -> bool {
    let (f, t) = (file.to_path_buf(), tx.clone());
    let (f2, t2) = (file.to_path_buf(), tx.clone());
    crate::jobs::spawn_guarded(
        "start-thumb",
        move || {
            let img = load_png(&f);
            let _ = t.send((f, img));
        },
        move |_| {
            let _ = t2.send((f2, None));
        },
    )
    .is_ok()
}

/// Text of a help line, commands by their label, cross-references left out.
fn inline_text(parts: &[Inline], label: &dyn Fn(&str) -> String) -> String {
    let mut s = String::new();
    for p in parts {
        match p {
            Inline::Text(t) | Inline::Bold(t) | Inline::Key(t) => s += t,
            Inline::Cmd(id) => s += &label(id),
            Inline::Topic(_) => {}
        }
    }
    s.trim().to_string()
}

/// First section of "what is new": its heading and bullets.
fn latest_news(
    blocks: &[Block],
    max: usize,
    label: &dyn Fn(&str) -> String,
) -> (String, Vec<String>) {
    let mut head = String::new();
    let mut out = vec![];
    for b in blocks {
        match b {
            Block::Heading(h) => {
                if !head.is_empty() {
                    break;
                }
                head = h.clone();
            }
            Block::Bullet(v) | Block::Para(v) if out.len() < max => {
                let t = inline_text(v, label);
                if !t.is_empty() {
                    out.push(t);
                }
            }
            _ => {}
        }
    }
    (head, out)
}

fn load_png(path: &Path) -> Option<egui::ColorImage> {
    let f = std::fs::File::open(path).ok()?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(f));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0u8; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0 || h == 0 || w.max(h) > 4096 {
        return None;
    }
    let px = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => px
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

/// Largest rectangle of `size` proportions centered inside `area`.
pub fn fit_rect(area: egui::Rect, size: Vec2) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return area;
    }
    let k = (area.width() / size.x).min(area.height() / size.y);
    egui::Rect::from_center_size(area.center(), size * k)
}

fn ext_upper(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_default()
}

/// Icon of a project without a preview: what the file type stands for.
fn ext_icon(p: &Path) -> &'static str {
    match ext_upper(p).as_str() {
        "DWG" | "DXF" => "import-cad",
        "PDF" => "import-pdf",
        "XLSX" => "xlsx-in",
        "DDC" => "open",
        _ => "import",
    }
}

fn reveal_in_folder(path: &Path) {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
    if let Err(e) = r {
        eprintln!("show in folder: {e}");
    }
}

impl StartUi {
    /// The text of "what is new" follows the language.
    pub fn drop_news(&mut self) {
        self.news = None;
    }
}

impl EditorApp {
    pub fn open_start(&mut self) {
        self.chrome.start.open = true;
        self.chrome.start.rev0 = self.ed.revision;
        self.chrome.start.recent = None;
    }

    /// Program start: a file on the command line opens and skips the start screen.
    pub fn open_at_start(&mut self, argv: &[String]) {
        let mut files = project_seam::file_args(argv).into_iter();
        if let Some(p) = files.next() {
            self.recovery_to_banner(&p);
            self.chrome.start.arg_file = Some(p);
            self.open.rest_args = files.collect();
        } else if self.prefs.start_screen && project_seam::plain_start(argv) {
            self.open_start();
        }
        if let Some(n) = super::settings_win::page_from_args(argv) {
            self.open_settings(n);
        }
    }

    pub(crate) fn close_start(&mut self) {
        self.chrome.start.open = false;
        self.chrome.start.pending_units = None;
        self.welcome_closed = true;
    }

    /// A dialog owns the window: no file drops, no start screen keys.
    fn dialog_up(&self, ctx: &egui::Context) -> bool {
        self.recover.is_some()
            || self.settings_open
            || self.chrome.onboard.open
            || self.import_open
            || project_seam::modal(self)
            || ctx.memory(|m| m.top_modal_layer().is_some())
    }

    /// Whole frame while the start screen is up: only what can be reached from it is drawn.
    pub(super) fn start_frame(&mut self, ctx: &egui::Context) {
        if self.big.busy() || self.ed.revision != self.chrome.start.rev0 {
            if self.ed.revision != self.chrome.start.rev0 {
                if let Some(u) = self.chrome.start.pending_units.take() {
                    self.ed.set_units(u);
                }
            }
            self.close_start();
            project_seam::frame(self, ctx);
            // nothing is drawn this frame; the canvas must not wait for the next event
            ctx.request_repaint();
            return;
        }
        if self.settings_open {
            self.settings_window(ctx);
        }
        self.onboarding_window(ctx);
        #[cfg(feature = "link")]
        crate::link::panel::frame(self, ctx);
        if self.autosave_open {
            self.autosave_window(ctx);
        }
        if self.import_open {
            self.import_window(ctx);
        }
        self.drawing_windows(ctx);
        if self.quick.keys_open {
            crate::quick::keys_win::ui(self, ctx);
        }
        let mut act = None;
        let start_units = units_of_pref(&self.prefs.start_units);
        if !self.dialog_up(ctx) {
            ctx.input_mut(|i| {
                use egui::{Key, KeyboardShortcut as S, Modifiers as M};
                if i.consume_shortcut(&S::new(M::COMMAND, Key::N)) {
                    act = Some(Act::New(Some(start_units)));
                } else if i.consume_shortcut(&S::new(M::COMMAND, Key::O)) {
                    act = Some(Act::OpenDialog);
                } else if i.consume_key(M::NONE, Key::Escape) {
                    act = Some(Act::Skip);
                }
            });
        }
        self.start_news();
        if self.chrome.start.recent.is_none() {
            self.chrome.start.recent = Some(project_seam::recent_projects(self));
        }
        let recent = self.chrome.start.recent.clone().unwrap_or_default();
        let paths: Vec<PathBuf> = recent.iter().map(|r| r.path.clone()).collect();
        self.chrome.start.probe.update(&paths, Instant::now());
        if self.chrome.start.probe.busy() || !self.chrome.start.thumb_busy.is_empty() {
            ctx.request_repaint_after(PROBE_POLL);
        }
        self.start_thumbs(ctx, &recent);
        let bg = self.sh.theme.c("bg.app");
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(bg))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("start.scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let avail = ui.available_width();
                        let w = (avail - 48.0).clamp(280.0, 1080.0);
                        let pad = ((avail - w) / 2.0).max(16.0);
                        ui.add_space(space::XL);
                        ui.horizontal_top(|ui| {
                            ui.add_space(pad);
                            ui.vertical(|ui| {
                                ui.set_width(w);
                                if let Some(a) = self.start_body(ui, w, &recent) {
                                    act = Some(a);
                                }
                            });
                        });
                        ui.add_space(space::XL);
                    });
            });
        crate::help::frame(self, ctx);
        self.first_run_frame(ctx);
        self.blocking_window(ctx);
        self.recover_window(ctx);
        project_seam::frame(self, ctx);
        self.status_to_toast();
        let d = self.sh.theme.d();
        let s = &mut self.sh;
        if let Some(cmd) = widgets::toasts::show(ctx, &s.theme, &s.cat, &mut s.toasts, d.row) {
            if cmd == FORGET_UNDO {
                if let Some(f) = self.chrome.start.forgot.take() {
                    if let Some(at) = f.in_recent {
                        put_entry(&mut self.prefs.recent, &f.key, at);
                    }
                    if let Some(at) = f.in_projects {
                        put_entry(&mut self.prefs.recent_projects, &f.key, at);
                    }
                    self.chrome.start.recent = None;
                    self.prefs.save();
                }
            } else if ddc_ui::notify::STEP_CMDS.contains(&cmd.as_str()) {
                if let Some(e) = self.sh.toasts.fired_entry.take() {
                    self.step_toast_entry(cmd == "edit.undo", e);
                }
            } else if self.sh.reg.get(&cmd).is_some() {
                self.run(&cmd);
            }
        }
        widgets::focus_ring(ctx, &self.sh.theme);
        if let Some(a) = act {
            self.start_act(a);
        }
    }

    fn start_news(&mut self) {
        let lang = self.sh.cat.lang;
        if matches!(&self.chrome.start.news, Some((l, ..)) if *l == lang) {
            return;
        }
        let topic = ddc_ui::help::whats_new(lang);
        let c = &self.sh.cat;
        let label = |id: &str| c.t(&format!("cmd.{id}"));
        let (head, items) = latest_news(&topic.blocks, NEWS_MAX, &label);
        self.chrome.start.news = Some((lang, head, items));
    }

    fn start_thumbs(&mut self, ctx: &egui::Context, recent: &[RecentProject]) {
        let st = &mut self.chrome.start;
        let (tx, rx) = st.thumb_chan.get_or_insert_with(channel);
        while let Ok((file, img)) = rx.try_recv() {
            let tex = img.map(|img| {
                ctx.load_texture(
                    format!("start.thumb.{}", file.display()),
                    img,
                    egui::TextureOptions::LINEAR,
                )
            });
            st.thumb_busy.remove(&file);
            st.thumbs.insert(file, tex);
        }
        for r in recent {
            let Some(file) = &r.thumbnail else { continue };
            if st.thumbs.contains_key(file) || st.thumb_busy.contains(file) {
                continue;
            }
            if decode_in_background(file, tx) {
                st.thumb_busy.insert(file.clone());
            } else {
                st.thumbs.insert(file.clone(), None);
            }
        }
    }

    fn start_body(&mut self, ui: &mut egui::Ui, w: f32, recent: &[RecentProject]) -> Option<Act> {
        let mut act = None;
        self.start_header(ui, &mut act);
        ui.add_space(space::XL);
        if w >= 760.0 {
            let left = 280.0;
            let right = w - left - 24.0;
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(left);
                    self.start_actions(ui, &mut act);
                });
                ui.add_space(space::XL);
                ui.vertical(|ui| {
                    ui.set_width(right);
                    self.start_recent(ui, right, recent, &mut act);
                    ui.add_space(space::XL);
                    self.start_whats_new(ui, &mut act);
                });
            });
        } else {
            self.start_actions(ui, &mut act);
            ui.add_space(space::XL);
            self.start_recent(ui, w, recent, &mut act);
            ui.add_space(space::XL);
            self.start_whats_new(ui, &mut act);
        }
        ui.add_space(space::XL);
        crate::brand::family_block(ui, &self.sh.theme, &self.sh.cat);
        act
    }

    fn start_header(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        let s = &mut self.sh;
        let (th, c) = (&s.theme, &s.cat);
        ui.horizontal(|ui| {
            crate::brand::design_mark(ui, th, 28.0);
            ui.label(
                RichText::new(c.t("start.brand"))
                    .font(th.font(Role::Heading))
                    .color(th.c("text.primary")),
            );
            ui.label(
                RichText::new(env!("CARGO_PKG_VERSION"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let tip = c.t("cmd.mgr.settings");
                if widgets::icon_button(ui, &mut s.icons, th, "settings", false, &tip).clicked() {
                    *act = Some(Act::Run("mgr.settings"));
                }
                if ui.link(c.t("start.skip")).on_hover_text("Esc").clicked() {
                    *act = Some(Act::Skip);
                }
            });
        });
    }

    /// The assistant first: describe the project in words and the AI panel takes it from there.
    fn start_ask(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        use ddc_ui::widgets::{field::TextField, Button};
        let s = &mut self.sh;
        let (th, c) = (&s.theme, &s.cat);
        let ask = &mut self.chrome.start.ask;
        widgets::card(ui, th, |ui| {
            ui.set_width(ui.available_width());
            let title_text = c.t("start.ai.title");
            let title_id = ui
                .horizontal(|ui| {
                    let px = ddc_ui::theme::icon_size::MD;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(px), Sense::hover());
                    s.icons.paint(ui, ddc_ui::icons::ctl::AI, px as u32, r, th.c("ai.accent"));
                    let title = th.rich(Role::BodyStrong, &title_text);
                    ui.label(title.color(th.c("text.primary"))).id
                })
                .inner;
            let sub = th.rich(Role::Caption, &c.t("start.ai.sub"));
            ui.label(sub.color(th.c("text.secondary")));
            ui.add_space(space::XS);
            let hint = c.t("start.ai.hint");
            let out = TextField::new(ask)
                .id_salt("ddc.start.ask")
                .labelled_by(title_id, &title_text)
                .hint(&hint)
                .width(ui.available_width())
                .show(ui, th);
            let enter = out.committed && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(space::XS);
            let go = c.t("start.ai.go");
            let tip = format!("{}  \u{b7}  Ctrl+J", c.t("cmd.ai.home.desc"));
            if Button::primary(&go).show(ui, th).on_hover_text(tip).clicked() || enter {
                *act = Some(Act::Ask(ask.trim().to_string()));
            }
        });
        ui.add_space(space::LG);
    }

    fn start_actions(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        use super::onboard::Focus;
        // the start page follows the focus: the assistant leads for AI (and with no focus),
        // otherwise the focus's own action leads and the assistant comes after the drop zone
        let primary = self.focus().first().copied();
        let ask_first = matches!(primary, None | Some(Focus::Ai));
        if ask_first {
            self.start_ask(ui, act);
        }
        let lead = match primary {
            Some(Focus::Data) => Some(("xlsx-in", "data.import_xlsx", "start.focus.data", "start.focus.data_sub")),
            Some(Focus::Drawings) => Some(("import-cad", "mgr.import_cad", "start.focus.drawings", "start.focus.drawings_sub")),
            _ => None,
        };
        if let Some((icon, id, title, sub)) = lead {
            let s = &mut self.sh;
            let (th, c) = (&s.theme, &s.cat);
            shell_kit::section_title(ui, th, &c.t("start.focus.title"));
            ui.add_space(space::XS);
            let (t, d) = (c.t(title), c.t(sub));
            if shell_kit::action_row(ui, th, &mut s.icons, icon, &t, &d, "", true).clicked() {
                *act = Some(Act::Run(id));
            }
            ui.add_space(space::LG);
        }
        self.start_begin(ui, act, lead.is_none());
        if !ask_first {
            ui.add_space(space::LG);
            self.start_ask(ui, act);
        }
    }

    fn start_begin(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>, new_is_primary: bool) {
        // with no recent files the empty recent list offers the sample: one place, not two
        let sample_row = self.chrome.start.recent.as_ref().is_some_and(|r| !r.is_empty());
        let s = &mut self.sh;
        let (th, c) = (&s.theme, &s.cat);
        shell_kit::section_title(ui, th, &c.t("start.begin"));
        ui.add_space(space::XS);
        let units = units_of_pref(&self.prefs.start_units);
        let new_label = c.t("start.new");
        let new_sub = c.t("start.new_sub");
        if shell_kit::action_row(
            ui,
            th,
            &mut s.icons,
            "template",
            &new_label,
            &new_sub,
            "Ctrl+N",
            new_is_primary,
        )
        .clicked()
        {
            *act = Some(Act::New(Some(units)));
        }
        ui.add_space(space::XS);
        // the units belong to the new project: a labelled row right under it
        let (m, i) = (c.t("start.tpl.metric"), c.t("start.tpl.imperial"));
        let picked = ui
            .horizontal(|ui| {
                ui.add_space(space::SM);
                let units_label = th.rich(Role::Caption, &c.t("sheet_tools.start.units"));
                ui.label(units_label.color(th.c("text.secondary")));
                widgets::segmented(ui, th, &[m.as_str(), i.as_str()], units_index(units))
            })
            .inner;
        if let Some(k) = picked {
            let u = if k == 1 { Units::Imperial } else { Units::Metric };
            self.prefs.start_units = pref_of_units(u).into();
            self.prefs.save();
        }
        ui.add_space(space::XS);
        if shell_kit::action_row(
            ui,
            th,
            &mut s.icons,
            "open",
            &c.t("start.open"),
            &c.t("start.open_sub"),
            "Ctrl+O",
            false,
        )
        .clicked()
        {
            *act = Some(Act::OpenDialog);
        }
        if sample_row {
            ui.add_space(space::XS);
            let (title, sub) = (c.t("start.sample"), c.t("start.sample_sub"));
            let icons = &mut s.icons;
            let row = shell_kit::action_row(ui, th, icons, "sample", &title, &sub, "", false);
            if row.clicked() {
                *act = Some(Act::Sample);
            }
        }
        ui.add_space(space::XS);
        if shell_kit::action_row(
            ui,
            th,
            &mut s.icons,
            "help",
            &c.t("start.tutorial"),
            &c.t("start.tutorial_sub"),
            "",
            false,
        )
        .clicked()
        {
            *act = Some(Act::Tutorial);
        }
        ui.add_space(space::LG);
        let (r, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 92.0), Sense::click());
        let fg = widgets::paint_state(ui, th, r, &resp, false);
        shell_kit::dashed_frame(ui, r.shrink(1.0), th.c("border.strong"));
        let px = ddc_ui::theme::icon_size::MD;
        let ic = egui::Rect::from_center_size(r.center() - Vec2::new(0.0, 22.0), Vec2::splat(px));
        s.icons.paint(ui, "import", px as u32, ic, fg);
        ui.painter().text(
            r.center() + Vec2::new(0.0, 2.0),
            egui::Align2::CENTER_CENTER,
            c.t("start.drop"),
            th.font(Role::BodyStrong),
            th.c("text.primary"),
        );
        ui.painter().text(
            r.center() + Vec2::new(0.0, 22.0),
            egui::Align2::CENTER_CENTER,
            c.t("start.drop_formats"),
            th.font(Role::Caption),
            th.c("text.secondary"),
        );
        if resp.clicked() {
            *act = Some(Act::OpenDialog);
        }
    }

    fn start_recent(
        &mut self,
        ui: &mut egui::Ui,
        w: f32,
        recent: &[RecentProject],
        act: &mut Option<Act>,
    ) {
        let cols = (((w + space::MD) / (CARD_W + space::MD)).floor() as usize).max(1);
        let ctx = ui.ctx().clone();
        let keys_live = !self.dialog_up(&ctx);
        let s = &mut self.sh;
        let (th, c) = (&s.theme, &s.cat);
        shell_kit::section_title(ui, th, &c.t("start.recent"));
        ui.add_space(space::XS);
        if recent.is_empty() {
            egui::Frame::new()
                .fill(th.c("bg.panel"))
                .corner_radius(th.radius("md"))
                .inner_margin(egui::Margin::same(space::LG as i8))
                .show(ui, |ui| {
                    ui.set_width(w - 2.0 * space::LG);
                    ui.vertical_centered(|ui| {
                        if crate::ui_kit::empty_state_action(
                            ui,
                            "start_recent",
                            "empty.start_recent.action",
                        ) {
                            *act = Some(Act::Sample);
                        }
                    });
                });
            return;
        }
        let n = recent.len();
        let mut scroll_to = None;
        if let Some(i) = focused_card(ctx.memory(|m| m.focused()), n) {
            // without the lock egui also moves focus by arrow direction
            let lock = egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                ..Default::default()
            };
            ctx.memory_mut(|m| m.set_focus_lock_filter(card_id(i), lock));
        }
        if keys_live {
            let focus = focused_card(ctx.memory(|m| m.focused()), n);
            let typing = ctx.wants_keyboard_input();
            let hit = ctx.input_mut(|i| {
                GRID_KEYS.iter().find_map(|k| {
                    let ev = recent_key(*k, focus, cols, n, typing)?;
                    i.consume_key(egui::Modifiers::NONE, *k).then_some(ev)
                })
            });
            match hit {
                Some(RecentKey::Focus(j)) => {
                    ctx.memory_mut(|m| m.request_focus(card_id(j)));
                    scroll_to = Some(j);
                    ctx.request_repaint();
                }
                Some(RecentKey::Forget(i)) => {
                    *act = Some(Act::Forget(recent[i].path.clone()));
                }
                None => {}
            }
        }
        for (row, chunk) in recent.chunks(cols).enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::MD;
                for (col, r) in chunk.iter().enumerate() {
                    let idx = row * cols + col;
                    let thumb = r
                        .thumbnail
                        .as_ref()
                        .and_then(|f| self.chrome.start.thumbs.get(f))
                        .and_then(|t| t.as_ref());
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(CARD_W, CARD_H), Sense::hover());
                    let resp = ui.interact(rect, card_id(idx), Sense::click());
                    if scroll_to == Some(idx) {
                        resp.scroll_to_me(None);
                    }
                    let st = self.chrome.start.probe.state(&r.path);
                    let kind = card_kind(st);
                    let line = match st {
                        Some(State::Present { modified, size }) => format!(
                            "{} · {}",
                            modified.map(fmt_time).unwrap_or_default(),
                            fmt_size(c, *size)
                        ),
                        Some(State::Missing) => c.t("start.missing_short"),
                        Some(State::Unavailable) => c.t("start_recent.unavailable_short"),
                        None => String::new(),
                    };
                    let tile = ThumbTile {
                        th,
                        project: r,
                        tex: thumb,
                        line: &line,
                        kind,
                    };
                    tile.paint(ui, rect, &resp, &mut s.icons);
                    let tip = match kind {
                        Kind::Missing => c.tf(
                            "start.missing",
                            &[("path", r.path.display().to_string().as_str())],
                        ),
                        Kind::Unavailable => c.tf(
                            "start_recent.unavailable",
                            &[("path", r.path.display().to_string().as_str())],
                        ),
                        _ => r.path.display().to_string(),
                    };
                    let resp = resp.on_hover_text(tip);
                    if matches!(kind, Kind::Missing | Kind::Unavailable) {
                        // a greyed card says what to do with it: find the file or drop the row;
                        // a place that does not answer only offers Retry (Forget stays in the menu)
                        let bar = egui::Rect::from_min_size(
                            rect.left_bottom() + Vec2::new(space::SM, -space::XXL),
                            Vec2::new(
                                rect.width() - 2.0 * space::SM,
                                ddc_ui::sem::control::HIT_MIN,
                            ),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
                            ui.horizontal(|ui| {
                                if kind == Kind::Unavailable {
                                    if widgets::Button::quiet(&c.t("start_recent.retry"))
                                        .show(ui, th)
                                        .clicked()
                                    {
                                        *act = Some(Act::Retry(r.path.clone()));
                                    }
                                    return;
                                }
                                if widgets::Button::quiet(&c.t("open_guard.locate"))
                                    .show(ui, th)
                                    .clicked()
                                {
                                    *act = Some(Act::Locate(r.path.clone()));
                                }
                                if widgets::Button::quiet(&c.t("start.menu.forget"))
                                    .show(ui, th)
                                    .clicked()
                                {
                                    *act = Some(Act::Forget(r.path.clone()));
                                }
                            });
                        });
                    }
                    if resp.clicked() && act.is_none() {
                        *act = match click_of(kind) {
                            Click::Nothing => None,
                            Click::Open => Some(Act::Open(r.path.clone())),
                            Click::Locate => Some(Act::Locate(r.path.clone())),
                            Click::Retry => Some(Act::Retry(r.path.clone())),
                        };
                    }
                    let ready = kind == Kind::Ready;
                    resp.context_menu(|ui| {
                        if menu_row(ui, th, &c.t("start.menu.open"), "Enter", ready).clicked() {
                            *act = Some(Act::Open(r.path.clone()));
                            ui.close();
                        }
                        if menu_row(ui, th, &c.t("start.menu.reveal"), "", ready).clicked() {
                            *act = Some(Act::Reveal(r.path.clone()));
                            ui.close();
                        }
                        if kind == Kind::Missing
                            && menu_row(ui, th, &c.t("open_guard.locate"), "", true).clicked()
                        {
                            *act = Some(Act::Locate(r.path.clone()));
                            ui.close();
                        }
                        if kind == Kind::Unavailable
                            && menu_row(ui, th, &c.t("start_recent.retry"), "", true).clicked()
                        {
                            *act = Some(Act::Retry(r.path.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if menu_row(ui, th, &c.t("start.menu.forget"), "Del", true).clicked() {
                            *act = Some(Act::Forget(r.path.clone()));
                            ui.close();
                        }
                    });
                }
            });
            ui.add_space(space::MD);
        }
    }

    fn start_whats_new(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        let Some((_, head, items)) = &self.chrome.start.news else {
            return;
        };
        if items.is_empty() {
            return;
        }
        let (th, c) = (&self.sh.theme, &self.sh.cat);
        ui.horizontal(|ui| {
            shell_kit::section_title(ui, th, &format!("{} · {head}", c.t("start.news")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link(c.t("start.news_all")).clicked() {
                    *act = Some(Act::Run("help.whats_new"));
                }
            });
        });
        ui.add_space(space::XS);
        for t in items {
            ui.horizontal_top(|ui| {
                let (dot, _) = ui.allocate_exact_size(Vec2::new(10.0, 16.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 2.0, th.c("text.secondary"));
                ui.add(
                    egui::Label::new(
                        RichText::new(t)
                            .font(th.font(Role::Body))
                            .color(th.c("text.primary")),
                    )
                    .wrap(),
                );
            });
        }
    }

    fn start_act(&mut self, act: Act) {
        self.chrome.start.pending_units = None;
        match act {
            Act::New(units) => {
                if let Some(u) = units {
                    self.prefs.start_units = pref_of_units(u).into();
                    self.prefs.save();
                }
                // while the unsaved prompt is up the screen stays; the new model closes it
                if project_seam::new_project(self, units) {
                    self.close_start();
                }
            }
            Act::OpenDialog => {
                crate::open::pick(self, crate::open::decide::Door::OpenDialog);
            }
            // the command asks before replacing a model that has content; the new model closes
            Act::Sample => {
                self.run("help.sample");
            }
            Act::Tutorial => {
                self.close_start();
                self.run("help.tutorial");
            }
            Act::Ask(text) => {
                self.close_start();
                self.chrome.start.ask.clear();
                if text.is_empty() {
                    self.run("ai.ask");
                } else {
                    crate::ai::ui::ask(self, &text);
                }
            }
            Act::Open(p) => self.start_open(p),
            Act::Reveal(p) => reveal_in_folder(&p),
            Act::Locate(p) => self.locate_recent(p),
            Act::Retry(p) => self.chrome.start.probe.retry(&p, Instant::now()),
            Act::Forget(p) => {
                let key = p.to_string_lossy().into_owned();
                let f = Forgot {
                    in_recent: take_entry(&mut self.prefs.recent, &key),
                    in_projects: take_entry(&mut self.prefs.recent_projects, &key),
                    key,
                };
                self.chrome.start.recent = None;
                self.prefs.save();
                if f.in_recent.is_some() || f.in_projects.is_some() {
                    let name = p.file_name().map_or_else(
                        || p.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    let cat = &self.sh.cat;
                    let (msg, undo) = (
                        cat.tf("start.forgot", &[("name", name.as_str())]),
                        cat.t("start.forgot_undo"),
                    );
                    self.sh.toasts.push_cmd(Level::Info, msg, undo, FORGET_UNDO);
                    self.chrome.start.forgot = Some(f);
                }
            }
            Act::Skip => self.close_start(),
            Act::Run(id) => {
                self.run(id);
            }
        }
    }

    /// Asks where a missing recent project is now; the row follows it and it opens.
    fn locate_recent(&mut self, old: PathBuf) {
        let mut d = crate::file_pick::Dlg::new().add_filter("DDC", &[ddc_model::PROJECT_EXT]);
        if let Some(name) = old.file_name() {
            d = d.set_file_name(name.to_string_lossy());
        }
        let Some(found) = d.pick_file() else {
            return;
        };
        let (from, to) = (
            old.to_string_lossy().into_owned(),
            found.to_string_lossy().into_owned(),
        );
        for list in [&mut self.prefs.recent, &mut self.prefs.recent_projects] {
            if list.contains(&from) {
                list.retain(|r| *r != to);
                for r in list.iter_mut().filter(|r| **r == from) {
                    r.clone_from(&to);
                }
            }
        }
        self.prefs.save();
        self.chrome.start.recent = None;
        self.start_open(found);
    }

    fn start_open(&mut self, p: PathBuf) {
        // the answer comes from the probe; the UI thread never asks the disk here
        let key = match self.chrome.start.probe.state(&p) {
            Some(State::Missing) => Some("start.missing"),
            Some(State::Unavailable) => Some("start_recent.unavailable"),
            _ => None,
        };
        if let Some(key) = key {
            let msg = self
                .sh
                .cat
                .tf(key, &[("path", p.display().to_string().as_str())]);
            self.sh.toasts.push(Level::Warning, msg, None);
            return;
        }
        // a project closes the screen when its model is in: it may still ask about unsaved work
        crate::open::request(self, vec![p], crate::open::decide::Door::Recent);
    }

    fn open_arg(&mut self, p: PathBuf) {
        let mut files = vec![p];
        files.append(&mut self.open.rest_args);
        let ok = crate::open::request(self, files, crate::open::decide::Door::Arg)
            != crate::open::Outcome::Nothing;
        if ok {
            self.welcome_closed = true;
        } else if self.prefs.start_screen {
            self.open_start();
        }
    }

    /// Files over the window: overlay while hovering, open on release. A dialog that blocks
    /// the window refuses the drop, with a message.
    pub(super) fn handle_drops(&mut self, ctx: &egui::Context) {
        let (hover, dropped): (bool, Vec<PathBuf>) = ctx.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect(),
            )
        });
        // the branding page of the open settings window takes a picture dropped on it
        let on_logo_page = self.settings_open
            && self.settings_page == super::settings_win::BRANDING_PAGE
            && self.chrome.settings.query.trim().is_empty();
        if on_logo_page && !dropped.is_empty() {
            if let Some(p) = dropped.into_iter().next() {
                super::settings_win::logo_dropped(self, p);
            }
            return;
        }
        if self.dialog_up(ctx) {
            if !dropped.is_empty() {
                let msg = self.sh.cat.t("drop.blocked");
                self.sh.toasts.push(Level::Info, msg, None);
            }
            return;
        }
        if let Some(p) = self.chrome.start.arg_file.take() {
            self.open_arg(p);
        }
        if hover {
            let hint = if crate::ai::ui::chat_drop_hint(ctx) { "ai.att.drop_hint" } else { "drop.hint" };
            drop_overlay(ctx, &self.sh.theme, &self.sh.cat.t(hint));
        }
        if !dropped.is_empty() && crate::ai::ui::take_chat_drop(ctx, &dropped) {
            ctx.request_repaint();
        } else if !dropped.is_empty() {
            crate::open::request(self, dropped, crate::open::decide::Door::Drop);
        }
    }
}

struct ThumbTile<'a> {
    th: &'a Theme,
    project: &'a RecentProject,
    tex: Option<&'a egui::TextureHandle>,
    /// Second text line of the card; empty while `kind` is `Loading`.
    line: &'a str,
    kind: Kind,
}

impl ThumbTile<'_> {
    fn paint(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        resp: &egui::Response,
        icons: &mut ddc_ui::icons::IconCache,
    ) {
        let (th, r) = (self.th, self.project);
        let dim = matches!(self.kind, Kind::Missing | Kind::Unavailable);
        let radius = th.radius("md");
        let border = if resp.hovered() || resp.has_focus() {
            th.c("accent")
        } else {
            th.c("border.subtle")
        };
        let p = ui.painter();
        p.rect_filled(rect, radius, th.c("bg.panel"));
        p.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let area = egui::Rect::from_min_size(
            rect.min + Vec2::splat(4.0),
            Vec2::new(rect.width() - 8.0, THUMB_H),
        );
        p.rect_filled(area, th.radius("sm"), th.c("bg.sunken"));
        if let Some(t) = self.tex {
            let img = fit_rect(area, t.size_vec2());
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            let tint = if dim {
                egui::Color32::from_white_alpha(90)
            } else {
                egui::Color32::WHITE
            };
            p.image(t.id(), img, uv, tint);
        } else {
            let ic = egui::Rect::from_center_size(
                area.center() - Vec2::new(0.0, 8.0),
                Vec2::splat(32.0),
            );
            let col = if dim {
                th.c("text.disabled")
            } else {
                th.c("text.secondary")
            };
            icons.paint(ui, ext_icon(&r.path), 32, ic, col);
            p.text(
                area.center() + Vec2::new(0.0, 22.0),
                egui::Align2::CENTER_CENTER,
                ext_upper(&r.path),
                th.font(Role::Caption),
                col,
            );
        }
        let (name_col, sub_col) = if dim {
            (th.c("text.disabled"), th.c("text.disabled"))
        } else {
            (th.c("text.primary"), th.c("text.secondary"))
        };
        let room = rect.width() - 16.0;
        let name = shell_kit::elided(ui, &r.name, th.font(Role::BodyStrong), name_col, room);
        let y = area.bottom() + 6.0;
        let y2 = y + name.size().y + 2.0;
        p.galley(egui::pos2(rect.left() + 8.0, y), name, name_col);
        if self.kind == Kind::Loading {
            // skeleton bar where date and size will be
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, y2 + 2.0),
                Vec2::new(room * 0.6, 10.0),
            );
            p.rect_filled(bar, th.radius("sm"), th.c("border.subtle"));
        } else {
            let sub = shell_kit::elided(ui, self.line, th.font(Role::Caption), sub_col, room);
            p.galley(egui::pos2(rect.left() + 8.0, y2), sub, sub_col);
        }
    }
}

fn drop_overlay(ctx: &egui::Context, th: &Theme, text: &str) {
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("ddc.drop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(false)
        .show(ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
            ui.painter()
                .rect_filled(r, 0.0, th.c("bg.app").gamma_multiply(0.85));
            shell_kit::dashed_frame(ui, r.shrink(16.0), th.c("accent"));
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                text,
                th.font(Role::Title),
                th.c("text.primary"),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn news_takes_the_first_section_only() {
        let t = |s: &str| vec![Inline::Text(s.into())];
        let blocks = vec![
            Block::Heading("2026.11".into()),
            Block::Bullet(t("a")),
            Block::Bullet(vec![
                Inline::Text("b ".into()),
                Inline::Cmd("x.y".into()),
                Inline::Topic("z".into()),
            ]),
            Block::Bullet(t("c")),
            Block::Heading("2026.10".into()),
            Block::Bullet(t("old")),
        ];
        let label = |id: &str| format!("[{id}]");
        let (head, items) = latest_news(&blocks, 2, &label);
        assert_eq!(head, "2026.11");
        assert_eq!(items, vec!["a".to_string(), "b [x.y]".to_string()]);
    }

    #[test]
    fn thumbnail_keeps_proportions() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(200.0, 100.0));
        let r = fit_rect(area, Vec2::new(100.0, 100.0));
        assert_eq!((r.width(), r.height()), (100.0, 100.0));
        assert_eq!(r.center(), area.center());
        assert_eq!(fit_rect(area, Vec2::ZERO), area);
    }

    #[test]
    fn start_new_uses_selected_units() {
        assert_eq!(units_of_pref(""), Units::Metric, "no choice yet: metric");
        assert_eq!(units_of_pref("imperial"), Units::Imperial);
        for u in [Units::Metric, Units::Imperial] {
            assert_eq!(units_of_pref(pref_of_units(u)), u);
            assert_eq!(units_of_pref(pref_of_units(u)), [Units::Metric, Units::Imperial][units_index(u)]);
        }
        let mut p = crate::prefs::Prefs::default();
        p.start_units = pref_of_units(Units::Imperial).into();
        let back = crate::prefs::Prefs::parse(&p.text());
        assert_eq!(units_of_pref(&back.start_units), Units::Imperial, "last choice survives a restart");
    }

    #[test]
    fn grid_nav_walks_and_wraps() {
        // 7 cards in 3 columns: rows of 3, 3, 1
        let g = |i, nav| grid_nav(i, 3, 7, nav);
        assert_eq!(g(0, Nav::Right), 1);
        assert_eq!(g(6, Nav::Right), 0, "wraps to the first");
        assert_eq!(g(0, Nav::Left), 6, "wraps to the last");
        assert_eq!(g(1, Nav::Down), 4);
        assert_eq!(g(0, Nav::Down), 3);
        assert_eq!(g(3, Nav::Up), 0);
        assert_eq!(g(2, Nav::Home), 0);
        assert_eq!(g(2, Nav::End), 6);
    }

    #[test]
    fn grid_nav_short_last_row() {
        let g = |i, nav| grid_nav(i, 3, 7, nav);
        assert_eq!(g(4, Nav::Down), 6, "no card under it: the last one");
        assert_eq!(g(5, Nav::Down), 6);
        assert_eq!(g(6, Nav::Down), 0, "bottom wraps to the top of its column");
        assert_eq!(g(0, Nav::Up), 6, "top wraps to the last row");
        assert_eq!(g(1, Nav::Up), 4, "short last row: nearest full row");
        assert_eq!(g(2, Nav::Up), 5);
    }

    #[test]
    fn grid_nav_edge_sizes() {
        assert_eq!(grid_nav(0, 3, 0, Nav::Down), 0, "empty grid");
        assert_eq!(grid_nav(0, 3, 1, Nav::Right), 0);
        assert_eq!(grid_nav(0, 3, 1, Nav::Up), 0);
        assert_eq!(grid_nav(0, 3, 1, Nav::Down), 0);
        assert_eq!(grid_nav(0, 0, 4, Nav::Down), 1, "zero columns count as one");
        assert_eq!(grid_nav(9, 3, 4, Nav::Left), 2, "stale index is clamped");
    }

    #[test]
    fn recent_keys_move_and_forget() {
        use egui::Key;
        assert_eq!(recent_key(Key::ArrowRight, Some(0), 3, 5, false), Some(RecentKey::Focus(1)));
        assert_eq!(recent_key(Key::ArrowDown, Some(1), 3, 5, false), Some(RecentKey::Focus(4)));
        assert_eq!(recent_key(Key::Delete, Some(2), 3, 5, false), Some(RecentKey::Forget(2)));
        assert_eq!(recent_key(Key::Backspace, Some(2), 3, 5, false), Some(RecentKey::Forget(2)));
        // nothing focused: forward keys enter the grid, back keys and Delete do nothing
        assert_eq!(recent_key(Key::ArrowDown, None, 3, 5, false), Some(RecentKey::Focus(0)));
        assert_eq!(recent_key(Key::End, None, 3, 5, false), Some(RecentKey::Focus(4)));
        assert_eq!(recent_key(Key::ArrowUp, None, 3, 5, false), None);
        assert_eq!(recent_key(Key::Delete, None, 3, 5, false), None);
        assert_eq!(recent_key(Key::A, Some(0), 3, 5, false), None, "other keys are not ours");
    }

    #[test]
    fn recent_keys_ignored_while_typing() {
        for k in GRID_KEYS {
            assert_eq!(recent_key(k, Some(1), 3, 5, true), None, "{k:?}");
            assert_eq!(recent_key(k, None, 3, 5, true), None, "{k:?}");
        }
    }

    #[test]
    fn delete_on_a_missing_card_does_nothing() {
        assert_eq!(recent_key(egui::Key::Delete, Some(5), 3, 5, false), None, "past the end");
        assert_eq!(recent_key(egui::Key::Delete, Some(0), 3, 0, false), None, "empty list");
        assert_eq!(
            recent_key(egui::Key::ArrowRight, Some(5), 3, 5, false),
            Some(RecentKey::Focus(0)),
            "stale focus counts as none"
        );
    }

    #[test]
    fn card_kinds_tell_gone_from_unreachable() {
        let present = State::Present {
            modified: None,
            size: 1,
        };
        assert_eq!(card_kind(None), Kind::Loading);
        assert_eq!(card_kind(Some(&present)), Kind::Ready);
        assert_eq!(card_kind(Some(&State::Missing)), Kind::Missing);
        assert_eq!(card_kind(Some(&State::Unavailable)), Kind::Unavailable);
    }

    #[test]
    fn click_never_opens_or_forgets_what_is_unknown() {
        assert_eq!(click_of(Kind::Loading), Click::Nothing, "no answer yet");
        assert_eq!(click_of(Kind::Ready), Click::Open);
        assert_eq!(click_of(Kind::Missing), Click::Locate);
        assert_eq!(click_of(Kind::Unavailable), Click::Retry, "not Open, not Forget");
    }

    #[test]
    fn recent_list_is_built_without_touching_the_disk() {
        let list = vec!["Z:/offline/share/a.ddc".to_string()];
        let r = crate::project::recent_entries(&list);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "a");
        assert_eq!(r[0].modified, None);
        assert_eq!(list.len(), 1, "the list itself is not touched");
    }

    #[test]
    fn unavailable_texts_exist_in_both_languages() {
        for lang in ["en", "ru"] {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/data/i18n/");
            let t = std::fs::read_to_string(format!("{dir}{lang}/start_recent.json")).unwrap();
            for k in ["start_recent.unavailable", "start_recent.unavailable_short", "start_recent.retry"] {
                assert!(t.contains(&format!("\"{k}\"")), "{lang}: {k}");
            }
        }
    }

    #[test]
    fn start_screen_asks_the_disk_only_through_the_probe() {
        let src = include_str!("start_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        for banned in [".exists()", "fs::metadata", "recent_from"] {
            assert!(!code.contains(banned), "UI thread must not use {banned}");
        }
        assert!(!code.contains("load_png(file)"), "decode runs on a worker");
    }

    #[test]
    fn focused_card_matches_ids() {
        assert_eq!(focused_card(Some(card_id(2)), 4), Some(2));
        assert_eq!(focused_card(Some(card_id(4)), 4), None);
        assert_eq!(focused_card(Some(egui::Id::new("other")), 4), None);
        assert_eq!(focused_card(None, 4), None);
    }

    #[test]
    fn forget_and_undo_restore_both_lists() {
        let mut recent: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let mut projects: Vec<String> = vec!["b".into(), "z".into()];
        let f = Forgot {
            in_recent: take_entry(&mut recent, "b"),
            in_projects: take_entry(&mut projects, "b"),
            key: "b".into(),
        };
        assert_eq!((f.in_recent, f.in_projects), (Some(1), Some(0)));
        assert_eq!(recent, ["a", "c"]);
        assert_eq!(projects, ["z"]);
        assert_eq!(take_entry(&mut recent, "nope"), None, "unknown entry is a no-op");
        put_entry(&mut recent, "b", 1);
        put_entry(&mut projects, "b", 0);
        assert_eq!(recent, ["a", "b", "c"]);
        assert_eq!(projects, ["b", "z"]);
        put_entry(&mut recent, "b", 99);
        assert_eq!(recent, ["a", "c", "b"], "short list: appended, never duplicated");
    }

    #[test]
    fn empty_recent_has_designed_texts_in_both_languages() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/data/i18n/");
        for lang in ["en", "ru"] {
            let t = std::fs::read_to_string(format!("{dir}{lang}/empty_start.json")).unwrap();
            for k in [
                "empty.start_recent.title",
                "empty.start_recent.hint",
                "empty.start_recent.action",
                "start.recent_open_file",
                "start.forgot",
                "start.forgot_undo",
            ] {
                assert!(t.contains(&format!("\"{k}\"")), "{lang}: {k}");
            }
        }
        let src = include_str!("start_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(!code.contains("start.recent_empty"), "old plain labels are gone");
        assert!(code.contains("empty_state_action"), "the shared empty state is used");
    }

    #[test]
    fn start_lists_one_new_action() {
        let src = include_str!("start_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        let rows = code.matches("\"template\",").count();
        assert_eq!(rows, 1, "one New project row");
        assert!(!code.contains("start.templates"), "no second template section");
        assert!(!code.contains("Act::New(None)"));
        for lang in ["en", "ru"] {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/data/i18n/");
            let t = std::fs::read_to_string(format!("{dir}{lang}/browser_settings.json")).unwrap();
            assert!(!t.contains("\"start.templates\""), "{lang}: stale key");
            assert!(t.contains("\"start.tpl.metric\"") && t.contains("\"start.tpl.imperial\""));
        }
    }
}
