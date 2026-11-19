//! Schedule categories from model tables (columns, beams, roofs, ceilings, stairs, railings,
//! MEP networks, components) of the current level; edits go through properties and project parameters.

use super::{col, tl, Cat, Col, Edit as E, Kind as K, Row, Val};
use crate::editor::Editor;
use rusqlite::types::Value;

/// (columns, SQL with `?1` = level; the first selected column is the id).
fn spec(cat: Cat, ru: bool) -> (Vec<Col>, &'static str) {
    match cat {
        Cat::Columns => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("type", tl(ru, "sched.col.type"), K::Text, E::No),
                col("structural", tl(ru, "sched.col.structural"), K::Text, E::No),
                col("len", tl(ru, "sched.col.length"), K::Len, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
                col("surf", tl(ru, "sched.col.surface"), K::Area, E::No),
            ],
            "SELECT c.id, t.name, CASE WHEN c.structural THEN 'yes' ELSE 'no' END, \
             q.length, q.volume, q.surface FROM col c JOIN member_type t ON t.id=c.mtype \
             LEFT JOIN member_qty q ON q.kind='col' AND q.ref=c.id WHERE c.level=?1 ORDER BY c.id",
        ),
        Cat::Beams => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("type", tl(ru, "sched.col.type"), K::Text, E::No),
                col("len", tl(ru, "sched.col.length"), K::Len, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
                col("surf", tl(ru, "sched.col.surface"), K::Area, E::No),
                col("cut", tl(ru, "sched.col.deduction"), K::Vol, E::No),
            ],
            "SELECT b.id, t.name, q.length, q.volume, q.surface, q.cut FROM beam b \
             JOIN member_type t ON t.id=b.mtype LEFT JOIN member_qty q ON q.kind='beam' AND q.ref=b.id \
             WHERE b.level=?1 ORDER BY b.id",
        ),
        Cat::Roofs => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("type", tl(ru, "sched.col.type"), K::Text, E::No),
                col("kind", tl(ru, "sched.col.shape"), K::Text, E::No),
                col("area", tl(ru, "sched.col.area"), K::Area, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
            ],
            "SELECT r.id, t.name, CASE r.kind WHEN 'extrusion' THEN 'extrusion' ELSE 'footprint' END, \
             (SELECT MAX(area) FROM roof_qty WHERE roof=r.id), \
             (SELECT SUM(volume) FROM roof_qty WHERE roof=r.id) \
             FROM roof r JOIN slab_type t ON t.id=r.stype WHERE r.level=?1 ORDER BY r.id",
        ),
        Cat::Ceilings => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("type", tl(ru, "sched.col.type"), K::Text, E::No),
                col("h", tl(ru, "sched.col.height"), K::Len, E::No),
                col("area", tl(ru, "sched.col.area"), K::Area, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
            ],
            "SELECT c.id, t.name, c.height, (SELECT MAX(area) FROM ceiling_qty WHERE ceiling=c.id), \
             (SELECT SUM(volume) FROM ceiling_qty WHERE ceiling=c.id) \
             FROM ceiling c JOIN slab_type t ON t.id=c.stype WHERE c.level=?1 ORDER BY c.id",
        ),
        Cat::Stairs => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("kind", tl(ru, "sched.col.shape"), K::Text, E::No),
                col("w", tl(ru, "sched.col.flight_width"), K::Len, E::No),
                col("risers", tl(ru, "sched.col.risers"), K::Int, E::No),
                col("treads", tl(ru, "sched.col.treads"), K::Int, E::No),
                col("riser", tl(ru, "sched.col.riser_height"), K::Len, E::No),
                col("tread", tl(ru, "sched.col.tread_depth"), K::Len, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
            ],
            "SELECT s.id, CASE s.kind WHEN 'l' THEN 'stair_l' WHEN 'u' THEN 'stair_u' \
             ELSE 'stair_straight' END, s.width, q.risers, q.treads, q.riser, q.tread, \
             q.volume + q.landing_volume + q.stringer_volume \
             FROM stair s LEFT JOIN stair_qty q ON q.stair=s.id WHERE s.level=?1 ORDER BY s.id",
        ),
        Cat::Railings => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("h", tl(ru, "sched.col.height"), K::Len, E::No),
                col("len", tl(ru, "sched.col.length"), K::Len, E::No),
                col("posts", tl(ru, "sched.col.posts"), K::Int, E::No),
                col("balusters", tl(ru, "sched.col.balusters"), K::Int, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
            ],
            "SELECT r.id, r.height, q.length, q.posts, q.balusters, q.volume \
             FROM railing r LEFT JOIN railing_qty q ON q.railing=r.id WHERE r.level=?1 ORDER BY r.id",
        ),
        Cat::MepRuns => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("system", tl(ru, "sched.col.system"), K::Text, E::No),
                col("domain", tl(ru, "sched.col.kind"), K::Text, E::No),
                col("shape", tl(ru, "sched.col.section"), K::Text, E::No),
                col("w", tl(ru, "sched.col.width_dia"), K::Len, E::No),
                col("h", tl(ru, "sched.col.height"), K::Len, E::No),
                col("len", tl(ru, "sched.col.length"), K::Len, E::No),
            ],
            "SELECT r.id, s.name, r.domain, r.shape, r.w, r.h, r.points \
             FROM mep_run r JOIN mep_system s ON s.id=r.system WHERE r.level=?1 ORDER BY r.id",
        ),
        Cat::Ramps => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("w", tl(ru, "sched.col.width"), K::Len, E::No),
                col("slope", tl(ru, "sched.col.slope"), K::Pct, E::No),
                col("th", tl(ru, "sched.col.thickness"), K::Len, E::No),
            ],
            "SELECT r.id, r.width, r.slope*100.0, r.thickness FROM ramp r WHERE r.level=?1 ORDER BY r.id",
        ),
        Cat::Meshes => (
            vec![
                col("id", "№", K::Int, E::No),
                col("category", tl(ru, "sched_cats.sched.category"), K::Text, E::No),
                col("name", tl(ru, "sched_cats.sched.name"), K::Text, E::No),
            ],
            "SELECT m.id, m.category, m.name FROM mesh m WHERE m.level=?1 ORDER BY m.id",
        ),
        Cat::Shafts => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("base", tl(ru, "sched.col.base_offset"), K::Len, E::No),
                col("top", tl(ru, "sched.col.top_offset"), K::Len, E::No),
                col("cuts", tl(ru, "sched.col.shaft_cuts"), K::Int, E::No),
                col("vol", tl(ru, "sched.col.cut_volume"), K::Vol, E::No),
            ],
            "SELECT s.id, s.base_offset, s.top_offset,              (SELECT count(*) FROM shaft_cut c WHERE c.shaft=s.id),              (SELECT total(c.volume) FROM shaft_cut c WHERE c.shaft=s.id)              FROM shaft s WHERE s.level=?1 ORDER BY s.id",
        ),
        Cat::Zones => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("name", tl(ru, "sched.col.name"), K::Text, E::No),
                col("rooms", tl(ru, "sched.col.rooms"), K::Int, E::No),
                col("area", tl(ru, "sched.col.area"), K::Area, E::No),
            ],
            "SELECT z.id, z.name, (SELECT count(*) FROM zone_room zr WHERE zr.zone=z.id),              (SELECT total(q.area) FROM zone_room zr JOIN room_qty q ON q.room=zr.room WHERE zr.zone=z.id)              FROM zone z WHERE EXISTS (SELECT 1 FROM zone_room zr JOIN room r ON r.id=zr.room              WHERE zr.zone=z.id AND r.level=?1) ORDER BY z.id",
        ),
        Cat::RoomSeps => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("len", tl(ru, "sched.col.length"), K::Len, E::No),
            ],
            // the two ends as a point list: the Len column measures its length
            "SELECT s.id, s.ax||' '||s.ay||';'||s.bx||' '||s.by FROM room_sep s WHERE s.level=?1 ORDER BY s.id",
        ),
        Cat::Components => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("family", tl(ru, "sched.col.family"), K::Text, E::No),
                col("type", tl(ru, "sched.col.type"), K::Text, E::No),
                col("category", tl(ru, "sched.col.category"), K::Text, E::No),
                col(IFC_COL, tl(ru, "sched.col.ifc_class"), K::Text, E::No),
                col("vol", tl(ru, "sched.col.volume"), K::Vol, E::No),
                col("surf", tl(ru, "sched.col.surface"), K::Area, E::No),
            ],
            // the family definition stands in for the IFC column until `ifc_classes` reads it
            "SELECT c.id, f.name, t.name, f.category, f.def, q.volume, q.surface FROM comp c \
             JOIN comp_type t ON t.id=c.ctype JOIN comp_family f ON f.id=t.family \
             LEFT JOIN comp_qty q ON q.comp=c.id WHERE c.level=?1 ORDER BY c.id",
        ),
        Cat::Footings => structural(ru, SQL_FOOTING),
        Cat::StripFootings => structural(ru, SQL_STRIP),
        Cat::Rafts => structural(ru, SQL_RAFT),
        Cat::Braces => structural(ru, SQL_BRACE),
        Cat::Trusses => structural(ru, SQL_TRUSS),
        Cat::BeamSystems => structural(ru, SQL_SYSTEM),
        Cat::Solids => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("name", tl(ru, "sched_cats.col.name"), K::Text, E::No),
                col("material", tl(ru, "sched_cats.col.material"), K::Text, E::No),
                col("void", tl(ru, "sched_cats.col.void"), K::Text, E::No),
                col("vol", tl(ru, "sched_cats.col.volume"), K::Vol, E::No),
                col("area", tl(ru, "sched_cats.col.area"), K::Area, E::No),
            ],
            "SELECT s.id, s.name, s.material,              CASE s.void WHEN 1 THEN 'x' ELSE '' END, q.net_volume, q.area              FROM solid s LEFT JOIN solid_qty q ON q.solid=s.id WHERE s.level=?1 ORDER BY s.id",
        ),
        Cat::Masses => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("name", tl(ru, "sched_cats.col.name"), K::Text, E::No),
                col("void", tl(ru, "sched_cats.col.void"), K::Text, E::No),
                col("inplace", tl(ru, "sched_cats.col.inplace"), K::Text, E::No),
                col("area", tl(ru, "sched_cats.col.floor_area"), K::Area, E::No),
            ],
            "SELECT m.id, m.name, CASE m.void WHEN 1 THEN 'x' ELSE '' END,              CASE m.inplace WHEN 1 THEN 'x' ELSE '' END,              (SELECT SUM(area) FROM mass_qty WHERE mass=m.id)              FROM mass m WHERE m.level=?1 ORDER BY m.id",
        ),
        Cat::Topography => (
            vec![
                col("id", tl(ru, "sched.col.id"), K::Int, E::No),
                col("name", tl(ru, "sched_cats.col.name"), K::Text, E::No),
                col("material", tl(ru, "sched_cats.col.material"), K::Text, E::No),
                col("th", tl(ru, "sched_cats.col.thickness"), K::Len, E::No),
                col("points", tl(ru, "sched_cats.col.points"), K::Int, E::No),
            ],
            "SELECT t.id, t.name, t.material, t.thickness,              (SELECT COUNT(*) FROM topo_point WHERE topo=t.id)              FROM toposolid t WHERE ?1 IS NOT NULL ORDER BY t.id",
        ),
        _ => (vec![], ""),
    }
}

/// Quantities of the structural families come from `struct_qty` (parts summed per element).
macro_rules! struct_sql {
    ($table:literal, $kind:literal) => {
        concat!(
            "SELECT t.id, (SELECT group_concat(DISTINCT material) FROM struct_qty WHERE kind='",
            $kind,
            "' AND ref=t.id), (SELECT count(*) FROM struct_qty WHERE kind='",
            $kind,
            "' AND ref=t.id), (SELECT total(length) FROM struct_qty WHERE kind='",
            $kind,
            "' AND ref=t.id), (SELECT total(area) FROM struct_qty WHERE kind='",
            $kind,
            "' AND ref=t.id), (SELECT total(volume) FROM struct_qty WHERE kind='",
            $kind,
            "' AND ref=t.id) FROM ",
            $table,
            " t WHERE t.level=?1 ORDER BY t.id"
        )
    };
}
const SQL_FOOTING: &str = struct_sql!("iso_footing", "footing");
const SQL_STRIP: &str = struct_sql!("strip_footing", "strip_footing");
const SQL_RAFT: &str = struct_sql!("raft", "raft");
const SQL_BRACE: &str = struct_sql!("brace", "brace");
const SQL_TRUSS: &str = struct_sql!("truss", "truss");
const SQL_SYSTEM: &str = struct_sql!("beam_system", "beam_system");

fn structural(ru: bool, sql: &'static str) -> (Vec<Col>, &'static str) {
    let t = |k: &str| tl(ru, k);
    (
        vec![
            col("id", t("sched.col.id"), K::Int, E::No),
            col("material", t("sched.col.material"), K::Text, E::No),
            col("parts", t("sched.col.parts"), K::Int, E::No),
            col("len", t("sched.col.length"), K::Len, E::No),
            col("area", t("sched.col.area"), K::Area, E::No),
            col("vol", t("sched.col.volume"), K::Vol, E::No),
        ],
        sql,
    )
}

/// Length of a polyline `x y z;x y z;...` (points of a network segment).
pub fn points_len(s: &str) -> f64 {
    let pts: Vec<[f64; 3]> = s
        .split(';')
        .filter_map(|p| {
            let v: Vec<f64> = p
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            (v.len() >= 2).then(|| [v[0], v[1], v.get(2).copied().unwrap_or(0.0)])
        })
        .collect();
    pts.windows(2)
        .map(|w| {
            let d = [w[1][0] - w[0][0], w[1][1] - w[0][1], w[1][2] - w[0][2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .sum()
}

/// Columns whose SQL value is a stable code, shown through the catalog (`sch.val.<code>`).
const CODED: [&str; 4] = ["structural", "kind", "domain", "shape"];

/// Key of the components' IFC class column.
pub const IFC_COL: &str = "ifc";

/// `Class.PREDEFINED` of a family definition (`Class` when not defined); empty when the
/// definition does not parse.
fn ifc_label(def: &str) -> String {
    match ddc_model::Family::parse(def) {
        Ok(f) if f.ifc.1.is_empty() || f.ifc.1 == "NOTDEFINED" => f.ifc.0,
        Ok(f) => format!("{}.{}", f.ifc.0, f.ifc.1),
        Err(_) => String::new(),
    }
}

/// Replaces the family definitions in the IFC column by their class, one parse per family.
fn ifc_classes(cols: &[Col], rows: &mut [Row]) {
    let Some(k) = cols.iter().position(|c| c.key == IFC_COL) else {
        return;
    };
    let mut seen: std::collections::HashMap<String, String> = Default::default();
    for r in rows {
        if let Some(Val::T(def)) = r.vals.get_mut(k) {
            let label = match seen.get(def.as_str()) {
                Some(l) => l.clone(),
                None => {
                    let l = ifc_label(def);
                    seen.insert(std::mem::take(def), l.clone());
                    l
                }
            };
            *def = label;
        }
    }
}

fn code_text(code: &str, ru: bool) -> String {
    let key = format!("sch.val.{code}");
    let t = tl(ru, &key);
    if t.is_empty() || t == key {
        code.to_string()
    } else {
        t
    }
}

fn val(v: Value, k: K) -> Val {
    match (v, k) {
        (Value::Integer(i), _) => Val::N(i as f64),
        (Value::Real(x), _) => Val::N(x),
        (Value::Text(s), K::Len) => Val::N(points_len(&s)),
        (Value::Text(s), _) => Val::T(s),
        (_, K::Text) => Val::T("—".into()),
        _ => Val::N(0.0),
    }
}

pub fn count(ed: &Editor, cat: Cat) -> usize {
    if matches!(cat, Cat::Grids | Cat::RefPlanes) {
        return datums(ed, cat, false).1.len();
    }
    if cat == Cat::Rebar {
        return ed
            .model
            .rebar_set_schedule(Some(ed.level))
            .map_or(0, |r| r.len());
    }
    if cat == Cat::Zones {
        return build(ed, cat, false).1.len();
    }
    let Some(t) = cat.table() else { return 0 };
    if cat.basic() {
        return 0;
    }
    if cat == Cat::Topography {
        return ed
            .model
            .sql()
            .query_row("SELECT COUNT(*) FROM toposolid", [], |r| r.get::<_, i64>(0))
            .map_or(0, |c| c as usize);
    }
    ed.model
        .sql()
        .query_row(
            &format!("SELECT COUNT(*) FROM {t} WHERE level=?1"),
            [ed.level],
            |r| r.get::<_, i64>(0),
        )
        .map_or(0, |c| c as usize)
}

/// Plan and 3D selection of the row of a family that has one.
fn row_sel(cat: Cat, id: i64) -> Option<crate::editor::Sel> {
    let kind = match cat {
        Cat::Stairs => "stair",
        Cat::Railings => "railing",
        _ => return None,
    };
    crate::editor::structx::sel(kind, id)
}

/// Grids and reference planes of the model: name, length and direction of the line.
fn datums(ed: &Editor, cat: Cat, ru: bool) -> (Vec<Col>, Vec<Row>) {
    let t = |k: &str| tl(ru, k);
    let grids = cat == Cat::Grids;
    let mut cols = vec![col("id", t("sched.col.id"), K::Int, E::No)];
    cols.push(col("name", t("sched.col.name"), K::Text, E::No));
    cols.push(col("len", t("sched.col.length"), K::Len, E::No));
    cols.push(col("angle", t("sched.col.angle"), K::Num, E::No));
    let lines: Vec<(i64, String, (f64, f64), (f64, f64))> = if grids {
        ed.model
            .grids()
            .unwrap_or_default()
            .into_iter()
            .map(|g| (g.id, g.name, g.spec.a, g.spec.b))
            .collect()
    } else {
        ed.model
            .sql()
            .prepare("SELECT id,name,ax,ay,bx,by FROM ref_plane ORDER BY id")
            .and_then(|mut st| {
                st.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, (r.get(2)?, r.get(3)?), (r.get(4)?, r.get(5)?)))
                })?
                .collect()
            })
            .unwrap_or_default()
    };
    let kind = if grids { "grid" } else { "ref_plane" };
    let rows = lines
        .into_iter()
        .map(|(id, name, a, b)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            Row {
                sel: crate::editor::structx::sel(kind, id),
                id,
                sub: 0,
                vals: vec![
                    Val::N(id as f64),
                    Val::T(name),
                    Val::N(dx.hypot(dy)),
                    Val::N(dy.atan2(dx).to_degrees()),
                ],
            }
        })
        .collect();
    (cols, rows)
}

/// Rebar of the level, one row per set, sorted by host and set; the numbers add up in the
/// group subtotals and the totals. The bending schedule by mark stays in the rebar dialog.
fn rebar(ed: &Editor, ru: bool) -> (Vec<Col>, Vec<Row>) {
    use crate::editor::Sel;
    use crate::scene_ids::EXT_REBAR;
    let t = |k: &str| tl(ru, k);
    let cols = vec![
        col("id", t("sched_cats.sched.set"), K::Int, E::No),
        col("host", t("elem.rebar.host"), K::Text, E::No),
        col("mark", t("rebar.col.mark"), K::Text, E::No),
        col("layout", t("elem.rebar.kind"), K::Text, E::No),
        col("d", t("rebar.col.d"), K::Num, E::No),
        col("grade", t("rebar.col.grade"), K::Text, E::No),
        col("count", t("rebar.col.count"), K::Int, E::No),
        col("total", t("rebar.col.total"), K::Len, E::No),
        col("kg", t("rebar.col.weight"), K::Mass, E::No),
    ];
    let rows = ed
        .model
        .rebar_set_schedule(Some(ed.level))
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let layout = ed
                .model
                .rebar_set(r.set)
                .ok()
                .flatten()
                .map_or_else(String::new, |s| {
                    t(&format!("elem.rebar.kind.{}", s.layout.name()))
                });
            Row {
                sel: Some(Sel::Ext(EXT_REBAR, r.set)),
                id: r.set,
                sub: 0,
                vals: vec![
                    Val::N(r.set as f64),
                    Val::T(format!(
                        "{} #{}",
                        t(&format!("rebar.host.{}", r.host_kind)),
                        r.host
                    )),
                    Val::T(r.marks.join(", ")),
                    Val::T(layout),
                    Val::N((r.d * 1000.0).round()),
                    Val::T(r.grade),
                    Val::N(r.count as f64),
                    Val::N(r.total_length),
                    Val::N(r.weight),
                ],
            }
        })
        .collect();
    (cols, rows)
}

pub fn build(ed: &Editor, cat: Cat, ru: bool) -> (Vec<Col>, Vec<Row>) {
    if cat == Cat::Rebar {
        return rebar(ed, ru);
    }
    if matches!(cat, Cat::Grids | Cat::RefPlanes) {
        return datums(ed, cat, ru);
    }
    let (cols, sql) = spec(cat, ru);
    if sql.is_empty() {
        return (cols, vec![]);
    }
    let kinds: Vec<K> = cols.iter().map(|c| c.kind).collect();
    let coded: Vec<bool> = cols.iter().map(|c| CODED.contains(&c.key.as_str())).collect();
    let n = cols.len();
    let got = ed.model.sql().prepare(sql).and_then(|mut st| {
        st.query_map([ed.level], |r| {
            (0..n)
                .map(|i| r.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })?
        .collect::<Result<Vec<_>, _>>()
    });
    let rows = got
        .unwrap_or_default()
        .into_iter()
        .map(|vs| {
            let id = match vs.first() {
                Some(Value::Integer(i)) => *i,
                _ => 0,
            };
            Row {
                sel: row_sel(cat, id),
                id,
                sub: 0,
                vals: vs
                    .into_iter()
                    .zip(kinds.iter().zip(&coded))
                    .map(|(v, (k, coded))| match val(v, *k) {
                        Val::T(s) if *coded => Val::T(code_text(&s, ru)),
                        v => v,
                    })
                    .collect(),
            }
        })
        .collect::<Vec<Row>>();
    let mut rows = rows;
    if cat == Cat::Components {
        ifc_classes(&cols, &mut rows);
    }
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mep_length_from_points() {
        assert!((points_len("0 0 0;3 4 0;3 4 2") - 7.0).abs() < 1e-12);
        assert_eq!(points_len(""), 0.0);
    }

    #[test]
    fn component_rows_carry_ifc_class_and_classification_into_xlsx() {
        use calamine::{open_workbook, Data, Reader, Xlsx};
        let mut e = Editor::new_in_memory().unwrap();
        let fam = e.model.load_library_family("Table").unwrap();
        let ct = e.model.comp_types(fam).unwrap()[0].0;
        let wf = e.model.load_library_family("Wardrobe").unwrap();
        let wt = e.model.comp_types(wf).unwrap()[0].0;
        let lv = e.level;
        let id = e
            .model
            .add_component(&ddc_model::CompSpec::at(lv, ct, 0.0, 0.0))
            .unwrap();
        e.model
            .add_component(&ddc_model::CompSpec::at(lv, wt, 3.0, 0.0))
            .unwrap();
        let pid = e
            .model
            .add_param(&ddc_model::ParamSpec {
                name: format!("{}Uniclass", ddc_model::CLASSIFICATION_PREFIX),
                ptype: ddc_model::ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["comp".into()],
            })
            .unwrap();
        e.model
            .set_param_values(&[ddc_model::ParamSet {
                param: pid,
                kind: "comp".into(),
                id,
                value: Some(ddc_model::PVal::Text("Pr_40_50_12".into())),
            }])
            .unwrap();
        e.reload().unwrap();
        let tb = super::super::build_in(&e, Cat::Components, false);
        let ifc = tb.cols.iter().position(|c| c.key == IFC_COL).unwrap();
        assert_eq!(tb.cols[ifc].title, "IFC class");
        let cls = tb
            .cols
            .iter()
            .position(|c| c.key == format!("p:{pid}"))
            .unwrap();
        let row = |id: i64| tb.rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(row(id).vals[ifc], Val::T("IfcFurniture.TABLE".into()));
        assert_eq!(row(id).vals[cls], Val::T("Pr_40_50_12".into()));
        let other = tb.rows.iter().find(|r| r.id != id).unwrap();
        // NOTDEFINED is left out of the label
        assert_eq!(other.vals[ifc], Val::T("IfcFurniture".into()));

        let p = std::env::temp_dir().join(format!("ddc_comp_ifc_{}.xlsx", std::process::id()));
        let ord = super::super::order(&tb, &[], None, crate::units::Units::Metric);
        let cols: Vec<usize> = (0..tb.cols.len()).collect();
        super::super::export_xlsx(&tb, &ord, &cols, crate::units::Units::Metric, &p).unwrap();
        let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
        let name = wb.sheet_names()[0].clone();
        let r = wb.worksheet_range(&name).unwrap();
        let cells: Vec<String> = r
            .cells()
            .filter_map(|(_, _, d)| match d {
                Data::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let _ = std::fs::remove_file(&p);
        for want in ["IFC class", "Classification.Uniclass", "IfcFurniture.TABLE", "Pr_40_50_12"] {
            assert!(cells.iter().any(|c| c.contains(want)), "{want} not in {cells:?}");
        }
    }

    #[test]
    fn column_category_rows() {
        let mut e = Editor::new_in_memory().unwrap();
        let mt = e
            .model
            .add_member_type(
                "C1",
                &ddc_model::MemberShape::Rect { b: 0.4, h: 0.4 },
                "Concrete",
            )
            .unwrap();
        let spec = ddc_model::ColumnSpec {
            level: e.level,
            top_level: None,
            member_type: mt,
            structural: true,
            at: (1.0, 1.0),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        };
        {
            e.model.add_column(&spec).unwrap();
            e.reload().unwrap();
            assert_eq!(count(&e, Cat::Columns), 1);
            let tb = super::super::build(&e, Cat::Columns);
            assert_eq!(tb.rows.len(), 1);
            assert_eq!(tb.rows[0].vals[2], Val::T("\u{414}\u{430}".into()));
            let en = super::super::build_in(&e, Cat::Columns, false);
            assert_eq!(en.rows[0].vals[2], Val::T("Yes".into()));
        }
    }
}
