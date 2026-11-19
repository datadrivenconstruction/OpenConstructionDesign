//! Objective scoring of live generation runs and the HTML report, shared by the generator
//! matrix (ddc-ai example) and the full-tool runner (ddc-app e2e_ai_live).
//!
//! A task's `expect` names what the brief fixes; each runner scores the keys it can observe and
//! reports the others as not applicable, so a check never passes by default.
#![allow(dead_code)]

use serde_json::{json, Value as J};

/// USD per million tokens: fresh input, output, cache read, cache write.
#[derive(Clone, Copy, Debug)]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Prices {
    pub fn of(p: &J) -> Prices {
        let f = |k: &str| p[k].as_f64().unwrap_or(0.0);
        Prices { input: f("in"), output: f("out"), cache_read: f("cache_read"), cache_write: f("cache_write") }
    }

    /// `input` holds every input token, cached ones included, as the usage totals report it.
    pub fn cost(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
        let fresh = input.saturating_sub(cache_read + cache_write);
        (fresh as f64 * self.input
            + output as f64 * self.output
            + cache_read as f64 * self.cache_read
            + cache_write as f64 * self.cache_write)
            / 1e6
    }
}

/// The provider profile file: no endpoint or model name lives in the source.
/// `{"base_url", "model", "auth_header", "headers": {k: v}, "prices": {"in","out","cache_read","cache_write"}}`
pub fn load_profile(path: &str) -> Result<J, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("profile {path}: {e}"))?;
    let p: J = serde_json::from_str(&t).map_err(|e| format!("profile {path}: {e}"))?;
    for k in ["base_url", "model", "auth_header"] {
        if !p[k].is_string() {
            return Err(format!("profile {path}: `{k}` is missing"));
        }
    }
    if !p["prices"].is_object() {
        return Err(format!("profile {path}: `prices` is missing"));
    }
    Ok(p)
}

/// The key from the process environment, then from the person's stored environment (a runner
/// started before the variable was set does not see it). Never printed.
pub fn read_key(names: &[&str]) -> Option<String> {
    for n in names {
        if let Ok(v) = std::env::var(n) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    #[cfg(windows)]
    for n in names {
        let out = std::process::Command::new("reg")
            .args(["query", "HKCU\\Environment", "/v", n])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if it.next() == Some(*n) && it.next().is_some_and(|t| t.starts_with("REG_")) {
                let v: Vec<&str> = it.collect();
                if !v.is_empty() {
                    return Some(v.join(" "));
                }
            }
        }
    }
    None
}

fn check(name: &str, pass: bool, detail: String) -> J {
    json!({"name": name, "pass": pass, "detail": detail})
}

/// Numbers written in a reply: `1,500`, `1 500` and `2.5` as values; `0,6` as a decimal comma.
pub fn numbers_in(text: &str) -> Vec<f64> {
    let c: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        if !c[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // digits glued to a name (IPE500, m2) are part of the name
        if i > 0 && (c[i - 1].is_ascii_alphabetic() || c[i - 1] == '_') {
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.') {
                i += 1;
            }
            continue;
        }
        let mut s = String::new();
        while i < c.len() {
            let ch = c[i];
            if ch.is_ascii_digit() {
                s.push(ch);
                i += 1;
            } else if (ch == ',' || ch == '\u{a0}' || ch == '\u{202f}')
                && c[i + 1..].iter().take(3).filter(|d| d.is_ascii_digit()).count() == 3
                && c.get(i + 4).is_none_or(|d| !d.is_ascii_digit())
                && !s.contains('.')
            {
                i += 1;
            } else if (ch == '.' || ch == ',') && c.get(i + 1).is_some_and(|d| d.is_ascii_digit()) && !s.contains('.') {
                s.push('.');
                i += 1;
            } else {
                break;
            }
        }
        if let Ok(v) = s.parse::<f64>() {
            out.push(v);
        }
    }
    out
}

fn near(v: f64, want: f64, tol: f64) -> bool {
    (v - want).abs() <= tol * want.abs().max(1e-9)
}

fn range(e: &J) -> Option<(f64, f64)> {
    match e {
        J::Number(n) => n.as_f64().map(|v| (v, v)),
        J::Array(a) if a.len() == 2 => Some((a[0].as_f64()?, a[1].as_f64()?)),
        _ => None,
    }
}

/// Checks a reply can settle: reference numbers (each within its tolerance) and terms, where
/// `a|b` accepts either spelling.
pub fn score_text(expect: &J, text: &str) -> Vec<J> {
    let mut out = vec![];
    let nums = numbers_in(text);
    for r in expect["numbers"].as_array().into_iter().flatten() {
        let (label, want) = (r["label"].as_str().unwrap_or("?"), r["value"].as_f64().unwrap_or(f64::NAN));
        let tol = r["tol"].as_f64().unwrap_or(0.05);
        let hit = nums.iter().find(|v| near(**v, want, tol));
        out.push(check(
            &format!("ref {label}"),
            hit.is_some(),
            match hit {
                Some(v) => format!("{v} ~ {want}"),
                None => format!("{want} \u{b1}{:.0}% not in reply", tol * 100.0),
            },
        ));
    }
    let low = text.to_lowercase();
    for t in expect["terms"].as_array().into_iter().flatten().filter_map(J::as_str) {
        let ok = t.split('|').any(|alt| low.contains(&alt.to_lowercase()));
        out.push(check(&format!("term {t}"), ok, String::new()));
    }
    out
}

/// Generator facts: `{"built", "gate_failures", "collisions", "storeys", "gfa", "footprint",
/// "rooms": {kind: n}, "apartments", "typology", "dropped_features"}`.
pub fn score_generate(expect: &J, facts: &J, text: &str) -> Vec<J> {
    let mut out = vec![];
    let built = facts["built"].as_bool().unwrap_or(false);
    out.push(check("built", built, String::new()));
    let list = |k: &str| facts[k].as_array().map(|a| a.iter().filter_map(J::as_str).collect::<Vec<_>>().join("; ")).unwrap_or_default();
    let gates = facts["gate_failures"].as_array().map_or(0, Vec::len);
    out.push(check("gates", built && gates == 0, list("gate_failures")));
    let coll = facts["collisions"].as_array().map_or(0, Vec::len);
    out.push(check("no collisions", built && coll == 0, list("collisions")));
    if let Some(t) = expect["typology"].as_str().filter(|t| !t.is_empty()) {
        let got = facts["typology"].as_str().unwrap_or("");
        out.push(check("typology", got == t, format!("got {got}, asked {t}")));
    }
    let n = |k: &str| facts[k].as_f64().unwrap_or(0.0);
    if let Some((lo, hi)) = range(&expect["storeys"]) {
        let s = n("storeys");
        out.push(check("storeys", built && s >= lo && s <= hi, format!("{s} built, brief {lo}-{hi}")));
    }
    if let Some((lo, hi)) = range(&expect["gfa"]) {
        let g = n("gfa");
        out.push(check("gross area", built && g >= lo && g <= hi, format!("{g:.0} m2, brief {lo:.0}-{hi:.0}")));
    }
    if let (Some(w), Some(d)) = (expect["footprint"][0].as_f64(), expect["footprint"][1].as_f64()) {
        let (gw, gd) = (facts["footprint"][0].as_f64().unwrap_or(0.0), facts["footprint"][1].as_f64().unwrap_or(0.0));
        let ok = (near(gw, w, 0.1) && near(gd, d, 0.1)) || (near(gw, d, 0.1) && near(gd, w, 0.1));
        out.push(check("footprint", built && ok, format!("{gw:.1}x{gd:.1}, brief {w}x{d}")));
    }
    for (k, v) in expect["rooms"].as_object().into_iter().flatten() {
        let want = v.as_f64().unwrap_or(0.0);
        let got = facts["rooms"][k].as_f64().unwrap_or(0.0);
        out.push(check(&format!("rooms {k}"), got >= want, format!("{got} of {want}")));
    }
    if let Some(a) = expect["apartments"].as_f64() {
        let got = n("apartments");
        out.push(check("apartments", got >= a, format!("{got} of {a}")));
    }
    if let Some(f) = facts["dropped_features"].as_array() {
        let names: Vec<&str> = f.iter().filter_map(J::as_str).collect();
        out.push(check("features (keyword)", names.is_empty(), names.join(", ")));
    }
    out.extend(score_text(expect, text));
    out
}

fn rows(summary: &J, table: &str) -> i64 {
    summary[table]["rows"].as_i64().unwrap_or(0)
}

/// Full-tool runs: the reply, and model rows before and after Apply (`model.delta`: table ->
/// least new rows; `model.at_least`: table -> least rows after; `model.unchanged`: read-only).
pub fn score_model(expect: &J, finished: bool, applied: bool, drafted: usize, text: &str, before: &J, after: &J) -> Vec<J> {
    let mut out = vec![check("finished", finished, String::new())];
    let m = &expect["model"];
    if m["draft"].as_bool() == Some(true) {
        out.push(check("draft prepared", drafted > 0, format!("{drafted} plan items")));
    }
    if m["unchanged"].as_bool() == Some(true) {
        let same = before == after;
        out.push(check("model unchanged", same, String::new()));
    }
    if m["delta"].is_object() || m["at_least"].is_object() {
        out.push(check("applied", applied, String::new()));
    }
    for (t, v) in m["delta"].as_object().into_iter().flatten() {
        let d = rows(after, t) - rows(before, t);
        let want = v.as_i64().unwrap_or(1);
        out.push(check(&format!("new {t}"), d >= want, format!("+{d}, want +{want}")));
    }
    for (t, v) in m["at_least"].as_object().into_iter().flatten() {
        let got = rows(after, t);
        let want = v.as_i64().unwrap_or(1);
        out.push(check(&format!("{t} rows"), got >= want, format!("{got}, want {want}")));
    }
    out.extend(score_text(expect, text));
    out
}

pub fn tally(checks: &[J]) -> (usize, usize) {
    (checks.iter().filter(|c| c["pass"] == true).count(), checks.len())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn thousands(v: f64) -> String {
    let s = format!("{:.0}", v.round());
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 && ch != '-' {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

const STYLE: &str = r#"<meta name=viewport content="width=device-width,initial-scale=1"><style>:root{--bg:#F7F6F3;--card:#fff;--ink:#181B1F;--mute:#6b6f75;--line:#e3e0db;--bad:#fde8e4;--acc:#E45128}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){--bg:#111316;--card:#1b1e22;--ink:#F0EEEB;--mute:#9a9ea4;--line:#2c3036;--bad:#3a1f1a}}
:root[data-theme="dark"]{--bg:#111316;--card:#1b1e22;--ink:#F0EEEB;--mute:#9a9ea4;--line:#2c3036;--bad:#3a1f1a}
body{margin:0;background:var(--bg);color:var(--ink);font-family:system-ui,sans-serif;padding:24px 16px;max-width:1200px;margin:auto}
.k{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px;margin:16px 0}.k div{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:12px}.k b{display:block;font-size:24px}.k span{color:var(--mute);font-size:13px}
.tw{overflow-x:auto}table{border-collapse:collapse;width:100%;font-size:13px;font-variant-numeric:tabular-nums}td,th{border-bottom:1px solid var(--line);padding:6px 8px;text-align:left;white-space:nowrap}td:last-child{white-space:normal;color:var(--mute)}tr.bad td{background:var(--bad)}
.note{border-left:4px solid var(--acc);background:var(--card);padding:12px 16px;margin:16px 0}</style>"#;

/// The report page from ledger lines (both runners write the same fields). `labels` is the
/// language file: title, subtitle, tile and column names.
pub fn report_html(lines: &[J], labels: &J) -> String {
    let l = |k: &str| esc(labels[k].as_str().unwrap_or(k));
    let ran: Vec<&J> = lines.iter().filter(|x| x["skipped"].is_null()).collect();
    let n = ran.len().max(1) as f64;
    let sum = |k: &str| ran.iter().map(|x| x[k].as_f64().unwrap_or(0.0)).sum::<f64>();
    let ok = ran.iter().filter(|x| x["ok"] == true).count();
    let (mut cp, mut ct) = (0usize, 0usize);
    for x in &ran {
        let c: Vec<J> = x["checks"].as_array().cloned().unwrap_or_default();
        let (p, t) = tally(&c);
        cp += p;
        ct += t;
    }
    let input = sum("input_tokens");
    let cached = sum("cache_read");
    let models: std::collections::BTreeSet<&str> = ran.iter().filter_map(|x| x["model"].as_str()).collect();
    let mut h = format!("<!doctype html><title>{}</title>{STYLE}\n<h1>{}</h1><p style=\"color:var(--mute)\">{} {}</p>\n", l("title"), l("title"), l("subtitle"), esc(&models.into_iter().collect::<Vec<_>>().join(", ")));
    let tiles = [
        (format!("{ok}/{}", ran.len()), l("t_ok")),
        (format!("{cp}/{ct}"), l("t_checks")),
        (format!("${:.2}", sum("cost_usd")), l("t_cost")),
        (thousands(input / n), l("t_in")),
        (thousands(sum("output_tokens") / n), l("t_out")),
        (format!("{:.0} %", if input > 0.0 { cached / input * 100.0 } else { 0.0 }), l("t_cache")),
        (format!("{:.0} s", sum("latency_ms") / n / 1000.0), l("t_time")),
    ];
    h.push_str("<div class=k>");
    for (v, k) in tiles {
        h.push_str(&format!("<div><b>{v}</b><span>{k}</span></div>"));
    }
    h.push_str("</div>\n");
    if let Some(note) = labels["note"].as_str().filter(|s| !s.is_empty()) {
        h.push_str(&format!("<div class=note>{}</div>\n", esc(note)));
    }
    h.push_str(&format!("<h2>{}</h2><div class=tw><table><tr>", l("runs")));
    for c in ["c_id", "c_ok", "c_score", "c_in", "c_out", "c_cost", "c_time", "c_tools", "c_failed"] {
        h.push_str(&format!("<th>{}</th>", l(c)));
    }
    h.push_str("</tr>");
    for x in lines {
        let id = esc(x["id"].as_str().unwrap_or("?"));
        if let Some(why) = x["skipped"].as_str() {
            h.push_str(&format!("<tr><td>{id}</td><td>\u{2014}</td><td colspan=6></td><td>{}</td></tr>", esc(why)));
            continue;
        }
        let c: Vec<J> = x["checks"].as_array().cloned().unwrap_or_default();
        let (p, t) = tally(&c);
        let failed: Vec<String> = c
            .iter()
            .filter(|k| k["pass"] != true)
            .map(|k| {
                let d = k["detail"].as_str().unwrap_or("");
                let name = k["name"].as_str().unwrap_or("?");
                if d.is_empty() { name.to_string() } else { format!("{name}: {}", d.chars().take(140).collect::<String>()) }
            })
            .collect();
        let good = x["ok"] == true;
        h.push_str(&format!(
            "<tr class=\"{}\"><td>{id}</td><td>{}</td><td>{p}/{t}</td><td>{}</td><td>{}</td><td>${:.3}</td><td>{:.0} s</td><td>{}</td><td>{}</td></tr>",
            if good { "" } else { "bad" },
            if good { "\u{2713}" } else { "\u{2717}" },
            thousands(x["input_tokens"].as_f64().unwrap_or(0.0)),
            thousands(x["output_tokens"].as_f64().unwrap_or(0.0)),
            x["cost_usd"].as_f64().unwrap_or(0.0),
            x["latency_ms"].as_f64().unwrap_or(0.0) / 1000.0,
            x["tool_calls"].as_array().map_or(0, Vec::len),
            esc(&failed.join("; ")),
        ));
    }
    h.push_str("</table></div>\n");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_thousands_and_decimals() {
        assert_eq!(
            numbers_in("total 1,500 m2, 2.5 m/s, 0,6 h, 1\u{a0}200 l/s, IPE500, 24 300 m3/h"),
            vec![1500.0, 2.5, 0.6, 1200.0, 24.0, 300.0]
        );
    }

    #[test]
    fn a_reference_number_needs_its_tolerance() {
        let e = json!({"numbers": [{"label": "supply", "value": 900, "tol": 0.05}], "terms": ["damper|clapet"]});
        let c = score_text(&e, "Supply 920 l/s, fire dampers at cores");
        assert_eq!(tally(&c), (2, 2));
        assert_eq!(tally(&score_text(&e, "Supply 1000 l/s")), (0, 2));
    }
}
