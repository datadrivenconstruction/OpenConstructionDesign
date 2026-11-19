//! Per-element anchors of the image texture projection. A textured element that has an anchor
//! projects its texture in a frame of its own (origin and rotation about the vertical axis), so
//! moving or turning it carries the pattern along. Elements without one keep the world-fixed
//! projection, which makes neighbours line up. Read by the realistic pipeline only.

use std::sync::{Arc, Mutex};

/// Elements per texel row (two texels each).
pub const PER_ROW: u32 = 1024;
/// Texel row width.
pub const ROW_TEXELS: u32 = PER_ROW * 2;
/// First element number that can carry an anchor: the architecture block of the scene
/// (`scene_ids::ARCH_BASE` in the app, which has a test for the match).
pub const ELEM_BASE: u32 = 0x4800_0000;
/// Elements from `ELEM_BASE` on that can carry an anchor.
pub const MAX_ELEM: u32 = 1 << 16;

/// Anchors of one scene: scene element number (from `ELEM_BASE`), origin in world metres, angle in
/// radians.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnchorSet {
    pub items: Vec<(u32, [f32; 3], f32)>,
}

impl AnchorSet {
    /// Texel rows needed for the highest element (at least one).
    pub fn rows(&self) -> u32 {
        let top = self
            .items
            .iter()
            .filter_map(|i| slot(i.0))
            .max()
            .map_or(0, |m| m / PER_ROW + 1);
        top.max(1)
    }

    /// RGBA32F texels, `ROW_TEXELS` per row: element `e` is at row `e / PER_ROW`, texels
    /// `(e % PER_ROW) * 2` (origin, 1) and the next (cos, sin, 0, 0). Zero means no anchor.
    pub fn texels(&self) -> Vec<f32> {
        let mut out = vec![0.0f32; (self.rows() * ROW_TEXELS * 4) as usize];
        for &(e, o, a) in &self.items {
            let Some(e) = slot(e) else { continue };
            if !a.is_finite() || o.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let t = ((e / PER_ROW) * ROW_TEXELS + (e % PER_ROW) * 2) as usize * 4;
            let (s, c) = a.sin_cos();
            out[t..t + 4].copy_from_slice(&[o[0], o[1], o[2], 1.0]);
            out[t + 4..t + 8].copy_from_slice(&[c, s, 0.0, 0.0]);
        }
        out
    }
}

/// Texel slot of a scene element, if it can carry an anchor.
fn slot(elem: u32) -> Option<u32> {
    elem.checked_sub(ELEM_BASE).filter(|&s| s < MAX_ELEM)
}

struct Slot {
    rev: u64,
    set: Arc<AnchorSet>,
}

static SLOT: Mutex<Option<Slot>> = Mutex::new(None);

/// Replaces the anchors every renderer uses. The same set again changes nothing, so a scene
/// rebuilt without moving anything does not upload again.
pub fn publish(mut set: AnchorSet) {
    set.items.sort_by_key(|i| i.0);
    if let Ok(mut g) = SLOT.lock() {
        match g.as_ref() {
            Some(s) if *s.set == set => {}
            Some(s) => {
                let rev = s.rev + 1;
                *g = Some(Slot {
                    rev,
                    set: Arc::new(set),
                });
            }
            None => {
                *g = Some(Slot {
                    rev: 1,
                    set: Arc::new(set),
                });
            }
        }
    }
}

/// The published anchors when they are newer than revision `seen`.
pub fn current(seen: u64) -> Option<(u64, Arc<AnchorSet>)> {
    let g = SLOT.lock().ok()?;
    let s = g.as_ref().filter(|s| s.rev != seen)?;
    Some((s.rev, s.set.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texels_place_origin_and_rotation_by_element() {
        let set = AnchorSet {
            items: vec![
                (ELEM_BASE, [1.0, 2.0, 3.0], 0.0),
                (ELEM_BASE + 1025, [4.0, 5.0, 6.0], std::f32::consts::FRAC_PI_2),
            ],
        };
        assert_eq!(set.rows(), 2);
        let t = set.texels();
        assert_eq!(t.len(), (2 * ROW_TEXELS * 4) as usize);
        assert_eq!(&t[0..4], &[1.0, 2.0, 3.0, 1.0]);
        assert_eq!(&t[4..8], &[1.0, 0.0, 0.0, 0.0]);
        // element 1025: row 1, element 1 of the row, texel 2
        let at = ((ROW_TEXELS + 2) * 4) as usize;
        assert_eq!(&t[at..at + 4], &[4.0, 5.0, 6.0, 1.0]);
        assert!(t[at + 4].abs() < 1e-6 && (t[at + 5] - 1.0).abs() < 1e-6);
        // an element without an anchor reads zero
        assert_eq!(&t[8..12], &[0.0; 4]);
    }

    #[test]
    fn bad_values_and_huge_numbers_are_left_out() {
        let set = AnchorSet {
            items: vec![
                (ELEM_BASE + MAX_ELEM, [0.0; 3], 0.0),
                (ELEM_BASE - 1, [0.0; 3], 0.0),
                (ELEM_BASE + 3, [f32::NAN, 0.0, 0.0], 0.0),
                (ELEM_BASE + 4, [0.0; 3], f32::INFINITY),
            ],
        };
        assert_eq!(set.rows(), 1);
        assert!(set.texels().iter().all(|&v| v == 0.0));
        assert_eq!(AnchorSet::default().rows(), 1);
    }

    #[test]
    fn same_set_keeps_the_revision() {
        let a = AnchorSet {
            items: vec![(ELEM_BASE + 7, [1.0, 0.0, 0.0], 0.5)],
        };
        publish(a.clone());
        let (rev, got) = current(0).expect("published");
        assert_eq!(*got, a);
        publish(a);
        assert!(current(rev).is_none());
        publish(AnchorSet::default());
        assert!(current(rev).is_some());
    }
}
