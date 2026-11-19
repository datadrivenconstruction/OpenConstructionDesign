//! What leaves the machine: the consent card, the "what was sent" window, the journal, and the
//! offered schedule export, which goes to a file the person picks and to nowhere else.

use super::kit::{self, space, Button};
use super::{logic, Action, Cx, PanelUi};
use crate::query;
use crate::schedule::{self, Cat, Table};
use crate::shell::EditorApp;
use ddc_ui::theme::Role;
use egui::{self, Key, Modifiers, RichText, Ui};
use std::collections::HashSet;

/// Journal rows the window loads; the model keeps its own cap on the table.
pub const AUDIT_ROWS: usize = 2000;

/// Enter takes the primary choice, Esc says no; the text stays in the composer on a no.
pub fn card(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    let Some(host) = st.consent.clone() else {
        return;
    };
    let focused = kit::card(ui, th, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new(cat.t("ai.consent.title"))
                .font(th.font(Role::BodyStrong))
                .color(th.c("text.primary")),
        );
        ui.add(
            egui::Label::new(
                RichText::new(&host)
                    .font(th.font(Role::Numeric))
                    .color(th.c("text.primary")),
            )
            .selectable(true)
            .wrap(),
        );
        ui.add_space(space::XS);
        kit::section_title(ui, th, &cat.t("ai.consent.goes"));
        ui.add(
            egui::Label::new(
                RichText::new(cat.t("ai.consent.goes_list"))
                    .font(th.font(Role::Label))
                    .color(th.c("text.primary")),
            )
            .wrap(),
        );
        kit::section_title(ui, th, &cat.t("ai.consent.stays"));
        ui.add(
            egui::Label::new(
                RichText::new(cat.t("ai.consent.stays_list"))
                    .font(th.font(Role::Label))
                    .color(th.c("text.secondary")),
            )
            .wrap(),
        );
        ui.add_space(space::XS);
        kit::checkbox(
            ui,
            th,
            &mut st.consent_snapshots,
            &cat.t("ai.consent.snapshots"),
        );
        kit::caption(ui, th, &cat.t("ai.consent.snapshots_hint"));
        ui.add_space(space::SM);
        let snapshots = st.consent_snapshots;
        ui.horizontal_wrapped(|ui| {
            if Button::primary(&cat.t("ai.consent.project"))
                .tooltip("", "Enter")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ConsentProject { snapshots });
            }
            if Button::secondary(&cat.t("ai.consent.once"))
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ConsentOnce { snapshots });
            }
            if Button::quiet(&cat.t("ai.consent.no"))
                .tooltip("", "Esc")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::ConsentNo);
            }
        });
        kit::focus_inside(ui, ui.min_rect())
    });
    // Enter and Esc are the card's only while focus is in it: a stray Enter in the composer
    // must never grant consent.
    if cx.palette_open || st.settings_open || !logic::key_scope(focused) {
        return;
    }
    let snapshots = st.consent_snapshots;
    if ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter)) {
        acts.push(Action::ConsentProject { snapshots });
    } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        acts.push(Action::ConsentNo);
    }
}

/// On a character boundary.
pub(super) fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The tool's category names, which are the query's.
pub fn schedule_cat(name: &str) -> Option<Cat> {
    Some(match name {
        "wall" => Cat::Walls,
        "door" => Cat::Doors,
        "window" => Cat::Windows,
        "curtain" => Cat::Curtains,
        "panel" => Cat::Panels,
        "floor" | "slab" => Cat::Slabs,
        "room" => Cat::Rooms,
        "column" => Cat::Columns,
        "beam" => Cat::Beams,
        "roof" => Cat::Roofs,
        "ceiling" => Cat::Ceilings,
        "stair" => Cat::Stairs,
        "railing" => Cat::Railings,
        "mep" => Cat::MepRuns,
        "comp" => Cat::Components,
        _ => return None,
    })
}

/// Column indices by key or title (any language: `tbs` share one column order, the first
/// carries the keys). None named: all. Unknown names are an error that lists the valid keys.
pub fn resolve_cols(tbs: &[&Table], names: &[String]) -> Result<Vec<usize>, String> {
    let tb = tbs[0];
    if names.is_empty() {
        return Ok((0..tb.cols.len()).collect());
    }
    let mut cols = vec![];
    let mut unknown = vec![];
    for n in names {
        match find_col(tbs, n) {
            Some(j) => cols.push(j),
            None => unknown.push(n.as_str()),
        }
    }
    if unknown.is_empty() {
        return Ok(cols);
    }
    Err(format!(
        "unknown column(s): {}; valid keys: {}",
        unknown.join(", "),
        tb.cols.iter().map(|c| c.key.as_str()).collect::<Vec<_>>().join(", ")
    ))
}

pub fn find_col(tbs: &[&Table], name: &str) -> Option<usize> {
    let name = name.trim();
    (0..tbs[0].cols.len()).find(|&j| {
        tbs[0].cols[j].key.eq_ignore_ascii_case(name)
            || tbs.iter().any(|t| t.cols.get(j).is_some_and(|c| c.title == name))
    })
}

/// Group column by key or title; an unknown name is an error that lists the valid keys.
pub fn resolve_group(tbs: &[&Table], group: Option<&str>) -> Result<Option<usize>, String> {
    let Some(g) = group.map(str::trim).filter(|g| !g.is_empty()) else {
        return Ok(None);
    };
    find_col(tbs, g).map(Some).ok_or_else(|| {
        format!(
            "unknown group column: {g}; valid keys: {}",
            tbs[0].cols.iter().map(|c| c.key.as_str()).collect::<Vec<_>>().join(", ")
        )
    })
}

/// Runs `f` on the `first`-language table; only when it fails (a title of the other language)
/// the other variant is built too and `f` runs on both. Saves a second full build per call.
fn with_tables<T>(
    ed: &crate::editor::Editor,
    which: Cat,
    first_ru: bool,
    f: impl Fn(&[&Table]) -> Result<T, String>,
) -> Result<(Table, T), String> {
    let a = schedule::build_in(ed, which, first_ru);
    match f(&[&a]) {
        Ok(v) => Ok((a, v)),
        Err(_) => {
            let b = schedule::build_in(ed, which, !first_ru);
            let v = f(&[&a, &b])?;
            Ok((a, v))
        }
    }
}

/// Tool-side check of an export request, so the model gets the error and can retry.
pub fn check_export(
    ed: &crate::editor::Editor,
    cat: &str,
    columns: &[String],
    group: Option<&str>,
) -> Result<(), String> {
    let which = schedule_cat(cat).ok_or_else(|| format!("no schedule for category: {cat}"))?;
    with_tables(ed, which, false, |tbs| {
        resolve_cols(tbs, columns)?;
        resolve_group(tbs, group).map(|_| ())
    })?;
    Ok(())
}

/// Rows (grouped by the group column, stable inside a group) and columns to save.
pub fn plan_export(
    tbs: &[&Table],
    keep: Option<&HashSet<crate::editor::Sel>>,
    columns: &[String],
    group: Option<&str>,
    units: crate::units::Units,
) -> Result<(Vec<u32>, Vec<usize>), String> {
    let cols = resolve_cols(tbs, columns)?;
    if cols.is_empty() {
        return Err("no columns to export".into());
    }
    let g = resolve_group(tbs, group)?;
    let tb = tbs[0];
    let mut rows: Vec<u32> = schedule::order(tb, &[], None, units)
        .into_iter()
        .filter(|i| match (keep, tb.rows[*i as usize].sel) {
            (Some(k), Some(s)) => k.contains(&s),
            (Some(_), None) => false,
            (None, _) => true,
        })
        .collect();
    if let Some(j) = g {
        schedule::group::sort(tb, &mut rows, &[j]);
    }
    Ok((rows, cols))
}

/// `Ok(false)`: the person closed the file dialog. Columns match by key or title; none named, all.
pub fn export_offer(
    app: &EditorApp,
    cat: &str,
    filter: Option<&str>,
    columns: &[String],
    group: Option<&str>,
) -> Result<bool, String> {
    let which =
        schedule_cat(cat).ok_or_else(|| app.sh.cat.tf("ai.export.unknown", &[("cat", cat)]))?;
    let units = app.ed.tool.units;
    let keep: Option<HashSet<crate::editor::Sel>> = match filter.filter(|f| !f.trim().is_empty()) {
        Some(f) => Some(
            query::eval_tool(&app.ed, f)?
                .into_iter()
                .collect(),
        ),
        None => None,
    };
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let (tb, (rows, cols)) = with_tables(&app.ed, which, ru, |tbs| {
        plan_export(tbs, keep.as_ref(), columns, group, units)
    })?;
    let Some(path) = crate::file_pick::Dlg::new()
        .add_filter("XLSX", &["xlsx"])
        .set_file_name(format!("{cat}.xlsx"))
        .save_file()
    else {
        return Ok(false);
    };
    schedule::export_xlsx(&tb, &rows, &cols, units, &path)?;
    Ok(true)
}

/// The AI's schedule as the view shows it (groups and subtotals included) in an .xlsx the
/// person names. `Ok(false)`: the file dialog was closed.
pub fn export_preset(
    app: &EditorApp,
    p: &crate::schedule::ui::preset::Preset,
    title: &str,
) -> Result<bool, String> {
    let units = app.ed.tool.units;
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let tb = schedule::build_in(&app.ed, p.cat, ru);
    let laid = p.lay(&tb, units);
    let Some(path) = crate::file_pick::Dlg::new()
        .add_filter("XLSX", &["xlsx"])
        .set_file_name(format!("{title}.xlsx"))
        .save_file()
    else {
        return Ok(false);
    };
    schedule::export_xlsx_grouped(&tb, &laid.ord, &laid.cols, units, &path, &laid.lines)?;
    Ok(true)
}

/// The last request's task journal as a JSON file the person names. `Ok(false)`: the dialog
/// was closed. No request text beyond the task head, no tool arguments or results.
pub fn export_journal(app: &EditorApp, j: &ddc_ai::TaskJournal) -> Result<bool, String> {
    let name = format!("{}.json", app.sh.cat.t("ai.journal.file"));
    let Some(path) = crate::file_pick::Dlg::new()
        .add_filter("JSON", &["json"])
        .set_file_name(name)
        .save_file()
    else {
        return Ok(false);
    };
    write_journal(j, &path)?;
    Ok(true)
}

fn write_journal(j: &ddc_ai::TaskJournal, path: &std::path::Path) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&j.to_json()).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_file_is_the_journal_json() {
        let mut j = ddc_ai::TaskJournal::new("four gazebos");
        j.steps.push(ddc_ai::StepRecord {
            n: 1,
            role: ddc_ai::StepRole::Planner,
            model: "m".into(),
            tokens_in: 12_000,
            tokens_cached: 9_000,
            tokens_out: 2_100,
            cost: Some(0.05),
            ms: 900,
            tools: vec!["create_geometry".into()],
        });
        j.end = Some("answered".into());
        let dir = std::env::temp_dir().join(format!("ddc_journal_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.json");
        write_journal(&j, &path).unwrap();
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, j.to_json());
        assert_eq!(back["steps"][0]["tools"][0], "create_geometry");
        assert_eq!(back["totals"]["tokens_cached"], 9000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn second_language_table_is_built_only_on_a_miss() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let calls = std::cell::Cell::new(0);
        let (_, n) = with_tables(&ed, Cat::Walls, false, |tbs| {
            calls.set(calls.get() + 1);
            Ok(tbs.len())
        })
        .unwrap();
        assert_eq!((n, calls.get()), (1, 1));
        calls.set(0);
        let (_, n) = with_tables(&ed, Cat::Walls, true, |tbs| {
            calls.set(calls.get() + 1);
            if tbs.len() == 1 { Err("miss".to_string()) } else { Ok(tbs.len()) }
        })
        .unwrap();
        assert_eq!((n, calls.get()), (2, 2));
    }

    fn sample() -> Table {
        use crate::schedule::{col, Edit, Kind, Row, Val};
        let r = |id: i64, ty: &str, len: f64| Row {
            sel: None,
            id,
            sub: 0,
            vals: vec![Val::N(id as f64), Val::T(ty.into()), Val::N(len)],
        };
        Table {
            cat: Cat::Walls,
            cols: vec![
                col("id", "Id", Kind::Int, Edit::No),
                col("type", "Type", Kind::Text, Edit::No),
                col("len", "Length", Kind::Len, Edit::No),
            ],
            rows: vec![r(1, "b", 1.0), r(2, "a", 2.0), r(3, "b", 3.0), r(4, "a", 4.0)],
            row_phase: vec![None; 4],
            phases: vec![],
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn export_schedule_unknown_column_is_tool_error() {
        let tb = sample();
        let e = resolve_cols(&[&tb], &names(&["len", "lenght", "colour"])).unwrap_err();
        assert!(e.contains("lenght") && e.contains("colour"), "{e}");
        assert!(e.contains("id, type, len"), "{e}");
        let g = resolve_group(&[&tb], Some("tyep")).unwrap_err();
        assert!(g.contains("tyep") && g.contains("id, type, len"), "{g}");
    }

    #[test]
    fn export_offer_groups_rows() {
        use crate::units::Units;
        let tb = sample();
        let (rows, cols) = plan_export(&[&tb], None, &names(&["id"]), Some("type"), Units::Metric)
            .unwrap();
        assert_eq!(cols, vec![0]);
        // group "a" first, source order kept inside each group
        assert_eq!(rows, vec![1, 3, 0, 2]);
    }

    #[test]
    fn export_offer_zero_columns_refused() {
        use crate::units::Units;
        let mut tb = sample();
        tb.cols.clear();
        let e = plan_export(&[&tb], None, &[], None, Units::Metric).unwrap_err();
        assert!(e.contains("no columns"), "{e}");
        let tb = sample();
        assert!(plan_export(&[&tb], None, &names(&["zzz"]), None, Units::Metric).is_err());
    }

    #[test]
    fn valid_request_unchanged() {
        use crate::units::Units;
        let tb = sample();
        let (rows, cols) =
            plan_export(&[&tb], None, &names(&["Length", "id"]), None, Units::Metric).unwrap();
        assert_eq!(cols, vec![2, 0]);
        assert_eq!(rows, vec![0, 1, 2, 3]);
        let (_, all) = plan_export(&[&tb], None, &[], None, Units::Metric).unwrap();
        assert_eq!(all, vec![0, 1, 2]);
    }

    #[test]
    fn clip_stays_on_a_character() {
        assert_eq!(clip("\u{430}\u{431}\u{432}", 3), "\u{430}");
        assert_eq!(clip("\u{430}\u{431}\u{432}", 100), "\u{430}\u{431}\u{432}");
    }

    #[test]
    fn every_query_category_of_a_schedule_maps() {
        for c in [
            "wall", "door", "window", "curtain", "floor", "room", "column", "beam", "roof",
            "ceiling", "stair", "railing", "mep", "comp",
        ] {
            assert!(schedule_cat(c).is_some(), "{c}");
        }
        assert!(schedule_cat("nonsense").is_none());
    }
}
