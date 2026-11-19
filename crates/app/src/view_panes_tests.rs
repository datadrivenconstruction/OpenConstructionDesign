use super::*;
use crate::modify_tools::tests::walls;
use ddc_model::SectionSpec;

fn area() -> Rect {
    Rect::from_min_max(pos2(240.5, 96.0), pos2(1301.0, 873.0))
}

fn spec(kind: ViewKind, a: (f64, f64), b: (f64, f64)) -> SectionSpec {
    SectionSpec {
        kind,
        name: "v".into(),
        a,
        b,
        depth: 10.0,
        far_clip: false,
    }
}

fn room(ed: &mut Editor) {
    walls(
        ed,
        &[
            ((0.0, 0.0), (8.0, 0.0)),
            ((8.0, 0.0), (8.0, 6.0)),
            ((8.0, 6.0), (0.0, 6.0)),
            ((0.0, 6.0), (0.0, 0.0)),
        ],
    );
}

#[test]
fn every_layout_tiles_the_area_without_overlap() {
    for a in [area(), Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(641.0, 401.0))] {
        for l in Layout::ALL {
            let rs = rects(l, a);
            assert_eq!(rs.len(), l.count(), "{l:?}");
            let mut sum = 0.0;
            for (i, r) in rs.iter().enumerate() {
                assert!(r.is_positive(), "{l:?} pane {i} {r:?}");
                assert!(a.expand(1e-3).contains_rect(*r), "{l:?} pane {i} leaves the area");
                for q in &rs[i + 1..] {
                    assert!(r.intersect(*q).area() <= 0.0, "{l:?} {r:?} overlaps {q:?}");
                }
                sum += r.area();
            }
            // what the panes leave is only the dividing lines
            let lines = a.area() - sum;
            let most = DIV * (a.width() + a.height()) + 1e-3;
            assert!(lines >= 0.0 && lines <= most, "{l:?}: {lines} of lines");
            // neighbours are one divider apart
            if l != Layout::One {
                let gaps = rs.iter().flat_map(|r| {
                    rs.iter().filter_map(move |q| {
                        let dx = q.left() - r.right();
                        let dy = q.top() - r.bottom();
                        (dx > 0.0 && dx < 10.0).then_some(dx).or((dy > 0.0 && dy < 10.0).then_some(dy))
                    })
                });
                for g in gaps {
                    assert!((g - DIV).abs() < 1e-3, "{l:?}: gap {g}");
                }
            }
        }
    }
}

#[test]
fn quad_puts_plan_and_3d_on_a_diagonal_and_the_split_on_whole_points() {
    let rs = rects(Layout::Quad, area());
    assert!(rs[0].left() == area().left() && rs[0].top() == area().top());
    assert!(rs[3].right() == area().right() && rs[3].bottom() == area().bottom());
    let x = rs[0].right() + DIV * 0.5;
    assert_eq!(x, x.round());
    let p = Panes::default();
    assert_eq!(p.panes[0].src, Src::Plan(0));
    assert_eq!(p.panes[3].src, Src::D3(0));
}

#[test]
fn plans_and_3d_views_open_side_by_side() {
    let mut p = Panes::default();
    let n = |p: &Panes, s: Src| p.panes.iter().filter(|x| x.src == s).count();
    // plans of two levels, and the same plan twice
    p.set_src(1, Src::Plan(5));
    p.set_src(2, Src::Plan(0));
    assert_eq!((n(&p, Src::Plan(0)), n(&p, Src::Plan(5))), (2, 1));
    // a second 3D view is a new one; one 3D view still has a single pane (one frame)
    assert_eq!(p.new_cam(), Some(1));
    p.set_src(1, Src::D3(1));
    assert_eq!(p.new_cam(), Some(2));
    assert_eq!((n(&p, Src::D3(0)), n(&p, Src::D3(1))), (1, 1));
    p.set_src(2, Src::D3(0));
    assert_eq!((p.panes[2].src, p.panes[3].src), (Src::D3(0), Src::Plan(0)), "3D 1 moved");
    // sections may repeat
    p.set_src(0, Src::View(7));
    p.set_src(3, Src::View(7));
    assert_eq!(n(&p, Src::View(7)), 2);
    // four views at most
    for (i, k) in [(0, 2), (3, 3)] {
        p.set_src(i, Src::D3(k));
    }
    assert_eq!(p.new_cam(), None);
}

#[test]
fn every_3d_view_keeps_its_camera_style_and_section_box() {
    let mut p = Panes {
        on: true,
        ..Default::default()
    };
    let mut view = Viewport::new();
    view.camera.yaw = 0.3;
    view.slot = 0;
    let bx = ddc_render::Aabb {
        min: ddc_render::glam::Vec3::new(0.0, 0.0, 0.0),
        max: ddc_render::glam::Vec3::new(4.0, 3.0, 2.5),
    };
    view.section_box = Some(bx);
    let e3_marker = crate::edit3d::plane::Source::Named(42);
    view.e3.plane.source = e3_marker;
    p.set_src(1, Src::D3(1));
    p.panes[1].style = VisualStyle::Wireframe;
    // the new view starts where the main one is, on a renderer slot of its own
    p.active = 1;
    p.make_main(&mut view, 1);
    assert_eq!((p.main, view.slot), (1, 1));
    assert_eq!(view.camera.yaw, 0.3);
    assert_eq!(view.section_box, Some(bx));
    // it turns and drops its box; the first one keeps its own
    view.camera.yaw = 1.2;
    view.section_box = None;
    view.nav.style = VisualStyle::Wireframe;
    assert_eq!(p.camera_of(&view, 0).map(|c| c.yaw), Some(0.3));
    p.make_main(&mut view, 0);
    assert_eq!((p.main, view.slot, view.camera.yaw), (0, 0, 0.3));
    assert_eq!(view.section_box, Some(bx));
    assert_eq!(view.nav.style, VisualStyle::default());
    assert_eq!(p.camera_of(&view, 1).map(|c| c.yaw), Some(1.2));
    // tools and the work plane stay the editor's whichever view is main
    assert_eq!(view.e3.plane.source, e3_marker);
    assert_eq!(p.main_pane(), Some(3));
}

#[test]
fn activating_a_plan_pane_makes_its_level_live() {
    let mut ed = Editor::new_in_memory().unwrap();
    room(&mut ed);
    let l0 = ed.level;
    let l1 = ed.add_level("L1", 3.0).unwrap();
    ed.set_level(l0);
    let mut p = Panes::default();
    p.enable(&ed, VisualStyle::default());
    p.set_src(1, Src::Plan(l1));
    let mut plan = crate::plan_view::PlanView::default();
    plan.center = v2(4.0, 3.0);
    plan.scale = 77.0;
    let mut view = Viewport::new();
    assert_eq!(p.plan_pane(l0), Some(0));
    // L1 has a shown pane, but only the editor's level draws live
    assert_eq!(p.plan_pane(l1), Some(1));
    assert!(p.draws_live_plan(0, ed.level));
    assert!(!p.draws_live_plan(1, ed.level), "only the editor's level draws live");
    p.active = 1;
    p.activate(&mut ed, &mut plan, &mut view);
    assert_eq!(ed.level, l1);
    assert_eq!(p.plan_pane(l1), Some(1));
    assert!(p.draws_live_plan(1, ed.level));
    assert!(!p.draws_live_plan(0, ed.level), "the plan it left draws read-only");
    // the plan it left stays where it was, read-only
    assert_eq!(p.sviews[0].plan_level, Some(l0));
    assert_eq!((p.sviews[0].center, p.sviews[0].scale), (v2(4.0, 3.0), 77.0));
    // a level picked elsewhere moves the live plan pane along
    ed.set_level(l0);
    p.check(&ed);
    assert_eq!((p.panes[0].src, p.panes[1].src), (Src::Plan(l0), Src::Plan(l0)));
    // a deleted level leaves no pane on it
    p.set_src(2, Src::Plan(l1));
    assert!(ed.delete_level(l1));
    p.check(&ed);
    assert_eq!(p.panes[2].src, Src::Plan(ed.level));
}

#[test]
fn a_level_plan_converts_to_drawn_and_pickable_pieces() {
    let mut ed = Editor::new_in_memory().unwrap();
    room(&mut ed);
    let v = ed.model.plan_view_ranged(ed.level, vec![]).unwrap();
    let cuts = v.count(|p| matches!(p, ddc_model::ViewPrim::Cut { .. }));
    assert!(cuts >= 4);
    let d = plan_as_drawing(v);
    let walls = d.count(|p| matches!(p, ddc_model::ViewPrim::SectionCut { table: "wall", .. }));
    assert_eq!(walls, cuts);
    assert_eq!(d.count(|p| matches!(p, ddc_model::ViewPrim::Cut { .. })), 0);
}

#[test]
fn styles_belong_to_the_slot() {
    let mut p = Panes::default();
    p.panes[1].style = VisualStyle::Wireframe;
    p.panes[3].style = VisualStyle::Realistic;
    p.set_src(1, Src::D3(0));
    assert_eq!(p.panes[1].style, VisualStyle::Wireframe, "3D now draws in pane 1's style");
    assert_eq!(p.panes[3].style, VisualStyle::Realistic);
}

#[test]
fn a_smaller_layout_keeps_the_active_pane_and_3d_on_screen() {
    let mut p = Panes::default();
    p.active = 0;
    p.set_layout(Layout::Cols2);
    let shown: Vec<Src> = p.shown().iter().map(|&i| p.panes[i].src).collect();
    assert_eq!(shown, vec![Src::Plan(0), Src::D3(0)]);
    let mut p = Panes::default();
    p.active = 3;
    p.set_layout(Layout::One);
    assert_eq!(p.shown(), vec![0]);
    assert_eq!((p.active, p.panes[0].src), (0, Src::D3(0)));
    let mut p = Panes::default();
    p.active = 2;
    p.set_layout(Layout::Left3);
    let shown: Vec<Src> = p.shown().iter().map(|&i| p.panes[i].src).collect();
    assert!(shown.contains(&Src::D3(0)) && shown.contains(&Src::Plan(0)), "{shown:?}");
    assert!(p.active < 3);
    // back to four: all four slots
    p.set_layout(Layout::Quad);
    assert_eq!(p.shown(), vec![0, 1, 2, 3]);
}

#[test]
fn maximize_shows_the_active_pane_over_the_whole_area_and_restores() {
    let mut p = Panes {
        on: true,
        ..Default::default()
    };
    p.active = 3;
    p.toggle_max();
    assert_eq!(p.layout_rects(area()), vec![(3, area())]);
    assert!(p.shows(Src::D3(0)) && p.shows_3d() && !p.shows(Src::Plan(0)));
    p.toggle_max();
    assert_eq!(p.layout_rects(area()).len(), 4);
    // a new layout ends the maximized state
    p.toggle_max();
    p.set_layout(Layout::Rows2);
    assert_eq!(p.max, None);
}

#[test]
fn focus_finds_the_pane_or_fills_the_active_one() {
    let mut p = Panes {
        on: true,
        ..Default::default()
    };
    p.focus(Src::D3(0));
    assert_eq!(p.active, 3);
    // the 3D tab goes to any 3D pane
    p.set_src(3, Src::D3(2));
    p.active = 0;
    p.focus(Src::D3(p.main));
    assert_eq!(p.active, 3);
    p.active = 1;
    p.focus(Src::View(4));
    assert_eq!(p.panes[1].src, Src::View(4));
}

#[test]
fn front_and_side_are_found_by_look() {
    // looking north: line west to east (the view looks to the left of a->b)
    let north = spec(ViewKind::Elevation, (0.0, -5.0), (10.0, -5.0));
    let west = spec(ViewKind::Elevation, (15.0, 0.0), (15.0, 10.0));
    let sec_north = spec(ViewKind::Section, (0.0, 2.0), (10.0, 2.0));
    let south = spec(ViewKind::Elevation, (10.0, 15.0), (0.0, 15.0));
    let views = vec![(1, sec_north), (2, north), (3, west), (4, south)];
    let f = FRONT_LOOK;
    let s = SIDE_LOOK;
    assert_eq!(pick_by_look(&views, v2(f.0, f.1)), Some(2), "elevation before section");
    assert_eq!(pick_by_look(&views, v2(s.0, s.1)), Some(3));
    assert_eq!(pick_by_look(&views[..1], v2(f.0, f.1)), Some(1), "a section when alone");
    assert_eq!(pick_by_look(&views[3..], v2(s.0, s.1)), None, "nothing looks west");
}

#[test]
fn seeding_uses_the_compass_elevations_and_empties_refill() {
    let mut ed = Editor::new_in_memory().unwrap();
    room(&mut ed);
    let mut p = Panes::default();
    p.enable(&ed, VisualStyle::HiddenLine);
    assert_eq!(p.panes[1].src, Src::Empty, "no elevations yet");
    assert!(p.panes.iter().all(|x| x.style == VisualStyle::HiddenLine));
    assert_eq!(ed.add_compass(), 4);
    p.check(&ed);
    let look_of = |p: &Panes, i: usize| match p.panes[i].src {
        Src::View(id) => look(ed.view_spec(id).unwrap()),
        s => panic!("pane {i} shows {s:?}"),
    };
    let (fl, sl) = (look_of(&p, 1), look_of(&p, 2));
    assert!(fl.y > 0.7, "front looks north: {fl:?}");
    assert!(sl.x < -0.7, "side looks west: {sl:?}");
    // a deleted view empties its pane, which then takes no other view twice
    let Src::View(front) = p.panes[1].src else { unreachable!() };
    ed.delete_view(front);
    p.check(&ed);
    assert_eq!(p.panes[1].src, Src::Empty);
    assert_eq!(p.panes.iter().filter(|x| matches!(x.src, Src::View(_))).count(), 1);
}

#[test]
fn commands_toggle_panes_and_maximize() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    a.prefs.path = None;
    assert!(!a.panes.on);
    assert!(a.run("view.pane_max"));
    assert_eq!(a.panes.max, None, "nothing to maximize without panes");
    assert_eq!(a.ed.status, Some(a.sh.cat.t("view_cube.max.no_panes")));
    let before = a.ed.revision;
    assert!(a.run("view.panes"));
    assert!(a.panes.on);
    assert_eq!(a.ed.revision, before, "panes are view state, not a model edit");
    assert!(a.run("view.pane_max"));
    assert_eq!(a.panes.max, Some(a.panes.active));
    assert!(a.run("view.pane_max"));
    assert_eq!(a.panes.max, None);
    assert!(a.run("view.panes"));
    assert!(!a.panes.on);
}

#[test]
fn pane_strings_exist_in_both_languages() {
    let (en, ru) = (Catalog::new(ddc_ui::Lang::En), Catalog::new(ddc_ui::Lang::Ru));
    let mut keys: Vec<&str> = Layout::ALL.iter().map(|l| l.key()).collect();
    keys.extend([
        "view_cube.src.pick",
        "view_cube.src.plan",
        "view_cube.src.d3n",
        "view_cube.src.d3new",
        "view_cube.pane.max",
        "view_cube.pane.restore",
        "view_cube.pane.menu",
        "view_cube.menu.style",
        "view_cube.menu.layout",
        "view_cube.menu.off",
        "view_cube.detail.template",
        "view_cube.detail.plan",
        "view_cube.empty.title",
        "view_cube.empty.hint",
        "view_cube.empty.action",
        "view_cube.panes.on",
        "view_cube.panes.off",
        "view_cube.max.no_panes",
        "cmd.view.panes",
        "cmd.view.pane_max",
    ]);
    for k in keys {
        assert_ne!(en.t(k), k, "{k}");
        assert_ne!(ru.t(k), k, "{k}");
        assert_ne!(en.t(k), ru.t(k), "{k}");
    }
    for s in VisualStyle::ALL {
        let k = format!("v3d.style.{}", s.key());
        assert_ne!(en.t(&k), k);
    }
    for d in DetailLevel::ALL {
        let k = format!("view_gfx.detail.{}", d.key());
        assert_ne!(ru.t(&k), k);
    }
}

/// Opening or importing a project shows all of it in every 3D view, the parked ones too,
/// whichever view is the main one.
#[test]
fn a_new_model_is_fitted_in_every_3d_view() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    a.prefs.path = None;
    a.panes.enable(&a.ed, VisualStyle::default());
    for k in [1, 2, 0, 1] {
        a.panes.make_main(&mut a.view, k);
    }
    assert_eq!(a.panes.main, 1);
    let parked = |a: &EditorApp| a.panes.cams.iter().flatten().count();
    assert_eq!(parked(&a), 2);
    a.view.keep_camera();
    for c in a.panes.cams.iter_mut().flatten() {
        c.keep_camera();
        c.camera.distance = 999.0;
    }
    a.ed = Editor::new_in_memory().unwrap();
    crate::project_ui::after_swap(&mut a);
    assert!(a.view.needs_fit(), "the main view");
    assert!(a.panes.cams.iter().flatten().all(|c| c.needs_fit()), "every parked view");
    assert_eq!(parked(&a), 2, "the views stay, only their cameras are fitted");
}

/// Texts a read-only plan pane of `level` paints in one frame.
fn plan_pane_texts(ed: &mut Editor, sv: &mut SectionView, level: i64) -> Vec<String> {
    use crate::layout_contract::{driven_ctx, pass};
    let (ctx, mut th) = driven_ctx();
    let tok = crate::ui_kit::from_theme(&th);
    let vis = crate::isolate::Vis::default();
    sv.set_plan(Some(level));
    let mut p = None;
    for _ in 0..3 {
        let mut frame = |ctx: &egui::Context| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| sv.show(ui, ed, crate::section_view::PLAN_ID, &tok, &vis, &mut None));
        };
        p = Some(pass(&ctx, &mut th, egui::Id::new("view4.ro"), vec![], &mut frame));
    }
    p.unwrap().text
}

/// A plan pane of a level that is not the editor's draws that level's tags and placed
/// dimensions as its live plan does.
#[test]
fn a_read_only_plan_draws_tags_and_dimensions_of_its_level() {
    use ddc_model::{DimKind, DimRef, DimSpec, TagHost, TagSpec};
    let mut ed = Editor::new_in_memory().unwrap();
    let l0 = ed.level;
    let l1 = ed.add_level("L1", 3.0).unwrap();
    ed.set_level(l1);
    room(&mut ed);
    let w = ed.walls[0].id;
    ed.set_level(l0);
    let mut sv = SectionView::default();
    let before = plan_pane_texts(&mut ed, &mut sv, l1);
    ed.model
        .add_dim(&DimSpec {
            level: l1,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::WallEnd { wall: w, face: 0, end: 0 },
                DimRef::WallEnd { wall: w, face: 0, end: 1 },
            ],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 0.8,
            tier: 0,
            auto: false,
        })
        .unwrap();
    ed.model
        .add_tags(&[TagSpec { level: l1, host: TagHost::Wall(w), offset: (0.0, 0.6), leader: false }])
        .unwrap();
    ed.reload().unwrap();
    assert_eq!(ed.level, l0, "the pane does not switch the editor's level");
    let after = plan_pane_texts(&mut ed, &mut sv, l1);
    let tag = ed.model.tags(l1).unwrap().remove(0);
    for line in crate::modify_tools::tags::tag_lines(&tag) {
        assert!(after.contains(&line), "tag text {line:?} missing: {after:?}");
    }
    let added: Vec<&String> = after.iter().filter(|s| !before.contains(s)).collect();
    let tag_lines = crate::modify_tools::tags::tag_lines(&tag);
    assert!(
        added.iter().any(|s| !tag_lines.contains(s)),
        "the dimension's value is drawn: {added:?}"
    );
}
