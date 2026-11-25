// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What the chat sends: the core tools always, a catalog group after `load_tools` or after a
//! call of one of its tools by name, in compact wire form (`data/tool_groups.json`). Requests
//! come in any language, so the model chooses groups; `intent_groups` (English/Russian subject
//! words) is an opt-in hint that only adds. MCP and the contracts keep the full catalog
//! (`catalog::schemas`); only the chat request is trimmed. Any catalog tool may still be called
//! by name: groups change what is shown, not what is allowed.

use crate::catalog::{self, ToolSpec};
use crate::convo::{Mode, ToolOutcome};
use crate::provider::ToolSchema;
use crate::registry::ensure_coded;
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

pub const LOAD_TOOLS: &str = "load_tools";
pub const HELP: &str = "help";

const DATA: &str = include_str!("../data/tool_groups.json");

pub struct Group {
    pub name: String,
    pub about: String,
    pub tools: Vec<String>,
}

struct Intent {
    groups: Vec<String>,
    words: Vec<String>,
}

struct Data {
    core: Vec<String>,
    groups: Vec<Group>,
    intents: Vec<Intent>,
    short: BTreeMap<String, String>,
    recipes: BTreeMap<String, String>,
}

fn strings(v: &J) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn text_map(v: &J) -> BTreeMap<String, String> {
    v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| {
        let v: J = serde_json::from_str(DATA).expect("data/tool_groups.json is JSON");
        let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        Data {
            core: strings(&v["core"]),
            groups: list("groups")
                .iter()
                .map(|g| Group {
                    name: g["name"].as_str().unwrap_or_default().to_string(),
                    about: g["about"].as_str().unwrap_or_default().to_string(),
                    tools: strings(&g["tools"]),
                })
                .collect(),
            intents: list("intents")
                .iter()
                .map(|i| Intent {
                    groups: strings(&i["groups"]),
                    words: strings(&i["words"]).iter().map(|w| w.to_lowercase()).collect(),
                })
                .collect(),
            short: text_map(&v["short"]),
            recipes: text_map(&v["recipes"]),
        }
    })
}

pub fn groups() -> &'static [Group] {
    &data().groups
}

pub fn core() -> &'static [String] {
    &data().core
}

pub fn recipe(name: &str) -> Option<&'static str> {
    data().recipes.get(name).map(String::as_str)
}

pub fn group_of(tool: &str) -> Option<&'static str> {
    groups()
        .iter()
        .find(|g| g.tools.iter().any(|t| t == tool))
        .map(|g| g.name.as_str())
}

fn at_word_start(text: &str, word: &str) -> bool {
    // CJK text has no spaces: a Chinese or Japanese subject word counts anywhere
    if word.chars().next().is_some_and(|c| c as u32 >= 0x2E80) {
        return text.contains(word);
    }
    text.match_indices(word)
        .any(|(i, _)| text[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
}

/// Groups whose subject the request names, matched at the start of a word.
pub fn intent_groups(text: &str) -> BTreeSet<&'static str> {
    let t = text.to_lowercase();
    let mut out = BTreeSet::new();
    for i in &data().intents {
        if i.words.iter().any(|w| at_word_start(&t, w)) {
            out.extend(i.groups.iter().map(String::as_str));
        }
    }
    out
}

/// Said once in the system prompt instead of in every description.
const BOILERPLATE: [&str; 6] = [
    " Draft only: nothing reaches the model before Apply.",
    ", and nothing reaches the model before Apply",
    " A schematic starting point, not a checked design.",
    " The result is a schematic starting point: do not claim it meets any code or is a finished design.",
    " Passing is not proof of code compliance.",
    " Not proof of code compliance.",
];

/// A description without the plan prefix and the notes the system prompt states once.
pub fn compact(text: &str) -> String {
    let mut t = text.strip_prefix("Plan: ").unwrap_or(text).to_string();
    for b in BOILERPLATE {
        t = t.replace(b, "");
    }
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Empty `required` lists go, and the top-level title and why lose the description the
/// system prompt gives once.
fn lean(v: &mut J, top: bool) {
    let Some(o) = v.as_object_mut() else {
        return;
    };
    if o.get("required").and_then(J::as_array).is_some_and(|a| a.is_empty()) {
        o.remove("required");
    }
    if let Some(p) = o.get_mut("properties").and_then(J::as_object_mut) {
        for (k, x) in p.iter_mut() {
            if top && (k == "title" || k == "why") {
                if let Some(xo) = x.as_object_mut() {
                    xo.remove("description");
                }
            }
            lean(x, false);
        }
    }
    if let Some(x) = o.get_mut("items") {
        lean(x, false);
    }
    if let Some(a) = o.get_mut("oneOf").and_then(J::as_array_mut) {
        for x in a {
            lean(x, false);
        }
    }
}

/// One tool as the chat sends it.
pub fn wire(s: &ToolSpec) -> ToolSchema {
    let mut input = (s.input)().to_json();
    lean(&mut input, true);
    let description = match data().short.get(s.name) {
        Some(t) => crate::limits::with_limits(s.name, t),
        None => crate::limits::with_limits(s.name, &compact(s.summary)),
    };
    ToolSchema {
        name: s.name.to_string(),
        description,
        input_schema: input,
    }
}

fn load_tools_schema() -> ToolSchema {
    let names: Vec<&str> = groups().iter().map(|g| g.name.as_str()).collect();
    let about: Vec<String> = groups()
        .iter()
        .map(|g| format!("{} ({})", g.name, g.about))
        .collect();
    ToolSchema {
        name: LOAD_TOOLS.to_string(),
        description: format!(
            "Add tool groups for the rest of the chat; their tools come with the next step. \
             Choose by what the task needs, whatever language it is asked in: {}.",
            about.join("; ")
        ),
        input_schema: json!({
            "type": "object",
            "properties": {"groups": {"type": "array", "items": {"type": "string", "enum": names}}},
            "required": ["groups"],
            "additionalProperties": false
        }),
    }
}

/// One thing `help` can explain: a design note, a worked script, a script block, a generator
/// or a typology. Script recipes and blocks come from the script crate and generators from the
/// generator registry, so the list cannot drift from what the product can build.
pub struct Topic {
    pub name: String,
    pub kind: &'static str,
    pub summary: String,
}

fn short(text: &str) -> String {
    let first = text.split(['.', '\n']).next().unwrap_or(text).trim();
    first.chars().take(110).collect()
}

/// Every topic, by kind: notes, recipes, blocks, generators, typologies.
pub fn topics() -> &'static [Topic] {
    static T: OnceLock<Vec<Topic>> = OnceLock::new();
    T.get_or_init(|| {
        let mut out = Vec::new();
        for (n, t) in &data().recipes {
            out.push(Topic { name: n.clone(), kind: "note", summary: short(t) });
        }
        for (n, sc) in ddc_script::recipes::RECIPES {
            let summary = ddc_script::recipes::summary(sc).to_string();
            out.push(Topic { name: n.to_string(), kind: "recipe", summary });
        }
        for l in ddc_script::recipes::BLOCKS.lines().map(str::trim_start) {
            let name = l.split(['{', '|', ',']).next().unwrap_or("").trim();
            if !name.is_empty() && !name.contains(' ') {
                out.push(Topic { name: name.to_string(), kind: "block", summary: short(&l[name.len()..]) });
            }
        }
        for g in ddc_gen::registry() {
            out.push(Topic { name: g.id().to_string(), kind: "generator", summary: short(g.summary()) });
        }
        for t in ddc_gen::project::Typology::NAMES {
            let summary = "a generate_project typology".to_string();
            out.push(Topic { name: t.to_string(), kind: "typology", summary });
        }
        out
    })
}

fn names_of(kind: &str) -> String {
    let v: Vec<&str> = topics().iter().filter(|t| t.kind == kind).map(|t| t.name.as_str()).collect();
    v.join(", ")
}

fn help_schema() -> ToolSchema {
    ToolSchema {
        name: HELP.to_string(),
        description: format!(
            "Full description and input of any tool, or how to build something. Topics (ask in any language, the names are English): notes: {}; worked scripts for run_script: {}; script blocks: {}; generators: {}. A topic \"all\" lists them with summaries.",
            names_of("note"),
            names_of("recipe"),
            names_of("block"),
            names_of("generator"),
        ),
        input_schema: json!({
            "type": "object",
            "properties": {"topic": {"type": "string", "maxLength": 64}},
            "required": ["topic"],
            "additionalProperties": false
        }),
    }
}

/// The topic a free-form request means: lower case, the `run_script` prefix and separators
/// dropped (`run_script space_frame`, `Space Frame` and `space-frame` all name one block).
fn normal(topic: &str) -> String {
    let t = topic.trim().to_lowercase();
    let t = t.strip_prefix("run_script").map_or(t.as_str(), str::trim);
    t.replace([' ', '-'], "_")
}

fn index_text() -> String {
    topics()
        .iter()
        .map(|t| format!("{} [{}]: {}", t.name, t.kind, t.summary))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Topics whose name or summary shares a word of at least four letters with the request.
fn close_to(topic: &str) -> Vec<&'static Topic> {
    let words: Vec<String> = topic
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_string)
        .collect();
    topics()
        .iter()
        .filter(|t| {
            let hay = format!("{} {}", t.name, t.summary).to_lowercase();
            words.iter().any(|w| hay.contains(w.as_str()))
        })
        .collect()
}

fn topic_answer(t: &str) -> Option<ToolOutcome> {
    if let Some(r) = recipe(t) {
        return Some(ToolOutcome::ok(json!({"recipe": t, "kind": "note", "text": r}).to_string()));
    }
    if let Some(sc) = ddc_script::recipes::recipe(t) {
        let out = json!({
            "recipe": t, "kind": "script recipe", "summary": ddc_script::recipes::summary(sc),
            "params": ddc_script::recipes::params_of(sc), "script": sc,
            "next": "run_script with this script, changing the params or options as asked"
        });
        return Some(ToolOutcome::ok(out.to_string()));
    }
    if let Some(g) = ddc_gen::registry().iter().find(|g| g.id() == t) {
        let params: Vec<J> = g
            .params()
            .iter()
            .map(|p| json!({"name": p.name, "min": p.min, "max": p.max, "default": p.default, "help": p.help}))
            .collect();
        let out = json!({"generator": t, "kind": "generator", "summary": g.summary(),
                         "variants": g.variants(), "params": params});
        return Some(ToolOutcome::ok(out.to_string()));
    }
    let blocks = ddc_script::recipes::block_lines(t);
    if !blocks.is_empty() {
        let with: Vec<&str> = ddc_script::recipes::RECIPES
            .iter()
            .filter(|(_, sc)| sc.contains(&format!("{t}{{")) || sc.contains(&format!("{t}.")))
            .map(|(n, _)| *n)
            .collect();
        let out = json!({"block": t, "kind": "script block", "options": blocks,
                         "worked_scripts": with,
                         "note": "a block is called inside run_script as name{options}; script_recipes lists them all"});
        return Some(ToolOutcome::ok(out.to_string()));
    }
    if ddc_gen::project::Typology::NAMES.contains(&t) {
        let out = json!({"typology": t, "kind": "typology",
                         "next": "generate_project with spec.typology; norms_programme gives its room programme"});
        return Some(ToolOutcome::ok(out.to_string()));
    }
    None
}

/// The tools of a chat request: the core and the two meta tools first, then each loaded group
/// in file order. A group loaded mid-chat only appends, so the cached core prefix still holds.
pub fn chat_schemas(mode: Mode, images: bool, loaded: &BTreeSet<String>) -> Vec<ToolSchema> {
    let ok = |s: &&ToolSpec| catalog::allowed(s, mode, images);
    let mut out: Vec<ToolSchema> = catalog::catalog()
        .iter()
        .filter(ok)
        .filter(|s| core().iter().any(|t| t == s.name))
        .map(wire)
        .collect();
    out.push(load_tools_schema());
    out.push(help_schema());
    for g in groups().iter().filter(|g| loaded.contains(&g.name)) {
        let of_g = |s: &&ToolSpec| g.tools.iter().any(|t| t == s.name);
        out.extend(catalog::catalog().iter().filter(ok).filter(of_g).map(wire));
    }
    out
}

pub fn is_meta(name: &str) -> bool {
    name == LOAD_TOOLS || name == HELP
}

fn err(text: impl Into<String>) -> ToolOutcome {
    ensure_coded(ToolOutcome::error(text))
}

/// Runs `load_tools` or `help`; they read the catalog and change only what the chat shows.
pub fn meta_call(
    name: &str,
    args: Option<&J>,
    mode: Mode,
    images: bool,
    loaded: &mut BTreeSet<String>,
) -> ToolOutcome {
    let Some(a) = args else {
        return err("the arguments are not valid JSON");
    };
    if name == LOAD_TOOLS {
        let known: Vec<&str> = groups().iter().map(|g| g.name.as_str()).collect();
        let Some(list) = a["groups"].as_array() else {
            return err(format!("groups: expected a list of {}", known.join(", ")));
        };
        let (mut added, mut tools) = (Vec::new(), Vec::new());
        for g in list {
            let Some(grp) = g.as_str().and_then(|n| groups().iter().find(|x| x.name == n)) else {
                return err(format!("unknown group {g}; known: {}", known.join(", ")));
            };
            if loaded.insert(grp.name.clone()) {
                added.push(grp.name.as_str());
            }
            let usable = |t: &&String| catalog::find(t).is_some_and(|s| catalog::allowed(s, mode, images));
            tools.extend(grp.tools.iter().filter(usable).map(String::as_str));
        }
        let out = json!({"loaded": added, "tools": tools, "note": "callable from the next step"});
        return ToolOutcome::ok(out.to_string());
    }
    let Some(topic) = a["topic"].as_str() else {
        return err("topic: expected a tool name or a recipe name");
    };
    let t = normal(topic);
    if matches!(t.as_str(), "all" | "list" | "topics" | "recipes" | "generators" | "blocks") {
        return ToolOutcome::ok(json!({"topics": index_text()}).to_string());
    }
    if let Some(r) = topic_answer(&t) {
        return r;
    }
    match catalog::find(topic.trim()).or_else(|| catalog::find(&t)) {
        Some(s) => ToolOutcome::ok(
            json!({"tool": s.name, "group": group_of(s.name).unwrap_or("core"),
                   "description": s.summary, "input": (s.input)().to_json()})
            .to_string(),
        ),
        None => {
            let near: Vec<String> = close_to(topic)
                .iter()
                .take(8)
                .map(|x| format!("{} [{}]: {}", x.name, x.kind, x.summary))
                .collect();
            err(format!(
                "no tool or topic '{topic}'. Closest: {}. All topics:\n{}",
                if near.is_empty() { "none".to_string() } else { near.join("; ") },
                index_text()
            ))
        }
    }
}

/// Estimated tokens of a tool list, four characters a token as the harness counts them.
pub fn tokens_est(tools: &[ToolSchema]) -> u64 {
    tools
        .iter()
        .map(|t| ((t.name.len() + t.description.len() + t.input_schema.to_string().len()) / 4) as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_frame_note_names_a_real_generator_and_its_params() {
        let r = recipe("lattice_frame").unwrap();
        let g = ddc_gen::registry().iter().find(|g| g.id() == "steel_frame").expect("steel_frame generator");
        assert!(r.contains("run_generator") && r.contains("connect_members"), "{r}");
        for p in ["kind", "pattern", "tower_height", "base_width", "platform_1", "platform_2", "platform_3", "detail"] {
            assert!(r.contains(&format!("\"{p}\"")), "recipe lacks {p}");
            assert!(g.params().iter().any(|d| d.name == p), "steel_frame has no param {p}");
        }
        for t in ["run_generator", "connect_members"] {
            assert!(catalog::find(t).is_some(), "no tool {t}");
        }
        assert!(r.contains("lattice_tower_300") && r.find("run_generator") < r.find("run_script"), "{r}");
    }

    /// A request for a tower "at excellent detail" in one line: the recipe must say which detail
    /// that is and that the same call joins the nodes, and the system prompt must send a tower to the recipe.
    #[test]
    fn a_detailed_tower_brief_finds_detail_3_and_the_joints() {
        let r = recipe("lattice_frame").unwrap();
        assert!(r.contains("detail 3 when the brief asks for high, fine, excellent or full detail"), "{r}");
        assert!(r.contains("a detailed tower is a single run_generator call: do not call connect_members after it"), "{r}");
        assert!(r.contains("\"joints\": \"all\""), "{r}");
        let g = ddc_gen::registry().iter().find(|g| g.id() == "steel_frame").expect("steel_frame generator");
        assert!(g.params().iter().any(|d| d.name == "joints"), "steel_frame has no joints param");
        let sys = include_str!("../data/system.md");
        assert!(sys.contains("An open steel lattice on legs") && sys.contains("`help` lattice_frame"));
        // the prompt teaches primitives: no object is routed to a template
        for gone in ["300 m kind", "nearest recipe", "landmark"] {
            assert!(!sys.contains(gone), "system prompt still says {gone}");
        }
    }

    /// A hard-case brief in any of five languages preloads the group its recipe needs, the
    /// recipe (or tool) exists, and the system prompt names it for that subject.
    /// A Russian prompt from the test data file (a string, or a list of strings).
    fn ru_all(key: &str) -> Vec<String> {
        let m: serde_json::Value = serde_json::from_str(include_str!("../tests/data/ru_prompts.json")).unwrap();
        match m[key].as_array() {
            Some(a) => a.iter().map(|x| x.as_str().unwrap().to_string()).collect(),
            None => vec![m[key].as_str().unwrap_or_else(|| panic!("no ru prompt {key}")).to_string()],
        }
    }

    fn ru(key: &str) -> String {
        ru_all(key).remove(0)
    }

    fn routes(briefs: [&str; 5], group: &str, target: &str, cue: &str) {
        for b in briefs {
            assert!(intent_groups(b).contains(group), "{b}: no {group}");
        }
        let known = recipe(target).is_some() || ddc_script::recipes::recipe(target).is_some() || catalog::find(target).is_some();
        assert!(known, "no recipe or tool {target}");
        let sys = include_str!("../data/system.md");
        assert!(sys.contains(cue) && sys.contains(target), "system prompt lacks {cue} -> {target}");
    }

    #[test]
    fn h04_stadium_brief_finds_its_recipe() {
        routes(
            [
                "A 40,000-seat football stadium with a cantilevered roof ring",
                "Ein Fußballstadion für 40 000 Zuschauer mit Kragdach",
                "Un estadio de fútbol de 40 000 asientos con cubierta en voladizo",
                ru("stadium").as_str(),
                "一座四万座的足球体育场，悬挑钢桁架屋顶",
            ],
            "scripts",
            "cantilever_ring",
            "cantilever trusses round an opening",
        );
    }

    #[test]
    fn h08_metro_station_brief_finds_its_recipe() {
        routes(
            [
                "Underground metro station 18 m below ground",
                "Eine U-Bahn-Station 18 m unter der Erde",
                "Una estación de metro subterránea a 18 m",
                ru("metro").as_str(),
                "地下18米的地铁车站",
            ],
            "station",
            "plan_station",
            "an underground box of levels",
        );
    }

    #[test]
    fn h10_arch_bridge_brief_finds_its_recipe() {
        routes(
            [
                "Pedestrian bridge over a river, 90 m steel arch with hangers",
                "Eine Fußgängerbrücke mit 90 m Stahlbogen",
                "Un puente peatonal con arco de acero de 90 m",
                ru("bridge").as_str(),
                "跨河人行拱桥，主跨90米",
            ],
            "scripts",
            "arch_span",
            "a deck hung from tied arches",
        );
    }

    #[test]
    fn h11_lattice_tower_joints_brief_finds_its_recipe() {
        routes(
            [
                "A riveted wrought-iron lattice tower 300 m tall with bolted joints",
                "Ein genieteter Gitterturm aus Stahl, 300 m hoch",
                "Una torre de celosía de acero de 300 m",
                ru("tower").as_str(),
                "一座300米高的钢结构格构铁塔",
            ],
            "steel",
            "lattice_frame",
            "An open steel lattice on legs",
        );
    }

    #[test]
    fn h14_tower_platform_brief_finds_its_recipe() {
        let r = recipe("lattice_frame").unwrap();
        assert!(r.contains("One more platform on a built tower") && r.contains("never rerun the generator"), "{r}");
        routes(
            [
                "Add one more viewing platform at 200 m on the lattice tower",
                "Eine weitere Plattform auf 200 m am Gitterturm",
                "Otra plataforma a 200 m en la torre de celosía",
                ru("tower_platform").as_str(),
                "在格构铁塔200米处再加一个观景平台",
            ],
            "scripts",
            "lattice_frame",
            "An open steel lattice on legs",
        );
    }

    #[test]
    fn h16_hangar_brief_finds_its_recipe() {
        routes(
            [
                "Aircraft maintenance hangar 100 by 80 m, space frame roof",
                "Ein Flugzeughangar 100 x 80 m mit Raumfachwerk",
                "Un hangar de aviones de 100 por 80 m con malla espacial",
                ru("hangar").as_str(),
                "100×80米飞机维修机库，网架屋顶",
            ],
            "scripts",
            "long_span_roof",
            "plane trusses over a clear span",
        );
    }

    /// A transmission pylon asked for in any of the three languages finds the power_line service.
    #[test]
    fn the_tower_recipe_names_the_power_line_pylon_in_every_language() {
        let r = recipe("lattice_frame").unwrap();
        for w in [ru("pylon").as_str(), "power pylon", "Strommast", "\"service\": \"power_line\""] {
            assert!(r.contains(w), "recipe lacks {w}");
        }
        let g = ddc_gen::registry().iter().find(|g| g.id() == "steel_frame").expect("steel_frame generator");
        assert!(g.params().iter().any(|d| d.name == "service"), "steel_frame has no service param");
    }

    #[test]
    fn every_catalog_tool_is_core_or_in_exactly_one_group() {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for t in core().iter().chain(groups().iter().flat_map(|g| &g.tools)) {
            *seen.entry(t.as_str()).or_insert(0) += 1;
        }
        for s in catalog::catalog() {
            assert_eq!(seen.remove(s.name), Some(1), "{}", s.name);
        }
        assert!(seen.is_empty(), "not in the catalog: {seen:?}");
        for k in data().short.keys() {
            assert!(catalog::find(k).is_some(), "short for unknown {k}");
        }
        assert!(groups().len() >= 8 && data().recipes.len() >= 6);
    }

    #[test]
    fn the_chat_sends_far_less_than_the_catalog_and_keeps_the_core() {
        let full = catalog::schemas(Mode::Propose, true);
        let before = tokens_est(&full);
        let core = chat_schemas(Mode::Propose, true, &BTreeSet::new());
        let after = tokens_est(&core);
        let all: BTreeSet<String> = groups().iter().map(|g| g.name.clone()).collect();
        let every = chat_schemas(Mode::Propose, true, &all);
        let everything = tokens_est(&every);
        let gp = core.iter().find(|t| t.name == "generate_project").map(|t| tokens_est(std::slice::from_ref(t)));
        eprintln!(
            "tool schemas: catalog {} tools ~{before} tokens; chat core {} tools ~{after}; \
             all groups {} tools ~{everything}; generate_project ~{gp:?}",
            full.len(),
            core.len(),
            every.len()
        );
        let mut per: Vec<(u64, &str)> =
            core.iter().map(|t| (tokens_est(std::slice::from_ref(t)), t.name.as_str())).collect();
        per.sort_by(|a, b| b.cmp(a));
        eprintln!("core by size: {per:?}");
        // Estimated 7759 after the schema trims (was 9814), limit with 2 % room; target 8000,
        // only lower.
        assert!(after <= 7_920, "core {after} tokens");
        assert!(everything < before, "{everything} >= {before}");
        assert_eq!(every.len(), full.len() + 2);
        // a loaded group only appends: the cached prefix of the core stays byte for byte
        let names = |v: &[ToolSchema]| v.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        let data: BTreeSet<String> = ["data".to_string()].into();
        let with = chat_schemas(Mode::Propose, true, &data);
        assert_eq!(names(&with[..core.len()]), names(&core));
        assert!(with.len() > core.len());
        for t in ["generate_project", "revise_project", "create_geometry", "query_elements"] {
            assert!(core.iter().any(|x| x.name == t), "{t}");
        }
        assert!(core.iter().all(|t| t.name != "move_elements" && t.name != "deliver_project"));
        let ro = chat_schemas(Mode::ReadOnly, true, &all);
        assert!(ro.iter().all(|t| t.name != "generate_project"));
    }

    /// What the chat description of a tool leaves out is still in `help <tool>`.
    #[test]
    fn the_long_tool_texts_moved_out_of_the_chat_are_in_help() {
        let mut loaded = BTreeSet::new();
        let mut ask = |t: &str| meta_call(HELP, Some(&json!({"topic": t})), Mode::Propose, true, &mut loaded).text;
        let gp = ask("generate_project");
        for hint in [
            "guest rooms, reception, restaurant",
            "tower_podium",
            "Setback and cantilever step a bar",
            "Data centre: tag rooms data_hall",
            "help equipment_fitout",
        ] {
            assert!(gp.contains(hint), "generate_project: {hint}");
        }
        let cg = ask("create_geometry");
        for hint in ["{by: network", "planarize: rounds", "{shape: hypar", "kind: roof|slab", "keep_tool"] {
            assert!(cg.contains(hint), "create_geometry: {hint}");
        }
        for (tool, pointer) in [("generate_project", "help generate_project"), ("create_geometry", "help create_geometry")] {
            let w = wire(catalog::find(tool).unwrap()).description;
            assert!(w.contains(pointer), "{tool}: {w}");
            assert!(w.len() < catalog::find(tool).unwrap().summary.len() / 2, "{tool} still long");
        }
    }

    /// What the chat copy of a core tool or schema field no longer says is in `help`.
    #[test]
    fn the_texts_trimmed_from_the_core_schemas_are_in_help() {
        let mut loaded = BTreeSet::new();
        let mut ask = |t: &str| meta_call(HELP, Some(&json!({"topic": t})), Mode::Propose, true, &mut loaded).text;
        let notes = ask("field_notes");
        for hint in [
            "not a position in this list",
            "from create_level too, before Apply",
            "a longer one may be cut off",
            "a minus needs a unit or %",
            "named in `skipped`",
            "x east, z up",
            "shape: chs",
            "draws no terrain",
            "toposolid",
            "washing machine, dishwasher, cooker",
            "Ventilation is automatic",
            "exactly one of filter text",
            "a bare level=3 is the third level by elevation",
            "brief_check says which were built",
            "lock it after the draft",
            "norms_programme gives one",
            "ends vertical",
            "roof.vault_*",
            "passenger traffic",
            "building height rule",
            "a list such as rooms is replaced whole",
            "and the ones they cleared",
            "in_frame",
            "pixel cap",
            "the IFC storeys follow the sum of the heights below",
        ] {
            assert!(notes.contains(hint), "field_notes: {hint}");
        }
        for (tool, phrase) in [
            ("revise_project", "gets a new item that changes, adds and deletes only what differs"),
            ("norms_programme", "allowed kinds"),
            ("view_snapshot", "taken offscreen"),
            ("render_plan", "Capped in pixels"),
            ("run_checks", "it is not proof of code compliance"),
            ("render_3d", "(not the screen)"),
            ("mark_checkpoint", "as it stands now"),
            ("replan_items", "off the plan"),
            ("get_reference", "in measured numbers"),
            ("list_materials", "is flagged on the plan"),
        ] {
            let full = ask(tool);
            assert!(full.contains(phrase), "{tool}: {phrase}");
            let s = catalog::find(tool).unwrap();
            // both carry the tool's limits; the chat text is the shorter one
            let long = crate::limits::with_limits(tool, s.summary);
            assert!(wire(s).description.len() < long.len(), "{tool} chat text is not shorter");
        }
        for tool in ["create_geometry", "create_elements", "update_elements", "generate_project"] {
            let w = wire(catalog::find(tool).unwrap());
            assert!(w.input_schema.to_string().contains("help field_notes"), "{tool}: no pointer to field_notes");
        }
        let data = group_of("clash");
        assert_eq!((data, group_of("norms_programme")), (Some("data"), Some("data")));
        assert!(intent_groups("find the clashes between the beams").contains("data"));
    }

    /// Notes trimmed from the design schema stay reachable through help.
    #[test]
    fn the_design_notes_moved_out_of_the_schema_are_in_help() {
        let t = recipe("design_fields").expect("design_fields note");
        for hint in [
            "footprint only",
            "negative grows",
            "core stays put",
            "wings round corner halls",
            "first storey (ground 0)",
            "planters on flat and podium roofs",
            "roofed courtyard, floor on the ground slab",
            "plain bar only",
            "outline must be seen whole from one inner point",
            "bearing walls",
            "columns on every grid crossing",
            "concrete corridor spine",
            "default concrete",
            "default true",
            "default 3",
        ] {
            assert!(t.contains(hint), "{hint}");
        }
        assert!(topics().iter().any(|x| x.name == "design_fields"));
    }

    #[test]
    fn wire_text_drops_the_plan_prefix_and_the_repeated_notes() {
        let s = catalog::find("create_geometry").unwrap();
        let w = wire(s);
        assert!(w.description.starts_with("Create real geometry"), "{}", w.description);
        assert!(!w.description.contains("Draft only"));
        assert!(w.input_schema.to_string().contains("\"title\""));
        let m = wire(catalog::find("move_elements").unwrap()).input_schema;
        assert!(m["properties"]["title"].get("description").is_none(), "{m}");
        assert!(m["properties"]["target"].get("description").is_some(), "{m}");
        assert!(!m.to_string().contains("\"required\":[]"), "{m}");
        assert!(wire(catalog::find("run_script").unwrap()).description.contains("help run_script"));
    }

    #[test]
    fn subjects_load_their_groups_at_word_starts() {
        let g = intent_groups("Route the supply DUCT to every fan coil");
        assert!(g.contains("mep"), "{g:?}");
        assert!(intent_groups("Remove the old note").is_empty());
        assert!(intent_groups(&ru("ducts")).contains("mep"));
        assert!(intent_groups("A 5-storey apartment section with a lift").is_empty());
        let s = intent_groups("Steel lattice tower with bolted connections");
        assert!(s.contains("steel") && s.contains("scripts"), "{s:?}");
    }

    /// The any-geometry briefs (scenarios G01-G30, four languages and Chinese) load `scripts`,
    /// where run_script and its blocks live; ordinary building briefs do not.
    #[test]
    fn freeform_briefs_load_the_script_group_in_four_languages() {
        let briefs = [
            "Build a spiral staircase between two floors, outer radius 1.5 m",
            "Baue eine Wendeltreppe zwischen zwei Geschossen",
            "Construye una escalera de caracol entre dos plantas",
            "Build a ribbed hemispherical dome, 24 m diameter",
            "Baue eine Rippenkuppel als Halbkugel mit 24 m Durchmesser",
            "Construye una cúpula nervada hemisférica",
            "Design a free-form timber gridshell canopy",
            "Entwirf ein Seilnetz-Dach über einem Platz",
            "Diseña una cubierta de red de cables sobre una plaza",
            "A hypar (hyperbolic paraboloid) concrete shell roof",
            "Mache einen verdrehten Büroturm mit 40 Geschossen",
            "Eine Halle mit Kreuzrippengewölben, sechs Joche",
            "Un puente de carretera de acero con arco superior",
            "Modelliere einen Esszimmerstuhl mit vier verjüngten Beinen",
            "Mache eine Stehlampe mit gebogenem Arm",
            "做一个落地灯：圆形底座直径30厘米",
            "建造一座五层塔：每层都有出挑很远的向上翘起的屋檐",
            "Una escultura creada barriendo un perfil triangular redondeado",
            "Ein Hanggrundstück mit Betonstützmauern bis 2.5 m",
            "Un puente de tuberías de acero de 60 m de longitud",
            "A parametric vertical-fin facade, fins 600 mm apart",
            "A tree-like canopy, one trunk branching recursively in 4 generations",
            "Ein Stamm verzweigt sich rekursiv in 4 Generationen",
            "设计一座艺术博物馆，外壳是流线型的双向曲面，像波浪一样，外墙和屋顶都没有直线",
            "A football stadium bowl: pitch 105 x 68 m",
            "Eine Fußballstadion-Schüssel mit Dachring",
            "Un anfiteatro semicircular al aire libre",
            "A geodesic sphere of 30 m diameter, frequency 4",
            "Una esfera geodésica de 30 m de diámetro",
            "A pavilion shaped as a Mobius band",
            "Ein Pavillon in Form eines Möbiusbandes",
            "Un aerogenerador con tres palas de 50 m",
            "Ein Riesenrad mit 50 m Durchmesser",
            "Ein Hochspannungsmast 45 m hoch",
            "A six-level car park with a double helical ramp",
            "Parkhaus mit doppelter Wendelrampe",
            "An organic blob-shaped art museum, double-curved envelope",
            "A parabolic dish antenna 18 m across as a steel gridshell",
            "Eine Parabolantenne mit 18 m Durchmesser als Stahl-Gitterschale",
            "Una antena parabólica de 18 m de diámetro como malla de acero",
        ];
        for b in briefs {
            assert!(intent_groups(b).contains("scripts"), "{b}");
        }
        for b in ru_all("scripts") {
            assert!(intent_groups(&b).contains("scripts"), "{b}");
        }
        assert!(!intent_groups("A 5-storey apartment building with two lifts").contains("scripts"));
        assert!(!intent_groups(&ru("apartment9")).contains("scripts"));
    }

    #[test]
    fn a_stadium_roof_ring_on_masts_loads_the_generators_in_every_language() {
        for b in [
            "A bowl with tribunes and a roof ring on masts",
            ru("stadium_ring").as_str(),
            "Ein Stadion mit Dachring auf Masten",
            "Un estadio con anillo de cubierta sobre mástiles",
        ] {
            assert!(intent_groups(b).contains("scripts"), "{b}");
        }
        assert!(groups().iter().any(|g| g.name == "scripts" && g.tools.iter().any(|t| t == "run_generator")));
        let r = meta_call(HELP, Some(&json!({"topic": "raked_tiers"})), Mode::Propose, true, &mut BTreeSet::new());
        assert!(!r.is_error && r.text.contains("stadium_ring"), "{}", r.text);
    }

    #[test]
    fn load_tools_and_help_answer_from_the_catalog() {
        let mut loaded = BTreeSet::new();
        let a = json!({"groups": ["mep", "steel"]});
        let r = meta_call(LOAD_TOOLS, Some(&a), Mode::Propose, true, &mut loaded);
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("route_mep") && r.text.contains("connect_members"), "{}", r.text);
        assert!(loaded.contains("mep") && loaded.contains("steel"));
        let r = meta_call(LOAD_TOOLS, Some(&json!({"groups": ["pipes"]})), Mode::Propose, true, &mut loaded);
        assert!(r.is_error && r.text.contains("known"), "{}", r.text);
        let r = meta_call(HELP, Some(&json!({"topic": "run_script"})), Mode::Propose, true, &mut loaded);
        assert!(r.text.contains("pattern_radial") && r.text.contains("scripts"), "{}", r.text);
        let r = meta_call(HELP, Some(&json!({"topic": "atrium"})), Mode::Propose, true, &mut loaded);
        assert!(!r.is_error && r.text.contains("void"), "{}", r.text);
        let r = meta_call(HELP, None, Mode::Propose, true, &mut loaded);
        assert!(r.is_error);
    }

    #[test]
    fn help_finds_every_script_recipe_block_and_generator_in_any_wording() {
        let mut loaded = BTreeSet::new();
        let mut ask = |t: &str| meta_call(HELP, Some(&json!({"topic": t})), Mode::Propose, true, &mut loaded);
        for (topic, kind) in [("long_span_roof", "truss_hall"), ("arch_span", "arch_bridge"), ("cantilever_ring", "cantilever_ring")] {
            let r = ask(topic);
            assert!(!r.is_error && r.text.contains(kind), "{topic}: {}", r.text);
        }
        for (name, _) in ddc_script::recipes::RECIPES {
            let r = ask(name);
            assert!(!r.is_error && r.text.contains("\"script\""), "{name}: {}", r.text);
        }
        let hall = ask("long_span_roof").text;
        for w in ["outfit", "roof", "sliding door", "crane runway", "rooms"] {
            assert!(hall.contains(w), "the hangar recipe names the {w}");
        }
        let bridge = ask("arch_span").text;
        assert!(bridge.contains("deck_freq_hz"), "the bridge recipe names the vibration metrics");
        let r = ask("arches_and_vaults");
        assert!(r.text.contains("param(\\\"length\\\""), "params are listed: {}", r.text);
        for b in ["tower", "arch", "bridge", "space_frame", "run_script space_frame", "Space Frame", "surf.dome"] {
            let r = ask(b);
            assert!(!r.is_error && r.text.contains("script block"), "{b}: {}", r.text);
        }
        for g in ddc_gen::registry() {
            let r = ask(g.id());
            assert!(!r.is_error && r.text.contains("\"generator\""), "{}: {}", g.id(), r.text);
        }
        assert!(!ask("run_script").is_error);
        // a miss lists everything with summaries and names the near ones
        let miss = ask("qwerty flange");
        assert!(miss.is_error && miss.text.contains("tiered_frame") && miss.text.contains("arches_and_vaults"), "{}", miss.text);
        let all = ask("all");
        assert!(!all.is_error && all.text.contains("long_span_grid") && all.text.contains("typical_floor"), "{}", all.text);
        // topics are techniques: no help topic is a named object
        for gone in ["lattice_tower", "stadium_ring", "stadium_bowl", "metro_station", "pagoda_tiers", "tied_arch_bridge", "ferris_wheel"] {
            assert!(!all.text.contains(&format!("{gone} [")), "{gone} is still a topic");
        }
    }

    #[test]
    fn the_help_tool_names_what_the_script_crate_and_the_registry_hold() {
        let d = help_schema().description;
        for (name, _) in ddc_script::recipes::RECIPES {
            assert!(d.contains(name), "{name} missing from the help description");
        }
        for g in ddc_gen::registry() {
            assert!(d.contains(g.id()), "{}", g.id());
        }
        for b in ["tower", "space_frame", "bridge", "arch"] {
            assert!(d.contains(b), "{b}");
        }
    }
}
