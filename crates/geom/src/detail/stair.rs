//! Stair detail over the flight bodies: a nosing on every tread, standing `nosing` past the
//! riser below, rounded at fine detail; the flight, landing and stringer bodies stay as built.

use super::prim::{chamfered, prism, rounded};
use super::{Detail, Lod, Mat};
use crate::math::{v3, V2};
use crate::stair::Stair;

/// Depth of a nosing on the tread and its thickness.
const BACK: f64 = 0.05;
const THICK: f64 = 0.03;

pub fn nosings(s: &Stair, nosing: f64, lod: Lod) -> Detail {
    let mut d = Detail::default();
    if lod == Lod::Coarse || nosing <= 0.0 {
        return d;
    }
    let (r, t) = (s.riser, s.tread);
    let th = THICK.min(r * 0.5);
    for f in &s.flights {
        let right = f.right();
        for i in 0..f.treads() {
            let (u, z) = (t * i as f64, r * (i + 1) as f64);
            // section in (u, z): from `nosing` before the riser to `BACK` onto the tread
            let sec: Vec<V2> = if lod == Lod::Fine {
                rounded(u - nosing, z - th, u + BACK, z, (th * 0.4).min(nosing), 3)
            } else {
                chamfered(u - nosing, z - th, u + BACK, z, 0.004)
            };
            let o = v3(f.start.x, f.start.y, f.z0);
            let du = v3(f.dir.x, f.dir.y, 0.0);
            let dw = v3(right.x, right.y, 0.0) * f.width;
            prism(d.part(Mat::Board), &sec, o, du, v3(0.0, 0.0, 1.0), dw);
        }
    }
    d
}
