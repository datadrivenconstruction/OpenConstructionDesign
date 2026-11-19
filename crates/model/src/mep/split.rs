//! Inserting a transition into a run: the run is cut at a point of its axis, the part after the
//! cut gets another size and the network puts a transition where the two parts meet. One journal
//! record; a cut the network cannot fit a fitting into is rolled back whole.

use ddc_geom::math::V3;
use ddc_geom::mep::{Domain, Shape};
use rusqlite::OptionalExtension;

use super::edit::with_dn;
use super::{invalid, p3, read_run, RunSpec, TOL};
use crate::{Error, Model};

/// Size of the part after the cut: a catalog DN for pipes, a section shape for the rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NewSize {
    Dn(u32),
    Shape(Shape),
}

/// Axis length of a run, m (the stored points carry the slope).
pub fn run_axis_length(s: &RunSpec) -> f64 {
    let p = s.pts();
    p.windows(2).map(|w| (w[1] - w[0]).len()).sum()
}

/// The point `at` metres along the axis: (segment index, point). None past either end.
fn point_along(pts: &[V3], at: f64) -> Option<(usize, V3)> {
    let mut run = 0.0;
    for (k, w) in pts.windows(2).enumerate() {
        let l = (w[1] - w[0]).len();
        if at <= run + l && l > 0.0 {
            return Some((k, w[0] + (w[1] - w[0]) * ((at - run) / l)));
        }
        run += l;
    }
    None
}

/// Both parts of a cut at `at`: the first keeps the start cap, the second the end cap.
fn halves(s: &RunSpec, at: f64) -> Result<(RunSpec, RunSpec), Error> {
    let pts = s.pts();
    let (k, p) = point_along(&pts, at).ok_or_else(|| Error::Invalid("mep.err.cut_ends".into()))?;
    // a cut on a bend has no straight joint for the transition
    if pts.iter().any(|q| (*q - p).len() < s.min_leg().max(TOL)) {
        return invalid("mep.err.cut_ends");
    }
    let mut a: Vec<V3> = pts[..=k].to_vec();
    a.push(p);
    let mut b = vec![p];
    b.extend_from_slice(&pts[k + 1..]);
    let part = |v: Vec<V3>, caps| RunSpec {
        points: v.into_iter().map(p3).collect(),
        slope: 0.0,
        caps,
        ..s.clone()
    };
    Ok((part(a, (s.caps.0, false)), part(b, (false, s.caps.1))))
}

fn resized(s: &RunSpec, size: NewSize) -> Result<RunSpec, Error> {
    match (s.section.domain, size) {
        (Domain::Pipe, NewSize::Dn(dn)) => with_dn(s, dn),
        (Domain::Pipe, NewSize::Shape(_)) | (_, NewSize::Dn(_)) => invalid("mep.err.no_size"),
        (_, NewSize::Shape(sh)) => {
            let (w, h) = sh.wh();
            if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
                return invalid("mep.err.no_size");
            }
            let mut r = s.clone();
            r.section.shape = sh;
            r.dn = None;
            Ok(r)
        }
    }
}

impl Model {
    /// Cut run `run` at `at` metres along its axis and give the part after the cut `size`: a
    /// transition appears at the cut. The run keeps its id for the first part; returns the id of
    /// the second. Refused for a device connection route, at a bend or end, for the same size,
    /// and when the network has no room for the fitting.
    pub fn mep_insert_transition(
        &mut self,
        run: i64,
        at: f64,
        size: NewSize,
    ) -> Result<i64, Error> {
        let s = read_run(&self.db, run)?;
        // a run that is a device's connection route belongs to that device's port
        let route_of: Option<i64> = self
            .db
            .query_row("SELECT id FROM mep_port WHERE stub=?1 LIMIT 1", [run], |r| r.get(0))
            .optional()?;
        if route_of.is_some() {
            return invalid("mep.err.cut_device");
        }
        if !at.is_finite() || at <= 0.0 {
            return invalid("mep.err.cut_ends");
        }
        let (a, b0) = halves(&s, at)?;
        let b = resized(&b0, size)?;
        if b.section.shape == s.section.shape && b.dn == s.dn {
            return invalid("mep.err.same_size");
        }
        self.mep_group("mep_transition", |m| {
            let was_ok = m.net_builds(s.system);
            m.update_mep_run(run, &a)?;
            let id = m.add_mep_run(&b)?;
            m.mep_valid(s.system, was_ok).map_err(|_| Error::Invalid("mep.err.no_room".into()))?;
            Ok(id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::mep::{FittingKind, PipeMaterial, Section, SystemKind};

    fn duct(m: &mut Model, pts: Vec<(f64, f64, f64)>) -> (i64, i64) {
        let l = m.add_level("L1", 0.0).unwrap();
        let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
        let sec = Section::new(Domain::Duct, Shape::Round { d: 0.4 });
        let r = m.add_mep_run(&RunSpec::new(sa, l, sec, pts)).unwrap();
        (sa, r)
    }

    #[test]
    fn transition_cuts_the_run_and_undoes_in_one_step() {
        let mut m = Model::open(":memory:").unwrap();
        let (sa, r) = duct(&mut m, vec![(0.0, 0.0, 3.0), (8.0, 0.0, 3.0)]);
        let ops = m.op_top().unwrap();
        let b = m
            .mep_insert_transition(r, 5.0, NewSize::Shape(Shape::Round { d: 0.25 }))
            .unwrap();
        assert_eq!(m.op_top().unwrap(), ops + 1, "one journal record");
        let (ra, rb) = (m.mep_run(r).unwrap(), m.mep_run(b).unwrap());
        assert_eq!(ra.points.last().unwrap().0, 5.0);
        assert_eq!(rb.points[0].0, 5.0);
        assert_eq!(rb.section.shape, Shape::Round { d: 0.25 });
        let net = m.mep_network(Some(sa)).unwrap();
        assert_eq!(net.count(FittingKind::Transition), 1);
        m.undo().unwrap();
        assert_eq!(m.mep_runs(Some(sa)).unwrap(), vec![r]);
        assert_eq!(m.mep_run(r).unwrap().points.len(), 2);
        assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Transition), 0);
    }

    #[test]
    fn caps_stay_on_their_ends() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, r) = duct(&mut m, vec![(0.0, 0.0, 3.0), (8.0, 0.0, 3.0)]);
        m.mep_cap_end(r, 1, true).unwrap();
        let b = m
            .mep_insert_transition(r, 4.0, NewSize::Shape(Shape::Rect { w: 0.3, h: 0.2 }))
            .unwrap();
        assert_eq!(m.mep_run(r).unwrap().caps, (false, false));
        assert_eq!(m.mep_run(b).unwrap().caps, (false, true));
    }

    #[test]
    fn cuts_at_an_end_a_bend_or_to_the_same_size_are_refused() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, r) = duct(&mut m, vec![(0.0, 0.0, 3.0), (6.0, 0.0, 3.0), (6.0, 6.0, 3.0)]);
        let small = NewSize::Shape(Shape::Round { d: 0.2 });
        let ops = m.op_top().unwrap();
        for at in [-1.0, 0.05, 6.0, 11.95, 40.0, f64::NAN] {
            let e = m.mep_insert_transition(r, at, small).unwrap_err();
            assert!(e.to_string().contains("mep.err.cut_ends"), "{at}: {e}");
        }
        let same = NewSize::Shape(Shape::Round { d: 0.4 });
        let e = m.mep_insert_transition(r, 3.0, same).unwrap_err();
        assert!(e.to_string().contains("mep.err.same_size"), "{e}");
        let e = m.mep_insert_transition(r, 3.0, NewSize::Dn(50)).unwrap_err();
        assert!(e.to_string().contains("mep.err.no_size"), "{e}");
        assert_eq!(m.op_top().unwrap(), ops, "nothing written");
    }

    #[test]
    fn pipe_takes_a_catalog_size() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let hw = m.add_mep_system("HW", SystemKind::Heating).unwrap();
        let s = RunSpec::pipe(hw, l, PipeMaterial::Steel, 100, vec![
            (0.0, 0.0, 3.0),
            (6.0, 0.0, 3.0),
        ])
        .unwrap();
        let r = m.add_mep_run(&s).unwrap();
        let b = m.mep_insert_transition(r, 3.0, NewSize::Dn(50)).unwrap();
        let rb = m.mep_run(b).unwrap();
        assert_eq!(rb.dn, Some(50));
        assert_eq!(rb.section.shape, Shape::Round { d: PipeMaterial::Steel.by_dn(50).unwrap().od });
        assert!(m.mep_insert_transition(r, 1.0, NewSize::Dn(7)).is_err());
    }
}
