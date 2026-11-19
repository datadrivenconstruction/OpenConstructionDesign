//! Anchors of wall textures in the realistic style. A textured wall projects its pattern in a frame
//! of its own, with the origin at its start and the x axis along the wall, so a moved or turned
//! wall carries the pattern with it. Walls that continue each other in a straight line share the
//! frame of the first, so a run built in place reads as one surface. The vertical origin stays at
//! the world's zero: courses line up from storey to storey.

use ddc_render::anchor::{self, AnchorSet};

/// Ends closer than this (metres) are the same joint.
const JOINT_TOL: f64 = 1e-3;
/// Directions closer than this (cosine) are one straight line, about 0.26 degrees.
const STRAIGHT_COS: f64 = 0.99999;

/// Frame of one wall: origin xy and angle (radians).
pub type Frame = ([f64; 2], f64);

/// Frames of walls given by their end points; `None` for a zero-length wall. A wall that starts
/// where an earlier wall of the same direction ends takes that wall's frame.
pub fn frames(walls: &[([f64; 2], [f64; 2])]) -> Vec<Option<Frame>> {
    let dir = |w: &([f64; 2], [f64; 2])| {
        let (dx, dy) = (w.1[0] - w.0[0], w.1[1] - w.0[1]);
        let len = dx.hypot(dy);
        (len > 1e-9 && len.is_finite()).then(|| (dx / len, dy / len))
    };
    let dirs: Vec<Option<(f64, f64)>> = walls.iter().map(dir).collect();
    // the wall that ends where wall `i` starts and runs the same way
    let before = |i: usize| -> Option<usize> {
        let d = dirs[i]?;
        (0..walls.len()).find(|&j| {
            j != i
                && dirs[j].is_some_and(|e| e.0 * d.0 + e.1 * d.1 > STRAIGHT_COS)
                && (walls[j].1[0] - walls[i].0[0]).hypot(walls[j].1[1] - walls[i].0[1]) < JOINT_TOL
        })
    };
    (0..walls.len())
        .map(|i| {
            dirs[i]?;
            // walk back to the first wall of the run; the step limit stops a malformed loop
            let mut head = i;
            for _ in 0..walls.len() {
                match before(head) {
                    Some(j) if j != i => head = j,
                    _ => break,
                }
            }
            let d = dirs[head]?;
            Some((walls[head].0, d.1.atan2(d.0)))
        })
        .collect()
}

/// Anchors for the scene. `element(i)` is the scene element number of wall `i`, `None` when it
/// has no textured faces and needs no anchor.
pub fn anchors(
    walls: &[([f64; 2], [f64; 2])],
    element: impl Fn(usize) -> Option<u32>,
) -> AnchorSet {
    let items = frames(walls)
        .into_iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let (o, a) = f?;
            let e = element(i)?;
            Some((e, [o[0] as f32, o[1] as f32, 0.0], a as f32))
        })
        .collect();
    AnchorSet { items }
}

/// Publishes the anchors of the walls of `ed` to the renderer. `slot_of(wall id)` gives the
/// scene element of a wall.
pub fn publish(ed: &crate::editor::Editor, slot_of: impl Fn(i64) -> Option<u32>) {
    let texl = crate::mat_tex::layers(ed);
    let set = if texl.is_empty() {
        AnchorSet::default()
    } else {
        let ends: Vec<([f64; 2], [f64; 2])> = ed
            .walls
            .iter()
            .map(|w| ([w.a.x, w.a.y], [w.b.x, w.b.y]))
            .collect();
        anchors(&ends, |i| {
            let w = &ed.walls[i];
            texl.wall_textured(w.wall_type).then(|| slot_of(w.id)).flatten()
        })
    };
    anchor::publish(set);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wall_is_framed_at_its_start_along_its_direction() {
        let f = frames(&[([2.0, 1.0], [2.0, 5.0])]);
        let (o, a) = f[0].unwrap();
        assert_eq!(o, [2.0, 1.0]);
        assert!((a - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    }

    #[test]
    fn a_straight_run_shares_the_frame_of_its_first_wall() {
        // listed out of order on purpose
        let w = [
            ([6.5, 0.0], [10.0, 0.0]),
            ([0.0, 0.0], [4.0, 0.0]),
            ([4.0, 0.0], [6.5, 0.0]),
        ];
        let f = frames(&w);
        for x in &f {
            assert_eq!(x.unwrap().0, [0.0, 0.0]);
        }
    }

    #[test]
    fn corners_and_reversed_walls_start_their_own_frame() {
        let w = [
            ([0.0, 0.0], [4.0, 0.0]),
            ([4.0, 0.0], [4.0, 3.0]),
            ([8.0, 0.0], [4.0, 0.0]),
        ];
        let f = frames(&w);
        assert_eq!(f[1].unwrap().0, [4.0, 0.0]);
        assert_eq!(f[2].unwrap().0, [8.0, 0.0]);
        assert!((f[2].unwrap().1 - std::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn moving_a_wall_moves_its_frame_with_it() {
        let a = frames(&[([0.0, 0.0], [4.0, 0.0])])[0].unwrap();
        let b = frames(&[([10.0, 7.0], [14.0, 7.0])])[0].unwrap();
        assert_eq!((b.0[0] - a.0[0], b.0[1] - a.0[1]), (10.0, 7.0));
        assert_eq!(a.1, b.1);
    }

    #[test]
    fn degenerate_and_looped_input_does_not_hang() {
        let f = frames(&[([1.0, 1.0], [1.0, 1.0]), ([0.0, 0.0], [1.0, 0.0])]);
        assert!(f[0].is_none() && f[1].is_some());
        // two walls that each claim the other as predecessor
        let w = [([0.0, 0.0], [0.0, 0.0]), ([0.0, 0.0], [1.0, 0.0]), ([1.0, 0.0], [0.0, 0.0])];
        assert_eq!(frames(&w).len(), 3);
    }

    #[test]
    fn only_textured_walls_get_anchors() {
        let w = [([0.0, 0.0], [4.0, 0.0]), ([0.0, 5.0], [4.0, 5.0])];
        let set = anchors(&w, |i| (i == 1).then_some(anchor::ELEM_BASE + 9));
        assert_eq!(set.items.len(), 1);
        assert_eq!(set.items[0].0, anchor::ELEM_BASE + 9);
        assert_eq!(anchor::ELEM_BASE, crate::scene_ids::ARCH_BASE);
        assert_eq!(set.items[0].1, [0.0, 5.0, 0.0]);
    }
}
