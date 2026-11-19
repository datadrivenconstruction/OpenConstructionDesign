// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Information requirements between the workbook and the model: the words of a book in both
//! languages, rows to stored rules, refusals as text, and the report as lines and sheets. The
//! panel and the assistant's tools share it, so both read a book and count a report alike.

use ddc_io::requirements::{
    read_requirements, BookError, RCell, ReadBook, ReportSheet, RowError, TemplateText, Vocab,
};
use ddc_model::checks::{
    CatReport, ReqField, ReqLine, ReqReport, RuleDef, ValueRule, REQ_CATS,
};
use ddc_model::{ElKey, Model};
use ddc_ui::{Catalog, Lang};
use std::path::Path;

const KINDS: [&str; 5] = ["non_empty", "one_of", "range", "regex", "unit"];
/// Problems named after a category's count before the line ends in "...".
const LINE_PARTS: usize = 2;

/// Why a book did not load; `Rows` lists every refused cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    NoSheet,
    Read(String),
    Empty,
    Rows(Vec<RowError>),
}

fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(',').map(str::trim).filter(|w| !w.is_empty()).map(str::to_lowercase)
}

/// Captions and cell words of both languages: a book filled in either one reads.
pub fn vocab(cat: &Catalog) -> Vocab {
    let mut v = Vocab::default();
    let add = |w: &mut Vec<(String, String)>, text: String, to: &str| {
        for x in words(&text) {
            if !w.iter().any(|(a, _)| *a == x) {
                w.push((x, to.to_string()));
            }
        }
    };
    // every UI language: the template is written in the UI language and must read back in it;
    // en and ru come first, so their words win a clash
    for &lang in Lang::ALL {
        for f in ReqField::ALL {
            v.headers.push((f, cat.t_in(lang, &format!("req.col.{}", f.key()))));
        }
        for &c in REQ_CATS {
            add(&mut v.words, cat.t_in(lang, &format!("req.cat.{c}")), c);
            add(&mut v.words, cat.t_in(lang, &format!("req.syn.cat.{c}")), c);
        }
        for k in KINDS {
            add(&mut v.words, cat.t_in(lang, &format!("req.kind.{k}")), k);
            add(&mut v.words, cat.t_in(lang, &format!("req.syn.kind.{k}")), k);
        }
        for (yn, to) in [("yes", "yes"), ("no", "no")] {
            add(&mut v.words, cat.t_in(lang, &format!("req.yn.{yn}")), to);
            add(&mut v.words, cat.t_in(lang, &format!("req.syn.{yn}")), to);
        }
        v.sheets.push(cat.t_in(lang, "req.sheet.req"));
        v.skip.push(cat.t_in(lang, "req.sheet.example"));
        v.skip.push(cat.t_in(lang, "req.sheet.help"));
    }
    v
}

/// The template in the catalog's language.
pub fn template_text(cat: &Catalog) -> TemplateText {
    let headers = ReqField::ALL
        .iter()
        .map(|f| cat.t(&format!("req.col.{}", f.key())))
        .collect();
    let example = (1..=6)
        .map(|i| {
            cat.t(&format!("req.example.{i}"))
                .split('|')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .map(|mut r| {
            r.resize(ReqField::ALL.len(), String::new());
            r
        })
        .collect();
    let help = (1..=14).map(|i| cat.t(&format!("req.help.{i}"))).collect();
    TemplateText {
        sheet_req: cat.t("req.sheet.req"),
        sheet_example: cat.t("req.sheet.example"),
        sheet_help: cat.t("req.sheet.help"),
        headers,
        example,
        help,
        kinds: KINDS.iter().map(|k| cat.t(&format!("req.kind.{k}"))).collect(),
        yes_no: vec![cat.t("req.yn.yes"), cat.t("req.yn.no")],
        cats: REQ_CATS.iter().map(|c| cat.t(&format!("req.cat.{c}"))).collect(),
    }
}

/// Rows to rules of this model. All or nothing: one refused cell refuses the book, so the
/// stored requirements never silently miss a row the person wrote.
pub fn rules_from_book(m: &Model, b: &ReadBook) -> Result<Vec<RuleDef>, Refusal> {
    let mut errors = b.errors.clone();
    let mut rules = vec![];
    for (i, row) in b.rows.iter().enumerate() {
        match m.requirement_rule(i + 1, &row.origin(), &row.req) {
            Ok(r) => rules.push(r),
            Err(e) => errors.push(b.error_at(row, e.field, e.code, &e.arg)),
        }
    }
    if !errors.is_empty() {
        errors.sort_by(|x, y| (&x.sheet, x.row).cmp(&(&y.sheet, y.row)));
        return Err(Refusal::Rows(errors));
    }
    if rules.is_empty() {
        return Err(Refusal::Empty);
    }
    Ok(rules)
}

/// Reads the book at `path` into rules of `m`; the sheet it read comes back with them.
pub fn load_book(
    m: &Model,
    cat: &Catalog,
    path: &Path,
    sheet: Option<&str>,
) -> Result<(Vec<RuleDef>, String), Refusal> {
    let b = read_requirements(path, &vocab(cat), sheet).map_err(|e| match e {
        BookError::NoSheet => Refusal::NoSheet,
        BookError::Read(s) => Refusal::Read(s),
    })?;
    let rules = rules_from_book(m, &b)?;
    Ok((rules, b.sheet))
}

/// `Requirements, row 6, column D: ...` in the catalog's language.
pub fn error_text(cat: &Catalog, e: &RowError) -> String {
    let row = e.row.to_string();
    let at = if e.col.is_empty() {
        cat.tf("req.err.at_row", &[("sheet", &e.sheet), ("row", &row)])
    } else {
        cat.tf("req.err.at", &[("sheet", &e.sheet), ("row", &row), ("col", &e.col)])
    };
    let field = cat.t(&format!("req.col.{}", e.field.key()));
    let why = cat.tf(e.code, &[("value", &e.arg), ("field", &field)]);
    format!("{at}: {why}")
}

pub fn refusal_text(cat: &Catalog, r: &Refusal) -> String {
    match r {
        Refusal::NoSheet => cat.t("req.err.no_sheet"),
        Refusal::Read(s) => cat.tf("req.err.read", &[("why", s)]),
        Refusal::Empty => cat.t("req.err.empty"),
        Refusal::Rows(v) => cat.tf("req.err.rows", &[("n", &v.len().to_string())]),
    }
}

pub fn cat_name(cat: &Catalog, c: &str) -> String {
    cat.t(&format!("req.cat.{c}"))
}

/// What a value must be, in words: `one of EI30; EI60`.
pub fn rule_text(cat: &Catalog, r: &ValueRule) -> String {
    cat.tf(&format!("req.rule.{}", r.kind()), &[("v", &r.text())])
}

/// One requirement: `4 missing FireRating`, `2 with Width not in 0.9..2.4`, or all passing.
pub fn line_text(cat: &Catalog, l: &ReqLine) -> String {
    if !l.ran {
        return cat.tf("req.line.not_run", &[("param", &l.param)]);
    }
    let mut parts = vec![];
    if l.missing() > 0 {
        let n = l.missing().to_string();
        parts.push(cat.tf("req.line.missing", &[("n", &n), ("param", &l.param)]));
    }
    if l.invalid() > 0 {
        let n = l.invalid().to_string();
        let rule = rule_text(cat, &l.rule);
        parts.push(cat.tf(
            "req.line.invalid",
            &[("n", &n), ("param", &l.param), ("rule", &rule)],
        ));
    }
    if parts.is_empty() {
        let n = l.checked.to_string();
        return cat.tf("req.line.ok", &[("param", &l.param), ("n", &n)]);
    }
    parts.join(", ")
}

/// `Windows: 198/202 compliant, 4 missing FireRating`.
pub fn cat_line(cat: &Catalog, c: &CatReport) -> String {
    let head = cat.tf(
        "req.cat_line",
        &[
            ("cat", &cat_name(cat, &c.cat)),
            ("ok", &c.compliant.to_string()),
            ("total", &c.total.to_string()),
        ],
    );
    let bad: Vec<String> = c
        .lines
        .iter()
        .filter(|l| !l.hits.is_empty() || !l.ran)
        .map(|l| line_text(cat, l))
        .collect();
    match bad.len() {
        0 => head,
        n if n <= LINE_PARTS => format!("{head}, {}", bad.join(", ")),
        _ => format!("{head}, {}, \u{2026}", bad[..LINE_PARTS].join(", ")),
    }
}

pub fn el_ref(k: &ElKey) -> String {
    format!("{}:{}", k.0, k.1)
}

/// Failing elements no automatic fix covers: each once, with what is wrong.
pub fn manual(rep: &ReqReport) -> Vec<(ElKey, String)> {
    let mut out: Vec<(ElKey, String)> = vec![];
    for l in rep.cats.iter().flat_map(|c| c.lines.iter()) {
        for h in l.hits.iter().filter(|h| !h.fixable) {
            match out.iter_mut().find(|(k, _)| *k == h.el) {
                Some((_, p)) => {
                    p.push_str(", ");
                    p.push_str(&l.param);
                }
                None => out.push((h.el.clone(), l.param.clone())),
            }
        }
    }
    out
}

fn txt(s: impl Into<String>) -> RCell {
    RCell::Text(s.into())
}

fn num(n: usize) -> RCell {
    RCell::Num(n as f64)
}

/// The report as a workbook: a summary, then one sheet per category with every failing
/// element and requirement, each with where its row came from.
pub fn report_sheets(cat: &Catalog, rep: &ReqReport) -> Vec<ReportSheet> {
    let h = |keys: &[&str]| -> Vec<String> {
        keys.iter().map(|k| cat.t(&format!("req.x.{k}"))).collect()
    };
    let line_cols = ["element", "param", "rule", "required", "value", "problem", "fix", "source"];
    let mut sum = ReportSheet {
        name: cat.t("req.x.summary"),
        note: Some(cat.t("req.basis")),
        headers: h(&["cat", "total", "compliant", "failing", "reqs", "fixable"]),
        rows: vec![],
    };
    let mut sheets = vec![];
    for c in &rep.cats {
        let fixable: usize = c
            .lines
            .iter()
            .map(|l| l.hits.iter().filter(|x| x.fixable).count())
            .sum();
        sum.rows.push(vec![
            txt(cat_name(cat, &c.cat)),
            num(c.total),
            num(c.compliant),
            num(c.total - c.compliant),
            num(c.lines.len()),
            num(fixable),
        ]);
        let mut s = ReportSheet {
            name: cat_name(cat, &c.cat),
            note: Some(cat_line(cat, c)),
            headers: h(&line_cols),
            rows: vec![],
        };
        for l in &c.lines {
            let rule = rule_text(cat, &l.rule);
            let required = cat.t(if l.required { "req.yn.yes" } else { "req.yn.no" });
            let source = l.origin.clone().unwrap_or_default();
            for x in &l.hits {
                let problem = cat.t(if x.missing { "req.x.missing" } else { "req.x.invalid" });
                let fix = cat.t(if x.fixable { "req.x.auto" } else { "req.x.manual" });
                s.rows.push(vec![
                    txt(el_ref(&x.el)),
                    txt(&l.param),
                    txt(&rule),
                    txt(&required),
                    txt(x.value.clone().unwrap_or_default()),
                    txt(problem),
                    txt(fix),
                    txt(&source),
                ]);
            }
        }
        sheets.push(s);
    }
    sum.rows.push(vec![
        txt(cat.t("req.x.all")),
        num(rep.total()),
        num(rep.compliant()),
        num(rep.total() - rep.compliant()),
        num(rep.requirements()),
        num(rep.fixable_split().0),
    ]);
    let mut out = vec![sum];
    out.extend(sheets);
    out
}

#[cfg(test)]
#[path = "reqbook_tests.rs"]
mod tests;
