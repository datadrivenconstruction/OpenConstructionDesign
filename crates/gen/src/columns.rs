// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Column grid: columns at the crossings of named grid lines.

use crate::layout::mm;
use crate::params::Args;
use crate::{
    variant_of, GColumn, GGrid, GLayout, GenError, GenOut, Generator, ParamDef, ParamKind,
};
use serde_json::Value as J;

const MIN_SPAN: f64 = 3.0;
const MAX_SPAN: f64 = 15.0;
const MAX_LINES: usize = 40;
const OVERRUN: f64 = 1.0;

static PARAMS: [ParamDef; 7] = [
    ParamDef {
        name: "x_span",
        kind: ParamKind::Length,
        min: MIN_SPAN,
        max: MAX_SPAN,
        default: 6.0,
        help: "Bay along x, m",
    },
    ParamDef {
        name: "y_span",
        kind: ParamKind::Length,
        min: MIN_SPAN,
        max: MAX_SPAN,
        default: 6.0,
        help: "Bay along y, m",
    },
    ParamDef {
        name: "x_count",
        kind: ParamKind::Count,
        min: 2.0,
        max: MAX_LINES as f64,
        default: 5.0,
        help: "Grid lines across x",
    },
    ParamDef {
        name: "y_count",
        kind: ParamKind::Count,
        min: 2.0,
        max: MAX_LINES as f64,
        default: 4.0,
        help: "Grid lines across y",
    },
    ParamDef {
        name: "section_b",
        kind: ParamKind::Length,
        min: 0.2,
        max: 1.5,
        default: 0.4,
        help: "Column width, m",
    },
    ParamDef {
        name: "section_h",
        kind: ParamKind::Length,
        min: 0.2,
        max: 1.5,
        default: 0.4,
        help: "Column depth, m",
    },
    ParamDef {
        name: "height",
        kind: ParamKind::Length,
        min: 2.4,
        max: 8.0,
        default: 3.0,
        help: "Storey height, m",
    },
];

static VARIANTS: [&str; 3] = [
    "bays as requested",
    "fewer columns over the same extent",
    "narrower bays over the same extent",
];

pub(crate) struct ColumnGrid;

/// Letter axes of the shared series: A..Z without I and O, then AA, BB..
fn letters(i: usize) -> String {
    ddc_geom::axes::axis_letter(i, false)
}

/// Bays of the variant, keeping the extent when the span limits allow.
fn bays(span: f64, count: usize, variant: usize) -> (f64, usize) {
    let now = (count - 1) as f64;
    let extent = span * now;
    let want = match variant {
        1 => (now * 0.75).floor().max(1.0),
        2 => (now / 0.75).ceil().min((MAX_LINES - 1) as f64),
        _ => now,
    };
    let s = extent / want;
    if (MIN_SPAN..=MAX_SPAN).contains(&s) {
        (s, want as usize + 1)
    } else {
        (span, count)
    }
}

impl Generator for ColumnGrid {
    fn id(&self) -> &'static str {
        "column_grid"
    }
    fn params(&self) -> &'static [ParamDef] {
        &PARAMS
    }
    fn variants(&self) -> &'static [&'static str] {
        &VARIANTS
    }
    fn summary(&self) -> &'static str {
        "Regular column grid with named grid lines. Spans and sections are not a structural calculation."
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let a = Args::new(&PARAMS, params)?;
        let v = variant_of(seed);
        let (sx, nx) = bays(a.num("x_span"), a.count("x_count"), v);
        let (sy, ny) = bays(a.num("y_span"), a.count("y_count"), v);
        let section = (a.num("section_b"), a.num("section_h"));
        let (ex, ey) = (mm(sx * (nx - 1) as f64), mm(sy * (ny - 1) as f64));
        let mut l = GLayout {
            height: a.num("height"),
            ..GLayout::default()
        };
        for i in 0..nx {
            let x = mm(sx * i as f64);
            l.grids.push(GGrid {
                name: (i + 1).to_string(),
                a: (x, -OVERRUN),
                b: (x, ey + OVERRUN),
            });
        }
        for j in 0..ny {
            let y = mm(sy * j as f64);
            l.grids.push(GGrid {
                name: letters(j),
                a: (-OVERRUN, y),
                b: (ex + OVERRUN, y),
            });
        }
        for i in 0..nx {
            for j in 0..ny {
                l.columns.push(GColumn {
                    at: (mm(sx * i as f64), mm(sy * j as f64)),
                    rotation: 0.0,
                    section,
                });
            }
        }
        let count = (nx * ny) as f64;
        l.metrics = vec![
            ("columns", count),
            ("bay_x", sx),
            ("bay_y", sy),
            ("extent_x", ex),
            ("extent_y", ey),
            ("area_per_column", ex * ey / count),
        ];
        crate::check::validate_layout(&l).map_err(GenError::Infeasible)?;
        Ok(GenOut::Layout(l))
    }
}

#[cfg(test)]
mod tests {
    use super::letters;

    #[test]
    fn grid_letters_continue_after_z() {
        assert_eq!(letters(0), "A");
        assert_eq!(letters(8), "J");
        assert_eq!(letters(23), "Z");
        assert_eq!(letters(24), "AA");
        assert_eq!(letters(39), "RR");
    }
}
