//! 3D: terrain bodies (by sub-region material) and sites in the scene; point handles in edit
//! mode — vertical dragging changes the elevation, committed as one operation on release.

use super::Tool;
use crate::editor::Editor;
use crate::viewport::Viewport;
use ddc_render::glam::Vec3;
use ddc_render::{SceneMesh, TAG_SLAB};
use egui::{self, Pos2, Rect, Sense, Stroke, Vec2};

/// First scene element of terrain: the k-th terrain is `TOPO_ELEMENT + k` (below the reserved u32::MAX-n ids).
pub const TOPO_ELEMENT: u32 = crate::scene_ids::TOPO_BASE;
const MAX_TOPO: usize = crate::scene_ids::TOPO_MAX as usize;
/// At most this many point handles in 3D (the rest are not shown).
const MAX_HANDLES: usize = 2000;

/// Shader palette tag by material (20 — ground, 21… — sub-regions); a site is treated like a slab.
pub fn tag_of(material: &str, pad: bool) -> u32 {
    if pad {
        return TAG_SLAB;
    }
    match material {
        "grass" => 21,
        "asphalt" => 22,
        "gravel" => 23,
        "paving" => 24,
        "water" => 25,
        _ => 20,
    }
}

/// Terrain bodies in the scene; heights are relative to the current level elevation, like the rest of the scene.
pub fn append_scene(ed: &Editor, out: &mut Vec<std::sync::Arc<SceneMesh>>) {
    let Ok(list) = ed.model.toposolids() else {
        return;
    };
    let dz = ed.elevation;
    for (k, (id, _)) in list.iter().take(MAX_TOPO).enumerate() {
        let Ok(pieces) = ed.model.topo_pieces_shared(*id) else {
            continue;
        };
        for pc in pieces.iter() {
            if pc.mesh.triangles.is_empty() {
                continue;
            }
            out.push(std::sync::Arc::new(SceneMesh {
                element: TOPO_ELEMENT + k as u32,
                tag: tag_of(&pc.material, pc.pad.is_some()),
                positions: pc
                    .mesh
                    .positions
                    .iter()
                    .map(|p| [p.x as f32, p.y as f32, (p.z - dz) as f32])
                    .collect(),
                triangles: pc.mesh.triangles.clone(),
            }));
        }
    }
}

/// Point handles of the active terrain over the 3D view (after `Viewport::show` in the same area).
pub fn overlay3d(ed: &mut Editor, view: &Viewport, ui: &mut egui::Ui) {
    if ed.topo.tool != Some(Tool::Edit) {
        return;
    }
    let Some(id) = super::active(ed) else {
        return;
    };
    let rect = ui.min_rect();
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let aspect = rect.width() / rect.height();
    let cam = &view.camera;
    let to_screen = |p: Vec3| -> Option<Pos2> {
        let n = cam.project(p, aspect);
        let visible = (0.0..=1.0).contains(&n.z) && n.x.abs() <= 1.05 && n.y.abs() <= 1.05;
        visible.then(|| {
            Pos2::new(
                rect.min.x + (n.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - n.y) * 0.5 * rect.height(),
            )
        })
    };
    let dz = ed.elevation;
    let pts = super::points_of(ed, id);
    let painter = ui.painter_at(rect);
    let accent = crate::overlay::pal(ui.visuals().dark_mode).selection;
    let ink = ui.visuals().text_color();
    let mut commit = None;
    let mut shown = 0;
    let mut drag_seen = false;
    for (idx, (x, y, z)) in pts {
        let cur = match ed.topo.drag3d {
            Some((t, i, _, zz)) if t == id && i == idx => zz,
            _ => z,
        };
        let w = Vec3::new(x as f32, y as f32, (cur - dz) as f32);
        let (Some(s), Some(e)) = (to_screen(w), to_screen(w + Vec3::Z)) else {
            continue;
        };
        drag_seen |= matches!(ed.topo.drag3d, Some((t, i, ..)) if t == id && i == idx);
        shown += 1;
        if shown > MAX_HANDLES {
            break;
        }
        let resp = ui
            .interact(
                Rect::from_center_size(s, Vec2::splat(12.0)),
                ui.id().with(("topo3d", id, idx)),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        let hot = resp.hovered() || resp.dragged();
        if resp.drag_started() {
            ed.topo.drag3d = Some((id, idx, (x, y, z), z));
        }
        if resp.dragged() {
            let (d, a) = (resp.drag_delta(), e - s);
            let l2 = a.length_sq();
            if l2 > 1e-3 {
                if let Some(dd) = &mut ed.topo.drag3d {
                    dd.3 += (d.dot(a) / l2) as f64;
                }
            }
        }
        if resp.drag_stopped() {
            if let Some((t, i, from, zz)) = ed.topo.drag3d.take() {
                if t == id && i == idx && (zz - from.2).abs() > 1e-9 {
                    commit = Some((t, i, (from.0, from.1, zz)));
                }
            }
        }
        painter.circle_filled(
            s,
            if hot { 5.0 } else { 3.5 },
            if hot { accent } else { ink },
        );
        if hot {
            if let Some(base) = to_screen(Vec3::new(x as f32, y as f32, (z - dz) as f32)) {
                painter.line_segment([base, s], Stroke::new(1.0, accent));
            }
            painter.text(
                s + egui::vec2(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                crate::units::fmt_num(cur, 2),
                egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                ink,
            );
        }
    }
    // handle left the view edge or got clipped: no release event will arrive — reset
    if !drag_seen && commit.is_none() && !ui.input(|i| i.pointer.any_down()) {
        ed.topo.drag3d = None;
    }
    if let Some((t, i, p)) = commit {
        match ed.run_op(super::op(move |m| m.move_topo_point(t, i, p))) {
            Ok(()) => super::say(ed, "topo.done.z"),
            Err(e) => super::say_err(ed, &e),
        }
    }
}
