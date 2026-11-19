// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parts of a connection as real solids (a frame plus a `gsolid::Form`): plates with holes,
//! bolt sets (head, washers, shank, nut), anchor rods, fillet welds, balls, sleeves and cones.
//! Small circles are polygons (shank 12, holes and washers 16 sides): the view tessellation is
//! too coarse for them, and the schedule takes sizes from the catalogue, not from volumes.
//! Material text carries tags the realism checks read: `S355;plate`, `8.8;bolt=M20;hole=22`.

use std::f64::consts::PI;

use super::catalog::{catalog, fmt_mm, BoltSize};
use crate::arc::BulgeRing;
use crate::gsolid::{Form, Frame, Section};
use crate::math::{v2, v3, V2, V3};
use crate::poly;

pub const SHANK_SIDES: usize = 12;
pub const HOLE_SIDES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Plate,
    Stiffener,
    Gusset,
    Cleat,
    Packer,
    Bolt,
    Head,
    Nut,
    Washer,
    Anchor,
    Weld,
    Ball,
    Sleeve,
    Cone,
}

impl Role {
    pub fn key(self) -> &'static str {
        match self {
            Role::Plate => "plate",
            Role::Stiffener => "stiffener",
            Role::Gusset => "gusset",
            Role::Cleat => "cleat",
            Role::Packer => "packer",
            Role::Bolt => "bolt",
            Role::Head => "head",
            Role::Nut => "nut",
            Role::Washer => "washer",
            Role::Anchor => "anchor",
            Role::Weld => "weld",
            Role::Ball => "ball",
            Role::Sleeve => "sleeve",
            Role::Cone => "cone",
        }
    }

    /// Plate-like parts: their outline and holes are checked.
    pub fn is_plate(self) -> bool {
        matches!(
            self,
            Role::Plate | Role::Stiffener | Role::Gusset | Role::Cleat | Role::Packer
        )
    }

    /// Head, nut and washers: counted with their bolt.
    pub fn is_fastener_piece(self) -> bool {
        matches!(self, Role::Head | Role::Nut | Role::Washer)
    }
}

/// What the schedule needs of a part.
#[derive(Clone, Debug, PartialEq)]
pub enum PartQty {
    /// Outline (frame xy, counter-clockwise), holes (centre, diameter), thickness.
    Plate {
        t: f64,
        outline: Vec<V2>,
        holes: Vec<(V2, f64)>,
    },
    /// One bolt (or anchor rod) set; the shank part carries it.
    Bolt {
        d: f64,
        class: String,
        length: f64,
        anchor: bool,
    },
    Weld {
        leg: f64,
        length: f64,
    },
    /// Ball, sleeve, cone, fastener pieces: volume only.
    Solid {
        volume: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub role: Role,
    pub frame: Frame,
    pub form: Form,
    pub material: String,
    /// `PL 12x150x260`, `M20x60 8.8`, `a5 L=210`.
    pub mark: String,
    pub qty: PartQty,
}

/// A bolt as the checks see it: axis from the head bearing face through the grip.
#[derive(Clone, Debug, PartialEq)]
pub struct BoltInfo {
    pub o: V3,
    pub dir: V3,
    pub grip: f64,
    pub size: BoltSize,
    pub class: String,
    pub length: f64,
    pub hole: f64,
    pub anchor: bool,
    /// Part indices of the plates the bolt passes through.
    pub plies: Vec<usize>,
}

impl BoltInfo {
    /// Head side and nut side envelopes along the axis: (from, to, radius) in axis distance.
    pub fn envelopes(&self) -> [(f64, f64, f64); 3] {
        let b = &self.size;
        let r = b.hex_r().max(b.w_od / 2.0);
        if self.anchor {
            let top = self.grip + b.plate_washer_t;
            return [
                (top, top + b.m, r),
                (-self.length + self.grip, top + b.m, b.d / 2.0),
                // square washer turned square to the plate: its inscribed circle
                (top - b.plate_washer_t, top, b.plate_washer / 2.0),
            ];
        }
        [
            (-(b.k + b.w_t), 0.0, r),
            (-b.w_t, self.length - b.w_t, b.d / 2.0),
            (self.grip, self.grip + b.w_t + b.m, r),
        ]
    }
}

/// Orthonormal frame with `z` along `zdir` and `x` as close to `xhint` as it can be.
pub fn frame_from(o: V3, zdir: V3, xhint: V3) -> Frame {
    let z = zdir.normalized();
    let mut x = xhint - z * z.dot(xhint);
    if x.len() < 1e-9 {
        let alt = if z.x.abs() < 0.9 {
            v3(1.0, 0.0, 0.0)
        } else {
            v3(0.0, 1.0, 0.0)
        };
        x = alt - z * z.dot(alt);
    }
    let x = x.normalized();
    Frame {
        o,
        x,
        y: z.cross(x),
        z,
    }
}

/// Regular polygon, counter-clockwise, first corner at angle `phase`.
pub fn ngon(c: V2, r: f64, n: usize, phase: f64) -> Vec<V2> {
    (0..n)
        .map(|i| {
            let a = phase + 2.0 * PI * i as f64 / n as f64;
            c + v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

fn ccw(mut r: Vec<V2>) -> Vec<V2> {
    if poly::area(&r) < 0.0 {
        r.reverse();
    }
    r
}

fn cw(mut r: Vec<V2>) -> Vec<V2> {
    if poly::area(&r) > 0.0 {
        r.reverse();
    }
    r
}

/// Section with polygon holes.
pub fn section(outer: Vec<V2>, holes: Vec<Vec<V2>>) -> Section {
    Section {
        outer: BulgeRing::from_polygon(ccw(outer)),
        holes: holes
            .into_iter()
            .map(|h| BulgeRing::from_polygon(cw(h)))
            .collect(),
    }
}

/// Prism of `profile` from `z0` to `z1` along the frame z.
pub fn prism(profile: Section, z0: f64, z1: f64) -> Form {
    Form::Extrusion {
        profile,
        start: z0,
        end: z1,
    }
}

/// Bounding box sides of a 2D outline (along x, along y).
pub fn extent(r: &[V2]) -> (f64, f64) {
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in r {
        x0 = x0.min(p.x);
        x1 = x1.max(p.x);
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    (x1 - x0, y1 - y0)
}

/// `PL 12x150x260` (thickness x shorter x longer side, mm).
pub fn plate_mark(t: f64, outline: &[V2]) -> String {
    let (a, b) = extent(outline);
    format!("PL {}x{}x{}", fmt_mm(t), fmt_mm(a.min(b)), fmt_mm(a.max(b)))
}

/// A plate: `outline` in the frame xy, holes as (centre, diameter), body from `z0` to `z0 + t`.
pub fn plate(
    role: Role,
    frame: Frame,
    outline: Vec<V2>,
    holes: Vec<(V2, f64)>,
    t: f64,
    z0: f64,
) -> Part {
    let outline = ccw(outline);
    let rings: Vec<Vec<V2>> = holes
        .iter()
        .map(|(c, d)| ngon(*c, d / 2.0, HOLE_SIDES, 0.0))
        .collect();
    let grade = &catalog().plate.grade;
    Part {
        role,
        frame,
        form: prism(section(outline.clone(), rings), z0, z0 + t),
        material: format!("{grade};plate"),
        mark: plate_mark(t, &outline),
        qty: PartQty::Plate { t, outline, holes },
    }
}

fn piece(role: Role, frame: Frame, form: Form, material: String, mark: String) -> Part {
    let volume = crate::gsolid::quantities(&form).map_or(0.0, |q| q.volume);
    Part {
        role,
        frame,
        form,
        material,
        mark,
        qty: PartQty::Solid { volume },
    }
}

/// Bolt set: head on the `o` side (bearing face at `o`), shank along `dir` through `grip`,
/// washer and nut beyond. `xhint` turns the hexagons.
pub fn bolt_set(
    size: &BoltSize,
    class: &str,
    o: V3,
    dir: V3,
    grip: f64,
    hole: f64,
    xhint: V3,
) -> Result<(Vec<Part>, BoltInfo), String> {
    let cat = catalog();
    let need = cat.bolt_need(size, grip);
    let length = cat.bolt_length(need).ok_or_else(|| {
        format!(
            "no stock length for {} with grip {} mm",
            size.name(),
            fmt_mm(grip)
        )
    })?;
    let f = frame_from(o, dir, xhint);
    let hex = |h0: f64, h1: f64| {
        prism(
            section(ngon(v2(0.0, 0.0), size.hex_r(), 6, 0.0), vec![]),
            h0,
            h1,
        )
    };
    let washer = |h0: f64| {
        let s = section(
            ngon(v2(0.0, 0.0), size.w_od / 2.0, HOLE_SIDES, 0.0),
            vec![ngon(v2(0.0, 0.0), size.w_id / 2.0, HOLE_SIDES, 0.0)],
        );
        prism(s, h0, h0 + size.w_t)
    };
    let name = size.name();
    let fast = |what: &str| format!("{class};{what}={name};fastener");
    let shank0 = -size.w_t;
    let parts = vec![
        piece(
            Role::Head,
            f,
            hex(-(size.w_t + size.k), -size.w_t),
            fast("head"),
            name.clone(),
        ),
        piece(
            Role::Washer,
            f,
            washer(-size.w_t),
            fast("washer"),
            name.clone(),
        ),
        Part {
            role: Role::Bolt,
            frame: f,
            form: prism(
                section(ngon(v2(0.0, 0.0), size.d / 2.0, SHANK_SIDES, 0.0), vec![]),
                shank0,
                shank0 + length,
            ),
            material: format!("{class};bolt={name};hole={}", fmt_mm(hole)),
            mark: format!("{name}x{} {class}", fmt_mm(length)),
            qty: PartQty::Bolt {
                d: size.d,
                class: class.to_string(),
                length,
                anchor: false,
            },
        },
        piece(Role::Washer, f, washer(grip), fast("washer"), name.clone()),
        piece(
            Role::Nut,
            f,
            hex(grip + size.w_t, grip + size.w_t + size.m),
            fast("nut"),
            name.clone(),
        ),
    ];
    let info = BoltInfo {
        o,
        dir: f.z,
        grip,
        size: size.clone(),
        class: class.to_string(),
        length,
        hole,
        anchor: false,
        plies: vec![],
    };
    Ok((parts, info))
}

/// Anchor rod standing up through a base plate: `top` is the plate top at the rod axis, `grip`
/// the plate thickness. Plate washer and nut above, an anchor plate at the rod foot.
pub fn anchor_set(
    size: &BoltSize,
    class: &str,
    top: V3,
    grip: f64,
    embed: f64,
    xhint: V3,
) -> (Vec<Part>, BoltInfo) {
    let cat = catalog();
    let up = v3(0.0, 0.0, 1.0);
    let f = frame_from(top, up, xhint);
    let name = size.name();
    let above = size.plate_washer_t + size.m + cat.conn.anchor_above * size.pitch;
    let z0 = -grip - embed;
    let length = above + grip + embed;
    let sq = |a: f64| {
        let h = a / 2.0;
        vec![v2(-h, -h), v2(h, -h), v2(h, h), v2(-h, h)]
    };
    let hole_ring = ngon(v2(0.0, 0.0), size.d / 2.0, HOLE_SIDES, 0.0);
    let pw = prism(
        section(sq(size.plate_washer), vec![hole_ring.clone()]),
        0.0,
        size.plate_washer_t,
    );
    let foot = prism(
        section(sq(size.plate_washer), vec![hole_ring]),
        z0,
        z0 + size.plate_washer_t,
    );
    let hex = |h0: f64, h1: f64| {
        prism(
            section(ngon(v2(0.0, 0.0), size.hex_r(), 6, 0.0), vec![]),
            h0,
            h1,
        )
    };
    let fast = |what: &str| format!("{class};{what}={name};fastener");
    let parts = vec![
        Part {
            role: Role::Anchor,
            frame: f,
            form: prism(
                section(ngon(v2(0.0, 0.0), size.d / 2.0, SHANK_SIDES, 0.0), vec![]),
                z0,
                above,
            ),
            material: format!(
                "{class};bolt={name};hole={};anchor",
                fmt_mm(size.anchor_hole)
            ),
            mark: format!("{name}x{} {class}", fmt_mm(length)),
            qty: PartQty::Bolt {
                d: size.d,
                class: class.to_string(),
                length,
                anchor: true,
            },
        },
        piece(
            Role::Washer,
            f,
            pw,
            fast("washer"),
            format!("{name} plate washer"),
        ),
        piece(
            Role::Nut,
            f,
            hex(size.plate_washer_t, size.plate_washer_t + size.m),
            fast("nut"),
            name.clone(),
        ),
        piece(
            Role::Washer,
            f,
            foot,
            fast("washer"),
            format!("{name} anchor plate"),
        ),
    ];
    let info = BoltInfo {
        o: top - up * grip,
        dir: up,
        grip,
        size: size.clone(),
        class: class.to_string(),
        length,
        hole: size.anchor_hole,
        anchor: true,
        plies: vec![],
    };
    (parts, info)
}

/// Fillet weld along the root line from `c` in `dir` for `len`; legs run along the unit
/// directions `a` and `b` (each across `dir`, one on each joined face).
pub fn fillet(c: V3, dir: V3, len: f64, a: V3, b: V3, leg: f64) -> Part {
    let f = frame_from(c, dir, a);
    let bl = v2(b.dot(f.x), b.dot(f.y));
    let bl = bl * (1.0 / bl.len().max(1e-12));
    let tri = vec![v2(0.0, 0.0), v2(leg, 0.0), bl * leg];
    Part {
        role: Role::Weld,
        frame: f,
        form: prism(section(tri, vec![]), 0.0, len),
        material: format!("weld;leg={}", fmt_mm(leg)),
        mark: format!("a{} L={}", fmt_mm(leg), fmt_mm(len)),
        qty: PartQty::Weld { leg, length: len },
    }
}

/// Body of revolution around the frame z from a profile in (radius, z), as a prism stack
/// would be too coarse: the profile turns a full circle.
pub fn revolve(role: Role, frame: Frame, rz: Vec<V2>, material: String, mark: String) -> Part {
    // profile in the frame xz plane: local x = radius, local y = z
    let f = Frame {
        o: frame.o,
        x: frame.x,
        y: frame.z,
        z: -frame.y,
    };
    let form = Form::Revolve {
        profile: section(rz, vec![]),
        axis_o: v2(0.0, 0.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: 2.0 * PI,
    };
    piece(role, f, form, material, mark)
}

/// Blend between two coaxial polygons (a cone of `n` sides) from `z0` to `z1` along the frame z.
pub fn cone(
    frame: Frame,
    r0: f64,
    r1: f64,
    z0: f64,
    z1: f64,
    material: String,
    mark: String,
) -> Part {
    let f = Frame {
        o: frame.at(v3(0.0, 0.0, z0)),
        ..frame
    };
    let n = 24;
    let form = Form::Blend {
        bottom: BulgeRing::from_polygon(ngon(v2(0.0, 0.0), r0, n, 0.0)),
        top: BulgeRing::from_polygon(ngon(v2(0.0, 0.0), r1, n, 0.0)),
        height: z1 - z0,
    };
    piece(Role::Cone, f, form, material, mark)
}

/// Hexagonal prism along the frame z (sleeve of a ball node).
pub fn hex_prism(
    role: Role,
    frame: Frame,
    s: f64,
    z0: f64,
    z1: f64,
    hole: f64,
    material: String,
    mark: String,
) -> Part {
    let holes = if hole > 0.0 {
        vec![ngon(v2(0.0, 0.0), hole / 2.0, HOLE_SIDES, 0.0)]
    } else {
        vec![]
    };
    let form = prism(
        section(ngon(v2(0.0, 0.0), s / 3f64.sqrt(), 6, 0.0), holes),
        z0,
        z1,
    );
    piece(role, frame, form, material, mark)
}
