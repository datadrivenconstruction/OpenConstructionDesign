use super::*;
use ddc_model::{LayerSpec, WallSpec};

fn editor() -> Editor {
    with(|s| *s = State::default());
    Editor::new_in_memory().unwrap()
}

fn plan(e: &Editor) -> Vec<Anno> {
    e.model.annos(AnnoView::Plan(e.level)).unwrap()
}

fn clicks(e: &mut Editor, pts: &[(f64, f64)]) {
    for p in pts {
        e.handle(Event::Click(v2(p.0, p.1)));
    }
}

/// Node line (0,0)-(4,0), tool released.
fn with_line(e: &mut Editor) -> i64 {
    arm(e, Kind::DetailLine, Lang::En);
    clicks(e, &[(0.0, 0.0), (4.0, 0.0)]);
    e.handle(Event::Key(Key::Enter));
    e.set_mode(Mode::Select);
    plan(e)[0].id
}

#[test]
fn text_is_typed_in_place_and_placed_as_one_undo_step() {
    let mut e = editor();
    arm(&mut e, Kind::Text, Lang::En);
    clicks(&mut e, &[(1.0, 1.0)]);
    assert!(with(|s| s.edit.is_some()));
    assert!(plan(&e).is_empty());
    with(|s| s.edit.as_mut().unwrap().buf = "Line 1\nLine 2\n".into());
    // a click outside the field commits, rather than placing the next text
    clicks(&mut e, &[(9.0, 9.0)]);
    assert!(with(|s| s.edit.is_none()));
    let all = plan(&e);
    assert_eq!(all.len(), 1);
    match &all[0].shape {
        AnnoShape::Text(n) => assert_eq!(n.text, "Line 1\nLine 2"),
        s => panic!("{s:?}"),
    }
    e.undo();
    assert!(plan(&e).is_empty());
}

#[test]
fn empty_new_text_is_not_placed_and_cancel_keeps_the_tool() {
    let mut e = editor();
    arm(&mut e, Kind::Text, Lang::En);
    clicks(&mut e, &[(1.0, 1.0), (2.0, 2.0)]);
    assert!(plan(&e).is_empty());
    clicks(&mut e, &[(1.0, 1.0)]);
    e.handle(Event::Key(Key::Escape));
    assert!(with(|s| s.edit.is_none() && s.draft.is_some()));
    e.handle(Event::Key(Key::Escape));
    assert!(!busy());
    assert_eq!(e.wall_mode(), Mode::Select);
}

#[test]
fn second_click_edits_existing_text_and_clearing_deletes_it() {
    let mut e = editor();
    arm(&mut e, Kind::Text, Lang::En);
    clicks(&mut e, &[(1.0, 1.0)]);
    with(|s| s.edit.as_mut().unwrap().buf = "Old".into());
    clicks(&mut e, &[(9.0, 9.0)]);
    e.set_mode(Mode::Select);
    let id = plan(&e)[0].id;
    clicks(&mut e, &[(1.0, 1.0)]);
    assert_eq!(single(&e), Some(id));
    assert!(with(|s| s.edit.is_none()));
    clicks(&mut e, &[(1.0, 1.0)]);
    assert_eq!(with(|s| s.edit.as_ref().unwrap().buf.clone()), "Old");
    with(|s| s.edit.as_mut().unwrap().buf = "New".into());
    clicks(&mut e, &[(9.0, 9.0)]);
    match &e.model.anno(id).unwrap().shape {
        AnnoShape::Text(n) => assert_eq!(n.text, "New"),
        s => panic!("{s:?}"),
    }
    e.undo();
    match &e.model.anno(id).unwrap().shape {
        AnnoShape::Text(n) => assert_eq!(n.text, "Old"),
        s => panic!("{s:?}"),
    }
    e.selection = vec![Sel::Anno(id)];
    edit_existing(&e, id);
    with(|s| s.edit.as_mut().unwrap().buf = "  ".into());
    commit_edit(&mut e);
    assert!(plan(&e).is_empty());
}

#[test]
fn leader_collects_tip_then_text_position() {
    let mut e = editor();
    arm(&mut e, Kind::Leader, Lang::En);
    clicks(&mut e, &[(0.0, 0.0), (3.0, 3.0)]);
    with(|s| s.edit.as_mut().unwrap().buf = "Note".into());
    commit_edit(&mut e);
    let all = plan(&e);
    assert_eq!(all.len(), 1);
    match &all[0].shape {
        AnnoShape::Text(n) => {
            assert_eq!(n.leaders.len(), 1);
            assert_ne!(n.leaders[0].to, n.pos);
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn second_leader_is_added_to_selected_text_and_ends_the_tool() {
    let mut e = editor();
    arm(&mut e, Kind::Text, Lang::En);
    clicks(&mut e, &[(1.0, 1.0)]);
    with(|s| s.edit.as_mut().unwrap().buf = "Note".into());
    commit_edit(&mut e);
    let id = plan(&e)[0].id;
    let v = AnnoView::Plan(e.level);
    apply(&mut e, v, Some(Act::AddLeader(id)));
    clicks(&mut e, &[(5.0, 5.0)]);
    assert_eq!(e.wall_mode(), Mode::Select);
    match &e.model.anno(id).unwrap().shape {
        AnnoShape::Text(n) => assert_eq!(n.leaders.len(), 1),
        s => panic!("{s:?}"),
    }
}

#[test]
fn detail_line_finishes_on_enter_or_close_as_one_step() {
    let mut e = editor();
    arm(&mut e, Kind::DetailLine, Lang::En);
    clicks(&mut e, &[(0.0, 0.0)]);
    e.handle(Event::Key(Key::Enter));
    assert!(plan(&e).is_empty(), "one point is not a line");
    clicks(&mut e, &[(4.0, 0.0), (4.0, 4.0)]);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(plan(&e).len(), 1);
    clicks(&mut e, &[(0.0, 8.0), (4.0, 8.0), (4.0, 12.0)]);
    e.handle(Event::Key(Key::Close));
    let all = plan(&e);
    assert_eq!(all.len(), 2);
    assert!(matches!(all[1].shape, AnnoShape::Line { closed: true, .. }));
    e.undo();
    assert_eq!(plan(&e).len(), 1);
}

#[test]
fn backspace_and_undo_take_back_draft_points_only() {
    let mut e = editor();
    let first = with_line(&mut e);
    arm(&mut e, Kind::Region, Lang::En);
    clicks(&mut e, &[(0.0, 2.0), (4.0, 2.0), (4.0, 6.0)]);
    e.handle(Event::Key(Key::Backspace));
    assert_eq!(with(|s| s.draft.as_ref().unwrap().pts.len()), 2);
    e.undo();
    assert_eq!(with(|s| s.draft.as_ref().unwrap().pts.len()), 0);
    assert_eq!(plan(&e)[0].id, first, "the placed line stays");
}

#[test]
fn region_takes_fill_options_and_a_second_contour() {
    let mut e = editor();
    with(|s| s.opts.fill = 0);
    arm(&mut e, Kind::Region, Lang::En);
    clicks(
        &mut e,
        &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
    );
    let v = AnnoView::Plan(e.level);
    apply(&mut e, v, Some(Act::NextRing));
    clicks(&mut e, &[(2.0, 2.0), (4.0, 2.0), (4.0, 4.0)]);
    e.handle(Event::Key(Key::Enter));
    let all = plan(&e);
    assert_eq!(all.len(), 1);
    match &all[0].shape {
        AnnoShape::Region { rings, fill, .. } => {
            assert_eq!(rings.len(), 2);
            assert!(matches!(fill, RegionFill::Solid { .. }));
        }
        s => panic!("{s:?}"),
    }
    e.undo();
    assert!(plan(&e).is_empty());
}

#[test]
fn region_fill_does_not_catch_clicks_but_its_outline_does() {
    let mut e = editor();
    arm(&mut e, Kind::Region, Lang::En);
    clicks(
        &mut e,
        &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
    );
    e.handle(Event::Key(Key::Enter));
    e.set_mode(Mode::Select);
    let v = AnnoView::Plan(e.level);
    assert_eq!(hit(&e, v, v2(5.0, 5.0), 0.1), None);
    assert_eq!(hit(&e, v, v2(5.0, 0.0), 0.1), Some(plan(&e)[0].id));
}

#[test]
fn spots_cloud_and_details_place_with_their_click_counts() {
    let mut e = editor();
    arm(&mut e, Kind::SpotElev, Lang::En);
    clicks(&mut e, &[(1.0, 1.0)]);
    assert!(plan(&e).is_empty());
    clicks(&mut e, &[(2.0, 2.0)]);
    assert_eq!(plan(&e).len(), 1);
    arm(&mut e, Kind::SpotCoord, Lang::En);
    clicks(&mut e, &[(3.0, 3.0), (4.0, 4.0)]);
    assert!(matches!(
        plan(&e)[1].shape,
        AnnoShape::Spot {
            kind: SpotKind::Coordinate,
            ..
        }
    ));
    arm(&mut e, Kind::Cloud, Lang::En);
    clicks(&mut e, &[(0.0, 0.0), (5.0, 0.0), (5.0, 5.0)]);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(plan(&e).len(), 3);
    let defs = e.model.detail_defs().unwrap();
    let fixed = defs.iter().find(|d| d.fit == DetailFit::Fixed).unwrap().id;
    let long = defs.iter().find(|d| d.fit != DetailFit::Fixed).unwrap().id;
    with(|s| s.opts.def = Some(fixed));
    arm(&mut e, Kind::Detail, Lang::En);
    clicks(&mut e, &[(6.0, 6.0)]);
    assert_eq!(plan(&e).len(), 4);
    with(|s| s.opts.def = Some(long));
    clicks(&mut e, &[(0.0, 8.0)]);
    assert_eq!(plan(&e).len(), 4);
    clicks(&mut e, &[(3.0, 8.0)]);
    // the list is in drawing order (details first): the long one is the second detail
    let details: Vec<Anno> = plan(&e)
        .into_iter()
        .filter(|a| matches!(a.shape, AnnoShape::Detail { .. }))
        .collect();
    assert_eq!(details.len(), 2);
    match &details[1].shape {
        AnnoShape::Detail { length, .. } => assert!(length.is_some()),
        s => panic!("{s:?}"),
    }
    e.undo();
    assert_eq!(plan(&e).len(), 4);
}

#[test]
fn line_is_selected_dragged_by_grip_and_body_then_deleted() {
    let mut e = editor();
    let id = with_line(&mut e);
    let pts = |e: &Editor| match e.model.anno(id).unwrap().shape {
        AnnoShape::Line { pts, .. } => pts,
        s => panic!("{s:?}"),
    };
    clicks(&mut e, &[(2.0, 0.0)]);
    assert_eq!(single(&e), Some(id));
    let (a, b) = (pts(&e)[0], pts(&e)[1]);
    e.handle(Event::Press(v2(b.0, b.1)));
    e.handle(Event::Move(v2(5.0, 1.0)));
    assert!(dragging());
    e.handle(Event::Release(v2(5.0, 1.0)));
    assert!(!dragging());
    assert_eq!(pts(&e), vec![a, (5.0, 1.0)]);
    e.undo();
    assert_eq!(pts(&e), vec![a, b]);

    e.handle(Event::Press(v2(2.0, 0.0)));
    e.handle(Event::Release(v2(2.0, 3.0)));
    assert_eq!(pts(&e), vec![(a.0, a.1 + 3.0), (b.0, b.1 + 3.0)]);

    e.selection = vec![Sel::Anno(id)];
    assert!(delete_selected(&mut e));
    assert!(plan(&e).is_empty());
    e.undo();
    assert_eq!(plan(&e).len(), 1);
}

#[test]
fn dragging_text_leaves_leader_tips_in_place() {
    let mut e = editor();
    arm(&mut e, Kind::Leader, Lang::En);
    clicks(&mut e, &[(0.0, 0.0), (3.0, 3.0)]);
    with(|s| s.edit.as_mut().unwrap().buf = "Note".into());
    commit_edit(&mut e);
    e.set_mode(Mode::Select);
    let a = plan(&e)[0].clone();
    let AnnoShape::Text(before) = a.shape.clone() else {
        panic!()
    };
    e.handle(Event::Press(v2(before.pos.0, before.pos.1)));
    e.handle(Event::Release(v2(before.pos.0 + 2.0, before.pos.1)));
    let AnnoShape::Text(after) = e.model.anno(a.id).unwrap().shape else {
        panic!()
    };
    assert_eq!(after.pos.0, before.pos.0 + 2.0);
    assert_eq!(after.leaders[0].to, before.leaders[0].to);
}

#[test]
fn model_selection_takes_over_and_delete_is_not_ours() {
    let mut e = editor();
    let id = with_line(&mut e);
    clicks(&mut e, &[(2.0, 0.0)]);
    e.selection = vec![Sel::Wall(1)];
    assert!(!delete_selected(&mut e));
    assert_eq!(e.model.anno(id).unwrap().id, id);
    assert_eq!(single(&e), None);
}

#[test]
fn annotation_and_wall_go_in_one_delete() {
    let mut e = editor();
    let id = with_line(&mut e);
    crate::modify_tools::tests::walls(&mut e, &[((0.0, 5.0), (4.0, 5.0))]);
    let wall = e.walls[0].id;
    e.selection = vec![Sel::Anno(id), Sel::Wall(wall)];
    let steps = e.undo_len();
    e.delete_selection();
    assert!(plan(&e).is_empty());
    assert!(e.walls.is_empty());
    assert_eq!(e.undo_len(), steps + 1);
    e.undo();
    assert_eq!(plan(&e).len(), 1);
    assert_eq!(e.walls.len(), 1);
}

#[test]
fn delete_and_enter_never_touch_an_annotation_of_another_view() {
    let mut e = editor();
    let id = with_line(&mut e);
    let other = e.model.add_level("L2", 3.0).unwrap();
    let home = e.level;
    e.level = other;
    e.selection = vec![Sel::Anno(id)];
    assert!(!delete_selected(&mut e));
    assert!(e.model.anno(id).is_ok());
    assert!(e.selection.is_empty());

    e.selection = vec![Sel::Anno(id)];
    e.handle(Event::Key(Key::Enter));
    assert!(with(|s| s.edit.is_none()));

    // plan L1, but the screen shows 3D or a sheet
    e.level = home;
    e.selection = vec![Sel::Anno(id)];
    sync_view(&mut e, true, None);
    assert!(!delete_selected(&mut e));
    assert!(e.model.anno(id).is_ok());
    sync_view(&mut e, false, None);
    e.selection = vec![Sel::Anno(id)];
    assert!(delete_selected(&mut e));
    assert!(e.model.anno(id).is_err());
}

#[test]
fn switching_tool_or_view_commits_typed_text() {
    let mut e = editor();
    arm(&mut e, Kind::Text, Lang::En);
    clicks(&mut e, &[(1.0, 1.0)]);
    with(|s| s.edit.as_mut().unwrap().buf = "Typed".into());
    e.set_mode(Mode::Select);
    assert_eq!(plan(&e).len(), 1);

    arm(&mut e, Kind::Text, Lang::En);
    clicks(&mut e, &[(5.0, 5.0)]);
    with(|s| s.edit.as_mut().unwrap().buf = "Second".into());
    sync_view(&mut e, true, None);
    assert_eq!(plan(&e).len(), 2);
    assert!(with(|s| s.edit.is_none()));
}

fn line_pts(e: &Editor, id: i64) -> Vec<P> {
    match e.model.anno(id).unwrap().shape {
        AnnoShape::Line { pts, .. } => pts,
        s => panic!("{s:?}"),
    }
}

#[test]
fn modify_tools_move_and_copy_annotations_as_one_undo_step() {
    use crate::modify_tools::{ModTool, Op};
    let mut e = editor();
    let id = with_line(&mut e);
    e.selection = vec![Sel::Anno(id)];
    let was = line_pts(&e, id);

    let mut t = ModTool::new(Op::Move, &plan_targets(&e));
    t.click(&mut e, v2(0.0, 0.0));
    t.click(&mut e, v2(0.0, 2.0));
    let moved: Vec<P> = was.iter().map(|p| (p.0, p.1 + 2.0)).collect();
    assert_eq!(line_pts(&e, id), moved);
    assert_eq!(e.selection, vec![Sel::Anno(id)]);
    e.undo();
    assert_eq!(line_pts(&e, id), was);

    let mut t = ModTool::new(Op::Copy, &plan_targets(&e));
    t.click(&mut e, v2(0.0, 0.0));
    t.click(&mut e, v2(0.0, 2.0));
    assert_eq!(plan(&e).len(), 2);
    e.undo();
    assert_eq!(plan(&e).len(), 1);
}

#[test]
fn tool_steps_cover_rotate_mirror_and_arrays() {
    let mut e = editor();
    let id = with_line(&mut e);
    let sel = [Sel::Anno(id)];
    let run = |e: &mut Editor, xfs: Vec<AnnoXf>, copy: bool| {
        let step = tool_step(&sel, xfs, copy).unwrap();
        step(&mut e.model).unwrap();
    };
    let rot = AnnoXf::Rotate {
        o: (0.0, 0.0),
        a: std::f64::consts::FRAC_PI_2,
    };
    let q = line_pts(&e, id)[1];
    run(&mut e, vec![rot], false);
    let p = line_pts(&e, id);
    assert!(
        (p[1].0 + q.1).abs() < 1e-9 && (p[1].1 - q.0).abs() < 1e-9,
        "{p:?}"
    );
    let mir = AnnoXf::Mirror {
        a: (0.0, 0.0),
        b: (1.0, 0.0),
    };
    run(&mut e, vec![mir], false);
    assert!((line_pts(&e, id)[1].1 + q.0).abs() < 1e-9);
    run(&mut e, linear_xfs(v2(3.0, 0.0), 3, false), true);
    assert_eq!(plan(&e).len(), 3);
    assert!(tool_step(&[Sel::Wall(1)], vec![rot], false).is_none());

    assert_eq!(
        linear_xfs(v2(4.0, 0.0), 3, true),
        vec![AnnoXf::Move((2.0, 0.0)), AnnoXf::Move((4.0, 0.0))]
    );
    let full = radial_xfs(v2(0.0, 0.0), std::f64::consts::TAU, 4);
    assert_eq!(full.len(), 3);
    assert!(
        matches!(full[0], AnnoXf::Rotate { a, .. } if (a - std::f64::consts::FRAC_PI_2).abs() < 1e-9)
    );
}

#[test]
fn selection_box_and_candidates_see_annotations() {
    use crate::selection::{candidates, in_box, BoxMode};
    let mut e = editor();
    let id = with_line(&mut e);
    let c = candidates(&e, v2(2.0, 0.0), 0.1, |_| true);
    assert_eq!(c.first(), Some(&vec![Sel::Anno(id)]));
    let got = in_box(&e, v2(-1.0, -1.0), v2(5.0, 1.0), BoxMode::Window, |_| true);
    assert!(got.contains(&Sel::Anno(id)));
    let got = in_box(&e, v2(1.0, -1.0), v2(2.0, 1.0), BoxMode::Window, |_| true);
    assert!(!got.contains(&Sel::Anno(id)));
    let got = in_box(&e, v2(1.0, -1.0), v2(2.0, 1.0), BoxMode::Crossing, |_| true);
    assert!(got.contains(&Sel::Anno(id)));
}

#[test]
fn undo_of_placement_drops_the_selection() {
    let mut e = editor();
    let id = with_line(&mut e);
    e.selection = vec![Sel::Anno(id)];
    e.undo();
    assert!(e.selection.is_empty());
}

#[test]
fn unselected_annotation_over_a_wall_leaves_the_press_to_the_wall() {
    let mut e = editor();
    let wt = e
        .model
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let lv = e.level;
    e.model
        .add_walls(&[WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        }])
        .unwrap();
    e.reload().unwrap();
    let id = with_line(&mut e);
    let view = AnnoView::Plan(lv);
    assert!(press_target(&e, view, v2(2.0, 0.0), 0.1, true).is_none());
    e.selection = vec![Sel::Anno(id)];
    assert!(press_target(&e, view, v2(2.0, 0.0), 0.1, true).is_some());
}

#[test]
fn even_odd_cells_leave_the_hole_empty() {
    let area = |c: &[P; 4]| {
        (0..4)
            .map(|i| {
                let (a, b) = (c[i], c[(i + 1) % 4]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum::<f64>()
            .abs()
            / 2.0
    };
    let outer = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
    let hole = vec![(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
    let cells = even_odd_cells(&[outer, hole]);
    let sum: f64 = cells.iter().map(area).sum();
    assert!((sum - 12.0).abs() < 1e-9, "{sum}");
}

#[test]
fn font_size_is_quantized_and_capped() {
    assert!((font_step(16.0) - 16.0).abs() < 1e-3);
    assert_eq!(font_step(16.2), font_step(16.3));
    assert_eq!(font_step(5000.0), TEXT_MAX_PX);
}

#[test]
fn view_scale_is_one_undo_step() {
    let mut e = editor();
    let v = AnnoView::Plan(e.level);
    let before = e.model.anno_scale(v).unwrap();
    apply(&mut e, v, Some(Act::Scale(50.0)));
    assert_eq!(items(&e, v).scale, 50.0);
    e.undo();
    assert_eq!(items(&e, v).scale, before);
}

#[test]
fn dim_chain_click_dimensions_the_wall_as_one_step() {
    let mut e = editor();
    let lv = e.level;
    let wt = e
        .model
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let ids = e
        .model
        .add_walls(&[WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        }])
        .unwrap();
    e.reload().unwrap();
    assert!(!ids.is_empty());
    arm(&mut e, Kind::DimChain, Lang::En);
    clicks(&mut e, &[(3.0, 0.0)]);
    assert!(!e.model.dims(lv).unwrap().is_empty());
    e.undo();
    assert!(e.model.dims(lv).unwrap().is_empty());
}

#[test]
fn hint_strings_exist_in_both_languages() {
    for lang in [Lang::Ru, Lang::En] {
        let c = Catalog::new(lang);
        for k in [
            Kind::Text,
            Kind::Leader,
            Kind::DetailLine,
            Kind::Region,
            Kind::Detail,
            Kind::SpotElev,
            Kind::SpotCoord,
            Kind::Cloud,
            Kind::DimChain,
            Kind::AddLeader,
        ] {
            let key = format!("annot_text.hint.{}", k.key());
            assert_ne!(c.t(&key), key);
            assert_ne!(c.t(k.cmd()), k.cmd());
        }
    }
}

#[test]
fn detail_line_in_drafting_not_in_plan() {
    let mut e = editor();
    e.run_op(OpFn(Arc::new(|m| m.add_drafting("Detail", 10.0).map(|_| ()))))
        .unwrap();
    let d = e.model.draftings().unwrap()[0].id;
    let view = AnnoView::Drafting(d);
    let vw = Vw {
        view,
        snap: false,
        r: 0.05,
    };
    sync_drafting(&mut e, d);
    arm(&mut e, Kind::DetailLine, Lang::En);
    for p in [(0.0, 0.0), (2.0, 0.0)] {
        dispatch(&mut e, Event::Click(v2(p.0, p.1)), vw);
    }
    dispatch(&mut e, Event::Key(Key::Enter), vw);
    e.set_mode(Mode::Select);
    let drawn = e.model.annos(view).unwrap();
    assert_eq!(drawn.len(), 1);
    assert!(matches!(drawn[0].shape, AnnoShape::Line { .. }));
    let id = drawn[0].id;
    // the plan neither lists nor hits it
    assert!(plan(&e).is_empty());
    assert!(hits(&e, v2(1.0, 0.0), 0.1).is_empty());
    assert!(!plan_ids(&e).contains(&id));
    // Delete on the drafting tab removes it as one undo step
    e.selection = vec![Sel::Anno(id)];
    assert!(delete_selected(&mut e));
    assert!(e.model.annos(view).unwrap().is_empty());
    e.undo();
    assert_eq!(e.model.annos(view).unwrap().len(), 1);
    // with the plan on screen it is out of reach of Delete
    sync_view(&mut e, false, None);
    e.selection = vec![Sel::Anno(id)];
    assert!(!delete_selected(&mut e));
    assert_eq!(e.model.annos(view).unwrap().len(), 1);
    // placing the line was one step
    e.undo();
    assert!(e.model.annos(view).unwrap().is_empty());
    assert!(e.model.drafting(d).is_ok());
}

#[test]
fn straight_line_outline_is_its_segment_and_its_middle_selects_it() {
    let mut e = editor();
    let id = with_line(&mut e);
    let o = outlines(&e, id);
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].len(), 2, "{o:?}");
    let mid = (o[0][0] + o[0][1]) * 0.5;
    e.selection.clear();
    e.handle(Event::Click(mid));
    assert_eq!(e.selection, vec![Sel::Anno(id)]);
}

#[test]
fn generic_move_rotate_copy_of_an_annotation_are_one_undo_each() {
    let mut e = editor();
    let id = with_line(&mut e);
    let before = e.model.anno(id).unwrap().shape;
    let g0 = before.grips();
    let t = ddc_model::Target::Anno(id);
    let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;

    e.run_op(OpFn(Arc::new(move |m: &mut Model| m.move_elements(&[t], (1.0, 2.0)))))
        .unwrap();
    let g = e.model.anno(id).unwrap().shape.grips();
    assert!(close(g[0], (g0[0].0 + 1.0, g0[0].1 + 2.0)), "{g:?}");
    e.undo();
    assert_eq!(e.model.anno(id).unwrap().shape, before);

    e.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.rotate_elements(&[t], (0.0, 0.0), std::f64::consts::PI, false).map(|_| ())
    })))
    .unwrap();
    let g = e.model.anno(id).unwrap().shape.grips();
    assert!(close(g[1], (-g0[1].0, -g0[1].1)), "{g:?}");
    e.undo();
    assert_eq!(e.model.anno(id).unwrap().shape, before);

    e.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.copy_elements(&[t], &[(0.0, 5.0)]).map(|_| ())
    })))
    .unwrap();
    assert_eq!(plan(&e).len(), 2);
    e.undo();
    assert_eq!(plan(&e).len(), 1);
    assert_eq!(e.model.anno(id).unwrap().shape, before);
}

/// A plan line and a drafting-view line; the drafting view is on screen.
fn plan_and_drafting(e: &mut Editor) -> (i64, i64, i64) {
    let pid = with_line(e);
    e.run_op(OpFn(Arc::new(|m| m.add_drafting("Detail", 10.0).map(|_| ()))))
        .unwrap();
    let d = e.model.draftings().unwrap()[0].id;
    let view = AnnoView::Drafting(d);
    let vw = Vw {
        view,
        snap: false,
        r: 0.05,
    };
    sync_drafting(e, d);
    arm(e, Kind::DetailLine, Lang::En);
    for p in [(0.0, 0.0), (4.0, 0.0)] {
        dispatch(e, Event::Click(v2(p.0, p.1)), vw);
    }
    dispatch(e, Event::Key(Key::Enter), vw);
    e.set_mode(Mode::Select);
    let did = e.model.annos(view).unwrap()[0].id;
    (pid, did, d)
}

#[test]
fn drafting_tools_target_only_the_drafting_view_items() {
    let mut e = editor();
    let (pid, did, _) = plan_and_drafting(&mut e);
    e.selection = vec![Sel::Anno(pid), Sel::Anno(did), Sel::Wall(1)];
    assert_eq!(plan_targets(&e), vec![Sel::Anno(did)]);
    assert_eq!(outlines(&e, did).len(), 1);
    assert!(outlines(&e, pid).is_empty());
    sync_view(&mut e, false, None);
    assert_eq!(
        plan_targets(&e),
        vec![Sel::Anno(pid), Sel::Wall(1)],
        "plan on screen: the plan items"
    );
}

#[test]
fn drafting_move_rotate_copy_change_only_that_view_one_undo_each() {
    use crate::modify_tools::{ModTool, Op};
    let mut e = editor();
    let (pid, did, d) = plan_and_drafting(&mut e);
    let view = AnnoView::Drafting(d);
    e.selection = vec![Sel::Anno(did)];
    let plan_was = line_pts(&e, pid);
    let was = line_pts(&e, did);
    let tool = |e: &Editor, op| {
        let mut t = ModTool::new(op, &plan_targets(e));
        t.free = true;
        t
    };

    // move
    let steps = e.undo_len();
    let mut t = tool(&e, Op::Move);
    t.click_snapped(&mut e, v2(0.0, 0.0));
    t.click_snapped(&mut e, v2(0.0, 2.0));
    assert_eq!(e.undo_len(), steps + 1);
    let moved: Vec<P> = was.iter().map(|p| (p.0, p.1 + 2.0)).collect();
    assert_eq!(line_pts(&e, did), moved);
    assert_eq!(line_pts(&e, pid), plan_was, "the plan is untouched");
    e.undo();
    assert_eq!(e.undo_len(), steps);
    assert_eq!(line_pts(&e, did), was);

    // rotate a quarter turn about the origin
    let mut t = tool(&e, Op::Rotate);
    t.click_snapped(&mut e, v2(0.0, 0.0));
    t.click_snapped(&mut e, v2(1.0, 0.0));
    t.click_snapped(&mut e, v2(0.0, 1.0));
    assert_eq!(e.undo_len(), steps + 1);
    let p = line_pts(&e, did);
    assert!((p[1].0).abs() < 1e-9 && (p[1].1 - 4.0).abs() < 1e-9, "{p:?}");
    assert_eq!(line_pts(&e, pid), plan_was);
    e.undo();
    assert_eq!(line_pts(&e, did), was);

    // copy: the copy stays in the drafting view
    let mut t = tool(&e, Op::Copy);
    t.click_snapped(&mut e, v2(0.0, 0.0));
    t.click_snapped(&mut e, v2(0.0, 3.0));
    assert_eq!(e.model.annos(view).unwrap().len(), 2);
    assert_eq!(plan(&e).len(), 1, "no copy lands on the plan");
    assert_eq!(line_pts(&e, pid), plan_was);
    e.undo();
    assert_eq!(e.model.annos(view).unwrap().len(), 1);
    assert_eq!(e.undo_len(), steps);
}

#[test]
fn drafting_lane_start_takes_drafting_items_without_plan_snaps() {
    use crate::modify_tools::{Lane, Op};
    let mut e = editor();
    let (_, did, _) = plan_and_drafting(&mut e);
    e.selection = vec![Sel::Anno(did)];
    let mut lane = Lane::default();
    lane.start(Op::Move, &mut e);
    let t = crate::interact::sessions::modify::tool(&e).expect("tool started");
    assert_eq!(t.targets, vec![Sel::Anno(did)]);
    assert!(t.free);
}

#[test]
fn plan_tool_targets_are_not_in_the_drafting_tool_view() {
    // a tool started on the plan keeps plan targets: the drafting tab must not edit them unseen
    let mut e = editor();
    let (pid, did, d) = plan_and_drafting(&mut e);
    assert!(in_tool_view(&e, Sel::Anno(did)));
    assert!(!in_tool_view(&e, Sel::Anno(pid)));
    assert!(!in_tool_view(&e, Sel::Wall(1)));
    sync_view(&mut e, false, None);
    assert!(in_tool_view(&e, Sel::Anno(pid)) && in_tool_view(&e, Sel::Wall(1)));
    assert!(!in_tool_view(&e, Sel::Anno(did)));
    sync_drafting(&mut e, d);
    assert!(in_tool_view(&e, Sel::Anno(did)));
}
