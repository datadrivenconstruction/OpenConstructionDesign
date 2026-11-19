// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Oracles that read the model through the product's own tools: the search language
//! (`oracle.query`), the totals of the quantity takeoff (`oracle.qty`) and the network check
//! (`sizes_typical`).

use super::case::{GeomO, QtyO, QueryO, ReceiptO};
use super::oracle::{in_range, no_capability, num, range_text, result, EvalCx, Oracle, OracleResult};

pub struct Query(pub QueryO);

impl Oracle for Query {
    fn name(&self) -> String {
        format!("query {}", self.0.q)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = range_text(self.0.min, self.0.max);
        match crate::query::parse(&self.0.q) {
            Ok(q) => {
                let n = crate::query::eval(cx.ed, &q).len();
                result(&self.name(), exp, format!("{n} element(s)"), in_range(n as f64, self.0.min, self.0.max))
            }
            Err(e) => no_capability(&self.name(), exp, &format!("query does not parse: {e}")),
        }
    }
}

/// One total of the whole model by its takeoff key.
pub fn total(t: &ddc_model::Totals, key: &str) -> Option<f64> {
    Some(match key {
        "walls" => t.walls as f64,
        "doors" => t.doors as f64,
        "windows" => t.windows as f64,
        "wall_plan_area" => t.wall_plan_area,
        "wall_volume" => t.wall_volume,
        "slabs" => t.slabs as f64,
        "slab_area" => t.slab_area,
        "slab_volume" => t.slab_volume,
        "rooms" => t.rooms as f64,
        "room_area" => t.room_area,
        "room_volume" => t.room_volume,
        "curtains" => t.curtains as f64,
        "curtain_panel_area" => t.curtain_panel_area,
        _ => return None,
    })
}

/// `x` is within the share `tol` of `value`; a zero target allows `tol` as an absolute bound.
pub fn within(x: f64, value: f64, tol: f64) -> bool {
    (x - value).abs() <= tol * value.abs().max(if value == 0.0 { 1.0 } else { 0.0 }) + 1e-9
}

pub struct Qty(pub QtyO);

impl Oracle for Qty {
    fn name(&self) -> String {
        format!("qty {}", self.0.key)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let tol = self.0.tol.unwrap_or(0.1);
        let exp = format!("{} within {:.0} %", self.0.value, tol * 100.0);
        if self.0.level.is_some() {
            return no_capability(&self.name(), exp, "per-level totals are not read by this oracle");
        }
        let t = match cx.ed.model.model_totals() {
            Ok(t) => t,
            Err(e) => return no_capability(&self.name(), exp, &format!("takeoff failed: {e:?}")),
        };
        match total(&t, &self.0.key) {
            Some(x) => result(&self.name(), exp, format!("{x:.2}"), within(x, self.0.value, tol)),
            None => no_capability(&self.name(), exp, &format!("unknown takeoff key {}", self.0.key)),
        }
    }
}

/// Network check issues that are about sizes, flows and slopes: guidance, not errors.
pub fn sizing_key(key: &str) -> bool {
    ["undersized", "overflow", "overload", "port_size", "slope_low"].iter().any(|k| key.contains(k))
}

pub fn build_geom(g: &GeomO) -> Option<Box<dyn Oracle>> {
    match g.kind.as_str() {
        "sizes_typical" => Some(Box::new(Sizes(g.clone()))),
        "ifc_wellformed" => Some(Box::new(IfcWellformed)),
        _ => None,
    }
}

/// Sizing notes of the network check stay within a share of the runs and the check has no
/// errors: the sizes the model chose are typical for the flows it carries.
struct Sizes(GeomO);

impl Oracle for Sizes {
    fn name(&self) -> String {
        "geom sizes_typical".to_string()
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let (share_max, err_max) = (num(&self.0.p, "share_max").unwrap_or(0.25), num(&self.0.p, "errors_max").unwrap_or(0.0));
        let exp = format!("sizing notes <= {share_max} of runs, errors <= {err_max}");
        let systems = match cx.ed.model.mep_check_all() {
            Ok(s) => s,
            Err(e) => return no_capability(&self.name(), exp, &format!("network check failed: {e:?}")),
        };
        let runs: usize = systems.iter().map(|s| s.runs.len()).sum();
        let (mut notes, mut errors) = (0usize, 0usize);
        for i in systems.iter().flat_map(|s| s.issues.iter()) {
            if i.is_error() {
                errors += 1;
            } else if sizing_key(i.key()) {
                notes += 1;
            }
        }
        if runs == 0 {
            return result(&self.name(), exp, format!("no runs in {} system(s)", systems.len()), false);
        }
        let share = notes as f64 / runs as f64;
        let shown = format!("{notes} sizing note(s) over {runs} run(s) ({share:.3}), {errors} error(s)");
        result(&self.name(), exp, shown, share <= share_max + 1e-9 && errors as f64 <= err_max)
    }
}

pub struct Receipts(pub ReceiptO);

impl Oracle for Receipts {
    fn name(&self) -> String {
        format!("receipt {}", self.0.field)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = range_text(self.0.min, self.0.max);
        if cx.receipts.is_empty() {
            return no_capability(&self.name(), exp, "no plan was applied, so there is no receipt");
        }
        let f = self.0.field.as_str();
        let v: f64 = match f {
            "created" => cx.receipts.iter().map(|r| r.n_created as f64).sum(),
            "changed" => cx.receipts.iter().map(|r| r.n_changed as f64).sum(),
            "deleted" => cx.receipts.iter().map(|r| r.n_deleted as f64).sum(),
            "blocked" => cx.receipts.iter().filter(|r| r.blocked.is_some()).count() as f64,
            _ => {
                if let Some(c) = f.strip_prefix("ifc:") {
                    cx.receipts.iter().flat_map(|r| r.ifc.iter()).filter(|(k, _)| k == c).map(|(_, n)| *n as f64).sum()
                } else if let Some(k) = f.strip_prefix("qty:") {
                    // `qty:key` sums every level; `qty:key@3` only the deltas of level 3
                    let (key, lv) = match k.split_once('@') {
                        Some((key, l)) => match l.parse::<i64>() {
                            Ok(l) => (key, Some(l)),
                            Err(_) => return no_capability(&self.name(), exp, &format!("bad level in {f}")),
                        },
                        None => (k, None),
                    };
                    cx.receipts
                        .iter()
                        .flat_map(|r| r.qty.iter())
                        .filter(|d| d.key == key && lv.is_none_or(|l| d.level == Some(l)))
                        .map(|d| d.after - d.before)
                        .sum()
                } else {
                    return no_capability(&self.name(), exp, &format!("unknown receipt field {f}"));
                }
            }
        };
        result(&self.name(), exp, format!("{v:.2} over {} receipt(s)", cx.receipts.len()), in_range(v, self.0.min, self.0.max))
    }
}

/// `/* ... */` comments become spaces (newlines stay, so line numbers hold); strings are left alone.
fn strip_comments(text: &str) -> String {
    let (mut out, mut in_str, mut in_c) = (String::with_capacity(text.len()), false, false);
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if in_c {
            if c == '*' && it.peek() == Some(&'/') {
                it.next();
                in_c = false;
            } else if c == '\n' {
                out.push('\n');
            }
            continue;
        }
        if in_str {
            in_str = c != '\'';
        } else if c == '\'' {
            in_str = true;
        } else if c == '/' && it.peek() == Some(&'*') {
            it.next();
            in_c = true;
            continue;
        }
        out.push(c);
    }
    out
}

/// The first malformed spot of the DATA section of a STEP file: an empty argument, a list that
/// is not closed, a statement that does not end, a reference to an entity nobody defines, no
/// entity at all. `Ok` carries the number of entities; ENDSEC and the like are not entities.
pub fn step_problem(text: &str) -> Result<usize, String> {
    let text = strip_comments(text);
    let start = text.find("DATA;").ok_or("no DATA section")? + 5;
    let end = text[start..].find("ENDSEC;").map_or(text.len(), |e| start + e);
    let (mut line, mut depth) = (1 + text[..start].matches('\n').count(), 0i32);
    let (mut in_str, mut prev, mut open_at) = (false, ' ', line);
    let (mut defs, mut refs): (std::collections::HashSet<String>, Vec<(String, usize)>) = (Default::default(), vec![]);
    let (mut at_start, mut entities) = (true, 0usize);
    let body: Vec<char> = text[start..end].chars().collect();
    let mut i = 0;
    while i < body.len() {
        let c = body[i];
        i += 1;
        if c == '\n' {
            line += 1;
        }
        if in_str {
            // a doubled quote is read as close-then-open, which leaves the string state right
            in_str = c != '\'';
            continue;
        }
        match c {
            '\'' => in_str = true,
            '#' => {
                let mut id = String::new();
                while i < body.len() && body[i].is_ascii_digit() {
                    id.push(body[i]);
                    i += 1;
                }
                let mut j = i;
                while j < body.len() && body[j].is_whitespace() {
                    j += 1;
                }
                if at_start && depth == 0 && body.get(j) == Some(&'=') {
                    defs.insert(id);
                    entities += 1;
                } else {
                    refs.push((id, line));
                }
                at_start = false;
                prev = '#';
                continue;
            }
            '(' => {
                if depth == 0 {
                    open_at = line;
                }
                depth += 1;
            }
            ')' => {
                if prev == ',' {
                    return Err(format!("line {line}: unexpected ',' before ')'"));
                }
                depth -= 1;
                if depth < 0 {
                    return Err(format!("line {line}: unexpected ')'"));
                }
            }
            ',' if prev == ',' || prev == '(' => return Err(format!("line {line}: unexpected ','")),
            ';' => {
                if depth != 0 {
                    return Err(format!("line {open_at}: statement ends inside a list"));
                }
                at_start = true;
            }
            _ => {}
        }
        if !c.is_whitespace() {
            prev = c;
            if c != ';' {
                at_start = false;
            }
        }
    }
    if depth != 0 {
        return Err(format!("line {open_at}: statement ends inside a list"));
    }
    if let Some((id, l)) = refs.iter().find(|(id, _)| !defs.contains(id)) {
        return Err(format!("line {l}: dangling reference #{id}"));
    }
    if entities == 0 {
        return Err("no entities in DATA".to_string());
    }
    Ok(entities)
}

struct IfcWellformed;

impl Oracle for IfcWellformed {
    fn name(&self) -> String {
        "geom ifc_wellformed".to_string()
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let exp = "the exported STEP reads back".to_string();
        let Some(t) = cx.ifc else {
            return no_capability(&self.name(), exp, "the case has no [oracle.ifc], so nothing was exported");
        };
        match step_problem(t) {
            Ok(n) => result(&self.name(), exp, format!("{n} statements, none malformed"), n > 0),
            Err(e) => result(&self.name(), exp, e, false),
        }
    }
}
