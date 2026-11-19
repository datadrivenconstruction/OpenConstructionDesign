//! One [`ElementRow`] per element, read from the model. Quantities are the values the schedules
//! and the xlsx export read (`*_qty` tables, `ddc_geom::arc::axis_length`); nothing is measured
//! a second time here except what a table does not hold (wall face areas, run lengths).

use std::collections::HashMap;

use ddc_model::Model;
use rusqlite::types::Value;
use sha2::{Digest, Sha256};

use super::{key_ok, ElementRow, Note};

/// Hash of an element without geometry.
pub const GEOMETRY_NONE: &str = "0000000000000000";

/// Geometry of one element, from the caller's meshes: box in GLB space and a hash of the
/// world-space triangles.
#[derive(Clone, Debug, PartialEq)]
pub struct Geo {
    pub bbox: [f64; 6],
    pub hash: String,
}

/// Geometry by (model table, element id).
pub type GeoMap = HashMap<(String, i64), Geo>;

#[derive(Debug, Default)]
pub struct Collected {
    pub rows: Vec<ElementRow>,
    pub notes: Vec<Note>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn has_table(m: &Model, t: &str) -> bool {
    m.sql()
        .query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
        .unwrap_or(false)
}

fn run(m: &Model, sql: &str) -> Result<Vec<Vec<Value>>, String> {
    let mut st = m.sql().prepare(sql).map_err(err)?;
    let n = st.column_count();
    let mut q = st.query([]).map_err(err)?;
    let mut out = vec![];
    while let Some(r) = q.next().map_err(err)? {
        out.push((0..n).map(|i| r.get::<_, Value>(i)).collect::<Result<Vec<_>, _>>().map_err(err)?);
    }
    Ok(out)
}

fn f(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(x) => Some(*x),
        _ => None,
    }
}

fn i(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Real(x) if x.fract() == 0.0 => Some(*x as i64),
        _ => None,
    }
}

fn s(v: &Value) -> Option<String> {
    match v {
        Value::Text(t) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

/// Short lower-case ASCII tag of a name for `type_key`: letters and digits, `-` between.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c == '.' || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() {
        "unnamed".into()
    } else {
        t
    }
}

/// Zup box (x0,y0,z0,x1,y1,z1) of the model to the GLB's Y-up box.
pub fn glb_box(b: [f64; 6]) -> [f64; 6] {
    [b[0].min(b[3]), b[2].min(b[5]), (-b[1]).min(-b[4]), b[0].max(b[3]), b[2].max(b[5]), (-b[1]).max(-b[4])]
}

/// First 32 hex digits of the SHA-256 of `bytes`.
pub fn short_hash(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

struct Spec {
    cat: &'static str,
    table: &'static str,
    etype: &'static str,
    ifc: &'static str,
    disc: &'static str,
    tkey: &'static str,
    sql: &'static str,
}

// Select list: id, level, name, type name, material, length, area, area gross, volume, volume gross,
// box x0 y0 z0 x1 y1 z1 (model space, Z up), then category extras.
macro_rules! sel {
    ($($t:tt)*) => { concat!($($t)*) };
}

const SPECS: &[Spec] = &[
    Spec {
        cat: "wall",
        table: "wall",
        etype: "Wall",
        ifc: "IfcWall",
        disc: "architecture",
        tkey: "wall",
        sql: sel!(
            "SELECT w.id, w.level, t.name, t.name, \
             (SELECT l.material FROM wall_type_layer l WHERE l.wtype=w.wtype ORDER BY l.thickness DESC, l.pos LIMIT 1), \
             NULL, NULL, NULL, (SELECT SUM(q.volume) FROM wall_qty q WHERE q.wall=w.id), NULL, ",
            "NULL,NULL,NULL,NULL,NULL,NULL",
            ", w.ax, w.ay, w.bx, w.by, w.bulge, w.height, \
             (SELECT COALESCE(SUM(o.width*o.height),0) FROM opening o WHERE o.wall=w.id), \
             (SELECT COALESCE(SUM(l.thickness),0) FROM wall_type_layer l WHERE l.wtype=w.wtype) \
             FROM wall w JOIN wall_type t ON t.id=w.wtype ORDER BY w.id"
        ),
    },
    Spec {
        cat: "opening",
        table: "opening",
        etype: "Opening",
        ifc: "IfcOpeningElement",
        disc: "architecture",
        tkey: "opening",
        sql: "SELECT o.id, w.level, o.kind, o.kind, NULL, o.width, o.width*o.height, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL, o.kind, o.width, o.height \
              FROM opening o JOIN wall w ON w.id=o.wall ORDER BY o.id",
    },
    Spec {
        cat: "slab",
        table: "slab",
        etype: "Slab",
        ifc: "IfcSlab",
        disc: "structure",
        tkey: "slab",
        sql: "SELECT s.id, s.level, t.name, t.name, \
              (SELECT l.material FROM slab_type_layer l WHERE l.stype=s.stype ORDER BY l.thickness DESC, l.pos LIMIT 1), \
              NULL, (SELECT MAX(q.area) FROM slab_qty q WHERE q.slab=s.id), NULL, \
              (SELECT SUM(q.volume) FROM slab_qty q WHERE q.slab=s.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM slab s JOIN slab_type t ON t.id=s.stype ORDER BY s.id",
    },
    Spec {
        cat: "roof",
        table: "roof",
        etype: "Roof",
        ifc: "IfcRoof",
        disc: "structure",
        tkey: "slab/roof",
        sql: "SELECT r.id, r.level, t.name, t.name, \
              (SELECT l.material FROM slab_type_layer l WHERE l.stype=r.stype ORDER BY l.thickness DESC, l.pos LIMIT 1), \
              NULL, (SELECT MAX(q.area) FROM roof_qty q WHERE q.roof=r.id), NULL, \
              (SELECT SUM(q.volume) FROM roof_qty q WHERE q.roof=r.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM roof r JOIN slab_type t ON t.id=r.stype ORDER BY r.id",
    },
    Spec {
        cat: "ceiling",
        table: "ceiling",
        etype: "Ceiling",
        ifc: "IfcCovering",
        disc: "architecture",
        tkey: "ceiling",
        sql: "SELECT c.id, c.level, t.name, t.name, \
              (SELECT l.material FROM slab_type_layer l WHERE l.stype=c.stype ORDER BY l.thickness DESC, l.pos LIMIT 1), \
              NULL, (SELECT MAX(q.area) FROM ceiling_qty q WHERE q.ceiling=c.id), NULL, \
              (SELECT SUM(q.volume) FROM ceiling_qty q WHERE q.ceiling=c.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM ceiling c JOIN slab_type t ON t.id=c.stype ORDER BY c.id",
    },
    Spec {
        cat: "room",
        table: "room",
        etype: "Room",
        ifc: "IfcSpace",
        disc: "architecture",
        tkey: "space",
        sql: "SELECT r.id, r.level, r.name, r.name, NULL, q.perimeter, q.area, NULL, q.volume, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL, q.rings, r.height, r.number \
              FROM room r LEFT JOIN room_qty q ON q.room=r.id ORDER BY r.id",
    },
    Spec {
        cat: "column",
        table: "col",
        etype: "Column",
        ifc: "IfcColumn",
        disc: "structure",
        tkey: "column",
        sql: "SELECT c.id, c.level, t.name, t.name, t.material, q.length, NULL, q.surface, q.volume, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL, c.x, c.y \
              FROM col c JOIN member_type t ON t.id=c.mtype \
              LEFT JOIN member_qty q ON q.kind='column' AND q.ref=c.id ORDER BY c.id",
    },
    Spec {
        cat: "beam",
        table: "beam",
        etype: "Beam",
        ifc: "IfcBeam",
        disc: "structure",
        tkey: "beam",
        sql: "SELECT b.id, b.level, t.name, t.name, t.material, q.length, NULL, q.surface, q.volume, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM beam b JOIN member_type t ON t.id=b.mtype \
              LEFT JOIN member_qty q ON q.kind='beam' AND q.ref=b.id ORDER BY b.id",
    },
    Spec {
        cat: "brace",
        table: "brace",
        etype: "Brace",
        ifc: "IfcMember",
        disc: "structure",
        tkey: "brace",
        sql: "SELECT b.id, b.level, t.name, t.name, t.material, \
              (SELECT SUM(q.length) FROM struct_qty q WHERE q.kind='brace' AND q.ref=b.id), NULL, \
              (SELECT SUM(q.area) FROM struct_qty q WHERE q.kind='brace' AND q.ref=b.id), \
              (SELECT SUM(q.volume) FROM struct_qty q WHERE q.kind='brace' AND q.ref=b.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM brace b JOIN member_type t ON t.id=b.mtype ORDER BY b.id",
    },
    Spec {
        cat: "footing",
        table: "iso_footing",
        etype: "Footing",
        ifc: "IfcFooting",
        disc: "structure",
        tkey: "footing/isolated",
        sql: "SELECT f.id, f.level, 'Footing', 'isolated', f.material, \
              (SELECT SUM(q.length) FROM struct_qty q WHERE q.kind='iso_footing' AND q.ref=f.id), \
              (SELECT SUM(q.area) FROM struct_qty q WHERE q.kind='iso_footing' AND q.ref=f.id), NULL, \
              (SELECT SUM(q.volume) FROM struct_qty q WHERE q.kind='iso_footing' AND q.ref=f.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM iso_footing f ORDER BY f.id",
    },
    Spec {
        cat: "strip_footing",
        table: "strip_footing",
        etype: "Footing",
        ifc: "IfcFooting",
        disc: "structure",
        tkey: "footing/strip",
        sql: "SELECT f.id, f.level, 'Strip footing', 'strip', f.material, \
              (SELECT SUM(q.length) FROM struct_qty q WHERE q.kind='strip_footing' AND q.ref=f.id), \
              (SELECT SUM(q.area) FROM struct_qty q WHERE q.kind='strip_footing' AND q.ref=f.id), NULL, \
              (SELECT SUM(q.volume) FROM struct_qty q WHERE q.kind='strip_footing' AND q.ref=f.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM strip_footing f ORDER BY f.id",
    },
    Spec {
        cat: "raft",
        table: "raft",
        etype: "Slab",
        ifc: "IfcSlab",
        disc: "structure",
        tkey: "slab/foundation-raft",
        sql: "SELECT f.id, f.level, 'Raft', 'raft', f.material, \
              (SELECT SUM(q.length) FROM struct_qty q WHERE q.kind='raft' AND q.ref=f.id), \
              (SELECT SUM(q.area) FROM struct_qty q WHERE q.kind='raft' AND q.ref=f.id), NULL, \
              (SELECT SUM(q.volume) FROM struct_qty q WHERE q.kind='raft' AND q.ref=f.id), NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM raft f ORDER BY f.id",
    },
    Spec {
        cat: "stair",
        table: "stair",
        etype: "Stair",
        ifc: "IfcStair",
        disc: "architecture",
        tkey: "stair",
        sql: "SELECT s.id, s.level, 'Stair', s.kind, s.material, q.run, NULL, NULL, \
              q.volume + q.landing_volume + q.stringer_volume, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM stair s LEFT JOIN stair_qty q ON q.stair=s.id ORDER BY s.id",
    },
    Spec {
        cat: "railing",
        table: "railing",
        etype: "Railing",
        ifc: "IfcRailing",
        disc: "architecture",
        tkey: "railing",
        sql: "SELECT r.id, r.level, 'Railing', 'railing', r.material, q.length, NULL, NULL, q.volume, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM railing r LEFT JOIN railing_qty q ON q.railing=r.id ORDER BY r.id",
    },
    Spec {
        cat: "curtain_wall",
        table: "curtain_wall",
        etype: "CurtainWall",
        ifc: "IfcCurtainWall",
        disc: "architecture",
        tkey: "curtain-wall",
        sql: "SELECT c.id, c.level, c.name, 'curtain-wall', NULL, NULL, NULL, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL, c.ax, c.ay, c.bx, c.by, c.bulge, c.height \
              FROM curtain_wall c ORDER BY c.id",
    },
    Spec {
        cat: "ramp",
        table: "ramp",
        etype: "Ramp",
        ifc: "IfcRamp",
        disc: "architecture",
        tkey: "ramp",
        sql: "SELECT r.id, r.level, 'Ramp', 'ramp', NULL, NULL, NULL, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM ramp r ORDER BY r.id",
    },
    Spec {
        cat: "mass",
        table: "mass",
        etype: "Mass",
        ifc: "IfcBuildingElementProxy",
        disc: "other",
        tkey: "mass",
        sql: "SELECT m.id, m.level, m.name, 'mass', NULL, NULL, NULL, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM mass m ORDER BY m.id",
    },
    Spec {
        cat: "mesh",
        table: "mesh",
        etype: "Mesh",
        ifc: "IfcBuildingElementProxy",
        disc: "other",
        tkey: "mesh",
        sql: "SELECT m.id, m.level, m.name, m.category, NULL, NULL, NULL, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM mesh m ORDER BY m.id",
    },
    Spec {
        cat: "solid",
        table: "solid",
        etype: "Solid",
        ifc: "IfcBuildingElementProxy",
        disc: "other",
        tkey: "solid",
        sql: "SELECT s.id, s.level, s.name, s.kind, NULLIF(s.material,''), NULL, q.area, NULL, q.net_volume, q.volume, \
              q.x0,q.y0,q.z0,q.x1,q.y1,q.z1 FROM solid s LEFT JOIN solid_qty q ON q.solid=s.id \
              WHERE s.void=0 ORDER BY s.id",
    },
    Spec {
        cat: "toposolid",
        table: "toposolid",
        etype: "Toposolid",
        ifc: "IfcGeographicElement",
        disc: "site",
        tkey: "toposolid",
        sql: "SELECT t.id, NULL, t.name, 'toposolid', NULL, NULL, NULL, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL FROM toposolid t ORDER BY t.id",
    },
    Spec {
        cat: "mep_run",
        table: "mep_run",
        etype: "MepRun",
        ifc: "IfcBuildingElementProxy",
        disc: "mep",
        tkey: "mep",
        sql: "SELECT r.id, r.level, s.name, r.domain || '-' || r.shape, r.material, NULL, NULL, NULL, NULL, NULL, \
              NULL,NULL,NULL,NULL,NULL,NULL, r.points, r.domain \
              FROM mep_run r JOIN mep_system s ON s.id=r.system ORDER BY r.id",
    },
];

fn pos(v: &[Value], k: usize) -> Option<f64> {
    v.get(k).and_then(f)
}

fn run_length(points: &str) -> Option<f64> {
    let j: serde_json::Value = serde_json::from_str(points).ok()?;
    let p: Vec<[f64; 3]> = j
        .as_array()?
        .iter()
        .map(|q| {
            let a = q.as_array()?;
            Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2).and_then(|z| z.as_f64()).unwrap_or(0.0)])
        })
        .collect::<Option<_>>()?;
    Some(p.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2) + (w[1][2] - w[0][2]).powi(2)).sqrt()).sum())
}

fn in_ring(r: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut inside = false;
    let mut j = r.len().wrapping_sub(1);
    for k in 0..r.len() {
        let (a, b) = (r[k], r[j]);
        if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = k;
    }
    inside
}

/// Parameters other than the key properties, as flat JSON text.
fn props_json(m: &Model, table: &str, cat: &str, id: i64) -> Result<Option<String>, String> {
    let mut o = serde_json::Map::new();
    for kind in [table, cat] {
        for (k, v, unit) in m.props(kind, id).map_err(err)? {
            if k == ddc_model::KEY_PROP || k == ddc_model::MINTED_PROP {
                continue;
            }
            let name = if unit.is_empty() { k } else { format!("{k} [{unit}]") };
            let j = match v {
                Value::Integer(n) => serde_json::json!(n),
                Value::Real(x) if x.is_finite() => serde_json::json!(x),
                Value::Real(x) => serde_json::json!(x.to_string()),
                Value::Text(t) => serde_json::json!(t),
                Value::Blob(_) | Value::Null => continue,
            };
            o.entry(name).or_insert(j);
        }
        if cat == table {
            break;
        }
    }
    Ok(if o.is_empty() { None } else { Some(serde_json::Value::Object(o).to_string()) })
}

fn key_of(m: &mut Model, table: &str, cat: &str, id: i64) -> Result<String, String> {
    if let Some((k, _)) = m.element_key(table, id).map_err(err)? {
        return Ok(k);
    }
    if cat != table {
        if let Some((k, _)) = m.element_key(cat, id).map_err(err)? {
            return Ok(k);
        }
    }
    Ok(m.ensure_element_key(table, id).map_err(err)?.0)
}

/// Rows of every element of `m`, in category order and by id. Keys are minted (and stored in the
/// model, undoably) for elements that have none. `geo` gives box and hash per (table, id); an
/// element without an entry gets a model-derived box where the model holds one (rooms, components,
/// solids) and none otherwise, which is noted.
pub fn collect(m: &mut Model, geo: &GeoMap) -> Result<Collected, String> {
    let mut out = Collected::default();
    let levels: HashMap<i64, (String, f64)> =
        m.level_list().map_err(err)?.into_iter().map(|(id, n, z)| (id, (n, z))).collect();
    let mut rooms: HashMap<i64, Vec<(String, Vec<Vec<(f64, f64)>>)>> = HashMap::new();
    let mut seen: HashMap<String, (String, i64)> = HashMap::new();

    macro_rules! push {
        ($row:expr, $table:expr, $id:expr) => {{
            let row: ElementRow = $row;
            if !key_ok(&row.key) {
                return Err(format!("{} {}: key '{}' is not in the contract grammar", $table, $id, row.key));
            }
            if let Some((t, n)) = seen.insert(row.key.clone(), ($table.to_string(), $id)) {
                return Err(format!("key '{}' names both {t} {n} and {} {}", row.key, $table, $id));
            }
            out.rows.push(row);
        }};
    }

    for sp in SPECS {
        if !has_table(m, sp.table) {
            continue;
        }
        let rows = match run(m, sp.sql) {
            Ok(r) => r,
            // a quantity table that this file never created: the category has no quantities
            Err(e) if e.contains("no such table") => {
                out.notes.push(Note { key: sp.cat.into(), field: "quantities", what: format!("skipped: {e}") });
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", sp.cat)),
        };
        for v in rows {
            let id = i(&v[0]).ok_or("element id is not an integer")?;
            let level = v[1].clone();
            let storey = i(&level).and_then(|l| levels.get(&l)).map(|l| l.0.clone());
            let elev = i(&level).and_then(|l| levels.get(&l)).map_or(0.0, |l| l.1);
            let key = key_of(m, sp.table, sp.cat, id)?;
            let tname = s(&v[3]).unwrap_or_default();
            let mut name = s(&v[2]).unwrap_or_else(|| tname.clone());
            let (mut etype, mut ifc) = (sp.etype.to_string(), sp.ifc.to_string());
            let mut tkey = format!("{}/{}", sp.tkey, slug(&tname));
            let mut length = pos(&v, 5);
            let mut area = pos(&v, 6);
            let mut gross = pos(&v, 7);
            let vol = pos(&v, 8);
            let mut vgross = pos(&v, 9);
            let mut bbox_model: Option<[f64; 6]> = None;
            let mut disc = sp.disc;
            let mut hash_src: Option<Vec<u8>> = None;
            if (10..16).all(|k| pos(&v, k).is_some()) {
                bbox_model = Some([10, 11, 12, 13, 14, 15].map(|k| pos(&v, k).unwrap_or(0.0)));
            }
            match sp.cat {
                "wall" | "curtain_wall" => {
                    let g = |k| pos(&v, k).unwrap_or(0.0);
                    let (a, b) = (ddc_geom::v2(g(16), g(17)), ddc_geom::v2(g(18), g(19)));
                    let len = ddc_geom::arc::axis_length(a, b, g(20));
                    length = Some(len);
                    gross = Some(len * g(21));
                    if sp.cat == "wall" {
                        let holes = g(22);
                        area = Some((len * g(21) - holes).max(0.0));
                        let thick = g(23);
                        vgross = Some(len * g(21) * thick);
                        tkey = format!("wall/{}", slug(&tname));
                    } else {
                        area = gross;
                        tkey = "curtain-wall/standard".into();
                    }
                }
                "opening" => {
                    let kind = s(&v[2]).unwrap_or_default();
                    let (w, h) = (pos(&v, 17).unwrap_or(0.0), pos(&v, 18).unwrap_or(0.0));
                    let class = match kind.as_str() {
                        "door" => ("Door", "IfcDoor", "door"),
                        "window" => ("Window", "IfcWindow", "window"),
                        _ => ("Opening", "IfcOpeningElement", "opening"),
                    };
                    etype = class.0.into();
                    ifc = class.1.into();
                    tkey = format!("{}/{}-{}x{}", class.2, slug(&kind), (w * 1000.0).round(), (h * 1000.0).round());
                    name = kind;
                }
                "room" => {
                    let h = pos(&v, 17).unwrap_or(0.0);
                    if let Some(txt) = s(&v[16]) {
                        if let Ok(r) = serde_json::from_str::<Vec<Vec<(f64, f64)>>>(&txt) {
                            let pts = r.iter().flatten();
                            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                            for p in pts {
                                x0 = x0.min(p.0);
                                y0 = y0.min(p.1);
                                x1 = x1.max(p.0);
                                y1 = y1.max(p.1);
                            }
                            if x0 <= x1 {
                                bbox_model = Some([x0, y0, elev, x1, y1, elev + h]);
                            }
                            if let Some(l) = i(&level) {
                                rooms.entry(l).or_default().push((key.clone(), r));
                            }
                        }
                        hash_src = Some(format!("room|{txt}|{elev}|{h}").into_bytes());
                    }
                    // the kind of room is part of the generated key: `.../room-kitchen`
                    let kind = key.rsplit('/').find_map(|p| p.strip_prefix("room-")).map(str::to_string);
                    tkey = format!("space/{}", kind.unwrap_or_else(|| slug(&tname)));
                }
                "stair" => tkey = format!("stair/{}", slug(&tname)),
                "mep_run" => {
                    length = s(&v[16]).and_then(|p| run_length(&p));
                    if length.is_none() {
                        out.notes.push(Note { key: key.clone(), field: "length_m", what: "run points could not be read".into() });
                    }
                    let domain = s(&v[17]).unwrap_or_default();
                    ifc = match domain.as_str() {
                        "duct" => "IfcDuctSegment",
                        "pipe" => "IfcPipeSegment",
                        "cable" | "electrical" => "IfcCableCarrierSegment",
                        _ => "IfcBuildingElementProxy",
                    }
                    .into();
                    tkey = format!("mep/{}", slug(&tname));
                }
                _ => {}
            }
            // the same text the model holds for a column's place, for the containing space
            let col_at = (sp.cat == "column").then(|| (pos(&v, 16).unwrap_or(0.0), pos(&v, 17).unwrap_or(0.0)));
            let mut space = None;
            if let (Some((x, y)), Some(l)) = (col_at, i(&level)) {
                space = rooms.get(&l).and_then(|rs| rs.iter().find(|(_, r)| r.first().is_some_and(|o| in_ring(o, x, y)))).map(|r| r.0.clone());
            }
            let (bbox, hash) = match geo.get(&(sp.table.to_string(), id)) {
                Some(g) => (g.bbox, g.hash.clone()),
                None => match bbox_model {
                    Some(b) => {
                        let h = hash_src.unwrap_or_else(|| format!("{:?}", b).into_bytes());
                        (glb_box(b), short_hash(&h))
                    }
                    None => {
                        out.notes.push(Note { key: key.clone(), field: "bbox", what: "no geometry given".into() });
                        ([0.0; 6], GEOMETRY_NONE.into())
                    }
                },
            };
            disc = if sp.cat == "mep_run" { "mep" } else { disc };
            push!(
                ElementRow {
                    key,
                    element_type: etype,
                    ifc_class: ifc,
                    type_key: tkey,
                    name: if name.is_empty() { sp.etype.into() } else { name },
                    storey,
                    space,
                    discipline: disc.into(),
                    material: s(&v[4]),
                    phase: None,
                    space_usage: None,
                    geometry_hash: hash,
                    length_m: length,
                    area_m2: area,
                    area_gross_m2: gross,
                    volume_m3: vol,
                    volume_gross_m3: vgross,
                    weight_kg: None,
                    count: Some(1),
                    bbox,
                    props: props_json(m, sp.table, sp.cat, id)?,
                },
                sp.table,
                id
            );
        }
    }

    // components: family, type, IFC class from the component record; box and quantities from comp_qty
    if has_table(m, "comp") {
        let recs = m.component_records(None).map_err(err)?;
        let qty: HashMap<i64, Vec<Value>> = run(m, "SELECT comp, volume, surface, x0, y0, z0, x1, y1, z1 FROM comp_qty")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| Some((i(&r[0])?, r)))
            .collect();
        for r in recs {
            let key = key_of(m, "comp", "component", r.id)?;
            let q = qty.get(&r.id);
            let storey = levels.get(&r.level).map(|l| l.0.clone());
            let ifc = if r.ifc_class.starts_with("Ifc") && r.ifc_class.chars().all(|c| c.is_ascii_alphabetic()) {
                r.ifc_class.clone()
            } else {
                out.notes.push(Note { key: key.clone(), field: "ifc_class", what: format!("'{}' is not an IFC class; IfcBuildingElementProxy", r.ifc_class) });
                "IfcBuildingElementProxy".into()
            };
            let disc = match ifc.as_str() {
                "IfcSanitaryTerminal" | "IfcFlowTerminal" | "IfcLamp" | "IfcLightFixture" | "IfcElectricAppliance" | "IfcValve" => "mep",
                "IfcColumn" | "IfcBeam" | "IfcMember" | "IfcFooting" => "structure",
                "IfcGeographicElement" => "site",
                _ => "architecture",
            };
            let boxm = q.and_then(|q| if (3..9).all(|k| pos(q, k).is_some()) { Some([3, 4, 5, 6, 7, 8].map(|k| pos(q, k).unwrap_or(0.0))) } else { None });
            let (bbox, hash) = match geo.get(&("comp".to_string(), r.id)) {
                Some(g) => (g.bbox, g.hash.clone()),
                None => match boxm {
                    Some(b) => (glb_box(b), short_hash(format!("{b:?}").as_bytes())),
                    None => {
                        out.notes.push(Note { key: key.clone(), field: "bbox", what: "no geometry given".into() });
                        ([0.0; 6], GEOMETRY_NONE.into())
                    }
                },
            };
            let space = boxm.and_then(|b| {
                let (cx, cy) = ((b[0] + b[3]) / 2.0, (b[1] + b[4]) / 2.0);
                rooms.get(&r.level).and_then(|rs| rs.iter().find(|(_, g)| g.first().is_some_and(|o| in_ring(o, cx, cy)))).map(|x| x.0.clone())
            });
            push!(
                ElementRow {
                    key,
                    element_type: format!("{}", r.category),
                    ifc_class: ifc,
                    type_key: format!("component/{}-{}", slug(&r.family), slug(&r.type_name)),
                    name: format!("{} {}", r.family, r.type_name).trim().to_string(),
                    storey,
                    space,
                    discipline: disc.into(),
                    material: None,
                    phase: None,
                    space_usage: None,
                    geometry_hash: hash,
                    length_m: None,
                    area_m2: None,
                    area_gross_m2: q.and_then(|q| pos(q, 2)),
                    volume_m3: q.and_then(|q| pos(q, 1)),
                    volume_gross_m3: None,
                    weight_kg: None,
                    count: Some(1),
                    bbox,
                    props: props_json(m, "comp", "component", r.id)?,
                },
                "comp",
                r.id
            );
        }
    }
    Ok(out)
}
