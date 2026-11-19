//! Site and topography windows: unit display, import gating, states, kit-only widgets.
//! Languages are passed explicitly; the global UI language is never switched.

use super::window::{
    all_state, import_block, surface_state, AllState, LenUnit, SurfaceState,
};
use crate::units::Units;
use ddc_io::topo::{DxfLayer, SourceKind};
use ddc_ui::i18n::Lang;

fn layer(name: &str, on: bool) -> (DxfLayer, bool) {
    (
        DxfLayer {
            name: name.into(),
            entities: 3,
            elevated: 2,
        },
        on,
    )
}

#[test]
fn project_units_pick_the_display_unit() {
    assert_eq!(LenUnit::of(Units::Metric), LenUnit::Mm);
    assert_eq!(LenUnit::of(Units::Imperial), LenUnit::Ft);
}

#[test]
fn unit_names_follow_the_language() {
    assert_eq!(LenUnit::Mm.unit_text(Lang::En), "mm");
    assert_eq!(LenUnit::Mm.unit_text(Lang::Ru), "\u{43c}\u{43c}");
    assert_eq!(LenUnit::M.unit_text(Lang::En), "m");
    assert_eq!(LenUnit::M.unit_text(Lang::Ru), "\u{43c}");
    assert_eq!(LenUnit::Ft.unit_text(Lang::En), "ft");
    assert_eq!(LenUnit::Ft.unit_text(Lang::Ru), "\u{444}\u{443}\u{442}");
}

#[test]
fn lengths_format_per_unit_and_locale() {
    assert_eq!(LenUnit::M.format(12.5, Lang::En), "12.500");
    assert_eq!(LenUnit::M.format(12.5, Lang::Ru), "12,500");
    assert_eq!(LenUnit::Mm.format(1.5, Lang::En), "1,500.0");
    assert_eq!(LenUnit::Mm.format(1.5, Lang::Ru), "1\u{a0}500,0");
    assert_eq!(LenUnit::Ft.format(1.0, Lang::En), "3.281");
    assert_eq!(LenUnit::Ft.format(0.3048, Lang::Ru), "1,000");
}

#[test]
fn lengths_round_trip_through_the_field_value() {
    for lu in [LenUnit::Mm, LenUnit::M, LenUnit::Ft] {
        for m in [0.0, 0.01, 0.05, 12.55, -3.2, 1234.5678] {
            let back = lu.from_disp(lu.to_disp(m));
            assert!((back - m).abs() < 1e-9, "{lu:?} {m} -> {back}");
        }
    }
    assert_eq!(LenUnit::Mm.to_disp(0.05), 50.0);
    assert!((LenUnit::Ft.to_disp(0.3048) - 1.0).abs() < 1e-12);
}

#[test]
fn import_is_blocked_until_a_layer_is_ticked() {
    assert_eq!(
        import_block(SourceKind::Dxf, 0),
        Some("topo.import.need_layer")
    );
    assert_eq!(import_block(SourceKind::Dxf, 1), None);
    assert_eq!(import_block(SourceKind::Dxf, 7), None);
    assert_eq!(import_block(SourceKind::Xyz, 0), None);
}

#[test]
fn all_layers_box_state() {
    assert_eq!(all_state(&[]), AllState::None);
    let mut v = vec![layer("a", false), layer("b", false)];
    assert_eq!(all_state(&v), AllState::None);
    v[0].1 = true;
    assert_eq!(all_state(&v), AllState::Mixed);
    v[1].1 = true;
    assert_eq!(all_state(&v), AllState::All);
}

#[test]
fn surface_section_states() {
    assert_eq!(surface_state(0, false, false), SurfaceState::Empty);
    assert_eq!(surface_state(2, false, false), SurfaceState::Loading);
    assert_eq!(surface_state(2, true, true), SurfaceState::Error);
    assert_eq!(surface_state(2, true, false), SurfaceState::Ready);
}

#[test]
fn new_strings_exist_in_both_languages() {
    let keys = [
        "topo.unit.mm",
        "topo.unit.metre",
        "topo.unit.ft",
        "topo.opt.material",
        "topo.row.active",
        "topo.btn.delete_pad",
        "topo.info.loading",
        "topo.info.warn_n",
        "topo.info.points_label",
        "topo.import.all_layers",
        "topo.import.need_layer",
    ];
    for lang in [Lang::En, Lang::Ru] {
        for k in keys {
            assert_ne!(crate::tr_in(lang, k, &[]), k, "{k} missing in {lang:?}");
        }
    }
}

#[test]
fn window_uses_only_kit_widgets() {
    let src = include_str!("window.rs");
    let body = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
    for raw in [
        "egui::DragValue",
        "egui::ComboBox",
        "ui.checkbox(",
        "colored_label(",
        "text_edit_singleline(",
        "ui.button(",
        "small_button(",
    ] {
        assert!(!body.contains(raw), "raw widget {raw} in topo/window.rs");
    }
}

/// The toposolid tool panel stays clear of the drawing area: at the default layout it sits
/// over the properties dock (at most a sliver of the canvas edge), never at the top-left.
#[test]
fn topo_tool_panel_opens_beside_the_canvas() {
    for (w, h) in [(1400.0, 900.0), (1600.0, 1000.0), (1920.0, 1080.0)] {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        let p = super::window::tool_pos(screen);
        let panel = egui::Rect::from_min_size(p, egui::vec2(ddc_ui::theme::layout::DIALOG_HUD, 240.0));
        assert!(screen.contains_rect(panel), "{panel:?}");
        let canvas_right = w - crate::ui_kit::PROPS_W;
        assert!(panel.left() >= canvas_right - 30.0, "{panel:?} over the canvas at {w}");
    }
}
