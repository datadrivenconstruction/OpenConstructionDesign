//! "Clean up outline" for ceilings and shafts saved with a spike (a near-collinear back turn,
//! refused in new outlines since f16c): a read-only preview and the clean-up as one undo step.
//! Floors have theirs in `slabs`. Runs only on request, never on open.

use crate::ops::Inv;
use crate::{Error, Model};

/// Rings without spikes and the number of spikes removed.
pub type Cleanup = (Vec<Vec<(f64, f64)>>, usize);

impl Model {
    /// Preview of the ceiling clean-up; None when its outline has no spike. Reads only.
    pub fn ceiling_outline_cleanup(&self, id: i64) -> Result<Option<Cleanup>, Error> {
        Ok(crate::slabs::cleaned(&crate::shafts::ceiling_rings(
            &self.db, id,
        )?))
    }

    /// Drops the spikes of a saved ceiling outline as one undo step; the count removed.
    pub fn clean_ceiling_outline(&mut self, id: i64) -> Result<usize, Error> {
        let before = crate::shafts::ceiling_rings(&self.db, id)?;
        let Some((rs, n)) = crate::slabs::cleaned(&before) else {
            return Ok(0);
        };
        self.apply_ceiling_outline_cleanup(id, &before, &rs).map(|()| n)
    }

    /// Writes exactly the previewed `after` rings of a ceiling as one undo step; refused when
    /// its outline is no longer `before` or `after` is not its clean-up.
    pub fn apply_ceiling_outline_cleanup(
        &mut self,
        id: i64,
        before: &[Vec<(f64, f64)>],
        after: &[Vec<(f64, f64)>],
    ) -> Result<(), Error> {
        let now = crate::shafts::ceiling_rings(&self.db, id)?;
        crate::slabs::preview_holds("ceiling", id, &now, before, after)?;
        let p = crate::slabs::profile(&after[0], &after[1..])?;
        self.op("ceiling_edit", |db| {
            let inv = Inv::default().delete("ceiling_vertex", "ceiling", id).restore(
                db,
                "ceiling_vertex",
                "ceiling",
                id,
            )?;
            crate::shafts::write_ceiling_rings(db, id, &p)?;
            Ok(((), id, inv))
        })
    }

    /// Preview of the shaft clean-up; None when its outline has no spike. Reads only.
    pub fn shaft_outline_cleanup(&self, id: i64) -> Result<Option<Cleanup>, Error> {
        Ok(crate::slabs::cleaned(&[crate::shafts::ring_of(&self.db, id)?]))
    }

    /// Drops the spikes of a saved shaft outline as one undo step; the count removed.
    pub fn clean_shaft_outline(&mut self, id: i64) -> Result<usize, Error> {
        let before = vec![crate::shafts::ring_of(&self.db, id)?];
        let Some((rs, n)) = crate::slabs::cleaned(&before) else {
            return Ok(0);
        };
        self.apply_shaft_outline_cleanup(id, &before, &rs).map(|()| n)
    }

    /// Writes exactly the previewed `after` ring of a shaft as one undo step; refused when its
    /// outline is no longer `before` or `after` is not its clean-up.
    pub fn apply_shaft_outline_cleanup(
        &mut self,
        id: i64,
        before: &[Vec<(f64, f64)>],
        after: &[Vec<(f64, f64)>],
    ) -> Result<(), Error> {
        let now = [crate::shafts::ring_of(&self.db, id)?];
        crate::slabs::preview_holds("shaft", id, &now, before, after)?;
        let ring = after[0].clone();
        crate::slabs::profile(&ring, &[])?;
        self.op("shaft_edit", |db| {
            let inv = Inv::default().delete("shaft_vertex", "shaft", id).restore(
                db,
                "shaft_vertex",
                "shaft",
                id,
            )?;
            crate::shafts::write_shaft_ring(db, id, &ring)?;
            Ok(((), id, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{CeilingSpec, LayerSpec, Model, ShaftSpec, SlabSpec};
    use rusqlite::{params, Connection};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    /// The 45-degree snap spike of creation fuzz seed 3 (tip at index 1), CCW.
    fn spiked() -> Vec<(f64, f64)> {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let (cx, cy) = (4.0 - s * 1.5, 4.0 - s * 1.5);
        vec![(1., 1.), (4., 4.), (cx, cy + 1e-15), (0., 3.)]
    }

    fn shifted(r: &[(f64, f64)], dx: f64, dy: f64) -> Vec<(f64, f64)> {
        r.iter().map(|p| (p.0 + dx, p.1 + dy)).collect()
    }

    /// Levels L0 (0 m) and L1 (3 m) and a one-layer floor type.
    fn base(m: &mut Model) -> (i64, i64, i64) {
        let layer = LayerSpec {
            material: "concrete".into(),
            thickness: 0.2,
        };
        let t = m.add_slab_type("floor", &[layer]).unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        (l0, l1, t)
    }

    fn ceiling(lv: i64, t: i64, outer: Vec<(f64, f64)>) -> CeilingSpec {
        CeilingSpec {
            level: lv,
            slab_type: t,
            height: 2.6,
            outer,
            holes: vec![],
        }
    }

    fn shaft(l0: i64, l1: i64, ring: Vec<(f64, f64)>) -> ShaftSpec {
        ShaftSpec {
            level: l0,
            top_level: l1,
            base_offset: 0.0,
            top_offset: 0.0,
            ring,
        }
    }

    /// Writes `ring` as the saved outline past every check, as a file from before f16c.
    fn write_legacy(db: &Connection, sql: &str, id: i64, ring: &[(f64, f64)]) {
        for (i, p) in ring.iter().enumerate() {
            db.execute(sql, params![id, i as i64, p.0, p.1]).unwrap();
        }
    }

    fn legacy_ceiling(m: &mut Model, lv: i64, t: i64) -> i64 {
        let c = m.add_ceiling(&ceiling(lv, t, rect(0., 0., 4., 4.))).unwrap();
        m.db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [c]).unwrap();
        let sql = "INSERT INTO ceiling_vertex VALUES (?1,0,?2,?3,?4)";
        write_legacy(&m.db, sql, c, &spiked());
        assert!(crate::slabs::profile(&spiked(), &[]).is_err(), "the fixture is a spike");
        c
    }

    fn legacy_shaft(m: &mut Model, l0: i64, l1: i64) -> i64 {
        let s = m.add_shaft(&shaft(l0, l1, rect(20., 0., 22., 2.))).unwrap();
        m.db.execute("DELETE FROM shaft_vertex WHERE shaft=?1", [s]).unwrap();
        let sql = "INSERT INTO shaft_vertex VALUES (?1,?2,?3,?4)";
        write_legacy(&m.db, sql, s, &shaft_ring());
        s
    }

    /// The spike away from the floor and ceiling fixtures, so the shaft cuts neither.
    fn shaft_ring() -> Vec<(f64, f64)> {
        shifted(&spiked(), 20.0, 0.0)
    }

    fn legacy_floor(m: &mut Model, lv: i64, t: i64) -> i64 {
        let spec = SlabSpec {
            level: lv,
            slab_type: t,
            offset: 0.0,
            outer: rect(0., 0., 4., 4.),
            holes: vec![],
        };
        let s = m.add_slab(&spec).unwrap();
        m.db.execute("DELETE FROM slab_vertex WHERE slab=?1", [s]).unwrap();
        let sql = "INSERT INTO slab_vertex VALUES (?1,0,?2,?3,?4)";
        write_legacy(&m.db, sql, s, &spiked());
        s
    }

    #[test]
    fn legacy_ceiling_draws_counts_and_stays_editable() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, _, t) = base(&mut m);
        let c = legacy_ceiling(&mut m, l0, t);
        assert!(m.ceiling_mesh(c).is_ok(), "a saved spike still draws");
        assert!(!m.ceiling_view_rings(c).unwrap().is_empty());
        // a property edit keeps the outline: allowed, and the quantities come back
        let mut s = ceiling(l0, t, spiked());
        s.height = 2.4;
        m.update_ceiling(c, &s).unwrap();
        assert_eq!(m.ceiling_quantities(c).unwrap().len(), 1, "it still counts");
        // moving the whole ceiling carries the old spike along: allowed
        s.outer = shifted(&spiked(), 1.5, -0.5);
        m.update_ceiling(c, &s).unwrap();
        assert_eq!(m.ceiling_rings(c).unwrap()[0].len(), 4);
        m.assert_integrity();
    }

    #[test]
    fn new_ceiling_spike_is_refused() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, _, t) = base(&mut m);
        let r = m.add_ceiling(&ceiling(l0, t, spiked()));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        let c = m.add_ceiling(&ceiling(l0, t, rect(0., 0., 4., 4.))).unwrap();
        let r = m.update_ceiling(c, &ceiling(l0, t, spiked()));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        assert_eq!(m.ceiling_rings(c).unwrap()[0], rect(0., 0., 4., 4.));
        // a legacy ceiling: a second spike, or the tip pulled out, is new
        let l = legacy_ceiling(&mut m, l0, t);
        let mut tip = spiked();
        tip[1] = (5., 5.);
        let r = m.update_ceiling(l, &ceiling(l0, t, tip));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        assert_eq!(m.ceiling_rings(l).unwrap()[0], spiked());
    }

    /// A copy of a legacy ceiling (moved, turned, mirrored) keeps the source's spike; the same
    /// outline as a new ceiling, or a copy of a clean source, is still refused.
    #[test]
    fn copy_of_a_legacy_ceiling_keeps_its_spike() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, _, t) = base(&mut m);
        let c = legacy_ceiling(&mut m, l0, t);
        let turned: Vec<(f64, f64)> = spiked().iter().map(|p| (-p.1 + 10.0, p.0)).collect();
        let mut mirrored: Vec<(f64, f64)> = spiked().iter().map(|p| (-p.0 - 10.0, p.1)).collect();
        mirrored.reverse();
        for ring in [shifted(&spiked(), 6.0, 0.0), turned, mirrored] {
            let n = m.add_ceiling_copy(c, &ceiling(l0, t, ring.clone())).unwrap();
            assert_eq!(m.ceiling_rings(n).unwrap()[0].len(), 4, "the spike came along");
            assert!(m.ceiling_outline_cleanup(n).unwrap().is_some());
            assert!(m.ceiling_mesh(n).is_ok());
            let r = m.add_ceiling(&ceiling(l0, t, ring));
            assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        }
        let clean = m.add_ceiling(&ceiling(l0, t, rect(0., 0., 4., 4.))).unwrap();
        let r = m.add_ceiling_copy(clean, &ceiling(l0, t, spiked()));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        assert_eq!(m.ceiling_rings(c).unwrap()[0], spiked(), "the source is untouched");
        m.assert_integrity();
    }

    /// A spike of the saved size somewhere else passes only when the whole outline
    /// moved or the same vertex carries it; a reshape that recreates it elsewhere is refused.
    #[test]
    fn a_same_size_spike_elsewhere_is_new_unless_the_outline_moved() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, _, t) = base(&mut m);
        let c = legacy_ceiling(&mut m, l0, t);
        // mirror and negation are exact, so the fixture's 1e-15 offset survives
        let flip = |r: &[(f64, f64)]| r.iter().map(|p| (-p.0, p.1)).collect::<Vec<_>>();
        // one more vertex: not the saved outline moved, so the mirrored spike is new
        let mut reshaped = spiked();
        reshaped.push((0.2, 1.5));
        let r = m.update_ceiling(c, &ceiling(l0, t, flip(&reshaped)));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        assert_eq!(m.ceiling_rings(c).unwrap()[0], spiked(), "nothing was written");
        // the same reshape with the tip where it was keeps the saved spike
        m.update_ceiling(c, &ceiling(l0, t, reshaped)).unwrap();
        assert_eq!(m.ceiling_rings(c).unwrap()[0].len(), 5);
        // a copy of that five-vertex ceiling as the mirrored four-vertex spike is refused
        let r = m.add_ceiling_copy(c, &ceiling(l0, t, flip(&spiked())));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        // the spike's own vertices dragged along its line (walls moved): the same spike
        let other = legacy_ceiling(&mut m, l0, t);
        let mut dragged = spiked();
        for p in dragged.iter_mut().take(3) {
            *p = (p.0 + 0.5, p.1 + 0.5);
        }
        m.update_ceiling(other, &ceiling(l0, t, dragged.clone())).unwrap();
        // a whole move of that outline still carries it
        m.update_ceiling(other, &ceiling(l0, t, shifted(&dragged, -3.0, 0.0))).unwrap();
        assert_eq!(m.ceiling_rings(other).unwrap()[0].len(), 4);
        m.assert_integrity();
    }

    /// A copy of a legacy shaft (moved, turned) keeps its spike; the same outline as a
    /// new shaft, or a copy of a clean shaft, is refused.
    #[test]
    fn copy_of_a_legacy_shaft_keeps_its_spike() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, l1, _) = base(&mut m);
        let s = legacy_shaft(&mut m, l0, l1);
        // a half turn about (24, 0) and a move within the same binade: exact, so the
        // fixture's 1e-15 offset survives
        let turned: Vec<(f64, f64)> = shaft_ring().iter().map(|p| (48.0 - p.0, -p.1)).collect();
        for ring in [shifted(&shaft_ring(), 6.0, 0.0), turned] {
            let n = m.add_shaft_copy(s, &shaft(l0, l1, ring.clone())).unwrap();
            assert_eq!(m.shaft_ring(n).unwrap().len(), 4, "the spike came along");
            assert!(m.shaft_outline_cleanup(n).unwrap().is_some());
            let r = m.add_shaft(&shaft(l0, l1, ring));
            assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        }
        let clean = m.add_shaft(&shaft(l0, l1, rect(40., 0., 42., 2.))).unwrap();
        let r = m.add_shaft_copy(clean, &shaft(l0, l1, shifted(&shaft_ring(), 6.0, 0.0)));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        assert_eq!(m.shaft_ring(s).unwrap(), shaft_ring(), "the source is untouched");
        m.assert_integrity();
    }

    #[test]
    fn ceiling_clean_up_is_previewed_valid_and_one_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, _, t) = base(&mut m);
        let c = legacy_ceiling(&mut m, l0, t);
        let (preview, n) = m.ceiling_outline_cleanup(c).unwrap().expect("a spike");
        assert_eq!(n, 1);
        assert_eq!(m.ceiling_rings(c).unwrap()[0], spiked(), "the preview writes nothing");
        assert_eq!(m.clean_ceiling_outline(c).unwrap(), 1);
        let after = m.ceiling_rings(c).unwrap();
        assert_eq!(after[0].len(), 3);
        assert!(crate::slabs::profile(&after[0], &after[1..]).is_ok());
        assert_eq!(after[0].len(), preview[0].len());
        assert!(m.ceiling_outline_cleanup(c).unwrap().is_none());
        assert_eq!(m.clean_ceiling_outline(c).unwrap(), 0, "nothing left to clean");
        m.undo().unwrap();
        assert_eq!(m.ceiling_rings(c).unwrap()[0], spiked(), "one undo brings it back");
    }

    #[test]
    fn legacy_shaft_draws_cuts_and_stays_editable() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, l1, _) = base(&mut m);
        let s = legacy_shaft(&mut m, l0, l1);
        assert_eq!(m.shaft_ring(s).unwrap(), shaft_ring());
        // a height edit keeps the outline: allowed
        let mut spec = shaft(l0, l1, shaft_ring());
        spec.top_offset = -0.5;
        m.update_shaft(s, &spec).unwrap();
        assert_eq!(m.shaft(s).unwrap().top_offset, -0.5);
        // a whole move carries the spike: allowed
        spec.ring = shifted(&shaft_ring(), 2.0, 0.0);
        m.update_shaft(s, &spec).unwrap();
        // a new shaft, or a pulled-out tip, is refused
        let r = m.add_shaft(&shaft(l0, l1, shaft_ring()));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        let mut tip = shifted(&shaft_ring(), 2.0, 0.0);
        tip[1] = (27., 5.);
        let r = m.update_shaft(s, &shaft(l0, l1, tip));
        assert!(matches!(r, Err(crate::Error::Geom(_))), "{r:?}");
        m.assert_integrity();
    }

    #[test]
    fn shaft_clean_up_is_previewed_valid_and_one_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, l1, _) = base(&mut m);
        let s = legacy_shaft(&mut m, l0, l1);
        let (preview, n) = m.shaft_outline_cleanup(s).unwrap().expect("a spike");
        assert_eq!((n, preview.len()), (1, 1));
        assert_eq!(m.shaft_ring(s).unwrap(), shaft_ring(), "the preview writes nothing");
        assert_eq!(m.clean_shaft_outline(s).unwrap(), 1);
        let after = m.shaft_ring(s).unwrap();
        assert_eq!(after.len(), 3);
        assert!(crate::slabs::profile(&after, &[]).is_ok());
        assert!(m.shaft_outline_cleanup(s).unwrap().is_none());
        m.undo().unwrap();
        assert_eq!(m.shaft_ring(s).unwrap(), shaft_ring(), "one undo brings it back");
    }

    #[test]
    fn legacy_floor_takes_openings_and_xlsx_outline_rows() {
        let mut m = Model::open(":memory:").unwrap();
        let (l0, _, t) = base(&mut m);
        let s = legacy_floor(&mut m, l0, t);
        // an opening in a legacy floor: allowed; a spiked opening is a new spike
        let hole = vec![(1.0, 2.0), (1.2, 2.0), (1.2, 2.2), (1.0, 2.2)];
        m.add_slab_opening(s, &hole).unwrap();
        let bad: Vec<(f64, f64)> = spiked()
            .iter()
            .map(|p| (0.5 + p.0 * 0.1, 2.5 + p.1 * 0.1))
            .collect();
        assert!(m.add_slab_opening(s, &bad).is_err());
        assert_eq!(m.slab_rings(s).unwrap().len(), 2);
        // an XLSX outline row with the saved outline unchanged applies
        let rs = m.slab_rings(s).unwrap();
        let e = crate::Edit::SlabBoundary {
            id: s,
            outer: rs[0].clone(),
            holes: rs[1..].to_vec(),
        };
        m.apply_edits(&[e]).unwrap();
        assert_eq!(m.slab_quantities(s).unwrap().len(), 1, "it still counts");
        m.assert_integrity();
    }

    /// A project saved before f16c with a spike in every kind of outline: it opens, nothing
    /// is lost, every element still draws and counts, and nothing is cleaned on its own.
    #[test]
    fn mixed_legacy_project_opens_with_every_element() {
        let d = std::env::temp_dir().join(format!("ddc-spike2-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("legacy.ddc");
        let _ = std::fs::remove_file(&p);
        let ps = p.to_str().unwrap();
        let (f, c, s) = {
            let mut m = Model::open(ps).unwrap();
            let (l0, l1, t) = base(&mut m);
            let f = legacy_floor(&mut m, l1, t);
            let c = legacy_ceiling(&mut m, l0, t);
            let s = legacy_shaft(&mut m, l0, l1);
            (f, c, s)
        };
        let m = Model::open(ps).unwrap();
        assert_eq!(m.slab_rings(f).unwrap()[0], spiked(), "floor kept as saved");
        assert_eq!(m.ceiling_rings(c).unwrap()[0], spiked(), "ceiling kept as saved");
        assert_eq!(m.shaft_ring(s).unwrap(), shaft_ring(), "shaft kept as saved");
        assert!(m.slab_mesh(f).is_ok(), "floor draws");
        assert!(m.ceiling_mesh(c).is_ok(), "ceiling draws");
        assert!(m.slab_outline_cleanup(f).unwrap().is_some(), "offered, not applied");
        assert!(m.ceiling_outline_cleanup(c).unwrap().is_some());
        assert!(m.shaft_outline_cleanup(s).unwrap().is_some());
        assert!(m.check_integrity().unwrap().is_empty(), "a saved spike is no fault");
        drop(m);
        let _ = std::fs::remove_file(&p);
    }
}
