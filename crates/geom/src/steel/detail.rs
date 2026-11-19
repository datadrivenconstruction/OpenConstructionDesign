// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Detailing sheet of a built connection: its bolt groups, plates, stiffeners and welds as the
//! catalogue's constructive rules laid them out, the tightest pitch and the rule findings.
//! Detailing only: no resistance is computed here (see [`super::forces`]), and the sheet says
//! that an engineer must check the connection.

use super::catalog::{catalog, fmt_mm};
use super::check::Finding;
use super::part::Role;
use super::schedule::{schedule, Schedule};
use super::Conn;

/// Said with every sheet.
pub const NOT_DESIGN: &str = "detailing by constructive rules (bolt series, pitches, edge \
    distances, minimum plate thickness); not a design calculation, no resistance is computed: \
    an engineer must check this connection";

/// Bolts of one size and class through one plate.
#[derive(Clone, Debug, PartialEq)]
pub struct BoltGroup {
    /// Mark of the first plate the bolts pass through.
    pub plate: String,
    /// `M20 8.8`.
    pub bolt: String,
    pub count: usize,
    /// Closest two bolt axes of the group, m; `None` for a single bolt.
    pub pitch: Option<f64>,
    /// The catalogue's least pitch for this hole, m.
    pub pitch_min: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub kind: String,
    pub groups: Vec<BoltGroup>,
    pub stiffeners: usize,
    pub schedule: Schedule,
    /// (rule key, text, is an error).
    pub findings: Vec<(&'static str, String, bool)>,
    pub note: &'static str,
}

impl Sheet {
    /// No rule is broken (spacing warnings aside).
    pub fn rules_kept(&self) -> bool {
        self.findings.iter().all(|f| !f.2)
    }

    /// One line per bolt group: `PL 20x150x420: 8 x M20 10.9, pitch 70 mm (min 53)`.
    pub fn lines(&self) -> Vec<String> {
        self.groups
            .iter()
            .map(|g| {
                let pitch = g.pitch.map_or(String::new(), |p| {
                    format!(", pitch {} mm (min {})", fmt_mm(p), fmt_mm(g.pitch_min))
                });
                format!("{}: {} x {}{pitch}", g.plate, g.count, g.bolt)
            })
            .collect()
    }
}

pub fn sheet(c: &Conn, findings: &[Finding]) -> Sheet {
    let sp = &catalog().spacing;
    let mut groups: Vec<(usize, String, Vec<usize>)> = vec![];
    for (i, (_, b)) in c.bolts.iter().enumerate() {
        if b.anchor {
            continue;
        }
        let plate = b.plies.first().copied().unwrap_or(usize::MAX);
        let bolt = format!("{} {}", b.size.name(), b.class);
        match groups.iter_mut().find(|g| g.0 == plate && g.1 == bolt) {
            Some(g) => g.2.push(i),
            None => groups.push((plate, bolt, vec![i])),
        }
    }
    let groups = groups
        .into_iter()
        .map(|(plate, bolt, ids)| {
            let mut pitch: Option<f64> = None;
            for (k, &i) in ids.iter().enumerate() {
                for &j in &ids[k + 1..] {
                    let d = (c.bolts[i].1.o - c.bolts[j].1.o).len();
                    pitch = Some(pitch.map_or(d, |p| p.min(d)));
                }
            }
            let hole = c.bolts[ids[0]].1.hole;
            BoltGroup {
                plate: c.parts.get(plate).map_or_else(|| "-".to_string(), |p| p.mark.clone()),
                bolt,
                count: ids.len(),
                pitch,
                pitch_min: sp.p1_min.min(sp.p2_min) * hole,
            }
        })
        .collect();
    Sheet {
        kind: c.kind.key().to_string(),
        groups,
        stiffeners: c.parts.iter().filter(|p| p.role == Role::Stiffener).count(),
        schedule: schedule(std::iter::once(c)),
        findings: findings.iter().map(|f| (f.rule.key(), f.text.clone(), f.rule.is_error())).collect(),
        note: NOT_DESIGN,
    }
}
