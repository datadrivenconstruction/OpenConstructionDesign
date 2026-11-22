// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One view over every run of the building services: water and power runs, sprinkler and
//! standpipe pipes, air ducts, each with its system key, its tier in the ceiling void and its
//! outer size.

use ddc_geom::mep::Domain;

use super::super::{AirSys, GProject, SvcSys};

/// Where a line comes from in the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Src {
    Water(usize),
    Elec(usize),
    Fire(usize),
    Stand(usize, usize),
    Duct(usize),
}

/// What a line is built of: sets the least straight length between fittings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Pipe,
    Duct,
    Tray,
    Conduit,
}

#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub src: Src,
    pub key: String,
    /// Place in the void from the top: gravity drain 0, ducts 1, pressure pipes 2, trays 3,
    /// conduits 4, sprinkler 5.
    pub tier: u8,
    pub kind: Kind,
    pub storey: usize,
    pub pts: Vec<(f64, f64, f64)>,
    /// Outer width and height.
    pub size: (f64, f64),
}

impl Line {
    /// Gravity drains and the sprinkler system keep their place.
    pub fn fixed(&self) -> bool {
        self.tier == 0 || self.tier == 5 || matches!(self.src, Src::Stand(..))
    }
}

fn water_key(s: SvcSys) -> (&'static str, u8) {
    match s {
        SvcSys::Sanitary => ("sanitary", 0),
        SvcSys::Cold => ("cold", 2),
        SvcSys::Hot => ("hot", 2),
        SvcSys::Fire => ("sprinkler", 5),
        SvcSys::Power => ("power", 3),
    }
}

pub(crate) fn duct_key(sys: AirSys, net: u8) -> String {
    let s = match sys {
        AirSys::Supply => "duct_supply",
        AirSys::Extract => "duct_extract",
    };
    if net == 0 {
        s.to_string()
    } else {
        format!("{s}_{net}")
    }
}

fn run_line(src: Src, r: &super::super::GRun) -> Line {
    let (w, h) = r.section.envelope().wh();
    let (key, tier, kind) = match (r.sys, r.section.domain) {
        (SvcSys::Power, Domain::CableTray) => ("power_tray".to_string(), 3, Kind::Tray),
        (SvcSys::Power, _) => ("power_conduit".to_string(), 4, Kind::Conduit),
        (s, _) => {
            let (k, t) = water_key(s);
            (k.to_string(), t, Kind::Pipe)
        }
    };
    Line {
        src,
        key,
        tier,
        kind,
        storey: r.storey,
        pts: r.points.clone(),
        size: (w, h),
    }
}

/// Every run of the project in a fixed order: water, power, sprinkler, standpipes, ducts.
pub(crate) fn lines(p: &GProject) -> Vec<Line> {
    let mut out = vec![];
    for (i, r) in p.services.water.iter().enumerate() {
        out.push(run_line(Src::Water(i), r));
    }
    for (i, r) in p.services.elec.iter().enumerate() {
        out.push(run_line(Src::Elec(i), r));
    }
    for (i, f) in p.fire.runs.iter().enumerate() {
        out.push(run_line(Src::Fire(i), &f.run));
    }
    for (s, sp) in p.fire.standpipes.iter().enumerate() {
        for (i, f) in sp.runs.iter().enumerate() {
            let mut l = run_line(Src::Stand(s, i), &f.run);
            l.key = "standpipe".into();
            out.push(l);
        }
    }
    for (i, d) in p.hvac.ducts.iter().enumerate() {
        let size = if d.size.1 > 0.0 {
            d.size
        } else {
            (d.size.0, d.size.0)
        };
        out.push(Line {
            src: Src::Duct(i),
            key: duct_key(d.sys, d.net),
            tier: 1,
            kind: Kind::Duct,
            storey: d.storey,
            pts: d.points.clone(),
            size,
        });
    }
    out
}

/// The axis points of a line in the project, to write a changed line back.
pub(crate) fn points_mut(p: &mut GProject, src: Src) -> &mut Vec<(f64, f64, f64)> {
    match src {
        Src::Water(i) => &mut p.services.water[i].points,
        Src::Elec(i) => &mut p.services.elec[i].points,
        Src::Fire(i) => &mut p.fire.runs[i].run.points,
        Src::Stand(s, i) => &mut p.fire.standpipes[s].runs[i].run.points,
        Src::Duct(i) => &mut p.hvac.ducts[i].points,
    }
}

/// Index of a line among the runs the void clash search numbers: water, power, sprinkler,
/// ducts; standpipes are not in it.
pub(crate) fn void_id(p: &GProject, src: Src) -> Option<usize> {
    let (w, e, f) = (
        p.services.water.len(),
        p.services.elec.len(),
        p.fire.runs.len(),
    );
    match src {
        Src::Water(i) => Some(i),
        Src::Elec(i) => Some(w + i),
        Src::Fire(i) => Some(w + e + i),
        Src::Stand(..) => None,
        Src::Duct(i) => Some(w + e + f + i),
    }
}
