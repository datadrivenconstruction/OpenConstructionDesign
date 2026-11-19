//! The family tools' one contract, shared by the chat, the agent surface and tests: a family
//! definition text is data. It is checked by the geom pipeline (`component::validate`), its
//! nested families are resolved against the project, and only a text that passes becomes a
//! draft for the family editor. The model and its journal are not touched here.

use crate::catalog::{MAX_FAMILY_NAME, MAX_FAMILY_TEXT};
use crate::refusal;
use ddc_model::refusal::{Refusal as Typed, RefusalCode};
use ddc_geom::component::validate::{validate, Limits, Report, Severity};
use ddc_geom::component::ddcf;
use serde_json::{json, Value as J};

/// A refusal with a stable kind (`draft_invalid`, `text_too_large`, `name_taken`,
/// `unknown_family`) riding on one of the existing error codes.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The text failed the pipeline: the report says where and why.
    DraftInvalid(J),
    TextTooLarge { bytes: usize },
    NameTaken(String),
    UnknownFamily(String),
}

impl Refusal {
    pub fn kind(&self) -> &'static str {
        match self {
            Refusal::DraftInvalid(_) => "draft_invalid",
            Refusal::TextTooLarge { .. } => "text_too_large",
            Refusal::NameTaken(_) => "name_taken",
            Refusal::UnknownFamily(_) => "unknown_family",
        }
    }

    pub fn code(&self) -> RefusalCode {
        match self {
            Refusal::DraftInvalid(_) => RefusalCode::DraftInvalid,
            Refusal::TextTooLarge { .. } => RefusalCode::TextTooLarge,
            Refusal::NameTaken(_) => RefusalCode::NameTaken,
            Refusal::UnknownFamily(_) => RefusalCode::UnknownFamily,
        }
    }

    /// The typed refusal the registry and the UI read: code, its text key, arguments, and the
    /// report (a `draft_invalid`) or the words of the failure as detail.
    pub fn typed(&self) -> Typed {
        let r = refusal::of(self.code());
        match self {
            Refusal::DraftInvalid(rep) => r.detail(rep.to_string()),
            Refusal::TextTooLarge { bytes } => r
                .arg("n", bytes.to_string())
                .arg("max", MAX_FAMILY_TEXT.to_string())
                .detail(format!("{bytes} bytes, at most {MAX_FAMILY_TEXT}")),
            Refusal::NameTaken(n) => r
                .arg("name", n.clone())
                .detail(format!("a project family named '{n}' exists; set base_family to edit it")),
            Refusal::UnknownFamily(n) => r
                .arg("name", n.clone())
                .detail(format!("no project family '{n}'")),
        }
    }

    /// The handler's failure text: the wire form of the typed refusal.
    pub fn wire(&self) -> String {
        refusal::wire(&self.typed())
    }
}

pub fn report_json(r: &Report) -> J {
    let findings: Vec<J> = r
        .findings
        .iter()
        .map(|f| {
            json!({
                "code": f.code,
                "severity": if f.severity == Severity::Error { "error" } else { "warn" },
                "lines": f.lines,
                "msg": f.msg,
                "hint": f.hint,
            })
        })
        .collect();
    json!({
        "ok": r.ok,
        "findings": findings,
        "stats": {
            "lines": r.stats.lines, "bodies": r.stats.bodies, "types": r.stats.types,
            "params": r.stats.params, "stage": r.stats.stage, "ms": r.stats.ms,
        },
    })
}

/// The report of `text`; a text the parser refuses also carries `at`: the `line:col` of the
/// offending token from `ddcf::check`, so a repair loop knows where to edit.
fn report_at(text: &str, r: &Report) -> J {
    let mut j = report_json(r);
    if !r.ok {
        if let Err(e) = ddcf::check(text) {
            j["at"] = json!({ "line": e.line, "col": e.col, "msg": e.msg });
        }
    }
    j
}

fn too_large(text: &str) -> Result<(), Refusal> {
    if text.len() > MAX_FAMILY_TEXT {
        return Err(Refusal::TextTooLarge { bytes: text.len() });
    }
    Ok(())
}

/// `family_validate`: the report of a text, nothing kept.
pub fn check(text: &str) -> Result<J, Refusal> {
    too_large(text)?;
    Ok(report_at(text, &validate(text, &Limits::default())))
}

/// An accepted draft.
#[derive(Clone, Debug, PartialEq)]
pub struct Drafted {
    /// The draft's name, which is also its id: the editor keeps drafts by name.
    pub name: String,
    /// The text as the editor takes it: project families it nests are embedded as `begin … end`.
    pub text: String,
    pub base_family: Option<String>,
    /// The report of an accepted draft (`ok`; warnings may remain).
    pub report: J,
}

/// `family_draft`. `project` gives the definition text of a project family by name.
pub fn draft(args: &J, project: &dyn Fn(&str) -> Option<String>) -> Result<Drafted, Refusal> {
    let text = args["text"].as_str().unwrap_or_default();
    too_large(text)?;
    let base = args["base_family"].as_str().map(str::to_string);
    if let Some(b) = &base {
        if project(b).is_none() {
            return Err(Refusal::UnknownFamily(b.clone()));
        }
    }
    let mut text = text.to_string();
    // Nests name project families or blocks of this text; the project's are embedded, once each.
    let mut seen: Vec<String> = vec![];
    while let Err(e) = ddcf::check(&text) {
        let Some(fam) = e
            .msg
            .split_once("no nested family '")
            .and_then(|(_, r)| r.split_once('\''))
            .map(|(f, _)| f.to_string())
        else {
            break;
        };
        if seen.contains(&fam) {
            break;
        }
        let Some(def) = project(&fam) else {
            return Err(Refusal::UnknownFamily(fam));
        };
        text = format!("{}\nbegin\n{}\nend\n", text.trim_end(), def.trim());
        too_large(&text)?;
        seen.push(fam);
    }
    let rep = validate(&text, &Limits::default());
    let report = report_at(&text, &rep);
    if !rep.ok {
        return Err(Refusal::DraftInvalid(report));
    }
    let own = ddcf::check(&text).map(|f| f.name).unwrap_or_default();
    let name: String = args["name"]
        .as_str()
        .map(str::to_string)
        .unwrap_or(own)
        .chars()
        .take(MAX_FAMILY_NAME)
        .collect();
    if name.trim().is_empty() {
        return Err(Refusal::DraftInvalid(report));
    }
    if project(&name).is_some() && base.as_deref() != Some(name.as_str()) {
        return Err(Refusal::NameTaken(name));
    }
    Ok(Drafted {
        name,
        text,
        base_family: base,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const DESK: &str = "family Desk\ncategory furniture\nparam Width length type 1200mm\nparam Depth length type 600mm\nparam Height length type 740mm\nref Left x = -Width / 2\nref Right x = Width / 2\nref Front y = -Depth / 2\nref Back y = Depth / 2\nextrude Board rect(Left, Front, Right, Back) z(Height - 40mm, Height)\n";

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn valid_text_becomes_a_draft() {
        let d = draft(&json!({ "text": DESK }), &none).unwrap();
        assert_eq!((d.name.as_str(), d.report["ok"].clone()), ("Desk", json!(true)));
        assert_eq!(d.text, DESK);
    }

    #[test]
    fn invalid_draft_returns_report() {
        let bad = DESK.replace("Back)", "Nowhere)");
        let r = draft(&json!({ "text": bad }), &none).unwrap_err();
        let Refusal::DraftInvalid(rep) = &r else { panic!("{r:?}") };
        assert_eq!(rep["ok"], json!(false));
        assert_eq!(rep["findings"][0]["code"], json!("unknown_ref"));
        assert_eq!(rep["findings"][0]["lines"], json!([10]));
        let w = r.wire();
        let back = refusal::from_wire(&w, &|_| None).expect("the wire text reads back");
        assert_eq!(back.code, RefusalCode::DraftInvalid);
        let report: J = serde_json::from_str(back.detail.as_deref().unwrap()).unwrap();
        assert_eq!(report["findings"][0]["lines"], json!([10]), "the report rides as the detail");
        assert_eq!(crate::registry::CmdError::from_text(&w).code, crate::registry::ErrorCode::InvalidArgs);
    }

    #[test]
    fn invalid_draft_report_has_line_and_col() {
        let bad = DESK.replace("category furniture", "category furnitur");
        let r = draft(&json!({ "text": bad }), &none).unwrap_err();
        let Refusal::DraftInvalid(rep) = &r else { panic!("{r:?}") };
        assert_eq!((rep["at"]["line"].clone(), rep["at"]["col"].clone()), (json!(2), json!(10)), "{rep}");
        assert!(rep["at"]["msg"].as_str().unwrap().contains("furnitur"), "{rep}");
        let v = check(&bad).unwrap();
        assert_eq!(v["at"], rep["at"], "family_validate carries the same position");
        assert!(check(DESK).unwrap().get("at").is_none());
    }

    #[test]
    fn oversize_text_rejected() {
        let big = format!("{DESK}{}", "# x\n".repeat(MAX_FAMILY_TEXT / 4 + 1));
        let r = draft(&json!({ "text": big }), &none).unwrap_err();
        assert_eq!(r.kind(), "text_too_large");
        assert_eq!(r.code(), RefusalCode::TextTooLarge);
        assert_eq!(r.typed().arg_of("max"), Some(MAX_FAMILY_TEXT.to_string().as_str()));
        assert_eq!(check(&big).unwrap_err().kind(), "text_too_large");
    }

    #[test]
    fn nest_unknown_family_rejected() {
        let t = format!("{DESK}nest Lamp Light at(0, 0, Height)\n");
        let r = draft(&json!({ "text": t }), &none).unwrap_err();
        assert_eq!(r, Refusal::UnknownFamily("Light".into()));
        assert_eq!(r.code(), RefusalCode::UnknownFamily);
        assert_eq!(r.typed().arg_of("name"), Some("Light"));
    }

    #[test]
    fn project_family_is_embedded_when_nested() {
        let light =
            "family Light\nparam R length type 50mm\nextrude Bulb circle(0, 0, R) z(0, 100mm)\n";
        let project = |n: &str| (n == "Light").then(|| light.to_string());
        let t = format!("{DESK}nest Lamp Light at(0, 0, Height)\n");
        let d = draft(&json!({ "text": t }), &project).unwrap();
        assert!(d.text.contains("\nbegin\nfamily Light"), "{}", d.text);
        assert_eq!(d.report["ok"], json!(true));
    }

    #[test]
    fn name_taken_and_unknown_base() {
        let project = |n: &str| (n == "Desk").then(|| DESK.to_string());
        let r = draft(&json!({ "text": DESK }), &project).unwrap_err();
        assert_eq!(r, Refusal::NameTaken("Desk".into()));
        let ok = draft(&json!({ "text": DESK, "base_family": "Desk" }), &project).unwrap();
        assert_eq!(ok.base_family.as_deref(), Some("Desk"));
        let r = draft(&json!({ "text": DESK, "base_family": "Ghost" }), &project).unwrap_err();
        assert_eq!(r, Refusal::UnknownFamily("Ghost".into()));
    }
}
