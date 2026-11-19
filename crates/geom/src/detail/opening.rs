//! Parameters of a door or window type: everything the detail builders draw from. A type
//! (its mark, schedule data, fire rating) belongs to the openings model; this struct is the
//! geometric part of it, so a type maps to one [`OpeningType`] and back without loss.
//!
//! Local frame of a built opening: x along the wall from the opening start (0..width), y across
//! the wall (0 = wall axis, +y = the swing face), z up from the opening bottom (the sill of a
//! window, the floor of a door).

use super::rules::DetailRules;

/// Section of a frame member: face width in the opening plane, depth across the wall, and the
/// stop (rebate) the leaf or sash closes against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameProfile {
    pub face: f64,
    pub depth: f64,
    /// Stop: how far it stands into the opening past the face, and its depth across.
    pub stop: f64,
    pub stop_depth: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LeafStyle {
    /// Plain slab.
    Flush,
    /// Stiles and rails around recessed panels, `cols` × `rows`.
    Panels { cols: u8, rows: u8 },
    /// Stiles and rails around a glazed light (`rows` lights one above the other).
    Glazed { rows: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DoorOp {
    Swing,
    Sliding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    Lever,
    /// Vertical pull bar (entrance doors).
    Pull,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoorType {
    /// 1 or 2 leaves.
    pub leaves: u8,
    pub op: DoorOp,
    pub leaf: LeafStyle,
    pub frame: FrameProfile,
    pub leaf_t: f64,
    /// Architrave on both faces: width and thickness; `None` for a flush reveal.
    pub casing: Option<(f64, f64)>,
    /// Threshold height; `None` for none.
    pub threshold: Option<f64>,
    pub handle: Handle,
    pub handle_z: f64,
}

/// How the fields of a window open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SashLayout {
    AllFixed,
    /// The outer fields of each row open; one field: it opens.
    OuterCasements,
    AllCasements,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowType {
    pub frame: FrameProfile,
    /// Face width of an opening sash.
    pub sash: f64,
    /// Mullion / transom face width.
    pub bar: f64,
    pub glass_t: f64,
    /// Fields across; 0 = from the width and the pane rule.
    pub cols: u8,
    /// Transom: height of the top light, `None` = by the transom rule.
    pub top_light: Option<f64>,
    pub sashes: SashLayout,
    /// Outside sill projection past the wall face; `None` for none.
    pub sill: Option<f64>,
    /// Inside board projection past the wall face; `None` for none.
    pub board: Option<f64>,
    /// Frame centre across the wall, from the wall axis toward the outside face (−y).
    pub setback: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpeningType {
    Door(DoorType),
    Window(WindowType),
}

/// One opening in its wall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub width: f64,
    pub height: f64,
    pub wall_t: f64,
    /// The leaf swings to −y instead of +y.
    pub swing_flip: bool,
    /// Hinges at the far end (x = width) instead of x = 0.
    pub hinge_flip: bool,
}

pub const DOOR_FRAME: FrameProfile = FrameProfile {
    face: 0.045,
    depth: 0.1,
    stop: 0.015,
    stop_depth: 0.04,
};

pub const WINDOW_FRAME: FrameProfile = FrameProfile {
    face: 0.06,
    depth: 0.08,
    stop: 0.012,
    stop_depth: 0.03,
};

impl DoorType {
    /// The type the rules give a door of this width: two glazed leaves for an entrance, one
    /// panelled leaf otherwise.
    pub fn for_width(width: f64, r: &DetailRules) -> DoorType {
        let double = width >= r.door_double_from;
        let glazed = double || width >= r.door_glazed_from;
        DoorType {
            leaves: if double { 2 } else { 1 },
            op: DoorOp::Swing,
            leaf: if glazed {
                LeafStyle::Glazed { rows: 1 }
            } else {
                LeafStyle::Panels { cols: 1, rows: 2 }
            },
            frame: DOOR_FRAME,
            leaf_t: 0.04,
            casing: Some((0.07, 0.015)),
            threshold: Some(0.012),
            handle: if glazed { Handle::Pull } else { Handle::Lever },
            handle_z: r.door_handle_z,
        }
    }
}

impl WindowType {
    pub fn standard(r: &DetailRules) -> WindowType {
        WindowType {
            frame: WINDOW_FRAME,
            sash: 0.05,
            bar: 0.05,
            glass_t: 0.024,
            cols: 0,
            top_light: None,
            sashes: SashLayout::OuterCasements,
            sill: Some(r.sill_projection),
            board: Some(r.sill_projection),
            setback: 0.0,
        }
    }

    /// Fields across for an inner width.
    pub fn columns(&self, inner: f64, r: &DetailRules) -> usize {
        if self.cols > 0 {
            return self.cols as usize;
        }
        ((inner / r.window_pane).round() as usize).clamp(1, 12)
    }

    /// Top-light height, if this window of `height` gets a transom.
    pub fn transom(&self, height: f64, r: &DetailRules) -> Option<f64> {
        match self.top_light {
            Some(t) => (t > 0.0 && t < height * 0.6).then_some(t),
            None => (height >= r.window_transom_from).then_some(r.window_top_light),
        }
    }
}

/// Key of a built prototype: the type, the placement and the level of detail, sizes to 0.1 mm.
pub fn proto_key(t: &OpeningType, p: &Placement, lod: crate::component::Lod) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let q = |x: f64, h: &mut std::collections::hash_map::DefaultHasher| ((x * 1e4).round() as i64).hash(h);
    match t {
        OpeningType::Door(d) => {
            0u8.hash(&mut h);
            (d.leaves, d.op, d.leaf, d.handle).hash(&mut h);
            for x in [d.frame.face, d.frame.depth, d.frame.stop, d.frame.stop_depth, d.leaf_t, d.handle_z] {
                q(x, &mut h);
            }
            for x in [d.casing.map(|c| c.0), d.casing.map(|c| c.1), d.threshold] {
                q(x.unwrap_or(-1.0), &mut h);
            }
        }
        OpeningType::Window(w) => {
            1u8.hash(&mut h);
            (w.cols, w.sashes).hash(&mut h);
            for x in [w.frame.face, w.frame.depth, w.frame.stop, w.frame.stop_depth, w.sash, w.bar, w.glass_t, w.setback] {
                q(x, &mut h);
            }
            for x in [w.top_light, w.sill, w.board] {
                q(x.unwrap_or(-1.0), &mut h);
            }
        }
    }
    for x in [p.width, p.height, p.wall_t] {
        q(x, &mut h);
    }
    (p.swing_flip, p.hinge_flip, lod).hash(&mut h);
    h.finish()
}
