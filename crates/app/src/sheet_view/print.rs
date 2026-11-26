// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sheet issue: range, paper, files (single PDF, PDF per sheet, DXF per sheet), preview,
//! print progress with cancellation. The printing itself lives in `print_job`.

use std::time::Duration;

use super::print_job::{plan_outputs, OutPlan, Outcome, PrintJob};
use super::print_view::{
    notice, orient_index, orient_is_landscape, paper_at, paper_index, preview_fit, step_page,
    zoom_about, Notice, Shown, Slot, Worker, ZOOM_MAX, ZOOM_MIN,
};
use super::*;
use ddc_ui::widgets::{self, Button, Tone};

/// Sheet assembly gets at most this much time per frame, but never less than one sheet.
const STEP_BUDGET: Duration = Duration::from_millis(30);

pub struct Print {
    /// 0 current, 1 all, 2 selected, 3 set.
    pub range: u8,
    pub chosen: Vec<i64>,
    pub set: Option<i64>,
    /// None means each sheet's own format.
    pub paper: Option<String>,
    pub landscape: bool,
    pub separate: bool,
    pub pattern: String,
    pub pdf: bool,
    /// An SVG per sheet as well (beside the PDF, or in the chosen folder).
    pub svg: bool,
    msg: String,
    /// Preview page, zoom (1.0 = whole page) and pan from the area centre.
    page: usize,
    zoom: f32,
    pan: Vec2,
    preview: Slot<PreviewKey, PreviewPage>,
    lists: Option<Rc<PrintLists>>,
    job: Option<PrintJob>,
    confirm: Option<Confirm>,
}

/// Window lists: read when the revision changes, not every frame.
struct PrintLists {
    revision: u64,
    sheets: Vec<(i64, SheetDef)>,
    sets: Vec<(i64, String, Vec<i64>)>,
}

/// Overwrite prompt before printing into a folder.
struct Confirm {
    plan: OutPlan,
    names: Vec<String>,
}

/// What determines the preview image: sheet assembly is expensive, so it is not repeated every frame.
#[derive(Clone, PartialEq)]
struct PreviewKey {
    sheet: i64,
    page: usize,
    pages: usize,
    revision: u64,
    units: Units,
    paper: Option<String>,
    landscape: bool,
    ru: bool,
}

/// Page laid out once per `PreviewKey` in paper millimetres (y down from the top-left corner);
/// a frame only maps it to the screen at the current zoom.
struct PreviewPage {
    size: Vec2,
    prims: Vec<Prim>,
    texts: Vec<PaperText>,
    warnings: Vec<String>,
}

enum Prim {
    Line { pts: Vec<Pos2>, width: f32 },
    Fill(egui::Mesh),
}

struct PaperText {
    at: Vec2,
    px: f32,
    angle: f32,
    s: String,
}

impl Print {
    pub fn new(active: Option<i64>) -> Self {
        Print {
            range: if active.is_some() { 0 } else { 1 },
            chosen: active.into_iter().collect(),
            set: None,
            paper: None,
            landscape: true,
            separate: false,
            pattern: "{number} - {name}".into(),
            pdf: true,
            svg: false,
            msg: String::new(),
            page: 0,
            zoom: 1.0,
            pan: Vec2::ZERO,
            preview: Slot::default(),
            lists: None,
            job: None,
            confirm: None,
        }
    }

    fn lists(&mut self, ed: &Editor) -> Rc<PrintLists> {
        match &self.lists {
            Some(l) if l.revision == ed.revision => l.clone(),
            _ => {
                let l = Rc::new(PrintLists {
                    revision: ed.revision,
                    sheets: ed.model.sheets().unwrap_or_default(),
                    sets: ed.model.sheet_sets().unwrap_or_default(),
                });
                self.lists = Some(l.clone());
                l
            }
        }
    }

    fn start(&mut self, plan: OutPlan) {
        self.msg.clear();
        self.job = Some(PrintJob::start(plan, self.paper.clone(), self.landscape));
    }

    /// Sheets to print for the selected range, in project order.
    pub fn ids(&self, ed: &Editor) -> Vec<i64> {
        self.pick(
            &ed.model.sheets().unwrap_or_default(),
            &ed.model.sheet_sets().unwrap_or_default(),
        )
    }

    fn pick(&self, sheets: &[(i64, SheetDef)], sets: &[(i64, String, Vec<i64>)]) -> Vec<i64> {
        let all = sheets.iter().map(|s| s.0);
        match self.range {
            0 => active().into_iter().collect(),
            1 => all.collect(),
            2 => all.filter(|i| self.chosen.contains(i)).collect(),
            _ => sets
                .iter()
                .find(|s| Some(s.0) == self.set)
                .map(|s| s.2.clone())
                .unwrap_or_default(),
        }
    }
}

/// Headless set printing: a single PDF or one PDF per sheet, optionally a DXF per sheet. Returns a report;
/// failed sheets are listed in it, and an error is returned only if nothing was written.
pub fn print_to(
    ed: &Editor,
    ru: bool,
    p: &Print,
    ids: &[i64],
    target: &std::path::Path,
) -> Result<String, String> {
    if ids.is_empty() {
        return Err(msg(ru, "sheets.print.none", &[]));
    }
    if !p.pdf && !p.svg {
        return Err(msg(ru, "sheets.print.no_format", &[]));
    }
    let mut run = PrintRun::start(ed, p, ids, target);
    while !run.step(ed) {
        std::thread::sleep(Duration::from_millis(1));
    }
    let o = run.finish(ru);
    if o.files == 0 {
        return Err(o.report);
    }
    Ok(o.report)
}

/// A print without the window that the caller steps once a frame; the model is only read.
pub struct PrintRun {
    job: PrintJob,
    /// The one PDF of the set, when there is one.
    single: Option<std::path::PathBuf>,
}

/// How a [`PrintRun`] ended.
pub struct PrintDone {
    pub files: usize,
    pub cancelled: bool,
    /// Nothing failed or was written under another name.
    pub clean: bool,
    /// The set's PDF as written, under another name when the target was held open.
    pub pdf: Option<std::path::PathBuf>,
    pub report: String,
}

impl PrintRun {
    pub fn start(ed: &Editor, p: &Print, ids: &[i64], target: &std::path::Path) -> PrintRun {
        let plan = plan_outputs(ed, p, ids, target);
        let single = (p.pdf && !p.separate).then(|| target.to_path_buf());
        PrintRun {
            job: PrintJob::start(plan, p.paper.clone(), p.landscape),
            single,
        }
    }

    /// One frame of work; true once the run has ended.
    pub fn step(&mut self, ed: &Editor) -> bool {
        self.job.step(ed, STEP_BUDGET)
    }

    /// Sheets done of all.
    pub fn fraction(&self) -> f32 {
        let (done, total, _) = self.job.progress();
        if total == 0 {
            return 0.0;
        }
        done as f32 / total as f32
    }

    pub fn cancel(&mut self) {
        self.job.cancel();
    }

    pub fn finish(self, ru: bool) -> PrintDone {
        let o = self.job.into_outcome();
        let moved = o
            .saved_as()
            .iter()
            .find(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
            .cloned();
        let pdf = moved.or(self.single.filter(|s| s.is_file()));
        PrintDone {
            files: o.files,
            cancelled: o.cancelled,
            clean: o.clean(),
            pdf: pdf.filter(|_| o.files > 0 && !o.cancelled),
            report: o.report(ru),
        }
    }
}

/// Left part of the window: issue settings. They are disabled while printing or while the overwrite prompt is shown.
fn options(ui: &mut egui::Ui, ru: bool, p: &mut Print, lists: &PrintLists, ids: &[i64]) {
    let th = ddc_ui::theme::of(ui.ctx());
    let (sheets, sets) = (&lists.sheets, &lists.sets);
    field_label(ui, &th, tr(ru, "shv.print_range"));
    let range = [
        tr(ru, "shv.current_sheet"),
        tr(ru, "shv.all_sheets"),
        tr(ru, "shv.selected"),
        tr(ru, "shv.sheet_set"),
    ];
    let flags = [active().is_some(), true, true, !sets.is_empty()];
    if let Some(i) = widgets::segmented_enabled(ui, &th, &range, usize::from(p.range), &flags) {
        p.range = i as u8;
    }
    match p.range {
        2 => {
            ui.add_space(space::SM);
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .show(ui, |ui| {
                    for (id, d) in sheets {
                        let mut on = p.chosen.contains(id);
                        if widgets::checkbox(ui, &th, &mut on, &format!("{}  {}", d.number, d.name))
                            .changed()
                        {
                            if on {
                                p.chosen.push(*id);
                            } else {
                                p.chosen.retain(|x| x != id);
                            }
                        }
                    }
                });
        }
        3 => {
            if p.set.is_none() {
                p.set = sets.first().map(|s| s.0);
            }
            ui.add_space(space::SM);
            let names: Vec<&str> = sets.iter().map(|s| s.1.as_str()).collect();
            let mut at = sets.iter().position(|s| Some(s.0) == p.set).unwrap_or(0);
            if widgets::dropdown(ui, &th, "sheet.print.set", &mut at, &names, Some(OPTIONS_W))
                .changed()
            {
                p.set = sets.get(at).map(|s| s.0);
            }
        }
        _ => {}
    }
    ui.add_space(space::LG);
    field_label(ui, &th, tr(ru, "shv.paper_2"));
    let same = tr(ru, "shv.as_on_sheet");
    let mut papers: Vec<&str> = vec![same];
    papers.extend(PAPERS.iter().copied());
    let mut at = paper_index(&p.paper);
    ui.horizontal(|ui| {
        if widgets::dropdown(ui, &th, "sheet.print.paper", &mut at, &papers, None).changed() {
            p.paper = paper_at(at);
        }
        if p.paper.is_some() {
            let items = [tr(ru, "shv.landscape_3"), tr(ru, "shv.portrait_3")];
            let sel = orient_index(p.landscape);
            if let Some(i) = widgets::segmented(ui, &th, &items, sel) {
                p.landscape = orient_is_landscape(i);
            }
        }
    });
    ui.add_space(space::LG);
    field_label(ui, &th, tr(ru, "shv.output"));
    widgets::checkbox(ui, &th, &mut p.pdf, &msg(ru, "sheets.print.pdf", &[]));
    widgets::checkbox(ui, &th, &mut p.svg, &msg(ru, "sheets.print.svg", &[]));
    ui.add_space(space::XS);
    let modes = [
        tr(ru, "shv.one_pdf_a_page_per_sheet"),
        tr(ru, "shv.a_pdf_per_sheet"),
    ];
    let mut mode = usize::from(p.separate);
    ui.add_enabled_ui(p.pdf, |ui| {
        if widgets::dropdown(ui, &th, "sheet.print.mode", &mut mode, &modes, Some(OPTIONS_W))
            .changed()
        {
            p.separate = mode == 1;
        }
    });
    ui.add_space(space::LG);
    field_label(ui, &th, tr(ru, "shv.file_name"));
    ui.add(egui::TextEdit::singleline(&mut p.pattern).desired_width(OPTIONS_W));
    ui.label(
        RichText::new(msg(ru, "sheets.print.hint", &[]))
            .font(th.font(Role::Caption))
            .color(th.sem().text_secondary),
    );
    let first = ids.first().and_then(|i| sheets.iter().find(|s| s.0 == *i));
    let sample = first
        .map(|d| io::file_name(&p.pattern, &d.1.number, &d.1.name, if p.pdf { "pdf" } else { "svg" }))
        .unwrap_or_default();
    ui.add_space(space::MD);
    ui.add(
        egui::Label::new(
            RichText::new(format!(
                "{}: {} · {}",
                tr(ru, "shv.sheets"),
                ids.len(),
                if p.separate || !p.pdf {
                    sample
                } else {
                    tr(ru, "shv.multipage_pdf").to_string()
                }
            ))
            .font(th.font(Role::Caption))
            .color(th.sem().text_secondary),
        )
        .truncate(),
    );
}

/// Width of the options column and of its full-width controls.
const OPTIONS_W: f32 = 400.0;

/// Small caption above a group of controls, with room above the control it names.
fn field_label(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.label(
        RichText::new(text)
            .font(th.font(Role::Label))
            .color(th.sem().text_secondary),
    );
    ui.add_space(space::XS);
}

/// Notice block: tone background, bold title, then the lines. One block per cause.
fn callout(ui: &mut egui::Ui, th: &Theme, n: &Notice) {
    let s = th.sem();
    let fg = n.tone.foreground(s);
    egui::Frame::new()
        .fill(n.tone.background(s))
        .stroke(Stroke::new(ddc_ui::theme::stroke::HAIRLINE, fg))
        .corner_radius(ddc_ui::theme::radius::MD)
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !n.title.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(&n.title)
                            .font(th.font(Role::BodyStrong))
                            .color(fg),
                    )
                    .wrap(),
                );
            }
            for l in &n.lines {
                ui.add(
                    egui::Label::new(
                        RichText::new(l)
                            .font(th.font(Role::Body))
                            .color(s.text_primary),
                    )
                    .wrap(),
                );
            }
        });
}

/// Settings, report and bottom line: buttons, the overwrite prompt, or print progress with cancel.
fn controls(
    ui: &mut egui::Ui,
    ru: bool,
    p: &mut Print,
    lists: &PrintLists,
    ids: &[i64],
    keep: &mut bool,
    go: &mut bool,
) {
    let th = ddc_ui::theme::of(ui.ctx());
    let idle = p.job.is_none() && p.confirm.is_none();
    ui.add_enabled_ui(idle, |ui| options(ui, ru, p, lists, ids));
    if !p.msg.is_empty() {
        ui.add_space(space::MD);
        egui::ScrollArea::vertical()
            .id_salt("sheet.print.report")
            .max_height(150.0)
            .show(ui, |ui| {
                let n = Notice {
                    tone: Tone::Warning,
                    title: String::new(),
                    lines: p.msg.lines().map(str::to_string).collect(),
                };
                callout(ui, &th, &n);
            });
    }
    // the footer draws its own rule
    ui.add_space(space::MD);
    if let Some(job) = &mut p.job {
        let (done, total, current) = job.progress();
        let text = msg(
            ru,
            "sheets.print.progress",
            &[
                ("n", &done.to_string()),
                ("of", &total.to_string()),
                ("sheet", current),
            ],
        );
        let part = done as f32 / total.max(1) as f32;
        widgets::progress(ui, &th, Some(part), &text);
        ui.add_space(space::SM);
        let label = msg(ru, "sheets.print.cancel", &[]);
        let off = job.is_cancelling();
        if ui
            .add_enabled_ui(!off, |ui| Button::secondary(&label).show(ui, &th))
            .inner
            .clicked()
        {
            job.cancel();
        }
        return;
    }
    if let Some(c) = &p.confirm {
        let n = c.names.len().to_string();
        let note = Notice {
            tone: Tone::Warning,
            title: msg(ru, "sheets.print.overwrite", &[("n", &n)]),
            lines: vec![],
        };
        callout(ui, &th, &note);
        ui.add_space(space::SM);
        egui::ScrollArea::vertical()
            .id_salt("sheet.print.replace")
            .max_height(90.0)
            .show(ui, |ui| {
                for name in &c.names {
                    kit::elided_weak(ui, name);
                }
            });
        ui.add_space(space::SM);
        let (mut replace, mut back) = (false, false);
        kit::dialog_footer(ui, |ui| {
            replace = kit::primary_click(ui, msg(ru, "sheets.print.replace", &[]), None);
            back = kit::cancel_button(ui, msg(ru, "sheets.cancel", &[])).clicked();
        });
        if back {
            p.confirm = None;
        } else if replace {
            if let Some(c) = p.confirm.take() {
                p.start(c.plan);
            }
        }
        return;
    }
    // the button names what comes out: files, not paper
    kit::dialog_footer(ui, |ui| {
        let ready = !ids.is_empty() && (p.pdf || p.svg);
        let label = super::msg_n(ru, "sheet_tools.print.save", ids.len() as u64);
        if kit::primary_button(ui, label, ready).clicked() {
            *go = true;
        }
        if kit::cancel_button(ui, tr(ru, "shv.close")).clicked() {
            *keep = false;
        }
    });
}

/// Right part of the window: the page as on paper, paging through the issue.
fn preview(ui: &mut egui::Ui, ed: &Editor, ru: bool, p: &mut Print, ids: &[i64]) {
    let th = ddc_ui::theme::of(ui.ctx());
    let s = th.sem();
    let pages = ids.len();
    p.page = p.page.min(pages.saturating_sub(1));
    let typing = ui.ctx().wants_keyboard_input();
    let (prev_key, next_key) = if typing {
        (false, false)
    } else {
        ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
            )
        })
    };
    let mut fit = false;
    ui.horizontal(|ui| {
        let tip = |k: &str| msg(ru, k, &[]);
        if widgets::glyph_button(ui, "arrow-left", p.page > 0, &tip("sheets.print.prev")).clicked()
            || (prev_key && p.page > 0)
        {
            p.page = step_page(p.page, pages, -1);
        }
        let (n, of) = ((p.page + 1).to_string(), pages.to_string());
        ui.label(
            RichText::new(msg(ru, "sheets.print.page", &[("n", &n), ("of", &of)]))
                .font(th.font(Role::Body))
                .color(s.text_primary),
        );
        let more = p.page + 1 < pages;
        if widgets::glyph_button(ui, "arrow-right", more, &tip("sheets.print.next")).clicked()
            || (next_key && more)
        {
            p.page = step_page(p.page, pages, 1);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            fit = widgets::glyph_button(ui, "zoom-fit", true, &tip("sheets.print.fit")).clicked();
            ui.label(
                RichText::new(format!("{:.0} %", p.zoom * 100.0))
                    .font(th.font(Role::Caption))
                    .color(s.text_secondary),
            );
        });
    });
    ui.add_space(space::SM);
    let height = (ui.ctx().screen_rect().height() * 0.55).clamp(PREVIEW_MIN_H, PREVIEW_MAX_H);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click_and_drag(),
    );
    ui.painter().rect_filled(rect, radius_md(), s.bg_sunken);
    let Some(&sheet) = ids.get(p.page) else {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            msg(ru, "sheets.print.no_sheets", &[]),
            th.font(Role::Body),
            s.text_secondary,
        );
        return;
    };
    let key = PreviewKey {
        sheet,
        page: p.page,
        pages,
        revision: ed.revision,
        units: ed.tool.units,
        paper: p.paper.clone(),
        landscape: p.landscape,
        ru,
    };
    p.preview.drive(&key, |_| start_build(ed, &key));
    if p.preview.is_running() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }
    let mut note = None;
    match p.preview.view(&key) {
        Shown::Ready(page) => {
            let area = rect.size();
            if resp.hovered() {
                let wheel = ui.input(|i| i.smooth_scroll_delta.y);
                let pinch = ui.input(|i| i.zoom_delta());
                let factor = pinch * (wheel * 0.0015).exp();
                if (factor - 1.0).abs() > 1e-4 {
                    let at = ui
                        .input(|i| i.pointer.hover_pos())
                        .map_or(Vec2::ZERO, |q| q - rect.center());
                    (p.zoom, p.pan) = zoom_about(p.zoom, p.pan, at, factor);
                }
                ui.ctx().set_cursor_icon(if resp.dragged() {
                    egui::CursorIcon::Grabbing
                } else {
                    egui::CursorIcon::Grab
                });
            }
            if resp.dragged() {
                p.pan += resp.drag_delta();
            }
            if fit || resp.double_clicked() {
                (p.zoom, p.pan) = (1.0, Vec2::ZERO);
            }
            p.zoom = p.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
            let at = preview_fit(page.size, area, p.zoom, p.pan);
            p.pan = at.center().to_vec2() - area / 2.0;
            let painter = ui.painter_at(rect);
            paint_page(&painter, &th, at.translate(rect.min.to_vec2()), page);
            note = notice(ru, None, &page.warnings);
        }
        Shown::Failed(e) => {
            note = notice(ru, Some(e), &[]);
            skeleton(ui, &th, rect, false);
        }
        Shown::Building => skeleton(ui, &th, rect, true),
        Shown::Stopped => skeleton(ui, &th, rect, false),
    }
    ui.add_space(space::SM);
    match p.preview.view(&key) {
        Shown::Building => {
            widgets::progress(ui, &th, None, &msg(ru, "sheets.print.building", &[]));
            ui.add_space(space::SM);
            if Button::secondary(&msg(ru, "sheets.print.stop_build", &[]))
                .show(ui, &th)
                .clicked()
            {
                p.preview.stop();
            }
        }
        Shown::Stopped => {
            let stopped = Notice {
                tone: Tone::Neutral,
                title: msg(ru, "sheets.print.build_stopped", &[]),
                lines: vec![],
            };
            callout(ui, &th, &stopped);
            ui.add_space(space::SM);
            if Button::secondary(&msg(ru, "sheets.print.retry", &[]))
                .show(ui, &th)
                .clicked()
            {
                p.preview.retry();
            }
        }
        Shown::Failed(_) => {
            if let Some(n) = &note {
                callout(ui, &th, n);
            }
            ui.add_space(space::SM);
            if Button::secondary(&msg(ru, "sheets.print.retry", &[]))
                .show(ui, &th)
                .clicked()
            {
                p.preview.retry();
            }
        }
        Shown::Ready(_) => {
            if let Some(n) = &note {
                callout(ui, &th, n);
            }
        }
    }
}

const PREVIEW_MIN_H: f32 = 420.0;
const PREVIEW_MAX_H: f32 = 640.0;

fn radius_md() -> f32 {
    ddc_ui::theme::radius::MD
}

/// Placeholder page while the real one is built, stopped or failed.
fn skeleton(ui: &egui::Ui, th: &Theme, area: Rect, busy: bool) {
    let s = th.sem();
    let page = preview_fit(Vec2::new(297.0, 210.0), area.size(), 1.0, Vec2::ZERO)
        .translate(area.min.to_vec2());
    let p = ui.painter_at(area);
    p.rect_filled(page, 0.0, s.bg_panel);
    p.rect_stroke(
        page,
        0.0,
        Stroke::new(ddc_ui::theme::stroke::HAIRLINE, s.border_subtle),
        egui::StrokeKind::Inside,
    );
    // grey bars stand for the frame and title block; they pulse unless motion is reduced
    let pulse = if busy && !th.reduced_motion {
        ui.ctx().request_repaint();
        0.5 + 0.5 * (ui.input(|i| i.time) as f32 * 3.0).sin().abs()
    } else {
        0.6
    };
    let bar = s.border_subtle.gamma_multiply(pulse);
    let u = page.width() / 100.0;
    for (x, y, w, h) in [
        (6.0, 8.0, 50.0, 4.0),
        (6.0, 16.0, 70.0, 3.0),
        (6.0, 22.0, 60.0, 3.0),
        (62.0, 80.0, 32.0, 10.0),
    ] {
        let r = Rect::from_min_size(
            page.min + Vec2::new(x * u, y * u),
            Vec2::new(w * u, h * u),
        );
        p.rect_filled(r, radius_md(), bar);
    }
}

/// Starts the page build: only a read-only copy handle is taken here (schedule tables are small),
/// composing and laying out the page happen on the worker.
fn start_build(ed: &Editor, key: &PreviewKey) -> Worker<PreviewPage> {
    let req = build_request(ed, key);
    match req.file.clone() {
        Some(file) => Worker::spawn(move |ctl| compose_page(&file, req, ctl)),
        None => {
            // an in-memory model (screenshots, tests) has no file a worker could open: compose
            // here instead of copying the whole database on the UI thread
            let page = compose_on(&ed.model, &req);
            Worker::spawn(move |_| page)
        }
    }
}

struct BuildReq {
    /// The model's database file; `None` for an in-memory model.
    file: Option<std::path::PathBuf>,
    sheet: i64,
    page: usize,
    pages: usize,
    paper: Option<(String, bool)>,
    units: LengthUnits,
    /// Document language of the sheet words.
    lang: Lang,
    schedules: std::collections::HashMap<String, io::Table>,
}

fn build_request(ed: &Editor, key: &PreviewKey) -> BuildReq {
    let file = ed
        .model
        .sql()
        .path()
        .filter(|f| !f.is_empty())
        .map(std::path::PathBuf::from);
    let mut schedules = std::collections::HashMap::new();
    for (_, place) in ed.model.schedule_places(key.sheet).unwrap_or_default() {
        if let Some(t) = schedule_table(ed, &place.schedule) {
            schedules.insert(place.schedule, t);
        }
    }
    BuildReq {
        file,
        sheet: key.sheet,
        page: key.page,
        pages: key.pages,
        paper: key.paper.clone().map(|s| (s, key.landscape)),
        units: lu(key.units),
        lang: crate::doc_lang::doc_lang(&ed.model),
        schedules,
    }
}

/// Worker body: open a read-only snapshot, compose the sheet, lay it out. Checks for Cancel
/// between steps.
fn compose_page(
    file: &std::path::Path,
    req: BuildReq,
    ctl: &std::sync::Arc<crate::jobs::Progress>,
) -> Result<PreviewPage, String> {
    let model = ddc_model::Model::open_work_snapshot(file).map_err(|e| e.to_string())?;
    if ctl.is_cancelled() {
        return Err(crate::jobs::CANCELLED.to_string());
    }
    let page = compose_on(&model, &req)?;
    if ctl.is_cancelled() {
        return Err(crate::jobs::CANCELLED.to_string());
    }
    Ok(page)
}

fn compose_on(model: &ddc_model::Model, req: &BuildReq) -> Result<PreviewPage, String> {
    let sched = |k: &str| req.schedules.get(k).cloned();
    let text = |k: &str| msg_in(req.lang, k, &[]);
    let paper = req.paper.as_ref().map(|(s, l)| (s.as_str(), *l));
    let ctx = Ctx {
        units: req.units,
        schedules: &sched,
        index: Some((req.page + 1, req.pages)),
        paper,
        text: &text,
        refs: None,
    };
    let c = io::print_composed(model, req.sheet, &ctx).map_err(|e| e.0)?;
    Ok(build_page(c))
}

/// Page layout in paper millimetres. Black on white whatever the theme: it shows the paper, not
/// the screen.
fn build_page(c: Composed) -> PreviewPage {
    let Composed {
        drawing: d,
        warnings,
        ..
    } = c;
    let k = 1.0f32;
    let size = Vec2::new(d.width as f32 * k, d.height as f32 * k);
    let sp = |q: &[f64; 2]| Pos2::new(q[0] as f32 * k, size.y - q[1] as f32 * k);
    let mut shapes = Vec::with_capacity(d.marks.len());
    let mut texts = vec![];
    for (_, m) in d.marks {
        match m {
            Mark::Line {
                pts,
                closed,
                weight,
                ..
            } => {
                let mut v: Vec<Pos2> = pts.iter().map(sp).collect();
                if closed {
                    if let Some(first) = v.first().copied() {
                        v.push(first);
                    }
                }
                shapes.push(Prim::Line {
                    pts: v,
                    width: weight as f32 * k,
                });
            }
            Mark::Fill { rings, gray, .. } => {
                let rs: Vec<Vec<Pos2>> = rings.iter().map(|r| r.iter().map(sp).collect()).collect();
                let col = Color32::from_gray(gray);
                let mesh = crate::section_view::even_odd_mesh(&rs, col);
                if !mesh.is_empty() {
                    shapes.push(Prim::Fill(mesh));
                }
            }
            Mark::Text { pos, h, rot, s, .. } => {
                let px = h as f32 * k;
                if px < 0.05 {
                    continue;
                }
                let angle = -(rot as f32).to_radians();
                let asc = px * 0.9;
                let at = sp(&pos).to_vec2() + Vec2::new(asc * angle.sin(), -asc * angle.cos());
                texts.push(PaperText { at, px, angle, s });
            }
        }
    }
    PreviewPage {
        size,
        prims: shapes,
        texts,
        warnings,
    }
}

/// Printable margin on paper, mm: the hairline inside the page edge.
const PRINT_MARGIN_MM: f32 = 5.0;

/// Paints the page card (shadow, white sheet, margin hairline) into `page`, the screen rectangle
/// of the whole sheet; the drawing is scaled from millimetres to that rectangle.
fn paint_page(painter: &egui::Painter, th: &Theme, page: Rect, p: &PreviewPage) {
    let ink = Color32::BLACK; // design-lint: data
    let k = page.width() / p.size.x.max(1e-3);
    painter.add(th.shadow(2).as_shape(page, 0.0));
    painter.rect_filled(page, 0.0, Color32::WHITE); // design-lint: data
    let at = |q: Pos2| page.min + q.to_vec2() * k;
    for prim in &p.prims {
        match prim {
            Prim::Line { pts, width } => {
                let v: Vec<Pos2> = pts.iter().map(|q| at(*q)).collect();
                // a thin line stays visible while the difference between weights stays readable
                let w = (width * k).max(0.4);
                painter.add(egui::Shape::line(v, Stroke::new(w, ink)));
            }
            Prim::Fill(mesh) => {
                let mut m = mesh.clone();
                for v in &mut m.vertices {
                    v.pos = at(v.pos);
                }
                painter.add(egui::Shape::mesh(m));
            }
        }
    }
    // egui caches text layouts itself; keeping one here is unsafe, the font atlas can change
    for t in &p.texts {
        let px = t.px * k;
        if px < 2.0 {
            continue;
        }
        let g = painter.layout_no_wrap(t.s.clone(), egui::FontId::proportional(px), ink);
        let origin = at(Pos2::ZERO + t.at);
        painter.add(egui::epaint::TextShape::new(origin, g, ink).with_angle(t.angle));
    }
    let hair = Stroke::new(ddc_ui::theme::stroke::HAIRLINE, th.sem().border_subtle);
    let inset = (PRINT_MARGIN_MM * k).min(page.width().min(page.height()) / 4.0);
    painter.rect_stroke(page.shrink(inset), 0.0, hair, egui::StrokeKind::Inside);
    painter.rect_stroke(page, 0.0, hair, egui::StrokeKind::Inside);
}

fn finished(app: &mut EditorApp, ru: bool, p: &mut Print, o: &Outcome, keep: &mut bool) {
    let level = if o.cancelled {
        Level::Info
    } else if o.files == 0 {
        Level::Error
    } else if o.clean() {
        Level::Success
    } else {
        Level::Warning
    };
    app.sh.toasts.push(level, o.headline(ru), None);
    if o.clean() && !o.cancelled && o.files > 0 {
        *keep = false;
    } else {
        p.msg = o.report(ru);
    }
}

pub(super) fn print_window(
    ctx: &egui::Context,
    app: &mut EditorApp,
    ru: bool,
    p: &mut Print,
) -> bool {
    let mut keep = true;
    let mut open = true;
    let mut go = false;
    if let Some(job) = &mut p.job {
        if job.step(&app.ed, STEP_BUDGET) {
            if let Some(job) = p.job.take() {
                finished(app, ru, p, &job.into_outcome(), &mut keep);
            }
        } else {
            ctx.request_repaint();
        }
    }
    let ed = &app.ed;
    let lists = p.lists(ed);
    let ids = p.pick(&lists.sheets, &lists.sets);
    kit::Dialog::new(
        "sheet_print",
        tr(ru, "shv.print_sheets"),
        kit::DialogSize::Form,
    )
    .size([OPTIONS_W + 640.0, 0.0])
    .open(&mut open)
    .show(ctx, |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(OPTIONS_W);
                controls(ui, ru, p, &lists, &ids, &mut keep, &mut go);
            });
            ui.add_space(space::MD);
            ui.separator();
            ui.add_space(space::MD);
            ui.vertical(|ui| preview(ui, ed, ru, p, &ids));
        });
    });
    // window closed during printing: cancel; the window goes away once the thread has cleaned up
    if !(keep && open) {
        p.preview.abandon();
        if let Some(job) = &mut p.job {
            job.cancel();
            return true;
        }
    }
    if go {
        let target = if p.separate || !p.pdf {
            crate::file_pick::Dlg::new().pick_folder()
        } else {
            let name = match ids.as_slice() {
                [one] => app
                    .ed
                    .model
                    .sheet(*one)
                    .map(|d| io::file_name(&p.pattern, &d.number, &d.name, "pdf"))
                    .unwrap_or_else(|_| "sheet.pdf".into()),
                _ => msg(ru, "sheets.print.default_file", &[]),
            };
            crate::file_pick::Dlg::new()
                .add_filter("PDF", &["pdf"])
                .set_file_name(name)
                .save_file()
        };
        if let Some(t) = target {
            let plan = plan_outputs(&app.ed, p, &ids, &t);
            let existing = plan.existing();
            if existing.is_empty() {
                p.start(plan);
            } else {
                let names = existing
                    .iter()
                    .filter_map(|f| f.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect();
                p.confirm = Some(Confirm { plan, names });
            }
        }
    }
    keep && open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Poll;

    #[test]
    fn in_memory_model_previews_without_a_file_copy() {
        let mut ed = Editor::new_in_memory().unwrap();
        let sheet = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let key = PreviewKey {
            sheet,
            page: 0,
            pages: 1,
            revision: ed.revision,
            units: Units::Metric,
            paper: None,
            landscape: true,
            ru: false,
        };
        let req = build_request(&ed, &key);
        assert!(req.file.is_none(), "nothing to open on a worker");
        let mut w = start_build(&ed, &key);
        let end = web_time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match w.poll() {
                Poll::Running if web_time::Instant::now() < end => std::thread::yield_now(),
                Poll::Done(page) => {
                    assert!(page.size.x > 0.0 && page.size.y > 0.0);
                    break;
                }
                Poll::Failed(e) => panic!("preview failed: {e}"),
                _ => panic!("preview not built in time or cancelled"),
            }
        }
    }
}
