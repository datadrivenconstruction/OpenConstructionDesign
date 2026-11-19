// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saved 3D views: viewport state ↔ the model's `View3dSpec` (camera, section box,
//! fill, style, hidden elements, category rules, level view). Coordinates in the spec are
//! model coordinates (z from the project zero); in the scene z is measured from the current level elevation.

use super::{Action, CatRule, Nav};
use crate::editor::{OpFn, Sel};
use crate::shell::EditorApp;
use ddc_model::{CategoryRule, View3dCamera, View3dSpec};
use ddc_render::glam::Vec3;
use ddc_render::{Aabb, OrbitCamera, VisualStyle};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Pick element → (kind, id) for the view spec.
pub fn sel_key(s: Sel) -> Option<(String, i64)> {
    Some(match s {
        Sel::Wall(id) => ("wall".into(), id),
        Sel::Opening(id) => ("opening".into(), id),
        Sel::Curtain(id) => ("curtain".into(), id),
        Sel::Panel(id, k) => (format!("panel.{k}"), id),
        Sel::Slab(id) => ("slab".into(), id),
        Sel::Room(id) => ("room".into(), id),
        Sel::Mep(id) => ("mep".into(), id),
        Sel::Comp(id) => ("comp".into(), id),
        #[allow(unreachable_patterns)]
        _ => return None,
    })
}

pub fn sel_from_key(kind: &str, id: i64) -> Option<Sel> {
    Some(match kind {
        "wall" => Sel::Wall(id),
        "opening" => Sel::Opening(id),
        "curtain" => Sel::Curtain(id),
        "slab" => Sel::Slab(id),
        "room" => Sel::Room(id),
        "mep" => Sel::Mep(id),
        "comp" => Sel::Comp(id),
        k => Sel::Panel(id, k.strip_prefix("panel.")?.parse().ok()?),
    })
}

/// View spec from camera and navigation state; `elevation` is the scene's level elevation.
pub fn spec_of(
    name: &str,
    cam: &OrbitCamera,
    section: Option<Aabb>,
    nav: &Nav,
    hidden: impl IntoIterator<Item = Sel>,
    elevation: f64,
) -> View3dSpec {
    let lift = |v: Vec3| [v.x as f64, v.y as f64, v.z as f64 + elevation];
    let mut hidden: Vec<(String, i64)> = hidden.into_iter().filter_map(sel_key).collect();
    hidden.sort();
    hidden.dedup();
    View3dSpec {
        name: name.to_string(),
        camera: View3dCamera {
            target: lift(cam.target),
            yaw: cam.yaw as f64,
            pitch: cam.pitch as f64,
            distance: cam.distance as f64,
            fov_y: cam.fov_y as f64,
            ortho: cam.ortho,
        },
        section_box: section.map(|b| (lift(b.min), lift(b.max))),
        caps: nav.caps,
        style: nav.style.key().to_string(),
        hidden,
        categories: nav
            .rules
            .iter()
            .filter(|(_, r)| **r != CatRule::default())
            .map(|(c, r)| CategoryRule {
                category: c.clone(),
                visible: r.visible,
                transparency: r.transparency,
            })
            .collect(),
        level: nav.level_view,
    }
}

/// Apply a spec to the camera and navigation (z relative to `elevation`); returns the view's hidden
/// elements (held by `Nav::view_hidden`; the plan's set is not changed).
pub fn apply_spec(
    s: &View3dSpec,
    cam: &mut OrbitCamera,
    section: &mut Option<Aabb>,
    nav: &mut Nav,
    elevation: f64,
) -> Vec<Sel> {
    let drop = |v: [f64; 3]| Vec3::new(v[0] as f32, v[1] as f32, (v[2] - elevation) as f32);
    let c = &s.camera;
    nav.anim = None;
    nav.walk = None;
    cam.target = drop(c.target);
    cam.yaw = c.yaw as f32;
    cam.pitch = (c.pitch as f32).clamp(-ddc_render::nav::PITCH_TOP, ddc_render::nav::PITCH_TOP);
    cam.distance = (c.distance as f32).max(1e-3);
    cam.fov_y = c.fov_y as f32;
    cam.ortho = c.ortho;
    *section = s.section_box.map(|(a, b)| Aabb {
        min: drop(a),
        max: drop(b),
    });
    if let Some(b) = section {
        cam.scene_radius = cam.scene_radius.max((b.max - b.min).length() * 0.5);
    }
    nav.caps = s.caps;
    nav.style = VisualStyle::parse(&s.style).unwrap_or_default();
    nav.rules = s
        .categories
        .iter()
        .map(|r| {
            (
                r.category.clone(),
                CatRule {
                    visible: r.visible,
                    transparency: r.transparency.min(100),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    nav.temp = None;
    nav.level_view = s.level;
    s.hidden
        .iter()
        .filter_map(|(k, id)| sel_from_key(k, *id))
        .collect()
}

/// A model edit as a single editor undo step.
fn op(
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> OpFn {
    OpFn(Arc::new(f))
}

fn msg(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(a.sh.cat.t(key));
}

/// What is hidden in 3D now: the open view's set and the plan's shared set (what you see is
/// what gets saved).
fn shown_hidden(a: &EditorApp) -> Vec<Sel> {
    let v = &a.view.nav.view_hidden;
    v.iter()
        .copied()
        .chain(
            a.plan
                .b
                .vis
                .hidden
                .iter()
                .copied()
                .filter(|s| !v.contains(s)),
        )
        .collect()
}

/// Saved-view action (from the overlay or a command).
pub fn apply(a: &mut EditorApp, act: Action) {
    let e = a.ed.elevation;
    match act {
        Action::Save(name) => {
            let s = spec_of(
                &name,
                &a.view.camera,
                a.view.section_box,
                &a.view.nav,
                shown_hidden(a),
                e,
            );
            match a.ed.run_op(op(move |m| m.add_view3d(&s).map(|_| ()))) {
                Ok(()) => {
                    a.view.nav.current =
                        a.ed.model
                            .view3ds()
                            .ok()
                            .and_then(|v| v.last().map(|x| x.0));
                    msg(a, "v3d.msg.saved");
                }
                Err(err) => a.ed.status = Some(err.to_string()),
            }
        }
        Action::Overwrite(id) => {
            let name = a
                .view
                .nav
                .saved
                .iter()
                .find(|v| v.0 == id)
                .map(|v| v.1.clone())
                .unwrap_or_default();
            let s = spec_of(
                &name,
                &a.view.camera,
                a.view.section_box,
                &a.view.nav,
                shown_hidden(a),
                e,
            );
            match a.ed.run_op(op(move |m| m.update_view3d(id, &s))) {
                Ok(()) => {
                    a.view.nav.current = Some(id);
                    msg(a, "v3d.msg.saved");
                }
                Err(err) => a.ed.status = Some(err.to_string()),
            }
        }
        Action::Delete(id) => match a.ed.run_op(op(move |m| m.delete_view3d(id))) {
            Ok(()) => {
                let name = a
                    .view
                    .nav
                    .saved
                    .iter()
                    .find(|(v, _)| *v == id)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_default();
                if a.view.nav.current == Some(id) {
                    a.view.nav.current = None;
                    a.view.nav.set_view_hidden(Default::default());
                }
                a.ed.status = Some(a.sh.cat.tf("v3d.msg.deleted_named", &[("name", &name)]));
                a.ed.offer_undo();
            }
            Err(err) => a.ed.status = Some(err.to_string()),
        },
        Action::Restore(id) => {
            let Ok(s) = a.ed.model.view3d(id) else {
                return;
            };
            if let Some((l, _)) = s.level {
                a.ed.set_level(l);
            }
            // the level change is already accounted for: the scene shift in `sync` is not repeated
            let e = a.ed.elevation;
            a.view.nav.elevation = Some(e);
            let hidden = apply_spec(
                &s,
                &mut a.view.camera,
                &mut a.view.section_box,
                &mut a.view.nav,
                e,
            );
            // the view's own set: what is hidden on the plan is neither lost nor replaced
            a.view.nav.set_view_hidden(hidden.into_iter().collect());
            a.view.nav.current = Some(id);
            a.view.keep_camera();
            a.view3d = true;
            msg(a, "v3d.msg.restored");
        }
    }
}
