// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shared helpers of the plan generators: a layout builder and room names.

use crate::{GLayout, GOpening, GRoom, GWall, WallRole};

pub(crate) const DOOR_H: f64 = 2.1;
pub(crate) const WIN_H: f64 = 1.5;
pub(crate) const WIN_SILL: f64 = 0.9;

#[derive(Default)]
pub(crate) struct Builder {
    pub l: GLayout,
}

impl Builder {
    pub fn wall(&mut self, a: (f64, f64), b: (f64, f64), role: WallRole) -> usize {
        self.l.walls.push(GWall::line(a, b, role));
        self.l.walls.len() - 1
    }

    /// `center` is measured along the wall from its start.
    pub fn door(&mut self, wall: usize, center: f64, width: f64) {
        self.l.openings.push(GOpening {
            wall,
            along: center - width / 2.0,
            door: true,
            width,
            height: DOOR_H,
            sill: 0.0,
            ty: None,
        });
    }

    pub fn window(&mut self, wall: usize, center: f64, width: f64) {
        self.l.openings.push(GOpening {
            wall,
            along: center - width / 2.0,
            door: false,
            width,
            height: WIN_H,
            sill: WIN_SILL,
            ty: None,
        });
    }

    pub fn room(&mut self, name: &str, apartment: Option<u32>, x0: f64, y0: f64, x1: f64, y1: f64) {
        let (xa, xb) = (x0.min(x1), x0.max(x1));
        let (ya, yb) = (y0.min(y1), y0.max(y1));
        self.l.rooms.push(GRoom {
            name: name.to_string(),
            apartment,
            point: ((xa + xb) / 2.0, (ya + yb) / 2.0),
            polygon: vec![(xa, ya), (xb, ya), (xb, yb), (xa, yb)],
        });
    }
}

// The millimetre grid keeps shared coordinates exactly equal on both sides of a wall.
pub(crate) use ddc_geom::snap_mm as mm;

pub(crate) fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

pub(crate) struct Names {
    pub living: &'static str,
    pub bedroom: &'static str,
    pub bath: &'static str,
    pub hall: &'static str,
    pub corridor: &'static str,
    pub stair: &'static str,
    pub service: &'static str,
}

/// Room names of the generators, from the one table of room kinds.
pub(crate) fn names(lang: &str) -> Names {
    use crate::project::RoomKind as K;
    let n = |k| crate::project::names::kind_name(k, lang);
    Names {
        living: n(K::Living),
        bedroom: n(K::Bedroom),
        bath: n(K::Bath),
        hall: n(K::Hall),
        corridor: n(K::Corridor),
        stair: n(K::Stair),
        service: n(K::Technical),
    }
}
