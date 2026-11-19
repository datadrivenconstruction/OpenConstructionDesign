//! Site windows: options of the active terrain tool; "Site" holds the plot properties (name, true-north
//! angle, project base point, latitude/longitude), terrain properties (name, thickness,
//! material), contour interval and the site report (cut, fill, area).

use super::{Tool, MATERIALS};
use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use crate::units::{Units, FOOT};
use super::units_ui;
use ddc_io::topo::{
    preview_xyz, ColumnOrder, DxfLayer, DxfTopoOptions, SourceKind, Span, SurveyUnit, TopoSource,
    UnitGuess, XyzOptions,
};
use ddc_ui::i18n::{Catalog, Lang};
use ddc_ui::theme::Theme;
use ddc_ui::widgets::{self, Button, NumberField, NumberFormat, TextField};
use std::path::{Path, PathBuf};

/// Unit lengths are shown in; the model keeps metres.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LenUnit {
    Mm,
    #[allow(dead_code)] // metre display: kept for a project setting and for the round-trip tests
    M,
    Ft,
}

impl LenUnit {
    /// Follows the project unit setting.
    pub fn of(u: Units) -> Self {
        match u {
            Units::Metric => Self::Mm,
            Units::Imperial => Self::Ft,
        }
    }
    /// Displayed value per metre.
    pub fn scale(self) -> f64 {
        match self {
            Self::Mm => 1000.0,
            Self::M => 1.0,
            Self::Ft => 1.0 / FOOT,
        }
    }
    pub fn decimals(self) -> usize {
        match self {
            Self::Mm => 1,
            Self::M | Self::Ft => 3,
        }
    }
    pub fn unit_key(self) -> &'static str {
        match self {
            Self::Mm => "topo.unit.mm",
            Self::M => "topo.unit.metre",
            Self::Ft => "topo.unit.ft",
        }
    }
    pub fn to_disp(self, m: f64) -> f64 {
        m * self.scale()
    }
    pub fn from_disp(self, v: f64) -> f64 {
        v / self.scale()
    }
    /// Unit name in an explicit language (the UI language is not touched).
    pub fn unit_text(self, lang: Lang) -> String {
        crate::tr_in(lang, self.unit_key(), &[])
    }
    /// Number as the length field shows it: locale separators, no unit.
    pub fn format(self, m: f64, lang: Lang) -> String {
        widgets::field::format_number(self.to_disp(m), self.decimals(), NumberFormat::for_lang(lang))
    }
}

/// Why Import is off, as an i18n key; `None` when it can run.
pub fn import_block(kind: SourceKind, ticked: usize) -> Option<&'static str> {
    match kind {
        SourceKind::Dxf if ticked == 0 => Some("topo.import.need_layer"),
        _ => None,
    }
}

/// State of the "All layers" box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllState {
    All,
    None,
    Mixed,
}

pub fn all_state(layers: &[(DxfLayer, bool)]) -> AllState {
    let on = layers.iter().filter(|l| l.1).count();
    if on == 0 {
        AllState::None
    } else if on == layers.len() {
        AllState::All
    } else {
        AllState::Mixed
    }
}

/// What the surface section of the site window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceState {
    Empty,
    Loading,
    Error,
    Ready,
}

pub fn surface_state(surfaces: usize, built: bool, has_error: bool) -> SurfaceState {
    if surfaces == 0 {
        SurfaceState::Empty
    } else if !built {
        SurfaceState::Loading
    } else if has_error {
        SurfaceState::Error
    } else {
        SurfaceState::Ready
    }
}

/// One numeric row: the value is shown as `value * scale`.
struct NumSpec<'a> {
    key: &'a str,
    scale: f64,
    decimals: usize,
    step: f64,
    min: f64,
    max: f64,
    unit: String,
}

impl<'a> NumSpec<'a> {
    /// Length in model metres, shown in the project unit.
    fn len(cat: &Catalog, key: &'a str, lu: LenUnit, step_m: f64, min_m: f64) -> Self {
        Self {
            key,
            scale: lu.scale(),
            decimals: lu.decimals(),
            step: lu.to_disp(step_m),
            min: lu.to_disp(min_m),
            max: f64::INFINITY,
            unit: cat.t(lu.unit_key()),
        }
    }
    /// Plain number with a fixed unit text.
    fn plain(key: &'a str, decimals: usize, step: f64, unit: String) -> Self {
        Self {
            key,
            scale: 1.0,
            decimals,
            step,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            unit,
        }
    }
    fn range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self
    }
}

fn num_row(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &NumSpec, v: &mut f64) {
    let label = cat.t(s.key);
    let nf = NumberFormat::for_lang(cat.lang);
    kit::labeled_row_id(ui, &label, |ui, lid| {
        let mut d = *v * s.scale;
        let before = d;
        NumberField::new(("topo.num", s.key), &mut d, nf)
            .labelled_by(lid, &label)
            .decimals(s.decimals)
            .step(s.step)
            .range(s.min, s.max)
            .unit(&s.unit)
            .width(FIELD_W)
            .show(ui, th);
        if d != before {
            *v = d / s.scale;
        }
    });
}

fn count_row(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, key: &str, v: &mut u32, lo: u32, hi: u32) {
    let spec = NumSpec::plain(key, 0, 1.0, String::new()).range(f64::from(lo), f64::from(hi));
    let mut f = f64::from(*v);
    num_row(ui, th, cat, &spec, &mut f);
    *v = f.round().clamp(f64::from(lo), f64::from(hi)) as u32;
}

fn text_row(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, key: &str, buf: &mut String) {
    let label = cat.t(key);
    kit::labeled_row_id(ui, &label, |ui, lid| {
        TextField::new(buf)
            .labelled_by(lid, &label)
            .id_salt(("topo.text", key))
            .show(ui, th);
    });
}

/// Dropdown row over translated items; true when the selection moved.
pub(super) fn pick_row(ui: &mut egui::Ui, th: &Theme, label: &str, salt: &str, sel: &mut usize, items: &[String]) -> bool {
    kit::labeled_row_id(ui, label, |ui, _| {
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        widgets::dropdown(ui, th, ("topo.pick", salt), sel, &refs, None).changed()
    })
}

/// Right-aligned primary Apply; true when clicked.
fn apply_row(ui: &mut egui::Ui, th: &Theme, label: &str) -> bool {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        Button::primary(label).show(ui, th).clicked()
    })
    .inner
}

/// Width of a number field in the topo windows.
const FIELD_W: f32 = 120.0;
/// Smallest toposolid or pad thickness, m.
const MIN_THICK: f64 = 0.01;

/// Pending plot-property edits until "Apply".
#[derive(Clone, Debug, PartialEq)]
pub struct SiteBuf {
    pub name: String,
    pub north: f64,
    pub base: [f64; 3],
    pub lat: f64,
    pub lon: f64,
    pub crs_code: String,
    pub crs_name: String,
    pub crs_vertical: String,
}

/// Pending terrain-property edits until "Apply".
#[derive(Clone, Debug, PartialEq)]
pub struct PropsBuf {
    pub id: i64,
    pub name: String,
    pub thickness: f64,
    pub material: String,
}

/// Terrain windows for one frame (called from `EditorApp::ui`).
pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    // terrain strings (status, north label) use the shell language, even without an active tool
    app.ed.topo.lang = Some(app.sh.cat.lang);
    if app.ed.topo.import.is_some() {
        import_window(app, ctx);
    }
    if app.ed.topo.tool.is_some() {
        tool_window(app, ctx);
    }
    if app.ed.topo.window_open {
        site_window(app, ctx);
    }
}

/// The tool panel opens at the right edge, over the properties dock, not over the drawing area
/// where survey points are clicked (a click on the panel never reaches the canvas).
pub fn tool_pos(screen: egui::Rect) -> egui::Pos2 {
    let w = ddc_ui::theme::layout::DIALOG_HUD;
    egui::pos2((screen.right() - w - 4.0).max(screen.left()), screen.top() + 200.0)
}

fn tool_window(app: &mut EditorApp, ctx: &egui::Context) {
    let cat = &app.sh.cat;
    let ed = &mut app.ed;
    let Some(tool) = ed.topo.tool else {
        return;
    };
    let hint = ed.topo.hint().unwrap_or_default();
    let th = ddc_ui::theme::of(ctx);
    let lu = LenUnit::of(ed.tool.units);
    let title = cat.t(&format!("topo.tool.{}", tool.key()));
    let mut close = false;
    kit::Dialog::new("topo_tool", title, kit::DialogSize::Hud)
        .first_pos(tool_pos(ctx.screen_rect()))
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(hint).weak());
            let o = &mut ed.topo.opts;
            match tool {
                Tool::Points => {
                    let s = NumSpec::len(cat, "topo.opt.elevation", lu, 0.05, f64::NEG_INFINITY);
                    num_row(ui, &th, cat, &s, &mut o.elevation);
                    let s = NumSpec::len(cat, "topo.opt.thickness", lu, 0.05, MIN_THICK);
                    num_row(ui, &th, cat, &s, &mut o.thickness);
                    widgets::checkbox(ui, &th, &mut o.add_to_active, &cat.t("topo.opt.add_to_active"));
                }
                Tool::Region => {
                    let items: Vec<String> = MATERIALS
                        .iter()
                        .map(|mk| cat.t(&format!("topo.mat.{mk}")))
                        .collect();
                    o.material = o.material.min(MATERIALS.len() - 1);
                    pick_row(ui, &th, &cat.t("topo.opt.material"), "mat", &mut o.material, &items);
                }
                Tool::Pad => {
                    let s = NumSpec::len(cat, "topo.opt.pad_elevation", lu, 0.05, f64::NEG_INFINITY);
                    num_row(ui, &th, cat, &s, &mut o.pad_elevation);
                    let s = NumSpec::plain("topo.opt.slope", 2, 0.05, String::new()).range(0.0, f64::INFINITY);
                    num_row(ui, &th, cat, &s, &mut o.slope);
                    let s = NumSpec::len(cat, "topo.opt.pad_thickness", lu, 0.01, MIN_THICK);
                    num_row(ui, &th, cat, &s, &mut o.pad_thickness);
                    o.slope = o.slope.max(0.0);
                    o.pad_thickness = o.pad_thickness.max(MIN_THICK);
                }
                Tool::Smooth => count_row(ui, &th, cat, "topo.opt.passes", &mut o.passes, 1, 50),
                Tool::Flatten => {
                    let s = NumSpec::len(cat, "topo.opt.flat_z", lu, 0.05, f64::NEG_INFINITY);
                    num_row(ui, &th, cat, &s, &mut o.flat_z);
                }
                Tool::Boundary => {}
                Tool::Edit => {
                    ui.label(cat.t("topo.opt.edit_help"));
                }
            }
            o.thickness = o.thickness.max(0.01);
            // Enter belongs to the tool on the canvas
            kit::dialog_footer(ui, |ui| {
                if kit::primary_button(ui, cat.t("topo.btn.finish"), true).clicked() {
                    close = true;
                }
            });
        });
    if close {
        super::disarm(&mut app.ed);
    }
}

fn site_window(app: &mut EditorApp, ctx: &egui::Context) {
    super::refresh(&mut app.ed);
    let cat = &app.sh.cat;
    let ed = &mut app.ed;
    if ed.topo.site_buf.is_none() {
        let s = ed.model.site().ok();
        ed.topo.site_buf = Some(match s {
            Some(s) => SiteBuf {
                name: s.name,
                north: s.north_deg,
                base: [s.base.0, s.base.1, s.base.2],
                lat: s.latitude,
                lon: s.longitude,
                crs_code: s.crs_code,
                crs_name: s.crs_name,
                crs_vertical: s.crs_vertical,
            },
            None => SiteBuf {
                name: String::new(),
                north: 0.0,
                base: [0.0; 3],
                lat: 0.0,
                lon: 0.0,
                crs_code: String::new(),
                crs_name: String::new(),
                crs_vertical: String::new(),
            },
        });
    }
    let act = super::active(ed);
    let list = ed.model.toposolids().unwrap_or_default();
    if ed.topo.props_buf.as_ref().map(|p| Some(p.id)) != Some(act) {
        ed.topo.props_buf = act.and_then(|id| {
            ed.model.toposolid(id).ok().map(|s| PropsBuf {
                id,
                name: s.name,
                thickness: s.thickness,
                material: s.material,
            })
        });
    }
    let th = ddc_ui::theme::of(ctx);
    let lu = LenUnit::of(ed.tool.units);
    let deg = cat.t("topo.unit.deg");
    let mut open = true;
    let mut site_apply = false;
    let mut props_apply = false;
    let mut choose: Option<i64> = None;
    let mut del_pad: Option<i64> = None;
    let mut del_topo = false;
    kit::Dialog::new("topo_site", cat.t("topo.win.site"), kit::DialogSize::Tool)
        .size([360.0, 560.0])
        .min_size([320.0, 240.0])
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| {
            kit::section_title(ui, cat.t("topo.sec.site"));
            if let Some(b) = &mut ed.topo.site_buf {
                text_row(ui, &th, cat, "topo.site.name", &mut b.name);
                let s = NumSpec::plain("topo.site.north", 1, 0.1, deg.clone());
                num_row(ui, &th, cat, &s, &mut b.north);
                let s = NumSpec::len(cat, "topo.site.east", lu, 0.1, f64::NEG_INFINITY);
                num_row(ui, &th, cat, &s, &mut b.base[0]);
                let s = NumSpec::len(cat, "topo.site.northing", lu, 0.1, f64::NEG_INFINITY);
                num_row(ui, &th, cat, &s, &mut b.base[1]);
                let s = NumSpec::len(cat, "topo.site.elevation", lu, 0.01, f64::NEG_INFINITY);
                num_row(ui, &th, cat, &s, &mut b.base[2]);
                let s = NumSpec::plain("topo.site.lat", 4, 0.0001, deg.clone()).range(-90.0, 90.0);
                num_row(ui, &th, cat, &s, &mut b.lat);
                let s = NumSpec::plain("topo.site.lon", 4, 0.0001, deg.clone()).range(-180.0, 180.0);
                num_row(ui, &th, cat, &s, &mut b.lon);
                b.lat = b.lat.clamp(-90.0, 90.0);
                b.lon = b.lon.clamp(-180.0, 180.0);
                text_row(ui, &th, cat, "topo.site.crs_code", &mut b.crs_code);
                text_row(ui, &th, cat, "topo.site.crs_name", &mut b.crs_name);
                text_row(ui, &th, cat, "topo.site.crs_vertical", &mut b.crs_vertical);
                if !ddc_model::crs_code_valid(b.crs_code.trim())
                    || !ddc_model::crs_code_valid(b.crs_vertical.trim())
                {
                    kit::empty_note(ui, cat.t("topo.site.crs_bad"), None);
                }
                if let Some((code, name)) = ddc_model::suggest_utm(b.lat, b.lon) {
                    let hint = cat.t("topo.site.crs_suggest").replace("{code}", &code);
                    if Button::secondary(&hint).show(ui, &th).clicked() {
                        b.crs_code = code;
                        b.crs_name = name;
                    }
                }
            }
            site_apply = apply_row(ui, &th, &cat.t("topo.btn.apply"));
            ui.separator();
            kit::section_title(ui, cat.t("topo.sec.topo"));
            let item = ed
                .topo
                .cache
                .as_ref()
                .and_then(|c| c.items.iter().find(|i| Some(i.id) == act));
            let state = surface_state(
                list.len(),
                act.is_none() || item.is_some(),
                item.is_some_and(|i| i.error.is_some()),
            );
            if state == SurfaceState::Empty {
                kit::empty_note(ui, cat.t("topo.err.none"), None);
                return;
            }
            let names: Vec<String> = list.iter().map(|t| t.1.clone()).collect();
            let mut sel = list.iter().position(|t| Some(t.0) == act).unwrap_or(0);
            if pick_row(ui, &th, &cat.t("topo.row.active"), "active", &mut sel, &names) {
                choose = list.get(sel).map(|t| t.0);
            }
            if let Some(p) = &mut ed.topo.props_buf {
                text_row(ui, &th, cat, "topo.prop.name", &mut p.name);
                let s = NumSpec::len(cat, "topo.prop.thickness", lu, 0.05, MIN_THICK);
                num_row(ui, &th, cat, &s, &mut p.thickness);
                p.thickness = p.thickness.max(MIN_THICK);
                text_row(ui, &th, cat, "topo.prop.material", &mut p.material);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        props_apply = Button::primary(&cat.t("topo.btn.apply")).show(ui, &th).clicked();
                        del_topo = kit::danger_button(ui, cat.t("topo.btn.delete"), true).clicked();
                    });
                });
            }
            ui.separator();
            let o = &mut ed.topo.opts;
            let s = NumSpec::len(cat, "topo.opt.interval", lu, 0.05, MIN_THICK);
            num_row(ui, &th, cat, &s, &mut o.interval);
            o.interval = o.interval.max(MIN_THICK);
            count_row(ui, &th, cat, "topo.opt.major", &mut o.major, 1, 50);
            let mut show = !ed.topo.hide_contours;
            if widgets::checkbox(ui, &th, &mut show, &cat.t("topo.opt.show_contours")).changed() {
                ed.topo.hide_contours = !show;
            }
            if state == SurfaceState::Loading {
                kit::hint(ui, cat.t("topo.info.loading"));
                return;
            }
            if let Some(it) = item {
                if let Some(e) = &it.error {
                    kit::error_note(ui, e);
                }
                if !it.warnings.is_empty() {
                    let title = cat
                        .t("topo.info.warn_n")
                        .replace("{n}", &it.warnings.len().to_string());
                    // collapsed until the user opens it
                    let sid = ui.id().with(("ddc.section", title.as_str()));
                    ui.data_mut(|d| {
                        d.get_persisted_mut_or(sid, false);
                    });
                    widgets::section(ui, &th, &title, |ui| {
                        kit::warn_note(ui, it.warnings.join("\n"));
                    });
                }
                kit::props(ui, "topo.info", |ui| {
                    kit::kv(ui, &cat.t("topo.info.points_label"), it.points.len().to_string());
                });
                if !it.pads.is_empty() {
                    ui.separator();
                    kit::section_title(ui, cat.t("topo.sec.pads"));
                }
                for (pid, name, _) in &it.pads {
                    let rep = it.reports.iter().find(|r| r.pad == *pid);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let tip = cat.t("topo.btn.delete_pad");
                            let r = widgets::with_icons(ctx, |icons| {
                                widgets::icon_button(ui, icons, &th, "delete", false, &tip)
                            });
                            if r.clicked() {
                                del_pad = Some(*pid);
                            }
                            if let Some(r) = rep {
                                let txt = cat
                                    .t("topo.info.pad")
                                    .replace("{cut}", &crate::unit_text::vol_ui(r.cut))
                                    .replace("{fill}", &crate::unit_text::vol_ui(r.fill))
                                    .replace("{area}", &crate::unit_text::area_ui(r.area));
                                let weak = kit::weak(ui);
                                ui.label(egui::RichText::new(txt).color(weak));
                            }
                            ui.add(egui::Label::new(name.as_str()).truncate());
                        });
                    });
                }
            }
        });
    if !open {
        ed.topo.window_open = false;
    }
    if let Some(id) = choose {
        ed.topo.active = Some(id);
        ed.topo.props_buf = None;
    }
    if site_apply {
        if let Some(b) = ed.topo.site_buf.clone() {
            let r = ed.run_op(super::op(move |m| {
                m.set_site(&ddc_model::SiteProps {
                    name: b.name.clone(),
                    north_deg: b.north,
                    base: (b.base[0], b.base[1], b.base[2]),
                    latitude: b.lat,
                    longitude: b.lon,
                    crs_code: b.crs_code.trim().to_string(),
                    crs_name: b.crs_name.trim().to_string(),
                    crs_vertical: b.crs_vertical.trim().to_string(),
                })
            }));
            report(ed, r, "topo.done.site");
        }
    }
    if props_apply {
        if let Some(p) = ed.topo.props_buf.clone() {
            let r = ed.run_op(super::op(move |m| {
                m.set_topo_props(p.id, &p.name, p.thickness, &p.material)
            }));
            report(ed, r, "topo.done.props");
        }
    }
    if del_topo {
        if let Some(id) = act {
            let r = ed.run_op(super::op(move |m| m.delete_toposolid(id)));
            ed.topo.props_buf = None;
            ed.topo.active = None;
            report(ed, r, "topo.done.deleted_topo");
        }
    }
    if let Some(pid) = del_pad {
        let r = ed.run_op(super::op(move |m| m.delete_pad(pid)));
        report(ed, r, "topo.done.deleted_pad");
    }
}

fn report(ed: &mut crate::editor::Editor, r: Result<(), ddc_model::Error>, ok: &str) {
    match r {
        Ok(()) => super::say(ed, ok),
        Err(e) => super::say_err(ed, &e),
    }
}

/// Source shown in the import window.
pub enum ImportKind {
    /// Point file: full text and the first lines for preview.
    Xyz {
        text: String,
        head: String,
        order: ColumnOrder,
    },
    /// Drawing: layers (with enabled flag) and filters.
    Dxf {
        doc: Box<ddc_io::dxf::DxfDocument>,
        layers: Vec<(DxfLayer, bool)>,
        keep_flat: bool,
        inserts: bool,
        breaklines: bool,
    },
}

impl ImportKind {
    /// What Import reads with the dialog's choices, coordinates converted from `unit` to metres.
    pub fn read(&self, unit: SurveyUnit) -> Result<TopoSource, String> {
        match self {
            ImportKind::Xyz { text, order, .. } => ddc_io::topo::parse_xyz_with(
                text,
                &XyzOptions {
                    order: *order,
                    unit: Some(unit),
                },
            ),
            ImportKind::Dxf {
                doc,
                layers,
                keep_flat,
                inserts,
                breaklines,
            } => {
                let o = DxfTopoOptions {
                    layers: layers
                        .iter()
                        .filter(|l| l.1)
                        .map(|l| l.0.name.clone())
                        .collect(),
                    contours_as_breaklines: *breaklines,
                    keep_flat: *keep_flat,
                    inserts: *inserts,
                    unit: Some(unit),
                };
                if o.layers.is_empty() {
                    Err("no layer chosen".into())
                } else {
                    ddc_io::topo::dxf_source(doc, &o)
                }
            }
        }
    }
}

/// Survey import window: column preview (XYZ) or layer selection (DXF) before writing to the model.
pub struct ImportDlg {
    pub path: PathBuf,
    pub name: String,
    pub kind: ImportKind,
    /// Unit of the file's coordinates; the dialog always shows it.
    pub unit: SurveyUnit,
    /// The default unit and where it came from.
    pub guess: UnitGuess,
    /// Span in file units of what Import would read now.
    pub span: Option<Span>,
    pub span_key: String,
}

impl ImportDlg {
    /// Default unit from the file, else the extent's suggestion.
    pub fn new(path: PathBuf, name: String, kind: ImportKind) -> Self {
        let span = units_ui::raw_span(&kind);
        let guess = ddc_io::topo::guess(units_ui::file_unit(&kind), span);
        ImportDlg {
            path,
            name,
            unit: guess.unit,
            guess,
            span,
            span_key: units_ui::span_key(&kind),
            kind,
        }
    }
}

/// The survey form for `--import-demo`.
pub fn demo_import(ed: &mut Editor, path: &str) {
    let head = "x y z\n100.0 200.0 12.5\n101.0 200.5 12.6\n".to_string();
    let kind = ImportKind::Xyz {
        text: head.clone(),
        head,
        order: ColumnOrder::Auto,
    };
    ed.topo.import = Some(ImportDlg::new(path.into(), "Survey".into(), kind));
}

/// Preview rows shown in the window.
const PREVIEW_ROWS: usize = 8;

/// Open the import window for a file; IFC and other formats are imported immediately.
pub fn open_import(ed: &mut Editor, path: &Path) {
    let kind = match ddc_io::topo::kind_of(path) {
        Some(SourceKind::Xyz) => match std::fs::read(path) {
            Ok(b) => {
                let text = String::from_utf8_lossy(&b).into_owned();
                let head = text.lines().take(60).collect::<Vec<_>>().join("\n");
                ImportKind::Xyz {
                    text,
                    head,
                    order: ColumnOrder::Auto,
                }
            }
            Err(e) => return super::import_error(ed, &format!("{}: {e}", path.display())),
        },
        Some(SourceKind::Dxf) => {
            let doc = match std::fs::read(path)
                .map_err(|e| format!("{}: {e}", path.display()))
                .and_then(|b| ddc_io::dxf::read_bytes(&b).map_err(|e| e.to_string()))
            {
                Ok(d) => d,
                Err(e) => return super::import_error(ed, &e),
            };
            let layers: Vec<(DxfLayer, bool)> = ddc_io::topo::dxf_layers(&doc)
                .into_iter()
                .map(|l| {
                    let on = l.elevated > 0;
                    (l, on)
                })
                .collect();
            if layers.is_empty() {
                return super::import_error(ed, "no terrain points in the drawing");
            }
            ImportKind::Dxf {
                doc: Box::new(doc),
                layers,
                keep_flat: false,
                inserts: true,
                breaklines: true,
            }
        }
        _ => return super::import_file(ed, path),
    };
    let name = super::stem(ed, path);
    ed.topo.import = Some(ImportDlg::new(path.to_path_buf(), name, kind));
}

fn import_window(app: &mut EditorApp, ctx: &egui::Context) {
    let cat = &app.sh.cat;
    let lu = LenUnit::of(app.ed.tool.units);
    let ed = &mut app.ed;
    let Some(dlg) = &mut ed.topo.import else {
        return;
    };
    let th = ddc_ui::theme::of(ctx);
    let (mut open, mut go, mut cancel) = (true, false, false);
    kit::Dialog::new(
        "topo_import",
        cat.t("topo.import.title"),
        kit::DialogSize::Form,
    )
    .stable()
    .open(&mut open)
    .show(ctx, |ui| {
        kit::elided_weak(ui, &dlg.path.display().to_string());
        text_row(ui, &th, cat, "topo.import.name", &mut dlg.name);
        let (source, ticked) = match &mut dlg.kind {
            ImportKind::Xyz { head, order, .. } => {
                xyz_preview(ui, &th, cat, head, order);
                (SourceKind::Xyz, 0)
            }
            ImportKind::Dxf {
                layers,
                keep_flat,
                inserts,
                breaklines,
                ..
            } => {
                kit::section_title(ui, cat.t("topo.import.layers"));
                let st = all_state(layers);
                let mut all = st == AllState::All;
                let head = cat.t("topo.import.all_layers");
                if widgets::checkbox_mixed(ui, &th, &mut all, st == AllState::Mixed, &head).changed() {
                    for l in layers.iter_mut() {
                        l.1 = all;
                    }
                }
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for (l, on) in layers.iter_mut() {
                            let txt = cat
                                .t("topo.import.layer")
                                .replace("{name}", &l.name)
                                .replace("{n}", &l.entities.to_string())
                                .replace("{z}", &l.elevated.to_string());
                            widgets::checkbox(ui, &th, on, &txt);
                        }
                    });
                widgets::checkbox(ui, &th, keep_flat, &cat.t("topo.import.keep_flat"));
                widgets::checkbox(ui, &th, inserts, &cat.t("topo.import.inserts"));
                widgets::checkbox(ui, &th, breaklines, &cat.t("topo.import.breaklines"));
                (SourceKind::Dxf, layers.iter().filter(|l| l.1).count())
            }
        };
        units_ui::refresh(dlg);
        units_ui::unit_rows(ui, &th, cat, dlg, lu);
        let why = import_block(source, ticked).map(|k| cat.t(k));
        if let Some(w) = &why {
            kit::hint(ui, w.as_str());
        }
        kit::dialog_footer(ui, |ui| {
            go = kit::primary_action(ui, cat.t("topo.import.ok"), why.as_deref());
            cancel = kit::cancel_button(ui, cat.t("topo.import.cancel")).clicked();
        });
    });
    if go {
        if let Some(d) = ed.topo.import.take() {
            run_import(ed, d);
        }
    } else if cancel || !open {
        ed.topo.import = None;
    }
}

/// Column order and the first rows with parsed X/Y/Z.
fn xyz_preview(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, head: &str, order: &mut ColumnOrder) {
    let items: Vec<String> = ColumnOrder::ALL
        .iter()
        .map(|o| cat.t(&format!("topo.import.order.{}", o.key())))
        .collect();
    let mut sel = ColumnOrder::ALL.iter().position(|o| *o == *order).unwrap_or(0);
    if pick_row(ui, th, &cat.t("topo.import.order"), "order", &mut sel, &items) {
        *order = ColumnOrder::ALL[sel];
    }
    let o = XyzOptions {
        order: *order,
        ..Default::default()
    };
    let p = preview_xyz(head, PREVIEW_ROWS, &o);
    let mut sep = cat.t("topo.import.sep").replace("{sep}", p.sep.label());
    if p.decimal_comma {
        sep = format!("{sep} · {}", cat.t("topo.import.decimal_comma"));
    }
    ui.label(egui::RichText::new(sep).weak());
    ui.label(egui::RichText::new(cat.t("topo.import.preview")).strong());
    let ncols = p
        .rows
        .iter()
        .map(Vec::len)
        .chain(p.header.as_ref().map(Vec::len))
        .max()
        .unwrap_or(0)
        .min(8);
    egui::ScrollArea::horizontal().show(ui, |ui| {
        egui::Grid::new("topo.import.grid")
            .striped(true)
            .show(ui, |ui| {
                for k in 0..ncols {
                    let h = p
                        .header
                        .as_ref()
                        .and_then(|h| h.get(k).cloned())
                        .unwrap_or_else(|| (k + 1).to_string());
                    ui.label(egui::RichText::new(h).weak());
                }
                for a in ["X", "Y", "Z"] {
                    ui.label(egui::RichText::new(a).strong());
                }
                ui.end_row();
                for (row, pt) in p.rows.iter().zip(&p.points) {
                    for k in 0..ncols {
                        ui.label(row.get(k).map_or("", String::as_str));
                    }
                    match pt {
                        Some((x, y, z)) => {
                            for v in [x, y, z] {
                                ui.label(crate::units::fmt_num(*v, 3));
                            }
                        }
                        None => {
                            kit::warn_note(ui, cat.t("topo.import.bad"));
                            ui.label("");
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
    });
}

/// Import with the dialog's choices: one toposolid, one undo step.
pub(super) fn run_import(ed: &mut Editor, d: ImportDlg) {
    let no_layer = matches!(&d.kind, ImportKind::Dxf { layers, .. } if !layers.iter().any(|l| l.1));
    let r = if no_layer {
        Err(ed.topo.t("topo.import.no_layers"))
    } else {
        d.kind.read(d.unit)
    };
    match r {
        Ok(src) => super::import_source(ed, &d.name, src),
        Err(e) => super::import_error(ed, &e),
    }
}
