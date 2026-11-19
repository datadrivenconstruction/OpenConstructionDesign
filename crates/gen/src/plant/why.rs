//! Why the plant came out as it did: each decision names its `plant.*` rule and the numbers it
//! gave. The UI renders `gen.why.e.<key>` with the same slots in its own language.

use std::fmt::Display;

use serde_json::Value as J;

use crate::project::rules;
use crate::project::why::Why;
use crate::project::why_text;

use super::layout::Lay;
use super::route::Piping;
use super::topo::Topo;
use super::{EqKind, Svc};

/// English templates of the plant's reasons.
pub const EN: &[(&str, &str)] = &[
    ("plant_pumps", "{group}: {duty} duty and {standby} standby pumps"),
    ("plant_pump_motor", "{tag}: {q} m3/h at {head} m takes {shaft} kW at the shaft, motor {kw} kW"),
    ("plant_pump_split", "{flow} m3/h shared by {duty} duty pumps of at most {max} m3/h each from {tag}"),
    ("plant_tank", "{tag}: {vol} m3 holds {hold} h of flow"),
    ("plant_storage_tank", "{tag}: {d} m wide, {h} m shell, {vol} m3 under a cone roof rising {rise} m"),
    ("plant_tank_nozzles", "{tag}: {n} nozzles DN {dn} at {z} m and a manway of {md} m"),
    ("plant_tank_ring", "{tag}: ring foundation {w} m wide and {depth} m deep under the {d} m shell"),
    ("plant_line_dn", "{line}: DN {dn} carries {flow} m3/h at {v} m/s"),
    ("plant_boiler_count", "{n} boilers in service so none is above {max} kW"),
    ("plant_boilers", "{units} boilers of {kw} kW: {duty} kW plus {margin} % margin over {duty_n} in service"),
    ("plant_boiler_standby", "{n} boilers on standby beside {duty_n} in service"),
    ("plant_flue", "{tag}: flue {d} m at {v} m/s for {gas} m3/h of gas"),
    ("plant_separator", "hydraulic separator {d} m for {flow} m3/h at {v} m/s"),
    ("plant_boiler_pumps", "one pump per boiler: {n} pumps of {q} m3/h at {head} m"),
    ("plant_softener", "{tag}: {d} m for {q} m3/h at {rate} m/h"),
    ("plant_makeup", "make-up {q} m3/h: {rate} of the {sys} m3 system per hour"),
    ("plant_expansion", "expansion vessel {vol} L for {sys} L of water, filled at {fill} bar, relief at {safety} bar"),
    ("plant_surge", "{tag}: {vol} m3 holds {s} s of flow"),
    ("plant_filters", "{n} filters of {d} m for {flow} m3/h at {rate} m/h, and {standby} standby"),
    ("plant_dosing", "{tag}: {vol} L tank for {q} L/h of dosing"),
    ("plant_reactor", "{tag}: {vol} m3 for a {batch} h batch filled to {fill} %"),
    ("plant_reactor_heat", "{tag}: {kw} kW of heating takes {q} m3/h of hot water"),
    ("plant_column", "{tag}: {vol} m3 holds {hold} min of feed, {d} m wide and {h} m tall"),
    ("plant_reflux", "{tag}: reflux ratio {r} sends {q} m3/h back and boils up {v} m3/h"),
    ("plant_bund", "bund {w} x {l} m, {h} m high, holds {vol} m3 round {n} tanks, the largest {big} m3"),
    ("plant_exchanger", "{tag}: {area} m2 for {kw} kW at U {u} W/m2K"),
    ("plant_aisle", "aisle {gap} m between the row fronts, walkway {w} m"),
    ("plant_spacing", "{a} to {b}: {gap} m for the clearances and pipe stubs"),
    ("plant_rack", "rack from {z} m, {n} tiers {tier} m apart, trapezes every {bay} m"),
    ("plant_platform", "{tag} at {deck} m for the valves of {unit} out of reach"),
    ("plant_ladder", "{tag}: ladder of {n} rungs"),
    ("plant_ladder_cage", "{tag}: caged ladder of {n} rungs"),
    ("plant_stair", "{tag}: stair of {n} treads at {angle} degrees"),
    ("plant_ship", "{tag}: ship ladder of {n} treads at {angle} degrees, the stair would not fit"),
    ("plant_ship_angle", "ship ladder pitch raised to {angle} degrees: it must be steeper than the stair at {stair}"),
    ("plant_drain", "drains fall {pct} % to {tag}, {fall} m over the run"),
    ("plant_insul", "{n} hot lines insulated {mm} mm"),
    ("plant_supports", "{n} supports, spans up to {span} m"),
    ("plant_hall", "hall {x} x {y} m, {z} m clear"),
];

pub fn template(key: &str) -> Option<&'static str> {
    EN.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

pub(super) fn s(x: impl Display) -> String {
    x.to_string()
}

fn trim(t: String) -> String {
    if t.contains('.') {
        t.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        t
    }
}

pub(super) fn s1(x: f64) -> String {
    trim(format!("{x:.1}"))
}

pub(super) fn s2(x: f64) -> String {
    trim(format!("{x:.2}"))
}

/// Adds one reason: `key` filled with `args` because of rule `rule_id`.
pub(super) fn add(out: &mut Vec<Why>, target: &str, rule_id: &str, key: &'static str, args: Vec<(&'static str, String)>) {
    let x = rules::rule(rule_id);
    let value = rules::def(rule_id).map_or(J::Null, |d| d.value_json(x));
    let effect = why_text::fill(template(key).unwrap_or(key), &args);
    out.push(Why { target: target.to_string(), rule: rule_id.to_string(), value, source: "builtin".into(), effect, key, args });
}

/// Reasons of the layout, the rack, the access, the drains, the supports and the hall.
pub(super) fn layout(out: &mut Vec<Why>, t: &Topo, lay: &Lay, p: &Piping) {
    let h = &lay.hall;
    add(out, "hall", "plant.aisle.width", "plant_aisle", vec![("gap", s2(lay.aisle_gap)), ("w", s2(h.walk.1 - h.walk.0))]);
    if let Some((a, b, g)) = &lay.spacing {
        add(out, a, "plant.clear.side", "plant_spacing", vec![("a", a.clone()), ("b", b.clone()), ("gap", s2(*g))]);
    }
    add(
        out,
        "rack",
        "plant.rack.tier",
        "plant_rack",
        vec![("z", s2(h.rack_z)), ("n", s(p.tiers.len())), ("tier", s2(lay.tier)), ("bay", s2(p.bay))],
    );
    if let Some(b) = &lay.bund {
        let vol = b.volume;
        add(
            out,
            "bund",
            "plant.bund.volume",
            "plant_bund",
            vec![("w", s2(b.hi.x - b.lo.x)), ("l", s2(b.hi.y - b.lo.y)), ("h", s2(b.h)), ("vol", s1(vol)), ("n", s(b.tanks.len())), ("big", s1(b.big))],
        );
    }
    let (ship, raised) = super::layout::ship_angle();
    if raised {
        add(out, "plant", "plant.stair.ship_angle", "plant_ship_angle", vec![("angle", s1(ship)), ("stair", s1(rules::rule("plant.stair.angle")))]);
    }
    for pf in &lay.platforms {
        add(out, &pf.tag, "plant.access.reach", "plant_platform", vec![("tag", pf.tag.clone()), ("deck", s2(pf.deck)), ("unit", pf.serves.clone())]);
        for a in std::iter::once(&pf.access).chain(&pf.extra) {
            let n = s(a.steps);
            if a.kind == "stair" {
                add(out, &pf.tag, "plant.access.stair_from", "plant_stair", vec![("tag", pf.tag.clone()), ("n", n), ("angle", s1(rules::rule("plant.stair.angle")))]);
            } else if a.kind == "ship_ladder" {
                add(out, &pf.tag, "plant.stair.max_run", "plant_ship", vec![("tag", pf.tag.clone()), ("n", n), ("angle", s1(ship))]);
            } else if a.caged {
                add(out, &pf.tag, "plant.ladder.cage_from", "plant_ladder_cage", vec![("tag", pf.tag.clone()), ("n", n)]);
            } else {
                add(out, &pf.tag, "plant.ladder.rung", "plant_ladder", vec![("tag", pf.tag.clone()), ("n", n)]);
            }
        }
    }
    if let Some(l) = t.lines.iter().find(|l| l.svc == Svc::Dr) {
        let sump = t.eqs.iter().find(|e| e.kind == EqKind::Sump).map_or(String::new(), |e| e.tag.clone());
        add(
            out,
            &l.number(),
            "plant.pipe.drain_slope",
            "plant_drain",
            vec![("pct", s2(rules::rule("plant.pipe.drain_slope"))), ("tag", sump), ("fall", s2(p.drain_fall))],
        );
    }
    let hot = t.lines.iter().filter(|l| l.insul > 0.0).count();
    if hot > 0 {
        let mm = (rules::rule("plant.pipe.insul") * 1000.0).round();
        add(out, "lines", "plant.pipe.insul_from", "plant_insul", vec![("n", s(hot)), ("mm", s(mm))]);
    }
    add(out, "supports", "plant.support.span", "plant_supports", vec![("n", s(p.supports.len())), ("span", s2(rules::rule("plant.support.span_max")))]);
    add(out, "hall", "plant.hall.module", "plant_hall", vec![("x", s2(h.length())), ("y", s2(h.width())), ("z", s2(h.z))]);
}
