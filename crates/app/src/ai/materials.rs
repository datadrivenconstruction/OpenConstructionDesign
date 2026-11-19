//! Material names an AI draft carries, matched against the project's own list. A typo or a case
//! slip would otherwise make Apply create a new material with default properties.

use super::plan::{NewElem, PlanItem, PlanOp};
use crate::query::Val;
use ddc_model::PipeMaterial;
use super::sets::norm_cat;
use super::validate::{IssueLevel, PlanIssue};
use ddc_model::Model;
use serde_json::{json, Value as J};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    /// The exact name of an existing material.
    Known,
    /// Differs only by case or edge spaces: this existing name is meant.
    Mapped(String),
    /// Not in the project; the closest existing name, when one is near enough.
    Unknown(Option<String>),
}

/// Names of the materials the project has. `Model::materials` also lists a name that only a
/// layer or an element uses (no row of its own, id 0), so those are included.
pub fn names(m: &Model) -> Vec<String> {
    m.materials()
        .map(|v| v.into_iter().map(|x| x.spec.name).collect())
        .unwrap_or_default()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn closest(known: &[String], text: &str) -> Option<String> {
    let want = text.to_lowercase();
    let limit = (want.chars().count() / 3).max(1);
    known
        .iter()
        .map(|k| (distance(&want, &k.to_lowercase()), k))
        .filter(|(d, _)| *d <= limit)
        .min_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)))
        .map(|(_, k)| k.clone())
}

pub fn classify(known: &[String], text: &str) -> Match {
    if known.is_empty() || known.iter().any(|k| k == text) {
        return Match::Known;
    }
    let want = text.trim().to_lowercase();
    if let Some(k) = known.iter().find(|k| k.to_lowercase() == want) {
        return Match::Mapped(k.clone());
    }
    Match::Unknown(closest(known, text.trim()))
}

/// The existing name for a case-only difference; any other text stays as given.
pub fn canon(known: &[String], text: &str) -> String {
    match classify(known, text) {
        Match::Mapped(k) => k,
        _ => text.to_string(),
    }
}

/// Which list a written name is checked against.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Src {
    Project,
    Pipe,
}

/// Materials a plan item writes as free text.
fn written(op: &PlanOp) -> Vec<(&str, Src)> {
    match op {
        PlanOp::NewType(t) => match norm_cat(&t.cat) {
            Some("column" | "beam") => vec![(t.material.as_str(), Src::Project)],
            Some("wall" | "floor" | "roof" | "ceiling") => t
                .layers
                .iter()
                .map(|l| (l.material.as_str(), Src::Project))
                .collect(),
            _ => vec![],
        },
        PlanOp::Assign { set, .. } => set
            .iter()
            .filter(|a| a.key.eq_ignore_ascii_case("material"))
            .filter_map(|a| match &a.val {
                Val::Lit(t) => Some((t.as_str(), Src::Project)),
                Val::Rel(..) => None,
            })
            .collect(),
        PlanOp::Create { items, .. } => items
            .iter()
            .filter_map(|e| match e {
                NewElem::Pipe { material, .. } => Some((material.as_str(), Src::Pipe)),
                _ => None,
            })
            .collect(),
        PlanOp::WallType(super::type_lib_plan::WallTypeOp::Layer {
            act: super::type_lib_plan::LayerAct::Add { layer, .. },
            ..
        }) => vec![(layer.material.as_str(), Src::Project)],
        _ => vec![],
    }
}

fn pipe_names() -> Vec<String> {
    PipeMaterial::ALL.iter().map(|m| m.name().to_string()).collect()
}

/// The pipe material a written name means (trimmed, case-insensitive); the preview and Apply both use it.
pub fn pipe_material(text: &str) -> Option<PipeMaterial> {
    let want = canon(&pipe_names(), text);
    PipeMaterial::ALL.iter().copied().find(|m| m.name() == want)
}

/// A warning for each name the project does not have, with the closest one when there is one.
/// `known` is the list from before the plan, so an earlier item's typo does not excuse a later one.
pub fn issues(items: &[PlanItem], known: &[String]) -> Vec<PlanIssue> {
    let mut out: Vec<PlanIssue> = vec![];
    let pipes = pipe_names();
    for it in items {
        let mut seen: Vec<&str> = vec![];
        for (text, src) in written(&it.op) {
            if seen.contains(&text) {
                continue;
            }
            seen.push(text);
            let list = if src == Src::Pipe { &pipes[..] } else { known };
            let Match::Unknown(near) = classify(list, text) else {
                continue;
            };
            let mut args = vec![text.to_string()];
            let key = match near {
                Some(n) => {
                    args.push(n);
                    "ai.issue.material_suggest"
                }
                None => "ai.issue.material_unknown",
            };
            out.push(PlanIssue {
                level: IssueLevel::Warn,
                item: Some(it.id),
                el: None,
                text_key: key,
                args,
            });
        }
    }
    out
}

/// Rows of `list_materials`: name, color, hatch, density, and how many element rows use it.
pub fn rows(m: &Model) -> Vec<J> {
    m.materials()
        .unwrap_or_default()
        .into_iter()
        .map(|x| {
            let used: usize = m
                .material_usage(&x.spec.name)
                .map(|u| u.iter().map(|(_, n)| n).sum())
                .unwrap_or(0);
            json!({
                "name": x.spec.name,
                "color": x.spec.color.map(|c| format!("#{c:06x}")),
                "hatch": x.spec.hatch,
                "density": x.spec.density.map(super::fmt::num),
                "used": used,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::ai::plan::{ElRef, NewType, Targets};
    use crate::query::Assign;
    use ddc_model::{LayerSpec, MaterialSpec};

    fn known() -> Vec<String> {
        ["concrete", "brick", "Steel"].map(String::from).to_vec()
    }

    fn type_item(layers: &[&str]) -> PlanItem {
        PlanItem {
            id: 3,
            title: "t".into(),
            why: String::new(),
            op: PlanOp::NewType(NewType {
                slot: 1,
                cat: "wall".into(),
                name: "W".into(),
                layers: layers
                    .iter()
                    .map(|m| LayerSpec {
                        material: (*m).to_string(),
                        thickness: 0.1,
                    })
                    .collect(),
                shape: None,
                material: String::new(),
                family: None,
                values: vec![],
            }),
            checked: true,
            depends_on: vec![],
            actor: "chat".into(),
            conflict: None,
            sets: Default::default(),
        }
    }

    #[test]
    fn names_include_a_material_only_a_layer_uses() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lay = LayerSpec {
            material: "LayerOnly".into(),
            thickness: 0.2,
        };
        ed.model.add_wall_type("T", &[lay]).unwrap();
        assert!(names(&ed.model).iter().any(|n| n == "LayerOnly"));
    }

    #[test]
    fn list_materials_reports_usage() {
        let mut ed = Editor::new_in_memory().unwrap();
        let spec = MaterialSpec {
            name: "Zorb".into(),
            color: Some(0xaabbcc),
            hatch: "dots".into(),
            density: Some(900.0),
            conductivity: None,
            specific_heat: None,
            note: String::new(),
        };
        ed.model.set_material("Zorb", &spec).unwrap();
        let got = rows(&ed.model);
        let z = got.iter().find(|r| r["name"] == "Zorb").expect("Zorb listed");
        assert_eq!(z["color"], "#aabbcc");
        assert_eq!(z["hatch"], "dots");
        assert_eq!(z["used"], 0);
        for r in &got {
            let name = r["name"].as_str().unwrap();
            let n: usize = ed
                .model
                .material_usage(name)
                .unwrap()
                .iter()
                .map(|(_, n)| n)
                .sum();
            assert_eq!(r["used"], n, "{name}");
        }
    }

    #[test]
    fn unknown_material_warns_with_suggestion() {
        let got = issues(&[type_item(&["concret"])], &known());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].level, IssueLevel::Warn);
        assert_eq!(got[0].item, Some(3));
        assert_eq!(got[0].text_key, "ai.issue.material_suggest");
        assert_eq!(got[0].args, vec!["concret", "concrete"]);
        let far = issues(&[type_item(&["unobtainium"])], &known());
        assert_eq!(far[0].text_key, "ai.issue.material_unknown");
        assert_eq!(far[0].args, vec!["unobtainium"]);
    }

    #[test]
    fn case_only_difference_maps_to_existing() {
        assert_eq!(classify(&known(), "Concrete"), Match::Mapped("concrete".into()));
        assert_eq!(canon(&known(), "STEEL"), "Steel");
        assert_eq!(canon(&known(), "concret"), "concret");
        assert!(issues(&[type_item(&["Concrete"])], &known())
            .iter()
            .all(|i| i.text_key != "ai.issue.material_unknown"));
    }

    #[test]
    fn known_material_no_issue() {
        assert_eq!(classify(&known(), "brick"), Match::Known);
        assert!(issues(&[type_item(&["concrete", "brick"])], &known()).is_empty());
    }

    fn assign_item(val: &str) -> PlanItem {
        let mut it = type_item(&[]);
        it.op = PlanOp::Assign {
            target: Targets::Refs(vec![ElRef::parse("wall:7").unwrap()]),
            set: vec![Assign {
                key: "material".into(),
                val: Val::Lit(val.into()),
            }],
        };
        it
    }

    fn pipe_item(material: &str) -> PlanItem {
        let mut it = type_item(&[]);
        it.op = PlanOp::Create {
            slot0: 1,
            items: vec![NewElem::Pipe {
                system: ElRef::parse("mep_system:2").unwrap(),
                level: 1,
                material: material.into(),
                dn: 50,
                points: vec![(0.0, 0.0, 2.5), (3.0, 0.0, 2.5)],
                section: None,
            }],
        };
        it
    }

    #[test]
    fn update_material_typo_warns() {
        let got = issues(&[assign_item("concret")], &known());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].text_key, "ai.issue.material_suggest");
        assert_eq!(got[0].args, vec!["concret", "concrete"]);
        assert!(issues(&[assign_item("brick")], &known()).is_empty());
    }

    #[test]
    fn pipe_unknown_material_warns() {
        let got = issues(&[pipe_item("unobtainium")], &known());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].level, IssueLevel::Warn);
        assert_eq!(got[0].text_key, "ai.issue.material_unknown");
        assert_eq!(got[0].args, vec!["unobtainium"]);
    }

    #[test]
    fn pipe_known_material_no_issue() {
        assert!(issues(&[pipe_item("steel")], &known()).is_empty());
        assert!(issues(&[pipe_item("Steel")], &known()).is_empty());
    }

    #[test]
    fn pipe_material_padded_preview_and_apply_agree() {
        assert!(issues(&[pipe_item(" Steel ")], &known()).is_empty());
        assert_eq!(pipe_material(" Steel "), pipe_material("Steel"));
        assert!(pipe_material(" Steel ").is_some());
    }

    #[test]
    fn pipe_material_case_only_applies() {
        assert_eq!(pipe_material("STEEL"), pipe_material("steel"));
        assert!(pipe_material("sTeEl").is_some());
    }

    #[test]
    fn pipe_material_unknown_refused_in_preview() {
        assert!(pipe_material("unobtainium").is_none());
        assert_eq!(issues(&[pipe_item("unobtainium")], &known()).len(), 1);
    }
}
