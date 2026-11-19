use super::*;
use crate::modify_tools::tests::walls;
use ddc_geom::v2;

/// A link model from the editor: 200 masonry walls 3 m high on level 0.
fn link_model(segs: &[((f64, f64), (f64, f64))]) -> load::LinkData {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, segs);
    let m = std::mem::replace(&mut ed.model, Model::open(":memory:").unwrap());
    load::build(m).unwrap()
}

#[test]
fn build_elements_plan_and_snaps() {
    let d = link_model(&[((0.0, 0.0), (5.0, 0.0)), ((5.0, 0.0), (5.0, 4.0))]);
    assert_eq!(d.elems.iter().filter(|e| e.kind == "wall").count(), 2);
    assert_eq!(d.walls.len(), 2);
    assert!((d.walls[0].half - 0.1).abs() < 1e-9 && (d.walls[0].z1 - 3.0).abs() < 1e-9);
    let xf = Transform {
        dx: 10.0,
        dy: 20.0,
        ..Default::default()
    };
    let lines = derive::plan_lines(&d.elems, &xf, PLAN_CUT);
    assert!(!lines.is_empty());
    assert!(lines
        .iter()
        .flat_map(|l| [l.0, l.1])
        .all(|p| (9.7..=15.3).contains(&p.x) && (19.7..=24.3).contains(&p.y)));
    // above the walls there are neither lines nor axes for snapping
    assert!(derive::plan_lines(&d.elems, &xf, 3.5).is_empty());
    let snaps = derive::snap_walls(&d.walls, &lines, &xf, PLAN_CUT);
    assert!(snaps.len() >= 2);
    assert!((snaps[0].a.x - 10.0).abs() < 1e-9 && (snaps[0].a.y - 20.0).abs() < 1e-9);
    assert!((snaps[0].b.x - 15.0).abs() < 1e-9);
    assert!(derive::snap_walls(&d.walls, &[], &xf, 3.5).is_empty());
    // a raised link: the host cut plane is below its walls
    let up = Transform { dz: 2.0, ..xf };
    assert!(derive::plan_lines(&d.elems, &up, PLAN_CUT).is_empty());
}

#[test]
fn link_groups_are_grey_and_unselectable() {
    let d = link_model(&[((0.0, 0.0), (5.0, 0.0))]);
    let xf = Transform {
        dz: 1.0,
        ..Default::default()
    };
    let gs = source::place(&d.source, 1, &xf, 3.0);
    assert_eq!(gs.len(), 1);
    assert!(gs.iter().all(|g| g.own
        && g.proto.element == LINK_ELEMENT
        && g.proto.tag == LINK_TAG
        && g.inst.iter().all(|i| i.element == LINK_ELEMENT)));
    let zs: Vec<f32> = gs
        .iter()
        .flat_map(|g| {
            g.inst
                .iter()
                .flat_map(|i| {
                    g.proto
                        .positions
                        .iter()
                        .map(|p| i.m.transform_point3((*p).into()).z)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let (lo, hi) = zs
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &z| (a.min(z), b.max(z)));
    // link world 0..3 to host 1..4, from elevation 3: -2..1
    assert!(
        (lo + 2.0).abs() < 1e-4 && (hi - 1.0).abs() < 1e-4,
        "{lo} {hi}"
    );
    let ed = Editor::new_in_memory().unwrap();
    assert_eq!(ed.sel_of_element(LINK_ELEMENT), None);
}

#[test]
fn clash_host_wall_with_crossing_link_wall() {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, &[((0.0, 0.0), (5.0, 0.0))]);
    let host_wall = ed.walls[0].id;
    let d = link_model(&[((2.5, -2.0), (2.5, 2.0))]);
    let level = ed.level;
    let m = std::mem::replace(&mut ed.model, Model::open(":memory:").unwrap());
    let (items, host) = clash::host_bodies(m).unwrap();
    assert_eq!(items, vec![(Sel::Wall(host_wall), level)]);
    let rows = clash::run(&items, &host, &d.elems, &Transform::default());
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].sel, rows[0].kind), (Sel::Wall(host_wall), "wall"));
    assert!(rows[0].min[0] < 2.5 && rows[0].max[0] > 2.5);
    let far = Transform {
        dx: 100.0,
        ..Default::default()
    };
    assert!(clash::run(&items, &host, &d.elems, &far).is_empty());
    // a 90 degree rotation lays the link wall along the host wall: an overlap, also a clash
    let along = Transform {
        dx: 2.5,
        dy: -2.5,
        rot: std::f64::consts::FRAC_PI_2,
        ..Default::default()
    };
    let p = along.apply([2.5, 2.0, 0.0]);
    assert!((p[0] - 0.5).abs() < 1e-9 && (p[1] - 0.0).abs() < 1e-9);
    assert_eq!(clash::run(&items, &host, &d.elems, &along).len(), 1);
}

#[test]
fn links_follow_model_undo_and_snaps_append() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    walls(&mut a.ed, &[((0.0, 0.0), (5.0, 0.0))]);
    let ctx = egui::Context::default();
    apply(
        &mut State::default(),
        &mut a,
        Act::Add("missing/site.ifc".into()),
    );
    let mut st = std::mem::take(&mut a.links);
    sync(&mut st, &mut a, &ctx);
    assert_eq!(st.list.len(), 1);
    assert_eq!(st.list[0].name, "site");
    // the load starts in the background (no file: the error arrives in poll, the UI does not wait)
    let id = st.list[0].id;
    assert!(st.jobs.contains_key(&id) || st.errors.contains_key(&id));
    // link data without a file: a hand-placed wall
    st.jobs.clear();
    st.loaded
        .insert(id, Loaded::new(link_model(&[((0.0, 3.0), (5.0, 3.0))])));
    sync(&mut st, &mut a, &ctx);
    let n = a.ed.walls.len();
    assert_eq!(a.ed.tool.walls.len(), n, "host snaps untouched");
    assert!(
        !a.ed.tool.link_walls.is_empty(),
        "link snaps in their own list"
    );
    assert!((a.ed.tool.link_walls[0].a.y - 3.0).abs() < 1e-9);
    // an editor reload does not overwrite link snaps
    a.ed.reload().unwrap();
    assert!(!a.ed.tool.link_walls.is_empty());
    a.links = st;
    let sc = attach(&a.links, &a.ed, ddc_render::Scene::new(vec![]));
    assert!(sc.meshes.is_empty(), "no link geometry in host meshes");
    assert!(sc.instanced.iter().any(|g| g.proto.element == LINK_ELEMENT));
    // hiding is one operation; undo restores visibility
    let mut st = std::mem::take(&mut a.links);
    apply(
        &mut st,
        &mut a,
        Act::Update(id, Transform::default(), false),
    );
    a.links = st;
    assert!(groups(&a.links, &a.ed).is_empty());
    a.ed.undo();
    assert!(a.ed.model.link(id).unwrap().unwrap().visible);
    // undoing an add removes the link and its data
    a.ed.undo();
    let mut st = std::mem::take(&mut a.links);
    sync(&mut st, &mut a, &ctx);
    assert!(st.list.is_empty() && st.loaded.is_empty());
    assert!(a.ed.tool.link_walls.is_empty());
}

#[test]
fn reload_all_runs_at_most_cores_minus_one_loads_at_once() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    let ctx = egui::Context::default();
    let dir = std::env::temp_dir().join(format!("ddc-links-queue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut st = std::mem::take(&mut a.links);
    for i in 0..5 {
        // the files exist, so the loads stay running for a moment instead of failing at once
        let p = dir.join(format!("l{i}.ifc"));
        std::fs::write(&p, b"ISO-10303-21;").unwrap();
        apply(&mut st, &mut a, Act::Add(p.to_string_lossy().into_owned()));
    }
    sync(&mut st, &mut a, &ctx);
    assert_eq!(st.list.len(), 5);
    assert!(st.jobs.len() <= max_loads() && max_loads() >= 1);
    let (d, n) = st.progress().expect("loads pending");
    assert!(d <= n && n <= 5);
    assert_eq!(
        st.jobs.len() + st.queue.len() + st.loaded.len() + st.errors.len(),
        5
    );
    a.links = st;
}

fn far_wall_app(other_level: bool) -> (EditorApp, Sel, i64) {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    let home = a.ed.level;
    walls(&mut a.ed, &[((0.0, 0.0), (5.0, 0.0))]);
    let mut level = home;
    if other_level {
        level = a.ed.model.add_level("L2", 3.0).unwrap();
        a.ed.set_level(level);
    }
    walls(&mut a.ed, &[((100.0, 40.0), (120.0, 40.0))]);
    let id = a.ed.walls.last().unwrap().id;
    a.ed.set_level(home);
    (a, Sel::Wall(id), level)
}

#[test]
fn clash_row_on_current_level_frames_the_element() {
    let (mut a, sel, level) = far_wall_app(false);
    a.view3d = false;
    a.plan.center = v2(0.0, 0.0);
    apply(&mut State::default(), &mut a, Act::Select(sel, level));
    assert_eq!(a.ed.selection, vec![sel]);
    assert!((a.plan.center.x - 110.0).abs() < 1.0 && (a.plan.center.y - 40.0).abs() < 1.0);
}

const RU: Loc = Loc { ru: true, mark: DecimalMark::Comma };
const EN: Loc = Loc { ru: false, mark: DecimalMark::Point };

fn buf(a: &str, b: &str, c: &str, d: &str) -> [String; 4] {
    [a, b, c, d].map(String::from)
}

#[test]
fn offsets_round_trip_in_both_marks_and_units() {
    let xf = Transform { dx: 1.5, dy: -2.25, dz: 3.81, rot: 0.5 };
    for units in [Units::Metric, Units::Imperial] {
        for loc in [RU, EN] {
            let back = parse_fields(&fields(&xf, units, loc), units, loc).expect("own text reads back");
            assert!((back.dx - xf.dx).abs() < 2e-3 && (back.dy - xf.dy).abs() < 2e-3, "{units:?}");
            assert!((back.dz - xf.dz).abs() < 2e-3, "{units:?}");
            assert!((back.rot - xf.rot).abs() < 1e-3, "{units:?}");
        }
    }
    // feet and inches survive exactly: 12 ft 6 in
    let ft = Transform { dz: 3.81, ..Default::default() };
    let back = parse_fields(&fields(&ft, Units::Imperial, RU), Units::Imperial, RU).unwrap();
    assert!((back.dz - 3.81).abs() < 1e-6);
}

#[test]
fn ru_reads_comma_and_en_refuses_it() {
    let ok = parse_fields(&buf("1,5 m", "0", "0", "30,5"), Units::Metric, RU).unwrap();
    assert!((ok.dx - 1.5).abs() < 1e-9 && (ok.rot - 30.5f64.to_radians()).abs() < 1e-9);
    assert!(parse_fields(&buf("1,5", "0", "0", "0"), Units::Metric, EN).is_none());
    assert!(parse_fields(&buf("1.5 m", "0", "0", "30,5"), Units::Metric, EN).is_none());
    assert!(parse_fields(&buf("1.5 m", "0", "0", "30.5"), Units::Metric, EN).is_some());
}

#[test]
fn refused_text_writes_nothing_and_apply_is_one_undo() {
    assert!(parse_fields(&buf("abc", "0", "0", "0"), Units::Metric, RU).is_none());
    assert!(parse_fields(&buf("1", "0", "0", "x"), Units::Metric, RU).is_none());
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    apply(&mut State::default(), &mut a, Act::Add("missing/site.ifc".into()));
    let id = a.ed.model.links().unwrap()[0].id;
    let xf = parse_fields(&buf("1,5 m", "2 m", "0", "90"), Units::Metric, RU).unwrap();
    apply(&mut State::default(), &mut a, Act::Update(id, xf, true));
    assert_eq!(a.ed.model.link(id).unwrap().unwrap().xf, xf);
    a.ed.undo();
    assert_eq!(a.ed.model.link(id).unwrap().unwrap().xf, Transform::default());
}

#[test]
fn refused_rotation_with_valid_offsets_disables_apply_and_writes_nothing() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    apply(&mut State::default(), &mut a, Act::Add("missing/site.ifc".into()));
    let id = a.ed.model.links().unwrap()[0].id;
    let base = a.ed.model.link(id).unwrap().unwrap().xf;
    // offsets read fine, the rotation text does not
    assert!(parse_fields(&buf("1,5 m", "2 m", "0", "9o"), Units::Metric, RU).is_none());
    let draft = Transform { dx: 1.5, dy: 2.0, ..base };
    assert!(can_apply(false, &draft, &base));
    assert!(!can_apply(true, &draft, &base), "refused text holds Apply back");
    assert_eq!(a.ed.model.link(id).unwrap().unwrap().xf, base);
}

fn font_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    ddc_ui::theme::install_fonts(&ctx);
    let _ = ctx.run(Default::default(), |_| {});
    ctx
}

#[test]
fn offset_fields_render_headless_in_both_languages() {
    for loc in [RU, EN] {
        for units in [Units::Metric, Units::Imperial] {
            let ctx = font_ctx();
            let mut draft = Transform { dx: 1.5, dy: 0.0, dz: 3.81, rot: 0.3 };
            let before = draft;
            for _ in 0..2 {
                let _ = ctx.run(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let th = ddc_ui::theme::of(ctx);
                        let refused = offset_fields(ui, &th, units, loc, 7, &mut draft);
                        assert!(!refused);
                    });
                });
            }
            assert_eq!(draft, before, "an untouched field commits nothing");
        }
    }
}

#[test]
fn window_uses_kit_widgets_only() {
    let src = include_str!("../links_ui.rs");
    for raw in ["DragValue", "ui.checkbox(", "ui.button(", "egui::Button::new(", "format!(\"{:.3}\""] {
        assert!(!src.contains(raw), "raw widget left: {raw}");
    }
    assert!(src.contains("len_field(") && src.contains("number_field("));
    for raw in ["TextEdit", "TextField", "desired_width(", "PATH_RESERVE", "560", "empty_note("] {
        assert!(!src.contains(raw), "raw widget or literal size left: {raw}");
    }
    assert!(src.contains("empty_state(") && src.contains("pinned_footer("));
    assert!(src.contains("window_size("));
    assert_eq!(window_size(true), kit::DialogSize::Form);
    assert_eq!(window_size(false), kit::DialogSize::Tool);
}

#[test]
fn empty_state_renders_in_both_languages_and_idle_frames_write_nothing() {
    for ru in [true, false] {
        let ctx = font_ctx();
        let a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        let mut st = State::default();
        let mut clicked = false;
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let th = ddc_ui::theme::of(ctx);
                    clicked |= intro(ui, &th, &st.refused, &mut st.icons, ru, true);
                });
            });
        }
        assert!(!clicked, "an idle empty state opens no picker");
        assert!(st.refused.is_none());
        assert!(a.ed.model.links().unwrap().is_empty());
        for k in ["links.empty", "links.empty_hint", "links.add_pick", "links.format_bad"] {
            let s = crate::tr_lang(ru, k, &[]);
            assert!(!s.is_empty() && s != k, "{k} missing for ru={ru}");
        }
    }
    assert!(!crate::tr_lang(true, "links.add_pick", &[]).is_ascii());
}

#[test]
fn one_primary_and_the_path_still_adds_the_same_link() {
    let mut refused = None;
    // a picked known file is linked at once, a refused one is only named in the notice
    assert!(picked(&mut refused, None).is_none() && refused.is_none());
    assert!(picked(&mut refused, Some("dir/notes.txt".into())).is_none());
    assert_eq!(refused.as_deref(), Some("dir/notes.txt"));
    for ru in [false, true] {
        let note = refused_note("dir/notes.txt", ru);
        assert!(note.starts_with("notes.txt: ") && note.len() > 12);
    }
    let act = picked(&mut refused, Some("missing/site.ifc".into()));
    assert!(matches!(&act, Some(Act::Add(p)) if p == "missing/site.ifc"));
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    let mut st = State { refused, ..State::default() };
    apply(&mut st, &mut a, act.unwrap());
    assert!(st.refused.is_none(), "a successful add clears the notice");
    let links = a.ed.model.links().unwrap();
    assert_eq!(links.len(), 1);
    assert!(links[0].path.ends_with("site.ifc"));
    assert_eq!(links[0].xf, Transform::default());
}

#[test]
fn clash_row_on_another_level_switches_and_frames() {
    let (mut a, sel, level) = far_wall_app(true);
    assert_ne!(a.ed.level, level);
    a.view3d = false;
    a.plan.center = v2(0.0, 0.0);
    apply(&mut State::default(), &mut a, Act::Select(sel, level));
    assert_eq!(a.ed.level, level);
    assert_eq!(a.ed.selection, vec![sel]);
    assert!((a.plan.center.x - 110.0).abs() < 1.0 && (a.plan.center.y - 40.0).abs() < 1.0);
}

/// The read-only subtitle uses the secondary text token: the faded `weak` text was hard to
/// read on the dialog body (b16 shot).
#[test]
fn read_only_note_uses_the_secondary_text_token() {
    let src = include_str!("../links_ui.rs");
    let body = src.split("fn intro(").nth(1).expect("intro");
    let body = &body[..body.find("\n}\n").expect("end of intro")];
    assert!(!body.contains("ui.weak("), "weak text for the read-only note");
    assert!(body.contains("text_secondary"));
}
