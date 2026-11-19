// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stored (imported) properties of the selected bodies in the Properties panel: grouped by set,
//! every row edited in place, a multi-selection shows the keys all elements share and writes them
//! all in one undo step. Edits land in `prop`, so the source diff carries them as `p:Key` fields
//! and the IFC patch writes them back to the file.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ddc_model::source::SqlValue as Sql;
use ddc_model::Model;
use ddc_ui::sem::space;
use ddc_ui::theme::Role;
use ddc_ui::Theme;
use egui::{self, RichText};

use crate::editor::bodies::Prop;
use crate::editor::{Editor, OpFn, Sel};
use crate::shell::prop_rows;
use crate::tr;
use crate::ui_kit;
use crate::units::{self, DecimalMark, NumError, Units, FOOT, INCH};

/// Above this many rows a filter box is shown; every row is still listed.
pub const FILTER_AT: usize = 40;

/// (table, id) of one selected element.
pub type Item = (&'static str, i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Text,
    Int,
    Real,
    /// Text on some elements, numbers on others: each element is written by its own type.
    Mixed,
    /// Binary or otherwise not editable as text.
    Other,
}

/// One property row of the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Full stored key («Set.Name»).
    pub key: String,
    /// Edit text; `None` when the selected elements differ.
    pub value: Option<String>,
    /// Stored unit; `None` when the selected elements store different units (read-only row).
    pub unit: Option<String>,
    /// Unit as shown.
    pub label: String,
    pub ty: Ty,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Refuse {
    Name,
    Number(String),
    /// A comma that is no thousands group where the UI mark is a point (`1,5` in English).
    Comma(String),
    Locked,
    Failed(String),
}

impl Refuse {
    pub fn text(&self) -> String {
        match self {
            Refuse::Name => tr("imported_props.err.name", &[]),
            Refuse::Number(s) => tr("imported_props.err.number", &[("text", s)]),
            Refuse::Comma(s) => tr("decimal_input.err.comma", &[("text", s)]),
            Refuse::Locked => tr("imported_props.err.locked", &[]),
            Refuse::Failed(e) => tr("imported_props.err.failed", &[("error", e)]),
        }
    }
}

/// Selected bodies, each once, in selection order.
pub fn selected(ed: &Editor) -> Vec<Item> {
    let mut out: Vec<Item> = vec![];
    for s in &ed.selection {
        if let Sel::Body(k, id) = s {
            let it = (k.table(), *id);
            if !out.contains(&it) {
                out.push(it);
            }
        }
    }
    out
}

/// Number as edit text in the locale of `mark`: up to 6 decimals, trailing zeros dropped, so
/// the shown text reads back with the same mark.
fn num_text(x: f64, mark: DecimalMark) -> String {
    let (s, dec) = match mark {
        DecimalMark::Comma => (units::fmt_num_ru(x, 6), ','),
        DecimalMark::Point => (units::fmt_num_en(x, 6), '.'),
    };
    if s.contains(dec) {
        s.trim_end_matches('0').trim_end_matches(dec).to_string()
    } else {
        s
    }
}

/// Stored unit as shown next to the value.
fn unit_label(unit: &str, u: Units) -> String {
    match unit {
        "m" => units::unit_label(u).to_string(),
        "m2" => "m\u{b2}".into(),
        "m3" => "m\u{b3}".into(),
        "deg" => "\u{b0}".into(),
        _ => unit.to_string(),
    }
}

/// Value as edit text in display units.
fn value_text(v: &Sql, unit: &str, u: Units, mark: DecimalMark) -> String {
    match v {
        Sql::Text(s) => s.clone(),
        Sql::Integer(i) => i.to_string(),
        Sql::Real(x) if unit == "m" => match u {
            Units::Metric => num_text(x * 1000.0, mark),
            Units::Imperial => units::edit_length_mark(*x, u, mark),
        },
        Sql::Real(x) => num_text(*x, mark),
        Sql::Null | Sql::Blob(_) => String::new(),
    }
}

/// Keys every element has, in the order of the first element; differing values show as `None`.
pub fn common_rows(lists: &[Vec<Prop>], u: Units) -> Vec<Row> {
    common_rows_in(lists, u, units::ui_decimal_mark())
}

/// [`common_rows`] with number text in the locale of `mark`.
pub fn common_rows_in(lists: &[Vec<Prop>], u: Units, mark: DecimalMark) -> Vec<Row> {
    let Some((first, rest)) = lists.split_first() else {
        return vec![];
    };
    let maps: Vec<HashMap<&str, (&Sql, &str)>> = rest
        .iter()
        .map(|l| {
            l.iter()
                .map(|(k, v, unit)| (k.as_str(), (v, unit.as_str())))
                .collect()
        })
        .collect();
    let mut out = vec![];
    'keys: for (k, v, unit) in first {
        let mut all: Vec<(&Sql, &str)> = vec![(v, unit.as_str())];
        for m in &maps {
            match m.get(k.as_str()) {
                Some(e) => all.push(*e),
                None => continue 'keys,
            }
        }
        let has = |f: fn(&Sql) -> bool| all.iter().any(|(v, _)| f(v));
        let (text, int, real) = (
            has(|v| matches!(v, Sql::Text(_))),
            has(|v| matches!(v, Sql::Integer(_))),
            has(|v| matches!(v, Sql::Real(_))),
        );
        let ty = if has(|v| matches!(v, Sql::Blob(_))) {
            Ty::Other
        } else if text && (int || real) {
            Ty::Mixed
        } else if text || !(int || real) {
            Ty::Text
        } else if real {
            Ty::Real
        } else {
            Ty::Int
        };
        let same_unit = all.iter().all(|(_, un)| *un == unit.as_str());
        let shown = value_text(v, unit, u, mark);
        let same_value = all.iter().all(|(w, un)| value_text(w, un, u, mark) == shown);
        out.push(Row {
            key: k.clone(),
            value: same_value.then_some(shown),
            unit: same_unit.then(|| unit.clone()),
            label: if same_unit {
                unit_label(unit, u)
            } else {
                String::new()
            },
            ty,
        });
    }
    out
}

/// Rows of the selected elements, read from the model.
pub fn rows_of(m: &Model, items: &[Item], u: Units) -> Vec<Row> {
    rows_of_in(m, items, u, units::ui_decimal_mark())
}

/// [`rows_of`] with number text in the locale of `mark`.
pub fn rows_of_in(m: &Model, items: &[Item], u: Units, mark: DecimalMark) -> Vec<Row> {
    let lists: Vec<Vec<Prop>> = items
        .iter()
        .map(|(k, id)| m.props(k, *id).unwrap_or_default())
        .collect();
    common_rows_in(&lists, u, mark)
}

/// Indices of the rows whose key or value contains `q` (any case); all rows for an empty `q`.
pub fn filter_rows(rows: &[Row], q: &str) -> Vec<usize> {
    let q = q.trim().to_lowercase();
    (0..rows.len())
        .filter(|&i| {
            q.is_empty()
                || rows[i].key.to_lowercase().contains(&q)
                || rows[i]
                    .value
                    .as_deref()
                    .is_some_and(|v| v.to_lowercase().contains(&q))
        })
        .collect()
}

/// Number typed by the user, read in the locale of `mark` (`1,250` is 1250 with a point mark and
/// 1.25 with a comma mark); finite values only.
fn plain_number(s: &str, mark: DecimalMark) -> Result<f64, NumError> {
    let c = ddc_ui::numparse::canonical(s, mark)?;
    ddc_model::parse_invariant(&c).ok_or(NumError::NotNumber)
}

/// Refusal of the typed `text` for a number error.
fn refuse_number(e: NumError, text: &str) -> Refuse {
    let t = text.trim().to_string();
    match e {
        NumError::CommaDecimal => Refuse::Comma(t),
        NumError::Empty | NumError::NotNumber => Refuse::Number(t),
    }
}

/// Length typed in display units (bare number: mm or inches) or with its own unit; metres.
fn parse_len(s: &str, u: Units, mark: DecimalMark) -> Result<f64, NumError> {
    let t = s.trim();
    if t.contains(['\'', '"', '/']) {
        let sc = crate::expr_field::Scope::none();
        return units::parse_edit_length_lang(t, u, &sc, mark).map_err(|e| match e {
            units::InputError::CommaDecimal => NumError::CommaDecimal,
            _ => NumError::NotNumber,
        });
    }
    let split = t
        .char_indices()
        .find(|&(_, c)| {
            !(c.is_ascii_digit()
                || matches!(c, '+' | '-' | '.' | ',' | 'e' | 'E' | ' ' | '\u{a0}' | '\u{202f}'))
        })
        .map_or(t.len(), |(i, _)| i);
    let (num, suffix) = (&t[..split], t[split..].trim());
    let x = plain_number(num, mark)?;
    let metric_mm = units::unit_label(Units::Metric);
    // divided, not multiplied: 2800 * 0.001 would store 2.8000000000000003
    Ok(match suffix {
        "" if u == Units::Metric => x / 1000.0,
        "mm" => x / 1000.0,
        s if s == metric_mm => x / 1000.0,
        "cm" => x / 100.0,
        "m" => x,
        "" | "in" => x * INCH,
        "ft" => x * FOOT,
        _ => return Err(NumError::NotNumber),
    })
}

/// Text typed into a row → stored value. Text rows keep the text as is; numbers are read in the
/// locale of `mark` and the row's unit, stored invariant, and must be finite.
pub fn parse_value(text: &str, ty: Ty, unit: &str, u: Units, mark: DecimalMark) -> Result<Sql, Refuse> {
    let bad = || Refuse::Number(text.trim().to_string());
    let x = match ty {
        Ty::Text => return Ok(Sql::Text(text.to_string())),
        // a mixed row has no single type: see `commit_in`
        Ty::Mixed | Ty::Other => return Err(Refuse::Locked),
        Ty::Int | Ty::Real if unit == "m" => {
            parse_len(text, u, mark).map_err(|e| refuse_number(e, text))?
        }
        Ty::Int | Ty::Real => {
            let mut t = text.trim();
            for s in [unit.to_string(), unit_label(unit, u)] {
                if !s.is_empty() {
                    t = t.strip_suffix(s.as_str()).unwrap_or(t);
                }
            }
            plain_number(t, mark).map_err(|e| refuse_number(e, text))?
        }
    };
    if !x.is_finite() {
        return Err(bad());
    }
    Ok(if ty == Ty::Int && x.fract() == 0.0 && x.abs() < 9e15 {
        Sql::Integer(x as i64)
    } else {
        Sql::Real(x)
    })
}

/// Runs `f` as one undo step. `Ok(true)`: the step is committed but the view reload failed
/// (status says so); `Err`: nothing was committed.
fn run_checked(ed: &mut Editor, f: OpFn) -> Result<bool, Refuse> {
    let top = ed.model.op_top().map_err(|e| Refuse::Failed(e.to_string()))?;
    match ed.run_op(f) {
        Ok(()) => Ok(false),
        Err(_) if ed.model.op_top().is_ok_and(|now| now != top) => {
            ed.status = Some(tr("imported_prop_edit.warn.reload", &[]));
            Ok(true)
        }
        Err(e) => Err(Refuse::Failed(e.to_string())),
    }
}

/// One value on every item: one model operation, one undo step.
pub fn set_all(ed: &mut Editor, items: &[Item], key: &str, v: Sql, unit: &str) -> Result<(), Refuse> {
    set_all_w(ed, items, key, v, unit).map(|_| ())
}

/// `set_all` that also tells whether the reload failed after the step was committed.
fn set_all_w(ed: &mut Editor, items: &[Item], key: &str, v: Sql, unit: &str) -> Result<bool, Refuse> {
    set_groups(ed, vec![(v, items.to_vec())], key, unit)
}

/// Writes each value to its own items, all in one model operation and one undo step.
fn set_groups(ed: &mut Editor, groups: Vec<(Sql, Vec<Item>)>, key: &str, unit: &str) -> Result<bool, Refuse> {
    if groups.iter().all(|g| g.1.is_empty()) {
        return Ok(false);
    }
    let (key, unit) = (key.to_string(), unit.to_string());
    let f = OpFn(Arc::new(move |m: &mut Model| {
        for (v, list) in groups.iter().filter(|g| !g.1.is_empty()) {
            m.set_source_props(list, &key, v.clone(), &unit)?;
        }
        Ok(())
    }));
    run_checked(ed, f)
}

/// Stored type of `key` on one element; a missing or empty value takes the row's type (text
/// on a mixed row), so nothing typed ever becomes a number where text was meant.
fn own_ty(m: &Model, it: Item, key: &str, row_ty: Ty) -> Result<Ty, Refuse> {
    let v = m
        .props(it.0, it.1)
        .map_err(|e| Refuse::Failed(e.to_string()))?
        .into_iter()
        .find(|p| p.0 == key)
        .map(|p| p.1);
    Ok(match v {
        Some(Sql::Text(_)) => Ty::Text,
        Some(Sql::Integer(_)) => Ty::Int,
        Some(Sql::Real(_)) => Ty::Real,
        Some(Sql::Blob(_)) => Ty::Other,
        Some(Sql::Null) | None if row_ty == Ty::Mixed => Ty::Text,
        Some(Sql::Null) | None => row_ty,
    })
}

/// Writes `text` typed into `row` to every item. `Ok(false)`: unchanged, nothing written.
pub fn commit(ed: &mut Editor, items: &[Item], row: &Row, text: &str) -> Result<bool, Refuse> {
    commit_in(ed, items, row, text, units::ui_decimal_mark())
}

/// [`commit`] with numbers typed in the locale of `mark`; a refused text writes nothing.
pub fn commit_in(
    ed: &mut Editor,
    items: &[Item],
    row: &Row,
    text: &str,
    mark: DecimalMark,
) -> Result<bool, Refuse> {
    let Some(unit) = &row.unit else {
        return Err(Refuse::Locked);
    };
    match &row.value {
        Some(v) if v == text => return Ok(false),
        None if text.trim().is_empty() => return Ok(false),
        _ => {}
    }
    if row.ty == Ty::Other {
        return Err(Refuse::Locked);
    }
    // each element keeps its own type: a number stays a number with its unit, a text stays
    // text; one element that cannot read the typed text refuses the whole edit
    let mut groups: Vec<(Sql, Vec<Item>)> = vec![];
    for &it in items {
        let ty = own_ty(&ed.model, it, &row.key, row.ty)?;
        let v = parse_value(text, ty, unit, ed.tool.units, mark)?;
        match groups.iter_mut().find(|g| g.0 == v) {
            Some(g) => g.1.push(it),
            None => groups.push((v, vec![it])),
        }
    }
    if !set_groups(ed, groups, &row.key, unit)? {
        let n = items.len().to_string();
        ed.status = Some(tr("imported_props.done", &[("key", &row.key), ("n", &n)]));
    }
    Ok(true)
}

/// Key and value of a new property: «Set.Name» (or the bare name without a set); a value that
/// reads as a number in the locale of `mark` is stored as one (invariant) and must be finite,
/// a comma the mark cannot read is refused, anything else is text.
pub fn new_prop(set: &str, name: &str, value: &str, mark: DecimalMark) -> Result<(String, Sql), Refuse> {
    let (set, name) = (set.trim(), name.trim());
    if name.is_empty() {
        return Err(Refuse::Name);
    }
    let key = if set.is_empty() {
        name.to_string()
    } else {
        format!("{set}.{name}")
    };
    let refuse = || Refuse::Number(value.trim().to_string());
    let word = value.trim().trim_start_matches(['+', '-']).to_ascii_lowercase();
    if matches!(word.as_str(), "inf" | "infinity" | "nan") {
        return Err(refuse());
    }
    let v = match ddc_ui::numparse::canonical(value, mark) {
        Err(NumError::CommaDecimal) => return Err(Refuse::Comma(value.trim().to_string())),
        Err(_) => Sql::Text(value.to_string()),
        Ok(c) => match c.parse::<f64>() {
            Ok(x) if !x.is_finite() => return Err(refuse()),
            Ok(x) if x.fract() == 0.0 && x.abs() < 9e15 && !c.contains(['.', 'e', 'E']) => {
                Sql::Integer(x as i64)
            }
            Ok(x) => Sql::Real(x),
            Err(_) => Sql::Text(value.to_string()),
        },
    };
    Ok((key, v))
}

/// «Add property» on the selection; returns the written key.
pub fn add_prop(
    ed: &mut Editor,
    items: &[Item],
    set: &str,
    name: &str,
    value: &str,
) -> Result<String, Refuse> {
    add_prop_in(ed, items, set, name, value, units::ui_decimal_mark())
}

/// [`add_prop`] with a number value typed in the locale of `mark`.
pub fn add_prop_in(
    ed: &mut Editor,
    items: &[Item],
    set: &str,
    name: &str,
    value: &str,
    mark: DecimalMark,
) -> Result<String, Refuse> {
    let (key, v) = new_prop(set, name, value, mark)?;
    if !set_all_w(ed, items, &key, v, "")? {
        let n = items.len().to_string();
        ed.status = Some(tr("imported_props.done", &[("key", &key), ("n", &n)]));
    }
    Ok(key)
}

/// Key prefixes the importer derives from something other than a property set: such rows are
/// read-only and cannot be removed.
const DERIVED: [&str; 3] = ["IFC.", "Material.", "type."];

/// Whether the row `key` may be removed (a stored property, not a derived one).
pub fn removable(key: &str) -> bool {
    !DERIVED.iter().any(|d| key.starts_with(d))
}

/// Whether the row gets the Remove context menu: editable and read-only rows alike.
pub fn row_removable(r: &Row) -> bool {
    removable(&r.key)
}

/// «Remove» on the selection: the property leaves every selected element in one undo step.
pub fn remove_prop(ed: &mut Editor, items: &[Item], key: &str) -> Result<(), Refuse> {
    if items.is_empty() {
        return Ok(());
    }
    if !removable(key) {
        return Err(Refuse::Locked);
    }
    let (list, k) = (items.to_vec(), key.to_string());
    let f = OpFn(Arc::new(move |m: &mut Model| {
        m.remove_source_props(&list, &k).map(|_| ())
    }));
    if !run_checked(ed, f)? {
        let n = items.len().to_string();
        ed.status = Some(tr("imported_prop_edit.done", &[("key", key), ("n", &n)]));
    }
    Ok(())
}

/// Panel state between frames (egui memory): rows cached per revision, the field being typed,
/// the filter and the «Add property» form.
#[derive(Clone, Default)]
struct State {
    items: u64,
    rev: Option<(u64, u64)>,
    rows: Arc<Vec<Row>>,
    focus: Option<(String, String)>,
    filter: String,
    add_open: bool,
    add: [String; 3],
    err: Option<String>,
}

fn items_sig(items: &[Item], u: Units, mark: DecimalMark) -> u64 {
    let mut h = DefaultHasher::new();
    items.hash(&mut h);
    (u == Units::Metric).hash(&mut h);
    (mark == DecimalMark::Comma).hash(&mut h);
    h.finish()
}

fn caption(ui: &mut egui::Ui, th: &Theme, text: String, color: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(th.font(Role::Caption))
                .color(th.c(color)),
        )
        .wrap(),
    );
}

fn row_label(name: &str) -> String {
    if crate::geom_props::looks_geometric(name) {
        format!("{name} \u{b7} {}", tr("geom.data_tag", &[]))
    } else {
        name.to_string()
    }
}

fn enter(ui: &egui::Ui, r: &egui::Response) -> bool {
    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Stored properties of the selected bodies, editable; nothing when no body is selected.
pub fn show(ui: &mut egui::Ui, th: &Theme, ed: &mut Editor) {
    let items = selected(ed);
    if items.is_empty() {
        return;
    }
    let id = egui::Id::new("ddc.ifp1.props");
    let mut st: State = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
    let u = ed.tool.units;
    let mark = units::ui_decimal_mark();
    let sig = items_sig(&items, u, mark);
    if st.items != sig {
        st = State {
            items: sig,
            ..Default::default()
        };
    }
    let rk = ed.cache_key();
    if st.rev != Some(rk) {
        st.rows = Arc::new(rows_of_in(&ed.model, &items, u, mark));
        st.rev = Some(rk);
    }
    let rows = st.rows.clone();
    let n = items.len().to_string();
    let varies = tr("imported_props.varies", &[]);
    let tip = tr("imported_props.tip.edit", &[("n", &n)]);
    ui.add_space(space::XS);
    if items.len() > 1 {
        caption(ui, th, tr("imported_props.common", &[("n", &n)]), "text.secondary");
    }
    if rows.len() > FILTER_AT {
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text(tr("imported_props.filter.hint", &[]))
                .desired_width(f32::INFINITY),
        );
    }
    let shown = filter_rows(&rows, &st.filter);
    if rows.len() > FILTER_AT {
        let (a, b) = (shown.len().to_string(), rows.len().to_string());
        caption(
            ui,
            th,
            tr("imported_props.filter.count", &[("shown", &a), ("total", &b)]),
            "text.secondary",
        );
    }
    if rows.is_empty() {
        caption(ui, th, tr("imported_props.empty", &[]), "text.secondary");
    }
    let keyed: Vec<(String, usize)> = shown.iter().map(|&i| (rows[i].key.clone(), i)).collect();
    let mut done: Option<(usize, String)> = None;
    let mut remove: Option<usize> = None;
    let rm_text = tr("imported_prop_edit.remove", &[]);
    let rm_tip = tr("imported_prop_edit.remove.tip", &[("n", &n)]);
    for (group, list) in prop_rows::group_props(&keyed) {
        if !group.is_empty() {
            prop_rows::block_title(ui, th, &group);
        }
        ui_kit::props(ui, &format!("ifp1.{group}"), |ui| {
            for (name, i) in &list {
                let r = &rows[*i];
                let label = row_label(name);
                if r.unit.is_none() || r.ty == Ty::Other {
                    let v = r.value.clone().unwrap_or_else(|| varies.clone());
                    if row_removable(r) {
                        // the value stays read-only; the row can still be removed
                        let text = format!("{v} {}", r.label).trim().to_string();
                        ui_kit::kv_widget(ui, &label, |ui| {
                            let resp = ui.add(
                                egui::Label::new(text).truncate().sense(egui::Sense::click()),
                            );
                            resp.context_menu(|ui| {
                                if crate::ctx_menu::danger_icon_row(ui, th, &rm_text)
                                    .on_hover_text(&rm_tip)
                                    .clicked()
                                {
                                    remove = Some(*i);
                                    ui.close();
                                }
                            });
                        });
                    } else {
                        ui_kit::kv(ui, &label, format!("{v} {}", r.label).trim().to_string());
                    }
                    continue;
                }
                let orig = r.value.clone().unwrap_or_default();
                let mut buf = match &st.focus {
                    Some((k, b)) if *k == r.key => b.clone(),
                    _ => orig.clone(),
                };
                let hint = if r.value.is_none() { varies.as_str() } else { "" };
                let align = if matches!(r.ty, Ty::Text | Ty::Mixed) {
                    egui::Align::LEFT
                } else {
                    egui::Align::RIGHT
                };
                let resp = ui_kit::kv_text(ui, &label, &mut buf, &r.label, &tip, hint, align);
                if row_removable(r) {
                    resp.context_menu(|ui| {
                        if crate::ctx_menu::danger_icon_row(ui, th, &rm_text)
                            .on_hover_text(&rm_tip)
                            .clicked()
                        {
                            remove = Some(*i);
                            ui.close();
                        }
                    });
                }
                if resp.has_focus() {
                    st.focus = Some((r.key.clone(), buf));
                } else if st.focus.as_ref().is_some_and(|f| f.0 == r.key) {
                    // Enter, Tab and a click elsewhere write a changed text; Esc drops it
                    let enter_key = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if ui_kit::field_end(resp.lost_focus(), enter_key, esc, buf != orig)
                        == ui_kit::FieldEnd::Apply
                    {
                        done = Some((*i, buf));
                    }
                    st.focus = None;
                }
            }
        });
    }
    if let Some((i, text)) = done {
        st.err = commit_in(ed, &items, &rows[i], &text, mark).err().map(|e| e.text());
    }
    if let Some(i) = remove {
        st.focus = None;
        st.err = remove_prop(ed, &items, &rows[i].key).err().map(|e| e.text());
    }
    let mesh = items.iter().all(|i| i.0 == "mesh");
    crate::geom_props::data_note(ui, mesh, rows.iter().map(|r| r.key.as_str()));
    add_form(ui, th, ed, &items, &mut st);
    if let Some(e) = &st.err {
        ui.add_space(space::XS);
        caption(ui, th, e.clone(), "error");
    }
    ui.data_mut(|d| d.insert_temp(id, st));
}

/// «Add property»: a quiet button that opens set / name / value fields under the rows.
fn add_form(ui: &mut egui::Ui, th: &Theme, ed: &mut Editor, items: &[Item], st: &mut State) {
    let n = items.len().to_string();
    ui.add_space(space::SM);
    if !st.add_open {
        if ui
            .button(tr("imported_props.add", &[]))
            .on_hover_text(tr("imported_props.add.tip", &[("n", &n)]))
            .clicked()
        {
            st.add_open = true;
            st.err = None;
        }
        return;
    }
    prop_rows::block_title(ui, th, &tr("imported_props.add.title", &[]));
    let mut go = false;
    let tip = tr("imported_props.add.tip", &[("n", &n)]);
    ui_kit::props(ui, "imported_props.add", |ui| {
        for (i, (k, hint)) in [
            ("imported_props.add.set", "imported_props.add.set_hint"),
            ("imported_props.add.name", "imported_props.add.name_hint"),
            ("imported_props.add.value", "imported_props.add.value_hint"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = ui_kit::kv_text(
                ui,
                &tr(k, &[]),
                &mut st.add[i],
                "",
                &tip,
                &tr(hint, &[]),
                egui::Align::LEFT,
            );
            // Enter in the name or value field adds; in the set field it only moves on
            go |= i > 0 && enter(ui, &r);
        }
    });
    let ok = !st.add[1].trim().is_empty();
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        go |= ui_kit::primary_button(ui, tr("imported_props.add.apply", &[]), ok).clicked();
        if ui_kit::cancel_button(ui, tr("imported_props.add.cancel", &[])).clicked() {
            st.add_open = false;
            st.add = Default::default();
            st.err = None;
        }
    });
    if go {
        match add_prop(ed, items, &st.add[0], &st.add[1], &st.add[2]) {
            Ok(_) => {
                st.add_open = false;
                st.add = Default::default();
                st.err = None;
            }
            Err(e) => st.err = Some(e.text()),
        }
    }
}

#[cfg(test)]
#[path = "imported_props_tests.rs"]
mod tests;
