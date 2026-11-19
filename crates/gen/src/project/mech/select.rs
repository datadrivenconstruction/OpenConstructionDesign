// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The system the rules pick for a project. Air side: all-air variable volume (VAV) where the
//! ceiling void takes the ducts and the cooling density stays within what air can carry; fan
//! coils with dedicated outdoor air where the loads are denser; chilled ceilings with
//! dedicated outdoor air where the void is shallow and the load fits the panels; radiators
//! with ventilation where no room needs cooling. Plant: reversible heat pumps up to their
//! capacity and down to their coldest outdoor design temperature, else chillers and boilers.

use super::super::rules::rule;
use super::loads::RoomLoad;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AirSide {
    Vav,
    FcuDoas,
    RadiantDoas,
    Radiators,
}

impl AirSide {
    /// Option index of `mep.hvac.system` past `off` and `auto`.
    pub const ALL: [AirSide; 4] = [AirSide::Vav, AirSide::FcuDoas, AirSide::RadiantDoas, AirSide::Radiators];

    pub fn name(self) -> &'static str {
        match self {
            AirSide::Vav => "vav",
            AirSide::FcuDoas => "fcu_doas",
            AirSide::RadiantDoas => "radiant_doas",
            AirSide::Radiators => "radiators",
        }
    }

    pub fn cooling(self) -> bool {
        self != AirSide::Radiators
    }

    /// The ducts carry the room loads, not only the outdoor air.
    pub fn all_air(self) -> bool {
        self == AirSide::Vav
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlantKind {
    ChillerBoiler,
    HeatPump,
}

impl PlantKind {
    pub fn name(self) -> &'static str {
        match self {
            PlantKind::ChillerBoiler => "chiller_boiler",
            PlantKind::HeatPump => "heat_pump",
        }
    }
}

/// What was chosen and the numbers it was chosen on.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub air: AirSide,
    pub plant: PlantKind,
    /// Chosen by the rules (`auto`) rather than named.
    pub auto_air: bool,
    pub auto_plant: bool,
    /// Highest sensible cooling of a room per floor area, W/m2.
    pub peak: f64,
    /// Shallowest ceiling void over the least clear height, m.
    pub void: f64,
    /// Block heating, kW.
    pub heat_kw: f64,
    /// Why, as a reason key of `why_text`.
    pub key: &'static str,
}

/// Air side by the rules: `forced` is the option index of `mep.hvac.system` (1 = auto).
pub fn pick_air(res: bool, peak: f64, void: f64, forced: usize) -> (AirSide, &'static str) {
    if forced >= 2 {
        return (AirSide::ALL[(forced - 2).min(AirSide::ALL.len() - 1)], "hvac_sys_named");
    }
    if res && rule("mep.hvac.res_cooling") == 0.0 {
        return (AirSide::Radiators, "hvac_sys_res");
    }
    if peak <= rule("mep.hvac.cool_min") {
        return (AirSide::Radiators, "hvac_sys_nocool");
    }
    if void < rule("mep.hvac.vav_void") {
        if peak <= rule("mep.hvac.radiant_max") * rule("mep.hvac.radiant_cover") {
            return (AirSide::RadiantDoas, "hvac_sys_radiant");
        }
        return (AirSide::FcuDoas, "hvac_sys_fcu_void");
    }
    if peak > rule("mep.hvac.vav_max_density") {
        return (AirSide::FcuDoas, "hvac_sys_fcu_dense");
    }
    (AirSide::Vav, "hvac_sys_vav")
}

/// Plant by the rules: `forced` is the option index of `mep.hvac.plant` (0 = auto).
pub fn pick_plant(heat_kw: f64, forced: usize) -> PlantKind {
    match forced {
        1 => PlantKind::ChillerBoiler,
        2 => PlantKind::HeatPump,
        _ if heat_kw <= rule("mep.hvac.hp_max_kw") && rule("mep.hvac.t_out_winter") >= rule("mep.hvac.hp_min_temp") => {
            PlantKind::HeatPump
        }
        _ => PlantKind::ChillerBoiler,
    }
}

/// The choice for a project's loads; `void` from the storeys.
pub fn choose(res: bool, loads: &[RoomLoad], void: f64) -> Choice {
    let peak = loads
        .iter()
        .filter(|l| l.kind.habitable() && l.area > 0.0)
        .map(|l| l.sensible() / l.area)
        .fold(0.0, f64::max);
    let heat_kw = loads.iter().map(|l| l.heating()).sum::<f64>() / 1000.0;
    let forced_air = rule("mep.hvac.system") as usize;
    let forced_plant = rule("mep.hvac.plant") as usize;
    let (air, key) = pick_air(res, peak, void, forced_air);
    Choice {
        air,
        plant: pick_plant(heat_kw, forced_plant),
        auto_air: forced_air < 2,
        auto_plant: forced_plant == 0,
        peak,
        void,
        heat_kw,
        key,
    }
}
