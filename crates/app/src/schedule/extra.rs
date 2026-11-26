// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shared schedule columns: construction/demolition phases and category project parameters;
//! a cell edit is a single model operation (`Editor::run_op`), reverted by Ctrl+Z.

use super::{col, Cat, Edit as E, Kind as K, Row, Table, Val};
use crate::editor::cw::panel_kind_parse;
use crate::doc_lang::AsLang;
use crate::editor::sched_ops::{CatBatch, CatEdit, Reason};
use crate::editor::{Editor, OpFn};
use crate::expr_field::Scope;
use crate::units::{
    parse_edit_length, parse_edit_length_lang, parse_locale_number, ui_decimal_mark, DecimalMark, Units,
};
use ddc_model::{PVal, ParamDef, ParamSet, ParamType};
use std::sync::Arc;

pub const PH_CREATED: &str = "ph_c";
pub const PH_DEMOLISHED: &str = "ph_d";
pub const NO_DEMOLITION: &str = "—";

/// Row selection by phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PhaseMode {
    #[default]
    All,
    /// Standing in the phase (existing or new; excludes earlier-demolished and future elements).
    In(i64),
    /// Demolition schedule: demolished in the phase.
    Demolished(i64),
}

pub fn param_kind(t: ParamType) -> (K, E) {
    match t {
        ParamType::Text => (K::Text, E::Text),
        ParamType::Int => (K::Int, E::Num),
        ParamType::Number => (K::Num, E::Num),
        ParamType::Length => (K::Len, E::Num),
        ParamType::Area => (K::Area, E::Num),
        ParamType::Volume => (K::Vol, E::Num),
        ParamType::YesNo => (K::Text, E::Choice),
    }
}

pub fn param_title(d: &ParamDef, ru: impl AsLang) -> String {
    let mut s = d.spec.name.clone();
    if d.spec.ptype == ParamType::Number && !d.spec.unit.trim().is_empty() {
        s += &format!(", {}", d.spec.unit.trim());
    }
    if d.spec.per_type {
        s += &format!(" ({})", super::tl(ru, "sch.col.per_type"));
    }
    s
}

fn yes_no(ru: impl AsLang) -> [String; 2] {
    [super::tl(ru, "sch.val.yes"), super::tl(ru, "sch.val.no")]
}

pub fn pval(v: Option<PVal>, ru: impl AsLang) -> Val {
    match v {
        None => Val::T(String::new()),
        Some(PVal::Text(s)) => Val::T(s),
        Some(PVal::Bool(b)) => Val::T(yes_no(ru)[usize::from(!b)].clone()),
        Some(p) => Val::N(p.as_f64().unwrap_or(0.0)),
    }
}

/// Appends phase and project-parameter columns (after the category columns);
/// phase labels and values and yes/no are in the UI language.
pub fn extend(ed: &Editor, tb: &mut Table, ru: impl AsLang) {
    let m = &ed.model;
    if let Some(kind) = tb.cat.table() {
        let phases = m.phases().unwrap_or_default();
        if let (Ok(ix), false) = (m.phase_index(), phases.is_empty()) {
            tb.phases = phases
                .iter()
                .map(|p| (p.id, crate::phase_ui::label(p, ru)))
                .collect();
            let name = |id: i64| {
                tb.phases
                    .iter()
                    .find(|p| p.0 == id)
                    .map_or(String::new(), |p| p.1.clone())
            };
            let (tc, td) = (
                super::tl(ru, "sch.col.phase_created"),
                super::tl(ru, "sch.col.phase_demolished"),
            );
            let mut c = col(PH_CREATED, tc, K::Text, E::Choice);
            c.choices = tb.phases.iter().map(|p| p.1.clone()).collect();
            let mut d = col(PH_DEMOLISHED, td, K::Text, E::Choice);
            d.choices = std::iter::once(NO_DEMOLITION.to_string())
                .chain(tb.phases.iter().map(|p| p.1.clone()))
                .collect();
            tb.row_phase = tb.rows.iter().map(|r| Some(ix.get(kind, r.id))).collect();
            for (r, e) in tb.rows.iter_mut().zip(&tb.row_phase) {
                let e = e.expect("set above");
                r.vals.push(Val::T(name(e.created)));
                r.vals
                    .push(Val::T(e.demolished.map_or(NO_DEMOLITION.to_string(), name)));
            }
            tb.cols.push(c);
            tb.cols.push(d);
        }
    }
    if tb.row_phase.len() != tb.rows.len() {
        tb.row_phase = vec![None; tb.rows.len()];
    }
    let (Some(pc), Some(kind)) = (tb.cat.param_cat(), tb.cat.table()) else {
        return;
    };
    let defs = m.params_for(pc).unwrap_or_default();
    // row type size: one query per row for all type parameters, not per cell
    let types: Vec<Option<(String, i64)>> = if defs.iter().any(|d| d.spec.per_type) {
        tb.rows
            .iter()
            .map(|r| {
                m.type_of(kind, r.id)
                    .ok()
                    .flatten()
                    .map(|(k, i)| (k.to_string(), i))
            })
            .collect()
    } else {
        vec![]
    };
    for d in defs {
        let (k, e) = param_kind(d.spec.ptype);
        let mut c = col(format!("p:{}", d.id), param_title(&d, ru), k, e);
        if d.spec.ptype == ParamType::YesNo {
            c.choices = yes_no(ru).to_vec();
        }
        // all values of a parameter in one query; for a type parameter, per type
        let vals = m.param_values(d.id).unwrap_or_default();
        for (i, r) in tb.rows.iter_mut().enumerate() {
            let key = if d.spec.per_type {
                types.get(i).cloned().flatten()
            } else {
                Some((kind.to_string(), r.id))
            };
            r.vals
                .push(pval(key.and_then(|k| vals.get(&k).cloned()), ru));
        }
        tb.cols.push(c);
    }
}

/// Whether row `i` passes the phase selection.
pub fn phase_ok(tb: &Table, i: usize, mode: PhaseMode) -> bool {
    let pos = |id: i64| tb.phases.iter().position(|p| p.0 == id);
    let e = tb.row_phase.get(i).copied().flatten();
    match (mode, e) {
        (PhaseMode::All, _) => true,
        (_, None) => false,
        (PhaseMode::Demolished(p), Some(e)) => e.demolished == Some(p),
        (PhaseMode::In(p), Some(e)) => {
            let (Some(c), Some(v)) = (pos(e.created), pos(p)) else {
                return true;
            };
            let st = ddc_model::status_of(c, e.demolished.and_then(pos), v);
            !matches!(
                st,
                ddc_model::PhaseStatus::Future | ddc_model::PhaseStatus::Gone
            )
        }
    }
}

fn run(
    ed: &mut Editor,
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    if let Err(e) = ed.run_op(OpFn(Arc::new(f))) {
        ed.status = Some(crate::tr("sch.edit.failed", &[("e", &e.to_string())]));
    }
}

/// Edit of a shared column; `false` means a category column (the caller edits it).
pub fn apply(ed: &mut Editor, tb: &Table, r: &Row, c: &super::Col, text: &str) -> bool {
    let key = c.key.as_str();
    if key.starts_with("f:") {
        return true; // computed: read-only
    }
    let Some(kind) = tb.cat.table() else {
        return key.starts_with("p:") || key.starts_with("ph_");
    };
    let id = r.id;
    if key == PH_CREATED || key == PH_DEMOLISHED {
        // column label, case-insensitive name, or standard phase key
        let find = |t: &str| {
            let low = t.trim().to_lowercase();
            tb.phases
                .iter()
                .find(|p| p.1.trim().to_lowercase() == low)
                .map(|p| p.0)
                .or_else(|| ddc_model::find_phase(&ed.model.phases().unwrap_or_default(), t))
        };
        let items = vec![(kind, id)];
        match (key == PH_CREATED, find(text)) {
            (true, Some(p)) => edit_phases(ed, items, Some(p), None),
            (false, _) if text.trim() == NO_DEMOLITION || text.trim().is_empty() => {
                edit_phases(ed, items, None, Some(None))
            }
            (false, Some(p)) => edit_phases(ed, items, None, Some(Some(p))),
            (_, None) => ed.status = Some(crate::tr("sch.edit.no_phase", &[("text", text)])),
        }
        return true;
    }
    let Some(pid) = key.strip_prefix("p:").and_then(|s| s.parse::<i64>().ok()) else {
        return false;
    };
    edit_param(ed, pid, kind, id, text);
    true
}

/// Text the user typed into a cell -> value, numbers in the UI decimal mark `mark` (`1 250,5` /
/// `1,250.5`; an English `1,5` is refused). Area and volume may end in `m2` / `m3` (SI kept).
pub(crate) fn typed_value(t: ParamType, text: &str, u: Units, mark: DecimalMark) -> Option<PVal> {
    let num = |s: &str| parse_locale_number(s, mark).ok();
    match t {
        ParamType::Length => parse_edit_length_lang(text, u, &Scope::none(), mark).ok().map(PVal::Num),
        ParamType::Area | ParamType::Volume => {
            let low = text.trim().to_lowercase();
            let body = ["m2", "m\u{b2}", "m3", "m\u{b3}"]
                .iter()
                .find_map(|s| low.strip_suffix(s))
                .unwrap_or(&low);
            num(body).map(PVal::Num)
        }
        ParamType::Number => num(text).map(PVal::Num),
        ParamType::Int => num(text)
            .filter(|x| x.fract() == 0.0 && x.abs() <= 9.0e15)
            .map(|x| PVal::Int(x as i64)),
        t => ddc_model::parse_value(t, text).ok(),
    }
}

/// Parameter write from field text (empty clears); a type parameter goes to the type.
fn param_set(ed: &Editor, pid: i64, kind: &str, id: i64, text: &str) -> Result<ParamSet, String> {
    param_set_in(&crate::tr, ed, pid, kind, id, text)
}

/// [`param_set`] with the message catalog passed in.
fn param_set_in(
    tr: &dyn Fn(&str, &[(&str, &str)]) -> String,
    ed: &Editor,
    pid: i64,
    kind: &str,
    id: i64,
    text: &str,
) -> Result<ParamSet, String> {
    let Some(d) = ed
        .model
        .params()
        .ok()
        .and_then(|v| v.into_iter().find(|d| d.id == pid))
    else {
        return Err(tr("sch.edit.param_gone", &[]));
    };
    let value = if text.is_empty() {
        None
    } else {
        match typed_value(d.spec.ptype, text, ed.tool.units, ui_decimal_mark()) {
            Some(v) => Some(v),
            None => {
                return Err(tr(
                    "sch.edit.bad_value",
                    &[("text", text), ("name", d.spec.name.as_str())],
                ))
            }
        }
    };
    let target = if d.spec.per_type {
        match ed.model.type_of(kind, id).ok().flatten() {
            Some((tk, tid)) => (tk.to_string(), tid),
            None => return Err(tr("sch.edit.no_size", &[])),
        }
    } else {
        (kind.to_string(), id)
    };
    Ok(ParamSet {
        param: pid,
        kind: target.0,
        id: target.1,
        value,
    })
}

/// Project parameter value from the field text (empty clears it); a type parameter goes to the type.
pub fn edit_param(ed: &mut Editor, pid: i64, kind: &str, id: i64, text: &str) {
    match param_set(ed, pid, kind, id, text) {
        Ok(set) => run(ed, move |m| m.set_param_values(std::slice::from_ref(&set))),
        Err(e) => ed.status = Some(e),
    }
}

/// [`edit_param`] for every element of a multi-selection: the same checks, a type parameter
/// written once per type, locked values kept and counted, one undo step. Returns the writes.
pub fn edit_param_many(ed: &mut Editor, pid: i64, targets: &[(&str, i64)], text: &str) -> usize {
    use crate::status::Severity;
    let mut sets: Vec<ParamSet> = vec![];
    let mut same = 0;
    for &(kind, id) in targets {
        let set = match param_set(ed, pid, kind, id, text) {
            Ok(s) => s,
            Err(e) => {
                ed.say(Severity::Refusal, e);
                return 0;
            }
        };
        if sets.iter().any(|s| s.kind == set.kind && s.id == set.id) {
            continue;
        }
        if ed.model.param_value(pid, &set.kind, set.id).ok().flatten() == set.value {
            same += 1;
            continue;
        }
        sets.push(set);
    }
    let (sets, locked) = match ed.model.split_locked_sets(&sets) {
        Ok(v) => v,
        Err(e) => {
            ed.say(Severity::Error, crate::tr("sch.edit.failed", &[("e", &e.to_string())]));
            return 0;
        }
    };
    let n = sets.len();
    let mut parts = vec![];
    if n > 0 {
        let op = OpFn(Arc::new(move |m: &mut ddc_model::Model| m.set_param_values(&sets)));
        if let Err(e) = ed.run_op(op) {
            ed.say(Severity::Error, crate::tr("sch.edit.failed", &[("e", &e.to_string())]));
            return 0;
        }
        parts.push(crate::tr("lnp.params_changed", &[("n", &n.to_string())]));
    } else if locked.is_empty() {
        parts.push(crate::tr("lnp.params_same", &[("u", &same.to_string())]));
    }
    if !locked.is_empty() {
        parts.push(crate::tr("locked_values.kept", &[("n", &locked.len().to_string())]));
    }
    let sev = match (n, locked.len()) {
        (0, 0) => Severity::Info,
        (0, _) => Severity::Refusal,
        (_, 0) => Severity::Info,
        _ => Severity::Warning,
    };
    ed.say(sev, parts.join(" \u{b7} "));
    n
}

/// One checked cell write for a batch.
enum Write {
    Param(ParamSet),
    Phase(&'static str, i64, Option<i64>, Option<Option<i64>>),
    Cat(i64, u32, CatEdit),
}

/// Result of a batch cell write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub done: usize,
    pub readonly: usize,
    pub bad: usize,
    /// Cells that already hold the pasted value: not an error, nothing to write.
    pub unchanged: usize,
}

impl Outcome {
    pub fn skipped(&self) -> usize {
        self.readonly + self.bad
    }

    /// Status line; `fill` is fill-down, otherwise paste.
    pub fn say(&self, fill: bool) -> String {
        let u = self.unchanged.to_string();
        if self.done == 0 && self.skipped() == 0 && self.unchanged > 0 {
            return crate::tr("sched_edit.no_changes", &[("u", &u)]);
        }
        let n = self.done.to_string();
        let mut msg = if self.skipped() == 0 {
            crate::tr(if fill { "sched_edit.filled" } else { "sched_edit.pasted" }, &[("n", &n)])
        } else {
            let mut why = vec![];
            if self.readonly > 0 {
                why.push(crate::tr("sched_edit.why_ro", &[("n", &self.readonly.to_string())]));
            }
            if self.bad > 0 {
                why.push(crate::tr("sched_edit.why_bad", &[("n", &self.bad.to_string())]));
            }
            let key = if fill { "sched_edit.filled_skipped" } else { "sched_edit.pasted_skipped" };
            crate::tr(
                key,
                &[
                    ("n", &n),
                    ("k", &self.skipped().to_string()),
                    ("why", &why.join(", ")),
                ],
            )
        };
        if self.unchanged > 0 {
            msg.push_str(&crate::tr("sched_edit.same_tail", &[("u", &u)]));
        }
        msg
    }
}

fn phase_of(ed: &Editor, tb: &Table, text: &str) -> Option<i64> {
    let low = text.trim().to_lowercase();
    tb.phases
        .iter()
        .find(|p| p.1.trim().to_lowercase() == low)
        .map(|p| p.0)
        .or_else(|| ddc_model::find_phase(&ed.model.phases().unwrap_or_default(), text))
}

/// Column value text to a category edit; `None` when the column has no writer.
fn cat_edit(ed: &Editor, cat: Cat, kind: K, key: &str, text: &str) -> Option<Result<CatEdit, ()>> {
    let u = ed.tool.units;
    let num = || {
        match kind {
            K::Len => parse_edit_length(text, u),
            _ => parse_locale_number(text.trim().trim_end_matches('%').trim(), ui_decimal_mark()).ok(),
        }
        .filter(|v| v.is_finite())
        .ok_or(())
    };
    Some(match (cat, key) {
        (Cat::Walls, "type") => ed
            .types
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(text))
            .map(|t| CatEdit::WallType(t.id))
            .ok_or(()),
        (Cat::Walls, "len") => num().map(CatEdit::WallLen),
        (Cat::Walls, "h") => num().map(CatEdit::WallHeight),
        (Cat::Doors | Cat::Windows, "w") => num().map(CatEdit::OpWidth),
        (Cat::Doors | Cat::Windows, "h") => num().map(CatEdit::OpHeight),
        (Cat::Doors | Cat::Windows, "sill") => num().map(CatEdit::OpSill),
        (Cat::Curtains, "name") => Ok(CatEdit::CurtainName(text.to_string())),
        (Cat::Curtains, "h") => num().map(CatEdit::CurtainHeight),
        (Cat::Panels, "kind") => panel_kind_parse(text).map(CatEdit::PanelKind).ok_or(()),
        (Cat::Panels, "name") => Ok(CatEdit::PanelName(text.to_string())),
        (Cat::Slabs, "slope") => num().map(CatEdit::SlabSlope),
        (Cat::Rooms, "name") => Ok(CatEdit::RoomName(text.to_string())),
        (Cat::Rooms, "number") => Ok(CatEdit::RoomNumber(text.to_string())),
        (Cat::Rooms, "h") => num().map(CatEdit::RoomHeight),
        _ => return None,
    })
}

/// Check one cell by the same rules as an edit: `Err(true)` read-only, `Err(false)` bad value.
fn plan_cell(ed: &Editor, tb: &Table, row: usize, col: usize, text: &str) -> Result<Write, bool> {
    let (Some(r), Some(c)) = (tb.rows.get(row), tb.cols.get(col)) else {
        return Err(true);
    };
    let key = c.key.as_str();
    let text = text.trim();
    // computed and non-editable columns
    if c.edit == E::No || key.starts_with("f:") {
        return Err(true);
    }
    if key == PH_CREATED || key == PH_DEMOLISHED || key.starts_with("p:") {
        let Some(kind) = tb.cat.table() else {
            return Err(true);
        };
        if key == PH_CREATED {
            return match phase_of(ed, tb, text) {
                Some(p) => Ok(Write::Phase(kind, r.id, Some(p), None)),
                None => Err(false),
            };
        }
        if key == PH_DEMOLISHED {
            if text == NO_DEMOLITION || text.is_empty() {
                return Ok(Write::Phase(kind, r.id, None, Some(None)));
            }
            return match phase_of(ed, tb, text) {
                Some(p) => Ok(Write::Phase(kind, r.id, None, Some(Some(p)))),
                None => Err(false),
            };
        }
        let Some(pid) = key.strip_prefix("p:").and_then(|s| s.parse::<i64>().ok()) else {
            return Err(true);
        };
        return param_set(ed, pid, kind, r.id, text)
            .map(Write::Param)
            .map_err(|_| false);
    }
    match cat_edit(ed, tb.cat, c.kind, key, text) {
        Some(Ok(e)) => Ok(Write::Cat(r.id, r.sub, e)),
        Some(Err(())) => Err(false),
        None => Err(true),
    }
}

/// Dry run of one cell by the [`write_cells`] rules, nothing written: `Ok(true)` the cell would
/// change, `Ok(false)` it already holds the value, `Err` why it is skipped.
pub fn check_cell(ed: &Editor, tb: &Table, row: usize, col: usize, text: &str) -> Result<bool, Skip> {
    match plan_cell(ed, tb, row, col, text).map_err(|ro| if ro { Skip::ReadOnly } else { Skip::Bad })? {
        Write::Param(s) => {
            if ed.model.param_value(s.param, &s.kind, s.id).ok() == Some(s.value.clone()) {
                return Ok(false);
            }
            // a locked value is refused as read-only, as `write_cells` does on apply
            if param_locked(ed, &s) {
                return Err(Skip::ReadOnly);
            }
            Ok(true)
        }
        Write::Phase(..) => Ok(true),
        Write::Cat(id, sub, e) => CatBatch::default()
            .stage(ed, id, sub, e)
            .map_err(Skip::Refused),
    }
}

/// Why [`check_cell`] skips a cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Skip {
    ReadOnly,
    Bad,
    /// A valid value the model cannot take; the reason is shown to the user.
    Refused(Reason),
}

fn param_locked(ed: &Editor, s: &ParamSet) -> bool {
    ed.model
        .split_locked_sets(std::slice::from_ref(s))
        .is_ok_and(|(_, locked)| !locked.is_empty())
}

/// The cell writes a parameter value that is locked (refused as read-only by [`check_cell`]).
pub fn cell_locked(ed: &Editor, tb: &Table, row: usize, col: usize, text: &str) -> bool {
    matches!(plan_cell(ed, tb, row, col, text), Ok(Write::Param(s)) if param_locked(ed, &s))
}

/// Cells `(row, col, text)` as one model op = one undo step.
/// Each cell is checked alone: unusable ones are skipped, cells that already hold the value
/// are counted as unchanged, the rest are written.
pub fn write_cells(ed: &mut Editor, tb: &Table, cells: &[(usize, usize, String)]) -> Outcome {
    write_cells_why(ed, tb, cells).0
}

/// Status of a paste or fill: the counts, then the first refused cell with row and column.
pub fn say_why(o: &Outcome, why: &[(usize, usize, Reason)], tb: &Table, fill: bool) -> String {
    let mut msg = o.say(fill);
    if let Some((r, c, m)) = why.first() {
        let col = tb.cols.get(*c).map_or("", |c| c.title.as_str());
        msg.push_str(". ");
        msg.push_str(&crate::tr(
            "sched_edit.refused",
            &[("row", &(r + 1).to_string()), ("col", col), ("why", &m.text_ui())],
        ));
    }
    msg
}

type PhaseSet = (&'static str, i64, Option<i64>, Option<Option<i64>>);

/// Cells checked one after another against the staged set, nothing written yet.
struct Staged {
    out: Outcome,
    why: Vec<(usize, usize, Reason)>,
    sets: Vec<ParamSet>,
    phases: Vec<PhaseSet>,
    batch: CatBatch,
    /// Cells that will be written.
    n: usize,
}

/// What [`write_cells_why`] would do with `cells`, same checks in the same order, nothing written:
/// `done` is the count it would write, the refused cells come with their reasons.
pub fn dry_run(ed: &Editor, tb: &Table, cells: &[(usize, usize, String)]) -> (Outcome, Vec<(usize, usize, Reason)>) {
    let mut s = stage_cells(ed, tb, cells);
    s.out.done = s.n;
    (s.out, s.why)
}

fn stage_cells(ed: &Editor, tb: &Table, cells: &[(usize, usize, String)]) -> Staged {
    let mut why = vec![];
    let mut out = Outcome::default();
    let mut sets: Vec<ParamSet> = vec![];
    let mut phases = vec![];
    let mut batch = CatBatch::default();
    // staged cells as (element, panel, field): a field pasted twice is one written cell
    let mut staged = vec![];
    for (r, c, text) in cells {
        match plan_cell(ed, tb, *r, *c, text) {
            Ok(Write::Param(s)) => {
                if ed.model.param_value(s.param, &s.kind, s.id).ok() == Some(s.value.clone()) {
                    out.unchanged += 1;
                } else {
                    sets.push(s);
                }
            }
            Ok(Write::Phase(k, id, cr, de)) => phases.push((k, id, cr, de)),
            Ok(Write::Cat(id, sub, e)) => match batch.stage(ed, id, sub, e.clone()) {
                Ok(true) => staged.push((id, sub, std::mem::discriminant(&e))),
                Ok(false) => out.unchanged += 1,
                Err(m) => {
                    out.bad += 1;
                    why.push((*r, *c, m));
                }
            },
            Err(true) => out.readonly += 1,
            Err(false) => out.bad += 1,
        }
    }
    // a locked value is a read-only cell: skipped alone, the rest of the paste is written
    if let Ok((keep, locked)) = ed.model.split_locked_sets(&sets) {
        out.readonly += locked.len();
        sets = keep;
    }
    // cells on elements that really differ after all cells; a value set and set back is unchanged
    let mut written = std::collections::HashSet::new();
    for (id, sub, f) in staged {
        if batch.differs(id) {
            written.insert((id, sub, f));
        } else {
            out.unchanged += 1;
        }
    }
    let cat_n = written.len();
    let n = sets.len() + phases.len() + cat_n;
    Staged { out, why, sets, phases, batch, n }
}

/// [`write_cells`] plus the refused cells `(row, col, reason)` of the staged set.
pub fn write_cells_why(
    ed: &mut Editor,
    tb: &Table,
    cells: &[(usize, usize, String)],
) -> (Outcome, Vec<(usize, usize, Reason)>) {
    let mut s = stage_cells(ed, tb, cells);
    let why = std::mem::take(&mut s.why);
    (commit(ed, vec![s]), why)
}

/// Cells of several tables (one per element category) as ONE model op = one undo step; each
/// cell is checked like in [`write_cells`].
pub fn write_cells_multi(ed: &mut Editor, parts: &[(&Table, Vec<(usize, usize, String)>)]) -> Outcome {
    let staged = parts.iter().map(|(tb, c)| stage_cells(ed, tb, c)).collect();
    commit(ed, staged)
}

fn commit(ed: &mut Editor, staged: Vec<Staged>) -> Outcome {
    let mut out = Outcome::default();
    let mut n = 0;
    let mut writes = vec![];
    for s in staged {
        out.readonly += s.out.readonly;
        out.bad += s.out.bad;
        out.unchanged += s.out.unchanged;
        n += s.n;
        if !(s.sets.is_empty() && s.phases.is_empty() && s.batch.changed() == 0) {
            writes.push((s.sets, s.phases, s.batch));
        }
    }
    if writes.is_empty() {
        return out;
    }
    let writes = Arc::new(writes);
    let f = move |m: &mut ddc_model::Model| {
        m.atomic("sched_cells", |m| {
            for (sets, phases, batch) in writes.iter() {
                m.set_param_values(sets)?;
                for (k, id, cr, de) in phases {
                    m.set_element_phases(&[(*k, *id)], *cr, *de)?;
                }
                batch.apply(m)?;
            }
            Ok(())
        })
    };
    let top = ed.model.op_top().ok();
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => out.done = n,
        // the op is in the journal and only the reload failed: written, not "not changed"
        Err(e) if ed.model.op_top().ok() != top => {
            ed.status = Some(crate::tr("sched_edit.not_refreshed", &[("e", &e.to_string())]));
            out.done = n;
        }
        Err(e) => {
            ed.status = Some(crate::tr("sched_edit.failed", &[("e", &e.to_string())]));
            out.bad += n;
        }
    }
    out
}

/// Construction phase (`Some(p)`) and/or demolition phase (`Some(None)` = not demolished) of elements.
pub fn edit_phases(
    ed: &mut Editor,
    items: Vec<(&'static str, i64)>,
    created: Option<i64>,
    demolished: Option<Option<i64>>,
) {
    run(ed, move |m| {
        m.set_element_phases(&items, created, demolished)
    });
}

#[cfg(test)]
mod tests {
    use super::super::{build, order, Cat};
    use super::*;
    use crate::units::Units;

    fn ed_with_walls() -> Editor {
        let mut e = Editor::new_in_memory().unwrap();
        super::super::tests::room(&mut e);
        e
    }

    #[test]
    fn typed_cells_follow_the_given_ui_mark() {
        use DecimalMark::{Comma as RU, Point as EN};
        let m = Units::Metric;
        let n = |t: ParamType, s: &str, mark| typed_value(t, s, m, mark);
        assert_eq!(n(ParamType::Number, "1 250,5", RU), Some(PVal::Num(1250.5)));
        assert_eq!(n(ParamType::Number, "1.5", RU), Some(PVal::Num(1.5)));
        assert_eq!(n(ParamType::Number, "1,250.5", EN), Some(PVal::Num(1250.5)));
        assert_eq!(n(ParamType::Number, "1,250", EN), Some(PVal::Num(1250.0)));
        assert_eq!(n(ParamType::Number, "1,5", EN), None, "English comma is never 1.5");
        assert_eq!(n(ParamType::Area, "12,5 m2", RU), Some(PVal::Num(12.5)));
        assert_eq!(n(ParamType::Int, "1 000", RU), Some(PVal::Int(1000)));
        assert_eq!(n(ParamType::Int, "1,000", EN), Some(PVal::Int(1000)));
        assert_eq!(n(ParamType::Int, "1,5", RU), None);
        assert_eq!(n(ParamType::Number, "abc", RU), None);
    }

    #[test]
    fn param_column_edit_is_one_undo_step() {
        let mut e = ed_with_walls();
        let pid = e
            .model
            .add_param(&ddc_model::ParamSpec {
                name: "Price".into(),
                ptype: ParamType::Number,
                per_type: false,
                unit: "rub".into(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        e.reload().unwrap();
        let tb = build(&e, Cat::Walls);
        let j = tb
            .cols
            .iter()
            .position(|c| c.key == format!("p:{pid}"))
            .unwrap();
        assert_eq!(tb.cols[j].title, "Price, rub");
        super::super::apply_edit(&mut e, &tb, 0, j, "12,5");
        let tb2 = build(&e, Cat::Walls);
        assert_eq!(tb2.rows[0].vals[j], Val::N(12.5), "{:?}", e.status);
        e.undo();
        assert_eq!(build(&e, Cat::Walls).rows[0].vals[j], Val::T(String::new()));
        // not a number: nothing is written
        super::super::apply_edit(&mut e, &tb, 0, j, "abc");
        let st = e.status.as_deref().unwrap_or("");
        assert!(st.starts_with("\u{43d}\u{435} \u{438}\u{437}\u{43c}\u{435}\u{43d}\u{435}\u{43d}\u{43e}") || st.starts_with("Not changed"), "{st}");
    }


    #[test]
    fn param_set_errors_localized_en() {
        use ddc_ui::i18n::{Catalog, Lang};
        let mut e = ed_with_walls();
        let pid = e
            .model
            .add_param(&ddc_model::ParamSpec {
                name: "Price".into(),
                ptype: ParamType::Number,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        e.reload().unwrap();
        let cat = Catalog::new(Lang::En);
        let tr = |k: &str, a: &[(&str, &str)]| {
            let mut s = cat.t_in(Lang::En, k);
            for (n, v) in a {
                s = s.replace(&format!("{{{n}}}"), v);
            }
            s
        };
        let gone = param_set_in(&tr, &e, pid + 100, "wall", 1, "1").unwrap_err();
        let bad = param_set_in(&tr, &e, pid, "wall", 1, "abc").unwrap_err();
        for m in [&gone, &bad] {
            assert!(m.starts_with("Not changed"), "{m}");
            assert!(!m.contains("sch.edit."), "{m}");
        }
        assert!(bad.contains("abc") && bad.contains("Price"), "{bad}");
        assert!(param_set_in(&tr, &e, pid, "wall", 1, "4,5").is_ok());
    }

    #[test]
    fn english_labels_and_type_params_in_one_pass() {
        let mut e = ed_with_walls();
        let mut spec = ddc_model::ParamSpec {
            name: "Fire rating".into(),
            ptype: ParamType::YesNo,
            per_type: true,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        };
        let pid = e.model.add_param(&spec).unwrap();
        spec.name = "Mark".into();
        spec.ptype = ParamType::Text;
        spec.per_type = false;
        e.model.add_param(&spec).unwrap();
        e.reload().unwrap();
        let base = build(&e, Cat::Walls);
        let (tk, tid) = e.model.type_of("wall", base.rows[0].id).unwrap().unwrap();
        let set = ParamSet {
            param: pid,
            kind: tk.to_string(),
            id: tid,
            value: Some(PVal::Bool(true)),
        };
        e.model.set_param_values(&[set]).unwrap();
        let mut tb = Table {
            cat: Cat::Walls,
            cols: vec![],
            rows: base.rows.clone(),
            row_phase: vec![],
            phases: vec![],
        };
        for r in tb.rows.iter_mut() {
            r.vals.clear();
        }
        extend(&e, &mut tb, false);
        let jc = tb.cols.iter().position(|c| c.key == PH_CREATED).unwrap();
        assert_eq!(tb.cols[jc].title, "Phase created");
        assert_eq!(tb.rows[0].vals[jc], Val::T("New construction".into()));
        let jp = tb
            .cols
            .iter()
            .position(|c| c.key == format!("p:{pid}"))
            .unwrap();
        assert_eq!(tb.cols[jp].title, "Fire rating (type)");
        assert_eq!(tb.rows[0].vals[jp], Val::T("Yes".into()));
        // phase edit: case and whitespace are ignored
        let jd = tb.cols.iter().position(|c| c.key == PH_DEMOLISHED).unwrap();
        let r = tb.rows[0].clone();
        assert!(apply(&mut e, &tb, &r, &tb.cols[jd], "  new construction "));
        let ps = e.model.phases().unwrap();
        let ix = e.model.phase_index().unwrap();
        assert_eq!(
            ix.get("wall", r.id).demolished,
            Some(ps[2].id),
            "{:?}",
            e.status
        );
    }

    #[test]
    fn phase_columns_and_filter() {
        let mut e = ed_with_walls();
        let ps = e.model.phases().unwrap();
        let tb = build(&e, Cat::Walls);
        let jc = tb.cols.iter().position(|c| c.key == PH_CREATED).unwrap();
        let jd = tb.cols.iter().position(|c| c.key == PH_DEMOLISHED).unwrap();
        // new walls are in the current phase ("New"); demolition is in the same phase
        let last = tb.phases.last().unwrap().1.clone();
        assert_eq!(tb.rows[0].vals[jc], Val::T(last.clone()));
        super::super::apply_edit(&mut e, &tb, 0, jd, &last);
        let tb = build(&e, Cat::Walls);
        assert_eq!(tb.rows[0].vals[jd], Val::T(last));
        let all = order(&tb, &[], None, Units::Metric);
        let demo: Vec<u32> = all
            .iter()
            .copied()
            .filter(|&i| phase_ok(&tb, i as usize, PhaseMode::Demolished(ps[2].id)))
            .collect();
        assert_eq!(demo.len(), 1);
        // the first phase has no new walls yet
        assert!(!phase_ok(&tb, 1, PhaseMode::In(ps[0].id)));
        assert!(phase_ok(&tb, 1, PhaseMode::In(ps[2].id)));
        e.undo();
        assert_eq!(
            build(&e, Cat::Walls).rows[0].vals[jd],
            Val::T(NO_DEMOLITION.into())
        );
    }
}
