//! 3D measurement: a chain of points (with snaps); segment length, dx/dy/dz increments,
//! chain total. Esc/right click resets.

use ddc_render::glam::Vec3;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measure {
    pub pts: Vec<Vec3>,
}

/// Chain segment: length, increments, slope angle to the horizontal (degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub a: Vec3,
    pub b: Vec3,
    pub len: f32,
    pub d: Vec3,
    pub slope: f32,
}

pub fn seg(a: Vec3, b: Vec3) -> Seg {
    let d = b - a;
    let h = d.truncate().length();
    Seg {
        a,
        b,
        len: d.length(),
        d,
        slope: d.z.atan2(h).to_degrees(),
    }
}

impl Measure {
    pub fn click(&mut self, p: Vec3) {
        if self.pts.last().is_none_or(|q| (*q - p).length() > 1e-6) {
            self.pts.push(p);
        }
    }

    pub fn clear(&mut self) {
        self.pts.clear();
    }

    /// Chain segments and (if there is a cursor) a rubber-band segment to it.
    pub fn segs(&self, cur: Option<Vec3>) -> Vec<Seg> {
        let mut v: Vec<Seg> = self.pts.windows(2).map(|w| seg(w[0], w[1])).collect();
        if let (Some(&a), Some(c)) = (self.pts.last(), cur) {
            v.push(seg(a, c));
        }
        v
    }

    pub fn total(&self, cur: Option<Vec3>) -> f32 {
        self.segs(cur).iter().map(|s| s.len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_lengths_deltas_and_total() {
        let mut m = Measure::default();
        m.click(Vec3::ZERO);
        m.click(Vec3::ZERO);
        m.click(Vec3::new(3.0, 4.0, 0.0));
        assert_eq!(m.pts.len(), 2, "duplicate point ignored");
        let s = m.segs(Some(Vec3::new(3.0, 4.0, 5.0)));
        assert_eq!(s.len(), 2);
        assert!((s[0].len - 5.0).abs() < 1e-6 && s[0].slope.abs() < 1e-6);
        assert_eq!(s[1].d, Vec3::new(0.0, 0.0, 5.0));
        assert!((s[1].slope - 90.0).abs() < 1e-4);
        assert!((m.total(Some(Vec3::new(3.0, 4.0, 5.0))) - 10.0).abs() < 1e-6);
        assert!((m.total(None) - 5.0).abs() < 1e-6);
        m.clear();
        assert!(m.segs(None).is_empty());
    }
}
