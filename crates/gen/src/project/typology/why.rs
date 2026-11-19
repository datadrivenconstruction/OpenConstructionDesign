//! Why the typology made the building as it did: the air its rooms get, the loads they put on
//! the services, the zones that sit together, the levels below ground and their walls, and what
//! it hands to the plant module. Each reason names the rule behind it, as every other reason of
//! the project does.

use super::super::why::{s, source, Why};
use super::super::why_text::Args;
use super::super::{GProject, ProjectSpec};
use super::{Pressure, Schedule};
use serde_json::{json, Value as J};

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The reasons of the typology of `spec`, most telling first; empty for a typology without a model.
pub fn explain(spec: &ProjectSpec, p: &GProject) -> Vec<Why> {
    let Some(m) = spec.model.as_ref() else {
        return vec![];
    };
    let sch: Schedule = super::schedule_of(spec, p).unwrap_or_default();
    let t = sch.totals();
    let ru = spec.ru;
    let name = spec.template.clone().unwrap_or_else(|| spec.name.clone());
    let mut out: Vec<Why> = vec![];
    let mut add = |target: &str, rule: &str, value: f64, key: &'static str, args: Args| {
        let v = super::super::rules::def(rule).map_or(json!(value), |d| d.value_json(value));
        out.push(Why::new(target.to_string(), rule, v, source(spec, rule), key, args));
    };
    add("typology", "typology.air.scale", spec.rules.get("typology.air.scale"), "typ_spaces", vec![
        ("name", name),
        ("classes", s(m.spaces.len())),
        ("rooms", s(t.rooms)),
        ("area", s(round(t.area))),
    ]);
    let aired = sch.rows.iter().filter(|r| r.supply_m3h + r.extract_m3h > 0.0).count();
    if aired > 0 {
        add("typology", "typology.air.scale", spec.rules.get("typology.air.scale"), "typ_air", vec![
            ("classes", s(aired)),
            ("supply", s(t.supply_m3h.round())),
            ("extract", s(t.extract_m3h.round())),
            ("k", s(spec.rules.get("typology.air.scale"))),
        ]);
    }
    let held = m.spaces.iter().filter(|x| matches!(x.demand.pressure, Some(Pressure::Positive | Pressure::Negative))).count();
    if held > 0 {
        let off = spec.rules.get("typology.air.pressure_offset");
        add("typology", "typology.air.pressure_offset", off, "typ_pressure", vec![("classes", s(held)), ("off", s(off))]);
    }
    if t.cooling_kw > 0.0 {
        let top = sch.rows.iter().max_by(|a, b| a.cooling_kw.total_cmp(&b.cooling_kw));
        add("typology", "typology.cooling.scale", spec.rules.get("typology.cooling.scale"), "typ_cooling", vec![
            ("kw", s(t.cooling_kw.round())),
            ("top", top.map_or(String::new(), |r| r.name.clone())),
            ("top_kw", s(top.map_or(0.0, |r| r.cooling_kw.round()))),
        ]);
    }
    if t.power_kw > 0.0 {
        add("typology", "typology.power.scale", spec.rules.get("typology.power.scale"), "typ_power", vec![
            ("kw", s(t.power_kw.round())),
            ("backup", s(t.backup_kw.round())),
        ]);
    }
    if t.water_m3d > 0.0 {
        add("typology", "typology.water.scale", spec.rules.get("typology.water.scale"), "typ_water", vec![("m3", s(round(t.water_m3d)))]);
    }
    let zoned = m.zones.iter().filter(|z| !z.adjacent_to.is_empty()).count();
    if zoned > 0 {
        add("typology", "goal.adjacency", spec.rules.get("goal.adjacency"), "typ_zones", vec![
            ("zones", s(m.zones.len())),
            ("wishes", s(zoned)),
        ]);
    }
    if t.live_load_kpa > 0.0 {
        let top = sch.rows.iter().max_by(|a, b| a.live_load_kpa.total_cmp(&b.live_load_kpa));
        add("typology", "typology.live_load_kpa", spec.rules.get("typology.live_load_kpa"), "typ_load", vec![
            ("kpa", s(t.live_load_kpa)),
            ("name", top.map_or(String::new(), |r| r.name.clone())),
        ]);
    }
    if t.persons > 0.0 {
        let per100 = spec.rules.get("typology.egress.width_per_100");
        let have: f64 = p.storeys.first().map_or(0.0, |x| x.layout.stairs.iter().map(|y| y.width).sum());
        add("typology", "typology.egress.width_per_100", per100, "typ_egress", vec![
            ("persons", s(t.persons.round())),
            ("exits", s(m.egress.exits_min)),
            ("have", s(round(have))),
            ("w", s(per100)),
        ]);
    }
    if let Some(b) = &m.below {
        let names: Vec<String> = b.levels.iter().map(|l| (if ru { &l.name.1 } else { &l.name.0 }).clone()).collect();
        let rooms: usize = p.basements.iter().map(|x| x.rooms.len()).sum();
        add("basement", "basement.count", p.basements.len() as f64, "typ_below", vec![
            ("n", s(p.basements.len())),
            ("names", names.join(", ")),
            ("rooms", s(rooms)),
        ]);
        if b.retaining {
            let walls = p.basements.iter().flat_map(|x| &x.layout.walls).filter(|w| w.role == crate::WallRole::Retaining).count();
            let high = p.basements.iter().map(|x| x.height).fold(0.0, f64::max);
            add("basement", "underground.retaining_thickness", spec.rules.get("underground.retaining_thickness"), "typ_retain", vec![
                ("n", s(walls)),
                ("t", s(spec.rules.get("underground.retaining_thickness"))),
                ("h", s(high)),
            ]);
        }
    }
    if let Some(pd) = m.periodic.iter().find(|p| p.at.is_empty()) {
        let every = pd.every.get(&spec.rules);
        let n = (0..p.storeys.len()).filter(|k| m.is_periodic(&pd.id, *k, spec.floors, &spec.rules)).count();
        add("typology", "typology.refuge.every", every, "typ_refuge", vec![("n", s(n)), ("every", s(every))]);
    }
    if m.units.len() == 3 {
        let n: Vec<u32> = m.units.iter().map(|u| u.count.get(&spec.rules).round() as u32).collect();
        add("typology", "typology.mix.two", f64::from(n[1]), "typ_mix", vec![
            ("flats", s(n.iter().sum::<u32>())),
            ("one", s(n[0])),
            ("two", s(n[1])),
            ("three", s(n[2])),
        ]);
    }
    let briefs = m.plant_briefs(&sch, &spec.rules);
    if !briefs.is_empty() {
        let kinds: Vec<String> = briefs.iter().filter_map(|b: &J| b["kind"].as_str().map(str::to_string)).collect();
        add("plant", "typology.cooling.scale", spec.rules.get("typology.cooling.scale"), "typ_plant", vec![
            ("n", s(briefs.len())),
            ("kinds", kinds.join(", ")),
        ]);
    }
    out
}
