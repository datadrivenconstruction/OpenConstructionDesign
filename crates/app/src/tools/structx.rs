//! Structure (schema 26): pad footing (click; at a column — under it), strip footing (click on a
//! wall or two points), slab (outline), brace (two points; ends attach to columns and
//! beams), truss (bottom chord by two points), beam system (outline). Writing is one undo step.

use super::opt_field::{self, FULL_TURN};
use super::{arm, ccw, column, commit, tup, Active, Kind, Opts, Rec};
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::struct_props::{layout_of, LAYOUT_KEYS};
use crate::units::Units;
use ddc_geom::arc::ArcAxis;
use ddc_geom::{v2, Tol, V2};
use ddc_model::{
    BeamLayout, BeamSystemSpec, BraceSpec, FootingSpec, FootingStep, GridForm, GridKind,
    GridPattern, GridShellSpec, Just, RaftSpec, StripFootingSpec, TrussChord, TrussSpec, TrussWeb,
};
use egui::{self, Color32, Pos2, Stroke};

/// Footing material (model material name; density is derived from it).
pub const CONCRETE: &str = "Concrete";

/// Pad footings: label, round, steps bottom to top (w, l, h), m.
#[allow(clippy::type_complexity)]
const FOOTINGS: [(&str, bool, &[(f64, f64, f64)]); 5] = [
    ("1200×1200×500", false, &[(1.2, 1.2, 0.5)]),
    ("1500×1500×600", false, &[(1.5, 1.5, 0.6)]),
    (
        "1800×1800 / 1000×1000",
        false,
        &[(1.8, 1.8, 0.3), (1.0, 1.0, 0.3)],
    ),
    (
        "2400×2400 / 1600×1600 / 900×900",
        false,
        &[(2.4, 2.4, 0.3), (1.6, 1.6, 0.3), (0.9, 0.9, 0.3)],
    ),
    ("Ø1200×600", true, &[(1.2, 1.2, 0.6)]),
];

/// Strip footings: label, width, height, m.
const STRIPS: [(&str, f64, f64); 4] = [
    ("600×500", 0.6, 0.5),
    ("800×600", 0.8, 0.6),
    ("1000×400", 1.0, 0.4),
    ("400×800", 0.4, 0.8),
];

/// Slabs: thickness, mm.
const RAFTS: [&str; 4] = ["250", "300", "400", "600"];

const BRACES: [&str; 5] = [
    "CHS 168.3x8",
    "SHS 200x200x10",
    "RHS 200x100x8",
    "L 100x100x10",
    "L 50x50x5",
];

/// Trusses: "chord / web".
const TRUSSES: [&str; 4] = [
    "SHS 200x200x10 / L 100x100x10",
    "RHS 200x100x8 / L 50x50x5",
    "HEA 200 / L 100x100x10",
    "IPE 200 / CHS 168.3x8",
];

const WEBS: [TrussWeb; 3] = [TrussWeb::Pratt, TrussWeb::Howe, TrussWeb::Warren];
pub const WEB_KEYS: [&str; 3] = ["opt.web.pratt", "opt.web.howe", "opt.web.warren"];

/// Grid structures: "member / edge" catalogue sections.
const GRIDS: [&str; 4] = [
    "CHS 88.9x5 / CHS 168.3x8",
    "CHS 114.3x6 / CHS 219.1x10",
    "SHS 100x100x5 / RHS 200x100x8",
    "CHS 60.3x4 / CHS 114.3x6",
];
pub const GRID_FORM_KEYS: [&str; 4] =
    ["opt.grid_form.dome", "opt.grid_form.vault", "opt.grid_form.hypar", "opt.grid_form.plane"];
const GRID_KINDS: [GridKind; 3] = [GridKind::Diagrid, GridKind::Gridshell, GridKind::CableNet];
pub const GRID_KIND_KEYS: [&str; 3] =
    ["opt.grid_kind.diagrid", "opt.grid_kind.gridshell", "opt.grid_kind.cable_net"];
/// Index 0 takes the kind's own pattern.
const GRID_PATTERNS: [Option<GridPattern>; 5] = [
    None,
    Some(GridPattern::Diagrid),
    Some(GridPattern::Triangular),
    Some(GridPattern::QuadBraced),
    Some(GridPattern::Quad),
];
pub const GRID_PATTERN_KEYS: [&str; 5] = [
    "opt.grid_pattern.auto",
    "opt.grid_pattern.diagrid",
    "opt.grid_pattern.triangular",
    "opt.grid_pattern.quad_braced",
    "opt.grid_pattern.quad",
];

pub fn types(kind: Kind) -> Vec<String> {
    let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
    match kind {
        Kind::Footing => FOOTINGS.iter().map(|f| f.0.to_string()).collect(),
        Kind::StripFooting => STRIPS.iter().map(|f| f.0.to_string()).collect(),
        Kind::Raft => own(&RAFTS),
        Kind::Brace => own(&BRACES),
        Kind::Truss => own(&TRUSSES),
        Kind::Gridshell => own(&GRIDS),
        _ => column::catalog_names(Kind::Beam),
    }
}

pub fn defaults(k: Kind, o: &mut Opts, wall_h: f64) {
    match k {
        Kind::StripFooting => o.pick = true,
        Kind::Raft => (o.width, o.height) = (0.6, 0.3),
        Kind::Brace => (o.z, o.height) = (0.0, wall_h),
        Kind::Truss => (o.z, o.height) = (wall_h, 1.5),
        Kind::BeamSystem => (o.z, o.width, o.depth) = (wall_h, 1.2, 0.0),
        // form (layout), kind (just), pattern (web), rise, divisions u / v, node diameter
        Kind::Gridshell => {
            (o.layout, o.just, o.web) = (0, 0, 0);
            (o.z, o.height, o.panels, o.count, o.width) = (0.0, 5.0, 16, 4, 0.0);
        }
        _ => {}
    }
}

/// The form over the rectangle with diagonal `s`-`e`: a dome on its inscribed circle, a vault
/// along its long side, a saddle or a flat grid over all of it; base at `z`, crown `rise` up.
pub fn grid_form(form: u8, s: V2, e: V2, z: f64, rise: f64) -> GridForm {
    let (lo, hi) = (v2(s.x.min(e.x), s.y.min(e.y)), v2(s.x.max(e.x), s.y.max(e.y)));
    let (w, h) = (hi.x - lo.x, hi.y - lo.y);
    let c = (lo + hi) * 0.5;
    let p = |q: V2| ddc_geom::v3(q.x, q.y, z);
    match form {
        0 => {
            let radius = w.min(h) / 2.0;
            GridForm::Dome { center: p(c), radius, rise: rise.min(radius), oculus: 0.0 }
        }
        1 => {
            let (a, b, span) = if w >= h {
                (v2(lo.x, c.y), v2(hi.x, c.y), h)
            } else {
                (v2(c.x, lo.y), v2(c.x, hi.y), w)
            };
            GridForm::Vault { a: p(a), b: p(b), span, rise: rise.min(span / 2.0) }
        }
        2 => GridForm::Hypar { center: p(c), lx: w, ly: h, rise, angle: 0.0 },
        _ => GridForm::Plane { corner: p(lo), w, h, angle: 0.0 },
    }
}

/// Grid structure over two corners: one record with its member types.
pub fn gridshell(a: &Active, s: V2, e: V2) -> Result<Rec, String> {
    let o = &a.opts;
    let form = grid_form(o.layout, s, e, o.z, o.height);
    let host = form.host().map_err(|e| e.to_string())?;
    let kind = GRID_KINDS[o.just as usize % GRID_KINDS.len()];
    let pattern = GRID_PATTERNS[o.web as usize % GRID_PATTERNS.len()].unwrap_or(match kind {
        GridKind::Diagrid => GridPattern::Diagrid,
        _ => GridPattern::Quad,
    });
    let sp = GridShellSpec {
        level: a.level,
        kind,
        host,
        pattern,
        nu: o.panels.clamp(2, 400),
        nv: o.count.clamp(1, 400),
        supports: form.default_supports(),
        member_type: 0,
        edge_type: None,
        node_d: o.width.max(0.0),
    };
    let name = a.type_name().to_string();
    let (mn, en) = match name.split_once(" / ") {
        Some((m, e)) => (m.to_string(), Some(e.to_string())),
        None => (name.clone(), None),
    };
    Ok(Rec::new("gridshell", move |m| {
        let (mt, n1) = column::ensure_member(m, &mn)?;
        let (et, n2) = match &en {
            Some(en) => {
                let (t, n) = column::ensure_member(m, en)?;
                (Some(t), n)
            }
            None => (None, 0),
        };
        m.add_gridshell(&GridShellSpec {
            member_type: mt,
            edge_type: et,
            ..sp.clone()
        })?;
        Ok(n1 + n2 + 1)
    }))
}

fn steps(a: &Active) -> (bool, Vec<FootingStep>) {
    let f = FOOTINGS.get(a.opts.ty).unwrap_or(&FOOTINGS[0]);
    let s = f.2.iter().map(|s| FootingStep {
        w: s.0,
        l: s.1,
        h: s.2,
    });
    (f.1, s.collect())
}

fn footing_spec(level: i64, a: Option<&Active>) -> FootingSpec {
    let (round, steps) = match a {
        Some(a) => steps(a),
        None => (
            false,
            vec![FootingStep {
                w: 1.2,
                l: 1.2,
                h: 0.5,
            }],
        ),
    };
    FootingSpec {
        level,
        column: None,
        at: (0.0, 0.0),
        rotation: a.map_or(0.0, |a| a.rotation()),
        round,
        steps,
        top_offset: a.map_or(0.0, |a| a.opts.base_off),
        material: CONCRETE.into(),
    }
}

fn ids(ed: &Editor, sql: &str, p: i64) -> Vec<(i64, f64, f64)> {
    let Ok(mut st) = ed.model.sql().prepare(sql) else {
        return vec![];
    };
    st.query_map([p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

fn has(ed: &Editor, table: &str, col: &str, id: i64) -> bool {
    let sql = format!("SELECT count(*) FROM {table} WHERE {col}=?1");
    ed.model
        .sql()
        .query_row(&sql, [id], |r| r.get::<_, i64>(0))
        .is_ok_and(|n| n > 0)
}

/// Level column whose axis is within the snap radius (at least 0.3 m).
fn column_at(ed: &Editor, q: V2) -> Option<i64> {
    let rad = ed.tool.radius.max(0.3);
    ids(ed, "SELECT id, x, y FROM col WHERE level=?1", ed.level)
        .into_iter()
        .map(|(id, x, y)| (id, (v2(x, y) - q).len()))
        .filter(|c| c.1 <= rad)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|c| c.0)
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 < 1e-12 {
        0.0
    } else {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    };
    (a + d * t - p).len()
}

/// Model operations of a batch add: an empty result means no operation happened.
fn done_ops(ids: &[i64]) -> usize {
    usize::from(!ids.is_empty())
}

/// Distance to a wall axis: for an arc — along the arc (not the chord), for a straight wall — to the segment.
fn axis_dist(q: V2, a: V2, b: V2, bulge: f64) -> f64 {
    if bulge.abs() > 1e-9 {
        if let Ok(ax) = ArcAxis::new(a, b, bulge, Tol::default()) {
            let s = ax.param_of(q).clamp(0.0, ax.length());
            let d = (ax.point_at(s) - q).len();
            return d.min((a - q).len()).min((b - q).len());
        }
    }
    seg_dist(q, a, b)
}

/// Level wall under the point (axis within the snap radius; arcs measured along the arc).
fn wall_at(ed: &Editor, q: V2) -> Option<i64> {
    let rad = ed.tool.radius.max(0.3);
    ed.walls
        .iter()
        .map(|w| (w.id, axis_dist(q, w.a, w.b, w.bulge)))
        .filter(|w| w.1 <= rad)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|w| w.0)
}

/// Click: under a column — a column footing (follows it), otherwise a free footing at the point.
pub fn footing(ed: &Editor, a: &Active, q: V2) -> Result<Rec, String> {
    let mut s = footing_spec(a.level, Some(a));
    match column_at(ed, q) {
        Some(c) if has(ed, "iso_footing", "col", c) => Err(a.t("tool.err.has_footing")),
        Some(c) => Ok(Rec::new("footing", move |m| {
            Ok(done_ops(&m.footings_under_columns(&[c], &s)?))
        })),
        None => {
            s.at = tup(q);
            Ok(Rec::new("footing", move |m| {
                m.add_footing(&s)?;
                Ok(1)
            }))
        }
    }
}

fn strip_dims(a: &Active) -> (f64, f64) {
    let s = STRIPS.get(a.opts.ty).unwrap_or(&STRIPS[0]);
    (s.1, s.2)
}

/// Strip footings under walls that have none (one write).
pub fn strip_walls(ed: &Editor, a: &Active, walls: &[i64]) -> Result<Rec, String> {
    let walls: Vec<i64> = walls
        .iter()
        .copied()
        .filter(|w| !has(ed, "strip_footing", "wall", *w))
        .collect();
    if walls.is_empty() {
        return Err(a.t("tool.err.has_footing"));
    }
    let (w, d) = strip_dims(a);
    Ok(Rec::new("strip_footing", move |m| {
        Ok(done_ops(
            &m.strip_footings_under_walls(&walls, w, d, CONCRETE)?,
        ))
    }))
}

/// "By wall" mode: click on a wall.
pub fn strip_pick(ed: &Editor, a: &Active, q: V2) -> Result<Rec, String> {
    match wall_at(ed, q) {
        Some(w) => strip_walls(ed, a, &[w]),
        None => Err(a.t("tool.err.pick")),
    }
}

/// Free strip footing by two points (top measured from the level).
pub fn strip_free(a: &Active, s: V2, e: V2) -> Result<Rec, String> {
    let (width, depth) = strip_dims(a);
    let sp = StripFootingSpec {
        level: a.level,
        wall: None,
        a: tup(s),
        b: tup(e),
        bulge: 0.0,
        width,
        depth,
        ecc: 0.0,
        top_offset: a.opts.base_off,
        material: CONCRETE.into(),
    };
    Ok(Rec::new("strip_footing", move |m| {
        m.add_strip_footings(std::slice::from_ref(&sp))?;
        Ok(1)
    }))
}

pub fn raft(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    let mut ring = pts.to_vec();
    ccw(&mut ring);
    let mm: f64 = a.type_name().parse().unwrap_or(300.0);
    let sp = RaftSpec {
        level: a.level,
        ring: ring.into_iter().map(tup).collect(),
        thickness: mm / 1000.0,
        edge_width: a.opts.width,
        edge_depth: a.opts.height,
        top_offset: a.opts.base_off,
        material: CONCRETE.into(),
    };
    Ok(Rec::new("raft", move |m| {
        m.add_raft(&sp)?;
        Ok(1)
    }))
}

/// Brace: bottom at `opts.z`, top at `opts.height` from the level; ends at columns/beams are anchors.
pub fn brace(ed: &Editor, a: &Active, s: V2, e: V2) -> Result<Rec, String> {
    let rad = ed.tool.radius.max(0.3);
    let (za, zb) = (a.opts.z, a.opts.height);
    let at = |p: V2, z: f64| {
        ed.model
            .brace_anchor(a.level, tup(p), z, rad)
            .ok()
            .flatten()
    };
    let sp = BraceSpec {
        level: a.level,
        member_type: 0,
        a: (s.x, s.y, za),
        b: (e.x, e.y, zb),
        start: at(s, za),
        end: at(e, zb),
        rotation: 0.0,
    };
    let name = a.type_name().to_string();
    Ok(Rec::new("brace", move |m| {
        let (t, n) = column::ensure_member(m, &name)?;
        m.add_brace(&BraceSpec {
            member_type: t,
            ..sp.clone()
        })?;
        Ok(n + 1)
    }))
}

/// Truss: bottom chord s→e at `opts.z`, height `opts.height`, panels, web pattern, chord.
pub fn truss(a: &Active, s: V2, e: V2) -> Result<Rec, String> {
    let o = &a.opts;
    let chord = if o.pitched {
        TrussChord::Pitched
    } else {
        TrussChord::Flat
    };
    let mut panels = o.panels.clamp(2, 200);
    if o.pitched && panels % 2 == 1 {
        panels += 1;
    }
    let sp = TrussSpec {
        level: a.level,
        web: WEBS[o.web as usize % 3],
        chord,
        a: (s.x, s.y, o.z),
        b: (e.x, e.y, o.z),
        height: o.height,
        heel: 0.0,
        panels,
        chord_type: 0,
        web_type: 0,
    };
    let name = a.type_name().to_string();
    let (cn, wn) = match name.split_once(" / ") {
        Some((c, w)) => (c.to_string(), w.to_string()),
        None => (name.clone(), name.clone()),
    };
    Ok(Rec::new("truss", move |m| {
        let (ct, n1) = column::ensure_member(m, &cn)?;
        let (wt, n2) = column::ensure_member(m, &wn)?;
        m.add_truss(&TrussSpec {
            chord_type: ct,
            web_type: wt,
            ..sp.clone()
        })?;
        Ok(n1 + n2 + 1)
    }))
}

/// Layout rule from the options.
fn layout(o: &Opts) -> BeamLayout {
    layout_of(o.layout as usize, o.count)
}

/// Counter-clockwise outline and the index of its edge matching the first picked one (pts[0]–pts[1]).
fn ccw_first(pts: &[V2]) -> (Vec<V2>, usize) {
    let n = pts.len();
    let signed: f64 = (0..n).map(|i| pts[i].cross(pts[(i + 1) % n])).sum();
    let mut ring = pts.to_vec();
    ccw(&mut ring);
    let first = if signed < 0.0 { n.saturating_sub(2) } else { 0 };
    (ring, first)
}

/// Beam system: outline, pitch `opts.width`, direction — rotation, gap `opts.depth`,
/// layout — `opts.layout`; measured from the first picked edge (`opts.first_edge`).
pub fn system(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    let (ring, first) = ccw_first(pts);
    let sp = BeamSystemSpec {
        level: a.level,
        member_type: 0,
        ring: ring.into_iter().map(tup).collect(),
        holes: vec![],
        z: a.opts.z,
        angle: a.rotation(),
        spacing: a.opts.width,
        layout: layout(&a.opts),
        end_gap: a.opts.depth,
        zjust: Just::Pos,
        start_edge: a.opts.first_edge.then_some(first),
    };
    let name = a.type_name().to_string();
    Ok(Rec::new("beam_system", move |m| {
        let (t, n) = column::ensure_member(m, &name)?;
        m.add_beam_system(&BeamSystemSpec {
            member_type: t,
            ..sp.clone()
        })?;
        Ok(n + 1)
    }))
}

fn done(app: &mut EditorApp, r: Result<Rec, String>, what: &str) {
    let cat = &app.sh.cat;
    let msg = match r.map(|r| app.ed.apply_ext(r)) {
        Ok(Ok(())) => cat.tf("tool.done", &[("what", &cat.t(what))]),
        Ok(Err(e)) => format!("{}: {e}", cat.t("tool.err.model")),
        Err(m) => m,
    };
    app.ed.status = Some(msg);
}

/// Columns of `level` with no footing yet and the default footing for them; the ribbon's
/// "footings under all columns" and the assistant both place exactly these.
pub fn footings_plan(ed: &Editor, level: i64) -> (Vec<i64>, FootingSpec) {
    let cols = ids(ed, "SELECT id, x, y FROM col WHERE level=?1", level)
        .into_iter()
        .map(|c| c.0)
        .filter(|c| !has(ed, "iso_footing", "col", *c))
        .collect();
    (cols, footing_spec(level, None))
}

/// "Footings under all level columns": first type, one write.
fn h_footing_all(app: &mut EditorApp) {
    let (cols, s) = footings_plan(&app.ed, app.ed.level);
    let r = if cols.is_empty() {
        Err(app.sh.cat.t("tool.err.no_columns"))
    } else {
        Ok(Rec::new("footing", move |m| {
            Ok(done_ops(&m.footings_under_columns(&cols, &s)?))
        }))
    };
    done(app, r, "tool.footing");
}

/// Strip footing: selected walls — directly under them; the tool stays active (click a wall / two points).
fn h_strip(app: &mut EditorApp) {
    let walls: Vec<i64> = app
        .ed
        .selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(w) => Some(*w),
            _ => None,
        })
        .collect();
    let lang = app.sh.cat.lang;
    arm(&mut app.ed, Kind::StripFooting, lang);
    if walls.is_empty() {
        return;
    }
    crate::interact::sessions::model_tool::with(&mut app.ed, |ed, a| {
        let r = strip_walls(ed, a, &walls);
        commit(ed, a, r);
    });
}

pub fn register(app: &mut EditorApp) {
    app.register_command("str.strip_footing", h_strip);
    app.register_command("str.footing_all", h_footing_all);
}

/// Option bar.
pub fn options(ui: &mut egui::Ui, k: Kind, a: &ddc_ui::i18n::Catalog, o: &mut Opts, u: Units) {
    let len = |ui: &mut egui::Ui, key: &str, v: &mut f64, r: (f64, f64)| {
        opt_field::len(ui, &a.t(key), v, r, u);
    };
    let turn = FULL_TURN;
    let names = |keys: &[&str]| -> Vec<String> { keys.iter().map(|k| a.t(k)).collect() };
    match k {
        Kind::Footing => {
            len(ui, "opt.top_off", &mut o.base_off, (-10.0, 10.0));
            opt_field::angle(ui, &a.t("opt.rotation"), &mut o.rot_deg, turn);
        }
        Kind::StripFooting => {
            len(ui, "opt.top_off", &mut o.base_off, (-10.0, 10.0));
            opt_field::toggle(ui, &a.t("opt.pick_host"), &mut o.pick);
        }
        Kind::Raft => {
            len(ui, "opt.top_off", &mut o.base_off, (-10.0, 10.0));
            len(ui, "opt.edge_width", &mut o.width, (0.0, 5.0));
            len(ui, "opt.edge_depth", &mut o.height, (0.0, 3.0));
        }
        Kind::Brace => {
            len(ui, "opt.z_start", &mut o.z, (-10.0, 50.0));
            len(ui, "opt.z_end", &mut o.height, (-10.0, 50.0));
        }
        Kind::Truss => {
            len(ui, "opt.z", &mut o.z, (-10.0, 50.0));
            len(ui, "opt.height", &mut o.height, (0.2, 10.0));
            opt_field::count(ui, &a.t("opt.panels"), &mut o.panels, (2, 200));
            opt_field::pick_u8(ui, "tool_web", &mut o.web, &names(&WEB_KEYS), true);
            opt_field::toggle(ui, &a.t("opt.pitched"), &mut o.pitched);
        }
        Kind::BeamSystem => {
            len(ui, "opt.z", &mut o.z, (-10.0, 50.0));
            ui.label(a.t("opt.layout"));
            opt_field::pick_u8(ui, "tool_layout", &mut o.layout, &names(&LAYOUT_KEYS), false);
            if matches!(layout(o), BeamLayout::FixedCount(_)) {
                opt_field::count(ui, &a.t("opt.count"), &mut o.count, (1, 1000));
            } else {
                len(ui, "opt.spacing", &mut o.width, (0.2, 10.0));
            }
            opt_field::angle(ui, &a.t("opt.direction"), &mut o.rot_deg, turn);
            len(ui, "opt.end_gap", &mut o.depth, (0.0, 1.0));
            opt_field::toggle(ui, &a.t("opt.first_edge"), &mut o.first_edge);
        }
        Kind::Gridshell => {
            ui.label(a.t("opt.grid_form"));
            opt_field::pick_u8(ui, "tool_grid_form", &mut o.layout, &names(&GRID_FORM_KEYS), false);
            ui.label(a.t("opt.grid_kind"));
            opt_field::pick_u8(ui, "tool_grid_kind", &mut o.just, &names(&GRID_KIND_KEYS), false);
            ui.label(a.t("opt.grid_pattern"));
            opt_field::pick_u8(ui, "tool_grid_pattern", &mut o.web, &names(&GRID_PATTERN_KEYS), false);
            len(ui, "opt.z", &mut o.z, (-10.0, 200.0));
            len(ui, "opt.rise", &mut o.height, (0.0, 100.0));
            opt_field::count(ui, &a.t("opt.divisions_u"), &mut o.panels, (2, 400));
            opt_field::count(ui, &a.t("opt.divisions_v"), &mut o.count, (1, 400));
            len(ui, "opt.node_d", &mut o.width, (0.0, 2.0));
        }
        _ => {}
    }
}

fn quad(p: V2, d: V2, w: f64, l: f64) -> Vec<V2> {
    let n = v2(-d.y, d.x);
    let (hw, hl) = (w / 2.0, l / 2.0);
    vec![
        p - d * hl - n * hw,
        p + d * hl - n * hw,
        p + d * hl + n * hw,
        p - d * hl + n * hw,
    ]
}

fn band(s: V2, e: V2, w: f64) -> Vec<V2> {
    let l = (e - s).len();
    if l < 1e-9 {
        return vec![];
    }
    quad((s + e) * 0.5, (e - s) * (1.0 / l), w, l)
}

/// Plan preview.
pub fn preview(
    p: &egui::Painter,
    a: &Active,
    fill: Color32,
    st: Stroke,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let s = |v: &[V2]| v.iter().map(|q| to_s(*q)).collect::<Vec<Pos2>>();
    let shape = |p: &egui::Painter, v: Vec<V2>| {
        if v.len() >= 3 {
            p.add(egui::Shape::convex_polygon(s(&v), fill, st));
        }
    };
    let cur = a.cur;
    let (sn, co) = a.rotation().sin_cos();
    match a.kind {
        Kind::Footing => {
            let (round, steps) = steps(a);
            for f in steps {
                if round {
                    let r = (to_s(cur + v2(f.w / 2.0, 0.0)) - to_s(cur)).length();
                    p.circle(to_s(cur), r, fill, st);
                } else {
                    shape(p, quad(cur, v2(co, sn), f.l, f.w));
                }
            }
        }
        Kind::StripFooting | Kind::Brace | Kind::Truss => {
            let Some(&p0) = a.pts.first() else { return };
            let w = match a.kind {
                Kind::StripFooting => strip_dims(a).0,
                _ => 0.15,
            };
            shape(p, band(p0, cur, w));
        }
        Kind::Gridshell => {
            let Some(&p0) = a.pts.first() else { return };
            let (lo, hi) = (v2(p0.x.min(cur.x), p0.y.min(cur.y)), v2(p0.x.max(cur.x), p0.y.max(cur.y)));
            let rect = vec![lo, v2(hi.x, lo.y), hi, v2(lo.x, hi.y)];
            p.add(egui::Shape::closed_line(s(&rect), st));
            // the plan outline of the form inside it
            match grid_form(a.opts.layout, p0, cur, 0.0, a.opts.height.max(0.01)) {
                GridForm::Dome { center, radius, .. } => {
                    let r = (to_s(v2(center.x + radius, center.y)) - to_s(v2(center.x, center.y))).length();
                    p.circle_stroke(to_s(v2(center.x, center.y)), r, st);
                }
                GridForm::Vault { a: va, b: vb, .. } => {
                    p.line_segment([to_s(v2(va.x, va.y)), to_s(v2(vb.x, vb.y))], Stroke::new(1.0, st.color));
                }
                _ => {
                    p.line_segment([to_s(lo), to_s(hi)], Stroke::new(1.0, st.color));
                    p.line_segment([to_s(v2(hi.x, lo.y)), to_s(v2(lo.x, hi.y))], Stroke::new(1.0, st.color));
                }
            }
        }
        Kind::Raft | Kind::BeamSystem => {
            if a.pts.is_empty() {
                return;
            }
            let mut v = a.pts.clone();
            v.push(cur);
            p.add(egui::Shape::closed_line(s(&v), st));
            if a.kind == Kind::BeamSystem && v.len() >= 3 {
                let dash = Stroke::new(1.0, st.color);
                for (q0, q1) in ddc_geom::framing::beam_lines_in(
                    &v,
                    &[],
                    a.rotation(),
                    a.opts.width.max(0.2),
                    layout(&a.opts),
                    a.opts.first_edge.then_some(0),
                ) {
                    p.line_segment([to_s(q0), to_s(q1)], dash);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::{arm, Kind};
    use crate::editor::Editor;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::arc::ArcAxis;
    use ddc_geom::{v2, Tol};
    use ddc_ui::i18n::Lang;

    fn click(e: &mut Editor, x: f64, y: f64) {
        e.handle(Event::Move(v2(x, y)));
        e.handle(Event::Click(v2(x, y)));
    }

    fn count(e: &Editor, t: &str) -> i64 {
        let q = format!("SELECT count(*) FROM {t}");
        e.model.sql().query_row(&q, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn footing_click_and_one_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Footing, Lang::En);
        click(&mut e, 3.0, 4.0);
        assert_eq!(count(&e, "iso_footing"), 1);
        let q = e.model.struct_quantities("footing", 1).unwrap();
        assert!((q.volume - 1.2 * 1.2 * 0.5).abs() < 1e-9);
        e.undo();
        assert_eq!(count(&e, "iso_footing"), 0);
    }

    #[test]
    fn truss_two_clicks_one_undo_with_types() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Truss, Lang::En);
        click(&mut e, 0.0, 0.0);
        click(&mut e, 12.0, 0.0);
        assert_eq!(count(&e, "truss"), 1);
        assert!(e.model.struct_parts("truss", 1).unwrap().len() > 10);
        e.undo();
        assert_eq!(count(&e, "truss"), 0);
        assert_eq!(
            count(&e, "member_type"),
            0,
            "types created by the action are undone too"
        );
    }

    /// Two corners make a diagrid dome on the inscribed circle, one undo removes it with
    /// its member types; a vault runs along the long side.
    #[test]
    fn gridshell_two_corners_one_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Gridshell, Lang::En);
        click(&mut e, 0.0, 0.0);
        click(&mut e, 24.0, 20.0);
        assert_eq!(count(&e, "gridshell"), 1, "{:?}", e.status);
        let id = e.model.gridshells(None).unwrap()[0];
        let s = e.model.gridshell(id).unwrap();
        assert_eq!((s.kind, s.pattern, s.nu, s.nv), (ddc_model::GridKind::Diagrid, ddc_model::GridPattern::Diagrid, 16, 4));
        assert_eq!(s.host.kind(), "sphere");
        assert!(s.edge_type.is_some());
        let far = e.model.gridshell_nodes(id).unwrap().iter().map(|n| (n.p.x - 12.0).hypot(n.p.y - 10.0)).fold(0.0, f64::max);
        assert!((far - 10.0).abs() < 1e-6, "base circle radius {far}");
        assert_eq!(count(&e, "member_type"), 2);
        e.undo();
        assert_eq!(count(&e, "gridshell"), 0);
        assert_eq!(count(&e, "member_type"), 0);
        let f = super::grid_form(1, v2(0.0, 0.0), v2(30.0, 12.0), 0.0, 4.0);
        let ddc_model::GridForm::Vault { a, b, span, rise } = f else { panic!("{f:?}") };
        assert_eq!((a.y, b.y, a.x, b.x, span, rise), (6.0, 6.0, 0.0, 30.0, 12.0, 4.0));
    }

    #[test]
    fn beam_system_loop() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::BeamSystem, Lang::En);
        for (x, y) in [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0), (0.0, 0.0)] {
            click(&mut e, x, y);
        }
        assert_eq!(count(&e, "beam_system"), 1);
        assert!(!e.model.struct_parts("beam_system", 1).unwrap().is_empty());
    }

    #[test]
    fn beam_system_layout_and_first_edge_options() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::BeamSystem, Lang::En);
        {
            let o = &mut e.model_tool_mut().unwrap().opts;
            (o.layout, o.count, o.first_edge) = (3, 4, true);
        }
        for (x, y) in [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0), (0.0, 0.0)] {
            click(&mut e, x, y);
        }
        let s = e.model.beam_system(1).unwrap();
        assert_eq!(s.layout, ddc_model::BeamLayout::FixedCount(4));
        assert!(s.holes.is_empty());
        let i = s.start_edge.expect("counted from the first picked edge");
        let (p, q) = (s.ring[i], s.ring[(i + 1) % s.ring.len()]);
        let first = [(0.0, 0.0), (6.0, 0.0)];
        assert!(first.contains(&p) && first.contains(&q), "{p:?} {q:?}");
    }

    #[test]
    fn first_edge_survives_ccw() {
        let cw = [v2(0.0, 0.0), v2(0.0, 4.0), v2(6.0, 4.0), v2(6.0, 0.0)];
        let (r, i) = super::ccw_first(&cw);
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        assert!((p, q) == (cw[1], cw[0]) || (p, q) == (cw[0], cw[1]));
        let (_, j) = super::ccw_first(&[cw[3], cw[2], cw[1], cw[0]]);
        assert_eq!(j, 0);
    }

    #[test]
    fn rec_counts_zero_ops_when_nothing_added() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Column, Lang::En);
        click(&mut e, 2.0, 3.0);
        e.set_mode(Mode::Select);
        assert_eq!(count(&e, "col"), 1);
        arm(&mut e, Kind::Footing, Lang::En);
        let a = crate::interact::sessions::model_tool::take(&mut e).unwrap();
        let r = super::footing(&e, &a, v2(2.0, 3.0)).unwrap();
        assert_eq!(r.apply(&mut e.model).unwrap().n, 1);
        assert_eq!(
            r.apply(&mut e.model).unwrap().n,
            0,
            "replay adds nothing, no op"
        );
        assert_eq!(count(&e, "iso_footing"), 1);

        crate::modify_tools::tests::walls(&mut e, &[((0.0, 0.0), (4.0, 0.0))]);
        let w = e.walls[0].id;
        arm(&mut e, Kind::StripFooting, Lang::En);
        let a = crate::interact::sessions::model_tool::take(&mut e).unwrap();
        let r = super::strip_walls(&e, &a, &[w]).unwrap();
        assert_eq!(r.apply(&mut e.model).unwrap().n, 1);
        assert_eq!(r.apply(&mut e.model).unwrap().n, 0);
        assert_eq!(count(&e, "strip_footing"), 1);
    }

    #[test]
    fn picked_arc_or_spline_wall_gets_a_curved_strip() {
        let mut e = Editor::new_in_memory().unwrap();
        let s = ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: e.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: e.types[1].id,
            offset0: None,
        };
        e.model.add_arc_wall(&s, 1.0).unwrap();
        let sw = ddc_model::SplineWallSpec::through(
            e.level,
            e.types[1].id,
            &[(0.0, 6.0), (3.0, 8.0), (7.0, 5.0), (10.0, 7.0)],
            3.0,
        )
        .unwrap();
        let sid = e.model.add_spline_wall(&sw).unwrap();
        let pieces = e.model.spline_wall_pieces(sid).unwrap();
        e.reload().unwrap();
        arm(&mut e, Kind::StripFooting, Lang::En);
        let a = crate::interact::sessions::model_tool::take(&mut e).unwrap();
        // the arc, picked at its middle (away from the chord)
        let ax = ArcAxis::new(v2(0.0, 0.0), v2(4.0, 0.0), 1.0, Tol::default()).unwrap();
        let r = super::strip_pick(&e, &a, ax.point_at(ax.length() / 2.0)).unwrap();
        assert_eq!(r.apply(&mut e.model).unwrap().n, 1);
        let bulge: f64 = e
            .model
            .sql()
            .query_row("SELECT bulge FROM strip_footing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bulge, 1.0, "arc band, not the chord");
        // one piece of the spline stands for the whole curve, in one undo step
        let r = super::strip_walls(&e, &a, &pieces[..1]).unwrap();
        // `n` is the journal step count (one), the bands are counted below
        assert_eq!(r.apply(&mut e.model).unwrap().n, 1);
        assert_eq!(count(&e, "strip_footing"), 1 + pieces.len() as i64);
        e.model.undo().unwrap();
        assert_eq!(count(&e, "strip_footing"), 1);
    }

    #[test]
    fn wall_at_hits_arc_away_from_chord() {
        let mut e = Editor::new_in_memory().unwrap();
        let s = ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: e.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: e.types[1].id,
            offset0: None,
        };
        let id = e.model.add_arc_wall(&s, 1.0).unwrap();
        e.reload().unwrap();
        let ax = ArcAxis::new(v2(0.0, 0.0), v2(4.0, 0.0), 1.0, Tol::default()).unwrap();
        let q = ax.point_at(ax.length() / 2.0);
        assert!(
            super::seg_dist(q, v2(0.0, 0.0), v2(4.0, 0.0)) > 1.0,
            "far from the chord"
        );
        assert_eq!(super::wall_at(&e, q), Some(id));
        assert_eq!(
            super::wall_at(&e, v2(2.0, 0.0)),
            None,
            "chord midpoint is off the arc"
        );
    }
}
