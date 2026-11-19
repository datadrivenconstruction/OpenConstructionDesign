//! Cut wall bodies of a level, kept by their full geometric input: the storeys of a typical
//! floor give the same walls, openings and level-relative cutters, so their join and cut run
//! once. The key is the exact input text (floats print round-trip), never a lossy hash.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ddc_geom::cut::{check_cutter, cut_walls, CutError, Cuts, Cutter, Filler};
use ddc_geom::layered::{join_with, layer_footprints, plain_pieces, JoinPrefs, Opening, Strip};
use ddc_geom::{Profile, Tol};

/// Inputs kept at most; a new one past this clears the store.
const KEEP: usize = 64;

/// Joined footprints, node fillers and the cut of one level's walls.
pub struct WallCut {
    pub fps: Vec<Vec<Vec<Profile>>>,
    pub fillers: Vec<Filler>,
    pub cut: Cuts,
}

pub struct WallCuts {
    /// Off: every call computes from all cutters (the reference the cache is checked against).
    pub on: bool,
    map: Mutex<HashMap<String, Arc<WallCut>>>,
}

impl Default for WallCuts {
    fn default() -> Self {
        WallCuts {
            on: true,
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl WallCuts {
    /// The cut of `strips`; cutters that cannot reach a wall or filler in height are dropped
    /// first, as the cut itself ignores them, so a storey's key does not see far levels.
    pub fn get(
        &self,
        strips: &[Strip],
        prefs: &[JoinPrefs],
        ops: &[Vec<Opening>],
        slabs: Vec<Cutter>,
        height: f64,
        tol: Tol,
    ) -> Arc<WallCut> {
        if !self.on {
            return Arc::new(compute(strips, prefs, ops, &slabs, vec![], height, tol));
        }
        let (slabs, bad) = reaching(strips, slabs, height, tol);
        let key = format!("{strips:?}|{prefs:?}|{ops:?}|{slabs:?}|{bad:?}|{height:?}");
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = map.get(&key) {
            return c.clone();
        }
        let c = Arc::new(compute(strips, prefs, ops, &slabs, bad, height, tol));
        if map.len() >= KEEP {
            map.clear();
        }
        map.insert(key, c.clone());
        c
    }
}

/// Valid cutters whose height range overlaps some wall or filler by more than `tol`, the test
/// `cut_prism` applies, and the invalid ones as the cut reports them: by their index in all
/// of `slabs`, wherever they stand.
fn reaching(
    strips: &[Strip],
    slabs: Vec<Cutter>,
    height: f64,
    tol: Tol,
) -> (Vec<Cutter>, Vec<CutError>) {
    let lo = strips.iter().map(|s| s.base).fold(0.0_f64, f64::min);
    let hi = strips.iter().map(|s| s.base + s.height).fold(height, f64::max);
    let all = !(lo.is_finite() && hi.is_finite());
    let mut bad = vec![];
    let mut kept = vec![];
    for (k, c) in slabs.into_iter().enumerate() {
        match check_cutter(k, &c) {
            Err(e) => bad.push(e),
            Ok(()) if all || c.z1 > lo + tol.length && c.z0 < hi - tol.length => kept.push(c),
            Ok(()) => {}
        }
    }
    (kept, bad)
}

/// `bad`: the invalid cutters taken out before, reported first as the cut reports its own.
fn compute(
    strips: &[Strip],
    prefs: &[JoinPrefs],
    ops: &[Vec<Opening>],
    slabs: &[Cutter],
    bad: Vec<CutError>,
    height: f64,
    tol: Tol,
) -> WallCut {
    let joined = join_with(strips, prefs, tol).ok();
    let fps: Vec<Vec<Vec<Profile>>> = strips
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let plain = plain_pieces(s);
            let pieces = joined.as_ref().map_or(&plain, |j| &j.pieces[i]);
            layer_footprints(s, pieces, i, tol).unwrap_or_default()
        })
        .collect();
    let fillers: Vec<Filler> = joined
        .as_ref()
        .map(|j| {
            j.fillers
                .iter()
                .map(|f| Filler {
                    ring: f.clone(),
                    zb: 0.0,
                    zt: height,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut cut = cut_walls(strips, &fps, ops, slabs, &fillers, tol);
    if !bad.is_empty() {
        cut.skipped.splice(0..0, bad);
    }
    WallCut { fps, fillers, cut }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::layered::Layer;
    use ddc_geom::v2;

    fn wall(x0: f64, x1: f64) -> Strip {
        Strip {
            a: v2(x0, 0.0),
            b: v2(x1, 0.0),
            layers: vec![Layer::new(0.2)],
            offset0: -0.1,
            base: 0.0,
            height: 3.0,
        }
    }

    fn slab(z0: f64, z1: f64) -> Cutter {
        Cutter {
            ring: vec![v2(-1.0, -1.0), v2(5.0, -1.0), v2(5.0, 1.0), v2(-1.0, 1.0)],
            holes: vec![],
            z0,
            z1,
        }
    }

    /// A broken cutter on a far level, one far valid and one that cuts: the shared cut reports
    /// the same issues by the same indices and gives the same bodies as the full one.
    #[test]
    fn a_far_broken_cutter_is_reported_as_the_full_cut_reports_it() {
        let strips = [wall(0.0, 4.0)];
        let prefs: Vec<JoinPrefs> = vec![[None, None]];
        let ops = vec![vec![Opening {
            along: 1.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
        }]];
        let mut broken = slab(-9.0, -8.8);
        broken.ring.truncate(2);
        let slabs = vec![slab(-6.2, -6.0), broken, slab(2.8, 3.0), slab(4.0, 3.5)];
        let tol = Tol::default();
        let run = |on: bool| {
            let w = WallCuts {
                on,
                ..Default::default()
            };
            let c = w.get(&strips, &prefs, &ops, slabs.clone(), 3.0, tol);
            (format!("{:?}", c.cut.skipped), format!("{:?}", c.cut.walls))
        };
        let (full, shared) = (run(false), run(true));
        assert!(full.0.contains("BadCutter { cutter: 1 }"), "{}", full.0);
        assert!(full.0.contains("BadCutter { cutter: 3 }"), "{}", full.0);
        assert_eq!(shared, full);
    }
}
