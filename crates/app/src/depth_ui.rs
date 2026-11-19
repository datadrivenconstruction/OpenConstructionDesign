//! Design depth in the editor: the project setting editor shared by the chat chip
//! and the Settings window, the chip text, the pre-run estimate line and the depth heat map
//! (a view tint by element depth, cleared by Reset view like any other coloring).
//! Every write is one undo step through `Editor::run_op`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ddc_model::depth::{self, DepthSettings, DiscSetting, Discipline, Estimate};
use ddc_model::Model;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText, Ui};

use crate::ai::sets::Key;
use crate::ai::view_tools::{Bucket, Legend, LegendRow, EMPTY_RGB};
use crate::editor::{Editor, OpFn};

/// `Legend::by` of the heat map: tells it from a coloring the assistant made.
pub const HEAT_BY: &str = "ddc.depth";

/// D1 cool .. D5 hot; readable on light and dark canvases.
pub const HEAT: [u32; 5] = [0x4a90b8, 0x2e9e5b, 0xc9a227, 0xe07a1f, 0xd64545]; // design-lint: data

/// Catalog key of a discipline's short name (Arch / Struct / MEP / Doc).
pub fn disc_key(d: Discipline) -> &'static str {
    match d {
        Discipline::Arch => "depth.disc.arch",
        Discipline::Struct => "depth.disc.struct",
        Discipline::Mep => "depth.disc.mep",
        Discipline::Doc => "depth.disc.doc",
    }
}

pub fn level_label(d: u8) -> String {
    format!("D{}", depth::clamp(d))
}

/// The chip text: "Depth D3 · AR ST MEP Doc"; a discipline with its own depth carries it
/// ("MEP D2"), one switched off is left out; all off says so.
pub fn chip_text(cat: &Catalog, s: &DepthSettings) -> String {
    let parts: Vec<String> = Discipline::ALL
        .into_iter()
        .filter_map(|d| {
            let at = s.of(d)?;
            let name = cat.t(disc_key(d));
            Some(if at == s.project { name } else { format!("{name} {}", level_label(at)) })
        })
        .collect();
    let head = cat.tf("depth.chip", &[("level", level_label(s.project).as_str())]);
    if parts.is_empty() {
        return format!("{head} · {}", cat.t("depth.all_off"));
    }
    format!("{head} · {}", parts.join(" "))
}

/// "≈ 1 240 elements · 3 min · 120k tokens · 9 sheets"
pub fn estimate_text(cat: &Catalog, e: &Estimate) -> String {
    let minutes = (e.seconds / 60.0).ceil().max(1.0) as u64;
    let tokens = if e.tokens >= 10_000 {
        format!("{}k", (e.tokens + 500) / 1000)
    } else {
        e.tokens.to_string()
    };
    let (elements, minutes) = (e.elements.to_string(), minutes.to_string());
    let (sheets, calls) = (e.sheets.to_string(), e.calls.to_string());
    cat.tf(
        "depth.estimate",
        &[
            ("elements", elements.as_str()),
            ("minutes", minutes.as_str()),
            ("tokens", tokens.as_str()),
            ("sheets", sheets.as_str()),
            ("calls", calls.as_str()),
        ],
    )
}

/// The estimate of a run to `s` from the model's own rows; `None` when the model is empty
/// (the brief's estimate comes with the drafted project instead).
pub fn model_estimate(m: &Model, s: &DepthSettings) -> Option<Estimate> {
    let counts = m.depth_base_counts().ok()?;
    let any = counts.iter().any(|c| c.0 != "levels" && c.1 > 0);
    any.then(|| depth::estimate(&counts, s))
}

/// The setting as the assistant reads it: `{"project": 3, "arch": 3, "mep": null, ...}`
/// (null: switched off).
pub fn settings_json(s: &DepthSettings) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    o.insert("project".into(), serde_json::json!(s.project));
    for d in Discipline::ALL {
        o.insert(d.as_str().into(), serde_json::json!(s.of(d)));
    }
    serde_json::Value::Object(o)
}

/// The estimate as the assistant reads it; seconds and minutes rounded.
pub fn estimate_json(e: &Estimate) -> serde_json::Value {
    let by: serde_json::Map<String, serde_json::Value> = Discipline::ALL
        .into_iter()
        .map(|d| (d.as_str().to_string(), serde_json::json!(e.by_disc[d.index()])))
        .collect();
    serde_json::json!({
        "elements": e.elements,
        "by_discipline": by,
        "added_beyond_draft": e.added,
        "stages": e.stages,
        "model_calls": e.calls,
        "minutes": (e.seconds / 6.0).round() / 10.0,
        "tokens": e.tokens,
        "sheets": e.sheets,
    })
}

/// Stores `s` as one undo step; false when nothing changed.
pub fn apply(ed: &mut Editor, s: DepthSettings) -> Result<bool, ddc_model::Error> {
    if ed.model.depth_settings()? == s.clamped() {
        return Ok(false);
    }
    let f = OpFn(Arc::new(move |m: &mut Model| m.set_depth_settings(&s).map(|_| ())));
    ed.run_op(f).map(|()| true)
}

/// The setting as controls: the project depth D1..D5, then one row per discipline with its
/// switch and its own depth ("as project" or D1..D5). True when anything moved.
pub fn editor(ui: &mut Ui, th: &Theme, cat: &Catalog, s: &mut DepthSettings) -> bool {
    let mut moved = false;
    let levels: Vec<String> = (depth::MIN..=depth::MAX).map(level_label).collect();
    let refs: Vec<&str> = levels.iter().map(String::as_str).collect();
    // label and choice on their own lines: the narrowest AI panel has no room for both
    ui.label(
        RichText::new(cat.t("depth.project"))
            .font(th.font(Role::Body))
            .color(th.c("text.primary")),
    );
    let sel = usize::from(depth::clamp(s.project) - depth::MIN);
    if let Some(i) = widgets::segmented(ui, th, &refs, sel) {
        s.project = depth::MIN + i as u8;
        moved = true;
    }
    ui.add(
        egui::Label::new(
            RichText::new(cat.t(&format!("depth.level.{}", s.project)))
                .font(th.font(Role::Caption))
                .color(th.c("text.secondary")),
        )
        .wrap(),
    );
    ui.add_space(space::SM);
    let mut own: Vec<String> = vec![cat.t("depth.as_project")];
    own.extend(levels.iter().cloned());
    let own_refs: Vec<&str> = own.iter().map(String::as_str).collect();
    for d in Discipline::ALL {
        let mut cur: DiscSetting = s.disc[d.index()];
        ui.push_id(d.as_str(), |ui| {
            if widgets::toggle(ui, th, &mut cur.on, &cat.t(disc_key(d))).changed() {
                moved = true;
            }
            ui.add_enabled_ui(cur.on, |ui| {
                let sel = cur.own.map_or(0, |x| usize::from(depth::clamp(x)));
                if let Some(i) = widgets::segmented(ui, th, &own_refs, sel) {
                    cur.own = (i > 0).then_some(i as u8);
                    moved = true;
                }
            });
        });
        ui.add_space(space::XS);
        s.disc[d.index()] = cur;
    }
    moved
}

/// Heat map rows from element depths: one row per depth present, D1 first; elements without
/// a depth form the quiet "not set" row.
pub fn heat_legend(
    keys: &[Key],
    depths: &HashMap<(String, i64), u8>,
    label: &str,
) -> Legend {
    let mut by: [Vec<Key>; 5] = Default::default();
    let mut none: Vec<Key> = vec![];
    let mut seen = HashSet::new();
    for k in keys {
        if !seen.insert((k.table(), k.id)) {
            continue;
        }
        match depths.get(&(k.table().to_string(), k.id)) {
            Some(d) => by[usize::from(depth::clamp(*d) - 1)].push(*k),
            None => none.push(*k),
        }
    }
    let mut rows: Vec<LegendRow> = vec![];
    for (i, mut keys) in by.into_iter().enumerate() {
        if keys.is_empty() {
            continue;
        }
        keys.sort();
        rows.push(LegendRow {
            value: format!("{label} D{}", i + 1),
            bucket: Bucket::Value,
            rgb: HEAT[i],
            keys: Arc::new(keys),
        });
    }
    if !none.is_empty() {
        none.sort();
        rows.push(LegendRow {
            value: String::new(),
            bucket: Bucket::Empty,
            rgb: EMPTY_RGB,
            keys: Arc::new(none),
        });
    }
    Legend {
        by: HEAT_BY.to_string(),
        rows,
        folded: 0,
    }
}

/// The heat map is the view's coloring now.
pub fn heat_on() -> bool {
    crate::ai::view_state::legend().is_some_and(|l| l.by == HEAT_BY)
}

/// Turns the heat map on (every element of the model by its depth) or off (Reset view).
pub fn set_heat(app: &mut crate::shell::EditorApp, on: bool) {
    use crate::ai::view_state::apply;
    use crate::ai::view_tools::ViewCmd;
    if !on {
        if heat_on() {
            apply(app, &ViewCmd::Reset);
        }
        return;
    }
    let depths: HashMap<(String, i64), u8> = app
        .ed
        .model
        .elem_depths()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, e)| (k, e.depth))
        .collect();
    let keys: Vec<Key> = crate::ai::rows::rows(&app.ed)
        .iter()
        .map(crate::ai::rows::key_of)
        .filter(|k| Discipline::of_table(k.table()).is_some())
        .collect();
    let label = app.sh.cat.t("depth.heat.row");
    let lg = heat_legend(&keys, &depths, &label);
    apply(app, &ViewCmd::Color(Arc::new(lg)));
}

#[cfg(test)]
#[path = "depth_ui_tests.rs"]
mod tests;
