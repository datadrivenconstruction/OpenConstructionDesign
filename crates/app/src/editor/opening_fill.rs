//! 3D fill of a plain door/window opening, built from the opening's type and the project's detail
//! rules: a door's lining, casings, threshold, panelled or glazed leaves and hardware; a window's
//! frame, mullions and transom, sashes, glazing, sill and board. One prototype per type and size.

use crate::opening_tool::{HostWall, Kind, Placed};
use ddc_geom::detail::opening::{DoorType, OpeningType, Placement, WindowType};
use ddc_geom::detail::rules::DetailRules;
use ddc_geom::detail::{cache, Lod, Mat};
use ddc_model::Model;
use ddc_render::{tag_rgb, SceneMesh, TAG_DOOR, TAG_GLASS};

/// Window frames: anthracite; door frames and casings: warm white; hardware: brushed steel.
pub const FRAME_DARK: [u8; 3] = [0x3b, 0x3e, 0x44];
pub const FRAME_LIGHT: [u8; 3] = [0xec, 0xea, 0xe4];
pub const STEEL: [u8; 3] = [0xb4, 0xb8, 0xbe];
pub const STONE: [u8; 3] = [0x8d, 0x8a, 0x84];

/// Shaded colour of a finish material by name (sRGB); `None` keeps the category palette.
/// Only finishes: structural materials (concrete, masonry) keep the neutral model colour.
pub fn finish_rgb(material: &str) -> Option<[u8; 3]> {
    let m = material.to_lowercase();
    let has = |k: &[&str]| k.iter().any(|k| m.contains(k));
    Some(if has(&["facade render", "render", "\u{448}\u{442}\u{443}\u{43a}\u{430}\u{442}\u{443}\u{440}"]) {
        [0xe6, 0xe0, 0xd4]
    } else if has(&["brick", "\u{43a}\u{438}\u{440}\u{43f}\u{438}\u{447}"]) {
        [0xa8, 0x5c, 0x46]
    } else if has(&["cladding", "\u{43e}\u{431}\u{43b}\u{438}\u{446}\u{43e}\u{432}"]) {
        [0x6e, 0x73, 0x7a]
    } else if has(&["roofing", "membrane", "\u{43a}\u{440}\u{43e}\u{432}\u{43b}", "\u{43c}\u{435}\u{43c}\u{431}\u{440}\u{430}\u{43d}"]) {
        [0x5c, 0x5f, 0x64]
    } else if has(&["timber", "wood", "\u{434}\u{435}\u{440}\u{435}\u{432}"]) {
        [0xa8, 0x7c, 0x55]
    } else {
        return None;
    })
}

/// Detail rules of the model: the built-in values under the last applied project's brief.
pub fn rules(m: &Model) -> DetailRules {
    let mut set = ddc_gen::project::rules::RuleSet::new();
    if let Some((_, v)) = crate::ai::project_plan::stored(m).ok().and_then(|s| s.into_iter().last()) {
        if v["input"]["rules"].is_object() {
            // a brief the rule set refuses is refused whole: the built-in values stay
            let _ = set.apply_patch(ddc_gen::project::rules::Layer::Brief, &v["input"]["rules"]);
        }
    }
    DetailRules::from_lookup(|id| set.value(id))
}

/// The type and placement the opening is built from.
pub fn opening_type(h: &HostWall, o: &Placed, r: &DetailRules) -> Option<(OpeningType, Placement)> {
    let t = match o.kind {
        Kind::Door => OpeningType::Door(DoorType::for_width(o.width, r)),
        Kind::Window => OpeningType::Window(WindowType::standard(r)),
        Kind::Void => return None,
    };
    let p = Placement {
        width: o.width,
        height: o.height,
        wall_t: 2.0 * h.half,
        swing_flip: o.flip.side,
        hinge_flip: o.flip.hinge,
    };
    Some((t, p))
}

/// Scene tag of a detail material.
fn tag(kind: Kind, m: Mat) -> u32 {
    match m {
        Mat::Frame if kind == Kind::Window => tag_rgb(FRAME_DARK, false),
        Mat::Frame | Mat::Board => tag_rgb(FRAME_LIGHT, false),
        Mat::Leaf => TAG_DOOR,
        Mat::Glass => TAG_GLASS,
        Mat::Metal => tag_rgb(STEEL, false),
        Mat::Stone => tag_rgb(STONE, false),
    }
}

/// Scene meshes of the fill at `lod`, all one element so the fill selects as its opening.
pub fn meshes(h: &HostWall, o: &Placed, element: u32, lod: Lod, r: &DetailRules) -> Vec<SceneMesh> {
    let Some((t, p)) = opening_type(h, o, r) else {
        return vec![];
    };
    let d = cache::opening(&t, &p, lod, r);
    let mut out: Vec<SceneMesh> = vec![];
    for (mat, m) in &d.parts {
        let tag = tag(o.kind, *mat);
        let i = match out.iter().position(|s| s.tag == tag) {
            Some(i) => i,
            None => {
                out.push(SceneMesh { element, tag, positions: vec![], triangles: vec![] });
                out.len() - 1
            }
        };
        let s = &mut out[i];
        let base = s.positions.len() as u32;
        s.positions.extend(m.positions.iter().map(|q| {
            let a = h.at(o.along + q.x, q.y);
            [a.x as f32, a.y as f32, (o.sill + q.z) as f32]
        }));
        // the host frame (along, normal, up) is right-handed like the local one: winding stays
        s.triangles.extend(m.triangles.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opening_tool::Flip;
    use ddc_geom::v2;

    fn wall() -> HostWall {
        HostWall { id: 1, a: v2(0.0, 0.0), b: v2(6.0, 0.0), height: 3.0, half: 0.15, run: 6.0 }
    }

    fn placed(kind: Kind, width: f64, sill: f64, height: f64) -> Placed {
        Placed { id: 1, wall: 1, along: 1.0, width, sill, height, kind, flip: Flip::default() }
    }

    fn bounds(ms: &[SceneMesh]) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in ms.iter().flat_map(|m| &m.positions) {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    }

    fn signed_volume(m: &SceneMesh) -> f64 {
        m.triangles
            .iter()
            .map(|t| {
                let p = |i: u32| m.positions[i as usize].map(|x| x as f64);
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]))
                    / 6.0
            })
            .sum()
    }

    #[test]
    fn door_fill_is_detailed_inside_the_opening_and_outward() {
        let r = DetailRules::default();
        let o = placed(Kind::Door, 0.9, 0.0, 2.1);
        for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
            let ms = meshes(&wall(), &o, 7, lod, &r);
            assert!(ms.iter().all(|m| m.element == 7));
            assert!(ms.iter().any(|m| m.tag == TAG_DOOR));
            let tris: usize = ms.iter().map(|m| m.triangles.len()).sum();
            // Coarse has no casings by design (4727cd5a7): lining, leaf and threshold measure 52
            if lod == Lod::Coarse {
                assert_eq!(tris, 52, "Coarse door fill");
            } else {
                assert!(tris > 12 * 6, "{lod:?}: {tris} triangles is a few blocks");
            }
            for m in &ms {
                assert!(signed_volume(m) > 0.0, "{lod:?} tag {}: inside out", m.tag);
            }
            let (lo, hi) = bounds(&ms);
            // a casing reaches 0.07 - 0.045 + 0.008 = 0.033 m past the opening edge
            let cased = lo[0] < 1.0 - 0.02 && hi[0] > 1.9 + 0.02;
            assert_eq!(cased, lod != Lod::Coarse, "{lod:?}: casings past the opening {lo:?} {hi:?}");
            // casings overlap the wall face; nothing below the floor or far above the head
            assert!(lo[0] > 1.0 - 0.1 && hi[0] < 1.9 + 0.1 && lo[2] > -1e-4 && hi[2] < 2.1 + 0.08);
        }
        let fine = meshes(&wall(), &o, 7, Lod::Fine, &r);
        assert!(fine.iter().any(|m| m.tag == tag_rgb(STEEL, false)), "hardware");
        assert!(fine.iter().any(|m| m.tag == tag_rgb(STONE, false)), "threshold");
    }

    #[test]
    fn wide_doors_follow_the_rules_and_swing_to_the_flipped_side() {
        let r = DetailRules::default();
        let glass = |o: &Placed, r: &DetailRules| meshes(&wall(), o, 1, Lod::Medium, r).iter().any(|m| m.tag == TAG_GLASS);
        let wide = placed(Kind::Door, 1.8, 0.0, 2.4);
        assert!(glass(&wide, &r));
        assert!(!glass(&wide, &DetailRules { door_glazed_from: 2.5, door_double_from: 2.5, ..r }));
        let leaf_y = |o: &Placed| {
            let ms = meshes(&wall(), o, 1, Lod::Medium, &r);
            let m = ms.iter().find(|m| m.tag == TAG_DOOR).unwrap();
            m.positions.iter().map(|p| p[1] as f64).sum::<f64>() / m.positions.len() as f64
        };
        let mut o = placed(Kind::Door, 0.9, 0.0, 2.1);
        assert!(leaf_y(&o) > 0.0);
        o.flip.side = true;
        assert!(leaf_y(&o) < 0.0);
    }

    #[test]
    fn window_fill_has_panes_per_field_and_sits_on_its_sill() {
        let r = DetailRules::default();
        let o = placed(Kind::Window, 2.0, 0.9, 1.8);
        let ms = meshes(&wall(), &o, 7, Lod::Medium, &r);
        let glass = ms.iter().find(|m| m.tag == TAG_GLASS).unwrap();
        assert!(signed_volume(glass) > 0.0);
        let (lo, hi) = bounds(&ms);
        // sill and board below the frame by their own thickness only; nothing above the head
        assert!(lo[2] > 0.9 - 0.06 && hi[2] < 2.7 + 1e-4, "{lo:?} {hi:?}");
        // the outside sill stands past the outer face
        assert!(lo[1] < -0.15 - 0.01);
        let tiny = meshes(&wall(), &placed(Kind::Window, 0.3, 1.5, 0.3), 1, Lod::Fine, &r);
        assert!(tiny.iter().any(|m| m.tag == TAG_GLASS));
        assert!(meshes(&wall(), &placed(Kind::Void, 1.0, 0.0, 2.0), 1, Lod::Fine, &r).is_empty());
    }

    #[test]
    fn identical_openings_share_one_prototype() {
        let r = DetailRules::default();
        let a = placed(Kind::Window, 1.4, 0.9, 1.5);
        let (t, p) = opening_type(&wall(), &a, &r).unwrap();
        let first = cache::opening(&t, &p, Lod::Fine, &r);
        let mut b = a;
        b.along = 3.5;
        let (t2, p2) = opening_type(&wall(), &b, &r).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &cache::opening(&t2, &p2, Lod::Fine, &r)));
    }
}
