//! Plan view range: four planes measured from the level elevation (m).
//! `top` >= `cut` >= `bottom` >= `depth`. Elements are sorted into bands by the vertical extent.
//! A value type only: the stored range is the plan's view graphics range (see `unify.rs`).

#[cfg(test)]
use serde_json::json;
use serde_json::Value as J;

use crate::views::LineClass;
use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewRange {
    pub top: f64,
    pub cut: f64,
    pub bottom: f64,
    pub depth: f64,
}

impl Default for ViewRange {
    fn default() -> Self {
        Self {
            top: 3.0,
            cut: 1.2,
            bottom: 0.0,
            depth: -0.5,
        }
    }
}

/// Where an element lies relative to the range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// The cut plane passes through the element.
    Cut,
    /// Below the cut plane and above the bottom: seen from above.
    Seen,
    /// Above the cut plane up to the top: drawn dashed.
    Overhead,
    /// Between the bottom and the depth: drawn light.
    Beyond,
    /// Outside the range: not drawn.
    Hidden,
}

impl Band {
    pub fn key(self) -> &'static str {
        match self {
            Band::Cut => "cut",
            Band::Seen => "seen",
            Band::Overhead => "overhead",
            Band::Beyond => "beyond",
            Band::Hidden => "hidden",
        }
    }

    pub fn line_class(self) -> Option<LineClass> {
        match self {
            Band::Cut | Band::Seen => Some(LineClass::Projection),
            Band::Overhead => Some(LineClass::Overhead),
            Band::Beyond => Some(LineClass::Beyond),
            Band::Hidden => None,
        }
    }
}

impl ViewRange {
    /// A range around an existing cut height, keeping the default spans.
    pub fn around_cut(cut: f64) -> Self {
        let d = Self::default();
        Self {
            top: cut + (d.top - d.cut),
            cut,
            bottom: cut - (d.cut - d.bottom),
            depth: cut - (d.cut - d.depth),
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        // an infinite top is an open range: no upper limit
        let all = [self.cut, self.bottom, self.depth];
        let top_bad = self.top.is_nan() || self.top == f64::NEG_INFINITY;
        if top_bad || all.iter().any(|v| !v.is_finite()) {
            return Err(Error::Invalid("view range must be finite".into()));
        }
        if !(self.top >= self.cut && self.cut >= self.bottom && self.bottom >= self.depth) {
            return Err(Error::Invalid(
                "view range needs top >= cut >= bottom >= depth".into(),
            ));
        }
        Ok(())
    }

    /// Moves one plane and pushes its neighbours so the order stays valid (used by the side editor).
    pub fn with_plane(mut self, plane: Plane, v: f64) -> Self {
        let v = if v.is_finite() { v } else { return self };
        match plane {
            Plane::Top => {
                self.top = v;
                self.cut = self.cut.min(v);
                self.bottom = self.bottom.min(self.cut);
                self.depth = self.depth.min(self.bottom);
            }
            Plane::Cut => {
                self.cut = v;
                self.top = self.top.max(v);
                self.bottom = self.bottom.min(v);
                self.depth = self.depth.min(self.bottom);
            }
            Plane::Bottom => {
                self.bottom = v;
                self.cut = self.cut.max(v);
                self.top = self.top.max(self.cut);
                self.depth = self.depth.min(v);
            }
            Plane::Depth => {
                self.depth = v;
                self.bottom = self.bottom.max(v);
                self.cut = self.cut.max(self.bottom);
                self.top = self.top.max(self.cut);
            }
        }
        self
    }

    /// Band of an element that spans `z0..z1` above the level elevation.
    pub fn classify(&self, z0: f64, z1: f64) -> Band {
        let (z0, z1) = if z0 <= z1 { (z0, z1) } else { (z1, z0) };
        if z0 > self.top || z1 < self.depth {
            return Band::Hidden;
        }
        if z0 <= self.cut && self.cut < z1 {
            return Band::Cut;
        }
        if z0 > self.cut {
            return Band::Overhead;
        }
        if z1 >= self.bottom {
            Band::Seen
        } else {
            Band::Beyond
        }
    }

    #[cfg(test)]
    pub(crate) fn to_json(self) -> J {
        json!({"top": self.top, "cut": self.cut, "bottom": self.bottom, "depth": self.depth})
    }

    pub(crate) fn from_json(j: &J) -> Option<Self> {
        let r = Self {
            top: j["top"].as_f64()?,
            cut: j["cut"].as_f64()?,
            bottom: j["bottom"].as_f64()?,
            depth: j["depth"].as_f64()?,
        };
        r.check().ok().map(|_| r)
    }
}

/// One of the four planes of the range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane {
    Top,
    Cut,
    Bottom,
    Depth,
}

impl Plane {
    pub const ALL: [Plane; 4] = [Plane::Top, Plane::Cut, Plane::Bottom, Plane::Depth];

    pub fn key(self) -> &'static str {
        match self {
            Plane::Top => "top",
            Plane::Cut => "cut",
            Plane::Bottom => "bottom",
            Plane::Depth => "depth",
        }
    }

    pub fn get(self, r: &ViewRange) -> f64 {
        match self {
            Plane::Top => r.top,
            Plane::Cut => r.cut,
            Plane::Bottom => r.bottom,
            Plane::Depth => r.depth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> ViewRange {
        ViewRange {
            top: 3.0,
            cut: 1.2,
            bottom: 0.0,
            depth: -0.5,
        }
    }

    #[test]
    fn bands() {
        let r = r();
        assert_eq!(r.classify(0.0, 3.0), Band::Cut);
        assert_eq!(r.classify(0.0, 1.2), Band::Seen, "cut plane at the top edge is not a cut");
        assert_eq!(r.classify(0.0, 0.9), Band::Seen);
        assert_eq!(r.classify(2.0, 2.5), Band::Overhead);
        assert_eq!(r.classify(-0.4, -0.1), Band::Beyond);
        assert_eq!(r.classify(-2.0, -1.0), Band::Hidden);
        assert_eq!(r.classify(3.5, 4.0), Band::Hidden);
        assert_eq!(r.classify(2.0, 4.0), Band::Overhead, "straddling the top still shows");
    }

    #[test]
    fn order_is_checked() {
        assert!(r().check().is_ok());
        let mut bad = r();
        bad.bottom = 2.0;
        assert!(bad.check().is_err());
        bad = r();
        bad.depth = f64::NAN;
        assert!(bad.check().is_err());
    }

    #[test]
    fn dragging_a_plane_pushes_neighbours() {
        let moved = r().with_plane(Plane::Bottom, 2.0);
        assert!(moved.check().is_ok());
        assert_eq!((moved.bottom, moved.cut, moved.top), (2.0, 2.0, 3.0));
        let moved = r().with_plane(Plane::Top, 0.5);
        assert!(moved.check().is_ok());
        assert_eq!(moved.cut, 0.5);
        assert_eq!(moved.bottom, 0.0);
    }

    #[test]
    fn json_round_trip_and_bad_json() {
        let j = r().to_json();
        assert_eq!(ViewRange::from_json(&j), Some(r()));
        assert_eq!(ViewRange::from_json(&json!({"top": 0.0, "cut": 1.0, "bottom": 0.0, "depth": 0.0})), None);
        assert_eq!(ViewRange::from_json(&json!({})), None);
    }
}
