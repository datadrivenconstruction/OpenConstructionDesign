// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Openings voiding a slab or roof (IfcRelVoidsElement): an opening body with vertical sides
//! becomes a plan hole of its host (slab profile hole, roof host opening). Anything else leaves
//! the host whole and is reported; nothing is dropped silently.

use ddc_geom::{poly, v2, V2};
use ddc_model::{Error, RoofKind, Snapshots};
use rusqlite::{params, Connection};

use super::{add, snap, Entity, PSlab, Plan, Rd, Why, V3};

/// Plan host of a deferred opening (index into `Plan::slabs` / `Plan::roofs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Host {
    Slab(usize),
    Roof(usize),
}

impl Rd<'_> {
    /// Positive length argument `i` of `e` in metres.
    fn opening_len(&self, e: &Entity, i: usize) -> Result<f64, Why> {
        Self::arg(e, i)
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v * self.scale)
            .ok_or("bad opening size")
    }

    /// Plan ring (world x, y, counter-clockwise) of an opening body with vertical sides: an
    /// extrusion straight up or down, or an upright block or bounding box.
    pub(super) fn opening_plan(&mut self, e: &Entity) -> Result<Vec<(f64, f64)>, Why> {
        let fr = self.product_frame(e)?;
        let items = self.body_items(e)?;
        let &[mut id] = items.as_slice() else {
            return Err(if items.is_empty() {
                "no body"
            } else {
                "unsupported body"
            });
        };
        // first operand of nested boolean results; the cap stops a cyclic file
        let mut depth = 0;
        while depth < 32
            && matches!(
                self.f.type_of(id),
                Some("IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT")
            )
        {
            id = Self::reference(self.ent(id, &[])?, 1)?;
            depth += 1;
        }
        let upright = |z: V3| z[2].abs() > 1.0 - 1e-6;
        let pts: Vec<V3> = match self.f.type_of(id) {
            Some("IFCEXTRUDEDAREASOLID") => {
                let x = self.ent(id, &[])?;
                let pos = fr.then(&self.axis_placement(Self::arg(x, 1))?);
                let d = pos.dir(self.direction(Self::arg(x, 2))?.ok_or("bad extrusion")?);
                // a profile plane standing on edge has no plan outline
                if !upright(d) || pos.z[2].abs() < 0.1 {
                    return Err("inclined opening");
                }
                let rings = self.profile_rings(Self::reference(x, 0)?)?;
                let [r] = rings.as_slice() else {
                    return Err("opening profile with inner loops");
                };
                r.iter().map(|p| pos.apply(*p)).collect()
            }
            Some("IFCBLOCK") => {
                let b = self.ent(id, &[])?;
                let pos = fr.then(&self.axis_placement(Self::arg(b, 0))?);
                if !upright(pos.z) {
                    return Err("inclined opening");
                }
                let (lx, ly) = (self.opening_len(b, 1)?, self.opening_len(b, 2)?);
                [[0.0, 0.0, 0.0], [lx, 0.0, 0.0], [lx, ly, 0.0], [0.0, ly, 0.0]]
                    .iter()
                    .map(|p| pos.apply(*p))
                    .collect()
            }
            Some("IFCBOUNDINGBOX") => {
                let b = self.ent(id, &[])?;
                if !upright(fr.z) {
                    return Err("inclined opening");
                }
                let c = self.point(Self::reference(b, 0)?)?;
                let (lx, ly) = (self.opening_len(b, 1)?, self.opening_len(b, 2)?);
                [[0.0, 0.0, 0.0], [lx, 0.0, 0.0], [lx, ly, 0.0], [0.0, ly, 0.0]]
                    .iter()
                    .map(|p| fr.apply(add(c, *p)))
                    .collect()
            }
            _ => return Err("unsupported opening body"),
        };
        let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1) <= 1e-6;
        let mut ring: Vec<(f64, f64)> = vec![];
        for p in pts {
            let q = (snap(p[0]), snap(p[1]));
            if ring.last().is_none_or(|l| !near(*l, q)) {
                ring.push(q);
            }
        }
        while ring.len() > 1 && near(ring[0], ring[ring.len() - 1]) {
            ring.pop();
        }
        if ring.len() < 3 {
            return Err("degenerate opening outline");
        }
        if poly::area(&to2(&ring)) < 0.0 {
            ring.reverse();
        }
        Ok(ring)
    }
}

fn to2(r: &[(f64, f64)]) -> Vec<V2> {
    r.iter().map(|p| v2(p.0, p.1)).collect()
}

fn back(r: &[V2]) -> Vec<(f64, f64)> {
    r.iter().map(|p| (p.x, p.y)).collect()
}

/// Same outline up to orientation and start vertex: equal bounding box and area.
fn same_ring(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    let bbox = |r: &[(f64, f64)]| {
        r.iter().fold(
            [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY],
            |m, p| [m[0].min(p.0), m[1].min(p.1), m[2].max(p.0), m[3].max(p.1)],
        )
    };
    let (ba, bb) = (bbox(a), bbox(b));
    let (aa, ab) = (poly::area(&to2(a)).abs(), poly::area(&to2(b)).abs());
    ba.iter().zip(bb).all(|(x, y)| (x - y).abs() <= 1e-4)
        && (aa - ab).abs() <= 1e-4 * aa.max(1.0)
}

/// Adds `ring` as a hole of slab `s`; a hole the profile already has counts as cut.
fn cut_slab(s: &mut PSlab, ring: &[(f64, f64)]) -> Result<(), Why> {
    use ddc_geom::ProfileError as E;
    let mut holes: Vec<Vec<V2>> = s.holes.iter().map(|h| to2(h)).collect();
    holes.push(to2(ring));
    match ddc_geom::Profile::new(to2(&s.outer), holes, ddc_geom::Tol::default()) {
        Ok(p) => {
            s.outer = back(&p.outer);
            s.holes = p.holes.iter().map(|h| back(h)).collect();
            Ok(())
        }
        Err(E::HolesOverlap { .. }) if s.holes.iter().any(|h| same_ring(h, ring)) => Ok(()),
        Err(E::HolesOverlap { .. }) => Err("overlaps another hole of the slab"),
        Err(E::HoleOutside { .. }) => Err("outside or across the slab edge"),
        Err(_) => Err("bad opening outline"),
    }
}

/// Queues `ring` as a host opening of roof `k` when it lies inside the roof outline.
fn roof_hole(pl: &mut Plan, k: usize, ring: &[(f64, f64)]) -> Result<(), Why> {
    let outline = match &pl.roofs[k].kind {
        RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => to2(ring),
        RoofKind::Extrusion { .. } => return Err("opening in an extruded roof"),
    };
    let rings = [outline];
    if !ring.iter().all(|p| poly::contains(&rings, v2(p.0, p.1))) {
        return Err("outside or across the roof edge");
    }
    if pl.roof_holes.iter().any(|(r, h)| *r == k && same_ring(h, ring)) {
        return Ok(());
    }
    pl.roof_holes.push((k, ring.to_vec()));
    Ok(())
}

/// Cuts opening `id` into the first of `hosts` (the voided element, then its parts) that takes
/// it; one that cannot be cut keeps the host whole and is reported.
pub(super) fn cut(rd: &mut Rd, pl: &mut Plan, id: u32, ty: &str, e: &Entity, hosts: &[Host]) {
    let ring = match rd.opening_plan(e) {
        Ok(r) => r,
        Err(why) => return not_cut(pl, id, ty, why),
    };
    let mut why = None;
    for h in hosts {
        let r = match *h {
            Host::Slab(k) => cut_slab(&mut pl.slabs[k], &ring),
            Host::Roof(k) => roof_hole(pl, k, &ring),
        };
        match r {
            Ok(()) => {
                pl.report.host_openings += 1;
                return;
            }
            Err(w) => {
                why.get_or_insert(w);
            }
        }
    }
    not_cut(pl, id, ty, why.unwrap_or("host slab or roof not imported"));
}

pub(super) fn not_cut(pl: &mut Plan, id: u32, ty: &str, why: Why) {
    pl.report.warnings.push(format!(
        "#{id}: opening not cut into its slab or roof ({why}); the host stays whole"
    ));
    pl.report.skip(ty, "slab or roof opening not cut");
}

/// Closed line sketch through `ring`.
fn ring_sketch(ring: &[(f64, f64)]) -> ddc_geom::sketch::Sketch {
    let p = to2(ring);
    ddc_geom::sketch::Sketch {
        curves: (0..p.len())
            .map(|i| ddc_geom::sketch::SketchCurve::Line {
                a: p[i],
                b: p[(i + 1) % p.len()],
            })
            .collect(),
    }
}

/// Marker (`prop`, kind `host_opening`) on openings this import made: a re-import may replace
/// them, while openings drawn by hand stay.
const MADE_KEY: &str = "ifc.roof_void";

/// Writes the host openings of roof row `roof` inside the import batch (undo removes them and
/// restores replaced ones). A re-import keeps an opening the roof already has and drops the
/// import-made ones whose outline is no longer in the source.
pub(super) fn commit_roof_holes<'r>(
    db: &Connection,
    snaps: &mut Snapshots,
    roof: i64,
    rings: impl Iterator<Item = &'r Vec<(f64, f64)>>,
) -> Result<(), Error> {
    let texts: Vec<String> = rings
        .map(|r| ddc_model::sketch_text(&ring_sketch(r)))
        .collect();
    let stale: Vec<i64> = {
        let mut st = db.prepare(
            "SELECT o.id, s.data FROM host_opening o JOIN prop p ON p.kind='host_opening'              AND p.ref=o.id AND p.key=?2 LEFT JOIN sketch s ON s.host_kind='host_opening'              AND s.host=o.id WHERE o.host_kind='roof' AND o.host=?1",
        )?;
        let rows = st.query_map(params![roof, MADE_KEY], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut v = vec![];
        for r in rows {
            let (id, data) = r?;
            if data.is_none_or(|d| !texts.contains(&d)) {
                v.push(id);
            }
        }
        v
    };
    for id in stale {
        snaps.save(db, "host_opening", "id", id)?;
        snaps.save(db, "sketch", "host", id)?;
        snaps.save(db, "prop", "ref", id)?;
        db.execute("DELETE FROM host_opening WHERE id=?1", [id])?;
        db.execute(
            "DELETE FROM sketch WHERE host_kind='host_opening' AND host=?1",
            [id],
        )?;
        db.execute(
            "DELETE FROM prop WHERE kind='host_opening' AND ref=?1",
            [id],
        )?;
    }
    for text in &texts {
        let have: i64 = db.query_row(
            "SELECT count(*) FROM host_opening o JOIN sketch s ON s.host_kind='host_opening'              AND s.host=o.id WHERE o.host_kind='roof' AND o.host=?1 AND s.data=?2",
            params![roof, text],
            |r| r.get(0),
        )?;
        if have > 0 {
            continue;
        }
        snaps.watermark(db, "host_opening")?;
        snaps.watermark(db, "sketch")?;
        snaps.watermark(db, "prop")?;
        db.execute(
            "INSERT INTO host_opening(host_kind, host, dormer) VALUES ('roof', ?1, 0)",
            [roof],
        )?;
        let oid = db.last_insert_rowid();
        db.execute(
            "INSERT OR REPLACE INTO sketch(host_kind, host, data) VALUES ('host_opening', ?1, ?2)",
            params![oid, text],
        )?;
        db.execute(
            "INSERT OR REPLACE INTO prop(kind, ref, key, value, unit)              VALUES ('host_opening', ?1, ?2, 1, '')",
            params![oid, MADE_KEY],
        )?;
    }
    Ok(())
}
