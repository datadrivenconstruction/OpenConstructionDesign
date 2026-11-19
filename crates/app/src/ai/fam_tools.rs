//! Chat tools over family definition texts: `family_read`, `family_validate` (reads) and
//! `family_draft` (writes only a draft for the family editor, never the model or its journal).
//! The contract and its refusals live in `ddc_ai::family_draft`; this file lends it the project.

use super::fmt::Out;
use super::session::{Core, Done, FromWorker, Handler};
use ddc_ai::family_draft as fd;
use ddc_ai::CallCx;
use ddc_model::Model;
use serde_json::{json, Value as J};

pub const HANDLERS: [(&str, Handler); 4] = [
    ("family_reference", family_reference),
    ("family_read", family_read),
    ("family_validate", family_validate),
    ("family_draft", family_draft),
];

/// The definition text of a project family by name.
pub fn project_def(m: &Model, name: &str) -> Option<String> {
    let (id, _) = m.families().ok()?.into_iter().find(|f| f.1 == name)?;
    m.family_def(id).ok()
}

fn family_read(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let name = a["name"].as_str().ok_or("name must be text")?;
    let text = project_def(&c.ed.model, name)
        .ok_or_else(|| fd::Refusal::UnknownFamily(name.to_string()).wire())?;
    let mut o = Out::new(c.max_chars);
    o.put("name", &json!(name)).put("text", &json!(text));
    Ok(o.finish().into())
}

fn family_reference(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let mut o = Out::new(c.max_chars.max(ddc_ai::family_repair::DSL_REFERENCE.len() + 200));
    o.put("reference", &json!(ddc_ai::family_repair::DSL_REFERENCE));
    Ok(o.finish().into())
}

fn family_validate(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let text = a["text"].as_str().ok_or("text must be text")?;
    let report = fd::check(text).map_err(|r| r.wire())?;
    let mut o = Out::new(c.max_chars);
    o.put("report", &report);
    Ok(o.finish().into())
}

/// The draft goes to the family editor by name; the project changes only on the person's load.
pub fn family_draft(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    // refusals of a text come back as the repair loop's report: excerpts, bounded rounds
    let mut book = std::mem::take(&mut c.fam_repair);
    let repair = book.state(cx.actor, std::time::Instant::now());
    let before = (repair.rounds, repair.tokens);
    let d = repair.draft(a, &|n| project_def(&c.ed.model, n));
    let (rounds, tokens) = if d.is_ok() { before } else { (repair.rounds, repair.tokens) };
    c.fam_repair = book;
    let d = d?;
    c.send(FromWorker::FamilyDraft {
        name: d.name.clone(),
        text: d.text.clone(),
        base_family: d.base_family.clone(),
        repair: (rounds, tokens),
        queued: c.fam_from_agent,
    });
    let mut o = Out::new(c.max_chars);
    o.put("draft_id", &json!(d.name))
        .put("status", &json!(if c.fam_from_agent { "pending_user" } else { "draft" }))
        .put("in_project", &json!(false))
        .put("report", &d.report)
        .put("repair", &json!({ "rounds_used": rounds, "tokens": tokens }));
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESK: &str = "family Desk
category furniture
param Width length type 1200mm
extrude Board rect(0, 0, Width, 600mm) z(0, 40mm)
";

    #[test]
    fn project_def_reads_a_loaded_family() {
        let mut m = Model::open(":memory:").unwrap();
        m.load_family(DESK).unwrap();
        assert!(project_def(&m, "Desk").unwrap().contains("family Desk"));
        assert!(project_def(&m, "Ghost").is_none());
        let r = fd::draft(&json!({ "text": DESK }), &|n| project_def(&m, n)).unwrap_err();
        assert_eq!(r.kind(), "name_taken");
        assert_eq!(r.code(), ddc_model::refusal::RefusalCode::NameTaken);
        let ok = fd::draft(&json!({ "text": DESK, "base_family": "Desk" }), &|n| project_def(&m, n));
        assert!(ok.is_ok());
    }
}
