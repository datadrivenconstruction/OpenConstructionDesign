// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Data-driven cable rules (table in `data/cable_rules.json`): breaker and section sizing,
//! cable length by laying method, and the cable log. Deterministic; every finding cites a
//! rule id.

use serde::Deserialize;

use super::circuit::{Circuit, CircuitKind};
use crate::{Error, Model};

const TABLE: &str = include_str!("../../data/cable_rules.json");

/// Where a table comes from and how far it is trusted.
#[derive(Clone, Debug, Deserialize)]
pub struct Source {
    pub doc: String,
    pub url: String,
}

/// One ampacity table: rows of section then one value per column (null: not given).
#[derive(Clone, Debug, Deserialize)]
pub struct AmpTable {
    pub id: String,
    standard: String,
    material: String,
    kind: Option<String>,
    loaded: Option<u32>,
    columns: Vec<String>,
    pub source: Source,
    pub confidence: String,
    #[serde(default)]
    pub note: Option<String>,
    rows: Vec<Vec<Option<f64>>>,
}

impl AmpTable {
    fn get(&self, section: f64, col: &str) -> Result<f64, String> {
        let c = self
            .columns
            .iter()
            .position(|x| x == col)
            .ok_or_else(|| format!("{}: no column {col}", self.id))?;
        let row = self
            .rows
            .iter()
            .find(|r| r.first().is_some_and(|s| s.is_some_and(|s| (s - section).abs() < 1e-9)))
            .ok_or_else(|| {
                let n = self.note.as_deref().unwrap_or("");
                format!("{}: no row for {section} mm2. {n}", self.id)
            })?;
        row.get(c + 1)
            .copied()
            .flatten()
            .ok_or_else(|| format!("{}: {section} mm2 is not given for {col}", self.id))
    }
}

#[derive(Clone, Debug, Deserialize)]
struct X0Table {
    ohm_per_km: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Deserialize)]
struct X0 {
    cu: X0Table,
    al: X0Table,
}

#[derive(Clone, Debug, Deserialize)]
struct Derating {
    ref_c: f64,
    temp_factors: Vec<(f64, f64)>,
    group_factors: Vec<(u32, f64)>,
}

#[derive(Clone, Debug, Deserialize)]
struct PueLaying {
    kind: String,
    columns: std::collections::HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Laying {
    pub id: String,
    /// Extra length for bends and slack, % of the route.
    pub slack_pct: f64,
    /// Reserve at each end, m.
    pub reserve_m: f64,
    pue: PueLaying,
    iec: Option<String>,
}

/// A load type with its power factor.
#[derive(Clone, Debug, Deserialize)]
pub struct LoadType {
    pub id: String,
    pub cos_phi: f64,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Clone, Debug, Deserialize)]
struct KindRule {
    min_section: f64,
    max_drop_pct: f64,
    load_type: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Kinds {
    power: KindRule,
    lighting: KindRule,
}

#[derive(Clone, Debug, Deserialize)]
struct Resistivity {
    cu: f64,
    al: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct DropRule {
    over_m: f64,
    pct_per_m: f64,
    max_extra_pct: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CableRules {
    pub sections: Vec<f64>,
    pub breakers: Vec<f64>,
    resistivity_op: Resistivity,
    x0: X0,
    pub ampacity: Vec<AmpTable>,
    derating: Derating,
    pub laying: Vec<Laying>,
    pub load_types: Vec<LoadType>,
    kinds: Kinds,
    drop: DropRule,
}

/// Conductor material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conductor {
    Cu,
    Al,
}

impl Conductor {
    pub fn name(self) -> &'static str {
        match self {
            Conductor::Cu => "cu",
            Conductor::Al => "al",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Conductor::Cu, Conductor::Al].into_iter().find(|c| c.name() == s)
    }
}

/// Which family of ampacity tables a project uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Standard {
    /// PUE 1.3.4 to 1.3.7: the main one for Russian projects.
    #[default]
    Pue,
    /// GOST R 50571.5.52 (IEC 60364-5-52).
    Iec,
}

impl Standard {
    pub fn name(self) -> &'static str {
        match self {
            Standard::Pue => "pue",
            Standard::Iec => "iec",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Standard::Pue, Standard::Iec].into_iter().find(|c| c.name() == s)
    }
}

/// Inputs of one circuit check.
#[derive(Clone, Debug)]
pub struct CableInput {
    pub kind: CircuitKind,
    pub standard: Standard,
    /// Phase voltage for one pole, line voltage for three.
    pub voltage: f64,
    pub poles: u32,
    /// Active power, W.
    pub load: f64,
    pub cos_phi: f64,
    pub material: Conductor,
    /// Route length to the farthest device, m.
    pub length: f64,
    pub breaker: f64,
    pub section: f64,
    pub laying: String,
    /// Ambient temperature, C (table reference 30).
    pub ambient_c: f64,
    /// Circuits bunched together, this one included.
    pub grouped: u32,
    /// Allowed voltage drop, %; None takes the table value of the kind.
    pub max_drop: Option<f64>,
    /// Drop measured along the real network, %; None takes the whole load at the full length.
    pub measured_drop: Option<f64>,
}

/// One rule result: id, pass or fail, the numbers behind it, and why when no table value exists.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleFinding {
    pub rule: &'static str,
    pub ok: bool,
    pub value: f64,
    pub limit: f64,
    pub note: Option<String>,
}

/// A sized circuit.
#[derive(Clone, Debug, PartialEq)]
pub struct Sized {
    pub breaker: f64,
    pub section: f64,
    pub drop_pct: f64,
}

impl CableRules {
    pub fn builtin() -> Self {
        serde_json::from_str(TABLE).expect("cable_rules.json is valid")
    }

    /// The built-in table, parsed once.
    pub fn shared() -> &'static CableRules {
        static RULES: std::sync::OnceLock<CableRules> = std::sync::OnceLock::new();
        RULES.get_or_init(Self::builtin)
    }

    fn laying(&self, id: &str) -> Option<&Laying> {
        self.laying.iter().find(|l| l.id == id)
    }

    fn kind(&self, k: CircuitKind) -> &KindRule {
        match k {
            CircuitKind::Power => &self.kinds.power,
            CircuitKind::Lighting => &self.kinds.lighting,
        }
    }

    /// Table limit of the voltage drop for a kind, %, for a short run.
    pub fn max_drop(&self, k: CircuitKind) -> f64 {
        self.kind(k).max_drop_pct
    }

    /// The limit for a run of `length` m: 0.005 % per metre over 100 m, at most 0.5 % more.
    pub fn drop_limit(&self, base: f64, length: f64) -> f64 {
        let d = &self.drop;
        base + (d.pct_per_m * (length - d.over_m).max(0.0)).min(d.max_extra_pct)
    }

    /// Reference ambient temperature of the correction factors, C.
    pub fn ambient_ref_c(&self) -> f64 {
        self.derating.ref_c
    }

    /// The load type a kind starts with.
    pub fn default_load_type(&self, k: CircuitKind) -> &str {
        &self.kind(k).load_type
    }

    /// Power factor of a load type.
    pub fn cos_phi_of(&self, load_type: &str) -> Option<f64> {
        self.load_types.iter().find(|t| t.id == load_type).map(|t| t.cos_phi)
    }

    /// Default power factor of a kind (through its default load type).
    pub fn cos_phi(&self, k: CircuitKind) -> f64 {
        self.cos_phi_of(self.default_load_type(k)).unwrap_or(0.9)
    }

    /// Resistivity at the operating temperature, ohm*mm2/m.
    pub fn rho(&self, m: Conductor) -> f64 {
        match m {
            Conductor::Cu => self.resistivity_op.cu,
            Conductor::Al => self.resistivity_op.al,
        }
    }

    /// Inductive reactance, ohm/km, and whether it is an estimate: sections the standard gives no
    /// value for take the nearest smaller listed one (the smallest listed below its range).
    pub fn x0(&self, m: Conductor, section: f64) -> (f64, bool) {
        let t = match m {
            Conductor::Cu => &self.x0.cu,
            Conductor::Al => &self.x0.al,
        };
        if let Some(r) = t.ohm_per_km.iter().find(|r| (r.0 - section).abs() < 1e-9) {
            return (r.1, false);
        }
        let below = t.ohm_per_km.iter().rev().find(|r| r.0 < section);
        (below.unwrap_or(&t.ohm_per_km[0]).1, true)
    }

    /// Ampacity of a section at 30 C, no grouping, A. Err says why the tables have no value
    /// (unknown laying, no confirmed column, a row that is left out).
    pub fn ampacity(
        &self,
        std: Standard,
        m: Conductor,
        laying: &str,
        section: f64,
        poles: u32,
    ) -> Result<f64, String> {
        let l = self.laying(laying).ok_or_else(|| format!("unknown laying method {laying}"))?;
        let find = |f: &dyn Fn(&AmpTable) -> bool| self.ampacity.iter().find(|t| f(t));
        match std {
            Standard::Pue => {
                let key = if poles == 3 { "3" } else { "1" };
                let col = l.pue.columns.get(key).ok_or_else(|| format!("{laying}: no PUE column"))?;
                let t = find(&|t| {
                    t.standard == "pue"
                        && t.material == m.name()
                        && t.kind.as_deref() == Some(l.pue.kind.as_str())
                })
                .ok_or_else(|| format!("no PUE table for {} {}", m.name(), l.pue.kind))?;
                t.get(section, col)
            }
            Standard::Iec => {
                let col = l
                    .iec
                    .as_deref()
                    .ok_or_else(|| format!("no confirmed IEC column for laying {laying}"))?;
                let loaded = if poles == 3 { 3 } else { 2 };
                let t = find(&|t| {
                    t.standard == "iec" && t.material == m.name() && t.loaded == Some(loaded)
                })
                .ok_or_else(|| format!("no IEC table for {} with {loaded} loaded conductors", m.name()))?;
                t.get(section, col)
            }
        }
    }

    /// Ambient temperature factor from the next hotter table row (None above the table).
    pub fn temp_factor(&self, c: f64) -> Option<f64> {
        self.derating.temp_factors.iter().find(|t| t.0 + 1e-9 >= c).map(|t| t.1)
    }

    /// Grouping factor from the next larger table row, the last one beyond the table.
    pub fn group_factor(&self, n: u32) -> f64 {
        let n = n.max(1);
        self.derating
            .group_factors
            .iter()
            .find(|g| g.0 >= n)
            .or(self.derating.group_factors.last())
            .map_or(1.0, |g| g.1)
    }

    /// Ampacity with both derating factors, A.
    pub fn derated(&self, i: &CableInput) -> Result<f64, String> {
        let base = self.ampacity(i.standard, i.material, &i.laying, i.section, i.poles)?;
        let t = self
            .temp_factor(i.ambient_c)
            .ok_or_else(|| format!("{} C is above the correction table", i.ambient_c))?;
        Ok(base * t * self.group_factor(i.grouped))
    }

    /// Cable length to order for a route of `route` m: slack plus a reserve at both ends.
    pub fn cable_length(&self, laying: &str, route: f64) -> Option<f64> {
        let l = self.laying(laying)?;
        Some(route * (1.0 + l.slack_pct / 100.0) + 2.0 * l.reserve_m)
    }

    /// Voltage drop, V, for `amp_m` = sum of current x length over the cable parts, A*m:
    /// k * amp_m * (R cos + X sin), k 2 for one pole and sqrt 3 for three,
    /// R = rho / section, X = x0 of the section.
    pub fn volt_drop(&self, m: Conductor, poles: u32, section: f64, cos_phi: f64, amp_m: f64) -> f64 {
        let k = if poles == 3 { 3f64.sqrt() } else { 2.0 };
        let sin = (1.0 - cos_phi * cos_phi).max(0.0).sqrt();
        let r = self.rho(m) / section;
        let x = self.x0(m, section).0 / 1000.0;
        k * amp_m * (r * cos_phi + x * sin)
    }

    /// Line current, A: P / (U cos) for one pole, P / (sqrt 3 U cos) for three.
    pub fn current(poles: u32, voltage: f64, cos_phi: f64, load: f64) -> f64 {
        let phase = if poles == 3 { 3f64.sqrt() } else { 1.0 };
        load / (voltage * cos_phi * phase)
    }

    fn drop_pct(&self, i: &CableInput, current: f64) -> f64 {
        i.measured_drop.unwrap_or_else(|| {
            self.volt_drop(i.material, i.poles, i.section, i.cos_phi, current * i.length)
                / i.voltage
                * 100.0
        })
    }

    /// Checks one circuit against the table. Order is fixed.
    pub fn check(&self, i: &CableInput) -> Vec<RuleFinding> {
        let cur = Self::current(i.poles, i.voltage, i.cos_phi, i.load);
        let kr = self.kind(i.kind);
        let amp = self.derated(i);
        let drop = self.drop_pct(i, cur);
        let max_drop = self.drop_limit(i.max_drop.unwrap_or(kr.max_drop_pct), i.length);
        let std = self.breakers.iter().any(|b| (b - i.breaker).abs() < 1e-9);
        let f = |rule, ok, value, limit| RuleFinding { rule, ok, value, limit, note: None };
        let brk2 = match &amp {
            Ok(a) => f("EL-BRK-02", i.breaker <= a + 1e-9, i.breaker, *a),
            Err(why) => RuleFinding {
                note: Some(why.clone()),
                ..f("EL-BRK-02", false, i.breaker, 0.0)
            },
        };
        vec![
            f("EL-BRK-01", cur <= i.breaker + 1e-9, cur, i.breaker),
            brk2,
            f("EL-BRK-03", std, i.breaker, i.breaker),
            f("EL-CAB-01", i.section + 1e-9 >= kr.min_section, i.section, kr.min_section),
            f("EL-DRP-01", drop <= max_drop + 1e-9, drop, max_drop),
        ]
    }

    /// Smallest standard breaker and section that pass every rule, or None when even the
    /// largest section fails (long run: split the circuit). A measured drop is not reused.
    pub fn size(&self, i: &CableInput) -> Option<Sized> {
        let cur = Self::current(i.poles, i.voltage, i.cos_phi, i.load);
        let kr = self.kind(i.kind);
        let breaker = self.breakers.iter().copied().find(|b| *b + 1e-9 >= cur)?;
        let max_drop = self.drop_limit(i.max_drop.unwrap_or(kr.max_drop_pct), i.length);
        for &s in &self.sections {
            if s + 1e-9 < kr.min_section {
                continue;
            }
            let c = CableInput { section: s, measured_drop: None, ..i.clone() };
            let Ok(amp) = self.derated(&c) else { continue };
            let drop = self.drop_pct(&c, cur);
            if breaker <= amp + 1e-9 && drop <= max_drop + 1e-9 {
                return Some(Sized { breaker, section: s, drop_pct: drop });
            }
        }
        None
    }
}

/// One cable of the log: panel circuit to one consumer.
#[derive(Clone, Debug, PartialEq)]
pub struct CableRow {
    pub circuit: u32,
    pub name: String,
    pub from: i64,
    pub to: i64,
    pub section: f64,
    pub poles: u32,
    pub laying: String,
    /// Route length along the network, m.
    pub route: f64,
    /// Length to order, m.
    pub length: f64,
}

impl Model {
    /// Cable log of a panel: one row per consumer, length by laying method (the network
    /// route, not a modelled cable). Unrouted consumers are left out.
    pub fn cable_log(&self, panel: i64, laying: &str) -> Result<Vec<CableRow>, Error> {
        let rules = CableRules::builtin();
        let mut rows = vec![];
        for c in self.circuits(Some(panel))? {
            rows.extend(self.circuit_cable_rows(&rules, &c, laying)?);
        }
        Ok(rows)
    }

    fn circuit_cable_rows(
        &self,
        rules: &CableRules,
        c: &Circuit,
        laying: &str,
    ) -> Result<Vec<CableRow>, Error> {
        let mut out = vec![];
        for (comp, route) in self.circuit_device_lengths(c)? {
            let Some(route) = route else { continue };
            let Some(length) = rules.cable_length(laying, route) else { continue };
            out.push(CableRow {
                circuit: c.spec.number,
                name: c.spec.name.clone(),
                from: c.spec.panel,
                to: comp,
                section: c.spec.cable,
                poles: c.spec.poles,
                laying: laying.to_string(),
                route,
                length,
            });
        }
        Ok(out)
    }
}

/// The log as CSV that spreadsheets open directly: `;` separated, UTF-8 with a BOM, and a
/// total row. Headers are passed in so the caller localises them.
pub fn cable_log_csv(rows: &[CableRow], headers: &[&str]) -> String {
    let esc = |s: &str| ddc_geom::csv::field(s, ';').into_owned();
    let mut s = String::from("\u{feff}");
    s.push_str(&headers.iter().map(|h| esc(h)).collect::<Vec<_>>().join(";"));
    s.push('\n');
    for r in rows {
        s.push_str(&format!(
            "{};{};{};{};{};{};{};{:.2};{:.2}\n",
            r.circuit,
            esc(&r.name),
            r.from,
            r.to,
            r.section,
            r.poles,
            r.laying,
            r.route,
            r.length
        ));
    }
    let total: f64 = rows.iter().map(|r| r.length).sum();
    s.push_str(&format!(";;;;;;;;{total:.2}\n"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CableInput {
        CableInput {
            kind: CircuitKind::Power,
            standard: Standard::Pue,
            voltage: 230.0,
            poles: 1,
            load: 2300.0,
            cos_phi: 0.9,
            material: Conductor::Cu,
            length: 20.0,
            breaker: 16.0,
            section: 2.5,
            laying: "clipped".into(),
            ambient_c: 30.0,
            grouped: 1,
            max_drop: None,
            measured_drop: None,
        }
    }

    fn amp(s: Standard, m: Conductor, l: &str, sec: f64, poles: u32) -> Result<f64, String> {
        CableRules::shared().ampacity(s, m, l, sec, poles)
    }

    #[test]
    fn every_table_has_source_confidence_and_aligned_rows() {
        let r = CableRules::shared();
        assert!(r.ampacity.len() >= 8);
        for t in &r.ampacity {
            assert!(t.source.doc.len() > 10 && t.source.url.starts_with("https://"), "{}", t.id);
            assert!(["high", "medium", "low"].contains(&t.confidence.as_str()), "{}", t.id);
            assert!(t.rows.iter().all(|row| row.len() == t.columns.len() + 1), "{}", t.id);
            // every section of a row is a section the rules know
            assert!(t.rows.iter().all(|row| r.sections.contains(&row[0].unwrap())), "{}", t.id);
        }
        assert_eq!(r.laying.len(), 4);
    }

    #[test]
    fn pue_points_of_every_table() {
        use Conductor::*;
        use Standard::Pue;
        // 1.3.4 copper wires: conduit 1 pole = 2 single-core wires in a pipe, 3 poles = 3 wires
        assert_eq!(amp(Pue, Cu, "conduit_wall", 2.5, 1), Ok(27.0));
        assert_eq!(amp(Pue, Cu, "conduit_wall", 2.5, 3), Ok(25.0));
        assert_eq!(amp(Pue, Cu, "clipped", 2.5, 1), Ok(30.0));
        assert_eq!(amp(Pue, Cu, "clipped", 120.0, 3), Ok(385.0));
        // 1.3.5 aluminium wires (no 1.5 mm2 row)
        assert_eq!(amp(Pue, Al, "clipped", 16.0, 1), Ok(75.0));
        assert_eq!(amp(Pue, Al, "conduit_wall", 2.5, 3), Ok(19.0));
        assert!(amp(Pue, Al, "clipped", 1.5, 1).unwrap_err().contains("no row"));
        // 1.3.6 copper cables: air 2 and 3 cores, ground 2 and 3 cores
        assert_eq!(amp(Pue, Cu, "tray", 2.5, 1), Ok(27.0));
        assert_eq!(amp(Pue, Cu, "tray", 2.5, 3), Ok(25.0));
        assert_eq!(amp(Pue, Cu, "ground", 2.5, 1), Ok(44.0));
        assert_eq!(amp(Pue, Cu, "ground", 120.0, 3), Ok(385.0));
        // 1.3.7 aluminium cables
        assert_eq!(amp(Pue, Al, "tray", 10.0, 1), Ok(55.0));
        assert_eq!(amp(Pue, Al, "ground", 70.0, 3), Ok(210.0));
        assert!(amp(Pue, Cu, "nowhere", 2.5, 1).is_err());
        assert!(amp(Pue, Cu, "clipped", 3.0, 1).is_err());
    }

    #[test]
    fn iec_points_of_every_table() {
        use Conductor::*;
        use Standard::Iec;
        // copper, columns B1 and C: 2 and 3 loaded
        assert_eq!(amp(Iec, Cu, "conduit_wall", 2.5, 1), Ok(24.0));
        assert_eq!(amp(Iec, Cu, "conduit_wall", 2.5, 3), Ok(21.0));
        assert_eq!(amp(Iec, Cu, "clipped", 16.0, 3), Ok(76.0));
        assert_eq!(amp(Iec, Cu, "clipped", 1.5, 3), Ok(17.5));
        // B.52.4 aluminium 3 loaded: 16 mm2 B1 53, C 59; 2.5 mm2 B1 16.5
        assert_eq!(amp(Iec, Al, "conduit_wall", 16.0, 3), Ok(53.0));
        assert_eq!(amp(Iec, Al, "clipped", 16.0, 3), Ok(59.0));
        assert_eq!(amp(Iec, Al, "conduit_wall", 2.5, 3), Ok(16.5));
        // B.52.2 aluminium 2 loaded: 16 mm2 B1 60, C 66; the 2.5 to 10 mm2 rows are refused
        assert_eq!(amp(Iec, Al, "conduit_wall", 16.0, 1), Ok(60.0));
        assert_eq!(amp(Iec, Al, "clipped", 120.0, 1), Ok(226.0));
        let why = amp(Iec, Al, "clipped", 6.0, 1).unwrap_err();
        assert!(why.contains("iec_b52_2_al") && why.contains("not confirmed"), "{why}");
        // no confirmed column for a tray or the ground
        assert!(amp(Iec, Cu, "tray", 2.5, 1).unwrap_err().contains("tray"));
        assert!(amp(Iec, Cu, "ground", 2.5, 1).unwrap_err().contains("ground"));
    }

    #[test]
    fn reactance_by_section_and_flag() {
        use Conductor::*;
        let r = CableRules::shared();
        assert_eq!(r.x0(Cu, 6.0), (0.100, false));
        assert_eq!(r.x0(Cu, 120.0), (0.070, false));
        // 1.5, 2.5 and 4 mm2 copper are not in the standard: the 6 mm2 value, flagged
        assert_eq!(r.x0(Cu, 2.5), (0.100, true));
        assert_eq!(r.x0(Cu, 4.0), (0.100, true));
        assert_eq!(r.x0(Al, 4.0), (0.092, false));
        assert_eq!(r.x0(Al, 2.5), (0.092, true));
        assert_eq!(r.x0(Al, 95.0), (0.057, false));
    }

    #[test]
    fn load_types_and_drop_limits_come_from_sp_256() {
        let r = CableRules::shared();
        assert_eq!(r.cos_phi_of("office"), Some(0.85));
        assert_eq!(r.cos_phi_of("light_fluorescent"), Some(0.92));
        assert_eq!(r.cos_phi_of("res_stove_electric"), Some(0.98));
        assert_eq!(r.cos_phi_of("workshop"), Some(0.60));
        assert_eq!(r.cos_phi_of("nothing"), None);
        assert_eq!(r.cos_phi(CircuitKind::Power), 0.85);
        assert_eq!(r.cos_phi(CircuitKind::Lighting), 0.92);
        // 3 % to lighting, 4 % to the rest
        assert_eq!((r.max_drop(CircuitKind::Lighting), r.max_drop(CircuitKind::Power)), (3.0, 4.0));
        // over 100 m: +0.005 % per metre, at most +0.5 %
        assert_eq!(r.drop_limit(4.0, 80.0), 4.0);
        assert_eq!(r.drop_limit(4.0, 100.0), 4.0);
        assert!((r.drop_limit(4.0, 150.0) - 4.25).abs() < 1e-12);
        assert!((r.drop_limit(3.0, 400.0) - 3.5).abs() < 1e-12);
    }

    #[test]
    fn current_uses_the_power_factor() {
        // 1 pole: 2300 / (230 * 0.9) = 11.111 A
        let i = CableRules::current(1, 230.0, 0.9, 2300.0);
        assert!((i - 11.1111).abs() < 1e-3, "{i}");
        // 3 poles: 6000 / (1.73205 * 400 * 0.8) = 10.825 A
        let i = CableRules::current(3, 400.0, 0.8, 6000.0);
        assert!((i - 10.825).abs() < 1e-3, "{i}");
    }

    #[test]
    fn drop_has_the_reactive_part_and_hot_resistivity() {
        let r = CableRules::builtin();
        // 2.5 mm2 Cu: R = 0.0225/2.5 = 0.009 ohm/m, X = 0.0001 ohm/m (x0 estimated), sin = 0.43589
        // R cos + X sin = 0.0081 + 0.0000436 = 0.0081436; 2 * 16 A * 20 m * that = 5.2119 V
        let v = r.volt_drop(Conductor::Cu, 1, 2.5, 0.9, 16.0 * 20.0);
        assert!((v - 5.2119).abs() < 1e-3, "{v}");
        assert!((v / 230.0 * 100.0 - 2.2660).abs() < 1e-3);
        // Al, 16 mm2, cos 1: R = 0.036/16 = 0.00225, three poles: 1.73205 * 10 A * 50 m * 0.00225
        let v = r.volt_drop(Conductor::Al, 3, 16.0, 1.0, 10.0 * 50.0);
        assert!((v - 1.9486).abs() < 1e-3, "{v}");
        assert_eq!((r.rho(Conductor::Cu), r.rho(Conductor::Al)), (0.0225, 0.036));
    }

    #[test]
    fn derating_by_temperature_and_grouping() {
        let r = CableRules::builtin();
        // clipped 2.5 mm2 = 30 A; 40 C 0.87, four circuits 0.65: 30 * 0.87 * 0.65 = 16.965 A
        let i = CableInput { ambient_c: 40.0, grouped: 4, ..input() };
        assert!((r.derated(&i).unwrap() - 16.965).abs() < 1e-9);
        assert!(r.check(&i).iter().all(|x| x.ok));
        // six circuits 0.57: 30 * 0.87 * 0.57 = 14.877 A, the 16 A breaker no longer fits
        let j = CableInput { grouped: 6, ..i };
        let bad: Vec<_> = r.check(&j).iter().filter(|x| !x.ok).map(|x| x.rule).collect();
        assert_eq!(bad, ["EL-BRK-02"]);
        assert_eq!(r.group_factor(10), 0.45);
        assert!(r.temp_factor(70.0).is_none());
        assert_eq!(r.temp_factor(60.0), Some(0.5));
    }

    #[test]
    fn a_missing_table_value_fails_the_rule_with_the_reason() {
        let r = CableRules::builtin();
        let i = CableInput { standard: Standard::Iec, laying: "tray".into(), ..input() };
        let f = r.check(&i);
        let b = f.iter().find(|x| x.rule == "EL-BRK-02").unwrap();
        assert!(!b.ok && b.note.as_deref().unwrap().contains("tray"));
        // aluminium 2.5 mm2 on the 2-loaded IEC table is refused, on PUE it is fine
        let al = CableInput { material: Conductor::Al, standard: Standard::Iec, ..input() };
        assert!(r.derated(&al).is_err());
        assert_eq!(r.derated(&CableInput { standard: Standard::Pue, ..al }), Ok(24.0));
    }

    #[test]
    fn passing_circuit_cites_every_rule() {
        let r = CableRules::builtin();
        let f = r.check(&input());
        let ids: Vec<_> = f.iter().map(|x| x.rule).collect();
        assert_eq!(ids, ["EL-BRK-01", "EL-BRK-02", "EL-BRK-03", "EL-CAB-01", "EL-DRP-01"]);
        assert!(f.iter().all(|x| x.ok), "{f:?}");
    }

    #[test]
    fn each_rule_fails_on_its_own() {
        let r = CableRules::builtin();
        let bad = |i: CableInput, id: &str| {
            let f = r.check(&i);
            let failed: Vec<_> = f.iter().filter(|x| !x.ok).map(|x| x.rule).collect();
            assert_eq!(failed, [id], "{id}");
        };
        // 4000 W: 4000 / (230 * 0.9) = 19.3 A above 16 A
        bad(CableInput { load: 4000.0, ..input() }, "EL-BRK-01");
        // 32 A breaker on 2.5 mm2 in a pipe: PUE 27 A
        bad(CableInput { breaker: 32.0, laying: "conduit_wall".into(), ..input() }, "EL-BRK-02");
        bad(CableInput { breaker: 15.0, ..input() }, "EL-BRK-03");
        bad(CableInput { section: 1.5, breaker: 10.0, load: 1000.0, ..input() }, "EL-CAB-01");
        // 100 m: 2 * 11.111 A * 100 m * 0.0081436 = 18.10 V = 7.87 % above 4 %
        bad(CableInput { length: 100.0, ..input() }, "EL-DRP-01");
    }

    #[test]
    fn measured_drop_replaces_the_estimate() {
        let r = CableRules::builtin();
        let f = r.check(&CableInput { measured_drop: Some(9.0), ..input() });
        let d = f.iter().find(|x| x.rule == "EL-DRP-01").unwrap();
        assert!(!d.ok && (d.value - 9.0).abs() < 1e-12 && (d.limit - 4.0).abs() < 1e-12);
        // a 150 m run may drop 4.25 %
        let f = r.check(&CableInput { measured_drop: Some(4.2), length: 150.0, ..input() });
        assert!(f.iter().find(|x| x.rule == "EL-DRP-01").unwrap().ok);
    }

    #[test]
    fn sizing_is_minimal_and_deterministic() {
        let r = CableRules::builtin();
        let a = r.size(&input()).unwrap();
        // 11.11 A takes the 13 A breaker; 2.5 mm2 drops 1.57 %
        assert_eq!((a.breaker, a.section), (13.0, 2.5));
        assert_eq!(r.size(&input()), Some(a));
        let long = CableInput { length: 100.0, ..input() };
        let b = r.size(&long).unwrap();
        // limit 4 % = 9.2 V; 4 mm2: 2 * 11.111 * 100 * (0.005625 * 0.9 + 0.0000436) = 11.35 V;
        // 6 mm2: 2 * 11.111 * 100 * (0.00375 * 0.9 + 0.0000436) = 7.597 V = 3.30 %
        assert_eq!(b.section, 6.0);
        assert!((b.drop_pct - 3.303).abs() < 0.01, "{}", b.drop_pct);
        // the sized circuit passes its own check
        let ok = CableInput { breaker: b.breaker, section: b.section, ..long };
        assert!(r.check(&ok).iter().all(|x| x.ok));
        let huge = CableInput { length: 5000.0, ..input() };
        assert_eq!(r.size(&huge), None);
    }

    #[test]
    fn length_by_laying_method() {
        let r = CableRules::builtin();
        let l = r.cable_length("conduit_wall", 10.0).unwrap();
        assert!((l - (10.8 + 1.0)).abs() < 1e-9);
        assert!(r.cable_length("x", 1.0).is_none());
    }

    #[test]
    fn csv_has_bom_total_and_escapes() {
        let rows = vec![CableRow {
            circuit: 1,
            name: "a;b".into(),
            from: 1,
            to: 2,
            section: 2.5,
            poles: 1,
            laying: "tray".into(),
            route: 10.0,
            length: 11.5,
        }];
        let s = cable_log_csv(&rows, &["n", "name"]);
        assert!(s.starts_with('\u{feff}'));
        assert!(s.contains("\"a;b\""));
        assert!(s.trim_end().ends_with(";11.50"));
    }
}
