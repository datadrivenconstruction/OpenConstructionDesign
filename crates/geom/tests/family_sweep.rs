//! Every shipped family builds across its declared parameter ranges: the span of the default
//! and the type values of each numeric parameter (min, max, mid, every type value, seeded
//! random values), one parameter at a time, at every level of detail. A required part (not a
//! void, not a plant tone skin, not an open mesh) that collapses to no volume fails the build.

use ddc_geom::component::plant::Organ;
use ddc_geom::component::{Built, Family, Kind, Lod, Shape, Value};

const LIBRARY: &str = include_str!("../../model/data/library.ddcf");
const LODS: [Lod; 3] = [Lod::Coarse, Lod::Medium, Lod::Fine];
const RANDOM: usize = 3;

fn families() -> Vec<Family> {
    LIBRARY
        .split("\n---")
        .filter(|s| s.lines().any(|l| l.trim_start().starts_with("family ")))
        .map(|s| Family::parse(s.trim_start_matches('\n')).unwrap_or_else(|e| panic!("{e}")))
        .collect()
}

fn family(name: &str) -> Family {
    families()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no family {name}"))
}

/// Fixed-seed xorshift: the sweep is the same on every run.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Values a sweep tries for one numeric parameter, rounded for counts and flags.
fn trial_values(f: &Family, name: &str, kind: Kind, rng: &mut Rng) -> Vec<f64> {
    let p = f.param(name).unwrap();
    let mut seen: Vec<f64> = vec![];
    if let Value::Num(v) = p.default {
        seen.push(v);
    }
    for t in &f.types {
        for (k, v) in &t.values {
            if let (true, Value::Num(x)) = (k == name, v) {
                seen.push(*x);
            }
        }
    }
    let lo = seen.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = seen.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut out = vec![lo, hi, 0.5 * (lo + hi)];
    out.extend(seen);
    for _ in 0..RANDOM {
        out.push(lo + (hi - lo) * rng.unit());
    }
    for v in &mut out {
        *v = match kind {
            Kind::Int => v.round(),
            Kind::Bool => (*v >= 0.5) as i32 as f64,
            _ => *v,
        };
    }
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// A part the family cannot do without: its collapse to no volume is a defect.
fn required(f: &Family, part: &str) -> bool {
    f.solids.iter().find(|s| s.name == part).is_some_and(|s| {
        !s.void
            && !s.alt
            && !matches!(
                s.shape,
                Shape::Mesh { .. } | Shape::Plant { organ: Organ::Shade | Organ::Light | Organ::Streak, .. }
            )
    })
}

fn build(f: &Family, values: &[(String, Value)], lod: Lod) -> Result<Built, String> {
    let (inst, ty): (Vec<_>, Vec<_>) = values
        .iter()
        .cloned()
        .partition(|(k, _)| f.param(k).is_some_and(|p| p.instance));
    let r = f.resolve(&ty, &inst).map_err(|e| e.to_string())?;
    let b = f.build(&r, lod).map_err(|e| e.to_string())?;
    if b.volume.is_nan() || b.volume <= 0.0 {
        return Err(format!("total volume {}", b.volume));
    }
    for p in b.parts.iter().filter(|p| p.visible && required(f, &p.name)) {
        if p.volume.is_nan() || p.volume <= 0.0 {
            return Err(format!("{}: volume {}", p.name, p.volume));
        }
    }
    Ok(b)
}

#[test]
fn every_family_builds_across_its_declared_ranges() {
    let mut rng = Rng(0x5EED_F00D_D0C5_1234);
    let (mut fails, mut runs, mut n) = (vec![], 0usize, 0usize);
    for f in families() {
        n += 1;
        let mut trials: Vec<(String, Vec<(String, Value)>)> = vec![("default".into(), vec![])];
        for t in &f.types {
            trials.push((format!("type {}", t.name), t.values.clone()));
        }
        for p in &f.params {
            if p.kind.is_text() || p.formula.is_some() || f.ext.has_text_formula(&p.name) {
                continue;
            }
            for v in trial_values(&f, &p.name, p.kind, &mut rng) {
                trials.push((format!("{}={v}", p.name), vec![(p.name.clone(), Value::Num(v))]));
            }
        }
        for (what, values) in &trials {
            for lod in LODS {
                runs += 1;
                if let Err(e) = build(&f, values, lod) {
                    fails.push(format!("{} [{what}] {lod:?}: {e}", f.name));
                }
            }
        }
    }
    assert!(n >= 100, "{n} families");
    assert!(fails.is_empty(), "{} of {runs} builds fail:\n{}", fails.len(), fails.join("\n"));
}

/// Shade and streak are tone skins: a leafy broadleaf tree builds them at medium and fine
/// with no volume of their own, and the tree still has volume.
#[test]
fn tree_shade_and_streak_build_as_skins() {
    let f = family("Tree");
    for lod in [Lod::Medium, Lod::Fine] {
        let b = build(&f, &[], lod).unwrap_or_else(|e| panic!("{lod:?}: {e}"));
        for organ in ["Shade", "Streak"] {
            let p = b.parts.iter().find(|p| p.name == organ).unwrap_or_else(|| panic!("{organ}"));
            assert!(p.visible && p.volume == 0.0, "{organ} {lod:?}: {p:?}");
        }
        let wood = b.parts.iter().find(|p| p.name == "Stem").unwrap();
        assert!(wood.volume > 0.0 && b.volume > wood.volume, "{lod:?}");
    }
}

/// A full-width or wider door leaves a Wall-wide front return each side.
#[test]
fn lift_car_front_returns_survive_a_full_width_door() {
    let f = family("Lift Car");
    let num = |k: &str, v: f64| (k.to_string(), Value::Num(v));
    for door in [1.1, 1.06, 1.3] {
        let b = build(&f, &[num("Width", 1.1), num("DoorW", door)], Lod::Fine)
            .unwrap_or_else(|e| panic!("DoorW {door}: {e}"));
        for side in ["FrontL", "FrontR"] {
            let p = b.parts.iter().find(|p| p.name == side).unwrap();
            // Wall 40mm x Wall 40mm x (2200 - 80 - 40)mm
            assert!((p.volume - 0.04 * 0.04 * 2.08).abs() < 1e-9, "{side} DoorW {door}: {}", p.volume);
        }
    }
    // a normal door keeps the panels it always had
    let b = build(&f, &[], Lod::Fine).unwrap();
    let p = b.parts.iter().find(|p| p.name == "FrontL").unwrap();
    assert!((p.volume - 0.1 * 0.04 * 2.08).abs() < 1e-9, "{}", p.volume);
}
