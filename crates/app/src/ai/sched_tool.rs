//! `create_schedule`: the assistant lays a schedule out (columns, groups, sort, filters) and
//! the person opens it as a real schedule view, or saves it to XLSX, from the chat card. The
//! model is not touched: only the view's own state changes, and only when the person asks.

use super::fmt::{self, Out};
use super::session::{Core, Done, FromWorker};
use crate::schedule::ui::preset::Preset;
use crate::schedule::{self, Table};
use ddc_ai::CallCx;
use serde_json::{json, Value as J};

const MAX_COLS: usize = 24;
const MAX_GROUPS: usize = 3;
/// Group headings named in the reply: the rest are counted.
const MAX_HEADS: usize = 40;

/// What the card keeps of a made schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub preset: Preset,
    /// Category word the person sees on the card.
    pub cat: String,
    pub title: String,
    /// Rows left after the filters.
    pub rows: usize,
}

fn unknown(what: &str, name: &str, tb: &Table) -> String {
    format!(
        "unknown {what}: {}; valid keys: {}",
        fmt::text(name),
        tb.cols.iter().map(|c| c.key.as_str()).collect::<Vec<_>>().join(", ")
    )
}

/// Resolves `a` against the tables of both languages: column names are keys or titles.
pub fn preset_of(en: &Table, ru: &Table, a: &J) -> Result<Preset, String> {
    let tbs = [en, ru];
    let key = |what: &str, name: &str| -> Result<String, String> {
        super::ui::consent::find_col(&tbs, name)
            .map(|j| en.cols[j].key.clone())
            .ok_or_else(|| unknown(what, name, en))
    };
    let names = |k: &str| -> Vec<String> {
        a[k].as_array()
            .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let mut columns = vec![];
    for n in names("columns").iter().take(MAX_COLS) {
        columns.push(key("column", n)?);
    }
    let mut group = vec![];
    for n in names("group_by").iter().take(MAX_GROUPS) {
        let k = key("group column", n)?;
        if !group.contains(&k) {
            group.push(k);
        }
    }
    // a group column that is not shown would leave headings without their values
    for g in &group {
        if !columns.is_empty() && !columns.contains(g) {
            columns.push(g.clone());
        }
    }
    let sort = match &a["sort"] {
        J::Null => None,
        s => {
            let col = s["col"].as_str().ok_or("sort.col must be text")?;
            Some((key("sort column", col)?, s["desc"].as_bool() != Some(true)))
        }
    };
    let mut filters = vec![];
    match &a["filters"] {
        J::Null => {}
        J::Object(o) => {
            for (col, text) in o {
                let t = text.as_str().ok_or("a filter is text")?;
                filters.push((key("filter column", col)?, fmt::text(t)));
            }
        }
        _ => return Err("filters must be an object of column to filter text".to_string()),
    }
    let subtotals = a["subtotals"].as_bool().unwrap_or(!group.is_empty());
    let word = a["cat"].as_str().unwrap_or_default();
    let cat = super::ui::consent::schedule_cat(word)
        .ok_or_else(|| format!("no schedule for category: {}", fmt::text(word)))?;
    Ok(Preset { cat, columns, group, sort, filters, subtotals })
}

pub fn create_schedule(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let word = a["cat"].as_str().ok_or("cat must be text")?;
    let cat = super::ui::consent::schedule_cat(word)
        .ok_or_else(|| format!("no schedule for category: {}", fmt::text(word)))?;
    let en = schedule::build_in(&c.ed, cat, false);
    let ru = schedule::build_in(&c.ed, cat, true);
    let preset = preset_of(&en, &ru, a)?;
    let units = c.ed.tool.units;
    let laid = preset.lay(&en, units);
    let rows = laid.ord.len();
    let totals = schedule::totals(&en, &laid.ord);
    let sums: Vec<J> = laid
        .cols
        .iter()
        .filter_map(|&j| {
            let col = &en.cols[j];
            let x = totals.get(j).copied().flatten()?;
            Some(json!({"col": col.key, "header": schedule::header(col, units),
                "total": fmt::num(schedule::export_num(x, col.kind, units))}))
        })
        .collect();
    let heads: Vec<J> = laid
        .lines
        .iter()
        .filter_map(|l| match l {
            schedule::group::Line::Head { depth, text, n } => {
                Some(json!({"depth": depth, "group": fmt::text(text), "rows": n}))
            }
            _ => None,
        })
        .collect();
    let made = Made {
        preset: preset.clone(),
        cat: word.to_string(),
        title: fmt::text(a["title"].as_str().unwrap_or_default()),
        rows,
    };
    c.send(FromWorker::ScheduleMade(Box::new(made)));
    let mut o = Out::new(c.max_chars);
    o.put("status", &json!("ready"));
    o.put("rows", &json!(rows));
    o.put("of", &json!(en.rows.len()));
    o.put("group_by", &json!(preset.group));
    o.put("groups", &json!(heads.len()));
    o.list_capped("group_list", &heads, MAX_HEADS);
    o.list("totals", &sums);
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> (Table, Table) {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut ed);
        (
            schedule::build_in(&ed, schedule::Cat::Walls, false),
            schedule::build_in(&ed, schedule::Cat::Walls, true),
        )
    }

    #[test]
    fn names_resolve_to_keys_in_either_language() {
        let (en, ru) = tables();
        let title_ru = ru.cols.iter().find(|c| c.key == "type").unwrap().title.clone();
        let a = json!({
            "cat": "wall",
            "columns": ["id", title_ru, "Vol"],
            "group_by": ["type", "type"],
            "sort": {"col": "len", "desc": true},
            "filters": {"h": ">=2"}
        });
        let p = preset_of(&en, &ru, &a).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.columns, vec!["id", "type", "vol"]);
        assert_eq!(p.group, vec!["type"], "a repeated level is dropped");
        assert_eq!(p.sort, Some(("len".to_string(), false)));
        assert_eq!(p.filters, vec![("h".to_string(), ">=2".to_string())]);
        assert!(p.subtotals, "groups come with subtotals unless asked otherwise");
    }

    #[test]
    fn the_group_column_is_shown_and_unknown_names_list_the_keys() {
        let (en, ru) = tables();
        let a = json!({"cat": "wall", "columns": ["id"], "group_by": ["type"]});
        let p = preset_of(&en, &ru, &a).unwrap();
        assert_eq!(p.columns, vec!["id", "type"]);
        let bad = json!({"cat": "wall", "group_by": ["nope"]});
        let e = preset_of(&en, &ru, &bad).unwrap_err();
        assert!(e.contains("nope") && e.contains("valid keys") && e.contains("type"), "{e}");
        let bad = json!({"cat": "wall", "filters": {"zzz": "1"}});
        assert!(preset_of(&en, &ru, &bad).is_err());
        let bad = json!({"cat": "wall", "filters": ["x"]});
        assert!(preset_of(&en, &ru, &bad).is_err());
        let bad = json!({"cat": "nothing"});
        assert!(preset_of(&en, &ru, &bad).is_err());
    }

    #[test]
    fn at_most_three_group_levels_and_no_more_than_the_column_cap() {
        let (en, ru) = tables();
        let keys: Vec<&str> = en.cols.iter().map(|c| c.key.as_str()).collect();
        let a = json!({"cat": "wall", "group_by": keys});
        let p = preset_of(&en, &ru, &a).unwrap();
        assert_eq!(p.group.len(), MAX_GROUPS);
    }
}
