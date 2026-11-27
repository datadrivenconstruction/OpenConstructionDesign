// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan tools armed from the ribbon work in the 3D view: the frame routes pointer and keys.
use super::super::plane::WorkPlane;
use super::super::{adapter, show, Tool};
use crate::editor::Editor;
use crate::interact::ViewAdapter;
use crate::viewport::Viewport;
use ddc_geom::{v2, v3};
use ddc_render::glam::Vec3;
use ddc_render::Scene;

struct Rig {
    ctx: egui::Context,
    view: Viewport,
    scene: Scene,
    ed: Editor,
}

impl Rig {
    fn new() -> Rig {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(crate::wall_tool::Mode::Select);
        let mut view = Viewport::new();
        view.camera.target = Vec3::new(3.0, 2.0, 1.0);
        view.camera.distance = 25.0;
        // the 3D tools draw their hints in the app's named fonts
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        Rig {
            ctx,
            view,
            scene: Scene::default(),
            ed,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            events,
            ..Default::default()
        };
        let (view, scene, ed) = (&mut self.view, &self.scene, &mut self.ed);
        let _ = self.ctx.run(input, |c| {
            egui::CentralPanel::default().show(c, |ui| show(ui, view, scene, ed));
        });
    }

    /// Screen position of a model point through the 3D view of the last frame.
    fn screen(&self, x: f64, y: f64, z: f64) -> egui::Pos2 {
        let rect = self.view.e3.rect.expect("a frame was drawn");
        let wp = WorkPlane::level(self.ed.level, 0.0);
        let ad = adapter(&self.view.camera, rect, self.ed.elevation, &wp);
        let s = ad
            .project_at(v3(x, y, z + self.ed.elevation))
            .expect("in front of the camera");
        egui::pos2(s[0] as f32, s[1] as f32)
    }

    /// Move there, press and release: one click of the 3D view.
    fn click(&mut self, at: egui::Pos2) {
        let btn = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![egui::Event::PointerMoved(at)]);
        self.frame(vec![btn(true)]);
        self.frame(vec![btn(false)]);
        self.frame(vec![]);
    }

    fn count(&self, table: &str) -> i64 {
        self.ed
            .model
            .sql()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

/// A stair armed from the ribbon is drawn by two clicks on the level plane in 3D: one record,
/// one undo step, nothing selected by those clicks.
#[test]
fn ribbon_stair_draws_in_3d_in_one_undo_step() {
    let mut r = Rig::new();
    r.ed.model.add_level("L2", 3.0).unwrap();
    r.frame(vec![]);
    crate::tools::arm(&mut r.ed, crate::tools::Kind::Stair, ddc_ui::i18n::Lang::En);
    crate::interact::Interaction::refresh(&mut r.ed);
    assert!(
        super::live(&r.ed) && super::takes_pointer(&r.ed),
        "a model tool is live"
    );
    let pushes = r.ed.pushes;
    let (a, b) = (r.screen(1.0, 0.0, 0.0), r.screen(1.0, 5.0, 0.0));
    r.click(a);
    r.click(b);
    assert_eq!(r.count("stair"), 1, "the stair is the plan's record");
    assert_eq!(r.ed.pushes - pushes, 1, "one action, one undo step");
    assert!(
        r.ed.selection.is_empty(),
        "the clicks were points, not picks"
    );
    assert_eq!(r.view.e3.tool, Tool::Select);
    r.ed.undo();
    assert_eq!(r.count("stair"), 0);
}

/// A door armed from the ribbon is placed on the wall face clicked in 3D.
#[test]
fn ribbon_door_places_on_the_wall_clicked_in_3d() {
    let mut r = Rig::new();
    r.ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    r.frame(vec![]);
    assert!(crate::interact::script::arm(&mut r.ed, "DR"));
    assert!(super::live(&r.ed));
    let pushes = r.ed.pushes;
    let at = r.screen(3.0, 0.0, 1.0);
    r.click(at);
    assert_eq!(r.count("opening"), 1, "door in the wall");
    assert_eq!(r.ed.pushes - pushes, 1);
}

/// The 3D view's own tools are not plan tools: their bridge session is never `live`, and an
/// idle editor routes nothing.
#[test]
fn own_3d_tools_and_idle_are_not_routed() {
    let mut r = Rig::new();
    r.frame(vec![]);
    assert!(!super::live(&r.ed));
    r.view.e3.set_tool(Tool::Measure);
    r.frame(vec![]);
    assert!(crate::interact::sessions::view3d::running(&r.ed));
    assert!(!super::live(&r.ed), "the 3D bridge is not a plan tool");
    // a raised user plane does not lift a routed plan tool off the level
    r.view.e3.set_tool(Tool::Select);
    r.frame(vec![]);
    crate::tools::arm(
        &mut r.ed,
        crate::tools::Kind::Column,
        ddc_ui::i18n::Lang::En,
    );
    crate::interact::Interaction::refresh(&mut r.ed);
    assert!(super::live(&r.ed));
    let raised = WorkPlane::level(r.ed.level, 2.0);
    assert_eq!(super::plane_for(&r.ed, raised).z(), 0.0);
}

/// The scene of the editor's model, so 3D picks meet its bodies.
fn with_scene(r: &mut Rig) {
    let (scene, t) = super::super::scene3d::compose(&mut r.ed);
    r.scene = scene;
    r.view.e3.set_ext(t);
    r.frame(vec![]);
}

/// A wall-hosted family armed from the ribbon goes onto the wall face clicked in 3D: on that
/// side (back to the wall), at the height clicked; the plan click keeps the base.
#[test]
fn ribbon_component_sits_on_the_wall_face_clicked_in_3d() {
    let mut r = Rig::new();
    r.ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    with_scene(&mut r);
    let pick = crate::tools::component::Pick { family: "Socket Outlet".into(), ty: "Single".into(), def: None };
    crate::tools::component::arm_with(&mut r.ed, &pick, ddc_ui::i18n::Lang::En);
    crate::interact::Interaction::refresh(&mut r.ed);
    assert!(super::live(&r.ed));
    let half = r.ed.current_type().unwrap().thickness() / 2.0;
    let rect = r.view.e3.rect.unwrap();
    let eye = adapter(&r.view.camera, rect, 0.0, &WorkPlane::level(r.ed.level, 0.0)).eye;
    let side = if eye.y < 0.0 { -1.0 } else { 1.0 };
    let at = r.screen(2.3, side * half, 1.2);
    r.click(at);
    assert_eq!(r.count("comp"), 1, "the socket is placed");
    let (wall, x, y, z, rot): (Option<i64>, f64, f64, f64, f64) = r
        .ed
        .model
        .sql()
        .query_row("SELECT host_wall, x, y, z, rotation FROM comp", [], |q| {
            Ok((q.get(0)?, q.get(1)?, q.get(2)?, q.get(3)?, q.get(4)?))
        })
        .unwrap();
    assert_eq!(wall, Some(r.ed.walls[0].id));
    assert!((x - 2.3).abs() < 0.05, "along {x}");
    assert!((y - side * half).abs() < 1e-9, "on the face clicked: {y}");
    assert!((z - 1.2).abs() < 0.05, "at the height clicked: {z}");
    let want = if side < 0.0 { std::f64::consts::PI } else { 0.0 };
    assert!((rot - want).abs() < 1e-9, "back to the wall: {rot}");
}

/// A free family clicked onto a face looking up stands on it: the top of a raised floor.
#[test]
fn ribbon_component_stands_on_the_face_clicked_in_3d() {
    let mut r = Rig::new();
    let layer = [ddc_model::LayerSpec { material: "c".into(), thickness: 0.2 }];
    let st = r.ed.model.add_slab_type("S raised", &layer).unwrap();
    let s = ddc_model::SlabSpec {
        level: r.ed.level,
        slab_type: st,
        offset: 0.9,
        outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
        holes: vec![],
    };
    r.ed.model.add_slab(&s).unwrap();
    r.ed.reload().unwrap();
    with_scene(&mut r);
    let pick = crate::tools::component::Pick { family: "Generic Box".into(), ty: crate::complib::type_labels(&r.ed.model).iter().find_map(|l| l.strip_prefix("Generic Box : ").map(str::to_string)).unwrap(), def: None };
    crate::tools::component::arm_with(&mut r.ed, &pick, ddc_ui::i18n::Lang::En);
    crate::interact::Interaction::refresh(&mut r.ed);
    let top = 0.9;
    let at = r.screen(2.0, 2.0, top);
    r.click(at);
    let (x, y, slab): (f64, f64, Option<i64>) = r
        .ed
        .model
        .sql()
        .query_row("SELECT x, y, host_slab FROM comp", [], |q| Ok((q.get(0)?, q.get(1)?, q.get(2)?)))
        .unwrap();
    assert!((x - 2.0).abs() < 0.05 && (y - 2.0).abs() < 0.05, "the point on the face, not the level plane behind it: {x}, {y}");
    assert!(slab.is_some(), "hosted by the floor clicked");
}
