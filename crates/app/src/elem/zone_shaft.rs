//! Shaft and zone families: plan grips, geometry rows and the 3D body of a shaft.
//!
//! A shaft is an outline through levels; its cuts in slabs and roofs follow every write of the
//! outline. A zone is a named group of rooms and has no outline of its own: it shows a small
//! marker under its first room, and its one grip moves all of its rooms. Every drag is one
//! model operation, so one undo step.

use crate::geom_props::{Effect, Row, Unit};
use crate::grips::{ring_edit, ring_grips, Grip, Kind, Role};
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, ShaftSpec, Target};

const LEN: (f64, f64) = (-1e4, 1e4);

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

/// Marker of a zone in plan: the point under its first room and that room's level.
fn anchor(m: &Model, id: i64) -> Result<(V2, i64), Error> {
    let z = m.zone(id)?;
    let room = *z.rooms.first().ok_or_else(bad)?;
    let level: i64 = m
        .sql()
        .query_row("SELECT level FROM room WHERE id=?1", [room], |r| r.get(0))?;
    let at = crate::tools::zone_anchor(m, &z, level).ok_or_else(bad)?;
    Ok((pv(at), level))
}

/// The marker square around `c`, closed.
fn marker(c: V2) -> Vec<V2> {
    let h = crate::tools::ZONE_MARK;
    vec![
        c + v2(-h, -h),
        c + v2(h, -h),
        c + v2(h, h),
        c + v2(-h, h),
        c + v2(-h, -h),
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub enum ZsChange {
    Shaft(i64, ShaftSpec),
    /// Zone moved by a vector; the point is the marker before the move.
    ZoneShift(i64, V2, V2),
}

pub fn grips_of(m: &Model, kind: Kind, id: i64, out: &mut Vec<Grip>) -> Result<(), Error> {
    match kind {
        Kind::Shaft => {
            let ring: Vec<V2> = m.shaft_ring(id)?.into_iter().map(pv).collect();
            out.push(Grip {
                kind,
                id,
                role: Role::Move,
                pos: centroid(&ring),
                view: String::new(),
            });
            ring_grips(kind, id, &ring, out);
        }
        Kind::Zone => {
            let (at, _) = anchor(m, id)?;
            out.push(Grip {
                kind,
                id,
                role: Role::Move,
                pos: at,
                view: String::new(),
            });
        }
        _ => return Err(bad()),
    }
    Ok(())
}

pub fn compute(m: &Model, g: &Grip, to: V2) -> Result<ZsChange, Error> {
    let d = to - g.pos;
    Ok(match g.kind {
        Kind::Shaft => {
            let mut s = m.shaft(g.id)?;
            if g.role == Role::Move {
                s.ring = s.ring.iter().map(|&p| xy(pv(p) + d)).collect();
            } else {
                let mut ring: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
                ring_edit(&mut ring, &g.role, g.pos, to)?;
                s.ring = ring.into_iter().map(xy).collect();
            }
            ZsChange::Shaft(g.id, s)
        }
        Kind::Zone => {
            if g.role != Role::Move {
                return Err(bad());
            }
            ZsChange::ZoneShift(g.id, d, g.pos)
        }
        _ => return Err(bad()),
    })
}

impl ZsChange {
    pub fn write(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            ZsChange::Shaft(id, s) => m.update_shaft(*id, s),
            ZsChange::ZoneShift(id, d, _) => m.move_elements(&[Target::Zone(*id)], xy(*d)),
        }
    }

    /// Live preview in plan.
    pub fn outline(&self) -> Vec<Vec<V2>> {
        match self {
            ZsChange::Shaft(_, s) => {
                let mut v: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
                if let Some(&f) = v.first() {
                    v.push(f);
                }
                vec![v]
            }
            ZsChange::ZoneShift(_, d, at) => vec![marker(*at + *d)],
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
        Kind::Shaft => {
            let s = m.shaft(g.id)?;
            Ok(elev(s.level)? + s.base_offset)
        }
        Kind::Zone => elev(anchor(m, g.id)?.1),
        _ => Ok(0.0),
    }
}

/// Offset rows of a shaft; the shaft panel (`struct_props`) already shows these fields.
pub fn rows() -> Vec<Row> {
    vec![
        Row {
            kind: "shaft",
            key: "geom.base_offset",
            unit: Unit::Len,
            lo: LEN.0,
            hi: LEN.1,
            delta: 0.25,
            effect: Effect::Scene,
            fields: &["base_offset"],
            in_panel: false,
            get: |m: &Model, id: i64| Ok(m.shaft(id)?.base_offset),
            set: |m: &mut Model, id: i64, v: f64| {
                let mut s = m.shaft(id)?;
                s.base_offset = v;
                m.update_shaft(id, &s)
            },
        },
        Row {
            kind: "shaft",
            key: "geom.top_offset",
            unit: Unit::Len,
            lo: LEN.0,
            hi: LEN.1,
            delta: 0.25,
            effect: Effect::Scene,
            fields: &["top_offset"],
            in_panel: false,
            get: |m: &Model, id: i64| Ok(m.shaft(id)?.top_offset),
            set: |m: &mut Model, id: i64, v: f64| {
                let mut s = m.shaft(id)?;
                s.top_offset = v;
                m.update_shaft(id, &s)
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::fixture;
    use ddc_model::{SlabSpec, ZoneSpec};

    fn grip(m: &Model, kind: Kind, id: i64, role: Role) -> Grip {
        crate::grips::grips(m, kind, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == role)
            .unwrap_or_else(|| panic!("no {role:?} grip"))
    }

    fn drag(m: &mut Model, g: &Grip, d: V2) {
        crate::grips::apply(m, g, g.pos + d).unwrap();
    }

    #[test]
    fn shaft_has_a_move_grip_and_a_vertex_and_edge_grip_per_corner() {
        let fx = fixture();
        let id = fx.ids["shaft"];
        let all = crate::grips::grips(&fx.ed.model, Kind::Shaft, id).unwrap();
        let ring = fx.ed.model.shaft_ring(id).unwrap();
        let count = |f: fn(&Role) -> bool| all.iter().filter(|g| f(&g.role)).count();
        assert_eq!(count(|r| *r == Role::Move), 1);
        assert_eq!(count(|r| matches!(r, Role::Vertex(_))), ring.len());
        assert_eq!(count(|r| matches!(r, Role::Edge(_))), ring.len());
    }

    #[test]
    fn dragging_the_move_grip_shifts_the_whole_outline_in_one_undo_step() {
        let mut fx = fixture();
        let id = fx.ids["shaft"];
        let before = fx.ed.model.shaft(id).unwrap();
        let g = grip(&fx.ed.model, Kind::Shaft, id, Role::Move);
        drag(&mut fx.ed.model, &g, v2(1.5, 0.5));
        let after = fx.ed.model.shaft(id).unwrap();
        for (a, b) in before.ring.iter().zip(after.ring.iter()) {
            assert!((b.0 - a.0 - 1.5).abs() < 1e-9 && (b.1 - a.1 - 0.5).abs() < 1e-9);
        }
        assert_eq!(
            (after.level, after.top_level, after.base_offset, after.top_offset),
            (before.level, before.top_level, before.base_offset, before.top_offset)
        );
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.shaft(id).unwrap(), before);
    }

    #[test]
    fn dragging_a_vertex_or_an_edge_changes_only_that_part() {
        let mut fx = fixture();
        let id = fx.ids["shaft"];
        let before = fx.ed.model.shaft(id).unwrap();
        let area = |r: &[(f64, f64)]| {
            let p: Vec<V2> = r.iter().map(|&q| pv(q)).collect();
            ddc_geom::poly::area(&p).abs()
        };
        let g = grip(&fx.ed.model, Kind::Shaft, id, Role::Vertex(2));
        drag(&mut fx.ed.model, &g, v2(1.0, 1.0));
        let moved = fx.ed.model.shaft(id).unwrap();
        assert!(area(&moved.ring) > area(&before.ring) + 1e-9);
        assert_eq!(
            moved.ring.iter().filter(|p| before.ring.contains(p)).count(),
            3,
            "three corners stay"
        );
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.shaft(id).unwrap(), before);

        let g = grip(&fx.ed.model, Kind::Shaft, id, Role::Edge(0));
        drag(&mut fx.ed.model, &g, v2(0.0, -1.0));
        assert_ne!(fx.ed.model.shaft(id).unwrap().ring, before.ring);
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.shaft(id).unwrap(), before);
    }

    #[test]
    fn a_collapsing_vertex_drag_is_refused_and_changes_nothing() {
        let mut fx = fixture();
        let id = fx.ids["shaft"];
        let before = fx.ed.model.shaft(id).unwrap();
        let g = grip(&fx.ed.model, Kind::Shaft, id, Role::Vertex(0));
        // onto the opposite corner: the outline would cross itself
        let opposite = pv(before.ring[2]);
        assert!(crate::grips::apply(&mut fx.ed.model, &g, opposite).is_err());
        assert_eq!(fx.ed.model.shaft(id).unwrap(), before);
    }

    #[test]
    fn grip_drag_keeps_the_cuts_of_every_slab_consistent() {
        let mut fx = fixture();
        let id = fx.ids["shaft"];
        let m = &mut fx.ed.model;
        let (_, slab_type, offset) = crate::query::slab_spec(m, fx.ids["slab"]).unwrap();
        let top = m.shaft(id).unwrap().top_level;
        // a slab on the upper level, clear of the shaft; the shaft goes into it
        let up = m
            .add_slab(&SlabSpec {
                level: top,
                slab_type,
                offset,
                outer: crate::geom_props::tests::square(600.0, 0.0, 10.0),
                holes: vec![],
            })
            .unwrap();
        let area = |m: &Model| m.slab_quantities(up).unwrap()[0].area;
        assert!(m.shaft_cuts(id).unwrap().iter().all(|c| !(c.kind == "slab" && c.id == up)));
        let whole = area(m);
        let g = grip(m, Kind::Shaft, id, Role::Move);
        drag(m, &g, v2(101.0, 1.0));
        assert!(area(m) < whole - 1e-9, "the shaft now cuts the slab");
        assert!(m.shaft_cuts(id).unwrap().iter().any(|c| c.kind == "slab" && c.id == up));
        let g = grip(m, Kind::Shaft, id, Role::Move);
        drag(m, &g, v2(-101.0, -1.0));
        assert!((area(m) - whole).abs() < 1e-9, "moved out: the slab is whole again");
        m.undo().unwrap();
        m.undo().unwrap();
        assert!((area(m) - whole).abs() < 1e-9);
    }

    fn zone_fixture() -> (crate::geom_props::tests::Fx, i64) {
        let mut fx = fixture();
        let room = fx.ids["room"];
        let z = fx
            .ed
            .model
            .add_zone(&ZoneSpec {
                name: "zz".into(),
                rooms: vec![room],
            })
            .unwrap();
        (fx, z)
    }

    #[test]
    fn zone_has_one_move_grip_under_its_first_room() {
        let (fx, z) = zone_fixture();
        let all = crate::grips::grips(&fx.ed.model, Kind::Zone, z).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].role, Role::Move);
        assert_eq!(all[0].pos, anchor(&fx.ed.model, z).unwrap().0);
    }

    #[test]
    fn zone_grip_moves_every_room_of_the_zone_and_keeps_the_group() {
        let (mut fx, z) = zone_fixture();
        let room = fx.ids["room"];
        let at = |m: &Model| -> (f64, f64) {
            m.sql()
                .query_row("SELECT px,py FROM room WHERE id=?1", [room], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap()
        };
        let before = (at(&fx.ed.model), fx.ed.model.zone(z).unwrap());
        let g = grip(&fx.ed.model, Kind::Zone, z, Role::Move);
        drag(&mut fx.ed.model, &g, v2(2.0, -1.0));
        let p = at(&fx.ed.model);
        assert!((p.0 - before.0 .0 - 2.0).abs() < 1e-9 && (p.1 - before.0 .1 + 1.0).abs() < 1e-9);
        assert_eq!(fx.ed.model.zone(z).unwrap(), before.1, "the same rooms in the zone");
        fx.ed.model.undo().unwrap();
        assert_eq!((at(&fx.ed.model), fx.ed.model.zone(z).unwrap()), before);
    }

    #[test]
    fn zone_preview_outline_is_a_closed_marker_at_the_new_place() {
        let (fx, z) = zone_fixture();
        let g = grip(&fx.ed.model, Kind::Zone, z, Role::Move);
        let c = compute(&fx.ed.model, &g, g.pos + v2(1.0, 0.0)).unwrap();
        let o = c.outline();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].first(), o[0].last(), "closed");
        let mid = centroid(&o[0][..4]);
        assert!((mid.x - g.pos.x - 1.0).abs() < 1e-9 && (mid.y - g.pos.y).abs() < 1e-9);
    }

    #[test]
    fn shaft_offset_rows_read_and_write_the_spec() {
        let mut fx = fixture();
        let id = fx.ids["shaft"];
        for r in rows() {
            let v0 = (r.get)(&fx.ed.model, id).unwrap();
            (r.set)(&mut fx.ed.model, id, v0 + r.delta).unwrap();
            assert_eq!((r.get)(&fx.ed.model, id).unwrap(), v0 + r.delta);
            fx.ed.model.undo().unwrap();
            assert_eq!((r.get)(&fx.ed.model, id).unwrap(), v0);
        }
    }
}
