// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The effect texts of [`super::why::Why`] as keyed templates with named `{slots}`. The English
//! template fills `effect` for the tool reply; the UI renders `gen.why.e.<key>` from its own
//! catalog with the same slots, so a reason reads in the UI language, not in English.

/// `(key, English template)`; the UI catalog's `gen.why.e.<key>` in `en` is the same text.
pub const EN: &[(&str, &str)] = &[
    ("depth_band", "footprint depth {d} m = depth.band {band} x 2 + corridor.width {corr}"),
    ("corridor", "corridor {w} m wide on every storey"),
    ("corridor_fp", "corridor {w} m wide; footprint {x} x {y} m"),
    ("floors", "{n} storeys of {h} m: {top} m to the roof"),
    ("massing_form", "{form} massing, {wings} wing(s) per storey"),
    ("setback", "upper storeys narrower each side by {x} m"),
    ("cantilever", "upper storeys overhang each end by {x} m"),
    ("curve_s", "south facade bows out at its middle by {x} m"),
    ("curve_n", "north facade bows out at its middle by {x} m"),
    ("shell", "free-form shell: {n} patches ({k} double-curved), {p} panels ({flat} flat, {cold} cold-bent, {hot} hot-bent), diagonal gap at most {gap} mm, {oc} m opening"),
    ("stairs", "{n} stair(s) with {w} m flights, risers at most {r} m"),
    ("norm_flight", "norm: flights at least {lim} m; built {built} m"),
    ("norm_riser", "norm: risers at most {lim} m; built {built} m"),
    ("door_zone", "free strip between the stair and its door"),
    ("lift_room", "{n} lift shaft(s) in a {w} m lift room"),
    ("sprinklers", "{heads} sprinkler heads cover {cov} of {area} m2, at most {per} m2 each; {risers} riser(s) in {zones} pressure zone(s)"),
    ("sprinkler_hazard", "{n} room(s) of {class} hazard: heads at most {sp} m apart, {area} m2 and {q} l/min each"),
    ("standpipes", "{n} standpipe(s) in the stair cores, {valves} landing valves, as the building is {h} m high; hose reach {reach} m from a valve"),
    ("stair_pressure", "the stairs are pressurised against smoke: the building is {h} m high"),
    ("lift_family", "{persons}-person cars ({class} kg, {car} m) in shafts of at least {shaft} m with a landing door on each of {levels} storeys, a {pit} m pit and {head} m headroom over the top stop; average wait {interval} s"),
    ("lift_fighting", "the top storey is {h} m up: every car is of at least {load} kg so one can serve the fire brigade"),
    ("lift_family_n", "{persons}-person cars ({class} kg, {car} m) in shafts of at least {shaft} m with a landing door on each of {levels} storeys, a {pit} m pit and {head} m headroom over the top stop"),
    ("lift_cars", "{cars} lift car(s) of the {wanted} the peak traffic of {demand} persons asks for, {zones} zone(s)"),
    ("curtain", "{n} curtain wall bays: mullions every {m} m, spandrel {sp} m high"),
    ("curtain_brief", "the brief asks for a curtain wall: {n} curtain walls on {k} storeys instead of windows in wall bays"),
    ("grid_span", "{axes} grid axes, spans at most {span} m; {cols} column(s) of {col} m"),
    ("grid_form", "{form} grid read from the plate outline; columns only where every storey leaves them free"),
    ("grid_usable", "{pct}% of the rooms hold a rectangle in the grid axes this wide both ways"),
    ("glazing", "{n} windows, {glass} m2 of glass: each lit room glazed to this share of its floor"),
    ("edge_clear", "windows and doors keep this from wall junctions"),
    ("doors", "{n} doors {w} m wide"),
    ("doors_wet", "{n} bath and wc doors {w} m wide"),
    ("doors_wide", "{n} wide and unit doors {w} m wide"),
    ("open_types", "{doors} door and {windows} window types over {n} openings, marks {first} to {last}; clear width is the opening less {loss} m for frame and leaf"),
    ("open_swing_exit", "{n} door(s) open with the escape direction: {out} exit(s) outward, {stair} into a stair"),
    ("open_swing_route", "{n} door(s) of rooms of {area} m2 or more open towards the route"),
    ("open_swing_room", "{n} door(s) keep their leaf in the room: in the corridor it would leave less than {clear} m"),
    ("open_fire_stair", "{n} door(s) in stair core walls are EI {min}"),
    ("open_fire_unit", "{n} flat entrance door(s) are EI {min}"),
    ("open_fire_shaft", "{n} door(s) to lifts and technical rooms are EI {min}"),
    ("open_fire_compartment", "{n} door(s) in walls between dwellings are EI {min}"),
    ("open_clear_access", "{n} door(s) on the accessible route keep {clear} m clear: {widened} widened, {short} short"),
    ("open_clear_exit", "{n} exit and stair door(s) keep {clear} m clear: {widened} widened, {short} short"),
    ("open_rw_unit", "{n} flat entrance door(s) reduce sound by {rw} dB or more"),
    ("open_rw_exterior", "{n} window(s) and exterior door(s) reduce sound by {rw} dB or more"),
    ("open_u", "{n} exterior opening(s) at U {u} W/m2K or better"),
    ("min_unit", "{n} lit rooms, none narrower along the facade than this"),
    ("storey", "storey {k}: {rooms} rooms, {h} m high"),
    ("basement_count", "{n} storey(s) below ground"),
    ("basement_park", "{park} parking deck(s); strips narrower than this are {store} store(s)"),
    ("paving", "paving {w} m wide, {trees} tree(s)"),
    ("tree_season", "{n} tree(s) stand in the season the rule names: leaf colour and fullness follow it, evergreens keep theirs"),
    ("landscape", "landscape: {hedge} hedge length(s) along the plot boundary, {shrubs} shrub(s) in beds, {lawn} lawn tile(s), {paths} path tile(s) from the entrances to the street"),
    ("tree_setback", "{n} tree(s) stand at least {m} m from the building and {c} m clear of the paths"),
    ("tree_rows", "{n} tree(s) in rows along the street and the paths, {gap} m apart"),
    ("tree_mix", "{n} tree(s) drawn from the site tree mix; neighbours differ in species and turn, a species repeats only once the mix has used them all"),
    ("cost_items", "{failed} of {of} items miss it: costs {cost} in the {tier} tier (weight {weight})"),
    ("cost_measured", "measured {m}: costs {cost} in the {tier} tier (weight {weight})"),
    ("wet_stack", "storey {k} asked for no wet rooms; it takes the toilets of the stack of storey {a} on the same risers"),
    ("envelope", "envelope of {area} m2 inside the set-backs; ground cover {cover} of the plot"),
    ("coverage", "ground cover {cover}, at most {max}"),
    ("far", "floor-area ratio {far}, at most {max}"),
    ("build_to", "facades on the build-to lines of plot edges {edges}"),
    ("ground", "ground floor at {z} m ({by}): cut {cut} m3, fill {fill} m3"),
    ("step_stepped", "terrain falls {slope} per m: stepped plinth, up to {n} terrace(s)"),
    ("step_pad", "terrain falls {slope} per m: one pad"),
    ("retain", "{n} retaining wall(s) up to {h} m high where the pad leaves the terrain"),
    ("split", "split levels on {cuts} cut(s): the downhill {share} of the floor {drop} m lower, {flights} half-level flight(s), {platform} platform lift(s), {through} through car(s), {lifts} second lift room(s); cut and fill {moved} m3 against {pad} m3 on one pad"),
    ("one_level", "one level: {why}"),
    ("exits", "{n} exit(s): {grade} at grade, {ramp} by ramp, {steps} by steps"),
    ("fire_exits", "storey {k}: {people} occupant(s) need {n} exit(s) {w} m wide in all; it has {have}"),
    ("end_stairs", "{n} stairs: one at each corridor end where travel, exits, dead ends or stair width fell short with one"),
    ("fire_walls", "{n} fire wall piece(s) of {min} min: compartments of at most {area} m2"),
    ("stair_enclosure", "{n} stair enclosure wall(s) rated {min} min"),
    ("asm_wall", "exterior walls {t} m thick with {ins} m of insulation: U {u} W/m2K, at most {lim}"),
    ("asm_roof", "roof {t} m thick with {ins} m of insulation: U {u} W/m2K, at most {lim}"),
    ("asm_ground", "ground slab {t} m thick with {ins} m of insulation: U {u} W/m2K, at most {lim}"),
    ("asm_party", "party walls {t} m thick, {m} kg/m2: Rw {rw} dB by the mass law, at least {lim}"),
    ("asm_floor", "floors {t} m thick, {m} kg/m2: Rw {rw} dB by the mass law, at least {lim}"),
    ("typ_spaces", "typology {name}: {classes} space classes, {rooms} rooms, {area} m2 net over all storeys"),
    ("typ_air", "{classes} space class(es) ventilated at their own rates: {supply} m3/h supplied, {extract} m3/h extracted (rates x{k})"),
    ("typ_pressure", "{classes} space class(es) held at pressure: the lower of supply and extract falls {off} short of the higher"),
    ("typ_cooling", "{kw} kW of heat to remove; the largest load is {top}, {top_kw} kW"),
    ("typ_power", "{kw} kW connected load, {backup} kW of it on backup power"),
    ("typ_water", "{m3} m3 of water a day"),
    ("typ_zones", "{zones} zone(s), {wishes} of them wanting neighbours: their spaces carry the adjacency wishes into the plan"),
    ("typ_load", "floors carry up to {kpa} kPa, set by {name}"),
    ("typ_egress", "{persons} people in all spaces; at least {exits} exit(s); ground-floor stairs {have} m wide in all, {w} m asked per 100 people of the largest space class"),
    ("typ_below", "{n} storey(s) below ground ({names}): {rooms} room(s)"),
    ("typ_retain", "{n} retaining wall(s) {t} m thick, storeys {h} m high; the wall thickness is a rule"),
    ("typ_plant", "{n} plant brief(s) worked out from the loads, in the shape the plant module takes: {kinds}"),
    ("typ_refuge", "{n} refuge storey(s), one in every {every}; they hold the refuge area and its services only"),
    ("typ_mix", "{flats} flats on a typical storey: {one} one-bedroom, {two} two-bedroom, {three} three-bedroom"),
    ("core_pair", "{n} stairs in the core as a separated pair: a floor holds up to {people} people and needs two exits, and the plan has no corridor ends for stairs"),
    // reasons of proposed rule fixes (`RuleFix::keyed`)
    ("fix_conflict", "nearest value that resolves the conflict"),
    ("fix_route_width", "a common route wide enough to pass a wheelchair"),
    ("fix_turning", "room on the corridor to turn a wheelchair"),
    ("fix_door_width", "doors off a common route wide enough for a wheelchair"),
    ("fix_compartment", "a storey no larger than one fire compartment"),
    ("fix_stair_capacity", "stairs wide enough for the occupants of the storey they serve"),
    ("fix_daylight_depth", "rooms no deeper than daylight reaches from their windows"),
    ("fix_wet_stack", "carry the stack's toilets down to the storeys that have none"),
    ("fix_facade_rhythm", "punched windows alone give the facade no rhythm"),
    ("fix_canopy", "the entrance is not legible without a canopy"),
    ("fix_bare_site", "the building stands on a bare site"),
    ("fix_critic", "proposed by the design critic"),
    ("hvac_loads", "{n} rooms: heating {heat} kW at {te} C outside, cooling {cool} kW at {ts} C"),
    ("hvac_envelope", "envelope U-values: wall {w}, window {g}, roof {r} W/m2K"),
    ("hvac_oa", "outdoor air {q} m3/h: {pp} m3/h per person and {pa} m3/h per m2"),
    ("hvac_sys_named", "{sys} named by the brief; peak room cooling {peak} W/m2, ceiling void {void} m"),
    ("hvac_sys_res", "{sys}: dwellings are not cooled by the rules"),
    ("hvac_sys_nocool", "{sys}: peak room cooling {peak} W/m2 needs no cooling"),
    ("hvac_sys_radiant", "{sys}: ceiling void {void} m is too shallow for ducts, {peak} W/m2 fits chilled panels"),
    ("hvac_sys_fcu_void", "{sys}: ceiling void {void} m is too shallow for ducts, {peak} W/m2 is too much for panels"),
    ("hvac_sys_fcu_dense", "{sys}: peak room cooling {peak} W/m2 is too dense for all-air"),
    ("hvac_sys_vav", "{sys}: ceiling void {void} m takes the ducts, peak room cooling {peak} W/m2 fits all-air"),
    ("hvac_plant", "{plant}: heating {heat} kW, cooling {cool} kW"),
    ("hvac_supply", "supply air {q} m3/h at {dt} K below the room through {n} VAV boxes"),
    ("hvac_ducts", "{n} duct runs with {fit} fittings; {over} over their velocity cap"),
    ("hvac_fan", "supply fan {dp} Pa ({ext} Pa on the critical path to terminal {t}), {kw} kW motor; SFP {sfp} of at most {max}"),
    ("hvac_pipes", "{n} pipes at most {r} Pa/m: chilled water {chw} m3/h, heating water {hw} m3/h; index circuits {h1} and {h2} kPa"),
    ("hvac_beams", "ducts lowered {d} m to pass under the beams"),
    ("steel_skeleton", "tower lattice of {n} members, {pattern} bracing, panels {panel} m high"),
    ("steel_latticed", "members at least {len} m long built as lattices {ratio} of their length wide, at most {max} m"),
    ("steel_rolled", "members shorter than {len} m stay rolled sections"),
    ("steel_setback", "latticed members stop {k} x the widest member short of each node, room for the node plates"),
    ("steel_pattern", "{pattern} bracing in lattices of level {level} ({rule})"),
    ("steel_chord_laced", "chords built as laced pairs (level {level}), {ratio} of the member width"),
    ("steel_chord_rolled", "chords of level {level} lattices stay rolled angles: a laced chord would be narrower than {w} m"),
    ("steel_plate", "node plates at least {t} mm thick"),
    ("steel_splice", "rolled chords in pieces of at most {len} m joined by splice plates"),
    ("steel_rails", "platform railings {h} m high on {posts} posts"),
    ("steel_stairs", "stairs {w} m wide inside the legs, {treads} treads, a landing after every {flight} risers ({landings} landings)"),
    ("steel_lift", "lift guide rails {inset} m inside the inner chord of each leg, {n} guide points"),
    ("steel_cabin", "top cabin {size} m wide and {h} m high with a {mast} m antenna mast"),
    ("steel_piers", "masonry piers reaching {margin} m beyond the leg feet, {depth} m deep, widening by {batter} per metre"),
    ("steel_sized", "wind sizing: lattice pieces taken {steps} bands heavier, {over} members still over the allowable force"),
    ("steel_check", "preliminary check, not a design: worst member at {ratio} of its allowable force, {over} over, sway {sway} m"),
];

/// Named slots and their values, in template order.
pub type Args = Vec<(&'static str, String)>;

pub fn template(key: &str) -> Option<&'static str> {
    EN.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

/// `{name}` slots replaced by their values.
pub fn fill(template: &str, args: &[(&'static str, String)]) -> String {
    let mut s = template.to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// The English effect of a key; an unknown key is a bug caught by the tests.
pub fn effect(key: &str, args: &[(&'static str, String)]) -> String {
    fill(template(key).unwrap_or(key), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_every_slot_is_named_once() {
        let mut seen = std::collections::BTreeSet::new();
        for (k, t) in EN {
            assert!(seen.insert(*k), "duplicate {k}");
            assert!(k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{k}");
            assert_eq!(t.matches('{').count(), t.matches('}').count(), "{k}");
        }
        assert_eq!(effect("doors", &vec![("n", "2".into()), ("w", "0.9".into())]), "2 doors 0.9 m wide");
    }
}
