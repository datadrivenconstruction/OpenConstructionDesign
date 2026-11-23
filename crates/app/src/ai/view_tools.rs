// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View tools: show, isolate or color a target, and reset. They change what the screen shows,
//! never the model, so they need no Apply: the worker resolves the target on its copy, exactly
//! as `query_elements` does, and sends the view command to the UI thread (`view_state`), which
//! keeps the view it replaced so `reset_view` (or the Reset view button) brings it back.

use super::fmt::Out;
use super::plan::{self, ElRef, IdOrNew, Targets};
use super::rows;
use super::session::{Core, Done, FromWorker, Handler};
use super::sets::Key;
use super::tools_read::row_value;
use super::tools_write::target_of;
use crate::editor::Editor;
use crate::query::{self, Expr, Row};
use ddc_ai::CallCx;
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

// A slice: the count can never drift from the entries.
pub const HANDLERS: &[(&str, Handler)] = &[
    ("show_elements", show_elements),
    ("select_similar", select_similar),
    ("isolate_elements", isolate_elements),
    ("color_by", color_by),
    ("reset_view", reset_view),
];

/// Legend rows named one by one; rarer values share one "other" row.
pub const MAX_ROWS: usize = ddc_ai::limits::LEGEND_ROWS.us();
/// Longest target text kept on a result card.
const WHAT_CHARS: usize = 120;

/// The first colors of the auto palette: distinct hues, readable on light and dark canvases.
const BASE: [u32; 12] = [
    0x2f6fdb, 0xe07a1f, 0x2e9e5b, 0xd64545, 0x8a5cd1, 0x1fa3a3, // design-lint: data
    0xc9a227, 0xd45d9e, 0x6b8e23, 0x8c6239, 0x4a90b8, 0xa83279, // design-lint: data
];
/// Elements without a value: a quiet neutral, so the eye goes to the named values first.
pub const EMPTY_RGB: u32 = 0xb4b4b4; // design-lint: data
/// The folded rarer values.
pub const OTHER_RGB: u32 = 0x7f7f7f; // design-lint: data

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    Value,
    Empty,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LegendRow {
    /// The value as text; empty for `Bucket::Empty` and `Bucket::Other`.
    pub value: String,
    pub bucket: Bucket,
    pub rgb: u32,
    pub keys: Arc<Vec<Key>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Legend {
    /// The property as the tool named it.
    pub by: String,
    pub rows: Vec<LegendRow>,
    /// Values folded into the `Other` row.
    pub folded: usize,
}

impl Legend {
    pub fn count(&self) -> usize {
        self.rows.iter().map(|r| r.keys.len()).sum()
    }
}

/// What the UI thread does to the view. Keys are live element keys: the copy keeps the ids.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewCmd {
    /// Select, highlight and zoom; `level` is the level most of them are on.
    Show {
        keys: Arc<Vec<Key>>,
        level: Option<i64>,
    },
    Isolate {
        keys: Arc<Vec<Key>>,
        hide: bool,
    },
    Color(Arc<Legend>),
    /// A result card's Select: the selection only.
    Select(Arc<Vec<Key>>),
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoundBy {
    Query,
    Show,
    Isolate,
    Hide,
    Color,
}

/// A result that names elements, for the chat's result card.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub by: FoundBy,
    /// The filter or set the tool was given, cut to `WHAT_CHARS`.
    pub what: String,
    pub keys: Arc<Vec<Key>>,
    /// Elements in scope: the categories the filter names, else the categories found.
    pub of: usize,
    pub level: Option<i64>,
}

/// The palette color of the `i`-th value; the same index always gives the same color.
pub fn color_at(i: usize) -> u32 {
    if let Some(c) = BASE.get(i) {
        return *c;
    }
    // golden-angle hues past the base set, two lightness steps so neighbours differ
    let h = (i as f64 * 137.507_764) % 360.0;
    let (s, v) = if i % 2 == 0 { (0.55, 0.82) } else { (0.45, 0.62) };
    hsv(h, s, v)
}

pub fn palette(n: usize) -> Vec<u32> {
    (0..n).map(color_at).collect()
}

fn hsv(h: f64, s: f64, v: f64) -> u32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |t: f64| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// Groups to legend rows: most elements first, ties by value; the empty value last; past
/// `MAX_ROWS` the rarer values fold into one row. Colors follow the row order.
pub fn legend(by: &str, groups: BTreeMap<String, Vec<Key>>) -> Legend {
    let mut empty: Vec<Key> = vec![];
    let mut named: Vec<(String, Vec<Key>)> = vec![];
    for (v, keys) in groups {
        if v.trim().is_empty() {
            empty.extend(keys);
        } else {
            named.push((v, keys));
        }
    }
    named.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let room = MAX_ROWS - usize::from(!empty.is_empty());
    let keep = if named.len() > room { room - 1 } else { named.len() };
    let rest = named.split_off(keep);
    let mut rows: Vec<LegendRow> = named
        .into_iter()
        .enumerate()
        .map(|(i, (value, mut keys))| {
            keys.sort();
            LegendRow {
                value,
                bucket: Bucket::Value,
                rgb: color_at(i),
                keys: Arc::new(keys),
            }
        })
        .collect();
    let folded = rest.len();
    if folded > 0 {
        let mut keys: Vec<Key> = rest.into_iter().flat_map(|r| r.1).collect();
        keys.sort();
        rows.push(LegendRow {
            value: String::new(),
            bucket: Bucket::Other,
            rgb: OTHER_RGB,
            keys: Arc::new(keys),
        });
    }
    if !empty.is_empty() {
        empty.sort();
        rows.push(LegendRow {
            value: String::new(),
            bucket: Bucket::Empty,
            rgb: EMPTY_RGB,
            keys: Arc::new(empty),
        });
    }
    Legend {
        by: by.to_string(),
        rows,
        folded,
    }
}

fn text_of(v: J) -> String {
    match v {
        J::String(s) => s,
        J::Null => String::new(),
        other => other.to_string(),
    }
}

/// The value an element shows for `by`: a field, the phase it was created in, or a parameter.
pub fn value_of(
    r: &Row,
    by: &str,
    level: &dyn Fn(i64) -> String,
    phases: Option<&ddc_model::PhaseIndex>,
) -> String {
    let word = by.trim();
    let canon = query::canon(word);
    match canon.as_str() {
        "level" => level(r.level),
        "category" => r.cat.to_string(),
        "phase" | "phases" => phases.map_or_else(String::new, |p| {
            let k = rows::key_of(r);
            p.name(p.get(k.table(), k.id).created).to_string()
        }),
        "type" | "name" | "mark" | "id" => text_of(row_value(r, &canon, level)),
        _ => text_of(row_value(r, word, level)),
    }
}

fn is_cat(e: &Expr) -> bool {
    matches!(e, Expr::Cat(_))
}

/// Categories a filter is about: its own category words at the top level.
fn cats_of(e: &Expr, out: &mut Vec<&'static str>) {
    match e {
        Expr::Cat(c) => out.push(c),
        Expr::Or(v) if v.iter().all(is_cat) => v.iter().for_each(|x| cats_of(x, out)),
        Expr::And(v) => {
            for x in v {
                if is_cat(x) || matches!(x, Expr::Or(o) if o.iter().all(is_cat)) {
                    cats_of(x, out);
                }
            }
        }
        _ => {}
    }
}

fn cat_is(r: &Row, c: &str) -> bool {
    r.cat == c || (c == "opening" && (r.cat == "door" || r.cat == "window"))
}

/// The M of "N of M": elements of the categories the filter names; without a filter, of the
/// categories found; a filter that names none counts the whole model.
pub fn scope_total(all: &[Row], expr: Option<&Expr>, hits: &[Key]) -> usize {
    let mut cats = vec![];
    match expr {
        Some(e) => cats_of(e, &mut cats),
        None => cats.extend(hits.iter().map(|k| k.cat)),
    }
    cats.sort_unstable();
    cats.dedup();
    let n = if cats.is_empty() {
        all.len()
    } else {
        all.iter().filter(|r| cats.iter().any(|c| cat_is(r, c))).count()
    };
    n.max(hits.len())
}

/// The level most of the elements are on; ties go to the lower level id.
pub fn main_level(all: &[Row], keys: &[Key]) -> Option<i64> {
    let want: HashSet<(&'static str, i64)> = keys.iter().map(|k| (k.table(), k.id)).collect();
    let mut by: BTreeMap<i64, usize> = BTreeMap::new();
    for r in all {
        if want.contains(&(rows::key_of(r).table(), r.id())) {
            *by.entry(r.level).or_default() += 1;
        }
    }
    by.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|x| x.0)
}

/// A resolved target with its scope.
#[derive(Clone, Debug, PartialEq)]
pub struct Hits {
    pub keys: Vec<Key>,
    pub of: usize,
    pub level: Option<i64>,
    pub what: String,
}

fn cut(s: &str) -> String {
    s.chars().take(WHAT_CHARS).collect()
}

/// A filter target: the same parse, filter and rows as `query_elements` (`plan::filter_keys`).
pub fn resolve_filter(ed: &Editor, filter: &str) -> Result<Hits, String> {
    let keys = plan::filter_keys(ed, filter).map_err(|e| e.0)?;
    let expr = query::parse(filter).map_err(|e| e.to_string())?.expr;
    let all = rows::rows(ed);
    Ok(Hits {
        of: scope_total(&all, Some(&expr), &keys),
        level: main_level(&all, &keys),
        what: cut(filter),
        keys,
    })
}

/// A new element of the draft is not in the model yet: the view cannot show it.
fn refuse_new(t: &J) -> Result<(), String> {
    for r in t["refs"].as_array().into_iter().flatten() {
        let s = r.as_str().unwrap_or_default();
        let drafted = ElRef::parse_many(s)
            .is_ok_and(|v| v.iter().any(|r| matches!(r.id, IdOrNew::New(_))));
        if drafted {
            return Err(format!("{s} is a draft element: it is in the model only after Apply"));
        }
    }
    Ok(())
}

fn hits(c: &mut Core, t: &J) -> Result<Hits, String> {
    refuse_new(t)?;
    let target = target_of(c, t)?;
    if let Targets::Filter(f) = &target {
        return resolve_filter(&c.ed, f);
    }
    let keys = c.resolve_target(&target).map_err(|e| e.0)?;
    let all = rows::rows(&c.ed);
    let what = match t["set"].as_str() {
        Some(s) => cut(s),
        None => cut(&keys.iter().take(4).map(Key::text).collect::<Vec<_>>().join(", ")),
    };
    Ok(Hits {
        of: scope_total(&all, None, &keys),
        level: main_level(&all, &keys),
        what,
        keys,
    })
}

/// The chat's card for a `query_elements` result; the reply to the model is unchanged.
pub fn query_card(c: &Core, all: &[Row], filter: &str, expr: &Expr, keys: &[Key]) {
    c.send(FromWorker::Found(Found {
        by: FoundBy::Query,
        what: cut(filter),
        keys: Arc::new(keys.to_vec()),
        of: scope_total(all, Some(expr), keys),
        level: main_level(all, keys),
    }));
}

/// Card, frozen set and reply of a view tool.
fn answer(c: &mut Core, by: FoundBy, h: Hits, keys: Arc<Vec<Key>>) -> Out {
    c.send(FromWorker::Found(Found {
        by,
        what: h.what.clone(),
        keys,
        of: h.of,
        level: h.level,
    }));
    let n = h.keys.len();
    let marks = c.marks_of(&h.keys);
    let set = c.sets.add_marked(h.keys, vec![], marks);
    let mut o = Out::new(c.max_chars);
    o.put("count", &json!(n));
    o.put("of", &json!(h.of));
    o.put("summary", &json!(format!("{n} of {}", h.of)));
    o.put("set", &json!(format!("s{set}")));
    o.put("view_changed", &json!(n > 0));
    o
}

fn show_elements(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let h = hits(c, &a["target"])?;
    let keys = Arc::new(h.keys.clone());
    if !keys.is_empty() {
        c.send(FromWorker::View(ViewCmd::Show {
            keys: keys.clone(),
            level: h.level,
        }));
    }
    Ok(answer(c, FoundBy::Show, h, keys).finish().into())
}

/// Elements like each target element, as «Select similar» finds them, without repeats.
pub fn similar_keys(ed: &Editor, seeds: &[Key], project: bool, by_cat: bool) -> Vec<Key> {
    let all = rows::rows(ed);
    let key: HashMap<crate::editor::Sel, Key> = all.iter().map(|r| (r.sel, rows::key_of(r))).collect();
    let mut seen = HashSet::new();
    let mut out = vec![];
    for s in seeds.iter().filter_map(|k| rows::sel_of(*k)) {
        let found = if by_cat {
            query::same_category(ed, s, project, |_| true)
        } else {
            crate::selection::similar(ed, s, project, |_| true)
        };
        for k in found.into_iter().filter_map(|f| key.get(&f).copied()) {
            if seen.insert(k) {
                out.push(k);
            }
        }
    }
    out
}

fn select_similar(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let h = hits(c, &a["target"])?;
    if h.keys.is_empty() {
        return Err("target: no element to match".into());
    }
    let project = a["scope"].as_str() != Some("level");
    let by_cat = a["match"].as_str() == Some("category");
    let found = similar_keys(&c.ed, &h.keys, project, by_cat);
    let all = rows::rows(&c.ed);
    let h = Hits {
        of: scope_total(&all, None, &found),
        level: main_level(&all, &found),
        what: h.what,
        keys: found,
    };
    let keys = Arc::new(h.keys.clone());
    if !keys.is_empty() {
        c.send(FromWorker::View(ViewCmd::Show {
            keys: keys.clone(),
            level: h.level,
        }));
    }
    Ok(answer(c, FoundBy::Show, h, keys).finish().into())
}

fn isolate_elements(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let hide = a["mode"].as_str() == Some("hide");
    let h = hits(c, &a["target"])?;
    let keys = Arc::new(h.keys.clone());
    if !keys.is_empty() {
        c.send(FromWorker::View(ViewCmd::Isolate {
            keys: keys.clone(),
            hide,
        }));
    }
    let by = if hide { FoundBy::Hide } else { FoundBy::Isolate };
    let mut o = answer(c, by, h, keys);
    o.put("mode", &json!(if hide { "hide" } else { "isolate" }));
    Ok(o.finish().into())
}

/// Groups the target by `by`; without a target, every element of the model.
pub fn color_groups(
    ed: &Editor,
    by: &str,
    keys: Option<&[Key]>,
) -> Result<BTreeMap<String, Vec<Key>>, String> {
    let levels: HashMap<i64, String> = ed.levels().into_iter().map(|l| (l.0, l.1)).collect();
    let level = |id: i64| levels.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let phases = match query::canon(by.trim()).as_str() {
        "phase" | "phases" => Some(ed.model.phase_index().map_err(|e| e.to_string())?),
        _ => None,
    };
    let want: Option<HashSet<(&'static str, i64)>> =
        keys.map(|k| k.iter().map(|k| (k.table(), k.id)).collect());
    let mut groups: BTreeMap<String, Vec<Key>> = BTreeMap::new();
    for r in rows::rows(ed).iter() {
        let k = rows::key_of(r);
        if want.as_ref().is_some_and(|w| !w.contains(&(k.table(), k.id))) {
            continue;
        }
        let v = value_of(r, by, &level, phases.as_ref());
        groups.entry(v.trim().to_string()).or_default().push(k);
    }
    Ok(groups)
}

fn color_by(c: &mut Core, a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    let by = a["by"].as_str().map(str::trim).unwrap_or_default().to_string();
    if by.is_empty() {
        return Err("by must name a property".to_string());
    }
    let h = if a["target"].is_null() {
        let all = rows::rows(&c.ed);
        let keys: Vec<Key> = all.iter().map(rows::key_of).collect();
        Hits {
            of: all.len(),
            level: None,
            what: String::new(),
            keys,
        }
    } else {
        hits(c, &a["target"])?
    };
    let target = (!a["target"].is_null()).then_some(h.keys.as_slice());
    let lg = legend(&by, color_groups(&c.ed, &by, target)?);
    let values: Vec<J> = lg
        .rows
        .iter()
        .map(|r| match r.bucket {
            Bucket::Value => json!([r.value, r.keys.len()]),
            Bucket::Empty => json!([J::Null, r.keys.len()]),
            Bucket::Other => json!(["(other values)", r.keys.len()]),
        })
        .collect();
    let folded = lg.folded;
    let keys = Arc::new(h.keys.clone());
    if !keys.is_empty() {
        c.send(FromWorker::View(ViewCmd::Color(Arc::new(lg))));
    }
    let mut o = answer(c, FoundBy::Color, h, keys);
    o.put("by", &json!(by));
    o.put("values", &J::from(values));
    o.put("folded_values", &json!(folded));
    Ok(o.finish().into())
}

fn reset_view(c: &mut Core, _a: &J, _cx: &CallCx<'_>) -> Result<Done, String> {
    c.send(FromWorker::View(ViewCmd::Reset));
    let mut o = Out::new(c.max_chars);
    o.put("reset", &json!(true));
    Ok(o.finish().into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ddc_model::{OpeningSpec, PVal, ParamSet, ParamSpec, ParamType};

    #[test]
    fn similar_keys_match_by_type_or_category_without_repeats() {
        let (mut ed, ids) = windows(6, &[]);
        let wide = OpeningSpec {
            along: 16.0,
            width: 2.0,
            sill: 0.9,
            height: 1.5,
            kind: "window".into(),
        };
        let w0 = ed.walls[0].id;
        let odd = ed.model.add_opening(w0, &wide).unwrap();
        ed.reload().unwrap();
        let win = |id| Key { cat: "window", id };
        let same = similar_keys(&ed, &[win(ids[0]), win(ids[1])], true, false);
        assert_eq!(same.len(), 6, "same size only, each once: {same:?}");
        assert!(!same.contains(&win(odd)));
        let cat = similar_keys(&ed, &[win(ids[0])], true, true);
        assert_eq!(cat.len(), 7, "{cat:?}");
        let walls = similar_keys(&ed, &[Key { cat: "wall", id: w0 }], false, false);
        assert_eq!(walls.len(), 2);
    }

    /// `n` windows, four to a 20 m wall; every window has a fire rating except `unrated`.
    pub(crate) fn windows(n: usize, unrated: &[usize]) -> (Editor, Vec<i64>) {
        let mut ed = Editor::new_in_memory().unwrap();
        let segs: Vec<((f64, f64), (f64, f64))> = (0..n.div_ceil(4))
            .map(|i| ((0.0, i as f64 * 3.0), (20.0, i as f64 * 3.0)))
            .collect();
        crate::modify_tools::tests::walls(&mut ed, &segs);
        let walls: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        let mut ids = vec![];
        for i in 0..n {
            let spec = OpeningSpec {
                along: 1.0 + (i % 4) as f64 * 4.5,
                width: 1.2,
                sill: 0.9,
                height: 1.5,
                kind: "window".into(),
            };
            ids.push(ed.model.add_opening(walls[i / 4], &spec).unwrap());
        }
        let p = ed
            .model
            .add_param(&ParamSpec {
                name: "fire_rating".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["window".into()],
            })
            .unwrap();
        let sets: Vec<ParamSet> = ids
            .iter()
            .enumerate()
            .filter(|(i, _)| !unrated.contains(i))
            .map(|(i, id)| ParamSet {
                param: p,
                kind: "opening".into(),
                id: *id,
                value: Some(PVal::Text(if i % 3 == 0 { "EI 60" } else { "EI 30" }.into())),
            })
            .collect();
        ed.model.set_param_values(&sets).unwrap();
        ed.reload().unwrap();
        (ed, ids)
    }

    fn k(cat: &'static str, id: i64) -> Key {
        Key { cat, id }
    }

    fn row(cat: &'static str, id: i64, level: i64) -> Row {
        Row {
            sel: crate::editor::Sel::Opening(id),
            cat,
            level,
            ty: String::new(),
            name: String::new(),
            mark: String::new(),
            nums: vec![],
            props: vec![],
        }
    }

    #[test]
    fn palette_is_deterministic_and_distinct() {
        assert_eq!(palette(40), palette(40));
        let p = palette(40);
        let uniq: HashSet<u32> = p.iter().copied().collect();
        assert_eq!(uniq.len(), 40, "{p:x?}");
        assert_eq!(palette(5)[3], color_at(3));
        assert_eq!(palette(30)[20], color_at(20));
        assert!(!p.contains(&EMPTY_RGB) && !p.contains(&OTHER_RGB));
        assert!(p.iter().all(|c| *c <= 0xff_ffff));
    }

    fn groups(spec: &[(&str, usize)]) -> BTreeMap<String, Vec<Key>> {
        let mut g = BTreeMap::new();
        let mut id = 0;
        for (v, n) in spec {
            let e: &mut Vec<Key> = g.entry(v.to_string()).or_default();
            for _ in 0..*n {
                id += 1;
                e.push(k("window", id));
            }
        }
        g
    }

    #[test]
    fn legend_counts_order_and_colors() {
        let spec = [("EI 60", 3), ("", 2), ("EI 30", 5), ("EI 90", 3)];
        let lg = legend("fire_rating", groups(&spec));
        let shown: Vec<(&str, Bucket, usize)> =
            lg.rows.iter().map(|r| (r.value.as_str(), r.bucket, r.keys.len())).collect();
        assert_eq!(
            shown,
            vec![
                ("EI 30", Bucket::Value, 5),
                ("EI 60", Bucket::Value, 3),
                ("EI 90", Bucket::Value, 3),
                ("", Bucket::Empty, 2),
            ]
        );
        assert_eq!(lg.count(), 13);
        assert_eq!(lg.folded, 0);
        let colors: Vec<u32> = lg.rows.iter().map(|r| r.rgb).collect();
        assert_eq!(colors, vec![color_at(0), color_at(1), color_at(2), EMPTY_RGB]);
        let again = [("EI 30", 5), ("EI 90", 3), ("EI 60", 3), ("", 2)];
        assert_eq!(lg.rows.len(), legend("fire_rating", groups(&again)).rows.len());
        let counts = |l: &Legend| l.rows.iter().map(|r| r.keys.len()).collect::<Vec<_>>();
        assert_eq!(counts(&lg), counts(&legend("fire_rating", groups(&again))));
        assert_eq!(lg, legend("fire_rating", groups(&spec)), "same input, same legend");
    }

    #[test]
    fn many_values_fold_into_one_row_and_keep_every_element() {
        let spec: Vec<(String, usize)> = (0..40).map(|i| (format!("v{i:02}"), 40 - i)).collect();
        let refs: Vec<(&str, usize)> = spec.iter().map(|(v, n)| (v.as_str(), *n)).collect();
        let mut g = groups(&refs);
        g.insert(String::new(), vec![k("window", 9999)]);
        let lg = legend("mark", g);
        assert_eq!(lg.rows.len(), MAX_ROWS);
        assert_eq!(lg.count(), (1..=40).sum::<usize>() + 1);
        let other = lg.rows.iter().filter(|r| r.bucket == Bucket::Other).count();
        assert_eq!(other, 1);
        assert_eq!(lg.rows.last().map(|r| r.bucket), Some(Bucket::Empty));
        assert_eq!(lg.folded, 40 - (MAX_ROWS - 2));
        assert_eq!(lg.rows[0].value, "v00");
    }

    #[test]
    fn scope_counts_the_named_categories() {
        let all = vec![
            row("window", 1, 1),
            row("window", 2, 1),
            row("door", 3, 1),
            row("wall", 4, 2),
        ];
        let q = |s: &str| query::parse(s).unwrap().expr;
        let unrated = q("window fire_rating=empty");
        assert_eq!(scope_total(&all, Some(&unrated), &[k("window", 1)]), 2);
        assert_eq!(scope_total(&all, Some(&q("opening")), &[]), 3);
        assert_eq!(scope_total(&all, Some(&q("level=1")), &[]), 4);
        assert_eq!(scope_total(&all, None, &[k("door", 3)]), 1);
        assert_eq!(main_level(&all, &[k("window", 1), k("window", 2), k("wall", 4)]), Some(1));
        assert_eq!(main_level(&all, &[]), None);
    }

    #[test]
    fn a_draft_reference_is_refused() {
        assert!(refuse_new(&json!({"refs": ["wall:n2"]})).is_err());
        assert!(refuse_new(&json!({"refs": ["wall:2"]})).is_ok());
        assert!(refuse_new(&json!({"filter": "wall"})).is_ok());
    }

    /// The view tools and `query_elements` see the same elements for the same filter text.
    #[test]
    fn a_filter_maps_to_the_query_elements_set() {
        let (ed, ids) = windows(12, &[2, 7]);
        for f in [
            "window",
            "window fire_rating=empty",
            "window fire_rating=\"EI 60\"",
            "wall",
            "opening",
            "window fire_rating!=empty",
        ] {
            let h = resolve_filter(&ed, f).unwrap();
            // what query_elements runs: parse, the tool environment, the cached rows
            let q = query::parse(f).unwrap();
            let env = query::Env::tool(&ed);
            let flt = query::Filter::new(&q.expr, &env);
            let all = rows::rows(&ed);
            let want: Vec<Key> = all.iter().filter(|r| flt.test(r)).map(rows::key_of).collect();
            assert_eq!(h.keys, want, "{f}");
        }
        let h = resolve_filter(&ed, "window fire_rating=empty").unwrap();
        let mut got: Vec<i64> = h.keys.iter().map(|k| k.id).collect();
        got.sort_unstable();
        assert_eq!(got, vec![ids[2], ids[7]]);
        assert_eq!(h.of, 12);
        assert_eq!(h.level, Some(ed.level));
        assert!(resolve_filter(&ed, "window width=1,5").is_err(), "AI numbers stay strict");
    }

    #[test]
    fn color_groups_by_parameter_level_and_phase() {
        let (ed, _) = windows(9, &[4]);
        let g = color_groups(&ed, "fire_rating", None).unwrap();
        let count = |v: &str| g.get(v).map_or(0, Vec::len);
        assert_eq!((count("EI 60"), count("EI 30"), count("")), (3, 5, 1 + 3));
        let windows: Vec<Key> =
            g.values().flatten().filter(|k| k.cat == "window").copied().collect();
        let only = color_groups(&ed, "fire_rating", Some(&windows)).unwrap();
        assert_eq!(only.values().map(Vec::len).sum::<usize>(), 9);
        let by_level = color_groups(&ed, "level", Some(&windows)).unwrap();
        assert_eq!(by_level.len(), 1);
        let by_phase = color_groups(&ed, "phase", Some(&windows)).unwrap();
        assert_eq!(by_phase.values().map(Vec::len).sum::<usize>(), 9);
        let by_cat = color_groups(&ed, "cat", None).unwrap();
        assert_eq!(by_cat.get("window").map(Vec::len), Some(9));
        assert_eq!(by_cat.get("wall").map(Vec::len), Some(3));
    }
}
