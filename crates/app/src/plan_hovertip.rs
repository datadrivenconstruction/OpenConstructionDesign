//! Hover tip in the plan: after a still pause over an item, category, type, size and level.
//! Pure state and text; the view feeds it the pointer and paints the result.

use crate::editor::{Editor, Sel};
use crate::units::{fmt_area_in, format_length, Units};
use ddc_geom::V2;

/// Still time before the tip shows, seconds.
pub const DELAY_S: f64 = 0.5;
/// Pointer travel, px, that counts as a move and restarts the wait.
pub const MOVE_PX: f32 = 2.0;

#[derive(Clone, Debug, Default)]
pub struct HoverTip {
    at: Option<(f32, f32)>,
    since: f64,
}

impl HoverTip {
    /// Call every frame: `pos` is the pointer (`None` off the canvas), `now` in seconds.
    pub fn update(&mut self, pos: Option<(f32, f32)>, now: f64) {
        match (pos, self.at) {
            (None, _) => self.at = None,
            (Some(p), Some(a)) if (p.0 - a.0).hypot(p.1 - a.1) <= MOVE_PX => {}
            (Some(p), _) => {
                self.at = Some(p);
                self.since = now;
            }
        }
    }

    /// Seconds still left to wait; `None` when there is no pointer.
    pub fn remaining(&self, now: f64) -> Option<f64> {
        self.at.map(|_| (DELAY_S - (now - self.since)).max(0.0))
    }

    /// The tip may show: still long enough and nothing else owns the pointer.
    pub fn visible(&self, now: f64, busy: Busy) -> bool {
        !busy.dragging
            && !busy.tool_active
            && !busy.cycle_label
            && self.remaining(now) == Some(0.0)
    }
}

/// What else uses the pointer this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Busy {
    pub dragging: bool,
    pub tool_active: bool,
    pub cycle_label: bool,
}

/// Size line of the tip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Measure {
    Length(f64),
    Area(f64),
    None,
}

/// Wall and similar linear items show length; room and slab show area.
pub fn measure(ed: &Editor, s: Sel) -> Measure {
    match s {
        Sel::Slab(id) => ed.slab(id).map_or(Measure::None, |x| Measure::Area(x.area)),
        Sel::Room(id) => ed
            .rooms
            .iter()
            .find(|r| r.id == id)
            .map_or(Measure::None, |r| Measure::Area(rings_area(&r.rings))),
        _ => crate::pick_rules::length(ed, s).map_or(Measure::None, Measure::Length),
    }
}

/// Outer ring minus holes, m2.
pub fn rings_area(rings: &[Vec<V2>]) -> f64 {
    let a = |r: &Vec<V2>| {
        let n = r.len();
        (0..n)
            .map(|i| r[i].x * r[(i + 1) % n].y - r[(i + 1) % n].x * r[i].y)
            .sum::<f64>()
            .abs()
            / 2.0
    };
    let mut it = rings.iter();
    let outer = it.next().map_or(0.0, a);
    (outer - it.map(a).sum::<f64>()).max(0.0)
}

/// Type name of the item, empty when it has none.
pub fn type_name(ed: &Editor, s: Sel) -> String {
    match s {
        Sel::Wall(id) => ed
            .walls
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| ed.types.iter().find(|t| t.id == w.wall_type))
            .map_or_else(String::new, |t| t.name.clone()),
        Sel::Room(id) => ed
            .rooms
            .iter()
            .find(|r| r.id == id)
            .map_or_else(String::new, |r| r.name.clone()),
        _ => String::new(),
    }
}

/// Two-line tip text: "Category: type" and "size - level".
pub fn tip_text(cat: &str, ty: &str, m: Measure, level: &str, u: Units, ru: bool) -> String {
    let head = if ty.is_empty() {
        cat.to_string()
    } else {
        crate::tr_lang(ru, "hovertip.head", &[("cat", cat), ("type", ty)])
    };
    let size = match m {
        Measure::Length(l) => format_length(l, u),
        Measure::Area(a) => fmt_area_in(a, u),
        Measure::None => String::new(),
    };
    let tail = match (size.is_empty(), level.is_empty()) {
        (true, true) => return head,
        (false, true) => size,
        (true, false) => level.to_string(),
        (false, false) => crate::tr_lang(ru, "hovertip.tail", &[("size", &size), ("level", level)]),
    };
    format!("{head}\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn still(t: f64) -> HoverTip {
        let mut h = HoverTip::default();
        h.update(Some((10.0, 10.0)), 0.0);
        h.update(Some((10.0, 10.0)), t);
        h
    }

    #[test]
    fn tip_appears_after_500ms_still() {
        assert!(!still(0.4).visible(0.4, Busy::default()));
        assert!(still(0.5).visible(0.5, Busy::default()));
    }

    #[test]
    fn move_resets_timer() {
        let mut h = still(0.6);
        h.update(Some((30.0, 10.0)), 0.7);
        assert!(!h.visible(0.9, Busy::default()));
        assert!(h.visible(1.2, Busy::default()));
        h.update(None, 1.3);
        assert_eq!(h.remaining(1.3), None);
    }

    #[test]
    fn hidden_while_dragging() {
        let b = Busy { dragging: true, ..Default::default() };
        assert!(!still(1.0).visible(1.0, b));
    }

    #[test]
    fn hidden_with_tool_active() {
        let b = Busy { tool_active: true, ..Default::default() };
        assert!(!still(1.0).visible(1.0, b));
    }

    #[test]
    fn hidden_when_cycle_label_active() {
        let b = Busy { cycle_label: true, ..Default::default() };
        assert!(!still(1.0).visible(1.0, b));
    }

    #[test]
    fn wall_shows_length() {
        let s = tip_text("Walls", "Basic", Measure::Length(4.0), "L1", Units::Metric, false);
        assert!(s.contains(&format_length(4.0, Units::Metric)), "{s}");
        assert!(s.contains("Basic") && s.contains("L1"), "{s}");
    }

    #[test]
    fn room_and_slab_show_area() {
        let s = tip_text("Rooms", "", Measure::Area(12.5), "L1", Units::Metric, true);
        assert!(s.contains(&fmt_area_in(12.5, Units::Metric)), "{s}");
        let sq = [
            vec![v(0.0, 0.0), v(4.0, 0.0), v(4.0, 3.0), v(0.0, 3.0)],
            vec![v(1.0, 1.0), v(2.0, 1.0), v(2.0, 2.0), v(1.0, 2.0)],
        ];
        assert!((rings_area(&sq) - 11.0).abs() < 1e-9);
    }

    fn v(x: f64, y: f64) -> V2 {
        ddc_geom::v2(x, y)
    }
}
