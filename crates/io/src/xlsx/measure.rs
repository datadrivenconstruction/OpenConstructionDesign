//! Sheet `Measures`: the saved measurements of the project, one row each (id, view, kind,
//! value, unit, state, reason). Values come from the model's own evaluator, in SI with the unit
//! named per row. A measurement with no number shows its state and reason, never a zero. Only a
//! driving measurement's value is editable (it moves geometry through the solver); a changed
//! value on any other row is reported for that row and changes nothing. State `driving` marks
//! the editable rows. The sheet is written
//! only when the project has measurements.

use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::Model;
use rust_xlsxwriter::Workbook;

pub(crate) const MEASURES: &str = "Measures";
const HEAD: [&str; 8] = ["id", "view", "kind", "value", "unit", "state", "reason", "label"];

/// One row of the sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasureRow {
    pub id: i64,
    pub view: String,
    pub kind: String,
    pub value: Option<f64>,
    pub unit: String,
    pub state: String,
    pub reason: String,
    pub label: String,
}

/// The rows the model would export now.
pub fn rows(model: &Model) -> Result<Vec<MeasureRow>, String> {
    let all = model.all_measures().map_err(|e| e.to_string())?;
    Ok(all
        .iter()
        .map(|i| {
            let (v, state, reason) = i.value.parts();
            MeasureRow {
                id: i.id,
                view: i.spec.view_code(),
                kind: i.spec.kind.code().to_string(),
                // an orphan keeps its last number visible next to the state
                value: v.or(i.last),
                unit: i.spec.kind.unit().to_string(),
                state: if i.driving && reason.is_empty() { "driving" } else { state }.to_string(),
                reason,
                label: i.spec.label.clone(),
            }
        })
        .collect())
}

/// Adds the sheet to the book; nothing when there are no measurements.
pub(crate) fn write(wb: &mut Workbook, model: &Model) -> Result<(), String> {
    let rs = rows(model)?;
    if rs.is_empty() {
        return Ok(());
    }
    let e = |x: rust_xlsxwriter::XlsxError| x.to_string();
    let ws = wb.add_worksheet();
    ws.set_name(MEASURES).map_err(e)?;
    for (c, h) in HEAD.iter().enumerate() {
        ws.write_string(0, c as u16, *h).map_err(e)?;
    }
    for (k, r) in rs.iter().enumerate() {
        let row = k as u32 + 1;
        ws.write_number(row, 0, r.id as f64).map_err(e)?;
        ws.write_string(row, 1, &r.view).map_err(e)?;
        ws.write_string(row, 2, &r.kind).map_err(e)?;
        if let Some(v) = r.value {
            ws.write_number(row, 3, v).map_err(e)?;
        }
        ws.write_string(row, 4, &r.unit).map_err(e)?;
        ws.write_string(row, 5, &r.state).map_err(e)?;
        ws.write_string(row, 6, &r.reason).map_err(e)?;
        ws.write_string(row, 7, &r.label).map_err(e)?;
    }
    Ok(())
}

fn text(d: Option<&Data>) -> String {
    match d {
        Some(Data::String(s)) => s.trim().to_string(),
        Some(Data::Float(f)) => f.to_string(),
        Some(Data::Int(i)) => i.to_string(),
        _ => String::new(),
    }
}

/// Reads the sheet of a book; an absent sheet is an empty list.
pub fn read(path: &Path) -> Result<Vec<MeasureRow>, String> {
    let mut wb: Xlsx<_> = open_workbook(path).map_err(|e| format!("{e}"))?;
    let Ok(range) = wb.worksheet_range(MEASURES) else {
        return Ok(vec![]);
    };
    let mut it = range.rows();
    let head: Vec<String> = it
        .next()
        .map(|r| r.iter().map(|d| text(Some(d)).to_lowercase()).collect())
        .unwrap_or_default();
    let at = |name: &str| head.iter().position(|h| h == name);
    let (Some(ci), Some(cv)) = (at("id"), at("value")) else {
        return Err("sheet Measures: columns id and value are required".into());
    };
    let mut out = vec![];
    for r in it {
        let Some(id) = r.get(ci).and_then(|d| match d {
            Data::Float(f) => Some(*f as i64),
            Data::Int(i) => Some(*i),
            _ => None,
        }) else {
            continue;
        };
        let get = |n: &str| text(at(n).and_then(|c| r.get(c)));
        out.push(MeasureRow {
            id,
            view: get("view"),
            kind: get("kind"),
            value: match r.get(cv) {
                Some(Data::Float(f)) => Some(*f),
                Some(Data::Int(i)) => Some(*i as f64),
                _ => None,
            },
            unit: get("unit"),
            state: get("state"),
            reason: get("reason"),
            label: get("label"),
        });
    }
    Ok(out)
}

/// Applies the changed values of the sheet: rows in state `driving` go through `drive_measure`
/// (the far support moves, linked walls follow); other rows, and rows that cannot be driven,
/// are reported and change nothing.
/// Every applied row is one journal step. Returns the report lines.
pub fn import_apply(model: &mut Model, path: &Path) -> Result<Vec<String>, String> {
    let book = read(path)?;
    let mut out = vec![];
    for b in book {
        let Some(n) = rows(model)?.into_iter().find(|n| n.id == b.id) else {
            out.push(format!("Measures id {}: no such measurement", b.id));
            continue;
        };
        let (Some(x), Some(y)) = (b.value, n.value) else { continue };
        if (x - y).abs() <= 1e-6 * y.abs().max(1.0) {
            continue;
        }
        // only a driving measurement's value is editable; the others are reported
        if n.state != "driving" {
            out.push(format!("Measures id {}: value {x} not applied, the measurement is not driving", b.id));
            continue;
        }
        if let Err(e) = model.drive_measure(b.id, x, ddc_model::MeasureSide::B) {
            out.push(format!("Measures id {}: value {x} not applied: {e}", b.id));
        }
    }
    Ok(out)
}

/// What an import of the sheet would change: one line per row whose value differs from the
/// model (outside 1e-6 relative) or whose id is unknown; the model is never touched here.
pub fn import_report(model: &Model, path: &Path) -> Result<Vec<String>, String> {
    let book = read(path)?;
    let now = rows(model)?;
    let mut out = vec![];
    for b in book {
        match now.iter().find(|n| n.id == b.id) {
            None => out.push(format!("Measures id {}: no such measurement", b.id)),
            Some(n) => {
                if let (Some(x), Some(y)) = (b.value, n.value) {
                    if (x - y).abs() > 1e-6 * y.abs().max(1.0) {
                        out.push(format!(
                            "Measures id {}: value {x} differs from {y} ({})",
                            b.id, n.kind
                        ));
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use ddc_model::{
        DimRef, LayerSpec, MeasureAnchor as A, MeasureKind, MeasureSpec, Model, ViewKey, WallSpec,
    };

    use super::*;

    fn model() -> (Model, Vec<i64>) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type("W", &[LayerSpec { material: "B".into(), thickness: 0.2 }])
            .unwrap();
        let specs: Vec<WallSpec> = [0.0, 4.0]
            .iter()
            .map(|y| WallSpec {
                a: (0.0, *y),
                b: (6.0, *y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .collect();
        let w = m.add_walls(&specs).unwrap();
        let face = |w: i64| A::Dim(DimRef::WallEnd { wall: w, face: 1, end: 0 });
        let mk = |m: &mut Model, kind, anchors: &[A]| {
            let s = MeasureSpec {
                kind,
                view: ViewKey::Plan(lv),
                pins: m.measure_pins(anchors).unwrap(),
                label: "x".into(),
            };
            m.add_measure(&s).unwrap()
        };
        mk(&mut m, MeasureKind::Clear, &[face(w[0]), face(w[1])]);
        mk(&mut m, MeasureKind::Distance, &[A::Point([0.0; 3]), A::Point([3.0, 4.0, 0.0])]);
        (m, w)
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ddc_meas_{tag}_{}.xlsx", std::process::id()))
    }

    #[test]
    fn xlsx_measures_roundtrip() {
        let (m, w) = model();
        let p = tmp("rt");
        crate::xlsx::export_xlsx(&m, &p).unwrap();
        let got = read(&p).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].kind, "clear");
        assert!((got[0].value.unwrap() - 3.6).abs() < 1e-9);
        assert_eq!((got[0].unit.as_str(), got[0].state.as_str()), ("m", "live"));
        assert!((got[1].value.unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(got, rows(&m).unwrap());
        assert!(import_report(&m, &p).unwrap().is_empty());
        // an orphan shows state and reason, with the last number kept
        let mut m = m;
        m.delete_wall(w[1]).unwrap();
        let r = rows(&m).unwrap();
        assert_eq!(r[0].state, "orphan");
        assert_eq!(r[0].reason, "orphan:1");
        assert!((r[0].value.unwrap() - 3.6).abs() < 1e-9);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn no_measures_no_sheet() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let p = tmp("none");
        crate::xlsx::export_xlsx(&m, &p).unwrap();
        assert!(read(&p).unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn edited_value_is_reported_not_applied() {
        let (m, _) = model();
        let p = tmp("edit");
        let mut rs = rows(&m).unwrap();
        rs[0].value = Some(9.0);
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name(MEASURES).unwrap();
        for (c, h) in HEAD.iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        for (k, r) in rs.iter().enumerate() {
            ws.write_number(k as u32 + 1, 0, r.id as f64).unwrap();
            ws.write_number(k as u32 + 1, 3, r.value.unwrap()).unwrap();
        }
        wb.save(&p).unwrap();
        let rep = import_report(&m, &p).unwrap();
        assert_eq!(rep.len(), 1, "{rep:?}");
        assert!(rep[0].contains("differs"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn xlsx_drive_one_undo() {
        use ddc_model::MeasureSide;
        let (mut m, w) = model();
        let id = rows(&m).unwrap()[0].id;
        m.set_measure_driving(id, true).unwrap();
        assert_eq!(rows(&m).unwrap()[0].state, "driving");
        let p = tmp("drive");
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name(MEASURES).unwrap();
        for (c, h) in HEAD.iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        ws.write_number(1, 0, id as f64).unwrap();
        ws.write_number(1, 3, 4.6).unwrap();
        // a row that is not driving is reported, not applied
        let other = rows(&m).unwrap()[1].id;
        ws.write_number(2, 0, other as f64).unwrap();
        ws.write_number(2, 3, 9.0).unwrap();
        wb.save(&p).unwrap();
        let y = |m: &Model| -> f64 {
            m.sql().query_row("SELECT ay FROM wall WHERE id=?1", [w[1]], |r| r.get(0)).unwrap()
        };
        let before = y(&m);
        let steps = m.journal_len().unwrap();
        let rep = import_apply(&mut m, &p).unwrap();
        assert_eq!(rep.len(), 1, "{rep:?}");
        assert!(rep[0].contains(&format!("id {other}")));
        assert!((rows(&m).unwrap()[0].value.unwrap() - 4.6).abs() < 1e-9);
        assert!((y(&m) - before - 1.0).abs() < 1e-9, "far wall moved by the change");
        assert_eq!(m.journal_len().unwrap(), steps + 1);
        m.undo().unwrap();
        assert!((rows(&m).unwrap()[0].value.unwrap() - 3.6).abs() < 1e-9);
        assert!((y(&m) - before).abs() < 1e-9);
        let _ = MeasureSide::B;
        let _ = std::fs::remove_file(&p);
    }
}
