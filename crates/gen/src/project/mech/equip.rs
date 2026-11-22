// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Equipment as parametric objects: casing, the sections inside it in air order, and ports
//! with the medium, position, outward direction, size and flow each connection takes.

use super::super::rules::rule;
use super::geo::{Bx, P3};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Medium {
    SupplyAir,
    ExtractAir,
    OutdoorAir,
    ExhaustAir,
    ChwFlow,
    ChwReturn,
    HwFlow,
    HwReturn,
}

impl Medium {
    pub fn name(self) -> &'static str {
        match self {
            Medium::SupplyAir => "supply_air",
            Medium::ExtractAir => "extract_air",
            Medium::OutdoorAir => "outdoor_air",
            Medium::ExhaustAir => "exhaust_air",
            Medium::ChwFlow => "chw_flow",
            Medium::ChwReturn => "chw_return",
            Medium::HwFlow => "hw_flow",
            Medium::HwReturn => "hw_return",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Ahu,
    VavBox,
    FanCoil,
    RadiantPanel,
    Radiator,
    Chiller,
    Boiler,
    HeatPump,
    Pump,
    FireDamper,
    VolumeDamper,
    Silencer,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ahu => "air_handling_unit",
            Kind::VavBox => "vav_box",
            Kind::FanCoil => "fan_coil",
            Kind::RadiantPanel => "radiant_panel",
            Kind::Radiator => "radiator",
            Kind::Chiller => "chiller",
            Kind::Boiler => "boiler",
            Kind::HeatPump => "heat_pump",
            Kind::Pump => "pump",
            Kind::FireDamper => "fire_damper",
            Kind::VolumeDamper => "volume_damper",
            Kind::Silencer => "duct_silencer",
        }
    }

    /// Tag prefix on drawings and schedules.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Ahu => "AHU",
            Kind::VavBox => "VAV",
            Kind::FanCoil => "FCU",
            Kind::RadiantPanel => "CP",
            Kind::Radiator => "RAD",
            Kind::Chiller => "CH",
            Kind::Boiler => "B",
            Kind::HeatPump => "HP",
            Kind::Pump => "P",
            Kind::FireDamper => "FD",
            Kind::VolumeDamper => "VD",
            Kind::Silencer => "SD",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub name: &'static str,
    pub medium: Medium,
    pub at: P3,
    /// Outward, unit length.
    pub dir: P3,
    /// Duct or pipe size at the connection, m.
    pub size: f64,
    /// m3/h.
    pub flow: f64,
}

/// A section of a unit in air order: length along the unit, pressure drop at design flow.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: &'static str,
    pub len: f64,
    pub dp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Equip {
    pub tag: String,
    pub kind: Kind,
    /// `storeys.len()` for the roof.
    pub storey: usize,
    pub room: Option<usize>,
    /// Centre of the casing's base.
    pub at: P3,
    /// Plan angle of the length axis, radians.
    pub rot: f64,
    /// Length, width, height.
    pub size: (f64, f64, f64),
    pub parts: Vec<Part>,
    pub ports: Vec<Port>,
    /// Ratings: (what, value) in the units the name says.
    pub duty: Vec<(&'static str, f64)>,
    /// The ventilation system the unit belongs to (0 the general one).
    pub net: u8,
}

impl Equip {
    pub fn new(kind: Kind, storey: usize, at: P3, rot: f64, size: (f64, f64, f64)) -> Equip {
        Equip { tag: String::new(), kind, storey, room: None, at, rot, size, parts: vec![], ports: vec![], duty: vec![], net: 0 }
    }

    /// A point of the casing frame (u along the length, v across, w up from the base).
    pub fn local(&self, u: f64, v: f64, w: f64) -> P3 {
        let (c, s) = (self.rot.cos(), self.rot.sin());
        (self.at.0 + c * u - s * v, self.at.1 + s * u + c * v, self.at.2 + w)
    }

    pub fn dir(&self, u: f64, v: f64, w: f64) -> P3 {
        let (c, s) = (self.rot.cos(), self.rot.sin());
        (c * u - s * v, s * u + c * v, w)
    }

    pub fn bbox(&self) -> Bx {
        let (l, w, h) = (self.size.0 / 2.0, self.size.1 / 2.0, self.size.2);
        let cs = [(-l, -w), (l, -w), (l, w), (-l, w)].map(|(u, v)| self.local(u, v, 0.0));
        let xs = cs.map(|c| c.0);
        let ys = cs.map(|c| c.1);
        let min = |a: [f64; 4]| a.iter().copied().fold(f64::MAX, f64::min);
        let max = |a: [f64; 4]| a.iter().copied().fold(f64::MIN, f64::max);
        [min(xs), min(ys), self.at.2, max(xs), max(ys), self.at.2 + h]
    }

    pub fn port(&self, name: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.name == name)
    }

    pub fn duty(&self, what: &str) -> f64 {
        self.duty.iter().find(|d| d.0 == what).map_or(0.0, |d| d.1)
    }
}

/// A catalogue size of a terminal unit: what it serves and its casing.
#[derive(Clone, Copy, Debug)]
pub struct Size {
    pub ty: &'static str,
    pub cap: f64,
    pub l: f64,
    pub w: f64,
    pub h: f64,
    /// Inlet or port diameter, m.
    pub d: f64,
}

/// VAV boxes by the airflow they control, m3/h.
pub fn vav_sizes() -> [Size; 3] {
    let s = |ty, k: &str| Size {
        ty,
        cap: rule(&format!("mep.hvac.vav_{k}_cap")),
        l: rule(&format!("mep.hvac.vav_{k}_l")),
        w: rule(&format!("mep.hvac.vav_{k}_w")),
        h: rule(&format!("mep.hvac.vav_{k}_h")),
        d: rule(&format!("mep.hvac.vav_{k}_d")),
    };
    [s("160", "s"), s("250", "m"), s("315", "l")]
}

/// Ceiling cassette fan coils by their cooling capacity, kW; square casing.
pub fn fcu_sizes() -> [Size; 3] {
    let s = |ty, k: &str| Size {
        ty,
        cap: rule(&format!("mep.hvac.fcu_{k}_kw")),
        l: rule(&format!("mep.hvac.fcu_{k}_side")),
        w: rule(&format!("mep.hvac.fcu_{k}_side")),
        h: rule(&format!("mep.hvac.fcu_{k}_h")),
        d: rule("mep.hvac.fcu_port"),
    };
    [s("FC-02", "s"), s("FC-04", "m"), s("FC-06", "l")]
}

/// The smallest size serving `q`, else the largest.
pub fn pick(sizes: &[Size], q: f64) -> Size {
    sizes.iter().copied().find(|s| s.cap + 1e-9 >= q).unwrap_or(sizes[sizes.len() - 1])
}

/// Sections of the air handling unit in air order, supply deck then extract deck, with the
/// drop of each at design flow; the coils only where the system has them.
pub fn ahu_parts(cooling: bool, heating: bool, recovery: bool) -> (Vec<Part>, Vec<Part>) {
    let part = |name: &'static str, k: &str| Part {
        name,
        len: rule(&format!("mep.hvac.len_{k}")),
        dp: rule(&format!("mep.hvac.dp_{k}")),
    };
    let mut sup = vec![part("intake damper", "intake"), part("supply filter", "filter")];
    let mut ext = vec![part("extract filter", "extract_filter")];
    if recovery {
        sup.push(part("heat recovery", "hr"));
        ext.push(part("heat recovery", "hr"));
    }
    if heating {
        sup.push(part("heating coil", "heat_coil"));
    }
    if cooling {
        sup.push(part("cooling coil", "cool_coil"));
    }
    sup.push(Part { name: "supply fan", len: rule("mep.hvac.len_fan"), dp: 0.0 });
    sup.push(part("supply silencer", "silencer"));
    ext.push(Part { name: "extract fan", len: rule("mep.hvac.len_fan"), dp: 0.0 });
    ext.push(part("exhaust damper", "intake"));
    (sup, ext)
}

/// Footprint of a plant unit of `kw` by its area per kW and the plan aspect: length, width.
pub fn plant_plan(kw: f64, m2_kw: f64) -> (f64, f64) {
    let a = (kw * m2_kw).max(rule("mep.hvac.plant_min_m2"));
    let aspect = rule("mep.hvac.plant_aspect");
    let w = (a / aspect).sqrt();
    (w * aspect, w)
}
