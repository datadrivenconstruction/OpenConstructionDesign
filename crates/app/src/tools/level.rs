//! Level: in a section or elevation a click places a level at the cursor elevation with an
//! auto-name; outside a section the command opens the level dialog. The active level is unchanged.

use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::ui_kit::Tokens;
use crate::units::format_length;
use egui::{self, Pos2, Rect, Stroke};

/// Elevation snap step, m.
const STEP: f64 = 0.05;
/// An elevation counts as taken if a level is closer than this, m.
const TAKEN: f64 = 0.01;

fn snap_z(z: f64) -> f64 {
    (z / STEP).round() * STEP
}

fn taken(ed: &Editor, z: f64) -> bool {
    ed.levels().iter().any(|l| (l.2 - z).abs() < TAKEN)
}

/// New level at an elevation (snapped); None — elevation taken or the model refused.
pub fn place(ed: &mut Editor, z: f64) -> Option<i64> {
    let z = snap_z(z);
    if taken(ed, z) {
        ed.status = Some(crate::tr("tool.err.level_taken", &[]));
        return None;
    }
    let name = ed.next_level_name(z);
    let cur = ed.level;
    let id = ed.add_level(&name, z)?;
    ed.set_level(cur);
    Some(id)
}

/// Line of the prospective level: name, elevation, distance to the level below.
pub fn overlay(p: &egui::Painter, rect: Rect, t: &Tokens, ed: &Editor, hover: Option<(Pos2, f64)>) {
    let Some((h, z)) = hover else { return };
    let z = snap_z(z);
    let col = if taken(ed, z) { t.text_dim } else { t.accent };
    p.line_segment(
        [Pos2::new(rect.left(), h.y), Pos2::new(rect.right(), h.y)],
        Stroke::new(1.5, col),
    );
    let u = ed.tool.units;
    let name = ed.next_level_name(z);
    let mut label = format!("{name}  {}", format_length(z, u));
    let below = ed
        .levels()
        .iter()
        .map(|l| l.2)
        .filter(|e| *e < z - TAKEN)
        .fold(f64::NEG_INFINITY, f64::max);
    if below.is_finite() {
        label += &format!("  Δ{}", format_length(z - below, u));
    }
    p.text(
        Pos2::new(rect.left() + 16.0, h.y - 4.0),
        egui::Align2::LEFT_BOTTOM,
        label,
        ddc_ui::typo::font(ddc_ui::theme::Role::Body),
        col,
    );
}

fn h_level(app: &mut EditorApp) {
    if app.section.is_some() {
        app.sview.level_tool = true;
        app.sview.level_tool_at = app.section;
        app.ed.status = Some(crate::tr("hint.level.place", &[]));
    } else {
        app.start_level_edit(None);
    }
}

pub fn register(app: &mut EditorApp) {
    app.register_command("arch.level", h_level);
}
