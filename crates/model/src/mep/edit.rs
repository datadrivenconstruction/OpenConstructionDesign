// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Run property edits: elevation, slope, pipe material, duct shape. Pure functions that return a
//! new `RunSpec` for `update_mep_run`. Stored points already carry the slope, so every result has
//! `slope = 0`: otherwise the write would apply the slope a second time.

use super::*;

/// Duct section shape as shown in the properties panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuctShape {
    Rect,
    Round,
    Oval,
}

impl DuctShape {
    pub const ALL: [DuctShape; 3] = [DuctShape::Rect, DuctShape::Round, DuctShape::Oval];
    pub fn key(self) -> &'static str {
        match self {
            DuctShape::Rect => "rect",
            DuctShape::Round => "round",
            DuctShape::Oval => "oval",
        }
    }
    pub fn of(shape: Shape) -> Self {
        match shape {
            Shape::Rect { .. } => DuctShape::Rect,
            Shape::Round { .. } => DuctShape::Round,
            Shape::Oval { .. } => DuctShape::Oval,
        }
    }
}

/// Slope of a run in percent, read from its first segment that travels in plan (a vertical
/// joint does not count).
pub fn run_slope_pct(s: &RunSpec) -> f64 {
    for w in s.points.windows(2) {
        let run = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
        let dz = w[1].2 - w[0].2;
        if run > 1e-9 && dz.abs() <= run {
            return (-dz / run * 100.0).max(0.0);
        }
    }
    0.0
}

/// World elevation of the axis at the start of the run, m.
pub fn start_z(s: &RunSpec) -> f64 {
    s.points.first().map_or(0.0, |p| p.2)
}

fn baked(s: &RunSpec) -> RunSpec {
    RunSpec {
        slope: 0.0,
        ..s.clone()
    }
}

/// Every point shifted so the axis starts at world elevation `z`.
pub fn with_elevation(s: &RunSpec, z: f64) -> Result<RunSpec, Error> {
    if !z.is_finite() {
        return invalid("elevation is not a number");
    }
    let dz = z - start_z(s);
    let mut r = baked(s);
    for p in &mut r.points {
        p.2 += dz;
    }
    Ok(r)
}

/// Slope `pct` percent downhill along the run; steep segments (vertical joints) keep their drop.
pub fn with_slope(s: &RunSpec, pct: f64) -> Result<RunSpec, Error> {
    if !(pct.is_finite() && pct >= 0.0) {
        return invalid("slope must be zero or positive");
    }
    let p: Vec<V3> = s.points.iter().map(|&(x, y, z)| v3(x, y, z)).collect();
    let mut r = baked(s);
    r.points = mep::apply_slope(&p, pct).into_iter().map(p3).collect();
    Ok(r)
}

/// The catalog size of material `m` closest to `dn`.
pub fn nearest_dn(m: PipeMaterial, dn: u32) -> Option<u32> {
    m.sizes()
        .into_iter()
        .map(|s| s.dn)
        .min_by_key(|d| d.abs_diff(dn))
}

/// Pipe material: the section comes from the catalog; a missing DN snaps to the nearest one.
pub fn with_material(s: &RunSpec, m: PipeMaterial) -> Result<RunSpec, Error> {
    if s.section.domain != Domain::Pipe {
        return invalid("material applies to pipes only");
    }
    let dn = nearest_dn(m, s.dn.unwrap_or(0))
        .ok_or_else(|| Error::Invalid("empty catalog".into()))?;
    let size = m
        .by_dn(dn)
        .ok_or_else(|| Error::Invalid(format!("no DN{dn} in {} catalog", m.name())))?;
    let mut r = baked(s);
    r.section.shape = Shape::Round { d: size.od };
    r.section.wall = size.wall;
    r.material = Some(m);
    r.dn = Some(dn);
    Ok(r)
}

/// Pipe size: `dn` must exist in the catalog of the current material.
pub fn with_dn(s: &RunSpec, dn: u32) -> Result<RunSpec, Error> {
    let m = s.material.unwrap_or(PipeMaterial::Steel);
    if m.by_dn(dn).is_none() {
        return invalid(format!("no DN{dn} in {} catalog", m.name()));
    }
    with_material(
        &RunSpec {
            dn: Some(dn),
            ..s.clone()
        },
        m,
    )
}

/// Duct shape. To round: equal-friction diameter; from round: both sides equal the diameter.
pub fn with_duct_shape(s: &RunSpec, to: DuctShape) -> Result<RunSpec, Error> {
    if s.section.domain != Domain::Duct {
        return invalid("shape applies to ducts only");
    }
    let (w, h) = s.section.shape.wh();
    let shape = match (to, s.section.shape) {
        (DuctShape::Round, Shape::Round { d }) => Shape::Round { d },
        (DuctShape::Round, _) => Shape::Round {
            d: 1.3 * (w * h).powf(0.625) / (w + h).powf(0.25),
        },
        (DuctShape::Rect, _) => Shape::Rect { w, h },
        (DuctShape::Oval, _) => Shape::Oval { w, h },
    };
    if !shape.valid() {
        return invalid("section must be positive");
    }
    let mut r = baked(s);
    r.section.shape = shape;
    r.dn = None;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe(pts: Vec<(f64, f64, f64)>) -> RunSpec {
        RunSpec::pipe(1, 1, PipeMaterial::Steel, 50, pts).unwrap()
    }

    #[test]
    fn slope_roundtrip_is_stable() {
        let s = pipe(vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0), (10.0, 5.0, 3.0)]);
        let a = with_slope(&s, 2.0).unwrap();
        assert!((a.points[1].2 - 2.8).abs() < 1e-9);
        assert!((a.points[2].2 - 2.7).abs() < 1e-9);
        assert!((run_slope_pct(&a) - 2.0).abs() < 1e-9);
        let b = with_slope(&a, 2.0).unwrap();
        assert_eq!(a.points, b.points, "baking twice changes nothing");
        let flat = with_slope(&b, 0.0).unwrap();
        assert!(flat.points.iter().all(|p| (p.2 - 3.0).abs() < 1e-9));
    }

    #[test]
    fn vertical_joint_keeps_its_drop() {
        let s = pipe(vec![(0.0, 0.0, 3.0), (0.0, 0.0, 1.0), (4.0, 0.0, 1.0)]);
        let a = with_slope(&s, 1.0).unwrap();
        assert!((a.points[1].2 - 1.0).abs() < 1e-9);
        assert!((a.points[2].2 - 0.96).abs() < 1e-9);
        assert!(with_slope(&s, -1.0).is_err());
    }

    #[test]
    fn elevation_shifts_every_point() {
        let s = pipe(vec![(0.0, 0.0, 3.0), (4.0, 0.0, 2.9)]);
        let a = with_elevation(&s, 2.0).unwrap();
        assert!((a.points[0].2 - 2.0).abs() < 1e-9);
        assert!((a.points[1].2 - 1.9).abs() < 1e-9);
        assert!(with_elevation(&s, f64::NAN).is_err());
    }

    #[test]
    fn material_swaps_catalog_section_and_snaps_dn() {
        let s = pipe(vec![(0.0, 0.0, 3.0), (4.0, 0.0, 3.0)]);
        let c = with_material(&s, PipeMaterial::Copper).unwrap();
        assert_eq!(c.material, Some(PipeMaterial::Copper));
        assert_eq!(c.dn, Some(50));
        assert!((c.section.shape.wh().0 - 0.054).abs() < 1e-9);
        let big = RunSpec {
            dn: Some(300),
            ..s.clone()
        };
        assert_eq!(
            with_material(&big, PipeMaterial::Pp).unwrap().dn,
            Some(100)
        );
        let d = with_dn(&c, 25).unwrap();
        assert!((d.section.shape.wh().0 - 0.028).abs() < 1e-9);
        assert!(with_dn(&c, 7).is_err());
    }

    #[test]
    fn duct_shape_converts_and_pipes_refuse() {
        let d = RunSpec::new(
            1,
            1,
            Section::new(Domain::Duct, Shape::Rect { w: 0.4, h: 0.25 }),
            vec![(0.0, 0.0, 3.0), (4.0, 0.0, 3.0)],
        );
        let r = with_duct_shape(&d, DuctShape::Round).unwrap();
        let Shape::Round { d: dia } = r.section.shape else {
            panic!("round expected");
        };
        assert!(dia > 0.25 && dia < 0.4, "equivalent diameter {dia}");
        assert_eq!(DuctShape::of(r.section.shape), DuctShape::Round);
        let back = with_duct_shape(&r, DuctShape::Rect).unwrap();
        assert_eq!(back.section.shape, Shape::Rect { w: dia, h: dia });
        let p = pipe(vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)]);
        assert!(with_duct_shape(&p, DuctShape::Oval).is_err());
        assert!(with_material(&d, PipeMaterial::Pvc).is_err());
    }

    #[test]
    fn edit_is_one_undo_step_and_slope_is_not_doubled() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let sys = m.add_mep_system("S", SystemKind::Sanitary).unwrap();
        let mut spec = pipe(vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0)]);
        spec.system = sys;
        spec.level = l;
        spec.slope = 2.0;
        let id = m.add_mep_run(&spec).unwrap();
        let before = m.mep_run(id).unwrap();
        assert!((before.points[1].2 - 2.8).abs() < 1e-9);
        let cu = with_material(&before, PipeMaterial::Copper).unwrap();
        m.update_mep_run(id, &cu).unwrap();
        let after = m.mep_run(id).unwrap();
        assert!((after.points[1].2 - 2.8).abs() < 1e-9);
        assert_eq!(after.material, Some(PipeMaterial::Copper));
        m.undo().unwrap();
        assert_eq!(m.mep_run(id).unwrap().material, Some(PipeMaterial::Steel));
    }
}
