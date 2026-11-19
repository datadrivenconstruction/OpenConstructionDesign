//! `norms_lookup`, `norms_check`, `norms_programme`: read-only access to the country norm
//! packs. The pack is the project's stored choice, else the nearest to the language and
//! region the assistant passes. Numbers not verified against their source come back marked.

use super::fmt::Out;
use super::session::{Core, Done, Handler};
use super::tools_read::scope_of;
use ddc_ai::CallCx;
use ddc_model::checks::norm_kinds::{run_pack, PackCtx};
use ddc_gen::project::presets::{self, Preset};
use ddc_model::checks::norm_pack::{NormPack, PackCheck, PackSet, Programme, Variant};
use ddc_model::checks::Severity;
use serde_json::{json, Value as J};

pub const HANDLERS: [(&str, Handler); 3] = [
    ("norms_lookup", norms_lookup),
    ("norms_check", norms_check),
    ("norms_programme", norms_programme),
];

fn pack_of(c: &Core, a: &J) -> Result<(PackSet, NormPack), String> {
    let set = PackSet::embedded();
    let id = match a["pack"].as_str() {
        Some(p) => p.to_string(),
        None => c
            .ed
            .model
            .effective_norm_pack(&set, a["lang"].as_str().unwrap_or("en"), a["region"].as_str())
            .map_err(|e| e.to_string())?,
    };
    if !set.has(&id) {
        return Err(format!("no norm pack {id:?}; packs: {}", set.ids().join(", ")));
    }
    let pack = set.resolve(&id).map_err(|e| e.to_string())?;
    Ok((set, pack))
}

/// The generator rules a pack sets, as the spec's `norms`: `{rule: {value, cite}}`. The cite
/// names pack, document and clause, and says `unverified` for a number not checked against
/// its source; a number outside the rule's range is left to the layers below.
pub fn rule_norms(p: &NormPack) -> J {
    ddc_gen::project::rules::norms_from(|k| p.dim_cite(k))
}

/// The spec with the norms of the chosen pack: `pack` when given, else the project's stored
/// choice; a spec that already carries `norms`, or a project without a choice, stays as is.
pub fn with_pack_norms(c: &Core, pack: Option<&str>, mut spec: J) -> Result<J, String> {
    if !spec["norms"].is_null() || !spec.is_object() {
        return Ok(spec);
    }
    let id = match pack {
        Some(p) => p.to_string(),
        None => c.ed.model.norm_locale().map_err(|e| e.to_string())?.pack,
    };
    if id.is_empty() {
        return Ok(spec);
    }
    let set = PackSet::embedded();
    if !set.has(&id) {
        return Err(format!("no norm pack {id:?}; packs: {}", set.ids().join(", ")));
    }
    let p = set.resolve(&id).map_err(|e| e.to_string())?;
    spec["norms"] = rule_norms(&p);
    Ok(spec)
}

fn head(o: &mut Out, p: &NormPack) {
    o.put("pack", &json!(p.id));
    o.put("chain", &json!(p.chain));
    o.put("status", &p.meta["status"]);
}

fn depth_of(a: &J) -> Result<u8, String> {
    match a["depth"].as_u64() {
        None => Ok(5),
        Some(d @ 1..=5) => Ok(d as u8),
        Some(d) => Err(format!("depth {d} is not 1..5")),
    }
}

fn source_json(p: &NormPack, id: &str) -> J {
    match p.source(id) {
        Some(s) => json!({"id": s.id, "title": s.title, "licence": s.licence, "verified": s.verified}),
        None => json!({"id": id}),
    }
}

fn variant_json(p: &NormPack, v: &Variant) -> J {
    let mut raw = v.raw.clone();
    raw["source"] = source_json(p, &v.source);
    raw["clause"] = json!(v.clause);
    raw["verified"] = json!(v.verified);
    raw
}

fn norms_lookup(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let (set, p) = pack_of(c, a)?;
    let mut o = Out::new(c.max_chars);
    let topic = a["topic"].as_str().unwrap_or("rules");
    let q = a["query"].as_str().unwrap_or("").to_lowercase();
    let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
    if topic == "packs" {
        let ids: Vec<J> = set.ids().iter().map(|i| json!(i)).collect();
        o.list("packs", &ids);
        return Ok(o.finish().into());
    }
    head(&mut o, &p);
    match topic {
        "rules" => {
            let depth = depth_of(a)?;
            let disc = a["discipline"].as_str().unwrap_or("*");
            let typ = a["typology"].as_str();
            let rk = a["room_kind"].as_str();
            let items: Vec<J> = p
                .rules(depth, disc)
                .into_iter()
                .filter(|r| typ.is_none_or(|t| r.applies.typology.is_empty() || r.applies.typology.iter().any(|x| x == t)))
                .filter(|r| rk.is_none_or(|k| r.applies.room_kind.is_empty() || r.applies.room_kind.iter().any(|x| x == k)))
                .filter(|r| hit(&r.id) || r.text.values().any(|t| hit(t)))
                .map(|r| {
                    let engine = matches!(r.check, PackCheck::Legacy(_));
                    json!({
                        "engine_rule": engine,
                        "id": r.id, "pack": r.pack, "text": r.text, "kind": r.kind,
                        "source": source_json(&p, &r.source), "clause": r.clause,
                        "value": r.value, "unit": r.unit, "verified": r.verified,
                        "example": r.example, "depth": r.depth, "discipline": r.discipline,
                        "applies": r.raw["applies"], "check": r.raw["check"],
                    })
                })
                .collect();
            o.put("total", &json!(items.len()));
            o.list("rules", &items);
        }
        "dims" => {
            let items: Vec<J> = p
                .dims
                .iter()
                .filter(|d| hit(&d.key))
                .map(|d| json!({"key": d.key, "value": d.value, "unit": d.unit, "source": source_json(&p, &d.source), "clause": d.clause, "verified": d.verified}))
                .collect();
            o.list("dims", &items);
        }
        "programme" => {
            let items: Vec<J> = p
                .programmes
                .iter()
                .filter(|g| a["typology"].as_str().is_none_or(|t| programme_of(&p, t).is_ok_and(|(x, _)| x.typology == g.typology)))
                .map(|g| json!({"typology": g.typology, "pack": g.pack, "variants": g.variants.iter().map(|v| variant_json(&p, v)).collect::<Vec<_>>()}))
                .collect();
            o.list("programmes", &items);
        }
        "assemblies" => {
            o.list("assemblies", &p.assemblies);
        }
        "drawing" => {
            o.put("drawing", &p.drawing);
        }
        "sources" => {
            let items: Vec<J> = p.sources.iter().map(|s| source_json(&p, &s.id)).collect();
            o.list("sources", &items);
        }
        t => return Err(format!("unknown topic {t:?}")),
    }
    Ok(o.finish().into())
}

/// A finding's proposed fix as `update_elements` arguments for its refs: the agent drafts it
/// (a ghost preview), the person applies it.
fn fix_json(field: &str, v: f64) -> J {
    json!({"tool": "update_elements", "set": {field: (v * 1e6).round() / 1e6}})
}

fn sev(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn norms_check(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let (_, p) = pack_of(c, a)?;
    let scope = scope_of(c, &a["scope"])?;
    let stored = c.ed.model.norm_locale().map_err(|e| e.to_string())?;
    let typology = a["typology"]
        .as_str()
        .map(String::from)
        .or_else(|| (!stored.typology.is_empty()).then(|| stored.typology.clone()));
    let ctx = PackCtx {
        depth: depth_of(a)?,
        discipline: a["discipline"].as_str().unwrap_or("*").to_string(),
        typology: typology.clone(),
        region: (!stored.region.is_empty()).then(|| stored.region.clone()),
        sprinklered: None,
        height: None,
    };
    let rep = run_pack(&c.ed.model, &p, &scope, &ctx).map_err(|e| e.to_string())?;
    let mut o = Out::new(c.max_chars);
    head(&mut o, &p);
    o.put("typology", &json!(typology));
    o.put("hard", &json!(rep.hard().count()));
    o.put("advice", &json!(rep.findings.len() - rep.hard().count()));
    o.put("rules_ran", &json!(rep.ran));
    let skipped: Vec<J> = rep.skipped.iter().map(|(id, why)| json!({"rule": id, "why": why})).collect();
    o.put("rules_skipped", &json!(skipped));
    let items: Vec<J> = rep
        .findings
        .iter()
        .map(|f| {
            let mut j = json!({
                "rule": f.rule_id, "pack": f.pack, "severity": sev(f.severity), "hard": f.hard,
                "what": f.what, "detail": f.detail,
                "refs": f.elements.iter().map(|(t, i)| format!("{t}:{i}")).collect::<Vec<_>>(),
                "measured": f.measured, "required": f.required, "unit": f.unit,
                "document": f.source, "clause": f.clause,
            });
            if let Some((field, v)) = &f.fix {
                j["fix"] = fix_json(field, *v);
            }
            j
        })
        .collect();
    o.list("findings", &items);
    o.put(
        "note",
        &json!("Findings with hard=false come from numbers not verified against the source: advice only. No findings is not proof of compliance. A `fix` holds update_elements arguments for the finding's refs: draft it, the person applies."),
    );
    Ok(o.finish().into())
}

/// A pack programme by typology, or by the name or alias of a built-in preset, which is built
/// as its base typology and so answers with that typology's norm. The error names what the
/// pack offers, so the caller picks instead of guessing.
fn programme_of<'a>(p: &'a NormPack, typ: &str) -> Result<(&'a Programme, Option<&'static Preset>), String> {
    if let Some(g) = p.programme(typ) {
        return Ok((g, None));
    }
    let preset = presets::find(typ);
    if let Some(g) = preset.and_then(|t| p.programme(t.base.name())) {
        return Ok((g, preset));
    }
    let have: Vec<&str> = p.programmes.iter().map(|g| g.typology.as_str()).collect();
    let built = preset.map(|t| format!(" (preset {} is built as {})", t.name, t.base.name())).unwrap_or_default();
    Err(format!("pack {} has no programme for {typ:?}{built}; it has: {}", p.id, have.join(", ")))
}

fn norms_programme(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let (_, p) = pack_of(c, a)?;
    let typ = a["typology"].as_str().ok_or("typology is required")?;
    let (prog, preset) = programme_of(&p, typ)?;
    let rooms = a["rooms"].as_u64().map(|n| n as usize);
    let mut o = Out::new(c.max_chars);
    head(&mut o, &p);
    o.put("typology", &json!(prog.typology));
    if let Some(t) = preset {
        o.put(
            "preset",
            &json!({"name": t.name, "built_as": t.base.name(), "note": t.note,
                "advice": "the pack holds the norm of the base typology; the preset's own rooms come from gen_preset"}),
        );
    }
    match rooms {
        Some(n) => match prog.variant(Some(n)) {
            Some(v) => {
                o.put("variant", &variant_json(&p, v));
            }
            None => {
                let codes: Vec<&str> = prog.variants.iter().map(|v| v.code.as_str()).collect();
                return Err(format!("no {typ} variant for {n} rooms; variants: {}", codes.join(", ")));
            }
        },
        None => {
            let all: Vec<J> = prog.variants.iter().map(|v| variant_json(&p, v)).collect();
            o.list("variants", &all);
        }
    }
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_gen::project::{nav, parse_spec, rules::Layer};

    fn norms(id: &str) -> J {
        rule_norms(&PackSet::embedded().resolve(id).unwrap())
    }

    #[test]
    fn every_pack_sets_the_check_rules_with_a_cited_clause() {
        let set = PackSet::embedded();
        for id in set.ids() {
            let n = rule_norms(&set.resolve(id).unwrap());
            for rule in ["egress.max_travel", "light.glazing_floor", "access.route_width", "access.door_width", "access.turning", "fire.compartment_area"] {
                let e = &n[rule];
                assert!(e["value"].is_number() || e["unlimited"] == json!(true), "{id}: {rule} unset: {n}");
                let cite = e["cite"].as_str().unwrap_or("");
                assert!(cite.contains(", ") && cite.len() > 12, "{id}: {rule} cites {cite:?}");
            }
            assert!(n["light.depth_ratio"].is_object() && n["mep.riser_offset"].is_object(), "{id}");
        }
    }

    #[test]
    fn choosing_a_pack_changes_the_rules_and_findings_cite_its_clause() {
        let (de, ru) = (norms("de"), norms("ru"));
        // a number checked against its source carries no mark; one not checked says so
        assert_eq!(de["egress.max_travel"]["value"], json!(35.0));
        let c = de["egress.max_travel"]["cite"].as_str().unwrap();
        assert!(c.starts_with("de egress.travel_max") && c.contains("par. 35(2)") && !c.contains("unverified"), "{c}");
        assert_ne!(ru["egress.max_travel"]["value"], de["egress.max_travel"]["value"]);
        assert!(norms("generic")["egress.max_travel"]["cite"].as_str().unwrap().ends_with("unverified"));
        assert_ne!(norms("us")["access.route_width"]["value"], ru["access.route_width"]["value"]);
        let brief = |n: &J| {
            json!({"typology": "office", "floors": 2, "norms": n,
                   "rooms": [{"kind": "office", "area": 30, "count": 10}, {"kind": "wc", "area": 4, "count": 2}]})
        };
        let s_de = parse_spec(&brief(&de)).unwrap();
        let s_ru = parse_spec(&brief(&ru)).unwrap();
        assert_eq!(s_de.rules.source("egress.max_travel"), Some(Layer::Norm));
        assert_eq!(s_de.rules.get("egress.max_travel"), 35.0);
        assert_ne!(s_de.rules.get("egress.max_travel"), s_ru.rules.get("egress.max_travel"));
        // a pack limit the plan breaks: the finding names the pack's clause
        let mut tight = de.clone();
        tight["egress.max_travel"]["value"] = json!(15.0);
        // one stair: the generator does not cure the travel with more
        let mut b = brief(&tight);
        b["rules"] = json!({"egress.stairs_max": 1});
        let s = parse_spec(&b).unwrap();
        let p = ddc_gen::project::build_project(&s).unwrap();
        let f = nav::check(&s, &p);
        let x = f.iter().find(|x| x.rule == "egress.max_travel").unwrap_or_else(|| panic!("{f:?}"));
        assert!(x.line().contains("[norm de egress.travel_max") && x.line().contains("par. 35(2)"), "{}", x.line());
    }

    #[test]
    fn a_pack_without_a_limit_turns_the_check_off_until_the_brief_sets_one() {
        // uk: table 8.1 sets no compartment area for offices
        let uk = norms("uk");
        let e = &uk["fire.compartment_area"];
        assert_eq!((&e["value"], &e["unlimited"]), (&J::Null, &json!(true)), "{e}");
        assert!(e["cite"].as_str().unwrap().contains("table 8.1"), "{e}");
        let brief = |n: &J, rules: J| {
            json!({"typology": "office", "floors": 2, "norms": n, "rules": rules,
                   "rooms": [{"kind": "office", "area": 30, "count": 10}, {"kind": "wc", "area": 4, "count": 2}]})
        };
        let s = parse_spec(&brief(&uk, json!({}))).unwrap();
        assert!(s.rules.unlimited("fire.compartment_area").is_some_and(|c| c.contains("table 8.1")));
        let p = ddc_gen::project::build_project(&s).unwrap();
        assert!(!nav::check(&s, &p).iter().any(|x| x.rule == "fire.compartment_area"));
        // a brief limit sits above the norm and checks again (fire walls off, so it has work)
        let s = parse_spec(&brief(&uk, json!({"fire.compartment_area": 200, "fire.walls": false}))).unwrap();
        assert!(s.rules.unlimited("fire.compartment_area").is_none());
        let p = ddc_gen::project::build_project(&s).unwrap();
        assert!(nav::check(&s, &p).iter().any(|x| x.rule == "fire.compartment_area"));
    }
}
