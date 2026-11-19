// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `deliver_project`: a building from a brief through to files in one call. It is
//! `generate_project` and `create_drawing_set` on one card, plus the files written after the
//! person's Apply; nothing here builds or exports on its own.
use super::fmt::Out;
use super::session::{Core, Done};
use crate::deliver::Outputs;
use ddc_ai::CallCx;
use serde_json::{json, Value as J};

/// A reply of another tool as a value; a reply cut to its budget stays as text.
fn value_of(d: &Done) -> J {
    serde_json::from_str(&d.reply.text).unwrap_or_else(|_| J::String(d.reply.text.clone()))
}

/// Arguments of `create_drawing_set` from the `drawings` field.
fn dset_args(a: &J) -> Option<J> {
    let mut args = match &a["drawings"] {
        J::Bool(false) => return None,
        J::Object(o) => J::Object(o.clone()),
        _ => json!({}),
    };
    if args["title"].as_str().is_none_or(|t| t.trim().is_empty()) {
        args["title"] = json!(crate::tr("dset.set_name", &[]));
    }
    args["why"] = a["why"].clone();
    Some(args)
}

pub fn deliver_project(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let outputs = Outputs::from_json(a)?;
    let drawings = dset_args(a);
    if drawings.is_none() && outputs.pdf {
        return Err("pdf prints the drawing set: leave drawings on or drop it".into());
    }
    let project = super::project_tools::generate_project(c, a, cx)?;
    let pv = value_of(&project);
    let set = match drawings {
        Some(d) => match super::dset_tool::create_drawing_set(c, &d, cx) {
            Ok(r) => Some(value_of(&r)),
            Err(e) => {
                // one call, one outcome: the project item goes with the failed set
                if let Some(id) = pv["item"].as_u64().and_then(|i| u32::try_from(i).ok()) {
                    c.plan.items.retain(|i| i.id != id);
                    c.replan().map_err(|e| e.to_string())?;
                }
                return Err(format!("drawing set: {e}"));
            }
        },
        None => None,
    };
    c.plan.deliver = outputs.any().then_some(outputs);
    let mut o = Out::new(c.max_chars);
    o.put_own("outputs", &json!(outputs.names()));
    if outputs.ifc {
        o.put_own("ifc_schema", &json!(outputs.schema_name()));
    }
    o.put_own(
        "after_apply",
        &json!("the person reviews one card; on Apply the files are written in the background \
                to a folder named after the project, with Open and Show in folder at the end"),
    );
    o.put_own("project", &pv);
    if let Some(s) = set {
        o.put_own("drawings", &s);
    }
    Ok(o.finish().into())
}
