use std::f64::consts::{FRAC_PI_2, PI};

use ddc_geom::v2;
use ddc_io::drawing_model::{Pen, UArc, UFill, ULayer, ULine, UText, Underlay};
use ddc_model::{Anno, AnnoShape, AnnoView, RegionFill, TextAlign};

use super::*;
use crate::drawing::{Drawings, Origin};

const EPS: f64 = 1e-9;

fn layer(name: &str, off: bool) -> ULayer {
    ULayer {
        name: name.into(),
        color: None,
        off,
        frozen: false,
        linetype: String::new(),
        weight_mm: 0.25,
    }
}

fn seg(layer: &str, a: P, b: P, pen: u32) -> ULine {
    ULine {
        layer: layer.into(),
        a,
        b,
        dashed: false,
        pen,
    }
}

fn text(s: &str, pos: P, height: f64, deg: f64, attach: u8) -> UText {
    UText {
        layer: "T".into(),
        pos,
        height,
        rotation: deg.to_radians(),
        text: s.into(),
        pen: 0,
        fmt: ddc_io::drawing_model::TextFmt {
            mtext: true,
            attach,
            ..Default::default()
        },
    }
}

fn setup(u: Underlay) -> (Editor, Drawings) {
    let ed = Editor::new_in_memory().unwrap();
    let mut ds = Drawings::default();
    ds.add_underlay(u, 1.0, "d", None, ed.level, Origin::AsIs, v2(0.0, 0.0));
    (ed, ds)
}

fn view(ed: &Editor) -> AnnoView {
    AnnoView::Plan(ed.level)
}

fn annos(ed: &Editor) -> Vec<Anno> {
    ed.model.annos(view(ed)).unwrap()
}

fn scale(ed: &Editor) -> f64 {
    ed.model.anno_scale(view(ed)).unwrap()
}

fn polylines(ed: &Editor) -> Vec<(Vec<P>, bool, Anno)> {
    annos(ed)
        .into_iter()
        .filter_map(|a| match &a.shape {
            AnnoShape::Line { pts, closed } => Some((pts.clone(), *closed, a.clone())),
            _ => None,
        })
        .collect()
}

fn notes(ed: &Editor) -> Vec<(TextNote, Anno)> {
    annos(ed)
        .into_iter()
        .filter_map(|a| match &a.shape {
            AnnoShape::Text(t) => Some((t.clone(), a.clone())),
            _ => None,
        })
        .collect()
}

fn close(a: P, b: P) -> bool {
    (a.0 - b.0).abs() <= EPS && (a.1 - b.1).abs() <= EPS
}

fn line_plan() -> Underlay {
    let mut u = Underlay::default();
    u.layers = vec![layer("A", false), layer("B", false), layer("HID", true)];
    u.pens = vec![
        Pen {
            color: None,
            weight_mm: 0.5,
            dash: vec![0.2, -0.1],
        },
        Pen {
            color: None,
            weight_mm: 0.25,
            dash: vec![],
        },
    ];
    // open polyline of three segments
    u.lines.push(seg("A", (0.0, 0.0), (1.0, 0.0), 0));
    u.lines.push(seg("A", (1.0, 0.0), (1.0, 1.0), 0));
    u.lines.push(seg("A", (1.0, 1.0), (2.0, 1.0), 0));
    // closed square
    u.lines.push(seg("B", (5.0, 0.0), (6.0, 0.0), 1));
    u.lines.push(seg("B", (6.0, 0.0), (6.0, 1.0), 1));
    u.lines.push(seg("B", (6.0, 1.0), (5.0, 1.0), 1));
    u.lines.push(seg("B", (5.0, 1.0), (5.0, 0.0), 1));
    // a single line that does not start where the square ends
    u.lines.push(seg("B", (0.0, 5.0), (3.0, 5.0), 1));
    // on a layer turned off in the file
    u.lines.push(seg("HID", (10.0, 10.0), (11.0, 10.0), 1));
    u.arcs.push(UArc {
        layer: "B".into(),
        c: (20.0, 0.0),
        r: 2.0,
        a0: 0.0,
        a1: FRAC_PI_2,
        pen: 1,
    });
    u.arcs.push(UArc {
        layer: "B".into(),
        c: (30.0, 0.0),
        r: 0.5,
        a0: 0.0,
        a1: 2.0 * PI,
        pen: 1,
    });
    u
}

#[test]
fn lines_become_detail_lines() {
    let (mut ed, ds) = setup(line_plan());
    let k = scale(&ed);
    let msg = ds.to_detail(&mut ed);
    let got = polylines(&ed);
    assert_eq!(got.len(), 5, "{msg}: {got:?}");
    let find = |p: P| got.iter().find(|g| close(g.0[0], p)).expect("line").clone();

    let (pts, closed, a) = find((0.0, 0.0));
    assert_eq!(pts, vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (2.0, 1.0)]);
    assert!(!closed);
    assert_eq!(a.line.weight_mm, 0.5);
    assert_eq!(a.line.dash, sheet_dash(&[0.2, -0.1], k));
    assert_eq!(a.line.dash.len(), 2);
    assert!((a.line.dash[0] - 0.2 * 1000.0 / k).abs() <= 1e-3);
    assert!((a.line.dash[1] - 0.1 * 1000.0 / k).abs() <= 1e-3);

    let (pts, closed, a) = find((5.0, 0.0));
    assert_eq!(pts, vec![(5.0, 0.0), (6.0, 0.0), (6.0, 1.0), (5.0, 1.0)]);
    assert!(closed);
    assert_eq!(a.line.weight_mm, 0.25);
    assert!(a.line.dash.is_empty());

    let (pts, closed, _) = find((0.0, 5.0));
    assert_eq!(pts, vec![(0.0, 5.0), (3.0, 5.0)]);
    assert!(!closed);

    // quarter arc: exact ends, every point on the arc, chords within the tolerance
    let (pts, closed, _) = find((22.0, 0.0));
    assert!(!closed);
    assert!(close(*pts.last().unwrap(), (20.0, 2.0)));
    for p in &pts {
        assert!(((p.0 - 20.0).hypot(p.1) - 2.0).abs() <= 1e-12);
    }
    for w in pts.windows(2) {
        let m = ((w[0].0 + w[1].0) / 2.0 - 20.0, (w[0].1 + w[1].1) / 2.0);
        assert!(2.0 - m.0.hypot(m.1) <= ARC_SAG + 1e-12);
    }

    let (pts, closed, _) = find((30.5, 0.0));
    assert!(closed && pts.len() >= 8);
    assert!(!close(pts[0], *pts.last().unwrap()));

    assert!(!got.iter().any(|g| g.0.iter().any(|p| close(*p, (10.0, 10.0)))));
}

#[test]
fn dash_patterns_become_sheet_dashes() {
    assert_eq!(sheet_dash(&[0.2, -0.1], 100.0), vec![2.0, 1.0]);
    // a dot stays visible; a pattern starting with a gap starts with its dash
    assert_eq!(
        sheet_dash(&[0.2, -0.1, 0.0, -0.1], 100.0),
        vec![2.0, 1.0, DOT_MM, 1.0]
    );
    assert_eq!(sheet_dash(&[-0.1, 0.2], 100.0), vec![2.0, 1.0]);
    assert_eq!(sheet_dash(&[0.2, 0.1], 100.0), Vec::<f64>::new());
    assert_eq!(sheet_dash(&[], 100.0), Vec::<f64>::new());
}

#[test]
fn text_keeps_height_and_rotation() {
    let mut u = Underlay::default();
    u.layers = vec![layer("T", false)];
    u.texts = vec![
        text("Room 101", (2.0, 3.0), 0.25, 30.0, 7),
        text("A\nB", (5.0, 5.0), 0.5, 0.0, 5),
        text("R", (8.0, 1.0), 0.3, 90.0, 3),
        text("  ", (9.0, 9.0), 0.3, 0.0, 7),
    ];
    let (mut ed, ds) = setup(u);
    let k = scale(&ed);
    ds.to_detail(&mut ed);
    let got = notes(&ed);
    assert_eq!(got.len(), 3, "an empty text is skipped");
    let by = |s: &str| got.iter().find(|n| n.0.text == s).expect("note").clone();

    // baseline text: the note's top is one height above the baseline, across the rotation
    let (n, a) = by("Room 101");
    let (s, c) = 30f64.to_radians().sin_cos();
    assert!(close(n.pos, (2.0 - s * 0.25, 3.0 + c * 0.25)), "{:?}", n.pos);
    assert!((n.rot_deg - 30.0).abs() <= EPS);
    assert_eq!(n.align, TextAlign::Left);
    assert!((a.text.height_mm * k / 1000.0 - 0.25).abs() <= 1e-12);

    // middle center over two lines: the block's middle stays on the anchor
    let (n, a) = by("A\nB");
    assert!(close(n.pos, (5.0, 5.0 + 0.5 * (1.0 + PITCH) / 2.0)), "{:?}", n.pos);
    assert_eq!(n.align, TextAlign::Center);
    assert!(n.rot_deg.abs() <= EPS);
    assert!((a.text.height_mm * k / 1000.0 - 0.5).abs() <= 1e-12);

    let (n, a) = by("R");
    assert!(close(n.pos, (8.0, 1.0)));
    assert_eq!(n.align, TextAlign::Right);
    assert!((n.rot_deg - 90.0).abs() <= EPS);
    assert!((a.text.height_mm * k / 1000.0 - 0.3).abs() <= 1e-12);
}

fn square(x: f64, y: f64, w: f64) -> Vec<P> {
    vec![(x, y), (x + w, y), (x + w, y + w), (x, y + w)]
}

fn hatch_plan() -> Underlay {
    let mut u = Underlay::default();
    u.layers = vec![layer("H", false)];
    u.pens = vec![
        Pen::default(),
        Pen {
            color: Some([255, 0, 0]),
            weight_mm: 0.25,
            dash: vec![],
        },
    ];
    // 45 degree lines 0.2 m apart, some split in two dashes
    let d = 0.2 * 2f64.sqrt();
    let mut segs = vec![];
    for i in -40..=40 {
        let o = i as f64 * d;
        segs.push(((0.0, o), (3.0, o + 3.0)));
        if i % 3 == 0 {
            segs.push(((5.0, o + 5.0), (7.0, o + 7.0)));
        }
    }
    let mut outer = square(0.0, 0.0, 10.0);
    outer.push((0.0, 0.0));
    u.fills.push(UFill {
        layer: "H".into(),
        loops: vec![outer, square(4.0, 4.0, 2.0)],
        solid: false,
        pattern: "ANSI31".into(),
        segs,
        pen: 0,
    });
    u.fills.push(UFill {
        layer: "H".into(),
        loops: vec![vec![(20.0, 0.0), (22.0, 0.0), (21.0, 2.0)]],
        solid: true,
        pattern: "SOLID".into(),
        segs: vec![],
        pen: 1,
    });
    let mut cross = vec![];
    for i in 0..10 {
        let o = i as f64 * 0.5;
        cross.push(((30.0, o), (35.0, o)));
        cross.push(((30.0 + o, 0.0), (30.0 + o, 5.0)));
    }
    u.fills.push(UFill {
        layer: "H".into(),
        loops: vec![square(30.0, 0.0, 5.0)],
        solid: false,
        pattern: "NET".into(),
        segs: cross,
        pen: 0,
    });
    // not picked: stays out
    u.lines.push(seg("H", (0.0, 20.0), (1.0, 20.0), 0));
    u
}

#[test]
fn hatch_becomes_region_with_holes() {
    let (mut ed, mut ds) = setup(hatch_plan());
    let k = scale(&ed);
    ds.pick = Some((
        0,
        Pick {
            fills: vec![0, 1, 2],
            ..Default::default()
        },
    ));
    ds.to_detail(&mut ed);
    assert!(polylines(&ed).is_empty(), "only the picked fills");
    let regions: Vec<(Vec<Vec<P>>, RegionFill, bool)> = annos(&ed)
        .into_iter()
        .filter_map(|a| match a.shape {
            AnnoShape::Region {
                rings,
                fill,
                boundary,
            } => Some((rings, fill, boundary)),
            _ => None,
        })
        .collect();
    assert_eq!(regions.len(), 3);
    let at = |p: P| {
        regions
            .iter()
            .find(|r| close(r.0[0][0], p))
            .expect("region")
            .clone()
    };

    let (rings, fill, boundary) = at((0.0, 0.0));
    assert_eq!(rings.len(), 2, "outer contour and one hole");
    assert_eq!(rings[0], square(0.0, 0.0, 10.0), "closing point dropped");
    assert_eq!(rings[1], square(4.0, 4.0, 2.0));
    assert!(!boundary);
    let RegionFill::Hatch {
        deg,
        step_mm,
        cross,
    } = fill
    else {
        panic!("hatch expected: {fill:?}");
    };
    assert!((deg - 45.0).abs() <= 1e-9, "{deg}");
    assert!((step_mm - 0.2 * 1000.0 / k).abs() <= 1e-6, "{step_mm}");
    assert!(!cross);

    let (rings, fill, _) = at((20.0, 0.0));
    assert_eq!(rings.len(), 1);
    assert_eq!(fill, RegionFill::Solid { gray: 76 });

    let (_, fill, _) = at((30.0, 0.0));
    let RegionFill::Hatch {
        deg,
        step_mm,
        cross,
    } = fill
    else {
        panic!("hatch expected: {fill:?}");
    };
    assert!(deg.abs() <= 1e-9 && cross, "{deg} {cross}");
    assert!((step_mm - 0.5 * 1000.0 / k).abs() <= 1e-6, "{step_mm}");
}

fn mixed_plan() -> Underlay {
    let mut u = line_plan();
    u.layers.push(layer("T", false));
    u.layers.push(layer("H", false));
    u.texts.push(text("Note", (1.0, 1.0), 0.35, 15.0, 1));
    u.fills.push(hatch_plan().fills.remove(0));
    u
}

#[test]
fn to_detail_one_undo() {
    let (mut ed, ds) = setup(mixed_plan());
    let steps = ed.undo_len();
    let (nt, nl) = (
        ed.model.text_styles().unwrap().len(),
        ed.model.line_styles().unwrap().len(),
    );
    ds.to_detail(&mut ed);
    assert_eq!(ed.undo_len(), steps + 1, "one action, one step");
    let after = annos(&ed);
    // 5 lines, one note, one region
    assert_eq!(after.len(), 7);
    assert!(ed.model.line_styles().unwrap().len() > nl, "new pens got styles");
    for _ in 0..2 {
        ed.undo();
        assert!(annos(&ed).is_empty());
        assert_eq!(ed.model.text_styles().unwrap().len(), nt);
        assert_eq!(ed.model.line_styles().unwrap().len(), nl);
        assert_eq!(ed.undo_len(), steps);
        ed.redo();
        let again = annos(&ed);
        assert_eq!(again.len(), after.len());
        assert!(again.iter().zip(&after).all(|(a, b)| a.shape == b.shape));
    }
}

#[test]
fn nothing_to_convert_adds_no_step() {
    let mut u = Underlay::default();
    u.layers = vec![layer("HID", true)];
    u.lines.push(seg("HID", (0.0, 0.0), (1.0, 0.0), 0));
    let (mut ed, ds) = setup(u);
    let steps = ed.undo_len();
    ds.to_detail(&mut ed);
    assert_eq!(ed.undo_len(), steps);
    assert!(annos(&ed).is_empty());

    let mut ed2 = Editor::new_in_memory().unwrap();
    let steps = ed2.undo_len();
    Drawings::default().to_detail(&mut ed2);
    assert_eq!(ed2.undo_len(), steps);
}

#[test]
fn drawing_unchanged_after_to_detail() {
    let (mut ed, mut ds) = setup(mixed_plan());
    let snap = |ds: &Drawings| {
        let d = &ds.list[0];
        (
            d.u.clone(),
            d.offset,
            d.rot,
            d.dirty,
            d.layers.clone(),
            ds.version,
            ds.pick.clone(),
            ds.steps.len(),
        )
    };
    let before = snap(&ds);
    ds.to_detail(&mut ed);
    assert!(!annos(&ed).is_empty());
    assert_eq!(snap(&ds), before, "whole drawing");

    ds.pick = Some((
        0,
        Pick {
            lines: vec![0, 1],
            texts: vec![0],
            ..Default::default()
        },
    ));
    let before = snap(&ds);
    ds.to_detail(&mut ed);
    assert_eq!(snap(&ds), before, "picked primitives");
}

fn add_section(ed: &mut Editor) -> i64 {
    ed.model
        .add_view_def(&ddc_model::SectionSpec {
            kind: ddc_model::ViewKind::Section,
            name: "1-1".into(),
            a: (3.0, -1.0),
            b: (3.0, 5.0),
            depth: 10.0,
            far_clip: false,
        })
        .unwrap()
}

#[test]
fn to_detail_in_section_lands_on_section() {
    let (mut ed, ds) = setup(mixed_plan());
    let sec = add_section(&mut ed);
    let steps = ed.undo_len();
    ds.to_detail_in(&mut ed, ViewKey::Section(sec));
    assert_eq!(ed.undo_len(), steps + 1);
    assert_eq!(ed.model.annos(AnnoView::Section(sec)).unwrap().len(), 7);
    assert!(annos(&ed).is_empty(), "nothing leaks onto the plan");
}

#[test]
fn to_detail_in_3d_refused() {
    let (mut ed, ds) = setup(mixed_plan());
    let steps = ed.undo_len();
    let msg = ds.to_detail_in(&mut ed, ViewKey::ThreeD);
    assert!(is_msg(&msg, "detail_items.st.bad_view"), "{msg}");
    assert_eq!(ed.undo_len(), steps, "no step");
    assert!(annos(&ed).is_empty());
}

#[test]
fn to_detail_reload_fail_is_warning_one_undo() {
    let (mut ed, ds) = setup(mixed_plan());
    ed.model
        .sql()
        .execute_batch("ALTER TABLE wall_type_layer RENAME TO zz_layer")
        .unwrap();
    let steps = ed.undo_len();
    let msg = ds.to_detail(&mut ed);
    assert_eq!(ed.undo_len(), steps + 1, "the committed step is on the stack");
    assert_eq!(annos(&ed).len(), 7);
    // either UI language: the status starts like the warning, not like the failure
    let head = |ru: bool, key: &str, args: &[(&str, &str)]| {
        let s = crate::tr_lang(ru, key, args);
        s.split("@@").next().unwrap_or_default().to_string()
    };
    let counts = [("lines", "5"), ("texts", "1"), ("regions", "1"), ("e", "@@")];
    assert!(
        [false, true]
            .iter()
            .any(|&ru| msg.starts_with(&head(ru, "detail_items.st.warn", &counts))),
        "warning text: {msg}"
    );
    assert!(
        ![false, true]
            .iter()
            .any(|&ru| msg.starts_with(&head(ru, "detail_items.st.failed", &[("e", "@@")]))),
        "not shown as a failure: {msg}"
    );
}

/// The status is the catalog string `key` in either UI language.
fn is_msg(msg: &str, key: &str) -> bool {
    [false, true]
        .iter()
        .any(|&ru| msg == crate::tr_lang(ru, key, &[]))
}

#[test]
fn empty_selection_is_refused_without_a_step() {
    let (mut ed, mut ds) = setup(mixed_plan());
    ds.pick = Some((0, Pick::default()));
    let steps = ed.undo_len();
    let msg = ds.to_detail(&mut ed);
    assert!(is_msg(&msg, "detail_items.st.nothing"), "{msg}");
    assert_eq!(ed.undo_len(), steps, "refusal adds no undo step");
    assert!(annos(&ed).is_empty());
    // a pick of only out-of-range indices converts nothing either
    ds.pick = Some((
        0,
        Pick {
            lines: vec![999],
            ..Default::default()
        },
    ));
    assert!(is_msg(&ds.to_detail(&mut ed), "detail_items.st.nothing"));
    assert_eq!(ed.undo_len(), steps);
}

fn level_z(ed: &Editor) -> f64 {
    ed.levels().into_iter().find(|l| l.0 == ed.level).unwrap().2
}

#[test]
fn to_detail_in_section_maps_into_the_view_system() {
    let (mut ed, ds) = setup(mixed_plan());
    let sec = add_section(&mut ed);
    ds.to_detail_in(&mut ed, ViewKey::Section(sec));
    let z0 = level_z(&ed);
    let got = ed.model.annos(AnnoView::Section(sec)).unwrap();
    // the section runs north from (3, -1): x = distance along it, y = level + offset across it
    let want = [(1.0, 3.0 + z0), (1.0, 2.0 + z0), (2.0, 2.0 + z0), (2.0, 1.0 + z0)];
    let open = got
        .iter()
        .find_map(|a| match &a.shape {
            AnnoShape::Line { pts, closed: false } if pts.len() == 4 => Some(pts.clone()),
            _ => None,
        })
        .expect("the open polyline");
    for (p, w) in open.iter().zip(want) {
        assert!((p.0 - w.0).abs() < 1e-9 && (p.1 - w.1).abs() < 1e-9, "{open:?}");
    }
    // the text keeps its place relative to the line: plan (1, 1) -> (2, 2 + z0), turned by -90
    let note = got
        .iter()
        .find_map(|a| match &a.shape {
            AnnoShape::Text(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert!((note.pos.0 - 2.0).abs() < 1e-9 && (note.pos.1 - 2.0 - z0).abs() < 1e-9);
    assert!(annos(&ed).is_empty(), "nothing leaks onto the plan");
}

#[test]
fn to_detail_in_gone_section_refused_model_unchanged() {
    let (mut ed, ds) = setup(mixed_plan());
    let steps = ed.undo_len();
    let (nt, nl) = (
        ed.model.text_styles().unwrap().len(),
        ed.model.line_styles().unwrap().len(),
    );
    let msg = ds.to_detail_in(&mut ed, ViewKey::Section(987_654));
    assert_eq!(msg, crate::tr("detail_items.st.no_transform", &[]));
    assert_eq!(ed.undo_len(), steps, "no undo step");
    assert!(annos(&ed).is_empty());
    assert_eq!(ed.model.text_styles().unwrap().len(), nt);
    assert_eq!(ed.model.line_styles().unwrap().len(), nl);
}
