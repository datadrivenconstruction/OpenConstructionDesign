// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Component parameter editor: a table of Parameter · Type · Type/Instance ·
//! Value · Formula, plus a "Type sizes" tab (columns are types). Formulas are checked by the same
//! expression engine that builds families; an error shows a red cell, a cycle blocks OK.
use crate::ui_kit as kit;
use crate::units::{display_length, fmt_fixed, Units};
use ddc_geom::component::expr::{self, Expr, Op, ANGLE, AREA, LENGTH, NUMBER, Q, VOLUME};
use ddc_geom::component::{Family, Kind, Value};
use ddc_ui::theme::Role;
use ddc_ui::widgets::{self, formula::Check};
use ddc_ui::Theme;
use egui::{self, RichText};
use std::collections::HashMap;
use crate::ui_kit::KitScroll;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub kind: Kind,
    pub instance: bool,
    /// Default value as text (`1600mm`, `"Oak"`); unused when a formula is set.
    pub value: String,
    /// Formula without `=`; empty means the value is set directly.
    pub formula: String,
}

/// Family draft: edited in the window, not written to the model until OK.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub family: String,
    pub rows: Vec<Row>,
    /// Type sizes: name and type-parameter values (per `rows`, as text; empty means default).
    pub types: Vec<(String, Vec<String>)>,
    pub tab: usize,
}

const KINDS: [Kind; 9] = [
    Kind::Length,
    Kind::Angle,
    Kind::Area,
    Kind::Volume,
    Kind::Number,
    Kind::Int,
    Kind::Bool,
    Kind::Material,
    Kind::Text,
];

pub fn kind_label(k: Kind) -> &'static str {
    kind_label_l(crate::ui_lang_now(), k)
}

pub(super) fn kind_label_l(ru: bool, k: Kind) -> &'static str {
    crate::tr_str(
        ru,
        match k {
            Kind::Length => "pe.kind_len",
            Kind::Angle => "pe.kind_angle",
            Kind::Area => "pe.kind_area",
            Kind::Volume => "pe.kind_volume",
            Kind::Number => "pe.kind_number",
            Kind::Int => "pe.kind_int",
            Kind::Bool => "pe.kind_bool",
            Kind::Material => "pe.kind_material",
            Kind::Text => "pe.kind_text",
            Kind::FamilyType => "pe.kind_family_type",
        },
    )
}

/// Value text that `expr::parse` reads back (lengths in mm).
pub fn value_text(kind: Kind, v: &Value) -> String {
    match v {
        Value::Text(t) => t.clone(),
        Value::Num(x) => num_literal(expr::q(*x, kind.dim())),
    }
}

fn trim(v: f64) -> String {
    let s = format!("{:.6}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

fn num_literal(q: Q) -> String {
    match (q.d.len, q.d.ang) {
        (1, 0) => format!("{}mm", trim(q.v * 1000.0)),
        (0, 1) => format!("{}deg", trim(q.v.to_degrees())),
        (0, 0) => trim(q.v),
        _ => expr::literal(q),
    }
}

fn prec(op: Op) -> u8 {
    match op {
        Op::Or => 1,
        Op::And => 2,
        Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne => 3,
        Op::Add | Op::Sub => 4,
        Op::Mul | Op::Div => 5,
    }
}

/// Source formula text from the tree (parentheses only where needed).
pub fn expr_text(e: &Expr) -> String {
    fn go(e: &Expr, parent: u8, right: bool, out: &mut String) {
        match e {
            Expr::Num(q) => out.push_str(&num_literal(*q)),
            Expr::Var(n) => out.push_str(n),
            Expr::Neg(a) => {
                out.push('-');
                go(a, 6, false, out);
            }
            Expr::Not(a) => {
                out.push_str("not ");
                go(a, 6, false, out);
            }
            Expr::Call(f, args) => {
                out.push_str(f);
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(a, 0, false, out);
                }
                out.push(')');
            }
            Expr::Bin(op, a, b) => {
                let p = prec(*op);
                let paren = p < parent || (right && p == parent);
                if paren {
                    out.push('(');
                }
                go(a, p, false, out);
                out.push_str(match op {
                    Op::Add => " + ",
                    Op::Sub => " - ",
                    Op::Mul => " * ",
                    Op::Div => " / ",
                    Op::Lt => " < ",
                    Op::Le => " <= ",
                    Op::Gt => " > ",
                    Op::Ge => " >= ",
                    Op::Eq => " == ",
                    Op::Ne => " != ",
                    Op::And => " and ",
                    Op::Or => " or ",
                });
                go(b, p, true, out);
                if paren {
                    out.push(')');
                }
            }
        }
    }
    let mut s = String::new();
    go(e, 0, false, &mut s);
    s
}

impl Draft {
    /// Type-size values for the model: an empty cell means default (not written).
    pub fn type_values(&self) -> Result<Vec<(String, Vec<(String, Value)>)>, String> {
        self.types
            .iter()
            .map(|(name, cells)| {
                let vals = cells
                    .iter()
                    .zip(&self.rows)
                    .filter(|(c, _)| !c.trim().is_empty())
                    .map(|(c, r)| {
                        let v = if r.kind.is_text() {
                            Value::Text(c.trim().to_string())
                        } else {
                            let q = expr::parse(c.trim())
                                .map_err(|e| e.msg)
                                .and_then(|e| e.eval(&|_| None))
                                .map_err(|m| format!("{name}.{}: {m}", r.name))?;
                            Value::Num(q.v)
                        };
                        Ok((r.name.clone(), v))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok((name.clone(), vals))
            })
            .collect()
    }

    pub fn from_family(f: &Family) -> Self {
        let rows: Vec<Row> = f
            .params
            .iter()
            .map(|p| Row {
                name: p.name.clone(),
                kind: p.kind,
                instance: p.instance,
                value: value_text(p.kind, &p.default),
                formula: p.formula.as_ref().map(expr_text).unwrap_or_default(),
            })
            .collect();
        let types = f
            .types
            .iter()
            .map(|t| {
                let vals = rows
                    .iter()
                    .map(|r| {
                        t.values
                            .iter()
                            .find(|(n, _)| *n == r.name)
                            .map(|(_, v)| value_text(r.kind, v))
                            .unwrap_or_default()
                    })
                    .collect();
                (t.name.clone(), vals)
            })
            .collect();
        Draft {
            family: f.name.clone(),
            rows,
            types,
            tab: 0,
        }
    }
}

fn dim_words(ru: bool, s: &str) -> String {
    let w = |k| crate::tr_lang(ru, k, &[]);
    s.replace("length", &w("pe.dim_length"))
        .replace("number", &w("pe.dim_number"))
        .replace("angle", &w("pe.dim_angle"))
        .replace("area", &w("pe.dim_area"))
        .replace("volume", &w("pe.dim_volume"))
}

/// Expression-engine message in the UI language via catalog keys (unknown forms pass through).
pub fn msg(m: &str) -> String {
    msg_l(crate::ui_lang_now(), m)
}

fn msg_l(ru: bool, m: &str) -> String {
    let t = |k, args: &[(&str, &str)]| crate::tr_lang(ru, k, args);
    if let Some(n) = m.strip_prefix("unknown name ") {
        return t("pe.msg_unknown_name", &[("n", n.trim_matches('\''))]);
    }
    if let Some(rest) = m.strip_prefix("expected ") {
        if let Some((a, b)) = rest.split_once(", got ") {
            return t(
                "pe.msg_expected_got",
                &[("a", dim_words(ru, a).as_str()), ("b", dim_words(ru, b).as_str())],
            );
        }
        return t("pe.msg_expected", &[("rest", rest)]);
    }
    if let Some(f) = m.strip_prefix("unknown function ") {
        return t("pe.msg_unknown_function", &[("f", f)]);
    }
    match m {
        "unexpected end" => return t("pe.msg_unexpected_end", &[]),
        "division by zero" => return t("pe.msg_div_zero", &[]),
        "unexpected text after expression" => return t("pe.msg_extra_text", &[]),
        "result is not finite" => return t("pe.msg_not_finite", &[]),
        _ => {}
    }
    if let Some((what, dims)) = m.split_once(": ") {
        if dims.contains(" and ") {
            let w = t(
                match what {
                    "cannot add or subtract" => "pe.msg_what_add",
                    "cannot compare" => "pe.msg_what_compare",
                    "if() branches differ" => "pe.msg_what_branches",
                    _ => "pe.msg_what_args",
                },
                &[],
            );
            let dims = dim_words(ru, dims).replace(" and ", &t("pe.msg_and", &[]));
            return t("pe.msg_incompatible", &[("w", w.as_str()), ("dims", dims.as_str())]);
        }
    }
    m.to_string()
}

/// Draft check result: per row — formula, value; cycle (names in order).
#[derive(Clone, Debug, Default)]
pub struct Eval {
    pub formula: Vec<Check>,
    pub value: Vec<Check>,
    pub nums: HashMap<String, Q>,
    pub cycle: Option<Vec<String>>,
}

impl Eval {
    /// OK is blocked: there is an error or a cycle.
    pub fn blocked(&self) -> bool {
        self.cycle.is_some() || self.formula.iter().chain(&self.value).any(Check::is_err)
    }
}

fn char_to_byte(s: &str, c: usize) -> usize {
    s.char_indices().nth(c).map_or(s.len(), |(b, _)| b)
}

fn preview(q: Q, kind: Kind, u: Units, ru: bool) -> String {
    match kind {
        Kind::Bool => crate::tr_str(ru, if q.v != 0.0 { "pe.yes" } else { "pe.no" }).into(),
        Kind::Int => format!("{}", q.v.round()),
        _ => match q.d {
            LENGTH => display_length(q.v, u),
            ANGLE => format!("{}°", trim(q.v.to_degrees())),
            AREA => crate::unit_text::area_ui(q.v),
            VOLUME => crate::unit_text::vol_ui(q.v),
            _ => fmt_fixed(q.v, 3),
        },
    }
}

/// First formula dependency cycle found (names, the first repeated at the end).
pub fn find_cycle(deps: &HashMap<String, Vec<String>>, order: &[String]) -> Option<Vec<String>> {
    fn dfs(
        n: &str,
        deps: &HashMap<String, Vec<String>>,
        state: &mut HashMap<String, u8>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        state.insert(n.into(), 1);
        path.push(n.into());
        for d in deps.get(n).into_iter().flatten() {
            match state.get(d.as_str()) {
                Some(1) => {
                    let i = path.iter().position(|p| p == d).unwrap_or(0);
                    let mut c = path[i..].to_vec();
                    c.push(d.clone());
                    return Some(c);
                }
                Some(_) => {}
                None => {
                    if let Some(c) = dfs(d, deps, state, path) {
                        return Some(c);
                    }
                }
            }
        }
        path.pop();
        state.insert(n.into(), 2);
        None
    }
    let mut state = HashMap::new();
    for n in order {
        if !state.contains_key(n.as_str()) {
            if let Some(c) = dfs(n, deps, &mut state, &mut vec![]) {
                return Some(c);
            }
        }
    }
    None
}

/// Checks all rows: parsing, cycles, evaluation in dependency order, type conformance.
pub fn evaluate(rows: &[Row], u: Units) -> Eval {
    evaluate_l(crate::ui_lang_now(), rows, u)
}

fn evaluate_l(ru: bool, rows: &[Row], u: Units) -> Eval {
    let tr = |m: &str| msg_l(ru, m);
    let n = rows.len();
    let mut ev = Eval {
        formula: vec![Check::Empty; n],
        value: vec![Check::Empty; n],
        ..Default::default()
    };
    let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
    let mut parsed: Vec<Option<Expr>> = vec![None; n];
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.formula.trim().is_empty() {
            if r.kind.is_text() {
                continue;
            }
            // the value is a constant: no parameter references
            match expr::parse(r.value.trim())
                .map_err(|e| (Some(e.pos), e.msg))
                .and_then(|e| e.eval(&|_| None).map_err(|m| (None, m)))
            {
                Ok(q) if expr::zero_fits(q, r.kind.dim()) => {
                    ev.nums.insert(r.name.clone(), expr::q(q.v, r.kind.dim()));
                }
                Ok(q) => {
                    let m = format!("expected {}, got {}", r.kind.name(), q.d);
                    ev.value[i] = Check::Err {
                        pos: None,
                        msg: tr(&m),
                    };
                }
                Err((p, m)) => {
                    ev.value[i] = Check::Err {
                        pos: p.map(|p| char_to_byte(&r.value, p)),
                        msg: tr(&m),
                    }
                }
            }
            continue;
        }
        if r.kind.is_text() {
            ev.formula[i] = Check::Err {
                pos: None,
                msg: crate::tr_lang(ru, "pe.text_no_formula", &[]),
            };
            continue;
        }
        match expr::parse(r.formula.trim()) {
            Ok(e) => {
                let mut v = vec![];
                e.vars(&mut v);
                v.retain(|x| names.contains(x));
                deps.insert(r.name.clone(), v);
                parsed[i] = Some(e);
            }
            Err(e) => {
                let lead = r.formula.len() - r.formula.trim_start().len();
                ev.formula[i] = Check::Err {
                    pos: Some(lead + char_to_byte(r.formula.trim(), e.pos)),
                    msg: tr(&e.msg),
                };
            }
        }
    }
    if let Some(c) = find_cycle(&deps, &names) {
        let text = c.join(" → ");
        for (i, r) in rows.iter().enumerate() {
            if c.contains(&r.name) && parsed[i].is_some() {
                ev.formula[i] = Check::Err {
                    pos: None,
                    msg: crate::tr_lang(ru, "pe.circular", &[("text", text.as_str())]),
                };
                parsed[i] = None;
            }
        }
        ev.cycle = Some(c);
    }
    // evaluation in waves: a row is ready once all its references are computed or failed
    let mut done: Vec<bool> = parsed.iter().map(|p| p.is_none()).collect();
    loop {
        let mut progress = false;
        for i in 0..n {
            if done[i] {
                continue;
            }
            let ds = deps.get(&rows[i].name).cloned().unwrap_or_default();
            let blocked = ds.iter().find(|d| {
                let j = names.iter().position(|x| x == *d).unwrap_or(0);
                ev.formula[j].is_err() || ev.value[j].is_err()
            });
            if let Some(b) = blocked {
                ev.formula[i] = Check::Err {
                    pos: None,
                    msg: crate::tr_lang(ru, "pe.depends_invalid", &[("b", b.as_str())]),
                };
                done[i] = true;
                progress = true;
                continue;
            }
            if !ds.iter().all(|d| ev.nums.contains_key(d)) {
                continue;
            }
            let e = parsed[i].as_ref().expect("parsed");
            let r = &rows[i];
            ev.formula[i] = match e.eval(&|s| ev.nums.get(s).copied()) {
                Ok(q) if expr::zero_fits(q, r.kind.dim()) => {
                    let q = expr::q(q.v, r.kind.dim());
                    ev.nums.insert(r.name.clone(), q);
                    Check::Ok(preview(q, r.kind, u, ru))
                }
                Ok(q) => Check::Err {
                    pos: None,
                    msg: tr(&format!("expected {}, got {}", r.kind.name(), q.d)),
                },
                Err(m) => Check::Err {
                    pos: None,
                    msg: tr(&m),
                },
            };
            done[i] = true;
            progress = true;
        }
        if !progress {
            break;
        }
    }
    let _ = NUMBER;
    ev
}

/// Type-size value cell: empty means default, otherwise a constant of the required type.
pub fn check_value(kind: Kind, text: &str) -> Check {
    check_value_l(crate::ui_lang_now(), kind, text)
}

fn check_value_l(ru: bool, kind: Kind, text: &str) -> Check {
    if text.trim().is_empty() || kind.is_text() {
        return Check::Empty;
    }
    match expr::parse(text.trim())
        .map_err(|e| e.msg)
        .and_then(|e| e.eval(&|_| None))
    {
        Ok(q) if expr::zero_fits(q, kind.dim()) => Check::Ok(String::new()),
        Ok(q) => Check::Err {
            pos: None,
            msg: msg_l(ru, &format!("expected {}, got {}", kind.name(), q.d)),
        },
        Err(m) => Check::Err {
            pos: None,
            msg: msg_l(ru, &m),
        },
    }
}

pub enum Outcome {
    Ok,
    Cancel,
}

fn t(key: &'static str) -> &'static str {
    crate::tr_str(crate::ui_lang_now(), key)
}

/// Editor window. `None` means the window stays open.
pub fn window(
    ctx: &egui::Context,
    th: &Theme,
    u: Units,
    d: &mut Draft,
) -> Option<Outcome> {
    let ev = evaluate(&d.rows, u);
    let mut out = None;
    let mut open = true;
    let title = format!(
        "{} — {}",
        t("pe.family_parameters"),
        d.family
    );
    kit::Dialog::new("param_editor", title, kit::DialogSize::Workspace)
        .size([820.0, 460.0])
        .min_size([820.0, 300.0])
        .open(&mut open)
        .show(ctx, |ui| {
            if let Some(i) = widgets::segmented(
                ui,
                th,
                &[
                    t("pe.parameters"),
                    t("pe.types"),
                ],
                d.tab,
            ) {
                d.tab = i;
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(ui.available_height() - 64.0)
                .kit_show(ui, |ui| {
                    if d.tab == 0 {
                        params_table(ui, th, d, &ev);
                    } else {
                        sizes_table(ui, th, d);
                    }
                });
            ui.add_space(6.0);
            if let Some(c) = &ev.cycle {
                ui.label(
                    RichText::new(format!(
                        "⚠ {}: {}",
                        t("pe.circular_reference"),
                        c.join(" → ")
                    ))
                    .color(th.c("error")),
                );
            }
            let bad_sizes = d.types.iter().any(|(_, vals)| {
                vals.iter()
                    .zip(&d.rows)
                    .any(|(v, r)| check_value(r.kind, v).is_err())
            });
            let blocked = ev.blocked() || bad_sizes;
            kit::dialog_footer(ui, |ui| {
                let why = t("pe.fix_formula_errors_first");
                if kit::primary_action_table(ui, "OK", blocked.then_some(why)) {
                    out = Some(Outcome::Ok);
                }
                if kit::cancel_button(ui, t("pe.cancel")).clicked() {
                    out = Some(Outcome::Cancel);
                }
                if ui.button(t("pe.parameter")).clicked() {
                    let k = d.rows.len() + 1;
                    d.rows.push(Row {
                        name: format!("P{k}"),
                        kind: Kind::Length,
                        instance: false,
                        value: "0mm".into(),
                        formula: String::new(),
                    });
                    for (_, v) in &mut d.types {
                        v.push(String::new());
                    }
                }
                ui.label(
                    RichText::new(t("pe.if_c_a_b_min_max_round_parameter_n"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
                );
            });
        });
    if !open {
        out = Some(Outcome::Cancel);
    }
    out
}

fn header(ui: &mut egui::Ui, th: &Theme, cols: &[&str]) {
    for c in cols {
        ui.label(
            RichText::new(*c)
                .font(th.font(Role::Label))
                .color(th.c("text.secondary")),
        );
    }
    ui.end_row();
}

fn params_table(ui: &mut egui::Ui, th: &Theme, d: &mut Draft, ev: &Eval) {
    egui::Grid::new("c.params")
        .num_columns(5)
        .spacing([10.0, 6.0])
        .min_row_height(26.0)
        .show(ui, |ui| {
            header(
                ui,
                th,
                &[
                    t("pe.parameter_2"),
                    t("pe.kind"),
                    t("pe.type_inst"),
                    t("pe.value"),
                    t("pe.formula"),
                ],
            );
            for (i, r) in d.rows.iter_mut().enumerate() {
                // kit fields: one control high, text on the row's centre line
                widgets::TextField::new(&mut r.name).id_salt(("c.name", i))
                    .labelled_by(egui::Id::new("c.params.name"), t("pe.parameter_2"))
                    .width(110.0)
                    .show(ui, th);
                let names: Vec<&str> = KINDS.iter().map(|k| kind_label(*k)).collect();
                let mut sel = KINDS.iter().position(|k| *k == r.kind).unwrap_or(0);
                let w = Some(90.0);
                if ddc_ui::widgets::field::dropdown(ui, th, ("c.kind", i), &mut sel, &names, w)
                    .changed()
                {
                    r.kind = KINDS[sel];
                }
                let lab = if r.instance {
                    t("pe.instance")
                } else {
                    t("pe.type")
                };
                if ui.selectable_label(r.instance, lab).clicked() {
                    r.instance = !r.instance;
                }
                let has_f = !r.formula.trim().is_empty();
                if has_f {
                    let shown = match &ev.formula[i] {
                        Check::Ok(v) => v.clone(),
                        _ => "—".into(),
                    };
                    ui.label(RichText::new(shown).color(th.c("text.disabled")));
                } else {
                    let bad = ev.value[i].is_err();
                    let resp = widgets::TextField::new(&mut r.value)
                        .id_salt(("c.value", i))
                        .labelled_by(egui::Id::new("c.params.value"), t("pe.value"))
                        .width(110.0)
                        .error(bad)
                        .show(ui, th)
                        .response;
                    if let Check::Err { msg, .. } = &ev.value[i] {
                        resp.on_hover_text(msg.clone());
                    }
                }
                widgets::formula::formula(
                    ui,
                    th,
                    ("c.f", i),
                    &mut r.formula,
                    260.0,
                    &ev.formula[i],
                );
                ui.end_row();
            }
        });
}

fn sizes_table(ui: &mut egui::Ui, th: &Theme, d: &mut Draft) {
    let type_rows: Vec<usize> = (0..d.rows.len())
        .filter(|&i| !d.rows[i].instance && d.rows[i].formula.trim().is_empty())
        .collect();
    egui::Grid::new("c.sizes")
        .num_columns(d.types.len() + 1)
        .spacing([10.0, 6.0])
        .min_row_height(26.0)
        .striped(true)
        .show(ui, |ui| {
            ui.label(
                RichText::new(t("pe.parameter_2"))
                    .font(th.font(Role::Label))
                    .color(th.c("text.secondary")),
            );
            for (name, _) in &d.types {
                ui.label(RichText::new(name).font(th.font(Role::BodyStrong)));
            }
            ui.end_row();
            for &i in &type_rows {
                let (name, kind, def) = (
                    d.rows[i].name.clone(),
                    d.rows[i].kind,
                    d.rows[i].value.clone(),
                );
                ui.label(&name);
                for (k, (_, vals)) in d.types.iter_mut().enumerate() {
                    let c = check_value(kind, &vals[i]);
                    let r = widgets::TextField::new(&mut vals[i])
                        .id_salt(("c.sz", i, k))
                        .labelled_by(egui::Id::new(("c.sz.lbl", i)), &name)
                        .hint(&def)
                        .width(96.0)
                        .error(c.is_err())
                        .show(ui, th)
                        .response;
                    if let Check::Err { msg, .. } = c {
                        r.on_hover_text(msg);
                    }
                }
                ui.end_row();
            }
        });
}

/// The editor drawn headless on `tab` (0 parameters, 1 types) with a family of four parameters
/// and two types, in the language of the calling test (`crate::with_test_lang`).
#[cfg(test)]
pub(crate) fn layout_probe(tab: usize) -> crate::layout_contract::Probe {
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let row = |name: &str, kind: Kind, instance: bool, value: &str, formula: &str| Row {
        name: name.into(),
        kind,
        instance,
        value: value.into(),
        formula: formula.into(),
    };
    let rows = vec![
        row("Width", Kind::Length, false, "900mm", ""),
        row("Height", Kind::Length, false, "2100mm", ""),
        row("Frame", Kind::Material, false, "Oak", ""),
        row("Leaf", Kind::Length, true, "", "Width - 100mm"),
    ];
    let cells = |w: &str, h: &str| vec![w.into(), h.into(), String::new(), String::new()];
    let mut d = Draft {
        family: "Door".into(),
        rows,
        types: vec![("D90".into(), cells("900mm", "2100mm")), ("D100".into(), cells("1m", ""))],
        tab,
    };
    crate::layout_contract::probe("param_editor", |ctx| {
        let _ = window(ctx, &th, Units::Metric, &mut d);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Family {
        ddc_model::library()
            .unwrap()
            .into_iter()
            .find(|f| f.name == "Table")
            .unwrap()
    }

    fn row(name: &str, kind: Kind, value: &str, formula: &str) -> Row {
        Row {
            name: name.into(),
            kind,
            instance: false,
            value: value.into(),
            formula: formula.into(),
        }
    }

    #[test]
    fn library_family_round_trips_and_evaluates() {
        let d = Draft::from_family(&table());
        let ev = evaluate_l(true, &d.rows, Units::Metric);
        assert!(!ev.blocked(), "{:?}", ev.formula);
        let legh = d.rows.iter().position(|r| r.name == "LegH").unwrap();
        assert_eq!(d.rows[legh].formula, "Height - Top");
        assert!(
            matches!(&ev.formula[legh], Check::Ok(v) if v.contains("710")),
            "{:?}",
            ev.formula[legh]
        );
        let seats = d.rows.iter().position(|r| r.name == "Seats").unwrap();
        assert_eq!(ev.formula[seats], Check::Ok("4".into()));
        // three metric sizes and one imperial
        assert_eq!(d.types.len(), 4);
        // the formula text is read back by the engine into the same tree
        for p in &table().params {
            if let Some(f) = &p.formula {
                assert_eq!(&expr::parse(&expr_text(f)).unwrap(), f, "{}", p.name);
            }
        }
    }

    #[test]
    fn dimension_error_is_red_and_blocks() {
        let rows = vec![
            row("H", Kind::Length, "750mm", ""),
            row("L", Kind::Length, "", "H + 2"),
            row("M", Kind::Length, "", "L * 2"),
        ];
        let ev = evaluate_l(true, &rows, Units::Metric);
        let Check::Err { msg, .. } = &ev.formula[1] else {
            panic!("{:?}", ev.formula[1])
        };
        assert!(msg.contains(&crate::app2_fx::fx("pe_incompatible_units")), "{msg}");
        assert!(matches!(&ev.formula[2], Check::Err { msg, .. } if msg.contains("«L»")));
        assert!(ev.blocked());
    }

    #[test]
    fn kind_mismatch_unknown_name_and_syntax() {
        let rows = vec![
            row("A", Kind::Angle, "", "10mm"),
            row("B", Kind::Length, "", "Nope * 2"),
            row("C", Kind::Length, "", "(1mm"),
        ];
        let ev = evaluate_l(true, &rows, Units::Metric);
        assert!(matches!(&ev.formula[0], Check::Err { msg, .. } if msg.contains(&crate::app2_fx::fx("pe_expected_angle"))));
        assert!(
            matches!(&ev.formula[1], Check::Err { msg, .. } if msg.contains(&crate::app2_fx::fx("pe_unknown_name")))
        );
        assert!(matches!(&ev.formula[2], Check::Err { pos: Some(_), .. }));
    }

    #[test]
    fn cycle_blocks_ok() {
        let rows = vec![
            row("A", Kind::Length, "", "B + 1mm"),
            row("B", Kind::Length, "", "A"),
            row("C", Kind::Length, "5mm", ""),
        ];
        let ev = evaluate_l(false, &rows, Units::Metric);
        assert_eq!(
            ev.cycle.as_deref(),
            Some(&["A".to_string(), "B".into(), "A".into()][..])
        );
        assert!(ev.formula[0].is_err() && ev.formula[1].is_err() && ev.blocked());
        let ok = evaluate_l(false, &rows[2..], Units::Metric);
        assert!(!ok.blocked());
    }

    #[test]
    fn size_cells_check_kind() {
        assert!(check_value_l(true, Kind::Length, "2").is_err());
        assert!(!check_value_l(true, Kind::Length, "1200mm").is_err());
        assert!(!check_value_l(true, Kind::Length, "").is_err());
    }

    #[test]
    fn param_error_en_no_cyrillic() {
        for m in [
            "unknown name 'X'",
            "expected length, got angle",
            "unknown function f",
            "unexpected end",
            "division by zero",
            "cannot add or subtract: length and number",
            "if() branches differ: area and volume",
        ] {
            let r = msg_l(false, m);
            assert_eq!(r, m, "{r}");
            assert!(!r.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), "{r}");
        }
        let ev = evaluate_l(false, &[row("T", Kind::Text, "", "1")], Units::Metric);
        assert!(
            matches!(&ev.formula[0], Check::Err { msg, .. } if msg == "text parameters take no formula")
        );
    }

    #[test]
    fn param_editor_preview_bool_from_catalog() {
        let q = |v| Q { v, d: NUMBER };
        for ru in [false, true] {
            let yes = crate::tr_str(ru, "pe.yes");
            let no = crate::tr_str(ru, "pe.no");
            assert_ne!(yes, "pe.yes");
            assert_ne!(yes, no);
            assert_eq!(preview(q(1.0), Kind::Bool, Units::Metric, ru), yes);
            assert_eq!(preview(q(0.0), Kind::Bool, Units::Metric, ru), no);
        }
    }

    #[test]
    fn kind_label_follows_ui_lang() {
        for k in KINDS {
            let (en, ru) = (kind_label_l(false, k), kind_label_l(true, k));
            assert!(!en.starts_with("pe."), "{en}");
            assert!(!ru.starts_with("pe."), "{ru}");
            assert_ne!(en, ru);
        }
    }

    #[test]
    fn param_error_ru_unchanged_text() {
        assert_eq!(msg_l(true, "unknown name 'X'"), crate::app2_fx::fx("pe_unknown_x"));
        assert_eq!(
            msg_l(true, "expected length, got angle"),
            crate::app2_fx::fx("pe_expected_length")
        );
        assert_eq!(msg_l(true, "division by zero"), crate::app2_fx::fx("pe_div_zero"));
        assert_eq!(
            msg_l(true, "cannot add or subtract: length and number"),
            crate::app2_fx::fx("pe_add_units")
        );
        assert_eq!(msg_l(true, "weird"), "weird");
    }

    #[test]
    fn qa13b_keys_both_languages() {
        let en: serde_json::Value =
            serde_json::from_str(include_str!("../../../ui/data/i18n/en/param_editor.json")).unwrap();
        let ru: serde_json::Value =
            serde_json::from_str(include_str!("../../../ui/data/i18n/ru/param_editor.json")).unwrap();
        let (en, ru) = (en.as_object().unwrap(), ru.as_object().unwrap());
        assert!(!en.is_empty());
        assert!(en.keys().eq(ru.keys()));
        for k in en.keys() {
            let n = |s: &str| s.matches('{').count();
            assert_eq!(n(en[k].as_str().unwrap()), n(ru[k].as_str().unwrap()), "{k}");
        }
    }
}
