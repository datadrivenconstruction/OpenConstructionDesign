// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model export to .xlsx: frozen header, autofilter, non-editable columns shown grey.

use std::collections::HashMap;
use std::path::Path;

use ddc_model::Model;
use rust_xlsxwriter::{Color, Format, Formula, Note, ProtectionOptions, Workbook};

use super::baseline;
use super::scope;
use super::book;
use super::extra::{self, Kept, XCol};
use super::*;

#[derive(Clone, Debug)]
pub(crate) enum V {
    N(f64),
    S(String),
    Blank,
    /// User formula (with "="), carried over by a re-export, and the result the book cached for
    /// it: a reader that does not recalculate sees that value instead of 0.
    F(String, Option<String>),
    /// Date, time or duration of a kept user cell: serial value and its number format.
    D(f64, &'static str),
    /// Boolean of a kept user cell.
    B(bool),
    /// Error constant of a kept user cell ("#N/A", ...).
    E(String),
}

/// Free columns to the right of the table are unlocked: a new column = a new parameter.
const SPARE_COLS: u16 = 30;

const READ_ONLY_NOTE: &str =
    "\u{422}\u{43e}\u{43b}\u{44c}\u{43a}\u{43e} \u{447}\u{442}\u{435}\u{43d}\u{438}\u{435}: \u{43b}\u{438}\u{441}\u{442} \u{43f}\u{435}\u{440}\u{435}\u{441}\u{447}\u{438}\u{442}\u{44b}\u{432}\u{430}\u{435}\u{442}\u{441}\u{44f} \u{438}\u{437} \u{43c}\u{43e}\u{434}\u{435}\u{43b}\u{438} \u{43f}\u{440}\u{438} \u{432}\u{44b}\u{433}\u{440}\u{443}\u{437}\u{43a}\u{435}, \u{43f}\u{440}\u{430}\u{432}\u{43a}\u{438} \u{437}\u{434}\u{435}\u{441}\u{44c} \u{43d}\u{435} \u{43f}\u{440}\u{438}\u{43c}\u{435}\u{43d}\u{44f}\u{44e}\u{442}\u{441}\u{44f}.
Read-only: recomputed from the model on export; edits here are not applied.";

pub(crate) type Rows = Vec<Vec<V>>;

/// Rows of the WallTypes sheet; the import checks its read-only columns against them.
pub(crate) const TYPES_SQL: &str = "SELECT t.id,t.name,COUNT(l.pos),COALESCE(SUM(l.thickness),0) FROM wall_type t \
         LEFT JOIN wall_type_layer l ON l.wtype=t.id GROUP BY t.id ORDER BY t.id";

/// Rows of the QuantitiesSummary sheet; the import recomputes them to check the book's cells.
pub(crate) const SUMMARY_SQL: &str = "SELECT 'wall', t.name, lv.name, COUNT(*), SUM(x.fp), SUM(x.vol) FROM ( \
           SELECT w.wtype, w.level, \
             (SELECT COALESCE(SUM(volume),0) FROM wall_qty q WHERE q.wall=w.id) AS vol, \
             (SELECT COALESCE(SUM(plan_area),0) FROM wall_qty q WHERE q.wall=w.id) AS fp \
           FROM wall w) x JOIN wall_type t ON t.id=x.wtype JOIN level lv ON lv.id=x.level \
           GROUP BY x.wtype, x.level \
         UNION ALL \
         SELECT 'slab', t.name, lv.name, COUNT(*), SUM(y.a), SUM(y.v) FROM ( \
           SELECT s.stype, s.level, \
             (SELECT COALESCE(MAX(area),0) FROM slab_qty q WHERE q.slab=s.id) AS a, \
             (SELECT COALESCE(SUM(volume),0) FROM slab_qty q WHERE q.slab=s.id) AS v \
           FROM slab s) y JOIN slab_type t ON t.id=y.stype JOIN level lv ON lv.id=y.level \
           GROUP BY y.stype, y.level \
         UNION ALL \
         SELECT 'room', '', lv.name, COUNT(*), COALESCE(SUM(q.area),0), COALESCE(SUM(q.volume),0) \
           FROM room r JOIN level lv ON lv.id=r.level LEFT JOIN room_qty q ON q.room=r.id \
           GROUP BY r.level \
         UNION ALL \
         SELECT 'opening', o.kind, lv.name, COUNT(*), SUM(o.width*o.height), NULL \
           FROM opening o JOIN wall w ON w.id=o.wall JOIN level lv ON lv.id=w.level \
           GROUP BY o.kind, w.level \
         ORDER BY 1,2,3";

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Logo picture at the top of column `col`, at most `LOGO_PX` px high; proportions kept.
const LOGO_PX: f64 = 72.0;

fn put_logo(ws: &mut rust_xlsxwriter::Worksheet, col: u16, png: &[u8]) -> Result<(), String> {
    let Some((w, h)) = ddc_model::png_size(png) else {
        return Ok(());
    };
    let k = (LOGO_PX / f64::from(h)).min(240.0 / f64::from(w));
    let img = rust_xlsxwriter::Image::new_from_buffer(png)
        .map_err(err)?
        .set_scale_height(k)
        .set_scale_width(k);
    ws.insert_image(0, col, &img).map_err(err)?;
    Ok(())
}

pub(crate) fn query(model: &Model, sql: &str, n: usize) -> Result<Rows, String> {
    use rusqlite::types::Value;
    let mut st = model.sql().prepare(sql).map_err(err)?;
    let rows = st
        .query_map([], |r| {
            (0..n)
                .map(|i| {
                    Ok(match r.get::<_, Value>(i)? {
                        Value::Integer(v) => V::N(v as f64),
                        Value::Real(v) => V::N(v),
                        Value::Text(s) => V::S(s),
                        _ => V::Blank,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

/// Materials in use that have no table row, with the values the model reads for them; the id
/// cell stays blank (import matches such a row by name).
fn rowless_materials(model: &Model) -> Result<Rows, String> {
    let all = model.materials().map_err(err)?;
    let num = |o: Option<f64>| o.map_or(V::Blank, V::N);
    Ok(all
        .into_iter()
        .filter(|m| m.id == 0)
        .map(|m| {
            let s = m.spec;
            vec![
                V::Blank,
                V::S(s.name),
                s.color.map_or(V::Blank, |c| V::S(format!("#{c:06X}"))),
                V::S(s.hatch),
                num(s.density),
                num(s.conductivity),
                num(s.specific_heat),
                V::S(s.note),
            ]
        })
        .collect())
}

fn ids(rows: &Rows) -> Vec<i64> {
    rows.iter()
        .map(|r| match r.first() {
            Some(V::N(v)) => *v as i64,
            _ => 0,
        })
        .collect()
}

fn sheet(
    wb: &mut Workbook,
    name: &str,
    cols: &[Col],
    rows: Rows,
    u: Units,
    extra: &[XCol],
) -> Result<(), String> {
    gate_sheet()?;
    super::cellsafe::enter(name);
    // conflicting cells the user left on the model value keep the book value; the baseline
    // keeps the old export value, so the next read finds the conflict again
    let mut rows = rows;
    // rows in the order the user left them in the old book (sheet_view.rs)
    let moved = super::sheet_view::arrange(name, cols, &mut rows, extra);
    let extra: &[XCol] = moved.as_deref().unwrap_or(extra);
    let mut book_x: Option<Vec<XCol>> = None;
    let mut base_view: Option<(Rows, Vec<XCol>)> = None;
    // a sheet without an id column (slab layers) puts its kept cells in itself
    if baseline::kept_on(name) && cols.first().map(|c| c.key) == Some("id") {
        let (mut br, mut bx) = (rows.clone(), extra.to_vec());
        baseline::overlay(name, cols, &mut br, &mut bx, false);
        base_view = Some((br, bx));
        let mut x = extra.to_vec();
        baseline::overlay(name, cols, &mut rows, &mut x, true);
        book_x = Some(x);
    }
    let extra: &[XCol] = book_x.as_deref().unwrap_or(extra);
    let ws = wb.add_worksheet();
    ws.set_name(name).map_err(err)?;
    // rows the user keeps above the header stay there; the table starts below them
    let top = extra::write_titles(ws, &extra::titles_of(name)).map_err(err)?;
    let styled = book::active();
    let grey = Format::new()
        .set_font_color(Color::RGB(0x666666))
        .set_background_color(Color::RGB(0xEEEEEE));
    let open = Format::new().set_unlocked();
    // text columns take the Text number format: a typed 007 or 1.10 stays text in the book
    let open_t = open.clone().set_num_format_index(49);
    let text_col = |x: &XCol| {
        x.edit
            && x.cells.iter().any(|v| matches!(v, V::S(_)))
            && x.cells.iter().all(|v| matches!(v, V::S(_) | V::Blank))
    };
    let head_ro = grey.clone().set_bold();
    let head_rw = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xDDEBF7));
    let mut captions: Vec<String> = Vec::with_capacity(cols.len() + extra.len());
    for (j, col) in cols.iter().enumerate() {
        let f = if col.edit { &head_rw } else { &head_ro };
        let header = if styled {
            book::header(col.key, col.header, u)
        } else {
            u.header(col.header)
        };
        captions.push(header.clone());
        ws.write_string_with_format(top, j as u16, &header, f)
            .map_err(err)?;
        let w = header.chars().count() as f64 + 4.0;
        let w = if styled { w.clamp(10.0, 40.0) } else { w.max(12.0) };
        ws.set_column_width(j as u16, w).map_err(err)?;
        if styled && !col.edit && name != QUANTITIES && name != SUMMARY {
            ws.insert_note(top, j as u16, &Note::new(READ_ONLY_NOTE).set_width(360))
                .map_err(err)?;
        }
        if col.edit {
            let cf = if super::textcode::is_text_key(col.key) { &open_t } else { &open };
            ws.set_column_format(j as u16, cf).map_err(err)?;
        }
    }
    let n = cols.len() as u16;
    for (j, x) in extra.iter().enumerate() {
        let c = n + j as u16;
        let f = if x.edit { &head_rw } else { &head_ro };
        captions.push(x.header.clone());
        ws.write_string_with_format(top, c, &x.header, f)
            .map_err(err)?;
        ws.set_column_width(c, (x.header.chars().count() as f64 + 4.0).max(12.0))
            .map_err(err)?;
        if x.edit {
            ws.set_column_format(c, if text_col(x) { &open_t } else { &open }).map_err(err)?;
        }
    }
    let last = n + extra.len() as u16;
    ws.set_column_range_format(last, last + SPARE_COLS - 1, &open)
        .map_err(err)?;
    if name == QUANTITIES || name == SUMMARY {
        ws.insert_note(top, 0, &Note::new(READ_ONLY_NOTE).set_width(360))
            .map_err(err)?;
        ws.set_tab_color(Color::RGB(0xA6A6A6));
    }
    // display as in the app: areas 2 decimals, volumes 3 (the cell value is full precision)
    let shown = |f: &Format, h: &str| match () {
        _ if h.contains("[m3]") || h.contains("[ft3]") => f.clone().set_num_format("0.000"),
        _ if h.contains("[m2]") || h.contains("[ft2]") => f.clone().set_num_format("0.00"),
        _ if styled && h.contains("[m]") => f.clone().set_num_format("0.000"),
        _ if styled && h.contains("[ft]") => f.clone().set_num_format("0.00"),
        _ => f.clone(),
    };
    let shown_key = |f: &Format, c: &Col| match c.key {
        "id" | "pos" | "wtype" | "stype" | "wall" if styled => f.clone().set_num_format("0"),
        "bulge" if styled => f.clone().set_num_format("0.0000"),
        _ => shown(f, c.header),
    };
    let num_fmt: Vec<(Format, Format)> = cols
        .iter()
        .map(|c| (shown_key(&open, c), shown_key(&grey, c)))
        .collect();
    let tf = |j: usize| if super::textcode::is_text_key(cols[j].key) { &open_t } else { &open };
    let forms = super::sheet_view::has_formulas(name);
    for (i, row) in rows.iter().enumerate() {
        gate_rows(i, rows.len())?;
        for (j, v) in row.iter().enumerate() {
            let (r, c) = (top + i as u32 + 1, j as u16);
            let k = u.factor(cols[j].header);
            let (nopen, ngrey) = &num_fmt[j];
            let id = match row.first() {
                Some(V::N(x)) if x.is_finite() => Some(*x as i64),
                _ => None,
            };
            // a formula the user put in this app cell stays while its result is the model value
            let live = forms.then(|| super::sheet_view::live_formula(name, cols, cols[j].key, row, r + 1, k, v));
            if let Some((f, res)) = live.flatten() {
                let fmt = match (v, cols[j].edit) {
                    (V::S(_) | V::Blank, true) => tf(j),
                    (_, true) => nopen,
                    (V::S(_) | V::Blank, false) => &grey,
                    (_, false) => ngrey,
                };
                ws.write_formula_with_format(r, c, Formula::new(f).set_result(res), fmt).map_err(err)?;
                continue;
            }
            if let Some(done) = super::cellsafe::write_safe(ws, r, c, v, id) {
                done.map_err(err)?;
                continue;
            }
            match (v, cols[j].edit) {
                (V::N(x), true) => ws.write_number_with_format(r, c, *x / k, nopen).map(|_| ()),
                (V::N(x), false) => ws.write_number_with_format(r, c, *x / k, ngrey).map(|_| ()),
                (V::S(s), true) => ws.write_string_with_format(r, c, s, tf(j)).map(|_| ()),
                (V::S(s), false) => ws.write_string_with_format(r, c, s, &grey).map(|_| ()),
                (V::Blank, false) => ws.write_blank(r, c, &grey).map(|_| ()),
                (V::Blank, true) => ws.write_blank(r, c, tf(j)).map(|_| ()),
                (V::F(f, res), _) => ws
                    .write_formula_with_format(r, c, super::extra::formula(f, res), &open)
                    .map(|_| ()),
                (v, edit) => extra::write_v(ws, r, c, v, Some(if edit { &open } else { &grey })),
            }
            .map_err(err)?;
        }
    }
    for (j, x) in extra.iter().enumerate() {
        let c = n + j as u16;
        let fmt = if text_col(x) {
            open_t.clone()
        } else {
            shown(if x.edit { &open } else { &grey }, &x.header)
        };
        for (i, v) in x.cells.iter().enumerate() {
            let r = top + i as u32 + 1;
            let id = match rows.get(i).and_then(|row| row.first()) {
                Some(V::N(x)) if x.is_finite() => Some(*x as i64),
                _ => None,
            };
            if let Some(done) = super::cellsafe::write_safe(ws, r, c, v, id) {
                done.map_err(err)?;
                continue;
            }
            extra::write_v(ws, r, c, v, Some(&fmt)).map_err(err)?;
        }
    }
    if styled {
        for (j, col) in cols.iter().enumerate() {
            if col.edit {
                book::validation(ws, name, col.key, j as u16, rows.len())?;
            }
        }
        for (j, x) in extra.iter().enumerate() {
            if x.edit {
                book::validation(ws, name, &x.header, n + j as u16, rows.len())?;
            }
        }
        book::record_keys(name, cols.iter().map(|c| c.key.to_string()).collect(), captions.clone());
    }
    // hidden rows and columns, widths and heights the user set in the old book
    super::sheet_view::apply(ws, name, cols, &captions, extra, top, &rows)?;
    ws.protect_with_options(&ProtectionOptions {
        use_autofilter: true,
        format_columns: true,
        insert_rows: true,
        insert_columns: true,
        ..ProtectionOptions::default()
    });
    ws.set_freeze_panes(top + 1, 0).map_err(err)?;
    ws.autofilter(top, 0, top + rows.len() as u32, last - 1)
        .map_err(err)?;
    match &base_view {
        Some((r, x)) => baseline::record(name, cols, r, x),
        None => baseline::record(name, cols, &rows, extra),
    }
    Ok(())
}

/// Outer slab outlines (ring 0), m.
pub(crate) fn outer_rings(model: &Model) -> Result<HashMap<i64, Vec<(f64, f64)>>, String> {
    let mut st = model
        .sql()
        .prepare("SELECT slab,x,y FROM slab_vertex WHERE ring=0 ORDER BY slab,idx")
        .map_err(err)?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?))))
        .map_err(err)?;
    let mut out: HashMap<i64, Vec<(f64, f64)>> = HashMap::new();
    for r in rows {
        let (id, p) = r.map_err(err)?;
        out.entry(id).or_default().push(p);
    }
    Ok(out)
}

/// `x y; x y; ...` in workbook units (`k` — metres per unit).
pub(crate) fn ring_text(ring: &[(f64, f64)], k: f64) -> String {
    ring.iter()
        .map(|p| format!("{} {}", p.0 / k, p.1 / k))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn export_xlsx(model: &Model, path: &Path) -> Result<(), String> {
    export_xlsx_units(model, path, Units::Metric)
}

/// Workbook in project units; read-only columns grey, sheets protected (no password).
/// Overwriting our own workbook preserves the user's columns (by row id) and their sheets.
pub fn export_xlsx_units(model: &Model, path: &Path, u: Units) -> Result<(), String> {
    export_xlsx_families(model, path, u, &[])
}

/// Like `export_xlsx_units`, plus one sheet per family in `fams`.
pub fn export_xlsx_families(
    model: &Model,
    path: &Path,
    u: Units,
    fams: &[super::family::FamilySheet],
) -> Result<(), String> {
    export_xlsx_families_from(model, path, path, u, fams)
}

/// Like `export_xlsx_families`, writing `path` while keeping the user columns and sheets of
/// `from` (a refreshed copy beside a book another program holds open).
pub fn export_xlsx_families_from(
    model: &Model,
    from: &Path,
    path: &Path,
    u: Units,
    fams: &[super::family::FamilySheet],
) -> Result<(), String> {
    export_xlsx_styled(model, from, path, u, fams, &book::Style::default())
}

/// Like `export_xlsx_families_from`, dressed per `style`: a professional style gives captions in
/// the project language, a hidden key row the import reads, dropdowns and number formats.
pub fn export_xlsx_styled(
    model: &Model,
    from: &Path,
    path: &Path,
    u: Units,
    fams: &[super::family::FamilySheet],
    style: &book::Style,
) -> Result<(), String> {
    export_xlsx_hooked(model, from, path, u, fams, style, None)
}

const SHEETS_FIXED: usize = 13;

/// Progress/cancel hook: gets the written fraction 0..1 at each sheet start and every
/// `GATE_ROWS` rows; returning false stops the export with `CANCELLED` and writes nothing.
pub type Hook = std::sync::Arc<dyn Fn(f32) -> bool + Send + Sync>;

/// Error text of an export stopped by its hook.
pub const CANCELLED: &str = "export cancelled";

const GATE_ROWS: usize = 2000;

struct Gate {
    hook: Hook,
    sheets: f32,
    done: f32,
}

thread_local! {
    static GATE: std::cell::RefCell<Option<Gate>> = const { std::cell::RefCell::new(None) };
}

struct GateGuard;

impl Drop for GateGuard {
    fn drop(&mut self) {
        GATE.with(|g| *g.borrow_mut() = None);
        // other writers on this thread see the layer keys as they are
        super::layer_match::begin(Path::new(""), false);
    }
}

fn gate_at(sheet_part: f32, advance: bool) -> Result<(), String> {
    GATE.with(|g| match g.borrow_mut().as_mut() {
        None => Ok(()),
        Some(g) => {
            if advance {
                g.done += 1.0;
            }
            let f = ((g.done - 1.0 + sheet_part) / g.sheets).clamp(0.0, 0.99);
            if (g.hook)(f) {
                Ok(())
            } else {
                Err(CANCELLED.to_string())
            }
        }
    })
}

/// At the start of each sheet.
pub(crate) fn gate_sheet() -> Result<(), String> {
    gate_at(0.0, true)
}

/// In a row loop: asks the hook every `GATE_ROWS` rows.
pub(crate) fn gate_rows(i: usize, n: usize) -> Result<(), String> {
    if i > 0 && i % GATE_ROWS == 0 {
        gate_at(i as f32 / n.max(1) as f32, false)?;
    }
    Ok(())
}

const LAYERS_SQL: &str = "SELECT l.wtype,l.pos,t.name,l.material,l.thickness FROM wall_type_layer l \
     JOIN wall_type t ON t.id=l.wtype ORDER BY l.wtype,l.pos";
const SLAB_LAYERS_SQL: &str = "SELECT l.stype,l.pos,t.name,l.material,l.thickness FROM slab_type_layer l \
     JOIN slab_type t ON t.id=l.stype ORDER BY l.stype,l.pos";

/// Layer sheets (Layers, SlabLayers): rows keyed by (type, pos), so the Baseline, the kept
/// cells and the user's own columns use `layer_key`.
fn layer_sheet(
    wb: &mut Workbook,
    model: &Model,
    u: Units,
    (name, cols): (&str, &[Col]),
    sql: &str,
    kept: &Kept,
) -> Result<(), String> {
    let mut rows = query(model, sql, 5)?;
    let int = |v: &V| match v {
        V::N(x) => *x as i64,
        _ => 0,
    };
    let mut base = Vec::with_capacity(rows.len());
    for r in &mut rows {
        let id = layer_key(int(&r[0]), int(&r[1]));
        let mut b = r.clone();
        for (j, c) in cols.iter().enumerate() {
            if let Some((book, was)) = baseline::kept_cell(name, id, c.key) {
                r[j] = baseline::bval_v(&book);
                b[j] = baseline::bval_v(&was);
            }
        }
        base.push((id, b));
    }
    // user cells and row marks follow their own layer when layers moved since the old book
    super::layer_match::settle(name, cols, &base);
    let keys_of_rows: Vec<i64> = base.iter().map(|b| b.0).collect();
    let extra = kept.keyed_cols(name, &keys_of_rows);
    sheet(wb, name, cols, rows, u, &extra)?;
    let keys = cols.iter().map(|c| c.key.to_string()).collect();
    baseline::record_raw(name, keys, base);
    Ok(())
}

/// `export_xlsx_styled` with an optional progress/cancel hook; nothing is saved when it stops.
pub fn export_xlsx_hooked(
    model: &Model,
    from: &Path,
    path: &Path,
    u: Units,
    fams: &[super::family::FamilySheet],
    style: &book::Style,
    hook: Option<Hook>,
) -> Result<(), String> {
    let _guard = GateGuard;
    // a parameter sheet the caller writes as a family sheet gets the parameter columns there;
    // a scoped book carries only the scope's categories
    let whole = style.scope.as_ref().map_or(true, |s| s.is_whole());
    let own_params: Vec<&ParamSheet> = PARAM_SHEETS
        .iter()
        .filter(|p| whole && !fams.iter().any(|f| f.sheet == p.sheet))
        .collect();
    GATE.with(|g| {
        *g.borrow_mut() = hook.map(|hook| Gate {
            hook,
            sheets: (SHEETS_FIXED + own_params.len() + fams.len()) as f32,
            done: 0.0,
        })
    });
    book::begin(model, style);
    extra::begin_notes();
    super::cellsafe::begin();
    super::sheet_view::begin(from);
    let mut kept = Kept::read(from, fams)?;
    kept.share_titles();
    kept.forget_sheets(fams.iter().map(|f| f.sheet));
    kept.forget_sheets(std::iter::once(super::value_sources::SHEET));
    let layered = [LAYERS, SLAB_LAYERS].into_iter().any(|s| kept.has_cols(s) || super::sheet_view::marks_rows(s));
    super::layer_match::begin(from, layered);
    let x = |sheet: &str, rows: &Rows| extra::columns(model, sheet, &ids(rows), u, &kept);
    baseline::begin();
    let allowed = match &style.scope {
        Some(s) => scope::resolve(model, s)?,
        None => scope::Allowed::default(),
    };
    let mut wb = Workbook::new();
    let levels = query(model, "SELECT id,name,elevation FROM level ORDER BY id", 3)?;
    let lvx = kept.keyed_cols(LEVELS, &ids(&levels));
    sheet(&mut wb, LEVELS, LEVELS_COLS, levels, u, &lvx)?;
    let types = query(model, TYPES_SQL, 4)?;
    let tx = x(TYPES, &types)?;
    sheet(&mut wb, TYPES, TYPES_COLS, types, u, &tx)?;
    layer_sheet(&mut wb, model, u, (LAYERS, LAYERS_COLS), LAYERS_SQL, &kept)?;
    // SlabTypes is a parameter sheet (written with them); its layers go here
    layer_sheet(&mut wb, model, u, (SLAB_LAYERS, SLAB_LAYERS_COLS), SLAB_LAYERS_SQL, &kept)?;
    // colour as #RRGGBB text, empty when the material follows the theme class colour
    let mats = query(
        model,
        "SELECT id,name,CASE WHEN color IS NULL THEN NULL ELSE '#'||printf('%06X',color) END,\
         hatch,density,conductivity,specific_heat,note FROM material ORDER BY id",
        8,
    )?;
    let mut mats = mats;
    mats.extend(rowless_materials(model)?);
    sheet(&mut wb, MATERIALS, MATERIALS_COLS, mats, u, &[])?;
    let walls = query(
        model,
        "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height,w.offset0,w.bulge \
         FROM wall w JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        11,
    )?;
    let mut walls = scope::keep(walls, &allowed.walls);
    for r in &mut walls {
        let g = |k: usize| if let V::N(x) = r[k] { x } else { 0.0 };
        let len =
            ddc_geom::arc::axis_length(ddc_geom::v2(g(3), g(4)), ddc_geom::v2(g(5), g(6)), g(10));
        r.push(V::N(len));
        r.push(V::Blank);
    }
    let wx = x(WALLS, &walls)?;
    sheet(&mut wb, WALLS, WALLS_COLS, walls, u, &wx)?;
    let ops = query(
        model,
        "SELECT id,wall,along,width,sill,height,kind,\
         CASE swing_side WHEN 1 THEN 'right' ELSE 'left' END,\
         CASE swing_hinge WHEN 1 THEN 'end' ELSE 'start' END FROM opening ORDER BY id",
        9,
    )?;
    let mut ops = scope::keep(ops, &allowed.openings);
    ops.iter_mut().for_each(|r| r.push(V::Blank));
    let ox = x(OPENINGS, &ops)?;
    sheet(&mut wb, OPENINGS, OPENINGS_COLS, ops, u, &ox)?;
    let q = query(
        model,
        "SELECT wall,pos,material,plan_area,volume FROM wall_qty ORDER BY wall,pos",
        5,
    )?;
    let q = scope::keep(q, &allowed.walls);
    sheet(&mut wb, QUANTITIES, QUANTITIES_COLS, q, u, &[])?;
    let rooms = query(
        model,
        "SELECT r.id,lv.name,r.name,r.number,r.px,r.py,r.height,q.area,q.perimeter,q.volume \
         FROM room r JOIN level lv ON lv.id=r.level LEFT JOIN room_qty q ON q.room=r.id \
         ORDER BY r.id",
        10,
    )?;
    let rooms = scope::keep(rooms, &allowed.rooms)
        .into_iter()
        .map(|mut r| {
            r.push(V::Blank);
            r
        })
        .collect();
    let rx = x(ROOMS, &rooms)?;
    sheet(&mut wb, ROOMS, ROOMS_COLS, rooms, u, &rx)?;
    let slabs = query(
        model,
        "SELECT s.id,lv.name,t.name,s.offset,MAX(q.area),SUM(q.volume) FROM slab s \
         JOIN level lv ON lv.id=s.level JOIN slab_type t ON t.id=s.stype \
         LEFT JOIN slab_qty q ON q.slab=s.id GROUP BY s.id ORDER BY s.id",
        6,
    )?;
    let outline = outer_rings(model)?;
    let lk = u.factor("[m]");
    // plan area, then the sloped top face area (= plan when flat), then volume
    let slabs = scope::keep(slabs, &allowed.slabs)
        .into_iter()
        .map(|mut r| {
            let id = match r[0] {
                V::N(v) => v as i64,
                _ => 0,
            };
            let f = model
                .slab_slope(id)
                .ok()
                .flatten()
                .map_or(1.0, |p| p.area_factor());
            let surf = match r[4] {
                V::N(a) => V::N(a * f),
                _ => V::Blank,
            };
            r.insert(5, surf);
            r.insert(
                4,
                V::S(outline.get(&id).map_or(String::new(), |o| ring_text(o, lk))),
            );
            r.push(V::Blank);
            r
        })
        .collect();
    let sx = x(SLABS, &slabs)?;
    sheet(&mut wb, SLABS, SLABS_COLS, slabs, u, &sx)?;
    let cws = query(
        model,
        "SELECT c.id,lv.name,c.name,c.ax,c.ay,c.bx,c.by,c.bulge,c.base_offset,c.height,         c.ugrid,c.vgrid FROM curtain_wall c JOIN level lv ON lv.id=c.level ORDER BY c.id",
        12,
    )?;
    let mut cws = scope::keep(cws, &allowed.curtains);
    for r in &mut cws {
        let id = match r[0] {
            V::N(v) => v as i64,
            _ => 0,
        };
        let q = model.curtain_quantities(id).map_err(err)?;
        r.extend([
            V::N(q.gross_area),
            V::N(q.glass_area),
            V::N(q.panels as f64),
            V::N(q.mullion_length),
            V::Blank,
        ]);
    }
    let cx = x(CURTAINS, &cws)?;
    sheet(&mut wb, CURTAINS, CURTAINS_COLS, cws, u, &cx)?;
    for ps in &own_params {
        let rows = query(model, ps.sql, ps.cols.len())?;
        let px = x(ps.sheet, &rows)?;
        sheet(&mut wb, ps.sheet, ps.cols, rows, u, &px)?;
    }
    for fam in fams {
        super::family::write_sheet(&mut wb, model, fam, u, &kept)?;
    }
    // Walls: area = plan footprint of all layers; openings: width x height; rooms: floor area.
    let summary = query(model, SUMMARY_SQL, 6)?;
    sheet(&mut wb, SUMMARY, SUMMARY_COLS, summary, u, &[])?;
    let ws = wb.add_worksheet();
    ws.set_name(INFO).map_err(err)?;
    ws.write_string(0, 0, "units").map_err(err)?;
    ws.write_string(0, 1, u.name()).map_err(err)?;
    ws.write_string(1, 0, "revision").map_err(err)?;
    ws.write_number(1, 1, model.change_head() as f64).map_err(err)?;
    let mut next = 2;
    if let Some(s) = style.scope.as_ref().filter(|s| !s.is_whole()) {
        ws.write_string(2, 0, "scope").map_err(err)?;
        ws.write_string(2, 1, &s.label).map_err(err)?;
        ws.write_string(3, 0, "scope_spec").map_err(err)?;
        ws.write_string(3, 1, &s.to_spec()).map_err(err)?;
        next = 4;
    }
    // names the project the book belongs to: an export over a book is checked against it
    if let Ok(uid) = model.sql().query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| r.get::<_, i64>(0)) {
        ws.write_string(next, 0, "project").map_err(err)?;
        ws.write_string(next, 1, &uid.to_string()).map_err(err)?;
        next += 1;
    }
    // separator for text numbers like 1.250: import resolves them with it (old books lack the rows)
    // a named locale decides the mark; an unknown one writes no row (import stays ambiguous)
    let mark = if style.locale.trim().is_empty() {
        Some(if style.decimal_comma { "comma" } else { "point" })
    } else {
        match units::dec_from_info(std::iter::once(("locale", style.locale.trim()))) {
            Some(units::Dec::Comma) => Some("comma"),
            Some(units::Dec::Point) => Some("point"),
            None => None,
        }
    };
    if let Some(m) = mark {
        ws.write_string(next, 0, "decimal").map_err(err)?;
        ws.write_string(next, 1, m).map_err(err)?;
        next += 1;
    }
    if !style.locale.trim().is_empty() {
        ws.write_string(next, 0, "locale").map_err(err)?;
        ws.write_string(next, 1, style.locale.trim()).map_err(err)?;
        next += 1;
    }
    // an import without the baseline names a missing sheet only if this export wrote it
    ws.write_string(next, 0, super::no_base::SHEETS_ROW).map_err(err)?;
    ws.write_string(next, 1, &super::no_base::WRITTEN.join(",")).map_err(err)?;
    // the company logo floats beside the key/value rows: no cell moves, import never sees it
    if let Ok(Some(png)) = model.project_logo() {
        put_logo(ws, 3, &png)?;
    }
    gate_at(1.0, false)?;
    book::finish(&mut wb)?;
    baseline::write(&mut wb)?;
    kept.write_others(&mut wb)?;
    if style.sources {
        super::value_sources::write(model, &mut wb)?;
    }
    super::measure::write(&mut wb, model)?;
    crate::fsutil::replace_durably(path, |p| wb.save(p).map_err(err))
}

/// Table export cell as in the app schedule: a number (in header units) or text.
#[derive(Clone, Debug, PartialEq)]
pub enum TableCell {
    Num(f64),
    Text(String),
    /// Totals row only: `SUBTOTAL(func, column range of the data rows)` with the cached value
    /// (109 sum, 103 count), so the total follows edits and filtering in the spreadsheet.
    Subtotal(u8, f64),
    /// Totals row only: `pre & SUBTOTAL(103, range) & post` with the cached text for `n` rows.
    CountLabel { pre: String, post: String, n: usize },
}

/// Count label formula (`pre`/`post` already quote-escaped): rows of the filtered data range
/// that hold at least one non-empty cell in any column, so a blank first column does not
/// undercount. `SUBTOTAL(103, OFFSET(..))` per row keeps the filter behaviour.
pub(crate) fn count_label_formula(pre: &str, post: &str, ncols: u32, nrows: u32) -> String {
    let last = nrows + 1;
    format!(
        "\"{pre}\"&SUMPRODUCT(--(SUBTOTAL(103,OFFSET($A$2,ROW($A$2:$A${last})-ROW($A$2),0,1,{}))>0))&\"{post}\"",
        ncols.max(1)
    )
}

/// One displayed line of a grouped table sheet.
#[derive(Clone, Debug, PartialEq)]
pub enum TableLine {
    /// Index into the `rows` of the table.
    Row(usize),
    /// Group header text (already indented/counted by the caller).
    Head { text: String },
    /// Closes the innermost open group: its `Subtotal` cells sum that group's rows only.
    Sub { cells: Vec<TableCell> },
}

/// Count label over the data range that skips the group rows (`skip` = 1-based sheet rows).
pub(crate) fn count_label_formula_skipping(
    pre: &str,
    post: &str,
    ncols: u32,
    nrows: u32,
    skip: &[u32],
) -> String {
    let last = nrows + 1;
    let list: Vec<String> = skip.iter().map(|r| r.to_string()).collect();
    format!(
        "\"{pre}\"&SUMPRODUCT(--(SUBTOTAL(103,OFFSET($A$2,ROW($A$2:$A${last})-ROW($A$2),0,1,{}))>0),--ISNA(MATCH(ROW($A$2:$A${last}),{{{}}},0)))&\"{post}\"",
        ncols.max(1),
        list.join(",")
    )
}

/// One table per sheet: headers, `formats[j]` — the column's number format ("" — general),
/// rows; the last row `totals` — totals in bold. Filter and frozen header.
pub fn export_table(
    path: &Path,
    sheet: &str,
    headers: &[String],
    formats: &[&str],
    rows: &[Vec<TableCell>],
    totals: Option<&[TableCell]>,
) -> Result<(), String> {
    let lines: Vec<TableLine> = (0..rows.len()).map(TableLine::Row).collect();
    export_table_lines(path, sheet, headers, formats, rows, &lines, totals)
}

/// `export_table` with group header and subtotal lines between the element rows. Group lines
/// are marked by `ddc_group_row_N` names so a re-import skips them; the grand total stays a
/// SUBTOTAL over the whole range (nested SUBTOTALs are ignored by the spreadsheet, no double count).
pub fn export_table_lines(
    path: &Path,
    sheet: &str,
    headers: &[String],
    formats: &[&str],
    rows: &[Vec<TableCell>],
    lines: &[TableLine],
    totals: Option<&[TableCell]>,
) -> Result<(), String> {
    table_lines_in(path, sheet, headers, formats, rows, lines, totals, false, None)
}

/// [`export_table_lines`] with column A drawn grey: a key column (element Id) not meant for edits.
pub fn export_keyed_table_lines(
    path: &Path,
    sheet: &str,
    headers: &[String],
    formats: &[&str],
    rows: &[Vec<TableCell>],
    lines: &[TableLine],
    totals: Option<&[TableCell]>,
) -> Result<(), String> {
    table_lines_in(path, sheet, headers, formats, rows, lines, totals, true, None)
}

/// Hidden sheet of a keyed table book: what was exported, so an import can tell a book edit
/// from a model edit made after the export.
pub const KEY_SHEET: &str = "ddc_schedule_key";

/// [`export_keyed_table_lines`] plus the hidden [`KEY_SHEET`] grid `key` (from A1, as given).
#[allow(clippy::too_many_arguments)]
pub fn export_keyed_table_lines_with_key(
    path: &Path,
    sheet: &str,
    headers: &[String],
    formats: &[&str],
    rows: &[Vec<TableCell>],
    lines: &[TableLine],
    totals: Option<&[TableCell]>,
    key: &[Vec<TableCell>],
) -> Result<(), String> {
    table_lines_in(path, sheet, headers, formats, rows, lines, totals, true, Some(key))
}

#[allow(clippy::too_many_arguments)]
fn table_lines_in(
    path: &Path,
    sheet: &str,
    headers: &[String],
    formats: &[&str],
    rows: &[Vec<TableCell>],
    lines: &[TableLine],
    totals: Option<&[TableCell]>,
    keyed: bool,
    key: Option<&[Vec<TableCell>]>,
) -> Result<(), String> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name(sheet).map_err(err)?;
    let head = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xDDEBF7));
    let grey = |f: Format| {
        f.set_font_color(Color::RGB(0x666666))
            .set_background_color(Color::RGB(0xEEEEEE))
    };
    let mut fmt: Vec<Format> = (0..headers.len())
        .map(|j| match formats.get(j).copied().unwrap_or("") {
            "" => Format::new(),
            f => Format::new().set_num_format(f),
        })
        .collect();
    if keyed {
        if let Some(f) = fmt.first_mut() {
            *f = grey(f.clone());
        }
    }
    let head_key = grey(Format::new().set_bold());
    for (j, h) in headers.iter().enumerate() {
        let f = if keyed && j == 0 { &head_key } else { &head };
        ws.write_string_with_format(0, j as u16, h, f)
            .map_err(err)?;
        ws.set_column_width(j as u16, (h.chars().count() as f64 + 4.0).max(10.0))
            .map_err(err)?;
    }
    let n = lines.len() as u32;
    // 1-based sheet rows of the header and subtotal lines
    let group_rows: Vec<u32> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !matches!(l, TableLine::Row(_)))
        .map(|(i, _)| i as u32 + 2)
        .collect();
    let put = |ws: &mut rust_xlsxwriter::Worksheet,
               r: u32,
               row: &[TableCell],
               bold: bool,
               span: (u32, u32)| {
        for (j, v) in row.iter().enumerate() {
            let c = j as u16;
            let col = rust_xlsxwriter::utility::column_number_to_name(c);
            let range = format!("{col}{}:{col}{}", span.0, span.1);
            let f = if bold {
                fmt[j].clone().set_bold()
            } else {
                fmt[j].clone()
            };
            match v {
                TableCell::Num(x) if x.is_finite() => ws.write_number_with_format(r, c, *x, &f),
                TableCell::Num(_) => ws.write_blank(r, c, &f),
                TableCell::Text(s) => ws.write_string_with_format(r, c, s, &f),
                // no data rows: the range would include the totals cell itself
                TableCell::Subtotal(_, x) if n == 0 => {
                    ws.write_number_with_format(r, c, if x.is_finite() { *x } else { 0.0 }, &f)
                }
                TableCell::Subtotal(func, x) => {
                    let fm = Formula::new(format!("SUBTOTAL({func},{range})"))
                        .set_result(if x.is_finite() { x.to_string() } else { "0".into() });
                    ws.write_formula_with_format(r, c, fm, &f)
                }
                TableCell::CountLabel { pre, post, n: cnt } if n > 0 => {
                    let q = |t: &str| t.replace('"', "\"\"");
                    let formula = if group_rows.is_empty() {
                        count_label_formula(&q(pre), &q(post), headers.len() as u32, n)
                    } else {
                        count_label_formula_skipping(
                            &q(pre),
                            &q(post),
                            headers.len() as u32,
                            n,
                            &group_rows,
                        )
                    };
                    if formula.len() > 8000 {
                        // too many groups for one formula: a static label is still correct
                        ws.write_string_with_format(r, c, format!("{pre}{cnt}{post}"), &f)
                    } else {
                        let fm = Formula::new(formula).set_result(format!("{pre}{cnt}{post}"));
                        ws.write_formula_with_format(r, c, fm, &f)
                    }
                }
                TableCell::CountLabel { pre, post, n: cnt } => {
                    ws.write_string_with_format(r, c, format!("{pre}{cnt}{post}"), &f)
                }
            }
            .map_err(err)?;
        }
        Ok::<(), String>(())
    };
    let ghead = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xF2F2F2));
    let mut open: Vec<u32> = vec![];
    let ncols = headers.len() as u16;
    for (i, line) in lines.iter().enumerate() {
        let r = i as u32 + 1;
        match line {
            TableLine::Row(k) => put(ws, r, &rows[*k], false, (2, n + 1))?,
            TableLine::Head { text } => {
                ws.write_string_with_format(r, 0, text, &ghead).map_err(err)?;
                for c in 1..ncols {
                    ws.write_blank(r, c, &ghead).map_err(err)?;
                }
                open.push(r + 2);
            }
            TableLine::Sub { cells } => {
                let start = open.pop().unwrap_or(2);
                put(ws, r, cells, true, (start, r))?;
            }
        }
    }
    if let Some(t) = totals {
        put(ws, lines.len() as u32 + 1, t, true, (2, n + 1))?;
    }
    ws.set_freeze_panes(1, 0).map_err(err)?;
    if !headers.is_empty() {
        // the totals row stays outside the filter range, also for an empty table
        let last = if totals.is_some() { n } else { n.max(1) };
        ws.autofilter(0, 0, last, headers.len() as u16 - 1).map_err(err)?;
    }
    let q = sheet.replace('\'', "''");
    if !headers.is_empty() {
        let end = rust_xlsxwriter::utility::column_number_to_name(headers.len() as u16 - 1);
        for (k, g) in group_rows.iter().enumerate() {
            wb.define_name(
                format!("{}{k}", super::book::GROUP_ROW),
                &format!("='{q}'!$A${g}:${end}${g}"),
            )
            .map_err(err)?;
        }
    }
    if let Some(t) = totals.filter(|t| !t.is_empty()) {
        let r = n + 2;
        let end = rust_xlsxwriter::utility::column_number_to_name(t.len() as u16 - 1);
        wb.define_name(super::book::TOTAL_ROW, &format!("='{q}'!$A${r}:${end}${r}"))
            .map_err(err)?;
    }
    if let Some(key) = key {
        let ks = wb.add_worksheet();
        ks.set_name(KEY_SHEET).map_err(err)?;
        for (r, row) in key.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let (r, c) = (r as u32, c as u16);
                match v {
                    TableCell::Num(x) if x.is_finite() => {
                        ks.write_number(r, c, *x).map_err(err)?;
                    }
                    TableCell::Text(s) if !s.is_empty() => {
                        ks.write_string(r, c, s).map_err(err)?;
                    }
                    _ => {}
                }
            }
        }
        ks.set_hidden(true);
    }
    crate::fsutil::replace_durably(path, |p| wb.save(p).map_err(err))
}

#[cfg(test)]
mod count_label_tests {
    use super::*;
    use std::io::Read;

    fn sheet_xml(p: &Path) -> String {
        let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name("xl/worksheets/sheet1.xml").unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn write(name: &str, rows: &[Vec<TableCell>], n: usize) -> String {
        let p = std::env::temp_dir().join(name);
        let heads: Vec<String> = vec!["a".into(), "b".into()];
        let tot = vec![
            TableCell::CountLabel { pre: "Total: ".into(), post: String::new(), n },
            TableCell::Text(String::new()),
        ];
        export_table(&p, "S", &heads, &[], rows, Some(&tot)).unwrap();
        let x = sheet_xml(&p);
        let _ = std::fs::remove_file(&p);
        x
    }

    #[test]
    fn schedule_count_label_ignores_blank_first_column() {
        let rows: Vec<Vec<TableCell>> = (0..3)
            .map(|i| vec![TableCell::Text(String::new()), TableCell::Num(i as f64)])
            .collect();
        let x = write("ddc_cl_blank.xlsx", &rows, 3);
        // the count is row-wise over all columns, never over the first column alone
        assert!(x.contains("SUMPRODUCT"), "{x}");
        assert!(x.contains("OFFSET($A$2,ROW($A$2:$A$4)-ROW($A$2),0,1,2)"), "{x}");
        assert!(!x.contains("SUBTOTAL(103,A2:A4)"), "{x}");
    }

    #[test]
    fn schedule_count_label_cached_matches_ui() {
        let rows: Vec<Vec<TableCell>> = (0..4)
            .map(|i| vec![TableCell::Text(String::new()), TableCell::Num(i as f64)])
            .collect();
        let x = write("ddc_cl_cached.xlsx", &rows, 4);
        assert!(x.contains("Total: 4"), "{x}");
    }
}

#[cfg(test)]
mod logo_tests {
    use super::*;

    fn media(p: &Path) -> Vec<String> {
        let z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
        z.file_names()
            .map(|n| n.unwrap().into_owned())
            .filter(|n| n.starts_with("xl/media/"))
            .collect()
    }

    #[test]
    fn project_book_carries_the_logo_only_when_the_project_has_one() {
        let mut m = Model::open(":memory:").unwrap();
        let dir = std::env::temp_dir();
        let plain = dir.join(format!("ddc_logo_x0_{}.xlsx", std::process::id()));
        export_xlsx(&m, &plain).unwrap();
        assert!(media(&plain).is_empty());

        let px = (0..40 * 20).flat_map(|_| [10u8, 80, 150, 255]).collect();
        let png = crate::logo::Logo::from_rgba(40, 20, px).unwrap().to_png();
        m.set_project_logo(Some(&png)).unwrap();
        let with = dir.join(format!("ddc_logo_x1_{}.xlsx", std::process::id()));
        export_xlsx(&m, &with).unwrap();
        assert_eq!(media(&with).len(), 1, "{:?}", media(&with));
        for f in [plain, with] {
            let _ = std::fs::remove_file(f);
        }
    }
}
