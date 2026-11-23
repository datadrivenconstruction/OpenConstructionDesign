// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Meaningful camera poses for `view_snapshot`: the agent names what to look at (a room, a
//! level plan, a side of the compass) and gets the pose with the world-to-pixel matrix, so a
//! picture ties back to model coordinates. Pure and deterministic: no GPU, no clock; the same
//! model and pose give the same numbers. Drawing is `snapshot`.

use super::plan::ElRef;
use super::render_req::resolve_level;
use crate::editor::{Editor, Sel};
use serde_json::{json, Value as J};

/// The frame is the target's extent plus this share.
pub const MARGIN: f64 = 0.05;
pub const MAX_HIGHLIGHT: usize = ddc_ai::limits::HIGHLIGHT.us();
pub const MAX_LABELS: usize = ddc_ai::limits::LABELS.us();
/// Share of the storey height where a room interior is cut.
pub const ROOM_CUT: f64 = 0.65;
pub const FOV_DEG: f64 = 45.0;
/// True isometric elevation of the eye, degrees.
const ISO_PITCH_DEG: f64 = 35.264_389_682_754_654;
/// Elevation of the compass view without an eye height, degrees.
const COMPASS_PITCH_DEG: f64 = 30.0;
/// Storey height when no level lies above.
const DEFAULT_STOREY: f64 = 3.0;
const MAX_GAP: f64 = ddc_ai::limits::POSE_GAP.f();

type V3 = [f64; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side8 {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Side8 {
    pub const ALL: [Side8; 8] = [
        Side8::N,
        Side8::NE,
        Side8::E,
        Side8::SE,
        Side8::S,
        Side8::SW,
        Side8::W,
        Side8::NW,
    ];

    pub fn parse(s: &str) -> Option<Side8> {
        let t = s.trim().to_ascii_uppercase();
        let t = match t.as_str() {
            "NORTH" => "N",
            "NORTHEAST" => "NE",
            "EAST" => "E",
            "SOUTHEAST" => "SE",
            "SOUTH" => "S",
            "SOUTHWEST" => "SW",
            "WEST" => "W",
            "NORTHWEST" => "NW",
            o => o,
        };
        Side8::ALL.into_iter().find(|k| k.name() == t)
    }

    pub fn name(self) -> &'static str {
        match self {
            Side8::N => "N",
            Side8::NE => "NE",
            Side8::E => "E",
            Side8::SE => "SE",
            Side8::S => "S",
            Side8::SW => "SW",
            Side8::W => "W",
            Side8::NW => "NW",
        }
    }

    /// Bearing of the eye from the model centre, degrees clockwise from north.
    pub fn bearing(self) -> f64 {
        match self {
            Side8::N => 0.0,
            Side8::NE => 45.0,
            Side8::E => 90.0,
            Side8::SE => 135.0,
            Side8::S => 180.0,
            Side8::SW => 225.0,
            Side8::W => 270.0,
            Side8::NW => 315.0,
        }
    }
}

/// Unit vector in the plan for a bearing, degrees clockwise from north (x east, y north).
fn outward(bearing_deg: f64) -> [f64; 2] {
    let b = bearing_deg.to_radians();
    [b.sin(), b.cos()]
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pose {
    /// What the window shows; only the window knows it.
    Current,
    Plan { level: i64, rect: Option<[f64; 4]> },
    Section { view_def: i64 },
    Compass { side: Side8, eye_h: Option<f64> },
    Iso { az_deg: f64 },
    Top,
    RoomInterior { room: i64, cut: f64 },
    Exploded { gap_m: f64 },
}

/// A refusal the agent can act on: a stable code, what was wrong, how to put it right.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>, fix: impl Into<String>) -> Refusal {
        Refusal {
            code,
            message: message.into(),
            fix: fix.into(),
        }
    }

    /// The refusal on the wire: a typed code, with what was wrong and how to put it right as
    /// the detail.
    pub fn wire(&self) -> String {
        use ddc_ai::refusal::{self, RefusalCode as C};
        let code = match self.code {
            "unknown_target" => C::UnknownTarget,
            "empty_target" => C::NoTarget,
            "too_many" | "image_too_large" => C::LimitExceeded,
            "snapshot_unavailable" => C::SnapshotUnavailable,
            _ => C::InvalidArgs,
        };
        let detail = if code == C::SnapshotUnavailable {
            self.message.clone()
        } else {
            format!("{}: {}; fix: {}", self.code, self.message, self.fix)
        };
        refusal::wire(&refusal::of(code).detail(detail))
    }

    pub fn text(&self) -> String {
        format!("{}: {}; fix: {}", self.code, self.message, self.fix)
    }
}

// ---- facts read from the model ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct LevelFact {
    pub id: i64,
    pub name: String,
    pub elev: f64,
    pub height: f64,
    /// Plan extent `[x0, y0, x1, y1]` of what stands on the level.
    pub foot: Option<[f64; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomFact {
    pub id: i64,
    pub level: i64,
    pub number: String,
    pub name: String,
    pub ring: Vec<[f64; 2]>,
}

/// Where an element can be pointed at: wall middle, opening centre, room centroid.
#[derive(Clone, Debug, PartialEq)]
pub struct PointFact {
    /// Table group: wall, opening, room, curtain.
    pub kind: &'static str,
    /// Category word in references: wall, door, window, opening, room, curtain.
    pub cat: &'static str,
    pub id: i64,
    pub level: i64,
    pub at: V3,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewFact {
    pub id: i64,
    pub name: String,
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub depth: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub levels: Vec<LevelFact>,
    pub rooms: Vec<RoomFact>,
    pub points: Vec<PointFact>,
    pub views: Vec<ViewFact>,
    /// `[x0, y0, z0, x1, y1, z1]` of the whole model.
    pub bounds: Option<[f64; 6]>,
}

fn sql_rows<T>(
    ed: &Editor,
    sql: &str,
    f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Vec<T> {
    let Ok(mut st) = ed.model.sql().prepare(sql) else {
        return vec![];
    };
    let rows = st.query_map([], f).and_then(|it| it.collect::<Result<Vec<T>, _>>());
    rows.unwrap_or_default()
}

fn grow(b: &mut Option<[f64; 6]>, p: V3) {
    *b = Some(match *b {
        None => [p[0], p[1], p[2], p[0], p[1], p[2]],
        Some(o) => [
            o[0].min(p[0]),
            o[1].min(p[1]),
            o[2].min(p[2]),
            o[3].max(p[0]),
            o[4].max(p[1]),
            o[5].max(p[2]),
        ],
    });
}

fn grow_foot(m: &mut std::collections::HashMap<i64, [f64; 4]>, level: i64, x: f64, y: f64) {
    let e = m.entry(level).or_insert([x, y, x, y]);
    e[0] = e[0].min(x);
    e[1] = e[1].min(y);
    e[2] = e[2].max(x);
    e[3] = e[3].max(y);
}

/// Area-weighted centroid of a ring; the vertex mean when the ring has no area.
pub fn centroid(ring: &[[f64; 2]]) -> [f64; 2] {
    let n = ring.len();
    if n == 0 {
        return [0.0, 0.0];
    }
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        let w = p[0] * q[1] - q[0] * p[1];
        a2 += w;
        cx += (p[0] + q[0]) * w;
        cy += (p[1] + q[1]) * w;
    }
    if a2.abs() < 1e-12 {
        let k = n as f64;
        return [
            ring.iter().map(|p| p[0]).sum::<f64>() / k,
            ring.iter().map(|p| p[1]).sum::<f64>() / k,
        ];
    }
    [cx / (3.0 * a2), cy / (3.0 * a2)]
}

/// What the poses need from the model, read in one pass over its tables.
pub fn facts_of(ed: &Editor) -> Facts {
    let lv = ed.levels();
    let elev_of = |id: i64| lv.iter().find(|l| l.0 == id).map_or(0.0, |l| l.2);
    let height_of = |id: i64| {
        let Some(i) = lv.iter().position(|l| l.0 == id) else {
            return DEFAULT_STOREY;
        };
        lv.get(i + 1)
            .map(|n| n.2 - lv[i].2)
            .filter(|h| *h > 0.1)
            .unwrap_or(DEFAULT_STOREY)
    };
    let mut bounds: Option<[f64; 6]> = None;
    let mut foot = std::collections::HashMap::new();
    let mut points = vec![];

    type W = (i64, i64, f64, f64, f64, f64, f64, f64);
    let walls: Vec<W> = sql_rows(
        ed,
        "SELECT id, level, ax, ay, bx, by, base_offset, height FROM wall ORDER BY id",
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        },
    );
    for &(id, level, ax, ay, bx, by, base, h) in &walls {
        let z0 = elev_of(level) + base;
        for (x, y) in [(ax, ay), (bx, by)] {
            grow(&mut bounds, [x, y, z0]);
            grow(&mut bounds, [x, y, z0 + h]);
            grow_foot(&mut foot, level, x, y);
        }
        points.push(PointFact {
            kind: "wall",
            cat: "wall",
            id,
            level,
            at: [(ax + bx) / 2.0, (ay + by) / 2.0, z0 + h / 2.0],
            text: format!("wall:{id}"),
        });
    }

    type O = (i64, i64, String, f64, f64, f64, f64, f64, f64, f64, f64, f64);
    let openings: Vec<O> = sql_rows(
        ed,
        "SELECT o.id, w.level, o.kind, o.along, o.width, o.sill, o.height, w.ax, w.ay, w.bx, w.by,
                w.base_offset
         FROM opening o JOIN wall w ON w.id=o.wall ORDER BY o.id",
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
            ))
        },
    );
    for (id, level, kind, along, width, sill, h, ax, ay, bx, by, base) in openings {
        let len = (bx - ax).hypot(by - ay);
        // an arc wall is pointed at by its chord: the label lands beside the opening, not on it
        let t = if len > 1e-9 {
            ((along + width / 2.0) / len).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let cat = match kind.as_str() {
            "door" => "door",
            "void" => "opening",
            _ => "window",
        };
        points.push(PointFact {
            kind: "opening",
            cat,
            id,
            level,
            at: [
                ax + (bx - ax) * t,
                ay + (by - ay) * t,
                elev_of(level) + base + sill + h / 2.0,
            ],
            text: format!("{cat}:{id}"),
        });
    }

    type C = (i64, i64, f64, f64, f64, f64, f64, f64);
    let curtains: Vec<C> = sql_rows(
        ed,
        "SELECT id, level, ax, ay, bx, by, base_offset, height FROM curtain_wall ORDER BY id",
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        },
    );
    for (id, level, ax, ay, bx, by, base, h) in curtains {
        let z0 = elev_of(level) + base;
        for (x, y) in [(ax, ay), (bx, by)] {
            grow(&mut bounds, [x, y, z0]);
            grow(&mut bounds, [x, y, z0 + h]);
            grow_foot(&mut foot, level, x, y);
        }
        points.push(PointFact {
            kind: "curtain",
            cat: "curtain",
            id,
            level,
            at: [(ax + bx) / 2.0, (ay + by) / 2.0, z0 + h / 2.0],
            text: format!("curtain:{id}"),
        });
    }

    let room_rows: Vec<(i64, i64, String, String)> = sql_rows(
        ed,
        "SELECT id, level, name, number FROM room ORDER BY id",
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    let mut rooms = vec![];
    for (id, level, name, number) in room_rows {
        let Some(q) = ed.model.room_quantities(id).ok().flatten() else {
            continue;
        };
        let ring: Vec<[f64; 2]> = q
            .rings
            .first()
            .map(|r| r.iter().map(|&(x, y)| [x, y]).collect())
            .unwrap_or_default();
        if ring.len() < 3 {
            continue;
        }
        let (e, h) = (elev_of(level), height_of(level));
        for p in &ring {
            grow(&mut bounds, [p[0], p[1], e]);
            grow(&mut bounds, [p[0], p[1], e + h]);
            grow_foot(&mut foot, level, p[0], p[1]);
        }
        let c = centroid(&ring);
        points.push(PointFact {
            kind: "room",
            cat: "room",
            id,
            level,
            at: [c[0], c[1], e + h.min(2.0) / 2.0],
            text: if number.is_empty() {
                format!("room:{id}")
            } else {
                number.clone()
            },
        });
        rooms.push(RoomFact {
            id,
            level,
            number,
            name,
            ring,
        });
    }

    let levels = lv
        .iter()
        .map(|l| LevelFact {
            id: l.0,
            name: l.1.clone(),
            elev: l.2,
            height: height_of(l.0),
            foot: foot.get(&l.0).copied(),
        })
        .collect();
    let views = ed
        .model
        .view_defs()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, s)| ViewFact {
            id,
            name: s.name,
            a: [s.a.0, s.a.1],
            b: [s.b.0, s.b.1],
            depth: s.depth,
        })
        .collect();
    Facts {
        levels,
        rooms,
        points,
        views,
        bounds,
    }
}

// ---- arguments ----------------------------------------------------------------------------

fn bad(message: impl Into<String>) -> Refusal {
    Refusal::new(
        "bad_pose",
        message,
        "give exactly one of plan, section, compass, iso, top, room, exploded, current",
    )
}

fn num_of(a: &J, k: &str) -> Result<Option<f64>, Refusal> {
    match &a[k] {
        J::Null => Ok(None),
        v => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or_else(|| bad(format!("{k} must be a number"))),
    }
}

fn level_list(f: &Facts) -> Vec<(i64, String, f64)> {
    f.levels.iter().map(|l| (l.id, l.name.clone(), l.elev)).collect()
}

fn unknown(what: &str, name: &str, known: String) -> Refusal {
    Refusal::new(
        "unknown_target",
        format!("no {what} {}", super::fmt::text(name)),
        format!("use one of: {known}"),
    )
}

/// The pose arguments of `view_snapshot`. `{}` and `{"current": true}` mean the window's view.
pub fn parse_pose(a: &J, f: &Facts) -> Result<Pose, Refusal> {
    let Some(o) = a.as_object() else {
        return Err(bad("pose must be an object"));
    };
    const KEYS: [&str; 8] = [
        "plan", "section", "compass", "iso", "top", "room", "exploded", "current",
    ];
    let given: Vec<&str> = KEYS.iter().copied().filter(|k| o.contains_key(*k)).collect();
    let extra: Vec<&String> = o
        .keys()
        .filter(|k| !KEYS.contains(&k.as_str()) && !["rect", "eye_h", "cut"].contains(&k.as_str()))
        .collect();
    if let Some(k) = extra.first() {
        return Err(bad(format!("unknown pose field {}", super::fmt::text(k))));
    }
    let key = match given.as_slice() {
        [] => return Ok(Pose::Current),
        [k] => *k,
        _ => return Err(bad("more than one of plan, section, compass, iso, top, room, exploded")),
    };
    let only = |allowed: &[&str]| -> Result<(), Refusal> {
        for k in ["rect", "eye_h", "cut"] {
            if o.contains_key(k) && !allowed.contains(&k) {
                return Err(bad(format!("{k} does not go with {key}")));
            }
        }
        Ok(())
    };
    match key {
        "current" => {
            only(&[])?;
            Ok(Pose::Current)
        }
        "plan" => {
            only(&["rect"])?;
            let level = resolve_level(&a["plan"], &level_list(f)).map_err(|e| {
                Refusal::new("unknown_target", e, "use a level id or full level name from list_levels")
            })?;
            let rect = match &a["rect"] {
                J::Null => None,
                v => {
                    let r: Vec<f64> = v
                        .as_array()
                        .filter(|r| r.len() == 4)
                        .map(|r| r.iter().filter_map(J::as_f64).collect())
                        .filter(|r: &Vec<f64>| r.len() == 4 && r.iter().all(|x| x.is_finite()))
                        .ok_or_else(|| bad("rect must be [x0, y0, x1, y1] in metres"))?;
                    let (x0, x1) = (r[0].min(r[2]), r[0].max(r[2]));
                    let (y0, y1) = (r[1].min(r[3]), r[1].max(r[3]));
                    if x1 - x0 < 0.01 || y1 - y0 < 0.01 {
                        return Err(bad("rect must span at least 0.01 m on both axes"));
                    }
                    Some([x0, y0, x1, y1])
                }
            };
            Ok(Pose::Plan { level, rect })
        }
        "section" => {
            only(&[])?;
            let hit = match &a["section"] {
                J::Number(n) => n.as_i64().and_then(|id| f.views.iter().find(|v| v.id == id)),
                J::String(s) => f.views.iter().find(|v| v.name.eq_ignore_ascii_case(s.trim())),
                _ => return Err(bad("section must be a view id or name")),
            };
            match hit {
                Some(v) => Ok(Pose::Section { view_def: v.id }),
                None => Err(unknown(
                    "section view",
                    &a["section"].to_string(),
                    f.views
                        .iter()
                        .map(|v| format!("{}:{}", v.id, v.name))
                        .collect::<Vec<_>>()
                        .join(", "),
                )),
            }
        }
        "compass" => {
            only(&["eye_h"])?;
            let s = a["compass"].as_str().unwrap_or_default();
            let side = Side8::parse(s)
                .ok_or_else(|| bad("compass must be N, NE, E, SE, S, SW, W or NW"))?;
            let eye_h = num_of(a, "eye_h")?;
            if let Some(h) = eye_h {
                if !(0.2..=100.0).contains(&h) {
                    return Err(bad("eye_h must be 0.2 to 100 metres"));
                }
            }
            Ok(Pose::Compass { side, eye_h })
        }
        "iso" => {
            only(&[])?;
            let az = a["iso"]
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| bad("iso must be the azimuth in degrees"))?;
            Ok(Pose::Iso { az_deg: az.rem_euclid(360.0) })
        }
        "top" => {
            only(&[])?;
            if a["top"] != json!(true) {
                return Err(bad("top must be true"));
            }
            Ok(Pose::Top)
        }
        "room" => {
            only(&["cut"])?;
            let hit = match &a["room"] {
                J::Number(n) => n.as_i64().and_then(|id| f.rooms.iter().find(|r| r.id == id)),
                J::String(s) => {
                    let s = s.trim();
                    let by_ref = s.strip_prefix("room:").and_then(|n| n.parse::<i64>().ok());
                    match by_ref {
                        Some(id) => f.rooms.iter().find(|r| r.id == id),
                        None => f.rooms.iter().find(|r| r.number == s),
                    }
                }
                _ => return Err(bad("room must be a room number or id")),
            };
            let Some(r) = hit else {
                return Err(unknown(
                    "room",
                    &a["room"].to_string(),
                    "a room number from query_elements category room".to_string(),
                ));
            };
            let cut = num_of(a, "cut")?.unwrap_or(ROOM_CUT);
            if !(0.1..=1.0).contains(&cut) {
                return Err(bad("cut must be 0.1 to 1.0 of the storey height"));
            }
            Ok(Pose::RoomInterior { room: r.id, cut })
        }
        _ => {
            only(&[])?;
            let gap = a["exploded"]
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| bad("exploded must be the gap between levels in metres"))?;
            if !(0.0..=MAX_GAP).contains(&gap) {
                return Err(bad(format!("exploded gap must be 0 to {MAX_GAP} metres")));
            }
            Ok(Pose::Exploded { gap_m: gap })
        }
    }
}

// ---- vector helpers -----------------------------------------------------------------------

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-12);
    mul(a, 1.0 / l)
}

/// Right, up and forward axes of a camera; `up` is the hint, the result is orthonormal.
pub fn axes(eye: V3, target: V3, up: V3) -> (V3, V3, V3) {
    let f = norm(sub(target, eye));
    let r = norm(cross(f, up));
    let u = cross(r, f);
    (r, u, f)
}

fn half_tan(fov_deg: f64) -> f64 {
    (fov_deg.to_radians() / 2.0).tan()
}

/// Row-major 4x4: world `[x, y, z, 1]` to `[px·w, py·w, depth, w]`; pixels from the top left.
/// Orthographic: `w = 1`; perspective: `w` is the depth along the view axis.
fn matrix(ortho: bool, (w, h): (u32, u32), eye: V3, target: V3, up: V3, fov_deg: f64, ppm: f64) -> [f64; 16] {
    let (r, u, f) = axes(eye, target, up);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    if ortho {
        let (rt, ut) = (dot(r, target), dot(u, target));
        return [
            r[0] * ppm, r[1] * ppm, r[2] * ppm, cx - ppm * rt,
            -u[0] * ppm, -u[1] * ppm, -u[2] * ppm, cy + ppm * ut,
            f[0], f[1], f[2], -dot(f, eye),
            0.0, 0.0, 0.0, 1.0,
        ];
    }
    let fpx = cy / half_tan(fov_deg);
    let (re, ue, fe) = (dot(r, eye), dot(u, eye), dot(f, eye));
    [
        cx * f[0] + fpx * r[0], cx * f[1] + fpx * r[1], cx * f[2] + fpx * r[2], -cx * fe - fpx * re,
        cy * f[0] - fpx * u[0], cy * f[1] - fpx * u[1], cy * f[2] - fpx * u[2], -cy * fe + fpx * ue,
        f[0], f[1], f[2], -fe,
        f[0], f[1], f[2], -fe,
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub struct PoseResolved {
    pub kind: &'static str,
    pub ortho: bool,
    pub size: (u32, u32),
    pub eye: V3,
    pub target: V3,
    pub up: V3,
    pub fov_deg: f64,
    /// Pixels per metre in the plane of the target.
    pub px_per_m: f64,
    pub world_to_px: [f64; 16],
    /// `[x0, y0, z0, x1, y1, z1]` of what the frame is built around.
    pub bounds_world: [f64; 6],
    /// Everything outside this box is cut away (room interiors, sections).
    pub section_box: Option<[f64; 6]>,
    pub level: Option<i64>,
    /// Area the image shows, `[x0, y0, x1, y1]`: plan metres for plan and top views, (distance
    /// along the line, height) for a section.
    pub region: Option<[f64; 4]>,
    /// Vertical shift of each level in an exploded view.
    pub level_dz: Vec<(i64, f64)>,
}

impl PoseResolved {
    /// Pixel, and depth along the view axis, of a world point; `None` behind the eye.
    pub fn project(&self, p: V3) -> Option<[f64; 3]> {
        let m = &self.world_to_px;
        let row = |i: usize| m[i * 4] * p[0] + m[i * 4 + 1] * p[1] + m[i * 4 + 2] * p[2] + m[i * 4 + 3];
        let w = row(3);
        if w <= 1e-9 {
            return None;
        }
        Some([row(0) / w, row(1) / w, row(2)])
    }

    pub fn in_frame(&self, px: f64, py: f64) -> bool {
        px >= 0.0 && py >= 0.0 && px <= self.size.0 as f64 && py <= self.size.1 as f64
    }

    /// The world point at a pixel and a depth along the view axis: the inverse of `project`.
    pub fn unproject(&self, px: f64, py: f64, depth: f64) -> V3 {
        let (r, u, f) = axes(self.eye, self.target, self.up);
        let (cx, cy) = (self.size.0 as f64 / 2.0, self.size.1 as f64 / 2.0);
        if self.ortho {
            let base = add(self.target, mul(f, depth - dot(f, sub(self.target, self.eye))));
            return add(
                base,
                add(mul(r, (px - cx) / self.px_per_m), mul(u, (cy - py) / self.px_per_m)),
            );
        }
        let fpx = cy / half_tan(self.fov_deg);
        let along = add(mul(r, (px - cx) * depth / fpx), mul(u, (cy - py) * depth / fpx));
        add(self.eye, add(along, mul(f, depth)))
    }

    /// The orbit camera of the same view: target, yaw, pitch (radians), distance.
    pub fn orbit(&self) -> (V3, f64, f64, f64) {
        let (r, _, _) = axes(self.eye, self.target, self.up);
        let d = sub(self.eye, self.target);
        let dist = dot(d, d).sqrt();
        let back = norm(d);
        (self.target, (-r[0]).atan2(r[1]), back[2].clamp(-1.0, 1.0).asin(), dist)
    }

    pub fn to_json(&self) -> J {
        let r = |v: f64| (v * 1e6).round() / 1e6;
        let v3 = |p: V3| json!([r(p[0]), r(p[1]), r(p[2])]);
        let six = |b: [f64; 6]| json!(b.map(r));
        json!({
            "kind": self.kind,
            "ortho": self.ortho,
            "size": [self.size.0, self.size.1],
            "eye": v3(self.eye),
            "target": v3(self.target),
            "up": v3(self.up),
            "fov_deg": r(self.fov_deg),
            "px_per_m": r(self.px_per_m),
            "world_to_px": self.world_to_px.map(r),
            "world_to_px_note": "row-major 4x4 on [x,y,z,1] (metres, x east, y north, z up): \
                px = m[0..4]·p / m[12..16]·p, py = m[4..8]·p / m[12..16]·p; origin top left",
            "bounds_world": six(self.bounds_world),
            "section_box": self.section_box.map(|b| six(b)),
            "level": self.level,
            "region_m": self.region.map(|g| g.map(r)),
            "level_dz": self.level_dz.iter().map(|(i, d)| json!({"level": i, "dz": r(*d)})).collect::<Vec<_>>(),
        })
    }
}

fn empty_target(what: &str) -> Refusal {
    Refusal::new(
        "empty_target",
        format!("nothing to frame: {what}"),
        "draw something there first, or name another target",
    )
}

fn finish(
    kind: &'static str,
    ortho: bool,
    size: (u32, u32),
    eye: V3,
    target: V3,
    up: V3,
    ppm: f64,
    bounds: [f64; 6],
) -> PoseResolved {
    let fpx = (size.1 as f64 / 2.0) / half_tan(FOV_DEG);
    let dist = dot(sub(eye, target), sub(eye, target)).sqrt();
    let ppm = if ortho { ppm } else { fpx / dist.max(1e-9) };
    PoseResolved {
        kind,
        ortho,
        size,
        eye,
        target,
        up,
        fov_deg: FOV_DEG,
        px_per_m: ppm,
        world_to_px: matrix(ortho, size, eye, target, up, FOV_DEG, ppm),
        bounds_world: bounds,
        section_box: None,
        level: None,
        region: None,
        level_dz: vec![],
    }
}

/// Distance of an orthographic eye whose frame is `h` pixels tall at `ppm`.
fn ortho_dist(h: u32, ppm: f64) -> f64 {
    (h as f64 / 2.0 / ppm) / half_tan(FOV_DEG)
}

/// Looking straight down at a plan rectangle, north up.
fn plan_view(
    kind: &'static str,
    size: (u32, u32),
    rect: [f64; 4],
    z: f64,
    bounds: [f64; 6],
) -> PoseResolved {
    let (dx, dy) = ((rect[2] - rect[0]).max(0.1), (rect[3] - rect[1]).max(0.1));
    let (w, h) = (size.0 as f64, size.1 as f64);
    let ppm = (w / (dx * (1.0 + MARGIN))).min(h / (dy * (1.0 + MARGIN)));
    let c = [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0, z];
    let eye = [c[0], c[1], z + ortho_dist(size.1, ppm)];
    let mut p = finish(kind, true, size, eye, c, [0.0, 1.0, 0.0], ppm, bounds);
    p.region = Some([
        c[0] - w / ppm / 2.0,
        c[1] - h / ppm / 2.0,
        c[0] + w / ppm / 2.0,
        c[1] + h / ppm / 2.0,
    ]);
    p
}

fn corners(b: [f64; 6]) -> Vec<V3> {
    let mut v = vec![];
    for x in [b[0], b[3]] {
        for y in [b[1], b[4]] {
            for z in [b[2], b[5]] {
                v.push([x, y, z]);
            }
        }
    }
    v
}

/// Orthographic view from a bearing and an elevation, the box centred and filling the frame.
fn ortho_from(
    kind: &'static str,
    size: (u32, u32),
    b: [f64; 6],
    bearing_deg: f64,
    pitch_deg: f64,
) -> PoseResolved {
    let o = outward(bearing_deg);
    let (cp, sp) = (pitch_deg.to_radians().cos(), pitch_deg.to_radians().sin());
    let back = [o[0] * cp, o[1] * cp, sp];
    let c = [(b[0] + b[3]) / 2.0, (b[1] + b[4]) / 2.0, (b[2] + b[5]) / 2.0];
    let up = [0.0, 0.0, 1.0];
    let (r, u, _) = axes(add(c, back), c, up);
    let (mut rx, mut uy) = ((f64::MAX, f64::MIN), (f64::MAX, f64::MIN));
    for p in corners(b) {
        let (a, v) = (dot(r, p), dot(u, p));
        rx = (rx.0.min(a), rx.1.max(a));
        uy = (uy.0.min(v), uy.1.max(v));
    }
    let (ex, ey) = ((rx.1 - rx.0).max(0.1), (uy.1 - uy.0).max(0.1));
    let ppm = (size.0 as f64 / (ex * (1.0 + MARGIN))).min(size.1 as f64 / (ey * (1.0 + MARGIN)));
    let eye = add(c, mul(back, ortho_dist(size.1, ppm)));
    finish(kind, true, size, eye, c, up, ppm, b)
}

/// Perspective distance at which a sphere fits the frame with the margin.
fn fit_dist(radius: f64, size: (u32, u32)) -> f64 {
    let half_v = (FOV_DEG.to_radians() / 2.0).min(1.5);
    let half_h = (half_v.tan() * size.0 as f64 / size.1 as f64).atan();
    radius.max(0.1) * (1.0 + MARGIN) / half_v.min(half_h).sin()
}

/// Radius of the sphere around `t` that holds the box.
fn radius_from(b: [f64; 6], t: V3) -> f64 {
    corners(b)
        .into_iter()
        .map(|q| dot(sub(q, t), sub(q, t)).sqrt())
        .fold(0.0, f64::max)
}

/// The perspective pose of an orbit camera (target, yaw and pitch in radians, distance): the
/// inverse of `PoseResolved::orbit`. `bounds` is what the frame is built around.
pub fn from_orbit(size: (u32, u32), target: V3, yaw: f64, pitch: f64, dist: f64, bounds: [f64; 6]) -> PoseResolved {
    let back = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()];
    let eye = add(target, mul(back, dist));
    finish("orbit", false, size, eye, target, [0.0, 0.0, 1.0], 0.0, bounds)
}

pub fn resolve(ed: &Editor, p: &Pose, size: (u32, u32)) -> Result<PoseResolved, Refusal> {
    resolve_in(&facts_of(ed), p, size)
}

pub fn resolve_in(f: &Facts, p: &Pose, size: (u32, u32)) -> Result<PoseResolved, Refusal> {
    let size = (size.0.clamp(16, 1280), size.1.clamp(16, 1280));
    let need_bounds = || f.bounds.ok_or_else(|| empty_target("the model has no elements"));
    match p {
        Pose::Current => Err(Refusal::new(
            "current_unresolved",
            "the window's camera is not known here",
            "ask for an explicit pose, for example {compass:\"SE\"} or {plan:<level>}",
        )),
        Pose::Plan { level, rect } => {
            let l = f
                .levels
                .iter()
                .find(|l| l.id == *level)
                .ok_or_else(|| unknown("level", &level.to_string(), "a level from list_levels".into()))?;
            let foot = rect
                .or(l.foot)
                .ok_or_else(|| empty_target(&format!("level {} has no elements; pass rect", l.name)))?;
            let b = [foot[0], foot[1], l.elev, foot[2], foot[3], l.elev + l.height];
            let mut r = plan_view("plan", size, foot, l.elev, b);
            r.level = Some(l.id);
            Ok(r)
        }
        Pose::Top => {
            let b = need_bounds()?;
            let mut r = plan_view("top", size, [b[0], b[1], b[3], b[4]], b[5], b);
            r.level = None;
            Ok(r)
        }
        Pose::Section { view_def } => {
            let v = f
                .views
                .iter()
                .find(|v| v.id == *view_def)
                .ok_or_else(|| unknown("section view", &view_def.to_string(), "a view from the project".into()))?;
            let b = need_bounds()?;
            let (dx, dy) = (v.b[0] - v.a[0], v.b[1] - v.a[1]);
            let len = dx.hypot(dy);
            if len < 1e-6 {
                return Err(empty_target("the section line has no length"));
            }
            // the view looks to the left of a -> b
            let n = [-dy / len, dx / len, 0.0];
            let zspan = (b[5] - b[2]).max(0.5);
            let ppm = (size.0 as f64 / (len * (1.0 + MARGIN))).min(size.1 as f64 / (zspan * (1.0 + MARGIN)));
            let c = [(v.a[0] + v.b[0]) / 2.0, (v.a[1] + v.b[1]) / 2.0, (b[2] + b[5]) / 2.0];
            let eye = sub(c, mul(n, ortho_dist(size.1, ppm)));
            let (x0, x1) = (
                v.a[0].min(v.b[0]).min(v.a[0] + n[0] * v.depth).min(v.b[0] + n[0] * v.depth),
                v.a[0].max(v.b[0]).max(v.a[0] + n[0] * v.depth).max(v.b[0] + n[0] * v.depth),
            );
            let (y0, y1) = (
                v.a[1].min(v.b[1]).min(v.a[1] + n[1] * v.depth).min(v.b[1] + n[1] * v.depth),
                v.a[1].max(v.b[1]).max(v.a[1] + n[1] * v.depth).max(v.b[1] + n[1] * v.depth),
            );
            let mut r = finish("section", true, size, eye, c, [0.0, 0.0, 1.0], ppm, b);
            r.section_box = Some([x0, y0, b[2], x1, y1, b[5]]);
            // the view's own coordinates: distance along the line from a, and height
            let (w, h, s) = (size.0 as f64, size.1 as f64, len / 2.0);
            r.region = Some([s - w / ppm / 2.0, c[2] - h / ppm / 2.0, s + w / ppm / 2.0, c[2] + h / ppm / 2.0]);
            Ok(r)
        }
        Pose::Compass { side, eye_h } => {
            let b = need_bounds()?;
            let o = outward(side.bearing());
            let c = [(b[0] + b[3]) / 2.0, (b[1] + b[4]) / 2.0, (b[2] + b[5]) / 2.0];
            let (target, back) = match eye_h {
                Some(h) => ([c[0], c[1], b[2] + h], [o[0], o[1], 0.0]),
                None => {
                    let (cp, sp) = (
                        COMPASS_PITCH_DEG.to_radians().cos(),
                        COMPASS_PITCH_DEG.to_radians().sin(),
                    );
                    (c, [o[0] * cp, o[1] * cp, sp])
                }
            };
            let dist = fit_dist(radius_from(b, target), size);
            let eye = add(target, mul(back, dist));
            Ok(finish("compass", false, size, eye, target, [0.0, 0.0, 1.0], 0.0, b))
        }
        Pose::Iso { az_deg } => Ok(ortho_from("iso", size, need_bounds()?, *az_deg, ISO_PITCH_DEG)),
        Pose::RoomInterior { room, cut } => {
            let r = f
                .rooms
                .iter()
                .find(|r| r.id == *room)
                .ok_or_else(|| unknown("room", &room.to_string(), "a room from query_elements".into()))?;
            let l = f
                .levels
                .iter()
                .find(|l| l.id == r.level)
                .ok_or_else(|| unknown("level", &r.level.to_string(), "a level from list_levels".into()))?;
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for p in &r.ring {
                x0 = x0.min(p[0]);
                y0 = y0.min(p[1]);
                x1 = x1.max(p[0]);
                y1 = y1.max(p[1]);
            }
            let cut_z = l.elev + cut * l.height;
            let c = centroid(&r.ring);
            let b = [x0, y0, l.elev, x1, y1, cut_z];
            let target = [c[0], c[1], (l.elev + cut_z) / 2.0];
            let dist = fit_dist(radius_from(b, target), size);
            // from the south-west, 45 degrees up: above the cut plane, looking down into the room
            let k = std::f64::consts::FRAC_1_SQRT_2;
            let back = [-0.5, -0.5, k];
            let eye = add(target, mul(back, dist));
            let mut p = finish("room", false, size, eye, target, [0.0, 0.0, 1.0], 0.0, b);
            // walls around the room stay in the cut: a metre beyond the room's extent
            p.section_box = Some([x0 - 1.0, y0 - 1.0, l.elev, x1 + 1.0, y1 + 1.0, cut_z]);
            p.level = Some(l.id);
            Ok(p)
        }
        Pose::Exploded { gap_m } => {
            let mut b = need_bounds()?;
            let dz: Vec<(i64, f64)> = f
                .levels
                .iter()
                .enumerate()
                .map(|(i, l)| (l.id, i as f64 * gap_m))
                .collect();
            b[5] += dz.last().map_or(0.0, |d| d.1);
            let mut r = ortho_from("exploded", size, b, Side8::SE.bearing(), ISO_PITCH_DEG);
            r.level_dz = dz;
            Ok(r)
        }
    }
}

// ---- highlight and labels -----------------------------------------------------------------

/// Reference text such as `wall:812` as a typed reference of a category the views can point at.
pub fn parse_highlight(refs: &[String], f: &Facts) -> Result<Vec<ElRef>, Refusal> {
    if refs.len() > MAX_HIGHLIGHT {
        return Err(Refusal::new(
            "too_many",
            format!("{} highlight references; the limit is {MAX_HIGHLIGHT}", refs.len()),
            "highlight fewer elements, or use a frozen set of the few that matter",
        ));
    }
    let mut out = vec![];
    for s in refs {
        let r = ElRef::parse(s).map_err(|e| {
            Refusal::new("bad_pose", e.0, "highlight takes references such as wall:812")
        })?;
        if point_of(f, &r).is_none() {
            return Err(unknown("element", s, "references from query_elements (wall, door, window, curtain, room)".into()));
        }
        out.push(r);
    }
    Ok(out)
}

fn group_of(cat: &str) -> Option<&'static str> {
    match cat {
        "wall" => Some("wall"),
        "door" | "window" | "opening" => Some("opening"),
        "room" => Some("room"),
        "curtain" | "curtain_wall" => Some("curtain"),
        _ => None,
    }
}

fn point_of<'a>(f: &'a Facts, r: &ElRef) -> Option<&'a PointFact> {
    let super::plan::IdOrNew::Id(id) = r.id else {
        return None;
    };
    let g = group_of(&r.cat)?;
    f.points.iter().find(|p| {
        p.kind == g && p.id == id && (r.cat == "opening" || g != "opening" || p.cat == r.cat)
    })
}

/// The editor selection of a reference, to colour its parts in a 3D image.
pub fn sel_of(r: &ElRef) -> Option<Sel> {
    let super::plan::IdOrNew::Id(id) = r.id else {
        return None;
    };
    match group_of(&r.cat)? {
        "wall" => Some(Sel::Wall(id)),
        "opening" => Some(Sel::Opening(id)),
        "room" => Some(Sel::Room(id)),
        _ => Some(Sel::Curtain(id)),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabelPx {
    pub reference: String,
    pub text: String,
    pub world: V3,
    pub px: f64,
    pub py: f64,
    pub depth: f64,
    pub in_frame: bool,
}

fn level_matches(f: &Facts, level: i64, tok: &str) -> bool {
    let tok = tok.trim();
    tok.parse::<i64>().is_ok_and(|n| n == level)
        || f.levels.iter().any(|l| l.id == level && l.name.eq_ignore_ascii_case(tok))
}

/// Points named by one label spec: `door:55`, `door:*`, `door:*@L3` (every door of a level), or
/// any `query_elements` text (resolved by `via_query`).
fn select_points<'a>(
    f: &'a Facts,
    spec: &str,
    via_query: &dyn Fn(&str) -> Result<Vec<Sel>, String>,
) -> Result<Vec<&'a PointFact>, Refusal> {
    let s = spec.trim();
    if let Some((cat, rest)) = s.split_once(':') {
        let cat = cat.trim();
        if let Some(g) = group_of(cat) {
            let (what, level) = match rest.split_once('@') {
                Some((w, l)) => (w.trim(), Some(l)),
                None => (rest.trim(), None),
            };
            let on_level = |p: &PointFact| level.map_or(true, |l| level_matches(f, p.level, l));
            let of_cat = |p: &PointFact| p.kind == g && (cat == "opening" || g != "opening" || p.cat == cat);
            if what == "*" {
                return Ok(f.points.iter().filter(|p| of_cat(p) && on_level(p)).collect());
            }
            if let Ok(id) = what.parse::<i64>() {
                let hit: Vec<&PointFact> = f.points.iter().filter(|p| of_cat(p) && p.id == id && on_level(p)).collect();
                if hit.is_empty() {
                    return Err(unknown("element", s, "references from query_elements".into()));
                }
                return Ok(hit);
            }
        }
    }
    let sels = via_query(s).map_err(|e| {
        Refusal::new("bad_pose", e, "labels take a reference, a category:*@level filter or query text")
    })?;
    let mut out = vec![];
    for sel in sels {
        let key = match sel {
            Sel::Wall(i) => ("wall", i),
            Sel::Opening(i) => ("opening", i),
            Sel::Curtain(i) => ("curtain", i),
            Sel::Room(i) => ("room", i),
            _ => continue,
        };
        if let Some(p) = f.points.iter().find(|p| p.kind == key.0 && p.id == key.1) {
            out.push(p);
        }
    }
    Ok(out)
}

/// Pixel places of the labelled elements, at most `MAX_LABELS`; the second value counts the
/// ones left out.
pub fn labels_in(
    f: &Facts,
    pr: &PoseResolved,
    specs: &[String],
    via_query: &dyn Fn(&str) -> Result<Vec<Sel>, String>,
) -> Result<(Vec<LabelPx>, usize), Refusal> {
    if specs.len() > MAX_LABELS {
        return Err(Refusal::new(
            "too_many",
            format!("{} label specs; the limit is {MAX_LABELS}", specs.len()),
            "merge them with a filter such as door:*@L3",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    let mut cut = 0;
    for s in specs {
        for p in select_points(f, s, via_query)? {
            if !seen.insert((p.kind, p.id)) {
                continue;
            }
            if out.len() == MAX_LABELS {
                cut += 1;
                continue;
            }
            let (px, py, depth, ok) = match pr.project(p.at) {
                Some([x, y, d]) => (x, y, d, true),
                None => (f64::NAN, f64::NAN, f64::NAN, false),
            };
            out.push(LabelPx {
                reference: format!("{}:{}", p.cat, p.id),
                text: p.text.clone(),
                world: p.at,
                px,
                py,
                depth,
                in_frame: ok && pr.in_frame(px, py),
            });
        }
    }
    Ok((out, cut))
}

pub fn labels_json(labels: &[LabelPx], cut: usize, hidden: &dyn Fn(&LabelPx) -> Option<bool>) -> J {
    let r2 = |v: f64| (v * 100.0).round() / 100.0;
    let rows: Vec<J> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut o = json!({
                "n": i + 1,
                "ref": l.reference,
                "text": l.text,
                "px": if l.px.is_finite() { json!(r2(l.px)) } else { J::Null },
                "py": if l.py.is_finite() { json!(r2(l.py)) } else { J::Null },
                "in_frame": l.in_frame,
            });
            if let Some(v) = hidden(l) {
                o["visible"] = json!(!v);
            }
            o
        })
        .collect();
    json!({ "items": rows, "cut": cut })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(id: i64, name: &str, elev: f64) -> LevelFact {
        LevelFact {
            id,
            name: name.into(),
            elev,
            height: 3.0,
            foot: Some([0.0, 0.0, 6.0, 4.0]),
        }
    }

    fn pt(kind: &'static str, cat: &'static str, id: i64, lv: i64, at: V3) -> PointFact {
        PointFact {
            kind,
            cat,
            id,
            level: lv,
            at,
            text: format!("{cat}:{id}"),
        }
    }

    /// A 6 x 4 m house of three storeys.
    fn house() -> Facts {
        let ring = vec![[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
        Facts {
            levels: vec![level(1, "L1", 0.0), level(2, "L2", 3.0), level(3, "L3", 6.0)],
            rooms: vec![
                RoomFact { id: 21, level: 2, number: "2.14".into(), name: "Hall".into(), ring: ring.clone() },
                RoomFact { id: 11, level: 1, number: "1.01".into(), name: "Kitchen".into(), ring },
            ],
            points: vec![
                pt("wall", "wall", 812, 1, [3.0, 0.0, 1.5]),
                pt("opening", "door", 5, 1, [1.0, 0.0, 1.0]),
                pt("opening", "door", 31, 3, [2.0, 0.0, 7.0]),
                pt("opening", "door", 32, 3, [4.0, 4.0, 7.0]),
                pt("opening", "window", 40, 3, [5.0, 0.0, 7.5]),
            ],
            views: vec![ViewFact { id: 7, name: "A-A".into(), a: [0.0, 2.0], b: [6.0, 2.0], depth: 2.0 }],
            bounds: Some([0.0, 0.0, 0.0, 6.0, 4.0, 9.0]),
        }
    }

    fn no_query(_: &str) -> Result<Vec<Sel>, String> {
        Err("no query text here".into())
    }

    fn parsed(f: &Facts, a: J) -> Pose {
        parse_pose(&a, f).unwrap()
    }

    #[test]
    fn pose_plan_exact() {
        let f = house();
        let size = (1000, 800);
        let p = resolve_in(&f, &parsed(&f, json!({"plan": "L1"})), size).unwrap();
        let ppm = (1000.0 / (6.0 * 1.05_f64)).min(800.0 / (4.0 * 1.05));
        let at = |x: f64, y: f64| (500.0 + (x - 3.0) * ppm, 400.0 - (y - 2.0) * ppm);
        for (x, y) in [(0.0, 0.0), (6.0, 4.0), (6.0, 0.0), (3.0, 2.0)] {
            let [px, py, _] = p.project([x, y, 0.0]).unwrap();
            let (ex, ey) = at(x, y);
            assert!((px - ex).abs() <= 0.5 && (py - ey).abs() <= 0.5, "({x},{y}): {px},{py} vs {ex},{ey}");
        }
        assert!((p.px_per_m - ppm).abs() < 1e-9);
        let [x0, y0, x1, y1] = p.region.unwrap();
        assert!(x0 < 0.0 && y0 < 0.0 && x1 > 6.0 && y1 > 4.0, "5 % margin around the target");
        // a rect is framed instead of the footprint
        let r = resolve_in(&f, &parsed(&f, json!({"plan": 1, "rect": [0, 0, 3, 2]})), size).unwrap();
        assert!(r.px_per_m > p.px_per_m * 1.9);
    }

    #[test]
    fn pose_room_cut_065() {
        let f = house();
        let p = resolve_in(&f, &parsed(&f, json!({"room": "2.14"})), (1000, 800)).unwrap();
        let b = p.section_box.unwrap();
        assert!((b[5] - (3.0 + 0.65 * 3.0)).abs() < 1e-12, "cut at 0.65 of the storey");
        assert!((b[2] - 3.0).abs() < 1e-12);
        assert!(p.eye[2] > b[5], "the eye sits above the cut plane");
        assert!(!p.ortho && p.level == Some(2));
        let [px, py, d] = p.project(p.target).unwrap();
        assert!((px - 500.0).abs() < 1e-6 && (py - 400.0).abs() < 1e-6 && d > 0.0);
        // every corner of the room at floor and cut height is in the frame
        for q in corners([0.0, 0.0, 3.0, 6.0, 4.0, b[5]]) {
            let [x, y, _] = p.project(q).unwrap();
            assert!(p.in_frame(x, y), "{q:?} -> {x},{y}");
        }
        let c = resolve_in(&f, &parsed(&f, json!({"room": 21, "cut": 0.5})), (800, 600)).unwrap();
        assert!((c.section_box.unwrap()[5] - 4.5).abs() < 1e-12);
        let e = parse_pose(&json!({"room": "9.99"}), &f).unwrap_err();
        assert_eq!(e.code, "unknown_target");
        assert!(parse_pose(&json!({"room": "2.14", "cut": 2.0}), &f).is_err());
    }

    #[test]
    fn pose_compass_8_sides() {
        let f = house();
        let mut eyes = vec![];
        for s in Side8::ALL {
            let pose = parsed(&f, json!({"compass": s.name()}));
            assert_eq!(pose, Pose::Compass { side: s, eye_h: None });
            let p = resolve_in(&f, &pose, (1000, 800)).unwrap();
            let d = sub(p.eye, p.target);
            let o = outward(s.bearing());
            let h = d[0].hypot(d[1]);
            assert!(((d[0] / h) - o[0]).abs() < 1e-9 && ((d[1] / h) - o[1]).abs() < 1e-9, "{s:?}");
            assert!(d[2] > 0.0, "elevated without an eye height");
            for q in corners(f.bounds.unwrap()) {
                let [x, y, _] = p.project(q).unwrap();
                assert!(p.in_frame(x, y), "{s:?} corner {q:?} -> {x},{y}");
            }
            eyes.push(p.eye.map(|v| (v * 1e6).round() as i64));
        }
        eyes.sort();
        eyes.dedup();
        assert_eq!(eyes.len(), 8, "eight different eyes");
        let h = resolve_in(&f, &parsed(&f, json!({"compass": "se", "eye_h": 1.6})), (1000, 800)).unwrap();
        assert!((h.eye[2] - 1.6).abs() < 1e-12 && (h.target[2] - 1.6).abs() < 1e-12);
        assert!(parse_pose(&json!({"compass": "up"}), &f).is_err());
        assert!(parse_pose(&json!({"compass": "N", "eye_h": 0.0}), &f).is_err());
    }

    #[test]
    fn pose_exploded_gaps() {
        let f = house();
        let p = resolve_in(&f, &parsed(&f, json!({"exploded": 2.0})), (1000, 800)).unwrap();
        assert_eq!(p.level_dz, vec![(1, 0.0), (2, 2.0), (3, 4.0)]);
        assert!((p.bounds_world[5] - (9.0 + 4.0)).abs() < 1e-12);
        let flat = resolve_in(&f, &parsed(&f, json!({"exploded": 0.0})), (1000, 800)).unwrap();
        assert_eq!(flat.bounds_world[5], 9.0);
        assert!(p.px_per_m < flat.px_per_m, "a taller stack is framed smaller");
        assert!(parse_pose(&json!({"exploded": -1.0}), &f).is_err());
        assert!(parse_pose(&json!({"exploded": 51.0}), &f).is_err());
    }

    #[test]
    fn labels_in_frame() {
        let f = house();
        let pose = parsed(&f, json!({"plan": "L3"}));
        let p = resolve_in(&f, &pose, (1000, 800)).unwrap();
        let specs = vec!["door:*@L3".to_string()];
        let (l, cut) = labels_in(&f, &p, &specs, &no_query).unwrap();
        assert_eq!((l.len(), cut), (2, 0), "only the doors of L3");
        assert!(l.iter().all(|x| x.in_frame && x.reference.starts_with("door:")));
        let ppm = p.px_per_m;
        let d31 = l.iter().find(|x| x.reference == "door:31").unwrap();
        assert!((d31.px - (500.0 + (2.0 - 3.0) * ppm)).abs() < 0.01);
        assert!((d31.py - (400.0 - (0.0 - 2.0) * ppm)).abs() < 0.01);
        // a rect that leaves the door out: the label is reported, outside the frame
        let r = resolve_in(&f, &parsed(&f, json!({"plan": "L3", "rect": [3, 1, 6, 4]})), (1000, 800)).unwrap();
        let (l, _) = labels_in(&f, &r, &["door:31".to_string()], &no_query).unwrap();
        assert!(!l[0].in_frame);
        // any door at all, by id or by level id
        let (l, _) = labels_in(&f, &p, &["opening:*@3".to_string(), "wall:812".to_string()], &no_query).unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!(
            labels_in(&f, &p, &["door:999".to_string()], &no_query).unwrap_err().code,
            "unknown_target"
        );
        assert!(labels_in(&f, &p, &vec!["door:*".to_string(); MAX_LABELS + 1], &no_query).is_err());
    }

    #[test]
    fn highlight_checks_the_references() {
        let f = house();
        let ok = parse_highlight(&["wall:812".to_string(), "door:5".to_string()], &f).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(sel_of(&ok[0]), Some(Sel::Wall(812)));
        assert_eq!(sel_of(&ok[1]), Some(Sel::Opening(5)));
        assert_eq!(parse_highlight(&["wall:1".to_string()], &f).unwrap_err().code, "unknown_target");
        assert_eq!(parse_highlight(&["window:5".to_string()], &f).unwrap_err().code, "unknown_target");
        let many = vec!["wall:812".to_string(); MAX_HIGHLIGHT + 1];
        assert_eq!(parse_highlight(&many, &f).unwrap_err().code, "too_many");
    }

    #[test]
    fn unknown_targets_are_refused_with_a_code() {
        let f = house();
        assert_eq!(parse_pose(&json!({"plan": "L9"}), &f).unwrap_err().code, "unknown_target");
        assert_eq!(parse_pose(&json!({"section": "Z"}), &f).unwrap_err().code, "unknown_target");
        assert_eq!(parse_pose(&json!({"plan": "L1", "top": true}), &f).unwrap_err().code, "bad_pose");
        assert_eq!(parse_pose(&json!({"plan": "L1", "cut": 0.5}), &f).unwrap_err().code, "bad_pose");
        assert_eq!(parse_pose(&json!({"zoom": 2}), &f).unwrap_err().code, "bad_pose");
        assert_eq!(parse_pose(&json!({}), &f).unwrap(), Pose::Current);
        let empty = Facts::default();
        let e = resolve_in(&empty, &Pose::Top, (800, 600)).unwrap_err();
        assert_eq!(e.code, "empty_target");
        assert_eq!(resolve_in(&f, &Pose::Current, (800, 600)).unwrap_err().code, "current_unresolved");
    }

    #[test]
    fn section_looks_left_of_the_line() {
        let f = house();
        let p = resolve_in(&f, &parsed(&f, json!({"section": "a-a"})), (1200, 800)).unwrap();
        // a -> b runs east, so the view looks north: the eye is south of the line
        assert!(p.eye[1] < 2.0 && (p.eye[0] - 3.0).abs() < 1e-9);
        let [x, _, d] = p.project([0.5, 3.0, 4.0]).unwrap();
        assert!(x < 600.0 && d > 0.0, "the west end is on the left of the image");
        let b = p.section_box.unwrap();
        assert_eq!((b[1], b[4]), (2.0, 4.0));
    }

    /// Deterministic points in and around the model.
    fn scatter(n: usize) -> Vec<V3> {
        let mut s = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        (0..n).map(|_| [next() * 20.0 - 5.0, next() * 20.0 - 5.0, next() * 12.0 - 1.0]).collect()
    }

    #[test]
    fn world_to_px_roundtrip() {
        let f = house();
        let poses = [
            json!({"plan": "L2"}),
            json!({"top": true}),
            json!({"section": 7}),
            json!({"compass": "SW"}),
            json!({"compass": "E", "eye_h": 1.6}),
            json!({"iso": 30.0}),
            json!({"room": "2.14"}),
            json!({"exploded": 1.5}),
        ];
        for a in poses {
            let p = resolve_in(&f, &parsed(&f, a.clone()), (1280, 800)).unwrap();
            for q in scatter(200) {
                let Some([px, py, d]) = p.project(q) else { continue };
                let b = p.unproject(px, py, d);
                for k in 0..3 {
                    assert!((b[k] - q[k]).abs() < 1e-9, "{a}: {q:?} -> {b:?}");
                }
            }
            // the orbit camera of the pose sees the same axes
            let (t, yaw, pitch, dist) = p.orbit();
            let back = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()];
            let e = add(t, mul(back, dist));
            let (r, _, _) = axes(p.eye, p.target, p.up);
            for k in 0..3 {
                assert!((e[k] - p.eye[k]).abs() < 1e-9, "{a}: orbit eye");
            }
            assert!((r[0] - (-yaw.sin())).abs() < 1e-9 && (r[1] - yaw.cos()).abs() < 1e-9, "{a}: orbit right axis");
        }
    }

    #[test]
    fn facts_come_from_the_model() {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::shot_scenes::build(&mut ed, "elems").unwrap();
        ed.reload().unwrap();
        let f = facts_of(&ed);
        assert!(!f.levels.is_empty() && f.bounds.is_some());
        assert!(f.points.iter().any(|p| p.kind == "wall"));
        let top = resolve_in(&f, &Pose::Top, (800, 600)).unwrap();
        let [x, y, _] = top.project(f.points[0].at).unwrap();
        assert!(top.in_frame(x, y));
    }
}
