// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Layered build-ups of walls, slabs and roofs. The catalogue (`data/assemblies.json`) holds
//! materials and build-ups as data; the rules of each use pick one: the exterior wall, roof and
//! ground slab size their insulation to the U-value rule, the party wall and the floor between
//! storeys size their core to the sound rule by the mass law, and the insulation of the
//! envelope keeps to the fire class rule. The thickness of a wall role and of a roof is the
//! total of its build-up, so the plan geometry follows the chosen layers.

use std::sync::OnceLock;

use serde_json::Value as J;

use super::design::Structure;
use super::nav::{finding, Finding};
use super::rules::{self, RuleSet};
use super::why_text::Args;
use super::{GProject, PStorey, ProjectSpec};
use crate::{GLayout, WallRole};

const DATA: &str = include_str!("../../data/assemblies.json");
pub const SCHEMA: &str = "gen_assemblies.v1";

/// Worst reaction-to-fire class the envelope insulation may have (a choice rule).
pub const CLASS_RULE: &str = "fire.insulation_class";
/// The project's fire rating (`params.fire_rating`, such as `REI 60`) a bearing build-up meets.
pub const FIRE_RATING: &str = "fire_rating";
const MM: f64 = 1000.0;
/// Float noise of a plan point on a wall axis.
const ON_AXIS: f64 = 1e-6;

/// What a build-up is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Use {
    ExteriorWall,
    /// The outside wall of a level below ground: waterproofing and reinforced concrete.
    RetainingWall,
    PartyWall,
    /// The wall between a dwelling and the common route: the core, concrete where the system is timber.
    CoreWall,
    Partition,
    WetPartition,
    IntermediateSlab,
    GroundSlab,
    FlatRoof,
    PitchedRoof,
}

impl Use {
    pub const ALL: [Use; 10] = [
        Use::ExteriorWall,
        Use::RetainingWall,
        Use::PartyWall,
        Use::CoreWall,
        Use::Partition,
        Use::WetPartition,
        Use::IntermediateSlab,
        Use::GroundSlab,
        Use::FlatRoof,
        Use::PitchedRoof,
    ];

    /// The catalogue's name of the use.
    pub fn key(self) -> &'static str {
        match self {
            Use::ExteriorWall => "exterior_wall",
            Use::RetainingWall => "retaining_wall",
            Use::PartyWall => "party_wall",
            Use::CoreWall => "core_wall",
            Use::Partition => "partition",
            Use::WetPartition => "wet_partition",
            Use::IntermediateSlab => "intermediate_slab",
            Use::GroundSlab => "ground_slab",
            Use::FlatRoof => "flat_roof",
            Use::PitchedRoof => "pitched_roof",
        }
    }

    pub fn parse(s: &str) -> Option<Use> {
        Use::ALL.into_iter().find(|u| u.key() == s)
    }

    /// The use of a wall role before wet rooms are looked at: a corridor wall separates a
    /// dwelling from the common route and is the core's wall.
    pub fn of_role(role: WallRole) -> Use {
        match role {
            WallRole::Exterior => Use::ExteriorWall,
            WallRole::Retaining => Use::RetainingWall,
            WallRole::Party => Use::PartyWall,
            WallRole::Corridor => Use::CoreWall,
            WallRole::Partition => Use::Partition,
        }
    }

    pub fn is_wall(self) -> bool {
        matches!(self, Use::ExteriorWall | Use::RetainingWall | Use::PartyWall | Use::CoreWall | Use::Partition | Use::WetPartition)
    }

    /// The surface resistances its U-value takes; `None`: no thermal rule.
    fn surface(self) -> Option<&'static str> {
        match self {
            Use::ExteriorWall => Some("wall"),
            Use::FlatRoof | Use::PitchedRoof => Some("roof"),
            Use::GroundSlab => Some("ground"),
            _ => None,
        }
    }

    pub fn u_rule(self) -> Option<&'static str> {
        match self {
            Use::ExteriorWall => Some("envelope.u_wall"),
            Use::FlatRoof | Use::PitchedRoof => Some("envelope.u_roof"),
            Use::GroundSlab => Some("envelope.u_ground"),
            _ => None,
        }
    }

    /// The rule that sets the least total thickness of the build-up, m.
    pub fn min_rule(self) -> Option<&'static str> {
        match self {
            Use::RetainingWall => Some("underground.retaining_thickness"),
            _ => None,
        }
    }

    pub fn rw_rule(self) -> Option<&'static str> {
        match self {
            Use::PartyWall | Use::CoreWall => Some("acoustic.rw_party"),
            Use::IntermediateSlab => Some("acoustic.rw_floor"),
            _ => None,
        }
    }

    /// Insulation of the envelope keeps to [`CLASS_RULE`]; a slab on the ground is not envelope
    /// a fire spreads along.
    fn class_ruled(self) -> bool {
        matches!(self, Use::ExteriorWall | Use::FlatRoof | Use::PitchedRoof)
    }

    /// Carries load, so it is held to the project's fire rating.
    pub fn bearing(self) -> bool {
        matches!(self, Use::ExteriorWall | Use::RetainingWall | Use::PartyWall | Use::CoreWall | Use::IntermediateSlab | Use::GroundSlab | Use::FlatRoof)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub id: String,
    pub name: String,
    /// Thermal conductivity, W/(m K).
    pub lambda: f64,
    /// kg/m3.
    pub density: f64,
    /// Reaction-to-fire class, one of the options of [`CLASS_RULE`].
    pub fire: String,
    /// structure, substrate, insulation, membrane or finish.
    pub function: String,
}

#[derive(Clone, Debug)]
struct DefLayer {
    material: usize,
    /// Fixed thickness, mm; empty `sizes`.
    mm: f64,
    /// Sizes the generator picks from, mm, ascending.
    sizes: Vec<f64>,
}

#[derive(Clone, Debug)]
struct Def {
    id: String,
    name: String,
    use_: Use,
    /// Structural systems it is built for; the concrete ones serve a system with none of its own.
    structures: Vec<Structure>,
    fire_minutes: u32,
    layers: Vec<DefLayer>,
}

#[derive(Clone, Debug)]
struct Surface {
    rsi: f64,
    rse: f64,
}

/// The board that raises the fire resistance of a build-up: layer thickness, minutes each adds.
#[derive(Clone, Debug)]
struct Board {
    material: usize,
    mm: f64,
    minutes: u32,
    max: usize,
}

#[derive(Debug)]
struct Catalogue {
    board: Board,
    materials: Vec<Material>,
    defs: Vec<Def>,
    wall: Surface,
    roof: Surface,
    ground: Surface,
    mass_a: f64,
    mass_b: f64,
    pitched_from: f64,
}

fn num(v: &J, k: &str, at: &str) -> Result<f64, String> {
    v[k].as_f64().filter(|x| x.is_finite()).ok_or(format!("{at}: {k} must be a number"))
}

fn text(v: &J, k: &str, at: &str) -> Result<String, String> {
    v[k].as_str().map(str::to_string).ok_or(format!("{at}: {k} must be text"))
}

fn parse(src: &str) -> Result<Catalogue, String> {
    let v: J = serde_json::from_str(src).map_err(|e| e.to_string())?;
    if v["schema"] != SCHEMA {
        return Err(format!("schema must be {SCHEMA}"));
    }
    let classes = rules::def(CLASS_RULE).map(|d| d.options.clone()).unwrap_or_default();
    let mut materials: Vec<Material> = vec![];
    for m in v["materials"].as_array().ok_or("materials must be a list")? {
        let id = text(m, "id", "material")?;
        let at = format!("material {id}");
        let mat = Material {
            name: text(m, "name", &at)?,
            lambda: num(m, "lambda", &at)?,
            density: num(m, "density", &at)?,
            fire: text(m, "fire", &at)?,
            function: text(m, "function", &at)?,
            id,
        };
        if mat.lambda <= 0.0 || mat.density <= 0.0 {
            return Err(format!("{at}: lambda and density must be positive"));
        }
        if !classes.contains(&mat.fire) {
            return Err(format!("{at}: fire class {} is not one of {classes:?}", mat.fire));
        }
        if materials.iter().any(|x| x.id == mat.id || x.name == mat.name) {
            return Err(format!("{at}: twice"));
        }
        materials.push(mat);
    }
    let mut defs: Vec<Def> = vec![];
    for a in v["assemblies"].as_array().ok_or("assemblies must be a list")? {
        let id = text(a, "id", "assembly")?;
        let at = format!("assembly {id}");
        let use_ = Use::parse(&text(a, "use", &at)?).ok_or(format!("{at}: unknown use"))?;
        let mut layers = vec![];
        for l in a["layers"].as_array().ok_or(format!("{at}: layers must be a list"))? {
            let mid = text(l, "material", &at)?;
            let material = materials.iter().position(|m| m.id == mid).ok_or(format!("{at}: no material {mid}"))?;
            let sizes: Vec<f64> = l["sizes"].as_array().into_iter().flatten().filter_map(J::as_f64).collect();
            let mm = if sizes.is_empty() { num(l, "mm", &at)? } else { 0.0 };
            if sizes.windows(2).any(|w| w[1] <= w[0]) || sizes.iter().chain([&mm]).any(|x| *x < 0.0) {
                return Err(format!("{at}: sizes must rise and thicknesses be positive"));
            }
            if sizes.is_empty() && mm <= 0.0 {
                return Err(format!("{at}: a layer without thickness"));
            }
            layers.push(DefLayer { material, mm, sizes });
        }
        if layers.iter().filter(|l| !l.sizes.is_empty()).count() > 1 || layers.is_empty() {
            return Err(format!("{at}: one sized layer at most, and at least one layer"));
        }
        let fire_minutes = a["fire_minutes"].as_u64().ok_or(format!("{at}: fire_minutes"))? as u32;
        let mut structures = vec![];
        for n in a["structure"].as_array().map_or(vec![J::from("concrete")], |x| x.clone()) {
            let n = n.as_str().unwrap_or_default();
            let k = Structure::NAMES.iter().position(|x| *x == n).ok_or(format!("{at}: unknown structure {n}"))?;
            structures.push(Structure::ALL[k]);
        }
        defs.push(Def { id, name: text(a, "name", &at)?, use_, structures, fire_minutes, layers });
    }
    for u in Use::ALL {
        if !defs.iter().any(|d| d.use_ == u && d.structures.contains(&Structure::Concrete)) {
            return Err(format!("no build-up for {}", u.key()));
        }
    }
    let ph = &v["physics"];
    let surface = |k: &str| -> Result<Surface, String> {
        let s = &ph["surface"][k];
        Ok(Surface { rsi: num(s, "rsi", k)?, rse: num(s, "rse", k)? })
    };
    let fb = &ph["fire_board"];
    let bid = text(fb, "material", "fire_board")?;
    let board = Board {
        material: materials.iter().position(|m| m.id == bid).ok_or(format!("fire_board: no material {bid}"))?,
        mm: num(fb, "mm", "fire_board")?,
        minutes: num(fb, "minutes", "fire_board")? as u32,
        max: num(fb, "max", "fire_board")? as usize,
    };
    if board.mm <= 0.0 || board.minutes == 0 {
        return Err("fire_board: thickness and minutes must be positive".into());
    }
    Ok(Catalogue {
        board,
        materials,
        defs,
        wall: surface("wall")?,
        roof: surface("roof")?,
        ground: surface("ground")?,
        mass_a: num(&ph["mass_law"], "a", "mass_law")?,
        mass_b: num(&ph["mass_law"], "b", "mass_law")?,
        pitched_from: num(ph, "pitched_from_deg", "physics")?,
    })
}

fn catalogue() -> &'static Catalogue {
    static C: OnceLock<Catalogue> = OnceLock::new();
    C.get_or_init(|| parse(DATA).unwrap_or_else(|e| panic!("assemblies.json: {e}")))
}

/// Every material of the catalogue.
pub fn materials() -> &'static [Material] {
    &catalogue().materials
}

/// Ids of the catalogue's build-ups of one use, in file order.
pub fn catalogue_ids(u: Use) -> Vec<&'static str> {
    catalogue().defs.iter().filter(|d| d.use_ == u).map(|d| d.id.as_str()).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub material: Material,
    /// mm.
    pub mm: f64,
}

impl Layer {
    pub fn thickness(&self) -> f64 {
        self.mm / MM
    }
}

/// A rule a chosen build-up does not meet: what it reaches against the limit.
#[derive(Clone, Debug, PartialEq)]
pub struct Miss {
    pub rule: &'static str,
    pub value: f64,
    pub limit: f64,
}

/// A build-up chosen for a use, sized.
#[derive(Clone, Debug, PartialEq)]
pub struct Assembly {
    pub id: String,
    pub name: String,
    pub use_: Use,
    /// Outside (or top) first.
    pub layers: Vec<Layer>,
    pub fire_minutes: u32,
    /// Heat transmittance, W/(m2 K), for a use with a thermal rule.
    pub u: Option<f64>,
    /// Mass-law sound reduction index, dB, for a use with a sound rule.
    pub rw: Option<f64>,
    /// kg/m2.
    pub mass: f64,
    /// Worst reaction-to-fire class of its insulation layers, as an option index of
    /// [`CLASS_RULE`]; `None` without insulation.
    pub insulation_class: Option<usize>,
    pub misses: Vec<Miss>,
}

impl Assembly {
    /// Total thickness, m: the sum of the layers in millimetres, so whole-millimetre build-ups
    /// give the exact metre value.
    pub fn thickness(&self) -> f64 {
        self.layers.iter().map(|l| l.mm).sum::<f64>() / MM
    }

    /// Thickness of the insulation layers, m.
    pub fn insulation(&self) -> f64 {
        self.layers.iter().filter(|l| l.material.function == "insulation").map(|l| l.mm).sum::<f64>() / MM
    }

    /// The type name a model gives it: the build-up's name and its total in millimetres.
    pub fn type_name(&self) -> String {
        format!("Gen {} {}", self.name, ddc_geom::to_mm(self.thickness()))
    }

    /// The same build-up listed inside first, for a wall whose outside lies on its left.
    pub fn reversed(&self) -> Assembly {
        let mut a = self.clone();
        a.layers.reverse();
        a.name = format!("{} inside first", a.name);
        a
    }
}

fn class_index(class: &str) -> usize {
    let opts = rules::def(CLASS_RULE).map(|d| d.options.as_slice()).unwrap_or_default();
    opts.iter().position(|o| o == class).unwrap_or(opts.len())
}

/// The build-up `d` with its sized layer at `size` mm, measured against the limits.
fn build(c: &Catalogue, d: &Def, size: Option<f64>, boards: usize, get: &dyn Fn(&str) -> f64) -> Assembly {
    let mut layers: Vec<Layer> = d
        .layers
        .iter()
        .map(|l| Layer {
            material: c.materials[l.material].clone(),
            mm: if l.sizes.is_empty() { l.mm } else { size.unwrap_or(l.mm) },
        })
        .collect();
    // fire boards go on the inside face: last for a wall listed outside first
    for _ in 0..boards {
        layers.push(Layer { material: c.materials[c.board.material].clone(), mm: c.board.mm });
    }
    let fire_minutes = d.fire_minutes + boards as u32 * c.board.minutes;
    let u = d.use_.surface().map(|s| {
        let s = match s {
            "wall" => &c.wall,
            "roof" => &c.roof,
            _ => &c.ground,
        };
        let r: f64 = layers.iter().map(|l| l.thickness() / l.material.lambda).sum();
        1.0 / (s.rsi + r + s.rse)
    });
    let mass: f64 = layers.iter().map(|l| l.thickness() * l.material.density).sum();
    let rw = d.use_.rw_rule().map(|_| c.mass_a * mass.log10() + c.mass_b);
    let insulation_class =
        layers.iter().filter(|l| l.material.function == "insulation").map(|l| class_index(&l.material.fire)).max();
    let mut misses = vec![];
    if let (Some(rule), Some(u)) = (d.use_.u_rule(), u) {
        let limit = get(rule);
        if u > limit + 1e-9 {
            misses.push(Miss { rule, value: u, limit });
        }
    }
    if let (Some(rule), Some(rw)) = (d.use_.rw_rule(), rw) {
        let limit = get(rule);
        if rw < limit - 1e-9 {
            misses.push(Miss { rule, value: rw, limit });
        }
    }
    if let Some(rule) = d.use_.min_rule() {
        let (t, limit) = (layers.iter().map(|l| l.mm).sum::<f64>() / MM, get(rule));
        if t < limit - 1e-9 {
            misses.push(Miss { rule, value: t, limit });
        }
    }
    if d.use_.class_ruled() {
        let worst = get(CLASS_RULE);
        if let Some(k) = insulation_class.filter(|k| *k as f64 > worst) {
            misses.push(Miss { rule: CLASS_RULE, value: k as f64, limit: worst });
        }
    }
    Assembly {
        id: d.id.clone(),
        name: if boards > 0 { format!("{} EI{fire_minutes}", d.name) } else { d.name.clone() },
        use_: d.use_,
        layers,
        fire_minutes,
        u,
        rw,
        mass,
        insulation_class,
        misses,
    }
}

/// The build-up of use `u` under the rule values `get` gives: of the catalogue's build-ups of
/// the use, each at the smallest size that meets every rule, the thinnest (file order on a
/// tie). When none meets them, the first one that keeps the fire class, at its largest size,
/// carries the misses.
pub fn pick(u: Use, get: &dyn Fn(&str) -> f64) -> Assembly {
    let (sys, want) = in_force_project();
    rated(u, sys, if u.bearing() { want } else { 0 }, get)
}

/// [`pick_with`] with as few layers of fire board added as reach `want` minutes.
fn rated(u: Use, sys: Structure, want: u32, get: &dyn Fn(&str) -> f64) -> Assembly {
    let max = catalogue().board.max;
    let mut a = pick_with(u, sys, 0, get);
    for n in 1..=max {
        if a.fire_minutes >= want {
            break;
        }
        a = pick_with(u, sys, n, get);
    }
    a
}

/// [`pick`] for a structural system, with `boards` layers of fire board on the inside face. A
/// system with no build-up of its own for the use takes the concrete one.
fn pick_with(u: Use, sys: Structure, boards: usize, get: &dyn Fn(&str) -> f64) -> Assembly {
    let c = catalogue();
    let sys = if c.defs.iter().any(|d| d.use_ == u && d.structures.contains(&sys)) { sys } else { Structure::Concrete };
    let mut best: Option<Assembly> = None;
    let mut fallback: Option<Assembly> = None;
    for d in c.defs.iter().filter(|d| d.use_ == u && d.structures.contains(&sys)) {
        let sizes: Vec<Option<f64>> = match d.layers.iter().find(|l| !l.sizes.is_empty()) {
            Some(l) => l.sizes.iter().copied().map(Some).collect(),
            None => vec![None],
        };
        let met = sizes.iter().map(|s| build(c, d, *s, boards, get)).find(|a| a.misses.is_empty());
        match met {
            Some(a) => {
                if best.as_ref().is_none_or(|b| a.thickness() < b.thickness() - 1e-9) {
                    best = Some(a);
                }
            }
            None => {
                let a = build(c, d, sizes.last().copied().flatten(), boards, get);
                let keeps_class = !a.misses.iter().any(|m| m.rule == CLASS_RULE);
                let better = match &fallback {
                    None => true,
                    Some(f) => keeps_class && f.misses.iter().any(|m| m.rule == CLASS_RULE),
                };
                if better {
                    fallback = Some(a);
                }
            }
        }
    }
    best.or(fallback).unwrap_or_else(|| panic!("assemblies.json: no build-up for {}", u.key()))
}

/// The build-up of `u` under a project's rule set.
pub fn for_rules(rs: &RuleSet, u: Use) -> Assembly {
    pick(u, &|id| rs.get(id))
}

/// [`for_rules`] for the project's structural system.
pub fn for_rules_in(rs: &RuleSet, u: Use, sys: Structure) -> Assembly {
    pick_with(u, sys, 0, &|id| rs.get(id))
}

/// The build-up of `u` whose layers reach `want` minutes of fire resistance: the chosen one with
/// as few layers of fire board added inside as it takes. Short of `want` it is the most the
/// board limit allows, and its `fire_minutes` say so.
pub fn rated_in(rs: &RuleSet, u: Use, sys: Structure, want: u32) -> Assembly {
    rated(u, sys, want, &|id| rs.get(id))
}

/// The build-up a project holds for `u`: its system's, and a bearing one rated to the project's
/// fire rating.
pub fn chosen(spec: &ProjectSpec, u: Use) -> Assembly {
    rated_in(&spec.rules, u, spec.design.structure, if u.bearing() { project_rating(spec) } else { 0 })
}

/// Minutes of the project's fire rating, 0 when it names none.
pub fn project_rating(spec: &ProjectSpec) -> u32 {
    rating_minutes(&spec.fire_rating).unwrap_or(0)
}

thread_local! {
    static SYSTEM: std::cell::Cell<(Structure, u32)> = const { std::cell::Cell::new((Structure::Concrete, 0)) };
}

/// Runs `f` with `sys` as the structural system and `want` minutes as the fire rating the
/// bearing build-ups and the wall thicknesses follow.
pub fn with_structure<T>(sys: Structure, want: u32, f: impl FnOnce() -> T) -> T {
    struct Restore((Structure, u32));
    impl Drop for Restore {
        fn drop(&mut self) {
            SYSTEM.with(|s| s.set(self.0));
        }
    }
    let _back = Restore(SYSTEM.with(|s| s.replace((sys, want))));
    f()
}

fn in_force_project() -> (Structure, u32) {
    SYSTEM.with(|s| s.get())
}

/// The build-up of `u` under the rules in force on this thread (the project being built).
pub fn in_force(u: Use) -> Assembly {
    pick(u, &rules::rule)
}

/// Thickness of the build-up of `u` under the rules in force, remembered per rule values: the
/// plan geometry asks for it at every wall.
fn thickness_in_force(u: Use) -> f64 {
    type Key = (Use, usize, u32, [u64; 4]);
    thread_local! {
        static SEEN: std::cell::RefCell<std::collections::HashMap<Key, f64>> = Default::default();
    }
    let bits = |r: Option<&str>| r.map_or(0, |id| rules::rule(id).to_bits());
    let class = if u.class_ruled() { Some(CLASS_RULE) } else { None };
    let (system, want) = in_force_project();
    let sys = Structure::ALL.iter().position(|s| *s == system).unwrap_or(0);
    let key = (u, sys, if u.bearing() { want } else { 0 }, [bits(u.u_rule()), bits(u.rw_rule()), bits(class), bits(u.min_rule())]);
    if let Some(t) = SEEN.with(|s| s.borrow().get(&key).copied()) {
        return t;
    }
    let t = in_force(u).thickness();
    SEEN.with(|s| s.borrow_mut().insert(key, t));
    t
}

/// Thickness of a wall role under the rules in force.
pub fn role_thickness(role: WallRole) -> f64 {
    thickness_in_force(Use::of_role(role))
}

/// A roof of this slope (degrees) is pitched, not flat.
pub fn pitched(slope: f64) -> bool {
    slope >= catalogue().pitched_from
}

pub fn roof_use(slope: f64) -> Use {
    if pitched(slope) {
        Use::PitchedRoof
    } else {
        Use::FlatRoof
    }
}

/// Thickness of a roof of this slope under the rules in force.
pub fn roof_thickness(slope: f64) -> f64 {
    thickness_in_force(roof_use(slope))
}

/// Thickness of the floor between storeys under the rules in force.
pub fn floor_thickness() -> f64 {
    thickness_in_force(Use::IntermediateSlab)
}

/// The build-up a wall of a storey takes, and whether its layers run inside first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallUse {
    pub use_: Use,
    /// An exterior wall whose outside lies on the left of a -> b: the model stacks the first
    /// layer on the right face, so its layers are listed inside first.
    pub reversed: bool,
}

fn inside(p: (f64, f64), poly: &[(f64, f64)]) -> bool {
    let n = poly.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            c = !c;
        }
    }
    c
}

/// The floor of the layout covers `p`.
fn on_slab(l: &GLayout, p: (f64, f64)) -> bool {
    inside(p, &l.slab) && !l.slab_holes.iter().any(|h| inside(p, h))
}

/// The outside of exterior wall `k` lies on its left: the point a wall thickness to the left of
/// its middle is off the floor while the one to the right is on it.
fn outside_left(l: &GLayout, k: usize) -> bool {
    let w = &l.walls[k];
    let t = w.length() / 2.0;
    let (m, u) = (w.at(t), w.tangent(t));
    let d = w.role.thickness();
    let left = (m.0 - u.1 * d, m.1 + u.0 * d);
    let right = (m.0 + u.1 * d, m.1 - u.0 * d);
    !on_slab(l, left) && on_slab(l, right)
}

/// A straight partition bounds a bath or a WC: some edge of the room's outline lies along it.
fn bounds_wet(s: &PStorey, k: usize) -> bool {
    let l = &s.layout;
    let w = &l.walls[k];
    if w.bulge != 0.0 {
        return false;
    }
    l.rooms.iter().zip(&s.rooms).filter(|(_, m)| super::stack::is_wet(m.kind)).any(|(r, _)| {
        let n = r.polygon.len();
        (0..n).any(|i| {
            let (a, b) = (r.polygon[i], r.polygon[(i + 1) % n]);
            w.dist(a) < ON_AXIS && w.dist(b) < ON_AXIS && (w.along(a) - w.along(b)).abs() > ON_AXIS
        })
    })
}

/// The build-up of every wall of a storey, in wall order.
pub fn wall_uses(s: &PStorey) -> Vec<WallUse> {
    let l = &s.layout;
    (0..l.walls.len())
        .map(|k| {
            let role = l.walls[k].role;
            let use_ = match Use::of_role(role) {
                Use::Partition if bounds_wet(s, k) => Use::WetPartition,
                u => u,
            };
            WallUse { use_, reversed: matches!(role, WallRole::Exterior | WallRole::Retaining) && outside_left(l, k) }
        })
        .collect()
}

/// The uses a built project holds, in [`Use::ALL`] order.
pub fn uses_of(p: &GProject) -> Vec<Use> {
    let mut out = vec![];
    for s in p.storeys.iter().chain(&p.basements) {
        out.extend(wall_uses(s).into_iter().map(|w| w.use_));
    }
    if !p.storeys.is_empty() {
        out.push(Use::GroundSlab);
    }
    if p.storeys.len() + p.basements.len() > 1 {
        out.push(Use::IntermediateSlab);
    }
    for r in std::iter::once(&p.roof).chain(&p.extra_roofs) {
        if r.ring.len() >= 3 {
            out.push(roof_use(r.slope));
        }
    }
    // a stair head on a pitched roof is covered flat
    if !p.fire.stair_heads.is_empty() {
        out.push(roof_use(0.0));
    }
    out.sort();
    out.dedup();
    out
}

/// Minutes of a fire rating such as `REI 60`; `None` when it names none.
pub fn rating_minutes(rating: &str) -> Option<u32> {
    let digits: String = rating.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn round2(x: f64) -> String {
    format!("{x:.2}")
}

fn round1(x: f64) -> String {
    format!("{x:.1}")
}

/// Findings of the build-ups a project holds: a U-value, a sound index or a fire class the
/// catalogue cannot reach, and a bearing build-up rated below the project's fire rating.
pub fn check(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    rated_walls(spec, p, out);
    let want = rating_minutes(&spec.fire_rating);
    for u in uses_of(p) {
        let a = chosen(spec, u);
        for m in &a.misses {
            let (excess, msg) = match m.rule {
                CLASS_RULE => {
                    let opts = rules::def(CLASS_RULE).map(|d| d.options.clone()).unwrap_or_default();
                    let name = |k: f64| opts.get(k as usize).cloned().unwrap_or_default();
                    let msg = format!(
                        "{} {}: insulation class {} is worse than {}",
                        u.key(),
                        a.id,
                        name(m.value),
                        name(m.limit)
                    );
                    (m.value - m.limit, msg)
                }
                r if Some(r) == u.min_rule() => {
                    let msg = format!("{} {}: {} m thick, the rule asks {} m", u.key(), a.id, round2(m.value), m.limit);
                    ((m.limit - m.value) / m.limit, msg)
                }
                r if Some(r) == u.u_rule() => {
                    let msg = format!("{} {}: U {} W/m2K over {}", u.key(), a.id, round2(m.value), m.limit);
                    ((m.value - m.limit) / m.limit, msg)
                }
                _ => {
                    let msg = format!("{} {}: Rw {} dB under {}", u.key(), a.id, round1(m.value), m.limit);
                    ((m.limit - m.value) / m.limit, msg)
                }
            };
            out.push(finding(spec, m.rule, None, excess, msg, None));
        }
        if let Some(w) = want.filter(|w| u.bearing() && a.fire_minutes < *w) {
            let msg = format!("{} {}: rated {} min, the project asks {}", u.key(), a.id, a.fire_minutes, spec.fire_rating);
            out.push(finding(spec, FIRE_RATING, None, f64::from(w - a.fire_minutes) / f64::from(w), msg, None));
        }
    }
}

/// A wall the plan rates as a fire compartment or a stair enclosure whose layers, with all the
/// fire board the catalogue allows, stay under the rating its rule asks for.
fn rated_walls(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let mut seen = std::collections::BTreeSet::new();
    for s in p.storeys.iter().chain(&p.basements) {
        let uses = wall_uses(s);
        for (k, w) in s.layout.walls.iter().enumerate() {
            let Some(f) = w.fire else { continue };
            let stair = f.sep == crate::FireSep::Stair;
            if !seen.insert((uses[k].use_, stair, f.minutes)) {
                continue;
            }
            let a = rated_in(&spec.rules, uses[k].use_, spec.design.structure, f.minutes);
            if a.fire_minutes < f.minutes {
                let rule = if stair { "fire.stair_rating" } else { "fire.wall_rating" };
                let msg = format!("{} {}: layers reach {} min, the rule asks {} min", uses[k].use_.key(), a.id, a.fire_minutes, f.minutes);
                out.push(finding(spec, rule, None, f64::from(f.minutes - a.fire_minutes) / f64::from(f.minutes), msg, None));
            }
        }
    }
}

/// Reasons of the chosen build-ups for `why`: (rule, its value, template key, args).
pub fn whys(spec: &ProjectSpec, p: &GProject) -> Vec<(&'static str, f64, &'static str, Args)> {
    let mut out = vec![];
    for u in uses_of(p) {
        let a = chosen(spec, u);
        let t = ("t", round2(a.thickness()));
        if let (Some(rule), Some(x)) = (u.u_rule(), a.u) {
            let key = match u {
                Use::ExteriorWall => "asm_wall",
                Use::GroundSlab => "asm_ground",
                _ => "asm_roof",
            };
            let lim = spec.rules.get(rule);
            out.push((rule, lim, key, vec![t.clone(), ("ins", round2(a.insulation())), ("u", round2(x)), ("lim", lim.to_string())]));
        }
        if let (Some(rule), Some(x)) = (u.rw_rule(), a.rw) {
            let key = if matches!(u, Use::PartyWall | Use::CoreWall) { "asm_party" } else { "asm_floor" };
            let lim = spec.rules.get(rule);
            out.push((rule, lim, key, vec![t, ("m", round1(a.mass)), ("rw", round1(x)), ("lim", lim.to_string())]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_parses_and_bad_entries_are_refused() {
        assert!(materials().len() >= 10);
        let bad = DATA.replace("\"fire\": \"E\"", "\"fire\": \"Z\"");
        assert!(parse(&bad).unwrap_err().contains("fire class"));
        let bad = DATA.replace("\"use\": \"pitched_roof\"", "\"use\": \"flat_roof\"");
        assert!(parse(&bad).unwrap_err().contains("pitched_roof"));
    }

    #[test]
    fn the_rating_reads_its_minutes() {
        assert_eq!(rating_minutes("REI 60"), Some(60));
        assert_eq!(rating_minutes("TBD"), None);
    }
}
