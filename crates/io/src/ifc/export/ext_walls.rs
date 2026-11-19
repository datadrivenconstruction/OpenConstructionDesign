//! Wall exterior flag: a wall is external when a point just off one face, at the middle of
//! the axis, lies outside every closed wall region of its level. An explicit
//! `Pset_WallCommon.IsExternal` property of the model wins over the computed value.

use std::collections::BTreeMap;

use ddc_geom::arc::edge_mid;
use ddc_geom::rooms::Region;
use ddc_geom::{v2, V2};
use ddc_model::Model;
use rusqlite::types::Value;

/// Probe distance beyond a wall face, metres.
const EPS: f64 = 0.02;

pub(super) const KEY: &str = ddc_model::WALL_IS_EXTERNAL_KEY;

/// Wall axis `a`-`b` (with `bulge`), layers spanning `offset0 .. offset0 + total` to the left.
pub(super) fn is_external(
    regions: &[Region],
    a: V2,
    b: V2,
    bulge: f64,
    offset0: f64,
    total: f64,
) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return false;
    }
    let n = v2(-dy / len, dx / len);
    let mid = edge_mid(a, b, bulge);
    [offset0 + total + EPS, offset0 - EPS].iter().any(|y| {
        let p = v2(mid.x + n.x * y, mid.y + n.y * y);
        !regions.iter().any(|g| g.contains(p))
    })
}

/// Set and name of the marker our export writes next to a computed flag: an imported
/// `IsExternal` equal to it is still computed, so later geometry edits keep updating it.
pub(super) const COMPUTED_SET: &str = "DDC_Computed";
pub(super) const COMPUTED_NAME: &str = "WallIsExternal";
pub(super) const COMPUTED: &str = ddc_model::WALL_COMPUTED_MARKER;

fn flag(v: &Value) -> Option<bool> {
    match v {
        Value::Integer(i) => Some(*i != 0),
        Value::Text(t) => match t.to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        Value::Real(x) => Some(*x != 0.0),
        _ => None,
    }
}

/// Explicit model value of the flag, if any (a value equal to the computed marker is not).
pub(super) fn explicit(model: &Model, wall: i64) -> Option<bool> {
    let p = model.props("wall", wall).ok()?;
    let get = |k: &str| p.iter().find(|x| x.0 == k).and_then(|x| flag(&x.1));
    let v = get(KEY)?;
    (get(COMPUTED) != Some(v)).then_some(v)
}

/// Wall of a level: id, axis ends, bulge, first layer offset, total thickness.
pub(super) type WallGeom = (i64, V2, V2, f64, f64, f64);

/// Final flag per wall id: explicit value, else computed.
pub(super) fn resolve(model: &Model, regions: &[Region], walls: &[WallGeom]) -> BTreeMap<i64, bool> {
    walls
        .iter()
        .map(|(id, a, b, bulge, o0, total)| {
            let v = explicit(model, *id)
                .unwrap_or_else(|| is_external(regions, *a, *b, *bulge, *o0, *total));
            (*id, v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::ifc::{export_ifc, import_ifc};
    use ddc_model::{LayerSpec, Model, RoomSpec, WallSpec};
    use rusqlite::types::Value;

    /// 6 x 4 box of 0.2 m walls, ids in order bottom, right, top, left; one room inside.
    fn box_model() -> (Model, Vec<i64>) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let pts = [(0., 0.), (6., 0.), (6., 4.), (0., 4.), (0., 0.)];
        let mut ids = vec![];
        for k in 0..4 {
            ids.push(
                m.add_wall(&WallSpec {
                    a: pts[k],
                    b: pts[k + 1],
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap(),
            );
        }
        m.add_room(&RoomSpec {
            level: lv,
            name: "R".into(),
            number: "1".into(),
            point: (3.0, 2.0),
            height: 3.0,
        })
        .unwrap();
        (m, ids)
    }

    fn add_partition(m: &mut Model) -> i64 {
        let lv: i64 = m
            .sql()
            .query_row("SELECT id FROM level", [], |x| x.get(0))
            .unwrap();
        let t: i64 = m
            .sql()
            .query_row("SELECT id FROM wall_type", [], |x| x.get(0))
            .unwrap();
        m.add_wall(&WallSpec {
            a: (3.0, 0.0),
            b: (3.0, 4.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
    }

    /// `IsExternal` values in file order (walls export in id order).
    fn flags_of(text: &str) -> Vec<bool> {
        text.lines()
            .filter(|l| l.contains("'IsExternal'"))
            .map(|l| l.contains("IFCBOOLEAN(.T.)"))
            .collect()
    }

    fn pset_count(text: &str) -> usize {
        text.matches("'Pset_WallCommon'").count()
    }

    #[test]
    fn box_plan_outer_walls_is_external_true_inner_false() {
        let (mut m, _) = box_model();
        add_partition(&mut m);
        let t = export_ifc(&m).unwrap();
        let f = flags_of(&t);
        assert_eq!(f, [true, true, true, true, false], "{f:?}");
    }

    #[test]
    fn space_boundary_of_facade_wall_is_external() {
        let (m, _) = box_model();
        let t = export_ifc(&m).unwrap();
        let b: Vec<&str> = t
            .lines()
            .filter(|l| l.contains("IFCRELSPACEBOUNDARY"))
            .collect();
        assert_eq!(b.len(), 4);
        assert!(b.iter().all(|l| l.contains(",.PHYSICAL.,.EXTERNAL.)")), "{b:?}");
    }

    #[test]
    fn reimport_keeps_is_external_value() {
        let (mut m, _) = box_model();
        add_partition(&mut m);
        let t = export_ifc(&m).unwrap();
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, t.as_bytes()).unwrap();
        let ids: Vec<i64> = {
            let mut st = d.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
            let rows = st.query_map([], |x| x.get(0)).unwrap();
            rows.collect::<Result<_, _>>().unwrap()
        };
        let vals: Vec<i64> = ids
            .iter()
            .map(|id| {
                let p = d.props("wall", *id).unwrap();
                match p.into_iter().find(|x| x.0 == super::KEY).map(|x| x.1) {
                    Some(Value::Integer(i)) => i,
                    o => panic!("{o:?}"),
                }
            })
            .collect();
        assert_eq!(vals.iter().filter(|v| **v == 1).count(), 4, "{vals:?}");
        assert_eq!(vals.iter().filter(|v| **v == 0).count(), 1, "{vals:?}");
        let t2 = export_ifc(&d).unwrap();
        assert_eq!(flags_of(&t2).iter().filter(|x| **x).count(), 4);
        assert_eq!(pset_count(&t2), 5);
    }

    #[test]
    fn round_tripped_computed_flag_follows_geometry() {
        let (mut m, _) = box_model();
        add_partition(&mut m);
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        let part: i64 = d
            .sql()
            .query_row(
                "SELECT ref FROM prop WHERE kind='wall' AND key=?1 AND value=0",
                [super::KEY],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(super::explicit(&d, part), None, "own computed value stays computed");
        // the partition moved out of the box: now a free-standing, external wall
        d.set_wall_axis(part, (10.0, 0.0), (10.0, 4.0)).unwrap();
        let f = flags_of(&export_ifc(&d).unwrap());
        assert_eq!(f.iter().filter(|x| **x).count(), 5, "{f:?}");
        // a value other than the exported one is explicit again
        d.set_prop("wall", part, super::KEY, Some((Value::Integer(1), "")))
            .unwrap();
        assert_eq!(super::explicit(&d, part), Some(true));
    }

    #[test]
    fn user_set_equal_to_computed_stays_explicit() {
        let (mut m, _) = box_model();
        add_partition(&mut m);
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        let part: i64 = d
            .sql()
            .query_row(
                "SELECT ref FROM prop WHERE kind='wall' AND key=?1 AND value=0",
                [super::KEY],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(super::explicit(&d, part), None);
        d.set_prop("wall", part, super::KEY, Some((Value::Integer(0), "")))
            .unwrap();
        assert_eq!(super::explicit(&d, part), Some(false));
    }

    #[test]
    fn pinned_flag_survives_geometry_edit_and_export() {
        let (mut m, _) = box_model();
        add_partition(&mut m);
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        let part: i64 = d
            .sql()
            .query_row(
                "SELECT ref FROM prop WHERE kind='wall' AND key=?1 AND value=0",
                [super::KEY],
                |x| x.get(0),
            )
            .unwrap();
        d.set_prop("wall", part, super::KEY, Some((Value::Integer(0), "")))
            .unwrap();
        d.set_wall_axis(part, (10.0, 0.0), (10.0, 4.0)).unwrap();
        let f = flags_of(&export_ifc(&d).unwrap());
        assert_eq!(f.iter().filter(|x| **x).count(), 4, "{f:?}");
    }

    #[test]
    fn reimport_untouched_marker_stays_computed() {
        let (mut m, _) = box_model();
        add_partition(&mut m);
        let mut d = Model::open(":memory:").unwrap();
        import_ifc(&mut d, export_ifc(&m).unwrap().as_bytes()).unwrap();
        let mut e = Model::open(":memory:").unwrap();
        import_ifc(&mut e, export_ifc(&d).unwrap().as_bytes()).unwrap();
        let ids: Vec<i64> = {
            let mut st = e.sql().prepare("SELECT id FROM wall").unwrap();
            let rows = st.query_map([], |x| x.get(0)).unwrap();
            rows.collect::<Result<_, _>>().unwrap()
        };
        assert!(ids.iter().all(|id| super::explicit(&e, *id).is_none()));
    }

    #[test]
    fn explicit_source_value_wins() {
        let (mut m, ids) = box_model();
        m.set_prop("wall", ids[0], super::KEY, Some((Value::Integer(0), "")))
            .unwrap();
        let t = export_ifc(&m).unwrap();
        let f = flags_of(&t);
        assert_eq!(f, [false, true, true, true], "{f:?}");
        let n = t
            .lines()
            .filter(|l| l.contains("IFCRELSPACEBOUNDARY") && l.contains(",.INTERNAL.)"))
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn single_pset_wallcommon_per_wall() {
        let (mut m, ids) = box_model();
        let fire = Value::Text("EI60".into());
        m.set_prop("wall", ids[1], "Pset_WallCommon.FireRating", Some((fire, "")))
            .unwrap();
        m.set_prop("wall", ids[1], super::KEY, Some((Value::Integer(0), "")))
            .unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(pset_count(&t), 4, "one set per wall");
        assert_eq!(t.matches("'IsExternal'").count(), 4);
        assert!(t.contains("'FireRating'"));
    }
}
