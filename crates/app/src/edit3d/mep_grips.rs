//! Network-run grips in the shared 3D grip system: axis vertices (`Role3::Ext(k)`, Ctrl for
//! vertical) and whole-run move (`Role3::Ext(MOVE)`). The result is `mep3d::dragged` (an f64 shift
//! added to the previous points) as one `update_mep_run` operation; copy-by-move uses the gizmo (Alt).

use ddc_model::{Error, Model};
use ddc_render::glam::Vec3;

use super::grips::{Axis3, Grip3, Provider, Role3};
use super::pick::{Cat, Target};
use crate::grips3d::GripKind;

/// Role number of the run-move grip.
pub const MOVE: u32 = u32::MAX;

pub struct MepGrips;

fn kind(r: &Role3) -> Option<GripKind> {
    match r {
        Role3::Ext(MOVE) => Some(GripKind::Move),
        Role3::Ext(k) => Some(GripKind::Vertex(*k as usize)),
        _ => None,
    }
}

impl Provider for MepGrips {
    fn key(&self) -> &'static str {
        "mep"
    }

    fn owns(&self, t: Target) -> bool {
        t.cat == Cat::Mep
    }

    fn grips(&self, m: &Model, z0: f64, t: Target) -> Result<Vec<Grip3>, Error> {
        let s = m.mep_run(t.id)?;
        let p = |q: &(f64, f64, f64)| Vec3::new(q.0 as f32, q.1 as f32, (q.2 - z0) as f32);
        let mut out: Vec<Grip3> = s
            .points
            .iter()
            .enumerate()
            .map(|(k, q)| {
                let pos = p(q);
                // the dimension is measured from the neighboring axis point
                let anchor = s
                    .points
                    .get(k.wrapping_sub(1))
                    .or(s.points.get(k + 1))
                    .map_or(pos, p);
                Grip3 {
                    target: t,
                    role: Role3::Ext(k as u32),
                    pos,
                    axis: Axis3::Plane(Vec3::Z),
                    anchor,
                }
            })
            .collect();
        if s.points.len() >= 2 {
            let pos = (p(&s.points[0]) + p(&s.points[1])) * 0.5;
            out.push(Grip3 {
                target: t,
                role: Role3::Ext(MOVE),
                pos,
                axis: Axis3::Plane(Vec3::Z),
                anchor: pos,
            });
        }
        Ok(out)
    }

    fn apply(&self, m: &mut Model, _z0: f64, g: &Grip3, to: Vec3) -> Result<(), Error> {
        let k = kind(&g.role).ok_or_else(|| Error::Invalid("e3d.err.unsupported".into()))?;
        let s = m.mep_run(g.target.id)?;
        if let GripKind::Vertex(i) = k {
            if i >= s.points.len() {
                return Err(Error::Invalid("e3d.err.unsupported".into()));
            }
        }
        let s = crate::mep3d::dragged(&s, k, g.pos, to);
        m.mep_edit_run(g.target.id, &s)
    }

    fn preview(&self, m: &Model, z0: f64, g: &Grip3, to: Vec3) -> Result<Vec<Vec<Vec3>>, Error> {
        let Some(k) = kind(&g.role) else {
            return Ok(vec![]);
        };
        let s = crate::mep3d::dragged(&m.mep_run(g.target.id)?, k, g.pos, to);
        let mut out = vec![s
            .points
            .iter()
            .map(|q| Vec3::new(q.0 as f32, q.1 as f32, (q.2 - z0) as f32))
            .collect()];
        // connected runs follow: ghosts only, the model is written on release
        out.extend(crate::mep_follow::run_ghosts(m, g.target.id, &s, z0)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit3d::grips;

    fn run(ed: &mut crate::editor::Editor) -> i64 {
        crate::mep3d::demo(ed).unwrap();
        let mut ids: Vec<i64> = ed.model.mep_run_info().unwrap().keys().copied().collect();
        ids.sort_unstable();
        // flat and free: a branch tees into the main duct, which would block a lifted vertex
        ids.into_iter()
            .rev()
            .find(|&i| ed.model.mep_run(i).unwrap().slope == 0.0)
            .expect("a flat run")
    }

    #[test]
    fn mep_grips_go_through_the_shared_provider() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let id = run(&mut ed);
        let t = Target::new(Cat::Mep, id);
        let p = grips::provider_for(t).expect("mep provider");
        assert_eq!(
            p.key(),
            "mep",
            "one 3D grip system: the run is owned by the adapter"
        );
        let gs = grips::grips_of(&ed.model, ed.elevation, t);
        let before = ed.model.mep_run(id).unwrap();
        assert_eq!(gs.len(), before.points.len() + 1);
        let v0 = gs.iter().find(|g| g.role == Role3::Ext(0)).unwrap().clone();
        assert!(v0.lifts(), "Ctrl lifts a run vertex");
        // vertex: an f64 shift added to the previous point; one operation is one undo step
        let z0 = ed.elevation;
        let to = v0.pos + Vec3::new(0.5, 0.0, 0.25);
        let g = v0.clone();
        ed.run_op(crate::editor::OpFn(std::sync::Arc::new(
            move |m: &mut Model| grips::apply(m, z0, &g, to),
        )))
        .unwrap();
        let after = ed.model.mep_run(id).unwrap();
        assert!((after.points[0].0 - before.points[0].0 - 0.5).abs() < 1e-6);
        assert!((after.points[0].2 - before.points[0].2 - 0.25).abs() < 1e-6);
        assert_eq!(after.points[1], before.points[1]);
        ed.undo();
        assert_eq!(ed.model.mep_run(id).unwrap().points, before.points);
        // whole-run move
        let mv = gs
            .iter()
            .find(|g| g.role == Role3::Ext(MOVE))
            .unwrap()
            .clone();
        let to = mv.pos + Vec3::new(0.0, 1.0, 0.0);
        grips::apply(&mut ed.model, z0, &mv, to).unwrap();
        let moved = ed.model.mep_run(id).unwrap();
        for (a, b) in moved.points.iter().zip(&before.points) {
            assert!((a.1 - b.1 - 1.0).abs() < 1e-6 && (a.0 - b.0).abs() < 1e-9);
        }
    }
}
