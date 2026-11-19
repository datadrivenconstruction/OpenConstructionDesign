// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bolt, weld and plate schedules of one or many connections.

use super::part::{PartQty, Role};
use super::Conn;
use crate::poly;

/// Steel density, kg/m3.
pub const DENSITY: f64 = 7850.0;

#[derive(Clone, Debug, PartialEq)]
pub struct BoltRow {
    /// `M20`.
    pub size: String,
    pub d: f64,
    pub class: String,
    pub length: f64,
    pub anchor: bool,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WeldRow {
    pub leg: f64,
    pub runs: usize,
    /// Total length, m.
    pub length: f64,
    /// Deposited metal, kg.
    pub mass: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlateRow {
    /// `PL 12x150x260`.
    pub mark: String,
    pub role: Role,
    pub t: f64,
    pub count: usize,
    /// Net area of one plate (holes out), m2.
    pub area: f64,
    /// All plates of the row, kg.
    pub mass: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schedule {
    pub bolts: Vec<BoltRow>,
    pub welds: Vec<WeldRow>,
    pub plates: Vec<PlateRow>,
    /// Balls, sleeves and cones: (role, mark, count, kg).
    pub other: Vec<(Role, String, usize, f64)>,
}

impl Schedule {
    pub fn bolt_count(&self) -> usize {
        self.bolts.iter().map(|r| r.count).sum()
    }
    pub fn weld_length(&self) -> f64 {
        self.welds.iter().map(|r| r.length).sum()
    }
    pub fn plate_mass(&self) -> f64 {
        self.plates.iter().map(|r| r.mass).sum()
    }
}

/// One schedule over all `conns`, rows sorted by size.
pub fn schedule<'a>(conns: impl IntoIterator<Item = &'a Conn>) -> Schedule {
    let mut s = Schedule::default();
    for c in conns {
        for p in &c.parts {
            match &p.qty {
                PartQty::Bolt {
                    d,
                    class,
                    length,
                    anchor,
                } => {
                    let name = format!("M{}", super::catalog::fmt_mm(*d));
                    match s.bolts.iter_mut().find(|r| {
                        r.size == name
                            && r.class == *class
                            && same(r.length, *length)
                            && r.anchor == *anchor
                    }) {
                        Some(r) => r.count += 1,
                        None => s.bolts.push(BoltRow {
                            size: name,
                            d: *d,
                            class: class.clone(),
                            length: *length,
                            anchor: *anchor,
                            count: 1,
                        }),
                    }
                }
                PartQty::Weld { leg, length } => {
                    let mass = leg * leg / 2.0 * length * DENSITY;
                    match s.welds.iter_mut().find(|r| same(r.leg, *leg)) {
                        Some(r) => {
                            r.runs += 1;
                            r.length += length;
                            r.mass += mass;
                        }
                        None => s.welds.push(WeldRow {
                            leg: *leg,
                            runs: 1,
                            length: *length,
                            mass,
                        }),
                    }
                }
                PartQty::Plate { t, outline, holes } => {
                    let holes_area: f64 = holes
                        .iter()
                        .map(|(_, d)| std::f64::consts::PI * d * d / 4.0)
                        .sum();
                    let area = poly::area(outline).abs() - holes_area;
                    let mass = area * t * DENSITY;
                    match s.plates.iter_mut().find(|r| {
                        r.mark == p.mark && r.role == p.role && (r.area - area).abs() < 1e-6
                    }) {
                        Some(r) => {
                            r.count += 1;
                            r.mass += mass;
                        }
                        None => s.plates.push(PlateRow {
                            mark: p.mark.clone(),
                            role: p.role,
                            t: *t,
                            count: 1,
                            area,
                            mass,
                        }),
                    }
                }
                PartQty::Solid { volume } => {
                    if p.role.is_fastener_piece() {
                        continue;
                    }
                    let kg = volume * DENSITY;
                    match s.other.iter_mut().find(|r| r.0 == p.role && r.1 == p.mark) {
                        Some(r) => {
                            r.2 += 1;
                            r.3 += kg;
                        }
                        None => s.other.push((p.role, p.mark.clone(), 1, kg)),
                    }
                }
            }
        }
    }
    s.bolts.sort_by(|a, b| {
        a.anchor
            .cmp(&b.anchor)
            .then(a.d.total_cmp(&b.d))
            .then(a.class.cmp(&b.class))
            .then(a.length.total_cmp(&b.length))
    });
    s.welds.sort_by(|a, b| a.leg.total_cmp(&b.leg));
    s.plates
        .sort_by(|a, b| a.t.total_cmp(&b.t).then(a.mark.cmp(&b.mark)));
    s
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}
