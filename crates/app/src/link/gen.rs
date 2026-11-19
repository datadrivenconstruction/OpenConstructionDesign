// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc gen`: a `ddc-brief/1` document becomes a generator spec, the spec becomes a model.
//!
//! Every brief field ends up in one of three places: used (`mapped`), or named in `unsupported`
//! with the reason. Values the mapping had to derive are in `derived`. Nothing is dropped
//! silently and no number is invented: a missing room area is derived from the unit area with
//! the weights of the generator's own apartment template, and said so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ddc_gen::project::{self, merge_patch, ProjectSpec};
use serde_json::{json, Map, Value};

use crate::ai::plan::{compile_one, CompileCx, PlanOp};
use crate::ai::project_plan::{self, ProjectOp};
use crate::editor::Editor;

#[derive(Debug)]
pub enum GenError {
    /// The brief does not follow `ddc-brief/1`.
    Schema(Vec<String>),
    /// Values the generator needs are not confirmed by a person.
    Unconfirmed(Vec<String>),
    /// The brief asks for something the generator cannot build.
    Unsupported(String),
    /// The generator spec or the building was refused.
    Spec(String),
    Save(String),
}

impl GenError {
    pub fn kind(&self) -> &'static str {
        match self {
            GenError::Schema(_) => "brief_invalid",
            GenError::Unconfirmed(_) => "unconfirmed",
            GenError::Unsupported(_) => "unsupported",
            GenError::Spec(_) => "spec",
            GenError::Save(_) => "save",
        }
    }
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::Schema(v) => write!(f, "the brief does not follow ddc-brief/1: {}", v.join("; ")),
            GenError::Unconfirmed(v) => write!(
                f,
                "values the generator needs are not confirmed (status proposed or default): {}",
                v.join(", ")
            ),
            GenError::Unsupported(s) | GenError::Spec(s) | GenError::Save(s) => f.write_str(s),
        }
    }
}

/// The spec and what became of every brief field.
#[derive(Debug, Default)]
pub struct Mapped {
    pub spec: Value,
    /// JSON pointers of the brief the spec was built from.
    pub mapped: Vec<String>,
    /// Pointers the generator has no input for, with the reason.
    pub unsupported: Vec<Value>,
    /// Values the mapping computed or approximated.
    pub derived: Vec<Value>,
}

/// Room kinds with an area in the generator's two-room apartment template (m2).
fn template_area(kind: &str) -> Option<f64> {
    Some(match kind {
        "living" => 18.0,
        "bedroom" => 13.0,
        "kitchen" => 10.0,
        "bath" => 4.4,
        "wc" => 1.8,
        _ => return None,
    })
}

struct Cx<'a> {
    b: &'a Value,
    used: BTreeSet<String>,
    out: Mapped,
}

impl Cx<'_> {
    fn take(&mut self, ptr: &str) -> Option<Value> {
        let v = self.b.pointer(ptr).filter(|v| !v.is_null()).cloned();
        if v.is_some() {
            self.used.insert(ptr.to_string());
        }
        v
    }

    fn derive(&mut self, ptr: &str, what: impl Into<String>) {
        self.out.derived.push(json!({"pointer": ptr, "what": what.into()}));
    }
}

const KNOWN: [&str; 4] = ["house", "apartment_section", "office", "school"];

fn typology(cx: &mut Cx) -> Result<String, GenError> {
    if let Some(t) = cx.take("/generator/template") {
        let t = t.as_str().unwrap_or_default().to_string();
        return if KNOWN.contains(&t.as_str()) {
            Ok(t)
        } else {
            Err(GenError::Unsupported(format!(
                "generator.template '{t}' has no generator here (known: {})",
                KNOWN.join(", ")
            )))
        };
    }
    let c = cx.take("/use/category").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    Ok(match c.as_str() {
        "residential_single" => "house",
        "residential_multi" => "apartment_section",
        "office" => "office",
        "school" | "kindergarten" => "school",
        other => {
            return Err(GenError::Unsupported(format!(
                "use.category '{other}' has no generator template: pin generator.template to one of {}",
                KNOWN.join(", ")
            )))
        }
    }
    .to_string())
}

fn room(cx: &mut Cx, ptr: &str, r: &Value, area: Option<f64>) -> Value {
    let mut o = Map::new();
    for k in ["kind", "name", "count"] {
        if cx.take(&format!("{ptr}/{k}")).is_some() {
            o.insert(k.into(), r[k].clone());
        }
    }
    if let Some(a) = area {
        o.insert("area".into(), json!(a));
    }
    Value::Object(o)
}

/// Rooms of one unit with their areas: the stated ones, the rest from the generator's own
/// apartment template. The unit area is not an input of the generator (it sizes rooms, then adds
/// hall, walls and circulation); the log compares the two.
fn unit_rooms(cx: &mut Cx, ptr: &str, unit: &Value) -> Vec<Value> {
    let rooms = unit["rooms"].as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    let mut total = 0.0;
    for (j, r) in rooms.iter().enumerate() {
        let rp = format!("{ptr}/rooms/{j}");
        let k = r["kind"].as_str().unwrap_or_default();
        let area = match r["area_m2"].as_f64() {
            Some(a) => {
                cx.used.insert(format!("{rp}/area_m2"));
                a
            }
            None => {
                let (a, how) = match template_area(k) {
                    Some(a) => (a, "area of the generator's apartment template"),
                    None => (
                        project::limits::ROOM_AREA.min,
                        "smallest area the generator accepts; the builder sizes this room",
                    ),
                };
                cx.derive(&format!("{rp}/area_m2"), format!("{a} m2: {how}"));
                a
            }
        };
        if k != "hall" {
            total += area * r["count"].as_u64().unwrap_or(1) as f64;
        }
        out.push(room(cx, &rp, r, Some(area)));
    }
    if let Some(u) = unit["area_m2"].as_f64() {
        let up = format!("{ptr}/area_m2");
        cx.out.unsupported.push(json!({
            "pointer": up,
            "reason": format!("the generator sizes rooms, not units: rooms add up to {total:.1} m2 against {u} m2 asked, hall and walls come on top"),
        }));
        cx.used.insert(up);
    }
    out
}

/// Maps the brief to a generator spec without building anything.
pub fn map_brief(brief: &Value) -> Result<Mapped, GenError> {
    let mut cx = Cx { b: brief, used: BTreeSet::new(), out: Mapped::default() };
    let typ = typology(&mut cx)?;
    let mut spec = Map::new();
    spec.insert("typology".into(), json!(typ));
    if let Some(n) = cx.take("/name") {
        spec.insert("name".into(), n);
    }
    if let Some(l) = cx.take("/locale") {
        let l = l.as_str().unwrap_or_default();
        let lang = if l == "ru" || l.starts_with("ru-") { "ru" } else { "en" };
        if l != lang && !l.starts_with(&format!("{lang}-")) {
            cx.derive("/locale", format!("locale '{l}' is not a generator language: names are written in English"));
        }
        spec.insert("lang".into(), json!(lang));
    }
    let floors = cx.take("/massing/floors_above").and_then(|v| v.as_u64()).unwrap_or(1);
    spec.insert("floors".into(), json!(floors));
    if let Some(h) = cx.take("/massing/floor_height_m") {
        spec.insert("floor_height".into(), h);
    }
    let (w, d) = (cx.take("/massing/footprint/width_m"), cx.take("/massing/footprint/depth_m"));
    if w.is_some() || d.is_some() {
        spec.insert("footprint".into(), json!({"width": w, "depth": d}));
    }
    if let Some(l) = cx.take("/massing/lift") {
        spec.insert("lift".into(), l);
    }
    if let Some(g) = cx.take("/structure/grid_m") {
        spec.insert("grid".into(), g);
    }
    // piles are not built: the pointer stays unused and is reported as such
    let found = brief.pointer("/structure/foundation").and_then(|v| v.as_str()).map(String::from);
    match found.as_deref() {
        Some(k @ ("raft" | "strip")) => {
            cx.take("/structure/foundation");
            spec.insert("foundation".into(), json!(k));
            let what = if k == "raft" { "raft 0.40 m thick under the footprint (brief gives no thickness)" } else { "strips 0.60 m wide, 0.50 m deep under exterior and party walls (brief gives no size)" };
            cx.derive("/structure/foundation", what.to_string());
        }
        Some(_) | None => {}
    }
    if let Some(r) = cx.take("/envelope/roof") {
        let k = r.as_str().unwrap_or_default();
        let kind = if k == "flat" { "flat" } else { "pitched" };
        if k != "flat" && k != "gable" {
            cx.derive("/envelope/roof", format!("'{k}' is built as a pitched roof"));
        }
        let mut roof = Map::new();
        roof.insert("kind".into(), json!(kind));
        if let Some(s) = cx.take("/envelope/roof_slope_deg") {
            roof.insert("slope".into(), s);
        }
        spec.insert("roof".into(), Value::Object(roof));
    } else if let Some(s) = cx.take("/envelope/roof_slope_deg") {
        spec.insert("roof".into(), json!({"slope": s}));
    }

    if let Some(units) = brief.pointer("/programme/units").and_then(|v| v.as_array()).cloned() {
        if typ == "apartment_section" || typ == "house" {
            let mut apts = vec![];
            for (i, u) in units.iter().enumerate() {
                let p = format!("/programme/units/{i}");
                cx.used.insert(format!("{p}/name"));
                cx.used.insert(format!("{p}/count"));
                let total = u["count"].as_u64().unwrap_or(1);
                // the brief counts the units of the whole building, the spec those of one floor
                let per = total.div_ceil(floors.max(1)).max(1);
                if per * floors != total {
                    cx.derive(
                        &format!("{p}/count"),
                        format!("{total} units over {floors} floors: {per} per floor, {} in all", per * floors),
                    );
                }
                let rooms = unit_rooms(&mut cx, &p, u);
                apts.push(json!({"name": u["name"], "count": per, "rooms": rooms}));
            }
            spec.insert("apartments".into(), Value::Array(apts));
        } else {
            for i in 0..units.len() {
                cx.out.unsupported.push(json!({
                    "pointer": format!("/programme/units/{i}"),
                    "reason": format!("the {typ} generator takes rooms, not dwelling units"),
                }));
                cx.used.insert(format!("/programme/units/{i}"));
            }
        }
    }
    if let Some(spaces) = brief.pointer("/programme/spaces").and_then(|v| v.as_array()).cloned() {
        let (mut all, mut ground) = (vec![], vec![]);
        for (i, s) in spaces.iter().enumerate() {
            let p = format!("/programme/spaces/{i}");
            let floors_of: Vec<i64> = s["floors"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default();
            cx.used.insert(format!("{p}/floors"));
            let area = s["area_m2"].as_f64();
            let Some(area) = area else {
                cx.out.unsupported.push(json!({"pointer": p, "reason": "no area_m2: the generator needs the area of every room"}));
                cx.used.insert(p);
                continue;
            };
            cx.used.insert(format!("{p}/area_m2"));
            let r = room(&mut cx, &p, s, Some(area));
            match floors_of.as_slice() {
                [] => all.push(r),
                [0] => ground.push(r),
                f if f.iter().all(|x| *x < 0) => {
                    cx.out.unsupported.push(json!({"pointer": p, "reason": "storeys below ground are not generated"}));
                }
                _ => {
                    cx.out.unsupported.push(json!({"pointer": p, "reason": "the generator places a room on every floor or on the ground floor only"}));
                }
            }
        }
        if !all.is_empty() {
            spec.insert("rooms".into(), Value::Array(all));
        }
        if !ground.is_empty() {
            spec.insert("ground_rooms".into(), Value::Array(ground));
        }
    }
    let mut spec = Value::Object(spec);
    if let Some(o) = cx.take("/generator/spec_overrides") {
        merge_patch(&mut spec, &o);
        cx.derive("/generator/spec_overrides", "merged into the spec as written, last");
    }

    // every leaf nothing consumed is reported
    let skip: BTreeSet<&str> =
        ["schema", "brief_id", "project_id", "version", "provenance", "questions"].into();
    let mut leaves = vec![];
    if let Some(o) = brief.as_object() {
        for (k, v) in o {
            if !skip.contains(k.as_str()) {
                leaves_of(&format!("/{k}"), v, &mut leaves);
            }
        }
    }
    let have: BTreeSet<String> =
        cx.out.unsupported.iter().filter_map(|u| u["pointer"].as_str().map(String::from)).collect();
    for p in leaves {
        let covered = cx.used.contains(&p)
            || have.iter().any(|h| p == *h || p.starts_with(&format!("{h}/")))
            || cx.used.iter().any(|u| p.starts_with(&format!("{u}/")));
        if !covered {
            cx.out.unsupported.push(json!({"pointer": p, "reason": reason(&p)}));
        }
    }
    cx.out.unsupported.sort_by(|a, b| a["pointer"].as_str().cmp(&b["pointer"].as_str()));
    cx.out.spec = spec;
    cx.out.mapped = cx.used.into_iter().collect();
    Ok(cx.out)
}

fn reason(p: &str) -> &'static str {
    match p.split('/').nth(1).unwrap_or_default() {
        "site" => "the generator has no site model: the plot, setbacks and limits are not used",
        "budget" | "quality_level" => "cost is not an input of the generator",
        "structure" => "the generator builds one structural system",
        "country" | "norm_pack" | "depth_level" => "norms and depth are applied in the editor, not by the generator",
        "generator" => "the generator has no such setting",
        _ => "the generator has no input for this field",
    }
}

fn leaves_of(ptr: &str, v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) if !o.is_empty() => {
            for (k, x) in o {
                leaves_of(&format!("{ptr}/{k}"), x, out);
            }
        }
        Value::Array(a) if !a.is_empty() && a.iter().all(|x| x.is_object()) => {
            for (i, x) in a.iter().enumerate() {
                leaves_of(&format!("{ptr}/{i}"), x, out);
            }
        }
        Value::Null => {}
        _ => out.push(ptr.to_string()),
    }
}

/// Pointers the generator uses whose provenance says a person has not confirmed them.
pub fn unconfirmed(brief: &Value, mapped: &[String]) -> Vec<String> {
    let Some(pv) = brief["provenance"].as_object() else { return vec![] };
    let mut out = BTreeSet::new();
    for (k, v) in pv {
        if v["status"].as_str() == Some("confirmed") {
            continue;
        }
        let hit = mapped.iter().any(|m| m == k || m.starts_with(&format!("{k}/")) || k.starts_with(&format!("{m}/")));
        if hit {
            out.insert(format!("{k} ({})", v["status"].as_str().unwrap_or("unknown")));
        }
    }
    out.into_iter().collect()
}

#[derive(Debug)]
pub struct Report {
    pub spec: Value,
    pub mapped: Vec<String>,
    pub unsupported: Vec<Value>,
    pub derived: Vec<Value>,
    pub summary: Value,
    pub counts: BTreeMap<String, i64>,
    /// What was built under the building (`kind`, `count`, `keys`, sizes); `None` when the brief has none.
    pub foundation: Option<Value>,
}

/// Checks the brief, maps it, builds the project and saves it to `out` (a `.ddc`).
pub fn generate(brief: &Value, out: &Path) -> Result<Report, GenError> {
    ddc_io::contract::validate(ddc_io::contract::Doc::Brief, brief).map_err(GenError::Schema)?;
    let m = map_brief(brief)?;
    let bad = unconfirmed(brief, &m.mapped);
    if !bad.is_empty() {
        return Err(GenError::Unconfirmed(bad));
    }
    // the parse, the build and the gates: the full text of what is wrong
    let built = project_plan::build(&m.spec).map_err(GenError::Spec)?;
    let _: &ProjectSpec = &built.spec;
    let summary = project_plan::summary(&built);
    let mut ed = Editor::new_in_memory().map_err(|e| GenError::Spec(e.to_string()))?;
    ed.reload().map_err(|e| GenError::Spec(e.to_string()))?;
    let op = PlanOp::Project(ProjectOp { input: m.spec.clone(), level: ed.level, slot0: 0, pid: "P1".into() });
    let mut slots = std::collections::HashMap::new();
    let sets = crate::ai::sets::Sets::default();
    {
        let mut cx = CompileCx {
            ed: &mut ed,
            slots: &mut slots,
            sets: &sets,
            gens: &[],
            rules: &[],
            declared: Default::default(),
            skips: vec![],
        };
        // the scratch copy is the model: the compiled steps are already written to it
        compile_one(&mut cx, &op).map_err(|e| GenError::Spec(e.0))?;
    }
    let foundation = crate::link::foundation::build(&mut ed, &built.spec, &built.project).map_err(GenError::Spec)?;
    let mut counts = BTreeMap::new();
    for (k, n) in crate::headless_file::counts(&ed.model) {
        counts.insert(k.to_string(), n);
    }
    ed.save_project_as(out).map_err(|e| GenError::Save(e.to_string()))?;
    Ok(Report {
        spec: m.spec,
        mapped: m.mapped,
        unsupported: m.unsupported,
        derived: m.derived,
        summary,
        counts,
        foundation,
    })
}
