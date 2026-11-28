// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tokens, money and time of the assistant as the editor shows them: the display level, the
//! live usage text, the costliest step of a request, and the project's journal of turns with
//! its CSV and JSON export.

use ddc_ai::{StepRecord, UsageTotals};
use ddc_model::Model;
use ddc_ui::Catalog;

/// Project policy key of the display level.
pub const POLICY_KEY: &str = "ai.show_cost";

/// How much of the cost and time the panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    Off,
    Brief,
    Detailed,
}

impl Detail {
    pub const ALL: [Detail; 3] = [Detail::Off, Detail::Brief, Detail::Detailed];

    /// An unset or unknown value is the default: brief.
    pub fn parse(v: Option<&str>) -> Detail {
        match v.map(str::trim) {
            Some("off") => Detail::Off,
            Some("detailed") => Detail::Detailed,
            _ => Detail::Brief,
        }
    }

    pub fn value(self) -> &'static str {
        match self {
            Detail::Off => "off",
            Detail::Brief => "brief",
            Detail::Detailed => "detailed",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Detail::Off => "ai.cost.show.off",
            Detail::Brief => "ai.cost.show.brief",
            Detail::Detailed => "ai.cost.show.detailed",
        }
    }
}

pub fn detail_of(m: &Model) -> Detail {
    Detail::parse(m.ai_policy(POLICY_KEY).ok().flatten().as_deref())
}

/// The usage text and whether the server's price and the estimate disagree by more than a tenth.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageText {
    pub text: String,
    pub diverges: bool,
}

/// What the live row adds: seconds since the request began and the writing speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Live {
    pub secs: Option<u64>,
    pub rate: Option<u64>,
    pub step: Option<u32>,
}

fn tokens(cat: &Catalog, n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    format!("{}k", crate::units::fmt_num_lang(n as f64 / 1000.0, 1, cat.lang))
}

fn money(cat: &Catalog, v: f64) -> String {
    crate::units::fmt_num_lang(v, 2, cat.lang)
}

/// Brief: tokens and one price (the charged one when the server reported it). Detailed: tokens
/// in, out and cached, the estimate and the charged price apart, time, speed and step number.
pub fn usage_text(cat: &Catalog, detail: Detail, u: &UsageTotals, live: Live) -> Option<UsageText> {
    let s = &u.session;
    if detail == Detail::Off || s.total() == 0 {
        return None;
    }
    let charged = s.charged();
    let diverges = match (charged, u.cost) {
        (Some(c), Some(e)) => (c - e).abs() > 0.1 * c.max(e) && (c - e).abs() >= 0.005,
        _ => false,
    };
    let fmt_t = |n: u64| tokens(cat, n);
    let mut parts: Vec<String> = Vec::new();
    if detail == Detail::Brief {
        parts.push(cat.tf("ai.usage.tokens", &[("n", &fmt_t(s.total()))]));
        match (charged, u.cost) {
            (Some(c), _) => parts.push(cat.tf("ai.cost.charged", &[("cost", &money(cat, c))])),
            (None, Some(e)) => parts.push(cat.tf("ai.usage.cost", &[("cost", &money(cat, e))])),
            _ => {}
        }
        if let Some(secs) = live.secs {
            parts.push(cat.tf("ai.stream.secs", &[("n", &secs.to_string())]));
        }
    } else {
        parts.push(cat.tf(
            "ai.cost.tokens_split",
            &[
                ("in", &fmt_t(s.input_tokens)),
                ("out", &fmt_t(s.output_tokens)),
                ("cache", &fmt_t(s.cache_read_tokens)),
            ],
        ));
        if let Some(e) = u.cost {
            parts.push(cat.tf("ai.usage.cost", &[("cost", &money(cat, e))]));
        }
        if let Some(c) = charged {
            parts.push(cat.tf("ai.cost.charged", &[("cost", &money(cat, c))]));
        }
        if let Some(secs) = live.secs {
            parts.push(cat.tf("ai.stream.secs", &[("n", &secs.to_string())]));
        }
        if let Some(r) = live.rate {
            parts.push(cat.tf("ai.stream.rate", &[("n", &r.to_string())]));
        }
        if let Some(n) = live.step {
            parts.push(cat.tf("ai.cost.step_no", &[("n", &n.to_string())]));
        }
    }
    Some(UsageText { text: parts.join(" \u{b7} "), diverges })
}

/// The display level as a three-way choice; a pick is stored in the project at once.
pub fn picker(ui: &mut egui::Ui, th: &ddc_ui::Theme, cat: &Catalog, m: &Model) {
    let now = detail_of(m);
    ui.horizontal(|ui| {
        ui.label(cat.t("ai.cost.show")).on_hover_text(cat.t("ai.cost.show_tip"));
        let names: Vec<String> = Detail::ALL.iter().map(|d| cat.t(d.label_key())).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let sel = Detail::ALL.iter().position(|d| *d == now).unwrap_or(1);
        if let Some(i) = ddc_ui::widgets::segmented(ui, th, &refs, sel) {
            if i != sel {
                let _ = m.set_ai_policy(POLICY_KEY, Detail::ALL[i].value());
            }
        }
    });
}

/// The plan card's total: what the request that made the plan took, and per element.
pub fn plan_total_line(cat: &Catalog, j: &ddc_ai::TaskJournal, elements: usize) -> Option<String> {
    let t = j.totals();
    if t.steps == 0 {
        return None;
    }
    let mut parts = vec![cat.tf("ai.cost.plan_tokens", &[("n", &tokens(cat, t.tokens_in + t.tokens_out))])];
    if let Some(c) = t.cost {
        parts.push(cat.tf("ai.cost.plan_cost", &[("cost", &money(cat, c))]));
    }
    let secs = t.ms / 1000;
    parts.push(if secs >= 60 {
        cat.tf("ai.cost.plan_time", &[("m", &(secs / 60).to_string()), ("s", &(secs % 60).to_string())])
    } else {
        cat.tf("ai.stream.secs", &[("n", &secs.to_string())])
    });
    if elements > 0 {
        parts.push(cat.tf("ai.cost.plan_elements", &[("n", &elements.to_string())]));
        if let Some(c) = t.cost.filter(|c| *c > 0.0) {
            let each = crate::units::fmt_num_lang(c / elements as f64, 4, cat.lang);
            parts.push(cat.tf("ai.cost.plan_each", &[("cost", &each)]));
        }
    }
    Some(parts.join(" \u{b7} "))
}

/// The costliest step of a request: by price, or by tokens when nothing is priced. A single
/// step has nothing to be compared with.
pub fn costliest(steps: &[StepRecord]) -> Option<usize> {
    if steps.len() < 2 {
        return None;
    }
    let key = |s: &StepRecord| (s.cost.unwrap_or(0.0), s.tokens_in + s.tokens_out);
    steps
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            let (x, y) = (key(a), key(b));
            x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal).then(x.1.cmp(&y.1))
        })
        .map(|(i, _)| i)
}

/// One finished or running turn of the project, from the assistant log.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnRow {
    /// Unix seconds.
    pub ts: i64,
    pub model: String,
    pub tok_in: u64,
    pub tok_out: u64,
    pub cost: Option<f64>,
    pub status: String,
    /// How long the turn ran, when the log kept it.
    pub ms: Option<u64>,
}

/// Every turn the project's log still holds, oldest first.
pub fn turn_rows(m: &Model) -> Vec<TurnRow> {
    let mut out = Vec::new();
    let mut after = 0;
    while let Ok(page) = m.audit_list(after, 500) {
        let Some(last) = page.last().map(|r| r.0) else {
            break;
        };
        after = last;
        out.extend(page.into_iter().filter(|(_, _, e)| e.event == "turn").map(|(_, ts, e)| TurnRow {
            ts,
            model: e.model.unwrap_or_default(),
            tok_in: e.tok_in,
            tok_out: e.tok_out,
            cost: e.cost,
            ms: e.plan.as_deref().and_then(|p| p.strip_prefix("ms=")).and_then(|n| n.parse().ok()),
            status: e.status,
        }));
    }
    out
}

/// What a turn of this project usually takes: the middle half of the finished ones (all of
/// them when there are few), so one odd turn does not set the range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanEstimate {
    pub cost: Option<(f64, f64)>,
    pub secs: Option<(u64, u64)>,
    pub turns: usize,
}

fn middle<T: Copy + PartialOrd>(mut v: Vec<T>) -> Option<(T, T)> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n >= 4 { (v[n / 4], v[(3 * n) / 4]) } else { (v[0], v[n - 1]) })
}

/// From at least two earlier turns that ended normally or at a limit; nothing to say otherwise.
pub fn plan_estimate(rows: &[TurnRow]) -> Option<PlanEstimate> {
    let done: Vec<&TurnRow> = rows
        .iter()
        .filter(|r| (r.status == "ok" || r.status == "limit") && r.tok_in + r.tok_out > 0)
        .collect();
    if done.len() < 2 {
        return None;
    }
    let cost = middle(done.iter().filter_map(|r| r.cost).filter(|c| *c > 0.0).collect());
    let secs = middle(done.iter().filter_map(|r| r.ms).map(|m| m / 1000).collect());
    (cost.is_some() || secs.is_some()).then_some(PlanEstimate { cost, secs, turns: done.len() })
}

fn duration(cat: &Catalog, secs: u64) -> String {
    if secs >= 60 {
        cat.tf("ai.cost.plan_time", &[("m", &(secs / 60).to_string()), ("s", &(secs % 60).to_string())])
    } else {
        cat.tf("ai.stream.secs", &[("n", &secs.to_string())])
    }
}

fn range(cat: &Catalog, a: String, b: String) -> String {
    if a == b {
        a
    } else {
        cat.tf("ai.cost.range", &[("a", &a), ("b", &b)])
    }
}

/// The line shown before a long request: money and time of the project's usual turn.
pub fn estimate_text(cat: &Catalog, e: &PlanEstimate) -> String {
    let mut parts = Vec::new();
    if let Some((a, b)) = e.cost {
        let r = range(cat, money(cat, a), money(cat, b));
        parts.push(cat.tf("ai.cost.plan_cost", &[("cost", &r)]));
    }
    if let Some((a, b)) = e.secs {
        parts.push(range(cat, duration(cat, a), duration(cat, b)));
    }
    cat.tf("ai.cost.estimate", &[("what", &parts.join(" \u{b7} ")), ("n", &e.turns.to_string())])
}

/// Tokens and money over `rows`; the money is `None` when no row has a price.
pub fn totals(rows: &[TurnRow]) -> (u64, Option<f64>) {
    let tokens = rows.iter().map(|r| r.tok_in + r.tok_out).sum();
    let cost = rows.iter().filter_map(|r| r.cost).fold(None, |a: Option<f64>, c| Some(a.unwrap_or(0.0) + c));
    (tokens, cost)
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Header and one line per turn; a price that is unknown is an empty cell, not zero.
pub fn rows_csv(rows: &[TurnRow]) -> String {
    let mut out = String::from("time_unix,model,tokens_in,tokens_out,cost,status,seconds\n");
    for r in rows {
        let cost = r.cost.map(|c| format!("{c:.6}")).unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            r.ts,
            csv_cell(&r.model),
            r.tok_in,
            r.tok_out,
            cost,
            csv_cell(&r.status),
            r.ms.map(|m| (m / 1000).to_string()).unwrap_or_default()
        ));
    }
    out
}

pub fn rows_json(rows: &[TurnRow]) -> String {
    let turns: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "time_unix": r.ts, "model": r.model, "tokens_in": r.tok_in,
                "tokens_out": r.tok_out, "cost": r.cost, "status": r.status,
                "seconds": r.ms.map(|m| m / 1000),
            })
        })
        .collect();
    let (tokens, cost) = totals(rows);
    serde_json::to_string_pretty(&serde_json::json!({
        "version": 1, "turns": turns, "totals": { "tokens": tokens, "cost": cost },
    }))
    .unwrap_or_default()
}

/// Saves the project's turns as CSV or JSON, by the extension the person picks. `Ok(false)`:
/// the dialog was closed.
pub fn export_turns(app: &crate::shell::EditorApp) -> Result<bool, String> {
    let rows = turn_rows(&app.ed.model);
    let Some(path) = crate::file_pick::Dlg::new()
        .add_filter("CSV", &["csv"])
        .add_filter("JSON", &["json"])
        .set_file_name(format!("{}.csv", app.sh.cat.t("ai.cost.file")))
        .save_file()
    else {
        return Ok(false);
    };
    let json = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    let text = if json { rows_json(&rows) } else { rows_csv(&rows) };
    std::fs::write(path, text).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ai::{StepRole, Usage};
    use ddc_ui::Lang;

    fn step(n: u32, cost: Option<f64>, tin: u64) -> StepRecord {
        StepRecord {
            n,
            role: StepRole::Planner,
            model: "m".into(),
            tokens_in: tin,
            tokens_cached: 0,
            tokens_out: 10,
            cost,
            charged: None,
            ms: 1,
            tools: vec![],
        }
    }

    fn totals_of(charged: Option<u64>, est: Option<f64>) -> UsageTotals {
        let session = Usage {
            input_tokens: 90_000,
            output_tokens: 1_300,
            cache_read_tokens: 60_000,
            charged_micros: charged,
            ..Usage::default()
        };
        UsageTotals { session, turn: session, cost: est }
    }

    #[test]
    fn the_level_defaults_to_brief_and_off_hides_everything() {
        assert_eq!(Detail::parse(None), Detail::Brief);
        assert_eq!(Detail::parse(Some("junk")), Detail::Brief);
        for d in Detail::ALL {
            assert_eq!(Detail::parse(Some(d.value())), d);
        }
        let cat = Catalog::new(Lang::En);
        assert_eq!(usage_text(&cat, Detail::Off, &totals_of(None, Some(1.0)), Live::default()), None);
    }

    #[test]
    fn brief_prefers_the_charged_price_and_detailed_shows_both() {
        let cat = Catalog::new(Lang::En);
        let u = totals_of(Some(380_000), Some(0.9));
        let b = usage_text(&cat, Detail::Brief, &u, Live::default()).unwrap();
        assert!(b.text.contains("0.38") && !b.text.contains("0.9"), "{}", b.text);
        assert!(b.diverges, "a price far from the estimate is flagged");
        let d = usage_text(
            &cat,
            Detail::Detailed,
            &u,
            Live { secs: Some(12), rate: Some(40), step: Some(3) },
        )
        .unwrap();
        for want in ["0.38", "0.9", "40", "12", "3"] {
            assert!(d.text.contains(want), "{want} in {}", d.text);
        }
        let near = usage_text(&cat, Detail::Brief, &totals_of(Some(100_000), Some(0.104)), Live::default()).unwrap();
        assert!(!near.diverges);
        let none = usage_text(&cat, Detail::Brief, &totals_of(None, None), Live::default()).unwrap();
        assert!(none.text.starts_with("tokens: "));
    }

    #[test]
    fn the_plan_total_names_tokens_money_time_and_elements() {
        let cat = Catalog::new(Lang::En);
        let mut j = ddc_ai::TaskJournal::new("t");
        assert_eq!(plan_total_line(&cat, &j, 5), None);
        let mut a = step(1, Some(1.5), 1000);
        a.ms = 125_000;
        j.steps.push(a);
        let line = plan_total_line(&cat, &j, 3).unwrap();
        for want in ["1.0k", "1.5", "2 min 5 s", "3 elements", "0.5"] {
            assert!(line.contains(want), "{want} in {line}");
        }
        assert!(!plan_total_line(&cat, &j, 0).unwrap().contains("per element"));
    }

    #[test]
    fn the_costliest_step_is_found_by_price_then_tokens() {
        assert_eq!(costliest(&[step(1, Some(1.0), 5)]), None);
        let s = [step(1, Some(0.1), 5), step(2, Some(0.9), 5), step(3, Some(0.2), 5)];
        assert_eq!(costliest(&s), Some(1));
        let s = [step(1, None, 5), step(2, None, 900), step(3, None, 7)];
        assert_eq!(costliest(&s), Some(1));
    }

    #[test]
    fn the_project_journal_exports_csv_and_json() {
        let rows = vec![
            TurnRow { ts: 10, model: "a, b".into(), tok_in: 100, tok_out: 5, cost: Some(0.25), status: "ok".into(), ms: Some(61_500) },
            TurnRow { ts: 20, model: "m".into(), tok_in: 1, tok_out: 1, cost: None, status: "limit".into(), ms: None },
        ];
        let csv = rows_csv(&rows);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[1], "10,\"a, b\",100,5,0.250000,ok,61");
        assert_eq!(lines[2], "20,m,1,1,,limit,");
        let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
        assert_eq!(v["totals"]["tokens"], 107);
        assert_eq!(v["totals"]["cost"], 0.25);
        assert!(v["turns"][1]["cost"].is_null());
    }

    #[test]
    fn the_estimate_is_the_middle_of_earlier_turns() {
        let cat = Catalog::new(Lang::En);
        let row = |cost: f64, ms: u64, status: &str| TurnRow {
            ts: 0,
            model: String::new(),
            tok_in: 10,
            tok_out: 5,
            cost: Some(cost),
            status: status.into(),
            ms: Some(ms),
        };
        assert_eq!(plan_estimate(&[row(1.0, 1000, "ok")]), None, "one turn is no basis");
        let rows: Vec<TurnRow> = [(0.1, 10_000), (1.0, 60_000), (2.0, 120_000), (3.0, 130_000), (90.0, 900_000)]
            .iter()
            .map(|(c, m)| row(*c, *m, "ok"))
            .chain([row(50.0, 1, "failed")])
            .collect();
        let e = plan_estimate(&rows).unwrap();
        assert_eq!((e.turns, e.cost, e.secs), (5, Some((1.0, 3.0)), Some((60, 130))));
        let t = estimate_text(&cat, &e);
        for want in ["1.00 to 3.00", "1 min 0 s to 2 min 10 s", "5"] {
            assert!(t.contains(want), "{want} in {t}");
        }
        let few = plan_estimate(&[row(2.0, 5000, "ok"), row(2.0, 5000, "limit")]).unwrap();
        assert!(!estimate_text(&cat, &few).contains(" to "), "equal ends read as one value");
    }

    #[test]
    fn turns_come_from_the_assistant_log() {
        let m = Model::open(":memory:").unwrap();
        let mk = |event: &str, tin: u64, cost: Option<f64>| ddc_model::AuditEntry {
            actor: "user".into(),
            event: event.into(),
            model: Some("x/y".into()),
            tok_in: tin,
            tok_out: 2,
            cost,
            status: "ok".into(),
            plan: (tin == 50).then(|| "ms=1500".to_string()),
            ..ddc_model::AuditEntry::default()
        };
        m.audit_append(&mk("turn", 50, Some(0.5))).unwrap();
        m.audit_append(&mk("plan", 0, None)).unwrap();
        m.audit_append(&mk("turn", 70, None)).unwrap();
        let rows = turn_rows(&m);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].tok_in, rows[0].cost, rows[1].tok_in), (50, Some(0.5), 70));
        assert_eq!(totals(&rows), (124, Some(0.5)));
        assert_eq!((rows[0].ms, rows[1].ms), (Some(1500), None), "the length rides with the turn");
    }

    #[test]
    fn every_key_exists_in_en_and_ru() {
        for k in [
            "ai.cost.show.off",
            "ai.cost.show.brief",
            "ai.cost.show.detailed",
            "ai.cost.charged",
            "ai.cost.tokens_split",
            "ai.cost.step_no",
            "ai.cost.file",
            "ai.cost.export_project",
            "ai.cost.show",
            "ai.cost.show_tip",
            "ai.cost.costliest",
            "ai.cost.plan_tokens",
            "ai.cost.plan_cost",
            "ai.cost.plan_time",
            "ai.cost.plan_elements",
            "ai.cost.plan_each",
            "ai.cost.range",
            "ai.cost.estimate",
        ] {
            for l in [Lang::En, Lang::Ru] {
                assert_ne!(crate::tr_in(l, k, &[]), k, "{k}");
            }
        }
    }
}
