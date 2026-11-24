// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editor shell on ddc-ui: title bar (file, undo/redo, plan/3D, palette), command ribbon,
//! tool options panel, properties, status bar (snap, units, cursor, selection), toasts.
//! Plan drawing and input live in `plan_view`, 3D in `viewport`; this module only routes commands.
use crate::editor::{Editor, Sel};
use crate::opening_tool::OpeningType;
use crate::plan_view::PlanView;
use crate::prefs::Prefs;
use crate::ui_kit::{self, Tokens};
use crate::units::{
    display_length, edit_length, parse_edit_length, parse_length, unit_label,
    Units,
};
use crate::viewport::Viewport;
use crate::wall_tool::Mode;
#[cfg(feature = "gui")]
use ddc_render::{wgpu, Renderer, Targets};
use ddc_ui::notify::Level;
use ddc_ui::theme::{Role, Width};
use ddc_ui::workspace::{self, PanelId, Surface, Workspace};
use ddc_ui::{widgets, Density, Lang, Shell, Theme};
use egui::{self, RichText};
use crate::RenderState;

pub mod chrome;
pub mod clash_win;
pub mod dock_ui;
pub mod fold;
pub mod pane_focus;
pub mod drawing_kit;
pub mod drawing_win;
pub mod xlsx_alias_ids;
pub mod xlsx_applied;
pub mod xlsx_counts;
pub mod xlsx_head;
pub mod xlsx_gate;
pub mod xlsx_idle;
pub mod xlsx_issues;
pub mod xlsx_read_as;
pub mod xlsx_job;
pub mod xlsx_export_verify;
pub mod xlsx_load;
pub mod xlsx_owner;
pub mod xlsx_project;
pub mod xlsx_recover;
pub mod xlsx_relink;
pub mod xlsx_links;
pub mod xlsx_loss;
pub mod xlsx_no_base;
pub mod xlsx_scope;
pub mod xlsx_style;
pub mod xlsx_undo;
pub mod xlsx_verify;
pub mod xlsx_win;
pub mod xl_diff;
pub mod xl_issue_list;
pub mod import_win;
mod level_win;
#[cfg(test)]
mod props_contract;
pub(crate) mod panels;
pub(crate) mod project_seam;
pub(crate) mod prop_head;
pub(crate) mod prop_multi;
mod tab_strip;
pub(crate) mod prop_rows;
pub mod recover_rows;
pub mod recover_win;
pub mod report_win;
pub mod licenses_win;
pub mod looks_ui;
pub mod mode_layout;
pub mod onboard;
pub mod settings_win;
pub mod start_win;
pub mod status_bar;
pub mod tasks_ui;
pub mod load_win;
mod tree_ux;
mod browser_rows;
pub(crate) mod faults;
pub(crate) mod warn;
#[cfg(test)]
mod i18n_lint;
#[cfg(test)]
mod undo_toast_tests;

/// Command handler a tool module registers from its `register(&mut EditorApp)`.
pub type Handler = fn(&mut EditorApp);
/// `register()` of tool modules; called once from [`EditorApp::new`].
pub const REGISTER: &[fn(&mut EditorApp)] = &[
    crate::gfx_ui::register,
    status_bar::register,
    crate::ai::mcp_bridge::register,
    crate::ai::register,
    crate::modify_tools::register,
    crate::elem::fitting::register,
    crate::elem::panel_rule::register,
    crate::elem::history::register,
    dock_ui::register,
    looks_ui::register,
    mode_layout::register,
    onboard::register,
    fold::register,
    pane_focus::register,
    crate::clip_tools::register,
    prop_rows::register,
    crate::value_src_ui::register,
    panels::browser::register,
    panels::type_library::register,
    crate::query::register,
    crate::tools::register,
    crate::quick::register,
    crate::mass_tools::register,
    crate::edit_bind::register,
    crate::grips_opening::register,
    clash_win::register,
    xlsx_win::register,
    recover_win::register,
    crate::mep_browser::register,
    crate::circuit_edit::register,
    crate::sheet_view::register,
    crate::deliver::ui::register,
    crate::phase_ui::register,
    crate::grid_set_ui::register,
    crate::view3d::register,
    crate::bigfile::register,
    crate::topo::register,
    crate::roof_edge_tools::register,
    crate::help::register,
    report_win::register,
    licenses_win::register,
    crate::edit3d::register,
    crate::solid_tools::register,
    crate::annot_text::register,
    crate::drafting_view::register,
    crate::drawing::detail::register,
    crate::family_editor::register,
    crate::mesh_tools::register,
    crate::source_ui::register,
    crate::links_ui::register,
    crate::rebar_ui::register,
    crate::project_ui::register,
    crate::open::register,
    crate::save::register,
    crate::data_ui::register,
    crate::where_is::register,
    crate::issues_ui::register,
    crate::view_gfx_ui::register,
    crate::range_ui::register,
    crate::find_replace::register,
    crate::pdf_underlay::register,
    crate::classify_ui::register,
    crate::wall_profile_ui::register,
    crate::fitting_insert::register,
    crate::explain_ui::register,
    crate::gen_rules_ui::register,
    crate::collab::register,
    crate::update::register,
    crate::boundary_link_ui::register,
    crate::wall_pick::register,
    crate::line_pick::register,
    crate::outline_cleanup::register,
    browser_rows::register,
    crate::view_cmds::register,
    crate::view_panes::register,
    crate::query_win::register,
    crate::scripts_win::register,
    crate::script_rec::register,
];

/// Status-bar hint key `hint.<tool>.<step>` (TOOLS_UI §7).
pub fn hint_key(tool: &str, step: &str) -> String {
    format!("hint.{tool}.{step}")
}

/// Length along the axis: chord of a straight wall or arc length from bulge.
fn wall_length(w: &crate::editor::Wall) -> f64 {
    let c = (w.b - w.a).len();
    if w.bulge == 0.0 {
        return c;
    }
    let th = 4.0 * w.bulge.atan();
    c * th / (2.0 * (th / 2.0).sin())
}

/// Commands `EditorApp::run` implements; the ribbon draws the rest disabled.
pub const WIRED: &[&str] = &[
    "app.palette",
    "arch.wall",
    "str.wall",
    "arch.wall_room",
    "arch.wall_pick",
    "arch.grid_pick",
    "struct.beam_pick",
    "arch.room",
    "arch.door",
    "arch.window",
    "arch.curtain_wall",
    "arch.section",
    "view.section",
    "view.elevation",
    "edit.undo",
    "edit.redo",
    "mod.delete",
    "edit.select_all",
    "view.plan",
    "view.plan_here",
    "view.3d",
    "view.zoom_fit",
    "view.section_box",
    "view.warnings",
    "mgr.level",
    "mgr.level_rename",
    "mgr.level_delete",
    "view.level_up",
    "view.level_down",
    "view.underlay",
    "mgr.units",
    "mgr.settings",
    "mgr.import_cad",
    "mgr.autosave",
    "file.open",
    "ui.layout.set.1",
    "ui.layout.set.2",
    "ui.layout.set.3",
    "ui.layout.set.4",
    "ui.layout.set.5",
    "ui.layout.next",
    "ui.layout.prev",
    "ui.layout.back",
    "ui.layout.settings",
];

/// Canvas colours derived from the shared theme tokens.
pub fn tokens(th: &Theme) -> Tokens {
    crate::ui_kit::from_theme(th)
}

pub struct EditorApp {
    pub ed: Editor,
    pub plan: PlanView,
    pub view: Viewport,
    /// Show-as-built player over the 3D view (`reveal`); none: the scene as it is.
    pub reveal: Option<crate::reveal::Player>,
    /// Background journal walk used for the reveal playback (`reveal::start_journal`).
    pub reveal_job: Option<crate::reveal::JournalJob>,
    /// `--reveal-motion`: beats the interface motion setting for every show.
    pub reveal_motion: Option<crate::reveal::Motion>,
    pub view3d: bool,
    pub sh: Shell,
    /// Adapter/backend line for the status bar.
    pub backend: String,
    pub scene: ddc_render::Scene,
    scene_rev: u64,
    /// Shape preview in the scene: change number and the shape replaced by the preview.
    scene_prev: u64,
    /// Link set (`links_ui::State::rev3d`) the scene was built with.
    scene_links: u64,
    /// Host scene before link groups: changing the link set does not rebuild the host.
    host_base: Option<crate::links_ui::HostBase>,
    scene_shown: Option<i64>,
    /// Hide/isolate state the 3D scene was built with.
    scene_vis: crate::isolate::VisKey,
    /// Opening size edit in properties: (id, [width, height, sill]) before commit.
    edit: Option<(i64, [f64; 3])>,
    /// Wall edit in properties: (id, length, height) — text held until Enter.
    wedit: Option<(i64, String, String)>,
    cwedit: crate::cw_props::Buf,
    bridges: crate::import::Bridges,
    import_open: bool,
    import_path: String,
    pub(crate) import_job: Option<crate::import::Job>,
    pub import_flow: import_win::ImportFlow,
    /// Background open/import of big files, lazy mesh feed (bigfile).
    pub big: crate::bigfile::BigFile,
    pub chrome: chrome::Chrome,
    /// Developer mode: key text and the key awaiting confirmation.
    dev_key: String,
    dev_pending: Option<String>,
    last_status: Option<String>,
    /// Settings persisted between runs; the "Settings" window.
    pub prefs: Prefs,
    pub(crate) settings_open: bool,
    /// Welcome card dismissed (first action taken).
    pub(crate) welcome_closed: bool,
    /// Discrete GPU: 3D effects default on («auto»).
    pub discrete_gpu: bool,
    /// "Warnings" window and the warnings cache keyed by model revision.
    pub warnings_open: bool,
    warnings: ((u64, u64), Vec<ddc_model::ElementWarning>),
    /// `warnings` without symmetric pairs and element labels — rebuilt per revision, not per frame.
    warn_uniq: Vec<ddc_model::ElementWarning>,
    warn_labels: warn::Labels,
    faults: faults::State,
    level_totals: Vec<ddc_model::LevelTotals>,
    /// "Model" block: all levels (otherwise the current one).
    totals_all: bool,
    /// Active workspace id (`classic`..`data`); switching changes nothing else.
    pub layout: String,
    layout_prev: Option<String>,
    /// Plan centre and scale left behind per split shape, put back on return.
    plan_by_shape: Vec<(String, ddc_geom::V2, f64)>,
    /// Settings page: 0 general, 1 interface.
    pub settings_page: u8,
    /// Show the first-run chooser even without a prefs file (screenshots).
    pub force_chooser: bool,
    /// Presets plus custom layouts from `workspaces.json`.
    pub layouts: Vec<Workspace>,
    /// Schedule of the "Model + table" interface.
    pub table: crate::schedule::ui::View,
    pub(crate) table_copy: bool,
    /// Ctrl+V text that arrived while the schedule table had focus.
    pub(crate) table_paste: Option<String>,
    /// Open section/elevation view (shown instead of the plan), tabs of open views, their camera.
    pub section: Option<i64>,
    pub tabs: Vec<i64>,
    pub sview: crate::section_view::SectionView,
    /// View panes (`view.panes`): plan, sections and 3D side by side.
    pub panes: crate::view_panes::Panes,
    /// "Clashes" window.
    pub clash: clash_win::ClashWin,
    pub mep_browser: crate::mep_browser::MepBrowser,
    pub circuit_edit: crate::circuit_edit::CircuitEdit,
    /// "Level" window: (id of the level being edited or None for a new one, name, elevation as text).
    pub level_edit: Option<level_win::LevelEdit>,
    /// XLSX import preview (window is open while Some).
    pub xlsx: Option<xlsx_win::XlsxPreview>,
    /// Export to XLSX running on a worker, and the «no linked book» offer.
    pub xlsx_job: Option<xlsx_job::XlsxJob>,
    /// Book read for the import preview, on a worker.
    pub xlsx_load: Option<xlsx_load::XlsxLoad>,
    pub xlsx_offer: bool,
    /// Open scope card of the XLSX export.
    pub xlsx_card: Option<xlsx_scope::ScopeCard>,
    /// Cells the last export cut or left blank; shown in a list window until closed.
    pub xlsx_cell_notes: Vec<ddc_io::xlsx::ExportNote>,
    /// Export target is an existing book with edits or rows that a rewrite would drop.
    pub xlsx_over: Option<xlsx_scope::OverCard>,
    /// Re-export refused: what the new book would drop from the old one, waiting for a choice.
    pub xlsx_loss: Option<xlsx_loss::LossCard>,
    /// Undo step of the last XLSX Apply, for its toast button.
    pub xlsx_undo: Option<xlsx_undo::Mark>,
    /// Scope of the export that is running or was last written.
    pub xlsx_scope: Option<ddc_io::xlsx::Scope>,
    /// Wall and curtain wall edit windows.
    pub wall_edit: Option<crate::edit_bind::WallPanel>,
    pub curtain_edit: Option<crate::edit_bind::CurtainPanel>,
    /// Drawing import window: files, levels, units, origin.
    pub drawing_form: drawing_win::Form,
    /// Two-letter shortcuts (registry + the user's `shortcuts.json`) and the letter typed so far.
    pub keys2: ddc_ui::shortcuts2::Keymap,
    pub seq: ddc_ui::shortcuts2::Seq,
    /// Tool step hint for this frame: (tool, step) → `hint.<tool>.<step>`.
    pub hint: Option<(String, String)>,
    /// Command handlers registered by tool modules.
    pub handlers: std::collections::BTreeMap<String, Handler>,
    pub autosave: recover_win::Autosave,
    pub autosave_open: bool,
    /// "Recovery" dialog (shown at startup).
    pub recover: Option<recover_win::Recover>,
    /// Blocking model error (modal window).
    pub blocking: Option<warn::Blocking>,
    /// Status bar selectors: links, pinned, by face.
    pub sel_flags: [bool; 3],
    /// Status bar: drawing aids, recent commands strip, save clock.
    pub sbar: status_bar::State,
    /// Palette with arguments, repeat, menu, HUD, selection sets, history (`quick`).
    pub quick: crate::quick::State,
    /// Import sources: provenance, re-import, conflicts, local edits.
    pub src: crate::source_ui::State,
    /// Linked models (read-only).
    pub links: crate::links_ui::State,
    /// New/open/save: task, dialogs, window title.
    pub project_ui: crate::project_ui::State,
    /// One door for opening files: choice card, queue, associations (o1).
    pub open: crate::open::State,
    /// Saving to IFC: its own job slot, target, result strip.
    pub save: crate::save::State,
    /// glTF, project info, purge.
    pub data: crate::data_ui::State,
    /// Merge with a colleague's file, version comparison, project variants.
    pub collab: crate::collab::State,
    /// ERP link: session, lock, "Publish to ERP" (feature `link`, off until the user turns it on).
    #[cfg(feature = "link")]
    pub erp: crate::link::panel::State,
}

/// Install the 3D renderer into the egui-wgpu callback resources.
#[cfg(feature = "gui")]
pub fn install_renderer(rs: &RenderState, scene: &ddc_render::Scene) {
    let mut r = Renderer::new(
        &rs.device,
        Targets {
            color: rs.target_format,
            depth: Some(wgpu::TextureFormat::Depth32Float),
            samples: crate::gfx::window_samples(),
        },
        scene,
    );
    let four = [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Depth32Float,
    ]
    .iter()
    .all(|f| {
        rs.adapter
            .get_texture_format_features(*f)
            .flags
            .sample_count_supported(4)
    });
    r.set_max_samples(if four { 4 } else { 1 });
    // reading depth without waiting for the GPU is unreliable on GL: only the pyramid and on-screen size are used there
    r.set_occlusion(
        &rs.device,
        rs.adapter.get_info().backend != wgpu::Backend::Gl,
    );
    r.set_adapter(&rs.adapter.get_info());
    rs.renderer.write().callback_resources.insert(r);
}

#[cfg(not(feature = "gui"))]
pub fn install_renderer(rs: &RenderState, _: &ddc_render::Scene) {
    match *rs {}
}

#[cfg(feature = "gui")]
fn discrete_gpu(rs: Option<&RenderState>) -> bool {
    rs.is_some_and(|rs| rs.adapter.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
}

#[cfg(not(feature = "gui"))]
fn discrete_gpu(_: Option<&RenderState>) -> bool {
    false
}

impl EditorApp {
    pub fn new(mut ed: Editor, rs: Option<&RenderState>, backend: String) -> Self {
        ed.set_mode(crate::wall_tool::Mode::Select);
        let mut e3 = crate::edit3d::Edit3d::default();
        e3.cache.inst.enable();
        let (scene, e3_ext) = crate::edit3d::scene3d::compose_with(&mut ed, &mut e3.cache);
        e3.set_ext(e3_ext);
        if let Some(rs) = rs {
            install_renderer(rs, &scene);
        }
        let mut sh = Shell::new(Theme::new(ddc_ui::Mode::Light, Density::Compact), Lang::Ru);
        sh.ribbon.available = Some(WIRED.iter().map(|s| s.to_string()).collect());
        let keys2 = ddc_ui::shortcuts2::Keymap::load(&sh.reg, crate::gfx::data_dir().as_deref());
        let mut app = Self {
            keys2,
            seq: Default::default(),
            hint: None,
            handlers: Default::default(),
            autosave: Default::default(),
            autosave_open: false,
            recover: None,
            blocking: None,
            sel_flags: [true, false, false],
            sbar: Default::default(),
            quick: Default::default(),
            src: Default::default(),
            links: Default::default(),
            project_ui: Default::default(),
            open: Default::default(),
            save: Default::default(),
            data: Default::default(),
            collab: Default::default(),
            #[cfg(feature = "link")]
            erp: Default::default(),
            // the first scene was built before providers were registered: the frame will rebuild it
            scene_rev: u64::MAX,
            scene_prev: 0,
            scene_links: 0,
            host_base: None,
            scene_shown: None,
            scene_vis: Default::default(),
            ed,
            plan: PlanView::default(),
            view: {
                let mut v = Viewport::new();
                v.e3 = e3;
                v
            },
            view3d: false,
            sh,
            backend,
            scene,
            edit: None,
            wedit: None,
            cwedit: None,
            reveal: None,
            reveal_job: None,
            reveal_motion: None,
            bridges: crate::import::Bridges::load(),
            import_open: false,
            drawing_form: Default::default(),
            import_path: String::new(),
            import_job: None,
            import_flow: Default::default(),
            big: Default::default(),
            chrome: Default::default(),
            dev_key: String::new(),
            dev_pending: None,
            last_status: None,
            prefs: Prefs::default(),
            settings_open: false,
            welcome_closed: false,
            layouts: workspace::presets(),
            table: Default::default(),
            table_copy: false,
            table_paste: None,
            section: None,
            tabs: vec![],
            sview: Default::default(),
            panes: Default::default(),
            clash: Default::default(),
            mep_browser: Default::default(),
            circuit_edit: Default::default(),
            level_edit: None,
            warnings_open: false,
            warnings: ((u64::MAX, 0), vec![]),
            warn_uniq: vec![],
            warn_labels: Default::default(),
            faults: Default::default(),
            level_totals: vec![],
            totals_all: true,
            xlsx: None,
            xlsx_job: None,
            xlsx_load: None,
            xlsx_offer: false,
            xlsx_card: None,
            xlsx_cell_notes: Vec::new(),
            xlsx_over: None,
            xlsx_loss: None,
            xlsx_undo: None,
            xlsx_scope: None,
            wall_edit: None,
            curtain_edit: None,
            discrete_gpu: discrete_gpu(rs),
            layout: "classic".into(),
            layout_prev: None,
            plan_by_shape: Vec::new(),
            settings_page: 0,
            force_chooser: false,
        };
        for r in REGISTER {
            r(&mut app);
        }
        // the start model is on disk or empty: its load revision is no edit a snapshot owes
        if !app.ed.is_dirty() {
            app.autosave.mark_clean(app.ed.revision);
        }
        // the tool the app starts with is not a "fresh start": leave the view alone
        app.quick.tool_was = crate::quick::defaults::tool_key(&app.ed);
        app
    }

    /// Tool module command: appears enabled in the ribbon and is executed by `h`.
    pub fn register_command(&mut self, id: &str, h: Handler) {
        self.handlers.insert(id.to_string(), h);
        if let Some(a) = &mut self.sh.ribbon.available {
            a.insert(id.to_string());
        }
    }

    /// Read bundle for `path` (source re-import): folder and signature keys.
    pub(crate) fn trust_store(&self) -> ddc_io::bridge::TrustStore {
        self.bridges.trust.clone()
    }

    pub(crate) fn bridge_for(
        &self,
        path: &std::path::Path,
    ) -> Option<(std::path::PathBuf, ddc_io::bridge::TrustStore)> {
        let entries = crate::import::entries(&self.bridges.list);
        let dir = crate::import::entry_for(&entries, path)?.dir.clone()?;
        Some((dir, self.bridges.trust.clone()))
    }

    /// Selection category for the contextual tab: (key, label).
    pub fn selection_category(&self) -> Option<(String, String)> {
        let k = match self.ed.selection.first()? {
            Sel::Wall(_) => "wall",
            Sel::Opening(_) => "opening",
            Sel::Curtain(_) | Sel::Panel(..) => "curtain",
            Sel::Slab(_) => "floor",
            Sel::Room(_) => "room",
            Sel::Mass(_) => "mass",
            Sel::Anno(_) => "anno",
            Sel::Mep(_) => "mep",
            Sel::Comp(_) => "comp",
            Sel::Ext(c, _) => crate::editor::structx::category_of(*c),
            Sel::Body(k, _) => crate::editor::bodies::category_key(*k),
        };
        Some((k.to_string(), self.sh.cat.t(&format!("selcat.{k}"))))
    }

    /// Step hint of the current tool (if the module did not set its own).
    pub fn tool_hint(&self) -> Option<(&'static str, &'static str)> {
        let ed = &self.ed;
        let step = |started: bool| if started { "next" } else { "first" };
        // a mass command drives the plan in polyline mode: its own hint, not the wall's
        if let Some(t) = &ed.mass {
            return Some((t.cmd.id(), t.step()));
        }
        if let Some(h) = crate::topo::hint_pair(ed) {
            return Some(h);
        }
        if ed.section_tool().is_some() {
            return Some(("section", "first"));
        }
        if let Some(m) = &ed.sketch {
            return Some(("sketch", m.draw.key()));
        }
        if ed.opening_mode {
            return Some(("opening", "place"));
        }
        if let Some(t) = ed.model_tool() {
            return Some((t.kind.key(), t.step()));
        }
        let started = !ed.wall_draft().points.is_empty();
        match ed.wall_mode() {
            Mode::PickLine => Some(crate::line_pick::hint(&ed.tool)),
            Mode::Polyline if ed.tool.curtain => Some(("curtain", step(started))),
            Mode::Polyline => Some(("wall", step(started))),
            Mode::Rect => Some(("rect", step(started))),
            Mode::Select => Some(("select", "idle")),
        }
    }

    /// Command of the current tool (ribbon highlight).
    pub fn active_command(&self) -> Option<&'static str> {
        use crate::interact::{sessions, Scope};
        let ed = &self.ed;
        // a 3D tool survives a switch to the plan; only the 3D view runs it
        if let Some(c) = self.view3d.then(|| self.view.e3.tool.command()).flatten() {
            return Some(c);
        }
        // a tool session names its command; a sketch is not the wall tool under it
        let scoped = match *ed.ix.scope() {
            _ if sessions::view3d::running(ed) => None,
            Scope::Moving { op } if sessions::modify::running(ed) => Some(op.command()),
            Scope::Drafting { tool, .. } | Scope::Placing { tool } if tool.0 != "sketch" => Some(tool.0),
            _ => None,
        };
        if scoped.is_some() {
            return scoped;
        }
        // the plan view's annotation tools are not a session yet
        if let Some(d) = &self.plan.b.dim {
            use crate::modify_tools::dims::DimMode;
            return Some(match d.mode {
                DimMode::Linear => "ann.dim",
                DimMode::Angular => "ann.dim_angular",
                DimMode::Radial => "ann.dim_radial",
            });
        }
        if self.plan.b.dist.is_some() {
            return Some("mod.distribute");
        }
        if self.plan.b.mt.is_some() {
            return Some("mod.match_type");
        }
        if let Some(t) = &self.plan.b.ann {
            return Some(match t.mode {
                crate::modify_tools::tags::AnnMode::Tag => "ann.tag",
                crate::modify_tools::tags::AnnMode::Number => "ann.number",
            });
        }
        if let Some(m) = &self.plan.b.ms {
            use crate::edit3d::measure_saved::Mode;
            return Some(match m.mode {
                Mode::Clear => "ann.measure_gap",
                Mode::Angle => "ann.measure_angle",
                Mode::Area => "ann.measure_area",
                _ => "ann.measure_distance",
            });
        }
        (self.section.is_some() && self.sview.level_tool).then_some("arch.level")
    }

    fn label(&self, id: &str) -> String {
        self.sh
            .reg
            .get(id)
            .map(|c| self.sh.cat.t(&c.label_key()))
            .unwrap_or_else(|| id.to_string())
    }

    /// Tooltip: name and shortcuts, if any.
    pub fn tip(&self, id: &str) -> String {
        let name = self.label(id);
        match self.sh.reg.get(id).map(|c| c.shortcuts.join(" / ")) {
            Some(s) if !s.is_empty() => format!("{name}  ·  {s}"),
            _ => name,
        }
    }

    /// Run a command by id; returns false for commands the editor does not implement yet.
    /// "Level" lives only in the section where it was enabled.
    pub(crate) fn sync_level_tool(&mut self) {
        let here =
            !self.view3d && self.section.is_some() && self.section == self.sview.level_tool_at;
        if self.sview.level_tool && !here {
            self.sview.level_tool = false;
        }
    }

    pub fn run(&mut self, id: &str) -> bool {
        self.run_as(id, |app| app.run_cmd(id))
    }

    /// Runs `act` as the command `id` would run: the scope follows and the entry it wrote is
    /// offered for Undo. For actions that take arguments and so are not registry handlers.
    pub(crate) fn run_as(&mut self, id: &str, act: impl FnOnce(&mut Self) -> bool) -> bool {
        let pushes = self.ed.pushes;
        let ok = act(self);
        // the command may have armed a tool: the scope (and the lit command) follows now
        crate::interact::Interaction::refresh(&mut self.ed);
        self.reveal_tool_tab(id);
        // the entry this command wrote while running; its toast may offer Undo for it alone
        if let Some(e) = self.ed.entry_since(pushes) {
            self.quick.ran = Some((self.sh.reg.resolve(id).to_string(), e));
        }
        ok
    }

    /// A tool started from the palette, a shortcut or the command line shows its own ribbon tab,
    /// so the strip never sits on Architecture while a structure, MEP or annotation tool runs.
    fn reveal_tool_tab(&mut self, id: &str) {
        let id = self.sh.reg.resolve(id).to_string();
        if self.active_command() != Some(id.as_str()) || self.sh.ribbon.tab == ddc_ui::widgets::ribbon::CTX_TAB {
            return;
        }
        if let Some(t) = self.sh.reg.get(&id).and_then(|c| c.tab.clone()) {
            if t != self.sh.ribbon.tab {
                let known = self.sh.reg.tabs();
                self.sh.ribbon.set_tab(&t, &known);
            }
        }
    }

    fn run_cmd(&mut self, id: &str) -> bool {
        let id = self.sh.reg.resolve(id).to_string();
        let id = id.as_str();
        // any other command clears "Level"; zoom and the palette do not change the view
        if !(id == "arch.level" || id == "app.palette" || id.starts_with("view.zoom")) {
            self.sview.level_tool = false;
        }
        // a command from anywhere closes the ribbon band shown over the canvas
        if id != fold::CMD_RIBBON {
            self.sh.ribbon.peek = false;
            self.chrome.fold.reveal = false;
        }
        if let Some(h) = self.handlers.get(id).copied() {
            crate::roof_edge_tools::reset();
            h(self);
            self.sh.palette.note_used(id);
            return true;
        }
        let ed = &mut self.ed;
        match id {
            "app.palette" => self.sh.palette.toggle(),
            "arch.wall" | "str.wall" => ed.set_mode(Mode::Polyline),
            "arch.wall_room" => ed.set_mode(Mode::Rect),
            "arch.door" => ed.start_opening(OpeningType::door()),
            "arch.curtain_wall" => ed.start_curtain(),
            "arch.window" => ed.start_opening(OpeningType::window()),
            "edit.undo" => {
                ed.undo();
                self.plan.drawings.sync_undo(ed);
            }
            "edit.redo" => {
                ed.redo();
                self.plan.drawings.sync_undo(ed);
            }
            "mod.delete" => crate::view_gfx_ui::delete_visible(self),
            "edit.select_all" => {
                ed.selection = ed.walls.iter().map(|w| Sel::Wall(w.id)).collect();
            }
            "view.plan" | "view.plan_here" => {
                crate::sheet_view::deactivate();
                crate::drafting_view::deactivate();
                self.view3d = false;
                self.section = None;
            }
            "view.3d" => {
                crate::sheet_view::deactivate();
                crate::drafting_view::deactivate();
                self.view3d = true;
                self.section = None;
            }
            "view.section" | "arch.section" => {
                ed.start_section();
                crate::drafting_view::deactivate();
                self.view3d = false;
                self.section = None;
            }
            "view.elevation" => {
                ed.add_compass();
            }
            "mgr.level" => self.start_level_edit(None),
            "mgr.level_rename" => self.start_level_edit(Some(self.ed.level)),
            "mgr.level_delete" => {
                let id = ed.level;
                ed.delete_level(id);
            }
            "view.level_up" | "view.level_down" => {
                ed.step_level(if id == "view.level_up" { 1 } else { -1 });
                // the other level has its own extent: frame it, and again once the pane is drawn
                self.plan.fit(ed);
                self.plan.fit_if_cut = true;
            }
            "view.underlay" => {
                ed.toggle_underlay();
                let key = if !ed.underlay_on {
                    "cmd_buttons.underlay.off"
                } else if ed.underlay.is_empty() {
                    "cmd_buttons.underlay.none"
                } else {
                    "cmd_buttons.underlay.on"
                };
                let msg = self.sh.cat.t(key);
                ed.status = Some(msg.clone());
                self.sh.toasts.push(Level::Info, msg, None);
            }
            "view.section_box" => {
                let b = self
                    .ed
                    .highlight()
                    .and_then(|r| self.scene.bounds_of(r))
                    .or(self.scene.focus)
                    .or(self.scene.bounds);
                self.view.toggle_section_box(b);
                self.view3d = true;
                self.section = None;
            }
            "view.warnings" => self.warnings_open = !self.warnings_open,
            "view.next" => self.next_tab(),
            "view.close" => {
                self.close_open_tab();
            }
            "view.zoom_fit" => {
                self.plan.fit(&self.ed);
                self.view.refit();
                self.sview.refit();
            }
            "mgr.units" => {
                let u = match ed.tool.units {
                    Units::Metric => Units::Imperial,
                    Units::Imperial => Units::Metric,
                };
                ed.set_units(u);
                let key = match u {
                    Units::Metric => "cmd_buttons.units.metric",
                    Units::Imperial => "cmd_buttons.units.imperial",
                };
                let msg = self.sh.cat.t(key);
                ed.status = Some(msg.clone());
                // the toast's button switches back: the press is never a one-way door
                let back = self.sh.cat.t("cmd_buttons.units.back");
                self.sh.toasts.push_cmd(Level::Info, msg, back, "mgr.units");
            }
            "mgr.autosave" => self.autosave_open = !self.autosave_open,
            "mgr.settings" => {
                self.settings_open = true;
                self.settings_page = 0;
            }
            "ui.layout.settings" => self.open_settings(1),
            // the literal, not `chrome::LAYOUT_BACK`: the stub check reads the ids in this match
            "ui.layout.back" => {
                if let Some(prev) = self.layout_prev.clone() {
                    self.set_layout(&prev);
                }
            }
            "ui.layout.next" | "ui.layout.prev" => {
                let ids = workspace::PRESET_IDS;
                let i = ids.iter().position(|x| *x == self.layout).unwrap_or(0);
                let n = ids.len();
                let j = if id == "ui.layout.next" {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                self.set_layout(ids[j]);
            }
            _ if id.starts_with("ui.layout.set.") => {
                let key = workspace::parse_layout_value(&id["ui.layout.set.".len()..]);
                if let Some(k) = key {
                    self.set_layout(&k);
                }
            }
            "mgr.import_cad" => self.import_open = true,
            "edit.copy"
                if self.workspace().docks.iter().any(|d| {
                    d.panel == PanelId::DataTable && d.side != workspace::Side::Hidden
                }) || matches!(
                    self.workspace().views,
                    workspace::ViewArrangement::ModelTable { .. }
                ) =>
            {
                self.table_copy = true
            }
            _ => {
                let name = self.label(id);
                let msg = self.sh.cat.tf("unavailable.unavailable", &[("name", name.as_str())]);
                self.sh.toasts.push(Level::Warning, msg, None);
                return false;
            }
        }
        if id != "app.palette" {
            self.sh.palette.note_used(id);
        }
        true
    }

    /// Open a section/elevation view in a tab of the main view.
    pub fn open_view(&mut self, id: i64) {
        if self.ed.view_spec(id).is_none() {
            return;
        }
        if !self.tabs.contains(&id) {
            self.tabs.push(id);
        }
        crate::sheet_view::deactivate();
        crate::drafting_view::deactivate();
        self.section = Some(id);
        self.view3d = false;
    }

    /// Close a view tab; the neighbouring tab or the plan becomes active.
    pub fn close_view(&mut self, id: i64) {
        let k = self.tabs.iter().position(|t| *t == id);
        self.tabs.retain(|t| *t != id);
        if self.section == Some(id) {
            self.section = k.and_then(|k| self.tabs.get(k.saturating_sub(1)).copied());
        }
    }

    /// Ctrl/Alt/F-key shortcuts from the command registry. Plain keys stay with the canvas tools.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        // texture images: decoded when a realistic view appears, reread when a file changed
        if crate::mat_tex::sync(&self.ed) {
            ctx.request_repaint();
        }
        // what was really drawn last frame: a rail or a folded pane must not take F6
        let (browser_shown, props_shown) = self.chrome.dock.take_shown();
        let panes = [
            browser_shown,
            true,
            props_shown,
            self.prefs.ai_panel_open,
        ];
        if self.modal_up() {
            pane_focus::apply(ctx, panes, true);
            return;
        }
        crate::family_editor::grab_keys(ctx);
        let typing = ctx.wants_keyboard_input();
        let now = ctx.input(|i| i.time);
        if self.seq.tick(now).is_some() {
            self.sh.echo.clear();
        }
        if self.seq.pending().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat: false,
                modifiers,
            } = ev
            else {
                continue;
            };
            let fkey = key.name().starts_with('F') && key.name().len() > 1;
            if !(modifiers.command || modifiers.alt || fkey) {
                // B with a draft shown is the Before hold; the plan card may hold A/R/J/K
                let ghost_b = crate::ai::ghost::holds_key(key, crate::ai::preview::rev() != 0);
                let held = ghost_b || crate::ai::ui::review_keys_live();
                if !typing && !modifiers.shift && self.seq_ready() && !held {
                    self.seq_key(physical_key.unwrap_or(key), now);
                }
                continue;
            }
            // the schedule grid's focus is not a text field: Ctrl/Alt/F-key commands still run
            let text = typing && !crate::schedule::ui::grid_focused(ctx);
            if text && !ddc_ui::shell::passes_while_typing(key, modifiers) {
                continue;
            }
            // one owner of Ctrl+F per frame: the browser's search field while that pane is active
            if pane_focus::browser_ctrl_f(ctx, key, modifiers) {
                continue;
            }
            if let Some(id) = crate::quick::chord(self, key, modifiers) {
                self.run(&id);
            }
        }
        pane_focus::apply(ctx, panes, false);
    }

    /// A window that demands a choice is open (project prompt or notice, blocking model error,
    /// recovery): global shortcuts wait, the window's own Esc/Enter keep working.
    pub(crate) fn modal_up(&self) -> bool {
        self.project_ui.modal() || self.blocking.is_some() || self.recover.is_some()
    }

    /// Two-letter input only while no tool owns plain letters and no window is modal.
    fn seq_ready(&self) -> bool {
        let ed = &self.ed;
        !ed.tool_armed()
            && !ed.opening_mode
            && ed.section_tool().is_none()
            // the shape-edit elevation box takes letters (unit suffixes such as "300 mm")
            && ed.shape.is_none()
            && !self.sh.palette.open
            && !self.modal_up()
            && self.quick.keys_ui.capture.is_none()
    }

    fn seq_key(&mut self, key: egui::Key, now: f64) {
        use ddc_ui::shortcuts2::SeqEvent;
        match self.seq.key(&self.keys2, key, now) {
            Some(SeqEvent::Pending(c)) => self.sh.echo = format!("{c}…"),
            Some(SeqEvent::Fired(id)) => {
                self.sh.echo.clear();
                self.run(&id);
            }
            Some(SeqEvent::NoMatch(m)) => {
                self.sh.echo = format!("{m} ?");
                let t = ddc_ui::shortcuts2::unknown_message(&self.sh.cat, &self.keys2, &m);
                self.sh.toasts.push(ddc_ui::notify::Level::Info, t, None);
            }
            Some(SeqEvent::Timeout) | None => {}
        }
    }

    /// Status bar (`status_bar`): drawing aids and cursor · step hint · view and save state.
    pub(super) fn status_line(&mut self, ui: &mut egui::Ui) {
        status_bar::show(self, ui);
    }

    /// Any job that replaces or feeds the model is running: a second one must not start.
    /// Same rule as the open/import/save gate: one definition in `open::jobs`.
    pub fn io_busy(&self) -> bool {
        crate::open::jobs(self).running()
    }

    fn poll_import(&mut self, ctx: &egui::Context) {
        if let Some(j) = &mut self.import_job {
            match j.poll() {
                Some(Ok(frames)) => {
                    let input = j.input.clone();
                    self.import_job = None;
                    let r = self.ed.apply_import(&input, frames);
                    self.import_finished(&input, r, false);
                }
                Some(Err(e)) => {
                    let input = j.input.clone();
                    let cancelled = j.cancel.load(std::sync::atomic::Ordering::Relaxed);
                    self.import_job = None;
                    self.import_finished(&input, Err(e), cancelled);
                }
                None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
    }

    /// One frame of the whole window. `rs` uploads the 3D scene when it changes.
    pub fn ui(&mut self, ctx: &egui::Context, rs: Option<&RenderState>) {
        self.sync_level_tool();
        crate::set_ui_lang(self.sh.cat.lang);
        if self.chrome.full != self.chrome.full_sent {
            self.chrome.full_sent = self.chrome.full;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.chrome.full));
        }
        self.resolve_theme(ctx);
        let ws = self.workspace();
        self.sh.theme.density = match ddc_ui::looks::density_of(&self.prefs.density) {
            Some(d) => d,
            None => match workspace::resolve_density(&ws, None) {
                workspace::LayoutDensity::Comfortable => Density::Comfortable,
                _ => Density::Compact,
            },
        };
        self.sh.theme.radius_scale = ws.tokens.radius_scale;
        self.apply_look();
        if !self.sh.theme.apply(ctx) {
            return;
        }
        self.sync_mode_layout(ctx);
        if let Some(l) = self.chrome.settings.apply_layout.take() {
            self.apply_mode_layout(ctx, &l);
        }
        if let Some(n) = self.chrome.settings.use_layout.take() {
            self.mode_layout_use(ctx, &n);
        }
        ddc_ui::z::begin(ctx);
        self.mode_keys(ctx);
        let overlay = self.sh.palette.open || self.quick.menu.is_some() || self.chrome.mode.open;
        ui_kit::keys_frame_begin(ctx, overlay);
        self.poll_import(ctx);
        tasks_ui::frame(self, ctx, overlay);
        crate::bigfile::frame(self, ctx);
        self.handle_drops(ctx);
        if std::mem::take(&mut self.chrome.settings.reset_panels) {
            self.panels_reset(ctx);
        }
        if self.chrome.start.open {
            self.start_frame(ctx);
            load_win::window(self, ctx);
            ddc_ui::z::settle(ctx);
            return;
        }
        crate::edit3d::pre_input(self, ctx);
        self.shortcuts(ctx);
        self.clean_screen_esc(ctx);
        crate::clip_tools::frame(self, ctx);
        crate::ai::pump(self, ctx);
        self.autosave_tick(ctx);
        let cat = self.selection_category();
        self.sh.ribbon.set_context(cat);
        self.sync_caps_block();
        // a form over the canvas is modal: the plan takes no click or drag until it is closed
        crate::plan_view::lock_canvas(ctx, self.level_edit.is_some());
        let other_view = self.view3d || crate::sheet_view::active().is_some();
        crate::drafting_view::follow(self.view3d, self.section, self.ed.level);
        let drafting = crate::drafting_view::current(&self.ed)
            .filter(|_| crate::sheet_view::active().is_none());
        match drafting {
            Some(d) => crate::annot_text::sync_drafting(&mut self.ed, d),
            None => crate::annot_text::sync_view(&mut self.ed, other_view, self.section),
        }
        if self.autosave_open {
            self.autosave_window(ctx);
        }
        if self.import_open {
            self.import_window(ctx);
        }
        if self.settings_open {
            self.settings_window(ctx);
        }
        self.onboarding_window(ctx);
        if self.level_edit.is_some() {
            self.level_window(ctx);
        }
        crate::roof_edge_ui::ui(self, ctx);
        crate::rebar_ui::ui(self, ctx);
        crate::level_ask::ui(self, ctx);
        self.refresh_warnings();
        self.refresh_faults();
        if self.faults.open {
            self.faults_window(ctx);
        }
        if self.warnings_open {
            self.warnings_window(ctx);
        }
        if self.xlsx.is_some() {
            self.xlsx_window(ctx);
        }
        self.xlsx_job_ui(ctx);
        self.xlsx_load_ui(ctx);
        crate::edit_bind::windows(self, ctx);
        if self.clash.open {
            self.clash_window(ctx);
        }
        self.drawing_windows(ctx);
        crate::topo::window(self, ctx);
        crate::mep_browser::window(self, ctx);
        crate::circuit_edit::window(self, ctx);
        crate::mesh_tools::window(self, ctx);
        crate::source_ui::frame(self, ctx);
        crate::links_ui::frame(self, ctx);
        crate::data_ui::frame(self, ctx);
        crate::collab::frame(self, ctx);
        #[cfg(feature = "link")]
        crate::link::panel::frame(self, ctx);
        self.status_to_toast();
        let t = tokens(&self.sh.theme);
        let d = self.sh.theme.d();
        let wc = self.sh.theme.ribbon_class(ctx.screen_rect().width());
        let pl = workspace::arrange(&ws, ctx.screen_rect(), &self.sh.theme.tokens.breakpoint);
        let mut fire: Vec<String> = Vec::new();
        let shown = fold::shown(&self.prefs, self.chrome.fold.clean, ctx.screen_rect().width());
        let p = &self.prefs;
        let folds = (p.browser_open, p.props_open, fold::RibbonMode::of(p), p.fold);
        let sig = format!("{folds:?} {shown:?} {:?}", self.chrome.dock.peeks());
        let follow = self.chrome.fold.follow(sig, ctx.input(|i| i.time));
        self.plan.follow_chrome = follow;
        self.view.follow_chrome = follow;
        if ui_kit::dialog_just_opened(ctx) {
            self.chrome.fold.reveal = false;
            self.sh.ribbon.peek = false;
            self.quick.menu = None;
        }

        if shown.title {
            self.panel_title(ctx, &mut fire);
        }
        if ws.surface == Surface::Ribbon {
            if let Some(mode) = shown.ribbon {
                self.panel_ribbon(ctx, wc, mode, &mut fire);
            }
        }
        if matches!(ws.surface, Surface::Ribbon | Surface::Toolbar) {
            self.panel_tool_options(ctx, &t, shown.options, &mut fire);
        }
        crate::open::summary::frame(self, ctx);
        if ws.status {
            self.panel_status(ctx, shown.status);
            if shown.status {
                crate::cmdline::panel(self, ctx);
            }
        }
        crate::ai::mcp_bridge::frame(self, ctx);
        crate::ai::ui::show(self, ctx);
        self.panel_docks(ctx, &ws, &pl, &t, shown, &mut fire);
        if let Some(r) = pl.get(PanelId::DataTable).filter(|_| shown.docks) {
            self.panel_table(ctx, r.height());
        }
        let area = self.panel_views(ctx, rs, &ws, &pl, &t, &mut fire);
        self.chrome.tasks.canvas = Some(area);
        crate::rebar_ui::note_view_area(ctx, area);
        self.chrome.fold.canvas = Some(area);
        crate::ai::ui::overlay(self, ctx, area);
        crate::quick::frame(self, ctx, area, &mut fire);
        self.panel_overlays(ctx, &ws, area, &t, &mut fire);
        self.clean_screen_exit(ctx, area);
        let ru = self.sh.cat.lang == ddc_ui::Lang::Ru;
        crate::phase_ui::window(ctx, &mut self.ed, ru);
        crate::schedule::multi_ui::window(ctx, &mut self.ed, ru);
        crate::grid_set_ui::window(ctx, &mut self.ed, ru);
        crate::annot_text::window(ctx, &mut self.ed, self.sh.cat.lang);
        crate::drafting_view::window(self, ctx);
        crate::params_ui::window(ctx, &mut self.ed, &mut self.sh.toasts, ru);
        crate::view_gfx_ui::frame(self, ctx);
        crate::range_ui::frame(self, ctx);
        crate::find_replace::frame(self, ctx);
        crate::edit3d::wp_win::frame(self, ctx);
        crate::classify_ui::frame(self, ctx);
        crate::wall_profile_ui::frame(self, ctx);
        crate::fitting_insert::frame(self, ctx);
        crate::explain_ui::frame(self, ctx);
        crate::gen_rules_ui::frame(self, ctx);
        crate::where_is::frame(self, ctx);
        crate::view_cmds::frame(self, ctx);
        crate::query_win::frame(self, ctx);
        crate::scripts_win::frame(self, ctx);
        crate::help::frame(self, ctx);
        crate::ix_card::frame(self, ctx);
        crate::update::frame(self, ctx);
        report_win::frame(self, ctx);
        crate::issues_ui::frame(self, ctx);
        licenses_win::frame(self, ctx);
        crate::outline_cleanup::frame(self, ctx);
        self.first_run_frame(ctx);
        self.blocking_window(ctx);
        self.recover_window(ctx);
        crate::project_ui::frame(self, ctx);
        crate::save::frame(self, ctx);

        let s = &mut self.sh;
        let bottom = d.row + self.chrome.tasks.window_h;
        let clicked = widgets::toasts::show(ctx, &s.theme, &s.cat, &mut s.toasts, bottom);
        let entry = s.toasts.fired_entry.take();
        match clicked.as_deref() {
            Some(ddc_ui::notify::HISTORY_CMD) => status_bar::open_messages(ctx),
            Some(crate::ai::pin_toast::CMD) => crate::ai::pin_toast::fire(self),
            Some(c) if ddc_ui::notify::STEP_CMDS.contains(&c) => {
                // never a plain Ctrl+Z: that would take back whatever edit is on top now
                if let Some(e) = entry {
                    self.step_toast_entry(c == "edit.undo", e);
                }
            }
            Some(cmd) if self.sh.reg.get(cmd).is_some() => fire.push(cmd.to_string()),
            _ => {}
        }
        crate::query::palette_feed(self, ctx);
        let s = &mut self.sh;
        widgets::focus_ring(ctx, &s.theme);
        if s.palette.open {
            s.palette.live = s.ribbon.available.clone();
        }
        if let Some(id) = s.palette.show(ctx, &s.theme, &s.cat, &s.reg, &mut s.icons) {
            fire.push(id);
        }
        ddc_ui::z::settle(ctx);
        for id in fire {
            if !crate::query::palette_pick(self, &id) {
                self.run(&id);
            }
        }
        // an open or import started by a command this frame shows its window in this frame
        load_win::window(self, ctx);
    }

    /// First run / empty model: welcome card over the plan — new, open, import, recent files
    /// and the keys worth knowing. Disappears with the first wall or any action.
    pub(crate) fn welcome(
        &mut self,
        ctx: &egui::Context,
        area: egui::Rect,
        fire: &mut Vec<String>,
    ) {
        let ed = &self.ed;
        let show = !(self.welcome_closed
            || self.chooser_visible()
            || self.view3d
            || !ed.walls.is_empty()
            || !ed.curtains.is_empty()
            || !ed.slabs.is_empty()
            || !ed.bodies.is_empty()
            || !ed.rooms.is_empty()
            || ed.opening_mode
            || ed.sketch.is_some()
            || ed.shape.is_some()
            || ed.model_tool().is_some()
            // the wall tool is armed on an empty plan from the start; any other tool hides it
            || self.active_command().is_some_and(|c| c != "arch.wall")
            || !ed.chain().is_empty()
            || crate::open::jobs(self).busy()
            || self.import_open
            // the assistant is open: the person chose to start by asking, not by the card
            || crate::ai::ui::panel_shown()
            || self.plan.drawings.import_open
            || self.plan.drawings.has_pending());
        // the plan's own hint card stays hidden under the first-run chooser too
        self.plan.welcome = show || self.chooser_visible();
        if !show {
            return;
        }
        // Esc dismisses the card for good; an open menu draws alone, the card waits under it
        let free = !ctx.wants_keyboard_input() && ctx.memory(|m| m.top_modal_layer().is_none());
        if free && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.welcome_closed = true;
            self.plan.welcome = self.chooser_visible();
            return;
        }
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        // an open window (settings, recovery, a job) owns the screen: the card waits for it
        let card = egui::Id::new("ddc.welcome");
        let last = ctx.memory(|m| m.area_rect(card)).unwrap_or(area);
        if crate::ui_kit::window_over(ctx, last) {
            return;
        }
        // a press on the canvas beside the card dismisses it for good and still reaches the tool
        let pressed = ctx.input(|i| {
            i.pointer
                .press_origin()
                .filter(|_| i.pointer.button_pressed(egui::PointerButton::Primary))
        });
        if pressed.is_some_and(|p| area.contains(p) && !last.contains(p)) {
            self.welcome_closed = true;
            self.plan.welcome = self.chooser_visible();
            return;
        }
        let th = &self.sh.theme;
        let w = 600.0f32.min(area.width() - 24.0);
        if w < 320.0 {
            return;
        }
        let cat = &self.sh.cat;
        let tile = |id: &str| {
            let c = self.sh.reg.get(id);
            let keys = c
                .map(|c| {
                    c.shortcuts
                        .first()
                        .cloned()
                        .or(c.mnemonic.as_deref().map(mnemonic_keys))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            (
                id.to_string(),
                c.map(|c| c.icon.clone()).unwrap_or_default(),
                keys,
            )
        };
        // "New model" opens the start screen: units and template, then a real project
        let mut new_tile = tile("file.new");
        new_tile.0 = "ui.start".into();
        let tiles = [
            // the start page's words: one name for each action wherever it is offered
            (new_tile, cat.t("start.new"), cat.t("welcome.new_sub")),
            (tile("file.open"), cat.t("start.open"), cat.t("welcome.open_sub")),
            (
                tile("mgr.import_cad"),
                cat.t("welcome.import"),
                if ddc_io::dxf::DWG_SUPPORTED {
                    ".rvt .ifc .dwg .dxf .pdf .xlsx".to_string()
                } else {
                    ".rvt .ifc .dxf .pdf .xlsx".to_string()
                },
            ),
        ];
        let lead = cat.t("welcome.lead");
        let (tutorial, start_wall) = (cat.t("welcome.tutorial"), cat.t("welcome.wall"));
        let (recent_title, recent_empty) = (cat.t("welcome.recent"), cat.t("welcome.recent_empty"));
        let hints = [
            ("Ctrl+K", cat.t("welcome.k_all")),
            ("W A", cat.t("welcome.k_wall")),
            ("W R", cat.t("welcome.k_room")),
            ("Ctrl+0", cat.t("welcome.k_fit")),
        ];
        let recent = self.prefs.recent.clone();
        let mut close = false;
        egui::Area::new(egui::Id::new("ddc.welcome"))
            .order(egui::Order::Middle)
            .fade_in(false)
            .fixed_pos(egui::pos2(
                (area.center().x - w / 2.0).round(),
                (area.top() + (area.height() * 0.14).clamp(24.0, 120.0)).round(),
            ))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(th.c("bg.elevated"))
                    .stroke(egui::Stroke::new(1.0, th.c("border.overlay")))
                    .corner_radius(th.radius("lg"))
                    .shadow(th.shadow(3))
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_width(w - 48.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(
                            RichText::new(crate::brand::PRODUCT)
                                .font(th.font(Role::Heading))
                                .color(th.c("text.primary")),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(lead.as_str())
                                .font(th.font(Role::Body))
                                .color(th.c("text.secondary")),
                        );
                        ui.add_space(16.0);
                        let gap = 8.0;
                        let tw = ((w - 48.0) - 2.0 * gap) / 3.0;
                        // a caption wraps inside its tile; the tiles share one height
                        let subs: Vec<_> = tiles
                            .iter()
                            .map(|(_, _, sub)| {
                                ui.painter().layout(
                                    sub.clone(),
                                    th.font(Role::Caption),
                                    th.c("text.secondary"),
                                    tw - 24.0,
                                )
                            })
                            .collect();
                        let sub_h = subs.iter().map(|g| g.size().y).fold(0.0, f32::max);
                        let tile_h = (64.0 + sub_h + 12.0).max(88.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for (((id, icon, keys), title, _), sub) in tiles.iter().zip(subs) {
                                let (r, resp) = ui.allocate_exact_size(
                                    egui::vec2(tw, tile_h),
                                    egui::Sense::click(),
                                );
                                let t = ui.ctx().animate_bool_with_time(
                                    resp.id,
                                    resp.hovered(),
                                    ui.style().animation_time,
                                );
                                let border =
                                    th.c("border.subtle").lerp_to_gamma(th.c("accent"), t * 0.8);
                                ui.painter().rect(
                                    r,
                                    th.radius("md"),
                                    th.hover_fill().gamma_multiply(t),
                                    egui::Stroke::new(1.0, border),
                                    egui::StrokeKind::Inside,
                                );
                                let px = ddc_ui::theme::icon_size::MD;
                                let ic = egui::Rect::from_min_size(
                                    r.min + egui::vec2(12.0, 12.0),
                                    egui::Vec2::splat(px),
                                );
                                self.sh.icons.paint(ui, icon, px as u32, ic, th.c("accent"));
                                let tg = widgets::elide(
                                    ui,
                                    title,
                                    th.font(Role::BodyStrong),
                                    th.c("text.primary"),
                                    tw - 24.0,
                                );
                                ui.painter().galley(
                                    r.min + egui::vec2(12.0, 44.0),
                                    tg,
                                    th.c("text.primary"),
                                );
                                ui.painter().galley(
                                    r.min + egui::vec2(12.0, 64.0),
                                    sub,
                                    th.c("text.secondary"),
                                );
                                if !keys.is_empty() {
                                    widgets::paint_keycaps(
                                        ui.painter(),
                                        th,
                                        egui::pos2(r.right() - 10.0, r.top() + 22.0),
                                        keys,
                                    );
                                }
                                if resp
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    fire.push(id.clone());
                                    close = true;
                                }
                            }
                        });
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.link(tutorial.as_str()).clicked() {
                                fire.push("help.tutorial".into());
                                close = true;
                            }
                            ui.label(RichText::new("·").color(th.c("text.disabled")));
                            if ui.link(start_wall.as_str()).clicked() {
                                fire.push("arch.wall".into());
                                close = true;
                            }
                        });
                        ui.add_space(16.0);
                        ui.label(
                            RichText::new(recent_title.as_str())
                                .font(th.font(Role::Caption))
                                .color(th.c("text.secondary")),
                        );
                        ui.add_space(6.0);
                        if recent.is_empty() {
                            ui.label(
                                RichText::new(recent_empty.as_str())
                                    .font(th.font(Role::Body))
                                    .color(th.c("text.disabled")),
                            );
                        }
                        for path in recent.iter().take(4) {
                            let (r, resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 28.0),
                                egui::Sense::click(),
                            );
                            widgets::paint_state(ui, th, r, &resp, false);
                            welcome_recent_row(ui, th, r, path);
                            let resp = resp.on_hover_text(path.as_str());
                            if resp.clicked() {
                                if crate::project::is_project_file(std::path::Path::new(path)) {
                                    self.project_ui.open_request = Some(path.into());
                                } else {
                                    self.import_path = path.clone();
                                    fire.push("mgr.import_cad".into());
                                }
                                close = true;
                            }
                        }
                        ui.add_space(20.0);
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 1.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().hline(
                            r.x_range(),
                            r.center().y,
                            egui::Stroke::new(1.0, th.c("border.subtle")),
                        );
                        ui.add_space(12.0);
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 20.0),
                            egui::Sense::hover(),
                        );
                        // keycap hints, left to right
                        let mut x = r.left();
                        for (keys, what) in hints {
                            let p = ui.painter();
                            let kw = widgets::keycaps_width(p, th, keys);
                            let tg = p.layout_no_wrap(
                                what,
                                th.font(Role::Caption),
                                th.c("text.secondary"),
                            );
                            let tw = tg.size().x;
                            // a hint that does not fit whole is left out, never cut
                            if x + kw + 4.0 + tw > r.right() {
                                break;
                            }
                            widgets::paint_keycaps(p, th, egui::pos2(x + kw, r.center().y), keys);
                            p.galley(
                                egui::pos2(x + kw + 4.0, r.center().y - tg.size().y / 2.0),
                                tg,
                                th.c("text.secondary"),
                            );
                            x += kw + 4.0 + tw + 16.0;
                        }
                    });
            });
        if close {
            self.welcome_closed = true;
        }
    }

    /// Tooltip of the title-bar undo/redo buttons: what the click will take, or that nothing is left.
    pub fn undo_redo_tip(&self, undo: bool) -> String {
        let id = if undo { "edit.undo" } else { "edit.redo" };
        let name = self.undo_redo_name(undo);
        match self.sh.reg.get(id).map(|c| c.shortcuts.join(" / ")) {
            Some(s) if !s.is_empty() => format!("{name}  ·  {s}"),
            _ => name,
        }
    }

    /// "Undo: <step>" / "Nothing to undo" (and the redo pair), for buttons and menu rows.
    pub fn undo_redo_name(&self, undo: bool) -> String {
        match self.ed.next_label(undo) {
            Some(w) => {
                let k = if undo { "undo_label.undo_what" } else { "undo_label.redo_what" };
                crate::tr(k, &[("what", w.as_str())])
            }
            None => crate::tr(if undo { "undo_label.undo_none" } else { "undo_label.redo_none" }, &[]),
        }
    }

    fn icon_cmd(&mut self, ui: &mut egui::Ui, id: &str, on: bool, enabled: bool) -> bool {
        let tip = match id {
            "edit.undo" => self.undo_redo_tip(true),
            "edit.redo" => self.undo_redo_tip(false),
            _ => self.tip(id),
        };
        let icon = self
            .sh
            .reg
            .get(id)
            .map(|c| c.icon.clone())
            .unwrap_or_default();
        let s = &mut self.sh;
        ui.add_enabled_ui(enabled, |ui| {
            widgets::icon_button(ui, &mut s.icons, &s.theme, &icon, on, &tip).clicked()
        })
        .inner
    }

    /// System / light / dark: the preference, or the OS setting when it is «system».
    fn resolve_theme(&mut self, ctx: &egui::Context) {
        use workspace::ThemeMode;
        let user = match self.prefs.theme.as_str() {
            "dark" => ThemeMode::Dark,
            "light" => ThemeMode::Light,
            _ => ThemeMode::System,
        };
        let system = match ctx.system_theme() {
            Some(egui::Theme::Dark) => ddc_ui::Mode::Dark,
            _ => ddc_ui::Mode::Light,
        };
        self.sh.theme.mode = workspace::resolve_mode(&self.workspace(), user, system);
    }

    /// Active workspace; unknown ids (custom layouts not loaded yet) fall back to classic.
    pub fn workspace(&self) -> Workspace {
        self.layouts
            .iter()
            .find(|w| w.id == self.layout)
            .cloned()
            .or_else(|| workspace::preset(&self.layout))
            .unwrap_or_else(|| workspace::presets().remove(0))
    }

    /// Custom layouts from `workspaces.json` next to prefs.txt; a broken file is set aside.
    pub fn load_layouts(&mut self) {
        let Some(dir) = self.prefs.path.as_ref().and_then(|p| p.parent()) else {
            return;
        };
        let l = workspace::load(dir);
        self.layouts = l.all;
        if let Some(bad) = l.moved_bad {
            self.sh.toasts.push(
                Level::Warning,
                crate::tr("type_dialog.shell.layouts_bad", &[("path", bad.display().to_string().as_str())]),
                None,
            );
        }
    }

    /// Switch layout: only `layout` changes, so selection, tool, undo and cameras survive.
    pub fn set_layout(&mut self, id: &str) {
        if id == self.layout || !self.layouts.iter().any(|w| w.id == id) {
            return;
        }
        let shape = |a: &Self| std::mem::discriminant(&a.workspace().views);
        let before = shape(self);
        self.layout_prev = Some(std::mem::replace(&mut self.layout, id.to_string()));
        // another split: the plan view of each shape is kept and put back; a shape not seen yet
        // keeps the current view
        if shape(self) != before {
            let key = |d: std::mem::Discriminant<_>| format!("{d:?}");
            let (old, new) = (key(before), key(shape(self)));
            let cur = (self.plan.center, self.plan.scale);
            match self.plan_by_shape.iter_mut().find(|e| e.0 == old) {
                Some(e) => (e.1, e.2) = cur,
                None => self.plan_by_shape.push((old, cur.0, cur.1)),
            }
            if let Some(e) = self.plan_by_shape.iter().find(|e| e.0 == new) {
                (self.plan.center, self.plan.scale) = (e.1, e.2);
            }
        }
        self.sh.palette.open = false;
        if self.prefs.layout != id {
            self.prefs.layout = id.to_string();
            self.prefs.save();
        }
        self.chrome.mode.close();
        self.layout_toast();
    }

    pub fn open_settings(&mut self, page: u8) {
        self.settings_open = true;
        self.settings_page = page;
    }

    fn chooser_visible(&self) -> bool {
        self.force_chooser
    }
}

/// A mnemonic is typed letter by letter: "IC" shows as two keycaps, like "W A".
fn mnemonic_keys(m: &str) -> String {
    let letters: Vec<String> =
        m.chars().filter(|c| !c.is_whitespace()).map(String::from).collect();
    letters.join(" ")
}

/// One recent row of the welcome card: the file name, then its folder cut in the middle so
/// the whole row stays inside `r`; no folder when there is no room for a useful part.
fn welcome_recent_row(ui: &egui::Ui, th: &Theme, r: egui::Rect, path: &str) {
    let pad = 8.0;
    let name = std::path::Path::new(path)
        .file_name()
        .map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
    let inner = r.shrink2(egui::vec2(pad, 0.0));
    let ng = widgets::elide(
        ui,
        &name,
        th.font(Role::Body),
        th.c("text.primary"),
        inner.width() * 0.6,
    );
    let name_w = ng.size().x;
    ui.painter().galley(
        egui::pos2(inner.left(), r.center().y - ng.size().y / 2.0),
        ng,
        th.c("text.primary"),
    );
    let room = inner.width() - name_w - 2.0 * pad;
    if room < 48.0 {
        return;
    }
    let font = th.font(Role::Caption);
    let color = th.c("text.secondary");
    let width = |s: &str| {
        let g = ui.painter().layout_no_wrap(s.to_owned(), font.clone(), color);
        g.size().x
    };
    let shown = crate::ui_kit::banner::mid_ellipsis(path, |s| width(s) <= room);
    let pg = ui.painter().layout_no_wrap(shown, font.clone(), color);
    ui.painter().galley(
        egui::pos2(inner.right() - pg.size().x, r.center().y - pg.size().y / 2.0),
        pg,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    /// Every command the editor wires is in the registry and has a rasterizable icon.
    #[test]
    fn wired_commands_exist_with_icons() {
        let a = app();
        for id in [
            "edit.undo",
            "edit.redo",
            "view.plan",
            "view.3d",
            "arch.wall",
            "arch.door",
            "arch.window",
        ] {
            let c = a.sh.reg.get(id).unwrap_or_else(|| panic!("{id}"));
            assert!(ddc_ui::icons::source(&c.icon).is_some(), "{id}: {}", c.icon);
            assert!(!a.tip(id).is_empty());
        }
        assert!(ddc_ui::icons::source("room").is_some());
        // every wired command runs (no "not available" toast) and has a drawn icon
        for id in WIRED {
            let c = a.sh.reg.get(id).unwrap_or_else(|| panic!("{id}"));
            assert!(
                c.icon.is_empty() || ddc_ui::icons::source(&c.icon).is_some(),
                "{id}"
            );
        }
        assert!(ddc_ui::icons::source("select").is_some());
        assert!(a.tip("edit.undo").contains("Ctrl+Z"));
    }

    /// A key the tooltips advertise must run something, never toast «unavailable».
    #[test]
    fn catalog_shortcuts_all_have_handlers() {
        let a = app();
        let bad: Vec<&str> = a
            .sh
            .reg
            .all()
            .iter()
            .filter(|c| !c.shortcuts.is_empty() && !a.sh.ribbon.enabled(&a.sh.reg, &c.id))
            .map(|c| c.id.as_str())
            .collect();
        assert!(bad.is_empty(), "shortcut without a handler: {bad:?}");
    }

    /// Buttons that were greyed out although the feature exists now run.
    #[test]
    fn former_dead_buttons_are_enabled() {
        let a = app();
        for id in [
            "mod.cut_geom",
            "mod.measure",
            "view.schedule",
            "data.table",
            "data.compare",
            "view.diff",
            "mgr.snaps",
            "mgr.load",
            "view.zoom_sel",
            "panel.browser",
            "panel.properties",
            "panel.issues",
            "app.find",
            "view.maximize",
        ] {
            assert!(a.sh.ribbon.enabled(&a.sh.reg, id), "{id}");
        }
    }

    /// Whatever the palette lists on an empty or typed query can run.
    #[test]
    fn palette_rows_are_all_runnable() {
        let mut a = app();
        a.sh.palette.live = a.sh.ribbon.available.clone();
        let mut queries = vec![String::new(), "view".to_string(), "mgr".to_string()];
        queries.extend(a.sh.reg.all().iter().map(|c| a.sh.cat.t(&c.label_key())));
        for q in queries {
            a.sh.palette.query = q.clone();
            for id in a.sh.palette.rows(&a.sh.reg, &a.sh.cat) {
                assert!(a.sh.ribbon.enabled(&a.sh.reg, &id), "{id} for query {q:?}");
            }
        }
        assert!(a.sh.ribbon.enabled(&a.sh.reg, "mgr.scripts"));
        // a command without a handler stays out of the palette
        let off: Vec<String> = a
            .sh
            .reg
            .all()
            .iter()
            .map(|c| c.id.clone())
            .filter(|id| !a.sh.ribbon.enabled(&a.sh.reg, id))
            .collect();
        for id in off {
            a.sh.palette.query = a.sh.cat.t(&format!("cmd.{id}"));
            assert!(!a.sh.palette.rows(&a.sh.reg, &a.sh.cat).contains(&id), "{id}");
        }
    }

    #[test]
    fn f11_toggles_fullscreen_flag() {
        let mut a = app();
        assert!(!a.chrome.full);
        assert!(a.run("view.maximize"));
        assert!(a.chrome.full);
        assert!(a.run("view.maximize"));
        assert!(!a.chrome.full);
    }

    /// Ctrl+F no longer opens the palette (Ctrl+K does); the find window handles it.
    #[test]
    fn ctrl_f_leaves_palette_closed() {
        let mut a = app();
        assert!(a.run("app.find"));
        assert!(!a.sh.palette.open);
    }

    #[test]
    fn data_table_switches_layout() {
        let mut a = app();
        assert_eq!(a.layout, "classic");
        assert!(a.run("data.table"));
        assert_eq!(a.layout, "data");
        assert!(a.run(chrome::LAYOUT_BACK));
        assert_eq!(a.layout, "classic");
    }

    /// Next tab walks plan, 3D and the open sections round; close shuts only a view tab.
    #[test]
    fn next_and_close_walk_the_view_tabs() {
        use tab_strip::OpenTab;
        let mut a = app();
        let s = crate::editor::sec::spec_from("S1", ddc_geom::v2(0.0, 0.0), ddc_geom::v2(5.0, 0.0), None)
            .unwrap();
        let id = a.ed.add_view(&s).unwrap();
        a.open_view(id);
        assert!(a.tabs.contains(&id));
        assert_eq!(a.open_tab(), OpenTab::View(id));
        let mut seen = vec![];
        for _ in 0..3 {
            assert!(a.run("view.next"));
            seen.push(a.open_tab());
        }
        assert_eq!(seen, [OpenTab::Plan, OpenTab::D3, OpenTab::View(id)]);
        assert!(a.run("view.close"));
        assert!(!a.tabs.contains(&id));
        assert_eq!(a.open_tab(), OpenTab::Plan);
        // the plan tab has no close: nothing changes
        assert!(a.run("view.close"));
        assert_eq!(a.open_tab(), OpenTab::Plan);
    }

    /// An app with one undoable step; Ctrl+Z is fed through `shortcuts`.
    fn undo_depth_after_ctrl_z(setup: impl FnOnce(&mut EditorApp)) -> (usize, usize) {
        let mut a = app();
        let f = crate::editor::OpFn(std::sync::Arc::new(
            |m: &mut ddc_model::Model| -> Result<(), ddc_model::Error> {
                m.add_level("ZZ", 9.0)?;
                Ok(())
            },
        ));
        a.ed.run_op(f).unwrap();
        setup(&mut a);
        let before = a.ed.undo_len();
        let mods = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: Some(egui::Key::Z),
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            modifiers: mods,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(input, |ctx| a.shortcuts(ctx));
        (before, a.ed.undo_len())
    }

    #[test]
    fn ctrl_z_fires_without_a_blocking_window() {
        let (before, after) = undo_depth_after_ctrl_z(|_| {});
        assert!(after < before, "{before} -> {after}");
    }

    #[test]
    fn ctrl_z_ignored_while_blocking() {
        let (before, after) = undo_depth_after_ctrl_z(|a| a.block_error("x", vec![]));
        assert_eq!(before, after);
    }

    #[test]
    fn ctrl_z_ignored_while_recover() {
        let (before, after) =
            undo_depth_after_ctrl_z(|a| a.recover = Some(recover_win::Recover::default()));
        assert_eq!(before, after);
    }

    #[test]
    fn ctrl_z_ignored_while_project_notice() {
        let (before, after) = undo_depth_after_ctrl_z(|a| {
            crate::project_ui::notify_kept(a, &[std::path::PathBuf::from("dir/a.ddc")]);
        });
        assert_eq!(before, after);
    }

    #[test]
    fn chords_work_after_close() {
        let mut a = app();
        assert!(!a.modal_up());
        a.block_error("x", vec![]);
        a.recover = Some(recover_win::Recover::default());
        assert!(a.modal_up());
        a.blocking = None;
        assert!(a.modal_up(), "recovery alone still blocks");
        a.recover = None;
        assert!(!a.modal_up());
    }

    #[test]
    fn run_routes_tools_and_views() {
        let mut a = app();
        assert!(a.run("arch.wall"));
        assert_eq!(a.ed.wall_mode(), Mode::Polyline);
        assert_eq!(a.active_command(), Some("arch.wall"));
        assert!(a.run("view.3d"));
        assert!(a.view3d);
        assert!(a.run("view.plan"));
        assert!(!a.view3d);
        assert!(!a.run("test.unwired"));
        assert_eq!(a.sh.toasts.live.len(), 1);
    }

    fn frame(a: &mut EditorApp, ctx: &egui::Context, t: f64, ev: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1366.0, 768.0),
            )),
            time: Some(t),
            events: ev,
            ..Default::default()
        };
        let _ = ctx.run(input, |c| a.ui(c, None));
    }

    fn letter(k: egui::Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Two letters within 800 ms run the command; a late second letter does not.
    #[test]
    fn two_letter_shortcuts_through_window() {
        let mut a = app();
        a.ed.set_mode(Mode::Select);
        let ctx = egui::Context::default();
        frame(&mut a, &ctx, 0.0, vec![]);
        frame(&mut a, &ctx, 0.1, vec![letter(egui::Key::W)]);
        assert_eq!(a.sh.echo, "W…");
        frame(&mut a, &ctx, 0.5, vec![letter(egui::Key::A)]);
        assert_eq!(a.ed.wall_mode(), Mode::Polyline);
        // a tool owns plain letters: no sequence while it runs
        frame(&mut a, &ctx, 1.0, vec![letter(egui::Key::M)]);
        assert!(a.seq.pending().is_none());
        a.ed.set_mode(Mode::Select);
        frame(&mut a, &ctx, 2.0, vec![letter(egui::Key::W)]);
        frame(&mut a, &ctx, 3.0, vec![]);
        assert!(a.sh.echo.is_empty(), "timeout clears the echo");
        frame(&mut a, &ctx, 3.1, vec![letter(egui::Key::A)]);
        assert_eq!(a.ed.wall_mode(), Mode::Select);
    }

    #[test]
    fn level_tool_exits_when_leaving_the_section() {
        let mut a = app();
        a.section = Some(7);
        assert!(a.run("arch.level") && a.sview.level_tool);
        assert!(
            a.level_edit.is_none(),
            "in a section it is the tool, not the dialog"
        );
        assert_eq!(a.active_command(), Some("arch.level"));
        // zoom does not clear the tool
        a.run("view.zoom_fit");
        assert!(a.sview.level_tool);
        // section change outside of commands (panel, tab)
        a.section = Some(8);
        a.sync_level_tool();
        assert!(!a.sview.level_tool);
        a.section = Some(7);
        a.run("arch.level");
        a.section = None;
        a.sync_level_tool();
        assert!(!a.sview.level_tool, "back to the plan");
        // another command
        a.section = Some(7);
        a.run("str.level");
        assert!(a.sview.level_tool, "same tool by the structure tab");
        a.run("arch.wall");
        assert!(!a.sview.level_tool);
        a.section = Some(7);
        a.run("arch.level");
        a.run("view.plan");
        assert!(!a.sview.level_tool);
    }

    /// Annotation and dimension tools name themselves in the option bar (not «Wall»), and a tool
    /// run from the palette shows its own ribbon tab.
    #[test]
    fn tools_name_themselves_and_show_their_tab() {
        let mut a = app();
        for (id, want) in [
            ("ann.detail", "ann.detail"),
            ("ann.text", "ann.text"),
            ("ann.cloud", "ann.cloud"),
            ("ann.dim_angular", "ann.dim_angular"),
            ("ann.dim_radial", "ann.dim_radial"),
            ("ann.dim", "ann.dim"),
        ] {
            assert!(a.run(id), "{id}");
            assert_eq!(a.active_command(), Some(want), "{id}");
            // the bar must not offer the wall type and height to a tool that has none
            assert!(crate::annot_text::armed() || a.plan.b.dim.is_some(), "{id}");
            a.ed.set_mode(Mode::Select);
            a.plan.b.dim = None;
            crate::annot_text::cancel(&mut a.ed);
        }
        let known = a.sh.reg.tabs();
        for id in ["ann.detail", "mep.fixture", "mep.duct", "str.column", "ann.dim"] {
            let Some(tab) = a.sh.reg.get(id).and_then(|c| c.tab.clone()) else {
                continue;
            };
            a.sh.ribbon.set_tab("architecture", &known);
            assert!(a.run(id), "{id}");
            assert_eq!(a.active_command(), Some(id), "{id} is the active tool");
            assert_eq!(a.sh.ribbon.tab, tab, "{id} shows its own tab");
            a.ed.set_mode(Mode::Select);
            a.plan.b.dim = None;
            crate::annot_text::cancel(&mut a.ed);
        }
    }

    #[test]
    fn undo_redo_name_follows_the_stacks() {
        let a = app();
        let none = crate::tr("undo_label.undo_none", &[]);
        assert_eq!(a.undo_redo_name(true), none);
        assert_eq!(a.undo_redo_name(false), crate::tr("undo_label.redo_none", &[]));
        assert!(a.undo_redo_tip(true).starts_with(&none));
    }

    #[test]
    fn aliases_handlers_and_context_tab() {
        let mut a = app();
        assert!(
            a.run("str.level"),
            "alias of arch.level; outside a section opens the dialog"
        );
        assert!(a.level_edit.is_some());
        assert!(a.run("mgr.autosave") && a.autosave_open);
        assert!(a.sh.ribbon.enabled(&a.sh.reg, "mgr.warnings"));
        // ann.tag has a handler since u3; the synthetic id never gets one outside this test
        assert!(a.sh.ribbon.enabled(&a.sh.reg, "ann.tag"));
        assert!(!a.sh.ribbon.enabled(&a.sh.reg, "test.synthetic"));
        fn tag(a: &mut EditorApp) {
            a.hint = Some(("tag".into(), "end".into()));
        }
        a.register_command("test.synthetic", tag);
        assert!(a.sh.ribbon.enabled(&a.sh.reg, "test.synthetic"));
        assert!(a.run("test.synthetic"));
        assert_eq!(a.hint.as_ref().map(|h| h.0.as_str()), Some("tag"));
        // selection opens the green «Modify | Walls» tab, clearing returns
        let ctx = egui::Context::default();
        a.level_edit = None;
        a.autosave_open = false;
        a.run("arch.wall_room");
        frame(&mut a, &ctx, 0.0, vec![]);
        let w = a.ed.walls.first().map(|w| w.id);
        if let Some(w) = w {
            a.ed.selection = vec![Sel::Wall(w)];
        } else {
            a.ed.selection = vec![Sel::Wall(1)];
        }
        frame(&mut a, &ctx, 0.1, vec![]);
        assert_eq!(a.sh.ribbon.tab, ddc_ui::widgets::ribbon::CTX_TAB);
        assert_eq!(a.sh.ribbon.context.as_ref().unwrap().1, crate::app2_fx::fx("shell_walls"));
        a.ed.selection.clear();
        frame(&mut a, &ctx, 0.2, vec![]);
        assert_ne!(a.sh.ribbon.tab, ddc_ui::widgets::ribbon::CTX_TAB);
    }

    /// Every key the sources ask the catalog for exists in ru and en (no raw keys).
    #[test]
    fn ui_keys_resolve_ru_en() {
        let a = app();
        let srcs = [
            include_str!("shell.rs"),
            include_str!("shell/warn.rs"),
            include_str!("shell/recover_win.rs"),
            include_str!("shell/start_win.rs"),
            include_str!("shell/settings_win.rs"),
            include_str!("shell/settings_win/branding.rs"),
            include_str!("shell/tasks_ui.rs"),
            include_str!("shell/dock_ui.rs"),
            include_str!("shell/tree_ux.rs"),
            include_str!("shell/prop_rows.rs"),
        ];
        let mut keys = vec![];
        for src in srcs {
            for pat in ["t(\"", "tf(\"", "plural(\""] {
                let mut parts = src.split(pat);
                let mut prev = parts.next().unwrap_or("");
                for part in parts {
                    // `from_id_salt("…")` ends in `t("` too: only whole identifiers count
                    let call = !prev.ends_with(|c: char| c.is_alphanumeric() || c == '_');
                    prev = part;
                    let k = part.split('"').next().unwrap_or("");
                    if call && k.contains('.') && !k.contains(' ') && !k.contains('{') {
                        keys.push(k.to_string());
                    }
                }
            }
        }
        assert!(keys.len() > 40, "{}", keys.len());
        let mut tools: Vec<(&str, &str)> = vec![
            ("select", "idle"),
            ("opening", "place"),
            ("section", "first"),
        ];
        for t in ["wall", "rect", "curtain"] {
            tools.push((t, "first"));
            tools.push((t, "next"));
        }
        keys.extend(tools.iter().map(|(t, s)| hint_key(t, s)));
        for k in [
            "overlap",
            "system",
            "dim_ref",
            "duplicate",
            "stair_level",
            "cut",
            "sketch",
            "other",
        ] {
            keys.push(format!("warn.kind.{k}"));
        }
        for k in ["wall", "opening", "curtain", "floor", "room", "mass"] {
            keys.push(format!("selcat.{k}"));
        }
        let c = &a.sh.cat;
        for k in keys {
            for l in [Lang::Ru, Lang::En] {
                assert!(c.table(l).contains_key(&k), "{l:?}: {k}");
            }
        }
    }

    /// Autosave of the in-memory model into the configured folder, indicator text.
    #[test]
    fn autosave_flush_and_label() {
        let mut a = app();
        let d = std::env::temp_dir().join(format!("ddc-app-as-{}", std::process::id()));
        a.prefs.autosave_dir = d.display().to_string();
        assert_eq!(a.autosave_label().0, crate::app2_fx::fx("shell_no_changes"));
        a.run("arch.wall_room");
        let p = a.flush_autosave().expect("snapshot written");
        assert!(p.exists());
        assert!(a.autosave_label().0.starts_with(&crate::app2_fx::fx("shell_saved_prefix")));
        a.prefs.autosave_dir = "Z:\\\0bad".into();
        assert!(a.flush_autosave().is_none());
        assert_eq!(
            a.autosave_label(),
            (crate::app2_fx::fx("shell_autosave_failed"), true)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Whole window, headless: palette opens on Ctrl+K and Ctrl+Shift+P; no panics on 2 frames.
    #[test]
    fn window_frames_headless_palette_shortcuts() {
        let mut a = app();
        let ctx = egui::Context::default();
        let run = |a: &mut EditorApp, ev: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1366.0, 768.0),
                )),
                events: ev,
                ..Default::default()
            };
            let _ = ctx.run(input, |c| a.ui(c, None));
        };
        let key = |k, m| egui::Event::Key {
            key: k,
            physical_key: Some(k),
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        run(&mut a, vec![]);
        run(&mut a, vec![key(egui::Key::K, egui::Modifiers::COMMAND)]);
        assert!(a.sh.palette.open);
        run(&mut a, vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
        assert!(!a.sh.palette.open);
        let csp = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        run(&mut a, vec![key(egui::Key::P, csp)]);
        assert!(a.sh.palette.open);
    }

    /// Keyboard scenario through the window: "6000 Enter", then "<30 Enter", "3000 Enter".
    #[test]
    fn typed_length_and_angle_through_window() {
        use crate::wall_tool::Event;
        let mut a = app();
        let ctx = egui::Context::default();
        let run = |a: &mut EditorApp, ev: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                events: ev,
                ..Default::default()
            };
            let _ = ctx.run(input, |c| a.ui(c, None));
        };
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let text = |s: &str| egui::Event::Text(s.into());
        assert!(a.run("arch.wall"));
        run(&mut a, vec![]);
        a.ed.handle(Event::Click(ddc_geom::v2(0.0, 0.0)));
        run(&mut a, vec![text("6000"), enter.clone()]);
        assert_eq!(a.ed.wall_draft().points.last(), Some(&ddc_geom::v2(6.0, 0.0)));
        run(&mut a, vec![text("<30"), enter.clone()]);
        run(&mut a, vec![text("3000"), enter]);
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        run(&mut a, vec![esc]);
        assert!((wall_length(&a.ed.walls[0]) - 6.0).abs() < 1e-9);
        assert_eq!(a.ed.walls.len(), 2);
        let b = a.ed.walls[1].b;
        let r = 30f64.to_radians();
        assert!((b.x - (6.0 + 3.0 * r.cos())).abs() < 1e-6, "{b:?}");
        assert!((b.y - 3.0 * r.sin()).abs() < 1e-6, "{b:?}");
    }

    #[test]
    fn panel_toast_names_the_cell_and_model_header() {
        let mut a = app();
        a.ed.add_curtains(&[(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0)]);
        let cw = a.ed.curtains[0].id;
        a.ed.set_panel_kind(cw, 1, ddc_model::PanelKind::Window);
        let s = a.ed.status.clone().unwrap();
        assert!(s.starts_with(&crate::app2_fx::fx("shell_panel_prefix")) && s.ends_with(&crate::app2_fx::fx("shell_window_suffix")), "{s}");
    }

    /// T3: cycle 1→5→1 through hotkeys keeps selection, tool with its first point, camera,
    /// undo depth; undo after the cycle reverts the same edit. Every layout renders headless.
    #[test]
    fn layout_cycle_keeps_state() {
        use crate::wall_tool::Event;
        let mut a = app();
        let ctx = egui::Context::default();
        let run = |a: &mut EditorApp, ev: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1920.0, 1080.0),
                )),
                events: ev,
                ..Default::default()
            };
            let _ = ctx.run(input, |c| a.ui(c, None));
        };
        a.ed.add_curtains(&[(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0)]);
        a.ed.add_curtains(&[(ddc_geom::v2(0.0, 3.0), ddc_geom::v2(6.0, 3.0), 0.0)]);
        a.ed.selection = vec![Sel::Curtain(a.ed.curtains[0].id)];
        assert!(a.run("arch.wall"));
        a.ed.handle(Event::Click(ddc_geom::v2(1.0, 1.0)));
        run(&mut a, vec![]);
        let snap = |a: &EditorApp| {
            (
                a.ed.selection.clone(),
                a.ed.wall_mode(),
                a.ed.wall_draft().points.clone(),
                a.plan.center,
                a.plan.scale,
                a.ed.can_undo(),
                a.ed.curtains.len(),
            )
        };
        let before = snap(&a);
        let ca = egui::Modifiers::COMMAND | egui::Modifiers::ALT;
        // press + release, or egui reports the next press as a repeat
        let key = |k| {
            let ev = |pressed| egui::Event::Key {
                key: k,
                physical_key: Some(k),
                pressed,
                repeat: false,
                modifiers: ca,
            };
            vec![ev(true), ev(false)]
        };
        let mut seen = vec![a.layout.clone()];
        for _ in 0..5 {
            run(&mut a, key(egui::Key::L));
            run(&mut a, vec![]);
            seen.push(a.layout.clone());
            assert_eq!(snap(&a), before, "{}", a.layout);
        }
        assert_eq!(
            seen,
            ["classic", "context", "split", "focus", "data", "classic"]
        );
        run(&mut a, key(egui::Key::Num3));
        assert_eq!(a.layout, "split");
        assert!(a.run("ui.layout.set.1"));
        assert_eq!(a.layout, "classic");
        a.ed.set_mode(Mode::Select);
        a.ed.undo();
        assert_eq!(a.ed.curtains.len(), 1);
    }
}
