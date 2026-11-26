// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `create_drawing_set`: one plan item that tags the model and lays out the whole drawing set.
//! It compiles on the scratch copy at once, so the reply lists the sheets as they will be.
use super::fmt::{self, Out};
use super::plan::PlanOp;
use super::session::{Core, Done};
use super::tools_write::why_of;
use ddc_ai::CallCx;
use ddc_io::sheets::DsetOptions;
use serde_json::{json, Value as J};

/// Sheets listed in a reply; the count says how many there are in all.
const LISTED: usize = 40;

/// The options and the language of the sheet words: `language`, else the interface language `ui`.
pub fn options_of(a: &J, ui: &str) -> Result<(DsetOptions, ddc_ui::Lang), String> {
    let on = |k: &str| a[k].as_bool().unwrap_or(true);
    let scale = match &a["scale"] {
        J::Null => None,
        v => Some(
            v.as_str()
                // num-lint: a model's JSON argument, not typed by a person
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
                .filter(|s| *s >= 1.0 && *s <= 1000.0)
                .ok_or("scale must be a denominator such as 100")?,
        ),
    };
    let standard = a["standard"].as_str().unwrap_or("gost").to_string();
    if !matches!(standard.as_str(), "gost" | "iso") {
        return Err("standard must be gost or iso".into());
    }
    let lang = match a["language"].as_str() {
        None => ddc_ui::Lang::from_code(ui),
        Some(l) if ddc_gen::project::names::locales().iter().any(|x| x.eq_ignore_ascii_case(l)) => ddc_ui::Lang::from_code(l),
        Some(_) => return Err("language must be a locale code such as en, ru, de, ja, pt-BR".into()),
    };
    Ok((
        DsetOptions {
            plans: on("plans"),
            sections: on("sections"),
            elevations: on("elevations"),
            schedules: on("schedules"),
            scale,
            paper: a["paper"].as_str().map(str::to_string),
            standard,
        },
        lang,
    ))
}

pub fn create_drawing_set(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let (opt, lang) = options_of(a, cx.lang)?;
    let title = fmt::text(a["title"].as_str().unwrap_or_default());
    // doors and windows the set marks by type, counted before its item changes the model
    let marks = crate::sheet_view::marks_to_assign(&c.ed)?;
    let added = c.add_item(title, why_of(a), PlanOp::DrawingSet(opt, lang), cx.actor)?;
    let m = &c.ed.model;
    let mut sheets = vec![];
    for (id, _) in m.drawing_set_sheets().map_err(|e| e.to_string())? {
        let s = m.sheet(id).map_err(|e| e.to_string())?;
        sheets.push((s.number, s.name, s.paper));
    }
    sheets.sort_by(|a, b| natural(&a.0).cmp(&natural(&b.0)));
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(added.0));
    let (n, ch, d) = added.1;
    o.put(
        "changes",
        &json!({"created": n, "changed": ch, "deleted": d}),
    );
    o.put("sheets", &json!(sheets.len()));
    o.put("marks_assigned", &json!(marks));
    if let Some(p) = sheets.first() {
        o.put("paper", &json!(p.2));
    }
    let list: Vec<J> = sheets
        .iter()
        .take(LISTED)
        .map(|(n, t, _)| json!(format!("{} {}", fmt::text(n), fmt::text(t))))
        .collect();
    o.put("list", &J::Array(list));
    Ok(o.finish().into())
}

/// «A-10» after «A-9».
fn natural(s: &str) -> (String, u64) {
    let d = s.len() - s.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (head, tail) = s.split_at(s.len() - d);
    (head.to_string(), tail.parse().unwrap_or(0))
}
