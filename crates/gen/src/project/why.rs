//! Why the building came out as it did: each decision names the rule, its value, the layer it
//! came from and the effect measured on the built project. The reply of generate/revise shows
//! the first few, `explain(ref)` the whole list of one part. Rule changes proposed by the
//! critic, the refine loop and the brief check are [`RuleFix`]es: a rule and a value in its
//! range, written into the spec through the rule's brief field.

use super::rules::{self, Layer};
use super::why_text::{self, Args};
use super::{GProject, ProjectSpec, RoomKind};
use serde_json::{json, Map, Value as J};

/// One reason: `effect` because `rule` = `value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Why {
    /// The part it explains: `massing`, `grid`, `core`, `facade`, `rooms`, `storey:<n>`,
    /// `basement`, `site`.
    pub target: String,
    pub rule: String,
    pub value: J,
    /// `builtin`, `norm`, `preset`, `project`, `brief`, or `typology` for a value the typology
    /// decided.
    pub source: String,
    /// English, for the tool reply: `key` filled with `args` (see [`super::why_text`]).
    pub effect: String,
    /// Template key: the UI renders `gen.why.e.<key>` with `args` in its own language.
    pub key: &'static str,
    pub args: Args,
}

/// One value as the effect texts print it.
pub(crate) fn s(x: impl std::fmt::Display) -> String {
    x.to_string()
}

impl Why {
    pub fn new(target: String, rule: &str, value: J, source: String, key: &'static str, args: Args) -> Why {
        Why { target, rule: rule.to_string(), value, source, effect: why_text::effect(key, &args), key, args }
    }

    pub fn to_json(&self) -> J {
        json!({"ref": self.target, "rule": self.rule, "value": self.value, "source": self.source,
               "effect": self.effect})
    }
}

/// A proposed rule change: the rule, the value within its range, and the finding behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleFix {
    pub rule: String,
    pub value: J,
    /// English, for the tool reply; `key` filled with `args` when the reason is keyed.
    pub why: String,
    /// Template key of [`super::why_text`]; empty for free text (a brief's or a critic's words).
    pub key: &'static str,
    pub args: Args,
}

impl RuleFix {
    /// A fix after checking the rule exists and the value fits it; `why` is free text.
    pub fn new(rule: &str, value: J, why: &str) -> Result<RuleFix, String> {
        let d = rules::def(rule).ok_or(format!("no rule {rule}"))?;
        let x = d.parse_value(&value).ok_or(format!("{rule}: must be {}", d.takes()))?;
        Ok(RuleFix {
            rule: rule.to_string(),
            value: d.value_json(x),
            why: why.to_string(),
            key: "",
            args: vec![],
        })
    }

    /// A fix whose reason is a keyed template, so the UI renders it in its own language.
    pub fn keyed(rule: &str, value: J, key: &'static str, args: Args) -> Result<RuleFix, String> {
        let mut f = RuleFix::new(rule, value, &why_text::effect(key, &args))?;
        f.key = key;
        f.args = args;
        Ok(f)
    }

    pub fn to_json(&self) -> J {
        let d = rules::def(&self.rule);
        let range = d.map(|d| d.takes()).unwrap_or_default();
        json!({"rule": self.rule, "value": self.value, "range": range, "why": self.why})
    }
}

/// `{rule: value}` of a list of fixes, the later winning.
pub fn rule_patch(fixes: &[RuleFix]) -> J {
    let m: Map<String, J> = fixes.iter().map(|f| (f.rule.clone(), f.value.clone())).collect();
    J::Object(m)
}

/// The fixes as a merge patch on the spec: each rule through its brief field, a rule without
/// one under `rules`.
pub fn spec_patch(fixes: &[RuleFix]) -> J {
    let mut out = json!({});
    for f in fixes {
        match rules::alias_value(&f.rule, &f.value) {
            Some((path, x)) if rules::set_path(&mut out, path, x.clone()) => {}
            _ => {
                out["rules"][&f.rule] = f.value.clone();
            }
        }
    }
    out
}

/// The fixes a spec merge patch stands for: every field that is a rule's brief field.
pub fn fixes_of(patch: &J, why: &str) -> Vec<RuleFix> {
    let mut out = vec![];
    fn walk(v: &J, at: &str, why: &str, out: &mut Vec<RuleFix>) {
        match v {
            J::Object(o) if at == "rules" => {
                for (id, x) in o {
                    out.extend(RuleFix::new(id, x.clone(), why));
                }
            }
            J::Object(o) => {
                for (k, x) in o {
                    let p = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
                    walk(x, &p, why, out);
                }
            }
            x => {
                if let Some(d) = rules::by_alias(at) {
                    out.extend(RuleFix::new(&d.id, x.clone(), why));
                }
            }
        }
    }
    walk(patch, "", why, &mut out);
    out
}

pub(crate) fn source(spec: &ProjectSpec, id: &str) -> String {
    match spec.rules.source(id) {
        Some(Layer::Builtin) if rules::def(id).is_some_and(|d| d.default.is_none()) => "typology".into(),
        Some(l) => match spec.rules.cite(id) {
            Some(c) => format!("{}: {c}", l.name()),
            None => l.name().into(),
        },
        None => "typology".into(),
    }
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Every reason of a built project, most telling first: the massing, the core, the grid, the
/// facade, the rooms, then each storey and the basement.
/// The reasons of the tall building's fire protection, curtain walls and lift count.
fn add_tall<F: FnMut(&str, &str, f64, &'static str, Args)>(add: &mut F, spec: &ProjectSpec, p: &GProject) {
    let f = &p.fire;
    let top = p.storeys.last().map_or(0.0, |t| t.elevation + t.height);
    if f.heads > 0 {
        add("fire", "fire.head_area", spec.rules.value("fire.head_area").unwrap_or_else(|| rules::rule("fire.head_area")),
            "sprinklers", vec![
                ("heads", s(f.heads)), ("cov", s(round(f.covered))), ("area", s(round(f.area))),
                ("per", s(round(f.area / f64::from(f.heads)))), ("risers", s(f.risers.len())),
                ("zones", s(f.zones.len())),
            ]);
    }
    if f.heads > 0 {
        let mut rooms = [0usize; super::fire_check::CLASSES.len()];
        for r in p.storeys.iter().flat_map(|t| &t.rooms).filter(|r| !matches!(r.kind, RoomKind::Stair | RoomKind::Lift)) {
            rooms[super::hazard_of(r.kind).class] += 1;
        }
        for (i, n) in rooms.iter().enumerate().filter(|x| *x.1 > 0) {
            let hz = super::Hazard::class(i);
            let id = if i == super::fire_check::ORDINARY { "fire.head_spacing".to_string() } else { format!("fire.{}.spacing", hz.name()) };
            add("fire", &id, hz.spacing, "sprinkler_hazard", vec![
                ("n", s(n)), ("class", s(hz.name())), ("sp", s(hz.spacing)), ("area", s(hz.area)), ("q", s(round(hz.flow()))),
            ]);
        }
    }
    if !f.standpipes.is_empty() {
        let valves: usize = f.standpipes.iter().map(|x| x.valves.len()).sum();
        add("fire", "fire.standpipe_height", rules::rule("fire.standpipe_height"), "standpipes", vec![
            ("n", s(f.standpipes.len())), ("valves", s(valves)), ("h", s(round(top))),
            ("reach", s(rules::rule("fire.hose_reach"))),
        ]);
    }
    if f.pressurised {
        add("fire", "fire.stair_pressure_height", rules::rule("fire.stair_pressure_height"), "stair_pressure",
            vec![("h", s(round(top)))]);
    }
    if p.lifts.cars > 0 && p.lifts.zones > 0 {
        add("core", "core.lift_traffic_from", rules::rule("core.lift_traffic_from"), "lift_cars", vec![
            ("cars", s(p.lifts.cars)), ("wanted", s(p.lifts.wanted)),
            ("demand", s(round(p.lifts.demand))), ("zones", s(p.lifts.zones)),
        ]);
    }
    if p.lifts.cars > 0 {
        let l = super::lift::summary(super::lift::serve_height(spec));
        let args = vec![
            ("persons", s(l.persons)), ("class", l.class.to_string()),
            ("car", format!("{} x {}", l.car.0, l.car.1)), ("shaft", format!("{:.2} x {:.2}", l.envelope.0, l.envelope.1)),
            ("levels", s(p.storeys.len())), ("pit", s(l.pit)), ("head", s(l.head)),
            ("interval", s(round(p.lifts.interval))),
        ];
        let key = if p.lifts.interval > 0.0 { "lift_family" } else { "lift_family_n" };
        add("core", "core.lift_interval", rules::rule("core.lift_interval"), key, args);
        if p.lifts.fighting > 0 {
            add("core", "fire.fighting_lift_height", rules::rule("fire.fighting_lift_height"), "lift_fighting",
                vec![("load", s(rules::rule("fire.fighting_lift_load"))), ("h", s(round(super::lift::serve_height(spec))))]);
        }
    }
    let bays: Vec<&super::GCurtain> = p.storeys.iter().flat_map(|t| &t.curtains).collect();
    if let Some(c) = bays.first() {
        add("facade", "facade.curtain_module", c.spacing.0, "curtain",
            vec![("n", s(bays.len())), ("m", s(c.spacing.0)), ("sp", s(c.spandrel))]);
    }
    if !bays.is_empty() && spec.features.iter().any(|f| super::brief::asks_curtain_wall(f)) {
        let (cs, cw) = super::brief::curtain_counts(p);
        add("facade", "facade.curtain_from_brief", spec.rules.get("facade.curtain_from_brief"), "curtain_brief",
            vec![("n", s(cw)), ("k", s(cs))]);
    }
}

pub fn explain_project(spec: &ProjectSpec, p: &GProject) -> Vec<Why> {
    let mut out: Vec<Why> = vec![];
    let mut add = |target: &str, rule: &str, value: f64, key: &'static str, args: Args| {
        let d = rules::def(rule);
        let v = d.map_or(json!(value), |d| d.value_json(value));
        out.push(Why::new(target.to_string(), rule, v, source(spec, rule), key, args));
    };
    let rule = |id: &str| spec.rules.value(id).unwrap_or_else(|| rules::rule(id));
    let (w, d) = p.footprint;
    if spec.plate.is_none() && spec.depth.is_none() {
        add("massing", "depth.band", spec.band_depth, "depth_band",
            vec![("d", s(round(d))), ("band", s(spec.band_depth)), ("corr", s(spec.corridor))]);
        add("massing", "corridor.width", spec.corridor, "corridor", vec![("w", s(spec.corridor))]);
    } else {
        add("massing", "corridor.width", spec.corridor, "corridor_fp",
            vec![("w", s(spec.corridor)), ("x", s(round(w))), ("y", s(round(d)))]);
    }
    add("massing", "massing.floor_height", spec.floor_height, "floors", vec![
        ("n", s(p.storeys.len())), ("h", s(spec.floor_height)),
        ("top", s(round(spec.building_height()))),
    ]);
    if spec.plate.is_none() {
        let key = if spec.width.is_some() { "width_asked" } else { "width_fit" };
        add("massing", "massing.width", w, key, vec![("w", s(round(w))), ("d", s(round(d)))]);
    }
    add("massing", "massing.form", super::design::Massing::NAMES
        .iter().position(|n| *n == spec.design.massing.name()).unwrap_or(0) as f64,
        "massing_form", vec![("form", s(spec.design.massing.name())), ("wings", s(p.copies))]);
    for (id, x, key) in [
        ("massing.setback", spec.design.setback, "setback"),
        ("massing.cantilever", spec.design.cantilever, "cantilever"),
        ("facade.curve_s", spec.design.curve, "curve_s"),
        ("facade.curve_n", spec.design.curve_north, "curve_n"),
    ] {
        if x > 0.0 {
            add("massing", id, x, key, vec![("x", s(x))]);
        }
    }
    if let Some(sh) = &p.shell {
        let r = &sh.report;
        add("massing", "massing.shell_patch", super::rules::rule("massing.shell_patch"), "shell", vec![
            ("n", s(sh.skin.len() + sh.roof.len())), ("k", s(r.double_curved + sh.roof.len())),
            ("p", s(r.panels)), ("flat", s(r.flat)), ("cold", s(r.cold)), ("hot", s(r.hot)),
            ("gap", s(round(r.gap * 1000.0))), ("oc", s(round(sh.oculus))),
        ]);
    }
    let ground = p.storeys.first().map(|s| &s.layout);
    if let Some(l) = ground {
        let stairs = l.stairs.len();
        if stairs > 0 {
            add("core", "core.stair_width", spec.stair_width, "stairs",
                vec![("n", s(stairs)), ("w", s(spec.stair_width)), ("r", s(spec.max_riser))]);
            // the country limits the stair was held to, each with its clause; the line above
            // already cites core.stair_width
            let width_rule = super::spec::stair_width_rule(spec.typology);
            for (id, key, built) in [
                (width_rule, "norm_flight", spec.stair_width),
                ("core.max_riser", "norm_riser", spec.max_riser),
            ] {
                if id != "core.stair_width" && spec.rules.source(id) == Some(Layer::Norm) {
                    let lim = rule(id);
                    add("core", id, lim, key, vec![("lim", s(lim)), ("built", s(built))]);
                }
            }
            add("core", "core.door_zone", rule("core.door_zone"), "door_zone", vec![]);
        }
        if !l.shafts.is_empty() {
            add("core", "core.lift_room", rule("core.lift_room"), "lift_room",
                vec![("n", s(l.shafts.len())), ("w", s(rule("core.lift_room")))]);
        }
        add_tall(&mut add, spec, p);
        add("grid", "grid.span", spec.grid, "grid_span", vec![
            ("axes", s(l.grids.len())), ("span", s(spec.grid)),
            ("cols", s(l.columns.len())), ("col", s(rule("grid.column"))),
        ]);
        let measure = |id: &str| p.metrics.iter().find(|m| m.0 == id).map(|m| m.1);
        if let (Some(form), Some(usable)) = (measure("grid_form"), measure("usable_rooms")) {
            let name = rules::def("grid.form").and_then(|d| d.options.get(form as usize).cloned()).unwrap_or_default();
            add("grid", "grid.form", form, "grid_form", vec![("form", name)]);
            add("grid", "grid.min_usable", rule("grid.min_usable"), "grid_usable", vec![("pct", s(round(usable)))]);
        }
    }
    let openings = || p.storeys.iter().flat_map(|s| &s.layout.openings);
    let windows = openings().filter(|o| !o.door).count();
    let glass: f64 = openings().filter(|o| !o.door).map(|o| o.width * o.height).sum();
    add("facade", "facade.glazing_share", rule("facade.glazing_share"), "glazing",
        vec![("n", s(windows)), ("glass", s(round(glass)))]);
    add("facade", "facade.edge_clear", rule("facade.edge_clear"), "edge_clear", vec![]);
    for (id, key) in [("room.door", "doors"), ("room.door_wet", "doors_wet"), ("room.door_wide", "doors_wide")] {
        let x = rule(id);
        let n = openings().filter(|o| o.door && (o.width - x).abs() < 1e-6).count();
        if n > 0 {
            add("rooms", id, x, key, vec![("n", s(n)), ("w", s(x))]);
        }
    }
    let narrow = p
        .storeys
        .iter()
        .flat_map(|s| &s.rooms)
        .filter(|r| r.kind.habitable())
        .count();
    add("rooms", "room.min_unit", rule("room.min_unit"), "min_unit", vec![("n", s(narrow))]);
    for (k, st) in p.storeys.iter().enumerate() {
        add(&format!("storey:{k}"), "massing.floor_height", spec.storey_height(k), "storey",
            vec![("k", s(k)), ("rooms", s(st.rooms.len())), ("h", s(spec.storey_height(k)))]);
    }
    if !p.basements.is_empty() {
        let kinds = |k: RoomKind| p.basements.iter().flat_map(|s| &s.rooms).filter(|r| r.kind == k).count();
        add("basement", "basement.count", p.basements.len() as f64, "basement_count", vec![("n", s(p.basements.len()))]);
        add("basement", "basement.park_min", rule("basement.park_min"), "basement_park",
            vec![("park", s(kinds(RoomKind::Parking))), ("store", s(kinds(RoomKind::Storage)))]);
    }
    if spec.design.trees > 0 || spec.design.paving > 0.0 {
        add("site", "site.paving", spec.design.paving, "paving",
            vec![("w", s(spec.design.paving)), ("trees", s(spec.design.trees))]);
    }
    if spec.design.trees > 0 {
        add("site", "site.tree_mix", rule("site.tree_mix"), "tree_mix",
            vec![("n", s(spec.design.trees))]);
        add("site", "site.tree_season", rule("site.tree_season"), "tree_season",
            vec![("n", s(spec.design.trees))]);
    }
    for (id, x, key, args) in super::openings::whys(spec, p) {
        add("openings", id, x, key, args);
    }
    if (spec.design.trees > 0 || spec.design.paving > 0.0) && rule("site.landscape") > 0.5 {
        let fx = &p.storeys[0].layout.fixtures;
        let count = |f: &str, ty: Option<&str>| fx.iter().filter(|x| x.family == f && ty.map_or(true, |t| x.ty == t)).count();
        add("site", "site.landscape", rule("site.landscape"), "landscape",
            vec![("hedge", s(count("Hedge", None))), ("shrubs", s(count("Shrub", None))),
                 ("lawn", s(count("Lawn", Some("Lawn")))), ("paths", s(count("Paving Slab", Some("Path"))))]);
        if spec.design.trees > 0 {
            add("site", "site.tree_setback", rule("site.tree_setback"), "tree_setback",
                vec![("n", s(count("Tree", None))), ("m", s(rule("site.tree_setback"))),
                     ("c", s(rule("site.tree_path_clear")))]);
            if rule("site.tree_rows") > 0.5 {
                let rows = count("Tree", None).saturating_sub(spec.design.trees as usize);
                add("site", "site.tree_rows", rule("site.tree_rows"), "tree_rows",
                    vec![("n", s(rows)), ("gap", s(rule("site.row_spacing")))]);
            }
        }
    }
    for (id, x, key, args) in super::site::whys(spec, p) {
        add("site", id, x, key, args);
    }
    for (id, x, key, args) in super::mech::whys(spec, p) {
        add("hvac", id, x, key, args);
    }
    for (id, x, key, args) in super::assembly::whys(spec, p) {
        add("assembly", id, x, key, args);
    }
    out.extend(super::stack::explain(spec));
    out.extend(super::fire::explain(spec, p));
    out.extend(super::typology::why::explain(spec, p));
    out
}

/// The goals that cost something, as reasons under `score`: what was measured against the
/// rule's value and what the miss costs in its tier.
pub fn trade_offs(spec: &ProjectSpec, p: &GProject) -> Vec<Why> {
    super::score::score(spec, p)
        .limiting()
        .into_iter()
        .map(|c| {
            let tail = [("cost", s(round(c.cost))), ("tier", s(c.tier.name())), ("weight", s(c.weight))];
            let (key, mut args): (&'static str, Args) = if c.of > 0 {
                ("cost_items", vec![("failed", s(c.failed)), ("of", s(c.of))])
            } else {
                ("cost_measured", vec![("m", s(round(c.measured)))])
            };
            args.extend(tail);
            Why::new("score".into(), &c.rule, json!(c.target), source(spec, &c.rule), key, args)
        })
        .collect()
}

/// The reasons of one part: `ref` is a target (`core`, `storey:2`, `score`) or a rule id.
pub fn explain(spec: &ProjectSpec, p: &GProject, r: &str) -> Vec<Why> {
    explain_project(spec, p)
        .into_iter()
        .chain(trade_offs(spec, p))
        .filter(|w| w.target == r || w.rule == r || (r == "storey" && w.target.starts_with("storey:")))
        .collect()
}

/// The first `n` reasons as JSON, for a tool reply.
pub fn top(spec: &ProjectSpec, p: &GProject, n: usize) -> J {
    J::Array(explain_project(spec, p).iter().take(n).map(Why::to_json).collect())
}
