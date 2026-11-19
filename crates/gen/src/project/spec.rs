//! Reading a brief: every problem is listed with its path, the allowed range and a fix, so the
//! assistant can correct the spec in one more call.

use super::limits::*;
use super::programme::{default_programme, Programme};
use super::design::{Balconies, Design, FacadeSystem, Massing, Structure};
use super::footprint::RoofForm;
use super::presets;
use super::program::Program;
use super::{ApartmentReq, FoundationSpec, ProjectSpec, RoofKind, RoomKind, RoomReq, Typology};
use serde_json::{Map, Value as J};

#[derive(Clone, Debug, PartialEq)]
pub struct SpecIssue {
    pub path: String,
    pub msg: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpecError(pub Vec<SpecIssue>);

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the spec has {} problem(s):", self.0.len())?;
        for i in &self.0 {
            write!(f, " {}: {};", i.path, i.msg)?;
        }
        Ok(())
    }
}

impl std::error::Error for SpecError {}

const TOP: [&str; 16] = [
    "foundation",
    "typology",
    "name",
    "lang",
    "floors",
    "floor_height",
    "footprint",
    "band_depth",
    "corridor",
    "grid",
    "stair",
    "lift",
    "rooms",
    "ground_rooms",
    "apartments",
    "roof",
];
const TOP_MORE: [&str; 19] = [
    "storey_heights", "params", "origin", "design", "services", "furnish", "features", "basements", "rules", "preset", "program",
    "norms", "site", "sprinklers", "lifts", "storey_rooms", "mep_auto",
    // read by locks::regenerate; the build itself ignores them
    "locks",
    // rooms a regeneration kept in place; the free layout builds round them
    "pins",
];

/// The brief with every rule of `rules` that is an alias of a brief field written into that
/// field: the rule is the brief/chat layer and wins over the older field.
fn with_rule_fields(v: &J) -> J {
    let mut out = v.clone();
    let Some(r) = v["rules"].as_object() else {
        return out;
    };
    for (id, val) in r {
        if let Some((path, x)) = super::rules::alias_value(id, val) {
            super::rules::set_path(&mut out, path, x);
        }
    }
    out
}

/// RFC 7386 merge patch: `null` removes a key, objects merge, anything else replaces.
pub fn merge_patch(target: &mut J, patch: &J) {
    let J::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = J::Object(Map::new());
    }
    let t = target.as_object_mut().expect("an object");
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            merge_patch(t.entry(k.clone()).or_insert(J::Null), v);
        }
    }
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for j in 0..b.len() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != b[j]))
                .min(row[j] + 1)
                .min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn suggest(word: &str, known: &[&str]) -> String {
    match known.iter().min_by_key(|k| distance(word, k)) {
        Some(k) if distance(word, k) <= 3 => format!(" (did you mean '{k}'?)"),
        _ => format!(" (known: {})", known.join(", ")),
    }
}

struct Rd {
    issues: Vec<SpecIssue>,
    notes: Vec<String>,
    /// Room lines that named a data kind (foyer, atrium, ...), checked once the rules are final.
    asked: Vec<super::kinds::Asked>,
}

impl Rd {
    fn bad(&mut self, path: &str, msg: impl Into<String>) {
        self.issues.push(SpecIssue {
            path: path.to_string(),
            msg: msg.into(),
        });
    }

    fn keys(&mut self, path: &str, o: &Map<String, J>, known: &[&str]) {
        for k in o.keys() {
            if !known.contains(&k.as_str()) {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                self.bad(&p, format!("unknown field{}", suggest(k, known)));
            }
        }
    }

    /// A number of the spec in its range; `def` when it is missing or wrong.
    fn num_at(&mut self, path: &str, v: &J, def: f64, p: &ParamDef) -> f64 {
        if v.is_null() {
            return def;
        }
        match v.as_f64() {
            Some(x) if p.contains(x) => x,
            Some(x) => {
                self.bad(path, format!("{x} is outside {}", p.range()));
                def
            }
            None => {
                let msg = format!("must be a number in {} (metres or m2)", p.range());
                self.bad(path, msg);
                def
            }
        }
    }

    fn num(&mut self, v: &J, def: f64, p: &ParamDef) -> f64 {
        self.num_at(p.path, v, def, p)
    }

    fn opt_num(&mut self, v: &J, p: &ParamDef) -> Option<f64> {
        (!v.is_null()).then(|| self.num(v, p.min, p))
    }

    fn int(&mut self, path: &str, v: &J, def: u32, lo: u32, hi: u32) -> u32 {
        if v.is_null() {
            return def;
        }
        match v.as_u64() {
            Some(x) if (u64::from(lo)..=u64::from(hi)).contains(&x) => x as u32,
            _ => {
                self.bad(path, format!("must be a whole number in {lo}..{hi}"));
                def
            }
        }
    }

    fn text(&mut self, path: &str, v: &J, def: &str) -> String {
        match v {
            J::Null => def.to_string(),
            J::String(s) if !s.trim().is_empty() && s.chars().count() <= MAX_TEXT_CHARS => {
                s.trim().to_string()
            }
            _ => {
                self.bad(
                    path,
                    format!("must be text of 1 to {MAX_TEXT_CHARS} characters"),
                );
                def.to_string()
            }
        }
    }

    fn rooms(&mut self, path: &str, v: &J, ru: bool, pg: Option<&Programme>) -> Vec<RoomReq> {
        // `None`: rooms of an apartment, named generically
        let inside_apartment = pg.is_none();
        let Some(list) = v.as_array() else {
            self.bad(path, "must be a list of {name?, kind, area, count?}");
            return vec![];
        };
        let mut out = vec![];
        for (i, r) in list.iter().enumerate() {
            let p = format!("{path}[{i}]");
            let Some(o) = r.as_object() else {
                self.bad(&p, "must be an object {name?, kind, area, count?}");
                continue;
            };
            self.keys(&p, o, &["name", "kind", "area", "count", "tags"]);
            let mut data_kind = None;
            let kind = match o.get("kind").and_then(J::as_str) {
                Some(k) => match super::kinds::parse(k) {
                    Some((k, d)) => {
                        data_kind = d;
                        k
                    }
                    None => {
                        let hint = suggest(k, &super::kinds::words());
                        self.bad(&format!("{p}.kind"), format!("unknown kind '{k}'{hint}"));
                        continue;
                    }
                },
                None => {
                    let known = super::kinds::words().join(", ");
                    self.bad(&format!("{p}.kind"), format!("required, one of {known}"));
                    continue;
                }
            };
            let house = pg.is_some_and(|g| g.typology == Typology::House);
            if kind == RoomKind::Hall && !inside_apartment && !house {
                self.bad(
                    &format!("{p}.kind"),
                    "a hall is only asked in a house or inside an apartment; the corridor is \
                     built",
                );
                continue;
            }
            // index the value, not the map: a missing key reads as null
            let area = self.num_at(&format!("{p}.area"), &r["area"], 0.0, &ROOM_AREA);
            if r["area"].is_null() {
                let msg = format!("required, m2 in {}", ROOM_AREA.range());
                self.bad(&format!("{p}.area"), msg);
            }
            let count = self.int(&format!("{p}.count"), &r["count"], 1, 1, MAX_ROOM_COUNT);
            let def = match (data_kind, pg) {
                (Some(d), _) => if ru { &d.name.1 } else { &d.name.0 }.clone(),
                (None, Some(g)) => g.room_name(kind, ru),
                (None, None) => super::names::kind_name(kind, ru).to_string(),
            };
            if let Some(def) = data_kind {
                self.asked.push(super::kinds::Asked { path: p.clone(), def, area, apartment: inside_apartment });
            }
            let name = self.text(&format!("{p}.name"), &r["name"], &def);
            let mut tags = super::RoomTags::default();
            match &r["tags"] {
                J::Null => {}
                J::Array(a) => {
                    for t in a {
                        match t.as_str().and_then(super::RoomTags::parse) {
                            Some(x) => tags = tags.with(x),
                            None => self.bad(&format!("{p}.tags"), format!("each one of {}", super::RoomTags::IDS.join(", "))),
                        }
                    }
                }
                _ => self.bad(&format!("{p}.tags"), format!("a list of {}", super::RoomTags::IDS.join(", "))),
            }
            out.push(RoomReq {
                name,
                kind,
                area,
                count,
                tags,
            });
        }
        out
    }
}

struct Defaults {
    floors: u32,
    height: f64,
    band: f64,
    corridor: f64,
    stair: f64,
    fire: &'static str,
}

fn defaults(t: Typology) -> Defaults {
    match t {
        Typology::House => Defaults {
            floors: 2,
            height: 3.0,
            band: 4.8,
            corridor: 1.4,
            stair: 1.0,
            fire: "REI 30",
        },
        Typology::ApartmentSection => Defaults {
            floors: 9,
            height: 3.0,
            band: 6.4,
            corridor: 1.6,
            stair: 1.2,
            fire: "REI 90",
        },
        Typology::Office => Defaults {
            floors: 4,
            height: 3.6,
            band: 6.0,
            corridor: 1.8,
            stair: 1.35,
            fire: "REI 60",
        },
        Typology::School => Defaults {
            floors: 3,
            height: 3.6,
            band: 7.2,
            corridor: 2.6,
            stair: 1.5,
            fire: "REI 60",
        },
    }
}

fn r(kind: RoomKind, area: f64, count: u32, ru: bool) -> RoomReq {
    RoomReq {
        name: super::names::kind_name(kind, ru).to_string(),
        kind,
        area,
        count,
        tags: Default::default(),
    }
}

fn default_apartments(ru: bool) -> Vec<ApartmentReq> {
    use RoomKind::*;
    let one = vec![
        r(Living, 19.0, 1, ru),
        r(Kitchen, 10.0, 1, ru),
        r(Bath, 4.0, 1, ru),
    ];
    let two = vec![
        r(Living, 18.0, 1, ru),
        r(Bedroom, 13.0, 1, ru),
        r(Kitchen, 10.0, 1, ru),
        r(Bath, 4.4, 1, ru),
        r(Wc, 1.8, 1, ru),
    ];
    vec![
        ApartmentReq {
            name: "1".to_string(),
            count: 2,
            rooms: one,
        },
        ApartmentReq {
            name: "2".to_string(),
            count: 2,
            rooms: two,
        },
    ]
}

fn read_apartments(rd: &mut Rd, v: &J, ru: bool, cap: u32) -> Vec<ApartmentReq> {
    let Some(list) = v.as_array() else {
        rd.bad("apartments", "must be a list of {name?, count?, rooms}");
        return vec![];
    };
    let mut out = vec![];
    for (i, a) in list.iter().enumerate() {
        let p = format!("apartments[{i}]");
        let Some(o) = a.as_object() else {
            rd.bad(&p, "must be an object {name?, count?, rooms}");
            continue;
        };
        rd.keys(&p, o, &["name", "count", "rooms"]);
        let name = rd.text(&format!("{p}.name"), &a["name"], &(i + 1).to_string());
        let count = rd.int(
            &format!("{p}.count"),
            &a["count"],
            1,
            1,
            cap,
        );
        let rooms = rd.rooms(&format!("{p}.rooms"), &a["rooms"], ru, None);
        if !rooms.iter().any(|x| x.kind.habitable() && !x.kind.inner()) {
            rd.bad(
                &format!("{p}.rooms"),
                "needs at least one living room or bedroom",
            );
        }
        if rooms.iter().filter(|x| x.kind == RoomKind::Hall).count() > 1 {
            rd.bad(
                &format!("{p}.rooms"),
                "at most one hall: the builder sizes it",
            );
        }
        // the apartment builder places one room per entry: a line with count 2 becomes two
        let rooms = rooms
            .into_iter()
            .flat_map(|r| {
                let n = r.count as usize;
                std::iter::repeat(RoomReq { count: 1, ..r }).take(n)
            })
            .collect();
        out.push(ApartmentReq { name, count, rooms });
    }
    out
}

/// Reads and checks a brief; all problems are reported at once.
pub fn parse_spec(v: &J) -> Result<ProjectSpec, SpecError> {
    parse_spec_with(v, None)
}

/// As [`parse_spec`], with a programme template in place of the typology's built-in one.
pub fn parse_spec_with(v: &J, pg: Option<&Programme>) -> Result<ProjectSpec, SpecError> {
    let mut rd = Rd {
        issues: vec![],
        notes: vec![],
        asked: vec![],
    };
    let mut ruled = with_rule_fields(v);
    // climate words of the site pick the tree mix unless the brief's rules name one
    // and its species and hedge words (English, Russian or German) the species and the hedge
    let brief = format!("{} {}", v["site"]["climate"].as_str().unwrap_or(""), v["site"]["brief"].as_str().unwrap_or(""));
    if !brief.trim().is_empty() {
        let mut want: Vec<(&str, J)> = vec![];
        let edge = super::trees::hedge_edge(&brief);
        // the compass word of a hedge side is no climate word
        let brief = edge.as_ref().map_or(brief, |e| e.1.clone());
        if let Some((e, _)) = &edge {
            want.push(("site.hedge_edge", J::String((*e).into())));
        }
        if let Some(mix) = super::trees::mix_for_climate(&brief) {
            want.push(("site.tree_mix", J::String(mix)));
        }
        let mask = super::trees::species_mask(&brief);
        if mask != 0 {
            want.push(("site.tree_species", J::from(mask)));
        }
        if super::trees::wants_hedge(&brief) {
            want.push(("site.hedge", J::String("clipped".into())));
        }
        // a rules value that is not an object is left for the parser to refuse
        if ruled["rules"].is_null() {
            ruled["rules"] = J::Object(Map::new());
        }
        if ruled["rules"].is_object() {
            for (id, x) in want {
                if ruled["rules"].get(id).is_none() {
                    ruled["rules"][id] = x;
                }
            }
        }
    }
    let v = &ruled;
    // a museum brief without rooms or outline: the programme and plan by rule
    let museum = super::typo_program::fill(v);
    if let Some((_, n)) = &museum {
        rd.notes.extend(n.iter().cloned());
    }
    let v = museum.as_ref().map_or(v, |m| &m.0);
    let Some(o) = v.as_object() else {
        return Err(SpecError(vec![SpecIssue {
            path: String::new(),
            msg: "the spec must be a JSON object".to_string(),
        }]));
    };
    let mut rules = super::rules::RuleSet::new();
    if !v["rules"].is_null() {
        if let Err(errs) = rules.apply_patch(super::rules::Layer::Brief, &v["rules"]) {
            for e in errs {
                rd.bad(&format!("rules.{}", e.rule), e.msg);
            }
        }
    }
    // the roof's overhang is a rule the brief may give with the roof
    if let Some(x) = v["roof"].get("overhang").filter(|x| !x.is_null()) {
        let patch = serde_json::json!({ "massing.roof_overhang": x });
        if let Err(errs) = rules.apply_patch(super::rules::Layer::Brief, &patch) {
            for e in errs {
                rd.bad("roof.overhang", e.msg);
            }
        }
    }
    // the country pack's numbers: {rule: {value, cite}}, below the preset and the brief
    match &v["norms"] {
        J::Null => {}
        J::Object(n) => {
            for (id, e) in n {
                let cite = e["cite"].as_str().unwrap_or_default();
                // {value: null, unlimited: true}: the norm sets no limit, the check is off
                let set = if e["value"].is_null() && e["unlimited"] == J::Bool(true) {
                    rules.set_norm_unlimited(id, cite)
                } else {
                    rules.set_norm(id, &e["value"], cite)
                };
                if let Err(err) = set {
                    rd.bad(&format!("norms.{id}"), err.msg);
                }
            }
        }
        _ => rd.bad("norms", "an object of rule id to {value, cite} or {value: null, unlimited: true, cite}"),
    }
    let known: Vec<&str> = TOP.iter().chain(TOP_MORE.iter()).copied().collect();
    rd.keys("", o, &known);
    // a preset (hotel, clinic...) is its base typology with its own programme and rule values
    let tpl = presets::of_brief(v).unwrap_or_else(|e| {
        rd.bad("preset", e);
        None
    });
    if let Some(t) = &tpl {
        // checked when the preset was read; the brief's own rules stay above
        let _ = rules.apply_patch(super::rules::Layer::Preset, &t.rules);
    }
    let expanded;
    let v = match &tpl {
        Some(t) => {
            expanded = t.expand(v);
            &expanded
        }
        None => v,
    };
    let Some(typology) = read_typology(&mut rd, v) else {
        return Err(SpecError(rd.issues));
    };
    let d = defaults(typology);
    let asked = match &v["program"] {
        J::Null => None,
        p => Program::from_json(p)
            .map_err(|errs| {
                for e in errs {
                    let (at, msg) = e.split_once(": ").unwrap_or((e.as_str(), "invalid"));
                    rd.bad(&format!("program.{at}"), msg);
                }
            })
            .ok(),
    };
    let own;
    let base_pg = match tpl.as_deref().map(|t| t.programme()).or(pg) {
        Some(g) if g.typology == typology => g,
        _ => {
            own = default_programme(typology);
            &own
        }
    };
    let over;
    let pg = match &asked {
        Some(p) => {
            over = p.over(base_pg);
            &over
        }
        None => base_pg,
    };
    // counts that follow a rule (a unit mix) are read under this project's rules
    let ruled;
    let pg = match tpl.as_deref().and_then(|t| t.model.as_deref()) {
        Some(m) if asked.is_none() && m.spaces.iter().any(|s| s.count_num.is_some()) => {
            let mut g = m.program_with(Some(&rules)).over(&default_programme(typology));
            if let Some(sp) = &m.spare {
                g.spare = sp.clone();
            }
            ruled = g;
            &ruled
        }
        _ => pg,
    };
    let program = asked.unwrap_or_else(|| Program::from_programme(pg));
    // any UI locale as a BCP-47 tag; an unknown one falls back to en
    let lang = match &v["lang"] {
        J::Null => "en",
        J::String(s) => super::names::locale(s),
        _ => {
            rd.bad("lang", "a locale code such as en, ru, de, ja, pt-BR");
            "en"
        }
    };
    let ru = lang == "ru";
    // a one-storey building may stand taller than a floor: its tall space (nave, hall, bay)
    let tall = ParamDef { max: rules.get("typology.tall_space_max").max(FLOOR_HEIGHT.max), ..*FLOOR_HEIGHT };
    let fh_def = if v["floors"].as_u64() == Some(1) { tall } else { *FLOOR_HEIGHT };
    let floor_height = rd.num(&v["floor_height"], d.height, &fh_def);
    let floors_cap = floors_allowed(&rules, typology, floor_height);
    let floors = rd.int("floors", &v["floors"], d.floors.min(floors_cap), 1, floors_cap);
    let storey_heights = read_storey_heights(&mut rd, &v["storey_heights"], floors, floor_height, rules.get("typology.tall_space_max"));
    if !storey_heights.is_empty() {
        let top: f64 = (0..floors as usize).map(|k| storey_heights.get(k).copied().unwrap_or(floor_height)).sum();
        if let Some(why) = height_refusal(&rules, top) {
            rd.bad("storey_heights", why);
        }
    }
    let fp = read_footprint(&mut rd, v, &d);
    let grid = rd.num(&v["grid"], GRID.default.unwrap_or(GRID.max), &GRID);
    let (mut stair_width, mut max_riser) = read_stair(&mut rd, v, &d);
    // a country riser limit lowers the default; a riser the brief names is its own choice
    if v["stair"]["max_riser"].is_null() && rules.source("core.max_riser") == Some(super::rules::Layer::Norm) {
        max_riser = max_riser.min(rules.get("core.max_riser"));
    }
    // a country flight width raises the default the same way; a width the brief names stays
    let width_rule = stair_width_rule(typology);
    if v["stair"]["width"].is_null() && rules.source(width_rule) == Some(super::rules::Layer::Norm) {
        stair_width = stair_width.max(rules.get(width_rule)).min(STAIR_WIDTH.max);
    }
    let lift = match &v["lift"] {
        J::Null => {
            floors > 1
                && typology != Typology::House
                && (floors >= 5 || typology != Typology::ApartmentSection)
        }
        J::Bool(b) => *b,
        _ => {
            rd.bad("lift", "true or false");
            false
        }
    };
    // the cars the brief names; the traffic rule sizes them otherwise
    let lifts = match &v["lifts"] {
        J::Null => None,
        _ => {
            let max = rules.get("core.lift_bank_max") as u32;
            Some(rd.int("lifts", &v["lifts"], 1, 1, max))
        }
    };
    let lift = lift || lifts.is_some();
    let mut sprinklers = match &v["sprinklers"] {
        J::Null => None,
        J::Bool(b) => Some(*b),
        _ => {
            rd.bad("sprinklers", "true or false; leave it out for the height rule");
            None
        }
    };
    let mep_auto = match &v["mep_auto"] {
        J::Null => true,
        J::Bool(b) => *b,
        _ => {
            rd.bad("mep_auto", "true or false");
            true
        }
    };
    let cap = apartments_cap(&rules, fp.width);
    let (rooms, ground_rooms, mut apartments) = read_program(&mut rd, v, pg, floors, ru, cap);
    let storey_rooms = read_storey_rooms(&mut rd, v, pg, floors, ru);
    // the apartments a typology model states, in the mix its rules give, unless the brief names its own
    if let Some(m) = tpl.as_deref().and_then(|t| t.model.as_deref()) {
        if pg.typology == Typology::ApartmentSection && v["apartments"].is_null() && !m.units.is_empty() {
            apartments = m.apartments(ru, &rules);
            let n: u32 = apartments.iter().map(|a| a.count).sum();
            if n == 0 || n > cap {
                let msg = format!(
                    "the unit mix gives {n} apartments on one floor; between 1 and {cap} for {} m of facade: steer typology.mix.* or widen the footprint",
                    fp.width.map_or("the default".to_string(), |w| format!("{w}"))
                );
                rd.bad("apartments", msg);
            }
        }
    }
    let multi = floors > 1 && typology != Typology::House;
    for (path, msg) in super::kinds::check(&rd.asked, &rules, multi, &mut rd.notes) {
        rd.bad(&path, msg);
    }
    let (roof, roof_slope, roof_form) = read_roof(&mut rd, v, typology);
    let [fire_rating, floor_finish, wall_finish, ceiling_finish] = read_params(&mut rd, v, d.fire);
    let origin = read_origin(&mut rd, v);
    let mut design = read_design(&mut rd, v, floors);
    let foundation = read_foundation(&mut rd, v);
    let services = read_services(&mut rd, v);
    let name = rd.text("name", &v["name"], default_name(typology, ru));
    let furnish = read_furnish(&mut rd, v);
    let features = read_features(&mut rd, v);
    // a brief that asks for sprinklers gets them, whatever the height rule says
    if sprinklers.is_none() && features.iter().any(|f| super::brief::asks_sprinklers(f)) {
        sprinklers = Some(true);
    }
    // a brief that asks for a curtain wall gets one unless it names another facade itself
    if rules.get("facade.curtain_from_brief") > 0.5
        && v["design"]["facade"].is_null()
        && features.iter().any(|f| super::brief::asks_curtain_wall(f))
        && design.facade != super::design::FacadeSystem::Curtain
    {
        design.facade = super::design::FacadeSystem::Curtain;
        rd.notes.push("features: a curtain wall facade asked, so design.facade is curtain".into());
    }
    // a brief that asks for timber gets a timber structure unless it names another itself
    if rules.get("structure.timber_from_brief") > 0.5 && v["design"]["structure"].is_null() {
        if let Some(st) = features.iter().filter_map(|f| super::brief::asks_timber(f)).max_by_key(|st| st.is_frame()) {
            design.structure = st;
            rd.notes.push(format!("features: timber asked, so design.structure is {}", Structure::NAMES[Structure::ALL.iter().position(|x| *x == st).unwrap_or(0)]));
        }
    }

    // a free-form brief on a plain bar gets the double-curved shell unless it names the shell or a
    // bow itself: the shell is the only double curvature the generator builds
    // (a polygon footprint is its own massing, and a bow set beside a free-form word gives way:
    // a live run sent both and got a bowed bar)
    if v["design"]["shell"].is_null()
        && (design.massing == super::Massing::Bar || super::footprint::asks_plate(&v["footprint"]))
        && design.step_at(floors).is_none()
        && (features.iter().any(|f| super::brief::asks_shell(f)) || super::brief::text_asks_shell(&super::brief::request_text()))
    {
        design.curve = 0.0;
        design.curve_north = 0.0;
        design.shell = true;
        rd.notes.push("features: a free-form envelope asked, so design.shell is on".into());
    }
    let basements = rd.int("basements", &v["basements"], 0, 0, max_basements());
    let mut plate = if super::footprint::asks_plate(&v["footprint"]) {
        let mut found = vec![];
        let p = super::footprint::read(v, floors, &mut found);
        for (path, msg) in found {
            rd.bad(&path, msg);
        }
        if design.curve > 0.0 || design.curve_north > 0.0 {
            rd.bad("design.curve", "a footprint polygon carries its own curves: use bulges or footprint.shape");
        }
        if design.massing != super::Massing::Bar || design.step_at(floors).is_some() {
            rd.bad("design.massing", "a footprint polygon or shape is the massing: drop design.massing, upper_from, setback and cantilever, use design.levels");
        }
        p
    } else {
        for key in ["twist", "taper", "levels"] {
            if !v["design"][key].is_null() {
                rd.bad(&format!("design.{key}"), "needs footprint.polygon, footprint.shape or footprint.plot");
            }
        }
        None
    };
    let pins = match &v["pins"] {
        J::Null => None,
        p => super::Pins::from_json(p).map_err(|e| rd.bad("pins", e)).ok(),
    };
    let mut found = vec![];
    let site = super::site::read(v, &rules, &mut found);
    for (path, msg) in found {
        rd.bad(&path, msg);
    }
    if let Some(pl) = plate.as_mut().filter(|pl| pl.source == "plot") {
        let turned = v["footprint"]["rotation"].as_f64().is_some_and(|r| r != 0.0);
        match super::site::shape_plate(&site, pl, floors, turned) {
            Ok(n) => rd.notes.extend(n),
            Err(e) => rd.bad("site", e),
        }
    }
    if !rd.issues.is_empty() {
        return Err(SpecError(rd.issues));
    }
    let mut notes = rd.notes;
    let template = tpl.as_deref().filter(|t| !Typology::NAMES.contains(&t.name.as_str()));
    if let Some(t) = template.filter(|t| !t.note().is_empty()) {
        notes.insert(0, t.note());
    }
    let room_tags = super::RoomTagTable(
        rooms
            .iter()
            .chain(&ground_rooms)
            .chain(apartments.iter().flat_map(|a| &a.rooms))
            .filter(|r| !r.tags.is_empty())
            .map(|r| (r.kind, r.name.clone(), r.tags))
            .chain(template.and_then(|t| t.model.as_deref()).map(|m| m.room_tags()).unwrap_or_default())
            .collect(),
    );
    Ok(ProjectSpec {
        typology,
        name,
        ru,
        lang,
        room_tags,
        floors,
        floor_height,
        storey_heights,
        width: fp.width,
        depth: fp.depth,
        band_depth: fp.band_depth,
        hall_depth: fp.hall_depth,
        corridor: fp.corridor,
        grid,
        stair_width,
        max_riser,
        lift,
        lifts,
        sprinklers,
        mep_auto,
        rooms,
        ground_rooms,
        storey_rooms,
        apartments,
        roof,
        roof_slope,
        fire_rating,
        floor_finish,
        wall_finish,
        ceiling_finish,
        origin,
        foundation,
        spare: pg.spare.clone(),
        furnish,
        core_rest: (pg.core_rest.0, (if ru { &pg.core_rest.2 } else { &pg.core_rest.1 }).clone()),
        design,
        services,
        template: template.map(|t| t.name.clone()),
        features,
        basements,
        notes,
        plate,
        roof_form,
        rules,
        program,
        site,
        pins,
        model: tpl.as_deref().and_then(|t| t.model.clone()),
    })
}

/// `"raft"`, `"strip"` or `{kind, thickness | width, depth}`; absent or null builds none.
fn read_foundation(rd: &mut Rd, v: &J) -> Option<FoundationSpec> {
    let f = &v["foundation"];
    let (kind, obj) = match f {
        J::Null => return None,
        J::String(s) => (s.as_str(), None),
        J::Object(o) => {
            rd.keys("foundation", o, &["kind", "thickness", "width", "depth"]);
            (f["kind"].as_str().unwrap_or_default(), Some(f))
        }
        _ => {
            rd.bad("foundation", "must be raft, strip or {kind, ...}");
            return None;
        }
    };
    let get = |k: &str| obj.map_or(J::Null, |o| o[k].clone());
    match kind {
        "raft" => Some(FoundationSpec::Raft { thickness: rd.num(&get("thickness"), RAFT_THICKNESS.default.unwrap_or(RAFT_THICKNESS.min), &RAFT_THICKNESS) }),
        "strip" => Some(FoundationSpec::Strip {
            width: rd.num(&get("width"), STRIP_WIDTH.default.unwrap_or(STRIP_WIDTH.min), &STRIP_WIDTH),
            depth: rd.num(&get("depth"), STRIP_DEPTH.default.unwrap_or(STRIP_DEPTH.min), &STRIP_DEPTH),
        }),
        k => {
            rd.bad("foundation", format!("unknown '{k}', one of raft, strip"));
            None
        }
    }
}

/// Every typology a brief may name: the four the builder lays out, then the presets.
pub fn typology_names() -> Vec<&'static str> {
    Typology::NAMES.into_iter().chain(presets::names()).collect()
}

/// The brief's patch of the fit-out rules, kept only when the patched rules hold together.
fn read_furnish(rd: &mut Rd, v: &J) -> Option<J> {
    let p = v.get("furnish").filter(|p| !p.is_null())?;
    if !p.is_object() {
        rd.bad("furnish", "an object patching the fit-out rules");
        return None;
    }
    match super::fit_rules::rules_for(Some(p)) {
        Ok(_) => Some(p.clone()),
        Err(errs) => {
            for e in errs {
                let (path, msg) = e.split_once(": ").unwrap_or(("furnish", e.as_str()));
                rd.bad(path, msg);
            }
            None
        }
    }
}

fn read_typology(rd: &mut Rd, v: &J) -> Option<Typology> {
    let Some(t) = v["typology"].as_str() else {
        let names = typology_names().join(", ");
        rd.bad("typology", format!("required, one of {names}"));
        return None;
    };
    let found = Typology::ALL.into_iter().find(|x| x.name() == t);
    if found.is_none() {
        let hint = suggest(t, &typology_names());
        rd.bad("typology", format!("unknown '{t}'{hint}"));
    }
    found
}

const MAX_FEATURES: usize = 24;
const MAX_FEATURE_CHARS: usize = 40;

/// Keywords of what the brief asked for (l_shape, balcony, atrium...): any word is taken, the
/// brief check names the ones the builder does not make.
fn read_features(rd: &mut Rd, v: &J) -> Vec<String> {
    let f = &v["features"];
    if f.is_null() {
        return vec![];
    }
    let Some(list) = f.as_array().filter(|a| a.len() <= MAX_FEATURES) else {
        rd.bad("features", format!("must be a list of at most {MAX_FEATURES} keywords"));
        return vec![];
    };
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for (i, x) in list.iter().enumerate() {
        match x.as_str().map(str::trim) {
            Some(s) if !s.is_empty() && s.chars().count() <= MAX_FEATURE_CHARS => {
                let k = super::feature_words::canon(s);
                if !out.contains(&k) {
                    out.push(k);
                }
            }
            _ => rd.bad(
                &format!("features[{i}]"),
                format!("must be a keyword of 1 to {MAX_FEATURE_CHARS} characters"),
            ),
        }
    }
    out
}

struct Footprint {
    width: Option<f64>,
    depth: Option<f64>,
    band_depth: f64,
    corridor: f64,
    hall_depth: Option<f64>,
}

/// Deepest open bay behind the room band of a deep plan.
const HALL_MAX: f64 = 120.0;

/// The outline, the room bands and the corridor. A fixed depth sets the bands; its bounds
/// follow the corridor actually asked for, so the message names the exact range.
/// Heights of the storeys from the ground, `null` for the default one: a tall level may stand
/// up to `typology.tall_space_max`, the others keep the floor height range.
fn read_storey_heights(rd: &mut Rd, v: &J, floors: u32, floor_height: f64, tall_max: f64) -> Vec<f64> {
    let Some(list) = v.as_array() else {
        if !v.is_null() {
            rd.bad("storey_heights", "must be a list of heights in metres from the ground floor, null for the default");
        }
        return vec![];
    };
    if list.len() > floors as usize {
        rd.bad("storey_heights", format!("{} heights for {floors} storeys", list.len()));
        return vec![];
    }
    let def = ParamDef { max: tall_max.max(FLOOR_HEIGHT.max), ..*FLOOR_HEIGHT };
    let mut out: Vec<f64> = list
        .iter()
        .enumerate()
        .map(|(k, x)| rd.num_at(&format!("storey_heights[{k}]"), x, floor_height, &def))
        .collect();
    out.resize(floors as usize, floor_height);
    out
}

fn read_footprint(rd: &mut Rd, v: &J, d: &Defaults) -> Footprint {
    let fp = &v["footprint"];
    if let Some(f) = fp.as_object() {
        let mut known = vec!["width", "depth"];
        known.extend(super::footprint::KEYS);
        rd.keys("footprint", f, &known);
    } else if !fp.is_null() {
        rd.bad("footprint", "must be {width?, depth?} in metres, or {polygon | shape | plot, ...}");
    }
    let width = rd.opt_num(&fp["width"], &WIDTH);
    // a data hall is lit by its lamps, not its facade: only its brief may go past daylight depth
    let band_range = if super::dc::brief_has_hall(v) {
        ParamDef { max: BAND_DEPTH.max.max(super::rules::rule("dc.hall_band")), ..*BAND_DEPTH }
    } else {
        *BAND_DEPTH
    };
    let mut band_depth = rd.num(&v["band_depth"], d.band, &band_range);
    let corridor = rd.num(&v["corridor"], d.corridor, &CORRIDOR);
    let mut hall_depth = None;
    let depth = match &fp["depth"] {
        J::Null => None,
        x => {
            let dp = x.as_f64().filter(|x| x.is_finite());
            if dp.is_none() {
                rd.bad(
                    DEPTH.path,
                    format!("must be a number in {} m", DEPTH.range()),
                );
            }
            dp
        }
    };
    if let Some(dp) = depth {
        // the outline is the firmer wish: the band depth follows it, and the reply says so
        if !v["band_depth"].is_null() {
            rd.notes.push(format!(
                "footprint.depth {dp} m and band_depth were both given: the footprint wins, \
                 band_depth is derived from it"
            ));
            band_depth = d.band;
        }
        // two bands of rooms lit from their facades and the corridor between them
        let (lo, hi) = (
            2.0 * BAND_DEPTH.min + corridor,
            2.0 * BAND_DEPTH.max + corridor,
        );
        // deeper than two bands: one band of rooms on the south facade, an open bay behind
        let deep = dp - band_depth - corridor;
        if dp < lo - 1e-9 || deep > HALL_MAX + 1e-9 {
            let msg = format!(
                "{dp} m is out of range: between {lo:.1} m and {:.1} m with a {corridor} m \
                 corridor (room bands {}-{} m deep; past {hi:.1} m the north band is an open \
                 bay of up to {HALL_MAX} m)",
                band_depth + corridor + HALL_MAX,
                BAND_DEPTH.min,
                BAND_DEPTH.max
            );
            rd.bad(DEPTH.path, msg);
        } else if dp > hi + 1e-9 {
            hall_depth = Some(deep);
        } else {
            band_depth = (dp - corridor) / 2.0;
        }
    }
    Footprint {
        width,
        depth,
        band_depth,
        corridor,
        hall_depth,
    }
}

/// The rule holding the norm's least flight width: houses have their own, lower one.
pub fn stair_width_rule(t: Typology) -> &'static str {
    if t == Typology::House {
        "core.stair_width_house"
    } else {
        "core.stair_width"
    }
}

fn read_stair(rd: &mut Rd, v: &J, d: &Defaults) -> (f64, f64) {
    let st = &v["stair"];
    if let Some(s) = st.as_object() {
        rd.keys("stair", s, &["width", "max_riser"]);
    } else if !st.is_null() {
        rd.bad("stair", "must be {width?, max_riser?}");
    }
    let width = rd.num(&st["width"], d.stair, &STAIR_WIDTH);
    let riser = MAX_RISER.default.unwrap_or(MAX_RISER.max);
    (width, rd.num(&st["max_riser"], riser, &MAX_RISER))
}

/// Rooms of a typical floor, of the ground floor, and apartments: each typology takes
/// either rooms or apartments.
/// `storey_rooms`: {"<storey above ground, 0 the ground floor>": [rooms]}, rooms added to that
/// storey only, on top of what the programme gives it.
fn read_storey_rooms(rd: &mut Rd, v: &J, pg: &Programme, floors: u32, ru: bool) -> std::collections::BTreeMap<usize, Vec<RoomReq>> {
    let mut out = std::collections::BTreeMap::new();
    let sv = &v["storey_rooms"];
    if sv.is_null() {
        return out;
    }
    let Some(o) = sv.as_object() else {
        rd.bad("storey_rooms", "must be an object {\"<storey>\": [{kind, area, name?, count?}]}");
        return out;
    };
    for (key, list) in o {
        let p = format!("storey_rooms.{key}");
        match key.parse::<usize>() {
            Ok(k) if k < floors as usize => {
                let rooms = rd.rooms(&p, list, ru, Some(pg));
                let n: u32 = rooms.iter().map(|x| x.count).sum();
                if n > MAX_ROOMS_PER_FLOOR {
                    rd.bad(&p, format!("{n} rooms on one floor; at most {MAX_ROOMS_PER_FLOOR}"));
                }
                out.insert(k, rooms);
            }
            _ => rd.bad(&p, format!("a storey above ground, 0..{}", floors.max(1) - 1)),
        }
    }
    out
}

fn read_program(
    rd: &mut Rd,
    v: &J,
    pg: &Programme,
    floors: u32,
    ru: bool,
    cap: u32,
) -> (Vec<RoomReq>, Vec<RoomReq>, Vec<ApartmentReq>) {
    let typology = pg.typology;
    let apt = typology == Typology::ApartmentSection;
    let single = floors == 1;
    let rooms = match (&v["rooms"], apt) {
        // one storey: the rooms asked are the ground floor, or the whole single programme
        (J::Null, _) if single && !v["ground_rooms"].is_null() => vec![],
        (J::Null, _) if single => pg.reqs(&pg.single, ru),
        (J::Null, _) => pg.reqs(&pg.typical, ru),
        (_, true) => {
            rd.bad(
                "rooms",
                "an apartment section takes apartments[].rooms instead",
            );
            vec![]
        }
        (x, false) => rd.rooms("rooms", x, ru, Some(pg)),
    };
    let ground_rooms = match (&v["ground_rooms"], apt) {
        (J::Null, true) => vec![],
        (J::Null, false) if single => vec![],
        // rooms asked without ground rooms hold on every floor (a house shares them out): a
        // parking deck or warehouse gets no default office lobby below
        (J::Null, false) if !v["rooms"].is_null() => vec![],
        (J::Null, false) => pg.reqs(&pg.ground, ru),
        (_, true) => {
            let msg = "an apartment section repeats its apartments on every floor";
            rd.bad("ground_rooms", msg);
            vec![]
        }
        (x, false) => rd.rooms("ground_rooms", x, ru, Some(pg)),
    };
    if single && !apt && !v["rooms"].is_null() && !v["ground_rooms"].is_null() {
        rd.bad("ground_rooms", "one storey takes rooms or ground_rooms, not both");
    }
    // a single storey builds one programme: the ground rooms when given, else the rooms
    let (rooms, ground_rooms) = if single && !ground_rooms.is_empty() {
        (ground_rooms, vec![])
    } else {
        (rooms, ground_rooms)
    };
    let apartments = match (&v["apartments"], apt) {
        (J::Null, true) => default_apartments(ru),
        (J::Null, false) => vec![],
        (x, true) => read_apartments(rd, x, ru, cap),
        (_, false) => {
            rd.bad(
                "apartments",
                "only for typology apartment_section; use rooms",
            );
            vec![]
        }
    };
    for (path, list) in [("rooms", &rooms), ("ground_rooms", &ground_rooms)] {
        let n: u32 = list.iter().map(|x| x.count).sum();
        if n > MAX_ROOMS_PER_FLOOR {
            let msg = format!("{n} rooms on one floor; at most {MAX_ROOMS_PER_FLOOR}");
            rd.bad(path, msg);
        }
        if !apt && v[path].as_array().is_some_and(Vec::is_empty) {
            rd.bad(path, "needs at least one room");
        }
    }
    let n_apts: u32 = apartments.iter().map(|a| a.count).sum();
    if n_apts > cap {
        let msg = format!("{n_apts} apartments on one floor; at most {cap}{}", cap_reason(v));
        rd.bad("apartments", msg);
    }
    (rooms, ground_rooms, apartments)
}

/// Apartments one floor holds: with the footprint width fixed, the facade of both sides of
/// the corridor divided by the facade one apartment needs; else the rule's own count. A count
/// the brief or a pack gave for `room.apartments_max` also limits the derived one.
fn apartments_cap(rules: &super::rules::RuleSet, width: Option<f64>) -> u32 {
    let fixed = rules.get("room.apartments_max") as u32;
    let Some(w) = width else { return fixed };
    let sides = 2.0;
    let each_side = (w / rules.get("room.apartment_frontage")).floor().max(1.0);
    let derived = (sides * each_side) as u32;
    let top = super::rules::def("room.apartments_max").map_or(derived, |d| d.max as u32);
    let own = rules.source("room.apartments_max").is_some_and(|l| l != super::rules::Layer::Builtin);
    let cap = if own { derived.min(fixed) } else { derived };
    cap.min(top)
}

/// Where a floor's apartment limit comes from, for the message.
fn cap_reason(v: &J) -> String {
    match v["footprint"]["width"].as_f64() {
        Some(w) => format!(
            " (footprint.width {w} m, both sides of the corridor; room.apartment_frontage sets the facade each needs)"
        ),
        None => " (room.apartments_max; a footprint.width raises it)".to_string(),
    }
}

fn read_roof(rd: &mut Rd, v: &J, typology: Typology) -> (RoofKind, f64, Option<RoofForm>) {
    let rf = &v["roof"];
    if let Some(x) = rf.as_object() {
        rd.keys("roof", x, &["kind", "slope", "overhang"]);
    } else if !rf.is_null() {
        rd.bad("roof", "must be {kind: flat|pitched|hip|gable|shed|barrel, slope?, overhang?}");
    }
    let (kind, form) = match rf["kind"].as_str() {
        None if typology == Typology::House => (RoofKind::Pitched, None),
        None | Some("flat") => (RoofKind::Flat, None),
        Some("pitched") => (RoofKind::Pitched, None),
        Some("hip") => (RoofKind::Pitched, Some(RoofForm::Hip)),
        Some("gable") => (RoofKind::Pitched, Some(RoofForm::Gable)),
        Some("shed") => (RoofKind::Pitched, Some(RoofForm::Shed)),
        Some("barrel") => (RoofKind::Pitched, Some(RoofForm::Barrel)),
        Some(k) => {
            rd.bad("roof.kind", format!("unknown '{k}', one of flat, pitched, hip, gable, shed, barrel"));
            (RoofKind::Flat, None)
        }
    };
    let slope = ROOF_SLOPE.default.unwrap_or(ROOF_SLOPE.min);
    (kind, rd.num(&rf["slope"], slope, &ROOF_SLOPE), form)
}

/// Fire rating and the three finishes, in that order.
fn read_params(rd: &mut Rd, v: &J, fire: &str) -> [String; 4] {
    const KEYS: [&str; 4] = [
        "fire_rating",
        "floor_finish",
        "wall_finish",
        "ceiling_finish",
    ];
    let pm = &v["params"];
    if let Some(x) = pm.as_object() {
        rd.keys("params", x, &KEYS);
    } else if !pm.is_null() {
        let msg = "must be {fire_rating?, floor_finish?, wall_finish?, ceiling_finish?}";
        rd.bad("params", msg);
    }
    KEYS.map(|k| {
        let def = if k == "fire_rating" { fire } else { "TBD" };
        rd.text(&format!("params.{k}"), &pm[k], def)
    })
}

fn pick<T: Copy>(rd: &mut Rd, path: &str, v: &J, names: &[&str], all: &[T], def: T) -> T {
    match v {
        J::Null => def,
        J::String(s) => match names.iter().position(|n| n == s) {
            Some(i) => all[i],
            None => {
                rd.bad(path, format!("unknown '{s}'{}", suggest(s, names)));
                def
            }
        },
        _ => {
            rd.bad(path, format!("one of {}", names.join(", ")));
            def
        }
    }
}

fn flag(rd: &mut Rd, path: &str, v: &J, def: bool) -> bool {
    match v {
        J::Null => def,
        J::Bool(b) => *b,
        _ => {
            rd.bad(path, "true or false");
            def
        }
    }
}

const DESIGN_KEYS: [&str; 19] = [
    "atrium",
    "shell",
    "structure",
    "curve",
    "curve_north",
    "twist",
    "taper",
    "levels",
    "massing",
    "upper_from",
    "setback",
    "cantilever",
    "facade",
    "rhythm",
    "balconies",
    "terrace",
    "canopy",
    "paving",
    "trees",
];

/// Design intent; a brief without it keeps the plain bar, a brief with it gets an entrance
/// canopy and a little site unless it says otherwise.
fn read_design(rd: &mut Rd, v: &J, floors: u32) -> Design {
    let dv = &v["design"];
    let Some(o) = dv.as_object() else {
        if !dv.is_null() {
            rd.bad("design", "must be an object");
        }
        return Design::default();
    };
    rd.keys("design", o, &DESIGN_KEYS);
    let d = Design::asked();
    let atrium = flag(rd, "design.atrium", &dv["atrium"], d.atrium);
    // the atrium is the courtyard under a roof: a brief naming no massing gets the courtyard
    let massing_def = if atrium { Massing::Courtyard } else { d.massing };
    let massing = pick(rd, "design.massing", &dv["massing"], &Massing::NAMES, &Massing::ALL, massing_def);
    if atrium && massing != Massing::Courtyard {
        rd.bad("design.atrium", "the atrium roofs the courtyard: massing courtyard, or no atrium");
    }
    if atrium && dv["massing"].is_null() {
        rd.notes.push("design.atrium: built as a courtyard block with the court roofed over".into());
    }
    let upper_from = (!dv["upper_from"].is_null())
        .then(|| rd.int("design.upper_from", &dv["upper_from"], 1, 1, floors.max(2) - 1));
    let setback = rd.num(&dv["setback"], d.setback, &SETBACK);
    let cantilever = rd.num(&dv["cantilever"], d.cantilever, &CANTILEVER);
    if massing.wings() > 0 && (setback > 0.0 || cantilever > 0.0) {
        rd.bad("design", "setback and cantilever step a bar or tower_podium, not wings");
    }
    if setback > 0.0 && cantilever > 0.0 {
        rd.bad("design.cantilever", "either setback or cantilever, not both");
    }
    let curve = rd.num(&dv["curve"], d.curve, &CURVE);
    let curve_north = rd.num(&dv["curve_north"], d.curve_north, &CURVE_NORTH);
    if (curve > 0.0 || curve_north > 0.0)
        && (massing != Massing::Bar || setback > 0.0 || cantilever > 0.0 || upper_from.is_some())
    {
        rd.bad("design.curve", "a curved facade bows a plain bar: no wings, podium, setback or cantilever");
    }
    // a curtain wall named by its keyword is the curtain facade
    let alias = dv["facade"].as_str().filter(|s| super::brief::asks_curtain_wall(s)).map(|_| serde_json::json!("curtain"));
    let fac = alias.as_ref().unwrap_or(&dv["facade"]);
    let facade = pick(rd, "design.facade", fac, &FacadeSystem::NAMES, &FacadeSystem::ALL, d.facade);
    let bal = &dv["balconies"];
    let balconies = pick(rd, "design.balconies", bal, &Balconies::NAMES, &Balconies::ALL, d.balconies);
    Design {
        massing,
        upper_from,
        setback,
        cantilever,
        facade,
        rhythm: rd.num(&dv["rhythm"], d.rhythm, &RHYTHM),
        balconies,
        terrace: flag(rd, "design.terrace", &dv["terrace"], d.terrace),
        canopy: flag(rd, "design.canopy", &dv["canopy"], d.canopy),
        paving: rd.num(&dv["paving"], d.paving, &PAVING),
        trees: rd.int("design.trees", &dv["trees"], d.trees, 0, max_trees()),
        atrium,
        curve,
        curve_north,
        shell: flag(rd, "design.shell", &dv["shell"], d.shell),
        structure: pick(rd, "design.structure", &dv["structure"], &Structure::NAMES, &Structure::ALL, d.structure),
    }
}

const SERVICES_KEYS: [&str; 5] = ["electrical", "water", "appliances", "boards", "hvac"];

/// Services: `true` alone switches every system on; an object picks them.
fn read_services(rd: &mut Rd, v: &J) -> super::Services {
    use super::{Boards, Services};
    let sv = &v["services"];
    match sv {
        J::Null | J::Bool(false) => return Services::default(),
        J::Bool(true) => {
            return Services {
                electrical: true,
                water: true,
                appliances: true,
                boards: Boards::Unit,
                hvac: None,
            }
        }
        J::Object(o) => rd.keys("services", o, &SERVICES_KEYS),
        _ => {
            rd.bad("services", "true, or {electrical?, water?, appliances?, boards?, hvac?}");
            return Services::default();
        }
    }
    let electrical = flag(rd, "services.electrical", &sv["electrical"], true);
    let water = flag(rd, "services.water", &sv["water"], true);
    Services {
        electrical,
        water,
        appliances: flag(rd, "services.appliances", &sv["appliances"], electrical || water),
        boards: pick(rd, "services.boards", &sv["boards"], &Boards::NAMES, &Boards::ALL, Boards::Unit),
        hvac: match &sv["hvac"] {
            J::Null => None,
            _ => Some(flag(rd, "services.hvac", &sv["hvac"], true)),
        },
    }
}

fn read_origin(rd: &mut Rd, v: &J) -> (f64, f64) {
    let pair = v["origin"].as_array().filter(|a| a.len() == 2);
    let xy = pair.and_then(|a| Some((a[0].as_f64()?, a[1].as_f64()?)));
    match (&v["origin"], xy) {
        (J::Null, _) => (0.0, 0.0),
        (_, Some((x, y))) if x.is_finite() && y.is_finite() => (x, y),
        _ => {
            rd.bad("origin", "must be [x, y] in metres");
            (0.0, 0.0)
        }
    }
}

fn default_name(typology: Typology, ru: bool) -> &'static str {
    match (typology, ru) {
        (Typology::House, false) => "House",
        (Typology::ApartmentSection, false) => "Apartment section",
        (Typology::Office, false) => "Office",
        (Typology::School, false) => "School",
        (Typology::House, true) => "\u{414}\u{43e}\u{43c}",
        (Typology::ApartmentSection, true) => "\u{421}\u{435}\u{43a}\u{446}\u{438}\u{44f}",
        (Typology::Office, true) => "\u{41e}\u{444}\u{438}\u{441}",
        (Typology::School, true) => "\u{428}\u{43a}\u{43e}\u{43b}\u{430}",
    }
}
