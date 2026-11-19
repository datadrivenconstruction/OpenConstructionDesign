// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Brief against built: the facts a reviewer counts (storeys, area, rooms by kind) and, for
//! every feature the brief named, whether the project has it. A feature the spec could have
//! but does not gets a merge patch for revise_project; one the builder cannot make is named
//! so the reply calls it an approximation instead of dropping it silently.

use super::why::{fixes_of, RuleFix};
use super::{Balconies, FacadeSystem, GProject, Massing, ProjectSpec, RoofKind, Structure};
use serde_json::{json, Value as J};
use std::collections::BTreeMap;

/// Rounds of generate, check and repair before the loop gives up.
pub const MAX_ROUNDS: usize = 3;

#[derive(Clone, Debug, PartialEq)]
enum Need {
    Met,
    /// Built, but only as the stated stand-in.
    Approx(&'static str),
    /// The spec can say it: the patch adds it.
    Missing(J),
    /// The spec can say it, but not together with what it already says.
    Conflict(&'static str),
    /// Not made by the generator: the hint names another way.
    Unsupported(&'static str),
}

const OTHER_TOOLS: &str = "not made by the generator: add it after Apply with create_geometry \
    or run_script (help recipes), or name it as approximated";

fn massing(want: Massing, s: &ProjectSpec) -> Need {
    if s.design.massing == want {
        return Need::Met;
    }
    if want.wings() > 0 && (s.design.setback > 0.0 || s.design.cantilever > 0.0) {
        return Need::Conflict("wings exclude setback and cantilever: drop those or use tower_podium");
    }
    Need::Missing(json!({"design": {"massing": want.name()}}))
}

/// Keywords that ask for a glazed curtain wall facade.
pub fn asks_curtain_wall(f: &str) -> bool {
    crate::lang::brief_words("curtain_wall").iter().any(|w| w == f)
        || matches!(
        f,
        "curtain_wall" | "curtain_facade" | "glass_curtain_wall" | "glass_facade" | "glazed_facade"
            | "glass_curtain" | "glazed_curtain_wall" | "curtain" | "glass" | "glazed" | "glazing"
            | "glass_tower" | "glass_envelope" | "all_glass" | "fully_glazed"
            | "vorhangfassade" | "glasfassade" | "pfosten_riegel" | "pfosten-riegel-fassade"
            | "muro_cortina" | "fachada_cortina" | "muro_cortina_de_vidrio"
            | "幕墙" | "玻璃幕墙"
    )
}

/// Keywords that ask for a sprinkler system, in en, ru, de, es and zh.
pub fn asks_sprinklers(f: &str) -> bool {
    crate::lang::brief_words("sprinklers").iter().any(|w| w == f)
        || matches!(
        f,
        "sprinkler" | "sprinklers" | "sprinkler_system" | "fire_sprinklers"
            | "sprinkleranlage" | "sprinkleranlagen"
            | "rociadores" | "sistema_de_rociadores"
            | "喷淋" | "自动喷水灭火系统"
    )
}

/// The timber structure a feature keyword asks for, if any.
pub fn asks_timber(f: &str) -> Option<Structure> {
    match f {
        "glulam" | "timber_frame" | "post_and_beam" => Some(Structure::TimberFrame),
        "timber" | "clt" | "mass_timber" | "wood" => Some(Structure::Timber),
        _ => None,
    }
}

/// Feature words that ask for a free-form double-curved envelope: the shell, not the single bow.
pub fn asks_shell(f: &str) -> bool {
    matches!(
        f,
        "freeform" | "free_form" | "shell" | "double_curved" | "doubly_curved" | "double_curvature" | "organic"
            | "sculptural" | "flowing" | "wave" | "wavy" | "curved_roof" | "curved_shell" | "gridshell"
            | "curvy" | "undulating" | "fluid" | "double_curve" | "two_way_curved" | "curved_walls"
            | "curved_envelope" | "free_form_shell" | "blob"
    )
}

/// Words of a request (any language) that ask for a free-form double-curved envelope.
pub fn text_asks_shell(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "free-form", "free form", "freeform", "organic", "sculptural", "double-curved", "doubly curved",
        "doubly-curved", "double curved", "double curvature", "gridshell", "like a wave",
        "freiform", "organisch", "doppelt gekrümmt", "doppelt gekrummt",
        "forma libre", "orgánic", "doble curvatura", "曲面", "波浪", "流线", "有机", "自由形",
    ]
    .iter()
    .any(|w| t.contains(w))
        || crate::lang::brief_words("shell").iter().any(|w| t.contains(w.as_str()))
}

thread_local! {
    static REQUEST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// The person's own request for the chat turn in progress; the AI layer sets it before it
/// parses a spec, so the free-form words of the brief reach the shell rule without the model.
pub fn set_request_text(text: &str) {
    REQUEST.with(|r| *r.borrow_mut() = text.to_string());
}

pub fn request_text() -> String {
    REQUEST.with(|r| r.borrow().clone())
}

/// A curtain wall is met by curtain wall elements in the project, not by the facade name: a
/// facade system whose bays carried none would pass a check on the name alone.
fn curtain_need(s: &ProjectSpec, p: &GProject) -> Need {
    if s.design.facade != FacadeSystem::Curtain {
        return Need::Missing(json!({"design": {"facade": "curtain"}}));
    }
    let (storeys, walls) = curtain_counts(p);
    let blocks = p.storeys.len();
    if walls == 0 {
        return Need::Conflict(
            "the curtain facade built no curtain wall: no outer bay is long enough or all are doors; widen the footprint or lower facade.curtain_module",
        );
    }
    if storeys < blocks {
        return Need::Approx("curtain walls on some storeys only; the others keep their windows");
    }
    Need::Met
}

/// Storeys with a curtain wall, and curtain walls in all.
pub fn curtain_counts(p: &GProject) -> (usize, usize) {
    let with = p.storeys.iter().filter(|t| !t.curtains.is_empty()).count();
    (with, p.storeys.iter().map(|t| t.curtains.len()).sum())
}

fn judge(f: &str, s: &ProjectSpec, p: &GProject) -> Need {
    let d = &s.design;
    let roof = |k: RoofKind, name: &str| {
        if s.roof == k {
            Need::Met
        } else {
            Need::Missing(json!({"roof": {"kind": name}}))
        }
    };
    let upper = || {
        if s.floors > 1 {
            Need::Met
        } else {
            Need::Missing(json!({"floors": 2}))
        }
    };
    match f {
        "multi_floor" | "multi_storey" | "stairs" | "stair" => upper(),
        "lift" | "elevator" if s.lift => Need::Met,
        "lift" | "elevator" if s.floors > 1 => Need::Missing(json!({"lift": true})),
        "lift" | "elevator" => Need::Missing(json!({"floors": 2, "lift": true})),
        "pitched_roof" | "gable_roof" => roof(RoofKind::Pitched, "pitched"),
        "flat_roof" => roof(RoofKind::Flat, "flat"),
        "sloped_roof" if s.roof == RoofKind::Pitched => {
            Need::Approx("a pitched roof stands in for the sloped one")
        }
        "sloped_roof" => roof(RoofKind::Pitched, "pitched"),
        "l_shape" => massing(Massing::L, s),
        "u_shape" => massing(Massing::U, s),
        "courtyard" if d.massing == Massing::U => Need::Approx("a U around an open court"),
        "courtyard" => massing(Massing::Courtyard, s),
        "podium" | "tower_podium" => massing(Massing::TowerPodium, s),
        "balcony" | "balconies" if d.balconies != Balconies::None => Need::Met,
        "balcony" | "balconies" => Need::Missing(json!({"design": {"balconies": "balcony"}})),
        "loggia" if d.balconies == Balconies::Loggia => Need::Met,
        "loggia" => Need::Missing(json!({"design": {"balconies": "loggia"}})),
        "setback" if d.setback > 0.0 => Need::Met,
        "setback" if d.massing.wings() > 0 => {
            Need::Conflict("a setback steps a bar or tower_podium, not wings")
        }
        "setback" if d.cantilever > 0.0 => Need::Conflict("either setback or cantilever, not both"),
        "setback" if s.floors < 2 => Need::Missing(json!({"floors": 3, "design": {"setback": 2.0}})),
        "setback" => Need::Missing(json!({"design": {"setback": 2.0}})),
        "cantilever" if d.cantilever > 0.0 => Need::Met,
        "cantilever" if d.massing.wings() > 0 => {
            Need::Conflict("a cantilever extends a bar or tower_podium, not wings")
        }
        "cantilever" if d.setback > 0.0 => Need::Conflict("either setback or cantilever, not both"),
        "cantilever" if s.floors < 2 => {
            Need::Missing(json!({"floors": 2, "design": {"cantilever": 1.5}}))
        }
        "cantilever" => Need::Missing(json!({"design": {"cantilever": 1.5}})),
        f if asks_curtain_wall(f) => curtain_need(s, p),
        "fins" if d.facade == FacadeSystem::Fins => Need::Met,
        "fins" => Need::Missing(json!({"design": {"facade": "fins"}})),
        "terrace" | "roof_terrace" if d.terrace => Need::Met,
        "terrace" | "roof_terrace" => Need::Missing(json!({"design": {"terrace": true}})),
        "green_roof" if d.terrace => Need::Approx("roof planters stand in for a green roof"),
        "green_roof" => Need::Missing(json!({"design": {"terrace": true}})),
        f if asks_sprinklers(f) && p.fire.heads > 0 => Need::Met,
        f if asks_sprinklers(f) => Need::Missing(json!({"sprinklers": true})),
        "standpipe" | "standpipes" | "wet_riser" | "fire_riser" if !p.fire.standpipes.is_empty() => Need::Met,
        "standpipe" | "standpipes" | "wet_riser" | "fire_riser" => {
            Need::Missing(json!({"rules": {"fire.standpipe_height": 0}}))
        }
        "hvac" | "mep" | "ventilation" if s.services.hvac == Some(false) => {
            Need::Missing(json!({"services": {"hvac": true}}))
        }
        "mep" | "hvac" | "ventilation" if !p.hvac.ducts.is_empty() => Need::Approx(
            "supply and extract ducts with risers in shafts; water and drainage come with \
             services.water, power with services.electrical; sprinklers and standpipes with the fire rules",
        ),
        "basement" | "cellar" if s.basements > 0 => Need::Met,
        // a garage asked on the ground floor is a parking room there, not a basement
        "garage" | "parking"
            if p.storeys.first().is_some_and(|g| g.rooms.iter().any(|r| r.kind == super::RoomKind::Parking)) =>
        {
            Need::Met
        }
        "parking" | "underground_parking" | "basement_parking" | "garage" if s.basements > 0 => {
            Need::Approx("parking bays below ground; the vehicle ramp is not drawn")
        }
        "basement" | "cellar" | "parking" | "underground_parking" | "basement_parking" | "garage"
            if s.floors < 2 =>
        {
            Need::Missing(json!({"floors": 2, "basements": 1}))
        }
        "basement" | "cellar" | "parking" | "underground_parking" | "basement_parking" | "garage" => {
            Need::Missing(json!({"basements": 1}))
        }
        "curved" | "curved_facade" | "arc" if p.shell.is_some() => Need::Met,
        "glulam" | "timber_frame" | "post_and_beam" if d.structure == Structure::TimberFrame => {
            Need::Approx("glulam columns on the grid, CLT floors and roof, the corridor spine in concrete; glulam beams between the columns")
        }
        "glulam" | "timber_frame" | "post_and_beam" => Need::Missing(json!({"design": {"structure": "timber_frame"}})),
        "timber" | "clt" | "mass_timber" | "wood" if d.structure.is_timber() => {
            Need::Approx("CLT floors and roof and timber walls, the corridor spine in concrete")
        }
        "timber" | "clt" | "mass_timber" | "wood" => Need::Missing(json!({"design": {"structure": "timber"}})),
        "frame" | "column_frame" | "skeleton" if d.structure.is_frame() => Need::Met,
        "frame" | "column_frame" | "skeleton" => Need::Missing(json!({"design": {"structure": "frame"}})),
        "masonry" | "brick" if d.structure == Structure::Masonry => Need::Met,
        "masonry" | "brick" => Need::Missing(json!({"design": {"structure": "masonry"}})),
        "curved" | "curved_facade" | "arc" if d.curve > 0.0 || d.curve_north > 0.0 => Need::Met,
        "curved" | "curved_facade" | "arc"
            if d.massing != Massing::Bar || d.setback > 0.0 || d.cantilever > 0.0 =>
        {
            Need::Conflict("a curved facade bows a plain bar, not wings or a stepped block")
        }
        "curved" | "curved_facade" | "arc" => Need::Missing(json!({"design": {"curve": 3.0}})),
        f if asks_shell(f) && p.shell.is_some() => Need::Met,
        f if asks_shell(f) && (s.plate.is_some() || s.design.massing == Massing::Bar) => {
            Need::Missing(json!({"design": {"shell": true}}))
        }
        "freeform" | "twisting" => Need::Unsupported(
            "straight walls only: a curved shell comes from create_geometry or run_script \
             (help curved_facade), or name the approximation",
        ),
        "atrium" if d.atrium => {
            Need::Approx("the courtyard roofed over, open through every upper floor; no galleries inside")
        }
        "atrium" if d.massing != Massing::Bar && d.massing != Massing::Courtyard => {
            Need::Conflict("the atrium roofs a courtyard block, not this massing")
        }
        "atrium" if d.setback > 0.0 || d.cantilever > 0.0 => {
            Need::Conflict("the atrium roofs a courtyard block, which takes no setback or cantilever")
        }
        "atrium" => Need::Missing(json!({"design": {"massing": "courtyard", "atrium": true}})),
        f if f == "operating_rooms" || f.starts_with("operating_rooms:") => operating_need(f, s),
        _ => Need::Unsupported(OTHER_TOOLS),
    }
}

/// Operating theatres asked (`operating_rooms` or `operating_rooms:3`) against the spec rooms
/// tagged `operating`.
fn operating_need(f: &str, s: &ProjectSpec) -> Need {
    let asked = f.split_once(':').and_then(|(_, n)| n.parse::<u32>().ok()).unwrap_or(1);
    let tag = super::RoomTags::parse("operating").unwrap_or_default();
    // the preset's model holds the theatres; spec rooms count too, for a programme given by hand
    let from_rooms: u32 = s.rooms.iter().chain(&s.ground_rooms).filter(|r| s.room_tags.of(r.kind, &r.name).meets(tag)).map(|r| r.count).sum();
    let from_model: u32 = s
        .template
        .as_deref()
        .and_then(super::presets::find)
        .and_then(|t| t.model.as_deref())
        .map_or(0, |m| m.spaces.iter().filter(|x| x.tags.meets(tag)).map(|x| x.count_in(Some(&s.rules))).sum());
    let built = from_rooms.max(from_model);
    match built {
        n if n >= asked => Need::Met,
        0 => Need::Unsupported("no operating theatres in this typology: use typology hospital_wing"),
        _ => Need::Unsupported(
            "fewer operating theatres than asked: raise the count of the theatre space in the programme (generate_project program)",
        ),
    }
}

/// What a check found; `repair` is one merge patch for all the missing features.
#[derive(Clone, Debug, PartialEq)]
pub struct BriefCheck {
    pub report: J,
    pub repair: Option<J>,
    /// Nothing the spec could still add is missing.
    pub settled: bool,
    /// The repair as rule changes, each naming the feature it serves.
    pub rules: Vec<RuleFix>,
}

/// Deep merge of patches, the earlier value kept on a clash.
fn fold(into: &mut J, patch: &J) {
    let (Some(a), Some(b)) = (into.as_object_mut(), patch.as_object()) else {
        return;
    };
    for (k, v) in b {
        match a.get_mut(k) {
            Some(x) if x.is_object() && v.is_object() => fold(x, v),
            Some(_) => {}
            None => {
                a.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Rooms of the project by kind, core rooms included.
pub fn rooms_by_kind(p: &GProject) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for r in p.blocks().flat_map(|(_, s)| &s.rooms) {
        *out.entry(r.kind.key()).or_insert(0) += 1;
    }
    out
}

/// The data centre fit-out's feature a brief word asks for.
fn dc_feature(f: &str) -> Option<&'static str> {
    Some(match f {
        "data_hall" | "data_center" | "data_centre" | "server_room" | "white_space" => "data_hall",
        "hot_cold_aisle" | "hot_aisle" | "cold_aisle" | "racks" => "hot_cold_aisle",
        "raised_floor" => "raised_floor",
        "crah" | "crac" | "cooling_units" => "crah",
        "chilled_water" | "chiller" | "chillers" => "chilled_water",
        "ups" | "batteries" | "battery_room" => "ups",
        "generators_outside" | "generators" | "generator" | "genset" | "generator_yard" => "generators_outside",
        "cable_tray" | "cable_trays" => "cable_tray",
        "aisle_containment" | "containment" | "containment_doors" => "aisle_containment",
        "pdu" | "pdus" | "power_distribution" => "pdu",
        _ => return None,
    })
}

const DC_ROOMS: &str = "made by the data centre fit-out once a room is a data hall: tag rooms data_hall,     ups_room, battery_room, cooling_plant (or name them so) and set the dc.* rules";

pub fn check(s: &ProjectSpec, p: &GProject) -> BriefCheck {
    let (mut met, mut approx, mut missing, mut conflict, mut unsupported) =
        (vec![], vec![], vec![], vec![], vec![]);
    let mut patch = json!({});
    let mut fixes: Vec<RuleFix> = vec![];
    let mut dc: Option<Vec<(&'static str, bool)>> = None;
    for f in &s.features {
        let need = match dc_feature(f) {
            Some(key) => {
                let fs = dc.get_or_insert_with(|| super::dc::features(s, &super::dc::design(s, p)));
                if fs.iter().any(|x| x.0 == key && x.1) {
                    Need::Met
                } else {
                    Need::Unsupported(DC_ROOMS)
                }
            }
            None => judge(f, s, p),
        };
        match need {
            Need::Met => met.push(json!(f)),
            Need::Approx(how) => approx.push(json!({"feature": f, "as": how})),
            Need::Missing(x) => {
                // wings and a step cannot both come in one patch
                let wings = |v: &J| {
                    v["design"]["massing"]
                        .as_str()
                        .is_some_and(|m| ["l", "u", "courtyard"].contains(&m))
                };
                // a curve also takes the plain bar, like a step
                let steps = |v: &J| {
                    ["setback", "cantilever", "curve"].iter().any(|k| !v["design"][*k].is_null())
                };
                let m = |v: &J| v["design"]["massing"].as_str().map(str::to_string);
                let two_massings = m(&x).is_some() && m(&patch).is_some() && m(&x) != m(&patch);
                if two_massings
                    || (wings(&x) && steps(&patch))
                    || (steps(&x) && (wings(&patch) || steps(&patch)))
                {
                    conflict.push(json!({"feature": f, "why": "clashes with another repair in this patch: ask the person which to keep"}));
                    continue;
                }
                fold(&mut patch, &x);
                fixes.extend(fixes_of(&x, f));
                missing.push(json!({"feature": f, "patch": x}));
            }
            Need::Conflict(why) => conflict.push(json!({"feature": f, "why": why})),
            Need::Unsupported(how) => unsupported.push(json!({"feature": f, "how": how})),
        }
    }
    // racks asked that the halls cannot seat are said, not silently cut
    if let Some(asked) = s.rules.value("dc.racks").map(|x| x.round() as usize).filter(|n| *n > 0) {
        let g = super::dc::design(s, p);
        let fit = g.count(super::dc::Kind::Rack);
        if !g.halls.is_empty() && fit < asked {
            let area = super::rules::scoped(&s.rules, || super::dc::hall_area(asked));
            unsupported.push(json!({"feature": "dc.racks", "how": format!(
                "{asked} racks asked, the halls seat {fit}: enlarge the data halls to about {area:.0} m2 in all (dc.hall_m2_rack per rack) with revise_project")}));
        }
    }
    let gfa = p
        .metrics
        .iter()
        .find(|m| m.0 == "gross_floor_area")
        .map_or(0.0, |m| (m.1 * 10.0).round() / 10.0);
    let settled = missing.is_empty();
    let next = if !settled {
        "call revise_project with `repair` (or your own patch), then check again; at most 3 rounds"
    } else if unsupported.is_empty() && approx.is_empty() && conflict.is_empty() {
        "the brief is met as far as its features go"
    } else {
        "tell the person which features are approximated or not built, in one line each"
    };
    let mut report = json!({
        "typology": {"asked": s.typology_name(), "built_as": s.typology.name()},
        "storeys": {"asked": s.floors, "built": p.storeys.len()},
        "gross_floor_area_m2": gfa,
        "rooms_by_kind": rooms_by_kind(p),
        "features": {"met": met, "approximated": approx, "missing": missing,
                     "conflict": conflict, "not_built": unsupported},
        "next": next,
    });
    let (cs, cw) = curtain_counts(p);
    if cw > 0 || s.design.facade == FacadeSystem::Curtain {
        let spandrels = p.storeys.iter().flat_map(|t| &t.curtains).filter(|c| c.spandrel > 0.0).count();
        report["curtain_walls"] = json!({"count": cw, "storeys": cs, "with_spandrel": spandrels});
    }
    if !s.notes.is_empty() {
        report["notes"] = json!(s.notes);
    }
    let repair = (!settled).then_some(patch);
    if let Some(r) = &repair {
        report["repair"] = r.clone();
        report["rule_repair"] = J::Array(fixes.iter().map(RuleFix::to_json).collect());
    }
    BriefCheck {
        report,
        repair,
        settled,
        rules: if settled { vec![] } else { fixes },
    }
}

/// One round of the loop: the spec built and what the check said.
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub spec: J,
    pub check: BriefCheck,
}

/// Generate, check against the brief, repair with the check's own patch; stops when settled,
/// when a build fails, or after [`MAX_ROUNDS`]. The model runs the same loop through
/// revise_project; this is the deterministic form the tests and the harness replay.
pub fn verify_loop(
    brief: J,
    build: &mut dyn FnMut(&J) -> Result<(ProjectSpec, GProject), String>,
) -> Result<Vec<Round>, String> {
    let mut rounds: Vec<Round> = Vec::new();
    let mut spec = brief;
    for _ in 0..MAX_ROUNDS {
        let (s, p) = build(&spec)?;
        let c = check(&s, &p);
        let next = c.repair.clone();
        rounds.push(Round {
            spec: spec.clone(),
            check: c,
        });
        match next {
            Some(patch) => super::merge_patch(&mut spec, &patch),
            None => break,
        }
    }
    Ok(rounds)
}
