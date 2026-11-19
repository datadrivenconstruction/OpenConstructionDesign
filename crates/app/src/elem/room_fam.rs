//! Room and room separator families: plan grips, the separator's length row and the 3D drawing
//! of both. A room whose boundary was sketched shows a grip on every boundary vertex; a room
//! closed by walls shows one grip at its room point. Dragging never touches a wall. Every drag
//! is one model operation, so one undo step.

use crate::geom_props::{Effect, Row, Unit};
use crate::grips::{Grip, Kind, Role};
use ddc_geom::predicates::point_in_ring;
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, SepSpec};

/// A shorter separator is degenerate; the grip springs back.
const MIN: f64 = 1e-3;
/// Half size of the room point marker in plan, m.
const MARK: f64 = 0.15;

fn bad() -> Error {
    Error::Invalid("grip does not fit the element".into())
}

fn pv((x, y): (f64, f64)) -> V2 {
    v2(x, y)
}

fn xy(p: V2) -> (f64, f64) {
    (p.x, p.y)
}

fn centroid(r: &[V2]) -> V2 {
    let n = r.len().max(1) as f64;
    r.iter().fold(v2(0.0, 0.0), |a, p| a + *p) * (1.0 / n)
}

/// Room point and sketched boundary of a room.
fn room_shape(m: &Model, id: i64) -> Result<(V2, Option<Vec<V2>>), Error> {
    let p: (f64, f64) = m
        .sql()
        .query_row("SELECT px,py FROM room WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let ring = m
        .room_outline(id)?
        .filter(|o| o.len() >= 3)
        .map(|o| o.into_iter().map(pv).collect());
    Ok((pv(p), ring))
}

fn room_number(m: &Model, id: i64) -> String {
    m.sql()
        .query_row("SELECT number FROM room WHERE id=?1", [id], |r| r.get(0))
        .unwrap_or_default()
}

fn leave(m: &Model, id: i64) -> Error {
    Error::Invalid(crate::tr("room.err.point_leave", &[("n", &room_number(m, id))]))
}

fn taken(m: &Model, id: i64, other: &str) -> Error {
    Error::Invalid(crate::tr(
        "room.err.point_taken",
        &[("n", &room_number(m, id)), ("other", other)],
    ))
}

/// A room point never lands in a region another room holds. A wall-bounded room's point also
/// stays in its own closed region; a point that starts outside every region may move freely
/// into free space.
fn check_point(m: &Model, id: i64, from: V2, to: V2) -> Result<(), Error> {
    let level = level_of(m, "room", id)?;
    let regions = m.room_regions(level)?;
    let own = regions.iter().position(|r| r.contains(from));
    if own.is_some_and(|o| regions[o].contains(to)) {
        return Ok(());
    }
    let mut st = m
        .sql()
        .prepare("SELECT number,px,py FROM room WHERE level=?1 AND id<>?2")?;
    let others: Vec<(String, f64, f64)> = st
        .query_map([level, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (n, x, y) in others {
        if regions.iter().any(|r| r.contains(to) && r.contains(v2(x, y))) {
            return Err(taken(m, id, &n));
        }
    }
    if own.is_some() {
        Err(leave(m, id))
    } else {
        Ok(())
    }
}

fn level_of(m: &Model, table: &str, id: i64) -> Result<i64, Error> {
    Ok(m.sql().query_row(
        &format!("SELECT level FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?)
}

#[derive(Clone, Debug, PartialEq)]
pub enum RfChange {
    /// Room point and, for a sketched room, its boundary.
    Room(i64, V2, Option<Vec<V2>>),
    Sep(i64, SepSpec),
}

fn put(out: &mut Vec<Grip>, kind: Kind, id: i64, role: Role, pos: V2) {
    out.push(Grip {
        kind,
        id,
        role,
        pos,
        view: String::new(),
    });
}

pub fn grips_of(m: &Model, kind: Kind, id: i64, out: &mut Vec<Grip>) -> Result<(), Error> {
    match kind {
        Kind::Room => match room_shape(m, id)? {
            (_, Some(ring)) => {
                put(out, kind, id, Role::Move, centroid(&ring));
                for (i, p) in ring.iter().enumerate() {
                    put(out, kind, id, Role::Vertex(i), *p);
                }
            }
            (at, None) => put(out, kind, id, Role::Move, at),
        },
        Kind::RoomSep => {
            let s = m.room_sep(id)?;
            let (a, b) = (pv(s.a), pv(s.b));
            put(out, kind, id, Role::Move, (a + b) * 0.5);
            put(out, kind, id, Role::End(0), a);
            put(out, kind, id, Role::End(1), b);
        }
        _ => return Err(bad()),
    }
    Ok(())
}

pub fn compute(m: &Model, g: &Grip, to: V2) -> Result<RfChange, Error> {
    let d = to - g.pos;
    Ok(match g.kind {
        Kind::Room => {
            let (at, ring) = room_shape(m, g.id)?;
            match (&g.role, ring) {
                (Role::Move, ring) => {
                    if ring.is_none() {
                        check_point(m, g.id, at, at + d)?;
                    }
                    RfChange::Room(
                        g.id,
                        at + d,
                        ring.map(|r| r.iter().map(|&p| p + d).collect()),
                    )
                }
                (Role::Vertex(i), Some(mut ring)) if *i < ring.len() => {
                    ring[*i] = to;
                    ddc_geom::poly::simple_ring(&ring, MIN)
                        .map_err(|e| Error::Degenerate(format!("room boundary: {e}")))?;
                    if point_in_ring(at, &ring) <= 0 {
                        return Err(leave(m, g.id));
                    }
                    RfChange::Room(g.id, at, Some(ring))
                }
                _ => return Err(bad()),
            }
        }
        Kind::RoomSep => {
            let mut s = m.room_sep(g.id)?;
            match g.role {
                Role::Move => {
                    s.a = xy(pv(s.a) + d);
                    s.b = xy(pv(s.b) + d);
                }
                Role::End(0) => s.a = xy(to),
                Role::End(1) => s.b = xy(to),
                _ => return Err(bad()),
            }
            if (pv(s.b) - pv(s.a)).len() < MIN {
                return Err(Error::Degenerate("separator length would be zero".into()));
            }
            RfChange::Sep(g.id, s)
        }
        _ => return Err(bad()),
    })
}

impl RfChange {
    pub fn write(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            RfChange::Room(id, at, ring) => {
                let ring: Option<Vec<(f64, f64)>> =
                    ring.as_ref().map(|r| r.iter().map(|&p| xy(p)).collect());
                m.set_room_shape(*id, xy(*at), ring.as_deref())
            }
            RfChange::Sep(id, s) => m.update_room_sep(*id, s),
        }
    }

    /// Live preview in plan.
    pub fn outline(&self) -> Vec<Vec<V2>> {
        match self {
            RfChange::Room(_, _, Some(ring)) => {
                let mut v = ring.clone();
                v.extend(ring.first().copied());
                vec![v]
            }
            RfChange::Room(_, at, None) => vec![
                vec![*at - v2(MARK, 0.0), *at + v2(MARK, 0.0)],
                vec![*at - v2(0.0, MARK), *at + v2(0.0, MARK)],
            ],
            RfChange::Sep(_, s) => vec![vec![pv(s.a), pv(s.b)]],
        }
    }
}

/// Absolute height of a plan grip for its 3D copy.
pub fn plan_z(
    m: &Model,
    g: &Grip,
    elev: &dyn Fn(i64) -> Result<f64, Error>,
) -> Result<f64, Error> {
    match g.kind {
        Kind::Room => elev(level_of(m, "room", g.id)?),
        Kind::RoomSep => elev(level_of(m, "room_sep", g.id)?),
        _ => Ok(0.0),
    }
}

/// Separator length: the far end moves along the line, the near end stays.
pub fn rows() -> Vec<Row> {
    vec![Row {
        kind: "room_sep",
        key: "geom.length",
        unit: Unit::Len,
        lo: 0.01,
        hi: 1e4,
        delta: 0.25,
        effect: Effect::Scene,
        fields: &["b"],
        in_panel: true,
        get: |m: &Model, id: i64| {
            let s = m.room_sep(id)?;
            Ok((pv(s.b) - pv(s.a)).len())
        },
        set: |m: &mut Model, id: i64, v: f64| {
            let mut s = m.room_sep(id)?;
            let d = pv(s.b) - pv(s.a);
            if d.len() < MIN {
                return Err(Error::Degenerate("separator length is zero".into()));
            }
            s.b = xy(pv(s.a) + d * (v / d.len()));
            m.update_room_sep(id, &s)
        },
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::fixture;

    fn grip(m: &Model, kind: Kind, id: i64, role: Role) -> Grip {
        crate::grips::grips(m, kind, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == role)
            .unwrap_or_else(|| panic!("no {role:?} grip"))
    }

    /// A room with a sketched boundary, away from every wall of the fixture.
    fn sketched(m: &mut Model) -> (i64, [(f64, f64); 4]) {
        let level = m
            .sql()
            .query_row("SELECT level FROM room LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let id = m
            .add_room(&ddc_model::RoomSpec {
                level,
                name: "s".into(),
                number: "9".into(),
                point: (300.0, 0.0),
                height: 2.7,
            })
            .unwrap();
        let ring = [(300.0, 0.0), (304.0, 0.0), (304.0, 3.0), (300.0, 3.0)];
        m.set_room_shape(id, (302.0, 1.0), Some(&ring)).unwrap();
        (id, ring)
    }

    #[test]
    fn a_room_closed_by_walls_has_one_grip_at_its_point() {
        let fx = fixture();
        let id = fx.ids["room"];
        let all = crate::grips::grips(&fx.ed.model, Kind::Room, id).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].role, Role::Move);
        assert_eq!(all[0].pos, v2(4.0, 3.0));
    }

    #[test]
    fn a_sketched_room_has_a_grip_per_vertex_and_a_drag_is_one_undo() {
        let mut fx = fixture();
        let m = &mut fx.ed.model;
        let (id, ring) = sketched(m);
        let all = crate::grips::grips(m, Kind::Room, id).unwrap();
        assert_eq!(
            all.iter()
                .filter(|g| matches!(g.role, Role::Vertex(_)))
                .count(),
            4
        );
        let g = grip(m, Kind::Room, id, Role::Vertex(2));
        crate::grips::apply(m, &g, g.pos + v2(1.0, 1.0)).unwrap();
        let got = m.room_outline(id).unwrap().unwrap();
        assert_eq!(got[2], (305.0, 4.0));
        assert_eq!(got[0], ring[0]);
        m.undo().unwrap();
        assert_eq!(m.room_outline(id).unwrap().unwrap(), ring.to_vec());
    }

    #[test]
    fn a_crossing_boundary_drag_is_refused_and_changes_nothing() {
        let mut fx = fixture();
        let m = &mut fx.ed.model;
        let (id, ring) = sketched(m);
        let g = grip(m, Kind::Room, id, Role::Vertex(0));
        assert!(crate::grips::apply(m, &g, v2(304.0, 3.0)).is_err());
        assert_eq!(m.room_outline(id).unwrap().unwrap(), ring.to_vec());
    }

    #[test]
    fn separator_end_grip_moves_one_end_and_zero_length_is_refused() {
        let mut fx = fixture();
        let id = fx.ids["room_sep"];
        let m = &mut fx.ed.model;
        let before = m.room_sep(id).unwrap();
        let g = grip(m, Kind::RoomSep, id, Role::End(1));
        crate::grips::apply(m, &g, g.pos + v2(0.0, 2.0)).unwrap();
        let s = m.room_sep(id).unwrap();
        assert_eq!((s.a, s.b), (before.a, (before.b.0, before.b.1 + 2.0)));
        m.undo().unwrap();
        assert_eq!(m.room_sep(id).unwrap(), before);
        let g = grip(m, Kind::RoomSep, id, Role::End(0));
        assert!(crate::grips::apply(m, &g, pv(before.b)).is_err());
        assert_eq!(m.room_sep(id).unwrap(), before);
    }

    #[test]
    fn length_row_keeps_the_near_end_and_the_direction() {
        let mut fx = fixture();
        let id = fx.ids["room_sep"];
        let m = &mut fx.ed.model;
        let s0 = m.room_sep(id).unwrap();
        let r = &rows()[0];
        let l0 = (r.get)(m, id).unwrap();
        (r.set)(m, id, l0 + 1.0).unwrap();
        let s1 = m.room_sep(id).unwrap();
        assert_eq!(s1.a, s0.a);
        assert!(((r.get)(m, id).unwrap() - l0 - 1.0).abs() < 1e-9);
        let cross = (s1.b.0 - s1.a.0) * (s0.b.1 - s0.a.1) - (s1.b.1 - s1.a.1) * (s0.b.0 - s0.a.0);
        assert!(cross.abs() < 1e-9);
    }

    fn number(m: &Model, id: i64) -> String {
        m.sql()
            .query_row("SELECT number FROM room WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn room_copy_gets_new_number() {
        use ddc_model::Target;
        let mut fx = fixture();
        let id = fx.ids["room"];
        let m = &mut fx.ed.model;
        let before = number(m, id);
        let copy = m.copy_elements(&[Target::Room(id)], &[(0.5, 0.25)]).unwrap();
        let Target::Room(c) = copy[0] else { panic!("not a room") };
        assert_eq!(number(m, id), before, "the original keeps its number");
        assert_ne!(number(m, c), before);
        let made = m
            .array_linear(&[Target::Room(id)], 3, ddc_model::Spacing::Step((0.5, 0.25)))
            .unwrap();
        let mut all: Vec<String> = made
            .iter()
            .filter_map(|t| match t {
                Target::Room(r) => Some(number(m, *r)),
                _ => None,
            })
            .collect();
        all.extend([before, number(m, c)]);
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n, "every room of the level has its own number");
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(number(m, id), "1");
    }

    /// Four walls around x..x+4, y 0..3: a closed region, no room in it.
    fn walls_at(m: &mut Model, level: i64, x: f64) {
        let (wt, h): (i64, f64) = m
            .sql()
            .query_row("SELECT wtype,height FROM wall LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let c = [(x, 0.0), (x + 4.0, 0.0), (x + 4.0, 3.0), (x, 3.0)];
        for i in 0..4 {
            m.add_wall(&ddc_model::WallSpec {
                a: c[i],
                b: c[(i + 1) % 4],
                level,
                base_offset: 0.0,
                height: h,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        }
    }

    fn walled(m: &mut Model, level: i64) -> i64 {
        walls_at(m, level, 20.0);
        m.add_room(&ddc_model::RoomSpec {
            level,
            name: "o".into(),
            number: "77".into(),
            point: (22.0, 1.5),
            height: 2.7,
        })
        .unwrap()
    }

    #[test]
    fn room_point_drag_out_of_walls_refused() {
        let mut fx = fixture();
        let id = fx.ids["room"];
        let m = &mut fx.ed.model;
        let g = grip(m, Kind::Room, id, Role::Move);
        let p0 = room_shape(m, id).unwrap().0;
        let e = crate::grips::apply(m, &g, v2(-50.0, -50.0)).unwrap_err();
        assert!(e.to_string().contains(&leave(m, id).to_string()));
        assert_eq!(room_shape(m, id).unwrap().0, p0);
    }

    #[test]
    fn room_point_drag_into_other_room_refused() {
        let mut fx = fixture();
        let id = fx.ids["room"];
        let level = level_of(&fx.ed.model, "room", id).unwrap();
        walled(&mut fx.ed.model, level);
        let m = &mut fx.ed.model;
        let g = grip(m, Kind::Room, id, Role::Move);
        let p0 = room_shape(m, id).unwrap().0;
        let e = crate::grips::apply(m, &g, v2(22.0, 1.0)).unwrap_err();
        assert!(e.to_string().contains(&taken(m, id, "77").to_string()));
        assert_eq!(room_shape(m, id).unwrap().0, p0);
    }

    fn loose_room(m: &mut Model, level: i64) -> i64 {
        m.add_room(&ddc_model::RoomSpec {
            level,
            name: "u".into(),
            number: "88".into(),
            point: (50.0, 50.0),
            height: 2.7,
        })
        .unwrap()
    }

    #[test]
    fn unenclosed_room_into_taken_region_refused() {
        let mut fx = fixture();
        let level = fx.ed.level;
        walled(&mut fx.ed.model, level);
        let m = &mut fx.ed.model;
        let id = loose_room(m, level);
        let g = grip(m, Kind::Room, id, Role::Move);
        let p0 = room_shape(m, id).unwrap().0;
        let e = crate::grips::apply(m, &g, v2(22.0, 1.0)).unwrap_err();
        assert!(e.to_string().contains(&taken(m, id, "77").to_string()));
        assert_eq!(room_shape(m, id).unwrap().0, p0);
    }

    #[test]
    fn unenclosed_room_into_free_region_ok() {
        let mut fx = fixture();
        let level = fx.ed.level;
        walls_at(&mut fx.ed.model, level, 30.0);
        let m = &mut fx.ed.model;
        let id = loose_room(m, level);
        let g = grip(m, Kind::Room, id, Role::Move);
        crate::grips::apply(m, &g, v2(32.0, 1.5)).unwrap();
        assert_eq!(room_shape(m, id).unwrap().0, v2(32.0, 1.5));
        m.undo().unwrap();
        assert_eq!(room_shape(m, id).unwrap().0, v2(50.0, 50.0));
    }

    #[test]
    fn point_errors_localized() {
        use ddc_ui::i18n::{Catalog, Lang};
        let mut fx = fixture();
        let level = fx.ed.level;
        walled(&mut fx.ed.model, level);
        let id = fx.ids["room"];
        let m = &mut fx.ed.model;
        let g = grip(m, Kind::Room, id, Role::Move);
        let cat = Catalog::new(Lang::En);
        for key in ["room.err.point_leave", "room.err.point_taken"] {
            let (en, ru) = (cat.t_in(Lang::En, key), cat.t_in(Lang::Ru, key));
            assert!(en.contains("{n}") && ru.contains("{n}"), "{key}");
            assert_ne!(en, ru, "{key} is translated");
            assert_ne!(en, key, "{key} exists");
        }
        assert!(cat.t_in(Lang::Ru, "room.err.point_taken").contains("{other}"));
        let num = room_number(m, id);
        let leave_msg = crate::grips::apply(m, &g, v2(-50.0, -50.0)).unwrap_err().to_string();
        let taken_msg = crate::grips::apply(m, &g, v2(22.0, 1.0)).unwrap_err().to_string();
        for msg in [&leave_msg, &taken_msg] {
            assert!(!msg.contains('{') && !msg.contains("room.err"), "{msg}");
            assert!(msg.contains(&num), "{msg}");
        }
        assert!(taken_msg.contains("77"), "{taken_msg}");
        assert_ne!(leave_msg, taken_msg);
    }

    #[test]
    fn sketched_vertex_drag_point_outside_refused() {
        let mut fx = fixture();
        let m = &mut fx.ed.model;
        let (id, ring) = sketched(m);
        // Pull vertex 1 so the room point (302,1) falls outside the new ring.
        let g = grip(m, Kind::Room, id, Role::Vertex(1));
        let e = crate::grips::apply(m, &g, v2(300.5, 0.5)).unwrap_err();
        assert!(e.to_string().contains(&leave(m, id).to_string()));
        assert_eq!(m.room_outline(id).unwrap().unwrap(), ring.to_vec());
    }

    #[test]
    fn room_point_drag_inside_one_undo() {
        let mut fx = fixture();
        let id = fx.ids["room"];
        let m = &mut fx.ed.model;
        let g = grip(m, Kind::Room, id, Role::Move);
        let p0 = room_shape(m, id).unwrap().0;
        crate::grips::apply(m, &g, g.pos + v2(0.1, 0.1)).unwrap();
        assert_eq!(room_shape(m, id).unwrap().0, p0 + v2(0.1, 0.1));
        m.undo().unwrap();
        assert_eq!(room_shape(m, id).unwrap().0, p0);
    }

    #[test]
    fn room_move_leaves_walls() {
        use ddc_model::Target;
        let mut fx = fixture();
        let id = fx.ids["room"];
        let m = &mut fx.ed.model;
        let walls: Vec<i64> = m
            .sql()
            .prepare("SELECT id FROM wall ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let rows = |m: &Model| -> String {
            let items: Vec<(&str, i64)> = walls.iter().map(|w| ("wall", *w)).collect();
            m.clip_copy(&items).unwrap().to_text()
        };
        let (w0, p0) = (rows(m), room_shape(m, id).unwrap().0);
        m.move_elements(&[Target::Room(id)], (1.0, -0.5)).unwrap();
        assert_eq!(rows(m), w0, "no wall moved");
        assert_eq!(room_shape(m, id).unwrap().0, p0 + v2(1.0, -0.5));
        m.undo().unwrap();
        assert_eq!(room_shape(m, id).unwrap().0, p0);
    }

    #[test]
    fn room_delete_removes_tags_one_undo() {
        use crate::editor::Sel;
        use ddc_model::TagHost;
        let mut fx = fixture();
        let id = fx.ids["room"];
        let level = fx.ed.level;
        fx.ed.model.tag_element(level, TagHost::Room(id), false).unwrap();
        assert_eq!(fx.ed.model.tags(level).unwrap().len(), 1);
        fx.ed.reload().unwrap();
        fx.ed.selection = vec![Sel::Room(id)];
        fx.ed.delete_selection();
        assert!(fx.ed.model.room_outline(id).is_err(), "the room is gone");
        assert!(fx.ed.model.tags(level).unwrap().is_empty(), "its tag went with it");
        fx.ed.undo();
        assert!(fx.ed.model.room_outline(id).is_ok());
        assert_eq!(fx.ed.model.tags(level).unwrap().len(), 1, "one undo restores both");
    }
}
