//! Steel frame generator: lattice tower, guyed triangular mast, portal frame shed, braced
//! multi-storey frame, or a single plane truss, with riveted or bolted joints derived and rule-checked.

use crate::hangar::{HallOutfit, Hangar};
use crate::params::{bad as params_bad, Args};
use crate::project::rules::{self, RuleSet};
use crate::project::why::Why;
use crate::steel_bridge;
use crate::{variant_of, GenError, GenOut, Generator, ParamDef, ParamKind};
use ddc_geom::steel::hier::{self, HierRules};
use ddc_geom::steel::hier_check::{self, CheckRules};
use ddc_geom::steel::lattice::{self, Frame, MemRole, Pattern, Secs};
use ddc_geom::steel::longspan::{self, RingRoof};
use ddc_geom::steel::outfit;
use ddc_geom::steel::plate::PlateKind;
use ddc_geom::steel::tower::{self, TowerParams};
use ddc_geom::steel::tower_size;
use ddc_geom::v3;
use serde_json::{json, Value as J};

const KINDS: [&str; 11] = ["lattice_tower_300", "portal_frame", "braced_frame", "truss", "tower_platform", "truss_hall", "arch_bridge", "cantilever_ring", "tree", "stadium_ring", "guyed_mast"];
const APPROACHES: [&str; 4] = ["auto", "ramps", "stairs", "none"];
const SERVICES: [&str; 2] = ["lattice", "power_line"];
const PATTERNS: [&str; 4] = ["x", "warren", "pratt", "k"];
const OUTFITS: [&str; 2] = ["frame", "hangar"];
const JOINTS: [&str; 3] = ["auto", "all", "none"];

const fn p(name: &'static str, kind: ParamKind, min: f64, max: f64, default: f64, help: &'static str) -> ParamDef {
    ParamDef { name, kind, min, max, default, help }
}

static PARAMS: [ParamDef; 38] = [
    p("kind", ParamKind::Choice(&KINDS), 0.0, 0.0, 1.0, "What to generate"),
    p("pattern", ParamKind::Choice(&PATTERNS), 0.0, 0.0, 1.0, "Lattice bracing (tower faces, truss web)"),
    p("span", ParamKind::Length, 3.0, 400.0, 24.0, "Clear span of the portal, truss, truss hall or arch bridge (up to steel.long.max_span), the long axis of the ring's opening (up to steel.ring.max_span), or bay x of the braced frame, m"),
    p("height", ParamKind::Length, 1.0, 60.0, 6.0, "Eaves (portal, truss hall), truss depth, storey height, the ring's top chord height, total height of the tree, or the inner roof ring of the stadium ring, m"),
    p("bays", ParamKind::Count, 1.0, 120.0, 6.0, "Frames along the shed or hall, trusses round the ring, or bays x of the braced frame"),
    p("spacing", ParamKind::Length, 2.0, 15.0, 6.0, "Frame spacing, or bay y of the braced frame, m"),
    p("storeys", ParamKind::Count, 1.0, 60.0, 4.0, "Storeys of the braced frame; bays y = 2"),
    p("pitch_deg", ParamKind::Ratio, 0.0, 45.0, 10.0, "Roof pitch of the portal, or truss top chord rise in degrees"),
    p("panel", ParamKind::Length, 0.6, 8.0, 1.6, "Lattice panel height (tower legs) or truss panel length, m"),
    p("panels", ParamKind::Count, 2.0, 200.0, 12.0, "Truss panels"),
    p("width", ParamKind::Length, 2.0, 400.0, 4.0, "Deck width of the arch bridge, the short axis of the ring's opening (up to steel.ring.max_span), or the opening of the stadium ring along y, m"),
    p("rise", ParamKind::Length, 2.0, 80.0, 15.0, "Arch rise above the deck, m"),
    p("deck_z", ParamKind::Length, 1.0, 40.0, 6.0, "Deck height of the arch bridge on its piers, m"),
    p("reach", ParamKind::Length, 2.0, 60.0, 25.0, "Cantilever of the ring past its front columns, m"),
    p("backspan", ParamKind::Length, 1.0, 40.0, 9.0, "Back-span of the ring from the front to the back columns, >= reach / 4, m"),
    p("detail", ParamKind::Count, 1.0, 3.0, 1.0, "Lattice levels of the tower: 1 its lattice of rolled members; 2 every long member a lattice; 3 the chords of those laced too. Levels 2-3 are reported (counts, plates, rivets, preliminary check) and size the members; the model gets the level-1 members and, from level 2, also plates and bolts at every free node (see joints), railings, stairs and lift guides in the legs, the top cabin with mast and the masonry piers as footings"),
    p("profile", ParamKind::Points, 2.0, 64.0, 0.0, "Tower outline as [[height, half-width], ...] in m, heights rising; replaces the built-in curve"),
    p("approach", ParamKind::Choice(&APPROACHES), 0.0, 0.0, 0.0, "What leads onto the arch bridge deck at each end: auto (ramps at the rule slope with landings, stairs where they would be longer than the rule allows), ramps, stairs or nothing"),
    p("outfit", ParamKind::Choice(&OUTFITS), 0.0, 0.0, 0.0, "truss_hall: frame = the structure only; hangar = also the roof cladding over the top chords and a sliding door across the gable at y = 0 on its track beam, crane runway beams on corbels with overhead cranes, a two-storey office and workshop annex of rooms behind the far gable"),
    p("rules", ParamKind::Rules, 0.0, 0.0, 0.0, "Overrides of steel.* rules, e.g. {\"steel.lat.setback\": 0.8}"),
    p("service", ParamKind::Choice(&SERVICES), 0.0, 0.0, 0.0, "What the lattice tower carries: lattice (a plain tower, the default), power_line (cross-arms with insulator strings, for a transmission or high-voltage tower)"),
    p("joints", ParamKind::Choice(&JOINTS), 0.0, 0.0, 0.0, "Connections when the frame lands in the model: auto (a lattice tower at detail 2 or 3 gets plates and bolts at every free node, other frames none), all (every free node of any frame), none"),
    p("tower_height", ParamKind::Length, 0.0, 400.0, 0.0, "Lattice tower height, m (0: 300)"),
    p("base_width", ParamKind::Length, 0.0, 200.0, 0.0, "Lattice tower footprint side at the ground, m (0: 125)"),
    p("platform_1", ParamKind::Length, 0.0, 400.0, 0.0, "Lattice tower first platform floor, m (0: scaled from 57.6 of 300)"),
    p("platform_2", ParamKind::Length, 0.0, 400.0, 0.0, "Lattice tower second platform, where the legs merge, m (0: scaled from 115.7)"),
    p("platform_3", ParamKind::Length, 0.0, 400.0, 0.0, "Lattice tower top platform, m (0: scaled from 276.1)"),
    p("at", ParamKind::Length, 2.0, 600.0, 200.0, "tower_platform: height of one more platform on a lattice tower built with the same panel, pattern, profile and seed; only the platform and its deck are added, m"),
    p("generations", ParamKind::Count, 1.0, 5.0, 4.0, "Tree: branching generations above the trunk"),
    p("branches", ParamKind::Count, 2.0, 4.0, 3.0, "Tree: branches out of every tip"),
    p("ratio", ParamKind::Ratio, 0.4, 0.9, 0.7, "Tree: length of a branch over its parent's"),
    p("length", ParamKind::Length, 20.0, 300.0, 150.0, "Stadium ring: opening of the roof along x, m"),
    p("roof_depth", ParamKind::Length, 4.0, 80.0, 30.0, "Stadium ring: roof from the inner to the outer ring, m"),
    p("masts", ParamKind::Count, 3.0, 64.0, 16.0, "Stadium ring: masts under the outer ring"),
    p("mast_height", ParamKind::Length, 5.0, 400.0, 80.0, "Height of the guyed mast, m"),
    p("side", ParamKind::Length, 0.3, 6.0, 1.2, "Side of the guyed mast's triangular shaft, m"),
    p("guys", ParamKind::Count, 1.0, 8.0, 3.0, "Guy levels of the mast, evenly up to just under the head; three guys per level"),
    p("anchor", ParamKind::Ratio, 0.2, 1.5, 0.625, "Guy anchor distance from the mast axis, as a share of its height"),
];

static VARIANTS: [&str; 3] = ["as requested", "coarser lattice, fewer members", "finer lattice, more members"];

pub(crate) struct SteelFrameGen;

fn pattern(s: &str) -> Pattern {
    Pattern::parse(s).unwrap_or(Pattern::X)
}

/// The lattice numbers from the rule set (`steel.lat.*`, `steel.conn.*`); `levels` is the brief's.
pub fn hier_rules(rs: &RuleSet, levels: usize) -> HierRules {
    let opt = |id: &str| hier::PATTERN_OPTIONS[(rs.get(id) as usize).min(3)];
    HierRules {
        upsize: 0,
        levels,
        min_len: rs.get("steel.lat.min_len"),
        width_ratio: rs.get("steel.lat.width_ratio"),
        max_width: rs.get("steel.lat.max_width"),
        min_width: rs.get("steel.lat.min_width"),
        child_ratio: rs.get("steel.lat.child_ratio"),
        panel_aspect: rs.get("steel.lat.panel_aspect"),
        x_min_width: rs.get("steel.lat.x_min_width"),
        k_ratio: rs.get("steel.lat.k_ratio"),
        girder_pattern: opt("steel.lat.girder_pattern"),
        arch_pattern: opt("steel.lat.arch_pattern"),
        setback: rs.get("steel.lat.setback"),
        plate_min: rs.get("steel.conn.plate_min"),
        piece_max: rs.get("steel.conn.piece_max"),
        tower_panel: rs.get("steel.lat.tower_panel"),
        floor_grid: rs.get("steel.lat.floor_grid"),
    }
}

/// The outfit numbers from the rule set (`steel.out.*`).
pub fn outfit_rules(rs: &RuleSet) -> outfit::OutfitRules {
    outfit::OutfitRules {
        rail_height: rs.get("steel.out.rail_height"),
        stair_width: rs.get("steel.out.stair_width"),
        stair_riser: rs.get("steel.out.stair_riser"),
        stair_flight: rs.get("steel.out.stair_flight"),
        lift_inset: rs.get("steel.out.lift_inset"),
        cabin_size: rs.get("steel.out.cabin_size"),
        cabin_height: rs.get("steel.out.cabin_height"),
        roof_rise: rs.get("steel.out.roof_rise"),
        mast_height: rs.get("steel.out.mast_height"),
        pier_margin: rs.get("steel.out.pier_margin"),
        pier_depth: rs.get("steel.out.pier_depth"),
        pier_batter: rs.get("steel.out.pier_batter"),
    }
}

/// The preliminary check numbers from the rule set (`steel.check.*`), in SI units.
pub fn check_rules(rs: &RuleSet) -> CheckRules {
    CheckRules {
        f_allow: rs.get("steel.check.f_allow") * 1e6,
        rankine_a: 1.0 / rs.get("steel.check.rankine_k"),
        wind_q: rs.get("steel.check.wind_q"),
        solidity: rs.get("steel.check.solidity"),
        live_q: rs.get("steel.check.live_q"),
    }
}

fn why_of(rs: &RuleSet, d: &hier::Decision) -> Why {
    let value = rules::def(d.rule).map_or(json!(d.value), |r| r.value_json(d.value));
    let source = rs.source(d.rule).map_or("builtin", |l| l.name()).to_string();
    Why::new("steel".into(), d.rule, value, source, d.key, d.args.clone())
}

fn num(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

type Detail = (Vec<(&'static str, f64)>, Vec<Why>, Frame, TowerParams);

/// The latticed tower: counts of every level, the decisions and the preliminary check; the piece
/// sections go up a band at a time (at most two) while the wind check finds members over.
fn detail_report(tp: &TowerParams, rs: &RuleSet, levels: usize) -> Result<Detail, GenError> {
    let inf = GenError::Infeasible;
    let (mut h, mut c, mut up) = (None, None, 0);
    while up <= 2 {
        let hr = HierRules { upsize: up, ..hier_rules(rs, levels) };
        let t = hier::tower(tp, &hr).map_err(inf)?;
        let r = hier_check::check(&t, &check_rules(rs)).map_err(inf)?;
        let done = r.over == 0;
        (h, c) = (Some(t), Some(r));
        if done || levels < 2 {
            break;
        }
        up += 1;
    }
    let (h, c) = (h.unwrap(), c.unwrap());
    let bad = h.validate();
    if !bad.is_empty() {
        return Err(GenError::Infeasible(format!("{} fastener findings: {}", bad.len(), bad[0])));
    }
    let t = h.totals();
    let mut why: Vec<Why> = h.decisions.iter().map(|d| why_of(rs, d)).collect();
    let args = vec![("ratio", format!("{:.2}", c.max)), ("over", c.over.to_string()), ("sway", format!("{:.2}", c.sway))];
    if levels >= 2 {
        let args = vec![("steps", up.min(2).to_string()), ("over", c.over.to_string())];
        why.push(Why::new("steel".into(), "steel.check.f_allow", json!(rs.get("steel.check.f_allow")), "builtin".into(), "steel_sized", args));
    }
    why.push(Why::new("steel".into(), "steel.check.f_allow", json!(rs.get("steel.check.f_allow")), "builtin".into(), "steel_check", args));
    let metrics = vec![
        ("lattice_levels", t.max_level() as f64),
        ("pieces", t.pieces as f64),
        ("pieces_level_1", t.by_level[1] as f64),
        ("pieces_level_2", t.by_level[2] as f64),
        ("pieces_level_3", t.by_level[3] as f64),
        ("plates", t.plates as f64),
        ("gussets", t.plates_by_kind[PlateKind::Gusset as usize] as f64),
        ("splices", t.plates_by_kind[PlateKind::Splice as usize] as f64),
        ("detail_rivets", t.rivets as f64),
        ("latticed_members", h.roots.len() as f64),
        ("prototypes", h.protos.len() as f64),
        ("instances", t.placements as f64),
        ("detail_steel_t", num(t.mass / 1e3, 1)),
        ("check_preliminary_max_use", num(c.max, 3)),
        ("check_preliminary_over", c.over as f64),
        ("check_sway_m", num(c.sway, 3)),
    ];
    Ok((metrics, why, h.skeleton, h.params))
}

/// The tower of the brief: the built-in 300 m outline scaled to the asked height and footprint
/// (or the asked profile), platforms where asked (else at their built-in share of the height),
/// member sizing and core lifts by the rules.
fn tower_of(a: &Args<'_>, rs: &RuleSet) -> Result<TowerParams, GenError> {
    let d = TowerParams::default();
    let h = match a.num("tower_height") {
        x if x > 0.0 => x,
        _ => d.height,
    };
    let s = h / d.height;
    let half = match a.num("base_width") {
        x if x > 0.0 => x / 2.0,
        _ => d.w(0.0),
    };
    let kz = |name: &str, i: usize| match a.num(name) {
        x if x > 0.0 => x,
        _ => d.platforms[i] * s,
    };
    let bare = a.choice("service") == "power_line";
    // a bare lattice has no platforms: its legs merge at the built-in share, whatever platform_* says
    let platforms = if bare { d.platforms.map(|z| z * s) } else { [kz("platform_1", 0), kz("platform_2", 1), kz("platform_3", 2)] };
    let truss_depth = d.truss_depth.map(|t| t * s.min(1.0));
    let mut t = TowerParams {
        height: h,
        w_a: d.w_a * half / d.w(0.0),
        w_b: d.w_b * half / d.w(0.0),
        w_k: d.w_k * s,
        leg0: d.leg0 * half / d.w(0.0),
        platforms,
        ring1: d.ring1 * half / d.w(0.0),
        truss_depth,
        arch_rise: d.arch_rise * s,
        profile: a.points("profile"),
        loads: Some(loads(rs)),
        lifts: rs.get("steel.out.lifts") as usize,
        lift_offset: rs.get("steel.out.lift_offset"),
        lift_box: (rs.get("steel.out.lift_box_w"), rs.get("steel.out.lift_box_d")),
        ..d
    };
    // the arches stay under the first platform truss
    t.arch_rise = t.arch_rise.min(0.8 * (platforms[0] - truss_depth[0]));
    // an outline that cannot be built is infeasible; platforms or lifts that do not fit it are the brief's
    t.outline().check().map_err(GenError::Infeasible)?;
    // a transmission tower has no platforms to fit; the full check is the tower's with them
    if bare {
        if let Some((part, e)) = t.shaft_fault() {
            let param = match part {
                "outline" if t.profile.is_some() => "profile",
                "outline" | "leg_box" => "base_width",
                "panel" => "panel",
                "loads" => "rules",
                _ => "tower_height",
            };
            return Err(params_bad(param, e));
        }
    } else {
        t.check().map_err(|e| params_bad("platform_1", e))?;
    }
    Ok(t)
}

/// Numbers of the tower sizing rule, from the rule set: the wind, floor load, stress and
/// slenderness of the preliminary check (`steel.check.*`), the modulus of steel.
pub fn loads(rs: &RuleSet) -> tower_size::Loads {
    tower_size::Loads {
        wind_kpa: rs.get("steel.check.wind_q") / 1e3,
        face_factor: rs.get("steel.check.solidity"),
        self_kn_m: rs.get("steel.check.self_weight"),
        deck_kpa: rs.get("steel.check.live_q") / 1e3,
        f_allow_mpa: rs.get("steel.check.f_allow"),
        rankine_a: rs.get("steel.check.rankine_k"),
        strut_share: rs.get("steel.check.strut_share"),
        e_gpa: ddc_geom::steel::sections::E_STEEL / 1e9,
    }
}

/// Section constants of the deck girder of the arch bridge, PG 1200/500/35/500/35/16, in m.
const GIRDER_H: f64 = 1.2;
const GIRDER_B: f64 = 0.5;
const GIRDER_TF: f64 = 0.035;
const GIRDER_TW: f64 = 0.016;
const STEEL_E: f64 = 210e9;
const STEEL_RHO: f64 = 7850.0;

/// First vertical frequency and the sag under crowd load of the footbridge deck, from the two
/// deck girders as a simply supported beam (the arches only stiffen it, so both are on the safe
/// side), and whether the frequency clears the band of walking.
pub fn deck_vibration(rs: &RuleSet, span: f64, width: f64) -> Vec<(&'static str, f64)> {
    let hw = GIRDER_H - 2.0 * GIRDER_TF;
    let arm = (GIRDER_H - GIRDER_TF) / 2.0;
    let i_one = GIRDER_TW * hw.powi(3) / 12.0 + 2.0 * (GIRDER_B * GIRDER_TF.powi(3) / 12.0 + GIRDER_B * GIRDER_TF * arm * arm);
    let area = 2.0 * GIRDER_B * GIRDER_TF + hw * GIRDER_TW;
    let ei = 2.0 * STEEL_E * i_one;
    let mass = 2.0 * area * STEEL_RHO + rs.get("steel.bridge.deck_mass") * width;
    let f = std::f64::consts::PI / (2.0 * span * span) * (ei / mass).sqrt();
    let q = rs.get("steel.bridge.crowd_load") * 1e3 * width;
    let sag = 5.0 * q * span.powi(4) / (384.0 * ei);
    vec![
        ("deck_freq_hz", (f * 100.0).round() / 100.0),
        ("deck_sag_mm", (sag * 1e4).round() / 10.0),
        ("deck_span_over_sag", (span / sag.max(1e-9)).round()),
        ("deck_comfort_ok", if f >= rs.get("steel.bridge.f_min") { 1.0 } else { 0.0 }),
    ]
}

/// Cross-arms of a transmission tower, with their insulator strings: `steel.line.arms` arms
/// from `steel.line.arm_drop` under the top at `steel.line.arm_pitch`, each reaching
/// `steel.line.arm_length` past the tower on both sides, and `steel.line.strings` strings of
/// `steel.line.string_length` hung from each tip, `steel.line.string_pitch` apart.
pub fn power_line(f: &mut Frame, tp: &TowerParams, rs: &RuleSet) -> Result<Vec<(&'static str, f64)>, String> {
    let (arms, reach) = (rs.get("steel.line.arms") as usize, rs.get("steel.line.arm_length"));
    let (drop, pitch) = (rs.get("steel.line.arm_drop"), rs.get("steel.line.arm_pitch"));
    let (per, len, gap) = (rs.get("steel.line.strings") as usize, rs.get("steel.line.string_length"), rs.get("steel.line.string_pitch"));
    let lowest = tp.height - drop - pitch * (arms as f64 - 1.0);
    let before = f.insulators.len();
    if lowest <= len + rs.get("steel.line.clearance") {
        return Err(format!("{arms} cross-arms {pitch} m apart leave the lowest at {lowest:.1} m, too low for strings of {len} m: lower steel.line.arms, steel.line.arm_pitch or steel.line.string_length"));
    }
    for k in 0..arms {
        let want = tp.height - drop - pitch * k as f64;
        // the arm hangs on the shaft corners of the nearest panel level: a bar through the open
        // shaft between them would meet no node and stand loose
        let z = f
            .nodes
            .iter()
            .filter(|n| n.x > 0.0 && (n.x - n.y).abs() < 1e-6 && n.z > tp.platforms[1])
            .map(|n| n.z)
            .min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs()))
            .unwrap_or(want);
        let (w, half) = (tp.w(z), tp.w(z) + reach);
        for (sx, sy) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
            f.add(v3(sx * w, sy * w, z), v3(sx * half, 0.0, z), "IPE 300", MemRole::Beam, v3(0.0, 0.0, 1.0));
        }
        for side in [-1.0, 1.0] {
            for j in 0..per {
                let y = (j as f64 - (per as f64 - 1.0) / 2.0) * gap;
                f.insulators.push(lattice::InsulatorString { at: (side * half, y), z, length: len });
            }
        }
    }
    Ok(vec![("cross_arms", arms as f64), ("insulator_strings", (f.insulators.len() - before) as f64), ("string_length_m", len)])
}

/// Builds the frame and its metrics for the given arguments and variant.
pub fn build_frame(params: &J, seed: u64) -> Result<(Frame, Vec<(&'static str, f64)>), GenError> {
    build_steel(params, seed).map(|(f, m, _)| (f, m))
}

/// The frame, its metrics and, for a detailed tower, every decision with its rule.
pub fn build_steel(params: &J, seed: u64) -> Result<(Frame, Vec<(&'static str, f64)>, Vec<Why>), GenError> {
    build_all(params, seed).map(|(f, m, w, _)| (f, m, w))
}

/// Whether the frame lands with a connection at every free node: `joints` all or none, or by
/// default (auto) a lattice tower at detail 2 or 3, so one call builds the tower with its joints.
pub fn joints(params: &J) -> Result<bool, GenError> {
    let a = Args::new(&PARAMS, params)?;
    Ok(match a.choice("joints") {
        "all" => true,
        "none" => false,
        _ => a.choice("kind") == "lattice_tower_300" && a.choice("service") == "lattice" && a.count("detail") >= 2,
    })
}

type Built = (Frame, Vec<(&'static str, f64)>, Vec<Why>, Option<Box<HallOutfit>>);

fn build_all(params: &J, seed: u64) -> Result<Built, GenError> {
    let a = Args::new(&PARAMS, params)?;
    let k = [1.0, 1.5, 0.75][variant_of(seed)];
    let inf = |e: String| GenError::Infeasible(e);
    let pat = pattern(a.choice("pattern"));
    let rs = a.rules("rules")?;
    // the ranges of the parameters hold the largest a rule may allow; each kind keeps to its own
    let (max_span, max_bays, what) = if a.choice("kind") == "cantilever_ring" {
        (rs.get("steel.ring.max_span"), rs.get("steel.ring.max_bays"), "steel.ring")
    } else {
        (rs.get("steel.long.max_span"), rs.get("steel.long.max_bays"), "steel.long")
    };
    for (name, v, max) in [("span", a.num("span"), max_span), ("width", a.num("width"), max_span), ("bays", a.num("bays"), max_bays)] {
        if v > max + 1e-9 && a.raw(name).is_number() {
            return Err(params_bad(name, format!("{v} is over the {max} the rule {what}.max_{} allows for this kind", if name == "bays" { "bays" } else { "span" })));
        }
    }
    if let Some(id) = a.raw("rules").as_object().and_then(|o| o.keys().find(|id| !id.starts_with("steel."))) {
        return Err(params_bad("rules", format!("{id} is not a steel rule")));
    }
    let mut extra = (vec![], vec![]);
    let mut sway = None;
    let tower_params = || -> Result<TowerParams, GenError> {
        let d = tower_of(&a, &rs)?;
        Ok(TowerParams { panel_leg: a.num("panel") * k, panel_shaft: a.num("panel") * k * d.panel_shaft / d.panel_leg, floor_grid: d.floor_grid * k, pattern: pat, ..d })
    };
    let hangar = (a.choice("kind") == "truss_hall" && a.choice("outfit") == "hangar").then(|| Hangar {
        rs: &rs,
        span: a.num("span"),
        len: a.count("bays") as f64 * a.num("spacing"),
        eaves: a.num("height"),
        bays: a.count("bays"),
        spacing: a.num("spacing"),
    });
    if let Some(h) = &hangar {
        h.check()?;
    }
    let mut f = match a.choice("kind") {
        "tower_platform" => tower::tower_platform(&tower_params()?, a.num("at"), 1.5).map_err(inf)?,
        "lattice_tower_300" => {
            let tp = tower_params()?;
            sway = Some(tower_size::sway(&tp, &loads(&rs)));
            let levels = a.count("detail");
            let mut f;
            if a.choice("service") == "power_line" {
                f = tower::pylon_tower(&tp).map_err(inf)?;
            } else if levels >= 2 {
                // one tower for the report and the model: the lattice the hierarchy was built on
                let (m, w, frame, built) = detail_report(&tp, &rs, levels)?;
                extra = (m, w);
                f = frame;
                let o = outfit::outfit(&mut f, &built, &outfit_rules(&rs)).map_err(inf)?;
                extra.0.extend([
                    ("rail_posts", o.rail_posts as f64),
                    ("stair_treads", o.stair_treads as f64),
                    ("stair_landings", o.stair_landings as f64),
                    ("lift_guide_members", o.lift_rail_members as f64),
                    ("cabin_members", o.cabin_members as f64),
                    ("mast_members", o.mast_members as f64),
                    ("masonry_piers", o.piers.len() as f64),
                ]);
                extra.1.extend(o.decisions.iter().map(|d| why_of(&rs, d)));
            } else {
                f = tower::lattice_tower_300(&tp).map_err(inf)?;
            }
            if a.choice("service") == "power_line" {
                extra.0.extend(power_line(&mut f, &tp, &rs).map_err(inf)?);
            }
            f
        }
        "guyed_mast" => tower::guyed_mast(a.num("mast_height"), a.num("side"), a.num("panel") * k, a.count("guys"), a.num("anchor"), pat).map_err(inf)?,
        "portal_frame" => tower::portal_frame(a.num("span"), a.num("height"), a.num("pitch_deg"), a.count("bays"), a.num("spacing"), 1.5 * k).map_err(inf)?,
        "truss_hall" => {
            let split = |b: usize| hangar.as_ref().map_or(vec![], |h| h.splits(b));
            longspan::truss_hall_split(a.num("span"), a.num("height"), a.count("bays"), a.num("spacing"), a.num("panel") * k * 2.0, pat, &split).map_err(inf)?
        }
        "arch_bridge" => longspan::arch_bridge(a.num("span"), a.num("rise"), a.num("deck_z"), a.num("width"), a.num("panel") * k * 3.0).map_err(inf)?,
        "cantilever_ring" => {
            let r = RingRoof {
                a: a.num("span") / 2.0,
                b: a.num("width") / 2.0,
                bays: a.count("bays").max(8),
                reach: a.num("reach"),
                backspan: a.num("backspan"),
                height: a.num("height"),
                panel: a.num("panel") * k * 2.0,
            };
            longspan::cantilever_ring(&r, pat).map_err(inf)?
        }
        "tree" => crate::steel_free::tree(&crate::steel_free::Tree {
            height: a.num("height"),
            generations: a.count("generations"),
            branches: a.count("branches"),
            ratio: a.num("ratio"),
        })
        .map_err(inf)?,
        "stadium_ring" => crate::steel_free::stadium_ring(&crate::steel_free::Ring {
            length: a.num("length"),
            width: a.num("width"),
            depth: a.num("roof_depth"),
            height: a.num("height"),
            masts: a.count("masts"),
            per_mast: (2.0 / k).round().max(1.0) as usize,
        })
        .map_err(inf)?,
        "braced_frame" => tower::braced_frame(a.count("bays").min(12), 2, a.num("span").min(12.0), a.num("spacing"), a.count("storeys"), a.num("height").clamp(2.5, 6.0)).map_err(inf)?,
        _ => {
            let (span, depth) = (a.num("span"), a.num("height"));
            let n = ((a.count("panels") as f64 / k).round() as usize).max(2);
            let rise = span / 2.0 * a.num("pitch_deg").to_radians().tan();
            let top: Vec<_> = (0..=n)
                .map(|i| {
                    let x = span * i as f64 / n as f64;
                    v3(x, 0.0, depth + rise * (1.0 - (2.0 * x / span - 1.0).abs()))
                })
                .collect();
            let bot: Vec<_> = (0..=n).map(|i| v3(span * i as f64 / n as f64, 0.0, 0.0)).collect();
            let mut f = Frame::new();
            let s = if span > 30.0 { Secs { chord: "HEA 200", diag: "L 100x100x10", strut: "L 80x80x8" } } else { Secs { chord: "L 120x120x12", diag: "L 70x70x7", strut: "L 60x60x6" } };
            f.truss(&top, &bot, pat, s, MemRole::Chord);
            f
        }
    };
    let hall = hangar.map(|h| h.outfit(&mut f)).transpose()?;
    let errs = f.validate();
    if !errs.is_empty() {
        return Err(GenError::Infeasible(errs.join("; ")));
    }
    let (js, rep) = lattice::joints(&f, 0.0);
    let fast = lattice::fasteners(&f, &js, true);
    let rivets: usize = fast.iter().map(|s| s.items.len()).sum();
    let len: f64 = f.members.iter().map(|m| f.length(m)).sum();
    let mut metrics = vec![
        ("members", f.members.len() as f64),
        ("nodes", f.nodes.len() as f64),
        ("steel_t", (f.mass() / 100.0).round() / 10.0),
        ("member_length_m", len.round()),
        ("joints", rep.joints as f64),
        ("rivets", rivets as f64),
        ("joint_findings", rep.findings.len() as f64),
    ];
    if let Some((d, over)) = sway {
        // wind sway of the equivalent cantilever, preliminary; cuts where a role outgrew its ladder
        metrics.push(("sway_top_mm", (d * 1000.0).round()));
        metrics.push(("over_capacity", over as f64));
    }
    if a.choice("kind") == "arch_bridge" {
        metrics.extend(deck_vibration(&rs, a.num("span"), a.num("width")));
        let g = steel_bridge::guards(&mut f, &rs, a.num("span"), a.num("width"), a.num("deck_z"));
        let mut g = g;
        let (span, width, deck_z) = (a.num("span"), a.num("width"), a.num("deck_z"));
        let mode = match a.choice("approach") {
            "auto" if steel_bridge::ramp_too_long(&rs, deck_z) => "stairs",
            "auto" => "ramps",
            m => m,
        };
        if mode == "ramps" {
            let r = steel_bridge::ramps(&mut f, &rs, span, width, deck_z).map_err(inf)?;
            g.guard_runs += r.guard_runs;
            g.guard_posts += r.guard_posts;
            g.guard_length += r.guard_length;
            metrics.extend([("ramps", r.ramps as f64), ("ramp_length_m", r.ramp_length.round()), ("ramp_landings", r.ramp_landings as f64)]);
        }
        if mode == "stairs" {
            let r = steel_bridge::stairs(&mut f, &rs, span, width, deck_z).map_err(inf)?;
            g.guard_runs += r.guard_runs;
            g.guard_posts += r.guard_posts;
            g.guard_length += r.guard_length;
            metrics.extend([("stairs", r.stairs as f64), ("stair_risers", r.stair_risers as f64), ("stair_landings", r.stair_landings as f64), ("stair_length_m", r.stair_length.round())]);
        }
        metrics.extend([("guard_runs", g.guard_runs as f64), ("guard_posts", g.guard_posts as f64), ("guard_length_m", g.guard_length.round())]);
    }
    let hall = hall.map(|(o, m)| {
        metrics.extend(m);
        Box::new(o)
    });
    metrics.extend(extra.0);
    Ok((f, metrics, extra.1, hall))
}

impl Generator for SteelFrameGen {
    fn id(&self) -> &'static str {
        "steel_frame"
    }
    fn params(&self) -> &'static [ParamDef] {
        &PARAMS
    }
    fn variants(&self) -> &'static [&'static str] {
        &VARIANTS
    }
    fn summary(&self) -> &'static str {
        "Steel frame from a template (lattice_tower_300, portal_frame, braced_frame, truss; tower_platform adds one platform at `at` m to a tower already built; long spans on their own columns: truss_hall up to 120 m clear, arch_bridge, cantilever_ring stadium roof with a back-span; tree: a trunk branching recursively to a crown ring at the given height; stadium_ring: a roof ring round an elliptic opening on masts; guyed_mast: a triangular lattice mast held by guy cables, from mast_height, side, guys, anchor): members from the section table, joints with gussets and riveted or bolted groups, spacing rules checked. Sections are not a structural calculation."
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let (frame, metrics, why, hall) = build_all(params, seed)?;
        Ok(GenOut::Steel { frame: Box::new(frame), metrics, why, hall, joints: joints(params)? })
    }
}
