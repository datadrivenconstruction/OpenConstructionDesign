//! Option comparison without a frame: hard filters, weighted score, Pareto set, synchronised
//! tiles. The numbers shown come from here, never from the model text.

use ddc_model::MetricsRow;
use std::collections::BTreeMap;

pub const TILES_MAX: usize = 4;
/// Message keys of the two empty faces.
pub const KEY_EMPTY: &str = "ai.cmp.empty";
pub const KEY_NO_MATCH: &str = "ai.cmp.no_match";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Ge,
    Le,
    Eq,
}

/// One option as the table sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: i64,
    pub name: String,
    /// Error findings; `None` when checks were not run for this option.
    pub blocks: Option<usize>,
    pub cells: BTreeMap<String, f64>,
}

pub fn row_from(m: &MetricsRow) -> Row {
    Row {
        id: m.option,
        name: m.name.clone(),
        blocks: m.metrics.findings.map(|f| f.errors),
        cells: m
            .metrics
            .cells()
            .into_iter()
            .map(|c| (c.key, c.value))
            .collect(),
    }
}

/// A stored recipe as a row: numbers come from its metrics snapshot (`OptionMetrics::to_json`);
/// a recipe without metrics, or with unreadable ones, is a row without cells.
pub fn row_from_rec(rec: &ddc_model::OptionRec) -> Row {
    let v: serde_json::Value = rec
        .metrics
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or(serde_json::Value::Null);
    let cells = v["cells"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| Some((c["key"].as_str()?.to_string(), c["value"].as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    Row {
        id: rec.id,
        name: rec.name.clone(),
        blocks: v["findings"]["error"].as_u64().map(|n| n as usize),
        cells,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub key: String,
    pub cmp: Cmp,
    pub value: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    /// Hide options whose checks found errors.
    pub hide_blocked: bool,
    /// An option without the metric fails the rule: nothing is assumed about missing numbers.
    pub rules: Vec<Rule>,
}

fn passes(r: &Row, f: &Filters) -> bool {
    if f.hide_blocked && r.blocks.is_some_and(|n| n > 0) {
        return false;
    }
    f.rules.iter().all(|u| match r.cells.get(&u.key) {
        None => false,
        Some(v) => match u.cmp {
            Cmp::Ge => *v >= u.value,
            Cmp::Le => *v <= u.value,
            Cmp::Eq => (*v - u.value).abs() <= f64::EPSILON * u.value.abs().max(1.0),
        },
    })
}

/// Rows that survive the hard filters, in input order.
pub fn visible<'a>(rows: &'a [Row], f: &Filters) -> Vec<&'a Row> {
    rows.iter().filter(|r| passes(r, f)).collect()
}

/// `None` when there is a table to draw, else the message key of the empty face.
pub fn empty_key(total: usize, shown: usize) -> Option<&'static str> {
    if total == 0 {
        Some(KEY_EMPTY)
    } else if shown == 0 {
        Some(KEY_NO_MATCH)
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weight {
    pub key: String,
    pub weight: f64,
    pub dir: Dir,
}

/// Weighted score in [0, 1]: each metric is scaled over the given rows so that the best value
/// is 1; a missing value, or a metric equal in all rows, adds 0.
pub fn scores(rows: &[&Row], w: &[Weight]) -> BTreeMap<i64, f64> {
    let total: f64 = w.iter().map(|x| x.weight.abs()).sum();
    let mut out: BTreeMap<i64, f64> = rows.iter().map(|r| (r.id, 0.0)).collect();
    if total <= 0.0 {
        return out;
    }
    for x in w {
        let vals: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.cells.get(&x.key).copied())
            .collect();
        let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
            continue;
        }
        for r in rows {
            if let Some(v) = r.cells.get(&x.key) {
                let n = match x.dir {
                    Dir::Max => (v - lo) / (hi - lo),
                    Dir::Min => (hi - v) / (hi - lo),
                };
                *out.entry(r.id).or_default() += x.weight.abs() * n / total;
            }
        }
    }
    out
}

/// Option ids by descending score; equal scores keep ascending id, so the order never flickers.
pub fn ranking(rows: &[&Row], w: &[Weight]) -> Vec<(i64, f64)> {
    let mut v: Vec<(i64, f64)> = scores(rows, w).into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Options no other option beats: at least as good on every chosen metric and better on one.
/// An option missing a chosen metric cannot be compared and stays out. Ids ascending.
pub fn pareto(rows: &[&Row], metrics: &[(String, Dir)]) -> Vec<i64> {
    if metrics.is_empty() {
        return Vec::new();
    }
    let point = |r: &Row| -> Option<Vec<f64>> {
        metrics
            .iter()
            .map(|(k, d)| {
                r.cells.get(k).map(|v| match d {
                    Dir::Max => *v,
                    Dir::Min => -*v,
                })
            })
            .collect()
    };
    let pts: Vec<(i64, Vec<f64>)> = rows
        .iter()
        .filter_map(|r| point(r).map(|v| (r.id, v)))
        .collect();
    let dominates = |a: &[f64], b: &[f64]| {
        a.iter().zip(b).all(|(x, y)| x >= y) && a.iter().zip(b).any(|(x, y)| x > y)
    };
    let mut out: Vec<i64> = pts
        .iter()
        .filter(|(id, p)| !pts.iter().any(|(o, q)| o != id && dominates(q, p)))
        .map(|(id, _)| *id)
        .collect();
    out.sort_unstable();
    out
}

/// How a metric's number is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Whole numbers (finding counts).
    Count,
    Area,
    Volume,
    /// Cost, score and anything else: two decimals.
    Plain,
}

pub fn shape_of(key: &str) -> Shape {
    if key.starts_with("findings") || key.ends_with("_count") {
        Shape::Count
    } else if key.ends_with("area") {
        Shape::Area
    } else if key.ends_with("volume") {
        Shape::Volume
    } else {
        Shape::Plain
    }
}

/// A plain number with `d` decimals and the decimal mark / thousands group of the language.
pub fn num_text(v: f64, d: usize, ru: bool) -> String {
    crate::units::fmt_num_lang(v, d, ru)
}

/// One table cell: counts as integers, areas and volumes in the project units, the rest with
/// two decimals; the language is explicit so no global state is read.
pub fn metric_text(key: &str, v: f64, u: crate::units::Units, ru: bool) -> String {
    match shape_of(key) {
        Shape::Count => num_text(v.round(), 0, ru),
        Shape::Area => crate::units::fmt_area_lang(v, u, crate::units::AREA_DECIMALS, false, ru),
        Shape::Volume => crate::units::fmt_volume_lang(v, u, crate::units::VOLUME_DECIMALS, ru),
        Shape::Plain => num_text(v, 2, ru),
    }
}

/// Differences only: of options with the same numbers on every chosen metric the first stays.
/// An option without any of the numbers is unknown, not equal, and stays.
pub fn diff_rows<'a>(rows: &[&'a Row], keys: &[String]) -> Vec<&'a Row> {
    let mut seen: Vec<Vec<Option<u64>>> = Vec::new();
    let mut out = Vec::new();
    for r in rows {
        let sig: Vec<Option<u64>> = keys.iter().map(|k| r.cells.get(k).map(|v| v.to_bits())).collect();
        if sig.iter().all(Option::is_none) {
            out.push(*r);
        } else if !seen.contains(&sig) {
            seen.push(sig);
            out.push(*r);
        }
    }
    out
}

/// Table name used by the live view for an op category; `None` for kinds it has no row for.
fn table_of_op_cat(key: &str) -> Option<&'static str> {
    Some(match key {
        "wall" => "wall",
        "opening" => "opening",
        "curtain_wall" => "curtain_wall",
        "slab" => "slab",
        "room" => "room",
        "mep_run" => "mep_run",
        "component" => "comp",
        "column" => "col",
        "beam" => "beam",
        "ceiling" => "ceiling",
        "mesh" => "mesh",
        "roof" => "roof",
        "stair" => "stair",
        "railing" => "railing",
        _ => return None,
    })
}

fn collect_refs(v: &serde_json::Value, out: &mut Vec<ddc_model::ElKey>) {
    match v {
        serde_json::Value::String(s) => {
            if let Ok(r) = ddc_ops::Ref::parse(s) {
                let mut r = &r;
                while let ddc_ops::Ref::Part { of, .. } = r {
                    r = of;
                }
                if let ddc_ops::Ref::Id { cat, id } = r {
                    if let Some(t) = table_of_op_cat(cat.key()) {
                        let k = (t.to_string(), *id);
                        if !out.contains(&k) {
                            out.push(k);
                        }
                    }
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_refs(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_refs(x, out)),
        _ => {}
    }
}

/// Existing elements a recipe's ops name, in order of first mention. Reads text only.
pub fn option_elements(envelope: &str) -> Vec<ddc_model::ElKey> {
    let v: serde_json::Value = serde_json::from_str(envelope).unwrap_or(serde_json::Value::Null);
    let mut out = Vec::new();
    collect_refs(&v["groups"], &mut out);
    out
}

/// The camera every tile shares (plain numbers; the renderer maps them to its own camera).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileCam {
    pub target: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
}

impl Default for TileCam {
    fn default() -> Self {
        Self {
            target: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            dist: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileView {
    pub option: i64,
    pub cam: TileCam,
    pub level: Option<i64>,
    pub diff_only: bool,
}

/// Up to four tiles, one option each; camera and level exist once, so no tile can drift.
#[derive(Clone, Debug, Default)]
pub struct TileSync {
    tiles: Vec<i64>,
    cam: TileCam,
    level: Option<i64>,
    pub diff_only: bool,
}

impl TileSync {
    /// False when the option is already shown or four tiles are open.
    pub fn add(&mut self, option: i64) -> bool {
        if self.tiles.len() >= TILES_MAX || self.tiles.contains(&option) {
            return false;
        }
        self.tiles.push(option);
        true
    }
    pub fn remove(&mut self, option: i64) {
        self.tiles.retain(|t| *t != option);
    }
    pub fn set_camera(&mut self, cam: TileCam) {
        self.cam = cam;
    }
    pub fn set_level(&mut self, level: Option<i64>) {
        self.level = level;
    }
    pub fn views(&self) -> Vec<TileView> {
        self.tiles
            .iter()
            .map(|o| TileView {
                option: *o,
                cam: self.cam,
                level: self.level,
                diff_only: self.diff_only,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, blocks: Option<usize>, cells: &[(&str, f64)]) -> Row {
        Row {
            id,
            name: format!("o{id}"),
            blocks,
            cells: cells.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn rec_row_reads_snapshot() {
        let mut rec = ddc_model::OptionRec {
            id: 7,
            set: "s".into(),
            name: "A".into(),
            base_head: 0,
            base_stamp: 0,
            envelope: String::new(),
            seed: 0,
            params: "{}".into(),
            generator: None,
            metrics: Some(r#"{"cells":[{"key":"cost","unit":"u","value":3.5}],"findings":{"error":2}}"#.into()),
            note: None,
            why: None,
            status: ddc_model::OptionStatus::Draft,
        };
        let r = row_from_rec(&rec);
        assert_eq!((r.id, r.blocks, r.cells.get("cost").copied()), (7, Some(2), Some(3.5)));
        rec.metrics = None;
        assert!(row_from_rec(&rec).cells.is_empty());
    }

    #[test]
    fn tiles_share_camera_and_level() {
        let mut t = TileSync::default();
        assert!(t.add(1) && t.add(2) && t.add(3) && t.add(4));
        assert!(!t.add(5), "at most four tiles");
        assert!(!t.add(2), "an option is shown once");
        let cam = TileCam {
            target: [1.0, 2.0, 3.0],
            yaw: 0.5,
            pitch: 0.2,
            dist: 12.0,
        };
        t.set_camera(cam);
        t.set_level(Some(7));
        t.diff_only = true;
        let v = t.views();
        assert_eq!(v.len(), 4);
        assert!(v
            .iter()
            .all(|x| x.cam == cam && x.level == Some(7) && x.diff_only));
        t.set_level(None);
        assert!(t.views().iter().all(|x| x.level.is_none()));
    }

    #[test]
    fn pareto_set_correct() {
        let rows = [
            row(1, None, &[("area", 100.0), ("cost", 50.0)]),
            row(2, None, &[("area", 90.0), ("cost", 60.0)]), // beaten by 1
            row(3, None, &[("area", 120.0), ("cost", 80.0)]), // more area, more cost
            row(4, None, &[("area", 100.0), ("cost", 50.0)]), // equal to 1: both stay
            row(5, None, &[("area", 500.0)]),                // no cost: not comparable
        ];
        let r: Vec<&Row> = rows.iter().collect();
        let m = [
            ("area".to_string(), Dir::Max),
            ("cost".to_string(), Dir::Min),
        ];
        assert_eq!(pareto(&r, &m), vec![1, 3, 4]);
        assert!(pareto(&r, &[]).is_empty());
    }

    #[test]
    fn filter_hides_blocked_options() {
        let rows = [
            row(1, Some(0), &[("area", 1300.0)]),
            row(2, Some(2), &[("area", 1500.0)]),
            row(3, None, &[("area", 1000.0)]),
            row(4, Some(0), &[]),
        ];
        let f = Filters {
            hide_blocked: true,
            rules: vec![],
        };
        let ids: Vec<i64> = visible(&rows, &f).iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![1, 3, 4]);
        let f = Filters {
            hide_blocked: true,
            rules: vec![Rule {
                key: "area".into(),
                cmp: Cmp::Ge,
                value: 1200.0,
            }],
        };
        let ids: Vec<i64> = visible(&rows, &f).iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![1], "blocked, too small and unmeasured are hidden");
    }

    #[test]
    fn weighted_score_deterministic_ties_by_id() {
        let rows = [
            row(9, None, &[("a", 10.0), ("c", 5.0)]),
            row(3, None, &[("a", 10.0), ("c", 5.0)]),
            row(5, None, &[("a", 0.0), ("c", 9.0)]),
        ];
        let r: Vec<&Row> = rows.iter().collect();
        let w = [
            Weight {
                key: "a".into(),
                weight: 2.0,
                dir: Dir::Max,
            },
            Weight {
                key: "c".into(),
                weight: 1.0,
                dir: Dir::Min,
            },
        ];
        let first = ranking(&r, &w);
        let rev: Vec<&Row> = rows.iter().rev().collect();
        assert_eq!(first, ranking(&rev, &w));
        let ids: Vec<i64> = first.iter().map(|x| x.0).collect();
        assert_eq!(ids, vec![3, 9, 5]);
        assert_eq!(first[0].1, first[1].1);
        assert!(first.iter().all(|x| (0.0..=1.0).contains(&x.1)));
        assert!(ranking(&r, &[]).iter().all(|x| x.1 == 0.0));
    }

    #[test]
    fn empty_list_gives_empty_state_key() {
        assert_eq!(empty_key(0, 0), Some("ai.cmp.empty"));
        assert_eq!(empty_key(3, 0), Some("ai.cmp.no_match"));
        assert_eq!(empty_key(3, 2), None);
        let f = Filters::default();
        assert!(visible(&[], &f).is_empty());
        assert!(pareto(&[], &[("a".into(), Dir::Max)]).is_empty());
    }

    #[test]
    fn counts_are_whole_numbers_in_both_languages() {
        use crate::units::Units;
        assert_eq!(metric_text("findings_error", 3.0, Units::Metric, false), "3");
        assert_eq!(metric_text("findings_error", 3.0, Units::Metric, true), "3");
    }

    #[test]
    fn cost_uses_the_language_mark() {
        use crate::units::Units;
        let en = metric_text("cost", 1234.5, Units::Metric, false);
        let ru = metric_text("cost", 1234.5, Units::Metric, true);
        assert_eq!(en, "1,234.50");
        assert_eq!(ru, "1\u{a0}234,50");
    }

    #[test]
    fn area_follows_project_units_and_language() {
        use crate::units::Units;
        let m = metric_text("room_area", 12.345, Units::Metric, false);
        assert!(m.starts_with("12.35") || m.starts_with("12.34"), "{m}");
        assert!(m.contains('\u{a0}'), "unit is separated: {m}");
        let ru = metric_text("room_area", 12.345, Units::Metric, true);
        assert!(ru.starts_with("12,3"), "{ru}");
        let ft = metric_text("room_area", 10.0, Units::Imperial, false);
        assert!(ft.ends_with("ft\u{b2}"), "{ft}");
        assert!(ft.starts_with("107.6"), "{ft}");
    }

    #[test]
    fn differences_only_hides_equal_rows() {
        let a = row(1, None, &[("cost", 5.0), ("room_area", 9.0)]);
        let b = row(2, None, &[("cost", 5.0), ("room_area", 9.0)]);
        let c = row(3, None, &[("cost", 6.0), ("room_area", 9.0)]);
        let n = row(4, None, &[]);
        let keys = vec!["cost".to_string(), "room_area".to_string()];
        let got: Vec<i64> = diff_rows(&[&a, &b, &c, &n], &keys).iter().map(|r| r.id).collect();
        assert_eq!(got, vec![1, 3, 4], "the duplicate goes, unknown stays");
        let one: Vec<i64> = diff_rows(&[&a], &keys).iter().map(|r| r.id).collect();
        assert_eq!(one, vec![1]);
    }

    #[test]
    fn option_elements_reads_existing_refs_only() {
        let env = r#"{"v":1,"title":"wall:99","groups":[{"id":1,"ops":[
            {"op":"wall","action":"set","targets":{"refs":["wall:5","component:7","wall:5/face:left"]}},
            {"op":"level","action":"create","slot":1,"name":"Level 9"},
            {"op":"x","at":"new:1","t":"level:2","c":"column:8"}]}]}"#;
        assert_eq!(
            option_elements(env),
            vec![("wall".to_string(), 5), ("comp".to_string(), 7), ("col".to_string(), 8)]
        );
        assert!(option_elements("not json").is_empty());
    }
}
