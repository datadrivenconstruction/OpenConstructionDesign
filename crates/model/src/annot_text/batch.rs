//! Many annotations in one model operation (one undo step), with text and line styles matched to
//! existing ones by value or created in the same operation. Used to turn drawing primitives into
//! 2D detail items.

use rusqlite::{params, Connection};

use super::{dash_text, line_styles_in, text_styles_in, view_exists, AnnoShape, AnnoView};
use super::{LineStyle, TextStyle};
use crate::{Error, Model};

/// One annotation to add: shape plus the wanted styles by value (`None` keeps the default).
#[derive(Clone, Debug, PartialEq)]
pub struct AnnoBatchItem {
    pub shape: AnnoShape,
    pub text: Option<TextStyle>,
    pub line: Option<LineStyle>,
}

/// Style values equal within this (mm on the sheet) reuse the existing style.
const STYLE_EPS: f64 = 1e-6;

fn same_dash(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= STYLE_EPS)
}

fn same_text(a: &TextStyle, b: &TextStyle) -> bool {
    (a.height_mm - b.height_mm).abs() <= STYLE_EPS
        && a.bold == b.bold
        && a.underline == b.underline
        && a.frame == b.frame
}

fn same_line(a: &LineStyle, b: &LineStyle) -> bool {
    (a.weight_mm - b.weight_mm).abs() <= STYLE_EPS && same_dash(&a.dash, &b.dash)
}

/// A style name not used yet in `table`: `base`, then `base 2`, `base 3`, ...
fn free_name(db: &Connection, table: &str, base: &str) -> Result<String, Error> {
    let base = if base.trim().is_empty() { "style" } else { base.trim() };
    let sql = format!("SELECT count(*) FROM {table} WHERE name=?1");
    crate::free_name(base, |k| format!("{base} {k}"), |name| {
        Ok::<_, Error>(db.query_row(&sql, [name], |r| r.get::<_, i64>(0))? > 0)
    })
}

fn text_id(db: &Connection, known: &mut Vec<TextStyle>, s: &TextStyle) -> Result<i64, Error> {
    if let Some(t) = known.iter().find(|t| same_text(t, s)) {
        return Ok(t.id);
    }
    let name = free_name(db, "anno_text_style", &s.name)?;
    db.execute(
        "INSERT INTO anno_text_style(name,height_mm,bold,underline,frame) VALUES (?1,?2,?3,?4,?5)",
        params![name, s.height_mm, s.bold, s.underline, s.frame],
    )?;
    let id = db.last_insert_rowid();
    known.push(TextStyle {
        id,
        name,
        ..s.clone()
    });
    Ok(id)
}

fn line_id(db: &Connection, known: &mut Vec<LineStyle>, s: &LineStyle) -> Result<i64, Error> {
    if let Some(l) = known.iter().find(|l| same_line(l, s)) {
        return Ok(l.id);
    }
    let name = free_name(db, "anno_line_style", &s.name)?;
    db.execute(
        "INSERT INTO anno_line_style(name,weight_mm,dash) VALUES (?1,?2,?3)",
        params![name, s.weight_mm, dash_text(&s.dash)],
    )?;
    let id = db.last_insert_rowid();
    known.push(LineStyle {
        id,
        name,
        ..s.clone()
    });
    Ok(id)
}

fn check_styles(it: &AnnoBatchItem) -> Result<(), Error> {
    if let Some(t) = &it.text {
        if !t.height_mm.is_finite() || t.height_mm <= 0.0 {
            return Err(Error::Invalid("text style needs a height".into()));
        }
    }
    if let Some(l) = &it.line {
        if !l.weight_mm.is_finite()
            || l.weight_mm <= 0.0
            || l.dash.iter().any(|d| !d.is_finite() || *d < 0.0)
        {
            return Err(Error::Invalid("line style needs a weight".into()));
        }
    }
    Ok(())
}

impl Model {
    /// Adds every item to `view` in one operation: one undo removes them all together with the
    /// styles this call created. Nothing is added when any item is invalid. Returns the new ids.
    pub fn add_annos_batch(
        &mut self,
        view: AnnoView,
        items: &[AnnoBatchItem],
    ) -> Result<Vec<i64>, Error> {
        if items.is_empty() {
            return Err(Error::Invalid("nothing to add".into()));
        }
        for it in items {
            it.shape.check()?;
            check_styles(it)?;
        }
        let (k, vid) = view.code();
        self.batch_in(
            "anno_batch",
            &["anno_text_style", "anno_line_style", "anno"],
            |db, _| {
                if !view_exists(db, view)? {
                    return Err(Error::Invalid(format!("no such view {k} {vid}")));
                }
                let mut texts = text_styles_in(db)?;
                let mut lines = line_styles_in(db)?;
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    let ts = match &it.text {
                        Some(s) => Some(text_id(db, &mut texts, s)?),
                        None => None,
                    };
                    let ls = match &it.line {
                        Some(s) => Some(line_id(db, &mut lines, s)?),
                        None => None,
                    };
                    db.execute(
                        "INSERT INTO anno(view_kind,view_id,kind,text_style,line_style,data)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![
                            k,
                            vid,
                            it.shape.code(),
                            ts,
                            ls,
                            it.shape.to_json().to_string()
                        ],
                    )?;
                    out.push(db.last_insert_rowid());
                }
                Ok(out)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::tests::room_6x4;
    use crate::{RegionFill, TextNote};

    fn line(pts: Vec<(f64, f64)>) -> AnnoShape {
        AnnoShape::Line { pts, closed: false }
    }

    #[test]
    fn batch_is_one_undo_with_its_new_styles() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let view = AnnoView::Plan(lv);
        let (nt, nl) = (m.text_styles().unwrap().len(), m.line_styles().unwrap().len());
        let thin = m.line_styles().unwrap()[0].clone();
        let items = vec![
            AnnoBatchItem {
                shape: line(vec![(0.0, 0.0), (1.0, 0.0)]),
                text: None,
                line: Some(LineStyle {
                    name: "other name".into(),
                    ..thin.clone()
                }),
            },
            AnnoBatchItem {
                shape: line(vec![(0.0, 1.0), (1.0, 1.0)]),
                text: None,
                line: Some(LineStyle {
                    id: 0,
                    name: thin.name.clone(),
                    weight_mm: 0.43,
                    dash: vec![3.0, 1.5],
                }),
            },
            AnnoBatchItem {
                shape: AnnoShape::Text(TextNote::new((0.0, 2.0), "A")),
                text: Some(TextStyle {
                    name: "drawing text".into(),
                    height_mm: 1.75,
                    ..TextStyle::default()
                }),
                line: None,
            },
            AnnoBatchItem {
                shape: AnnoShape::Region {
                    rings: vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]],
                    fill: RegionFill::Solid { gray: 0 },
                    boundary: false,
                },
                text: None,
                line: None,
            },
        ];
        let ids = m.add_annos_batch(view, &items).unwrap();
        assert_eq!(ids.len(), 4);
        let got = m.annos(view).unwrap();
        assert_eq!(got.len(), 4);
        // equal values reuse the style whatever its name; new values get a free name
        let a0 = m.anno(ids[0]).unwrap();
        assert_eq!(a0.line_style, Some(thin.id));
        let a1 = m.anno(ids[1]).unwrap();
        assert_eq!(a1.line.weight_mm, 0.43);
        assert_eq!(a1.line.dash, vec![3.0, 1.5]);
        assert_eq!(a1.line.name, format!("{} 2", thin.name));
        assert_eq!(m.anno(ids[2]).unwrap().text.height_mm, 1.75);
        assert_eq!(m.text_styles().unwrap().len(), nt + 1);
        assert_eq!(m.line_styles().unwrap().len(), nl + 1);
        m.undo().unwrap();
        assert!(m.annos(view).unwrap().is_empty());
        assert_eq!(m.text_styles().unwrap().len(), nt);
        assert_eq!(m.line_styles().unwrap().len(), nl);
    }

    #[test]
    fn one_bad_item_adds_nothing() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let view = AnnoView::Plan(lv);
        let good = AnnoBatchItem {
            shape: line(vec![(0.0, 0.0), (1.0, 0.0)]),
            text: None,
            line: None,
        };
        let bad = AnnoBatchItem {
            shape: line(vec![(0.0, 0.0)]),
            ..good.clone()
        };
        assert!(m.add_annos_batch(view, &[good.clone(), bad]).is_err());
        assert!(m.add_annos_batch(AnnoView::Plan(lv + 999), &[good]).is_err());
        assert!(m.add_annos_batch(view, &[]).is_err());
        assert!(m.annos(view).unwrap().is_empty());
    }
}
