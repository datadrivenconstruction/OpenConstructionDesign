//! Room: a click inside a closed wall loop creates it with `add_room`; an open region is refused.

use std::sync::Arc;

use super::ui::poly_any;
use super::{ceiling, Active, Kind, Opts, Rec};
use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;
use ddc_geom::rooms::interior_point;
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, RoomSpec};
use ddc_ui::i18n::Catalog;
use egui::{self, Color32, Pos2, Stroke};

fn numbers(m: &Model, level: i64) -> Result<Vec<String>, Error> {
    let used: Vec<String> = m
        .sql()
        .prepare("SELECT number FROM room WHERE level=?1")?
        .query_map([level], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(used)
}

/// First free positive integer among `used`, counting up from `used.len() + 1`.
fn next_free(used: &[String]) -> String {
    let mut n = used.len() + 1;
    while used.iter().any(|u| u.trim() == n.to_string()) {
        n += 1;
    }
    n.to_string()
}

/// First free positive integer among the level's room numbers.
fn next_number(m: &Model, level: i64) -> Result<String, Error> {
    Ok(next_free(&numbers(m, level)?))
}

/// What place-all did: rooms placed, closed regions that already had a room, open loops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoReport {
    pub placed: usize,
    pub occupied: usize,
    pub open: usize,
}

impl AutoReport {
    pub fn status(&self) -> String {
        let key = match (self.placed, self.occupied, self.open) {
            (0, 0, 0) => "room_place.none",
            (0, 0, _) => "room_place.none_open",
            (0, _, _) => "room_place.all_had",
            _ => "room_place.placed",
        };
        let (n, m, k) = (
            self.placed.to_string(),
            self.occupied.to_string(),
            self.open.to_string(),
        );
        crate::tr(key, &[("n", &n), ("m", &m), ("k", &k)])
    }
}

/// Rooms for every closed region of `level` with no room point inside; the model is not
/// changed. Open loops are only counted. Numbers continue the level's sequence.
pub fn plan_all(
    m: &Model,
    level: i64,
    height: f64,
    name: &str,
    base: &str,
) -> Result<(Vec<RoomSpec>, AutoReport), Error> {
    let regions = m.room_regions(level)?;
    let points: Vec<V2> = m
        .sql()
        .prepare("SELECT px,py FROM room WHERE level=?1")?
        .query_map([level], |r| Ok(v2(r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut used = numbers(m, level)?;
    let mut rep = AutoReport {
        open: m.open_wall_groups(level)?,
        ..AutoReport::default()
    };
    let name = name.trim();
    let mut specs = vec![];
    for g in &regions {
        if points.iter().any(|&p| g.contains(p)) {
            rep.occupied += 1;
            continue;
        }
        let rings: Vec<&[V2]> = std::iter::once(g.outer.as_slice())
            .chain(g.holes.iter().map(|h| h.as_slice()))
            .collect();
        // a sliver with no interior point cannot hold a room point
        let Some(p) = interior_point(&rings).filter(|&p| g.contains(p)) else {
            continue;
        };
        let number = next_free(&used);
        used.push(number.clone());
        specs.push(RoomSpec {
            level,
            name: if name.is_empty() {
                format!("{base} {number}")
            } else {
                name.to_string()
            },
            number,
            point: (p.x, p.y),
            height,
        });
    }
    rep.placed = specs.len();
    Ok((specs, rep))
}

/// Places a room in every free closed region of the active level: one model operation, one
/// undo. Height and name come from the room tool options (defaults when the tool is not armed).
pub fn place_all(ed: &mut Editor) -> Result<AutoReport, Error> {
    let (height, name) = match ed.model_tool() {
        Some(a) if a.kind == Kind::Room => (a.opts.height, a.opts.name.clone()),
        _ => (Opts::for_kind(Kind::Room, ed.height).height, String::new()),
    };
    let base = crate::tr("tool.room.name", &[]);
    let (specs, rep) = plan_all(&ed.model, ed.level, height, &name, &base)?;
    if !specs.is_empty() {
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.add_rooms(&specs).map(|_| ())
        })))?;
    }
    Ok(rep)
}

pub(super) fn h_room_auto(app: &mut EditorApp) {
    let msg = match place_all(&mut app.ed) {
        Ok(r) => r.status(),
        Err(e) => crate::constraints_ui::report(&e),
    };
    // a room warning of this step stays: the frame's settle adds it to this line
    app.ed.status = Some(msg);
}

/// Locale key of why a click at `p` cannot make a room; the held-region test is the one the
/// XLSX import uses.
fn refusal(ed: &Editor, p: V2) -> Option<&'static str> {
    let Some(region) = ceiling::region_at(ed, p) else {
        return Some("tool.err.room_open");
    };
    let held: Result<Vec<((f64, f64), ())>, _> = ed
        .model
        .sql()
        .prepare("SELECT px, py FROM room WHERE level=?1")
        .and_then(|mut q| {
            q.query_map([ed.level], |r| Ok(((r.get(0)?, r.get(1)?), ())))?
                .collect::<Result<_, _>>()
        });
    // an unreadable room table must not let a duplicate through
    let Ok(held) = held else {
        return Some("tool.err.room_check");
    };
    ddc_model::region_holder(&[region], &held, (p.x, p.y)).map(|_| "tool.err.room_exists")
}

/// Record for a click at `p`; the message says why the click cannot make a room.
pub fn pick(ed: &Editor, a: &Active, p: V2) -> Result<Rec, String> {
    if let Some(key) = refusal(ed, p) {
        return Err(a.t(key));
    }
    let level = a.level;
    let height = a.opts.height;
    let name = a.opts.name.trim().to_string();
    let base = a.t("tool.room.name");
    Ok(Rec::new("room", move |m| {
        let number = next_number(m, level)?;
        let name = if name.is_empty() {
            format!("{base} {number}")
        } else {
            name.clone()
        };
        m.add_room(&RoomSpec {
            level,
            name,
            number,
            point: (p.x, p.y),
            height,
        })?;
        Ok(1)
    }))
}

/// Wall loop under the cursor: green fill when a room can be made, nothing when open.
pub fn preview(
    p: &egui::Painter,
    a: &Active,
    fill: Color32,
    st: Stroke,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    if a.region.is_empty() {
        return;
    }
    let s: Vec<Pos2> = a.region.iter().map(|q| to_s(*q)).collect();
    poly_any(p, &s, fill, st);
}

pub fn options(ui: &mut egui::Ui, cat: &Catalog, o: &mut Opts) {
    ui.label(cat.t("room_tool.room.name"));
    ui.add(
        egui::TextEdit::singleline(&mut o.name)
            .desired_width(140.0)
            .hint_text(cat.t("tool.room.name")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    fn boxed(e: &mut Editor, x0: f64) {
        let t = e.types[1].id;
        let p = [(x0, 0.0), (x0 + 5.0, 0.0), (x0 + 5.0, 4.0), (x0, 4.0)];
        for k in 0..4 {
            e.model
                .add_wall(&ddc_model::WallSpec {
                    a: p[k],
                    b: p[(k + 1) % 4],
                    level: e.level,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap();
        }
    }

    #[test]
    fn room_tool_uses_region_holder() {
        let mut e = Editor::new_in_memory().unwrap();
        boxed(&mut e, 0.0);
        boxed(&mut e, 10.0);
        e.model
            .add_room(&RoomSpec {
                level: e.level,
                name: "Hall".into(),
                number: "1".into(),
                point: (2.5, 2.0),
                height: 3.0,
            })
            .unwrap();
        e.reload().unwrap();
        assert_eq!(refusal(&e, v2(1.0, 1.0)), Some("tool.err.room_exists"));
        assert_eq!(refusal(&e, v2(12.0, 2.0)), None);
        assert_eq!(refusal(&e, v2(30.0, 30.0)), Some("tool.err.room_open"));
        // an unreadable room table refuses instead of skipping the check
        e.model.sql().execute_batch("ALTER TABLE room RENAME TO room_gone").unwrap();
        assert_eq!(refusal(&e, v2(12.0, 2.0)), Some("tool.err.room_check"));
    }
}
