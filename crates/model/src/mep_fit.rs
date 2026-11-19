// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fittings as a derived family: the network computes them from its runs, so a fitting has no
//! row. Its key is stable while the runs and the fitting position stay put.

use std::collections::HashMap;

use ddc_geom::mep::FittingKind;

use crate::{Error, Model};

/// A fitting of the computed network with a stable key and the level of its first run.
#[derive(Clone, Debug, PartialEq)]
pub struct FittingRef {
    pub key: i64,
    pub kind: FittingKind,
    pub at: (f64, f64, f64),
    /// Sorted, without repeats.
    pub runs: Vec<i64>,
    pub size: String,
    pub level: i64,
}

/// Key from kind, sorted runs and position in millimetres (FNV-1a, always positive).
pub fn fitting_key(kind: &FittingKind, at: (f64, f64, f64), runs: &[i64]) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(kind.name().as_bytes());
    let mut rs = runs.to_vec();
    rs.sort_unstable();
    rs.dedup();
    for r in rs {
        eat(&r.to_le_bytes());
    }
    for c in [at.0, at.1, at.2] {
        eat(&((c * 1000.0).round() as i64).to_le_bytes());
    }
    (h & 0x7FFF_FFFF_FFFF_FFFF) as i64
}

impl Model {
    /// Fittings of the network, all or those whose first run is on `level`, in a stable order.
    pub fn mep_fittings(&self, level: Option<i64>) -> Result<Vec<FittingRef>, Error> {
        if self.mep_runs(None)?.is_empty() {
            return Ok(vec![]);
        }
        let levels: HashMap<i64, i64> = {
            let mut st = self.db.prepare_cached("SELECT id, level FROM mep_run")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let net = self.mep_network(None)?;
        let mut out: Vec<FittingRef> = net
            .fittings
            .iter()
            .filter_map(|f| {
                let mut runs = f.runs.clone();
                runs.sort_unstable();
                runs.dedup();
                let lv = *levels.get(runs.first()?)?;
                (level.is_none_or(|l| l == lv)).then(|| FittingRef {
                    key: fitting_key(&f.kind, f.at, &runs),
                    kind: f.kind.clone(),
                    at: f.at,
                    runs,
                    size: f.size.clone(),
                    level: lv,
                })
            })
            .collect();
        out.sort_by_key(|f| f.key);
        Ok(out)
    }

    /// One fitting by key, None when the network no longer has it.
    pub fn mep_fitting(&self, key: i64) -> Result<Option<FittingRef>, Error> {
        Ok(self.mep_fittings(None)?.into_iter().find(|f| f.key == key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PipeMaterial, RunSpec, SystemKind};

    fn elbow() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let s = m.add_mep_system("HW", SystemKind::Heating).unwrap();
        let spec = RunSpec::pipe(
            s,
            l,
            PipeMaterial::Steel,
            100,
            vec![(0.0, 0.0, 1.0), (3.0, 0.0, 1.0), (3.0, 4.0, 1.0)],
        )
        .unwrap();
        let r = m.add_mep_run(&spec).unwrap();
        (m, l, r)
    }

    #[test]
    fn key_ignores_run_order_and_kind_matters() {
        let at = (1.0, 2.0, 3.0);
        let a = fitting_key(&FittingKind::Tee, at, &[5, 3, 3]);
        assert_eq!(a, fitting_key(&FittingKind::Tee, at, &[3, 5]));
        assert_ne!(a, fitting_key(&FittingKind::Cross, at, &[3, 5]));
        assert!(a > 0);
    }

    #[test]
    fn elbow_is_listed_with_level_and_stays_stable() {
        let (m, l, r) = elbow();
        let all = m.mep_fittings(None).unwrap();
        assert_eq!(all.len(), 1);
        let f = &all[0];
        assert_eq!((f.kind.clone(), f.runs.clone(), f.level), (FittingKind::Elbow, vec![r], l));
        assert_eq!(m.mep_fittings(Some(l)).unwrap(), all);
        assert!(m.mep_fittings(Some(l + 99)).unwrap().is_empty());
        assert_eq!(m.mep_fitting(f.key).unwrap().as_ref(), Some(f));
        assert_eq!(m.mep_fitting(f.key + 1).unwrap(), None);
    }

    #[test]
    fn deleting_the_run_removes_the_fitting() {
        let (mut m, _, r) = elbow();
        let key = m.mep_fittings(None).unwrap()[0].key;
        m.delete_mep_run(r).unwrap();
        assert_eq!(m.mep_fitting(key).unwrap(), None);
    }
}
