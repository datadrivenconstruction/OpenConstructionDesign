//! Schedules ("model + table" interface): rows per category from model quantities,
//! sorting, filtering, totals, TSV and .xlsx; cell edits go through editor commands (Ctrl+Z).

pub mod cats;
pub mod extra;
pub mod formula;
pub mod group;
pub mod multi;
pub mod multi_ui;
pub mod ui;
pub mod xlsx_in;

use crate::editor::cw::{panel_kind_parse, panel_label_in, PANEL_KINDS};
use crate::editor::{Editor, Sel};
use crate::opening_tool::Kind as OpKind;
use crate::expr_field::Scope;
use crate::units::{
    area_in_units, edit_length, fmt_fixed, fmt_num, fmt_num_en, parse_edit_length,
    parse_edit_length_lang, precision, ui_decimal_mark, volume_in_units, DecimalMark, InputError,
    NumError, Units,
};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cat {
    Walls,
    Doors,
    Windows,
    Curtains,
    Panels,
    Slabs,
    Rooms,
    Columns,
    Beams,
    Roofs,
    Ceilings,
    Stairs,
    Railings,
    MepRuns,
    Components,
    /// Rebar bending schedule (one row per mark).
    Rebar,
    Ramps,
    Meshes,
    Footings,
    StripFootings,
    Rafts,
    Braces,
    Trusses,
    BeamSystems,
    Solids,
    Masses,
    /// Terrain: one row per terrain body (it has no level; it appears on every level).
    Topography,
    Shafts,
    Zones,
    /// Datums have no level and no phases: one row per grid or reference plane of the model.
    Grids,
    RefPlanes,
    RoomSeps,
}

impl Cat {
    pub const ALL: [Cat; 32] = [
        Cat::Walls,
        Cat::Doors,
        Cat::Windows,
        Cat::Curtains,
        Cat::Panels,
        Cat::Slabs,
        Cat::Rooms,
        Cat::Columns,
        Cat::Beams,
        Cat::Roofs,
        Cat::Ceilings,
        Cat::Stairs,
        Cat::Railings,
        Cat::MepRuns,
        Cat::Components,
        Cat::Rebar,
        Cat::Ramps,
        Cat::Meshes,
        Cat::Footings,
        Cat::StripFootings,
        Cat::Rafts,
        Cat::Braces,
        Cat::Trusses,
        Cat::BeamSystems,
        Cat::Solids,
        Cat::Masses,
        Cat::Topography,
        Cat::Shafts,
        Cat::Zones,
        Cat::Grids,
        Cat::RefPlanes,
        Cat::RoomSeps,
    ];
    /// The tab is always shown (others only if the level has elements).
    pub fn basic(self) -> bool {
        self.index() < 7
    }
    /// Model table of rows (phases, project parameters); panels have no row of their own.
    pub fn table(self) -> Option<&'static str> {
        Some(match self {
            Cat::Walls => "wall",
            Cat::Doors | Cat::Windows => "opening",
            Cat::Curtains => "curtain_wall",
            // a zone is a group of rooms: no phase or project parameter of its own
            Cat::Panels | Cat::Rebar | Cat::Zones | Cat::Grids | Cat::RefPlanes => return None,
            Cat::Slabs => "slab",
            Cat::Rooms => "room",
            Cat::Columns => "col",
            Cat::Beams => "beam",
            Cat::Roofs => "roof",
            Cat::Ceilings => "ceiling",
            Cat::Stairs => "stair",
            Cat::Railings => "railing",
            Cat::MepRuns => "mep_run",
            Cat::Components => "comp",
            Cat::Ramps => "ramp",
            Cat::Meshes => "mesh",
            Cat::Footings => "iso_footing",
            Cat::StripFootings => "strip_footing",
            Cat::Rafts => "raft",
            Cat::Braces => "brace",
            Cat::Trusses => "truss",
            Cat::BeamSystems => "beam_system",
            Cat::Solids => "solid",
            Cat::Masses => "mass",
            Cat::Topography => "toposolid",
            Cat::Shafts => "shaft",
            Cat::RoomSeps => "room_sep",
        })
    }
    /// Binding category of project parameters (`ddc_model::PARAM_CATEGORIES`).
    pub fn param_cat(self) -> Option<&'static str> {
        Some(match self {
            Cat::Doors => "door",
            Cat::Windows => "window",
            Cat::Panels | Cat::Meshes => return None,
            c => c.table()?,
        })
    }
    /// Key of the tab title and of the `sched.cat.*` label.
    fn key(self) -> &'static str {
        match self {
            Cat::Walls => "walls",
            Cat::Doors => "doors",
            Cat::Windows => "windows",
            Cat::Curtains => "curtains",
            Cat::Panels => "panels",
            Cat::Slabs => "slabs",
            Cat::Rooms => "rooms",
            Cat::Columns => "columns",
            Cat::Beams => "beams",
            Cat::Roofs => "roofs",
            Cat::Ceilings => "ceilings",
            Cat::Stairs => "stairs",
            Cat::Railings => "railings",
            Cat::MepRuns => "mep_runs",
            Cat::Components => "components",
            Cat::Rebar => "rebar",
            Cat::Ramps => "ramps",
            Cat::Meshes => "meshes",
            Cat::Footings => "footings",
            Cat::StripFootings => "strip_footings",
            Cat::Rafts => "rafts",
            Cat::Braces => "braces",
            Cat::Trusses => "trusses",
            Cat::BeamSystems => "beam_systems",
            Cat::Solids => "solids",
            Cat::Masses => "masses",
            Cat::Topography => "topography",
            Cat::Shafts => "shafts",
            Cat::Zones => "zones",
            Cat::Grids => "grids",
            Cat::RefPlanes => "ref_planes",
            Cat::RoomSeps => "room_seps",
        }
    }
    /// Tab title in the interface language.
    pub fn title(self) -> String {
        crate::tr(&format!("sched.cat.{}", self.key()), &[])
    }
    /// Tab title in the given language.
    pub fn title_in(self, ru: bool) -> String {
        tl(ru, &format!("sched.cat.{}", self.key()))
    }
    pub fn index(self) -> usize {
        Cat::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// Unit labels are a handful of fixed texts: each is built once per language and kept.
fn unit_text(ru: bool, key: &'static str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<(&'static str, bool), &'static str>>> = OnceLock::new();
    let mut m = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *m.entry((key, ru))
        .or_insert_with(|| Box::leak(tl(ru, key).into_boxed_str()))
}

/// Column quantity: sets display, header unit, total and XLSX format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Int,
    Len,
    Area,
    Vol,
    Pct,
    /// Mass in kg; the title carries the unit. Summed.
    Mass,
    /// Number without a length unit (project parameters, formulas); summed.
    Num,
}

impl Kind {
    pub fn numeric(self) -> bool {
        self != Kind::Text
    }
    /// Additive (lengths, areas, volumes, counts): summed in totals. A unitless number
    /// (price, coefficient) is not: a sum of coefficients is meaningless.
    pub fn summed(self) -> bool {
        matches!(
            self,
            Kind::Len | Kind::Area | Kind::Vol | Kind::Int | Kind::Mass
        )
    }
    /// Unit label in the language of the interface.
    pub fn unit(self, u: Units) -> &'static str {
        self.unit_in(u, crate::ui_lang_now())
    }
    pub fn unit_in(self, u: Units, ru: bool) -> &'static str {
        match (self, u) {
            (Kind::Len, Units::Metric) => unit_text(ru, "sch.unit.mm"),
            (Kind::Len, Units::Imperial) => unit_text(ru, "sch.unit.ft"),
            (Kind::Area, Units::Metric) => unit_text(ru, "sch.unit.m2"),
            (Kind::Area, Units::Imperial) => "ft²",
            (Kind::Vol, Units::Metric) => unit_text(ru, "sch.unit.m3"),
            (Kind::Vol, Units::Imperial) => "ft³",
            (Kind::Pct, _) => "%",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    No,
    Text,
    Num,
    /// A choice from `Col::choices`.
    Choice,
}

#[derive(Clone, Debug)]
pub struct Col {
    pub key: String,
    pub title: String,
    pub kind: Kind,
    pub edit: Edit,
    pub width: f32,
    pub choices: Vec<String>,
}

/// Catalog string in an explicit language (the table builders get `ru` from the caller).
pub fn tl(ru: bool, key: &str) -> String {
    use ddc_ui::i18n::{Catalog, Lang};
    static CAT: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    let c = CAT.get_or_init(|| Catalog::new(Lang::En));
    c.t_in(if ru { Lang::Ru } else { Lang::En }, key)
}

pub fn col(key: impl Into<String>, title: impl Into<String>, kind: Kind, edit: Edit) -> Col {
    let (key, title) = (key.into(), title.into());
    let width = match (kind, edit) {
        (Kind::Int, _) => 64.0f32,
        (Kind::Text, Edit::No) => 120.0,
        (Kind::Text, _) => 150.0,
        _ => 110.0,
    };
    // the header with its unit fits entirely
    let unit = if kind.unit(Units::Metric).is_empty() {
        0
    } else {
        4
    };
    let width = width.max(((title.chars().count() + unit) as f32 * 7.6 + 18.0).round());
    Col {
        key,
        title,
        kind,
        edit,
        width,
        choices: vec![],
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    T(String),
    N(f64),
}

#[derive(Clone, Debug)]
pub struct Row {
    /// Element in plan/3D; rooms are not selectable in the view: `None`.
    pub sel: Option<Sel>,
    pub id: i64,
    /// Cell index for curtain-wall panels.
    pub sub: u32,
    pub vals: Vec<Val>,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub cat: Cat,
    pub cols: Vec<Col>,
    pub rows: Vec<Row>,
    /// Row phases (per `rows`); None if the category has no phases.
    pub row_phase: Vec<Option<ddc_model::ElemPhase>>,
    /// Model phases in order: (id, label).
    pub phases: Vec<(i64, String)>,
}

pub fn wall_length(w: &crate::editor::Wall) -> f64 {
    let c = (w.b - w.a).len();
    if w.bulge == 0.0 {
        return c;
    }
    let th = 4.0 * w.bulge.atan();
    c * th / (2.0 * (th / 2.0).sin())
}

fn n(v: f64) -> Val {
    Val::N(v)
}
/// Valid panel kinds in one language, comma separated.
fn panel_kind_list(ru: bool) -> String {
    PANEL_KINDS
        .iter()
        .map(|k| panel_label_in(*k, ru))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Status for an unknown panel kind typed in the schedule.
fn panel_kind_unknown_status(ru: bool, text: &str) -> String {
    crate::tr_lang(
        ru,
        "sch.edit.no_panel_kind",
        &[("text", text), ("kinds", &panel_kind_list(ru))],
    )
}

fn t(s: impl Into<String>) -> Val {
    Val::T(s.into())
}

/// Row count of a category without building the table (tab counters).
pub fn count(ed: &Editor, cat: Cat) -> usize {
    match cat {
        Cat::Walls => ed.walls.len(),
        Cat::Doors => ed
            .openings
            .iter()
            .filter(|o| o.kind == OpKind::Door)
            .count(),
        Cat::Windows => ed
            .openings
            .iter()
            .filter(|o| o.kind == OpKind::Window)
            .count(),
        Cat::Curtains => ed.curtains.len(),
        Cat::Panels => ed
            .curtains
            .iter()
            .map(|c| c.layout.layout.cells.len())
            .sum(),
        Cat::Slabs => ed.slabs.len(),
        Cat::Rooms => ed
            .model
            .sql()
            .query_row(
                "SELECT COUNT(*) FROM room WHERE level=?1",
                [ed.level],
                |r| r.get::<_, i64>(0),
            )
            .map_or(0, |c| c as usize),
        c => cats::count(ed, c),
    }
}

/// Category table from the editor's current level (SI units; display via `fmt`).
/// A door's or window's mark as its tag and the drawing set show it ("" when none).
fn mark_of(ed: &Editor, id: i64) -> String {
    ed.model
        .mark(ddc_model::TagHost::Opening(id))
        .unwrap_or_default()
}

pub fn build(ed: &Editor, cat: Cat) -> Table {
    build_in(ed, cat, true)
}

/// Same, with shared columns (phases, parameters) in the UI language (`ru`).
pub fn build_in(ed: &Editor, cat: Cat, ru: bool) -> Table {
    build_scoped(ed, cat, ru, false)
}

/// The schedule a sheet carries: rooms, doors and windows of every level, lowest level first,
/// as a building's drawing set lists them; the other categories as on the current level.
pub fn build_building(ed: &Editor, cat: Cat) -> Table {
    build_scoped(ed, cat, true, true)
}

fn build_scoped(ed: &Editor, cat: Cat, ru: bool, building: bool) -> Table {
    use Edit as E;
    use Kind as K;
    let mut cols;
    let mut rows = vec![];
    match cat {
        Cat::Walls => {
            cols = vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("type", tl(ru, "sched.col.type"), K::Text, E::Choice),
                col("len", tl(ru, "sched.col.length"), K::Len, E::Num),
                col("h", tl(ru, "sched.col.height"), K::Len, E::Num),
                col("th", tl(ru, "sched.col.thickness"), K::Len, E::No),
                col("plan", tl(ru, "sched.col.plan_area"), K::Area, E::No),
                col("side", tl(ru, "sched.col.side_area"), K::Area, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
                col("ops", tl(ru, "sched.col.openings"), K::Int, E::No),
            ];
            cols[1].choices = ed.types.iter().map(|t| t.name.clone()).collect();
            for w in &ed.walls {
                let ty = ed.types.iter().find(|t| t.id == w.wall_type);
                let len = wall_length(w);
                let ops = ed.openings.iter().filter(|o| o.wall == w.id).count();
                rows.push(Row {
                    sel: Some(Sel::Wall(w.id)),
                    id: w.id,
                    sub: 0,
                    vals: vec![
                        n(w.id as f64),
                        t(ty.map_or("—".into(), |t| t.name.clone())),
                        n(len),
                        n(w.height),
                        n(ty.map_or(0.0, |t| t.thickness())),
                        n(w.plan_area),
                        n(len * w.height),
                        n(w.volume),
                        n(ops as f64),
                    ],
                });
            }
        }
        Cat::Doors | Cat::Windows => {
            cols = vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("mark", tl(ru, "sched.col.mark"), K::Text, E::No),
                col("wall", tl(ru, "sched.col.wall"), K::Int, E::No),
                col("w", tl(ru, "sched.col.width"), K::Len, E::Num),
                col("h", tl(ru, "sched.col.height"), K::Len, E::Num),
                col("sill", tl(ru, "sched.col.sill"), K::Len, E::Num),
                col("area", tl(ru, "sched.col.opening_area"), K::Area, E::No),
                col("along", tl(ru, "sched.col.along"), K::Len, E::No),
            ];
            let door = cat == Cat::Doors;
            let every;
            let openings = if building {
                every = ed.openings_all().unwrap_or_default();
                &every
            } else {
                &ed.openings
            };
            for o in openings
                .iter()
                .filter(|o| o.kind == if door { OpKind::Door } else { OpKind::Window })
            {
                rows.push(Row {
                    sel: Some(Sel::Opening(o.id)),
                    id: o.id,
                    sub: 0,
                    vals: vec![
                        n(o.id as f64),
                        t(mark_of(ed, o.id)),
                        n(o.wall as f64),
                        n(o.width),
                        n(o.height),
                        n(o.sill),
                        n(o.width * o.height),
                        n(o.along),
                    ],
                });
            }
        }
        Cat::Curtains => {
            cols = vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("name", tl(ru, "sched.col.name"), K::Text, E::Text),
                col("len", tl(ru, "sched.col.length"), K::Len, E::No),
                col("h", tl(ru, "sched.col.height"), K::Len, E::Num),
                col("cells", tl(ru, "sched.col.cells"), K::Int, E::No),
                col("doors", tl(ru, "sched.col.doors"), K::Int, E::No),
                col("windows", tl(ru, "sched.col.windows"), K::Int, E::No),
                col("glass", tl(ru, "sched.col.glass_area"), K::Area, E::No),
                col("panel", tl(ru, "sched.col.panel_area"), K::Area, E::No),
                col("gross", tl(ru, "sched.col.gross_area"), K::Area, E::No),
                col("mull", tl(ru, "sched.col.mullion_len"), K::Len, E::No),
            ];
            for c in &ed.curtains {
                let q = &c.qty;
                rows.push(Row {
                    sel: Some(Sel::Curtain(c.id)),
                    id: c.id,
                    sub: 0,
                    vals: vec![
                        n(c.id as f64),
                        t(c.spec().name.clone()),
                        n(c.length()),
                        n(c.spec().height),
                        n(q.cells as f64),
                        n(q.doors as f64),
                        n(q.windows as f64),
                        n(q.glass_area),
                        n(q.panel_area),
                        n(q.gross_area),
                        n(q.mullion_length),
                    ],
                });
            }
        }
        Cat::Panels => {
            cols = vec![
                col("cw", tl(ru, "sched.col.curtain"), K::Int, E::No),
                col("cell", tl(ru, "sched.col.cell"), K::Text, E::No),
                col("kind", tl(ru, "sched.col.panel_kind"), K::Text, E::Choice),
                col("name", tl(ru, "sched.col.panel_type"), K::Text, E::Text),
                col("w", tl(ru, "sched.col.width"), K::Len, E::No),
                col("h", tl(ru, "sched.col.height"), K::Len, E::No),
                col("area", tl(ru, "sched.col.area"), K::Area, E::No),
            ];
            cols[2].choices = PANEL_KINDS.iter().map(|k| panel_label_in(*k, ru)).collect();
            for c in &ed.curtains {
                for (k, cell) in c.layout.layout.cells.iter().enumerate() {
                    let p = c.layout.panels.get(k).cloned().unwrap_or_default();
                    rows.push(Row {
                        sel: Some(Sel::Panel(c.id, k as u32)),
                        id: c.id,
                        sub: k as u32,
                        vals: vec![
                            n(c.id as f64),
                            t(format!("{}×{}", cell.i + 1, cell.j + 1)),
                            t(panel_label_in(p.kind, ru)),
                            t(p.name),
                            n(cell.width),
                            n(cell.height),
                            n(cell.net_area),
                        ],
                    });
                }
            }
        }
        Cat::Slabs => {
            cols = vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("th", tl(ru, "sched.col.thickness"), K::Len, E::No),
                col("top", tl(ru, "sched.col.top_offset"), K::Len, E::No),
                col("slope", tl(ru, "sched.col.slope"), K::Pct, E::Num),
                col("area", tl(ru, "sched.col.plan_area"), K::Area, E::No),
                col("surf", tl(ru, "sched.col.surface"), K::Area, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
            ];
            for s in &ed.slabs {
                rows.push(Row {
                    sel: Some(Sel::Slab(s.id)),
                    id: s.id,
                    sub: 0,
                    vals: vec![
                        n(s.id as f64),
                        n(s.thickness),
                        n(s.top),
                        n(s.slope_percent()),
                        n(s.area),
                        n(s.surface_area()),
                        n(s.volume),
                    ],
                });
            }
        }
        Cat::Rooms => {
            cols = vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("number", tl(ru, "sched.col.number"), K::Text, E::Text),
                col("name", tl(ru, "sched.col.name"), K::Text, E::Text),
                col("h", tl(ru, "sched.col.height"), K::Len, E::Num),
                col("area", tl(ru, "sched.col.area"), K::Area, E::No),
                col("per", tl(ru, "sched.col.perimeter"), K::Len, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
            ];
            let db = ed.model.sql();
            let got = db
                .prepare(
                    "SELECT r.id, r.number, r.name, r.height, q.area, q.perimeter, q.volume \
                     FROM room r LEFT JOIN room_qty q ON q.room=r.id \
                     WHERE ?2 OR r.level=?1 \
                     ORDER BY (SELECT elevation FROM level l WHERE l.id=r.level), r.id",
                )
                .and_then(|mut st| {
                    st.query_map(rusqlite::params![ed.level, building], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, f64>(3)?,
                            r.get::<_, Option<f64>>(4)?,
                            r.get::<_, Option<f64>>(5)?,
                            r.get::<_, Option<f64>>(6)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()
                });
            for (id, num, name, h, a, p, v) in got.unwrap_or_default() {
                rows.push(Row {
                    sel: Some(Sel::Room(id)),
                    id,
                    sub: 0,
                    vals: vec![
                        n(id as f64),
                        t(num),
                        t(name),
                        n(h),
                        n(a.unwrap_or(0.0)),
                        n(p.unwrap_or(0.0)),
                        n(v.unwrap_or(0.0)),
                    ],
                });
            }
        }
        c => (cols, rows) = cats::build(ed, c, ru),
    }
    let mut tb = Table {
        cat,
        cols,
        rows,
        row_phase: vec![],
        phases: vec![],
    };
    extra::extend(ed, &mut tb, ru);
    tb
}

/// Cell text as in the other panels (digit grouping, decimal comma).
pub fn fmt(v: &Val, k: Kind, u: Units) -> String {
    match v {
        Val::T(s) => s.clone(),
        Val::N(x) if x.is_nan() => "#".into(),
        Val::N(x) => match k {
            Kind::Text => fmt_num(*x, 2),
            Kind::Int => fmt_fixed(*x, 0),
            Kind::Len => edit_length(*x, u),
            Kind::Area if u == Units::Imperial => fmt_num_en(area_in_units(*x, u), precision().0),
            Kind::Vol if u == Units::Imperial => fmt_num_en(volume_in_units(*x, u), precision().1),
            Kind::Area => fmt_num(*x, precision().0),
            Kind::Vol => fmt_num(*x, precision().1),
            Kind::Pct | Kind::Mass => fmt_num(*x, 1),
            Kind::Num => fmt_num(*x, 2),
        },
    }
}

/// Number for XLSX/TSV in header units: mm or feet, m², m³, %.
pub fn export_num(x: f64, k: Kind, u: Units) -> f64 {
    match (k, u) {
        (Kind::Len, Units::Metric) => (x * 1000.0 * 1e6).round() / 1e6,
        (Kind::Len, Units::Imperial) => x / 0.3048,
        (Kind::Area, _) => area_in_units(x, u),
        (Kind::Vol, _) => volume_in_units(x, u),
        _ => x,
    }
}

pub fn header(c: &Col, u: Units) -> String {
    match c.kind.unit(u) {
        "" => c.title.to_string(),
        un => format!("{}, {un}", c.title),
    }
}

/// Book text cell compared with an exported number: any comma counts as a decimal mark.
/// Typed cells and filters read numbers through [`typed_num`] instead.
fn num_key(s: &str) -> Option<f64> {
    let t: String = s
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}'))
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    t.parse().ok().filter(|v: &f64| v.is_finite())
}

/// A number typed in a cell or a filter, in the locale of `mark` (`1,234.5` / `1 234,5`);
/// ambiguous text (`1,5` in English) is refused as in every other numeric field.
fn typed_num(s: &str, mark: DecimalMark) -> Result<f64, NumError> {
    crate::units::parse_locale_number(s, mark)
}

/// Typed text of a numeric cell to its value; `Err(true)`: refused for an ambiguous comma.
fn cell_num(text: &str, k: Kind, u: Units, mark: DecimalMark) -> Result<f64, bool> {
    let v = match k {
        Kind::Len => parse_edit_length_lang(text, u, &Scope::none(), mark)
            .map_err(|e| matches!(e, InputError::CommaDecimal)),
        _ => typed_num(text.trim().trim_end_matches('%'), mark)
            .map_err(|e| e == NumError::CommaDecimal),
    }?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(false)
    }
}

/// The filter word for a blank cell: the English one or the one of the interface language.
fn is_empty_word(f: &str) -> bool {
    let low = f.to_lowercase();
    f == "\"\""
        || low == "empty"
        || [true, false]
            .iter()
            .any(|ru| low == tl(*ru, "sch.filter.empty").to_lowercase())
}

/// Column filter: case-insensitive substring; for numbers `>5`, `<=3000`, `=2,5`
/// in display units.
/// `!...` negates the condition, `<>...` is not-equal, and the Russian keyword for "empty" matches an empty cell.
pub fn matches(v: &Val, k: Kind, f: &str, u: Units) -> bool {
    let f = f.trim();
    if f.is_empty() {
        return true;
    }
    if let Some(rest) = f.strip_prefix('!') {
        return rest.trim().is_empty() || !matches(v, k, rest, u);
    }
    if let Some(rest) = f.strip_prefix("<>") {
        return !matches(v, k, &format!("={rest}"), u);
    }
    if is_empty_word(f) {
        return matches!(v, Val::T(s) if s.trim().is_empty() || s == "—");
    }
    if let (Val::N(x), true) = (v, k.numeric()) {
        let ops: [(&str, fn(f64, f64) -> bool); 5] = [
            (">=", |a, b| a >= b - 1e-9),
            ("<=", |a, b| a <= b + 1e-9),
            (">", |a, b| a > b),
            ("<", |a, b| a < b),
            ("=", |a, b| (a - b).abs() < 1e-6 * b.abs().max(1.0)),
        ];
        for (op, cmp) in ops {
            if let Some(rest) = f.strip_prefix(op) {
                let shown = export_num(*x, k, u);
                let want = match (k, u) {
                    (Kind::Len, Units::Imperial) => parse_edit_length(rest, u).map(|m| m / 0.3048),
                    _ => typed_num(rest.trim(), ui_decimal_mark()).ok(),
                };
                return want.is_some_and(|w| cmp(shown, w));
            }
        }
    }
    let s = fmt(v, k, u).replace(['\u{a0}', '\u{202f}'], " ");
    s.to_lowercase().contains(&f.to_lowercase())
}

fn cmp_val(a: &Val, b: &Val) -> Ordering {
    match (a, b) {
        (Val::N(x), Val::N(y)) => x.total_cmp(y),
        (Val::T(x), Val::T(y)) => natural(x, y),
        (Val::N(_), Val::T(_)) => Ordering::Less,
        (Val::T(_), Val::N(_)) => Ordering::Greater,
    }
}

/// "Wall 2" < "Wall 10": digit runs are compared as numbers.
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (p, q) = (take(&mut x), take(&mut y));
                let (p, q) = (p.trim_start_matches('0'), q.trim_start_matches('0'));
                let o = p.len().cmp(&q.len()).then_with(|| p.cmp(q));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// `natural` for already-lowercase strings: no allocations.
fn natural_low(a: &str, b: &str) -> Ordering {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut k) = (0, 0);
    while i < x.len() && k < y.len() {
        if x[i].is_ascii_digit() && y[k].is_ascii_digit() {
            let (si, sk) = (i, k);
            while i < x.len() && x[i].is_ascii_digit() {
                i += 1;
            }
            while k < y.len() && y[k].is_ascii_digit() {
                k += 1;
            }
            let p = std::str::from_utf8(&x[si..i])
                .unwrap_or("")
                .trim_start_matches('0');
            let q = std::str::from_utf8(&y[sk..k])
                .unwrap_or("")
                .trim_start_matches('0');
            let o = p.len().cmp(&q.len()).then_with(|| p.cmp(q));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            if x[i] != y[k] {
                // UTF-8 bytes order the same as code points
                return x[i].cmp(&y[k]);
            }
            i += 1;
            k += 1;
        }
    }
    (x.len() - i).cmp(&(y.len() - k))
}

/// Indices of visible rows: column filters, then a stable sort.
pub fn order(tb: &Table, filters: &[String], sort: Option<(usize, bool)>, u: Units) -> Vec<u32> {
    let active: Vec<(usize, &str)> = filters
        .iter()
        .enumerate()
        .filter(|(j, f)| !f.trim().is_empty() && *j < tb.cols.len())
        .map(|(j, f)| (j, f.as_str()))
        .collect();
    let mut out: Vec<u32> = (0..tb.rows.len() as u32)
        .filter(|&i| {
            let r = &tb.rows[i as usize];
            active
                .iter()
                .all(|(j, f)| matches(&r.vals[*j], tb.cols[*j].kind, f, u))
        })
        .collect();
    if let Some((j, asc)) = sort.filter(|(j, _)| *j < tb.cols.len()) {
        // strings are lowercased once, not on every comparison
        let low: Vec<Option<String>> = tb
            .rows
            .iter()
            .map(|r| match &r.vals[j] {
                Val::T(s) => Some(s.to_lowercase()),
                Val::N(_) => None,
            })
            .collect();
        out.sort_by(|&a, &b| {
            let o = match (&low[a as usize], &low[b as usize]) {
                (Some(x), Some(y)) => natural_low(x, y),
                _ => cmp_val(&tb.rows[a as usize].vals[j], &tb.rows[b as usize].vals[j]),
            };
            if asc {
                o
            } else {
                o.reverse()
            }
        });
    }
    out
}

/// Integer reference columns (number, wall, curtain wall): not quantities, not summed.
const REFS: [&str; 3] = ["id", "wall", "cw"];

/// Sum of column `j` over `rows`: `None` if the column is not additive; NaN if a value
/// is a formula error ("#..."), so the total shows "#" rather than a sum that omits it.
pub fn sum_col(tb: &Table, rows: &[u32], j: usize) -> Option<f64> {
    let c = &tb.cols[j];
    if !c.kind.summed() || REFS.contains(&c.key.as_str()) {
        return None;
    }
    let mut s = 0.0;
    for &i in rows {
        match &tb.rows[i as usize].vals[j] {
            Val::N(x) if x.is_finite() => s += x,
            Val::N(_) => return Some(f64::NAN),
            Val::T(t) if t.starts_with('#') => return Some(f64::NAN),
            Val::T(_) => {}
        }
    }
    Some(s)
}

/// Totals over visible rows: sums of additive columns; `None` if not summable.
pub fn totals(tb: &Table, ord: &[u32]) -> Vec<Option<f64>> {
    (0..tb.cols.len()).map(|j| sum_col(tb, ord, j)).collect()
}

/// TSV for pasting into a spreadsheet: header with units, decimal-comma numbers without digit grouping.
pub fn tsv(tb: &Table, rows: &[u32], cols: &[usize], u: Units) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = cols
        .iter()
        .map(|&j| clean(&header(&tb.cols[j], u)))
        .collect::<Vec<_>>()
        .join("\t");
    for &i in rows {
        out.push('\n');
        let r = &tb.rows[i as usize];
        let line: Vec<String> = cols
            .iter()
            .map(|&j| {
                let k = tb.cols[j].kind;
                match (&r.vals[j], k, u) {
                    (v, Kind::Len, Units::Imperial) => clean(&fmt(v, k, u)),
                    (Val::N(x), _, _) => {
                        let d = match k {
                            Kind::Int => 0,
                            Kind::Len => 1,
                            Kind::Area => precision().0,
                            Kind::Vol => precision().1,
                            _ => 2,
                        };
                        let s = fmt_fixed(export_num(*x, k, u), d);
                        let s = if s.contains('.') && k == Kind::Len {
                            s.trim_end_matches('0').trim_end_matches('.').to_string()
                        } else {
                            s
                        };
                        s.replace('.', ",")
                    }
                    (Val::T(s), _, _) => clean(s),
                }
            })
            .collect();
        out.push_str(&line.join("\t"));
    }
    out
}

/// Export of visible rows and columns to .xlsx: numbers stay numeric in header units, totals at the bottom.
pub fn export_xlsx(
    tb: &Table,
    rows: &[u32],
    cols: &[usize],
    u: Units,
    path: &std::path::Path,
) -> Result<(), String> {
    export_xlsx_grouped(tb, rows, cols, u, path, &[])
}

/// As `export_xlsx`, with the group headers and subtotal rows the schedule shows (`lines` from
/// `group::lines`; empty: plain table).
pub fn export_xlsx_grouped(
    tb: &Table,
    rows: &[u32],
    cols: &[usize],
    u: Units,
    path: &std::path::Path,
    lines: &[group::Line],
) -> Result<(), String> {
    export_book(tb, rows, cols, u, path, lines, None)
}

/// As `export_xlsx_grouped`, with the leading Id column that `xlsx_in` reads the book back by,
/// and the hidden key sheet: project uid, change head and every exported cell of `model`.
pub fn export_xlsx_keyed(
    model: &ddc_model::Model,
    tb: &Table,
    rows: &[u32],
    cols: &[usize],
    u: Units,
    path: &std::path::Path,
    lines: &[group::Line],
) -> Result<(), String> {
    export_book(tb, rows, cols, u, path, lines, Some(model))
}

fn export_book(
    tb: &Table,
    rows: &[u32],
    cols: &[usize],
    u: Units,
    path: &std::path::Path,
    lines: &[group::Line],
    keyed: Option<&ddc_model::Model>,
) -> Result<(), String> {
    use ddc_io::xlsx::{TableCell as C, TableLine as L};
    let headers: Vec<String> = cols.iter().map(|&j| header(&tb.cols[j], u)).collect();
    let (ad, vd) = precision();
    let area = format!("0.{}", "0".repeat(ad));
    let vol = format!("0.{}", "0".repeat(vd));
    let formats: Vec<&str> = cols
        .iter()
        .map(|&j| match tb.cols[j].kind {
            Kind::Int => "0",
            Kind::Len if u == Units::Metric => "0",
            Kind::Len => "0.000",
            Kind::Area if ad > 0 => area.as_str(),
            Kind::Vol if vd > 0 => vol.as_str(),
            Kind::Pct => "0.0",
            _ => "",
        })
        .collect();
    let cell = |v: &Val, k: Kind| match v {
        Val::N(x) => C::Num(export_num(*x, k, u)),
        Val::T(s) => C::Text(s.clone()),
    };
    let data: Vec<Vec<C>> = rows
        .iter()
        .map(|&i| {
            let r = &tb.rows[i as usize];
            cols.iter()
                .map(|&j| cell(&r.vals[j], tb.cols[j].kind))
                .collect()
        })
        .collect();
    let tot = totals(tb, rows);
    let trow: Vec<C> = cols
        .iter()
        .enumerate()
        .map(|(p, &j)| match tot[j] {
            Some(x) if x.is_nan() => C::Text("#".into()),
            Some(x) => C::Subtotal(109, export_num(x, tb.cols[j].kind, u)),
            None if p == 0 => {
                let lab = crate::tr("sch.total", &[("n", "\u{1f}")]);
                let (pre, post) = lab.split_once('\u{1f}').unwrap_or((&lab, ""));
                C::CountLabel { pre: pre.into(), post: post.into(), n: rows.len() }
            }
            None => C::Text(String::new()),
        })
        .collect();
    let sheet: String = tb.cat.title().chars().take(31).collect();
    let mut out: Vec<L> = lines
        .iter()
        .map(|l| match l {
            group::Line::Row(pos) => L::Row(*pos),
            group::Line::Head { depth, text, n } => L::Head {
                text: format!("{}{text} \u{b7} {n}", "  ".repeat(*depth as usize)),
            },
            group::Line::Sub { depth, tot, n } => L::Sub {
                cells: cols
                    .iter()
                    .enumerate()
                    .map(|(p, &j)| match tot.get(j).copied().flatten() {
                        Some(x) if x.is_nan() => C::Text("#".into()),
                        Some(x) => C::Subtotal(109, export_num(x, tb.cols[j].kind, u)),
                        None if p == 0 => {
                            let lab = crate::tr("sch.group_total", &[("n", &n.to_string())]);
                            C::Text(format!("{}{lab}", "  ".repeat(*depth as usize)))
                        }
                        None => C::Text(String::new()),
                    })
                    .collect(),
            },
        })
        .collect();
    let (mut headers, mut formats, mut data, mut trow) = (headers, formats, data, trow);
    if let Some(model) = keyed {
        // leading element Id: the import matches rows by it. Total and subtotal lines leave it
        // blank; a group header writes its text there and the import skips such text-only rows.
        headers.insert(0, xlsx_in::ID_HEADER.to_string());
        formats.insert(0, "0");
        for (row, &i) in data.iter_mut().zip(rows) {
            let r = &tb.rows[i as usize];
            row.insert(0, xlsx_in::id_cell(tb.cat, r.id, r.sub));
        }
        trow.insert(0, C::Text(String::new()));
        for l in &mut out {
            if let L::Sub { cells } = l {
                cells.insert(0, C::Text(String::new()));
            }
        }
        if out.is_empty() {
            out = (0..data.len()).map(L::Row).collect();
        }
        let key = xlsx_in::key_grid(model, &headers, &data);
        return ddc_io::xlsx::export_keyed_table_lines_with_key(
            path, &sheet, &headers, &formats, &data, &out, Some(&trow), &key,
        );
    }
    if lines.is_empty() {
        return ddc_io::xlsx::export_table(path, &sheet, &headers, &formats, &data, Some(&trow));
    }
    ddc_io::xlsx::export_table_lines(path, &sheet, &headers, &formats, &data, &out, Some(&trow))
}

/// Apply a cell edit via an editor command (a single undoable operation).
pub fn apply_edit(ed: &mut Editor, tb: &Table, row: usize, col: usize, text: &str) {
    let (Some(r), Some(c)) = (tb.rows.get(row), tb.cols.get(col)) else {
        return;
    };
    if extra::apply(ed, tb, r, c, text.trim()) {
        return;
    }
    let (u, mark) = (ed.tool.units, ui_decimal_mark());
    let num = || cell_num(text, c.kind, u, mark).ok();
    let bad = |ed: &mut Editor| {
        ed.status = Some(match cell_num(text, c.kind, u, mark) {
            Err(true) => crate::tr("decimal_input.comma_decimal", &[]),
            _ => crate::tr("sch.edit.not_number", &[("text", text)]),
        })
    };
    let text = text.trim().to_string();
    match (tb.cat, c.key.as_str()) {
        (Cat::Walls, "type") => {
            match ed.types.iter().find(|t| t.name.eq_ignore_ascii_case(&text)) {
                Some(t) => {
                    let t = t.id;
                    ed.edit_wall(r.id, None, Some(t), None)
                }
                None => ed.status = Some(crate::tr("sch.edit.no_type", &[("text", &text)])),
            }
        }
        (Cat::Walls, k @ ("len" | "h")) => match num() {
            Some(v) if k == "len" => ed.edit_wall(r.id, Some(v), None, None),
            Some(v) => ed.edit_wall(r.id, None, None, Some(v)),
            None => bad(ed),
        },
        (Cat::Doors | Cat::Windows, k) => {
            let (Some(o), Some(v)) = (ed.opening(r.id).copied(), num()) else {
                return bad(ed);
            };
            let mut d = [o.width, o.height, o.sill];
            d[match k {
                "w" => 0,
                "h" => 1,
                _ => 2,
            }] = v;
            ed.set_opening_dims(o.id, d[0], d[1], d[2]);
        }
        (Cat::Curtains, "name") => ed.edit_curtain(r.id, |s| s.name = text),
        (Cat::Curtains, "h") => match num() {
            Some(v) if v.is_finite() && v > 0.0 => ed.edit_curtain(r.id, |s| s.height = v),
            _ => bad(ed),
        },
        (Cat::Panels, "kind") => {
            match panel_kind_parse(&text) {
                Some(k) => ed.set_panel_kind(r.id, r.sub, k),
                None => ed.status = Some(panel_kind_unknown_status(crate::ui_lang_now(), &text)),
            }
        }
        (Cat::Panels, "name") => ed.set_panel(r.id, r.sub, |p| p.name = text),
        (Cat::Slabs, "slope") => match num() {
            Some(v) => ed.set_slab_slope_percent(r.id, v),
            None => bad(ed),
        },
        (Cat::Rooms, "name") => ed.edit_room(r.id, |s| s.name = text),
        (Cat::Rooms, "number") => ed.edit_room(r.id, |s| s.number = text),
        (Cat::Rooms, "h") => match num() {
            Some(v) if v.is_finite() && v > 0.0 => ed.edit_room(r.id, |s| s.height = v),
            _ => bad(ed),
        },
        _ => {}
    }
}

/// Text for the edit field: like the display text, but lengths use the input-field format.
pub fn edit_text(v: &Val, k: Kind, u: Units) -> String {
    let plain = |v: &Val| fmt(v, k, u).replace(['\u{a0}', '\u{202f}'], " ");
    let x = match v {
        Val::N(x) if !x.is_nan() => *x,
        _ => return plain(v),
    };
    let (n, d) = match k {
        Kind::Len | Kind::Int => return plain(v),
        Kind::Area => (area_in_units(x, u), precision().0),
        Kind::Vol => (volume_in_units(x, u), precision().1),
        Kind::Pct | Kind::Mass => (x, 1),
        Kind::Text | Kind::Num => (x, 2),
    };
    // ungrouped in English: the field shows what is typed, `typed_num` reads both back
    match crate::units::ui_decimal_mark() {
        crate::units::DecimalMark::Comma => crate::units::fmt_num_ru(n, d).replace('\u{a0}', " "),
        crate::units::DecimalMark::Point => fmt_fixed(n, d),
    }
}

/// A four-wall room for other modules' tests.
#[cfg(test)]
pub(crate) fn test_room(e: &mut Editor) {
    tests::room(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn room(e: &mut Editor) {
        use crate::wall_tool::{Event, Key};
        let typed = |e: &mut Editor, s: &str| {
            for c in s.chars() {
                e.handle(Event::Char(c));
            }
            e.handle(Event::Key(Key::Enter));
        };
        e.current_type = 1;
        e.handle(Event::Click(ddc_geom::v2(0.0, 0.0)));
        e.handle(Event::Move(ddc_geom::v2(1.0, 0.02)));
        typed(e, "6000");
        e.handle(Event::Move(ddc_geom::v2(6.03, 1.0)));
        typed(e, "4000");
        e.handle(Event::Move(ddc_geom::v2(3.0, 4.04)));
        typed(e, "6000");
        e.handle(Event::Char('C'));
    }

    #[test]
    fn walls_table_edit_sort_filter_totals_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        room(&mut e);
        let tb = build(&e, Cat::Walls);
        assert_eq!(tb.rows.len(), 4, "four walls");
        let len = tb.cols.iter().position(|c| c.key == "len").unwrap();
        let ord = order(&tb, &[], Some((len, false)), Units::Metric);
        assert_eq!(tb.rows[ord[0] as usize].vals[len], Val::N(6.0));
        let mut f = vec![String::new(); tb.cols.len()];
        f[len] = ">5000".into();
        assert_eq!(order(&tb, &f, None, Units::Metric).len(), 2);
        f[len] = "4 000".into();
        assert_eq!(order(&tb, &f, None, Units::Metric).len(), 2);
        let tot = totals(&tb, &order(&tb, &[], None, Units::Metric));
        assert!((tot[len].unwrap() - 20.0).abs() < 1e-9);
        // length edit: a single operation, Ctrl+Z reverts it
        let r = ord[0] as usize;
        apply_edit(&mut e, &tb, r, len, "7 000");
        let tb2 = build(&e, Cat::Walls);
        assert_eq!(tb2.rows[r].vals[len], Val::N(7.0), "{:?}", e.status);
        e.undo();
        assert_eq!(build(&e, Cat::Walls).rows[r].vals[len], Val::N(6.0));
        // type by name
        let ty = tb.cols.iter().position(|c| c.key == "type").unwrap();
        let name = tb.cols[ty].choices[0].clone();
        apply_edit(&mut e, &tb, 0, ty, &name);
        assert_eq!(build(&e, Cat::Walls).rows[0].vals[ty], Val::T(name));
        let s = tsv(&tb, &[0], &[0, len], Units::Metric);
        assert!(s.starts_with("\u{2116}\t\u{414}\u{43b}\u{438}\u{43d}\u{430}, \u{43c}\u{43c}\n"), "{s}");
    }

    #[test]
    fn num_key_rejects_non_finite() {
        assert_eq!(num_key("2,5"), Some(2.5));
        for s in ["inf", "-inf", "NaN", "infinity", "1e999"] {
            assert_eq!(num_key(s), None, "{s}");
        }
    }

    /// `f` with the decimal mark of this thread set to `mark`; the previous one comes back.
    fn with_mark<R>(mark: DecimalMark, f: impl FnOnce() -> R) -> R {
        let prev = ui_decimal_mark();
        crate::units::set_ui_decimal_mark(mark);
        let out = f();
        crate::units::set_ui_decimal_mark(prev);
        out
    }

    #[test]
    fn typed_numbers_follow_the_ui_mark() {
        let en = |s: &str| typed_num(s, DecimalMark::Point);
        let ru = |s: &str| typed_num(s, DecimalMark::Comma);
        assert_eq!(en("1,234"), Ok(1234.0), "an English comma groups thousands");
        assert_eq!(en("1,234.5"), Ok(1234.5));
        assert_eq!(en("-1,000.5"), Ok(-1000.5));
        assert_eq!(en("2.000"), Ok(2.0));
        assert_eq!(en("0"), Ok(0.0));
        assert_eq!(en("1,5"), Err(NumError::CommaDecimal), "ambiguous, never 1.5 or 15");
        assert_eq!(ru("1 234,5"), Ok(1234.5));
        assert_eq!(ru("1\u{a0}234"), Ok(1234.0));
        assert_eq!(ru("-1\u{a0}000,5"), Ok(-1000.5));
        assert_eq!(ru("2,50"), Ok(2.5));
        assert_eq!(ru("0,0"), Ok(0.0));
        assert!(en("abc").is_err() && ru("").is_err());
        // book cells keep their own rule (open question for dot-mark books)
        assert_eq!(num_key("2,5"), Some(2.5));
    }

    #[test]
    fn numeric_filter_reads_the_ui_mark() {
        let m = Units::Metric;
        with_mark(DecimalMark::Point, || {
            assert!(matches(&Val::N(1234.0), Kind::Num, "=1,234", m), "not 1.234");
            assert!(!matches(&Val::N(1.234), Kind::Num, "=1,234", m));
            assert!(matches(&Val::N(1234.5), Kind::Num, ">=1,234.5", m));
            assert!(matches(&Val::N(-5.0), Kind::Num, ">-1,000.5", m));
            assert!(matches(&Val::N(2.0), Kind::Num, "=2.000", m));
            assert!(matches(&Val::N(0.0), Kind::Num, "=0", m));
            assert!(!matches(&Val::N(1.5), Kind::Num, "=1,5", m), "refused, matches nothing");
            assert!(!matches(&Val::N(15.0), Kind::Num, "=1,5", m));
            assert!(matches(&Val::N(1234.0), Kind::Num, "<>1,000", m));
        });
        with_mark(DecimalMark::Comma, || {
            assert!(matches(&Val::N(1234.5), Kind::Num, "=1 234,5", m));
            assert!(matches(&Val::N(1234.0), Kind::Num, "=1\u{a0}234", m));
            assert!(matches(&Val::N(-5.0), Kind::Num, ">-1 000,5", m));
            assert!(matches(&Val::N(2.5), Kind::Num, "=2,50", m));
            assert!(matches(&Val::N(0.0), Kind::Num, "<=0,0", m));
        });
    }

    #[test]
    fn cell_edit_reads_the_ui_mark() {
        let mut e = Editor::new_in_memory().unwrap();
        room(&mut e);
        let tb = build(&e, Cat::Walls);
        let len = tb.cols.iter().position(|c| c.key == "len").unwrap();
        let val = |e: &Editor| build(e, Cat::Walls).rows[0].vals[len].clone();
        crate::with_test_lang(false, || {
            with_mark(DecimalMark::Point, || {
                apply_edit(&mut e, &tb, 0, len, "7,000");
                assert_eq!(val(&e), Val::N(7.0), "{:?}", e.status);
                apply_edit(&mut e, &tb, 0, len, "7,250.0");
                assert_eq!(val(&e), Val::N(7.25), "{:?}", e.status);
                e.status = None;
                apply_edit(&mut e, &tb, 0, len, "7,5");
                assert_eq!(val(&e), Val::N(7.25), "ambiguous text changes nothing");
                let why = crate::tr_lang(false, "decimal_input.comma_decimal", &[]);
                assert_eq!(e.status.as_deref(), Some(why.as_str()));
            });
        });
        crate::with_test_lang(true, || {
            with_mark(DecimalMark::Comma, || {
                apply_edit(&mut e, &tb, 0, len, "6\u{a0}500,5");
                let Val::N(v) = val(&e) else { panic!("{:?}", e.status) };
                assert!((v - 6.5005).abs() < 1e-6, "{v} {:?}", e.status);
            });
        });
        assert_eq!(cell_num("-1,000.5", Kind::Num, Units::Metric, DecimalMark::Point), Ok(-1000.5));
        assert_eq!(cell_num("12,5 %", Kind::Pct, Units::Metric, DecimalMark::Comma), Ok(12.5));
        assert_eq!(cell_num("0.00", Kind::Area, Units::Metric, DecimalMark::Point), Ok(0.0));
        assert_eq!(cell_num("1,5", Kind::Mass, Units::Metric, DecimalMark::Point), Err(true));
        assert_eq!(cell_num("x", Kind::Num, Units::Metric, DecimalMark::Point), Err(false));
    }

    #[test]
    fn edit_text_reads_back_in_both_languages() {
        use crate::units::{set_ui_decimal_mark, DecimalMark};
        let kinds = [Kind::Area, Kind::Vol, Kind::Pct, Kind::Mass, Kind::Num, Kind::Text];
        for (mark, ru) in [(DecimalMark::Comma, true), (DecimalMark::Point, false)] {
            set_ui_decimal_mark(mark);
            crate::with_test_lang(ru, || {
                for k in kinds {
                    let t = edit_text(&Val::N(1234.5), k, Units::Metric);
                    let back = typed_num(t.trim(), mark).unwrap_or_else(|e| panic!("{t:?} {e:?}"));
                    assert!((back - 1234.5).abs() < 1.0, "{t:?} read as {back}");
                }
            });
        }
    }

    #[test]
    fn natural_order_and_filters() {
        assert_eq!(natural("\u{421}\u{442}\u{435}\u{43d}\u{430} 2", "\u{421}\u{442}\u{435}\u{43d}\u{430} 10"), Ordering::Less);
        assert_eq!(natural_low("\u{441}\u{442}\u{435}\u{43d}\u{430} 2", "\u{441}\u{442}\u{435}\u{43d}\u{430} 10"), Ordering::Less);
        assert_eq!(natural_low("\u{430}", "\u{431}"), Ordering::Less);
        assert!(matches(&Val::N(2.5), Kind::Area, "=2,5", Units::Metric));
        assert!(matches(
            &Val::T("\u{41a}\u{43b}\u{430}\u{434}\u{43a}\u{430} 200".into()),
            Kind::Text,
            "\u{43a}\u{43b}\u{430}\u{434}\u{43a}\u{430}",
            Units::Metric
        ));
        assert!(!matches(&Val::N(1.0), Kind::Len, "<500", Units::Metric));
        assert!(matches(&Val::N(1.0), Kind::Len, "!<500", Units::Metric));
        assert!(matches(&Val::N(1.0), Kind::Len, "<>500", Units::Metric));
        assert!(!matches(
            &Val::T("\u{41a}\u{43b}\u{430}\u{434}\u{43a}\u{430}".into()),
            Kind::Text,
            "!\u{43a}\u{43b}\u{430}\u{434}",
            Units::Metric
        ));
        assert!(matches(
            &Val::T(String::new()),
            Kind::Text,
            "\u{43f}\u{443}\u{441}\u{442}\u{43e}",
            Units::Metric
        ));
    }

    #[test]
    fn export_xlsx_writes_file() {
        let mut e = Editor::new_in_memory().unwrap();
        room(&mut e);
        let tb = build(&e, Cat::Walls);
        let ord = order(&tb, &[], None, Units::Metric);
        let p = std::env::temp_dir().join(format!("ddc_sched_{}.xlsx", std::process::id()));
        let cols: Vec<usize> = (0..tb.cols.len()).collect();
        export_xlsx(&tb, &ord, &cols, Units::Metric, &p).unwrap();
        assert!(std::fs::metadata(&p).unwrap().len() > 1000);
        let _ = std::fs::remove_file(&p);
    }

    /// (sheet xml, workbook xml) of an .xlsx.
    fn book_xml(p: &std::path::Path) -> (String, String) {
        use std::io::Read;
        let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
        let mut rd = |n: &str| {
            let mut s = String::new();
            z.by_name(n).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        (rd("xl/worksheets/sheet1.xml"), rd("xl/workbook.xml"))
    }

    /// (formula, cached value) of every SUBTOTAL cell in document order.
    fn subtotals(sheet: &str) -> Vec<(String, String)> {
        let mut out = vec![];
        let mut rest = sheet;
        while let Some(i) = rest.find("<f>") {
            rest = &rest[i + 3..];
            let (f, tail) = rest.split_once("</f>").unwrap();
            let v = tail.split_once("<v>").map_or("", |(_, v)| v.split("</v>").next().unwrap());
            out.push((f.to_string(), v.to_string()));
        }
        out
    }

    fn export_walls(rows_of: impl Fn(&Table) -> Vec<u32>, tag: &str) -> (Table, Vec<u32>, std::path::PathBuf) {
        let mut e = Editor::new_in_memory().unwrap();
        room(&mut e);
        let tb = build(&e, Cat::Walls);
        let ord = rows_of(&tb);
        let p = std::env::temp_dir().join(format!("ddc_sched_{tag}_{}.xlsx", std::process::id()));
        let cols: Vec<usize> = (0..tb.cols.len()).collect();
        export_xlsx(&tb, &ord, &cols, Units::Metric, &p).unwrap();
        (tb, ord, p)
    }

    #[test]
    fn schedule_totals_row_is_subtotal_formula() {
        let (tb, ord, p) = export_walls(|tb| order(tb, &[], None, Units::Metric), "f");
        let (sheet, wbk) = book_xml(&p);
        let n = ord.len();
        let f = subtotals(&sheet);
        let sums: Vec<_> = f.iter().filter(|(f, _)| f.starts_with("SUBTOTAL(109,")).collect();
        let want = totals(&tb, &ord).iter().filter(|t| t.is_some()).count();
        assert!(want > 0 && sums.len() == want, "{f:?}");
        for (f, _) in &sums {
            assert!(f.contains("2:") && f.ends_with(&format!("{})", n + 1)), "{f}");
        }
        assert!(f.iter().any(|(f, _)| f.contains("SUBTOTAL(103,")), "{f:?}");
        assert!(wbk.contains("ddc_total_row"), "{wbk}");
        // totals row (sheet row n+2) stays outside the autofilter range
        assert!(sheet.contains(&format!("<c r=\"A{}\"", n + 2)));
        let af = sheet.split("<autoFilter ref=\"").nth(1).unwrap().split('"').next().unwrap();
        assert!(af.starts_with("A1:") && af.ends_with(&(n + 1).to_string()), "{af}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schedule_totals_cached_value_matches_ui() {
        let (tb, ord, p) = export_walls(|tb| order(tb, &[], None, Units::Metric), "c");
        let (sheet, _) = book_xml(&p);
        let mut got: Vec<f64> = subtotals(&sheet)
            .iter()
            .filter(|(f, _)| f.starts_with("SUBTOTAL(109,"))
            .map(|(_, v)| v.parse().unwrap())
            .collect();
        let mut want: Vec<f64> = totals(&tb, &ord)
            .iter()
            .enumerate()
            .filter_map(|(j, t)| t.map(|x| export_num(x, tb.cols[j].kind, Units::Metric)))
            .collect();
        got.sort_by(f64::total_cmp);
        want.sort_by(f64::total_cmp);
        assert_eq!(got.len(), want.len());
        for (a, b) in got.iter().zip(&want) {
            assert!((a - b).abs() < 1e-9, "{got:?} vs {want:?}");
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schedule_totals_empty_schedule_has_no_self_reference() {
        let (_, _, p) = export_walls(|_| vec![], "e");
        let (sheet, wbk) = book_xml(&p);
        assert!(subtotals(&sheet).is_empty(), "{sheet}");
        assert!(wbk.contains("ddc_total_row"));
        let _ = std::fs::remove_file(&p);
    }

    /// Five rows in three groups (a: 1+3, b: 2+4, c: 5), sorted by the group column.
    fn grouped_fixture(tag: &str, grouped: bool) -> (Table, Vec<u32>, Vec<group::Line>, std::path::PathBuf) {
        let cols = vec![
            col("type", "Type", Kind::Text, Edit::No),
            col("len", "Len", Kind::Len, Edit::No),
        ];
        let rows = [("b", 1.0), ("a", 2.0), ("b", 3.0), ("a", 4.0), ("c", 5.0)]
            .iter()
            .enumerate()
            .map(|(i, (t, l))| Row {
                sel: None,
                id: i as i64,
                sub: 0,
                vals: vec![Val::T((*t).into()), Val::N(*l)],
            })
            .collect();
        let tb = Table { cat: Cat::Walls, cols, rows, row_phase: vec![None; 5], phases: vec![] };
        let mut ord: Vec<u32> = (0..5).collect();
        let by: &[usize] = if grouped { &[0] } else { &[] };
        group::sort(&tb, &mut ord, by);
        let lines = group::lines(&tb, &ord, by, true, Units::Metric);
        let p = std::env::temp_dir().join(format!("ddc_sched_grp_{tag}_{}.xlsx", std::process::id()));
        export_xlsx_grouped(&tb, &ord, &[0, 1], Units::Metric, &p, &lines).unwrap();
        (tb, ord, lines, p)
    }

    #[test]
    fn grouped_export_has_group_subtotals() {
        let (_, _, lines, p) = grouped_fixture("s", true);
        assert_eq!(lines.len(), 11);
        let (sheet, wbk) = book_xml(&p);
        let f: Vec<String> = subtotals(&sheet).into_iter().map(|(f, _)| f).collect();
        for want in ["SUBTOTAL(109,B3:B4)", "SUBTOTAL(109,B7:B8)", "SUBTOTAL(109,B11:B11)"] {
            assert!(f.iter().any(|x| x == want), "{want} in {f:?}");
        }
        // header and subtotal rows are marked: 3 headers + 3 subtotals
        assert_eq!(wbk.matches("ddc_group_row_").count(), 6, "{wbk}");
        // the writer keeps cell text in the shared string table, not inline in the sheet
        let strings = {
            use std::io::Read;
            let mut z = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
            let mut s = String::new();
            z.by_name("xl/sharedStrings.xml").unwrap().read_to_string(&mut s).unwrap();
            s
        };
        assert!(strings.contains("a \u{b7} 2"), "{strings}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn xlsx_group_rows_match_ui_groups() {
        let cols = vec![
            col("type", "Type", Kind::Text, Edit::No),
            col("len", "Len", Kind::Len, Edit::No),
        ];
        let rows = [("Room A", 1.0), ("room a", 2.0), ("Room B", 3.0), ("Room A", 4.0)]
            .iter()
            .enumerate()
            .map(|(i, (t, l))| Row {
                sel: None,
                id: i as i64,
                sub: 0,
                vals: vec![Val::T((*t).into()), Val::N(*l)],
            })
            .collect();
        let tb = Table { cat: Cat::Walls, cols, rows, row_phase: vec![None; 4], phases: vec![] };
        let mut ord: Vec<u32> = (0..4).collect();
        group::sort(&tb, &mut ord, &[0]);
        let lines = group::lines(&tb, &ord, &[0], true, Units::Metric);
        let heads = lines.iter().filter(|l| matches!(l, group::Line::Head { .. })).count();
        assert_eq!(heads, 2);
        let p = std::env::temp_dir().join(format!("ddc_sched_grpc_{}.xlsx", std::process::id()));
        export_xlsx_grouped(&tb, &ord, &[0, 1], Units::Metric, &p, &lines).unwrap();
        let (sheet, wbk) = book_xml(&p);
        assert_eq!(wbk.matches("ddc_group_row_").count(), heads * 2, "{wbk}");
        let f: Vec<String> = subtotals(&sheet).into_iter().map(|(f, _)| f).collect();
        assert!(f.iter().any(|x| x == "SUBTOTAL(109,B3:B5)"), "{f:?}");
        assert!(f.iter().any(|x| x == "SUBTOTAL(109,B8:B8)"), "{f:?}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn grand_total_not_double_counted() {
        let (_, _, _, p) = grouped_fixture("g", true);
        let (sheet, _) = book_xml(&p);
        let s = subtotals(&sheet);
        // one grand total over the whole data range; nested subtotals are ignored by SUBTOTAL
        let grand: Vec<_> = s.iter().filter(|(f, _)| f == "SUBTOTAL(109,B2:B12)").collect();
        assert_eq!(grand.len(), 1, "{s:?}");
        assert_eq!(grand[0].1, "15000");
        // the count label skips the group rows and reports element rows only
        let cnt = s.iter().find(|(f, _)| f.contains("SUBTOTAL(103,")).unwrap();
        assert!(cnt.0.contains("{2,5,6,9,10,12}"), "{}", cnt.0);
        assert!(sheet.contains(": 5"), "{sheet}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn ungrouped_export_unchanged() {
        let (tb, ord, lines, p) = grouped_fixture("u", false);
        assert!(lines.is_empty());
        let plain = std::env::temp_dir().join(format!("ddc_sched_grp_plain_{}.xlsx", std::process::id()));
        export_xlsx(&tb, &ord, &[0, 1], Units::Metric, &plain).unwrap();
        let (a, wa) = book_xml(&p);
        let (b, wb) = book_xml(&plain);
        assert_eq!(a, b);
        assert_eq!(wa, wb);
        assert!(!wa.contains("ddc_group_row_") && !a.contains("ISNA"), "{a}");
        assert!(a.contains("SUBTOTAL(109,B2:B6)"), "{a}");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&plain);
    }

    #[test]
    fn every_tab_title_and_column_is_translated_in_both_languages() {
        let e = Editor::new_in_memory().unwrap();
        let cyr = |s: &str| s.chars().any(|c| (0x400..=0x4ff).contains(&(c as u32)));
        for c in Cat::ALL {
            let (en, ru) = (c.title_in(false), c.title_in(true));
            assert!(!en.starts_with("sched.") && !ru.starts_with("sched."), "{c:?}");
            assert!(!cyr(&en), "{c:?}: {en}");
            assert!(cyr(&ru), "{c:?}: {ru}");
            let tb = build_in(&e, c, false);
            for k in &tb.cols {
                assert!(!cyr(&k.title) && !k.title.contains("sched."), "{c:?}: {}", k.title);
            }
            let tb = build_in(&e, c, true);
            for k in &tb.cols {
                assert!(!k.title.contains("sched."), "{c:?}: {}", k.title);
            }
        }
    }

    #[test]
    fn schedule_panel_choices_localized() {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0)]);
        let id = e.curtains[0].id;
        let en = build_in(&e, Cat::Panels, false);
        let ru = build_in(&e, Cat::Panels, true);
        let kind = en.cols.iter().position(|c| c.key == "kind").unwrap();
        assert_eq!(en.cols[kind].choices.len(), PANEL_KINDS.len());
        assert!(en.cols[kind]
            .choices
            .iter()
            .all(|c| !c.chars().any(|ch| ('\u{0400}'..='\u{04FF}').contains(&ch))));
        assert_ne!(en.cols[kind].choices, ru.cols[kind].choices);
        // typed text in either language or the key sets the type
        for text in ["Door", "\u{414}\u{432}\u{435}\u{440}\u{44c}", "door"] {
            e.set_panel_kind(id, 1, ddc_model::PanelKind::Glazed);
            let tb = build_in(&e, Cat::Panels, false);
            apply_edit(&mut e, &tb, 1, kind, text);
            assert_eq!(e.curtain(id).unwrap().qty.doors, 1, "{text}");
        }
    }

    #[test]
    fn panel_kind_unknown_status_key() {
        for ru in [false, true] {
            let st = panel_kind_unknown_status(ru, "zzz");
            assert!(!st.contains("sch.edit.no_panel_kind"), "{st}");
            assert!(st.contains("zzz") && !st.contains('{'), "{st}");
            assert_ne!(st, crate::tr_lang(ru, "sch.edit.no_type", &[("text", "zzz")]));
        }
    }

    #[test]
    fn panel_kind_list_localized() {
        let cyr = |c: char| ('\u{0400}'..='\u{04FF}').contains(&c);
        let en = panel_kind_list(false);
        let ru = panel_kind_list(true);
        assert_ne!(en, ru);
        assert!(!en.chars().any(cyr));
        assert!(ru.chars().any(cyr));
        assert_eq!(en.split(", ").count(), PANEL_KINDS.len());
        assert_eq!(ru.split(", ").count(), PANEL_KINDS.len());
    }

    #[test]
    fn panel_kind_known_sets() {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0)]);
        let id = e.curtains[0].id;
        let tb = build_in(&e, Cat::Panels, false);
        let kind = tb.cols.iter().position(|c| c.key == "kind").unwrap();
        e.status = None;
        apply_edit(&mut e, &tb, 1, kind, "door");
        // a known kind is set: the panel's own done status, never the unknown-kind refusal
        assert_ne!(e.status, Some(panel_kind_unknown_status(crate::ui_lang_now(), "door")));
        assert_eq!(e.curtain(id).unwrap().qty.doors, 1);
        apply_edit(&mut e, &tb, 1, kind, "zzz");
        assert!(e.status.as_deref().unwrap_or("").contains("zzz"));
    }

    #[test]
    fn old_russian_project_reads_and_schedules() {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.0)]);
        let id = e.curtains[0].id;
        let old = "\u{41d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{430}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{430}";
        e.edit_curtain(id, |s| s.name = old.into());
        e.set_panel_kind(id, 1, ddc_model::PanelKind::Door);
        e.reload().unwrap();
        assert_eq!(e.curtain(id).unwrap().spec().name, old);
        let tb = build_in(&e, Cat::Panels, true);
        let kind = tb.cols.iter().position(|c| c.key == "kind").unwrap();
        assert_eq!(tb.rows.len(), 12);
        assert_eq!(tb.rows[1].vals[kind], t("\u{414}\u{432}\u{435}\u{440}\u{44c}"));
        let en = build_in(&e, Cat::Panels, false);
        assert_eq!(en.rows[1].vals[kind], t("Door"));
    }
}
