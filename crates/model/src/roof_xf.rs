//! Plan transforms of a roof spec, the helper behind `Target::Roof`. Orientation is kept under a
//! mirror: rings are reversed with their edge slopes, an extrusion swaps its ends and profile.

use crate::{RoofKind, RoofSpec};

type P = (f64, f64);

/// The spec with every plan point passed through `f`; `mirrored` when `f` flips handedness.
pub(crate) fn transformed(s: &RoofSpec, f: &dyn Fn(P) -> P, mirrored: bool) -> RoofSpec {
    let mut out = s.clone();
    match &mut out.kind {
        RoofKind::Footprint { ring, slopes, .. } => {
            ring.iter_mut().for_each(|p| *p = f(*p));
            if mirrored {
                let n = ring.len();
                ring.reverse();
                if n >= 2 && slopes.len() == n {
                    // edge j of the reversed ring is the old edge n-2-j, walked backwards
                    let old = slopes.clone();
                    for (j, v) in slopes.iter_mut().enumerate() {
                        *v = old[(2 * n - 2 - j) % n];
                    }
                }
            }
        }
        RoofKind::Extrusion {
            a, b, profile, ..
        } => {
            let l = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            let (na, nb) = (f(*a), f(*b));
            if mirrored {
                // the extrusion side is the left of a -> b; swapping the ends keeps it
                *a = nb;
                *b = na;
                profile.reverse();
                profile.iter_mut().for_each(|p| p.0 = l - p.0);
            } else {
                *a = na;
                *b = nb;
            }
        }
        RoofKind::Arrow {
            ring, tail, head, ..
        } => {
            ring.iter_mut().for_each(|p| *p = f(*p));
            *tail = f(*tail);
            *head = f(*head);
            if mirrored {
                ring.reverse();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flip_x(p: P) -> P {
        (-p.0, p.1)
    }

    fn footprint() -> RoofSpec {
        RoofSpec {
            level: 1,
            slab_type: 1,
            offset: 0.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Footprint {
                ring: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)],
                slopes: vec![Some(30.0), None, Some(20.0), None],
                mansard: None,
            },
        }
    }

    #[test]
    fn mirror_keeps_each_slope_on_its_physical_edge() {
        let s = footprint();
        let t = transformed(&s, &flip_x, true);
        let RoofKind::Footprint { ring, slopes, .. } = &t.kind else {
            panic!()
        };
        // the south edge (30 deg) is now the edge between (0,0) and (-4,0), walked towards x=0
        let i = ring.iter().position(|p| *p == (0.0, 0.0)).unwrap();
        let next = ring[(i + 1) % ring.len()];
        assert_eq!(next, (0.0, 3.0), "orientation reversed with the ring");
        let j = ring.iter().position(|p| *p == (-4.0, 0.0)).unwrap();
        assert_eq!(ring[(j + 1) % 4], (0.0, 0.0));
        assert_eq!(slopes[j], Some(30.0));
        assert_eq!(slopes.iter().flatten().count(), 2);
    }

    #[test]
    fn translation_keeps_slopes_and_order() {
        let s = footprint();
        let t = transformed(&s, &|p| (p.0 + 1.0, p.1 + 2.0), false);
        let (RoofKind::Footprint { slopes: a, .. }, RoofKind::Footprint { ring, slopes: b, .. }) =
            (&s.kind, &t.kind)
        else {
            panic!()
        };
        assert_eq!(a, b);
        assert_eq!(ring[0], (1.0, 2.0));
    }

    #[test]
    fn mirrored_extrusion_swaps_ends_and_profile() {
        let s = RoofSpec {
            kind: RoofKind::Extrusion {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                profile: vec![(0.0, 0.0), (1.0, 2.0), (6.0, 0.0)],
                depth: 4.0,
            },
            ..footprint()
        };
        let t = transformed(&s, &flip_x, true);
        let RoofKind::Extrusion {
            a, b, profile, depth,
        } = &t.kind
        else {
            panic!()
        };
        assert_eq!((*a, *b, *depth), ((-6.0, 0.0), (0.0, 0.0), 4.0));
        assert_eq!(profile.first(), Some(&(0.0, 0.0)));
        assert_eq!(profile[1], (5.0, 2.0));
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use crate::{LayerSpec, Model, Spacing, Target};

    fn roof_model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let st = m
            .add_slab_type(
                "R",
                &[LayerSpec {
                    material: "C".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let id = m
            .add_roof(&RoofSpec {
                level: lv,
                slab_type: st,
                offset: 0.0,
                overhang: 0.3,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                    slopes: vec![Some(30.0), None, Some(30.0), None],
                    mansard: None,
                },
            })
            .unwrap();
        (m, id)
    }

    fn ring(m: &Model, id: i64) -> Vec<(f64, f64)> {
        match m.roof(id).unwrap().kind {
            RoofKind::Footprint { ring, .. } => ring,
            _ => panic!(),
        }
    }

    #[test]
    fn move_is_one_undo_step() {
        let (mut m, id) = roof_model();
        let before = m.roof(id).unwrap();
        m.move_elements(&[Target::Roof(id)], (2.0, 1.0)).unwrap();
        assert_eq!(ring(&m, id)[0], (2.0, 1.0));
        m.undo().unwrap();
        assert_eq!(m.roof(id).unwrap(), before);
    }

    #[test]
    fn copy_and_array_make_whole_roofs() {
        let (mut m, id) = roof_model();
        let out = m
            .copy_elements(&[Target::Roof(id)], &[(10.0, 0.0)])
            .unwrap();
        let Target::Roof(n) = out[0] else { panic!() };
        assert_ne!(n, id);
        assert_eq!(ring(&m, n)[0], (10.0, 0.0));
        assert_eq!(m.roof(n).unwrap().slab_type, m.roof(id).unwrap().slab_type);
        m.undo().unwrap();
        assert!(m.roof(n).is_err(), "undo removes the copy");
        let arr = m
            .array_linear(&[Target::Roof(id)], 3, Spacing::Step((0.0, 8.0)))
            .unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn rotate_and_double_mirror_keep_the_shape() {
        let (mut m, id) = roof_model();
        let area = m.roof_quantities(id).unwrap().geom.area;
        m.rotate_elements(&[Target::Roof(id)], (0.0, 0.0), std::f64::consts::FRAC_PI_2, false)
            .unwrap();
        let a2 = m.roof_quantities(id).unwrap().geom.area;
        assert!((area - a2).abs() < 1e-6, "{area} vs {a2}");
        let before = m.roof(id).unwrap();
        for _ in 0..2 {
            m.mirror_elements(&[Target::Roof(id)], (1.0, 0.0), (1.0, 1.0), false)
                .unwrap();
        }
        let RoofKind::Footprint { ring: r0, slopes: s0, .. } = before.kind else {
            panic!()
        };
        let RoofKind::Footprint { ring: r1, slopes: s1, .. } = m.roof(id).unwrap().kind else {
            panic!()
        };
        assert_eq!(s0, s1);
        assert!(r0
            .iter()
            .zip(&r1)
            .all(|(p, q)| (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9));
    }
}
