// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model walls -> IFC4X3_ADD2. Walls are placed in their own frame (origin = axis start,
//! x along the axis, y to the left), so the layer-set usage direction is the local +Y.

use std::collections::BTreeMap;
use std::path::Path;

use ddc_geom::arc::{edge_mid, ArcAxis, BulgeRing, CHORD_SAG};
#[cfg(test)]
use ddc_geom::layered::{layer_footprints, LayerPieces};
use ddc_geom::layered::{Layer, Strip};
use ddc_geom::wall_lean::Lean;
use ddc_geom::{v2, Tol, V2};
use ddc_model::{Boundary, Model};

use super::step::{list, r, real, reals, text, Step};

mod adv;
pub(super) mod comp_types;
mod ext_walls;
mod extra;
mod freeform;
mod shell_elem;
mod guids;
mod ifc2x3;
mod matdef;
mod mep;
mod panel_system;
mod post;
mod projinfo;
mod ramps;
mod rebar;
mod roof_edge;
pub(super) mod sink;
mod solids;
mod spline_walls;
mod stairs;
mod structure;
mod topo;
mod userprops;

const NONE: &str = "$";
/// Opening body overshoots both wall faces so boolean cuts are not coplanar.
const OVERCUT: f64 = 0.05;

fn db<T>(e: rusqlite::Error) -> Result<T, String> {
    Err(format!("db: {e}"))
}

struct WallRow {
    id: i64,
    wtype: i64,
    a: V2,
    b: V2,
    base_offset: f64,
    height: f64,
    offset0: Option<f64>,
    bulge: f64,
    /// Top edge (u along the axis, height above the base); None — flat at `height`.
    top: Option<Vec<(f64, f64)>>,
    /// Top of the core layers (roof structure underside); None - as `top`.
    top_core: Option<Vec<(f64, f64)>>,
    /// Bottom edge (u, rise above the base) when attached to a slab; None — flat.
    base: Option<Vec<(f64, f64)>>,
    lean: Lean,
}

struct Ctx {
    s: Step,
    origin: u32,
    dir_z: u32,
    dir_x: u32,
    /// -X direction, created on the first door that opens to the right of its wall.
    dir_nx: Option<u32>,
    /// IfcRoof by roof id: hosts of skylight openings.
    roof_ent: BTreeMap<i64, u32>,
    /// IfcMaterial by name: written once, shared by every element that names it.
    mat_ids: BTreeMap<String, u32>,
    g: guids::Guids,
    /// (`prop` table, row) -> exported product, for the property sets of `userprops`.
    prod: BTreeMap<(String, i64), u32>,
    /// Material specs by name, and the IfcMaterial written for each (one per name).
    mat_spec: BTreeMap<String, ddc_model::MaterialSpec>,
    mat_ent: BTreeMap<String, u32>,
    /// Context of the material style representations.
    mat_ctx: u32,
}

impl Ctx {
    fn point3(&mut self, x: f64, y: f64, z: f64) -> u32 {
        self.s.add("IFCCARTESIANPOINT", &[reals(&[x, y, z])])
    }
    fn point2(&mut self, x: f64, y: f64) -> u32 {
        self.s.add("IFCCARTESIANPOINT", &[reals(&[x, y])])
    }
    fn axis3(&mut self, p: u32, x_dir: Option<u32>) -> u32 {
        let xd = x_dir.map_or(NONE.to_string(), r);
        self.s
            .add("IFCAXIS2PLACEMENT3D", &[r(p), r(self.dir_z), xd])
    }
    fn placement(&mut self, rel: Option<u32>, axis: u32) -> u32 {
        let rel = rel.map_or(NONE.to_string(), r);
        self.s.add("IFCLOCALPLACEMENT", &[rel, r(axis)])
    }
    fn rooted(&mut self, name: &str, kind: &str, id: i64, label: &str, rest: &[String]) -> u32 {
        let g = self.g.get(kind, id);
        let mut a = vec![g, NONE.into(), text(label), NONE.into()];
        a.extend_from_slice(rest);
        let ent = self.s.add(name, &a);
        let product = !name.starts_with("IFCREL")
            && !matches!(name, "IFCPROPERTYSET" | "IFCELEMENTQUANTITY");
        // grid rows are axes, the IfcGrid is per level: no 1:1 link
        if let Some(k) = guids::ext_kind(kind).filter(|k| product && *k != "grid") {
            self.prod.entry((k.to_string(), id)).or_insert(ent);
        }
        ent
    }
    fn rel(&mut self, name: &str, kind: &str, id: i64, rest: &[String]) -> u32 {
        self.rooted(name, kind, id, "", rest)
    }
}

type Quad = ([(f64, f64); 2], [(f64, f64); 2]);

/// Body footprints in the wall frame. One merged ring when every layer is a single piece;
/// otherwise one ring per layer piece (exact, from the layer priority cuts).
#[cfg(test)]
fn outlines(
    s: &Strip,
    joined: &[Vec<LayerPieces>],
    idx: usize,
) -> Result<Vec<Vec<(f64, f64)>>, String> {
    let fp =
        layer_footprints(s, &joined[idx], idx, Tol::default()).map_err(|e| format!("{e:?}"))?;
    let fp: Vec<Vec<Vec<V2>>> = fp
        .into_iter()
        .map(|l| l.into_iter().map(|p| p.outer).collect())
        .collect();
    outlines_of(s, &fp)
}

/// Like `outlines`, from ready plans of layer pieces `[layer][piece]`.
fn outlines_of(s: &Strip, fp: &[Vec<Vec<V2>>]) -> Result<Vec<Vec<(f64, f64)>>, String> {
    let single = fp.iter().all(|l| l.len() == 1);
    let (d, n) = (s.dir(), s.normal());
    let loc = |p: V2| {
        (
            (p.x - s.a.x) * d.x + (p.y - s.a.y) * d.y,
            (p.x - s.a.x) * n.x + (p.y - s.a.y) * n.y,
        )
    };
    if !single {
        let mut out = vec![];
        for piece in fp.iter().flatten() {
            let mut ring: Vec<(f64, f64)> = piece.iter().map(|p| loc(*p)).collect();
            let area: f64 = (0..ring.len())
                .map(|i| {
                    let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
                    p.0 * q.1 - q.0 * p.1
                })
                .sum();
            if area < 0.0 {
                ring.reverse();
            }
            out.push(ring);
        }
        return Ok(out);
    }
    // per layer: lower edge [start, end], upper edge [start, end]
    let mut quads: Vec<Quad> = vec![];
    for l in fp {
        let mut q: Vec<(f64, f64)> = l[0].iter().map(|p| loc(*p)).collect();
        if q.len() != 4 {
            return Err("layer footprint is not a quadrilateral".into());
        }
        q.sort_by(|a, b| a.1.total_cmp(&b.1));
        let (mut lo, mut hi) = ([q[0], q[1]], [q[2], q[3]]);
        lo.sort_by(|a, b| a.0.total_cmp(&b.0));
        hi.sort_by(|a, b| a.0.total_cmp(&b.0));
        quads.push((lo, hi));
    }
    let mut ring = vec![];
    for (lo, hi) in &quads {
        ring.push(lo[1]);
        ring.push(hi[1]);
    }
    for (lo, hi) in quads.iter().rev() {
        ring.push(hi[0]);
        ring.push(lo[0]);
    }
    Ok(vec![simplify(ring)])
}

/// Removes repeated and collinear vertices.
fn simplify(mut ring: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    loop {
        let n = ring.len();
        let mut drop = None;
        for i in 0..n {
            let (p, c, q) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
            let cross = (c.0 - p.0) * (q.1 - c.1) - (c.1 - p.1) * (q.0 - c.0);
            if near(p, c) || cross.abs() < 1e-12 {
                drop = Some(i);
                break;
            }
        }
        match drop {
            Some(i) if n > 3 => {
                ring.remove(i);
            }
            _ => return ring,
        }
    }
}

/// Frame (jambs, head, window sill rail) and panel as boxes in the opening frame
/// (origin = bottom centre of the opening, x along the wall, y through it).
fn fill_body(c: &mut Ctx, door: bool, w: f64, h: f64, depth: f64) -> Vec<u32> {
    let fw = 0.05_f64.min(w / 4.0).min(h / 4.0);
    let pt = 0.04_f64.min(depth);
    let mut b = vec![
        (fw, depth, h, -w / 2.0 + fw / 2.0, 0.0),
        (fw, depth, h, w / 2.0 - fw / 2.0, 0.0),
        (w - 2.0 * fw, depth, fw, 0.0, h - fw),
    ];
    let (pz, ph) = if door {
        (0.0, h - fw)
    } else {
        (fw, h - 2.0 * fw)
    };
    if !door {
        b.push((w - 2.0 * fw, depth, fw, 0.0, 0.0));
    }
    b.push((w - 2.0 * fw, pt, ph, 0.0, pz));
    b.into_iter()
        .map(|(dx, dy, dz, x, z)| {
            let p2 = c.point2(0.0, 0.0);
            let pos2 = c.s.add("IFCAXIS2PLACEMENT2D", &[r(p2), s(NONE)]);
            let rect = c.s.add(
                "IFCRECTANGLEPROFILEDEF",
                &[s(".AREA."), s(NONE), r(pos2), real(dx), real(dy)],
            );
            let o = c.point3(x, 0.0, z);
            let ax = c.axis3(o, None);
            c.s.add(
                "IFCEXTRUDEDAREASOLID",
                &[r(rect), r(ax), r(c.dir_z), real(dz)],
            )
        })
        .collect()
}

fn s(x: &str) -> String {
    x.to_string()
}

pub fn export_ifc(model: &Model) -> Result<String, String> {
    export_ifc_with_warnings(model).map(|(t, _)| t)
}

/// As `export_ifc`; also the GlobalId conflicts that were resolved by generating a new id.
pub fn export_ifc_with_warnings(model: &Model) -> Result<(String, Vec<String>), String> {
    let (step, g) = run(model, Step::default())?;
    let text = step.finish("model.ifc");
    // Every real written passes through here: NaN/inf must never reach a file.
    crate::ifc::check_step(&text)?;
    Ok((text, g.warnings))
}

pub use post::{IfcOptions, IfcSchema};

/// Export for exchange: schema from `opts` (IFC4 by default; IFC2X3 and IFC4X3 on request),
/// element types, common property sets, header time stamp. Same model and options (with a
/// fixed time stamp) give identical bytes.
pub fn export_ifc_with(
    model: &Model,
    opts: &IfcOptions,
) -> Result<(String, Vec<String>), String> {
    let ((text, mut warnings), note) = adv::with_options(opts, || export_ifc_with_warnings(model))?;
    warnings.extend(note);
    let (out, more) = post::apply(&text, opts, "model.ifc", &post::project_salt(model))?;
    crate::ifc::check_step(&out)?;
    warnings.extend(more);
    Ok((out, warnings))
}

/// The whole model through `step`, kept in memory or streaming to a sink.
fn run(model: &Model, step: Step) -> Result<(Step, guids::Guids), String> {
    let sql = model.sql();
    let tol = Tol::default();
    let mut c = Ctx {
        s: step,
        origin: 0,
        dir_z: 0,
        dir_x: 0,
        dir_nx: None,
        roof_ent: BTreeMap::new(),
        mat_ids: BTreeMap::new(),
        g: guids::Guids::load(model)?,
        prod: BTreeMap::new(),
        mat_spec: matdef::load(model)?,
        mat_ent: BTreeMap::new(),
        mat_ctx: 0,
    };

    // --- context, units, project
    c.origin = c.point3(0.0, 0.0, 0.0);
    c.dir_z = c.s.add("IFCDIRECTION", &[reals(&[0.0, 0.0, 1.0])]);
    c.dir_x = c.s.add("IFCDIRECTION", &[reals(&[1.0, 0.0, 0.0])]);
    let world = c.axis3(c.origin, Some(c.dir_x));
    let north = topo::true_north(&mut c, model);
    let ctx = c.s.add(
        "IFCGEOMETRICREPRESENTATIONCONTEXT",
        &[s(NONE), text("Model"), s("3"), s("1.E-6"), r(world), north],
    );
    let sub = |c: &mut Ctx, id: &str, view: &str| {
        c.s.add(
            "IFCGEOMETRICREPRESENTATIONSUBCONTEXT",
            &[
                text(id),
                text("Model"),
                s("*"),
                s("*"),
                s("*"),
                s("*"),
                r(ctx),
                s(NONE),
                s(view),
                s(NONE),
            ],
        )
    };
    let body_ctx = sub(&mut c, "Body", ".MODEL_VIEW.");
    c.mat_ctx = body_ctx;
    let axis_ctx = sub(&mut c, "Axis", ".GRAPH_VIEW.");
    let unit = |c: &mut Ctx, t: &str, n: &str| c.s.add("IFCSIUNIT", &[s("*"), s(t), s(NONE), s(n)]);
    let len_u = unit(&mut c, ".LENGTHUNIT.", ".METRE.");
    let area_u = unit(&mut c, ".AREAUNIT.", ".SQUARE_METRE.");
    let vol_u = unit(&mut c, ".VOLUMEUNIT.", ".CUBIC_METRE.");
    let ang_u = unit(&mut c, ".PLANEANGLEUNIT.", ".RADIAN.");
    let units =
        c.s.add("IFCUNITASSIGNMENT", &[list(&[len_u, area_u, vol_u, ang_u])]);
    let q_units = [len_u, area_u, vol_u];
    topo::map_conversion(&mut c, model, ctx, len_u);
    let info = projinfo::load(model)?;
    let [obj_type, long_name, phase] = projinfo::head(&info);
    let project = c.rooted(
        "IFCPROJECT",
        "project",
        0,
        projinfo::name(&info),
        &[obj_type, long_name, phase, list(&[ctx]), r(units)],
    );
    projinfo::pset(&mut c, project, &info);
    let addr = projinfo::address(&mut c, &info);

    // --- site, building
    let ax = c.axis3(c.origin, None);
    let site_pl = c.placement(None, ax);
    let [lat, lon, ref_z] = topo::site_attrs(model);
    let site = c.rooted(
        "IFCSITE",
        "site",
        0,
        &topo::site_name(model),
        &[
            s(NONE),
            r(site_pl),
            s(NONE),
            s(NONE),
            s(".ELEMENT."),
            lat,
            lon,
            ref_z,
            s(NONE),
            addr.clone(),
        ],
    );
    let ax = c.axis3(c.origin, None);
    let bld_pl = c.placement(Some(site_pl), ax);
    let building = c.rooted(
        "IFCBUILDING",
        "building",
        0,
        "Building",
        &[
            s(NONE),
            r(bld_pl),
            s(NONE),
            s(NONE),
            s(".ELEMENT."),
            s(NONE),
            s(NONE),
            addr,
        ],
    );
    c.rel(
        "IFCRELAGGREGATES",
        "agg-project",
        0,
        &[r(project), list(&[site])],
    );
    c.rel(
        "IFCRELAGGREGATES",
        "agg-site",
        0,
        &[r(site), list(&[building])],
    );
    for w in topo::site_elements(&mut c, model, site, site_pl, body_ctx, q_units)? {
        eprintln!("ifc export: {w}");
    }

    // --- materials and layer sets
    let mut layer_sets: BTreeMap<i64, (u32, f64)> = BTreeMap::new();
    let types: Vec<(i64, String)> = {
        let mut st = sql
            .prepare("SELECT id,name FROM wall_type ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    for (tid, tname) in &types {
        let layers: Vec<(String, f64)> = {
            let mut st = sql
                .prepare(
                    "SELECT material,thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos",
                )
                .or_else(db)?;
            let rows = st
                .query_map([tid], |x| Ok((x.get(0)?, x.get(1)?)))
                .or_else(db)?;
            rows.collect::<Result<_, _>>().or_else(db)?
        };
        let mut ids = vec![];
        for (m, t) in &layers {
            let mid = c.material(m);
            ids.push(c.s.add(
                "IFCMATERIALLAYER",
                &[
                    r(mid),
                    real(*t),
                    s(NONE),
                    text(m),
                    s(NONE),
                    s(NONE),
                    s(NONE),
                ],
            ));
        }
        let set =
            c.s.add("IFCMATERIALLAYERSET", &[list(&ids), text(tname), s(NONE)]);
        layer_sets.insert(*tid, (set, layers.iter().map(|l| l.1).sum()));
    }

    // --- slab layer sets (layer 0 is the top; usage runs downward from the top face)
    let mut slab_sets: BTreeMap<i64, (u32, f64)> = BTreeMap::new();
    let stypes: Vec<(i64, String)> = {
        let mut st = sql
            .prepare("SELECT id,name FROM slab_type ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    for (tid, tname) in &stypes {
        let layers: Vec<(String, f64)> = {
            let mut st = sql
                .prepare(
                    "SELECT material,thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos",
                )
                .or_else(db)?;
            let rows = st
                .query_map([tid], |x| Ok((x.get(0)?, x.get(1)?)))
                .or_else(db)?;
            rows.collect::<Result<_, _>>().or_else(db)?
        };
        let mut ids = vec![];
        for (m, t) in &layers {
            let mid = c.material(m);
            ids.push(c.s.add(
                "IFCMATERIALLAYER",
                &[
                    r(mid),
                    real(*t),
                    s(NONE),
                    text(m),
                    s(NONE),
                    s(NONE),
                    s(NONE),
                ],
            ));
        }
        let set =
            c.s.add("IFCMATERIALLAYERSET", &[list(&ids), text(tname), s(NONE)]);
        slab_sets.insert(*tid, (set, layers.iter().map(|l| l.1).sum()));
    }
    let dir_down = c.s.add("IFCDIRECTION", &[reals(&[0.0, 0.0, -1.0])]);

    // --- storeys and walls
    let levels: Vec<(i64, String, f64)> = {
        let mut st = sql
            .prepare("SELECT id,name,elevation FROM level ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    let mut storeys = vec![];
    let mut storey_pls = vec![];
    let mut storey_walls: Vec<(u32, Vec<u32>)> = vec![];
    let mut storey_spaces: Vec<(u32, Vec<u32>)> = vec![];
    let mut room_spaces: BTreeMap<i64, u32> = BTreeMap::new();
    let mut wall_ent: BTreeMap<i64, u32> = BTreeMap::new();
    let mut wall_ext: BTreeMap<i64, bool> = BTreeMap::new();
    let mut comp_maps: BTreeMap<(i64, String), u32> = BTreeMap::new();
    let mut comp_type_out: BTreeMap<i64, comp_types::Members> = BTreeMap::new();
    let mut solid_voids: Vec<solids::PendingVoid> = vec![];
    let mut solid_asm = solids::Assemblies::default();
    let net = model.mep_network(None).map_err(|e| e.to_string())?;
    let extra = extra::prepare(model)?;
    let struct_bodies = structure::load(model)?;
    let mut mep_out = mep::MepOut {
        devices: mep::device_ports(model, &net)?,
        ..Default::default()
    };
    for (lid, lname, elev) in &levels {
        c.s.alive()?;
        let p = c.point3(0.0, 0.0, *elev);
        let ax = c.axis3(p, None);
        let pl = c.placement(Some(bld_pl), ax);
        storey_pls.push(pl);
        let storey = c.rooted(
            "IFCBUILDINGSTOREY",
            "storey",
            *lid,
            lname,
            &[
                s(NONE),
                r(pl),
                s(NONE),
                s(NONE),
                s(".ELEMENT."),
                real(*elev),
            ],
        );
        storeys.push(storey);

        let walls: Vec<WallRow> = {
            let mut st = sql
                .prepare(
                    "SELECT id,wtype,ax,ay,bx,by,base_offset,height,offset0,bulge,top_profile,lean,top_scale,top_core \
                     FROM wall WHERE level=?1 ORDER BY id",
                )
                .or_else(db)?;
            let rows = st
                .query_map([lid], |x| {
                    Ok(WallRow {
                        id: x.get(0)?,
                        wtype: x.get(1)?,
                        a: v2(x.get(2)?, x.get(3)?),
                        b: v2(x.get(4)?, x.get(5)?),
                        base_offset: x.get(6)?,
                        height: x.get(7)?,
                        offset0: x.get(8)?,
                        bulge: x.get(9)?,
                        top: x
                            .get::<_, Option<String>>(10)?
                            .as_deref()
                            .and_then(ddc_model::parse_top_profile),
                        top_core: x
                            .get::<_, Option<String>>(13)?
                            .as_deref()
                            .and_then(ddc_model::parse_top_profile),
                        base: None,
                        lean: Lean::new(x.get(11)?, x.get(12)?),
                    })
                })
                .or_else(db)?;
            let mut v: Vec<WallRow> = rows.collect::<Result<_, _>>().or_else(db)?;
            let straight: Vec<i64> = v.iter().filter(|w| w.bulge == 0.0).map(|w| w.id).collect();
            let bases = model.wall_base_profiles(&straight).map_err(|e| e.to_string())?;
            for (w, base) in v.iter_mut().filter(|w| w.bulge == 0.0).zip(bases) {
                w.base = base;
            }
            v
        };
        {
            let regions = model.wall_regions(*lid).unwrap_or_default();
            let mut geoms = vec![];
            for w in &walls {
                let (_, total) = layer_sets.get(&w.wtype).ok_or("wall type missing")?;
                let o0 = w.offset0.unwrap_or(-total / 2.0);
                geoms.push((w.id, w.a, w.b, w.bulge, o0, *total));
            }
            wall_ext.extend(ext_walls::resolve(model, &regions, &geoms));
        }
        let mut strips = vec![];
        // layers of each wall type: read once, not once per wall
        let mut type_layers: BTreeMap<i64, (Vec<f64>, Vec<(bool, u8)>)> = BTreeMap::new();
        for w in &walls {
            let (_, total) = layer_sets.get(&w.wtype).ok_or("wall type missing")?;
            if !type_layers.contains_key(&w.wtype) {
                let mut st = sql
                    .prepare("SELECT thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")
                    .or_else(db)?;
                let rows = st.query_map([w.wtype], |x| x.get(0)).or_else(db)?;
                let th: Vec<f64> = rows.collect::<Result<_, _>>().or_else(db)?;
                let fl = model.wall_layer_flags(w.wtype).unwrap_or_default();
                type_layers.insert(w.wtype, (th, fl));
            }
            let (th, fl) = &type_layers[&w.wtype];
            strips.push(Strip {
                a: w.a,
                b: w.b,
                layers: {
                    th.iter()
                        .enumerate()
                        .map(|(k, t)| {
                            Layer::with_priority(
                                *t,
                                fl.get(k).map_or(Layer::DEFAULT_PRIORITY, |f| f.1),
                            )
                        })
                        .collect()
                },
                offset0: w.offset0.unwrap_or(-total / 2.0),
                base: elev + w.base_offset,
                height: w.height,
            });
        }
        let bulges: Vec<f64> = walls.iter().map(|w| w.bulge).collect();
        // an edited model may hold overlapping corners: those walls get butt ends, as in the model
        let ids: Vec<i64> = walls.iter().map(|w| w.id).collect();
        let prefs = model.join_prefs(&ids).map_err(|e| e.to_string())?;
        let (joined, _) = ddc_model::join_tolerant(&strips, &bulges, &prefs, tol)
            .map_err(|e| format!("{e:?}"))?;
        // lean holds on walls with a flat top only, straight or arc (as in the model)
        let leans: Vec<Lean> = walls
            .iter()
            .map(|w| {
                let ok = w.top.is_none() && w.lean.valid();
                if ok { w.lean } else { Lean::PLAIN }
            })
            .collect();
        let top_join = if leans.iter().any(|l| !l.is_plain()) {
            let (tops, top_b) = ddc_geom::wall_lean::top_strips(&strips, &bulges, &leans);
            ddc_model::join_tolerant(&tops, &top_b, &prefs, tol).ok().map(|j| j.0)
        } else {
            None
        };
        // spline walls of the level: the arc pieces of each export as one IfcWall
        let splines: BTreeMap<i64, ddc_model::SplineWallSpec> = model
            .spline_walls(Some(*lid))
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();
        let mut spline_of: BTreeMap<i64, i64> = BTreeMap::new();
        for sid in splines.keys() {
            for p in model.spline_wall_pieces(*sid).map_err(|e| e.to_string())? {
                spline_of.insert(p, *sid);
            }
        }
        let mut spline_ent: BTreeMap<i64, u32> = BTreeMap::new();
        let mut elems = vec![];
        for (i, w) in walls.iter().enumerate() {
            let st = &strips[i];
            let lean = leans[i];
            let leaning = !lean.is_plain();
            let (set, total) = layer_sets[&w.wtype];
            let d = st.dir();
            let arc = (w.bulge != 0.0)
                .then(|| ArcAxis::new(w.a, w.b, w.bulge, tol))
                .transpose()
                .map_err(|e| format!("wall {}: arc {e:?}", w.id))?;
            let length = arc.map_or(st.length(), |a| a.length());
            let off0 = st.offset0;
            // wall frame: origin a, x along the chord, y to its left
            let n = st.normal();
            let loc = |p: V2| ((p - w.a).dot(d), (p - w.a).dot(n));

            // placement in the storey frame
            let p = c.point3(w.a.x, w.a.y, w.base_offset);
            let xd = c.s.add("IFCDIRECTION", &[reals(&[d.x, d.y, 0.0])]);
            let ax = c.axis3(p, Some(xd));
            let wpl = c.placement(Some(pl), ax);
            // a spline wall's piece: the IfcWall of the whole wall (written at its first piece)
            // stands in for the piece's own; the piece frame still places its openings
            let group = match spline_of.get(&w.id) {
                Some(&sid) => Some(match spline_ent.get(&sid) {
                    Some(&e) => e,
                    None => {
                        let host = spline_walls::Host {
                            wpl,
                            loc: &loc,
                            body_ctx,
                            axis_ctx,
                            set: (set, total),
                            units: q_units,
                        };
                        let th = &type_layers[&w.wtype].0;
                        let e = spline_walls::wall(&mut c, model, sid, &splines[&sid], th, &host)?;
                        spline_ent.insert(sid, e);
                        elems.push(e);
                        e
                    }
                }),
                None => None,
            };

            // body
            let mut solids = vec![];
            // core layers under a roof with a finish below its structure: own outlines and own clip
            let split = w.top_core.is_some() && arc.is_none() && !leaning && group.is_none();
            let mut core_curves: Vec<u32> = vec![];
            // an arc wall's profile (u along the axis): faceted sectors, each with its own clip
            let arc_top = arc.filter(|_| !leaning && group.is_none()).and(w.top.as_ref());
            let curves: Vec<u32> = if leaning || group.is_some() || arc_top.is_some() {
                vec![]
            } else if split {
                let core = model.wall_core_layers(w.wtype).unwrap_or_default();
                let mut out = vec![];
                for (k, layer) in joined.rings[i].iter().enumerate() {
                    let fp: Vec<Vec<Vec<V2>>> =
                        vec![layer.iter().map(|r| r.tessellate(CHORD_SAG)).collect()];
                    for ring in outlines_of(st, &fp)? {
                        let mut pts: Vec<u32> = ring.iter().map(|q| c.point2(q.0, q.1)).collect();
                        pts.push(pts[0]);
                        let curve = c.s.add("IFCPOLYLINE", &[list(&pts)]);
                        if core.get(k) == Some(&true) {
                            core_curves.push(curve);
                        } else {
                            out.push(curve);
                        }
                    }
                }
                out
            } else if arc.is_some() {
                joined.rings[i]
                    .iter()
                    .flatten()
                    .map(|ring| indexed_curve(&mut c, ring, &loc))
                    .collect()
            } else {
                let fp: Vec<Vec<Vec<V2>>> = joined.rings[i]
                    .iter()
                    .map(|l| l.iter().map(|r| r.tessellate(CHORD_SAG)).collect())
                    .collect();
                let mut out = vec![];
                for ring in outlines_of(st, &fp)? {
                    let mut pts: Vec<u32> = ring.iter().map(|q| c.point2(q.0, q.1)).collect();
                    pts.push(pts[0]);
                    out.push(c.s.add("IFCPOLYLINE", &[list(&pts)]));
                }
                out
            };
            // a non-flat top: plan extrusion up to the highest point, intersected with the wall's
            // elevation (top edge) extruded across the wall
            let top = w.top.as_ref().filter(|_| arc.is_none());
            let base = w.base.as_ref().filter(|_| arc.is_none() && !leaning);
            let rise = top.map_or(w.height, |t| t.iter().fold(w.height, |m, p| m.max(p.1)));
            let lo = base.map_or(0.0, |b| b.iter().fold(0.0f64, |m, p| m.min(p.1)));
            let flat = vec![(0.0, w.height), (length, w.height)];
            let clip = (top.is_some() || base.is_some()).then(|| {
                let t = top.unwrap_or(&flat);
                let b = base.map_or(&[][..], |b| &b[..]);
                elevation_solid(&mut c, t, b, length, off0.abs() + total + 1.0, (0.0, 0.0), 0.0)
            });
            let clip_core = w.top_core.as_ref().filter(|_| split).map(|t| {
                let b = base.map_or(&[][..], |b| &b[..]);
                elevation_solid(&mut c, t, b, length, off0.abs() + total + 1.0, (0.0, 0.0), 0.0)
            });
            let rise_core = w
                .top_core
                .as_ref()
                .filter(|_| split)
                .map_or(rise, |t| t.iter().fold(rise, |m, p| m.max(p.1)));
            let all: Vec<(u32, bool)> = curves
                .into_iter()
                .map(|p| (p, false))
                .chain(core_curves.into_iter().map(|p| (p, true)))
                .collect();
            for (poly, is_core) in all {
                let (clip, rise) = if is_core { (clip_core, rise_core) } else { (clip, rise) };
                let prof = c.s.add(
                    "IFCARBITRARYCLOSEDPROFILEDEF",
                    &[s(".AREA."), s(NONE), r(poly)],
                );
                let o3 = c.point3(0.0, 0.0, lo);
                let sax = c.axis3(o3, None);
                let sol = c.s.add(
                    "IFCEXTRUDEDAREASOLID",
                    &[r(prof), r(sax), r(c.dir_z), real(rise - lo)],
                );
                solids.push(match clip {
                    Some(e) => {
                        c.s.add("IFCBOOLEANRESULT", &[s(".INTERSECTION."), r(sol), r(e)])
                    }
                    None => sol,
                });
            }
            if let (Some(ax), Some(t)) = (arc, arc_top) {
                let len = ax.length();
                let mut us = vec![0.0, len];
                us.extend(t.iter().map(|q| q.0).filter(|u| *u > 1e-9 && *u < len - 1e-9));
                us.sort_by(f64::total_cmp);
                us.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
                let step = (10f64.to_radians() * ax.radius).max(1e-3);
                let half = off0.abs() + total + 1.0;
                for pair in us.windows(2) {
                    let parts = ((pair[1] - pair[0]) / step).ceil().max(1.0) as usize;
                    for k in 0..parts {
                        let s0 = pair[0] + (pair[1] - pair[0]) * k as f64 / parts as f64;
                        let s1 = pair[0] + (pair[1] - pair[0]) * (k + 1) as f64 / parts as f64;
                        let (p0, p1) = (ax.point_at(s0), ax.point_at(s1));
                        let ch = (p1 - p0).len();
                        if ch < 1e-9 {
                            continue;
                        }
                        let (h0, h1) = (
                            ddc_geom::top::height_at(t, s0 + 1e-7),
                            ddc_geom::top::height_at(t, s1 - 1e-7),
                        );
                        // the sector's frame: x along its chord, in the wall frame
                        let e = p1 - p0;
                        let theta = e.dot(n).atan2(e.dot(d));
                        let clip = elevation_solid(&mut c, &[(0.0, h0), (ch, h1)], &[], ch, half, loc(p0), theta);
                        let rise = w.height.max(h0).max(h1);
                        for layer in &joined.rings[i] {
                            for pr in layer {
                                let mut ring = pr.tessellate(CHORD_SAG);
                                if s0 > 1e-9 {
                                    ring = clip_half(&ring, ax.center, ax.tangent_at(s0));
                                }
                                if s1 < len - 1e-9 {
                                    ring = clip_half(&ring, ax.center, ax.tangent_at(s1) * -1.0);
                                }
                                if ring.len() < 3 {
                                    continue;
                                }
                                let mut pts: Vec<u32> = ring
                                    .iter()
                                    .map(|q| {
                                        let l = loc(*q);
                                        c.point2(l.0, l.1)
                                    })
                                    .collect();
                                pts.push(pts[0]);
                                let poly = c.s.add("IFCPOLYLINE", &[list(&pts)]);
                                let prof = c.s.add(
                                    "IFCARBITRARYCLOSEDPROFILEDEF",
                                    &[s(".AREA."), s(NONE), r(poly)],
                                );
                                let o3 = c.point3(0.0, 0.0, 0.0);
                                let sax = c.axis3(o3, None);
                                let sol = c.s.add(
                                    "IFCEXTRUDEDAREASOLID",
                                    &[r(prof), r(sax), r(c.dir_z), real(rise)],
                                );
                                solids.push(c.s.add("IFCBOOLEANRESULT", &[s(".INTERSECTION."), r(sol), r(clip)]));
                            }
                        }
                    }
                }
            }
            let kind = if leaning {
                let top = top_join.as_ref().map(|t| &t.rings[i]);
                let rings = &joined.rings[i];
                lean_body(&mut c, (st, w.bulge), &lean, rings, top, &loc, &mut solids)
            } else if clip.is_some() || arc_top.is_some() {
                "CSG"
            } else {
                "SweptSolid"
            };
            let wall = match group {
                Some(g) => g,
                None => {
                    let body = c.s.add(
                        "IFCSHAPEREPRESENTATION",
                        &[r(body_ctx), text("Body"), text(kind), list(&solids)],
                    );
                    // axis
                    let line = match arc {
                        Some(ax) => {
                            let (cx, cy) = loc(ax.center);
                            let cp = c.point2(cx, cy);
                            let cpl = c.s.add("IFCAXIS2PLACEMENT2D", &[r(cp), s(NONE)]);
                            let circle = c.s.add("IFCCIRCLE", &[r(cpl), real(ax.radius)]);
                            let (bx, by) = loc(w.b);
                            let (p0, p1) = (c.point2(0.0, 0.0), c.point2(bx, by));
                            let sense = if ax.sweep > 0.0 { ".T." } else { ".F." };
                            c.s.add(
                                "IFCTRIMMEDCURVE",
                                &[
                                    r(circle),
                                    list(&[p0]),
                                    list(&[p1]),
                                    s(sense),
                                    s(".CARTESIAN."),
                                ],
                            )
                        }
                        None => {
                            let (p0, p1) = (c.point2(0.0, 0.0), c.point2(length, 0.0));
                            c.s.add("IFCPOLYLINE", &[list(&[p0, p1])])
                        }
                    };
                    let axis = c.s.add(
                        "IFCSHAPEREPRESENTATION",
                        &[r(axis_ctx), text("Axis"), text("Curve2D"), list(&[line])],
                    );
                    let pds = c.s.add(
                        "IFCPRODUCTDEFINITIONSHAPE",
                        &[s(NONE), s(NONE), list(&[axis, body])],
                    );
                    let wall = c.rooted(
                        "IFCWALL",
                        "wall",
                        w.id,
                        &format!("Wall {}", w.id),
                        &[s(NONE), r(wpl), r(pds), s(NONE), s(".STANDARD.")],
                    );
                    let usage = c.s.add(
                        "IFCMATERIALLAYERSETUSAGE",
                        &[r(set), s(".AXIS2."), s(".POSITIVE."), real(off0), s(NONE)],
                    );
                    c.rel(
                        "IFCRELASSOCIATESMATERIAL",
                        "mat",
                        w.id,
                        &[list(&[wall]), r(usage)],
                    );
                    wall
                }
            };

            // openings
            type Op = (i64, f64, f64, f64, f64, String, bool, bool);
            let ops: Vec<Op> = {
                let mut q = sql
                    .prepare_cached(
                        "SELECT id,along,width,sill,height,kind,swing_side,swing_hinge FROM opening \
                         WHERE wall=?1 ORDER BY id",
                    )
                    .or_else(db)?;
                let rows = q
                    .query_map([w.id], |x| {
                        Ok((
                            x.get(0)?,
                            x.get(1)?,
                            x.get(2)?,
                            x.get(3)?,
                            x.get(4)?,
                            x.get(5)?,
                            x.get(6)?,
                            x.get(7)?,
                        ))
                    })
                    .or_else(db)?;
                rows.collect::<Result<_, _>>().or_else(db)?
            };
            let mut fills = vec![];
            for (oid, along, width, sill, oh, kind, side, hinge) in ops {
                // on an arc wall the opening frame sits at the arc point, x along the tangent;
                // `nl`: the wall's left normal there in the wall frame
                let (oc, ox, nl) = match arc {
                    Some(ax) => {
                        let m = ax.point_at(along + width / 2.0);
                        let t = ax.tangent_at(along + width / 2.0);
                        let nn = v2(-t.y, t.x);
                        let (mx, my) = loc(m + nn * (off0 + total / 2.0));
                        let tl = (t.dot(d), t.dot(n));
                        (
                            (mx, my),
                            Some(c.s.add("IFCDIRECTION", &[reals(&[tl.0, tl.1, 0.0])])),
                            (nn.dot(d), nn.dot(n)),
                        )
                    }
                    None => ((along + width / 2.0, off0 + total / 2.0), None, (0.0, 1.0)),
                };
                let (opl, sol, okind) = if leaning {
                    // the model's cutter: a box normal to the leaning surface
                    let o = ddc_geom::layered::Opening {
                        along,
                        width,
                        sill,
                        height: oh,
                    };
                    let m = ddc_geom::wall_lean::opening_cutter(st, w.bulge, &lean, &o);
                    let fs = face_set(&mut c, &m, |q| {
                        let (x, y) = loc(v2(q.x, q.y));
                        (x, y, q.z - st.base)
                    });
                    let o3 = c.point3(0.0, 0.0, 0.0);
                    let ax = c.axis3(o3, None);
                    (c.placement(Some(wpl), ax), fs, "Tessellation")
                } else {
                    let p = c.point3(oc.0, oc.1, sill);
                    let ax = c.axis3(p, ox);
                    let opl = c.placement(Some(wpl), ax);
                    let p2 = c.point2(0.0, 0.0);
                    let pos2 = c.s.add("IFCAXIS2PLACEMENT2D", &[r(p2), s(NONE)]);
                    let rect = c.s.add(
                        "IFCRECTANGLEPROFILEDEF",
                        &[
                            s(".AREA."),
                            s(NONE),
                            r(pos2),
                            real(width),
                            real(total + 2.0 * OVERCUT),
                        ],
                    );
                    let o3 = c.point3(0.0, 0.0, 0.0);
                    let sax = c.axis3(o3, None);
                    let sol = c.s.add(
                        "IFCEXTRUDEDAREASOLID",
                        &[r(rect), r(sax), r(c.dir_z), real(oh)],
                    );
                    (opl, sol, "SweptSolid")
                };
                let rep = c.s.add(
                    "IFCSHAPEREPRESENTATION",
                    &[r(body_ctx), text("Body"), text(okind), list(&[sol])],
                );
                let opds = c.s.add(
                    "IFCPRODUCTDEFINITIONSHAPE",
                    &[s(NONE), s(NONE), list(&[rep])],
                );
                let open = c.rooted(
                    "IFCOPENINGELEMENT",
                    "opening",
                    oid,
                    &format!("Opening {oid} ({kind})"),
                    &[s(NONE), r(opl), r(opds), s(NONE), s(".OPENING.")],
                );
                c.rel("IFCRELVOIDSELEMENT", "void", oid, &[r(wall), r(open)]);
                if kind == "door" || kind == "window" {
                    let door = kind == "door";
                    // on a leaning wall the fill turns about its sill line with the cut
                    let (sn, cs) = lean.angle.sin_cos();
                    let p = if leaning {
                        // the centre at offset `oo` turns about the sill line: across by
                        // oo (cos - 1) + shift, down by oo sin (radially on an arc wall)
                        let oo = off0 + total / 2.0;
                        let (x, y) = if arc.is_some() {
                            let across = oo * (cs - 1.0) + lean.shift(sill);
                            (oc.0 + nl.0 * across, oc.1 + nl.1 * across)
                        } else {
                            (oc.0, oo * cs + lean.shift(sill))
                        };
                        c.point3(x, y, sill - oo * sn)
                    } else {
                        c.point3(oc.0, oc.1, sill)
                    };
                    // Doors swing toward their local +Y: opening to the wall's right turns the
                    // door frame by 180 degrees about Z.
                    let xd = if let (Some(ax), Some(ox)) = (arc, ox) {
                        if door && side {
                            let t = ax.tangent_at(along + width / 2.0) * -1.0;
                            Some(c.s.add("IFCDIRECTION", &[reals(&[t.dot(d), t.dot(n), 0.0])]))
                        } else {
                            Some(ox)
                        }
                    } else if door && side {
                        let nx = match c.dir_nx {
                            Some(d) => d,
                            None => {
                                let d = c.s.add("IFCDIRECTION", &[reals(&[-1.0, 0.0, 0.0])]);
                                c.dir_nx = Some(d);
                                d
                            }
                        };
                        Some(nx)
                    } else {
                        None
                    };
                    let ax = if leaning {
                        let up = match arc {
                            Some(_) => [nl.0 * sn, nl.1 * sn, cs],
                            None => [0.0, sn, cs],
                        };
                        let zd = c.s.add("IFCDIRECTION", &[reals(&up)]);
                        let xd = xd.map_or(NONE.to_string(), r);
                        c.s.add("IFCAXIS2PLACEMENT3D", &[r(p), r(zd), xd])
                    } else {
                        c.axis3(p, xd)
                    };
                    let fpl = c.placement(Some(wpl), ax);
                    let items = fill_body(&mut c, door, width, oh, total);
                    let rep = c.s.add(
                        "IFCSHAPEREPRESENTATION",
                        &[r(body_ctx), text("Body"), text("SweptSolid"), list(&items)],
                    );
                    let fds = c.s.add(
                        "IFCPRODUCTDEFINITIONSHAPE",
                        &[s(NONE), s(NONE), list(&[rep])],
                    );
                    // LEFT: hinge on the door-local -X side; in the wall frame that is the
                    // `along` start unless the door is turned (side).
                    let op_type = match (door, side == hinge) {
                        (false, _) => NONE,
                        (true, true) => ".SINGLE_SWING_LEFT.",
                        (true, false) => ".SINGLE_SWING_RIGHT.",
                    };
                    let (ent, label, pre) = if door {
                        ("IFCDOOR", "Door", ".DOOR.")
                    } else {
                        ("IFCWINDOW", "Window", ".WINDOW.")
                    };
                    let fill = c.rooted(
                        ent,
                        kind.as_str(),
                        oid,
                        &format!("{label} {oid}"),
                        &[
                            s(NONE),
                            r(fpl),
                            r(fds),
                            s(NONE),
                            real(oh),
                            real(width),
                            s(pre),
                            s(op_type),
                            s(NONE),
                        ],
                    );
                    c.rel("IFCRELFILLSELEMENT", "fill", oid, &[r(open), r(fill)]);
                    let q = |c: &mut Ctx, name: &str, ent: &str, unit: u32, v: f64| {
                        c.s.add(ent, &[text(name), s(NONE), r(unit), real(v), s(NONE)])
                    };
                    let ql = [
                        q(&mut c, "Width", "IFCQUANTITYLENGTH", len_u, width),
                        q(&mut c, "Height", "IFCQUANTITYLENGTH", len_u, oh),
                        q(&mut c, "Area", "IFCQUANTITYAREA", area_u, width * oh),
                    ];
                    let eq = c.rooted(
                        "IFCELEMENTQUANTITY",
                        "qto-fill",
                        oid,
                        &format!("Qto_{label}BaseQuantities"),
                        &[s(NONE), list(&ql)],
                    );
                    c.rel(
                        "IFCRELDEFINESBYPROPERTIES",
                        "qto-fill-rel",
                        oid,
                        &[list(&[fill]), r(eq)],
                    );
                    fills.push(fill);
                }
            }

            wall_ent.insert(w.id, wall);
            if group.is_some() {
                // the spline wall's quantities went with its IfcWall
                elems.extend(fills);
                continue;
            }
            // quantities (equal to the model's wall_qty)
            let (net, area): (f64, f64) = sql
                .query_row(
                    "SELECT COALESCE(SUM(volume),0), COALESCE(SUM(plan_area),0) \
                     FROM wall_qty WHERE wall=?1",
                    [w.id],
                    |x| Ok((x.get(0)?, x.get(1)?)),
                )
                .or_else(db)?;
            let gross = area * w.height;
            let q = |c: &mut Ctx, name: &str, ent: &str, unit: u32, v: f64| {
                c.s.add(ent, &[text(name), s(NONE), r(unit), real(v), s(NONE)])
            };
            let ql = [
                q(&mut c, "Length", "IFCQUANTITYLENGTH", len_u, length),
                q(&mut c, "Height", "IFCQUANTITYLENGTH", len_u, w.height),
                q(&mut c, "NetVolume", "IFCQUANTITYVOLUME", vol_u, net),
                q(&mut c, "GrossVolume", "IFCQUANTITYVOLUME", vol_u, gross),
            ];
            let eq = c.rooted(
                "IFCELEMENTQUANTITY",
                "qto",
                w.id,
                "Qto_WallBaseQuantities",
                &[s(NONE), list(&ql)],
            );
            c.rel(
                "IFCRELDEFINESBYPROPERTIES",
                "qto-rel",
                w.id,
                &[list(&[wall]), r(eq)],
            );
            elems.push(wall);
            elems.extend(fills);
        }
        let g = Geo {
            storey_pl: pl,
            body_ctx,
            axis_ctx,
            dir_down,
            units: q_units,
        };
        slabs(&mut c, model, *lid, &g, &slab_sets, &mut elems)?;
        curtain_walls(&mut c, model, *lid, *elev, &g, axis_ctx, &mut elems)?;
        let seps = separators(&mut c, model, *lid, &g, axis_ctx, &mut elems)?;
        let spaces = spaces(
            &mut c,
            model,
            *lid,
            &g,
            &wall_ent,
            &wall_ext,
            &seps,
            &mut room_spaces,
        )?;
        components(
            &mut c,
            model,
            *lid,
            *elev,
            &g,
            &mut comp_maps,
            &mut mep_out.comps,
            &mut comp_type_out,
            &mut elems,
        )?;
        extra::all(
            &mut c, model, &extra, &slab_sets, *lid, *elev, &g, &mut elems,
        )?;
        solids::solids(
            &mut c,
            model,
            *lid,
            *elev,
            &g,
            &mut solid_voids,
            &mut solid_asm,
            &mut elems,
        )?;
        freeform::freeforms(&mut c, model, *lid, &g, &mut elems)?;
        adv::masses(&mut c, model, *lid, *elev, &g, &mut elems)?;
        panel_system::panel_systems(&mut c, model, *lid, *elev, &g, &mut elems)?;
        shell_elem::shells(&mut c, model, *lid, &g, &mut elems)?;
        mep::runs(
            &mut c,
            model,
            &net,
            *lid,
            *elev,
            &g,
            &mut mep_out,
            &mut elems,
        )?;
        mep::devices(&mut c, model, *elev, &g, &mut mep_out)?;
        structure::export(&mut c, &struct_bodies, *lid, *elev, &g, &mut elems)?;
        roof_edge::items(&mut c, model, *lid, *elev, &g, &mut elems)?;
        rebar::bars(&mut c, model, *lid, *elev, &g, &mut elems)?;
        ramps::items(&mut c, model, *lid, *elev, &g, &mut elems)?;
        stairs::stairs(&mut c, model, *lid, *elev, &g, &mut elems)?;
        stairs::railings(&mut c, model, *lid, *elev, &g, &mut elems)?;
        storey_walls.push((storey, elems));
        storey_spaces.push((storey, spaces));
    }
    ramps::zones(&mut c, model, &room_spaces)?;
    // void hosts may sit on other levels: relate after all levels
    solids::relate_voids(&mut c, &solid_voids);
    let level_ids: Vec<i64> = levels.iter().map(|l| l.0).collect();
    solids::assemblies(&mut c, &solid_asm, &level_ids, &storey_pls, &mut storey_walls);
    comp_types::write(&mut c, model, &comp_type_out)?;
    c.s.alive()?;
    for (k, (storey, sp)) in storey_spaces.iter().enumerate() {
        if !sp.is_empty() {
            c.rel(
                "IFCRELAGGREGATES",
                "agg-storey",
                levels[k].0,
                &[r(*storey), list(sp)],
            );
        }
    }
    mep::finish(&mut c, model, &net, &mep_out)?;
    mep::circuits(&mut c, model, building, &mep_out)?;
    rebar::analysis(&mut c, model, ctx, building)?;
    grids(
        &mut c,
        model,
        axis_ctx,
        &levels,
        &storey_pls,
        &mut storey_walls,
    )?;
    if !storeys.is_empty() {
        c.rel(
            "IFCRELAGGREGATES",
            "agg-building",
            0,
            &[r(building), list(&storeys)],
        );
    }
    for (k, (storey, elems)) in storey_walls.iter().enumerate() {
        if !elems.is_empty() {
            c.rel(
                "IFCRELCONTAINEDINSPATIALSTRUCTURE",
                "contain",
                levels[k].0,
                &[list(elems), r(*storey)],
            );
        }
    }
    userprops::write(&mut c, model, [len_u, area_u, vol_u, ang_u], &wall_ext)?;
    Ok((c.s, c.g))
}

/// Components: one IfcRepresentationMap (triangulated body in the family frame) per type and
/// instance values, placed by IfcMappedItem; a mirrored instance maps with x scale −1. The exact
/// definition, type, values and placement go to `DDC_Component`, so a round trip is exact.
#[allow(clippy::too_many_arguments)]
fn components(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    maps: &mut BTreeMap<(i64, String), u32>,
    ents: &mut BTreeMap<i64, u32>,
    types: &mut BTreeMap<i64, comp_types::Members>,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    struct Row {
        id: i64,
        ctype: i64,
        vals: String,
        local: [f64; 4],
        mirror: i64,
        host: [Option<i64>; 2],
        world: [f64; 4],
        wmirror: i64,
        volume: f64,
        def: String,
        tname: String,
        tvals: String,
        fname: String,
    }
    let sql = model.sql();
    let rows: Vec<Row> = {
        let mut st = sql
            .prepare(
                "SELECT c.id,c.ctype,c.vals,c.x,c.y,c.z,c.rotation,c.mirror,c.host_wall,c.host_slab,\
                 q.wx,q.wy,q.wz,q.wrot,q.wmirror,q.volume,f.def,t.name,t.vals,f.name \
                 FROM comp c JOIN comp_qty q ON q.comp=c.id JOIN comp_type t ON t.id=c.ctype \
                 JOIN comp_family f ON f.id=t.family                  WHERE c.level=?1 AND c.id NOT IN (SELECT comp FROM solid_comp) ORDER BY c.id",
            )
            .or_else(db)?;
        let rows = st
            .query_map([level], |x| {
                Ok(Row {
                    id: x.get(0)?,
                    ctype: x.get(1)?,
                    vals: x.get(2)?,
                    local: [x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?],
                    mirror: x.get(7)?,
                    host: [x.get(8)?, x.get(9)?],
                    world: [x.get(10)?, x.get(11)?, x.get(12)?, x.get(13)?],
                    wmirror: x.get(14)?,
                    volume: x.get(15)?,
                    def: x.get(16)?,
                    tname: x.get(17)?,
                    tvals: x.get(18)?,
                    fname: x.get(19)?,
                })
            })
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    let label = |v: &str| format!("IFCLABEL({})", text(v));
    let ftext = |v: &str| format!("IFCTEXT({})", text(v));
    let num = |v: f64| format!("{v:?}");
    for w in rows {
        let (fam, res, mut built) = model
            .component_built(w.id, ddc_model::Lod::Medium)
            .map_err(|e| e.to_string())?;
        let key = (w.ctype, w.vals.clone());
        let plant = fam.solids.iter().any(|s| matches!(s.shape, ddc_geom::component::Shape::Plant { .. }));
        // a plant is exported at its full detail: the model of a site keeps its trees
        if plant && !maps.contains_key(&key) {
            built = model
                .component_built(w.id, ddc_model::Lod::Fine)
                .map_err(|e| e.to_string())?
                .2;
        }
        let map = match maps.get(&key) {
            Some(&m) => m,
            None => {
                let mesh = &built.mesh;
                let coords = mesh
                    .positions
                    .iter()
                    .map(|p| reals(&[p.x, p.y, p.z]))
                    .collect::<Vec<_>>()
                    .join(",");
                let pts =
                    c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({coords})"), s(NONE)]);
                let idx = mesh
                    .triangles
                    .iter()
                    .map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1))
                    .collect::<Vec<_>>()
                    .join(",");
                let fs = c.s.add(
                    "IFCTRIANGULATEDFACESET",
                    &[r(pts), s(NONE), s(".T."), format!("({idx})"), s(NONE)],
                );
                let rep = c.s.add(
                    "IFCSHAPEREPRESENTATION",
                    &[
                        r(g.body_ctx),
                        text("Body"),
                        text("Tessellation"),
                        list(&[fs]),
                    ],
                );
                let o = c.point3(0.0, 0.0, 0.0);
                let ax = c.axis3(o, None);
                let m = c.s.add("IFCREPRESENTATIONMAP", &[r(ax), r(rep)]);
                maps.insert(key, m);
                m
            }
        };
        let o = c.point3(0.0, 0.0, 0.0);
        let op = if w.wmirror != 0 {
            c.s.add(
                "IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM",
                &[
                    s(NONE),
                    s(NONE),
                    r(o),
                    real(-1.0),
                    s(NONE),
                    real(1.0),
                    real(1.0),
                ],
            )
        } else {
            c.s.add(
                "IFCCARTESIANTRANSFORMATIONOPERATOR3D",
                &[s(NONE), s(NONE), r(o), s(NONE), s(NONE)],
            )
        };
        let item = c.s.add("IFCMAPPEDITEM", &[r(map), r(op)]);
        let rep = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[
                r(g.body_ctx),
                text("Body"),
                text("MappedRepresentation"),
                list(&[item]),
            ],
        );
        let pds = c.s.add(
            "IFCPRODUCTDEFINITIONSHAPE",
            &[s(NONE), s(NONE), list(&[rep])],
        );
        let (wx, wy, wz, wr) = (w.world[0], w.world[1], w.world[2] - elev, w.world[3]);
        let p = c.point3(wx, wy, wz);
        let xd =
            c.s.add("IFCDIRECTION", &[reals(&[wr.cos(), wr.sin(), 0.0])]);
        let ax = c.axis3(p, Some(xd));
        let pl = c.placement(Some(g.storey_pl), ax);
        let (class, pre) = &fam.ifc;
        let ent = c.rooted(
            &class.to_ascii_uppercase(),
            "comp",
            w.id,
            &format!("{}:{}", w.fname, w.tname),
            &[
                text(&w.tname),
                r(pl),
                r(pds),
                s(NONE),
                s(&format!(".{pre}.")),
            ],
        );
        let (hk, hg) = match w.host {
            [Some(h), _] => ("wall", c.g.get("wall", h)),
            [_, Some(h)] => ("slab", c.g.get("slab", h)),
            _ => ("", String::new()),
        };
        let lp = w.local.map(num).join(" ");
        let wp = [wx, wy, wz, wr].map(num).join(" ");
        let fields = [
            ("Family", label(&w.fname)),
            ("Type", label(&w.tname)),
            ("Category", label(fam.category.name())),
            ("Definition", ftext(&w.def)),
            ("TypeValues", ftext(&w.tvals)),
            ("Values", ftext(&w.vals)),
            ("Local", label(&format!("{lp} {}", w.mirror))),
            ("World", label(&format!("{wp} {}", w.wmirror))),
            ("HostKind", label(hk)),
            ("Host", label(hg.trim_matches('\''))),
            ("Volume", format!("IFCVOLUMEMEASURE({})", real(w.volume))),
        ];
        let props: Vec<u32> = fields
            .into_iter()
            .map(|(n, v)| {
                c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)])
            })
            .collect();
        let set = c.rooted(
            "IFCPROPERTYSET",
            "comp-pset",
            w.id,
            "DDC_Component",
            &[list(&props)],
        );
        c.rel(
            "IFCRELDEFINESBYPROPERTIES",
            "comp-pset-rel",
            w.id,
            &[list(&[ent]), r(set)],
        );
        if plant {
            // what a landscape take-off asks of a tree
            let mut pl: Vec<(&str, String)> = vec![("Species", label(&w.tname))];
            if let Some(p) = ddc_geom::component::plant_of(&fam, &res) {
                pl.push(("Form", label(p.form.name())));
                for (name, v) in [("Height", p.height), ("CrownWidth", p.crown), ("TrunkDiameter", p.trunk), ("ClearTrunkHeight", p.clear)] {
                    pl.push((name, format!("IFCLENGTHMEASURE({})", real(v))));
                }
                pl.push(("FoliageFullness", format!("IFCREAL({})", real(p.leaf))));
                pl.push(("GrowthSeed", format!("IFCINTEGER({})", p.seed as i64)));
            }
            for s in fam.solids.iter().filter(|s| matches!(s.shape, ddc_geom::component::Shape::Plant { .. })) {
                if let Some(m) = &s.material {
                    use ddc_geom::component::{plant::Organ, Shape};
                    let name = match s.shape {
                        Shape::Plant { organ: Organ::Wood, .. } => "BarkMaterial",
                        Shape::Plant { organ: Organ::Shade, .. } => "InnerCrownMaterial",
                        Shape::Plant { organ: Organ::Light, .. } => "LitCrownMaterial",
                        Shape::Plant { organ: Organ::Streak, .. } => "BarkStreakMaterial",
                        _ => "FoliageMaterial",
                    };
                    pl.push((name, label(&res.material_name(m))));
                }
            }
            let ps: Vec<u32> = pl
                .into_iter()
                .map(|(k, v)| c.s.add("IFCPROPERTYSINGLEVALUE", &[text(k), s(NONE), v, s(NONE)]))
                .collect();
            let pset = c.rooted("IFCPROPERTYSET", "comp-plant-pset", w.id, "DDC_Planting", &[list(&ps)]);
            c.rel("IFCRELDEFINESBYPROPERTIES", "comp-plant-rel", w.id, &[list(&[ent]), r(pset)]);
        }
        let t = types.entry(w.ctype).or_default();
        t.members.push(ent);
        // the type shows its own values: the map of an instance without overrides
        if t.map.is_none() || w.vals.trim().is_empty() {
            t.map = Some(map);
        }
        ents.insert(w.id, ent);
        out.push(ent);
    }
    Ok(())
}

/// Grids: one IfcGrid per storey of the axes' base level (no base — first storey); axes whose
/// name starts with a digit go to U, others to V. Bubbles, level range, exact bulge and order are
/// kept in the `DDC_Grid` property set, so a round trip is exact.
fn grids(
    c: &mut Ctx,
    model: &Model,
    axis_ctx: u32,
    levels: &[(i64, String, f64)],
    storey_pls: &[u32],
    storey_walls: &mut [(u32, Vec<u32>)],
) -> Result<(), String> {
    let gs = model.grids().map_err(|e| e.to_string())?;
    if gs.is_empty() || storey_pls.is_empty() {
        return Ok(());
    }
    let lname = |l: Option<i64>| {
        l.and_then(|l| levels.iter().find(|x| x.0 == l))
            .map_or(String::new(), |x| x.1.clone())
    };
    let mut groups: BTreeMap<usize, Vec<(usize, &ddc_model::Grid)>> = BTreeMap::new();
    for (k, g) in gs.iter().enumerate() {
        let si = g
            .spec
            .base_level
            .and_then(|l| levels.iter().position(|x| x.0 == l))
            .unwrap_or(0);
        groups.entry(si).or_default().push((k, g));
    }
    for (si, members) in groups {
        let lid = &levels[si].0;
        let (mut u, mut v, mut curves, mut props) = (vec![], vec![], vec![], vec![]);
        for (k, g) in &members {
            let sp = &g.spec;
            let curve = if sp.bulge == 0.0 {
                let (p0, p1) = (c.point2(sp.a.0, sp.a.1), c.point2(sp.b.0, sp.b.1));
                c.s.add("IFCPOLYLINE", &[list(&[p0, p1])])
            } else {
                let m = sp.curve().mid();
                let coords = [sp.a, (m.x, m.y), sp.b]
                    .iter()
                    .map(|p| reals(&[p.0, p.1]))
                    .collect::<Vec<_>>()
                    .join(",");
                let pl =
                    c.s.add("IFCCARTESIANPOINTLIST2D", &[format!("({coords})"), s(NONE)]);
                c.s.add(
                    "IFCINDEXEDPOLYCURVE",
                    &[r(pl), "(IFCARCINDEX((1,2,3)))".into(), s(".F.")],
                )
            };
            curves.push(curve);
            let ax = c.s.add("IFCGRIDAXIS", &[text(&g.name), r(curve), s(".T.")]);
            if g.name.starts_with(|ch: char| ch.is_ascii_digit()) {
                u.push(ax);
            } else {
                v.push(ax);
            }
            let bub = sp.bubble_start as i64 | (sp.bubble_end as i64) << 1;
            let val = format!(
                "ord={k};bubbles={bub};base={};top={};bulge={}",
                lname(sp.base_level),
                lname(sp.top_level),
                real(sp.bulge)
            );
            props.push(c.s.add(
                "IFCPROPERTYSINGLEVALUE",
                &[
                    text(&g.name),
                    s(NONE),
                    format!("IFCLABEL({})", text(&val)),
                    s(NONE),
                ],
            ));
        }
        let set = c.s.add("IFCGEOMETRICCURVESET", &[list(&curves)]);
        let rep = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[
                r(axis_ctx),
                text("FootPrint"),
                text("GeometricCurveSet"),
                list(&[set]),
            ],
        );
        let pds = c.s.add(
            "IFCPRODUCTDEFINITIONSHAPE",
            &[s(NONE), s(NONE), list(&[rep])],
        );
        let p = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(p, None);
        let pl = c.placement(Some(storey_pls[si]), ax);
        // the IfcGrid keeps the GlobalId of the first imported axis row that has one
        let via: Vec<(&'static str, i64)> = members.iter().map(|(_, g)| ("grid", g.id)).collect();
        c.g.get_via("grid", *lid, &via);
        let grid = c.rooted(
            "IFCGRID",
            "grid",
            *lid,
            "Grid",
            &[
                s(NONE),
                r(pl),
                r(pds),
                list(&u),
                list(&v),
                s(NONE),
                s(".RECTANGULAR."),
            ],
        );
        let pset = c.rooted(
            "IFCPROPERTYSET",
            "grid-pset",
            *lid,
            "DDC_Grid",
            &[list(&props)],
        );
        c.rel(
            "IFCRELDEFINESBYPROPERTIES",
            "grid-pset-rel",
            *lid,
            &[list(&[grid]), r(pset)],
        );
        storey_walls[si].1.push(grid);
    }
    Ok(())
}

/// Ring with arc edges as IfcIndexedPolyCurve (IfcLineIndex / IfcArcIndex), in the wall frame.
fn indexed_curve(c: &mut Ctx, ring: &BulgeRing, loc: &dyn Fn(V2) -> (f64, f64)) -> u32 {
    let n = ring.pts.len();
    let mut pts: Vec<(f64, f64)> = vec![];
    let mut segs: Vec<String> = vec![];
    for i in 0..n {
        let (p, q, b) = (ring.pts[i], ring.pts[(i + 1) % n], ring.bulges[i]);
        let start = pts.len() + 1;
        pts.push(loc(p));
        let end = if i + 1 == n {
            1
        } else {
            pts.len() + 1 + (b != 0.0) as usize
        };
        if b != 0.0 {
            pts.push(loc(edge_mid(p, q, b)));
            segs.push(format!("IFCARCINDEX(({start},{},{end}))", start + 1));
        } else {
            segs.push(format!("IFCLINEINDEX(({start},{end}))"));
        }
    }
    let coords: Vec<String> = pts.iter().map(|p| reals(&[p.0, p.1])).collect();
    let list2 = c.s.add(
        "IFCCARTESIANPOINTLIST2D",
        &[format!("({})", coords.join(",")), s(NONE)],
    );
    c.s.add(
        "IFCINDEXEDPOLYCURVE",
        &[r(list2), format!("({})", segs.join(",")), s(".F.")],
    )
}

/// Profile of plan rings (outer CCW, holes CW), shifted by `-o`.
fn profile_def(c: &mut Ctx, rings: &[Vec<(f64, f64)>], o: (f64, f64)) -> u32 {
    let mut curves = vec![];
    for ring in rings {
        let mut pts: Vec<u32> = ring
            .iter()
            .map(|q| c.point2(q.0 - o.0, q.1 - o.1))
            .collect();
        pts.push(pts[0]);
        curves.push(c.s.add("IFCPOLYLINE", &[list(&pts)]));
    }
    if curves.len() == 1 {
        c.s.add(
            "IFCARBITRARYCLOSEDPROFILEDEF",
            &[s(".AREA."), s(NONE), r(curves[0])],
        )
    } else {
        c.s.add(
            "IFCARBITRARYPROFILEDEFWITHVOIDS",
            &[s(".AREA."), s(NONE), r(curves[0]), list(&curves[1..])],
        )
    }
}

fn ring_len(r: &[(f64, f64)]) -> f64 {
    (0..r.len())
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            (b.0 - a.0).hypot(b.1 - a.1)
        })
        .sum()
}

fn ring_area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1)
        .sum::<f64>()
        / 2.0
}

/// Shared ids for slab and space bodies of one storey.
struct Geo {
    storey_pl: u32,
    body_ctx: u32,
    axis_ctx: u32,
    dir_down: u32,
    /// Length, area, volume units.
    units: [u32; 3],
}

/// Quantity: (name, unit 0=length 1=area 2=volume, value).
type Q<'a> = (&'a str, usize, f64);

fn qto(c: &mut Ctx, g: &Geo, kind: &str, id: i64, name: &str, el: u32, q: &[Q]) {
    let ents = ["IFCQUANTITYLENGTH", "IFCQUANTITYAREA", "IFCQUANTITYVOLUME"];
    let ql: Vec<u32> = q
        .iter()
        .map(|(n, k, v)| {
            c.s.add(
                ents[*k],
                &[text(n), s(NONE), r(g.units[*k]), real(*v), s(NONE)],
            )
        })
        .collect();
    let eq = c.rooted("IFCELEMENTQUANTITY", kind, id, name, &[s(NONE), list(&ql)]);
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        &format!("{kind}-rel"),
        id,
        &[list(&[el]), r(eq)],
    );
}

/// The wall's elevation in its frame (x along the axis, z up): bottom edge (`base`, empty — the
/// base line) to the top edge, both kept level past the ends, extruded across `half` either side.
fn elevation_solid(
    c: &mut Ctx,
    top: &[(f64, f64)],
    base: &[(f64, f64)],
    length: f64,
    half: f64,
    at: (f64, f64),
    theta: f64,
) -> u32 {
    let reach = length + 2.0 * half;
    let (first, last) = (top[0], top[top.len() - 1]);
    let bot = |u: f64| {
        if base.is_empty() {
            0.0
        } else {
            ddc_geom::top::height_at(base, u)
        }
    };
    let mut ring = vec![(-reach, bot(-reach))];
    ring.extend(base.iter().copied());
    ring.push((length + reach, bot(length + reach)));
    ring.push((length + reach, last.1));
    // zero heights (no body past an end) stay a hair above the base so the outline is simple
    ring.extend(top.iter().rev().map(|p| (p.0, p.1.max(bot(p.0) + 1e-4))));
    ring.push((-reach, first.1.max(bot(-reach) + 1e-4)));
    ring.dedup();
    let mut pts: Vec<u32> = ring.iter().map(|q| c.point2(q.0, q.1)).collect();
    pts.push(pts[0]);
    let poly = c.s.add("IFCPOLYLINE", &[list(&pts)]);
    let prof = c.s.add(
        "IFCARBITRARYCLOSEDPROFILEDEF",
        &[s(".AREA."), s(NONE), r(poly)],
    );
    // local z (extrusion) = -y of the wall frame, local x = x, so local y = up
    // (a frame turned by `theta` about its origin `at`, for a sector of an arc wall)
    let (sn, cs) = theta.sin_cos();
    let o = c.point3(at.0 - sn * half, at.1 + cs * half, 0.0);
    let zd = c.s.add("IFCDIRECTION", &[reals(&[sn, -cs, 0.0])]);
    let xd = c.s.add("IFCDIRECTION", &[reals(&[cs, sn, 0.0])]);
    let ax = c.s.add("IFCAXIS2PLACEMENT3D", &[r(o), r(zd), r(xd)]);
    c.s.add(
        "IFCEXTRUDEDAREASOLID",
        &[r(prof), r(ax), r(c.dir_z), real(2.0 * half)],
    )
}

/// The part of `ring` with (p - o) . d >= 0.
fn clip_half(ring: &[V2], o: V2, d: V2) -> Vec<V2> {
    let f = |p: V2| (p - o).dot(d);
    let mut out = vec![];
    for i in 0..ring.len() {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        let (fp, fq) = (f(p), f(q));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp >= 0.0) != (fq >= 0.0) {
            out.push(p + (q - p) * (fp / (fp - fq)));
        }
    }
    out
}

fn swept(c: &mut Ctx, g: &Geo, prof: u32, dir: u32, depth: f64) -> u32 {
    let o3 = c.point3(0.0, 0.0, 0.0);
    let sax = c.axis3(o3, None);
    let sol = c.s.add(
        "IFCEXTRUDEDAREASOLID",
        &[r(prof), r(sax), r(dir), real(depth)],
    );
    let body = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[
            r(g.body_ctx),
            text("Body"),
            text("SweptSolid"),
            list(&[sol]),
        ],
    );
    c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[body])],
    )
}

/// Slabs of a level: one extrusion down from the top face, layer set usage along -Z.
fn slabs(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    g: &Geo,
    sets: &BTreeMap<i64, (u32, f64)>,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    let sql = model.sql();
    let rows: Vec<(i64, i64, f64)> = {
        let mut st = sql
            .prepare("SELECT id,stype,offset FROM slab WHERE level=?1 ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([level], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    for (id, stype, offset) in rows {
        let &(set, total) = sets.get(&stype).ok_or("slab type missing")?;
        // plan after shaft and opening cut-outs: holes and notches go in the profile; several pieces
        // (a cut across) — as a mesh body
        let pieces = model.slab_cut_pieces(id).map_err(|e| e.to_string())?;
        let rings: Vec<Vec<(f64, f64)>> = match pieces.as_slice() {
            [(o, hs)] => std::iter::once(o.clone())
                .chain(hs.iter().cloned())
                .collect(),
            _ => model.slab_rings(id).map_err(|e| e.to_string())?,
        };
        if rings.is_empty() || !offset.is_finite() {
            return Err(format!("slab {id}: bad boundary or offset"));
        }
        let slope = model
            .slab_slope(id)
            .map_err(|e| e.to_string())?
            .filter(|p| !p.is_flat());
        let shaped = model.slab_shape(id).map_err(|e| e.to_string())?.is_some() || pieces.len() > 1;
        let (spl, pds) = match slope {
            // shape (points, separation lines): the body is a top/bottom mesh
            _ if shaped => {
                let m = model.slab_mesh(id).map_err(|e| e.to_string())?;
                let elev: f64 = sql
                    .query_row("SELECT elevation FROM level WHERE id=?1", [level], |x| {
                        x.get(0)
                    })
                    .map_err(|e| e.to_string())?;
                let p = c.point3(0.0, 0.0, 0.0);
                let ax = c.axis3(p, None);
                let spl = c.placement(Some(g.storey_pl), ax);
                (spl, mep::body(c, g, &m, elev))
            }
            None => {
                let p = c.point3(0.0, 0.0, offset);
                let ax = c.axis3(p, None);
                let spl = c.placement(Some(g.storey_pl), ax);
                let prof = profile_def(c, &rings, (0.0, 0.0));
                (spl, swept(c, g, prof, g.dir_down, total))
            }
            Some(pl) => {
                // local XY = the sloped top through p0; extruded straight down by the thickness
                let z = ddc_geom::v3(-pl.gx, -pl.gy, 1.0).normalized();
                let x = ddc_geom::v3(1.0, 0.0, pl.gx).normalized();
                let y = z.cross(x);
                let o = ddc_geom::v3(pl.p0.x, pl.p0.y, offset);
                let local: Vec<Vec<(f64, f64)>> = rings
                    .iter()
                    .map(|ring| {
                        ring.iter()
                            .map(|&(px, py)| {
                                let q = ddc_geom::v3(px, py, offset + pl.rise(v2(px, py))) - o;
                                (q.dot(x), q.dot(y))
                            })
                            .collect()
                    })
                    .collect();
                let p = c.point3(o.x, o.y, o.z);
                let (zd, xd) = (dir3(c, z), dir3(c, x));
                let ax = c.s.add("IFCAXIS2PLACEMENT3D", &[r(p), r(zd), r(xd)]);
                let spl = c.placement(Some(g.storey_pl), ax);
                let prof = profile_def(c, &local, (0.0, 0.0));
                let down = c.s.add("IFCDIRECTION", &[reals(&[-x.z, -y.z, -z.z])]);
                (spl, swept(c, g, prof, down, total))
            }
        };
        let slab = c.rooted(
            "IFCSLAB",
            "slab",
            id,
            &format!("Slab {id}"),
            &[s(NONE), r(spl), r(pds), s(NONE), s(".FLOOR.")],
        );
        let usage = c.s.add(
            "IFCMATERIALLAYERSETUSAGE",
            &[r(set), s(".AXIS3."), s(".NEGATIVE."), real(0.0), s(NONE)],
        );
        c.rel(
            "IFCRELASSOCIATESMATERIAL",
            "slab-mat",
            id,
            &[list(&[slab]), r(usage)],
        );
        let q = model.slab_quantities(id).map_err(|e| e.to_string())?;
        let net_area = q.first().map_or(0.0, |l| l.area);
        let surface = q.first().map_or(0.0, |l| l.surface_area);
        let net_vol: f64 = q.iter().map(|l| l.volume).sum();
        let gross = ring_area(&rings[0]);
        let ql = [
            ("Width", 0, total),
            ("Perimeter", 0, ring_len(&rings[0])),
            ("GrossArea", 1, gross),
            ("NetArea", 1, net_area),
            ("GrossVolume", 2, gross * total),
            ("NetVolume", 2, net_vol),
        ];
        qto(c, g, "slab-qto", id, "Qto_SlabBaseQuantities", slab, &ql);
        // plan (projected) area above; the sloped top/bottom face area as its own quantity
        let sl = [("PlanArea", 1, net_area), ("SurfaceArea", 1, surface)];
        qto(
            c,
            g,
            "slab-surface-qto",
            id,
            "DDC_SlabSurfaceQuantities",
            slab,
            &sl,
        );
        out.push(slab);
    }
    Ok(())
}

/// Triangulated face set of `m`, its points mapped by `f` into the placement frame.
fn face_set(
    c: &mut Ctx,
    m: &ddc_geom::mesh::Mesh,
    f: impl Fn(ddc_geom::V3) -> (f64, f64, f64),
) -> u32 {
    let coords = m
        .positions
        .iter()
        .map(|p| {
            let (x, y, z) = f(*p);
            reals(&[x, y, z])
        })
        .collect::<Vec<_>>()
        .join(",");
    let pts = c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({coords})"), s(NONE)]);
    let idx = m
        .triangles
        .iter()
        .map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1))
        .collect::<Vec<_>>()
        .join(",");
    c.s.add(
        "IFCTRIANGULATEDFACESET",
        &[r(pts), s(NONE), s(".T."), format!("({idx})"), s(NONE)],
    )
}

/// Body items of a leaning or tapered wall in its frame (x along the axis, y its left normal):
/// one oblique extrusion per layer piece when every top footprint is the base moved by the
/// lean (exact), else each piece lofted base→top as a face set. Returns the representation type.
fn lean_body(
    c: &mut Ctx,
    (st, bulge): (&Strip, f64),
    l: &Lean,
    base: &[Vec<BulgeRing>],
    top: Option<&Vec<Vec<BulgeRing>>>,
    loc: &dyn Fn(V2) -> (f64, f64),
    out: &mut Vec<u32>,
) -> &'static str {
    use ddc_geom::wall_lean::{is_mapped, loft, paired_polygons, top_bulge_ring};
    let mut pieces = vec![];
    for (li, rings) in base.iter().enumerate() {
        let tl = top.and_then(|t| t.get(li)).filter(|t| t.len() == rings.len());
        for (k, ring) in rings.iter().enumerate() {
            let t = top_bulge_ring(st, bulge, l, ring, tl.map(|t| &t[k]));
            pieces.push(paired_polygons(ring, &t, CHORD_SAG));
        }
    }
    // an oblique extrusion is the lean of a straight wall only (an arc's faces are cones)
    let exact = bulge == 0.0
        && l.top_scale == 1.0
        && pieces.iter().all(|(b, t)| is_mapped(st, 0.0, l, b, t, 1e-9));
    if exact {
        let (sn, cs) = l.angle.sin_cos();
        let dir = c.s.add("IFCDIRECTION", &[reals(&[0.0, sn, cs])]);
        for (b, _) in &pieces {
            let mut pts: Vec<u32> = b
                .iter()
                .map(|q| {
                    let (x, y) = loc(*q);
                    c.point2(x, y)
                })
                .collect();
            pts.push(pts[0]);
            let poly = c.s.add("IFCPOLYLINE", &[list(&pts)]);
            let prof = c.s.add(
                "IFCARBITRARYCLOSEDPROFILEDEF",
                &[s(".AREA."), s(NONE), r(poly)],
            );
            let o3 = c.point3(0.0, 0.0, 0.0);
            let sax = c.axis3(o3, None);
            out.push(c.s.add(
                "IFCEXTRUDEDAREASOLID",
                &[r(prof), r(sax), r(dir), real(st.height / cs)],
            ));
        }
        return "SweptSolid";
    }
    for (b, t) in &pieces {
        let m = loft(b, t, 0.0, st.height);
        out.push(face_set(c, &m, |p| {
            let (x, y) = loc(v2(p.x, p.y));
            (x, y, p.z)
        }));
    }
    "Tessellation"
}

/// Direction entity for a unit vector.
fn dir3(c: &mut Ctx, v: ddc_geom::V3) -> u32 {
    c.s.add("IFCDIRECTION", &[reals(&[v.x, v.y, v.z])])
}

/// Body: one extrusion of `prof` (in the local XY of the frame at `o`, local Z = `z`, local X =
/// `x`) by `depth` along local Z.
fn part_body(
    c: &mut Ctx,
    g: &Geo,
    o: ddc_geom::V3,
    z: ddc_geom::V3,
    x: ddc_geom::V3,
    prof: u32,
    depth: f64,
) -> (u32, u32) {
    let p = c.point3(o.x, o.y, o.z);
    let (zd, xd) = (dir3(c, z), dir3(c, x));
    let ax = c.s.add("IFCAXIS2PLACEMENT3D", &[r(p), r(zd), r(xd)]);
    let pl = c.placement(Some(g.storey_pl), ax);
    let o3 = c.point3(0.0, 0.0, 0.0);
    let sax = c.axis3(o3, None);
    let sol = c.s.add(
        "IFCEXTRUDEDAREASOLID",
        &[r(prof), r(sax), r(c.dir_z), real(depth)],
    );
    let body = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[
            r(g.body_ctx),
            text("Body"),
            text("SweptSolid"),
            list(&[sol]),
        ],
    );
    let pds = c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[body])],
    );
    (pl, pds)
}

/// Rectangle `w × h` with its local origin at (`cx`, 0).
fn rect_prof(c: &mut Ctx, cx: f64, w: f64, h: f64) -> u32 {
    let p2 = c.point2(cx, 0.0);
    let pos2 = c.s.add("IFCAXIS2PLACEMENT2D", &[r(p2), s(NONE)]);
    c.s.add(
        "IFCRECTANGLEPROFILEDEF",
        &[s(".AREA."), s(NONE), r(pos2), real(w), real(h)],
    )
}

/// Curtain walls of a level: IfcCurtainWall (axis only) aggregating one IfcPlate / IfcDoor /
/// IfcWindow per non-empty cell and one IfcMember per mullion segment, all in storey
/// coordinates; quantities from the model's layout.
fn curtain_walls(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    axis_ctx: u32,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    use ddc_geom::curtain::Section;
    use ddc_model::PanelKind;
    let ids: Vec<i64> = {
        let mut st = model
            .sql()
            .prepare("SELECT id FROM curtain_wall WHERE level=?1 ORDER BY id")
            .or_else(db)?;
        let rows = st.query_map([level], |x| x.get(0)).or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    let up = ddc_geom::v3(0.0, 0.0, 1.0);
    let dn = |p: ddc_geom::V3| ddc_geom::v3(p.x, p.y, p.z - elev);
    for id in ids {
        let l = model.curtain_layout(id).map_err(|e| e.to_string())?;
        let sp = &l.spec;
        let (a, b) = (v2(sp.a.0, sp.a.1), v2(sp.b.0, sp.b.1));
        let len = (b - a).len();
        let d = (b - a) * (1.0 / len);
        let n = v2(-d.y, d.x);
        let loc = |p: V2| ((p - a).dot(d), (p - a).dot(n));
        let p = c.point3(a.x, a.y, sp.base_offset);
        let xd = c.s.add("IFCDIRECTION", &[reals(&[d.x, d.y, 0.0])]);
        let ax = c.axis3(p, Some(xd));
        let cpl = c.placement(Some(g.storey_pl), ax);
        let line = if sp.bulge != 0.0 {
            let arc = ArcAxis::new(a, b, sp.bulge, Tol::default())
                .map_err(|e| format!("curtain wall {id}: arc {e:?}"))?;
            let (cx, cy) = loc(arc.center);
            let cp = c.point2(cx, cy);
            let cpos = c.s.add("IFCAXIS2PLACEMENT2D", &[r(cp), s(NONE)]);
            let circle = c.s.add("IFCCIRCLE", &[r(cpos), real(arc.radius)]);
            let (p0, p1) = (c.point2(0.0, 0.0), c.point2(len, 0.0));
            let sense = if arc.sweep > 0.0 { ".T." } else { ".F." };
            c.s.add(
                "IFCTRIMMEDCURVE",
                &[
                    r(circle),
                    list(&[p0]),
                    list(&[p1]),
                    s(sense),
                    s(".CARTESIAN."),
                ],
            )
        } else {
            let (p0, p1) = (c.point2(0.0, 0.0), c.point2(len, 0.0));
            c.s.add("IFCPOLYLINE", &[list(&[p0, p1])])
        };
        let axis = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[r(axis_ctx), text("Axis"), text("Curve2D"), list(&[line])],
        );
        let pds = c.s.add(
            "IFCPRODUCTDEFINITIONSHAPE",
            &[s(NONE), s(NONE), list(&[axis])],
        );
        let cw = c.rooted(
            "IFCCURTAINWALL",
            "curtain",
            id,
            &sp.name,
            &[s(NONE), r(cpl), r(pds), s(NONE), s(NONE)],
        );
        let mut parts = vec![];
        for (cell, panel) in l.layout.cells.iter().zip(&l.panels) {
            if panel.kind == PanelKind::Empty {
                continue;
            }
            // GlobalId from (wall, cell i, j): stable across exports, no id arithmetic overlap
            let cell_kind = |kind: &str| format!("{kind}:{}:{}", cell.i, cell.j);
            let along = cell.net[1] - cell.net[0];
            let w = along.len();
            let h = cell.net[3].z - cell.net[0].z;
            if w <= 1e-9 || h <= 1e-9 {
                continue;
            }
            let side = along * (1.0 / w);
            let t = 0.025;
            let door = panel.kind == PanelKind::Door;
            // doors/windows: local X runs against the axis when they open to its right
            let flip = (door || panel.kind == PanelKind::Window) && panel.swing_side;
            let (o, x, cx) = if flip {
                (dn(cell.net[1]), -side, w / 2.0)
            } else {
                (dn(cell.net[0]), side, w / 2.0)
            };
            let clipped = (cell.net_area - w * h).abs() > 1e-12;
            if clipped && cell.net_area <= 1e-12 {
                continue;
            }
            let (ppl, pds) = if clipped {
                // panel (door/window too) under a sloped top: its outline in the wall plane,
                // extruded across it, as in the mesh
                let nrm = up.cross(side);
                let base = cell.corners[0] - up * (cell.corners[0].z - l.z0);
                let mut pts: Vec<u32> = cell.outline.iter().map(|q| c.point2(q.0, q.1)).collect();
                pts.push(pts[0]);
                let poly = c.s.add("IFCPOLYLINE", &[list(&pts)]);
                let prof = c.s.add(
                    "IFCARBITRARYCLOSEDPROFILEDEF",
                    &[s(".AREA."), s(NONE), r(poly)],
                );
                part_body(c, g, dn(base + nrm * (t / 2.0)), -nrm, side, prof, t)
            } else {
                let prof = rect_prof(c, cx, w, t);
                part_body(c, g, o, up, x, prof, h)
            };
            let label = if panel.name.is_empty() {
                format!("Panel {}-{}", cell.i, cell.j)
            } else {
                panel.name.clone()
            };
            let ent = match panel.kind {
                PanelKind::Door | PanelKind::Window => {
                    let op = match (door, panel.swing_side == panel.swing_hinge) {
                        (false, _) => NONE,
                        (true, true) => ".SINGLE_SWING_LEFT.",
                        (true, false) => ".SINGLE_SWING_RIGHT.",
                    };
                    let (ty, kind, pre) = if door {
                        ("IFCDOOR", "curtain-door", ".DOOR.")
                    } else {
                        ("IFCWINDOW", "curtain-window", ".WINDOW.")
                    };
                    c.rooted(
                        ty,
                        &cell_kind(kind),
                        id,
                        &label,
                        &[
                            s(NONE),
                            r(ppl),
                            r(pds),
                            s(NONE),
                            real(h),
                            real(w),
                            s(pre),
                            s(op),
                            s(NONE),
                        ],
                    )
                }
                _ => {
                    let desc = if panel.kind == PanelKind::Solid {
                        "solid"
                    } else {
                        "glazed"
                    };
                    let pid = c.g.get(&cell_kind("curtain-plate"), id);
                    let mut a = vec![pid, NONE.into(), text(&label), text(desc)];
                    a.extend_from_slice(&[s(NONE), r(ppl), r(pds), s(NONE), s(".CURTAIN_PANEL.")]);
                    c.s.add("IFCPLATE", &a)
                }
            };
            let ql = [
                ("Width", 0, w),
                ("Height", 0, h),
                ("Area", 1, cell.net_area),
            ];
            qto(
                c,
                g,
                &cell_kind("curtain-panel-qto"),
                id,
                "Qto_PlateBaseQuantities",
                ent,
                &ql,
            );
            parts.push(ent);
        }
        for bar in &l.layout.bars {
            let sec = bar.section;
            let prof = match sec {
                Section::None => continue,
                Section::Rect { w, d } => rect_prof(c, 0.0, w, d),
                Section::Circle { d } => {
                    let p2 = c.point2(0.0, 0.0);
                    let pos2 = c.s.add("IFCAXIS2PLACEMENT2D", &[r(p2), s(NONE)]);
                    c.s.add(
                        "IFCCIRCLEPROFILEDEF",
                        &[s(".AREA."), s(NONE), r(pos2), real(d / 2.0)],
                    )
                }
            };
            let dir = bar.b - bar.a;
            let blen = dir.len();
            if blen <= 1e-9 {
                continue;
            }
            let z = dir * (1.0 / blen);
            let x = bar.normal.cross(z).normalized();
            let o = dn(bar.a + bar.inward * (sec.width() / 2.0));
            let (mpl, pds) = part_body(c, g, o, z, x, prof, blen);
            let bar_kind = |kind: &str| {
                let dir = if bar.vertical { 'v' } else { 'h' };
                format!("{kind}:{dir}:{}:{}", bar.line, bar.seg)
            };
            let mid = c.g.get(&bar_kind("curtain-member"), id);
            let mut a = vec![
                mid,
                NONE.into(),
                text(if bar.vertical { "Mullion" } else { "Transom" }),
                text(if bar.border { "border" } else { "interior" }),
            ];
            a.extend_from_slice(&[s(NONE), r(mpl), r(pds), s(NONE), s(".MULLION.")]);
            let m = c.s.add("IFCMEMBER", &a);
            qto(
                c,
                g,
                &bar_kind("curtain-member-qto"),
                id,
                "Qto_MemberBaseQuantities",
                m,
                &[("Length", 0, blen)],
            );
            parts.push(m);
        }
        if !parts.is_empty() {
            c.rel(
                "IFCRELAGGREGATES",
                "agg-curtain",
                id,
                &[r(cw), list(&parts)],
            );
        }
        let q = l.quantities();
        let ql = [
            ("Length", 0, ddc_geom::arc::axis_length(a, b, sp.bulge)),
            ("Height", 0, sp.height),
            ("GrossSideArea", 1, q.gross_area),
            ("NetSideArea", 1, q.glass_area),
        ];
        qto(
            c,
            g,
            "curtain-qto",
            id,
            "Qto_CurtainWallQuantities",
            cw,
            &ql,
        );
        out.push(cw);
    }
    Ok(())
}

/// Rooms of a level as IfcSpace placed at the room point; enclosed rooms get a body.
fn spaces(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    g: &Geo,
    wall_ent: &BTreeMap<i64, u32>,
    wall_ext: &BTreeMap<i64, bool>,
    seps: &BTreeMap<i64, u32>,
    room_spaces: &mut BTreeMap<i64, u32>,
) -> Result<Vec<u32>, String> {
    type Row = (i64, String, String, f64, f64, f64);
    let rows: Vec<Row> = {
        let mut st = model
            .sql()
            .prepare("SELECT id,name,number,px,py,height FROM room WHERE level=?1 ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([level], |x| {
                Ok((
                    x.get(0)?,
                    x.get(1)?,
                    x.get(2)?,
                    x.get(3)?,
                    x.get(4)?,
                    x.get(5)?,
                ))
            })
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    let mut out = vec![];
    for (id, name, number, px, py, h) in rows {
        let q = model.room_quantities(id).map_err(|e| e.to_string())?;
        let p = c.point3(px, py, 0.0);
        let ax = c.axis3(p, None);
        let spl = c.placement(Some(g.storey_pl), ax);
        let pds = match &q {
            Some(q) => {
                let prof = profile_def(c, &q.rings, (px, py));
                r(swept(c, g, prof, c.dir_z, h))
            }
            None => s(NONE),
        };
        let sp = c.rooted(
            "IFCSPACE",
            "space",
            id,
            &number,
            &[
                s(NONE),
                r(spl),
                pds,
                text(&name),
                s(".ELEMENT."),
                s(".SPACE."),
                s(NONE),
            ],
        );
        let mut ql = vec![("Height", 0, h)];
        if let Some(q) = q {
            ql.extend([
                ("GrossPerimeter", 0, q.perimeter),
                ("NetPerimeter", 0, q.perimeter),
                ("GrossFloorArea", 1, q.area),
                ("NetFloorArea", 1, q.area),
                ("GrossVolume", 2, q.volume),
                ("NetVolume", 2, q.volume),
            ]);
        }
        qto(c, g, "space-qto", id, "Qto_SpaceBaseQuantities", sp, &ql);
        // second-level boundaries: bounding walls (physical) and separators (virtual)
        for (k, b) in model
            .room_boundaries(id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .enumerate()
        {
            let (el, kind, ext) = match b {
                Boundary::Wall(w) => (
                    wall_ent.get(&w),
                    ".PHYSICAL.",
                    wall_ext.get(&w).copied().unwrap_or(false),
                ),
                Boundary::Sep(x) => (seps.get(&x), ".VIRTUAL.", false),
            };
            if let Some(&el) = el {
                c.rel(
                    "IFCRELSPACEBOUNDARY",
                    "space-boundary",
                    id * 1_000_000 + k as i64,
                    &[r(sp), r(el), s(NONE), s(kind), s(if ext { ".EXTERNAL." } else { ".INTERNAL." })],
                );
            }
        }
        room_spaces.insert(id, sp);
        out.push(sp);
    }
    Ok(out)
}

/// Room separators of a level as IfcVirtualElement with a two-point axis curve in storey
/// coordinates; contained in the storey like walls. Returns separator id -> element.
fn separators(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    g: &Geo,
    axis_ctx: u32,
    elems: &mut Vec<u32>,
) -> Result<BTreeMap<i64, u32>, String> {
    let mut out = BTreeMap::new();
    for (id, sp) in model.room_seps(level).map_err(|e| e.to_string())? {
        let ax = c.axis3(c.origin, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let (p0, p1) = (c.point2(sp.a.0, sp.a.1), c.point2(sp.b.0, sp.b.1));
        let line = c.s.add("IFCPOLYLINE", &[list(&[p0, p1])]);
        let rep = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[r(axis_ctx), text("Axis"), text("Curve2D"), list(&[line])],
        );
        let pds = c.s.add(
            "IFCPRODUCTDEFINITIONSHAPE",
            &[s(NONE), s(NONE), list(&[rep])],
        );
        let el = c.rooted(
            "IFCVIRTUALELEMENT",
            "separator",
            id,
            &format!("Room separator {id}"),
            &[s(NONE), r(pl), r(pds), s(NONE), s(NONE)],
        );
        elems.push(el);
        out.insert(id, el);
    }
    Ok(out)
}

pub fn write_ifc_file(model: &Model, path: &Path) -> Result<(), String> {
    std::fs::write(path, export_ifc(model)?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ifc::{check_step, validate_ifc};
    use crate::ifc::step::guid;
    use ddc_geom::layered::{join, Strip};
    use ddc_model::{LayerSpec, OpeningSpec, WallSpec};

    fn model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "ext",
                &[
                    LayerSpec {
                        material: "brick".into(),
                        thickness: 0.2,
                    },
                    LayerSpec {
                        material: "insulation".into(),
                        thickness: 0.1,
                    },
                ],
            )
            .unwrap();
        let pts = [(0., 0.), (6., 0.), (6., 4.), (0., 4.), (0., 0.)];
        let mut first = 0;
        for k in 0..4 {
            let w = m
                .add_wall(&WallSpec {
                    a: pts[k],
                    b: pts[k + 1],
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap();
            if k == 0 {
                first = w;
            }
        }
        m.add_opening(
            first,
            &OpeningSpec {
                along: 1.5,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .unwrap();
        m
    }

    fn count(t: &str, name: &str) -> usize {
        t.matches(&format!("={name}(")).count()
    }

    fn net_sum(t: &str) -> f64 {
        t.lines()
            .filter(|l| l.contains("IFCQUANTITYVOLUME('NetVolume'"))
            .map(|l| l.rsplit(',').nth(1).unwrap().parse::<f64>().unwrap())
            .sum()
    }

    /// Storey elevations are the model's own levels (the sum of the storey heights below), not
    /// the index times one height: a tall ground level shifts every storey above it.
    #[test]
    fn storey_elevations_follow_the_levels_not_an_index() {
        let mut m = Model::open(":memory:").unwrap();
        for (n, z) in [("L1", 0.0), ("L2", 6.5), ("L3", 10.5)] {
            m.add_level(n, z).unwrap();
        }
        let t = export_ifc(&m).unwrap();
        let elev: Vec<f64> = t
            .lines()
            .filter(|l| l.contains("=IFCBUILDINGSTOREY("))
            .map(|l| l.trim_end_matches(");").rsplit(',').next().unwrap().trim_end_matches('.').parse::<f64>().unwrap())
            .collect();
        assert_eq!(elev.len(), 3, "{t}");
        let k = elev[1] / 6.5;
        assert!(elev[0].abs() < 1e-9);
        assert!((elev[1] - 6.5 * k).abs() < 1e-6 && (elev[2] - 10.5 * k).abs() < 1e-6, "{elev:?}");
        assert!((elev[2] - elev[1] - 4.0 * k).abs() < 1e-6, "an index x height would give equal steps");
    }

    #[test]
    fn four_walls_and_door() {
        let m = model();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCWALL"), 4);
        assert_eq!(count(&t, "IFCOPENINGELEMENT"), 1);
        assert_eq!(count(&t, "IFCRELVOIDSELEMENT"), 1);
        assert_eq!(count(&t, "IFCBUILDINGSTOREY"), 1);
        assert_eq!(count(&t, "IFCMATERIALLAYERSETUSAGE"), 4);
        assert!(t.contains("FILE_SCHEMA(('IFC4X3_ADD2'))"));
        let model_vol: f64 = m.material_volumes().unwrap().iter().map(|v| v.1).sum();
        assert!(
            (net_sum(&t) - model_vol).abs() <= 1e-9,
            "{} vs {model_vol}",
            net_sum(&t)
        );
        check_step(&t).unwrap();
    }

    /// `model()` with every wall leaning inward by 7 degrees (and tapered by `top_scale`).
    fn lean_model(top_scale: f64) -> Model {
        let mut m = model();
        let ids: Vec<i64> = {
            let mut q = m.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
            let rows = q.query_map([], |x| x.get(0)).unwrap();
            rows.collect::<Result<_, _>>().unwrap()
        };
        for id in ids {
            m.set_wall_lean(id, Lean::new(7f64.to_radians(), top_scale)).unwrap();
        }
        m
    }

    #[test]
    fn leaning_walls_export_exact_and_tapered_ones_tessellated() {
        use crate::ifc::validate::validate_ifc;
        // a free wall leaning only: its top is the base moved by the lean, one oblique extrusion
        let mut one = Model::open(":memory:").unwrap();
        let lv = one.add_level("L1", 0.0).unwrap();
        let ty = one
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        let w = one
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ty,
                offset0: None,
            })
            .unwrap();
        one.set_wall_lean(w, Lean::new(7f64.to_radians(), 1.0)).unwrap();
        // cases: (model, the body is one exact oblique extrusion)
        for (m, exact) in [(one, true), (lean_model(1.0), false), (lean_model(0.6), false)] {
            let t = export_ifc(&m).unwrap();
            check_step(&t).unwrap();
            let (sn, cs) = 7f64.to_radians().sin_cos();
            let oblique = format!("=IFCDIRECTION({})", reals(&[0.0, sn, cs]));
            assert!(t.contains(&oblique), "lean direction");
            let sets = count(&t, "IFCTRIANGULATEDFACESET");
            if exact {
                assert_eq!(sets, 0);
                assert!(t.contains(&format!(",{})", real(3.0 / cs))), "oblique depth");
            } else {
                // corners joined at the top: lofted pieces; the door's cutter is the model's
                // box normal to the surface
                assert_eq!(count(&t, "IFCWALL"), 4);
                assert_eq!(count(&t, "IFCOPENINGELEMENT"), 1);
                assert_eq!(count(&t, "IFCDOOR"), 1);
                assert!(sets > 4, "lofted pieces and the cutter");
            }
            let model_vol: f64 = m.material_volumes().unwrap().iter().map(|v| v.1).sum();
            assert!((net_sum(&t) - model_vol).abs() <= 1e-9);
            let rep = validate_ifc(t.as_bytes());
            assert!(rep.is_clean(), "{:?}", rep.errors);
            let opts = IfcOptions {
                schema: IfcSchema::Ifc2x3,
                timestamp: Some("2026-01-01T00:00:00".into()),
                ..Default::default()
            };
            let (old, _) = export_ifc_with(&m, &opts).unwrap();
            assert_eq!(count(&old, "IFCTRIANGULATEDFACESET"), 0);
            let rep = validate_ifc(old.as_bytes());
            assert!(rep.is_clean(), "{:?}", rep.errors);
        }
    }

    fn door_window_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "ext",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.25,
                }],
            )
            .unwrap();
        let mut ids = vec![];
        for y in [0.0, 5.0] {
            ids.push(
                m.add_wall(&WallSpec {
                    a: (0.0, y),
                    b: (8.0, y),
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap(),
            );
        }
        for (w, kind, along, sill, h) in [(0, "door", 1.0, 0.0, 2.1), (1, "window", 3.0, 0.9, 1.2)]
        {
            m.add_opening(
                ids[w],
                &OpeningSpec {
                    along,
                    width: 1.0,
                    sill,
                    height: h,
                    kind: kind.into(),
                },
            )
            .unwrap();
        }
        m
    }

    #[test]
    fn sloped_curtain_door_is_clipped_like_the_mesh() {
        use ddc_model::{CurtainSpec, GridLines, MullionSection, PanelKind, PanelSpec};
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
        let cw = m
            .add_curtain_wall(&CurtainSpec {
                level: lv,
                name: "Gable".into(),
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                u: GridLines::Spacing(1.5),
                v: GridLines::Spacing(1.0),
                interior: sec,
                border: sec,
                lines: Default::default(),
                top: vec![(0.0, 2.0), (3.0, 3.0), (6.0, 2.0)],
            })
            .unwrap();
        let door = PanelSpec {
            kind: PanelKind::Door,
            name: "Door".into(),
            swing_side: false,
            swing_hinge: false,
        };
        m.set_curtain_panel(cw, 0, 2, &door).unwrap();
        let l = m.curtain_layout(cw).unwrap();
        let cell = l
            .layout
            .cells
            .iter()
            .find(|c| (c.i, c.j) == (0, 2))
            .unwrap();
        let w = (cell.net[1] - cell.net[0]).len();
        let h = cell.net[3].z - cell.net[0].z;
        assert!(cell.net_area > 1e-6 && cell.net_area < w * h - 1e-6);
        let clipped = l
            .layout
            .cells
            .iter()
            .zip(&l.panels)
            .filter(|(c, p)| {
                p.kind != PanelKind::Empty
                    && c.net_area > 1e-12
                    && (c.net_area - (c.net[1] - c.net[0]).len() * (c.net[3].z - c.net[0].z)).abs()
                        > 1e-12
            })
            .count();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCDOOR"), 1);
        assert_eq!(count(&t, "IFCARBITRARYCLOSEDPROFILEDEF"), clipped);
        // door area quantity is the clipped one
        let area = format!("{}", cell.net_area);
        assert!(
            t.lines()
                .any(|l| l.contains("IFCQUANTITYAREA('Area'") && l.contains(&area[..6])),
            "{area}"
        );
    }

    #[test]
    fn door_and_window_fill_openings() {
        let t = export_ifc(&door_window_model()).unwrap();
        assert_eq!(count(&t, "IFCWALL"), 2);
        assert_eq!(count(&t, "IFCDOOR"), 1);
        assert_eq!(count(&t, "IFCWINDOW"), 1);
        assert_eq!(count(&t, "IFCRELFILLSELEMENT"), 2);
        assert_eq!(count(&t, "IFCRELVOIDSELEMENT"), 2);
        assert!(t.contains("IFCRECTANGLEPROFILEDEF(.AREA.,$,#"));
        let depth = format!(",1.,{})", 0.25 + 2.0 * OVERCUT);
        assert!(
            t.matches(&depth).count() >= 2,
            "opening depth must exceed wall thickness"
        );
        assert!(t.contains("Qto_DoorBaseQuantities"));
        assert!(t.contains("Qto_WindowBaseQuantities"));
        let areas: Vec<&str> = t
            .lines()
            .filter(|l| l.contains("IFCQUANTITYAREA('Area'"))
            .collect();
        assert_eq!(areas.len(), 2);
        assert!(
            areas[0].contains(",2.1,") && areas[1].contains(",1.2,"),
            "{areas:?}"
        );
        let door = t.lines().find(|l| l.contains("=IFCDOOR(")).unwrap();
        assert!(
            door.contains(",2.1,1.,.DOOR.,.SINGLE_SWING_LEFT.,$)"),
            "{door}"
        );
        let win = t.lines().find(|l| l.contains("=IFCWINDOW(")).unwrap();
        assert!(win.contains(",1.2,1.,.WINDOW.,$,$)"), "{win}");
        // 13 attributes each: 4 rooted + 9
        for l in [door, win] {
            let body = &l[l.find("(").unwrap() + 1..l.len() - 2];
            assert_eq!(top_level_args(body), 13, "{l}");
        }
        let fill = t
            .lines()
            .find(|l| l.contains("=IFCRELFILLSELEMENT("))
            .unwrap();
        assert_eq!(
            top_level_args(&fill[fill.find("(").unwrap() + 1..fill.len() - 2]),
            6
        );
        // contained in the storey together with the walls
        assert!(t.lines().any(
            |l| l.contains("IFCRELCONTAINEDINSPATIALSTRUCTURE") && l.matches('#').count() == 6
        ));
        check_step(&t).unwrap();
        assert_eq!(t, export_ifc(&door_window_model()).unwrap());
    }

    fn top_level_args(s: &str) -> usize {
        let (mut depth, mut n, mut q) = (0i32, 1, false);
        for ch in s.chars() {
            match ch {
                '\'' => q = !q,
                '(' if !q => depth += 1,
                ')' if !q => depth -= 1,
                ',' if !q && depth == 0 => n += 1,
                _ => {}
            }
        }
        n
    }

    #[test]
    fn multi_piece_layers_give_one_solid_per_piece() {
        // a tee: the stem's core cuts the host's finish into two pieces
        let mk = |a: V2, b: V2| Strip {
            a,
            b,
            layers: vec![
                Layer::with_priority(0.02, 5),
                Layer::with_priority(0.2, 1),
                Layer::with_priority(0.02, 5),
            ],
            offset0: -0.12,
            base: 0.0,
            height: 3.0,
        };
        let hi = mk(v2(0.0, 0.0), v2(10.0, 0.0));
        let lo = mk(v2(5.0, -4.0), v2(5.0, 0.0));
        let strips = [hi, lo];
        let j = join(&strips, Tol::default()).unwrap();
        let pieces: usize = j.pieces[0].iter().map(|l| l.len()).sum();
        assert_eq!(pieces, 4);
        let rings = outlines(&strips[0], &j.pieces, 0).unwrap();
        assert_eq!(rings.len(), pieces);
        let area: f64 = rings
            .iter()
            .map(|r| {
                (0..r.len())
                    .map(|i| r[i].0 * r[(i + 1) % r.len()].1 - r[(i + 1) % r.len()].0 * r[i].1)
                    .sum::<f64>()
                    / 2.0
            })
            .sum();
        assert!((area - (10.0 * 0.24 - 0.02 * 0.2)).abs() < 1e-9, "{area}");
    }

    #[test]
    fn deterministic() {
        assert_eq!(export_ifc(&model()).unwrap(), export_ifc(&model()).unwrap());
    }

    #[test]
    fn check_detects_broken_ref_and_line() {
        let t = export_ifc(&model()).unwrap();
        let bad = t.replacen("IFCDIRECTION((0.,0.,1.))", "IFCDIRECTION(#99999)", 1);
        assert!(check_step(&bad).is_err());
        assert!(check_step(&t.replacen(");\n#", ")\n#", 1)).is_err());
    }

    #[test]
    fn guid_shape() {
        let g = guid("wall", 7);
        assert_eq!(g.len(), 24);
        assert_ne!(g, guid("wall", 8));
    }

    /// Needs a local validation venv: DDC_IFC_VAL_DIR holds `venv/Scripts/python.exe` and
    /// `ifcval.py`; run with --ignored. Skips when the variable or the files are missing.
    #[test]
    #[ignore = "local resource: external IFC validator (DDC_IFC_VAL_DIR)"]
    fn external_validation() {
        let Some(root) = std::env::var_os("DDC_IFC_VAL_DIR") else {
            eprintln!("skip: DDC_IFC_VAL_DIR not set");
            return;
        };
        let root = std::path::PathBuf::from(root);
        let (py, script) = (root.join("venv/Scripts/python.exe"), root.join("ifcval.py"));
        let (py, script) = (py.as_path(), script.as_path());
        if !py.exists() || !script.exists() {
            eprintln!("skip: validator not found under {}", root.display());
            return;
        }
        let dir = std::env::temp_dir().join("ddc_ifc_val");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, m) in [
            ("walls.ifc", model()),
            ("doors.ifc", door_window_model()),
            ("leaning.ifc", lean_model(1.0)),
            ("tapered.ifc", lean_model(0.6)),
            ("slabs.ifc", crate::ifc::import::tests::slab_room_model()),
            ("swing.ifc", crate::ifc::import::tests::swing_model()),
            (
                "boundaries.ifc",
                crate::ifc::import::tests::separated_model(),
            ),
        ] {
            let f = dir.join(name);
            write_ifc_file(&m, &f).unwrap();
            let out = std::process::Command::new(py)
                .arg(script)
                .arg(f.to_string_lossy().replace('\\', "/"))
                .output()
                .unwrap();
            let o = String::from_utf8_lossy(&out.stdout);
            println!("{o}");
            assert!(
                o.contains("\"errors\": 0"),
                "{o}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn non_finite_length_is_export_error_and_writes_nothing() {
        for bad in [f64::NAN, f64::INFINITY] {
            let mut m = Model::open(":memory:").unwrap();
            let lv = m.add_level("L", 0.0).unwrap();
            let t = m
                .add_wall_type(
                    "t",
                    &[LayerSpec {
                        material: "c".into(),
                        thickness: 0.2,
                    }],
                )
                .unwrap();
            let spec = WallSpec {
                a: (0.0, 0.0),
                b: (bad, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            };
            if m.add_wall(&spec).is_err() {
                continue; // the model already refuses it: nothing to export
            }
            let err = export_ifc(&m).unwrap_err();
            assert!(err.contains("non-finite") || err.contains("zero"), "{err}");
            let f = std::env::temp_dir().join("ddc_nan_wall.ifc");
            let _ = std::fs::remove_file(&f);
            assert!(write_ifc_file(&m, &f).is_err());
            assert!(!f.exists());
        }
    }

    #[test]
    fn non_finite_level_elevation_is_export_error() {
        let mut m = Model::open(":memory:").unwrap();
        if m.add_level("L", f64::NAN).is_err() {
            return;
        }
        let err = export_ifc(&m).unwrap_err();
        assert!(err.contains("non-finite"), "{err}");
    }
    #[test]
    fn skylight_is_a_window_filling_an_opening_in_its_roof() {
        use ddc_model::{RoofKind, RoofSpec, SkylightSpec};
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let st = m
            .add_slab_type(
                "roof",
                &[LayerSpec {
                    material: "deck".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let roof = m
            .add_roof(&RoofSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                overhang: 0.5,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)],
                    slopes: vec![Some(30.0); 4],
                    mansard: None,
                },
            })
            .unwrap();
        m.add_skylight(&SkylightSpec {
            roof,
            center: (5.0, 1.0),
            width: 1.0,
            length: 1.2,
            curb: 0.15,
        })
        .unwrap();
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCROOF"), 1);
        assert_eq!(count(&t, "IFCWINDOW"), 1);
        assert_eq!(count(&t, "IFCOPENINGELEMENT"), 1);
        assert_eq!(count(&t, "IFCRELVOIDSELEMENT"), 1);
        assert_eq!(count(&t, "IFCRELFILLSELEMENT"), 1);
        // empty point lists do not occur
        assert!(!t.contains("IFCCARTESIANPOINTLIST3D(()"));
    }

    #[test]
    fn leaning_arc_wall_exports_lofted_cones_and_model_volume() {
        use crate::ifc::validate::validate_ifc;
        let mut m = model();
        let first: i64 = m
            .sql()
            .query_row("SELECT MIN(id) FROM wall", [], |x| x.get(0))
            .unwrap();
        m.set_wall_bulge(first, 0.2).unwrap();
        m.set_wall_lean(first, Lean::new(6f64.to_radians(), 0.8)).unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCWALL"), 4);
        assert_eq!(count(&t, "IFCDOOR"), 1);
        assert!(t.contains("'Body','Tessellation'"));
        let model_vol: f64 = m.material_volumes().unwrap().iter().map(|v| v.1).sum();
        assert!((net_sum(&t) - model_vol).abs() <= 1e-9, "{} vs {model_vol}", net_sum(&t));
        check_step(&t).unwrap();
        let rep = validate_ifc(t.as_bytes());
        assert!(rep.is_clean(), "{:?}", rep.errors);
        let opts = IfcOptions {
            schema: IfcSchema::Ifc2x3,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        };
        let (old, _) = export_ifc_with(&m, &opts).unwrap();
        assert_eq!(count(&old, "IFCTRIANGULATEDFACESET"), 0);
        let rep = validate_ifc(old.as_bytes());
        assert!(rep.is_clean(), "{:?}", rep.errors);
    }

    /// One spline wall of two layers with a door on one of its pieces; `rational`: the axis is
    /// a quarter circle (weighted) instead of a cubic through points.
    fn spline_model(rational: bool) -> (Model, i64) {
        use ddc_geom::nurbs::Curve;
        use ddc_geom::v3;
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let layers = [("brick", 0.2), ("insulation", 0.1)].map(|(n, t)| LayerSpec {
            material: n.into(),
            thickness: t,
        });
        let t = m.add_wall_type("ext", &layers).unwrap();
        let pts = [(0.0, 0.0), (4.0, 2.0), (8.0, -1.0), (12.0, 1.0)];
        let mut s = ddc_model::SplineWallSpec::through(lv, t, &pts, 3.0).unwrap();
        // a 2 cm fit, as callers set: at the 1 mm default this S axis folds into arcs under 0.8 m
        s.tol = 0.02;
        if rational {
            let (x, y) = (v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
            s.axis = Curve::arc(v3(0.0, 0.0, 0.0), x, y, 5.0, 0.0, 1.5).unwrap();
        }
        let sid = m.add_spline_wall(&s).unwrap();
        // an opening sits inside one piece of the arc chain: the longest one, centred
        let pieces = m.spline_wall_pieces(sid).unwrap();
        let lens: Vec<f64> = pieces.iter().map(|&w| m.wall_length(w).unwrap()).collect();
        let k = (0..pieces.len()).max_by(|&a, &b| lens[a].total_cmp(&lens[b])).unwrap();
        assert!(lens[k] > 1.0, "{lens:?}");
        let door = OpeningSpec {
            along: (lens[k] - 0.9) / 2.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        m.add_opening(pieces[k], &door).unwrap();
        (m, sid)
    }

    #[test]
    fn spline_wall_exports_as_one_wall_with_its_exact_axis() {
        use crate::ifc::validate::validate_ifc;
        for rational in [false, true] {
            let (m, sid) = spline_model(rational);
            assert!(rational || m.spline_wall_pieces(sid).unwrap().len() >= 2);
            let t = export_ifc(&m).unwrap();
            assert_eq!(count(&t, "IFCWALL"), 1, "one wall, not one per piece");
            assert_eq!(count(&t, "IFCMATERIALLAYERSETUSAGE"), 1);
            assert_eq!(count(&t, "IFCELEMENTQUANTITY") - count(&t, "IFCDOOR"), 1);
            let (plain, weighted) = (
                count(&t, "IFCBSPLINECURVEWITHKNOTS"),
                count(&t, "IFCRATIONALBSPLINECURVEWITHKNOTS"),
            );
            assert_eq!((plain, weighted), if rational { (0, 1) } else { (1, 0) });
            assert_eq!(count(&t, "IFCTRIMMEDCURVE"), 0, "no piece axes");
            assert_eq!(count(&t, "IFCINDEXEDPOLYCURVE"), 0, "no piece bodies");
            assert_eq!(count(&t, "IFCARBITRARYCLOSEDPROFILEDEF"), 2, "a body per layer");
            // the door voids the spline wall
            assert_eq!(count(&t, "IFCOPENINGELEMENT"), 1);
            assert_eq!(count(&t, "IFCDOOR"), 1);
            let wall = t.lines().find(|l| l.contains("=IFCWALL(")).unwrap();
            let wid = wall.split('=').next().unwrap();
            let void = t.lines().find(|l| l.contains("=IFCRELVOIDSELEMENT(")).unwrap();
            assert!(void.contains(&format!("{wid},")), "{void}");
            // the model's exact quantities
            let q = m.spline_wall_quantities(sid).unwrap();
            let net: f64 = q.iter().map(|x| x.volume).sum();
            assert!((net_sum(&t) - net).abs() <= 1e-9, "{} vs {net}", net_sum(&t));
            let len = ddc_geom::wall_spline::SplineAxis::new(m.spline_wall(sid).unwrap().axis)
                .unwrap()
                .length();
            let qlen = t
                .lines()
                .find(|l| l.contains("IFCQUANTITYLENGTH('Length'"))
                .and_then(|l| l.rsplit(',').nth(1)?.parse::<f64>().ok())
                .unwrap();
            assert!((qlen - len).abs() <= 1e-9, "{qlen} vs {len}");
            check_step(&t).unwrap();
            let rep = validate_ifc(t.as_bytes());
            assert!(rep.is_clean(), "{:?}", rep.errors);
            // IFC2X3 has no B-spline with knots: the axis becomes a polyline along the curve
            let opts = IfcOptions {
                schema: IfcSchema::Ifc2x3,
                timestamp: Some("2026-01-01T00:00:00".into()),
                ..Default::default()
            };
            let (old, _) = export_ifc_with(&m, &opts).unwrap();
            assert_eq!(count(&old, "IFCBSPLINECURVEWITHKNOTS"), 0);
            assert_eq!(count(&old, "IFCRATIONALBSPLINECURVEWITHKNOTS"), 0);
            let axis = old.lines().find(|l| l.contains("'Axis','Curve2D'")).unwrap();
            let line = axis.split("'Curve2D',(").nth(1).unwrap().trim_end_matches("));");
            let poly = old.lines().find(|l| l.starts_with(&format!("{line}="))).unwrap();
            assert!(poly.contains("=IFCPOLYLINE(") && poly.matches('#').count() > 8, "{poly}");
            let rep = validate_ifc(old.as_bytes());
            assert!(rep.is_clean(), "{:?}", rep.errors);
        }
    }

    /// STEP entities of an exported file: id -> (NAME, top-level arguments).
    fn step_entities(t: &str) -> std::collections::HashMap<u32, (String, Vec<String>)> {
        let mut out = std::collections::HashMap::new();
        for line in t.lines() {
            let Some(rest) = line.strip_prefix('#') else { continue };
            let Some((id, body)) = rest.split_once('=') else { continue };
            let Ok(id) = id.trim().parse::<u32>() else { continue };
            let Some((name, args)) = body.trim().split_once('(') else { continue };
            let args = args.trim_end().trim_end_matches(';').trim_end_matches(')');
            let (mut parts, mut depth, mut quoted, mut cur) = (vec![], 0i32, false, String::new());
            for ch in args.chars() {
                match ch {
                    '\'' => {
                        quoted = !quoted;
                        cur.push(ch);
                    }
                    '(' if !quoted => {
                        depth += 1;
                        cur.push(ch);
                    }
                    ')' if !quoted => {
                        depth -= 1;
                        cur.push(ch);
                    }
                    ',' if !quoted && depth == 0 => parts.push(std::mem::take(&mut cur)),
                    _ => cur.push(ch),
                }
            }
            parts.push(cur);
            out.insert(id, (name.trim().to_string(), parts));
        }
        out
    }

    fn refs(arg: &str) -> Vec<u32> {
        arg.split(|c: char| !(c.is_ascii_digit() || c == '#'))
            .filter_map(|p| p.strip_prefix('#').and_then(|n| n.parse().ok()))
            .collect()
    }


    #[test]
    fn walls_under_a_roof_are_clipped_bodies_without_a_full_height_box() {
        use ddc_model::{RoofKind, RoofSpec};
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let layer = |n: &str, t: f64| LayerSpec { material: n.into(), thickness: t };
        let wt = m.add_wall_type("ext", &[layer("Brick", 0.2)]).unwrap();
        let st = m.add_slab_type("Roof", &[layer("Tiles", 0.05), layer("Rafters", 0.15)]).unwrap();
        let p = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
        for i in 0..4 {
            m.add_wall(&WallSpec { a: p[i], b: p[(i + 1) % 4], level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: Some(0.0) })
                .unwrap();
        }
        m.add_roof(&RoofSpec {
            level: lv,
            slab_type: st,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Footprint { ring: p.to_vec(), slopes: vec![Some(30.0), None, Some(30.0), None], mansard: None },
        })
        .unwrap();
        let t = export_ifc(&m).unwrap();
        let e = step_entities(&t);
        let num = |a: &str| a.trim().parse::<f64>().unwrap_or(f64::NAN);
        let ridge = 3.0 + 3.0 * 30f64.to_radians().tan() - 0.2 / 30f64.to_radians().cos();
        // points of an IFCPOLYLINE
        let poly_pts = |id: u32| -> Vec<(f64, f64)> {
            refs(&e[&id].1[0])
                .iter()
                .map(|q| {
                    let c: Vec<f64> = e[q].1[0].trim_matches(|ch| ch == '(' || ch == ')').split(',').map(num).collect();
                    (c[0], c[1])
                })
                .collect()
        };
        let walls: Vec<&Vec<String>> = e.values().filter(|v| v.0 == "IFCWALL").map(|v| &v.1).collect();
        assert_eq!(walls.len(), 4);
        let (mut gables, mut eaves) = (0, 0);
        for w in walls {
            // only this wall's own body items: Representation -> shape -> 'Body' representation
            let items: Vec<u32> = refs(&w[6])
                .iter()
                .flat_map(|id| refs(&e[id].1[2]))
                .filter_map(|id| e.get(&id))
                .filter(|r| r.0 == "IFCSHAPEREPRESENTATION" && r.1[1].contains("Body"))
                .flat_map(|r| {
                    assert!(r.1[2].contains("CSG"), "body kind {}", r.1[2]);
                    refs(&r.1[3])
                })
                .collect();
            assert_eq!(items.len(), 1, "one body item per single-layer wall");
            let b = &e[&items[0]];
            assert_eq!(b.0, "IFCBOOLEANRESULT", "no bare full-height box under a roof");
            assert!(b.1[0].contains(".INTERSECTION."));
            let (body, clip) = (&e[&refs(&b.1[1])[0]], &e[&refs(&b.1[2])[0]]);
            assert_eq!((body.0.as_str(), clip.0.as_str()), ("IFCEXTRUDEDAREASOLID", "IFCEXTRUDEDAREASOLID"));
            let (rise, across) = (num(&body.1[3]), num(&clip.1[3]));
            assert!(across > 0.2 + 1.0, "clip solid spans the wall thickness and more: {across}");
            // the clip's profile is the wall elevation: x along the axis, y up
            let prof = poly_pts(refs(&e[&refs(&clip.1[0])[0]].1[2])[0]);
            let top = prof.iter().fold(f64::MIN, |m, q| m.max(q.1));
            if (rise - ridge).abs() < 1e-3 {
                gables += 1;
                assert!((top - ridge).abs() < 1e-3, "gable clip peaks at the ridge {ridge}: {top}");
                assert!(prof.iter().any(|q| (q.0 - 3.0).abs() < 1e-3 && (q.1 - ridge).abs() < 1e-3));
            } else {
                eaves += 1;
                assert!((rise - 3.0).abs() < 1e-6, "eave wall box stays 3 m: {rise}");
                assert!(top < 3.0 - 0.1, "eave clip cuts below the wall height: {top}");
            }
        }
        assert_eq!((gables, eaves), (2, 2));
    }

    #[test]
    fn an_arc_wall_with_a_top_profile_is_clipped_in_sectors() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m.add_wall_type("ext", &[LayerSpec { material: "Brick".into(), thickness: 0.2 }]).unwrap();
        let w = m
            .add_arc_wall(&WallSpec { a: (0.0, 0.0), b: (6.0, 0.0), level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: Some(0.0) }, 0.3)
            .unwrap();
        let ax = ddc_geom::arc::ArcAxis::new(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(6.0, 0.0), 0.3, ddc_geom::Tol::default()).unwrap();
        let l = ax.length();
        m.set_wall_top_profile(w, Some(&[(0.0, 1.0), (l / 2.0, 2.5), (l, 1.0)])).unwrap();
        let t = export_ifc(&m).unwrap();
        let e = step_entities(&t);
        let num = |a: &str| a.trim().parse::<f64>().unwrap_or(f64::NAN);
        let wall = e.values().find(|v| v.0 == "IFCWALL").expect("one wall");
        let items: Vec<u32> = refs(&wall.1[6])
            .iter()
            .flat_map(|id| refs(&e[id].1[2]))
            .filter_map(|id| e.get(&id))
            .filter(|r| r.0 == "IFCSHAPEREPRESENTATION" && r.1[1].contains("Body"))
            .flat_map(|r| {
                assert!(r.1[2].contains("CSG"), "body kind {}", r.1[2]);
                refs(&r.1[3])
            })
            .collect();
        assert!(items.len() >= 6, "faceted into sectors: {}", items.len());
        let (mut top, mut low) = (f64::MIN, f64::MAX);
        for id in items {
            let b = &e[&id];
            assert_eq!(b.0, "IFCBOOLEANRESULT");
            let clip = &e[&refs(&b.1[2])[0]];
            let body = &e[&refs(&b.1[1])[0]];
            assert!(num(&clip.1[3]) > 1.2, "clip spans the wall thickness");
            assert!((num(&body.1[3]) - 3.0).abs() < 1e-6, "body box to the wall height");
            let poly = &e[&refs(&e[&refs(&clip.1[0])[0]].1[2])[0]];
            for q in refs(&poly.1[0]) {
                let c: Vec<f64> = e[&q].1[0].trim_matches(|ch| ch == '(' || ch == ')').split(',').map(num).collect();
                top = top.max(c[1]);
                if c[1] > 0.5 {
                    low = low.min(c[1]);
                }
            }
        }
        assert!((top - 2.5).abs() < 1e-3, "the clips peak at the profile's 2.5: {top}");
        assert!((low - 1.0).abs() < 1e-3, "and drop to its 1.0 at the ends: {low}");
        let rep = validate_ifc(t.as_bytes());
        assert!(rep.is_clean(), "{:?}", rep.errors);
    }
}
