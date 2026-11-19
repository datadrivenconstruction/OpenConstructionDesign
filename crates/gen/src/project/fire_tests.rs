//! Unit tests of the hazard classes, the head grid kept off the columns and the coverage test.

use super::fire_check::*;
use super::rules::{def, rule, scoped, Layer, RuleSet};
use super::sprinkler::{bbox, grid_for, placeable, P2};
use super::RoomKind;
use crate::GColumn;

fn rect(w: f64, h: f64) -> Vec<P2> {
    vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]
}

fn heads(poly: &[P2], (xs, ys): (Vec<f64>, Vec<f64>)) -> Vec<P2> {
    ys.iter().flat_map(|y| xs.iter().map(move |x| (*x, *y))).filter(|q| placeable(poly, *q)).collect()
}

/// Every class reads its own rules, the ordinary class the base rules, and a room kind reads
/// its class from its rule, which a project may change.
#[test]
fn hazard_classes_read_their_rules() {
    let (l, o, h) = (Hazard::class(0), Hazard::class(ORDINARY), Hazard::class(2));
    assert_eq!((l.spacing, l.area, l.wall, l.density), (rule("fire.light.spacing"), rule("fire.light.head_area"), rule("fire.light.wall_max"), rule("fire.light.density")));
    assert_eq!((o.spacing, o.area, o.wall, o.density), (rule("fire.head_spacing"), rule("fire.head_area"), rule("fire.head_wall_max"), rule("fire.density")));
    assert_eq!((h.spacing, h.area, h.wall, h.density), (rule("fire.high.spacing"), rule("fire.high.head_area"), rule("fire.high.wall_max"), rule("fire.high.density")));
    // heavier classes put heads closer and give more water per square metre
    assert!(l.area > o.area && o.area > h.area && l.density < o.density && o.density < h.density);
    assert_eq!(hazard_of(RoomKind::Office).name(), "light");
    assert_eq!(hazard_of(RoomKind::Parking).name(), "ordinary");
    assert_eq!(hazard_of(RoomKind::Stair).name(), "ordinary", "a kind without a rule is ordinary");
    for k in ["office", "parking", "storage", "retail", "workshop", "living"] {
        let d = def(&format!("fire.hazard.{k}")).unwrap();
        assert_eq!(d.options, CLASSES.map(str::to_string).to_vec(), "{k}");
    }
    let mut rs = RuleSet::new();
    rs.set(Layer::Project, "fire.hazard.office", 2.0).unwrap();
    assert_eq!(scoped(&rs, || hazard_of(RoomKind::Office)).name(), "high");
}

/// The grid of each class keeps its spacing, area per head and wall distance, and a heavier
/// class needs more heads over the same floor.
#[test]
fn each_class_grid_keeps_its_limits() {
    let room = rect(17.0, 11.0);
    let mut counts = vec![];
    for i in 0..CLASSES.len() {
        let hz = Hazard::class(i);
        let (xs, ys) = grid_for(&bbox(&room), &hz);
        let pitch = |v: &[f64]| v.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
        assert!(pitch(&xs) <= hz.spacing + 1e-9 && pitch(&ys) <= hz.spacing + 1e-9, "{}", hz.name());
        assert!(xs[0] <= hz.wall + 1e-9 && ys[0] <= hz.wall + 1e-9, "{}", hz.name());
        let hs = heads(&room, (xs, ys));
        assert!(17.0 * 11.0 / hs.len() as f64 <= hz.area + 1e-9, "{}", hz.name());
        let (share, gap) = coverage_sq(&room, &hs, &hz);
        assert!(share == 1.0 && gap.is_none(), "{} {share} {gap:?}", hz.name());
        counts.push(hs.len());
    }
    assert!(counts[0] < counts[1] && counts[1] < counts[2], "{counts:?}");
}

/// A column on a head row moves the row past it, or makes the rows denser, and the room
/// stays covered with no head nor row against the column.
#[test]
fn rows_keep_off_the_columns() {
    let room = rect(13.6, 10.2);
    let hz = Hazard::class(ORDINARY);
    let (_, ys0) = grid_for(&bbox(&room), &hz);
    let col = GColumn { at: (6.0, ys0[1]), rotation: 0.0, section: (0.4, 0.4) };
    let (xs, ys) = grid_clear(&room, &hz, std::slice::from_ref(&col));
    let pad = rule("fire.column_clear") + 0.2;
    assert!(ys.iter().all(|y| (y - col.at.1).abs() >= pad - 1e-9), "{ys:?}");
    let hs: Vec<P2> = heads(&room, (xs, ys)).into_iter().filter_map(|q| clear_of_columns(std::slice::from_ref(&col), q)).collect();
    let (share, gap) = coverage_sq(&room, &hs, &hz);
    assert!(share == 1.0 && gap.is_none(), "{share} {gap:?}");
    // with no column the grid is the plain one
    assert_eq!(grid_clear(&room, &hz, &[]).1, ys0);
}

/// The coverage test finds the corner a missing head leaves.
#[test]
fn coverage_finds_a_missing_head() {
    let room = rect(10.0, 7.0);
    let hz = Hazard::class(0);
    let hs = heads(&room, grid_for(&bbox(&room), &hz));
    assert_eq!(coverage_sq(&room, &hs, &hz).0, 1.0);
    let fewer: Vec<P2> = hs.iter().skip(1).copied().collect();
    let (share, gap) = coverage_sq(&room, &fewer, &hz);
    assert!(share < 1.0 && gap.is_some_and(|g| g.0 < 5.0 && g.1 < 3.5), "{share} {gap:?}");
}
