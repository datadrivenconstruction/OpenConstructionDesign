// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! ddc-app: demo plan (demo), UI (ui_kit), 3D viewport (viewport), plan (plan_view),
//! the "Wall" tool: units (units), snaps (snap), state machine (wall_tool), editor (editor);
//! the "Door/Window" tool (opening_tool); in-place masses and bodies (mass_tools); "File -> Import" via bridges (import);
//! sections and elevations (section_view, editor::sec);
//! wall editing (wall_edit) and curtain wall editing (curtain_edit);
//! outline sketch mode (sketch_mode), slab shape editing (shape_edit);
//! graphics backend choice (gfx), offscreen frame to PNG (shot), the ddc-ui shell (shell);
//! background jobs (jobs), large files and lazy mesh loading (bigfile).

/// The window's GPU handles. A build without `gui` has no window: the type has no values, so
/// every `Option<&RenderState>` there is `None`.
#[cfg(feature = "gui")]
pub use eframe::egui_wgpu::RenderState;
#[cfg(not(feature = "gui"))]
pub enum RenderState {}

pub mod ai;
#[cfg(feature = "link")]
pub mod link;
#[cfg(test)]
mod app2_fx;
#[cfg(test)]
mod btn_fix_tests;
#[cfg(test)]
mod known_gaps;
#[cfg(test)]
mod label_coverage;
#[cfg(test)]
mod layout_contract;
#[cfg(test)]
mod layout_fold;
#[cfg(test)]
mod layout_looks_tests;
#[cfg(test)]
mod qa_app_layout;
#[cfg(test)]
mod qa16d_tests;
#[cfg(test)]
mod qa37b_tests;
#[cfg(test)]
mod ux29e_tests;
pub mod annot_text;
pub mod safe_file;
pub mod bigfile;
pub mod boundary_link_ui;
pub mod clip_tools;
pub mod collab;
pub mod constraints_ui;
pub mod create_names;
pub mod curtain_edit;
pub mod cw_props;
pub mod ctx_menu;
pub mod data_ui;
pub mod depth_ui;
pub mod deliver;
pub mod issues_ui;
pub mod demo;
pub mod drafting_view;
pub mod drawing;
pub mod edit3d;
pub mod edit_bind;
pub mod editor;
pub mod elem;
pub mod family_editor;
pub mod find_replace;
pub mod query_win;
pub mod script_rec;
pub mod scripts_win;
pub mod view_cmds;
pub mod view_panes;
pub mod wp_views;
pub mod fit_check;
pub mod fitting_insert;
pub mod classify_ui;
pub mod explain_ui;
pub mod gen_rules_ui;
pub mod pdf_underlay;
pub mod wall_profile_ui;
pub mod where_is;
pub mod geom_props;
pub mod complib;
pub mod fam_label;
pub mod gfx;
#[cfg(feature = "gui")]
pub mod gfx_bench;
pub mod gfx_guard;
pub mod gfx_ui;
pub mod gltf_export;
pub mod mesh_io;
pub mod grid_set;
pub mod grid_set_ui;
pub mod grips;
pub mod interact;
pub mod headless_file;
pub mod build_record;
#[cfg(feature = "gui")]
pub mod perf_bench;
pub mod grips3d;
pub mod grips_opening;
pub mod help;
pub mod ix_card;
pub mod refusals;
pub mod import;
pub mod imported_props;
pub mod isolate;
pub mod line_pick;
pub mod jobs;
pub mod links_ui;
pub mod mass_tools;
pub mod layer_split;
pub mod mat_anchor;
pub mod logo_ui;
pub mod mat_tex;
pub mod mep3d;
pub mod circuit_edit;
pub mod mep_browser;
pub mod mep_follow;
pub mod mesh_tools;
pub mod outline_cleanup;
pub mod modify_opts;
pub mod modify_tools;
pub mod multi_edit;
pub mod open;
pub mod opening_tool;
pub mod ops_exec;
pub mod os_assoc;
pub mod overlay;
pub mod params_multi;
pub mod params_ui;
pub mod phase_ui;
pub mod plan_cull;
pub mod pick_rules;
pub mod plan_cycle;
pub mod plan_hovertip;
pub mod plan_empty;
pub mod nav_cursor;
pub mod plan_hud;
pub mod plan_range;
pub mod plan_view;
pub mod room_label_fit;
pub mod prefs;
pub mod cmdline;
pub mod project;
pub mod recent_probe;
pub mod project_ui;
pub mod qa_accept;
pub mod query;
/// Quick work: a palette with arguments, repeat, menus, HUD, sets, history (g9).
pub mod quick;
pub mod range_layer;
pub mod range_libs;
pub mod range_styles;
pub mod range_ui;
pub mod unsaved;
pub mod reveal;
pub mod rebar_scene;
pub mod level_ask;
pub mod rebar_ui;
pub mod room_watch;
pub mod roof_edge_scene;
pub mod roof_edge_tools;
pub mod roof_edge_ui;
pub mod save;
pub mod scene_ids;
pub mod schedule;
pub mod steel_text;
pub mod file_pick;
pub mod section_hud;
pub mod section_view;
pub mod bg_latest;
pub mod sel_dims;
pub mod selection;
pub mod shape_edit;
pub mod sheet_view;
pub mod shell;
pub mod report_shot;
pub mod shot;
pub mod shot_press;
pub mod shot_scenes;
pub mod sketch_mode;
pub mod snap;
pub mod snap_core;
pub mod snap_pin;
pub mod snap_ref;
pub mod snap_svc;
pub mod status;
pub mod snap_align;
pub mod snap_refine;
pub mod draw_chip;
pub mod cursor_labels;
pub mod empty_kind;
#[path = "solid_scene_edit3d.rs"]
pub mod solid_edit3d;
pub mod freeform_edit3d;
pub mod panel_system_edit3d;
pub mod shell_elem_edit3d;
pub mod solid_scene;
pub mod solid_tools;
pub mod source_cf;
pub mod source_ui;
pub mod source_watch;
pub mod spatial;
pub mod struct_props;
pub mod clearance;
pub mod temp_dims;
#[path = "tools/mod_arch.rs"]
pub mod tools;
pub mod topo;
pub mod trial;
pub mod wall_cuts;
pub mod type_use;
pub mod ui_err;
pub mod io_kind;
pub mod brand;
pub mod update;
pub mod ui_kit;
pub mod xl_text;
pub mod expr_field;
#[cfg(test)]
mod qa16e_tests;
#[cfg(test)]
mod ux28a_tests;
#[cfg(test)]
mod ux28b_tests;
pub mod sel3d;
pub mod unit_text;
pub mod units;
pub mod value_src_ui;
#[cfg(test)]
mod units_round_trip_tests;
#[cfg(test)]
mod untouched_matrix_tests;
pub mod view3d;
pub mod view_gfx_ui;
pub mod viewport;
pub mod wall_edit;
pub mod wall_pick;
pub mod wall_tool;
pub mod win;
pub mod win_geom;

use std::sync::atomic::{AtomicBool, Ordering};

static UI_RU: AtomicBool = AtomicBool::new(true);

/// UI language for strings outside the shell (sketch and mass modes, command status).
pub fn set_ui_lang(l: ddc_ui::i18n::Lang) {
    let ru = l == ddc_ui::i18n::Lang::Ru;
    #[cfg(not(test))]
    ddc_ui::i18n::set_current(l);
    // Tests reach this through `EditorApp::ui`: it must never flip the flag other threads read.
    // Each frame sets this thread's language, unless a test pinned one (`with_test_lang`).
    #[cfg(test)]
    if !LANG_PIN.with(std::cell::Cell::get) {
        LANG_HERE.with(|c| c.set(Some(ru)));
    }
    #[cfg(not(test))]
    UI_RU.store(ru, Ordering::Relaxed);
    units::set_ui_decimal_mark(units::DecimalMark::for_lang(l));
}

pub fn ui_is_ru() -> bool {
    ui_lang_now()
}

/// Plural message of the catalog (`one`/`few`/`many`/`other` forms) for `n`, in the UI language.
pub fn tr_n(key: &str, n: u64) -> String {
    tr_n_lang(ui_lang_now(), key, n)
}

/// As `tr_n`, in an explicit language.
pub fn tr_n_lang(ru: bool, key: &str, n: u64) -> String {
    use ddc_ui::i18n::{Catalog, Lang};
    static RU: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    static EN: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    let c = if ru {
        RU.get_or_init(|| Catalog::new(Lang::Ru))
    } else {
        EN.get_or_init(|| Catalog::new(Lang::En))
    };
    c.plural(key, n)
}

fn catalog() -> &'static ddc_ui::i18n::Catalog {
    use ddc_ui::i18n::{Catalog, Lang};
    static CAT: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    CAT.get_or_init(|| Catalog::new(Lang::En))
}

pub(crate) fn ui_lang_now() -> bool {
    #[cfg(test)]
    if let Some(ru) = LANG_HERE.with(std::cell::Cell::get) {
        return ru;
    }
    UI_RU.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    static LANG_HERE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    // set by the test helpers below: frames (`set_ui_lang`) then leave LANG_HERE alone
    static LANG_PIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test only: the UI language of this thread alone, so parallel tests keep the global flag.
#[cfg(test)]
pub(crate) fn lang_on_this_thread(ru: bool) {
    LANG_HERE.with(|c| c.set(Some(ru)));
    LANG_PIN.with(|c| c.set(true));
}

/// Test only: `f` in the UI language `ru` on this thread; the previous one comes back after.
#[cfg(test)]
pub(crate) fn with_test_lang<R>(ru: bool, f: impl FnOnce() -> R) -> R {
    let prev = LANG_HERE.with(|c| c.replace(Some(ru)));
    let pin = LANG_PIN.with(|c| c.replace(true));
    let out = f();
    LANG_HERE.with(|c| c.set(prev));
    LANG_PIN.with(|c| c.set(pin));
    out
}

/// A catalog string by key in the current language; `{name}` in it comes from `args`.
pub fn tr(key: &str, args: &[(&str, &str)]) -> String {
    if ui_lang_now() {
        tr_lang(true, key, args)
    } else {
        // any of the 45 locales; English (or the base language) fills missing keys
        tr_in(ddc_ui::i18n::current(), key, args)
    }
}

/// As `tr`, in an explicit language.
pub fn tr_lang(ru: bool, key: &str, args: &[(&str, &str)]) -> String {
    use ddc_ui::i18n::Lang;
    let lang = if ru { Lang::Ru } else { Lang::En };
    let mut s = catalog().t_in(lang, key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// As `tr`, for a locale code; unknown codes and missing keys fall back to English.
pub fn tr_locale(code: &str, key: &str, args: &[(&str, &str)]) -> String {
    let lang = ddc_ui::i18n::Lang::from_code(code);
    let mut s = catalog().t_in(lang, key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Catalog text without substitutions, borrowed from the static catalog (RU, then EN, then the key).
pub fn tr_str(ru: bool, key: &'static str) -> &'static str {
    use ddc_ui::i18n::Lang;
    let c = catalog();
    let own = if ru { Lang::Ru } else { Lang::En };
    c.table(own)
        .get(key)
        .or_else(|| c.table(Lang::En).get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(key)
}

/// Same as `tr` for an explicit language; a key missing there falls back to English.
pub fn tr_in(lang: ddc_ui::i18n::Lang, key: &str, args: &[(&str, &str)]) -> String {
    let mut s = catalog().t_in(lang, key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

#[cfg(test)]
mod field_a11y_scan;

#[cfg(test)]
mod lang_guard_tests {
    use ddc_ui::i18n::Lang;

    /// No test may change the process-wide UI language: every default-thread test reads it.
    #[test]
    fn tests_never_flip_the_global_ui_language() {
        for l in [Lang::En, Lang::Ru, Lang::En] {
            super::set_ui_lang(l);
            assert!(super::UI_RU.load(super::Ordering::Relaxed), "set_ui_lang({l:?}) wrote it");
        }
        // an unpinned thread follows its last call (En here), and with_test_lang wins over it
        assert!(!super::ui_is_ru());
        assert!(super::with_test_lang(true, super::ui_is_ru));
    }

    /// Every frame language switch on one thread reaches `crate::tr`, not only the first one.
    #[test]
    fn frames_switch_the_thread_language_each_time() {
        const KEY: &str = "ribbon.split_tip";
        let (en, ru) = (super::tr_lang(false, KEY, &[]), super::tr_lang(true, KEY, &[]));
        assert_ne!(en, ru, "{KEY} must differ in en and ru");
        let mut seen = Vec::new();
        for l in [Lang::En, Lang::Ru, Lang::En] {
            super::set_ui_lang(l);
            seen.push(super::tr(KEY, &[]));
        }
        assert_eq!(seen, [en.clone(), ru.clone(), en.clone()]);
        // a pin wins over frames inside it; the frame language comes back after it
        super::with_test_lang(true, || {
            super::set_ui_lang(Lang::En);
            assert_eq!(super::tr(KEY, &[]), ru);
        });
        assert_eq!(super::tr(KEY, &[]), en);
        assert!(super::UI_RU.load(super::Ordering::Relaxed), "the global flag stays");
    }
}
