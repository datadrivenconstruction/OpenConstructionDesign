// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project information from IFCPROJECT (Name, Phase, the `DDC_ProjectInfo` property set) and
//! the postal address of IFCSITE / IFCBUILDING; applied to the model's project info.

use ddc_model::ProjectInfo;

use crate::ifc::reader::{StepFile, Value};

pub(super) const PSET: &str = "DDC_ProjectInfo";
/// Name written when the model has none; not taken back as a real name.
pub(super) const DEFAULT_NAME: &str = "Project";

fn text(v: &Value) -> Option<String> {
    v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn arg(f: &StepFile, id: u32, i: usize) -> Value {
    f.get(id)
        .and_then(|e| e.args().get(i).cloned())
        .unwrap_or(Value::Null)
}

fn refs(v: &Value) -> Vec<u32> {
    match v {
        Value::Ref(r) => vec![*r],
        Value::List(l) => l.iter().filter_map(Value::as_ref).collect(),
        _ => vec![],
    }
}

/// Address lines of an IFCPOSTALADDRESS (lines, then postal code + town, region, country).
fn address(f: &StepFile, id: u32) -> Option<String> {
    if f.type_of(id) != Some("IFCPOSTALADDRESS") {
        return None;
    }
    let mut lines: Vec<String> = match arg(f, id, 4) {
        Value::List(l) => l.iter().filter_map(text).collect(),
        _ => vec![],
    };
    let town = [8, 6]
        .iter()
        .filter_map(|&i| text(&arg(f, id, i)))
        .collect::<Vec<_>>()
        .join(" ");
    if !town.is_empty() {
        lines.push(town);
    }
    for i in [7, 9] {
        lines.extend(text(&arg(f, id, i)));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// `None`: the file has no project information worth merging.
pub(super) fn read(f: &StepFile) -> Option<ProjectInfo> {
    let mut p = ProjectInfo::default();
    let mut project = None;
    let (mut site, mut building) = (None, None);
    for (id, ty) in f.iter() {
        match ty {
            "IFCPROJECT" if project.is_none() => project = Some(id),
            "IFCSITE" if site.is_none() => site = Some(id),
            "IFCBUILDING" if building.is_none() => building = Some(id),
            _ => {}
        }
    }
    let mut from_pset = false;
    if let Some(pr) = project {
        for (rid, ty) in f.iter() {
            if ty != "IFCRELDEFINESBYPROPERTIES" || !refs(&arg(f, rid, 4)).contains(&pr) {
                continue;
            }
            for ps in refs(&arg(f, rid, 5)) {
                if f.type_of(ps) != Some("IFCPROPERTYSET")
                    || arg(f, ps, 2).as_str() != Some(PSET)
                {
                    continue;
                }
                from_pset = true;
                for pid in refs(&arg(f, ps, 4)) {
                    let (Some(k), Some(v)) = (arg(f, pid, 0).as_str().map(str::to_string), text(&arg(f, pid, 2)))
                    else {
                        continue;
                    };
                    if let Some(slot) = p.slot(&k) {
                        *slot = v;
                    }
                }
            }
        }
        if p.name.is_empty() {
            let name = text(&arg(f, pr, 2)).filter(|n| from_pset || n != DEFAULT_NAME);
            p.name = name.or_else(|| text(&arg(f, pr, 5))).unwrap_or_default();
        }
        if p.stage.is_empty() {
            p.stage = text(&arg(f, pr, 6)).unwrap_or_default();
        }
    }
    if let Some(a) = site
        .and_then(|s| address(f, arg(f, s, 13).as_ref()?))
        .or_else(|| building.and_then(|b| address(f, arg(f, b, 11).as_ref()?)))
    {
        p.address = a;
    }
    (p != ProjectInfo::default()).then_some(p)
}

/// Fields the model accepts: no control characters, bounded length.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(500)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Fields of `file` for the empty fields of `cur`; non-empty `cur` values stay.
pub(super) fn merge(cur: &ProjectInfo, file: &ProjectInfo) -> ProjectInfo {
    let mut out = cur.clone();
    for (k, v) in file.fields() {
        let v = clean(v);
        if let (false, Some(slot)) = (v.is_empty(), out.slot(k)) {
            if slot.trim().is_empty() {
                *slot = v;
            }
        }
    }
    out
}
