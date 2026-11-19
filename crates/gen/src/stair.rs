// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A stair between two levels, by the comfort rule of the geometry crate.

use crate::params::Args;
use crate::{variant_of, GLayout, GStair, GenError, GenOut, Generator, ParamDef, ParamKind};
use crate::project::rules::rule;
use ddc_geom::stair::{Shape, Stair, COMFORT_TARGET};
use serde_json::Value as J;

const GAP: f64 = 0.1;
const MIN_RISER: f64 = 0.15;

static PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: "shape",
        kind: ParamKind::Choice(&["straight", "l", "u", "spiral"]),
        min: 0.0,
        max: 0.0,
        default: 2.0,
        help: "Straight, quarter turn, half turn or spiral",
    },
    ParamDef {
        name: "width",
        kind: ParamKind::Length,
        min: 0.8,
        max: 2.4,
        default: 1.2,
        help: "Flight width, m",
    },
    ParamDef {
        name: "height",
        kind: ParamKind::Length,
        min: 2.1,
        max: 6.0,
        default: 3.0,
        help: "Floor to floor, m",
    },
    ParamDef {
        name: "max_riser",
        kind: ParamKind::Length,
        min: MIN_RISER,
        max: 0.2,
        default: 0.18,
        help: "Largest riser, m",
    },
    ParamDef {
        name: "landing",
        kind: ParamKind::Length,
        min: 0.8,
        max: 3.0,
        default: 1.2,
        help: "Landing depth of a half turn, m",
    },
];

static VARIANTS: [&str; 3] = [
    "shape and riser as requested",
    "lower risers, longer run",
    "a more compact turn",
];

pub(crate) struct StairFlight;

const SPIRAL_CORE: f64 = 0.3;

fn shape_of(name: &str, width: f64, height: f64, riser: f64) -> Result<Shape, GenError> {
    Ok(match name {
        "straight" => Shape::Straight,
        "l" => Shape::L {
            left: true,
            first: None,
        },
        "u" => Shape::U {
            left: true,
            first: None,
            gap: GAP,
        },
        "spiral" => spiral(width, height, riser),
        other => return Err(GenError::Infeasible(format!("stair: no shape {other}"))),
    })
}

/// Walk line at the comfort target: the risers set the going, the going sets the turn.
fn spiral(width: f64, height: f64, riser: f64) -> Shape {
    let radius = SPIRAL_CORE + width;
    let walk = (SPIRAL_CORE + radius) / 2.0;
    let n = (height / riser).ceil().max(1.0);
    let going = COMFORT_TARGET - 2.0 * height / n;
    Shape::Spiral {
        radius,
        core: SPIRAL_CORE,
        turn: n * going / walk,
    }
}

/// Rise per full turn must clear the head by the rule, or the spiral is refused.
fn headroom_check(shape: &Shape, height: f64) -> Result<(), GenError> {
    if let Shape::Spiral { turn, .. } = *shape {
        let rise = height * std::f64::consts::TAU / turn;
        let need = rule("stair.spiral_headroom");
        if rise < need {
            return Err(GenError::Infeasible(format!(
                "spiral: rise per turn {rise:.2} m is under stair.spiral_headroom {need:.2} m"
            )));
        }
    }
    Ok(())
}

fn footprint(st: &Stair) -> (f64, f64) {
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    let rings = st
        .flights
        .iter()
        .map(|f| f.ring(st.tread))
        .chain(st.landings.iter().map(|l| l.ring.clone()));
    for r in rings {
        for p in r {
            lo = (lo.0.min(p.x), lo.1.min(p.y));
            hi = (hi.0.max(p.x), hi.1.max(p.y));
        }
    }
    (hi.0 - lo.0, hi.1 - lo.1)
}

impl Generator for StairFlight {
    fn id(&self) -> &'static str {
        "stair"
    }
    fn params(&self) -> &'static [ParamDef] {
        &PARAMS
    }
    fn variants(&self) -> &'static [&'static str] {
        &VARIANTS
    }
    fn summary(&self) -> &'static str {
        "A stair by the 2R+T comfort rule. Local stair rules are not checked."
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let a = Args::new(&PARAMS, params)?;
        let asked = a.choice("shape");
        let mut riser = a.num("max_riser");
        let mut shape = asked;
        match variant_of(seed) {
            1 => riser = (riser - 0.015).max(MIN_RISER),
            2 => shape = if asked == "u" { "l" } else { "u" },
            _ => {}
        }
        let g = GStair {
            origin: (0.0, 0.0),
            angle: 0.0,
            shape: shape_of(shape, a.num("width"), a.num("height"), riser)?,
            width: a.num("width"),
            max_riser: riser,
            landing: a.num("landing"),
            height: a.num("height"),
        };
        let st = g
            .def()
            .layout()
            .map_err(|e| GenError::Infeasible(format!("stair: {e:?}")))?;
        headroom_check(&g.shape, g.height)?;
        let (w, h) = footprint(&st);
        let treads = st.risers.saturating_sub(st.flights.len()) as f64;
        let mut l = GLayout {
            height: g.height,
            ..GLayout::default()
        };
        l.metrics = vec![
            ("risers", st.risers as f64),
            ("riser", st.riser),
            ("tread", st.tread),
            ("run", st.tread * treads),
            ("plan_area", w * h),
        ];
        l.stairs.push(g);
        crate::check::validate_layout(&l).map_err(GenError::Infeasible)?;
        Ok(GenOut::Layout(l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spiral_is_its_own_shape_and_an_unknown_name_is_refused() {
        assert!(matches!(shape_of("spiral", 1.2, 3.0, 0.18), Ok(Shape::Spiral { .. })));
        assert!(shape_of("pizza", 1.2, 3.0, 0.18).is_err());
    }

    #[test]
    fn a_spiral_that_climbs_too_little_per_turn_is_refused_with_the_rule() {
        let ok = StairFlight.build(
            &json!({"shape": "spiral", "width": 1.2, "height": 3.0, "max_riser": 0.18}),
            0,
        );
        assert!(ok.is_ok());
        let narrow = StairFlight.build(
            &json!({"shape": "spiral", "width": 0.8, "height": 3.0, "max_riser": 0.15}),
            0,
        );
        match narrow {
            Err(GenError::Infeasible(msg)) => assert!(msg.contains("stair.spiral_headroom"), "{msg}"),
            _ => panic!("a spiral under the headroom rule must be refused"),
        }
    }
}
