use super::*;
use crate::ai::point::ViewKind;
use ddc_model::WallSpec;

fn view() -> ViewRef {
    ViewRef {
        kind: ViewKind::Plan,
        name: "plan".into(),
        level: None,
    }
}

fn walls(ed: &mut Editor, n: usize) {
    let ws: Vec<WallSpec> = (0..n)
        .map(|k| WallSpec {
            a: (k as f64 * 5.0, 0.0),
            b: (k as f64 * 5.0 + 4.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        })
        .collect();
    ed.model.add_walls(&ws).unwrap();
    ed.reload().unwrap();
}

fn count(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
}

/// Apply as the panel does: journal head before, the row after.
fn apply(ed: &mut Editor, marks: &mut Marks, msg: u64, n: usize) {
    let before = ed.model.op_top().unwrap();
    walls(ed, n);
    let row = ed.model.ai_option_head().unwrap();
    marks.applied(ed, msg, before, row);
}

#[test]
fn mark_lifecycle_pending_answered_applied() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut marks = Marks::default();
    let id = marks.add([1.0, 2.0, 3.0], view(), 7);
    assert_eq!(marks.get(id).unwrap().state, MarkState::Pending);
    marks.answered(7);
    assert_eq!(marks.get(id).unwrap().state, MarkState::Answered);
    assert_eq!(marks.latest_open(), Some(7));
    apply(&mut ed, &mut marks, 7, 2);
    marks.sync(&ed);
    let m = marks.get(id).unwrap();
    assert_eq!(m.state, MarkState::Applied);
    assert!(m.before_seq.is_some() && m.applied_token.is_some());
    assert_eq!(marks.latest_open(), None);
    // a later edit on top keeps it applied
    walls(&mut ed, 1);
    marks.sync(&ed);
    assert_eq!(marks.get(id).unwrap().state, MarkState::Applied);
    marks.failed(7, RefusalCode::Internal, "provider down");
    assert_eq!(marks.get(id).unwrap().state, MarkState::Failed(RefusalCode::Internal, "provider down".into()));
}

#[test]
fn mark_undo_cancelled() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut marks = Marks::default();
    let id = marks.add([0.0; 3], view(), 1);
    marks.answered(1);
    apply(&mut ed, &mut marks, 1, 2);
    ed.model.undo().unwrap();
    ed.reload().unwrap();
    marks.sync(&ed);
    assert_eq!(marks.get(id).unwrap().state, MarkState::Cancelled);
}

#[test]
fn before_copy_is_pre_apply_and_live_untouched() {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, 3);
    let mut marks = Marks::default();
    let id = marks.add([0.0; 3], view(), 1);
    marks.answered(1);
    apply(&mut ed, &mut marks, 1, 4);
    assert_eq!(count(&ed.model), 7);
    assert!(marks.before_available(&ed, id));
    let before = marks.before_copy(&ed, id).unwrap();
    assert_eq!(count(&before.model), 3);
    assert_eq!(count(&ed.model), 7);
    assert!(marks.before_copy(&ed, 99).is_err());
}

#[test]
fn before_unavailable_without_apply() {
    let ed = Editor::new_in_memory().unwrap();
    let mut marks = Marks::default();
    let id = marks.add([0.0; 3], view(), 1);
    marks.answered(1);
    assert!(!marks.before_available(&ed, id));
}

#[test]
fn marks_session_only_and_capped() {
    let ed = Editor::new_in_memory().unwrap();
    let changes = ed.model.sql().total_changes();
    let mut marks = Marks::default();
    for k in 0..120 {
        marks.add([0.0; 3], view(), k);
    }
    assert_eq!(marks.all().len(), MAX_MARKS);
    assert_eq!(marks.all()[0].msg_id, 20);
    assert_eq!(ed.model.sql().total_changes(), changes);
    // a reopened session starts empty
    assert!(Marks::default().all().is_empty());
}

fn plan_at(center: (f64, f64), scale: f64) -> crate::plan_view::PlanView {
    let mut p = crate::plan_view::PlanView::default();
    p.center = ddc_geom::math::v2(center.0, center.1);
    p.scale = scale;
    p
}

#[test]
fn marks_project_to_screen_point() {
    use eframe::egui::{pos2, Rect};
    let rect = Rect::from_min_max(pos2(100.0, 50.0), pos2(500.0, 350.0));
    let plan = plan_at((3.0, 2.0), 80.0);
    let mut marks = Marks::default();
    let on = marks.add([4.0, 3.0, 0.0], view(), 1);
    let off = marks.add([400.0, 3.0, 0.0], view(), 2);
    let project = |a: [f64; 3]| Some(plan.screen_at(rect, ddc_geom::math::v2(a[0], a[1])));
    let got = layout(&marks, false, 0, rect, &project);
    // centre (300,200); one metre right = +80 px, one metre up = -80 px
    assert_eq!(got, vec![(on, pos2(380.0, 120.0))]);
    assert!(got.iter().all(|(id, _)| *id != off));
    // zoom about the same centre moves the bubble with the model
    let plan2 = plan_at((3.0, 2.0), 40.0);
    let p2 = |a: [f64; 3]| Some(plan2.screen_at(rect, ddc_geom::math::v2(a[0], a[1])));
    assert_eq!(layout(&marks, false, 0, rect, &p2), vec![(on, pos2(340.0, 160.0))]);
    // behind the camera: no bubble
    assert!(layout(&marks, true, 0, rect, &|_| None).is_empty());
}

#[test]
fn bubble_colour_follows_state() {
    let all = [
        MarkState::Pending,
        MarkState::Answered,
        MarkState::Applied,
        MarkState::Cancelled,
        MarkState::Failed(RefusalCode::Internal, "x".into()),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(colour(a), colour(b));
        }
    }
}

#[test]
fn bubble_click_asks_the_panel_for_the_answer() {
    use eframe::egui;
    assert!(!take_focus());
    with(|m| {
        m.add([0.0; 3], view(), 1);
    });
    let ctx = egui::Context::default();
    let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
    let at = egui::pos2(100.0, 100.0);
    let mut clicked = None;
    for ev in [None, Some(true), Some(false)] {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(rect);
        input.events.push(egui::Event::PointerMoved(at));
        if let Some(down) = ev {
            input.events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: down,
                modifiers: Default::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(id) = overlay(ui, false, 0, rect, &|_| Some(at)) {
                    clicked = Some(id);
                }
            });
        });
    }
    assert!(clicked.is_some());
    assert!(take_focus());
    assert!(!take_focus());
    with(|m| *m = Marks::default());
}

#[test]
fn mark_keeps_pose_and_overlay_can_be_suppressed() {
    use eframe::egui;
    let pose = crate::ai::point::PoseResolved {
        target: [1.0, 2.0, 3.0],
        yaw: 0.5,
        pitch: 0.25,
        distance: 9.0,
    };
    let mut marks = Marks::default();
    let id = marks.add([0.0; 3], view(), 1);
    assert_eq!(marks.get(id).unwrap().pose, None);
    marks.set_pose(id, Some(pose));
    assert_eq!(marks.get(id).unwrap().pose, Some(pose));

    with(|m| {
        m.add([0.0; 3], view(), 1);
    });
    let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
    let at = egui::pos2(100.0, 100.0);
    let painted = |hidden: bool| {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(rect);
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut f = || overlay(ui, false, 0, rect, &|_| Some(at));
                if hidden {
                    suppressed(f);
                } else {
                    f();
                }
            });
        });
        out.shapes.iter().any(|s| matches!(s.shape, egui::epaint::Shape::Circle(_)))
    };
    assert!(painted(false));
    assert!(!painted(true));
    assert!(painted(false), "the hiding ends with the call");
    with(|m| *m = Marks::default());
}

fn have_gpu() -> bool {
    crate::ai::render_off::gpu().is_ok()
}

fn view_of(kind: ViewKind, level: i64) -> ViewRef {
    ViewRef {
        kind,
        name: "v".into(),
        level: Some(level),
    }
}

/// Draws until two pictures in a row are the same, so a renderer that refines over frames is
/// compared once it has settled; a picture that never settles fails the test.
fn settled(mut draw: impl FnMut() -> ImageData) -> ImageData {
    let mut last = draw();
    for _ in 0..8 {
        let next = draw();
        if hash(&next) == hash(&last) {
            return next;
        }
        last = next;
    }
    panic!("the picture does not settle");
}

fn hash(img: &ImageData) -> u64 {
    crate::ai::snapshot::pixel_hash(img).unwrap()
}

#[test]
fn from_orbit_inverts_orbit() {
    for (yaw, pitch, dist) in [(0.5, 0.25, 9.0), (-2.0, 0.9, 30.0), (3.0, -0.3, 4.0)] {
        let r = crate::ai::pose::from_orbit((800, 500), [1.0, 2.0, 3.0], yaw, pitch, dist, [0.0; 6]);
        let (t, y, p, d) = r.orbit();
        assert_eq!(t, [1.0, 2.0, 3.0]);
        assert!((y - yaw).abs() < 1e-9 && (p - pitch).abs() < 1e-9 && (d - dist).abs() < 1e-9, "{yaw} {pitch}");
    }
}

#[test]
fn failed_codes_follow_the_error() {
    use crate::ai::AiError;
    assert_eq!(code_of(&AiError::Busy), RefusalCode::Busy);
    assert_eq!(code_of(&AiError::Stale), RefusalCode::PlanStale);
    assert_eq!(code_of(&AiError::Provider("x".into())), RefusalCode::Internal);
}

#[test]
fn before_render_matches_pre_apply() {
    if !have_gpu() {
        return;
    }
    for kind in [ViewKind::Plan, ViewKind::View3d] {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, 3);
        let pose = (kind == ViewKind::View3d).then_some(crate::ai::point::PoseResolved {
            target: [6.0, 0.0, 1.0],
            yaw: 0.8,
            pitch: 0.5,
            distance: 25.0,
        });
        let v = view_of(kind, ed.level);
        let size = (320, 200);
        let pre = settled(|| picture(&ed, &v, pose, size).unwrap());
        let mut marks = Marks::default();
        let id = marks.add([0.0; 3], v.clone(), 1);
        marks.set_pose(id, pose);
        marks.answered(1);
        apply(&mut ed, &mut marks, 1, 4);
        let after = settled(|| picture(&ed, &v, pose, size).unwrap());
        assert_ne!(hash(&pre), hash(&after), "{kind:?}: the apply must show");
        let before = settled(|| marks.before_picture(&ed, id, size).unwrap());
        assert_eq!(hash(&before), hash(&pre), "{kind:?}: as it was = before the apply");
        assert_eq!(count(&ed.model), 7);
    }
}

#[test]
fn before_without_a_picture_for_sections() {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, 1);
    let v = view_of(ViewKind::Section, ed.level);
    assert!(picture(&ed, &v, None, (64, 40)).is_err());
}

#[test]
#[cfg_attr(debug_assertions, ignore = "the budget is for release builds")]
fn before_editor_on_5000_walls_within_a_second() {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, 5000);
    let mut marks = Marks::default();
    let id = marks.add([0.0; 3], view(), 1);
    marks.answered(1);
    apply(&mut ed, &mut marks, 1, 10);
    let t = std::time::Instant::now();
    let copy = marks.before_editor(&ed, id).unwrap();
    let took = t.elapsed();
    assert_eq!(count(&copy.model), 5000);
    assert!(took.as_millis() <= 1000, "rollback copy took {took:?}");
}
