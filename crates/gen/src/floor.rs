//! Typical floor: a rectangle with a central corridor, apartments on both sides, a stair and
//! service core in the south block. Internal walls stagger so no wall crossing has four arms.

use crate::layout::{mm, names, rect, Builder, Names};
use crate::params::{bad, Args};
use crate::{
    variant_of, GLayout, GShaft, GStair, GenError, GenOut, Generator, ParamDef, ParamKind, WallRole,
};
use ddc_geom::stair::Shape;
use serde_json::Value as J;

const BATH_W: f64 = 1.8;
const BEDROOM_FROM: f64 = 6.4;
const STAGGER: f64 = 0.15;
const SHAFT_DEPTH: f64 = 0.8;
const SHAFT_GAP: f64 = 0.2;
const SHAFT_INSET: f64 = 0.25;
const SHAFT_ENTRY: f64 = 0.7;
const STAIR_MARGIN: f64 = 0.3;
const STAIR_ENTRY: f64 = 0.9;
const STAIR_HOLE_GAP: f64 = 0.05;
const CORNER_WEIGHT: f64 = 1.35;

static PARAMS: [ParamDef; 12] = [
    ParamDef {
        name: "apartments",
        kind: ParamKind::Count,
        min: 2.0,
        max: 40.0,
        default: 8.0,
        help: "Apartments on the floor",
    },
    ParamDef {
        name: "depth",
        kind: ParamKind::Length,
        min: 12.0,
        max: 24.0,
        default: 16.0,
        help: "Building depth, m",
    },
    ParamDef {
        name: "length",
        kind: ParamKind::Length,
        min: 0.0,
        max: 200.0,
        default: 0.0,
        help:
            "Building length, m: 0 or at least 12; 0 derives it from the area or the apartment area",
    },
    ParamDef {
        name: "area",
        kind: ParamKind::Area,
        min: 0.0,
        max: 6000.0,
        default: 0.0,
        help: "Floor area, m2: 0 or at least 100; length wins over area",
    },
    ParamDef {
        name: "apartment_area",
        kind: ParamKind::Area,
        min: 25.0,
        max: 200.0,
        default: 55.0,
        help: "Target apartment area, m2, used when neither length nor area is given",
    },
    ParamDef {
        name: "min_width",
        kind: ParamKind::Length,
        min: 4.2,
        max: 12.0,
        default: 4.2,
        help: "Smallest apartment width along the corridor, m",
    },
    ParamDef {
        name: "core",
        kind: ParamKind::Choice(&["end", "center"]),
        min: 0.0,
        max: 0.0,
        default: 1.0,
        help: "Where the stair and service core stands",
    },
    ParamDef {
        name: "corridor",
        kind: ParamKind::Length,
        min: 1.4,
        max: 2.4,
        default: 1.6,
        help: "Corridor width, m",
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
        name: "shafts",
        kind: ParamKind::Count,
        min: 1.0,
        max: 4.0,
        default: 2.0,
        help: "Service shafts in the core",
    },
    ParamDef {
        name: "shaft_width",
        kind: ParamKind::Length,
        min: 1.2,
        max: 2.4,
        default: 1.5,
        help: "Shaft width, m",
    },
    ParamDef {
        name: "lang",
        kind: ParamKind::Choice(&["en", "ru"]),
        min: 0.0,
        max: 0.0,
        default: 0.0,
        help: "Language of room names",
    },
];

static VARIANTS: [&str; 3] = [
    "core as requested, equal apartments",
    "core moved to the other position",
    "core as requested, larger apartments at the corners",
];

pub(crate) struct TypicalFloor;

impl Generator for TypicalFloor {
    fn id(&self) -> &'static str {
        "typical_floor"
    }
    fn params(&self) -> &'static [ParamDef] {
        &PARAMS
    }
    fn variants(&self) -> &'static [&'static str] {
        &VARIANTS
    }
    fn summary(&self) -> &'static str {
        "Typical floor: N apartments along a central corridor, a stair and service core, shafts."
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let a = Args::new(&PARAMS, params)?;
        let s = Spec::read(&a)?;
        let v = variant_of(seed);
        let core = core_pos(a.choice("core"), v, s.n);
        let out = s.layout(core, v == 2)?;
        crate::check::validate_layout(&out).map_err(GenError::Infeasible)?;
        Ok(GenOut::Layout(out))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Core {
    West,
    Center,
    East,
}

/// A central core makes three blocks, so it needs three apartments.
fn core_pos(asked: &str, variant: usize, n: usize) -> Core {
    let center_ok = n >= 3;
    let base = if asked == "center" && center_ok {
        Core::Center
    } else {
        Core::West
    };
    if variant != 1 {
        return base;
    }
    match base {
        Core::Center => Core::West,
        _ if center_ok => Core::Center,
        _ => Core::East,
    }
}

struct Spec {
    n: usize,
    depth: f64,
    length: f64,
    area: f64,
    apartment_area: f64,
    min_w: f64,
    corridor: f64,
    height: f64,
    shafts: usize,
    shaft_w: f64,
    lang: &'static str,
}

impl Spec {
    fn read(a: &Args<'_>) -> Result<Self, GenError> {
        let length = a.num("length");
        if length > 0.0 && length < 12.0 {
            return Err(bad("length", "must be 0 or at least 12"));
        }
        let area = a.num("area");
        if area > 0.0 && area < 100.0 {
            return Err(bad("area", "must be 0 or at least 100"));
        }
        Ok(Spec {
            n: a.count("apartments"),
            depth: mm(a.num("depth")),
            length: mm(length),
            area,
            apartment_area: a.num("apartment_area"),
            min_w: a.num("min_width"),
            corridor: a.num("corridor"),
            height: a.num("height"),
            shafts: a.count("shafts"),
            shaft_w: a.num("shaft_width"),
            lang: a.choice("lang"),
        })
    }

    /// The stair at the origin going south, and its plan box.
    fn stair_box(&self) -> Result<((f64, f64), (f64, f64), GStair), GenError> {
        let g = GStair {
            origin: (0.0, 0.0),
            angle: -std::f64::consts::FRAC_PI_2,
            shape: Shape::U {
                left: true,
                first: None,
                gap: 0.1,
            },
            width: 1.2,
            max_riser: 0.18,
            landing: 1.2,
            height: self.height,
        };
        let st = g
            .def()
            .layout()
            .map_err(|e| GenError::Infeasible(format!("stair: {e:?}")))?;
        let mut lo = (f64::MAX, f64::MAX);
        let mut hi = (f64::MIN, f64::MIN);
        let rings = st
            .flights
            .iter()
            .map(|f| f.ring(st.tread))
            .chain(st.landings.iter().map(|l| l.ring.clone()));
        for r in rings {
            for p in r {
                lo = (lo.0.min(p.x), lo.1.min(p.y));
                hi = (hi.0.max(p.x), hi.1.max(p.y));
            }
        }
        Ok((lo, hi, g))
    }

    /// One length for every core position, so variants compare the same building.
    fn derive_length(&self, core_w: f64) -> Result<f64, GenError> {
        if self.length > 0.0 {
            return Ok(self.length);
        }
        if self.area > 0.0 {
            let l = self.area / self.depth;
            if !(12.0..=200.0).contains(&l) {
                return Err(GenError::Infeasible(format!(
                    "area {} at depth {} gives length {l:.1}",
                    self.area, self.depth
                )));
            }
            return Ok(mm(l));
        }
        let block = (self.depth - self.corridor) / 2.0;
        let w = (self.apartment_area / block).max(self.min_w);
        let start = ((self.n as f64 * w + core_w) / 2.0)
            .max(core_w + self.min_w)
            .max(12.0);
        let mut l = (start * 10.0).ceil() / 10.0;
        while self.capacity(l, core_w) < self.n {
            l += 0.5;
            if l > 200.0 {
                return Err(GenError::Infeasible(format!(
                    "{} apartments need more than 200 m of length",
                    self.n
                )));
            }
        }
        Ok(mm(l))
    }

    /// Whole apartments a length holds, whichever way the core stands.
    fn capacity(&self, len: f64, core_w: f64) -> usize {
        let fit = |x: f64| (x.max(0.0) / self.min_w).floor() as usize;
        let at_end = fit(len) + fit(len - core_w);
        if self.n < 3 {
            return at_end;
        }
        let side = (len - core_w) / 2.0;
        let centered = if side < self.min_w {
            at_end
        } else {
            fit(len) + 2 * fit(side)
        };
        at_end.min(centered)
    }

    fn layout(&self, core: Core, corners: bool) -> Result<GLayout, GenError> {
        let d = self.depth;
        let y1 = mm((d - self.corridor) / 2.0);
        let y2 = mm(y1 + self.corridor);
        let c = y2 - y1;
        let (lo, hi, stair0) = self.stair_box()?;
        let (st_w, st_h) = (hi.0 - lo.0, hi.1 - lo.1);
        if st_h + STAIR_MARGIN + STAIR_ENTRY > y1 {
            return Err(GenError::Infeasible(format!(
                "a stair {st_h:.2} m long does not fit a block {y1:.2} m deep"
            )));
        }
        let shafts_h = self.shafts as f64 * SHAFT_DEPTH + (self.shafts - 1) as f64 * SHAFT_GAP;
        if SHAFT_INSET + shafts_h + SHAFT_ENTRY > y1 {
            return Err(GenError::Infeasible(format!(
                "{} shafts do not fit a block {y1:.2} m deep",
                self.shafts
            )));
        }
        let stair_room = mm(st_w + 2.0 * STAIR_MARGIN);
        let service = mm(self.shaft_w + 2.0 * SHAFT_INSET);
        let core_w = mm(stair_room + service);
        let len = self.derive_length(core_w)?;
        if len < core_w {
            return Err(GenError::Infeasible(format!(
                "length {len} is shorter than the core {core_w:.2}"
            )));
        }
        let mut xc = match core {
            Core::West => 0.0,
            Core::East => mm(len - core_w),
            Core::Center => mm((len - core_w) / 2.0),
        };
        // a stub too short for one apartment goes to the core
        if core == Core::Center && xc < self.min_w {
            xc = 0.0;
        }
        if xc > 0.0 && xc < self.min_w {
            return Err(GenError::Infeasible(format!(
                "length {len} leaves {xc:.2} m beside the core"
            )));
        }
        let mut core_end = mm(xc + core_w);
        if len - core_end < self.min_w {
            core_end = len;
        }

        let nm = names(self.lang);
        let mut p = Builder::default();
        p.l.height = self.height;
        let south = p.wall((0.0, 0.0), (len, 0.0), WallRole::Exterior);
        let east = p.wall((len, 0.0), (len, d), WallRole::Exterior);
        let north = p.wall((0.0, d), (len, d), WallRole::Exterior);
        let west = p.wall((0.0, 0.0), (0.0, d), WallRole::Exterior);
        let cs = p.wall((0.0, y1), (len, y1), WallRole::Corridor);
        let cn = p.wall((0.0, y2), (len, y2), WallRole::Corridor);
        p.room(nm.corridor, None, 0.0, y1, len, y2);

        let south_row = Row {
            corridor: cs,
            facade: south,
            y0: y1,
            sign: -1.0,
            depth: y1,
        };
        let north_row = Row {
            corridor: cn,
            facade: north,
            y0: y2,
            sign: 1.0,
            depth: mm(d - y2),
        };

        // core: walls only where the outline is not already a wall
        let x_stair = mm(xc + stair_room);
        for x in [xc, x_stair, core_end] {
            if x > 0.0 && x < len {
                p.wall((x, 0.0), (x, y1), WallRole::Partition);
            }
        }
        p.room(nm.stair, None, xc, 0.0, x_stair, y1);
        p.room(nm.service, None, x_stair, 0.0, core_end, y1);
        let stair_door = (xc + x_stair) / 2.0;
        p.door(cs, stair_door, 1.0);
        p.door(cs, (x_stair + core_end) / 2.0, 0.8);

        let dx = stair_door - (lo.0 + hi.0) / 2.0;
        let dy = STAIR_MARGIN - lo.1;
        p.l.stairs.push(GStair {
            origin: (mm(dx), mm(dy)),
            ..stair0
        });
        let g = STAIR_HOLE_GAP;
        // shafts cut the slabs themselves; only the stair needs an opening
        p.l.slab = rect(0.0, 0.0, len, d);
        p.l.slab_holes = vec![rect(
            lo.0 + dx - g,
            lo.1 + dy - g,
            hi.0 + dx + g,
            hi.1 + dy + g,
        )];
        let sx0 = x_stair + SHAFT_INSET;
        for k in 0..self.shafts {
            let y = SHAFT_INSET + k as f64 * (SHAFT_DEPTH + SHAFT_GAP);
            p.l.shafts.push(GShaft {
                kind: crate::ShaftKind::Lift,
                ring: rect(sx0, y, sx0 + self.shaft_w, y + SHAFT_DEPTH),
            });
        }

        let mut segs = vec![Seg {
            x0: 0.0,
            x1: len,
            row: &north_row,
            n: 1,
        }];
        if xc > 0.0 {
            segs.push(Seg {
                x0: 0.0,
                x1: xc,
                row: &south_row,
                n: 1,
            });
        }
        if core_end < len {
            segs.push(Seg {
                x0: core_end,
                x1: len,
                row: &south_row,
                n: 1,
            });
        }
        allocate(&mut segs, self.n, self.min_w)?;
        let strip = (0.4 * south_row.depth.min(north_row.depth)).clamp(1.8, 2.6);
        let cx = Flats {
            names: &nm,
            strip,
            west,
            east,
            len,
            min_w: self.min_w,
            corners,
        };
        let mut number = 0u32;
        let mut useful = 0.0;
        let mut windows = 0u32;
        let mut far: f64 = 0.0;
        for s in &segs {
            for (k, (x0, x1)) in cx.widths(s).into_iter().enumerate() {
                number += 1;
                let entrance = cx.flat(&mut p, s.row, number, k, x0, x1, &mut windows);
                useful += (x1 - x0) * s.row.depth;
                let across = if s.row.corridor == cs { 0.0 } else { c };
                far = far.max((entrance - stair_door).abs() + across);
                if k + 1 < s.n {
                    p.wall(
                        (x1, s.row.y(0.0)),
                        (x1, s.row.y(s.row.depth)),
                        WallRole::Party,
                    );
                }
            }
        }
        let floor = len * d;
        p.l.metrics = vec![
            ("apartments", number as f64),
            ("floor_area", floor),
            ("useful_area", useful),
            ("corridor_share", len * c / floor),
            ("stair_distance_max", far),
            ("windows", windows as f64),
            ("length", len),
        ];
        Ok(p.l)
    }
}

struct Row {
    corridor: usize,
    facade: usize,
    y0: f64,
    sign: f64,
    depth: f64,
}

impl Row {
    fn y(&self, v: f64) -> f64 {
        mm(self.y0 + self.sign * v)
    }
}

struct Seg<'a> {
    x0: f64,
    x1: f64,
    row: &'a Row,
    n: usize,
}

/// Every block takes one apartment; the rest go one by one to where apartments are widest.
fn allocate(segs: &mut [Seg<'_>], n: usize, min_w: f64) -> Result<(), GenError> {
    let mut left = n.checked_sub(segs.len()).ok_or_else(|| {
        GenError::Infeasible(format!("{n} apartments cannot fill {} blocks", segs.len()))
    })?;
    if segs.iter().any(|s| s.x1 - s.x0 < min_w) {
        return Err(GenError::Infeasible(format!(
            "a block is narrower than one apartment ({min_w} m)"
        )));
    }
    while left > 0 {
        let mut best: Option<(usize, f64)> = None;
        for (i, s) in segs.iter().enumerate() {
            let w = (s.x1 - s.x0) / (s.n + 1) as f64;
            if w >= min_w && best.is_none_or(|b| w > b.1) {
                best = Some((i, w));
            }
        }
        let Some((i, _)) = best else {
            return Err(GenError::Infeasible(format!(
                "{left} more apartments do not fit at width {min_w} m"
            )));
        };
        segs[i].n += 1;
        left -= 1;
    }
    Ok(())
}

struct Flats<'a> {
    names: &'a Names,
    strip: f64,
    west: usize,
    east: usize,
    len: f64,
    min_w: f64,
    corners: bool,
}

impl Flats<'_> {
    fn widths(&self, s: &Seg<'_>) -> Vec<(f64, f64)> {
        let seg_len = s.x1 - s.x0;
        let weight = |k: usize| -> f64 {
            let corner = (k == 0 && s.x0 <= 0.0) || (k + 1 == s.n && s.x1 >= self.len);
            if self.corners && corner && s.n > 1 {
                CORNER_WEIGHT
            } else {
                1.0
            }
        };
        let total: f64 = (0..s.n).map(weight).sum();
        let narrowest = (0..s.n)
            .map(|k| weight(k) / total * seg_len)
            .fold(f64::MAX, f64::min);
        // rounding to the millimetre must not push an apartment under the minimum
        let use_weights = narrowest >= self.min_w + 0.01;
        let mut out = Vec::with_capacity(s.n);
        let mut x = s.x0;
        for k in 0..s.n {
            let w = if use_weights {
                weight(k) / total * seg_len
            } else {
                seg_len / s.n as f64
            };
            let x1 = if k + 1 == s.n { s.x1 } else { mm(x + w) };
            out.push((x, x1));
            x = x1;
        }
        out
    }

    /// Returns the x of the entrance door. `k` staggers the strip against the next apartment.
    #[allow(clippy::too_many_arguments)]
    fn flat(
        &self,
        p: &mut Builder,
        row: &Row,
        number: u32,
        k: usize,
        x0: f64,
        x1: f64,
        windows: &mut u32,
    ) -> f64 {
        let nm = self.names;
        let s = mm(self.strip + if k % 2 == 0 { STAGGER } else { -STAGGER });
        let hx = mm(x0 + BATH_W);
        let w = x1 - x0;
        let strip = p.wall((x0, row.y(s)), (x1, row.y(s)), WallRole::Partition);
        let bath = p.wall((hx, row.y(0.0)), (hx, row.y(s)), WallRole::Partition);
        p.room(nm.bath, Some(number), x0, row.y(0.0), hx, row.y(s));
        p.room(nm.hall, Some(number), hx, row.y(0.0), x1, row.y(s));
        let entrance = (hx + x1) / 2.0;
        p.door(row.corridor, entrance, 0.9);
        p.door(bath, s / 2.0, 0.7);
        let top = row.y(row.depth);
        let mut spans = vec![];
        if w >= BEDROOM_FROM {
            let xm = mm(x1 - f64::max(2.6, w * 0.42));
            p.wall((xm, row.y(s)), (xm, top), WallRole::Partition);
            p.room(nm.living, Some(number), x0, row.y(s), xm, top);
            p.room(nm.bedroom, Some(number), xm, row.y(s), x1, top);
            p.door(strip, (hx + xm) / 2.0 - x0, 0.8);
            p.door(strip, (xm + x1) / 2.0 - x0, 0.8);
            spans.push((x0, xm));
            spans.push((xm, x1));
        } else {
            p.room(nm.living, Some(number), x0, row.y(s), x1, top);
            p.door(strip, (hx + x1) / 2.0 - x0, 0.8);
            spans.push((x0, x1));
        }
        for (a, b) in spans {
            p.window(row.facade, (a + b) / 2.0, 1.5);
            *windows += 1;
        }
        let mid = (row.y(s) + top) / 2.0;
        if x0 <= 0.0 {
            p.window(self.west, mid, 1.2);
            *windows += 1;
        }
        if x1 >= self.len {
            p.window(self.east, mid, 1.2);
            *windows += 1;
        }
        entrance
    }
}
