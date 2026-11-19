//! Walls and rooms from an area program: two rows of rooms along a corridor.

use crate::layout::{mm, names, rect, Builder};
use crate::params::{bad, Args};
use crate::{variant_of, GenError, GenOut, Generator, ParamDef, ParamKind, WallRole};
use serde_json::Value as J;

const MAX_ROOMS: usize = 60;
const MAX_NAME: usize = 40;
const MIN_ROOM_W: f64 = 1.8;
const MIN_DEPTH: f64 = 2.4;
const MAX_DEPTH: f64 = 12.0;
const MAX_LEN: f64 = 300.0;

static PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: "rooms",
        kind: ParamKind::RoomList,
        min: 2.0,
        max: MAX_ROOMS as f64,
        default: 0.0,
        help: "List of {name, area in m2, window: true|false}",
    },
    ParamDef {
        name: "corridor",
        kind: ParamKind::Length,
        min: 1.2,
        max: 3.0,
        default: 1.6,
        help: "Corridor width, m",
    },
    ParamDef {
        name: "aspect",
        kind: ParamKind::Ratio,
        min: 1.0,
        max: 4.0,
        default: 2.0,
        help: "Length to depth of the block",
    },
    ParamDef {
        name: "height",
        kind: ParamKind::Length,
        min: 2.4,
        max: 4.5,
        default: 3.0,
        help: "Storey height, m",
    },
    ParamDef {
        name: "lang",
        kind: ParamKind::Choice(&["en", "ru"]),
        min: 0.0,
        max: 0.0,
        default: 0.0,
        help: "Language of the corridor name",
    },
];

static VARIANTS: [&str; 3] = [
    "aspect as requested, largest rooms first",
    "more compact block, smallest rooms first",
    "more elongated block, largest rooms first",
];

pub(crate) struct AreaProgram;

struct Want {
    name: String,
    area: f64,
    window: bool,
}

fn read_rooms(v: &J) -> Result<Vec<Want>, GenError> {
    let list = v.as_array().ok_or_else(|| bad("rooms", "must be a list"))?;
    if list.len() < 2 || list.len() > MAX_ROOMS {
        return Err(bad("rooms", format!("must hold 2..{MAX_ROOMS} rooms")));
    }
    let mut out = Vec::with_capacity(list.len());
    for r in list {
        let name = r["name"].as_str().unwrap_or("").trim();
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            return Err(bad(
                "rooms",
                format!("a room name must be 1..{MAX_NAME} plain characters"),
            ));
        }
        let area = r["area"]
            .as_f64()
            .filter(|a| a.is_finite() && (3.0..=500.0).contains(a));
        let Some(area) = area else {
            return Err(bad("rooms", format!("{name}: the area must be 3..500 m2")));
        };
        out.push(Want {
            name: name.to_string(),
            area,
            window: r["window"].as_bool().unwrap_or(true),
        });
    }
    Ok(out)
}

/// Row depth so the block has the wanted proportion: aspect·(2d + c)·d = A/2.
fn row_depth(total: f64, aspect: f64, c: f64) -> f64 {
    let disc = aspect * aspect * c * c + 4.0 * aspect * total;
    mm(((disc.sqrt() - aspect * c) / (4.0 * aspect)).clamp(MIN_DEPTH, MAX_DEPTH))
}

fn order(rooms: &[Want], smallest_first: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rooms.len()).collect();
    idx.sort_by(|&a, &b| {
        let by = rooms[a].area.total_cmp(&rooms[b].area);
        (if smallest_first { by } else { by.reverse() }).then(a.cmp(&b))
    });
    idx
}

/// Greedy balance: each room goes to the shorter row, kept in placing order.
fn split(rooms: &[Want], idx: &[usize]) -> [Vec<usize>; 2] {
    let mut rows = [vec![], vec![]];
    let mut sums = [0.0, 0.0];
    for &i in idx {
        let k = usize::from(sums[1] < sums[0]);
        rows[k].push(i);
        sums[k] += rooms[i].area;
    }
    rows
}

impl Generator for AreaProgram {
    fn id(&self) -> &'static str {
        "area_program"
    }
    fn params(&self) -> &'static [ParamDef] {
        &PARAMS
    }
    fn variants(&self) -> &'static [&'static str] {
        &VARIANTS
    }
    fn summary(&self) -> &'static str {
        "Walls and rooms from a list of room areas: two rows along a corridor, doors and windows."
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let a = Args::new(&PARAMS, params)?;
        let rooms = read_rooms(a.raw("rooms"))?;
        let c = a.num("corridor");
        let v = variant_of(seed);
        let aspect = a.num("aspect") * [1.0, 0.75, 1.35][v];
        let total: f64 = rooms.iter().map(|r| r.area).sum();
        let d = row_depth(total, aspect, c);
        let rows = split(&rooms, &order(&rooms, v == 1));

        let widths = |row: &[usize]| -> Vec<f64> {
            row.iter()
                .map(|&i| (rooms[i].area / d).max(MIN_ROOM_W))
                .collect()
        };
        let (w0, w1) = (widths(&rows[0]), widths(&rows[1]));
        let (s0, s1): (f64, f64) = (w0.iter().sum(), w1.iter().sum());
        let len = mm(s0.max(s1));
        if len > MAX_LEN {
            return Err(GenError::Infeasible(format!(
                "the program needs a block {len:.0} m long"
            )));
        }
        let y1 = d;
        let y2 = mm(d + c);
        let top = mm(y2 + d);
        let mut p = Builder::default();
        p.l.height = a.num("height");
        let south = p.wall((0.0, 0.0), (len, 0.0), WallRole::Exterior);
        p.wall((len, 0.0), (len, top), WallRole::Exterior);
        let north = p.wall((0.0, top), (len, top), WallRole::Exterior);
        p.wall((0.0, 0.0), (0.0, top), WallRole::Exterior);
        let cs = p.wall((0.0, y1), (len, y1), WallRole::Corridor);
        let cn = p.wall((0.0, y2), (len, y2), WallRole::Corridor);
        p.room(names(a.choice("lang")).corridor, None, 0.0, y1, len, y2);

        let mut worst: f64 = 0.0;
        let mut windows = 0u32;
        let spec = [
            (&rows[0], &w0, 0.0, y1, south, cs),
            (&rows[1], &w1, y2, top, north, cn),
        ];
        for (row, ws, ya, yb, facade, corridor) in spec {
            let stretch = len / ws.iter().sum::<f64>();
            let mut x = 0.0;
            for (k, &i) in row.iter().enumerate() {
                let x1 = if k + 1 == row.len() {
                    len
                } else {
                    mm(x + ws[k] * stretch)
                };
                if x1 < len {
                    p.wall((x1, ya), (x1, yb), WallRole::Partition);
                }
                let r = &rooms[i];
                p.room(&r.name, None, x, ya, x1, yb);
                let width = x1 - x;
                p.door(corridor, (x + x1) / 2.0, 0.8);
                if r.window {
                    p.window(facade, (x + x1) / 2.0, f64::min(1.5, width - 0.6));
                    windows += 1;
                }
                worst = worst.max((width * (yb - ya) - r.area).abs() / r.area);
                x = x1;
            }
        }
        p.l.slab = rect(0.0, 0.0, len, top);
        let floor = len * top;
        p.l.metrics = vec![
            ("program_area", total),
            ("floor_area", floor),
            ("corridor_share", len * (y2 - y1) / floor),
            ("area_deviation", worst),
            ("length", len),
            ("depth", top),
            ("windows", windows as f64),
        ];
        crate::check::validate_layout(&p.l).map_err(GenError::Infeasible)?;
        Ok(GenOut::Layout(p.l))
    }
}
