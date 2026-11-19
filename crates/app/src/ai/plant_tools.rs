//! `design_plant`: a process plant (boiler house, pumping station, water treatment, process
//! line) designed from a brief by rules and drafted as one plan item: units with their nozzle
//! connectors, the items on every nozzle, pipes with their fittings, supports, platforms,
//! ladders, stairs and flues. Apply writes it as one undo step; the same `plant_id` later
//! brings the stored plant to a new brief in place.

use super::fmt::{self, Out};
use super::plan::PlanOp;
use super::plant_plan::{self, PlantOp, SLOTS};
use super::session::{Core, Done, Handler};
use super::tools_write::why_of;
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use ddc_gen::plant;
use serde_json::{json, Value as J};

pub const HANDLERS: [(&str, Handler); 1] = [("design_plant", design_plant)];

/// What the answer carries beside the summary.
const SHOW: [&str; 5] = ["summary", "equipment", "lines", "why", "all"];

/// Plant ids of the drafted items.
fn drafted(c: &Core) -> Vec<String> {
    c.plan
        .items
        .iter()
        .filter_map(|i| match &i.op {
            PlanOp::Plant(p) => Some(p.pid.clone()),
            _ => None,
        })
        .collect()
}

fn design_plant(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let show = a["show"].as_str().unwrap_or("summary");
    if !SHOW.contains(&show) {
        return Err(format!("show is one of {}", SHOW.join(", ")));
    }
    let brief = match &a["brief"] {
        J::Object(_) => a["brief"].clone(),
        J::String(s) => json!({"text": s}),
        _ => return Err("brief: the plant to design, as text or {kind, duty_kw, flow_m3h, head_m, units, rules}".into()),
    };
    let p = plant::design(&brief)?;
    let stored = plant_plan::stored(&c.ed.model).map_err(|e| e.to_string())?;
    let (pid, level) = match a["plant_id"].as_str() {
        Some(id) => {
            let known = drafted(c).iter().any(|d| d == id);
            match stored.iter().find(|s| s.0 == id) {
                Some((_, head)) => (id.to_string(), head["level"].as_i64().unwrap_or(c.view.level)),
                None if known => (id.to_string(), c.view.level),
                None => return Err(format!("no plant {id} in this plan or the model")),
            }
        }
        None => (plant_plan::next_pid(&c.ed.model, &drafted(c)).map_err(|e| e.to_string())?, c.view.level),
    };
    if !c.ed.levels().iter().any(|l| l.0 == level) {
        return Err(format!("the level of {pid} was deleted: design the plant again without plant_id"));
    }
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(SLOTS, item);
    let title = match a["title"].as_str() {
        Some(s) if !s.trim().is_empty() => fmt::text(s),
        _ => format!("{} ({pid})", p.to_json()["kind"].as_str().unwrap_or("plant")),
    };
    let op = PlanOp::Plant(PlantOp { input: brief, level, slot0, pid: pid.clone() });
    let mut o = Out::new(c.max_chars);
    match c.add_item(title, why_of(a), op, cx.actor) {
        Ok(added) => {
            o.put("item", &json!(added.0));
            o.put("changes", &json!({"created": added.1 .0, "changed": added.1 .1, "deleted": added.1 .2}));
        }
        // the stored plant already is this brief: nothing to draft, which is the answer
        Err(e) if a["plant_id"].is_string() && CmdError::from_text(&e).code == ErrorCode::NoEffect => {
            o.put("unchanged", &json!(true));
            o.put("changes", &json!({"created": 0, "changed": 0, "deleted": 0}));
        }
        Err(e) => return Err(e),
    }
    o.put("plant_id", &json!(pid));
    o.put_own("elements", &plant_plan::summary(&p)["elements"]);
    let all = p.to_json();
    o.put_own("kind", &all["kind"]);
    o.put_own("hall", &all["hall"]);
    let r = &p.report;
    let issues: Vec<J> = r.issues.iter().map(|i| json!(format!("{}: {}", i.check, i.what))).collect();
    o.put_own(
        "report",
        &json!({"ok": r.ok, "nozzles": r.nozzles, "connected": r.connected, "valves": r.valves,
                "valves_in_reach": r.valves_in_reach, "min_margin_m": r.min_margin}),
    );
    o.list_capped("issues", &issues, 1500);
    o.put_own("totals", &all["schedules"]["totals"]);
    let units: Vec<J> = all["equipment"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| json!({"tag": e["tag"], "kind": e["kind"], "role": e["role"], "standby": e["standby"], "duty": e["duty"]}))
        .collect();
    let why: Vec<J> = p.why.iter().map(|y| json!({"rule": y.rule, "source": y.source, "why": y.effect})).collect();
    let lines = all["schedules"]["lines"].as_array().cloned().unwrap_or_default();
    match show {
        "equipment" => {
            o.list("equipment", &units);
        }
        "lines" => {
            o.list("lines", &lines);
        }
        "why" => {
            o.list("why", &why);
        }
        "all" => {
            let third = o.left() / 3;
            o.list_capped("equipment", &units, third);
            o.list_capped("why", &why, third);
            o.list("lines", &lines);
        }
        _ => {
            o.list_capped("equipment", &units, 2500);
            // each line with its service and size, so the answer can quote them
            let short: Vec<J> = lines
                .iter()
                .map(|l| json!(format!("{} {} DN {} PN {} {} m", l["number"].as_str().unwrap_or(""), l["service"].as_str().unwrap_or(""), l["dn"], l["pn"], l["length"])))
                .collect();
            o.list_capped("lines", &short, 2500);
            let top: Vec<J> = why.iter().take(12).cloned().collect();
            o.list("why", &top);
        }
    }
    o.put_own("note", &all["note"]);
    o.put("revise", &json!("design_plant with this plant_id and a new brief changes the plant in place, also after Apply"));
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boiler_house_brief_designs_a_checked_plant() {
        let p = plant::design(&json!({"text": "boiler house 1 MW"})).unwrap();
        assert!(p.report.ok, "{:?}", p.report.issues);
        assert!(plant::design(&json!({"text": "a shed"})).is_err());
    }
}
