//! Plan grips of the foundation and framing families: footings, strip footings, rafts, braces,
//! trusses, beam systems and ramps. One drag is one spec update, so one undo step. Elements that
//! follow a host (column, wall, member) show no grips: the host moves them.

use crate::grips::{ring_edit, ring_grips, Grip, Kind, Role};
use ddc_geom::predicates::point_in_ring;
use ddc_geom::{v2, V2};
use ddc_model::{
    BeamSystemSpec, BraceSpec, Error, FootingSpec, Model, RaftSpec, RampSpec, RebarLayout,
    StripFootingSpec, TrussSpec,
};

/// A smaller change is a degenerate element; the grip springs back.
const MIN: f64 = 1e-3;
/// Reach of the rotate grip of a footing, m.
const ARM: f64 = 0.8;

#[derive(Clone, Debug, PartialEq)]
pub enum StructChange {
    Footing(i64, FootingSpec),
    StripFooting(i64, StripFootingSpec),
    Raft(i64, RaftSpec),
    Brace(i64, BraceSpec),
    Truss(i64, TrussSpec),
    BeamSystem(i64, BeamSystemSpec),
    Ramp(i64, RampSpec),
    /// Shaft and zone (`elem::zone_shaft`).
    ZoneShaft(super::zone_shaft::ZsChange),
    /// Room and room separator (`elem::room_fam`).
    RoomFam(super::room_fam::RfChange),
}

fn pv((x, y): (f64, f64)) -> V2 {
    v2(x, y)
}

fn xy(p: V2) -> (f64, f64) {
    (p.x, p.y)
}

fn polar(a: f64, r: f64) -> V2 {
    v2(r * a.cos(), r * a.sin())
}

fn mid(a: V2, b: V2) -> V2 {
    (a + b) * 0.5
}

fn centroid(r: &[V2]) -> V2 {
    let n = r.len().max(1) as f64;
    r.iter().fold(v2(0.0, 0.0), |a, p| a + *p) * (1.0 / n)
}

fn bad() -> Error {
    Error::Invalid("grip does not fit the element".into())
}

fn degenerate(what: &str) -> Error {
    Error::Invalid(format!("{what} would collapse to a point"))
}

fn shift(ring: &[(f64, f64)], d: V2) -> Vec<(f64, f64)> {
    ring.iter().map(|&p| (p.0 + d.x, p.1 + d.y)).collect()
}

/// Insert grips at a quarter of each edge: dragging one adds a point after vertex `i`.
fn insert_grips(kind: Kind, id: i64, ring: &[V2], out: &mut Vec<Grip>) {
    let n = ring.len();
    for (i, &p) in ring.iter().enumerate() {
        let q = ring[(i + 1) % n];
        put(out, kind, id, Role::Insert(i), p + (q - p) * 0.25);
    }
}

fn edit_ring(ring: &mut Vec<V2>, role: &Role, grab: V2, to: V2) -> Result<(), Error> {
    match *role {
        Role::Insert(i) if i < ring.len() => ring.insert(i + 1, to),
        _ => ring_edit(ring, role, grab, to)?,
    }
    Ok(())
}

fn simple_ring(what: &str, ring: &[(f64, f64)]) -> Result<(), Error> {
    let r: Vec<V2> = ring.iter().map(|&p| pv(p)).collect();
    ddc_geom::poly::simple_ring(&r, MIN).map_err(|e| Error::Degenerate(format!("{what}: {e}")))
}

/// Beams of the level that lay on the old outline must stay on the new one.
fn hosted_beams_ok(
    m: &Model,
    level: i64,
    old: &[(f64, f64)],
    new: &[(f64, f64)],
) -> Result<(), Error> {
    let o: Vec<V2> = old.iter().map(|&p| pv(p)).collect();
    let n: Vec<V2> = new.iter().map(|&p| pv(p)).collect();
    for (_, b) in m.beams_on(level)? {
        let ends = [v2(b.a.0, b.a.1), v2(b.b.0, b.b.1)];
        if ends.iter().all(|&e| point_in_ring(e, &o) >= 0)
            && ends.iter().any(|&e| point_in_ring(e, &n) < 0)
        {
            return Err(Error::Degenerate(crate::tr("rbdrag.beam", &[])));
        }
    }
    Ok(())
}

/// Openings of a beam system must stay inside the new outline.
fn holes_ok(holes: &[Vec<(f64, f64)>], new: &[(f64, f64)]) -> Result<(), Error> {
    let n: Vec<V2> = new.iter().map(|&p| pv(p)).collect();
    if holes
        .iter()
        .flatten()
        .any(|&p| point_in_ring(pv(p), &n) < 0)
    {
        return Err(Error::Degenerate(crate::tr("rbdrag.opening", &[])));
    }
    Ok(())
}

/// Rebar on a raft: area sets are rebuilt inside the outline, so only explicit bars (free
/// layout, plan ends in world coordinates) can be stranded. One that lay inside the old outline
/// must stay inside the new one, else the edit is refused and nothing changes.
fn hosted_rebar_ok(
    m: &Model,
    raft: i64,
    old: &[(f64, f64)],
    new: &[(f64, f64)],
) -> Result<(), Error> {
    let o: Vec<V2> = old.iter().map(|&p| pv(p)).collect();
    let n: Vec<V2> = new.iter().map(|&p| pv(p)).collect();
    for (id, s) in m.rebar_sets_of("raft", Some(raft))? {
        let RebarLayout::Free { a, b, .. } = s.layout else {
            continue;
        };
        let ends = [v2(a.0, a.1), v2(b.0, b.1)];
        if ends.iter().all(|&e| point_in_ring(e, &o) >= 0)
            && ends.iter().any(|&e| point_in_ring(e, &n) < 0)
        {
            return Err(Error::Degenerate(crate::tr(
                "rbdrag.rebar",
                &[("id", &id.to_string())],
            )));
        }
    }
    Ok(())
}

fn drop_point(what: &str, ring: &mut Vec<(f64, f64)>, i: usize) -> Result<(), Error> {
    if i >= ring.len() {
        return Err(bad());
    }
    if ring.len() <= 3 {
        return Err(Error::Degenerate(format!(
            "{what} needs at least three points"
        )));
    }
    ring.remove(i);
    simple_ring(what, ring)
}

/// Del on a vertex of a raft or beam system: one model op; the element always stays.
pub fn remove_vertex(m: &mut Model, kind: Kind, id: i64, i: usize) -> Result<(), Error> {
    match kind {
        Kind::Raft => {
            let mut s = m.raft(id)?;
            let old = s.ring.clone();
            drop_point("raft", &mut s.ring, i)?;
            hosted_beams_ok(m, s.level, &old, &s.ring)?;
            hosted_rebar_ok(m, id, &old, &s.ring)?;
            m.update_raft(id, &s)
        }
        Kind::BeamSystem => {
            let mut s = m.beam_system(id)?;
            let old = s.ring.clone();
            let n = s.ring.len();
            drop_point("beam system", &mut s.ring, i)?;
            // edges i-1 and i merge into one; later edges shift down
            s.start_edge = s.start_edge.map(|e| match (i, e) {
                (0, 0) => n - 2,
                (0, e) => e - 1,
                (_, e) if e >= i => e - 1,
                (_, e) => e,
            });
            hosted_beams_ok(m, s.level, &old, &s.ring)?;
            holes_ok(&s.holes, &s.ring)?;
            m.update_beam_system(id, &s)
        }
        _ => Err(bad()),
    }
}

/// Adds the grips of `kind` for element `id` to `out`.
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
        Kind::Footing => {
            let s = m.footing(id)?;
            if s.column.is_none() {
                let at = pv(s.at);
                put(out, kind, id, Role::Move, at);
                put(out, kind, id, Role::Rotate, at + polar(s.rotation, ARM));
            }
        }
        Kind::StripFooting => {
            let s = m.strip_footing(id)?;
            if s.wall.is_none() {
                let (a, b) = (pv(s.a), pv(s.b));
                put(out, kind, id, Role::Move, mid(a, b));
                put(out, kind, id, Role::End(0), a);
                put(out, kind, id, Role::End(1), b);
            }
        }
        Kind::Raft => {
            let s = m.raft(id)?;
            let ring: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
            put(out, kind, id, Role::Move, centroid(&ring));
            ring_grips(kind, id, &ring, out);
            insert_grips(kind, id, &ring, out);
        }
        Kind::Brace => {
            let s = m.brace(id)?;
            if s.start.is_none() && s.end.is_none() {
                let (a, b) = (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1));
                put(out, kind, id, Role::Move, mid(a, b));
                put(out, kind, id, Role::End(0), a);
                put(out, kind, id, Role::End(1), b);
            }
        }
        Kind::Truss => {
            let s = m.truss(id)?;
            let (a, b) = (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1));
            put(out, kind, id, Role::Move, mid(a, b));
            put(out, kind, id, Role::End(0), a);
            put(out, kind, id, Role::End(1), b);
        }
        Kind::BeamSystem => {
            let s = m.beam_system(id)?;
            let ring: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
            put(out, kind, id, Role::Move, centroid(&ring));
            ring_grips(kind, id, &ring, out);
            insert_grips(kind, id, &ring, out);
        }
        Kind::Ramp => {
            let s = m.ramp(id)?;
            let path: Vec<V2> = s.path.iter().map(|&p| pv(p)).collect();
            put(out, kind, id, Role::Move, centroid(&path));
            for (i, p) in path.iter().enumerate() {
                put(out, kind, id, Role::Vertex(i), *p);
            }
        }
        Kind::Shaft | Kind::Zone => return super::zone_shaft::grips_of(m, kind, id, out),
        Kind::Room | Kind::RoomSep => return super::room_fam::grips_of(m, kind, id, out),
        _ => return Err(bad()),
    }
    Ok(())
}

/// The new state of the element after dragging `g` to `to`.
pub fn compute(m: &Model, g: &Grip, to: V2) -> Result<StructChange, Error> {
    let id = g.id;
    let d = to - g.pos;
    let moved = g.role == Role::Move;
    Ok(match g.kind {
        Kind::Footing => {
            let mut s = m.footing(id)?;
            match g.role {
                Role::Move => s.at = xy(pv(s.at) + d),
                Role::Rotate => {
                    let r = to - pv(s.at);
                    if r.len() < MIN {
                        return Err(degenerate("footing turn"));
                    }
                    s.rotation = r.y.atan2(r.x);
                }
                _ => return Err(bad()),
            }
            StructChange::Footing(id, s)
        }
        Kind::StripFooting => {
            let mut s = m.strip_footing(id)?;
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
                return Err(degenerate("strip footing"));
            }
            StructChange::StripFooting(id, s)
        }
        Kind::Raft => {
            let mut s = m.raft(id)?;
            if moved {
                s.ring = shift(&s.ring, d);
            } else {
                let mut ring: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
                edit_ring(&mut ring, &g.role, g.pos, to)?;
                let old = std::mem::replace(&mut s.ring, ring.into_iter().map(xy).collect());
                hosted_beams_ok(m, s.level, &old, &s.ring)?;
                hosted_rebar_ok(m, id, &old, &s.ring)?;
            }
            StructChange::Raft(id, s)
        }
        Kind::Brace => {
            let mut s = m.brace(id)?;
            match g.role {
                Role::Move => {
                    s.a = (s.a.0 + d.x, s.a.1 + d.y, s.a.2);
                    s.b = (s.b.0 + d.x, s.b.1 + d.y, s.b.2);
                }
                Role::End(0) => s.a = (to.x, to.y, s.a.2),
                Role::End(1) => s.b = (to.x, to.y, s.b.2),
                _ => return Err(bad()),
            }
            if (v2(s.b.0, s.b.1) - v2(s.a.0, s.a.1)).len() < MIN && (s.b.2 - s.a.2).abs() < MIN {
                return Err(degenerate("brace"));
            }
            StructChange::Brace(id, s)
        }
        Kind::Truss => {
            let mut s = m.truss(id)?;
            match g.role {
                Role::Move => {
                    s.a = (s.a.0 + d.x, s.a.1 + d.y, s.a.2);
                    s.b = (s.b.0 + d.x, s.b.1 + d.y, s.b.2);
                }
                Role::End(0) => s.a = (to.x, to.y, s.a.2),
                Role::End(1) => s.b = (to.x, to.y, s.b.2),
                _ => return Err(bad()),
            }
            if (v2(s.b.0, s.b.1) - v2(s.a.0, s.a.1)).len() < MIN {
                return Err(degenerate("truss"));
            }
            StructChange::Truss(id, s)
        }
        Kind::BeamSystem => {
            let mut s = m.beam_system(id)?;
            if moved {
                s.ring = shift(&s.ring, d);
                s.holes = s.holes.iter().map(|h| shift(h, d)).collect();
            } else {
                let mut ring: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
                edit_ring(&mut ring, &g.role, g.pos, to)?;
                let old = std::mem::replace(&mut s.ring, ring.into_iter().map(xy).collect());
                hosted_beams_ok(m, s.level, &old, &s.ring)?;
                holes_ok(&s.holes, &s.ring)?;
                // the start edge keeps its side: edges after the split shift by one
                if let (Role::Insert(i), Some(e)) = (&g.role, s.start_edge) {
                    s.start_edge = Some(if e > *i { e + 1 } else { e });
                }
            }
            StructChange::BeamSystem(id, s)
        }
        Kind::Ramp => {
            let mut s = m.ramp(id)?;
            match g.role {
                Role::Move => s.path = shift(&s.path, d),
                Role::Vertex(i) if i < s.path.len() => s.path[i] = xy(to),
                _ => return Err(bad()),
            }
            StructChange::Ramp(id, s)
        }
        Kind::Shaft | Kind::Zone => StructChange::ZoneShaft(super::zone_shaft::compute(m, g, to)?),
        Kind::Room | Kind::RoomSep => StructChange::RoomFam(super::room_fam::compute(m, g, to)?),
        _ => return Err(bad()),
    })
}

impl StructChange {
    pub fn write(&self, m: &mut Model) -> Result<(), Error> {
        match self {
            StructChange::Footing(id, s) => m.update_footing(*id, s),
            StructChange::StripFooting(id, s) => m.update_strip_footing(*id, s),
            StructChange::Raft(id, s) => m.update_raft(*id, s),
            StructChange::Brace(id, s) => m.update_brace(*id, s),
            StructChange::Truss(id, s) => m.update_truss(*id, s),
            StructChange::BeamSystem(id, s) => m.update_beam_system(*id, s),
            StructChange::Ramp(id, s) => m.update_ramp(*id, s),
            StructChange::ZoneShaft(c) => c.write(m),
            StructChange::RoomFam(c) => c.write(m),
        }
    }

    /// Live preview: polylines of the new state in plan (closed outlines repeat the first point).
    pub fn outline(&self) -> Vec<Vec<V2>> {
        let closed = |r: &[(f64, f64)]| {
            let mut v: Vec<V2> = r.iter().map(|&p| pv(p)).collect();
            if let Some(&f) = v.first() {
                v.push(f);
            }
            v
        };
        match self {
            StructChange::Footing(_, s) => {
                let (w, l) = s.steps.first().map_or((1.0, 1.0), |t| (t.w, t.l));
                let (u, v) = (polar(s.rotation, w / 2.0), polar(s.rotation + std::f64::consts::FRAC_PI_2, l / 2.0));
                let c = pv(s.at);
                vec![vec![c - u - v, c + u - v, c + u + v, c - u + v, c - u - v]]
            }
            StructChange::StripFooting(_, s) => vec![vec![pv(s.a), pv(s.b)]],
            StructChange::Raft(_, s) => vec![closed(&s.ring)],
            StructChange::Brace(_, s) => vec![vec![v2(s.a.0, s.a.1), v2(s.b.0, s.b.1)]],
            StructChange::Truss(_, s) => vec![vec![v2(s.a.0, s.a.1), v2(s.b.0, s.b.1)]],
            StructChange::BeamSystem(_, s) => vec![closed(&s.ring)],
            StructChange::Ramp(_, s) => vec![s.path.iter().map(|&p| pv(p)).collect()],
            StructChange::ZoneShaft(c) => c.outline(),
            StructChange::RoomFam(c) => c.outline(),
        }
    }
}

/// Absolute height of a plan grip for its 3D copy; `elev` is the elevation of a level.
pub fn plan_z(
    m: &Model,
    g: &Grip,
    elev: &dyn Fn(i64) -> Result<f64, Error>,
) -> Result<f64, Error> {
    let id = g.id;
    let end1 = g.role == Role::End(1);
    Ok(match g.kind {
        Kind::Footing => {
            let s = m.footing(id)?;
            elev(s.level)? + s.top_offset
        }
        Kind::StripFooting => {
            let s = m.strip_footing(id)?;
            elev(s.level)? + s.top_offset
        }
        Kind::Raft => {
            let s = m.raft(id)?;
            elev(s.level)? + s.top_offset
        }
        Kind::Brace => {
            let s = m.brace(id)?;
            elev(s.level)? + if end1 { s.b.2 } else { s.a.2 }
        }
        Kind::Truss => {
            let s = m.truss(id)?;
            elev(s.level)? + if end1 { s.b.2 } else { s.a.2 }
        }
        Kind::BeamSystem => {
            let s = m.beam_system(id)?;
            elev(s.level)? + s.z
        }
        Kind::Ramp => {
            let s = m.ramp(id)?;
            elev(s.level)? + s.base_offset
        }
        Kind::Shaft | Kind::Zone => return super::zone_shaft::plan_z(m, g, elev),
        Kind::Room | Kind::RoomSep => return super::room_fam::plan_z(m, g, elev),
        _ => 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::{fixture, Fx};

    const TABLES: [(Kind, &str); 7] = [
        (Kind::Footing, "iso_footing"),
        (Kind::StripFooting, "strip_footing"),
        (Kind::Raft, "raft"),
        (Kind::Brace, "brace"),
        (Kind::Truss, "truss"),
        (Kind::BeamSystem, "beam_system"),
        (Kind::Ramp, "ramp"),
    ];

    #[test]
    fn every_family_has_a_move_grip_that_changes_the_spec() {
        let mut fx = fixture();
        for (kind, table) in TABLES {
            let id = fx.ids[table];
            let all = crate::grips::grips(&fx.ed.model, kind, id).unwrap();
            let mv = all
                .iter()
                .find(|g| g.role == Role::Move)
                .unwrap_or_else(|| panic!("{table}: no move grip"))
                .clone();
            let before = format!("{:?}", compute(&fx.ed.model, &mv, mv.pos).unwrap());
            let to = mv.pos + v2(1.5, 0.5);
            crate::grips::apply(&mut fx.ed.model, &mv, to).unwrap();
            let after = format!("{:?}", compute(&fx.ed.model, &mv, mv.pos).unwrap());
            assert_ne!(before, after, "{table}: the drag changed nothing");
            fx.ed.model.undo().unwrap();
        }
    }

    fn five() -> Vec<(f64, f64)> {
        vec![(200.0, 0.0), (206.0, 0.0), (206.0, 6.0), (203.0, 9.0), (200.0, 6.0)]
    }

    #[test]
    fn raft_vertex_del_keeps_raft() {
        let mut fx = fixture();
        let id = fx.ids["raft"];
        let mut s = fx.ed.model.raft(id).unwrap();
        s.ring = five();
        fx.ed.model.update_raft(id, &s).unwrap();
        remove_vertex(&mut fx.ed.model, Kind::Raft, id, 3).unwrap();
        let r = fx.ed.model.raft(id).unwrap().ring;
        assert_eq!(r.len(), 4);
        assert!(!r.contains(&(203.0, 9.0)));
    }

    /// A free bar on the raft (plan ends in world coordinates) on a five-point outline.
    fn raft_with_bar(a: (f64, f64), b: (f64, f64)) -> (Fx, i64) {
        let mut fx = fixture();
        let id = fx.ids["raft"];
        let mut s = fx.ed.model.raft(id).unwrap();
        s.ring = five();
        fx.ed.model.update_raft(id, &s).unwrap();
        let rt = fx.ed.model.add_rebar_type("T8", 0.008, "A500", 0.032).unwrap();
        let sql = "INSERT INTO rebar_set(host_kind,host,layout,rtype,ax,ay,az,bx,by,bz) VALUES ('raft',?1,'free',?2,?3,?4,0.1,?5,?6,0.1)";
        fx.ed
            .model
            .sql()
            .execute(sql, rusqlite::params![id, rt, a.0, a.1, b.0, b.1])
            .unwrap();
        (fx, id)
    }

    fn vertex_grip(fx: &Fx, kind: Kind, id: i64, i: usize) -> Grip {
        crate::grips::grips(&fx.ed.model, kind, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == Role::Vertex(i))
            .unwrap()
    }

    /// The refusal of the raft's only rebar set (Error's display adds its own prefix).
    fn rebar_refusal(fx: &Fx, raft: i64) -> String {
        let sid = fx.ed.model.rebar_sets_of("raft", Some(raft)).unwrap()[0].0;
        Error::Degenerate(crate::tr("rbdrag.rebar", &[("id", &sid.to_string())])).to_string()
    }

    #[test]
    fn raft_vertex_del_refuses_rebar_outside() {
        let (mut fx, id) = raft_with_bar((203.0, 8.0), (203.0, 5.0));
        let e = remove_vertex(&mut fx.ed.model, Kind::Raft, id, 3).unwrap_err();
        assert_eq!(e.to_string(), rebar_refusal(&fx, id));
        assert_eq!(fx.ed.model.raft(id).unwrap().ring, five());
    }

    #[test]
    fn raft_vertex_del_ok_rebar_inside() {
        let (mut fx, id) = raft_with_bar((202.0, 2.0), (204.0, 4.0));
        remove_vertex(&mut fx.ed.model, Kind::Raft, id, 3).unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring.len(), 4);
        assert_eq!(fx.ed.model.rebar_sets_of("raft", Some(id)).unwrap().len(), 1);
    }

    #[test]
    fn raft_drag_with_rebar_one_undo() {
        let (mut fx, id) = raft_with_bar((202.0, 2.0), (204.0, 4.0));
        let g = vertex_grip(&fx, Kind::Raft, id, 0);
        crate::grips::apply(&mut fx.ed.model, &g, g.pos + v2(-1.0, -1.0)).unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring[0], (199.0, -1.0));
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring, five());
        assert_eq!(fx.ed.model.rebar_sets_of("raft", Some(id)).unwrap().len(), 1);
    }

    #[test]
    fn raft_drag_refuses_rebar_outside() {
        // pulling the top vertex below the bar is refused and changes nothing
        let (mut fx, id) = raft_with_bar((203.0, 8.0), (203.0, 5.0));
        let g = vertex_grip(&fx, Kind::Raft, id, 3);
        let e = crate::grips::apply(&mut fx.ed.model, &g, v2(203.0, 4.0)).unwrap_err();
        assert_eq!(e.to_string(), rebar_refusal(&fx, id));
        assert_eq!(fx.ed.model.raft(id).unwrap().ring, five());
    }

    #[test]
    fn raft_drag_refuses_beam_outside() {
        let mut fx = fixture();
        let id = fx.ids["raft"];
        let mut s = fx.ed.model.raft(id).unwrap();
        s.ring = five();
        fx.ed.model.update_raft(id, &s).unwrap();
        let mut b = fx.ed.model.beam(fx.ids["beam"]).unwrap();
        b.level = s.level;
        b.a = (200.0, 0.0, 0.0);
        b.b = (206.0, 0.0, 0.0);
        fx.ed.model.add_beam(&b).unwrap();
        let g = vertex_grip(&fx, Kind::Raft, id, 1);
        let e = crate::grips::apply(&mut fx.ed.model, &g, v2(203.0, 3.0)).unwrap_err();
        assert_eq!(e.to_string(), Error::Degenerate(crate::tr("rbdrag.beam", &[])).to_string());
        assert_eq!(fx.ed.model.raft(id).unwrap().ring, five());
    }

    #[test]
    fn beam_system_drag_refuses_opening_outside_then_allows_outward() {
        let mut fx = fixture();
        let id = fx.ids["beam_system"];
        let mut s = fx.ed.model.beam_system(id).unwrap();
        let p = s.ring[0];
        s.holes = vec![vec![
            (p.0 + 0.5, p.1 + 0.5),
            (p.0 + 2.0, p.1 + 0.5),
            (p.0 + 2.0, p.1 + 2.0),
            (p.0 + 0.5, p.1 + 2.0),
        ]];
        fx.ed.model.update_beam_system(id, &s).unwrap();
        let g = vertex_grip(&fx, Kind::BeamSystem, id, 0);
        let e = crate::grips::apply(&mut fx.ed.model, &g, g.pos + v2(3.0, 3.0)).unwrap_err();
        assert_eq!(e.to_string(), Error::Degenerate(crate::tr("rbdrag.opening", &[])).to_string());
        assert_eq!(fx.ed.model.beam_system(id).unwrap().ring, s.ring);
        // moving the corner outward keeps the opening inside: allowed, one undo step
        crate::grips::apply(&mut fx.ed.model, &g, g.pos + v2(-1.0, -1.0)).unwrap();
        assert_ne!(fx.ed.model.beam_system(id).unwrap().ring, s.ring);
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.beam_system(id).unwrap().ring, s.ring);
    }

    #[test]
    fn refusal_keys_exist_in_both_languages() {
        use ddc_ui::i18n::Lang;
        for key in ["rbdrag.beam", "rbdrag.rebar", "rbdrag.opening"] {
            let en = crate::tr_in(Lang::En, key, &[("id", "7")]);
            let ru = crate::tr_in(Lang::Ru, key, &[("id", "7")]);
            assert_ne!(en, key, "{key}: missing in en");
            assert_ne!(ru, key, "{key}: missing in ru");
            assert_ne!(en, ru, "{key}: ru equals en");
            assert!(!en.contains('{') && !ru.contains('{'), "{key}: unfilled argument");
        }
        assert!(crate::tr_in(Lang::En, "rbdrag.rebar", &[("id", "7")]).contains('7'));
        assert!(crate::tr_in(Lang::Ru, "rbdrag.rebar", &[("id", "7")]).contains('7'));
    }

    #[test]
    fn beam_system_drag_refuses_beam_outside() {
        let mut fx = fixture();
        let id = fx.ids["beam_system"];
        let s = fx.ed.model.beam_system(id).unwrap();
        let mut b = fx.ed.model.beam(fx.ids["beam"]).unwrap();
        b.level = s.level;
        let (p, q) = (s.ring[0], s.ring[1]);
        b.a = (p.0, p.1, 0.0);
        b.b = (q.0, q.1, 0.0);
        fx.ed.model.add_beam(&b).unwrap();
        let g = vertex_grip(&fx, Kind::BeamSystem, id, 0);
        let far = g.pos + (pv(s.ring[2]) - g.pos) * 0.5;
        assert!(crate::grips::apply(&mut fx.ed.model, &g, far).is_err());
        assert_eq!(fx.ed.model.beam_system(id).unwrap().ring, s.ring);
    }

    #[test]
    fn beam_system_three_points_refuses() {
        let mut fx = fixture();
        let id = fx.ids["beam_system"];
        let mut s = fx.ed.model.beam_system(id).unwrap();
        s.ring = vec![(200.0, 0.0), (206.0, 0.0), (200.0, 6.0)];
        fx.ed.model.update_beam_system(id, &s).unwrap();
        let e = remove_vertex(&mut fx.ed.model, Kind::BeamSystem, id, 0).unwrap_err();
        assert!(e.to_string().contains("three points"));
        assert_eq!(fx.ed.model.beam_system(id).unwrap().ring.len(), 3);
    }

    #[test]
    fn beam_system_start_edge_follows_vertex_del() {
        let mut fx = fixture();
        let id = fx.ids["beam_system"];
        let mut s = fx.ed.model.beam_system(id).unwrap();
        s.start_edge = Some(2);
        fx.ed.model.update_beam_system(id, &s).unwrap();
        remove_vertex(&mut fx.ed.model, Kind::BeamSystem, id, 1).unwrap();
        let t = fx.ed.model.beam_system(id).unwrap();
        assert_eq!(t.ring[1], s.ring[2]);
        assert_eq!(t.start_edge, Some(1));
    }

    #[test]
    fn hosted_beam_outside_refuses() {
        crate::lang_on_this_thread(false);
        let mut fx = fixture();
        let id = fx.ids["raft"];
        let mut s = fx.ed.model.raft(id).unwrap();
        s.ring = five();
        fx.ed.model.update_raft(id, &s).unwrap();
        let mut b = fx.ed.model.beam(fx.ids["beam"]).unwrap();
        b.level = s.level;
        b.a = (203.0, 8.0, 0.0);
        b.b = (203.0, 5.0, 0.0);
        fx.ed.model.add_beam(&b).unwrap();
        let e = remove_vertex(&mut fx.ed.model, Kind::Raft, id, 3).unwrap_err();
        assert!(e.to_string().contains("hosted beam"));
        assert_eq!(fx.ed.model.raft(id).unwrap().ring.len(), 5);
    }

    #[test]
    fn one_undo_restores() {
        let mut fx = fixture();
        let id = fx.ids["raft"];
        let before = fx.ed.model.raft(id).unwrap().ring;
        let ins = crate::grips::grips(&fx.ed.model, Kind::Raft, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == Role::Insert(0))
            .unwrap();
        crate::grips::apply(&mut fx.ed.model, &ins, ins.pos + v2(0.0, -0.5)).unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring.len(), before.len() + 1);
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring, before);
        remove_vertex(&mut fx.ed.model, Kind::Raft, id, 1).unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring.len(), before.len() - 1);
        fx.ed.model.undo().unwrap();
        assert_eq!(fx.ed.model.raft(id).unwrap().ring, before);
    }

    #[test]
    fn a_degenerate_strip_footing_drag_is_refused() {
        let fx = fixture();
        let id = fx.ids["strip_footing"];
        let all = crate::grips::grips(&fx.ed.model, Kind::StripFooting, id).unwrap();
        let end = all.iter().find(|g| g.role == Role::End(1)).unwrap();
        let a = fx.ed.model.strip_footing(id).unwrap().a;
        assert!(compute(&fx.ed.model, end, v2(a.0, a.1)).is_err());
    }
}
