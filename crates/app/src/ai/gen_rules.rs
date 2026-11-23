// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The rules engine as the chat tools and the Rules window see it: rule rows, range-checked
//! patches, locks, explanations, conflicts with their fixes and variants. Pure functions of a
//! brief; a change returns the next brief, which goes through the usual preview and Apply.

use ddc_gen::project::conflict;
use ddc_gen::project::locks::{self, Keep, Lock, Target};
use ddc_gen::project::rules::{self, Layer, RuleDef, RuleSet, Tier};
use ddc_gen::project::score::{score, Score};
use ddc_gen::project::variants::{self, VaryCfg};
use ddc_gen::project::why::{self, RuleFix, Why};
use ddc_gen::project::{merge_patch, parse_spec, slots, GProject, ProjectSpec};
use ddc_model::Model;
use serde_json::{json, Map, Value as J};

/// Rules the variants may vary, in the order the variant search reads them.
pub const VARY: [&str; 4] = ["massing.form", "grid.span", "depth.band", "facade.system"];

/// Most variants offered at once.
pub fn max_variants() -> usize {
    ddc_ai::limits::VARIANTS.get_us()
}

/// One rule as the window and `list_rules` show it.
#[derive(Clone, Debug)]
pub struct RuleRow {
    pub def: &'static RuleDef,
    /// The value in force; `None` when the typology decides and nothing set it.
    pub value: Option<f64>,
    /// `builtin`, `norm`, `preset`, `project`, `brief`, or `typology`.
    pub source: &'static str,
    pub tier: Option<Tier>,
    pub weight: f64,
    pub locked: bool,
    /// What the goal costs in the current build; 0 when met or not a goal.
    pub cost: f64,
}

impl RuleRow {
    /// The goal costs something now: changing it moves the score.
    pub fn limiting(&self) -> bool {
        self.cost > 1e-9
    }

    pub fn value_json(&self) -> J {
        self.value.map_or(J::Null, |x| self.def.value_json(x))
    }

    /// The short form a tool reply lists.
    pub fn to_json(&self) -> J {
        let d = self.def;
        let mut o = Map::new();
        o.insert("id".into(), json!(d.id));
        o.insert("value".into(), self.value_json());
        o.insert("takes".into(), json!(d.takes()));
        o.insert("source".into(), json!(self.source));
        if let Some(t) = self.tier {
            o.insert("tier".into(), json!(t.name()));
            o.insert("weight".into(), json!(self.weight));
        }
        if self.locked {
            o.insert("locked".into(), json!(true));
        }
        if self.limiting() {
            o.insert("cost".into(), json!((self.cost * 1000.0).round() / 1000.0));
        }
        J::Object(o)
    }
}

/// Values a typology decides live on the spec, not in the rule set.
fn spec_value(spec: &ProjectSpec, id: &str) -> Option<f64> {
    match id {
        "massing.floor_height" => Some(spec.floor_height),
        "depth.band" => Some(spec.band_depth),
        "corridor.width" => Some(spec.corridor),
        "core.stair_width" => Some(spec.stair_width),
        "grid.span" => Some(spec.grid),
        "basement.count" => Some(f64::from(spec.basements)),
        _ => None,
    }
}

/// The value in force and where it comes from.
pub fn value_of(spec: &ProjectSpec, id: &str) -> (Option<f64>, &'static str) {
    let rs = &spec.rules;
    let typology = rules::def(id).is_some_and(|d| d.default.is_none());
    match rs.resolve(id) {
        Some((x, Layer::Builtin)) => (spec_value(spec, id).or(Some(x)), if typology { "typology" } else { "builtin" }),
        Some((x, l)) => (Some(x), l.name()),
        None => (spec_value(spec, id), "typology"),
    }
}

/// Rule ids a brief locks.
pub fn locked_rules(brief: &J) -> Vec<String> {
    locks::of_brief(brief)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|l| match l.target {
            Target::Rule(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// Every rule of a brief in file order; the cost comes from `score` when a build is at hand.
pub fn rows(brief: &J, spec: &ProjectSpec, sc: Option<&Score>) -> Vec<RuleRow> {
    let locked = locked_rules(brief);
    rules::defs()
        .iter()
        .map(|d| {
            let (value, source) = value_of(spec, &d.id);
            RuleRow {
                def: d,
                value,
                source,
                tier: spec.rules.tier(&d.id),
                weight: spec.rules.weight(&d.id),
                locked: locked.iter().any(|l| *l == d.id),
                cost: sc.map_or(0.0, |s| s.cost_of(&d.id)),
            }
        })
        .collect()
}

/// Rows of one group, matching words of the id, or changed above the built-in set.
pub fn filter<'a>(rows: &'a [RuleRow], group: Option<&str>, q: &str, changed: bool) -> Vec<&'a RuleRow> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    rows.iter()
        .filter(|r| group.is_none_or(|g| r.def.group == g))
        .filter(|r| words.iter().all(|w| r.def.id.to_lowercase().contains(w.as_str())))
        .filter(|r| !changed || !matches!(r.source, "builtin" | "typology"))
        .collect()
}

/// A built brief: spec, project and score.
pub struct Built {
    pub spec: ProjectSpec,
    pub project: GProject,
    pub score: Score,
}

pub fn build(brief: &J) -> Result<Built, String> {
    let spec = parse_spec(brief).map_err(|e| e.to_string())?;
    let project = ddc_gen::project::build_project(&spec).map_err(|e| e.to_string())?;
    let score = score(&spec, &project);
    Ok(Built { spec, project, score })
}

// ---- changes: each returns the next brief ----

/// Merges one patch entry into what the brief already says about the rule: a value keeps the
/// weight and tier set before, a weight keeps the value.
fn merge_entry(old: Option<&J>, new: &J) -> J {
    match (old, new) {
        (Some(J::Object(o)), J::Object(n)) => {
            let mut m = o.clone();
            for (k, v) in n {
                m.insert(k.clone(), v.clone());
            }
            J::Object(m)
        }
        (Some(J::Object(o)), x) => {
            let mut m = o.clone();
            m.insert("value".into(), x.clone());
            J::Object(m)
        }
        (Some(x), J::Object(n)) if !x.is_null() && !n.contains_key("value") => {
            let mut m = n.clone();
            m.insert("value".into(), x.clone());
            J::Object(m)
        }
        (_, x) => x.clone(),
    }
}

/// The brief with a rule patch `{id: value | {value, weight, tier, hard} | null}`: every entry
/// is checked against its rule's range and type, a locked rule is refused, and either all
/// apply or none.
pub fn set_rules(brief: &J, patch: &J) -> Result<J, Vec<String>> {
    let Some(p) = patch.as_object().filter(|p| !p.is_empty()) else {
        return Err(vec!["rules must name at least one rule: {id: value}".into()]);
    };
    let mut check = RuleSet::new();
    let mut errs: Vec<String> = match check.apply_patch(Layer::Brief, patch) {
        Ok(()) => vec![],
        Err(e) => e.iter().map(|x| x.to_string()).collect(),
    };
    let locked = locked_rules(brief);
    for id in p.keys() {
        if locked.iter().any(|l| l == id) {
            errs.push(format!("{id}: locked; unlock it first"));
        }
    }
    if !errs.is_empty() {
        return Err(errs);
    }
    let mut next = brief.clone();
    if !next["rules"].is_object() {
        next["rules"] = json!({});
    }
    let r = next["rules"].as_object_mut().expect("rules is an object");
    for (id, v) in p {
        if v.is_null() {
            r.remove(id);
        } else {
            let e = merge_entry(r.get(id), v);
            r.insert(id.clone(), e);
        }
    }
    if r.is_empty() {
        if let Some(o) = next.as_object_mut() {
            o.remove("rules");
        }
    }
    parse_spec(&next).map_err(|e| vec![e.to_string()])?;
    Ok(next)
}

/// The target and keep of a lock argument `{rule | storey [, room | zone], keep}`.
pub fn lock_target(v: &J) -> Result<(Target, Keep), String> {
    let mut t = v.clone();
    if !t.is_object() {
        return Err("a lock target is {rule} or {storey, room?, zone?, keep?}".into());
    }
    if t["rule"].is_null() && t["storey"].is_null() {
        return Err("a lock target names a rule or a storey".into());
    }
    t["seal"] = json!(0);
    let l = Lock::from_json(&t)?;
    if let Target::Rule(id) = &l.target {
        rules::def(id).ok_or(format!("no rule {id}"))?;
    }
    Ok((l.target, l.keep))
}

/// The brief with `targets` locked as they are in its build; a target locked before is sealed
/// again. Answers the labels.
pub fn add_locks(brief: &J, b: &Built, targets: &[J]) -> Result<(J, Vec<String>), String> {
    if targets.is_empty() {
        return Err("targets is empty".into());
    }
    let mut all = locks::of_brief(brief)?;
    let mut labels = vec![];
    for t in targets {
        let (target, keep) = lock_target(t)?;
        let l = Lock::take(target, keep, brief, &b.spec, &b.project)?;
        all.retain(|o| o.target != l.target);
        labels.push(l.label());
        all.push(l);
    }
    let mut next = brief.clone();
    next["locks"] = J::Array(all.iter().map(Lock::to_json).collect());
    Ok((next, labels))
}

/// The brief without the locks on `targets`, or without any with `all`.
pub fn remove_locks(brief: &J, targets: &[J], all: bool) -> Result<(J, Vec<String>), String> {
    let have = locks::of_brief(brief)?;
    let mut drop: Vec<Target> = vec![];
    if !all {
        if targets.is_empty() {
            return Err("name targets, or all: true".into());
        }
        for t in targets {
            drop.push(lock_target(t)?.0);
        }
    }
    let (gone, kept): (Vec<Lock>, Vec<Lock>) = have.into_iter().partition(|l| all || drop.contains(&l.target));
    if gone.is_empty() {
        return Err("no such lock on this project".into());
    }
    let mut next = brief.clone();
    if kept.is_empty() {
        if let Some(o) = next.as_object_mut() {
            o.remove("locks");
        }
    } else {
        next["locks"] = J::Array(kept.iter().map(Lock::to_json).collect());
    }
    Ok((next, gone.iter().map(Lock::label).collect()))
}

/// `next` rebuilt around the locks of `next` that were sealed on `prev`: what a lock forbids is
/// refused and reported, never dropped silently.
pub fn around_locks(prev: &J, next: &J) -> Result<(J, J), String> {
    let ls = locks::of_brief(next)?;
    let r = locks::regenerate(prev, next, &ls)?;
    let refused: Vec<J> = r.refused.iter().map(|x| x.to_json()).collect();
    let applied: Vec<String> = r.applied.iter().map(|p| p.join("/")).collect();
    Ok((r.brief, json!({"applied": applied, "refused": refused})))
}

/// RFC 7386 patch that turns `a` into `b`.
pub fn diff_patch(a: &J, b: &J) -> J {
    match (a, b) {
        (J::Object(x), J::Object(y)) => {
            let mut out = Map::new();
            for k in x.keys() {
                if !y.contains_key(k) {
                    out.insert(k.clone(), J::Null);
                }
            }
            for (k, v) in y {
                match x.get(k) {
                    Some(old) if old == v => {}
                    Some(old @ J::Object(_)) if v.is_object() => {
                        out.insert(k.clone(), diff_patch(old, v));
                    }
                    _ => {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            J::Object(out)
        }
        _ => b.clone(),
    }
}

// ---- reading ----

/// The explain targets of a slot of a stored project: the storey or basement and the parts
/// whose rules shaped the element.
pub fn slot_targets(rel: u32, basements: u32) -> Vec<String> {
    // a storey's columns past its own block stand in the overflow block after the services
    let more = (slots::COLUMNS_MORE..slots::RAILINGS).contains(&rel);
    let located = if more {
        Some(((rel - slots::COLUMNS_MORE) / slots::COLUMNS_MORE_PER_STOREY, slots::COLUMNS))
    } else {
        slots::locate(rel)
    };
    let Some((block, at)) = located else {
        // the roof block's slots, and everything past the storeys
        return vec![if rel / slots::PER_STOREY == slots::LOW_STOREYS { "massing" } else { "rooms" }.into()];
    };
    let storey = match slots::floor_of(block, basements as usize) {
        Some(slots::Floor::Storey(k)) => format!("storey:{k}"),
        _ => "basement".to_string(),
    };
    let part = if at < slots::OPENINGS {
        "massing"
    } else if at < slots::ROOMS {
        "facade"
    } else if at < slots::SLAB {
        "rooms"
    } else if at == slots::SLAB {
        "massing"
    } else if at < slots::GRIDS {
        "core"
    } else {
        "grid"
    };
    vec![part.into(), storey]
}

/// The recipe table of an element category as the chat and the selection name it.
pub fn table_of(cat: &str) -> Option<&'static str> {
    Some(match cat {
        "wall" => "wall",
        "door" | "window" | "opening" => "opening",
        "floor" | "slab" => "slab",
        "room" => "room",
        "stair" => "stair",
        "shaft" => "shaft",
        "grid" => "grid",
        "column" | "col" => "col",
        "roof" => "roof",
        "comp" => "comp",
        "mep" => "mep_run",
        _ => return None,
    })
}

/// The stored project that built an element, its brief and the explain refs of the element.
pub fn element_refs(m: &Model, cat: &str, id: i64) -> Result<(String, J, Vec<String>), String> {
    let table = table_of(cat).ok_or(format!("{cat}: a project builds no such element"))?;
    let (pid, rel) = super::project_plan::slot_of(m, table, id)
        .map_err(|e| e.to_string())?
        .ok_or(format!("{cat}:{id} was not built by a generated project"))?;
    let stored = super::project_plan::stored(m).map_err(|e| e.to_string())?;
    let brief = stored.into_iter().find(|s| s.0 == pid).map(|s| s.1["input"].clone()).unwrap_or(J::Null);
    let basements = parse_spec(&brief).map(|s| s.basements).unwrap_or(0);
    Ok((pid, brief, slot_targets(rel, basements)))
}

/// The reasons of a part, a rule, `score`, or of several refs together, without repeats.
pub fn explain(spec: &ProjectSpec, p: &GProject, refs: &[String]) -> Vec<Why> {
    let mut out: Vec<Why> = vec![];
    for r in refs {
        for w in why::explain(spec, p, r) {
            if !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

/// What a fix is, as the chat and the window read it: the rule, the value, and the patch that
/// applies it.
pub fn fix_json(f: &RuleFix) -> J {
    let mut v = f.to_json();
    v["set_rule"] = json!({ &f.rule: f.value });
    v
}

/// The conflict of a brief, or of the brief with a spec merge patch; `None` when it builds.
pub fn conflicts(brief: &J, changes: Option<&J>) -> Option<conflict::Conflict> {
    let mut v = brief.clone();
    if let Some(c) = changes.filter(|c| c.is_object()) {
        merge_patch(&mut v, c);
    }
    conflict::find(&v)
}

/// The variant search for a brief: its locks kept, the rules not in `vary` fixed.
pub fn vary_cfg(brief: &J, n: usize, seed: u64, vary: &[String]) -> Result<VaryCfg, String> {
    if let Some(bad) = vary.iter().find(|v| !VARY.contains(&v.as_str())) {
        return Err(format!("{bad} cannot vary; vary takes {}", VARY.join(", ")));
    }
    let fixed = if vary.is_empty() {
        vec![]
    } else {
        VARY.iter().filter(|r| !vary.iter().any(|v| v == *r)).map(|r| r.to_string()).collect()
    };
    Ok(VaryCfg {
        n: n.clamp(1, max_variants()),
        seed,
        fixed,
        locks: locks::of_brief(brief)?,
        ..VaryCfg::default()
    })
}

pub fn variants(brief: &J, cfg: &VaryCfg) -> Result<variants::Variants, String> {
    variants::variants(brief, cfg)
}

/// Tier names, strongest first, for the window's choice.
pub fn tier_names() -> [&'static str; 4] {
    Tier::ALL.map(Tier::name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> J {
        json!({"typology": "office", "floors": 3})
    }

    #[test]
    fn a_value_outside_its_range_is_refused_and_the_brief_kept() {
        let d = rules::def("massing.floor_height").unwrap();
        let e = set_rules(&brief(), &json!({"massing.floor_height": d.max + 1.0})).unwrap_err();
        assert!(e[0].contains("massing.floor_height"), "{e:?}");
        let e = set_rules(&brief(), &json!({"no.such": 1})).unwrap_err();
        assert!(e[0].contains("no such rule"), "{e:?}");
        // all or none: one good entry does not slip through beside a bad one
        let e = set_rules(&brief(), &json!({"massing.floor_height": 3.3, "grid.span": -1})).unwrap_err();
        assert_eq!(e.len(), 1, "{e:?}");
        let ok = set_rules(&brief(), &json!({"massing.floor_height": 3.3})).unwrap();
        assert_eq!(ok["rules"]["massing.floor_height"], json!(3.3));
    }

    #[test]
    fn a_weight_keeps_the_value_and_null_resets() {
        let a = set_rules(&brief(), &json!({"massing.floor_height": 3.3})).unwrap();
        let b = set_rules(&a, &json!({"goal.compact": {"weight": 5}})).unwrap();
        let c = set_rules(&b, &json!({"goal.compact": 6})).unwrap();
        assert_eq!(c["rules"]["goal.compact"], json!({"weight": 5, "value": 6}));
        let d = set_rules(&c, &json!({"massing.floor_height": null, "goal.compact": null})).unwrap();
        assert!(d.get("rules").is_none(), "{d}");
    }

    #[test]
    fn a_lock_round_trips_and_blocks_its_rule() {
        let v = brief();
        let b = build(&v).unwrap();
        let (locked, labels) = add_locks(&v, &b, &[json!({"rule": "grid.span"}), json!({"storey": 0})]).unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(locked_rules(&locked), vec!["grid.span".to_string()]);
        let e = set_rules(&locked, &json!({"grid.span": 7.2})).unwrap_err();
        assert!(e[0].contains("locked"), "{e:?}");
        // locking again reseals, it does not add a twin
        let (again, _) = add_locks(&locked, &b, &[json!({"rule": "grid.span"})]).unwrap();
        assert_eq!(again["locks"].as_array().unwrap().len(), 2);
        let (one, gone) = remove_locks(&again, &[json!({"rule": "grid.span"})], false).unwrap();
        assert_eq!(gone, vec!["rule grid.span".to_string()]);
        let (none, _) = remove_locks(&one, &[], true).unwrap();
        assert_eq!(none, v, "unlocking everything gives the brief back");
        assert!(remove_locks(&none, &[], true).is_err());
    }

    #[test]
    fn a_change_a_lock_forbids_is_refused_not_dropped() {
        let v = brief();
        let b = build(&v).unwrap();
        let (locked, _) = add_locks(&v, &b, &[json!({"storey": 0, "keep": "geometry"})]).unwrap();
        let mut next = locked.clone();
        next["rules"] = json!({"grid.span": 6.0});
        let (out, report) = around_locks(&locked, &next).unwrap();
        let st = locks::of_brief(&out).unwrap();
        let ob = build(&out).unwrap();
        assert!(st.iter().all(|l| l.holds(&out, &ob.spec, &ob.project)), "{report}");
        let applied = report["applied"].as_array().unwrap().len();
        let refused = report["refused"].as_array().unwrap().len();
        assert!(applied + refused >= 1, "{report}");
    }

    #[test]
    fn diff_patch_rebuilds_the_target() {
        let a = json!({"x": 1, "o": {"a": 1, "b": 2}, "gone": true});
        let b = json!({"x": 2, "o": {"a": 1, "c": 3}, "new": [1]});
        let mut c = a.clone();
        merge_patch(&mut c, &diff_patch(&a, &b));
        assert_eq!(c, b);
        assert_eq!(diff_patch(&a, &a), json!({}));
    }

    #[test]
    fn rows_cover_every_rule_and_filter() {
        let v = brief();
        let b = build(&v).unwrap();
        let rs = rows(&v, &b.spec, Some(&b.score));
        assert_eq!(rs.len(), rules::defs().len());
        assert!(rs.iter().all(|r| r.value.is_some() || r.source == "typology"));
        let g = filter(&rs, Some("goal"), "", false);
        assert!(!g.is_empty() && g.iter().all(|r| r.tier.is_some()));
        let set = set_rules(&v, &json!({"massing.floor_height": 3.3})).unwrap();
        let s2 = parse_spec(&set).unwrap();
        let r2 = rows(&set, &s2, None);
        let ch = filter(&r2, None, "floor height", true);
        assert_eq!(ch.len(), 1);
        assert_eq!((ch[0].source, ch[0].value), ("brief", Some(3.3)));
    }

    #[test]
    fn slots_name_the_parts_that_shaped_them() {
        assert_eq!(slot_targets(slots::ROOMS + 3, 0), vec!["rooms", "storey:0"]);
        assert_eq!(slot_targets(2 * slots::PER_STOREY + slots::STAIRS, 0), vec!["core", "storey:2"]);
        assert_eq!(slot_targets(slots::block_base(slots::basement_block(0)), 1)[1], "basement");
        // a storey over the basements' blocks lives in a top tall block and keeps its name
        let up = slots::storey_block(39, 1);
        assert_eq!(slot_targets(slots::block_base(up) + slots::ROOMS, 1), vec!["rooms", "storey:39"]);
        assert_eq!(slot_targets(slots::FIXTURES + 5, 0), vec!["rooms"]);
        // a column past the storey's own block names its storey's grid
        let own = slots::PER_STOREY - slots::COLUMNS;
        assert_eq!(slot_targets(slots::column(3, own + 2), 0), vec!["grid", "storey:3"]);
        assert_eq!(slot_targets(slots::column(3, 2), 0), vec!["grid", "storey:3"]);
    }

    #[test]
    fn applying_a_rule_fix_resolves_the_conflict() {
        let mut fixed = 0;
        for (g, x) in [("goal.circulation", 0.05), ("goal.efficiency", 0.98), ("goal.daylight", 0.5)] {
            let b = json!({"typology": "office", "floors": 2,
                           "rules": {g: {"value": x, "hard": true}, "room.door": 1.0}});
            let k = conflicts(&b, None).unwrap_or_else(|| panic!("{g}: should not build"));
            for f in &k.fixes {
                let fj = fix_json(f);
                let next = set_rules(&b, &fj["set_rule"]).unwrap();
                assert!(conflicts(&next, None).is_none(), "{g}: {fj} leaves a conflict");
                assert_eq!(next["rules"][g]["hard"], json!(true), "the fix keeps the goal hard");
                fixed += 1;
            }
        }
        assert!(fixed >= 1, "no fix offered");
        // a change tested before it is proposed: the brief itself builds, the change does not
        let ok = json!({"typology": "office", "floors": 2});
        assert!(conflicts(&ok, None).is_none());
        let what_if = json!({"rules": {"goal.efficiency": {"value": 0.98, "hard": true}, "room.door": 1.0}});
        assert!(conflicts(&ok, Some(&what_if)).is_some());
    }

    #[test]
    fn variants_refuse_an_unknown_axis() {
        assert!(vary_cfg(&brief(), 3, 0, &["corridor.width".into()]).is_err());
        let c = vary_cfg(&brief(), 9, 1, &["grid.span".into()]).unwrap();
        assert_eq!(c.n, max_variants());
        assert_eq!(c.fixed.len(), VARY.len() - 1);
    }
}
