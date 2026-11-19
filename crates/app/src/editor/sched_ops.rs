//! Schedule cell edits of category columns (wall, opening, curtain, panel, slab, room), staged
//! against the current model so a pasted block applies as ONE model op (one undo step).

use super::{Editor, OpSpec};
use crate::opening_tool::{validate, OpeningType, PlaceError, Placed};
use ddc_model::{CurtainSpec, Error, Model, PanelKind, PanelSpec, RoomSpec, SlabSlope, WallSpec};
use std::collections::BTreeMap;

/// One parsed category cell value.
#[derive(Clone, Debug)]
pub enum CatEdit {
    WallLen(f64),
    WallType(i64),
    WallHeight(f64),
    OpWidth(f64),
    OpHeight(f64),
    OpSill(f64),
    CurtainName(String),
    CurtainHeight(f64),
    PanelKind(PanelKind),
    PanelName(String),
    SlabSlope(f64),
    RoomName(String),
    RoomNumber(String),
    RoomHeight(f64),
}

/// Staged specs per element: (as in the model, as it will be written).
#[derive(Clone, Default)]
pub struct CatBatch {
    walls: BTreeMap<i64, (WallSpec, WallSpec)>,
    ops: BTreeMap<i64, (OpSpec, OpSpec)>,
    curtains: BTreeMap<i64, (CurtainSpec, CurtainSpec)>,
    panels: BTreeMap<(i64, u32, u32), (PanelSpec, PanelSpec)>,
    rooms: BTreeMap<i64, (RoomSpec, RoomSpec)>,
    slabs: BTreeMap<i64, (Option<SlabSlope>, Option<SlabSlope>)>,
}

/// Why a cell cannot be written. The user sees it through the catalog; `Display` is English for logs.
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    NotPositive,
    NotNumber,
    NoHost,
    WallLength,
    NoWall,
    NoOpening,
    NoCurtain,
    NoPanel,
    NoSlab,
    NoRoom,
    SlopeRange,
    /// An opening of the staged set does not fit its wall.
    Place { id: i64, err: PlaceError },
    /// A failure reported by the model layer (text as is).
    Model(String),
}

impl Reason {
    /// Every catalog key a reason can use (checked in en and ru by the tests).
    pub const KEYS: [&'static str; 13] = [
        "sched_ops.not_positive",
        "sched_ops.not_number",
        "sched_ops.no_host",
        "sched_ops.wall_length",
        "sched_ops.no_wall",
        "sched_ops.no_opening",
        "sched_ops.no_curtain",
        "sched_ops.no_panel",
        "sched_ops.no_slab",
        "sched_ops.no_room",
        "sched_ops.slope_range",
        "sched_ops.place",
        "sched_ops.model",
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Reason::NotPositive => Self::KEYS[0],
            Reason::NotNumber => Self::KEYS[1],
            Reason::NoHost => Self::KEYS[2],
            Reason::WallLength => Self::KEYS[3],
            Reason::NoWall => Self::KEYS[4],
            Reason::NoOpening => Self::KEYS[5],
            Reason::NoCurtain => Self::KEYS[6],
            Reason::NoPanel => Self::KEYS[7],
            Reason::NoSlab => Self::KEYS[8],
            Reason::NoRoom => Self::KEYS[9],
            Reason::SlopeRange => Self::KEYS[10],
            Reason::Place { .. } => Self::KEYS[11],
            Reason::Model(_) => Self::KEYS[12],
        }
    }

    /// Text in `lang`, lengths in `units`.
    pub fn text(&self, lang: ddc_ui::i18n::Lang, units: crate::units::Units) -> String {
        match self {
            Reason::Place { id, err } => {
                let ru = lang == ddc_ui::i18n::Lang::Ru;
                crate::tr_in(lang, self.key(), &[("id", &id.to_string()), ("why", &err.text(ru, units))])
            }
            Reason::Model(e) => crate::tr_in(lang, self.key(), &[("e", e)]),
            _ => crate::tr_in(lang, self.key(), &[]),
        }
    }

    /// Text in the interface language and the active units.
    pub fn text_ui(&self) -> String {
        let lang = if crate::ui_is_ru() { ddc_ui::i18n::Lang::Ru } else { ddc_ui::i18n::Lang::En };
        self.text(lang, crate::units::active_units())
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.text(ddc_ui::i18n::Lang::En, crate::units::Units::Metric))
    }
}

/// Change `pair.1` and tell whether it differs now.
fn upd<T: PartialEq + Clone>(pair: &mut (T, T), f: impl FnOnce(&mut T)) -> bool {
    let prev = pair.1.clone();
    f(&mut pair.1);
    pair.1 != prev
}

fn positive(v: f64) -> Result<f64, Reason> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(Reason::NotPositive)
    }
}

impl CatBatch {
    /// Elements whose staged spec differs from the model.
    pub fn changed(&self) -> usize {
        fn n<K, T: PartialEq>(m: &BTreeMap<K, (T, T)>) -> usize {
            m.values().filter(|p| p.0 != p.1).count()
        }
        n(&self.walls)
            + n(&self.ops)
            + n(&self.curtains)
            + n(&self.panels)
            + n(&self.rooms)
            + n(&self.slabs)
    }

    /// Element `id` (or one of its panels) is staged with a spec that differs from the model.
    pub fn differs(&self, id: i64) -> bool {
        fn d<K, T: PartialEq>(m: &BTreeMap<K, (T, T)>, f: impl Fn(&K) -> bool) -> bool {
            m.iter().any(|(k, p)| f(k) && p.0 != p.1)
        }
        d(&self.walls, |k| *k == id)
            || d(&self.ops, |k| *k == id)
            || d(&self.curtains, |k| *k == id)
            || d(&self.panels, |k| k.0 == id)
            || d(&self.rooms, |k| *k == id)
            || d(&self.slabs, |k| *k == id)
    }

    /// Stage one cell of element `id` (`sub` is the panel cell index). `Ok(true)` the value
    /// differs from the model, `Ok(false)` it is already there, `Err` it cannot be written.
    pub fn stage(&mut self, ed: &Editor, id: i64, sub: u32, e: CatEdit) -> Result<bool, Reason> {
        let geo = matches!(
            e,
            CatEdit::WallLen(_)
                | CatEdit::WallHeight(_)
                | CatEdit::OpWidth(_)
                | CatEdit::OpHeight(_)
                | CatEdit::OpSill(_)
        );
        let (w0, o0) = (self.walls.get(&id).cloned(), self.ops.get(&id).cloned());
        let changed = self.stage_one(ed, id, sub, e)?;
        if changed && geo {
            // the whole staged set must stay valid; a refused cell leaves the batch as it was
            if let Err(m) = self.staged_ok(ed) {
                match w0 {
                    Some(v) => self.walls.insert(id, v),
                    None => self.walls.remove(&id),
                };
                match o0 {
                    Some(v) => self.ops.insert(id, v),
                    None => self.ops.remove(&id),
                };
                return Err(m);
            }
        }
        Ok(changed)
    }

    /// Every opening that is staged, or sits on a staged wall, against its host as it will be
    /// written and the other openings as they will be written.
    fn staged_ok(&self, ed: &Editor) -> Result<(), Reason> {
        let placed: Vec<Placed> = ed
            .openings
            .iter()
            .map(|o| match self.ops.get(&o.id) {
                Some((_, a)) => Placed {
                    along: a.along,
                    width: a.ty.width,
                    sill: a.ty.sill,
                    height: a.ty.height,
                    ..*o
                },
                None => *o,
            })
            .collect();
        for p in &placed {
            let moved = self.ops.get(&p.id).is_some_and(|q| q.0 != q.1);
            let wall_moved = self.walls.get(&p.wall).is_some_and(|q| q.0 != q.1);
            if !moved && !wall_moved {
                continue;
            }
            let mut h = ed.host_of(p.wall).ok_or(Reason::NoHost)?;
            if let Some((_, w)) = self.walls.get(&p.wall) {
                let bulge = ed.walls.iter().find(|x| x.id == p.wall).map_or(0.0, |x| x.bulge);
                h.b = ddc_geom::v2(w.b.0, w.b.1);
                h.height = w.height;
                h.run = ddc_geom::arc::axis_length(h.a, h.b, bulge);
            }
            let t = OpeningType { kind: p.kind, width: p.width, height: p.height, sill: p.sill };
            validate(&h, &placed, Some(p.id), p.along, &t)
                .map_err(|err| Reason::Place { id: p.id, err })?;
        }
        Ok(())
    }

    fn stage_one(&mut self, ed: &Editor, id: i64, sub: u32, e: CatEdit) -> Result<bool, Reason> {
        match e {
            CatEdit::WallLen(l) => {
                let w = ed.walls.iter().find(|w| w.id == id).ok_or(Reason::NoWall)?;
                let d = w.b - w.a;
                let cur = d.len();
                if w.bulge != 0.0 || w.top.is_some() || cur <= 0.0 || !(l > 0.01) {
                    return Err(Reason::WallLength);
                }
                let b = w.a + d * (l / cur);
                let p = self.wall(ed, id)?;
                let same = (p.1.b.0 - b.x).hypot(p.1.b.1 - b.y) < 1e-9;
                Ok(!same && upd(p, |s| s.b = (b.x, b.y)))
            }
            CatEdit::WallType(t) => Ok(upd(self.wall(ed, id)?, |s| s.wall_type = t)),
            CatEdit::WallHeight(h) => {
                let h = positive(h)?;
                Ok(upd(self.wall(ed, id)?, |s| s.height = h))
            }
            CatEdit::OpWidth(v) | CatEdit::OpHeight(v) | CatEdit::OpSill(v) => {
                if !v.is_finite() {
                    return Err(Reason::NotNumber);
                }
                let p = self.opening(ed, id)?;
                let mut n = p.1;
                match e {
                    CatEdit::OpWidth(_) => n.ty.width = v,
                    CatEdit::OpHeight(_) => n.ty.height = v,
                    _ => n.ty.sill = v,
                }
                if n == p.1 {
                    return Ok(false);
                }
                p.1 = n;
                Ok(true)
            }
            CatEdit::CurtainName(t) => Ok(upd(self.curtain(ed, id)?, |s| s.name = t)),
            CatEdit::CurtainHeight(h) => {
                let h = positive(h)?;
                Ok(upd(self.curtain(ed, id)?, |s| s.height = h))
            }
            CatEdit::PanelKind(k) => self.panel(ed, id, sub, |p| p.kind = k),
            CatEdit::PanelName(t) => self.panel(ed, id, sub, |p| p.name = t),
            CatEdit::SlabSlope(pct) => {
                let s = ed.slab(id).ok_or(Reason::NoSlab)?;
                let now = super::cw::slope_for_percent(s, pct).ok_or(Reason::SlopeRange)?;
                let slope = s.slope;
                let p = self.slabs.entry(id).or_insert((slope, slope));
                Ok(upd(p, |v| *v = now))
            }
            CatEdit::RoomName(t) => Ok(upd(self.room(ed, id)?, |s| s.name = t)),
            CatEdit::RoomNumber(t) => Ok(upd(self.room(ed, id)?, |s| s.number = t)),
            CatEdit::RoomHeight(h) => {
                let h = positive(h)?;
                Ok(upd(self.room(ed, id)?, |s| s.height = h))
            }
        }
    }

    fn wall(&mut self, ed: &Editor, id: i64) -> Result<&mut (WallSpec, WallSpec), Reason> {
        if !self.walls.contains_key(&id) {
            let s = ed.wall_specs(&[id]).map_err(|e| Reason::Model(e.to_string()))?.remove(0);
            self.walls.insert(id, (s.clone(), s));
        }
        self.walls.get_mut(&id).ok_or(Reason::NoWall)
    }

    fn opening(&mut self, ed: &Editor, id: i64) -> Result<&mut (OpSpec, OpSpec), Reason> {
        if !self.ops.contains_key(&id) {
            let s = ed.spec_of(ed.opening(id).ok_or(Reason::NoOpening)?);
            self.ops.insert(id, (s, s));
        }
        self.ops.get_mut(&id).ok_or(Reason::NoOpening)
    }

    fn curtain(&mut self, ed: &Editor, id: i64) -> Result<&mut (CurtainSpec, CurtainSpec), Reason> {
        if !self.curtains.contains_key(&id) {
            let s = ed.curtain(id).ok_or(Reason::NoCurtain)?.spec().clone();
            self.curtains.insert(id, (s.clone(), s));
        }
        self.curtains.get_mut(&id).ok_or(Reason::NoCurtain)
    }

    fn panel(
        &mut self,
        ed: &Editor,
        id: i64,
        k: u32,
        f: impl FnOnce(&mut PanelSpec),
    ) -> Result<bool, Reason> {
        let c = ed.curtain(id).ok_or(Reason::NoCurtain)?;
        let cell = c.layout.layout.cells.get(k as usize).ok_or(Reason::NoPanel)?;
        let key = (id, cell.i as u32, cell.j as u32);
        let cur = c.layout.panels.get(k as usize).cloned().unwrap_or_default();
        let p = self.panels.entry(key).or_insert((cur.clone(), cur));
        Ok(upd(p, f))
    }

    fn room(&mut self, ed: &Editor, id: i64) -> Result<&mut (RoomSpec, RoomSpec), Reason> {
        if !self.rooms.contains_key(&id) {
            let s = ed
                .model
                .sql()
                .query_row(
                    "SELECT level, name, number, px, py, height FROM room WHERE id=?1",
                    [id],
                    |r| {
                        Ok(RoomSpec {
                            level: r.get(0)?,
                            name: r.get(1)?,
                            number: r.get(2)?,
                            point: (r.get(3)?, r.get(4)?),
                            height: r.get(5)?,
                        })
                    },
                )
                .map_err(|e| Reason::Model(e.to_string()))?;
            self.rooms.insert(id, (s.clone(), s));
        }
        self.rooms.get_mut(&id).ok_or(Reason::NoRoom)
    }

    /// Write every staged change; call inside one `Model::atomic`.
    pub fn apply(&self, m: &mut Model) -> Result<(), Error> {
        for (&id, (b, a)) in &self.walls {
            if a == b {
                continue;
            }
            m.batch_walls_edit("wall_edit", |db, snaps| {
                snaps.save(db, "wall", "id", id)?;
                db.execute(
                    // a stored top polyline pins the 3D top, so a new height must replace it
                    "UPDATE wall SET bx=?2, by=?3, wtype=?4, height=?5, \
                     top_profile=CASE WHEN ABS(?5-height)>1e-9 THEN NULL ELSE top_profile END \
                     WHERE id=?1",
                    (id, a.b.0, a.b.1, a.wall_type, a.height),
                )?;
                Ok(())
            })?;
        }
        for (&id, (b, a)) in &self.ops {
            if a != b {
                m.update_opening_with(id, &a.model(), a.swing())?;
            }
        }
        for (&id, (b, a)) in &self.curtains {
            if a != b {
                m.set_curtain_wall(id, a)?;
            }
        }
        for (&(id, i, j), (b, a)) in &self.panels {
            if a != b {
                m.set_curtain_panel(id, i, j, a)?;
            }
        }
        for (&id, (b, a)) in &self.rooms {
            if a != b {
                m.update_room(id, a)?;
            }
        }
        for (&id, (b, a)) in &self.slabs {
            if a != b {
                m.set_slab_slope(id, *a)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Reason;
    use crate::editor::Editor;
    use crate::opening_tool::PlaceError;
    use crate::schedule::{build, extra::write_cells_why, Cat};
    use crate::units::Units;
    use ddc_model::OpeningSpec;
    use ddc_ui::i18n::Lang;

    #[test]
    fn every_reason_key_exists_in_both_languages_and_renders() {
        let all = [
            Reason::NotPositive,
            Reason::NotNumber,
            Reason::NoHost,
            Reason::WallLength,
            Reason::NoWall,
            Reason::NoOpening,
            Reason::NoCurtain,
            Reason::NoPanel,
            Reason::NoSlab,
            Reason::NoRoom,
            Reason::SlopeRange,
            Reason::Place { id: 7, err: PlaceError::Overlap { other: 9 } },
            Reason::Model("boom".into()),
        ];
        let keys: Vec<&str> = all.iter().map(|r| r.key()).collect();
        assert_eq!(keys, Reason::KEYS.to_vec(), "KEYS follows the variants");
        for r in &all {
            let en = r.text(Lang::En, Units::Metric);
            let ru = r.text(Lang::Ru, Units::Metric);
            for (lang, t) in [("en", &en), ("ru", &ru)] {
                assert!(!t.is_empty() && t.as_str() != r.key(), "{lang}: key {} missing", r.key());
                assert!(!t.contains('{'), "{lang}: unfilled argument in {t}");
            }
            if !matches!(r, Reason::Place { .. } | Reason::Model(_)) {
                assert_ne!(en, ru, "{} is translated", r.key());
            }
        }
        let p = Reason::Place { id: 7, err: PlaceError::Overlap { other: 9 } };
        assert!(p.text(Lang::En, Units::Metric).starts_with("#7: "));
        assert_eq!(Reason::NoHost.to_string(), "no host wall");
    }

    /// Editor with the test room; two doors on its first wall at 1.0..2.0 and 2.2..3.2 m.
    fn doors() -> (Editor, i64, [i64; 2]) {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        let wall = e.walls[0].id;
        let spec = |along: f64| OpeningSpec {
            along,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
            kind: "door".into(),
        };
        let a = e.model.add_opening(wall, &spec(1.0)).unwrap();
        let b = e.model.add_opening(wall, &spec(2.2)).unwrap();
        e.reload().unwrap();
        (e, wall, [a, b])
    }

    fn cell(cat: Cat, e: &Editor, id: i64, key: &str, text: &str) -> (usize, usize, String) {
        let tb = build(e, cat);
        let r = tb.rows.iter().position(|r| r.id == id).unwrap();
        let c = tb.cols.iter().position(|c| c.key == key).unwrap();
        (r, c, text.to_string())
    }

    fn width(e: &Editor, id: i64) -> f64 {
        e.opening(id).unwrap().width
    }

    fn overlaps(e: &Editor) -> bool {
        e.openings.iter().any(|a| {
            e.openings.iter().any(|b| {
                a.id < b.id
                    && a.wall == b.wall
                    && a.along < b.along + b.width - 1e-9
                    && b.along < a.along + a.width - 1e-9
            })
        })
    }

    #[test]
    fn paste_two_widened_openings_overlap_refused() {
        let (mut e, _, [a, b]) = doors();
        let tb = build(&e, Cat::Doors);
        // a: 1.0..2.5 runs into b (2.2..3.2); b: 2.2..3.7 is free
        let cells = vec![
            cell(Cat::Doors, &e, a, "w", "1500"),
            cell(Cat::Doors, &e, b, "w", "1500"),
        ];
        let u0 = e.undo_len();
        let (o, why) = write_cells_why(&mut e, &tb, &cells);
        assert_eq!((o.done, o.bad, o.unchanged), (1, 1, 0), "{why:?}");
        assert_eq!(why.len(), 1);
        assert_eq!((why[0].0, why[0].1), (cells[0].0, cells[0].1), "row and column of the refusal");
        assert!(matches!(why[0].2, Reason::Place { err: PlaceError::Overlap { .. }, .. }), "{:?}", why[0].2);
        assert!((width(&e, a) - 1.0).abs() < 1e-9, "refused cell left as it was");
        assert!((width(&e, b) - 1.5).abs() < 1e-9);
        assert!(!overlaps(&e));
        assert_eq!(e.undo_len(), u0 + 1);
    }

    #[test]
    fn paste_wall_shorter_than_opening_refused() {
        let (mut e, wall, [a, _]) = doors();
        let tb = build(&e, Cat::Walls);
        let len0 = (e.walls[0].b - e.walls[0].a).len();
        // a ends at 2.0 m, so a 1.5 m wall cannot keep it
        let cells = vec![cell(Cat::Walls, &e, wall, "len", "1500")];
        let u0 = e.undo_len();
        let (o, why) = write_cells_why(&mut e, &tb, &cells);
        assert_eq!((o.done, o.bad), (0, 1), "{why:?}");
        assert_eq!(why.len(), 1);
        assert!(((e.walls[0].b - e.walls[0].a).len() - len0).abs() < 1e-9);
        assert_eq!(e.undo_len(), u0, "nothing written");
        assert!(e.opening(a).is_some());
    }

    #[test]
    fn set_and_back_counts_unchanged() {
        let (mut e, wall, _) = doors();
        let tb = build(&e, Cat::Walls);
        let h = e.walls[0].height;
        let mm = format!("{}", (h * 1000.0).round());
        assert!((mm.parse::<f64>().unwrap() / 1000.0 - h).abs() < 1e-12, "fixture height in mm");
        let cells = vec![
            cell(Cat::Walls, &e, wall, "h", "2500"),
            cell(Cat::Walls, &e, wall, "h", &mm),
        ];
        let u0 = e.undo_len();
        let (o, why) = write_cells_why(&mut e, &tb, &cells);
        assert_eq!((o.done, o.bad, o.unchanged), (0, 0, 2), "{why:?}");
        assert_eq!(e.undo_len(), u0, "net no change, no undo step");
        assert!((e.walls[0].height - h).abs() < 1e-12);
    }

    #[test]
    fn valid_paste_still_one_undo() {
        let (mut e, wall, [a, b]) = doors();
        let tb = build(&e, Cat::Doors);
        let (wa, wb) = (width(&e, a), width(&e, b));
        // a shrinks, b widens; the wall is shortened to 4 m, above both
        let mut cells = vec![
            cell(Cat::Doors, &e, a, "w", "800"),
            cell(Cat::Doors, &e, b, "w", "1200"),
        ];
        let (o, why) = write_cells_why(&mut e, &tb, &cells);
        assert_eq!((o.done, o.bad), (2, 0), "{why:?}");
        assert_eq!(why.len(), 0);
        let tw = build(&e, Cat::Walls);
        cells = vec![cell(Cat::Walls, &e, wall, "len", "4000")];
        let u0 = e.undo_len();
        let (o, why) = write_cells_why(&mut e, &tw, &cells);
        assert_eq!((o.done, o.bad), (1, 0), "{why:?}");
        assert_eq!(e.undo_len(), u0 + 1);
        e.undo();
        e.undo();
        assert!((width(&e, a) - wa).abs() < 1e-9 && (width(&e, b) - wb).abs() < 1e-9);
        assert!(!overlaps(&e));
    }

    #[test]
    fn two_cells_of_one_wall_count_two() {
        let (mut e, wall, _) = doors();
        let tb = build(&e, Cat::Walls);
        let cells = vec![
            cell(Cat::Walls, &e, wall, "len", "5000"),
            cell(Cat::Walls, &e, wall, "h", "3100"),
        ];
        let u0 = e.undo_len();
        let (o, why) = write_cells_why(&mut e, &tb, &cells);
        assert_eq!((o.done, o.bad, o.unchanged), (2, 0, 0), "{why:?}");
        assert_eq!(e.undo_len(), u0 + 1);
        assert!(((e.walls[0].b - e.walls[0].a).len() - 5.0).abs() < 1e-9);
        assert!((e.walls[0].height - 3.1).abs() < 1e-9);
    }
}
