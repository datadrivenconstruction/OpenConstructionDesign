//! Sizing data: water properties, pipe sizes by velocity, flanges, motors and inline items.
//! Physical constants and catalogue rows, not decisions: the decisions read `plant.*` rules.

use ddc_geom::mep::{PipeMaterial, PipeSize};
use std::f64::consts::PI;

use crate::project::rules::rule;

/// Gravity, m/s².
pub const G: f64 = 9.81;
/// Specific heat of water, kJ/(kg·K).
pub const CP: f64 = 4.19;
/// Pascal per bar.
const PA_PER_BAR: f64 = 1e5;

/// Density of water at `t` °C, kg/m³ (Thiesen's fit, 0..150 °C).
pub fn rho(t: f64) -> f64 {
    1000.0 * (1.0 - (t + 288.9414) / (508929.2 * (t + 68.12963)) * (t - 3.9863).powi(2))
}

/// Water flow, m³/h, that carries `kw` at a temperature difference of `dt` K.
pub fn heat_flow(kw: f64, dt: f64, t: f64) -> f64 {
    kw / (rho(t) * CP * dt) * 3600.0
}

/// Heat, kW, of `flow` m³/h of water warmed by `dt` K.
pub fn heat_of(flow: f64, dt: f64, t: f64) -> f64 {
    flow / 3600.0 * rho(t) * CP * dt
}

/// Pressure, bar, of a water column `h` m high.
pub fn bar_of_head(h: f64, t: f64) -> f64 {
    rho(t) * G * h / PA_PER_BAR
}

/// Diameter, m, of a circle of area `a` m².
pub fn d_of_area(a: f64) -> f64 {
    (4.0 * a / PI).sqrt()
}

/// Diameter, m, of an upright cylinder of volume `v` m³ whose height is `hd` diameters.
pub fn d_of_volume(v: f64, hd: f64) -> f64 {
    (4.0 * v / (PI * hd)).cbrt()
}

/// Gap, m, between a bund wall and the tanks' bounding box `w` x `l` so that walls `h` high hold
/// `need` m3 with `inside` m2 of tank footprints standing in it; never less than `least`.
pub fn bund_margin(w: f64, l: f64, inside: f64, h: f64, need: f64, least: f64) -> f64 {
    // (w + 2m)(l + 2m) - inside = need / h, a quadratic in m
    let a = need / h + inside - w * l;
    let b = w + l;
    let m = (-b + (b * b + 4.0 * a).max(0.0).sqrt()) / 4.0;
    m.max(least)
}

/// `x` rounded up to a whole number of `step`.
pub fn round_up(x: f64, step: f64) -> f64 {
    (x / step - 1e-9).ceil() * step
}

/// One decimal place, for reports.
pub fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Two decimal places, for reports.
pub fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// A pipe specification: material, rating and joints of a class of lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Spec {
    /// Carbon steel, welded, flanged at valves: water.
    C1,
    /// Carbon steel, welded: fuel gas.
    G1,
    /// Stainless steel, welded: process product and hot chemicals.
    S1,
    /// Polypropylene, fused: cold chemicals.
    P1,
}

impl Spec {
    pub const ALL: [Spec; 4] = [Spec::C1, Spec::G1, Spec::S1, Spec::P1];
    pub fn code(self) -> &'static str {
        match self {
            Spec::C1 => "C1",
            Spec::G1 => "G1",
            Spec::S1 => "S1",
            Spec::P1 => "P1",
        }
    }
    pub fn material(self) -> PipeMaterial {
        match self {
            Spec::P1 => PipeMaterial::Pp,
            _ => PipeMaterial::Steel,
        }
    }
    pub fn material_name(self) -> &'static str {
        match self {
            Spec::C1 | Spec::G1 => "carbon steel",
            Spec::S1 => "stainless steel",
            Spec::P1 => "polypropylene",
        }
    }
    /// Material name in a family's `mat()`.
    pub fn mat(self) -> &'static str {
        match self {
            Spec::C1 | Spec::G1 => "Steel",
            Spec::S1 => "Stainless Steel",
            Spec::P1 => "Polypropylene",
        }
    }
    /// Nominal pressure of the flanges for a design pressure in bar.
    pub fn pn(self, p: f64) -> u32 {
        match self {
            Spec::P1 => PN_PLASTIC,
            _ if p <= 16.0 => 16,
            _ => 25,
        }
    }
}

/// Rating of plastic lines, bar.
pub const PN_PLASTIC: u32 = 10;

/// The size of `spec` that is `dn`, else the next larger, else the largest.
pub fn pipe(spec: Spec, dn: u32) -> PipeSize {
    let s = spec.material().sizes();
    let last = *s.last().expect("a pipe table has rows");
    s.into_iter().find(|p| p.dn >= dn).unwrap_or(last)
}

/// Smallest size of `spec` whose bore keeps `flow` m³/h at `v` m/s or less, at least the
/// smallest size the rules allow.
pub fn dn_for(spec: Spec, flow: f64, v: f64) -> u32 {
    let need = d_of_area(flow.max(0.0) / 3600.0 / v);
    let min = rule("plant.pipe.min_dn");
    let s = spec.material().sizes();
    let last = s.last().expect("a pipe table has rows").dn;
    s.iter().find(|p| p.dn as f64 >= min - 1e-9 && p.id() >= need).map_or(last, |p| p.dn)
}

/// Velocity, m/s, of `flow` m³/h in the bore of `dn`.
pub fn velocity(spec: Spec, dn: u32, flow: f64) -> f64 {
    let id = pipe(spec, dn).id();
    flow / 3600.0 / (PI * id * id / 4.0)
}

/// Bend radius at the axis, m.
pub fn bend(spec: Spec, dn: u32) -> f64 {
    let p = pipe(spec, dn);
    spec.material().bend_radius(&p)
}

/// Flange outside diameters, mm, of the PN 16 and PN 25 series.
const FLANGE_PN16: [(u32, f64); 14] = [
    (15, 95.0),
    (20, 105.0),
    (25, 115.0),
    (32, 140.0),
    (40, 150.0),
    (50, 165.0),
    (65, 185.0),
    (80, 200.0),
    (100, 220.0),
    (125, 250.0),
    (150, 285.0),
    (200, 340.0),
    (250, 405.0),
    (300, 460.0),
];
const FLANGE_PN25: [(u32, f64); 14] = [
    (15, 95.0),
    (20, 105.0),
    (25, 115.0),
    (32, 140.0),
    (40, 150.0),
    (50, 165.0),
    (65, 185.0),
    (80, 200.0),
    (100, 235.0),
    (125, 270.0),
    (150, 300.0),
    (200, 360.0),
    (250, 425.0),
    (300, 485.0),
];

/// Flange outside diameter, m.
pub fn flange_od(dn: u32, pn: u32) -> f64 {
    let t = if pn > 16 { &FLANGE_PN25 } else { &FLANGE_PN16 };
    t.iter().find(|r| r.0 >= dn).unwrap_or(&t[t.len() - 1]).1 / 1000.0
}

/// Flange thickness, m: a fit of the PN 16 series.
pub fn flange_t(dn: u32) -> f64 {
    0.014 + 0.00006 * dn as f64
}

/// Standard motor ratings, kW.
const MOTOR_KW: [f64; 25] = [
    0.25, 0.37, 0.55, 0.75, 1.1, 1.5, 2.2, 3.0, 4.0, 5.5, 7.5, 11.0, 15.0, 18.5, 22.0, 30.0, 37.0,
    45.0, 55.0, 75.0, 90.0, 110.0, 132.0, 160.0, 200.0,
];

/// The smallest standard motor of at least `kw`.
pub fn motor_kw(kw: f64) -> f64 {
    MOTOR_KW.iter().copied().find(|&m| m >= kw - 1e-9).unwrap_or(MOTOR_KW[MOTOR_KW.len() - 1])
}

/// Length of a concentric or eccentric reducer between outside diameters `a` and `b`, m.
pub fn reducer_len(a: f64, b: f64) -> f64 {
    a.max(b) + (a - b).abs()
}

/// An item in a line: a valve, a strainer, a meter, a tapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    Ball,
    Gate,
    Butterfly,
    Check,
    Strainer,
    FlowMeter,
    GasFilter,
    Regulator,
    ShutOff,
    Psv,
    Pi,
    Ti,
}

impl ItemKind {
    pub const ALL: [ItemKind; 12] = [
        ItemKind::Ball,
        ItemKind::Gate,
        ItemKind::Butterfly,
        ItemKind::Check,
        ItemKind::Strainer,
        ItemKind::FlowMeter,
        ItemKind::GasFilter,
        ItemKind::Regulator,
        ItemKind::ShutOff,
        ItemKind::Psv,
        ItemKind::Pi,
        ItemKind::Ti,
    ];
    /// Tag prefix.
    pub fn code(self) -> &'static str {
        match self {
            ItemKind::Ball => "BV",
            ItemKind::Gate => "GV",
            ItemKind::Butterfly => "BFV",
            ItemKind::Check => "NRV",
            ItemKind::Strainer => "STR",
            ItemKind::FlowMeter => "FQ",
            ItemKind::GasFilter => "GF",
            ItemKind::Regulator => "PCV",
            ItemKind::ShutOff => "XV",
            ItemKind::Psv => "PSV",
            ItemKind::Pi => "PI",
            ItemKind::Ti => "TI",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            ItemKind::Ball => "ball valve",
            ItemKind::Gate => "gate valve",
            ItemKind::Butterfly => "butterfly valve",
            ItemKind::Check => "check valve",
            ItemKind::Strainer => "strainer",
            ItemKind::FlowMeter => "flow meter",
            ItemKind::GasFilter => "gas filter",
            ItemKind::Regulator => "pressure regulator",
            ItemKind::ShutOff => "safety shut-off valve",
            ItemKind::Psv => "safety relief valve",
            ItemKind::Pi => "pressure gauge",
            ItemKind::Ti => "thermometer",
        }
    }
    /// IFC class and predefined type.
    pub fn ifc(self) -> &'static str {
        match self {
            ItemKind::Ball | ItemKind::Gate | ItemKind::Butterfly => "IfcValve ISOLATING",
            ItemKind::Check => "IfcValve CHECK",
            ItemKind::Strainer => "IfcFilter STRAINER",
            ItemKind::FlowMeter => "IfcFlowMeter WATERMETER",
            ItemKind::GasFilter => "IfcFilter COMPRESSEDAIRFILTER",
            ItemKind::Regulator => "IfcValve PRESSUREREDUCING",
            ItemKind::ShutOff => "IfcValve GASTAP",
            ItemKind::Psv => "IfcValve SAFETYCUTOFF",
            ItemKind::Pi => "IfcFlowInstrument PRESSUREGAUGE",
            ItemKind::Ti => "IfcFlowInstrument THERMOMETER",
        }
    }
    pub fn is_valve(self) -> bool {
        matches!(
            self,
            ItemKind::Ball
                | ItemKind::Gate
                | ItemKind::Butterfly
                | ItemKind::Check
                | ItemKind::Regulator
                | ItemKind::ShutOff
                | ItemKind::Psv
        )
    }
    pub fn is_instrument(self) -> bool {
        matches!(self, ItemKind::FlowMeter | ItemKind::Pi | ItemKind::Ti)
    }
    /// Turned by hand in service: must be within reach.
    pub fn operated(self) -> bool {
        matches!(self, ItemKind::Ball | ItemKind::Gate | ItemKind::Butterfly)
    }
    /// Length along the pipe, m, from the flange diameter `fl` (a tapping takes none; a relief
    /// valve sits on a tee of the flange's length).
    pub fn face(self, fl: f64) -> f64 {
        fl * match self {
            ItemKind::Ball => 1.0,
            ItemKind::Gate => 1.1,
            ItemKind::Butterfly => 0.25,
            ItemKind::Check => 1.2,
            ItemKind::Strainer => 1.3,
            ItemKind::FlowMeter => 1.2,
            ItemKind::GasFilter => 1.3,
            ItemKind::Regulator => 1.5,
            ItemKind::ShutOff => 1.2,
            ItemKind::Psv => 1.0,
            ItemKind::Pi | ItemKind::Ti => 0.0,
        }
    }
    /// Flanged joints the item adds.
    pub fn flanges(self) -> usize {
        match self {
            ItemKind::Pi | ItemKind::Ti => 0,
            ItemKind::Psv => 1,
            _ => 2,
        }
    }
    /// Reach of the handwheel or lever from the pipe axis, m.
    pub fn stem(self, fl: f64) -> f64 {
        fl * match self {
            ItemKind::Gate => 2.0,
            ItemKind::Butterfly => 1.2,
            ItemKind::Ball => 0.8,
            _ => 0.6,
        }
    }
    /// The isolating valve of a line of `dn` by the rules.
    pub fn isolation(dn: u32) -> ItemKind {
        let dn = dn as f64;
        if dn <= rule("plant.valve.ball_to") {
            ItemKind::Ball
        } else if dn >= rule("plant.valve.butterfly_from") {
            ItemKind::Butterfly
        } else {
            ItemKind::Gate
        }
    }
}

/// One piece of a nozzle's stack: a reducer (`item` None) or an item, `at` from the face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StackPiece {
    pub item: Option<ItemKind>,
    pub dn: u32,
    pub at: f64,
    pub len: f64,
}

/// What sits on a nozzle from its face outward: a reducer to the pipe size, then the items at
/// the pipe size.
pub fn stack(dn_nozzle: u32, dn_pipe: u32, items: &[ItemKind], spec: Spec, pn: u32) -> Vec<StackPiece> {
    let mut out = vec![];
    let mut s = 0.0;
    if dn_nozzle != dn_pipe {
        let l = reducer_len(pipe(spec, dn_nozzle).od, pipe(spec, dn_pipe).od);
        out.push(StackPiece { item: None, dn: dn_pipe, at: s, len: l });
        s += l;
    }
    let fl = flange_od(dn_pipe, pn);
    for &k in items {
        let l = k.face(fl);
        out.push(StackPiece { item: Some(k), dn: dn_pipe, at: s, len: l });
        s += l;
    }
    out
}

/// Total length of a stack, m.
pub fn stack_len(dn_nozzle: u32, dn_pipe: u32, items: &[ItemKind], spec: Spec, pn: u32) -> f64 {
    stack(dn_nozzle, dn_pipe, items, spec, pn).iter().map(|p| p.len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_and_pipes() {
        assert!((rho(4.0) - 1000.0).abs() < 0.1 && (rho(90.0) - 965.3).abs() < 0.5);
        assert!((heat_flow(1000.0, 20.0, 70.0) - 43.8).abs() < 0.3);
        // 43 m³/h at 1.5 m/s needs a 101 mm bore: DN100
        assert_eq!(dn_for(Spec::C1, 43.0, 1.5), 100);
        assert!(velocity(Spec::C1, 100, 43.0) <= 1.5);
        assert_eq!(motor_kw(5.6), 7.5);
        assert_eq!(flange_od(100, 16), 0.22);
        assert_eq!(flange_od(100, 25), 0.235);
        assert_eq!(ItemKind::isolation(25), ItemKind::Ball);
        assert_eq!(ItemKind::isolation(100), ItemKind::Gate);
        assert_eq!(ItemKind::isolation(200), ItemKind::Butterfly);
        let s = stack(65, 100, &[ItemKind::Strainer, ItemKind::Gate], Spec::C1, 16);
        assert_eq!(s.len(), 3);
        assert!(s[0].item.is_none() && s[1].at > 0.0 && s[2].at > s[1].at);
    }
}
